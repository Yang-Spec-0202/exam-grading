//! `eg-platform`：配置、密钥、SQLite、资产库、任务队列、日志、审计、单实例、备份恢复、时钟与 id。
//!
//! 规范：`SPEC-platform.md` v0.2.1（§2 目录/配置/密钥、§4 数据流与状态机、§5 失败模式、
//! §6 日志与审计、§7 测试策略）· `CONFIG.md`（配置键与默认值唯一依据）·
//! `CAPABILITY-MAP.md` §2/§3/§5 · 任务：`tasks/todo.md` T07–T12。
//!
//! 边界：
//! - 上层模块只能经本 crate 读写磁盘/数据库/Keychain；禁止绕过。
//! - 原图与资产只读、永久保留，本 crate **不提供任何删除资产的 API**。
//! - 审计只追加、哈希链；写业务数据必须同事务写审计，审计失败即拒绝业务写。
//! - 业务代码不直接读系统时间（用 [`eg_domain::Clock`] 注入）。

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
    clippy::missing_const_for_fn,
    clippy::return_self_not_must_use,
    clippy::needless_pass_by_value,
    clippy::struct_excessive_bools,
    clippy::option_if_let_else,
    clippy::unreadable_literal,
    clippy::items_after_statements,
    clippy::significant_drop_tightening,
    clippy::redundant_clone,
    reason = "基础设施层的锁/事务/IO 结构使这些 pedantic 提示误报；\
              错误文案由中文错误类型承载；测试中的 unwrap/expect 仅在 #[cfg(test)] 允许"
)]

pub mod assets;
pub mod audit;
pub mod backup;
pub mod cancel;
pub mod clock;
pub mod config;
pub mod consent;
pub mod db;
pub mod diagnostics;
pub mod error;
pub mod ids;
pub mod index;
pub mod instance;
pub mod logging;
pub mod paths;
pub mod platform;
pub mod repo;
pub mod schema;
pub mod secrets;
pub mod tasks;

pub use assets::{AssetSettings, AssetStore, AssetUsage, PutOutcome};
pub use audit::{AuditEntry, AuditLog, AuditQuery, ChainHash, ChainVerification, NewAuditEntry};
pub use backup::{
    BackupKind, BackupManifest, BackupRecord, BackupService, RestoreReport, RestoreVerification,
};
pub use cancel::CancelToken;
pub use clock::{FixedClock, SharedClock, SystemClock};
pub use config::{AppConfig, ConfigDocument, ConfigIssue, ConfigStore, MetaConfig, SamplingSeed};
pub use consent::{ConsentState, ConsentStore, TaskConsentRecord};
pub use db::{Database, DatabaseOptions};
pub use diagnostics::{DiagnosticBundle, DiagnosticRequest, Diagnostics};
pub use error::{ErrorClass, PlatformError, Result};
pub use ids::UlidFactory;
pub use index::{IndexState, LogIndex, LogIndexHit};
pub use instance::{InstanceInfo, InstanceLock};
pub use logging::{
    CleanupReport, LogIndexState, LogLevel, LogPage, LogQuery, LogRecord, LogSearchOutcome,
    LogSettings, LogSink, Redactor, TraceId,
};
pub use paths::DataDirs;
pub use platform::Platform;
pub use secrets::{InMemorySecretStore, KeychainSecretStore, Secret, SecretRef, SecretStore};
pub use tasks::{
    EnqueueOutcome, FailOutcome, NewTask, TaskAttempt, TaskFilter, TaskKind, TaskQueue, TaskRecord,
    TaskState, TaskVersions,
};
