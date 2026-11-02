//! 集合与区间比较器夹具（T26，≥20 例）：并集/交集、开闭、无穷端点、无序集合。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use eg_domain::{AnswerValue, Comparator, ComparatorVerdict, NormalizedAnswer};
use eg_subject_math::{IntervalComparator, SetExactComparator};

fn answer(latex: &str) -> NormalizedAnswer {
    NormalizedAnswer {
        text: latex.to_owned(),
        latex: Some(latex.to_owned()),
        value: None,
        steps: Vec::new(),
    }
}

fn expression(latex: &str) -> AnswerValue {
    AnswerValue::Expression(latex.to_owned())
}

fn interval(expected: &str, actual: &str) -> ComparatorVerdict {
    IntervalComparator.compare(&expression(expected), &answer(actual))
}

fn set(expected: &str, actual: &str) -> ComparatorVerdict {
    SetExactComparator.compare(&expression(expected), &answer(actual))
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
fn intervals_open_closed_and_infinity() {
    let cases = [
        ("[1,2)", "[1,2)"),
        ("[1,2]", "[1,2]"),
        ("(1,2)", "(1,2)"),
        ("(-\\infty,2]", "(-\\infty,2]"),
        ("[1,\\infty)", "[1,\\infty)"),
        ("(-\\infty,\\infty)", "(-\\infty,\\infty)"),
        ("[1,2)", "1\\le x<2"),
        ("x<2", "(-\\infty,2)"),
        ("x\\ge 1", "[1,\\infty)"),
        ("2*x+1\\ge 5", "[2,\\infty)"),
        ("[1,2]", "[1,2]"),
    ];
    for (expected, actual) in cases {
        let verdict = interval(expected, actual);
        assert!(
            verdict.is_equal_like(),
            "{expected} 应等价 {actual}，实际 {verdict:?}"
        );
    }
}

#[test]
fn interval_union_and_intersection() {
    let cases = [
        ("(-\\infty,2] \\cup [2,3)", "(-\\infty,3)"),
        ("[1,2] \\cup [2,3]", "[1,3]"),
        ("[1,2] \\cup [3,4]", "[3,4] \\cup [1,2]"),
        ("[1,2] \\cap [2,3]", "[2,2]"),
        ("(-\\infty,2] \\cap [1,\\infty)", "[1,2]"),
        ("([1,2] \\cup [3,4]) \\cap [2,3]", "[2,2] \\cup [3,3]"),
        ("[1,2] \\cup \\varnothing", "[1,2]"),
        ("[1,2] \\cap \\varnothing", "\\varnothing"),
    ];
    for (expected, actual) in cases {
        let verdict = interval(expected, actual);
        assert!(
            verdict.is_equal_like(),
            "{expected} 应等价 {actual}，实际 {verdict:?}"
        );
    }
}

#[test]
fn interval_differences_are_conclusive() {
    assert!(interval("[1,2)", "[1,2]").is_not_equal());
    assert!(interval("[1,2)", "(1,2)").is_not_equal());
    assert!(interval("[1,2] \\cup [3,4]", "[1,2]").is_not_equal());
    assert!(interval("[1,2] \\cap [2,3]", "[2,2]").is_equal_like());
    // 单点区间 ↔ 单元素集合
    assert!(interval("[2,2]", "\\{2\\}").is_equal_like());
}

#[test]
fn interval_undecidable_cases() {
    // 端点含变量
    assert_undecidable_with_reason(interval("[1,2)", "[a,b]"));
    // 二次不等式（并集，T14 不支持；T26 仍不扩二次不等式解集）
    assert_undecidable_with_reason(interval("[1,2)", "x^2>1"));
    // 答案不是区间/集合
    assert_undecidable_with_reason(interval("[1,2)", "x+y"));
}

#[test]
fn finite_sets_order_dedup_union_intersection() {
    let cases = [
        ("\\{1,2\\}", "\\{2,1\\}"),
        ("\\{1,2,2\\}", "\\{2,1\\}"),
        ("\\{1/2,3\\}", "\\{0.5,3\\}"),
        ("\\{1,2\\} \\cup \\{2,3\\}", "\\{1,2,3\\}"),
        ("\\{1,2,3\\} \\cap \\{2,3,4\\}", "\\{2,3\\}"),
        ("\\{1,2\\} \\cup \\varnothing", "\\{1,2\\}"),
        ("\\{\\}", "\\varnothing"),
        ("\\{x+y,1\\}", "\\{1,y+x\\}"),
    ];
    for (expected, actual) in cases {
        let verdict = set(expected, actual);
        assert!(
            verdict.is_equal_like(),
            "{expected} 应等价 {actual}，实际 {verdict:?}"
        );
    }
}

#[test]
fn finite_set_differences_and_undecidable() {
    assert!(set("\\{1,2\\}", "\\{1,3\\}").is_not_equal());
    assert!(set("\\{1,2\\}", "\\{1,2,3\\}").is_not_equal());
    // 符号元素不同 → 无法判定（不能假设变量取值）
    assert_undecidable_with_reason(set("\\{x\\}", "\\{y\\}"));
    // 区间不是有限集合 → 无法判定（交给区间比较器）
    assert_undecidable_with_reason(set("\\{1,2\\}", "[1,2]"));
    assert_undecidable_with_reason(set("[1,2]", "[1,2]"));
    // 答案不是集合
    assert_undecidable_with_reason(set("\\{1,2\\}", "1,2"));
}

#[test]
fn mixed_set_interval_chain_semantics() {
    // 区间比较器可处理有限数值集合（视为点区间并集）
    assert!(interval("[1,1] \\cup [2,2] \\cup [3,3]", "\\{1,2,3\\}").is_equal_like());
    // 集合比较器对区间返回无法判定；链会继续到后续比较器
    assert_undecidable_with_reason(set("\\{1,2\\}", "[1,2]"));
}
