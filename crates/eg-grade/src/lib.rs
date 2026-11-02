//! `eg-grade`：两层判分、模式、置信度门禁、冲突仲裁、证据（`SPEC-grade.md`，T16–T18）。
//!
//! 架构：
//! - L1 路由（[`route`]）：R0–R11 有序表纯函数；R9 分类澄清由注入后端执行一次；
//! - 就绪检查（[`readiness`]）：答案键/已确认规则/仅人工标记/版本；
//! - L2 规则段（[`l2`]）：学科包比较器链（首个确定结论终止，落比较轨迹）；
//! - L2 模型段（[`validate`]）：模型输出硬校验（得分/粒度/一致性/逐项覆盖/聚合）；
//! - 聚合（[`aggregate`]）：累加/全对/分步/多选部分给分（缺档非法→人工，绝不默认 0）；
//! - 门禁与置信度（[`gate`]）：标准/快速阈值、未校准模型、证据链、冲突策略；
//! - 抽检（[`sampling`]）：快速模式比例 0.20、低置信权重 0.70、种子可复现；
//! - 幂等（[`idempotent`]）：重复判分命中既有记录，不重复计费；
//! - 流水线（[`pipeline`]）：单题判分 + 提交级汇总。
//!
//! 并行依赖适配（TODO）：`eg-rubric` 稳定后实现 [`ports::RubricSource`]；
//! `eg-inference` 稳定后实现 [`ports::JudgeBackend`]（judge 角色/fixture/校准）与
//! [`ports::ClassifierBackend`]（router 角色）；生产 id/时钟/幂等持久化分别接
//! `eg-platform` 的 `UlidFactory`/`SystemClock`/`repo`。本 crate 不修改其他 crate。
//!
//! 失败语义（规范 §9）：任何失败路径不给 0 分成功、不静默丢题、不丢失模式标记；
//! 无法判定一律进人工。

#![deny(unsafe_code)]
#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::module_name_repetitions,
    clippy::too_many_lines,
    reason = "错误文档由各错误枚举的中文文案承载；模块名重复为领域术语；\
              判分流水线天然较长，已按职责拆分模块"
)]

pub mod adapters;
pub mod aggregate;
pub mod align;
pub mod config;
pub mod gate;
pub mod idempotent;
pub mod l2;
pub mod pipeline;
pub mod ports;
pub mod readiness;
pub mod route;
pub mod sampling;
pub mod submission;
pub mod validate;

pub use adapters::align::{AlignReportProvider, AlignReportSource, project_report};
pub use adapters::inference::{InferenceClassifier, InferenceJudge};
pub use adapters::platform::{
    PlatformClock, PlatformGradeStore, PlatformIdGenerator, PlatformIdempotencyStore,
};
pub use adapters::rubric::RubricStoreSource;
pub use align::{AlignError, AlignIssue, AlignSource, AlignedSubmission, FixtureAlignSource};
pub use config::{ConfigError, ConflictPolicy, GradeConfig, ModeThresholds};
pub use gate::{GateInput, GateOutcome, compose_confidence, gate};
pub use idempotent::idempotency_key;
pub use l2::{RuleEvaluation, RuleRun, evaluate_rule_chain};
pub use pipeline::{
    GradeError, GradedQuestion, Grader, QuestionAnswer, QuestionFailure, SubmissionGrade,
    SubmissionRequest, SubmissionSummary,
};
pub use ports::{
    ClassificationResponse, ClassifierBackend, FixedClock, FixtureClassifier, FixtureRubrics,
    IdGenerator, IdempotencyKey, IdempotencyStore, InMemoryIdempotency, JudgeBackend, JudgeFailure,
    JudgeRequest, JudgeResponse, JudgeVerdict, RubricAvailability, RubricSource, ScriptedJudge,
    ScriptedStep, SequentialIds,
};
pub use readiness::{ReadinessIssue, ReadinessIssueKind, ReadinessReport, check_readiness};
pub use route::{RouteInput, RouteOutcome, key_kind_conflict, route};
pub use sampling::{SamplingCandidate, SamplingResult, sample_count, sample_fast};
pub use submission::{
    GradeStore, InMemoryGradeStore, InMemoryProgressStore, ProgressSnapshot, ProgressStore,
    StoreError, SubmissionPipeline, SubmissionPipelineError, SubmissionRun, SubmissionRunRequest,
};
pub use validate::{
    ConflictKind, ValidatedJudge, ValidationError, validate_judge_response,
    validate_judgement_against,
};
