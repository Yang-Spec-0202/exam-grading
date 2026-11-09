//! 有界命令/事件通道（`SPEC-app.md` §3 表、§5「通道满/界面刷新合并」）。
//!
//! 方向与容量（与规范一致）：
//! - 界面→后台：**界面命令**，容量 [`COMMAND_CAPACITY`] = 64；满时 `try_send` 返回
//!   [`CommandRejected::Busy`]，UI 立刻提示「操作过密」，不排队、不阻塞 UI 线程；
//! - 后台→界面：**界面事件**，容量 [`EVENT_CAPACITY`] = 256；满时的策略分三类：
//!   1. 可合并事件（任务进度 / 后端健康 / 页面快照）：只保留同类最新一条并计数
//!      （[`Event::merge_key`]）；
//!   2. 错误类事件（任务失败、错误通知）：进独立的待投递暂存区，**不静默丢弃**；
//!      暂存区也满时计数并保留计数（`critical_dropped_total`，UI 状态栏可见）；
//!   3. 其它事件：计数丢弃（`dropped_total`，UI 状态栏可见）。
//!
//! 后台投递到 UI 线程时必须经 [`crate::ui_bridge`] 的 `invoke_from_event_loop` 封装
//! （弱引用，窗口失效即丢弃并计数）。本模块不依赖 Slint，可纯单测。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};

use crate::nav::PageId;

/// 界面命令通道容量（`SPEC-app.md` §3）。
pub(crate) const COMMAND_CAPACITY: usize = 64;
/// 界面事件通道容量（`SPEC-app.md` §3）。
pub(crate) const EVENT_CAPACITY: usize = 256;
/// 可合并事件的暂存键上限（超出淘汰最旧键，计数到 `dropped_total`）。
pub(crate) const PENDING_MERGED_CAPACITY: usize = 64;
/// 错误类事件暂存区上限（超出计数到 `critical_dropped_total`，UI 可见）。
pub(crate) const PENDING_CRITICAL_CAPACITY: usize = 32;

/// 界面命令（UI → 后台 worker）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Command {
    /// 占位后台任务：worker 依次投递进度事件后完成（验证通道与进度投影）。
    RunPlaceholderJob,
    /// 确认切换模式（`to_fast` 为目标是否为快速模式）：写配置 + 审计后回投事件。
    ConfirmModeSwitch {
        /// 目标模式是否为快速。
        to_fast: bool,
    },
    /// 页内主操作占位（M9 后续任务替换为真实命令）。
    PrimaryAction {
        /// 触发页。
        page: PageId,
    },
    /// 通用页面动作转发（id 由页面视图模型给出，worker 解析）。
    PageAction {
        /// 动作 id（如 `import.pick`）。
        id: String,
    },
    /// 选择文件/文件夹（worker 调系统选择器；`folder=true` 为文件夹）。
    ImportPick {
        /// 是否选择文件夹。
        folder: bool,
    },
    /// 拖拽放入的文件路径（绝对路径）。
    ImportDrop {
        /// 路径列表。
        paths: Vec<String>,
    },
    /// 重试指定任务。
    TaskRetry {
        /// 任务 id。
        task_id: String,
    },
    /// 取消指定任务。
    TaskCancel {
        /// 任务 id。
        task_id: String,
    },
    /// 复核：选中队列行。
    ReviewSelect {
        /// 行下标。
        index: usize,
    },
    /// 复核：执行命令（kind = accept/override/regrade_text/batch_accept/sampling_pass）。
    ReviewCommand {
        /// 命令种类。
        kind: String,
        /// 改分输入（千分之一分文本；override 用）。
        score: Option<String>,
        /// 原因码下标（0 = 未选）。
        reason: Option<i32>,
        /// 修正文本（regrade_text 用）。
        text: Option<String>,
    },
    /// 复核：抽检不通过后的重跑范围（single/paper）。
    SamplingScope {
        /// 范围。
        scope: String,
    },
    /// 规则页：更新原文。
    RulesRaw {
        /// 原文。
        text: String,
    },
    /// 规则页：模型整理。
    RulesStructure,
    /// 规则页：手写直译草稿（不依赖模型；criterion/points 来自界面输入）。
    RulesHandwritten {
        /// 规则原文。
        raw_text: String,
        /// 评分项判据。
        criterion: String,
        /// 评分项分值（分，十进制文本）。
        points: String,
    },
    /// 规则页：确认草稿。
    RulesConfirm,
    /// 规则页：拒绝草稿。
    RulesReject {
        /// 原因。
        reason: String,
    },
    /// 授权页：刷新列表。
    ConsentLoad,
    /// 授权页：新建授权。
    ConsentRegister {
        /// 任务 id。
        task_id: String,
        /// 后端。
        backend: String,
        /// 范围说明。
        scope: String,
        /// 预算（微货币单位）。
        budget: String,
        /// 有效期（小时）。
        ttl_hours: String,
    },
    /// 授权页：撤销。
    ConsentRevoke {
        /// 授权 id。
        consent_id: String,
    },
    /// 设置页：加载配置快照。
    SettingsLoad,
    /// 设置页：修改一个键（候选值由 worker 校验后原子保存）。
    SettingsPatch {
        /// 点分键。
        key: String,
        /// 文本值。
        raw: String,
    },
    /// 设置页：恢复上一版配置。
    SettingsRestoreBackup,
    /// 诊断页：查询日志。
    LogQuery {
        /// 级别（空 = 全部）。
        level: String,
        /// 模块（空 = 全部）。
        module: String,
        /// 关键词（空 = 字段过滤）。
        keyword: String,
        /// 链路 id（空 = 全部）。
        trace_id: String,
        /// 页码（1 起）。
        page: u32,
    },
    /// 诊断页：导出诊断包。
    DiagnosticsExport,
    /// 打开数据/日志目录（系统文件管理器）。
    OpenDataDir {
        /// 是否打开日志目录（否则数据根）。
        logs: bool,
    },
    /// 错误横幅「重试」占位。
    RetryError,
    /// 记录启动阶段耗时（写入 platform 日志；UI 线程不直接做 IO）。
    LogStartup {
        /// 平台初始化（含单实例锁/配置/迁移检查）毫秒。
        platform_open_ms: u64,
        /// Slint 组件实例化毫秒。
        ui_new_ms: u64,
        /// `show()` 毫秒。
        ui_show_ms: u64,
        /// 进程启动 → 首帧回调毫秒。
        first_frame_ms: u64,
    },
    /// 报表页：导出当前试卷报表（真实 platform/eg-report）。
    ExportReport,
    /// 冒烟链路：夹具落库 → 复核改分 → 导出（仅 --smoke）。
    RunSmokeChain,
    /// 退出事件循环。
    Quit,
}

/// 通知级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoticeLevel {
    /// 信息。
    Info,
    /// 警告。
    Warn,
    /// 错误（错误类事件，满通道时不静默丢弃）。
    Error,
}

impl NoticeLevel {
    /// 稳定机器码（日志/UI 颜色映射用）。
    #[must_use]
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

/// 界面事件（后台 → UI 线程）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Event {
    /// 任务进度（同类可按 `task_id` 合并，只留最新）。
    Progress {
        /// 任务 id（文本形式）。
        task_id: String,
        /// 进度文案（阶段/文件名等，已由调用方本地化）。
        label: String,
        /// 已完成。
        done: u32,
        /// 总数。
        total: u32,
    },
    /// 任务失败（错误类，不静默丢弃）。
    TaskFailed {
        /// 任务 id。
        task_id: String,
        /// 中文摘要。
        message: String,
        /// 是否可重试。
        retryable: bool,
        /// 诊断 id（可复制，映射日志/任务）。
        diagnostic_id: String,
    },
    /// 后端健康（同类合并，只留最新）。
    BackendHealth {
        /// 是否就绪。
        ready: bool,
        /// 中文说明。
        message: String,
    },
    /// 模式已变更（写盘成功后的回投）。
    ModeChanged {
        /// 是否快速模式。
        fast: bool,
    },
    /// 普通通知（信息/警告；错误级见 `TaskFailed`/`NoticeLevel::Error`）。
    Notice {
        /// 级别。
        level: NoticeLevel,
        /// 标题。
        title: String,
        /// 正文。
        body: String,
    },
    /// 队列统计（同类合并，只留最新；用于「运行中任务禁止切换模式」守卫）。
    QueueStats {
        /// 运行中任务数。
        running: u32,
        /// 待运行任务数。
        pending: u32,
    },
    /// 页面快照更新（同类按页合并，只留最新）。
    PageSnapshot {
        /// 页。
        page: PageId,
        /// 标题。
        title: String,
        /// 副标题。
        subtitle: String,
        /// 正文。
        body: String,
    },
    /// 日志分页（T38 日志查看器占位）。
    LogPage {
        /// 当前页。
        page: usize,
        /// 总条数。
        total: usize,
        /// 本页行数。
        lines: usize,
    },
    /// 导入进度。
    ImportProgress {
        /// 已完成。
        done: u32,
        /// 总数。
        total: u32,
    },
    /// 导入完成（含部分成功/拒收/失败明细）。
    ImportFinished {
        /// 报告。
        report: crate::pages::import::ImportReportVm,
    },
    /// 导入失败（批次级）。
    ImportFailed {
        /// 中文原因。
        message: String,
    },
    /// 任务队列快照。
    QueueTasks {
        /// 行。
        rows: Vec<crate::pages::progress::TaskRowVm>,
    },
    /// 配置快照（TOML 文本）。
    SettingsLoaded {
        /// 文本。
        toml_text: String,
    },
    /// 配置保存失败（含校验原因；旧配置保持不变）。
    SettingsRejected {
        /// 键。
        key: String,
        /// 原因。
        message: String,
    },
    /// 配置已保存（新快照）。
    SettingsSaved {
        /// 文本。
        toml_text: String,
    },
    /// 复核队列已加载。
    ReviewLoaded {
        /// 行。
        rows: Vec<crate::pages::review::QueueRowVm>,
        /// 计数/说明文本。
        counts: String,
    },
    /// 复核详情。
    ReviewDetail {
        /// 元信息。
        meta: String,
        /// 学生答案。
        answer: String,
        /// 参考答案。
        reference: String,
        /// 判定摘要。
        judgement: String,
        /// 得分文本。
        score_text: String,
        /// 原因码。
        reasons: Vec<String>,
        /// 证据。
        evidence: Vec<crate::pages::review::EvidenceVm>,
        /// 是否只读。
        read_only: bool,
    },
    /// 复核命令已应用。
    ReviewApplied {
        /// 回显。
        notice: String,
        /// 是否前进到下一题。
        advance: bool,
    },
    /// 撤销已应用。
    ReviewUndone {
        /// 回显。
        notice: String,
    },
    /// 抽检不通过弹窗。
    ReviewSamplingPrompt {
        /// 差异摘要。
        message: String,
    },
    /// 复核提示（不改变判定）。
    ReviewNotice {
        /// 回显。
        message: String,
    },
    /// 规则草稿就绪。
    RulesDraft {
        /// 草稿 id。
        draft_id: String,
        /// 模型调用 id（手写路径为空）。
        model_call_id: String,
        /// 评分项。
        items: Vec<crate::pages::rules::RuleItemVm>,
        /// 校验问题。
        findings: Vec<crate::pages::rules::FindingVm>,
    },
    /// 规则整理失败。
    RulesFailed {
        /// 原因。
        message: String,
        /// 模型调用 id（可空）。
        model_call_id: String,
    },
    /// 规则已确认。
    RulesConfirmed {
        /// 新版本。
        version: u32,
    },
    /// 规则已拒绝。
    RulesRejected {
        /// 原因。
        reason: String,
    },
    /// 授权列表。
    ConsentRows {
        /// 行。
        rows: Vec<crate::pages::consent::ConsentRowVm>,
    },
    /// 授权已登记。
    ConsentRegistered {
        /// 授权 id。
        consent_id: String,
    },
    /// 授权已撤销。
    ConsentRevoked {
        /// 授权 id。
        consent_id: String,
    },
    /// 授权操作失败。
    ConsentFailed {
        /// 原因。
        message: String,
    },
    /// 日志一页结果。
    LogRows {
        /// 行。
        rows: Vec<crate::pages::diagnostics::LogRowVm>,
        /// 命中总数。
        total: u32,
        /// 页码。
        page: u32,
        /// 索引状态说明。
        index_note: String,
    },
    /// 复核图像证据懒加载（platform 资产路径；UI 线程加载失败显式提示）。
    ReviewImage {
        /// 资产文件绝对路径。
        path: String,
    },
    /// 撤销/重做可用性（platform 持久栈长度；用于按钮可用性与提示）。
    ReviewHistory {
        /// 可撤销步数。
        undo_len: u32,
        /// 可重做步数。
        redo_len: u32,
    },
    /// 报表导出成功（清 stale 并展示路径/行数）。
    ReportExported {
        /// 中文摘要。
        summary: String,
    },
    /// 诊断包导出结果。
    DiagnosticsExported {
        /// 摘要（成功/失败均用）。
        summary: String,
    },
}

impl Event {
    /// 可合并键；`None` 表示不可合并（满通道按错误/普通策略处理）。
    #[must_use]
    pub(crate) fn merge_key(&self) -> Option<MergeKey> {
        match self {
            Self::LogPage { page, .. } => Some(MergeKey::Log(*page)),
            Self::Progress { task_id, .. } => Some(MergeKey::Progress(task_id.clone())),
            Self::BackendHealth { .. } => Some(MergeKey::Backend),
            Self::QueueStats { .. } => Some(MergeKey::Queue),
            Self::PageSnapshot { page, .. } => Some(MergeKey::Snapshot(*page)),
            Self::TaskFailed { .. } | Self::ModeChanged { .. } | Self::Notice { .. } => None,
            Self::ImportProgress { .. } => Some(MergeKey::Import),
            Self::ReviewImage { .. } => Some(MergeKey::Image),
            Self::ReportExported { .. }
            | Self::ImportFinished { .. }
            | Self::ImportFailed { .. }
            | Self::QueueTasks { .. }
            | Self::SettingsLoaded { .. }
            | Self::SettingsRejected { .. }
            | Self::SettingsSaved { .. }
            | Self::ReviewLoaded { .. }
            | Self::ReviewDetail { .. }
            | Self::ReviewApplied { .. }
            | Self::ReviewUndone { .. }
            | Self::ReviewSamplingPrompt { .. }
            | Self::ReviewNotice { .. }
            | Self::RulesDraft { .. }
            | Self::RulesFailed { .. }
            | Self::RulesConfirmed { .. }
            | Self::RulesRejected { .. }
            | Self::ConsentRows { .. }
            | Self::ConsentRegistered { .. }
            | Self::ConsentRevoked { .. }
            | Self::ConsentFailed { .. }
            | Self::LogRows { .. }
            | Self::DiagnosticsExported { .. }
            | Self::ReviewHistory { .. } => None,
        }
    }

    /// 是否错误类事件（满通道时优先保留，不静默丢弃）。
    #[must_use]
    pub(crate) fn is_critical(&self) -> bool {
        match self {
            Self::TaskFailed { .. } => true,
            Self::Notice { level, .. } => *level == NoticeLevel::Error,
            Self::ImportFailed { .. }
            | Self::SettingsRejected { .. }
            | Self::RulesFailed { .. }
            | Self::ConsentFailed { .. }
            | Self::DiagnosticsExported { .. } => true,
            Self::ReportExported { .. } => false,
            Self::Progress { .. }
            | Self::BackendHealth { .. }
            | Self::QueueStats { .. }
            | Self::ModeChanged { .. }
            | Self::PageSnapshot { .. }
            | Self::LogPage { .. }
            | Self::ImportProgress { .. }
            | Self::ImportFinished { .. }
            | Self::QueueTasks { .. }
            | Self::SettingsLoaded { .. }
            | Self::SettingsSaved { .. }
            | Self::ReviewLoaded { .. }
            | Self::ReviewDetail { .. }
            | Self::ReviewApplied { .. }
            | Self::ReviewUndone { .. }
            | Self::ReviewSamplingPrompt { .. }
            | Self::ReviewNotice { .. }
            | Self::RulesDraft { .. }
            | Self::RulesConfirmed { .. }
            | Self::RulesRejected { .. }
            | Self::ConsentRows { .. }
            | Self::ConsentRegistered { .. }
            | Self::ConsentRevoked { .. }
            | Self::LogRows { .. }
            | Self::ReviewHistory { .. }
            | Self::ReviewImage { .. } => false,
        }
    }
}

/// 可合并事件的键（同类只留最新）。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum MergeKey {
    /// 某任务的最新进度。
    Progress(String),
    /// 最新后端健康。
    Backend,
    /// 最新队列统计。
    Queue,
    /// 最新导入进度。
    Import,
    /// 最新复核图像。
    Image,
    /// 某页的最新快照。
    Snapshot(PageId),
    /// 最新日志分页。
    Log(usize),
}

/// 单次投递结果（测试与日志用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EmitOutcome {
    /// 直接进入事件通道。
    Sent,
    /// 通道满，已合并进待投递暂存区。
    Merged,
    /// 通道满，错误类事件已进待投递暂存区。
    Carried,
    /// 通道满，已计数丢弃（非错误类）。
    Dropped,
    /// 通道已断开（接收端不在）。
    Disconnected,
}

/// 通道计数快照（UI 状态栏/日志显式展示，禁止静默）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ChannelStats {
    /// 合并事件次数。
    pub(crate) merged_total: u64,
    /// 普通事件丢弃次数。
    pub(crate) dropped_total: u64,
    /// 错误事件丢弃次数（暂存区溢出；UI 必须可见）。
    pub(crate) critical_dropped_total: u64,
    /// 命令被拒（「操作过密」）次数。
    pub(crate) command_rejected_total: u64,
    /// 当前命令通道近似深度。
    pub(crate) command_depth: usize,
    /// 当前事件通道近似深度。
    pub(crate) event_depth: usize,
}

#[derive(Debug, Default)]
struct Carry {
    merged: BTreeMap<MergeKey, Event>,
    critical: Vec<Event>,
    merged_total: u64,
    dropped_total: u64,
    critical_dropped_total: u64,
}

/// 后台 → 界面事件总线。
#[derive(Debug, Clone)]
pub(crate) struct EventBus {
    tx: SyncSender<Event>,
    carry: Arc<Mutex<Carry>>,
    depth: Arc<AtomicUsize>,
    rejected: Arc<AtomicU64>,
}

impl EventBus {
    /// 新建事件总线（返回总线与接收端）。
    #[must_use]
    pub(crate) fn new() -> (Self, Receiver<Event>) {
        let (tx, rx) = sync_channel(EVENT_CAPACITY);
        (
            Self {
                tx,
                carry: Arc::new(Mutex::new(Carry::default())),
                depth: Arc::new(AtomicUsize::new(0)),
                rejected: Arc::new(AtomicU64::new(0)),
            },
            rx,
        )
    }

    /// 投递事件（非阻塞；满时按合并/错误/丢弃策略处理）。
    /// 发送事件并忽略可合并/计数结果（页面服务使用；错误类事件仍计数）。
    pub(crate) fn send(&self, event: Event) {
        let _ = self.emit(event);
    }

    pub(crate) fn emit(&self, event: Event) -> EmitOutcome {
        match self.tx.try_send(event) {
            Ok(()) => {
                self.depth.fetch_add(1, Ordering::Relaxed);
                EmitOutcome::Sent
            }
            Err(TrySendError::Disconnected(_)) => EmitOutcome::Disconnected,
            Err(TrySendError::Full(event)) => self.carry_event(event),
        }
    }

    fn carry_event(&self, event: Event) -> EmitOutcome {
        let Ok(mut carry) = self.carry.lock() else {
            // 锁中毒：退化为普通丢弃并计数（库不 panic）。
            self.rejected.fetch_add(1, Ordering::Relaxed);
            return EmitOutcome::Dropped;
        };
        if let Some(key) = event.merge_key() {
            if !carry.merged.contains_key(&key) && carry.merged.len() >= PENDING_MERGED_CAPACITY {
                // 键过多（极端）：淘汰最旧键并计数，避免无界内存。
                if let Some(oldest) = carry.merged.keys().next().cloned() {
                    carry.merged.remove(&oldest);
                    carry.dropped_total += 1;
                }
            }
            carry.merged.insert(key, event);
            carry.merged_total += 1;
            return EmitOutcome::Merged;
        }
        if event.is_critical() {
            if carry.critical.len() >= PENDING_CRITICAL_CAPACITY {
                carry.critical_dropped_total += 1;
                return EmitOutcome::Dropped;
            }
            carry.critical.push(event);
            return EmitOutcome::Carried;
        }
        carry.dropped_total += 1;
        EmitOutcome::Dropped
    }

    /// 取出暂存区事件（错误类优先，其后为可合并事件的最新值，顺序确定）。
    pub(crate) fn take_carry(&self) -> Vec<Event> {
        let Ok(mut carry) = self.carry.lock() else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(carry.critical.len() + carry.merged.len());
        out.append(&mut carry.critical);
        let merged = std::mem::take(&mut carry.merged);
        out.extend(merged.into_values());
        out
    }

    /// 接收端取走一条事件后调用（维护近似深度）。
    pub(crate) fn record_receive(&self) {
        let _ = self
            .depth
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                Some(value.saturating_sub(1))
            });
    }

    /// 计数快照。
    #[must_use]
    pub(crate) fn stats(&self) -> ChannelStats {
        let (merged, dropped, critical_dropped) = self.carry.lock().map_or((0, 0, 0), |carry| {
            (
                carry.merged_total,
                carry.dropped_total,
                carry.critical_dropped_total,
            )
        });
        ChannelStats {
            merged_total: merged,
            dropped_total: dropped,
            critical_dropped_total: critical_dropped,
            command_rejected_total: 0,
            command_depth: 0,
            event_depth: self.depth.load(Ordering::Relaxed),
        }
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new().0
    }
}

/// 命令被拒原因（UI 必须提示，不阻塞）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandRejected {
    /// 通道满：提示「操作过密」。
    Busy,
    /// 后台 worker 已退出。
    Disconnected,
}

/// 界面 → 后台命令总线。
#[derive(Debug, Clone)]
pub(crate) struct CommandBus {
    tx: SyncSender<Command>,
    rejected: Arc<AtomicU64>,
    depth: Arc<AtomicUsize>,
}

impl CommandBus {
    /// 新建命令总线（返回总线与接收端，接收端交给后台 worker）。
    #[must_use]
    pub(crate) fn new() -> (Self, Receiver<Command>) {
        let (tx, rx) = sync_channel(COMMAND_CAPACITY);
        (
            Self {
                tx,
                rejected: Arc::new(AtomicU64::new(0)),
                depth: Arc::new(AtomicUsize::new(0)),
            },
            rx,
        )
    }

    /// 非阻塞发送；满时计数并返回 [`CommandRejected::Busy`]（调用方须提示用户）。
    pub(crate) fn try_send(&self, command: Command) -> Result<(), CommandRejected> {
        match self.tx.try_send(command) {
            Ok(()) => {
                self.depth.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(TrySendError::Full(_)) => {
                self.rejected.fetch_add(1, Ordering::Relaxed);
                Err(CommandRejected::Busy)
            }
            Err(TrySendError::Disconnected(_)) => {
                self.rejected.fetch_add(1, Ordering::Relaxed);
                Err(CommandRejected::Disconnected)
            }
        }
    }

    /// 接收端取走一条命令后调用（维护近似深度）。
    pub(crate) fn record_receive(&self) {
        let _ = self
            .depth
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                Some(value.saturating_sub(1))
            });
    }

    /// 被拒总次数。
    #[must_use]
    pub(crate) fn rejected_total(&self) -> u64 {
        self.rejected.load(Ordering::Relaxed)
    }

    /// 近似深度。
    #[must_use]
    pub(crate) fn depth(&self) -> usize {
        self.depth.load(Ordering::Relaxed)
    }
}

impl Default for CommandBus {
    fn default() -> Self {
        Self::new().0
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn notice(body: &str) -> Event {
        Event::Notice {
            level: NoticeLevel::Info,
            title: "提示".to_owned(),
            body: body.to_owned(),
        }
    }

    fn progress(done: u32) -> Event {
        Event::Progress {
            task_id: "task-1".to_owned(),
            label: "识别".to_owned(),
            done,
            total: 40,
        }
    }

    #[test]
    fn event_bus_carries_exact_capacity_then_counts_drops() {
        let (bus, rx) = EventBus::new();
        for i in 0..EVENT_CAPACITY {
            assert_eq!(bus.emit(notice(&format!("n{i}"))), EmitOutcome::Sent);
        }
        // 非合并、非错误事件在满通道后计数丢弃。
        assert_eq!(bus.emit(notice("overflow")), EmitOutcome::Dropped);
        assert_eq!(bus.emit(notice("overflow-2")), EmitOutcome::Dropped);
        let stats = bus.stats();
        assert_eq!(stats.dropped_total, 2);
        assert_eq!(stats.critical_dropped_total, 0);
        assert_eq!(rx.try_iter().count(), EVENT_CAPACITY);
        assert_eq!(stats.event_depth.max(1), 256); // 近似深度在接收后可能滞后
    }

    #[test]
    fn coalescible_progress_keeps_latest_and_counts_merged() {
        let (bus, _rx) = EventBus::new();
        for i in 0..EVENT_CAPACITY {
            assert_eq!(bus.emit(notice(&format!("n{i}"))), EmitOutcome::Sent);
        }
        for done in 1..=5 {
            assert_eq!(bus.emit(progress(done)), EmitOutcome::Merged);
        }
        assert_eq!(bus.stats().merged_total, 5);
        let carried = bus.take_carry();
        assert_eq!(carried.len(), 1, "同类进度只保留最新一条");
        assert_eq!(
            carried[0],
            Event::Progress {
                task_id: "task-1".to_owned(),
                label: "识别".to_owned(),
                done: 5,
                total: 40,
            }
        );
    }

    #[test]
    fn critical_events_are_carried_not_silently_dropped() {
        let (bus, _rx) = EventBus::new();
        for i in 0..EVENT_CAPACITY {
            assert_eq!(bus.emit(notice(&format!("n{i}"))), EmitOutcome::Sent);
        }
        for i in 0..PENDING_CRITICAL_CAPACITY {
            let event = Event::TaskFailed {
                task_id: format!("t{i}"),
                message: "判分失败".to_owned(),
                retryable: true,
                diagnostic_id: format!("diag-{i}"),
            };
            assert_eq!(bus.emit(event), EmitOutcome::Carried);
        }
        // 暂存区溢出：计数可见，不静默。
        let overflow = Event::TaskFailed {
            task_id: "t-overflow".to_owned(),
            message: "判分失败".to_owned(),
            retryable: false,
            diagnostic_id: "diag-overflow".to_owned(),
        };
        assert_eq!(bus.emit(overflow), EmitOutcome::Dropped);
        let stats = bus.stats();
        assert_eq!(stats.critical_dropped_total, 1);
        assert_eq!(stats.dropped_total, 0);
        let carried = bus.take_carry();
        assert_eq!(carried.len(), PENDING_CRITICAL_CAPACITY);
        assert!(carried.iter().all(Event::is_critical));
    }

    #[test]
    fn carry_drains_all_kinds_and_resets() {
        let (bus, _rx) = EventBus::new();
        for i in 0..EVENT_CAPACITY {
            assert_eq!(bus.emit(notice(&format!("n{i}"))), EmitOutcome::Sent);
        }
        assert_eq!(
            bus.emit(Event::BackendHealth {
                ready: false,
                message: "未就绪".to_owned(),
            }),
            EmitOutcome::Merged
        );
        assert_eq!(bus.emit(progress(3)), EmitOutcome::Merged);
        let carried = bus.take_carry();
        assert_eq!(carried.len(), 2);
        assert_eq!(bus.take_carry().len(), 0, "取出后暂存区应为空");
    }

    #[test]
    fn command_bus_rejects_when_full_and_counts() {
        let (bus, rx) = CommandBus::new();
        for _ in 0..COMMAND_CAPACITY {
            bus.try_send(Command::RunPlaceholderJob).unwrap();
        }
        assert_eq!(
            bus.try_send(Command::RunPlaceholderJob),
            Err(CommandRejected::Busy)
        );
        assert_eq!(bus.rejected_total(), 1);
        assert_eq!(rx.try_iter().count(), COMMAND_CAPACITY);
    }

    #[test]
    fn command_bus_still_accepts_after_receiver_drains() {
        let (bus, rx) = CommandBus::new();
        for _ in 0..COMMAND_CAPACITY {
            bus.try_send(Command::RunPlaceholderJob).unwrap();
        }
        assert_eq!(bus.try_send(Command::Quit), Err(CommandRejected::Busy));
        rx.recv().unwrap();
        assert!(bus.try_send(Command::Quit).is_ok());
    }
}
