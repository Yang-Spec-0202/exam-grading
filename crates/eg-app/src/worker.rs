//! 后台 worker：独占 `Platform`，消费有界命令通道，经事件总线把结果回投 UI。
//!
//! 线程边界（`SPEC-app.md` §3/§4）：UI 线程永不等待后台；worker 内可以同步 IO，
//! 但只允许通过 [`EventBus`] 与 [`UiDispatcher`](crate::ui_bridge::UiDispatcher) 回投，
//! 不直接触碰 Slint 类型。

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::thread::JoinHandle;

use eg_domain::GradeMode;
use eg_platform::{ErrorClass, NewAuditEntry, Platform, TaskState};

use crate::channel::{Command, CommandBus, Event, EventBus, NoticeLevel};
use crate::i18n::Strings;
use crate::nav::PageId;
use crate::services::{self, RulesBundle};

/// 启动后台 worker 线程（拥有 `Platform`；收到 [`Command::Quit`] 或命令通道断开时关闭平台退出）。
///
/// # Errors
///
/// 线程创建失败时返回 IO 错误。
#[allow(
    clippy::needless_pass_by_value,
    reason = "参数被 move 进新线程；按值传递是所有权要求，非多余拷贝"
)]
pub(crate) fn spawn(
    platform: Platform,
    commands: Receiver<Command>,
    bus: CommandBus,
    events: EventBus,
    strings: &'static Strings,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("eg-app-worker".to_owned())
        .spawn(move || run(platform, commands, bus, events, strings))
}

#[allow(
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    reason = "worker 主循环按命令集中分派；拆分会引入额外状态传递"
)]
fn run(
    mut platform: Platform,
    commands: Receiver<Command>,
    bus: CommandBus,
    events: EventBus,
    strings: &'static Strings,
) {
    let mut review_ids: Vec<eg_domain::GradeId> = Vec::new();
    let mut rules = RulesBundle::new();
    let mut rules_raw = String::new();
    let mut review_index: usize = 0;
    emit_initial(&platform, &events, strings);
    while let Ok(command) = commands.recv() {
        bus.record_receive();
        match command {
            Command::RunPlaceholderJob => run_ping(&events, strings),
            Command::ConfirmModeSwitch { to_fast } => {
                persist_mode(&mut platform, &events, strings, to_fast);
            }
            Command::PrimaryAction { page } => {
                page_load(
                    &platform,
                    &events,
                    &mut review_ids,
                    &mut review_index,
                    page,
                    strings,
                );
            }
            Command::PageAction { id } => {
                if let Some(page) = id.strip_prefix("page.load.").and_then(page_from_code) {
                    page_load(
                        &platform,
                        &events,
                        &mut review_ids,
                        &mut review_index,
                        page,
                        strings,
                    );
                } else {
                    events.send(Event::Notice {
                        level: NoticeLevel::Info,
                        title: strings.notice_placeholder_title.to_owned(),
                        body: format!("动作未接入：{id}"),
                    });
                }
            }
            Command::ImportPick { folder } => match services::pick_paths(folder) {
                Ok(paths) => services::run_import(&platform, paths, &events),
                Err(message) => {
                    events.send(Event::ImportFailed { message });
                }
            },
            Command::ImportDrop { paths } => {
                let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
                services::run_import(&platform, paths, &events);
            }
            Command::TaskRetry { task_id } => match eg_domain::TaskId::parse(&task_id) {
                Ok(id) => {
                    let _ = platform.tasks().retry(id);
                    events.send(Event::QueueTasks {
                        rows: services::load_tasks(&platform),
                    });
                }
                Err(error) => services::notify_error(&events, format!("任务 id 非法：{error}")),
            },
            Command::TaskCancel { task_id } => match eg_domain::TaskId::parse(&task_id) {
                Ok(id) => {
                    let _ = platform.tasks().cancel(id);
                    events.send(Event::QueueTasks {
                        rows: services::load_tasks(&platform),
                    });
                }
                Err(error) => services::notify_error(&events, format!("任务 id 非法：{error}")),
            },
            Command::ReviewSelect { index } => {
                review_index = index;
                if let Some(grade_id) = review_ids.get(index) {
                    if let Some(event) = services::review_detail(&platform, *grade_id) {
                        events.send(event);
                    }
                    if let Some(image) = services::review_image_event(&platform, *grade_id) {
                        events.send(image);
                    }
                    emit_review_history(&platform, &events);
                }
            }
            Command::ReviewCommand {
                kind,
                score,
                reason,
                text,
            } => {
                if let Some(grade_id) = review_ids.get(review_index).copied() {
                    let event = services::review_execute(
                        &platform,
                        grade_id,
                        &kind,
                        score.as_deref(),
                        reason,
                        text.as_deref(),
                    );
                    events.send(event);
                } else {
                    events.send(Event::ReviewNotice {
                        message: "没有选中的题目".to_owned(),
                    });
                }
                let (ids, rows, counts) = services::review_queue(&platform);
                review_ids = ids;
                events.send(Event::ReviewLoaded { rows, counts });
                if let Some(grade_id) = review_ids.get(review_index) {
                    if let Some(event) = services::review_detail(&platform, *grade_id) {
                        events.send(event);
                    }
                    if let Some(image) = services::review_image_event(&platform, *grade_id) {
                        events.send(image);
                    }
                    emit_review_history(&platform, &events);
                }
            }
            Command::SamplingScope { scope } => {
                let message = if scope == "paper" {
                    "整卷重跑已登记：新判分修订版本将在判分服务接入后生成".to_owned()
                } else {
                    "已选择仅本题重跑".to_owned()
                };
                events.send(Event::ReviewNotice { message });
            }
            Command::RulesRaw { text } => {
                rules_raw = text;
            }
            Command::RulesStructure => {
                if rules_raw.trim().is_empty() {
                    events.send(Event::RulesFailed {
                        message: "没有规则原文：请先填写原文再整理".to_owned(),
                        model_call_id: String::new(),
                    });
                } else {
                    events.send(rules.structured(&rules_raw));
                }
            }
            Command::RulesHandwritten {
                raw_text,
                criterion,
                points,
            } => events.send(rules.handwritten(&raw_text, &criterion, &points)),
            Command::RulesConfirm => events.send(rules.confirm()),
            Command::RulesReject { reason } => events.send(rules.reject(&reason)),
            Command::ConsentLoad => events.send(Event::ConsentRows {
                rows: services::load_consents(&platform),
            }),
            Command::ConsentRegister {
                task_id,
                backend,
                scope,
                budget,
                ttl_hours,
            } => {
                let _ = ttl_hours;
                match services::register_consent(&platform, &task_id, &backend, &scope, &budget) {
                    Ok(consent_id) => {
                        events.send(Event::ConsentRegistered { consent_id });
                        events.send(Event::ConsentRows {
                            rows: services::load_consents(&platform),
                        });
                    }
                    Err(message) => events.send(Event::ConsentFailed { message }),
                }
            }
            Command::ConsentRevoke { consent_id } => {
                match services::revoke_consent(&platform, &consent_id) {
                    Ok(consent_id) => {
                        events.send(Event::ConsentRevoked { consent_id });
                        events.send(Event::ConsentRows {
                            rows: services::load_consents(&platform),
                        });
                    }
                    Err(message) => events.send(Event::ConsentFailed { message }),
                }
            }
            Command::SettingsLoad => events.send(Event::SettingsLoaded {
                toml_text: services::settings_toml(&platform),
            }),
            Command::SettingsPatch { key, raw } => {
                match services::save_setting(&mut platform, &key, &raw) {
                    Ok(()) => events.send(Event::SettingsSaved {
                        toml_text: services::settings_toml(&platform),
                    }),
                    Err(message) => events.send(Event::SettingsRejected { key, message }),
                }
            }
            Command::SettingsRestoreBackup => match services::restore_config(&mut platform) {
                Ok(toml_text) => events.send(Event::SettingsSaved { toml_text }),
                Err(message) => events.send(Event::SettingsRejected {
                    key: "config".to_owned(),
                    message,
                }),
            },
            Command::LogQuery {
                level,
                module,
                keyword,
                trace_id,
                page,
            } => {
                let (rows, total, index_note) =
                    services::load_logs(&platform, &level, &module, &keyword, &trace_id, page);
                events.send(Event::LogRows {
                    rows,
                    total,
                    page,
                    index_note,
                });
            }
            Command::DiagnosticsExport => {
                events.send(Event::DiagnosticsExported {
                    summary: services::export_diagnostics(&platform),
                });
            }
            Command::OpenDataDir { logs } => {
                let dir = if logs {
                    platform.dirs().log_dir().to_path_buf()
                } else {
                    platform.data_root()
                };
                let _ = std::process::Command::new("/usr/bin/open").arg(dir).spawn();
            }
            Command::RetryError => {
                events.send(Event::Notice {
                    level: NoticeLevel::Info,
                    title: strings.notice_info_title.to_owned(),
                    body: strings.retry_placeholder.to_owned(),
                });
            }
            Command::LogStartup {
                platform_open_ms,
                ui_new_ms,
                ui_show_ms,
                first_frame_ms,
            } => log_startup(
                &platform,
                platform_open_ms,
                ui_new_ms,
                ui_show_ms,
                first_frame_ms,
            ),
            Command::ExportReport => match services::export_report(&platform) {
                Ok(summary) => events.send(Event::ReportExported { summary }),
                Err(error) => services::notify_error(&events, error),
            },
            Command::RunSmokeChain => {
                let (summary, exported, import_view) = services::run_smoke_chain(&platform);
                eprintln!("[smoke-chain] {summary}");
                if let Some(report) = import_view {
                    events.send(Event::ImportFinished { report });
                }
                if let Some(exported) = exported {
                    events.send(Event::ReportExported { summary: exported });
                }
                let (ids, rows, counts) = services::review_queue(&platform);
                review_ids = ids;
                events.send(Event::ReviewLoaded { rows, counts });
                events.send(Event::QueueTasks {
                    rows: services::load_tasks(&platform),
                });
                if let Some(grade_id) = review_ids.first()
                    && let Some(event) = services::review_detail(&platform, *grade_id)
                {
                    events.send(event);
                }
                events.send(Event::Notice {
                    level: NoticeLevel::Info,
                    title: "冒烟链路".to_owned(),
                    body: summary,
                });
            }
            Command::Quit => break,
        }
    }
    let _ = platform.close();
}

/// 回投撤销/重做持久栈长度（UI 按钮可用性；重启后由 platform 恢复）。
fn emit_review_history(platform: &Platform, events: &EventBus) {
    let (undo, redo) = services::review_history(platform);
    events.send(Event::ReviewHistory {
        undo_len: u32::try_from(undo).unwrap_or(u32::MAX),
        redo_len: u32::try_from(redo).unwrap_or(u32::MAX),
    });
}

fn page_from_code(code: &str) -> Option<PageId> {
    PageId::ALL.into_iter().find(|page| page.code() == code)
}

fn page_load(
    platform: &Platform,
    events: &EventBus,
    review_ids: &mut Vec<eg_domain::GradeId>,
    review_index: &mut usize,
    page: PageId,
    strings: &'static Strings,
) {
    match page {
        PageId::Import => events.send(Event::Notice {
            level: NoticeLevel::Info,
            title: strings.page(PageId::Import).title.to_owned(),
            body: strings.import_hint.to_owned(),
        }),
        PageId::Progress => events.send(Event::QueueTasks {
            rows: services::load_tasks(platform),
        }),
        PageId::Settings => events.send(Event::SettingsLoaded {
            toml_text: services::settings_toml(platform),
        }),
        PageId::Review => {
            *review_index = 0;
            let (ids, rows, counts) = services::review_queue(platform);
            *review_ids = ids;
            events.send(Event::ReviewLoaded { rows, counts });
            if let Some(grade_id) = review_ids.first() {
                if let Some(event) = services::review_detail(platform, *grade_id) {
                    events.send(event);
                }
                if let Some(image) = services::review_image_event(platform, *grade_id) {
                    events.send(image);
                }
                emit_review_history(platform, events);
            }
        }
        PageId::Consent => events.send(Event::ConsentRows {
            rows: services::load_consents(platform),
        }),
        PageId::Diagnostics => {
            let (rows, total, index_note) = services::load_logs(platform, "", "", "", "", 1);
            events.send(Event::LogRows {
                rows,
                total,
                page: 1,
                index_note,
            });
        }
        PageId::Home | PageId::Papers | PageId::Rules | PageId::Report => {}
    }
}

fn emit_initial(platform: &Platform, events: &EventBus, strings: &'static Strings) {
    let counts = platform.tasks().count_by_state().unwrap_or_default();
    let running = u32::try_from(*counts.get(&TaskState::Running).unwrap_or(&0)).unwrap_or(0);
    let pending = u32::try_from(*counts.get(&TaskState::Pending).unwrap_or(&0)).unwrap_or(0);
    events.send(Event::QueueStats { running, pending });
    // T35 占位：推理模块未接入前如实显示「未接入」，不伪造「就绪」。
    events.send(Event::BackendHealth {
        ready: false,
        message: strings.backend_note.to_owned(),
    });
    let home = strings.page(PageId::Home);
    events.send(Event::PageSnapshot {
        page: PageId::Home,
        title: home.title.to_owned(),
        subtitle: home.subtitle.to_owned(),
        body: home.body.to_owned(),
    });
}

fn run_ping(events: &EventBus, strings: &'static Strings) {
    for step in 1..=10u32 {
        events.send(Event::Progress {
            task_id: "eg-app-placeholder".to_owned(),
            label: strings.worker_label.to_owned(),
            done: step,
            total: 10,
        });
        std::thread::sleep(std::time::Duration::from_millis(80));
    }
    events.send(Event::Notice {
        level: NoticeLevel::Info,
        title: strings.notice_worker_title.to_owned(),
        body: strings.worker_done_body.to_owned(),
    });
}

fn persist_mode(
    platform: &mut Platform,
    events: &EventBus,
    strings: &'static Strings,
    to_fast: bool,
) {
    let target = if to_fast {
        GradeMode::Fast
    } else {
        GradeMode::Standard
    };
    let mut document = platform.config_document().clone();
    if document.config.general.mode == target {
        events.send(Event::ModeChanged { fast: to_fast });
        return;
    }
    document.config.general.mode = target;
    let save_result = platform.save_config(document);
    if let Err(error) = save_result {
        emit_failure(platform, events, strings, "模式切换失败", &error);
        return;
    }
    let entry = NewAuditEntry::new("mode.switch", "config", "general.mode")
        .with_note(format!("切换模式：{}", strings.mode_label(to_fast)));
    match platform.audit().append(entry) {
        Ok(_) => {
            events.send(Event::ModeChanged { fast: to_fast });
        }
        Err(error) => emit_failure(platform, events, strings, "模式切换审计失败", &error),
    }
}

fn emit_failure(
    platform: &Platform,
    events: &EventBus,
    strings: &'static Strings,
    summary: &str,
    error: &eg_platform::PlatformError,
) {
    let diagnostic_id = platform
        .ids()
        .next_ulid()
        .map_or_else(|_| "unknown".to_owned(), |ulid| ulid.to_string());
    let _ = error.class();
    events.send(Event::TaskFailed {
        task_id: "eg-app".to_owned(),
        message: format!("{summary}：{error}"),
        retryable: matches!(error.class(), ErrorClass::Transient),
        diagnostic_id,
    });
    events.send(Event::Notice {
        level: NoticeLevel::Error,
        title: strings.error_title.to_owned(),
        body: format!("{summary}（{}）", strings.error_retry),
    });
}

fn log_startup(
    platform: &Platform,
    platform_open_ms: u64,
    ui_new_ms: u64,
    ui_show_ms: u64,
    first_frame_ms: u64,
) {
    let record = eg_platform::LogRecord::new(
        platform.clock().now(),
        eg_platform::LogLevel::Info,
        "eg_app",
        "app.start",
        format!(
            "外壳启动：platform_open={platform_open_ms}ms ui_new={ui_new_ms}ms \
             show={ui_show_ms}ms first_frame={first_frame_ms}ms"
        ),
    )
    .with_latency_ms(first_frame_ms);
    let _ = platform.log().write(record);
    let _ = platform.log().flush();
}
