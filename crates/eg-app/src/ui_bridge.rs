//! Slint 属性投影与跨线程投递封装（`SPEC-app.md` §3 / §4）。
//!
//! - Slint 回调只做极薄转发：导航、模式、错误操作等动作在 Rust 侧完成状态迁移，
//!   再把 [`ViewModel`](crate::state::ViewModel) 投影到窗口属性；
//! - 后台线程 → UI 线程只经 [`UiDispatcher::post`]（内部 `slint::invoke_from_event_loop`），
//!   弱引用失效即丢弃并计数（不 panic、不阻塞后台线程）；
//! - 单次同步工作耗时记入 [`UiMetrics`]，供性能预算验收。

use std::sync::{Arc, Mutex};
use std::time::Instant;

use slint::{ModelRc, VecModel, Weak};

use crate::channel::Event;
use crate::i18n::Strings;
use crate::nav::PageId;
use crate::perf::UiMetrics;
use crate::state::{AppState, ViewModel};

use crate::{
    ActionItem, AppWindow, ConfirmContent, NavEntry, PageContent, PageLabels as SlintPageLabels,
    ToastItem,
};

/// 把状态视图投影到窗口属性（UI 线程执行；无 IO）。
pub(crate) fn apply_view(ui: &AppWindow, view: &ViewModel, strings: &Strings) {
    let labels = crate::i18n::page_labels();
    let nav: Vec<NavEntry> = view
        .nav
        .iter()
        .map(|entry| NavEntry {
            id: entry.id.as_str().into(),
            title: entry.title.as_str().into(),
            shortcut: entry.shortcut.as_str().into(),
        })
        .collect();
    ui.set_nav_entries(ModelRc::new(VecModel::from(nav)));

    ui.set_current_index(i32::try_from(view.current_index).unwrap_or(0));
    ui.set_page(PageContent {
        title: view.page_title.as_str().into(),
        subtitle: view.page_subtitle.as_str().into(),
        body: view.page_body.as_str().into(),
        action_label: view.page_action.as_str().into(),
    });
    let actions: Vec<ActionItem> = view
        .actions
        .iter()
        .map(|action| ActionItem {
            id: action.id.as_str().into(),
            label: action.label.as_str().into(),
            enabled: action.enabled,
        })
        .collect();
    ui.set_page_actions(ModelRc::new(VecModel::from(actions)));
    ui.set_page_labels(SlintPageLabels {
        save_setting: labels.save_setting.into(),
        setting_key_hint: labels.setting_key_hint.into(),
        setting_value_hint: labels.setting_value_hint.into(),
        review_score: labels.review_score.into(),
        review_image: labels.review_image.into(),
        image_evidence: labels.image_evidence.into(),
        sampling_title: labels.sampling_title.into(),
        sampling_single: labels.sampling_single.into(),
        sampling_whole: labels.sampling_whole.into(),
        rule_criterion_hint: labels.rule_criterion_hint.into(),
        rule_points_hint: labels.rule_points_hint.into(),
        rule_handwritten: labels.rule_handwritten.into(),
        consent_task_hint: labels.consent_task_hint.into(),
        consent_backend_hint: labels.consent_backend_hint.into(),
        consent_scope_hint: labels.consent_scope_hint.into(),
        consent_budget_hint: labels.consent_budget_hint.into(),
        consent_ttl_hint: labels.consent_ttl_hint.into(),
        consent_register: labels.consent_register.into(),
    });
    ui.set_import_page(view.import.visible);
    ui.set_progress_page(view.progress.visible);
    ui.set_settings_page(view.settings.visible);
    ui.set_review_page(view.review.visible);
    ui.set_rules_page(view.rules.visible);
    ui.set_consent_page(view.consent.visible);
    ui.set_diagnostics_page(view.diagnostics.visible);
    ui.set_sampling_visible(view.review.sampling.visible);
    ui.set_review_score(view.review.score_input.as_str().into());
    ui.set_review_queue_text(view.review.counts.as_str().into());
    let reasons: Vec<slint::SharedString> = view
        .review
        .reasons
        .iter()
        .map(|reason| reason.as_str().into())
        .collect();
    ui.set_review_reasons(ModelRc::new(VecModel::from(reasons)));
    ui.set_mode_label(view.mode_label.as_str().into());
    ui.set_mode_switch_label(view.mode_switch_label.as_str().into());
    ui.set_fast_mode(view.fast_mode);
    ui.set_fast_enabled(view.fast_enabled);
    ui.set_backend_ok(view.backend_ok);
    ui.set_backend_text(view.backend_label.as_str().into());
    ui.set_status_left(view.status_left.as_str().into());
    ui.set_channel_text(view.channel_text.as_str().into());
    ui.set_hint(view.hint.as_str().into());
    ui.set_progress_text(view.progress_text.as_str().into());

    ui.set_error_visible(view.error.is_some());
    ui.set_error_title(strings.error_title.into());
    ui.set_error_show_detail(strings.error_show_detail.into());
    ui.set_error_hide_detail(strings.error_hide_detail.into());
    ui.set_error_retry(strings.error_retry.into());
    ui.set_error_copy(strings.error_copy_id.into());
    if let Some(error) = &view.error {
        ui.set_error_summary(error.summary.as_str().into());
        ui.set_error_detail(error.detail.as_str().into());
        ui.set_error_diagnostic_id(
            format!("{}: {}", strings.diagnostic_id_label, error.diagnostic_id)
                .as_str()
                .into(),
        );
    } else {
        ui.set_error_summary("".into());
        ui.set_error_detail("".into());
        ui.set_error_diagnostic_id("".into());
    }
    ui.set_error_detail_visible(view.error_detail_visible);
    ui.set_retry_enabled(view.error.as_ref().is_some_and(|error| error.retryable));

    let toasts: Vec<ToastItem> = view
        .toasts
        .iter()
        .map(|toast| ToastItem {
            serial: i32::try_from(toast.serial).unwrap_or(i32::MAX),
            level: toast.level.code().into(),
            title: toast.title.as_str().into(),
            body: toast.body.as_str().into(),
        })
        .collect();
    ui.set_toasts(ModelRc::new(VecModel::from(toasts)));

    match &view.confirm {
        Some(confirm) => {
            ui.set_confirm_content(ConfirmContent {
                title: confirm.title.as_str().into(),
                body: confirm.body.as_str().into(),
                yes_label: strings.confirm_yes.into(),
                no_label: strings.confirm_no.into(),
            });
            ui.set_confirm_visible(true);
        }
        None => ui.set_confirm_visible(false),
    }

    ui.set_show_about(view.show_about);
    ui.set_about_title(view.about_title.as_str().into());
    ui.set_about_body(view.about_body.as_str().into());
    ui.set_open_settings(strings.open_settings.into());
    ui.set_open_logs(strings.open_logs.into());
    ui.set_log_placeholder(strings.log_viewer_placeholder.into());
    ui.set_dismiss_symbol(strings.dismiss_symbol.into());
    ui.set_startup_error_help(strings.startup_error_help.into());
}

/// 应用当前状态（加锁 + 投影 + 记录耗时；UI 线程调用）。
pub(crate) fn apply_state(ui: &AppWindow, state: &Arc<Mutex<AppState>>, strings: &Strings) {
    let Ok(guard) = state.lock() else {
        return;
    };
    let view = guard.view(strings);
    drop(guard);
    apply_view(ui, &view, strings);
}

/// 导航（快捷键/导航点击共用路径）：只改内存状态 + 投影，不做任何 IO。
pub(crate) fn navigate(
    ui: &AppWindow,
    state: &Arc<Mutex<AppState>>,
    metrics: &UiMetrics,
    index: usize,
) -> bool {
    let started = Instant::now();
    let known = PageId::from_index(index).is_some();
    if known {
        let Ok(mut guard) = state.lock() else {
            return false;
        };
        let decision = guard.navigate_to(PageId::from_index(index).unwrap_or(PageId::Home));
        let view = guard.view(crate::i18n::strings());
        drop(guard);
        apply_view(ui, &view, crate::i18n::strings());
        let _ = decision;
        // 重新聚焦快捷键作用域，保证切页后全局快捷键仍生效。
        ui.invoke_take_focus();
    }
    metrics.record_nav(started.elapsed());
    known
}

/// 在 UI 线程直接应用一个后台事件（bench/启动错误用；正常路径见 [`UiDispatcher`]）。
pub(crate) fn apply_event_now(
    ui: &AppWindow,
    state: &Arc<Mutex<AppState>>,
    metrics: &UiMetrics,
    event: Event,
) {
    let started = Instant::now();
    let image_path = match &event {
        Event::ReviewImage { path } => Some(path.clone()),
        _ => None,
    };
    let view = {
        let Ok(mut guard) = state.lock() else {
            return;
        };
        guard.apply_event(event);
        guard.view(crate::i18n::strings())
    };
    apply_view(ui, &view, crate::i18n::strings());
    if let Some(path) = image_path {
        match slint::Image::load_from_path(std::path::Path::new(&path)) {
            Ok(image) => {
                ui.set_review_image(image);
                ui.set_review_image_note("".into());
            }
            Err(error) => {
                ui.set_review_image_note(format!("图像证据加载失败：{error}").into());
            }
        }
    }
    metrics.record_event(started.elapsed());
}

/// 后台 → UI 线程投递器（弱引用；事件循环不可用时计数失败，不阻塞）。
#[derive(Clone)]
pub(crate) struct UiDispatcher {
    weak: Weak<AppWindow>,
    state: Arc<Mutex<AppState>>,
    metrics: Arc<UiMetrics>,
}

impl UiDispatcher {
    /// 构造。
    #[must_use]
    pub(crate) fn new(
        weak: Weak<AppWindow>,
        state: Arc<Mutex<AppState>>,
        metrics: Arc<UiMetrics>,
    ) -> Self {
        Self {
            weak,
            state,
            metrics,
        }
    }

    /// 投递事件到 UI 线程（`slint::invoke_from_event_loop`）。
    pub(crate) fn post(&self, event: Event) {
        let weak = self.weak.clone();
        let state = Arc::clone(&self.state);
        let metrics = Arc::clone(&self.metrics);
        let result = slint::invoke_from_event_loop(move || {
            let started = Instant::now();
            let Some(ui) = weak.upgrade() else {
                // 窗口已销毁：丢弃并计数（不 panic）。
                return;
            };
            let view = {
                let Ok(mut guard) = state.lock() else {
                    return;
                };
                guard.apply_event(event);
                guard.view(crate::i18n::strings())
            };
            apply_view(&ui, &view, crate::i18n::strings());
            metrics.record_event(started.elapsed());
        });
        if result.is_err() {
            self.metrics.note_invoke_failure();
        }
    }
}

/// 命令通道满/断开时的显式提示（不静默、不阻塞）。
pub(crate) fn notify_command_rejected(
    ui: &AppWindow,
    state: &Arc<Mutex<AppState>>,
    strings: &Strings,
) {
    let Ok(mut guard) = state.lock() else {
        return;
    };
    guard.push_toast(
        crate::channel::NoticeLevel::Warn,
        strings.notice_busy_title.to_owned(),
        strings.channel_full.to_owned(),
    );
    let view = guard.view(strings);
    drop(guard);
    apply_view(ui, &view, strings);
}
