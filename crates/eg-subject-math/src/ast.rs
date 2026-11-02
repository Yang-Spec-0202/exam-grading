//! 数学内部语法树（`SPEC-subject-math.md` §4）。
//!
//! 覆盖节点：数值、变量、和、积、幂、分式、根式、方程/不等式、集合、区间、向量、
//! 矩阵、单位、布尔、选项；另有函数节点用于**超出支持范围但语法可识别**的写法
//! （三角、微积分等）：它们不导致归一化失败，而是让比较器返回「无法判定」，
//! 为 T26（全符号比较）与 T27（CAS 适配）留扩展点。
//!
//! 设计要点：
//! - 数值节点只存 [`Rational`]（精确有理数，禁止 f64）；
//! - 单位节点 `Quantity` 携带 [`Unit`]（量纲 + 换算因子 + 规范符号）；
//! - 渲染器 `to_latex`/`to_text` 输出**可被本 crate 解析器重新解析**的规范形式，
//!   这是「归一化幂等」的实现基础。

use core::fmt;
use std::collections::BTreeMap;

use crate::number::Rational;

/// 关系运算符。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RelOp {
    /// 等于。
    Eq,
    /// 不等于。
    Ne,
    /// 小于。
    Lt,
    /// 小于等于。
    Le,
    /// 大于。
    Gt,
    /// 大于等于。
    Ge,
}

impl RelOp {
    /// 规范 LaTeX 记号。
    #[must_use]
    pub const fn to_latex(self) -> &'static str {
        match self {
            Self::Eq => "=",
            Self::Ne => "\\neq",
            Self::Lt => "<",
            Self::Le => "\\le",
            Self::Gt => ">",
            Self::Ge => "\\ge",
        }
    }

    /// 是否为等式。
    #[must_use]
    pub const fn is_equality(self) -> bool {
        matches!(self, Self::Eq)
    }

    /// 取反方向（`a<b` ⇔ `b>a`）。
    #[must_use]
    pub const fn mirror(self) -> Self {
        match self {
            Self::Eq => Self::Eq,
            Self::Ne => Self::Ne,
            Self::Lt => Self::Gt,
            Self::Le => Self::Ge,
            Self::Gt => Self::Lt,
            Self::Ge => Self::Le,
        }
    }
}

/// 常见计数量词（无量纲但符号有意义，如「3 个」≠「3 人」）。
pub const COUNT_UNITS: &[&str] = &[
    "个", "人", "次", "元", "本", "张", "份", "只", "件", "台", "组", "对", "种", "岁", "位",
];

/// 基本量纲（单位换算用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Dim {
    /// 长度。
    Length,
    /// 质量。
    Mass,
    /// 时间。
    Time,
}

impl Dim {
    /// 规范符号。
    #[must_use]
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Length => "m",
            Self::Mass => "kg",
            Self::Time => "s",
        }
    }
}

/// 单位：量纲指数 + 到 SI 基本单位的换算因子 + 规范符号。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Unit {
    /// 各基本量纲的指数（缺省为 0）。
    pub dims: BTreeMap<Dim, i16>,
    /// 到 SI 基本单位组合的换算因子。
    pub factor: Rational,
    /// 规范符号（如 `cm`、`m/s`、`m^2`）。
    pub symbol: String,
}

impl Unit {
    /// 解析单位符号（支持 `/`、`*`、`^n` 与基础单位表）。
    ///
    /// 支持范围（T14 基础量纲）：长度 m/cm/mm/dm/km/μm、质量 kg/g/mg/t、
    /// 时间 s/ms/min/h/d、面积体积（m^2、cm^3、L、mL）与简单复合单位（m/s、km/h）。
    ///
    /// # Errors
    ///
    /// 无法识别的单位返回 `None`（上层转「无法判定：单位不可识别」）。
    pub fn parse(symbol: &str) -> Option<Self> {
        let compact: String = symbol.chars().filter(|ch| !ch.is_whitespace()).collect();
        if compact.is_empty() {
            return None;
        }
        // 常见计数量词（无量纲，仅按符号区分）：个/人/次/元/本/张/份/只/件/台/组/对/种/岁
        if COUNT_UNITS.contains(&compact.as_str()) {
            return Some(Self::count(&compact));
        }
        let (numerator, denominator) = match compact.split_once('/') {
            Some((numerator, denominator)) => (numerator, Some(denominator)),
            None => (compact.as_str(), None),
        };
        let mut dims: BTreeMap<Dim, i16> = BTreeMap::new();
        let mut factor = Rational::one();
        apply_unit_product(numerator, 1, &mut dims, &mut factor)?;
        if let Some(denominator) = denominator {
            apply_unit_product(denominator, -1, &mut dims, &mut factor)?;
        }
        let canonical = canonical_unit_symbol(&dims, &factor, &compact);
        Some(Self {
            dims,
            factor,
            symbol: canonical,
        })
    }

    /// 无量纲命名单位（如「个」「人」）：量纲为空且携带标签。
    #[must_use]
    pub fn count(label: &str) -> Self {
        Self {
            dims: BTreeMap::new(),
            factor: Rational::one(),
            symbol: label.to_owned(),
        }
    }

    /// 是否无量纲。
    #[must_use]
    pub fn is_dimensionless(&self) -> bool {
        self.dims.is_empty()
    }

    /// 是否为无量纲「计数」单位（如「个」）。
    #[must_use]
    pub fn is_count(&self) -> bool {
        self.dims.is_empty() && self.factor.is_one() && !self.symbol.is_empty()
    }

    /// 从本单位换算到目标单位的因子（量纲一致时返回 `Some`）。
    #[must_use]
    pub fn conversion_factor_to(&self, target: &Self) -> Option<Rational> {
        if self.dims != target.dims {
            return None;
        }
        self.factor.checked_div(&target.factor).ok()
    }
}

impl fmt::Display for Unit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.symbol)
    }
}

fn apply_unit_product(
    part: &str,
    sign: i16,
    dims: &mut BTreeMap<Dim, i16>,
    factor: &mut Rational,
) -> Option<()> {
    if part.is_empty() {
        return None;
    }
    for piece in part.split('*') {
        if piece.is_empty() {
            return None;
        }
        let (symbol, exponent) = match piece.split_once('^') {
            Some((symbol, exponent)) => (symbol, exponent.parse::<i16>().ok()?),
            None => (piece, 1),
        };
        let (dim, unit_factor) = base_unit(symbol)?;
        *dims.entry(dim).or_insert(0) += sign * exponent;
        let scaled = unit_factor.checked_pow(i32::from(sign * exponent)).ok()?;
        *factor = factor.checked_mul(&scaled).ok()?;
    }
    Some(())
}

fn base_unit(symbol: &str) -> Option<(Dim, Rational)> {
    let (dim, factor) = match symbol {
        "m" => (Dim::Length, Rational::one()),
        "dm" => (Dim::Length, Rational::new(1.into(), 10.into()).ok()?),
        "cm" => (Dim::Length, Rational::new(1.into(), 100.into()).ok()?),
        "mm" => (Dim::Length, Rational::new(1.into(), 1_000.into()).ok()?),
        "km" => (Dim::Length, Rational::from_i64(1_000)),
        "um" | "μm" => (Dim::Length, Rational::new(1.into(), 1_000_000.into()).ok()?),
        "kg" => (Dim::Mass, Rational::one()),
        "g" => (Dim::Mass, Rational::new(1.into(), 1_000.into()).ok()?),
        "mg" => (Dim::Mass, Rational::new(1.into(), 1_000_000.into()).ok()?),
        "t" => (Dim::Mass, Rational::from_i64(1_000)),
        "s" => (Dim::Time, Rational::one()),
        "ms" => (Dim::Time, Rational::new(1.into(), 1_000.into()).ok()?),
        "min" => (Dim::Time, Rational::from_i64(60)),
        "h" => (Dim::Time, Rational::from_i64(3_600)),
        "d" => (Dim::Time, Rational::from_i64(86_400)),
        _ => return None,
    };
    Some((dim, factor))
}

fn canonical_unit_symbol(dims: &BTreeMap<Dim, i16>, factor: &Rational, original: &str) -> String {
    // 保留原始写法（去空白），但把 `*` 统一；量纲与因子由结构承载。
    let compact: String = original.chars().filter(|ch| !ch.is_whitespace()).collect();
    if dims.is_empty() && factor.is_one() {
        return compact;
    }
    compact
}

/// 区间端点。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IntervalEnd {
    /// 负无穷。
    NegInfinity,
    /// 正无穷。
    PosInfinity,
    /// 有限端点。
    Value {
        /// 端点表达式（规范要求常量；含变量时比较器返回无法判定）。
        expr: Box<Expr>,
        /// 是否闭端点。
        closed: bool,
    },
}

impl IntervalEnd {
    /// 端点是否闭合（无穷端点视为开）。
    #[must_use]
    pub fn is_closed(&self) -> bool {
        matches!(self, Self::Value { closed: true, .. })
    }

    /// 有限端点表达式。
    #[must_use]
    pub fn expr(&self) -> Option<&Expr> {
        match self {
            Self::Value { expr, .. } => Some(expr.as_ref()),
            Self::NegInfinity | Self::PosInfinity => None,
        }
    }
}

/// 区间：`[a,b]`、`(a,b)`、`[a,b)`、`(a,b]`，支持无穷端点。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Interval {
    /// 左端点。
    pub start: IntervalEnd,
    /// 右端点。
    pub end: IntervalEnd,
}

impl Interval {
    /// 构造区间。
    #[must_use]
    pub fn new(start: IntervalEnd, end: IntervalEnd) -> Self {
        Self { start, end }
    }

    /// 是否为空区间（同端点且至少一端开，或端点顺序颠倒）。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        match (&self.start, &self.end) {
            (
                IntervalEnd::Value {
                    expr: left,
                    closed: left_closed,
                },
                IntervalEnd::Value {
                    expr: right,
                    closed: right_closed,
                },
            ) => {
                if left == right {
                    !(*left_closed && *right_closed)
                } else {
                    compare_constant(left, right) == Some(core::cmp::Ordering::Greater)
                }
            }
            (IntervalEnd::PosInfinity, _) | (_, IntervalEnd::NegInfinity) => true,
            _ => false,
        }
    }
}

impl Interval {
    /// 规范 LaTeX 文本。
    #[must_use]
    pub fn to_latex(&self) -> String {
        render_interval(self, true)
    }

    /// 规范纯文本。
    #[must_use]
    pub fn to_text(&self) -> String {
        render_interval(self, false)
    }
}

fn compare_constant(left: &Expr, right: &Expr) -> Option<core::cmp::Ordering> {
    match (left, right) {
        (Expr::Number(left), Expr::Number(right)) => Some(left.compare(right)),
        _ => None,
    }
}

/// 数学内部语法树节点。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Expr {
    /// 精确数值。
    Number(Rational),
    /// 变量（单字母/希腊字母，如 `x`、`π`）。
    Variable(String),
    /// 和（≥2 项；规范化时按确定顺序排序）。
    Sum(Vec<Expr>),
    /// 积（≥2 因子；规范化时按确定顺序排序）。
    Product(Vec<Expr>),
    /// 幂。
    Power(Box<Expr>, Box<Expr>),
    /// 分式。
    Fraction(Box<Expr>, Box<Expr>),
    /// 根式（`degree` 为空表示平方根）。
    Radical {
        /// 根指数（可空）。
        degree: Option<Box<Expr>>,
        /// 被开方数。
        radicand: Box<Expr>,
    },
    /// 绝对值。
    Absolute(Box<Expr>),
    /// 带单位数值/物理量。
    Quantity {
        /// 数值部分。
        value: Box<Expr>,
        /// 单位。
        unit: Unit,
    },
    /// 函数调用（三角/对数/微积分等；T14 只做结构保留，比较时返回无法判定）。
    Function {
        /// 函数名。
        name: String,
        /// 参数列表。
        args: Vec<Expr>,
    },
    /// 向量（二维/三维或 \vec 形式）。
    Vector(Vec<Expr>),
    /// 矩阵（行 × 列）。
    Matrix(Vec<Vec<Expr>>),
    /// 区间。
    Interval(Interval),
    /// 集合（有限集合，忽略顺序/去重）。
    Set(Vec<Expr>),
    /// 集合并集（`A ∪ B`；可嵌套、可含区间）。
    SetUnion(Vec<Expr>),
    /// 集合交集（`A ∩ B`；可嵌套、可含区间）。
    SetIntersection(Vec<Expr>),
    /// 方程/不等式。
    Relation {
        /// 关系运算符。
        op: RelOp,
        /// 左式。
        lhs: Box<Expr>,
        /// 右式。
        rhs: Box<Expr>,
    },
    /// 多解列表（`x=1 或 x=2`）。
    Solutions(Vec<Expr>),
    /// 布尔值。
    Boolean(bool),
    /// 选项字母。
    Option(String),
}

impl Expr {
    /// 是否为常量（不含变量/函数/关系/集合等）。
    #[must_use]
    pub fn is_constant(&self) -> bool {
        match self {
            Self::Number(_) | Self::Boolean(_) | Self::Option(_) => true,
            Self::Sum(items)
            | Self::Product(items)
            | Self::Set(items)
            | Self::SetUnion(items)
            | Self::SetIntersection(items)
            | Self::Vector(items) => items.iter().all(Self::is_constant),
            Self::Power(base, exponent) => base.is_constant() && exponent.is_constant(),
            Self::Fraction(numerator, denominator) => {
                numerator.is_constant() && denominator.is_constant()
            }
            Self::Radical { degree, radicand } => {
                radicand.is_constant() && degree.as_deref().is_none_or(Expr::is_constant)
            }
            Self::Absolute(value) | Self::Quantity { value, .. } => value.is_constant(),
            Self::Matrix(rows) => rows.iter().all(|row| row.iter().all(Self::is_constant)),
            Self::Interval(interval) => {
                interval.start.expr().is_none_or(Expr::is_constant)
                    && interval.end.expr().is_none_or(Expr::is_constant)
            }
            Self::Function { .. }
            | Self::Relation { .. }
            | Self::Solutions(_)
            | Self::Variable(_) => false,
        }
    }

    /// 变量名集合（按字典序）。
    #[must_use]
    pub fn variables(&self) -> Vec<String> {
        let mut names = Vec::new();
        self.collect_variables(&mut names);
        names.sort();
        names.dedup();
        names
    }

    fn collect_variables(&self, names: &mut Vec<String>) {
        match self {
            Self::Variable(name) => names.push(name.clone()),
            Self::Number(_) | Self::Boolean(_) | Self::Option(_) => {}
            Self::Sum(items)
            | Self::Product(items)
            | Self::Set(items)
            | Self::SetUnion(items)
            | Self::SetIntersection(items)
            | Self::Vector(items)
            | Self::Solutions(items) => {
                for item in items {
                    item.collect_variables(names);
                }
            }
            Self::Power(base, exponent) => {
                base.collect_variables(names);
                exponent.collect_variables(names);
            }
            Self::Fraction(numerator, denominator) => {
                numerator.collect_variables(names);
                denominator.collect_variables(names);
            }
            Self::Radical { degree, radicand } => {
                radicand.collect_variables(names);
                if let Some(degree) = degree {
                    degree.collect_variables(names);
                }
            }
            Self::Absolute(value) | Self::Quantity { value, .. } => {
                value.collect_variables(names);
            }
            Self::Function { args, .. } => {
                for arg in args {
                    arg.collect_variables(names);
                }
            }
            Self::Matrix(rows) => {
                for row in rows {
                    for cell in row {
                        cell.collect_variables(names);
                    }
                }
            }
            Self::Interval(interval) => {
                if let Some(expr) = interval.start.expr() {
                    expr.collect_variables(names);
                }
                if let Some(expr) = interval.end.expr() {
                    expr.collect_variables(names);
                }
            }
            Self::Relation { lhs, rhs, .. } => {
                lhs.collect_variables(names);
                rhs.collect_variables(names);
            }
        }
    }

    /// 规范 LaTeX 文本（可被本 crate 解析器重新解析）。
    #[must_use]
    pub fn to_latex(&self) -> String {
        render_latex(self, 0)
    }

    /// 规范纯文本（可被本 crate 解析器重新解析）。
    #[must_use]
    pub fn to_text(&self) -> String {
        render_text(self, 0)
    }

    /// 渲染为 CAS（sympy）可解析的 ASCII 表达式；非标量/含单位返回 `None`。
    ///
    /// 只输出受限字符集（字母、数字、`+ - * / ^ ( ) . ,`、空格），供 CAS
    /// sidecar 安全解析；变量 `π` 渲染为 `pi`，根式渲染为 `sqrt(...)`。
    #[must_use]
    pub fn to_cas(&self) -> Option<String> {
        render_cas(self)
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_text())
    }
}

/// 运算符优先级（渲染时决定是否加括号）。
fn precedence(expr: &Expr) -> u8 {
    match expr {
        Expr::Relation { .. } | Expr::Solutions(_) => 1,
        Expr::SetUnion(_) => 2,
        Expr::SetIntersection(_) => 3,
        Expr::Sum(_) => 4,
        Expr::Product(_) => 5,
        Expr::Power(_, _) | Expr::Fraction(_, _) => 6,
        _ => 7,
    }
}

fn render_latex(expr: &Expr, parent: u8) -> String {
    let precedence = precedence(expr);
    let text = render_latex_inner(expr);
    if precedence < parent {
        format!("\\left({text}\\right)")
    } else {
        text
    }
}

fn render_latex_inner(expr: &Expr) -> String {
    match expr {
        Expr::Number(value) => value.to_latex(),
        Expr::Variable(name) => render_variable(name),
        Expr::Sum(items) => render_sum(items, true),
        Expr::Product(items) => render_product(items, true),
        Expr::Power(base, exponent) => {
            format!(
                "{}^{{{}}}",
                render_latex(base, 7),
                render_latex(exponent, 0)
            )
        }
        Expr::Fraction(numerator, denominator) => format!(
            "\\frac{{{}}}{{{}}}",
            render_latex(numerator, 0),
            render_latex(denominator, 0)
        ),
        Expr::Radical { degree, radicand } => match degree {
            Some(degree) => format!(
                "\\sqrt[{}]{{{}}}",
                render_latex(degree, 0),
                render_latex(radicand, 0)
            ),
            None => format!("\\sqrt{{{}}}", render_latex(radicand, 0)),
        },
        Expr::Absolute(value) => format!("\\left|{}\\right|", render_latex(value, 0)),
        Expr::Quantity { value, unit } => {
            format!("{}\\text{{{}}}", render_latex(value, 7), unit.symbol)
        }
        Expr::Function { name, args } => {
            let rendered: Vec<String> = args.iter().map(|arg| render_latex(arg, 0)).collect();
            format!("\\{name}\\left({}\\right)", rendered.join(","))
        }
        Expr::Vector(items) => {
            let rendered: Vec<String> = items.iter().map(|item| render_latex(item, 0)).collect();
            format!("\\vec{{\\left({}\\right)}}", rendered.join(","))
        }
        Expr::Matrix(rows) => {
            let mut text = String::from("\\begin{pmatrix}");
            for (row_index, row) in rows.iter().enumerate() {
                if row_index > 0 {
                    text.push_str("\\\\");
                }
                let cells: Vec<String> = row.iter().map(|cell| render_latex(cell, 0)).collect();
                text.push_str(&cells.join("&"));
            }
            text.push_str("\\end{pmatrix}");
            text
        }
        Expr::Interval(interval) => render_interval(interval, true),
        Expr::Set(items) => {
            let rendered: Vec<String> = items.iter().map(|item| render_latex(item, 0)).collect();
            format!("\\{{{}\\}}", rendered.join(","))
        }
        Expr::SetUnion(items) => render_set_op(items, "\\cup", true),
        Expr::SetIntersection(items) => render_set_op(items, "\\cap", true),
        Expr::Relation { op, lhs, rhs } => format!(
            "{}{}{}",
            render_latex(lhs, 4),
            op.to_latex(),
            render_latex(rhs, 4)
        ),
        Expr::Solutions(items) => {
            let rendered: Vec<String> = items.iter().map(|item| render_latex(item, 1)).collect();
            rendered.join("\\text{或}")
        }
        Expr::Boolean(value) => {
            if *value {
                "\\text{true}".to_owned()
            } else {
                "\\text{false}".to_owned()
            }
        }
        Expr::Option(letter) => letter.clone(),
    }
}

fn render_text(expr: &Expr, parent: u8) -> String {
    let precedence = precedence(expr);
    let text = render_text_inner(expr);
    if precedence < parent {
        format!("({text})")
    } else {
        text
    }
}

fn render_text_inner(expr: &Expr) -> String {
    match expr {
        Expr::Number(value) => value.to_text(),
        Expr::Variable(name) => name.clone(),
        Expr::Sum(items) => render_sum(items, false),
        Expr::Product(items) => render_product(items, false),
        Expr::Power(base, exponent) => {
            format!("{}^{}", render_text(base, 7), render_text(exponent, 0))
        }
        Expr::Fraction(numerator, denominator) => {
            format!(
                "{}/{}",
                render_text(numerator, 4),
                render_text(denominator, 4)
            )
        }
        Expr::Radical { degree, radicand } => match degree {
            Some(degree) => format!(
                "\\sqrt[{}]{{{}}}",
                render_text(degree, 0),
                render_text(radicand, 0)
            ),
            None => format!("\\sqrt{{{}}}", render_text(radicand, 0)),
        },
        Expr::Absolute(value) => format!("|{}|", render_text(value, 0)),
        Expr::Quantity { value, unit } => {
            format!("{}\\text{{{}}}", render_text(value, 7), unit.symbol)
        }
        Expr::Function { name, args } => {
            let rendered: Vec<String> = args.iter().map(|arg| render_text(arg, 0)).collect();
            format!("{name}({})", rendered.join(","))
        }
        Expr::Vector(items) => {
            let rendered: Vec<String> = items.iter().map(|item| render_text(item, 0)).collect();
            format!("\\vec{{({})}}", rendered.join(","))
        }
        Expr::Matrix(rows) => {
            let mut text = String::from("\\begin{pmatrix}");
            for (row_index, row) in rows.iter().enumerate() {
                if row_index > 0 {
                    text.push_str("\\\\");
                }
                let cells: Vec<String> = row.iter().map(|cell| render_text(cell, 0)).collect();
                text.push_str(&cells.join("&"));
            }
            text.push_str("\\end{pmatrix}");
            text
        }
        Expr::Interval(interval) => render_interval(interval, false),
        Expr::Set(items) => {
            let rendered: Vec<String> = items.iter().map(|item| render_text(item, 0)).collect();
            format!("\\{{{}\\}}", rendered.join(","))
        }
        Expr::SetUnion(items) => render_set_op(items, "\\cup", false),
        Expr::SetIntersection(items) => render_set_op(items, "\\cap", false),
        Expr::Relation { op, lhs, rhs } => format!(
            "{}{}{}",
            render_text(lhs, 4),
            op.to_latex(),
            render_text(rhs, 4)
        ),
        Expr::Solutions(items) => {
            let rendered: Vec<String> = items.iter().map(|item| render_text(item, 1)).collect();
            rendered.join(" 或 ")
        }
        Expr::Boolean(value) => {
            if *value {
                "true".to_owned()
            } else {
                "false".to_owned()
            }
        }
        Expr::Option(letter) => letter.clone(),
    }
}

fn render_cas(expr: &Expr) -> Option<String> {
    Some(match expr {
        Expr::Number(value) => value.to_text(),
        Expr::Variable(name) if name == "π" => "pi".to_owned(),
        Expr::Variable(name) => name.clone(),
        Expr::Sum(items) => {
            let mut rendered = String::new();
            for (index, item) in items.iter().enumerate() {
                let text = render_cas(item)?;
                if index == 0 {
                    rendered.push_str(&text);
                } else if let Some(positive) = text.strip_prefix('-') {
                    rendered.push_str(" - ");
                    rendered.push_str(positive);
                } else {
                    rendered.push_str(" + ");
                    rendered.push_str(&text);
                }
            }
            rendered
        }
        Expr::Product(items) => {
            let rendered: Vec<String> = items.iter().map(render_cas).collect::<Option<Vec<_>>>()?;
            rendered.join("*")
        }
        Expr::Power(base, exponent) => {
            format!("({})^({})", render_cas(base)?, render_cas(exponent)?)
        }
        Expr::Fraction(numerator, denominator) => {
            format!(
                "({})/({})",
                render_cas(numerator)?,
                render_cas(denominator)?
            )
        }
        Expr::Radical { degree, radicand } => match degree {
            Some(degree) => format!("root({}, {})", render_cas(radicand)?, render_cas(degree)?),
            None => format!("sqrt({})", render_cas(radicand)?),
        },
        Expr::Absolute(value) => format!("Abs({})", render_cas(value)?),
        Expr::Function { name, args } => {
            let rendered: Vec<String> = args.iter().map(render_cas).collect::<Option<Vec<_>>>()?;
            format!("{name}({})", rendered.join(","))
        }
        Expr::Boolean(_)
        | Expr::Option(_)
        | Expr::Quantity { .. }
        | Expr::Vector(_)
        | Expr::Matrix(_)
        | Expr::Interval(_)
        | Expr::Set(_)
        | Expr::SetUnion(_)
        | Expr::SetIntersection(_)
        | Expr::Relation { .. }
        | Expr::Solutions(_) => return None,
    })
}

fn render_sum(items: &[Expr], latex: bool) -> String {
    let mut text = String::new();
    for (index, item) in items.iter().enumerate() {
        let rendered = if latex {
            render_latex(item, 4)
        } else {
            render_text(item, 4)
        };
        if index == 0 {
            text.push_str(&rendered);
        } else if let Some(positive) = rendered.strip_prefix('-') {
            text.push_str(" - ");
            text.push_str(positive);
        } else {
            text.push_str(" + ");
            text.push_str(&rendered);
        }
    }
    text
}

fn render_product(items: &[Expr], latex: bool) -> String {
    let mut text = String::new();
    for (index, item) in items.iter().enumerate() {
        let rendered = if latex {
            render_latex(item, 5)
        } else {
            render_text(item, 5)
        };
        if index == 0 {
            text.push_str(&rendered);
        } else if latex {
            text.push_str("\\cdot ");
            text.push_str(&rendered);
        } else {
            text.push('*');
            text.push_str(&rendered);
        }
    }
    text
}

fn render_set_op(items: &[Expr], operator: &str, latex: bool) -> String {
    let parent = if operator == "\\cup" { 2 } else { 3 };
    let rendered: Vec<String> = items
        .iter()
        .map(|item| {
            if latex {
                render_latex(item, parent)
            } else {
                render_text(item, parent)
            }
        })
        .collect();
    rendered.join(&format!(" {operator} "))
}

fn render_interval(interval: &Interval, latex: bool) -> String {
    let start_bracket = if interval.start.is_closed() { "[" } else { "(" };
    let end_bracket = if interval.end.is_closed() { "]" } else { ")" };
    let start = match &interval.start {
        IntervalEnd::NegInfinity => "-\\infty".to_owned(),
        IntervalEnd::PosInfinity => "\\infty".to_owned(),
        IntervalEnd::Value { expr, .. } => {
            if latex {
                render_latex(expr, 0)
            } else {
                render_text(expr, 0)
            }
        }
    };
    let end = match &interval.end {
        IntervalEnd::NegInfinity => "-\\infty".to_owned(),
        IntervalEnd::PosInfinity => "\\infty".to_owned(),
        IntervalEnd::Value { expr, .. } => {
            if latex {
                render_latex(expr, 0)
            } else {
                render_text(expr, 0)
            }
        }
    };
    format!("{start_bracket}{start},{end}{end_bracket}")
}

fn render_variable(name: &str) -> String {
    match name {
        "π" => "\\pi".to_owned(),
        "α" => "\\alpha".to_owned(),
        "β" => "\\beta".to_owned(),
        "γ" => "\\gamma".to_owned(),
        "θ" => "\\theta".to_owned(),
        "λ" => "\\lambda".to_owned(),
        "μ" => "\\mu".to_owned(),
        "φ" => "\\varphi".to_owned(),
        "ω" => "\\omega".to_owned(),
        other => other.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use num_bigint::BigInt;

    fn number(value: i64) -> Expr {
        Expr::Number(Rational::from_i64(value))
    }

    fn fraction(numer: i64, denom: i64) -> Expr {
        Expr::Number(Rational::new(BigInt::from(numer), BigInt::from(denom)).unwrap())
    }

    #[test]
    fn unit_parsing_and_conversion() {
        let meter = Unit::parse("m").unwrap();
        let centimeter = Unit::parse("cm").unwrap();
        let factor = centimeter.conversion_factor_to(&meter).unwrap();
        assert_eq!(factor.to_text(), "0.01");
        assert!(
            meter
                .conversion_factor_to(&Unit::parse("s").unwrap())
                .is_none()
        );
        assert_eq!(Unit::parse("m/s").unwrap().dims.len(), 2);
        assert_eq!(Unit::parse("m^2").unwrap().dims[&Dim::Length], 2);
        assert!(Unit::parse("furlong").is_none());
        assert_eq!(Unit::parse("km/h").unwrap().symbol, "km/h");
    }

    #[test]
    fn renders_round_trip_text() {
        let sum = Expr::Sum(vec![number(1), Expr::Variable("x".to_owned())]);
        assert_eq!(sum.to_text(), "1 + x");
        let product = Expr::Product(vec![number(2), Expr::Variable("x".to_owned())]);
        assert_eq!(product.to_text(), "2*x");
        assert_eq!(fraction(1, 2).to_text(), "0.5");
        assert_eq!(fraction(1, 3).to_latex(), "\\frac{1}{3}");
        let radical = Expr::Radical {
            degree: None,
            radicand: Box::new(number(2)),
        };
        assert_eq!(radical.to_latex(), "\\sqrt{2}");
        let quantity = Expr::Quantity {
            value: Box::new(number(12)),
            unit: Unit::parse("cm").unwrap(),
        };
        assert_eq!(quantity.to_text(), "12\\text{cm}");
        let interval = Expr::Interval(Interval::new(
            IntervalEnd::Value {
                expr: Box::new(number(1)),
                closed: true,
            },
            IntervalEnd::PosInfinity,
        ));
        assert_eq!(interval.to_text(), "[1,\\infty)");
        let relation = Expr::Relation {
            op: RelOp::Eq,
            lhs: Box::new(Expr::Variable("x".to_owned())),
            rhs: Box::new(number(1)),
        };
        assert_eq!(relation.to_text(), "x=1");
    }

    #[test]
    fn negative_terms_render_as_subtraction() {
        let sum = Expr::Sum(vec![number(1), number(-2)]);
        assert_eq!(sum.to_text(), "1 - 2");
        let product = Expr::Product(vec![number(-2), Expr::Variable("x".to_owned())]);
        assert_eq!(product.to_text(), "-2*x");
    }

    #[test]
    fn variables_and_constants() {
        let expr = Expr::Sum(vec![
            Expr::Variable("x".to_owned()),
            Expr::Variable("y".to_owned()),
            Expr::Variable("x".to_owned()),
        ]);
        assert_eq!(expr.variables(), vec!["x".to_owned(), "y".to_owned()]);
        assert!(!expr.is_constant());
        assert!(number(3).is_constant());
    }

    #[test]
    fn empty_interval_detection() {
        let empty = Interval::new(
            IntervalEnd::Value {
                expr: Box::new(number(1)),
                closed: true,
            },
            IntervalEnd::Value {
                expr: Box::new(number(1)),
                closed: false,
            },
        );
        assert!(empty.is_empty());
        let single = Interval::new(
            IntervalEnd::Value {
                expr: Box::new(number(1)),
                closed: true,
            },
            IntervalEnd::Value {
                expr: Box::new(number(1)),
                closed: true,
            },
        );
        assert!(!single.is_empty());
    }
}
