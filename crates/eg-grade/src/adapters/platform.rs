//! `eg-platform` 适配器：id / 时钟 / 判分记录落库（记录+轨迹+模型调用+审计同事务）。
//!
//! 已知平台缺口（已报告 Lead / platform v2 任务）：
//! - `grade_record` 无幂等键表：本模块提供 `PlatformIdempotencyStore` 进程内实现，
//!   进程重启后首次重跑可能重新判分一次（重复修订被唯一约束拒绝后由落库幂等跳过）；
//! - 无判分断点表：`ProgressStore` 仍用内存 fixture（平台任务队列可后续承载）；
//! - `repo` 无队列/最新修订视图（review task-42 负责）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use eg_domain::{Clock, ComparatorTrace, GradeRecord, ModelCall, Sha256Hex, Timestamp, Ulid};
use eg_platform::audit::{AuditLog, NewAuditEntry};
use eg_platform::clock::SharedClock;
use eg_platform::db::Database;
use eg_platform::ids::UlidFactory;
use eg_platform::repo;

use crate::ports::{IdGenerator, IdSourceError, IdempotencyKey, IdempotencyStore};
use crate::submission::{GradeStore, StoreError};

/// 平台 id 生成器（`UlidFactory` 单调不倒退）。
#[derive(Debug, Clone)]
pub struct PlatformIdGenerator {
    ids: Arc<UlidFactory>,
}

impl PlatformIdGenerator {
    /// 构造。
    #[must_use]
    pub fn new(ids: Arc<UlidFactory>) -> Self {
        Self { ids }
    }
}

impl IdGenerator for PlatformIdGenerator {
    fn try_next_ulid(&self) -> Result<Ulid, IdSourceError> {
        self.ids
            .next_ulid()
            .map_err(|error| IdSourceError(error.to_string()))
    }
}

/// 平台时钟适配（`SystemClock`/注入时钟均可）。
#[derive(Clone)]
pub struct PlatformClock {
    clock: SharedClock,
}

impl std::fmt::Debug for PlatformClock {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("PlatformClock").finish()
    }
}

impl PlatformClock {
    /// 构造。
    #[must_use]
    pub fn new(clock: SharedClock) -> Self {
        Self { clock }
    }
}

impl Clock for PlatformClock {
    fn now(&self) -> Timestamp {
        self.clock.now()
    }
}

/// 判分记录落库适配（`grade_record` + `comparator_trace` + `model_call` + 审计同事务）。
#[derive(Clone)]
pub struct PlatformGradeStore {
    db: Arc<Database>,
    audit: Arc<AuditLog>,
}

impl std::fmt::Debug for PlatformGradeStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PlatformGradeStore")
            .finish_non_exhaustive()
    }
}

impl PlatformGradeStore {
    /// 构造。
    #[must_use]
    pub fn new(db: Arc<Database>, audit: Arc<AuditLog>) -> Self {
        Self { db, audit }
    }
}

impl GradeStore for PlatformGradeStore {
    fn persist_question(
        &self,
        record: &GradeRecord,
        traces: &[ComparatorTrace],
        unpersisted_model_calls: &[ModelCall],
    ) -> Result<(), StoreError> {
        let existing = self.latest_records(record.submission_id)?;
        if existing.iter().any(|candidate| candidate.id == record.id) {
            return Ok(()); // 幂等：重跑/断点恢复不重复写
        }
        let record = record.clone();
        let traces: Vec<ComparatorTrace> = traces.to_vec();
        let calls: Vec<ModelCall> = unpersisted_model_calls.to_vec();
        let after_hash = Sha256Hex::of(
            serde_json::to_string(&record)
                .unwrap_or_default()
                .as_bytes(),
        );
        let entry = NewAuditEntry::new("grade.record", "grade_record", record.id.to_string())
            .with_note(format!(
                "判分记录（提交 {}，题 {}，修订 {}，{:?}/{:?}）",
                record.submission_id,
                record.question_id,
                record.revision,
                record.mode,
                record.status
            ))
            .with_hashes(None, Some(after_hash));
        self.audit
            .business_write(entry, move |tx| {
                repo::append_grade_record(tx, &record)?;
                for trace in &traces {
                    repo::insert_comparator_trace(tx, record.id, trace)?;
                }
                for call in &calls {
                    repo::insert_model_call(tx, call)?;
                }
                Ok(())
            })
            .map_err(|error| StoreError::Failed(error.to_string()))?;
        Ok(())
    }

    fn latest_records(
        &self,
        submission_id: eg_domain::SubmissionId,
    ) -> Result<Vec<GradeRecord>, StoreError> {
        let all = self
            .db
            .read(|conn| repo::list_grade_records(conn, submission_id))
            .map_err(|error| StoreError::Failed(error.to_string()))?;
        let mut latest: Vec<GradeRecord> = Vec::new();
        for record in all {
            match latest
                .iter_mut()
                .find(|candidate| candidate.question_id == record.question_id)
            {
                Some(existing) if existing.revision < record.revision => *existing = record,
                Some(_) => {}
                None => latest.push(record),
            }
        }
        latest.sort_by_key(|record| record.question_id);
        Ok(latest)
    }
}

/// 平台幂等键存储（进程内；平台无幂等表，见模块文档缺口）。
#[derive(Debug, Default)]
pub struct PlatformIdempotencyStore {
    records: Mutex<HashMap<IdempotencyKey, GradeRecord>>,
}

impl PlatformIdempotencyStore {
    /// 空存储。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前条目数（诊断）。
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.lock().map_or(0, |records| records.len())
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl IdempotencyStore for PlatformIdempotencyStore {
    fn lookup(&self, key: &IdempotencyKey) -> Option<GradeRecord> {
        self.records
            .lock()
            .ok()
            .and_then(|records| records.get(key).cloned())
    }

    fn store(&self, key: IdempotencyKey, record: GradeRecord) {
        if let Ok(mut records) = self.records.lock() {
            records.entry(key).or_insert(record);
        }
    }
}
