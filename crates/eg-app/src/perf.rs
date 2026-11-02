//! UI 线程性能记账（`SPEC-app.md` §4 预算、§6 可观测性、§9 成功标准 1–3）。
//!
//! 记录三类「UI 线程同步工作」的耗时：
//! - 导航处理（快捷键/点击 → 状态迁移 + Slint 属性投影）；
//! - 后台事件应用（`invoke_from_event_loop` 回投后的状态更新 + 投影）；
//! - 命令投递（`CommandBus::try_send`，含通道满提示路径）。
//!
//! 采样有上限（[`MAX_SAMPLES`]），避免长会话内存增长；本模块纯逻辑可单测。
//! 说明：这里量的是 **UI 线程同步工作**，不含 GPU 渲染/合成时间（Slint 未暴露帧计时，
//! 见 `docs/dev/research/app.md` 的「测量口径与限制」）。

use std::sync::Mutex;
use std::time::Duration;

/// 每类指标最大采样数（超过后不再追加，避免无界内存）。
pub(crate) const MAX_SAMPLES: usize = 4096;

/// UI 线程指标采样器。
#[derive(Debug, Default)]
pub(crate) struct UiMetrics {
    nav: Mutex<Vec<f64>>,
    event: Mutex<Vec<f64>>,
    command: Mutex<Vec<f64>>,
    invoke_failures: std::sync::atomic::AtomicU64,
}

/// 单类分位结果。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MetricSummary {
    /// 样本数。
    pub(crate) samples: usize,
    /// P50（毫秒）。
    pub(crate) p50_ms: f64,
    /// P95（毫秒）。
    pub(crate) p95_ms: f64,
    /// P99（毫秒）。
    pub(crate) p99_ms: f64,
    /// 最大值（毫秒）。
    pub(crate) max_ms: f64,
}

/// 三类指标汇总。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MetricsReport {
    /// 导航。
    pub(crate) nav: MetricSummary,
    /// 事件应用。
    pub(crate) event: MetricSummary,
    /// 命令投递。
    pub(crate) command: MetricSummary,
    /// `invoke_from_event_loop` 失败次数（事件循环未运行/已退出）。
    pub(crate) invoke_failures: u64,
}

fn push_sample(samples: &Mutex<Vec<f64>>, elapsed: Duration) {
    let Ok(mut guard) = samples.lock() else {
        return;
    };
    if guard.len() < MAX_SAMPLES {
        guard.push(elapsed.as_secs_f64() * 1000.0);
    }
}

#[must_use]
fn summary(samples: &Mutex<Vec<f64>>) -> MetricSummary {
    let Ok(mut guard) = samples.lock() else {
        return MetricSummary {
            samples: 0,
            p50_ms: 0.0,
            p95_ms: 0.0,
            p99_ms: 0.0,
            max_ms: 0.0,
        };
    };
    if guard.is_empty() {
        return MetricSummary {
            samples: 0,
            p50_ms: 0.0,
            p95_ms: 0.0,
            p99_ms: 0.0,
            max_ms: 0.0,
        };
    }
    guard.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let max = guard.last().copied().unwrap_or(0.0);
    MetricSummary {
        samples: guard.len(),
        p50_ms: percentile(&guard, 0.50),
        p95_ms: percentile(&guard, 0.95),
        p99_ms: percentile(&guard, 0.99),
        max_ms: max,
    }
}

impl UiMetrics {
    /// 记录一次导航处理。
    pub(crate) fn record_nav(&self, elapsed: Duration) {
        push_sample(&self.nav, elapsed);
    }

    /// 记录一次后台事件应用。
    pub(crate) fn record_event(&self, elapsed: Duration) {
        push_sample(&self.event, elapsed);
    }

    /// 记录一次命令投递。
    pub(crate) fn record_command(&self, elapsed: Duration) {
        push_sample(&self.command, elapsed);
    }

    /// 记录一次 `invoke_from_event_loop` 失败（UI 已退出/事件循环未启动）。
    pub(crate) fn note_invoke_failure(&self) {
        self.invoke_failures
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// 汇总报告。
    #[must_use]
    pub(crate) fn report(&self) -> MetricsReport {
        MetricsReport {
            nav: summary(&self.nav),
            event: summary(&self.event),
            command: summary(&self.command),
            invoke_failures: self
                .invoke_failures
                .load(std::sync::atomic::Ordering::Relaxed),
        }
    }
}

/// 线性插值分位（输入需已升序）。
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "样本数 ≤4096 且 p∈[0,1]，f64 精度远高于所需"
)]
#[must_use]
pub(crate) fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = p.clamp(0.0, 1.0) * (sorted.len() - 1) as f64;
    let lower = rank.floor() as usize;
    let upper = rank.ceil() as usize;
    if lower == upper {
        return sorted[lower];
    }
    let weight = rank - lower as f64;
    sorted[lower] * (1.0 - weight) + sorted[upper] * weight
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn percentiles_interpolate() {
        let values = [1.0, 2.0, 3.0, 4.0, 5.0];
        assert!((percentile(&values, 0.0) - 1.0).abs() < f64::EPSILON);
        assert!((percentile(&values, 0.5) - 3.0).abs() < f64::EPSILON);
        assert!((percentile(&values, 1.0) - 5.0).abs() < f64::EPSILON);
        assert!((percentile(&values, 0.25) - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn metrics_summarize_and_cap_samples() {
        let metrics = UiMetrics::default();
        for i in 0..MAX_SAMPLES + 10 {
            metrics.record_nav(Duration::from_micros(u64::try_from(i).unwrap()));
        }
        let report = metrics.report();
        assert_eq!(report.nav.samples, MAX_SAMPLES, "采样应有上限");
        assert!(report.nav.p50_ms <= report.nav.p95_ms);
        assert!(report.nav.p95_ms <= report.nav.p99_ms);
        assert!(report.nav.max_ms >= report.nav.p99_ms);
        assert_eq!(report.event.samples, 0);
        metrics.note_invoke_failure();
        assert_eq!(metrics.report().invoke_failures, 1);
    }

    #[test]
    fn three_metric_kinds_are_independent() {
        let metrics = UiMetrics::default();
        metrics.record_nav(Duration::from_millis(1));
        metrics.record_event(Duration::from_millis(2));
        metrics.record_command(Duration::from_millis(3));
        let report = metrics.report();
        assert_eq!(report.nav.samples, 1);
        assert_eq!(report.event.samples, 1);
        assert_eq!(report.command.samples, 1);
    }
}
