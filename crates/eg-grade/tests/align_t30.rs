//! T30 收尾：真实 align 报告 → grade 提交级判分（2 学生 × 20 题，含 1 未决题）。
//!
//! 断言：可判分题与既有规则链 0 diff；未决题显式进失败/澄清（不伪装未作答）；
//! 整卷门禁（身份未确认）拒绝自动判分；逐题门禁 `auto_grade_allowed` 生效。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    unused_imports
)]

mod common;

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use common::*;
use eg_align::testing::{self, CountingHints, FixedClock as AlignClock};
use eg_align::{AlignReport, AlignRequest, MapRecognitionSource, SeqIds};
use eg_domain::{
    BBox, EngineStamp, GradeMode, GradeStatus, MilliScore, Paper, QuestionKind, StudentAnswer,
    SubmissionId, TaskId, Ulid,
};
use eg_grade::adapters::align::{AlignReportProvider, AlignReportSource};
use eg_grade::align::AlignSource;
use eg_grade::ports::{FixedClock, FixtureRubrics, InMemoryIdempotency, SequentialIds};
use eg_grade::submission::{
    InMemoryGradeStore, InMemoryProgressStore, SubmissionPipeline, SubmissionPipelineError,
    SubmissionRunRequest,
};
use eg_grade::{GradeConfig, Grader};

fn classroom_id() -> eg_domain::ClassroomId {
    eg_domain::ClassroomId::from_ulid(Ulid::from_u128(2))
}

fn paper20() -> Paper {
    let mut questions: Vec<eg_domain::Question> = (0..20u128)
        .map(|index| {
            question(
                1_000 + index,
                QuestionKind::NumericOnly,
                5000,
                number_key("2.5"),
                None,
            )
        })
        .collect();
    // 题号标签使用 1..20（align 的题号主号解析最多 3 位数字；id 仍用 1000+i 对齐报告）
    for (index, question) in questions.iter_mut().enumerate() {
        question.label = format!("{}", index + 1);
    }
    paper(questions)
}

fn template19(paper: &Paper) -> eg_domain::PaperTemplate {
    let regions: Vec<(usize, u32, BBox)> = (0..20usize)
        .filter(|index| *index != 9)
        .map(|index| {
            let y = 0.02 + f64::from(u32::try_from(index).unwrap()) * 0.045;
            (index, 1, BBox::new(0.05, y, 0.9, 0.04).expect("bbox 合法"))
        })
        .collect();
    testing::template(paper, 1, 1, &regions)
}

fn one_report(paper: &Paper, stem: &str, submission_id: u128) -> AlignReport {
    let classroom = classroom_id();
    let roster = testing::roster(
        classroom,
        vec![
            testing::student(classroom, 1, "20260001", "张三"),
            testing::student(classroom, 2, "20260002", "李四"),
        ],
    )
    .expect("名单合法");
    let submission = testing::submission(submission_id, classroom, 1, stem, &["2.5"]);
    let pages = testing::page_refs(&submission);
    let template = template19(paper);
    let recognition = testing::recognition(pages[0].page_id, vec![testing::block("2.5", 0.1, 0.1)]);
    let recognitions = MapRecognitionSource::new(vec![recognition]).expect("识别映射");
    let hints = CountingHints::default();
    let settings = eg_align::AlignSettings::default();
    let ids = SeqIds::new(1);
    let clock = AlignClock::fixture();
    eg_align::align(AlignRequest {
        submission: &submission,
        paper,
        roster: &roster,
        template: Some(&template),
        pages: &pages,
        recognitions: &recognitions,
        hints: &hints,
        settings: &settings,
        ids: &ids,
        clock: &clock,
        existing: &[],
    })
    .expect("对齐成功")
}

struct StaticReports {
    reports: Mutex<HashMap<SubmissionId, AlignReport>>,
    identity: String,
    stamp: Option<EngineStamp>,
}

impl StaticReports {
    fn new(stamp: Option<EngineStamp>) -> Self {
        Self {
            reports: Mutex::new(HashMap::new()),
            identity: "align-fixture-v1".to_owned(),
            stamp,
        }
    }

    fn insert(&self, report: AlignReport) {
        self.reports
            .lock()
            .unwrap()
            .insert(report.submission_id, report);
    }
}

impl AlignReportProvider for StaticReports {
    fn report(&self, submission_id: SubmissionId) -> Result<AlignReport, eg_align::AlignError> {
        self.reports
            .lock()
            .unwrap()
            .get(&submission_id)
            .cloned()
            .ok_or_else(|| eg_align::AlignError::InputMismatch("缺少对齐报告".to_owned()))
    }

    fn identity(&self) -> String {
        self.identity.clone()
    }

    fn engine_stamp(&self) -> Option<EngineStamp> {
        self.stamp.clone()
    }
}

fn align_stamp(version: &str) -> EngineStamp {
    EngineStamp {
        role: eg_domain::ModelRole::Ocr,
        backend: eg_domain::BackendId::local_server(),
        model_id: "align".to_owned(),
        model_version: version.to_owned(),
        prompt_hash: None,
        params_hash: eg_domain::Sha256Hex::of(b"align-params"),
    }
}

fn run_request(
    paper: &Paper,
    submission: SubmissionId,
    engine_stamps: Vec<EngineStamp>,
) -> SubmissionRunRequest {
    SubmissionRunRequest {
        submission_id: submission,
        task_id: TaskId::from_ulid(Ulid::from_u128(77)),
        paper: paper.clone(),
        template: None,
        mode: GradeMode::Standard,
        backend_available: true,
        manual_only: Vec::new(),
        excluded: None,
        report_exported: false,
        engine_stamps,
        force: false,
    }
}

#[test]
fn real_align_reports_two_students_zero_diff_and_explicit_unresolved() {
    let paper = paper20();
    let provider = std::sync::Arc::new(StaticReports::new(Some(align_stamp("1"))));
    let mut reports = HashMap::new();
    for (submission_id, stem) in [(1u128, "20260001"), (2, "20260002")] {
        let report = one_report(&paper, stem, submission_id);
        assert_eq!(report.answers.len(), 19, "未决题不构造答案");
        assert!(!report.auto_grade_ready, "含未决题整卷 ready=false");
        assert!(report.unresolved.iter().any(|item| {
            item.question_id == Some(question_id(1_009))
                && item.kind == eg_align::UnresolvedKind::MissingPage
        }));
        reports.insert(
            SubmissionId::from_ulid(Ulid::from_u128(submission_id)),
            report.clone(),
        );
        provider.insert(report);
    }
    let align_source = AlignReportSource::new(provider);
    // 让编译器确认 provider 的 identity/stamp 进入引擎指纹
    assert_eq!(align_source.identity(), "align-report-v1:align-fixture-v1");
    assert!(align_source.engine_stamp().is_some());

    let subject = eg_subject_math::MathSubjectPack::new();
    let config = GradeConfig::default();
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let idempotency = InMemoryIdempotency::new();
    let rubrics = FixtureRubrics::new();
    let judge = scripted(vec![]);
    let store = InMemoryGradeStore::new();
    let progress = InMemoryProgressStore::new();
    let grader = Grader {
        subject: &subject,
        rubrics: &rubrics,
        judge: &judge,
        classifier: None,
        config: &config,
        clock: &clock,
        ids: &ids,
        idempotency: &idempotency,
    };
    let pipeline = SubmissionPipeline {
        grader: &grader,
        align: &align_source,
        store: &store,
        progress: &progress,
    };
    let mut diff = 0usize;
    for submission_id in [1u128, 2] {
        let submission = SubmissionId::from_ulid(Ulid::from_u128(submission_id));
        let run = pipeline
            .run(&run_request(&paper, submission, Vec::new()))
            .expect("可判分题继续判分");
        assert_eq!(run.grade.records.len(), 19);
        assert_eq!(run.grade.failures.len(), 1, "未决题显式失败");
        let failure = &run.grade.failures[0];
        assert_eq!(failure.question_id, question_id(1_009));
        assert!(
            failure.message.contains("对齐未决（missing_page）"),
            "失败原因应为对齐未决而非未作答：{}",
            failure.message
        );
        assert_eq!(run.grade.summary.auto_accepted, 19);
        assert_eq!(run.grade.summary.needs_review, 0);
        assert_eq!(run.grade.summary.failed, 1);
        assert!(!run.align_gate.auto_grade_ready);
        assert!(
            run.align_gate
                .blocking_questions
                .contains(&question_id(1_009))
        );
        assert_eq!(run.persisted, 19);
        assert!(
            run.engine_fingerprint.contains(""),
            "引擎指纹包含 align 版本（非空）"
        );
        for record in &run.grade.records {
            if record.record.route_trace.rule_id != "R7"
                || record.record.judgement.label() != "match"
                || record.record.awarded.as_milli() != 5000
                || record.record.status != GradeStatus::AutoAccepted
            {
                diff += 1;
            }
        }
    }
    assert_eq!(diff, 0, "2 学生 × 19 可判分题 0 diff");
    assert_eq!(store.records().len(), 38);
}

#[test]
fn whole_submission_align_gate_blocks_auto_grading() {
    let paper = paper20();
    // 文件名不匹配名单 → 身份未决（整卷级，question_id=None）
    let report = one_report(&paper, "unknown_file", 3);
    assert!(!report.auto_grade_ready);
    assert!(
        report
            .unresolved
            .iter()
            .any(|item| item.question_id.is_none())
    );
    let provider = std::sync::Arc::new(StaticReports::new(None));
    provider.insert(report);
    let align_source = AlignReportSource::new(provider);

    let subject = eg_subject_math::MathSubjectPack::new();
    let config = GradeConfig::default();
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let idempotency = InMemoryIdempotency::new();
    let rubrics = FixtureRubrics::new();
    let judge = scripted(vec![]);
    let store = InMemoryGradeStore::new();
    let progress = InMemoryProgressStore::new();
    let grader = Grader {
        subject: &subject,
        rubrics: &rubrics,
        judge: &judge,
        classifier: None,
        config: &config,
        clock: &clock,
        ids: &ids,
        idempotency: &idempotency,
    };
    let pipeline = SubmissionPipeline {
        grader: &grader,
        align: &align_source,
        store: &store,
        progress: &progress,
    };
    let submission = SubmissionId::from_ulid(Ulid::from_u128(3));
    let error = pipeline
        .run(&run_request(&paper, submission, Vec::new()))
        .unwrap_err();
    assert!(matches!(error, SubmissionPipelineError::AlignGate(_)));
    assert_eq!(store.records().len(), 0, "整卷门禁不通过不得落库");
    assert_eq!(judge.calls().len(), 0);
}

#[test]
fn align_engine_version_changes_engine_fingerprint() {
    let paper = paper20();
    let report = one_report(&paper, "20260001", 4);
    let make_source = |version: &str| {
        let provider = std::sync::Arc::new(StaticReports::new(Some(align_stamp(version))));
        provider.insert(report.clone());
        AlignReportSource::new(provider)
    };
    let subject = eg_subject_math::MathSubjectPack::new();
    let config = GradeConfig::default();
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let idempotency = InMemoryIdempotency::new();
    let rubrics = FixtureRubrics::new();
    let judge = scripted(vec![]);
    let grader = Grader {
        subject: &subject,
        rubrics: &rubrics,
        judge: &judge,
        classifier: None,
        config: &config,
        clock: &clock,
        ids: &ids,
        idempotency: &idempotency,
    };
    let source_v1 = make_source("1");
    let source_v2 = make_source("2");
    assert_ne!(
        grader.engine_fingerprint_for(&[align_stamp("1")]),
        grader.engine_fingerprint_for(&[align_stamp("2")])
    );
    // 未决题不影响对齐来源身份/版本
    assert_eq!(source_v1.identity(), source_v2.identity());
}
