//! 错误分类与质量原因码（`SPEC-capture.md` §3.3、§4；`SPEC-platform.md` 的 `ErrorClass` 语义）。

use thiserror::Error;

/// 导入/预处理错误。
///
/// 每个变体都映射到 [`CaptureErrorClass`] 与稳定 `code()`：
/// 报告、事件、日志用 `code`，界面用 `Display` 中文文案。
#[derive(Debug, Error, Clone)]
pub enum CaptureError {
    /// `import.*` 取值越界（启动前拒绝，不进入流水线）。
    #[error("导入配置非法：{0}")]
    InvalidSettings(String),
    /// 扩展名或文件头不在白名单内（含伪装扩展名）。
    #[error("不支持的格式：扩展名 {extension}，文件头 {magic}")]
    UnsupportedFormat {
        /// 原始扩展名（小写，不含点）。
        extension: String,
        /// 文件头指纹（十六进制前 8 位）。
        magic: String,
    },
    /// HEIC/HEIF：需要系统图像接口，当前构建不可用（显式失败，不静默跳过）。
    #[error("HEIC/HEIF 解码器不可用：请转码为 JPG 或 PNG 后重试")]
    HeicUnsupported,
    /// 文件字节数超过 `import.max_file_mb`。
    #[error("文件超过大小上限：{bytes} 字节 > {limit_mb} MB")]
    FileTooLarge {
        /// 实际字节数。
        bytes: u64,
        /// 配置上限（MB）。
        limit_mb: u32,
    },
    /// PDF 页数超过 `import.max_pdf_pages`。
    #[error("PDF 页数超限：{pages} 页 > {limit} 页")]
    PdfPageLimit {
        /// 实际页数。
        pages: usize,
        /// 配置上限。
        limit: u32,
    },
    /// 解码失败（含伪装扩展名、损坏文件）。
    #[error("无法解码：{0}")]
    Decode(String),
    /// PDF 打开失败（加密或损坏）。
    #[error("无法打开 PDF（可能已加密或损坏）：{0}")]
    PdfOpen(String),
    /// 单页渲染失败（页码从 1 起）。
    #[error("PDF 第 {page} 页渲染失败：{message}")]
    PdfRender {
        /// 页号（1 起）。
        page: u32,
        /// 失败原因。
        message: String,
    },
    /// 资产写入失败（含磁盘满）。
    #[error("资产写入失败：{0}")]
    Asset(String),
    /// 图像处理失败（派生图、几何矫正等）。
    #[error("图像处理失败：{0}")]
    ImageOp(String),
    /// 协作式取消命中检查点。
    #[error("导入已取消")]
    Cancelled,
    /// 读取来源文件失败。
    #[error("读取文件失败：{0}")]
    Io(String),
    /// 批内文件数超过 `import.batch_file_limit`（开始前拒绝）。
    #[error("单批文件数超限：{files} 个 > {limit} 个")]
    BatchTooLarge {
        /// 实际文件数。
        files: usize,
        /// 配置上限。
        limit: u32,
    },
    /// 落库失败（D1 单事务）：资产未先落盘、数据库约束冲突或事务/阶段更新失败。
    #[error("落库失败：{0}")]
    PersistFailed(String),
    /// 内部不变量被破坏（提交校验失败等；属缺陷，不是教师输入问题）。
    #[error("内部错误：{0}")]
    Internal(String),
    /// 明确未实现的通道（如 DOCX 参考答案）：显式失败，不静默跳过。
    #[error("该能力尚未实现：{0}")]
    NotYetImplemented(&'static str),
}

impl CaptureError {
    /// 稳定错误码（报告/事件/日志用；界面不显示）。
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidSettings(_) => "invalid_settings",
            Self::UnsupportedFormat { .. } => "unsupported_format",
            Self::HeicUnsupported => "heic_unsupported",
            Self::FileTooLarge { .. } => "file_too_large",
            Self::PdfPageLimit { .. } => "pdf_page_limit",
            Self::Decode(_) => "decode_failed",
            Self::PdfOpen(_) => "pdf_open_failed",
            Self::PdfRender { .. } => "pdf_render_failed",
            Self::Asset(_) => "asset_write_failed",
            Self::ImageOp(_) => "image_op_failed",
            Self::Cancelled => "cancelled",
            Self::Io(_) => "io_failed",
            Self::BatchTooLarge { .. } => "batch_too_large",
            Self::PersistFailed(_) => "persist_failed",
            Self::Internal(_) => "internal",
            Self::NotYetImplemented(_) => "not_implemented",
        }
    }

    /// 错误分类：决定重试策略与界面文案（`SPEC-capture.md` §4「重试策略」）。
    pub const fn class(&self) -> CaptureErrorClass {
        match self {
            // 落库事务整体回滚，可安全整体重试（阶段推进失败除外，见 persist 文档）。
            Self::Io(_) | Self::Asset(_) | Self::PersistFailed(_) => CaptureErrorClass::Transient,
            Self::Cancelled => CaptureErrorClass::PolicyBlocked,
            Self::Internal(_) => CaptureErrorClass::Permanent,
            Self::BatchTooLarge { .. } => CaptureErrorClass::TeacherFixable,
            Self::InvalidSettings(_)
            | Self::UnsupportedFormat { .. }
            | Self::HeicUnsupported
            | Self::FileTooLarge { .. }
            | Self::PdfPageLimit { .. }
            | Self::Decode(_)
            | Self::PdfOpen(_)
            | Self::PdfRender { .. }
            | Self::ImageOp(_)
            | Self::NotYetImplemented(_) => CaptureErrorClass::Permanent,
        }
    }

    /// 是否可重试（仅瞬时 IO 错误，最多 2 次，指数退避）。
    pub const fn retryable(&self) -> bool {
        matches!(self.class(), CaptureErrorClass::Transient)
    }
}

/// 错误分类（与 `SPEC-platform.md` 的 `ErrorClass` 取值对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureErrorClass {
    /// 教师可修复（换文件、解密、转码）。
    TeacherFixable,
    /// 瞬态（IO/磁盘），可退避重试。
    Transient,
    /// 永久（格式/解码/渲染），不重试。
    Permanent,
    /// 策略拦截（取消、授权）。
    PolicyBlocked,
}

/// 页面拒收原因（`SPEC-capture.md` §3.3 质量门控表；命中任一即拒收）。
///
/// `code()` 稳定供报告/事件/审计使用，`Display` 给教师看。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RejectReason {
    /// 模糊（拉普拉斯方差低于 `import.quality_blur_threshold`）。
    Blur,
    /// 欠曝（曝光度量低于 `import.quality_exposure_min`）。
    Underexposed,
    /// 过曝（曝光度量高于 `import.quality_exposure_max`）。
    Overexposed,
    /// 眩光（高光溢出像素占比 > 25%）。
    Glare,
    /// 死黑（近黑像素占比 > 40%）。
    DeadBlack,
    /// 倾斜（估计倾角大于 `import.quality_skew_max`）。
    Skew,
    /// 分辨率不足（长边小于 `import.quality_min_long_edge`）。
    Resolution,
    /// 无文字行结构（空白/倒置/纯噪声）。
    NoLineStructure,
}

impl RejectReason {
    /// 稳定原因码。
    pub const fn code(self) -> &'static str {
        match self {
            Self::Blur => "blur",
            Self::Underexposed => "underexposed",
            Self::Overexposed => "overexposed",
            Self::Glare => "glare",
            Self::DeadBlack => "dead_black",
            Self::Skew => "skew",
            Self::Resolution => "resolution",
            Self::NoLineStructure => "no_line_structure",
        }
    }

    /// 教师可读原因（中文）。
    pub const fn message(self) -> &'static str {
        match self {
            Self::Blur => "图像模糊",
            Self::Underexposed => "光线不足（欠曝）",
            Self::Overexposed => "过曝",
            Self::Glare => "反光/眩光",
            Self::DeadBlack => "画面过暗（死黑）",
            Self::Skew => "页面倾斜",
            Self::Resolution => "分辨率不足",
            Self::NoLineStructure => "未检测到文字行结构（可能空白或倒置）",
        }
    }
}

impl std::fmt::Display for RejectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

/// 页面非拒收提示（只记录原因码，不升级为拒收；`SPEC-capture.md` §3.3 末段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PageNote {
    /// 同框多页：取最大者并提示。
    MultiplePages,
    /// 未找到合格轮廓：整图作页。
    NoContour,
    /// 透视异常回退到方向校正图。
    PerspectiveFallback,
    /// 超大图降采样后处理（长边 > 4000）。
    Downsampled,
    /// 无 EXIF 方向信息（按 1 处理）。
    ExifMissing,
    /// 多页 TIFF 仅取首帧。
    MultiPageTiffFirstFrameOnly,
}

impl PageNote {
    /// 稳定提示码。
    pub const fn code(self) -> &'static str {
        match self {
            Self::MultiplePages => "multiple_pages",
            Self::NoContour => "no_contour",
            Self::PerspectiveFallback => "perspective_fallback",
            Self::Downsampled => "downsampled",
            Self::ExifMissing => "exif_missing",
            Self::MultiPageTiffFirstFrameOnly => "multipage_tiff_first_frame_only",
        }
    }
}
