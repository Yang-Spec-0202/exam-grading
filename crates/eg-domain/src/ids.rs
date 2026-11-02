//! ULID 与业务 id 新类型（`SPEC-domain.md` §2）。
//!
//! - [`Ulid`]：26 字符 Crockford Base32 文本包装，可解析、可格式化、按时间可排序。
//! - id 族：`StudentId`…`ConsentId` 等，各自包装 [`Ulid`]，禁止裸字符串跨模块传递。
//!
//! 生成策略：本模块**不提供**依赖系统时间的 `new()`。ULID 的单调生成由
//! `platform` 的 id 生成器（单例、单调不倒退）负责，本模块只提供确定性构造
//! （`from_parts`/`from_u128`）、解析与比较，保证 `domain` 不读系统时间。

use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use ulid::Ulid as UlidImpl;

use crate::error::IdError;

/// 26 字符 Crockford Base32 的 ULID 文本包装。
///
/// 不变量：解析后固定为 128 位值；`Display` 输出规范大写 26 字符文本
/// （规范 §2「Ulid：26 字符 Crockford Base32 文本」）。
/// 排序即字典序即时间序（ULID 编码保证）。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ulid(UlidImpl);

impl Ulid {
    /// 全零 ULID（测试与占位用；业务 id 不应使用）。
    pub const NIL: Self = Self(UlidImpl::nil());

    /// 由 128 位整数构造（确定性，不读时钟）。
    #[must_use]
    pub const fn from_u128(value: u128) -> Self {
        Self(UlidImpl(value))
    }

    /// 由时间戳（毫秒）与随机部分构造（确定性，不读时钟）。
    ///
    /// 生产环境的单调生成由 `platform` 负责；本构造器供回放、测试与迁移使用。
    #[must_use]
    pub const fn from_parts(timestamp_ms: u64, random: u128) -> Self {
        Self(UlidImpl::from_parts(timestamp_ms, random))
    }

    /// 128 位原始值。
    #[must_use]
    pub const fn as_u128(self) -> u128 {
        self.0.0
    }

    /// ULID 内嵌的时间戳（Unix 毫秒）。
    #[must_use]
    pub const fn timestamp_ms(self) -> u64 {
        self.0.timestamp_ms()
    }

    /// ULID 的 80 位随机部分。
    #[must_use]
    pub const fn random(self) -> u128 {
        self.0.random()
    }

    /// 是否为全零 ULID。
    #[must_use]
    pub const fn is_nil(self) -> bool {
        self.0.is_nil()
    }

    /// 严格解析 26 字符 ULID 文本（大小写不敏感，输出规范大写）。
    ///
    /// # Errors
    ///
    /// 长度不是 26 或含非 Crockford Base32 字符时返回 [`IdError::InvalidUlid`]。
    pub fn parse(input: &str) -> Result<Self, IdError> {
        let parsed = UlidImpl::from_string(input).map_err(|err| IdError::InvalidUlid {
            input: input.to_owned(),
            reason: decode_reason(err).to_owned(),
        })?;
        Ok(Self(parsed))
    }

    /// 单调递增一个步长（供 `platform` 生成器实现「单调不倒退」）。
    ///
    /// 已到 128 位上限时返回 `None`（调用方必须处理，不得回绕）。
    #[must_use]
    pub fn checked_increment(self) -> Option<Self> {
        self.0.increment().ok().map(Self)
    }
}

fn decode_reason(err: ulid::DecodeError) -> &'static str {
    match err {
        ulid::DecodeError::InvalidLength => "长度必须为 26 字符",
        ulid::DecodeError::InvalidChar => "包含非 Crockford Base32 字符",
    }
}

impl fmt::Display for Ulid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl fmt::Debug for Ulid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Ulid").field(&self.to_string()).finish()
    }
}

impl FromStr for Ulid {
    type Err = IdError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Self::parse(input)
    }
}

impl Serialize for Ulid {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Ulid {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        UlidImpl::deserialize(deserializer).map(Self)
    }
}

/// 声明一个包装 [`Ulid`] 的 id 新类型。
macro_rules! ulid_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(Ulid);

        impl $name {
            /// 由 ULID 构造（确定性）。
            #[must_use]
            pub const fn from_ulid(value: Ulid) -> Self {
                Self(value)
            }

            /// 取出内部 ULID。
            #[must_use]
            pub const fn as_ulid(self) -> Ulid {
                self.0
            }

            /// 严格解析 26 字符 ULID 文本。
            ///
            /// # Errors
            ///
            /// 文本非法时返回 [`IdError::InvalidUlid`]。
            pub fn parse(input: &str) -> Result<Self, IdError> {
                Ulid::parse(input).map(Self)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_tuple(stringify!($name))
                    .field(&self.0.to_string())
                    .finish()
            }
        }

        impl FromStr for $name {
            type Err = IdError;

            fn from_str(input: &str) -> Result<Self, Self::Err> {
                Self::parse(input)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                self.0.serialize(serializer)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                Ulid::deserialize(deserializer).map(Self)
            }
        }
    };
}

ulid_id! {
    /// 学生 id。
    StudentId
}
ulid_id! {
    /// 班级 id。
    ClassroomId
}
ulid_id! {
    /// 试卷 id。
    PaperId
}
ulid_id! {
    /// 题目 id。
    QuestionId
}
ulid_id! {
    /// 提交 id。
    SubmissionId
}
ulid_id! {
    /// 页面 id。
    PageId
}
ulid_id! {
    /// 答案区域 id。
    SegmentId
}
ulid_id! {
    /// 识别结果 id。
    RecognitionId
}
ulid_id! {
    /// 判分记录 id。
    GradeId
}
ulid_id! {
    /// 模型调用记录 id。
    ModelCallId
}
ulid_id! {
    /// 评分标准 id（生效规则与规则草稿共用；草稿转生效时保持一致）。
    RubricId
}
ulid_id! {
    /// 评分项 id。
    RubricItemId
}
ulid_id! {
    /// 复核动作 id。
    ReviewActionId
}
ulid_id! {
    /// 持久任务 id（按任务鉴权与审计用）。
    TaskId
}
ulid_id! {
    /// 云端调用授权（同意）id；随任务生命周期失效。
    ConsentId
}
ulid_id! {
    /// 比较轨迹 id（`ComparatorTrace` 的证据引用）。
    ComparatorTraceId
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn ulid_roundtrip_is_canonical() {
        let ulid = Ulid::from_u128(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef);
        let text = ulid.to_string();
        assert_eq!(text.len(), 26);
        assert_eq!(Ulid::parse(&text).unwrap(), ulid);
    }

    #[test]
    fn ulid_parse_accepts_lowercase_but_formats_uppercase() {
        let upper = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
        let lower = upper.to_lowercase();
        let parsed = Ulid::parse(&lower).unwrap();
        assert_eq!(parsed.to_string(), upper);
    }

    #[test]
    fn ulid_parse_rejects_bad_input() {
        assert!(matches!(
            Ulid::parse("short"),
            Err(IdError::InvalidUlid { .. })
        ));
        assert!(matches!(
            Ulid::parse("01ARZ3NDEKTSV4RRFFQ69G5FA!"),
            Err(IdError::InvalidUlid { .. })
        ));
        assert!(matches!(Ulid::parse(""), Err(IdError::InvalidUlid { .. })));
    }

    #[test]
    fn ulid_is_time_sortable() {
        let early = Ulid::from_parts(1_000, 0);
        let late = Ulid::from_parts(2_000, 0);
        assert!(early < late);
        assert_eq!(early.timestamp_ms(), 1_000);
        assert_eq!(late.timestamp_ms(), 2_000);
    }

    #[test]
    fn ulid_increment_is_monotonic_and_checked() {
        let ulid = Ulid::from_u128(1);
        let next = ulid.checked_increment().unwrap();
        assert_eq!(next.as_u128(), 2);
        assert!(Ulid::from_u128(u128::MAX).checked_increment().is_none());
    }

    #[test]
    fn nil_is_recognised() {
        assert!(Ulid::NIL.is_nil());
        assert!(Ulid::from_u128(0).is_nil());
        assert!(!Ulid::from_u128(1).is_nil());
    }

    #[test]
    fn id_newtypes_roundtrip_and_debug_shows_text() {
        let raw = Ulid::from_parts(1_700_000_000_000, 42);
        let id = StudentId::from_ulid(raw);
        assert_eq!(id.as_ulid(), raw);
        assert_eq!(StudentId::parse(&raw.to_string()).unwrap(), id);
        assert_eq!(id.to_string(), raw.to_string());
        assert!(format!("{id:?}").starts_with("StudentId("));
        // 不同 id 类型之间不互换（类型系统保证，这里只验证取值域一致）。
        let q = QuestionId::from_ulid(raw);
        assert_eq!(q.as_ulid().as_u128(), raw.as_u128());
    }

    #[test]
    fn ulid_serde_is_string() {
        let ulid = Ulid::from_u128(7);
        let json = serde_json::to_string(&ulid).unwrap();
        assert_eq!(json, format!("\"{ulid}\""));
        let back: Ulid = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ulid);
        assert!(serde_json::from_str::<Ulid>("\"oops\"").is_err());
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig { cases: 256, ..proptest::prelude::ProptestConfig::default() })]

        /// 性质：任意 128 位值 → 26 字符文本 → 解析回同值（往返稳定）。
        #[test]
        fn prop_ulid_text_roundtrip(value in proptest::prelude::any::<u128>()) {
            let ulid = Ulid::from_u128(value);
            let text = ulid.to_string();
            let parsed = Ulid::parse(&text).unwrap();
            proptest::prop_assert_eq!(parsed, ulid);
            proptest::prop_assert_eq!(text.len(), 26);
        }

        /// 性质：ULID 字典序与 128 位值序一致（时间可排序的充分条件）。
        #[test]
        fn prop_ulid_order_matches_value_order(a in proptest::prelude::any::<u128>(), b in proptest::prelude::any::<u128>()) {
            let ua = Ulid::from_u128(a);
            let ub = Ulid::from_u128(b);
            proptest::prop_assert_eq!(ua.cmp(&ub), a.cmp(&b));
        }
    }
}
