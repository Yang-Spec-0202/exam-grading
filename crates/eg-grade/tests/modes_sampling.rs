//! 标准/快速阈值、未校准模型、抽检可复现（40 组）、排除/缺考汇总。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod common;

use common::*;
use eg_domain::{ExcludeReason, GradeMode, GradeStatus, MilliScore, QuestionKind};
use eg_grade::ports::{FixedClock, InMemoryIdempotency, SequentialIds};
use eg_grade::sampling::{SamplingCandidate, sample_count, sample_fast};
use eg_grade::{GradeConfig, GradeError, Grader, SamplingResult};
use eg_subject_math::MathSubjectPack;

fn build_grader<'a>(
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

#[test]
fn standard_and_fast_thresholds_are_both_asserted() {
    let subject = MathSubjectPack::new();
    let rubrics = eg_grade::ports::FixtureRubrics::new();
    let config = GradeConfig::default();
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let standard_judge = scripted(vec![]);
    let standard_store = InMemoryIdempotency::new();
    let standard = build_grader(
        &subject,
        &rubrics,
        &standard_judge,
        &config,
        &clock,
        &ids,
        &standard_store,
    );
    let fast_judge = scripted(vec![]);
    let fast_store = InMemoryIdempotency::new();
    let fast = build_grader(
        &subject,
        &rubrics,
        &fast_judge,
        &config,
        &clock,
        &ids,
        &fast_store,
    );
    // 0.86 置信度：标准模式 RuleThenModel 阈值 0.90 → 待复核；快速模式 0.80 → 自动
    let numeric = question(
        101,
        QuestionKind::NumericOnly,
        5000,
        number_key("2.5"),
        None,
    );
    let numeric_answer = answer_with_confidence(numeric.clone(), "2.5", 0.86);
    let request = submission_request(
        paper(vec![numeric.clone()]),
        vec![numeric_answer.clone()],
        GradeMode::Standard,
    );
    let result = standard.grade_submission(&request).unwrap();
    assert_eq!(result.records[0].record.status, GradeStatus::NeedsReview);
    assert!(
        result.records[0]
            .blockers
            .contains(&eg_domain::ReviewReasonKind::LowConfidence)
    );
    let request = submission_request(paper(vec![numeric]), vec![numeric_answer], GradeMode::Fast);
    let result = fast.grade_submission(&request).unwrap();
    assert_eq!(result.records[0].record.status, GradeStatus::AutoAccepted);
    // 0.84 置信度：标准模式单选阈值 0.85 → 待复核；快速 0.80 → 自动
    let choice = question(
        102,
        QuestionKind::SingleChoice,
        5000,
        options_key(&["A"]),
        None,
    );
    let choice_answer = answer_with_confidence(choice.clone(), "A", 0.84);
    let result = standard
        .grade_submission(&submission_request(
            paper(vec![choice.clone()]),
            vec![choice_answer.clone()],
            GradeMode::Standard,
        ))
        .unwrap();
    assert_eq!(result.records[0].record.status, GradeStatus::NeedsReview);
    let result = fast
        .grade_submission(&submission_request(
            paper(vec![choice]),
            vec![choice_answer],
            GradeMode::Fast,
        ))
        .unwrap();
    assert_eq!(result.records[0].record.status, GradeStatus::AutoAccepted);
}

#[test]
fn uncalibrated_model_fast_requires_high_confidence_and_marks_sampled() {
    let subject = MathSubjectPack::new();
    let rubrics = eg_grade::ports::FixtureRubrics::new();
    let config = GradeConfig::default();
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let question = question(
        111,
        QuestionKind::ShortAnswer,
        5000,
        expression_key("\\sqrt{x^2}"),
        None,
    );
    let answer = answer(question.clone(), "xyz");
    // 未校准但置信度 0.95：快速模式放行并强制抽检；标准模式进人工
    let response = match_response(5000, false);
    let judge = scripted(vec![respond(response.clone()), respond(response)]);
    let idempotency = InMemoryIdempotency::new();
    let grader = build_grader(
        &subject,
        &rubrics,
        &judge,
        &config,
        &clock,
        &ids,
        &idempotency,
    );
    let result = grader
        .grade_submission(&submission_request(
            paper(vec![question.clone()]),
            vec![answer.clone()],
            GradeMode::Fast,
        ))
        .unwrap();
    assert_eq!(result.records[0].record.status, GradeStatus::AutoAccepted);
    assert!(result.records[0].force_sampled);
    assert!(result.records[0].record.sampled, "快速模式放行必须标记抽检");
    // 标准模式：禁止未校准自动放行
    let judge = scripted(vec![respond(match_response(5000, false))]);
    let idempotency = InMemoryIdempotency::new();
    let grader = build_grader(
        &subject,
        &rubrics,
        &judge,
        &config,
        &clock,
        &ids,
        &idempotency,
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
            .contains(&eg_domain::ReviewReasonKind::UncalibratedModel)
    );
}

fn numeric_answers(paper_questions: &[eg_domain::Question]) -> Vec<eg_grade::QuestionAnswer> {
    paper_questions
        .iter()
        .map(|question| answer_with_confidence(question.clone(), "2.5", 0.95))
        .collect()
}

#[test]
fn fast_sampling_is_reproducible_and_marks_records() {
    let subject = MathSubjectPack::new();
    let rubrics = eg_grade::ports::FixtureRubrics::new();
    let config = GradeConfig::default();
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let questions: Vec<eg_domain::Question> = (0..10)
        .map(|index| {
            question(
                200 + u128::try_from(index).unwrap(),
                QuestionKind::NumericOnly,
                5000,
                number_key("2.5"),
                None,
            )
        })
        .collect();
    let answers = numeric_answers(&questions);
    let paper = paper(questions.clone());
    let run = |store: &InMemoryIdempotency| {
        let judge = scripted(vec![]);
        let grader = build_grader(&subject, &rubrics, &judge, &config, &clock, &ids, store);
        grader
            .grade_submission(&submission_request(
                paper.clone(),
                answers.clone(),
                GradeMode::Fast,
            ))
            .unwrap()
    };
    let first = run(&InMemoryIdempotency::new());
    let second = run(&InMemoryIdempotency::new());
    let first_sampling = first.sampling.as_ref().expect("应有抽检结果");
    let second_sampling = second.sampling.as_ref().unwrap();
    assert_eq!(first_sampling.seed, second_sampling.seed);
    assert_eq!(first_sampling.sampled, second_sampling.sampled);
    assert_eq!(
        first_sampling.sampled.len(),
        sample_count(10, config.sampling_rate)
    );
    let sampled_set: std::collections::BTreeSet<_> =
        first_sampling.sampled.iter().copied().collect();
    for graded in &first.records {
        assert_eq!(
            graded.record.sampled,
            sampled_set.contains(&graded.record.question_id)
        );
        assert_eq!(graded.record.mode, GradeMode::Fast);
    }
    assert_eq!(first.summary.sampled, first_sampling.sampled.len());
}

#[test]
fn forty_sampling_groups_are_identical_across_runs() {
    let config = GradeConfig::default();
    for group in 0..40u128 {
        let candidates: Vec<SamplingCandidate> = (0..=group % 7)
            .map(|index| SamplingCandidate {
                question_id: question_id(1_000 + group * 10 + index),
                confidence: if index % 3 == 0 {
                    confidence(0.85)
                } else {
                    confidence(0.95)
                },
                threshold: confidence(0.80),
            })
            .collect();
        let seed = u64::try_from(group).unwrap() * 7 + 1;
        let first: SamplingResult = sample_fast(&candidates, &config, seed);
        let second = sample_fast(&candidates, &config, seed);
        assert_eq!(first, second, "第 {group} 组不一致");
        if !candidates.is_empty() {
            assert_eq!(
                first.sampled.len(),
                sample_count(candidates.len(), config.sampling_rate)
            );
        }
    }
}

#[test]
fn excluded_and_absent_submissions_are_summarized_without_grading() {
    let subject = MathSubjectPack::new();
    let rubrics = eg_grade::ports::FixtureRubrics::new();
    let config = GradeConfig::default();
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let judge = scripted(vec![]);
    let store = InMemoryIdempotency::new();
    let grader = build_grader(&subject, &rubrics, &judge, &config, &clock, &ids, &store);
    let paper = paper(vec![question(
        301,
        QuestionKind::NumericOnly,
        5000,
        number_key("1"),
        None,
    )]);
    let mut request = submission_request(paper, Vec::new(), GradeMode::Standard);
    request.excluded = Some(ExcludeReason::Absent);
    let result = grader.grade_submission(&request).unwrap();
    assert!(result.records.is_empty());
    assert_eq!(result.summary.excluded, Some(ExcludeReason::Absent));
    assert!(result.summary.absent);
    assert_eq!(result.summary.total_questions, 1);
    assert_eq!(judge.calls().len(), 0);
    let _ = MilliScore::from_milli(0).unwrap();
    let _ = GradeError::FastModeDisabled;
}
