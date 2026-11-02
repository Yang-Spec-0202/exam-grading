//! 可复用学科契约测试套件（`SPEC-domain.md` §9、§12）。
//!
//! 所有学科实现（如 `math`）都应在自己的测试里调用 [`check_subject_pack`]，
//! 用同一套契约检查「归一化幂等、比较器链顺序与终止、解析失败不得判为不等、
//! 静态校验、能力声明不虚报」。本模块是**纯函数**，不依赖测试框架，
//! 因此可在下游 crate 的单元测试与集成测试中复用。
//!
//! 故意违规的「反例包」也应在各实现测试中出现：契约检查必须能报出违规。

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::answer::{AnswerValue, ConcreteKey};
use crate::error::DomainError;
use crate::paper::{Question, QuestionKind};
use crate::subject::{
    Comparator, ComparatorVerdict, NormalizedAnswer, RawAnswer, RubricIssue, StepEvidence,
    SubjectPack, error_issue_count,
};
use crate::values::ComparatorId;

/// 契约样本：一道题 + 其答案键 + 若干可比较输入。
///
/// - `correct`：与答案键一致的归一化答案（链必须判为相等/等价）；
/// - `wrong`：明显错误的归一化答案（可空；给定时链必须判为不相等）；
/// - `malformed`：该学科**确定无法解析**的归一化答案（链不得判为不相等）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContractCase {
    /// 题目（含题型、答案键、可选评分标准）。
    pub question: Question,
    /// 具体答案键。
    pub key: ConcreteKey,
    /// 原始答案（可归一化）。
    pub raw: RawAnswer,
    /// 正确的归一化答案。
    pub correct: NormalizedAnswer,
    /// 明显错误的归一化答案（可空）。
    pub wrong: Option<NormalizedAnswer>,
    /// 确定无法解析的归一化答案。
    pub malformed: NormalizedAnswer,
}

/// 契约套件：一组样本。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ContractSuite {
    /// 样本列表。
    pub cases: Vec<ContractCase>,
}

/// 比较器链执行结果：首个确定结论终止（`SPEC-domain.md` §9 链式规则）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChainOutcome {
    /// 最终判定；全部无法判定时保留最后一个「无法判定」的原因。
    pub verdict: ComparatorVerdict,
    /// 已尝试的比较器 id 与判定（按执行顺序）。
    pub attempts: Vec<(ComparatorId, ComparatorVerdict)>,
}

/// 按「首个确定结论终止」执行比较器链。
///
/// 没有比较器或全部无法判定时返回 [`ComparatorVerdict::Undecidable`]，
/// 上游据此升级模型或转人工（规范 §9）。
#[must_use]
pub fn run_comparator_chain(
    comparators: &[Box<dyn Comparator>],
    expected: &AnswerValue,
    answer: &NormalizedAnswer,
) -> ChainOutcome {
    let mut attempts = Vec::with_capacity(comparators.len());
    let mut last_undecidable: Option<ComparatorVerdict> = None;
    for comparator in comparators {
        let id = comparator.id();
        let verdict = comparator.compare(expected, answer);
        attempts.push((id, verdict.clone()));
        if verdict.is_conclusive() {
            return ChainOutcome { verdict, attempts };
        }
        last_undecidable = Some(verdict);
    }
    let verdict = last_undecidable.unwrap_or_else(|| ComparatorVerdict::Undecidable {
        reason: "没有可用比较器".to_owned(),
    });
    ChainOutcome { verdict, attempts }
}

/// 契约违规项。
#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
pub enum ContractViolation {
    /// 能力声明自身不合法（空题型/空比较器/空提示词版本/重复项）。
    #[error("能力声明不合法：{reason}")]
    CapabilitiesInvalid {
        /// 可读原因。
        reason: String,
    },
    /// 用例题型未被声明支持。
    #[error("能力声明未包含用例题型：{kind:?}")]
    DeclaredKindUnsupported {
        /// 用例题型。
        kind: QuestionKind,
    },
    /// 原始答案归一化失败（契约样本必须是可归一化的）。
    #[error("归一化失败：{input:?}（{reason}）")]
    NormalizeFailed {
        /// 原始输入。
        input: String,
        /// 可读原因。
        reason: String,
    },
    /// 归一化不幂等。
    #[error("归一化不幂等（同一输入两次结果不同）")]
    NormalizeNotIdempotent,
    /// 默认比较器链为空。
    #[error("默认比较器链为空（按题型必须有默认链）")]
    EmptyComparatorChain,
    /// 比较器链顺序与答案键声明不一致。
    #[error("比较器链顺序与答案键声明不一致：期望 {expected:?}，实际 {actual:?}")]
    ChainOrderViolation {
        /// 答案键声明的比较器顺序。
        expected: Vec<String>,
        /// 实际链顺序。
        actual: Vec<String>,
    },
    /// 链使用了能力声明之外的比较器。
    #[error("链使用了未声明的比较器：{comparator}")]
    UndeclaredComparator {
        /// 比较器 id。
        comparator: String,
    },
    /// 链中比较器 id 重复。
    #[error("链中比较器 id 重复：{comparator}")]
    DuplicateComparator {
        /// 比较器 id。
        comparator: String,
    },
    /// 解析失败被判定为「不相等」（规范明确禁止）。
    #[error("解析失败不得判为不等，但比较器 {comparator} 返回了 NotEqual")]
    ParseFailureJudgedNotEqual {
        /// 比较器 id。
        comparator: String,
    },
    /// 正确样答案未被接受。
    #[error("正确样本未被判为相等/等价（链全部无法判定或判为不等）")]
    CorrectAnswerUndecided,
    /// 正确样本被拒绝。
    #[error("正确样本被判为错误：{verdict}")]
    CorrectAnswerRejected {
        /// 判定描述。
        verdict: String,
    },
    /// 错误样本被接受。
    #[error("错误样本被判为相等/等价：{verdict}")]
    WrongAnswerAccepted {
        /// 判定描述。
        verdict: String,
    },
    /// 静态校验拒绝了合法题目（错误级问题）。
    #[error("静态校验对合法题目返回错误级问题：{message}")]
    StaticValidationRejectsCase {
        /// 问题消息。
        message: String,
    },
    /// 规则问题消息为空。
    #[error("规则问题消息不得为空")]
    EmptyIssueMessage,
    /// 判定 profile id 为空。
    #[error("判定 profile id 不得为空")]
    EmptyJudgeProfile,
    /// 显示名为空。
    #[error("学科显示名不得为空")]
    EmptyDisplayName,
    /// 能力声明「不支持分步给分」却返回了步骤证据（虚报）。
    #[error("能力声明不支持分步给分，却返回了步骤证据（虚报）")]
    StepScoringCapabilityMismatch,
}

impl ContractViolation {
    /// 稳定错误码（供测试断言与统计）。
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::CapabilitiesInvalid { .. } => "capabilities_invalid",
            Self::DeclaredKindUnsupported { .. } => "declared_kind_unsupported",
            Self::NormalizeFailed { .. } => "normalize_failed",
            Self::NormalizeNotIdempotent => "normalize_not_idempotent",
            Self::EmptyComparatorChain => "empty_comparator_chain",
            Self::ChainOrderViolation { .. } => "chain_order_violation",
            Self::UndeclaredComparator { .. } => "undeclared_comparator",
            Self::DuplicateComparator { .. } => "duplicate_comparator",
            Self::ParseFailureJudgedNotEqual { .. } => "parse_failure_judged_not_equal",
            Self::CorrectAnswerUndecided => "correct_answer_undecided",
            Self::CorrectAnswerRejected { .. } => "correct_answer_rejected",
            Self::WrongAnswerAccepted { .. } => "wrong_answer_accepted",
            Self::StaticValidationRejectsCase { .. } => "static_validation_rejects_case",
            Self::EmptyIssueMessage => "empty_issue_message",
            Self::EmptyJudgeProfile => "empty_judge_profile",
            Self::EmptyDisplayName => "empty_display_name",
            Self::StepScoringCapabilityMismatch => "step_scoring_capability_mismatch",
        }
    }
}

/// 运行完整契约检查；返回**全部**违规（空表示通过）。
#[must_use]
pub fn check_subject_pack(pack: &dyn SubjectPack, suite: &ContractSuite) -> Vec<ContractViolation> {
    let mut violations = Vec::new();
    if let Err(err) = pack.capabilities().validate() {
        violations.push(ContractViolation::CapabilitiesInvalid {
            reason: err.to_string(),
        });
    }
    if pack.display_name().trim().is_empty() {
        violations.push(ContractViolation::EmptyDisplayName);
    }
    if pack.judge_profile_id().trim().is_empty() {
        violations.push(ContractViolation::EmptyJudgeProfile);
    }
    for case in &suite.cases {
        check_case(pack, case, &mut violations);
    }
    violations
}

/// 契约检查的 `Result` 形式：通过 `Ok`，失败返回全部违规。
///
/// # Errors
///
/// 存在任一违规时返回违规列表。
pub fn contract_ok(
    pack: &dyn SubjectPack,
    suite: &ContractSuite,
) -> Result<(), Vec<ContractViolation>> {
    let violations = check_subject_pack(pack, suite);
    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

fn check_case(
    pack: &dyn SubjectPack,
    case: &ContractCase,
    violations: &mut Vec<ContractViolation>,
) {
    let capabilities = pack.capabilities();
    if !capabilities.supports_kind(case.question.kind) {
        violations.push(ContractViolation::DeclaredKindUnsupported {
            kind: case.question.kind,
        });
    }

    // 归一化必须成功且幂等。
    match pack.normalize(&case.raw) {
        Ok(first) => match pack.normalize(&case.raw) {
            Ok(second) => {
                if first != second {
                    violations.push(ContractViolation::NormalizeNotIdempotent);
                }
            }
            Err(err) => violations.push(ContractViolation::NormalizeFailed {
                input: case.raw.text.clone(),
                reason: err.to_string(),
            }),
        },
        Err(err) => {
            violations.push(ContractViolation::NormalizeFailed {
                input: case.raw.text.clone(),
                reason: err.to_string(),
            });
            return;
        }
    }

    // 静态校验：合法题目不得出现错误级问题。
    let issues = pack.validate_question(&case.question);
    if error_issue_count(&issues) > 0 {
        violations.push(ContractViolation::StaticValidationRejectsCase {
            message: issues
                .iter()
                .find(|issue| issue.is_error())
                .map_or_else(String::new, |issue| issue.message.clone()),
        });
    }
    if issues.iter().any(|issue| !issue.is_message_valid()) {
        violations.push(ContractViolation::EmptyIssueMessage);
    }

    check_chain(pack, case, violations);
    check_step_evidence(pack, case, &capabilities, violations);
}

fn check_chain(
    pack: &dyn SubjectPack,
    case: &ContractCase,
    violations: &mut Vec<ContractViolation>,
) {
    let chain = pack.comparator_chain(&case.key, case.question.kind);
    let declared: Vec<String> = case
        .key
        .comparators
        .iter()
        .map(ToString::to_string)
        .collect();
    let actual: Vec<String> = chain
        .iter()
        .map(|comparator| comparator.id().to_string())
        .collect();
    if declared.is_empty() {
        if chain.is_empty() {
            violations.push(ContractViolation::EmptyComparatorChain);
            return;
        }
    } else if declared != actual {
        violations.push(ContractViolation::ChainOrderViolation {
            expected: declared,
            actual: actual.clone(),
        });
    }
    let capabilities = pack.capabilities();
    let mut seen: Vec<&str> = Vec::with_capacity(actual.len());
    for (index, id) in actual.iter().enumerate() {
        if seen.contains(&id.as_str()) {
            violations.push(ContractViolation::DuplicateComparator {
                comparator: id.clone(),
            });
        }
        seen.push(id);
        if !capabilities
            .comparators
            .iter()
            .any(|declared_id| declared_id.as_str() == id)
        {
            violations.push(ContractViolation::UndeclaredComparator {
                comparator: id.clone(),
            });
        }
        let comparator = &chain[index];
        let malformed_verdict = comparator.compare(&case.key.primary, &case.malformed);
        if malformed_verdict.is_not_equal() {
            violations.push(ContractViolation::ParseFailureJudgedNotEqual {
                comparator: id.clone(),
            });
        }
    }

    let correct = run_comparator_chain(&chain, &case.key.primary, &case.correct);
    if !correct.verdict.is_conclusive() {
        violations.push(ContractViolation::CorrectAnswerUndecided);
    } else if !correct.verdict.is_equal_like() {
        violations.push(ContractViolation::CorrectAnswerRejected {
            verdict: describe_verdict(&correct.verdict),
        });
    }

    if let Some(wrong) = &case.wrong {
        let wrong_outcome = run_comparator_chain(&chain, &case.key.primary, wrong);
        if wrong_outcome.verdict.is_equal_like() {
            violations.push(ContractViolation::WrongAnswerAccepted {
                verdict: describe_verdict(&wrong_outcome.verdict),
            });
        }
    }
}

fn check_step_evidence(
    pack: &dyn SubjectPack,
    case: &ContractCase,
    capabilities: &crate::subject::SubjectCapabilities,
    violations: &mut Vec<ContractViolation>,
) {
    let Some(rubric) = &case.question.rubric else {
        return;
    };
    let evidence: Option<StepEvidence> = pack.step_evidence(rubric, &case.correct);
    if !capabilities.supports_step_scoring && evidence.is_some() {
        violations.push(ContractViolation::StepScoringCapabilityMismatch);
    }
}

fn describe_verdict(verdict: &ComparatorVerdict) -> String {
    match verdict {
        ComparatorVerdict::Equal => "equal".to_owned(),
        ComparatorVerdict::Equivalent { note } => format!("equivalent({note})"),
        ComparatorVerdict::NotEqual { reason } => format!("not_equal({reason})"),
        ComparatorVerdict::Undecidable { reason } => format!("undecidable({reason})"),
    }
}

/// 便捷函数：检查是否存在某类违规（测试与下游断言用）。
#[must_use]
pub fn contains_code(violations: &[ContractViolation], code: &str) -> bool {
    violations.iter().any(|violation| violation.code() == code)
}

/// 便捷函数：把违规列表转为可读报告。
#[must_use]
pub fn format_violations(violations: &[ContractViolation]) -> String {
    violations
        .iter()
        .map(|violation| format!("[{}] {violation}", violation.code()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 供实现者使用的门禁包装：静态校验后若存在错误级问题则返回领域错误。
///
/// # Errors
///
/// 存在错误级问题时返回 [`DomainError::RubricHasErrors`]。
pub fn ensure_question_gradable(issues: &[RubricIssue]) -> Result<(), DomainError> {
    crate::subject::ensure_no_error_issues(issues)
}
