//! 表达式等价判定（供向量/矩阵逐分量、逐元素比较复用，T26）。
//!
//! 判定顺序（保守，绝不猜）：
//! 1. 规范形完全一致 → 相等；
//! 2. 多项式恒等（展开/合并同类项后差为零）→ 相等；差非零 → 不相等（多项式恒等式可靠）；
//! 3. 两者都是可精确求值常量 → 精确比较；
//! 4. CAS 可用时请求 `simplify_zero`：差为 0 → 相等；差为常量且非 0 → 不相等；
//!    其余（非常量差、失败）→ 未知（交由上层「无法判定」）；
//! 5. 否则未知（原因说明需要 CAS/人工）。

use std::sync::Arc;

use crate::ast::Expr;
use crate::canonical::{canonicalize, evaluate, polynomial_equality};
use crate::cas::{CasClient, CasZeroResult};

/// 两个表达式的等价结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpressionEquivalence {
    /// 相等（恒等）。
    Equal {
        /// 依据（中文）。
        note: String,
    },
    /// 不相等。
    Unequal {
        /// 可读原因。
        reason: String,
    },
    /// 无法判定。
    Unknown {
        /// 可读原因。
        reason: String,
    },
}

impl ExpressionEquivalence {
    /// 是否相等。
    #[must_use]
    pub fn is_equal(&self) -> bool {
        matches!(self, Self::Equal { .. })
    }

    /// 是否不相等。
    #[must_use]
    pub fn is_unequal(&self) -> bool {
        matches!(self, Self::Unequal { .. })
    }

    /// 原因/依据。
    #[must_use]
    pub fn reason(&self) -> &str {
        match self {
            Self::Equal { note } => note,
            Self::Unequal { reason } | Self::Unknown { reason } => reason,
        }
    }
}

/// 判定两个表达式是否等价（CAS 可为 `None`）。
#[must_use]
pub fn expressions_equivalent(
    left: &Expr,
    right: &Expr,
    cas: Option<&Arc<CasClient>>,
) -> ExpressionEquivalence {
    let left = canonicalize(left);
    let right = canonicalize(right);
    if left == right {
        return ExpressionEquivalence::Equal {
            note: "规范形一致".to_owned(),
        };
    }
    if let Some(equal) = polynomial_equality(&left, &right) {
        return if equal {
            ExpressionEquivalence::Equal {
                note: "展开/合并同类项后一致".to_owned(),
            }
        } else {
            ExpressionEquivalence::Unequal {
                reason: format!(
                    "多项式展开后不相同：{} ≠ {}",
                    left.to_latex(),
                    right.to_latex()
                ),
            }
        };
    }
    if let (Ok(left_value), Ok(right_value)) = (evaluate(&left), evaluate(&right)) {
        return if left_value.compare(&right_value) == core::cmp::Ordering::Equal {
            ExpressionEquivalence::Equal {
                note: "精确求值一致".to_owned(),
            }
        } else {
            ExpressionEquivalence::Unequal {
                reason: format!(
                    "精确值不同：{} ≠ {}",
                    left_value.to_text(),
                    right_value.to_text()
                ),
            }
        };
    }
    let Some(cas) = cas else {
        return ExpressionEquivalence::Unknown {
            reason: "需要 CAS/恒等变形才能判定，且 CAS 不可用".to_owned(),
        };
    };
    let (Some(left_cas), Some(right_cas)) = (left.to_cas(), right.to_cas()) else {
        return ExpressionEquivalence::Unknown {
            reason: "表达式含 CAS 无法处理的节点（单位/区间/矩阵等）".to_owned(),
        };
    };
    match cas.simplify_zero(&left_cas, &right_cas) {
        CasZeroResult::Zero { version, .. } => ExpressionEquivalence::Equal {
            note: format!("CAS（sympy {version}）化简差式为 0"),
        },
        CasZeroResult::NonZero {
            constant: true,
            diff,
            version,
        } => ExpressionEquivalence::Unequal {
            reason: format!("CAS（sympy {version}）化简差式为常量 {diff} ≠ 0"),
        },
        CasZeroResult::NonZero {
            constant: false,
            diff,
            version,
        } => ExpressionEquivalence::Unknown {
            reason: format!(
                "CAS（sympy {version}）化简差式为 {diff}，但无法排除等价变形（转人工）"
            ),
        },
        CasZeroResult::Failure(failure) => ExpressionEquivalence::Unknown {
            reason: failure.reason(),
        },
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::parse::parse_expr;

    fn expr(input: &str) -> Expr {
        canonicalize(&parse_expr(input).unwrap())
    }

    #[test]
    fn structural_and_polynomial_equivalence() {
        assert!(expressions_equivalent(&expr("x+1"), &expr("1+x"), None).is_equal());
        assert!(expressions_equivalent(&expr("(x+1)^2"), &expr("x^2+2*x+1"), None).is_equal());
        assert!(expressions_equivalent(&expr("x+1"), &expr("x+2"), None).is_unequal());
        assert!(matches!(
            expressions_equivalent(&expr("\\sin(x)"), &expr("\\sin(x)+0"), None),
            ExpressionEquivalence::Equal { .. }
        ));
    }

    #[test]
    fn unknown_without_cas() {
        let result = expressions_equivalent(&expr("\\sqrt{x^2}"), &expr("x"), None);
        assert!(matches!(result, ExpressionEquivalence::Unknown { .. }));
        assert_ne!(result.reason(), "");
    }
}
