//! `eg-rubric`：评分规则草稿、全国卷模板库、规则整理、静态校验与版本化
//! （`SPEC-rubric.md` v0.2.2；T15 无模型部分 + T25 规则整理与模板数据对齐）。
//!
//! 边界（`CAPABILITY-MAP.md` §3）：本模块只回答「规则怎么来、长什么样、是否合法」，
//! 不判分（`grade`）、不直接调用模型（规则整理经 [`StructurerPort`] 端口，
//! 由 `app` 用 `eg-inference` 适配；本 crate 不持有模型/授权/预算）、
//! 不直接持有 SQL/HTTP；全部持久化与审计经 [`RubricStore`]/[`RubricAudit`] 端口，
//! 平台实现接入前用 [`memory`] 提供的内存适配器（TODO(T12/T18)：接 `eg-platform`）。
//!
//! 核心不变量（`SPEC-domain.md` §4/§5）：
//! 1. 模型整理与模板套用只允许产出 [`eg_domain::RubricDraft`]；生效规则 [`eg_domain::Rubric`]
//!    只能由教师确认（[`RubricEngine::confirm`]）产生，确认恒开、不可配置；
//! 2. 草稿必须通过全部静态校验（11 条基础 + 部分给分专项）才允许确认；
//! 3. 版本只递增、不回改历史；每次状态迁移写审计。
//!
//! 已知缺口（详见 `docs/dev/research/rubric.md`）：
//! - `eg_domain::RubricDraft` 无 `mode`/`partial_choice`/模板引用字段，本模块用自有
//!   [`StoredDraft`] 伴随记录承载，确认时再构造 `RubricOrigin`；
//! - 规则整理输出 Schema 与 `eg-inference` 当前 `StructurerOutput` 字段名不同，
//!   `app` 适配器需做单点映射；
//! - 多选部分给分档位已随 T25 写入现行卷模板数据，2 个正确选项的题目需教师按
//!   答案键改用备选骨架。

#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::module_name_repetitions,
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::missing_const_for_fn,
    clippy::return_self_not_must_use,
    clippy::needless_pass_by_value,
    clippy::option_if_let_else,
    clippy::similar_names,
    clippy::doc_markdown,
    clippy::items_after_statements,
    clippy::assigning_clones,
    clippy::redundant_clone,
    reason = "规则域的中文错误文案、模板解析与状态机分支较多；\
              这些 pedantic 提示在本模块属误报，逐条 allow 会淹没真实警告"
)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::assert_is_empty
    )
)]

pub mod confirm;
pub mod draft;
pub mod error;
pub mod memory;
pub mod ports;
pub mod structurer;
pub mod templates;
pub mod validate;

pub use confirm::{ConfirmOutcome, RejectOutcome};
pub use draft::{DraftSource, RubricEngine};
pub use error::{Result, RubricError};
pub use memory::{FixedClock, InMemoryRubricStore, RecordingAudit, SequentialIds};
pub use ports::{
    DEFAULT_GRANULARITY_MILLI, MAX_ITEMS, NewRubricAuditEvent, RubricAction, RubricAudit,
    RubricAuditEvent, RubricIdSource, RubricSettings, RubricStore, StoredDraft, TemplateRef,
    TemplateStatus, TemplateVersion,
};
pub use structurer::{
    MAX_CRITERION_CHARS, MAX_ITEM_POINTS_MILLI, MAX_TOTAL_POINTS_MILLI, STRUCTURER_SCHEMA_VERSION,
    StructurerAttempt, StructurerInvocation, StructurerPort, StructurerPrompt, StructurerReply,
    StructurerRequest, infer_mode, parse_structurer_output, render_structurer_prompt,
};
pub use templates::{
    LoadedTemplate, TemplatePack, TemplateSelection, TemplateWarning, TemplateWarningCode,
};
pub use validate::{CheckId, Finding, ValidationContext, ValidationReport};

/// 确认人固定为本地教师（`SPEC-domain.md` §4；`SPEC-rubric.md` §2.5）。
pub use eg_domain::LOCAL_TEACHER;
