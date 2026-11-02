//! 判分测试公共夹具（只依赖 eg-domain + eg-subject-math）。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(dead_code, unreachable_pub, unused_imports)]

use eg_domain::{
    AnswerKey, AnswerValue, AssetId, BackendId, CalibrationId, ClassroomId, ConcreteKey,
    Confidence, Decimal, EngineStamp, GradeMode, MilliScore, ModelRole, Paper, PaperId,
    PartialChoiceRule, PartialChoiceSource, PartialChoiceTier, Question, QuestionId, QuestionKind,
    RawAnswer, RecognitionId, Rubric, RubricId, RubricItem, RubricItemId, RubricOrigin, Sha256Hex,
    SubjectId, TaskId, Timestamp, Ulid,
};
use eg_grade::ports::{JudgeFailure, JudgeResponse, JudgeVerdict, ScriptedJudge, ScriptedStep};

pub fn ts() -> Timestamp {
    eg_domain::time::parse_rfc3339("2026-11-02T00:00:00Z").unwrap()
}

pub fn id(value: u128) -> Ulid {
    Ulid::from_u128(value)
}

pub fn question_id(value: u128) -> QuestionId {
    QuestionId::from_ulid(id(value))
}

pub fn submission_id(value: u128) -> eg_domain::SubmissionId {
    eg_domain::SubmissionId::from_ulid(id(value))
}

pub fn task_id() -> TaskId {
    TaskId::from_ulid(id(9_000))
}

pub fn engine(role: ModelRole) -> EngineStamp {
    EngineStamp {
        role,
        backend: BackendId::local_server(),
        model_id: "judge-fixture".to_owned(),
        model_version: "1".to_owned(),
        prompt_hash: None,
        params_hash: Sha256Hex::of(b"params"),
    }
}

pub fn judge_engine() -> EngineStamp {
    engine(ModelRole::Judge)
}

pub fn confidence(value: f64) -> Confidence {
    Confidence::new(value).unwrap()
}

pub fn calibration() -> CalibrationId {
    CalibrationId::new("cal-v1").unwrap()
}

pub fn raw(text: &str) -> RawAnswer {
    RawAnswer {
        text: text.to_owned(),
        latex: None,
        blocks: Vec::new(),
        source: eg_domain::AnswerSource::Recognition,
    }
}

pub fn raw_latex(latex: &str) -> RawAnswer {
    RawAnswer {
        text: String::new(),
        latex: Some(latex.to_owned()),
        blocks: Vec::new(),
        source: eg_domain::AnswerSource::Recognition,
    }
}

pub fn concrete(value: AnswerValue) -> AnswerKey {
    AnswerKey::Concrete(ConcreteKey {
        primary: value,
        alternatives: Vec::new(),
        comparators: Vec::new(),
    })
}

pub fn number_key(value: &str) -> AnswerKey {
    concrete(AnswerValue::Number {
        value: Decimal::parse(value).unwrap(),
        unit: None,
        tolerance: None,
    })
}

pub fn number_key_with_tolerance(value: &str, absolute: &str) -> AnswerKey {
    concrete(AnswerValue::Number {
        value: Decimal::parse(value).unwrap(),
        unit: None,
        tolerance: Some(
            eg_domain::Tolerance::new(Some(Decimal::parse(absolute).unwrap()), None).unwrap(),
        ),
    })
}

pub fn expression_key(latex: &str) -> AnswerKey {
    concrete(AnswerValue::Expression(latex.to_owned()))
}

pub fn options_key(options: &[&str]) -> AnswerKey {
    concrete(AnswerValue::Options(
        options.iter().map(|value| (*value).to_owned()).collect(),
    ))
}

pub fn text_key(text: &str) -> AnswerKey {
    concrete(AnswerValue::Text(text.to_owned()))
}

pub fn rubric_only_key() -> AnswerKey {
    AnswerKey::RubricOnly
}

pub fn question(
    value: u128,
    kind: QuestionKind,
    max_milli: i64,
    key: AnswerKey,
    rubric: Option<Rubric>,
) -> Question {
    Question {
        id: question_id(value),
        paper_id: PaperId::from_ulid(id(1)),
        ordinal: u32::try_from(value % 100).unwrap().max(1),
        label: format!("{value}"),
        kind,
        max_score: MilliScore::from_milli(max_milli).unwrap(),
        stem: None,
        key,
        rubric,
        subject_hint: Some(SubjectId::math()),
        knowledge_tags: Vec::new(),
    }
}

pub fn paper(mut questions: Vec<Question>) -> Paper {
    // 夹具用：按卷面顺序重编题号（1 起连续），避免测试里手工维护 ordinal。
    for (index, question) in questions.iter_mut().enumerate() {
        question.ordinal = u32::try_from(index + 1).unwrap();
    }
    let total: i64 = questions
        .iter()
        .map(|question| question.max_score.as_milli())
        .sum();
    Paper {
        id: PaperId::from_ulid(id(1)),
        classroom_id: ClassroomId::from_ulid(id(2)),
        title: "判分夹具".to_owned(),
        subject: SubjectId::math(),
        total_score: MilliScore::from_milli(total).unwrap(),
        questions,
        key_version: 1,
        rubric_version: 1,
        created_at: ts(),
    }
}

pub fn rubric_item(value: u128, points: i64, depends_on: Vec<RubricItemId>) -> RubricItem {
    RubricItem {
        id: RubricItemId::from_ulid(id(value)),
        criterion: format!("评分项 {value}"),
        points: MilliScore::from_milli(points).unwrap(),
        required: true,
        depends_on,
    }
}

pub fn rubric(
    value: u128,
    mode: eg_domain::ScoringMode,
    items: Vec<RubricItem>,
    total: i64,
) -> Rubric {
    Rubric {
        id: RubricId::from_ulid(id(value)),
        version: 1,
        items,
        mode,
        total: MilliScore::from_milli(total).unwrap(),
        origin: RubricOrigin::TeacherHandwritten {
            confirmed_by: eg_domain::LOCAL_TEACHER.to_owned(),
            confirmed_at: ts(),
        },
        partial_choice: None,
    }
}

pub fn additive_rubric() -> Rubric {
    rubric(
        7_000,
        eg_domain::ScoringMode::Additive,
        vec![
            rubric_item(7_001, 2_000, Vec::new()),
            rubric_item(7_002, 3_000, Vec::new()),
        ],
        5_000,
    )
}

pub fn partial_choice_rubric() -> Rubric {
    let mut rubric = rubric(
        7_010,
        eg_domain::ScoringMode::PartialChoice,
        vec![rubric_item(7_011, 5_000, Vec::new())],
        5_000,
    );
    rubric.partial_choice = Some(PartialChoiceRule {
        correct_option_count: 2,
        tiers: vec![PartialChoiceTier {
            selected_correct: 1,
            points: MilliScore::from_milli(2_000).unwrap(),
            label: None,
        }],
        wrong_selection_zero: true,
        source: PartialChoiceSource::TemplateData,
        source_note: None,
        total_milli: MilliScore::from_milli(5_000).unwrap(),
    });
    rubric
}

pub fn judge_response(
    verdict: JudgeVerdict,
    awarded: Option<i64>,
    satisfied: Vec<RubricItemId>,
    missing: Vec<RubricItemId>,
    reason: &str,
    calibrated: bool,
) -> JudgeResponse {
    JudgeResponse {
        model_call: None,
        verdict,
        awarded: awarded.map(|milli| MilliScore::from_milli(milli).unwrap()),
        satisfied,
        missing,
        matched_alternative: None,
        reason: reason.to_owned(),
        confidence: confidence(0.95),
        calibration_id: calibrated.then(calibration),
        evidence_confidence: Some(confidence(0.95)),
        engine: judge_engine(),
        latency_ms: 120,
        cost_micros: 0,
        output: serde_json::json!({"verdict": "fixture"}),
    }
}

pub fn match_response(max_milli: i64, calibrated: bool) -> JudgeResponse {
    judge_response(
        JudgeVerdict::Match,
        Some(max_milli),
        Vec::new(),
        Vec::new(),
        "与参考答案一致",
        calibrated,
    )
}

pub fn mismatch_response(calibrated: bool) -> JudgeResponse {
    judge_response(
        JudgeVerdict::Mismatch,
        Some(0),
        Vec::new(),
        Vec::new(),
        "与参考答案不一致",
        calibrated,
    )
}

pub fn ungradable_response() -> JudgeResponse {
    judge_response(
        JudgeVerdict::Ungradable(eg_domain::UngradableReason::OutOfScope),
        None,
        Vec::new(),
        Vec::new(),
        "超出判定范围",
        true,
    )
}

pub fn scripted(steps: Vec<ScriptedStep>) -> ScriptedJudge {
    ScriptedJudge::new(steps)
}

pub fn respond(response: JudgeResponse) -> ScriptedStep {
    ScriptedStep::Respond(response)
}

pub fn fail(failure: JudgeFailure) -> ScriptedStep {
    ScriptedStep::Fail(failure)
}

pub fn submission_request(
    paper: Paper,
    answers: Vec<eg_grade::QuestionAnswer>,
    mode: GradeMode,
) -> eg_grade::SubmissionRequest {
    eg_grade::SubmissionRequest {
        submission_id: submission_id(5_000),
        task_id: task_id(),
        paper,
        template: None,
        answers,
        mode,
        backend_available: true,
        manual_only: Vec::new(),
        excluded: None,
        engine_stamps: Vec::new(),
    }
}

pub fn answer_with_confidence(
    question: Question,
    text: &str,
    value: f64,
) -> eg_grade::QuestionAnswer {
    let mut answer = eg_grade::QuestionAnswer::new(question, raw(text), confidence(value));
    answer.assets = vec![AssetId::of(b"page-1")];
    answer.recognition = Some(RecognitionId::from_ulid(id(8_000)));
    answer
}

pub fn answer(question: Question, text: &str) -> eg_grade::QuestionAnswer {
    answer_with_confidence(question, text, 0.95)
}

pub fn answer_latex(question: Question, latex: &str) -> eg_grade::QuestionAnswer {
    let mut answer = eg_grade::QuestionAnswer::new(question, raw_latex(latex), confidence(0.95));
    answer.assets = vec![AssetId::of(b"page-1")];
    answer.recognition = Some(RecognitionId::from_ulid(id(8_000)));
    answer
}
