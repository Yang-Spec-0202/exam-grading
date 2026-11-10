//! T30 真实适配器：rubric / inference / platform（数据库+审计+id+时钟）。

#![allow(
    unused_imports,
    clippy::vec_init_then_push,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

mod common;

use common::*;
use std::sync::Arc;

use eg_domain::{
    ComparatorTrace, GradeRecord, MilliScore, ModelRole, NormalizedAnswer, QuestionKind, Sha256Hex,
    TaskId, Ulid,
};
use eg_grade::adapters::inference::{
    InferenceClassifier, InferenceJudge, JUDGE_PROMPT_SCHEMA_VERSION, ROUTER_PROMPT_SCHEMA_VERSION,
    build_judge_prompt, build_router_prompt,
};
use eg_grade::adapters::platform::{PlatformClock, PlatformGradeStore, PlatformIdGenerator};
use eg_grade::adapters::rubric::RubricStoreSource;
use eg_grade::ports::{
    ClassifierBackend, IdGenerator, JudgeBackend, JudgeRequest, JudgeVerdict, RubricAvailability,
    RubricSource,
};
use eg_grade::submission::GradeStore;
use eg_inference::testing::{CounterIds, RecordingAudit, RecordingStore, StubTransport, TestClock};
use eg_inference::{
    Backend, BreakerConfig, BudgetLedger, CallRecorder, ChatMessage, ChatRequestSpec,
    CircuitBreaker, CompletionRequest, ExpectedSchema, FixtureBackend, FixtureEntry, FixtureStore,
    InferenceService, MemoryConsentGate, ModelRegistry, ServiceConfig, fixture_key,
};
use eg_platform::FixedClock as PlatformFixedClock;
use eg_platform::audit::AuditLog;
use eg_platform::db::Database;
use eg_platform::ids::UlidFactory;
use eg_rubric::memory::InMemoryRubricStore;
use eg_rubric::ports::RubricStore;

const REGISTRY: &str = r#"schema_version = 1

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

[[models]]
id = "fixture-router"
version = "1"
role = "router"
backend = "fixture"
quantization = "fixture"
context_length = 4096
license = "MIT"
max_concurrency = 1
timeout_secs = 20
prompt_schema_version = "router.kind.v1"
"#;

struct InferenceHarness {
    service: Arc<InferenceService>,
    store: Arc<RecordingStore>,
}

fn inference_service(
    model_id: &str,
    model_version: &str,
    schema_version: &str,
    prompt: &str,
    input: &[u8],
    output: &str,
) -> InferenceHarness {
    let registry = Arc::new(ModelRegistry::parse_str(REGISTRY).expect("注册表合法"));
    let mut fixture_store = FixtureStore::new();
    fixture_store
        .insert(FixtureEntry {
            key: fixture_key(model_id, model_version, schema_version, prompt, input),
            model_id: model_id.to_owned(),
            model_version: model_version.to_owned(),
            prompt_schema_version: schema_version.to_owned(),
            output_raw: output.to_owned(),
        })
        .unwrap();
    let clock = Arc::new(TestClock::at_ms(0));
    let store = Arc::new(RecordingStore::default());
    let audit = Arc::new(RecordingAudit::default());
    let recorder = CallRecorder::new(
        store.clone(),
        audit,
        clock.clone(),
        Arc::new(CounterIds::default()),
    );
    let backend = Backend::Fixture(Arc::new(FixtureBackend::new(
        model_id,
        model_version,
        schema_version,
        fixture_store,
    )));
    let service = InferenceService::new(
        registry,
        backend,
        Arc::new(CircuitBreaker::with_config(
            clock.clone(),
            BreakerConfig {
                failure_threshold: 3,
                open_duration_ms: 60_000,
            },
        )),
        Arc::new(BudgetLedger::new(clock.clone())),
        Arc::new(MemoryConsentGate::new()),
        recorder,
        ServiceConfig {
            cloud_enabled: false,
            pricing_tier: eg_inference::PricingTier::Peak,
            default_timeout_ms: 5_000,
        },
        clock,
    );
    InferenceHarness {
        service: Arc::new(service),
        store,
    }
}

#[test]
fn rubric_store_source_returns_confirmed_and_version() {
    let store = Arc::new(InMemoryRubricStore::default());
    let rubric = additive_rubric();
    let question = question_id(501);
    store
        .save_active_rubric(question, &rubric, 0)
        .expect("保存生效规则");
    let source = RubricStoreSource::new(store);
    match source.availability(question) {
        RubricAvailability::Confirmed(active) => assert_eq!(active, rubric),
        other => panic!("应为已确认规则：{other:?}"),
    }
    assert_eq!(source.active_version(question), 1);
    assert_eq!(source.identity(), "rubric-store-v1");
    // 无规则题目 → Missing
    assert_eq!(
        source.availability(question_id(502)),
        RubricAvailability::Missing
    );
}

fn judge_request<'a>(
    question: &'a eg_domain::Question,
    answer: &'a eg_domain::RawAnswer,
    normalized: Option<&'a NormalizedAnswer>,
) -> JudgeRequest<'a> {
    JudgeRequest {
        submission_id: submission_id(1),
        task_id: TaskId::from_ulid(Ulid::from_u128(77)),
        question,
        key: question.key.concrete(),
        rubric: question.rubric.as_ref(),
        answer,
        normalized,
        recognition_confidence: confidence(0.95),
        mode: eg_domain::GradeMode::Standard,
        attempt: 0,
        repair_hint: None,
        granularity: MilliScore::from_milli(100).unwrap(),
        max_reason_chars: 500,
        judge_timeout_s: 20,
    }
}

#[test]
fn inference_judge_adapter_maps_fixture_output_and_records_model_call() {
    let question = question(
        503,
        QuestionKind::NumericOnly,
        5000,
        number_key("2.5"),
        None,
    );
    let answer = raw("2.5");
    let request = judge_request(&question, &answer, None);
    let (_, prompt, input) = build_judge_prompt(&request);
    let output = r#"{"judgement":"match","score_milli":5000,"reason":"数值一致","confidence":0.95,"matched_alternative":null,"calibration_id":"cal-v1","satisfied":[],"missing":[]}"#;
    let harness = inference_service(
        "fixture-judge",
        "1",
        JUDGE_PROMPT_SCHEMA_VERSION,
        &prompt,
        &input,
        output,
    );
    let judge = InferenceJudge::new(harness.service.clone(), "fixture-judge", 256).unwrap();
    let response = judge.judge(&request).expect("夹具判定应成功");
    assert_eq!(response.verdict, JudgeVerdict::Match);
    assert_eq!(response.awarded.map(MilliScore::as_milli), Some(5000));
    // 无可信校准表：模型自报 id 必须清除（uncalibrated），不得静默信任。
    assert!(response.calibration_id.is_none());
    assert!(!response.calibration_verified);
    assert!(response.calibrated_confidence.is_none());
    let call = response.model_call.expect("真实推理应返回已落库调用");
    assert_eq!(call.engine.backend, eg_domain::BackendId::fixture());
    assert_eq!(harness.store.len(), 1, "推理服务已落库 ModelCall");
    assert!(judge.identity().contains("inference-judge-v1"));
}

#[test]
fn inference_classifier_adapter_maps_router_output() {
    let question = question(
        504,
        QuestionKind::NumericOnly,
        5000,
        number_key("2.5"),
        None,
    );
    let (_, prompt, input) = build_router_prompt(&question);
    let output = r#"{"decision":"numeric_only","confidence":0.95}"#;
    let harness = inference_service(
        "fixture-router",
        "1",
        ROUTER_PROMPT_SCHEMA_VERSION,
        &prompt,
        &input,
        output,
    );
    let classifier =
        InferenceClassifier::new(harness.service.clone(), "fixture-router", 64).unwrap();
    let response = classifier
        .classify(&question, TaskId::from_ulid(Ulid::from_u128(78)))
        .expect("夹具分类应成功");
    assert_eq!(response.kind, QuestionKind::NumericOnly);
    assert!(response.model_call.is_some());
    assert_eq!(harness.store.len(), 1);
}

fn sample_record() -> GradeRecord {
    let max = MilliScore::from_milli(5000).unwrap();
    let awarded = MilliScore::from_milli(2000).unwrap();
    GradeRecord {
        id: eg_domain::GradeId::from_ulid(Ulid::from_u128(700)),
        submission_id: submission_id(1),
        question_id: question_id(1),
        revision: 1,
        mode: eg_domain::GradeMode::Standard,
        route: eg_domain::RouteDecision::RuleThenModel,
        route_trace: eg_domain::RouteTrace {
            rule_id: "R7".to_owned(),
            inputs_hash: Sha256Hex::of(b"inputs"),
            classifier_call: None,
            decided_at: ts(),
        },
        judgement: eg_domain::Judgement::Partial {
            score: awarded,
            satisfied: Vec::new(),
            missing: Vec::new(),
            reason: "部分".to_owned(),
        },
        awarded,
        max_score: max,
        confidence: confidence(0.8),
        status: eg_domain::GradeStatus::NeedsReview,
        evidence: vec![eg_domain::EvidenceRef::Asset(eg_domain::AssetId::of(
            b"page",
        ))],
        engines: Vec::new(),
        created_at: ts(),
        reviewed_at: None,
        review_note: None,
        sampled: false,
    }
}

#[test]
fn platform_grade_store_persists_record_traces_calls_and_audit() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(Database::open(&dir.path().join("grade.db")).unwrap());
    let clock: eg_platform::SharedClock = Arc::new(PlatformFixedClock::new(ts()));
    let ids = Arc::new(UlidFactory::new(clock.clone()));
    let audit = Arc::new(AuditLog::new(db.clone(), clock.clone(), ids.clone()));
    let store = PlatformGradeStore::new(db.clone(), audit.clone());
    let record = sample_record();
    // 满足 grade_record 外键：写入最小学情/试卷/题目/提交行
    let question_text = record.question_id.to_string();
    let submission_text = record.submission_id.to_string();
    db.write(move |tx| {
        let created = "2026-11-02T00:00:00Z";
        tx.execute(
            "INSERT INTO classroom(id, name, term, body, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            ("classroom-1", "班级", "2026-1", "{}", created),
        ).map_err(eg_platform::error::PlatformError::from)?;
        tx.execute(
            "INSERT INTO paper(id, classroom_id, title, subject, total_score, key_version, rubric_version, body, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            ("paper-1", "classroom-1", "试卷", "math", 5000_i64, 1_i64, 1_i64, "{}", created),
        ).map_err(eg_platform::error::PlatformError::from)?;
        tx.execute(
            "INSERT INTO question(id, paper_id, ordinal, label, kind, max_score, body) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            (
                question_text.clone(),
                "paper-1",
                1_i64,
                "1",
                "\"numeric_only\"",
                5000_i64,
                "{}",
            ),
        ).map_err(eg_platform::error::PlatformError::from)?;
        tx.execute(
            "INSERT INTO submission(id, paper_id, classroom_id, source_file_name, source_stem, status, body, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            (
                submission_text.clone(),
                "paper-1",
                "classroom-1",
                "scan.pdf",
                "scan",
                "\"imported\"",
                "{}",
                created,
            ),
        ).map_err(eg_platform::error::PlatformError::from)?;
        Ok(())
    }).expect("写入外键行");
    let _ = &record;
    let trace = ComparatorTrace {
        id: eg_domain::ComparatorTraceId::from_ulid(Ulid::from_u128(900)),
        comparator: eg_domain::ComparatorId::new("num_abs").unwrap(),
        verdict: eg_domain::ComparatorVerdict::Equal,
        detail: serde_json::json!({"expected": "2.5"}),
    };
    let call = eg_domain::ModelCall {
        id: eg_domain::ModelCallId::from_ulid(Ulid::from_u128(901)),
        role: ModelRole::Judge,
        engine: engine(ModelRole::Judge),
        task_id: TaskId::from_ulid(Ulid::from_u128(77)),
        consent_id: None,
        input_hash: Sha256Hex::of(b"input"),
        output: serde_json::json!({"ok": true}),
        latency_ms: 10,
        cost_micros: 0,
        created_at: ts(),
    };
    store
        .persist_question(&record, &[trace], &[call])
        .expect("落库成功");
    let stored: Vec<GradeRecord> = db
        .read(|conn| eg_platform::repo::list_grade_records(conn, record.submission_id))
        .unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].id, record.id);
    let audits = audit
        .list(&eg_platform::AuditQuery {
            entity_type: Some("grade_record".to_owned()),
            limit: 10,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].action, "grade.record");
    // 幂等：同记录重复落库不重复
    store
        .persist_question(&stored[0], &[], &[])
        .expect("重复落库应幂等");
    let again = db
        .read(|conn| eg_platform::repo::list_grade_records(conn, record.submission_id))
        .unwrap();
    assert_eq!(again.len(), 1);
    // id/时钟适配
    let generator = PlatformIdGenerator::new(ids);
    let ulid = generator.try_next_ulid().unwrap();
    assert!(!ulid.is_nil());
    let platform_clock = PlatformClock::new(clock);
    assert_eq!(eg_domain::Clock::now(&platform_clock), ts());
}
