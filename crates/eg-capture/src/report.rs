//! 导入报告（`SPEC-capture.md` §2「导入报告」、§5 指标）与导入种类。

use eg_domain::{AssetId, SubmissionId, SubmissionStatus, Ulid};
use serde::{Deserialize, Serialize};

use crate::PIPELINE_VERSION;
use crate::event::SourceFingerprint;

/// 导入种类（`SPEC-capture.md` §2「导入请求」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportKind {
    /// 学生作业：每份文件一个提交 + 若干页。
    StudentWork,
    /// 参考答案页面（PDF/图片）：产出 `KeyPageAsset`，**不建模成 Page**。
    KeyReferencePages,
    /// 参考答案文本（DOCX）：当前里程碑显式未实现。
    KeyReferenceText,
}

impl ImportKind {
    /// 稳定字符串（事件/报告/审计）。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StudentWork => "student_work",
            Self::KeyReferencePages => "key_reference_pages",
            Self::KeyReferenceText => "key_reference_text",
        }
    }
}

/// 新建提交摘要（报告只给 id 与页数，不含姓名/路径）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SubmissionSummary {
    /// 提交 id。
    pub submission_id: SubmissionId,
    /// 页数（含被拒页）。
    pub page_count: u32,
    /// 提交状态（`Imported` 或 `Failed`）。
    pub status: SubmissionStatus,
}

/// 文件级失败（原因码 + 中文消息）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileFailure {
    /// 来源指纹（脱敏）。
    pub fingerprint: SourceFingerprint,
    /// 稳定错误码。
    pub code: &'static str,
    /// 教师可读消息。
    pub message: String,
}

/// 被拒页面（页号 + 原因码列表）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RejectedPage {
    /// 所属提交（参考答案为 `None`）。
    pub submission_id: Option<SubmissionId>,
    /// 页号（1 起）。
    pub page_no: u32,
    /// 原因码（稳定码，按字典序去重）。
    pub reasons: Vec<&'static str>,
}

/// 重复命中类别（`SPEC-capture.md` §3.4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DuplicateCategory {
    /// 同批重复文件（只处理一次）。
    SameBatch,
    /// 历史重复（资产已存在，页面不重复建）。
    Historical,
    /// 疑似重复提交（页面哈希重合 ≥80%，默认询问）。
    SuspectedSubmission,
}

/// 重复命中记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DuplicateHit {
    /// 来源指纹。
    pub fingerprint: SourceFingerprint,
    /// 类别。
    pub category: DuplicateCategory,
    /// 命中的资产（`Historical`/`SameBatch`）或提交页的首个资产（疑似重复）。
    pub asset: AssetId,
}

/// 导入报告（批次级汇总；同时驱动 UI 与审计）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ImportReport {
    /// 批次 id。
    pub batch_id: Ulid,
    /// 导入种类。
    pub kind: ImportKind,
    /// 新建提交。
    pub submissions: Vec<SubmissionSummary>,
    /// 参考答案资产。
    pub key_assets: Vec<KeyAssetSummary>,
    /// 文件失败列表。
    pub file_failures: Vec<FileFailure>,
    /// 被拒页面列表。
    pub rejected_pages: Vec<RejectedPage>,
    /// 重复命中列表。
    pub duplicates: Vec<DuplicateHit>,
    /// 是否部分成功（有失败文件或拒收页，且不是全部失败）。
    pub partial_success: bool,
    /// 是否被取消。
    pub cancelled: bool,
    /// 耗时（毫秒）。
    pub elapsed_ms: u64,
    /// 是否显示过命名提示（仅一次）。
    pub naming_reminder_shown: bool,
    /// 选项哈希（`import.*` + 流水线版本）。
    pub options_hash: String,
    /// 流水线版本。
    pub pipeline_version: String,
    /// 文件总数。
    pub files_total: u32,
    /// 成功文件数。
    pub files_ok: u32,
    /// 失败文件数。
    pub files_failed: u32,
    /// 页面总数（含被拒/失败）。
    pub pages_total: u32,
    /// 良好页数。
    pub pages_good: u32,
    /// 可接受页数。
    pub pages_acceptable: u32,
    /// 被拒页数。
    pub pages_rejected: u32,
    /// 页面失败数（渲染/存储）。
    pub pages_failed: u32,
}

impl ImportReport {
    /// 新建空的批次报告骨架。
    pub fn empty(batch_id: Ulid, kind: ImportKind, options_hash: &str, files_total: u32) -> Self {
        Self {
            batch_id,
            kind,
            submissions: Vec::new(),
            key_assets: Vec::new(),
            file_failures: Vec::new(),
            rejected_pages: Vec::new(),
            duplicates: Vec::new(),
            partial_success: false,
            cancelled: false,
            elapsed_ms: 0,
            naming_reminder_shown: false,
            options_hash: options_hash.to_owned(),
            pipeline_version: PIPELINE_VERSION.to_owned(),
            files_total,
            files_ok: 0,
            files_failed: 0,
            pages_total: 0,
            pages_good: 0,
            pages_acceptable: 0,
            pages_rejected: 0,
            pages_failed: 0,
        }
    }

    /// 本次运行是否没有任何问题（无失败、无拒收、无重复）。
    pub fn clean(&self) -> bool {
        self.file_failures.is_empty()
            && self.rejected_pages.is_empty()
            && self.duplicates.is_empty()
            && !self.cancelled
    }

    /// 被拒原因计数（按原因码分组，供指标）。
    pub fn reject_reason_counts(&self) -> Vec<(&'static str, u32)> {
        let mut counts: std::collections::BTreeMap<&'static str, u32> =
            std::collections::BTreeMap::new();
        for page in &self.rejected_pages {
            for reason in &page.reasons {
                *counts.entry(reason).or_insert(0) += 1;
            }
        }
        counts.into_iter().collect()
    }
}

/// 参考答案资产摘要（`KeyPageAsset` 的报告投影；页面类别只到「已预处理」）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KeyAssetSummary {
    /// 参考资产 id（页码为 0 的文档文本记录为 `None`）。
    pub page_no: u32,
    /// 原图/页面资产。
    pub asset: Option<AssetId>,
    /// 类别：`page` 或 `document_text`。
    pub kind: &'static str,
    /// 方向校正角度（0/90/180/270）。
    pub rotation_applied: u16,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn report_tracks_counts_and_reasons() {
        let mut report =
            ImportReport::empty(Ulid::from_u128(1), ImportKind::StudentWork, "hash", 2);
        report.files_ok = 1;
        report.files_failed = 1;
        report.pages_total = 3;
        report.pages_good = 1;
        report.pages_rejected = 2;
        report.rejected_pages.push(RejectedPage {
            submission_id: Some(SubmissionId::from_ulid(Ulid::from_u128(2))),
            page_no: 1,
            reasons: vec!["blur", "skew"],
        });
        report.rejected_pages.push(RejectedPage {
            submission_id: Some(SubmissionId::from_ulid(Ulid::from_u128(2))),
            page_no: 2,
            reasons: vec!["blur"],
        });
        assert!(!report.clean());
        assert_eq!(
            report.reject_reason_counts(),
            vec![("blur", 2), ("skew", 1)]
        );
        assert_eq!(report.pipeline_version, PIPELINE_VERSION);
    }
}
