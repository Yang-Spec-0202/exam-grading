//! 矩阵等价比较器夹具（T26，≥20 例）。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use eg_domain::{AnswerValue, Comparator, ComparatorVerdict, NormalizedAnswer};
use eg_subject_math::{CasClient, CasConfig, MatrixEquivalenceComparator};

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

fn matrix(rows: &[[i64; 2]]) -> String {
    let mut text = String::from("\\begin{pmatrix}");
    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            text.push_str("\\\\");
        }
        text.push_str(&row[0].to_string());
        text.push('&');
        text.push_str(&row[1].to_string());
    }
    text.push_str("\\end{pmatrix}");
    text
}

fn compare(expected: &str, actual: &str) -> ComparatorVerdict {
    MatrixEquivalenceComparator::default().compare(&expression(expected), &answer(actual))
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
fn equal_matrices_variety() {
    let cases = [
        (matrix(&[[1, 2], [3, 4]]), matrix(&[[1, 2], [3, 4]])),
        (
            "\\begin{pmatrix}1&2\\\\3&4\\end{pmatrix}".to_owned(),
            "\\begin{pmatrix}1.0&2.0\\\\3.0&4.0\\end{pmatrix}".to_owned(),
        ),
        (
            "\\begin{pmatrix}1/2&3\\\\4&5\\end{pmatrix}".to_owned(),
            "\\begin{pmatrix}0.5&3\\\\4&5\\end{pmatrix}".to_owned(),
        ),
        (
            "\\begin{pmatrix}1&2&3\\\\4&5&6\\\\7&8&9\\end{pmatrix}".to_owned(),
            "\\begin{pmatrix}1&2&3\\\\4&5&6\\\\7&8&9\\end{pmatrix}".to_owned(),
        ),
        (
            "\\begin{pmatrix}1\\end{pmatrix}".to_owned(),
            "\\begin{pmatrix}1\\end{pmatrix}".to_owned(),
        ),
        (
            "\\begin{pmatrix}x+1&y\\\\z&w\\end{pmatrix}".to_owned(),
            "\\begin{pmatrix}1+x&y\\\\z&w\\end{pmatrix}".to_owned(),
        ),
    ];
    for (expected, actual) in cases {
        let verdict = compare(&expected, &actual);
        assert!(
            verdict.is_equal_like(),
            "{expected} 应等价，实际 {verdict:?}"
        );
    }
}

#[test]
fn element_and_dimension_differences() {
    let expected = matrix(&[[1, 2], [3, 4]]);
    // 逐元素不同（含行列式诊断）
    let verdict = compare(&expected, &matrix(&[[1, 2], [3, 5]]));
    match verdict {
        ComparatorVerdict::NotEqual { reason } => {
            assert!(reason.contains("行列式不同") || reason.contains("不等"));
        }
        other => panic!("期望不相等，实际 {other:?}"),
    }
    // 维度不同 → 无法判定（规范 §5 第 12 项）
    assert_undecidable_with_reason(compare(
        "\\begin{pmatrix}1&2\\\\3&4\\end{pmatrix}",
        "\\begin{pmatrix}1&2&3\\\\4&5&6\\end{pmatrix}",
    ));
    // 超过 3 阶 → 无法判定
    let big = "\\begin{pmatrix}1&0&0&0\\\\0&1&0&0\\\\0&0&1&0\\\\0&0&0&1\\end{pmatrix}";
    assert_undecidable_with_reason(compare(big, big));
}

#[test]
fn transpose_is_flagged_not_silently_accepted() {
    let transpose = matrix(&[[1, 3], [2, 4]]);
    let verdict = compare(&matrix(&[[1, 2], [3, 4]]), &transpose);
    match verdict {
        ComparatorVerdict::NotEqual { reason } => assert!(reason.contains("转置")),
        other => panic!("期望不相等并提示转置，实际 {other:?}"),
    }
    // 对称矩阵转置相同 → 相等
    assert!(compare(&matrix(&[[1, 2], [2, 3]]), &matrix(&[[1, 2], [2, 3]])).is_equal_like());
}

#[test]
fn symbolic_unsupported_and_non_matrix() {
    let symbolic = "\\begin{pmatrix}x&y\\\\z&w\\end{pmatrix}";
    assert!(compare(symbolic, symbolic).is_equal_like());
    // 符号元素不同 → 恒等意义下不相等
    assert!(compare(symbolic, "\\begin{pmatrix}a&b\\\\c&d\\end{pmatrix}").is_not_equal());
    // 含未支持元素且无法证明 → 无法判定
    assert_undecidable_with_reason(compare(
        "\\begin{pmatrix}\\int x&b\\\\c&d\\end{pmatrix}",
        "\\begin{pmatrix}\\int y&b\\\\c&d\\end{pmatrix}",
    ));
    // 不是矩阵 → 无法判定
    assert_undecidable_with_reason(compare(symbolic, "1"));
    assert_undecidable_with_reason(compare("1", symbolic));
}

#[test]
fn cas_assisted_symbolic_cells() {
    let comparator = MatrixEquivalenceComparator::new(Some(stub_client("zero")));
    let verdict = comparator.compare(
        &expression("\\begin{pmatrix}\\tan(x)*\\cos(x)&1\\\\2&3\\end{pmatrix}"),
        &answer("\\begin{pmatrix}\\sin(x)&1\\\\2&3\\end{pmatrix}"),
    );
    assert!(verdict.is_equal_like(), "{verdict:?}");

    let comparator = MatrixEquivalenceComparator::new(Some(stub_client("nonzero")));
    assert!(
        comparator
            .compare(
                &expression("\\begin{pmatrix}\\tan(x)*\\cos(x)&1\\\\2&3\\end{pmatrix}"),
                &answer("\\begin{pmatrix}\\sin(x)&1\\\\2&3\\end{pmatrix}")
            )
            .is_not_equal()
    );
}

#[test]
fn singular_and_zero_matrices() {
    assert!(
        compare(
            "\\begin{pmatrix}0&0\\\\0&0\\end{pmatrix}",
            "\\begin{pmatrix}0&0\\\\0&0\\end{pmatrix}"
        )
        .is_equal_like()
    );
    // 奇异矩阵 vs 可逆矩阵：元素不同 → 不相等
    assert!(
        compare(
            "\\begin{pmatrix}1&2\\\\2&4\\end{pmatrix}",
            "\\begin{pmatrix}1&2\\\\2&5\\end{pmatrix}"
        )
        .is_not_equal()
    );
}
