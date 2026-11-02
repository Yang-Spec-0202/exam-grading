//! 跨页题拼接（`SPEC-align.md` §3.3「跨页题」、§5「缺页/跨页冲突」）。
//!
//! 一题可对应多页 `Segment`：`StudentAnswer.parts` 每页一段（页 id/段 id/识别 id/坐标/文本），
//! 主段取首页，`raw.text` 按 `page_no` 升序合并，`recognition_confidence` 取各段最小值。
//! 缺中间页 → 不合并（`raw.text` 留空）并记未决；超过 `align.max_cross_pages` → 未决且不构造答案。

use std::collections::BTreeMap;

use eg_domain::{
    AnswerPart, AnswerSource, BBox, Confidence, Paper, QuestionId, RawAnswer, Segment, SegmentId,
    StudentAnswer, SubmissionId,
};

use crate::error::AlignError;
use crate::ports::{PageRef, RecognitionSource};
use crate::region::AlignedQuestion;
use crate::report::{UnresolvedItem, UnresolvedKind};
use crate::settings::AlignSettings;

/// 跨页拼接产物。
#[derive(Debug, Clone)]
pub struct AnswerBuildOutcome {
    /// 学生答案（只为有区域且可构造的题）。
    pub answers: Vec<StudentAnswer>,
    /// 未决项（缺页/跨页冲突/识别不可用）。
    pub unresolved: Vec<UnresolvedItem>,
}

/// 为已对齐题目构造 `StudentAnswer`（按试卷顺序）。
///
/// # Errors
///
/// 内部不变量（区域 id 对不上、域校验失败）→ [`AlignError::Internal`]。
pub fn build_answers(
    submission_id: SubmissionId,
    paper: &Paper,
    aligned: &[AlignedQuestion],
    segments: &[Segment],
    pages: &[PageRef],
    recognitions: &dyn RecognitionSource,
    settings: &AlignSettings,
) -> Result<AnswerBuildOutcome, AlignError> {
    let page_no_by_id: BTreeMap<eg_domain::PageId, u32> = pages
        .iter()
        .map(|page| (page.page_id, page.page_no))
        .collect();
    let segment_by_id: BTreeMap<SegmentId, &Segment> = segments
        .iter()
        .map(|segment| (segment.id, segment))
        .collect();
    let aligned_by_question: BTreeMap<QuestionId, &AlignedQuestion> = aligned
        .iter()
        .map(|question| (question.question_id, question))
        .collect();
    let mut answers = Vec::new();
    let mut unresolved = Vec::new();
    for question in &paper.questions {
        let Some(aligned_question) = aligned_by_question.get(&question.id) else {
            continue;
        };
        let mut per_page: BTreeMap<u32, Vec<&Segment>> = BTreeMap::new();
        for segment_id in &aligned_question.segment_ids {
            let Some(segment) = segment_by_id.get(segment_id) else {
                return Err(AlignError::Internal(format!(
                    "题目 {} 的区域 {segment_id} 不在区域列表中",
                    question.id
                )));
            };
            let Some(page_no) = page_no_by_id.get(&segment.page_id) else {
                return Err(AlignError::Internal(format!(
                    "区域 {segment_id} 的页面不在提交页列表内"
                )));
            };
            per_page.entry(*page_no).or_default().push(segment);
        }
        let mut parts: Vec<AnswerPart> = Vec::new();
        let mut confidences: Vec<Confidence> = Vec::new();
        let mut texts: Vec<String> = Vec::new();
        let mut conflict = false;
        for (page_no, mut list) in per_page {
            list.sort_by(|left, right| {
                left.bbox
                    .y
                    .partial_cmp(&right.bbox.y)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            if list.len() > 1 {
                unresolved.push(item(
                    UnresolvedKind::CrossPageConflict,
                    submission_id,
                    question.id,
                    Some(page_no),
                    "同一页存在多个区域（主段不唯一），不合并".to_owned(),
                ));
                conflict = true;
                continue;
            }
            let segment = list[0];
            let Some(recognition) = recognitions.recognition(segment.page_id) else {
                unresolved.push(item(
                    UnresolvedKind::RegionUncertain,
                    submission_id,
                    question.id,
                    Some(page_no),
                    "该页没有识别结果，答案不可用".to_owned(),
                ));
                conflict = true;
                continue;
            };
            parts.push(AnswerPart {
                page_id: segment.page_id,
                segment_id: segment.id,
                recognition_id: recognition.id,
                bbox: segment.bbox,
                text: recognition.text.clone(),
            });
            confidences.push(recognition.confidence);
            texts.push(recognition.text.clone());
        }
        if conflict || parts.is_empty() {
            continue;
        }
        let page_numbers: Vec<u32> = parts
            .iter()
            .filter_map(|part| page_no_by_id.get(&part.page_id).copied())
            .collect();
        let contiguous = page_numbers
            .windows(2)
            .all(|pair| pair[1] == pair[0].saturating_add(1));
        let cross_pages = u32::try_from(page_numbers.len()).unwrap_or(u32::MAX);
        if cross_pages > settings.max_cross_pages {
            unresolved.push(item(
                UnresolvedKind::CrossPageConflict,
                submission_id,
                question.id,
                page_numbers.first().copied(),
                format!(
                    "跨页 {} 页超过上限 {} 页，不合并",
                    cross_pages, settings.max_cross_pages
                ),
            ));
            continue;
        }
        if !contiguous {
            unresolved.push(item(
                UnresolvedKind::MissingPage,
                submission_id,
                question.id,
                page_numbers.first().copied(),
                format!("跨页缺中间页（页序 {page_numbers:?}），不合并"),
            ));
        }
        let main = parts.first().cloned();
        let merged_text = if contiguous {
            texts.join("\n")
        } else {
            String::new()
        };
        let blocks = if parts.len() == 1 {
            recognitions
                .recognition(parts[0].page_id)
                .map(|recognition| recognition.blocks.clone())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let mut minimum = Confidence::ONE;
        for confidence in confidences {
            minimum = minimum.min(confidence);
        }
        let answer = StudentAnswer {
            submission_id,
            question_id: question.id,
            recognition_id: main.as_ref().map(|part| part.recognition_id),
            segment_id: main.as_ref().map(|part| part.segment_id),
            raw: RawAnswer {
                text: merged_text,
                latex: None,
                blocks,
                source: AnswerSource::Recognition,
            },
            parts,
            recognition_confidence: minimum,
        };
        answer
            .validate()
            .map_err(|error| AlignError::Internal(format!("学生答案校验失败：{error}")))?;
        answers.push(answer);
    }
    Ok(AnswerBuildOutcome {
        answers,
        unresolved,
    })
}

fn item(
    kind: UnresolvedKind,
    submission_id: SubmissionId,
    question_id: QuestionId,
    page_no: Option<u32>,
    detail: String,
) -> UnresolvedItem {
    UnresolvedItem {
        kind,
        submission_id,
        question_id: Some(question_id),
        page_no,
        detail,
        candidates: Vec::new(),
        confidence: None,
    }
}

/// 单页答案的坐标框（供测试与下游裁片核验）。
#[must_use]
pub fn part_bbox(part: &AnswerPart) -> BBox {
    part.bbox
}
