//! 精确十进制值对象（`AnswerValue` 数值与 `Tolerance` 绝对容差的表示）。
//!
//! 规范 §2/§4：答案数值必须是「精确十进制或精确有理数，禁止二进制浮点比较」
//! （`SPEC-subject-math.md` §4）。本类型用「i128 尾数 + 十进制标度」精确表示
//! 有限小数，解析即规范化（去尾随 0、去前导 0、消除负零），因此
//! `Eq`/`Hash`/`Ord` 与数值语义一致。
//!
//! 不用于分数：分数一律 [`crate::score::MilliScore`]（规范 §7）。

use core::cmp::Ordering;
use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::DecimalError;

/// 允许的最大小数位（保证尾数在 i128 内精确表示）。
pub const MAX_SCALE: u32 = 18;

/// 精确有限十进制数。
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Decimal {
    mantissa: i128,
    scale: u32,
}

impl Decimal {
    /// 零。
    pub const ZERO: Self = Self {
        mantissa: 0,
        scale: 0,
    };

    /// 由整数构造。
    ///
    /// # Errors
    ///
    /// 当前实现不会失败（保留 `Result` 以便将来扩展精度限制）。
    pub fn from_integer(value: i128) -> Result<Self, DecimalError> {
        Self::from_mantissa_scale(value, 0)
    }

    /// 由「尾数 × 10^-scale」构造并规范化。
    ///
    /// # Errors
    ///
    /// `scale` 超过 [`MAX_SCALE`] 返回 [`DecimalError::ScaleTooLarge`]。
    pub fn from_mantissa_scale(mantissa: i128, scale: u32) -> Result<Self, DecimalError> {
        if scale > MAX_SCALE {
            return Err(DecimalError::ScaleTooLarge {
                input: format!("{mantissa}e-{scale}"),
                max_scale: MAX_SCALE,
            });
        }
        Ok(canonicalize(mantissa, scale))
    }

    /// 解析十进制文本（可带符号；整数或小数；不接受指数写法）。
    ///
    /// 接受前后 ASCII 空白；规范化后 `"2.50"` 与 `"2.5"` 相等。
    ///
    /// # Errors
    ///
    /// 语法非法返回 [`DecimalError::Format`]；小数位过多返回
    /// [`DecimalError::ScaleTooLarge`]；尾数溢出返回 [`DecimalError::Overflow`]。
    pub fn parse(input: &str) -> Result<Self, DecimalError> {
        let text = input.trim();
        if text.is_empty() {
            return Err(DecimalError::Format {
                input: input.to_owned(),
            });
        }
        let (negative, unsigned) = match text.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => match text.strip_prefix('+') {
                Some(rest) => (false, rest),
                None => (false, text),
            },
        };
        if unsigned.is_empty() {
            return Err(DecimalError::Format {
                input: input.to_owned(),
            });
        }
        let (integer_part, fraction_part) = match unsigned.split_once('.') {
            Some((integer, fraction)) => (integer, Some(fraction)),
            None => (unsigned, None),
        };
        if integer_part.is_empty() && fraction_part.is_none() {
            return Err(DecimalError::Format {
                input: input.to_owned(),
            });
        }
        if !integer_part.bytes().all(|b| b.is_ascii_digit()) {
            return Err(DecimalError::Format {
                input: input.to_owned(),
            });
        }
        let fraction_text = match fraction_part {
            None => "",
            Some(fraction) => {
                if fraction.is_empty() || !fraction.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(DecimalError::Format {
                        input: input.to_owned(),
                    });
                }
                fraction
            }
        };
        let scale = u32::try_from(fraction_text.len()).map_err(|_| DecimalError::Overflow)?;
        if scale > MAX_SCALE {
            return Err(DecimalError::ScaleTooLarge {
                input: input.to_owned(),
                max_scale: MAX_SCALE,
            });
        }
        let mut mantissa: i128 = 0;
        for byte in integer_part.bytes().chain(fraction_text.bytes()) {
            let digit = i128::from(byte - b'0');
            mantissa = mantissa
                .checked_mul(10)
                .and_then(|value| value.checked_add(digit))
                .ok_or(DecimalError::Overflow)?;
        }
        if negative {
            mantissa = mantissa.checked_neg().ok_or(DecimalError::Overflow)?;
        }
        Ok(canonicalize(mantissa, scale))
    }

    /// 尾数（值为 `mantissa × 10^-scale`）。
    #[must_use]
    pub const fn mantissa(self) -> i128 {
        self.mantissa
    }

    /// 十进制小数位数（规范形：无尾随 0）。
    #[must_use]
    pub const fn scale(self) -> u32 {
        self.scale
    }

    /// 是否为零。
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.mantissa == 0
    }

    /// 是否为负。
    #[must_use]
    pub const fn is_negative(self) -> bool {
        self.mantissa < 0
    }

    /// 规范十进制文本（无指数、无尾随 0、无前导 +、负零归一为 `"0"`）。
    #[must_use]
    pub fn to_canonical_string(self) -> String {
        let sign = if self.mantissa < 0 { "-" } else { "" };
        let digits = self.mantissa.unsigned_abs().to_string();
        if self.scale == 0 {
            return format!("{sign}{digits}");
        }
        let scale = self.scale as usize;
        if digits.len() > scale {
            let (integer, fraction) = digits.split_at(digits.len() - scale);
            format!("{sign}{integer}.{fraction}")
        } else {
            let zeros = "0".repeat(scale - digits.len());
            format!("{sign}0.{zeros}{digits}")
        }
    }
}

fn canonicalize(mut mantissa: i128, mut scale: u32) -> Decimal {
    if mantissa == 0 {
        return Decimal {
            mantissa: 0,
            scale: 0,
        };
    }
    while scale > 0 && mantissa % 10 == 0 {
        mantissa /= 10;
        scale -= 1;
    }
    Decimal { mantissa, scale }
}

fn absolute_parts(value: Decimal) -> (String, String) {
    let text = value.to_canonical_string();
    let unsigned = text.strip_prefix('-').unwrap_or(&text).to_owned();
    match unsigned.split_once('.') {
        Some((integer, fraction)) => (integer.to_owned(), fraction.to_owned()),
        None => (unsigned, String::new()),
    }
}

fn compare_absolute(left: Decimal, right: Decimal) -> Ordering {
    let (left_integer, left_fraction) = absolute_parts(left);
    let (right_integer, right_fraction) = absolute_parts(right);
    let integer_order = left_integer
        .len()
        .cmp(&right_integer.len())
        .then_with(|| left_integer.cmp(&right_integer));
    if integer_order != Ordering::Equal {
        return integer_order;
    }
    let max_fraction = left_fraction.len().max(right_fraction.len());
    for index in 0..max_fraction {
        let left_digit = left_fraction.as_bytes().get(index).copied().unwrap_or(b'0');
        let right_digit = right_fraction
            .as_bytes()
            .get(index)
            .copied()
            .unwrap_or(b'0');
        match left_digit.cmp(&right_digit) {
            Ordering::Equal => {}
            other => return other,
        }
    }
    Ordering::Equal
}

impl Ord for Decimal {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.is_negative(), other.is_negative()) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => compare_absolute(*self, *other),
            (true, true) => compare_absolute(*other, *self),
        }
    }
}

impl PartialOrd for Decimal {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Decimal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_canonical_string())
    }
}

impl fmt::Debug for Decimal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Decimal({})", self.to_canonical_string())
    }
}

impl FromStr for Decimal {
    type Err = DecimalError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Self::parse(input)
    }
}

impl Serialize for Decimal {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // 序列化为规范文本，避免任何二进制浮点转换（规范 §4 精确性要求）。
        serializer.serialize_str(&self.to_canonical_string())
    }
}

impl<'de> Deserialize<'de> for Decimal {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn parses_and_canonicalizes() {
        let cases = [
            ("0", "0", 0, 0),
            ("-0", "0", 0, 0),
            ("2.50", "2.5", 25, 1),
            ("0002.500", "2.5", 25, 1),
            (".5", "0.5", 5, 1),
            ("-1.25", "-1.25", -125, 2),
            ("10", "10", 10, 0),
            ("0.000", "0", 0, 0),
        ];
        for (input, canonical, mantissa, scale) in cases {
            let value = Decimal::parse(input).unwrap();
            assert_eq!(value.to_canonical_string(), canonical, "{input}");
            assert_eq!(value.mantissa(), mantissa, "{input}");
            assert_eq!(value.scale(), scale, "{input}");
        }
    }

    #[test]
    fn rejects_bad_inputs() {
        for bad in [
            "", " ", "abc", "1.", ".", "1..2", "1e3", "1,5", "1.2.3", "+", "-", "0x10",
        ] {
            assert!(
                matches!(Decimal::parse(bad), Err(DecimalError::Format { .. })),
                "{bad}"
            );
        }
        assert!(matches!(
            Decimal::parse("1.1234567890123456789"),
            Err(DecimalError::ScaleTooLarge { .. })
        ));
        assert!(matches!(
            Decimal::parse("999999999999999999999999999999999999999999"),
            Err(DecimalError::Overflow)
        ));
    }

    #[test]
    fn equality_ignores_trailing_zeros() {
        assert_eq!(
            Decimal::parse("2.5").unwrap(),
            Decimal::parse("2.50").unwrap()
        );
        assert_eq!(Decimal::parse("0").unwrap(), Decimal::parse("-0").unwrap());
        assert_ne!(
            Decimal::parse("2.5").unwrap(),
            Decimal::parse("2.51").unwrap()
        );
    }

    #[test]
    fn ordering_matches_numeric_order() {
        let mut values = [
            "10", "9.99", "-1", "0", "0.001", "-0.5", "100", "2.5", "2.50", "-2.5",
        ]
        .map(|text| Decimal::parse(text).unwrap());
        values.sort();
        let texts: Vec<String> = values.iter().map(ToString::to_string).collect();
        assert_eq!(
            texts,
            [
                "-2.5", "-1", "-0.5", "0", "0.001", "2.5", "2.5", "9.99", "10", "100"
            ]
        );
    }

    #[test]
    fn serde_is_canonical_string() {
        let value = Decimal::parse("2.500").unwrap();
        let json = serde_json::to_string(&value).unwrap();
        assert_eq!(json, "\"2.5\"");
        assert_eq!(serde_json::from_str::<Decimal>(&json).unwrap(), value);
        assert!(serde_json::from_str::<Decimal>("\"abc\"").is_err());
        assert!(serde_json::from_str::<Decimal>("2.5").is_err());
    }

    #[test]
    fn from_integer_and_zero() {
        assert_eq!(Decimal::from_integer(7).unwrap().to_string(), "7");
        assert!(Decimal::ZERO.is_zero());
        assert!(!Decimal::ZERO.is_negative());
        assert!(Decimal::parse("-3").unwrap().is_negative());
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig { cases: 256, ..proptest::prelude::ProptestConfig::default() })]

        /// 性质：尾数/标度构造 → 文本 → 解析回同一规范值。
        #[test]
        fn prop_decimal_roundtrip(mantissa in -1_000_000_000_000_000_000i128..=1_000_000_000_000_000_000i128, scale in 0u32..=MAX_SCALE) {
            let value = Decimal::from_mantissa_scale(mantissa, scale).unwrap();
            let text = value.to_canonical_string();
            proptest::prop_assert_eq!(Decimal::parse(&text).unwrap(), value);
            proptest::prop_assert!(!text.contains('e'));
        }

        /// 性质：规范文本序列化 → 反序列化稳定。
        #[test]
        fn prop_decimal_serde_roundtrip(mantissa in -1_000_000i128..=1_000_000i128, scale in 0u32..=6) {
            let value = Decimal::from_mantissa_scale(mantissa, scale).unwrap();
            let json = serde_json::to_string(&value).unwrap();
            proptest::prop_assert_eq!(serde_json::from_str::<Decimal>(&json).unwrap(), value);
        }

        /// 性质：排序与「乘以正标度后的整数序」一致（同一标度下）。
        #[test]
        fn prop_ordering_consistent(a in -100_000i128..=100_000i128, b in -100_000i128..=100_000i128, scale in 0u32..=3) {
            let left = Decimal::from_mantissa_scale(a, scale).unwrap();
            let right = Decimal::from_mantissa_scale(b, scale).unwrap();
            proptest::prop_assert_eq!(left.cmp(&right), a.cmp(&b));
        }
    }
}
