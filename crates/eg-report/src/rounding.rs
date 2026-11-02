//! 整数舍入与显示（`SPEC-report.md` §3.4：分数不额外舍入、率四舍五入到 0.1%、
//! 均分/标准差/区分度四舍五入到 0.01、全程整数运算、禁止浮点汇总）。
//!
//! 所有函数只接受非负输入；分母为 0 或负数时返回 `None`，由调用方输出「不适用」。

use crate::error::{ReportError, Result};
use eg_domain::MilliScore;

/// 分数精度：`MilliScore` 的 1 分 = 1000 单位；显示到 0.01 分 = 10 单位。
pub const MILLI_PER_POINT: i64 = 1000;

/// 率的分母：百万分比（ppm），100% = 1_000_000。
pub const PPM_SCALE: u32 = 1_000_000;

/// 整数 half-up 除法（非负）。
///
/// 返回 `(num + den / 2) / den`；`den == 0` 或任一输入为负时返回 `None`。
#[must_use]
pub fn div_half_up(num: i128, den: i128) -> Option<i128> {
    if num < 0 || den <= 0 {
        return None;
    }
    Some((num + den / 2) / den)
}

/// 把已算出的整数值 half-up 到给定步长（如 10 表示 0.01 分）。
#[must_use]
pub fn round_to_step(value: i128, step: i128) -> Option<i128> {
    if value < 0 || step <= 0 {
        return None;
    }
    Some(((value + step / 2) / step) * step)
}

/// 均分：`sum / n`，四舍五入到 0.01 分（10 个千分之一分）。
#[must_use]
pub fn mean_milli(sum: i128, n: usize) -> Option<i64> {
    let n = i128::try_from(n).ok()?;
    let raw = div_half_up(sum, n)?;
    let rounded = round_to_step(raw, 10)?;
    i64::try_from(rounded).ok()
}

/// 标准差（总体）：四舍五入到 0.01 分；输入为已归一到毫分的分数。
#[must_use]
pub fn stddev_milli(values: &[i64], mean: i64) -> Option<i64> {
    if values.is_empty() {
        return None;
    }
    let n = i128::try_from(values.len()).ok()?;
    let mean = i128::from(mean);
    let mut sum_sq: i128 = 0;
    for &value in values {
        let diff = i128::from(value) - mean;
        sum_sq = sum_sq.checked_add(diff.checked_mul(diff)?)?;
    }
    let variance = sum_sq / n;
    let root = isqrt_u128(u128::try_from(variance).ok()?);
    let rounded = round_to_step(i128::try_from(root).ok()?, 10)?;
    i64::try_from(rounded).ok()
}

/// 非负整数的向下取整平方根（牛顿迭代）。
#[must_use]
pub fn isqrt_u128(value: u128) -> u128 {
    if value < 2 {
        return value;
    }
    let mut x = value;
    let mut y = x.div_ceil(2);
    while y < x {
        x = y;
        y = u128::midpoint(x, value / x);
    }
    x
}

/// 计算率（ppm，half-up）；分母为 0 时返回 `None`（规范要求输出「不适用」）。
#[must_use]
pub fn rate_ppm(numerator: i128, denominator: i128) -> Option<u32> {
    let scaled = numerator.checked_mul(i128::from(PPM_SCALE))?;
    let ppm = div_half_up(scaled, denominator)?;
    u32::try_from(ppm.clamp(0, i128::from(PPM_SCALE))).ok()
}

/// 把 ppm 四舍五入到 0.1%（1000 ppm 粒度），用于显示与阈值比较。
#[must_use]
pub fn round_ppm_to_tenth_percent(ppm: u32) -> u32 {
    let rounded = (ppm + 500) / 1000 * 1000;
    rounded.min(PPM_SCALE)
}

/// 区分度：高低分组得分率之差 `D`（范围 -1..=1），返回 `D × 100`（百分数整数，
/// 即 `60` 表示 `0.60`；舍入到 0.01 时另行除以 100 显示）。
#[must_use]
pub fn discrimination_hundredths(high_ppm: u32, low_ppm: u32) -> i32 {
    let diff = i64::from(high_ppm) - i64::from(low_ppm);
    // diff 是 ppm（100% = 1_000_000）；除以 10_000 得 D × 100。
    match div_half_up(i128::from(diff.abs()), 10_000) {
        Some(value) => {
            let signed = if diff < 0 { -value } else { value };
            i32::try_from(signed.clamp(-100, 100)).unwrap_or(0)
        }
        None => 0,
    }
}

/// 分数显示：最多 3 位小数、去尾零（规范：分数不额外舍入）。
#[must_use]
pub fn display_score(milli: i64) -> String {
    let negative = milli < 0;
    let value = milli.unsigned_abs();
    let whole = value / 1000;
    let frac = value % 1000;
    let mut text = if frac == 0 {
        whole.to_string()
    } else if frac.is_multiple_of(100) {
        format!("{whole}.{}", frac / 100)
    } else if frac.is_multiple_of(10) {
        format!("{whole}.{:02}", frac / 10)
    } else {
        format!("{whole}.{frac:03}")
    };
    if negative {
        text.insert(0, '-');
    }
    text
}

/// 得分率显示：四舍五入到 0.1%。
#[must_use]
pub fn display_percent(ppm: u32) -> String {
    let rounded = round_ppm_to_tenth_percent(ppm);
    let tenths = rounded / 1000;
    format!("{}.{}%", tenths / 10, tenths % 10)
}

/// 平均分/标准差/区分度显示：两位小数。
#[must_use]
pub fn display_two_decimals(value: i64) -> String {
    let negative = value < 0;
    let abs = value.unsigned_abs();
    let mut text = format!("{}.{:02}", abs / 1000, (abs % 1000) / 10);
    if abs % 10 >= 5 {
        // 千分位 ≥5 时进位到百分位（值已按 10 对齐时不会发生）。
        let cents = abs / 10 + 1;
        text = format!("{}.{:02}", cents / 100, cents % 100);
    }
    if negative {
        text.insert(0, '-');
    }
    text
}

/// 由分数构造安全求和累加器（避免 `i64` 溢出）。
pub(crate) fn sum_scores(scores: impl IntoIterator<Item = MilliScore>) -> Result<i128> {
    let mut total: i128 = 0;
    for score in scores {
        total = total
            .checked_add(i128::from(score.as_milli()))
            .ok_or_else(|| ReportError::Invariant("分数求和无溢出保护失败".to_owned()))?;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_up_boundaries() {
        assert_eq!(div_half_up(1, 2), Some(1)); // 0.5 → 1
        assert_eq!(div_half_up(1, 3), Some(0)); // 0.33 → 0
        assert_eq!(div_half_up(2, 3), Some(1)); // 0.67 → 1
        assert_eq!(div_half_up(0, 5), Some(0));
        assert_eq!(div_half_up(1, 0), None);
        assert_eq!(div_half_up(-1, 2), None);
    }

    #[test]
    fn mean_rounds_to_cent() {
        // 2.005 分（2005 毫分）的一半是 1002.5 → 四舍五入到 0.01 分为 1.00。
        assert_eq!(mean_milli(2005, 2), Some(1000));
        // 2.020 分 / 2 = 1.010 分 → 1010（0.01 分粒度）。
        assert_eq!(mean_milli(2020, 2), Some(1010));
        // 平均 2.004 → 2004/2=1002 → 到 1000
        assert_eq!(mean_milli(2004, 2), Some(1000));
        assert_eq!(mean_milli(0, 0), None);
    }

    #[test]
    fn stddev_rounds() {
        let values = [1000_i64, 2000, 3000];
        let mean = 2000;
        // 方差 = (1e6+0+1e6)/3 = 666666.67 → sqrt ≈ 816.49 → 820（0.01 分）
        assert_eq!(stddev_milli(&values, mean), Some(820));
        assert_eq!(stddev_milli(&[], 0), None);
    }

    #[test]
    fn rate_and_percent_boundaries() {
        assert_eq!(rate_ppm(1, 2), Some(500_000));
        assert_eq!(rate_ppm(1, 3), Some(333_333)); // 333333.33 → half-up
        assert_eq!(rate_ppm(0, 5), Some(0));
        assert_eq!(rate_ppm(5, 0), None);
        assert_eq!(display_percent(333_333), "33.3%");
        assert_eq!(display_percent(1_000_000), "100.0%");
        assert_eq!(display_percent(0), "0.0%");
        // 0.1% 边界：0.05% 进位到 0.1%
        assert_eq!(round_ppm_to_tenth_percent(500), 1000);
        assert_eq!(round_ppm_to_tenth_percent(499), 0);
    }

    #[test]
    fn discrimination_two_decimals() {
        // 高分组 80%、低分组 20% → D = 0.60 → 60（百分数整数）。
        assert_eq!(discrimination_hundredths(800_000, 200_000), 60);
        assert_eq!(discrimination_hundredths(200_000, 800_000), -60);
        assert_eq!(discrimination_hundredths(0, 0), 0);
        assert_eq!(discrimination_hundredths(1_000_000, 0), 100);
    }

    #[test]
    fn score_display_trims_zeros() {
        assert_eq!(display_score(2500), "2.5");
        assert_eq!(display_score(250), "0.25");
        assert_eq!(display_score(2005), "2.005");
        assert_eq!(display_score(0), "0");
    }

    #[test]
    fn isqrt_values() {
        assert_eq!(isqrt_u128(0), 0);
        assert_eq!(isqrt_u128(1), 1);
        assert_eq!(isqrt_u128(15), 3);
        assert_eq!(isqrt_u128(16), 4);
        assert_eq!(isqrt_u128(1_000_000), 1000);
    }
}
