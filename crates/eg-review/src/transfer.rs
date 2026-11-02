//! 一键转标准（`SPEC-review.md` §3.2/§3.3、T32）。
//!
//! 保留原快速记录；由 `grade` 以标准模式重判生成新标准修订；无法重判则进入
//! 标准待复核队列（仍生成标准模式新修订，状态待复核）。

use eg_domain::{GradeMode, GradeRecord, GradeStatus};

use crate::model::{RegradeResult, ReviewError};

/// 校验转标准前置（仅快速记录可转）。
///
/// # Errors
///
/// 已是标准模式返回 [`ReviewError::WrongMode`]。
pub fn ensure_transferable(record: &GradeRecord) -> Result<(), ReviewError> {
    if record.mode == GradeMode::Fast {
        Ok(())
    } else {
        Err(ReviewError::WrongMode {
            command: crate::model::CommandKind::TransferToStandard,
            mode: record.mode,
            sampled: record.sampled,
        })
    }
}

/// 将重判结果映射为标准修订（无法重判时降级为待复核标准修订）。
///
/// # Errors
///
/// 引擎既无记录也无任务、且未标记降级时返回错误（保留原判定）。
pub fn standard_record_from_result(
    result: &RegradeResult,
    fallback_base: &GradeRecord,
) -> Result<GradeRecord, ReviewError> {
    if let Some(record) = &result.record {
        let mut record = record.clone();
        record.mode = GradeMode::Standard;
        record.sampled = false;
        if result.fallback_to_queue {
            record.status = GradeStatus::NeedsReview;
        }
        return Ok(record);
    }
    if result.task_id.is_some() {
        return Err(ReviewError::RegradeFailed(
            "整卷任务不返回单题记录（由 app 调度）".to_owned(),
        ));
    }
    // 无记录也无任务：构造标准待复核修订（保留原判定；绝不写 0 分）
    let mut record = fallback_base.clone();
    record.mode = GradeMode::Standard;
    record.sampled = false;
    record.status = GradeStatus::NeedsReview;
    Ok(record)
}
