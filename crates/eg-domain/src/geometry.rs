//! 归一化矩形框（`SPEC-domain.md` §6 `BBox`）。
//!
//! 不变量：坐标归一化到 0–1、左上原点；`w`、`h` 必须为正；不得越出页面
//! （`x + w ≤ 1`、`y + h ≤ 1`，允许浮点运算的极小误差）。
//!
//! 字段公开以便构造与序列化；绕过 [`BBox::new`] 直接构造的实例必须调用
//! [`BBox::validate`]，反序列化路径已强制校验。

use core::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use crate::error::{DomainError, invalid_field};

/// 归一化坐标允许的比较误差（吸收浮点加减的舍入误差）。
pub const COORD_EPSILON: f64 = 1e-9;

/// 归一化矩形框：左上原点，`w`/`h` 为正。
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct BBox {
    /// 左上角 x（0–1）。
    pub x: f64,
    /// 左上角 y（0–1）。
    pub y: f64,
    /// 宽度（0–1，必须为正）。
    pub w: f64,
    /// 高度（0–1，必须为正）。
    pub h: f64,
}

impl BBox {
    /// 校验并构造矩形框。
    ///
    /// # Errors
    ///
    /// 坐标非有限、越界、`w`/`h` 非正或越过页面边界时返回
    /// [`DomainError::InvalidField`]。
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Result<Self, DomainError> {
        let candidate = Self { x, y, w, h };
        candidate.validate()?;
        Ok(candidate)
    }

    /// 整页矩形框（`x=0, y=0, w=1, h=1`）。
    #[must_use]
    pub fn full_page() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
        }
    }

    /// 校验不变量（供直接构造实例后自查）。
    ///
    /// # Errors
    ///
    /// 与 [`BBox::new`] 相同的错误条件。
    pub fn validate(&self) -> Result<(), DomainError> {
        for (field, value) in [
            ("bbox.x", self.x),
            ("bbox.y", self.y),
            ("bbox.w", self.w),
            ("bbox.h", self.h),
        ] {
            if !value.is_finite() {
                return Err(invalid_field("bbox", format!("{field} 必须是有限数值")));
            }
        }
        if !(0.0..=1.0).contains(&self.x) || !(0.0..=1.0).contains(&self.y) {
            return Err(invalid_field("bbox", "x/y 必须在 0–1 之间（归一化坐标）"));
        }
        if self.w <= 0.0 || self.h <= 0.0 {
            return Err(invalid_field("bbox", "w/h 必须为正"));
        }
        if self.x + self.w > 1.0 + COORD_EPSILON {
            return Err(invalid_field("bbox", "x + w 不得越过页面右边界"));
        }
        if self.y + self.h > 1.0 + COORD_EPSILON {
            return Err(invalid_field("bbox", "y + h 不得越出页面下边界"));
        }
        Ok(())
    }

    /// 右边界（`x + w`）。
    #[must_use]
    pub fn right(&self) -> f64 {
        self.x + self.w
    }

    /// 下边界（`y + h`）。
    #[must_use]
    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }

    /// 是否完全包含另一矩形（含边界）。
    #[must_use]
    pub fn contains(&self, other: &Self) -> bool {
        other.x + COORD_EPSILON >= self.x
            && other.y + COORD_EPSILON >= self.y
            && other.right() <= self.right() + COORD_EPSILON
            && other.bottom() <= self.bottom() + COORD_EPSILON
    }
}

impl fmt::Display for BBox {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "({:.6},{:.6},{:.6},{:.6})",
            self.x, self.y, self.w, self.h
        )
    }
}

/// 反序列化用的线格式（历史数据宽容：忽略未知字段）。
#[derive(Deserialize)]
struct BBoxWire {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

impl<'de> Deserialize<'de> for BBox {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = BBoxWire::deserialize(deserializer)?;
        Self::new(wire.x, wire.y, wire.w, wire.h).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn accepts_valid_boxes() {
        let full = BBox::new(0.0, 0.0, 1.0, 1.0).unwrap();
        assert_eq!(full, BBox::full_page());
        let inset = BBox::new(0.1, 0.2, 0.3, 0.4).unwrap();
        assert!((inset.right() - 0.4).abs() < 1e-12);
        assert!((inset.bottom() - 0.6).abs() < 1e-12);
    }

    #[test]
    fn rejects_invalid_boxes() {
        let bad = [
            (-0.1, 0.0, 0.5, 0.5),
            (0.0, -0.1, 0.5, 0.5),
            (0.0, 0.0, 0.0, 0.5),
            (0.0, 0.0, 0.5, 0.0),
            (0.0, 0.0, -0.5, 0.5),
            (0.6, 0.0, 0.5, 0.5),
            (0.0, 0.6, 0.5, 0.5),
            (f64::NAN, 0.0, 0.5, 0.5),
            (0.0, f64::INFINITY, 0.5, 0.5),
        ];
        for (x, y, w, h) in bad {
            assert!(BBox::new(x, y, w, h).is_err(), "({x},{y},{w},{h})");
        }
    }

    #[test]
    fn contains_works() {
        let outer = BBox::new(0.0, 0.0, 0.5, 0.5).unwrap();
        let inner = BBox::new(0.1, 0.1, 0.2, 0.2).unwrap();
        let crossing = BBox::new(0.4, 0.4, 0.5, 0.5).unwrap();
        assert!(outer.contains(&inner));
        assert!(!outer.contains(&crossing));
        assert!(outer.contains(&outer));
    }

    #[test]
    fn serde_roundtrip_and_validation() {
        let bbox = BBox::new(0.1, 0.2, 0.3, 0.4).unwrap();
        let json = serde_json::to_string(&bbox).unwrap();
        assert_eq!(json, r#"{"x":0.1,"y":0.2,"w":0.3,"h":0.4}"#);
        assert_eq!(serde_json::from_str::<BBox>(&json).unwrap(), bbox);
        assert!(serde_json::from_str::<BBox>(r#"{"x":-1.0,"y":0.0,"w":0.5,"h":0.5}"#).is_err());
        assert!(serde_json::from_str::<BBox>(r#"{"x":0.0,"y":0.0,"w":0.0,"h":0.5}"#).is_err());
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig { cases: 256, ..proptest::prelude::ProptestConfig::default() })]

        /// 性质：任意合法矩形框序列化往返稳定。
        #[test]
        fn prop_bbox_serde_roundtrip(x in 0.0f64..=0.5, y in 0.0f64..=0.5, w in 0.001f64..=0.5, h in 0.001f64..=0.5) {
            proptest::prop_assume!(x + w <= 1.0 && y + h <= 1.0);
            let bbox = BBox::new(x, y, w, h).unwrap();
            let json = serde_json::to_string(&bbox).unwrap();
            proptest::prop_assert_eq!(serde_json::from_str::<BBox>(&json).unwrap(), bbox);
            proptest::prop_assert!(bbox.contains(&bbox));
        }
    }
}
