//! 段级识别与按需触发（`SPEC-recognize.md` §3.1、§3.4）。
//!
//! 段级识别只影响该题：失败不推翻页级结果（由调用方保留页级 `Recognition`）。
//! 触发策略取 `ocr.segment_recognition`：关闭 / 按需（默认）/ 全量。按需触发条件为
//! 「页级存在低置信块」「段裁片长边不足 800 像素」「公式密集（计算/证明类需要分步
//! 证据）」「超大块」；题型上下文由 `align`/`grade` 提供，本模块以块结构近似判定。

use eg_domain::{Block, BlockKind, Page, Recognition, Segment, TaskId};
use eg_platform::config::{OcrConfig, SegmentRecognition};

use crate::error::RecognizeError;
use crate::page::{RecognizeOutcome, ScanTarget, run_scan};
use crate::types::RecognizeIo;

/// 段裁片长边不足该像素数时触发段级识别（`SPEC-recognize.md` §3.1）。
pub const SMALL_CROP_LONG_SIDE: f64 = 800.0;
/// 超大块面积占比阈值（触发段级识别）。
pub const OVERSIZED_BLOCK_AREA: f64 = 0.35;
/// 公式密集判定：段内公式块数量下限。
pub const DENSE_FORMULA_BLOCKS: usize = 2;
/// 公式密集判定：公式块面积占段面积比例。
pub const DENSE_FORMULA_AREA_RATIO: f64 = 0.25;

/// 段级识别题目上下文（由 `align`/`grade` 提供；不得包含答案内容）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SegmentContext {
    /// 题号文本（如「3(2)」），仅用于定位提示，不得据此编造内容。
    pub question_label: Option<String>,
    /// 是否预期为手写作答。
    pub expected_handwriting: bool,
}

/// 段级识别触发来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentTrigger {
    /// 页级存在低置信块。
    LowConfidence,
    /// 段裁片长边不足 800 像素。
    SmallCrop,
    /// 公式密集（需要分步证据）。
    FormulaDense,
    /// 超大块（页级结构可能被压缩）。
    OversizedBlock,
    /// 全量模式（实验开关，成本约翻倍）。
    Full,
}

/// 段级识别决策。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentRecognizeDecision {
    /// 是否执行段级识别。
    pub should_recognize: bool,
    /// 命中的触发条件（按固定顺序，便于测试与日志稳定）。
    pub triggers: Vec<SegmentTrigger>,
}

/// 是否对某段执行段级识别（只做决策，不调用模型）。
///
/// TODO(接口接缝)：规范还包含「该题题型为计算/解答或证明且需要分步证据」这一条件，
/// 需要 `align`/`grade` 的题型信息；当前用「公式密集」近似，待题型接口定稿后补充。
#[must_use]
pub fn segment_decision(
    page: &Page,
    segment: &Segment,
    page_recognition: &Recognition,
    config: &OcrConfig,
) -> SegmentRecognizeDecision {
    match config.segment_recognition {
        SegmentRecognition::Off => SegmentRecognizeDecision {
            should_recognize: false,
            triggers: Vec::new(),
        },
        SegmentRecognition::Full => SegmentRecognizeDecision {
            should_recognize: true,
            triggers: vec![SegmentTrigger::Full],
        },
        SegmentRecognition::OnDemand => {
            let mut triggers = Vec::new();
            if has_low_confidence_block(page_recognition, segment, config.confidence_floor) {
                triggers.push(SegmentTrigger::LowConfidence);
            }
            let long_side = (segment.bbox.w * f64::from(page.width))
                .max(segment.bbox.h * f64::from(page.height));
            if long_side < SMALL_CROP_LONG_SIDE {
                triggers.push(SegmentTrigger::SmallCrop);
            }
            if formula_dense(page_recognition, segment) {
                triggers.push(SegmentTrigger::FormulaDense);
            }
            if segment.bbox.w * segment.bbox.h > OVERSIZED_BLOCK_AREA {
                triggers.push(SegmentTrigger::OversizedBlock);
            }
            SegmentRecognizeDecision {
                should_recognize: !triggers.is_empty(),
                triggers,
            }
        }
    }
}

/// 段级识别入口（聚焦单个作答区域）。
///
/// # Errors
///
/// 与页级识别相同；段级失败只影响该题，调用方必须保留页级结果。
pub fn recognize_segment(
    io: &RecognizeIo<'_>,
    page: &Page,
    segment: &Segment,
    context: &SegmentContext,
    config: &OcrConfig,
    task_id: TaskId,
) -> Result<RecognizeOutcome, RecognizeError> {
    run_scan(
        io,
        ScanTarget::Segment {
            page,
            segment,
            context,
        },
        config,
        task_id,
    )
}

fn has_low_confidence_block(recognition: &Recognition, segment: &Segment, floor: f64) -> bool {
    recognition
        .blocks
        .iter()
        .any(|block| block.confidence.get() < floor && block_in_segment(block, segment))
}

fn formula_dense(recognition: &Recognition, segment: &Segment) -> bool {
    let segment_area = segment.bbox.w * segment.bbox.h;
    if segment_area <= 0.0 {
        return false;
    }
    let formulas: Vec<&Block> = recognition
        .blocks
        .iter()
        .filter(|block| block.kind == BlockKind::Formula && block_in_segment(block, segment))
        .collect();
    if formulas.len() >= DENSE_FORMULA_BLOCKS {
        return true;
    }
    let formula_area: f64 = formulas
        .iter()
        .map(|block| block.bbox.w * block.bbox.h)
        .sum();
    formula_area / segment_area > DENSE_FORMULA_AREA_RATIO
}

fn block_in_segment(block: &Block, segment: &Segment) -> bool {
    let intersection_width =
        (block.bbox.right().min(segment.bbox.right()) - block.bbox.x.max(segment.bbox.x)).max(0.0);
    let intersection_height = (block.bbox.bottom().min(segment.bbox.bottom())
        - block.bbox.y.max(segment.bbox.y))
    .max(0.0);
    let intersection = intersection_width * intersection_height;
    let block_area = block.bbox.w * block.bbox.h;
    if block_area <= 0.0 {
        return false;
    }
    let center_x = block.bbox.x + block.bbox.w / 2.0;
    let center_y = block.bbox.y + block.bbox.h / 2.0;
    let center_inside = center_x >= segment.bbox.x
        && center_x <= segment.bbox.right()
        && center_y >= segment.bbox.y
        && center_y <= segment.bbox.bottom();
    center_inside || intersection / block_area > 0.5
}

#[cfg(test)]
mod tests {
    use super::*;
    use eg_domain::{
        BBox, Confidence, EngineStamp, ExifOrientation, ModelRole, PageQuality, QualityVerdict,
        Rotation, Sha256Hex, Ulid,
    };

    fn page(width: u32, height: u32) -> Page {
        Page {
            id: eg_domain::PageId::from_ulid(Ulid::from_parts(1, 1)),
            submission_id: eg_domain::SubmissionId::from_ulid(Ulid::from_parts(1, 2)),
            page_no: 1,
            asset: eg_domain::AssetId::of(b"page"),
            width,
            height,
            rotation_applied: Rotation::NONE,
            exif_orientation: ExifOrientation::NONE,
            quality: PageQuality {
                blur_var: 100.0,
                exposure: 1.0,
                skew_deg: 0.0,
                resolution_ok: true,
                verdict: QualityVerdict::Good,
            },
            derived: Vec::new(),
        }
    }

    fn segment(x: f64, y: f64, w: f64, h: f64) -> Segment {
        Segment {
            id: eg_domain::SegmentId::from_ulid(Ulid::from_parts(1, 3)),
            page_id: eg_domain::PageId::from_ulid(Ulid::from_parts(1, 1)),
            question_id: None,
            bbox: BBox::new(x, y, w, h).unwrap(),
            crop: eg_domain::AssetId::of(b"crop"),
            method: eg_domain::SegmentMethod::LayoutDetection,
        }
    }

    fn block(kind: BlockKind, x: f64, y: f64, w: f64, h: f64, confidence: f64) -> Block {
        Block {
            kind,
            bbox: BBox::new(x, y, w, h).unwrap(),
            text: "作答".to_owned(),
            latex: if kind == BlockKind::Formula {
                Some("x+1".to_owned())
            } else {
                None
            },
            confidence: Confidence::new(confidence).unwrap(),
        }
    }

    fn recognition(blocks: Vec<Block>) -> Recognition {
        Recognition {
            id: eg_domain::RecognitionId::from_ulid(Ulid::from_parts(1, 4)),
            page_id: eg_domain::PageId::from_ulid(Ulid::from_parts(1, 1)),
            segment_id: None,
            blocks,
            text: String::new(),
            engine: EngineStamp {
                role: ModelRole::Ocr,
                backend: eg_domain::BackendId::fixture(),
                model_id: "m".to_owned(),
                model_version: "v".to_owned(),
                prompt_hash: None,
                params_hash: Sha256Hex::of(b"p"),
            },
            confidence: Confidence::ZERO,
            created_at: eg_domain::time::parse_rfc3339("2026-11-02T00:00:00Z").unwrap(),
        }
    }

    fn config() -> OcrConfig {
        OcrConfig::default()
    }

    #[test]
    fn off_mode_never_triggers() {
        let mut config = config();
        config.segment_recognition = SegmentRecognition::Off;
        let decision = segment_decision(
            &page(1600, 2200),
            &segment(0.1, 0.1, 0.2, 0.1),
            &recognition(vec![block(BlockKind::Text, 0.1, 0.1, 0.2, 0.1, 0.2)]),
            &config,
        );
        assert!(!decision.should_recognize);
        assert_eq!(decision.triggers.len(), 0);
    }

    #[test]
    fn full_mode_always_triggers() {
        let mut config = config();
        config.segment_recognition = SegmentRecognition::Full;
        let decision = segment_decision(
            &page(1600, 2200),
            &segment(0.1, 0.1, 0.8, 0.8),
            &recognition(vec![block(BlockKind::Text, 0.1, 0.1, 0.8, 0.8, 0.99)]),
            &config,
        );
        assert!(decision.should_recognize);
        assert_eq!(decision.triggers, vec![SegmentTrigger::Full]);
    }

    #[test]
    fn on_demand_detects_low_confidence_and_small_crop() {
        let decision = segment_decision(
            &page(1600, 2200),
            &segment(0.1, 0.1, 0.2, 0.2),
            &recognition(vec![block(BlockKind::Text, 0.1, 0.1, 0.2, 0.2, 0.4)]),
            &config(),
        );
        assert!(decision.should_recognize);
        assert_eq!(
            decision.triggers,
            vec![SegmentTrigger::LowConfidence, SegmentTrigger::SmallCrop]
        );
    }

    #[test]
    fn on_demand_detects_formula_dense_and_oversized() {
        let recognition = recognition(vec![
            block(BlockKind::Formula, 0.1, 0.1, 0.5, 0.2, 0.9),
            block(BlockKind::Formula, 0.1, 0.35, 0.5, 0.2, 0.9),
        ]);
        let decision = segment_decision(
            &page(1600, 2200),
            &segment(0.05, 0.05, 0.8, 0.6),
            &recognition,
            &config(),
        );
        assert!(decision.triggers.contains(&SegmentTrigger::FormulaDense));
        assert!(decision.triggers.contains(&SegmentTrigger::OversizedBlock));
    }

    #[test]
    fn no_trigger_means_no_segment_call() {
        let decision = segment_decision(
            &page(1600, 2200),
            &segment(0.1, 0.1, 0.5, 0.6),
            &recognition(vec![block(BlockKind::Text, 0.1, 0.1, 0.5, 0.6, 0.95)]),
            &config(),
        );
        assert!(!decision.should_recognize);
        assert_eq!(decision.triggers.len(), 0);
    }
}
