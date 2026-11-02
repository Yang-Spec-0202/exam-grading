//! 日志系统（`SPEC-platform.md` §6）：JSONL、级别覆盖、脱敏、轮转、保留、查询、全文索引集成。
//!
//! - 字段：时间戳、级别、模块、`trace_id`、`task_id`、`stage`、`event_code`、`error_class`、
//!   `latency_ms`、`attempt`、`asset_prefix`、`retry_in_ms` + 脱敏后的业务字段（顶层平铺）。
//! - 级别解析：模块级覆盖（`log.per_module_level`，支持模块族前缀）→ 全局（`log.level`）。
//! - 不阻塞：有界队列 + 后台线程；队列满丢弃低级别并累计 `dropped_total`；
//!   **错误级同步落盘**（失败向调用方返回错误，由 UI 显式告警）。
//! - 脱敏：姓名 → 掩码 + 短哈希；学号 → 仅末 4 位；家目录用户名 → `~`；密钥类字段 → 占位符。
//! - 轮转：按天（`app.<日期>.<序号>.jsonl`）或单文件 50MB；保留仅作用于日志（审计不动）。

use std::collections::BTreeMap;
use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use eg_domain::{Stage, TaskId, Timestamp, Ulid};
use serde::{Deserialize, Serialize};

use crate::clock::SharedClock;
use crate::config::LogRotation;
use crate::error::{ErrorClass, PlatformError, Result};
use crate::index::{LogIndex, LogIndexHit};

/// 默认单文件轮转阈值：50MB（`log.rotation = size_50mb`）。
pub const DEFAULT_SIZE_ROTATION_BYTES: u64 = 50 * 1024 * 1024;

/// 异步日志队列深度。
pub const LOG_QUEUE_DEPTH: usize = 1_024;

// ---------------------------------------------------------------------------
// 级别
// ---------------------------------------------------------------------------

/// 日志级别（`trace < debug < info < warn < error`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// 跟踪（按需开，含逐次模型调用摘要）。
    Trace,
    /// 调试（默认关）。
    Debug,
    /// 信息（全局默认）。
    Info,
    /// 警告（异常但可继续）。
    Warn,
    /// 错误（必须落盘、教师可见）。
    Error,
}

impl LogLevel {
    /// 稳定机器码（日志 JSON `level` 字段）。
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }

    /// 中文标签。
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Trace => "跟踪",
            Self::Debug => "调试",
            Self::Info => "信息",
            Self::Warn => "警告",
            Self::Error => "错误",
        }
    }

    /// 是否达到给定门槛（`self >= threshold`）。
    #[must_use]
    pub const fn enabled_at(self, threshold: Self) -> bool {
        self as u8 >= threshold as u8
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl FromStr for LogLevel {
    type Err = String;

    fn from_str(input: &str) -> std::result::Result<Self, Self::Err> {
        match input.trim().to_ascii_lowercase().as_str() {
            "trace" => Ok(Self::Trace),
            "debug" => Ok(Self::Debug),
            "info" => Ok(Self::Info),
            "warn" | "warning" => Ok(Self::Warn),
            "error" => Ok(Self::Error),
            other => Err(format!(
                "非法日志级别「{other}」：只允许 trace/debug/info/warn/error"
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// 记录
// ---------------------------------------------------------------------------

/// 跨模块 trace 关联 id（`SPEC-domain.md` §15.2 已批准）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TraceId(Ulid);

impl TraceId {
    /// 由 ULID 生成器创建。
    ///
    /// # Errors
    ///
    /// ULID 生成失败时返回错误。
    pub fn generate(ids: &crate::ids::UlidFactory) -> Result<Self> {
        ids.next_ulid().map(Self)
    }

    /// 由既有 ULID 构造。
    #[must_use]
    pub const fn from_ulid(value: Ulid) -> Self {
        Self(value)
    }

    /// 内部 ULID。
    #[must_use]
    pub const fn as_ulid(self) -> Ulid {
        self.0
    }

    /// 严格解析 26 字符 ULID。
    ///
    /// # Errors
    ///
    /// 文本非法时返回错误。
    pub fn parse(text: &str) -> Result<Self> {
        Ulid::parse(text)
            .map(Self)
            .map_err(|error| PlatformError::InvalidArgument(format!("trace_id 非法：{error}")))
    }
}

impl fmt::Display for TraceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// 一条结构化日志。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogRecord {
    /// 时间戳（由注入时钟提供）。
    pub at: Timestamp,
    /// 级别。
    pub level: LogLevel,
    /// 模块（crate/子模块名）。
    pub module: String,
    /// 事件码（稳定机器码，便于统计）。
    pub event_code: String,
    /// 中文消息。
    pub message: String,
    /// 链路 id。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<TraceId>,
    /// 任务 id。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    /// 阶段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<Stage>,
    /// 错误分类。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_class: Option<ErrorClass>,
    /// 延迟（毫秒）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    /// 尝试序号。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    /// 资产哈希前 8 位。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_prefix: Option<String>,
    /// 下次重试间隔（毫秒）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_in_ms: Option<u64>,
    /// 业务字段（脱敏后；顶层平铺）。
    #[serde(flatten)]
    pub fields: BTreeMap<String, String>,
}

impl LogRecord {
    /// 构造日志（时间必须由调用方从注入时钟取）。
    #[must_use]
    pub fn new(
        at: Timestamp,
        level: LogLevel,
        module: impl Into<String>,
        event_code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            at,
            level,
            module: module.into(),
            event_code: event_code.into(),
            message: message.into(),
            trace_id: None,
            task_id: None,
            stage: None,
            error_class: None,
            latency_ms: None,
            attempt: None,
            asset_prefix: None,
            retry_in_ms: None,
            fields: BTreeMap::new(),
        }
    }

    /// 设置链路 id。
    #[must_use]
    pub fn with_trace(mut self, trace_id: TraceId) -> Self {
        self.trace_id = Some(trace_id);
        self
    }

    /// 设置任务 id。
    #[must_use]
    pub fn with_task(mut self, task_id: TaskId) -> Self {
        self.task_id = Some(task_id);
        self
    }

    /// 设置阶段。
    #[must_use]
    pub fn with_stage(mut self, stage: Stage) -> Self {
        self.stage = Some(stage);
        self
    }

    /// 设置错误分类。
    #[must_use]
    pub fn with_error_class(mut self, class: ErrorClass) -> Self {
        self.error_class = Some(class);
        self
    }

    /// 设置延迟。
    #[must_use]
    pub fn with_latency_ms(mut self, latency_ms: u64) -> Self {
        self.latency_ms = Some(latency_ms);
        self
    }

    /// 设置尝试序号。
    #[must_use]
    pub fn with_attempt(mut self, attempt: u32) -> Self {
        self.attempt = Some(attempt);
        self
    }

    /// 设置资产哈希前缀。
    #[must_use]
    pub fn with_asset_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.asset_prefix = Some(prefix.into());
        self
    }

    /// 设置重试间隔。
    #[must_use]
    pub fn with_retry_in_ms(mut self, retry_in_ms: u64) -> Self {
        self.retry_in_ms = Some(retry_in_ms);
        self
    }

    /// 追加业务字段。
    #[must_use]
    pub fn field(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.fields.insert(key.into(), value.into());
        self
    }
}

// ---------------------------------------------------------------------------
// 设置 / 查询类型
// ---------------------------------------------------------------------------

/// 日志设置（来自 `AppConfig.log`；阈值与保留期与 `CONFIG.md` 一致）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogSettings {
    /// 全局级别。
    pub level: LogLevel,
    /// 模块级覆盖（模块名 → 级别）。
    pub per_module_level: BTreeMap<String, LogLevel>,
    /// 保留天数。
    pub retention_days: u32,
    /// 轮转策略。
    pub rotation: LogRotation,
    /// 脱敏开关。
    pub redaction: bool,
    /// 全文索引开关。
    pub search_index: bool,
    /// 索引覆盖天数。
    pub index_range_days: u32,
    /// 单文件轮转阈值（`size_50mb` 用；测试可调小）。
    pub rotation_size_bytes: u64,
}

impl Default for LogSettings {
    fn default() -> Self {
        Self {
            level: LogLevel::Info,
            per_module_level: BTreeMap::new(),
            retention_days: 30,
            rotation: LogRotation::Daily,
            redaction: true,
            search_index: true,
            index_range_days: 30,
            rotation_size_bytes: DEFAULT_SIZE_ROTATION_BYTES,
        }
    }
}

impl From<&crate::config::LogConfig> for LogSettings {
    fn from(config: &crate::config::LogConfig) -> Self {
        Self {
            level: config.level,
            per_module_level: config.per_module_level.clone(),
            retention_days: config.retention_days,
            rotation: config.rotation,
            redaction: config.redaction,
            search_index: config.search_index,
            index_range_days: config.index_range_days,
            rotation_size_bytes: DEFAULT_SIZE_ROTATION_BYTES,
        }
    }
}

impl LogSettings {
    /// 解析模块有效级别：模块级覆盖（含模块族前缀）优先，否则全局。
    #[must_use]
    pub fn level_for(&self, module: &str) -> LogLevel {
        if let Some(level) = self.per_module_level.get(module) {
            return *level;
        }
        let mut best: Option<(usize, LogLevel)> = None;
        for (key, level) in &self.per_module_level {
            let boundary = module.as_bytes().get(key.len()).copied();
            let matches = module.starts_with(key.as_str())
                && (boundary.is_none() || matches!(boundary, Some(b':' | b'.')));
            if matches && best.is_none_or(|(length, _)| key.len() > length) {
                best = Some((key.len(), *level));
            }
        }
        best.map_or(self.level, |(_, level)| level)
    }
}

/// 日志查询过滤。
#[derive(Debug, Clone, Default)]
pub struct LogQuery {
    /// 最低级别（含）。
    pub level_min: Option<LogLevel>,
    /// 模块（精确或前缀）。
    pub module: Option<String>,
    /// 链路 id。
    pub trace_id: Option<TraceId>,
    /// 任务 id。
    pub task_id: Option<TaskId>,
    /// 起始时间（含）。
    pub from: Option<Timestamp>,
    /// 结束时间（含）。
    pub to: Option<Timestamp>,
    /// 偏移。
    pub offset: usize,
    /// 条数（0 = 100）。
    pub limit: usize,
}

/// 分页结果。
#[derive(Debug, Clone, PartialEq)]
pub struct LogPage {
    /// 当前页记录（时间倒序）。
    pub records: Vec<LogRecord>,
    /// 命中总数。
    pub total: usize,
    /// 偏移。
    pub offset: usize,
    /// 本次条数上限。
    pub limit: usize,
}

/// 检索结果（索引开/关两种形态，显式提示）。
#[derive(Debug, Clone, PartialEq)]
pub enum LogSearchOutcome {
    /// 索引可用。
    Indexed {
        /// 命中。
        hits: Vec<LogIndexHit>,
        /// 索引状态说明。
        note: String,
    },
    /// 索引关闭/不可用：请用字段过滤（`LogQuery`）。
    Disabled {
        /// 中文提示。
        note: String,
    },
}

/// 索引状态（UI/诊断包）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogIndexState {
    /// 是否开启（配置）。
    pub enabled: bool,
    /// 是否可用（已打开索引文件）。
    pub available: bool,
    /// 已索引行数。
    pub indexed_records: u64,
    /// 中文说明。
    pub note: String,
}

/// 清理报告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupReport {
    /// 删除文件数。
    pub deleted_files: u64,
    /// 释放字节数。
    pub deleted_bytes: u64,
    /// 保留天数。
    pub retention_days: u32,
}

// ---------------------------------------------------------------------------
// 脱敏
// ---------------------------------------------------------------------------

/// 脱敏器。
#[derive(Debug, Clone)]
pub struct Redactor {
    enabled: bool,
    home: Option<String>,
}

impl Redactor {
    /// 按配置创建（家目录从 `HOME` 读取）。
    #[must_use]
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            home: std::env::var("HOME").ok().filter(|value| !value.is_empty()),
        }
    }

    /// 指定家目录（测试用）。
    #[must_use]
    pub fn with_home(enabled: bool, home: Option<String>) -> Self {
        Self { enabled, home }
    }

    /// 就地脱敏一条日志。
    pub fn redact(&self, record: &mut LogRecord) {
        if !self.enabled {
            return;
        }
        record.message = self.redact_text(&record.message);
        for (key, value) in &mut record.fields {
            let lower = key.to_ascii_lowercase();
            if is_secret_key(&lower) {
                "<已脱敏>".clone_into(value);
            } else if is_name_key(&lower) {
                *value = mask_name(value);
            } else if is_student_no_key(&lower) {
                *value = mask_student_no(value);
            } else {
                *value = self.redact_text(value);
            }
        }
    }

    /// 文本脱敏：家目录 → `~`；≥6 位连续数字（学号/手机号）→ `****` + 末 4 位。
    #[must_use]
    pub fn redact_text(&self, text: &str) -> String {
        let mut out = text.to_owned();
        if let Some(home) = &self.home
            && !home.is_empty()
        {
            out = out.replace(home, "~");
        }
        mask_digit_runs(&out, 6)
    }

    /// 是否启用。
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.enabled
    }
}

fn is_secret_key(key: &str) -> bool {
    [
        "secret",
        "token",
        "password",
        "passwd",
        "api_key",
        "apikey",
        "credential",
        "authorization",
        "private_key",
    ]
    .iter()
    .any(|needle| key.contains(needle))
}

fn is_name_key(key: &str) -> bool {
    matches!(key, "name" | "student_name" | "姓名" | "学生姓名")
        || key.ends_with("_name")
        || key.ends_with("姓名")
}

fn is_student_no_key(key: &str) -> bool {
    matches!(key, "student_no" | "studentno" | "学号" | "student_number") || key.ends_with("_no")
}

/// 姓名 → 首字符 + `**` + 短哈希（不可逆、可关联）。
fn mask_name(name: &str) -> String {
    let first = name.chars().next().unwrap_or('*');
    let digest = blake3::hash(name.as_bytes()).to_hex().to_string();
    format!("{first}**{}", &digest[..4])
}

/// 学号 → `****` + 末 4 位。
fn mask_student_no(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= 4 {
        return "****".to_owned();
    }
    let tail: String = trimmed
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("****{tail}")
}

/// 连续数字 ≥ `min_len` 的片段 → `****` + 末 4 位。
fn mask_digit_runs(text: &str, min_len: usize) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run: Vec<char> = Vec::new();
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            run.push(ch);
            continue;
        }
        flush_run(&mut run, &mut out, min_len);
        out.push(ch);
    }
    flush_run(&mut run, &mut out, min_len);
    out
}

fn flush_run(run: &mut Vec<char>, out: &mut String, min_len: usize) {
    if run.is_empty() {
        return;
    }
    if run.len() >= min_len {
        out.push_str("****");
        let tail_start = run.len().saturating_sub(4);
        for ch in &run[tail_start..] {
            out.push(*ch);
        }
    } else {
        out.extend(run.iter());
    }
    run.clear();
}

// ---------------------------------------------------------------------------
// 落盘
// ---------------------------------------------------------------------------

enum LogCommand {
    Write(Box<LogRecord>),
    Flush(SyncSender<()>),
    Shutdown,
}

/// 日志写入状态（当前文件、日期、序号、字节数）。
#[derive(Debug)]
struct LogWriter {
    file: Option<std::fs::File>,
    current_date: Option<String>,
    sequence: u32,
    bytes: u64,
    path: Option<PathBuf>,
}

impl LogWriter {
    fn new() -> Self {
        Self {
            file: None,
            current_date: None,
            sequence: 1,
            bytes: 0,
            path: None,
        }
    }
}

/// 日志落盘器（异步 + 错误级同步）。
pub struct LogSink {
    inner: Arc<Inner>,
}

struct Inner {
    log_dir: PathBuf,
    settings: LogSettings,
    clock: SharedClock,
    writer: Arc<Mutex<LogWriter>>,
    sender: SyncSender<LogCommand>,
    dropped: Arc<AtomicU64>,
    index: Option<Arc<LogIndex>>,
    join: Mutex<Option<JoinHandle<()>>>,
    closing: AtomicBool,
    redactor: Redactor,
}

impl Clone for LogSink {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl fmt::Debug for LogSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LogSink")
            .field("log_dir", &self.inner.log_dir)
            .field("settings", &self.inner.settings)
            .finish_non_exhaustive()
    }
}

impl LogSink {
    /// 创建日志落盘器（启动后台线程；目录不存在时创建）。
    ///
    /// # Errors
    ///
    /// 目录创建或线程启动失败时返回错误。
    pub fn new(
        log_dir: impl Into<PathBuf>,
        settings: LogSettings,
        clock: SharedClock,
        index: Option<Arc<LogIndex>>,
    ) -> Result<Self> {
        let log_dir = log_dir.into();
        std::fs::create_dir_all(&log_dir)
            .map_err(|source| PlatformError::io(log_dir.clone(), source))?;
        let (sender, receiver) = mpsc::sync_channel(LOG_QUEUE_DEPTH);
        let writer = Arc::new(Mutex::new(LogWriter::new()));
        let dropped = Arc::new(AtomicU64::new(0));
        let redactor = Redactor::new(settings.redaction);
        let thread_writer = Arc::clone(&writer);
        let thread_dropped = Arc::clone(&dropped);
        let thread_index = index.clone();
        let thread_dir = log_dir.clone();
        let thread_settings = settings.clone();
        let join = std::thread::Builder::new()
            .name("eg-log-writer".to_owned())
            .spawn(move || {
                while let Ok(command) = receiver.recv() {
                    match command {
                        LogCommand::Write(record) => {
                            if append_to(&thread_writer, &thread_dir, &thread_settings, &record)
                                .is_err()
                            {
                                thread_dropped.fetch_add(1, Ordering::Relaxed);
                            }
                            if let Some(index) = &thread_index
                                && index.add(&record).is_err()
                            {
                                thread_dropped.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        LogCommand::Flush(ack) => {
                            let _ = ack.send(());
                        }
                        LogCommand::Shutdown => break,
                    }
                }
            })
            .map_err(|source| PlatformError::io(log_dir.clone(), source))?;
        let inner = Inner {
            log_dir,
            settings,
            clock,
            writer,
            sender,
            dropped,
            index,
            join: Mutex::new(Some(join)),
            closing: AtomicBool::new(false),
            redactor,
        };
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// 写一条日志（级别过滤 + 脱敏；错误级同步落盘）。
    ///
    /// # Errors
    ///
    /// 错误级同步写盘失败时返回错误（调用方必须显式告警）；普通级别队列满时
    /// 不返回错误，只累计丢弃计数。
    pub fn write(&self, mut record: LogRecord) -> Result<()> {
        let threshold = self.inner.settings.level_for(&record.module);
        if !record.level.enabled_at(threshold) {
            return Ok(());
        }
        if self.inner.settings.redaction {
            self.inner.redactor.redact(&mut record);
        }
        if record.level == LogLevel::Error {
            append_to(
                &self.inner.writer,
                &self.inner.log_dir,
                &self.inner.settings,
                &record,
            )?;
            if let Some(index) = &self.inner.index {
                index.add(&record)?;
            }
            return Ok(());
        }
        match self
            .inner
            .sender
            .try_send(LogCommand::Write(Box::new(record)))
        {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                self.inner.dropped.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(TrySendError::Disconnected(_)) => Err(PlatformError::Internal(
                "日志后台线程不可用（已关闭）".to_owned(),
            )),
        }
    }

    /// 等待队列中的日志全部落盘。
    ///
    /// # Errors
    ///
    /// 线程不可用时返回错误。
    pub fn flush(&self) -> Result<()> {
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        self.inner
            .sender
            .send(LogCommand::Flush(ack_tx))
            .map_err(|_| PlatformError::Internal("日志后台线程不可用".to_owned()))?;
        ack_rx
            .recv()
            .map_err(|_| PlatformError::Internal("日志后台线程未确认".to_owned()))
    }

    /// 按字段查询（时间倒序；索引关闭时同样可用）。
    ///
    /// # Errors
    ///
    /// 读取日志文件或解析失败时返回错误。
    pub fn query(&self, query: &LogQuery) -> Result<LogPage> {
        let mut records = Vec::new();
        for file in self.log_files()? {
            for line in read_lines(&file)? {
                let Ok(record) = serde_json::from_str::<LogRecord>(&line) else {
                    continue;
                };
                if !matches_query(&record, query) {
                    continue;
                }
                records.push(record);
            }
        }
        records.sort_by_key(|record| std::cmp::Reverse(record.at));
        let total = records.len();
        let offset = query.offset.min(total);
        let limit = if query.limit == 0 { 100 } else { query.limit };
        let page = records.into_iter().skip(offset).take(limit).collect();
        Ok(LogPage {
            records: page,
            total,
            offset,
            limit,
        })
    }

    /// 全文检索（索引开启且可用时）；否则返回显式提示。
    ///
    /// # Errors
    ///
    /// 索引查询失败时返回错误。
    pub fn search(&self, keyword: &str, limit: usize, offset: usize) -> Result<LogSearchOutcome> {
        if !self.inner.settings.search_index {
            return Ok(LogSearchOutcome::Disabled {
                note:
                    "全文索引已关闭（log.search_index=false）：请用级别/模块/trace_id/时间字段过滤"
                        .to_owned(),
            });
        }
        match &self.inner.index {
            Some(index) => {
                let hits = index.search(keyword, limit, offset)?;
                Ok(LogSearchOutcome::Indexed {
                    hits,
                    note: format!(
                        "索引覆盖最近 {} 天；内容已脱敏",
                        self.inner.settings.index_range_days
                    ),
                })
            }
            None => Ok(LogSearchOutcome::Disabled {
                note: "索引未初始化：请重试或改用字段过滤".to_owned(),
            }),
        }
    }

    /// 按保留期清理日志（仅日志文件；审计与资产不动）。
    ///
    /// # Errors
    ///
    /// 读取目录或删除失败时返回错误。
    pub fn cleanup(&self) -> Result<CleanupReport> {
        let cutoff = self.inner.clock.now()
            - chrono::Duration::days(i64::from(self.inner.settings.retention_days));
        let mut deleted_files = 0;
        let mut deleted_bytes = 0;
        for file in self.log_files()? {
            let Some(date) = file_date(&file) else {
                continue;
            };
            if date >= cutoff {
                continue;
            }
            let bytes = std::fs::metadata(&file).map_or(0, |meta| meta.len());
            std::fs::remove_file(&file)
                .map_err(|source| PlatformError::io(file.clone(), source))?;
            deleted_files += 1;
            deleted_bytes += bytes;
        }
        Ok(CleanupReport {
            deleted_files,
            deleted_bytes,
            retention_days: self.inner.settings.retention_days,
        })
    }

    /// 已丢弃的日志条数（队列满/写盘失败）。
    #[must_use]
    pub fn dropped_total(&self) -> u64 {
        self.inner.dropped.load(Ordering::Relaxed)
    }

    /// 索引状态。
    ///
    /// # Errors
    ///
    /// 索引查询失败时返回错误。
    pub fn index_state(&self) -> Result<LogIndexState> {
        let enabled = self.inner.settings.search_index;
        match (&self.inner.index, enabled) {
            (Some(index), true) => {
                let state = index.state()?;
                Ok(LogIndexState {
                    enabled,
                    available: true,
                    indexed_records: state.indexed_records,
                    note: state.note,
                })
            }
            _ => Ok(LogIndexState {
                enabled,
                available: false,
                indexed_records: 0,
                note: "全文索引不可用：检索退化为字段过滤".to_owned(),
            }),
        }
    }

    /// 当前日志文件清单（升序）。
    ///
    /// # Errors
    ///
    /// 读取目录失败时返回错误。
    pub fn log_files(&self) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        let entries = match std::fs::read_dir(&self.inner.log_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(files),
            Err(source) => {
                return Err(PlatformError::io(self.inner.log_dir.clone(), source));
            }
        };
        for entry in entries {
            let entry =
                entry.map_err(|source| PlatformError::io(self.inner.log_dir.clone(), source))?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("app.") && name.ends_with(".jsonl") {
                files.push(entry.path());
            }
        }
        files.sort();
        Ok(files)
    }

    /// 日志目录。
    #[must_use]
    pub fn log_dir(&self) -> &Path {
        &self.inner.log_dir
    }

    /// 关闭后台线程（幂等）。
    ///
    /// # Errors
    ///
    /// 线程 join 失败时返回错误。
    pub fn close(&self) -> Result<()> {
        if self.inner.closing.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let _ = self.inner.sender.send(LogCommand::Shutdown);
        let handle = self
            .inner
            .join
            .lock()
            .map_err(|_| PlatformError::Internal("日志线程句柄锁中毒".to_owned()))?
            .take();
        if let Some(handle) = handle {
            handle
                .join()
                .map_err(|_| PlatformError::Internal("日志线程 panic".to_owned()))?;
        }
        Ok(())
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.closing.store(true, Ordering::SeqCst);
        let _ = self.sender.send(LogCommand::Shutdown);
        if let Ok(mut guard) = self.join.lock()
            && let Some(handle) = guard.take()
        {
            let _ = handle.join();
        }
    }
}

/// 追加一行到当前日志文件（必要时切换/轮转）。
fn append_to(
    writer: &Mutex<LogWriter>,
    log_dir: &Path,
    settings: &LogSettings,
    record: &LogRecord,
) -> Result<()> {
    let mut state = writer
        .lock()
        .map_err(|_| PlatformError::Internal("日志写入锁中毒".to_owned()))?;
    let date = record.at.format("%Y-%m-%d").to_string();
    let mut line =
        serde_json::to_vec(record).map_err(|error| PlatformError::Serialize(error.to_string()))?;
    line.push(b'\n');

    let date_changed = state.current_date.as_deref() != Some(date.as_str());
    let size_exceeded = state.bytes > 0
        && settings.rotation == LogRotation::Size50Mb
        && state.bytes + line.len() as u64 > settings.rotation_size_bytes;

    if date_changed || size_exceeded || state.file.is_none() {
        if date_changed {
            state.sequence = 1;
        } else if size_exceeded {
            state.sequence += 1;
        }
        let path = log_dir.join(format!("app.{date}.{:04}.jsonl", state.sequence));
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|source| PlatformError::io(path.clone(), source))?;
        state.file = Some(file);
        state.current_date = Some(date);
        state.bytes = 0;
        state.path = Some(path);
    }

    let path = state.path.clone().unwrap_or_default();
    let file = state
        .file
        .as_mut()
        .ok_or_else(|| PlatformError::Internal("日志文件未打开".to_owned()))?;
    file.write_all(&line)
        .map_err(|source| PlatformError::io(path.clone(), source))?;
    file.flush()
        .map_err(|source| PlatformError::io(path, source))?;
    state.bytes += line.len() as u64;
    Ok(())
}

/// 日志文件读取（内部工具）。
fn read_lines(path: &Path) -> Result<Vec<String>> {
    let text = std::fs::read_to_string(path)
        .map_err(|source| PlatformError::io(path.to_path_buf(), source))?;
    Ok(text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .collect())
}

/// 从文件名解析日期（`app.<YYYY-MM-DD>.<seq>.jsonl`）。
fn file_date(path: &Path) -> Option<Timestamp> {
    let name = path.file_name()?.to_string_lossy();
    let rest = name.strip_prefix("app.")?;
    let date = rest.split('.').next()?;
    let naive = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    let datetime = naive.and_hms_opt(0, 0, 0)?;
    Some(chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(
        datetime,
        chrono::Utc,
    ))
}

/// 查询过滤。
fn matches_query(record: &LogRecord, query: &LogQuery) -> bool {
    if let Some(min) = query.level_min
        && !record.level.enabled_at(min)
    {
        return false;
    }
    if let Some(module) = &query.module {
        let boundary = record.module.as_bytes().get(module.len()).copied();
        let matches = record.module == *module
            || (record.module.starts_with(module.as_str())
                && (boundary.is_none() || matches!(boundary, Some(b':' | b'.'))));
        if !matches {
            return false;
        }
    }
    if let Some(trace) = query.trace_id
        && record.trace_id != Some(trace)
    {
        return false;
    }
    if let Some(task) = query.task_id
        && record.task_id != Some(task)
    {
        return false;
    }
    if let Some(from) = query.from
        && record.at < from
    {
        return false;
    }
    if let Some(to) = query.to
        && record.at > to
    {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::clock::FixedClock;

    fn timestamp(text: &str) -> Timestamp {
        eg_domain::time::parse_rfc3339(text).unwrap()
    }

    fn setup(settings: LogSettings) -> (tempfile::TempDir, LogSink, Arc<FixedClock>) {
        let temp = tempfile::tempdir().unwrap();
        let clock = Arc::new(FixedClock::new(timestamp("2026-11-02T12:00:00Z")));
        let sink = LogSink::new(temp.path().join("logs"), settings, clock.clone(), None).unwrap();
        (temp, sink, clock)
    }

    fn read_all(sink: &LogSink) -> String {
        sink.flush().unwrap();
        let mut text = String::new();
        for file in sink.log_files().unwrap() {
            text.push_str(&std::fs::read_to_string(file).unwrap());
        }
        text
    }

    #[test]
    fn level_ordering_and_parsing() {
        assert!(LogLevel::Error > LogLevel::Warn);
        assert!(LogLevel::Info.enabled_at(LogLevel::Info));
        assert!(!LogLevel::Debug.enabled_at(LogLevel::Info));
        assert_eq!("INFO".parse::<LogLevel>().unwrap(), LogLevel::Info);
        assert!("verbose".parse::<LogLevel>().is_err());
        assert_eq!(serde_json::to_string(&LogLevel::Warn).unwrap(), "\"warn\"");
    }

    #[test]
    fn module_level_override_beats_global() {
        let mut settings = LogSettings {
            level: LogLevel::Info,
            ..LogSettings::default()
        };
        settings
            .per_module_level
            .insert("eg_recognize".to_owned(), LogLevel::Debug);
        assert_eq!(settings.level_for("eg_recognize"), LogLevel::Debug);
        assert_eq!(settings.level_for("eg_recognize::ocr"), LogLevel::Debug);
        assert_eq!(settings.level_for("eg_grade"), LogLevel::Info);
        // 前缀边界：`eg_recognize_extra` 不应命中 `eg_recognize`。
        assert_eq!(settings.level_for("eg_recognize_extra"), LogLevel::Info);
    }

    #[test]
    fn redactor_masks_pii() {
        let redactor = Redactor::with_home(true, Some("/Users/alice".to_owned()));
        let mut record = LogRecord::new(
            timestamp("2026-11-02T12:00:00Z"),
            LogLevel::Info,
            "eg_capture",
            "import.done",
            "导入 /Users/alice/Documents/张三.jpg 学号 2026001234 电话 13800138000",
        )
        .field("student_name", "张三")
        .field("student_no", "2026001234")
        .field("api_key", "sk-123456")
        .field("note", "普通字段 13800138000");
        redactor.redact(&mut record);
        assert!(!record.message.contains("alice"));
        assert!(!record.message.contains("2026001234"));
        assert!(record.message.contains("****1234"));
        assert!(!record.fields["student_name"].contains("张三"));
        assert!(record.fields["student_name"].starts_with("张**"));
        assert_eq!(record.fields["student_no"], "****1234");
        assert_eq!(record.fields["api_key"], "<已脱敏>");
        assert!(!record.fields["note"].contains("13800138000"));
        // 关闭脱敏时原样保留。
        let disabled = Redactor::with_home(false, Some("/Users/alice".to_owned()));
        let mut raw = LogRecord::new(
            timestamp("2026-11-02T12:00:00Z"),
            LogLevel::Info,
            "m",
            "e",
            "张三",
        );
        disabled.redact(&mut raw);
        assert_eq!(raw.message, "张三");
    }

    #[test]
    fn writes_jsonl_with_required_fields() {
        let (_temp, sink, _clock) = setup(LogSettings::default());
        let trace = TraceId::from_ulid(Ulid::from_u128(7));
        let task = TaskId::from_ulid(Ulid::from_u128(8));
        sink.write(
            LogRecord::new(
                timestamp("2026-11-02T12:00:00Z"),
                LogLevel::Info,
                "eg_recognize",
                "ocr.done",
                "识别完成",
            )
            .with_trace(trace)
            .with_task(task)
            .with_stage(Stage::Recognize)
            .with_latency_ms(123)
            .with_attempt(1)
            .with_asset_prefix("abcd1234")
            .field("pages", "2"),
        )
        .unwrap();
        let text = read_all(&sink);
        let line = text.lines().next().unwrap();
        let value: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(value["level"], "info");
        assert_eq!(value["module"], "eg_recognize");
        assert_eq!(value["event_code"], "ocr.done");
        assert_eq!(value["trace_id"], trace.to_string());
        assert_eq!(value["task_id"], task.to_string());
        assert_eq!(value["latency_ms"], 123);
        assert_eq!(value["pages"], "2");
        assert!(value["stage"].is_string());
    }

    #[test]
    fn global_and_module_levels_filter_writes() {
        let mut settings = LogSettings {
            level: LogLevel::Info,
            ..LogSettings::default()
        };
        settings
            .per_module_level
            .insert("eg_recognize".to_owned(), LogLevel::Debug);
        let (_temp, sink, _clock) = setup(settings);
        sink.write(LogRecord::new(
            timestamp("2026-11-02T12:00:00Z"),
            LogLevel::Debug,
            "eg_grade",
            "x",
            "应被丢弃（debug < info）",
        ))
        .unwrap();
        sink.write(LogRecord::new(
            timestamp("2026-11-02T12:00:01Z"),
            LogLevel::Debug,
            "eg_recognize",
            "x",
            "应写入（模块覆盖 debug）",
        ))
        .unwrap();
        let text = read_all(&sink);
        assert!(!text.contains("应被丢弃"));
        assert!(text.contains("应写入"));
    }

    #[test]
    fn written_logs_contain_no_pii() {
        let (_temp, sink, _clock) = setup(LogSettings::default());
        let home = std::env::var("HOME").unwrap_or_default();
        sink.write(
            LogRecord::new(
                timestamp("2026-11-02T12:00:00Z"),
                LogLevel::Info,
                "eg_capture",
                "import.done",
                format!("从 {home}/Documents 导入，学号 2026001234，电话 13800138000"),
            )
            .field("student_name", "张三")
            .field("student_no", "2026001234")
            .field("token", "sk-secret-value"),
        )
        .unwrap();
        let text = read_all(&sink);
        assert!(!text.contains("张三"), "姓名不得出现在日志");
        assert!(!text.contains("2026001234"), "学号不得完整出现");
        assert!(!text.contains("13800138000"), "手机号不得完整出现");
        assert!(!text.contains("sk-secret-value"), "密钥不得出现在日志");
        if !home.is_empty() {
            assert!(!text.contains(&home), "家目录用户名不得出现");
        }
    }

    #[test]
    fn daily_rotation_creates_new_file_per_day() {
        let (_temp, sink, _clock) = setup(LogSettings::default());
        sink.write(LogRecord::new(
            timestamp("2026-11-02T23:59:59Z"),
            LogLevel::Info,
            "m",
            "e",
            "第一天",
        ))
        .unwrap();
        sink.flush().unwrap();
        sink.write(LogRecord::new(
            timestamp("2026-11-03T00:00:01Z"),
            LogLevel::Info,
            "m",
            "e",
            "第二天",
        ))
        .unwrap();
        sink.flush().unwrap();
        let files = sink.log_files().unwrap();
        assert_eq!(files.len(), 2, "跨天应轮转：{files:?}");
        assert!(files[0].to_string_lossy().contains("2026-11-02"));
        assert!(files[1].to_string_lossy().contains("2026-11-03"));
    }

    #[test]
    fn size_rotation_splits_file() {
        let settings = LogSettings {
            rotation: LogRotation::Size50Mb,
            rotation_size_bytes: 120,
            ..LogSettings::default()
        };
        let (_temp, sink, _clock) = setup(settings);
        for index in 0..4 {
            sink.write(
                LogRecord::new(
                    timestamp("2026-11-02T12:00:00Z"),
                    LogLevel::Info,
                    "m",
                    "e",
                    format!("记录 {index}：这是一个较长的消息用于触发单文件轮转"),
                )
                .field("padding", "x".repeat(40)),
            )
            .unwrap();
        }
        sink.flush().unwrap();
        let files = sink.log_files().unwrap();
        assert!(files.len() >= 2, "超过阈值应切分文件：{files:?}");
        assert!(files[0].to_string_lossy().contains("0001"));
        assert!(files[1].to_string_lossy().contains("0002"));
    }

    #[test]
    fn retention_deletes_only_old_logs() {
        let (_temp, sink, clock) = setup(LogSettings::default());
        sink.write(LogRecord::new(
            timestamp("2026-11-02T12:00:00Z"),
            LogLevel::Info,
            "m",
            "e",
            "旧日志",
        ))
        .unwrap();
        sink.flush().unwrap();
        assert_eq!(sink.log_files().unwrap().len(), 1);
        // 未到期：不删除。
        let report = sink.cleanup().unwrap();
        assert_eq!(report.deleted_files, 0);
        // 推进 31 天：旧日志按保留期删除。
        clock.advance(chrono::Duration::days(31)).unwrap();
        let report = sink.cleanup().unwrap();
        assert_eq!(report.deleted_files, 1);
        assert_eq!(report.retention_days, 30);
        assert_eq!(sink.log_files().unwrap().len(), 0);
    }

    #[test]
    fn query_filters_and_paginates() {
        let (_temp, sink, _clock) = setup(LogSettings::default());
        let trace = TraceId::from_ulid(Ulid::from_u128(11));
        for index in 0..5 {
            sink.write(
                LogRecord::new(
                    timestamp(&format!("2026-11-02T12:00:0{index}Z")),
                    if index % 2 == 0 {
                        LogLevel::Info
                    } else {
                        LogLevel::Warn
                    },
                    if index < 3 { "eg_grade" } else { "eg_report" },
                    "e",
                    format!("第 {index} 条"),
                )
                .with_trace(trace),
            )
            .unwrap();
        }
        sink.flush().unwrap();
        let all = sink.query(&LogQuery::default()).unwrap();
        assert_eq!(all.total, 5);
        let warns = sink
            .query(&LogQuery {
                level_min: Some(LogLevel::Warn),
                ..LogQuery::default()
            })
            .unwrap();
        assert_eq!(warns.total, 2);
        let grade = sink
            .query(&LogQuery {
                module: Some("eg_grade".to_owned()),
                ..LogQuery::default()
            })
            .unwrap();
        assert_eq!(grade.total, 3);
        let page = sink
            .query(&LogQuery {
                offset: 1,
                limit: 2,
                ..LogQuery::default()
            })
            .unwrap();
        assert_eq!(page.records.len(), 2);
        assert_eq!(page.total, 5);
        let by_trace = sink
            .query(&LogQuery {
                trace_id: Some(trace),
                ..LogQuery::default()
            })
            .unwrap();
        assert_eq!(by_trace.total, 5);
    }

    #[test]
    fn search_uses_index_when_available() {
        let temp = tempfile::tempdir().unwrap();
        let index = Arc::new(LogIndex::open(&temp.path().join("index.sqlite3")).unwrap());
        let clock = Arc::new(FixedClock::new(timestamp("2026-11-02T12:00:00Z")));
        let sink = LogSink::new(
            temp.path().join("logs"),
            LogSettings::default(),
            clock,
            Some(index.clone()),
        )
        .unwrap();
        sink.write(LogRecord::new(
            timestamp("2026-11-02T12:00:00Z"),
            LogLevel::Info,
            "eg_recognize",
            "ocr.done",
            "识别到公式 x^2",
        ))
        .unwrap();
        sink.flush().unwrap();
        match sink.search("公式", 10, 0).unwrap() {
            LogSearchOutcome::Indexed { hits, note } => {
                assert_eq!(hits.len(), 1);
                assert_ne!(note, "");
            }
            LogSearchOutcome::Disabled { note } => panic!("索引应可用：{note}"),
        }
        assert!(sink.index_state().unwrap().available);
    }

    #[test]
    fn search_reports_disabled_when_index_off() {
        let settings = LogSettings {
            search_index: false,
            ..LogSettings::default()
        };
        let (_temp, sink, _clock) = setup(settings);
        match sink.search("任意", 10, 0).unwrap() {
            LogSearchOutcome::Disabled { note } => {
                assert!(note.contains("字段过滤"), "{note}");
            }
            LogSearchOutcome::Indexed { .. } => panic!("索引关闭时不应检索"),
        }
        let state = sink.index_state().unwrap();
        assert!(!state.enabled);
        assert!(!state.available);
    }

    #[test]
    fn error_level_is_written_synchronously() {
        let (_temp, sink, _clock) = setup(LogSettings::default());
        sink.write(
            LogRecord::new(
                timestamp("2026-11-02T12:00:00Z"),
                LogLevel::Error,
                "eg_platform",
                "db.corrupt",
                "数据库损坏",
            )
            .with_error_class(ErrorClass::DataCorruption),
        )
        .unwrap();
        // 不 flush 也能读到（错误级同步落盘）。
        let text: String = sink
            .log_files()
            .unwrap()
            .iter()
            .map(|file| std::fs::read_to_string(file).unwrap())
            .collect();
        assert!(text.contains("数据库损坏"));
        assert_eq!(sink.dropped_total(), 0);
    }
}
