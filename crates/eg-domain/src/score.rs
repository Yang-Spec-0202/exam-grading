//! 分数值对象：千分之一分的非负整数（`SPEC-domain.md` §2、§7）。
//!
//! - 表示：内部 `i64` 千分之一分原始值（`2.5 分 = 2500`）。
//! - 不变量：非负；受检加法/减法不溢出；禁止浮点比较与汇总。
//! - 解析：`"2.5"` → 2500；最多 3 位小数；负号、超精度、空串均为类型化错误；
//!   解析失败不 clamp、不四舍五入。
//! - 格式化：规范最短十进制文本（去掉尾随 0，整数不带小数点）。

use core::fmt;
use core::ops::Add;
use core::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::ScoreError;

/// 千分之一分的非负整数分数。
///
/// 规范 §7：分数一律用千分之一分整数新类型，禁止浮点参与比较与汇总。
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MilliScore(i64);

/// 1 分等于的千分之一分数量。
pub const MILLI_PER_POINT: i64 = 1_000;

impl MilliScore {
    /// 零分。
    pub const ZERO: Self = Self(0);

    /// 允许的最大分值（`i64::MAX` 千分之一分；仅作溢出上界，不用于业务约束）。
    pub const MAX: Self = Self(i64::MAX);

    /// 由千分之一分原始值构造。
    ///
    /// # Errors
    ///
    /// 值为负时返回 [`ScoreError::Negative`]。
    pub const fn from_milli(value: i64) -> Result<Self, ScoreError> {
        if value < 0 {
            return Err(ScoreError::Negative { value });
        }
        Ok(Self(value))
    }

    /// 由整分数构造（`from_points(2) == 2000`）。
    ///
    /// # Errors
    ///
    /// 负分或乘以 1000 后溢出时返回错误。
    pub const fn from_points(points: i64) -> Result<Self, ScoreError> {
        if points < 0 {
            return Err(ScoreError::Negative { value: points });
        }
        match points.checked_mul(MILLI_PER_POINT) {
            Some(milli) => Ok(Self(milli)),
            None => Err(ScoreError::Overflow),
        }
    }

    /// 千分之一分原始值（存储层按 INTEGER 落库，见规范 §10）。
    #[must_use]
    pub const fn as_milli(self) -> i64 {
        self.0
    }

    /// 是否为零分。
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// 受检加法。
    ///
    /// # Errors
    ///
    /// 溢出时返回 [`ScoreError::Overflow`]。
    pub const fn checked_add_score(self, rhs: Self) -> Result<Self, ScoreError> {
        match self.0.checked_add(rhs.0) {
            Some(sum) => Ok(Self(sum)),
            None => Err(ScoreError::Overflow),
        }
    }

    /// 受检减法（结果不得为负）。
    ///
    /// # Errors
    ///
    /// 结果为负返回 [`ScoreError::Negative`]；溢出返回 [`ScoreError::Overflow`]。
    pub const fn checked_sub_score(self, rhs: Self) -> Result<Self, ScoreError> {
        match self.0.checked_sub(rhs.0) {
            Some(diff) if diff >= 0 => Ok(Self(diff)),
            Some(diff) => Err(ScoreError::Negative { value: diff }),
            None => Err(ScoreError::Overflow),
        }
    }

    /// 受检求和（用于试卷总分、评分标准合计等不变量校验）。
    ///
    /// # Errors
    ///
    /// 任一步溢出时返回 [`ScoreError::Overflow`]。
    pub fn checked_sum(iter: impl IntoIterator<Item = Self>) -> Result<Self, ScoreError> {
        let mut total = 0i64;
        for score in iter {
            total = match total.checked_add(score.0) {
                Some(value) => value,
                None => return Err(ScoreError::Overflow),
            };
        }
        Ok(Self(total))
    }

    /// 是否为给定评分粒度的整数倍（规范 §4 档位与 §8 得分粒度）。
    ///
    /// # Errors
    ///
    /// 粒度为 0 时返回 [`ScoreError::Format`]（调用方不应传入 0 粒度）。
    pub fn is_multiple_of(self, granularity: Self) -> Result<bool, ScoreError> {
        if granularity.0 == 0 {
            return Err(ScoreError::Format {
                input: "0".to_owned(),
            });
        }
        Ok(self.0 % granularity.0 == 0)
    }

    /// 解析分数字符串（整数或最多 3 位小数，如 `"2.5"`、`"17"`、`"0.001"`）。
    ///
    /// 接受前后空白；不接受负号、指数写法、超过 3 位小数与空串。
    ///
    /// # Errors
    ///
    /// 负值返回 [`ScoreError::Negative`]；格式非法返回 [`ScoreError::Format`]；
    /// 超出 `i64` 返回 [`ScoreError::Overflow`]。
    pub fn parse(input: &str) -> Result<Self, ScoreError> {
        let text = input.trim();
        if text.is_empty() {
            return Err(ScoreError::Format {
                input: input.to_owned(),
            });
        }
        if let Some(rest) = text.strip_prefix('-') {
            // 负号单独识别，便于给出「负值」而不是「格式非法」。
            let magnitude = Self::parse_unsigned(rest).map_err(|err| match err {
                ScoreError::Format { .. } => ScoreError::Format {
                    input: input.to_owned(),
                },
                other => other,
            })?;
            return Err(ScoreError::Negative {
                value: -magnitude.as_milli(),
            });
        }
        Self::parse_unsigned(text).map_err(|err| match err {
            ScoreError::Format { .. } => ScoreError::Format {
                input: input.to_owned(),
            },
            other => other,
        })
    }

    /// 解析非负十进制文本（不带符号）。
    fn parse_unsigned(text: &str) -> Result<Self, ScoreError> {
        let mut parts = text.split('.');
        let integer = parts.next().unwrap_or_default();
        let fraction = parts.next();
        if parts.next().is_some() {
            return Err(ScoreError::Format {
                input: text.to_owned(),
            });
        }
        if integer.is_empty() || !integer.bytes().all(|b| b.is_ascii_digit()) {
            return Err(ScoreError::Format {
                input: text.to_owned(),
            });
        }
        let fraction = match fraction {
            None => "",
            Some(value) => {
                if value.is_empty() || value.len() > 3 || !value.bytes().all(|b| b.is_ascii_digit())
                {
                    return Err(ScoreError::Format {
                        input: text.to_owned(),
                    });
                }
                value
            }
        };
        let integer_value: i64 = integer.parse().map_err(|_| ScoreError::Overflow)?;
        let whole = integer_value
            .checked_mul(MILLI_PER_POINT)
            .ok_or(ScoreError::Overflow)?;
        let mut fraction_value: i64 = 0;
        if !fraction.is_empty() {
            let padded = format!("{fraction:0<3}");
            fraction_value = padded.parse().map_err(|_| ScoreError::Format {
                input: text.to_owned(),
            })?;
        }
        whole
            .checked_add(fraction_value)
            .map(Self)
            .ok_or(ScoreError::Overflow)
    }

    /// 格式化为规范分数字符串（整数不带小数点，最多 3 位小数且无尾随 0）。
    #[must_use]
    pub fn to_score_string(self) -> String {
        let whole = self.0 / MILLI_PER_POINT;
        let fraction = self.0 % MILLI_PER_POINT;
        if fraction == 0 {
            return whole.to_string();
        }
        let mut text = format!("{whole}.{fraction:03}");
        while text.ends_with('0') {
            text.pop();
        }
        text
    }
}

impl fmt::Display for MilliScore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_score_string())
    }
}

impl fmt::Debug for MilliScore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MilliScore({} = {})", self.0, self.to_score_string())
    }
}

impl FromStr for MilliScore {
    type Err = ScoreError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Self::parse(input)
    }
}

impl TryFrom<i64> for MilliScore {
    type Error = ScoreError;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        Self::from_milli(value)
    }
}

impl Add for MilliScore {
    type Output = Result<Self, ScoreError>;

    /// 加法返回受检结果（不 panic、不回绕），与规范 §2「加减不溢出」一致。
    fn add(self, rhs: Self) -> Self::Output {
        self.checked_add_score(rhs)
    }
}

impl Serialize for MilliScore {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // 规范 §10：分数存 INTEGER（千分之一分原始值）。
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for MilliScore {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = i64::deserialize(deserializer)?;
        Self::from_milli(raw).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn parse_and_format_roundtrip() {
        let cases = [
            ("0", 0, "0"),
            ("1", 1000, "1"),
            ("2.5", 2500, "2.5"),
            ("17", 17000, "17"),
            ("0.001", 1, "0.001"),
            ("0.010", 10, "0.01"),
            ("12.345", 12_345, "12.345"),
            (" 3.50 ", 3500, "3.5"),
        ];
        for (text, milli, canonical) in cases {
            let score = MilliScore::parse(text).unwrap();
            assert_eq!(score.as_milli(), milli, "{text}");
            assert_eq!(score.to_score_string(), canonical, "{text}");
            assert_eq!(MilliScore::parse(&score.to_score_string()).unwrap(), score);
        }
    }

    #[test]
    fn parse_rejects_negative_with_typed_error() {
        let err = MilliScore::parse("-1").unwrap_err();
        assert_eq!(err, ScoreError::Negative { value: -1000 });
        assert_eq!(
            MilliScore::parse("-0.5").unwrap_err(),
            ScoreError::Negative { value: -500 }
        );
    }

    #[test]
    fn parse_rejects_malformed_inputs() {
        for bad in [
            "", "  ", "abc", "1.2345", "1.", ".5", "1..2", "1e3", "+1", "1,5", "1.2.3",
        ] {
            assert!(
                matches!(MilliScore::parse(bad), Err(ScoreError::Format { .. })),
                "{bad} 应被拒绝"
            );
        }
    }

    #[test]
    fn parse_rejects_overflow() {
        let huge = "9223372036854776";
        assert!(matches!(MilliScore::parse(huge), Err(ScoreError::Overflow)));
    }

    #[test]
    fn from_points_roundtrips() {
        assert_eq!(MilliScore::from_points(2).unwrap().as_milli(), 2000);
        assert_eq!(MilliScore::from_points(0).unwrap(), MilliScore::ZERO);
        assert_eq!(
            MilliScore::from_points(-1).unwrap_err(),
            ScoreError::Negative { value: -1 }
        );
        assert!(matches!(
            MilliScore::from_points(i64::MAX),
            Err(ScoreError::Overflow)
        ));
    }

    #[test]
    fn checked_add_and_sub_behave() {
        let a = MilliScore::from_milli(2500).unwrap();
        let b = MilliScore::from_milli(500).unwrap();
        assert_eq!(a.checked_add_score(b).unwrap().as_milli(), 3000);
        assert_eq!(a.checked_sub_score(b).unwrap().as_milli(), 2000);
        assert_eq!(
            b.checked_sub_score(a).unwrap_err(),
            ScoreError::Negative { value: -2000 }
        );
        assert!(matches!(
            MilliScore::MAX.checked_add_score(b),
            Err(ScoreError::Overflow)
        ));
        assert!(matches!(
            MilliScore::ZERO.checked_sub_score(b),
            Err(ScoreError::Negative { .. })
        ));
    }

    #[test]
    fn checked_sum_works_and_detects_overflow() {
        let scores = [
            MilliScore::from_milli(1000).unwrap(),
            MilliScore::from_milli(1500).unwrap(),
        ];
        assert_eq!(MilliScore::checked_sum(scores).unwrap().as_milli(), 2500);
        let empty: [MilliScore; 0] = [];
        assert_eq!(MilliScore::checked_sum(empty).unwrap(), MilliScore::ZERO);
        assert!(matches!(
            MilliScore::checked_sum([MilliScore::MAX, MilliScore::from_milli(1).unwrap()]),
            Err(ScoreError::Overflow)
        ));
    }

    #[test]
    fn granularity_multiple() {
        let tenth = MilliScore::from_milli(100).unwrap();
        assert!(
            MilliScore::from_milli(2500)
                .unwrap()
                .is_multiple_of(tenth)
                .unwrap()
        );
        assert!(
            !MilliScore::from_milli(2550)
                .unwrap()
                .is_multiple_of(tenth)
                .unwrap()
        );
        assert!(matches!(
            MilliScore::ZERO.is_multiple_of(MilliScore::ZERO),
            Err(ScoreError::Format { .. })
        ));
    }

    #[test]
    fn serde_uses_integer_milli() {
        let score = MilliScore::parse("2.5").unwrap();
        let json = serde_json::to_string(&score).unwrap();
        assert_eq!(json, "2500");
        let back: MilliScore = serde_json::from_str(&json).unwrap();
        assert_eq!(back, score);
        assert!(serde_json::from_str::<MilliScore>("-1").is_err());
    }

    #[test]
    fn debug_is_human_readable() {
        let score = MilliScore::parse("2.5").unwrap();
        assert_eq!(format!("{score:?}"), "MilliScore(2500 = 2.5)");
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig { cases: 256, ..proptest::prelude::ProptestConfig::default() })]

        /// 性质：解析与格式化互逆（分数往返解析）。
        #[test]
        fn prop_parse_format_roundtrip(milli in 0i64..=100_000_000) {
            let score = MilliScore::from_milli(milli).unwrap();
            let text = score.to_score_string();
            proptest::prop_assert_eq!(MilliScore::parse(&text).unwrap(), score);
            // 规范最短形式：整数不带小数点，最多 3 位小数。
            proptest::prop_assert!(text.len() <= 19);
        }

        /// 性质：加法结合律（在受检加法成立时）。
        #[test]
        fn prop_add_is_associative(a in 0i64..1_000_000, b in 0i64..1_000_000, c in 0i64..1_000_000) {
            let (a, b, c) = (MilliScore::from_milli(a).unwrap(), MilliScore::from_milli(b).unwrap(), MilliScore::from_milli(c).unwrap());
            let left = a.checked_add_score(b).unwrap().checked_add_score(c).unwrap();
            let right = a.checked_add_score(b.checked_add_score(c).unwrap()).unwrap();
            proptest::prop_assert_eq!(left, right);
        }

        /// 性质：加法可交换，且零元恒等。
        #[test]
        fn prop_add_commutative_and_identity(a in 0i64..1_000_000, b in 0i64..1_000_000) {
            let (a, b) = (MilliScore::from_milli(a).unwrap(), MilliScore::from_milli(b).unwrap());
            proptest::prop_assert_eq!(a.checked_add_score(b).unwrap(), b.checked_add_score(a).unwrap());
            proptest::prop_assert_eq!(a.checked_add_score(MilliScore::ZERO).unwrap(), a);
        }

        /// 性质：受检加法永不 panic，且成功结果必然大于等于两个操作数。
        #[test]
        fn prop_add_checked_never_panics(a in 0i64..=i64::MAX, b in 0i64..=i64::MAX) {
            let (a, b) = (
                MilliScore::from_milli(a).unwrap(),
                MilliScore::from_milli(b).unwrap(),
            );
            match a.checked_add_score(b) {
                Ok(sum) => {
                    proptest::prop_assert!(sum >= a);
                    proptest::prop_assert!(sum >= b);
                }
                Err(_) => {
                    proptest::prop_assert!(a.as_milli().checked_add(b.as_milli()).is_none());
                }
            }
        }
    }
}
