//! 学科契约测试：一个合规实现 + 若干故意违规的反例包（`SPEC-domain.md` §9、§12）。
//!
//! 目的：证明 [`eg_domain::contract`] 的可复用契约套件既能放行合规实现，
//! 也能逐项报出违规，从而可被 `eg-subject-math` 等下游 crate 直接复用。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};

use eg_domain::contract::{
    ContractCase, ContractSuite, ContractViolation, check_subject_pack, contains_code,
    format_violations,
};
use eg_domain::{
    AnswerKey, AnswerValue, BBox, ComparatorId, ConcreteKey, Confidence, NormalizedAnswer, Paper,
    PaperId, Question, QuestionId, QuestionKind, RawAnswer, Rubric, RubricId, RubricItem,
    RubricItemId, ScoringMode, StepEvidence, StepMatch, StudentId, SubjectCapabilities, SubjectId,
    SubjectPack, Timestamp, Ulid,
};

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

fn timestamp() -> Timestamp {
    eg_domain::time::parse_rfc3339("2026-11-02T00:00:00Z").unwrap()
}

fn ulid(value: u128) -> Ulid {
    Ulid::from_u128(value)
}

fn rubric() -> Rubric {
    Rubric {
        id: RubricId::from_ulid(ulid(900)),
        version: 1,
        items: vec![
            RubricItem {
                id: RubricItemId::from_ulid(ulid(901)),
                criterion: "写出公式".to_owned(),
                points: eg_domain::MilliScore::from_milli(1000).unwrap(),
                required: true,
                depends_on: Vec::new(),
            },
            RubricItem {
                id: RubricItemId::from_ulid(ulid(902)),
                criterion: "得出结果".to_owned(),
                points: eg_domain::MilliScore::from_milli(1500).unwrap(),
                required: false,
                depends_on: vec![RubricItemId::from_ulid(ulid(901))],
            },
        ],
        mode: ScoringMode::Stepwise,
        total: eg_domain::MilliScore::from_milli(2500).unwrap(),
        origin: eg_domain::RubricOrigin::TeacherHandwritten {
            confirmed_by: eg_domain::LOCAL_TEACHER.to_owned(),
            confirmed_at: timestamp(),
        },
        partial_choice: None,
    }
}

fn question_with(key: &ConcreteKey) -> Question {
    Question {
        id: QuestionId::from_ulid(ulid(100)),
        paper_id: PaperId::from_ulid(ulid(101)),
        ordinal: 1,
        label: "17(2)".to_owned(),
        kind: QuestionKind::NumericOnly,
        max_score: eg_domain::MilliScore::from_milli(2500).unwrap(),
        stem: None,
        key: AnswerKey::Concrete(key.clone()),
        rubric: Some(rubric()),
        subject_hint: Some(SubjectId::math()),
        knowledge_tags: vec!["函数".to_owned()],
    }
}

fn question_multiple_choice(key: &ConcreteKey) -> Question {
    Question {
        id: QuestionId::from_ulid(ulid(110)),
        kind: QuestionKind::MultipleChoice,
        label: "5".to_owned(),
        max_score: eg_domain::MilliScore::from_milli(6000).unwrap(),
        rubric: None,
        key: AnswerKey::Concrete(key.clone()),
        ..question_with(key)
    }
}

fn answer(text: &str) -> NormalizedAnswer {
    NormalizedAnswer {
        text: text.to_owned(),
        latex: None,
        value: None,
        steps: Vec::new(),
    }
}

fn raw(text: &str) -> RawAnswer {
    RawAnswer {
        text: text.to_owned(),
        latex: None,
        blocks: Vec::new(),
        source: eg_domain::AnswerSource::Recognition,
    }
}

fn suite() -> ContractSuite {
    let numeric_key = ConcreteKey {
        primary: AnswerValue::Text("2.5".to_owned()),
        alternatives: Vec::new(),
        comparators: Vec::new(),
    };
    let choice_key = ConcreteKey {
        primary: AnswerValue::Options(vec!["A".to_owned(), "B".to_owned()]),
        alternatives: Vec::new(),
        comparators: vec![ComparatorId::new("text_exact").unwrap()],
    };
    ContractSuite {
        cases: vec![
            ContractCase {
                question: question_with(&numeric_key),
                key: numeric_key,
                raw: raw("2.5"),
                correct: answer("2.5"),
                wrong: Some(answer("3")),
                malformed: answer("\\bad"),
            },
            ContractCase {
                question: question_multiple_choice(&choice_key),
                key: choice_key,
                raw: raw("AB"),
                correct: answer("AB"),
                wrong: Some(answer("AC")),
                malformed: answer("\\bad"),
            },
        ],
    }
}

// ---------------------------------------------------------------------------
// 比较器与学科包：一个合规实现 + 可配置违规项
// ---------------------------------------------------------------------------

struct TextComparator {
    id: ComparatorId,
    parse_as_not_equal: bool,
    always_equal: bool,
    always_not_equal: bool,
}

impl TextComparator {
    fn new(id: &str) -> Self {
        Self {
            id: ComparatorId::new(id).unwrap(),
            parse_as_not_equal: false,
            always_equal: false,
            always_not_equal: false,
        }
    }
}

impl eg_domain::Comparator for TextComparator {
    fn id(&self) -> ComparatorId {
        self.id.clone()
    }

    fn compare(
        &self,
        expected: &AnswerValue,
        answer: &NormalizedAnswer,
    ) -> eg_domain::ComparatorVerdict {
        use eg_domain::ComparatorVerdict;
        if answer.text.contains("\\bad") {
            return if self.parse_as_not_equal {
                ComparatorVerdict::NotEqual {
                    reason: "文本不同".to_owned(),
                }
            } else {
                ComparatorVerdict::Undecidable {
                    reason: "输入无法解析".to_owned(),
                }
            };
        }
        if self.always_equal {
            return ComparatorVerdict::Equal;
        }
        if self.always_not_equal {
            return ComparatorVerdict::NotEqual {
                reason: "固定判为不等（反例包）".to_owned(),
            };
        }
        match expected {
            AnswerValue::Text(text) if *text == answer.text => ComparatorVerdict::Equal,
            AnswerValue::Text(_) => ComparatorVerdict::NotEqual {
                reason: "文本不同".to_owned(),
            },
            AnswerValue::Options(options) => {
                let mut expected_letters: Vec<String> = options.clone();
                expected_letters.sort();
                let mut actual_letters: Vec<String> =
                    answer.text.chars().map(|ch| ch.to_string()).collect();
                actual_letters.sort();
                if expected_letters == actual_letters {
                    ComparatorVerdict::Equivalent {
                        note: "选项集合一致".to_owned(),
                    }
                } else {
                    ComparatorVerdict::NotEqual {
                        reason: "选项集合不同".to_owned(),
                    }
                }
            }
            _ => ComparatorVerdict::Undecidable {
                reason: "该比较器不支持此答案值类型".to_owned(),
            },
        }
    }
}

/// 可配置反例包：默认配置即合规实现；每个 `true` 打开一项违规。
#[derive(Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "测试夹具用布尔开关逐项制造违规"
)]
struct FixturePack {
    non_idempotent: bool,
    parse_as_not_equal: bool,
    undeclared: bool,
    empty_chain: bool,
    reverse_chain: bool,
    kinds: Option<Vec<QuestionKind>>,
    always_equal: bool,
    always_not_equal: bool,
    static_error: bool,
    supports_step_scoring: bool,
    step_evidence_always: bool,
    normalize_fails: bool,
    empty_profile: bool,
    empty_display_name: bool,
    normalize_calls: AtomicUsize,
}

impl FixturePack {
    fn compliant() -> Self {
        Self {
            supports_step_scoring: true,
            ..Self::default()
        }
    }
}

impl SubjectPack for FixturePack {
    fn id(&self) -> SubjectId {
        SubjectId::math()
    }

    fn display_name(&self) -> String {
        if self.empty_display_name {
            String::new()
        } else {
            "数学".to_owned()
        }
    }

    fn capabilities(&self) -> SubjectCapabilities {
        SubjectCapabilities {
            kinds: self
                .kinds
                .clone()
                .unwrap_or_else(|| vec![QuestionKind::NumericOnly, QuestionKind::MultipleChoice]),
            comparators: vec![ComparatorId::new("text_exact").unwrap()],
            prompt_version: "math-judge-v1".to_owned(),
            supports_step_scoring: self.supports_step_scoring,
        }
    }

    fn normalize(&self, raw: &RawAnswer) -> Result<NormalizedAnswer, eg_domain::NormalizeError> {
        if self.normalize_fails {
            return Err(eg_domain::NormalizeError::Unparsable {
                input: raw.text.clone(),
                reason: "反例包：固定失败".to_owned(),
            });
        }
        let mut text = raw.text.trim().to_owned();
        if self.non_idempotent {
            let calls = self.normalize_calls.fetch_add(1, Ordering::SeqCst);
            if calls % 2 == 1 {
                text.push('x');
            }
        }
        Ok(NormalizedAnswer {
            text,
            latex: raw.latex.clone(),
            value: None,
            steps: Vec::new(),
        })
    }

    fn comparator_chain(
        &self,
        key: &ConcreteKey,
        _kind: QuestionKind,
    ) -> Vec<Box<dyn eg_domain::Comparator>> {
        if self.empty_chain {
            return Vec::new();
        }
        let mut ids = if key.comparators.is_empty() {
            vec![ComparatorId::new("text_exact").unwrap()]
        } else {
            key.comparators.clone()
        };
        if self.undeclared {
            ids.push(ComparatorId::new("hidden_comp").unwrap());
        }
        if self.reverse_chain {
            ids.reverse();
        }
        ids.into_iter()
            .map(|id| {
                let mut comparator = TextComparator::new(id.as_str());
                comparator.parse_as_not_equal = self.parse_as_not_equal;
                comparator.always_equal = self.always_equal;
                comparator.always_not_equal = self.always_not_equal;
                Box::new(comparator) as Box<dyn eg_domain::Comparator>
            })
            .collect()
    }

    fn step_evidence(&self, rubric: &Rubric, _answer: &NormalizedAnswer) -> Option<StepEvidence> {
        if self.step_evidence_always {
            return Some(StepEvidence {
                steps: rubric
                    .items
                    .iter()
                    .map(|item| StepMatch {
                        rubric_item: item.id,
                        matched: true,
                        evidence: Some(BBox::full_page()),
                        note: None,
                    })
                    .collect(),
            });
        }
        if rubric.items.len() < 2 {
            // 证据位置未知时返回空，交由模型/人工（规范 §9）。
            return None;
        }
        Some(StepEvidence {
            steps: rubric
                .items
                .iter()
                .map(|item| StepMatch {
                    rubric_item: item.id,
                    matched: true,
                    evidence: None,
                    note: Some("反例包：可定位".to_owned()),
                })
                .collect(),
        })
    }

    fn judge_profile_id(&self) -> String {
        if self.empty_profile {
            String::new()
        } else {
            "math-judge-v1".to_owned()
        }
    }

    fn validate_question(&self, _question: &Question) -> Vec<eg_domain::RubricIssue> {
        if self.static_error {
            vec![eg_domain::RubricIssue::error("反例包：静态校验失败")]
        } else {
            Vec::new()
        }
    }
}

fn violations(pack: &FixturePack) -> Vec<ContractViolation> {
    check_subject_pack(pack, &suite())
}

fn assert_has(pack: &FixturePack, code: &str) {
    let found = violations(pack);
    assert!(
        contains_code(&found, code),
        "期望违规 {code}，实际：\n{}",
        format_violations(&found)
    );
}

// ---------------------------------------------------------------------------
// 测试：合规实现通过；每个反例包被对应规则拦下
// ---------------------------------------------------------------------------

#[test]
fn compliant_pack_passes_contract() {
    let found = violations(&FixturePack::compliant());
    assert!(
        found.is_empty(),
        "合规实现不应有违规：\n{}",
        format_violations(&found)
    );
}

#[test]
fn non_idempotent_normalization_is_rejected() {
    let pack = FixturePack {
        non_idempotent: true,
        ..FixturePack::compliant()
    };
    assert_has(&pack, "normalize_not_idempotent");
}

#[test]
fn normalize_failure_is_rejected() {
    let pack = FixturePack {
        normalize_fails: true,
        ..FixturePack::compliant()
    };
    assert_has(&pack, "normalize_failed");
}

#[test]
fn parse_failure_judged_not_equal_is_rejected() {
    let pack = FixturePack {
        parse_as_not_equal: true,
        ..FixturePack::compliant()
    };
    assert_has(&pack, "parse_failure_judged_not_equal");
}

#[test]
fn empty_chain_is_rejected() {
    let pack = FixturePack {
        empty_chain: true,
        ..FixturePack::compliant()
    };
    assert_has(&pack, "empty_comparator_chain");
}

#[test]
fn undeclared_comparator_is_rejected() {
    let pack = FixturePack {
        undeclared: true,
        ..FixturePack::compliant()
    };
    assert_has(&pack, "undeclared_comparator");
}

#[test]
fn reversed_chain_is_rejected() {
    // 案例 2 的答案键声明了 1 个比较器；再补一个声明顺序案例由用例 2 覆盖，
    // 这里用「声明顺序被反转」的最小构造：把两个比较器声明进答案键。
    let pack = FixturePack {
        reverse_chain: true,
        ..FixturePack::compliant()
    };
    let mut custom = suite();
    for case in &mut custom.cases {
        case.key.comparators = vec![
            ComparatorId::new("text_exact").unwrap(),
            ComparatorId::new("text_exact_2").unwrap(),
        ];
        case.question.key = AnswerKey::Concrete(case.key.clone());
    }
    let found = check_subject_pack(&pack, &custom);
    assert!(
        contains_code(&found, "chain_order_violation"),
        "实际：\n{}",
        format_violations(&found)
    );
}

#[test]
fn unsupported_kind_is_rejected() {
    let pack = FixturePack {
        kinds: Some(vec![QuestionKind::Proof]),
        ..FixturePack::compliant()
    };
    assert_has(&pack, "declared_kind_unsupported");
}

#[test]
fn correct_answer_rejection_is_rejected() {
    let pack = FixturePack {
        always_not_equal: true,
        ..FixturePack::compliant()
    };
    assert_has(&pack, "correct_answer_rejected");
}

#[test]
fn wrong_answer_acceptance_is_rejected() {
    let pack = FixturePack {
        always_equal: true,
        ..FixturePack::compliant()
    };
    assert_has(&pack, "wrong_answer_accepted");
}

#[test]
fn static_validation_error_is_rejected() {
    let pack = FixturePack {
        static_error: true,
        ..FixturePack::compliant()
    };
    assert_has(&pack, "static_validation_rejects_case");
}

#[test]
fn step_scoring_capability_mismatch_is_rejected() {
    let pack = FixturePack {
        supports_step_scoring: false,
        step_evidence_always: true,
        ..FixturePack::compliant()
    };
    assert_has(&pack, "step_scoring_capability_mismatch");
}

#[test]
fn empty_profile_and_display_name_are_rejected() {
    let pack = FixturePack {
        empty_profile: true,
        ..FixturePack::compliant()
    };
    assert_has(&pack, "empty_judge_profile");

    let pack = FixturePack {
        empty_display_name: true,
        ..FixturePack::compliant()
    };
    assert_has(&pack, "empty_display_name");
}

// ---------------------------------------------------------------------------
// 门禁工具
// ---------------------------------------------------------------------------

#[test]
fn error_issue_gate_blocks_grading() {
    use eg_domain::{RubricIssue, ensure_no_error_issues};
    assert!(ensure_no_error_issues(&[]).is_ok());
    assert!(ensure_no_error_issues(&[RubricIssue::warning("提示")]).is_ok());
    let blocked = ensure_no_error_issues(&[
        RubricIssue::warning("提示"),
        RubricIssue::error("分值合计不等于题目满分"),
    ]);
    assert!(matches!(
        blocked,
        Err(eg_domain::DomainError::RubricHasErrors { count: 1 })
    ));
}

#[test]
fn contract_suite_is_reusable_without_test_framework_deps() {
    // 套件本身不依赖 test/assert，只用普通函数返回违规列表（下游 crate 可直接调用）。
    let violations = eg_domain::contract_ok(&FixturePack::compliant(), &suite());
    assert!(violations.is_ok());
    let report = check_subject_pack(&FixturePack::compliant(), &suite());
    assert_eq!(report.len(), 0);
}

// 让夹具里的 `StudentId`/`Paper` 等导入被使用，避免 dead_code。
#[test]
fn fixtures_are_constructible() {
    let paper_id = PaperId::from_ulid(ulid(1));
    let student = StudentId::from_ulid(ulid(2));
    assert_eq!(student.to_string().len(), 26);
    let _paper = Paper {
        id: paper_id,
        classroom_id: eg_domain::ClassroomId::from_ulid(ulid(3)),
        title: "试卷".to_owned(),
        subject: SubjectId::math(),
        total_score: eg_domain::MilliScore::from_milli(2500).unwrap(),
        questions: Vec::new(),
        key_version: 1,
        rubric_version: 1,
        created_at: timestamp(),
    };
    let _confidence = Confidence::new(0.5).unwrap();
}
