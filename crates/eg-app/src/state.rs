//! 全局界面状态与状态迁移（纯逻辑，可单测；不依赖 Slint）。
//!
//! 规范：`SPEC-app.md` §2（页面/模式/导航守卫）、§3（通道计数可见）、§4（页面切换只读
//! 内存快照，不做同步 IO）、§5（错误呈现：中文摘要 + 可展开详情 + 诊断 id + 可行动作）。
//!
//! 本模块只处理「状态怎么变」，不碰窗口/线程；Slint 属性投影见 `ui_bridge`。

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use eg_domain::GradeMode;

use crate::channel::{ChannelStats, Event, NoticeLevel};
use crate::i18n::Strings;
use crate::nav::PageId;
use crate::pages::consent::ConsentPage;
use crate::pages::diagnostics::DiagnosticsPage;
use crate::pages::import::ImportPage;
use crate::pages::progress::ProgressPage;
use crate::pages::review::ReviewPage;
use crate::pages::rules::RulesPage;
use crate::pages::settings::SettingsPage;

/// 提示气泡最多保留条数（超出丢弃最旧，UI 状态栏仍可见计数）。
pub(crate) const MAX_TOASTS: usize = 5;

/// 页面切换决策。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NavDecision {
    /// 已切换（内存快照，无 IO）。
    Switched,
    /// 目标页就是当前页。
    Same,
    /// 当前页有未保存编辑，需要二次确认。
    NeedsConfirm,
}

/// 模式切换请求结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModeRequest {
    /// 已弹出二次确认。
    NeedsConfirm,
    /// 已有运行中任务：拒绝并给原因（`SPEC-app.md` §2 双模式表）。
    BlockedByRunning,
    /// `fast.enabled=false`：快速入口禁用。
    Disabled,
}

/// 二次确认类型（模式切换 / 离开未保存页）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConfirmKind {
    /// 进入快速模式。
    EnterFast,
    /// 退出快速模式。
    ExitFast,
    /// 离开有未保存编辑的页。
    LeavePage {
        /// 目标页。
        target: PageId,
    },
}

/// 二次确认弹窗数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfirmDialog {
    /// 类型。
    pub(crate) kind: ConfirmKind,
    /// 标题。
    pub(crate) title: String,
    /// 正文。
    pub(crate) body: String,
}

/// 后端健康视图。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BackendStatus {
    /// 未接入/未探测（T35 占位）。
    Unknown,
    /// 就绪。
    Ready,
    /// 不可用。
    Down,
}

/// 单条提示气泡。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Toast {
    /// 序号（UI 关闭回调使用）。
    pub(crate) serial: u64,
    /// 级别。
    pub(crate) level: NoticeLevel,
    /// 标题。
    pub(crate) title: String,
    /// 正文。
    pub(crate) body: String,
}

/// 错误横幅（中文摘要 + 可展开详情 + 诊断 id + 可行动作）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UiError {
    /// 摘要。
    pub(crate) summary: String,
    /// 详情（任务 id、错误分类等）。
    pub(crate) detail: String,
    /// 诊断 id（可复制，映射日志/任务）。
    pub(crate) diagnostic_id: String,
    /// 是否可重试。
    pub(crate) retryable: bool,
}

/// 任务进度视图（按任务只留最新）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProgressView {
    /// 进度文案。
    pub(crate) label: String,
    /// 已完成。
    pub(crate) done: u32,
    /// 总数。
    pub(crate) total: u32,
}

/// 导航项视图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NavEntryView {
    /// 机器码。
    pub(crate) id: String,
    /// 标题。
    pub(crate) title: String,
    /// 快捷键显示（`⌘1` 等）。
    pub(crate) shortcut: String,
}

/// 错误横幅视图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ErrorView {
    /// 摘要。
    pub(crate) summary: String,
    /// 详情。
    pub(crate) detail: String,
    /// 诊断 id。
    pub(crate) diagnostic_id: String,
    /// 是否可重试。
    pub(crate) retryable: bool,
}

/// 二次确认视图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfirmView {
    /// 标题。
    pub(crate) title: String,
    /// 正文。
    pub(crate) body: String,
}

/// 界面视图模型（Slint 属性投影的唯一输入）。
#[allow(
    clippy::struct_excessive_bools,
    reason = "视图模型集中承载多个独立布尔状态（快速模式/后端就绪/详情展开/关于区），与 Slint 属性一一对应"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ViewModel {
    /// 导航项。
    pub(crate) nav: Vec<NavEntryView>,
    /// 当前页索引。
    pub(crate) current_index: usize,
    /// 当前页标题。
    pub(crate) page_title: String,
    /// 当前页副标题。
    pub(crate) page_subtitle: String,
    /// 当前页正文（含事件快照覆盖）。
    pub(crate) page_body: String,
    /// 当前页主操作按钮文字。
    pub(crate) page_action: String,
    /// 模式徽标。
    pub(crate) mode_label: String,
    /// 模式切换按钮文字。
    pub(crate) mode_switch_label: String,
    /// 是否快速模式。
    pub(crate) fast_mode: bool,
    /// 快速模式入口是否可用。
    pub(crate) fast_enabled: bool,
    /// 后端徽标文字。
    pub(crate) backend_label: String,
    /// 后端是否就绪。
    pub(crate) backend_ok: bool,
    /// 后端说明。
    pub(crate) backend_note: String,
    /// 错误横幅。
    pub(crate) error: Option<ErrorView>,
    /// 详情是否展开。
    pub(crate) error_detail_visible: bool,
    /// 提示气泡。
    pub(crate) toasts: Vec<Toast>,
    /// 状态栏左文本（就绪/队列等）。
    pub(crate) status_left: String,
    /// 状态栏通道计数文本。
    pub(crate) channel_text: String,
    /// 底部快捷键提示。
    pub(crate) hint: String,
    /// 最新进度文本（无任务时为空）。
    pub(crate) progress_text: String,
    /// 二次确认。
    pub(crate) confirm: Option<ConfirmView>,
    /// 是否显示「关于」区（设置页占位）。
    pub(crate) show_about: bool,
    /// 关于标题。
    pub(crate) about_title: String,
    /// 关于正文。
    pub(crate) about_body: String,
    /// 导入页。
    pub(crate) import: crate::pages::import::ImportVm,
    /// 进度页。
    pub(crate) progress: crate::pages::progress::ProgressVm,
    /// 设置页。
    pub(crate) settings: crate::pages::settings::SettingsVm,
    /// 复核页。
    pub(crate) review: crate::pages::review::ReviewVm,
    /// 规则页。
    pub(crate) rules: crate::pages::rules::RulesVm,
    /// 授权页。
    pub(crate) consent: crate::pages::consent::ConsentVm,
    /// 诊断页。
    pub(crate) diagnostics: crate::pages::diagnostics::DiagnosticsVm,
    /// 当前页可执行动作（由页面视图模型汇总）。
    pub(crate) actions: Vec<crate::pages::ActionVm>,
}

/// 全局界面状态。
#[derive(Debug, Clone)]
pub(crate) struct AppState {
    current: PageId,
    mode: GradeMode,
    fast_enabled: bool,
    running_tasks: u32,
    pending_tasks: u32,
    backend: BackendStatus,
    backend_message: String,
    toasts: VecDeque<Toast>,
    next_serial: u64,
    error: Option<UiError>,
    error_detail_visible: bool,
    confirm: Option<ConfirmDialog>,
    unsaved: BTreeSet<PageId>,
    channel: ChannelStats,
    progress: BTreeMap<String, ProgressView>,
    snapshot_bodies: BTreeMap<PageId, String>,
    log_summary: Option<String>,
    status_left: String,
    import: ImportPage,
    progress_page: ProgressPage,
    settings: SettingsPage,
    review: ReviewPage,
    rules: RulesPage,
    consent: ConsentPage,
    diagnostics: DiagnosticsPage,
}

#[allow(
    dead_code,
    reason = "UI 状态查询 API 供测试与 M9 后续任务（T36–T38）使用"
)]
impl AppState {
    /// 以启动配置初始化（模式 / 快速开关 / 队列计数）。
    #[must_use]
    pub(crate) fn new(
        mode: GradeMode,
        fast_enabled: bool,
        running_tasks: u32,
        pending_tasks: u32,
    ) -> Self {
        Self {
            current: PageId::Home,
            mode,
            fast_enabled,
            running_tasks,
            pending_tasks,
            backend: BackendStatus::Unknown,
            backend_message: String::new(),
            toasts: VecDeque::new(),
            next_serial: 1,
            error: None,
            error_detail_visible: false,
            confirm: None,
            unsaved: BTreeSet::new(),
            channel: ChannelStats::default(),
            progress: BTreeMap::new(),
            snapshot_bodies: BTreeMap::new(),
            log_summary: None,
            status_left: String::new(),
            import: ImportPage::default(),
            progress_page: ProgressPage::default(),
            settings: SettingsPage::new(toml::Value::Table(toml::map::Map::new())),
            review: ReviewPage::default(),
            rules: RulesPage::default(),
            consent: ConsentPage::default(),
            diagnostics: DiagnosticsPage::default(),
        }
    }

    /// 当前页。
    #[must_use]
    pub(crate) fn current(&self) -> PageId {
        self.current
    }

    /// 当前模式。
    #[must_use]
    pub(crate) fn mode(&self) -> GradeMode {
        self.mode
    }

    /// 快速模式入口是否可用。
    #[must_use]
    pub(crate) fn fast_enabled(&self) -> bool {
        self.fast_enabled
    }

    /// 运行中任务数。
    #[must_use]
    pub(crate) fn running_tasks(&self) -> u32 {
        self.running_tasks
    }

    /// 错误横幅（测试/投影用）。
    #[must_use]
    pub(crate) fn error(&self) -> Option<&UiError> {
        self.error.as_ref()
    }

    /// 错误详情是否展开。
    #[must_use]
    pub(crate) fn error_detail_visible(&self) -> bool {
        self.error_detail_visible
    }

    /// 待确认弹窗。
    #[must_use]
    pub(crate) fn confirm(&self) -> Option<&ConfirmDialog> {
        self.confirm.as_ref()
    }

    /// 通道计数快照。
    #[must_use]
    pub(crate) fn channel_stats(&self) -> ChannelStats {
        self.channel
    }

    /// 更新通道计数（UI 每次事件循环轮询；不计入「静默」）。
    pub(crate) fn update_channel_stats(&mut self, stats: ChannelStats) {
        self.channel = stats;
    }

    /// 更新队列计数（启动与 `QueueStats` 事件）。
    pub(crate) fn set_queue(&mut self, running: u32, pending: u32) {
        self.running_tasks = running;
        self.pending_tasks = pending;
    }

    /// 标记某页有未保存编辑（T36/T37 真实编辑态接入前的守卫用）。
    pub(crate) fn mark_unsaved(&mut self, page: PageId) {
        self.unsaved.insert(page);
    }

    /// 清除未保存标记。
    pub(crate) fn clear_unsaved(&mut self, page: PageId) {
        self.unsaved.remove(&page);
    }

    /// 页面切换（只改内存状态；不触发 IO、不等后台线程）。
    pub(crate) fn navigate_to(&mut self, target: PageId) -> NavDecision {
        if target == self.current {
            return NavDecision::Same;
        }
        if self.unsaved.contains(&self.current) {
            let strings = crate::i18n::strings();
            self.confirm = Some(ConfirmDialog {
                kind: ConfirmKind::LeavePage { target },
                title: strings.confirm_leave_title.to_owned(),
                body: strings.confirm_leave_body.to_owned(),
            });
            return NavDecision::NeedsConfirm;
        }
        self.current = target;
        self.status_left.clear();
        NavDecision::Switched
    }

    /// 直接切页（供启动/快捷键路径；仍走未保存守卫逻辑）。
    pub(crate) fn navigate_shortcut(&mut self, target: PageId) -> NavDecision {
        self.navigate_to(target)
    }

    /// 确认离开未保存页（丢弃未保存编辑并切页）。
    pub(crate) fn confirm_leave(&mut self) -> Option<PageId> {
        let Some(ConfirmDialog {
            kind: ConfirmKind::LeavePage { target },
            ..
        }) = self.confirm.clone()
        else {
            return None;
        };
        self.unsaved.remove(&self.current);
        self.current = target;
        self.confirm = None;
        self.status_left.clear();
        Some(target)
    }

    /// 取消二次确认。
    pub(crate) fn cancel_confirm(&mut self) {
        self.confirm = None;
    }

    /// 请求切换模式（标准 ↔ 快速），按规范先做守卫再弹二次确认。
    pub(crate) fn request_mode_toggle(&mut self) -> ModeRequest {
        let target_fast = self.mode == GradeMode::Standard;
        if target_fast && !self.fast_enabled {
            self.push_toast(
                NoticeLevel::Warn,
                crate::i18n::strings().notice_mode_title.to_owned(),
                crate::i18n::strings().mode_blocked_running.to_owned(),
            );
            return ModeRequest::Disabled;
        }
        if self.running_tasks > 0 {
            self.push_toast(
                NoticeLevel::Warn,
                crate::i18n::strings().notice_mode_title.to_owned(),
                crate::i18n::strings().mode_blocked_running.to_owned(),
            );
            return ModeRequest::BlockedByRunning;
        }
        let strings = crate::i18n::strings();
        let (title, body) = strings.mode_confirm(target_fast);
        self.confirm = Some(ConfirmDialog {
            kind: if target_fast {
                ConfirmKind::EnterFast
            } else {
                ConfirmKind::ExitFast
            },
            title: title.to_owned(),
            body: body.to_owned(),
        });
        ModeRequest::NeedsConfirm
    }

    /// 确认模式切换：返回目标是否为快速模式（由组合根写配置 + 审计后回投 `ModeChanged`）。
    pub(crate) fn confirm_mode_switch(&mut self) -> Option<bool> {
        let target_fast = match self.confirm.as_ref()?.kind {
            ConfirmKind::EnterFast => true,
            ConfirmKind::ExitFast => false,
            ConfirmKind::LeavePage { .. } => return None,
        };
        self.confirm = None;
        Some(target_fast)
    }

    /// 应用后台事件（全部在 UI 线程执行；单条工作应远小于 16ms）。
    pub(crate) fn apply_event(&mut self, event: Event) {
        let strings = crate::i18n::strings();
        match event {
            Event::Progress {
                task_id,
                label,
                done,
                total,
            } => {
                self.progress
                    .insert(task_id, ProgressView { label, done, total });
            }
            Event::TaskFailed {
                task_id,
                message,
                retryable,
                diagnostic_id,
            } => {
                let error = UiError {
                    summary: message.clone(),
                    detail: format!("任务 {task_id} 失败；诊断 id：{diagnostic_id}"),
                    diagnostic_id: diagnostic_id.clone(),
                    retryable,
                };
                self.error = Some(error);
                self.error_detail_visible = false;
                self.push_toast(NoticeLevel::Error, strings.error_title.to_owned(), message);
            }
            Event::BackendHealth { ready, message } => {
                self.backend = if ready {
                    BackendStatus::Ready
                } else {
                    BackendStatus::Down
                };
                self.backend_message = message;
            }
            Event::QueueStats { running, pending } => self.set_queue(running, pending),
            Event::ModeChanged { fast } => {
                self.mode = if fast {
                    GradeMode::Fast
                } else {
                    GradeMode::Standard
                };
                self.confirm = None;
                self.push_toast(
                    NoticeLevel::Info,
                    strings.mode_label(fast).to_owned(),
                    strings.mode_switch_label(!fast).to_owned(),
                );
            }
            Event::Notice { level, title, body } => {
                if level == NoticeLevel::Error {
                    self.error = Some(UiError {
                        summary: title.clone(),
                        detail: body.clone(),
                        diagnostic_id: String::new(),
                        retryable: false,
                    });
                }
                self.push_toast(level, title, body);
            }
            Event::PageSnapshot {
                page,
                title: _,
                subtitle: _,
                body,
            } => {
                self.snapshot_bodies.insert(page, body);
            }
            Event::ImportProgress { done, total } => self.import.progress(done, total),
            Event::ImportFinished { report } => self.import.finish(report),
            Event::ImportFailed { message } => self.import.fail(message),
            Event::QueueTasks { rows } => self.progress_page.set_tasks(rows),
            Event::SettingsLoaded { toml_text } | Event::SettingsSaved { toml_text } => {
                match toml::from_str::<toml::Value>(&toml_text) {
                    Ok(document) => self.settings.reload(document),
                    Err(error) => {
                        let message = format!("配置快照解析失败：{error}");
                        self.settings.reject(message.clone());
                        self.push_toast(
                            NoticeLevel::Error,
                            strings.error_title.to_owned(),
                            message,
                        );
                    }
                }
            }
            Event::SettingsRejected { key, message } => {
                self.settings.reject(format!("{key}：{message}"));
                self.push_toast(
                    NoticeLevel::Warn,
                    strings.notice_info_title.to_owned(),
                    format!("{key}：{message}"),
                );
            }
            Event::ReviewLoaded { rows, counts } => {
                self.review.set_queue(rows);
                self.review.set_counts(counts);
            }
            Event::ReviewDetail {
                meta,
                answer,
                reference,
                judgement,
                score_text,
                reasons,
                evidence,
                read_only,
            } => self.review.set_detail(
                meta, answer, reference, judgement, score_text, reasons, evidence, read_only,
            ),
            Event::ReviewApplied { notice, advance } => self.review.applied(notice, advance),
            Event::ReviewUndone { notice } => self.review.undone(notice),
            Event::ReviewSamplingPrompt { message } => self.review.show_sampling(message),
            Event::ReviewNotice { message } => self.review.notice(message),
            Event::RulesDraft {
                draft_id,
                model_call_id,
                items,
                findings,
            } => self
                .rules
                .draft_ready(draft_id, model_call_id, items, findings),
            Event::RulesFailed {
                message,
                model_call_id,
            } => self.rules.structuring_failed(message, model_call_id),
            Event::RulesConfirmed { version } => self.rules.confirmed(version),
            Event::RulesRejected { reason } => self.rules.rejected(reason),
            Event::ConsentRows { rows } => self.consent.set_rows(rows),
            Event::ConsentRegistered { consent_id } => self.consent.registered(&consent_id),
            Event::ConsentRevoked { consent_id } => self.consent.revoked(&consent_id),
            Event::ConsentFailed { message } => self.consent.fail(message),
            Event::LogRows {
                rows,
                total,
                page,
                index_note,
            } => {
                let _ = page;
                self.diagnostics.apply_page(rows, total, index_note);
            }
            Event::DiagnosticsExported { summary } => {
                self.diagnostics.set_export(summary.clone());
                self.push_toast(
                    NoticeLevel::Info,
                    strings.notice_info_title.to_owned(),
                    summary,
                );
            }
            Event::LogPage { page, total, lines } => {
                self.log_summary = Some(format!("日志第 {page} 页：{lines}/{total} 条"));
            }
        }
    }

    /// 关闭错误横幅。
    pub(crate) fn dismiss_error(&mut self) {
        self.error = None;
        self.error_detail_visible = false;
    }

    /// 展开/收起错误详情。
    pub(crate) fn toggle_error_details(&mut self) {
        self.error_detail_visible = !self.error_detail_visible;
    }

    /// 退出确认弹窗（Esc）。
    pub(crate) fn dismiss_top(&mut self) -> bool {
        if self.confirm.take().is_some() {
            return true;
        }
        if self.error.take().is_some() {
            self.error_detail_visible = false;
            return true;
        }
        false
    }

    /// 关闭一条提示气泡。
    pub(crate) fn dismiss_toast(&mut self, serial: u64) {
        self.toasts.retain(|toast| toast.serial != serial);
    }

    /// 追加提示气泡（有界）。
    pub(crate) fn push_toast(&mut self, level: NoticeLevel, title: String, body: String) {
        let toast = Toast {
            serial: self.next_serial,
            level,
            title,
            body,
        };
        self.next_serial = self.next_serial.saturating_add(1);
        self.toasts.push_back(toast);
        while self.toasts.len() > MAX_TOASTS {
            self.toasts.pop_front();
        }
    }

    /// 生成 Slint 投影（纯函数，无 IO）。
    #[must_use]
    pub(crate) fn view(&self, strings: &Strings) -> ViewModel {
        let page = strings.page(self.current);
        let fallback = self
            .snapshot_bodies
            .get(&self.current)
            .cloned()
            .unwrap_or_else(|| page.body.to_owned());
        let page_body = self.page_text(&fallback);
        let backend_label = match self.backend {
            BackendStatus::Unknown => strings.backend_unknown,
            BackendStatus::Ready => strings.backend_ready,
            BackendStatus::Down => strings.backend_down,
        };
        let toasts = self.toasts.iter().cloned().collect();
        let mut import = self.import.view().clone();
        import.visible = self.current == PageId::Import;
        import.actions = self.import.actions();
        let mut progress = self.progress_page.view();
        progress.visible = self.current == PageId::Progress;
        let mut settings = self.settings.view();
        settings.visible = self.current == PageId::Settings;
        let mut review = self.review.view();
        review.visible = self.current == PageId::Review;
        let mut rules = self.rules.view();
        rules.visible = self.current == PageId::Rules;
        let mut consent = self.consent.view();
        consent.visible = self.current == PageId::Consent;
        let mut diagnostics = self.diagnostics.view();
        diagnostics.visible = self.current == PageId::Diagnostics;
        let progress_text = self
            .progress
            .values()
            .next_back()
            .map_or_else(String::new, |progress| {
                format!("{} {}/{}", progress.label, progress.done, progress.total)
            });
        ViewModel {
            nav: PageId::ALL
                .iter()
                .map(|page| NavEntryView {
                    id: page.code().to_owned(),
                    title: strings.page(*page).title.to_owned(),
                    shortcut: format!("\u{2318}{}", page.shortcut_digit()),
                })
                .collect(),
            current_index: self.current.index(),
            page_title: page.title.to_owned(),
            page_subtitle: page.subtitle.to_owned(),
            page_body,
            page_action: page.action.to_owned(),
            mode_label: strings.mode_label(self.mode == GradeMode::Fast).to_owned(),
            mode_switch_label: strings
                .mode_switch_label(self.mode == GradeMode::Fast)
                .to_owned(),
            fast_mode: self.mode == GradeMode::Fast,
            fast_enabled: self.fast_enabled,
            backend_label: backend_label.to_owned(),
            backend_ok: self.backend == BackendStatus::Ready,
            backend_note: self.backend_message.clone(),
            error: self.error.as_ref().map(|error| ErrorView {
                summary: error.summary.clone(),
                detail: error.detail.clone(),
                diagnostic_id: error.diagnostic_id.clone(),
                retryable: error.retryable,
            }),
            error_detail_visible: self.error_detail_visible,
            toasts,
            status_left: if self.status_left.is_empty() {
                Strings::queue_line(self.running_tasks, self.pending_tasks)
            } else {
                self.status_left.clone()
            },
            channel_text: Strings::channel_line(
                self.channel.merged_total,
                self.channel.dropped_total,
                self.channel.critical_dropped_total,
                self.channel.command_rejected_total,
            ),
            hint: strings.shortcut_hint.to_owned(),
            progress_text,
            confirm: self.confirm.as_ref().map(|confirm| ConfirmView {
                title: confirm.title.clone(),
                body: confirm.body.clone(),
            }),
            show_about: self.current == PageId::Settings,
            about_title: strings.about_title.to_owned(),
            about_body: self.about_body(strings),
            import,
            progress,
            settings,
            review,
            rules,
            consent,
            diagnostics,
            actions: self.page_actions(),
        }
    }

    /// 当前页动作列表。
    fn page_actions(&self) -> Vec<crate::pages::ActionVm> {
        match self.current {
            PageId::Import => self.import.actions(),
            PageId::Progress => self.progress_page.view().actions,
            PageId::Settings => self.settings.view().actions,
            PageId::Review => self.review.view().actions,
            PageId::Rules => self.rules.view().actions,
            PageId::Consent => self.consent.view().actions,
            PageId::Diagnostics => self.diagnostics.view().actions,
            PageId::Home | PageId::Papers | PageId::Report => Vec::new(),
        }
    }

    /// 页面正文：真实视图模型内容优先（文本投影；详细交互由动作/快捷键承载）。
    fn page_text(&self, fallback: &str) -> String {
        let strings = crate::i18n::strings();
        match self.current {
            PageId::Import => {
                let view = self.import.view();
                let naming = if view.naming_visible {
                    view.naming_text.as_str()
                } else {
                    ""
                };
                let rows = view
                    .rows
                    .iter()
                    .map(|row| format!("[{}] {} {}", row.status.code(), row.name, row.detail))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!(
                    "{}\n{}\n进度 {}/{}\n成功 {} / 拒收 {} / 失败 {} / 重复 {}\n{}",
                    view.hint,
                    naming,
                    view.done,
                    view.total,
                    view.ok,
                    view.rejected,
                    view.failed,
                    view.duplicates,
                    rows
                )
            }
            PageId::Progress => {
                let view = self.progress_page.view();
                let rows = view
                    .rows
                    .iter()
                    .map(|row| {
                        format!(
                            "{} · {} · {}/{} · {}",
                            row.id, row.state, row.done, row.total, row.error
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("{}\n{}", view.summary, rows)
            }
            PageId::Settings => {
                let view = self.settings.view();
                let groups = view
                    .groups
                    .iter()
                    .map(|group| {
                        let entries = group
                            .entries
                            .iter()
                            .map(|entry| {
                                if entry.read_only {
                                    format!(
                                        "{} = {}（{}：{}）",
                                        entry.key,
                                        entry.value,
                                        strings.settings_read_only,
                                        entry.note
                                    )
                                } else {
                                    format!("{} = {}", entry.key, entry.value)
                                }
                            })
                            .collect::<Vec<_>>()
                            .join("\n  ");
                        format!("[{}] {}\n  {}", group.title, group.tables, entries)
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                format!(
                    "配置分组 {} 组\n{}\n{}",
                    view.groups.len(),
                    view.error,
                    groups
                )
            }
            PageId::Review => {
                let view = self.review.view();
                format!(
                    "{}\n{} {} / {} {}\n{} · {}\n学生答案：{}\n参考答案：{}\n判定：{}\n得分：{}\n{}\n{}\n{}",
                    view.counts,
                    strings.review_queue_total,
                    view.rows.len(),
                    strings.review_batch,
                    view.batch_count,
                    view.item_title,
                    view.meta,
                    view.answer,
                    view.reference,
                    view.judgement,
                    view.score_text,
                    view.export_stale,
                    view.notice,
                    view.evidence
                        .iter()
                        .map(|e| format!("{}: {}", e.label, e.value))
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            }
            PageId::Rules => {
                let view = self.rules.view();
                let items = view
                    .items
                    .iter()
                    .map(|item| format!("- {}（{} 分）", item.criterion, item.points))
                    .collect::<Vec<_>>()
                    .join("\n");
                let findings = view
                    .findings
                    .iter()
                    .map(|finding| {
                        format!(
                            "[{}] {}",
                            if finding.error { "错误" } else { "警告" },
                            finding.message
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                format!(
                    "状态：{}\n{}\n模型调用：{}\n草稿：{}\n版本：{}\n{}\n{}\n{}",
                    view.stage_label,
                    view.raw_text,
                    view.model_call_id,
                    view.draft_id,
                    view.version,
                    items,
                    findings,
                    view.notice
                )
            }
            PageId::Consent => {
                let view = self.consent.view();
                let rows = view
                    .rows
                    .iter()
                    .map(|row| {
                        format!(
                            "{} · {} · {}/{} · {} · {}",
                            row.task_id,
                            row.state_label,
                            row.spent,
                            row.budget,
                            row.remaining,
                            row.scope
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                format!(
                    "{}\n{}\n{}{}",
                    view.empty_hint, rows, view.exhausted_hint, view.notice
                )
            }
            PageId::Diagnostics => {
                let view = self.diagnostics.view();
                let rows = view
                    .rows
                    .iter()
                    .map(|row| {
                        format!(
                            "{} [{}] {} · {}",
                            row.at, row.level, row.module, row.message
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                format!(
                    "{} · 第 {}/{} 页 · 共 {}\n{}\n{}\n{}\n{}",
                    view.filter_text,
                    view.page,
                    view.pages,
                    view.total,
                    view.index_note,
                    view.error,
                    view.export_summary,
                    rows
                )
            }
            _ => fallback.to_owned(),
        }
    }

    fn about_body(&self, strings: &Strings) -> String {
        if let Some(log) = &self.log_summary {
            format!("{}\n{}", strings.about_body, log)
        } else {
            strings.about_body.to_owned()
        }
    }
}

impl AppState {
    /// 导入页状态。
    pub(crate) fn import_page_mut(&mut self) -> &mut ImportPage {
        &mut self.import
    }

    /// 设置页状态。
    pub(crate) fn settings_page(&self) -> &SettingsPage {
        &self.settings
    }

    /// 复核页状态。
    pub(crate) fn review_page_mut(&mut self) -> &mut ReviewPage {
        &mut self.review
    }

    /// 规则页状态。
    pub(crate) fn rules_page_mut(&mut self) -> &mut RulesPage {
        &mut self.rules
    }

    /// 规则页只读快照。
    pub(crate) fn rules_page(&self) -> &RulesPage {
        &self.rules
    }

    /// 授权页状态。
    pub(crate) fn consent_page_mut(&mut self) -> &mut ConsentPage {
        &mut self.consent
    }

    /// 诊断页状态。
    pub(crate) fn diagnostics_page(&self) -> &DiagnosticsPage {
        &self.diagnostics
    }

    /// 进度页状态。
    pub(crate) fn progress_page(&self) -> &ProgressPage {
        &self.progress_page
    }

    /// 当前提示气泡（测试/投影辅助）。
    #[must_use]
    pub(crate) fn toasts(&self) -> Vec<Toast> {
        self.toasts.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::i18n::strings;

    fn standard_state() -> AppState {
        AppState::new(GradeMode::Standard, true, 0, 0)
    }

    #[test]
    fn starts_on_home_in_standard_mode() {
        let state = standard_state();
        assert_eq!(state.current(), PageId::Home);
        assert_eq!(state.mode(), GradeMode::Standard);
        let view = state.view(strings());
        assert_eq!(view.current_index, 0);
        assert_eq!(view.nav.len(), 10);
        assert_eq!(view.page_title, "首页");
        assert_eq!(view.nav[0].shortcut, "⌘1");
        assert_eq!(view.nav[9].shortcut, "⌘0");
    }

    #[test]
    fn navigation_switches_only_memory_and_tracks_current() {
        let mut state = standard_state();
        assert_eq!(state.navigate_to(PageId::Papers), NavDecision::Switched);
        assert_eq!(state.current(), PageId::Papers);
        assert_eq!(state.navigate_to(PageId::Papers), NavDecision::Same);
        assert_eq!(state.view(strings()).page_title, "试卷与答案键");
    }

    #[test]
    fn unsaved_edit_requires_confirmation_before_leave() {
        let mut state = standard_state();
        state.navigate_to(PageId::Review);
        state.mark_unsaved(PageId::Review);
        assert_eq!(state.navigate_to(PageId::Home), NavDecision::NeedsConfirm);
        assert_eq!(state.current(), PageId::Review, "确认前不得离开");
        let confirm = state.confirm().unwrap().clone();
        assert_eq!(
            confirm.kind,
            ConfirmKind::LeavePage {
                target: PageId::Home
            }
        );
        assert_eq!(state.confirm_leave(), Some(PageId::Home));
        assert_eq!(state.current(), PageId::Home);
        assert!(state.confirm().is_none());
    }

    #[test]
    fn cancel_leave_keeps_current_page() {
        let mut state = standard_state();
        state.navigate_to(PageId::Rules);
        state.mark_unsaved(PageId::Rules);
        assert_eq!(state.navigate_to(PageId::Report), NavDecision::NeedsConfirm);
        state.cancel_confirm();
        assert_eq!(state.current(), PageId::Rules);
        assert!(state.confirm().is_none());
    }

    #[test]
    fn mode_toggle_blocked_while_tasks_running() {
        let mut state = AppState::new(GradeMode::Standard, true, 2, 5);
        assert_eq!(state.request_mode_toggle(), ModeRequest::BlockedByRunning);
        assert!(state.confirm().is_none());
        assert_eq!(state.toasts().len(), 1);
        let view = state.view(strings());
        assert_eq!(view.mode_label, "标准模式");
    }

    #[test]
    fn mode_toggle_disabled_when_fast_disabled() {
        let mut state = AppState::new(GradeMode::Standard, false, 0, 0);
        assert_eq!(state.request_mode_toggle(), ModeRequest::Disabled);
    }

    #[test]
    fn mode_toggle_requires_confirmation_then_events_apply() {
        let mut state = standard_state();
        assert_eq!(state.request_mode_toggle(), ModeRequest::NeedsConfirm);
        assert_eq!(state.confirm().unwrap().kind, ConfirmKind::EnterFast);
        assert_eq!(state.confirm_mode_switch(), Some(true));
        state.apply_event(Event::ModeChanged { fast: true });
        assert_eq!(state.mode(), GradeMode::Fast);
        assert_eq!(state.view(strings()).mode_label, "快速模式");
        assert!(state.toasts().iter().any(|t| t.title == "快速模式"));
    }

    #[test]
    fn task_failed_sets_error_banner_with_diagnostic_id() {
        let mut state = standard_state();
        state.apply_event(Event::TaskFailed {
            task_id: "task-1".to_owned(),
            message: "判分失败：模型超时".to_owned(),
            retryable: true,
            diagnostic_id: "diag-01".to_owned(),
        });
        let error = state.error().unwrap();
        assert_eq!(error.summary, "判分失败：模型超时");
        assert!(error.detail.contains("task-1"));
        assert!(error.detail.contains("diag-01"));
        assert!(error.retryable);
        assert!(!state.error_detail_visible());
        state.toggle_error_details();
        assert!(state.error_detail_visible());
        state.toggle_error_details();
        assert!(!state.error_detail_visible());
        let view = state.view(strings());
        assert_eq!(view.error.as_ref().unwrap().diagnostic_id, "diag-01");
        state.dismiss_error();
        assert!(state.error().is_none());
        assert!(state.view(strings()).error.is_none());
    }

    #[test]
    fn progress_is_kept_per_task_and_rendered_in_status() {
        let mut state = standard_state();
        state.apply_event(Event::Progress {
            task_id: "t1".to_owned(),
            label: "识别".to_owned(),
            done: 3,
            total: 40,
        });
        state.apply_event(Event::Progress {
            task_id: "t1".to_owned(),
            label: "识别".to_owned(),
            done: 9,
            total: 40,
        });
        state.apply_event(Event::Progress {
            task_id: "t2".to_owned(),
            label: "判分".to_owned(),
            done: 1,
            total: 10,
        });
        let view = state.view(strings());
        assert_eq!(view.progress_text, "判分 1/10");
    }

    #[test]
    fn queue_stats_event_updates_mode_guard() {
        let mut state = standard_state();
        state.apply_event(Event::QueueStats {
            running: 1,
            pending: 3,
        });
        assert_eq!(state.request_mode_toggle(), ModeRequest::BlockedByRunning);
        state.apply_event(Event::QueueStats {
            running: 0,
            pending: 3,
        });
        assert_eq!(state.request_mode_toggle(), ModeRequest::NeedsConfirm);
    }

    #[test]
    fn toasts_are_bounded_and_dismissable() {
        let mut state = standard_state();
        for i in 0..(MAX_TOASTS + 3) {
            state.push_toast(NoticeLevel::Info, format!("标题{i}"), format!("正文{i}"));
        }
        assert_eq!(state.toasts().len(), MAX_TOASTS);
        let serial = state.toasts()[0].serial;
        state.dismiss_toast(serial);
        assert_eq!(state.toasts().len(), MAX_TOASTS - 1);
        assert!(state.toasts().iter().all(|toast| toast.serial != serial));
    }

    #[test]
    fn channel_stats_are_visible_in_view() {
        let mut state = standard_state();
        state.update_channel_stats(ChannelStats {
            merged_total: 7,
            dropped_total: 2,
            critical_dropped_total: 0,
            command_rejected_total: 3,
            command_depth: 1,
            event_depth: 4,
        });
        let view = state.view(strings());
        assert!(view.channel_text.contains('7'));
        assert!(view.channel_text.contains('2'));
        assert!(view.channel_text.contains('3'));
    }

    #[test]
    fn page_snapshot_event_overrides_body() {
        let mut state = standard_state();
        state.apply_event(Event::PageSnapshot {
            page: PageId::Home,
            title: "首页".to_owned(),
            subtitle: "副标题".to_owned(),
            body: "新快照正文".to_owned(),
        });
        assert_eq!(state.view(strings()).page_body, "新快照正文");
        state.navigate_to(PageId::Papers);
        assert!(state.view(strings()).page_body.contains("占位"));
    }

    #[test]
    fn escape_dismisses_topmost_layer() {
        let mut state = standard_state();
        assert!(!state.dismiss_top());
        state.apply_event(Event::TaskFailed {
            task_id: "t".to_owned(),
            message: "失败".to_owned(),
            retryable: false,
            diagnostic_id: "d".to_owned(),
        });
        assert!(state.dismiss_top());
        assert!(state.error().is_none());
        assert_eq!(state.request_mode_toggle(), ModeRequest::NeedsConfirm);
        assert!(state.dismiss_top());
        assert!(state.confirm().is_none());
    }

    #[test]
    fn settings_page_shows_about_section() {
        let mut state = standard_state();
        assert!(!state.view(strings()).show_about);
        state.navigate_to(PageId::Settings);
        let view = state.view(strings());
        assert!(view.show_about);
        assert!(view.about_body.contains("GPL-3.0-only"));
    }
}
