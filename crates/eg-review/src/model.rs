//! 复核领域模型：配置、队列/详情视图、命令、审计、错误（`SPEC-review.md` §3/§5）。

use eg_domain::{
    AssetId, ComparatorTrace, Confidence, ExcludeReason, GradeId, GradeMode, GradeRecord,
    GradeStatus, Judgement, MilliScore, ModelCall, QuestionId, ReviewReasonKind,
    SubmissionExclusion, SubmissionId, Timestamp, Ulid,
};

/// 无登录约束下的固定 actor（`SPEC-review.md` §2/§4）。
pub const ACTOR: &str = "local-teacher";

// ---------------------------------------------------------------------------
// 配置
// ---------------------------------------------------------------------------

/// 抽检不通过后的处理方式（`fast.fail_action`；v1 只允许弹窗询问）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FailAction {
    /// 弹窗询问（默认；未确认不重跑）。
    #[default]
    Prompt,
}

/// 重跑范围（默认单题；整卷需教师明确选择）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RegradeScope {
    /// 单题。
    #[default]
    Single,
    /// 整份提交。
    WholeSubmission,
}

impl RegradeScope {
    /// 审计标签。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::WholeSubmission => "whole_submission",
        }
    }
}

/// 复核配置（只读消费规范默认值；修改属 Ask first）。
#[derive(Debug, Clone, PartialEq)]
pub struct ReviewConfig {
    /// 分页大小（`ui.page_size`）。
    pub page_size: usize,
    /// 撤销深度（`ui.undo_depth`，默认 50）。
    pub undo_depth: usize,
    /// 抽检队列只读（`fast.sampled_readonly`，默认开；关闭需 Ask first）。
    pub sampled_readonly: bool,
    /// 抽检不通过处理（默认弹窗）。
    pub fail_action: FailAction,
    /// 弹窗默认重跑范围（默认单题）。
    pub fail_default_scope: RegradeScope,
    /// 批量上限（保护误操作；默认 500）。
    pub max_batch: usize,
    /// 判分配置（阈值/粒度由 `grade` 决定）。
    pub grade: eg_grade::GradeConfig,
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self {
            page_size: 20,
            undo_depth: 50,
            sampled_readonly: true,
            fail_action: FailAction::Prompt,
            fail_default_scope: RegradeScope::Single,
            max_batch: 500,
            grade: eg_grade::GradeConfig::default(),
        }
    }
}

impl ReviewConfig {
    /// 分数粒度。
    #[must_use]
    pub fn granularity(&self) -> MilliScore {
        self.grade.score_granularity
    }
}

// ---------------------------------------------------------------------------
// 原因码/审计
// ---------------------------------------------------------------------------

/// 复核原因码（`SPEC-review.md` §3.3；改分/排除必填）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReviewReasonCode {
    /// 识别错误。
    RecognitionError,
    /// 答案键歧义。
    AnswerKeyAmbiguity,
    /// 模型判错。
    ModelMisjudged,
    /// 部分给分。
    PartialCredit,
    /// 单位或精度。
    UnitOrPrecision,
    /// 其他。
    Other,
}

impl ReviewReasonCode {
    /// 审计/日志标签。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RecognitionError => "recognition_error",
            Self::AnswerKeyAmbiguity => "answer_key_ambiguity",
            Self::ModelMisjudged => "model_misjudged",
            Self::PartialCredit => "partial_credit",
            Self::UnitOrPrecision => "unit_or_precision",
            Self::Other => "other",
        }
    }
}

/// 审计结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditOutcome {
    /// 已应用。
    Applied,
    /// 被拒绝（只读/乐观锁/参数非法）。
    Rejected,
    /// 无副作用（抽检通过只落结论）。
    Noop,
}

impl AuditOutcome {
    /// 标签。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Rejected => "rejected",
            Self::Noop => "noop",
        }
    }
}

/// 抽检结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SamplingConclusion {
    /// 抽检通过。
    Passed,
    /// 抽检不通过。
    Failed,
}

/// 审计记录（只追加；生产接 `platform::audit`，先以本结构对齐字段）。
#[derive(Debug, Clone, PartialEq)]
pub struct AuditRecord {
    /// 审计 id。
    pub id: Ulid,
    /// 时间。
    pub at: Timestamp,
    /// actor（固定 [`ACTOR`]）。
    pub actor: String,
    /// 动作（如 `review.accept`）。
    pub action: String,
    /// 实体类型（`grade_record`/`submission`）。
    pub entity_type: String,
    /// 实体 id。
    pub entity_id: String,
    /// 提交 id。
    pub submission_id: SubmissionId,
    /// 题目 id（提交级排除为空）。
    pub question_id: Option<QuestionId>,
    /// 判分记录 id（抽检结论/提交级排除为空）。
    pub grade_id: Option<GradeId>,
    /// 命令幂等键。
    pub idempotency_key: String,
    /// 结果。
    pub outcome: AuditOutcome,
    /// 变更前记录快照。
    pub before: Option<GradeRecord>,
    /// 变更后记录快照。
    pub after: Option<GradeRecord>,
    /// 变更前提交级排除。
    pub before_exclusion: Option<SubmissionExclusion>,
    /// 变更后提交级排除。
    pub after_exclusion: Option<SubmissionExclusion>,
    /// 原因码。
    pub reason: Option<ReviewReasonCode>,
    /// 备注。
    pub note: String,
    /// 重跑范围。
    pub scope: Option<RegradeScope>,
    /// 是否弹窗确认（抽检不通过）。
    pub confirmed: Option<bool>,
    /// 抽检结论。
    pub sampling_conclusion: Option<SamplingConclusion>,
    /// 附加说明（错误信息/教学提示）。
    pub detail: Option<String>,
}

impl AuditRecord {
    /// 构造基础审计记录（其余字段由调用方补齐）。
    #[must_use]
    pub fn base(
        id: Ulid,
        at: Timestamp,
        action: impl Into<String>,
        entity_type: impl Into<String>,
        entity_id: impl Into<String>,
        submission_id: SubmissionId,
        idempotency_key: impl Into<String>,
    ) -> Self {
        Self {
            id,
            at,
            actor: ACTOR.to_owned(),
            action: action.into(),
            entity_type: entity_type.into(),
            entity_id: entity_id.into(),
            submission_id,
            question_id: None,
            grade_id: None,
            idempotency_key: idempotency_key.into(),
            outcome: AuditOutcome::Applied,
            before: None,
            after: None,
            before_exclusion: None,
            after_exclusion: None,
            reason: None,
            note: String::new(),
            scope: None,
            confirmed: None,
            sampling_conclusion: None,
            detail: None,
        }
    }
}

// ---------------------------------------------------------------------------
// 队列
// ---------------------------------------------------------------------------

/// 队列类型（标准/快速抽检/已确认）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum QueueKind {
    /// 标准待复核。
    Standard,
    /// 快速抽检待办。
    Sampling,
    /// 已人工复核（只读展示/可撤销）。
    Confirmed,
}

/// 队列过滤（`SPEC-review.md` §3.1）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QueueFilter {
    /// 队列类型（空 = 全部三种）。
    pub kinds: Vec<QueueKind>,
    /// 学号过滤。
    pub student_no: Option<String>,
    /// 题目过滤。
    pub question_id: Option<QuestionId>,
    /// 模式过滤。
    pub mode: Option<GradeMode>,
    /// 状态过滤。
    pub statuses: Vec<GradeStatus>,
    /// 原因过滤（仅标准队列）。
    pub reasons: Vec<ReviewReasonKind>,
    /// 只看抽检（`Some(true)`）/ 只看非抽检。
    pub sampled: Option<bool>,
}

/// 队列条目（视图模型；不含像素）。
#[derive(Debug, Clone, PartialEq)]
pub struct QueueItem {
    /// 判分记录 id（最新修订）。
    pub grade_id: GradeId,
    /// 提交 id。
    pub submission_id: SubmissionId,
    /// 题目 id。
    pub question_id: QuestionId,
    /// 学号（排序/过滤用；日志需脱敏）。
    pub student_no: String,
    /// 学生显示名（掩码）。
    pub student_display_name: String,
    /// 题号标签。
    pub question_label: String,
    /// 模式。
    pub mode: GradeMode,
    /// 抽检标记。
    pub sampled: bool,
    /// 状态。
    pub status: GradeStatus,
    /// 得分。
    pub awarded: MilliScore,
    /// 满分。
    pub max_score: MilliScore,
    /// 置信度。
    pub confidence: Confidence,
    /// 复核原因（标准队列风险排序用）。
    pub reasons: Vec<ReviewReasonKind>,
    /// 修订号。
    pub revision: u32,
    /// 所属队列。
    pub kind: QueueKind,
    /// 排序键（游标编码）。
    pub sort_key: String,
}

/// 队列计数（状态栏/首页）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QueueCounts {
    /// 标准待复核。
    pub standard_pending: usize,
    /// 快速抽检待办。
    pub sampling_pending: usize,
    /// 已确认。
    pub confirmed: usize,
}

/// 队列页（游标分页）。
#[derive(Debug, Clone, PartialEq)]
pub struct QueuePage {
    /// 条目。
    pub items: Vec<QueueItem>,
    /// 下一页游标（无更多时为 None）。
    pub next_cursor: Option<String>,
    /// 过滤后总数。
    pub total: usize,
    /// 计数。
    pub counts: QueueCounts,
}

/// 题目元数据（平台/识别模块提供；日志使用掩码显示名）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewMetadata {
    /// 学号。
    pub student_no: String,
    /// 学生显示名（掩码或后 4 位学号）。
    pub student_display_name: String,
    /// 题号标签。
    pub question_label: String,
    /// 卷面顺序。
    pub question_ordinal: u32,
}

impl Default for ReviewMetadata {
    fn default() -> Self {
        Self {
            student_no: "unknown".to_owned(),
            student_display_name: "学生***".to_owned(),
            question_label: "?".to_owned(),
            question_ordinal: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// 详情
// ---------------------------------------------------------------------------

/// 答案/参考答案视图（文本；图片按需懒加载，不在详情返回像素）。
#[derive(Debug, Clone, PartialEq)]
pub struct AnswerView {
    /// 文本。
    pub text: String,
    /// LaTeX。
    pub latex: Option<String>,
    /// 识别置信度（参考答案为空）。
    pub confidence: Option<Confidence>,
    /// 来源标签（识别/人工修正/参考答案）。
    pub source: String,
}

/// 详情（`SPEC-review.md` §3.1）。
#[derive(Debug, Clone, PartialEq)]
pub struct ReviewDetail {
    /// 队列条目（最新修订）。
    pub item: QueueItem,
    /// 最新判分记录。
    pub latest: GradeRecord,
    /// 历史修订（按 revision 升序，含最新）。
    pub history: Vec<GradeRecord>,
    /// 学生答案视图（懒加载失败时为空）。
    pub answer: Option<AnswerView>,
    /// 参考答案视图。
    pub reference: Option<AnswerView>,
    /// 判定。
    pub judgement: Judgement,
    /// 证据引用。
    pub evidence: Vec<eg_domain::EvidenceRef>,
    /// 资产 id（原图/裁片；按需懒加载）。
    pub assets: Vec<AssetId>,
    /// 比较器轨迹（按证据引用解析）。
    pub comparator_traces: Vec<ComparatorTrace>,
    /// 模型调用（按证据引用解析）。
    pub model_calls: Vec<ModelCall>,
    /// 是否只读（快速抽检队列且 `fast.sampled_readonly=开`）。
    pub read_only: bool,
    /// 允许命令（UI 可据此渲染按钮/快捷键）。
    pub allowed_commands: Vec<CommandKind>,
    /// 是否已转标准（快速记录被标准修订覆盖）。
    pub transferred_to_standard: bool,
}

/// 命令种类（UI/审计标签）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CommandKind {
    /// 接受。
    Accept,
    /// 改分。
    OverrideScore,
    /// 修文本重判。
    RegradeText,
    /// 题级排除。
    ExcludeQuestion,
    /// 提交级排除。
    ExcludeSubmission,
    /// 批量接受。
    BatchAccept,
    /// 批量改分。
    BatchOverrideScore,
    /// 抽检通过。
    SamplingPass,
    /// 抽检不通过。
    SamplingFail,
    /// 转标准复核。
    TransferToStandard,
    /// 撤销。
    Undo,
    /// 重做。
    Redo,
}

impl CommandKind {
    /// 动作标签。
    #[must_use]
    pub const fn action(self) -> &'static str {
        match self {
            Self::Accept => "review.accept",
            Self::OverrideScore => "review.override_score",
            Self::RegradeText => "review.regrade_text",
            Self::ExcludeQuestion => "review.exclude_question",
            Self::ExcludeSubmission => "review.exclude_submission",
            Self::BatchAccept => "review.batch_accept",
            Self::BatchOverrideScore => "review.batch_override_score",
            Self::SamplingPass => "review.sampling_pass",
            Self::SamplingFail => "review.sampling_fail",
            Self::TransferToStandard => "review.transfer_to_standard",
            Self::Undo => "review.undo",
            Self::Redo => "review.redo",
        }
    }
}

// ---------------------------------------------------------------------------
// 命令与结果
// ---------------------------------------------------------------------------

/// 修正后的文本。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorrectedText {
    /// 文本。
    pub text: String,
    /// LaTeX。
    pub latex: Option<String>,
}

/// 批量改分目标（新得分 + 原因码）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchScoreTarget {
    /// 记录与期望修订。
    pub target: BatchTarget,
    /// 新得分（千分之一分）。
    pub score_milli: i64,
    /// 原因码。
    pub reason: ReviewReasonCode,
}

/// 批量目标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchTarget {
    /// 判分记录 id。
    pub grade_id: GradeId,
    /// 期望修订（乐观锁）。
    pub expected_revision: u32,
}

/// 复核命令（`SPEC-review.md` §3.3）。
#[derive(Debug, Clone, PartialEq)]
pub enum ReviewCommand {
    /// 接受（标准）。
    Accept {
        /// 备注。
        note: Option<String>,
    },
    /// 改分（标准；原因码必填）。
    OverrideScore {
        /// 新得分（千分之一分）。
        score_milli: i64,
        /// 原因码。
        reason: ReviewReasonCode,
        /// 备注。
        note: Option<String>,
    },
    /// 修文本重判（标准；先落人工修正识别，再重判）。
    RegradeText {
        /// 修正文本。
        corrected: CorrectedText,
        /// 备注。
        note: Option<String>,
    },
    /// 题级排除（标准）。
    ExcludeQuestion {
        /// 排除原因（领域统计口径）。
        exclude_reason: ExcludeReason,
        /// 复核原因码。
        reason: ReviewReasonCode,
        /// 备注。
        note: Option<String>,
    },
    /// 提交级排除（标准）。
    ExcludeSubmission {
        /// 排除原因。
        reason: ExcludeReason,
        /// 备注。
        note: Option<String>,
    },
    /// 批量接受（标准）。
    BatchAccept {
        /// 目标列表。
        targets: Vec<BatchTarget>,
    },
    /// 批量改分（标准；每项得分与原因码）。
    BatchOverrideScore {
        /// 目标列表。
        items: Vec<BatchScoreTarget>,
    },
    /// 抽检通过（只写结论与审计，判分不变）。
    SamplingPass {
        /// 备注。
        note: Option<String>,
    },
    /// 抽检不通过（未确认不重跑）。
    SamplingFail {
        /// 重跑范围（默认单题）。
        scope: RegradeScope,
        /// 教师是否在弹窗中确认。
        confirmed: bool,
        /// 备注。
        note: Option<String>,
    },
    /// 一键转标准复核。
    TransferToStandard {
        /// 范围（单题/整份提交）。
        scope: RegradeScope,
        /// 备注。
        note: Option<String>,
    },
    /// 撤销。
    Undo,
    /// 重做。
    Redo,
}

impl ReviewCommand {
    /// 命令种类。
    #[must_use]
    pub const fn kind(&self) -> CommandKind {
        match self {
            Self::Accept { .. } => CommandKind::Accept,
            Self::OverrideScore { .. } => CommandKind::OverrideScore,
            Self::RegradeText { .. } => CommandKind::RegradeText,
            Self::ExcludeQuestion { .. } => CommandKind::ExcludeQuestion,
            Self::ExcludeSubmission { .. } => CommandKind::ExcludeSubmission,
            Self::BatchAccept { .. } => CommandKind::BatchAccept,
            Self::BatchOverrideScore { .. } => CommandKind::BatchOverrideScore,
            Self::SamplingPass { .. } => CommandKind::SamplingPass,
            Self::SamplingFail { .. } => CommandKind::SamplingFail,
            Self::TransferToStandard { .. } => CommandKind::TransferToStandard,
            Self::Undo => CommandKind::Undo,
            Self::Redo => CommandKind::Redo,
        }
    }

    /// 是否编辑命令（快速抽检只读校验用）。
    #[must_use]
    pub const fn is_edit(&self) -> bool {
        matches!(
            self,
            Self::Accept { .. }
                | Self::OverrideScore { .. }
                | Self::RegradeText { .. }
                | Self::ExcludeQuestion { .. }
                | Self::BatchAccept { .. }
                | Self::BatchOverrideScore { .. }
        )
    }

    /// 是否抽检命令。
    #[must_use]
    pub const fn is_sampling(&self) -> bool {
        matches!(self, Self::SamplingPass { .. } | Self::SamplingFail { .. })
    }
}

/// 命令信封（幂等键 + 乐观锁）。
#[derive(Debug, Clone, PartialEq)]
pub struct CommandEnvelope {
    /// 目标判分记录 id（撤销/重做/批量可为空）。
    pub target: Option<GradeId>,
    /// 命令幂等键（重复提交返回既有结果，不重复执行）。
    pub idempotency_key: String,
    /// 期望修订（乐观锁；单记录命令必填）。
    pub expected_revision: Option<u32>,
    /// 命令。
    pub command: ReviewCommand,
}

/// 命令结果。
#[derive(Debug, Clone, PartialEq)]
pub struct CommandResult {
    /// 审计 id（本次命令的审计记录）。
    pub audit_id: Ulid,
    /// 结果。
    pub outcome: CommandOutcome,
}

/// 命令结果类型。
#[derive(Debug, Clone, PartialEq)]
pub enum CommandOutcome {
    /// 已追加新修订。
    Record {
        /// 判分记录 id。
        grade_id: GradeId,
        /// 新修订号。
        revision: u32,
        /// 新状态。
        status: GradeStatus,
        /// 模式。
        mode: GradeMode,
    },
    /// 抽检通过（判分不变）。
    SamplingPassed {
        /// 判分记录 id。
        grade_id: GradeId,
    },
    /// 抽检不通过但未确认：返回弹窗协议事件，不重跑。
    SamplingPrompt {
        /// 弹窗事件。
        prompt: SamplingPrompt,
    },
    /// 整卷重跑任务已调度（由 app 执行）。
    RegradeScheduled {
        /// 任务 id。
        task_id: String,
        /// 范围。
        scope: RegradeScope,
    },
    /// 提交级排除已应用。
    SubmissionExcluded {
        /// 提交 id。
        submission_id: SubmissionId,
    },
    /// 批量结果。
    Batch {
        /// 批量结果。
        batch: BatchOutcome,
    },
    /// 撤销/重做已应用。
    Undone {
        /// 目标记录。
        grade_id: Option<GradeId>,
        /// 方向（true=撤销，false=重做）。
        undone: bool,
    },
}

/// 抽检不通过弹窗协议事件（`app` 展示；默认单题）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SamplingPrompt {
    /// 判分记录 id。
    pub grade_id: GradeId,
    /// 提交 id。
    pub submission_id: SubmissionId,
    /// 题目 id。
    pub question_id: QuestionId,
    /// 默认范围。
    pub default_scope: RegradeScope,
    /// 可选范围。
    pub options: Vec<RegradeScope>,
    /// 提示文案。
    pub message: String,
}

/// 批量失败项。
#[derive(Debug, Clone, PartialEq)]
pub struct BatchFailure {
    /// 判分记录 id。
    pub grade_id: GradeId,
    /// 失败原因。
    pub error: ReviewError,
}

/// 批量结果（逐项，已成功不回滚）。
#[derive(Debug, Clone, PartialEq)]
pub struct BatchOutcome {
    /// 成功项。
    pub succeeded: Vec<CommandResult>,
    /// 失败项。
    pub failed: Vec<BatchFailure>,
    /// 审计 id（批量汇总 + 逐项）。
    pub audit_ids: Vec<Ulid>,
}

// ---------------------------------------------------------------------------
// 重判
// ---------------------------------------------------------------------------

/// 重判请求（单题）。
#[derive(Debug, Clone, PartialEq)]
pub struct RegradeRequest {
    /// 期望重判的记录（最新修订）。
    pub record: GradeRecord,
    /// 目标模式（转标准时 Standard；抽检重跑保持当前模式）。
    pub mode: GradeMode,
    /// 修正文本（修文本重判时）。
    pub corrected: Option<CorrectedText>,
    /// 范围（单题/整卷；整卷由引擎调度任务）。
    pub scope: RegradeScope,
    /// 原因码。
    pub reason: Option<ReviewReasonCode>,
    /// 幂等键（经 `grade` 幂等键：提交+题+版本+识别哈希+引擎指纹+模式）。
    pub idempotency_key: String,
}

/// 重判结果。
#[derive(Debug, Clone, PartialEq)]
pub struct RegradeResult {
    /// 新判分记录（单题重判成功时）。
    pub record: Option<GradeRecord>,
    /// 整卷任务 id（范围=整卷时）。
    pub task_id: Option<String>,
    /// 是否无法重判、已转标准待复核队列（仅转标准使用）。
    pub fallback_to_queue: bool,
}

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

/// 复核错误（一律显式返回，不静默改状态）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReviewError {
    /// 记录不存在。
    #[error("判分记录不存在：{0}")]
    NotFound(GradeId),
    /// 乐观锁冲突。
    #[error("修订已过期（期望 {expected}，实际 {actual}）：请刷新后重试")]
    StaleRevision {
        /// 期望修订。
        expected: u32,
        /// 实际修订。
        actual: u32,
    },
    /// 快速抽检只读。
    #[error("快速抽检队列只读：不允许改分/改文本/排除，请先转标准复核")]
    ReadOnlySampled,
    /// 快速模式非抽检记录不允许编辑（需转标准）。
    #[error("快速模式记录不允许该编辑命令，请先转标准复核")]
    FastModeReadOnly,
    /// 命令用于错误的模式/队列。
    #[error("命令 {command:?} 不适用于当前模式 {mode:?}/抽检状态 {sampled}")]
    WrongMode {
        /// 命令。
        command: CommandKind,
        /// 模式。
        mode: GradeMode,
        /// 是否抽检。
        sampled: bool,
    },
    /// 缺原因码。
    #[error("改分/排除必须提供原因码")]
    MissingReason,
    /// 得分越界。
    #[error("得分 {awarded} 超出 0..={max}")]
    ScoreOutOfRange {
        /// 新得分（千分之一分）。
        awarded: i64,
        /// 满分（千分之一分）。
        max: i64,
    },
    /// 得分不是粒度整数倍。
    #[error("得分 {awarded} 不是粒度 {granularity} 的整数倍")]
    GranularityViolation {
        /// 得分。
        awarded: i64,
        /// 粒度。
        granularity: i64,
    },
    /// 修订号非法。
    #[error("新修订号 {0} 非法（必须为最新修订+1）")]
    InvalidRevision(u32),
    /// 重判进行中（题级锁）。
    #[error("该题正在重判，请稍后")]
    RegradeInProgress,
    /// 重判失败/后端不可用（保留原判定，可重试）。
    #[error("重判失败（原判定保留）：{0}")]
    RegradeFailed(String),
    /// 无可撤销。
    #[error("无可撤销命令")]
    NothingToUndo,
    /// 无可重做。
    #[error("无可重做命令")]
    NothingToRedo,
    /// 游标非法。
    #[error("分页游标非法：{0}")]
    InvalidCursor(String),
    /// 批量超过上限。
    #[error("批量目标 {count} 超过上限 {limit}")]
    BatchTooLarge {
        /// 数量。
        count: usize,
        /// 上限。
        limit: usize,
    },
    /// 配置非法。
    #[error("复核配置非法：{0}")]
    Config(String),
    /// 审计写入失败。
    #[error("审计写入失败：{0}")]
    Audit(String),
    /// 撤销历史已损坏。
    #[error("撤销历史损坏：{0}")]
    UndoCorrupted(String),
    /// 单记录命令必须携带期望修订（乐观锁）。
    #[error("单记录命令必须携带期望修订（乐观锁）")]
    MissingExpectedRevision,
    /// 新修订记录未通过领域不变量校验。
    #[error("新修订记录非法：{0}")]
    InvalidRecord(String),
    /// 数据源错误。
    #[error("数据源错误：{0}")]
    Repository(String),
}

impl ReviewError {
    /// 稳定错误码（日志/审计用）。
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NotFound(_) => "review.not_found",
            Self::StaleRevision { .. } => "review.stale_revision",
            Self::ReadOnlySampled => "review.read_only_sampled",
            Self::FastModeReadOnly => "review.fast_mode_read_only",
            Self::WrongMode { .. } => "review.wrong_mode",
            Self::MissingReason => "review.missing_reason",
            Self::ScoreOutOfRange { .. } => "review.score_out_of_range",
            Self::GranularityViolation { .. } => "review.granularity_violation",
            Self::InvalidRevision(_) => "review.invalid_revision",
            Self::RegradeInProgress => "review.regrade_in_progress",
            Self::RegradeFailed(_) => "review.regrade_failed",
            Self::NothingToUndo => "review.nothing_to_undo",
            Self::NothingToRedo => "review.nothing_to_redo",
            Self::InvalidCursor(_) => "review.invalid_cursor",
            Self::BatchTooLarge { .. } => "review.batch_too_large",
            Self::Config(_) => "review.config",
            Self::Audit(_) => "review.audit",
            Self::UndoCorrupted(_) => "review.undo_corrupted",
            Self::MissingExpectedRevision => "review.missing_expected_revision",
            Self::InvalidRecord(_) => "review.invalid_record",
            Self::Repository(_) => "review.repository",
        }
    }
}
