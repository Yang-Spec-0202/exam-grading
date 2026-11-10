//! 校准门禁集成测试（task-78，合成 judge + 校准表，不依赖模型）。
//!
//! 证明：有可信校准表 → 标准模式按校准置信度走阈值自动接受；无表/伪造 id →
//! 保持 `needs_review(uncalibrated)`；阈值边界生效；`GradeRecord.confidence` 为校准后值。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod common;

use common::*;
use eg_domain::{Confidence, GradeMode, GradeStatus, QuestionKind, ReviewReasonKind};
use eg_grade::ports::{FixedClock, InMemoryIdempotency, SequentialIds};
use eg_grade::{GradeConfig, Grader};
use eg_subject_math::MathSubjectPack;

fn build<'a>(
    subject: &'a MathSubjectPack,
    rubrics: &'a eg_grade::ports::FixtureRubrics,
    judge: &'a dyn eg_grade::ports::JudgeBackend,
    config: &'a GradeConfig,
    clock: &'a FixedClock,
    ids: &'a SequentialIds,
    idempotency: &'a InMemoryIdempotency,
) -> Grader<'a> {
    Grader {
        subject,
        rubrics,
        judge,
        classifier: None,
        config,
        clock,
        ids,
        idempotency,
    }
}

fn short_answer_question() -> eg_domain::Question {
    question(
        301,
        QuestionKind::ShortAnswer,
        5000,
        expression_key("\\sqrt{x^2}"),
        None,
    )
}

#[test]
fn calibrated_response_auto_accepts_but_uncalibrated_reviews() {
    let subject = MathSubjectPack::new();
    let rubrics = eg_grade::ports::FixtureRubrics::new();
    let config = GradeConfig::default();
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let question = short_answer_question();
    let answer = answer(question.clone(), "xyz");

    // 可信校准：calibration_id + calibrated_confidence + verified → 0.95/0.9 均 ≥0.90
    let calibrated_response = match_response(5000, true);
    let calibrated_response = eg_grade::ports::JudgeResponse {
        calibrated_confidence: Some(Confidence::new(0.92).unwrap()),
        ..calibrated_response
    };
    let calibrated_judge = scripted(vec![respond(calibrated_response)]);
    let store = InMemoryIdempotency::new();
    let grader = build(
        &subject,
        &rubrics,
        &calibrated_judge,
        &config,
        &clock,
        &ids,
        &store,
    );
    let result = grader
        .grade_submission(&submission_request(
            paper(vec![question.clone()]),
            vec![answer.clone()],
            GradeMode::Standard,
        ))
        .unwrap();
    assert_eq!(result.records[0].record.status, GradeStatus::AutoAccepted);
    assert!((result.records[0].record.confidence.get() - 0.92).abs() < 1e-9);

    // 未校准：同判定、同 raw confidence 仍必须 review(uncalibrated)
    let uncalibrated_judge = scripted(vec![respond(match_response(5000, false))]);
    let store = InMemoryIdempotency::new();
    let grader = build(
        &subject,
        &rubrics,
        &uncalibrated_judge,
        &config,
        &clock,
        &ids,
        &store,
    );
    let result = grader
        .grade_submission(&submission_request(
            paper(vec![question]),
            vec![answer],
            GradeMode::Standard,
        ))
        .unwrap();
    assert_eq!(result.records[0].record.status, GradeStatus::NeedsReview);
    assert!(
        result.records[0]
            .blockers
            .contains(&ReviewReasonKind::UncalibratedModel)
    );
}

#[test]
fn calibrated_confidence_threshold_boundary_and_fake_id_rejected() {
    let subject = MathSubjectPack::new();
    let rubrics = eg_grade::ports::FixtureRubrics::new();
    let config = GradeConfig::default();
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let question = short_answer_question();
    let answer = answer(question.clone(), "xyz");

    // 0.80 < 0.85 ModelDirect 标准阈值 → 即使有可信校准也 review(low_confidence)
    let mut low = match_response(5000, true);
    low.calibrated_confidence = Some(Confidence::new(0.80).unwrap());
    let judge = scripted(vec![respond(low)]);
    let store = InMemoryIdempotency::new();
    let grader = build(&subject, &rubrics, &judge, &config, &clock, &ids, &store);
    let result = grader
        .grade_submission(&submission_request(
            paper(vec![question.clone()]),
            vec![answer.clone()],
            GradeMode::Standard,
        ))
        .unwrap();
    assert_eq!(result.records[0].record.status, GradeStatus::NeedsReview);
    assert!(
        result.records[0]
            .blockers
            .contains(&ReviewReasonKind::LowConfidence)
    );

    // 伪造 id：有 id/无 verified（无表）→ 不视为已校准
    let mut fake = match_response(5000, true);
    fake.calibration_verified = false;
    let judge = scripted(vec![respond(fake)]);
    let store = InMemoryIdempotency::new();
    let grader = build(&subject, &rubrics, &judge, &config, &clock, &ids, &store);
    let result = grader
        .grade_submission(&submission_request(
            paper(vec![question]),
            vec![answer],
            GradeMode::Standard,
        ))
        .unwrap();
    assert_eq!(result.records[0].record.status, GradeStatus::NeedsReview);
    assert!(
        result.records[0]
            .blockers
            .contains(&ReviewReasonKind::UncalibratedModel)
    );
}
