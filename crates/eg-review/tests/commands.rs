//! 命令：接受/改分/修文本重判/排除/批量/撤销重做/乐观锁/审计（T31）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    unreachable_pub,
    clippy::needless_pass_by_value,
    clippy::unused_self
)]
mod common;

use common::*;
use eg_domain::{ExcludeReason, GradeMode, GradeStatus};
use eg_grade::ports::{FixedClock, SequentialIds};
use eg_review::ports::{AuditSink, FixtureRegradeStep, ReviewRepository, UndoStore};
use eg_review::{
    CommandEnvelope, CommandOutcome, CorrectedText, FixtureRegradeEngine, InMemoryAuditSink,
    InMemoryReviewRepository, InMemoryUndoStore, RegradeScope, ReviewCommand, ReviewConfig,
    ReviewError, ReviewReasonCode, ReviewService,
};

#[test]
fn accept_appends_revision_writes_audit_and_supports_undo_redo() {
    let repo = InMemoryReviewRepository::new();
    let audit = InMemoryAuditSink::new();
    let undo = InMemoryUndoStore::new(50);
    let engine = FixtureRegradeEngine::new(Vec::new());
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let config = ReviewConfig::default();
    let service = ReviewService::new(&repo, &audit, &undo, &engine, &clock, &ids, &config).unwrap();
    let original = record(RecordSpec {
        submission: 31,
        question: 31,
        ..RecordSpec::default()
    });
    repo.insert(original.clone());
    let result = service
        .execute(&envelope(original.id, 1, "cmd-1", accept("cmd-1")))
        .unwrap();
    let CommandOutcome::Record {
        revision,
        status,
        mode,
        ..
    } = result.outcome
    else {
        panic!("应追加新修订");
    };
    assert_eq!(revision, 2);
    assert_eq!(status, GradeStatus::ManuallyReviewed);
    assert_eq!(mode, GradeMode::Standard);
    assert_eq!(
        repo.revisions(original.submission_id, original.question_id)
            .len(),
        2
    );
    let audits = audit.list();
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].actor, "local-teacher");
    assert_eq!(audits[0].action, "review.accept");
    assert!(audits[0].before.is_some());
    assert!(audits[0].after.is_some());
    // 撤销：追加反向修订，状态恢复
    let undo_result = service
        .execute(&CommandEnvelope {
            target: None,
            idempotency_key: "undo-1".to_owned(),
            expected_revision: None,
            command: ReviewCommand::Undo,
        })
        .unwrap();
    assert!(matches!(
        undo_result.outcome,
        CommandOutcome::Undone { undone: true, .. }
    ));
    let latest = repo
        .revisions(original.submission_id, original.question_id)
        .pop()
        .unwrap();
    assert_eq!(latest.revision, 3);
    assert_eq!(latest.status, GradeStatus::NeedsReview);
    // 重做：再回到已人工复核
    service
        .execute(&CommandEnvelope {
            target: None,
            idempotency_key: "redo-1".to_owned(),
            expected_revision: None,
            command: ReviewCommand::Redo,
        })
        .unwrap();
    let latest = repo
        .revisions(original.submission_id, original.question_id)
        .pop()
        .unwrap();
    assert_eq!(latest.revision, 4);
    assert_eq!(latest.status, GradeStatus::ManuallyReviewed);
}

#[test]
fn override_score_validates_bounds_granularity_and_is_audited() {
    let repo = InMemoryReviewRepository::new();
    let audit = InMemoryAuditSink::new();
    let undo = InMemoryUndoStore::new(50);
    let engine = FixtureRegradeEngine::new(Vec::new());
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let config = ReviewConfig::default();
    let service = ReviewService::new(&repo, &audit, &undo, &engine, &clock, &ids, &config).unwrap();
    let original = standard_needs_review(32, 32);
    repo.insert(original.clone());
    // 越界
    let error = service
        .execute(&envelope(
            original.id,
            1,
            "oob",
            override_score(6000, "oob"),
        ))
        .unwrap_err();
    assert!(matches!(error, ReviewError::ScoreOutOfRange { .. }));
    // 粒度非法（默认 0.1 分 = 100 千分之一；250 非整数倍）
    let error = service
        .execute(&envelope(
            original.id,
            1,
            "gran",
            override_score(250, "gran"),
        ))
        .unwrap_err();
    assert!(matches!(error, ReviewError::GranularityViolation { .. }));
    assert_eq!(repo.append_count(), 0);
    assert_eq!(audit.list().len(), 2, "被拒命令也要审计");
    assert!(
        audit
            .list()
            .iter()
            .all(|record| record.outcome == eg_review::AuditOutcome::Rejected)
    );
    // 合法改分
    let result = service
        .execute(&envelope(original.id, 1, "ok", override_score(3000, "ok")))
        .unwrap();
    assert!(matches!(
        result.outcome,
        CommandOutcome::Record {
            revision: 2,
            status: GradeStatus::ManuallyReviewed,
            ..
        }
    ));
    let latest = repo
        .revisions(original.submission_id, original.question_id)
        .pop()
        .unwrap();
    assert_eq!(latest.awarded.as_milli(), 3000);
    assert_eq!(
        latest
            .judgement
            .score()
            .map(eg_domain::MilliScore::as_milli),
        Some(3000)
    );
    let audit = audit.list().pop().unwrap();
    assert_eq!(audit.reason, Some(ReviewReasonCode::PartialCredit));
}

#[test]
fn stale_revision_is_rejected_and_input_preserved() {
    let repo = InMemoryReviewRepository::new();
    let audit = InMemoryAuditSink::new();
    let undo = InMemoryUndoStore::new(50);
    let engine = FixtureRegradeEngine::new(Vec::new());
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let config = ReviewConfig::default();
    let service = ReviewService::new(&repo, &audit, &undo, &engine, &clock, &ids, &config).unwrap();
    let original = standard_needs_review(33, 33);
    repo.insert(original.clone());
    let error = service
        .execute(&envelope(original.id, 99, "stale", accept("stale")))
        .unwrap_err();
    assert_eq!(
        error,
        ReviewError::StaleRevision {
            expected: 99,
            actual: 1
        }
    );
    assert_eq!(repo.append_count(), 0);
    assert_eq!(audit.list().len(), 1);
    assert_eq!(audit.list()[0].outcome, eg_review::AuditOutcome::Rejected);
}

#[test]
fn batch_accept_returns_per_item_results_without_rollback() {
    let repo = InMemoryReviewRepository::new();
    let audit = InMemoryAuditSink::new();
    let undo = InMemoryUndoStore::new(50);
    let engine = FixtureRegradeEngine::new(Vec::new());
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let config = ReviewConfig::default();
    let service = ReviewService::new(&repo, &audit, &undo, &engine, &clock, &ids, &config).unwrap();
    let first = standard_needs_review(34, 34);
    let second = standard_needs_review(34, 35);
    repo.insert(first.clone());
    repo.insert(second.clone());
    let command = ReviewCommand::BatchAccept {
        targets: vec![
            eg_review::BatchTarget {
                grade_id: first.id,
                expected_revision: 1,
            },
            eg_review::BatchTarget {
                grade_id: second.id,
                expected_revision: 9,
            },
        ],
    };
    let result = service
        .execute(&CommandEnvelope {
            target: None,
            idempotency_key: "batch-1".to_owned(),
            expected_revision: None,
            command,
        })
        .unwrap();
    let CommandOutcome::Batch { batch } = result.outcome else {
        panic!("应为批量结果");
    };
    assert_eq!(batch.succeeded.len(), 1);
    assert_eq!(batch.failed.len(), 1);
    assert!(matches!(
        batch.failed[0].error,
        ReviewError::StaleRevision { .. }
    ));
    assert_eq!(repo.append_count(), 1, "已成功项不回滚");
    assert!(audit.list().len() >= 3, "逐项 + 汇总审计");
}

#[test]
fn undo_fifty_steps_and_redo_restore_values() {
    let repo = InMemoryReviewRepository::new();
    let audit = InMemoryAuditSink::new();
    let undo = InMemoryUndoStore::new(50);
    let engine = FixtureRegradeEngine::new(Vec::new());
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let config = ReviewConfig::default();
    let service = ReviewService::new(&repo, &audit, &undo, &engine, &clock, &ids, &config).unwrap();
    let original = record(RecordSpec {
        submission: 35,
        question: 35,
        awarded: 0,
        ..RecordSpec::default()
    });
    repo.insert(original.clone());
    for step in 1..=55i64 {
        let score = step.min(50) * 100;
        let latest = repo
            .revisions(original.submission_id, original.question_id)
            .pop()
            .unwrap();
        service
            .execute(&envelope(
                latest.id,
                latest.revision,
                &format!("step-{step}"),
                override_score(score, "step"),
            ))
            .unwrap();
    }
    assert_eq!(undo.undo_len(), 50);
    for step in 0..50 {
        service
            .execute(&CommandEnvelope {
                target: None,
                idempotency_key: format!("undo-{step}"),
                expected_revision: None,
                command: ReviewCommand::Undo,
            })
            .unwrap();
    }
    assert_eq!(undo.undo_len(), 0);
    let latest = repo
        .revisions(original.submission_id, original.question_id)
        .pop()
        .unwrap();
    assert_eq!(
        latest.awarded.as_milli(),
        500,
        "撤销最近 50 步后回到第 5 步状态"
    );
    for step in 0..10 {
        service
            .execute(&CommandEnvelope {
                target: None,
                idempotency_key: format!("redo-{step}"),
                expected_revision: None,
                command: ReviewCommand::Redo,
            })
            .unwrap();
    }
    let latest = repo
        .revisions(original.submission_id, original.question_id)
        .pop()
        .unwrap();
    assert_eq!(
        latest.awarded.as_milli(),
        1500,
        "重做 10 步回到第 15 步状态"
    );
}

#[test]
fn submission_exclusion_is_audited_and_undoable() {
    let repo = InMemoryReviewRepository::new();
    let audit = InMemoryAuditSink::new();
    let undo = InMemoryUndoStore::new(50);
    let engine = FixtureRegradeEngine::new(Vec::new());
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let config = ReviewConfig::default();
    let service = ReviewService::new(&repo, &audit, &undo, &engine, &clock, &ids, &config).unwrap();
    let original = standard_needs_review(36, 36);
    repo.insert(original.clone());
    let command = ReviewCommand::ExcludeSubmission {
        reason: ExcludeReason::Absent,
        note: Some("缺考".to_owned()),
    };
    let result = service
        .execute(&envelope(original.id, 1, "excl", command))
        .unwrap();
    assert!(matches!(
        result.outcome,
        CommandOutcome::SubmissionExcluded { .. }
    ));
    assert_eq!(
        repo.exclusion(original.submission_id)
            .map(|exclusion| exclusion.reason),
        Some(ExcludeReason::Absent)
    );
    service
        .execute(&CommandEnvelope {
            target: None,
            idempotency_key: "undo-excl".to_owned(),
            expected_revision: None,
            command: ReviewCommand::Undo,
        })
        .unwrap();
    assert!(repo.exclusion(original.submission_id).is_none());
    service
        .execute(&CommandEnvelope {
            target: None,
            idempotency_key: "redo-excl".to_owned(),
            expected_revision: None,
            command: ReviewCommand::Redo,
        })
        .unwrap();
    assert_eq!(
        repo.exclusion(original.submission_id)
            .map(|exclusion| exclusion.reason),
        Some(ExcludeReason::Absent)
    );
}

#[test]
fn idempotency_key_prevents_duplicate_execution() {
    let repo = InMemoryReviewRepository::new();
    let audit = InMemoryAuditSink::new();
    let undo = InMemoryUndoStore::new(50);
    let engine = FixtureRegradeEngine::new(Vec::new());
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let config = ReviewConfig::default();
    let service = ReviewService::new(&repo, &audit, &undo, &engine, &clock, &ids, &config).unwrap();
    let original = standard_needs_review(37, 37);
    repo.insert(original.clone());
    let first = service
        .execute(&envelope(original.id, 1, "same", accept("same")))
        .unwrap();
    let second = service
        .execute(&envelope(original.id, 1, "same", accept("same")))
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(repo.append_count(), 1);
    assert_eq!(audit.list().len(), 1);
}

#[test]
fn regrade_text_failure_keeps_original_and_releases_lock() {
    let repo = InMemoryReviewRepository::new();
    let audit = InMemoryAuditSink::new();
    let undo = InMemoryUndoStore::new(50);
    let original = standard_needs_review(38, 38);
    repo.insert(original.clone());
    let engine = FixtureRegradeEngine::new(vec![
        FixtureRegradeStep::Failed(ReviewError::RegradeFailed("后端不可用".to_owned())),
        FixtureRegradeStep::Regraded(regraded(&original, 4000, GradeMode::Standard)),
    ]);
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let config = ReviewConfig::default();
    let service = ReviewService::new(&repo, &audit, &undo, &engine, &clock, &ids, &config).unwrap();
    let command = ReviewCommand::RegradeText {
        corrected: CorrectedText {
            text: "修正后文本".to_owned(),
            latex: None,
        },
        note: None,
    };
    let error = service
        .execute(&envelope(original.id, 1, "regrade-1", command.clone()))
        .unwrap_err();
    assert!(matches!(error, ReviewError::RegradeFailed(_)));
    assert_eq!(repo.append_count(), 0);
    // 锁已释放：同题再次重判成功
    let result = service
        .execute(&envelope(original.id, 1, "regrade-2", command))
        .unwrap();
    assert!(matches!(
        result.outcome,
        CommandOutcome::Record { revision: 2, .. }
    ));
    assert_eq!(engine.executions(), 2);
}

#[test]
fn batch_limit_is_enforced() {
    let repo = InMemoryReviewRepository::new();
    let audit = InMemoryAuditSink::new();
    let undo = InMemoryUndoStore::new(50);
    let engine = FixtureRegradeEngine::new(Vec::new());
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let config = ReviewConfig {
        max_batch: 1,
        ..ReviewConfig::default()
    };
    let service = ReviewService::new(&repo, &audit, &undo, &engine, &clock, &ids, &config).unwrap();
    let first = standard_needs_review(39, 39);
    let second = standard_needs_review(39, 40);
    repo.insert(first.clone());
    repo.insert(second.clone());
    let error = service
        .execute(&CommandEnvelope {
            target: None,
            idempotency_key: "batch-big".to_owned(),
            expected_revision: None,
            command: ReviewCommand::BatchAccept {
                targets: vec![
                    eg_review::BatchTarget {
                        grade_id: first.id,
                        expected_revision: 1,
                    },
                    eg_review::BatchTarget {
                        grade_id: second.id,
                        expected_revision: 1,
                    },
                ],
            },
        })
        .unwrap_err();
    assert!(matches!(error, ReviewError::BatchTooLarge { .. }));
    assert_eq!(audit.list().len(), 1);
}

#[test]
fn batch_override_score_applies_per_item() {
    let repo = InMemoryReviewRepository::new();
    let audit = InMemoryAuditSink::new();
    let undo = InMemoryUndoStore::new(50);
    let engine = FixtureRegradeEngine::new(Vec::new());
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let config = ReviewConfig::default();
    let service = ReviewService::new(&repo, &audit, &undo, &engine, &clock, &ids, &config).unwrap();
    let first = standard_needs_review(41, 41);
    let second = standard_needs_review(41, 42);
    repo.insert(first.clone());
    repo.insert(second.clone());
    let result = service
        .execute(&CommandEnvelope {
            target: None,
            idempotency_key: "batch-score".to_owned(),
            expected_revision: None,
            command: ReviewCommand::BatchOverrideScore {
                items: vec![
                    eg_review::BatchScoreTarget {
                        target: eg_review::BatchTarget {
                            grade_id: first.id,
                            expected_revision: 1,
                        },
                        score_milli: 3000,
                        reason: ReviewReasonCode::PartialCredit,
                    },
                    eg_review::BatchScoreTarget {
                        target: eg_review::BatchTarget {
                            grade_id: second.id,
                            expected_revision: 9,
                        },
                        score_milli: 4000,
                        reason: ReviewReasonCode::ModelMisjudged,
                    },
                ],
            },
        })
        .unwrap();
    let CommandOutcome::Batch { batch } = result.outcome else {
        panic!("批量结果");
    };
    assert_eq!(batch.succeeded.len(), 1);
    assert_eq!(batch.failed.len(), 1);
    let latest = repo
        .revisions(first.submission_id, first.question_id)
        .pop()
        .unwrap();
    assert_eq!(latest.awarded.as_milli(), 3000);
    assert!(
        audit
            .list()
            .iter()
            .any(|record| record.action == "review.batch_override_score")
    );
}

#[test]
fn undo_stack_survives_service_restart() {
    let repo = InMemoryReviewRepository::new();
    let audit = InMemoryAuditSink::new();
    let undo = InMemoryUndoStore::new(50);
    let engine = FixtureRegradeEngine::new(Vec::new());
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let config = ReviewConfig::default();
    let original = standard_needs_review(42, 42);
    repo.insert(original.clone());
    {
        let service =
            ReviewService::new(&repo, &audit, &undo, &engine, &clock, &ids, &config).unwrap();
        service
            .execute(&envelope(
                original.id,
                1,
                "restart-accept",
                accept("restart"),
            ))
            .unwrap();
    }
    // 模拟进程重启：新 service 实例共享持久化撤销栈
    let service = ReviewService::new(&repo, &audit, &undo, &engine, &clock, &ids, &config).unwrap();
    let result = service
        .execute(&CommandEnvelope {
            target: None,
            idempotency_key: "restart-undo".to_owned(),
            expected_revision: None,
            command: ReviewCommand::Undo,
        })
        .unwrap();
    assert!(matches!(
        result.outcome,
        CommandOutcome::Undone { undone: true, .. }
    ));
    let latest = repo
        .revisions(original.submission_id, original.question_id)
        .pop()
        .unwrap();
    assert_eq!(latest.status, GradeStatus::NeedsReview);
}

#[test]
fn queue_kind_rejects_wrong_mode_commands() {
    let repo = InMemoryReviewRepository::new();
    let audit = InMemoryAuditSink::new();
    let undo = InMemoryUndoStore::new(50);
    let engine = FixtureRegradeEngine::new(Vec::new());
    let clock = FixedClock::new(ts());
    let ids = SequentialIds::new();
    let config = ReviewConfig::default();
    let service = ReviewService::new(&repo, &audit, &undo, &engine, &clock, &ids, &config).unwrap();
    let standard = record(RecordSpec {
        submission: 40,
        question: 40,
        status: GradeStatus::ManuallyReviewed,
        ..RecordSpec::default()
    });
    repo.insert(standard.clone());
    let error = service
        .execute(&envelope(standard.id, 1, "pass-standard", sampling_pass()))
        .unwrap_err();
    assert!(matches!(error, ReviewError::WrongMode { .. }));
    let _ = RegradeScope::Single;
}
