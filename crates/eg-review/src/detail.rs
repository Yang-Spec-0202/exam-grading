//! 详情：题目/学生、答案与参考、证据、比较器轨迹、模型调用、判定、历史修订、抽检标记。

use eg_domain::{EvidenceRef, GradeId, GradeMode, GradeStatus};

use crate::model::{AnswerView, CommandKind, QueueKind, ReviewConfig, ReviewDetail, ReviewError};
use crate::ports::ReviewRepository;
use crate::queue::{classify_record, item_for};

/// 构造详情（图片只给资产 id，按需懒加载；不返回像素）。
///
/// # Errors
///
/// 记录不存在返回 [`ReviewError::NotFound`]。
pub fn build_detail(
    repo: &dyn ReviewRepository,
    grade_id: GradeId,
    config: &ReviewConfig,
) -> Result<ReviewDetail, ReviewError> {
    let latest = repo.get(grade_id).ok_or(ReviewError::NotFound(grade_id))?;
    let history = repo.revisions(latest.submission_id, latest.question_id);
    let kind = classify_record(&latest, config);
    let metadata = repo.metadata(&latest);
    let item = item_for(
        &latest,
        kind.unwrap_or(QueueKind::Confirmed),
        &metadata,
        config,
    );
    let mut assets = Vec::new();
    let mut comparator_traces = Vec::new();
    let mut model_calls = Vec::new();
    for evidence in &latest.evidence {
        match evidence {
            EvidenceRef::Asset(asset) => assets.push(*asset),
            EvidenceRef::ComparatorTrace(id) => {
                if let Some(trace) = repo.comparator_trace(*id) {
                    comparator_traces.push(trace);
                }
            }
            EvidenceRef::ModelCall(id) => {
                if let Some(call) = repo.model_call(*id) {
                    model_calls.push(call);
                }
            }
            EvidenceRef::Recognition(_) => {}
        }
    }
    let answer = repo.answer(&latest);
    let reference = repo.reference(latest.question_id);
    let read_only = kind == Some(QueueKind::Sampling) && config.sampled_readonly;
    let allowed_commands = allowed_commands(&latest, kind, config);
    let has_fast = history.iter().any(|record| record.mode == GradeMode::Fast);
    let has_standard = history
        .iter()
        .any(|record| record.mode == GradeMode::Standard);
    let transferred_to_standard = has_fast && has_standard;
    Ok(ReviewDetail {
        item,
        judgement: latest.judgement.clone(),
        latest,
        history,
        answer,
        reference,
        evidence: repo_answer_evidence(repo, grade_id),
        assets,
        comparator_traces,
        model_calls,
        read_only,
        allowed_commands,
        transferred_to_standard,
    })
}

fn repo_answer_evidence(repo: &dyn ReviewRepository, grade_id: GradeId) -> Vec<EvidenceRef> {
    repo.get(grade_id)
        .map_or_else(Vec::new, |record| record.evidence)
}

/// 允许命令（模式/抽检/只读约束）。
#[must_use]
pub fn allowed_commands(
    record: &eg_domain::GradeRecord,
    kind: Option<QueueKind>,
    config: &ReviewConfig,
) -> Vec<CommandKind> {
    match kind {
        Some(QueueKind::Standard) => vec![
            CommandKind::Accept,
            CommandKind::OverrideScore,
            CommandKind::RegradeText,
            CommandKind::ExcludeQuestion,
            CommandKind::ExcludeSubmission,
            CommandKind::BatchAccept,
            CommandKind::Undo,
            CommandKind::Redo,
        ],
        Some(QueueKind::Sampling) => {
            let _ = config.sampled_readonly;
            vec![
                CommandKind::SamplingPass,
                CommandKind::SamplingFail,
                CommandKind::TransferToStandard,
                CommandKind::Undo,
                CommandKind::Redo,
            ]
        }
        Some(QueueKind::Confirmed) => vec![CommandKind::Undo, CommandKind::Redo],
        None => {
            if record.mode == GradeMode::Fast {
                vec![
                    CommandKind::TransferToStandard,
                    CommandKind::Undo,
                    CommandKind::Redo,
                ]
            } else {
                Vec::new()
            }
        }
    }
}

/// 详情中的答案视图缺省说明（UI 降级）。
#[must_use]
pub fn answer_or_degraded(answer: Option<AnswerView>) -> AnswerView {
    answer.unwrap_or(AnswerView {
        text: "（图片/识别加载失败）".to_owned(),
        latex: None,
        confidence: None,
        source: "unavailable".to_owned(),
    })
}

/// 状态是否为只读编辑（快速抽检且只读开关开）。
#[must_use]
pub fn is_read_only(record: &eg_domain::GradeRecord, config: &ReviewConfig) -> bool {
    record.mode == GradeMode::Fast
        && record.sampled
        && record.status == GradeStatus::AutoAccepted
        && config.sampled_readonly
}
