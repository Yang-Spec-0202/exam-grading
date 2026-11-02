//! 协作式取消（`SPEC-platform.md` §4.3）。
//!
//! `CancelToken` 可在阶段边界与模型调用前后检查；取消不 panic、不中断线程，
//! 由调用方在检查点返回 [`PlatformError::Cancelled`]。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{PlatformError, Result};

/// 取消令牌（可克隆；克隆共享同一状态）。
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    inner: Arc<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    cancelled: AtomicBool,
    reason: std::sync::Mutex<Option<String>>,
}

impl CancelToken {
    /// 创建未取消的令牌。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 请求取消（幂等；首次原因保留）。
    pub fn cancel(&self) {
        self.cancel_with_reason("用户取消");
    }

    /// 请求取消并记录原因。
    pub fn cancel_with_reason(&self, reason: impl Into<String>) {
        if !self.inner.cancelled.swap(true, Ordering::SeqCst)
            && let Ok(mut guard) = self.inner.reason.lock()
        {
            *guard = Some(reason.into());
        }
    }

    /// 是否已取消。
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    /// 取消原因（未取消为 `None`）。
    #[must_use]
    pub fn reason(&self) -> Option<String> {
        self.inner
            .reason
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
    }

    /// 在检查点校验：已取消则返回 [`PlatformError::Cancelled`]。
    ///
    /// # Errors
    ///
    /// 已取消时返回 [`PlatformError::Cancelled`]。
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(PlatformError::Cancelled)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_starts_active_and_cancels() {
        let token = CancelToken::new();
        assert!(!token.is_cancelled());
        token.check().unwrap();
        token.cancel();
        assert!(token.is_cancelled());
        assert!(matches!(token.check(), Err(PlatformError::Cancelled)));
        assert_eq!(token.reason().as_deref(), Some("用户取消"));
    }

    #[test]
    fn clones_share_state_and_first_reason_wins() {
        let token = CancelToken::new();
        let clone = token.clone();
        token.cancel_with_reason("阶段边界取消");
        assert!(clone.is_cancelled());
        assert_eq!(clone.reason().as_deref(), Some("阶段边界取消"));
        clone.cancel_with_reason("第二次");
        assert_eq!(token.reason().as_deref(), Some("阶段边界取消"));
    }
}
