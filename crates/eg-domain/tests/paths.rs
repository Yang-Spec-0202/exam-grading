//! 补充覆盖测试：逐一触达公开 API 的展示、转换、错误与不变量分支。
//!
//! 目的：把领域层行覆盖率抬到 `SPEC-domain.md` §14 要求的 ≥95%，
//! 并为每条不变量提供至少一个反例断言。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::wildcard_imports,
    clippy::too_many_lines,
    clippy::similar_names,
    clippy::float_cmp
)]

use std::str::FromStr;

use eg_domain::*;

fn ts() -> Timestamp {
    eg_domain::time::parse_rfc3339("2026-11-02T00:00:00Z").unwrap()
}

fn u(value: u128) -> Ulid {
    Ulid::from_u128(value)
}

fn milli(value: i64) -> MilliScore {
    MilliScore::from_milli(value).unwrap()
}

fn conf(value: f64) -> Confidence {
    Confidence::new(value).unwrap()
}

fn dec(text: &str) -> Decimal {
    Decimal::parse(text).unwrap()
}

fn bbox() -> BBox {
    BBox::new(0.1, 0.2, 0.3, 0.4).unwrap()
}

fn rubric_item(id: u128, criterion: &str, points: i64) -> RubricItem {
    RubricItem {
        id: RubricItemId::from_ulid(u(id)),
        criterion: criterion.to_owned(),
        points: milli(points),
        required: true,
        depends_on: Vec::new(),
    }
}

fn origin() -> RubricOrigin {
    RubricOrigin::TeacherHandwritten {
        confirmed_by: LOCAL_TEACHER.to_owned(),
        confirmed_at: ts(),
    }
}

fn additive_rubric() -> Rubric {
    Rubric {
        id: RubricId::from_ulid(u(31)),
        version: 1,
        items: vec![
            rubric_item(32, "步骤一", 1000),
            rubric_item(33, "步骤二", 1500),
        ],
        mode: ScoringMode::Additive,
        total: milli(2500),
        origin: origin(),
        partial_choice: None,
    }
}

fn question() -> Question {
    Question {
        id: QuestionId::from_ulid(u(21)),
        paper_id: PaperId::from_ulid(u(11)),
        ordinal: 1,
        label: "1".to_owned(),
        kind: QuestionKind::NumericOnly,
        max_score: milli(2500),
        stem: None,
        key: AnswerKey::Missing,
        rubric: None,
        subject_hint: None,
        knowledge_tags: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// 值对象：Display / Debug / FromStr / TryFrom / serde 错误路径
// ---------------------------------------------------------------------------

#[test]
fn value_object_display_and_conversions() {
    // MilliScore：Display、TryFrom、Add（受检）。
    assert_eq!(milli(2500).to_string(), "2.5");
    assert_eq!(MilliScore::try_from(2500).unwrap(), milli(2500));
    assert!(MilliScore::try_from(-1).is_err());
    assert_eq!((milli(1000) + milli(500)).unwrap(), milli(1500));
    assert!(MilliScore::from_str("1.5").is_ok());
    assert!(MilliScore::from_str("x").is_err());
    assert!(format!("{:?}", milli(2500)).contains("2.5"));

    // Confidence：Display、Debug、TryFrom。
    assert_eq!(conf(0.85).to_string(), "0.85");
    assert_eq!(format!("{:?}", conf(0.85)), "Confidence(0.85)");
    assert_eq!(Confidence::try_from(0.5).unwrap(), conf(0.5));
    assert!(Confidence::try_from(2.0).is_err());

    // Ulid/id：Display、Debug、FromStr、nil、from_parts。
    let ulid = Ulid::from_parts(1_700_000_000_000, 7);
    assert_eq!(ulid.timestamp_ms(), 1_700_000_000_000);
    assert_eq!(ulid.random(), 7);
    assert_eq!(Ulid::NIL.to_string(), "00000000000000000000000000");
    assert_eq!(format!("{ulid:?}").len(), 26 + 8);
    assert_eq!(Ulid::from_str(&ulid.to_string()).unwrap(), ulid);
    let id = StudentId::from_ulid(ulid);
    assert_eq!(id.to_string(), ulid.to_string());
    assert!(format!("{id:?}").contains("StudentId("));

    // 哈希：Display、Debug、FromStr。
    let asset = AssetId::of(b"payload");
    assert_eq!(asset.to_string(), asset.to_hex());
    assert!(format!("{asset:?}").starts_with("AssetId("));
    assert_eq!(AssetId::from_str(&asset.to_string()).unwrap(), asset);
    let hash = ContentHash::of(b"payload");
    assert!(format!("{hash:?}").starts_with("ContentHash("));
    assert_eq!(ContentHash::from_str(&hash.to_string()).unwrap(), hash);
    let sha = Sha256Hex::of(b"payload");
    assert!(format!("{sha:?}").starts_with("Sha256Hex("));
    assert_eq!(Sha256Hex::from_str(&sha.to_string()).unwrap(), sha);

    // Decimal：Display、Debug、FromStr、负号分支。
    assert_eq!(dec("-2.50").to_string(), "-2.5");
    assert_eq!(format!("{:?}", dec("2.5")), "Decimal(2.5)");
    assert_eq!(Decimal::from_str("2.5").unwrap(), dec("2.5"));
    assert!(dec("-3") < dec("-2"));
    assert!(dec("-2") < dec("0"));
    assert!(dec("10") > dec("9.99"));
    assert_eq!(Decimal::from_mantissa_scale(0, 5).unwrap(), Decimal::ZERO);
    assert!(Decimal::from_mantissa_scale(1, 19).is_err());
    assert!(Decimal::from_integer(5).is_ok());

    // 字符串 id / 枚举值对象：Display、Debug、FromStr、serde。
    assert_eq!(SubjectId::math().to_string(), "math");
    assert_eq!(format!("{:?}", SubjectId::math()), "SubjectId(\"math\")");
    assert!(SubjectId::from_str(" ").is_err());
    assert_eq!(Rotation::CW_180.to_string(), "180");
    assert_eq!(format!("{:?}", Rotation::CW_90), "Rotation(90)");
    assert_eq!(ExifOrientation::new(2).unwrap().to_string(), "2");
    assert_eq!(
        format!("{:?}", ExifOrientation::new(2).unwrap()),
        "ExifOrientation(2)"
    );
    assert_eq!(BackendId::openai_compat().to_string(), "openai_compat");
    assert_eq!(BackendId::fixture().to_string(), "fixture");
    assert!(BackendId::new("custom").is_ok());
}

#[test]
fn error_variants_expose_field_names() {
    let cases: Vec<(DomainError, &str)> = vec![
        (ScoreError::Overflow.into(), "score"),
        (ConfidenceError::NaN.into(), "confidence"),
        (
            IdError::InvalidUlid {
                input: "x".to_owned(),
                reason: "y".to_owned(),
            }
            .into(),
            "id",
        ),
        (
            HashError::InvalidHex {
                kind: "ContentHash",
                reason: "z",
            }
            .into(),
            "hash",
        ),
        (
            DecimalError::Format {
                input: "x".to_owned(),
            }
            .into(),
            "decimal",
        ),
        (
            DomainError::TotalScoreMismatch {
                declared: milli(1),
                computed: milli(2),
            },
            "total_score",
        ),
        (
            DomainError::DuplicateQuestionLabel {
                label: "1".to_owned(),
            },
            "label",
        ),
        (
            DomainError::NonContiguousOrdinal {
                expected: 1,
                found: 2,
            },
            "ordinal",
        ),
        (
            DomainError::RubricTotalMismatch {
                declared: milli(1),
                computed: milli(2),
            },
            "rubric.total",
        ),
        (DomainError::AllOrNothingItemCount { count: 2 }, "items"),
        (DomainError::EmptyRubric, "items"),
        (
            DomainError::RubricDependencyMissing {
                item: RubricItemId::from_ulid(u(1)),
                missing: RubricItemId::from_ulid(u(2)),
            },
            "items.depends_on",
        ),
        (
            DomainError::RubricDependencyCycle {
                items: vec![RubricItemId::from_ulid(u(1))],
            },
            "items.depends_on",
        ),
        (
            DomainError::DraftDependencyOutOfRange { index: 9, len: 1 },
            "items.depends_on",
        ),
        (
            DomainError::DraftDependencyCycle { items: vec![1] },
            "items.depends_on",
        ),
        (
            DomainError::ScoringModeUnsupported {
                mode: ScoringMode::PartialChoice,
                kind: QuestionKind::SingleChoice,
            },
            "mode",
        ),
        (DomainError::PartialChoiceMissing, "partial_choice"),
        (DomainError::PartialChoiceNotAllowed, "partial_choice"),
        (DomainError::PartialChoiceTierOrder, "partial_choice"),
        (
            DomainError::PartialChoiceWrongSelectionZero,
            "partial_choice",
        ),
        (
            DomainError::PartialChoiceSourceNoteMissing,
            "partial_choice",
        ),
        (
            DomainError::GranularityViolation {
                value: milli(1500),
                granularity: milli(1000),
            },
            "granularity",
        ),
        (
            DomainError::AwardedOutOfRange {
                awarded: milli(3),
                max_score: milli(2),
            },
            "awarded",
        ),
        (
            DomainError::JudgeScoreInconsistent {
                judgement: "match",
                awarded: milli(1),
                max_score: milli(2),
            },
            "judgement",
        ),
        (DomainError::MissingEvidence, "evidence"),
        (DomainError::MissingModelCallEvidence, "evidence"),
        (DomainError::CloudConsentMissing, "consent_id"),
        (DomainError::SampledInStandardMode, "sampled"),
        (DomainError::ReviewTimestampMissing, "reviewed_at"),
        (DomainError::InvalidRevision { revision: 0 }, "revision"),
        (DomainError::AutoAcceptNotAllowed, "status"),
        (
            DomainError::TemplateVersionMismatch {
                template_version: 1,
                key_version: 2,
            },
            "version",
        ),
        (DomainError::TemplatePaperMismatch, "paper_id"),
        (
            DomainError::DraftNotConfirmable {
                status: DraftStatus::Draft,
            },
            "status",
        ),
        (DomainError::DraftHasErrors { count: 1 }, "validation"),
        (DomainError::MissingModelCallId, "model_call_id"),
        (DomainError::MissingConfirmationTimestamp, "confirmed_at"),
        (DomainError::UnexpectedConfirmationTimestamp, "confirmed_at"),
        (DomainError::MissingRubricForAnswerKey, "rubric"),
        (DomainError::RubricHasErrors { count: 2 }, "validation"),
    ];
    for (error, field) in cases {
        assert_eq!(error.field_name(), field, "{error}");
        assert_ne!(error.to_string().len(), 0);
    }
}

// ---------------------------------------------------------------------------
// 契约套件：违规码、报告格式、链执行边界
// ---------------------------------------------------------------------------

#[test]
fn contract_violation_codes_and_report() {
    let violations = vec![
        ContractViolation::CapabilitiesInvalid {
            reason: "空的题型".to_owned(),
        },
        ContractViolation::DeclaredKindUnsupported {
            kind: QuestionKind::Proof,
        },
        ContractViolation::NormalizeFailed {
            input: "x".to_owned(),
            reason: "y".to_owned(),
        },
        ContractViolation::NormalizeNotIdempotent,
        ContractViolation::EmptyComparatorChain,
        ContractViolation::ChainOrderViolation {
            expected: vec!["a".to_owned()],
            actual: vec!["b".to_owned()],
        },
        ContractViolation::UndeclaredComparator {
            comparator: "x".to_owned(),
        },
        ContractViolation::DuplicateComparator {
            comparator: "x".to_owned(),
        },
        ContractViolation::ParseFailureJudgedNotEqual {
            comparator: "x".to_owned(),
        },
        ContractViolation::CorrectAnswerUndecided,
        ContractViolation::CorrectAnswerRejected {
            verdict: "not_equal".to_owned(),
        },
        ContractViolation::WrongAnswerAccepted {
            verdict: "equal".to_owned(),
        },
        ContractViolation::StaticValidationRejectsCase {
            message: "错误".to_owned(),
        },
        ContractViolation::EmptyIssueMessage,
        ContractViolation::EmptyJudgeProfile,
        ContractViolation::EmptyDisplayName,
        ContractViolation::StepScoringCapabilityMismatch,
    ];
    let expected_codes = [
        "capabilities_invalid",
        "declared_kind_unsupported",
        "normalize_failed",
        "normalize_not_idempotent",
        "empty_comparator_chain",
        "chain_order_violation",
        "undeclared_comparator",
        "duplicate_comparator",
        "parse_failure_judged_not_equal",
        "correct_answer_undecided",
        "correct_answer_rejected",
        "wrong_answer_accepted",
        "static_validation_rejects_case",
        "empty_issue_message",
        "empty_judge_profile",
        "empty_display_name",
        "step_scoring_capability_mismatch",
    ];
    for (violation, code) in violations.iter().zip(expected_codes) {
        assert_eq!(violation.code(), code);
        assert_ne!(violation.to_string().len(), 0);
    }
    let report = eg_domain::contract::format_violations(&violations);
    assert_eq!(report.lines().count(), violations.len());
    assert!(eg_domain::contract::contains_code(
        &violations,
        "empty_judge_profile"
    ));
}

struct AlwaysUndecidable;
impl Comparator for AlwaysUndecidable {
    fn id(&self) -> ComparatorId {
        ComparatorId::new("always_undecidable").unwrap()
    }
    fn compare(&self, _expected: &AnswerValue, _answer: &NormalizedAnswer) -> ComparatorVerdict {
        ComparatorVerdict::Undecidable {
            reason: "无法判定".to_owned(),
        }
    }
}

#[test]
fn comparator_chain_boundaries() {
    let expected = AnswerValue::Text("2".to_owned());
    let answer = NormalizedAnswer {
        text: "2".to_owned(),
        latex: None,
        value: None,
        steps: Vec::new(),
    };
    let empty: Vec<Box<dyn Comparator>> = Vec::new();
    let outcome = run_comparator_chain(&empty, &expected, &answer);
    assert!(outcome.verdict.is_undecidable());
    assert_eq!(outcome.attempts.len(), 0);

    let chain: Vec<Box<dyn Comparator>> = vec![Box::new(AlwaysUndecidable)];
    let outcome = run_comparator_chain(&chain, &expected, &answer);
    assert!(outcome.verdict.is_undecidable());
    assert_eq!(outcome.attempts.len(), 1);
    assert!(outcome.attempts[0].1.is_undecidable());
}

#[test]
fn error_issue_gate_helper() {
    use eg_domain::contract::ensure_question_gradable;
    assert!(ensure_question_gradable(&[]).is_ok());
    assert!(ensure_question_gradable(&[RubricIssue::error("坏规则")]).is_err());
    assert_eq!(error_issue_count(&[RubricIssue::error("坏规则")]), 1);
}

// ---------------------------------------------------------------------------
// 试卷/题目/答案键分支
// ---------------------------------------------------------------------------

#[test]
fn paper_and_question_edge_branches() {
    // 题目 label 为空。
    let mut q = question();
    q.label = "  ".to_owned();
    assert!(q.validate().is_err());

    // 序号为 0。
    let mut q = question();
    q.ordinal = 0;
    assert!(matches!(
        q.validate(),
        Err(DomainError::NonContiguousOrdinal { found: 0, .. })
    ));

    // 「仅评分标准」但无 Rubric。
    let mut q = question();
    q.key = AnswerKey::RubricOnly;
    assert!(matches!(
        q.validate(),
        Err(DomainError::MissingRubricForAnswerKey)
    ));

    // Rubric.total 与题目满分不一致。
    let mut q = question();
    q.key = AnswerKey::RubricOnly;
    q.rubric = Some(additive_rubric());
    q.max_score = milli(3000);
    assert!(matches!(
        q.validate(),
        Err(DomainError::RubricTotalMismatch { .. })
    ));

    // 具体答案键内容非法。
    let mut q = question();
    q.key = AnswerKey::Concrete(ConcreteKey {
        primary: AnswerValue::Text(String::new()),
        alternatives: Vec::new(),
        comparators: Vec::new(),
    });
    assert!(q.validate().is_err());

    // 省略备注为空字符串。
    assert!(
        AnswerKey::ExplicitlyOmitted {
            note: Some(" ".to_owned())
        }
        .validate()
        .is_err()
    );

    // 单位为空字符串、Structured 合法。
    assert!(
        AnswerValue::Number {
            value: dec("1"),
            unit: Some(String::new()),
            tolerance: None
        }
        .validate()
        .is_err()
    );
    assert!(
        AnswerValue::Structured(serde_json::json!({"k": 1}))
            .validate()
            .is_ok()
    );

    // 模板 paper_id 不一致。
    let template = PaperTemplate {
        paper_id: PaperId::from_ulid(u(999)),
        version: 1,
        page_count: 1,
        regions: Vec::new(),
    };
    let paper = Paper {
        id: PaperId::from_ulid(u(11)),
        classroom_id: ClassroomId::from_ulid(u(12)),
        title: "试卷".to_owned(),
        subject: SubjectId::math(),
        total_score: milli(2500),
        questions: vec![q],
        key_version: 1,
        rubric_version: 1,
        created_at: ts(),
    };
    assert!(matches!(
        template.ensure_auto_align_allowed(&paper),
        Err(DomainError::TemplatePaperMismatch)
    ));

    // 试卷标题为空。
    let mut bad_paper = paper.clone();
    bad_paper.title = String::new();
    assert!(bad_paper.validate().is_err());

    // 题目 paper_id 与试卷不一致。
    let mut bad_paper = paper.clone();
    bad_paper.questions[0].paper_id = PaperId::from_ulid(u(777));
    assert!(bad_paper.validate().is_err());

    // ContentRef 序列化标签。
    assert!(
        serde_json::to_string(&ContentRef::Text("题干".to_owned()))
            .unwrap()
            .contains("text")
    );
}

// ---------------------------------------------------------------------------
// 评分标准/草稿分支
// ---------------------------------------------------------------------------

fn sample_draft() -> RubricDraft {
    RubricDraft {
        id: RubricId::from_ulid(u(71)),
        question_id: QuestionId::from_ulid(u(21)),
        status: DraftStatus::Draft,
        items: vec![RubricDraftItem {
            criterion: "写出公式".to_owned(),
            points: milli(1000),
            required: true,
            depends_on: Vec::new(),
        }],
        raw_text: String::new(),
        origin: DraftOrigin::HandwrittenTransliteration,
        model_call_id: None,
        validation: Vec::new(),
        created_at: ts(),
        confirmed_at: None,
    }
}

#[test]
fn rubric_validation_edge_branches() {
    // 版本为 0。
    let mut rubric = additive_rubric();
    rubric.version = 0;
    assert!(rubric.validate(QuestionKind::Calculation).is_err());

    // 评分项判据为空。
    let mut rubric = additive_rubric();
    rubric.items[0].criterion = " ".to_owned();
    assert!(rubric.validate(QuestionKind::Calculation).is_err());

    // 评分项 id 重复。
    let mut rubric = additive_rubric();
    rubric.items[1].id = rubric.items[0].id;
    assert!(rubric.validate(QuestionKind::Calculation).is_err());

    // 自依赖直接报环。
    let mut rubric = additive_rubric();
    rubric.items[0].depends_on = vec![rubric.items[0].id];
    assert!(matches!(
        rubric.validate(QuestionKind::Calculation),
        Err(DomainError::RubricDependencyCycle { .. })
    ));

    // 全对模式携带档位规则。
    let mut rubric = additive_rubric();
    rubric.mode = ScoringMode::AllOrNothing;
    rubric.items.truncate(1);
    rubric.total = milli(1000);
    rubric.partial_choice = Some(PartialChoiceRule {
        correct_option_count: 2,
        tiers: vec![PartialChoiceTier {
            selected_correct: 1,
            points: milli(500),
            label: None,
        }],
        wrong_selection_zero: true,
        source: PartialChoiceSource::TeacherCustom,
        source_note: None,
        total_milli: milli(1000),
    });
    assert!(matches!(
        rubric.validate(QuestionKind::Calculation),
        Err(DomainError::PartialChoiceNotAllowed)
    ));

    // 部分给分：档位乱序、档位 0 分、档位说明空串。
    let mut rubric = additive_rubric();
    rubric.mode = ScoringMode::PartialChoice;
    rubric.items = vec![rubric_item(1, "全对", 6000)];
    rubric.total = milli(6000);
    let rule = PartialChoiceRule {
        correct_option_count: 3,
        tiers: vec![
            PartialChoiceTier {
                selected_correct: 1,
                points: milli(3000),
                label: Some(String::new()),
            },
            PartialChoiceTier {
                selected_correct: 2,
                points: milli(2000),
                label: None,
            },
        ],
        wrong_selection_zero: true,
        source: PartialChoiceSource::TemplateData,
        source_note: None,
        total_milli: milli(6000),
    };
    rubric.partial_choice = Some(rule);
    assert!(rubric.validate(QuestionKind::MultipleChoice).is_err());

    // 正确选项数不合法（4）。
    let mut rubric = additive_rubric();
    rubric.mode = ScoringMode::PartialChoice;
    rubric.items = vec![rubric_item(1, "全对", 6000)];
    rubric.total = milli(6000);
    rubric.partial_choice = Some(PartialChoiceRule {
        correct_option_count: 4,
        tiers: Vec::new(),
        wrong_selection_zero: true,
        source: PartialChoiceSource::Official,
        source_note: Some("细则".to_owned()),
        total_milli: milli(6000),
    });
    assert!(matches!(
        rubric.validate(QuestionKind::MultipleChoice),
        Err(DomainError::PartialChoiceOptionCount { count: 4 })
    ));

    // 档位规则满分与 Rubric.total 不一致。
    let mut rubric = additive_rubric();
    rubric.mode = ScoringMode::PartialChoice;
    rubric.items = vec![rubric_item(1, "全对", 6000)];
    rubric.total = milli(6000);
    rubric.partial_choice = Some(PartialChoiceRule {
        correct_option_count: 2,
        tiers: vec![PartialChoiceTier {
            selected_correct: 1,
            points: milli(3000),
            label: None,
        }],
        wrong_selection_zero: true,
        source: PartialChoiceSource::Official,
        source_note: Some("细则".to_owned()),
        total_milli: milli(5000),
    });
    assert!(matches!(
        rubric.validate(QuestionKind::MultipleChoice),
        Err(DomainError::PartialChoiceTotalMismatch { .. })
    ));

    // 非部分给分模式的分值粒度校验。
    let rubric = additive_rubric();
    assert!(matches!(
        rubric.validate_with_granularity(QuestionKind::Calculation, Some(milli(2000))),
        Err(DomainError::GranularityViolation { .. })
    ));
    assert!(!rubric.has_error_issue(QuestionKind::Calculation));
    assert_eq!(
        rubric.has_error_issue(QuestionKind::Calculation),
        rubric.validate(QuestionKind::Calculation).is_err()
    );

    // 模板来源版本 0。
    let mut rubric = additive_rubric();
    rubric.origin = RubricOrigin::StandardTemplate {
        template_id: TemplateId::new("t").unwrap(),
        template_version: 0,
        confirmed_by: LOCAL_TEACHER.to_owned(),
        confirmed_at: ts(),
    };
    assert!(rubric.validate(QuestionKind::Calculation).is_err());

    // 规则原文 serde。
    assert!(
        serde_json::to_string(&RuleText {
            question_id: QuestionId::from_ulid(u(1)),
            text: "每步 1 分".to_owned(),
            source: RuleTextSource::Template,
        })
        .unwrap()
        .contains("template")
    );
}

#[test]
fn draft_validation_edge_branches() {
    // 判据为空、分值为 0、状态与确认时间不一致。
    let mut draft = sample_draft();
    draft.items[0].criterion = " ".to_owned();
    assert!(draft.validate_items().is_err());

    let mut draft = sample_draft();
    draft.items[0].points = MilliScore::ZERO;
    assert!(draft.validate_items().is_err());

    let mut draft = sample_draft();
    draft.status = DraftStatus::Rejected;
    draft.confirmed_at = Some(ts());
    assert!(matches!(
        draft.validate(),
        Err(DomainError::UnexpectedConfirmationTimestamp)
    ));

    // 待确认但没有任何评分项。
    let mut draft = sample_draft();
    draft.status = DraftStatus::PendingConfirmation;
    draft.items.clear();
    assert!(matches!(
        draft.ensure_confirmable(),
        Err(DomainError::EmptyRubric)
    ));

    // 拒绝状态不可确认。
    let mut draft = sample_draft();
    draft.status = DraftStatus::Rejected;
    assert!(matches!(
        draft.ensure_confirmable(),
        Err(DomainError::DraftNotConfirmable { .. })
    ));

    // 已确认草稿缺确认时间。
    let mut draft = sample_draft();
    draft.status = DraftStatus::Confirmed;
    assert!(matches!(
        draft.validate(),
        Err(DomainError::MissingConfirmationTimestamp)
    ));

    // 模型整理草稿缺 model_call_id。
    let mut draft = sample_draft();
    draft.origin = DraftOrigin::ModelStructured;
    assert!(matches!(
        draft.validate(),
        Err(DomainError::MissingModelCallId)
    ));
    assert_eq!(draft.error_issue_count(), 0);

    // 草稿来源标签。
    assert_eq!(
        serde_json::to_string(&DraftStatus::Confirmed).unwrap(),
        "\"confirmed\""
    );
    assert_eq!(
        serde_json::to_string(&DraftOrigin::TemplateApplied).unwrap(),
        "\"template_applied\""
    );
}

// ---------------------------------------------------------------------------
// 页面/提交/识别分支
// ---------------------------------------------------------------------------

#[test]
fn page_and_submission_edge_branches() {
    let page = Page {
        id: PageId::from_ulid(u(61)),
        submission_id: SubmissionId::from_ulid(u(52)),
        page_no: 1,
        asset: AssetId::of(b"original"),
        width: 100,
        height: 100,
        rotation_applied: Rotation::NONE,
        exif_orientation: ExifOrientation::NONE,
        quality: PageQuality {
            blur_var: 10.0,
            exposure: 0.5,
            skew_deg: 0.0,
            resolution_ok: false,
            verdict: QualityVerdict::Rejected {
                reason: "分辨率不足".to_owned(),
            },
        },
        derived: Vec::new(),
    };
    assert!(page.validate().is_ok());

    let submission = Submission {
        id: SubmissionId::from_ulid(u(52)),
        paper_id: PaperId::from_ulid(u(11)),
        classroom_id: ClassroomId::from_ulid(u(12)),
        student_id: None,
        source_file_name: "a.jpg".to_owned(),
        source_stem: "a".to_owned(),
        pages: vec![page.clone()],
        status: SubmissionStatus::Failed {
            stage: Stage::Capture,
            message: "损坏文件".to_owned(),
        },
        created_at: ts(),
    };
    assert!(submission.validate().is_ok());

    // 页面 submission_id 不一致。
    let mut bad = submission.clone();
    bad.pages[0].submission_id = SubmissionId::from_ulid(u(999));
    assert!(bad.validate().is_err());

    // 质量字段非有限、模糊度负值。
    let mut quality = page.quality.clone();
    quality.exposure = f64::NAN;
    assert!(quality.validate().is_err());
    let mut quality = page.quality.clone();
    quality.blur_var = -1.0;
    assert!(quality.validate().is_err());

    // 区域坐标非法。
    let segment = Segment {
        id: SegmentId::from_ulid(u(62)),
        page_id: PageId::from_ulid(u(61)),
        question_id: None,
        bbox: BBox::new(0.0, 0.0, 0.5, 0.5).unwrap(),
        crop: AssetId::of(b"crop"),
        method: SegmentMethod::Manual,
    };
    assert!(segment.validate().is_ok());

    // 枚举标签与 Debug 分支。
    assert_eq!(serde_json::to_string(&Stage::Export).unwrap(), "\"export\"");
    assert_eq!(
        serde_json::to_string(&SegmentMethod::Manual).unwrap(),
        "\"manual\""
    );
    assert!(
        serde_json::to_string(&SubmissionStatus::Failed {
            stage: Stage::Align,
            message: "缺页".to_owned()
        })
        .unwrap()
        .contains("failed")
    );
}

#[test]
fn recognition_edge_branches() {
    let engine = EngineStamp {
        role: ModelRole::Ocr,
        backend: BackendId::local_server(),
        model_id: "ocr".to_owned(),
        model_version: "1".to_owned(),
        prompt_hash: None,
        params_hash: Sha256Hex::of(b"p"),
    };
    assert!(engine.validate().is_ok());

    // 块非法时错误信息带块序号。
    let recognition = Recognition {
        id: RecognitionId::from_ulid(u(41)),
        page_id: PageId::from_ulid(u(61)),
        segment_id: None,
        blocks: vec![Block {
            kind: BlockKind::Formula,
            bbox: bbox(),
            text: String::new(),
            latex: None,
            confidence: conf(0.5),
        }],
        text: String::new(),
        engine,
        confidence: conf(0.5),
        created_at: ts(),
    };
    let error = recognition.validate().unwrap_err();
    assert!(error.to_string().contains("第 1 个识别块"));
    assert_eq!(recognition.formula_block_count(), 1);

    // 引擎 model_version 为空。
    let mut recognition = recognition;
    recognition.blocks.clear();
    recognition.engine.model_version = String::new();
    assert!(recognition.validate().is_err());

    // 枚举标签。
    assert_eq!(
        serde_json::to_string(&BlockKind::Handwriting).unwrap(),
        "\"handwriting\""
    );
    assert_eq!(
        serde_json::to_string(&ModelRole::Router).unwrap(),
        "\"router\""
    );
}

// ---------------------------------------------------------------------------
// 判分/证据/学科分支
// ---------------------------------------------------------------------------

fn engine_stamp() -> EngineStamp {
    EngineStamp {
        role: ModelRole::Judge,
        backend: BackendId::local_server(),
        model_id: "judge".to_owned(),
        model_version: "1".to_owned(),
        prompt_hash: None,
        params_hash: Sha256Hex::of(b"p"),
    }
}

fn match_judgement() -> Judgement {
    Judgement::Match {
        score: milli(2500),
        reason: "一致".to_owned(),
        evidence: vec![EvidenceRef::ModelCall(ModelCallId::from_ulid(u(43)))],
    }
}

fn grade_record() -> GradeRecord {
    GradeRecord {
        id: GradeId::from_ulid(u(51)),
        submission_id: SubmissionId::from_ulid(u(52)),
        question_id: QuestionId::from_ulid(u(21)),
        revision: 1,
        mode: GradeMode::Standard,
        route: RouteDecision::RuleOnly,
        route_trace: RouteTrace {
            rule_id: "R0".to_owned(),
            inputs_hash: Sha256Hex::of(b"i"),
            classifier_call: None,
            decided_at: ts(),
        },
        judgement: match_judgement(),
        awarded: milli(2500),
        max_score: milli(2500),
        confidence: conf(0.9),
        status: GradeStatus::AutoAccepted,
        evidence: vec![EvidenceRef::ModelCall(ModelCallId::from_ulid(u(43)))],
        engines: vec![engine_stamp()],
        created_at: ts(),
        reviewed_at: None,
        review_note: None,
        sampled: false,
    }
}

#[test]
fn grade_record_edge_branches() {
    // 满分 0、review_note 空串、路由轨迹空 rule_id。
    let mut record = grade_record();
    record.max_score = MilliScore::ZERO;
    record.awarded = MilliScore::ZERO;
    record.judgement = Judgement::Ungradable {
        reason: UngradableReason::NotAnswered,
    };
    assert!(record.validate().is_err());

    let mut record = grade_record();
    record.review_note = Some(" ".to_owned());
    assert!(record.validate().is_err());

    let mut record = grade_record();
    record.route_trace.rule_id = " ".to_owned();
    assert!(record.validate().is_err());

    // 已复核状态与排除状态。
    let mut record = grade_record();
    record.status = GradeStatus::ManuallyReviewed;
    record.reviewed_at = Some(ts());
    assert!(record.validate().is_ok());
    record.status = GradeStatus::Excluded;
    assert!(record.validate().is_ok());

    // 不可判定 + 待复核通过；仅人工路由不得自动接受。
    let mut record = grade_record();
    record.judgement = Judgement::Ungradable {
        reason: UngradableReason::PolicyBlocked,
    };
    record.awarded = MilliScore::ZERO;
    record.status = GradeStatus::NeedsReview;
    assert!(record.validate().is_ok());

    let mut record = grade_record();
    record.route = RouteDecision::Manual(ManualReason::LowConfidence);
    assert!(matches!(
        record.validate(),
        Err(DomainError::AutoAcceptNotAllowed)
    ));

    // 快速模式 + 抽检合法；标准模式 + 抽检非法（已覆盖），这里验证 Fast。
    let mut record = grade_record();
    record.mode = GradeMode::Fast;
    record.sampled = true;
    assert!(record.validate().is_ok());

    // 提交级排除。
    let exclusion = SubmissionExclusion {
        submission_id: SubmissionId::from_ulid(u(52)),
        reason: ExcludeReason::Other,
        note: String::new(),
        at: ts(),
    };
    assert!(exclusion.validate().is_ok());
    assert_eq!(
        serde_json::to_string(&ExcludeReason::Duplicate).unwrap(),
        "\"duplicate\""
    );

    // JudgedUnit 两种标签。
    let partial = Judgement::Partial {
        score: milli(1000),
        satisfied: vec![
            JudgedUnit::RubricItem(RubricItemId::from_ulid(u(1))),
            JudgedUnit::Choice("A".to_owned()),
        ],
        missing: vec![JudgedUnit::Choice("B".to_owned())],
        reason: "少选".to_owned(),
    };
    assert!(partial.validate_against(milli(2500)).is_ok());
    let json = serde_json::to_string(&partial).unwrap();
    assert!(json.contains("\"rubric_item\""));
    assert!(json.contains("\"choice\""));
}

#[test]
fn evidence_and_subject_edge_branches() {
    // 比较轨迹：等价说明为空、无法判定原因为空。
    let trace = ComparatorTrace {
        id: ComparatorTraceId::from_ulid(u(42)),
        comparator: ComparatorId::new("text_exact").unwrap(),
        verdict: ComparatorVerdict::Equivalent {
            note: " ".to_owned(),
        },
        detail: serde_json::json!({}),
    };
    assert!(trace.validate().is_err());
    let trace = ComparatorTrace {
        verdict: ComparatorVerdict::Undecidable {
            reason: String::new(),
        },
        ..trace
    };
    assert!(trace.validate().is_err());

    // 模型调用：角色不一致、云端缺授权、输出为空。
    let call = ModelCall {
        id: ModelCallId::from_ulid(u(43)),
        role: ModelRole::Judge,
        engine: engine_stamp(),
        task_id: TaskId::from_ulid(u(44)),
        consent_id: None,
        input_hash: Sha256Hex::of(b"i"),
        output: serde_json::json!({"ok": true}),
        latency_ms: 1,
        cost_micros: 0,
        created_at: ts(),
    };
    assert!(call.validate().is_ok());
    assert!(!call.is_cloud());
    let mut mismatch = call.clone();
    mismatch.role = ModelRole::Ocr;
    assert!(mismatch.validate().is_err());
    let mut cloud = call.clone();
    cloud.engine.backend = BackendId::openai_compat();
    assert!(matches!(
        cloud.validate(),
        Err(DomainError::CloudConsentMissing)
    ));
    cloud.consent_id = Some(ConsentId::from_ulid(u(45)));
    assert!(cloud.validate().is_ok());
    let mut null_output = call;
    null_output.output = serde_json::Value::Null;
    assert!(null_output.validate().is_err());

    // 能力声明：比较器重复、提示词版本为空。
    let duplicate = SubjectCapabilities {
        kinds: vec![QuestionKind::NumericOnly],
        comparators: vec![
            ComparatorId::new("a").unwrap(),
            ComparatorId::new("a").unwrap(),
        ],
        prompt_version: "v1".to_owned(),
        supports_step_scoring: false,
    };
    assert!(duplicate.validate().is_err());
    let empty_version = SubjectCapabilities {
        comparators: vec![ComparatorId::new("a").unwrap()],
        prompt_version: " ".to_owned(),
        ..duplicate
    };
    assert!(empty_version.validate().is_err());

    // AnswerPart 坐标非法。
    let part = AnswerPart {
        page_id: PageId::from_ulid(u(1)),
        segment_id: SegmentId::from_ulid(u(2)),
        recognition_id: RecognitionId::from_ulid(u(3)),
        bbox: bbox(),
        text: "x".to_owned(),
    };
    assert!(part.validate().is_ok());
    let mut bad_part = part.clone();
    bad_part.bbox.x = f64::NAN;
    assert!(bad_part.validate().is_err());

    // StudentAnswer 主段 id 不在 parts 中。
    let answer = StudentAnswer {
        submission_id: SubmissionId::from_ulid(u(52)),
        question_id: QuestionId::from_ulid(u(21)),
        recognition_id: Some(RecognitionId::from_ulid(u(3))),
        segment_id: Some(SegmentId::from_ulid(u(99))),
        raw: RawAnswer {
            text: "x".to_owned(),
            latex: None,
            blocks: Vec::new(),
            source: AnswerSource::ElectronicText,
        },
        parts: vec![part],
        recognition_confidence: conf(1.0),
    };
    assert!(answer.validate().is_err());
    let mut fixed = answer;
    fixed.segment_id = Some(SegmentId::from_ulid(u(2)));
    assert!(fixed.validate().is_ok());

    // NormalizeError 序列化（快照之外再验证标签）。
    let json = serde_json::to_string(&NormalizeError::Unsupported {
        reason: "超范围".to_owned(),
    })
    .unwrap();
    assert!(json.contains("unsupported"));
}

#[test]
fn schema_versions_and_clock_trait() {
    struct Fixed(Timestamp);
    impl Clock for Fixed {
        fn now(&self) -> Timestamp {
            self.0
        }
    }

    assert_eq!(DOMAIN_SCHEMA_VERSION, 1);
    assert_eq!(Paper::SCHEMA_VERSION, 1);
    assert_eq!(Question::SCHEMA_VERSION, 1);
    assert_eq!(RubricDraft::SCHEMA_VERSION, 1);
    assert_eq!(Submission::SCHEMA_VERSION, 1);
    assert_eq!(Page::SCHEMA_VERSION, 1);
    assert_eq!(Segment::SCHEMA_VERSION, 1);
    assert_eq!(StudentAnswer::SCHEMA_VERSION, 1);
    assert_eq!(ComparatorTrace::SCHEMA_VERSION, 1);
    assert_eq!(SubmissionExclusion::SCHEMA_VERSION, 1);
    assert_eq!(Classroom::SCHEMA_VERSION, 1);
    assert_eq!(Student::SCHEMA_VERSION, 1);
    assert_eq!(PaperTemplate::SCHEMA_VERSION, 1);
    assert_eq!(RuleText::SCHEMA_VERSION, 1);
    assert_eq!(PageQuality::SCHEMA_VERSION, 1);
    assert_eq!(AnswerKey::SCHEMA_VERSION, 1);

    // Clock 端口可用（测试注入固定时钟）。
    let clock = Fixed(ts());
    assert_eq!(clock.now(), ts());
    assert_eq!(
        eg_domain::time::format_rfc3339(ts()),
        "2026-11-02T00:00:00Z"
    );
}
