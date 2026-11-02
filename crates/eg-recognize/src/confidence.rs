//! 置信度合成（`SPEC-recognize.md` §3.5）。
//!
//! 规则要点：模型自评未经本地校准 → 上报上限 0.95；模型没给概率时用结构启发值并
//! **标注来源**（不得伪装成模型置信度）；页级按字符数加权、乘结构因子；题级未做段级
//! 识别乘 0.9，跨页取最小值，跨 3 页及以上强制人工。本模块只产出识别侧置信度，
//! 最终是否自动接受由 `grade` 按模式阈值决定。

use eg_domain::{BlockKind, Confidence, ManualReason};

use crate::semantic::{BlockRecord, StructureQuality};

/// 上报置信度上限（模型未在本地校准集上校准前）。
pub const CONFIDENCE_CAP: f64 = 0.95;
/// 结构启发：首次通过 Schema。
pub const HEURISTIC_FIRST_PASS: f64 = 0.90;
/// 结构启发：修复后通过 Schema。
pub const HEURISTIC_REPAIRED: f64 = 0.75;
/// 结构启发：未知块。
pub const HEURISTIC_UNKNOWN_BLOCK: f64 = 0.30;
/// 结构启发：非公式块文本为空。
pub const HEURISTIC_EMPTY_TEXT: f64 = 0.20;
/// 结构启发：不可用块（公式非法或 LaTeX 关闭）。
pub const HEURISTIC_UNUSABLE: f64 = 0.0;
/// 结构因子：修复后通过。
pub const STRUCT_FACTOR_REPAIRED: f64 = 0.90;
/// 结构因子：含未知块。
pub const STRUCT_FACTOR_UNKNOWN_BLOCK: f64 = 0.85;
/// 结构因子：阅读顺序可疑。
pub const STRUCT_FACTOR_ORDER_SUSPECT: f64 = 0.90;
/// 结构因子：印章遮挡。
pub const STRUCT_FACTOR_STAMP_OCCLUDES: f64 = 0.85;
/// 未聚焦识别惩罚（未做段级识别）。
pub const UNFOCUSED_PENALTY: f64 = 0.90;
/// 跨页题强制人工的页数下限。
pub const CROSS_PAGE_MANUAL_LIMIT: usize = 3;
/// 块级平均权重上下限（字符数）。
pub const WEIGHT_MIN: f64 = 1.0;
/// 块级平均权重上限。
pub const WEIGHT_MAX: f64 = 200.0;

/// 置信度来源标注（进 `ModelCall` 输出与审计，禁止把启发值伪装成模型值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfidenceSource {
    /// 模型给出的概率（token/序列概率或自评）。
    Model,
    /// 结构启发值。
    StructuralHeuristic,
}

/// 块级置信度：模型给了值就用模型值（裁剪到上限），否则用结构启发值。
#[must_use]
#[allow(
    clippy::fn_params_excessive_bools,
    reason = "四个布尔量分别对应规范 §3.5 的独立规则（首次/修复/未知块/空文本/可用性）"
)]
pub fn block_confidence(
    model_value: Option<f64>,
    first_pass: bool,
    is_unknown_block: bool,
    is_empty_text: bool,
    usable: bool,
) -> (Confidence, ConfidenceSource) {
    if !usable {
        return (
            confidence(HEURISTIC_UNUSABLE),
            ConfidenceSource::StructuralHeuristic,
        );
    }
    if let Some(value) = model_value {
        return (
            confidence(value.min(CONFIDENCE_CAP)),
            ConfidenceSource::Model,
        );
    }
    let mut value = if first_pass {
        HEURISTIC_FIRST_PASS
    } else {
        HEURISTIC_REPAIRED
    };
    if is_unknown_block {
        value = value.min(HEURISTIC_UNKNOWN_BLOCK);
    }
    if is_empty_text {
        value = value.min(HEURISTIC_EMPTY_TEXT);
    }
    (confidence(value), ConfidenceSource::StructuralHeuristic)
}

/// 页级置信度：字符数加权平均 × 结构因子，裁剪到 0–0.95。
#[must_use]
pub fn page_confidence(records: &[BlockRecord], quality: &StructureQuality) -> Confidence {
    if records.is_empty() {
        return Confidence::ZERO;
    }
    let mut weighted_sum = 0.0;
    let mut weight_sum = 0.0;
    for record in records {
        let weight = record_weight(record);
        weighted_sum += record.block.confidence.get() * weight;
        weight_sum += weight;
    }
    if weight_sum <= 0.0 {
        return Confidence::ZERO;
    }
    let mean = weighted_sum / weight_sum;
    confidence(mean * quality.factor())
}

fn record_weight(record: &BlockRecord) -> f64 {
    let raw = if record.block.kind == BlockKind::Formula {
        record
            .block
            .latex
            .as_deref()
            .map_or(0, |latex| latex.chars().count()) as f64
    } else {
        record.block.text.chars().count() as f64
    };
    raw.clamp(WEIGHT_MIN, WEIGHT_MAX)
}

fn confidence(value: f64) -> Confidence {
    let clamped = if value.is_finite() {
        value.clamp(0.0, CONFIDENCE_CAP)
    } else {
        0.0
    };
    Confidence::new(clamped).unwrap_or(Confidence::ZERO)
}

/// 题级置信度结论。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QuestionConfidence {
    /// 合成后的识别侧置信度。
    pub confidence: Confidence,
    /// 强制人工原因（跨 3 页及以上等）；`None` 表示不强制。
    pub manual_reason: Option<ManualReason>,
}

/// 题级置信度：跨页取最小；做过段级识别用段级值；否则页级乘 0.9。
#[must_use]
pub fn question_confidence(
    page_confidences: &[Confidence],
    segment_confidences: &[Confidence],
) -> QuestionConfidence {
    if page_confidences.is_empty() && segment_confidences.is_empty() {
        return QuestionConfidence {
            confidence: Confidence::ZERO,
            manual_reason: Some(ManualReason::LowConfidence),
        };
    }
    let mut minimum = Confidence::ONE;
    for value in page_confidences.iter().chain(segment_confidences) {
        minimum = minimum.min(*value);
    }
    if page_confidences.len() >= CROSS_PAGE_MANUAL_LIMIT {
        return QuestionConfidence {
            confidence: confidence(minimum.get()),
            manual_reason: Some(ManualReason::LowConfidence),
        };
    }
    let value = if segment_confidences.is_empty() {
        minimum.get() * UNFOCUSED_PENALTY
    } else {
        minimum.get()
    };
    QuestionConfidence {
        confidence: confidence(value),
        manual_reason: None,
    }
}

/// 一次识别结果的置信度明细（进 `ModelCall` 输出/审计/界面提示）。
#[derive(Debug, Clone, PartialEq)]
pub struct ConfidenceBreakdown {
    /// 识别级（页级或段级）合成置信度。
    pub page: Confidence,
    /// 块级置信度（与 `Recognition.blocks` 一一对应）。
    pub blocks: Vec<Confidence>,
    /// 结构因子（乘在加权平均上）。
    pub structure_factor: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic::{BlockRecord, BlockStatus, StructureQuality};

    fn record(kind: BlockKind, text: &str, latex: Option<&str>, confidence: f64) -> BlockRecord {
        BlockRecord {
            block: eg_domain::Block {
                kind,
                bbox: eg_domain::BBox::new(0.1, 0.1, 0.2, 0.1).unwrap(),
                text: text.to_owned(),
                latex: latex.map(str::to_owned),
                confidence: Confidence::new(confidence).unwrap(),
            },
            source: ConfidenceSource::Model,
            status: BlockStatus::Usable,
        }
    }

    fn quality() -> StructureQuality {
        StructureQuality {
            first_pass: true,
            repaired: false,
            order_violation_ratio: 0.0,
            order_suspect: false,
            two_column_layout: false,
            has_unknown_block: false,
            formula_invalid_count: 0,
            formula_latex_disabled_count: 0,
            stamp_occludes: false,
            coverage_ratio: 0.2,
            background_verified: true,
        }
    }

    #[test]
    fn block_confidence_rules_table() {
        // 模型值：裁剪到 0.95 上限，来源标注为模型。
        assert_eq!(
            block_confidence(Some(0.99), true, false, false, true),
            (Confidence::new(0.95).unwrap(), ConfidenceSource::Model)
        );
        assert_eq!(
            block_confidence(Some(0.60), true, true, false, true),
            (Confidence::new(0.60).unwrap(), ConfidenceSource::Model)
        );
        // 首次通过：0.90；修复后：0.75。
        assert_eq!(
            block_confidence(None, true, false, false, true).0.get(),
            HEURISTIC_FIRST_PASS
        );
        assert_eq!(
            block_confidence(None, false, false, false, true).0.get(),
            HEURISTIC_REPAIRED
        );
        // 未知块/空文本/不可用逐级取小。
        assert_eq!(
            block_confidence(None, true, true, false, true).0.get(),
            HEURISTIC_UNKNOWN_BLOCK
        );
        assert_eq!(
            block_confidence(None, true, false, true, true).0.get(),
            HEURISTIC_EMPTY_TEXT
        );
        assert_eq!(
            block_confidence(None, true, true, true, true).0.get(),
            HEURISTIC_EMPTY_TEXT
        );
        // 不可用块恒为 0，即使模型给了高置信度。
        let (value, source) = block_confidence(Some(0.99), true, false, false, false);
        assert_eq!(value, Confidence::ZERO);
        assert_eq!(source, ConfidenceSource::StructuralHeuristic);
    }

    #[test]
    fn page_confidence_is_character_weighted() {
        // 短块 0.9（1 字符）与长块 0.3（199 字符）：加权平均接近 0.303。
        let short = record(BlockKind::Text, "1", None, 0.90);
        let long = record(BlockKind::Text, &"答".repeat(199), None, 0.30);
        let value = page_confidence(&[short, long], &quality());
        let expected = (0.90 * 1.0 + 0.30 * 199.0) / 200.0;
        assert!(
            (value.get() - expected).abs() < 1e-9,
            "{value:?} vs {expected}"
        );
        // 公式按 LaTeX 长度计权。
        let formula = record(BlockKind::Formula, "ignored", Some(&"x".repeat(50)), 0.8);
        let page = page_confidence(&[formula], &quality());
        assert!((page.get() - 0.8).abs() < 1e-9);
    }

    #[test]
    fn page_confidence_multiplies_structure_factor() {
        let mut flags = quality();
        flags.repaired = true;
        flags.first_pass = false;
        flags.has_unknown_block = true;
        flags.order_suspect = true;
        flags.stamp_occludes = true;
        assert!((flags.factor() - 0.90 * 0.85 * 0.90 * 0.85).abs() < 1e-9);
        let record = record(BlockKind::Text, "answer", None, 0.90);
        let page = page_confidence(&[record], &flags);
        assert!(page.get() <= 0.90 * 0.90 * 0.85 * 0.90 * 0.85 + 1e-9);
        assert!(page.get() <= CONFIDENCE_CAP);
    }

    #[test]
    fn page_confidence_caps_at_095() {
        let record = record(BlockKind::Text, "answer", None, 1.0);
        let page = page_confidence(&[record], &quality());
        assert_eq!(page.get(), CONFIDENCE_CAP);
    }

    #[test]
    fn question_confidence_rules_table() {
        let high = Confidence::new(0.90).unwrap();
        let low = Confidence::new(0.60).unwrap();
        // 单页、无段级：页级 × 0.9。
        let single = question_confidence(&[high], &[]);
        assert!((single.confidence.get() - 0.81).abs() < 1e-9);
        assert_eq!(single.manual_reason, None);
        // 做过段级识别：取段级（不乘 0.9）。
        let focused = question_confidence(&[high], &[Confidence::new(0.80).unwrap()]);
        assert!((focused.confidence.get() - 0.80).abs() < 1e-9);
        // 跨页取最小。
        let cross = question_confidence(&[high, low], &[]);
        assert!((cross.confidence.get() - 0.60 * 0.9).abs() < 1e-9);
        // 跨 3 页及以上强制人工（即使置信度高）。
        let three = question_confidence(&[high, high, high], &[]);
        assert_eq!(three.manual_reason, Some(ManualReason::LowConfidence));
        assert!((three.confidence.get() - 0.90).abs() < 1e-9);
        // 无任何页级结果：0 且强制人工。
        let empty = question_confidence(&[], &[]);
        assert_eq!(empty.confidence, Confidence::ZERO);
        assert_eq!(empty.manual_reason, Some(ManualReason::LowConfidence));
    }
}
