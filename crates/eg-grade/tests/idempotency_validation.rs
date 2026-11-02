//! 幂等键命中不重复执行 + 非法模型输出 100% 拒绝 + 失败降级/汇总。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::vec_init_then_push
)]
mod common;

use common::*;
use eg_domain::{
    GradeMode, GradeStatus, Judgement, QuestionKind, ReviewReasonKind, UngradableReason,
};
use eg_grade::ports::{
    FixedClock, FixtureRubrics, InMemoryIdempotency, JudgeFailure, SequentialIds,
};
use eg_grade::{GradeConfig, Grader};

fn run_question(
    question: eg_domain::Question,
    answer: eg_grade::QuestionAnswer,
    judge: &dyn eg_grade::ports::JudgeBackend,
    config: &GradeConfig,
    store: &InMemoryIdempotency,
) -> eg_grade::SubmissionGrade {
    let subject = eg_subject_math::MathSubjectPack::new();
    let rubrics = FixtureRubrics::new();
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let paper = paper(vec![question]);
    let grader = Grader {
        subject: &subject,
        rubrics: &rubrics,
        judge,
        classifier: None,
        config,
        clock: &clock,
        ids: &ids,
        idempotency: store,
    };
    grader
        .grade_submission(&submission_request(
            paper,
            vec![answer],
            GradeMode::Standard,
        ))
        .unwrap()
}

#[test]
fn idempotency_key_prevents_second_judge_call() {
    let question = question(
        401,
        QuestionKind::Calculation,
        5000,
        expression_key("\\sqrt{x^2}"),
        None,
    );
    let answer = answer(question.clone(), "x");
    let judge = scripted(vec![respond(match_response(5000, true))]);
    let config = GradeConfig::default();
    let store = InMemoryIdempotency::new();
    let first = run_question(question.clone(), answer.clone(), &judge, &config, &store);
    assert!(!first.records[0].idempotent_hit);
    assert_eq!(judge.calls().len(), 1);
    let second = run_question(question, answer, &judge, &config, &store);
    assert!(second.records[0].idempotent_hit);
    assert_eq!(judge.calls().len(), 1, "幂等命中不得重复计费/判分");
    assert_eq!(first.records[0].record.id, second.records[0].record.id);
}

fn invalid_responses() -> Vec<(&'static str, eg_grade::ports::JudgeResponse, u32)> {
    let mut cases = Vec::new();
    // 一致但未满分
    cases.push((
        "match-not-full",
        judge_response(
            eg_grade::JudgeVerdict::Match,
            Some(4000),
            vec![],
            vec![],
            "理由",
            true,
        ),
        2,
    ));
    // 不一致但非 0
    cases.push((
        "mismatch-nonzero",
        judge_response(
            eg_grade::JudgeVerdict::Mismatch,
            Some(1000),
            vec![],
            vec![],
            "理由",
            true,
        ),
        2,
    ));
    // 部分但 0 分/满分
    cases.push((
        "partial-zero",
        judge_response(
            eg_grade::JudgeVerdict::Partial,
            Some(0),
            vec![],
            vec![],
            "理由",
            true,
        ),
        2,
    ));
    // 粒度违规（150 不是默认 100 的整数倍）
    cases.push((
        "granularity",
        judge_response(
            eg_grade::JudgeVerdict::Partial,
            Some(150),
            vec![],
            vec![],
            "理由",
            true,
        ),
        2,
    ));
    // 理由为空
    let mut empty_reason = match_response(5000, true);
    empty_reason.reason.clear();
    empty_reason.reason.push_str("   ");
    cases.push(("empty-reason", empty_reason, 2));
    // 原始输出为空
    let mut null_output = match_response(5000, true);
    null_output.output = serde_json::Value::Null;
    cases.push(("null-output", null_output, 2));
    // 云端调用缺少授权（不可修复 → 只调用 1 次）
    let mut cloud = match_response(5000, true);
    cloud.engine.backend = eg_domain::BackendId::openai_compat();
    cases.push(("cloud-consent", cloud, 1));
    // 命中候选序号越界（不可修复）
    let mut alternative = match_response(5000, true);
    alternative.matched_alternative = Some(5);
    cases.push(("alternative-range", alternative, 1));
    // 逐项覆盖缺失（有评分标准时）
    let rubric = additive_rubric();
    let mut coverage = judge_response(
        eg_grade::JudgeVerdict::Match,
        Some(5000),
        vec![rubric.items[0].id],
        Vec::new(),
        "覆盖缺失",
        true,
    );
    coverage.output = serde_json::json!({"ok": true});
    cases.push(("coverage", coverage, 2));
    cases
}

#[test]
fn invalid_model_outputs_are_all_rejected() {
    for (name, response, expected_calls) in invalid_responses() {
        let is_rubric_case = name == "coverage";
        let question = if is_rubric_case {
            question(
                402,
                QuestionKind::Calculation,
                5000,
                eg_domain::AnswerKey::RubricOnly,
                Some(additive_rubric()),
            )
        } else {
            question(402, QuestionKind::ShortAnswer, 5000, text_key("abc"), None)
        };
        let answer = answer(question.clone(), "xyz");
        let judge = scripted(vec![respond(response.clone()), respond(response)]);
        let config = GradeConfig::default();
        let store = InMemoryIdempotency::new();
        // 评分标准题需要规则源确认，否则就绪失败：单独构造带规则的 grader
        let subject = eg_subject_math::MathSubjectPack::new();
        let mut rubrics = FixtureRubrics::new();
        if is_rubric_case {
            rubrics.insert_confirmed(question.id, additive_rubric());
        }
        let clock = FixedClock::new(ts());
        let ids = SequentialIds::new();
        let paper = paper(vec![question]);
        let grader = Grader {
            subject: &subject,
            rubrics: &rubrics,
            judge: &judge,
            classifier: None,
            config: &config,
            clock: &clock,
            ids: &ids,
            idempotency: &store,
        };
        let grade = grader
            .grade_submission(&submission_request(
                paper,
                vec![answer],
                GradeMode::Standard,
            ))
            .expect("判分本身应成功（不 panic）");
        let record = &grade.records[0].record;
        assert!(
            matches!(record.judgement, Judgement::Ungradable { .. }),
            "{name} 应被拒绝：{:?}",
            record.judgement
        );
        assert_eq!(record.status, GradeStatus::NeedsReview, "{name}");
        assert_eq!(record.awarded.as_milli(), 0, "{name} 不得给 0 分成功");
        assert_eq!(
            judge.calls().len(),
            usize::try_from(expected_calls).unwrap(),
            "{name}"
        );
    }
}

#[test]
fn timeout_and_backend_unavailable_degrade_to_manual() {
    for (failure, expected_reason) in [
        (JudgeFailure::Timeout, UngradableReason::Timeout),
        (
            JudgeFailure::BackendUnavailable {
                reason: "模型服务不可用".to_owned(),
            },
            UngradableReason::BackendUnavailable,
        ),
    ] {
        let question = question(403, QuestionKind::ShortAnswer, 5000, text_key("abc"), None);
        let answer = answer(question.clone(), "xyz");
        let judge = scripted(vec![fail(failure)]);
        let config = GradeConfig::default();
        let store = InMemoryIdempotency::new();
        let grade = run_question(question, answer, &judge, &config, &store);
        let record = &grade.records[0].record;
        assert!(
            matches!(
                record.judgement,
                Judgement::Ungradable { reason } if reason == expected_reason
            ),
            "{:?}",
            record.judgement
        );
        assert_eq!(record.status, GradeStatus::NeedsReview);
        assert!(grade.records[0].blockers.iter().any(|reason| matches!(
            reason,
            ReviewReasonKind::Ungradable | ReviewReasonKind::BackendUnavailable
        )));
    }
}

#[test]
fn missing_answer_is_explicit_failure_not_silent_drop() {
    let question = question(
        404,
        QuestionKind::NumericOnly,
        5000,
        number_key("2.5"),
        None,
    );
    let judge = scripted(vec![]);
    let config = GradeConfig::default();
    let subject = eg_subject_math::MathSubjectPack::new();
    let rubrics = FixtureRubrics::new();
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let store = InMemoryIdempotency::new();
    let paper = paper(vec![question]);
    let grader = Grader {
        subject: &subject,
        rubrics: &rubrics,
        judge: &judge,
        classifier: None,
        config: &config,
        clock: &clock,
        ids: &ids,
        idempotency: &store,
    };
    let grade = grader
        .grade_submission(&submission_request(paper, Vec::new(), GradeMode::Standard))
        .unwrap();
    assert_eq!(grade.failures.len(), 1);
    assert_eq!(grade.summary.failed, 1);
    assert_eq!(grade.summary.total_questions, 1);
    assert!(grade.records.is_empty());
}
