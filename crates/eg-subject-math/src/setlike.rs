//! 集合与区间的规范化（`SPEC-subject-math.md` §4/§5 第 9/10 项，T26 扩展）。
//!
//! T14 只支持单个区间与有限集合；T26 扩展为：
//! - 区间的并集（`∪`）与交集（`∩`）表达式；
//! - 无穷端点、开闭端点、退化单点区间；
//! - 区间并集规范化为「按端点排序、互不相交、相邻可合并则合并」的唯一形式，
//!   因此规范化结果不同 ⇒ 集合确实不同（可安全判「不相等」）；
//! - 有限集合的并/交与无序、去重比较。
//!
//! 超出范围（端点含变量、含未支持元素、区间与有限集合混合等）一律返回
//! `Err(中文原因)`，由比较器转「无法判定」，绝不猜测。

use core::cmp::Ordering;
use std::collections::BTreeSet;

use crate::ast::{Expr, Interval, IntervalEnd};
use crate::canonical::{
    canonicalize, compare_constants, evaluate, interval_is_point, relation_to_interval,
};
use crate::number::Rational;

/// 规范化后的区间并集：按起点升序、互不相交（相邻可合并已合并）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntervalUnion {
    /// 区间列表（规范形式）。
    pub intervals: Vec<Interval>,
}

impl IntervalUnion {
    /// 由区间列表构造并规范化。
    #[must_use]
    pub fn new(intervals: Vec<Interval>) -> Self {
        let mut normalized: Vec<Interval> = intervals
            .into_iter()
            .filter(|interval| !interval.is_empty())
            .collect();
        normalized.sort_by(compare_interval_starts);
        let mut merged: Vec<Interval> = Vec::with_capacity(normalized.len());
        for interval in normalized {
            match merged.last_mut() {
                Some(previous) if intervals_merge(previous, &interval) => {
                    *previous = merge_intervals(previous, &interval);
                }
                _ => merged.push(interval),
            }
        }
        Self { intervals: merged }
    }

    /// 空集。
    #[must_use]
    pub fn empty() -> Self {
        Self {
            intervals: Vec::new(),
        }
    }

    /// 是否为空集。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.intervals.is_empty()
    }

    /// 转为语法树（单区间、并集或空集）。
    #[must_use]
    pub fn to_expr(&self) -> Expr {
        match self.intervals.len() {
            0 => Expr::Set(Vec::new()),
            1 => self.intervals.first().map_or_else(
                || Expr::Set(Vec::new()),
                |interval| Expr::Interval(interval.clone()),
            ),
            _ => Expr::SetUnion(
                self.intervals
                    .iter()
                    .map(|interval| Expr::Interval(interval.clone()))
                    .collect(),
            ),
        }
    }

    /// 规范文本（用于判定原因）。
    #[must_use]
    pub fn to_text(&self) -> String {
        if self.is_empty() {
            return "\\varnothing".to_owned();
        }
        self.intervals
            .iter()
            .map(Interval::to_text)
            .collect::<Vec<_>>()
            .join(" \\cup ")
    }
}

fn compare_interval_starts(left: &Interval, right: &Interval) -> Ordering {
    compare_interval_end(&left.start, &right.start)
        .then_with(|| compare_interval_end(&left.end, &right.end))
}

fn compare_interval_end(left: &IntervalEnd, right: &IntervalEnd) -> Ordering {
    use IntervalEnd::{NegInfinity, PosInfinity, Value};
    match (left, right) {
        (NegInfinity, NegInfinity) | (PosInfinity, PosInfinity) => Ordering::Equal,
        (NegInfinity, _) | (_, PosInfinity) => Ordering::Less,
        (PosInfinity, _) | (_, NegInfinity) => Ordering::Greater,
        (Value { expr: left, .. }, Value { expr: right, .. }) => {
            compare_constants(left, right).unwrap_or_else(|| left.cmp(right))
        }
    }
}

fn endpoint_closed(end: &IntervalEnd) -> bool {
    end.is_closed()
}

fn intervals_merge(previous: &Interval, next: &Interval) -> bool {
    match compare_interval_end(&next.start, &previous.end) {
        Ordering::Less => true,
        Ordering::Equal => endpoint_closed(&previous.end) || endpoint_closed(&next.start),
        Ordering::Greater => false,
    }
}

fn merge_intervals(previous: &Interval, next: &Interval) -> Interval {
    let start = match compare_interval_end(&previous.start, &next.start) {
        Ordering::Less | Ordering::Equal => previous.start.clone(),
        Ordering::Greater => next.start.clone(),
    };
    let end = match compare_interval_end(&previous.end, &next.end) {
        Ordering::Greater => previous.end.clone(),
        Ordering::Less => next.end.clone(),
        Ordering::Equal => IntervalEnd::Value {
            expr: match (&previous.end, &next.end) {
                (IntervalEnd::Value { expr: left, .. }, IntervalEnd::Value { .. }) => left.clone(),
                _ => Box::new(Expr::Number(Rational::zero())),
            },
            closed: endpoint_closed(&previous.end) || endpoint_closed(&next.end),
        },
    };
    Interval::new(start, end)
}

/// 解析表达式为区间并集（支持区间、不等式、有限点集、`∪`/`∩`）。
///
/// # Errors
///
/// 端点含变量、含未支持元素、关系不可转区间等返回中文原因。
pub fn interval_union_from_expr(expr: &Expr) -> Result<IntervalUnion, String> {
    let canonical = canonicalize(expr);
    match &canonical {
        Expr::Interval(interval) => {
            ensure_constant_endpoints(interval)?;
            Ok(IntervalUnion::new(vec![interval.clone()]))
        }
        Expr::Relation { .. } => relation_to_interval(&canonical)
            .map(|interval| IntervalUnion::new(vec![interval]))
            .map_err(|error| error.to_string()),
        Expr::Set(items) => {
            if items.is_empty() {
                return Ok(IntervalUnion::empty());
            }
            let mut intervals = Vec::new();
            for item in items {
                let item_canonical = canonicalize(item);
                if matches!(
                    item_canonical,
                    Expr::Set(_) | Expr::SetUnion(_) | Expr::SetIntersection(_)
                ) {
                    return Err("集合元素不可是集合（需显式区间/数值）".to_owned());
                }
                if let Ok(value) = evaluate(&item_canonical) {
                    intervals.push(point_interval(value));
                    continue;
                }
                if let Ok(mut nested) = interval_union_from_expr(&item_canonical) {
                    intervals.append(&mut nested.intervals);
                    continue;
                }
                return Err(format!(
                    "集合元素不是可比较的区间/常量：{}",
                    item_canonical.to_text()
                ));
            }
            Ok(IntervalUnion::new(intervals))
        }
        Expr::SetUnion(items) | Expr::Solutions(items) => {
            let mut intervals = Vec::new();
            for item in items {
                let mut nested = interval_union_from_expr(item)?;
                intervals.append(&mut nested.intervals);
            }
            Ok(IntervalUnion::new(intervals))
        }
        Expr::SetIntersection(items) => {
            let mut iter = items.iter();
            let first = iter
                .next()
                .ok_or_else(|| "交集为空（缺操作数）".to_owned())?;
            let mut accumulator = interval_union_from_expr(first)?;
            for item in iter {
                let next = interval_union_from_expr(item)?;
                accumulator = intersect_unions(&accumulator, &next);
            }
            Ok(accumulator)
        }
        other => match evaluate(other) {
            Ok(value) => Ok(IntervalUnion::new(vec![point_interval(value)])),
            Err(_) => Err(format!("不是可达的区间/集合表达式：{}", other.to_text())),
        },
    }
}

fn ensure_constant_endpoints(interval: &Interval) -> Result<(), String> {
    for (label, end) in [("左", &interval.start), ("右", &interval.end)] {
        if let Some(expr) = end.expr() {
            if !expr.variables().is_empty() {
                return Err(format!("{label}端点含变量：{}", expr.to_text()));
            }
            if evaluate(expr).is_err() {
                return Err(format!(
                    "{label}端点不是可精确求值的常量：{}",
                    expr.to_text()
                ));
            }
        }
    }
    Ok(())
}

fn point_interval(value: Rational) -> Interval {
    let endpoint = IntervalEnd::Value {
        expr: Box::new(Expr::Number(value)),
        closed: true,
    };
    Interval::new(endpoint.clone(), endpoint)
}

/// 两个区间并集的交集（规范形式）。
#[must_use]
pub fn intersect_unions(left: &IntervalUnion, right: &IntervalUnion) -> IntervalUnion {
    let mut result = Vec::new();
    for a in &left.intervals {
        for b in &right.intervals {
            if let Some(intersection) = intersect_intervals(a, b) {
                result.push(intersection);
            }
        }
    }
    IntervalUnion::new(result)
}

fn intersect_intervals(left: &Interval, right: &Interval) -> Option<Interval> {
    let (start, start_closed) = match compare_interval_end(&left.start, &right.start) {
        Ordering::Less => (right.start.clone(), endpoint_closed(&right.start)),
        Ordering::Greater => (left.start.clone(), endpoint_closed(&left.start)),
        Ordering::Equal => (
            left.start.clone(),
            endpoint_closed(&left.start) && endpoint_closed(&right.start),
        ),
    };
    let (end, end_closed) = match compare_interval_end(&left.end, &right.end) {
        Ordering::Less => (left.end.clone(), endpoint_closed(&left.end)),
        Ordering::Greater => (right.end.clone(), endpoint_closed(&right.end)),
        Ordering::Equal => (
            left.end.clone(),
            endpoint_closed(&left.end) && endpoint_closed(&right.end),
        ),
    };
    let interval = Interval::new(
        with_closed(start, start_closed),
        with_closed(end, end_closed),
    );
    if interval.is_empty() {
        None
    } else {
        Some(interval)
    }
}

fn with_closed(end: IntervalEnd, closed: bool) -> IntervalEnd {
    match end {
        IntervalEnd::Value { expr, .. } => IntervalEnd::Value { expr, closed },
        IntervalEnd::NegInfinity => IntervalEnd::NegInfinity,
        IntervalEnd::PosInfinity => IntervalEnd::PosInfinity,
    }
}

/// 解析表达式为有限集合（元素为规范化表达式；忽略顺序/去重）。
///
/// # Errors
///
/// 含区间/嵌套集合（应走区间比较器）、含未支持函数等返回中文原因。
pub fn finite_set_from_expr(expr: &Expr) -> Result<BTreeSet<Expr>, String> {
    let canonical = canonicalize(expr);
    match &canonical {
        Expr::Set(items) => {
            let mut set = BTreeSet::new();
            for item in items {
                let item = canonicalize(item);
                match &item {
                    Expr::Interval(_) | Expr::Relation { .. } => {
                        return Err(format!(
                            "集合含区间/不等式元素，需区间比较器：{}",
                            item.to_text()
                        ));
                    }
                    Expr::Set(_) | Expr::SetUnion(_) | Expr::SetIntersection(_) => {
                        return Err("集合嵌套集合，无法作为有限元素集合".to_owned());
                    }
                    Expr::Function { name, .. }
                        if !matches!(
                            name.as_str(),
                            "sin" | "cos" | "tan" | "cot" | "sec" | "csc" | "log" | "ln" | "lg"
                        ) =>
                    {
                        return Err(format!("集合含未支持元素：{}", item.to_text()));
                    }
                    _ => set.insert(item),
                };
            }
            Ok(set)
        }
        Expr::SetUnion(items) => {
            let mut set = BTreeSet::new();
            for item in items {
                set.extend(finite_set_from_expr(item)?);
            }
            Ok(set)
        }
        Expr::SetIntersection(items) => {
            let mut iter = items.iter();
            let first = iter.next().ok_or_else(|| "交集为空".to_owned())?;
            let mut set = finite_set_from_expr(first)?;
            for item in iter {
                let next = finite_set_from_expr(item)?;
                set = set.intersection(&next).cloned().collect();
            }
            Ok(set)
        }
        Expr::Interval(_) => Err("不是有限集合（区间请用区间比较器）".to_owned()),
        other => Err(format!("不是集合表达式：{}", other.to_text())),
    }
}

/// 集合元素是否全部为可精确求值的常量。
#[must_use]
pub fn all_constant_elements(set: &BTreeSet<Expr>) -> bool {
    set.iter().all(|element| evaluate(element).is_ok())
}

/// 区间并集是否为退化单点（单元素集合）。
#[must_use]
pub fn union_is_single_point(union: &IntervalUnion) -> Option<&Expr> {
    match union.intervals.as_slice() {
        [interval] => interval_is_point(interval),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::parse::parse_expr;

    fn union(input: &str) -> IntervalUnion {
        interval_union_from_expr(&parse_expr(input).unwrap()).unwrap()
    }

    #[test]
    fn merges_overlapping_and_touching_intervals() {
        let merged = union("(-\\infty,2] \\cup [2,3)");
        assert_eq!(merged.to_text(), "(-\\infty,3)");
        let disjoint = union("[1,2] \\cup [3,4]");
        assert_eq!(disjoint.to_text(), "[1,2] \\cup [3,4]");
        let point_join = union("[1,2] \\cup (2,3]");
        assert_eq!(point_join.to_text(), "[1,3]");
    }

    #[test]
    fn intersection_canonicalizes() {
        let value = union("[1,2] \\cap [2,3]");
        assert_eq!(value.to_text(), "[2,2]");
        let empty = union("[1,2] \\cap [3,4]");
        assert!(empty.is_empty());
        let infinite = union("(-\\infty,2] \\cap [1,\\infty)");
        assert_eq!(infinite.to_text(), "[1,2]");
    }

    #[test]
    fn finite_sets_ignore_order_and_union() {
        let set =
            finite_set_from_expr(&parse_expr("\\{1,2,2\\} \\cup \\{2,3\\}").unwrap()).unwrap();
        assert_eq!(set.len(), 3);
        let left = finite_set_from_expr(&parse_expr("\\{1,2,3\\}").unwrap()).unwrap();
        let right = finite_set_from_expr(&parse_expr("\\{3,2,1,1\\}").unwrap()).unwrap();
        assert_eq!(left, right);
        let inter =
            finite_set_from_expr(&parse_expr("\\{1,2,3\\} \\cap \\{2,3,4\\}").unwrap()).unwrap();
        assert_eq!(inter.len(), 2);
    }

    #[test]
    fn rejects_variable_endpoints() {
        assert!(interval_union_from_expr(&parse_expr("[a,b]").unwrap()).is_err());
        assert!(finite_set_from_expr(&parse_expr("[1,2]").unwrap()).is_err());
        assert!(interval_union_from_expr(&parse_expr("[1,2] \\cup [a,b]").unwrap()).is_err());
    }
}
