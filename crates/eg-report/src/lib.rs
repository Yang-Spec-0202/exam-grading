//! `eg-report`：双模式成绩汇总、题目统计、错题标签与导出（XLSX/CSV/HTML）。
//!
//! 规范：`SPEC-report.md` v0.2.1（§3 对外契约、§3.4 统计口径、§3.5 报表定义、§4 数据流、
//! §5 失败、§6 可观测性、§7 测试、§9 成功标准）· `SPEC-domain.md`（`GradeRecord.mode`/
//! `sampled`/`revision`、`SubmissionExclusion`、`GradeMode`）· `CONFIG.md`（`fast.*`、
//! `privacy.export_mask`）· `CAPABILITY-MAP.md` §2/§3（单向依赖，app 为组合根）·
//! 任务：`tasks/todo.md` T33–T34。
//!
//! 边界：
//! - **只读**：不写 `GradeRecord`/`Submission`，不改状态；单机离线，不发起网络请求。
//! - **端口取数**：数据来源由 `app` 经 [`ReportSource`] 注入（本 crate 不直连数据库）。
//!   研究记录：`docs/dev/research/report.md`。
//! - **纯计算**：同一快照重复构建/导出的内容摘要逐字节一致（[`verify_replay`]）。
//! - **双模式隔离**：标准与快速分列统计，永不混合均分/得分率/排名；「全部」仅对照计数。
//! - **整数口径**：分数以千分之一分为单位、率以 ppm 为单位，浮点只用于展示与 XLSX 单元格。
//! - **原子导出**：临时文件 + `rename`；多格式全成功或全无；审计失败回滚已写文件。
//!
//! 偏差（待 `SPEC-report.md` 审批修订，详见研究记录 §与规范的偏差）：
//! 1. 暂不依赖 `eg-grade`（并行开发中未通过编译），判分记录按其领域类型 `GradeRecord` 取数；
//! 2. 时间戳按调用方给出的 `Timestamp` 以 UTC 格式化为 `yyyyMMdd-HHmmss`（不读本地时区）；
//! 3. `CONFIG.md` 尚无 `report.*` 键，口径默认值在 [`ReportSettings::default`] 内定义并由 app 映射；
//! 4. 单学生成绩单作为「成绩单」工作表的行提供命名规则与函数，不单独产出逐生文件（v1 范围）；
//! 5. 选项分布依赖 `align` 的作答投影；投影缺失时该表为空（不会伪造数据）。

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
    clippy::needless_pass_by_value,
    clippy::struct_excessive_bools,
    clippy::items_after_statements,
    clippy::unreadable_literal,
    clippy::option_if_let_else,
    clippy::return_self_not_must_use,
    clippy::missing_const_for_fn,
    clippy::redundant_clone,
    clippy::significant_drop_tightening,
    clippy::match_same_arms,
    clippy::manual_let_else,
    clippy::single_match_else,
    clippy::needless_for_each,
    clippy::map_unwrap_or,
    clippy::unnecessary_wraps,
    reason = "报表层大量整数口径/行集构造代码；错误文档由中文错误类型承载；\
              接口为教师端内部 API，返回值取舍以可读性优先；测试中的 unwrap 仅在 cfg(test) 允许"
)]

pub mod atomic;
pub mod audit;
pub mod csv;
pub mod error;
pub mod export;
pub mod html;
pub mod masking;
pub mod naming;
pub mod rounding;
pub mod settings;
pub mod source;
pub mod stats;
pub mod table;
pub mod xlsx;

pub use audit::{
    AUDIT_ACTION, AUDIT_ACTOR, AUDIT_ENTITY_TYPE, ExportAuditEvent, ExportAuditor, MemoryAuditor,
};
pub use error::{ReportError, Result};
pub use export::{ExportFormat, ExportOutcome, ExportRequest, ExportedFile, export, verify_replay};
pub use masking::{MASK_NOTE, MASK_SUFFIX, assert_no_leak, masked_name, masked_student_no};
pub use naming::{ExportNaming, MAX_NAME_CHARS, ModeToken, TIMESTAMP_FORMAT, format_timestamp};
pub use rounding::{display_percent, display_score};
pub use settings::{NAMING_RULE_VERSION, ReportSettings, SAMPLING_METHOD};
pub use source::{
    ChoiceAnswer, ChoiceRecord, PaperSnapshot, QuestionSnapshot, ReportInput, ReportSource,
    StaticReportSource, SubmissionSummary, exclusion_reason_label, grade_status_label, mode_label,
};
pub use stats::{
    ClassStats, DISCRIMINATION_MIN_SAMPLES, DISTRIBUTION_BINS, DistributionBin, FastSamplingRow,
    ModeQuestionStats, OptionStats, QuestionStats, Report, StudentOutcome, StudentResult, TagStats,
    meta_rows, mode_text, outcome_label,
};
