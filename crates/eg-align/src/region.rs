//! 页面 → 题目区域对齐（`SPEC-align.md` §3.3：模板 → 版面 → 题号文本 → 人工）。
//!
//! 置信度公式（规范固定）：
//! - 模板区域：`1.000`（版本必须等于 `Paper.key_version`，否则禁用并降级）；
//! - 版面检测：`0.500 + 0.400 × 锚点精确率 + 0.100 × 顺序单调`，上限 `0.950`；
//! - 题号文本：同公式，按文本顺序；与版面路径共用锚点抽取，但边界按文本顺序而非几何 y；
//! - 人工：`1.000`（[`crate::manual`]）。
//!
//! 偏差（记录于 `docs/dev/research/align.md`）：`eg_domain::SegmentMethod` 只有
//! `TemplateRegion`/`LayoutDetection`/`Manual` 三个变体，没有「题号文本」变体；
//! 本模块用 [`RegionPath`] 保留真实路径，落库 `Segment.method` 统一记为
//! `LayoutDetection`（读方以本报告的 `RegionPath` 为准）。
//!
//! 裁片（`Segment.crop`）：`eg-align` 不做图像裁剪，当前复用页面资产占位
//! （内容寻址、有效资产）；实际裁片由下游按 `Segment.bbox` 生成，TODO 记录在此。

use std::collections::BTreeMap;

use eg_domain::{
    BBox, Confidence, PageId, Paper, PaperTemplate, Question, QuestionId, Segment, SegmentId,
    SegmentMethod, SubmissionId,
};

use crate::confidence_milli;
use crate::error::AlignError;
use crate::normalize::{leading_anchor_number, main_number};
use crate::ports::{IdPort, PageRef, RecognitionSource};
use crate::report::{UnresolvedItem, UnresolvedKind};
use crate::settings::AlignSettings;

/// 区域对齐实际走过的路径（模板版本漂移时可见降级）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RegionPath {
    /// 模板区域。
    Template,
    /// 版面检测（锚点 + 几何 y 边界）。
    Layout,
    /// 卷面题号文本（文本顺序）。
    TextOrder,
    /// 人工画框。
    Manual,
    /// 未对齐（无可用路径）。
    None,
}

impl RegionPath {
    /// 稳定字符串（审计/报告用）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Template => "template",
            Self::Layout => "layout",
            Self::TextOrder => "text_order",
            Self::Manual => "manual",
            Self::None => "none",
        }
    }

    /// 与 `SegmentMethod` 的映射（domain 缺失「题号文本」变体，见模块文档）。
    #[must_use]
    pub const fn segment_method(self) -> SegmentMethod {
        match self {
            Self::Template => SegmentMethod::TemplateRegion,
            Self::Manual => SegmentMethod::Manual,
            Self::Layout | Self::TextOrder | Self::None => SegmentMethod::LayoutDetection,
        }
    }
}

/// 单题区域结论（进报告与门禁）。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct AlignedQuestion {
    /// 题目 id。
    pub question_id: QuestionId,
    /// 卷面序号（1 起）。
    pub ordinal: u32,
    /// 题目标签（如 `17(2)`）。
    pub label: String,
    /// 题号主号。
    pub main_number: u32,
    /// 区域结论置信度（已四舍五入到千分位）。
    pub confidence: Confidence,
    /// 实际路径。
    pub path: RegionPath,
    /// 覆盖的页号（升序）。
    pub pages: Vec<u32>,
    /// 区域 id（按页序）。
    pub segment_ids: Vec<SegmentId>,
}

/// 区域对齐结果。
#[derive(Debug, Clone)]
pub struct RegionOutcome {
    /// 区域列表（已通过重叠校验）。
    pub segments: Vec<Segment>,
    /// 逐题结论（只含成功对齐的题；未对齐题在 `unresolved` 中）。
    pub aligned: Vec<AlignedQuestion>,
    /// 未决项（缺页/多页/重复题号/区域不确定等）。
    pub unresolved: Vec<UnresolvedItem>,
    /// 模板被禁用的原因（`None` = 未尝试或模板可用）。
    pub template_disabled: Option<String>,
    /// 使用的路径。
    pub path: RegionPath,
}

/// 版面/文本路径的置信度（`0.5 + 0.4·精度 + 0.1·单调`，上限 0.950）。
#[must_use]
pub fn region_confidence(anchor_precision: f64, order_monotonic: f64) -> Confidence {
    let precision = anchor_precision.clamp(0.0, 1.0);
    let monotonic = order_monotonic.clamp(0.0, 1.0);
    let raw = 0.5 + 0.4 * precision + 0.1 * monotonic;
    confidence_milli(raw.min(0.950))
}

/// 执行区域对齐。
///
/// # Errors
///
/// - 模板自身非法（`PaperTemplate::validate` 失败）→ 禁用模板并降级，不返回错误；
/// - ID 生成失败 → [`AlignError`]；
/// - 内部不变量（页 id 对不上识别结果等）→ [`AlignError::Internal`]。
pub fn align_regions(
    submission_id: SubmissionId,
    paper: &Paper,
    template: Option<&PaperTemplate>,
    pages: &[PageRef],
    recognitions: &dyn RecognitionSource,
    settings: &AlignSettings,
    ids: &dyn IdPort,
) -> Result<RegionOutcome, AlignError> {
    paper
        .validate()
        .map_err(|error| AlignError::Template(format!("试卷不可用：{error}")))?;
    let questions = &paper.questions;
    let mut unresolved: Vec<UnresolvedItem> = Vec::new();
    // 页序缺口：不分路径，任何缺页都必须显式（避免把缺页当成某些题「未作答」）。
    let mut numbers: Vec<u32> = pages.iter().map(|page| page.page_no).collect();
    numbers.sort_unstable();
    for window in numbers.windows(2) {
        for missing in (window[0] + 1)..window[1] {
            unresolved.push(item(
                UnresolvedKind::MissingPage,
                submission_id,
                None,
                Some(missing),
                format!("提交缺少第 {missing} 页"),
            ));
        }
    }
    let template_disabled = template_disabled_reason(paper, template, settings);

    if template_disabled.is_none()
        && let Some(template) = template
    {
        return template_regions(submission_id, template, questions, pages, ids);
    }

    let mut outcome = layout_or_text(
        submission_id,
        questions,
        pages,
        recognitions,
        ids,
        settings,
        unresolved,
    )?;
    outcome.template_disabled = template_disabled;
    Ok(outcome)
}

fn template_disabled_reason(
    paper: &Paper,
    template: Option<&PaperTemplate>,
    settings: &AlignSettings,
) -> Option<String> {
    if !settings.template_region {
        return Some("align.template_region=关".to_owned());
    }
    let Some(template) = template else {
        return Some("未提供试卷模板".to_owned());
    };
    if let Err(error) = template.validate() {
        return Some(format!("模板不可用：{error}"));
    }
    if let Err(error) = template.ensure_auto_align_allowed(paper) {
        return Some(format!("模板版本不匹配，禁用模板路径：{error}"));
    }
    None
}

fn template_regions(
    submission_id: SubmissionId,
    template: &PaperTemplate,
    questions: &[Question],
    pages: &[PageRef],
    ids: &dyn IdPort,
) -> Result<RegionOutcome, AlignError> {
    let page_by_no: BTreeMap<u32, &PageRef> =
        pages.iter().map(|page| (page.page_no, page)).collect();
    let question_by_id: BTreeMap<QuestionId, &Question> = questions
        .iter()
        .map(|question| (question.id, question))
        .collect();
    let mut segments = Vec::with_capacity(template.regions.len());
    let mut unresolved = Vec::new();
    let mut covered: BTreeMap<QuestionId, Vec<&PageRef>> = BTreeMap::new();
    for region in &template.regions {
        let Some(question) = question_by_id.get(&region.question_id) else {
            unresolved.push(item(
                UnresolvedKind::RegionUncertain,
                submission_id,
                Some(region.question_id),
                Some(region.page_no),
                "模板区域引用了试卷中不存在的题目".to_owned(),
            ));
            continue;
        };
        let Some(page) = page_by_no.get(&region.page_no) else {
            unresolved.push(item(
                UnresolvedKind::MissingPage,
                submission_id,
                Some(question.id),
                Some(region.page_no),
                format!(
                    "模板第 {} 页缺失（提交只有 {:?} 页）",
                    region.page_no,
                    pages.iter().map(|p| p.page_no).collect::<Vec<_>>()
                ),
            ));
            continue;
        };
        segments.push(Segment {
            id: ids.next_segment_id()?,
            page_id: page.page_id,
            question_id: Some(question.id),
            bbox: region.bbox,
            crop: page.asset,
            method: SegmentMethod::TemplateRegion,
        });
        covered.entry(question.id).or_default().push(page);
    }
    // 提交多余页：不参与判分，但显式可见。
    for page in pages {
        if page.page_no > template.page_count {
            unresolved.push(item(
                UnresolvedKind::MultiPage,
                submission_id,
                None,
                Some(page.page_no),
                format!(
                    "提交第 {} 页超出模板页数 {}（多余页不参与判分）",
                    page.page_no, template.page_count
                ),
            ));
        }
    }
    // 题号覆盖缺口：模板没有该题区域。
    for question in questions {
        if !covered.contains_key(&question.id) {
            unresolved.push(item(
                UnresolvedKind::MissingPage,
                submission_id,
                Some(question.id),
                None,
                format!(
                    "模板缺少第 {} 题（{}）的区域",
                    question.ordinal, question.label
                ),
            ));
        }
    }
    check_overlaps(submission_id, &segments, pages, &mut unresolved);
    let aligned = build_aligned(
        questions,
        &segments,
        page_no_by_id(pages),
        RegionPath::Template,
        Confidence::ONE,
        &unresolved,
    );
    Ok(RegionOutcome {
        segments,
        aligned,
        unresolved,
        template_disabled: None,
        path: RegionPath::Template,
    })
}

#[allow(clippy::too_many_arguments)]
fn layout_or_text(
    submission_id: SubmissionId,
    questions: &[Question],
    pages: &[PageRef],
    recognitions: &dyn RecognitionSource,
    ids: &dyn IdPort,
    settings: &AlignSettings,
    mut unresolved: Vec<UnresolvedItem>,
) -> Result<RegionOutcome, AlignError> {
    let anchors = collect_anchors(
        submission_id,
        questions,
        pages,
        recognitions,
        &mut unresolved,
    );
    if anchors.is_empty() {
        for page in pages {
            unresolved.push(item(
                UnresolvedKind::RegionUncertain,
                submission_id,
                None,
                Some(page.page_no),
                "该页没有识别结果，无法定位题号锚点".to_owned(),
            ));
        }
        for question in questions {
            unresolved.push(item(
                UnresolvedKind::RegionUncertain,
                submission_id,
                Some(question.id),
                None,
                format!(
                    "第 {} 题（{}）没有可用锚点",
                    question.ordinal, question.label
                ),
            ));
        }
        return Ok(RegionOutcome {
            segments: Vec::new(),
            aligned: Vec::new(),
            unresolved,
            template_disabled: None,
            path: RegionPath::None,
        });
    }

    // 版面路径要求锚点沿几何 y 单调；否则转题号文本路径（忽略几何）。
    let geometric_monotonic = anchors
        .windows(2)
        .map(|pair| pair[0].page_no < pair[1].page_no || pair[0].bbox.y <= pair[1].bbox.y)
        .filter(|ok| *ok)
        .count();
    let geometric_ratio = if anchors.len() <= 1 {
        1.0
    } else {
        f64::from(u32::try_from(geometric_monotonic).unwrap_or(u32::MAX))
            / f64::from(u32::try_from(anchors.len() - 1).unwrap_or(u32::MAX))
    };
    let path = if geometric_ratio >= 1.0 {
        RegionPath::Layout
    } else {
        RegionPath::TextOrder
    };
    let precision = f64::from(u32::try_from(anchors.len()).unwrap_or(u32::MAX))
        / f64::from(u32::try_from(questions.len()).unwrap_or(u32::MAX));
    let order_monotonic = ordinal_monotonic(&anchors);
    let confidence = region_confidence(precision, order_monotonic);
    let mut segments = Vec::with_capacity(anchors.len());
    for (index, anchor) in anchors.iter().enumerate() {
        let next = next_anchor_on_same_page(&anchors, index, anchor.page_no);
        let top = anchor.bbox.y;
        let bottom = match next {
            Some(next) => next.bbox.y.min(1.0),
            None => 1.0,
        };
        let height = (bottom - top).max(0.0);
        if height <= f64::EPSILON {
            unresolved.push(item(
                UnresolvedKind::RegionUncertain,
                submission_id,
                Some(anchor.question_id),
                Some(anchor.page_no),
                "锚点到下一锚点之间没有可用区域（高度为 0）".to_owned(),
            ));
            continue;
        }
        let bbox = BBox {
            x: 0.0,
            y: top,
            w: 1.0,
            h: height.min(1.0 - top),
        };
        if bbox.validate().is_err() {
            unresolved.push(item(
                UnresolvedKind::RegionUncertain,
                submission_id,
                Some(anchor.question_id),
                Some(anchor.page_no),
                "锚点区域坐标非法（越界）".to_owned(),
            ));
            continue;
        }
        segments.push(Segment {
            id: ids.next_segment_id()?,
            page_id: anchor.page_id,
            question_id: Some(anchor.question_id),
            bbox,
            crop: anchor.page_asset,
            method: path.segment_method(),
        });
    }
    // 没有锚点的题必须显式可见（不得表现为「未作答」）。
    for question in questions {
        if unresolved
            .iter()
            .any(|item| item.question_id == Some(question.id))
            || segments
                .iter()
                .any(|segment| segment.question_id == Some(question.id))
        {
            continue;
        }
        unresolved.push(item(
            UnresolvedKind::RegionUncertain,
            submission_id,
            Some(question.id),
            None,
            format!(
                "第 {} 题（{}）未找到题号锚点，无法定位区域",
                question.ordinal, question.label
            ),
        ));
    }
    check_overlaps(submission_id, &segments, pages, &mut unresolved);
    let mut aligned = build_aligned(
        questions,
        &segments,
        page_no_by_id(pages),
        path,
        confidence,
        &unresolved,
    );
    // 低于门限的题：区域保留（供教师查看）但门禁必须拦截。
    for aligned_question in &mut aligned {
        if aligned_question.confidence.get() < settings.region_confidence_floor {
            unresolved.push(item(
                UnresolvedKind::RegionUncertain,
                submission_id,
                Some(aligned_question.question_id),
                aligned_question.pages.first().copied(),
                format!(
                    "区域置信度 {:.3} 低于下限 {:.2}",
                    aligned_question.confidence.get(),
                    settings.region_confidence_floor
                ),
            ));
        }
    }
    Ok(RegionOutcome {
        segments,
        aligned,
        unresolved,
        template_disabled: None,
        path,
    })
}

#[derive(Debug, Clone, Copy)]
struct Anchor {
    question_id: QuestionId,
    ordinal: u32,
    page_id: PageId,
    page_no: u32,
    page_asset: eg_domain::AssetId,
    bbox: BBox,
}

fn collect_anchors(
    submission_id: SubmissionId,
    questions: &[Question],
    pages: &[PageRef],
    recognitions: &dyn RecognitionSource,
    unresolved: &mut Vec<UnresolvedItem>,
) -> Vec<Anchor> {
    // 主号 → 题目；同一主号对应多题（如 17(1)/17(2)）时锚点有歧义，按规范记重复题号。
    let mut by_main: BTreeMap<u32, Vec<&Question>> = BTreeMap::new();
    for question in questions {
        if let Some(number) = main_number(&question.label) {
            by_main.entry(number).or_default().push(question);
        }
    }
    let mut duplicate_numbers: Vec<u32> = by_main
        .iter()
        .filter(|(_, list)| list.len() > 1)
        .map(|(number, _)| *number)
        .collect();
    duplicate_numbers.sort_unstable();
    let mut reported_duplicates: Vec<u32> = Vec::new();
    let mut anchors: Vec<Anchor> = Vec::new();
    for page in pages {
        let Some(recognition) = recognitions.recognition(page.page_id) else {
            continue;
        };
        for block in &recognition.blocks {
            let Some(number) = leading_anchor_number(&block.text) else {
                continue;
            };
            if duplicate_numbers.contains(&number) {
                if !reported_duplicates.contains(&number) {
                    reported_duplicates.push(number);
                    unresolved.push(item(
                        UnresolvedKind::RegionUncertain,
                        submission_id,
                        None,
                        Some(page.page_no),
                        format!("题号 {number} 在试卷中重复（同一主号多题），锚点有歧义"),
                    ));
                }
                continue;
            }
            let Some(list) = by_main.get(&number) else {
                continue;
            };
            let Some(question) = list.first().copied() else {
                continue;
            };
            // 同页重复锚点会让主段不唯一；跨页重复是合法的跨页作答（保留每页一段）。
            if anchors
                .iter()
                .any(|anchor| anchor.question_id == question.id && anchor.page_no == page.page_no)
            {
                unresolved.push(item(
                    UnresolvedKind::CrossPageConflict,
                    submission_id,
                    Some(question.id),
                    Some(page.page_no),
                    "同一页出现多个该题锚点（主段不唯一）".to_owned(),
                ));
                continue;
            }
            anchors.push(Anchor {
                question_id: question.id,
                ordinal: question.ordinal,
                page_id: page.page_id,
                page_no: page.page_no,
                page_asset: page.asset,
                bbox: block.bbox,
            });
        }
    }
    anchors
}

fn next_anchor_on_same_page(anchors: &[Anchor], index: usize, page_no: u32) -> Option<&Anchor> {
    anchors[index + 1..]
        .iter()
        .find(|anchor| anchor.page_no == page_no)
}

fn ordinal_monotonic(anchors: &[Anchor]) -> f64 {
    if anchors.len() <= 1 {
        return 1.0;
    }
    let pairs = anchors.windows(2).count();
    let ok = anchors
        .windows(2)
        .filter(|pair| pair[0].ordinal < pair[1].ordinal)
        .count();
    f64::from(u32::try_from(ok).unwrap_or(u32::MAX))
        / f64::from(u32::try_from(pairs).unwrap_or(u32::MAX))
}

fn check_overlaps(
    submission_id: SubmissionId,
    segments: &[Segment],
    pages: &[PageRef],
    unresolved: &mut Vec<UnresolvedItem>,
) {
    let page_no: BTreeMap<PageId, u32> = pages
        .iter()
        .map(|page| (page.page_id, page.page_no))
        .collect();
    for (index, left) in segments.iter().enumerate() {
        for right in &segments[index + 1..] {
            if left.page_id != right.page_id {
                continue;
            }
            let ratio = overlap_ratio(&left.bbox, &right.bbox);
            if ratio > 0.20 {
                for question_id in [left.question_id, right.question_id] {
                    unresolved.push(item(
                        UnresolvedKind::RegionUncertain,
                        submission_id,
                        question_id,
                        page_no.get(&left.page_id).copied(),
                        format!("区域相互重叠 {:.0}%（上限 20%），该页不猜", ratio * 100.0),
                    ));
                }
            }
        }
    }
}

fn overlap_ratio(left: &BBox, right: &BBox) -> f64 {
    let x1 = left.x.max(right.x);
    let y1 = left.y.max(right.y);
    let x2 = left.right().min(right.right());
    let y2 = left.bottom().min(right.bottom());
    let width = (x2 - x1).max(0.0);
    let height = (y2 - y1).max(0.0);
    let intersection = width * height;
    let min_area = (left.w * left.h).min(right.w * right.h);
    if min_area <= f64::EPSILON {
        0.0
    } else {
        intersection / min_area
    }
}

fn page_no_by_id(pages: &[PageRef]) -> BTreeMap<PageId, u32> {
    pages
        .iter()
        .map(|page| (page.page_id, page.page_no))
        .collect()
}

fn build_aligned(
    questions: &[Question],
    segments: &[Segment],
    page_numbers: BTreeMap<PageId, u32>,
    path: RegionPath,
    confidence: Confidence,
    unresolved: &[UnresolvedItem],
) -> Vec<AlignedQuestion> {
    let mut per_question: BTreeMap<QuestionId, Vec<&Segment>> = BTreeMap::new();
    for segment in segments {
        if let Some(question_id) = segment.question_id {
            per_question.entry(question_id).or_default().push(segment);
        }
    }
    let mut aligned = Vec::new();
    for question in questions {
        let Some(list) = per_question.get(&question.id) else {
            continue;
        };
        if unresolved.iter().any(|item| {
            item.question_id == Some(question.id)
                && matches!(item.kind, UnresolvedKind::RegionUncertain)
        }) {
            continue;
        }
        let mut sorted: Vec<&Segment> = list.clone();
        sorted.sort_by(|left, right| {
            page_numbers
                .get(&left.page_id)
                .cmp(&page_numbers.get(&right.page_id))
                .then_with(|| {
                    left.bbox
                        .y
                        .partial_cmp(&right.bbox.y)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });
        let mut pages: Vec<u32> = Vec::new();
        let mut segment_ids = Vec::new();
        for segment in &sorted {
            if let Some(page_no) = page_numbers.get(&segment.page_id)
                && !pages.contains(page_no)
            {
                pages.push(*page_no);
            }
            segment_ids.push(segment.id);
        }
        let Some(number) = main_number(&question.label) else {
            continue;
        };
        aligned.push(AlignedQuestion {
            question_id: question.id,
            ordinal: question.ordinal,
            label: question.label.clone(),
            main_number: number,
            confidence,
            path,
            pages,
            segment_ids,
        });
    }
    aligned
}

fn item(
    kind: UnresolvedKind,
    submission_id: SubmissionId,
    question_id: Option<QuestionId>,
    page_no: Option<u32>,
    detail: String,
) -> UnresolvedItem {
    UnresolvedItem {
        kind,
        submission_id,
        question_id,
        page_no,
        detail,
        candidates: Vec::new(),
        confidence: None,
    }
}
