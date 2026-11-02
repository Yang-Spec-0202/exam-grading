//! 服务编排集成测试（`SPEC-inference.md` §4/§7/§9）。
//!
//! 全部使用注入替身：**不发起真实网络请求、不依赖真实模型**。
//! 覆盖：注册表未命中、上下文超限、云端双重门禁（0 次外联）、预算、熔断、
//! 严格 Schema、夹具字节一致、`ModelCall` 落库与费用记账。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use eg_domain::{MilliScore, ModelRole, Sha256Hex, TaskId, Ulid};
use eg_inference::testing::{
    CounterIds, FakeRunner, RecordingAudit, RecordingStore, StubOutcome, StubTransport, TestClock,
};
use eg_inference::{
    Backend, BreakerConfig, BudgetLedger, CallRecorder, ChatMessage, ChatRequestSpec,
    CircuitBreaker, CloudBackend, CompletionRequest, ExpectedSchema, FixtureBackend, FixtureEntry,
    FixtureStore, InferenceService, LifecycleConfig, LocalServer, LocalServerSpec,
    MemoryConsentGate, ModelRegistry, PortAllocator, ProcessRunner, ServiceConfig, TaskConsent,
    Transport, WeightsFile, fixture_key,
};

fn task(seed: u128) -> TaskId {
    TaskId::from_ulid(Ulid::from_u128(seed))
}

const REGISTRY: &str = r#"schema_version = 1

[[models]]
id = "local-ocr"
version = "1.6"
role = "ocr"
backend = "local_server"
implementation = "llama.cpp"
quantization = "bf16"
context_length = 8192
license = "MIT"
max_concurrency = 1
timeout_secs = 60
prompt_schema_version = "ocr.blocks.v1"

[models.local]
binary = "llama-server"
port = 9101
startup_timeout_secs = 2
extra_args = []

[[models.weights]]
component = "model"
file = "model.gguf"
sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
bytes = 1024

[[models]]
id = "cloud-judge"
version = "DeepSeek-V4.1-Flash-1017"
role = "judge"
backend = "openai_compat"
quantization = "remote"
context_length = 65536
license = "MIT"
max_concurrency = 2
timeout_secs = 120
prompt_schema_version = "judge.outcome.v1"

[models.cloud]
base_url = "https://api.example.com"
model_name = "cloud-judge"
credentials_env = "TEST_API_KEY"
input_cache_miss_idle_micros = 1000000
input_cache_miss_peak_micros = 2000000
input_cache_hit_idle_micros = 20000
input_cache_hit_peak_micros = 40000
output_idle_micros = 4000000
output_peak_micros = 8000000

[[models]]
id = "fixture-judge"
version = "1"
role = "judge"
backend = "fixture"
quantization = "fixture"
context_length = 8192
license = "MIT"
max_concurrency = 1
timeout_secs = 20
prompt_schema_version = "judge.outcome.v1"
"#;

fn registry() -> Arc<ModelRegistry> {
    Arc::new(ModelRegistry::parse_str(REGISTRY).expect("测试注册表必须合法"))
}

struct Harness {
    service: InferenceService,
    store: Arc<RecordingStore>,
    audit: Arc<RecordingAudit>,
    stub: StubTransport,
    runner: Arc<FakeRunner>,
    clock: Arc<TestClock>,
    consents: Arc<MemoryConsentGate>,
    budget: Arc<BudgetLedger>,
    models_dir: tempfile::TempDir,
}

struct HarnessOptions {
    cloud_enabled: bool,
    backend: BackendKind,
    consent: Option<TaskConsent>,
    budget_limit: Option<u64>,
}

#[derive(Clone, Copy)]
enum BackendKind {
    Local,
    Cloud,
    Fixture {
        prompt: &'static str,
        input: &'static [u8],
        output: &'static str,
    },
}

/// 按用例选项组装后端（本地 sidecar / 云端 / 夹具回放）。
fn build_backend(
    options: &HarnessOptions,
    stub: &StubTransport,
    runner: &Arc<FakeRunner>,
    models_dir: &std::path::Path,
    clock: &Arc<TestClock>,
) -> Backend {
    let fixture_store = match options.backend {
        BackendKind::Fixture {
            prompt,
            input,
            output,
        } => {
            let mut store = FixtureStore::new();
            store
                .insert(FixtureEntry {
                    key: fixture_key("fixture-judge", "1", "judge.outcome.v1", prompt, input),
                    model_id: "fixture-judge".to_owned(),
                    model_version: "1".to_owned(),
                    prompt_schema_version: "judge.outcome.v1".to_owned(),
                    output_raw: output.to_owned(),
                })
                .unwrap();
            store
        }
        _ => FixtureStore::new(),
    };

    match options.backend {
        BackendKind::Local => {
            let spec = LocalServerSpec {
                unit: "local-ocr".to_owned(),
                binary: "llama-server".to_owned(),
                models_dir: models_dir.to_path_buf(),
                weights: vec![WeightsFile {
                    component: "model".to_owned(),
                    file: "model.gguf".to_owned(),
                    sha256: Sha256Hex::of(b"w"),
                    bytes: 1,
                }],
                preferred_port: 9101,
                host: "127.0.0.1".to_owned(),
                context_length: 8192,
                temperature: 0.0,
                chat_template: None,
                extra_args: Vec::new(),
                startup_timeout_ms: 500,
            };
            let server = LocalServer::new(
                spec,
                LifecycleConfig {
                    poll_interval_ms: 1,
                    stop_timeout_ms: 5,
                    port_attempts: 1,
                },
                Arc::new(stub.clone()) as Arc<dyn Transport>,
                runner.clone() as Arc<dyn ProcessRunner>,
                Arc::new(FixedPort(9101)) as Arc<dyn PortAllocator>,
                clock.clone(),
            );
            Backend::Local(Arc::new(server))
        }
        BackendKind::Cloud => {
            let entry = registry()
                .require("cloud-judge")
                .unwrap()
                .cloud
                .clone()
                .unwrap();
            Backend::Cloud(Arc::new(CloudBackend::new(
                entry,
                Arc::new(stub.clone()) as Arc<dyn Transport>,
            )))
        }
        BackendKind::Fixture { .. } => Backend::Fixture(Arc::new(FixtureBackend::new(
            "fixture-judge",
            "1",
            "judge.outcome.v1",
            fixture_store,
        ))),
    }
}

fn harness(options: HarnessOptions) -> Harness {
    let clock = Arc::new(TestClock::at_ms(0));
    let stub = StubTransport::new();
    stub.route(
        "/health",
        StubOutcome::Response(eg_inference::HttpResponse::new(
            200,
            b"{\"status\":\"ok\"}".to_vec(),
        )),
    );
    let runner = Arc::new(FakeRunner::default());
    let models_dir = tempfile::tempdir().unwrap();

    let backend = build_backend(&options, &stub, &runner, models_dir.path(), &clock);

    let store = Arc::new(RecordingStore::default());
    let audit = Arc::new(RecordingAudit::default());
    let recorder = CallRecorder::new(
        store.clone(),
        audit.clone(),
        clock.clone(),
        Arc::new(CounterIds::default()),
    );
    let consents = Arc::new(MemoryConsentGate::new());
    if let Some(consent) = options.consent {
        consents.grant(consent);
    }
    let budget = Arc::new(BudgetLedger::new(clock.clone()));
    if let Some(limit) = options.budget_limit {
        budget.open_limited(task(1), limit);
    }

    let service = InferenceService::new(
        registry(),
        backend,
        Arc::new(CircuitBreaker::with_config(
            clock.clone(),
            BreakerConfig {
                failure_threshold: 3,
                open_duration_ms: 60_000,
            },
        )),
        budget.clone(),
        consents.clone(),
        recorder,
        ServiceConfig {
            cloud_enabled: options.cloud_enabled,
            pricing_tier: eg_inference::PricingTier::Peak,
            default_timeout_ms: 5_000,
        },
        clock.clone(),
    );
    Harness {
        service,
        store,
        audit,
        stub,
        runner,
        clock,
        consents,
        budget,
        models_dir,
    }
}

#[derive(Debug)]
struct FixedPort(u16);

impl PortAllocator for FixedPort {
    fn pick(&self, _preferred: u16, _attempts: u8) -> Result<u16, eg_inference::InferenceError> {
        Ok(self.0)
    }
}

fn chat_calls(stub: &StubTransport) -> usize {
    stub.requests()
        .iter()
        .filter(|request| request.url.contains("chat/completions"))
        .count()
}

fn local_request(task_id: TaskId) -> CompletionRequest {
    CompletionRequest::new(
        task_id,
        "local-ocr",
        "OCR:",
        b"page".to_vec(),
        ChatRequestSpec::new("local-ocr", vec![ChatMessage::user("OCR:")], 64),
    )
}

fn cloud_request(task_id: TaskId, scope: Sha256Hex) -> CompletionRequest {
    CompletionRequest::new(
        task_id,
        "cloud-judge",
        "judge prompt",
        b"answer".to_vec(),
        ChatRequestSpec::new("cloud-judge", vec![ChatMessage::user("judge")], 64),
    )
    .with_scope(scope)
    .with_budget_hint(1_000, 100)
}

fn consent_for(task_id: TaskId, scope: Sha256Hex, budget: u64) -> TaskConsent {
    TaskConsent {
        consent_id: eg_domain::ConsentId::from_ulid(Ulid::from_u128(99)),
        task_id,
        backend: eg_domain::BackendId::openai_compat(),
        scope_hash: scope,
        budget_micros: budget,
        state: eg_inference::ConsentState::Active,
        granted_at: eg_domain::time::parse_rfc3339("1970-01-01T00:00:00Z").unwrap(),
        ended_at: None,
    }
}

#[test]
fn registry_miss_fails_before_any_call() {
    let harness = harness(HarnessOptions {
        cloud_enabled: false,
        backend: BackendKind::Local,
        consent: None,
        budget_limit: None,
    });
    let mut request = local_request(task(1));
    request.model_id = "not-registered".to_owned();
    let error = harness.service.complete(request).unwrap_err();
    assert!(matches!(
        error,
        eg_inference::InferenceError::RegistryMiss { .. }
    ));
    assert_eq!(chat_calls(&harness.stub), 0, "注册表未命中不得发起调用");
    assert_eq!(harness.store.len(), 0, "未发生调用则不写 ModelCall");
}

#[test]
fn context_overflow_is_rejected_before_any_call() {
    let harness = harness(HarnessOptions {
        cloud_enabled: false,
        backend: BackendKind::Local,
        consent: None,
        budget_limit: None,
    });
    let request = local_request(task(1)).with_budget_hint(0, 9_000);
    let error = harness.service.complete(request).unwrap_err();
    assert!(matches!(
        error,
        eg_inference::InferenceError::ContextOverflow { .. }
    ));
    assert_eq!(chat_calls(&harness.stub), 0);
}

#[test]
fn local_success_records_model_call_with_zero_cost() {
    let harness = harness(HarnessOptions {
        cloud_enabled: false,
        backend: BackendKind::Local,
        consent: None,
        budget_limit: None,
    });
    let ocr_content = serde_json::json!({
        "blocks": [{"kind": "formula", "text": r"\(x^{2}\)"}]
    })
    .to_string();
    harness.stub.push_json(
        200,
        &serde_json::json!({
            "choices": [{"message": {"content": ocr_content}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1200, "completion_tokens": 80}
        })
        .to_string(),
    );
    let request = local_request(task(1)).with_expected(ExpectedSchema::Ocr);
    let outcome = harness.service.complete(request).unwrap();
    assert_eq!(outcome.cost_micros, 0, "本地费用必须为 0");
    assert_eq!(outcome.engine.backend, eg_domain::BackendId::local_server());
    assert_eq!(harness.store.len(), 1);
    let call = &harness.store.calls()[0];
    assert_eq!(call.role, ModelRole::Ocr);
    assert_eq!(call.cost_micros, 0);
    assert!(call.consent_id.is_none(), "本地调用不带授权 id");
    assert_eq!(
        outcome.validated,
        eg_inference::ValidatedOutput::Ocr(
            eg_inference::OcrOutput::validate(&serde_json::json!({
                "blocks": [{"kind": "formula", "text": "\\(x^{2}\\)"}]
            }))
            .unwrap()
        )
    );
    assert_eq!(
        harness.runner.spawn_count(),
        1,
        "本地后端必须拉起一次 sidecar"
    );
    let _ = &harness.models_dir;
}

#[test]
fn truncated_or_malformed_output_fails_and_is_recorded() {
    for (index, body) in [
        r#"{"choices":[{"message":{"content":"not json"},"finish_reason":"stop"}]}"#,
        r#"{"choices":[{"message":{"content":"{\"blocks\":"},"finish_reason":"length"}]}"#,
        r#"{"choices":[{"message":{"content":"```json\n{}\n```"},"finish_reason":"stop"}]}"#,
    ]
    .iter()
    .enumerate()
    {
        let harness = harness(HarnessOptions {
            cloud_enabled: false,
            backend: BackendKind::Local,
            consent: None,
            budget_limit: None,
        });
        harness.stub.push_json(200, body);
        let request = local_request(task(u128::try_from(index).unwrap() + 1))
            .with_expected(ExpectedSchema::Ocr);
        let error = harness.service.complete(request).unwrap_err();
        assert!(
            matches!(error, eg_inference::InferenceError::SchemaViolation { .. }),
            "第 {index} 条必须被判为 Schema 不合法：{error}"
        );
        assert_eq!(harness.store.len(), 1, "失败也必须写 ModelCall");
        assert!(harness.store.calls()[0].output["error"]["class"].is_string());
    }
}

#[test]
fn cloud_switch_off_makes_zero_outbound_calls() {
    let scope = Sha256Hex::of(b"scope");
    let harness = harness(HarnessOptions {
        cloud_enabled: false,
        backend: BackendKind::Cloud,
        consent: Some(consent_for(task(1), scope, 1_000_000)),
        budget_limit: None,
    });
    let error = harness
        .service
        .complete(cloud_request(task(1), scope))
        .unwrap_err();
    assert!(matches!(error, eg_inference::InferenceError::CloudDisabled));
    assert_eq!(chat_calls(&harness.stub), 0);
    assert_eq!(harness.store.len(), 0, "未发生的调用不伪造 ModelCall");
    assert_eq!(harness.audit.events().len(), 1, "策略拦截必须写审计留痕");
    assert_eq!(harness.audit.events()[0].action, "inference.policy_blocked");
}

#[test]
fn cloud_without_consent_makes_zero_outbound_calls() {
    let scope = Sha256Hex::of(b"scope");
    let harness = harness(HarnessOptions {
        cloud_enabled: true,
        backend: BackendKind::Cloud,
        consent: None,
        budget_limit: None,
    });
    let error = harness
        .service
        .complete(cloud_request(task(1), scope))
        .unwrap_err();
    assert!(matches!(
        error,
        eg_inference::InferenceError::ConsentMissing { .. }
    ));
    assert_eq!(chat_calls(&harness.stub), 0, "无授权必须 0 次外联");
    assert_eq!(harness.store.len(), 0);
    assert_eq!(harness.audit.events().len(), 1);
}

#[test]
fn cloud_scope_mismatch_makes_zero_outbound_calls() {
    let granted = Sha256Hex::of(b"scope:answer_crop");
    let harness = harness(HarnessOptions {
        cloud_enabled: true,
        backend: BackendKind::Cloud,
        consent: Some(consent_for(task(1), granted, 1_000_000)),
        budget_limit: None,
    });
    let error = harness
        .service
        .complete(cloud_request(task(1), Sha256Hex::of(b"scope:full_page")))
        .unwrap_err();
    assert!(matches!(error, eg_inference::InferenceError::ScopeMismatch));
    assert_eq!(chat_calls(&harness.stub), 0);
}

#[test]
fn budget_exceeded_is_checked_before_the_call() {
    let scope = Sha256Hex::of(b"scope");
    let harness = harness(HarnessOptions {
        cloud_enabled: true,
        backend: BackendKind::Cloud,
        consent: Some(consent_for(task(1), scope, 1_000_000)),
        budget_limit: Some(500),
    });
    let mut request = cloud_request(task(1), scope);
    request.estimated_cost_micros = 600;
    let error = harness.service.complete(request).unwrap_err();
    assert!(matches!(
        error,
        eg_inference::InferenceError::BudgetExceeded { .. }
    ));
    assert_eq!(chat_calls(&harness.stub), 0, "预算超限必须不发请求");
}

#[test]
fn cloud_success_charges_exact_cost_writes_audit_and_exhausts_consent() {
    let scope = Sha256Hex::of(b"scope");
    let harness = harness(HarnessOptions {
        cloud_enabled: true,
        backend: BackendKind::Cloud,
        // 授权额度正好等于一次调用费用：100 输入 token ×2 微元 + 100 输出 token ×8 微元 = 1000
        consent: Some(consent_for(task(1), scope, 1_000)),
        budget_limit: None,
    });
    harness.stub.push_json(
        200,
        &serde_json::json!({
            "choices": [{
                "message": {"content": r#"{"judgement":"match","score_milli":5000,"reason":"正确","confidence":0.9}"#},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 100, "completion_tokens": 100}
        })
        .to_string(),
    );
    let request = cloud_request(task(1), scope).with_expected(ExpectedSchema::Judge {
        max_score: MilliScore::from_points(5).unwrap(),
    });
    let outcome = harness.service.complete(request).unwrap();
    assert_eq!(outcome.cost_micros, 1_000, "费用必须与定价表逐微元一致");
    assert_eq!(
        harness.budget.spent_micros(task(1)),
        1_000,
        "记账必须与本次费用一致"
    );
    assert_eq!(
        harness.consents.get(task(1)).unwrap().state,
        eg_inference::ConsentState::Exhausted,
        "额度用尽后授权必须置为已用尽"
    );
    assert_eq!(
        harness.store.calls()[0].consent_id,
        Some(eg_domain::ConsentId::from_ulid(Ulid::from_u128(99))),
        "云端 ModelCall 必须带 consent_id"
    );
    assert_eq!(harness.audit.events().len(), 1, "云端调用必须写审计");

    // 额度用尽后：下一次调用必须被门禁拒绝且 0 次外联
    harness.stub.push_json(
        200,
        r#"{"choices":[{"message":{"content":"{}"},"finish_reason":"stop"}]}"#,
    );
    let error = harness
        .service
        .complete(cloud_request(task(1), scope))
        .unwrap_err();
    assert!(matches!(
        error,
        eg_inference::InferenceError::ConsentInactive { .. }
    ));
    assert_eq!(chat_calls(&harness.stub), 1, "用尽后必须 0 次新外联");
}

#[test]
fn breaker_opens_after_three_transient_failures_and_probe_recovers() {
    let harness = harness(HarnessOptions {
        cloud_enabled: false,
        backend: BackendKind::Local,
        consent: None,
        budget_limit: None,
    });
    // 三次 503 → 瞬态失败
    for attempt in 0..3 {
        harness.stub.push_json(503, "{}");
        let error = harness
            .service
            .complete(local_request(task(1)))
            .unwrap_err();
        assert!(
            matches!(
                error,
                eg_inference::InferenceError::Transient { status: 503, .. }
            ),
            "第 {attempt} 次应为瞬态失败，实际：{error}"
        );
    }
    assert_eq!(harness.service.breaker_state(), "open");

    // 熔断打开：第 4 次快速失败且不发出 chat 请求
    let error = harness
        .service
        .complete(local_request(task(1)))
        .unwrap_err();
    assert!(matches!(
        error,
        eg_inference::InferenceError::CircuitOpen { .. }
    ));
    assert_eq!(chat_calls(&harness.stub), 3, "打开期间 0 次实际调用");

    // 60 秒后放行 1 次探测，成功即恢复
    harness.clock.advance_ms(60_000);
    let probe_content = serde_json::json!({"blocks": [{"kind": "text", "text": "ok"}]}).to_string();
    harness.stub.push_json(
        200,
        &serde_json::json!({
            "choices": [{"message": {"content": probe_content}, "finish_reason": "stop"}]
        })
        .to_string(),
    );
    harness
        .service
        .complete(local_request(task(1)).with_expected(ExpectedSchema::Ocr))
        .unwrap();
    assert_eq!(harness.service.breaker_state(), "closed");
    assert_eq!(chat_calls(&harness.stub), 4, "探测必须真正发一次");
}

#[test]
fn fixture_backend_replays_byte_identical_outputs() {
    let prompt = "judge prompt";
    let input: &[u8] = b"answer";
    let output = r#"{"judgement":"match","score_milli":5000,"reason":"正确","confidence":0.9}"#;
    let harness = harness(HarnessOptions {
        cloud_enabled: false,
        backend: BackendKind::Fixture {
            prompt,
            input,
            output,
        },
        consent: None,
        budget_limit: None,
    });
    let mut previous: Option<String> = None;
    for _ in 0..100 {
        let request = CompletionRequest::new(
            task(1),
            "fixture-judge",
            prompt,
            input.to_vec(),
            ChatRequestSpec::new("fixture-judge", vec![ChatMessage::user("judge")], 0),
        )
        .with_expected(ExpectedSchema::Judge {
            max_score: MilliScore::from_points(5).unwrap(),
        });
        let outcome = harness.service.complete(request).unwrap();
        let engine = &outcome.model_call.engine;
        assert_eq!(
            engine.backend,
            eg_domain::BackendId::fixture(),
            "夹具数据必须可识别"
        );
        let serialized = serde_json::to_string(&outcome.validated.raw()).unwrap();
        if let Some(previous) = &previous {
            assert_eq!(previous, &serialized, "同输入 100 次必须一致");
        }
        previous = Some(serialized);
    }
    assert_eq!(harness.store.len(), 100);
}

#[test]
fn fixture_miss_fails_explicitly_without_synthesizing_output() {
    let harness = harness(HarnessOptions {
        cloud_enabled: false,
        backend: BackendKind::Fixture {
            prompt: "judge prompt",
            input: b"answer",
            output: r#"{"judgement":"match"}"#,
        },
        consent: None,
        budget_limit: None,
    });
    let request = CompletionRequest::new(
        task(1),
        "fixture-judge",
        "another prompt",
        b"other input".to_vec(),
        ChatRequestSpec::new("fixture-judge", vec![ChatMessage::user("judge")], 0),
    );
    let error = harness.service.complete(request).unwrap_err();
    assert!(matches!(
        error,
        eg_inference::InferenceError::FixtureMiss { .. }
    ));
}

#[test]
fn health_reports_backend_state() {
    let local = harness(HarnessOptions {
        cloud_enabled: false,
        backend: BackendKind::Local,
        consent: None,
        budget_limit: None,
    });
    assert_eq!(
        local.service.health().backend,
        eg_domain::BackendId::local_server()
    );

    let cloud_off = harness(HarnessOptions {
        cloud_enabled: false,
        backend: BackendKind::Cloud,
        consent: None,
        budget_limit: None,
    });
    assert_eq!(
        cloud_off.service.health().state,
        eg_inference::HealthState::Unavailable
    );

    let cloud_on = harness(HarnessOptions {
        cloud_enabled: true,
        backend: BackendKind::Cloud,
        consent: None,
        budget_limit: None,
    });
    assert_eq!(
        cloud_on.service.health().state,
        eg_inference::HealthState::Ready
    );
}
