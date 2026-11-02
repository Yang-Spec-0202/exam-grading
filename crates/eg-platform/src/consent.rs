//! 任务级云端授权记录（`SPEC-platform.md` §4.4）。
//!
//! - 登记：教师选定任务范围与后端 → 生成 `ConsentId` 并关联 `task_id`，记录范围哈希与预算；
//! - 校验：任务运行期每次云端调用前查「该任务是否有有效授权」，并核对范围哈希；
//! - 失效：任务到终态（成功/失败/取消）授权自动结束；续跑/新任务需重新授权；
//! - 撤销：写审计并置撤销状态（历史记录永久保留）。
//! - 额度：`spent_micros` 累计花费；达到 `budget_micros` 时置「额度用尽」
//!   （`exhausted`），重启后仍不可用；`end_for_task` 不覆盖用尽原因。
//!
//! 说明：本模块只负责「是否允许外发」的事实查询；授权语义（哪些数据可外发）由
//! `inference` 用 `scope_hash` 校验，本模块不解析范围内容。

use std::sync::Arc;

use eg_domain::{ConsentId, Sha256Hex, TaskId, Timestamp};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::clock::SharedClock;
use crate::db::Database;
use crate::error::{PlatformError, Result};
use crate::ids::UlidFactory;

/// 授权状态（`SPEC-platform.md` §4.4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsentState {
    /// 有效。
    Active,
    /// 任务终态自动结束。
    Ended,
    /// 教师撤销。
    Revoked,
    /// 额度用尽（预算耗尽；重启后仍不可用）。
    Exhausted,
}

impl ConsentState {
    /// 稳定机器码。
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Ended => "ended",
            Self::Revoked => "revoked",
            Self::Exhausted => "exhausted",
        }
    }

    /// 中文标签。
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Active => "有效",
            Self::Ended => "已结束",
            Self::Revoked => "已撤销",
            Self::Exhausted => "额度用尽",
        }
    }

    /// 从机器码解析。
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "active" => Some(Self::Active),
            "ended" => Some(Self::Ended),
            "revoked" => Some(Self::Revoked),
            "exhausted" => Some(Self::Exhausted),
            _ => None,
        }
    }
}

/// 云端授权持久化记录（与 `inference::TaskConsent` 以 `consent_id` 关联）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskConsentRecord {
    /// 授权 id。
    pub consent_id: ConsentId,
    /// 关联任务 id。
    pub task_id: TaskId,
    /// 后端标识（如 `openai_compat`/后端名）。
    pub backend: String,
    /// 允许外发的数据范围哈希（哪些资产/文本）。
    pub scope_hash: Sha256Hex,
    /// 预算上限（百万分之一货币单位；0 = 本地/无费用）。
    pub budget_micros: u64,
    /// 状态。
    pub state: ConsentState,
    /// 登记时间。
    pub granted_at: Timestamp,
    /// 结束时间（终态/撤销时填；额度用尽不填此项，另记 `exhausted_at`）。
    pub ended_at: Option<Timestamp>,
    /// 已花费（百万分之一货币单位；0 预算 = 本地/无费用，不做用尽判定）。
    pub spent_micros: u64,
    /// 额度用尽时间（仅 `state == Exhausted` 时填写）。
    pub exhausted_at: Option<Timestamp>,
}

impl TaskConsentRecord {
    /// 剩余预算（微货币单位）；`budget_micros == 0`（本地/无费用）返回 `None`。
    #[must_use]
    pub const fn remaining_micros(&self) -> Option<u64> {
        if self.budget_micros == 0 {
            None
        } else {
            Some(self.budget_micros.saturating_sub(self.spent_micros))
        }
    }

    /// 是否仍可用于云端调用（仅 `Active` 且未超预算）。
    #[must_use]
    pub const fn is_usable(&self) -> bool {
        matches!(self.state, ConsentState::Active)
    }
}

/// 授权存储。
#[derive(Clone)]
pub struct ConsentStore {
    db: Arc<Database>,
    clock: SharedClock,
    ids: Arc<UlidFactory>,
}

impl std::fmt::Debug for ConsentStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConsentStore").finish_non_exhaustive()
    }
}

impl ConsentStore {
    /// 构造授权存储。
    #[must_use]
    pub fn new(db: Arc<Database>, clock: SharedClock, ids: Arc<UlidFactory>) -> Self {
        Self { db, clock, ids }
    }

    /// 登记一次任务级授权（任务必须已存在）。
    ///
    /// # Errors
    ///
    /// 任务不存在（外键）或写入失败时返回错误。
    pub fn register(
        &self,
        task_id: TaskId,
        backend: &str,
        scope_hash: Sha256Hex,
        budget_micros: u64,
    ) -> Result<TaskConsentRecord> {
        if backend.trim().is_empty() {
            return Err(PlatformError::InvalidArgument(
                "授权后端的标识不能为空".to_owned(),
            ));
        }
        let consent_id = self.ids.next_consent_id()?;
        let now = self.clock.now();
        let backend = backend.to_owned();
        let record = TaskConsentRecord {
            consent_id,
            task_id,
            backend: backend.clone(),
            scope_hash,
            budget_micros,
            state: ConsentState::Active,
            granted_at: now,
            ended_at: None,
            spent_micros: 0,
            exhausted_at: None,
        };
        let stored = record.clone();
        self.db.write(move |tx| {
            tx.execute(
                "INSERT INTO task_consent(consent_id, task_id, backend, scope_hash, \
                 budget_micros, state, granted_at, ended_at, spent_micros, exhausted_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, 0, NULL)",
                params![
                    record.consent_id.to_string(),
                    record.task_id.to_string(),
                    record.backend,
                    record.scope_hash.to_hex(),
                    i64::try_from(record.budget_micros).unwrap_or(i64::MAX),
                    record.state.code(),
                    format_timestamp(record.granted_at)
                ],
            )
            .map_err(PlatformError::from)?;
            Ok(())
        })?;
        Ok(stored)
    }

    /// 查任务当前有效授权（`None` = 无有效授权）。
    ///
    /// # Errors
    ///
    /// 查询或解析失败时返回错误。
    pub fn is_valid(&self, task_id: TaskId) -> Result<Option<TaskConsentRecord>> {
        self.db.read(|conn| find_active(conn, task_id))
    }

    /// 校验一次云端调用的授权与范围（`SPEC-platform.md` §5「范围外请求 100% 拒绝」）。
    ///
    /// # Errors
    ///
    /// 无有效授权、已结束/撤销、范围哈希不匹配时返回
    /// [`PlatformError::PolicyBlocked`]。
    pub fn validate_call(
        &self,
        task_id: TaskId,
        scope_hash: Sha256Hex,
    ) -> Result<TaskConsentRecord> {
        let Some(record) = self.is_valid(task_id)? else {
            return Err(PlatformError::PolicyBlocked(format!(
                "任务 {task_id} 没有有效的云端授权（按任务鉴权恒开）"
            )));
        };
        if record.scope_hash != scope_hash {
            return Err(PlatformError::PolicyBlocked(
                "请求外发的数据不在授权范围内（范围哈希不匹配），已硬拒绝".to_owned(),
            ));
        }
        // 兜底：即使状态尚未翻转为 exhausted（如进程在记账中途崩溃），
        // 已达预算上限也不得继续外发。
        if record.remaining_micros() == Some(0) {
            return Err(PlatformError::PolicyBlocked(format!(
                "任务 {task_id} 的云端授权额度已用尽（预算 {} 微单位），已硬拒绝",
                record.budget_micros
            )));
        }
        Ok(record)
    }

    /// 任务到终态时结束其全部有效授权（幂等）。
    ///
    /// 只把 `state = 'active'` 改为 `ended`；已「额度用尽」的记录保持
    /// `exhausted`（保留用尽原因，`exhausted_at` 不丢），已结束/撤销的不变。
    ///
    /// # Errors
    ///
    /// 写入失败时返回错误。
    pub fn end_for_task(&self, task_id: TaskId) -> Result<u64> {
        let now = self.clock.now();
        self.db.write(move |tx| {
            let changed = tx
                .execute(
                    "UPDATE task_consent SET state = 'ended', ended_at = ?1 \
                     WHERE task_id = ?2 AND state = 'active'",
                    params![format_timestamp(now), task_id.to_string()],
                )
                .map_err(PlatformError::from)?;
            Ok(u64::try_from(changed).unwrap_or(0))
        })
    }

    /// 将任务的**有效**授权置为「额度用尽」（幂等）。
    ///
    /// 只更新 `state = 'active'` 的记录；已结束/已撤销/已用尽的不变。返回更新条数。
    /// 用尽状态持久化，重启后 `is_valid`/`validate_call` 仍拒绝。
    ///
    /// # Errors
    ///
    /// 写入失败时返回错误。
    pub fn mark_exhausted(&self, task_id: TaskId) -> Result<u64> {
        let now = self.clock.now();
        self.db.write(move |tx| {
            let changed = tx
                .execute(
                    "UPDATE task_consent SET state = 'exhausted', exhausted_at = ?1 \
                     WHERE task_id = ?2 AND state = 'active'",
                    params![format_timestamp(now), task_id.to_string()],
                )
                .map_err(PlatformError::from)?;
            Ok(u64::try_from(changed).unwrap_or(0))
        })
    }

    /// 记录一次云端调用花费（余额更新）；达到预算上限时自动置「额度用尽」。
    ///
    /// `delta_micros` 为本次增量（饱和累加，避免溢出）；返回更新后的记录。
    /// `budget_micros == 0`（本地/无费用）永不置用尽，但仍累计花费供诊断。
    /// 已结束/已撤销的授权拒绝继续记账。
    ///
    /// # Errors
    ///
    /// 授权不存在、已结束/撤销或写入失败时返回错误。
    pub fn record_spend(
        &self,
        consent_id: ConsentId,
        delta_micros: u64,
    ) -> Result<TaskConsentRecord> {
        let now = self.clock.now();
        self.db.write(move |tx| {
            let row = read_raw_by_id(tx, consent_id)?
                .ok_or_else(|| PlatformError::NotFound(format!("授权不存在：{consent_id}")))?;
            let record = decode_row(&row)?;
            if matches!(record.state, ConsentState::Ended | ConsentState::Revoked) {
                return Err(PlatformError::PolicyBlocked(format!(
                    "授权 {consent_id} 已结束/撤销，拒绝继续记录额度"
                )));
            }
            let spent = record.spent_micros.saturating_add(delta_micros);
            let exhausted = record.budget_micros > 0
                && spent >= record.budget_micros
                && matches!(record.state, ConsentState::Active);
            let spent_db = i64::try_from(spent).unwrap_or(i64::MAX);
            if exhausted {
                tx.execute(
                    "UPDATE task_consent SET spent_micros = ?1, state = 'exhausted', \
                     exhausted_at = ?2 WHERE consent_id = ?3",
                    params![spent_db, format_timestamp(now), consent_id.to_string()],
                )
                .map_err(PlatformError::from)?;
            } else {
                tx.execute(
                    "UPDATE task_consent SET spent_micros = ?1 WHERE consent_id = ?2",
                    params![spent_db, consent_id.to_string()],
                )
                .map_err(PlatformError::from)?;
            }
            let row = read_raw_by_id(tx, consent_id)?
                .ok_or_else(|| PlatformError::Internal("额度更新后授权记录消失".to_owned()))?;
            decode_row(&row)
        })
    }

    /// 任务当前是否存在「额度用尽」记录（不受是否随后结束影响）。
    ///
    /// # Errors
    ///
    /// 查询失败时返回错误。
    pub fn is_exhausted(&self, task_id: TaskId) -> Result<bool> {
        self.db.read(|conn| {
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM task_consent                  WHERE task_id = ?1 AND state = 'exhausted')",
                [task_id.to_string()],
                |row| row.get(0),
            )
            .map_err(PlatformError::from)
        })
    }

    /// 撤销授权（写审计动作由调用方负责；历史记录保留，状态置撤销）。
    ///
    /// # Errors
    ///
    /// 授权不存在时返回 [`PlatformError::NotFound`]。
    pub fn revoke(&self, consent_id: ConsentId) -> Result<()> {
        let now = self.clock.now();
        self.db.write(move |tx| {
            let changed = tx
                .execute(
                    "UPDATE task_consent SET state = 'revoked', ended_at = ?1 \
                     WHERE consent_id = ?2 AND state != 'revoked'",
                    params![format_timestamp(now), consent_id.to_string()],
                )
                .map_err(PlatformError::from)?;
            if changed == 0 {
                return Err(PlatformError::NotFound(format!(
                    "授权不存在或已撤销：{consent_id}"
                )));
            }
            Ok(())
        })
    }

    /// 读取单条授权记录。
    ///
    /// # Errors
    ///
    /// 查询或解析失败时返回错误。
    pub fn get(&self, consent_id: ConsentId) -> Result<Option<TaskConsentRecord>> {
        self.db.read(|conn| {
            let row: Option<RawRow> = conn
                .query_row(
                    "SELECT consent_id, task_id, backend, scope_hash, budget_micros, state, \
                     granted_at, ended_at, spent_micros, exhausted_at \
                     FROM task_consent WHERE consent_id = ?1",
                    [consent_id.to_string()],
                    raw_row,
                )
                .optional()
                .map_err(PlatformError::from)?;
            row.map(|row| decode_row(&row)).transpose()
        })
    }

    /// 列出某任务的全部授权记录（时间倒序，审计与 UI 用）。
    ///
    /// # Errors
    ///
    /// 查询或解析失败时返回错误。
    pub fn list_by_task(&self, task_id: TaskId) -> Result<Vec<TaskConsentRecord>> {
        self.db.read(|conn| {
            let mut statement = conn
                .prepare(
                    "SELECT consent_id, task_id, backend, scope_hash, budget_micros, state, \
                     granted_at, ended_at, spent_micros, exhausted_at \
                     FROM task_consent WHERE task_id = ?1 ORDER BY granted_at DESC",
                )
                .map_err(PlatformError::from)?;
            let rows = statement
                .query_map([task_id.to_string()], raw_row)
                .map_err(PlatformError::from)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(decode_row(&row.map_err(PlatformError::from)?)?);
            }
            Ok(out)
        })
    }

    /// 结束/撤销计数（诊断包用）。
    ///
    /// # Errors
    ///
    /// 查询失败时返回错误。
    pub fn count_by_state(&self) -> Result<Vec<(ConsentState, u64)>> {
        self.db.read(|conn| {
            let mut statement = conn
                .prepare("SELECT state, COUNT(*) FROM task_consent GROUP BY state")
                .map_err(PlatformError::from)?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })
                .map_err(PlatformError::from)?;
            let mut out = Vec::new();
            for row in rows {
                let (state, count) = row.map_err(PlatformError::from)?;
                if let Some(state) = ConsentState::from_code(&state) {
                    out.push((state, u64::try_from(count).unwrap_or(0)));
                }
            }
            Ok(out)
        })
    }
}

fn format_timestamp(value: Timestamp) -> String {
    eg_domain::time::format_rfc3339(value)
}

fn parse_timestamp(text: &str, column: &str) -> Result<Timestamp> {
    eg_domain::time::parse_rfc3339(text)
        .map_err(|error| PlatformError::DatabaseCorrupt(format!("{column} 时间非法：{error}")))
}

type RawRow = (
    String,
    String,
    String,
    String,
    i64,
    String,
    String,
    Option<String>,
    i64,
    Option<String>,
);

fn decode_row(row: &RawRow) -> Result<TaskConsentRecord> {
    let (consent, task, backend, scope, budget, state, granted, ended, spent, exhausted_at) = row;
    Ok(TaskConsentRecord {
        consent_id: ConsentId::parse(consent)
            .map_err(|error| PlatformError::DatabaseCorrupt(error.to_string()))?,
        task_id: TaskId::parse(task)
            .map_err(|error| PlatformError::DatabaseCorrupt(error.to_string()))?,
        backend: backend.clone(),
        scope_hash: Sha256Hex::from_hex(scope)
            .map_err(|error| PlatformError::DatabaseCorrupt(error.to_string()))?,
        budget_micros: u64::try_from(*budget).unwrap_or(0),
        state: ConsentState::from_code(state)
            .ok_or_else(|| PlatformError::DatabaseCorrupt(format!("未知授权状态：{state}")))?,
        granted_at: parse_timestamp(granted, "granted_at")?,
        ended_at: ended
            .as_deref()
            .map(|text| parse_timestamp(text, "ended_at"))
            .transpose()?,
        spent_micros: u64::try_from(*spent).unwrap_or(0),
        exhausted_at: exhausted_at
            .as_deref()
            .map(|text| parse_timestamp(text, "exhausted_at"))
            .transpose()?,
    })
}

/// 按 id 读取原始行（写事务内使用；`&Transaction` 经 `Deref` 传给 `&Connection`）。
fn read_raw_by_id(conn: &Connection, consent_id: ConsentId) -> Result<Option<RawRow>> {
    conn.query_row(
        "SELECT consent_id, task_id, backend, scope_hash, budget_micros, state, granted_at, \
         ended_at, spent_micros, exhausted_at FROM task_consent WHERE consent_id = ?1",
        [consent_id.to_string()],
        raw_row,
    )
    .optional()
    .map_err(PlatformError::from)
}

/// 行映射（10 列顺序固定）。
fn raw_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawRow> {
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
    ))
}

/// 查询任务当前有效授权（活动且未结束）。
fn find_active(conn: &Connection, task_id: TaskId) -> Result<Option<TaskConsentRecord>> {
    let row: Option<RawRow> = conn
        .query_row(
            "SELECT consent_id, task_id, backend, scope_hash, budget_micros, state, granted_at, \
             ended_at, spent_micros, exhausted_at FROM task_consent \
             WHERE task_id = ?1 AND state = 'active' AND ended_at IS NULL \
               AND (budget_micros = 0 OR spent_micros < budget_micros) \
             ORDER BY granted_at DESC LIMIT 1",
            [task_id.to_string()],
            raw_row,
        )
        .optional()
        .map_err(PlatformError::from)?;
    row.as_ref().map(decode_row).transpose()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::audit::AuditLog;
    use crate::clock::FixedClock;
    use crate::tasks::{NewTask, TaskKind, TaskQueue};
    use eg_domain::Ulid;

    struct Harness {
        _temp: tempfile::TempDir,
        consents: ConsentStore,
        tasks: TaskQueue,
    }

    fn harness() -> Harness {
        let temp = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open(&temp.path().join("db.sqlite3")).unwrap());
        let clock: SharedClock =
            Arc::new(FixedClock::from_rfc3339("2026-11-02T12:00:00Z").unwrap());
        let ids = Arc::new(UlidFactory::new(clock.clone()));
        let audit = Arc::new(AuditLog::new(db.clone(), clock.clone(), ids.clone()));
        let tasks = TaskQueue::new(db.clone(), clock.clone(), ids.clone(), audit);
        let consents = ConsentStore::new(db, clock, ids);
        Harness {
            _temp: temp,
            consents,
            tasks,
        }
    }

    fn new_task(tasks: &TaskQueue) -> TaskId {
        tasks
            .enqueue(NewTask::new(
                TaskKind::SubmissionGrade,
                serde_json::json!({"submission": "s1"}),
            ))
            .unwrap()
            .task_id
    }

    #[test]
    fn register_makes_call_valid() {
        let h = harness();
        let task = new_task(&h.tasks);
        let scope = Sha256Hex::of(b"assets:a,b texts:t1");
        let record = h
            .consents
            .register(task, "openai_compat", scope, 1_000)
            .unwrap();
        assert_eq!(record.state, ConsentState::Active);
        let valid = h.consents.is_valid(task).unwrap().expect("应有有效授权");
        assert_eq!(valid.consent_id, record.consent_id);
        let checked = h.consents.validate_call(task, scope).unwrap();
        assert_eq!(checked.consent_id, record.consent_id);
    }

    #[test]
    fn outside_scope_is_hard_rejected() {
        let h = harness();
        let task = new_task(&h.tasks);
        let scope = Sha256Hex::of(b"in-scope");
        h.consents
            .register(task, "openai_compat", scope, 0)
            .unwrap();
        let error = h
            .consents
            .validate_call(task, Sha256Hex::of(b"out-of-scope"))
            .unwrap_err();
        assert_eq!(error.class(), crate::error::ErrorClass::PolicyBlocked);
        assert!(error.to_string().contains("范围哈希不匹配"));
    }

    #[test]
    fn missing_consent_is_rejected() {
        let h = harness();
        let task = new_task(&h.tasks);
        let error = h
            .consents
            .validate_call(task, Sha256Hex::of(b"x"))
            .unwrap_err();
        assert!(error.to_string().contains("没有有效的云端授权"));
    }

    #[test]
    fn task_end_invalidates_consent() {
        let h = harness();
        let task = new_task(&h.tasks);
        h.consents
            .register(task, "openai_compat", Sha256Hex::of(b"s"), 0)
            .unwrap();
        assert_eq!(h.consents.end_for_task(task).unwrap(), 1);
        assert!(h.consents.is_valid(task).unwrap().is_none());
        let records = h.consents.list_by_task(task).unwrap();
        assert_eq!(records[0].state, ConsentState::Ended);
        assert!(records[0].ended_at.is_some());
        // 幂等
        assert_eq!(h.consents.end_for_task(task).unwrap(), 0);
    }

    #[test]
    fn revoke_invalidates_and_keeps_history() {
        let h = harness();
        let task = new_task(&h.tasks);
        let record = h
            .consents
            .register(task, "openai_compat", Sha256Hex::of(b"s"), 0)
            .unwrap();
        h.consents.revoke(record.consent_id).unwrap();
        assert!(h.consents.is_valid(task).unwrap().is_none());
        let stored = h.consents.get(record.consent_id).unwrap().unwrap();
        assert_eq!(stored.state, ConsentState::Revoked);
        assert!(stored.ended_at.is_some());
        assert!(h.consents.revoke(record.consent_id).is_err());
    }

    #[test]
    fn counts_and_invalid_backend() {
        let h = harness();
        let task = new_task(&h.tasks);
        assert!(
            h.consents
                .register(task, "  ", Sha256Hex::of(b"s"), 0)
                .is_err()
        );
        h.consents
            .register(task, "openai_compat", Sha256Hex::of(b"s"), 0)
            .unwrap();
        let counts = h.consents.count_by_state().unwrap();
        assert_eq!(counts, vec![(ConsentState::Active, 1)]);
        assert!(
            h.consents
                .get(ConsentId::from_ulid(Ulid::from_u128(42)))
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn record_spend_accumulates_and_auto_exhausts() {
        let h = harness();
        let task = new_task(&h.tasks);
        let record = h
            .consents
            .register(task, "openai_compat", Sha256Hex::of(b"s"), 100)
            .unwrap();
        assert_eq!(record.spent_micros, 0);
        assert_eq!(record.remaining_micros(), Some(100));

        let after_first = h.consents.record_spend(record.consent_id, 40).unwrap();
        assert_eq!(after_first.state, ConsentState::Active);
        assert_eq!(after_first.spent_micros, 40);
        assert_eq!(after_first.remaining_micros(), Some(60));
        assert_eq!(
            h.consents
                .validate_call(task, Sha256Hex::of(b"s"))
                .unwrap()
                .consent_id,
            record.consent_id
        );

        let exhausted = h.consents.record_spend(record.consent_id, 60).unwrap();
        assert_eq!(exhausted.state, ConsentState::Exhausted);
        assert_eq!(exhausted.spent_micros, 100);
        assert_eq!(exhausted.remaining_micros(), Some(0));
        assert!(exhausted.exhausted_at.is_some());
        assert!(h.consents.is_valid(task).unwrap().is_none());
        assert!(h.consents.is_exhausted(task).unwrap());
        let error = h
            .consents
            .validate_call(task, Sha256Hex::of(b"s"))
            .unwrap_err();
        assert_eq!(error.class(), crate::error::ErrorClass::PolicyBlocked);
        // 重启前状态已落库；计数含 exhausted。
        let counts = h.consents.count_by_state().unwrap();
        assert!(counts.contains(&(ConsentState::Exhausted, 1)));
        // 幂等：再次记账只累计，不重复改状态。
        let again = h.consents.record_spend(record.consent_id, 5).unwrap();
        assert_eq!(again.state, ConsentState::Exhausted);
        assert_eq!(again.spent_micros, 105);
    }

    #[test]
    fn budget_zero_never_exhausts_and_has_no_remaining() {
        let h = harness();
        let task = new_task(&h.tasks);
        let record = h
            .consents
            .register(task, "openai_compat", Sha256Hex::of(b"s"), 0)
            .unwrap();
        assert_eq!(record.remaining_micros(), None);
        let updated = h
            .consents
            .record_spend(record.consent_id, u64::MAX)
            .unwrap();
        assert_eq!(updated.state, ConsentState::Active);
        assert_eq!(updated.remaining_micros(), None);
        assert!(h.consents.is_valid(task).unwrap().is_some());
    }

    #[test]
    fn ended_consent_rejects_further_spending() {
        let h = harness();
        let task = new_task(&h.tasks);
        let record = h
            .consents
            .register(task, "openai_compat", Sha256Hex::of(b"s"), 100)
            .unwrap();
        h.consents.end_for_task(task).unwrap();
        let error = h.consents.record_spend(record.consent_id, 1).unwrap_err();
        assert_eq!(error.class(), crate::error::ErrorClass::PolicyBlocked);
    }

    #[test]
    fn end_for_task_keeps_exhausted_reason() {
        let h = harness();
        let task = new_task(&h.tasks);
        let record = h
            .consents
            .register(task, "openai_compat", Sha256Hex::of(b"s"), 10)
            .unwrap();
        h.consents.record_spend(record.consent_id, 10).unwrap();
        assert_eq!(
            h.consents.end_for_task(task).unwrap(),
            0,
            "exhausted 不由终态覆盖"
        );
        let stored = h.consents.get(record.consent_id).unwrap().unwrap();
        assert_eq!(stored.state, ConsentState::Exhausted);
        assert!(stored.exhausted_at.is_some());
        assert!(stored.ended_at.is_none());
    }

    #[test]
    fn exhausted_persists_across_restart() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("db.sqlite3");
        let (task, consent_id);
        {
            let db = Arc::new(Database::open(&path).unwrap());
            let clock: SharedClock =
                Arc::new(FixedClock::from_rfc3339("2026-11-02T12:00:00Z").unwrap());
            let ids = Arc::new(UlidFactory::new(clock.clone()));
            let audit = Arc::new(AuditLog::new(db.clone(), clock.clone(), ids.clone()));
            let tasks = TaskQueue::new(db.clone(), clock.clone(), ids.clone(), audit);
            let consents = ConsentStore::new(db.clone(), clock, ids);
            task = new_task(&tasks);
            let record = consents
                .register(task, "openai_compat", Sha256Hex::of(b"s"), 500)
                .unwrap();
            consent_id = record.consent_id;
            assert_eq!(consents.mark_exhausted(task).unwrap(), 1);
            assert_eq!(consents.mark_exhausted(task).unwrap(), 0, "幂等");
            db.close().unwrap();
        }
        let db = Arc::new(Database::open(&path).unwrap());
        let clock: SharedClock =
            Arc::new(FixedClock::from_rfc3339("2026-11-02T12:00:00Z").unwrap());
        let ids = Arc::new(UlidFactory::new(clock.clone()));
        let consents = ConsentStore::new(db.clone(), clock, ids);
        assert!(consents.is_valid(task).unwrap().is_none());
        assert!(consents.is_exhausted(task).unwrap());
        let stored = consents.get(consent_id).unwrap().unwrap();
        assert_eq!(stored.state, ConsentState::Exhausted);
        assert!(stored.exhausted_at.is_some());
        assert!(stored.ended_at.is_none());
        assert_eq!(
            consents
                .validate_call(task, Sha256Hex::of(b"s"))
                .unwrap_err()
                .class(),
            crate::error::ErrorClass::PolicyBlocked
        );
        db.close().unwrap();
    }
}
