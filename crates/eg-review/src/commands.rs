//! 复核服务：命令执行、审计、乐观锁、撤销/重做（`SPEC-review.md` §3/§4/§5，T31–T32）。
//!
//! 不变量：
//! - 只追加修订（新 `GradeRecord.id` + `revision=latest+1`），绝不原地改；
//! - 每条命令（含被拒）落审计（actor 固定 `local-teacher`、命令幂等键、前后快照）；
//! - 快速抽检只读（`fast.sampled_readonly=开`）：编辑命令 100% 拒绝；
//! - 抽检不通过未确认不重跑；确认后默认单题，整卷仅显式选择；
//! - 撤销/重做 ≥50 步（`UndoStore` 持久化），撤销通过追加反向修订实现。

use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;

use eg_domain::{
    Clock, ExcludeReason, GradeId, GradeMode, GradeRecord, GradeStatus, Judgement, MilliScore,
    ReviewReasonKind, Sha256Hex, SubmissionExclusion, SubmissionId, Timestamp, Ulid,
};
use eg_grade::ports::IdGenerator;

use crate::detail::build_detail;
use crate::model::{
    ACTOR, AuditOutcome, AuditRecord, BatchFailure, BatchOutcome, BatchTarget, CommandEnvelope,
    CommandKind, CommandOutcome, CommandResult, CorrectedText, RegradeRequest, RegradeScope,
    ReviewCommand, ReviewConfig, ReviewDetail, ReviewError, ReviewMetadata, SamplingConclusion,
};
use crate::ports::{AuditSink, RegradeEngine, ReviewRepository, UndoStore};
use crate::queue::{build_queue, classify_record, item_for, queue_reasons};
use crate::sampling;
use crate::transfer;
use crate::undo::{UndoEntry, UndoKind};

/// 题级锁键。
type QuestionKey = (SubmissionId, eg_domain::QuestionId);

/// 复核服务（组合根注入数据源/审计/撤销栈/重判/时钟/id/配置）。
pub struct ReviewService<'a> {
    /// 数据源。
    pub repo: &'a dyn ReviewRepository,
    /// 审计。
    pub audit: &'a dyn AuditSink,
    /// 撤销栈。
    pub undo_store: &'a dyn UndoStore,
    /// 重判后端。
    pub regrade: &'a dyn RegradeEngine,
    /// 时钟。
    pub clock: &'a dyn Clock,
    /// id 生成。
    pub ids: &'a dyn IdGenerator,
    /// 配置。
    pub config: &'a ReviewConfig,
    /// 命令幂等缓存。
    idempotency: Mutex<HashMap<String, CommandResult>>,
    /// 题级重判锁。
    locks: Mutex<BTreeSet<QuestionKey>>,
}

impl std::fmt::Debug for ReviewService<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReviewService")
            .field("page_size", &self.config.page_size)
            .field("undo_depth", &self.config.undo_depth)
            .field("sampled_readonly", &self.config.sampled_readonly)
            .finish()
    }
}

#[allow(
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    clippy::needless_return,
    reason = "命令实现按记录快照传递便于审计/撤销；撤销与命令分派天然较长，已按命令拆分"
)]
impl<'a> ReviewService<'a> {
    /// 构造服务（校验配置：撤销深度 ≥50、分页/批量 >0、只读开关合法）。
    ///
    /// # Errors
    ///
    /// 配置非法返回 [`ReviewError::Config`]。
    pub fn new(
        repo: &'a dyn ReviewRepository,
        audit: &'a dyn AuditSink,
        undo_store: &'a dyn UndoStore,
        regrade: &'a dyn RegradeEngine,
        clock: &'a dyn Clock,
        ids: &'a dyn IdGenerator,
        config: &'a ReviewConfig,
    ) -> Result<Self, ReviewError> {
        if config.undo_depth < 50 {
            return Err(ReviewError::Config(
                "撤销深度必须 ≥50（ui.undo_depth）".to_owned(),
            ));
        }
        if config.page_size == 0 {
            return Err(ReviewError::Config("分页大小必须 >0".to_owned()));
        }
        if config.max_batch == 0 {
            return Err(ReviewError::Config("批量上限必须 >0".to_owned()));
        }
        Ok(Self {
            repo,
            audit,
            undo_store,
            regrade,
            clock,
            ids,
            config,
            idempotency: Mutex::new(HashMap::new()),
            locks: Mutex::new(BTreeSet::new()),
        })
    }

    /// 队列查询（委托 `queue::build_queue`）。
    ///
    /// # Errors
    ///
    /// 游标非法返回错误。
    pub fn queue(
        &self,
        filter: &crate::model::QueueFilter,
        cursor: Option<&str>,
    ) -> Result<crate::model::QueuePage, ReviewError> {
        build_queue(self.repo, filter, cursor, self.config)
    }

    /// 详情查询。
    ///
    /// # Errors
    ///
    /// 记录不存在返回错误。
    pub fn detail(&self, grade_id: GradeId) -> Result<ReviewDetail, ReviewError> {
        build_detail(self.repo, grade_id, self.config)
    }

    /// 执行命令（幂等：同 `idempotency_key` 重复提交返回既有结果，不重复执行）。
    ///
    /// # Errors
    ///
    /// 只读/乐观锁/参数非法/重判失败等返回 [`ReviewError`]；被拒命令同样写审计。
    pub fn execute(&self, envelope: &CommandEnvelope) -> Result<CommandResult, ReviewError> {
        if let Some(cached) = self.cached(&envelope.idempotency_key) {
            return Ok(cached);
        }
        let result = self.execute_inner(envelope);
        match &result {
            Ok(result) => {
                if let Ok(mut cache) = self.idempotency.lock() {
                    cache.insert(envelope.idempotency_key.clone(), result.clone());
                }
            }
            Err(error) => {
                let _ = self.audit_rejection(envelope, error);
            }
        }
        result
    }

    fn cached(&self, key: &str) -> Option<CommandResult> {
        self.idempotency
            .lock()
            .ok()
            .and_then(|cache| cache.get(key).cloned())
    }

    fn now(&self) -> Timestamp {
        self.clock.now()
    }

    fn next_id(&self) -> GradeId {
        GradeId::from_ulid(self.ids.next_ulid())
    }

    fn derive(
        &self,
        base: &GradeRecord,
        mutate: impl FnOnce(&mut GradeRecord),
    ) -> Result<GradeRecord, ReviewError> {
        let mut record = base.clone();
        record.id = self.next_id();
        record.revision = base.revision + 1;
        record.created_at = self.now();
        mutate(&mut record);
        record
            .validate()
            .map_err(|error| ReviewError::InvalidRecord(error.to_string()))?;
        Ok(record)
    }

    fn execute_inner(&self, envelope: &CommandEnvelope) -> Result<CommandResult, ReviewError> {
        match &envelope.command {
            ReviewCommand::Undo => self.undo(),
            ReviewCommand::Redo => self.redo(),
            ReviewCommand::BatchAccept { targets } => self.batch_accept(envelope, targets),
            ReviewCommand::BatchOverrideScore { items } => {
                self.batch_override_score(envelope, items)
            }
            command => {
                let record = self.target_record(envelope)?;
                match command {
                    ReviewCommand::Accept { note } => {
                        sampling::ensure_editable(&record, self.config.sampled_readonly)?;
                        self.accept(envelope, record, note.as_deref())
                    }
                    ReviewCommand::OverrideScore {
                        score_milli,
                        reason,
                        note,
                    } => {
                        sampling::ensure_editable(&record, self.config.sampled_readonly)?;
                        self.override_score(
                            envelope,
                            record,
                            *score_milli,
                            *reason,
                            note.as_deref(),
                        )
                    }
                    ReviewCommand::RegradeText { corrected, note } => {
                        sampling::ensure_editable(&record, self.config.sampled_readonly)?;
                        self.regrade_text(envelope, record, corrected, note.as_deref())
                    }
                    ReviewCommand::ExcludeQuestion {
                        exclude_reason,
                        reason,
                        note,
                    } => {
                        sampling::ensure_editable(&record, self.config.sampled_readonly)?;
                        self.exclude_question(
                            envelope,
                            record,
                            *exclude_reason,
                            *reason,
                            note.as_deref(),
                        )
                    }
                    ReviewCommand::ExcludeSubmission { reason, note } => {
                        sampling::ensure_editable(&record, self.config.sampled_readonly)?;
                        self.exclude_submission(envelope, record, *reason, note.as_deref())
                    }
                    ReviewCommand::SamplingPass { note } => {
                        self.sampling_pass(envelope, record, note.as_deref())
                    }
                    ReviewCommand::SamplingFail {
                        scope,
                        confirmed,
                        note,
                    } => self.sampling_fail(envelope, record, *scope, *confirmed, note.as_deref()),
                    ReviewCommand::TransferToStandard { scope, note } => {
                        self.transfer_to_standard(envelope, record, *scope, note.as_deref())
                    }
                    ReviewCommand::BatchAccept { .. }
                    | ReviewCommand::BatchOverrideScore { .. }
                    | ReviewCommand::Undo
                    | ReviewCommand::Redo => {
                        unreachable!("上方已分派")
                    }
                }
            }
        }
    }

    fn target_record(&self, envelope: &CommandEnvelope) -> Result<GradeRecord, ReviewError> {
        let target = envelope
            .target
            .ok_or(ReviewError::MissingExpectedRevision)?;
        let record = self.repo.get(target).ok_or(ReviewError::NotFound(target))?;
        let expected = envelope
            .expected_revision
            .ok_or(ReviewError::MissingExpectedRevision)?;
        if record.revision != expected {
            return Err(ReviewError::StaleRevision {
                expected,
                actual: record.revision,
            });
        }
        Ok(record)
    }

    // ------------------------------------------------------------------
    // 标准队列命令
    // ------------------------------------------------------------------

    fn accept(
        &self,
        envelope: &CommandEnvelope,
        record: GradeRecord,
        note: Option<&str>,
    ) -> Result<CommandResult, ReviewError> {
        let after = self.derive(&record, |next| {
            next.status = GradeStatus::ManuallyReviewed;
            next.reviewed_at = Some(self.now());
            next.review_note = note
                .map(str::to_owned)
                .or_else(|| record.review_note.clone());
        })?;
        self.commit_record(
            envelope,
            CommandKind::Accept,
            &record,
            &after,
            None,
            note.unwrap_or(""),
            None,
            None,
            None,
            None,
        )
    }

    fn override_score(
        &self,
        envelope: &CommandEnvelope,
        record: GradeRecord,
        score_milli: i64,
        reason: crate::model::ReviewReasonCode,
        note: Option<&str>,
    ) -> Result<CommandResult, ReviewError> {
        let max = record.max_score.as_milli();
        if score_milli < 0 || score_milli > max {
            return Err(ReviewError::ScoreOutOfRange {
                awarded: score_milli,
                max,
            });
        }
        let awarded =
            MilliScore::from_milli(score_milli).map_err(|_| ReviewError::ScoreOutOfRange {
                awarded: score_milli,
                max,
            })?;
        let granularity = self.config.granularity();
        if !awarded.is_multiple_of(granularity).unwrap_or(false) {
            return Err(ReviewError::GranularityViolation {
                awarded: score_milli,
                granularity: granularity.as_milli(),
            });
        }
        let reason_text = format!("人工改分：{}", reason.as_str());
        let judgement = if awarded == record.max_score {
            Judgement::Match {
                score: record.max_score,
                reason: reason_text,
                evidence: Vec::new(),
            }
        } else if awarded.is_zero() {
            Judgement::Mismatch {
                score: MilliScore::ZERO,
                reason: reason_text,
                evidence: Vec::new(),
            }
        } else {
            Judgement::Partial {
                score: awarded,
                satisfied: Vec::new(),
                missing: Vec::new(),
                reason: reason_text,
            }
        };
        let final_note = note.map(str::to_owned);
        let after = self.derive(&record, |next| {
            next.awarded = awarded;
            next.judgement = judgement;
            next.status = GradeStatus::ManuallyReviewed;
            next.reviewed_at = Some(self.now());
            next.review_note.clone_from(&final_note);
        })?;
        self.commit_record(
            envelope,
            CommandKind::OverrideScore,
            &record,
            &after,
            Some(reason),
            note.unwrap_or(""),
            None,
            None,
            None,
            None,
        )
    }

    fn regrade_text(
        &self,
        envelope: &CommandEnvelope,
        record: GradeRecord,
        corrected: &CorrectedText,
        note: Option<&str>,
    ) -> Result<CommandResult, ReviewError> {
        if corrected.text.trim().is_empty() {
            return Err(ReviewError::Config("修正文本不得为空".to_owned()));
        }
        let _guard = self.lock_question(record.submission_id, record.question_id)?;
        self.run_regrade(
            envelope,
            record,
            GradeMode::Standard,
            Some(corrected.clone()),
            RegradeScope::Single,
            Some(crate::model::ReviewReasonCode::RecognitionError),
            CommandKind::RegradeText,
            note.unwrap_or(""),
            None,
            None,
        )
    }

    fn exclude_question(
        &self,
        envelope: &CommandEnvelope,
        record: GradeRecord,
        exclude_reason: ExcludeReason,
        reason: crate::model::ReviewReasonCode,
        note: Option<&str>,
    ) -> Result<CommandResult, ReviewError> {
        let after = self.derive(&record, |next| {
            next.status = GradeStatus::Excluded;
            next.reviewed_at = Some(self.now());
            next.review_note = note.map(str::to_owned);
        })?;
        let detail = format!("题级排除：{exclude_reason:?}");
        self.commit_record(
            envelope,
            CommandKind::ExcludeQuestion,
            &record,
            &after,
            Some(reason),
            note.unwrap_or(""),
            None,
            None,
            Some(detail),
            None,
        )
    }

    fn exclude_submission(
        &self,
        envelope: &CommandEnvelope,
        record: GradeRecord,
        reason: ExcludeReason,
        note: Option<&str>,
    ) -> Result<CommandResult, ReviewError> {
        let before = self.repo.exclusion(record.submission_id);
        let after = SubmissionExclusion {
            submission_id: record.submission_id,
            reason,
            note: note.unwrap_or("").to_owned(),
            at: self.now(),
        };
        self.repo.save_exclusion(after.clone())?;
        let audit_id = self.emit(AuditRecord {
            outcome: AuditOutcome::Applied,
            entity_type: "submission".to_owned(),
            entity_id: record.submission_id.to_string(),
            submission_id: record.submission_id,
            grade_id: Some(record.id),
            before: Some(record.clone()),
            after_exclusion: Some(after.clone()),
            before_exclusion: before.clone(),
            note: note.unwrap_or("").to_owned(),
            ..self.audit_base(envelope, CommandKind::ExcludeSubmission, Some(&record))
        })?;
        self.undo_store.push_undo(UndoEntry::new(
            UndoKind::SubmissionExclusion {
                submission_id: record.submission_id,
                before,
                after,
            },
            "提交级排除",
        ));
        self.undo_store.clear_redo();
        Ok(CommandResult {
            audit_id,
            outcome: CommandOutcome::SubmissionExcluded {
                submission_id: record.submission_id,
            },
        })
    }

    // ------------------------------------------------------------------
    // 抽检/转标准命令（实现见 sampling/transfer 辅助模块）
    // ------------------------------------------------------------------

    fn sampling_pass(
        &self,
        envelope: &CommandEnvelope,
        record: GradeRecord,
        note: Option<&str>,
    ) -> Result<CommandResult, ReviewError> {
        sampling::ensure_sampling_item(&record)?;
        let audit_id = self.emit(AuditRecord {
            outcome: AuditOutcome::Noop,
            sampling_conclusion: Some(SamplingConclusion::Passed),
            note: note.unwrap_or("").to_owned(),
            before: Some(record.clone()),
            after: Some(record.clone()),
            ..self.audit_base(envelope, CommandKind::SamplingPass, Some(&record))
        })?;
        self.undo_store.push_undo(UndoEntry::new(
            UndoKind::SamplingConclusion {
                grade_id: record.id,
                conclusion: SamplingConclusion::Passed,
            },
            "抽检通过",
        ));
        self.undo_store.clear_redo();
        Ok(CommandResult {
            audit_id,
            outcome: CommandOutcome::SamplingPassed {
                grade_id: record.id,
            },
        })
    }

    fn sampling_fail(
        &self,
        envelope: &CommandEnvelope,
        record: GradeRecord,
        scope: RegradeScope,
        confirmed: bool,
        note: Option<&str>,
    ) -> Result<CommandResult, ReviewError> {
        sampling::ensure_sampling_item(&record)?;
        // 未确认：不重跑，返回弹窗协议事件并写「待处理」审计
        if !confirmed {
            let prompt = sampling::build_prompt(&record, self.config.fail_default_scope);
            let audit_id = self.emit(AuditRecord {
                outcome: AuditOutcome::Noop,
                sampling_conclusion: Some(SamplingConclusion::Failed),
                scope: Some(scope),
                confirmed: Some(false),
                note: note.unwrap_or("").to_owned(),
                detail: Some("待确认：未重跑".to_owned()),
                before: Some(record.clone()),
                after: Some(record.clone()),
                ..self.audit_base(envelope, CommandKind::SamplingFail, Some(&record))
            })?;
            return Ok(CommandResult {
                audit_id,
                outcome: CommandOutcome::SamplingPrompt { prompt },
            });
        }
        let action_note = if note.is_some_and(|text| !text.is_empty()) {
            format!("{}（抽检不通过，确认重跑）", note.unwrap_or(""))
        } else {
            "抽检不通过，确认重跑".to_owned()
        };
        self.run_regrade(
            envelope,
            record,
            GradeMode::Fast,
            None,
            scope,
            Some(crate::model::ReviewReasonCode::ModelMisjudged),
            CommandKind::SamplingFail,
            &action_note,
            Some(confirmed),
            Some(SamplingConclusion::Failed),
        )
    }

    fn transfer_to_standard(
        &self,
        envelope: &CommandEnvelope,
        record: GradeRecord,
        scope: RegradeScope,
        note: Option<&str>,
    ) -> Result<CommandResult, ReviewError> {
        transfer::ensure_transferable(&record)?;
        let request = RegradeRequest {
            record: record.clone(),
            mode: GradeMode::Standard,
            corrected: None,
            scope,
            reason: None,
            idempotency_key: regrade_key(&record, GradeMode::Standard, None, scope),
        };
        let result = self.regrade.regrade(&request)?;
        if let Some(task_id) = result.task_id {
            let audit_id = self.emit(AuditRecord {
                outcome: AuditOutcome::Applied,
                scope: Some(scope),
                note: note.unwrap_or("").to_owned(),
                before: Some(record.clone()),
                detail: Some(format!("整卷转标准任务：{task_id}")),
                ..self.audit_base(envelope, CommandKind::TransferToStandard, Some(&record))
            })?;
            return Ok(CommandResult {
                audit_id,
                outcome: CommandOutcome::RegradeScheduled { task_id, scope },
            });
        }
        let mut after = transfer::standard_record_from_result(&result, &record)?;
        after.id = self.next_id();
        after.revision = record.revision + 1;
        after.created_at = self.now();
        after
            .validate()
            .map_err(|error| ReviewError::InvalidRecord(error.to_string()))?;
        self.commit_record(
            envelope,
            CommandKind::TransferToStandard,
            &record,
            &after,
            None,
            note.unwrap_or(""),
            Some(scope),
            None,
            if result.fallback_to_queue {
                Some("无法重判：进入标准待复核队列".to_owned())
            } else {
                None
            },
            None,
        )
    }

    // ------------------------------------------------------------------
    // 重判（含幂等键）
    // ------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn run_regrade(
        &self,
        envelope: &CommandEnvelope,
        record: GradeRecord,
        mode: GradeMode,
        corrected: Option<CorrectedText>,
        scope: RegradeScope,
        reason: Option<crate::model::ReviewReasonCode>,
        kind: CommandKind,
        note: &str,
        confirmed: Option<bool>,
        sampling_conclusion: Option<SamplingConclusion>,
    ) -> Result<CommandResult, ReviewError> {
        let request = RegradeRequest {
            record: record.clone(),
            mode,
            corrected: corrected.clone(),
            scope,
            reason,
            idempotency_key: regrade_key(&record, mode, corrected.as_ref(), scope),
        };
        let result = self.regrade.regrade(&request)?;
        if let Some(task_id) = result.task_id {
            let audit_id = self.emit(AuditRecord {
                outcome: AuditOutcome::Applied,
                scope: Some(scope),
                confirmed,
                sampling_conclusion,
                note: note.to_owned(),
                before: Some(record.clone()),
                detail: Some(format!("整卷重跑任务：{task_id}")),
                ..self.audit_base(envelope, kind, Some(&record))
            })?;
            return Ok(CommandResult {
                audit_id,
                outcome: CommandOutcome::RegradeScheduled { task_id, scope },
            });
        }
        let mut after = result
            .record
            .ok_or_else(|| ReviewError::RegradeFailed("重判引擎未返回记录".to_owned()))?;
        if result.fallback_to_queue {
            after.status = GradeStatus::NeedsReview;
        }
        if after.revision <= record.revision {
            after.revision = record.revision + 1;
        }
        after.id = self.next_id();
        after.created_at = self.now();
        after
            .validate()
            .map_err(|error| ReviewError::InvalidRecord(error.to_string()))?;
        self.commit_record(
            envelope,
            kind,
            &record,
            &after,
            reason,
            note,
            Some(scope),
            confirmed,
            sampling_conclusion.map(|conclusion| format!("抽检结论：{conclusion:?}")),
            sampling_conclusion,
        )
    }

    // ------------------------------------------------------------------
    // 批量
    // ------------------------------------------------------------------

    fn batch_accept(
        &self,
        envelope: &CommandEnvelope,
        targets: &[BatchTarget],
    ) -> Result<CommandResult, ReviewError> {
        if targets.len() > self.config.max_batch {
            return Err(ReviewError::BatchTooLarge {
                count: targets.len(),
                limit: self.config.max_batch,
            });
        }
        if targets.is_empty() {
            return Err(ReviewError::Config("批量目标不得为空".to_owned()));
        }
        let mut succeeded = Vec::new();
        let mut failed = Vec::new();
        let mut audit_ids = Vec::new();
        for target in targets {
            let sub = CommandEnvelope {
                target: Some(target.grade_id),
                idempotency_key: format!("{}#{}", envelope.idempotency_key, target.grade_id),
                expected_revision: Some(target.expected_revision),
                command: ReviewCommand::Accept { note: None },
            };
            match self.execute_inner(&sub) {
                Ok(result) => {
                    audit_ids.push(result.audit_id);
                    succeeded.push(result);
                }
                Err(error) => {
                    let _ = self.audit_rejection(&sub, &error);
                    failed.push(BatchFailure {
                        grade_id: target.grade_id,
                        error,
                    });
                }
            }
        }
        let submission_id = succeeded
            .first()
            .and_then(|_| targets.first())
            .and_then(|target| self.repo.get(target.grade_id))
            .map_or_else(
                || SubmissionId::from_ulid(Ulid::NIL),
                |record| record.submission_id,
            );
        let summary = self.emit(AuditRecord {
            outcome: if failed.is_empty() {
                AuditOutcome::Applied
            } else {
                AuditOutcome::Rejected
            },
            entity_type: "batch".to_owned(),
            entity_id: envelope.idempotency_key.clone(),
            submission_id,
            note: format!(
                "批量接受：成功 {} 项，失败 {} 项",
                succeeded.len(),
                failed.len()
            ),
            ..self.audit_base(envelope, CommandKind::BatchAccept, None)
        })?;
        audit_ids.push(summary);
        Ok(CommandResult {
            audit_id: summary,
            outcome: CommandOutcome::Batch {
                batch: BatchOutcome {
                    succeeded,
                    failed,
                    audit_ids,
                },
            },
        })
    }

    fn batch_override_score(
        &self,
        envelope: &CommandEnvelope,
        items: &[crate::model::BatchScoreTarget],
    ) -> Result<CommandResult, ReviewError> {
        if items.len() > self.config.max_batch {
            return Err(ReviewError::BatchTooLarge {
                count: items.len(),
                limit: self.config.max_batch,
            });
        }
        if items.is_empty() {
            return Err(ReviewError::Config("批量目标不得为空".to_owned()));
        }
        let mut succeeded = Vec::new();
        let mut failed = Vec::new();
        let mut audit_ids = Vec::new();
        for item in items {
            let sub = CommandEnvelope {
                target: Some(item.target.grade_id),
                idempotency_key: format!("{}#{}", envelope.idempotency_key, item.target.grade_id),
                expected_revision: Some(item.target.expected_revision),
                command: ReviewCommand::OverrideScore {
                    score_milli: item.score_milli,
                    reason: item.reason,
                    note: Some("批量改分".to_owned()),
                },
            };
            match self.execute_inner(&sub) {
                Ok(result) => {
                    audit_ids.push(result.audit_id);
                    succeeded.push(result);
                }
                Err(error) => {
                    let _ = self.audit_rejection(&sub, &error);
                    failed.push(BatchFailure {
                        grade_id: item.target.grade_id,
                        error,
                    });
                }
            }
        }
        let submission_id = items
            .first()
            .and_then(|item| self.repo.get(item.target.grade_id))
            .map_or_else(
                || SubmissionId::from_ulid(Ulid::NIL),
                |record| record.submission_id,
            );
        let summary = self.emit(AuditRecord {
            outcome: if failed.is_empty() {
                AuditOutcome::Applied
            } else {
                AuditOutcome::Rejected
            },
            entity_type: "batch".to_owned(),
            entity_id: envelope.idempotency_key.clone(),
            submission_id,
            note: format!(
                "批量改分：成功 {} 项，失败 {} 项",
                succeeded.len(),
                failed.len()
            ),
            ..self.audit_base(envelope, CommandKind::BatchOverrideScore, None)
        })?;
        audit_ids.push(summary);
        Ok(CommandResult {
            audit_id: summary,
            outcome: CommandOutcome::Batch {
                batch: BatchOutcome {
                    succeeded,
                    failed,
                    audit_ids,
                },
            },
        })
    }

    // ------------------------------------------------------------------
    // 撤销/重做
    // ------------------------------------------------------------------

    fn undo(&self) -> Result<CommandResult, ReviewError> {
        let entry = self
            .undo_store
            .pop_undo()
            .ok_or(ReviewError::NothingToUndo)?;
        match &entry.kind {
            UndoKind::Record {
                grade_id,
                before,
                after,
            } => {
                let anchor = self
                    .repo
                    .get(*grade_id)
                    .ok_or(ReviewError::NotFound(*grade_id))?;
                let current = self.latest_for(&anchor)?;
                if !same_state(&current, after) {
                    return Err(ReviewError::UndoCorrupted(
                        "当前记录已变化，无法撤销该步".to_owned(),
                    ));
                }
                let restored = self.restore(&current, before)?;
                self.repo.append(restored.clone())?;
                self.undo_store.push_redo(UndoEntry::new(
                    UndoKind::Record {
                        grade_id: restored.id,
                        before: restored.clone(),
                        after: after.clone(),
                    },
                    entry.label.clone(),
                ));
                let audit_id = self.emit(AuditRecord {
                    outcome: AuditOutcome::Applied,
                    action: CommandKind::Undo.action().to_owned(),
                    entity_type: "grade_record".to_owned(),
                    entity_id: restored.id.to_string(),
                    submission_id: restored.submission_id,
                    question_id: Some(restored.question_id),
                    grade_id: Some(restored.id),
                    idempotency_key: format!("undo:{}", restored.id),
                    note: entry.label.clone(),
                    before: Some(current),
                    after: Some(restored.clone()),
                    ..self.audit_base_raw()
                })?;
                return Ok(CommandResult {
                    audit_id,
                    outcome: CommandOutcome::Undone {
                        grade_id: Some(restored.id),
                        undone: true,
                    },
                });
            }
            UndoKind::SubmissionExclusion {
                submission_id,
                before,
                after,
            } => {
                let current = self.repo.exclusion(*submission_id);
                if current.as_ref() != Some(after) {
                    return Err(ReviewError::UndoCorrupted(
                        "当前提交排除状态已变化".to_owned(),
                    ));
                }
                if let Some(before) = before {
                    self.repo.save_exclusion(before.clone())?;
                } else {
                    self.repo.clear_exclusion(*submission_id)?;
                }
                self.undo_store.push_redo(UndoEntry::new(
                    UndoKind::SubmissionExclusion {
                        submission_id: *submission_id,
                        before: before.clone(),
                        after: after.clone(),
                    },
                    entry.label.clone(),
                ));
                let audit_id = self.emit(AuditRecord {
                    outcome: AuditOutcome::Applied,
                    action: CommandKind::Undo.action().to_owned(),
                    entity_type: "submission".to_owned(),
                    entity_id: submission_id.to_string(),
                    submission_id: *submission_id,
                    idempotency_key: format!("undo:submission:{submission_id}"),
                    note: entry.label.clone(),
                    before_exclusion: Some(after.clone()),
                    after_exclusion: before.clone(),
                    ..self.audit_base_raw()
                })?;
                return Ok(CommandResult {
                    audit_id,
                    outcome: CommandOutcome::Undone {
                        grade_id: None,
                        undone: true,
                    },
                });
            }
            UndoKind::SamplingConclusion {
                grade_id,
                conclusion,
            } => {
                self.undo_store.push_redo(UndoEntry::new(
                    UndoKind::SamplingConclusion {
                        grade_id: *grade_id,
                        conclusion: *conclusion,
                    },
                    entry.label.clone(),
                ));
                let audit_id = self.emit(AuditRecord {
                    outcome: AuditOutcome::Noop,
                    action: CommandKind::Undo.action().to_owned(),
                    entity_type: "grade_record".to_owned(),
                    entity_id: grade_id.to_string(),
                    submission_id: SubmissionId::from_ulid(Ulid::NIL),
                    grade_id: Some(*grade_id),
                    idempotency_key: format!("undo:sampling:{grade_id}"),
                    note: entry.label.clone(),
                    sampling_conclusion: Some(*conclusion),
                    ..self.audit_base_raw()
                })?;
                return Ok(CommandResult {
                    audit_id,
                    outcome: CommandOutcome::Undone {
                        grade_id: Some(*grade_id),
                        undone: true,
                    },
                });
            }
        }
    }

    fn redo(&self) -> Result<CommandResult, ReviewError> {
        let entry = self
            .undo_store
            .pop_redo()
            .ok_or(ReviewError::NothingToRedo)?;
        match entry.kind {
            UndoKind::Record {
                grade_id,
                before,
                after,
            } => {
                let anchor = self
                    .repo
                    .get(grade_id)
                    .ok_or(ReviewError::NotFound(grade_id))?;
                let current = self.latest_for(&anchor)?;
                if !same_state(&current, &before) {
                    return Err(ReviewError::UndoCorrupted(
                        "当前记录已变化，无法重做该步".to_owned(),
                    ));
                }
                let redone = self.restore(&current, &after)?;
                self.repo.append(redone.clone())?;
                self.undo_store.push_undo(UndoEntry::new(
                    UndoKind::Record {
                        grade_id: redone.id,
                        before: current.clone(),
                        after: redone.clone(),
                    },
                    entry.label.clone(),
                ));
                let audit_id = self.emit(AuditRecord {
                    outcome: AuditOutcome::Applied,
                    action: CommandKind::Redo.action().to_owned(),
                    entity_type: "grade_record".to_owned(),
                    entity_id: redone.id.to_string(),
                    submission_id: redone.submission_id,
                    question_id: Some(redone.question_id),
                    grade_id: Some(redone.id),
                    idempotency_key: format!("redo:{}", redone.id),
                    note: entry.label.clone(),
                    before: Some(current),
                    after: Some(redone.clone()),
                    ..self.audit_base_raw()
                })?;
                Ok(CommandResult {
                    audit_id,
                    outcome: CommandOutcome::Undone {
                        grade_id: Some(redone.id),
                        undone: false,
                    },
                })
            }
            UndoKind::SubmissionExclusion {
                submission_id,
                before,
                after,
            } => {
                let current = self.repo.exclusion(submission_id);
                if current.as_ref() != before.as_ref() {
                    return Err(ReviewError::UndoCorrupted(
                        "当前提交排除状态已变化".to_owned(),
                    ));
                }
                self.repo.save_exclusion(after.clone())?;
                self.undo_store.push_undo(UndoEntry::new(
                    UndoKind::SubmissionExclusion {
                        submission_id,
                        before,
                        after: after.clone(),
                    },
                    entry.label.clone(),
                ));
                let audit_id = self.emit(AuditRecord {
                    outcome: AuditOutcome::Applied,
                    action: CommandKind::Redo.action().to_owned(),
                    entity_type: "submission".to_owned(),
                    entity_id: submission_id.to_string(),
                    submission_id,
                    idempotency_key: format!("redo:submission:{submission_id}"),
                    note: entry.label.clone(),
                    before_exclusion: None,
                    after_exclusion: Some(after),
                    ..self.audit_base_raw()
                })?;
                Ok(CommandResult {
                    audit_id,
                    outcome: CommandOutcome::Undone {
                        grade_id: None,
                        undone: false,
                    },
                })
            }
            UndoKind::SamplingConclusion {
                grade_id,
                conclusion,
            } => {
                self.undo_store.push_undo(UndoEntry::new(
                    UndoKind::SamplingConclusion {
                        grade_id,
                        conclusion,
                    },
                    entry.label.clone(),
                ));
                let audit_id = self.emit(AuditRecord {
                    outcome: AuditOutcome::Noop,
                    action: CommandKind::Redo.action().to_owned(),
                    entity_type: "grade_record".to_owned(),
                    entity_id: grade_id.to_string(),
                    submission_id: SubmissionId::from_ulid(Ulid::NIL),
                    grade_id: Some(grade_id),
                    idempotency_key: format!("redo:sampling:{grade_id}"),
                    note: entry.label.clone(),
                    sampling_conclusion: Some(conclusion),
                    ..self.audit_base_raw()
                })?;
                Ok(CommandResult {
                    audit_id,
                    outcome: CommandOutcome::Undone {
                        grade_id: Some(grade_id),
                        undone: false,
                    },
                })
            }
        }
    }

    fn restore(
        &self,
        current: &GradeRecord,
        snapshot: &GradeRecord,
    ) -> Result<GradeRecord, ReviewError> {
        self.derive(current, |next| {
            next.awarded = snapshot.awarded;
            next.judgement = snapshot.judgement.clone();
            next.status = snapshot.status;
            next.mode = snapshot.mode;
            next.sampled = snapshot.sampled;
            next.confidence = snapshot.confidence;
            next.reviewed_at = snapshot.reviewed_at;
            next.review_note.clone_from(&snapshot.review_note);
        })
    }

    // ------------------------------------------------------------------
    // 通用：提交/审计/锁
    // ------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn commit_record(
        &self,
        envelope: &CommandEnvelope,
        kind: CommandKind,
        before: &GradeRecord,
        after: &GradeRecord,
        reason: Option<crate::model::ReviewReasonCode>,
        note: &str,
        scope: Option<RegradeScope>,
        confirmed: Option<bool>,
        detail: Option<String>,
        sampling_conclusion: Option<SamplingConclusion>,
    ) -> Result<CommandResult, ReviewError> {
        self.repo.append(after.clone())?;
        let audit_id = self.emit(AuditRecord {
            outcome: AuditOutcome::Applied,
            before: Some(before.clone()),
            after: Some(after.clone()),
            reason,
            note: note.to_owned(),
            scope,
            confirmed,
            detail,
            sampling_conclusion,
            ..self.audit_base(envelope, kind, Some(after))
        })?;
        self.undo_store.push_undo(UndoEntry::new(
            UndoKind::Record {
                grade_id: after.id,
                before: before.clone(),
                after: after.clone(),
            },
            kind.action().to_owned(),
        ));
        self.undo_store.clear_redo();
        Ok(CommandResult {
            audit_id,
            outcome: CommandOutcome::Record {
                grade_id: after.id,
                revision: after.revision,
                status: after.status,
                mode: after.mode,
            },
        })
    }

    fn audit_base(
        &self,
        envelope: &CommandEnvelope,
        kind: CommandKind,
        record: Option<&GradeRecord>,
    ) -> AuditRecord {
        AuditRecord {
            id: Ulid::NIL,
            at: self.now(),
            actor: ACTOR.to_owned(),
            action: kind.action().to_owned(),
            entity_type: "grade_record".to_owned(),
            entity_id: record.map_or_else(
                || {
                    envelope
                        .target
                        .map_or_else(String::new, |target| target.to_string())
                },
                |record| record.id.to_string(),
            ),
            submission_id: record.map_or_else(
                || SubmissionId::from_ulid(Ulid::NIL),
                |record| record.submission_id,
            ),
            question_id: record.map(|record| record.question_id),
            grade_id: record.map(|record| record.id),
            idempotency_key: envelope.idempotency_key.clone(),
            outcome: AuditOutcome::Applied,
            before: None,
            after: None,
            before_exclusion: None,
            after_exclusion: None,
            reason: None,
            note: String::new(),
            scope: None,
            confirmed: None,
            sampling_conclusion: None,
            detail: None,
        }
    }

    fn audit_base_raw(&self) -> AuditRecord {
        AuditRecord {
            id: Ulid::NIL,
            at: self.now(),
            actor: ACTOR.to_owned(),
            action: String::new(),
            entity_type: String::new(),
            entity_id: String::new(),
            submission_id: SubmissionId::from_ulid(Ulid::NIL),
            question_id: None,
            grade_id: None,
            idempotency_key: String::new(),
            outcome: AuditOutcome::Applied,
            before: None,
            after: None,
            before_exclusion: None,
            after_exclusion: None,
            reason: None,
            note: String::new(),
            scope: None,
            confirmed: None,
            sampling_conclusion: None,
            detail: None,
        }
    }

    fn emit(&self, mut record: AuditRecord) -> Result<Ulid, ReviewError> {
        record.id = self.ids.next_ulid();
        record.at = self.now();
        let id = record.id;
        self.audit.append(record)?;
        Ok(id)
    }

    fn audit_rejection(
        &self,
        envelope: &CommandEnvelope,
        error: &ReviewError,
    ) -> Result<(), ReviewError> {
        let record = envelope.target.and_then(|target| self.repo.get(target));
        let record_ref = record.as_ref();
        let mut base = self.audit_base(envelope, envelope.command.kind(), record_ref);
        base.outcome = AuditOutcome::Rejected;
        base.detail = Some(format!("{}: {}", error.code(), error));
        base.note = String::new();
        self.emit(base)?;
        Ok(())
    }

    fn latest_for(&self, anchor: &GradeRecord) -> Result<GradeRecord, ReviewError> {
        self.repo
            .revisions(anchor.submission_id, anchor.question_id)
            .pop()
            .ok_or(ReviewError::NotFound(anchor.id))
    }

    fn lock_question(
        &self,
        submission_id: SubmissionId,
        question_id: eg_domain::QuestionId,
    ) -> Result<QuestionLockGuard<'_>, ReviewError> {
        let mut locks = self
            .locks
            .lock()
            .map_err(|_| ReviewError::Repository("题级锁失败".to_owned()))?;
        let key = (submission_id, question_id);
        if !locks.insert(key) {
            return Err(ReviewError::RegradeInProgress);
        }
        Ok(QuestionLockGuard {
            locks: &self.locks,
            key,
        })
    }
}

/// 记录可变状态是否等价（撤销/重做栈校验；id/revision 不参与）。
fn same_state(left: &GradeRecord, right: &GradeRecord) -> bool {
    left.awarded == right.awarded
        && left.judgement == right.judgement
        && left.status == right.status
        && left.mode == right.mode
        && left.sampled == right.sampled
        && left.confidence == right.confidence
        && left.review_note == right.review_note
}

/// 题级锁守卫（Drop 释放）。
struct QuestionLockGuard<'a> {
    locks: &'a Mutex<BTreeSet<QuestionKey>>,
    key: QuestionKey,
}

impl Drop for QuestionLockGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut locks) = self.locks.lock() {
            locks.remove(&self.key);
        }
    }
}

/// 重判幂等键（经 grade 幂等语义：提交+题+模式+修正文本；不含学生姓名）。
#[must_use]
pub fn regrade_key(
    record: &GradeRecord,
    mode: GradeMode,
    corrected: Option<&CorrectedText>,
    scope: RegradeScope,
) -> String {
    let material = format!(
        "{}|{}|{:?}|{}|{}|{}",
        record.submission_id,
        record.question_id,
        mode,
        scope.as_str(),
        corrected.map_or("", |text| text.text.as_str()),
        corrected
            .and_then(|text| text.latex.as_deref())
            .unwrap_or("")
    );
    Sha256Hex::of(material.as_bytes()).to_hex()
}

/// 队列条目辅助（测试/UI）：当前记录的分类。
#[must_use]
pub fn default_item(
    repo: &dyn ReviewRepository,
    record: &GradeRecord,
    config: &ReviewConfig,
) -> Option<crate::model::QueueItem> {
    let kind = classify_record(record, config)?;
    let metadata: ReviewMetadata = repo.metadata(record);
    Some(item_for(record, kind, &metadata, config))
}

/// 原因集合辅助（测试/UI）。
#[must_use]
pub fn reasons_for(record: &GradeRecord, config: &ReviewConfig) -> Vec<ReviewReasonKind> {
    queue_reasons(record, config)
}
