//! 模型输出硬校验（`SPEC-grade.md` §5.2/§5.3、T17）。
//!
//! 任一硬性校验失败 → 不可判定并进人工（不猜）；按评分标准判定时逐项覆盖且
//! 聚合得分必须等于声明得分；不一致记为冲突并采用聚合值（§8）。
//! 修复重试由流水线控制（本模块只返回结构化错误，便于重试提示）。

use eg_domain::{
    CalibrationId, Confidence, EngineStamp, JudgeOutcome, JudgedUnit, Judgement, MilliScore,
    Rubric, RubricItemId, UngradableReason,
};

use crate::aggregate::{AggregateError, aggregate};
use crate::ports::{JudgeRequest, JudgeResponse, JudgeVerdict};

/// 冲突类型（§8 冲突仲裁）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictKind {
    /// 规则聚合与模型声明得分不一致（采用聚合值）。
    AggregateMismatch {
        /// 模型声明得分。
        declared: MilliScore,
        /// 规则聚合得分。
        aggregated: MilliScore,
    },
    /// 规则「不相等」vs 模型「一致」。
    RuleNotEqualModelMatch,
    /// 规则「相等」vs 模型「非一致」。
    RuleMatchModelNotMatch,
    /// 模型输出不合法（Schema/一致性），已进入修复或不可判定。
    InvalidOutput,
}

impl ConflictKind {
    /// 可读原因。
    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Self::AggregateMismatch {
                declared,
                aggregated,
            } => format!(
                "模型声明得分 {} 与规则聚合 {} 不一致，采用聚合值",
                declared.as_milli(),
                aggregated.as_milli()
            ),
            Self::RuleNotEqualModelMatch => "规则判不相等但模型判一致：冲突未解决".to_owned(),
            Self::RuleMatchModelNotMatch => "规则判相等但模型判非一致：冲突".to_owned(),
            Self::InvalidOutput => "模型输出不合法".to_owned(),
        }
    }
}

/// 校验错误（不可判定原因 + 修复提示）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    /// 理由为空。
    #[error("判定理由为空")]
    EmptyReason,
    /// 理由超长。
    #[error("判定理由过长（{0} 字，上限 {1}）")]
    ReasonTooLong(usize, usize),
    /// 得分越界/与判定不一致。
    #[error("得分与判定不一致：{0}")]
    InconsistentScore(String),
    /// 得分不是粒度整数倍。
    #[error("得分 {0} 不是粒度 {1} 的整数倍")]
    GranularityViolation(i64, i64),
    /// 逐项覆盖不全/重复/未知项。
    #[error("逐项覆盖非法：{0}")]
    Coverage(String),
    /// 评分项聚合失败。
    #[error("评分项聚合失败：{0}")]
    Aggregate(String),
    /// 命中候选序号越界。
    #[error("命中候选序号 {index} 越界（候选数 {count}）")]
    AlternativeOutOfRange {
        /// 序号。
        index: u32,
        /// 候选数。
        count: u32,
    },
    /// 引擎指纹非法。
    #[error("引擎指纹非法：{0}")]
    Engine(String),
    /// 原始输出为空。
    #[error("模型原始输出为空")]
    EmptyOutput,
    /// 推理层 Schema 错误（可修复重试）。
    #[error("模型输出 Schema 错误：{0}")]
    Schema(String),
    /// 云端调用缺少授权。
    #[error("云端模型调用缺少任务授权 id（规范 §5.2）")]
    CloudConsentMissing,
    /// 无评分标准却返回逐项满足情况。
    #[error("无评分标准但模型返回逐项满足情况")]
    UnexpectedItemization,
}

impl ValidationError {
    /// 面向教师的原因。
    #[must_use]
    pub fn reason(&self) -> String {
        self.to_string()
    }

    /// 是否可通过修复重试解决（Schema 类错误）。
    #[must_use]
    pub fn is_repairable(&self) -> bool {
        !matches!(
            self,
            Self::CloudConsentMissing | Self::Engine(_) | Self::AlternativeOutOfRange { .. }
        )
    }
}

/// 校验后的判定（携带聚合/冲突信息）。
#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedJudge {
    /// 领域层判定产出。
    pub outcome: JudgeOutcome,
    /// 判定（已按聚合结果修正）。
    pub judgement: Judgement,
    /// 生效满足项。
    pub satisfied: Vec<RubricItemId>,
    /// 缺失项。
    pub missing: Vec<RubricItemId>,
    /// 证据块最低置信度。
    pub evidence_confidence: Option<Confidence>,
    /// 引擎指纹。
    pub engine: EngineStamp,
    /// 延迟（毫秒）。
    pub latency_ms: u64,
    /// 费用（微成本单位）。
    pub cost_micros: u64,
    /// 原始结构化输出。
    pub output: serde_json::Value,
    /// 冲突列表（为空表示无冲突）。
    pub conflicts: Vec<ConflictKind>,
}

/// 判定「一致/部分/不一致」的得分边界检查。
fn require_score(
    verdict: &JudgeVerdict,
    awarded: Option<MilliScore>,
    max_score: MilliScore,
) -> Result<MilliScore, ValidationError> {
    let score = awarded.ok_or_else(|| {
        ValidationError::InconsistentScore("非不可判定结论必须携带得分".to_owned())
    })?;
    match verdict {
        JudgeVerdict::Match => {
            if score != max_score {
                return Err(ValidationError::InconsistentScore(format!(
                    "一致必须满分（{}），实际 {}",
                    max_score.as_milli(),
                    score.as_milli()
                )));
            }
        }
        JudgeVerdict::Mismatch => {
            if !score.is_zero() {
                return Err(ValidationError::InconsistentScore(
                    "不一致必须 0 分".to_owned(),
                ));
            }
        }
        JudgeVerdict::Partial => {
            if score.is_zero() || score >= max_score {
                return Err(ValidationError::InconsistentScore(
                    "部分一致得分必须在开区间 (0, 满分)".to_owned(),
                ));
            }
        }
        JudgeVerdict::Ungradable(_) => {}
    }
    Ok(score)
}

/// 校验模型响应并转换为领域判定。
///
/// `candidate_count` 为答案键候选数量（命中序号边界）。
///
/// # Errors
///
/// Schema/一致性/覆盖/粒度/引擎等硬校验失败返回 [`ValidationError`]。
pub fn validate_judge_response(
    response: &JudgeResponse,
    request: &JudgeRequest<'_>,
    rubric: Option<&Rubric>,
    candidate_count: u32,
) -> Result<ValidatedJudge, ValidationError> {
    let reason = response.reason.trim();
    if reason.is_empty() {
        return Err(ValidationError::EmptyReason);
    }
    if reason.chars().count() > request.max_reason_chars {
        return Err(ValidationError::ReasonTooLong(
            reason.chars().count(),
            request.max_reason_chars,
        ));
    }
    if let Some(index) = response.matched_alternative
        && index >= candidate_count
    {
        return Err(ValidationError::AlternativeOutOfRange {
            index,
            count: candidate_count,
        });
    }
    response
        .engine
        .validate()
        .map_err(|error| ValidationError::Engine(error.to_string()))?;
    if response.engine.is_cloud()
        && response
            .model_call
            .as_ref()
            .and_then(|call| call.consent_id)
            .is_none()
    {
        // 云端调用必须携带任务授权 id（真实推理服务返回的 ModelCall 已带 consent）
        return Err(ValidationError::CloudConsentMissing);
    }
    if response.output.is_null() {
        return Err(ValidationError::EmptyOutput);
    }
    if let Some(score) = response.awarded
        && !score.is_multiple_of(request.granularity).unwrap_or(false)
    {
        return Err(ValidationError::GranularityViolation(
            score.as_milli(),
            request.granularity.as_milli(),
        ));
    }
    let max_score = request.question.max_score;
    let mut conflicts: Vec<ConflictKind> = Vec::new();
    let (judgement, satisfied, missing) = match &response.verdict {
        JudgeVerdict::Ungradable(reason) => (
            Judgement::Ungradable { reason: *reason },
            Vec::new(),
            Vec::new(),
        ),
        JudgeVerdict::Match | JudgeVerdict::Mismatch | JudgeVerdict::Partial => {
            let declared = require_score(&response.verdict, response.awarded, max_score)?;
            if let Some(rubric) = rubric {
                let validated = validate_itemized(
                    rubric,
                    response,
                    declared,
                    request.granularity,
                    &mut conflicts,
                )?;
                let (judgement, satisfied, missing) = validated;
                (judgement, satisfied, missing)
            } else {
                if !response.satisfied.is_empty() || !response.missing.is_empty() {
                    return Err(ValidationError::UnexpectedItemization);
                }
                let judgement = match &response.verdict {
                    JudgeVerdict::Match => Judgement::Match {
                        score: max_score,
                        reason: reason.to_owned(),
                        evidence: Vec::new(),
                    },
                    JudgeVerdict::Mismatch => Judgement::Mismatch {
                        score: MilliScore::ZERO,
                        reason: reason.to_owned(),
                        evidence: Vec::new(),
                    },
                    JudgeVerdict::Partial => Judgement::Partial {
                        score: declared,
                        satisfied: Vec::new(),
                        missing: Vec::new(),
                        reason: reason.to_owned(),
                    },
                    JudgeVerdict::Ungradable(_) => unreachable!("已在上方分支处理"),
                };
                (judgement, Vec::new(), Vec::new())
            }
        }
    };
    let outcome = JudgeOutcome {
        judgement: judgement.clone(),
        confidence: response.confidence,
        matched_alternative: response.matched_alternative,
        calibration_id: response.calibration_id.clone(),
    };
    let _: Option<CalibrationId> = response.calibration_id;
    Ok(ValidatedJudge {
        outcome,
        judgement,
        satisfied,
        missing,
        evidence_confidence: response.evidence_confidence,
        engine: response.engine.clone(),
        latency_ms: response.latency_ms,
        cost_micros: response.cost_micros,
        output: response.output.clone(),
        conflicts,
    })
}

type Itemized = (Judgement, Vec<RubricItemId>, Vec<RubricItemId>);

fn validate_itemized(
    rubric: &Rubric,
    response: &JudgeResponse,
    declared: MilliScore,
    granularity: MilliScore,
    conflicts: &mut Vec<ConflictKind>,
) -> Result<Itemized, ValidationError> {
    let item_ids: Vec<RubricItemId> = rubric.items.iter().map(|item| item.id).collect();
    for item in &response.satisfied {
        if !item_ids.contains(item) {
            return Err(ValidationError::Coverage(format!(
                "满足项 {item} 不在评分标准中"
            )));
        }
    }
    for item in &response.missing {
        if !item_ids.contains(item) {
            return Err(ValidationError::Coverage(format!(
                "缺失项 {item} 不在评分标准中"
            )));
        }
    }
    let mut seen: Vec<RubricItemId> = Vec::new();
    for item in response.satisfied.iter().chain(response.missing.iter()) {
        if seen.contains(item) {
            return Err(ValidationError::Coverage(format!("评分项 {item} 重复出现")));
        }
        seen.push(*item);
    }
    if seen.len() != item_ids.len() {
        return Err(ValidationError::Coverage(format!(
            "逐项覆盖不全：覆盖 {} 项，评分标准 {} 项",
            seen.len(),
            item_ids.len()
        )));
    }
    let aggregated = aggregate(rubric, &response.satisfied).map_err(|error| match error {
        AggregateError::UnknownItem(item) => {
            ValidationError::Coverage(format!("满足项 {item} 不在评分标准中"))
        }
        other => ValidationError::Aggregate(other.to_string()),
    })?;
    let score = aggregated.score;
    if score != declared {
        conflicts.push(ConflictKind::AggregateMismatch {
            declared,
            aggregated: score,
        });
    }
    let judgement = if score == rubric.total {
        Judgement::Match {
            score: rubric.total,
            reason: format!("按评分标准逐项聚合：{}", response.reason.trim()),
            evidence: Vec::new(),
        }
    } else if score.is_zero() {
        Judgement::Mismatch {
            score: MilliScore::ZERO,
            reason: format!("按评分标准逐项聚合：{}", response.reason.trim()),
            evidence: Vec::new(),
        }
    } else {
        if !score.is_multiple_of(granularity).unwrap_or(false) {
            return Err(ValidationError::GranularityViolation(
                score.as_milli(),
                granularity.as_milli(),
            ));
        }
        let satisfied_units: Vec<JudgedUnit> = aggregated
            .satisfied
            .iter()
            .map(|id| JudgedUnit::RubricItem(*id))
            .collect();
        let missing_units: Vec<JudgedUnit> = item_ids
            .iter()
            .filter(|id| !aggregated.satisfied.contains(id))
            .map(|id| JudgedUnit::RubricItem(*id))
            .collect();
        Judgement::Partial {
            score,
            satisfied: satisfied_units,
            missing: missing_units,
            reason: format!("按评分标准逐项聚合：{}", response.reason.trim()),
        }
    };
    Ok((judgement, aggregated.satisfied, response.missing.clone()))
}

/// 判定与得分一致性校验（领域层 `Judgement::validate_against` 的封装）。
///
/// # Errors
///
/// 不一致时返回 [`ValidationError`]。
pub fn validate_judgement_against(
    judgement: &Judgement,
    max_score: MilliScore,
) -> Result<(), ValidationError> {
    judgement
        .validate_against(max_score)
        .map_err(|error| ValidationError::InconsistentScore(error.to_string()))
}

/// 不可判定原因 → 校验错误（供流水线降级）。
#[must_use]
pub fn ungradable_reason(error: &ValidationError) -> UngradableReason {
    let _ = error;
    UngradableReason::InvalidSchema
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn score_boundaries_are_enforced() {
        let max = MilliScore::from_milli(3000).unwrap();
        assert!(require_score(&JudgeVerdict::Match, Some(max), max).is_ok());
        assert!(require_score(&JudgeVerdict::Match, Some(MilliScore::ZERO), max).is_err());
        assert!(require_score(&JudgeVerdict::Mismatch, Some(MilliScore::ZERO), max).is_ok());
        assert!(
            require_score(
                &JudgeVerdict::Mismatch,
                Some(MilliScore::from_milli(100).unwrap()),
                max
            )
            .is_err()
        );
        assert!(
            require_score(
                &JudgeVerdict::Partial,
                Some(MilliScore::from_milli(1000).unwrap()),
                max
            )
            .is_ok()
        );
        assert!(require_score(&JudgeVerdict::Partial, Some(max), max).is_err());
    }
}
