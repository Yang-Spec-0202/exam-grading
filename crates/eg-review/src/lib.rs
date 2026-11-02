//! `eg-review`：双模式复核工作台（队列/详情/命令/撤销审计/抽检只读/转标准）。
//!
//! 依据 `SPEC-review.md` v0.2.1 实现 T31–T32：
//! - 标准队列：待复核按风险排序、游标分页；可接受/改分/修文本重判/题级与提交级排除/批量；
//! - 快速抽检队列：`fast.sampled_readonly=开`（默认）时 **只读**，仅允许抽检通过、
//!   抽检不通过（弹窗确认后默认单题重跑）与一键转标准；
//! - 每条命令落审计（actor 固定 `local-teacher`、命令幂等键、前后快照、原因码）；
//! - 仅追加修订、`revision` 乐观锁；撤销/重做 ≥50 步且可跨进程（`UndoStore` 适配）；
//! - 重跑经 `RegradeEngine`（生产接 `eg-grade` 幂等键）。
//!
//! 数据源分三组 trait（`ports`）：`ReviewRepository`（记录/历史/证据/元数据）、
//! `AuditSink`（只追加审计）、`UndoStore`（命令栈持久化）、`RegradeEngine`（重判/整卷任务）。
//! 平台 repo 缺口（跨提交队列查询、最新修订视图、审计命令字段、撤销栈持久化）见
//! `docs/dev/research/review.md` §平台缺口；本 crate 不改其他 crate。

#![deny(unsafe_code)]
#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::module_name_repetitions,
    reason = "错误文案由错误枚举承载；模块名重复为领域术语"
)]

pub mod commands;
pub mod detail;
pub mod model;
pub mod ports;
pub mod queue;
pub mod sampling;
pub mod transfer;
pub mod undo;

pub use commands::ReviewService;
pub use detail::build_detail;
pub use model::{
    AnswerView, AuditOutcome, AuditRecord, BatchFailure, BatchOutcome, BatchScoreTarget,
    BatchTarget, CommandEnvelope, CommandKind, CommandOutcome, CommandResult, CorrectedText,
    FailAction, QueueCounts, QueueFilter, QueueItem, QueueKind, QueuePage, RegradeRequest,
    RegradeResult, RegradeScope, ReviewCommand, ReviewConfig, ReviewDetail, ReviewError,
    ReviewMetadata, ReviewReasonCode, SamplingConclusion, SamplingPrompt,
};
pub use ports::{
    AuditSink, FixtureRegradeEngine, InMemoryAuditSink, InMemoryReviewRepository,
    InMemoryUndoStore, RegradeEngine, ReviewRepository, UndoStore,
};
pub use queue::build_queue;
pub use undo::UndoEntry;
