//! 撤销/重做（`SPEC-review.md` §3/§4：命令栈持久化、仅追加反向动作）。
//!
//! 栈本身由 [`UndoStore`] 适配器持久化（生产接 platform；测试用内存 fixture），
//! 撤销/重做的落库由 [`crate::commands::ReviewService`] 以「追加新修订」实现，
//! 绝不删除历史修订。

use eg_domain::{GradeId, GradeRecord, SubmissionExclusion, SubmissionId};

use crate::model::SamplingConclusion;

/// 可撤销动作。
#[derive(Debug, Clone, PartialEq)]
#[allow(
    clippy::large_enum_variant,
    reason = "撤销条目需保存完整前后快照以保证仅追加撤销可精确恢复，大小由领域记录决定"
)]
pub enum UndoKind {
    /// 判分记录状态/得分变更。
    Record {
        /// 判分记录 id。
        grade_id: GradeId,
        /// 变更前快照。
        before: GradeRecord,
        /// 变更后快照。
        after: GradeRecord,
    },
    /// 抽检结论变更（无记录修订，仅审计结论；撤销/重做只追加审计）。
    SamplingConclusion {
        /// 判分记录 id。
        grade_id: GradeId,
        /// 结论。
        conclusion: SamplingConclusion,
    },
    /// 提交级排除变更。
    SubmissionExclusion {
        /// 提交 id。
        submission_id: SubmissionId,
        /// 变更前（None = 原未排除）。
        before: Option<SubmissionExclusion>,
        /// 变更后。
        after: SubmissionExclusion,
    },
}

/// 撤销栈条目。
#[derive(Debug, Clone, PartialEq)]
pub struct UndoEntry {
    /// 动作。
    pub kind: UndoKind,
    /// 可读标签（日志/调试，不含学生姓名）。
    pub label: String,
}

impl UndoEntry {
    /// 构造。
    #[must_use]
    pub fn new(kind: UndoKind, label: impl Into<String>) -> Self {
        Self {
            kind,
            label: label.into(),
        }
    }
}
