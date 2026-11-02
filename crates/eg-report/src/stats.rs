//! 统计口径（`SPEC-report.md` §3.4/§3.5）：有效样本、双模式分列、单题/整卷/班级指标、
//! 选项分布、错题标签、班级分布与快速抽检。
//!
//! 全部计算为整数运算（千分之一分与 ppm），浮点只出现在 `Confidence` 的展示与
//! canonical 快照文本中；分母为 0 时输出 `None`（渲染为「不适用」），绝不输出 0 冒充指标。
//!
//! 样本定义（规范 §3.4）：
//! - 单题：最新修订、模式匹配、状态为自动接受/已人工复核、提交未被排除、学生在名单内。
//! - 整卷：一份提交内所有有效记录同属一种模式；待复核/排除/名单外/混合模式不计入班级样本；
//!   有效满分为 0 时不计入样本；有效满分小于整卷总分时标「不完整」。
//! - 混合模式提交的单题仍按记录模式分别计入，但整卷分与排名不进入任一模式。

use crate::error::Result;
use crate::rounding::{
    discrimination_hundredths, div_half_up, mean_milli, rate_ppm, round_ppm_to_tenth_percent,
    stddev_milli,
};
use crate::settings::ReportSettings;
use crate::source::{
    ChoiceAnswer, ChoiceRecord, PaperSnapshot, QuestionSnapshot, ReportInput, SubmissionSummary,
    mode_label,
};
use eg_domain::{
    GradeMode, GradeRecord, GradeStatus, MilliScore, Sha256Hex, Student, StudentId, SubmissionId,
};
use std::collections::{BTreeMap, BTreeSet};

/// 区分度最小样本量（规范 §3.4：n < 30 输出「不适用」）。
pub const DISCRIMINATION_MIN_SAMPLES: usize = 30;

/// 班级分布箱数（率区间 `[0,1]` 分 10 箱）。
pub const DISTRIBUTION_BINS: u8 = 10;

/// 整卷结果状态（成绩单「状态」列）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StudentOutcome {
    /// 完整卷：全部题目有有效记录且同属一种模式。
    Complete,
    /// 不完整：部分题目无有效记录或记录被排除，但有效满分大于 0。
    Incomplete,
    /// 待复核：存在待复核记录（不进样本）。
    NeedsReview,
    /// 缺考：名单内无提交。
    Absent,
    /// 已排除：提交被 `SubmissionExclusion` 排除。
    Excluded,
    /// 名单外：提交的学生不在名单内（或有提交但身份未确认）。
    NotInRoster,
    /// 混合模式：同一份提交内标准与快速记录并存（整卷分不进任一模式）。
    MixedMode,
    /// 无有效记录（有效满分为 0）。
    NoRecords,
    /// 重复提交：同一学生存在多份提交，本行非最新一份。
    Duplicate,
}

impl StudentOutcome {
    /// 成绩单状态文本（规范 §3.5「状态」列）。
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Complete => "完整",
            Self::Incomplete => "不完整",
            Self::NeedsReview => "待复核",
            Self::Absent => "缺考",
            Self::Excluded => "排除",
            Self::NotInRoster => "名单外",
            Self::MixedMode => "混合模式",
            Self::NoRecords => "无有效记录",
            Self::Duplicate => "重复提交",
        }
    }

    /// 是否计入班级/整卷样本。
    #[must_use]
    pub const fn in_class_sample(self) -> bool {
        matches!(self, Self::Complete | Self::Incomplete)
    }
}

/// 单题指标（按模式）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeQuestionStats {
    /// 模式。
    pub mode: GradeMode,
    /// 有效样本数。
    pub valid_count: usize,
    /// 得分之和（千分）。
    pub awarded_sum_milli: i64,
    /// 满分之和（千分；等于样本记录满分之和）。
    pub max_sum_milli: i64,
    /// 平均得分（千分，保留 0.01 分）。
    pub mean_milli: Option<i64>,
    /// 得分率（ppm；满分之和为 0 时为空）。
    pub rate_ppm: Option<u32>,
    /// 区分度（百分点 ×100；`n < 30` 或满分不足时为空）。
    pub discrimination_hundredths: Option<i32>,
    /// 区分度不适用的原因（为空表示可用）。
    pub discrimination_na: Option<&'static str>,
}

/// 单题统计（标准/快速各一组，列序见规范 §3.5）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionStats {
    /// 卷面顺序。
    pub ordinal: u32,
    /// 题号标签。
    pub label: String,
    /// 题型。
    pub kind: eg_domain::QuestionKind,
    /// 满分。
    pub max_score: MilliScore,
    /// 标准模式指标。
    pub standard: ModeQuestionStats,
    /// 快速模式指标。
    pub fast: ModeQuestionStats,
}

/// 学生成绩单行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StudentResult {
    /// 序号（从 1 起，按学号排序）。
    pub ordinal: usize,
    /// 学生 id（名单外行为空）。
    pub student_id: Option<StudentId>,
    /// 学号。
    pub student_no: String,
    /// 姓名。
    pub name: String,
    /// 整卷模式（混合/无有效记录时为空）。
    pub mode: Option<GradeMode>,
    /// 状态。
    pub outcome: StudentOutcome,
    /// 有效满分（千分；排除题不计）。
    pub effective_max_milli: i64,
    /// 得分之和（千分）。
    pub awarded_sum_milli: i64,
    /// 折算分（千分；有效满分为 0 时为空）。
    pub normalized_milli: Option<i64>,
    /// 整卷满分（千分）。
    pub full_mark_milli: i64,
    /// 整卷得分率（ppm；有效满分为 0 时为空）。
    pub rate_ppm: Option<u32>,
    /// 同模式排名（竞争排名 1、1、3；缺考/待复核/排除/混合模式留空）。
    pub rank: Option<u32>,
    /// 各题得分（千分；无有效记录/被排除/缺页为空）。
    pub question_awards: Vec<Option<i64>>,
    /// 错题标签（失分题目的知识点标签，去重排序）。
    pub tags: Vec<String>,
    /// 是否含抽检记录（快速模式）。
    pub sampled: bool,
}

/// 班级汇总行（`mode == None` 为「全部」对照行，指标输出不适用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassStats {
    /// 模式（`None` 表示全部对照行）。
    pub mode: Option<GradeMode>,
    /// 有效整卷样本数。
    pub valid_count: usize,
    /// 缺考数。
    pub absent_count: usize,
    /// 排除数。
    pub excluded_count: usize,
    /// 待复核数。
    pub pending_review_count: usize,
    /// 名单外数。
    pub not_in_roster_count: usize,
    /// 混合模式数。
    pub mixed_mode_count: usize,
    /// 平均分（折算分，千分）。
    pub mean_milli: Option<i64>,
    /// 标准差（折算分，千分）。
    pub stddev_milli: Option<i64>,
    /// 最高折算分。
    pub max_milli: Option<i64>,
    /// 最低折算分。
    pub min_milli: Option<i64>,
    /// 及格率（整卷得分率 ≥ 及格线）。
    pub pass_ppm: Option<u32>,
    /// 良好率。
    pub good_ppm: Option<u32>,
    /// 优秀率。
    pub excellent_ppm: Option<u32>,
    /// 满分人数。
    pub full_mark_count: usize,
    /// 快速抽检记录数（全部行亦展示）。
    pub sampled_record_count: usize,
}

/// 选项分布行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionStats {
    /// 卷面顺序。
    pub ordinal: u32,
    /// 题号标签。
    pub label: String,
    /// 模式。
    pub mode: GradeMode,
    /// 选项（单选为选项值；多选为组合，如 `A+C`；未作答为「空」；无法解析为「不确定」）。
    pub option: String,
    /// 人数。
    pub count: usize,
    /// 占比（分母为该题该模式有效样本数）。
    pub ppm: Option<u32>,
    /// 是否为正确答案组合。
    pub correct: bool,
    /// 未作答人数（该题该模式）。
    pub blank_count: usize,
    /// 无法解析人数（该题该模式）。
    pub unparsed_count: usize,
}

/// 错题标签行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagStats {
    /// 知识点标签。
    pub tag: String,
    /// 模式。
    pub mode: GradeMode,
    /// 涉及题数。
    pub question_count: usize,
    /// 作答次数（有效记录数）。
    pub answer_count: usize,
    /// 错题次数（得分小于满分）。
    pub wrong_count: usize,
    /// 错题率。
    pub wrong_ppm: Option<u32>,
    /// 失分率（失分之和 / 满分之和）。
    pub lost_ppm: Option<u32>,
}

/// 班级分布行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DistributionBin {
    /// 箱号（0–9）。
    pub bin: u8,
    /// 区间文本（如 `60%–70%`；末箱含 100%）。
    pub range: String,
    /// 模式。
    pub mode: GradeMode,
    /// 人数。
    pub count: usize,
    /// 占比。
    pub ppm: Option<u32>,
}

/// 快速抽检行（每份含快速记录的提交一行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FastSamplingRow {
    /// 提交 id。
    pub submission_id: SubmissionId,
    /// 提交展示名（去扩展名文件名）。
    pub submission_label: String,
    /// 学生显示名（排名表序号对应的掩码在渲染阶段处理）。
    pub student_name: String,
    /// 学生学号。
    pub student_no: String,
    /// 抽检标记：「已抽检」/「未抽检」。
    pub sampled_label: &'static str,
    /// 抽检状态：「待复核」/「通过」。
    pub status_label: &'static str,
    /// 是否已转标准复核。
    pub converted_label: &'static str,
    /// 抽检记录数。
    pub sampled_count: usize,
}

/// 报表（纯计算产物；不含 IO）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// 口径配置。
    pub settings: ReportSettings,
    /// 试卷快照。
    pub paper: PaperSnapshot,
    /// 快照哈希（元信息/审计/重放）。
    pub snapshot_hash: Sha256Hex,
    /// 成绩单行（名单顺序：学号升序，其后为名单外/重复行）。
    pub students: Vec<StudentResult>,
    /// 班级汇总（标准、快速、全部对照）。
    pub class: Vec<ClassStats>,
    /// 题目统计。
    pub questions: Vec<QuestionStats>,
    /// 选项分布。
    pub options: Vec<OptionStats>,
    /// 错题标签。
    pub tags: Vec<TagStats>,
    /// 班级分布。
    pub distribution: Vec<DistributionBin>,
    /// 快速抽检。
    pub sampling: Vec<FastSamplingRow>,
}

impl Report {
    /// 由一致快照构建报表（纯计算；同一输入逐字节一致，用于重放对比）。
    ///
    /// # Errors
    ///
    /// 口径非法、快照不满足不变量、分数守恒不成立时返回
    /// [`ReportError::Config`] / [`ReportError::Invariant`]。
    pub fn build(input: &ReportInput, settings: ReportSettings) -> Result<Self> {
        settings.validate()?;
        input.validate()?;
        let snapshot_hash = input.snapshot_hash();

        let mut submissions_by_student: BTreeMap<String, Vec<&SubmissionSummary>> = BTreeMap::new();
        for submission in &input.submissions {
            if let Some(student_id) = submission.student_id {
                submissions_by_student
                    .entry(student_id.to_string())
                    .or_default()
                    .push(submission);
            }
        }
        let known_students: BTreeSet<String> = input
            .students
            .iter()
            .map(|student| student.id.to_string())
            .collect();
        let excluded_submissions: BTreeSet<String> = input
            .exclusions
            .iter()
            .map(|exclusion| exclusion.submission_id.to_string())
            .collect();

        // 最新修订：同一（提交, 题目）取 revision 最大者；重复修订已在校验中拒绝。
        let mut latest: BTreeMap<(String, String), &GradeRecord> = BTreeMap::new();
        for record in &input.records {
            let key = (
                record.submission_id.to_string(),
                record.question_id.to_string(),
            );
            let replace = latest
                .get(&key)
                .is_none_or(|existing| record.revision > existing.revision);
            if replace {
                latest.insert(key, record);
            }
        }

        let mut roster: Vec<&Student> = input.students.iter().collect();
        roster.sort_by(|left, right| {
            left.student_no
                .cmp(&right.student_no)
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.id.to_string().cmp(&right.id.to_string()))
        });

        let mut selected_submissions: BTreeSet<String> = BTreeSet::new();
        let mut duplicate_submissions: Vec<&SubmissionSummary> = Vec::new();
        let mut students: Vec<StudentResult> = Vec::with_capacity(roster.len());
        for (index, student) in roster.iter().enumerate() {
            let mut submissions = submissions_by_student
                .get(&student.id.to_string())
                .cloned()
                .unwrap_or_default();
            submissions.sort_by(|left, right| {
                left.created_at
                    .cmp(&right.created_at)
                    .then_with(|| left.id.to_string().cmp(&right.id.to_string()))
            });
            let ordinal = index.saturating_add(1);
            let Some(selected) = submissions.last().copied() else {
                students.push(StudentResult {
                    ordinal,
                    student_id: Some(student.id),
                    student_no: student.student_no.clone(),
                    name: student.name.clone(),
                    mode: None,
                    outcome: StudentOutcome::Absent,
                    effective_max_milli: 0,
                    awarded_sum_milli: 0,
                    normalized_milli: None,
                    full_mark_milli: input.paper.total_score.as_milli(),
                    rate_ppm: None,
                    rank: None,
                    question_awards: vec![None; input.paper.questions.len()],
                    tags: Vec::new(),
                    sampled: false,
                });
                for duplicate in submissions {
                    duplicate_submissions.push(duplicate);
                }
                continue;
            };
            for duplicate in submissions.iter().take(submissions.len().saturating_sub(1)) {
                duplicate_submissions.push(duplicate);
            }
            selected_submissions.insert(selected.id.to_string());
            let result = build_student_result(
                &input.paper,
                &latest,
                student,
                selected,
                ordinal,
                excluded_submissions.contains(&selected.id.to_string()),
            );
            students.push(result);
        }

        // 名单外/重复提交行（状态列单列；不计入样本）。
        for duplicate in duplicate_submissions {
            let years = build_orphan_result(
                &input.paper,
                &latest,
                duplicate,
                students.len().saturating_add(1),
                StudentOutcome::Duplicate,
            );
            students.push(years);
        }
        for submission in &input.submissions {
            if selected_submissions.contains(&submission.id.to_string()) {
                continue;
            }
            let in_roster = submission
                .student_id
                .is_some_and(|id| known_students.contains(&id.to_string()));
            if in_roster {
                continue;
            }
            let outcome = if excluded_submissions.contains(&submission.id.to_string()) {
                StudentOutcome::Excluded
            } else {
                StudentOutcome::NotInRoster
            };
            let position = students.len().saturating_add(1);
            let result = build_orphan_result(&input.paper, &latest, submission, position, outcome);
            students.push(result);
        }

        assign_ranks(&mut students);

        let class = build_class_stats(input, &students, &settings)?;
        let questions = build_question_stats(
            input,
            &latest,
            &selected_submissions,
            &excluded_submissions,
            &known_students,
        )?;
        let options = build_option_stats(
            input,
            &latest,
            &selected_submissions,
            &excluded_submissions,
            &known_students,
        )?;
        let tags = build_tag_stats(
            input,
            &latest,
            &selected_submissions,
            &excluded_submissions,
            &known_students,
        )?;
        let distribution = build_distribution(&students, &class);
        let sampling = build_sampling(
            input,
            &latest,
            &students,
            &excluded_submissions,
            &known_students,
        );

        Ok(Self {
            settings,
            paper: input.paper.clone(),
            snapshot_hash,
            students,
            class,
            questions,
            options,
            tags,
            distribution,
            sampling,
        })
    }

    /// 「全部」对照行的快照计数（元信息展示）。
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.students
            .iter()
            .filter(|student| student.outcome == StudentOutcome::NeedsReview)
            .count()
    }

    /// 是否有任何快速模式记录（`fast.export_label` 与表头标注用）。
    #[must_use]
    pub fn has_fast_records(&self) -> bool {
        self.students.iter().any(|student| {
            student.mode == Some(GradeMode::Fast) || (student.mode.is_none() && student.sampled)
        }) || !self.sampling.is_empty()
    }

    /// 快速模式是否需要在表头标「快速」（`fast.export_label` 开且有快速记录）。
    #[must_use]
    pub fn fast_label_visible(&self) -> bool {
        self.settings.fast_export_label && self.has_fast_records()
    }
}

/// 单份提交的有效记录集合。
struct SubmissionRecords<'a> {
    /// 每题最新有效记录（按题目下标）。
    records: Vec<Option<&'a GradeRecord>>,
    /// 有效记录的模式集合（按出现顺序去重）。
    modes: Vec<GradeMode>,
    /// 是否存在待复核记录。
    pending: bool,
    /// 是否存在被排除的题级记录。
    has_excluded_record: bool,
    /// 是否含抽检记录。
    sampled: bool,
}

fn collect_submission_records<'a>(
    paper: &PaperSnapshot,
    latest: &BTreeMap<(String, String), &'a GradeRecord>,
    submission_id: SubmissionId,
) -> SubmissionRecords<'a> {
    let mut records: Vec<Option<&GradeRecord>> = vec![None; paper.questions.len()];
    let mut modes: Vec<GradeMode> = Vec::new();
    let mut pending = false;
    let mut has_excluded_record = false;
    let mut sampled = false;
    for (index, question) in paper.questions.iter().enumerate() {
        let key = (submission_id.to_string(), question.id.to_string());
        let Some(record) = latest.get(&key).copied() else {
            continue;
        };
        sampled |= record.sampled;
        match record.status {
            GradeStatus::NeedsReview => {
                pending = true;
            }
            GradeStatus::Excluded => {
                has_excluded_record = true;
            }
            GradeStatus::AutoAccepted | GradeStatus::ManuallyReviewed => {
                records[index] = Some(record);
                if !modes.contains(&record.mode) {
                    modes.push(record.mode);
                }
            }
        }
    }
    SubmissionRecords {
        records,
        modes,
        pending,
        has_excluded_record,
        sampled,
    }
}

fn build_student_result(
    paper: &PaperSnapshot,
    latest: &BTreeMap<(String, String), &GradeRecord>,
    student: &Student,
    submission: &SubmissionSummary,
    ordinal: usize,
    excluded: bool,
) -> StudentResult {
    let full_mark = paper.total_score.as_milli();
    let collected = collect_submission_records(paper, latest, submission.id);
    let mut awarded_sum: i64 = 0;
    let mut effective_max: i64 = 0;
    let mut question_awards: Vec<Option<i64>> = Vec::with_capacity(paper.questions.len());
    let mut tags: BTreeSet<String> = BTreeSet::new();
    for (index, question) in paper.questions.iter().enumerate() {
        match collected.records[index] {
            Some(record) => {
                awarded_sum = awarded_sum.saturating_add(record.awarded.as_milli());
                effective_max = effective_max.saturating_add(record.max_score.as_milli());
                question_awards.push(Some(record.awarded.as_milli()));
                if record.awarded < record.max_score {
                    for tag in &question.knowledge_tags {
                        tags.insert(tag.clone());
                    }
                }
            }
            None => question_awards.push(None),
        }
    }
    let mode = if collected.modes.len() == 1 {
        collected.modes.first().copied()
    } else {
        None
    };
    let outcome = if excluded {
        StudentOutcome::Excluded
    } else if collected.pending {
        StudentOutcome::NeedsReview
    } else if collected.modes.len() > 1 {
        StudentOutcome::MixedMode
    } else if effective_max <= 0 {
        StudentOutcome::NoRecords
    } else if effective_max == full_mark {
        StudentOutcome::Complete
    } else {
        StudentOutcome::Incomplete
    };
    let normalized = if effective_max > 0 {
        div_half_up(
            i128::from(awarded_sum) * i128::from(full_mark),
            i128::from(effective_max),
        )
        .and_then(|value| i64::try_from(value).ok())
    } else {
        None
    };
    let rate = rate_ppm(i128::from(awarded_sum), i128::from(effective_max));
    let display_mode = if outcome.in_class_sample() {
        mode
    } else {
        None
    };
    let _ = collected.has_excluded_record;
    StudentResult {
        ordinal,
        student_id: Some(student.id),
        student_no: student.student_no.clone(),
        name: student.name.clone(),
        mode: display_mode,
        outcome,
        effective_max_milli: effective_max,
        awarded_sum_milli: awarded_sum,
        normalized_milli: normalized,
        full_mark_milli: full_mark,
        rate_ppm: rate,
        rank: None,
        question_awards,
        tags: tags.into_iter().collect(),
        sampled: collected.sampled,
    }
}

fn build_orphan_result(
    paper: &PaperSnapshot,
    latest: &BTreeMap<(String, String), &GradeRecord>,
    submission: &SubmissionSummary,
    ordinal: usize,
    outcome: StudentOutcome,
) -> StudentResult {
    let full_mark = paper.total_score.as_milli();
    let collected = collect_submission_records(paper, latest, submission.id);
    let mut awarded_sum: i64 = 0;
    let mut effective_max: i64 = 0;
    let mut question_awards: Vec<Option<i64>> = Vec::with_capacity(paper.questions.len());
    for question in &paper.questions {
        match collected
            .records
            .get(
                usize::try_from(question.ordinal)
                    .unwrap_or(0)
                    .saturating_sub(1),
            )
            .copied()
            .flatten()
        {
            Some(record) => {
                awarded_sum = awarded_sum.saturating_add(record.awarded.as_milli());
                effective_max = effective_max.saturating_add(record.max_score.as_milli());
                question_awards.push(Some(record.awarded.as_milli()));
            }
            None => question_awards.push(None),
        }
    }
    let mode = if collected.modes.len() == 1 {
        collected.modes.first().copied()
    } else {
        None
    };
    let normalized = if effective_max > 0 {
        div_half_up(
            i128::from(awarded_sum) * i128::from(full_mark),
            i128::from(effective_max),
        )
        .and_then(|value| i64::try_from(value).ok())
    } else {
        None
    };
    StudentResult {
        ordinal,
        student_id: submission.student_id,
        student_no: "—".to_owned(),
        name: submission.source_stem.clone(),
        mode: if outcome.in_class_sample() {
            mode
        } else {
            None
        },
        outcome,
        effective_max_milli: effective_max,
        awarded_sum_milli: awarded_sum,
        normalized_milli: normalized,
        full_mark_milli: full_mark,
        rate_ppm: rate_ppm(i128::from(awarded_sum), i128::from(effective_max)),
        rank: None,
        question_awards,
        tags: Vec::new(),
        sampled: collected.sampled,
    }
}

/// 同模式内竞争排名（1、1、3）；缺考/待复核/排除/混合模式/名单外留空。
fn assign_ranks(students: &mut [StudentResult]) {
    for mode in [GradeMode::Standard, GradeMode::Fast] {
        let mut indexes: Vec<usize> = students
            .iter()
            .enumerate()
            .filter(|(_, student)| {
                student.mode == Some(mode)
                    && student.outcome.in_class_sample()
                    && student.normalized_milli.is_some()
            })
            .map(|(index, _)| index)
            .collect();
        indexes.sort_by(|left, right| {
            let left_score = students[*left].normalized_milli.unwrap_or(0);
            let right_score = students[*right].normalized_milli.unwrap_or(0);
            right_score
                .cmp(&left_score)
                .then_with(|| students[*left].student_no.cmp(&students[*right].student_no))
        });
        let mut previous_score: Option<i64> = None;
        let mut rank: usize = 0;
        for (position, index) in indexes.iter().enumerate() {
            let score = students[*index].normalized_milli.unwrap_or(0);
            if previous_score != Some(score) {
                rank = position.saturating_add(1);
                previous_score = Some(score);
            }
            students[*index].rank = u32::try_from(rank).ok();
        }
    }
}

/// `usize` → `i128`（不会失败；溢出上限按 i128::MAX 处理）。
fn usize_to_i128(value: usize) -> i128 {
    i128::try_from(value).unwrap_or(i128::MAX)
}

fn build_class_stats(
    input: &ReportInput,
    students: &[StudentResult],
    settings: &ReportSettings,
) -> Result<Vec<ClassStats>> {
    let absent_count = students
        .iter()
        .filter(|student| student.outcome == StudentOutcome::Absent)
        .count();
    let pending_review_count = students
        .iter()
        .filter(|student| student.outcome == StudentOutcome::NeedsReview)
        .count();
    let mixed_mode_count = students
        .iter()
        .filter(|student| student.outcome == StudentOutcome::MixedMode)
        .count();
    let duplicate_count = students
        .iter()
        .filter(|student| student.outcome == StudentOutcome::Duplicate)
        .count();
    let not_in_roster_count = students
        .iter()
        .filter(|student| {
            matches!(
                student.outcome,
                StudentOutcome::NotInRoster | StudentOutcome::Duplicate
            ) || student.student_no == "—"
        })
        .count();
    let excluded_count = input.exclusions.len().saturating_add(duplicate_count);
    let sampled_record_count = input.records.iter().filter(|record| record.sampled).count();

    let mut rows: Vec<ClassStats> = Vec::with_capacity(3);
    for mode in [Some(GradeMode::Standard), Some(GradeMode::Fast)] {
        let samples: Vec<&StudentResult> = students
            .iter()
            .filter(|student| {
                student.outcome.in_class_sample()
                    && student.mode == mode
                    && student.normalized_milli.is_some()
            })
            .collect();
        let valid_count = samples.len();
        let scores: Vec<i64> = samples
            .iter()
            .filter_map(|student| student.normalized_milli)
            .collect();
        let sum: i128 = scores.iter().map(|value| i128::from(*value)).sum();
        let mean = mean_milli(sum, valid_count);
        let stddev = mean.and_then(|mean| stddev_milli(&scores, mean));
        let max = scores.iter().copied().max();
        let min = scores.iter().copied().min();
        let pass = count_at_least(&samples, settings.pass_rate_ppm);
        let good = count_at_least(&samples, settings.good_rate_ppm);
        let excellent = count_at_least(&samples, settings.excellent_rate_ppm);
        let full_mark_count = samples
            .iter()
            .filter(|student| student.normalized_milli == Some(student.full_mark_milli))
            .count();
        rows.push(ClassStats {
            mode,
            valid_count,
            absent_count,
            excluded_count,
            pending_review_count,
            not_in_roster_count,
            mixed_mode_count,
            mean_milli: mean,
            stddev_milli: stddev,
            max_milli: max,
            min_milli: min,
            pass_ppm: rate_ppm(usize_to_i128(pass), usize_to_i128(valid_count)),
            good_ppm: rate_ppm(usize_to_i128(good), usize_to_i128(valid_count)),
            excellent_ppm: rate_ppm(usize_to_i128(excellent), usize_to_i128(valid_count)),
            full_mark_count,
            sampled_record_count,
        });
    }
    // 「全部」对照行：只给样本计数，不产出混合均分/得分率（规范 §3.3）。
    let valid_total: usize = rows.iter().map(|row| row.valid_count).sum();
    rows.push(ClassStats {
        mode: None,
        valid_count: valid_total,
        absent_count,
        excluded_count,
        pending_review_count,
        not_in_roster_count,
        mixed_mode_count,
        mean_milli: None,
        stddev_milli: None,
        max_milli: None,
        min_milli: None,
        pass_ppm: None,
        good_ppm: None,
        excellent_ppm: None,
        full_mark_count: 0,
        sampled_record_count,
    });
    Ok(rows)
}

fn count_at_least(samples: &[&StudentResult], threshold_ppm: u32) -> usize {
    samples
        .iter()
        .filter(|student| {
            student
                .rate_ppm
                .is_some_and(|rate| round_ppm_to_tenth_percent(rate) >= threshold_ppm)
        })
        .count()
}

/// 单题有效样本（按模式收集）。
struct QuestionSample {
    awarded: i64,
    max: i64,
    student_no: String,
}

fn collect_question_samples(
    input: &ReportInput,
    latest: &BTreeMap<(String, String), &GradeRecord>,
    selected: &BTreeSet<String>,
    excluded: &BTreeSet<String>,
    known: &BTreeSet<String>,
    question_id: &str,
    mode: GradeMode,
) -> Vec<QuestionSample> {
    let submission_by_id: BTreeMap<String, &SubmissionSummary> = input
        .submissions
        .iter()
        .map(|submission| (submission.id.to_string(), submission))
        .collect();
    let student_no_by_id: BTreeMap<String, String> = input
        .students
        .iter()
        .map(|student| (student.id.to_string(), student.student_no.clone()))
        .collect();
    let mut samples = Vec::new();
    for ((submission_key, question_key), record) in latest {
        if question_key != question_id
            || record.mode != mode
            || !matches!(
                record.status,
                GradeStatus::AutoAccepted | GradeStatus::ManuallyReviewed
            )
            || !selected.contains(submission_key)
            || excluded.contains(submission_key)
        {
            continue;
        }
        let Some(submission) = submission_by_id.get(submission_key) else {
            continue;
        };
        let Some(student_id) = submission.student_id else {
            continue;
        };
        if !known.contains(&student_id.to_string()) {
            continue;
        }
        let student_no = student_no_by_id
            .get(&student_id.to_string())
            .cloned()
            .unwrap_or_default();
        samples.push(QuestionSample {
            awarded: record.awarded.as_milli(),
            max: record.max_score.as_milli(),
            student_no,
        });
    }
    samples.sort_by(|left, right| left.student_no.cmp(&right.student_no));
    samples
}

fn build_question_stats(
    input: &ReportInput,
    latest: &BTreeMap<(String, String), &GradeRecord>,
    selected: &BTreeSet<String>,
    excluded: &BTreeSet<String>,
    known: &BTreeSet<String>,
) -> Result<Vec<QuestionStats>> {
    let mut rows = Vec::with_capacity(input.paper.questions.len());
    for question in &input.paper.questions {
        let question_id = question.id.to_string();
        let standard = mode_question_stats(
            input,
            latest,
            selected,
            excluded,
            known,
            &question_id,
            GradeMode::Standard,
        );
        let fast = mode_question_stats(
            input,
            latest,
            selected,
            excluded,
            known,
            &question_id,
            GradeMode::Fast,
        );
        rows.push(QuestionStats {
            ordinal: question.ordinal,
            label: question.label.clone(),
            kind: question.kind,
            max_score: question.max_score,
            standard,
            fast,
        });
    }
    Ok(rows)
}

fn mode_question_stats(
    input: &ReportInput,
    latest: &BTreeMap<(String, String), &GradeRecord>,
    selected: &BTreeSet<String>,
    excluded: &BTreeSet<String>,
    known: &BTreeSet<String>,
    question_id: &str,
    mode: GradeMode,
) -> ModeQuestionStats {
    let samples =
        collect_question_samples(input, latest, selected, excluded, known, question_id, mode);
    let valid_count = samples.len();
    let mut awarded_sum: i64 = 0;
    let mut max_sum: i64 = 0;
    for sample in &samples {
        awarded_sum = awarded_sum.saturating_add(sample.awarded);
        max_sum = max_sum.saturating_add(sample.max);
    }
    let rate = rate_ppm(i128::from(awarded_sum), i128::from(max_sum));
    let (discrimination, na) = discrimination(&samples);
    ModeQuestionStats {
        mode,
        valid_count,
        awarded_sum_milli: awarded_sum,
        max_sum_milli: max_sum,
        mean_milli: mean_milli(i128::from(awarded_sum), valid_count),
        rate_ppm: rate,
        discrimination_hundredths: discrimination,
        discrimination_na: na,
    }
}

fn discrimination(samples: &[QuestionSample]) -> (Option<i32>, Option<&'static str>) {
    let n = samples.len();
    if n < DISCRIMINATION_MIN_SAMPLES {
        return (None, Some("样本不足（n<30），不适用"));
    }
    let mut ordered: Vec<&QuestionSample> = samples.iter().collect();
    ordered.sort_by(|left, right| {
        let left_rate = rate_ppm(i128::from(left.awarded), i128::from(left.max)).unwrap_or(0);
        let right_rate = rate_ppm(i128::from(right.awarded), i128::from(right.max)).unwrap_or(0);
        right_rate
            .cmp(&left_rate)
            .then_with(|| left.student_no.cmp(&right.student_no))
    });
    // k = 四舍五入(0.27 × n)，至少 1；2k > n 时 k = n/2。
    let mut k = (n.saturating_mul(27).saturating_add(50)) / 100;
    k = k.max(1);
    if k.saturating_mul(2) > n {
        k = n / 2;
    }
    if k == 0 || k.saturating_mul(2) > n {
        return (None, Some("样本分组后不足（n<2）"));
    }
    let group_rate = |group: &[&QuestionSample]| -> Option<u32> {
        let mut awarded: i64 = 0;
        let mut max: i64 = 0;
        for sample in group {
            awarded = awarded.saturating_add(sample.awarded);
            max = max.saturating_add(sample.max);
        }
        rate_ppm(i128::from(awarded), i128::from(max))
    };
    let high = group_rate(&ordered[..k]);
    let low = group_rate(&ordered[n - k..]);
    match (high, low) {
        (Some(high), Some(low)) => (Some(discrimination_hundredths(high, low)), None),
        _ => (None, Some("满分之和为 0，不适用")),
    }
}

fn build_option_stats(
    input: &ReportInput,
    latest: &BTreeMap<(String, String), &GradeRecord>,
    selected: &BTreeSet<String>,
    excluded: &BTreeSet<String>,
    known: &BTreeSet<String>,
) -> Result<Vec<OptionStats>> {
    let mut rows = Vec::new();
    let submission_ids: BTreeSet<String> = input
        .submissions
        .iter()
        .map(|submission| submission.id.to_string())
        .collect();
    let mut choices: BTreeMap<(String, String), &ChoiceRecord> = BTreeMap::new();
    for choice in &input.choices {
        if !submission_ids.contains(&choice.submission_id.to_string()) {
            continue;
        }
        choices.insert(
            (
                choice.submission_id.to_string(),
                choice.question_id.to_string(),
            ),
            choice,
        );
    }
    let student_by_submission: BTreeMap<String, String> = input
        .submissions
        .iter()
        .filter_map(|submission| {
            submission
                .student_id
                .map(|id| (submission.id.to_string(), id.to_string()))
        })
        .collect();
    for question in &input.paper.questions {
        if question.answer_options.is_empty() {
            continue;
        }
        for mode in [GradeMode::Standard, GradeMode::Fast] {
            let mut counts: BTreeMap<String, usize> = BTreeMap::new();
            let mut blank: usize = 0;
            let mut unparsed: usize = 0;
            let mut total: usize = 0;
            for ((submission_key, question_key), record) in latest {
                if question_key != &question.id.to_string()
                    || record.mode != mode
                    || !matches!(
                        record.status,
                        GradeStatus::AutoAccepted | GradeStatus::ManuallyReviewed
                    )
                    || !selected.contains(submission_key)
                    || excluded.contains(submission_key)
                {
                    continue;
                }
                let Some(student_id) = student_by_submission.get(submission_key) else {
                    continue;
                };
                if !known.contains(student_id) {
                    continue;
                }
                total = total.saturating_add(1);
                match choices.get(&(submission_key.clone(), question_key.clone())) {
                    Some(choice) => match &choice.answer {
                        ChoiceAnswer::Options(options) => {
                            let mut sorted = options.clone();
                            sorted.sort();
                            *counts.entry(sorted.join("+")).or_insert(0) += 1;
                        }
                        ChoiceAnswer::Blank => blank = blank.saturating_add(1),
                        ChoiceAnswer::Unparsed => unparsed = unparsed.saturating_add(1),
                    },
                    None => blank = blank.saturating_add(1),
                }
            }
            if total == 0 && counts.is_empty() && blank == 0 && unparsed == 0 {
                continue;
            }
            if blank > 0 {
                counts.insert("空".to_owned(), blank);
            }
            if unparsed > 0 {
                counts.insert("不确定".to_owned(), unparsed);
            }
            let correct = joined_options(&question.correct_options);
            let mut labels: Vec<String> = counts.keys().cloned().collect();
            labels.sort_by_key(|label| option_order(question, label));
            for label in labels {
                let count = counts.get(&label).copied().unwrap_or(0);
                rows.push(OptionStats {
                    ordinal: question.ordinal,
                    label: question.label.clone(),
                    mode,
                    option: label.clone(),
                    count,
                    ppm: rate_ppm(usize_to_i128(count), usize_to_i128(total)),
                    correct: label == correct,
                    blank_count: blank,
                    unparsed_count: unparsed,
                });
            }
        }
    }
    Ok(rows)
}

fn joined_options(options: &[String]) -> String {
    let mut sorted = options.to_vec();
    sorted.sort();
    sorted.join("+")
}

fn option_order(question: &QuestionSnapshot, label: &str) -> (usize, String) {
    if label == "空" {
        return (2, label.to_owned());
    }
    if label == "不确定" {
        return (3, label.to_owned());
    }
    if let Some(index) = question
        .answer_options
        .iter()
        .position(|option| option == label)
    {
        return (0, format!("{index:03}"));
    }
    (1, label.to_owned())
}

fn build_tag_stats(
    input: &ReportInput,
    latest: &BTreeMap<(String, String), &GradeRecord>,
    selected: &BTreeSet<String>,
    excluded: &BTreeSet<String>,
    known: &BTreeSet<String>,
) -> Result<Vec<TagStats>> {
    let mut tags: Vec<String> = input
        .paper
        .questions
        .iter()
        .flat_map(|question| question.knowledge_tags.clone())
        .collect();
    tags.sort();
    tags.dedup();
    let mut rows = Vec::new();
    for tag in tags {
        for mode in [GradeMode::Standard, GradeMode::Fast] {
            let mut question_count = 0usize;
            let mut answer_count = 0usize;
            let mut wrong_count = 0usize;
            let mut lost: i128 = 0;
            let mut max_sum: i128 = 0;
            for question in &input.paper.questions {
                if !question.knowledge_tags.iter().any(|item| item == &tag) {
                    continue;
                }
                question_count = question_count.saturating_add(1);
                let samples = collect_question_samples(
                    input,
                    latest,
                    selected,
                    excluded,
                    known,
                    &question.id.to_string(),
                    mode,
                );
                for sample in samples {
                    answer_count = answer_count.saturating_add(1);
                    max_sum = max_sum.saturating_add(i128::from(sample.max));
                    if sample.awarded < sample.max {
                        wrong_count = wrong_count.saturating_add(1);
                        lost = lost.saturating_add(i128::from(sample.max - sample.awarded));
                    }
                }
            }
            if answer_count == 0 {
                continue;
            }
            rows.push(TagStats {
                tag: tag.clone(),
                mode,
                question_count,
                answer_count,
                wrong_count,
                wrong_ppm: rate_ppm(usize_to_i128(wrong_count), usize_to_i128(answer_count)),
                lost_ppm: rate_ppm(lost, max_sum),
            });
        }
    }
    Ok(rows)
}

fn build_distribution(students: &[StudentResult], class: &[ClassStats]) -> Vec<DistributionBin> {
    let mut rows = Vec::new();
    for stats in class {
        let Some(mode) = stats.mode else {
            continue;
        };
        let samples: Vec<&StudentResult> = students
            .iter()
            .filter(|student| {
                student.outcome.in_class_sample()
                    && student.mode == Some(mode)
                    && student.rate_ppm.is_some()
            })
            .collect();
        if samples.is_empty() {
            continue;
        }
        let mut counts = [0usize; DISTRIBUTION_BINS as usize];
        for student in &samples {
            let rate = student.rate_ppm.unwrap_or(0);
            let bin = (rate / 100_000).min(u32::from(DISTRIBUTION_BINS - 1));
            if let Some(slot) = counts.get_mut(usize::try_from(bin).unwrap_or(0)) {
                *slot = slot.saturating_add(1);
            }
        }
        for (index, count) in counts.iter().enumerate() {
            let low = index.saturating_mul(10);
            let high = low.saturating_add(10);
            rows.push(DistributionBin {
                bin: u8::try_from(index).unwrap_or(0),
                range: if index == usize::from(DISTRIBUTION_BINS - 1) {
                    format!("{low}%–100%")
                } else {
                    format!("{low}%–{high}%")
                },
                mode,
                count: *count,
                ppm: rate_ppm(usize_to_i128(*count), usize_to_i128(samples.len())),
            });
        }
    }
    rows
}

fn build_sampling(
    input: &ReportInput,
    latest: &BTreeMap<(String, String), &GradeRecord>,
    students: &[StudentResult],
    excluded: &BTreeSet<String>,
    known: &BTreeSet<String>,
) -> Vec<FastSamplingRow> {
    let student_names: BTreeMap<String, (String, String)> = students
        .iter()
        .filter_map(|student| {
            student.student_id.map(|id| {
                (
                    id.to_string(),
                    (student.name.clone(), student.student_no.clone()),
                )
            })
        })
        .collect();
    let mut rows = Vec::new();
    for submission in &input.submissions {
        let submission_key = submission.id.to_string();
        if excluded.contains(&submission_key) {
            continue;
        }
        let mut sampled_count = 0usize;
        let mut pending = false;
        let mut converted = false;
        for ((key, question_key), record) in latest {
            if key != &submission_key || !record.sampled {
                continue;
            }
            sampled_count = sampled_count.saturating_add(1);
            if record.status == GradeStatus::NeedsReview {
                pending = true;
            }
            let standard_key = (submission_key.clone(), question_key.clone());
            if latest
                .get(&standard_key)
                .is_some_and(|standard| standard.mode == GradeMode::Standard)
            {
                converted = true;
            }
        }
        if sampled_count == 0 {
            continue;
        }
        let roster_identity = submission
            .student_id
            .map(|id| id.to_string())
            .filter(|id| known.contains(id))
            .and_then(|id| student_names.get(&id).cloned());
        let (student_name, student_no) =
            roster_identity.unwrap_or_else(|| (submission.source_stem.clone(), "—".to_owned()));
        rows.push(FastSamplingRow {
            submission_id: submission.id,
            submission_label: submission.source_stem.clone(),
            student_name,
            student_no,
            sampled_label: "已抽检",
            status_label: if pending { "待复核" } else { "通过" },
            converted_label: if converted {
                "已转标准复核"
            } else {
                "未转"
            },
            sampled_count,
        });
    }
    rows
}

/// 元信息行集合（工作表「元信息」，规范 §3.5）。
#[must_use]
pub fn meta_rows(
    report: &Report,
    exported_at: eg_domain::Timestamp,
    masked: bool,
    naming_rule_version: u32,
) -> Vec<(String, String)> {
    let mut rows = vec![
        ("试卷名".to_owned(), report.paper.title.clone()),
        ("试卷 id".to_owned(), report.paper.id.to_string()),
        ("班级".to_owned(), report.paper.classroom_name.clone()),
        (
            "整卷满分".to_owned(),
            report.paper.total_score.to_score_string(),
        ),
        (
            "题目数".to_owned(),
            report.paper.questions.len().to_string(),
        ),
        (
            "答案键版本".to_owned(),
            report.paper.key_version.to_string(),
        ),
        (
            "规则版本".to_owned(),
            report.paper.rubric_version.to_string(),
        ),
        ("名单版本".to_owned(), report.students.len().to_string()),
        (
            "模式筛选".to_owned(),
            format!(
                "标准 + 快速（分列统计，不混合计算）{}",
                if report.fast_label_visible() {
                    ""
                } else {
                    "；快速模式标注已关闭（仅影响文件名与表头）"
                }
            ),
        ),
        (
            "口径版本".to_owned(),
            report.settings.policy_version.to_string(),
        ),
        ("命名规则版本".to_owned(), naming_rule_version.to_string()),
        ("批次时间".to_owned(), exported_at.to_rfc3339()),
        ("快照哈希".to_owned(), report.snapshot_hash.to_hex()),
        ("抽检率".to_owned(), report.settings.display_sampling_rate()),
        (
            "低置信度权重".to_owned(),
            report.settings.display_low_confidence_weight(),
        ),
        (
            "抽样方法".to_owned(),
            crate::settings::SAMPLING_METHOD.to_owned(),
        ),
        (
            "快速标注开关".to_owned(),
            if report.settings.fast_export_label {
                "开".to_owned()
            } else {
                "关".to_owned()
            },
        ),
        (
            "脱敏开关".to_owned(),
            if masked {
                "开".to_owned()
            } else {
                "关".to_owned()
            },
        ),
        ("及格线".to_owned(), report.settings.display_pass()),
        ("优秀线".to_owned(), report.settings.display_excellent()),
        (
            "有效整卷样本（标准）".to_owned(),
            report
                .class
                .first()
                .map_or_else(|| "不适用".to_owned(), |row| row.valid_count.to_string()),
        ),
        (
            "有效整卷样本（快速）".to_owned(),
            report
                .class
                .get(1)
                .map_or_else(|| "不适用".to_owned(), |row| row.valid_count.to_string()),
        ),
        ("待复核".to_owned(), report.pending_count().to_string()),
        (
            "抽检记录数".to_owned(),
            report
                .class
                .first()
                .map_or(0, |row| row.sampled_record_count)
                .to_string(),
        ),
        (
            "报表说明".to_owned(),
            format!(
                "{}；抽检率 {}；低置信度权重 {}；标准与快速分列统计，不产出混合得分率",
                crate::settings::SAMPLING_METHOD,
                report.settings.display_sampling_rate(),
                report.settings.display_low_confidence_weight()
            ),
        ),
    ];
    if masked {
        rows.push(("脱敏提示".to_owned(), crate::masking::MASK_NOTE.to_owned()));
    }
    rows
}

/// 模式行文本（成绩单/班级汇总/选项分布/错题标签/班级分布共用）。
#[must_use]
pub fn mode_text(mode: Option<GradeMode>) -> String {
    match mode {
        Some(mode) => mode_label(mode).to_owned(),
        None => "全部".to_owned(),
    }
}

/// 状态列展示文本（成绩单）。
#[must_use]
pub fn outcome_label(outcome: StudentOutcome) -> &'static str {
    outcome.label()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_samples() {
        assert!(StudentOutcome::Complete.in_class_sample());
        assert!(StudentOutcome::Incomplete.in_class_sample());
        assert!(!StudentOutcome::NeedsReview.in_class_sample());
        assert!(!StudentOutcome::MixedMode.in_class_sample());
        assert_eq!(StudentOutcome::NotInRoster.label(), "名单外");
        assert_eq!(mode_text(None), "全部");
        assert_eq!(mode_text(Some(GradeMode::Fast)), "快速");
    }

    #[test]
    fn option_order_prefers_paper_order() {
        let question = QuestionSnapshot {
            id: eg_domain::QuestionId::from_ulid(eg_domain::Ulid::from_u128(1)),
            ordinal: 1,
            label: "1".to_owned(),
            kind: eg_domain::QuestionKind::SingleChoice,
            max_score: MilliScore::from_milli(5000).unwrap(),
            knowledge_tags: Vec::new(),
            answer_options: vec!["A".to_owned(), "B".to_owned()],
            correct_options: vec!["A".to_owned()],
        };
        let mut labels = vec![
            "不确定".to_owned(),
            "B".to_owned(),
            "空".to_owned(),
            "A".to_owned(),
        ];
        labels.sort_by_key(|label| option_order(&question, label));
        assert_eq!(labels, vec!["A", "B", "空", "不确定"]);
    }
}
