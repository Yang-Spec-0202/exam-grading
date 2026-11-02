//! 时间类型与可注入时钟（`SPEC-domain.md` §2、§7）。
//!
//! 规范要求：时间存 RFC3339 文本；业务代码不得直接读系统时间，测试注入固定时钟。
//! `domain` 只定义 [`Timestamp`]（UTC 时间点）与 [`Clock`] 端口，
//! `SystemClock`/`FixedClock` 等实现由 `platform` 提供（`SPEC-platform.md` T12）。
//!
//! [`Timestamp`] 是 `chrono::DateTime<Utc>` 的类型别名：序列化即 RFC3339 文本
//! （`chrono` serde 默认 `SecondsFormat::AutoSi`、UTC 用 `Z`），满足规范 §10。

use chrono::{DateTime, SecondsFormat, Utc};

/// UTC 时间点（RFC3339 序列化；存储为 TEXT）。
pub type Timestamp = DateTime<Utc>;

/// 可注入时钟端口：业务代码经此取时间，禁止直接读系统时间。
pub trait Clock: Send + Sync {
    /// 当前 UTC 时间。
    fn now(&self) -> Timestamp;
}

/// 将时间点格式化为 RFC3339 文本（UTC、`Z` 结尾、按需保留小数秒）。
#[must_use]
pub fn format_rfc3339(timestamp: Timestamp) -> String {
    timestamp.to_rfc3339_opts(SecondsFormat::AutoSi, true)
}

/// 解析 RFC3339 文本为 UTC 时间点。
///
/// # Errors
///
/// 文本不是合法 RFC3339 时返回 [`chrono::ParseError`]。
pub fn parse_rfc3339(text: &str) -> Result<Timestamp, chrono::ParseError> {
    DateTime::parse_from_rfc3339(text).map(|fixed| fixed.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use core::sync::atomic::{AtomicI64, Ordering};

    #[test]
    fn rfc3339_roundtrip() {
        let timestamp = parse_rfc3339("2026-11-02T12:34:56.789Z").unwrap();
        let text = format_rfc3339(timestamp);
        assert_eq!(text, "2026-11-02T12:34:56.789Z");
        assert_eq!(parse_rfc3339(&text).unwrap(), timestamp);
    }

    #[test]
    fn serde_is_rfc3339() {
        let timestamp = parse_rfc3339("2026-11-02T12:34:56Z").unwrap();
        let json = serde_json::to_string(&timestamp).unwrap();
        assert_eq!(json, "\"2026-11-02T12:34:56Z\"");
        let back: Timestamp = serde_json::from_str(&json).unwrap();
        assert_eq!(back, timestamp);
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse_rfc3339("2026-11-02 12:34:56").is_err());
        assert!(parse_rfc3339("not-a-time").is_err());
    }

    #[test]
    fn clock_is_injectable() {
        struct FixedClock(AtomicI64);
        impl Clock for FixedClock {
            fn now(&self) -> Timestamp {
                let millis = self.0.load(Ordering::SeqCst);
                DateTime::from_timestamp_millis(millis).unwrap()
            }
        }
        let clock = FixedClock(AtomicI64::new(1_700_000_000_000));
        let first = clock.now();
        assert_eq!(first.timestamp_millis(), 1_700_000_000_000);
        clock.0.store(1_700_000_001_000, Ordering::SeqCst);
        assert!(clock.now() > first);
    }
}
