//! 提交级判分流水线（T30）：对齐 → 逐题判分 → 落库 → 断点/幂等 → 抽检标记 → 汇总。
//!
//! 不变量：单题失败不影响其余题且显式记录；重跑命中幂等键不重复判分/计费；
//! 已导出报表不自动刷新（只置 `report_refresh_required` 标记）；抽检标记随记录落库。

use std::collections::BTreeSet;
use std::sync::Mutex;

use eg_domain::{
    ComparatorTrace, EngineStamp, GradeMode, GradeRecord, GradeStatus, ModelCall, Paper,
    PaperTemplate, QuestionId, ReviewReasonKind, SubmissionId,
};

use crate::align::{AlignError, AlignIssue, AlignSource, to_question_answer};
use crate::pipeline::{
    GradeError, GradedQuestion, Grader, QuestionAnswer, QuestionFailure, SubmissionGrade,
    SubmissionRequest, SubmissionSummary,
};
use crate::readiness::ReadinessReport;

/// 落库错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    /// 数据库/事务失败。
    #[error("落库失败：{0}")]
    Failed(String),
}

/// 判分结果落库（生产接 `platform::repo` + `audit` 同事务；见 `adapters::platform`）。
pub trait GradeStore: Send + Sync {
    /// 落库单题：判分记录 + 比较器轨迹 + 未落库模型调用（同事务审计）。
    ///
    /// 幂等：同记录 id 重复落库视为成功无操作（重跑/恢复安全）。
    ///
    /// # Errors
    ///
    /// 存储失败返回 [`StoreError`]。
    fn persist_question(
        &self,
        record: &GradeRecord,
        traces: &[ComparatorTrace],
        unpersisted_model_calls: &[ModelCall],
    ) -> Result<(), StoreError>;

    /// 某提交的最新修订（按题）。
    ///
    /// # Errors
    ///
    /// 读取失败返回 [`StoreError`]。
    fn latest_records(&self, submission_id: SubmissionId) -> Result<Vec<GradeRecord>, StoreError>;
}

/// 断点进度快照（按提交/题粒度）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProgressSnapshot {
    /// 已处理题目（已落库）。
    pub processed: BTreeSet<QuestionId>,
    /// 是否已完成整卷。
    pub completed: bool,
    /// 单题失败记录（题 id + 原因）。
    pub failures: Vec<(QuestionId, String)>,
}

/// 断点存储（生产接 platform（任务/断点表）；当前 fixture + TODO）。
pub trait ProgressStore: Send + Sync {
    /// 读取提交进度（不存在 = 未开始）。
    ///
    /// # Errors
    ///
    /// 存储失败返回 [`StoreError`]。
    fn load(&self, submission_id: SubmissionId) -> Result<ProgressSnapshot, StoreError>;

    /// 标记某题已处理（幂等）。
    ///
    /// # Errors
    ///
    /// 存储失败返回 [`StoreError`]。
    fn mark_question_processed(
        &self,
        submission_id: SubmissionId,
        question_id: QuestionId,
    ) -> Result<(), StoreError>;

    /// 标记整卷完成。
    ///
    /// # Errors
    ///
    /// 存储失败返回 [`StoreError`]。
    fn mark_completed(&self, submission_id: SubmissionId) -> Result<(), StoreError>;

    /// 记录单题失败（不阻断其余题）。
    ///
    /// # Errors
    ///
    /// 存储失败返回 [`StoreError`]。
    fn mark_failed(
        &self,
        submission_id: SubmissionId,
        question_id: QuestionId,
        reason: String,
    ) -> Result<(), StoreError>;
}

/// 内存断点存储（测试/回放；生产 TODO 接 platform）。
#[derive(Debug, Default)]
pub struct InMemoryProgressStore {
    state: Mutex<std::collections::HashMap<SubmissionId, ProgressSnapshot>>,
}

impl InMemoryProgressStore {
    /// 空存储。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl ProgressStore for InMemoryProgressStore {
    fn load(&self, submission_id: SubmissionId) -> Result<ProgressSnapshot, StoreError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| StoreError::Failed("断点存储锁失败".to_owned()))?
            .get(&submission_id)
            .cloned()
            .unwrap_or_default())
    }

    fn mark_question_processed(
        &self,
        submission_id: SubmissionId,
        question_id: QuestionId,
    ) -> Result<(), StoreError> {
        self.state
            .lock()
            .map_err(|_| StoreError::Failed("断点存储锁失败".to_owned()))?
            .entry(submission_id)
            .or_default()
            .processed
            .insert(question_id);
        Ok(())
    }

    fn mark_completed(&self, submission_id: SubmissionId) -> Result<(), StoreError> {
        self.state
            .lock()
            .map_err(|_| StoreError::Failed("断点存储锁失败".to_owned()))?
            .entry(submission_id)
            .or_default()
            .completed = true;
        Ok(())
    }

    fn mark_failed(
        &self,
        submission_id: SubmissionId,
        question_id: QuestionId,
        reason: String,
    ) -> Result<(), StoreError> {
        self.state
            .lock()
            .map_err(|_| StoreError::Failed("断点存储锁失败".to_owned()))?
            .entry(submission_id)
            .or_default()
            .failures
            .push((question_id, reason));
        Ok(())
    }
}

/// 内存落库（测试/回放；按提交保存最新修订与轨迹/调用）。
#[derive(Debug, Default)]
pub struct InMemoryGradeStore {
    records: Mutex<Vec<GradeRecord>>,
    traces: Mutex<std::collections::HashMap<String, Vec<ComparatorTrace>>>,
    calls: Mutex<Vec<ModelCall>>,
}

impl InMemoryGradeStore {
    /// 空存储。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 全部记录（测试）。
    #[must_use]
    pub fn records(&self) -> Vec<GradeRecord> {
        self.records
            .lock()
            .map_or_else(|_| Vec::new(), |records| records.clone())
    }

    /// 已落库模型调用（测试）。
    #[must_use]
    pub fn model_calls(&self) -> Vec<ModelCall> {
        self.calls
            .lock()
            .map_or_else(|_| Vec::new(), |calls| calls.clone())
    }
}

impl GradeStore for InMemoryGradeStore {
    fn persist_question(
        &self,
        record: &GradeRecord,
        traces: &[ComparatorTrace],
        unpersisted_model_calls: &[ModelCall],
    ) -> Result<(), StoreError> {
        let mut records = self
            .records
            .lock()
            .map_err(|_| StoreError::Failed("内存落库锁失败".to_owned()))?;
        if records.iter().any(|existing| existing.id == record.id) {
            return Ok(());
        }
        records.push(record.clone());
        if let Ok(mut map) = self.traces.lock() {
            map.insert(record.id.to_string(), traces.to_vec());
        }
        if let Ok(mut calls) = self.calls.lock() {
            calls.extend_from_slice(unpersisted_model_calls);
        }
        Ok(())
    }

    fn latest_records(&self, submission_id: SubmissionId) -> Result<Vec<GradeRecord>, StoreError> {
        let records = self
            .records
            .lock()
            .map_err(|_| StoreError::Failed("内存落库锁失败".to_owned()))?;
        let mut latest: Vec<GradeRecord> = Vec::new();
        for record in records
            .iter()
            .filter(|record| record.submission_id == submission_id)
        {
            match latest
                .iter_mut()
                .find(|existing| existing.question_id == record.question_id)
            {
                Some(existing) if existing.revision < record.revision => *existing = record.clone(),
                Some(_) => {}
                None => latest.push(record.clone()),
            }
        }
        latest.sort_by_key(|record| record.question_id);
        Ok(latest)
    }
}

/// 提交级运行请求。
#[derive(Debug)]
pub struct SubmissionRunRequest {
    /// 提交 id。
    pub submission_id: SubmissionId,
    /// 任务 id（模型调用授权/审计）。
    pub task_id: eg_domain::TaskId,
    /// 试卷。
    pub paper: Paper,
    /// 模板（版本对齐）。
    pub template: Option<PaperTemplate>,
    /// 模式。
    pub mode: GradeMode,
    /// 判定后端是否可用。
    pub backend_available: bool,
    /// 教师标记仅人工的题目。
    pub manual_only: Vec<QuestionId>,
    /// 提交级排除。
    pub excluded: Option<eg_domain::ExcludeReason>,
    /// 报表是否已导出（已导出时修改只置刷新标记，不自动刷新）。
    pub report_exported: bool,
    /// 外部引擎指纹（OCR/对齐/CAS 等）。
    pub engine_stamps: Vec<EngineStamp>,
    /// 是否强制重新判分（忽略断点完成标记；仍走幂等键）。
    pub force: bool,
}

/// 对齐门禁投影（整卷 ready / 阻断题 / 未决清单）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AlignGate {
    /// 整卷是否允许自动判分。
    pub auto_grade_ready: bool,
    /// 题级阻断未决。
    pub blocking_questions: Vec<QuestionId>,
    /// 全部未决（含整卷级）。
    pub issues: Vec<AlignIssue>,
}

/// 提交级运行结果。
#[derive(Debug, Clone)]
pub struct SubmissionRun {
    /// 判分结果。
    pub grade: SubmissionGrade,
    /// 本次新落库题数。
    pub persisted: usize,
    /// 断点恢复命中题数（已处理）。
    pub resumed: usize,
    /// 已导出报表需刷新（不自动刷新）。
    pub report_refresh_required: bool,
    /// 本次引擎指纹（含外部引擎版本）。
    pub engine_fingerprint: String,
    /// 对齐门禁投影（整卷 ready/阻断题/未决）。
    pub align_gate: AlignGate,
}

/// 提交流水线错误（对齐/整卷级；单题失败在 `grade.failures`）。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SubmissionPipelineError {
    /// 对齐失败。
    #[error("对齐失败：{0}")]
    Align(#[from] AlignError),
    /// 判分整卷错误（配置/就绪/学科等）。
    #[error("判分失败：{0}")]
    Grade(#[from] GradeError),
    /// 落库/断点失败。
    #[error("落库失败：{0}")]
    Store(#[from] StoreError),
    /// 整卷对齐门禁不通过（身份/重复等整卷级未决；不得自动判分）。
    #[error("整卷对齐门禁不通过：{0}")]
    AlignGate(String),
}

/// 提交级流水线（组合根注入对齐来源/落库/断点）。
pub struct SubmissionPipeline<'a> {
    /// 判分器。
    pub grader: &'a Grader<'a>,
    /// 对齐来源。
    pub align: &'a dyn AlignSource,
    /// 落库。
    pub store: &'a dyn GradeStore,
    /// 断点。
    pub progress: &'a dyn ProgressStore,
}

impl std::fmt::Debug for SubmissionPipeline<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SubmissionPipeline")
            .field("align", &self.align.identity())
            .finish_non_exhaustive()
    }
}

impl SubmissionPipeline<'_> {
    /// 执行整份提交判分（幂等/断点恢复）。
    ///
    /// # Errors
    ///
    /// 对齐失败、判分整卷错误、落库失败返回 [`SubmissionPipelineError`]。
    pub fn run(
        &self,
        request: &SubmissionRunRequest,
    ) -> Result<SubmissionRun, SubmissionPipelineError> {
        let snapshot = self.progress.load(request.submission_id)?;
        let mut engine_stamps = request.engine_stamps.clone();
        if let Some(stamp) = self.align.engine_stamp()
            && !engine_stamps.contains(&stamp)
        {
            engine_stamps.push(stamp);
        }
        let fingerprint = self.grader.engine_fingerprint_for(&engine_stamps);
        // 已完成且不强制：直接从落库记录重建汇总（不重判、不重新计费）
        if snapshot.completed && !request.force {
            let records = self.store.latest_records(request.submission_id)?;
            let grade =
                grade_from_records(request, self.grader, records, &[], &[], &snapshot.failures);
            return Ok(SubmissionRun {
                grade,
                persisted: 0,
                resumed: snapshot.processed.len(),
                report_refresh_required: false,
                engine_fingerprint: fingerprint,
                align_gate: AlignGate::default(),
            });
        }
        let aligned = self.align.aligned_submission(request.submission_id)?;
        let align_gate = AlignGate {
            auto_grade_ready: aligned.auto_grade_ready,
            blocking_questions: aligned.blocking_questions().into_iter().collect(),
            issues: aligned.issues.clone(),
        };
        // 整卷级阻断未决（身份/重复等）→ 整卷拒绝自动判分
        if aligned.whole_submission_blocked() {
            let detail = aligned
                .issues
                .iter()
                .filter(|issue| issue.blocks_auto_grade && issue.question_id.is_none())
                .map(|issue| format!("{}：{}", issue.kind, issue.detail))
                .collect::<Vec<_>>()
                .join("；");
            return Err(SubmissionPipelineError::AlignGate(detail));
        }
        // 只判可放行题；未放行题由缺失答案进入 failures 并显式标注未决（不伪装未作答）
        let answers: Vec<QuestionAnswer> = aligned
            .answers
            .iter()
            .filter(|student| aligned.allowed_questions.contains(&student.question_id))
            .filter_map(|student| {
                request
                    .paper
                    .questions
                    .iter()
                    .find(|question| question.id == student.question_id)
                    .map(|question| to_question_answer(question, student))
            })
            .collect();
        let submission_request = SubmissionRequest {
            submission_id: request.submission_id,
            task_id: request.task_id,
            paper: request.paper.clone(),
            template: request.template.clone(),
            answers,
            mode: request.mode,
            backend_available: request.backend_available,
            manual_only: request.manual_only.clone(),
            excluded: request.excluded,
            engine_stamps: engine_stamps.clone(),
        };
        let mut grade = self.grader.grade_submission(&submission_request)?;
        // 未决题显式进失败/澄清（说明来自 align，不伪装未作答）
        for failure in &mut grade.failures {
            if let Some(issue) = aligned.issues.iter().find(|issue| {
                issue.blocks_auto_grade && issue.question_id == Some(failure.question_id)
            }) {
                failure.message = format!("对齐未决（{}）：{}", issue.kind, issue.detail);
            }
        }
        let mut persisted = 0usize;
        let mut resumed = 0usize;
        for graded in &grade.records {
            let question_id = graded.record.question_id;
            if graded.idempotent_hit {
                // 幂等命中：已有记录（断点恢复/重跑），不重复落库/计费
                resumed += 1;
                continue;
            }
            self.store.persist_question(
                &graded.record,
                &graded.traces,
                &graded.unpersisted_model_calls,
            )?;
            self.progress
                .mark_question_processed(request.submission_id, question_id)?;
            persisted += 1;
        }
        for failure in &grade.failures {
            self.progress.mark_failed(
                request.submission_id,
                failure.question_id,
                failure.message.clone(),
            )?;
        }
        let all_done = grade.records.len() + grade.failures.len() >= request.paper.questions.len();
        if all_done {
            self.progress.mark_completed(request.submission_id)?;
        }
        Ok(SubmissionRun {
            report_refresh_required: request.report_exported && persisted > 0,
            engine_fingerprint: fingerprint,
            grade,
            persisted,
            resumed,
            align_gate,
        })
    }
}

fn grade_from_records(
    request: &SubmissionRunRequest,
    grader: &Grader<'_>,
    records: Vec<GradeRecord>,
    _empty_traces: &[ComparatorTrace],
    _empty_calls: &[ModelCall],
    failures: &[(QuestionId, String)],
) -> SubmissionGrade {
    let mut graded_questions = Vec::new();
    for record in records {
        let blockers =
            record.auto_accept_blockers(grader.config.threshold(&record.route, record.mode));
        graded_questions.push(GradedQuestion {
            record,
            traces: Vec::new(),
            model_calls: Vec::new(),
            unpersisted_model_calls: Vec::new(),
            blockers,
            idempotent_hit: true,
            force_sampled: false,
        });
    }
    let mut summary = SubmissionSummary {
        total_questions: request.paper.questions.len(),
        failed: failures.len(),
        ..SubmissionSummary::default()
    };
    for graded in &graded_questions {
        match graded.record.status {
            GradeStatus::AutoAccepted => summary.auto_accepted += 1,
            GradeStatus::NeedsReview => summary.needs_review += 1,
            GradeStatus::ManuallyReviewed | GradeStatus::Excluded => {}
        }
    }
    let failed = failures
        .iter()
        .map(|(question_id, message)| QuestionFailure {
            question_id: *question_id,
            message: message.clone(),
        })
        .collect();
    SubmissionGrade {
        submission_id: request.submission_id,
        records: graded_questions,
        failures: failed,
        summary,
        sampling: None,
        readiness: ReadinessReport::default(),
    }
}

/// 汇总辅助：按记录计算待复核原因（UI/测试）。
#[must_use]
pub fn blockers_for(record: &GradeRecord, grader: &Grader<'_>) -> Vec<ReviewReasonKind> {
    record.auto_accept_blockers(grader.config.threshold(&record.route, record.mode))
}
