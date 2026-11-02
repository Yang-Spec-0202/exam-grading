//! 学科契约测试：`eg-subject-math` 必须通过 `eg-domain` 的契约套件
//! （`SPEC-domain.md` §9、`SPEC-subject-math.md` §2/§12）。
//!
//! 同时验证三条硬约束：
//! 1. 解析失败不得判为「不相等」（归一化错误 vs 比较器无法判定）；
//! 2. 归一化幂等（同输入多次调用结果一致）；
//! 3. 静态校验对合法题目不产生错误级问题。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use eg_domain::contract::{ContractCase, ContractSuite, check_subject_pack, contract_ok};
use eg_domain::{
    AnswerKey, AnswerSource, ComparatorVerdict, ConcreteKey, MilliScore, NormalizedAnswer, PaperId,
    Question, QuestionId, QuestionKind, RawAnswer, SubjectId, SubjectPack, Ulid,
};
use eg_subject_math::MathSubjectPack;

fn ulid(value: u128) -> Ulid {
    Ulid::from_u128(value)
}

fn timestamp() -> eg_domain::Timestamp {
    eg_domain::time::parse_rfc3339("2026-11-02T00:00:00Z").unwrap()
}

fn question(kind: QuestionKind, key: AnswerKey) -> Question {
    Question {
        id: QuestionId::from_ulid(ulid(1)),
        paper_id: PaperId::from_ulid(ulid(2)),
        ordinal: 1,
        label: "1".to_owned(),
        kind,
        max_score: MilliScore::from_milli(2_500).unwrap(),
        stem: None,
        key,
        rubric: None,
        subject_hint: Some(SubjectId::math()),
        knowledge_tags: Vec::new(),
    }
}

fn answer(text: &str, latex: Option<&str>) -> NormalizedAnswer {
    NormalizedAnswer {
        text: text.to_owned(),
        latex: latex.map(ToString::to_string),
        value: None,
        steps: Vec::new(),
    }
}

fn raw(text: &str) -> RawAnswer {
    RawAnswer {
        text: text.to_owned(),
        latex: None,
        blocks: Vec::new(),
        source: AnswerSource::Recognition,
    }
}

fn suite() -> ContractSuite {
    let numeric_key = ConcreteKey {
        primary: eg_domain::AnswerValue::Number {
            value: eg_domain::Decimal::parse("2.5").unwrap(),
            unit: None,
            tolerance: None,
        },
        alternatives: Vec::new(),
        comparators: Vec::new(),
    };
    let choice_key = ConcreteKey {
        primary: eg_domain::AnswerValue::Options(vec!["A".to_owned(), "B".to_owned()]),
        alternatives: Vec::new(),
        comparators: Vec::new(),
    };
    ContractSuite {
        cases: vec![
            ContractCase {
                question: question(
                    QuestionKind::NumericOnly,
                    AnswerKey::Concrete(numeric_key.clone()),
                ),
                key: numeric_key,
                raw: raw("2.5"),
                correct: answer("2.5", Some("2.5")),
                wrong: Some(answer("3", Some("3"))),
                malformed: answer("\\bad", None),
            },
            ContractCase {
                question: question(
                    QuestionKind::MultipleChoice,
                    AnswerKey::Concrete(choice_key.clone()),
                ),
                key: choice_key,
                raw: raw("AB"),
                correct: answer("AB", None),
                wrong: Some(answer("AC", None)),
                malformed: answer("\\bad", None),
            },
        ],
    }
}

#[test]
fn math_pack_passes_domain_contract_suite() {
    let pack = MathSubjectPack::new();
    let violations = check_subject_pack(&pack, &suite());
    assert!(
        violations.is_empty(),
        "数学学科包未通过契约套件：\n{}",
        eg_domain::contract::format_violations(&violations)
    );
    assert!(contract_ok(&pack, &suite()).is_ok());
}

#[test]
fn parse_failure_never_judged_not_equal() {
    let pack = MathSubjectPack::new();
    let key = ConcreteKey {
        primary: eg_domain::AnswerValue::Number {
            value: eg_domain::Decimal::parse("2.5").unwrap(),
            unit: None,
            tolerance: None,
        },
        alternatives: Vec::new(),
        comparators: Vec::new(),
    };
    // 归一化明确失败的输入 → 类型化错误（grade 会转人工，不是 0 分）
    let malformed = pack.normalize(&raw("x^")).unwrap_err();
    assert_ne!(malformed.to_string(), "");

    // 若绕过归一化直接交给比较器，必须「无法判定」而不是「不相等」
    let malformed_answer = answer("\\bad", None);
    let chain = pack.comparator_chain(&key, QuestionKind::NumericOnly);
    let outcome =
        eg_domain::contract::run_comparator_chain(&chain, &key.primary, &malformed_answer);
    assert!(outcome.verdict.is_undecidable(), "{:?}", outcome.verdict);
    match outcome.verdict {
        ComparatorVerdict::Undecidable { reason } => assert_ne!(reason, ""),
        other => panic!("期望无法判定，实际 {other:?}"),
    }
}

#[test]
fn normalization_is_idempotent_for_pack() {
    let pack = MathSubjectPack::new();
    for input in [
        "2.5",
        "1/3",
        "\\sqrt{12}",
        "(x+1)^2",
        "x=1 或 x=2",
        "[1,2)",
        "12\\text{cm}",
        "对",
        "AC",
        "answer",
    ] {
        let once = pack.normalize(&raw(input)).unwrap();
        let twice = pack
            .normalize(&RawAnswer {
                text: once.text.clone(),
                latex: once.latex.clone(),
                blocks: Vec::new(),
                source: AnswerSource::Recognition,
            })
            .unwrap();
        assert_eq!(once, twice, "{input}");
    }
}

#[test]
fn static_validation_has_no_errors_for_well_formed_questions() {
    let pack = MathSubjectPack::new();
    let numeric = question(
        QuestionKind::NumericOnly,
        AnswerKey::Concrete(ConcreteKey {
            primary: eg_domain::AnswerValue::Number {
                value: eg_domain::Decimal::parse("2.5").unwrap(),
                unit: None,
                tolerance: None,
            },
            alternatives: Vec::new(),
            comparators: Vec::new(),
        }),
    );
    let issues = pack.validate_question(&numeric);
    assert!(
        !issues.iter().any(eg_domain::RubricIssue::is_error),
        "{issues:?}"
    );

    let choice = question(
        QuestionKind::MultipleChoice,
        AnswerKey::Concrete(ConcreteKey {
            primary: eg_domain::AnswerValue::Options(vec!["A".to_owned(), "B".to_owned()]),
            alternatives: Vec::new(),
            comparators: Vec::new(),
        }),
    );
    assert!(
        !pack
            .validate_question(&choice)
            .iter()
            .any(eg_domain::RubricIssue::is_error)
    );
}

#[test]
fn capability_declaration_is_truthful() {
    let pack = MathSubjectPack::new();
    let capabilities = pack.capabilities();
    assert!(capabilities.validate().is_ok());
    assert!(!capabilities.supports_step_scoring);
    // 声明了哪些比较器，就必须能构造出同 id 的比较器
    for id in &capabilities.comparators {
        let comparator = eg_subject_math::comparator_by_id(id);
        assert_eq!(comparator.id(), id.clone());
    }
    assert_eq!(pack.id(), SubjectId::math());
    assert_ne!(pack.display_name(), "");
    assert_ne!(pack.judge_profile_id(), "");
    assert!(
        pack.step_evidence(&dummy_rubric(), &answer("1", Some("1")))
            .is_none()
    );
    let _ = timestamp();
}

fn dummy_rubric() -> eg_domain::Rubric {
    use eg_domain::{RubricId, RubricItem, RubricItemId, RubricOrigin, ScoringMode};
    eg_domain::Rubric {
        id: RubricId::from_ulid(ulid(9)),
        version: 1,
        items: vec![RubricItem {
            id: RubricItemId::from_ulid(ulid(10)),
            criterion: "步骤".to_owned(),
            points: MilliScore::from_milli(1_000).unwrap(),
            required: true,
            depends_on: Vec::new(),
        }],
        mode: ScoringMode::Additive,
        total: MilliScore::from_milli(1_000).unwrap(),
        origin: RubricOrigin::TeacherHandwritten {
            confirmed_by: eg_domain::LOCAL_TEACHER.to_owned(),
            confirmed_at: timestamp(),
        },
        partial_choice: None,
    }
}
