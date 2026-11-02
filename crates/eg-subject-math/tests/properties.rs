//! 性质测试（`SPEC-subject-math.md` §12「性质测试」）。
//!
//! 覆盖：归一化幂等、数值比较自反/对称、容差单调、比较器链「首个确定结论终止」、
//! 集合/区间顺序无关。每条属性 256 例（满足 `SPEC.md` §8「每属性 ≥200 例」）。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use eg_domain::{
    AnswerSource, AnswerValue, Comparator, ComparatorVerdict, Decimal, NormalizedAnswer, RawAnswer,
    Tolerance,
};
use eg_subject_math::{
    IntervalComparator, NumericAbsoluteComparator, SetExactComparator, normalize_raw,
    unimplemented_comparator,
};
use proptest::prelude::*;

fn milli_text(value: i64) -> String {
    let sign = if value < 0 { "-" } else { "" };
    let magnitude = value.unsigned_abs();
    let integer = magnitude / 1_000;
    let fraction = magnitude % 1_000;
    if fraction == 0 {
        format!("{sign}{integer}")
    } else {
        format!("{sign}{integer}.{fraction:03}")
    }
}

fn number_answer(value: i64) -> AnswerValue {
    AnswerValue::Number {
        value: Decimal::parse(&milli_text(value)).unwrap(),
        unit: None,
        tolerance: None,
    }
}

fn number_with_tolerance(value: i64, tolerance: i64) -> AnswerValue {
    AnswerValue::Number {
        value: Decimal::parse(&milli_text(value)).unwrap(),
        unit: None,
        tolerance: Some(
            Tolerance::new(Some(Decimal::parse(&milli_text(tolerance)).unwrap()), None).unwrap(),
        ),
    }
}

fn math_answer(latex: &str) -> NormalizedAnswer {
    NormalizedAnswer {
        text: latex.to_owned(),
        latex: Some(latex.to_owned()),
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

fn normalize_again(answer: &NormalizedAnswer) -> NormalizedAnswer {
    normalize_raw(&RawAnswer {
        text: answer.text.clone(),
        latex: answer.latex.clone(),
        blocks: Vec::new(),
        source: AnswerSource::Recognition,
    })
    .unwrap()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// 性质：归一化幂等（对产物再次归一化得到同一结果）。
    #[test]
    fn prop_normalization_is_idempotent(integer in -10_000i64..10_000, fraction in 0u32..1_000_000) {
        let input = format!("{integer}.{fraction:06}");
        let once = normalize_raw(&raw(&input)).unwrap();
        let twice = normalize_again(&once);
        prop_assert_eq!(once, twice);
    }

    /// 性质：数值比较自反（同一数值与自身比较必为相等）。
    #[test]
    fn prop_numeric_comparison_is_reflexive(value in -1_000_000i64..1_000_000) {
        let comparator = NumericAbsoluteComparator;
        let verdict = comparator.compare(&number_answer(value), &math_answer(&milli_text(value)));
        prop_assert!(verdict.is_equal_like(), "{verdict:?}");
    }

    /// 性质：数值比较对称（相等/不相等在两个方向上一致）。
    #[test]
    fn prop_numeric_comparison_is_symmetric(left in -100_000i64..100_000, right in -100_000i64..100_000) {
        let comparator = NumericAbsoluteComparator;
        let forward = comparator.compare(&number_answer(left), &math_answer(&milli_text(right)));
        let backward = comparator.compare(&number_answer(right), &math_answer(&milli_text(left)));
        prop_assert_eq!(forward.is_equal_like(), backward.is_equal_like());
    }

    /// 性质：容差单调（放宽容差只会让更多答案被接受，不会反过来）。
    #[test]
    fn prop_tolerance_is_monotonic(
        expected in -100_000i64..100_000,
        actual in -100_000i64..100_000,
        tolerance_small in 0i64..500,
        tolerance_delta in 0i64..500,
    ) {
        let comparator = NumericAbsoluteComparator;
        let tolerance_large = tolerance_small + tolerance_delta;
        let small = comparator.compare(
            &number_with_tolerance(expected, tolerance_small),
            &math_answer(&milli_text(actual)),
        );
        let large = comparator.compare(
            &number_with_tolerance(expected, tolerance_large),
            &math_answer(&milli_text(actual)),
        );
        if small.is_equal_like() {
            prop_assert!(large.is_equal_like(), "容差放大后反而拒绝：{small:?} → {large:?}");
        }
    }

    /// 性质：比较器链在首个确定结论处终止（不再尝试后续比较器）。
    #[test]
    fn prop_chain_stops_at_first_conclusive(placeholders in 0usize..4) {
        let mut chain: Vec<Box<dyn eg_domain::Comparator>> = (0..placeholders)
            .map(|index| unimplemented_comparator(&format!("placeholder_{index}"), "性质测试"))
            .collect();
        chain.push(Box::new(NumericAbsoluteComparator));
        let outcome = eg_domain::contract::run_comparator_chain(
            &chain,
            &number_answer(2_500),
            &math_answer("2.5"),
        );
        prop_assert!(outcome.verdict.is_equal_like());
        prop_assert_eq!(outcome.attempts.len(), placeholders + 1);
    }

    /// 性质：集合比较忽略顺序与重复。
    #[test]
    fn prop_set_comparison_ignores_order(values in proptest::collection::vec(-100i64..100, 1..6)) {
        let comparator = SetExactComparator;
        let mut forward = String::from("\\{");
        for (index, value) in values.iter().enumerate() {
            if index > 0 {
                forward.push(',');
            }
            forward.push_str(&value.to_string());
        }
        forward.push_str("\\}");
        let mut backward = String::from("\\{");
        for (index, value) in values.iter().rev().enumerate() {
            if index > 0 {
                backward.push(',');
            }
            backward.push_str(&value.to_string());
            // 重复元素不应改变集合语义
            backward.push(',');
            backward.push_str(&value.to_string());
        }
        backward.push_str("\\}");
        let verdict = comparator.compare(
            &AnswerValue::Expression(forward),
            &math_answer(&backward),
        );
        prop_assert!(verdict.is_equal_like(), "{verdict:?}");
    }

    /// 性质：区间比较自反（同开闭、同端点必判等价）。
    #[test]
    fn prop_interval_comparison_is_reflexive(
        low in -100i64..100,
        width in 1i64..100,
        closed_start in proptest::bool::ANY,
        closed_end in proptest::bool::ANY,
    ) {
        let high = low + width;
        let left = if closed_start { '[' } else { '(' };
        let right = if closed_end { ']' } else { ')' };
        let interval = format!("{left}{low},{high}{right}");
        let comparator = IntervalComparator;
        let verdict = comparator.compare(
            &AnswerValue::Expression(interval.clone()),
            &math_answer(&interval),
        );
        prop_assert!(verdict.is_equal_like(), "{verdict:?}");
    }

    /// 性质：无法判定必带非空原因（绝不静默）。
    #[test]
    fn prop_undecidable_verdicts_have_reasons(_ignored in 0u8..1) {
        let comparator = unimplemented_comparator("cas_trig_identity", "性质测试");
        match comparator.compare(
            &AnswerValue::Expression("\\sin(x)".to_owned()),
            &math_answer("\\sin(x)"),
        ) {
            ComparatorVerdict::Undecidable { reason } => prop_assert!(!reason.trim().is_empty()),
            other => prop_assert!(false, "期望无法判定，实际 {other:?}"),
        }
    }
}
