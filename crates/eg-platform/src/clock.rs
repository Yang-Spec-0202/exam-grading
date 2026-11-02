//! 时钟实现（`SPEC-platform.md` §4.5；`SPEC-domain.md` §2 的 `Clock` 端口）。
//!
//! 生产用 [`SystemClock`]；测试注入 [`FixedClock`]（可前进、可回拨以验证 ULID 与保留期）。
//! 业务代码不得直接读系统时间。

use std::sync::{Arc, RwLock};

use chrono::Duration;
use eg_domain::{Clock, Timestamp};

use crate::error::{PlatformError, Result};

/// 生产时钟：读系统 UTC 时间。
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        chrono::Utc::now()
    }
}

/// 可注入的固定/可调时钟（线程安全；克隆共享同一时间点）。
#[derive(Debug, Clone)]
pub struct FixedClock {
    now: Arc<RwLock<Timestamp>>,
}

impl FixedClock {
    /// 以给定时间点创建。
    #[must_use]
    pub fn new(now: Timestamp) -> Self {
        Self {
            now: Arc::new(RwLock::new(now)),
        }
    }

    /// 以 RFC3339 文本创建（测试夹具常用）。
    ///
    /// # Errors
    ///
    /// 文本不是合法 RFC3339 时返回 [`PlatformError::InvalidArgument`]。
    pub fn from_rfc3339(text: &str) -> Result<Self> {
        let timestamp = eg_domain::time::parse_rfc3339(text).map_err(|error| {
            PlatformError::InvalidArgument(format!("时钟初始值不是合法 RFC3339：{text}（{error}）"))
        })?;
        Ok(Self::new(timestamp))
    }

    /// 设置到指定时间点（测试中用于推进/回拨）。
    ///
    /// # Errors
    ///
    /// 锁中毒时返回 [`PlatformError::Internal`]。
    pub fn set(&self, now: Timestamp) -> Result<()> {
        let mut guard = self
            .now
            .write()
            .map_err(|_| PlatformError::Internal("时钟锁中毒".to_owned()))?;
        *guard = now;
        Ok(())
    }

    /// 前进指定时长。
    ///
    /// # Errors
    ///
    /// 锁中毒或时间溢出时返回错误。
    pub fn advance(&self, delta: Duration) -> Result<Timestamp> {
        let mut guard = self
            .now
            .write()
            .map_err(|_| PlatformError::Internal("时钟锁中毒".to_owned()))?;
        *guard = guard
            .checked_add_signed(delta)
            .ok_or_else(|| PlatformError::InvalidArgument("时钟前进溢出".to_owned()))?;
        Ok(*guard)
    }

    /// 回拨指定时长（用于验证时钟回拨保护）。
    ///
    /// # Errors
    ///
    /// 锁中毒或时间溢出时返回错误。
    pub fn rewind(&self, delta: Duration) -> Result<Timestamp> {
        self.advance(-delta)
    }
}

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        // 读锁中毒说明其他线程 panic；此处退化为 UNIX 纪元而不是 panic（库不 panic）。
        self.now
            .read()
            .map_or_else(|poisoned| *poisoned.into_inner(), |guard| *guard)
    }
}

/// 统一时钟句柄类型（`platform` 内部与上层注入使用）。
pub type SharedClock = Arc<dyn Clock>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_clock_advances_and_rewinds() {
        let clock = FixedClock::from_rfc3339("2026-11-02T12:00:00Z").unwrap();
        assert_eq!(
            eg_domain::time::format_rfc3339(clock.now()),
            "2026-11-02T12:00:00Z"
        );
        clock.advance(Duration::days(31)).unwrap();
        assert_eq!(
            eg_domain::time::format_rfc3339(clock.now()),
            "2026-12-03T12:00:00Z"
        );
        clock.rewind(Duration::seconds(60)).unwrap();
        assert_eq!(
            eg_domain::time::format_rfc3339(clock.now()),
            "2026-12-03T11:59:00Z"
        );
    }

    #[test]
    fn fixed_clock_is_shared_between_clones() {
        let clock = FixedClock::from_rfc3339("2026-11-02T00:00:00Z").unwrap();
        let clone = clock.clone();
        clock.advance(Duration::hours(1)).unwrap();
        assert_eq!(clock.now(), clone.now());
    }

    #[test]
    fn system_clock_moves_forward() {
        let clock = SystemClock;
        let first = clock.now();
        let second = clock.now();
        assert!(second >= first);
    }

    #[test]
    fn invalid_rfc3339_rejected() {
        assert!(FixedClock::from_rfc3339("不是时间").is_err());
    }
}
