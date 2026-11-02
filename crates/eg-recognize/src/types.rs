//! 识别 IO 端口与调用数据结构（`SPEC-recognize.md` §2.1、§2.4）。
//!
//! 这些是模块自有类型：`recognize` 不感知 HTTP、进程、SQL，只依赖注入的能力。
//! 并行实现期间的接缝：`eg-inference` 的 OCR 输出契约（块/文本/置信度/引擎指纹）
//! 与 `eg-capture` 的白底派生图尚未定稿，因此这里给出识别侧**最小端口**，
//! 真实适配器与替换点见 [`OcrBackend`] 与 `testing` 模块的 TODO。

use eg_domain::{AssetId, EngineStamp, ModelCall, Page, Recognition, Segment, Sha256Hex, TaskId};
use eg_inference::TokenUsage;
use eg_platform::CancelToken;

use crate::error::{RecognizeError, storage_error};

/// 识别作用域（页级/段级）。缓存键与提示词按作用域区分。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScanScope {
    /// 页级识别（整页透视矫正图）。
    Page,
    /// 段级识别（`Segment.crop` 裁片）。
    Segment,
}

impl ScanScope {
    /// 稳定文本（写入缓存键、日志与 `ModelCall` 输出标注）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Page => "page",
            Self::Segment => "segment",
        }
    }
}

/// 送入模型的图像背景状态。
///
/// `SPEC-capture.md` 要求派生出**白底**图（PDF 透明背景必须合成白底）；识别侧对
/// 未验证白底的输入 fail-closed 拒绝，避免把透明底当黑底整页识别后伪造证据。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageBackground {
    /// 已确保白底（合成或原图即白底）。
    White,
    /// 未验证/非白底：识别拒绝执行（配置或流水线错误）。
    Unverified,
}

/// 一次模型输入的图像。
#[derive(Debug, Clone, PartialEq)]
pub struct ImageInput {
    /// 实际送入模型的字节对应的内容寻址资产 id（缓存键的输入部分）。
    pub asset: AssetId,
    /// 编码后的图像字节（PNG/JPEG 等，视后端而定；由适配器保证可直接送模型）。
    pub bytes: Vec<u8>,
    /// 像素宽度（长边降采样前/后均按实际字节填写）。
    pub width: u32,
    /// 像素高度。
    pub height: u32,
    /// 背景是否已确认为白底。
    pub background: ImageBackground,
}

/// 图像来源端口：选页/段图像并读取字节。
///
/// TODO(接口接缝)：`eg-platform::AssetStore` + `eg-capture` 的派生图落地后，由
/// `app` 组合根实现「取 `Page.derived` 的透视矫正图 / `Segment.crop`」并合成白底；
/// 本模块不直接读文件系统。
pub trait ImageSource {
    /// 页级图像（`Page.derived` 的透视矫正图）。
    ///
    /// # Errors
    ///
    /// 资产缺失/解码失败/未白底时返回 [`RecognizeError::Configuration`]。
    fn page_image(&self, page: &Page) -> Result<ImageInput, RecognizeError>;

    /// 段级图像（`Segment.crop`）。
    ///
    /// # Errors
    ///
    /// 资产缺失/解码失败/未白底时返回 [`RecognizeError::Configuration`]。
    fn segment_image(&self, page: &Page, segment: &Segment) -> Result<ImageInput, RecognizeError>;
}

/// 一次识别请求（已渲染提示词 + 图像 + 预算参数）。
#[derive(Debug, Clone, PartialEq)]
pub struct OcrRequest {
    /// 页级还是段级。
    pub scope: ScanScope,
    /// 已渲染的提示词（含 Schema 版本与输出约束）。
    pub prompt: String,
    /// 提示词哈希（模板字节 + 渲染参数）。
    pub prompt_hash: Sha256Hex,
    /// 图像字节。
    pub image: Vec<u8>,
    /// 图像宽度（像素）。
    pub image_width: u32,
    /// 图像高度（像素）。
    pub image_height: u32,
    /// 单次调用超时（毫秒，来自 `ocr.page_timeout_s`；段级同值）。
    pub timeout_ms: u64,
    /// 所属持久任务 id（按任务鉴权与审计）。
    pub task_id: TaskId,
}

/// 一次识别响应（未经本模块校验的原始输出 + 引擎指纹 + 计量）。
#[derive(Debug, Clone, PartialEq)]
pub struct OcrResponse {
    /// 模型原始输出文本（逐字节，不做任何修补）。
    pub raw: String,
    /// 引擎指纹（含本次提示词哈希；`inference` 负责填后端/模型/参数哈希）。
    pub engine: EngineStamp,
    /// 模型调用延迟（毫秒）。
    pub latency_ms: u64,
    /// 费用（百万分之一货币单位；本地后端为 0）。
    pub cost_micros: u64,
    /// token 用量（本地后端可全 0）。
    pub usage: TokenUsage,
    /// 是否因输出上限被截断（截断即显式失败，不得当成功解析）。
    pub truncated: bool,
}

/// 机械修复反馈（回填给模型的校验错误摘要，不含任何识别内容改写）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairFeedback {
    /// 上一次的原始输出（逐字节回填，供模型只改格式）。
    pub previous_raw: String,
    /// 结构校验错误摘要（不允许携带「请补全内容」一类指令）。
    pub errors: Vec<String>,
}

/// 识别后端端口（识别角色；只经 `eg-inference`，不感知端口与进程）。
///
/// TODO(接口接缝)：`eg-inference` 稳定后由 `app` 组合根实现本端口——调用其 OCR
/// 角色、传入已渲染提示词与提示词哈希、取回原始输出与引擎指纹；本模块负责解析与
/// 业务语义校验。`engine()` 只返回不含提示词哈希的基础指纹（后端/模型/参数哈希），
/// 用于缓存键；每次响应必须回带含本次提示词哈希的完整指纹。
pub trait OcrBackend {
    /// 后端基础引擎指纹（`prompt_hash` 为 `None`）。
    fn engine(&self) -> EngineStamp;

    /// 发起一次识别调用。
    ///
    /// # Errors
    ///
    /// 后端不可用/超时/熔断时返回 [`RecognizeError::BackendUnavailable`]。
    fn infer(&self, request: &OcrRequest) -> Result<OcrResponse, RecognizeError>;

    /// 发起一次机械修复调用（提示词由实现方在原始提示词后追加修复反馈）。
    ///
    /// # Errors
    ///
    /// 后端不可用/超时/熔断时返回 [`RecognizeError::BackendUnavailable`]。
    fn repair(
        &self,
        request: &OcrRequest,
        feedback: &RepairFeedback,
    ) -> Result<OcrResponse, RecognizeError>;
}

/// 识别结果仓储端口：写 `Recognition` 与 `ModelCall`。
///
/// TODO(接口接缝)：`eg-platform::repo::{insert_recognition, insert_model_call}` 需要
/// SQLite 事务（`rusqlite::Transaction`），由 `app` 组合根实现本端口并在同一事务内
/// 写入识别结果与调用证据；本模块不持有 SQL。
pub trait RecognitionRepo {
    /// 写入识别结果（幂等键由调用方保证：同页/段仅一条生效记录）。
    ///
    /// # Errors
    ///
    /// 存储失败返回 [`RecognizeError::Storage`]（任务级重试，不转人工）。
    fn save_recognition(&self, recognition: &Recognition) -> Result<(), RecognizeError>;

    /// 写入一次模型调用证据（每次后端调用一条，含失败与修复尝试）。
    ///
    /// # Errors
    ///
    /// 存储失败返回 [`RecognizeError::Storage`]。
    fn save_model_call(&self, call: &ModelCall) -> Result<(), RecognizeError>;
}

/// 领域 id 生成端口（单调 ULID）。
///
/// `SPEC-recognize.md` §2.1：`Recognition` 的 id 由本模块经注入的生成器产生，模型与
/// 推理层不伪造领域 id。`eg_platform::UlidFactory` 已实现本端口。
pub trait IdSource {
    /// 生成识别结果 id。
    ///
    /// # Errors
    ///
    /// 生成失败返回识别错误（按存储/内部失败处理，任务级重试）。
    fn next_recognition_id(&self) -> Result<eg_domain::RecognitionId, RecognizeError>;

    /// 生成模型调用记录 id。
    ///
    /// # Errors
    ///
    /// 生成失败返回识别错误。
    fn next_model_call_id(&self) -> Result<eg_domain::ModelCallId, RecognizeError>;
}

impl IdSource for eg_platform::UlidFactory {
    fn next_recognition_id(&self) -> Result<eg_domain::RecognitionId, RecognizeError> {
        eg_platform::UlidFactory::next_recognition_id(self)
            .map_err(|error| storage_error(format!("生成识别结果 id 失败：{error}")))
    }

    fn next_model_call_id(&self) -> Result<eg_domain::ModelCallId, RecognizeError> {
        eg_platform::UlidFactory::next_model_call_id(self)
            .map_err(|error| storage_error(format!("生成模型调用 id 失败：{error}")))
    }
}

/// 注入给识别器的全部 IO 能力（缺一即配置错误，不静默跳过）。
#[derive(Clone, Copy)]
pub struct RecognizeIo<'a> {
    /// 识别后端（识别角色）。
    pub backend: &'a dyn OcrBackend,
    /// 页/段图像来源。
    pub images: &'a dyn ImageSource,
    /// 识别缓存（开关由 `ocr.cache` 决定；关闭时仍必须注入，由调用方保证不读写）。
    pub cache: &'a dyn crate::cache::RecognitionCache,
    /// 识别结果与调用证据仓储。
    pub repo: &'a dyn RecognitionRepo,
    /// 注入时钟（`Recognition.created_at`/`ModelCall.created_at`）。
    pub clock: &'a dyn eg_domain::Clock,
    /// 领域 id 生成器。
    pub ids: &'a dyn IdSource,
    /// 协作式取消令牌。
    pub cancel: &'a CancelToken,
}

impl core::fmt::Debug for RecognizeIo<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RecognizeIo").finish_non_exhaustive()
    }
}

impl RecognizeIo<'_> {
    /// 取消检查：触发即返回 [`RecognizeError::Cancelled`]。
    pub(crate) fn check_cancel(&self) -> Result<(), RecognizeError> {
        self.cancel.check().map_err(|_| RecognizeError::Cancelled)
    }
}
