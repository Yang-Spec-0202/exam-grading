//! 测试夹具：构造一份双模式混合快照（`SPEC-report.md` §7 夹具矩阵）。
//!
//! 覆盖：36 名标准模式 + 8 名快速模式 + 1 名缺考、待复核、提交级排除、混合模式、
//! 缺页（不完整）、旧修订、名单外提交、快速抽检、选择题作答（含空/不确定）。

#![allow(
    dead_code,
    unreachable_pub,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::redundant_closure
)]

use chrono::{TimeZone as _, Utc};
use eg_domain::{
    AssetId, ClassroomId, Confidence, EvidenceRef, ExcludeReason, GradeMode, GradeRecord,
    GradeStatus, Judgement, MilliScore, PaperId, QuestionId, QuestionKind, RosterSource,
    RouteDecision, RouteTrace, Sha256Hex, Student, StudentId, SubmissionExclusion, SubmissionId,
    SubmissionStatus, Timestamp, Ulid,
};
use eg_report::{
    ChoiceAnswer, ChoiceRecord, PaperSnapshot, QuestionSnapshot, ReportInput, SubmissionSummary,
};

/// 班级 id。
pub fn classroom_id() -> ClassroomId {
    ClassroomId::from_ulid(Ulid::from_u128(1))
}

/// 试卷 id。
pub fn paper_id() -> PaperId {
    PaperId::from_ulid(Ulid::from_u128(2))
}

/// 固定夹具时间 2026-11-02T12:00:00Z。
pub fn timestamp() -> Timestamp {
    match Utc.with_ymd_and_hms(2026, 11, 2, 12, 0, 0) {
        chrono::LocalResult::Single(value) => value,
        _ => Utc.timestamp_opt(0, 0).single().unwrap_or_default(),
    }
}

/// 批量 id。
pub fn student_id(index: usize) -> StudentId {
    StudentId::from_ulid(Ulid::from_u128(1000 + u128::try_from(index).unwrap_or(0)))
}

/// 题目 id。
pub fn question_id(index: usize) -> QuestionId {
    QuestionId::from_ulid(Ulid::from_u128(2000 + u128::try_from(index).unwrap_or(0)))
}

/// 提交 id。
pub fn submission_id(index: usize) -> SubmissionId {
    SubmissionId::from_ulid(Ulid::from_u128(3000 + u128::try_from(index).unwrap_or(0)))
}

/// 判分记录 id。
pub fn grade_id(index: usize) -> eg_domain::GradeId {
    eg_domain::GradeId::from_ulid(Ulid::from_u128(9000 + u128::try_from(index).unwrap_or(0)))
}

fn milli(value: i64) -> MilliScore {
    MilliScore::from_milli(value).unwrap_or(MilliScore::ZERO)
}

/// 20 道题：第 1 题单选（选项 A–D），其余为计算题。
pub fn questions() -> Vec<QuestionSnapshot> {
    let mut questions = Vec::with_capacity(20);
    questions.push(QuestionSnapshot {
        id: question_id(0),
        ordinal: 1,
        label: "1".to_owned(),
        kind: QuestionKind::SingleChoice,
        max_score: milli(5000),
        knowledge_tags: vec!["集合".to_owned(), "运算".to_owned()],
        answer_options: vec![
            "A".to_owned(),
            "B".to_owned(),
            "C".to_owned(),
            "D".to_owned(),
        ],
        correct_options: vec!["A".to_owned()],
    });
    for index in 1..20 {
        let ordinal = u32::try_from(index).unwrap_or(1) + 1;
        questions.push(QuestionSnapshot {
            id: question_id(index),
            ordinal,
            label: format!("{ordinal}"),
            kind: QuestionKind::Calculation,
            max_score: milli(5000),
            knowledge_tags: if index % 2 == 0 {
                vec!["函数".to_owned()]
            } else {
                vec!["几何".to_owned()]
            },
            answer_options: Vec::new(),
            correct_options: Vec::new(),
        });
    }
    questions
}

/// 试卷快照（20 题 × 5 分 = 100 分）。
pub fn paper() -> PaperSnapshot {
    PaperSnapshot {
        id: paper_id(),
        classroom_id: classroom_id(),
        title: "期中/数学:2026".to_owned(),
        classroom_name: "高一(1)班".to_owned(),
        total_score: milli(100_000),
        key_version: 3,
        rubric_version: 2,
        questions: questions(),
    }
}

fn student(index: usize) -> Student {
    let number = format!("2024{:04}", index + 1);
    let name = match index {
        0 => "张三".to_owned(),
        1 => "李四".to_owned(),
        2 => "王五".to_owned(),
        3 => "赵六".to_owned(),
        _ => format!("考生{:02}", index + 1),
    };
    Student {
        id: student_id(index),
        classroom_id: classroom_id(),
        student_no: number,
        name,
        source: RosterSource::RosterImport,
    }
}

fn route_trace(submission: SubmissionId) -> RouteTrace {
    RouteTrace {
        rule_id: "R1".to_owned(),
        inputs_hash: Sha256Hex::of(submission.to_string().as_bytes()),
        classifier_call: None,
        decided_at: timestamp(),
    }
}

fn judgement(awarded: MilliScore, max: MilliScore) -> Judgement {
    if awarded == max {
        return Judgement::Match {
            score: awarded,
            reason: "与标准答案一致".to_owned(),
            evidence: vec![EvidenceRef::Asset(AssetId::of(b"fixture-asset"))],
        };
    }
    if awarded.is_zero() {
        return Judgement::Mismatch {
            score: awarded,
            reason: "与标准答案不一致".to_owned(),
            evidence: vec![EvidenceRef::Asset(AssetId::of(b"fixture-asset"))],
        };
    }
    Judgement::Partial {
        score: awarded,
        satisfied: Vec::new(),
        missing: Vec::new(),
        reason: "部分正确".to_owned(),
    }
}

/// 构造一条合法判分记录。
pub fn record(
    id: usize,
    submission: SubmissionId,
    question: QuestionId,
    mode: GradeMode,
    awarded: i64,
    status: GradeStatus,
    sampled: bool,
    revision: u32,
) -> GradeRecord {
    let max = milli(5000);
    let awarded = milli(awarded);
    let reviewed_at = if status == GradeStatus::ManuallyReviewed {
        Some(timestamp())
    } else {
        None
    };
    GradeRecord {
        id: grade_id(id),
        submission_id: submission,
        question_id: question,
        revision,
        mode,
        route: RouteDecision::RuleOnly,
        route_trace: route_trace(submission),
        judgement: judgement(awarded, max),
        awarded,
        max_score: max,
        confidence: Confidence::new(0.95).unwrap_or(Confidence::ZERO),
        status,
        evidence: vec![EvidenceRef::Asset(AssetId::of(b"fixture-asset"))],
        engines: Vec::new(),
        created_at: timestamp(),
        reviewed_at,
        review_note: None,
        sampled,
    }
}

/// 覆盖全部夹具场景的报表快照。
pub struct Scenario {
    /// 一致快照。
    pub input: ReportInput,
    /// 固定的批次时间（命名/元信息）。
    pub exported_at: Timestamp,
}

impl Scenario {
    /// 双模式混合夹具。
    pub fn build() -> Self {
        const STANDARD_STUDENTS: usize = 36;
        const FAST_STUDENTS: usize = 44; // 36..43 为快速模式
        const ABSENT_STUDENT: usize = 44; // 名单内无提交

        let paper = paper();
        let questions = paper.questions.clone();
        let mut students: Vec<Student> = (0..=ABSENT_STUDENT).map(student).collect();
        students.sort_by(|left, right| left.student_no.cmp(&right.student_no));

        let mut submissions: Vec<SubmissionSummary> = Vec::new();
        let mut records: Vec<GradeRecord> = Vec::new();
        let mut choices: Vec<ChoiceRecord> = Vec::new();
        let mut exclusions: Vec<SubmissionExclusion> = Vec::new();
        let mut record_index = 0usize;

        for index in 0..=ABSENT_STUDENT {
            if index == ABSENT_STUDENT {
                continue;
            }
            let mode = if index < STANDARD_STUDENTS {
                GradeMode::Standard
            } else {
                GradeMode::Fast
            };
            let submission = submission_id(index);
            submissions.push(SubmissionSummary {
                id: submission,
                paper_id: paper_id(),
                student_id: Some(student_id(index)),
                status: SubmissionStatus::Graded,
                source_stem: format!("考生{:02}", index + 1),
                created_at: timestamp(),
            });
            for (question_index, question) in questions.iter().enumerate() {
                // 场景注入
                if index == 0 && question_index == 7 {
                    // 待复核
                    records.push(record(
                        record_index,
                        submission,
                        question.id,
                        mode,
                        5000,
                        GradeStatus::NeedsReview,
                        false,
                        1,
                    ));
                    record_index += 1;
                    continue;
                }
                if index == 2 && question_index >= 15 {
                    // 混合模式：后 5 题改用快速记录
                    records.push(record(
                        record_index,
                        submission,
                        question.id,
                        GradeMode::Fast,
                        4000,
                        GradeStatus::AutoAccepted,
                        false,
                        1,
                    ));
                    record_index += 1;
                    continue;
                }
                if index == 3 && question_index == 19 {
                    // 缺页：不产生记录
                    continue;
                }
                let awarded = match (index, question_index) {
                    (4, 4) => 0,
                    (5, 5) => 2500,
                    (_, q) if q % 7 == 0 => 3000,
                    _ => 5000,
                };
                records.push(record(
                    record_index,
                    submission,
                    question.id,
                    mode,
                    awarded,
                    GradeStatus::AutoAccepted,
                    mode == GradeMode::Fast && index < 39 && question_index % 2 == 0,
                    1,
                ));
                record_index += 1;
                if index == 5 && question_index == 0 {
                    // 旧修订（错误得分）不应进入统计
                    records.push(record(
                        record_index,
                        submission,
                        question.id,
                        mode,
                        0,
                        GradeStatus::AutoAccepted,
                        false,
                        2,
                    ));
                    record_index += 1;
                }
            }
            // 选择题作答投影
            let answer = match index % 5 {
                0 => ChoiceAnswer::Options(vec!["A".to_owned()]),
                1 => ChoiceAnswer::Options(vec!["B".to_owned()]),
                2 => ChoiceAnswer::Options(vec!["A".to_owned(), "C".to_owned()]),
                3 => ChoiceAnswer::Blank,
                _ => ChoiceAnswer::Unparsed,
            };
            choices.push(ChoiceRecord {
                submission_id: submission,
                question_id: questions[0].id,
                answer,
            });
        }

        // 提交级排除：李四（index 1）
        exclusions.push(SubmissionExclusion {
            submission_id: submission_id(1),
            reason: ExcludeReason::Invalid,
            note: "答卷缺页且无法补拍".to_owned(),
            at: timestamp(),
        });

        // 名单外提交（无学生 id）
        let orphan = submission_id(900);
        submissions.push(SubmissionSummary {
            id: orphan,
            paper_id: paper_id(),
            student_id: None,
            status: SubmissionStatus::Graded,
            source_stem: "未命名-01".to_owned(),
            created_at: timestamp(),
        });
        for question in &questions {
            records.push(record(
                record_index,
                orphan,
                question.id,
                GradeMode::Standard,
                5000,
                GradeStatus::AutoAccepted,
                false,
                1,
            ));
            record_index += 1;
        }

        Self {
            input: ReportInput {
                paper,
                students,
                submissions,
                records,
                exclusions,
                choices,
            },
            exported_at: Utc
                .with_ymd_and_hms(2026, 11, 1, 19, 30, 0)
                .single()
                .unwrap_or_else(|| timestamp()),
        }
    }
}

/// 生成 `count` 份提交的合成数据（性能测试；每题满分、全标准模式）。
pub fn synthetic_input(count: usize, questions_per_paper: usize) -> ReportInput {
    let mut question_list = Vec::with_capacity(questions_per_paper);
    for index in 0..questions_per_paper {
        let ordinal = u32::try_from(index).unwrap_or(1) + 1;
        question_list.push(QuestionSnapshot {
            id: question_id(index + 100),
            ordinal,
            label: format!("{ordinal}"),
            kind: QuestionKind::Calculation,
            max_score: milli(5000),
            knowledge_tags: vec![format!("知识点{}", index % 5)],
            answer_options: Vec::new(),
            correct_options: Vec::new(),
        });
    }
    let total = i64::try_from(questions_per_paper).unwrap_or(20) * 5000;
    let paper = PaperSnapshot {
        id: paper_id(),
        classroom_id: classroom_id(),
        title: "性能测试卷".to_owned(),
        classroom_name: "性能测试班".to_owned(),
        total_score: milli(total),
        key_version: 1,
        rubric_version: 1,
        questions: question_list.clone(),
    };
    let mut students = Vec::with_capacity(count);
    let mut submissions = Vec::with_capacity(count);
    let mut records = Vec::with_capacity(count.saturating_mul(questions_per_paper));
    let mut record_index = 0usize;
    for index in 0..count {
        students.push(Student {
            id: student_id(index),
            classroom_id: classroom_id(),
            student_no: format!("P{:06}", index + 1),
            name: format!("性能考生{:04}", index + 1),
            source: RosterSource::RosterImport,
        });
        let submission = submission_id(index);
        submissions.push(SubmissionSummary {
            id: submission,
            paper_id: paper_id(),
            student_id: Some(student_id(index)),
            status: SubmissionStatus::Graded,
            source_stem: format!("性能考生{:04}", index + 1),
            created_at: timestamp(),
        });
        for (question_index, question) in question_list.iter().enumerate() {
            let awarded = if (index + question_index) % 4 == 0 {
                3000
            } else {
                5000
            };
            records.push(record(
                record_index,
                submission,
                question.id,
                GradeMode::Standard,
                awarded,
                GradeStatus::AutoAccepted,
                false,
                1,
            ));
            record_index += 1;
        }
    }
    ReportInput {
        paper,
        students,
        submissions,
        records,
        exclusions: Vec::new(),
        choices: Vec::new(),
    }
}

/// 临时目录（测试用；调用方负责清理）。
pub fn temp_dir(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    std::env::temp_dir().join(format!("eg-report-it-{tag}-{nanos}"))
}
