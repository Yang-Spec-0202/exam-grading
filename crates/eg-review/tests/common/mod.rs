//! eg-review 测试夹具（领域记录/队列/命令）。
#![allow(dead_code, unused_imports)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    unreachable_pub,
    clippy::needless_pass_by_value,
    clippy::unused_self
)]
use std::sync::atomic::{AtomicU64, Ordering};

use eg_domain::{
    AssetId, ComparatorTrace, ComparatorTraceId, Confidence, EvidenceRef, GradeId, GradeMode,
    GradeRecord, GradeStatus, Judgement, MilliScore, ModelCall, ModelCallId, ReviewReasonKind,
    RouteDecision, RouteTrace, Sha256Hex, SubmissionId, Timestamp, Ulid,
};
use eg_grade::ports::{FixedClock, SequentialIds};
use eg_review::{
    AnswerView, AuditSink, CommandEnvelope, FixtureRegradeEngine, InMemoryAuditSink,
    InMemoryReviewRepository, InMemoryUndoStore, QueueFilter, ReviewCommand, ReviewConfig,
    ReviewMetadata, ReviewReasonCode,
};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub fn ts() -> Timestamp {
    eg_domain::time::parse_rfc3339("2026-11-02T00:00:00Z").unwrap()
}

pub fn up() -> Ulid {
    // 与 `SequentialIds`（timestamp=0）错开命名空间，避免夹具 id 与生成器 id 碰撞。
    Ulid::from_parts(1, u128::from(NEXT_ID.fetch_add(1, Ordering::Relaxed)))
}

pub fn submission_id(n: u128) -> SubmissionId {
    SubmissionId::from_ulid(Ulid::from_u128(n))
}

pub fn question_id(n: u128) -> eg_domain::QuestionId {
    eg_domain::QuestionId::from_ulid(Ulid::from_u128(n))
}

pub fn grade_id() -> GradeId {
    GradeId::from_ulid(up())
}

pub fn confidence(value: f64) -> Confidence {
    Confidence::new(value).unwrap()
}

pub fn milli(value: i64) -> MilliScore {
    MilliScore::from_milli(value).unwrap()
}

pub fn judgement(awarded: MilliScore, max: MilliScore) -> Judgement {
    if awarded == max {
        Judgement::Match {
            score: max,
            reason: "一致".to_owned(),
            evidence: Vec::new(),
        }
    } else if awarded.is_zero() {
        Judgement::Mismatch {
            score: MilliScore::ZERO,
            reason: "不一致".to_owned(),
            evidence: Vec::new(),
        }
    } else {
        Judgement::Partial {
            score: awarded,
            satisfied: Vec::new(),
            missing: Vec::new(),
            reason: "部分".to_owned(),
        }
    }
}

pub struct RecordSpec {
    pub submission: u128,
    pub question: u128,
    pub revision: u32,
    pub mode: GradeMode,
    pub status: GradeStatus,
    pub awarded: i64,
    pub max: i64,
    pub confidence: f64,
    pub sampled: bool,
    pub reasons: Option<Vec<ReviewReasonKind>>,
}

impl Default for RecordSpec {
    fn default() -> Self {
        Self {
            submission: 1,
            question: 1,
            revision: 1,
            mode: GradeMode::Standard,
            status: GradeStatus::NeedsReview,
            awarded: 2000,
            max: 5000,
            confidence: 0.7,
            sampled: false,
            reasons: None,
        }
    }
}

pub fn record(spec: RecordSpec) -> GradeRecord {
    let max = milli(spec.max);
    let awarded = milli(spec.awarded);
    let route = match spec.mode {
        GradeMode::Standard => RouteDecision::RuleThenModel,
        GradeMode::Fast => RouteDecision::RuleOnly,
    };
    let record = GradeRecord {
        id: grade_id(),
        submission_id: submission_id(spec.submission),
        question_id: question_id(spec.question),
        revision: spec.revision,
        mode: spec.mode,
        route,
        route_trace: RouteTrace {
            rule_id: "R7".to_owned(),
            inputs_hash: Sha256Hex::of(b"inputs"),
            classifier_call: None,
            decided_at: ts(),
        },
        judgement: judgement(awarded, max),
        awarded,
        max_score: max,
        confidence: confidence(spec.confidence),
        status: spec.status,
        evidence: vec![EvidenceRef::Asset(AssetId::of(b"page"))],
        engines: Vec::new(),
        created_at: ts(),
        reviewed_at: (spec.status == GradeStatus::ManuallyReviewed).then(ts),
        review_note: None,
        sampled: spec.sampled,
    };
    let _ = spec.reasons;
    record
}

pub fn standard_needs_review(submission: u128, question: u128) -> GradeRecord {
    record(RecordSpec {
        submission,
        question,
        ..RecordSpec::default()
    })
}

pub fn fast_sampled(submission: u128, question: u128) -> GradeRecord {
    record(RecordSpec {
        submission,
        question,
        mode: GradeMode::Fast,
        status: GradeStatus::AutoAccepted,
        awarded: 5000,
        confidence: 0.95,
        sampled: true,
        ..RecordSpec::default()
    })
}

pub fn metadata(student_no: &str, ordinal: u32) -> ReviewMetadata {
    ReviewMetadata {
        student_no: student_no.to_owned(),
        student_display_name: format!("学生{}", &student_no[student_no.len().saturating_sub(2)..]),
        question_label: format!("{ordinal}"),
        question_ordinal: ordinal,
    }
}

pub fn envelope(
    target: GradeId,
    expected: u32,
    key: &str,
    command: ReviewCommand,
) -> CommandEnvelope {
    CommandEnvelope {
        target: Some(target),
        idempotency_key: key.to_owned(),
        expected_revision: Some(expected),
        command,
    }
}

pub fn accept(_key: &str) -> ReviewCommand {
    ReviewCommand::Accept {
        note: Some("确认".to_owned()),
    }
}

pub fn override_score(score: i64, _key: &str) -> ReviewCommand {
    ReviewCommand::OverrideScore {
        score_milli: score,
        reason: ReviewReasonCode::PartialCredit,
        note: Some("改分".to_owned()),
    }
}

pub fn sampling_pass() -> ReviewCommand {
    ReviewCommand::SamplingPass {
        note: Some("抽检通过".to_owned()),
    }
}

pub fn sampling_fail(scope: eg_review::RegradeScope, confirmed: bool) -> ReviewCommand {
    ReviewCommand::SamplingFail {
        scope,
        confirmed,
        note: Some("抽检不通过".to_owned()),
    }
}

pub fn transfer(scope: eg_review::RegradeScope) -> ReviewCommand {
    ReviewCommand::TransferToStandard { scope, note: None }
}

pub fn queue_filter() -> QueueFilter {
    QueueFilter::default()
}

pub fn default_config() -> ReviewConfig {
    ReviewConfig::default()
}

/// 重判成功的新记录（基于当前记录 revision+1）。
pub fn regraded(previous: &GradeRecord, awarded: i64, mode: GradeMode) -> GradeRecord {
    let mut record = previous.clone();
    record.id = grade_id();
    record.revision = previous.revision + 1;
    record.mode = mode;
    record.awarded = milli(awarded);
    record.judgement = judgement(record.awarded, record.max_score);
    record.status = GradeStatus::AutoAccepted;
    record.sampled = false;
    record.reviewed_at = None;
    record.review_note = None;
    record
}

pub fn answer_view(text: &str, confidence_value: f64) -> AnswerView {
    AnswerView {
        text: text.to_owned(),
        latex: None,
        confidence: Some(confidence(confidence_value)),
        source: "recognition".to_owned(),
    }
}
