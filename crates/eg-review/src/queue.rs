//! 队列：标准待复核（风险排序）与快速抽检（只读待办），游标分页。
//!
//! 排序（`SPEC-review.md` §3.2）：标准队列按原因严重度 → 置信度升序 → 分值降序 →
//! 题序 → 学号；抽检队列按抽检顺序（本项目以题序/学号近似）→ 稳定排序。
//! 游标为稳定排序键（不含判分记录 id），翻页不重复；新修订会改变排序键，见研究记录限制。

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "排序键把置信度编码为千分整数（规范展示精度 3 位）"
)]

use eg_domain::{GradeMode, GradeRecord, GradeStatus, ReviewReasonKind};

use crate::model::{
    QueueCounts, QueueFilter, QueueItem, QueueKind, QueuePage, ReviewConfig, ReviewError,
    ReviewMetadata,
};
use crate::ports::ReviewRepository;

/// 标准队列风险严重度（数字越小越优先）。
#[must_use]
pub fn reason_severity(reason: ReviewReasonKind) -> u8 {
    match reason {
        ReviewReasonKind::Conflict => 0,
        ReviewReasonKind::Ungradable => 1,
        ReviewReasonKind::LowConfidence => 2,
        ReviewReasonKind::MissingEvidence => 3,
        ReviewReasonKind::UncalibratedModel => 4,
        ReviewReasonKind::BackendUnavailable => 5,
        ReviewReasonKind::InvalidScore => 6,
    }
}

/// 记录所属队列（不属于任何队列时为空：标准自动接受未抽检/快速自动接受未抽检）。
#[must_use]
pub fn classify_record(record: &GradeRecord, _config: &ReviewConfig) -> Option<QueueKind> {
    match record.status {
        GradeStatus::ManuallyReviewed => Some(QueueKind::Confirmed),
        GradeStatus::NeedsReview => Some(QueueKind::Standard),
        GradeStatus::Excluded => None,
        GradeStatus::AutoAccepted => {
            if record.mode == GradeMode::Fast && record.sampled {
                Some(QueueKind::Sampling)
            } else {
                None
            }
        }
    }
}

/// 标准队列原因（来自 grade 门禁阻塞原因，已去重并按严重度排序）。
#[must_use]
pub fn queue_reasons(record: &GradeRecord, config: &ReviewConfig) -> Vec<ReviewReasonKind> {
    let mut reasons =
        record.auto_accept_blockers(config.grade.threshold(&record.route, record.mode));
    reasons.sort_by_key(|reason| reason_severity(*reason));
    reasons.dedup();
    reasons
}

fn encode_key(
    kind: QueueKind,
    severity: u8,
    confidence: f64,
    max_milli: i64,
    ordinal: u32,
    student_no: &str,
    record: &GradeRecord,
) -> String {
    // 固定宽度字段，字符串比较等价于元组比较；不含个人信息（学号仅内部排序，日志需脱敏）。
    let confidence_milli = (confidence * 1000.0).round() as i64;
    let neg_max = -max_milli;
    format!(
        "{:02}|{:02}|{:06}|{:019}|{:06}|{}|{}|{}",
        kind as u8,
        severity,
        confidence_milli.clamp(0, 1_000_000),
        neg_max,
        ordinal,
        student_no,
        record.submission_id,
        record.question_id
    )
}

/// 构造队列条目。
pub(crate) fn item_for(
    record: &GradeRecord,
    kind: QueueKind,
    metadata: &ReviewMetadata,
    config: &ReviewConfig,
) -> QueueItem {
    let reasons = if kind == QueueKind::Standard {
        queue_reasons(record, config)
    } else {
        Vec::new()
    };
    let severity = reasons
        .iter()
        .map(|reason| reason_severity(*reason))
        .min()
        .unwrap_or(7);
    let sort_key = encode_key(
        kind,
        severity,
        record.confidence.get(),
        record.max_score.as_milli(),
        metadata.question_ordinal,
        &metadata.student_no,
        record,
    );
    QueueItem {
        grade_id: record.id,
        submission_id: record.submission_id,
        question_id: record.question_id,
        student_no: metadata.student_no.clone(),
        student_display_name: metadata.student_display_name.clone(),
        question_label: metadata.question_label.clone(),
        mode: record.mode,
        sampled: record.sampled,
        status: record.status,
        awarded: record.awarded,
        max_score: record.max_score,
        confidence: record.confidence,
        reasons,
        revision: record.revision,
        kind,
        sort_key,
    }
}

fn validate_cursor(cursor: &str) -> Result<(), ReviewError> {
    // 排序键：kind|severity|confidence|neg_max|ordinal|student_no|submission|question（8 段）
    let parts: Vec<&str> = cursor.split('|').collect();
    if parts.len() != 8 {
        return Err(ReviewError::InvalidCursor(cursor.to_owned()));
    }
    if parts[0].parse::<u8>().is_err()
        || parts[1].parse::<u8>().is_err()
        || parts[2].parse::<i64>().is_err()
        || parts[3].parse::<i64>().is_err()
        || parts[4].parse::<u32>().is_err()
        || parts[6].is_empty()
        || parts[7].is_empty()
    {
        return Err(ReviewError::InvalidCursor(cursor.to_owned()));
    }
    Ok(())
}

/// 计数（全量，不受分页影响；按当前过滤条件之外的全库计数，供状态栏）。
#[must_use]
pub fn counts(repo: &dyn ReviewRepository, config: &ReviewConfig) -> QueueCounts {
    let mut counts = QueueCounts::default();
    for record in repo.all_latest() {
        match classify_record(&record, config) {
            Some(QueueKind::Standard) => counts.standard_pending += 1,
            Some(QueueKind::Sampling) => counts.sampling_pending += 1,
            Some(QueueKind::Confirmed) => counts.confirmed += 1,
            None => {}
        }
    }
    counts
}

/// 构建队列页（游标分页；`cursor` 为上一页 `next_cursor`）。
///
/// # Errors
///
/// 游标非法返回 [`ReviewError::InvalidCursor`]。
pub fn build_queue(
    repo: &dyn ReviewRepository,
    filter: &QueueFilter,
    cursor: Option<&str>,
    config: &ReviewConfig,
) -> Result<QueuePage, ReviewError> {
    if let Some(cursor) = cursor {
        validate_cursor(cursor)?;
    }
    let counts = counts(repo, config);
    let mut items: Vec<QueueItem> = Vec::new();
    for record in repo.all_latest() {
        let Some(kind) = classify_record(&record, config) else {
            continue;
        };
        if !filter.kinds.is_empty() && !filter.kinds.contains(&kind) {
            continue;
        }
        if let Some(status_filter) = filter.statuses.first()
            && !filter.statuses.contains(&record.status)
        {
            let _ = status_filter;
            continue;
        }
        if let Some(mode) = filter.mode
            && record.mode != mode
        {
            continue;
        }
        if let Some(sampled) = filter.sampled
            && record.sampled != sampled
        {
            continue;
        }
        if let Some(question_id) = filter.question_id
            && record.question_id != question_id
        {
            continue;
        }
        let metadata = repo.metadata(&record);
        if let Some(student_no) = &filter.student_no
            && &metadata.student_no != student_no
        {
            continue;
        }
        let item = item_for(&record, kind, &metadata, config);
        if !filter.reasons.is_empty()
            && !filter
                .reasons
                .iter()
                .any(|reason| item.reasons.contains(reason))
        {
            continue;
        }
        items.push(item);
    }
    items.sort_by(|left, right| left.sort_key.cmp(&right.sort_key));
    let total = items.len();
    let start = cursor.map_or(0, |cursor| {
        items
            .iter()
            .position(|item| item.sort_key.as_str() > cursor)
            .unwrap_or(items.len())
    });
    let page_size = config.page_size.max(1);
    let end = start.saturating_add(page_size).min(total);
    let page_items: Vec<QueueItem> = items[start..end].to_vec();
    let next_cursor = if end < total {
        page_items.last().map(|item| item.sort_key.clone())
    } else {
        None
    };
    Ok(QueuePage {
        items: page_items,
        next_cursor,
        total,
        counts,
    })
}

/// 某记录的队列种类（详情/命令校验用）。
#[must_use]
pub fn kind_of(record: &GradeRecord, config: &ReviewConfig) -> Option<QueueKind> {
    classify_record(record, config)
}

/// 队列过滤原因集合（测试/调试辅助）。
#[must_use]
pub fn reasons_of(records: &[GradeRecord], config: &ReviewConfig) -> Vec<ReviewReasonKind> {
    let mut reasons: Vec<ReviewReasonKind> = records
        .iter()
        .flat_map(|record| queue_reasons(record, config))
        .collect();
    reasons.sort_by_key(|reason| reason_severity(*reason));
    reasons.dedup();
    reasons
}
