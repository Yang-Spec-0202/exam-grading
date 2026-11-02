//! 审计（与日志严格分离；只追加、哈希链恒开；`SPEC-platform.md` §6）。
//!
//! - 表：`audit_log`（`actor` 固定 `local-teacher`）。
//! - 链：`chain_hash = BLAKE3(prev_chain_hash || 规范化条目字节)`；首条 `prev = 32 字节零`。
//! - 写业务数据必须同事务写审计；**审计写失败则业务写回滚**（[`AuditLog::business_write`]）。
//! - 启动与导出诊断包时校验链；篡改可检出。

use std::sync::Arc;

use eg_domain::{
    ConsentId, ModelCallId, QuestionId, Sha256Hex, SubmissionId, TaskId, TemplateId, Timestamp,
    Ulid,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

use crate::clock::SharedClock;
use crate::db::Database;
use crate::error::{PlatformError, Result};
use crate::ids::UlidFactory;

/// 审计 actor（匿名单机教师，固定值，来自 `domain`）。
pub const AUDIT_ACTOR: &str = eg_domain::LOCAL_TEACHER;

/// BLAKE3 链哈希（`prev_hash`/`chain_hash` 列）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChainHash([u8; 32]);

impl ChainHash {
    /// 创世哈希（全零；表示链首）。
    pub const GENESIS: Self = Self([0u8; 32]);

    /// 计算链哈希。
    #[must_use]
    pub fn chain(previous: Self, payload: &[u8]) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&previous.0);
        hasher.update(payload);
        Self(*hasher.finalize().as_bytes())
    }

    /// 十六进制文本。
    #[must_use]
    pub fn to_hex(self) -> String {
        use std::fmt::Write as _;
        let mut out = String::with_capacity(64);
        for byte in self.0 {
            let _ = write!(out, "{byte:02x}");
        }
        out
    }

    /// 由十六进制文本解析（64 字符）。
    ///
    /// # Errors
    ///
    /// 长度或字符非法时返回错误。
    pub fn from_hex(text: &str) -> Result<Self> {
        if text.len() != 64 {
            return Err(PlatformError::DatabaseCorrupt(format!(
                "链哈希长度非法：{text}"
            )));
        }
        let mut bytes = [0u8; 32];
        for (index, chunk) in text.as_bytes().chunks(2).enumerate() {
            let part = std::str::from_utf8(chunk)
                .map_err(|error| PlatformError::DatabaseCorrupt(error.to_string()))?;
            bytes[index] = u8::from_str_radix(part, 16)
                .map_err(|error| PlatformError::DatabaseCorrupt(error.to_string()))?;
        }
        Ok(Self(bytes))
    }
}

impl std::fmt::Display for ChainHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// 新增审计条目（调用方填写业务语义；时间/actor/id/链由本模块生成）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewAuditEntry {
    /// 动作（如 `submission.import`）。
    pub action: String,
    /// 实体类型（如 `submission`）。
    pub entity_type: String,
    /// 实体 id。
    pub entity_id: String,
    /// 变更前内容哈希。
    pub before_hash: Option<Sha256Hex>,
    /// 变更后内容哈希。
    pub after_hash: Option<Sha256Hex>,
    /// 备注（中文可读）。
    pub note: String,
    /// 关联授权 id。
    pub consent_id: Option<ConsentId>,
    /// 关联任务 id。
    pub task_id: Option<TaskId>,
    /// 本次操作后的生效规则版本（规则确认等；schema v2）。
    pub rubric_version: Option<u32>,
    /// 模板 id（模板套用/模板来源；schema v2）。
    pub template_id: Option<TemplateId>,
    /// 模板版本（编码规则见 `eg-rubric::TemplateVersion::as_u32`；schema v2）。
    pub template_version: Option<u32>,
    /// 模板完整引用 JSON（由 `eg-rubric` 序列化；schema v2）。
    pub template_ref: Option<serde_json::Value>,
    /// 提示词哈希（模型整理复现用；schema v2）。
    pub prompt_hash: Option<Sha256Hex>,
    /// 模型调用 id（模型整理来源；schema v2）。
    pub model_call_id: Option<ModelCallId>,
    /// 命令幂等键（复核/重判命令；schema v3；同一命令重复提交可定位）。
    pub command_id: Option<String>,
    /// 关联提交 id（复核/判分命令；schema v3）。
    pub submission_id: Option<SubmissionId>,
    /// 关联题目 id（题级命令；schema v3）。
    pub question_id: Option<QuestionId>,
    /// 原因码（复核/排除/抽检结论；schema v3，调用方定义的稳定文本）。
    pub reason_code: Option<String>,
    /// 重跑/操作范围（如 `single_question`、`whole_submission`；schema v3）。
    pub scope: Option<String>,
    /// 确认标记（抽检不通过后是否已确认重跑等；schema v3）。
    pub confirmed: Option<bool>,
    /// 抽检结论（如 `pass`/`fail`；schema v3）。
    pub sampling_verdict: Option<String>,
}

impl NewAuditEntry {
    /// 构造最简条目。
    #[must_use]
    pub fn new(
        action: impl Into<String>,
        entity_type: impl Into<String>,
        entity_id: impl Into<String>,
    ) -> Self {
        Self {
            action: action.into(),
            entity_type: entity_type.into(),
            entity_id: entity_id.into(),
            ..Self::default()
        }
    }

    /// 设置备注。
    #[must_use]
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = note.into();
        self
    }

    /// 设置变更前后哈希。
    #[must_use]
    pub fn with_hashes(mut self, before: Option<Sha256Hex>, after: Option<Sha256Hex>) -> Self {
        self.before_hash = before;
        self.after_hash = after;
        self
    }

    /// 关联任务。
    #[must_use]
    pub fn with_task(mut self, task_id: TaskId) -> Self {
        self.task_id = Some(task_id);
        self
    }

    /// 关联授权。
    #[must_use]
    pub fn with_consent(mut self, consent_id: ConsentId) -> Self {
        self.consent_id = Some(consent_id);
        self
    }

    /// 生效规则版本（规则确认）。
    #[must_use]
    pub const fn with_rubric_version(mut self, rubric_version: u32) -> Self {
        self.rubric_version = Some(rubric_version);
        self
    }

    /// 模板引用（id + 版本；完整结构由 [`Self::with_template_ref`] 提供）。
    #[must_use]
    pub fn with_template(mut self, template_id: TemplateId, template_version: u32) -> Self {
        self.template_id = Some(template_id);
        self.template_version = Some(template_version);
        self
    }

    /// 模板完整引用 JSON（`eg-rubric::TemplateRef` 序列化结果）。
    #[must_use]
    pub fn with_template_ref(mut self, template_ref: serde_json::Value) -> Self {
        self.template_ref = Some(template_ref);
        self
    }

    /// 提示词哈希（模型整理复现）。
    #[must_use]
    pub const fn with_prompt_hash(mut self, prompt_hash: Sha256Hex) -> Self {
        self.prompt_hash = Some(prompt_hash);
        self
    }

    /// 模型调用 id。
    #[must_use]
    pub const fn with_model_call(mut self, model_call_id: ModelCallId) -> Self {
        self.model_call_id = Some(model_call_id);
        self
    }

    /// 命令幂等键（复核/重判命令；重复提交可定位本次命令的审计）。
    #[must_use]
    pub fn with_command_id(mut self, command_id: impl Into<String>) -> Self {
        self.command_id = Some(command_id.into());
        self
    }

    /// 关联提交/题实体（复核、判分命令）。
    #[must_use]
    pub const fn with_entities(
        mut self,
        submission_id: SubmissionId,
        question_id: Option<QuestionId>,
    ) -> Self {
        self.submission_id = Some(submission_id);
        self.question_id = question_id;
        self
    }

    /// 原因码（复核原因/排除原因等稳定文本）。
    #[must_use]
    pub fn with_reason_code(mut self, reason_code: impl Into<String>) -> Self {
        self.reason_code = Some(reason_code.into());
        self
    }

    /// 重跑/操作范围（`single_question`、`whole_submission` 等）。
    #[must_use]
    pub fn with_scope(mut self, scope: impl Into<String>) -> Self {
        self.scope = Some(scope.into());
        self
    }

    /// 确认标记（抽检不通过后是否已确认重跑等）。
    #[must_use]
    pub const fn with_confirmed(mut self, confirmed: bool) -> Self {
        self.confirmed = Some(confirmed);
        self
    }

    /// 抽检结论（`pass`/`fail` 等稳定文本）。
    #[must_use]
    pub fn with_sampling_verdict(mut self, verdict: impl Into<String>) -> Self {
        self.sampling_verdict = Some(verdict.into());
        self
    }
}

/// 已落库审计条目。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEntry {
    /// 条目 id（ULID）。
    pub id: Ulid,
    /// 时间。
    pub at: Timestamp,
    /// actor（固定 `local-teacher`）。
    pub actor: String,
    /// 动作。
    pub action: String,
    /// 实体类型。
    pub entity_type: String,
    /// 实体 id。
    pub entity_id: String,
    /// 变更前哈希。
    pub before_hash: Option<Sha256Hex>,
    /// 变更后哈希。
    pub after_hash: Option<Sha256Hex>,
    /// 上一条链哈希。
    pub prev_hash: ChainHash,
    /// 本条链哈希。
    pub chain_hash: ChainHash,
    /// 备注。
    pub note: String,
    /// 关联授权 id。
    pub consent_id: Option<ConsentId>,
    /// 关联任务 id。
    pub task_id: Option<TaskId>,
    /// 本次操作后的生效规则版本（规则确认等；schema v2）。
    pub rubric_version: Option<u32>,
    /// 模板 id（模板套用/模板来源；schema v2）。
    pub template_id: Option<TemplateId>,
    /// 模板版本（schema v2）。
    pub template_version: Option<u32>,
    /// 模板完整引用 JSON（schema v2）。
    pub template_ref: Option<serde_json::Value>,
    /// 提示词哈希（schema v2）。
    pub prompt_hash: Option<Sha256Hex>,
    /// 模型调用 id（schema v2）。
    pub model_call_id: Option<ModelCallId>,
    /// 命令幂等键（schema v3）。
    pub command_id: Option<String>,
    /// 关联提交 id（schema v3）。
    pub submission_id: Option<SubmissionId>,
    /// 关联题目 id（schema v3）。
    pub question_id: Option<QuestionId>,
    /// 原因码（schema v3）。
    pub reason_code: Option<String>,
    /// 重跑/操作范围（schema v3）。
    pub scope: Option<String>,
    /// 确认标记（schema v3）。
    pub confirmed: Option<bool>,
    /// 抽检结论（schema v3）。
    pub sampling_verdict: Option<String>,
}

/// 链校验结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainVerification {
    /// 已校验条目数。
    pub entries: u64,
    /// 是否通过。
    pub ok: bool,
    /// 首个断链位置（条目 id）与原因。
    pub broken: Option<(String, String)>,
}

/// 审计查询过滤。
#[derive(Debug, Clone, Default)]
pub struct AuditQuery {
    /// 实体类型过滤。
    pub entity_type: Option<String>,
    /// 实体 id 过滤。
    pub entity_id: Option<String>,
    /// 起始时间（含）。
    pub from: Option<Timestamp>,
    /// 结束时间（含）。
    pub to: Option<Timestamp>,
    /// 最大条数（默认 100）。
    pub limit: usize,
    /// 命令幂等键过滤（复核/重判命令定位；schema v3）。
    pub command_id: Option<String>,
    /// 提交 id 过滤（复核/判分命令；schema v3）。
    pub submission_id: Option<SubmissionId>,
}

/// 审计日志（只追加）。
#[derive(Clone)]
pub struct AuditLog {
    db: Arc<Database>,
    clock: SharedClock,
    ids: Arc<UlidFactory>,
}

impl std::fmt::Debug for AuditLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditLog").finish_non_exhaustive()
    }
}

impl AuditLog {
    /// 构造审计日志。
    #[must_use]
    pub fn new(db: Arc<Database>, clock: SharedClock, ids: Arc<UlidFactory>) -> Self {
        Self { db, clock, ids }
    }

    /// 追加一条审计（独立事务）。
    ///
    /// # Errors
    ///
    /// 生成 id、查询链尾或写入失败时返回错误。
    pub fn append(&self, entry: NewAuditEntry) -> Result<AuditEntry> {
        let at = self.clock.now();
        let id = self.ids.next_ulid()?;
        let log = self.clone();
        self.db
            .write(move |tx| log.append_in_tx(tx, id, at, &entry))
    }

    /// 在给定事务中追加审计（**业务写与审计同一事务**，供 `business_write` 使用）。
    ///
    /// # Errors
    ///
    /// 查询链尾或写入失败时返回错误。
    pub fn append_in_tx(
        &self,
        tx: &Transaction<'_>,
        id: Ulid,
        at: Timestamp,
        entry: &NewAuditEntry,
    ) -> Result<AuditEntry> {
        let previous = last_chain_hash(tx)?;
        let record = AuditEntry {
            id,
            at,
            actor: AUDIT_ACTOR.to_owned(),
            action: entry.action.clone(),
            entity_type: entry.entity_type.clone(),
            entity_id: entry.entity_id.clone(),
            before_hash: entry.before_hash,
            after_hash: entry.after_hash,
            prev_hash: previous,
            chain_hash: ChainHash::GENESIS,
            note: entry.note.clone(),
            consent_id: entry.consent_id,
            task_id: entry.task_id,
            rubric_version: entry.rubric_version,
            template_id: entry.template_id.clone(),
            template_version: entry.template_version,
            template_ref: entry.template_ref.clone(),
            prompt_hash: entry.prompt_hash,
            model_call_id: entry.model_call_id,
            command_id: entry.command_id.clone(),
            submission_id: entry.submission_id,
            question_id: entry.question_id,
            reason_code: entry.reason_code.clone(),
            scope: entry.scope.clone(),
            confirmed: entry.confirmed,
            sampling_verdict: entry.sampling_verdict.clone(),
        };
        let record = AuditEntry {
            chain_hash: chain_of(&record)?,
            ..record
        };
        tx.execute(
            "INSERT INTO audit_log(id, at, actor, action, entity_type, entity_id, before_hash, \
             after_hash, prev_hash, chain_hash, note, consent_id, task_id, rubric_version, \
             template_id, template_version, template_ref, prompt_hash, model_call_id, \
             command_id, submission_id, question_id, reason_code, scope, confirmed, \
             sampling_verdict) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, \
             ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26)",
            params![
                record.id.to_string(),
                eg_domain::time::format_rfc3339(record.at),
                record.actor,
                record.action,
                record.entity_type,
                record.entity_id,
                record.before_hash.map(Sha256Hex::to_hex),
                record.after_hash.map(Sha256Hex::to_hex),
                record.prev_hash.to_hex(),
                record.chain_hash.to_hex(),
                record.note,
                record.consent_id.map(|id| id.to_string()),
                record.task_id.map(|id| id.to_string()),
                record.rubric_version.map(i64::from),
                record.template_id.as_ref().map(TemplateId::as_str),
                record.template_version.map(i64::from),
                record
                    .template_ref
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()
                    .map_err(|error| PlatformError::Serialize(error.to_string()))?,
                record.prompt_hash.map(Sha256Hex::to_hex),
                record.model_call_id.map(|id| id.to_string()),
                record.command_id,
                record.submission_id.map(|id| id.to_string()),
                record.question_id.map(|id| id.to_string()),
                record.reason_code,
                record.scope,
                record.confirmed.map(i64::from),
                record.sampling_verdict
            ],
        )
        .map_err(|error| PlatformError::Audit(format!("审计写入失败：{error}")))?;
        Ok(record)
    }

    /// 业务写 + 审计（同一事务）：审计失败则业务写回滚。
    ///
    /// # Errors
    ///
    /// 业务闭包错误或审计写入失败时返回错误（事务整体回滚）。
    pub fn business_write<T, F>(&self, entry: NewAuditEntry, business: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Transaction<'_>) -> Result<T> + Send + 'static,
    {
        let at = self.clock.now();
        let id = self.ids.next_ulid()?;
        let clock_ids = (at, id);
        let log = self.clone();
        self.db.write(move |tx| {
            let value = business(tx)?;
            let (at, id) = clock_ids;
            log.append_in_tx(tx, id, at, &entry)?;
            Ok(value)
        })
    }

    /// 复核/判分命令事务化 helper（等价 [`Self::business_write`]）。
    ///
    /// 语义：业务闭包（追加判分修订 + 撤销栈入栈 + 修改提交级排除等）与审计**同一事务**，
    /// 任一步失败整体回滚，消除「已改分未审计」窗口。命令幂等键填
    /// [`NewAuditEntry::with_command_id`]，重复提交可据审计定位。
    ///
    /// # Errors
    ///
    /// 业务闭包错误或审计写入失败时返回错误（事务整体回滚）。
    pub fn command_write<T, F>(&self, entry: NewAuditEntry, business: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Transaction<'_>) -> Result<T> + Send + 'static,
    {
        self.business_write(entry, business)
    }

    /// 按条件查询（时间倒序）。
    ///
    /// # Errors
    ///
    /// 查询失败时返回错误。
    pub fn list(&self, query: &AuditQuery) -> Result<Vec<AuditEntry>> {
        let limit = if query.limit == 0 { 100 } else { query.limit };
        self.db.read(|conn| {
            let mut statement = conn
                .prepare(
                    "SELECT id, at, actor, action, entity_type, entity_id, before_hash, \
                     after_hash, prev_hash, chain_hash, note, consent_id, task_id, \
                     rubric_version, template_id, template_version, template_ref, \
                     prompt_hash, model_call_id, command_id, submission_id, question_id, \
                     reason_code, scope, confirmed, sampling_verdict \
                     FROM audit_log ORDER BY rowid DESC LIMIT ?1",
                )
                .map_err(PlatformError::from)?;
            let rows = statement
                .query_map([i64::try_from(limit).unwrap_or(i64::MAX)], row_to_entry)
                .map_err(PlatformError::from)?;
            let mut out = Vec::new();
            for row in rows {
                let entry = row.map_err(PlatformError::from)?;
                if !matches_query(&entry, query) {
                    continue;
                }
                out.push(entry);
            }
            Ok(out)
        })
    }

    /// 校验整条哈希链。
    ///
    /// # Errors
    ///
    /// 查询或解析失败时返回错误（链不一致不返回错误，记录在报告里）。
    pub fn verify_chain(&self) -> Result<ChainVerification> {
        let entries = self.read_all()?;
        let mut previous = ChainHash::GENESIS;
        let mut count = 0;
        for entry in &entries {
            count += 1;
            if entry.prev_hash != previous {
                return Ok(ChainVerification {
                    entries: count,
                    ok: false,
                    broken: Some((
                        entry.id.to_string(),
                        "prev_hash 与上一条 chain_hash 不一致".to_owned(),
                    )),
                });
            }
            let expected = chain_of(entry)?;
            if expected != entry.chain_hash {
                return Ok(ChainVerification {
                    entries: count,
                    ok: false,
                    broken: Some((
                        entry.id.to_string(),
                        "条目内容与 chain_hash 不一致".to_owned(),
                    )),
                });
            }
            previous = entry.chain_hash;
        }
        Ok(ChainVerification {
            entries: count,
            ok: true,
            broken: None,
        })
    }

    /// 全部审计条目（按写入顺序）。
    ///
    /// # Errors
    ///
    /// 查询或解析失败时返回错误。
    pub fn read_all(&self) -> Result<Vec<AuditEntry>> {
        self.db.read(|conn| {
            let mut statement = conn
                .prepare(
                    "SELECT id, at, actor, action, entity_type, entity_id, before_hash, \
                     after_hash, prev_hash, chain_hash, note, consent_id, task_id, \
                     rubric_version, template_id, template_version, template_ref, \
                     prompt_hash, model_call_id, command_id, submission_id, question_id, \
                     reason_code, scope, confirmed, sampling_verdict \
                     FROM audit_log ORDER BY rowid ASC",
                )
                .map_err(PlatformError::from)?;
            let rows = statement
                .query_map([], row_to_entry)
                .map_err(PlatformError::from)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.map_err(PlatformError::from)?);
            }
            Ok(out)
        })
    }

    /// 条目数。
    ///
    /// # Errors
    ///
    /// 查询失败时返回错误。
    pub fn count(&self) -> Result<u64> {
        self.db.read(|conn| {
            conn.query_row("SELECT COUNT(*) FROM audit_log", [], |row| row.get(0))
                .map_err(PlatformError::from)
                .map(|count: i64| u64::try_from(count).unwrap_or(0))
        })
    }
}

/// 计算条目链哈希（不含 `chain_hash` 字段本身）。
fn chain_of(entry: &AuditEntry) -> Result<ChainHash> {
    #[derive(Serialize)]
    struct Payload<'a> {
        id: String,
        at: String,
        actor: &'a str,
        action: &'a str,
        entity_type: &'a str,
        entity_id: &'a str,
        before_hash: Option<String>,
        after_hash: Option<String>,
        note: &'a str,
        consent_id: Option<String>,
        task_id: Option<String>,
        // schema v2 扩展字段：为空时不进入规范化载荷，保证旧行链哈希可验证
        // （旧行这些列恒为 NULL，序列化结果与 v1 完全一致）。
        #[serde(skip_serializing_if = "Option::is_none")]
        rubric_version: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        template_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        template_version: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        template_ref: Option<&'a serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        prompt_hash: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        model_call_id: Option<String>,
        // schema v3 命令扩展字段：同样仅在非空时进入载荷，保证 v1/v2 旧行链哈希不变。
        #[serde(skip_serializing_if = "Option::is_none")]
        command_id: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        submission_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        question_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason_code: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        scope: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        confirmed: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        sampling_verdict: Option<&'a str>,
    }
    let payload = Payload {
        id: entry.id.to_string(),
        at: eg_domain::time::format_rfc3339(entry.at),
        actor: &entry.actor,
        action: &entry.action,
        entity_type: &entry.entity_type,
        entity_id: &entry.entity_id,
        before_hash: entry.before_hash.map(Sha256Hex::to_hex),
        after_hash: entry.after_hash.map(Sha256Hex::to_hex),
        note: &entry.note,
        consent_id: entry.consent_id.map(|id| id.to_string()),
        task_id: entry.task_id.map(|id| id.to_string()),
        rubric_version: entry.rubric_version,
        template_id: entry.template_id.as_ref().map(|id| id.as_str().to_owned()),
        template_version: entry.template_version,
        template_ref: entry.template_ref.as_ref(),
        prompt_hash: entry.prompt_hash.map(Sha256Hex::to_hex),
        model_call_id: entry.model_call_id.map(|id| id.to_string()),
        command_id: entry.command_id.as_deref(),
        submission_id: entry.submission_id.map(|id| id.to_string()),
        question_id: entry.question_id.map(|id| id.to_string()),
        reason_code: entry.reason_code.as_deref(),
        scope: entry.scope.as_deref(),
        confirmed: entry.confirmed,
        sampling_verdict: entry.sampling_verdict.as_deref(),
    };
    let bytes = serde_json::to_vec(&payload)
        .map_err(|error| PlatformError::Serialize(error.to_string()))?;
    Ok(ChainHash::chain(entry.prev_hash, &bytes))
}

/// 取链尾哈希（事务内；空表返回创世哈希）。
fn last_chain_hash(tx: &Transaction<'_>) -> Result<ChainHash> {
    let last: Option<String> = tx
        .query_row(
            "SELECT chain_hash FROM audit_log ORDER BY rowid DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(PlatformError::from)?;
    match last {
        None => Ok(ChainHash::GENESIS),
        Some(text) => ChainHash::from_hex(&text),
    }
}

/// 行 → 条目。
fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<AuditEntry> {
    let id_text: String = row.get(0)?;
    let at_text: String = row.get(1)?;
    let before: Option<String> = row.get(6)?;
    let after: Option<String> = row.get(7)?;
    let prev: String = row.get(8)?;
    let chain: String = row.get(9)?;
    let consent: Option<String> = row.get(11)?;
    let task: Option<String> = row.get(12)?;
    let rubric_version: Option<i64> = row.get(13)?;
    let template_id: Option<String> = row.get(14)?;
    let template_version: Option<i64> = row.get(15)?;
    let template_ref: Option<String> = row.get(16)?;
    let prompt_hash: Option<String> = row.get(17)?;
    let model_call_id: Option<String> = row.get(18)?;
    let command_id: Option<String> = row.get(19)?;
    let submission_id: Option<String> = row.get(20)?;
    let question_id: Option<String> = row.get(21)?;
    let reason_code: Option<String> = row.get(22)?;
    let scope: Option<String> = row.get(23)?;
    let confirmed: Option<i64> = row.get(24)?;
    let sampling_verdict: Option<String> = row.get(25)?;
    let parse_err = |what: &str, detail: String| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{what}: {detail}"),
            )),
        )
    };
    Ok(AuditEntry {
        id: Ulid::parse(&id_text).map_err(|error| parse_err("id", error.to_string()))?,
        at: eg_domain::time::parse_rfc3339(&at_text)
            .map_err(|error| parse_err("at", error.to_string()))?,
        actor: row.get(2)?,
        action: row.get(3)?,
        entity_type: row.get(4)?,
        entity_id: row.get(5)?,
        before_hash: before
            .map(|text| Sha256Hex::from_hex(&text))
            .transpose()
            .map_err(|error| parse_err("before_hash", error.to_string()))?,
        after_hash: after
            .map(|text| Sha256Hex::from_hex(&text))
            .transpose()
            .map_err(|error| parse_err("after_hash", error.to_string()))?,
        prev_hash: ChainHash::from_hex(&prev)
            .map_err(|error| parse_err("prev_hash", error.to_string()))?,
        chain_hash: ChainHash::from_hex(&chain)
            .map_err(|error| parse_err("chain_hash", error.to_string()))?,
        note: row.get(10)?,
        consent_id: consent
            .map(|text| ConsentId::parse(&text))
            .transpose()
            .map_err(|error| parse_err("consent_id", error.to_string()))?,
        task_id: task
            .map(|text| TaskId::parse(&text))
            .transpose()
            .map_err(|error| parse_err("task_id", error.to_string()))?,
        rubric_version: rubric_version
            .map(|value| {
                u32::try_from(value).map_err(|_| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Integer,
                        Box::new(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("rubric_version 越界: {value}"),
                        )),
                    )
                })
            })
            .transpose()?,
        template_id: template_id
            .map(|text| TemplateId::new(&text))
            .transpose()
            .map_err(|error| parse_err("template_id", error.to_string()))?,
        template_version: template_version
            .map(|value| {
                u32::try_from(value).map_err(|_| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Integer,
                        Box::new(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("template_version 越界: {value}"),
                        )),
                    )
                })
            })
            .transpose()?,
        template_ref: template_ref
            .map(|text| {
                serde_json::from_str(&text)
                    .map_err(|error| parse_err("template_ref", error.to_string()))
            })
            .transpose()?,
        prompt_hash: prompt_hash
            .map(|text| Sha256Hex::from_hex(&text))
            .transpose()
            .map_err(|error| parse_err("prompt_hash", error.to_string()))?,
        model_call_id: model_call_id
            .map(|text| ModelCallId::parse(&text))
            .transpose()
            .map_err(|error| parse_err("model_call_id", error.to_string()))?,
        command_id,
        submission_id: submission_id
            .map(|text| SubmissionId::parse(&text))
            .transpose()
            .map_err(|error| parse_err("submission_id", error.to_string()))?,
        question_id: question_id
            .map(|text| QuestionId::parse(&text))
            .transpose()
            .map_err(|error| parse_err("question_id", error.to_string()))?,
        reason_code,
        scope,
        confirmed: confirmed.map(|value| value != 0),
        sampling_verdict,
    })
}

/// 内存过滤（时间/实体）。
fn matches_query(entry: &AuditEntry, query: &AuditQuery) -> bool {
    if let Some(entity_type) = &query.entity_type
        && &entry.entity_type != entity_type
    {
        return false;
    }
    if let Some(entity_id) = &query.entity_id
        && &entry.entity_id != entity_id
    {
        return false;
    }
    if let Some(from) = query.from
        && entry.at < from
    {
        return false;
    }
    if let Some(to) = query.to
        && entry.at > to
    {
        return false;
    }
    if let Some(command_id) = &query.command_id
        && entry.command_id.as_deref() != Some(command_id.as_str())
    {
        return false;
    }
    if let Some(submission_id) = query.submission_id
        && entry.submission_id != Some(submission_id)
    {
        return false;
    }
    true
}

/// 链哈希辅助：从条目文本计算（测试/诊断用）。
///
/// # Errors
///
/// 序列化失败时返回错误。
pub fn hash_entry(entry: &AuditEntry) -> Result<ChainHash> {
    chain_of(entry)
}

/// 读取指定连接上的链尾（测试/诊断用）。
///
/// # Errors
///
/// 查询失败时返回错误。
pub fn read_last_chain(conn: &Connection) -> Result<ChainHash> {
    let last: Option<String> = conn
        .query_row(
            "SELECT chain_hash FROM audit_log ORDER BY rowid DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(PlatformError::from)?;
    match last {
        None => Ok(ChainHash::GENESIS),
        Some(text) => ChainHash::from_hex(&text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;
    use std::sync::Arc;

    fn setup() -> (tempfile::TempDir, Arc<Database>, AuditLog) {
        let temp = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open(&temp.path().join("db.sqlite3")).unwrap());
        let clock: SharedClock =
            Arc::new(FixedClock::from_rfc3339("2026-11-02T12:00:00Z").unwrap());
        let ids = Arc::new(UlidFactory::new(clock.clone()));
        let audit = AuditLog::new(db.clone(), clock, ids);
        (temp, db, audit)
    }

    #[test]
    fn append_builds_verifiable_hash_chain() {
        let (_temp, _db, audit) = setup();
        for index in 0..5 {
            let entry =
                NewAuditEntry::new("submission.import", "submission", format!("sub-{index}"))
                    .with_note("导入学生作业")
                    .with_hashes(
                        None,
                        Some(Sha256Hex::of(format!("body-{index}").as_bytes())),
                    );
            audit.append(entry).unwrap();
        }
        assert_eq!(audit.count().unwrap(), 5);
        let verification = audit.verify_chain().unwrap();
        assert!(verification.ok, "{verification:?}");
        assert_eq!(verification.entries, 5);
        let all = audit.read_all().unwrap();
        assert_eq!(all[0].actor, AUDIT_ACTOR);
        assert_eq!(all[0].prev_hash, ChainHash::GENESIS);
        for pair in all.windows(2) {
            assert_eq!(pair[1].prev_hash, pair[0].chain_hash);
        }
    }

    #[test]
    fn tampered_row_is_detected() {
        let (_temp, db, audit) = setup();
        audit
            .append(NewAuditEntry::new("a", "b", "c").with_note("原始"))
            .unwrap();
        audit
            .append(NewAuditEntry::new("a2", "b", "c2").with_note("第二条"))
            .unwrap();
        // 直接篡改（模拟故意/损坏）。
        db.write_raw(|conn| {
            conn.execute("UPDATE audit_log SET note = '被篡改' WHERE rowid = 1", [])
                .map_err(PlatformError::from)?;
            Ok(())
        })
        .unwrap();
        let verification = audit.verify_chain().unwrap();
        assert!(!verification.ok);
        assert!(verification.broken.is_some());
    }

    #[test]
    fn business_write_requires_audit() {
        let (_temp, db, audit) = setup();
        db.write(|tx| {
            tx.execute("CREATE TABLE demo(id TEXT PRIMARY KEY)", [])
                .map_err(PlatformError::from)?;
            Ok(())
        })
        .unwrap();
        // 正常路径：业务写与审计同一事务。
        audit
            .business_write(NewAuditEntry::new("demo.insert", "demo", "1"), |tx| {
                tx.execute("INSERT INTO demo(id) VALUES ('1')", [])
                    .map_err(PlatformError::from)?;
                Ok(())
            })
            .unwrap();
        // 审计失败路径：用触发器让审计插入失败，业务写必须回滚。
        db.write_raw(|conn| {
            conn.execute_batch(
                "CREATE TRIGGER fail_audit BEFORE INSERT ON audit_log \
                 BEGIN SELECT RAISE(FAIL, '模拟审计写入失败'); END;",
            )
            .map_err(PlatformError::from)
        })
        .unwrap();
        let result = audit.business_write(NewAuditEntry::new("demo.insert", "demo", "2"), |tx| {
            tx.execute("INSERT INTO demo(id) VALUES ('2')", [])
                .map_err(PlatformError::from)?;
            Ok(())
        });
        assert!(result.is_err(), "审计失败必须拒绝业务写");
        let count: i64 = db
            .read(|conn| {
                conn.query_row("SELECT COUNT(*) FROM demo", [], |row| row.get(0))
                    .map_err(PlatformError::from)
            })
            .unwrap();
        assert_eq!(count, 1, "审计失败时业务写必须回滚");
        // 审计条数仍为 1。
        assert_eq!(audit.count().unwrap(), 1);
    }

    #[test]
    fn query_filters_entity_and_time() {
        let (_temp, _db, audit) = setup();
        audit
            .append(NewAuditEntry::new("x", "submission", "s1"))
            .unwrap();
        audit
            .append(NewAuditEntry::new("y", "paper", "p1"))
            .unwrap();
        let all = audit.list(&AuditQuery::default()).unwrap();
        assert_eq!(all.len(), 2);
        let only_paper = audit
            .list(&AuditQuery {
                entity_type: Some("paper".to_owned()),
                ..AuditQuery::default()
            })
            .unwrap();
        assert_eq!(only_paper.len(), 1);
        assert_eq!(only_paper[0].entity_id, "p1");
    }

    #[test]
    fn chain_hash_hex_roundtrip() {
        let hash = ChainHash::chain(ChainHash::GENESIS, b"payload");
        assert_eq!(ChainHash::from_hex(&hash.to_hex()).unwrap(), hash);
        assert!(ChainHash::from_hex("zz").is_err());
        assert_ne!(hash, ChainHash::GENESIS);
    }
    #[test]
    fn extended_fields_roundtrip_and_chain_verifies() {
        let (_temp, _db, audit) = setup();
        let template_ref = serde_json::json!({
            "template_id": "national-new-1",
            "set_id": "new-curriculum-1",
            "display_name": "新课标Ⅰ卷",
            "version": {"snapshot_year": 2026, "revision": 1},
            "status": "active",
        });
        let model_call = ModelCallId::from_ulid(Ulid::from_u128(9));
        let appended = audit
            .append(
                NewAuditEntry::new("rubric.confirm", "rubric", "r-1")
                    .with_note("教师确认规则")
                    .with_hashes(None, Some(Sha256Hex::of(b"after")))
                    .with_rubric_version(3)
                    .with_template(TemplateId::new("national-new-1").unwrap(), 2_026_001)
                    .with_template_ref(template_ref.clone())
                    .with_prompt_hash(Sha256Hex::of(b"prompt"))
                    .with_model_call(model_call),
            )
            .unwrap();
        assert_eq!(appended.rubric_version, Some(3));
        assert_eq!(appended.template_version, Some(2_026_001));
        assert_eq!(appended.template_ref.as_ref(), Some(&template_ref));
        assert_eq!(appended.prompt_hash, Some(Sha256Hex::of(b"prompt")));
        assert_eq!(appended.model_call_id, Some(model_call));

        let verification = audit.verify_chain().unwrap();
        assert!(verification.ok, "{verification:?}");
        let stored = audit.read_all().unwrap();
        assert_eq!(stored.len(), 1);
        let entry = &stored[0];
        assert_eq!(entry.rubric_version, Some(3));
        assert_eq!(
            entry.template_id.as_ref().map(TemplateId::as_str),
            Some("national-new-1")
        );
        assert_eq!(entry.template_version, Some(2_026_001));
        assert_eq!(entry.template_ref.as_ref(), Some(&template_ref));
        assert_eq!(entry.prompt_hash, Some(Sha256Hex::of(b"prompt")));
        assert_eq!(entry.model_call_id, Some(model_call));
    }

    /// v1 行（无扩展列）必须仍能通过链校验：扩展字段为空时不进入规范化载荷。
    #[test]
    fn legacy_rows_written_before_v2_still_verify() {
        let (_temp, db, audit) = setup();
        #[derive(Serialize)]
        struct LegacyPayload<'a> {
            id: String,
            at: String,
            actor: &'a str,
            action: &'a str,
            entity_type: &'a str,
            entity_id: &'a str,
            before_hash: Option<String>,
            after_hash: Option<String>,
            note: &'a str,
            consent_id: Option<String>,
            task_id: Option<String>,
        }
        let legacy_id = Ulid::from_u128(7);
        let at = "2026-11-02T12:00:00Z";
        let payload = LegacyPayload {
            id: legacy_id.to_string(),
            at: at.to_owned(),
            actor: AUDIT_ACTOR,
            action: "submission.import",
            entity_type: "submission",
            entity_id: "legacy-1",
            before_hash: None,
            after_hash: None,
            note: "v1 旧行",
            consent_id: None,
            task_id: None,
        };
        let bytes = serde_json::to_vec(&payload).unwrap();
        let chain = ChainHash::chain(ChainHash::GENESIS, &bytes);
        db.write_raw(move |conn| {
            conn.execute(
                "INSERT INTO audit_log(id, at, actor, action, entity_type, entity_id, \
                 before_hash, after_hash, prev_hash, chain_hash, note, consent_id, task_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, NULL, ?7, ?8, ?9, NULL, NULL)",
                params![
                    legacy_id.to_string(),
                    at,
                    AUDIT_ACTOR,
                    "submission.import",
                    "submission",
                    "legacy-1",
                    ChainHash::GENESIS.to_hex(),
                    chain.to_hex(),
                    "v1 旧行"
                ],
            )
            .map_err(PlatformError::from)?;
            Ok(())
        })
        .unwrap();
        // 新行接在 v1 行之后，链应整体通过。
        audit
            .append(NewAuditEntry::new("rubric.confirm", "rubric", "r-1").with_rubric_version(1))
            .unwrap();
        let verification = audit.verify_chain().unwrap();
        assert!(verification.ok, "{verification:?}");
        assert_eq!(verification.entries, 2);
        let stored = audit.read_all().unwrap();
        assert_eq!(stored[0].rubric_version, None);
        assert_eq!(stored[0].template_id, None);
        assert_eq!(stored[0].template_ref, None);
        assert_eq!(stored[0].prompt_hash, None);
        assert_eq!(stored[0].model_call_id, None);
        assert_eq!(stored[1].rubric_version, Some(1));
    }

    #[test]
    fn tampered_extension_field_is_detected() {
        let (_temp, db, audit) = setup();
        audit
            .append(NewAuditEntry::new("rubric.confirm", "rubric", "r-1").with_rubric_version(3))
            .unwrap();
        db.write_raw(|conn| {
            conn.execute(
                "UPDATE audit_log SET rubric_version = 99 WHERE rowid = 1",
                [],
            )
            .map_err(PlatformError::from)?;
            Ok(())
        })
        .unwrap();
        let verification = audit.verify_chain().unwrap();
        assert!(!verification.ok, "扩展字段必须纳入链哈希");
    }

    #[test]
    fn command_fields_roundtrip_and_chain_verifies() {
        let (_temp, _db, audit) = setup();
        let submission = SubmissionId::from_ulid(Ulid::from_u128(31));
        let question = QuestionId::from_ulid(Ulid::from_u128(32));
        let appended = audit
            .append(
                NewAuditEntry::new("review.change_score", "grade_record", "g-1")
                    .with_note("改分：+1 分")
                    .with_hashes(
                        Some(Sha256Hex::of(b"before")),
                        Some(Sha256Hex::of(b"after")),
                    )
                    .with_command_id("cmd-2026-0001")
                    .with_entities(submission, Some(question))
                    .with_reason_code("manual_override")
                    .with_scope("single_question")
                    .with_confirmed(true)
                    .with_sampling_verdict("fail"),
            )
            .unwrap();
        assert_eq!(appended.command_id.as_deref(), Some("cmd-2026-0001"));
        assert_eq!(appended.submission_id, Some(submission));
        assert_eq!(appended.question_id, Some(question));
        assert_eq!(appended.reason_code.as_deref(), Some("manual_override"));
        assert_eq!(appended.scope.as_deref(), Some("single_question"));
        assert_eq!(appended.confirmed, Some(true));
        assert_eq!(appended.sampling_verdict.as_deref(), Some("fail"));
        assert!(audit.verify_chain().unwrap().ok);

        // 链上混合 v2/v3 条目仍可验证（v3 字段为空时不进载荷）。
        audit
            .append(NewAuditEntry::new("submission.import", "submission", "s-1"))
            .unwrap();
        assert!(audit.verify_chain().unwrap().ok);

        // 按命令幂等键/提交过滤。
        let by_command = audit
            .list(&AuditQuery {
                command_id: Some("cmd-2026-0001".to_owned()),
                ..AuditQuery::default()
            })
            .unwrap();
        assert_eq!(by_command.len(), 1);
        assert_eq!(by_command[0].id, appended.id);
        let by_submission = audit
            .list(&AuditQuery {
                submission_id: Some(submission),
                ..AuditQuery::default()
            })
            .unwrap();
        assert_eq!(by_submission.len(), 1);
    }

    #[test]
    fn command_write_rolls_back_business_and_audit_together() {
        let (_temp, db, audit) = setup();
        db.write(|tx| {
            tx.execute("INSERT INTO app_meta(key, value) VALUES ('probe', '0')", [])
                .map_err(PlatformError::from)?;
            Ok(())
        })
        .unwrap();

        // 成功路径：业务写 + 审计同事务。
        let value = audit
            .command_write(
                NewAuditEntry::new("review.accept", "grade_record", "g-1")
                    .with_command_id("cmd-ok")
                    .with_entities(SubmissionId::from_ulid(Ulid::from_u128(41)), None),
                |tx| {
                    tx.execute("UPDATE app_meta SET value = '1' WHERE key = 'probe'", [])
                        .map_err(PlatformError::from)?;
                    Ok(7_u32)
                },
            )
            .unwrap();
        assert_eq!(value, 7);
        assert_eq!(audit.count().unwrap(), 1);
        let probe: String = db
            .read(|conn| {
                conn.query_row(
                    "SELECT value FROM app_meta WHERE key = 'probe'",
                    [],
                    |row| row.get(0),
                )
                .map_err(PlatformError::from)
            })
            .unwrap();
        assert_eq!(probe, "1");

        // 失败路径：业务写失败 → 审计不落、业务回滚。
        let failure: Result<()> = audit.command_write(
            NewAuditEntry::new("review.accept", "grade_record", "g-2").with_command_id("cmd-fail"),
            |tx| {
                tx.execute("UPDATE app_meta SET value = '2' WHERE key = 'probe'", [])
                    .map_err(PlatformError::from)?;
                Err::<(), _>(PlatformError::Audit("模拟业务失败".to_owned()))
            },
        );
        assert!(failure.is_err());
        assert_eq!(audit.count().unwrap(), 1, "失败命令不得落审计");
        let probe: String = db
            .read(|conn| {
                conn.query_row(
                    "SELECT value FROM app_meta WHERE key = 'probe'",
                    [],
                    |row| row.get(0),
                )
                .map_err(PlatformError::from)
            })
            .unwrap();
        assert_eq!(probe, "1", "失败命令的业务写必须回滚");
        assert!(audit.verify_chain().unwrap().ok);
    }
}
