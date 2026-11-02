//! UI 性能预算实测（`SPEC-app.md` §4 预算、§9 成功标准 1–3；任务 T35）。
//!
//! 测量口径（与 `docs/dev/research/app.md` 一致）：
//! - **切页 P95 ≤100ms**：10 页 × 20 轮 = 200 次「快捷键/点击 → 状态迁移 → 属性投影」；
//! - **UI 线程 p99 ≤16ms**：导航、后台事件应用、命令投递三类同步工作分别取 p99；
//! - **冷启动 P95 ≤1.5s**：由脚本多次运行 `--smoke` 采集「进程启动 → 首帧回调」，
//!   本模块只输出单次启动分阶段耗时（平台初始化/组件实例化/`show()`/首帧）。
//!
//! 本模块提供的是**单一进程内**指标；跨进程冷启动分位由 `run_smoke` 输出 + 外部脚本汇总。

use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::AppWindow;
use crate::channel::{Command, CommandBus, Event, NoticeLevel};
use crate::nav::PageId;
use crate::perf::UiMetrics;
use crate::state::AppState;
use crate::ui_bridge;

/// 切页轮数（10 页 × 20 轮 = 200 样本）。
pub(crate) const NAV_ROUNDS: usize = 20;
/// 事件应用样本数。
pub(crate) const EVENT_SAMPLES: usize = 200;
/// 命令投递样本数。
pub(crate) const COMMAND_SAMPLES: usize = 50;

/// 单次启动的分阶段耗时（毫秒）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct StartupStages {
    /// 平台初始化（单实例锁/配置/迁移检查）。
    pub(crate) platform_open: f64,
    /// 组件实例化。
    pub(crate) ui_new: f64,
    /// `show()`。
    pub(crate) ui_show: f64,
    /// 进程启动 → 首帧回调。
    pub(crate) first_frame: f64,
}

/// 性能实测结果。
#[derive(Debug, Clone, Copy)]
pub(crate) struct BenchReport {
    /// 启动阶段。
    pub(crate) startup: StartupStages,
    /// 导航 p50/p95/p99/max。
    pub(crate) nav: crate::perf::MetricSummary,
    /// 事件应用分位。
    pub(crate) event: crate::perf::MetricSummary,
    /// 命令投递分位。
    pub(crate) command: crate::perf::MetricSummary,
    /// `invoke_from_event_loop` 失败计数。
    pub(crate) invoke_failures: u64,
}

impl BenchReport {
    /// 是否满足 `SPEC-app.md` §9 标准 2/3（切页 P95 ≤100ms、UI 线程 p99 ≤16ms）。
    #[must_use]
    pub(crate) fn meets_budget(&self) -> bool {
        let nav_budget = self.nav.p95_ms <= 100.0 && self.nav.p99_ms <= 16.0;
        let event_budget = self.event.p99_ms <= 16.0;
        let command_budget = self.command.p99_ms <= 16.0;
        nav_budget && event_budget && command_budget
    }

    /// 打印稳定的机器可读行（供 `docs/dev/research/app.md` 归档与后续回归对比）。
    pub(crate) fn print(&self) {
        println!(
            "[bench] startup_ms={{\"platform_open\":{:.3},\"ui_new\":{:.3},\"ui_show\":{:.3},\"first_frame\":{:.3}}}",
            self.startup.platform_open,
            self.startup.ui_new,
            self.startup.ui_show,
            self.startup.first_frame
        );
        print_summary("nav_ms", &self.nav);
        print_summary("event_ms", &self.event);
        print_summary("command_ms", &self.command);
        println!("[bench] invoke_failures={}", self.invoke_failures);
        println!(
            "[bench] verdict={}",
            if self.meets_budget() { "pass" } else { "fail" }
        );
    }
}

fn print_summary(label: &str, summary: &crate::perf::MetricSummary) {
    println!(
        "[bench] {label}={{\"samples\":{},\"p50\":{:.3},\"p95\":{:.3},\"p99\":{:.3},\"max\":{:.3}}}",
        summary.samples, summary.p50_ms, summary.p95_ms, summary.p99_ms, summary.max_ms
    );
}

/// 在 UI 线程执行性能实测（须在事件循环内调用；结束后调用方负责退出事件循环）。
#[must_use]
pub(crate) fn run(
    ui: &AppWindow,
    state: &Arc<Mutex<AppState>>,
    metrics: &Arc<UiMetrics>,
    command_bus: &CommandBus,
    startup: StartupStages,
) -> BenchReport {
    // 1) 切页：10 页 × 20 轮。
    for _ in 0..NAV_ROUNDS {
        for page in PageId::ALL {
            ui_bridge::navigate(ui, state, metrics, page.index());
        }
    }

    // 2) 后台事件应用：进度/通知/后端健康轮换（均为 UI 线程同步工作）。
    for i in 0..EVENT_SAMPLES {
        let event = match i % 3 {
            0 => Event::Progress {
                task_id: format!("bench-{}", i % 5),
                label: "识别".to_owned(),
                done: u32::try_from(i).unwrap_or(u32::MAX),
                total: 40,
            },
            1 => Event::Notice {
                level: NoticeLevel::Info,
                title: "bench".to_owned(),
                body: format!("事件 {i}"),
            },
            _ => Event::BackendHealth {
                ready: i % 2 == 0,
                message: "bench".to_owned(),
            },
        };
        ui_bridge::apply_event_now(ui, state, metrics, event);
    }

    // 3) 命令投递（非阻塞；worker 会异步处理并回投事件）。
    for _ in 0..COMMAND_SAMPLES {
        let started = Instant::now();
        let _ = command_bus.try_send(Command::RetryError);
        metrics.record_command(started.elapsed());
    }

    let report = metrics.report();
    BenchReport {
        startup,
        nav: report.nav,
        event: report.event,
        command: report.command,
        invoke_failures: report.invoke_failures,
    }
}
