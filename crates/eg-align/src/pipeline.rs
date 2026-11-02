//! 对齐流水线（`SPEC-align.md` §4「页分组 → 文件名校验 →（兜底）→ 冲突 → 区域 → 答案 → 门禁」）。
//!
//! 幂等与可复现：同一输入 + 同一算法版本 + 同一配置快照 → 输入/输出哈希相同
//! （生成 id 属存储坐标，不参与语义输出哈希；见 `docs/dev/research/align.md` §5）。

use eg_domain::{Clock, Paper, PaperTemplate, SegmentMethod, Sha256Hex, Submission};

use crate::ALGORITHM_VERSION;
use crate::crosspage::build_answers;
use crate::dedupe::{DuplicateVerdict, ExistingSubmission, check_duplicate};
use crate::error::AlignError;
use crate::identity::{IdentityConclusion, IdentityHintSource, IdentityStatus, resolve_identity};
use crate::normalize::{normalize_key, page_suffix};
use crate::ports::{IdPort, PageRef, PageSource, RecognitionSource, SubmissionPages};
use crate::region::align_regions;
use crate::report::{
    AlignFingerprint, AlignMetrics, AlignReport, QuestionRegionSummary, UnresolvedItem,
    UnresolvedKind, summaries_from_aligned,
};
use crate::roster::Roster;
use crate::settings::AlignSettings;

/// 对齐请求（只读输入 + 注入端口）。
pub struct AlignRequest<'a> {
    /// 提交（capture 产物）。
    pub submission: &'a Submission,
    /// 试卷（含题号与答案键版本）。
    pub paper: &'a Paper,
    /// 班级名单。
    pub roster: &'a Roster,
    /// 试卷模板（`None` = 无模板，直接走版面/文本路径）。
    pub template: Option<&'a PaperTemplate>,
    /// 页投影（与提交页一致，按页号升序或无序均可）。
    pub pages: &'a [PageRef],
    /// 页级识别结果。
    pub recognitions: &'a dyn RecognitionSource,
    /// OCR 兜底提示（`identity.ocr_fallback=关` 时不会被调用）。
    pub hints: &'a dyn IdentityHintSource,
    /// 配置快照。
    pub settings: &'a AlignSettings,
    /// 区域 id 生成端口。
    pub ids: &'a dyn IdPort,
    /// 时钟（写指纹时间）。
    pub clock: &'a dyn Clock,
    /// 同学生同卷的既有提交（重复检测）。
    pub existing: &'a [ExistingSubmission],
}

/// 执行一份提交的对齐。
///
/// # Errors
///
/// 配置非法、名单/试卷不可用、输入不一致、id 生成失败、域校验失败时返回 [`AlignError`]。
pub fn align(request: AlignRequest<'_>) -> Result<AlignReport, AlignError> {
    request.settings.validate()?;
    request
        .submission
        .validate()
        .map_err(|error| AlignError::InputMismatch(format!("提交不可用：{error}")))?;
    request
        .paper
        .validate()
        .map_err(|error| AlignError::Template(format!("试卷不可用：{error}")))?;
    let submission_pages = SubmissionPages::from_pages(request.pages.to_vec())?;
    if submission_pages.pages().len() != request.submission.pages.len() {
        return Err(AlignError::InputMismatch(format!(
            "页投影数 {} 与提交页数 {} 不一致",
            submission_pages.pages().len(),
            request.submission.pages.len()
        )));
    }
    let pages = submission_pages.pages();
    let identity = resolve_identity(
        request.submission,
        request.roster,
        request.settings,
        request.hints,
    )?;
    let duplicate = check_duplicate(request.submission, request.existing);
    let region = align_regions(
        request.submission.id,
        request.paper,
        request.template,
        pages,
        request.recognitions,
        request.settings,
        request.ids,
    )?;
    let answers = build_answers(
        request.submission.id,
        request.paper,
        &region.aligned,
        &region.segments,
        pages,
        request.recognitions,
        request.settings,
    )?;

    let mut unresolved: Vec<UnresolvedItem> = Vec::new();
    if let Some(item) = identity_unresolved(&identity) {
        unresolved.push(item);
    }
    match duplicate {
        DuplicateVerdict::New => {}
        DuplicateVerdict::Identical { first } => unresolved.push(UnresolvedItem {
            kind: UnresolvedKind::DuplicateSubmission,
            submission_id: request.submission.id,
            question_id: None,
            page_no: None,
            detail: format!("与既有提交 {first} 全页内容相同（复用首份或人工确认）"),
            candidates: Vec::new(),
            confidence: None,
        }),
        DuplicateVerdict::PartialOverlap { first } => unresolved.push(UnresolvedItem {
            kind: UnresolvedKind::DuplicateSubmission,
            submission_id: request.submission.id,
            question_id: None,
            page_no: None,
            detail: format!("与既有提交 {first} 部分页面重叠（不自动去重）"),
            candidates: Vec::new(),
            confidence: None,
        }),
    }
    unresolved.extend(region.unresolved.iter().cloned());
    unresolved.extend(answers.unresolved.iter().cloned());

    let mut question_summaries = summaries_from_aligned(
        &region.aligned,
        &unresolved,
        request.settings.region_confidence_floor,
    );
    // 未对齐的题目也进摘要（`auto_grade_allowed=false`），保证「丢题可见」。
    for question in &request.paper.questions {
        if question_summaries
            .iter()
            .any(|summary| summary.question_id == question.id)
        {
            continue;
        }
        question_summaries.push(QuestionRegionSummary {
            question_id: question.id,
            ordinal: question.ordinal,
            label: question.label.clone(),
            main_number: crate::normalize::main_number(&question.label).unwrap_or(0),
            path: crate::region::RegionPath::None,
            confidence: eg_domain::Confidence::ZERO,
            pages: Vec::new(),
            segment_ids: Vec::new(),
            auto_grade_allowed: false,
        });
    }
    question_summaries.sort_by_key(|summary| summary.ordinal);

    let template_hash = match request.template {
        Some(template) => Some(Sha256Hex::of(
            &serde_json::to_vec(template)
                .map_err(|error| AlignError::Internal(format!("模板序列化失败：{error}")))?,
        )),
        None => None,
    };
    let input_hash = input_hash(&request, pages, &identity, template_hash)?;
    let ocr_hint_calls = identity.evidence.hint_count;
    let mut report = AlignReport {
        submission_id: request.submission.id,
        paper_id: request.paper.id,
        classroom_id: request.submission.classroom_id,
        identity,
        duplicate,
        pages: pages.to_vec(),
        segments: region.segments.clone(),
        question_summaries,
        answers: answers.answers,
        unresolved,
        template_disabled: region.template_disabled.clone(),
        auto_grade_ready: false,
        metrics: AlignMetrics {
            identity_source: request.settings.identity_source,
            identity_confirmed: false,
            pages: u32::try_from(pages.len()).unwrap_or(u32::MAX),
            questions: u32::try_from(request.paper.questions.len()).unwrap_or(u32::MAX),
            aligned_questions: 0,
            region_coverage: 0.0,
            unresolved: 0,
            ocr_hint_calls,
            duplicate: !matches!(duplicate, DuplicateVerdict::New),
            template_used: region.path == crate::region::RegionPath::Template,
        },
        region_confidence_floor: request.settings.region_confidence_floor,
        manual_history: Vec::new(),
        fingerprint: AlignFingerprint {
            algorithm_version: ALGORITHM_VERSION.to_owned(),
            roster_hash: request.roster.hash(),
            template_version: request.template.map(|template| template.version),
            template_hash,
            input_hash,
            output_hash: Sha256Hex::of(b""),
            generated_at: request.clock.now(),
        },
    };
    report.refresh_gates(request.settings.region_confidence_floor);
    let aligned_questions = u32::try_from(
        report
            .question_summaries
            .iter()
            .filter(|summary| summary.auto_grade_allowed)
            .count(),
    )
    .unwrap_or(u32::MAX);
    report.metrics.identity_confirmed = report.identity.status.is_confirmed();
    report.metrics.aligned_questions = aligned_questions;
    report.metrics.region_coverage = if report.metrics.questions == 0 {
        0.0
    } else {
        f64::from(aligned_questions) / f64::from(report.metrics.questions)
    };
    report.metrics.unresolved = u32::try_from(report.unresolved.len()).unwrap_or(u32::MAX);
    report.fingerprint.output_hash = output_hash(&report)?;
    Ok(report)
}

fn identity_unresolved(identity: &IdentityConclusion) -> Option<UnresolvedItem> {
    match &identity.status {
        IdentityStatus::Confirmed { .. } => None,
        IdentityStatus::Unresolved { kind, candidates } => Some(UnresolvedItem {
            kind: *kind,
            submission_id: identity.submission_id,
            question_id: None,
            page_no: None,
            detail: identity_unresolved_detail(*kind),
            candidates: candidates.clone(),
            confidence: candidates
                .first()
                .and_then(|candidate| candidate.confidence),
        }),
        IdentityStatus::OutsideRoster { .. } => Some(UnresolvedItem {
            kind: UnresolvedKind::OutsideRoster,
            submission_id: identity.submission_id,
            question_id: None,
            page_no: None,
            detail: "文件名为名单外学生：不参与班级统计与正式成绩，待补名单或标记无效".to_owned(),
            candidates: Vec::new(),
            confidence: None,
        }),
    }
}

fn identity_unresolved_detail(kind: UnresolvedKind) -> String {
    match kind {
        UnresolvedKind::FileNameNotFound => "文件名归一后不在名单：请从名单中人工选择".to_owned(),
        UnresolvedKind::IdentityAmbiguous => {
            "存在多个身份候选（重名或分差不足）：请人工确认".to_owned()
        }
        UnresolvedKind::IdentityConflict => {
            "文件名与 OCR 兜底指向不同学生：请人工确认（不静默择优）".to_owned()
        }
        UnresolvedKind::OutsideRoster => "名单外学生".to_owned(),
        _ => "身份未决".to_owned(),
    }
}

#[derive(serde::Serialize)]
struct CanonicalInput<'a> {
    algorithm_version: &'a str,
    settings_hash: Sha256Hex,
    roster_hash: Sha256Hex,
    submission_id: String,
    stem_hash: Sha256Hex,
    page_suffix: Option<u32>,
    page_assets: Vec<String>,
    page_numbers: Vec<u32>,
    paper_id: String,
    key_version: u32,
    question_ids: Vec<String>,
    question_labels: Vec<String>,
    template_version: Option<u32>,
    template_hash: Option<Sha256Hex>,
    recognitions: Vec<CanonicalRecognition>,
}

#[derive(serde::Serialize)]
struct CanonicalRecognition {
    page_id: String,
    recognition_id: String,
    confidence: f64,
    blocks_hash: Sha256Hex,
    text_hash: Sha256Hex,
}

fn input_hash(
    request: &AlignRequest<'_>,
    pages: &[PageRef],
    identity: &IdentityConclusion,
    template_hash: Option<Sha256Hex>,
) -> Result<Sha256Hex, AlignError> {
    let mut recognitions = Vec::new();
    for page in pages {
        let Some(recognition) = request.recognitions.recognition(page.page_id) else {
            continue;
        };
        let blocks_hash = Sha256Hex::of(
            &serde_json::to_vec(&recognition.blocks)
                .map_err(|error| AlignError::Internal(format!("识别块序列化失败：{error}")))?,
        );
        recognitions.push(CanonicalRecognition {
            page_id: page.page_id.to_string(),
            recognition_id: recognition.id.to_string(),
            confidence: recognition.confidence.get(),
            blocks_hash,
            text_hash: Sha256Hex::of(recognition.text.as_bytes()),
        });
    }
    let canonical = CanonicalInput {
        algorithm_version: ALGORITHM_VERSION,
        settings_hash: request.settings.options_hash(),
        roster_hash: request.roster.hash(),
        submission_id: request.submission.id.to_string(),
        stem_hash: identity.evidence.stem_hash,
        page_suffix: page_suffix(&request.submission.source_stem).1,
        page_assets: pages.iter().map(|page| page.asset.to_hex()).collect(),
        page_numbers: pages.iter().map(|page| page.page_no).collect(),
        paper_id: request.paper.id.to_string(),
        key_version: request.paper.key_version,
        question_ids: request
            .paper
            .questions
            .iter()
            .map(|question| question.id.to_string())
            .collect(),
        question_labels: request
            .paper
            .questions
            .iter()
            .map(|question| normalize_key(&question.label))
            .collect(),
        template_version: request.template.map(|template| template.version),
        template_hash,
        recognitions,
    };
    Ok(Sha256Hex::of(&serde_json::to_vec(&canonical).map_err(
        |error| AlignError::Internal(format!("输入指纹序列化失败：{error}")),
    )?))
}

#[derive(serde::Serialize)]
struct CanonicalOutput {
    identity: IdentityStatus,
    duplicate: DuplicateVerdict,
    template_disabled: Option<String>,
    template_used: bool,
    auto_grade_ready: bool,
    segments: Vec<CanonicalSegment>,
    summaries: Vec<CanonicalSummary>,
    answers: Vec<CanonicalAnswer>,
    unresolved: Vec<CanonicalUnresolved>,
    manual_history: Vec<String>,
}

#[derive(serde::Serialize)]
struct CanonicalSegment {
    page_no: u32,
    question_id: String,
    bbox: [f64; 4],
    method: &'static str,
}

#[derive(serde::Serialize)]
struct CanonicalSummary {
    question_id: String,
    confidence: f64,
    path: &'static str,
    auto_grade_allowed: bool,
}

#[derive(serde::Serialize)]
struct CanonicalAnswer {
    question_id: String,
    text: String,
    parts: Vec<CanonicalPart>,
    recognition_confidence: f64,
}

#[derive(serde::Serialize)]
struct CanonicalPart {
    page_no: u32,
    bbox: [f64; 4],
    text: String,
}

#[derive(serde::Serialize)]
struct CanonicalUnresolved {
    kind: &'static str,
    question_id: Option<String>,
    page_no: Option<u32>,
    detail: String,
}

pub(crate) fn output_hash(report: &AlignReport) -> Result<Sha256Hex, AlignError> {
    let page_no_by_id: std::collections::BTreeMap<_, _> = report
        .pages
        .iter()
        .map(|page| (page.page_id, page.page_no))
        .collect();
    let mut segments: Vec<CanonicalSegment> = report
        .segments
        .iter()
        .map(|segment| CanonicalSegment {
            page_no: page_no_by_id.get(&segment.page_id).copied().unwrap_or(0),
            question_id: segment
                .question_id
                .map(|id| id.to_string())
                .unwrap_or_default(),
            bbox: [
                segment.bbox.x,
                segment.bbox.y,
                segment.bbox.w,
                segment.bbox.h,
            ],
            method: method_code(segment.method),
        })
        .collect();
    segments.sort_by(|left, right| {
        left.page_no
            .cmp(&right.page_no)
            .then_with(|| left.question_id.cmp(&right.question_id))
            .then_with(|| {
                left.bbox[1]
                    .partial_cmp(&right.bbox[1])
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    let summaries = report
        .question_summaries
        .iter()
        .map(|summary| CanonicalSummary {
            question_id: summary.question_id.to_string(),
            confidence: summary.confidence.get(),
            path: summary.path.as_str(),
            auto_grade_allowed: summary.auto_grade_allowed,
        })
        .collect();
    let mut answers: Vec<CanonicalAnswer> = report
        .answers
        .iter()
        .map(|answer| CanonicalAnswer {
            question_id: answer.question_id.to_string(),
            text: answer.raw.text.clone(),
            parts: answer
                .parts
                .iter()
                .map(|part| CanonicalPart {
                    page_no: page_no_by_id.get(&part.page_id).copied().unwrap_or(0),
                    bbox: [part.bbox.x, part.bbox.y, part.bbox.w, part.bbox.h],
                    text: part.text.clone(),
                })
                .collect(),
            recognition_confidence: answer.recognition_confidence.get(),
        })
        .collect();
    answers.sort_by(|left, right| left.question_id.cmp(&right.question_id));
    let mut unresolved: Vec<CanonicalUnresolved> = report
        .unresolved
        .iter()
        .map(|item| CanonicalUnresolved {
            kind: item.kind.as_str(),
            question_id: item.question_id.map(|id| id.to_string()),
            page_no: item.page_no,
            detail: item.detail.clone(),
        })
        .collect();
    unresolved.sort_by(|left, right| {
        left.kind
            .cmp(right.kind)
            .then_with(|| left.question_id.cmp(&right.question_id))
            .then_with(|| left.detail.cmp(&right.detail))
    });
    let canonical = CanonicalOutput {
        identity: report.identity.status.clone(),
        duplicate: report.duplicate,
        template_disabled: report.template_disabled.clone(),
        template_used: report.metrics.template_used,
        auto_grade_ready: report.auto_grade_ready,
        segments,
        summaries,
        answers,
        unresolved,
        manual_history: report
            .manual_history
            .iter()
            .map(|record| record.command.to_owned())
            .collect(),
    };
    Ok(Sha256Hex::of(&serde_json::to_vec(&canonical).map_err(
        |error| AlignError::Internal(format!("输出指纹序列化失败：{error}")),
    )?))
}

fn method_code(method: SegmentMethod) -> &'static str {
    match method {
        SegmentMethod::TemplateRegion => "template_region",
        SegmentMethod::LayoutDetection => "layout_detection",
        SegmentMethod::Manual => "manual",
    }
}
