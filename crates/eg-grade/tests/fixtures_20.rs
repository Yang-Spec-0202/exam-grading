//! CP-C 判分夹具：20 题端到端（0 diff），覆盖「略/仅规则/多解/分步/低置信/多选部分给分/
//! 冲突/聚合不一致」。断言每题的（路由规则、判定标签、得分、状态）与预期逐项一致。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::vec_init_then_push
)]
mod common;

use common::*;
use eg_domain::{GradeMode, GradeStatus, Judgement, QuestionKind};
use eg_grade::ports::{FixedClock, FixtureRubrics, InMemoryIdempotency, SequentialIds};
use eg_grade::{GradedQuestion, Grader};

#[test]
fn twenty_question_fixture_matches_expected_diff_zero() {
    let mut rubrics = FixtureRubrics::new();
    // 多选部分给分（Q5/Q6）
    rubrics.insert_confirmed(question_id(5), partial_choice_rubric());
    rubrics.insert_confirmed(question_id(6), partial_choice_rubric());
    // 仅评分标准（Q17–Q19）
    rubrics.insert_confirmed(question_id(17), additive_rubric());
    rubrics.insert_confirmed(question_id(18), additive_rubric());
    rubrics.insert_confirmed(question_id(19), additive_rubric());

    let mut questions = Vec::new();
    // Q1/Q2 单选
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
    // Q3 判断
    questions.push(question(
        3,
        QuestionKind::TrueFalse,
        5000,
        text_key("正确"),
        None,
    ));
    // Q4/Q5/Q6 多选
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
    // Q7–Q9 数值
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
    // Q10/Q11 表达式
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
    // Q12/Q13 规则无法判定 → 升级模型
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
    // Q14–Q16 直接模型
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
    // Q17–Q19 仅评分标准
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
    // Q20 低置信 → 人工
    questions.push(question(
        20,
        QuestionKind::NumericOnly,
        5000,
        number_key("2.5"),
        None,
    ));

    let answers = vec![
        answer(questions[0].clone(), "A"),
        answer(questions[1].clone(), "B"),
        answer(questions[2].clone(), "对"),
        answer(questions[3].clone(), "AB"),
        answer(questions[4].clone(), "A"),
        answer(questions[5].clone(), "AC"),
        answer(questions[6].clone(), "2.5"),
        answer(questions[7].clone(), "3"),
        answer(questions[8].clone(), "2.6"),
        answer(questions[9].clone(), "(x+1)*(x-1)"),
        answer(questions[10].clone(), "5*x"),
        answer(questions[11].clone(), "x"),
        answer(questions[12].clone(), "x"),
        answer(questions[13].clone(), "abc"),
        answer(questions[14].clone(), "xyz"),
        answer(questions[15].clone(), "xyz"),
        answer(questions[16].clone(), "（1）2分；（2）3分"),
        answer(questions[17].clone(), "（1）2分"),
        answer(questions[18].clone(), "（1）2分；（2）3分"),
        answer_with_confidence(questions[19].clone(), "2.5", 0.5),
    ];
    let paper = paper(questions);

    let rubric_ids: Vec<eg_domain::RubricItemId> =
        additive_rubric().items.iter().map(|item| item.id).collect();
    let steps = vec![
        // Q12 模型一致
        respond(match_response(5000, true)),
        // Q13 模型部分一致
        respond(judge_response(
            eg_grade::JudgeVerdict::Partial,
            Some(2000),
            Vec::new(),
            Vec::new(),
            "部分正确",
            true,
        )),
        // Q14 模型一致
        respond(match_response(5000, true)),
        // Q15 模型不一致
        respond(mismatch_response(true)),
        // Q16 模型一致（与规则冲突）
        respond(match_response(5000, true)),
        // Q17 逐项全满足
        respond(judge_response(
            eg_grade::JudgeVerdict::Match,
            Some(5000),
            rubric_ids.clone(),
            Vec::new(),
            "两项均满足",
            true,
        )),
        // Q18 逐项部分
        respond(judge_response(
            eg_grade::JudgeVerdict::Partial,
            Some(2000),
            vec![rubric_ids[0]],
            vec![rubric_ids[1]],
            "只满足第一项",
            true,
        )),
        // Q19 声明 5000 但只满足一项（聚合=2000 → 冲突）
        respond(judge_response(
            eg_grade::JudgeVerdict::Match,
            Some(5000),
            vec![rubric_ids[0]],
            vec![rubric_ids[1]],
            "声明满分",
            true,
        )),
    ];
    let judge = scripted(steps);
    let config = eg_grade::GradeConfig::default();
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let idempotency = InMemoryIdempotency::new();
    let subject = eg_subject_math::MathSubjectPack::new();
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
    let request = submission_request(paper, answers, GradeMode::Standard);
    let grade = grader.grade_submission(&request).expect("判分应成功");

    assert_eq!(grade.failures.len(), 0, "{:?}", grade.failures);
    assert_eq!(grade.records.len(), 20);
    let expected: Vec<(&str, &str, i64, GradeStatus)> = vec![
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
    ];
    for (index, (graded, expected)) in grade.records.iter().zip(expected.iter()).enumerate() {
        assert_record(graded, expected, &format!("Q{}", index + 1));
    }
    assert_eq!(grade.summary.auto_accepted, 17);
    assert_eq!(grade.summary.needs_review, 3);
    assert_eq!(grade.summary.failed, 0);
    assert_eq!(grade.summary.total_questions, 20);
    // 低置信题不得调用模型：Q20 之后没有额外 judge 调用
    let calls = judge.calls();
    assert_eq!(calls.len(), 8);
    assert!(calls.iter().all(|call| call.question_id != question_id(20)));
    assert!(
        calls
            .iter()
            .all(|call| call.has_concrete_key || call.has_rubric)
    );
    // 记录结构不变量（域层校验已在流水线执行；此处复核状态与模式标记）
    for graded in &grade.records {
        assert_eq!(graded.record.mode, GradeMode::Standard);
        assert!(!graded.record.sampled);
        assert_ne!(graded.record.evidence, [] as [eg_domain::EvidenceRef; 0]);
    }
}

fn assert_record(graded: &GradedQuestion, expected: &(&str, &str, i64, GradeStatus), label: &str) {
    let (rule, judgement, awarded, status) = *expected;
    assert_eq!(graded.record.route_trace.rule_id, rule, "{label} 路由");
    assert_eq!(graded.record.judgement.label(), judgement, "{label} 判定");
    assert_eq!(graded.record.awarded.as_milli(), awarded, "{label} 得分");
    assert_eq!(graded.record.status, status, "{label} 状态");
    if matches!(graded.record.judgement, Judgement::Ungradable { .. }) {
        assert_eq!(graded.record.awarded.as_milli(), 0);
    }
}
