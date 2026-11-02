//! 熔断器（`SPEC-inference.md` §4/§9.4）：连续 3 次瞬态失败 → 打开 60 秒 → 放 1 次探测。
//!
//! 不变量：
//! - 只有**瞬态**失败计数（永久错误、Schema 不合法、策略拦截一律不计）；
//! - 打开期间 `allow()` 快速失败且**不产生任何实际调用**；
//! - 打开到期后**只放行 1 次**探测，成功关闭、失败重新打开；
//! - 时间来自注入的 [`eg_domain::Clock`]，测试可精确推进，不睡眠。

use std::sync::{Arc, Mutex};

use eg_domain::Clock;

use crate::error::InferenceError;

/// 熔断配置（修改默认值属 `SPEC-inference.md` §8 Ask first）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BreakerConfig {
    /// 连续瞬态失败达到该次数即打开。
    pub failure_threshold: u32,
    /// 打开持续时间（毫秒）。
    pub open_duration_ms: u64,
}

impl Default for BreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 3,
            open_duration_ms: 60_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BreakerState {
    Closed { consecutive: u32 },
    Open { until_ms: i64 },
    HalfOpen { probe_in_flight: bool },
}

/// 熔断器（`Send + Sync`，可跨线程共享）。
pub struct CircuitBreaker {
    clock: Arc<dyn Clock>,
    config: BreakerConfig,
    state: Mutex<BreakerState>,
}

impl core::fmt::Debug for CircuitBreaker {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("CircuitBreaker")
            .field("config", &self.config)
            .field("state", &self.state_label())
            .finish_non_exhaustive()
    }
}

impl CircuitBreaker {
    /// 用默认配置构造。
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self::with_config(clock, BreakerConfig::default())
    }

    /// 用指定配置构造。
    #[must_use]
    pub fn with_config(clock: Arc<dyn Clock>, config: BreakerConfig) -> Self {
        Self {
            clock,
            config,
            state: Mutex::new(BreakerState::Closed { consecutive: 0 }),
        }
    }

    /// 请求一次调用许可。
    ///
    /// # Errors
    ///
    /// 熔断打开（或半开探测已在飞行）时返回 [`InferenceError::CircuitOpen`]，
    /// 调用方必须**不发出任何请求**。
    pub fn allow(&self) -> Result<(), InferenceError> {
        let now_ms = self.now_ms();
        let mut state = self.lock_state();
        match *state {
            BreakerState::Closed { .. } => Ok(()),
            BreakerState::Open { until_ms } => {
                if now_ms >= until_ms {
                    *state = BreakerState::HalfOpen {
                        probe_in_flight: true,
                    };
                    Ok(())
                } else {
                    Err(InferenceError::CircuitOpen {
                        retry_after_ms: u64::try_from(until_ms - now_ms).unwrap_or(0),
                    })
                }
            }
            BreakerState::HalfOpen { probe_in_flight } => {
                if probe_in_flight {
                    Err(InferenceError::CircuitOpen { retry_after_ms: 0 })
                } else {
                    *state = BreakerState::HalfOpen {
                        probe_in_flight: true,
                    };
                    Ok(())
                }
            }
        }
    }

    /// 记录一次成功：关闭熔断并清零计数。
    pub fn record_success(&self) {
        *self.lock_state() = BreakerState::Closed { consecutive: 0 };
    }

    /// 记录一次失败；只有瞬态失败计数，永久错误直接忽略。
    pub fn record_failure(&self, error: &InferenceError) {
        if !error.counts_toward_breaker() {
            return;
        }
        let now_ms = self.now_ms();
        let mut state = self.lock_state();
        match *state {
            BreakerState::Closed { consecutive } => {
                let next = consecutive.saturating_add(1);
                if next >= self.config.failure_threshold {
                    *state = BreakerState::Open {
                        until_ms: now_ms + i64::try_from(self.config.open_duration_ms).unwrap_or(0),
                    };
                } else {
                    *state = BreakerState::Closed { consecutive: next };
                }
            }
            BreakerState::HalfOpen { .. } => {
                *state = BreakerState::Open {
                    until_ms: now_ms + i64::try_from(self.config.open_duration_ms).unwrap_or(0),
                };
            }
            BreakerState::Open { .. } => {}
        }
    }

    /// 状态标签（日志/指标用）。
    #[must_use]
    pub fn state_label(&self) -> &'static str {
        match *self.lock_state() {
            BreakerState::Closed { .. } => "closed",
            BreakerState::Open { .. } => "open",
            BreakerState::HalfOpen { .. } => "half_open",
        }
    }

    /// 连续瞬态失败次数（打开状态返回阈值）。
    #[must_use]
    pub fn consecutive_failures(&self) -> u32 {
        match *self.lock_state() {
            BreakerState::Closed { consecutive } => consecutive,
            BreakerState::Open { .. } => self.config.failure_threshold,
            BreakerState::HalfOpen { .. } => self.config.failure_threshold.saturating_sub(1),
        }
    }

    fn now_ms(&self) -> i64 {
        self.clock.now().timestamp_millis()
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, BreakerState> {
        match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::sync::Arc;

    use crate::testing::TestClock;

    use super::*;

    fn transient() -> InferenceError {
        InferenceError::Transient {
            status: 503,
            detail: "sidecar 重启中".to_owned(),
        }
    }

    fn permanent() -> InferenceError {
        InferenceError::Permanent {
            status: 400,
            detail: "参数错误".to_owned(),
        }
    }

    #[test]
    fn three_transient_failures_open_the_circuit_for_60_seconds() {
        let clock = Arc::new(TestClock::at_ms(1_000_000));
        let breaker = CircuitBreaker::new(clock.clone());

        for expected in 1..=2 {
            breaker.record_failure(&transient());
            assert_eq!(breaker.state_label(), "closed");
            assert_eq!(breaker.consecutive_failures(), expected);
            assert!(breaker.allow().is_ok());
        }
        breaker.record_failure(&transient());
        assert_eq!(breaker.state_label(), "open");

        // 打开期间：0 次实际调用 + 给出剩余等待
        let error = breaker.allow().unwrap_err();
        assert!(matches!(error, InferenceError::CircuitOpen { .. }));
        if let InferenceError::CircuitOpen { retry_after_ms } = error {
            assert!((59_000..=60_000).contains(&retry_after_ms));
        }

        // 59 秒后仍打开
        clock.advance_ms(59_000);
        assert!(breaker.allow().is_err());

        // 60 秒后放行 1 次探测
        clock.advance_ms(1_000);
        assert!(breaker.allow().is_ok(), "到期后必须放行一次探测");
        assert_eq!(breaker.state_label(), "half_open");
        // 探测在飞行时不再放第二个
        assert!(breaker.allow().is_err());
    }

    #[test]
    fn probe_success_closes_and_probe_failure_reopens() {
        let clock = Arc::new(TestClock::at_ms(0));
        let breaker = CircuitBreaker::new(clock.clone());
        for _ in 0..3 {
            breaker.record_failure(&transient());
        }
        clock.advance_ms(60_000);
        assert!(breaker.allow().is_ok(), "放行探测");
        breaker.record_success();
        assert_eq!(breaker.state_label(), "closed");
        assert_eq!(breaker.consecutive_failures(), 0);
        assert!(breaker.allow().is_ok());

        // 再来一轮，探测失败 → 重新打开
        for _ in 0..3 {
            breaker.record_failure(&transient());
        }
        clock.advance_ms(60_000);
        assert!(breaker.allow().is_ok());
        breaker.record_failure(&transient());
        assert_eq!(breaker.state_label(), "open");
        assert!(breaker.allow().is_err());
    }

    #[test]
    fn permanent_errors_never_open_the_circuit() {
        let clock = Arc::new(TestClock::at_ms(0));
        let breaker = CircuitBreaker::new(clock);
        for _ in 0..10 {
            breaker.record_failure(&permanent());
            breaker.record_failure(&InferenceError::SchemaViolation {
                reason: "未知字段".to_owned(),
            });
            breaker.record_failure(&InferenceError::CloudDisabled);
        }
        assert_eq!(breaker.state_label(), "closed");
        assert_eq!(breaker.consecutive_failures(), 0);
        assert!(breaker.allow().is_ok());
    }

    #[test]
    fn success_resets_the_consecutive_counter() {
        let clock = Arc::new(TestClock::at_ms(0));
        let breaker = CircuitBreaker::new(clock);
        breaker.record_failure(&transient());
        breaker.record_failure(&transient());
        breaker.record_success();
        breaker.record_failure(&transient());
        assert_eq!(breaker.consecutive_failures(), 1);
        assert_eq!(breaker.state_label(), "closed");
    }

    #[test]
    fn custom_config_is_honoured() {
        let clock = Arc::new(TestClock::at_ms(0));
        let breaker = CircuitBreaker::with_config(
            clock.clone(),
            BreakerConfig {
                failure_threshold: 2,
                open_duration_ms: 5_000,
            },
        );
        breaker.record_failure(&transient());
        breaker.record_failure(&transient());
        assert_eq!(breaker.state_label(), "open");
        clock.advance_ms(5_000);
        assert!(breaker.allow().is_ok());
    }
}
