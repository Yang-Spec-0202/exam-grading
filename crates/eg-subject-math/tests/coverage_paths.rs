//! 覆盖路径测试：系统性触达 AST 渲染、解析文法分支、规范化与比较器的边角路径。
//!
//! 目的：把 `eg-subject-math` 行覆盖率抬到 `SPEC.md` §8 的核心 crate 行 ≥80% 基线，
//! 同时作为「新增文法/节点必须可渲染、可重新解析」的回归保障。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines
)]

use eg_domain::{
    AnswerSource, AnswerValue, Comparator, ComparatorVerdict, Decimal, NormalizedAnswer, RawAnswer,
    Tolerance,
};
use eg_subject_math::ast::{Expr, Interval, IntervalEnd, RelOp, Unit};
use eg_subject_math::canonical::{
    EvalError, SolveError, canonicalize, compare_constants, equation_solutions, evaluate,
    interval_is_point, polynomial, polynomial_equality, relation_to_interval, same_dimension,
    value_set,
};
use eg_subject_math::comparators::{
    ExpressionCanonicalComparator, IntervalComparator, NumericAbsoluteComparator,
    NumericUnitComparator, SolutionSetComparator, comparator_by_id, comparator_chain_for,
    default_chain_ids, exact_value_set, point_interval_value,
};
use eg_subject_math::{NumericAbsoluteComparator as Numeric, Rational, normalize_raw, parse_expr};

fn raw(text: &str) -> RawAnswer {
    RawAnswer {
        text: text.to_owned(),
        latex: None,
        blocks: Vec::new(),
        source: AnswerSource::Recognition,
    }
}

fn answer(latex: &str) -> NormalizedAnswer {
    NormalizedAnswer {
        text: latex.to_owned(),
        latex: Some(latex.to_owned()),
        value: None,
        steps: Vec::new(),
    }
}

fn number(value: &str) -> AnswerValue {
    AnswerValue::Number {
        value: Decimal::parse(value).unwrap(),
        unit: None,
        tolerance: None,
    }
}

#[test]
fn ast_renderers_cover_all_nodes() {
    let variable = || Expr::Variable("x".to_owned());
    let one = || Expr::Number(Rational::from_i64(1));
    let nodes: Vec<Expr> = vec![
        Expr::Boolean(true),
        Expr::Boolean(false),
        Expr::Option("A".to_owned()),
        Expr::Function {
            name: "sin".to_owned(),
            args: vec![variable()],
        },
        Expr::Vector(vec![one(), variable()]),
        Expr::Matrix(vec![vec![one()], vec![variable()]]),
        Expr::Set(Vec::new()),
        Expr::Set(vec![one(), variable()]),
        Expr::Solutions(vec![variable(), one()]),
        Expr::Interval(Interval::new(
            IntervalEnd::NegInfinity,
            IntervalEnd::PosInfinity,
        )),
        Expr::Interval(Interval::new(
            IntervalEnd::Value {
                expr: Box::new(one()),
                closed: true,
            },
            IntervalEnd::Value {
                expr: Box::new(variable()),
                closed: false,
            },
        )),
        Expr::Quantity {
            value: Box::new(one()),
            unit: Unit::parse("cm").unwrap(),
        },
        Expr::Quantity {
            value: Box::new(one()),
            unit: Unit::count("个"),
        },
        Expr::Absolute(Box::new(variable())),
        Expr::Radical {
            degree: Some(Box::new(Expr::Number(Rational::from_i64(3)))),
            radicand: Box::new(variable()),
        },
        Expr::Fraction(Box::new(one()), Box::new(variable())),
        Expr::Power(Box::new(variable()), Box::new(one())),
        Expr::Product(vec![one(), variable()]),
        Expr::Sum(vec![one(), variable()]),
        Expr::Relation {
            op: RelOp::Ne,
            lhs: Box::new(variable()),
            rhs: Box::new(one()),
        },
        Expr::Relation {
            op: RelOp::Lt,
            lhs: Box::new(variable()),
            rhs: Box::new(one()),
        },
        Expr::Relation {
            op: RelOp::Le,
            lhs: Box::new(variable()),
            rhs: Box::new(one()),
        },
        Expr::Relation {
            op: RelOp::Gt,
            lhs: Box::new(variable()),
            rhs: Box::new(one()),
        },
        Expr::Relation {
            op: RelOp::Ge,
            lhs: Box::new(variable()),
            rhs: Box::new(one()),
        },
        Expr::Variable("π".to_owned()),
        Expr::Variable("α".to_owned()),
        Expr::Variable("β".to_owned()),
        Expr::Variable("γ".to_owned()),
        Expr::Variable("θ".to_owned()),
        Expr::Variable("λ".to_owned()),
        Expr::Variable("μ".to_owned()),
        Expr::Variable("φ".to_owned()),
        Expr::Variable("ω".to_owned()),
    ];
    for node in &nodes {
        let latex = node.to_latex();
        let text = node.to_text();
        assert_ne!(latex.len(), 0);
        assert_ne!(text.len(), 0);
        // 渲染结果必须能被解析器重新解析（布尔/选项是文本层节点，仅出现在归一化
        // 短路产物中，数学解析器不产出它们）
        if !matches!(node, Expr::Boolean(_) | Expr::Option(_)) {
            assert!(
                parse_expr(&latex).is_ok(),
                "LaTeX 渲染不可解析：{latex:?}（来自 {node:?}）"
            );
        }
        let _ = node.variables();
        let _ = node.is_constant();
    }
    // 关系运算符全覆盖
    for op in [
        RelOp::Eq,
        RelOp::Ne,
        RelOp::Lt,
        RelOp::Le,
        RelOp::Gt,
        RelOp::Ge,
    ] {
        assert_ne!(op.to_latex().len(), 0);
        assert_eq!(op.mirror().mirror(), op);
        assert_eq!(op.is_equality(), matches!(op, RelOp::Eq));
    }
}

#[test]
fn parser_covers_grammar_branches() {
    let cases = [
        "\\varnothing",
        "\\{\\}",
        "\\begin{bmatrix}1&2\\\\3&4\\end{bmatrix}",
        "\\begin{vmatrix}1\\end{vmatrix}",
        "\\left|{-2}\\right|",
        "\\sqrt[3]{x+1}",
        "10\\text{m/s^2}",
        "\\frac{1}{x}",
        "x_{n+1}",
        "2.5e-3",
        "\\vec{(1,2)}",
        "(1,2,3)",
        "[1,2]",
        "(1,2]",
        "(1,2)",
        "\\{a,b\\}",
        "a<b<c",
        "c>b>a",
        "x=1 或 x=2 或 x=3",
        "\\sin x",
        "\\log(x)",
        "\\int x",
        "2(x+1)",
        "(x+1)(x-1)",
        "-x^2",
        "+x",
        "\\max(x,1)",
        "\\operatorname{min}(x,1)",
    ];
    for case in cases {
        assert!(parse_expr(case).is_ok(), "解析失败：{case}");
    }
    // 错误分支
    for case in ["", "(1,2", "\\frac{1}", "1+", "x^^2", "\\vec{(1,2}"] {
        assert!(parse_expr(case).is_err(), "应当解析失败：{case}");
    }
}

#[test]
fn normalize_covers_character_and_text_paths() {
    // 平方根后接字母/括号
    assert_eq!(normalize_raw(&raw("√x")).unwrap().text, "\\sqrt{x}");
    assert_eq!(normalize_raw(&raw("√(x+1)")).unwrap().text, "\\sqrt{x + 1}");
    // 零宽字符与上标负号
    assert_eq!(normalize_raw(&raw("x\u{200b}^2")).unwrap().text, "x^2");
    assert_eq!(normalize_raw(&raw("2⁻¹")).unwrap().text, "0.5");
    // 文本型答案（汉字/单词）与错误
    assert_eq!(normalize_raw(&raw("光合作用")).unwrap().text, "光合作用");
    assert!(normalize_raw(&raw("x^")).is_err());
    assert!(normalize_raw(&raw("")).is_err());
    // latex 字段空的 RawAnswer 走 text
    let latex_only = RawAnswer {
        text: String::new(),
        latex: Some("1+2".to_owned()),
        blocks: Vec::new(),
        source: AnswerSource::ElectronicText,
    };
    assert_eq!(normalize_raw(&latex_only).unwrap().text, "3");
}

#[test]
fn canonical_covers_evaluation_and_interval_paths() {
    // 求值错误分支
    assert!(matches!(
        evaluate(&canonicalize(&parse_expr("x+1").unwrap())),
        Err(EvalError::NonConstant { .. })
    ));
    assert!(matches!(
        evaluate(&canonicalize(&parse_expr("1/0").unwrap())),
        Err(EvalError::DivideByZero)
    ));
    assert!(matches!(
        evaluate(&canonicalize(&parse_expr("\\sqrt{2}").unwrap())),
        Err(EvalError::Inexact { .. })
    ));
    assert!(matches!(
        evaluate(&canonicalize(&parse_expr("\\sin(x)").unwrap())),
        Err(EvalError::NonConstant { .. })
    ));
    assert!(matches!(
        evaluate(&canonicalize(&parse_expr("\\vec{(1,2)}").unwrap())),
        Err(EvalError::NonConstant { .. })
    ));
    assert!(matches!(
        evaluate(&canonicalize(&parse_expr("x=1").unwrap())),
        Err(EvalError::NonConstant { .. })
    ));
    // 多项式
    let poly = polynomial(&canonicalize(&parse_expr("(x+1)^2").unwrap())).unwrap();
    assert_eq!(poly.single_variable_degree("x"), Some(2));
    assert_eq!(poly.coefficient("x", 1), Rational::from_i64(2));
    assert_ne!(poly.to_expr().to_text().len(), 0);
    assert!(polynomial(&canonicalize(&parse_expr("\\sin(x)").unwrap())).is_none());
    assert!(
        polynomial_equality(
            &canonicalize(&parse_expr("x+1").unwrap()),
            &canonicalize(&parse_expr("x+1").unwrap())
        )
        .unwrap()
    );
    // 不等式 → 区间（常量真假/开闭/除零端点）
    assert!(relation_to_interval(&canonicalize(&parse_expr("1<2").unwrap())).is_ok());
    assert!(relation_to_interval(&canonicalize(&parse_expr("1>2").unwrap())).is_ok());
    assert!(relation_to_interval(&canonicalize(&parse_expr("x\\ne 2").unwrap())).is_err());
    assert!(relation_to_interval(&canonicalize(&parse_expr("x+y<2").unwrap())).is_err());
    assert!(relation_to_interval(&canonicalize(&parse_expr("x^2<2").unwrap())).is_err());
    // 方程解
    let solutions = equation_solutions(&canonicalize(&parse_expr("x^2-2*x+1=0").unwrap())).unwrap();
    assert_eq!(solutions, vec![Rational::from_i64(1)]);
    assert!(matches!(
        equation_solutions(&canonicalize(&parse_expr("x^2-2*x+2=0").unwrap()))
            .unwrap()
            .len(),
        0
    ));
    assert!(matches!(
        equation_solutions(&canonicalize(&parse_expr("x^2=2").unwrap())),
        Err(SolveError::Undecidable { .. })
    ));
    assert!(matches!(
        equation_solutions(&canonicalize(&parse_expr("x+y=1").unwrap())),
        Err(SolveError::Undecidable { .. })
    ));
    // 辅助函数
    assert_eq!(
        compare_constants(
            &Expr::Number(Rational::from_i64(1)),
            &Expr::Number(Rational::from_i64(2))
        ),
        Some(core::cmp::Ordering::Less)
    );
    assert!(
        compare_constants(
            &Expr::Variable("x".to_owned()),
            &Expr::Number(Rational::one())
        )
        .is_none()
    );
    assert_eq!(
        value_set(&[
            Expr::Number(Rational::from_i64(2)),
            Expr::Number(Rational::from_i64(1))
        ])
        .unwrap()
        .len(),
        2
    );
    assert!(value_set(&[Expr::Variable("x".to_owned())]).is_none());
    assert!(same_dimension(
        &Unit::parse("m").unwrap(),
        &Unit::parse("cm").unwrap()
    ));
    assert!(!same_dimension(
        &Unit::parse("m").unwrap(),
        &Unit::parse("s").unwrap()
    ));
    let interval = Interval::new(
        IntervalEnd::Value {
            expr: Box::new(Expr::Number(Rational::from_i64(2))),
            closed: true,
        },
        IntervalEnd::Value {
            expr: Box::new(Expr::Number(Rational::from_i64(2))),
            closed: true,
        },
    );
    assert!(interval_is_point(&interval).is_some());
    assert!(point_interval_value(&interval).is_some());
    assert!(exact_value_set(&[Expr::Number(Rational::from_i64(1))]).is_some());
}

#[test]
fn comparators_cover_undecidable_and_value_paths() {
    let numeric = NumericAbsoluteComparator;
    // 分步值/结构化答案键 → 无法判定
    let steps = AnswerValue::Steps(vec![number("1")]);
    assert!(matches!(
        numeric.compare(&steps, &answer("1")),
        ComparatorVerdict::Undecidable { .. }
    ));
    let structured = AnswerValue::Structured(serde_json::json!({"kind": "x"}));
    assert!(matches!(
        numeric.compare(&structured, &answer("1")),
        ComparatorVerdict::Undecidable { .. }
    ));
    // 无法解析的答案键表达式
    let bad_expression = AnswerValue::Expression("\\frac{1}".to_owned());
    assert!(matches!(
        numeric.compare(&bad_expression, &answer("1")),
        ComparatorVerdict::Undecidable { .. }
    ));
    // 文本答案键能解析成数值时按数值比较
    let expected_text = AnswerValue::Text("2.5".to_owned());
    assert!(
        numeric
            .compare(&expected_text, &answer("2.5"))
            .is_equal_like()
    );
    // 答案 value 字段为精确数值时优先使用
    let mut valued = answer("x");
    valued.text = "x".to_owned();
    valued.latex = None;
    valued.value = Some(number("2.5"));
    assert!(numeric.compare(&number("2.5"), &valued).is_equal_like());

    // 带单位：无量纲计数单位不一致 → 不相等；换算因子失败 → 无法判定
    let unit = NumericUnitComparator;
    let count_expected = AnswerValue::Number {
        value: Decimal::parse("3").unwrap(),
        unit: Some("个".to_owned()),
        tolerance: None,
    };
    let count_answer = answer("3\\text{人}");
    let verdict = unit.compare(&count_expected, &count_answer);
    // 计数量词不同（个 ≠ 人）→ 不相等（量纲同为空，但不能等同）
    match verdict {
        ComparatorVerdict::NotEqual { reason } => assert!(reason.contains("单位")),
        other => panic!("期望不相等，实际 {other:?}"),
    }

    // 表达式比较：答案键无法解析 → 无法判定；带单位期望 → 无法判定
    let expression = ExpressionCanonicalComparator;
    assert!(matches!(
        expression.compare(&bad_expression, &answer("1")),
        ComparatorVerdict::Undecidable { .. }
    ));
    let unit_expected = AnswerValue::Number {
        value: Decimal::parse("1").unwrap(),
        unit: Some("m".to_owned()),
        tolerance: None,
    };
    assert!(matches!(
        expression.compare(&unit_expected, &answer("1\\text{m}")),
        ComparatorVerdict::Undecidable { .. }
    ));

    // 解集：答案键不是方程 / 答案含变量 → 无法判定
    let solutions = SolutionSetComparator;
    assert!(matches!(
        solutions.compare(&number("1"), &answer("x=1")),
        ComparatorVerdict::Undecidable { .. }
    ));
    assert!(matches!(
        solutions.compare(&AnswerValue::Expression("x=1".to_owned()), &answer("x=y")),
        ComparatorVerdict::Undecidable { .. }
    ));

    // 区间：答案键不是区间；单元素集合 ↔ 单点区间
    let interval = IntervalComparator;
    assert!(matches!(
        interval.compare(&number("1"), &answer("[1,2)")),
        ComparatorVerdict::Undecidable { .. }
    ));
    assert!(
        interval
            .compare(
                &AnswerValue::Expression("[2,2]".to_owned()),
                &answer("\\{2\\}")
            )
            .is_equal_like()
    );
}

#[test]
fn chain_helpers_cover_unknown_and_defaults() {
    use eg_domain::SubjectPack;
    let pack = eg_subject_math::MathSubjectPack::new();
    let key = eg_domain::ConcreteKey {
        primary: number("1"),
        alternatives: Vec::new(),
        comparators: vec![
            eg_domain::ComparatorId::new("numeric_abs").unwrap(),
            eg_domain::ComparatorId::new("cas_trig_identity").unwrap(),
        ],
    };
    let chain = comparator_chain_for(&key, eg_domain::QuestionKind::Calculation);
    assert_eq!(chain.len(), 2);
    assert_eq!(chain[0].id().as_str(), "numeric_abs");
    // 未实现比较器占位：无法判定且带原因
    let placeholder = &chain[1];
    assert!(
        placeholder
            .compare(&number("1"), &answer("1"))
            .is_undecidable()
    );
    // 未知比较器 id
    let unknown = comparator_by_id(&eg_domain::ComparatorId::new("nope").unwrap());
    assert!(unknown.compare(&number("1"), &answer("1")).is_undecidable());
    // 全题型默认链非空
    for kind in [
        eg_domain::QuestionKind::SingleChoice,
        eg_domain::QuestionKind::MultipleChoice,
        eg_domain::QuestionKind::TrueFalse,
        eg_domain::QuestionKind::FillInBlank,
        eg_domain::QuestionKind::NumericOnly,
        eg_domain::QuestionKind::Calculation,
        eg_domain::QuestionKind::Proof,
        eg_domain::QuestionKind::ShortAnswer,
        eg_domain::QuestionKind::Essay,
        eg_domain::QuestionKind::Other,
    ] {
        assert!(!default_chain_ids(kind).is_empty(), "{kind:?}");
        assert!(
            !pack
                .comparator_chain(
                    &eg_domain::ConcreteKey {
                        primary: number("1"),
                        alternatives: Vec::new(),
                        comparators: Vec::new(),
                    },
                    kind
                )
                .is_empty()
        );
    }
    // 数值容差比较器与小写 id 常量
    let _ = Numeric;
    let _ = Tolerance::STRICT;
}

#[test]
fn static_validation_covers_dimension_paths() {
    use eg_domain::{
        AnswerKey, ConcreteKey, MilliScore, PaperId, Question, QuestionId, QuestionKind,
        RubricIssueSeverity, SubjectPack, Ulid,
    };
    let pack = eg_subject_math::MathSubjectPack::new();
    let make = |kind: QuestionKind, latex: &str| Question {
        id: QuestionId::from_ulid(Ulid::from_u128(1)),
        paper_id: PaperId::from_ulid(Ulid::from_u128(2)),
        ordinal: 1,
        label: "1".to_owned(),
        kind,
        max_score: MilliScore::from_milli(5_000).unwrap(),
        stem: None,
        key: AnswerKey::Concrete(ConcreteKey {
            primary: AnswerValue::Expression(latex.to_owned()),
            alternatives: Vec::new(),
            comparators: Vec::new(),
        }),
        rubric: None,
        subject_hint: None,
        knowledge_tags: Vec::new(),
    };
    // 空矩阵/空向量/超 3 维向量 → 错误/警告
    let issues = pack.validate_question(&make(
        QuestionKind::Calculation,
        "\\begin{pmatrix}\\end{pmatrix}",
    ));
    assert!(
        issues
            .iter()
            .any(|issue| issue.severity == RubricIssueSeverity::Error)
    );
    let issues = pack.validate_question(&make(QuestionKind::Calculation, "\\vec{(1,2,3,4)}"));
    assert!(
        issues
            .iter()
            .any(|issue| issue.severity == RubricIssueSeverity::Warning)
    );
    let issues = pack.validate_question(&make(QuestionKind::Calculation, "\\vec{}"));
    assert!(
        issues
            .iter()
            .any(|issue| issue.severity == RubricIssueSeverity::Error)
    );
    // 合法矩阵/向量 → 无错误
    let issues = pack.validate_question(&make(
        QuestionKind::Calculation,
        "\\begin{pmatrix}1&2\\\\3&4\\end{pmatrix}",
    ));
    assert!(
        !issues
            .iter()
            .any(|issue| issue.severity == RubricIssueSeverity::Error)
    );
}

#[test]
fn normalization_trace_records_layers() {
    let traced = eg_subject_math::normalize_with_trace(&raw("解：２×３。")).unwrap();
    assert_eq!(traced.answer.text, "6");
    assert_ne!(traced.corrections.len(), 0);
}
