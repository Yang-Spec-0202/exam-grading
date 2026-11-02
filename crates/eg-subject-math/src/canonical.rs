//! 语法树规范化：常数折叠、合并同类项、展开、根式化简、多项式/解集辅助。
//!
//! 规范依据：`SPEC-subject-math.md` §3 第 4 层「结构层」、§4「和/积/幂：规范化：
//! 常数折叠、合并同类项、展开」「根式：化简：提取完全平方因子」、§5 比较器
//! 第 6 项「表达式规范形（展开、合并同类项、常数折叠后语法树相等）」。
//!
//! 所有运算都走 [`Rational`]（精确有理数）；位长超限时不折叠、保留结构，
//! 由比较器返回「无法判定」，绝不静默近似。
//!
//! T26/T27 扩展点：多项式/根式规范化是纯函数，可被 CAS 适配层补充（如因式分解、
//! 三角恒等）；当前超出能力时统一返回 [`SolveError`] 或 `None`。

use core::cmp::Ordering;
use std::collections::BTreeMap;

use crate::ast::{Expr, Interval, IntervalEnd, RelOp, Unit};
use crate::number::{NumError, Rational};

/// 正则化最大迭代次数（保证收敛，防御异常结构）。
const MAX_PASSES: usize = 8;

/// 根式化简时允许试除的最大被开方数（超出则不化简，避免误判）。
const MAX_RADICAL_FACTOR: u64 = 1_000_000_000;

/// 单项式：变量名 → 非负整数指数。
pub type Monomial = BTreeMap<String, u32>;

/// 多项式：单项式 → 有理系数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Polynomial {
    /// 项表（零系数的项不存）。
    pub terms: BTreeMap<Monomial, Rational>,
}

impl Polynomial {
    /// 零多项式。
    #[must_use]
    pub fn zero() -> Self {
        Self {
            terms: BTreeMap::new(),
        }
    }

    /// 常数多项式。
    #[must_use]
    pub fn constant(value: Rational) -> Self {
        let mut terms = BTreeMap::new();
        if !value.is_zero() {
            terms.insert(Monomial::new(), value);
        }
        Self { terms }
    }

    /// 单变量多项式。
    #[must_use]
    pub fn variable(name: &str) -> Self {
        let mut monomial = Monomial::new();
        monomial.insert(name.to_owned(), 1);
        let mut terms = BTreeMap::new();
        terms.insert(monomial, Rational::one());
        Self { terms }
    }

    /// 是否为零多项式。
    #[must_use]
    pub fn is_zero(&self) -> bool {
        self.terms.values().all(Rational::is_zero)
    }

    /// 加。
    #[must_use]
    pub fn add(&self, other: &Self) -> Self {
        let mut terms = self.terms.clone();
        for (monomial, coefficient) in &other.terms {
            let entry = terms.entry(monomial.clone()).or_insert_with(Rational::zero);
            *entry = entry
                .checked_add(coefficient)
                .unwrap_or_else(|_| entry.clone());
        }
        terms.retain(|_, coefficient| !coefficient.is_zero());
        Self { terms }
    }

    /// 减。
    #[must_use]
    pub fn sub(&self, other: &Self) -> Self {
        self.add(&other.neg())
    }

    /// 取负。
    #[must_use]
    pub fn neg(&self) -> Self {
        Self {
            terms: self
                .terms
                .iter()
                .map(|(monomial, coefficient)| (monomial.clone(), coefficient.neg()))
                .collect(),
        }
    }

    /// 乘。
    #[must_use]
    pub fn mul(&self, other: &Self) -> Self {
        let mut terms: BTreeMap<Monomial, Rational> = BTreeMap::new();
        for (left_monomial, left_coefficient) in &self.terms {
            for (right_monomial, right_coefficient) in &other.terms {
                let mut monomial = left_monomial.clone();
                for (name, exponent) in right_monomial {
                    let entry = monomial.entry(name.clone()).or_insert(0);
                    *entry = entry.saturating_add(*exponent);
                }
                let product = left_coefficient
                    .checked_mul(right_coefficient)
                    .unwrap_or_else(|_| Rational::zero());
                let entry = terms.entry(monomial).or_insert_with(Rational::zero);
                *entry = entry
                    .checked_add(&product)
                    .unwrap_or_else(|_| entry.clone());
            }
        }
        terms.retain(|_, coefficient| !coefficient.is_zero());
        Self { terms }
    }

    /// 乘有理数。
    #[must_use]
    pub fn scale(&self, factor: &Rational) -> Self {
        let mut terms = BTreeMap::new();
        if factor.is_zero() {
            return Self::zero();
        }
        for (monomial, coefficient) in &self.terms {
            let scaled = coefficient
                .checked_mul(factor)
                .unwrap_or_else(|_| coefficient.clone());
            terms.insert(monomial.clone(), scaled);
        }
        Self { terms }
    }

    /// 非负整数次幂。
    #[must_use]
    pub fn pow(&self, exponent: u32) -> Self {
        let mut result = Self::constant(Rational::one());
        for _ in 0..exponent {
            result = result.mul(self);
        }
        result
    }

    /// 变量集合（按字典序）。
    #[must_use]
    pub fn variables(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for monomial in self.terms.keys() {
            for name in monomial.keys() {
                if !names.contains(name) {
                    names.push(name.clone());
                }
            }
        }
        names.sort();
        names
    }

    /// 单变量次数（多变量/零多项式返回 `None`）。
    #[must_use]
    pub fn single_variable_degree(&self, variable: &str) -> Option<u32> {
        let mut degree = 0;
        for monomial in self.terms.keys() {
            for (name, exponent) in monomial {
                if name != variable {
                    return None;
                }
                degree = degree.max(*exponent);
            }
        }
        Some(degree)
    }

    /// 取某变量的系数（按次数）。
    #[must_use]
    pub fn coefficient(&self, variable: &str, degree: u32) -> Rational {
        let mut total = Rational::zero();
        for (monomial, coefficient) in &self.terms {
            if monomial.get(variable).copied().unwrap_or(0) != degree {
                continue;
            }
            let mut matches = true;
            for (name, exponent) in monomial {
                if name == variable {
                    continue;
                }
                if *exponent != 0 {
                    matches = false;
                    break;
                }
            }
            if matches {
                total = total
                    .checked_add(coefficient)
                    .unwrap_or_else(|_| total.clone());
            }
        }
        total
    }

    /// 转为语法树（用于渲染/对比）。
    #[must_use]
    pub fn to_expr(&self) -> Expr {
        let mut terms: Vec<Expr> = Vec::new();
        for (monomial, coefficient) in &self.terms {
            let mut factors: Vec<Expr> = Vec::new();
            if !coefficient.is_one() {
                factors.push(Expr::Number(coefficient.clone()));
            }
            for (name, exponent) in monomial {
                match exponent {
                    0 => {}
                    1 => factors.push(Expr::Variable(name.clone())),
                    _ => factors.push(Expr::Power(
                        Box::new(Expr::Variable(name.clone())),
                        Box::new(Expr::Number(Rational::from_i64(i64::from(*exponent)))),
                    )),
                }
            }
            let term = match factors.len() {
                0 => Expr::Number(coefficient.clone()),
                1 => factors.remove(0),
                _ => Expr::Product(factors),
            };
            terms.push(term);
        }
        match terms.len() {
            0 => Expr::Number(Rational::zero()),
            1 => terms
                .pop()
                .unwrap_or_else(|| Expr::Number(Rational::zero())),
            _ => Expr::Sum(terms),
        }
    }
}

/// 从语法树提取多项式；非多项式（含变量分母/根式/函数/绝对值等）返回 `None`。
#[must_use]
pub fn polynomial(expr: &Expr) -> Option<Polynomial> {
    match expr {
        Expr::Number(value) => Some(Polynomial::constant(value.clone())),
        Expr::Variable(name) => Some(Polynomial::variable(name)),
        Expr::Sum(items) => {
            let mut total = Polynomial::zero();
            for item in items {
                total = total.add(&polynomial(item)?);
            }
            Some(total)
        }
        Expr::Product(items) => {
            let mut total = Polynomial::constant(Rational::one());
            for item in items {
                total = total.mul(&polynomial(item)?);
            }
            Some(total)
        }
        Expr::Power(base, exponent) => {
            let base = polynomial(base)?;
            let Expr::Number(exponent) = exponent.as_ref() else {
                return None;
            };
            if !exponent.is_integer() || exponent.is_negative() {
                return None;
            }
            let degree =
                u32::try_from(exponent.to_integer().to_string().parse::<i64>().ok()?).ok()?;
            if degree > 64 {
                return None;
            }
            Some(base.pow(degree))
        }
        Expr::Fraction(numerator, denominator) => {
            let numerator = polynomial(numerator)?;
            let denominator = polynomial(denominator)?;
            if denominator.variables().is_empty() {
                let constant = denominator.coefficient("", 0);
                if constant.is_zero() {
                    return None;
                }
                let reciprocal = Rational::one().checked_div(&constant).ok()?;
                return Some(numerator.scale(&reciprocal));
            }
            None
        }
        Expr::Quantity { .. }
        | Expr::Radical { .. }
        | Expr::Absolute(_)
        | Expr::Function { .. }
        | Expr::Vector(_)
        | Expr::Matrix(_)
        | Expr::Interval(_)
        | Expr::Set(_)
        | Expr::SetUnion(_)
        | Expr::SetIntersection(_)
        | Expr::Relation { .. }
        | Expr::Solutions(_)
        | Expr::Boolean(_)
        | Expr::Option(_) => None,
    }
}

/// 两表达式是否为同一多项式（展开/合并同类项后），无法判定时返回 `None`。
#[must_use]
pub fn polynomial_equality(left: &Expr, right: &Expr) -> Option<bool> {
    let left = polynomial(left)?;
    let right = polynomial(right)?;
    Some(left.sub(&right).is_zero())
}

/// 求值错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EvalError {
    /// 含变量/函数/带单位/关系等，无法作为单一数值求值。
    #[error("不是可求值的常量表达式：{reason}")]
    NonConstant {
        /// 可读原因。
        reason: String,
    },
    /// 除数为 0。
    #[error("除数为 0")]
    DivideByZero,
    /// 根式不能精确求值（非完全平方/完全 n 次幂）。
    #[error("根式不能精确求值：{reason}")]
    Inexact {
        /// 可读原因。
        reason: String,
    },
    /// 数值超出精确表示范围。
    #[error("数值超出精确表示范围：{source}")]
    Num {
        /// 底层数值错误。
        source: NumError,
    },
}

impl From<NumError> for EvalError {
    fn from(error: NumError) -> Self {
        match error {
            NumError::DivideByZero => Self::DivideByZero,
            other => Self::Num { source: other },
        }
    }
}

/// 对常量表达式求精确值。
///
/// # Errors
///
/// 含变量/函数/单位/关系等返回 [`EvalError::NonConstant`]；除零或非精确根式返回
/// 对应错误。
pub fn evaluate(expr: &Expr) -> Result<Rational, EvalError> {
    match expr {
        Expr::Number(value) => Ok(value.clone()),
        Expr::Sum(items) => {
            let mut total = Rational::zero();
            for item in items {
                total = total.checked_add(&evaluate(item)?)?;
            }
            Ok(total)
        }
        Expr::Product(items) => {
            let mut total = Rational::one();
            for item in items {
                total = total.checked_mul(&evaluate(item)?)?;
            }
            Ok(total)
        }
        Expr::Power(base, exponent) => {
            let base = evaluate(base)?;
            let Expr::Number(exponent) = exponent.as_ref() else {
                return Err(EvalError::NonConstant {
                    reason: "指数含变量".to_owned(),
                });
            };
            if !exponent.is_integer() {
                return Err(EvalError::NonConstant {
                    reason: "指数不是整数（非有理数次幂超出 T14 范围）".to_owned(),
                });
            }
            let exponent = exponent
                .to_integer()
                .to_string()
                .parse::<i32>()
                .map_err(|_| EvalError::NonConstant {
                    reason: "指数超出 i32 范围".to_owned(),
                })?;
            Ok(base.checked_pow(exponent)?)
        }
        Expr::Fraction(numerator, denominator) => {
            let numerator = evaluate(numerator)?;
            let denominator = evaluate(denominator)?;
            Ok(numerator.checked_div(&denominator)?)
        }
        Expr::Absolute(value) => Ok(evaluate(value)?.abs()),
        Expr::Radical { degree, radicand } => {
            let radicand = evaluate(radicand)?;
            let degree = match degree {
                None => 2,
                Some(degree) => {
                    let value = evaluate(degree)?;
                    if !value.is_integer() {
                        return Err(EvalError::Inexact {
                            reason: "根指数不是整数".to_owned(),
                        });
                    }
                    u32::try_from(value.to_integer().to_string().parse::<i64>().map_err(|_| {
                        EvalError::Inexact {
                            reason: "根指数超出范围".to_owned(),
                        }
                    })?)
                    .map_err(|_| EvalError::Inexact {
                        reason: "根指数超出范围".to_owned(),
                    })?
                }
            };
            radicand
                .exact_nth_root(degree)
                .ok_or_else(|| EvalError::Inexact {
                    reason: format!("被开方数不是完全 {degree} 次幂"),
                })
        }
        Expr::Variable(_) => Err(EvalError::NonConstant {
            reason: "含变量".to_owned(),
        }),
        Expr::Quantity { .. } => Err(EvalError::NonConstant {
            reason: "带单位数值需单位比较器".to_owned(),
        }),
        Expr::Function { name, .. } => Err(EvalError::NonConstant {
            reason: format!("含函数 {name}（超出 T14 范围）"),
        }),
        Expr::Vector(_)
        | Expr::Matrix(_)
        | Expr::Interval(_)
        | Expr::Set(_)
        | Expr::SetUnion(_)
        | Expr::SetIntersection(_) => Err(EvalError::NonConstant {
            reason: "不是标量表达式".to_owned(),
        }),
        Expr::Relation { .. } | Expr::Solutions(_) => Err(EvalError::NonConstant {
            reason: "关系/多解表达式需解集比较器".to_owned(),
        }),
        Expr::Boolean(_) | Expr::Option(_) => Err(EvalError::NonConstant {
            reason: "布尔/选项需专用比较器".to_owned(),
        }),
    }
}

/// 解集/区间转换错误（一律映射为「无法判定」）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SolveError {
    /// 超出 T14 能精确处理的范围。
    #[error("{reason}")]
    Undecidable {
        /// 可读原因（必须说明为什么无法判定）。
        reason: String,
    },
}

impl SolveError {
    fn new(reason: impl Into<String>) -> Self {
        Self::Undecidable {
            reason: reason.into(),
        }
    }
}

/// 一元一次不等式/等式 → 区间（区间与不等式互通，规范 §5 第 9 项）。
///
/// # Errors
///
/// 关系含多变量、次数 >1、`≠`（并集）或非多项式时返回 [`SolveError`]。
pub fn relation_to_interval(expr: &Expr) -> Result<Interval, SolveError> {
    let Expr::Relation { op, lhs, rhs } = expr else {
        return Err(SolveError::new("不是方程/不等式"));
    };
    let left = polynomial(lhs).ok_or_else(|| SolveError::new("左侧不是多项式"))?;
    let right = polynomial(rhs).ok_or_else(|| SolveError::new("右侧不是多项式"))?;
    let difference = left.sub(&right);
    let variables = difference.variables();
    if variables.is_empty() {
        return Ok(constant_relation_interval(&difference, *op));
    }
    if variables.len() > 1 {
        return Err(SolveError::new("不等式含多个变量（T14 只做一元）"));
    }
    let variable = &variables[0];
    let degree = difference
        .single_variable_degree(variable)
        .ok_or_else(|| SolveError::new("不等式不是单变量多项式"))?;
    if degree > 1 {
        return Err(SolveError::new(
            "不等式次数 >1（二次不等式解集为并集，T26 后续）",
        ));
    }
    let a = difference.coefficient(variable, 1);
    let b = difference.coefficient(variable, 0);
    if a.is_zero() {
        return Ok(constant_relation_interval(&difference, *op));
    }
    let boundary = b
        .checked_div(&a)
        .map_err(|_| SolveError::new("端点计算失败"))?
        .neg();
    let effective_op = if a.is_negative() { op.mirror() } else { *op };
    let endpoint = IntervalEnd::Value {
        expr: Box::new(Expr::Number(boundary)),
        closed: matches!(effective_op, RelOp::Le | RelOp::Ge | RelOp::Eq),
    };
    match effective_op {
        RelOp::Eq => Ok(Interval::new(endpoint.clone(), endpoint)),
        RelOp::Le | RelOp::Lt => Ok(Interval::new(IntervalEnd::NegInfinity, endpoint)),
        RelOp::Ge | RelOp::Gt => Ok(Interval::new(endpoint, IntervalEnd::PosInfinity)),
        RelOp::Ne => Err(SolveError::new("≠ 的解集是并集，T14 不支持")),
    }
}

fn constant_relation_interval(difference: &Polynomial, op: RelOp) -> Interval {
    let constant = difference.coefficient("", 0);
    let truth = match op {
        RelOp::Eq => constant.is_zero(),
        RelOp::Ne => !constant.is_zero(),
        RelOp::Lt => constant.is_negative(),
        RelOp::Le => constant.is_negative() || constant.is_zero(),
        RelOp::Gt => !constant.is_negative() && !constant.is_zero(),
        RelOp::Ge => !constant.is_negative(),
    };
    if truth {
        Interval::new(IntervalEnd::NegInfinity, IntervalEnd::PosInfinity)
    } else {
        // 空集用同端点开区间表示
        Interval::new(
            IntervalEnd::Value {
                expr: Box::new(Expr::Number(Rational::zero())),
                closed: true,
            },
            IntervalEnd::Value {
                expr: Box::new(Expr::Number(Rational::zero())),
                closed: false,
            },
        )
    }
}

/// 一元方程（等式）的精确有理数解集。
///
/// # Errors
///
/// 恒等式、次数 >2、判别式为负平方数以外的无理根等返回 [`SolveError`]。
pub fn equation_solutions(expr: &Expr) -> Result<Vec<Rational>, SolveError> {
    let Expr::Relation { op, lhs, rhs } = expr else {
        return Err(SolveError::new("不是方程"));
    };
    if *op != RelOp::Eq {
        return Err(SolveError::new("解集比较只支持等式"));
    }
    let left = polynomial(lhs).ok_or_else(|| SolveError::new("方程左侧不是多项式"))?;
    let right = polynomial(rhs).ok_or_else(|| SolveError::new("方程右侧不是多项式"))?;
    let difference = left.sub(&right);
    let variables = difference.variables();
    if variables.is_empty() {
        return Err(SolveError::new("方程不含变量，解集不是有限集合"));
    }
    if variables.len() > 1 {
        return Err(SolveError::new("方程含多个变量（T14 只做一元）"));
    }
    let variable = &variables[0];
    let degree = difference
        .single_variable_degree(variable)
        .ok_or_else(|| SolveError::new("方程不是单变量多项式"))?;
    let a2 = difference.coefficient(variable, 2);
    let a1 = difference.coefficient(variable, 1);
    let a0 = difference.coefficient(variable, 0);
    match degree {
        0 => Err(SolveError::new("方程不含变量")),
        1 => {
            if a1.is_zero() {
                return Err(SolveError::new("方程退化"));
            }
            let root = a0
                .checked_div(&a1)
                .map_err(|_| SolveError::new("根计算失败"))?
                .neg();
            Ok(vec![root])
        }
        2 => {
            if a2.is_zero() {
                return Err(SolveError::new("二次项系数为 0"));
            }
            let four = Rational::from_i64(4);
            let discriminant = a1
                .checked_mul(&a1)?
                .checked_sub(&four.checked_mul(&a2)?.checked_mul(&a0)?)?;
            if discriminant.is_negative() {
                return Ok(Vec::new());
            }
            if discriminant.is_zero() {
                let two_a = a2.checked_mul(&Rational::from_i64(2))?;
                let root = a1
                    .neg()
                    .checked_div(&two_a)
                    .map_err(|_| SolveError::new("根计算失败"))?;
                return Ok(vec![root]);
            }
            let sqrt = discriminant
                .exact_sqrt()
                .ok_or_else(|| SolveError::new("解集含无理根（需 CAS，T27 后续）"))?;
            let two_a = a2.checked_mul(&Rational::from_i64(2))?;
            let first = a1
                .neg()
                .checked_add(&sqrt)?
                .checked_div(&two_a)
                .map_err(|_| SolveError::new("根计算失败"))?;
            let second = a1
                .neg()
                .checked_sub(&sqrt)?
                .checked_div(&two_a)
                .map_err(|_| SolveError::new("根计算失败"))?;
            let mut roots = vec![first, second];
            roots.sort();
            roots.dedup();
            Ok(roots)
        }
        _ => Err(SolveError::new(
            "方程次数 >2（超出 T14 范围，T26/T27 扩展）",
        )),
    }
}

impl From<NumError> for SolveError {
    fn from(error: NumError) -> Self {
        Self::Undecidable {
            reason: format!("数值运算失败：{error}"),
        }
    }
}

// ---------------------------------------------------------------------------
// 规范化
// ---------------------------------------------------------------------------

/// 规范化语法树（幂等：再次规范化不再变化）。
#[must_use]
pub fn canonicalize(expr: &Expr) -> Expr {
    let mut current = canonicalize_once(expr);
    for _ in 0..MAX_PASSES {
        let next = canonicalize_once(&current);
        if next == current {
            return next;
        }
        current = next;
    }
    current
}

fn canonicalize_once(expr: &Expr) -> Expr {
    match expr {
        Expr::Number(_) | Expr::Variable(_) | Expr::Boolean(_) | Expr::Option(_) => expr.clone(),
        Expr::Sum(items) => {
            let items: Vec<Expr> = items.iter().map(canonicalize_once).collect();
            normalize_sum(items)
        }
        Expr::Product(items) => {
            let items: Vec<Expr> = items.iter().map(canonicalize_once).collect();
            normalize_product(items)
        }
        Expr::Power(base, exponent) => {
            let base = canonicalize_once(base);
            let exponent = canonicalize_once(exponent);
            normalize_power(base, exponent)
        }
        Expr::Fraction(numerator, denominator) => {
            let numerator = canonicalize_once(numerator);
            let denominator = canonicalize_once(denominator);
            normalize_fraction(numerator, denominator)
        }
        Expr::Radical { degree, radicand } => {
            let degree = degree.as_ref().map(|degree| canonicalize_once(degree));
            let radicand = canonicalize_once(radicand);
            normalize_radical(degree, radicand)
        }
        Expr::Absolute(value) => {
            let value = canonicalize_once(value);
            match value {
                Expr::Number(number) => Expr::Number(number.abs()),
                other => Expr::Absolute(Box::new(other)),
            }
        }
        Expr::Quantity { value, unit } => Expr::Quantity {
            value: Box::new(canonicalize_once(value)),
            unit: unit.clone(),
        },
        Expr::Function { name, args } => Expr::Function {
            name: name.clone(),
            args: args.iter().map(canonicalize_once).collect(),
        },
        Expr::Vector(items) => Expr::Vector(items.iter().map(canonicalize_once).collect()),
        Expr::Matrix(rows) => Expr::Matrix(
            rows.iter()
                .map(|row| row.iter().map(canonicalize_once).collect())
                .collect(),
        ),
        Expr::Interval(interval) => Expr::Interval(Interval::new(
            canonicalize_end(&interval.start),
            canonicalize_end(&interval.end),
        )),
        Expr::Set(items) => {
            let mut items: Vec<Expr> = items.iter().map(canonicalize_once).collect();
            items.sort();
            items.dedup();
            Expr::Set(items)
        }
        Expr::SetUnion(items) => canonicalize_set_op(items, true),
        Expr::SetIntersection(items) => canonicalize_set_op(items, false),
        Expr::Relation { op, lhs, rhs } => Expr::Relation {
            op: *op,
            lhs: Box::new(canonicalize_once(lhs)),
            rhs: Box::new(canonicalize_once(rhs)),
        },
        Expr::Solutions(items) => {
            let mut items: Vec<Expr> = items.iter().map(canonicalize_once).collect();
            items.sort();
            items.dedup();
            Expr::Solutions(items)
        }
    }
}

fn canonicalize_end(end: &IntervalEnd) -> IntervalEnd {
    match end {
        IntervalEnd::NegInfinity => IntervalEnd::NegInfinity,
        IntervalEnd::PosInfinity => IntervalEnd::PosInfinity,
        IntervalEnd::Value { expr, closed } => IntervalEnd::Value {
            expr: Box::new(canonicalize_once(expr)),
            closed: *closed,
        },
    }
}

fn canonicalize_set_op(items: &[Expr], union: bool) -> Expr {
    let mut flat: Vec<Expr> = Vec::new();
    for item in items {
        let canonical = canonicalize_once(item);
        match (&canonical, union) {
            (Expr::SetUnion(children), true) | (Expr::SetIntersection(children), false) => {
                flat.extend(children.iter().cloned());
            }
            _ => flat.push(canonical),
        }
    }
    flat.sort();
    flat.dedup();
    match flat.len() {
        0 => Expr::Set(Vec::new()),
        1 => flat.pop().unwrap_or_else(|| Expr::Set(Vec::new())),
        _ => {
            if union {
                Expr::SetUnion(flat)
            } else {
                Expr::SetIntersection(flat)
            }
        }
    }
}

fn normalize_sum(items: Vec<Expr>) -> Expr {
    let mut constant = Rational::zero();
    let mut groups: BTreeMap<Expr, Rational> = BTreeMap::new();
    let mut fallback: Vec<Expr> = Vec::new();
    for item in items {
        match split_term(item.clone()) {
            Some(Term::Constant(value)) => {
                constant = constant
                    .checked_add(&value)
                    .unwrap_or_else(|_| constant.clone());
            }
            Some(Term::Term { coefficient, base }) => {
                let entry = groups.entry(base).or_insert_with(Rational::zero);
                *entry = entry
                    .checked_add(&coefficient)
                    .unwrap_or_else(|_| entry.clone());
            }
            None => fallback.push(item),
        }
    }
    let mut terms: Vec<Expr> = Vec::new();
    for (base, coefficient) in groups {
        if coefficient.is_zero() {
            continue;
        }
        if coefficient.is_one() {
            terms.push(base);
        } else {
            terms.push(Expr::Product(vec![Expr::Number(coefficient), base]));
        }
    }
    terms.extend(fallback);
    // 按次数降序排列（x^2 在 2x 之前），同次按结构序，常数项放最后
    terms.sort_by(|left, right| {
        term_degree(right)
            .cmp(&term_degree(left))
            .then_with(|| left.cmp(right))
    });
    if !constant.is_zero() || terms.is_empty() {
        terms.push(Expr::Number(constant));
    }
    match terms.len() {
        1 => terms
            .pop()
            .unwrap_or_else(|| Expr::Number(Rational::zero())),
        _ => Expr::Sum(terms),
    }
}

enum Term {
    Constant(Rational),
    Term { coefficient: Rational, base: Expr },
}

fn term_degree(expr: &Expr) -> u32 {
    match expr {
        Expr::Variable(_) => 1,
        Expr::Power(_, exponent) => match exponent.as_ref() {
            Expr::Number(value) if value.is_integer() => {
                u32::try_from(parse_i32(value)).unwrap_or(0)
            }
            _ => 1,
        },
        Expr::Product(items) => items.iter().map(term_degree).sum(),
        _ => 0,
    }
}

fn split_term(item: Expr) -> Option<Term> {
    match item {
        Expr::Number(value) => Some(Term::Constant(value)),
        Expr::Product(factors) => {
            let mut coefficient = Rational::one();
            let mut rest: Vec<Expr> = Vec::new();
            for factor in factors {
                match factor {
                    Expr::Number(value) => {
                        coefficient = coefficient.checked_mul(&value).ok()?;
                    }
                    other => rest.push(other),
                }
            }
            if rest.is_empty() {
                Some(Term::Constant(coefficient))
            } else {
                Some(Term::Term {
                    coefficient,
                    base: match rest.len() {
                        1 => rest.pop()?,
                        _ => Expr::Product(rest),
                    },
                })
            }
        }
        other => Some(Term::Term {
            coefficient: Rational::one(),
            base: other,
        }),
    }
}

fn normalize_product(items: Vec<Expr>) -> Expr {
    // 展开：积中含和式时先分配（`(x+1)(x-1) → x^2-1`）
    if items.iter().any(|item| matches!(item, Expr::Sum(_))) {
        let mut expanded: Vec<Expr> = vec![Expr::Number(Rational::one())];
        for factor in items {
            let next: Vec<Expr> = match factor {
                Expr::Sum(terms) => {
                    let mut next = Vec::with_capacity(expanded.len() * terms.len());
                    for accumulated in &expanded {
                        for term in &terms {
                            next.push(Expr::Product(vec![accumulated.clone(), term.clone()]));
                        }
                    }
                    next
                }
                other => expanded
                    .iter()
                    .map(|accumulated| Expr::Product(vec![accumulated.clone(), other.clone()]))
                    .collect(),
            };
            expanded = next;
        }
        return match expanded.len() {
            0 => Expr::Number(Rational::zero()),
            1 => expanded
                .pop()
                .unwrap_or_else(|| Expr::Number(Rational::one())),
            _ => Expr::Sum(expanded),
        };
    }
    let mut coefficient = Rational::one();
    let mut powers: BTreeMap<Expr, i32> = BTreeMap::new();
    for item in items {
        match item {
            Expr::Number(value) => {
                coefficient = coefficient
                    .checked_mul(&value)
                    .unwrap_or_else(|_| coefficient.clone());
            }
            Expr::Power(base, exponent) => match exponent.as_ref() {
                Expr::Number(exponent) if exponent.is_integer() => {
                    let exponent = exponent
                        .to_integer()
                        .to_string()
                        .parse::<i32>()
                        .unwrap_or(1);
                    let entry = powers.entry(*base).or_insert(0);
                    *entry = entry.saturating_add(exponent);
                }
                _ => {
                    let entry = powers.entry(Expr::Power(base, exponent)).or_insert(0);
                    *entry = entry.saturating_add(1);
                }
            },
            other => {
                let entry = powers.entry(other).or_insert(0);
                *entry = entry.saturating_add(1);
            }
        }
    }
    if coefficient.is_zero() {
        return Expr::Number(Rational::zero());
    }
    let mut numerator: Vec<Expr> = Vec::new();
    let mut denominator: Vec<Expr> = Vec::new();
    for (base, exponent) in powers {
        if exponent == 0 {
            continue;
        }
        let power = |value: i32| -> Expr {
            if value == 1 {
                base.clone()
            } else {
                Expr::Power(
                    Box::new(base.clone()),
                    Box::new(Expr::Number(Rational::from_i64(i64::from(value)))),
                )
            }
        };
        if exponent > 0 {
            numerator.push(power(exponent));
        } else {
            denominator.push(power(-exponent));
        }
    }
    let mut ordered: Vec<Expr> = Vec::with_capacity(numerator.len() + 1);
    if !coefficient.is_one() || numerator.is_empty() {
        ordered.push(Expr::Number(coefficient.clone()));
    }
    ordered.extend(numerator);
    let mut numerator = ordered;
    let numerator_expr = match numerator.len() {
        1 => numerator
            .pop()
            .unwrap_or_else(|| Expr::Number(Rational::one())),
        _ => Expr::Product(numerator),
    };
    if denominator.is_empty() {
        return numerator_expr;
    }
    let denominator_expr = match denominator.len() {
        1 => denominator
            .pop()
            .unwrap_or_else(|| Expr::Number(Rational::one())),
        _ => Expr::Product(denominator),
    };
    Expr::Fraction(Box::new(numerator_expr), Box::new(denominator_expr))
}

fn normalize_power(base: Expr, exponent: Expr) -> Expr {
    if let Expr::Number(exponent_value) = &exponent {
        if exponent_value.is_zero() {
            return Expr::Number(Rational::one());
        }
        if exponent_value.is_one() {
            return base;
        }
        if exponent_value.is_integer() {
            if let Expr::Number(base_value) = &base {
                if let Ok(value) = base_value.checked_pow(parse_i32(exponent_value)) {
                    return Expr::Number(value);
                }
                return Expr::Power(Box::new(base), Box::new(exponent));
            }
            if let Expr::Radical {
                degree: None,
                radicand,
            } = &base
                && let Expr::Number(radicand_value) = radicand.as_ref()
                && let Some(root) = radicand_value.exact_sqrt()
            {
                let squared = Expr::Number(root.clone());
                if exponent_value.is_integer() && parse_i32(exponent_value) == 2 {
                    return squared;
                }
                return normalize_power(squared, exponent);
            }
            // 和式的正整数幂展开（固定点迭代后续会合并同类项）
            if let Expr::Sum(_) = &base {
                let exponent_int = parse_i32(exponent_value);
                if exponent_int > 0
                    && let Ok(count) = u32::try_from(exponent_int)
                    && count <= 8
                {
                    return Expr::Product(vec![base; count as usize]);
                }
            }
            if let Expr::Power(inner_base, inner_exponent) = &base
                && let Expr::Number(inner_value) = inner_exponent.as_ref()
                && inner_value.is_integer()
            {
                let product = Rational::from_i64(i64::from(parse_i32(inner_value)))
                    .checked_mul(&Rational::from_i64(i64::from(parse_i32(exponent_value))));
                if let Ok(product) = product
                    && product.is_integer()
                {
                    return normalize_power(*inner_base.clone(), Expr::Number(product));
                }
            }
        }
    }
    Expr::Power(Box::new(base), Box::new(exponent))
}

fn parse_i32(value: &Rational) -> i32 {
    value
        .to_integer()
        .to_string()
        .parse::<i32>()
        .unwrap_or_default()
}

fn normalize_fraction(numerator: Expr, denominator: Expr) -> Expr {
    if let Expr::Number(denominator_value) = &denominator {
        if denominator_value.is_zero() {
            return Expr::Fraction(Box::new(numerator), Box::new(denominator));
        }
        if denominator_value.is_one() {
            return numerator;
        }
        if let Expr::Number(numerator_value) = &numerator
            && let Ok(value) = numerator_value.checked_div(denominator_value)
        {
            return Expr::Number(value);
        }
        if numerator.is_constant()
            && let Ok(value) = evaluate(&numerator)
            && let Ok(quotient) = value.checked_div(denominator_value)
        {
            return Expr::Number(quotient);
        }
    }
    if let Expr::Number(numerator_value) = &numerator
        && numerator_value.is_zero()
        && denominator.is_constant()
    {
        return Expr::Number(Rational::zero());
    }
    Expr::Fraction(Box::new(numerator), Box::new(denominator))
}

fn normalize_radical(degree: Option<Expr>, radicand: Expr) -> Expr {
    if let Expr::Number(value) = &radicand {
        match &degree {
            None => {
                if value.is_zero() {
                    return Expr::Number(Rational::zero());
                }
                if value.is_negative() {
                    return Expr::Radical {
                        degree: None,
                        radicand: Box::new(radicand),
                    };
                }
                if let Some(root) = value.exact_sqrt() {
                    return Expr::Number(root);
                }
                if let Some((outside, inside)) = simplify_square_factor(value) {
                    let radical = Expr::Radical {
                        degree: None,
                        radicand: Box::new(Expr::Number(inside)),
                    };
                    if outside.is_one() {
                        return radical;
                    }
                    return Expr::Product(vec![Expr::Number(outside), radical]);
                }
                Expr::Radical {
                    degree: None,
                    radicand: Box::new(radicand),
                }
            }
            Some(Expr::Number(degree_value)) if degree_value.is_integer() => {
                let Ok(degree_int) = degree_value.to_integer().to_string().parse::<u32>() else {
                    return Expr::Radical {
                        degree: Some(Box::new(Expr::Number(degree_value.clone()))),
                        radicand: Box::new(radicand),
                    };
                };
                if let Some(root) = value.exact_nth_root(degree_int) {
                    return Expr::Number(root);
                }
                Expr::Radical {
                    degree: Some(Box::new(Expr::Number(degree_value.clone()))),
                    radicand: Box::new(radicand),
                }
            }
            Some(_) => Expr::Radical {
                degree: degree.map(Box::new),
                radicand: Box::new(radicand),
            },
        }
    } else {
        Expr::Radical {
            degree: degree.map(Box::new),
            radicand: Box::new(radicand),
        }
    }
}

/// 把正有理数的平方根拆成 `(外部系数, 被开方数)`：`√(p/q) = outside·√(inside)`。
///
/// 仅对 `p·q ≤ 10^9` 做试除分解；超出范围返回 `None`（不化简，避免误判）。
fn simplify_square_factor(value: &Rational) -> Option<(Rational, Rational)> {
    let numer = value.numer().magnitude().to_u64_digits();
    let denom = value.denom().magnitude().to_u64_digits();
    let (numer, denom) = (digits_to_u64(&numer)?, digits_to_u64(&denom)?);
    let product = numer.checked_mul(denom)?;
    if product == 0 || product > MAX_RADICAL_FACTOR {
        return None;
    }
    let mut remaining = product;
    let mut outside: u64 = 1;
    let mut factor: u64 = 2;
    while factor * factor <= remaining {
        while remaining % (factor * factor) == 0 {
            remaining /= factor * factor;
            outside = outside.checked_mul(factor)?;
        }
        factor += if factor == 2 { 1 } else { 2 };
    }
    let outside_rational = Rational::new(outside.into(), denom.into()).ok()?;
    Some((
        outside_rational,
        Rational::from_i64(i64::try_from(remaining).ok()?),
    ))
}

fn digits_to_u64(digits: &[u64]) -> Option<u64> {
    match digits {
        [] => Some(0),
        [value] => Some(*value),
        _ => None,
    }
}

/// 常量比较（供区间/集合元素排序与判等）。
#[must_use]
pub fn compare_constants(left: &Expr, right: &Expr) -> Option<Ordering> {
    let left = evaluate(left).ok()?;
    let right = evaluate(right).ok()?;
    Some(left.compare(&right))
}

/// 判断单点区间（`[a,a]`）。
#[must_use]
pub fn interval_is_point(interval: &Interval) -> Option<&Expr> {
    let IntervalEnd::Value {
        expr: start,
        closed: start_closed,
    } = &interval.start
    else {
        return None;
    };
    let IntervalEnd::Value {
        expr: end,
        closed: end_closed,
    } = &interval.end
    else {
        return None;
    };
    if *start_closed && *end_closed && start == end {
        Some(start.as_ref())
    } else {
        None
    }
}

/// 从集合元素构造值集合（仅支持可精确求值的元素）。
#[must_use]
pub fn value_set(items: &[Expr]) -> Option<Vec<Rational>> {
    let mut values = Vec::with_capacity(items.len());
    for item in items {
        values.push(evaluate(item).ok()?);
    }
    values.sort();
    values.dedup();
    Some(values)
}

/// 判断 `unit` 是否与自身一致的量纲（占位辅助，供比较器复用）。
#[must_use]
pub fn same_dimension(left: &Unit, right: &Unit) -> bool {
    left.dims == right.dims
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::parse::parse_expr;

    fn canonical(input: &str) -> Expr {
        match parse_expr(input) {
            Ok(expr) => canonicalize(&expr),
            Err(error) => panic!("输入 {input:?} 解析失败：{error}"),
        }
    }

    #[test]
    fn folds_constants() {
        assert_eq!(canonical("1+2*3").to_text(), "7");
        assert_eq!(canonical("2/3+1/6").to_text(), "5/6");
        assert_eq!(canonical("2^10").to_text(), "1024");
        assert_eq!(canonical("\\sqrt{16}").to_text(), "4");
        assert_eq!(canonical("\\sqrt{12}").to_text(), "2*\\sqrt{3}");
        assert_eq!(canonical("|{-3}|").to_text(), "3");
    }

    #[test]
    fn combines_like_terms_and_expands() {
        assert_eq!(canonical("x+x").to_text(), "2*x");
        assert_eq!(canonical("x+x+1").to_text(), "2*x + 1");
        assert_eq!(canonical("2*x+3*x").to_text(), "5*x");
        assert_eq!(canonical("(x+1)*(x-1)").to_text(), "x^2 - 1");
        assert_eq!(canonical("(x+1)^2").to_text(), "x^2 + 2*x + 1");
        assert_eq!(canonical("x*x").to_text(), "x^2");
        assert_eq!(canonical("x^2*x^3").to_text(), "x^5");
    }

    #[test]
    fn canonicalization_is_idempotent() {
        for input in [
            "1+2*3",
            "(x+1)*(x-1)",
            "\\sqrt{12}",
            "x/2+1",
            "\\{1,2,2\\}",
            "[1,2)",
            "x=1 或 x=2",
            "12\\text{cm}",
        ] {
            let once = canonical(input);
            let twice = canonicalize(&once);
            assert_eq!(once, twice, "{input}");
            // 渲染结果可被重新解析，且规范化后不变
            let rendered = once.to_text();
            let reparsed = canonicalize(&parse_expr(&rendered).unwrap());
            assert_eq!(reparsed, once, "{input} -> {rendered}");
        }
    }

    #[test]
    fn evaluates_constants() {
        assert_eq!(evaluate(&canonical("1+2*3")).unwrap().to_text(), "7");
        assert_eq!(evaluate(&canonical("\\sqrt{9}")).unwrap().to_text(), "3");
        assert!(evaluate(&canonical("x+1")).is_err());
        assert!(matches!(
            evaluate(&canonical("1/0")),
            Err(EvalError::DivideByZero)
        ));
    }

    #[test]
    fn polynomial_extraction_and_equality() {
        assert_eq!(
            polynomial_equality(&canonical("(x+1)*(x-1)"), &canonical("x^2-1")),
            Some(true)
        );
        assert_eq!(
            polynomial_equality(&canonical("x+1"), &canonical("x+2")),
            Some(false)
        );
        assert_eq!(
            polynomial_equality(&canonical("\\sin(x)"), &canonical("\\sin(x)")),
            None
        );
        let poly = polynomial(&canonical("(x+1)^2")).unwrap();
        assert_eq!(poly.single_variable_degree("x"), Some(2));
    }

    #[test]
    fn relation_to_interval_conversion() {
        let interval = relation_to_interval(&canonical("x<2")).unwrap();
        assert_eq!(interval.to_latex(), "(-\\infty,2)");
        let interval = relation_to_interval(&canonical("2*x+1\\ge 5")).unwrap();
        assert_eq!(interval.to_latex(), "[2,\\infty)");
        let interval = relation_to_interval(&canonical("-x<2")).unwrap();
        assert_eq!(interval.to_latex(), "(-2,\\infty)");
        assert!(relation_to_interval(&canonical("x^2<2")).is_err());
        assert!(relation_to_interval(&canonical("x\\neq 2")).is_err());
    }

    #[test]
    fn equation_solutions_are_exact() {
        assert_eq!(
            equation_solutions(&canonical("2*x+1=5")).unwrap(),
            vec![Rational::from_i64(2)]
        );
        let roots = equation_solutions(&canonical("x^2-3*x+2=0")).unwrap();
        assert_eq!(roots, vec![Rational::from_i64(1), Rational::from_i64(2)]);
        assert_eq!(equation_solutions(&canonical("x^2=-1")).unwrap().len(), 0);
        assert!(equation_solutions(&canonical("x^2=2")).is_err());
        assert!(equation_solutions(&canonical("x^3=1")).is_err());
    }
}
