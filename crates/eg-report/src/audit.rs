//! 导出审计（`SPEC-report.md` §6：每次导出写导出运行；只追加；不记学生原文）。
//!
//! 本 crate 不直连数据库：审计经 [`ExportAuditor`] 端口交给 `app`/`platform` 落库
//! （`eg-platform` 的 `AuditLog` 适配为 TODO）。审计失败即导出失败，由调用方回滚文件。

use crate::error::{ReportError, Result};
use eg_domain::{Sha256Hex, Timestamp};
use std::sync::Mutex;

/// 审计动作名（`eg-platform` 的 `audit.action`）。
pub const AUDIT_ACTION: &str = "report.export";
/// 审计实体类型。
pub const AUDIT_ENTITY_TYPE: &str = "report_export";
/// 固定操作者（无登录；规范 §6）。
pub const AUDIT_ACTOR: &str = "local-teacher";

/// 导出审计事件（聚合计数；不含姓名/学号原文）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportAuditEvent {
    /// 动作（恒为 [`AUDIT_ACTION`]）。
    pub action: String,
    /// 操作者（恒为 [`AUDIT_ACTOR`]）。
    pub actor: String,
    /// 实体类型。
    pub entity_type: String,
    /// 实体 id（导出运行 id，由调用方/平台生成后回填；本 crate 用快照哈希前缀）。
    pub entity_id: String,
    /// 备注（格式、模式、行数、脱敏/草稿标记、命名规则版本等）。
    pub note: String,
    /// 文件内容摘要（导出运行内容哈希）。
    pub after_hash: Option<Sha256Hex>,
    /// 事件时间。
    pub occurred_at: Timestamp,
    /// 口径版本。
    pub policy_version: u32,
    /// 命名规则版本。
    pub naming_rule_version: u32,
    /// 是否脱敏。
    pub masked: bool,
    /// 是否草稿导出。
    pub draft: bool,
    /// 快照哈希。
    pub snapshot_hash: Sha256Hex,
    /// 文件数。
    pub file_count: usize,
    /// 行数（全部工作表合计）。
    pub row_count: usize,
    /// 格式集合（`xlsx`/`csv`/`html`）。
    pub formats: Vec<String>,
}

/// 审计端口（由 `app` 实现）。
pub trait ExportAuditor {
    /// 追加一条导出审计。
    ///
    /// # Errors
    ///
    /// 审计写入失败时返回 [`ReportError::Audit`]；调用方必须回滚导出文件。
    fn record(&self, event: &ExportAuditEvent) -> Result<()>;
}

/// 内存审计（测试夹具；可注入失败以覆盖「审计失败即导出失败」）。
#[derive(Debug)]
pub struct MemoryAuditor {
    events: Mutex<Vec<ExportAuditEvent>>,
    fail: bool,
}

impl MemoryAuditor {
    /// 记录全部事件的审计器。
    #[must_use]
    pub fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
            fail: false,
        }
    }

    /// 始终失败的审计器（模拟审计库不可写）。
    #[must_use]
    pub fn failing() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
            fail: true,
        }
    }

    /// 已记录事件（按写入顺序）。
    ///
    /// # Errors
    ///
    /// 锁中毒时返回 [`ReportError::Audit`]。
    pub fn events(&self) -> Result<Vec<ExportAuditEvent>> {
        let guard = self
            .events
            .lock()
            .map_err(|_| ReportError::Audit("内存审计器锁中毒".to_owned()))?;
        Ok(guard.clone())
    }
}

impl Default for MemoryAuditor {
    fn default() -> Self {
        Self::new()
    }
}

impl ExportAuditor for MemoryAuditor {
    fn record(&self, event: &ExportAuditEvent) -> Result<()> {
        if self.fail {
            return Err(ReportError::Audit("模拟审计写入失败".to_owned()));
        }
        let mut guard = self
            .events
            .lock()
            .map_err(|_| ReportError::Audit("内存审计器锁中毒".to_owned()))?;
        guard.push(event.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event() -> ExportAuditEvent {
        ExportAuditEvent {
            action: AUDIT_ACTION.to_owned(),
            actor: AUDIT_ACTOR.to_owned(),
            entity_type: AUDIT_ENTITY_TYPE.to_owned(),
            entity_id: "export-1".to_owned(),
            note: "xlsx".to_owned(),
            after_hash: Some(Sha256Hex::of(b"payload")),
            occurred_at: eg_domain::Timestamp::default(),
            policy_version: 1,
            naming_rule_version: 1,
            masked: false,
            draft: false,
            snapshot_hash: Sha256Hex::of(b"snapshot"),
            file_count: 1,
            row_count: 2,
            formats: vec!["xlsx".to_owned()],
        }
    }

    #[test]
    fn memory_auditor_records_and_fails() {
        let auditor = MemoryAuditor::new();
        auditor.record(&event()).unwrap();
        assert_eq!(auditor.events().unwrap().len(), 1);
        let failing = MemoryAuditor::failing();
        assert!(matches!(
            failing.record(&event()),
            Err(ReportError::Audit(_))
        ));
    }
}
