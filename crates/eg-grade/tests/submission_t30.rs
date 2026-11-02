//! T30 提交级流水线：20 题 × 3 学生端到端 0 diff、断点/幂等、失败隔离、抽检标记、
//! 引擎指纹变化使缓存失效、已导出报表不自动刷新。

#![allow(
    unused_imports,
    clippy::vec_init_then_push,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines
)]

mod common;

use common::*;
use std::sync::Arc;

use eg_domain::{
    AnswerPart, BBox, GradeMode, GradeStatus, PageId, QuestionKind, RecognitionId, SegmentId,
    StudentAnswer, SubmissionId, TaskId, Ulid,
};
use eg_grade::align::FixtureAlignSource;
use eg_grade::ports::{FixedClock, InMemoryIdempotency, RubricSource, SequentialIds};
use eg_grade::submission::{
    GradeStore, InMemoryGradeStore, InMemoryProgressStore, ProgressStore, SubmissionPipeline,
    SubmissionRunRequest,
};
use eg_grade::{GradeConfig, Grader, SamplingResult};
use eg_rubric::memory::InMemoryRubricStore;
use eg_rubric::ports::RubricStore;
use eg_subject_math::MathSubjectPack;

fn paper20() -> eg_domain::Paper {
    let mut questions = Vec::new();
    questions.push(question(
        1,
        QuestionKind::SingleChoice,
        5000,
        options_key(&["A"]),
        None,
    ));
    questions.push(question(
        2,
        QuestionKind::SingleChoice,
        5000,
        options_key(&["A"]),
        None,
    ));
    questions.push(question(
        3,
        QuestionKind::TrueFalse,
        5000,
        text_key("正确"),
        None,
    ));
    questions.push(question(
        4,
        QuestionKind::MultipleChoice,
        5000,
        options_key(&["A", "B"]),
        None,
    ));
    questions.push(question(
        5,
        QuestionKind::MultipleChoice,
        5000,
        options_key(&["A", "B"]),
        Some(partial_choice_rubric()),
    ));
    questions.push(question(
        6,
        QuestionKind::MultipleChoice,
        5000,
        options_key(&["A", "B"]),
        Some(partial_choice_rubric()),
    ));
    questions.push(question(
        7,
        QuestionKind::NumericOnly,
        5000,
        number_key("2.5"),
        None,
    ));
    questions.push(question(
        8,
        QuestionKind::NumericOnly,
        5000,
        number_key("2.5"),
        None,
    ));
    questions.push(question(
        9,
        QuestionKind::NumericOnly,
        5000,
        number_key_with_tolerance("2.5", "0.1"),
        None,
    ));
    questions.push(question(
        10,
        QuestionKind::FillInBlank,
        5000,
        expression_key("x^2-1"),
        None,
    ));
    questions.push(question(
        11,
        QuestionKind::Calculation,
        5000,
        expression_key("2*x+3*x"),
        None,
    ));
    questions.push(question(
        12,
        QuestionKind::Calculation,
        5000,
        expression_key("\\sqrt{x^2}"),
        None,
    ));
    questions.push(question(
        13,
        QuestionKind::Calculation,
        5000,
        expression_key("\\sqrt{x^2}"),
        None,
    ));
    questions.push(question(
        14,
        QuestionKind::ShortAnswer,
        5000,
        text_key("abc"),
        None,
    ));
    questions.push(question(
        15,
        QuestionKind::ShortAnswer,
        5000,
        text_key("abc"),
        None,
    ));
    questions.push(question(
        16,
        QuestionKind::ShortAnswer,
        5000,
        text_key("abc"),
        None,
    ));
    questions.push(question(
        17,
        QuestionKind::Calculation,
        5000,
        rubric_only_key(),
        Some(additive_rubric()),
    ));
    questions.push(question(
        18,
        QuestionKind::Calculation,
        5000,
        rubric_only_key(),
        Some(additive_rubric()),
    ));
    questions.push(question(
        19,
        QuestionKind::Calculation,
        5000,
        rubric_only_key(),
        Some(additive_rubric()),
    ));
    questions.push(question(
        20,
        QuestionKind::NumericOnly,
        5000,
        number_key("2.5"),
        None,
    ));
    paper(questions)
}

fn answers20() -> Vec<(&'static str, f64)> {
    vec![
        ("A", 0.95),
        ("B", 0.95),
        ("对", 0.95),
        ("AB", 0.95),
        ("A", 0.95),
        ("AC", 0.95),
        ("2.5", 0.95),
        ("3", 0.95),
        ("2.6", 0.95),
        ("(x+1)*(x-1)", 0.95),
        ("5*x", 0.95),
        ("x", 0.95),
        ("x", 0.95),
        ("abc", 0.95),
        ("xyz", 0.95),
        ("xyz", 0.95),
        ("（1）2分；（2）3分", 0.95),
        ("（1）2分", 0.95),
        ("（1）2分；（2）3分", 0.95),
        ("2.5", 0.50),
    ]
}

fn student_answer(
    paper: &eg_domain::Paper,
    index: usize,
    submission: SubmissionId,
    text: &str,
    confidence_value: f64,
) -> StudentAnswer {
    let question = &paper.questions[index];
    let recognition = RecognitionId::from_ulid(Ulid::from_u128(
        10_000 + submission.as_ulid().as_u128() * 100 + u128::try_from(index).unwrap(),
    ));
    StudentAnswer {
        submission_id: submission,
        question_id: question.id,
        recognition_id: Some(recognition),
        segment_id: Some(SegmentId::from_ulid(Ulid::from_u128(1))),
        raw: raw(text),
        parts: vec![AnswerPart {
            page_id: PageId::from_ulid(Ulid::from_u128(1)),
            segment_id: SegmentId::from_ulid(Ulid::from_u128(1)),
            recognition_id: recognition,
            bbox: BBox::full_page(),
            text: text.to_owned(),
        }],
        recognition_confidence: confidence(confidence_value),
    }
}

fn rubric_store() -> Arc<InMemoryRubricStore> {
    let store = Arc::new(InMemoryRubricStore::default());
    for value in [5u128, 6] {
        store
            .save_active_rubric(question_id(value), &partial_choice_rubric(), 0)
            .unwrap();
    }
    for value in [17u128, 18, 19] {
        store
            .save_active_rubric(question_id(value), &additive_rubric(), 0)
            .unwrap();
    }
    store
}

fn judge_steps() -> Vec<eg_grade::ports::ScriptedStep> {
    let rubric_ids: Vec<eg_domain::RubricItemId> =
        additive_rubric().items.iter().map(|item| item.id).collect();
    vec![
        respond(match_response(5000, true)),
        respond(judge_response(
            eg_grade::JudgeVerdict::Partial,
            Some(2000),
            Vec::new(),
            Vec::new(),
            "部分正确",
            true,
        )),
        respond(match_response(5000, true)),
        respond(mismatch_response(true)),
        respond(match_response(5000, true)),
        respond(judge_response(
            eg_grade::JudgeVerdict::Match,
            Some(5000),
            rubric_ids.clone(),
            Vec::new(),
            "两项均满足",
            true,
        )),
        respond(judge_response(
            eg_grade::JudgeVerdict::Partial,
            Some(2000),
            vec![rubric_ids[0]],
            vec![rubric_ids[1]],
            "只满足第一项",
            true,
        )),
        respond(judge_response(
            eg_grade::JudgeVerdict::Match,
            Some(5000),
            vec![rubric_ids[0]],
            vec![rubric_ids[1]],
            "声明满分",
            true,
        )),
    ]
}

fn align_source(
    paper: &eg_domain::Paper,
    submission: SubmissionId,
    skip: Option<usize>,
) -> FixtureAlignSource {
    let align = FixtureAlignSource::new();
    let answers = answers20();
    let students: Vec<StudentAnswer> = paper
        .questions
        .iter()
        .enumerate()
        .filter(|(index, _)| Some(*index) != skip)
        .map(|(index, _)| {
            let (text, confidence_value) = answers[index];
            student_answer(paper, index, submission, text, confidence_value)
        })
        .collect();
    align.insert(submission, students);
    align
}

struct Harness {
    subject: MathSubjectPack,
    config: GradeConfig,
    clock: FixedClock,
    ids: SequentialIds,
    idempotency: InMemoryIdempotency,
    store: InMemoryGradeStore,
    progress: InMemoryProgressStore,
    rubrics: Arc<InMemoryRubricStore>,
}

impl Harness {
    fn new() -> Self {
        Self {
            subject: MathSubjectPack::new(),
            config: GradeConfig::default(),
            clock: FixedClock::new(ts()),
            ids: SequentialIds::new(),
            idempotency: InMemoryIdempotency::new(),
            store: InMemoryGradeStore::new(),
            progress: InMemoryProgressStore::new(),
            rubrics: rubric_store(),
        }
    }
}

fn run_request(
    paper: &eg_domain::Paper,
    submission: SubmissionId,
    report_exported: bool,
    engine_stamps: Vec<eg_domain::EngineStamp>,
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
        report_exported,
        engine_stamps,
        force: false,
    }
}

fn expected() -> Vec<(&'static str, &'static str, i64, GradeStatus)> {
    vec![
        ("R5", "match", 5000, GradeStatus::AutoAccepted),
        ("R5", "mismatch", 0, GradeStatus::AutoAccepted),
        ("R5", "match", 5000, GradeStatus::AutoAccepted),
        ("R5", "match", 5000, GradeStatus::AutoAccepted),
        ("R5", "partial", 2000, GradeStatus::AutoAccepted),
        ("R5", "mismatch", 0, GradeStatus::AutoAccepted),
        ("R7", "match", 5000, GradeStatus::AutoAccepted),
        ("R7", "mismatch", 0, GradeStatus::AutoAccepted),
        ("R7", "match", 5000, GradeStatus::AutoAccepted),
        ("R7", "match", 5000, GradeStatus::AutoAccepted),
        ("R7", "match", 5000, GradeStatus::AutoAccepted),
        ("R7", "match", 5000, GradeStatus::AutoAccepted),
        ("R7", "partial", 2000, GradeStatus::AutoAccepted),
        ("R8", "match", 5000, GradeStatus::AutoAccepted),
        ("R8", "mismatch", 0, GradeStatus::AutoAccepted),
        ("R8", "ungradable", 0, GradeStatus::NeedsReview),
        ("R4", "match", 5000, GradeStatus::AutoAccepted),
        ("R4", "partial", 2000, GradeStatus::AutoAccepted),
        ("R4", "partial", 2000, GradeStatus::NeedsReview),
        ("R1", "ungradable", 0, GradeStatus::NeedsReview),
    ]
}

#[test]
fn twenty_questions_three_students_end_to_end_zero_diff() {
    let harness = Harness::new();
    let paper = paper20();
    let mut steps = judge_steps();
    steps.extend(judge_steps());
    steps.extend(judge_steps());
    let judge = scripted(steps);
    let rubric_source = eg_grade::RubricStoreSource::new(harness.rubrics.clone());
    let grader = Grader {
        subject: &harness.subject,
        rubrics: &rubric_source,
        judge: &judge,
        classifier: None,
        config: &harness.config,
        clock: &harness.clock,
        ids: &harness.ids,
        idempotency: &harness.idempotency,
    };
    let expected_rows = expected();
    let mut total_diff = 0usize;
    let mut first_run: Option<(SubmissionId, SubmissionRunRequest)> = None;
    for student in 1..=3u128 {
        let submission = SubmissionId::from_ulid(Ulid::from_u128(20_000 + student));
        let align = align_source(&paper, submission, None);
        let pipeline = SubmissionPipeline {
            grader: &grader,
            align: &align,
            store: &harness.store,
            progress: &harness.progress,
        };
        let mut request = run_request(&paper, submission, true, Vec::new());
        request.force = false;
        let run = pipeline.run(&request).expect("提交判分成功");
        assert_eq!(run.grade.records.len(), 20, "学生 {student}");
        assert_eq!(run.grade.failures.len(), 0);
        assert_eq!(run.grade.summary.auto_accepted, 17);
        assert_eq!(run.grade.summary.needs_review, 3);
        assert_eq!(run.persisted, 20);
        assert!(run.report_refresh_required);
        for (index, record) in run.grade.records.iter().enumerate() {
            let (rule, judgement, awarded, status) = expected_rows[index];
            if record.record.route_trace.rule_id != rule
                || record.record.judgement.label() != judgement
                || record.record.awarded.as_milli() != awarded
                || record.record.status != status
            {
                total_diff += 1;
            }
        }
        if student == 1 {
            first_run = Some((submission, request));
        }
    }
    assert_eq!(total_diff, 0, "20×3 端到端 diff 必须为 0");
    assert_eq!(harness.store.records().len(), 60);
    // 已完成提交：不再重判、不重新计费
    let (submission, mut request) = first_run.expect("第一份提交");
    let align = align_source(&paper, submission, None);
    let pipeline = SubmissionPipeline {
        grader: &grader,
        align: &align,
        store: &harness.store,
        progress: &harness.progress,
    };
    request.force = false;
    let cached = pipeline.run(&request).unwrap();
    assert_eq!(cached.persisted, 0);
    assert_eq!(cached.resumed, 20);
    assert!(!cached.report_refresh_required);
    assert_eq!(judge.calls().len(), 24, "断点命中不重复调用模型");
    // 强制重跑：幂等键命中，仍不重复判分/计费
    request.force = true;
    let repeated = pipeline.run(&request).unwrap();
    assert_eq!(repeated.persisted, 0, "幂等键命中不重复落库");
    assert_eq!(repeated.resumed, 20);
    assert_eq!(judge.calls().len(), 24);
}

#[test]
fn engine_stamp_change_invalidates_cache_and_regrades() {
    let harness = Harness::new();
    let paper = paper20();
    let judge = scripted({
        let mut steps = judge_steps();
        steps.extend(judge_steps()); // 第二次判定（引擎版本变化后重判 8 题）
        steps
    });
    let rubric_source = eg_grade::RubricStoreSource::new(harness.rubrics.clone());
    let grader = Grader {
        subject: &harness.subject,
        rubrics: &rubric_source,
        judge: &judge,
        classifier: None,
        config: &harness.config,
        clock: &harness.clock,
        ids: &harness.ids,
        idempotency: &harness.idempotency,
    };
    let submission = SubmissionId::from_ulid(Ulid::from_u128(30_001));
    let align = align_source(&paper, submission, None);
    let pipeline = SubmissionPipeline {
        grader: &grader,
        align: &align,
        store: &harness.store,
        progress: &harness.progress,
    };
    let request = run_request(&paper, submission, false, Vec::new());
    let first = pipeline.run(&request).unwrap();
    let calls_after_first = judge.calls().len();
    // 外部引擎版本变化（OCR/CAS 等）→ 引擎指纹变化 → 幂等键失效 → 重判
    let changed_stamp = eg_domain::EngineStamp {
        role: eg_domain::ModelRole::Ocr,
        backend: eg_domain::BackendId::local_server(),
        model_id: "ocr".to_owned(),
        model_version: "2".to_owned(),
        prompt_hash: None,
        params_hash: eg_domain::Sha256Hex::of(b"ocr-params-v2"),
    };
    let mut forced = run_request(&paper, submission, false, vec![changed_stamp]);
    forced.force = true;
    let second = pipeline.run(&forced).unwrap();
    assert_ne!(first.engine_fingerprint, second.engine_fingerprint);
    assert_eq!(
        judge.calls().len(),
        calls_after_first + 8,
        "引擎指纹变化使缓存失效"
    );
    assert_eq!(second.persisted, 20);
}

#[test]
fn single_question_failure_does_not_block_other_questions() {
    let harness = Harness::new();
    let paper = paper20();
    let judge = scripted(judge_steps());
    let rubric_source = eg_grade::RubricStoreSource::new(harness.rubrics.clone());
    let grader = Grader {
        subject: &harness.subject,
        rubrics: &rubric_source,
        judge: &judge,
        classifier: None,
        config: &harness.config,
        clock: &harness.clock,
        ids: &harness.ids,
        idempotency: &harness.idempotency,
    };
    let submission = SubmissionId::from_ulid(Ulid::from_u128(40_001));
    // 缺少第 10 题（索引 9）的学生答案
    let align = align_source(&paper, submission, Some(9));
    let pipeline = SubmissionPipeline {
        grader: &grader,
        align: &align,
        store: &harness.store,
        progress: &harness.progress,
    };
    let run = pipeline
        .run(&run_request(&paper, submission, false, Vec::new()))
        .expect("整卷不失败");
    assert_eq!(run.grade.failures.len(), 1);
    assert_eq!(run.grade.failures[0].question_id, question_id(10));
    assert_eq!(run.grade.records.len(), 19);
    assert_eq!(run.persisted, 19);
    assert_eq!(run.grade.summary.failed, 1);
    assert_eq!(run.grade.summary.total_questions, 20);
    let snapshot = harness.progress.load(submission).unwrap();
    assert_eq!(snapshot.processed.len(), 19);
    assert_eq!(snapshot.failures.len(), 1);
}

#[test]
fn fast_mode_sampling_flags_are_persisted() {
    let harness = Harness::new();
    let paper = paper20();
    let judge = scripted(judge_steps());
    let rubric_source = eg_grade::RubricStoreSource::new(harness.rubrics.clone());
    let grader = Grader {
        subject: &harness.subject,
        rubrics: &rubric_source,
        judge: &judge,
        classifier: None,
        config: &harness.config,
        clock: &harness.clock,
        ids: &harness.ids,
        idempotency: &harness.idempotency,
    };
    let submission = SubmissionId::from_ulid(Ulid::from_u128(50_001));
    let align = align_source(&paper, submission, None);
    let pipeline = SubmissionPipeline {
        grader: &grader,
        align: &align,
        store: &harness.store,
        progress: &harness.progress,
    };
    let mut request = run_request(&paper, submission, false, Vec::new());
    request.mode = GradeMode::Fast;
    let run = pipeline.run(&request).expect("快速模式判分");
    let sampling: &SamplingResult = run.grade.sampling.as_ref().expect("应有抽检");
    assert_eq!(sampling.seed, sampling.seed);
    let sampled_ids: Vec<_> = sampling.sampled.clone();
    assert_ne!(sampled_ids.len(), 0);
    let stored = harness.store.latest_records(submission).unwrap();
    for record in &stored {
        if sampled_ids.contains(&record.question_id) {
            assert!(record.sampled, "抽检标记必须写入记录");
        }
    }
}

#[test]
fn align_failure_is_whole_submission_error() {
    let harness = Harness::new();
    let paper = paper20();
    let judge = scripted(vec![]);
    let rubric_source = eg_grade::RubricStoreSource::new(harness.rubrics.clone());
    let grader = Grader {
        subject: &harness.subject,
        rubrics: &rubric_source,
        judge: &judge,
        classifier: None,
        config: &harness.config,
        clock: &harness.clock,
        ids: &harness.ids,
        idempotency: &harness.idempotency,
    };
    let submission = SubmissionId::from_ulid(Ulid::from_u128(60_001));
    let align = FixtureAlignSource::new();
    align.fail(eg_grade::AlignError::Read("对齐服务不可用".to_owned()));
    let pipeline = SubmissionPipeline {
        grader: &grader,
        align: &align,
        store: &harness.store,
        progress: &harness.progress,
    };
    let error = pipeline
        .run(&run_request(&paper, submission, false, Vec::new()))
        .unwrap_err();
    assert!(matches!(error, eg_grade::SubmissionPipelineError::Align(_)));
    assert_eq!(harness.store.records().len(), 0);
}
