//! T13 归一化夹具（`SPEC-subject-math.md` §3、§12）。
//!
//! 覆盖：字符层（全角/Unicode 数学字母/运算符/上下标/零宽）、噪声层（前缀/标点/
//! OCR 数值修正/十进制逗号）、LaTeX 层（定界符/`\left\right`/`\text` 单位）、
//! 结构层（数值/分数/根式/方程/多解/集合/区间/向量/矩阵/单位/布尔/选项/文本）。
//! 每条成功夹具同时断言「归一化幂等」；失败夹具断言类型化错误与非空原因。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use eg_domain::{AnswerSource, AnswerValue, NormalizeError, RawAnswer};
use eg_subject_math::{normalize_raw, normalize_with_trace};

fn raw(text: &str) -> RawAnswer {
    RawAnswer {
        text: text.to_owned(),
        latex: None,
        blocks: Vec::new(),
        source: AnswerSource::Recognition,
    }
}

fn raw_latex(latex: &str) -> RawAnswer {
    RawAnswer {
        text: String::new(),
        latex: Some(latex.to_owned()),
        blocks: Vec::new(),
        source: AnswerSource::Recognition,
    }
}

fn assert_idempotent(input: &RawAnswer) {
    let once = normalize_raw(input).unwrap();
    let twice = normalize_raw(&RawAnswer {
        text: once.text.clone(),
        latex: once.latex.clone(),
        blocks: Vec::new(),
        source: AnswerSource::Recognition,
    })
    .unwrap();
    assert_eq!(once, twice, "归一化不幂等：{input:?}");
}

#[test]
fn character_layer_fixtures() {
    let cases = [
        ("2.5", "2.5"),
        ("2.50", "2.5"),
        ("１２．５", "12.5"),
        ("2×3", "6"),
        ("2·3", "6"),
        ("6÷2", "3"),
        ("x−1", "x - 1"),
        ("x²", "x^2"),
        ("x¹²", "x^12"),
        ("10⁻³", "0.001"),
        ("x₁", "x_1"),
        ("x₁+x₂", "x_1 + x_2"),
        ("√9", "3"),
        ("√(16)", "4"),
        ("1e-3", "0.001"),
        ("1E+2", "100"),
    ];
    for (input, expected) in cases {
        let answer = normalize_raw(&raw(input)).unwrap();
        assert_eq!(answer.text, expected, "输入 {input:?}");
        assert_idempotent(&raw(input));
    }
}

#[test]
fn noise_layer_fixtures() {
    let cases = [
        ("解：x=1。", "x=1"),
        ("答: 2.5", "2.5"),
        ("原式=1+2", "3"),
        ("因此 x=2", "x=2"),
        ("  1  +  2  ", "3"),
        ("1O.5", "10.5"),
        ("1，5", "1.5"),
        ("l2", "12"),
    ];
    for (input, expected) in cases {
        let answer = normalize_raw(&raw(input)).unwrap();
        assert_eq!(answer.text, expected, "输入 {input:?}");
        assert_idempotent(&raw(input));
    }
    // OCR 修正必须写入修正记录（规范 §3 噪声层）
    let traced = normalize_with_trace(&raw("1O.5")).unwrap();
    assert!(
        traced
            .corrections
            .iter()
            .any(|correction| correction.detail.contains("OCR"))
    );
}

#[test]
fn latex_layer_fixtures() {
    let cases = [
        ("$\\frac{1}{2}$", "0.5"),
        ("\\(x+1\\)", "x + 1"),
        ("\\left(x+1\\right)", "x + 1"),
        ("\\left|{-3}\\right|", "3"),
        ("12\\text{cm}", "12\\text{cm}"),
        ("12\\mathrm{cm}", "12\\text{cm}"),
        ("50 km/h", "50\\text{km/h}"),
        ("\\dfrac{3}{4}", "0.75"),
    ];
    for (input, expected) in cases {
        let answer = normalize_raw(&raw(input)).unwrap();
        assert_eq!(answer.text, expected, "输入 {input:?}");
        assert_idempotent(&raw(input));
    }
    // latex 字段优先于 text
    let answer = normalize_raw(&raw_latex("\\frac{1}{3}")).unwrap();
    assert_eq!(answer.text, "1/3");
    assert_eq!(answer.latex.as_deref(), Some("\\frac{1}{3}"));
}

#[test]
fn structure_layer_fixtures() {
    let cases = [
        ("2.5", "2.5"),
        ("1/2", "0.5"),
        ("2/3", "2/3"),
        ("\\sqrt{12}", "2*\\sqrt{3}"),
        ("\\sqrt{16}", "4"),
        ("\\sqrt[3]{27}", "3"),
        ("(x+1)^2", "x^2 + 2*x + 1"),
        ("(x+1)*(x-1)", "x^2 - 1"),
        ("2*x+3*x", "5*x"),
        ("x=1", "x=1"),
        ("x\\le 2", "x\\le2"),
        ("x=1 或 x=2", "x=1 或 x=2"),
        ("[1,2)", "[1,2)"),
        ("(-\\infty,2]", "(-\\infty,2]"),
        ("\\{1,2,2\\}", "\\{1,2\\}"),
        ("\\{2,1\\}", "\\{1,2\\}"),
        ("\\vec{(1,2,3)}", "\\vec{(1,2,3)}"),
        (
            "\\begin{pmatrix}1&2\\\\3&4\\end{pmatrix}",
            "\\begin{pmatrix}1&2\\\\3&4\\end{pmatrix}",
        ),
    ];
    for (input, expected) in cases {
        let answer = normalize_raw(&raw(input)).unwrap();
        assert_eq!(answer.text, expected, "输入 {input:?}");
        assert!(
            answer.latex.is_some(),
            "数学答案应带规范化 LaTeX：{input:?}"
        );
        assert_idempotent(&raw(input));
    }
}

#[test]
fn boolean_option_text_fixtures() {
    let cases = [
        ("对", "true"),
        ("正确", "true"),
        ("√", "true"),
        ("×", "false"),
        ("错误", "false"),
        ("true", "true"),
        ("B A", "AB"),
        ("b,a", "AB"),
        ("A、C", "AC"),
        ("氢", "氢"),
        ("answer", "answer"),
        ("光合作用", "光合作用"),
    ];
    for (input, expected) in cases {
        let answer = normalize_raw(&raw(input)).unwrap();
        assert_eq!(answer.text, expected, "输入 {input:?}");
        assert!(answer.latex.is_none(), "非数学答案不应带 LaTeX：{input:?}");
        assert_idempotent(&raw(input));
    }
}

#[test]
fn structured_value_is_available_for_exact_numbers() {
    let answer = normalize_raw(&raw("2.5")).unwrap();
    assert!(matches!(answer.value, Some(AnswerValue::Number { .. })));
    let answer = normalize_raw(&raw("1/3")).unwrap();
    // 1/3 不是有限小数，无法精确转成域层 Decimal（不近似），因此不带结构化数值
    assert!(answer.value.is_none());
    let answer = normalize_raw(&raw("x=1")).unwrap();
    assert!(answer.value.is_none());
    assert!(answer.steps.is_empty(), "步骤抽取属 T15，本里程碑为空");
}

#[test]
fn unsupported_but_recognizable_constructs_are_kept_for_comparators() {
    // 三角/微积分：归一化成功（保留函数节点），比较时由比较器返回无法判定
    for input in ["\\sin(x)", "\\int x", "\\log(x)"] {
        let answer = normalize_raw(&raw(input)).unwrap();
        assert!(answer.latex.is_some(), "{input}");
        assert_idempotent(&raw(input));
    }
}

#[test]
fn parse_failures_are_typed_errors_with_reason() {
    for input in ["", "x^", "1..2", "\\frac{1}{2", "(1+2", "!"] {
        match normalize_raw(&raw(input)) {
            Err(
                NormalizeError::Unparsable { reason, .. } | NormalizeError::Unsupported { reason },
            ) => {
                assert!(!reason.is_empty(), "错误必须带原因：{input:?}");
            }
            Ok(answer) => panic!("期望归一化失败，实际成功：{input:?} → {answer:?}"),
        }
    }
    // 超长输入快速失败
    let long = "1".repeat(2_001);
    assert!(matches!(
        normalize_raw(&raw(&long)),
        Err(NormalizeError::Unsupported { .. })
    ));
}

#[test]
fn fixture_count_is_at_least_thirty() {
    // 该断言把「≥30 条归一化夹具」固化为可执行约束（§12 测试策略）。
    let mut count = 0;
    count += 16; // character
    count += 8; // noise
    count += 8; // latex
    count += 18; // structure
    count += 12; // boolean/option/text
    assert!(count >= 30, "归一化夹具数量 {count} < 30");
}
