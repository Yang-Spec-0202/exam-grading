//! 判定链路执行：比较器链（`SPEC-grade.md` §5.1）+ R0 未作答规则。
//!
//! 复用 `eg-domain` 的链执行器（首个确定结论终止），把每次尝试落为
//! `ComparatorTrace`（可复现证据）。多解候选逐个判定，命中最高分（等价即满分），
//! 记录命中候选序号。

use eg_domain::{
    ComparatorTrace, ComparatorTraceId, ComparatorVerdict, ConcreteKey, ManualReason,
    NormalizedAnswer, Question, QuestionId, RouteDecision, SubjectPack,
};

use crate::ports::{IdGenerator, IdSourceError};

/// 规则段判定。
#[derive(Debug, Clone, PartialEq)]
pub enum RuleEvaluation {
    /// 一致（命中候选序号）。
    Match {
        /// 候选序号（0 = 主答案）。
        alternative: usize,
    },
    /// 不一致（附原因）。
    Mismatch {
        /// 可读原因。
        reason: String,
    },
    /// 无法判定（附原因；路由允许时升级模型）。
    Undecidable {
        /// 可读原因。
        reason: String,
    },
}

/// 规则段执行结果（含全部比较器尝试轨迹）。
#[derive(Debug, Clone, PartialEq)]
pub struct RuleRun {
    /// 结论。
    pub evaluation: RuleEvaluation,
    /// 比较轨迹（按候选与比较器顺序）。
    pub traces: Vec<ComparatorTrace>,
}

impl RuleRun {
    /// 轨迹 id 列表（证据引用用）。
    #[must_use]
    pub fn trace_ids(&self) -> Vec<ComparatorTraceId> {
        self.traces.iter().map(|trace| trace.id).collect()
    }
}

/// 执行确定性比较器链（规则段）。
///
/// 多解：对每个候选（主答案 + 备选）分别执行；任一等价即判一致并取最高分；
/// 全部候选都确定不相等才判不一致；存在无法判定且无等价候选时返回无法判定。
pub fn evaluate_rule_chain(
    subject: &dyn SubjectPack,
    question: &Question,
    key: &ConcreteKey,
    normalized: &NormalizedAnswer,
    ids: &dyn IdGenerator,
) -> Result<RuleRun, IdSourceError> {
    let chain = subject.comparator_chain(key, question.kind);
    let candidates: Vec<eg_domain::AnswerValue> = std::iter::once(key.primary.clone())
        .chain(key.alternatives.iter().cloned())
        .collect();
    let mut traces: Vec<ComparatorTrace> = Vec::new();
    let mut undecidable_reason: Option<String> = None;
    let mut rejection_reason: Option<String> = None;
    for (index, candidate) in candidates.iter().enumerate() {
        let outcome = eg_domain::contract::run_comparator_chain(&chain, candidate, normalized);
        for (comparator, verdict) in &outcome.attempts {
            traces.push(ComparatorTrace {
                id: ComparatorTraceId::from_ulid(ids.try_next_ulid()?),
                comparator: comparator.clone(),
                verdict: verdict.clone(),
                detail: serde_json::json!({
                    "candidate": index,
                    "answer_text": normalized.text,
                    "answer_latex": normalized.latex,
                }),
            });
        }
        match &outcome.verdict {
            ComparatorVerdict::Equal | ComparatorVerdict::Equivalent { .. } => {
                return Ok(RuleRun {
                    evaluation: RuleEvaluation::Match { alternative: index },
                    traces,
                });
            }
            ComparatorVerdict::NotEqual { reason } => {
                rejection_reason.get_or_insert_with(|| reason.clone());
            }
            ComparatorVerdict::Undecidable { reason } => {
                undecidable_reason.get_or_insert_with(|| reason.clone());
            }
        }
    }
    let evaluation = if let Some(reason) = undecidable_reason {
        RuleEvaluation::Undecidable { reason }
    } else {
        RuleEvaluation::Mismatch {
            reason: rejection_reason.unwrap_or_else(|| "答案与参考答案不一致".to_owned()),
        }
    };
    Ok(RuleRun { evaluation, traces })
}

/// R0 规则：未作答且识别置信度足够 → 不一致（0 分），不调模型。
#[must_use]
pub fn unanswered_rule() -> RuleEvaluation {
    RuleEvaluation::Mismatch {
        reason: "未作答（学生答案为空）".to_owned(),
    }
}

/// 规则段无需执行（无具体答案键）。
#[must_use]
pub fn no_rule_available() -> RuleEvaluation {
    RuleEvaluation::Undecidable {
        reason: "没有具体答案键，无法进行确定性比较".to_owned(),
    }
}

/// 规则段结论是否需要升级模型。
#[must_use]
pub fn should_escalate(route: &RouteDecision, evaluation: &RuleEvaluation) -> bool {
    matches!(evaluation, RuleEvaluation::Undecidable { .. })
        && matches!(
            route,
            RouteDecision::RuleThenModel
                | RouteDecision::ModelDirect
                | RouteDecision::RubricItemized
        )
}

/// 手动路由对应的规则段结论（保持不可判定并追加原因标签）。
#[must_use]
pub fn manual_rule_reason(reason: ManualReason) -> RuleEvaluation {
    RuleEvaluation::Undecidable {
        reason: format!("路由转人工：{reason:?}"),
    }
}

/// 题目标签（用于模型调用输入哈希，不含个人信息）。
#[must_use]
pub fn question_input_hash_material(
    question: &Question,
    normalized: Option<&NormalizedAnswer>,
) -> String {
    let answer = normalized.map_or_else(String::new, |answer| answer.text.clone());
    format!(
        "q:{}:{}:{}:{}",
        question.id, question.label, question.kind as u8 as char as u8, answer
    )
}

/// 记录题目 id 与答案的组合（避免把整卷信息发模型）。
#[must_use]
pub fn single_question_context(question_id: QuestionId, answer_text: &str) -> String {
    format!("{question_id}:{answer_text}")
}
