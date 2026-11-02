//! `eg-recognize`：页面到区块结构化、Schema 校验、缓存与置信度。
//!
//! 规范：`SPEC-recognize.md` v0.2.1（§2 契约与错误映射、§3 两段识别/校验修复/LaTeX/
//! 手写降级/置信度/缓存/阅读顺序、§4 失败模式、§8 验收）· `SPEC-inference.md`
//! v0.2.3（OCR 角色、严格 Schema、结构校验归属）· `SPEC-capture.md`（白底派生图）·
//! `CONFIG.md` §4（`ocr.*`）· `SPEC-domain.md` §7/§8（`Recognition`/`Block`/
//! `EngineStamp`/`ModelCall`）。
//!
//! 边界：
//! - 只产出 [`eg_domain::Recognition`] 与 [`eg_domain::ModelCall`] 证据，不写半成品。
//! - 模型输出一律视为不可信输入：严格 Schema 校验（拒绝未知字段/枚举、长度上限）→
//!   语义校验（坐标范围、条码块、重复块、数量上限）→ 只对「可机械修复」的错误发起
//!   至多 `ocr.repair_attempts` 次修复；语义违规不重试。
//! - 公式只做确定性规范化与词法/结构校验；非法即该块不可用（转人工），**不猜测补全**。
//! - 不直接 HTTP、不直接 SQL：后端、缓存、仓储、时钟、id 生成器、取消令牌全部注入。
//!
//! 并行实现期间的接口接缝（`eg-inference`/`eg-capture` 尚未定稿）见各模块 TODO；
//! 约定最小识别 IO 端口在 [`types`]，真实适配器由 `app` 组合根提供。

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]
#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::module_name_repetitions,
    clippy::doc_markdown,
    clippy::too_many_lines,
    clippy::similar_names,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::missing_const_for_fn,
    clippy::return_self_not_must_use,
    clippy::needless_pass_by_value,
    clippy::option_if_let_else,
    clippy::items_after_statements,
    clippy::struct_excessive_bools,
    clippy::float_cmp,
    clippy::redundant_clone,
    reason = "识别流水线是有状态的阶段机；质量/置信度多字段结构是规范§2.4 明文要求的\
              模块自有类型；错误文案由中文错误类型承载；测试中的 unwrap 仅 #[cfg(test)] 允许"
)]

pub mod cache;
pub mod confidence;
pub mod error;
pub mod handwriting;
pub mod latex;
pub mod page;
pub mod plausibility;
pub mod prompt;
pub mod reading_order;
pub mod schema;
pub mod segment;
pub mod semantic;
pub mod testing;
pub mod types;

pub use cache::{CacheKey, CacheValue, MemoryRecognitionCache, RecognitionCache};
pub use confidence::{
    ConfidenceBreakdown, ConfidenceSource, QuestionConfidence, block_confidence, page_confidence,
    question_confidence,
};
pub use error::{RecognizeError, SemanticViolation, StructureError};
pub use handwriting::{HandwritingPolicy, HandwritingTrigger, handwriting_fallback};
pub use latex::{LatexOutcome, normalize_latex, validate_latex};
pub use page::{
    CrossPageCandidates, RecognizeMetrics, RecognizeOutcome, cross_page_candidates,
    has_question_number_prefix, recognize_page,
};
pub use plausibility::{QuestionPlausibility, inspect_question_patterns, leading_question_number};
pub use prompt::{
    PROMPT_SCHEMA_VERSION, prompt_hash, render_page_prompt, render_repair_prompt,
    render_segment_prompt,
};
pub use schema::{PageDims, ParsedBlock, ParsedOutput, WireKind, parse_output};
pub use segment::{
    SegmentContext, SegmentRecognizeDecision, SegmentTrigger, recognize_segment, segment_decision,
};
pub use semantic::{
    BlockRecord, BlockStatus, SemanticOptions, StructureQuality, compute_structure,
    validate_semantics,
};
pub use types::{
    IdSource, ImageBackground, ImageInput, ImageSource, OcrBackend, OcrRequest, OcrResponse,
    RecognitionRepo, RecognizeIo, RepairFeedback, ScanScope,
};
