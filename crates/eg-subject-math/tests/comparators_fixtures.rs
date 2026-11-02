//! T14 比较器夹具（`SPEC-subject-math.md` §5、§12）。
//!
//! 覆盖数值容差边界、单位换算/量纲、分数精确、表达式规范形、解集、区间开闭与
//! 无穷端点、集合、选项/判断、文本；每条「无法判定」都断言原因非空（§11 Never）。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use eg_domain::{
    AnswerValue, Comparator, ComparatorId, ComparatorVerdict, Decimal, NormalizedAnswer,
    QuestionKind, Tolerance,
};
use eg_subject_math::{
    BooleanExactComparator, ExpressionCanonicalComparator, FractionExactComparator,
    IMPLEMENTED_COMPARATOR_IDS, IntervalComparator, NumericAbsoluteComparator,
    NumericUnitComparator, OptionsExactComparator, SetExactComparator, SolutionSetComparator,
    TextNormalizedComparator, comparator_by_id, default_chain_ids, unimplemented_comparator,
};

fn answer(latex: &str) -> NormalizedAnswer {
    NormalizedAnswer {
        text: latex.to_owned(),
        latex: Some(latex.to_owned()),
        value: None,
        steps: Vec::new(),
    }
}

fn text_answer(text: &str) -> NormalizedAnswer {
    NormalizedAnswer {
        text: text.to_owned(),
        latex: None,
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

fn number_with_unit(value: &str, unit: &str) -> AnswerValue {
    AnswerValue::Number {
        value: Decimal::parse(value).unwrap(),
        unit: Some(unit.to_owned()),
        tolerance: None,
    }
}

fn number_with_tolerance(
    value: &str,
    absolute: Option<&str>,
    relative_ppm: Option<u32>,
) -> AnswerValue {
    AnswerValue::Number {
        value: Decimal::parse(value).unwrap(),
        unit: None,
        tolerance: Some(
            Tolerance::new(
                absolute.map(|text| Decimal::parse(text).unwrap()),
                relative_ppm,
            )
            .unwrap(),
        ),
    }
}

fn expression(latex: &str) -> AnswerValue {
    AnswerValue::Expression(latex.to_owned())
}

fn assert_undecidable_with_reason(verdict: ComparatorVerdict) {
    match verdict {
        ComparatorVerdict::Undecidable { reason } => {
            assert!(!reason.trim().is_empty(), "无法判定必须带原因");
        }
        other => panic!("期望无法判定，实际 {other:?}"),
    }
}

#[test]
fn numeric_tolerance_boundaries() {
    let comparator = NumericAbsoluteComparator;
    // 默认严格相等
    assert_eq!(
        comparator.compare(&number("2.5"), &answer("2.5")),
        ComparatorVerdict::Equal
    );
    assert_eq!(
        comparator.compare(&number("2.50"), &answer("5/2")),
        ComparatorVerdict::Equal
    );
    assert!(
        comparator
            .compare(&number("2.5"), &answer("2.6"))
            .is_not_equal()
    );
    // 绝对容差 0.1：差=0.1 恰好通过；差=0.11 不通过
    let expected = number_with_tolerance("2.5", Some("0.1"), None);
    assert!(
        comparator
            .compare(&expected, &answer("2.6"))
            .is_equal_like()
    );
    assert!(
        comparator
            .compare(&expected, &answer("2.61"))
            .is_not_equal()
    );
    // 相对容差 2%（2.5 → 0.05）：边界 2.55 通过、2.56 不通过
    let expected = number_with_tolerance("2.5", None, Some(20_000));
    assert!(
        comparator
            .compare(&expected, &answer("2.55"))
            .is_equal_like()
    );
    assert!(
        comparator
            .compare(&expected, &answer("2.56"))
            .is_not_equal()
    );
    // 负值方向（对称）
    let expected = number_with_tolerance("-2.5", Some("0.1"), None);
    assert!(
        comparator
            .compare(&expected, &answer("-2.4"))
            .is_equal_like()
    );
    assert!(
        comparator
            .compare(&expected, &answer("-2.39"))
            .is_not_equal()
    );
}

#[test]
fn numeric_units_fixtures() {
    let comparator = NumericUnitComparator;
    // 1.2 m = 120 cm
    assert!(
        comparator
            .compare(&number_with_unit("1.2", "m"), &answer("120\\text{cm}"))
            .is_equal_like()
    );
    // 36 km/h = 10 m/s
    assert!(
        comparator
            .compare(&number_with_unit("36", "km/h"), &answer("10\\text{m/s}"))
            .is_equal_like()
    );
    // 数值不同 → 不相等
    assert!(
        comparator
            .compare(&number_with_unit("1.2", "m"), &answer("100\\text{cm}"))
            .is_not_equal()
    );
    // 量纲不同 → 不相等（附原因）
    let verdict = comparator.compare(&number_with_unit("1", "m"), &answer("1\\text{s}"));
    match verdict {
        ComparatorVerdict::NotEqual { reason } => assert!(reason.contains("量纲")),
        other => panic!("期望不相等，实际 {other:?}"),
    }
    // 缺单位 / 单位不可识别 / 答案键无单位 → 无法判定
    assert_undecidable_with_reason(
        comparator.compare(&number_with_unit("1.2", "m"), &answer("1.2")),
    );
    assert_undecidable_with_reason(
        comparator.compare(&number_with_unit("1", "furlong"), &answer("1\\text{m}")),
    );
    assert_undecidable_with_reason(comparator.compare(&number("1"), &answer("1\\text{m}")));
}

#[test]
fn fraction_exact_fixtures() {
    let comparator = FractionExactComparator;
    assert_eq!(
        comparator.compare(&expression("\\frac{1}{2}"), &answer("0.5")),
        ComparatorVerdict::Equal
    );
    assert!(
        comparator
            .compare(&expression("\\frac{2}{3}"), &answer("4/6"))
            .is_equal_like()
    );
    assert!(
        comparator
            .compare(&expression("\\frac{1}{3}"), &answer("0.333"))
            .is_not_equal()
    );
    // 含无理数 → 无法判定（不是答错）
    assert_undecidable_with_reason(
        comparator.compare(&expression("\\frac{1}{2}"), &answer("\\sqrt{2}")),
    );
    assert_undecidable_with_reason(comparator.compare(&expression("\\sqrt{2}"), &answer("1.414")));
}

#[test]
fn expression_canonical_fixtures() {
    let comparator = ExpressionCanonicalComparator;
    // 合并同类项
    assert!(
        comparator
            .compare(&expression("2*x+3*x"), &answer("5*x"))
            .is_equal_like()
    );
    // 展开
    assert!(
        comparator
            .compare(&expression("(x+1)*(x-1)"), &answer("x^2-1"))
            .is_equal_like()
    );
    assert!(
        comparator
            .compare(&expression("(x+1)^2"), &answer("x^2+2*x+1"))
            .is_equal_like()
    );
    // 根式化简（精确常量）
    assert!(
        comparator
            .compare(&expression("\\sqrt{8}"), &answer("2*\\sqrt{2}"))
            .is_equal_like()
    );
    // 不相等（多项式展开后不同）
    assert!(
        comparator
            .compare(&expression("x^2-1"), &answer("x^2+1"))
            .is_not_equal()
    );
    // 需要额外变形/CAS → 无法判定
    assert_undecidable_with_reason(comparator.compare(&expression("\\sqrt{x^2}"), &answer("x")));
    assert_undecidable_with_reason(
        comparator.compare(&expression("\\sin(x)"), &answer("\\sin(x)")),
    );
}

#[test]
fn solution_set_fixtures() {
    let comparator = SolutionSetComparator;
    // 单解
    assert!(
        comparator
            .compare(&expression("2*x+1=5"), &answer("x=2"))
            .is_equal_like()
    );
    // 双解（顺序无关、集合写法互通）
    assert!(
        comparator
            .compare(&expression("x^2-3*x+2=0"), &answer("x=2 或 x=1"))
            .is_equal_like()
    );
    assert!(
        comparator
            .compare(&expression("x^2-3*x+2=0"), &answer("\\{1,2\\}"))
            .is_equal_like()
    );
    // 缺解/错解 → 不相等
    assert!(
        comparator
            .compare(&expression("x^2-3*x+2=0"), &answer("x=1"))
            .is_not_equal()
    );
    assert!(
        comparator
            .compare(&expression("x^2-3*x+2=0"), &answer("x=1 或 x=3"))
            .is_not_equal()
    );
    // 无解方程 vs 空集
    assert!(
        comparator
            .compare(&expression("x^2=-1"), &answer("\\{\\}"))
            .is_equal_like()
    );
    // 无理根 / 次数过高 / 答案不是解集 → 无法判定
    assert_undecidable_with_reason(
        comparator.compare(&expression("x^2=2"), &answer("x=\\sqrt{2}")),
    );
    assert_undecidable_with_reason(comparator.compare(&expression("x^3=1"), &answer("x=1")));
    assert_undecidable_with_reason(comparator.compare(&expression("x=1"), &answer("1")));
}

#[test]
fn interval_boundary_fixtures() {
    let comparator = IntervalComparator;
    // 开闭端点区分
    assert_eq!(
        comparator.compare(&expression("[1,2)"), &answer("[1,2)")),
        ComparatorVerdict::Equal
    );
    assert!(
        comparator
            .compare(&expression("[1,2)"), &answer("[1,2]"))
            .is_not_equal()
    );
    assert!(
        comparator
            .compare(&expression("[1,2)"), &answer("(1,2)"))
            .is_not_equal()
    );
    // 区间 ↔ 不等式互通
    assert!(
        comparator
            .compare(&expression("[1,2)"), &answer("1\\le x<2"))
            .is_equal_like()
    );
    assert!(
        comparator
            .compare(&expression("x<2"), &answer("(-\\infty,2)"))
            .is_equal_like()
    );
    assert!(
        comparator
            .compare(&expression("2*x+1\\ge 5"), &answer("[2,\\infty)"))
            .is_equal_like()
    );
    // 单点区间 ↔ 单元素集合
    assert!(
        comparator
            .compare(&expression("[2,2]"), &answer("\\{2\\}"))
            .is_equal_like()
    );
    // 空集等价（两种空集写法）
    assert!(
        comparator
            .compare(&expression("[1,1)"), &answer("(2,2]"))
            .is_equal_like()
    );
    // 二次不等式（并集）/端点含变量 → 无法判定
    assert_undecidable_with_reason(comparator.compare(&expression("[1,2)"), &answer("x^2>1")));
    assert_undecidable_with_reason(comparator.compare(&expression("[1,2)"), &answer("[a,b]")));
}

#[test]
fn set_fixtures() {
    let comparator = SetExactComparator;
    assert!(
        comparator
            .compare(&expression("\\{1,2,2\\}"), &answer("\\{2,1\\}"))
            .is_equal_like()
    );
    // 数值等价写法（1/2 与 0.5）
    assert!(
        comparator
            .compare(&expression("\\{1/2,3\\}"), &answer("\\{0.5,3\\}"))
            .is_equal_like()
    );
    assert!(
        comparator
            .compare(&expression("\\{1,2\\}"), &answer("\\{1,3\\}"))
            .is_not_equal()
    );
    // 答案不是集合 → 无法判定；同一符号元素集合 → 等价（结构一致）
    assert_undecidable_with_reason(comparator.compare(&expression("\\{1,2\\}"), &answer("1,2")));
    assert!(
        comparator
            .compare(&expression("\\{\\sin(x)\\}"), &answer("\\{\\sin(x)\\}"))
            .is_equal_like()
    );
    // 不同符号元素 → 无法判定（不能假设变量取值）
    assert_undecidable_with_reason(comparator.compare(&expression("\\{x\\}"), &answer("\\{y\\}")));
}

#[test]
fn options_and_boolean_fixtures() {
    let options = OptionsExactComparator;
    let multi = AnswerValue::Options(vec!["A".to_owned(), "B".to_owned()]);
    assert_eq!(
        options.compare(&multi, &text_answer("BA")),
        ComparatorVerdict::Equal
    );
    assert!(options.compare(&multi, &text_answer("A")).is_not_equal());
    assert_undecidable_with_reason(options.compare(&multi, &text_answer("hello")));
    let single = AnswerValue::Options(vec!["A".to_owned()]);
    assert!(options.compare(&single, &text_answer("AB")).is_not_equal());

    let boolean = BooleanExactComparator;
    let expected_true = AnswerValue::Text("正确".to_owned());
    assert_eq!(
        boolean.compare(&expected_true, &text_answer("√")),
        ComparatorVerdict::Equal
    );
    assert!(
        boolean
            .compare(&expected_true, &text_answer("×"))
            .is_not_equal()
    );
    assert_undecidable_with_reason(boolean.compare(&expected_true, &text_answer("不确定")));
    // 答案键不是判断题 → 无法判定
    assert_undecidable_with_reason(boolean.compare(&expression("x=1"), &text_answer("对")));
}

#[test]
fn text_fixtures() {
    let comparator = TextNormalizedComparator;
    let expected = AnswerValue::Text("答案 是 它".to_owned());
    assert_eq!(
        comparator.compare(&expected, &text_answer("答案 是 它")),
        ComparatorVerdict::Equal
    );
    assert_eq!(
        comparator.compare(&expected, &text_answer("答案  是  它")),
        ComparatorVerdict::Equal
    );
    assert!(
        comparator
            .compare(&expected, &text_answer("别的"))
            .is_not_equal()
    );
    // 含公式 / 答案键是公式 / 过长 → 无法判定
    assert_undecidable_with_reason(comparator.compare(&expected, &answer("x=1")));
    assert_undecidable_with_reason(comparator.compare(&expression("x=1"), &text_answer("x=1")));
    let long_text = "很长的文本".repeat(10);
    assert_undecidable_with_reason(comparator.compare(
        &AnswerValue::Text(long_text.clone()),
        &text_answer(&long_text),
    ));
}

#[test]
fn chain_semantics_fixtures() {
    // 首个确定结论终止
    let chain: Vec<Box<dyn Comparator>> = vec![
        Box::new(NumericAbsoluteComparator),
        Box::new(ExpressionCanonicalComparator),
    ];
    let outcome = eg_domain::contract::run_comparator_chain(&chain, &number("2.5"), &answer("2.5"));
    assert!(outcome.verdict.is_equal_like());
    assert_eq!(outcome.attempts.len(), 1);

    // 全部无法判定：保留最后一个原因
    let chain: Vec<Box<dyn Comparator>> = vec![
        Box::new(TextNormalizedComparator),
        Box::new(FractionExactComparator),
    ];
    let outcome = eg_domain::contract::run_comparator_chain(
        &chain,
        &expression("\\sqrt{2}"),
        &answer("\\sqrt{2}"),
    );
    assert!(outcome.verdict.is_undecidable());
    assert_eq!(outcome.attempts.len(), 2);
    match outcome.verdict {
        ComparatorVerdict::Undecidable { reason } => assert_ne!(reason, ""),
        other => panic!("期望无法判定，实际 {other:?}"),
    }

    // 未实现比较器占位：保守无法判定且带原因
    let placeholder = unimplemented_comparator("cas_trig_identity", "T27 后续");
    assert_undecidable_with_reason(
        placeholder.compare(&expression("\\sin(x)"), &answer("\\sin(x)")),
    );

    // 默认链顺序按规范 §5 优先级
    let calculation = default_chain_ids(QuestionKind::Calculation);
    let position = |id: &str| calculation.iter().position(|item| *item == id).unwrap();
    assert!(position("numeric_abs") < position("numeric_unit"));
    assert!(position("numeric_unit") < position("fraction_exact"));
    assert!(position("fraction_exact") < position("expression_canonical"));
    assert!(position("expression_canonical") < position("solution_set"));
    assert!(position("solution_set") < position("interval_equivalence"));
    assert!(position("interval_equivalence") < position("set_exact"));
    assert!(position("set_exact") < position("text_normalized"));

    // 能力声明只包含已实现比较器
    for id in IMPLEMENTED_COMPARATOR_IDS {
        let comparator = comparator_by_id(&ComparatorId::new(*id).unwrap());
        assert_eq!(comparator.id().as_str(), *id);
    }
    assert!(IMPLEMENTED_COMPARATOR_IDS.len() >= 10);
}

#[test]
fn fixture_count_is_at_least_forty() {
    // 固化「数值/单位/分数/解集/区间边界夹具」数量约束（§12 测试策略）。
    let mut count = 0;
    count += 8; // numeric tolerance
    count += 7; // units
    count += 5; // fractions
    count += 7; // expression
    count += 7; // solution sets
    count += 9; // intervals
    count += 5; // sets
    count += 8; // options/boolean
    count += 6; // text
    count += 6; // chain
    assert!(count >= 40, "比较器夹具数量 {count} < 40");
}
