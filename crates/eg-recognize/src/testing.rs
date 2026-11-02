//! 识别侧测试替身与夹具回放适配器（`SPEC-recognize.md` §8 验收）。
//!
//! 只提供**注入端口**的内存实现与 `eg-inference` 夹具回放适配器；真实适配器
//! （平台仓储事务、sidecar 后端、资产读取）由 `app` 组合根实现。测试替身故意
//! 记录全部调用，便于断言「缓存命中零次模型调用」「修复次数上限」等验收条件。

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};

use eg_domain::{
    AssetId, BBox, EngineStamp, ExifOrientation, ModelCall, ModelRole, Page, PageId, PageQuality,
    QualityVerdict, Recognition, RecognitionId, Rotation, Segment, SegmentId, SegmentMethod,
    Sha256Hex, TaskId, Ulid,
};
use eg_inference::{FixtureBackend, FixtureStore};
use eg_platform::config::OcrConfig;

use crate::cache::MemoryRecognitionCache;
use crate::error::{RecognizeError, storage_error};
use crate::prompt::{PROMPT_SCHEMA_VERSION, render_repair_prompt};
use crate::types::{
    IdSource, ImageBackground, ImageInput, ImageSource, OcrBackend, OcrRequest, OcrResponse,
    RecognitionRepo, RepairFeedback,
};

/// 构造白色背景的测试图像输入（资产 id 由字节内容决定）。
#[must_use]
pub fn white_image(bytes: &[u8], width: u32, height: u32) -> ImageInput {
    ImageInput {
        asset: AssetId::of(bytes),
        bytes: bytes.to_vec(),
        width,
        height,
        background: ImageBackground::White,
    }
}

/// 内存图像来源（页/段 id → 图像）。
#[derive(Debug, Default)]
pub struct MemoryImageSource {
    pages: RefCell<HashMap<PageId, ImageInput>>,
    segments: RefCell<HashMap<SegmentId, ImageInput>>,
}

impl MemoryImageSource {
    /// 新建空来源。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记页图像。
    pub fn insert_page(&self, page_id: PageId, image: ImageInput) {
        self.pages.borrow_mut().insert(page_id, image);
    }

    /// 登记段图像。
    pub fn insert_segment(&self, segment_id: SegmentId, image: ImageInput) {
        self.segments.borrow_mut().insert(segment_id, image);
    }
}

impl ImageSource for MemoryImageSource {
    fn page_image(&self, page: &Page) -> Result<ImageInput, RecognizeError> {
        self.pages
            .borrow()
            .get(&page.id)
            .cloned()
            .ok_or_else(|| RecognizeError::Configuration {
                reason: format!("测试图像来源缺少页面 {} 的派生图", page.id),
            })
    }

    fn segment_image(&self, _page: &Page, segment: &Segment) -> Result<ImageInput, RecognizeError> {
        self.segments
            .borrow()
            .get(&segment.id)
            .cloned()
            .ok_or_else(|| RecognizeError::Configuration {
                reason: format!("测试图像来源缺少段 {} 的裁片", segment.id),
            })
    }
}

/// 内存识别结果仓储（记录 `Recognition` 与 `ModelCall`，可注入写入失败）。
#[derive(Debug, Default)]
pub struct MemoryRecognitionRepo {
    recognitions: RefCell<Vec<Recognition>>,
    model_calls: RefCell<Vec<ModelCall>>,
    fail_writes: Cell<bool>,
}

impl MemoryRecognitionRepo {
    /// 新建空仓储。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 已写入的识别结果。
    #[must_use]
    pub fn recognitions(&self) -> Vec<Recognition> {
        self.recognitions.borrow().clone()
    }

    /// 已写入的模型调用证据。
    #[must_use]
    pub fn model_calls(&self) -> Vec<ModelCall> {
        self.model_calls.borrow().clone()
    }

    /// 清空记录。
    pub fn clear(&self) {
        self.recognitions.borrow_mut().clear();
        self.model_calls.borrow_mut().clear();
    }

    /// 让后续写入失败（测试任务级重试路径）。
    pub fn fail_writes(&self, fail: bool) {
        self.fail_writes.set(fail);
    }
}

impl RecognitionRepo for MemoryRecognitionRepo {
    fn save_recognition(&self, recognition: &Recognition) -> Result<(), RecognizeError> {
        if self.fail_writes.get() {
            return Err(storage_error("识别结果写入失败（注入）"));
        }
        self.recognitions.borrow_mut().push(recognition.clone());
        Ok(())
    }

    fn save_model_call(&self, call: &ModelCall) -> Result<(), RecognizeError> {
        if self.fail_writes.get() {
            return Err(storage_error("模型调用写入失败（注入）"));
        }
        self.model_calls.borrow_mut().push(call.clone());
        Ok(())
    }
}

/// 确定性 id 生成器（测试用；单调 ULID）。
#[derive(Debug, Default)]
pub struct MemoryIds {
    counter: Cell<u128>,
}

impl MemoryIds {
    /// 新建生成器。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn next(&self) -> Ulid {
        let next = self.counter.get() + 1;
        self.counter.set(next);
        Ulid::from_parts(1_700_000_000_000, next)
    }
}

impl IdSource for MemoryIds {
    fn next_recognition_id(&self) -> Result<RecognitionId, RecognizeError> {
        Ok(RecognitionId::from_ulid(self.next()))
    }

    fn next_model_call_id(&self) -> Result<eg_domain::ModelCallId, RecognizeError> {
        Ok(eg_domain::ModelCallId::from_ulid(self.next()))
    }
}

/// 脚本化识别后端：按顺序回放原始输出或失败，并记录请求与修复反馈。
#[derive(Debug)]
pub struct ScriptedBackend {
    engine: EngineStamp,
    responses: RefCell<VecDeque<ScriptedResponse>>,
    /// 收到的识别请求（按调用顺序）。
    pub requests: RefCell<Vec<OcrRequest>>,
    /// 收到的修复反馈（按调用顺序）。
    pub repairs: RefCell<Vec<RepairFeedback>>,
    latency_ms: u64,
    cost_micros: u64,
}

/// 脚本项。
#[derive(Debug, Clone)]
pub enum ScriptedResponse {
    /// 原始输出。
    Output {
        /// 模型原始输出文本。
        raw: String,
        /// 是否截断。
        truncated: bool,
    },
    /// 失败（后端不可用等）。
    Failure(RecognizeError),
}

impl ScriptedBackend {
    /// 新建脚本后端（后端保留字 `fixture`，模型 id/版本由参数决定）。
    #[must_use]
    pub fn new(model_id: &str, model_version: &str) -> Self {
        Self {
            engine: EngineStamp {
                role: ModelRole::Ocr,
                backend: eg_domain::BackendId::fixture(),
                model_id: model_id.to_owned(),
                model_version: model_version.to_owned(),
                prompt_hash: None,
                params_hash: Sha256Hex::of(b"scripted-params"),
            },
            responses: RefCell::new(VecDeque::new()),
            requests: RefCell::new(Vec::new()),
            repairs: RefCell::new(Vec::new()),
            latency_ms: 5,
            cost_micros: 10,
        }
    }

    /// 追加一条原始输出。
    pub fn push_raw(&self, raw: impl Into<String>) {
        self.responses
            .borrow_mut()
            .push_back(ScriptedResponse::Output {
                raw: raw.into(),
                truncated: false,
            });
    }

    /// 追加一条被截断的输出。
    pub fn push_truncated(&self, raw: impl Into<String>) {
        self.responses
            .borrow_mut()
            .push_back(ScriptedResponse::Output {
                raw: raw.into(),
                truncated: true,
            });
    }

    /// 追加一条失败响应。
    pub fn push_failure(&self, error: RecognizeError) {
        self.responses
            .borrow_mut()
            .push_back(ScriptedResponse::Failure(error));
    }

    fn next_response(&self, request: &OcrRequest) -> Result<OcrResponse, RecognizeError> {
        let response = self.responses.borrow_mut().pop_front().ok_or_else(|| {
            RecognizeError::BackendUnavailable {
                reason: "脚本响应已耗尽（测试夹具不足）".to_owned(),
            }
        })?;
        match response {
            ScriptedResponse::Failure(error) => Err(error),
            ScriptedResponse::Output { raw, truncated } => Ok(OcrResponse {
                raw,
                engine: EngineStamp {
                    prompt_hash: Some(request.prompt_hash),
                    ..self.engine.clone()
                },
                latency_ms: self.latency_ms,
                cost_micros: self.cost_micros,
                usage: eg_inference::TokenUsage {
                    input_tokens: 100,
                    cached_input_tokens: 0,
                    output_tokens: 50,
                },
                truncated,
            }),
        }
    }
}

impl OcrBackend for ScriptedBackend {
    fn engine(&self) -> EngineStamp {
        self.engine.clone()
    }

    fn infer(&self, request: &OcrRequest) -> Result<OcrResponse, RecognizeError> {
        self.requests.borrow_mut().push(request.clone());
        self.next_response(request)
    }

    fn repair(
        &self,
        request: &OcrRequest,
        feedback: &RepairFeedback,
    ) -> Result<OcrResponse, RecognizeError> {
        self.requests.borrow_mut().push(request.clone());
        self.repairs.borrow_mut().push(feedback.clone());
        self.next_response(request)
    }
}

/// `eg-inference` 夹具回放适配器（识别角色）。
///
/// TODO(接口接缝)：真实后端适配器应调用 `inference` 的 OCR 角色并透传原始输出；
/// 本适配器仅用于离线验收（夹具命中才回放，未命中绝不合成输出）。
#[derive(Debug, Clone)]
pub struct FixtureOcrBackend {
    inner: FixtureBackend,
    model_id: String,
    model_version: String,
}

impl FixtureOcrBackend {
    /// 用夹具库构造回放后端（提示词 Schema 版本取本模块输出契约版本）。
    #[must_use]
    pub fn new(model_id: &str, model_version: &str, store: FixtureStore) -> Self {
        Self {
            inner: FixtureBackend::new(
                model_id.to_owned(),
                model_version.to_owned(),
                PROMPT_SCHEMA_VERSION,
                store,
            ),
            model_id: model_id.to_owned(),
            model_version: model_version.to_owned(),
        }
    }

    /// 夹具条数。
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    fn stamp(&self, prompt_hash: Option<Sha256Hex>) -> EngineStamp {
        EngineStamp {
            role: ModelRole::Ocr,
            backend: eg_domain::BackendId::fixture(),
            model_id: self.model_id.clone(),
            model_version: self.model_version.clone(),
            prompt_hash,
            params_hash: Sha256Hex::of(
                format!("fixture:{}:{}", self.model_id, PROMPT_SCHEMA_VERSION).as_bytes(),
            ),
        }
    }

    fn replay(&self, prompt: &str, image: &[u8]) -> Result<String, RecognizeError> {
        self.inner
            .replay(prompt, image)
            .map(|replay| replay.output_raw)
            .map_err(|error| RecognizeError::BackendUnavailable {
                reason: format!("夹具回放失败：{error}"),
            })
    }
}

impl OcrBackend for FixtureOcrBackend {
    fn engine(&self) -> EngineStamp {
        self.stamp(None)
    }

    fn infer(&self, request: &OcrRequest) -> Result<OcrResponse, RecognizeError> {
        let raw = self.replay(&request.prompt, &request.image)?;
        Ok(OcrResponse {
            raw,
            engine: self.stamp(Some(request.prompt_hash)),
            latency_ms: 0,
            cost_micros: 0,
            usage: eg_inference::TokenUsage::ZERO,
            truncated: false,
        })
    }

    fn repair(
        &self,
        request: &OcrRequest,
        feedback: &RepairFeedback,
    ) -> Result<OcrResponse, RecognizeError> {
        let repair_prompt = render_repair_prompt(&request.prompt, feedback);
        let raw = self.replay(&repair_prompt, &request.image)?;
        Ok(OcrResponse {
            raw,
            engine: self.stamp(Some(crate::prompt::prompt_hash(&repair_prompt))),
            latency_ms: 0,
            cost_micros: 0,
            usage: eg_inference::TokenUsage::ZERO,
            truncated: false,
        })
    }
}

/// 计算夹具键（与 `inference` 的 `fixture_key` 一致；测试录制夹具用）。
#[must_use]
pub fn fixture_key_for(
    model_id: &str,
    model_version: &str,
    prompt: &str,
    image: &[u8],
) -> Sha256Hex {
    eg_inference::fixture_key(
        model_id,
        model_version,
        PROMPT_SCHEMA_VERSION,
        prompt,
        image,
    )
}

/// 构造一条夹具记录（`prompt`/`image` 必须与真实调用逐字节一致）。
#[must_use]
pub fn fixture_entry(
    model_id: &str,
    model_version: &str,
    prompt: &str,
    image: &[u8],
    output_raw: &str,
) -> eg_inference::FixtureEntry {
    eg_inference::FixtureEntry {
        key: fixture_key_for(model_id, model_version, prompt, image),
        model_id: model_id.to_owned(),
        model_version: model_version.to_owned(),
        prompt_schema_version: PROMPT_SCHEMA_VERSION.to_owned(),
        output_raw: output_raw.to_owned(),
    }
}

/// 测试配置：默认 `ocr.*`，模型 id 与脚本后端一致。
#[must_use]
pub fn sample_config(model_id: &str) -> OcrConfig {
    OcrConfig {
        model: model_id.to_owned(),
        ..OcrConfig::default()
    }
}

/// 构造测试页面（含质量结论与派生资产）。
#[must_use]
pub fn sample_page(page_no: u32, width: u32, height: u32) -> Page {
    let seed = u128::from(page_no) + 1;
    Page {
        id: PageId::from_ulid(Ulid::from_parts(1_700_000_000_000, seed)),
        submission_id: eg_domain::SubmissionId::from_ulid(Ulid::from_parts(1_700_000_000_000, 100)),
        page_no,
        asset: AssetId::of(format!("page-{page_no}").as_bytes()),
        width,
        height,
        rotation_applied: Rotation::NONE,
        exif_orientation: ExifOrientation::NONE,
        quality: PageQuality {
            blur_var: 120.0,
            exposure: 1.0,
            skew_deg: 0.0,
            resolution_ok: true,
            verdict: QualityVerdict::Good,
        },
        derived: Vec::new(),
    }
}

/// 构造测试答案区域。
#[must_use]
pub fn sample_segment(page: &Page, bbox: BBox) -> Segment {
    Segment {
        id: SegmentId::from_ulid(Ulid::from_parts(1_700_000_000_000, 200)),
        page_id: page.id,
        question_id: None,
        bbox,
        crop: AssetId::of(b"segment-crop"),
        method: SegmentMethod::LayoutDetection,
    }
}

/// 构造测试任务 id。
#[must_use]
pub fn sample_task_id() -> TaskId {
    TaskId::from_ulid(Ulid::from_parts(1_700_000_000_000, 300))
}

/// 便捷组合：内存缓存 + 内存仓储 + 内存图像 + 确定性 id。
#[derive(Debug, Default)]
pub struct MemoryRecognizeIo {
    /// 图像来源。
    pub images: MemoryImageSource,
    /// 缓存。
    pub cache: MemoryRecognitionCache,
    /// 仓储。
    pub repo: MemoryRecognitionRepo,
    /// id 生成器。
    pub ids: MemoryIds,
}

impl MemoryRecognizeIo {
    /// 新建。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripted_backend_replays_and_records() {
        let backend = ScriptedBackend::new("m", "v");
        backend.push_raw("{\"schema_version\":1,\"blocks\":[]}");
        let request = OcrRequest {
            scope: crate::types::ScanScope::Page,
            prompt: "p".to_owned(),
            prompt_hash: Sha256Hex::of(b"p"),
            image: vec![1, 2, 3],
            image_width: 10,
            image_height: 10,
            timeout_ms: 1000,
            task_id: sample_task_id(),
        };
        let response = backend.infer(&request).unwrap();
        assert_eq!(response.raw, "{\"schema_version\":1,\"blocks\":[]}");
        assert_eq!(backend.requests.borrow().len(), 1);
        assert!(response.engine.prompt_hash.is_some());
    }

    #[test]
    fn fixture_entry_key_matches_fixture_key() {
        let key = fixture_key_for("m", "v", "prompt", b"image");
        let entry = fixture_entry("m", "v", "prompt", b"image", "{}");
        assert_eq!(key, entry.key);
        assert_eq!(entry.prompt_schema_version, PROMPT_SCHEMA_VERSION);
    }

    #[test]
    fn fixture_backend_miss_is_backend_unavailable() {
        let backend = FixtureOcrBackend::new("m", "v", FixtureStore::new());
        assert!(backend.is_empty());
        let request = OcrRequest {
            scope: crate::types::ScanScope::Page,
            prompt: "p".to_owned(),
            prompt_hash: Sha256Hex::of(b"p"),
            image: vec![1],
            image_width: 1,
            image_height: 1,
            timeout_ms: 1000,
            task_id: sample_task_id(),
        };
        let error = backend.infer(&request).unwrap_err();
        assert!(matches!(error, RecognizeError::BackendUnavailable { .. }));
        assert_eq!(
            error.manual_reason(),
            Some(eg_domain::ManualReason::BackendUnavailable)
        );
    }

    #[test]
    fn fixture_backend_replays_recorded_output() {
        let prompt = crate::prompt::render_page_prompt(10, 20, true);
        let image = b"image-bytes";
        let output = "{\"schema_version\":1,\"blocks\":[{\"kind\":\"text\",\"bbox\":[0.1,0.1,0.2,0.1],\"text\":\"1.\"}]}";
        let mut store = FixtureStore::new();
        store
            .insert(fixture_entry("m", "v", &prompt, image, output))
            .unwrap();
        let backend = FixtureOcrBackend::new("m", "v", store);
        let request = OcrRequest {
            scope: crate::types::ScanScope::Page,
            prompt: prompt.clone(),
            prompt_hash: crate::prompt::prompt_hash(&prompt),
            image: image.to_vec(),
            image_width: 10,
            image_height: 20,
            timeout_ms: 1000,
            task_id: sample_task_id(),
        };
        let response = backend.infer(&request).unwrap();
        assert_eq!(response.raw, output);
        assert!(!response.truncated);
    }

    #[test]
    fn memory_ids_are_monotonic() {
        let ids = MemoryIds::new();
        let first = ids.next_recognition_id().unwrap();
        let second = ids.next_recognition_id().unwrap();
        assert!(first.as_ulid() < second.as_ulid());
    }
}
