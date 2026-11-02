//! `eg-platform` 接入（`SPEC-platform.md`：配置快照、名单仓储、ULID、审计）。
//!
//! 本模块是 `eg-align` 唯一直接触碰平台类型的地方；核心算法只依赖 [`crate::ports`]。
//! 回读名单走 `repo::list_students`，人工修正审计走 `AuditLog::append`

use eg_domain::{ClassroomId, SegmentId};
use eg_platform::{AuditEntry, Platform, UlidFactory, repo};

use crate::error::AlignError;
use crate::manual::{ManualOutcome, ManualRequest, manual_audit_entry};
use crate::ports::IdPort;
use crate::report::AlignReport;
use crate::roster::Roster;
use crate::settings::AlignSettings;

/// 平台 ULID 生成器适配为 [`IdPort`]（生产路径；测试用 [`crate::ports::SeqIds`]）。
#[derive(Debug)]
pub struct PlatformIds<'a> {
    factory: &'a UlidFactory,
}

impl<'a> PlatformIds<'a> {
    /// 借用平台的 id 生成器。
    #[must_use]
    pub fn new(factory: &'a UlidFactory) -> Self {
        Self { factory }
    }
}

impl IdPort for PlatformIds<'_> {
    fn next_segment_id(&self) -> Result<SegmentId, AlignError> {
        self.factory
            .next_segment_id()
            .map_err(|error| AlignError::Internal(format!("ULID 生成失败：{error}")))
    }
}

/// 从平台配置快照读取 `identity.*`/`align.*` 并校验。
///
/// # Errors
///
/// 越界/不支持的配置返回 [`AlignError::InvalidSettings`] 或 [`AlignError::NotYetImplemented`]。
pub fn settings_from_platform(platform: &Platform) -> Result<AlignSettings, AlignError> {
    let settings = AlignSettings::from_platform_config(platform.config());
    settings.validate()?;
    Ok(settings)
}

/// 读取班级名单（`repo::list_students`）。
///
/// # Errors
///
/// 查询失败 → [`AlignError::Roster`]；名单不可用（空/重复学号/空姓名）→ [`AlignError::Roster`]。
pub fn load_roster(platform: &Platform, classroom_id: ClassroomId) -> Result<Roster, AlignError> {
    let students = platform
        .db()
        .read(move |conn| repo::list_students(conn, classroom_id))
        .map_err(|error| AlignError::Roster(format!("读取名单失败：{error}")))?;
    Roster::new(classroom_id, students)
}

/// 把人工修正审计写入平台审计链（actor 由平台固定为 `local-teacher`）。
///
/// # Errors
///
/// 审计写入失败 → [`AlignError::Audit`]（可整体重试，人工修正本身是纯计算）。
pub fn write_manual_audit(
    platform: &Platform,
    request: &ManualRequest,
    outcome: &ManualOutcome,
) -> Result<AuditEntry, AlignError> {
    let entry = manual_audit_entry(request, outcome);
    platform
        .audit()
        .append(entry)
        .map_err(|error| AlignError::Audit(error.to_string()))
}

/// 把一次对齐结果写入平台审计（`action=align.alignment`，after=输出哈希）。
///
/// # Errors
///
/// 审计写入失败 → [`AlignError::Audit`]。
pub fn write_alignment_audit(
    platform: &Platform,
    report: &AlignReport,
) -> Result<AuditEntry, AlignError> {
    let detail = match &report.identity.status {
        crate::identity::IdentityStatus::Confirmed {
            student_id, basis, ..
        } => format!("身份已确认（{basis:?}，学生 {student_id}）"),
        crate::identity::IdentityStatus::Unresolved { kind, .. } => {
            format!("身份未决（{}）", kind.as_str())
        }
        crate::identity::IdentityStatus::OutsideRoster { .. } => "身份=名单外".to_owned(),
    };
    let note = format!(
        "对齐完成：{detail}；模板路径={}；题数={}；放行题数={}；未决={}；区域覆盖={:.3}；指纹={}",
        report.metrics.template_used,
        report.metrics.questions,
        report.metrics.aligned_questions,
        report.metrics.unresolved,
        report.metrics.region_coverage,
        report.fingerprint.output_hash.to_hex()
    );
    let entry = eg_platform::NewAuditEntry::new(
        "align.alignment",
        "submission",
        report.submission_id.to_string(),
    )
    .with_note(note)
    .with_hashes(None, Some(report.fingerprint.output_hash));
    platform
        .audit()
        .append(entry)
        .map_err(|error| AlignError::Audit(error.to_string()))
}

/// 平台审计链校验（人工修正后自检用）。
///
/// # Errors
///
/// 读取/校验失败 → [`AlignError::Audit`]。
pub fn verify_audit_chain(platform: &Platform) -> Result<u64, AlignError> {
    let verification = platform
        .audit()
        .verify_chain()
        .map_err(|error| AlignError::Audit(error.to_string()))?;
    if !verification.ok {
        return Err(AlignError::Audit(format!(
            "审计链断裂：{:?}",
            verification.broken
        )));
    }
    Ok(verification.entries)
}
