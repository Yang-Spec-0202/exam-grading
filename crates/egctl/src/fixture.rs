//! 报表夹具与导出运行（回放/基准/doctor 自检共用）。
//!
//! 夹具是确定性合成数据（不读数据库、不含个人真实信息），用于验证 `eg-report` 的
//! 统计口径、导出与重放；判分链路（`eg-grade`）未接入时这是唯一可用的端到端夹具。

use chrono::{TimeZone as _, Utc};
use eg_domain::{
    AssetId, ClassroomId, Confidence, EvidenceRef, GradeMode, GradeRecord, GradeStatus, Judgement,
    MilliScore, PaperId, QuestionId, QuestionKind, RosterSource, RouteDecision, RouteTrace,
    Sha256Hex, Student, StudentId, SubmissionId, SubmissionStatus, Timestamp, Ulid,
};
use eg_report::{
    ChoiceAnswer, ChoiceRecord, ExportFormat, ExportOutcome, ExportRequest, MemoryAuditor,
    PaperSnapshot, QuestionSnapshot, Report, ReportInput, ReportSettings, SubmissionSummary,
    export,
};

/// 批次时间（回放/命名固定值）。
#[must_use]
pub fn fixture_time() -> Timestamp {
    Utc.with_ymd_and_hms(2026, 11, 3, 9, 0, 0)
        .single()
        .unwrap_or_else(|| Utc.timestamp_opt(0, 0).single().unwrap_or_default())
}

fn classroom_id() -> ClassroomId {
    ClassroomId::from_ulid(Ulid::from_u128(1))
}

fn paper_id() -> PaperId {
    PaperId::from_ulid(Ulid::from_u128(2))
}

fn milli(value: i64) -> MilliScore {
    MilliScore::from_milli(value).unwrap_or(MilliScore::ZERO)
}

/// 构造确定性快照：`students` 名学生、`questions` 道题；每 `fast_every` 名取 1 名为快速模式。
///
/// 得分模式：全对为主，按 `(学生序号 + 题目序号) % 5` 制造部分得分与零分，便于
/// 统计口径（得分率/区分度/分布/错题标签）有真实分布。
#[must_use]
pub fn report_input(students: usize, questions: usize, fast_every: usize) -> ReportInput {
    let question_list: Vec<QuestionSnapshot> = (0..questions)
        .map(|index| {
            let ordinal = u32::try_from(index).unwrap_or(1) + 1;
            QuestionSnapshot {
                id: QuestionId::from_ulid(Ulid::from_u128(
                    100 + u128::try_from(index).unwrap_or(0),
                )),
                ordinal,
                label: format!("{ordinal}"),
                kind: if index == 0 {
                    QuestionKind::SingleChoice
                } else {
                    QuestionKind::Calculation
                },
                max_score: milli(5000),
                knowledge_tags: vec![format!("知识点{}", index % 4)],
                answer_options: if index == 0 {
                    vec![
                        "A".to_owned(),
                        "B".to_owned(),
                        "C".to_owned(),
                        "D".to_owned(),
                    ]
                } else {
                    Vec::new()
                },
                correct_options: if index == 0 {
                    vec!["A".to_owned()]
                } else {
                    Vec::new()
                },
            }
        })
        .collect();
    let total = i64::try_from(questions).unwrap_or(20) * 5000;
    let paper = PaperSnapshot {
        id: paper_id(),
        classroom_id: classroom_id(),
        title: "验收夹具数学".to_owned(),
        classroom_name: "验收班级".to_owned(),
        total_score: milli(total),
        key_version: 1,
        rubric_version: 1,
        questions: question_list.clone(),
    };

    let mut roster = Vec::with_capacity(students);
    let mut submissions = Vec::with_capacity(students);
    let mut records = Vec::with_capacity(students.saturating_mul(questions));
    let mut choices = Vec::with_capacity(students);
    let mut record_index = 0usize;
    for index in 0..students {
        let student_id =
            StudentId::from_ulid(Ulid::from_u128(1000 + u128::try_from(index).unwrap_or(0)));
        let submission_id =
            SubmissionId::from_ulid(Ulid::from_u128(50_000 + u128::try_from(index).unwrap_or(0)));
        let mode = if fast_every > 0 && index % fast_every == 0 {
            GradeMode::Fast
        } else {
            GradeMode::Standard
        };
        roster.push(Student {
            id: student_id,
            classroom_id: classroom_id(),
            student_no: format!("F{:05}", index + 1),
            name: format!("夹具学生{:04}", index + 1),
            source: RosterSource::RosterImport,
        });
        submissions.push(SubmissionSummary {
            id: submission_id,
            paper_id: paper_id(),
            student_id: Some(student_id),
            status: SubmissionStatus::Graded,
            source_stem: format!("夹具学生{:04}", index + 1),
            created_at: fixture_time(),
        });
        for (question_index, question) in question_list.iter().enumerate() {
            let awarded = match (index + question_index) % 5 {
                2 => 2500,
                3 => 0,
                _ => 5000,
            };
            records.push(make_record(
                record_index,
                submission_id,
                question.id,
                mode,
                awarded,
                GradeStatus::AutoAccepted,
                mode == GradeMode::Fast && question_index % 2 == 0,
            ));
            record_index += 1;
        }
        choices.push(ChoiceRecord {
            submission_id,
            question_id: question_list[0].id,
            answer: match index % 4 {
                0 => ChoiceAnswer::Options(vec!["A".to_owned()]),
                1 => ChoiceAnswer::Options(vec!["B".to_owned()]),
                2 => ChoiceAnswer::Blank,
                _ => ChoiceAnswer::Unparsed,
            },
        });
    }

    ReportInput {
        paper,
        students: roster,
        submissions,
        records,
        exclusions: Vec::new(),
        choices,
    }
}

fn make_record(
    index: usize,
    submission: SubmissionId,
    question: QuestionId,
    mode: GradeMode,
    awarded: i64,
    status: GradeStatus,
    sampled: bool,
) -> GradeRecord {
    let max = milli(5000);
    let awarded = milli(awarded);
    let judgement = if awarded == max {
        Judgement::Match {
            score: awarded,
            reason: "与标准答案一致".to_owned(),
            evidence: vec![EvidenceRef::Asset(AssetId::of(b"fixture"))],
        }
    } else if awarded.is_zero() {
        Judgement::Mismatch {
            score: awarded,
            reason: "与标准答案不一致".to_owned(),
            evidence: vec![EvidenceRef::Asset(AssetId::of(b"fixture"))],
        }
    } else {
        Judgement::Partial {
            score: awarded,
            satisfied: Vec::new(),
            missing: Vec::new(),
            reason: "部分正确".to_owned(),
        }
    };
    GradeRecord {
        id: eg_domain::GradeId::from_ulid(Ulid::from_u128(
            90_000 + u128::try_from(index).unwrap_or(0),
        )),
        submission_id: submission,
        question_id: question,
        revision: 1,
        mode,
        route: RouteDecision::RuleOnly,
        route_trace: RouteTrace {
            rule_id: "R1".to_owned(),
            inputs_hash: Sha256Hex::of(submission.to_string().as_bytes()),
            classifier_call: None,
            decided_at: fixture_time(),
        },
        judgement,
        awarded,
        max_score: max,
        confidence: Confidence::new(0.95).unwrap_or(Confidence::ZERO),
        status,
        evidence: vec![EvidenceRef::Asset(AssetId::of(b"fixture"))],
        engines: Vec::new(),
        created_at: fixture_time(),
        reviewed_at: None,
        review_note: None,
        sampled,
    }
}

/// 构建报表（默认口径）。
///
/// # Errors
///
/// 快照不合法或口径非法时返回中文错误。
pub fn build_report(input: &ReportInput) -> std::result::Result<Report, String> {
    Report::build(input, ReportSettings::default()).map_err(|error| error.to_string())
}

/// 运行一次报表导出（三格式），返回结果。
///
/// # Errors
///
/// 渲染/写盘/审计失败时返回中文错误。
pub fn export_report(
    report: &Report,
    out_dir: &std::path::Path,
    exported_at: Timestamp,
    masked: bool,
) -> std::result::Result<ExportOutcome, String> {
    let mut request = ExportRequest::new(out_dir, exported_at);
    request.formats = ExportFormat::all().to_vec();
    request.masked = masked;
    export(report, &request, &MemoryAuditor::new()).map_err(|error| error.to_string())
}

/// `doctor` 用的报表模块自检：构建 → 导出 2 次 → 内容摘要相等 → 清理。
///
/// # Errors
///
/// 任一环节失败时返回中文错误。
pub fn report_self_test() -> std::result::Result<String, String> {
    let input = report_input(6, 3, 2);
    let report = build_report(&input)?;
    let base = std::env::temp_dir().join(format!("egctl-report-self-{}", std::process::id()));
    let dir_a = base.join("a");
    let dir_b = base.join("b");
    let first = export_report(&report, &dir_a, fixture_time(), false)?;
    let second = export_report(&report, &dir_b, fixture_time(), false)?;
    eg_report::verify_replay(&first, &second).map_err(|error| error.to_string())?;
    let summary = format!(
        "{} 行 / {} 个文件 / 摘要 {}",
        first.row_count,
        first.files.len(),
        &first.content_digest.to_hex()[..12]
    );
    let _ = std::fs::remove_dir_all(&base);
    Ok(summary)
}
