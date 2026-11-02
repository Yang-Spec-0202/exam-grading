//! 三角恒等比较器（`SPEC-subject-math.md` §5 第 7 项，T26）。
//!
//! 本地规则（不依赖 CAS，覆盖规范 §12「常见恒等式」）：
//! - 正弦平方和：`sin²u + cos²u = 1`（约化 `sin² → 1 − cos²` 得到唯一规范形）；
//! - 和角/差角：`sin(a±b)`、`cos(a±b)` 展开；
//! - 倍角：`sin(2u)`、`cos(2u)` 展开；
//! - 周期与象限：参数按 `π/2` 的整数倍约化（`sin(u+π/2)=cos u`、
//!   `sin(u+π)=−sin u`、`cos(u+π)=−cos u` 等），含 `2π` 周期；
//! - 奇偶性：`sin(−u)=−sin u`、`cos(−u)=cos u`。
//!
//! 判定顺序：规范形一致 → 本地三角规范形一致 → 常量精确比较 →
//! CAS（`simplify_zero`）→ 仍不确定则「无法判定」带原因。
//! 本地规则只能证明「相等」；不能证明「不相等」时绝不猜（除非两边都是常量）。
//!
//! 不支持（超出即无法判定）：反三角、含参数方程、三角不等式、符号微积分；
//! `tan/cot/sec/csc` 仅经 `to_sin_cos` 归一后参与本地规则（变量分母结构本地无法
//! 约分时转 CAS）。

use std::collections::BTreeMap;
use std::sync::Arc;

use eg_domain::{AnswerValue, Comparator, ComparatorId, ComparatorVerdict, NormalizedAnswer};

use crate::ast::Expr;
use crate::canonical::{Polynomial, canonicalize, evaluate, polynomial};
use crate::cas::{CasClient, CasZeroResult};
use crate::comparators::{Expected, answer_expr, comparator_id, expected_from_value};
use crate::number::Rational;

/// 三角函数名。
pub const TRIG_FUNCTIONS: &[&str] = &["sin", "cos", "tan", "cot", "sec", "csc"];

/// 表达式是否含三角函数（供静态校验/比较器适用性判断）。
#[must_use]
pub fn expr_has_trig(expr: &Expr) -> bool {
    match expr {
        Expr::Function { name, args } => {
            TRIG_FUNCTIONS.contains(&name.as_str()) || args.iter().any(expr_has_trig)
        }
        Expr::Sum(items)
        | Expr::Product(items)
        | Expr::Set(items)
        | Expr::SetUnion(items)
        | Expr::SetIntersection(items)
        | Expr::Vector(items)
        | Expr::Solutions(items) => items.iter().any(expr_has_trig),
        Expr::Power(base, exponent) => expr_has_trig(base) || expr_has_trig(exponent),
        Expr::Fraction(numerator, denominator) => {
            expr_has_trig(numerator) || expr_has_trig(denominator)
        }
        Expr::Radical { radicand, degree } => {
            expr_has_trig(radicand) || degree.as_deref().is_some_and(expr_has_trig)
        }
        Expr::Absolute(value) | Expr::Quantity { value, .. } => expr_has_trig(value),
        Expr::Matrix(rows) => rows.iter().any(|row| row.iter().any(expr_has_trig)),
        Expr::Interval(interval) => {
            interval.start.expr().is_some_and(expr_has_trig)
                || interval.end.expr().is_some_and(expr_has_trig)
        }
        Expr::Relation { lhs, rhs, .. } => expr_has_trig(lhs) || expr_has_trig(rhs),
        Expr::Number(_) | Expr::Variable(_) | Expr::Boolean(_) | Expr::Option(_) => false,
    }
}

/// 三角恒等比较器。
#[derive(Debug, Clone, Default)]
pub struct TrigIdentityComparator {
    /// CAS 客户端（本地规则不足时使用）。
    pub cas: Option<Arc<CasClient>>,
}

impl TrigIdentityComparator {
    /// 构造（可注入 CAS）。
    #[must_use]
    pub fn new(cas: Option<Arc<CasClient>>) -> Self {
        Self { cas }
    }
}

impl Comparator for TrigIdentityComparator {
    fn id(&self) -> ComparatorId {
        comparator_id("cas_trig_identity")
    }

    fn compare(&self, expected: &AnswerValue, answer: &NormalizedAnswer) -> ComparatorVerdict {
        let expected = match expected_from_value(expected) {
            Ok(Expected::Expr(expr)) => canonicalize(&expr),
            Ok(_) => {
                return ComparatorVerdict::Undecidable {
                    reason: "答案键不是三角表达式".to_owned(),
                };
            }
            Err(reason) => return ComparatorVerdict::Undecidable { reason },
        };
        let Some(actual) = answer_expr(answer) else {
            return ComparatorVerdict::Undecidable {
                reason: format!("答案不是可解析的数学表达式：{}", answer.text),
            };
        };
        if !expr_has_trig(&expected) && !expr_has_trig(&actual) {
            return ComparatorVerdict::Undecidable {
                reason: "双方都不含三角函数，三角恒等比较器不适用".to_owned(),
            };
        }
        if expected == actual {
            return ComparatorVerdict::Equal;
        }
        // 本地三角规范形
        let expected_form = trig_normal_form(&expected);
        let actual_form = trig_normal_form(&actual);
        match (&expected_form, &actual_form) {
            (Ok(left), Ok(right)) if left == right => {
                return ComparatorVerdict::Equivalent {
                    note: "三角恒等变形后一致（本地规则）".to_owned(),
                };
            }
            (Ok(left), Ok(right)) => {
                if let (Some(left_value), Some(right_value)) =
                    (polynomial_constant(left), polynomial_constant(right))
                {
                    return if left_value == right_value {
                        ComparatorVerdict::Equivalent {
                            note: "三角常量精确相等".to_owned(),
                        }
                    } else {
                        ComparatorVerdict::NotEqual {
                            reason: format!(
                                "三角常量不相等：{} ≠ {}",
                                left_value.to_text(),
                                right_value.to_text()
                            ),
                        }
                    };
                }
            }
            _ => {}
        }
        // CAS 补充（本地规则不足）
        let Some(cas) = self.cas.as_ref() else {
            return ComparatorVerdict::Undecidable {
                reason: "本地三角恒等规则不足，且 CAS 不可用（无法判定）".to_owned(),
            };
        };
        let (Some(left_cas), Some(right_cas)) = (expected.to_cas(), actual.to_cas()) else {
            return ComparatorVerdict::Undecidable {
                reason: "三角表达式含 CAS 无法处理的节点（单位/区间等）".to_owned(),
            };
        };
        match cas.simplify_zero(&left_cas, &right_cas) {
            CasZeroResult::Zero { version, .. } => ComparatorVerdict::Equivalent {
                note: format!("CAS（sympy {version}）化简差式为 0（三角恒等）"),
            },
            CasZeroResult::NonZero {
                constant: true,
                diff,
                version,
            } => ComparatorVerdict::NotEqual {
                reason: format!("CAS（sympy {version}）化简差式为常量 {diff} ≠ 0"),
            },
            CasZeroResult::NonZero {
                constant: false,
                diff,
                version,
            } => ComparatorVerdict::Undecidable {
                reason: format!(
                    "CAS（sympy {version}）化简差式为 {diff}，但无法排除未覆盖的恒等变形（转人工）"
                ),
            },
            CasZeroResult::Failure(failure) => ComparatorVerdict::Undecidable {
                reason: failure.reason(),
            },
        }
    }
}

fn polynomial_constant(polynomial: &Polynomial) -> Option<Rational> {
    if polynomial
        .terms
        .iter()
        .any(|(monomial, _)| !monomial.is_empty())
    {
        return None;
    }
    Some(
        polynomial
            .terms
            .values()
            .fold(Rational::zero(), |accumulator, coefficient| {
                accumulator.checked_add(coefficient).unwrap_or(accumulator)
            }),
    )
}

// ---------------------------------------------------------------------------
// 本地三角规范形
// ---------------------------------------------------------------------------

const MAX_NODES: usize = 600;
const MAX_EXPAND_DEPTH: u8 = 6;

/// 三角表达式规范形（约化 `sin² → 1 − cos²` 后的多项式）。
///
/// # Errors
///
/// 超出展开规模、含非多项式结构（变量分母/根式含变量等）返回中文原因。
pub fn trig_normal_form(expr: &Expr) -> Result<Polynomial, String> {
    let rewritten = to_sin_cos(expr)?;
    let expanded = expand_trig(&rewritten, 0)?;
    let canonical = canonicalize(&expanded);
    if node_count(&canonical) > MAX_NODES {
        return Err("三角展开规模过大（超过 600 个节点）".to_owned());
    }
    let mut atoms: BTreeMap<String, Expr> = BTreeMap::new();
    let substituted = substitute_atoms(&canonical, &mut atoms)?;
    let polynomial = polynomial(&substituted)
        .ok_or_else(|| "三角表达式含变量分母/根式等非多项式结构（本地规则不支持）".to_owned())?;
    Ok(reduce_sin_squares(&polynomial))
}

fn node_count(expr: &Expr) -> usize {
    match expr {
        Expr::Number(_) | Expr::Variable(_) | Expr::Boolean(_) | Expr::Option(_) => 1,
        Expr::Sum(items)
        | Expr::Product(items)
        | Expr::Set(items)
        | Expr::SetUnion(items)
        | Expr::SetIntersection(items)
        | Expr::Vector(items)
        | Expr::Solutions(items) => 1 + items.iter().map(node_count).sum::<usize>(),
        Expr::Power(base, exponent) => 1 + node_count(base) + node_count(exponent),
        Expr::Fraction(numerator, denominator) => {
            1 + node_count(numerator) + node_count(denominator)
        }
        Expr::Radical { degree, radicand } => {
            1 + node_count(radicand) + degree.as_deref().map_or(0, node_count)
        }
        Expr::Absolute(value) | Expr::Quantity { value, .. } => 1 + node_count(value),
        Expr::Function { args, .. } => 1 + args.iter().map(node_count).sum::<usize>(),
        Expr::Matrix(rows) => 1 + rows.iter().flatten().map(node_count).sum::<usize>(),
        Expr::Interval(interval) => {
            1 + interval.start.expr().map_or(0, node_count)
                + interval.end.expr().map_or(0, node_count)
        }
        Expr::Relation { lhs, rhs, .. } => 1 + node_count(lhs) + node_count(rhs),
    }
}

/// 将 tan/cot/sec/csc 改写为 sin/cos 形式（递归，受节点上限保护）。
fn to_sin_cos(expr: &Expr) -> Result<Expr, String> {
    let result = match expr {
        Expr::Function { name, args } if args.len() == 1 => {
            let argument = to_sin_cos(&args[0])?;
            match name.as_str() {
                "tan" => Expr::Fraction(
                    Box::new(sin_of(argument.clone())),
                    Box::new(cos_of(argument)),
                ),
                "cot" => Expr::Fraction(
                    Box::new(cos_of(argument.clone())),
                    Box::new(sin_of(argument)),
                ),
                "sec" => Expr::Fraction(
                    Box::new(Expr::Number(Rational::one())),
                    Box::new(cos_of(argument)),
                ),
                "csc" => Expr::Fraction(
                    Box::new(Expr::Number(Rational::one())),
                    Box::new(sin_of(argument)),
                ),
                _ => Expr::Function {
                    name: name.clone(),
                    args: vec![argument],
                },
            }
        }
        Expr::Function { name, args } => Expr::Function {
            name: name.clone(),
            args: args.iter().map(to_sin_cos).collect::<Result<Vec<_>, _>>()?,
        },
        Expr::Sum(items) => Expr::Sum(
            items
                .iter()
                .map(to_sin_cos)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Expr::Product(items) => Expr::Product(
            items
                .iter()
                .map(to_sin_cos)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Expr::Power(base, exponent) => {
            Expr::Power(Box::new(to_sin_cos(base)?), Box::new(to_sin_cos(exponent)?))
        }
        Expr::Fraction(numerator, denominator) => Expr::Fraction(
            Box::new(to_sin_cos(numerator)?),
            Box::new(to_sin_cos(denominator)?),
        ),
        Expr::Radical { degree, radicand } => Expr::Radical {
            degree: degree.as_deref().map(to_sin_cos).transpose()?.map(Box::new),
            radicand: Box::new(to_sin_cos(radicand)?),
        },
        Expr::Absolute(value) => Expr::Absolute(Box::new(to_sin_cos(value)?)),
        other => other.clone(),
    };
    Ok(result)
}

fn expand_trig(expr: &Expr, depth: u8) -> Result<Expr, String> {
    if depth > MAX_EXPAND_DEPTH {
        return Err("三角展开深度超限（>6 层）".to_owned());
    }
    let expanded = match expr {
        Expr::Function { name, args }
            if args.len() == 1 && matches!(name.as_str(), "sin" | "cos") =>
        {
            let argument = expand_trig(&args[0], depth + 1)?;
            expand_sin_cos(name, argument, depth)?
        }
        Expr::Function { name, args } => Expr::Function {
            name: name.clone(),
            args: args
                .iter()
                .map(|arg| expand_trig(arg, depth + 1))
                .collect::<Result<Vec<_>, _>>()?,
        },
        Expr::Sum(items) => Expr::Sum(
            items
                .iter()
                .map(|item| expand_trig(item, depth + 1))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Expr::Product(items) => Expr::Product(
            items
                .iter()
                .map(|item| expand_trig(item, depth + 1))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Expr::Power(base, exponent) => Expr::Power(
            Box::new(expand_trig(base, depth + 1)?),
            Box::new(expand_trig(exponent, depth + 1)?),
        ),
        Expr::Fraction(numerator, denominator) => Expr::Fraction(
            Box::new(expand_trig(numerator, depth + 1)?),
            Box::new(expand_trig(denominator, depth + 1)?),
        ),
        Expr::Radical { degree, radicand } => Expr::Radical {
            degree: degree
                .as_deref()
                .map(|degree| expand_trig(degree, depth + 1))
                .transpose()?
                .map(Box::new),
            radicand: Box::new(expand_trig(radicand, depth + 1)?),
        },
        Expr::Absolute(value) => Expr::Absolute(Box::new(expand_trig(value, depth + 1)?)),
        other => other.clone(),
    };
    Ok(expanded)
}

fn expand_sin_cos(name: &str, argument: Expr, depth: u8) -> Result<Expr, String> {
    let argument = canonicalize(&argument);
    // 特殊角 0：sin(0)=0、cos(0)=1
    if argument == Expr::Number(Rational::zero()) {
        return Ok(if name == "sin" {
            Expr::Number(Rational::zero())
        } else {
            Expr::Number(Rational::one())
        });
    }
    // 1) 周期/象限约化
    if let Some((new_name, negate, base)) = reduce_angle(name, &argument) {
        let base = expand_trig(&base, depth + 1)?;
        let value = if depth > MAX_EXPAND_DEPTH {
            return Err("三角展开深度超限".to_owned());
        } else {
            expand_sin_cos(&new_name, base, depth + 1)?
        };
        let value = canonicalize(&value);
        return Ok(if negate { neg(value) } else { value });
    }
    // 2) 倍角
    if let Some(inner) = double_angle_inner(&argument) {
        let value = if name == "sin" {
            let two = Expr::Number(Rational::from_i64(2));
            Expr::Product(vec![two, sin_of(inner.clone()), cos_of(inner.clone())])
        } else {
            let sin = Expr::Power(
                Box::new(sin_of(inner.clone())),
                Box::new(Expr::Number(Rational::from_i64(2))),
            );
            let cos = Expr::Power(
                Box::new(cos_of(inner.clone())),
                Box::new(Expr::Number(Rational::from_i64(2))),
            );
            Expr::Sum(vec![cos, neg(sin)])
        };
        return expand_trig(&value, depth + 1);
    }
    // 3) 和角/差角
    if let Some((left, right, sign)) = split_sum(&argument) {
        let left = expand_trig(&left, depth + 1)?;
        let right = expand_trig(&right, depth + 1)?;
        let value = if name == "sin" {
            let first = Expr::Product(vec![sin_of(left.clone()), cos_of(right.clone())]);
            let second = Expr::Product(vec![cos_of(left), sin_of(right)]);
            Expr::Sum(vec![first, with_sign(second, sign)])
        } else {
            let first = Expr::Product(vec![cos_of(left.clone()), cos_of(right.clone())]);
            let second = Expr::Product(vec![sin_of(left), sin_of(right)]);
            Expr::Sum(vec![first, with_sign(second, -sign)])
        };
        return expand_trig(&value, depth + 1);
    }
    Ok(Expr::Function {
        name: name.to_owned(),
        args: vec![argument],
    })
}

fn with_sign(expr: Expr, sign: i8) -> Expr {
    if sign < 0 { neg(expr) } else { expr }
}

/// 约化 `arg = base + k·(π/2)` 到基础区间；返回（新函数名、是否取负、基础参数）。
fn reduce_angle(name: &str, argument: &Expr) -> Option<(String, bool, Expr)> {
    let argument = canonicalize(argument);
    // 奇偶性：-u
    if let Some(inner) = negative_argument(&argument) {
        return match name {
            "sin" => Some(("sin".to_owned(), true, inner)),
            _ => Some(("cos".to_owned(), false, inner)),
        };
    }
    let (pi_coefficient, base) = split_pi_terms(&argument);
    if pi_coefficient.is_zero() {
        return None;
    }
    let doubled = pi_coefficient.checked_mul(&Rational::from_i64(2)).ok()?;
    if !doubled.is_integer() {
        return None;
    }
    let integer = doubled.to_integer().to_string().parse::<i64>().ok()?;
    let step = integer.rem_euclid(4);
    let is_sin = name == "sin";
    let (new_name, negate) = if step == 0 {
        (name, false)
    } else if step == 2 {
        (name, true)
    } else if step == 1 {
        if is_sin {
            ("cos", false)
        } else {
            ("sin", true)
        }
    } else if is_sin {
        ("cos", true)
    } else {
        ("sin", false)
    };
    Some((new_name.to_owned(), negate, base))
}

fn negative_argument(argument: &Expr) -> Option<Expr> {
    match argument {
        Expr::Number(value) if value.is_negative() => Some(Expr::Number(value.abs())),
        Expr::Product(items) => {
            let (first, rest) = items.split_first()?;
            let Expr::Number(coefficient) = first else {
                return None;
            };
            if !coefficient.is_negative() {
                return None;
            }
            let mut factors = rest.to_vec();
            factors.insert(0, Expr::Number(coefficient.abs()));
            Some(Expr::Product(factors))
        }
        _ => None,
    }
}

/// 拆分 `k·π + base`；返回 π 的有理系数与其余项（可能为 0）。
fn split_pi_terms(argument: &Expr) -> (Rational, Expr) {
    let canonical = canonicalize(argument);
    let terms: Vec<Expr> = match &canonical {
        Expr::Sum(items) => items.clone(),
        other => vec![other.clone()],
    };
    let mut pi_coefficient = Rational::zero();
    let mut rest: Vec<Expr> = Vec::new();
    for term in terms {
        match pi_term_coefficient(&term) {
            Some(coefficient) => {
                pi_coefficient = pi_coefficient
                    .checked_add(&coefficient)
                    .unwrap_or_else(|_| pi_coefficient.clone());
            }
            None => rest.push(term),
        }
    }
    let base = match rest.len() {
        0 => Expr::Number(Rational::zero()),
        1 => rest.pop().unwrap_or_else(|| Expr::Number(Rational::zero())),
        _ => Expr::Sum(rest),
    };
    (pi_coefficient, canonicalize(&base))
}

fn pi_term_coefficient(term: &Expr) -> Option<Rational> {
    let (coefficient, pi_count) = pi_factor(term)?;
    (pi_count == 1).then_some(coefficient)
}

/// 拆分「系数 × π^k」：返回（不含 π 的系数，π 的次数）。
fn pi_factor(expr: &Expr) -> Option<(Rational, usize)> {
    match expr {
        Expr::Number(value) => Some((value.clone(), 0)),
        Expr::Variable(name) if name == "π" => Some((Rational::one(), 1)),
        Expr::Fraction(numerator, denominator) => {
            let (numerator_coefficient, pi_count) = pi_factor(numerator)?;
            let denominator_value = evaluate(denominator).ok()?;
            if denominator_value.is_zero() || pi_count != 1 {
                return None;
            }
            let coefficient = numerator_coefficient.checked_div(&denominator_value).ok()?;
            Some((coefficient, 1))
        }
        Expr::Product(items) => {
            let mut coefficient = Rational::one();
            let mut pi_count = 0usize;
            for item in items {
                let (item_coefficient, item_pi_count) = pi_factor(item)?;
                coefficient = coefficient.checked_mul(&item_coefficient).ok()?;
                pi_count += item_pi_count;
            }
            Some((coefficient, pi_count))
        }
        _ => None,
    }
}

/// `2u` / `u·2`（整数系数 2，恰两个因子） → `u`。
fn double_angle_inner(argument: &Expr) -> Option<Expr> {
    let Expr::Product(items) = argument else {
        return None;
    };
    if items.len() != 2 {
        return None;
    }
    let (first, second) = (&items[0], &items[1]);
    match (first, second) {
        (Expr::Number(value), other)
            if value.compare(&Rational::from_i64(2)) == core::cmp::Ordering::Equal =>
        {
            Some(other.clone())
        }
        (other, Expr::Number(value))
            if value.compare(&Rational::from_i64(2)) == core::cmp::Ordering::Equal =>
        {
            Some(other.clone())
        }
        _ => None,
    }
}

/// 拆成 `a + b`（b 可带负号）；多于 2 项时拆成 `term0 + 其余`。
fn split_sum(argument: &Expr) -> Option<(Expr, Expr, i8)> {
    let Expr::Sum(items) = argument else {
        return None;
    };
    if items.len() < 2 {
        return None;
    }
    let first = items[0].clone();
    let rest = if items.len() == 2 {
        items[1].clone()
    } else {
        Expr::Sum(items[1..].to_vec())
    };
    let (right, sign) = match rest {
        Expr::Number(value) if value.is_negative() => (Expr::Number(value.abs()), -1),
        Expr::Product(mut factors) => {
            let negative =
                matches!(factors.first(), Some(Expr::Number(value)) if value.is_negative());
            if negative {
                if let Some(Expr::Number(value)) = factors.first_mut() {
                    *value = value.abs();
                }
                (Expr::Product(factors), -1)
            } else {
                (Expr::Product(factors), 1)
            }
        }
        other => (other, 1),
    };
    Some((first, right, sign))
}

fn sin_of(argument: Expr) -> Expr {
    Expr::Function {
        name: "sin".to_owned(),
        args: vec![argument],
    }
}

fn cos_of(argument: Expr) -> Expr {
    Expr::Function {
        name: "cos".to_owned(),
        args: vec![argument],
    }
}

fn neg(expr: Expr) -> Expr {
    match expr {
        Expr::Number(value) => Expr::Number(value.neg()),
        other => Expr::Product(vec![Expr::Number(Rational::from_i64(-1)), other]),
    }
}

/// 把 sin/cos 原子替换为独立变量（其他函数视为不透明原子），便于多项式规范化。
fn substitute_atoms(expr: &Expr, atoms: &mut BTreeMap<String, Expr>) -> Result<Expr, String> {
    Ok(match expr {
        Expr::Number(_) | Expr::Variable(_) => expr.clone(),
        Expr::Sum(items) => Expr::Sum(
            items
                .iter()
                .map(|item| substitute_atoms(item, atoms))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Expr::Product(items) => Expr::Product(
            items
                .iter()
                .map(|item| substitute_atoms(item, atoms))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Expr::Power(base, exponent) => Expr::Power(
            Box::new(substitute_atoms(base, atoms)?),
            Box::new(substitute_atoms(exponent, atoms)?),
        ),
        Expr::Fraction(numerator, denominator) => Expr::Fraction(
            Box::new(substitute_atoms(numerator, atoms)?),
            Box::new(substitute_atoms(denominator, atoms)?),
        ),
        Expr::Radical { degree, radicand } => Expr::Radical {
            degree: degree
                .as_deref()
                .map(|degree| substitute_atoms(degree, atoms))
                .transpose()?
                .map(Box::new),
            radicand: Box::new(substitute_atoms(radicand, atoms)?),
        },
        Expr::Absolute(value) => Expr::Absolute(Box::new(substitute_atoms(value, atoms)?)),
        Expr::Function { name, args }
            if args.len() == 1 && matches!(name.as_str(), "sin" | "cos") =>
        {
            let key = format!("__{name}:{}", args[0].to_latex());
            atoms
                .entry(key.clone())
                .or_insert_with(|| Expr::Variable(key.clone()));
            Expr::Variable(key)
        }
        Expr::Function { .. } | Expr::Quantity { .. } => {
            let key = format!("__a:{}", expr.to_latex());
            atoms
                .entry(key.clone())
                .or_insert_with(|| Expr::Variable(key.clone()));
            Expr::Variable(key)
        }
        Expr::Boolean(_)
        | Expr::Option(_)
        | Expr::Vector(_)
        | Expr::Matrix(_)
        | Expr::Interval(_)
        | Expr::Set(_)
        | Expr::SetUnion(_)
        | Expr::SetIntersection(_)
        | Expr::Relation { .. }
        | Expr::Solutions(_) => {
            return Err(format!("非标量节点无法进入三角规范形：{}", expr.to_text()));
        }
    })
}

/// 约化 `sin² → 1 − cos²`，得到关于 `cos`（与不透明原子）的多项式。
fn reduce_sin_squares(polynomial: &Polynomial) -> Polynomial {
    let mut terms: BTreeMap<crate::canonical::Monomial, Rational> = BTreeMap::new();
    for (monomial, coefficient) in &polynomial.terms {
        reduce_term(monomial.clone(), coefficient.clone(), &mut terms);
    }
    terms.retain(|_, coefficient| !coefficient.is_zero());
    Polynomial { terms }
}

fn reduce_term(
    monomial: crate::canonical::Monomial,
    coefficient: Rational,
    output: &mut BTreeMap<crate::canonical::Monomial, Rational>,
) {
    // 找到指数 ≥2 的 sin 变量
    let mut target: Option<(String, u32)> = None;
    for (name, exponent) in &monomial {
        if name.starts_with("__sin:") && *exponent >= 2 {
            target = Some((name.clone(), *exponent));
            break;
        }
    }
    let Some((sin_name, exponent)) = target else {
        let entry = output.entry(monomial).or_insert_with(Rational::zero);
        *entry = entry
            .checked_add(&coefficient)
            .unwrap_or_else(|_| entry.clone());
        return;
    };
    let cos_name = sin_name.replacen("__sin:", "__cos:", 1);
    // sin² → 1 - cos²
    let mut base = monomial;
    if exponent == 2 {
        base.remove(&sin_name);
    } else {
        base.insert(sin_name, exponent - 2);
    }
    // 常量项：coeff * base
    reduce_term(base.clone(), coefficient.clone(), output);
    // cos² 项：-coeff * base * cos²
    let mut with_cos = base;
    let entry = with_cos.entry(cos_name).or_insert(0);
    *entry = entry.saturating_add(2);
    reduce_term(with_cos, coefficient.neg(), output);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::parse::parse_expr;

    fn expr(input: &str) -> Expr {
        canonicalize(&parse_expr(input).unwrap())
    }

    fn form(input: &str) -> Polynomial {
        trig_normal_form(&expr(input)).unwrap()
    }

    #[test]
    fn pythagorean_identity() {
        assert_eq!(form("\\sin(x)^2+\\cos(x)^2"), form("1"));
        assert_ne!(form("\\sin(x)^2+\\cos(x)^2"), form("0"));
    }

    #[test]
    fn double_angle_and_sum_formulas() {
        assert_eq!(form("\\sin(2*x)"), form("2*\\sin(x)*\\cos(x)"));
        assert_eq!(form("\\cos(2*x)"), form("\\cos(x)^2-\\sin(x)^2"));
        assert_eq!(
            form("\\sin(x+y)"),
            form("\\sin(x)*\\cos(y)+\\cos(x)*\\sin(y)")
        );
        assert_eq!(
            form("\\cos(x-y)"),
            form("\\cos(x)*\\cos(y)+\\sin(x)*\\sin(y)")
        );
    }

    #[test]
    fn periodicity_and_parity() {
        assert_eq!(form("\\sin(x+2*\\pi)"), form("\\sin(x)"));
        assert_eq!(form("\\sin(x+\\pi)"), form("-\\sin(x)"));
        assert_eq!(form("\\cos(x+\\pi)"), form("-\\cos(x)"));
        assert_eq!(form("\\sin(-x)"), form("-\\sin(x)"));
        assert_eq!(form("\\cos(-x)"), form("\\cos(x)"));
        assert_eq!(form("\\sin(x+\\pi/2)"), form("\\cos(x)"));
    }

    #[test]
    fn unsupported_forms_report_errors() {
        assert!(trig_normal_form(&expr("\\tan(x)*\\cos(x)")).is_err());
        assert!(trig_normal_form(&expr("\\sin(x)^{-1}")).is_err());
    }
}
