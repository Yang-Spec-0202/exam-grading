//! 持久任务队列（`SPEC-platform.md` §4.3）。
//!
//! - 状态机：pending → running → succeeded；running 遇瞬态错误/租约过期 → pending（尝试 +1，退避）；
//!   永久错误或重试耗尽 → failed；pending/running 可取消 → cancelled；failed 可人工重试 → pending。
//! - 幂等键 = 任务类型 + 作用域 id + 输入哈希 + 版本三元组（BLAKE3）；唯一约束保证不重复执行。
//! - 租约：认领写所有者与到期时间；心跳 5s、租期 30s；重启后过期 running 归位 pending。
//! - 重试仅限瞬态错误，上限 3 次；退避 500ms 起指数增长 + 抖动，封顶 30s。
//! - 任务提交/重试/取消写审计（与业务同事务）。

use std::collections::BTreeMap;
use std::sync::Arc;

use eg_domain::{Stage, TaskId, Timestamp};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

use crate::audit::{AuditLog, NewAuditEntry};
use crate::clock::SharedClock;
use crate::db::Database;
use crate::error::{ErrorClass, PlatformError, Result};
use crate::ids::UlidFactory;

/// 租约时长（毫秒）：30s。
pub const LEASE_DURATION_MS: u64 = 30_000;

/// 心跳间隔（毫秒）：5s。
pub const HEARTBEAT_INTERVAL_MS: u64 = 5_000;

/// 默认最大尝试次数。
pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;

/// 退避基数（毫秒）。
pub const BACKOFF_BASE_MS: u64 = 500;

/// 退避封顶（毫秒）。
pub const BACKOFF_CAP_MS: u64 = 30_000;

/// 任务类型（`SPEC-platform.md` §4.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    /// 导入批次。
    ImportBatch,
    /// 页面预处理。
    PagePreprocess,
    /// 页面识别。
    PageRecognize,
    /// 提交对齐。
    SubmissionAlign,
    /// 规则整理。
    RubricStructure,
    /// 规则校验。
    RubricValidate,
    /// 提交判分。
    SubmissionGrade,
    /// 报表导出。
    ReportExport,
    /// 备份。
    Backup,
    /// 诊断包导出。
    DiagnosticsExport,
}

impl TaskKind {
    /// 稳定机器码。
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::ImportBatch => "import_batch",
            Self::PagePreprocess => "page_preprocess",
            Self::PageRecognize => "page_recognize",
            Self::SubmissionAlign => "submission_align",
            Self::RubricStructure => "rubric_structure",
            Self::RubricValidate => "rubric_validate",
            Self::SubmissionGrade => "submission_grade",
            Self::ReportExport => "report_export",
            Self::Backup => "backup",
            Self::DiagnosticsExport => "diagnostics_export",
        }
    }

    /// 中文标签。
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ImportBatch => "导入批次",
            Self::PagePreprocess => "页面预处理",
            Self::PageRecognize => "页面识别",
            Self::SubmissionAlign => "提交对齐",
            Self::RubricStructure => "规则整理",
            Self::RubricValidate => "规则校验",
            Self::SubmissionGrade => "提交判分",
            Self::ReportExport => "报表导出",
            Self::Backup => "备份",
            Self::DiagnosticsExport => "诊断包导出",
        }
    }

    /// 与 `domain::Stage` 的映射（规则整理/校验无对应阶段，返回 `None`）。
    #[must_use]
    pub const fn stage(self) -> Option<Stage> {
        match self {
            Self::ImportBatch => Some(Stage::Capture),
            Self::PagePreprocess => Some(Stage::Preprocess),
            Self::PageRecognize => Some(Stage::Recognize),
            Self::SubmissionAlign => Some(Stage::Align),
            Self::SubmissionGrade => Some(Stage::Grade),
            Self::ReportExport => Some(Stage::Export),
            Self::RubricStructure
            | Self::RubricValidate
            | Self::Backup
            | Self::DiagnosticsExport => None,
        }
    }
}

/// 任务状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    /// 待运行。
    Pending,
    /// 运行中。
    Running,
    /// 已成功（终态）。
    Succeeded,
    /// 失败（可人工重试）。
    Failed,
    /// 已取消（终态）。
    Cancelled,
}

impl TaskState {
    /// 稳定机器码。
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// 中文标签。
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Pending => "待运行",
            Self::Running => "运行中",
            Self::Succeeded => "已成功",
            Self::Failed => "失败",
            Self::Cancelled => "已取消",
        }
    }

    /// 是否终态。
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Cancelled)
    }

    /// 状态机允许的迁移（表驱动测试覆盖全部组合）。
    #[must_use]
    pub const fn can_transition(from: Self, to: Self) -> bool {
        matches!(
            (from, to),
            (Self::Pending, Self::Running | Self::Cancelled)
                | (
                    Self::Running,
                    Self::Succeeded | Self::Pending | Self::Failed | Self::Cancelled
                )
                | (Self::Failed, Self::Pending)
        )
    }

    /// 从机器码解析。
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "pending" => Some(Self::Pending),
            "running" => Some(Self::Running),
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// 一次尝试记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskAttempt {
    /// 第几次尝试（从 1 起）。
    pub attempt_no: u32,
    /// 开始时间。
    pub started_at: Timestamp,
    /// 结束时间（进行中为 `None`）。
    pub ended_at: Option<Timestamp>,
    /// 错误分类（失败时填）。
    pub error_class: Option<ErrorClass>,
    /// 错误说明。
    pub message: Option<String>,
}

/// 任务记录（含尝试明细）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskRecord {
    /// 任务 id。
    pub id: TaskId,
    /// 任务类型。
    pub kind: TaskKind,
    /// 状态。
    pub state: TaskState,
    /// 当前阶段（断点续跑用）。
    pub stage: Option<Stage>,
    /// 优先级（越大越先）。
    pub priority: i32,
    /// 幂等键。
    pub idempotency_key: String,
    /// 任务负载（JSON）。
    pub payload: serde_json::Value,
    /// 已尝试次数。
    pub attempts: u32,
    /// 最大尝试次数。
    pub max_attempts: u32,
    /// 下次可运行时间。
    pub next_run_at: Timestamp,
    /// 租约持有者。
    pub lease_owner: Option<String>,
    /// 租约到期时间。
    pub lease_expires_at: Option<Timestamp>,
    /// 最近一次错误分类。
    pub last_error_class: Option<ErrorClass>,
    /// 最近一次错误说明。
    pub last_error: Option<String>,
    /// 创建时间。
    pub created_at: Timestamp,
    /// 更新时间。
    pub updated_at: Timestamp,
    /// 尝试明细（按尝试号升序）。
    pub attempt_log: Vec<TaskAttempt>,
}

/// 版本三元组（幂等键的一部分）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskVersions {
    /// 答案键版本。
    pub key_version: u32,
    /// 规则版本。
    pub rubric_version: u32,
    /// 模型/提示词版本标识。
    pub model_version: String,
}

impl Default for TaskVersions {
    fn default() -> Self {
        Self {
            key_version: 0,
            rubric_version: 0,
            model_version: "none".to_owned(),
        }
    }
}

/// 新任务请求。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewTask {
    /// 任务类型。
    pub kind: TaskKind,
    /// 作用域 id 集合（如提交 id 列表；顺序无关）。
    pub scope: Vec<String>,
    /// 输入哈希集合（顺序无关）。
    pub input_hashes: Vec<String>,
    /// 版本三元组。
    pub versions: TaskVersions,
    /// 负载（JSON）。
    pub payload: serde_json::Value,
    /// 优先级（默认 0）。
    pub priority: i32,
    /// 最大尝试次数（默认 3）。
    pub max_attempts: u32,
}

impl NewTask {
    /// 构造最简任务。
    #[must_use]
    pub fn new(kind: TaskKind, payload: serde_json::Value) -> Self {
        Self {
            kind,
            scope: Vec::new(),
            input_hashes: Vec::new(),
            versions: TaskVersions::default(),
            payload,
            priority: 0,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
        }
    }

    /// 设置作用域。
    #[must_use]
    pub fn with_scope(mut self, scope: Vec<String>) -> Self {
        self.scope = scope;
        self
    }

    /// 设置输入哈希。
    #[must_use]
    pub fn with_input_hashes(mut self, hashes: Vec<String>) -> Self {
        self.input_hashes = hashes;
        self
    }

    /// 设置版本三元组。
    #[must_use]
    pub fn with_versions(mut self, versions: TaskVersions) -> Self {
        self.versions = versions;
        self
    }

    /// 设置优先级。
    #[must_use]
    pub fn with_priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }

    /// 设置最大尝试次数。
    #[must_use]
    pub fn with_max_attempts(mut self, attempts: u32) -> Self {
        self.max_attempts = attempts.max(1);
        self
    }

    /// 计算幂等键（类型 + 排序后的作用域/输入哈希 + 版本三元组 → BLAKE3）。
    #[must_use]
    pub fn idempotency_key(&self) -> String {
        let mut scope = self.scope.clone();
        scope.sort();
        scope.dedup();
        let mut hashes = self.input_hashes.clone();
        hashes.sort();
        hashes.dedup();
        let canonical = serde_json::json!({
            "kind": self.kind.code(),
            "scope": scope,
            "input_hashes": hashes,
            "key_version": self.versions.key_version,
            "rubric_version": self.versions.rubric_version,
            "model_version": self.versions.model_version,
        });
        let bytes = serde_json::to_vec(&canonical).unwrap_or_default();
        blake3::hash(&bytes).to_hex().to_string()
    }
}

/// 入队结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnqueueOutcome {
    /// 任务 id（新建或既有）。
    pub task_id: TaskId,
    /// 是否新建（false = 幂等命中既有任务）。
    pub inserted: bool,
}

/// 失败处理结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailOutcome {
    /// 是否进入重试（待运行）。
    pub retried: bool,
    /// 下次运行时间（不重试时为 `None`）。
    pub next_run_at: Option<Timestamp>,
    /// 最终状态。
    pub state: TaskState,
}

/// 任务查询过滤。
#[derive(Debug, Clone, Default)]
pub struct TaskFilter {
    /// 状态过滤。
    pub state: Option<TaskState>,
    /// 类型过滤。
    pub kind: Option<TaskKind>,
    /// 最大条数（0 = 100）。
    pub limit: usize,
}

/// 计算重试退避：500ms × 2^(attempt-1) × 抖动（0.8–1.2），封顶 30s。
#[must_use]
pub fn compute_retry_delay(attempt: u32, jitter_seed: u64) -> std::time::Duration {
    let exponent = attempt.saturating_sub(1).min(16);
    let base = BACKOFF_BASE_MS.saturating_mul(1u64 << exponent);
    let seed = jitter_seed ^ u64::from(attempt).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mixed = seed.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33;
    // 0.8–1.2 倍（800–1200 千分比）。
    let factor_per_mille = 800 + (mixed % 401);
    let jittered = base.saturating_mul(factor_per_mille) / 1_000;
    std::time::Duration::from_millis(jittered.min(BACKOFF_CAP_MS))
}

/// 持久任务队列。
#[derive(Clone)]
pub struct TaskQueue {
    db: Arc<Database>,
    clock: SharedClock,
    ids: Arc<UlidFactory>,
    audit: Arc<AuditLog>,
}

impl std::fmt::Debug for TaskQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskQueue").finish_non_exhaustive()
    }
}

impl TaskQueue {
    /// 构造任务队列（审计必需：任务提交/重试/取消都要留痕）。
    #[must_use]
    pub fn new(
        db: Arc<Database>,
        clock: SharedClock,
        ids: Arc<UlidFactory>,
        audit: Arc<AuditLog>,
    ) -> Self {
        Self {
            db,
            clock,
            ids,
            audit,
        }
    }

    /// 提交任务（幂等：同一幂等键返回既有任务，不重复执行）。
    ///
    /// # Errors
    ///
    /// id 生成、写入或审计失败时返回错误。
    pub fn enqueue(&self, task: NewTask) -> Result<EnqueueOutcome> {
        let now = self.clock.now();
        let id = self.ids.next_task_id()?;
        let key = task.idempotency_key();
        let queue = self.clone();
        self.db.write(move |tx| {
            let inserted = tx
                .execute(
                    "INSERT INTO task(id, kind, state, stage, priority, idempotency_key, payload, \
                     attempts, max_attempts, next_run_at, lease_owner, lease_expires_at, \
                     last_error_class, last_error, created_at, updated_at) \
                     VALUES (?1, ?2, 'pending', ?3, ?4, ?5, ?6, 0, ?7, ?8, NULL, NULL, NULL, NULL, ?9, ?9) \
                     ON CONFLICT(idempotency_key) DO NOTHING",
                    params![
                        id.to_string(),
                        task.kind.code(),
                        task.kind.stage().map(stage_code),
                        task.priority,
                        key,
                        serde_json::to_string(&task.payload)
                            .map_err(|error| PlatformError::Serialize(error.to_string()))?,
                        task.max_attempts,
                        format_timestamp(now),
                        format_timestamp(now)
                    ],
                )
                .map_err(PlatformError::from)?;
            let task_id = if inserted > 0 {
                id
            } else {
                let existing: String = tx
                    .query_row(
                        "SELECT id FROM task WHERE idempotency_key = ?1",
                        [&key],
                        |row| row.get(0),
                    )
                    .map_err(PlatformError::from)?;
                TaskId::parse(&existing)
                    .map_err(|error| PlatformError::DatabaseCorrupt(error.to_string()))?
            };
            if inserted > 0 {
                let entry = NewAuditEntry::new("task.enqueue", "task", task_id.to_string())
                    .with_note(format!("提交任务：{}", task.kind.label()))
                    .with_task(task_id);
                queue.audit.append_in_tx(tx, queue.ids.next_ulid()?, now, &entry)?;
            }
            Ok(EnqueueOutcome {
                task_id,
                inserted: inserted > 0,
            })
        })
    }

    /// 认领下一个可运行任务（按优先级降序、创建时间升序）。
    ///
    /// # Errors
    ///
    /// 事务或时间解析失败时返回错误。
    pub fn claim(&self, owner: &str) -> Result<Option<TaskRecord>> {
        let now = self.clock.now();
        let owner = owner.to_owned();
        self.db.write(move |tx| {
            let candidate: Option<(String, i64)> = tx
                .query_row(
                    "SELECT id, attempts FROM task WHERE state = 'pending' AND next_run_at <= ?1 \
                     ORDER BY priority DESC, created_at ASC LIMIT 1",
                    [format_timestamp(now)],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(PlatformError::from)?;
            let Some((id_text, attempts)) = candidate else {
                return Ok(None);
            };
            let task_id = TaskId::parse(&id_text)
                .map_err(|error| PlatformError::DatabaseCorrupt(error.to_string()))?;
            let attempt_no = u32::try_from(attempts).unwrap_or(0) + 1;
            let lease_expires = now
                + chrono::Duration::milliseconds(
                    i64::try_from(LEASE_DURATION_MS).unwrap_or(30_000),
                );
            let updated = tx
                .execute(
                    "UPDATE task SET state = 'running', attempts = attempts + 1, \
                     lease_owner = ?1, lease_expires_at = ?2, updated_at = ?3 \
                     WHERE id = ?4 AND state = 'pending'",
                    params![
                        owner,
                        format_timestamp(lease_expires),
                        format_timestamp(now),
                        task_id.to_string()
                    ],
                )
                .map_err(PlatformError::from)?;
            if updated == 0 {
                return Ok(None);
            }
            tx.execute(
                "INSERT INTO task_attempt(task_id, attempt_no, started_at) VALUES (?1, ?2, ?3)",
                params![task_id.to_string(), attempt_no, format_timestamp(now)],
            )
            .map_err(PlatformError::from)?;
            load_record(tx, task_id)
        })
    }

    /// 心跳：续租（仅限当前租约持有者）。
    ///
    /// # Errors
    ///
    /// 任务非运行中或所有者不匹配时返回 [`PlatformError::LeaseConflict`]。
    pub fn heartbeat(&self, id: TaskId, owner: &str) -> Result<()> {
        let now = self.clock.now();
        let owner = owner.to_owned();
        self.db.write(move |tx| {
            let current: Option<(String, Option<String>)> = tx
                .query_row(
                    "SELECT state, lease_owner FROM task WHERE id = ?1",
                    [id.to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(PlatformError::from)?;
            let Some((state, lease_owner)) = current else {
                return Err(PlatformError::NotFound(format!("任务不存在：{id}")));
            };
            if state != "running" {
                return Err(PlatformError::LeaseConflict(format!(
                    "任务 {id} 不是运行中（{state}）"
                )));
            }
            if lease_owner.as_deref() != Some(owner.as_str()) {
                return Err(PlatformError::LeaseConflict(format!(
                    "任务 {id} 租约不属于 {owner}（当前 {lease_owner:?}）"
                )));
            }
            let expires = now
                + chrono::Duration::milliseconds(
                    i64::try_from(LEASE_DURATION_MS).unwrap_or(30_000),
                );
            tx.execute(
                "UPDATE task SET lease_expires_at = ?1, updated_at = ?2 WHERE id = ?3",
                params![
                    format_timestamp(expires),
                    format_timestamp(now),
                    id.to_string()
                ],
            )
            .map_err(PlatformError::from)?;
            Ok(())
        })
    }

    /// 推进阶段（断点续跑：阶段产物落库后再提交下一阶段）。
    ///
    /// # Errors
    ///
    /// 任务不存在或非运行中时返回错误。
    pub fn advance_stage(&self, id: TaskId, stage: Stage) -> Result<()> {
        let now = self.clock.now();
        self.db.write(move |tx| {
            let state: Option<String> = tx
                .query_row(
                    "SELECT state FROM task WHERE id = ?1",
                    [id.to_string()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(PlatformError::from)?;
            match state.as_deref() {
                None => Err(PlatformError::NotFound(format!("任务不存在：{id}"))),
                Some("running") => {
                    tx.execute(
                        "UPDATE task SET stage = ?1, updated_at = ?2 WHERE id = ?3",
                        params![stage_code(stage), format_timestamp(now), id.to_string()],
                    )
                    .map_err(PlatformError::from)?;
                    Ok(())
                }
                Some(other) => Err(PlatformError::InvalidTransition {
                    from: other.to_owned(),
                    to: "stage".to_owned(),
                }),
            }
        })
    }

    /// 标记成功（running → succeeded，释放租约，结束当前尝试）。
    ///
    /// # Errors
    ///
    /// 非运行中或写入失败时返回错误。
    pub fn complete(&self, id: TaskId) -> Result<()> {
        let now = self.clock.now();
        let queue = self.clone();
        self.db.write(move |tx| {
            require_state(tx, id, TaskState::Running)?;
            tx.execute(
                "UPDATE task SET state = 'succeeded', stage = NULL, lease_owner = NULL, \
                 lease_expires_at = NULL, updated_at = ?1 WHERE id = ?2",
                params![format_timestamp(now), id.to_string()],
            )
            .map_err(PlatformError::from)?;
            tx.execute(
                "UPDATE task_attempt SET ended_at = ?1 WHERE task_id = ?2 AND ended_at IS NULL",
                params![format_timestamp(now), id.to_string()],
            )
            .map_err(PlatformError::from)?;
            let entry = NewAuditEntry::new("task.succeed", "task", id.to_string())
                .with_note("任务成功")
                .with_task(id);
            queue
                .audit
                .append_in_tx(tx, queue.ids.next_ulid()?, now, &entry)?;
            Ok(())
        })
    }

    /// 失败处理：瞬态且未耗尽 → 退避回待运行；否则 failure（终态「失败」）。
    ///
    /// # Errors
    ///
    /// 非运行中或写入失败时返回错误。
    pub fn fail(&self, id: TaskId, class: ErrorClass, message: &str) -> Result<FailOutcome> {
        let now = self.clock.now();
        let message = message.to_owned();
        let queue = self.clone();
        self.db.write(move |tx| {
            let record = load_record(tx, id)?
                .ok_or_else(|| PlatformError::NotFound(format!("任务不存在：{id}")))?;
            if record.state != TaskState::Running {
                return Err(PlatformError::InvalidTransition {
                    from: record.state.code().to_owned(),
                    to: TaskState::Failed.code().to_owned(),
                });
            }
            let retry = class.retryable() && record.attempts < record.max_attempts;
            if retry {
                let delay = compute_retry_delay(record.attempts, seed_of(id));
                let next_run = now + chrono::Duration::from_std(delay).unwrap_or_default();
                tx.execute(
                    "UPDATE task SET state = 'pending', lease_owner = NULL, lease_expires_at = NULL, \
                     next_run_at = ?1, last_error_class = ?2, last_error = ?3, updated_at = ?4 \
                     WHERE id = ?5",
                    params![
                        format_timestamp(next_run),
                        class.code(),
                        message,
                        format_timestamp(now),
                        id.to_string()
                    ],
                )
                .map_err(PlatformError::from)?;
                close_attempt(tx, id, class, &message, now)?;
                let entry = NewAuditEntry::new("task.retry", "task", id.to_string())
                    .with_note(format!(
                        "任务瞬态失败，将重试（第 {} 次尝试，{}ms 后）",
                        record.attempts,
                        delay.as_millis()
                    ))
                    .with_task(id);
                queue.audit.append_in_tx(tx, queue.ids.next_ulid()?, now, &entry)?;
                Ok(FailOutcome {
                    retried: true,
                    next_run_at: Some(next_run),
                    state: TaskState::Pending,
                })
            } else {
                tx.execute(
                    "UPDATE task SET state = 'failed', lease_owner = NULL, lease_expires_at = NULL, \
                     last_error_class = ?1, last_error = ?2, updated_at = ?3 WHERE id = ?4",
                    params![
                        class.code(),
                        message,
                        format_timestamp(now),
                        id.to_string()
                    ],
                )
                .map_err(PlatformError::from)?;
                close_attempt(tx, id, class, &message, now)?;
                let entry = NewAuditEntry::new("task.fail", "task", id.to_string())
                    .with_note(format!("任务失败（{class}）：{message}"))
                    .with_task(id);
                queue.audit.append_in_tx(tx, queue.ids.next_ulid()?, now, &entry)?;
                Ok(FailOutcome {
                    retried: false,
                    next_run_at: None,
                    state: TaskState::Failed,
                })
            }
        })
    }

    /// 取消任务（pending/running → cancelled；终态不可取消）。
    ///
    /// # Errors
    ///
    /// 任务不存在、已终态或写入失败时返回错误。
    pub fn cancel(&self, id: TaskId) -> Result<()> {
        let now = self.clock.now();
        let queue = self.clone();
        self.db.write(move |tx| {
            let record = load_record(tx, id)?
                .ok_or_else(|| PlatformError::NotFound(format!("任务不存在：{id}")))?;
            if record.state.is_terminal() {
                return Err(PlatformError::InvalidTransition {
                    from: record.state.code().to_owned(),
                    to: TaskState::Cancelled.code().to_owned(),
                });
            }
            tx.execute(
                "UPDATE task SET state = 'cancelled', lease_owner = NULL, lease_expires_at = NULL, \
                 updated_at = ?1 WHERE id = ?2",
                params![format_timestamp(now), id.to_string()],
            )
            .map_err(PlatformError::from)?;
            tx.execute(
                "UPDATE task_attempt SET ended_at = ?1 WHERE task_id = ?2 AND ended_at IS NULL",
                params![format_timestamp(now), id.to_string()],
            )
            .map_err(PlatformError::from)?;
            let entry = NewAuditEntry::new("task.cancel", "task", id.to_string())
                .with_note("任务取消（已产生的证据与派生资产保留）")
                .with_task(id);
            queue
                .audit
                .append_in_tx(tx, queue.ids.next_ulid()?, now, &entry)?;
            Ok(())
        })
    }

    /// 人工重试失败任务（failed → pending，立即可运行；尝试次数不重置）。
    ///
    /// # Errors
    ///
    /// 非「失败」状态或写入失败时返回错误。
    pub fn retry(&self, id: TaskId) -> Result<()> {
        let now = self.clock.now();
        let queue = self.clone();
        self.db.write(move |tx| {
            require_state(tx, id, TaskState::Failed)?;
            tx.execute(
                "UPDATE task SET state = 'pending', next_run_at = ?1, lease_owner = NULL, \
                 lease_expires_at = NULL, updated_at = ?1 WHERE id = ?2",
                params![format_timestamp(now), id.to_string()],
            )
            .map_err(PlatformError::from)?;
            let entry = NewAuditEntry::new("task.manual_retry", "task", id.to_string())
                .with_note("教师手动重试失败任务")
                .with_task(id);
            queue
                .audit
                .append_in_tx(tx, queue.ids.next_ulid()?, now, &entry)?;
            Ok(())
        })
    }

    /// 重启恢复：租约过期的 running 任务回待运行（尝试次数已在认领时 +1，此处不再加）。
    ///
    /// # Errors
    ///
    /// 查询或写入失败时返回错误。
    pub fn recover_expired(&self) -> Result<u64> {
        let now = self.clock.now();
        let queue = self.clone();
        self.db.write(move |tx| {
            let mut statement = tx
                .prepare(
                    "SELECT id, attempts, max_attempts FROM task WHERE state = 'running' \
                     AND lease_expires_at IS NOT NULL AND lease_expires_at <= ?1",
                )
                .map_err(PlatformError::from)?;
            let rows = statement
                .query_map([format_timestamp(now)], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })
                .map_err(PlatformError::from)?;
            let mut candidates = Vec::new();
            for row in rows {
                candidates.push(row.map_err(PlatformError::from)?);
            }
            drop(statement);
            let mut recovered = 0u64;
            for (id_text, attempts, max_attempts) in candidates {
                let task_id = TaskId::parse(&id_text)
                    .map_err(|error| PlatformError::DatabaseCorrupt(error.to_string()))?;
                let attempts = u32::try_from(attempts).unwrap_or(0);
                let max_attempts = u32::try_from(max_attempts).unwrap_or(DEFAULT_MAX_ATTEMPTS);
                let exhausted = attempts >= max_attempts;
                let next_run = now
                    + chrono::Duration::from_std(compute_retry_delay(
                        attempts.max(1),
                        seed_of(task_id),
                    ))
                    .unwrap_or_default();
                if exhausted {
                    tx.execute(
                        "UPDATE task SET state = 'failed', lease_owner = NULL, \
                         lease_expires_at = NULL, last_error_class = 'transient', \
                         last_error = '租约过期且重试耗尽', updated_at = ?1 WHERE id = ?2",
                        params![format_timestamp(now), task_id.to_string()],
                    )
                    .map_err(PlatformError::from)?;
                } else {
                    tx.execute(
                        "UPDATE task SET state = 'pending', lease_owner = NULL, \
                         lease_expires_at = NULL, next_run_at = ?1, last_error_class = 'transient', \
                         last_error = '租约过期（进程崩溃/强杀）', updated_at = ?2 WHERE id = ?3",
                        params![
                            format_timestamp(next_run),
                            format_timestamp(now),
                            task_id.to_string()
                        ],
                    )
                    .map_err(PlatformError::from)?;
                }
                tx.execute(
                    "UPDATE task_attempt SET ended_at = ?1, error_class = 'transient', \
                     message = '租约过期（进程崩溃/强杀）' WHERE task_id = ?2 AND ended_at IS NULL",
                    params![format_timestamp(now), task_id.to_string()],
                )
                .map_err(PlatformError::from)?;
                let entry = NewAuditEntry::new("task.recover", "task", task_id.to_string())
                    .with_note("启动恢复：租约过期的运行中任务归位")
                    .with_task(task_id);
                queue.audit.append_in_tx(tx, queue.ids.next_ulid()?, now, &entry)?;
                recovered += 1;
            }
            Ok(recovered)
        })
    }

    /// 读取任务（含尝试明细）。
    ///
    /// # Errors
    ///
    /// 查询或解析失败时返回错误。
    pub fn get(&self, id: TaskId) -> Result<Option<TaskRecord>> {
        self.db.read(|conn| load_record_conn(conn, id))
    }

    /// 按条件列出任务（创建时间倒序）。
    ///
    /// # Errors
    ///
    /// 查询或解析失败时返回错误。
    pub fn list(&self, filter: &TaskFilter) -> Result<Vec<TaskRecord>> {
        let limit = if filter.limit == 0 { 100 } else { filter.limit };
        self.db.read(|conn| {
            let mut statement = conn
                .prepare(
                    "SELECT id FROM task WHERE (?1 IS NULL OR state = ?1) \
                     AND (?2 IS NULL OR kind = ?2) ORDER BY created_at DESC LIMIT ?3",
                )
                .map_err(PlatformError::from)?;
            let rows = statement
                .query_map(
                    params![
                        filter.state.map(TaskState::code),
                        filter.kind.map(TaskKind::code),
                        i64::try_from(limit).unwrap_or(i64::MAX)
                    ],
                    |row| row.get::<_, String>(0),
                )
                .map_err(PlatformError::from)?;
            let mut out = Vec::new();
            for row in rows {
                let id_text = row.map_err(PlatformError::from)?;
                let id = TaskId::parse(&id_text)
                    .map_err(|error| PlatformError::DatabaseCorrupt(error.to_string()))?;
                if let Some(record) = load_record_conn(conn, id)? {
                    out.push(record);
                }
            }
            Ok(out)
        })
    }

    /// 各状态计数（诊断包/UI 用）。
    ///
    /// # Errors
    ///
    /// 查询失败时返回错误。
    pub fn count_by_state(&self) -> Result<BTreeMap<TaskState, u64>> {
        self.db.read(|conn| {
            let mut statement = conn
                .prepare("SELECT state, COUNT(*) FROM task GROUP BY state")
                .map_err(PlatformError::from)?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })
                .map_err(PlatformError::from)?;
            let mut out = BTreeMap::new();
            for row in rows {
                let (state, count) = row.map_err(PlatformError::from)?;
                if let Some(state) = TaskState::from_code(&state) {
                    out.insert(state, u64::try_from(count).unwrap_or(0));
                }
            }
            Ok(out)
        })
    }

    /// 待运行任务数（队列深度）。
    ///
    /// # Errors
    ///
    /// 查询失败时返回错误。
    pub fn pending_count(&self) -> Result<u64> {
        self.db.read(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM task WHERE state = 'pending'",
                [],
                |row| row.get(0),
            )
            .map_err(PlatformError::from)
            .map(|count: i64| u64::try_from(count).unwrap_or(0))
        })
    }

    /// 尝试明细。
    ///
    /// # Errors
    ///
    /// 查询失败时返回错误。
    pub fn attempts(&self, id: TaskId) -> Result<Vec<TaskAttempt>> {
        self.db.read(|conn| load_attempts(conn, id))
    }
}

/// 时间 → 文本（RFC3339，带毫秒）。
fn format_timestamp(value: Timestamp) -> String {
    eg_domain::time::format_rfc3339(value)
}

/// 解析时间列。
fn parse_timestamp(text: &str, column: &str) -> rusqlite::Result<Timestamp> {
    eg_domain::time::parse_rfc3339(text).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{column} 时间非法：{error}"),
            )),
        )
    })
}

/// 阶段 → 机器码（不含 JSON 引号）。
fn stage_code(stage: Stage) -> String {
    serde_json::to_value(stage)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

/// 阶段机器码 → 枚举。
fn parse_stage(text: &str) -> Option<Stage> {
    serde_json::from_value::<Stage>(serde_json::Value::String(text.to_owned())).ok()
}

/// 任务 id → 退避抖动种子。
fn seed_of(id: TaskId) -> u64 {
    let bytes = id.as_ulid().as_u128().to_le_bytes();
    u64::from_le_bytes(bytes[..8].try_into().unwrap_or([0u8; 8]))
}

/// 校验任务处于指定状态。
fn require_state(tx: &Transaction<'_>, id: TaskId, expected: TaskState) -> Result<()> {
    let state: Option<String> = tx
        .query_row(
            "SELECT state FROM task WHERE id = ?1",
            [id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(PlatformError::from)?;
    match state {
        None => Err(PlatformError::NotFound(format!("任务不存在：{id}"))),
        Some(text) if text == expected.code() => Ok(()),
        Some(text) => Err(PlatformError::InvalidTransition {
            from: text,
            to: expected.code().to_owned(),
        }),
    }
}

/// 结束当前尝试（写错误分类与消息）。
fn close_attempt(
    tx: &Transaction<'_>,
    id: TaskId,
    class: ErrorClass,
    message: &str,
    now: Timestamp,
) -> Result<()> {
    tx.execute(
        "UPDATE task_attempt SET ended_at = ?1, error_class = ?2, message = ?3 \
         WHERE task_id = ?4 AND ended_at IS NULL",
        params![format_timestamp(now), class.code(), message, id.to_string()],
    )
    .map_err(PlatformError::from)?;
    Ok(())
}

type TaskRow = (
    String,
    String,
    String,
    Option<String>,
    i64,
    String,
    String,
    i64,
    i64,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    String,
);

/// 读取任务记录（事务/连接通用：经 `Connection`）。
fn load_record_conn(conn: &rusqlite::Connection, id: TaskId) -> Result<Option<TaskRecord>> {
    let row: Option<TaskRow> = conn
        .query_row(
            "SELECT id, kind, state, stage, priority, idempotency_key, payload, attempts, \
             max_attempts, next_run_at, lease_owner, lease_expires_at, last_error_class, \
             last_error, created_at, updated_at FROM task WHERE id = ?1",
            [id.to_string()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                    row.get(12)?,
                    row.get(13)?,
                    row.get(14)?,
                    row.get(15)?,
                ))
            },
        )
        .optional()
        .map_err(PlatformError::from)?;
    let Some((
        id_text,
        kind_text,
        state_text,
        stage_text,
        priority,
        key,
        payload_text,
        attempts,
        max_attempts,
        next_run_at,
        lease_owner,
        lease_expires_at,
        last_error_class,
        last_error,
        created_at,
        updated_at,
    )) = row
    else {
        return Ok(None);
    };
    let kind = task_kind_from_code(&kind_text)
        .ok_or_else(|| PlatformError::DatabaseCorrupt(format!("未知任务类型：{kind_text}")))?;
    let state = TaskState::from_code(&state_text)
        .ok_or_else(|| PlatformError::DatabaseCorrupt(format!("未知任务状态：{state_text}")))?;
    let attempt_log = load_attempts(conn, id)?;
    Ok(Some(TaskRecord {
        id: TaskId::parse(&id_text)
            .map_err(|error| PlatformError::DatabaseCorrupt(error.to_string()))?,
        kind,
        state,
        stage: stage_text.as_deref().and_then(parse_stage),
        priority: i32::try_from(priority).unwrap_or(0),
        idempotency_key: key,
        payload: serde_json::from_str(&payload_text)
            .map_err(|error| PlatformError::DatabaseCorrupt(error.to_string()))?,
        attempts: u32::try_from(attempts).unwrap_or(0),
        max_attempts: u32::try_from(max_attempts).unwrap_or(DEFAULT_MAX_ATTEMPTS),
        next_run_at: parse_timestamp(&next_run_at, "next_run_at").map_err(PlatformError::from)?,
        lease_owner,
        lease_expires_at: lease_expires_at
            .as_deref()
            .map(|text| parse_timestamp(text, "lease_expires_at"))
            .transpose()
            .map_err(PlatformError::from)?,
        last_error_class: last_error_class.as_deref().and_then(ErrorClass::from_code),
        last_error,
        created_at: parse_timestamp(&created_at, "created_at").map_err(PlatformError::from)?,
        updated_at: parse_timestamp(&updated_at, "updated_at").map_err(PlatformError::from)?,
        attempt_log,
    }))
}

/// 事务内读取任务记录。
fn load_record(tx: &Transaction<'_>, id: TaskId) -> Result<Option<TaskRecord>> {
    load_record_conn(tx, id)
}

/// 读取尝试明细。
fn load_attempts(conn: &rusqlite::Connection, id: TaskId) -> Result<Vec<TaskAttempt>> {
    let mut statement = conn
        .prepare(
            "SELECT attempt_no, started_at, ended_at, error_class, message FROM task_attempt \
             WHERE task_id = ?1 ORDER BY attempt_no",
        )
        .map_err(PlatformError::from)?;
    let rows = statement
        .query_map([id.to_string()], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })
        .map_err(PlatformError::from)?;
    let mut out = Vec::new();
    for row in rows {
        let (attempt_no, started, ended, class, message) = row.map_err(PlatformError::from)?;
        out.push(TaskAttempt {
            attempt_no: u32::try_from(attempt_no).unwrap_or(0),
            started_at: parse_timestamp(&started, "started_at").map_err(PlatformError::from)?,
            ended_at: ended
                .as_deref()
                .map(|text| parse_timestamp(text, "ended_at"))
                .transpose()
                .map_err(PlatformError::from)?,
            error_class: class.as_deref().and_then(ErrorClass::from_code),
            message,
        });
    }
    Ok(out)
}

/// 任务类型机器码 → 枚举。
fn task_kind_from_code(code: &str) -> Option<TaskKind> {
    Some(match code {
        "import_batch" => TaskKind::ImportBatch,
        "page_preprocess" => TaskKind::PagePreprocess,
        "page_recognize" => TaskKind::PageRecognize,
        "submission_align" => TaskKind::SubmissionAlign,
        "rubric_structure" => TaskKind::RubricStructure,
        "rubric_validate" => TaskKind::RubricValidate,
        "submission_grade" => TaskKind::SubmissionGrade,
        "report_export" => TaskKind::ReportExport,
        "backup" => TaskKind::Backup,
        "diagnostics_export" => TaskKind::DiagnosticsExport,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::clock::FixedClock;
    use chrono::Duration;
    use eg_domain::Clock as _;
    use eg_domain::Ulid;
    use std::time::Duration as StdDuration;

    struct Harness {
        _temp: tempfile::TempDir,
        queue: TaskQueue,
        clock: Arc<FixedClock>,
        audit: Arc<AuditLog>,
    }

    fn harness() -> Harness {
        let temp = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open(&temp.path().join("db.sqlite3")).unwrap());
        let clock = Arc::new(FixedClock::from_rfc3339("2026-11-02T12:00:00Z").unwrap());
        let clock_dyn: SharedClock = clock.clone();
        let ids = Arc::new(UlidFactory::new(clock_dyn.clone()));
        let audit = Arc::new(AuditLog::new(db.clone(), clock_dyn.clone(), ids.clone()));
        let queue = TaskQueue::new(db, clock_dyn, ids, audit.clone());
        Harness {
            _temp: temp,
            queue,
            clock,
            audit,
        }
    }

    fn sample_task(kind: TaskKind) -> NewTask {
        NewTask::new(kind, serde_json::json!({"pages": 1}))
    }

    #[test]
    fn state_machine_transition_table() {
        let states = [
            TaskState::Pending,
            TaskState::Running,
            TaskState::Succeeded,
            TaskState::Failed,
            TaskState::Cancelled,
        ];
        let allowed = [
            (TaskState::Pending, TaskState::Running),
            (TaskState::Pending, TaskState::Cancelled),
            (TaskState::Running, TaskState::Succeeded),
            (TaskState::Running, TaskState::Pending),
            (TaskState::Running, TaskState::Failed),
            (TaskState::Running, TaskState::Cancelled),
            (TaskState::Failed, TaskState::Pending),
        ];
        for from in states {
            for to in states {
                let expected = allowed.contains(&(from, to));
                assert_eq!(
                    TaskState::can_transition(from, to),
                    expected,
                    "{from:?} -> {to:?}"
                );
            }
        }
        for state in states {
            assert_eq!(
                state.is_terminal(),
                matches!(state, TaskState::Succeeded | TaskState::Cancelled)
            );
            assert_eq!(TaskState::from_code(state.code()), Some(state));
        }
    }

    #[test]
    fn retry_backoff_grows_and_is_capped() {
        let first = compute_retry_delay(1, 42);
        let second = compute_retry_delay(2, 42);
        assert!(first >= StdDuration::from_millis(400) && first <= StdDuration::from_millis(600));
        assert!(second > first, "退避应指数增长");
        for attempt in 1..=20 {
            let delay = compute_retry_delay(attempt, 7);
            assert!(delay >= StdDuration::from_millis(400), "下界");
            assert!(
                delay <= StdDuration::from_millis(BACKOFF_CAP_MS),
                "封顶 30s"
            );
        }
        // 抖动：同 attempt 不同种子不保证不同，但必须在范围内。
        let a = compute_retry_delay(3, 1);
        let b = compute_retry_delay(3, 2);
        assert!(a >= StdDuration::from_millis(1_600) && a <= StdDuration::from_millis(2_400));
        assert!(b >= StdDuration::from_millis(1_600) && b <= StdDuration::from_millis(2_400));
    }

    #[test]
    fn enqueue_is_idempotent_by_key() {
        let h = harness();
        let task = sample_task(TaskKind::PageRecognize)
            .with_scope(vec!["page-1".to_owned()])
            .with_input_hashes(vec!["abc".to_owned()]);
        let key1 = task.idempotency_key();
        let key2 = task.clone().idempotency_key();
        assert_eq!(key1, key2);

        let first = h.queue.enqueue(task.clone()).unwrap();
        assert!(first.inserted);
        let second = h.queue.enqueue(task).unwrap();
        assert!(!second.inserted, "幂等键命中不应重复插入");
        assert_eq!(first.task_id, second.task_id);
        assert_eq!(h.queue.pending_count().unwrap(), 1);
        // 不同版本三元组 → 不同任务
        let with_version = sample_task(TaskKind::PageRecognize)
            .with_scope(vec!["page-1".to_owned()])
            .with_versions(TaskVersions {
                key_version: 2,
                rubric_version: 2,
                model_version: "ocr-v2".to_owned(),
            });
        assert_ne!(
            first.task_id,
            h.queue.enqueue(with_version).unwrap().task_id
        );
    }

    #[test]
    fn claim_respects_priority_and_sets_lease() {
        let h = harness();
        let low = h
            .queue
            .enqueue(sample_task(TaskKind::Backup).with_priority(0))
            .unwrap()
            .task_id;
        let high = h
            .queue
            .enqueue(sample_task(TaskKind::SubmissionGrade).with_priority(10))
            .unwrap()
            .task_id;
        let claimed = h.queue.claim("worker-1").unwrap().expect("应能认领");
        assert_eq!(claimed.id, high, "高优先级先认领");
        assert_eq!(claimed.state, TaskState::Running);
        assert_eq!(claimed.lease_owner.as_deref(), Some("worker-1"));
        assert_eq!(claimed.attempts, 1);
        assert_eq!(claimed.attempt_log.len(), 1);
        let second = h.queue.claim("worker-2").unwrap().expect("第二任务");
        assert_eq!(second.id, low);
        assert!(h.queue.claim("worker-3").unwrap().is_none());
    }

    #[test]
    fn heartbeat_requires_lease_owner() {
        let h = harness();
        let id = h
            .queue
            .enqueue(sample_task(TaskKind::PagePreprocess))
            .unwrap()
            .task_id;
        h.queue.claim("worker-1").unwrap().unwrap();
        h.queue.heartbeat(id, "worker-1").unwrap();
        let error = h.queue.heartbeat(id, "worker-2").unwrap_err();
        assert!(
            matches!(error, PlatformError::LeaseConflict(_)),
            "{error:?}"
        );
        h.clock.advance(Duration::seconds(10)).unwrap();
        h.queue.heartbeat(id, "worker-1").unwrap();
    }

    #[test]
    fn complete_moves_to_succeeded_and_releases_lease() {
        let h = harness();
        let id = h
            .queue
            .enqueue(sample_task(TaskKind::PageRecognize))
            .unwrap()
            .task_id;
        h.queue.claim("worker-1").unwrap().unwrap();
        h.queue.advance_stage(id, Stage::Recognize).unwrap();
        h.queue.complete(id).unwrap();
        let record = h.queue.get(id).unwrap().unwrap();
        assert_eq!(record.state, TaskState::Succeeded);
        assert!(record.lease_owner.is_none());
        assert_eq!(record.stage, None);
        assert!(record.attempt_log[0].ended_at.is_some());
        // 终态不能再成功/认领
        assert!(h.queue.complete(id).is_err());
    }

    #[test]
    fn transient_failure_backs_off_then_fails_after_max_attempts() {
        let h = harness();
        let id = h
            .queue
            .enqueue(sample_task(TaskKind::PageRecognize).with_max_attempts(2))
            .unwrap()
            .task_id;
        h.queue.claim("worker").unwrap().unwrap();
        let first = h.queue.fail(id, ErrorClass::Transient, "数据库忙").unwrap();
        assert!(first.retried);
        let next = first.next_run_at.expect("应安排重试");
        assert!(next > h.clock.now());
        let record = h.queue.get(id).unwrap().unwrap();
        assert_eq!(record.state, TaskState::Pending);
        assert_eq!(record.last_error_class, Some(ErrorClass::Transient));

        // 推进到下次运行时间后再次认领（attempts=2）。
        h.clock.advance(Duration::milliseconds(40_000)).unwrap();
        h.queue.claim("worker").unwrap().unwrap();
        let second = h.queue.fail(id, ErrorClass::Transient, "再次超时").unwrap();
        assert!(!second.retried, "达到上限后不再重试");
        assert_eq!(second.state, TaskState::Failed);
        let record = h.queue.get(id).unwrap().unwrap();
        assert_eq!(record.state, TaskState::Failed);
        assert_eq!(record.attempts, 2);
    }

    #[test]
    fn permanent_failure_does_not_retry() {
        let h = harness();
        let id = h
            .queue
            .enqueue(sample_task(TaskKind::RubricStructure))
            .unwrap()
            .task_id;
        h.queue.claim("worker").unwrap().unwrap();
        let outcome = h
            .queue
            .fail(id, ErrorClass::Permanent, "Schema 不合法")
            .unwrap();
        assert!(!outcome.retried);
        assert_eq!(outcome.state, TaskState::Failed);
    }

    #[test]
    fn manual_retry_moves_failed_back_to_pending() {
        let h = harness();
        let id = h
            .queue
            .enqueue(sample_task(TaskKind::ReportExport))
            .unwrap()
            .task_id;
        h.queue.claim("worker").unwrap().unwrap();
        h.queue
            .fail(id, ErrorClass::PolicyBlocked, "授权无效")
            .unwrap();
        assert_eq!(h.queue.get(id).unwrap().unwrap().state, TaskState::Failed);
        h.queue.retry(id).unwrap();
        assert_eq!(h.queue.get(id).unwrap().unwrap().state, TaskState::Pending);
        assert!(h.queue.retry(id).is_err(), "非失败状态不可重试");
    }

    #[test]
    fn cancel_pending_and_running_but_not_terminal() {
        let h = harness();
        let pending = h
            .queue
            .enqueue(sample_task(TaskKind::Backup))
            .unwrap()
            .task_id;
        h.queue.cancel(pending).unwrap();
        assert_eq!(
            h.queue.get(pending).unwrap().unwrap().state,
            TaskState::Cancelled
        );
        assert!(h.queue.cancel(pending).is_err(), "终态不可取消");

        let running = h
            .queue
            .enqueue(sample_task(TaskKind::DiagnosticsExport))
            .unwrap()
            .task_id;
        h.queue.claim("worker").unwrap().unwrap();
        h.queue.cancel(running).unwrap();
        let record = h.queue.get(running).unwrap().unwrap();
        assert_eq!(record.state, TaskState::Cancelled);
        assert!(record.attempt_log[0].ended_at.is_some());
    }

    #[test]
    fn recover_expired_requeues_running_tasks() {
        let h = harness();
        let id = h
            .queue
            .enqueue(sample_task(TaskKind::PageRecognize))
            .unwrap()
            .task_id;
        h.queue.claim("crashed-worker").unwrap().unwrap();
        // 模拟进程被强杀：租约到期（30s）后时钟前进 31s。
        h.clock.advance(Duration::seconds(31)).unwrap();
        let recovered = h.queue.recover_expired().unwrap();
        assert_eq!(recovered, 1);
        let record = h.queue.get(id).unwrap().unwrap();
        assert_eq!(record.state, TaskState::Pending);
        assert!(record.lease_owner.is_none());
        assert!(record.last_error.unwrap_or_default().contains("租约过期"));
        // 恢复后可再次认领（尝试次数 +1 = 2）。
        let claimed = h.queue.claim("new-worker").unwrap().unwrap();
        assert_eq!(claimed.id, id);
        assert_eq!(claimed.attempts, 2);
        // 再次恢复不应重复处理。
        assert_eq!(h.queue.recover_expired().unwrap(), 0);
    }

    #[test]
    fn list_filter_and_counts() {
        let h = harness();
        for kind in [TaskKind::Backup, TaskKind::ReportExport] {
            h.queue.enqueue(sample_task(kind)).unwrap();
        }
        let all = h.queue.list(&TaskFilter::default()).unwrap();
        assert_eq!(all.len(), 2);
        let backups = h
            .queue
            .list(&TaskFilter {
                kind: Some(TaskKind::Backup),
                ..TaskFilter::default()
            })
            .unwrap();
        assert_eq!(backups.len(), 1);
        let counts = h.queue.count_by_state().unwrap();
        assert_eq!(counts.get(&TaskState::Pending), Some(&2));
        assert_eq!(h.queue.pending_count().unwrap(), 2);
    }

    #[test]
    fn task_actions_are_audited() {
        let h = harness();
        let id = h
            .queue
            .enqueue(sample_task(TaskKind::PageRecognize))
            .unwrap()
            .task_id;
        h.queue.claim("worker").unwrap().unwrap();
        h.queue.fail(id, ErrorClass::Transient, "网络抖动").unwrap();
        h.queue.cancel(id).unwrap();
        assert!(h.audit.count().unwrap() >= 3, "提交/重试/取消都应审计");
        assert!(h.audit.verify_chain().unwrap().ok);
    }

    #[test]
    fn invalid_transitions_are_rejected() {
        let h = harness();
        let id = h
            .queue
            .enqueue(sample_task(TaskKind::Backup))
            .unwrap()
            .task_id;
        // pending 直接完成 → 非法
        assert!(h.queue.complete(id).is_err());
        // 不存在任务
        assert!(
            h.queue
                .get(TaskId::from_ulid(Ulid::from_u128(999)))
                .unwrap()
                .is_none()
        );
    }
}
