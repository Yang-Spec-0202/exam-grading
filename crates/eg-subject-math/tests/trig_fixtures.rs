//! 三角恒等比较器夹具（T26，≥20 例）：本地规则 / CAS 补充 / 无法判定带原因。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use eg_domain::{AnswerValue, Comparator, ComparatorVerdict, NormalizedAnswer};
use eg_subject_math::{CasClient, CasConfig, TrigIdentityComparator};

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

fn compare(expected: &str, actual: &str) -> ComparatorVerdict {
    TrigIdentityComparator::default().compare(&expression(expected), &answer(actual))
}

fn assert_undecidable_with_reason(verdict: ComparatorVerdict) {
    match verdict {
        ComparatorVerdict::Undecidable { reason } => {
            assert!(!reason.trim().is_empty(), "无法判定必须带原因");
        }
        other => panic!("期望无法判定，实际 {other:?}"),
    }
}

fn stub_client(mode: &str) -> Arc<CasClient> {
    let python = std::env::var("EG_CAS_PYTHON").unwrap_or_else(|_| "python3".to_owned());
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/cas/stub_sidecar.py")
        .to_string_lossy()
        .into_owned();
    Arc::new(CasClient::new(CasConfig::new(
        vec![python, script, mode.to_owned()],
        Duration::from_millis(1_000),
        false,
    )))
}

#[test]
fn local_identities_are_equivalent() {
    let cases = [
        ("\\sin(x)^2+\\cos(x)^2", "1"),
        ("1-\\sin(x)^2", "\\cos(x)^2"),
        ("\\sin(x)^2-1", "-\\cos(x)^2"),
        ("\\sin(2*x)", "2*\\sin(x)*\\cos(x)"),
        ("\\cos(2*x)", "\\cos(x)^2-\\sin(x)^2"),
        ("\\sin(x+y)", "\\sin(x)*\\cos(y)+\\cos(x)*\\sin(y)"),
        ("\\cos(x-y)", "\\cos(x)*\\cos(y)+\\sin(x)*\\sin(y)"),
        ("\\sin(x+2*\\pi)", "\\sin(x)"),
        ("\\cos(x-2*\\pi)", "\\cos(x)"),
        ("\\sin(x+\\pi)", "-\\sin(x)"),
        ("\\cos(x+\\pi)", "-\\cos(x)"),
        ("\\sin(-x)", "-\\sin(x)"),
        ("\\cos(-x)", "\\cos(x)"),
        ("\\sin(x+\\pi/2)", "\\cos(x)"),
        ("\\cos(x-\\pi/2)", "\\sin(x)"),
        ("2*\\sin(x)*\\cos(x)", "\\sin(2*x)"),
        ("\\frac{1}{2}*\\sin(2*x)", "\\sin(x)*\\cos(x)"),
        ("\\sin(x)^3", "\\sin(x)*(1-\\cos(x)^2)"),
    ];
    for (expected, actual) in cases {
        let verdict = compare(expected, actual);
        assert!(
            verdict.is_equal_like(),
            "{expected} 应等价 {actual}，实际 {verdict:?}"
        );
    }
}

#[test]
fn constants_and_clear_inequalities() {
    // 常量三角值（本地可约化到常量）
    assert!(compare("\\sin(0)", "0").is_equal_like());
    assert!(compare("\\cos(0)", "1").is_equal_like());
    assert!(compare("\\sin(0)", "1").is_not_equal());
    assert!(compare("\\cos(0)", "0").is_not_equal());
    // 非三角表达式 → 不适用
    assert_undecidable_with_reason(compare("x+1", "x+1"));
    assert_undecidable_with_reason(compare("1", "2"));
}

#[test]
fn undecidable_without_cas_and_with_reason() {
    let cases = [
        ("\\sin(x)", "\\cos(x)"),
        ("\\tan(x)*\\cos(x)", "\\sin(x)"),
        ("\\log(\\sin(x))", "\\log(\\cos(x))"),
        ("\\sin(x)", "x"),
    ];
    for (expected, actual) in cases {
        assert_undecidable_with_reason(compare(expected, actual));
    }
}

#[test]
fn cas_supplement_when_local_rules_insufficient() {
    let comparator = TrigIdentityComparator::new(Some(stub_client("zero")));
    let verdict = comparator.compare(&expression("\\tan(x)*\\cos(x)"), &answer("\\sin(x)"));
    assert!(verdict.is_equal_like(), "{verdict:?}");
    // CAS 非零常量 → 不相等
    let comparator = TrigIdentityComparator::new(Some(stub_client("nonzero")));
    assert!(
        comparator
            .compare(&expression("\\tan(x)*\\cos(x)"), &answer("\\sin(x)"))
            .is_not_equal()
    );
    // CAS 超时 → 无法判定（不是不相等）
    let comparator = TrigIdentityComparator::new(Some(Arc::new(CasClient::new(CasConfig::new(
        vec![
            std::env::var("EG_CAS_PYTHON").unwrap_or_else(|_| "python3".to_owned()),
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../scripts/cas/stub_sidecar.py")
                .to_string_lossy()
                .into_owned(),
            "hang".to_owned(),
        ],
        Duration::from_millis(150),
        false,
    )))));
    assert_undecidable_with_reason(
        comparator.compare(&expression("\\tan(x)*\\cos(x)"), &answer("\\sin(x)")),
    );
}
