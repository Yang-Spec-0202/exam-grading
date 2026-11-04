//! 仓库级夹具（`fixtures/ocr/**`、`fixtures/judge/**`）的接线集成测试。
//!
//! 目的：证明外部夹具目录能被 `FixtureCatalog` 装载，并按
//! 「精确字节键 → 组合键 → 语义标识」三级匹配回放；同时覆盖畸形样例拒绝与
//! 「跨注册表模型 id」的 `InferenceService` 端到端。
//!
//! 全部使用仓库内已提交的合成夹具：不联网、不加载真实模型、不写入仓库。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;

use eg_domain::{BackendId, MilliScore, TaskId, Ulid};
use eg_inference::testing::{CounterIds, RecordingAudit, RecordingStore, TestClock};
use eg_inference::{
    Backend, BreakerConfig, BudgetLedger, CallRecorder, ChatMessage, ChatRequestSpec,
    CircuitBreaker, CompletionRequest, ExpectedSchema, FixtureBackend, FixtureCatalog,
    FixtureLookup, FixtureRole, InferenceService, JudgeOutput, MemoryConsentGate, ModelRegistry,
    OcrOutput, PageIdentity, QuestionIdentity, ServiceConfig, ValidatedOutput, extract_json_object,
};

fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn task(seed: u128) -> TaskId {
    TaskId::from_ulid(Ulid::from_u128(seed))
}

fn catalog() -> FixtureCatalog {
    FixtureCatalog::load(&fixtures_root()).expect("仓库夹具必须能通过完整性校验")
}

#[test]
fn loads_repo_fixtures_and_replays_every_record_byte_identically() {
    let catalog = catalog();
    assert_eq!(
        catalog.roles(),
        vec![FixtureRole::Ocr, FixtureRole::Judge],
        "ocr 与 judge 两类夹具都必须装载"
    );
    assert!(
        catalog.by_role(FixtureRole::Ocr).len() >= 6,
        "至少 3 名学生 × 2 页"
    );
    assert!(catalog.by_role(FixtureRole::Judge).len() >= 6);
    assert!(
        catalog.negatives().len() >= 3,
        "ocr 2 个 + judge 1 个负向样例"
    );

    for record in catalog.records() {
        // ① 精确字节键：输出逐字节一致
        let exact = catalog
            .exact(record.entry.key)
            .expect("每条记录都必须能精确回放");
        assert_eq!(exact.key, record.entry.key);
        assert_eq!(
            exact.output_raw.as_bytes(),
            record.entry.output_raw.as_bytes(),
            "回放必须逐字节一致：{}",
            record.source.display()
        );
        // ② 组合键（不限定模型身份）：同一提示词 + 同一输入资产
        let combo = catalog
            .resolve(
                &FixtureLookup::new()
                    .role(record.role)
                    .hashes(record.prompt_sha256, record.asset_sha256),
            )
            .expect("组合键必须命中");
        assert_eq!(combo.output_raw, record.entry.output_raw);
        // 记录声明的模型身份必须与索引 backend 一致（装载期已校验，这里做二次断言）
        let ids = catalog
            .backend_ids(record.role)
            .expect("角色元数据必须存在");
        assert_eq!(record.entry.model_id, ids.model_id);
        assert_eq!(record.entry.model_version, ids.model_version);
    }
}

#[test]
fn semantic_ids_resolve_ocr_pages_and_judge_questions() {
    let catalog = catalog();
    let ocr = catalog.by_role(FixtureRole::Ocr);

    // OCR：学生号 + 页码
    let by_student = ocr
        .iter()
        .find(|record| {
            record
                .page
                .as_ref()
                .is_some_and(|page| page.student_no.is_some())
        })
        .expect("至少有一条带学生号的 OCR 记录");
    let page = by_student.page.as_ref().unwrap();
    let replayed = catalog
        .by_page(&PageIdentity::by_student(
            page.student_no.clone().unwrap(),
            page.page_no,
        ))
        .expect("学生号 + 页码必须命中");
    assert_eq!(replayed.output_raw, by_student.entry.output_raw);

    // OCR：文件 stem + 页码（相机文件名等身份未决页）
    let by_stem = ocr
        .iter()
        .find(|record| {
            record
                .page
                .as_ref()
                .is_some_and(|page| page.student_no.is_none() && page.submission_stem.is_some())
        })
        .expect("至少有一条仅按 stem 标识的 OCR 记录");
    let stem_page = by_stem.page.as_ref().unwrap();
    let replayed = catalog
        .by_page(&PageIdentity::by_stem(
            stem_page.submission_stem.clone().unwrap(),
            stem_page.page_no,
        ))
        .expect("文件 stem + 页码必须命中");
    assert_eq!(replayed.output_raw, by_stem.entry.output_raw);

    // judge：学生号 + 题号
    let judge = catalog.by_role(FixtureRole::Judge);
    let by_question = judge
        .iter()
        .find(|record| record.question.is_some())
        .expect("至少有一条带题号的 judge 记录");
    let question = by_question.question.as_ref().unwrap();
    let replayed = catalog
        .by_question(&QuestionIdentity::new(
            question.student_no.clone(),
            question.question_ordinal,
        ))
        .expect("学生号 + 题号必须命中");
    assert_eq!(replayed.output_raw, by_question.entry.output_raw);

    // 未命中显式失败
    assert!(
        catalog
            .by_page(&PageIdentity::by_student("SO2026-404", 9))
            .is_err()
    );
    assert!(
        catalog
            .by_question(&QuestionIdentity::new("SO2026-404", 99))
            .is_err()
    );
    assert!(
        catalog.resolve(&FixtureLookup::new()).is_err(),
        "空匹配请求必须显式失败，不猜"
    );
}

#[test]
fn negatives_are_rejected_by_schema_validators_and_never_replayed() {
    let catalog = catalog();
    let mut checked = 0_u32;
    for negative in catalog.negatives() {
        // negative 的输出是合法 JSON 对象（夹具文件格式合法），但内容必须被 Schema 拒绝。
        let value = negative
            .entry
            .output_value()
            .expect("negative 仍是合法 JSON 对象");
        let rejected = match negative.role {
            FixtureRole::Ocr => OcrOutput::validate(&value).is_err(),
            FixtureRole::Judge => {
                JudgeOutput::validate(&value, MilliScore::from_milli(15000).unwrap()).is_err()
            }
        };
        assert!(
            rejected,
            "negative 样例必须被 Schema 拒绝（{}：{}）",
            negative.id, negative.note
        );
        // 不进回放索引
        assert!(catalog.exact(negative.entry.key).is_err());
        checked += 1;
    }
    assert!(checked >= 3);
}

#[test]
fn fixture_backend_serves_repo_fixtures_through_inference_service() {
    let catalog = catalog();
    let judge = catalog.by_role(FixtureRole::Judge);
    let record = judge
        .iter()
        .find(|record| record.case.as_deref() == Some("normal"))
        .expect("至少一条 normal judge 夹具");
    let prompt = std::fs::read_to_string(&record.prompt_source).unwrap();
    let input = std::fs::read(&record.asset_source).unwrap();
    let input_value: serde_json::Value = serde_json::from_slice(&input).unwrap();
    let max_milli = i64::try_from(
        input_value["max_score_milli"]
            .as_u64()
            .expect("judge 输入必须带 max_score_milli"),
    )
    .unwrap();
    let max_score = MilliScore::from_milli(max_milli).unwrap();

    // 调用方注册表模型 id 与夹具录制 id 不同 → 精确键 miss，组合键回填。
    let registry = Arc::new(
        ModelRegistry::parse_str(
            r#"
schema_version = 1

[[models]]
id = "caller-judge"
version = "9.9"
role = "judge"
backend = "fixture"
quantization = "fixture"
context_length = 8192
license = "MIT"
max_concurrency = 1
timeout_secs = 20
prompt_schema_version = "judge.outcome.v1"
"#,
        )
        .unwrap(),
    );
    let store = catalog.fixture_store(FixtureRole::Judge).unwrap();
    let backend = FixtureBackend::new("caller-judge", "9.9", "judge.outcome.v1", store)
        .with_catalog(Arc::new(catalog.clone()), FixtureRole::Judge);

    let clock = Arc::new(TestClock::at_ms(0));
    let calls = Arc::new(RecordingStore::default());
    let audit = Arc::new(RecordingAudit::default());
    let service = InferenceService::new(
        registry,
        Backend::Fixture(Arc::new(backend)),
        Arc::new(CircuitBreaker::with_config(
            clock.clone(),
            BreakerConfig::default(),
        )),
        Arc::new(BudgetLedger::new(clock.clone())),
        Arc::new(MemoryConsentGate::new()),
        CallRecorder::new(
            calls.clone(),
            audit.clone(),
            clock.clone(),
            Arc::new(CounterIds::default()),
        ),
        ServiceConfig::default(),
        clock,
    );
    let request = CompletionRequest::new(
        task(7),
        "caller-judge",
        prompt,
        input,
        ChatRequestSpec::new("caller-judge", vec![ChatMessage::user("判定")], 256),
    )
    .with_expected(ExpectedSchema::Judge { max_score });
    let outcome = service
        .complete(request)
        .expect("真实夹具必须端到端回放成功");
    assert_eq!(
        outcome.model_call.engine.backend,
        BackendId::fixture(),
        "夹具数据必须可识别"
    );
    let ValidatedOutput::Judge(judge_output) = &outcome.validated else {
        panic!("期望 Judge 产出");
    };
    assert_ne!(judge_output.reason.trim(), "");
    assert_eq!(calls.len(), 1, "每次调用必须写 ModelCall");
}

#[test]
fn ocr_fixture_output_is_a_json_object_with_blocks() {
    let catalog = catalog();
    let ocr = catalog.by_role(FixtureRole::Ocr);
    let record = ocr.first().expect("至少一条 OCR 夹具");
    let replay = catalog
        .by_hashes(FixtureRole::Ocr, record.prompt_sha256, record.asset_sha256)
        .expect("组合键必须命中");
    let value = extract_json_object(&replay.output_raw).expect("OCR 夹具输出必须是单个 JSON 对象");
    assert!(
        value
            .get("blocks")
            .and_then(serde_json::Value::as_array)
            .is_some(),
        "OCR 夹具输出必须带 blocks 数组"
    );
    // 已知跨模块偏差（inference.md §7-R11 / recognize.md）：仓库夹具是
    // `SPEC-recognize` 形状（`schema_version`/`page`/`blocks[bbox,latex]`），当前
    // `OcrOutput` 只认 `{blocks}`，因此结构校验必然拒绝。两边 Schema 对齐后删除此断言，
    // 改为正向校验并把 R11 标记为已消解。
    assert!(
        OcrOutput::validate(&value).is_err(),
        "若此断言失败说明 OcrOutput 已与仓库夹具对齐，请同步更新 inference.md §7-R11"
    );
}
