//! 适配端口（`RubricSource`/`JudgeBackend`/`ClassifierBackend`/`IdGenerator`/
//! `IdempotencyStore`）与 fixture 实现。
//!
//! TODO（并行依赖）：`eg-rubric` 稳定后实现 `RubricSource` 适配；`eg-inference`
//! 稳定后实现 `JudgeBackend` 适配（含 fixture 回放、校准 id、超时）；本 crate 不
//! 修改其他 crate。所有 trait 均为纯接口，便于单测注入。

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use eg_domain::{
    AnswerKey, CalibrationId, Confidence, EngineStamp, GradeRecord, MilliScore, NormalizedAnswer,
    Question, QuestionId, QuestionKind, RawAnswer, Rubric, RubricItemId, SubmissionId, TaskId,
    Timestamp, Ulid,
};

// ---------------------------------------------------------------------------
// 评分标准来源
// ---------------------------------------------------------------------------

/// 生效评分标准可用性（草稿不算生效规则，规范 §3/§9）。
#[derive(Debug, Clone, PartialEq)]
#[allow(
    clippy::large_enum_variant,
    reason = "Confirmed 携带完整规则是接口语义；测试/运行时按值传递规则快照"
)]
pub enum RubricAvailability {
    /// 已确认生效。
    Confirmed(Rubric),
    /// 草稿（未确认，不参与判分）。
    Draft,
    /// 不存在。
    Missing,
}

/// 评分标准来源适配器（生产实现见 [`crate::adapters::rubric::RubricStoreSource`]）。
pub trait RubricSource: Send + Sync {
    /// 查询题目的规则可用性（仅返回已确认规则）。
    fn availability(&self, question_id: QuestionId) -> RubricAvailability;

    /// 当前生效规则版本（无生效规则为 0；进幂等键/引擎指纹）。
    fn active_version(&self, _question_id: QuestionId) -> u32 {
        0
    }

    /// 来源身份（进引擎指纹；如 `rubric-store-v1`）。
    fn identity(&self) -> String {
        "fixture-rubric-v1".to_owned()
    }
}

/// fixture 规则源：显式登记已确认规则与草稿。
#[derive(Debug, Default)]
pub struct FixtureRubrics {
    confirmed: BTreeMap<QuestionId, Rubric>,
    drafts: BTreeSet<QuestionId>,
}

impl FixtureRubrics {
    /// 空规则源。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记已确认规则。
    pub fn insert_confirmed(&mut self, question_id: QuestionId, rubric: Rubric) {
        self.confirmed.insert(question_id, rubric);
    }

    /// 登记草稿（不生效）。
    pub fn insert_draft(&mut self, question_id: QuestionId) {
        self.drafts.insert(question_id);
    }
}

impl RubricSource for FixtureRubrics {
    fn availability(&self, question_id: QuestionId) -> RubricAvailability {
        if let Some(rubric) = self.confirmed.get(&question_id) {
            return RubricAvailability::Confirmed(rubric.clone());
        }
        if self.drafts.contains(&question_id) {
            return RubricAvailability::Draft;
        }
        RubricAvailability::Missing
    }
}

// ---------------------------------------------------------------------------
// 模型判定后端
// ---------------------------------------------------------------------------

/// 模型判定请求（字段对应 `SPEC-grade.md` §5.2 请求字段）。
#[derive(Debug)]
pub struct JudgeRequest<'a> {
    /// 提交 id。
    pub submission_id: SubmissionId,
    /// 任务 id（按任务鉴权/审计）。
    pub task_id: TaskId,
    /// 题目。
    pub question: &'a Question,
    /// 具体答案键（无具体答案时为 `None`，模型不得自造答案）。
    pub key: Option<&'a eg_domain::ConcreteKey>,
    /// 已确认评分标准（可空）。
    pub rubric: Option<&'a Rubric>,
    /// 学生原始答案（只含本题上下文）。
    pub answer: &'a RawAnswer,
    /// 学科归一化结果（可空；归一化失败时为空且路由已转人工）。
    pub normalized: Option<&'a NormalizedAnswer>,
    /// 答案级识别置信度。
    pub recognition_confidence: Confidence,
    /// 运行模式。
    pub mode: eg_domain::GradeMode,
    /// 第几次尝试（0 为首次；修复重试 >0）。
    pub attempt: u32,
    /// 修复提示（上一次输出不合法原因）。
    pub repair_hint: Option<String>,
    /// 分数粒度。
    pub granularity: MilliScore,
    /// 理由长度上限（字）。
    pub max_reason_chars: usize,
    /// 单次判定超时（秒；`grade.judge_timeout_s`）。
    pub judge_timeout_s: u32,
}

/// 模型判定结论类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JudgeVerdict {
    /// 一致。
    Match,
    /// 部分一致。
    Partial,
    /// 不一致。
    Mismatch,
    /// 不可判定（附原因）。
    Ungradable(eg_domain::UngradableReason),
}

/// 模型判定原始响应（未校验；校验见 `validate` 模块）。
#[derive(Debug, Clone, PartialEq)]
pub struct JudgeResponse {
    /// 真实推理服务已落库的 `ModelCall`（生产由 `eg-inference` 返回；fixture 为空时由
    /// `grade` 自行构造未落库记录，供测试/回放使用）。
    pub model_call: Option<eg_domain::ModelCall>,
    /// 结论类型。
    pub verdict: JudgeVerdict,
    /// 声明得分（不可判定时为空）。
    pub awarded: Option<MilliScore>,
    /// 满足的评分项（按评分标准判定时必填）。
    pub satisfied: Vec<RubricItemId>,
    /// 缺失的评分项。
    pub missing: Vec<RubricItemId>,
    /// 命中候选序号。
    pub matched_alternative: Option<u32>,
    /// 中文理由。
    pub reason: String,
    /// 模型原始置信度（校准前）。
    pub confidence: Confidence,
    /// 校准记录 id（为空禁止自动放行）。
    pub calibration_id: Option<CalibrationId>,
    /// 证据块最低置信度（无证据块时为空 → 置信度上限 0.5）。
    pub evidence_confidence: Option<Confidence>,
    /// 引擎指纹。
    pub engine: EngineStamp,
    /// 延迟（毫秒）。
    pub latency_ms: u64,
    /// 费用（百万分之一货币单位）。
    pub cost_micros: u64,
    /// 已通过 Schema 校验的原始结构化输出（模拟 inference 返回）。
    pub output: serde_json::Value,
}

/// 模型调用失败（全部映射为不可判定/人工，绝不猜）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JudgeFailure {
    /// 超时。
    Timeout,
    /// 后端不可用。
    BackendUnavailable {
        /// 原因。
        reason: String,
    },
    /// 内部错误（非 Schema 问题）。
    Internal {
        /// 原因。
        reason: String,
    },
    /// 模型输出未通过推理层 Schema（可修复重试一次）。
    InvalidOutput {
        /// 原因。
        reason: String,
    },
    /// 策略/授权/许可拦截（0 次外联；转人工，可重试）。
    PolicyBlocked {
        /// 原因。
        reason: String,
    },
}

impl JudgeFailure {
    /// 可读原因。
    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Self::Timeout => "模型判定超时".to_owned(),
            Self::BackendUnavailable { reason }
            | Self::Internal { reason }
            | Self::InvalidOutput { reason }
            | Self::PolicyBlocked { reason } => reason.clone(),
        }
    }
}

/// 判定后端适配器（TODO：接 `eg-inference` judge 角色与 fixture 回放）。
pub trait JudgeBackend: Send + Sync {
    /// 后端身份（进引擎指纹/幂等键；不得含个人信息）。
    fn identity(&self) -> String;

    /// 执行一次判定（纯请求/响应；重试由 `grade` 控制）。
    ///
    /// # Errors
    ///
    /// 超时/后端不可用/内部错误返回 [`JudgeFailure`]。
    fn judge(&self, request: &JudgeRequest<'_>) -> Result<JudgeResponse, JudgeFailure>;
}

/// fixture 判定步骤。
#[derive(Debug, Clone)]
#[allow(
    clippy::large_enum_variant,
    reason = "fixture 脚本按值保存判定响应；仅测试路径，大 variant 可接受"
)]
pub enum ScriptedStep {
    /// 返回给定响应。
    Respond(JudgeResponse),
    /// 返回失败。
    Fail(JudgeFailure),
}

/// fixture 判定后端：按顺序返回脚本化响应（可断言调用次数/请求内容）。
#[derive(Debug)]
pub struct ScriptedJudge {
    steps: Mutex<VecDeque<ScriptedStep>>,
    calls: Mutex<Vec<JudgeCallRecord>>,
    identity: String,
}

/// 已发生的调用（测试断言用）。
#[derive(Debug, Clone, PartialEq)]
pub struct JudgeCallRecord {
    /// 题目 id。
    pub question_id: QuestionId,
    /// 尝试次数。
    pub attempt: u32,
    /// 是否携带具体答案键（断言「缺答案不调模型」）。
    pub has_concrete_key: bool,
    /// 是否携带评分标准。
    pub has_rubric: bool,
    /// 模式。
    pub mode: eg_domain::GradeMode,
}

impl ScriptedJudge {
    /// 构造 fixture 后端。
    #[must_use]
    pub fn new(steps: Vec<ScriptedStep>) -> Self {
        Self {
            steps: Mutex::new(steps.into()),
            calls: Mutex::new(Vec::new()),
            identity: "fixture-judge-v1".to_owned(),
        }
    }

    /// 已发生调用记录。
    #[must_use]
    pub fn calls(&self) -> Vec<JudgeCallRecord> {
        self.calls
            .lock()
            .map_or_else(|_| Vec::new(), |calls| calls.clone())
    }

    /// 剩余未消费步骤数。
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.steps.lock().map_or(0, |steps| steps.len())
    }
}

impl JudgeBackend for ScriptedJudge {
    fn identity(&self) -> String {
        self.identity.clone()
    }

    fn judge(&self, request: &JudgeRequest<'_>) -> Result<JudgeResponse, JudgeFailure> {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(JudgeCallRecord {
                question_id: request.question.id,
                attempt: request.attempt,
                has_concrete_key: request.key.is_some(),
                has_rubric: request.rubric.is_some(),
                mode: request.mode,
            });
        }
        let step = self
            .steps
            .lock()
            .map_err(|_| JudgeFailure::Internal {
                reason: "fixture 判定后端锁失败".to_owned(),
            })?
            .pop_front();
        match step {
            Some(ScriptedStep::Respond(response)) => Ok(response),
            Some(ScriptedStep::Fail(failure)) => Err(failure),
            None => Err(JudgeFailure::Internal {
                reason: "fixture 判定步骤已耗尽".to_owned(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// 题型分类后端（仅 R9 使用）
// ---------------------------------------------------------------------------

/// 分类结果。
#[derive(Debug, Clone, PartialEq)]
pub struct ClassificationResponse {
    /// 真实推理服务已落库的 `ModelCall`（可空）。
    pub model_call: Option<eg_domain::ModelCall>,
    /// 分类题型。
    pub kind: QuestionKind,
    /// 分类置信度（<0.9 直接转人工）。
    pub confidence: Confidence,
    /// 引擎指纹（写入分类 `ModelCall`）。
    pub engine: EngineStamp,
    /// 延迟（毫秒）。
    pub latency_ms: u64,
}

/// 分类后端适配器（TODO：接 `eg-inference` router 角色）。
pub trait ClassifierBackend: Send + Sync {
    /// 后端身份。
    fn identity(&self) -> String;

    /// 分类一次（只输出题型与置信度，不得输出答案/分数）。
    ///
    /// # Errors
    ///
    /// 超时/不可用返回 [`JudgeFailure`]。
    fn classify(
        &self,
        question: &Question,
        task_id: TaskId,
    ) -> Result<ClassificationResponse, JudgeFailure>;
}

/// fixture 分类后端：固定返回一个结果或失败。
#[derive(Debug)]
pub struct FixtureClassifier {
    outcome: Result<ClassificationResponse, JudgeFailure>,
}

impl FixtureClassifier {
    /// 固定成功结果。
    #[must_use]
    pub fn new(kind: QuestionKind, confidence: Confidence, engine: EngineStamp) -> Self {
        Self {
            outcome: Ok(ClassificationResponse {
                model_call: None,
                kind,
                confidence,
                engine,
                latency_ms: 5,
            }),
        }
    }

    /// 固定失败。
    #[must_use]
    pub fn failing(failure: JudgeFailure) -> Self {
        Self {
            outcome: Err(failure),
        }
    }
}

impl ClassifierBackend for FixtureClassifier {
    fn identity(&self) -> String {
        "fixture-classifier-v1".to_owned()
    }

    fn classify(
        &self,
        _question: &Question,
        _task_id: TaskId,
    ) -> Result<ClassificationResponse, JudgeFailure> {
        self.outcome.clone()
    }
}

// ---------------------------------------------------------------------------
// id / 时钟 / 幂等存储
// ---------------------------------------------------------------------------

/// id 生成器（生产实现见 [`crate::adapters::platform::PlatformIdGenerator`]）。
pub trait IdGenerator: Send + Sync {
    /// 生成下一个 ULID（26 字符，单调）；平台失败时返回错误（调用方必须显式失败）。
    ///
    /// # Errors
    ///
    /// id 源不可用时返回 [`IdSourceError`]。
    fn try_next_ulid(&self) -> Result<Ulid, IdSourceError>;

    /// 测试/回放便捷方法（错误时返回 NIL；生产路径必须用 [`Self::try_next_ulid`]）。
    fn next_ulid(&self) -> Ulid {
        self.try_next_ulid().unwrap_or(Ulid::NIL)
    }
}

/// id 源错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("id 生成失败：{0}")]
pub struct IdSourceError(pub String);

/// 确定性 id 生成器（测试/回放；不读时钟）。
#[derive(Debug, Default)]
pub struct SequentialIds {
    next: AtomicU64,
}

impl SequentialIds {
    /// 从 1 开始。
    #[must_use]
    pub fn new() -> Self {
        Self {
            next: AtomicU64::new(1),
        }
    }
}

impl IdGenerator for SequentialIds {
    fn try_next_ulid(&self) -> Result<Ulid, IdSourceError> {
        let value = self.next.fetch_add(1, Ordering::Relaxed);
        Ok(Ulid::from_parts(0, u128::from(value)))
    }
}

/// 固定时钟（测试）。
#[derive(Debug, Clone)]
pub struct FixedClock {
    now: Timestamp,
}

impl FixedClock {
    /// 构造。
    #[must_use]
    pub fn new(now: Timestamp) -> Self {
        Self { now }
    }
}

impl eg_domain::Clock for FixedClock {
    fn now(&self) -> Timestamp {
        self.now
    }
}

/// 幂等键（`SPEC-grade.md` §8 重复判分）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IdempotencyKey {
    /// 提交 id。
    pub submission_id: SubmissionId,
    /// 题目 id。
    pub question_id: QuestionId,
    /// 答案键版本。
    pub key_version: u32,
    /// 规则版本。
    pub rubric_version: u32,
    /// 识别内容哈希（原始答案文本/LaTeX）。
    pub recognition_hash: String,
    /// 引擎指纹哈希（配置 + 学科能力 + 判定后端身份）。
    pub engine_fingerprint: String,
    /// 模式。
    pub mode: eg_domain::GradeMode,
}

/// 幂等存储适配器（TODO：生产接 `eg-platform::repo` 持久化）。
pub trait IdempotencyStore: Send + Sync {
    /// 命中返回既有判分记录。
    fn lookup(&self, key: &IdempotencyKey) -> Option<GradeRecord>;

    /// 保存记录（重复写入以首次为准）。
    fn store(&self, key: IdempotencyKey, record: GradeRecord);
}

/// 内存幂等存储（测试；进程内不重复执行）。
#[derive(Debug, Default)]
pub struct InMemoryIdempotency {
    records: Mutex<HashMap<IdempotencyKey, GradeRecord>>,
}

impl InMemoryIdempotency {
    /// 空存储。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前记录数。
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

impl IdempotencyStore for InMemoryIdempotency {
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

/// 构造判定请求所需的答案键引用（类型安全辅助）。
#[must_use]
pub fn concrete_key(key: &AnswerKey) -> Option<&eg_domain::ConcreteKey> {
    key.concrete()
}
