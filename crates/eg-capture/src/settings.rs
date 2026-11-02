//! 导入设置：`CONFIG.md` §2 `import.*` 的**快照**（本 crate 只使用快照，不读全局配置）。

use serde::{Deserialize, Serialize};

use eg_domain::Sha256Hex;

use crate::PIPELINE_VERSION;
use crate::error::CaptureError;

/// 拒收行为（`import.reject_behavior`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectBehavior {
    /// 页级拒收、批次继续（默认）。
    PageReject,
    /// 整批中止：未开始文件不再派发，已完成保留。
    AbortBatch,
}

/// 导入设置快照（键名与默认值见 `CONFIG.md` §2）。
///
/// `platform` 的 `AppConfig.import` 在接入时映射到本结构（见 `platform_bridge`）；
/// 本 crate 的测试与 CLI 直接用 [`Default`]（= CONFIG 默认值）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportSettings {
    /// `import.remind_naming`：选择文件时提示规范命名（仅提示一次，不解析文件名）。
    pub remind_naming: bool,
    /// `import.batch_file_limit`：单批文件数上限（1–10000）。
    pub batch_file_limit: u32,
    /// `import.pdf_dpi`：PDF 拆页分辨率（150–600）。
    pub pdf_dpi: u32,
    /// `import.quality_blur_threshold`：模糊阈值（60–300，长边 1600 口径）。
    pub quality_blur_threshold: f64,
    /// `import.quality_exposure_min`：欠曝下限（0.05–0.3）。
    pub quality_exposure_min: f64,
    /// `import.quality_exposure_max`：过曝上限（0.7–0.98）。
    pub quality_exposure_max: f64,
    /// `import.quality_skew_max`：倾斜上限（度，3–15）。
    pub quality_skew_max: f64,
    /// `import.quality_min_long_edge`：分辨率下限与模糊度量口径（1200–3000 px）。
    pub quality_min_long_edge: u32,
    /// `import.reject_behavior`：拒收行为。
    pub reject_behavior: RejectBehavior,
    /// `import.duplicate_notice`：重复/疑似重复提示。
    pub duplicate_notice: bool,
    /// `import.max_pdf_pages`：单份 PDF 最大页数（1–1000）。
    pub max_pdf_pages: u32,
    /// `import.max_file_mb`：单文件大小上限（1–512 MB）。
    pub max_file_mb: u32,
    /// `import.parallelism`：预处理并行度（1–8）。
    pub parallelism: u32,
    /// 缩略图长边（模块默认，`SPEC-capture.md` §3.1「缩略图（长边 512）」）。
    pub thumbnail_long_edge: u32,
}

impl Default for ImportSettings {
    fn default() -> Self {
        Self {
            remind_naming: true,
            batch_file_limit: 2_000,
            pdf_dpi: 300,
            quality_blur_threshold: 120.0,
            quality_exposure_min: 0.12,
            quality_exposure_max: 0.90,
            quality_skew_max: 8.0,
            quality_min_long_edge: 1_600,
            reject_behavior: RejectBehavior::PageReject,
            duplicate_notice: true,
            max_pdf_pages: 200,
            max_file_mb: 64,
            parallelism: 4,
            thumbnail_long_edge: 512,
        }
    }
}

impl ImportSettings {
    /// 校验取值范围（`CONFIG.md` §2 的取值列）；越界在批次开始前拒绝。
    ///
    /// # Errors
    ///
    /// 任一键越界时返回 [`CaptureError::InvalidSettings`]。
    pub fn validate(&self) -> Result<(), CaptureError> {
        let bad = |key: &str, value: String, range: &str| {
            CaptureError::InvalidSettings(format!("{key} = {value}，允许范围 {range}"))
        };
        if !(1..=10_000).contains(&self.batch_file_limit) {
            return Err(bad(
                "import.batch_file_limit",
                self.batch_file_limit.to_string(),
                "1–10000",
            ));
        }
        if !(150..=600).contains(&self.pdf_dpi) {
            return Err(bad("import.pdf_dpi", self.pdf_dpi.to_string(), "150–600"));
        }
        if !(60.0..=300.0).contains(&self.quality_blur_threshold) {
            return Err(bad(
                "import.quality_blur_threshold",
                self.quality_blur_threshold.to_string(),
                "60–300",
            ));
        }
        if !(0.05..=0.30).contains(&self.quality_exposure_min) {
            return Err(bad(
                "import.quality_exposure_min",
                self.quality_exposure_min.to_string(),
                "0.05–0.3",
            ));
        }
        if !(0.70..=0.98).contains(&self.quality_exposure_max) {
            return Err(bad(
                "import.quality_exposure_max",
                self.quality_exposure_max.to_string(),
                "0.7–0.98",
            ));
        }
        if self.quality_exposure_min >= self.quality_exposure_max {
            return Err(CaptureError::InvalidSettings(
                "import.quality_exposure_min 必须小于 import.quality_exposure_max".to_owned(),
            ));
        }
        if !(3.0..=15.0).contains(&self.quality_skew_max) {
            return Err(bad(
                "import.quality_skew_max",
                self.quality_skew_max.to_string(),
                "3–15 度",
            ));
        }
        if !(1_200..=3_000).contains(&self.quality_min_long_edge) {
            return Err(bad(
                "import.quality_min_long_edge",
                self.quality_min_long_edge.to_string(),
                "1200–3000 像素",
            ));
        }
        if !(1..=1_000).contains(&self.max_pdf_pages) {
            return Err(bad(
                "import.max_pdf_pages",
                self.max_pdf_pages.to_string(),
                "1–1000",
            ));
        }
        if !(1..=512).contains(&self.max_file_mb) {
            return Err(bad(
                "import.max_file_mb",
                self.max_file_mb.to_string(),
                "1–512 MB",
            ));
        }
        if !(1..=8).contains(&self.parallelism) {
            return Err(bad(
                "import.parallelism",
                self.parallelism.to_string(),
                "1–8",
            ));
        }
        if self.thumbnail_long_edge == 0 {
            return Err(CaptureError::InvalidSettings(
                "thumbnail_long_edge 必须大于 0".to_owned(),
            ));
        }
        Ok(())
    }

    /// 选项哈希：覆盖所有 `import.*` 键 + 流水线版本 + 缩略图尺寸（`SPEC-capture.md` §5 审计）。
    ///
    /// 派生图可重建的判据之一；键或值变化即视为不同派生口径。
    pub fn options_hash(&self) -> String {
        let mut parts: Vec<String> = vec![
            format!("pipeline={PIPELINE_VERSION}"),
            format!("remind_naming={}", self.remind_naming),
            format!("batch_file_limit={}", self.batch_file_limit),
            format!("pdf_dpi={}", self.pdf_dpi),
            format!("quality_blur_threshold={}", self.quality_blur_threshold),
            format!("quality_exposure_min={}", self.quality_exposure_min),
            format!("quality_exposure_max={}", self.quality_exposure_max),
            format!("quality_skew_max={}", self.quality_skew_max),
            format!("quality_min_long_edge={}", self.quality_min_long_edge),
            format!("reject_behavior={:?}", self.reject_behavior),
            format!("duplicate_notice={}", self.duplicate_notice),
            format!("max_pdf_pages={}", self.max_pdf_pages),
            format!("max_file_mb={}", self.max_file_mb),
            format!("parallelism={}", self.parallelism),
            format!("thumbnail_long_edge={}", self.thumbnail_long_edge),
        ];
        parts.sort();
        Sha256Hex::of(parts.join("\n").as_bytes()).to_hex()
    }
}
