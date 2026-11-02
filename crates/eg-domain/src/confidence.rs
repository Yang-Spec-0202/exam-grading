//! 置信度值对象（`SPEC-domain.md` §2）。
//!
//! 表示 0.0–1.0 的浮点包装；构造时校验非 NaN、在区间内。
//!
//! 规范区分「模型原始概率（校准前）」与「校准后置信度」：本类型两者通用，
//! 区分由字段语义承载——`JudgeOutcome.confidence` 为原始值，
//! `GradeRecord.confidence` 为校准后值（见 `SPEC-domain.md` §8）。

use core::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::ConfidenceError;

/// 0.0–1.0 的置信度。
#[derive(Clone, Copy, PartialEq, PartialOrd, Default)]
pub struct Confidence(f64);

impl Confidence {
    /// 零置信度。
    pub const ZERO: Self = Self(0.0);

    /// 满置信度（仅人工确认等场景使用）。
    pub const ONE: Self = Self(1.0);

    /// 构造置信度。
    ///
    /// # Errors
    ///
    /// NaN 返回 [`ConfidenceError::NaN`]；不在 `0.0..=1.0` 返回
    /// [`ConfidenceError::OutOfRange`]。
    pub fn new(value: f64) -> Result<Self, ConfidenceError> {
        if value.is_nan() {
            return Err(ConfidenceError::NaN);
        }
        if !(0.0..=1.0).contains(&value) {
            return Err(ConfidenceError::OutOfRange { value });
        }
        Ok(Self(value))
    }

    /// 取出浮点值（仅用于展示与存储为 REAL，不用于分数计算）。
    #[must_use]
    pub const fn get(self) -> f64 {
        self.0
    }

    /// 是否达到或超过阈值。
    #[must_use]
    pub fn is_at_least(self, threshold: Self) -> bool {
        self.0 >= threshold.0
    }

    /// 两个置信度的较小者（`StudentAnswer` 多段取最小值等场景）。
    #[must_use]
    pub fn min(self, other: Self) -> Self {
        if self.0 <= other.0 { self } else { other }
    }
}

impl fmt::Display for Confidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl fmt::Debug for Confidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Confidence({})", self.0)
    }
}

impl TryFrom<f64> for Confidence {
    type Error = ConfidenceError;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl Serialize for Confidence {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // 规范 §10：置信度存 REAL。
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Confidence {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = f64::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    #[allow(clippy::float_cmp)]
    fn accepts_boundaries() {
        assert_eq!(Confidence::new(0.0).unwrap().get(), 0.0);
        assert_eq!(Confidence::new(1.0).unwrap().get(), 1.0);
        assert_eq!(Confidence::new(0.5).unwrap().get(), 0.5);
    }

    #[test]
    fn rejects_out_of_range_and_nan() {
        assert_eq!(
            Confidence::new(1.000_001).unwrap_err(),
            ConfidenceError::OutOfRange { value: 1.000_001 }
        );
        assert_eq!(
            Confidence::new(-0.1).unwrap_err(),
            ConfidenceError::OutOfRange { value: -0.1 }
        );
        assert_eq!(Confidence::new(f64::NAN).unwrap_err(), ConfidenceError::NaN);
        assert!(matches!(
            Confidence::new(f64::INFINITY),
            Err(ConfidenceError::OutOfRange { .. })
        ));
    }

    #[test]
    fn threshold_and_min_helpers() {
        let low = Confidence::new(0.4).unwrap();
        let high = Confidence::new(0.9).unwrap();
        assert!(!low.is_at_least(high));
        assert!(high.is_at_least(low));
        assert_eq!(low.min(high), low);
        assert_eq!(high.min(low), low);
    }

    #[test]
    fn serde_roundtrip_and_validation() {
        let confidence = Confidence::new(0.85).unwrap();
        let json = serde_json::to_string(&confidence).unwrap();
        assert_eq!(json, "0.85");
        let back: Confidence = serde_json::from_str(&json).unwrap();
        assert_eq!(back, confidence);
        assert!(serde_json::from_str::<Confidence>("1.5").is_err());
        assert!(serde_json::from_str::<Confidence>("null").is_err());
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig { cases: 256, ..proptest::prelude::ProptestConfig::default() })]

        /// 性质：区间内任意浮点可往返序列化。
        #[test]
        fn prop_valid_values_roundtrip(value in 0.0f64..=1.0f64) {
            let confidence = Confidence::new(value).unwrap();
            let json = serde_json::to_string(&confidence).unwrap();
            let back: Confidence = serde_json::from_str(&json).unwrap();
            proptest::prop_assert_eq!(back, confidence);
        }

        /// 性质：最小值与顺序无关且不超过任一操作数。
        #[test]
        fn prop_min_is_commutative(a in 0.0f64..=1.0f64, b in 0.0f64..=1.0f64) {
            let (a, b) = (Confidence::new(a).unwrap(), Confidence::new(b).unwrap());
            proptest::prop_assert!((a.min(b).get() - b.min(a).get()).abs() < f64::EPSILON);
            proptest::prop_assert!(a.min(b).get() <= a.get());
            proptest::prop_assert!(a.min(b).get() <= b.get());
        }
    }
}
