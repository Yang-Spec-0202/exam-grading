//! 复核数据源/审计/撤销栈/重判端口与内存 fixture。
//!
//! 平台缺口（见 `docs/dev/research/review.md`）：`eg-platform::repo` 目前只有
//! `append_grade_record`/`list_grade_records(conn, submission_id)`（自由函数、需
//! rusqlite `Connection`），缺少跨提交队列/最新修订视图/按模式状态抽检过滤/游标分页、
//! 提交级排除读写与撤销栈持久化；`eg-platform::audit` 的 `AuditLog` 需 `Database`+
//! `UlidFactory`，且 `NewAuditEntry` 无命令幂等键/前后快照字段。故本模块先以 trait
//! 适配，生产实现为后续 platform 扩展任务（本 crate 不改其他 crate）。

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Mutex;

use eg_domain::{
    ComparatorTrace, ComparatorTraceId, GradeId, GradeRecord, ModelCall, ModelCallId, QuestionId,
    ReviewReasonKind, SubmissionExclusion, SubmissionId, Timestamp,
};

use crate::model::{
    AnswerView, AuditRecord, RegradeRequest, RegradeResult, ReviewError, ReviewMetadata,
};
use crate::undo::UndoEntry;

// ---------------------------------------------------------------------------
// 数据源（只读记录 + 追加修订）
// ---------------------------------------------------------------------------

/// 复核数据源（生产接 `platform::repo` + 识别/资产只读查询）。
pub trait ReviewRepository: Send + Sync {
    /// 按判分记录 id 取记录（任意修订）。
    fn get(&self, grade_id: GradeId) -> Option<GradeRecord>;

    /// 某题的全部修订（按 revision 升序）。
    fn revisions(&self, submission_id: SubmissionId, question_id: QuestionId) -> Vec<GradeRecord>;

    /// 所有（提交,题）的最新修订（队列数据源）。
    fn all_latest(&self) -> Vec<GradeRecord>;

    /// 追加新修订（只追加；revision 必须为当前最新+1；同 id 重复写视为幂等无操作）。
    ///
    /// # Errors
    ///
    /// 修订号非法/归属不一致返回 [`ReviewError`]。
    fn append(&self, record: GradeRecord) -> Result<(), ReviewError>;

    /// 保存提交级排除。
    ///
    /// # Errors
    ///
    /// 存储失败返回错误。
    fn save_exclusion(&self, exclusion: SubmissionExclusion) -> Result<(), ReviewError>;

    /// 清除提交级排除（撤销用）。
    ///
    /// # Errors
    ///
    /// 存储失败返回错误。
    fn clear_exclusion(&self, submission_id: SubmissionId) -> Result<(), ReviewError>;

    /// 当前提交级排除。
    fn exclusion(&self, submission_id: SubmissionId) -> Option<SubmissionExclusion>;

    /// 题目/学生元数据。
    fn metadata(&self, record: &GradeRecord) -> ReviewMetadata;

    /// 学生答案视图（可空：懒加载失败降级为文本摘要）。
    fn answer(&self, record: &GradeRecord) -> Option<AnswerView>;

    /// 参考答案视图。
    fn reference(&self, question_id: QuestionId) -> Option<AnswerView>;

    /// 比较器轨迹。
    fn comparator_trace(&self, id: ComparatorTraceId) -> Option<ComparatorTrace>;

    /// 模型调用。
    fn model_call(&self, id: ModelCallId) -> Option<ModelCall>;

    /// 已追加修订次数（测试/指标；默认 0）。
    fn append_count(&self) -> usize {
        0
    }
}

/// 内存数据源（测试/回放）。
#[derive(Debug, Default)]
pub struct InMemoryReviewRepository {
    state: Mutex<RepoState>,
}

#[derive(Debug, Default)]
struct RepoState {
    revisions: BTreeMap<(SubmissionId, QuestionId), Vec<GradeRecord>>,
    metadata: HashMap<QuestionId, ReviewMetadata>,
    answers: HashMap<QuestionId, AnswerView>,
    references: HashMap<QuestionId, AnswerView>,
    traces: HashMap<ComparatorTraceId, ComparatorTrace>,
    model_calls: HashMap<ModelCallId, ModelCall>,
    exclusions: HashMap<SubmissionId, SubmissionExclusion>,
    append_count: usize,
}

impl InMemoryReviewRepository {
    /// 空仓库。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 预置一条记录（测试夹具；不做修订连续性校验）。
    pub fn insert(&self, record: GradeRecord) {
        if let Ok(mut state) = self.state.lock() {
            state
                .revisions
                .entry((record.submission_id, record.question_id))
                .or_default()
                .push(record);
        }
    }

    /// 设置题目元数据。
    pub fn set_metadata(&self, question_id: QuestionId, metadata: ReviewMetadata) {
        if let Ok(mut state) = self.state.lock() {
            state.metadata.insert(question_id, metadata);
        }
    }

    /// 设置学生答案视图。
    pub fn set_answer(&self, question_id: QuestionId, answer: AnswerView) {
        if let Ok(mut state) = self.state.lock() {
            state.answers.insert(question_id, answer);
        }
    }

    /// 设置参考答案视图。
    pub fn set_reference(&self, question_id: QuestionId, reference: AnswerView) {
        if let Ok(mut state) = self.state.lock() {
            state.references.insert(question_id, reference);
        }
    }

    /// 预置比较器轨迹。
    pub fn insert_trace(&self, trace: ComparatorTrace) {
        if let Ok(mut state) = self.state.lock() {
            state.traces.insert(trace.id, trace);
        }
    }

    /// 预置模型调用。
    pub fn insert_model_call(&self, call: ModelCall) {
        if let Ok(mut state) = self.state.lock() {
            state.model_calls.insert(call.id, call);
        }
    }

    /// 预置提交级排除。
    pub fn set_exclusion(&self, exclusion: SubmissionExclusion) {
        if let Ok(mut state) = self.state.lock() {
            state.exclusions.insert(exclusion.submission_id, exclusion);
        }
    }

    /// 最新修订号（不存在时为 0）。
    #[must_use]
    pub fn latest_revision(&self, submission_id: SubmissionId, question_id: QuestionId) -> u32 {
        self.state
            .lock()
            .ok()
            .and_then(|state| {
                state
                    .revisions
                    .get(&(submission_id, question_id))
                    .and_then(|records| records.last().map(|record| record.revision))
            })
            .unwrap_or(0)
    }
}

impl ReviewRepository for InMemoryReviewRepository {
    fn get(&self, grade_id: GradeId) -> Option<GradeRecord> {
        self.state.lock().ok().and_then(|state| {
            state
                .revisions
                .values()
                .flat_map(|records| records.iter())
                .find(|record| record.id == grade_id)
                .cloned()
        })
    }

    fn revisions(&self, submission_id: SubmissionId, question_id: QuestionId) -> Vec<GradeRecord> {
        self.state
            .lock()
            .ok()
            .and_then(|state| state.revisions.get(&(submission_id, question_id)).cloned())
            .unwrap_or_default()
    }

    fn all_latest(&self) -> Vec<GradeRecord> {
        self.state.lock().map_or_else(
            |_| Vec::new(),
            |state| {
                state
                    .revisions
                    .values()
                    .filter_map(|records| records.last().cloned())
                    .collect()
            },
        )
    }

    fn append(&self, record: GradeRecord) -> Result<(), ReviewError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ReviewError::Repository("仓库锁失败".to_owned()))?;
        let key = (record.submission_id, record.question_id);
        let records = state.revisions.entry(key).or_default();
        if let Some(last) = records.last() {
            if last.id == record.id {
                return Ok(());
            }
            if record.revision != last.revision + 1 {
                return Err(ReviewError::InvalidRevision(record.revision));
            }
        } else if record.revision != 1 {
            return Err(ReviewError::InvalidRevision(record.revision));
        }
        records.push(record);
        state.append_count += 1;
        Ok(())
    }

    fn save_exclusion(&self, exclusion: SubmissionExclusion) -> Result<(), ReviewError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ReviewError::Repository("仓库锁失败".to_owned()))?;
        state.exclusions.insert(exclusion.submission_id, exclusion);
        Ok(())
    }

    fn clear_exclusion(&self, submission_id: SubmissionId) -> Result<(), ReviewError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ReviewError::Repository("仓库锁失败".to_owned()))?;
        state.exclusions.remove(&submission_id);
        Ok(())
    }

    fn exclusion(&self, submission_id: SubmissionId) -> Option<SubmissionExclusion> {
        self.state
            .lock()
            .ok()
            .and_then(|state| state.exclusions.get(&submission_id).cloned())
    }

    fn metadata(&self, record: &GradeRecord) -> ReviewMetadata {
        self.state
            .lock()
            .ok()
            .and_then(|state| state.metadata.get(&record.question_id).cloned())
            .unwrap_or_default()
    }

    fn answer(&self, record: &GradeRecord) -> Option<AnswerView> {
        self.state
            .lock()
            .ok()
            .and_then(|state| state.answers.get(&record.question_id).cloned())
    }

    fn reference(&self, question_id: QuestionId) -> Option<AnswerView> {
        self.state
            .lock()
            .ok()
            .and_then(|state| state.references.get(&question_id).cloned())
    }

    fn comparator_trace(&self, id: ComparatorTraceId) -> Option<ComparatorTrace> {
        self.state
            .lock()
            .ok()
            .and_then(|state| state.traces.get(&id).cloned())
    }

    fn model_call(&self, id: ModelCallId) -> Option<ModelCall> {
        self.state
            .lock()
            .ok()
            .and_then(|state| state.model_calls.get(&id).cloned())
    }

    fn append_count(&self) -> usize {
        self.state.lock().map_or(0, |state| state.append_count)
    }
}

// ---------------------------------------------------------------------------
// 审计
// ---------------------------------------------------------------------------

/// 审计端口（只追加；生产接 `platform::audit::AuditLog`）。
pub trait AuditSink: Send + Sync {
    /// 追加审计。
    ///
    /// # Errors
    ///
    /// 存储失败返回错误。
    fn append(&self, record: AuditRecord) -> Result<(), ReviewError>;

    /// 全部审计（按时间升序）。
    fn list(&self) -> Vec<AuditRecord>;

    /// 按实体查询。
    fn by_entity(&self, entity_type: &str, entity_id: &str) -> Vec<AuditRecord>;
}

/// 内存审计（测试/回放）。
#[derive(Debug, Default)]
pub struct InMemoryAuditSink {
    records: Mutex<Vec<AuditRecord>>,
}

impl InMemoryAuditSink {
    /// 空审计。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 条数。
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

impl AuditSink for InMemoryAuditSink {
    fn append(&self, record: AuditRecord) -> Result<(), ReviewError> {
        self.records
            .lock()
            .map_err(|_| ReviewError::Audit("审计锁失败".to_owned()))?
            .push(record);
        Ok(())
    }

    fn list(&self) -> Vec<AuditRecord> {
        self.records
            .lock()
            .map_or_else(|_| Vec::new(), |records| records.clone())
    }

    fn by_entity(&self, entity_type: &str, entity_id: &str) -> Vec<AuditRecord> {
        self.list()
            .into_iter()
            .filter(|record| record.entity_type == entity_type && record.entity_id == entity_id)
            .collect()
    }
}

// ---------------------------------------------------------------------------
// 撤销栈
// ---------------------------------------------------------------------------

/// 撤销栈端口（生产持久化到 platform；进程重启后仍有效）。
pub trait UndoStore: Send + Sync {
    /// 压入撤销栈（超过深度丢弃最旧）。
    fn push_undo(&self, entry: UndoEntry);

    /// 弹出撤销条目。
    fn pop_undo(&self) -> Option<UndoEntry>;

    /// 压入重做栈。
    fn push_redo(&self, entry: UndoEntry);

    /// 弹出重做条目。
    fn pop_redo(&self) -> Option<UndoEntry>;

    /// 清空重做栈（新命令使重做失效）。
    fn clear_redo(&self);

    /// 撤销栈深度。
    fn undo_len(&self) -> usize;

    /// 重做栈深度。
    fn redo_len(&self) -> usize;
}

/// 内存撤销栈（测试/回放；深度由配置设定）。
#[derive(Debug)]
pub struct InMemoryUndoStore {
    state: Mutex<StackState>,
    depth: usize,
}

#[derive(Debug, Default)]
struct StackState {
    undo: VecDeque<UndoEntry>,
    redo: VecDeque<UndoEntry>,
}

impl InMemoryUndoStore {
    /// 构造（`depth` 至少 50，规范要求）。
    #[must_use]
    pub fn new(depth: usize) -> Self {
        Self {
            state: Mutex::new(StackState::default()),
            depth: depth.max(50),
        }
    }
}

impl UndoStore for InMemoryUndoStore {
    fn push_undo(&self, entry: UndoEntry) {
        if let Ok(mut state) = self.state.lock() {
            if state.undo.len() >= self.depth {
                state.undo.pop_front();
            }
            state.undo.push_back(entry);
        }
    }

    fn pop_undo(&self) -> Option<UndoEntry> {
        self.state
            .lock()
            .ok()
            .and_then(|mut state| state.undo.pop_back())
    }

    fn push_redo(&self, entry: UndoEntry) {
        if let Ok(mut state) = self.state.lock() {
            if state.redo.len() >= self.depth {
                state.redo.pop_front();
            }
            state.redo.push_back(entry);
        }
    }

    fn pop_redo(&self) -> Option<UndoEntry> {
        self.state
            .lock()
            .ok()
            .and_then(|mut state| state.redo.pop_back())
    }

    fn clear_redo(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.redo.clear();
        }
    }

    fn undo_len(&self) -> usize {
        self.state.lock().map_or(0, |state| state.undo.len())
    }

    fn redo_len(&self) -> usize {
        self.state.lock().map_or(0, |state| state.redo.len())
    }
}

// ---------------------------------------------------------------------------
// 重判
// ---------------------------------------------------------------------------

/// 重判端口（生产接 `eg-grade::Grader` 的幂等键；整卷任务由 app/平台调度）。
pub trait RegradeEngine: Send + Sync {
    /// 执行单题重判或整卷调度。
    ///
    /// # Errors
    ///
    /// 后端不可用/超时/非法输出返回错误（调用方保留原判定）。
    fn regrade(&self, request: &RegradeRequest) -> Result<RegradeResult, ReviewError>;
}

/// fixture 重判步骤。
#[derive(Debug, Clone)]
pub enum FixtureRegradeStep {
    /// 返回新判分记录。
    Regraded(GradeRecord),
    /// 调度整卷任务。
    SubmissionTask(String),
    /// 无法重判但转标准待复核。
    FallbackToQueue(GradeRecord),
    /// 失败（后端不可用/超时）。
    Failed(ReviewError),
}

/// fixture 重判后端（按幂等键去重，断言「重跑幂等」）。
#[derive(Debug)]
pub struct FixtureRegradeEngine {
    steps: Mutex<VecDeque<FixtureRegradeStep>>,
    calls: Mutex<Vec<RegradeRequest>>,
    cache: Mutex<HashMap<String, RegradeResult>>,
}

impl FixtureRegradeEngine {
    /// 构造。
    #[must_use]
    pub fn new(steps: Vec<FixtureRegradeStep>) -> Self {
        Self {
            steps: Mutex::new(steps.into()),
            calls: Mutex::new(Vec::new()),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// 实际执行次数（幂等命中不计）。
    #[must_use]
    pub fn executions(&self) -> usize {
        self.calls.lock().map_or(0, |calls| calls.len())
    }

    /// 全部请求（含幂等命中）。
    #[must_use]
    pub fn requests(&self) -> Vec<RegradeRequest> {
        self.calls
            .lock()
            .map_or_else(|_| Vec::new(), |calls| calls.clone())
    }
}

impl RegradeEngine for FixtureRegradeEngine {
    fn regrade(&self, request: &RegradeRequest) -> Result<RegradeResult, ReviewError> {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(request.clone());
        }
        if let Some(cached) = self
            .cache
            .lock()
            .ok()
            .and_then(|cache| cache.get(&request.idempotency_key).cloned())
        {
            return Ok(cached);
        }
        let step = self
            .steps
            .lock()
            .map_err(|_| ReviewError::RegradeFailed("fixture 锁失败".to_owned()))?
            .pop_front()
            .ok_or_else(|| ReviewError::RegradeFailed("fixture 步骤已耗尽".to_owned()))?;
        let result = match step {
            FixtureRegradeStep::Regraded(record) => RegradeResult {
                record: Some(record),
                task_id: None,
                fallback_to_queue: false,
            },
            FixtureRegradeStep::SubmissionTask(task_id) => RegradeResult {
                record: None,
                task_id: Some(task_id),
                fallback_to_queue: false,
            },
            FixtureRegradeStep::FallbackToQueue(record) => RegradeResult {
                record: Some(record),
                task_id: None,
                fallback_to_queue: true,
            },
            FixtureRegradeStep::Failed(error) => return Err(error),
        };
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(request.idempotency_key.clone(), result.clone());
        }
        Ok(result)
    }
}

/// 测试辅助：由已有记录派生下一修订的综合辅助。
#[must_use]
pub fn next_revision_record(previous: &GradeRecord, id: GradeId, at: Timestamp) -> GradeRecord {
    let mut record = previous.clone();
    record.id = id;
    record.revision = previous.revision + 1;
    record.created_at = at;
    record.reviewed_at = Some(at);
    record
}

/// 原因列表去重（队列过滤辅助）。
#[must_use]
pub fn dedup_reasons(mut reasons: Vec<ReviewReasonKind>) -> Vec<ReviewReasonKind> {
    reasons.sort_by_key(|reason| format!("{reason:?}"));
    reasons.dedup();
    reasons
}
