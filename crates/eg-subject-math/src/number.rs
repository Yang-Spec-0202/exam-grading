//! 精确数值：有理数（`num-rational` 的 `BigRational` 包装）。
//!
//! 规范依据：`SPEC-subject-math.md` §4「数值：整数、小数、分数、科学计数；
//! 精确十进制或精确有理数，禁止二进制浮点比较」、§10「大整数/分数运算设位数上限
//! （建议 256 位），超限 → 无法判定」。
//!
//! 因此本模块**不出现 f64**：所有数值运算走大整数/大分数，每次运算后检查
//! 分子/分母位长上限（[`MAX_BITS`]），超限返回类型化错误，由上层转为
//! 「无法判定」而不是静默近似。

use core::cmp::Ordering;
use core::fmt;

use num_bigint::{BigInt, BigUint};
use num_rational::BigRational;
use num_traits::{One, Signed, Zero};
use thiserror::Error;

/// 分数分子/分母各自允许的最大二进制位长（规范 §10 建议 256 位）。
pub const MAX_BITS: u64 = 256;

/// 科学计数指数的绝对值上限（防止构造巨大 10^n；超出即位长超限）。
const MAX_EXPONENT: i64 = 1_000;

/// 数值构造/运算错误。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum NumError {
    /// 分母为零。
    #[error("分母不能为 0")]
    DivideByZero,
    /// 超出精确表示位长上限。
    #[error("数值超出精确表示位长上限（{bits} 位 > {limit} 位）")]
    TooLarge {
        /// 实际位长。
        bits: u64,
        /// 上限。
        limit: u64,
    },
    /// 数值文本格式非法。
    #[error("数值文本格式非法：{input:?}（{reason}）")]
    Format {
        /// 原始输入。
        input: String,
        /// 可读原因。
        reason: String,
    },
}

/// 精确有理数（禁止二进制浮点比较）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Rational(BigRational);

impl Rational {
    /// 零。
    #[must_use]
    pub fn zero() -> Self {
        Self(BigRational::zero())
    }

    /// 一。
    #[must_use]
    pub fn one() -> Self {
        Self(BigRational::one())
    }

    /// 由 `i64` 构造。
    #[must_use]
    pub fn from_i64(value: i64) -> Self {
        Self(BigRational::from_integer(BigInt::from(value)))
    }

    /// 由分子/分母构造（自动约分；分母不得为 0）。
    ///
    /// # Errors
    ///
    /// 分母为 0 返回 [`NumError::DivideByZero`]；位长超限返回 [`NumError::TooLarge`]。
    pub fn new(numer: BigInt, denom: BigInt) -> Result<Self, NumError> {
        if denom.is_zero() {
            return Err(NumError::DivideByZero);
        }
        let value = Self(BigRational::new(numer, denom));
        value.check_bits()?;
        Ok(value)
    }

    /// 分子。
    #[must_use]
    pub fn numer(&self) -> &BigInt {
        self.0.numer()
    }

    /// 分母（恒为正）。
    #[must_use]
    pub fn denom(&self) -> &BigInt {
        self.0.denom()
    }

    /// 是否为零。
    #[must_use]
    pub fn is_zero(&self) -> bool {
        self.0.is_zero()
    }

    /// 是否为一。
    #[must_use]
    pub fn is_one(&self) -> bool {
        self.0.is_one()
    }

    /// 是否为负。
    #[must_use]
    pub fn is_negative(&self) -> bool {
        self.0.is_negative()
    }

    /// 是否为整数。
    #[must_use]
    pub fn is_integer(&self) -> bool {
        self.0.is_integer()
    }

    /// 转为整数（截断；调用方应先判断 [`Rational::is_integer`]）。
    #[must_use]
    pub fn to_integer(&self) -> BigInt {
        self.0.to_integer()
    }

    /// 绝对值。
    #[must_use]
    pub fn abs(&self) -> Self {
        Self(self.0.abs())
    }

    /// 相反数。
    #[must_use]
    pub fn neg(&self) -> Self {
        Self(-self.0.clone())
    }

    /// 受检加法。
    ///
    /// # Errors
    ///
    /// 位长超限返回 [`NumError::TooLarge`]。
    pub fn checked_add(&self, rhs: &Self) -> Result<Self, NumError> {
        Self::wrap(self.0.clone() + rhs.0.clone())
    }

    /// 受检减法。
    ///
    /// # Errors
    ///
    /// 位长超限返回 [`NumError::TooLarge`]。
    pub fn checked_sub(&self, rhs: &Self) -> Result<Self, NumError> {
        Self::wrap(self.0.clone() - rhs.0.clone())
    }

    /// 受检乘法。
    ///
    /// # Errors
    ///
    /// 位长超限返回 [`NumError::TooLarge`]。
    pub fn checked_mul(&self, rhs: &Self) -> Result<Self, NumError> {
        Self::wrap(self.0.clone() * rhs.0.clone())
    }

    /// 受检除法。
    ///
    /// # Errors
    ///
    /// 除数为 0 返回 [`NumError::DivideByZero`]；位长超限返回 [`NumError::TooLarge`]。
    pub fn checked_div(&self, rhs: &Self) -> Result<Self, NumError> {
        if rhs.is_zero() {
            return Err(NumError::DivideByZero);
        }
        Self::wrap(self.0.clone() / rhs.0.clone())
    }

    /// 受检整数次幂（负指数表示取倒数）。
    ///
    /// # Errors
    ///
    /// 0 的负指数幂返回 [`NumError::DivideByZero`]；位长超限返回 [`NumError::TooLarge`]。
    pub fn checked_pow(&self, exponent: i32) -> Result<Self, NumError> {
        if exponent == 0 {
            return Ok(Self::one());
        }
        let power = exponent.unsigned_abs();
        if exponent > 0 {
            Self::new(
                self.0.numer().clone().pow(power),
                self.0.denom().clone().pow(power),
            )
        } else {
            if self.is_zero() {
                return Err(NumError::DivideByZero);
            }
            Self::new(
                self.0.denom().clone().pow(power),
                self.0.numer().clone().pow(power),
            )
        }
    }

    /// 精确平方根（非完全平方或有理数时返回 `None`）。
    #[must_use]
    pub fn exact_sqrt(&self) -> Option<Self> {
        self.exact_nth_root(2)
    }

    /// 精确 n 次根（不能整除时返回 `None`）。
    #[must_use]
    pub fn exact_nth_root(&self, degree: u32) -> Option<Self> {
        if degree == 0 {
            return None;
        }
        if self.is_negative() {
            if degree.is_multiple_of(2) {
                return None;
            }
            let numer = exact_root(self.0.numer().magnitude(), degree)?;
            let denom = exact_root(self.0.denom().magnitude(), degree)?;
            return Self::new(-BigInt::from(numer), BigInt::from(denom)).ok();
        }
        let numer = exact_root(self.0.numer().magnitude(), degree)?;
        let denom = exact_root(self.0.denom().magnitude(), degree)?;
        Self::new(BigInt::from(numer), BigInt::from(denom)).ok()
    }

    /// 解析数值文本：整数、小数、科学计数（如 `12`、`2.50`、`1e-3`、`-0.5`）。
    ///
    /// # Errors
    ///
    /// 语法非法返回 [`NumError::Format`]；位长超限返回 [`NumError::TooLarge`]。
    pub fn parse(text: &str) -> Result<Self, NumError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(NumError::Format {
                input: text.to_owned(),
                reason: "空字符串".to_owned(),
            });
        }
        let (negative, rest) = match trimmed.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => match trimmed.strip_prefix('+') {
                Some(rest) => (false, rest),
                None => (false, trimmed),
            },
        };
        if rest.is_empty() {
            return Err(NumError::Format {
                input: text.to_owned(),
                reason: "缺少数字".to_owned(),
            });
        }
        let (mantissa_text, exponent) = split_exponent(rest, text)?;
        let (integer_part, fraction_part) = match mantissa_text.split_once('.') {
            Some((integer, fraction)) => (integer, fraction),
            None => (mantissa_text, ""),
        };
        if integer_part.is_empty() && fraction_part.is_empty() {
            return Err(NumError::Format {
                input: text.to_owned(),
                reason: "缺少数字".to_owned(),
            });
        }
        if !integer_part.bytes().all(|byte| byte.is_ascii_digit())
            || !fraction_part.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(NumError::Format {
                input: text.to_owned(),
                reason: "只允许十进制数字、小数点和科学计数指数".to_owned(),
            });
        }
        let mut digits = String::with_capacity(integer_part.len() + fraction_part.len());
        digits.push_str(if integer_part.is_empty() {
            "0"
        } else {
            integer_part
        });
        digits.push_str(fraction_part);
        let mut parsed =
            BigInt::parse_bytes(digits.as_bytes(), 10).ok_or_else(|| NumError::Format {
                input: text.to_owned(),
                reason: "数字解析失败".to_owned(),
            })?;
        if negative {
            parsed = -parsed;
        }
        let fraction_len = i64::try_from(fraction_part.len()).map_err(|_| NumError::Format {
            input: text.to_owned(),
            reason: "小数位过多".to_owned(),
        })?;
        let total_scale = fraction_len - exponent;
        if total_scale.unsigned_abs() > MAX_EXPONENT.unsigned_abs() {
            return Err(NumError::TooLarge {
                bits: total_scale.unsigned_abs().saturating_mul(4),
                limit: MAX_BITS,
            });
        }
        if total_scale >= 0 {
            let denom = BigInt::from(10_i32).pow(u32::try_from(total_scale).map_err(|_| {
                NumError::TooLarge {
                    bits: total_scale.unsigned_abs().saturating_mul(4),
                    limit: MAX_BITS,
                }
            })?);
            Self::new(parsed, denom)
        } else {
            let factor = BigInt::from(10_i32).pow(u32::try_from(-total_scale).map_err(|_| {
                NumError::TooLarge {
                    bits: total_scale.unsigned_abs().saturating_mul(4),
                    limit: MAX_BITS,
                }
            })?);
            Self::new(parsed * factor, BigInt::from(1_i32))
        }
    }

    /// 规范文本：整数直接输出；有限小数输出精确小数；其余输出 `分子/分母`。
    #[must_use]
    pub fn to_text(&self) -> String {
        if self.is_integer() {
            return self.to_integer().to_string();
        }
        if let Some(decimal) = self.to_finite_decimal() {
            return decimal;
        }
        format!("{}/{}", self.numer(), self.denom())
    }

    /// 规范 LaTeX：整数直接输出；其余用 `\frac{分子}{分母}`。
    #[must_use]
    pub fn to_latex(&self) -> String {
        if self.is_integer() {
            return self.to_integer().to_string();
        }
        if self.is_negative() {
            return format!(
                "-\\frac{{{}}}{{{}}}",
                self.numer().magnitude(),
                self.denom().magnitude()
            );
        }
        format!("\\frac{{{}}}{{{}}}", self.numer(), self.denom())
    }

    /// 若为有限小数（分母只含因子 2/5），输出精确十进制文本。
    #[must_use]
    pub fn to_finite_decimal(&self) -> Option<String> {
        let mut denom = self.0.denom().magnitude().clone();
        let two = BigUint::from(2_u32);
        let five = BigUint::from(5_u32);
        let mut twos = 0u32;
        let mut fives = 0u32;
        while (&denom % &two).is_zero() {
            denom /= &two;
            twos += 1;
        }
        while (&denom % &five).is_zero() {
            denom /= &five;
            fives += 1;
        }
        if !denom.is_one() {
            return None;
        }
        let scale = twos.max(fives);
        let mut numer = self.0.numer().magnitude().clone();
        if twos < scale {
            numer *= two.pow(scale - twos);
        }
        if fives < scale {
            numer *= five.pow(scale - fives);
        }
        let digits = numer.to_string();
        let negative = self.is_negative();
        if scale == 0 {
            return Some(if negative {
                format!("-{digits}")
            } else {
                digits
            });
        }
        let scale_usize = usize::try_from(scale).ok()?;
        let mut text = String::new();
        if negative {
            text.push('-');
        }
        if digits.len() > scale_usize {
            let split = digits.len() - scale_usize;
            let (integer, fraction) = digits.split_at(split);
            text.push_str(integer);
            text.push('.');
            text.push_str(fraction.trim_end_matches('0'));
            if text.ends_with('.') {
                text.pop();
            }
        } else {
            let zeros = "0".repeat(scale_usize - digits.len());
            text.push_str("0.");
            text.push_str(&zeros);
            text.push_str(digits.trim_end_matches('0'));
            if text.ends_with('.') {
                text.pop();
            }
        }
        Some(text)
    }

    /// 比较两个有理数。
    #[must_use]
    pub fn compare(&self, rhs: &Self) -> Ordering {
        self.0.cmp(&rhs.0)
    }

    fn wrap(value: BigRational) -> Result<Self, NumError> {
        let wrapped = Self(value);
        wrapped.check_bits()?;
        Ok(wrapped)
    }

    fn check_bits(&self) -> Result<(), NumError> {
        let bits = self.0.numer().bits().max(self.0.denom().bits());
        if bits > MAX_BITS {
            return Err(NumError::TooLarge {
                bits,
                limit: MAX_BITS,
            });
        }
        Ok(())
    }
}

impl From<i64> for Rational {
    fn from(value: i64) -> Self {
        Self::from_i64(value)
    }
}

impl fmt::Display for Rational {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_text())
    }
}

/// 拆分科学计数指数；返回（尾数文本，指数）。
fn split_exponent<'a>(text: &'a str, original: &str) -> Result<(&'a str, i64), NumError> {
    match text.find(['e', 'E']) {
        None => Ok((text, 0)),
        Some(index) => {
            let (mantissa, exponent_text) = text.split_at(index);
            let exponent_text = exponent_text.get(1..).unwrap_or_default();
            let (negative, digits) = match exponent_text.strip_prefix('-') {
                Some(rest) => (true, rest),
                None => match exponent_text.strip_prefix('+') {
                    Some(rest) => (false, rest),
                    None => (false, exponent_text),
                },
            };
            if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(NumError::Format {
                    input: original.to_owned(),
                    reason: "科学计数指数必须是整数".to_owned(),
                });
            }
            let magnitude: i64 = digits.parse().map_err(|_| NumError::TooLarge {
                bits: u64::try_from(digits.len())
                    .unwrap_or(u64::MAX)
                    .saturating_mul(4),
                limit: MAX_BITS,
            })?;
            if magnitude.abs() > MAX_EXPONENT {
                return Err(NumError::TooLarge {
                    bits: u64::try_from(magnitude)
                        .unwrap_or(u64::MAX)
                        .saturating_mul(4),
                    limit: MAX_BITS,
                });
            }
            Ok((mantissa, if negative { -magnitude } else { magnitude }))
        }
    }
}

/// 精确 n 次根（返回整数值；不是完全 n 次幂时返回 `None`）。
fn exact_root(value: &BigUint, degree: u32) -> Option<BigUint> {
    if degree == 0 {
        return None;
    }
    let root = value.nth_root(degree);
    if &root.pow(degree) == value {
        Some(root)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn parses_integers_decimals_scientific() {
        assert_eq!(Rational::parse("12").unwrap().to_text(), "12");
        assert_eq!(Rational::parse("2.50").unwrap().to_text(), "2.5");
        assert_eq!(Rational::parse("-0.5").unwrap().to_text(), "-0.5");
        assert_eq!(Rational::parse("1e3").unwrap().to_text(), "1000");
        assert_eq!(Rational::parse("1E+2").unwrap().to_text(), "100");
        assert_eq!(Rational::parse("0.001").unwrap().to_text(), "0.001");
        assert_eq!(Rational::parse("1e-3").unwrap().to_text(), "0.001");
        assert_eq!(Rational::parse(".5").unwrap().to_text(), "0.5");
        assert_eq!(Rational::parse("2.").unwrap().to_text(), "2");
    }

    #[test]
    fn rejects_bad_forms() {
        for bad in ["", "abc", "1e", "1e+", "--1", "1.2.3", "1/2"] {
            assert!(
                matches!(Rational::parse(bad), Err(NumError::Format { .. })),
                "{bad}"
            );
        }
    }

    #[test]
    fn arithmetic_is_exact_and_checked() {
        let half = Rational::parse("0.5").unwrap();
        let third = Rational::new(BigInt::from(1), BigInt::from(3)).unwrap();
        assert_eq!(half.checked_add(&half).unwrap().to_text(), "1");
        assert_eq!(half.checked_mul(&third).unwrap().to_text(), "1/6");
        assert_eq!(third.checked_add(&third).unwrap().to_text(), "2/3");
        assert!(matches!(
            half.checked_div(&Rational::zero()),
            Err(NumError::DivideByZero)
        ));
        assert_eq!(Rational::from_i64(2).checked_pow(3).unwrap().to_text(), "8");
        assert_eq!(
            Rational::from_i64(2).checked_pow(-1).unwrap().to_text(),
            "0.5"
        );
        assert!(matches!(
            Rational::zero().checked_pow(-1),
            Err(NumError::DivideByZero)
        ));
    }

    #[test]
    fn exact_roots() {
        assert_eq!(Rational::from_i64(9).exact_sqrt().unwrap().to_text(), "3");
        assert_eq!(
            Rational::parse("0.25")
                .unwrap()
                .exact_sqrt()
                .unwrap()
                .to_text(),
            "0.5"
        );
        assert!(Rational::from_i64(2).exact_sqrt().is_none());
        assert!(Rational::from_i64(-4).exact_sqrt().is_none());
        assert_eq!(
            Rational::from_i64(27).exact_nth_root(3).unwrap().to_text(),
            "3"
        );
        assert!(Rational::from_i64(27).exact_nth_root(2).is_none());
    }

    #[test]
    fn bit_limit_is_enforced() {
        let big = BigInt::from(2_i32).pow(200);
        let value = Rational::new(big, BigInt::from(1)).unwrap();
        assert!(value.to_text().len() > 30);
        assert!(matches!(
            value.checked_mul(&value),
            Err(NumError::TooLarge { .. })
        ));
    }

    #[test]
    fn decimal_rendering() {
        assert_eq!(Rational::parse("0.125").unwrap().to_text(), "0.125");
        assert_eq!(Rational::parse("0.125").unwrap().to_latex(), "\\frac{1}{8}");
        assert_eq!(Rational::from_i64(-3).to_text(), "-3");
        assert_eq!(
            Rational::new(BigInt::from(-1), BigInt::from(2))
                .unwrap()
                .to_latex(),
            "-\\frac{1}{2}"
        );
        assert_eq!(
            Rational::new(BigInt::from(1), BigInt::from(3))
                .unwrap()
                .to_text(),
            "1/3"
        );
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig { cases: 256, ..proptest::prelude::ProptestConfig::default() })]

        /// 性质：整数/小数往返稳定（有限小数）。
        #[test]
        fn prop_decimal_roundtrip(integer in -10_000i64..10_000, fraction in 0u32..1_000_000) {
            let text = format!("{integer}.{fraction:06}");
            let value = Rational::parse(&text).unwrap();
            let rendered = value.to_text();
            proptest::prop_assert_eq!(Rational::parse(&rendered).unwrap(), value.clone());
            proptest::prop_assert!(value.to_finite_decimal().is_some());
        }

        /// 性质：加法交换、比较与 Ord 一致。
        #[test]
        fn prop_rational_add_commutative(a in -1000i64..1000, b in -1000i64..1000) {
            let (a, b) = (Rational::from_i64(a), Rational::from_i64(b));
            proptest::prop_assert_eq!(a.checked_add(&b).unwrap(), b.checked_add(&a).unwrap());
            proptest::prop_assert_eq!(a.compare(&b), a.cmp(&b));
        }

        /// 性质：非零有理数乘以其倒数等于 1。
        #[test]
        fn prop_reciprocal(numer in -1000i64..1000, denom in -1000i64..1000) {
            proptest::prop_assume!(numer != 0 && denom != 0);
            let value = Rational::new(BigInt::from(numer), BigInt::from(denom)).unwrap();
            let reciprocal = Rational::one().checked_div(&value).unwrap();
            proptest::prop_assert_eq!(value.checked_mul(&reciprocal).unwrap(), Rational::one());
        }
    }
}
