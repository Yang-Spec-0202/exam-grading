//! acceptance 的 20 题判分夹具（与 `crates/eg-grade/tests/fixtures_20.rs` 同口径）。
//!
//! 覆盖：单选/判断/多选（含 `partial_choice` 档位）/数值带容差/表达式/仅规则/低置信；
//! 期望表逐题给出（路由、判定标签、得分、状态）。夹具全部由代码构造，不依赖真实模型。

use eg_domain::{
    AnswerKey, AnswerValue, AssetId, BackendId, CalibrationId, ClassroomId, ConcreteKey,
    Confidence, Decimal, EngineStamp, GradeMode, GradeStatus, MilliScore, ModelRole, Paper,
    PaperId, PartialChoiceRule, PartialChoiceSource, PartialChoiceTier, Question, QuestionId,
    QuestionKind, RawAnswer, RecognitionId, Rubric, RubricId, RubricItem, RubricItemId,
    RubricOrigin, Sha256Hex, SubjectId, TaskId, Timestamp, Ulid,
};
use eg_grade::ports::{JudgeFailure, JudgeResponse, JudgeVerdict, ScriptedStep};
use eg_grade::{GradedQuestion, QuestionAnswer};

/// 固定时间（2026-11-02T00:00:00Z）。
#[must_use]
pub fn ts() -> Timestamp {
    chrono::DateTime::parse_from_rfc3339("2026-11-02T00:00:00Z").map_or_else(
        |_| chrono::Utc::now(),
        |value| value.with_timezone(&chrono::Utc),
    )
}

/// ULID。
#[must_use]
pub fn id(value: u128) -> Ulid {
    Ulid::from_u128(value)
}

/// 题目 id。
#[must_use]
pub fn question_id(value: u128) -> QuestionId {
    QuestionId::from_ulid(id(value))
}

/// 任务 id。
#[must_use]
pub fn task_id() -> TaskId {
    TaskId::from_ulid(id(9_000))
}

fn confidence(value: f64) -> Confidence {
    Confidence::new(value).unwrap_or(Confidence::ZERO)
}

fn raw(text: &str) -> RawAnswer {
    RawAnswer {
        text: text.to_owned(),
        latex: None,
        blocks: Vec::new(),
        source: eg_domain::AnswerSource::Recognition,
    }
}

fn concrete(value: AnswerValue) -> AnswerKey {
    AnswerKey::Concrete(ConcreteKey {
        primary: value,
        alternatives: Vec::new(),
        comparators: Vec::new(),
    })
}

fn options_key(options: &[&str]) -> AnswerKey {
    concrete(AnswerValue::Options(
        options.iter().map(|value| (*value).to_owned()).collect(),
    ))
}

fn text_key(text: &str) -> AnswerKey {
    concrete(AnswerValue::Text(text.to_owned()))
}

fn number_key(value: &str) -> AnswerKey {
    AnswerKey::Concrete(ConcreteKey {
        primary: AnswerValue::Number {
            value: match Decimal::parse(value) {
                Ok(decimal) => decimal,
                Err(_) => Decimal::ZERO,
            },
            unit: None,
            tolerance: None,
        },
        alternatives: Vec::new(),
        comparators: Vec::new(),
    })
}

fn number_key_with_tolerance(value: &str, absolute: &str) -> AnswerKey {
    let tolerance = Decimal::parse(absolute)
        .ok()
        .and_then(|decimal| eg_domain::Tolerance::new(Some(decimal), None).ok());
    AnswerKey::Concrete(ConcreteKey {
        primary: AnswerValue::Number {
            value: match Decimal::parse(value) {
                Ok(decimal) => decimal,
                Err(_) => Decimal::ZERO,
            },
            unit: None,
            tolerance,
        },
        alternatives: Vec::new(),
        comparators: Vec::new(),
    })
}

fn expression_key(latex: &str) -> AnswerKey {
    concrete(AnswerValue::Expression(latex.to_owned()))
}

fn rubric_item(value: u128, points: i64, depends_on: Vec<RubricItemId>) -> RubricItem {
    RubricItem {
        id: RubricItemId::from_ulid(id(value)),
        criterion: format!("评分项 {value}"),
        points: MilliScore::from_milli(points).unwrap_or(MilliScore::ZERO),
        required: true,
        depends_on,
    }
}

fn rubric(mode: eg_domain::ScoringMode, items: Vec<RubricItem>, total: i64) -> Rubric {
    Rubric {
        id: RubricId::from_ulid(id(7_000)),
        version: 1,
        items,
        mode,
        total: MilliScore::from_milli(total).unwrap_or(MilliScore::ZERO),
        origin: RubricOrigin::TeacherHandwritten {
            confirmed_by: eg_domain::LOCAL_TEACHER.to_owned(),
            confirmed_at: ts(),
        },
        partial_choice: None,
    }
}

/// 分步累加规则（Q17–Q19）。
#[must_use]
pub fn additive_rubric() -> Rubric {
    rubric(
        eg_domain::ScoringMode::Additive,
        vec![
            rubric_item(7_001, 2_000, Vec::new()),
            rubric_item(7_002, 3_000, Vec::new()),
        ],
        5_000,
    )
}

/// 多选部分给分规则（Q5/Q6）。
#[must_use]
pub fn partial_choice_rubric() -> Rubric {
    let mut rubric = rubric(
        eg_domain::ScoringMode::PartialChoice,
        vec![rubric_item(7_011, 5_000, Vec::new())],
        5_000,
    );
    rubric.partial_choice = Some(PartialChoiceRule {
        correct_option_count: 2,
        tiers: vec![PartialChoiceTier {
            selected_correct: 1,
            points: MilliScore::from_milli(2_000).unwrap_or(MilliScore::ZERO),
            label: None,
        }],
        wrong_selection_zero: true,
        source: PartialChoiceSource::TemplateData,
        source_note: None,
        total_milli: MilliScore::from_milli(5_000).unwrap_or(MilliScore::ZERO),
    });
    rubric
}

fn question(
    index: u128,
    label: &str,
    kind: QuestionKind,
    key: AnswerKey,
    rubric: Option<Rubric>,
) -> Question {
    Question {
        id: question_id(index),
        paper_id: PaperId::from_ulid(id(1)),
        ordinal: u32::try_from(index).unwrap_or(1),
        label: label.to_owned(),
        kind,
        max_score: MilliScore::from_milli(5_000).unwrap_or(MilliScore::ZERO),
        stem: None,
        key,
        rubric,
        subject_hint: Some(SubjectId::math()),
        knowledge_tags: vec![format!("知识点{}", index % 3)],
    }
}

/// 20 题试卷（20 × 5 分 = 100 分）。
#[must_use]
pub fn questions() -> Vec<Question> {
    let mut questions = Vec::with_capacity(20);
    // Q1–Q2 单选；Q3 判断；Q4–Q6 多选（Q5/Q6 带部分给分）
    questions.push(question(
        1,
        "1",
        QuestionKind::SingleChoice,
        options_key(&["A"]),
        None,
    ));
    questions.push(question(
        2,
        "2",
        QuestionKind::SingleChoice,
        options_key(&["A"]),
        None,
    ));
    questions.push(question(
        3,
        "3",
        QuestionKind::TrueFalse,
        text_key("正确"),
        None,
    ));
    questions.push(question(
        4,
        "4",
        QuestionKind::MultipleChoice,
        options_key(&["A", "B"]),
        None,
    ));
    questions.push(question(
        5,
        "5",
        QuestionKind::MultipleChoice,
        options_key(&["A", "B"]),
        Some(partial_choice_rubric()),
    ));
    questions.push(question(
        6,
        "6",
        QuestionKind::MultipleChoice,
        options_key(&["A", "B"]),
        Some(partial_choice_rubric()),
    ));
    // Q7–Q9 数值
    questions.push(question(
        7,
        "7",
        QuestionKind::NumericOnly,
        number_key("2.5"),
        None,
    ));
    questions.push(question(
        8,
        "8",
        QuestionKind::NumericOnly,
        number_key("2.5"),
        None,
    ));
    questions.push(question(
        9,
        "9",
        QuestionKind::NumericOnly,
        number_key_with_tolerance("2.5", "0.1"),
        None,
    ));
    // Q10/Q11 表达式
    questions.push(question(
        10,
        "10",
        QuestionKind::FillInBlank,
        expression_key("x^2-1"),
        None,
    ));
    questions.push(question(
        11,
        "11",
        QuestionKind::Calculation,
        expression_key("2*x+3*x"),
        None,
    ));
    // Q12/Q13 规则无法判定 → 升级模型
    questions.push(question(
        12,
        "12",
        QuestionKind::Calculation,
        expression_key("\\sqrt{x^2}"),
        None,
    ));
    questions.push(question(
        13,
        "13",
        QuestionKind::Calculation,
        expression_key("\\sqrt{x^2}"),
        None,
    ));
    // Q14–Q16 直接模型
    questions.push(question(
        14,
        "14",
        QuestionKind::ShortAnswer,
        text_key("abc"),
        None,
    ));
    questions.push(question(
        15,
        "15",
        QuestionKind::ShortAnswer,
        text_key("abc"),
        None,
    ));
    questions.push(question(
        16,
        "16",
        QuestionKind::ShortAnswer,
        text_key("abc"),
        None,
    ));
    // Q17–Q19 仅评分标准（分步）
    questions.push(question(
        17,
        "17",
        QuestionKind::Calculation,
        AnswerKey::RubricOnly,
        Some(additive_rubric()),
    ));
    questions.push(question(
        18,
        "18",
        QuestionKind::Calculation,
        AnswerKey::RubricOnly,
        Some(additive_rubric()),
    ));
    questions.push(question(
        19,
        "19",
        QuestionKind::Calculation,
        AnswerKey::RubricOnly,
        Some(additive_rubric()),
    ));
    // Q20 低置信（识别置信度 0.5 → R1 不可判定/待复核）
    questions.push(question(
        20,
        "20",
        QuestionKind::NumericOnly,
        number_key("2.5"),
        None,
    ));
    questions
}

/// 20 题试卷（总分 = 各题满分之和）。
#[must_use]
pub fn paper() -> Paper {
    let questions = questions();
    let total: i64 = questions
        .iter()
        .map(|question| question.max_score.as_milli())
        .sum();
    Paper {
        id: PaperId::from_ulid(id(1)),
        classroom_id: ClassroomId::from_ulid(id(2)),
        title: "验收夹具 20 题数学".to_owned(),
        subject: SubjectId::math(),
        total_score: MilliScore::from_milli(total).unwrap_or(MilliScore::ZERO),
        questions,
        key_version: 1,
        rubric_version: 1,
        created_at: ts(),
    }
}

fn answer(question: Question, text: &str) -> QuestionAnswer {
    let mut answer = QuestionAnswer::new(question, raw(text), confidence(0.95));
    answer.assets = vec![AssetId::of(b"acceptance-page")];
    answer.recognition = Some(RecognitionId::from_ulid(id(8_000)));
    answer
}

/// 张三的 20 题作答（与期望表对应）。
#[must_use]
pub fn answers() -> Vec<QuestionAnswer> {
    let questions = questions();
    let texts = [
        "A",
        "B",
        "对",
        "AB",
        "A",
        "AC",
        "2.5",
        "3",
        "2.6",
        "(x+1)*(x-1)",
        "5*x",
        "x",
        "x",
        "abc",
        "xyz",
        "xyz",
        "（1）2分；（2）3分",
        "（1）2分",
        "（1）2分；（2）3分",
        "2.5",
    ];
    let mut answers: Vec<QuestionAnswer> = questions
        .iter()
        .zip(texts)
        .map(|(question, text)| answer(question.clone(), text))
        .collect();
    // Q20 低置信
    if let Some(last) = answers.last_mut() {
        last.confidence = confidence(0.5);
    }
    answers
}

/// 逐题期望：（路由规则、判定标签、得分（千分）、状态）。
#[must_use]
pub fn expected() -> Vec<(&'static str, &'static str, i64, GradeStatus)> {
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

fn engine() -> EngineStamp {
    EngineStamp {
        role: ModelRole::Judge,
        backend: BackendId::local_server(),
        model_id: "fixture-judge".to_owned(),
        model_version: "1".to_owned(),
        prompt_hash: None,
        params_hash: Sha256Hex::of(b"acceptance-params"),
    }
}

fn judge_response(
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
        awarded: awarded.and_then(|milli| MilliScore::from_milli(milli).ok()),
        satisfied,
        missing,
        matched_alternative: None,
        reason: reason.to_owned(),
        confidence: confidence(0.95),
        calibration_id: if calibrated {
            CalibrationId::new("cal-v1").ok()
        } else {
            None
        },
        evidence_confidence: Some(confidence(0.95)),
        engine: engine(),
        latency_ms: 120,
        cost_micros: 0,
        output: serde_json::json!({"verdict": "fixture"}),
    }
}

fn match_response(max_milli: i64, calibrated: bool) -> JudgeResponse {
    judge_response(
        JudgeVerdict::Match,
        Some(max_milli),
        Vec::new(),
        Vec::new(),
        "与参考答案一致",
        calibrated,
    )
}

fn mismatch_response(calibrated: bool) -> JudgeResponse {
    judge_response(
        JudgeVerdict::Mismatch,
        Some(0),
        Vec::new(),
        Vec::new(),
        "与参考答案不一致",
        calibrated,
    )
}

/// 脚本化 judge 响应（顺序与 fixture 逐题期望对应）。
#[must_use]
pub fn judge_steps() -> Vec<ScriptedStep> {
    let rubric_ids: Vec<RubricItemId> =
        additive_rubric().items.iter().map(|item| item.id).collect();
    vec![
        // Q12 模型一致
        ScriptedStep::Respond(match_response(5000, true)),
        // Q13 模型部分一致
        ScriptedStep::Respond(judge_response(
            JudgeVerdict::Partial,
            Some(2000),
            Vec::new(),
            Vec::new(),
            "部分正确",
            true,
        )),
        // Q14 模型一致
        ScriptedStep::Respond(match_response(5000, true)),
        // Q15 模型不一致
        ScriptedStep::Respond(mismatch_response(true)),
        // Q16 模型一致（与规则冲突）
        ScriptedStep::Respond(match_response(5000, true)),
        // Q17 逐项全满足
        ScriptedStep::Respond(judge_response(
            JudgeVerdict::Match,
            Some(5000),
            rubric_ids.clone(),
            Vec::new(),
            "两项均满足",
            true,
        )),
        // Q18 逐项部分
        ScriptedStep::Respond(judge_response(
            JudgeVerdict::Partial,
            Some(2000),
            rubric_ids.first().copied().into_iter().collect(),
            rubric_ids.get(1).copied().into_iter().collect(),
            "只满足第一项",
            true,
        )),
        // Q19 声明 5000 但只满足一项（聚合=2000 → 冲突）
        ScriptedStep::Respond(judge_response(
            JudgeVerdict::Match,
            Some(5000),
            rubric_ids.first().copied().into_iter().collect(),
            rubric_ids.get(1).copied().into_iter().collect(),
            "声明满分",
            true,
        )),
    ]
}

/// 可用于负向断言的失败响应（模型后端不可用）。
#[must_use]
pub fn failure_step() -> ScriptedStep {
    ScriptedStep::Fail(JudgeFailure::BackendUnavailable {
        reason: "fixture: judge 后端不可用（负向用例）".to_owned(),
    })
}

/// 断言单题的（路由/判定/得分/状态）。
///
/// # Errors
///
/// 任一字段不一致时返回中文差异说明。
pub fn assert_graded(
    graded: &GradedQuestion,
    expected: (&str, &str, i64, GradeStatus),
    label: &str,
) -> Result<(), String> {
    let (rule, judgement, awarded, status) = expected;
    let actual = (
        graded.record.route_trace.rule_id.as_str(),
        graded.record.judgement.label(),
        graded.record.awarded.as_milli(),
        graded.record.status,
    );
    if actual == (rule, judgement, awarded, status) {
        return Ok(());
    }
    Err(format!(
        "{label} 期望（{rule}/{judgement}/{awarded}/{status:?}），实际（{}/{}/{}/{:?}）",
        actual.0, actual.1, actual.2, actual.3
    ))
}

/// 标准模式判分请求的公共字段。
#[must_use]
pub fn mode_standard() -> GradeMode {
    GradeMode::Standard
}
