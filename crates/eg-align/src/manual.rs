//! 人工修正（`SPEC-align.md` §3.1/§3.6：只追加、写审计、可撤销）。
//!
//! 人工路径是规范里的「最后一级」：身份人工选定恒 `1.000`（与模型置信度分开标注），
//! 区域人工画框恒 `1.000`；所有修正追加历史记录、重算门禁，并生成审计条目
//! （actor 固定 `local-teacher`，由平台审计模块写入）。

use eg_domain::{BBox, Clock, Confidence, Segment, SegmentId, StudentId, SubmissionId};

use crate::error::AlignError;
use crate::identity::{IdentityConclusion, IdentityStatus, MatchBasis};
use crate::normalize::main_number;
use crate::ports::IdPort;
use crate::report::{AlignReport, ManualRecord};
use crate::roster::Roster;

/// 人工命令。
#[derive(Debug, Clone, PartialEq)]
pub enum ManualCommand {
    /// 确认身份（名单内选择；`basis=Manual`，置信度恒 1.000）。
    ConfirmIdentity {
        /// 选定的学生。
        student_id: StudentId,
    },
    /// 人工画框补齐/替换某题在指定页的区域。
    RebindRegion {
        /// 题目 id。
        question_id: eg_domain::QuestionId,
        /// 页号（1 起）。
        page_no: u32,
        /// 归一化坐标框。
        bbox: BBox,
    },
}

impl ManualCommand {
    /// 稳定命令码（审计）。
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::ConfirmIdentity { .. } => "confirm_identity",
            Self::RebindRegion { .. } => "rebind_region",
        }
    }
}

/// 人工修正请求。
#[derive(Debug, Clone, PartialEq)]
pub struct ManualRequest {
    /// 提交 id。
    pub submission_id: SubmissionId,
    /// 命令。
    pub command: ManualCommand,
    /// 原因（可空；审计记录）。
    pub reason: Option<String>,
}

/// 人工修正结果。
#[derive(Debug, Clone)]
pub struct ManualOutcome {
    /// 修正后的报告（新对象；历史只追加）。
    pub report: AlignReport,
    /// 追加的历史记录。
    pub record: ManualRecord,
    /// 是否需要重跑对齐（区域修正后，答案需按新区域重建）。
    pub requires_realign: bool,
}

/// 应用人工命令并返回新报告（不修改传入报告）。
///
/// # Errors
///
/// 提交不匹配、学生不在名单、题目未知、页不存在、坐标非法、id 生成失败时返回
/// [`AlignError::InvalidManual`]。
pub fn apply_manual(
    report: &AlignReport,
    request: &ManualRequest,
    roster: &Roster,
    ids: &dyn IdPort,
    clock: &dyn Clock,
) -> Result<ManualOutcome, AlignError> {
    if report.submission_id != request.submission_id {
        return Err(AlignError::InvalidManual(format!(
            "提交不匹配：报告 {}，请求 {}",
            report.submission_id, request.submission_id
        )));
    }
    let before_hash = report.fingerprint.output_hash;
    let mut next = report.clone();
    let requires_realign = match &request.command {
        ManualCommand::ConfirmIdentity { student_id } => {
            let Some(student) = roster.get(*student_id) else {
                return Err(AlignError::InvalidManual(format!(
                    "学生 {student_id} 不在名单内"
                )));
            };
            let evidence = next.identity.evidence.clone();
            next.identity = IdentityConclusion {
                submission_id: next.identity.submission_id,
                status: IdentityStatus::Confirmed {
                    student_id: student.id,
                    basis: MatchBasis::Manual,
                    confidence: Confidence::ONE,
                },
                source: next.identity.source,
                evidence,
            };
            next.unresolved.retain(|item| {
                !matches!(
                    item.kind,
                    crate::report::UnresolvedKind::FileNameNotFound
                        | crate::report::UnresolvedKind::IdentityAmbiguous
                        | crate::report::UnresolvedKind::IdentityConflict
                        | crate::report::UnresolvedKind::OutsideRoster
                )
            });
            false
        }
        ManualCommand::RebindRegion {
            question_id,
            page_no,
            bbox,
        } => {
            bbox.validate()
                .map_err(|error| AlignError::InvalidManual(format!("人工区域坐标非法：{error}")))?;
            if !next
                .question_summaries
                .iter()
                .any(|summary| summary.question_id == *question_id)
            {
                return Err(AlignError::InvalidManual(format!(
                    "题目 {question_id} 不在本报告的试卷中"
                )));
            }
            let Some(page) = next.pages.iter().find(|page| page.page_no == *page_no) else {
                return Err(AlignError::InvalidManual(format!(
                    "页号 {page_no} 不在提交页列表内"
                )));
            };
            // 替换该题在该页的旧区域（保持主段唯一）。
            next.segments.retain(|segment| {
                !(segment.question_id == Some(*question_id) && segment.page_id == page.page_id)
            });
            let segment_id = ids.next_segment_id()?;
            next.segments.push(Segment {
                id: segment_id,
                page_id: page.page_id,
                question_id: Some(*question_id),
                bbox: *bbox,
                crop: page.asset,
                method: eg_domain::SegmentMethod::Manual,
            });
            next.unresolved.retain(|item| {
                !(item.question_id == Some(*question_id)
                    && item.kind == crate::report::UnresolvedKind::RegionUncertain)
            });
            // 答案需按新区域重建（本函数不做识别/裁剪，标记重跑）。
            next.answers
                .retain(|answer| answer.question_id != *question_id);
            if let Some(summary) = next
                .question_summaries
                .iter_mut()
                .find(|summary| summary.question_id == *question_id)
            {
                summary.path = crate::region::RegionPath::Manual;
                summary.confidence = Confidence::ONE;
                summary.pages = vec![*page_no];
                summary.segment_ids = vec![segment_id];
                summary.main_number = main_number(&summary.label).unwrap_or(summary.main_number);
            }
            true
        }
    };
    next.refresh_gates(next.region_confidence_floor);
    let after_hash = crate::pipeline::output_hash(&next)?;
    next.fingerprint.output_hash = after_hash;
    let record = ManualRecord {
        command: request.command.code(),
        before_hash,
        after_hash,
        actor: "local-teacher".to_owned(),
        at: clock.now(),
        reason: request.reason.clone(),
    };
    next.manual_history.push(record.clone());
    Ok(ManualOutcome {
        report: next,
        record,
        requires_realign,
    })
}

/// 生成人工修正审计条目（调用方用平台审计模块追加，保证审计链）。
#[must_use]
pub fn manual_audit_entry(
    request: &ManualRequest,
    outcome: &ManualOutcome,
) -> eg_platform::NewAuditEntry {
    let note = format!(
        "对齐人工修正：{}；原因={}；输出哈希 {} → {}",
        outcome.record.command,
        outcome.record.reason.as_deref().unwrap_or("（未填写）"),
        outcome.record.before_hash.to_hex(),
        outcome.record.after_hash.to_hex()
    );
    eg_platform::NewAuditEntry::new(
        "align.manual_correction",
        "submission",
        request.submission_id.to_string(),
    )
    .with_note(note)
    .with_hashes(
        Some(outcome.record.before_hash),
        Some(outcome.record.after_hash),
    )
}

/// 人工区域修正后的区域 id（供审计/复核回查）。
#[must_use]
pub fn manual_segment_ids(
    report: &AlignReport,
    question_id: eg_domain::QuestionId,
) -> Vec<SegmentId> {
    report
        .question_summaries
        .iter()
        .filter(|summary| summary.question_id == question_id)
        .flat_map(|summary| summary.segment_ids.clone())
        .collect()
}
