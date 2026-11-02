//! `eg-align`：文件名优先的身份匹配、页面到题目区域对齐、未决项。
//!
//! 规范：`SPEC-align.md` v0.2.1（§3.2 身份顺序、§3.3 三级区域对齐、§3.4 未决种类、
//! §3.5 门禁、§3.6 指纹与审计）· `CONFIG.md` §3（`identity.*`/`align.*`）·
//! `SPEC-domain.md`（`Submission.source_stem`、`StudentAnswer.parts`、`PaperTemplate`、
//! `Segment`）· 任务：`tasks/todo.md` T28–T29。
//!
//! 边界与铁律（`SPEC-align.md` §8）：
//! - **身份只认文件名**：`identity.source` 在 v1 只允许「文件名」；OCR 兜底默认关闭，
//!   关闭时本 crate 不读任何识别文本作身份依据，也**不产生 OCR 调用**（端口不会被调用）。
//! - 文件名结论不被 OCR 覆盖；文件名与已启用兜底冲突时**显式未决**，绝不静默择优。
//! - 无法确定就进未决：低于阈值、区域不确定、缺页/多页/跨页冲突一律输出未决项；
//!   门禁（§3.5）不满足时 `auto_grade_ready=false`，`grade` 只能走人工路由。
//! - 不纠正、不改写用户文件名；不自行排除提交；不把「未对齐」当成「未作答」。
//! - 任何丢页/丢题/丢未决项都必须可见（报告 + 未决列表）。
//!
//! 端口解耦：`eg-capture`/`eg-recognize` 仍在并行实现，故以
//! [`PageSource`](ports::PageSource)/[`RecognitionSource`](ports::RecognitionSource)
//! 端口接入；夹具与适配器见 [`testing`]，真实适配器 TODO 记录在 `src/ports.rs`。

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::module_name_repetitions,
    clippy::doc_markdown,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::struct_excessive_bools,
    clippy::option_if_let_else,
    clippy::manual_let_else,
    clippy::items_after_statements,
    clippy::return_self_not_must_use,
    clippy::needless_pass_by_value,
    clippy::float_cmp,
    clippy::map_unwrap_or,
    clippy::uninlined_format_args,
    clippy::if_not_else,
    clippy::match_same_arms,
    clippy::redundant_else,
    clippy::type_complexity,
    missing_debug_implementations,
    reason = "身份/区域打分需要 f64 与千分位整数互转；端口对象（dyn trait 引用）无法 derive \
              Debug；中文错误文案由 Error 承载；其余为纯风格类 pedantic 提示"
)]

pub mod crosspage;
pub mod dedupe;
pub mod error;
pub mod identity;
pub mod manual;
pub mod normalize;
pub mod pipeline;
pub mod platform_bridge;
pub mod ports;
pub mod region;
pub mod report;
pub mod roster;
pub mod settings;
pub mod testing;

/// 对齐算法版本（落 `AlignFingerprint.algorithm_version`；升级必须显式记录）。
pub const ALGORITHM_VERSION: &str = "align-1";

/// 统一四舍五入到千分位（规范固定值；置信度落库前必须经过本函数）。
#[must_use]
pub fn confidence_milli(value: f64) -> eg_domain::Confidence {
    let rounded = (value * 1000.0).round() / 1000.0;
    eg_domain::Confidence::new(rounded).unwrap_or(eg_domain::Confidence::ZERO)
}

pub use crosspage::{AnswerBuildOutcome, build_answers};
pub use dedupe::{DuplicateVerdict, ExistingSubmission, check_duplicate};
pub use error::AlignError;
pub use identity::{
    IdentityCandidate, IdentityConclusion, IdentityEvidence, IdentityHint, IdentityHintKind,
    IdentityHintSource, IdentityStatus, MatchBasis, NoIdentityHints, resolve_identity,
};
pub use manual::{
    ManualCommand, ManualOutcome, ManualRequest, apply_manual, manual_audit_entry,
    manual_segment_ids,
};
pub use normalize::{
    leading_anchor_number, main_number, normalize_key, normalize_text, page_suffix, tokenize,
};
pub use pipeline::{AlignRequest, align};
pub use platform_bridge::{
    PlatformIds, load_roster, settings_from_platform, verify_audit_chain, write_alignment_audit,
    write_manual_audit,
};
pub use ports::{
    IdPort, MapRecognitionSource, PageRef, PageSource, RecognitionSource, SeqIds, SubmissionPages,
};
pub use region::{AlignedQuestion, RegionOutcome, RegionPath, align_regions, region_confidence};
pub use report::{
    AlignFingerprint, AlignMetrics, AlignReport, ManualRecord, QuestionRegionSummary,
    UnresolvedItem, UnresolvedKind,
};
pub use roster::Roster;
pub use settings::AlignSettings;
