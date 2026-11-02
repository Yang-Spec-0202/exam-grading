//! 队列排序/过滤/分页 + 详情（证据/轨迹/历史/只读命令集）。

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
use eg_domain::{ComparatorTrace, ComparatorVerdict, GradeStatus, UngradableReason};
use eg_review::{
    CommandKind, InMemoryReviewRepository, QueueFilter, QueueKind, ReviewConfig, build_detail,
    build_queue,
};

fn with_judgement(
    mut record: eg_domain::GradeRecord,
    value: eg_domain::Judgement,
) -> eg_domain::GradeRecord {
    record.judgement = value;
    record
}

#[test]
fn standard_queue_sorted_by_risk_then_confidence_then_score() {
    let repo = InMemoryReviewRepository::new();
    let config = ReviewConfig::default();
    // 冲突（最高风险）
    let mut conflict = record(RecordSpec {
        submission: 1,
        question: 1,
        confidence: 0.6,
        awarded: 0,
        ..RecordSpec::default()
    });
    conflict.judgement = eg_domain::Judgement::Ungradable {
        reason: UngradableReason::Conflict,
    };
    conflict.reviewed_at = None;
    repo.insert(conflict);
    // 不可判定（风险次之）
    let ungradable = with_judgement(
        record(RecordSpec {
            submission: 2,
            question: 2,
            confidence: 0.7,
            awarded: 0,
            ..RecordSpec::default()
        }),
        eg_domain::Judgement::Ungradable {
            reason: UngradableReason::OutOfScope,
        },
    );
    repo.insert(ungradable);
    // 低置信（同置信度时高分在前）
    repo.insert(record(RecordSpec {
        submission: 3,
        question: 3,
        confidence: 0.8,
        awarded: 5000,
        ..RecordSpec::default()
    }));
    repo.insert(record(RecordSpec {
        submission: 4,
        question: 4,
        confidence: 0.8,
        awarded: 1000,
        ..RecordSpec::default()
    }));
    // 抽检
    repo.insert(fast_sampled(5, 5));
    // 已确认
    repo.insert(record(RecordSpec {
        submission: 6,
        question: 6,
        status: GradeStatus::ManuallyReviewed,
        ..RecordSpec::default()
    }));
    repo.set_metadata(question_id(1), metadata("1001", 1));
    repo.set_metadata(question_id(2), metadata("1002", 2));
    repo.set_metadata(question_id(3), metadata("1003", 3));
    repo.set_metadata(question_id(4), metadata("1004", 4));
    repo.set_metadata(question_id(5), metadata("1005", 5));
    repo.set_metadata(question_id(6), metadata("1006", 6));
    let page = build_queue(&repo, &QueueFilter::default(), None, &config).unwrap();
    let order: Vec<(QueueKind, eg_domain::QuestionId)> = page
        .items
        .iter()
        .map(|item| (item.kind, item.question_id))
        .collect();
    assert_eq!(order.len(), 6);
    assert_eq!(order[0].0, QueueKind::Standard);
    assert_eq!(order[0].1, question_id(1), "冲突应最先");
    assert_eq!(order[1].1, question_id(2), "不可判定次之");
    assert_eq!(order[2].1, question_id(3), "低置信同分时高分在前");
    assert_eq!(order[3].1, question_id(4));
    assert_eq!(order[4].0, QueueKind::Sampling);
    assert_eq!(order[5].0, QueueKind::Confirmed);
    assert_eq!(page.counts.standard_pending, 4);
    assert_eq!(page.counts.sampling_pending, 1);
    assert_eq!(page.counts.confirmed, 1);
}

#[test]
fn cursor_pagination_has_no_duplicates_or_missing_items() {
    let repo = InMemoryReviewRepository::new();
    let config = ReviewConfig {
        page_size: 2,
        ..ReviewConfig::default()
    };
    for index in 0..7u128 {
        repo.insert(record(RecordSpec {
            submission: index + 1,
            question: index + 1,
            confidence: 0.8,
            ..RecordSpec::default()
        }));
        repo.set_metadata(
            question_id(index + 1),
            metadata(&format!("10{index:02}"), u32::try_from(index).unwrap() + 1),
        );
    }
    let mut cursor: Option<String> = None;
    let mut seen = Vec::new();
    loop {
        let page = build_queue(&repo, &QueueFilter::default(), cursor.as_deref(), &config).unwrap();
        assert!(page.items.len() <= 2);
        for item in &page.items {
            assert!(!seen.contains(&item.grade_id), "翻页重复");
            seen.push(item.grade_id);
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(seen.len(), 7);
}

#[test]
fn filters_are_applied_and_invalid_cursor_rejected() {
    let repo = InMemoryReviewRepository::new();
    let config = ReviewConfig::default();
    let first = record(RecordSpec {
        submission: 11,
        question: 11,
        confidence: 0.6,
        ..RecordSpec::default()
    });
    let second = record(RecordSpec {
        submission: 12,
        question: 12,
        confidence: 0.95,
        status: GradeStatus::NeedsReview,
        ..RecordSpec::default()
    });
    repo.insert(first);
    repo.insert(second);
    repo.set_metadata(question_id(11), metadata("2001", 1));
    repo.set_metadata(question_id(12), metadata("2002", 2));
    let filter = QueueFilter {
        student_no: Some("2001".to_owned()),
        ..QueueFilter::default()
    };
    let page = build_queue(&repo, &filter, None, &config).unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].question_id, question_id(11));
    let filter = QueueFilter {
        question_id: Some(question_id(12)),
        ..QueueFilter::default()
    };
    let page = build_queue(&repo, &filter, None, &config).unwrap();
    assert_eq!(page.items.len(), 1);
    let error = build_queue(&repo, &QueueFilter::default(), Some("bad"), &config).unwrap_err();
    assert!(matches!(error, eg_review::ReviewError::InvalidCursor(_)));
}

#[test]
fn thousand_item_queue_pages_without_duplicates() {
    let repo = InMemoryReviewRepository::new();
    let config = ReviewConfig::default();
    for index in 0..1_000u128 {
        repo.insert(record(RecordSpec {
            submission: index + 1,
            question: index + 1,
            confidence: 0.6 + f64::from(u8::try_from(index % 30).unwrap()) / 100.0,
            ..RecordSpec::default()
        }));
        repo.set_metadata(
            question_id(index + 1),
            metadata(
                &format!("{index:05}"),
                u32::try_from(index % 30).unwrap() + 1,
            ),
        );
    }
    let mut cursor: Option<String> = None;
    let mut seen = 0usize;
    loop {
        let page = build_queue(&repo, &QueueFilter::default(), cursor.as_deref(), &config).unwrap();
        assert!(page.items.len() <= config.page_size);
        assert_eq!(page.total, 1_000);
        seen += page.items.len();
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(seen, 1_000);
}

#[test]
fn detail_contains_history_evidence_traces_and_readonly_commands() {
    let repo = InMemoryReviewRepository::new();
    let config = ReviewConfig::default();
    let mut revision1 = standard_needs_review(21, 21);
    revision1.evidence = vec![
        eg_domain::EvidenceRef::Asset(eg_domain::AssetId::of(b"page")),
        eg_domain::EvidenceRef::ComparatorTrace(ComparatorTraceId::from_ulid(Ulid::from_u128(900))),
        eg_domain::EvidenceRef::ModelCall(eg_domain::ModelCallId::from_ulid(Ulid::from_u128(901))),
    ];
    let revision1_id = revision1.id;
    let mut revision2 = revision1.clone();
    revision2.id = grade_id();
    revision2.revision = 2;
    revision2.status = GradeStatus::ManuallyReviewed;
    revision2.reviewed_at = Some(ts());
    repo.insert(revision1);
    repo.insert(revision2.clone());
    repo.set_metadata(question_id(21), metadata("3001", 1));
    repo.set_answer(question_id(21), answer_view("学生作答", 0.8));
    repo.set_reference(
        question_id(21),
        AnswerView {
            text: "参考答案".to_owned(),
            latex: None,
            confidence: None,
            source: "answer_key".to_owned(),
        },
    );
    repo.insert_trace(ComparatorTrace {
        id: ComparatorTraceId::from_ulid(Ulid::from_u128(900)),
        comparator: eg_domain::ComparatorId::new("num_abs").unwrap(),
        verdict: ComparatorVerdict::Equivalent {
            note: "数值等价".to_owned(),
        },
        detail: serde_json::json!({"expected": "2.5"}),
    });
    repo.insert_model_call(eg_domain::ModelCall {
        id: eg_domain::ModelCallId::from_ulid(Ulid::from_u128(901)),
        role: eg_domain::ModelRole::Judge,
        engine: eg_domain::EngineStamp {
            role: eg_domain::ModelRole::Judge,
            backend: eg_domain::BackendId::local_server(),
            model_id: "judge".to_owned(),
            model_version: "1".to_owned(),
            prompt_hash: None,
            params_hash: Sha256Hex::of(b"params"),
        },
        task_id: eg_domain::TaskId::from_ulid(Ulid::from_u128(902)),
        consent_id: None,
        input_hash: Sha256Hex::of(b"input"),
        output: serde_json::json!({"ok": true}),
        latency_ms: 10,
        cost_micros: 0,
        created_at: ts(),
    });
    let detail = build_detail(&repo, revision1_id, &config).unwrap();
    assert_eq!(detail.history.len(), 2);
    assert_eq!(detail.answer.unwrap().text, "学生作答");
    assert_eq!(detail.reference.unwrap().text, "参考答案");
    assert_eq!(detail.assets.len(), 1);
    assert_eq!(detail.comparator_traces.len(), 1);
    assert_eq!(detail.model_calls.len(), 1);
    assert!(!detail.read_only);
    assert!(detail.allowed_commands.contains(&CommandKind::Accept));
    assert!(detail.allowed_commands.contains(&CommandKind::RegradeText));

    // 快速抽检只读详情
    let fast = fast_sampled(22, 22);
    repo.insert(fast.clone());
    repo.set_metadata(question_id(22), metadata("3002", 1));
    let detail = build_detail(&repo, fast.id, &config).unwrap();
    assert!(detail.read_only);
    assert!(!detail.allowed_commands.contains(&CommandKind::Accept));
    assert!(detail.allowed_commands.contains(&CommandKind::SamplingPass));
    assert!(
        detail
            .allowed_commands
            .contains(&CommandKind::TransferToStandard)
    );
}

use eg_domain::{ComparatorTraceId, Sha256Hex, Ulid};
use eg_review::AnswerView;
