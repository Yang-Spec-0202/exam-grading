//! 对齐报告、未决项、指纹（`SPEC-align.md` §3.4/§3.5/§3.6）。
//!
//! 报告是 `grade`/`review`/`app` 的唯一消费口径：身份结论、逐题区域结论、学生答案、
//! 未决项、门禁结论、指纹。所有未决项都进 `unresolved`，不在日志里散落。

use eg_domain::{
    ClassroomId, Confidence, PaperId, QuestionId, Segment, SegmentId, Sha256Hex, StudentAnswer,
    StudentId, SubmissionId, Timestamp,
};

use crate::dedupe::DuplicateVerdict;
use crate::identity::{IdentityCandidate, IdentityConclusion};
use crate::ports::PageRef;
use crate::region::{AlignedQuestion, RegionPath};
use crate::settings::IdentitySourceKind;

/// 未决项种类（`SPEC-align.md` §3.4，一一对应）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnresolvedKind {
    /// 身份歧义（重名/多候选，分差不足）。
    IdentityAmbiguous,
    /// 文件名未命中（默认走人工选择）。
    FileNameNotFound,
    /// 身份冲突（文件名与已启用的 OCR 兜底指向不同学生）。
    IdentityConflict,
    /// 分组可疑（多文件 stem 相同但页序不可解析）。
    GroupingSuspicious,
    /// 区域不确定（低于门限、锚点歧义、重叠、无识别等）。
    RegionUncertain,
    /// 缺页（模板页数/题号覆盖缺口）。
    MissingPage,
    /// 多页（页数超过模板页数）。
    MultiPage,
    /// 重复提交（同学生同卷已有提交）。
    DuplicateSubmission,
    /// 跨页冲突（超过 `align.max_cross_pages` 或主段不唯一）。
    CrossPageConflict,
    /// 名单外（`identity.unmatched_behavior=自动归入名单外`）。
    OutsideRoster,
}

impl UnresolvedKind {
    /// 稳定字符串（报告/审计/日志用）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::IdentityAmbiguous => "identity_ambiguous",
            Self::FileNameNotFound => "file_name_not_found",
            Self::IdentityConflict => "identity_conflict",
            Self::GroupingSuspicious => "grouping_suspicious",
            Self::RegionUncertain => "region_uncertain",
            Self::MissingPage => "missing_page",
            Self::MultiPage => "multi_page",
            Self::DuplicateSubmission => "duplicate_submission",
            Self::CrossPageConflict => "cross_page_conflict",
            Self::OutsideRoster => "outside_roster",
        }
    }

    /// 是否阻断自动判分（`SPEC-align.md` §3.5 门禁）。
    #[must_use]
    pub const fn blocks_auto_grade(self) -> bool {
        !matches!(self, Self::MultiPage)
    }
}

/// 一条未决项（可序列化、可审计；候选已脱敏）。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct UnresolvedItem {
    /// 种类。
    pub kind: UnresolvedKind,
    /// 提交 id。
    pub submission_id: SubmissionId,
    /// 相关题目（有则定位到题）。
    pub question_id: Option<QuestionId>,
    /// 相关页号（有则定位到页）。
    pub page_no: Option<u32>,
    /// 教师可读的中文说明。
    pub detail: String,
    /// 身份候选（脱敏；非身份类未决为空）。
    pub candidates: Vec<IdentityCandidate>,
    /// 相关置信度（有则展示）。
    pub confidence: Option<Confidence>,
}

/// 逐题区域摘要（进报告与门禁）。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct QuestionRegionSummary {
    /// 题目 id。
    pub question_id: QuestionId,
    /// 卷面序号。
    pub ordinal: u32,
    /// 题目标签。
    pub label: String,
    /// 题号主号。
    pub main_number: u32,
    /// 实际路径。
    pub path: RegionPath,
    /// 区域置信度。
    pub confidence: Confidence,
    /// 覆盖页号（升序）。
    pub pages: Vec<u32>,
    /// 区域 id。
    pub segment_ids: Vec<SegmentId>,
    /// 该题是否允许自动判分（置信度达门限且无阻断未决）。
    pub auto_grade_allowed: bool,
}

/// 报告指标（`SPEC-align.md` §6；脱敏，不含文件名/姓名/学号原文）。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct AlignMetrics {
    /// 身份来源。
    pub identity_source: IdentitySourceKind,
    /// 身份是否确认。
    pub identity_confirmed: bool,
    /// 页数。
    pub pages: u32,
    /// 题数。
    pub questions: u32,
    /// 已对齐题数。
    pub aligned_questions: u32,
    /// 区域覆盖率（已对齐题 / 总题数）。
    pub region_coverage: f64,
    /// 未决项数。
    pub unresolved: u32,
    /// OCR 兜底端口调用次数（默认配置恒 0）。
    pub ocr_hint_calls: u32,
    /// 是否命中重复提交。
    pub duplicate: bool,
    /// 是否使用模板路径。
    pub template_used: bool,
}

/// 对齐指纹（`SPEC-align.md` §3.6；供 `grade` 复现与审计核对）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AlignFingerprint {
    /// 算法版本。
    pub algorithm_version: String,
    /// 名单哈希。
    pub roster_hash: Sha256Hex,
    /// 模板版本（未用模板为 `None`）。
    pub template_version: Option<u32>,
    /// 模板哈希（未用模板为 `None`）。
    pub template_hash: Option<Sha256Hex>,
    /// 输入哈希（页/识别/配置/试卷）。
    pub input_hash: Sha256Hex,
    /// 输出哈希（去生成 id 后的语义输出）。
    pub output_hash: Sha256Hex,
    /// 生成时间。
    pub generated_at: Timestamp,
}

/// 人工修正历史条目（只追加；撤销即追加反向记录，不删除历史）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ManualRecord {
    /// 命令类型（稳定码）。
    pub command: &'static str,
    /// 修正前输出哈希。
    pub before_hash: Sha256Hex,
    /// 修正后输出哈希。
    pub after_hash: Sha256Hex,
    /// 操作者（固定 `local-teacher`）。
    pub actor: String,
    /// 时间。
    pub at: Timestamp,
    /// 原因（可空）。
    pub reason: Option<String>,
}

/// 对齐报告（`align` 的唯一产物）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct AlignReport {
    /// 提交 id。
    pub submission_id: SubmissionId,
    /// 试卷 id。
    pub paper_id: PaperId,
    /// 班级 id。
    pub classroom_id: ClassroomId,
    /// 身份结论。
    pub identity: IdentityConclusion,
    /// 重复提交判定。
    pub duplicate: DuplicateVerdict,
    /// 页投影（页号/资产；供指纹与下游裁片核验）。
    pub pages: Vec<PageRef>,
    /// 区域列表。
    pub segments: Vec<Segment>,
    /// 逐题区域摘要。
    pub question_summaries: Vec<QuestionRegionSummary>,
    /// 学生答案（只为可判定的题构造）。
    pub answers: Vec<StudentAnswer>,
    /// 未决项。
    pub unresolved: Vec<UnresolvedItem>,
    /// 模板被禁用原因（`None` = 未降级）。
    pub template_disabled: Option<String>,
    /// 是否允许自动判分（`SPEC-align.md` §3.5 三条件同时成立）。
    pub auto_grade_ready: bool,
    /// 指标。
    pub metrics: AlignMetrics,
    /// 区域可用下限（门禁复算用，来自配置快照）。
    pub region_confidence_floor: f64,
    /// 人工修正历史。
    pub manual_history: Vec<ManualRecord>,
    /// 指纹。
    pub fingerprint: AlignFingerprint,
}

impl AlignReport {
    /// 身份已确认的学生 id。
    #[must_use]
    pub fn student_id(&self) -> Option<StudentId> {
        self.identity.status.student_id()
    }

    /// 指定题目是否允许自动判分（门禁逐题口径）。
    #[must_use]
    pub fn question_allows_auto_grade(&self, question_id: QuestionId) -> bool {
        self.question_summaries
            .iter()
            .any(|summary| summary.question_id == question_id && summary.auto_grade_allowed)
    }

    /// 阻断自动判分的未决项（`review` 优先处理）。
    #[must_use]
    pub fn blocking_unresolved(&self) -> Vec<&UnresolvedItem> {
        self.unresolved
            .iter()
            .filter(|item| item.kind.blocks_auto_grade())
            .collect()
    }

    /// 重建区域摘要的门禁标记（人工修正/降级后复用）。
    ///
    /// # Errors
    ///
    /// 不返回错误；保持纯函数形态便于审计重放。
    pub fn refresh_gates(&mut self, region_confidence_floor: f64) {
        for summary in &mut self.question_summaries {
            let blocking = self.unresolved.iter().any(|item| {
                item.question_id == Some(summary.question_id) && item.kind.blocks_auto_grade()
            });
            summary.auto_grade_allowed =
                summary.confidence.get() >= region_confidence_floor && !blocking;
        }
        self.auto_grade_ready = self.identity.status.is_confirmed()
            && matches!(self.duplicate, DuplicateVerdict::New)
            && self
                .question_summaries
                .iter()
                .all(|summary| summary.auto_grade_allowed)
            && !self
                .unresolved
                .iter()
                .any(|item| item.kind.blocks_auto_grade());
    }
}

/// 把区域结论投影为报告摘要（`aligned` 已按试卷顺序）。
#[must_use]
pub fn summaries_from_aligned(
    aligned: &[AlignedQuestion],
    unresolved: &[UnresolvedItem],
    floor: f64,
) -> Vec<QuestionRegionSummary> {
    aligned
        .iter()
        .map(|question| {
            let blocking = unresolved.iter().any(|item| {
                item.question_id == Some(question.question_id) && item.kind.blocks_auto_grade()
            });
            QuestionRegionSummary {
                question_id: question.question_id,
                ordinal: question.ordinal,
                label: question.label.clone(),
                main_number: question.main_number,
                path: question.path,
                confidence: question.confidence,
                pages: question.pages.clone(),
                segment_ids: question.segment_ids.clone(),
                auto_grade_allowed: question.confidence.get() >= floor && !blocking,
            }
        })
        .collect()
}
