//! `eg-app`：Slint 桌面外壳与组合根（M9 / T35 外壳、十页导航、状态与通道）。
//!
//! 规范：`SPEC-app.md` v0.2.3（§2 页面/快捷键、§3 视图模型边界、§4 启动与线程、
//! §5 失败模式、§9 性能预算）· `CONFIG.md` · `docs/dev/research/slint.md`（1.18.1 实测）。
//!
//! 线程模型：
//! - **UI 线程**（Slint 事件循环）：状态迁移 + 属性投影，零阻塞、零同步 IO；
//! - **后台 worker**：独占 `Platform`，消费容量 64 的命令通道，经事件总线回投；
//! - **事件泵线程**：接收容量 256 的事件通道，经 `invoke_from_event_loop` 投到 UI，
//!   通道满时按「合并进度 / 保留错误 / 计数丢弃」策略处理（`channel.rs`）。
//!
//! 运行方式（开发机直接运行，A7 不做打包）：
//! - 正常：`cargo run -p eg-app`；
//! - 冒烟：`cargo run -p eg-app -- --smoke [--demo-error]`（打印首帧与错误呈现状态后退出）；
//! - 性能：`cargo run -p eg-app -- --bench`（打印切页/事件/命令分位，未达标退出码 2）。

#![allow(
    dead_code,
    clippy::assert_is_empty,
    clippy::assigning_clones,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::enum_variant_names,
    clippy::let_unit_value,
    clippy::match_same_arms,
    clippy::needless_pass_by_value,
    clippy::struct_excessive_bools,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::unnecessary_to_owned,
    clippy::unused_self,
    reason = "页面视图模型/事件枚举：展示字段与命令集中分派；未接线的 VM API 在 UI 收尾前保留；\
              与 eg-platform lib.rs 的既有 allow 清单一致"
)]

mod bench;
mod channel;
mod gates;
mod i18n;
mod nav;
mod pages;
mod perf;
mod platform_adapters;
mod services;
mod state;
mod ui_bridge;
mod worker;

// Slint 生成代码（include_modules!）含大量 unwrap/panic/todo 与 UI 内部 `pub` 项：
// 它是编译器生成的可信代码，不属于本 crate 手写代码的 lint 范围。用独立 module + allow
// 隔离，避免污染全 crate 的拒绝级 lint（记录：docs/dev/research/app.md「与规范的偏差」）。
mod ui {
    #[allow(
        clippy::all,
        clippy::pedantic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::todo,
        clippy::dbg_macro
    )]
    mod generated {
        slint::include_modules!();
    }
    pub(crate) use generated::*;
}
pub(crate) use ui::*;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use eg_platform::{DataDirs, Platform, TaskState};
use slint::{ComponentHandle as _, ModelRc, VecModel};

use crate::bench::StartupStages;
use crate::channel::{ChannelStats, Command, CommandBus, Event, EventBus, NoticeLevel};
use crate::i18n::Strings;
use crate::nav::{Modifiers, PageId, Shortcut};
use crate::perf::UiMetrics;
use crate::state::{AppState, ConfirmKind};
use crate::ui_bridge::UiDispatcher;

/// bench 是否未达标（用于进程退出码）。
static BENCH_FAILED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunMode {
    /// 正常常驻窗口。
    App,
    /// 冒烟：打印首帧/错误呈现后自动退出。
    Smoke,
    /// 性能实测：打印分位后自动退出（未达标退出码 2）。
    Bench,
}

#[derive(Debug, Clone, Copy)]
struct Cli {
    mode: RunMode,
    demo_error: bool,
    hold_ms: u64,
}

fn main() {
    if std::env::args().any(|arg| arg == "--help" || arg == "-h") {
        println!("用法：eg-app [--smoke|--bench] [--demo-error]");
        println!("  --smoke        启动后打印首帧耗时与错误呈现状态并退出");
        println!("  --bench        运行切页/事件/命令性能实测并退出（未达标退出码 2）");
        println!("  --demo-error   冒烟时注入一条模拟任务失败，验证错误横幅与详情展开");
        return;
    }
    let cli = match parse_cli() {
        Ok(cli) => cli,
        Err(message) => {
            eprintln!("参数错误：{message}");
            std::process::exit(2);
        }
    };
    if let Err(error) = run(&cli) {
        eprintln!("[eg-app] 运行失败：{error}");
        std::process::exit(1);
    }
}

fn parse_cli() -> Result<Cli, String> {
    let mut mode = RunMode::App;
    let mut demo_error = false;
    let mut hold_ms = 1_200;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--smoke" => mode = RunMode::Smoke,
            "--bench" => mode = RunMode::Bench,
            "--demo-error" => demo_error = true,
            other => return Err(format!("未知参数「{other}」（--help 查看用法）")),
        }
    }
    if let Ok(value) = std::env::var("EG_APP_HOLD_MS")
        && let Ok(parsed) = value.parse::<u64>()
    {
        hold_ms = parsed;
    }
    Ok(Cli {
        mode,
        demo_error,
        hold_ms,
    })
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn ms_to_u64(ms: f64) -> u64 {
    if ms <= 0.0 { 0 } else { ms.round() as u64 }
}

#[allow(
    clippy::too_many_lines,
    reason = "组合根启动序列与线程装配按规范顺序集中展示，拆分反而遮蔽启动路径"
)]
fn run(cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    let t0 = Instant::now();
    let strings = i18n::strings();
    let dirs = DataDirs::resolve()?;

    // 启动序列（`SPEC-app.md` §4）：单实例锁 → 配置 → 数据库/迁移 → 日志 → 首页快照。
    let platform = match Platform::open(dirs) {
        Ok(platform) => platform,
        Err(error) => return run_startup_error(&error.to_string(), cli, strings, t0),
    };
    let platform_open_ms = elapsed_ms(t0);
    let config = platform.config().clone();
    let counts = platform.tasks().count_by_state().unwrap_or_default();
    let running = u32::try_from(*counts.get(&TaskState::Running).unwrap_or(&0)).unwrap_or(0);
    let pending = u32::try_from(*counts.get(&TaskState::Pending).unwrap_or(&0)).unwrap_or(0);
    let state = Arc::new(Mutex::new(AppState::new(
        config.general.mode,
        config.fast.enabled,
        running,
        pending,
    )));
    let metrics = Arc::new(UiMetrics::default());

    let ui_new_started = Instant::now();
    let ui = AppWindow::new()?;
    let ui_new_ms = elapsed_ms(ui_new_started);
    ui_bridge::apply_state(&ui, &state, strings);

    let show_started = Instant::now();
    ui.show()?;
    let ui_show_ms = elapsed_ms(show_started);
    ui.invoke_take_focus();

    let (command_bus, command_rx) = CommandBus::new();
    let (event_bus, event_rx) = EventBus::new();
    let event_bus_stats = event_bus.clone();
    let dispatcher = Arc::new(UiDispatcher::new(
        ui.as_weak(),
        Arc::clone(&state),
        Arc::clone(&metrics),
    ));
    let threads = Arc::new(Mutex::new(Threads::default()));
    let stats_timer = slint::Timer::default();
    let stages = Arc::new(Mutex::new(StartupStages {
        platform_open: platform_open_ms,
        ui_new: ui_new_ms,
        ui_show: ui_show_ms,
        first_frame: 0.0,
    }));

    wire_callbacks(ui.as_weak(), &state, &metrics, &command_bus, strings);

    schedule_start(StartContext {
        ui: ui.as_weak(),
        cli: *cli,
        t0,
        platform,
        command_rx,
        command_bus: command_bus.clone(),
        event_bus,
        event_rx,
        dispatcher,
        threads: Arc::clone(&threads),
        state: Arc::clone(&state),
        metrics: Arc::clone(&metrics),
        stages: Arc::clone(&stages),
        strings,
    });

    // 通道计数每 250ms 刷新到状态栏（合并/丢弃/命令拒绝显式可见）。
    {
        let weak = ui.as_weak();
        let state = Arc::clone(&state);
        let event_bus = event_bus_stats.clone();
        let commands = command_bus.clone();
        stats_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(250),
            move || {
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                let mut bus_stats = event_bus.stats();
                bus_stats.command_rejected_total = commands.rejected_total();
                bus_stats.command_depth = commands.depth();
                let view = {
                    let Ok(mut guard) = state.lock() else {
                        return;
                    };
                    guard.update_channel_stats(bus_stats);
                    guard.view(strings)
                };
                ui_bridge::apply_view(&ui, &view, strings);
            },
        );
    }

    slint::run_event_loop()?;
    stats_timer.stop();
    let _ = ui.hide();
    stop_threads(&threads, &command_bus);
    let exit_stats = state.lock().map_or(ChannelStats::default(), |guard| {
        let mut snapshot = guard.channel_stats();
        let _ = guard.toasts().len();
        snapshot.command_rejected_total = command_bus.rejected_total();
        snapshot
    });
    eprintln!(
        "[eg-app] exit: command_rejected={} event_merged={} event_dropped={} critical_dropped={}",
        exit_stats.command_rejected_total,
        exit_stats.merged_total,
        exit_stats.dropped_total,
        exit_stats.critical_dropped_total
    );
    if cli.mode == RunMode::Bench && BENCH_FAILED.load(Ordering::Relaxed) {
        std::process::exit(2);
    }
    Ok(())
}

/// 后台线程句柄（事件泵 + worker）。
#[derive(Default)]
struct Threads {
    pump: Option<JoinHandle<()>>,
    worker: Option<JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
}

struct StartContext {
    ui: slint::Weak<AppWindow>,
    cli: Cli,
    t0: Instant,
    platform: Platform,
    command_rx: Receiver<Command>,
    command_bus: CommandBus,
    event_bus: EventBus,
    event_rx: Receiver<Event>,
    dispatcher: Arc<UiDispatcher>,
    threads: Arc<Mutex<Threads>>,
    state: Arc<Mutex<AppState>>,
    metrics: Arc<UiMetrics>,
    stages: Arc<Mutex<StartupStages>>,
    strings: &'static Strings,
}

/// 在事件循环首个回合启动线程、记录启动耗时，并按运行模式安排后续计时器。
fn schedule_start(context: StartContext) {
    slint::Timer::single_shot(Duration::from_millis(1), move || {
        let StartContext {
            ui,
            cli,
            t0,
            platform,
            command_rx,
            command_bus,
            event_bus,
            event_rx,
            dispatcher,
            threads,
            state,
            metrics,
            stages,
            strings,
        } = context;
        let Some(ui_handle) = ui.upgrade() else {
            return;
        };
        let first_frame_ms = elapsed_ms(t0);
        let startup = {
            let Ok(mut guard) = stages.lock() else {
                return;
            };
            guard.first_frame = first_frame_ms;
            *guard
        };

        let shutdown = {
            let Ok(guard) = threads.lock() else {
                return;
            };
            Arc::clone(&guard.shutdown)
        };
        let Ok(pump) = spawn_pump(
            event_rx,
            event_bus.clone(),
            Arc::clone(&dispatcher),
            Arc::clone(&shutdown),
        ) else {
            eprintln!("[eg-app] 事件泵线程创建失败");
            let _ = slint::quit_event_loop();
            return;
        };
        let Ok(worker) = worker::spawn(
            platform,
            command_rx,
            command_bus.clone(),
            event_bus,
            strings,
        ) else {
            eprintln!("[eg-app] 工作线程创建失败");
            let _ = slint::quit_event_loop();
            return;
        };
        if let Ok(mut guard) = threads.lock() {
            guard.pump = Some(pump);
            guard.worker = Some(worker);
        }

        let _ = command_bus.try_send(Command::LogStartup {
            platform_open_ms: ms_to_u64(startup.platform_open),
            ui_new_ms: ms_to_u64(startup.ui_new),
            ui_show_ms: ms_to_u64(startup.ui_show),
            first_frame_ms: ms_to_u64(startup.first_frame),
        });

        match cli.mode {
            RunMode::App => {}
            RunMode::Smoke => {
                // 冒烟时触发一次占位后台任务，验证「worker → 事件通道 → UI 进度投影」。
                let _ = command_bus.try_send(Command::RunPlaceholderJob);
                println!(
                    "[smoke] startup_ms={{\"platform_open\":{:.3},\"ui_new\":{:.3},\"ui_show\":{:.3},\"first_frame\":{:.3}}}",
                    startup.platform_open, startup.ui_new, startup.ui_show, startup.first_frame
                );
                let _ = command_bus.try_send(Command::RunSmokeChain);
                schedule_smoke(
                    ui_handle.as_weak(),
                    &state,
                    &metrics,
                    &command_bus,
                    cli,
                    startup.first_frame,
                    strings,
                );
            }
            RunMode::Bench => {
                schedule_bench(ui_handle.as_weak(), state, metrics, command_bus, startup);
            }
        }
    });
}

/// 逐页冒烟：导航 + 请求加载 + 等待事件回投，打印每页首行数据。
#[allow(clippy::too_many_arguments)]
fn smoke_pages(
    ui: slint::Weak<AppWindow>,
    state: Arc<Mutex<AppState>>,
    metrics: Arc<UiMetrics>,
    commands: CommandBus,
    pages: &'static [PageId],
    index: usize,
    collected: Arc<Mutex<Vec<String>>>,
    strings: &'static Strings,
) {
    if index >= pages.len() {
        return;
    }
    let page = pages[index];
    if let Some(handle) = ui.upgrade() {
        let _ = ui_bridge::navigate(&handle, &state, &metrics, page.index());
        let _ = commands.try_send(Command::PageAction {
            id: format!("page.load.{}", page.code()),
        });
    }
    let ui_next = ui.clone();
    let state_next = Arc::clone(&state);
    let metrics_next = Arc::clone(&metrics);
    let commands_next = commands.clone();
    let collected_next = Arc::clone(&collected);
    slint::Timer::single_shot(Duration::from_millis(190), move || {
        if let Ok(guard) = state_next.lock() {
            let view = guard.view(strings);
            let head = view
                .page_body
                .lines()
                .next()
                .unwrap_or_default()
                .chars()
                .take(72)
                .collect::<String>();
            drop(guard);
            if let Ok(mut items) = collected_next.lock() {
                items.push(format!("{}: {head}", page.code()));
            }
        }
        smoke_pages(
            ui_next,
            state_next,
            metrics_next,
            commands_next,
            pages,
            index + 1,
            collected_next,
            strings,
        );
    });
}

fn schedule_smoke(
    ui: slint::Weak<AppWindow>,
    state: &Arc<Mutex<AppState>>,
    metrics: &Arc<UiMetrics>,
    command_bus: &CommandBus,
    cli: Cli,
    first_frame_ms: f64,
    strings: &'static Strings,
) {
    let state = Arc::clone(state);
    let metrics = Arc::clone(metrics);
    let commands = command_bus.clone();
    let hold = cli.hold_ms;
    let demo_error = cli.demo_error;
    slint::Timer::single_shot(Duration::from_millis(hold), move || {
        let Some(ui_handle) = ui.upgrade() else {
            return;
        };
        if demo_error {
            let event = Event::TaskFailed {
                task_id: "smoke-task".to_owned(),
                message: "演示错误：模拟任务失败（--demo-error）".to_owned(),
                retryable: true,
                diagnostic_id: "smoke-diag-0001".to_owned(),
            };
            ui_bridge::apply_event_now(&ui_handle, &state, &metrics, event);
            eprintln!("[smoke] injected_error=1 first_frame_ms={first_frame_ms:.3}");
        }
        let collected = Arc::new(Mutex::new(Vec::<String>::new()));
        // 等真实导入/复核/导出链路完成（含图片解码），再逐页采集，避免采到链路前快照。
        let ui_chain = ui_handle.as_weak();
        let state_chain = Arc::clone(&state);
        let metrics_chain = Arc::clone(&metrics);
        let commands_chain = commands.clone();
        let collected_chain = Arc::clone(&collected);
        slint::Timer::single_shot(Duration::from_millis(3200), move || {
            smoke_pages(
                ui_chain,
                state_chain,
                metrics_chain,
                commands_chain,
                &PageId::ALL,
                0,
                collected_chain,
                strings,
            );
        });
        let ui = ui_handle.as_weak();
        slint::Timer::single_shot(Duration::from_millis(6200), move || {
            if let Ok(items) = collected.lock() {
                for item in items.iter() {
                    println!("[smoke] page {item}");
                }
            }
            if let Some(ui) = ui.upgrade() {
                println!(
                    "[smoke] error_visible={} error_detail_visible={} page_index={} mode=\"{}\" progress=\"{}\" channel=\"{}\" image_note=\"{}\" hint=\"{}\"",
                    ui.get_error_visible(),
                    ui.get_error_detail_visible(),
                    ui.get_current_index(),
                    ui.get_mode_label(),
                    ui.get_progress_text(),
                    ui.get_channel_text(),
                    ui.get_review_image_note(),
                    ui.get_hint()
                );
                let _ = strings;
            }
            let _ = slint::quit_event_loop();
        });
    });
}

fn schedule_bench(
    ui: slint::Weak<AppWindow>,
    state: Arc<Mutex<AppState>>,
    metrics: Arc<UiMetrics>,
    command_bus: CommandBus,
    startup: StartupStages,
) {
    let stages = Arc::new(Mutex::new(startup));
    slint::Timer::single_shot(Duration::from_millis(150), move || {
        let Some(ui_handle) = ui.upgrade() else {
            return;
        };
        let startup = stages.lock().map_or(startup, |guard| *guard);
        let report = bench::run(&ui_handle, &state, &metrics, &command_bus, startup);
        report.print();
        if !report.meets_budget() {
            BENCH_FAILED.store(true, Ordering::Relaxed);
            eprintln!("[bench] 未达标：切页 P95 应 ≤100ms、p99 ≤16ms；UI 线程 p99 应 ≤16ms");
        }
        let _ = slint::quit_event_loop();
    });
}

fn spawn_pump(
    receiver: Receiver<Event>,
    bus: EventBus,
    dispatcher: Arc<UiDispatcher>,
    shutdown: Arc<AtomicBool>,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("eg-app-event-pump".to_owned())
        .spawn(move || {
            loop {
                match receiver.recv_timeout(Duration::from_millis(16)) {
                    Ok(event) => {
                        bus.record_receive();
                        dispatcher.post(event);
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
                if shutdown.load(Ordering::Relaxed) {
                    break;
                }
                for event in bus.take_carry() {
                    dispatcher.post(event);
                }
            }
        })
}

fn stop_threads(threads: &Arc<Mutex<Threads>>, command_bus: &CommandBus) {
    let (shutdown, worker, pump) = {
        let Ok(mut guard) = threads.lock() else {
            return;
        };
        (
            Arc::clone(&guard.shutdown),
            guard.worker.take(),
            guard.pump.take(),
        )
    };
    shutdown.store(true, Ordering::Relaxed);
    let _ = command_bus.try_send(Command::Quit);
    if let Some(handle) = worker {
        let _ = handle.join();
    }
    if let Some(handle) = pump {
        let _ = handle.join();
    }
}

#[allow(
    clippy::too_many_lines,
    clippy::needless_pass_by_value,
    reason = "回调接线按「导航/快捷键/模式/错误/入口」分组集中展示；Weak 句柄直接 clone 分发"
)]
fn wire_callbacks(
    ui: slint::Weak<AppWindow>,
    state: &Arc<Mutex<AppState>>,
    metrics: &Arc<UiMetrics>,
    command_bus: &CommandBus,
    strings: &'static Strings,
) {
    let Some(handle) = ui.upgrade() else {
        return;
    };

    // 导航点击：切页后只发一条「加载本页」命令（IO 在 worker）。
    {
        let weak = ui.clone();
        let state = Arc::clone(state);
        let metrics = Arc::clone(metrics);
        let commands = command_bus.clone();
        handle.on_navigate(move |index| {
            if let Some(ui) = weak.upgrade() {
                let index = usize::try_from(index).unwrap_or(0);
                let _ = ui_bridge::navigate(&ui, &state, &metrics, index);
                if let Some(page) = PageId::from_index(index) {
                    load_page(&ui, &state, &metrics, &commands, page);
                }
            }
        });
    }

    // 全局快捷键（Slint 只转发按键；解析在 nav.rs，可单测）。
    {
        let weak = ui.clone();
        let state = Arc::clone(state);
        let metrics = Arc::clone(metrics);
        let state_commands = command_bus.clone();
        handle.on_shortcut(move |text, control, shift, alt, meta| {
            let Some(ui) = weak.upgrade() else {
                return false;
            };
            let page = state.lock().map_or(PageId::Home, |guard| guard.current());
            // 复核页：J/K/Enter/E/Space/X/数字/Cmd+Z（文本编辑态由 UI 焦点保证不进来）。
            if page == PageId::Review
                && let Some(key) = crate::pages::review::map_key(text.as_str(), control, false)
            {
                let commands = &state_commands;
                dispatch_review_key(&ui, &state, &metrics, commands, key);
                return true;
            }
            if page == PageId::Rules && control && text.as_str().eq_ignore_ascii_case("Enter") {
                send_command(
                    &ui,
                    &state,
                    &metrics,
                    &state_commands,
                    strings,
                    Command::RulesConfirm,
                );
                return true;
            }
            let Some(action) =
                nav::resolve_shortcut(text.as_str(), Modifiers::new(control, shift, alt, meta))
            else {
                return false;
            };
            match action {
                Shortcut::Navigate(page) => {
                    let _ = ui_bridge::navigate(&ui, &state, &metrics, page.index());
                    load_page(&ui, &state, &metrics, &state_commands, page);
                }
                Shortcut::Dismiss => {
                    let _ = dismiss_top(&ui, &state, strings);
                }
                Shortcut::ToggleMode => {
                    request_mode_toggle(&ui, &state, strings);
                }
                Shortcut::ImportFiles => {
                    let _ = ui_bridge::navigate(&ui, &state, &metrics, PageId::Import.index());
                    send_command(
                        &ui,
                        &state,
                        &metrics,
                        &state_commands,
                        strings,
                        Command::ImportPick { folder: false },
                    );
                }
                Shortcut::ImportFolder => {
                    let _ = ui_bridge::navigate(&ui, &state, &metrics, PageId::Import.index());
                    send_command(
                        &ui,
                        &state,
                        &metrics,
                        &state_commands,
                        strings,
                        Command::ImportPick { folder: true },
                    );
                }
            }
            true
        });
    }

    // 模式切换按钮与二次确认。
    {
        let weak = ui.clone();
        let state = Arc::clone(state);
        handle.on_toggle_mode(move || {
            if let Some(ui) = weak.upgrade() {
                request_mode_toggle(&ui, &state, strings);
            }
        });
    }
    {
        let weak = ui.clone();
        let state = Arc::clone(state);
        let metrics = Arc::clone(metrics);
        let commands = command_bus.clone();
        handle.on_confirm_yes(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            confirm_yes(&ui, &state, &metrics, &commands, strings);
        });
    }
    {
        let weak = ui.clone();
        let state = Arc::clone(state);
        handle.on_confirm_no(move || {
            if let Some(ui) = weak.upgrade() {
                cancel_confirm(&ui, &state, strings);
            }
        });
    }

    // 页内主操作（占位命令经有界通道去 worker）。
    {
        let weak = ui.clone();
        let state = Arc::clone(state);
        let metrics = Arc::clone(metrics);
        let commands = command_bus.clone();
        handle.on_primary_action(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let page = state.lock().map_or(PageId::Home, |guard| guard.current());
            let command = if page == PageId::Progress {
                Command::RunPlaceholderJob
            } else if page == PageId::Report {
                Command::ExportReport
            } else {
                Command::PrimaryAction { page }
            };
            send_command(&ui, &state, &metrics, &commands, strings, command);
        });
    }

    // 页面动作按钮（id 来自页面视图模型）。
    {
        let weak = ui.clone();
        let state = Arc::clone(state);
        let metrics = Arc::clone(metrics);
        let commands = command_bus.clone();
        handle.on_page_action(move |id| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if id.as_str() == "import.open_progress" {
                let _ = ui_bridge::navigate(&ui, &state, &metrics, PageId::Progress.index());
                load_page(&ui, &state, &metrics, &commands, PageId::Progress);
                return;
            }
            for command in page_action_commands(&state, id.as_str()) {
                send_command(&ui, &state, &metrics, &commands, strings, command);
            }
        });
    }

    // 表单控件回调（设置/复核改分与原因/抽检范围/规则手写/授权登记）。
    {
        let weak = ui.clone();
        let state = Arc::clone(state);
        let metrics = Arc::clone(metrics);
        let commands = command_bus.clone();
        handle.on_apply_setting(move |key, value| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            send_command(
                &ui,
                &state,
                &metrics,
                &commands,
                strings,
                Command::SettingsPatch {
                    key: key.to_string(),
                    raw: value.to_string(),
                },
            );
        });
    }
    {
        let state = Arc::clone(state);
        handle.on_review_score_changed(move |text| {
            let Ok(mut guard) = state.lock() else {
                return;
            };
            guard.review_page_mut().set_score_input(text.to_string());
        });
    }
    {
        let state = Arc::clone(state);
        handle.on_review_reason_changed(move |reason| {
            let Ok(mut guard) = state.lock() else {
                return;
            };
            guard
                .review_page_mut()
                .select_reason_by_text(reason.as_str());
        });
    }
    {
        let weak = ui.clone();
        let state = Arc::clone(state);
        let metrics = Arc::clone(metrics);
        let commands = command_bus.clone();
        handle.on_sampling_scope(move |scope| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if let Ok(mut guard) = state.lock() {
                guard.review_page_mut().cancel_sampling();
            }
            send_command(
                &ui,
                &state,
                &metrics,
                &commands,
                strings,
                Command::SamplingScope {
                    scope: scope.to_string(),
                },
            );
        });
    }
    {
        let weak = ui.clone();
        let state = Arc::clone(state);
        let metrics = Arc::clone(metrics);
        let commands = command_bus.clone();
        handle.on_rules_handwritten(move |criterion, points| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let raw_text = state.lock().map_or_else(
                |_| String::new(),
                |guard| guard.rules_page().raw_text().to_owned(),
            );
            send_command(
                &ui,
                &state,
                &metrics,
                &commands,
                strings,
                Command::RulesHandwritten {
                    raw_text,
                    criterion: criterion.to_string(),
                    points: points.to_string(),
                },
            );
        });
    }
    {
        let weak = ui.clone();
        let state = Arc::clone(state);
        let metrics = Arc::clone(metrics);
        let commands = command_bus.clone();
        handle.on_consent_register(move |task, backend, scope, budget, ttl| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            send_command(
                &ui,
                &state,
                &metrics,
                &commands,
                strings,
                Command::ConsentRegister {
                    task_id: task.to_string(),
                    backend: backend.to_string(),
                    scope: scope.to_string(),
                    budget: budget.to_string(),
                    ttl_hours: ttl.to_string(),
                },
            );
        });
    }

    // 顶栏入口。
    {
        let weak = ui.clone();
        let state = Arc::clone(state);
        let metrics = Arc::clone(metrics);
        handle.on_open_settings_clicked(move || {
            if let Some(ui) = weak.upgrade() {
                let _ = ui_bridge::navigate(&ui, &state, &metrics, PageId::Settings.index());
            }
        });
    }
    {
        let weak = ui.clone();
        let state = Arc::clone(state);
        let metrics = Arc::clone(metrics);
        handle.on_open_logs_clicked(move || {
            if let Some(ui) = weak.upgrade() {
                let _ = ui_bridge::navigate(&ui, &state, &metrics, PageId::Diagnostics.index());
            }
        });
    }

    // 错误横幅操作。
    {
        let weak = ui.clone();
        let state = Arc::clone(state);
        handle.on_toggle_error_detail(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            toggle_error_detail(&ui, &state, strings);
        });
    }
    {
        let weak = ui.clone();
        let state = Arc::clone(state);
        let metrics = Arc::clone(metrics);
        let commands = command_bus.clone();
        handle.on_retry_error(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            send_command(
                &ui,
                &state,
                &metrics,
                &commands,
                strings,
                Command::RetryError,
            );
        });
    }
    {
        let weak = ui.clone();
        let state = Arc::clone(state);
        handle.on_copy_diagnostic_id(move || {
            if let Some(ui) = weak.upgrade() {
                push_notice(&ui, &state, strings, strings.copy_placeholder);
            }
        });
    }
    {
        let weak = ui.clone();
        let state = Arc::clone(state);
        handle.on_dismiss_toast(move |serial| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            dismiss_toast(&ui, &state, strings, serial);
        });
    }

    handle.on_quit(move || {
        let _ = slint::quit_event_loop();
    });
}

fn view_and_apply(ui: &AppWindow, state: &Arc<Mutex<AppState>>, strings: &'static Strings) {
    ui_bridge::apply_state(ui, state, strings);
}

/// 切页后请求后台加载本页数据（一条命令）。
fn load_page(
    ui: &AppWindow,
    state: &Arc<Mutex<AppState>>,
    metrics: &Arc<UiMetrics>,
    commands: &CommandBus,
    page: PageId,
) {
    send_command(
        ui,
        state,
        metrics,
        commands,
        crate::i18n::strings(),
        Command::PageAction {
            id: format!("page.load.{}", page.code()),
        },
    );
}

/// 复核页键盘动作分派（纯映射在 `pages::review::map_key`）。
fn dispatch_review_key(
    ui: &AppWindow,
    state: &Arc<Mutex<AppState>>,
    metrics: &Arc<UiMetrics>,
    commands: &CommandBus,
    key: crate::pages::review::ReviewKey,
) {
    use crate::pages::review::ReviewKey;
    let strings = crate::i18n::strings();
    match key {
        ReviewKey::Next
        | ReviewKey::Prev
        | ReviewKey::ToggleEvidence
        | ReviewKey::Digit(_)
        | ReviewKey::Escape => {
            let (selected, view) = {
                let Ok(mut guard) = state.lock() else {
                    return;
                };
                guard.review_page_mut().on_key(key);
                let selected = guard.review_page_mut().selected();
                (selected, guard.view(strings))
            };
            ui_bridge::apply_view(ui, &view, strings);
            send_command(
                ui,
                state,
                metrics.as_ref(),
                commands,
                strings,
                Command::ReviewSelect { index: selected },
            );
        }
        ReviewKey::Accept => {
            send_command(
                ui,
                state,
                metrics.as_ref(),
                commands,
                strings,
                Command::ReviewCommand {
                    kind: "accept".to_owned(),
                    score: None,
                    reason: None,
                    text: None,
                },
            );
        }
        ReviewKey::Undo => {
            send_command(
                ui,
                state,
                metrics.as_ref(),
                commands,
                strings,
                Command::ReviewCommand {
                    kind: "undo".to_owned(),
                    score: None,
                    reason: None,
                    text: None,
                },
            );
        }
        ReviewKey::FocusScore | ReviewKey::Exclude => {
            let view = {
                let Ok(guard) = state.lock() else {
                    return;
                };
                guard.view(strings)
            };
            ui_bridge::apply_view(ui, &view, strings);
        }
    }
}

/// 页面动作 id → 命令（多个动作按顺序发送；未知 id 忽略并提示）。
fn page_action_commands(state: &Arc<Mutex<AppState>>, id: &str) -> Vec<Command> {
    let Ok(mut guard) = state.lock() else {
        return Vec::new();
    };
    match id {
        "import.pick" => vec![Command::ImportPick { folder: false }],
        "import.pick_folder" => vec![Command::ImportPick { folder: true }],
        "progress.refresh" => vec![Command::PageAction {
            id: "page.load.progress".to_owned(),
        }],
        "progress.cancel_all" => guard
            .progress_page()
            .view()
            .rows
            .into_iter()
            .filter(|row| row.cancelable)
            .map(|row| Command::TaskCancel { task_id: row.id })
            .collect(),
        "settings.restore_backup" => vec![Command::SettingsRestoreBackup],
        "settings.open_dir" => vec![Command::OpenDataDir { logs: false }],
        "review.accept" => vec![Command::ReviewCommand {
            kind: "accept".to_owned(),
            score: None,
            reason: None,
            text: None,
        }],
        "review.override" => vec![Command::ReviewCommand {
            kind: "override".to_owned(),
            score: Some(guard.review_page_mut().score_input().to_owned()),
            reason: Some(guard.review_page_mut().reason_selected()),
            text: None,
        }],
        "review.regrade_text" => vec![Command::ReviewCommand {
            kind: "regrade_text".to_owned(),
            score: None,
            reason: None,
            text: None,
        }],
        "review.batch_accept" => vec![Command::ReviewCommand {
            kind: "batch_accept".to_owned(),
            score: None,
            reason: None,
            text: None,
        }],
        "review.undo" => vec![Command::ReviewCommand {
            kind: "undo".to_owned(),
            score: None,
            reason: None,
            text: None,
        }],
        "review.sampling_pass" => vec![Command::ReviewCommand {
            kind: "sampling_pass".to_owned(),
            score: None,
            reason: None,
            text: None,
        }],
        "rules.structure" => vec![
            Command::RulesRaw {
                text: guard.rules_page().raw_text().to_owned(),
            },
            Command::RulesStructure,
        ],
        "rules.confirm" => vec![Command::RulesConfirm],
        "rules.reject" => vec![Command::RulesReject {
            reason: "界面拒绝（请在后续版本填写理由）".to_owned(),
        }],
        "rules.reset" => vec![Command::RulesRaw {
            text: guard.rules_page().raw_text().to_owned(),
        }],
        "consent.refresh" => vec![Command::ConsentLoad],
        "consent.register" => {
            let view = guard.consent_page_mut().view();
            vec![Command::ConsentRegister {
                task_id: view.task_id,
                backend: view.backend,
                scope: view.scope,
                budget: view.budget,
                ttl_hours: view.ttl_hours,
            }]
        }
        "diagnostics.search" => {
            let filter = guard.diagnostics_page().filter();
            vec![Command::LogQuery {
                level: filter.level.clone(),
                module: filter.module.clone(),
                keyword: filter.keyword.clone(),
                trace_id: filter.trace_id.clone(),
                page: 1,
            }]
        }
        "report.export" => vec![Command::ExportReport],
        "diagnostics.export" => vec![Command::DiagnosticsExport],
        "diagnostics.open_dir" => vec![Command::OpenDataDir { logs: true }],
        _ => Vec::new(),
    }
}

fn dismiss_top(ui: &AppWindow, state: &Arc<Mutex<AppState>>, strings: &'static Strings) -> bool {
    let Ok(mut guard) = state.lock() else {
        return false;
    };
    let handled = guard.dismiss_top();
    let view = guard.view(strings);
    drop(guard);
    ui_bridge::apply_view(ui, &view, strings);
    handled
}

fn request_mode_toggle(ui: &AppWindow, state: &Arc<Mutex<AppState>>, strings: &'static Strings) {
    let view = {
        let Ok(mut guard) = state.lock() else {
            return;
        };
        let _ = guard.request_mode_toggle();
        guard.view(strings)
    };
    ui_bridge::apply_view(ui, &view, strings);
}

fn confirm_yes(
    ui: &AppWindow,
    state: &Arc<Mutex<AppState>>,
    metrics: &UiMetrics,
    commands: &CommandBus,
    strings: &'static Strings,
) {
    let mode_target = {
        let Ok(mut guard) = state.lock() else {
            return;
        };
        let kind = guard.confirm().map(|dialog| dialog.kind.clone());
        match kind {
            Some(ConfirmKind::EnterFast | ConfirmKind::ExitFast) => guard.confirm_mode_switch(),
            Some(ConfirmKind::LeavePage { .. }) => {
                let _ = guard.confirm_leave();
                None
            }
            None => None,
        }
    };
    if let Some(to_fast) = mode_target {
        send_command(
            ui,
            state,
            metrics,
            commands,
            strings,
            Command::ConfirmModeSwitch { to_fast },
        );
    } else {
        view_and_apply(ui, state, strings);
    }
}

fn cancel_confirm(ui: &AppWindow, state: &Arc<Mutex<AppState>>, strings: &'static Strings) {
    let view = {
        let Ok(mut guard) = state.lock() else {
            return;
        };
        guard.cancel_confirm();
        guard.view(strings)
    };
    ui_bridge::apply_view(ui, &view, strings);
}

fn toggle_error_detail(ui: &AppWindow, state: &Arc<Mutex<AppState>>, strings: &'static Strings) {
    let view = {
        let Ok(mut guard) = state.lock() else {
            return;
        };
        guard.toggle_error_details();
        guard.view(strings)
    };
    ui_bridge::apply_view(ui, &view, strings);
}

fn dismiss_toast(
    ui: &AppWindow,
    state: &Arc<Mutex<AppState>>,
    strings: &'static Strings,
    serial: i32,
) {
    let view = {
        let Ok(mut guard) = state.lock() else {
            return;
        };
        guard.dismiss_toast(u64::try_from(serial).unwrap_or(u64::MAX));
        guard.view(strings)
    };
    ui_bridge::apply_view(ui, &view, strings);
}

fn push_notice(
    ui: &AppWindow,
    state: &Arc<Mutex<AppState>>,
    strings: &'static Strings,
    body: &str,
) {
    let view = {
        let Ok(mut guard) = state.lock() else {
            return;
        };
        guard.push_toast(
            NoticeLevel::Info,
            strings.notice_info_title.to_owned(),
            body.to_owned(),
        );
        guard.view(strings)
    };
    ui_bridge::apply_view(ui, &view, strings);
}

fn send_command(
    ui: &AppWindow,
    state: &Arc<Mutex<AppState>>,
    metrics: &UiMetrics,
    commands: &CommandBus,
    strings: &'static Strings,
    command: Command,
) {
    let started = Instant::now();
    let result = commands.try_send(command);
    metrics.record_command(started.elapsed());
    if result.is_err() {
        ui_bridge::notify_command_rejected(ui, state, strings);
    }
}

fn run_startup_error(
    message: &str,
    cli: &Cli,
    strings: &'static Strings,
    t0: Instant,
) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("[eg-app] 启动失败：{message}");
    let ui = AppWindow::new()?;
    ui.set_app_title(strings.app_title.into());
    ui.set_error_visible(true);
    ui.set_startup_error(true);
    ui.set_quit_label(strings.quit_label.into());
    ui.set_error_title(strings.startup_error_title.into());
    ui.set_error_summary(message.into());
    ui.set_error_detail(message.into());
    ui.set_error_show_detail(strings.error_show_detail.into());
    ui.set_error_hide_detail(strings.error_hide_detail.into());
    ui.set_error_retry(strings.error_retry.into());
    ui.set_error_copy(strings.error_copy_id.into());
    ui.set_error_diagnostic_id("".into());
    ui.set_error_detail_visible(true);
    ui.set_retry_enabled(false);
    ui.set_startup_error_help(strings.startup_error_help.into());
    ui.set_dismiss_symbol(strings.dismiss_symbol.into());
    ui.set_status_left(strings.status_ready.into());
    ui.set_channel_text("".into());
    ui.set_hint(strings.shortcut_hint.into());
    ui.set_log_placeholder(strings.log_viewer_placeholder.into());
    ui.set_open_settings(strings.open_settings.into());
    ui.set_open_logs(strings.open_logs.into());
    ui.set_nav_entries(ModelRc::new(VecModel::from(Vec::<NavEntry>::new())));
    ui.set_toasts(ModelRc::new(VecModel::from(Vec::<ToastItem>::new())));
    ui.set_page(PageContent {
        title: strings.startup_error_title.into(),
        subtitle: message.into(),
        body: strings.startup_error_help.into(),
        action_label: strings.quit_label.into(),
    });
    ui.on_quit(|| {
        let _ = slint::quit_event_loop();
    });
    ui.show()?;
    if cli.mode != RunMode::App {
        slint::Timer::single_shot(Duration::from_millis(300), move || {
            eprintln!(
                "[smoke] startup_error=1 first_frame_ms={:.3}",
                elapsed_ms(t0)
            );
            let _ = slint::quit_event_loop();
        });
    }
    slint::run_event_loop()?;
    let _ = ui.hide();
    Ok(())
}
