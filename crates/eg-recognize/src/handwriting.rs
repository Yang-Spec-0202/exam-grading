//! 手写降级路径（`SPEC-recognize.md` §3.4）。
//!
//! 触发：识别级合成置信度低于阈值（默认 0.55）且页面存在手写块；或手写块位置信度
//! 低于 0.5。路径：保留原图与 `Segment.crop`，该题进人工录入队列；教师转录后产出
//! `RawAnswer`（来源「人工修正」，`blocks` 为空），本模块**不伪造识别证据**：
//! 人工录入不生成 `Recognition`、不写 `EngineStamp`。
//!
//! 不承诺任意手写体准确率；连笔、涂改、划线、重写默认走人工；可检出多版本答案但
//! 无法判定最终答案时标未决，不替教师选择。

use eg_domain::{
    AnswerSource, Confidence, ManualReason, PageId, QuestionId, RawAnswer, Recognition, SegmentId,
};

/// 页级/段级合成置信度低于该值且存在手写块 → 转人工。
pub const HANDWRITING_PAGE_THRESHOLD: f64 = 0.55;
/// 手写块置信度低于该值 → 转人工。
pub const HANDWRITING_BLOCK_THRESHOLD: f64 = 0.50;

/// 手写降级阈值（可配置；规范默认值见上）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HandwritingPolicy {
    /// 识别级合成置信度阈值。
    pub page_threshold: f64,
    /// 手写块置信度阈值。
    pub block_threshold: f64,
}

impl Default for HandwritingPolicy {
    fn default() -> Self {
        Self {
            page_threshold: HANDWRITING_PAGE_THRESHOLD,
            block_threshold: HANDWRITING_BLOCK_THRESHOLD,
        }
    }
}

/// 手写降级触发来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandwritingTrigger {
    /// 识别级合成置信度不足。
    PageConfidence,
    /// 手写块位置信度不足。
    BlockConfidence,
}

/// 转人工交接单（只描述「该题需要人工转录」，不写任何模型证据）。
#[derive(Debug, Clone, PartialEq)]
pub struct ManualHandoff {
    /// 页面 id（保留原图与裁片证据）。
    pub page_id: PageId,
    /// 段 id（页级识别时为空）。
    pub segment_id: Option<SegmentId>,
    /// 题目 id（页级识别阶段通常未对齐，由 `align` 回填）。
    ///
    /// TODO(接口接缝)：页级识别没有题目对齐信息；段级识别若 `Segment.question_id`
    /// 存在则填入，否则留空由 `align` 关联。
    pub question_id: Option<QuestionId>,
    /// 转人工原因（手写降级固定为「置信度不足」）。
    pub reason: ManualReason,
    /// 触发来源。
    pub trigger: HandwritingTrigger,
    /// 触发时的识别级置信度（供界面提示，不是人工确认值）。
    pub recognition_confidence: Confidence,
}

/// 判断是否需要走手写人工降级。
#[must_use]
pub fn handwriting_fallback(
    recognition: &Recognition,
    policy: &HandwritingPolicy,
) -> Option<ManualHandoff> {
    let handwriting: Vec<(usize, Confidence)> = recognition
        .blocks
        .iter()
        .enumerate()
        .filter(|(_, block)| block.kind == eg_domain::BlockKind::Handwriting)
        .map(|(index, block)| (index, block.confidence))
        .collect();
    if handwriting.is_empty() {
        return None;
    }
    let low_block = handwriting
        .iter()
        .find(|(_, confidence)| confidence.get() < policy.block_threshold);
    if let Some((_index, confidence)) = low_block {
        return Some(ManualHandoff {
            page_id: recognition.page_id,
            segment_id: recognition.segment_id,
            question_id: None,
            reason: ManualReason::LowConfidence,
            trigger: HandwritingTrigger::BlockConfidence,
            recognition_confidence: *confidence,
        });
    }
    if recognition.confidence.get() < policy.page_threshold {
        return Some(ManualHandoff {
            page_id: recognition.page_id,
            segment_id: recognition.segment_id,
            question_id: None,
            reason: ManualReason::LowConfidence,
            trigger: HandwritingTrigger::PageConfidence,
            recognition_confidence: recognition.confidence,
        });
    }
    None
}

/// 教师手工转录后的答案（来源「人工修正」，`blocks` 为空，不生成识别证据）。
///
/// 调用方必须使用 `platform` 审计记录操作者（固定 `local-teacher`）、时间与题目；
/// 本函数只构造领域对象，不写审计（审计属 `platform`）。
#[must_use]
pub fn manual_correction_answer(text: impl Into<String>, latex: Option<String>) -> RawAnswer {
    RawAnswer {
        text: text.into(),
        latex,
        blocks: Vec::new(),
        source: AnswerSource::ManualCorrection,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eg_domain::{BBox, Block, BlockKind, EngineStamp, ModelRole, Sha256Hex, Ulid};

    fn recognition(blocks: Vec<Block>, confidence: f64) -> Recognition {
        Recognition {
            id: eg_domain::RecognitionId::from_ulid(Ulid::from_parts(1, 1)),
            page_id: PageId::from_ulid(Ulid::from_parts(1, 2)),
            segment_id: None,
            blocks,
            text: "text".to_owned(),
            engine: EngineStamp {
                role: ModelRole::Ocr,
                backend: eg_domain::BackendId::fixture(),
                model_id: "m".to_owned(),
                model_version: "v".to_owned(),
                prompt_hash: Some(Sha256Hex::of(b"p")),
                params_hash: Sha256Hex::of(b"params"),
            },
            confidence: Confidence::new(confidence).unwrap(),
            created_at: eg_domain::time::parse_rfc3339("2026-11-02T00:00:00Z").unwrap(),
        }
    }

    fn block(kind: BlockKind, confidence: f64) -> Block {
        Block {
            kind,
            bbox: BBox::new(0.1, 0.1, 0.2, 0.1).unwrap(),
            text: "手写作答".to_owned(),
            latex: None,
            confidence: Confidence::new(confidence).unwrap(),
        }
    }

    #[test]
    fn no_handwriting_blocks_means_no_fallback() {
        let page = recognition(vec![block(BlockKind::Text, 0.9)], 0.9);
        assert!(handwriting_fallback(&page, &HandwritingPolicy::default()).is_none());
    }

    #[test]
    fn low_handwriting_block_triggers_block_trigger() {
        let page = recognition(
            vec![
                block(BlockKind::Text, 0.95),
                block(BlockKind::Handwriting, 0.40),
            ],
            0.90,
        );
        let handoff = handwriting_fallback(&page, &HandwritingPolicy::default()).unwrap();
        assert_eq!(handoff.trigger, HandwritingTrigger::BlockConfidence);
        assert_eq!(handoff.reason, ManualReason::LowConfidence);
        assert_eq!(handoff.recognition_confidence.get(), 0.40);
    }

    #[test]
    fn low_page_confidence_with_handwriting_triggers_page_trigger() {
        let page = recognition(vec![block(BlockKind::Handwriting, 0.80)], 0.50);
        let handoff = handwriting_fallback(&page, &HandwritingPolicy::default()).unwrap();
        assert_eq!(handoff.trigger, HandwritingTrigger::PageConfidence);
        assert_eq!(handoff.recognition_confidence.get(), 0.50);
    }

    #[test]
    fn confident_handwriting_is_not_forced_to_manual() {
        let page = recognition(vec![block(BlockKind::Handwriting, 0.90)], 0.88);
        assert!(handwriting_fallback(&page, &HandwritingPolicy::default()).is_none());
    }

    #[test]
    fn manual_correction_is_marked_and_has_no_blocks() {
        let answer = manual_correction_answer("x=2", Some("x=2".to_owned()));
        assert_eq!(answer.source, AnswerSource::ManualCorrection);
        assert_eq!(answer.blocks.len(), 0);
        assert_eq!(answer.text, "x=2");
        assert_eq!(answer.latex.as_deref(), Some("x=2"));
    }
}
