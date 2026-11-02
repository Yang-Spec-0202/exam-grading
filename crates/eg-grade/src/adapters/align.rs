//! `AlignSource` ← `eg-align` 的 pipeline/report 输出（T29/T30）。
//!
//! 组合根实现 [`AlignReportProvider`]（用 `eg_align::align` + capture/recognize/platform
//! 端口跑一份提交），本适配器把 `AlignReport` 投影为判分需要的
//! 「答案 + 逐题门禁 + 整卷 ready + 未决」。未对齐题不伪造未作答：只投影已构造答案，
//! 未决题由提交级流水线显式转澄清/失败。

use std::sync::Arc;

use eg_align::AlignReport;
use eg_domain::{EngineStamp, StudentAnswer, SubmissionId};

use crate::align::{AlignError, AlignIssue, AlignSource, AlignedSubmission};

/// 对齐报告提供者（组合根用 `eg_align::align` 实现；fixture 可直接返回报告）。
pub trait AlignReportProvider: Send + Sync {
    /// 执行/读取一份提交的对齐报告。
    ///
    /// # Errors
    ///
    /// 对齐失败返回 [`eg_align::AlignError`]。
    fn report(&self, submission_id: SubmissionId) -> Result<AlignReport, eg_align::AlignError>;

    /// 提供者身份（进引擎指纹）。
    fn identity(&self) -> String;

    /// 对齐算法/版本指纹（进引擎指纹；无则空）。
    fn engine_stamp(&self) -> Option<EngineStamp> {
        None
    }
}

/// 真实对齐来源（消费 `eg_align::AlignReport`）。
#[derive(Clone)]
pub struct AlignReportSource {
    provider: Arc<dyn AlignReportProvider>,
}

impl std::fmt::Debug for AlignReportSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AlignReportSource")
            .field("provider", &self.provider.identity())
            .finish_non_exhaustive()
    }
}

impl AlignReportSource {
    /// 构造。
    #[must_use]
    pub fn new(provider: Arc<dyn AlignReportProvider>) -> Self {
        Self { provider }
    }
}

impl AlignSource for AlignReportSource {
    fn aligned_submission(
        &self,
        submission_id: SubmissionId,
    ) -> Result<AlignedSubmission, AlignError> {
        let report = self
            .provider
            .report(submission_id)
            .map_err(|error| AlignError::Read(error.to_string()))?;
        Ok(project_report(&report))
    }

    fn identity(&self) -> String {
        format!("align-report-v1:{}", self.provider.identity())
    }

    fn engine_stamp(&self) -> Option<EngineStamp> {
        self.provider.engine_stamp()
    }
}

/// 把 `AlignReport` 投影为判分输入。
#[must_use]
pub fn project_report(report: &AlignReport) -> AlignedSubmission {
    let allowed_questions = report
        .question_summaries
        .iter()
        .filter(|summary| summary.auto_grade_allowed)
        .map(|summary| summary.question_id)
        .collect();
    let issues = report
        .unresolved
        .iter()
        .map(|item| AlignIssue {
            question_id: item.question_id,
            kind: item.kind.as_str().to_owned(),
            detail: item.detail.clone(),
            blocks_auto_grade: item.kind.blocks_auto_grade(),
        })
        .collect();
    AlignedSubmission {
        answers: report.answers.clone(),
        allowed_questions,
        auto_grade_ready: report.auto_grade_ready,
        issues,
    }
}

/// 便捷：仅投影答案（不丢门禁信息时请用 [`project_report`]）。
#[must_use]
pub fn project_answers(report: &AlignReport) -> Vec<StudentAnswer> {
    report.answers.clone()
}
