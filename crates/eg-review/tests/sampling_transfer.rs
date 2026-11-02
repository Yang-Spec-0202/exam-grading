//! 抽检只读、抽检通过/不通过与弹窗协议、转标准、重跑幂等（T32）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    unreachable_pub,
    clippy::needless_pass_by_value,
    clippy::unused_self,
    clippy::too_many_lines
)]
mod common;

use common::*;
use eg_domain::{ExcludeReason, GradeMode, GradeStatus};
use eg_grade::ports::{FixedClock, SequentialIds};
use eg_review::ports::{AuditSink, FixtureRegradeStep, ReviewRepository, UndoStore};
use eg_review::{
    CommandEnvelope, CommandOutcome, CorrectedText, FixtureRegradeEngine, InMemoryAuditSink,
    InMemoryReviewRepository, InMemoryUndoStore, RegradeScope, ReviewCommand, ReviewConfig,
    ReviewError, ReviewReasonCode, ReviewService, SamplingConclusion,
};

fn fast_record() -> eg_domain::GradeRecord {
    fast_sampled(51, 51)
}

#[test]
fn sampling_item_rejects_every_edit_command() {
    let repo = InMemoryReviewRepository::new();
    let audit = InMemoryAuditSink::new();
    let undo = InMemoryUndoStore::new(50);
    let engine = FixtureRegradeEngine::new(Vec::new());
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let config = ReviewConfig::default();
    let service = ReviewService::new(&repo, &audit, &undo, &engine, &clock, &ids, &config).unwrap();
    let record = fast_record();
    repo.insert(record.clone());
    let commands = vec![
        (ReviewCommand::Accept { note: None }, "ro-accept"),
        (
            ReviewCommand::OverrideScore {
                score_milli: 3000,
                reason: ReviewReasonCode::ModelMisjudged,
                note: None,
            },
            "ro-score",
        ),
        (
            ReviewCommand::RegradeText {
                corrected: CorrectedText {
                    text: "文本".to_owned(),
                    latex: None,
                },
                note: None,
            },
            "ro-regrade",
        ),
        (
            ReviewCommand::ExcludeQuestion {
                exclude_reason: ExcludeReason::Invalid,
                reason: ReviewReasonCode::Other,
                note: None,
            },
            "ro-exclude",
        ),
        (
            ReviewCommand::ExcludeSubmission {
                reason: ExcludeReason::Invalid,
                note: None,
            },
            "ro-exclude-sub",
        ),
    ];
    for (command, key) in commands {
        let error = service
            .execute(&envelope(record.id, 1, key, command))
            .unwrap_err();
        assert_eq!(error, ReviewError::ReadOnlySampled, "{key}");
    }
    // 批量同样被拒绝（逐项）
    let result = service
        .execute(&CommandEnvelope {
            target: None,
            idempotency_key: "ro-batch".to_owned(),
            expected_revision: None,
            command: ReviewCommand::BatchAccept {
                targets: vec![eg_review::BatchTarget {
                    grade_id: record.id,
                    expected_revision: 1,
                }],
            },
        })
        .unwrap();
    let CommandOutcome::Batch { batch } = result.outcome else {
        panic!("批量结果");
    };
    assert_eq!(batch.succeeded.len(), 0);
    assert_eq!(batch.failed.len(), 1);
    // 批量改分同样拒绝
    let result = service
        .execute(&CommandEnvelope {
            target: None,
            idempotency_key: "ro-batch-score".to_owned(),
            expected_revision: None,
            command: ReviewCommand::BatchOverrideScore {
                items: vec![eg_review::BatchScoreTarget {
                    target: eg_review::BatchTarget {
                        grade_id: record.id,
                        expected_revision: 1,
                    },
                    score_milli: 3000,
                    reason: ReviewReasonCode::ModelMisjudged,
                }],
            },
        })
        .unwrap();
    let CommandOutcome::Batch { batch } = result.outcome else {
        panic!("批量结果");
    };
    assert_eq!(batch.succeeded.len(), 0);
    assert_eq!(repo.append_count(), 0, "只读队列 100% 拒绝编辑");
    assert!(audit.list().len() >= 6);
    // 详情允许命令仅抽检/转标准/撤销重做
    let detail = service.detail(record.id).unwrap();
    assert!(detail.read_only);
    assert!(
        detail
            .allowed_commands
            .contains(&eg_review::CommandKind::SamplingPass)
    );
    assert!(
        !detail
            .allowed_commands
            .contains(&eg_review::CommandKind::OverrideScore)
    );
}

#[test]
fn sampling_pass_writes_conclusion_without_revision() {
    let repo = InMemoryReviewRepository::new();
    let audit = InMemoryAuditSink::new();
    let undo = InMemoryUndoStore::new(50);
    let engine = FixtureRegradeEngine::new(Vec::new());
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let config = ReviewConfig::default();
    let service = ReviewService::new(&repo, &audit, &undo, &engine, &clock, &ids, &config).unwrap();
    let record = fast_record();
    repo.insert(record.clone());
    let result = service
        .execute(&envelope(record.id, 1, "pass", sampling_pass()))
        .unwrap();
    assert!(matches!(
        result.outcome,
        CommandOutcome::SamplingPassed { .. }
    ));
    assert_eq!(repo.append_count(), 0, "抽检通过不改判分状态");
    let audit_record = audit.list().pop().unwrap();
    assert_eq!(
        audit_record.sampling_conclusion,
        Some(SamplingConclusion::Passed)
    );
    assert_eq!(audit_record.outcome, eg_review::AuditOutcome::Noop);
    // 撤销抽检结论（审计型，无数据变更）
    service
        .execute(&CommandEnvelope {
            target: None,
            idempotency_key: "undo-pass".to_owned(),
            expected_revision: None,
            command: ReviewCommand::Undo,
        })
        .unwrap();
    assert_eq!(repo.append_count(), 0);
    assert_eq!(undo.undo_len(), 0);
}

#[test]
fn sampling_fail_unconfirmed_returns_prompt_without_rerun() {
    let repo = InMemoryReviewRepository::new();
    let audit = InMemoryAuditSink::new();
    let undo = InMemoryUndoStore::new(50);
    let engine = FixtureRegradeEngine::new(Vec::new());
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let config = ReviewConfig::default();
    let service = ReviewService::new(&repo, &audit, &undo, &engine, &clock, &ids, &config).unwrap();
    let record = fast_record();
    repo.insert(record.clone());
    let result = service
        .execute(&envelope(
            record.id,
            1,
            "fail-prompt",
            sampling_fail(RegradeScope::Single, false),
        ))
        .unwrap();
    let CommandOutcome::SamplingPrompt { prompt } = result.outcome else {
        panic!("未确认应返回弹窗事件");
    };
    assert_eq!(prompt.default_scope, RegradeScope::Single);
    assert_eq!(prompt.options.len(), 2);
    assert_eq!(engine.executions(), 0, "未确认不自动重跑");
    assert_eq!(repo.append_count(), 0);
    let audit_record = audit.list().pop().unwrap();
    assert_eq!(audit_record.confirmed, Some(false));
    assert_eq!(audit_record.detail.as_deref(), Some("待确认：未重跑"));
}

#[test]
fn sampling_fail_confirmed_default_single_reruns_once() {
    let repo = InMemoryReviewRepository::new();
    let audit = InMemoryAuditSink::new();
    let undo = InMemoryUndoStore::new(50);
    let record = fast_record();
    repo.insert(record.clone());
    let engine = FixtureRegradeEngine::new(vec![FixtureRegradeStep::Regraded(regraded(
        &record,
        3000,
        GradeMode::Fast,
    ))]);
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let config = ReviewConfig::default();
    let service = ReviewService::new(&repo, &audit, &undo, &engine, &clock, &ids, &config).unwrap();
    let result = service
        .execute(&envelope(
            record.id,
            1,
            "fail-single",
            sampling_fail(RegradeScope::Single, true),
        ))
        .unwrap();
    assert!(matches!(
        result.outcome,
        CommandOutcome::Record {
            revision: 2,
            mode: GradeMode::Fast,
            ..
        }
    ));
    assert_eq!(engine.executions(), 1);
    assert_eq!(repo.append_count(), 1);
    let audit_record = audit.list().pop().unwrap();
    assert_eq!(audit_record.confirmed, Some(true));
    assert_eq!(audit_record.scope, Some(RegradeScope::Single));
    assert_eq!(
        audit_record.sampling_conclusion,
        Some(SamplingConclusion::Failed)
    );
}

#[test]
fn sampling_fail_whole_submission_schedules_task_and_keeps_records() {
    let repo = InMemoryReviewRepository::new();
    let audit = InMemoryAuditSink::new();
    let undo = InMemoryUndoStore::new(50);
    let record = fast_record();
    repo.insert(record.clone());
    let engine = FixtureRegradeEngine::new(vec![FixtureRegradeStep::SubmissionTask(
        "task-42".to_owned(),
    )]);
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let config = ReviewConfig::default();
    let service = ReviewService::new(&repo, &audit, &undo, &engine, &clock, &ids, &config).unwrap();
    let result = service
        .execute(&envelope(
            record.id,
            1,
            "fail-whole",
            sampling_fail(RegradeScope::WholeSubmission, true),
        ))
        .unwrap();
    let CommandOutcome::RegradeScheduled { task_id, scope } = result.outcome else {
        panic!("整卷应返回任务");
    };
    assert_eq!(task_id, "task-42");
    assert_eq!(scope, RegradeScope::WholeSubmission);
    assert_eq!(repo.append_count(), 0);
    assert_eq!(
        audit.list().pop().unwrap().scope,
        Some(RegradeScope::WholeSubmission)
    );
}

#[test]
fn transfer_single_preserves_fast_history_and_creates_standard_revision() {
    let repo = InMemoryReviewRepository::new();
    let audit = InMemoryAuditSink::new();
    let undo = InMemoryUndoStore::new(50);
    let record = fast_record();
    repo.insert(record.clone());
    let engine = FixtureRegradeEngine::new(vec![FixtureRegradeStep::Regraded(regraded(
        &record,
        5000,
        GradeMode::Standard,
    ))]);
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let config = ReviewConfig::default();
    let service = ReviewService::new(&repo, &audit, &undo, &engine, &clock, &ids, &config).unwrap();
    let result = service
        .execute(&envelope(
            record.id,
            1,
            "transfer-1",
            transfer(RegradeScope::Single),
        ))
        .unwrap();
    assert!(matches!(
        result.outcome,
        CommandOutcome::Record {
            revision: 2,
            mode: GradeMode::Standard,
            ..
        }
    ));
    let history = repo.revisions(record.submission_id, record.question_id);
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].mode, GradeMode::Fast, "原快速记录保留");
    assert_eq!(history[1].mode, GradeMode::Standard);
    let audit_record = audit.list().pop().unwrap();
    assert_eq!(audit_record.before.as_ref().unwrap().mode, GradeMode::Fast);
    assert_eq!(
        audit_record.after.as_ref().unwrap().mode,
        GradeMode::Standard
    );
    let detail = service.detail(history[1].id).unwrap();
    assert!(detail.transferred_to_standard);
}

#[test]
fn transfer_whole_submission_schedules_standard_task() {
    let repo = InMemoryReviewRepository::new();
    let audit = InMemoryAuditSink::new();
    let undo = InMemoryUndoStore::new(50);
    let record = fast_record();
    repo.insert(record.clone());
    let engine = FixtureRegradeEngine::new(vec![FixtureRegradeStep::SubmissionTask(
        "task-standard".to_owned(),
    )]);
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let config = ReviewConfig::default();
    let service = ReviewService::new(&repo, &audit, &undo, &engine, &clock, &ids, &config).unwrap();
    let result = service
        .execute(&envelope(
            record.id,
            1,
            "transfer-all",
            transfer(RegradeScope::WholeSubmission),
        ))
        .unwrap();
    let CommandOutcome::RegradeScheduled { task_id, .. } = result.outcome else {
        panic!("整份提交转标准应调度任务");
    };
    assert_eq!(task_id, "task-standard");
}

#[test]
fn transfer_fallback_to_standard_queue_marks_needs_review() {
    let repo = InMemoryReviewRepository::new();
    let audit = InMemoryAuditSink::new();
    let undo = InMemoryUndoStore::new(50);
    let record = fast_record();
    repo.insert(record.clone());
    let mut fallback = regraded(&record, 5000, GradeMode::Standard);
    fallback.status = GradeStatus::NeedsReview;
    let engine = FixtureRegradeEngine::new(vec![FixtureRegradeStep::FallbackToQueue(fallback)]);
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let config = ReviewConfig::default();
    let service = ReviewService::new(&repo, &audit, &undo, &engine, &clock, &ids, &config).unwrap();
    let result = service
        .execute(&envelope(
            record.id,
            1,
            "transfer-fallback",
            transfer(RegradeScope::Single),
        ))
        .unwrap();
    assert!(matches!(
        result.outcome,
        CommandOutcome::Record {
            status: GradeStatus::NeedsReview,
            mode: GradeMode::Standard,
            ..
        }
    ));
}

#[test]
fn transfer_rejects_standard_records() {
    let repo = InMemoryReviewRepository::new();
    let audit = InMemoryAuditSink::new();
    let undo = InMemoryUndoStore::new(50);
    let engine = FixtureRegradeEngine::new(Vec::new());
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let config = ReviewConfig::default();
    let service = ReviewService::new(&repo, &audit, &undo, &engine, &clock, &ids, &config).unwrap();
    let record = standard_needs_review(52, 52);
    repo.insert(record.clone());
    let error = service
        .execute(&envelope(
            record.id,
            1,
            "transfer-standard",
            transfer(RegradeScope::Single),
        ))
        .unwrap_err();
    assert!(matches!(error, ReviewError::WrongMode { .. }));
}

#[test]
fn fast_non_sampled_record_is_readonly_for_edits_but_transferable() {
    let repo = InMemoryReviewRepository::new();
    let audit = InMemoryAuditSink::new();
    let undo = InMemoryUndoStore::new(50);
    let record = record(RecordSpec {
        submission: 53,
        question: 53,
        mode: GradeMode::Fast,
        status: GradeStatus::AutoAccepted,
        sampled: false,
        ..RecordSpec::default()
    });
    repo.insert(record.clone());
    let engine = FixtureRegradeEngine::new(vec![FixtureRegradeStep::Regraded(regraded(
        &record,
        5000,
        GradeMode::Standard,
    ))]);
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let config = ReviewConfig::default();
    let service = ReviewService::new(&repo, &audit, &undo, &engine, &clock, &ids, &config).unwrap();
    let error = service
        .execute(&envelope(record.id, 1, "fast-edit", accept("fast-edit")))
        .unwrap_err();
    assert_eq!(error, ReviewError::FastModeReadOnly);
    let result = service
        .execute(&envelope(
            record.id,
            1,
            "fast-transfer",
            transfer(RegradeScope::Single),
        ))
        .unwrap();
    assert!(matches!(
        result.outcome,
        CommandOutcome::Record {
            mode: GradeMode::Standard,
            ..
        }
    ));
}

#[test]
fn regrade_is_idempotent_by_command_key() {
    let repo = InMemoryReviewRepository::new();
    let audit = InMemoryAuditSink::new();
    let undo = InMemoryUndoStore::new(50);
    let record = fast_record();
    repo.insert(record.clone());
    let engine = FixtureRegradeEngine::new(vec![FixtureRegradeStep::Regraded(regraded(
        &record,
        3000,
        GradeMode::Fast,
    ))]);
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let config = ReviewConfig::default();
    let service = ReviewService::new(&repo, &audit, &undo, &engine, &clock, &ids, &config).unwrap();
    let envelope = envelope(
        record.id,
        1,
        "regrade-same",
        sampling_fail(RegradeScope::Single, true),
    );
    let first = service.execute(&envelope).unwrap();
    let second = service.execute(&envelope).unwrap();
    assert_eq!(first, second);
    assert_eq!(engine.executions(), 1, "幂等键命中不重复重跑");
    assert_eq!(repo.append_count(), 1);
}
