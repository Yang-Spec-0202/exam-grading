//! `eg-inference`：推理后端抽象、模型注册表、健康、预算、许可门禁、按任务鉴权。
//!
//! 规范：`SPEC-inference.md` v0.2.3（§2 角色/后端/任务授权、§3 契约与注册表字段、
//! §4 生命周期与熔断预算、§5 失败模式、§9 成功标准）· `CONFIG.md` §4/§5/§10 ·
//! `SPEC-domain.md` §8（`ModelCall`/`EngineStamp`/`JudgeOutcome`/`TaskConsent`）·
//! `AGENTS.md`（检索优先、来源可追溯）。
//!
//! 边界：
//! - 所有模型调用只经本模块；默认离线；云端必须绑定有效任务授权（无授权 0 次外联）。
//! - 每次调用写 `ModelCall`（经注入的 store），云端调用必须带 `consent_id`。
//! - 模型输出一律视为不可信输入，先校验后使用；不修复、不补全、不静默截断。
//! - 本 crate 不做业务判定语义（`grade`/`recognize` 负责提示词渲染与业务映射）。
//!
//! 依赖说明：
//! - `eg-platform` 的配置/仓储/审计接口正在并行实现。本 crate 只经由
//!   `platform_adapter` 模块接触它；其余逻辑依赖注入点
//!   [`eg_domain::Clock`]、`call::ModelCallStore`、`call::AuditSink`。
//!   未稳定的平台接口在 `platform_adapter` 内以 TODO 标注，替换时只改该文件。
//! - 传输层是可注入的抽象（`http::Transport`）：测试注入桩，本地 sidecar 用内置
//!   HTTP/1.1 回环实现；HTTPS（云端）需显式接入 TLS 传输（v1 默认关闭，见研究记录）。

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
    clippy::option_if_let_else,
    clippy::unreadable_literal,
    clippy::items_after_statements,
    clippy::significant_drop_tightening,
    clippy::redundant_clone,
    clippy::struct_excessive_bools,
    reason = "基础设施层的生命周期/状态机结构使这些 pedantic 提示误报；\
              错误文案由中文错误类型承载；测试中的 unwrap/expect 仅在 #[cfg(test)] 允许"
)]

pub mod breaker;
pub mod budget;
pub mod calibration;
pub mod call;
pub mod consent;
pub mod error;
pub mod fixture;
pub mod fixture_disk;
pub mod http;
pub mod license;
pub mod local;
pub mod openai;
pub mod platform_adapter;
pub mod registry;
pub mod schema;
pub mod service;
pub mod testing;
pub mod types;

pub use breaker::{BreakerConfig, CircuitBreaker};
pub use budget::{BudgetLedger, PricingTier, cost_micros};
pub use call::{AuditEvent, AuditSink, CallContext, CallRecorder, IdSource, ModelCallStore};
pub use consent::{
    ConsentCheck, ConsentGate, ConsentState, MemoryConsentGate, TaskConsent, require_cloud_enabled,
};
pub use error::{FailureClass, InferenceError};
pub use fixture::{
    FIXTURE_SCHEMA_VERSION, FixtureBackend, FixtureEntry, FixtureReplay, FixtureStore, fixture_key,
};
pub use fixture_disk::{
    FIXTURE_INDEX_SCHEMA_VERSION, FixtureBackendIds, FixtureCatalog, FixtureLookup, FixtureRole,
    LoadedFixture, LoadedNegative, PageIdentity, QuestionIdentity,
};
pub use http::{HttpMethod, HttpRequest, HttpResponse, TcpTransport, Transport};
pub use license::{LicensePolicy, LicenseVerdict, LicenseViolation};
pub use local::{
    ChildProcess, LifecycleConfig, LocalServer, LocalServerSpec, LoopbackPortAllocator,
    PortAllocator, ProcessRunner, ServeCommand, ServerState, StartupMetrics, SystemRunner,
    default_models_dir, unit_dir,
};
pub use openai::{
    ChatContent, ChatMessage, ChatRequestSpec, ChatResponse, extract_json_object,
    parse_chat_response,
};
pub use platform_adapter::{
    PlatformAdapters, PlatformAuditSink, PlatformConsentGate, PlatformIdSource,
    PlatformModelCallStore, register_cloud_consent,
};
pub use registry::{
    CloudEndpoint, CloudPricing, DEFAULT_REGISTRY_TOML, Implementation, LocalEndpoint, ModelEntry,
    ModelRegistry, REGISTRY_SCHEMA_VERSION, ServeUnit, WeightsFile,
};
pub use schema::{
    JudgeOutput, JudgeTag, MAX_BLOCK_TEXT_CHARS, MAX_OCR_BLOCKS, MAX_STRUCTURER_ITEMS, OcrBlock,
    OcrBlockKind, OcrOutput, RouterOutput, StructurerItem, StructurerOutput,
};
pub use service::{
    Backend, CloudBackend, CompletionOutcome, CompletionRequest, ExpectedSchema, InferenceService,
    ServiceConfig, ValidatedOutput, implementation_label, role_label,
};
pub use types::{HealthReport, HealthState, TokenUsage};
