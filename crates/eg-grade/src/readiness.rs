//! 试卷就绪检查（`SPEC-grade.md` §3、T16）。
//!
//! 不满足则整卷不得开始判分：每道题要么有可判定的答案键，要么有**已确认**的
//! 评分规则，要么被明确标记为「仅人工」；草稿规则不算生效规则；答案键/规则版本
//! 必须已写入试卷；模板版本必须与答案键版本对齐（若提供模板）。

use eg_domain::{
    ManualReason, Paper, PaperTemplate, Question, QuestionId, RubricIssue, SubjectPack,
};

use crate::ports::{RubricAvailability, RubricSource};

/// 就绪问题类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadinessIssueKind {
    /// 答案键缺失（且无已确认规则/仅人工标记）。
    AnswerKeyMissing,
    /// 答案键「略」且无已确认规则。
    AnswerKeyOmitted,
    /// 规则为草稿未确认。
    RubricDraftNotConfirmed,
    /// 「仅评分标准」但无已确认规则。
    RubricRequiredButMissing,
    /// 答案键版本未写入。
    KeyVersionMissing,
    /// 规则版本未写入。
    RubricVersionMissing,
    /// 模板版本与答案键版本不一致。
    TemplateVersionMismatch,
    /// 题目自身校验失败（分值/选项/容差/维度等）。
    InvalidQuestion,
    /// 具体答案键内容非法。
    InvalidAnswerKey,
}

/// 就绪问题（错误级；任一存在则拒绝整卷判分）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadinessIssue {
    /// 题目（试卷级问题为空）。
    pub question_id: Option<QuestionId>,
    /// 问题类型。
    pub kind: ReadinessIssueKind,
    /// 面向教师的中文说明。
    pub message: String,
}

/// 就绪报告。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReadinessReport {
    /// 错误级问题列表。
    pub issues: Vec<ReadinessIssue>,
}

impl ReadinessReport {
    /// 是否就绪。
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.issues.is_empty()
    }
}

/// 试卷就绪检查。
///
/// `manual_only` 为教师明确标记「仅人工」的题目集合（其余题目必须可自动判分）。
#[must_use]
pub fn check_readiness(
    paper: &Paper,
    template: Option<&PaperTemplate>,
    rubrics: &dyn RubricSource,
    manual_only: &[QuestionId],
    subject: &dyn SubjectPack,
) -> ReadinessReport {
    let mut issues = Vec::new();
    if paper.key_version == 0 {
        issues.push(ReadinessIssue {
            question_id: None,
            kind: ReadinessIssueKind::KeyVersionMissing,
            message: "试卷答案键版本为 0：答案键/规则版本必须写入试卷后才能判分".to_owned(),
        });
    }
    if paper.rubric_version == 0 {
        issues.push(ReadinessIssue {
            question_id: None,
            kind: ReadinessIssueKind::RubricVersionMissing,
            message: "试卷规则版本为 0：规则版本必须写入试卷后才能判分".to_owned(),
        });
    }
    if let Some(template) = template
        && let Err(error) = template.ensure_auto_align_allowed(paper)
    {
        issues.push(ReadinessIssue {
            question_id: None,
            kind: ReadinessIssueKind::TemplateVersionMismatch,
            message: format!("模板与试卷版本不一致：{error}"),
        });
    }
    for question in &paper.questions {
        check_question(question, rubrics, manual_only, subject, &mut issues);
    }
    ReadinessReport { issues }
}

fn check_question(
    question: &Question,
    rubrics: &dyn RubricSource,
    manual_only: &[QuestionId],
    subject: &dyn SubjectPack,
    issues: &mut Vec<ReadinessIssue>,
) {
    let marked_manual = manual_only.contains(&question.id);
    if marked_manual {
        return;
    }
    let availability = rubrics.availability(question.id);
    if let Err(error) = question.validate() {
        issues.push(ReadinessIssue {
            question_id: Some(question.id),
            kind: ReadinessIssueKind::InvalidQuestion,
            message: format!("题目校验失败：{error}"),
        });
        return;
    }
    match &question.key {
        eg_domain::AnswerKey::Concrete(_) => {
            // 学科静态校验（选项/数值/维度/容差等）
            let subject_issues: Vec<RubricIssue> = subject.validate_question(question);
            for issue in subject_issues {
                if issue.is_error() {
                    issues.push(ReadinessIssue {
                        question_id: Some(question.id),
                        kind: ReadinessIssueKind::InvalidAnswerKey,
                        message: format!("答案键/规则静态校验失败：{}", issue.message),
                    });
                }
            }
            if let Err(error) = question.key.validate() {
                issues.push(ReadinessIssue {
                    question_id: Some(question.id),
                    kind: ReadinessIssueKind::InvalidAnswerKey,
                    message: format!("答案键非法：{error}"),
                });
            }
        }
        eg_domain::AnswerKey::RubricOnly => match availability {
            RubricAvailability::Confirmed(_) => {}
            RubricAvailability::Draft => issues.push(ReadinessIssue {
                question_id: Some(question.id),
                kind: ReadinessIssueKind::RubricDraftNotConfirmed,
                message: "答案键为「仅评分标准」，但规则仍是草稿：请先确认规则".to_owned(),
            }),
            RubricAvailability::Missing => issues.push(ReadinessIssue {
                question_id: Some(question.id),
                kind: ReadinessIssueKind::RubricRequiredButMissing,
                message: "答案键为「仅评分标准」，但没有已确认规则".to_owned(),
            }),
        },
        eg_domain::AnswerKey::ExplicitlyOmitted { .. } => match availability {
            // R3：略且无规则 → 仅人工；就绪检查要求明确标记仅人工
            RubricAvailability::Confirmed(_) => {}
            RubricAvailability::Draft => issues.push(ReadinessIssue {
                question_id: Some(question.id),
                kind: ReadinessIssueKind::RubricDraftNotConfirmed,
                message: "答案键为「略」，规则仍是草稿：确认后才能按规则判分".to_owned(),
            }),
            RubricAvailability::Missing => issues.push(ReadinessIssue {
                question_id: Some(question.id),
                kind: ReadinessIssueKind::AnswerKeyOmitted,
                message: "答案键为「略」且无已确认规则：请标记为仅人工或补规则".to_owned(),
            }),
        },
        eg_domain::AnswerKey::Missing => issues.push(ReadinessIssue {
            question_id: Some(question.id),
            kind: ReadinessIssueKind::AnswerKeyMissing,
            message: "答案键缺失：请补答案键、补已确认规则或标记为仅人工".to_owned(),
        }),
    }
    // 规则模式与题型的一致性（若存在已确认规则）
    if let RubricAvailability::Confirmed(rubric) = availability
        && let Err(error) = rubric.validate(question.kind)
    {
        issues.push(ReadinessIssue {
            question_id: Some(question.id),
            kind: ReadinessIssueKind::InvalidQuestion,
            message: format!("评分标准非法：{error}"),
        });
    }
}

/// 手动转人工原因映射（供路由/记录使用）。
#[must_use]
pub fn manual_reason_for(kind: ReadinessIssueKind) -> ManualReason {
    match kind {
        ReadinessIssueKind::AnswerKeyMissing => ManualReason::AnswerKeyMissing,
        ReadinessIssueKind::AnswerKeyOmitted => ManualReason::AnswerKeyOmitted,
        ReadinessIssueKind::RubricDraftNotConfirmed
        | ReadinessIssueKind::RubricRequiredButMissing
        | ReadinessIssueKind::RubricVersionMissing => ManualReason::RubricMissing,
        ReadinessIssueKind::KeyVersionMissing
        | ReadinessIssueKind::TemplateVersionMismatch
        | ReadinessIssueKind::InvalidQuestion
        | ReadinessIssueKind::InvalidAnswerKey => ManualReason::OutOfScope,
    }
}
