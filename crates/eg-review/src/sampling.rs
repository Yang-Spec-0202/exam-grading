//! 抽检只读与抽检结论（`SPEC-review.md` §3.2/§3.3、T32）。
//!
//! 快速抽检队列只读：编辑命令 100% 拒绝；仅允许抽检通过/不通过与转标准。
//! 不通过必须弹窗确认（`fast.fail_action=弹窗`，默认单题）；未确认不自动重跑。

use eg_domain::{GradeMode, GradeRecord, GradeStatus};

use crate::model::{RegradeScope, ReviewError, SamplingPrompt};

/// 记录是否处于「快速抽检待办」（mode=Fast、sampled=true、状态=自动接受）。
#[must_use]
pub fn is_sampling_item(record: &GradeRecord) -> bool {
    record.mode == GradeMode::Fast && record.sampled && record.status == GradeStatus::AutoAccepted
}

/// 抽检命令前置校验（模式/抽检状态）。
///
/// # Errors
///
/// 非抽检记录返回 [`ReviewError::WrongMode`]。
pub fn ensure_sampling_item(record: &GradeRecord) -> Result<(), ReviewError> {
    if is_sampling_item(record) {
        Ok(())
    } else {
        Err(ReviewError::WrongMode {
            command: crate::model::CommandKind::SamplingPass,
            mode: record.mode,
            sampled: record.sampled,
        })
    }
}

/// 编辑命令只读校验（快速抽检 100% 拒绝编辑）。
///
/// # Errors
///
/// 快速记录返回只读/模式错误；标准记录通过。
pub fn ensure_editable(record: &GradeRecord, sampled_readonly: bool) -> Result<(), ReviewError> {
    match record.mode {
        GradeMode::Standard => Ok(()),
        GradeMode::Fast => {
            if record.sampled && record.status == GradeStatus::AutoAccepted && sampled_readonly {
                Err(ReviewError::ReadOnlySampled)
            } else {
                Err(ReviewError::FastModeReadOnly)
            }
        }
    }
}

/// 构造抽检不通过弹窗协议事件（默认单题、可选整卷；未确认不重跑）。
#[must_use]
pub fn build_prompt(record: &GradeRecord, default_scope: RegradeScope) -> SamplingPrompt {
    SamplingPrompt {
        grade_id: record.id,
        submission_id: record.submission_id,
        question_id: record.question_id,
        default_scope,
        options: if default_scope == RegradeScope::WholeSubmission {
            vec![RegradeScope::WholeSubmission, RegradeScope::Single]
        } else {
            vec![RegradeScope::Single, RegradeScope::WholeSubmission]
        },
        message: "抽检不通过：是否重跑？默认单题重跑，可选整卷（需确认）。".to_owned(),
    }
}

/// 抽检不通过时是否允许直接重跑（只有教师确认后）。
#[must_use]
pub fn allows_auto_rerun() -> bool {
    false
}
