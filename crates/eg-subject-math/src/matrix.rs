//! 矩阵等价比较器（`SPEC-subject-math.md` §5 第 12 项，T26）。
//!
//! 规则（规范）：维度一致 + 逐元素等价；维度不同、含未支持元素 → 「无法判定」；
//! 不支持矩阵运算化简（矩阵乘法/求逆等超出范围）。
//!
//! 附加诊断（只写原因，不改变判定）：
//! - 答案与期望互为转置：规范未声明接受转置，返回「无法判定」转人工；
//! - 行列式：数值矩阵可精确计算行列式，相等/不等写入原因供人工复核。

use std::sync::Arc;

use eg_domain::{AnswerValue, Comparator, ComparatorId, ComparatorVerdict, NormalizedAnswer};

use crate::ast::Expr;
use crate::canonical::{canonicalize, evaluate};
use crate::cas::CasClient;
use crate::comparators::{Expected, answer_expr, comparator_id, expected_from_value};
use crate::equiv::{ExpressionEquivalence, expressions_equivalent};
use crate::number::Rational;

/// 支持的最大矩阵阶数（规范 §4：二阶/三阶，可扩展；T26 固定 ≤3）。
pub const MAX_MATRIX_DIM: usize = 3;

/// 矩阵等价比较器。
#[derive(Debug, Clone, Default)]
pub struct MatrixEquivalenceComparator {
    /// CAS 客户端（逐元素符号等价）。
    pub cas: Option<Arc<CasClient>>,
}

impl MatrixEquivalenceComparator {
    /// 构造（可注入 CAS）。
    #[must_use]
    pub fn new(cas: Option<Arc<CasClient>>) -> Self {
        Self { cas }
    }
}

impl Comparator for MatrixEquivalenceComparator {
    fn id(&self) -> ComparatorId {
        comparator_id("matrix_equivalence")
    }

    fn compare(&self, expected: &AnswerValue, answer: &NormalizedAnswer) -> ComparatorVerdict {
        let expected = match parse_expected_matrix(expected) {
            Ok(expr) => expr,
            Err(reason) => return ComparatorVerdict::Undecidable { reason },
        };
        let Some(actual) = answer_expr(answer) else {
            return ComparatorVerdict::Undecidable {
                reason: format!("答案不是可解析的数学表达式：{}", answer.text),
            };
        };
        let actual = canonicalize(&actual);
        let (Expr::Matrix(expected_rows), Expr::Matrix(actual_rows)) = (&expected, &actual) else {
            return ComparatorVerdict::Undecidable {
                reason: "答案键或答案不是矩阵".to_owned(),
            };
        };
        let expected_shape = (
            expected_rows.len(),
            expected_rows.first().map_or(0, Vec::len),
        );
        let actual_shape = (actual_rows.len(), actual_rows.first().map_or(0, Vec::len));
        if expected_shape != actual_shape {
            return ComparatorVerdict::Undecidable {
                reason: format!(
                    "矩阵维度不同：期望 {}×{}，实际 {}×{}（规范 §5 第 12 项：维度不同不判）",
                    expected_shape.0, expected_shape.1, actual_shape.0, actual_shape.1
                ),
            };
        }
        if expected_shape.0 > MAX_MATRIX_DIM || expected_shape.1 > MAX_MATRIX_DIM {
            return ComparatorVerdict::Undecidable {
                reason: format!("矩阵维度超出 T26 支持范围（≤{MAX_MATRIX_DIM} 阶）"),
            };
        }
        let mut unequal: Vec<String> = Vec::new();
        let mut unknown: Vec<String> = Vec::new();
        for (row, (expected_row, actual_row)) in
            expected_rows.iter().zip(actual_rows.iter()).enumerate()
        {
            for (column, (left, right)) in expected_row.iter().zip(actual_row.iter()).enumerate() {
                match expressions_equivalent(left, right, self.cas.as_ref()) {
                    ExpressionEquivalence::Equal { .. } => {}
                    ExpressionEquivalence::Unequal { reason } => {
                        unequal.push(format!(
                            "第 {} 行第 {} 列不等（{reason}）",
                            row + 1,
                            column + 1
                        ));
                    }
                    ExpressionEquivalence::Unknown { reason } => {
                        unknown.push(format!(
                            "第 {} 行第 {} 列未知（{reason}）",
                            row + 1,
                            column + 1
                        ));
                    }
                }
            }
        }
        if !unequal.is_empty() {
            let transpose_note = if is_transpose(expected_rows, actual_rows) {
                "；注意：答案与期望互为转置"
            } else {
                ""
            };
            let determinant = determinant_note(expected_rows, actual_rows);
            return ComparatorVerdict::NotEqual {
                reason: format!("{}{transpose_note}{determinant}", unequal.join("；")),
            };
        }
        if !unknown.is_empty() {
            // 互为转置时明确提示转人工（规范未声明接受转置）
            if is_transpose(expected_rows, actual_rows) {
                return ComparatorVerdict::Undecidable {
                    reason: format!(
                        "答案与期望互为转置，规范未声明接受转置，转人工；{}",
                        unknown.join("；")
                    ),
                };
            }
            return ComparatorVerdict::Undecidable {
                reason: unknown.join("；"),
            };
        }
        if expected == actual {
            ComparatorVerdict::Equal
        } else {
            ComparatorVerdict::Equivalent {
                note: "逐元素等价（写法不同）".to_owned(),
            }
        }
    }
}

fn parse_expected_matrix(expected: &AnswerValue) -> Result<Expr, String> {
    match expected_from_value(expected) {
        Ok(Expected::Expr(expr)) => {
            let canonical = canonicalize(&expr);
            if matches!(canonical, Expr::Matrix(_)) {
                Ok(canonical)
            } else {
                Err("答案键不是矩阵".to_owned())
            }
        }
        Ok(_) => Err("答案键不是矩阵".to_owned()),
        Err(reason) => Err(reason),
    }
}

fn is_transpose(expected: &[Vec<Expr>], actual: &[Vec<Expr>]) -> bool {
    let (rows, columns) = (expected.len(), expected.first().map_or(0, Vec::len));
    if actual.len() != columns || actual.first().map_or(0, Vec::len) != rows {
        return false;
    }
    (0..rows).all(|row| (0..columns).all(|column| expected[row][column] == actual[column][row]))
}

fn determinant_note(expected: &[Vec<Expr>], actual: &[Vec<Expr>]) -> String {
    let (Some(left), Some(right)) = (determinant(expected), determinant(actual)) else {
        return String::new();
    };
    if left == right {
        format!("；两矩阵行列式相同（{}）", left.to_text())
    } else {
        format!(
            "；行列式不同（期望 {}，实际 {}）",
            left.to_text(),
            right.to_text()
        )
    }
}

/// 数值矩阵的精确行列式（非数值/非方阵返回 `None`）。
#[must_use]
#[allow(
    clippy::needless_range_loop,
    reason = "高斯消元需要同时按行/列索引访问 matrix[row][inner]"
)]
pub fn determinant(rows: &[Vec<Expr>]) -> Option<Rational> {
    let size = rows.len();
    if size == 0 || rows.iter().any(|row| row.len() != size) {
        return None;
    }
    let mut matrix: Vec<Vec<Rational>> = Vec::with_capacity(size);
    for row in rows {
        let values: Option<Vec<Rational>> = row.iter().map(evaluate).collect::<Result<_, _>>().ok();
        matrix.push(values?);
    }
    // 高斯消元（有理数精确运算）
    let mut determinant = Rational::one();
    for column in 0..size {
        let pivot = (column..size).find(|row| !matrix[*row][column].is_zero())?;
        if pivot != column {
            matrix.swap(pivot, column);
            determinant = determinant.neg();
        }
        let pivot_value = matrix[column][column].clone();
        determinant = determinant.checked_mul(&pivot_value).ok()?;
        for row in (column + 1)..size {
            let factor = matrix[row][column].checked_div(&pivot_value).ok()?;
            for inner in column..size {
                let product = factor.checked_mul(&matrix[column][inner]).ok()?;
                matrix[row][inner] = matrix[row][inner].checked_sub(&product).ok()?;
            }
        }
    }
    Some(determinant)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use eg_domain::NormalizedAnswer;

    fn expression(latex: &str) -> AnswerValue {
        AnswerValue::Expression(latex.to_owned())
    }

    fn answer(latex: &str) -> NormalizedAnswer {
        NormalizedAnswer {
            text: latex.to_owned(),
            latex: Some(latex.to_owned()),
            value: None,
            steps: Vec::new(),
        }
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

    #[test]
    fn equal_and_unequal_matrices() {
        let comparator = MatrixEquivalenceComparator::default();
        let expected = matrix(&[[1, 2], [3, 4]]);
        assert_eq!(
            comparator.compare(&expression(&expected), &answer(&expected)),
            ComparatorVerdict::Equal
        );
        let other = matrix(&[[1, 2], [3, 5]]);
        let verdict = comparator.compare(&expression(&expected), &answer(&other));
        match verdict {
            ComparatorVerdict::NotEqual { reason } => {
                assert!(reason.contains("行列式不同") || reason.contains("不等"));
            }
            other => panic!("期望不相等，实际 {other:?}"),
        }
        // 维度不同 → 无法判定
        assert!(
            comparator
                .compare(
                    &expression("\\begin{pmatrix}1&2\\\\3&4\\end{pmatrix}"),
                    &answer("\\begin{pmatrix}1&2&3\\\\4&5&6\\end{pmatrix}")
                )
                .is_undecidable()
        );
    }

    #[test]
    fn transpose_and_symbolic_cells() {
        let comparator = MatrixEquivalenceComparator::default();
        let transpose = matrix(&[[1, 3], [2, 4]]);
        let verdict = comparator.compare(
            &expression("\\begin{pmatrix}1&2\\\\3&4\\end{pmatrix}"),
            &answer(&transpose),
        );
        // 转置不默认接受，但元素确定不等 → 不相等（原因提示转置）
        match verdict {
            ComparatorVerdict::NotEqual { reason } => assert!(reason.contains("转置")),
            other => panic!("期望不相等，实际 {other:?}"),
        }
        let symbolic = "\\begin{pmatrix}x&y\\\\z&w\\end{pmatrix}";
        assert!(
            comparator
                .compare(&expression(symbolic), &answer(symbolic))
                .is_equal_like()
        );
        // 符号元素不同 → 恒等意义下不相等
        assert!(
            comparator
                .compare(
                    &expression(symbolic),
                    &answer("\\begin{pmatrix}a&b\\\\c&d\\end{pmatrix}")
                )
                .is_not_equal()
        );
        // 含未支持元素（微积分）且无法证明不等 → 无法判定
        let unsupported_expected = "\\begin{pmatrix}\\int x&b\\\\c&d\\end{pmatrix}";
        let unsupported_actual = "\\begin{pmatrix}\\int y&b\\\\c&d\\end{pmatrix}";
        assert!(
            comparator
                .compare(
                    &expression(unsupported_expected),
                    &answer(unsupported_actual)
                )
                .is_undecidable()
        );
    }

    #[test]
    fn exact_determinant() {
        let rows = vec![
            vec![
                Expr::Number(Rational::from_i64(1)),
                Expr::Number(Rational::from_i64(2)),
            ],
            vec![
                Expr::Number(Rational::from_i64(3)),
                Expr::Number(Rational::from_i64(4)),
            ],
        ];
        assert_eq!(determinant(&rows), Some(Rational::from_i64(-2)));
    }
}
