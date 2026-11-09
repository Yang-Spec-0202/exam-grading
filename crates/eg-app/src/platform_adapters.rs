//! platform v3 真实适配器：把 `eg-review` 的端口 trait 落到 platform 仓储/审计/撤销栈。
//!
//! 只做字段映射与事务组合，不写复核业务规则（规则仍在 `eg-review::ReviewService`）。

use eg_domain::{
    Clock, ComparatorTrace, ComparatorTraceId, ExcludeReason, GradeId, GradeRecord, ModelCall,
    ModelCallId, QuestionId, SubmissionExclusion, SubmissionId, Timestamp, Ulid,
};
use eg_grade::ports::{IdGenerator, IdSourceError};
use eg_platform::Platform;
use eg_platform::repo;
use eg_review::undo::UndoKind;
use eg_review::{
    AnswerView, AuditOutcome, AuditRecord, AuditSink, RegradeEngine, RegradeRequest, RegradeResult,
    ReviewError, ReviewMetadata, ReviewRepository, SamplingConclusion, UndoEntry, UndoStore,
};

use crate::nav::PageId;

/// 平台 id 生成器适配（`eg-grade` 端口 → platform `UlidFactory`）。
pub(crate) struct PlatformIdGen<'a> {
    platform: &'a Platform,
}

impl<'a> PlatformIdGen<'a> {
    pub(crate) const fn new(platform: &'a Platform) -> Self {
        Self { platform }
    }
}

impl IdGenerator for PlatformIdGen<'_> {
    fn try_next_ulid(&self) -> Result<Ulid, IdSourceError> {
        self.platform
            .ids()
            .next_ulid()
            .map_err(|error| IdSourceError(error.to_string()))
    }
}

/// 复核数据源：platform 真实仓储。
pub(crate) struct PlatformReviewRepo<'a> {
    platform: &'a Platform,
}

impl<'a> PlatformReviewRepo<'a> {
    pub(crate) const fn new(platform: &'a Platform) -> Self {
        Self { platform }
    }
}

fn io_error(error: eg_platform::PlatformError) -> ReviewError {
    ReviewError::Repository(error.to_string())
}

impl ReviewRepository for PlatformReviewRepo<'_> {
    fn get(&self, grade_id: GradeId) -> Option<GradeRecord> {
        self.platform
            .db()
            .read(|conn| repo::get_grade_record(conn, grade_id))
            .ok()
            .flatten()
    }

    fn revisions(&self, submission_id: SubmissionId, question_id: QuestionId) -> Vec<GradeRecord> {
        self.platform
            .db()
            .read(|conn| repo::list_grade_records(conn, submission_id))
            .unwrap_or_default()
            .into_iter()
            .filter(|record| record.question_id == question_id)
            .collect()
    }

    fn all_latest(&self) -> Vec<GradeRecord> {
        let filter = repo::GradeQueueFilter::default();
        self.platform
            .db()
            .read(|conn| repo::list_grade_records_for_queue(conn, &filter, None))
            .unwrap_or_default()
    }

    fn append(&self, record: GradeRecord) -> Result<(), ReviewError> {
        let record_clone = record.clone();
        self.platform
            .db()
            .write(move |tx| repo::append_grade_record_with_reasons(tx, &record_clone, &[]))
            .map_err(io_error)
    }

    fn save_exclusion(&self, exclusion: SubmissionExclusion) -> Result<(), ReviewError> {
        self.platform
            .db()
            .write(move |tx| repo::upsert_submission_exclusion(tx, &exclusion))
            .map_err(io_error)
    }

    fn clear_exclusion(&self, submission_id: SubmissionId) -> Result<(), ReviewError> {
        self.platform
            .db()
            .write(move |tx| repo::delete_submission_exclusion(tx, submission_id))
            .map(|_changed| ())
            .map_err(io_error)
    }

    fn exclusion(&self, submission_id: SubmissionId) -> Option<SubmissionExclusion> {
        self.platform
            .db()
            .read(|conn| repo::get_submission_exclusion(conn, submission_id))
            .ok()
            .flatten()
    }

    fn metadata(&self, record: &GradeRecord) -> ReviewMetadata {
        // 单次读事务内取提交与题目标签，避免嵌套 db.read（读连接是不可重入 Mutex）。
        let data = self
            .platform
            .db()
            .read(|conn| {
                let submission = repo::get_submission(conn, record.submission_id)?;
                let label = match &submission {
                    Some(submission) => repo::list_questions(conn, submission.paper_id)?
                        .into_iter()
                        .find(|question| question.id == record.question_id)
                        .map(|question| (question.label, question.ordinal)),
                    None => None,
                };
                Ok((submission, label))
            })
            .unwrap_or((None, None));
        let (submission, label) = data;
        let display = submission.as_ref().map_or_else(
            || "学生***".to_owned(),
            |submission| submission.source_stem.clone(),
        );
        ReviewMetadata {
            student_no: submission.map_or_else(|| "unknown".to_owned(), |s| s.source_stem),
            student_display_name: mask_name(&display),
            question_label: label
                .as_ref()
                .map_or_else(|| "?".to_owned(), |(l, _)| l.clone()),
            question_ordinal: label.map_or(0, |(_, ordinal)| ordinal),
        }
    }

    fn answer(&self, record: &GradeRecord) -> Option<AnswerView> {
        let summary = self
            .platform
            .db()
            .read(|conn| repo::load_grade_evidence_summary(conn, record.id))
            .ok()
            .flatten()?;
        summary.recognitions.first().map(|recognition| AnswerView {
            text: recognition.text.clone(),
            latex: None,
            confidence: eg_domain::Confidence::new(recognition.confidence).ok(),
            source: "recognition".to_owned(),
        })
    }

    fn reference(&self, question_id: QuestionId) -> Option<AnswerView> {
        // 参考答案文本由试卷答案键/识别提供；当前 platform 无按题投影，返回 None 由 UI 降级。
        let _ = question_id;
        None
    }

    fn comparator_trace(&self, id: ComparatorTraceId) -> Option<ComparatorTrace> {
        let _ = id;
        None
    }

    fn model_call(&self, id: ModelCallId) -> Option<ModelCall> {
        let _ = id;
        None
    }
}

fn mask_name(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => format!("{first}***"),
        None => "学生***".to_owned(),
    }
}

/// 复核审计：platform 审计链（命令幂等键、原因码、范围、抽检结论）。
pub(crate) struct PlatformAuditSink<'a> {
    platform: &'a Platform,
}

impl<'a> PlatformAuditSink<'a> {
    pub(crate) const fn new(platform: &'a Platform) -> Self {
        Self { platform }
    }
}

impl AuditSink for PlatformAuditSink<'_> {
    fn append(&self, record: AuditRecord) -> Result<(), ReviewError> {
        let mut entry = eg_platform::NewAuditEntry::new(
            record.action.clone(),
            record.entity_type.clone(),
            record.entity_id.clone(),
        )
        .with_command_id(record.idempotency_key.clone())
        .with_note(record.note.clone());
        if let Some(reason) = record.reason {
            entry = entry.with_reason_code(reason.as_str());
        }
        if let Some(scope) = record.scope {
            entry = entry.with_scope(format!("{scope:?}"));
        }
        if let Some(verdict) = record.sampling_conclusion {
            entry = entry.with_sampling_verdict(match verdict {
                SamplingConclusion::Passed => "passed",
                SamplingConclusion::Failed => "failed",
            });
        }
        if let Some(detail) = &record.detail {
            entry = entry.with_note(format!("{}；{}", record.note, detail));
        }
        self.platform
            .audit()
            .append(entry)
            .map(|_| ())
            .map_err(|error| ReviewError::Audit(error.to_string()))
    }

    fn list(&self) -> Vec<AuditRecord> {
        Vec::new()
    }

    fn by_entity(&self, _entity_type: &str, _entity_id: &str) -> Vec<AuditRecord> {
        Vec::new()
    }
}

/// 撤销/重做栈：undo 与 redo 都持久化到 platform v4（重启后可继续撤销/重做）。
pub(crate) struct PlatformUndoStore<'a> {
    platform: &'a Platform,
    depth: usize,
}

impl<'a> PlatformUndoStore<'a> {
    pub(crate) fn new(platform: &'a Platform, depth: usize) -> Self {
        Self {
            platform,
            depth: depth.max(50),
        }
    }

    fn encode(entry: &UndoEntry) -> (String, String, String, serde_json::Value) {
        let entity_id = match &entry.kind {
            UndoKind::Record { grade_id, .. } => grade_id.to_string(),
            UndoKind::SamplingConclusion { grade_id, .. } => grade_id.to_string(),
            UndoKind::SubmissionExclusion { submission_id, .. } => submission_id.to_string(),
        };
        let entity_type = match &entry.kind {
            UndoKind::Record { .. } | UndoKind::SamplingConclusion { .. } => "grade_record",
            UndoKind::SubmissionExclusion { .. } => "submission",
        };
        let payload = match &entry.kind {
            UndoKind::Record {
                grade_id,
                before,
                after,
            } => serde_json::json!({
                "kind": "record",
                "grade_id": grade_id.to_string(),
                "before": before,
                "after": after,
            }),
            UndoKind::SamplingConclusion {
                grade_id,
                conclusion,
            } => serde_json::json!({
                "kind": "sampling",
                "grade_id": grade_id.to_string(),
                "conclusion": match conclusion {
                    SamplingConclusion::Passed => "passed",
                    SamplingConclusion::Failed => "failed",
                },
            }),
            UndoKind::SubmissionExclusion {
                submission_id,
                before,
                after,
            } => serde_json::json!({
                "kind": "exclusion",
                "submission_id": submission_id.to_string(),
                "before": before,
                "after": after,
            }),
        };
        (
            entity_type.to_owned(),
            entity_id,
            entry.label.clone(),
            payload,
        )
    }

    fn decode(label: &str, payload: &serde_json::Value) -> Option<UndoEntry> {
        let kind = payload.get("kind")?.as_str()?;
        let kind = match kind {
            "record" => UndoKind::Record {
                grade_id: GradeId::from_ulid(Ulid::parse(payload.get("grade_id")?.as_str()?).ok()?),
                before: serde_json::from_value(payload.get("before")?.clone()).ok()?,
                after: serde_json::from_value(payload.get("after")?.clone()).ok()?,
            },
            "sampling" => UndoKind::SamplingConclusion {
                grade_id: GradeId::from_ulid(Ulid::parse(payload.get("grade_id")?.as_str()?).ok()?),
                conclusion: match payload.get("conclusion")?.as_str()? {
                    "passed" => SamplingConclusion::Passed,
                    _ => SamplingConclusion::Failed,
                },
            },
            "exclusion" => UndoKind::SubmissionExclusion {
                submission_id: SubmissionId::from_ulid(
                    Ulid::parse(payload.get("submission_id")?.as_str()?).ok()?,
                ),
                before: payload
                    .get("before")
                    .filter(|value| !value.is_null())
                    .and_then(|value| serde_json::from_value(value.clone()).ok()),
                after: serde_json::from_value(payload.get("after")?.clone()).ok()?,
            },
            _ => return None,
        };
        Some(UndoEntry::new(kind, label))
    }
}

impl UndoStore for PlatformUndoStore<'_> {
    fn push_undo(&self, entry: UndoEntry) {
        let Ok(id) = self.platform.ids().next_ulid() else {
            return;
        };
        let Ok(command_id) = self.platform.ids().next_ulid() else {
            return;
        };
        let (entity_type, entity_id, label, payload) = Self::encode(&entry);
        let stored = repo::UndoEntry {
            id,
            command_id: command_id.to_string(),
            action: "review.undo".to_owned(),
            entity_type,
            entity_id,
            payload,
            created_at: self.platform.clock().now(),
        };
        let _ = self.depth;
        let _ = label;
        let _ = self
            .platform
            .db()
            .write(move |tx| repo::push_undo(tx, &stored));
    }

    fn pop_undo(&self) -> Option<UndoEntry> {
        let stored = self
            .platform
            .db()
            .write(|tx| repo::pop_undo(tx))
            .ok()
            .flatten()?;
        Self::decode(&stored.action, &stored.payload)
    }

    fn push_redo(&self, entry: UndoEntry) {
        let Ok(id) = self.platform.ids().next_ulid() else {
            return;
        };
        let Ok(command_id) = self.platform.ids().next_ulid() else {
            return;
        };
        let (entity_type, entity_id, _label, payload) = Self::encode(&entry);
        let stored = repo::RedoEntry {
            id,
            command_id: command_id.to_string(),
            action: "review.redo".to_owned(),
            entity_type,
            entity_id,
            payload: payload.clone(),
            created_at: self.platform.clock().now(),
        };
        let _ = self.depth;
        let _ = self
            .platform
            .db()
            .write(move |tx| repo::push_redo(tx, &stored));
    }

    fn pop_redo(&self) -> Option<UndoEntry> {
        let stored = self
            .platform
            .db()
            .write(|tx| repo::pop_redo(tx))
            .ok()
            .flatten()?;
        Self::decode(&stored.action, &stored.payload)
    }

    fn clear_redo(&self) {
        let _ = self
            .platform
            .db()
            .write(|tx| repo::clear_redo(tx).map(|_| ()));
    }

    fn undo_len(&self) -> usize {
        self.platform
            .db()
            .read(|conn| repo::list_undo(conn, 1000))
            .map_or(0, |entries| entries.len())
    }

    fn redo_len(&self) -> usize {
        self.platform
            .db()
            .read(|conn| repo::list_redo(conn, 1000, None))
            .map_or(0, |entries| entries.len())
    }
}

/// 重判引擎：把重判请求落成 platform 任务（grade 流水线 worker 消费；幂等键进任务输入哈希）。
///
/// 说明：app 进程内不直接跑整卷判分流水线（需 OCR/align/推理装配与模型授权）；
/// 重判以**真实任务**登记，失败保留原判（记录不改，只写审计）；任务执行结果由
/// grade worker 回投新修订。单题/整卷都走同一任务队列，重跑范围写入 payload。
pub(crate) struct PlatformRegradeEngine<'a> {
    platform: &'a Platform,
}

impl<'a> PlatformRegradeEngine<'a> {
    pub(crate) const fn new(platform: &'a Platform) -> Self {
        Self { platform }
    }
}

impl RegradeEngine for PlatformRegradeEngine<'_> {
    fn regrade(&self, request: &RegradeRequest) -> Result<RegradeResult, ReviewError> {
        let scope = match request.scope {
            eg_review::RegradeScope::Single => "single",
            eg_review::RegradeScope::WholeSubmission => "whole_submission",
        };
        let payload = serde_json::json!({
            "kind": "regrade",
            "grade_id": request.record.id.to_string(),
            "submission_id": request.record.submission_id.to_string(),
            "question_id": request.record.question_id.to_string(),
            "expected_revision": request.record.revision,
            "scope": scope,
            "mode": format!("{:?}", request.mode),
            "corrected_text": request.corrected.as_ref().map(|text| text.text.clone()),
            "reason": request.reason.map(eg_review::ReviewReasonCode::as_str),
            "idempotency_key": request.idempotency_key,
        });
        let task = self
            .platform
            .tasks()
            .enqueue(
                eg_platform::NewTask::new(eg_platform::TaskKind::SubmissionGrade, payload)
                    .with_scope(vec![format!("regrade:{}", request.record.submission_id)])
                    .with_input_hashes(vec![request.idempotency_key.clone()]),
            )
            .map_err(|error| ReviewError::RegradeFailed(error.to_string()))?;
        Ok(RegradeResult {
            record: None,
            task_id: Some(task.task_id.to_string()),
            fallback_to_queue: false,
        })
    }
}

/// 供 `worker` 复用：当前页的页面 id（避免未使用告警占位）。
#[allow(dead_code)]
pub(crate) const _PAGE: PageId = PageId::Review;

/// 供 `worker` 复用：排除原因标签（审计展示）。
#[must_use]
pub(crate) fn exclusion_label(reason: ExcludeReason) -> &'static str {
    match reason {
        ExcludeReason::Absent => "缺考",
        ExcludeReason::Duplicate => "重复提交",
        ExcludeReason::OutOfScope => "超出范围",
        ExcludeReason::Invalid => "无效提交",
        ExcludeReason::Other => "其他",
    }
}

/// 抽检结论时间辅助（审计显示）。
#[must_use]
pub(crate) fn now_timestamp(platform: &Platform) -> Timestamp {
    platform.clock().now()
}

/// `Clock` trait 占位使用（若未来需要自定义时钟注入）。
#[allow(dead_code)]
pub(crate) fn clock_of(platform: &Platform) -> &dyn Clock {
    platform.clock().as_ref()
}

/// 审计结果标签（诊断）。
#[must_use]
pub(crate) fn outcome_label(outcome: AuditOutcome) -> &'static str {
    outcome.as_str()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::sync::Mutex;

    use super::*;

    /// 进程环境变量是全局的：本模块内串行化涉及数据根的测试。
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn sampling_entry(seed: u128) -> UndoEntry {
        UndoEntry::new(
            UndoKind::SamplingConclusion {
                grade_id: GradeId::from_ulid(Ulid::from_parts(0, seed)),
                conclusion: SamplingConclusion::Passed,
            },
            "重做持久化测试",
        )
    }

    #[test]
    fn redo_stack_persists_across_platform_reopen() {
        let _guard = ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "eg-app-redo-test-{}-{}",
            std::process::id(),
            Ulid::from_parts(0, u128::from(std::process::id()))
        ));
        let _ = std::fs::remove_dir_all(&root);
        // Safety：测试内串行修改变量，且仅本测试读取。
        unsafe {
            std::env::set_var("EXAM_GRADING_DATA_DIR", &root);
        }
        let dirs = eg_platform::DataDirs::resolve().unwrap();

        {
            let platform = Platform::open(dirs.clone()).unwrap();
            let store = PlatformUndoStore::new(&platform, 50);
            store.push_redo(sampling_entry(11));
            store.push_redo(sampling_entry(12));
            assert_eq!(store.redo_len(), 2, "写入后应有 2 条 redo");
        }
        // 重新打开（模拟重启）：redo 从 platform 表恢复。
        {
            let platform = Platform::open(dirs).unwrap();
            let store = PlatformUndoStore::new(&platform, 50);
            assert_eq!(store.redo_len(), 2, "重启后 redo 应恢复");
            assert!(store.pop_redo().is_some(), "重启后应可弹出 redo");
            assert_eq!(store.redo_len(), 1);
            store.clear_redo();
            assert_eq!(store.redo_len(), 0);
        }
        unsafe {
            std::env::remove_var("EXAM_GRADING_DATA_DIR");
        }
        let _ = std::fs::remove_dir_all(root);
    }
}
