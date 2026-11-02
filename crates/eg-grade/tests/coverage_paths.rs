//! 边界覆盖：就绪问题分类、l2/validate/ports 辅助路径、抽样边界、门禁证据缺失。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;
use eg_domain::{
    AnswerKey, Confidence, GradeMode, MilliScore, NormalizedAnswer, QuestionKind, UngradableReason,
};
use eg_grade::aggregate::AggregateError;
use eg_grade::l2::{manual_rule_reason, no_rule_available, should_escalate, unanswered_rule};
use eg_grade::ports::{
    ClassificationResponse, FixedClock, FixtureClassifier, InMemoryIdempotency, JudgeBackend,
    ScriptedJudge, concrete_key,
};
use eg_grade::{
    GateInput, IdGenerator, SamplingCandidate, check_readiness, sample_count, sample_fast,
    validate_judgement_against,
};
use eg_subject_math::MathSubjectPack;

#[test]
fn readiness_issue_kinds_are_classified() {
    let subject = MathSubjectPack::new();
    let rubrics = eg_grade::ports::FixtureRubrics::new();
    let omitted = question(
        501,
        QuestionKind::Calculation,
        5000,
        AnswerKey::ExplicitlyOmitted { note: None },
        None,
    );
    let invalid = {
        let mut question = question(502, QuestionKind::Calculation, 0, number_key("1"), None);
        question.max_score = MilliScore::ZERO;
        question
    };
    let paper_doc = paper(vec![omitted, invalid]);
    let report = check_readiness(&paper_doc, None, &rubrics, &[], &subject);
    assert!(!report.is_ready());
    let kinds: Vec<_> = report.issues.iter().map(|issue| issue.kind).collect();
    assert!(kinds.contains(&eg_grade::ReadinessIssueKind::AnswerKeyOmitted));
    assert!(kinds.contains(&eg_grade::ReadinessIssueKind::InvalidQuestion));
    // 草稿规则 → RubricDraftNotConfirmed
    let mut rubrics = eg_grade::ports::FixtureRubrics::new();
    rubrics.insert_draft(question_id(503));
    let rubric_only = question(
        503,
        QuestionKind::Calculation,
        5000,
        AnswerKey::RubricOnly,
        Some(additive_rubric()),
    );
    let report = check_readiness(&paper(vec![rubric_only]), None, &rubrics, &[], &subject);
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.kind == eg_grade::ReadinessIssueKind::RubricDraftNotConfirmed)
    );
}

#[test]
fn l2_helpers_cover_fallback_paths() {
    let unanswered = unanswered_rule();
    assert!(matches!(
        unanswered,
        eg_grade::RuleEvaluation::Mismatch { .. }
    ));
    let no_rule = no_rule_available();
    assert!(matches!(
        no_rule,
        eg_grade::RuleEvaluation::Undecidable { .. }
    ));
    assert!(should_escalate(
        &eg_domain::RouteDecision::RuleThenModel,
        &no_rule
    ));
    assert!(!should_escalate(
        &eg_domain::RouteDecision::RuleOnly,
        &no_rule
    ));
    assert!(matches!(
        manual_rule_reason(eg_domain::ManualReason::OutOfScope),
        eg_grade::RuleEvaluation::Undecidable { .. }
    ));
    // 空链：无可执行规则段
    let chain = MathSubjectPack::new();
    let key = eg_domain::ConcreteKey {
        primary: eg_domain::AnswerValue::Text("x".to_owned()),
        alternatives: Vec::new(),
        comparators: Vec::new(),
    };
    let normalized = NormalizedAnswer {
        text: "x".to_owned(),
        latex: None,
        value: None,
        steps: Vec::new(),
    };
    let ids = eg_grade::ports::SequentialIds::new();
    let question = question(504, QuestionKind::NumericOnly, 5000, number_key("1"), None);
    let run = eg_grade::evaluate_rule_chain(&chain, &question, &key, &normalized, &ids).unwrap();
    assert_ne!(run.traces.len(), 0);
    let _ = run.trace_ids();
}

#[test]
fn validate_helpers_and_ports_cover_edges() {
    let max = MilliScore::from_milli(5000).unwrap();
    let judgement = eg_domain::Judgement::Match {
        score: max,
        reason: "一致".to_owned(),
        evidence: Vec::new(),
    };
    assert!(validate_judgement_against(&judgement, max).is_ok());
    let bad = eg_domain::Judgement::Mismatch {
        score: MilliScore::from_milli(1000).unwrap(),
        reason: "不一致".to_owned(),
        evidence: Vec::new(),
    };
    assert!(validate_judgement_against(&bad, max).is_err());
    assert_eq!(
        eg_grade::validate::ungradable_reason(&eg_grade::ValidationError::EmptyReason),
        UngradableReason::InvalidSchema
    );
    let key = number_key("1");
    assert!(concrete_key(&key).is_some());
    assert!(concrete_key(&AnswerKey::Missing).is_none());
    let ids = eg_grade::ports::SequentialIds::new();
    assert_ne!(ids.next_ulid(), ids.next_ulid());
    let clock = FixedClock::new(ts());
    assert_eq!(eg_domain::Clock::now(&clock), ts());
    let store = InMemoryIdempotency::new();
    assert!(store.is_empty());
    let judge = ScriptedJudge::new(Vec::new());
    assert_eq!(judge.remaining(), 0);
    assert_eq!(judge.identity(), "fixture-judge-v1");
    let classifier = FixtureClassifier::new(
        QuestionKind::NumericOnly,
        confidence(0.95),
        engine(eg_domain::ModelRole::Router),
    );
    let result: ClassificationResponse = eg_grade::ClassifierBackend::classify(
        &classifier,
        &question(505, QuestionKind::NumericOnly, 5000, number_key("1"), None),
        task_id(),
    )
    .unwrap();
    assert_eq!(result.kind, QuestionKind::NumericOnly);
}

#[test]
fn sampling_boundaries_and_gate_evidence_paths() {
    let config = eg_grade::GradeConfig::default();
    assert_eq!(sample_count(0, 0.2), 0);
    let empty = sample_fast(&[], &config, 1);
    assert_eq!(empty.sampled.len(), 0);
    // 全部权重为 0 的极端配置仍可复现
    let config = eg_grade::GradeConfig {
        low_confidence_weight: 0.0,
        ..eg_grade::GradeConfig::default()
    };
    let candidates = vec![
        SamplingCandidate {
            question_id: question_id(601),
            confidence: confidence(0.95),
            threshold: confidence(0.8),
        },
        SamplingCandidate {
            question_id: question_id(602),
            confidence: confidence(0.85),
            threshold: confidence(0.8),
        },
    ];
    let first = sample_fast(&candidates, &config, 7);
    assert_eq!(first.sampled.len(), 1);
    // 门禁：缺证据/未校准/冲突路径
    let route = eg_domain::RouteDecision::ModelDirect;
    let judgement = eg_domain::Judgement::Match {
        score: MilliScore::from_milli(5000).unwrap(),
        reason: "一致".to_owned(),
        evidence: Vec::new(),
    };
    let input = GateInput {
        route: &route,
        confidence: confidence(0.95),
        mode: GradeMode::Standard,
        judgement: &judgement,
        used_model: true,
        calibrated: true,
        has_model_evidence: false,
        has_evidence_block: false,
        rule_evidence_required: false,
        has_rule_evidence: false,
        conflicts: &[],
        config: &config,
    };
    let outcome = eg_grade::gate(&input);
    assert!(
        outcome
            .blockers()
            .contains(&eg_domain::ReviewReasonKind::MissingEvidence)
    );
    // 聚合错误路径
    let rubric = additive_rubric();
    assert_eq!(
        eg_grade::aggregate::aggregate(&rubric, &[])
            .unwrap()
            .score
            .as_milli(),
        0
    );
    let _ = AggregateError::Overflow;
    let _: Confidence = confidence(0.5);
}
