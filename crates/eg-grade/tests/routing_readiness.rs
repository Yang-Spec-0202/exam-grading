//! L1 路由表 R0–R11 每行正例/边界 + 就绪检查 + R9 分类澄清。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod common;

use common::*;
use eg_domain::{AnswerKey, Confidence, GradeMode, ManualReason, QuestionKind, RouteDecision};
use eg_grade::ports::{
    FixedClock, FixtureClassifier, FixtureRubrics, InMemoryIdempotency, SequentialIds,
};
use eg_grade::{GradeConfig, GradeError, Grader, RouteInput, route};
use eg_subject_math::MathSubjectPack;

fn base_input(kind: QuestionKind, key: AnswerKey) -> RouteInput {
    RouteInput {
        kind,
        kind_known: true,
        key,
        rubric_confirmed: false,
        answer_empty: false,
        recognition_confidence: confidence(0.95),
        recognition_threshold: confidence(0.75),
        normalization_failed: false,
        backend_available: true,
        key_kind_conflict: false,
        manual_only: false,
    }
}

#[test]
fn route_table_each_rule_positive_and_boundary() {
    // R0 未作答且置信度 ≥ 下限
    let mut input = base_input(QuestionKind::NumericOnly, number_key("1"));
    input.answer_empty = true;
    assert_eq!(route(&input).rule_id, "R0");
    // R0 边界：未作答但低置信 → R1（不得判 0）
    input.recognition_confidence = confidence(0.70);
    assert_eq!(route(&input).rule_id, "R1");
    // R1 归一化失败
    let mut input = base_input(QuestionKind::NumericOnly, number_key("1"));
    input.normalization_failed = true;
    assert_eq!(route(&input).rule_id, "R1");
    assert_eq!(
        route(&input).decision,
        RouteDecision::Manual(ManualReason::RecognitionUnavailable)
    );
    // R1 边界：恰好等于阈值应继续
    let mut input = base_input(QuestionKind::NumericOnly, number_key("1"));
    input.recognition_confidence = confidence(0.75);
    assert_eq!(route(&input).rule_id, "R7");
    // R2 答案键缺失
    assert_eq!(
        route(&base_input(QuestionKind::Calculation, AnswerKey::Missing)).rule_id,
        "R2"
    );
    // R3 略且无规则
    let omitted = AnswerKey::ExplicitlyOmitted {
        note: Some("略".to_owned()),
    };
    assert_eq!(
        route(&base_input(QuestionKind::Calculation, omitted.clone())).rule_id,
        "R3"
    );
    // R3 边界：略但有已确认规则 → R4
    let mut input = base_input(QuestionKind::Calculation, omitted);
    input.rubric_confirmed = true;
    assert_eq!(route(&input).rule_id, "R4");
    // R4 仅评分标准
    assert_eq!(
        route(&base_input(
            QuestionKind::Calculation,
            AnswerKey::RubricOnly
        ))
        .rule_id,
        "R4"
    );
    // R5 客观题
    for kind in [QuestionKind::SingleChoice, QuestionKind::MultipleChoice] {
        assert_eq!(route(&base_input(kind, options_key(&["A"]))).rule_id, "R5");
    }
    assert_eq!(
        route(&base_input(QuestionKind::TrueFalse, text_key("正确"))).rule_id,
        "R5"
    );
    // R6 计算/证明 + 已确认规则
    let mut input = base_input(QuestionKind::Calculation, number_key("1"));
    input.rubric_confirmed = true;
    assert_eq!(route(&input).rule_id, "R6");
    // R7 填空/数值/计算
    for kind in [
        QuestionKind::FillInBlank,
        QuestionKind::NumericOnly,
        QuestionKind::Calculation,
    ] {
        assert_eq!(route(&base_input(kind, number_key("1"))).rule_id, "R7");
    }
    // R8 主观题
    for kind in [
        QuestionKind::Proof,
        QuestionKind::ShortAnswer,
        QuestionKind::Essay,
        QuestionKind::Other,
    ] {
        assert_eq!(route(&base_input(kind, text_key("答案"))).rule_id, "R8");
    }
    // R9 题型未知/答案键冲突 → 需分类
    let mut input = base_input(QuestionKind::SingleChoice, text_key("x"));
    input.kind_known = false;
    let outcome = route(&input);
    assert_eq!(outcome.rule_id, "R9");
    assert!(outcome.needs_classification);
    let mut input = base_input(QuestionKind::SingleChoice, text_key("x"));
    input.key_kind_conflict = true;
    assert_eq!(route(&input).rule_id, "R9");
    // R10 需要模型但后端不可用
    let mut input = base_input(QuestionKind::Calculation, number_key("1"));
    input.backend_available = false;
    assert_eq!(route(&input).rule_id, "R10");
    assert_eq!(
        route(&input).decision,
        RouteDecision::Manual(ManualReason::BackendUnavailable)
    );
    // R11 仅人工标记 / 兜底
    let mut input = base_input(QuestionKind::Calculation, number_key("1"));
    input.manual_only = true;
    assert_eq!(route(&input).rule_id, "R11");
    assert!(route(&input).decision.is_manual());
}

#[test]
fn readiness_conditions() {
    let subject = MathSubjectPack::new();
    let mut rubrics = FixtureRubrics::new();
    let key_question = question(1, QuestionKind::NumericOnly, 5000, number_key("2.5"), None);
    let rubric_question = question(
        2,
        QuestionKind::Calculation,
        5000,
        AnswerKey::RubricOnly,
        Some(additive_rubric()),
    );
    let missing_question = question(3, QuestionKind::Calculation, 5000, AnswerKey::Missing, None);
    let draft_question = question(
        4,
        QuestionKind::Calculation,
        5000,
        AnswerKey::RubricOnly,
        Some(additive_rubric()),
    );
    rubrics.insert_confirmed(question_id(2), additive_rubric());
    rubrics.insert_draft(question_id(4));
    let paper_doc = paper(vec![
        key_question.clone(),
        rubric_question,
        missing_question,
        draft_question,
    ]);
    let report = eg_grade::check_readiness(&paper_doc, None, &rubrics, &[], &subject);
    assert!(!report.is_ready());
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.question_id == Some(question_id(3)))
    );
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.question_id == Some(question_id(4)))
    );
    // 仅人工标记可放行缺答案键题
    let report = eg_grade::check_readiness(
        &paper_doc,
        None,
        &rubrics,
        &[question_id(3), question_id(4)],
        &subject,
    );
    assert!(report.is_ready(), "{report:?}");
    // 模板版本不一致 → 拒绝
    let template = eg_domain::PaperTemplate {
        paper_id: paper_doc.id,
        version: 2,
        page_count: 1,
        regions: Vec::new(),
    };
    let report = eg_grade::check_readiness(
        &paper_doc,
        Some(&template),
        &rubrics,
        &[question_id(3), question_id(4)],
        &subject,
    );
    assert!(!report.is_ready());
    let _ = key_question;
}

#[test]
fn r9_classification_fixture_and_manual_fallback() {
    let mut rubrics = FixtureRubrics::new();
    let _ = &mut rubrics;
    let question = question(
        31,
        QuestionKind::SingleChoice,
        5000,
        options_key(&["A"]),
        None,
    );
    let mut student_answer = answer(question.clone(), "A");
    student_answer.kind_known = false;
    let classifier = FixtureClassifier::new(
        QuestionKind::MultipleChoice,
        confidence(0.95),
        engine(eg_domain::ModelRole::Router),
    );
    let judge = scripted(vec![respond(match_response(5000, true))]);
    let config = GradeConfig::default();
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let idempotency = InMemoryIdempotency::new();
    let subject = MathSubjectPack::new();
    let grader = Grader {
        subject: &subject,
        rubrics: &rubrics,
        judge: &judge,
        classifier: Some(&classifier),
        config: &config,
        clock: &clock,
        ids: &ids,
        idempotency: &idempotency,
    };
    let paper = paper(vec![question.clone()]);
    let request = submission_request(paper.clone(), vec![student_answer], GradeMode::Standard);
    let grade = grader.grade_submission(&request).expect("应成功");
    assert_eq!(grade.records[0].record.route_trace.rule_id, "R5");
    assert!(
        grade.records[0]
            .record
            .route_trace
            .classifier_call
            .is_some()
    );
    // 分类置信度不足 → 人工
    let low = FixtureClassifier::new(
        QuestionKind::SingleChoice,
        confidence(0.5),
        engine(eg_domain::ModelRole::Router),
    );
    let mut student_answer = answer(question.clone(), "A");
    student_answer.kind_known = false;
    let judge = scripted(vec![]);
    let idempotency = InMemoryIdempotency::new();
    let grader = Grader {
        subject: &subject,
        rubrics: &rubrics,
        judge: &judge,
        classifier: Some(&low),
        config: &config,
        clock: &clock,
        ids: &ids,
        idempotency: &idempotency,
    };
    let grade = grader
        .grade_submission(&submission_request(
            paper,
            vec![student_answer],
            GradeMode::Standard,
        ))
        .expect("应成功");
    assert_eq!(
        grade.records[0].record.status,
        eg_domain::GradeStatus::NeedsReview
    );
    assert_eq!(judge.calls().len(), 0, "分类失败不得调用判定模型");
}

#[test]
fn readiness_blocks_whole_submission() {
    let rubrics = FixtureRubrics::new();
    let subject = MathSubjectPack::new();
    let paper_doc = paper(vec![question(
        41,
        QuestionKind::Calculation,
        5000,
        AnswerKey::Missing,
        None,
    )]);
    let judge = scripted(vec![]);
    let config = GradeConfig::default();
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let idempotency = InMemoryIdempotency::new();
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
    let request = submission_request(paper_doc, Vec::new(), GradeMode::Standard);
    let error = grader.grade_submission(&request).unwrap_err();
    assert!(matches!(error, GradeError::NotReady(_)));
    // 快速模式关闭
    let config = GradeConfig {
        fast_enabled: false,
        ..GradeConfig::default()
    };
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
    let error = grader
        .grade_submission(&submission_request(
            paper(vec![question(
                42,
                QuestionKind::NumericOnly,
                5000,
                number_key("1"),
                None,
            )]),
            Vec::new(),
            GradeMode::Fast,
        ))
        .unwrap_err();
    assert!(matches!(error, GradeError::FastModeDisabled));
    let _ = Confidence::new(0.5).unwrap();
}
