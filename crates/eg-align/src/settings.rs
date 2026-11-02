//! 对齐配置快照（`CONFIG.md` §3：`identity.*`/`align.*`）。
//!
//! 只读消费平台配置；`validate` 在批次开始前拒绝越界/不支持的组合（Ask first 的
//! 开关一律显式失败，不静默忽略）。

use eg_domain::Sha256Hex;
use eg_platform::AppConfig;

use crate::error::AlignError;

/// 身份来源（v1 只允许文件名；其余需要审批后才启用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentitySourceKind {
    /// 文件名（v1 唯一启用）。
    FileName,
    /// 条码（v1 无解码器，默认关闭）。
    Barcode,
}

/// 文件名未命中时的处理（`identity.unmatched_behavior`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnmatchedBehavior {
    /// 未决，教师人工选择（默认）。
    Undecided,
    /// 自动归入「名单外」（仍不自动判分）。
    OutsideRoster,
}

/// 对齐配置快照。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct AlignSettings {
    /// `identity.source`（v1 只允许文件名）。
    pub identity_source: IdentitySourceKind,
    /// `identity.ocr_fallback`（默认关；开启需审批）。
    pub ocr_fallback: bool,
    /// `identity.barcode`（默认关；v1 无解码器）。
    pub barcode: bool,
    /// `identity.unmatched_behavior`。
    pub unmatched_behavior: UnmatchedBehavior,
    /// `identity.ocr_threshold`（0.7–0.99）。
    pub ocr_threshold: f64,
    /// `identity.ocr_margin`（0.02–0.3）。
    pub ocr_margin: f64,
    /// `align.template_region`（默认开）。
    pub template_region: bool,
    /// `align.region_confidence_floor`（0.5–0.99）。
    pub region_confidence_floor: f64,
    /// `align.max_cross_pages`（1–5）。
    pub max_cross_pages: u32,
}

impl Default for AlignSettings {
    fn default() -> Self {
        Self {
            identity_source: IdentitySourceKind::FileName,
            ocr_fallback: false,
            barcode: false,
            unmatched_behavior: UnmatchedBehavior::Undecided,
            ocr_threshold: 0.90,
            ocr_margin: 0.10,
            template_region: true,
            region_confidence_floor: 0.80,
            max_cross_pages: 3,
        }
    }
}

impl AlignSettings {
    /// 从平台配置快照构造（`Platform::config()`）。
    ///
    /// 平台侧枚举到本 crate 语义的映射固定在唯一入口，避免两处漂移。
    #[must_use]
    pub fn from_platform_config(config: &AppConfig) -> Self {
        Self {
            identity_source: match config.identity.source {
                eg_platform::config::IdentitySource::FileName => IdentitySourceKind::FileName,
            },
            ocr_fallback: config.identity.ocr_fallback,
            barcode: config.identity.barcode,
            unmatched_behavior: match config.identity.unmatched_behavior {
                eg_platform::config::UnmatchedBehavior::Undecided => UnmatchedBehavior::Undecided,
                eg_platform::config::UnmatchedBehavior::RosterExternal => {
                    UnmatchedBehavior::OutsideRoster
                }
            },
            ocr_threshold: config.identity.ocr_threshold,
            ocr_margin: config.identity.ocr_margin,
            template_region: config.align.template_region,
            region_confidence_floor: config.align.region_confidence_floor,
            max_cross_pages: config.align.max_cross_pages,
        }
    }

    /// 校验配置（越界或不支持的组合在批次开始前拒绝）。
    ///
    /// # Errors
    ///
    /// 返回 [`AlignError::InvalidSettings`]（中文原因，含键名）。
    pub fn validate(&self) -> Result<(), AlignError> {
        if self.identity_source != IdentitySourceKind::FileName {
            return Err(AlignError::InvalidSettings(
                "identity.source：v1 只允许「文件名」（条码/OCR 身份需审批后启用）".to_owned(),
            ));
        }
        if self.barcode {
            return Err(AlignError::NotYetImplemented(
                "identity.barcode：v1 无条码解码器，需 Ask first 后才可开启",
            ));
        }
        if !(0.7..=0.99).contains(&self.ocr_threshold) {
            return Err(AlignError::InvalidSettings(format!(
                "identity.ocr_threshold={} 越界（0.7–0.99）",
                self.ocr_threshold
            )));
        }
        if !(0.02..=0.3).contains(&self.ocr_margin) {
            return Err(AlignError::InvalidSettings(format!(
                "identity.ocr_margin={} 越界（0.02–0.3）",
                self.ocr_margin
            )));
        }
        if !(0.5..=0.99).contains(&self.region_confidence_floor) {
            return Err(AlignError::InvalidSettings(format!(
                "align.region_confidence_floor={} 越界（0.5–0.99）",
                self.region_confidence_floor
            )));
        }
        if !(1..=5).contains(&self.max_cross_pages) {
            return Err(AlignError::InvalidSettings(format!(
                "align.max_cross_pages={} 越界（1–5）",
                self.max_cross_pages
            )));
        }
        Ok(())
    }

    /// 配置快照哈希（进对齐指纹；键名固定、浮点用 6 位小数避免格式化抖动）。
    #[must_use]
    pub fn options_hash(&self) -> Sha256Hex {
        let canonical = format!(
            "identity.source={:?}\nidentity.ocr_fallback={}\nidentity.barcode={}\n\
             identity.unmatched_behavior={:?}\nidentity.ocr_threshold={:.6}\n\
             identity.ocr_margin={:.6}\nalign.template_region={}\n\
             align.region_confidence_floor={:.6}\nalign.max_cross_pages={}",
            self.identity_source,
            self.ocr_fallback,
            self.barcode,
            self.unmatched_behavior,
            self.ocr_threshold,
            self.ocr_margin,
            self.template_region,
            self.region_confidence_floor,
            self.max_cross_pages
        );
        Sha256Hex::of(canonical.as_bytes())
    }
}
