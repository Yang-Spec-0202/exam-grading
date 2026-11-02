//! 向量等价比较器夹具（T26，≥20 例）。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use eg_domain::{AnswerValue, Comparator, ComparatorVerdict, NormalizedAnswer};
use eg_subject_math::{CasClient, CasConfig, VectorEquivalenceComparator};

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
    VectorEquivalenceComparator::default().compare(&expression(expected), &answer(actual))
}

fn assert_undecidable_with_reason(verdict: ComparatorVerdict) {
    match verdict {
        ComparatorVerdict::Undecidable { reason } => {
            assert!(!reason.trim().is_empty(), "无法判定必须带原因");
        }
        other => panic!("期望无法判定，实际 {other:?}"),
    }
}

fn stub_client() -> Arc<CasClient> {
    let python = std::env::var("EG_CAS_PYTHON").unwrap_or_else(|_| "python3".to_owned());
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/cas/stub_sidecar.py")
        .to_string_lossy()
        .into_owned();
    Arc::new(CasClient::new(CasConfig::new(
        vec![python, script, "zero".to_owned()],
        Duration::from_millis(1_000),
        false,
    )))
}

#[test]
fn equal_vectors_variety() {
    let cases = [
        ("\\vec{(1,2)}", "\\vec{(1,2)}"),
        ("\\vec{(1,2)}", "\\vec{(1.0,2.0)}"),
        ("\\vec{(1/2,3)}", "\\vec{(0.5,3)}"),
        ("\\vec{(0,0)}", "\\vec{(0,0)}"),
        ("\\vec{(-1,2)}", "\\vec{(-1,2)}"),
        ("\\vec{(1,2,3)}", "\\vec{(1,2,3)}"),
        ("\\vec{(x+1,y)}", "\\vec{(1+x,y)}"),
        ("\\vec{(x,y)}", "\\vec{(x,y)}"),
        ("\\vec{(2*x,y)}", "\\vec{(x+x,y)}"),
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
fn order_and_dimension_matter() {
    assert!(compare("\\vec{(1,2)}", "\\vec{(2,1)}").is_not_equal());
    assert_undecidable_with_reason(compare("\\vec{(1,2)}", "\\vec{(1,2,3)}"));
    assert_undecidable_with_reason(compare("\\vec{(1,2,3)}", "\\vec{(1,2)}"));
    assert_undecidable_with_reason(compare("\\vec{(1,2,3,4)}", "\\vec{(1,2,3,4)}"));
}

#[test]
fn symbolic_and_unsupported_components() {
    // 符号分量不同（独立符号）→ 恒等意义下不相等
    assert!(compare("\\vec{(x,y)}", "\\vec{(y,x)}").is_not_equal());
    // 含未支持函数且无法证明 → 无法判定
    let verdict = compare("\\vec{(1,2)}", "\\vec{(1,\\int x)}");
    assert_undecidable_with_reason(verdict);
    // 答案不是向量
    assert_undecidable_with_reason(compare("\\vec{(1,2)}", "1"));
    assert_undecidable_with_reason(compare("1", "\\vec{(1,2)}"));
}

#[test]
fn cas_assisted_symbolic_components() {
    let comparator = VectorEquivalenceComparator::new(Some(stub_client()));
    let verdict = comparator.compare(
        &expression("\\vec{(\\tan(x)*\\cos(x),1)}"),
        &answer("\\vec{(\\sin(x),1)}"),
    );
    assert!(verdict.is_equal_like(), "{verdict:?}");
}

#[test]
fn diagnostics_are_in_reason() {
    let verdict = compare("\\vec{(1,0)}", "\\vec{(0,1)}");
    match verdict {
        ComparatorVerdict::NotEqual { reason } => {
            assert!(reason.contains("点积") || reason.contains("分量"));
        }
        other => panic!("期望不相等，实际 {other:?}"),
    }
}
