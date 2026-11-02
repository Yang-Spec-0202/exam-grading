//! 推理服务编排（`SPEC-inference.md` §4 单次调用时序）。
//!
//! 一次调用的固定顺序，任一步失败都**显式失败并留痕**：
//! ① 注册表解析（未登记即失败，不静默回退）→ ② 上下文预算 →
//! ③ 云端双重门禁（总开关 + 任务授权五维）→ ④ 费用预算 →
//! ⑤ 熔断闸门（打开时 0 次实际调用）→ ⑥ 调用（超时保护）→
//! ⑦ 原始输出留档并严格 Schema 校验 → ⑧ 记账 → ⑨ 写 `ModelCall` → ⑩ 返回。
//!
//! 本模块不渲染提示词（由 `recognize`/`grade`/`rubric` 渲染后传入），
//! 也不做业务语义映射（`JudgeOutcome` 等由调用方按其上下文构造）。

use std::sync::Arc;
use std::time::Duration;

use eg_domain::{
    BackendId, Clock, EngineStamp, MilliScore, ModelCall, ModelRole, Sha256Hex, TaskId,
};
use serde_json::Value;

use crate::breaker::CircuitBreaker;
use crate::budget::{BudgetLedger, PricingTier, cost_micros};
use crate::call::{CallContext, CallRecorder};
use crate::consent::{ConsentCheck, ConsentGate, TaskConsent, require_cloud_enabled};
use crate::error::InferenceError;
use crate::fixture::FixtureBackend;
use crate::http::{HttpRequest, Transport};
use crate::local::LocalServer;
use crate::openai::{ChatRequestSpec, ChatResponse, extract_json_object, parse_chat_response};
use crate::registry::{CloudEndpoint, Implementation, ModelEntry};
use crate::schema::{JudgeOutput, OcrOutput, RouterOutput, StructurerOutput};
use crate::types::{HealthReport, HealthState, TokenUsage};

/// 云端后端（`openai_compat`）：只构造请求与解析响应，授权/预算/记录在服务层。
pub struct CloudBackend {
    endpoint: CloudEndpoint,
    transport: Arc<dyn Transport>,
    credentials: Option<String>,
}

impl core::fmt::Debug for CloudBackend {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("CloudBackend")
            .field("base_url", &self.endpoint.base_url)
            .field("model_name", &self.endpoint.model_name)
            .field("credentials_env", &self.endpoint.credentials_env)
            .field("has_credentials", &self.credentials.is_some())
            .finish_non_exhaustive()
    }
}

impl CloudBackend {
    /// 构造云端后端（无凭据：请求不带 Authorization，便于测试注入桩）。
    #[must_use]
    pub fn new(endpoint: CloudEndpoint, transport: Arc<dyn Transport>) -> Self {
        Self {
            endpoint,
            transport,
            credentials: None,
        }
    }

    /// 注入凭据（生产由 `platform` 的 Keychain 读取后传入；**不写日志、不入库**）。
    #[must_use]
    pub fn with_credentials(mut self, credentials: Option<String>) -> Self {
        self.credentials = credentials;
        self
    }

    /// 端点信息。
    #[must_use]
    pub fn endpoint(&self) -> &CloudEndpoint {
        &self.endpoint
    }

    /// 发起一次 chat completions 调用。
    ///
    /// # Errors
    ///
    /// 传输失败/超时（瞬态）、HTTP 4xx（永久）、输出被截断（Schema）显式失败。
    pub fn chat(
        &self,
        spec: &ChatRequestSpec,
        timeout: Duration,
    ) -> Result<(ChatResponse, u64), InferenceError> {
        // DeepSeek 官方 OpenAI 兼容端点即 `{base_url}/chat/completions`
        // （`config/cloud.deepseek.toml`：base_url=https://api.deepseek.com）。
        let url = format!("{}/chat/completions", self.endpoint.base_url);
        let payload =
            serde_json::to_vec(&spec.to_body()).map_err(|error| InferenceError::Internal {
                detail: format!("请求序列化失败：{error}"),
            })?;
        let mut request = HttpRequest::post_json(url, payload, timeout);
        if let Some(secret) = &self.credentials {
            request
                .headers
                .push(("Authorization".to_owned(), format!("Bearer {secret}")));
        }
        let started = std::time::Instant::now();
        let response = self.transport.execute(&request)?;
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(0);
        if !response.is_success() {
            let detail = format!("云端返回 HTTP {}", response.status);
            return Err(
                if response.status == 429 || (500..600).contains(&response.status) {
                    InferenceError::Transient {
                        status: response.status,
                        detail,
                    }
                } else {
                    InferenceError::Permanent {
                        status: response.status,
                        detail,
                    }
                },
            );
        }
        let text = response.body_text()?;
        let parsed = parse_chat_response(&text)?;
        if parsed.is_truncated() {
            return Err(InferenceError::SchemaViolation {
                reason: "云端输出被 max_tokens 截断（finish_reason=length）".to_owned(),
            });
        }
        Ok((parsed, latency_ms))
    }
}

/// 后端选择（同一服务实例只跑一个后端，由组合根决定）。
#[derive(Debug, Clone)]
pub enum Backend {
    /// 本地 sidecar（`local_server`）。
    Local(Arc<LocalServer>),
    /// 夹具回放（`fixture`，测试/离线演示）。
    Fixture(Arc<FixtureBackend>),
    /// 云端（`openai_compat`，默认关闭）。
    Cloud(Arc<CloudBackend>),
}

impl Backend {
    /// 后端 id。
    #[must_use]
    pub fn id(&self) -> BackendId {
        match self {
            Self::Local(_) => BackendId::local_server(),
            Self::Fixture(_) => BackendId::fixture(),
            Self::Cloud(_) => BackendId::openai_compat(),
        }
    }
}

/// 调用方期望的输出 Schema（决定 ⑦ 的校验器）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpectedSchema {
    /// 只要求是单个 JSON 对象（调用方自行解释）。
    Any,
    /// OCR 块列表。
    Ocr,
    /// 判定（需要满分以校验分数一致性）。
    Judge {
        /// 本题满分。
        max_score: MilliScore,
    },
    /// 路由分类（需要允许的枚举集合）。
    Router {
        /// 允许的枚举值。
        allowed: Vec<String>,
    },
    /// 规则整理草稿。
    Structurer,
}

/// 通过严格校验的结构化产出。
#[derive(Debug, Clone, PartialEq)]
pub enum ValidatedOutput {
    /// 任意 JSON 对象。
    Any(Value),
    /// OCR 产出。
    Ocr(OcrOutput),
    /// 判定产出。
    Judge(JudgeOutput),
    /// 路由产出。
    Router(RouterOutput),
    /// 规则整理产出。
    Structurer(StructurerOutput),
}

impl ValidatedOutput {
    /// 原始 JSON（记录 `ModelCall.output` 用）。
    #[must_use]
    pub fn raw(&self) -> Value {
        match self {
            Self::Any(value) => value.clone(),
            Self::Ocr(output) => ocr_to_json(output),
            Self::Judge(output) => judge_to_json(output),
            Self::Router(output) => {
                serde_json::json!({"decision": output.decision, "confidence": output.confidence.get()})
            }
            Self::Structurer(output) => serde_json::json!({
                "items": output.items.iter().map(|item| serde_json::json!({
                    "title": item.title,
                    "criteria": item.criteria,
                    "max_score_milli": item.max_score_milli,
                    "required": item.required,
                    "depends": item.depends,
                })).collect::<Vec<_>>()
            }),
        }
    }
}

fn ocr_to_json(output: &OcrOutput) -> Value {
    let blocks: Vec<Value> = output
        .blocks
        .iter()
        .map(|block| {
            serde_json::json!({
                "kind": block.kind.as_str(),
                "text": block.text,
                "confidence": block.confidence.map(eg_domain::Confidence::get),
            })
        })
        .collect();
    serde_json::json!({"blocks": blocks})
}

fn judge_to_json(output: &JudgeOutput) -> Value {
    serde_json::json!({
        "judgement": output.judgement.as_str(),
        "score_milli": output.score_milli,
        "reason": output.reason,
        "confidence": output.confidence.get(),
        "matched_alternative": output.matched_alternative,
        "calibration_id": output.calibration_id.as_ref().map(ToString::to_string),
        "satisfied": output.satisfied,
        "missing": output.missing,
    })
}

/// 一次完成请求。
#[derive(Debug, Clone)]
pub struct CompletionRequest {
    /// 所属任务。
    pub task_id: TaskId,
    /// 注册表模型 id（必须已登记）。
    pub model_id: String,
    /// 已渲染的提示词文本（参与哈希与夹具键）。
    pub prompt: String,
    /// 输入字节（图像字节或文本载荷；参与哈希与夹具键）。
    pub input: Vec<u8>,
    /// 发给模型的 chat 请求。
    pub chat: ChatRequestSpec,
    /// 云端外发内容范围指纹（云端必填）。
    pub scope_hash: Option<Sha256Hex>,
    /// 本次预计费用（微元；仅云端用于调用前预算检查）。
    pub estimated_cost_micros: u64,
    /// 提示词 token 预估（用于上下文超限的显式拒绝）。
    pub prompt_tokens_estimate: Option<u32>,
    /// 期望的输出 Schema。
    pub expected: ExpectedSchema,
    /// 覆盖默认超时。
    pub timeout: Option<Duration>,
}

impl CompletionRequest {
    /// 构造最简请求（默认期望任意 JSON、无云端范围）。
    #[must_use]
    pub fn new(
        task_id: TaskId,
        model_id: impl Into<String>,
        prompt: impl Into<String>,
        input: Vec<u8>,
        chat: ChatRequestSpec,
    ) -> Self {
        Self {
            task_id,
            model_id: model_id.into(),
            prompt: prompt.into(),
            input,
            chat,
            scope_hash: None,
            estimated_cost_micros: 0,
            prompt_tokens_estimate: None,
            expected: ExpectedSchema::Any,
            timeout: None,
        }
    }

    /// 设置期望 Schema。
    #[must_use]
    pub fn with_expected(mut self, expected: ExpectedSchema) -> Self {
        self.expected = expected;
        self
    }

    /// 设置云端外发范围。
    #[must_use]
    pub fn with_scope(mut self, scope_hash: Sha256Hex) -> Self {
        self.scope_hash = Some(scope_hash);
        self
    }

    /// 设置费用预估与期望 token。
    #[must_use]
    pub fn with_budget_hint(mut self, estimated_cost_micros: u64, prompt_tokens: u32) -> Self {
        self.estimated_cost_micros = estimated_cost_micros;
        self.prompt_tokens_estimate = Some(prompt_tokens);
        self
    }

    /// 覆盖超时。
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

/// 一次调用的结果。
#[derive(Debug, Clone)]
pub struct CompletionOutcome {
    /// 通过校验的结构化产出。
    pub validated: ValidatedOutput,
    /// 引擎指纹（进 `ModelCall` 与缓存键）。
    pub engine: EngineStamp,
    /// 调用记录（已落库）。
    pub model_call: ModelCall,
    /// token 用量。
    pub usage: TokenUsage,
    /// 本次费用（微元；本地恒 0）。
    pub cost_micros: u64,
    /// 缓存键（模型/参数/提示词/输入任一变化即变化）。
    pub cache_key: String,
}

/// 服务配置（来自 `CONFIG.md` 的应用配置）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceConfig {
    /// 云端总开关（`privacy.cloud_enabled`，默认关）。
    pub cloud_enabled: bool,
    /// 计价时段（用于云端费用记账）。
    pub pricing_tier: PricingTier,
    /// 默认单次超时（毫秒）。
    pub default_timeout_ms: u64,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            cloud_enabled: false,
            pricing_tier: PricingTier::Peak,
            default_timeout_ms: 60_000,
        }
    }
}

/// 推理服务（编排层）。
pub struct InferenceService {
    registry: Arc<crate::registry::ModelRegistry>,
    backend: Backend,
    breaker: Arc<CircuitBreaker>,
    budget: Arc<BudgetLedger>,
    consents: Arc<dyn ConsentGate>,
    recorder: CallRecorder,
    config: ServiceConfig,
    clock: Arc<dyn Clock>,
}

impl core::fmt::Debug for InferenceService {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("InferenceService")
            .field("backend", &self.backend.id())
            .field("cloud_enabled", &self.config.cloud_enabled)
            .field("breaker", &self.breaker_state())
            .finish_non_exhaustive()
    }
}

impl InferenceService {
    /// 组装服务。
    // 8 个依赖项均为显式注入的独立组件；拆成结构体只会把校验/默认值藏到调用点。
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        registry: Arc<crate::registry::ModelRegistry>,
        backend: Backend,
        breaker: Arc<CircuitBreaker>,
        budget: Arc<BudgetLedger>,
        consents: Arc<dyn ConsentGate>,
        recorder: CallRecorder,
        config: ServiceConfig,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            registry,
            backend,
            breaker,
            budget,
            consents,
            recorder,
            config,
            clock,
        }
    }

    /// 注册表（只读）。
    #[must_use]
    pub fn registry(&self) -> &crate::registry::ModelRegistry {
        &self.registry
    }

    /// 当前后端 id。
    #[must_use]
    pub fn backend_id(&self) -> BackendId {
        self.backend.id()
    }

    /// 熔断状态标签（指标/日志用）。
    #[must_use]
    pub fn breaker_state(&self) -> &'static str {
        self.breaker.state_label()
    }

    /// 健康报告（不改变状态、不发起模型调用）。
    #[must_use]
    pub fn health(&self) -> HealthReport {
        match &self.backend {
            Backend::Local(server) => server.probe_health(),
            Backend::Fixture(fixture) => HealthReport {
                backend: BackendId::fixture(),
                state: HealthState::Ready,
                probe_latency_ms: None,
                detail: format!("夹具回放就绪（{} 条）", fixture.len()),
            },
            Backend::Cloud(cloud) => {
                if self.config.cloud_enabled {
                    HealthReport {
                        backend: BackendId::openai_compat(),
                        state: HealthState::Ready,
                        probe_latency_ms: None,
                        detail: format!("云端已启用：{}", cloud.endpoint().base_url),
                    }
                } else {
                    HealthReport {
                        backend: BackendId::openai_compat(),
                        state: HealthState::Unavailable,
                        probe_latency_ms: None,
                        detail: "云端总开关关闭（privacy.cloud_enabled=false）".to_owned(),
                    }
                }
            }
        }
    }

    /// 执行一次调用（完整时序见模块文档）。
    ///
    /// # Errors
    ///
    /// 注册表未命中、上下文超限、策略拦截、预算超限、熔断打开、后端失败、
    /// Schema 不合法、记录失败等，全部显式返回 [`InferenceError`]。
    pub fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionOutcome, InferenceError> {
        // ① 注册表解析：未登记即失败，不静默回退默认。
        let entry = self.registry.require(&request.model_id)?;
        let prompt_hash = Some(Sha256Hex::of(request.prompt.as_bytes()));
        let input_hash = Sha256Hex::of(&request.input);
        let engine = entry.engine_stamp(prompt_hash);
        let cache_key = entry.cache_key(prompt_hash, input_hash);

        // ② 上下文超限：显式拒绝，禁止静默截断。
        if let Some(estimate) = request.prompt_tokens_estimate {
            let needed = estimate.saturating_add(request.chat.max_tokens);
            if needed > entry.context_length {
                let error = InferenceError::ContextOverflow {
                    needed,
                    limit: entry.context_length,
                };
                return self.fail(&request, entry, &engine, input_hash, &error);
            }
        }

        // ③④ 云端双重门禁 + 费用预算
        let mut context = CallContext::local(request.task_id, entry.role, prompt_hash);
        let mut consent: Option<TaskConsent> = None;
        if entry.is_cloud() {
            if let Err(error) = require_cloud_enabled(self.config.cloud_enabled) {
                return self.fail(&request, entry, &engine, input_hash, &error);
            }
            let Some(scope_hash) = request.scope_hash else {
                let error = InferenceError::ConsentMissing {
                    detail: "云端请求必须显式给出外发范围指纹".to_owned(),
                };
                return self.fail(&request, entry, &engine, input_hash, &error);
            };
            let check = ConsentCheck {
                task_id: request.task_id,
                backend: BackendId::openai_compat(),
                scope_hash,
                now: self.clock.now(),
            };
            match self.consents.validate(&check) {
                Ok(active) => consent = Some(active),
                Err(error) => return self.fail(&request, entry, &engine, input_hash, &error),
            }
            if let Err(error) = self
                .budget
                .authorize(request.task_id, request.estimated_cost_micros)
            {
                return self.fail(&request, entry, &engine, input_hash, &error);
            }
            if let Some(active) = &consent {
                context.consent_id = Some(active.consent_id);
            }
        }

        // ⑤ 熔断闸门：打开时 0 次实际调用。
        if let Err(error) = self.breaker.allow() {
            return self.fail(&request, entry, &engine, input_hash, &error);
        }

        // ⑥ 调用
        let timeout = request
            .timeout
            .unwrap_or_else(|| Duration::from_millis(self.config.default_timeout_ms));
        let attempt = match self.attempt(entry, &request, timeout) {
            Ok(attempt) => attempt,
            Err(error) => {
                return self.fail(&request, entry, &engine, input_hash, &error);
            }
        };

        // ⑦ 严格 Schema 校验
        let parsed = Self::parse(&attempt.raw_content, &request.expected);
        let (output, validated) = match parsed {
            Ok(parsed) => parsed,
            Err(error) => {
                // 已产生的云端费用照实记账（输出不合法也要记账）。
                self.budget.charge(request.task_id, attempt.cost_micros);
                self.breaker.record_failure(&error);
                return self.fail(&request, entry, &engine, input_hash, &error);
            }
        };

        // ⑧ 记账：本地恒 0；云端按定价表。
        self.budget.charge(request.task_id, attempt.cost_micros);
        if let Some(active) = &consent
            && self.budget.spent_micros(request.task_id) >= active.budget_micros
        {
            // 额度用尽：后续调用必须被授权门禁拒绝（0 次外联）。
            let _ = self.consents.mark_exhausted(request.task_id);
        }
        self.breaker.record_success();

        // ⑨ 记录 ModelCall（失败即调用失败）
        let model_call = self.recorder.record_success(
            &context,
            &engine,
            input_hash,
            output.clone(),
            attempt.latency_ms,
            attempt.cost_micros,
        )?;

        Ok(CompletionOutcome {
            validated,
            engine,
            model_call,
            usage: attempt.usage,
            cost_micros: attempt.cost_micros,
            cache_key,
        })
    }

    fn attempt(
        &self,
        entry: &ModelEntry,
        request: &CompletionRequest,
        timeout: Duration,
    ) -> Result<Attempt, InferenceError> {
        match &self.backend {
            Backend::Fixture(fixture) => {
                let replay = fixture.replay(&request.prompt, &request.input)?;
                Ok(Attempt {
                    raw_content: replay.output_raw,
                    usage: TokenUsage::ZERO,
                    latency_ms: 0,
                    cost_micros: 0,
                })
            }
            Backend::Local(server) => {
                let (response, latency_ms) = server.chat(&request.chat, timeout)?;
                Ok(Attempt {
                    raw_content: response.content,
                    usage: response.usage,
                    latency_ms,
                    cost_micros: 0,
                })
            }
            Backend::Cloud(cloud) => {
                let (response, latency_ms) = cloud.chat(&request.chat, timeout)?;
                let cost = entry.cloud.as_ref().map_or(0, |endpoint| {
                    cost_micros(&endpoint.pricing, self.config.pricing_tier, response.usage)
                });
                Ok(Attempt {
                    raw_content: response.content,
                    usage: response.usage,
                    latency_ms,
                    cost_micros: cost,
                })
            }
        }
    }

    fn parse(
        content: &str,
        expected: &ExpectedSchema,
    ) -> Result<(Value, ValidatedOutput), InferenceError> {
        let value = extract_json_object(content)?;
        let validated = match expected {
            ExpectedSchema::Any => ValidatedOutput::Any(value.clone()),
            ExpectedSchema::Ocr => ValidatedOutput::Ocr(OcrOutput::validate(&value)?),
            ExpectedSchema::Judge { max_score } => {
                ValidatedOutput::Judge(JudgeOutput::validate(&value, *max_score)?)
            }
            ExpectedSchema::Router { allowed } => {
                let allowed_refs: Vec<&str> = allowed.iter().map(String::as_str).collect();
                ValidatedOutput::Router(RouterOutput::validate(&value, &allowed_refs)?)
            }
            ExpectedSchema::Structurer => {
                ValidatedOutput::Structurer(StructurerOutput::validate(&value)?)
            }
        };
        Ok((validated.raw(), validated))
    }

    /// 失败统一处理：计数熔断 → 记录失败 `ModelCall` → 返回原错误。
    fn fail(
        &self,
        request: &CompletionRequest,
        entry: &ModelEntry,
        engine: &EngineStamp,
        input_hash: Sha256Hex,
        error: &InferenceError,
    ) -> Result<CompletionOutcome, InferenceError> {
        self.breaker.record_failure(error);
        let context = CallContext::local(request.task_id, entry.role, engine.prompt_hash);
        if entry.is_cloud() && context.consent_id.is_none() {
            // 云端调用根本没有发生（总开关关闭/无授权/范围不符/预算超限）：
            // 领域不变量要求云端 ModelCall 必带 consent_id，因此这里**不伪造调用记录**，
            // 改由审计留痕（SPEC §4「失败也留痕」+ §8「无 ModelCall 就出分」）。
            let event = crate::call::AuditEvent::new(
                "inference.policy_blocked",
                "model",
                entry.id.clone(),
                format!("{}：{error}", error.class_label()),
            )
            .with_task(request.task_id);
            let _ = self.recorder.audit_only(&event);
            return Err(clone_error(error));
        }
        let _ = self
            .recorder
            .record_failure(&context, engine, input_hash, error, 0);
        Err(clone_error(error))
    }
}

/// 失败调用记录用的 `Attempt`（成功解析前的最小产物）。
struct Attempt {
    raw_content: String,
    usage: TokenUsage,
    latency_ms: u64,
    cost_micros: u64,
}

fn clone_error(error: &InferenceError) -> InferenceError {
    match error {
        InferenceError::RegistryInvalid { reason } => InferenceError::RegistryInvalid {
            reason: reason.clone(),
        },
        InferenceError::RegistryMiss { model_id } => InferenceError::RegistryMiss {
            model_id: model_id.clone(),
        },
        InferenceError::LicenseDenied { model_id, license } => InferenceError::LicenseDenied {
            model_id: model_id.clone(),
            license: license.clone(),
        },
        InferenceError::LicenseReviewRequired { model_id, license } => {
            InferenceError::LicenseReviewRequired {
                model_id: model_id.clone(),
                license: license.clone(),
            }
        }
        InferenceError::ContextOverflow { needed, limit } => InferenceError::ContextOverflow {
            needed: *needed,
            limit: *limit,
        },
        InferenceError::BackendUnavailable { detail } => InferenceError::BackendUnavailable {
            detail: detail.clone(),
        },
        InferenceError::Timeout { stage, timeout_ms } => InferenceError::Timeout {
            stage: stage.clone(),
            timeout_ms: *timeout_ms,
        },
        InferenceError::Transient { status, detail } => InferenceError::Transient {
            status: *status,
            detail: detail.clone(),
        },
        InferenceError::Permanent { status, detail } => InferenceError::Permanent {
            status: *status,
            detail: detail.clone(),
        },
        InferenceError::Transport { detail } => InferenceError::Transport {
            detail: detail.clone(),
        },
        InferenceError::TransportUnsupported { detail } => InferenceError::TransportUnsupported {
            detail: detail.clone(),
        },
        InferenceError::SchemaViolation { reason } => InferenceError::SchemaViolation {
            reason: reason.clone(),
        },
        InferenceError::BudgetExceeded {
            spent_micros,
            limit_micros,
            needed_micros,
        } => InferenceError::BudgetExceeded {
            spent_micros: *spent_micros,
            limit_micros: *limit_micros,
            needed_micros: *needed_micros,
        },
        InferenceError::CloudDisabled => InferenceError::CloudDisabled,
        InferenceError::ConsentMissing { detail } => InferenceError::ConsentMissing {
            detail: detail.clone(),
        },
        InferenceError::ConsentInactive { consent_id, state } => InferenceError::ConsentInactive {
            consent_id: consent_id.clone(),
            state: state.clone(),
        },
        InferenceError::ScopeMismatch => InferenceError::ScopeMismatch,
        InferenceError::ConsentBudgetExhausted {
            limit_micros,
            spent_micros,
        } => InferenceError::ConsentBudgetExhausted {
            limit_micros: *limit_micros,
            spent_micros: *spent_micros,
        },
        InferenceError::CircuitOpen { retry_after_ms } => InferenceError::CircuitOpen {
            retry_after_ms: *retry_after_ms,
        },
        InferenceError::FixtureMiss { key } => InferenceError::FixtureMiss { key: key.clone() },
        InferenceError::Cancelled => InferenceError::Cancelled,
        InferenceError::Platform { detail } => InferenceError::Platform {
            detail: detail.clone(),
        },
        InferenceError::ConfigUnresolved { key } => {
            InferenceError::ConfigUnresolved { key: key.clone() }
        }
        InferenceError::Internal { detail } => InferenceError::Internal {
            detail: detail.clone(),
        },
    }
}

/// 实现类型展示（诊断用）。
#[must_use]
pub fn implementation_label(implementation: Option<Implementation>) -> &'static str {
    implementation.map_or("remote", Implementation::as_str)
}

/// 角色中文标签（日志用）。
#[must_use]
pub const fn role_label(role: ModelRole) -> &'static str {
    match role {
        ModelRole::Ocr => "识别",
        ModelRole::Router => "路由",
        ModelRole::Judge => "判定",
        ModelRole::Structurer => "规则整理",
    }
}
