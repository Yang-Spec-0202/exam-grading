//! 内存适配层（平台/密钥/数据库接入前的临时实现；`SPEC-rubric.md` §2.6 端口）。
//!
//! 用途：
//! 1. 单元与集成测试（确定性、无 IO、可断言审计序列）；
//! 2. `app` 在 `eg-platform` 仓储/审计接口稳定前的临时装配。
//!
//! TODO(T12/T18)：`eg-platform` 完成后提供真实实现：
//! - `save_draft`/`load_draft` → `platform::repo::insert_rubric_draft`/`get_rubric_draft`
//!   （`rubric_draft` 表，版本化 JSON 落库）；
//! - `save_active_rubric` → `platform::repo::insert_rubric_revision` + `paper.rubric_version`
//!   CAS（同一事务内写审计，`platform::audit::AuditLog::business_write`）；
//! - `RubricAudit` → `platform::audit::AuditLog`（区块链式只追加）；
//! - `RubricIdSource` → `platform::ids::UlidFactory`。
//!
//! 本实现的限制（仅测试可用，不用于生产）：
//! - 无持久化、无事务：草稿写入与审计追加不是原子的；
//! - 无并发安全保证到事务级（仅进程内互斥）；
//! - 不模拟平台审计哈希链。

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use eg_domain::{Clock, MilliScore, QuestionId, Rubric, RubricId, RubricItemId, Timestamp, Ulid};

use crate::error::{Result, RubricError};
use crate::ports::{
    NewRubricAuditEvent, RubricAudit, RubricAuditEvent, RubricIdSource, RubricStore, StoredDraft,
};

/// 内存草稿/生效规则存储（键分别为草稿 id 与题目 id）。
#[derive(Debug, Default)]
pub struct InMemoryRubricStore {
    drafts: Mutex<BTreeMap<RubricId, StoredDraft>>,
    rubrics: Mutex<BTreeMap<QuestionId, Rubric>>,
}

impl InMemoryRubricStore {
    /// 草稿数量（测试断言用）。
    ///
    /// # Errors
    ///
    /// 锁中毒时返回 [`RubricError::Store`]。
    pub fn draft_count(&self) -> Result<usize> {
        Ok(self.lock_drafts()?.len())
    }

    fn lock_drafts(&self) -> Result<std::sync::MutexGuard<'_, BTreeMap<RubricId, StoredDraft>>> {
        self.drafts
            .lock()
            .map_err(|_| RubricError::store("内存草稿存储锁中毒"))
    }

    fn lock_rubrics(&self) -> Result<std::sync::MutexGuard<'_, BTreeMap<QuestionId, Rubric>>> {
        self.rubrics
            .lock()
            .map_err(|_| RubricError::store("内存规则存储锁中毒"))
    }
}

impl RubricStore for InMemoryRubricStore {
    fn load_draft(&self, draft_id: RubricId) -> Result<Option<StoredDraft>> {
        Ok(self.lock_drafts()?.get(&draft_id).cloned())
    }

    fn save_draft(&self, draft: &StoredDraft) -> Result<()> {
        self.lock_drafts()?.insert(draft.draft.id, draft.clone());
        Ok(())
    }

    fn list_drafts(&self, question_id: QuestionId) -> Result<Vec<StoredDraft>> {
        let drafts = self.lock_drafts()?;
        let mut result: Vec<StoredDraft> = drafts
            .values()
            .filter(|stored| stored.draft.question_id == question_id)
            .cloned()
            .collect();
        result.sort_by_key(|stored| stored.draft.created_at);
        Ok(result)
    }

    fn active_rubric(&self, question_id: QuestionId) -> Result<Option<Rubric>> {
        Ok(self.lock_rubrics()?.get(&question_id).cloned())
    }

    fn rubric_version(&self, question_id: QuestionId) -> Result<u32> {
        Ok(self
            .lock_rubrics()?
            .get(&question_id)
            .map_or(0, |rubric| rubric.version))
    }

    fn save_active_rubric(
        &self,
        question_id: QuestionId,
        rubric: &Rubric,
        expected_version: u32,
    ) -> Result<u32> {
        let mut rubrics = self.lock_rubrics()?;
        let actual = rubrics
            .get(&question_id)
            .map_or(0, |current| current.version);
        if actual != expected_version {
            return Err(RubricError::VersionConflict {
                expected: expected_version,
                actual,
            });
        }
        if rubric.version != actual.saturating_add(1) {
            return Err(RubricError::config(format!(
                "生效规则版本必须为当前版本加一（当前 {actual}，实际写入 {}）",
                rubric.version
            )));
        }
        rubrics.insert(question_id, rubric.clone());
        Ok(rubric.version)
    }
}

/// 内存审计（只追加；仅供测试与临时装配，无哈希链）。
#[derive(Debug, Default)]
pub struct RecordingAudit {
    events: Mutex<Vec<RubricAuditEvent>>,
}

impl RecordingAudit {
    /// 已记录的审计事件（按追加顺序）。
    ///
    /// # Errors
    ///
    /// 锁中毒时返回 [`RubricError::Audit`]。
    pub fn events(&self) -> Vec<RubricAuditEvent> {
        self.events
            .lock()
            .map_or_else(|_| Vec::new(), |events| events.clone())
    }

    /// 事件数量。
    ///
    /// # Errors
    ///
    /// 锁中毒时返回 [`RubricError::Audit`]。
    pub fn len(&self) -> Result<usize> {
        self.events
            .lock()
            .map(|events| events.len())
            .map_err(|_| RubricError::audit("内存审计锁中毒"))
    }

    /// 是否为空。
    ///
    /// # Errors
    ///
    /// 锁中毒时返回 [`RubricError::Audit`]。
    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }
}

impl RubricAudit for RecordingAudit {
    fn append(&self, event: NewRubricAuditEvent) -> Result<RubricAuditEvent> {
        let event = event.into_event();
        let mut events = self
            .events
            .lock()
            .map_err(|_| RubricError::audit("内存审计锁中毒"))?;
        events.push(event.clone());
        Ok(event)
    }
}

/// 确定性 id 源（草稿/规则 id 从 1 递增；评分项 id 从独立区段递增）。
#[derive(Debug, Default)]
pub struct SequentialIds {
    rubric: AtomicU64,
    item: AtomicU64,
}

impl SequentialIds {
    /// 评分项 id 起始区段（避免与草稿 id 混淆）。
    const ITEM_BASE: u128 = 0x1000_0000;
}

impl RubricIdSource for SequentialIds {
    fn next_rubric_id(&self) -> RubricId {
        let value = self.rubric.fetch_add(1, Ordering::Relaxed) + 1;
        RubricId::from_ulid(Ulid::from_u128(u128::from(value)))
    }

    fn next_rubric_item_id(&self) -> RubricItemId {
        let value = self.item.fetch_add(1, Ordering::Relaxed) + 1;
        RubricItemId::from_ulid(Ulid::from_u128(Self::ITEM_BASE + u128::from(value)))
    }
}

/// 固定时钟（测试与复现；生产用 `platform::SystemClock`）。
#[derive(Debug, Clone)]
pub struct FixedClock {
    now: Timestamp,
    granularity: MilliScore,
}

impl FixedClock {
    /// 以给定时间点构造固定时钟。
    #[must_use]
    pub const fn new(now: Timestamp) -> Self {
        Self {
            now,
            granularity: MilliScore::ZERO,
        }
    }

    /// 时间点与默认粒度组合（仅供调试展示，不参与业务校验）。
    #[must_use]
    pub const fn with_granularity(mut self, granularity: MilliScore) -> Self {
        self.granularity = granularity;
        self
    }

    /// 当前固定时间点。
    #[must_use]
    pub const fn timestamp(&self) -> Timestamp {
        self.now
    }

    /// 调试用粒度回显。
    #[must_use]
    pub const fn granularity(&self) -> MilliScore {
        self.granularity
    }
}

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        self.now
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::ports::{RubricAction, RubricSettings, RubricStore};
    use eg_domain::time::parse_rfc3339;
    use eg_domain::{
        DraftOrigin, DraftStatus, LOCAL_TEACHER, RubricDraft, RubricItem, RubricOrigin,
    };

    fn timestamp() -> Timestamp {
        parse_rfc3339("2026-11-02T00:00:00Z").unwrap()
    }

    fn empty_draft(id: u128) -> StoredDraft {
        StoredDraft {
            draft: RubricDraft {
                id: RubricId::from_ulid(Ulid::from_u128(id)),
                question_id: QuestionId::from_ulid(Ulid::from_u128(42)),
                status: DraftStatus::Draft,
                items: Vec::new(),
                raw_text: String::new(),
                origin: DraftOrigin::HandwrittenTransliteration,
                model_call_id: None,
                validation: Vec::new(),
                created_at: timestamp(),
                confirmed_at: None,
            },
            question_kind: eg_domain::QuestionKind::Calculation,
            max_score: MilliScore::from_milli(1000).unwrap(),
            mode: eg_domain::ScoringMode::Additive,
            partial_choice: None,
            template: None,
        }
    }

    fn rubric(version: u32) -> Rubric {
        Rubric {
            id: RubricId::from_ulid(Ulid::from_u128(1)),
            version,
            items: vec![RubricItem {
                id: RubricItemId::from_ulid(Ulid::from_u128(11)),
                criterion: "步骤".to_owned(),
                points: MilliScore::from_milli(1000).unwrap(),
                required: false,
                depends_on: Vec::new(),
            }],
            mode: eg_domain::ScoringMode::AllOrNothing,
            total: MilliScore::from_milli(1000).unwrap(),
            origin: RubricOrigin::TeacherHandwritten {
                confirmed_by: LOCAL_TEACHER.to_owned(),
                confirmed_at: timestamp(),
            },
            partial_choice: None,
        }
    }

    #[test]
    fn store_cas_and_versioning() {
        let store = InMemoryRubricStore::default();
        let question = QuestionId::from_ulid(Ulid::from_u128(42));
        assert_eq!(store.rubric_version(question).unwrap(), 0);
        assert!(store.active_rubric(question).unwrap().is_none());

        store.save_active_rubric(question, &rubric(1), 0).unwrap();
        assert_eq!(store.rubric_version(question).unwrap(), 1);

        // 期望版本不符 → VersionConflict。
        assert!(matches!(
            store
                .save_active_rubric(question, &rubric(2), 0)
                .unwrap_err(),
            RubricError::VersionConflict { .. }
        ));
        // 版本必须递增 1。
        assert!(store.save_active_rubric(question, &rubric(3), 1).is_err());
        store.save_active_rubric(question, &rubric(2), 1).unwrap();
        assert_eq!(store.active_rubric(question).unwrap().unwrap().version, 2);
    }

    #[test]
    fn drafts_are_saved_and_listed() {
        let store = InMemoryRubricStore::default();
        let first = empty_draft(1);
        let mut second = empty_draft(2);
        second.draft.created_at = parse_rfc3339("2026-11-02T01:00:00Z").unwrap();
        store.save_draft(&first).unwrap();
        store.save_draft(&second).unwrap();
        assert_eq!(store.draft_count().unwrap(), 2);
        let listed = store.list_drafts(first.draft.question_id).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].draft.id, first.draft.id);
    }

    #[test]
    fn audit_records_events() {
        let audit = RecordingAudit::default();
        assert!(audit.is_empty().unwrap());
        let event = NewRubricAuditEvent::new(
            RubricAction::DraftCreated,
            RubricId::from_ulid(Ulid::from_u128(1)),
            QuestionId::from_ulid(Ulid::from_u128(2)),
            LOCAL_TEACHER,
            timestamp(),
        );
        audit.append(event).unwrap();
        assert_eq!(audit.len().unwrap(), 1);
        assert_eq!(audit.events()[0].action, RubricAction::DraftCreated);
    }

    #[test]
    fn sequential_ids_are_distinct_and_deterministic() {
        let ids = SequentialIds::default();
        let first = ids.next_rubric_id();
        let second = ids.next_rubric_id();
        assert_ne!(first, second);
        let item = ids.next_rubric_item_id();
        assert_ne!(first.as_ulid().to_string(), item.as_ulid().to_string());
    }

    #[test]
    fn fixed_clock_is_stable() {
        let clock = FixedClock::new(timestamp());
        assert_eq!(clock.now(), timestamp());
        assert_eq!(clock.now(), clock.now());
        let _ = RubricSettings::default();
        let _ = clock.granularity();
    }

    #[test]
    fn fixed_clock_with_granularity_is_debug_only() {
        let clock =
            FixedClock::new(timestamp()).with_granularity(MilliScore::from_milli(100).unwrap());
        assert_eq!(clock.granularity().as_milli(), 100);
    }
}
