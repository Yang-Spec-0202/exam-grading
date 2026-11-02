//! 向量等价比较器（`SPEC-subject-math.md` §5 第 11 项，T26）。
//!
//! 规则（规范）：维度一致、逐分量等价；维度不同、含未支持分量 → 「无法判定」。
//! 诊断信息（模长/点积/平行关系）只写入原因，用于人工复核，不改变判定结论。

use std::sync::Arc;

use eg_domain::{AnswerValue, Comparator, ComparatorId, ComparatorVerdict, NormalizedAnswer};

use crate::ast::Expr;
use crate::canonical::{canonicalize, evaluate};
use crate::cas::CasClient;
use crate::comparators::{Expected, answer_expr, comparator_id, expected_from_value};
use crate::equiv::{ExpressionEquivalence, expressions_equivalent};
use crate::number::Rational;

/// 支持的最大向量维度（规范 §4：二维/三维；超限无法判定）。
pub const MAX_VECTOR_DIM: usize = 3;

/// 向量等价比较器。
#[derive(Debug, Clone, Default)]
pub struct VectorEquivalenceComparator {
    /// CAS 客户端（逐分量符号等价）。
    pub cas: Option<Arc<CasClient>>,
}

impl VectorEquivalenceComparator {
    /// 构造（可注入 CAS）。
    #[must_use]
    pub fn new(cas: Option<Arc<CasClient>>) -> Self {
        Self { cas }
    }
}

impl Comparator for VectorEquivalenceComparator {
    fn id(&self) -> ComparatorId {
        comparator_id("vector_equivalence")
    }

    fn compare(&self, expected: &AnswerValue, answer: &NormalizedAnswer) -> ComparatorVerdict {
        let expected = parse_expected_vector(expected);
        let expected = match expected {
            Ok(expr) => expr,
            Err(reason) => return ComparatorVerdict::Undecidable { reason },
        };
        let Some(actual) = answer_expr(answer) else {
            return ComparatorVerdict::Undecidable {
                reason: format!("答案不是可解析的数学表达式：{}", answer.text),
            };
        };
        let actual = canonicalize(&actual);
        let (Expr::Vector(expected_items), Expr::Vector(actual_items)) = (&expected, &actual)
        else {
            return ComparatorVerdict::Undecidable {
                reason: "答案键或答案不是向量".to_owned(),
            };
        };
        if expected_items.len() != actual_items.len() {
            return ComparatorVerdict::Undecidable {
                reason: format!(
                    "向量维度不同：期望 {} 维，实际 {} 维（规范 §5 第 11 项：维度不同不判）",
                    expected_items.len(),
                    actual_items.len()
                ),
            };
        }
        if expected_items.len() > MAX_VECTOR_DIM {
            return ComparatorVerdict::Undecidable {
                reason: format!(
                    "向量维度 {} 超出 T26 支持范围（≤{MAX_VECTOR_DIM} 维）",
                    expected_items.len()
                ),
            };
        }
        let mut unequal: Vec<String> = Vec::new();
        let mut unknown: Vec<String> = Vec::new();
        for (index, (left, right)) in expected_items.iter().zip(actual_items.iter()).enumerate() {
            match expressions_equivalent(left, right, self.cas.as_ref()) {
                ExpressionEquivalence::Equal { .. } => {}
                ExpressionEquivalence::Unequal { reason } => {
                    unequal.push(format!("第 {} 个分量不等（{reason}）", index + 1));
                }
                ExpressionEquivalence::Unknown { reason } => {
                    unknown.push(format!("第 {} 个分量未知（{reason}）", index + 1));
                }
            }
        }
        if !unequal.is_empty() {
            let diagnostics = vector_diagnostics(expected_items, actual_items);
            return ComparatorVerdict::NotEqual {
                reason: format!("{}{diagnostics}", unequal.join("；")),
            };
        }
        if !unknown.is_empty() {
            return ComparatorVerdict::Undecidable {
                reason: unknown.join("；"),
            };
        }
        if expected == actual {
            ComparatorVerdict::Equal
        } else {
            ComparatorVerdict::Equivalent {
                note: "逐分量等价（写法不同）".to_owned(),
            }
        }
    }
}

fn parse_expected_vector(expected: &AnswerValue) -> Result<Expr, String> {
    match expected_from_value(expected) {
        Ok(Expected::Expr(expr)) => {
            let canonical = canonicalize(&expr);
            if matches!(canonical, Expr::Vector(_)) {
                Ok(canonical)
            } else {
                Err("答案键不是向量".to_owned())
            }
        }
        Ok(_) => Err("答案键不是向量".to_owned()),
        Err(reason) => Err(reason),
    }
}

/// 数值向量的模/点积/平行关系诊断（仅用于原因文本，不改变判定）。
fn vector_diagnostics(expected: &[Expr], actual: &[Expr]) -> String {
    let expected_values: Option<Vec<Rational>> =
        expected.iter().map(evaluate).collect::<Result<_, _>>().ok();
    let actual_values: Option<Vec<Rational>> =
        actual.iter().map(evaluate).collect::<Result<_, _>>().ok();
    let (Some(left), Some(right)) = (expected_values, actual_values) else {
        return String::new();
    };
    let dot = left
        .iter()
        .zip(right.iter())
        .fold(Rational::zero(), |accumulator, (a, b)| {
            accumulator
                .checked_add(&a.checked_mul(b).unwrap_or_else(|_| Rational::zero()))
                .unwrap_or(accumulator)
        });
    let norm_squared = |values: &[Rational]| {
        values.iter().fold(Rational::zero(), |accumulator, value| {
            accumulator
                .checked_add(
                    &value
                        .checked_mul(value)
                        .unwrap_or_else(|_| Rational::zero()),
                )
                .unwrap_or(accumulator)
        })
    };
    let mut text = format!(
        "（模长²：期望 {}，实际 {}；点积 {}",
        norm_squared(&left).to_text(),
        norm_squared(&right).to_text(),
        dot.to_text()
    );
    // 平行判定（2/3 维分量成比例）；同向/反向只作诊断
    let parallel = (0..left.len()).all(|i| {
        (0..right.len()).all(|j| {
            let left_cross = left[i]
                .checked_mul(&right[j])
                .unwrap_or_else(|_| Rational::zero());
            let right_cross = left[j]
                .checked_mul(&right[i])
                .unwrap_or_else(|_| Rational::zero());
            left_cross == right_cross
        })
    });
    text.push_str(if parallel {
        "；两向量平行"
    } else {
        "；两向量不平行"
    });
    text.push('）');
    text
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use eg_domain::{AnswerSource, NormalizedAnswer, RawAnswer, Tolerance};

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

    #[test]
    fn equal_vectors_and_order_matters() {
        let comparator = VectorEquivalenceComparator::default();
        assert_eq!(
            comparator.compare(&expression("\\vec{(1,2)}"), &answer("\\vec{(1,2)}")),
            ComparatorVerdict::Equal
        );
        assert!(
            comparator
                .compare(&expression("\\vec{(1,2)}"), &answer("\\vec{(2,1)}"))
                .is_not_equal()
        );
        assert!(
            comparator
                .compare(&expression("\\vec{(1,2)}"), &answer("\\vec{(1,2,3)}"))
                .is_undecidable()
        );
        let _ = Tolerance::STRICT;
        let _ = RawAnswer {
            text: String::new(),
            latex: None,
            blocks: Vec::new(),
            source: AnswerSource::Recognition,
        };
    }

    #[test]
    fn symbolic_and_unsupported_components() {
        let comparator = VectorEquivalenceComparator::default();
        // 符号分量不同（x 与 y 作为独立符号）→ 恒等意义下不相等
        assert!(
            comparator
                .compare(&expression("\\vec{(x,y)}"), &answer("\\vec{(y,x)}"))
                .is_not_equal()
        );
        // 含未支持分量（微积分）→ 无法判定
        assert!(
            comparator
                .compare(&expression("\\vec{(1,2)}"), &answer("\\vec{(1,\\int x)}"))
                .is_undecidable()
        );
        assert!(
            comparator
                .compare(&expression("\\vec{(1,2)}"), &answer("1"))
                .is_undecidable()
        );
        assert!(
            comparator
                .compare(&expression("\\vec{(1,2,3,4)}"), &answer("\\vec{(1,2,3,4)}"))
                .is_undecidable()
        );
    }
}
