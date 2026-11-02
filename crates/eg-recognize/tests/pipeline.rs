#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::assert_is_empty
)]

//! 端到端验收（`SPEC-recognize.md` §8 R1–R8）：页级/段级流水线、缓存、修复、
//! 置信度、手写降级、夹具回放与失败映射。

use eg_domain::{BBox, BlockKind, Confidence, ManualReason, Page};
use eg_platform::config::{OcrConfig, SegmentRecognition};
use eg_platform::{CancelToken, FixedClock};
use eg_recognize::cache::MemoryRecognitionCache;
use eg_recognize::testing::{
    MemoryIds, MemoryImageSource, MemoryRecognitionRepo, ScriptedBackend, sample_config,
    sample_page, sample_segment, sample_task_id, white_image,
};
use eg_recognize::{
    OcrBackend, RecognizeError, RecognizeIo, RecognizeOutcome, SegmentContext, recognize_page,
    recognize_segment,
};
use serde_json::json;

const MODEL_ID: &str = "scripted-model";
const IMAGE_WIDTH: u32 = 1400;
const IMAGE_HEIGHT: u32 = 1800;

struct Harness {
    backend: ScriptedBackend,
    images: MemoryImageSource,
    cache: MemoryRecognitionCache,
    repo: MemoryRecognitionRepo,
    ids: MemoryIds,
    clock: FixedClock,
    cancel: CancelToken,
    page: Page,
    segment: eg_domain::Segment,
}

impl Harness {
    fn new() -> Self {
        let page = sample_page(1, 1600, 2262);
        let images = MemoryImageSource::new();
        images.insert_page(
            page.id,
            white_image(b"page-bytes", IMAGE_WIDTH, IMAGE_HEIGHT),
        );
        let segment = sample_segment(&page, BBox::new(0.1, 0.2, 0.3, 0.2).unwrap());
        images.insert_segment(segment.id, white_image(b"segment-bytes", 900, 700));
        Self {
            backend: ScriptedBackend::new(MODEL_ID, "v1"),
            images,
            cache: MemoryRecognitionCache::new(),
            repo: MemoryRecognitionRepo::new(),
            ids: MemoryIds::new(),
            clock: FixedClock::from_rfc3339("2026-11-02T08:00:00Z").unwrap(),
            cancel: CancelToken::new(),
            page,
            segment,
        }
    }

    fn io(&self) -> RecognizeIo<'_> {
        RecognizeIo {
            backend: &self.backend,
            images: &self.images,
            cache: &self.cache,
            repo: &self.repo,
            clock: &self.clock,
            ids: &self.ids,
            cancel: &self.cancel,
        }
    }

    fn config(&self) -> OcrConfig {
        sample_config(&self.backend.engine().model_id)
    }

    fn recognize_page(&self, config: &OcrConfig) -> Result<RecognizeOutcome, RecognizeError> {
        recognize_page(&self.io(), &self.page, config, sample_task_id())
    }

    fn recognize_segment(&self, config: &OcrConfig) -> Result<RecognizeOutcome, RecognizeError> {
        recognize_segment(
            &self.io(),
            &self.page,
            &self.segment,
            &SegmentContext::default(),
            config,
            sample_task_id(),
        )
    }
}

#[allow(clippy::needless_pass_by_value)]
fn raw_blocks(blocks: Vec<serde_json::Value>) -> String {
    serde_json::Value::Object(
        [
            ("schema_version".to_owned(), json!(1)),
            ("blocks".to_owned(), json!(blocks)),
        ]
        .into_iter()
        .collect(),
    )
    .to_string()
}

fn text_block(x: f64, y: f64, w: f64, h: f64, text: &str, confidence: f64) -> serde_json::Value {
    json!({
        "kind": "text",
        "bbox": [x, y, w, h],
        "text": text,
        "confidence": confidence,
    })
}

fn simple_answer() -> String {
    raw_blocks(vec![
        text_block(0.1, 0.05, 0.8, 0.08, "1. 求 x+1=2 的解。", 0.92),
        text_block(0.1, 0.20, 0.8, 0.10, "解：x=1", 0.88),
    ])
}

#[test]
fn page_recognition_happy_path_writes_evidence() {
    let harness = Harness::new();
    harness.backend.push_raw(simple_answer());
    let config = harness.config();
    let outcome = harness.recognize_page(&config).unwrap();

    assert_eq!(outcome.recognition.blocks.len(), 2);
    assert_eq!(outcome.recognition.blocks[0].text, "1. 求 x+1=2 的解。");
    assert_eq!(outcome.recognition.segment_id, None);
    assert_eq!(outcome.recognition.page_id, harness.page.id);
    assert!(outcome.recognition.text.contains("x=1"));
    assert_eq!(outcome.metrics.model_calls, 1);
    assert_eq!(outcome.metrics.repair_attempts, 0);
    assert!(!outcome.metrics.cache_hit);
    assert!(outcome.structure.first_pass);
    assert!(!outcome.structure.repaired);
    // 模型给了概率：块级用模型值并标注来源。
    assert!(
        outcome
            .recognition
            .blocks
            .iter()
            .all(|block| block.confidence.get() > 0.8)
    );
    // 页级 = 字符加权平均 × 结构因子（首次通过 1.0）。
    assert!(outcome.confidence.page.get() <= 0.95);
    assert_eq!(outcome.confidence.blocks.len(), 2);

    let recognitions = harness.repo.recognitions();
    assert_eq!(recognitions.len(), 1);
    let calls = harness.repo.model_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].role, eg_domain::ModelRole::Ocr);
    assert_eq!(calls[0].latency_ms, 5);
    assert_eq!(calls[0].cost_micros, 10);
    assert_eq!(calls[0].output["status"], "valid");
    assert_eq!(calls[0].output["usage"]["input_tokens"], 100);
    assert!(calls[0].engine.prompt_hash.is_some());
}

#[test]
fn cache_hit_calls_no_model_and_is_byte_identical() {
    let harness = Harness::new();
    harness.backend.push_raw(simple_answer());
    let config = harness.config();
    let first = harness.recognize_page(&config).unwrap();
    assert_eq!(harness.backend.requests.borrow().len(), 1);

    let second = harness.recognize_page(&config).unwrap();
    assert_eq!(
        harness.backend.requests.borrow().len(),
        1,
        "缓存命中必须零次模型调用"
    );
    assert!(second.metrics.cache_hit);
    assert_eq!(second.metrics.model_calls, 0);
    assert_eq!(second.recognition.blocks, first.recognition.blocks);
    assert_eq!(second.recognition.text, first.recognition.text);
    assert_eq!(second.recognition.engine, first.recognition.engine);
    assert_eq!(second.recognition.confidence, first.recognition.confidence);
    assert_eq!(harness.cache.hits(), 1);
    assert_eq!(harness.repo.recognitions().len(), 2, "命中也要写识别结果");
    assert_eq!(
        harness.repo.model_calls().len(),
        1,
        "命中不得新增 ModelCall"
    );
}

#[test]
fn cache_disabled_always_calls_model() {
    let harness = Harness::new();
    harness.backend.push_raw(simple_answer());
    harness.backend.push_raw(simple_answer());
    let mut config = harness.config();
    config.cache = false;
    harness.recognize_page(&config).unwrap();
    let outcome = harness.recognize_page(&config).unwrap();
    assert_eq!(harness.backend.requests.borrow().len(), 2);
    assert!(!outcome.metrics.cache_hit);
    assert!(harness.cache.is_empty());
}

#[test]
fn prompt_change_invalidates_cache() {
    let harness = Harness::new();
    harness.backend.push_raw(simple_answer());
    harness.backend.push_raw(simple_answer());
    let config = harness.config();
    harness.recognize_page(&config).unwrap();
    let mut changed = config.clone();
    changed.emit_latex = false;
    let outcome = harness.recognize_page(&changed).unwrap();
    assert_eq!(harness.backend.requests.borrow().len(), 2);
    assert!(!outcome.metrics.cache_hit);
}

#[test]
fn mechanical_repair_retries_format_only_and_records_both_calls() {
    let harness = Harness::new();
    // 第一次：JSON 语法错误（可机械修复）。
    harness
        .backend
        .push_raw("{\"schema_version\":1,\"blocks\":[");
    harness.backend.push_raw(simple_answer());
    let outcome = harness.recognize_page(&harness.config()).unwrap();
    assert_eq!(outcome.metrics.model_calls, 2);
    assert_eq!(outcome.metrics.repair_attempts, 1);
    assert!(outcome.structure.repaired);
    assert!(!outcome.structure.first_pass);
    assert_eq!(harness.backend.repairs.borrow().len(), 1);
    assert!(
        harness.backend.repairs.borrow()[0].errors[0].contains("EOF")
            || !harness.backend.repairs.borrow()[0].errors[0].is_empty()
    );
    let calls = harness.repo.model_calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].output["status"], "invalid");
    assert_eq!(calls[0].output["failure_stage"], "structure");
    assert_eq!(calls[1].output["status"], "valid");
    // 修复后通过：结构因子 0.90。
    assert!((outcome.confidence.structure_factor - 0.90).abs() < 1e-9);
}

#[test]
fn repair_budget_zero_means_single_call_and_structure_error() {
    let harness = Harness::new();
    harness
        .backend
        .push_raw("{\"schema_version\":1,\"blocks\":[");
    let mut config = harness.config();
    config.repair_attempts = 0;
    let error = harness.recognize_page(&config).unwrap_err();
    assert!(matches!(error, RecognizeError::Structure { .. }));
    assert_eq!(
        error.manual_reason(),
        Some(ManualReason::RecognitionUnavailable)
    );
    assert_eq!(harness.backend.requests.borrow().len(), 1);
    assert_eq!(harness.repo.model_calls().len(), 1);
    assert!(
        harness.repo.recognitions().is_empty(),
        "失败不得写半成品识别"
    );
}

#[test]
fn repair_budget_exhausted_is_structure_error() {
    let harness = Harness::new();
    harness
        .backend
        .push_raw("{\"schema_version\":1,\"blocks\":[");
    harness
        .backend
        .push_raw("{\"schema_version\":1,\"blocks\":[");
    let error = harness.recognize_page(&harness.config()).unwrap_err();
    assert!(matches!(
        error,
        RecognizeError::Structure { repaired: true, .. }
    ));
    assert_eq!(harness.backend.requests.borrow().len(), 2);
    assert!(harness.repo.recognitions().is_empty());
}

#[test]
fn malformed_outputs_are_rejected_or_degraded() {
    // 缺失 blocks：不可机械修复 → 结构错误。
    let harness = Harness::new();
    harness.backend.push_raw("{\"schema_version\":1}");
    let error = harness.recognize_page(&harness.config()).unwrap_err();
    assert!(matches!(error, RecognizeError::Structure { .. }));

    // 类型错误：bbox 不是四元数组。
    let harness = Harness::new();
    harness.backend.push_raw(raw_blocks(vec![json!({
        "kind": "text",
        "bbox": "0.1,0.1,0.2,0.1",
        "text": "x",
    })]));
    let error = harness.recognize_page(&harness.config()).unwrap_err();
    assert!(matches!(error, RecognizeError::Structure { .. }));

    // 重复块：语义违规（不重试）。
    let harness = Harness::new();
    let block = text_block(0.1, 0.1, 0.2, 0.1, "same", 0.9);
    harness
        .backend
        .push_raw(raw_blocks(vec![block.clone(), block]));
    let error = harness.recognize_page(&harness.config()).unwrap_err();
    assert!(matches!(error, RecognizeError::Semantic { .. }));
    assert_eq!(
        harness.backend.requests.borrow().len(),
        1,
        "语义违规不得重试"
    );

    // 坐标越界：语义违规。
    let harness = Harness::new();
    harness
        .backend
        .push_raw(raw_blocks(vec![text_block(0.9, 0.0, 0.2, 0.1, "x", 0.9)]));
    let error = harness.recognize_page(&harness.config()).unwrap_err();
    assert!(matches!(error, RecognizeError::Semantic { .. }));

    // 条码块：语义违规。
    let harness = Harness::new();
    harness.backend.push_raw(raw_blocks(vec![json!({
        "kind": "barcode",
        "bbox": [0.1, 0.1, 0.2, 0.1],
        "text": "690",
    })]));
    let error = harness.recognize_page(&harness.config()).unwrap_err();
    assert!(matches!(error, RecognizeError::Semantic { .. }));

    // 零块：空结果。
    let harness = Harness::new();
    harness.backend.push_raw(raw_blocks(Vec::new()));
    let error = harness.recognize_page(&harness.config()).unwrap_err();
    assert!(matches!(error, RecognizeError::EmptyResult));

    // 输出截断：显式失败。
    let harness = Harness::new();
    harness
        .backend
        .push_truncated("{\"schema_version\":1,\"blocks\":[");
    let error = harness.recognize_page(&harness.config()).unwrap_err();
    assert!(matches!(error, RecognizeError::Truncated));
    assert!(harness.repo.recognitions().is_empty());
}

#[test]
fn invalid_formula_degrades_block_without_fabrication() {
    let harness = Harness::new();
    harness.backend.push_raw(raw_blocks(vec![json!({
        "kind": "formula",
        "bbox": [0.1, 0.2, 0.5, 0.1],
        "text": "\\frac{1}{2",
        "latex": "\\frac{1}{2",
        "confidence": 0.99,
    })]));
    let outcome = harness.recognize_page(&harness.config()).unwrap();
    assert_eq!(outcome.structure.formula_invalid_count, 1);
    assert_eq!(
        outcome.recognition.formula_block_count(),
        0,
        "不得伪造公式块"
    );
    assert_eq!(outcome.recognition.blocks[0].kind, BlockKind::Unknown);
    assert_eq!(outcome.recognition.blocks[0].latex, None);
    assert_eq!(outcome.recognition.blocks[0].confidence, Confidence::ZERO);
    assert!(outcome.structure.has_unknown_block);
    // 未知块结构因子 0.85。
    assert!((outcome.confidence.structure_factor - 0.85).abs() < 1e-9);
}

#[test]
fn emit_latex_off_routes_formula_to_unusable_unknown() {
    let harness = Harness::new();
    harness.backend.push_raw(raw_blocks(vec![json!({
        "kind": "formula",
        "bbox": [0.1, 0.2, 0.5, 0.1],
        "text": "x+1",
        "latex": "x+1",
        "confidence": 0.99,
    })]));
    let mut config = harness.config();
    config.emit_latex = false;
    let outcome = harness.recognize_page(&config).unwrap();
    assert_eq!(outcome.structure.formula_latex_disabled_count, 1);
    assert_eq!(outcome.recognition.formula_block_count(), 0);
    assert_eq!(outcome.recognition.blocks[0].kind, BlockKind::Unknown);
    assert_eq!(outcome.recognition.blocks[0].confidence, Confidence::ZERO);
}

#[test]
fn valid_latex_is_normalized_and_kept() {
    let harness = Harness::new();
    harness.backend.push_raw(raw_blocks(vec![json!({
        "kind": "formula",
        "bbox": [0.1, 0.2, 0.5, 0.1],
        "text": "x 小于等于 1",
        "latex": "$x≤1$",
        "confidence": 0.9,
    })]));
    let outcome = harness.recognize_page(&harness.config()).unwrap();
    assert_eq!(outcome.recognition.formula_block_count(), 1);
    assert_eq!(
        outcome.recognition.blocks[0].latex.as_deref(),
        Some("x\\leq 1")
    );
}

#[test]
fn storage_failure_is_task_level_retriable_and_writes_nothing() {
    let harness = Harness::new();
    harness.backend.push_raw(simple_answer());
    harness.repo.fail_writes(true);
    let error = harness.recognize_page(&harness.config()).unwrap_err();
    assert!(matches!(error, RecognizeError::Storage { .. }));
    assert!(error.is_task_level_retriable());
    assert_eq!(error.manual_reason(), None);
    assert!(harness.repo.recognitions().is_empty());
}

#[test]
fn cancelled_before_call_writes_nothing() {
    let harness = Harness::new();
    harness.cancel.cancel();
    let error = harness.recognize_page(&harness.config()).unwrap_err();
    assert!(matches!(error, RecognizeError::Cancelled));
    assert_eq!(harness.backend.requests.borrow().len(), 0);
    assert!(harness.repo.recognitions().is_empty());
    assert_eq!(error.manual_reason(), None);
}

#[test]
fn backend_unavailable_maps_to_manual_backend_reason() {
    let harness = Harness::new();
    harness
        .backend
        .push_failure(RecognizeError::BackendUnavailable {
            reason: "sidecar 未就绪".to_owned(),
        });
    let error = harness.recognize_page(&harness.config()).unwrap_err();
    assert_eq!(
        error.manual_reason(),
        Some(ManualReason::BackendUnavailable)
    );
    assert!(harness.repo.recognitions().is_empty());
}

#[test]
fn handwriting_low_confidence_routes_to_manual() {
    let harness = Harness::new();
    harness.backend.push_raw(raw_blocks(vec![json!({
        "kind": "handwriting",
        "bbox": [0.1, 0.2, 0.5, 0.2],
        "text": "手写答案",
        "confidence": 0.4,
    })]));
    let outcome = harness.recognize_page(&harness.config()).unwrap();
    let handoff = outcome.manual.expect("手写低置信必须转人工");
    assert_eq!(handoff.reason, ManualReason::LowConfidence);
    assert_eq!(handoff.page_id, harness.page.id);
    assert_eq!(
        handoff.trigger,
        eg_recognize::HandwritingTrigger::BlockConfidence
    );
}

#[test]
fn question_pattern_plausibility_is_reported() {
    let harness = Harness::new();
    harness.backend.push_raw(raw_blocks(vec![
        text_block(0.1, 0.05, 0.8, 0.1, "1. 求值（5分）", 0.9),
        text_block(0.1, 0.25, 0.8, 0.1, "2. 计算（10 分）", 0.9),
        text_block(0.1, 0.45, 0.8, 0.1, "2. 重复题号", 0.9),
    ]));
    let outcome = harness.recognize_page(&harness.config()).unwrap();
    assert_eq!(outcome.plausibility.question_numbers, vec![1, 2, 2]);
    assert_eq!(outcome.plausibility.duplicate_numbers, vec![2]);
    assert!(outcome.plausibility.has_duplicate_numbers());
    assert_eq!(outcome.plausibility.score_annotations, 2);
}

#[test]
fn cross_page_candidates_are_marked() {
    let harness = Harness::new();
    harness.backend.push_raw(raw_blocks(vec![
        text_block(0.1, 0.0, 0.8, 0.1, "继续：x=2", 0.9),
        text_block(0.1, 0.5, 0.8, 0.1, "2. 下一题", 0.9),
        text_block(0.1, 0.88, 0.8, 0.08, "y=3", 0.9),
    ]));
    let outcome = harness.recognize_page(&harness.config()).unwrap();
    assert!(outcome.cross_page.starts_mid_question);
    assert!(outcome.cross_page.ends_at_bottom);
}

#[test]
fn segment_recognition_writes_segment_scoped_result() {
    let harness = Harness::new();
    harness.backend.push_raw(raw_blocks(vec![json!({
        "kind": "text",
        "bbox": [0.1, 0.1, 0.6, 0.2],
        "text": "分步作答",
        "confidence": 0.9,
    })]));
    let outcome = harness.recognize_segment(&harness.config()).unwrap();
    assert_eq!(outcome.recognition.segment_id, Some(harness.segment.id));
    assert_eq!(outcome.metrics.scope, eg_recognize::ScanScope::Segment);
    assert!(outcome.metrics.model_calls >= 1);
}

#[test]
fn segment_failure_does_not_touch_page_result() {
    let harness = Harness::new();
    // 页级成功。
    harness.backend.push_raw(simple_answer());
    harness.recognize_page(&harness.config()).unwrap();
    // 段级失败。
    harness.backend.push_raw("{}");
    let mut config = harness.config();
    config.repair_attempts = 0;
    let error = harness.recognize_segment(&config).unwrap_err();
    assert!(matches!(error, RecognizeError::Structure { .. }));
    let recognitions = harness.repo.recognitions();
    assert_eq!(recognitions.len(), 1, "段级失败不得推翻页级结果");
    assert!(recognitions[0].segment_id.is_none());
}

#[test]
fn segment_decision_respects_off_and_on_demand() {
    let harness = Harness::new();
    harness.backend.push_raw(simple_answer());
    let page_outcome = harness.recognize_page(&harness.config()).unwrap();
    let mut off = harness.config();
    off.segment_recognition = SegmentRecognition::Off;
    let decision = eg_recognize::segment_decision(
        &harness.page,
        &harness.segment,
        &page_outcome.recognition,
        &off,
    );
    assert!(!decision.should_recognize);
    let on_demand = eg_recognize::segment_decision(
        &harness.page,
        &harness.segment,
        &page_outcome.recognition,
        &harness.config(),
    );
    // 低置信块落在段内 → 触发（本夹具的页级块置信度 0.92/0.88 不低，靠裁片尺寸判断）。
    assert!(on_demand.should_recognize);
    assert!(
        on_demand
            .triggers
            .contains(&eg_recognize::SegmentTrigger::SmallCrop)
    );
}

#[test]
fn fixture_replay_drives_full_pipeline() {
    use eg_inference::{FixtureStore, fixture_key};

    use eg_recognize::prompt::{PROMPT_SCHEMA_VERSION, render_page_prompt};
    use eg_recognize::testing::{FixtureOcrBackend, fixture_entry};

    let prompt = render_page_prompt(IMAGE_WIDTH, IMAGE_HEIGHT, true);
    let image_bytes = b"page-bytes";
    let output = simple_answer();
    let mut store = FixtureStore::new();
    store
        .insert(fixture_entry(MODEL_ID, "v1", &prompt, image_bytes, &output))
        .unwrap();
    assert_eq!(
        fixture_key(MODEL_ID, "v1", PROMPT_SCHEMA_VERSION, &prompt, image_bytes),
        fixture_entry(MODEL_ID, "v1", &prompt, image_bytes, &output).key
    );
    let backend = FixtureOcrBackend::new(MODEL_ID, "v1", store);
    let harness = Harness::new();
    let io = RecognizeIo {
        backend: &backend,
        ..harness.io()
    };
    let outcome = recognize_page(&io, &harness.page, &harness.config(), sample_task_id()).unwrap();
    assert_eq!(outcome.recognition.blocks.len(), 2);
    assert_eq!(
        outcome.recognition.engine.backend,
        eg_domain::BackendId::fixture()
    );
    assert_eq!(outcome.metrics.model_calls, 1);
}

#[test]
fn config_errors_fail_closed() {
    let harness = Harness::new();
    let mut wrong_model = harness.config();
    wrong_model.model = "other-model".to_owned();
    let error = harness.recognize_page(&wrong_model).unwrap_err();
    assert!(matches!(error, RecognizeError::Configuration { .. }));

    let mut bad_repair = harness.config();
    bad_repair.repair_attempts = 3;
    assert!(matches!(
        harness.recognize_page(&bad_repair).unwrap_err(),
        RecognizeError::Configuration { .. }
    ));

    let mut bad_timeout = harness.config();
    bad_timeout.page_timeout_s = 5;
    assert!(matches!(
        harness.recognize_page(&bad_timeout).unwrap_err(),
        RecognizeError::Configuration { .. }
    ));
}

#[test]
fn oversized_image_input_fails_closed() {
    let harness = Harness::new();
    harness
        .images
        .insert_page(harness.page.id, white_image(b"huge-page", 3001, 1200));
    let error = harness.recognize_page(&harness.config()).unwrap_err();
    assert!(matches!(error, RecognizeError::Configuration { .. }));
    assert!(harness.backend.requests.borrow().is_empty());
}
