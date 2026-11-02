//! `eg-domain`：领域类型、不变量、分数与证据模型、学科契约、规则草稿（纯类型，零 IO）。
//!
//! 规范：`SPEC-domain.md` v0.2.2（类型权威源）· `SPEC.md` §4/§7/§10 ·
//! `CAPABILITY-MAP.md` §3/§5 · 任务：`tasks/todo.md` T04–T06。
//!
//! 边界（规范 §13）：
//! - 零 IO/DB/HTTP；不 panic、不 clamp；禁止 `unsafe`。
//! - 分数只经 [`MilliScore`]；跨模块 id 只用本 crate 的 ULID 新类型。
//! - 跨模块只暴露本 crate 的公开类型；不定义与规范冲突的同义类型。
//!
//! 本 crate 只依赖 `serde`/`serde_json`/`thiserror`/`ulid`/`chrono`/`blake3`/`sha2`
//! （均为纯计算库，无文件、网络或数据库访问），依赖闭包与版本见
//! `docs/dev/research/domain.md`。

#![deny(unsafe_code)]
#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::doc_markdown,
    clippy::must_use_candidate,
    clippy::return_self_not_must_use,
    clippy::missing_const_for_fn,
    clippy::module_name_repetitions,
    clippy::needless_pass_by_value,
    reason = "错误文档由错误枚举的中文文案承载；域类型构造器接受所有权便于实现；\
              文档中的中文与专有名词不强制反引号；其余为纯风格类 pedantic 提示"
)]

pub mod answer;
pub mod confidence;
pub mod contract;
pub mod decimal;
pub mod error;
pub mod evidence;
pub mod geometry;
pub mod grade;
pub mod hash;
pub mod ids;
pub mod page;
pub mod paper;
pub mod recognition;
pub mod rubric;
pub mod schema;
pub mod score;
pub mod subject;
pub mod time;
pub mod values;

pub use answer::{AnswerKey, AnswerValue, ConcreteKey, Tolerance};
pub use confidence::Confidence;
pub use contract::{
    ChainOutcome, ContractCase, ContractSuite, ContractViolation, check_subject_pack, contract_ok,
    run_comparator_chain,
};
pub use decimal::Decimal;
pub use error::{ConfidenceError, DecimalError, DomainError, HashError, IdError, ScoreError};
pub use evidence::{ComparatorTrace, EvidenceRef, ModelCall};
pub use geometry::BBox;
pub use grade::{
    ExcludeReason, GradeMode, GradeRecord, GradeStatus, JudgeOutcome, JudgedUnit, Judgement,
    ManualReason, ReviewReasonKind, RouteDecision, RouteTrace, SubmissionExclusion,
    UngradableReason,
};
pub use hash::{AssetId, ContentHash, Sha256Hex};
pub use ids::{
    ClassroomId, ComparatorTraceId, ConsentId, GradeId, ModelCallId, PageId, PaperId, QuestionId,
    RecognitionId, ReviewActionId, RubricId, RubricItemId, SegmentId, StudentId, SubmissionId,
    TaskId, Ulid,
};
pub use page::{
    Page, PageQuality, QualityVerdict, Segment, SegmentMethod, Stage, Submission, SubmissionStatus,
};
pub use paper::{
    Classroom, ContentRef, Paper, PaperTemplate, Question, QuestionKind, RosterSource, Student,
    TemplateRegion,
};
pub use recognition::{Block, BlockKind, EngineStamp, ModelRole, Recognition};
pub use rubric::{
    DraftOrigin, DraftStatus, LOCAL_TEACHER, PartialChoiceRule, PartialChoiceSource,
    PartialChoiceTier, Rubric, RubricDraft, RubricDraftItem, RubricItem, RubricOrigin, RuleText,
    RuleTextSource, ScoringMode,
};
pub use schema::{DOMAIN_SCHEMA_VERSION, SchemaVersioned};
pub use score::MilliScore;
pub use subject::{
    AnswerPart, AnswerSource, Comparator, ComparatorVerdict, JudgeContext, NormalizeError,
    NormalizedAnswer, NormalizedStep, RawAnswer, RubricIssue, RubricIssueSeverity, RubricItemRef,
    StepEvidence, StepMatch, StudentAnswer, SubjectCapabilities, SubjectPack,
    ensure_no_error_issues, error_issue_count,
};
pub use time::{Clock, Timestamp};
pub use values::{
    BackendId, CalibrationId, ComparatorId, ExifOrientation, Rotation, SubjectId, TemplateId,
};
