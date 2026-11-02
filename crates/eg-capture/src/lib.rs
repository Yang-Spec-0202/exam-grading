//! `eg-capture`：导入、EXIF 方向、去畸变、质量门控、PDF 拆页、资产入库。
//!
//! 规范：`SPEC-capture.md` v0.2.1（§2 契约、§3 数据流与门控、§4 失败模式、§5 可观测性、
//! §6 测试策略）· `CONFIG.md` §2（`import.*`）· `SPEC-domain.md`（`Page`/`PageQuality`/
//! `ContentHash`/`AssetId`）· 任务：`tasks/todo.md` T21–T22。
//!
//! 边界：
//! - 原图只读、只追加、永久保留：本 crate 不提供删除/覆盖路径。
//! - 派生图只新增，可由「原图 + 流水线版本 + 选项哈希」重建。
//! - 资产字节经 [`AssetSink`] 端口交给 `platform` 资产库（内容寻址、幂等）；
//!   业务行落库由 [`persist::persist`] 在**单个 `platform::db.write` 事务**内完成
//!   （D1：capture 直接依赖 `eg-platform` 的 db/repo/audit，不新增抽象层）。
//! - 被拒页面**不进** `recognize`；质量门禁优先于自动化。
//! - 事件/报告不得出现文件路径原文、学生姓名或学号：来源只以 [`SourceFingerprint`]
//!   （路径哈希前 12 位 + 扩展名）表示。
//!
//! 外部能力与实测依据（`docs/dev/research/`）：
//! - PDF 栅格化用纯 Rust 的 `hayro`（0.8.0，见 `pdf.md`）；**hayro 渲染的位图默认透明背景**，
//!   因此解码/渲染后统一走 [`imageops::composite_on_white`]，否则送 OCR 会严重降质
//!   （见 `inference-serving.md`）。
//! - `image` 0.25.10 负责 JPG/PNG/TIFF 解码与派生图；`kamadak-exif` 0.6.1 读 EXIF 方向（`deps.md`）。

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::return_self_not_must_use,
    clippy::module_name_repetitions,
    clippy::needless_pass_by_value,
    clippy::doc_markdown,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::struct_excessive_bools,
    clippy::float_cmp,
    clippy::option_if_let_else,
    clippy::unreadable_literal,
    clippy::items_after_statements,
    clippy::manual_let_else,
    clippy::match_same_arms,
    clippy::type_complexity,
    clippy::unused_self,
    clippy::needless_range_loop,
    clippy::manual_midpoint,
    clippy::map_unwrap_or,
    clippy::format_collect,
    clippy::too_many_arguments,
    clippy::trivially_copy_pass_by_ref,
    clippy::field_reassign_with_default,
    clippy::semicolon_if_nothing_returned,
    clippy::unnecessary_cast,
    clippy::manual_range_patterns,
    clippy::useless_vec,
    clippy::bool_assert_comparison,
    clippy::nonminimal_bool,
    missing_debug_implementations,
    reason = "图像数值运算需要 f64/usize 互转；中文错误文案由 Error 承载；\
              端口对象（dyn trait 引用）无法 derive Debug；其余为纯风格类 pedantic 提示"
)]

pub mod dedup;
pub mod detect;
pub mod error;
pub mod event;
pub mod exif;
pub mod imageops;
pub mod import;
pub mod media;
pub mod pdf;
pub mod persist;
pub mod pipeline;
/// 端口与 `platform` 的接入点（当前只有 TODO 标记与适配说明）。
pub mod platform_bridge;
pub mod quality;
pub mod report;
pub mod settings;
pub mod sink;
pub mod transform;

pub use dedup::{
    BatchDedupe, KnownSubmissions, NoKnownSubmissions, SUSPECTED_OVERLAP, SubmissionFingerprint,
    suspicious_submission,
};
pub use error::{CaptureError, CaptureErrorClass, PageNote, RejectReason};
pub use event::{
    ChannelEventSink, ImportEvent, ImportEventSink, MemoryEventSink, SourceFingerprint,
};
pub use import::{
    DuplicatePolicy, FileInput, ImportOptions, ImportOutcome, ImportRequest, Importer, InputSource,
};
pub use media::MediaKind;
pub use persist::{
    OWNER_KEY_PAGE_ASSET, OWNER_PAGE, PersistRequest, PersistSummary, ROLE_DERIVED_PREFIX,
    ROLE_ORIGINAL, persist,
};
pub use pipeline::{
    CancelCheck, ClockPort, IdPort, NeverCancel, Pipeline, PreparedFile, PreparedKeyAsset,
    PreparedPage, SeqIdSource,
};
pub use quality::{QualityAssessment, QualityMetrics, assess, classify, measure};
pub use report::{
    DuplicateCategory, DuplicateHit, FileFailure, ImportKind, ImportReport, KeyAssetSummary,
    RejectedPage, SubmissionSummary,
};
pub use settings::{ImportSettings, RejectBehavior};
pub use sink::{AssetSink, LocalDirAssetSink, MemoryAssetSink, PutResult};

/// 流水线版本：派生图重建与缓存失效的键之一（`SPEC-capture.md` §3.3、§9-Q6）。
///
/// 修改任何会改变派生图像素或质量判定的算法时**必须**提升此版本，并同步
/// `docs/dev/research/capture.md`。
pub const PIPELINE_VERSION: &str = "capture-1";
