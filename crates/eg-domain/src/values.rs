//! 规范中以名称出现、但未给出表示细节的辅助值对象。
//!
//! 这些类型都是「有名字的标量」，用于替代跨模块裸 `String`（规范 §7
//! 「禁止裸字符串跨模块传递」）与页面方向参数：
//!
//! - [`SubjectId`]（§3，v1 恒为 `math`）、[`ComparatorId`]（§9）、
//!   [`BackendId`]（§8 保留字）、[`TemplateId`]（§5 模板 id）、
//!   [`CalibrationId`]（§8 校准 id）。
//! - [`Rotation`]（§6 `rotation_applied` 0/90/180/270）、
//!   [`ExifOrientation`]（§6 `exif_orientation` 0–8）。
//!
//! 说明：`SPEC-domain.md` §2 的 id 族仅列出 ULID 新类型；上述字符串 id 是
//! 规范正文中点名、但未列入 §2 的字段类型。实现按「非空字符串」校验，
//! 并在研究记录中登记为规范解释项（未改规范）。

use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{DomainError, invalid_field};

/// 声明一个「非空字符串 id」新类型。
macro_rules! string_id {
    ($(#[$meta:meta])* $name:ident, $field:literal) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            /// 构造并校验非空（去掉首尾空白后不得为空）。
            ///
            /// # Errors
            ///
            /// 空白字符串返回 [`DomainError::InvalidField`]。
            pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
                let text = value.into();
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    return Err(invalid_field($field, "不得为空"));
                }
                Ok(Self(trimmed.to_owned()))
            }

            /// 取出字符串切片。
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_tuple(stringify!($name))
                    .field(&self.0)
                    .finish()
            }
        }

        impl FromStr for $name {
            type Err = DomainError;

            fn from_str(input: &str) -> Result<Self, Self::Err> {
                Self::new(input)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let text = String::deserialize(deserializer)?;
                Self::new(text).map_err(serde::de::Error::custom)
            }
        }
    };
}

string_id! {
    /// 学科 id（`SPEC-domain.md` §3；v1 恒为 `math`，为后续学科留位）。
    SubjectId, "subject"
}

impl SubjectId {
    /// 数学学科 id（v1 唯一实现）。
    pub const MATH_NAME: &'static str = "math";

    /// 数学学科 id。
    ///
    /// # Panics
    ///
    /// 不会 panic：常量 `"math"` 非空。
    #[must_use]
    pub fn math() -> Self {
        Self("math".to_owned())
    }
}

string_id! {
    /// 比较器 id（`SPEC-domain.md` §9；由学科包声明与解释）。
    ComparatorId, "comparator"
}

string_id! {
    /// 推理后端 id（`SPEC-domain.md` §8；保留字 `local_server`/`openai_compat`/`fixture`）。
    BackendId, "backend"
}

impl BackendId {
    /// 本地 HTTP sidecar 后端（默认离线方案）。
    pub const LOCAL_SERVER: &'static str = "local_server";
    /// OpenAI 兼容云端后端（仅任务授权后可用）。
    pub const OPENAI_COMPAT: &'static str = "openai_compat";
    /// 录制回放后端（测试离线用）。
    pub const FIXTURE: &'static str = "fixture";

    /// 本地 sidecar 后端 id。
    ///
    /// # Panics
    ///
    /// 不会 panic：常量非空。
    #[must_use]
    pub fn local_server() -> Self {
        Self(Self::LOCAL_SERVER.to_owned())
    }

    /// OpenAI 兼容云端后端 id。
    ///
    /// # Panics
    ///
    /// 不会 panic：常量非空。
    #[must_use]
    pub fn openai_compat() -> Self {
        Self(Self::OPENAI_COMPAT.to_owned())
    }

    /// fixture 回放后端 id。
    ///
    /// # Panics
    ///
    /// 不会 panic：常量非空。
    #[must_use]
    pub fn fixture() -> Self {
        Self(Self::FIXTURE.to_owned())
    }

    /// 是否为云端后端（决定 `ModelCall.consent_id` 必填，规范 §8）。
    #[must_use]
    pub fn is_cloud(&self) -> bool {
        self.as_str() == Self::OPENAI_COMPAT
    }
}

string_id! {
    /// 标准规则模板 id（`SPEC-domain.md` §5；模板库归 `rubric` 模块）。
    TemplateId, "template_id"
}

string_id! {
    /// 模型校准记录 id（`SPEC-domain.md` §8 `calibration_id`；为空禁止自动放行）。
    CalibrationId, "calibration_id"
}

/// 页面已应用的方向校正角度（`SPEC-domain.md` §6：0/90/180/270）。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Rotation(u16);

impl Rotation {
    /// 不旋转。
    pub const NONE: Self = Self(0);
    /// 顺时针 90°。
    pub const CW_90: Self = Self(90);
    /// 180°。
    pub const CW_180: Self = Self(180);
    /// 顺时针 270°。
    pub const CW_270: Self = Self(270);

    /// 构造并校验角度只能是 0/90/180/270。
    ///
    /// # Errors
    ///
    /// 非法角度返回 [`DomainError::InvalidField`]。
    pub fn new(degrees: u16) -> Result<Self, DomainError> {
        match degrees {
            0 | 90 | 180 | 270 => Ok(Self(degrees)),
            _ => Err(invalid_field(
                "rotation_applied",
                format!("只允许 0/90/180/270，实际 {degrees}"),
            )),
        }
    }

    /// 角度值（度）。
    #[must_use]
    pub const fn degrees(self) -> u16 {
        self.0
    }
}

impl fmt::Display for Rotation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Debug for Rotation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Rotation({})", self.0)
    }
}

impl Serialize for Rotation {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Rotation {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let degrees = u16::deserialize(deserializer)?;
        Self::new(degrees).map_err(serde::de::Error::custom)
    }
}

/// EXIF 方向标记（`SPEC-domain.md` §6：1–8，0 表示无 EXIF；镜像信息不可丢）。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct ExifOrientation(u8);

impl ExifOrientation {
    /// 无 EXIF 方向信息。
    pub const NONE: Self = Self(0);

    /// 构造并校验取值 0–8。
    ///
    /// # Errors
    ///
    /// 超出 0–8 返回 [`DomainError::InvalidField`]。
    pub fn new(value: u8) -> Result<Self, DomainError> {
        if value <= 8 {
            Ok(Self(value))
        } else {
            Err(invalid_field(
                "exif_orientation",
                format!("必须在 0–8 之间，实际 {value}"),
            ))
        }
    }

    /// 原始 EXIF 方向值。
    #[must_use]
    pub const fn value(self) -> u8 {
        self.0
    }

    /// 是否表示存在镜像（EXIF 2/4/5/7 含水平或垂直翻转）。
    #[must_use]
    pub const fn is_mirrored(self) -> bool {
        matches!(self.0, 2 | 4 | 5 | 7)
    }
}

impl fmt::Display for ExifOrientation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Debug for ExifOrientation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ExifOrientation({})", self.0)
    }
}

impl Serialize for ExifOrientation {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ExifOrientation {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = u8::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn string_ids_validate_non_empty() {
        assert!(SubjectId::new("math").is_ok());
        assert_eq!(SubjectId::new(" math ").unwrap().as_str(), "math");
        assert!(matches!(
            SubjectId::new("  "),
            Err(DomainError::InvalidField { .. })
        ));
        assert!(ComparatorId::new("num_abs").is_ok());
        assert!(TemplateId::new("national-1").is_ok());
        assert!(CalibrationId::new("cal-1").is_ok());
        assert_eq!(SubjectId::math().as_str(), SubjectId::MATH_NAME);
    }

    #[test]
    fn backend_id_knows_reserved_names() {
        assert!(BackendId::openai_compat().is_cloud());
        assert!(!BackendId::local_server().is_cloud());
        assert!(!BackendId::fixture().is_cloud());
        assert!(BackendId::new("custom_backend").is_ok());
        assert_eq!(BackendId::local_server().as_str(), "local_server");
    }

    #[test]
    fn string_id_serde_roundtrip() {
        let id = SubjectId::math();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"math\"");
        assert_eq!(serde_json::from_str::<SubjectId>(&json).unwrap(), id);
        assert!(serde_json::from_str::<SubjectId>("\"\"").is_err());
        assert_eq!(format!("{id:?}"), "SubjectId(\"math\")");
    }

    #[test]
    fn rotation_only_accepts_right_angles() {
        for degrees in [0u16, 90, 180, 270] {
            assert_eq!(Rotation::new(degrees).unwrap().degrees(), degrees);
        }
        for bad in [1u16, 45, 89, 271, 360, u16::MAX] {
            assert!(matches!(
                Rotation::new(bad),
                Err(DomainError::InvalidField {
                    field: "rotation_applied",
                    ..
                })
            ));
        }
        assert_eq!(Rotation::default(), Rotation::NONE);
    }

    #[test]
    fn exif_orientation_range_and_mirroring() {
        for value in 0..=8u8 {
            assert_eq!(ExifOrientation::new(value).unwrap().value(), value);
        }
        assert!(matches!(
            ExifOrientation::new(9),
            Err(DomainError::InvalidField {
                field: "exif_orientation",
                ..
            })
        ));
        assert!(ExifOrientation::new(2).unwrap().is_mirrored());
        assert!(ExifOrientation::new(4).unwrap().is_mirrored());
        assert!(ExifOrientation::new(5).unwrap().is_mirrored());
        assert!(ExifOrientation::new(7).unwrap().is_mirrored());
        assert!(!ExifOrientation::new(6).unwrap().is_mirrored());
        assert!(!ExifOrientation::new(0).unwrap().is_mirrored());
    }

    #[test]
    fn rotation_and_exif_serde_validate() {
        assert_eq!(
            serde_json::from_str::<Rotation>("90").unwrap(),
            Rotation::CW_90
        );
        assert!(serde_json::from_str::<Rotation>("45").is_err());
        assert_eq!(
            serde_json::from_str::<ExifOrientation>("6")
                .unwrap()
                .value(),
            6
        );
        assert!(serde_json::from_str::<ExifOrientation>("9").is_err());
    }
}
