//! 基础比较器与比较器链（`SPEC-subject-math.md` §5、§11；`SPEC-grade.md` §5.1）。
//!
//! T14 实现的比较器（按规范 §5 的优先级编号）：
//! 1 选项精确、2 布尔精确、3 数值绝对容差、4 数值带单位、5 分数精确、
//! 6 表达式规范形、8 解集、9 区间等价、10 集合等价、14 文本规范化。
//!
//! 未实现项（7 三角恒等/CAS、11 向量、12 矩阵、13 量纲一致）由
//! [`unimplemented_comparator`] 提供**保守占位**：返回「无法判定」并说明原因，
//! 绝不猜测（T26/T27 扩展点）。
//!
//! 链语义：按序执行，首个「相等/等价/不相等」终止；全部「无法判定」时保留
//! 最后一个原因（由 `eg-domain` 的契约套件与 `grade` 共同保证）。

use std::sync::Arc;

use eg_domain::{
    AnswerValue, Comparator, ComparatorId, ComparatorVerdict, Decimal, NormalizedAnswer, Tolerance,
};
use num_bigint::{BigInt, BigUint};

use crate::ast::{Expr, Interval, Unit};
use crate::canonical::{
    SolveError, canonicalize, equation_solutions, evaluate, interval_is_point, polynomial_equality,
    value_set,
};
use crate::cas::CasClient;
use crate::number::Rational;
use crate::parse::parse_expr;
use crate::setlike::{all_constant_elements, finite_set_from_expr, interval_union_from_expr};

/// 已实现（能力声明中可列出）的比较器 id。
pub const IMPLEMENTED_COMPARATOR_IDS: &[&str] = &[
    "options_exact",
    "boolean_exact",
    "numeric_abs",
    "numeric_unit",
    "fraction_exact",
    "expression_canonical",
    "cas_trig_identity",
    "solution_set",
    "interval_equivalence",
    "set_exact",
    "vector_equivalence",
    "matrix_equivalence",
    "dimension_consistency",
    "text_normalized",
];

/// 规范 §5 第 7/11/12/13 项（尚未实现，占位为无法判定）。
pub const UNIMPLEMENTED_COMPARATOR_IDS: &[&str] = &[
    "cas_trig_identity",
    "vector_equivalence",
    "matrix_equivalence",
    "dimension_consistency",
];

/// 默认比较器链（按规范 §5 优先级；实际实现项按题型筛选）。
#[must_use]
pub fn default_chain_ids(kind: eg_domain::QuestionKind) -> Vec<&'static str> {
    use eg_domain::QuestionKind;
    match kind {
        QuestionKind::SingleChoice | QuestionKind::MultipleChoice => vec!["options_exact"],
        QuestionKind::TrueFalse => vec!["boolean_exact"],
        QuestionKind::NumericOnly => vec!["numeric_abs", "numeric_unit", "fraction_exact"],
        QuestionKind::FillInBlank => vec![
            "numeric_abs",
            "numeric_unit",
            "fraction_exact",
            "expression_canonical",
            "cas_trig_identity",
            "set_exact",
            "interval_equivalence",
            "vector_equivalence",
            "matrix_equivalence",
            "dimension_consistency",
            "text_normalized",
        ],
        QuestionKind::Calculation
        | QuestionKind::Proof
        | QuestionKind::ShortAnswer
        | QuestionKind::Essay
        | QuestionKind::Other => vec![
            "numeric_abs",
            "numeric_unit",
            "fraction_exact",
            "expression_canonical",
            "cas_trig_identity",
            "solution_set",
            "interval_equivalence",
            "set_exact",
            "vector_equivalence",
            "matrix_equivalence",
            "dimension_consistency",
            "text_normalized",
        ],
    }
}

/// 按 id 构造比较器（不带 CAS）；未知 id 返回保守占位（无法判定）。
#[must_use]
pub fn comparator_by_id(id: &ComparatorId) -> Box<dyn Comparator> {
    comparator_by_id_with_cas(id, None)
}

/// 按 id 构造比较器（可注入 CAS；三角/向量/矩阵使用）。
#[must_use]
pub fn comparator_by_id_with_cas(
    id: &ComparatorId,
    cas: Option<&Arc<CasClient>>,
) -> Box<dyn Comparator> {
    let cas = cas.cloned();
    match id.as_str() {
        "options_exact" => Box::new(OptionsExactComparator),
        "boolean_exact" => Box::new(BooleanExactComparator),
        "numeric_abs" => Box::new(NumericAbsoluteComparator),
        "numeric_unit" => Box::new(NumericUnitComparator),
        "fraction_exact" => Box::new(FractionExactComparator),
        "expression_canonical" => Box::new(ExpressionCanonicalComparator),
        "solution_set" => Box::new(SolutionSetComparator),
        "interval_equivalence" => Box::new(IntervalComparator),
        "set_exact" => Box::new(SetExactComparator),
        "text_normalized" => Box::new(TextNormalizedComparator),
        "dimension_consistency" => Box::new(DimensionConsistencyComparator),
        "cas_trig_identity" => Box::new(crate::trig::TrigIdentityComparator::new(cas)),
        "vector_equivalence" => Box::new(crate::vector::VectorEquivalenceComparator::new(cas)),
        "matrix_equivalence" => Box::new(crate::matrix::MatrixEquivalenceComparator::new(cas)),
        other => Box::new(UnimplementedComparator {
            id: id.clone(),
            reason: if UNIMPLEMENTED_COMPARATOR_IDS.contains(&other) {
                "该比较器属于 T26/T27 范围，尚未实现".to_owned()
            } else {
                "未知比较器 id（答案键或规则可能来自更高版本）".to_owned()
            },
        }),
    }
}

/// 按答案键与题型构造比较器链（答案键声明优先，否则按题型默认链）。
#[must_use]
pub fn comparator_chain_for(
    key: &eg_domain::ConcreteKey,
    kind: eg_domain::QuestionKind,
) -> Vec<Box<dyn Comparator>> {
    comparator_chain_for_with_cas(key, kind, None)
}

/// 按答案键与题型构造比较器链（可注入 CAS）。
#[must_use]
pub fn comparator_chain_for_with_cas(
    key: &eg_domain::ConcreteKey,
    kind: eg_domain::QuestionKind,
    cas: Option<&Arc<CasClient>>,
) -> Vec<Box<dyn Comparator>> {
    if key.comparators.is_empty() {
        let ids: Vec<ComparatorId> = default_chain_ids(kind)
            .into_iter()
            .filter_map(|id| ComparatorId::new(id).ok())
            .collect();
        ids.iter()
            .map(|id| comparator_by_id_with_cas(id, cas))
            .collect()
    } else {
        key.comparators
            .iter()
            .map(|id| comparator_by_id_with_cas(id, cas))
            .collect()
    }
}

/// 构造指定 id 的比较器链（保持传入顺序）。
#[must_use]
pub fn chain_by_ids(ids: &[ComparatorId]) -> Vec<Box<dyn Comparator>> {
    ids.iter().map(comparator_by_id).collect()
}

#[allow(
    clippy::expect_used,
    reason = "比较器 id 均为本文件内的非空静态常量；构造失败只可能是编码错误"
)]
pub(crate) fn comparator_id(name: &'static str) -> ComparatorId {
    ComparatorId::new(name).expect("静态比较器 id 非空")
}

// ---------------------------------------------------------------------------
// 期望值解析
// ---------------------------------------------------------------------------

/// 解析后的期望值（来自答案键 `AnswerValue`）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Expected {
    Expr(Expr),
    Number {
        value: Rational,
        unit: Option<Unit>,
        tolerance: Option<Tolerance>,
    },
    Options(Vec<String>),
    Text(String),
    Boolean(bool),
}

/// 把域层 `Decimal` 精确转成有理数。
pub(crate) fn rational_from_decimal(value: &Decimal) -> Option<Rational> {
    let numer = BigInt::from(value.mantissa());
    let scale = value.scale();
    let denom = BigUint::from(10_u32).pow(scale);
    Rational::new(numer, BigInt::from(denom)).ok()
}

/// 解析期望值；无法解析时返回中文原因（比较器转为「无法判定」，绝不判错）。
pub(crate) fn expected_from_value(value: &AnswerValue) -> Result<Expected, String> {
    match value {
        AnswerValue::Number {
            value,
            unit,
            tolerance,
        } => {
            let rational = rational_from_decimal(value)
                .ok_or_else(|| "答案键数值超出精确表示范围".to_owned())?;
            let unit = match unit {
                None => None,
                Some(symbol) => Some(
                    Unit::parse(symbol).ok_or_else(|| format!("答案键单位不可识别：{symbol}"))?,
                ),
            };
            Ok(Expected::Number {
                value: rational,
                unit,
                tolerance: *tolerance,
            })
        }
        AnswerValue::Expression(latex) => {
            let expr =
                parse_expr(latex).map_err(|error| format!("答案键表达式无法解析：{error}"))?;
            Ok(Expected::Expr(canonicalize(&expr)))
        }
        AnswerValue::Text(text) => {
            if let Some(boolean) = parse_boolean(text) {
                return Ok(Expected::Boolean(boolean));
            }
            if let Some(options) = parse_options(text) {
                return Ok(Expected::Options(options));
            }
            if let Ok(expr) = parse_expr(text) {
                return Ok(Expected::Expr(canonicalize(&expr)));
            }
            Ok(Expected::Text(text.trim().to_owned()))
        }
        AnswerValue::Options(options) => {
            let letters = options
                .iter()
                .map(|option| option.trim().to_ascii_uppercase())
                .collect::<Vec<_>>();
            if letters.is_empty() || letters.iter().any(|letter| !is_option_letter(letter)) {
                return Err("答案键选项非法（必须是 A–Z 单个字母）".to_owned());
            }
            Ok(Expected::Options(letters))
        }
        AnswerValue::Steps(_) => Err("分步值答案需分步比较（T15 后续，当前无法判定）".to_owned()),
        AnswerValue::Structured(_) => {
            Err("结构化扩展答案需学科专用比较（T26 后续，当前无法判定）".to_owned())
        }
    }
}

/// 解析学生答案的语法树（优先规范化 LaTeX；失败返回 `None` → 无法判定）。
pub(crate) fn answer_expr(answer: &NormalizedAnswer) -> Option<Expr> {
    if let Some(AnswerValue::Number { value, unit, .. }) = &answer.value
        && unit.is_none()
        && let Some(rational) = rational_from_decimal(value)
    {
        return Some(Expr::Number(rational));
    }
    let text = answer.latex.as_deref().unwrap_or(&answer.text);
    parse_expr(text).ok().map(|expr| canonicalize(&expr))
}

fn answer_display(answer: &NormalizedAnswer) -> String {
    answer.latex.clone().unwrap_or_else(|| answer.text.clone())
}

fn is_option_letter(letter: &str) -> bool {
    let mut chars = letter.chars();
    matches!(
        (chars.next(), chars.next()),
        (Some(ch), None) if ch.is_ascii_uppercase()
    )
}

/// 从文本解析选项集合（A–Z 字母，忽略分隔符；顺序无关）。
pub(crate) fn parse_options(text: &str) -> Option<Vec<String>> {
    let trimmed = text.trim();
    let compact: String = trimmed
        .chars()
        .filter(|ch| {
            !ch.is_whitespace() && !matches!(ch, ',' | '，' | '、' | '/' | ';' | '；' | '和' | '与')
        })
        .collect();
    if compact.is_empty() || compact.len() > 8 {
        return None;
    }
    if !compact.chars().all(|ch| ch.is_ascii_alphabetic()) {
        return None;
    }
    // 选项字母限定 A–H（常见选项范围），避免把普通单词误判为选项。
    let letters: Vec<char> = compact.chars().map(|ch| ch.to_ascii_uppercase()).collect();
    if letters.iter().any(|letter| !matches!(letter, 'A'..='H')) {
        return None;
    }
    let has_separator = trimmed
        .chars()
        .any(|ch| matches!(ch, ',' | '，' | '、' | '/' | ';' | '；' | ' ' | '\t'));
    let all_uppercase = compact.chars().all(|ch| ch.is_ascii_uppercase());
    if !all_uppercase && !has_separator && compact.len() > 1 {
        return None;
    }
    let mut letters: Vec<String> = letters
        .into_iter()
        .map(|letter| letter.to_string())
        .collect();
    letters.sort();
    letters.dedup();
    Some(letters)
}

/// 解析判断题答案（对/错、√/×、true/false 等）。
pub(crate) fn parse_boolean(text: &str) -> Option<bool> {
    let compact: String = text.chars().filter(|ch| !ch.is_whitespace()).collect();
    let lower = compact.to_ascii_lowercase();
    match lower.as_str() {
        "对" | "正确" | "√" | "✓" | "是" | "真" | "true" | "t" | "1" => Some(true),
        "错" | "错误" | "×" | "✗" | "否" | "假" | "false" | "f" | "0" => Some(false),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// 容差
// ---------------------------------------------------------------------------

fn tolerance_allowed(
    actual: &Rational,
    expected: &Rational,
    tolerance: Option<&Tolerance>,
) -> (bool, String) {
    let difference = actual
        .checked_sub(expected)
        .map_or_else(|_| Rational::zero(), |value| value.abs());
    let Some(tolerance) = tolerance else {
        return (
            difference.is_zero(),
            format!(
                "按严格相等比较：期望 {}，实际 {}",
                expected.to_text(),
                actual.to_text()
            ),
        );
    };
    let absolute = tolerance.absolute.as_ref().and_then(rational_from_decimal);
    let relative = tolerance.relative_ppm.map(|ppm| {
        let scale = Rational::new(BigInt::from(ppm), BigInt::from(1_000_000_u32))
            .unwrap_or_else(|_| Rational::zero());
        expected
            .abs()
            .checked_mul(&scale)
            .unwrap_or_else(|_| Rational::zero())
    });
    if absolute.is_none() && relative.is_none() {
        return (
            difference.is_zero(),
            format!(
                "容差未配置，按严格相等比较：期望 {}，实际 {}",
                expected.to_text(),
                actual.to_text()
            ),
        );
    }
    let absolute_ok = absolute
        .as_ref()
        .is_some_and(|limit| difference.compare(limit) != core::cmp::Ordering::Greater);
    let relative_ok = relative
        .as_ref()
        .is_some_and(|limit| difference.compare(limit) != core::cmp::Ordering::Greater);
    let allowed = absolute_ok || relative_ok;
    let reason = if allowed {
        String::new()
    } else {
        format!(
            "差值 {} 超出容差（绝对 {}，相对 {}）；期望 {}，实际 {}",
            difference.to_text(),
            absolute
                .as_ref()
                .map_or_else(|| "未配置".to_owned(), Rational::to_text),
            relative
                .as_ref()
                .map_or_else(|| "未配置".to_owned(), Rational::to_text),
            expected.to_text(),
            actual.to_text()
        )
    };
    (allowed, reason)
}

/// 带容差的数值比较判定。
fn numeric_verdict(
    actual: &Rational,
    expected: &Rational,
    tolerance: Option<&Tolerance>,
) -> ComparatorVerdict {
    let (allowed, reason) = tolerance_allowed(actual, expected, tolerance);
    if allowed {
        if actual.compare(expected) == core::cmp::Ordering::Equal {
            ComparatorVerdict::Equal
        } else {
            ComparatorVerdict::Equivalent {
                note: format!(
                    "数值在容差内：期望 {}，实际 {}",
                    expected.to_text(),
                    actual.to_text()
                ),
            }
        }
    } else {
        ComparatorVerdict::NotEqual { reason }
    }
}

/// 取答案数值（标量；不含单位）。
fn answer_numeric(answer: &NormalizedAnswer) -> Result<Rational, String> {
    let expr = answer_expr(answer)
        .ok_or_else(|| format!("答案不是可解析的数学表达式：{}", answer_display(answer)))?;
    match &expr {
        Expr::Quantity { .. } => Err("答案带单位，需带单位比较器".to_owned()),
        Expr::Function { name, .. } => Err(format!(
            "答案含超出支持范围的函数 {name}（需 CAS，T27 后续）"
        )),
        other => evaluate(other).map_err(|error| format!("答案不能精确求值：{error}")),
    }
}

// ---------------------------------------------------------------------------
// 比较器实现
// ---------------------------------------------------------------------------

/// 1 选项精确：字母集合一致（多选忽略顺序）。
#[derive(Debug, Clone, Copy, Default)]
pub struct OptionsExactComparator;

impl Comparator for OptionsExactComparator {
    fn id(&self) -> ComparatorId {
        comparator_id("options_exact")
    }

    fn compare(&self, expected: &AnswerValue, answer: &NormalizedAnswer) -> ComparatorVerdict {
        let expected = match expected_from_value(expected) {
            Ok(Expected::Options(options)) => options,
            Ok(_) => {
                return ComparatorVerdict::Undecidable {
                    reason: "答案键不是选项集合，选项比较器不适用".to_owned(),
                };
            }
            Err(reason) => return ComparatorVerdict::Undecidable { reason },
        };
        let Some(actual) = parse_options(&answer.text) else {
            return ComparatorVerdict::Undecidable {
                reason: format!("答案不是可识别的选项：{}", answer.text),
            };
        };
        if expected.len() == 1 && actual.len() != 1 {
            return ComparatorVerdict::NotEqual {
                reason: format!("单选题答案必须且只能有 1 个选项，实际 {actual:?}"),
            };
        }
        if expected.len() > 1 && actual.len() != expected.len() {
            return ComparatorVerdict::NotEqual {
                reason: format!("多选答案选项数不同：期望 {expected:?}，实际 {actual:?}"),
            };
        }
        if expected == actual {
            ComparatorVerdict::Equal
        } else {
            ComparatorVerdict::NotEqual {
                reason: format!("选项集合不同：期望 {expected:?}，实际 {actual:?}"),
            }
        }
    }
}

/// 2 布尔精确：正确/错误、对/错、√/× 归一后一致。
#[derive(Debug, Clone, Copy, Default)]
pub struct BooleanExactComparator;

impl Comparator for BooleanExactComparator {
    fn id(&self) -> ComparatorId {
        comparator_id("boolean_exact")
    }

    fn compare(&self, expected: &AnswerValue, answer: &NormalizedAnswer) -> ComparatorVerdict {
        let expected = match expected_from_value(expected) {
            Ok(Expected::Boolean(value)) => value,
            Ok(Expected::Options(options)) if options.len() == 1 => match options[0].as_str() {
                "T" => true,
                "F" => false,
                _ => {
                    return ComparatorVerdict::Undecidable {
                        reason: "答案键选项不是判断题取值（T/F）".to_owned(),
                    };
                }
            },
            Ok(_) => {
                return ComparatorVerdict::Undecidable {
                    reason: "答案键不是判断题答案，布尔比较器不适用".to_owned(),
                };
            }
            Err(reason) => return ComparatorVerdict::Undecidable { reason },
        };
        let Some(actual) = parse_boolean(&answer.text) else {
            return ComparatorVerdict::Undecidable {
                reason: format!("答案不是可识别的判断（对/错）：{}", answer.text),
            };
        };
        if expected == actual {
            ComparatorVerdict::Equal
        } else {
            ComparatorVerdict::NotEqual {
                reason: format!(
                    "判断题答案不一致：期望 {}，实际 {}",
                    if expected { "对" } else { "错" },
                    if actual { "对" } else { "错" }
                ),
            }
        }
    }
}

/// 3 数值绝对容差：差值 ≤ 容差；默认严格相等。
#[derive(Debug, Clone, Copy, Default)]
pub struct NumericAbsoluteComparator;

impl Comparator for NumericAbsoluteComparator {
    fn id(&self) -> ComparatorId {
        comparator_id("numeric_abs")
    }

    fn compare(&self, expected: &AnswerValue, answer: &NormalizedAnswer) -> ComparatorVerdict {
        let (value, unit, tolerance) = match expected_from_value(expected) {
            Ok(Expected::Number {
                value,
                unit,
                tolerance,
            }) => (value, unit, tolerance),
            Ok(Expected::Expr(expr)) => match expr {
                Expr::Number(value) => (value, None, None),
                other => match evaluate(&other) {
                    Ok(value) => (value, None, None),
                    Err(_) => {
                        return ComparatorVerdict::Undecidable {
                            reason: "答案键不是可求值数值，数值比较器不适用".to_owned(),
                        };
                    }
                },
            },
            Ok(_) => {
                return ComparatorVerdict::Undecidable {
                    reason: "答案键不是数值，数值比较器不适用".to_owned(),
                };
            }
            Err(reason) => return ComparatorVerdict::Undecidable { reason },
        };
        if unit.is_some() {
            return ComparatorVerdict::Undecidable {
                reason: "期望值带单位，需带单位比较器（规范 §5 第 3 项明确不判）".to_owned(),
            };
        }
        match answer_numeric(answer) {
            Ok(actual) => numeric_verdict(&actual, &value, tolerance.as_ref()),
            Err(reason) => ComparatorVerdict::Undecidable { reason },
        }
    }
}

/// 4 数值带单位：量纲一致、可换算、数值在容差内。
#[derive(Debug, Clone, Copy, Default)]
pub struct NumericUnitComparator;

impl Comparator for NumericUnitComparator {
    fn id(&self) -> ComparatorId {
        comparator_id("numeric_unit")
    }

    fn compare(&self, expected: &AnswerValue, answer: &NormalizedAnswer) -> ComparatorVerdict {
        let (value, unit, tolerance) = match expected_from_value(expected) {
            Ok(Expected::Number {
                value,
                unit,
                tolerance,
            }) => (value, unit, tolerance),
            Ok(Expected::Expr(Expr::Quantity {
                value: expected_value,
                unit,
            })) => match evaluate(&expected_value) {
                Ok(value) => (value, Some(unit), None),
                Err(error) => {
                    return ComparatorVerdict::Undecidable {
                        reason: format!("答案键单位数值无法求值：{error}"),
                    };
                }
            },
            Ok(_) => {
                return ComparatorVerdict::Undecidable {
                    reason: "答案键无单位，带单位比较器不适用".to_owned(),
                };
            }
            Err(reason) => return ComparatorVerdict::Undecidable { reason },
        };
        let Some(expected_unit) = unit else {
            return ComparatorVerdict::Undecidable {
                reason: "答案键无单位，带单位比较器不适用".to_owned(),
            };
        };
        let Some(expr) = answer_expr(answer) else {
            return ComparatorVerdict::Undecidable {
                reason: format!("答案不是可解析的数学表达式：{}", answer_display(answer)),
            };
        };
        let Expr::Quantity {
            value: actual_value,
            unit: actual_unit,
        } = expr
        else {
            return ComparatorVerdict::Undecidable {
                reason: "答案缺少单位，需人工确认（规范 §5 第 4 项）".to_owned(),
            };
        };
        let actual_value = match evaluate(&actual_value) {
            Ok(value) => value,
            Err(error) => {
                return ComparatorVerdict::Undecidable {
                    reason: format!("答案单位数值无法精确求值：{error}"),
                };
            }
        };
        if expected_unit.dims != actual_unit.dims {
            return ComparatorVerdict::NotEqual {
                reason: format!(
                    "量纲不同：期望 {}，实际 {}",
                    expected_unit.symbol, actual_unit.symbol
                ),
            };
        }
        if expected_unit.dims.is_empty()
            && actual_unit.dims.is_empty()
            && expected_unit.symbol != actual_unit.symbol
        {
            return ComparatorVerdict::NotEqual {
                reason: format!(
                    "单位不一致：期望 {}，实际 {}",
                    expected_unit.symbol, actual_unit.symbol
                ),
            };
        }
        let Some(factor) = actual_unit.conversion_factor_to(&expected_unit) else {
            return ComparatorVerdict::Undecidable {
                reason: format!(
                    "单位不可换算：{} → {}",
                    actual_unit.symbol, expected_unit.symbol
                ),
            };
        };
        let converted = match actual_value.checked_mul(&factor) {
            Ok(value) => value,
            Err(error) => {
                return ComparatorVerdict::Undecidable {
                    reason: format!("单位换算数值溢出：{error}"),
                };
            }
        };
        match numeric_verdict(&converted, &value, tolerance.as_ref()) {
            ComparatorVerdict::Equal => ComparatorVerdict::Equivalent {
                note: format!(
                    "单位换算后一致：{} {} = {} {}",
                    actual_value.to_text(),
                    actual_unit.symbol,
                    converted.to_text(),
                    expected_unit.symbol
                ),
            },
            other => other,
        }
    }
}

/// 5 分数精确：精确有理数相等；含无理数 → 无法判定。
#[derive(Debug, Clone, Copy, Default)]
pub struct FractionExactComparator;

impl Comparator for FractionExactComparator {
    fn id(&self) -> ComparatorId {
        comparator_id("fraction_exact")
    }

    fn compare(&self, expected: &AnswerValue, answer: &NormalizedAnswer) -> ComparatorVerdict {
        let expected = match expected_from_value(expected) {
            Ok(Expected::Number { value, unit, .. }) => {
                if unit.is_some() {
                    return ComparatorVerdict::Undecidable {
                        reason: "带单位数值需单位比较器".to_owned(),
                    };
                }
                value
            }
            Ok(Expected::Expr(expr)) => match evaluate(&expr) {
                Ok(value) => value,
                Err(_) => {
                    return ComparatorVerdict::Undecidable {
                        reason: "答案键不是精确有理数，分数比较器不适用".to_owned(),
                    };
                }
            },
            Ok(_) => {
                return ComparatorVerdict::Undecidable {
                    reason: "答案键不是分数/有理数".to_owned(),
                };
            }
            Err(reason) => return ComparatorVerdict::Undecidable { reason },
        };
        let actual = match answer_numeric(answer) {
            Ok(value) => value,
            Err(reason) => {
                return ComparatorVerdict::Undecidable {
                    reason: format!("{reason}（分数精确比较要求精确有理数）"),
                };
            }
        };
        if expected.compare(&actual) == core::cmp::Ordering::Equal {
            ComparatorVerdict::Equal
        } else {
            ComparatorVerdict::NotEqual {
                reason: format!(
                    "有理数不相等：期望 {}，实际 {}",
                    expected.to_text(),
                    actual.to_text()
                ),
            }
        }
    }
}

/// 6 表达式规范形：展开、合并同类项、常数折叠后一致；否则按多项式判定。
#[derive(Debug, Clone, Copy, Default)]
pub struct ExpressionCanonicalComparator;

impl Comparator for ExpressionCanonicalComparator {
    fn id(&self) -> ComparatorId {
        comparator_id("expression_canonical")
    }

    fn compare(&self, expected: &AnswerValue, answer: &NormalizedAnswer) -> ComparatorVerdict {
        let expected = match expected_from_value(expected) {
            Ok(Expected::Expr(expr)) => expr,
            Ok(Expected::Number { value, unit, .. }) => {
                if unit.is_some() {
                    return ComparatorVerdict::Undecidable {
                        reason: "带单位数值需单位比较器".to_owned(),
                    };
                }
                Expr::Number(value)
            }
            Ok(_) => {
                return ComparatorVerdict::Undecidable {
                    reason: "答案键不是表达式".to_owned(),
                };
            }
            Err(reason) => return ComparatorVerdict::Undecidable { reason },
        };
        let Some(actual) = answer_expr(answer) else {
            return ComparatorVerdict::Undecidable {
                reason: format!("答案不是可解析的数学表达式：{}", answer_display(answer)),
            };
        };
        if contains_unsupported(&actual) {
            return ComparatorVerdict::Undecidable {
                reason: "答案含超出 T14 支持范围的函数/结构（需 CAS/模型，T26/T27 后续）"
                    .to_owned(),
            };
        }
        if expected == actual {
            return ComparatorVerdict::Equal;
        }
        if let Some(equal) = polynomial_equality(&expected, &actual) {
            return if equal {
                ComparatorVerdict::Equivalent {
                    note: "展开、合并同类项、常数折叠后一致".to_owned(),
                }
            } else {
                ComparatorVerdict::NotEqual {
                    reason: format!(
                        "展开合并同类项后不相同：期望 {}，实际 {}",
                        expected.to_latex(),
                        actual.to_latex()
                    ),
                }
            };
        }
        // 非常量常量表达式（可精确求值）按精确数值判定（如 √8 与 2√2）。
        if let (Ok(expected_value), Ok(actual_value)) = (evaluate(&expected), evaluate(&actual)) {
            return if expected_value.compare(&actual_value) == core::cmp::Ordering::Equal {
                ComparatorVerdict::Equivalent {
                    note: format!(
                        "精确求值后一致：{} = {}",
                        expected.to_text(),
                        actual.to_text()
                    ),
                }
            } else {
                ComparatorVerdict::NotEqual {
                    reason: format!(
                        "精确求值后不相等：期望 {}，实际 {}",
                        expected.to_text(),
                        actual.to_text()
                    ),
                }
            };
        }
        ComparatorVerdict::Undecidable {
            reason: "需要因式分解/三角变形/CAS 才能判定（超出 T14 范围，T26/T27 后续）".to_owned(),
        }
    }
}

fn contains_unsupported(expr: &Expr) -> bool {
    match expr {
        Expr::Function { .. } => true,
        Expr::Sum(items)
        | Expr::Product(items)
        | Expr::Set(items)
        | Expr::SetUnion(items)
        | Expr::SetIntersection(items)
        | Expr::Vector(items)
        | Expr::Solutions(items) => items.iter().any(contains_unsupported),
        Expr::Power(base, exponent) => contains_unsupported(base) || contains_unsupported(exponent),
        Expr::Fraction(numerator, denominator) => {
            contains_unsupported(numerator) || contains_unsupported(denominator)
        }
        Expr::Radical { radicand, degree } => {
            contains_unsupported(radicand) || degree.as_deref().is_some_and(contains_unsupported)
        }
        Expr::Absolute(value) | Expr::Quantity { value, .. } => contains_unsupported(value),
        Expr::Matrix(rows) => rows.iter().any(|row| row.iter().any(contains_unsupported)),
        Expr::Interval(interval) => {
            interval.start.expr().is_some_and(contains_unsupported)
                || interval.end.expr().is_some_and(contains_unsupported)
        }
        Expr::Relation { lhs, rhs, .. } => contains_unsupported(lhs) || contains_unsupported(rhs),
        Expr::Number(_) | Expr::Variable(_) | Expr::Boolean(_) | Expr::Option(_) => false,
    }
}

/// 8 解集：方程/多解答案的精确有理数解集一致（忽略顺序、去重）。
#[derive(Debug, Clone, Copy, Default)]
pub struct SolutionSetComparator;

impl Comparator for SolutionSetComparator {
    fn id(&self) -> ComparatorId {
        comparator_id("solution_set")
    }

    fn compare(&self, expected: &AnswerValue, answer: &NormalizedAnswer) -> ComparatorVerdict {
        let expected = match expected_from_value(expected) {
            Ok(Expected::Expr(expr)) => expr,
            Ok(_) => {
                return ComparatorVerdict::Undecidable {
                    reason: "答案键不是方程/解集".to_owned(),
                };
            }
            Err(reason) => return ComparatorVerdict::Undecidable { reason },
        };
        let Some(actual) = answer_expr(answer) else {
            return ComparatorVerdict::Undecidable {
                reason: format!("答案不是可解析的数学表达式：{}", answer_display(answer)),
            };
        };
        let expected_set = match solution_values(&expected) {
            Ok(values) => values,
            Err(error) => {
                return ComparatorVerdict::Undecidable {
                    reason: error.to_string(),
                };
            }
        };
        let actual_set = match solution_values(&actual) {
            Ok(values) => values,
            Err(error) => {
                return ComparatorVerdict::Undecidable {
                    reason: error.to_string(),
                };
            }
        };
        if expected_set == actual_set {
            if expected == actual {
                ComparatorVerdict::Equal
            } else {
                ComparatorVerdict::Equivalent {
                    note: format!(
                        "解集相同（忽略顺序、去重）：{:?}",
                        render_values(&expected_set)
                    ),
                }
            }
        } else {
            ComparatorVerdict::NotEqual {
                reason: format!(
                    "解集不同：期望 {}，实际 {}",
                    render_values(&expected_set),
                    render_values(&actual_set)
                ),
            }
        }
    }
}

fn solution_values(expr: &Expr) -> Result<Vec<Rational>, SolveError> {
    match expr {
        Expr::Relation { .. } => equation_solutions(expr),
        Expr::Set(items) => {
            let mut values = Vec::new();
            for item in items {
                match item {
                    Expr::Relation { .. } => values.extend(equation_solutions(item)?),
                    other => values.push(evaluate(other).map_err(|_| SolveError::Undecidable {
                        reason: format!("集合元素不是可精确求值的解：{}", other.to_text()),
                    })?),
                }
            }
            values.sort();
            values.dedup();
            Ok(values)
        }
        Expr::Solutions(items) => {
            let mut values = Vec::new();
            for item in items {
                match item {
                    Expr::Relation { .. } => values.extend(equation_solutions(item)?),
                    other => values.push(evaluate(other).map_err(|_| SolveError::Undecidable {
                        reason: format!("多解元素不是可精确求值的解：{}", other.to_text()),
                    })?),
                }
            }
            values.sort();
            values.dedup();
            Ok(values)
        }
        other => Err(SolveError::Undecidable {
            reason: format!("不是方程/解集：{}", other.to_text()),
        }),
    }
}

fn render_values(values: &[Rational]) -> String {
    let rendered: Vec<String> = values.iter().map(Rational::to_text).collect();
    format!("{{{}}}", rendered.join(", "))
}

/// 9 区间等价：端点与开闭一致；区间与不等式形式互通。
#[derive(Debug, Clone, Copy, Default)]
pub struct IntervalComparator;

impl Comparator for IntervalComparator {
    fn id(&self) -> ComparatorId {
        comparator_id("interval_equivalence")
    }

    fn compare(&self, expected: &AnswerValue, answer: &NormalizedAnswer) -> ComparatorVerdict {
        let expected = match expected_from_value(expected) {
            Ok(Expected::Expr(expr)) => expr,
            Ok(_) => {
                return ComparatorVerdict::Undecidable {
                    reason: "答案键不是区间/集合".to_owned(),
                };
            }
            Err(reason) => return ComparatorVerdict::Undecidable { reason },
        };
        let Some(actual) = answer_expr(answer) else {
            return ComparatorVerdict::Undecidable {
                reason: format!("答案不是可解析的数学表达式：{}", answer_display(answer)),
            };
        };
        let expected_union = match interval_union_from_expr(&expected) {
            Ok(union) => union,
            Err(reason) => {
                return ComparatorVerdict::Undecidable {
                    reason: format!("答案键区间/集合无法规范化：{reason}"),
                };
            }
        };
        let actual_union = match interval_union_from_expr(&actual) {
            Ok(union) => union,
            Err(reason) => {
                return ComparatorVerdict::Undecidable {
                    reason: format!("答案区间/集合无法规范化：{reason}"),
                };
            }
        };
        if expected_union == actual_union {
            if expected == actual {
                return ComparatorVerdict::Equal;
            }
            return ComparatorVerdict::Equivalent {
                note: format!(
                    "区间/集合等价（并集规范化后一致）：{}",
                    expected_union.to_text()
                ),
            };
        }
        // 规范形式不同 ⇒ 集合确实不同（区间并集规范形式唯一）
        ComparatorVerdict::NotEqual {
            reason: format!(
                "区间/集合不同：期望 {}，实际 {}",
                expected_union.to_text(),
                actual_union.to_text()
            ),
        }
    }
}

/// 10 集合等价：有限集合元素一致（忽略顺序、去重）。
#[derive(Debug, Clone, Copy, Default)]
pub struct SetExactComparator;

impl Comparator for SetExactComparator {
    fn id(&self) -> ComparatorId {
        comparator_id("set_exact")
    }

    fn compare(&self, expected: &AnswerValue, answer: &NormalizedAnswer) -> ComparatorVerdict {
        let expected = match expected_from_value(expected) {
            Ok(Expected::Expr(expr)) => expr,
            Ok(_) => {
                return ComparatorVerdict::Undecidable {
                    reason: "答案键不是集合".to_owned(),
                };
            }
            Err(reason) => return ComparatorVerdict::Undecidable { reason },
        };
        let Some(actual) = answer_expr(answer) else {
            return ComparatorVerdict::Undecidable {
                reason: format!("答案不是可解析的数学表达式：{}", answer_display(answer)),
            };
        };
        let expected_set = match finite_set_from_expr(&expected) {
            Ok(set) => set,
            Err(reason) => {
                return ComparatorVerdict::Undecidable {
                    reason: format!("答案键集合无法规范化：{reason}"),
                };
            }
        };
        let actual_set = match finite_set_from_expr(&actual) {
            Ok(set) => set,
            Err(reason) => {
                return ComparatorVerdict::Undecidable {
                    reason: format!("答案集合无法规范化：{reason}"),
                };
            }
        };
        if expected_set == actual_set {
            if expected == actual {
                return ComparatorVerdict::Equal;
            }
            return ComparatorVerdict::Equivalent {
                note: format!(
                    "有限集合等价（忽略顺序/去重/并交）：{} 个元素",
                    actual_set.len()
                ),
            };
        }
        if all_constant_elements(&expected_set) && all_constant_elements(&actual_set) {
            return ComparatorVerdict::NotEqual {
                reason: format!(
                    "集合元素不同：期望 {:?}，实际 {:?}",
                    render_expr_set(&expected_set),
                    render_expr_set(&actual_set)
                ),
            };
        }
        ComparatorVerdict::Undecidable {
            reason: "集合含符号元素，无法在不假设变量取值的情况下判定".to_owned(),
        }
    }
}

fn render_expr_set(set: &std::collections::BTreeSet<Expr>) -> Vec<String> {
    set.iter().map(Expr::to_text).collect()
}

/// 13 量纲一致：只判量纲（数值由数值带单位比较器负责）。
#[derive(Debug, Clone, Copy, Default)]
pub struct DimensionConsistencyComparator;

impl Comparator for DimensionConsistencyComparator {
    fn id(&self) -> ComparatorId {
        comparator_id("dimension_consistency")
    }

    #[allow(
        clippy::match_same_arms,
        reason = "Expected::Number 与 Expr::Quantity 是不同类型变体，无法用 or-pattern 合并"
    )]
    fn compare(&self, expected: &AnswerValue, answer: &NormalizedAnswer) -> ComparatorVerdict {
        let expected_unit = match expected_from_value(expected) {
            Ok(Expected::Number {
                unit: Some(unit), ..
            }) => unit,
            Ok(Expected::Expr(Expr::Quantity { unit, .. })) => unit,
            Ok(_) => {
                return ComparatorVerdict::Undecidable {
                    reason: "答案键无单位，量纲一致比较器不适用（规范 §5 第 13 项）".to_owned(),
                };
            }
            Err(reason) => return ComparatorVerdict::Undecidable { reason },
        };
        let Some(expr) = answer_expr(answer) else {
            return ComparatorVerdict::Undecidable {
                reason: format!("答案不是可解析的数学表达式：{}", answer_display(answer)),
            };
        };
        let Expr::Quantity {
            unit: actual_unit, ..
        } = expr
        else {
            return ComparatorVerdict::Undecidable {
                reason: "答案缺少单位，无法比较量纲".to_owned(),
            };
        };
        if expected_unit.dims != actual_unit.dims {
            return ComparatorVerdict::NotEqual {
                reason: format!(
                    "量纲不同：期望 {}，实际 {}",
                    expected_unit.symbol, actual_unit.symbol
                ),
            };
        }
        ComparatorVerdict::Equivalent {
            note: format!(
                "量纲一致（{}），数值由第 4 项带单位比较器负责",
                expected_unit.symbol
            ),
        }
    }
}

/// 14 文本规范化：仅答案键为文本、答案不含公式时做归一化文本比较。
#[derive(Debug, Clone, Copy, Default)]
pub struct TextNormalizedComparator;

impl Comparator for TextNormalizedComparator {
    fn id(&self) -> ComparatorId {
        comparator_id("text_normalized")
    }

    fn compare(&self, expected: &AnswerValue, answer: &NormalizedAnswer) -> ComparatorVerdict {
        let expected = match expected_from_value(expected) {
            Ok(Expected::Text(text)) => text,
            Ok(_) => {
                return ComparatorVerdict::Undecidable {
                    reason: "答案键不是文本（含公式/数值），文本比较器不适用".to_owned(),
                };
            }
            Err(reason) => return ComparatorVerdict::Undecidable { reason },
        };
        if answer.latex.is_some() {
            return ComparatorVerdict::Undecidable {
                reason: "答案含公式，不做文本比较（规范 §5 第 14 项）".to_owned(),
            };
        }
        let actual = answer.text.trim();
        if actual.chars().count() > 40 {
            return ComparatorVerdict::Undecidable {
                reason: "文本答案过长（超过 40 字符），转人工".to_owned(),
            };
        }
        if parse_boolean(actual).is_some() || parse_options(actual).is_some() {
            return ComparatorVerdict::Undecidable {
                reason: "答案可按判断/选项解释，文本比较器不适用".to_owned(),
            };
        }
        let normalized_expected: String = expected.split_whitespace().collect();
        let normalized_actual: String = actual.split_whitespace().collect();
        if normalized_expected == normalized_actual {
            ComparatorVerdict::Equal
        } else {
            ComparatorVerdict::NotEqual {
                reason: format!(
                    "文本不一致：期望 {normalized_expected:?}，实际 {normalized_actual:?}"
                ),
            }
        }
    }
}

/// 未实现比较器的保守占位：永远「无法判定」，并写明原因。
#[derive(Debug, Clone)]
pub struct UnimplementedComparator {
    /// 比较器 id。
    pub id: ComparatorId,
    /// 无法判定原因。
    pub reason: String,
}

impl Comparator for UnimplementedComparator {
    fn id(&self) -> ComparatorId {
        self.id.clone()
    }

    fn compare(&self, _expected: &AnswerValue, _answer: &NormalizedAnswer) -> ComparatorVerdict {
        ComparatorVerdict::Undecidable {
            reason: format!("比较器 {} 暂不可用：{}", self.id, self.reason),
        }
    }
}

/// 为「已声明但未实现」的比较器 id 构造占位（供 pack 与测试使用）。
#[must_use]
pub fn unimplemented_comparator(id: &str, reason: &str) -> Box<dyn Comparator> {
    Box::new(UnimplementedComparator {
        id: ComparatorId::new(id).unwrap_or_else(|_| comparator_id("unknown")),
        reason: reason.to_owned(),
    })
}

/// 区间单点辅助（供测试断言）。
#[must_use]
pub fn point_interval_value(interval: &Interval) -> Option<&Expr> {
    interval_is_point(interval)
}

/// 精确值集合辅助（供测试断言）。
#[must_use]
pub fn exact_value_set(items: &[Expr]) -> Option<Vec<Rational>> {
    value_set(items)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::parse::parse_expr;
    use num_bigint::BigInt;

    fn answer(text: &str) -> NormalizedAnswer {
        NormalizedAnswer {
            text: text.to_owned(),
            latex: Some(text.to_owned()),
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

    fn expression(latex: &str) -> AnswerValue {
        AnswerValue::Expression(latex.to_owned())
    }

    fn assert_undecidable(verdict: ComparatorVerdict) {
        match verdict {
            ComparatorVerdict::Undecidable { reason } => {
                assert!(!reason.is_empty(), "无法判定必须带原因");
            }
            other => panic!("期望无法判定，实际 {other:?}"),
        }
    }

    #[test]
    fn options_comparator() {
        let comparator = OptionsExactComparator;
        let expected = AnswerValue::Options(vec!["A".to_owned(), "B".to_owned()]);
        assert_eq!(
            comparator.compare(&expected, &text_answer("BA")),
            ComparatorVerdict::Equal
        );
        assert!(
            comparator
                .compare(&expected, &text_answer("AB"))
                .is_equal_like()
        );
        assert!(
            comparator
                .compare(&expected, &text_answer("A"))
                .is_not_equal()
        );
        assert_undecidable(comparator.compare(&expected, &text_answer("hello")));
        // 单选多答 → 不相等
        let single = AnswerValue::Options(vec!["A".to_owned()]);
        assert!(
            comparator
                .compare(&single, &text_answer("AB"))
                .is_not_equal()
        );
    }

    #[test]
    fn boolean_comparator() {
        let comparator = BooleanExactComparator;
        let expected = AnswerValue::Text("正确".to_owned());
        assert_eq!(
            comparator.compare(&expected, &text_answer("对")),
            ComparatorVerdict::Equal
        );
        assert!(
            comparator
                .compare(&expected, &text_answer("×"))
                .is_not_equal()
        );
        assert_undecidable(comparator.compare(&expected, &text_answer("也许")));
        let expected = AnswerValue::Text("错误".to_owned());
        assert_eq!(
            comparator.compare(&expected, &text_answer("false")),
            ComparatorVerdict::Equal
        );
    }

    #[test]
    fn numeric_abs_tolerance_boundaries() {
        let comparator = NumericAbsoluteComparator;
        let expected = number("2.5");
        assert_eq!(
            comparator.compare(&expected, &answer("2.5")),
            ComparatorVerdict::Equal
        );
        // 默认严格：2.50 与 2.5 精确相等
        assert!(
            comparator
                .compare(&expected, &answer("2.50"))
                .is_equal_like()
        );
        assert!(comparator.compare(&expected, &answer("2.6")).is_not_equal());
        // 容差边界：差 = 容差 → 相等；差 > 容差 → 不相等
        let with_tolerance = AnswerValue::Number {
            value: Decimal::parse("2.5").unwrap(),
            unit: None,
            tolerance: Some(Tolerance::new(Some(Decimal::parse("0.1").unwrap()), None).unwrap()),
        };
        assert!(
            comparator
                .compare(&with_tolerance, &answer("2.6"))
                .is_equal_like()
        );
        assert!(
            comparator
                .compare(&with_tolerance, &answer("2.7"))
                .is_not_equal()
        );
        // 相对容差 2%：2.5 * 0.02 = 0.05
        let relative = AnswerValue::Number {
            value: Decimal::parse("2.5").unwrap(),
            unit: None,
            tolerance: Some(Tolerance::new(None, Some(20_000)).unwrap()),
        };
        assert!(
            comparator
                .compare(&relative, &answer("2.54"))
                .is_equal_like()
        );
        assert!(
            comparator
                .compare(&relative, &answer("2.56"))
                .is_not_equal()
        );
    }

    #[test]
    fn numeric_abs_rejects_unit_mismatch_cases() {
        let comparator = NumericAbsoluteComparator;
        let expected = AnswerValue::Number {
            value: Decimal::parse("12").unwrap(),
            unit: Some("cm".to_owned()),
            tolerance: None,
        };
        assert_undecidable(comparator.compare(&expected, &answer("12")));
        let expected = number("12");
        assert_undecidable(comparator.compare(&expected, &answer("12\\text{cm}")));
        assert_undecidable(comparator.compare(&number("12"), &text_answer("十二")));
    }

    #[test]
    fn numeric_unit_conversion() {
        let comparator = NumericUnitComparator;
        let expected = AnswerValue::Number {
            value: Decimal::parse("1.2").unwrap(),
            unit: Some("m".to_owned()),
            tolerance: None,
        };
        assert!(
            comparator
                .compare(&expected, &answer("120\\text{cm}"))
                .is_equal_like()
        );
        assert!(
            comparator
                .compare(&expected, &answer("100\\text{cm}"))
                .is_not_equal()
        );
        // 量纲不同 → 不相等（附原因）
        let verdict = comparator.compare(&expected, &answer("2\\text{s}"));
        assert!(verdict.is_not_equal());
        // 缺单位 → 无法判定
        assert_undecidable(comparator.compare(&expected, &answer("1.2")));
        // 单位不可识别 → 无法判定
        let unknown_unit = AnswerValue::Number {
            value: Decimal::parse("1").unwrap(),
            unit: Some("furlong".to_owned()),
            tolerance: None,
        };
        assert_undecidable(comparator.compare(&unknown_unit, &answer("1\\text{m}")));
    }

    #[test]
    fn fraction_exact() {
        let comparator = FractionExactComparator;
        let expected = expression("\\frac{1}{2}");
        assert_eq!(
            comparator.compare(&expected, &answer("0.5")),
            ComparatorVerdict::Equal
        );
        assert!(
            comparator
                .compare(&expected, &answer("\\frac{2}{4}"))
                .is_equal_like()
        );
        assert!(comparator.compare(&expected, &answer("0.6")).is_not_equal());
        assert_undecidable(comparator.compare(&expected, &answer("\\sqrt{2}")));
    }

    #[test]
    fn expression_canonical_form() {
        let comparator = ExpressionCanonicalComparator;
        let expected = expression("(x+1)*(x-1)");
        assert!(
            comparator
                .compare(&expected, &answer("x^2-1"))
                .is_equal_like()
        );
        assert!(
            comparator
                .compare(&expression("2*x+3*x"), &answer("5*x"))
                .is_equal_like()
        );
        assert!(
            comparator
                .compare(&expression("x^2-1"), &answer("x^2+1"))
                .is_not_equal()
        );
        // 需要因式分解才能判定 → 无法判定
        assert_undecidable(comparator.compare(&expression("\\sqrt{x^2}"), &answer("x")));
        assert_undecidable(comparator.compare(&expression("\\sin(x)"), &answer("\\sin(x)")));
        // 精确数值常量等价（√8 = 2√2）
        assert!(
            comparator
                .compare(&expression("\\sqrt{8}"), &answer("2*\\sqrt{2}"))
                .is_equal_like()
        );
    }

    #[test]
    fn solution_set_comparator() {
        let comparator = SolutionSetComparator;
        let expected = expression("x^2-3*x+2=0");
        assert!(
            comparator
                .compare(&expected, &answer("x=1 或 x=2"))
                .is_equal_like()
        );
        assert!(
            comparator
                .compare(&expected, &answer("\\{1,2\\}"))
                .is_equal_like()
        );
        assert!(
            comparator
                .compare(&expected, &answer("x=1 或 x=3"))
                .is_not_equal()
        );
        // 无理根 → 无法判定（且带原因）
        assert_undecidable(comparator.compare(&expression("x^2=2"), &answer("x=\\sqrt{2}")));
        assert_undecidable(comparator.compare(&expression("2*x+1=5"), &answer("2")));
    }

    #[test]
    fn interval_comparator() {
        let comparator = IntervalComparator;
        assert_eq!(
            comparator.compare(&expression("[1,2)"), &answer("[1,2)")),
            ComparatorVerdict::Equal
        );
        assert!(
            comparator
                .compare(&expression("[1,2)"), &answer("1\\le x<2"))
                .is_equal_like()
        );
        assert!(
            comparator
                .compare(&expression("[1,2]"), &answer("(1,2)"))
                .is_not_equal()
        );
        assert!(
            comparator
                .compare(&expression("[1,2)"), &answer("x>1"))
                .is_not_equal()
        );
        assert_undecidable(comparator.compare(&expression("[1,2)"), &answer("x^2>1")));
        // 区间 ↔ 不等式互通的等价说明
        let verdict = comparator.compare(&expression("x<2"), &answer("(-\\infty,2)"));
        match verdict {
            ComparatorVerdict::Equivalent { note } => assert_ne!(note.len(), 0),
            other => panic!("期望等价，实际 {other:?}"),
        }
    }

    #[test]
    fn set_comparator() {
        let comparator = SetExactComparator;
        assert!(
            comparator
                .compare(&expression("\\{1,2,2\\}"), &answer("\\{2,1\\}"))
                .is_equal_like()
        );
        assert!(
            comparator
                .compare(&expression("\\{1,2\\}"), &answer("\\{1,3\\}"))
                .is_not_equal()
        );
        assert_undecidable(comparator.compare(&expression("\\{1,2\\}"), &answer("1,2")));
    }

    #[test]
    fn text_comparator() {
        let comparator = TextNormalizedComparator;
        let expected = AnswerValue::Text("答案 是 它".to_owned());
        assert_eq!(
            comparator.compare(&expected, &text_answer("答案 是 它")),
            ComparatorVerdict::Equal
        );
        assert!(
            comparator
                .compare(&expected, &text_answer("别的"))
                .is_not_equal()
        );
        // 含公式 → 无法判定
        assert_undecidable(comparator.compare(&expected, &answer("x=1")));
        // 答案键是数学表达式 → 无法判定
        assert_undecidable(comparator.compare(&expression("x=1"), &text_answer("x=1")));
    }

    #[test]
    fn unimplemented_comparator_is_undecidable() {
        let comparator = unimplemented_comparator("cas_trig_identity", "T27 后续");
        assert_undecidable(comparator.compare(&expression("\\sin(x)"), &answer("\\sin(x)")));
        let chain = vec![unimplemented_comparator("matrix_equivalence", "T26 后续")];
        let outcome =
            eg_domain::contract::run_comparator_chain(&chain, &expression("x"), &answer("x"));
        assert!(outcome.verdict.is_undecidable());
    }

    #[test]
    fn chain_terminates_at_first_conclusive() {
        let chain: Vec<Box<dyn Comparator>> = vec![
            Box::new(NumericAbsoluteComparator),
            Box::new(ExpressionCanonicalComparator),
        ];
        let outcome =
            eg_domain::contract::run_comparator_chain(&chain, &number("2.5"), &answer("2.5"));
        assert!(outcome.verdict.is_equal_like());
        assert_eq!(outcome.attempts.len(), 1);
    }

    #[test]
    fn default_chains_follow_priority_order() {
        use eg_domain::QuestionKind;
        let numeric = default_chain_ids(QuestionKind::NumericOnly);
        assert_eq!(numeric.first().copied(), Some("numeric_abs"));
        let calculation = default_chain_ids(QuestionKind::Calculation);
        let expression_index = calculation
            .iter()
            .position(|id| *id == "expression_canonical")
            .unwrap();
        let solution_index = calculation
            .iter()
            .position(|id| *id == "solution_set")
            .unwrap();
        let interval_index = calculation
            .iter()
            .position(|id| *id == "interval_equivalence")
            .unwrap();
        assert!(expression_index < solution_index);
        assert!(solution_index < interval_index);
        assert!(
            comparator_by_id(&comparator_id("cas_trig_identity"))
                .compare(&expression("x"), &answer("x"))
                .is_undecidable()
        );
    }

    #[test]
    fn helper_parsers() {
        assert_eq!(
            parse_options("B, A"),
            Some(vec!["A".to_owned(), "B".to_owned()])
        );
        assert_eq!(parse_options("hello"), None);
        assert_eq!(parse_boolean("√"), Some(true));
        assert_eq!(parse_boolean("×"), Some(false));
        assert_eq!(parse_boolean("不确定"), None);
        assert_eq!(
            rational_from_decimal(&Decimal::parse("2.5").unwrap()),
            Some(Rational::new(BigInt::from(5), BigInt::from(2)).unwrap())
        );
        assert!(parse_expr("x^2").is_ok());
    }
}
