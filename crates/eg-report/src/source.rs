//! 数据源端口与一致快照（`SPEC-report.md` §3.1：由 `app` 实现，`report` 不直连数据库）。
//!
//! `report` 只消费一个**一致快照** [`ReportInput`]：试卷、班级名单、提交、判分记录（全部修订）、
//! `SubmissionExclusion` 与学生作答投影。读取由调用方（`app` 的适配器，TODO）完成；
//! 本 crate 提供 [`ReportSource`] 端口与测试用 [`StaticReportSource`] 夹具。
//!
//! 快照哈希（`SHA-256`，canonical 文本）在加载时写入元信息与导出审计，用于重放对比。

use crate::error::{ReportError, Result};
use crate::rounding::sum_scores;
use eg_domain::{
    ClassroomId, ExcludeReason, GradeMode, GradeRecord, GradeStatus, MilliScore, PaperId,
    QuestionId, QuestionKind, Sha256Hex, Student, StudentId, SubmissionExclusion, SubmissionId,
    SubmissionStatus, Timestamp,
};
use std::collections::{BTreeMap, BTreeSet};

/// 题目快照（报表只读所需字段）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionSnapshot {
    /// 题目 id。
    pub id: QuestionId,
    /// 卷面顺序（从 1 起）。
    pub ordinal: u32,
    /// 题号标签（如 `17(2)`）。
    pub label: String,
    /// 题型。
    pub kind: QuestionKind,
    /// 满分。
    pub max_score: MilliScore,
    /// 知识点标签（错题标签统计用）。
    pub knowledge_tags: Vec<String>,
    /// 选项（选择题；按卷面顺序）。
    pub answer_options: Vec<String>,
    /// 正确答案选项（选择题；组合答案按选项集合给出）。
    pub correct_options: Vec<String>,
}

/// 试卷快照（含班级名，报表元信息用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaperSnapshot {
    /// 试卷 id。
    pub id: PaperId,
    /// 班级 id。
    pub classroom_id: ClassroomId,
    /// 试卷标题（导出命名的主体）。
    pub title: String,
    /// 班级名。
    pub classroom_name: String,
    /// 试卷总分（必须等于各题满分之和）。
    pub total_score: MilliScore,
    /// 答案键修订号。
    pub key_version: u32,
    /// 规则版本。
    pub rubric_version: u32,
    /// 题目列表（按 `ordinal` 升序）。
    pub questions: Vec<QuestionSnapshot>,
}

/// 提交摘要（只读；不含原图）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmissionSummary {
    /// 提交 id。
    pub id: SubmissionId,
    /// 试卷 id。
    pub paper_id: PaperId,
    /// 已确认的学生 id（未匹配为空）。
    pub student_id: Option<StudentId>,
    /// 提交状态。
    pub status: SubmissionStatus,
    /// 去扩展名文件名（身份来源；成绩单「提交」列展示用）。
    pub source_stem: String,
    /// 创建时间。
    pub created_at: Timestamp,
}

/// 学生作答投影中的选择题作答（`StudentAnswer` 由 `align` 重建，见 SPEC-report §10.9）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChoiceAnswer {
    /// 已解析出的选项集合（多选按组合计）。
    Options(Vec<String>),
    /// 未作答。
    Blank,
    /// 无法解析。
    Unparsed,
}

/// 一道选择题的作答记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoiceRecord {
    /// 提交 id。
    pub submission_id: SubmissionId,
    /// 题目 id。
    pub question_id: QuestionId,
    /// 作答。
    pub answer: ChoiceAnswer,
}

/// 报表一致快照（唯一输入）。
///
/// 只实现 `PartialEq`：`GradeRecord.confidence` 是浮点，领域类型未实现 `Eq`。
#[derive(Debug, Clone, PartialEq)]
pub struct ReportInput {
    /// 试卷快照。
    pub paper: PaperSnapshot,
    /// 班级名单。
    pub students: Vec<Student>,
    /// 提交摘要。
    pub submissions: Vec<SubmissionSummary>,
    /// 判分记录（全部修订）。
    pub records: Vec<GradeRecord>,
    /// 提交级排除。
    pub exclusions: Vec<SubmissionExclusion>,
    /// 学生作答投影（可选；为空时选项分布输出「不适用」）。
    pub choices: Vec<ChoiceRecord>,
}

/// 报表数据源端口（由 `app` 实现；本 crate 不直连数据库）。
pub trait ReportSource {
    /// 读取一致快照；实现方负责事务内一致性（同一读事务）。
    ///
    /// # Errors
    ///
    /// 数据库/文件读取失败时返回 [`ReportError::Source`]。
    fn load(&self) -> Result<ReportInput>;
}

/// 静态数据源：测试夹具与只读快照回放。
#[derive(Debug, Clone)]
pub struct StaticReportSource {
    input: ReportInput,
}

impl StaticReportSource {
    /// 由已构造的快照创建数据源。
    #[must_use]
    pub const fn new(input: ReportInput) -> Self {
        Self { input }
    }

    /// 取回快照（测试断言用）。
    #[must_use]
    pub const fn input(&self) -> &ReportInput {
        &self.input
    }
}

impl ReportSource for StaticReportSource {
    fn load(&self) -> Result<ReportInput> {
        Ok(self.input.clone())
    }
}

impl ReportInput {
    /// 快照哈希（`SHA-256`，canonical 文本；用于元信息与重放对比）。
    #[must_use]
    pub fn snapshot_hash(&self) -> Sha256Hex {
        Sha256Hex::of(self.canonical_text().as_bytes())
    }

    /// canonical 文本：字段固定、分组排序固定，保证同一快照逐字节一致。
    fn canonical_text(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let paper = format!(
            "paper:{}\t{}\t{}\t{}\t{}\t{}\n",
            self.paper.id,
            self.paper.title,
            self.paper.classroom_name,
            self.paper.total_score.as_milli(),
            self.paper.key_version,
            self.paper.rubric_version
        );
        out.push_str(&paper);
        for question in &self.paper.questions {
            let _ = writeln!(
                out,
                "question:{}\t{}\t{}\t{:?}\t{}\t{}\t{}\t{}",
                question.id,
                question.ordinal,
                question.label,
                question.kind,
                question.max_score.as_milli(),
                question.knowledge_tags.join("|"),
                question.answer_options.join("|"),
                question.correct_options.join("|")
            );
        }
        for student in &self.students {
            let _ = writeln!(
                out,
                "student:{}\t{}\t{}\t{}",
                student.id, student.student_no, student.name, student.classroom_id
            );
        }
        for submission in &self.submissions {
            let _ = writeln!(
                out,
                "submission:{}\t{}\t{}\t{:?}\t{}\t{}",
                submission.id,
                submission
                    .student_id
                    .map_or_else(String::new, |id| id.to_string()),
                submission.paper_id,
                submission.status,
                submission.source_stem,
                submission.created_at
            );
        }
        for record in &self.records {
            let _ = writeln!(
                out,
                "record:{}\t{}\t{}\t{:?}\t{:?}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                record.submission_id,
                record.question_id,
                record.revision,
                record.mode,
                record.status,
                record.awarded.as_milli(),
                record.max_score.as_milli(),
                record.confidence.get(),
                record.sampled,
                record.route_trace.rule_id,
                record.id,
                record.created_at
            );
        }
        for exclusion in &self.exclusions {
            let _ = writeln!(
                out,
                "exclusion:{}\t{:?}\t{}\t{}",
                exclusion.submission_id, exclusion.reason, exclusion.note, exclusion.at
            );
        }
        for choice in &self.choices {
            let answer = match &choice.answer {
                ChoiceAnswer::Options(options) => options.join("|"),
                ChoiceAnswer::Blank => "空".to_owned(),
                ChoiceAnswer::Unparsed => "不确定".to_owned(),
            };
            let _ = writeln!(
                out,
                "choice:{}\t{}\t{}",
                choice.submission_id, choice.question_id, answer
            );
        }
        out
    }

    /// 校验快照（规范 §4「校验」、§5「守恒/越界/引用」）。
    ///
    /// # Errors
    ///
    /// 守恒不成立、引用悬空、判分记录不满足领域不变量时返回
    /// [`ReportError::Invariant`]；不截断、不静默修正。
    pub fn validate(&self) -> Result<()> {
        if self.paper.title.trim().is_empty() {
            return Err(ReportError::Invariant("试卷标题为空".to_owned()));
        }
        if self.paper.questions.is_empty() {
            return Err(ReportError::Invariant("试卷没有题目".to_owned()));
        }
        let mut total: i128 = 0;
        let mut question_ids: BTreeSet<String> = BTreeSet::new();
        for (index, question) in self.paper.questions.iter().enumerate() {
            let expected = u32::try_from(index + 1)
                .map_err(|_| ReportError::Invariant("题目数量超出 u32 序号上限".to_owned()))?;
            if question.ordinal != expected {
                return Err(ReportError::Invariant(format!(
                    "题目序号不连续：期望 {expected}，实际 {}",
                    question.ordinal
                )));
            }
            if question.max_score.as_milli() <= 0 {
                return Err(ReportError::Invariant(format!(
                    "题目 {} 满分必须大于 0",
                    question.label
                )));
            }
            if question.label.trim().is_empty() {
                return Err(ReportError::Invariant("题目标签为空".to_owned()));
            }
            if !question_ids.insert(question.id.to_string()) {
                return Err(ReportError::Invariant(format!(
                    "题目 id 重复：{}",
                    question.id
                )));
            }
            total = total
                .checked_add(i128::from(question.max_score.as_milli()))
                .ok_or_else(|| ReportError::Invariant("题分求和无溢出保护失败".to_owned()))?;
        }
        if total != i128::from(self.paper.total_score.as_milli()) {
            return Err(ReportError::Invariant(format!(
                "题分之和（{total} 千分）与整卷总分（{} 千分）不符",
                self.paper.total_score.as_milli()
            )));
        }
        if self.paper.total_score.as_milli() <= 0 {
            return Err(ReportError::Invariant("试卷总分必须大于 0".to_owned()));
        }

        let mut student_ids: BTreeSet<String> = BTreeSet::new();
        for student in &self.students {
            if student.name.trim().is_empty() {
                return Err(ReportError::Invariant(format!(
                    "学号 {} 的姓名为空",
                    student.student_no
                )));
            }
            if !student_ids.insert(student.id.to_string()) {
                return Err(ReportError::Invariant(format!(
                    "学生 id 重复：{}",
                    student.id
                )));
            }
        }

        let question_max: BTreeMap<String, i64> = self
            .paper
            .questions
            .iter()
            .map(|question| (question.id.to_string(), question.max_score.as_milli()))
            .collect();
        let mut submission_ids: BTreeSet<String> = BTreeSet::new();
        for submission in &self.submissions {
            if !submission_ids.insert(submission.id.to_string()) {
                return Err(ReportError::Invariant(format!(
                    "提交 id 重复：{}",
                    submission.id
                )));
            }
            if submission.paper_id != self.paper.id {
                return Err(ReportError::Invariant(format!(
                    "提交 {} 的试卷 id 与快照不一致",
                    submission.id
                )));
            }
        }

        let mut seen: BTreeSet<(String, String, u32)> = BTreeSet::new();
        for record in &self.records {
            record
                .validate()
                .map_err(|error| ReportError::Invariant(format!("判分记录不合法：{error}")))?;
            if !submission_ids.contains(&record.submission_id.to_string()) {
                return Err(ReportError::Invariant(format!(
                    "判分记录引用了不存在的提交：{}",
                    record.submission_id
                )));
            }
            let max = question_max
                .get(&record.question_id.to_string())
                .ok_or_else(|| {
                    ReportError::Invariant(format!(
                        "判分记录引用了不属于本卷的题目：{}",
                        record.question_id
                    ))
                })?;
            if record.max_score.as_milli() != *max {
                return Err(ReportError::Invariant(format!(
                    "判分记录 {} 的满分（{} 千分）与题目满分（{max} 千分）不符",
                    record.id,
                    record.max_score.as_milli()
                )));
            }
            if !seen.insert((
                record.submission_id.to_string(),
                record.question_id.to_string(),
                record.revision,
            )) {
                return Err(ReportError::Invariant(format!(
                    "判分记录修订重复：提交 {} 题目 {} 修订 {}",
                    record.submission_id, record.question_id, record.revision
                )));
            }
        }

        let mut excluded: BTreeSet<String> = BTreeSet::new();
        for exclusion in &self.exclusions {
            if !submission_ids.contains(&exclusion.submission_id.to_string()) {
                return Err(ReportError::Invariant(format!(
                    "排除记录引用了不存在的提交：{}",
                    exclusion.submission_id
                )));
            }
            if !excluded.insert(exclusion.submission_id.to_string()) {
                return Err(ReportError::Invariant(format!(
                    "提交存在重复排除记录：{}",
                    exclusion.submission_id
                )));
            }
        }

        for choice in &self.choices {
            if !submission_ids.contains(&choice.submission_id.to_string()) {
                return Err(ReportError::Invariant(format!(
                    "作答投影引用了不存在的提交：{}",
                    choice.submission_id
                )));
            }
            if !question_max.contains_key(&choice.question_id.to_string()) {
                return Err(ReportError::Invariant(format!(
                    "作答投影引用了不属于本卷的题目：{}",
                    choice.question_id
                )));
            }
            if let ChoiceAnswer::Options(options) = &choice.answer
                && options.is_empty()
            {
                return Err(ReportError::Invariant(
                    "作答投影的选项集合为空，应记「空」或「不确定」".to_owned(),
                ));
            }
        }

        // 复用 sum_scores 的溢出语义（题分总和已在上面逐题累加校验）。
        let _ = sum_scores(
            self.paper
                .questions
                .iter()
                .map(|question| question.max_score),
        )?;
        Ok(())
    }
}

/// 供调用方识别「名单外提交」的原因标签。
#[must_use]
pub fn exclusion_reason_label(reason: ExcludeReason) -> &'static str {
    match reason {
        ExcludeReason::Absent => "缺考",
        ExcludeReason::Invalid => "无效",
        ExcludeReason::Duplicate => "重复",
        ExcludeReason::OutOfScope => "超范围",
        ExcludeReason::Other => "其他",
    }
}

/// 供调用方识别「判分状态」的展示标签（元信息与快速抽检用）。
#[must_use]
pub fn grade_status_label(status: GradeStatus) -> &'static str {
    match status {
        GradeStatus::AutoAccepted => "自动接受",
        GradeStatus::NeedsReview => "待复核",
        GradeStatus::ManuallyReviewed => "已人工复核",
        GradeStatus::Excluded => "排除",
    }
}

/// 模式展示标签（文件名与表头；见 `SPEC-report.md` §3.2/§3.3）。
#[must_use]
pub fn mode_label(mode: GradeMode) -> &'static str {
    match mode {
        GradeMode::Standard => "标准",
        GradeMode::Fast => "快速",
    }
}
