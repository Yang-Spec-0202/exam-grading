//! L1 路由：R0–R11 有序表（`SPEC-grade.md` §4、T16）。
//!
//! 纯函数、默认零模型；仅 R9 分类澄清例外（分类动作由调用方注入，
//! 本模块只输出「需要分类」标记）。首个命中即返回，路由轨迹记录规则编号。

use eg_domain::{
    AnswerKey, AnswerValue, Confidence, GradeMode, ManualReason, QuestionKind, RouteDecision,
    UngradableReason,
};

/// 路由输入（只含规范允许的字段）。
#[derive(Debug, Clone)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "路由条件即规范 §4 表的独立布尔输入（题型已知/归一化/后端/冲突/仅人工等）"
)]
pub struct RouteInput {
    /// 题型。
    pub kind: QuestionKind,
    /// 题型是否已知（未知触发 R9 分类澄清）。
    pub kind_known: bool,
    /// 答案键。
    pub key: AnswerKey,
    /// 是否存在已确认规则（草稿不算）。
    pub rubric_confirmed: bool,
    /// 学生答案是否为空。
    pub answer_empty: bool,
    /// 答案级识别置信度。
    pub recognition_confidence: Confidence,
    /// 识别置信度下限。
    pub recognition_threshold: Confidence,
    /// 学科归一化是否失败。
    pub normalization_failed: bool,
    /// 判定后端是否可用。
    pub backend_available: bool,
    /// 答案键与题型是否明显冲突。
    pub key_kind_conflict: bool,
    /// 教师是否明确标记「仅人工」。
    pub manual_only: bool,
}

/// 路由结果。
#[derive(Debug, Clone, PartialEq)]
pub struct RouteOutcome {
    /// 路由决策。
    pub decision: RouteDecision,
    /// 命中规则编号（`R0`–`R11`）。
    pub rule_id: &'static str,
    /// 是否需要 R9 分类澄清（由调用方执行一次分类后重新路由）。
    pub needs_classification: bool,
}

impl RouteOutcome {
    fn new(rule_id: &'static str, decision: RouteDecision) -> Self {
        Self {
            decision,
            rule_id,
            needs_classification: false,
        }
    }

    /// 手动路由。
    #[must_use]
    pub fn manual(reason: ManualReason) -> Self {
        Self::new("R11", RouteDecision::Manual(reason))
    }
}

/// 执行 R0–R11 路由（首个命中即返回）。
#[must_use]
pub fn route(input: &RouteInput) -> RouteOutcome {
    // R9：题型未知或答案键与题型明显冲突 → 分类澄清一次
    if !input.kind_known || input.key_kind_conflict {
        let mut outcome = RouteOutcome::new("R9", RouteDecision::Manual(ManualReason::OutOfScope));
        outcome.needs_classification = true;
        return outcome;
    }
    // 教师明确标记仅人工
    if input.manual_only {
        return RouteOutcome::new("R11", RouteDecision::Manual(ManualReason::OutOfScope));
    }
    // R0：未作答且识别置信度 ≥ 下限 → 仅规则判不一致（0 分）
    if input.answer_empty
        && input
            .recognition_confidence
            .is_at_least(input.recognition_threshold)
    {
        return RouteOutcome::new("R0", RouteDecision::RuleOnly);
    }
    // R1：识别置信度不足或归一化失败 → 仅人工
    if input
        .recognition_confidence
        .is_at_least(input.recognition_threshold)
        && !input.normalization_failed
    {
        // 继续后续规则
    } else {
        return RouteOutcome::new(
            "R1",
            RouteDecision::Manual(ManualReason::RecognitionUnavailable),
        );
    }
    // R2：答案键缺失
    if input.key.is_missing() {
        return RouteOutcome::new("R2", RouteDecision::Manual(ManualReason::AnswerKeyMissing));
    }
    // R3：答案键为「略」且无已确认规则
    if input.key.is_explicitly_omitted() && !input.rubric_confirmed {
        return RouteOutcome::new("R3", RouteDecision::Manual(ManualReason::AnswerKeyOmitted));
    }
    // R4：仅评分标准，或「略」但有已确认规则
    if input.key.requires_rubric() || input.key.is_explicitly_omitted() {
        return model_route(input, "R4", RouteDecision::RubricItemized);
    }
    // 以下要求具体答案键
    if !input.key.has_concrete() {
        return RouteOutcome::new("R11", RouteDecision::Manual(ManualReason::OutOfScope));
    }
    // R5：单选/多选/判断 → 仅规则
    if matches!(
        input.kind,
        QuestionKind::SingleChoice | QuestionKind::MultipleChoice | QuestionKind::TrueFalse
    ) {
        return RouteOutcome::new("R5", RouteDecision::RuleOnly);
    }
    // R6：计算/证明 + 已确认分步规则 → 按评分标准逐项判定
    if input.rubric_confirmed
        && matches!(input.kind, QuestionKind::Calculation | QuestionKind::Proof)
    {
        return model_route(input, "R6", RouteDecision::RubricItemized);
    }
    // R7：填空/纯数值/计算 → 规则优先再升级模型
    if matches!(
        input.kind,
        QuestionKind::FillInBlank | QuestionKind::NumericOnly | QuestionKind::Calculation
    ) {
        return model_route(input, "R7", RouteDecision::RuleThenModel);
    }
    // R8：证明/简答/论述/其他 → 直接模型判定
    if matches!(
        input.kind,
        QuestionKind::Proof | QuestionKind::ShortAnswer | QuestionKind::Essay | QuestionKind::Other
    ) {
        return model_route(input, "R8", RouteDecision::ModelDirect);
    }
    // R11：兜底
    RouteOutcome::new("R11", RouteDecision::Manual(ManualReason::OutOfScope))
}

fn model_route(input: &RouteInput, rule_id: &'static str, decision: RouteDecision) -> RouteOutcome {
    // R10：需要模型但后端不可用 → 仅人工（记录可重试）
    if !input.backend_available {
        return RouteOutcome::new(
            "R10",
            RouteDecision::Manual(ManualReason::BackendUnavailable),
        );
    }
    RouteOutcome::new(rule_id, decision)
}

/// 路由是否允许在规则无法判定时升级模型。
#[must_use]
pub fn allows_model_escalation(decision: &RouteDecision) -> bool {
    matches!(
        decision,
        RouteDecision::RuleThenModel | RouteDecision::ModelDirect | RouteDecision::RubricItemized
    )
}

/// 路由是否需要模型判定。
#[must_use]
pub fn needs_model(decision: &RouteDecision) -> bool {
    matches!(
        decision,
        RouteDecision::RuleThenModel | RouteDecision::ModelDirect | RouteDecision::RubricItemized
    )
}

/// 手动路由到不可判定原因的映射（用于判分记录）。
#[must_use]
pub fn manual_to_ungradable(reason: ManualReason) -> UngradableReason {
    match reason {
        ManualReason::AnswerKeyMissing
        | ManualReason::AnswerKeyOmitted
        | ManualReason::RubricMissing => UngradableReason::AnswerKeyIncomplete,
        ManualReason::RecognitionUnavailable | ManualReason::LowConfidence => {
            UngradableReason::RecognitionUnavailable
        }
        ManualReason::OutOfScope => UngradableReason::OutOfScope,
        ManualReason::ConflictUnresolved => UngradableReason::Conflict,
        ManualReason::BackendUnavailable => UngradableReason::BackendUnavailable,
    }
}

/// 答案键与题型是否明显冲突（R9 边界）。
#[must_use]
pub fn key_kind_conflict(kind: QuestionKind, key: &AnswerKey) -> bool {
    let Some(concrete) = key.concrete() else {
        return false;
    };
    match kind {
        QuestionKind::SingleChoice | QuestionKind::MultipleChoice => {
            !matches!(concrete.primary, AnswerValue::Options(_))
        }
        QuestionKind::TrueFalse => !matches!(
            concrete.primary,
            AnswerValue::Text(_) | AnswerValue::Options(_)
        ),
        QuestionKind::NumericOnly => matches!(
            concrete.primary,
            AnswerValue::Text(_) | AnswerValue::Options(_)
        ),
        _ => false,
    }
}

/// 模式校验（快速模式不可用时直接拒绝进入）。
#[must_use]
pub fn mode_allowed(mode: GradeMode, fast_enabled: bool) -> bool {
    mode == GradeMode::Standard || fast_enabled
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    #![allow(clippy::items_after_statements)]

    use super::*;
    use eg_domain::{AnswerValue, ConcreteKey};

    fn input(kind: QuestionKind, key: AnswerKey) -> RouteInput {
        RouteInput {
            kind,
            kind_known: true,
            key,
            rubric_confirmed: false,
            answer_empty: false,
            recognition_confidence: Confidence::new(0.95).unwrap(),
            recognition_threshold: Confidence::new(0.75).unwrap(),
            normalization_failed: false,
            backend_available: true,
            key_kind_conflict: false,
            manual_only: false,
        }
    }

    fn concrete(value: AnswerValue) -> AnswerKey {
        AnswerKey::Concrete(ConcreteKey {
            primary: value,
            alternatives: Vec::new(),
            comparators: Vec::new(),
        })
    }

    #[test]
    fn route_table_values() {
        let numeric = || concrete(AnswerValue::Text("2.5".to_owned()));
        let choice = || concrete(AnswerValue::Options(vec!["A".to_owned()]));

        let mut route_input = input(QuestionKind::NumericOnly, numeric());
        route_input.answer_empty = true;
        assert_eq!(route(&route_input).rule_id, "R0");

        let mut route_input = input(QuestionKind::NumericOnly, numeric());
        route_input.recognition_confidence = Confidence::new(0.5).unwrap();
        assert_eq!(route(&route_input).rule_id, "R1");
        assert!(route(&route_input).decision.is_manual());

        let mut route_input = input(QuestionKind::NumericOnly, numeric());
        route_input.normalization_failed = true;
        assert_eq!(route(&route_input).rule_id, "R1");

        assert_eq!(
            route(&input(QuestionKind::NumericOnly, AnswerKey::Missing)).rule_id,
            "R2"
        );

        let omitted = AnswerKey::ExplicitlyOmitted {
            note: Some("略".to_owned()),
        };
        assert_eq!(
            route(&input(QuestionKind::Calculation, omitted.clone())).rule_id,
            "R3"
        );

        assert_eq!(
            route(&input(QuestionKind::Calculation, AnswerKey::RubricOnly)).decision,
            RouteDecision::RubricItemized
        );

        assert_eq!(
            route(&input(QuestionKind::SingleChoice, choice())).decision,
            RouteDecision::RuleOnly
        );
        assert_eq!(
            route(&input(QuestionKind::MultipleChoice, choice())).decision,
            RouteDecision::RuleOnly
        );
        assert_eq!(
            route(&input(
                QuestionKind::TrueFalse,
                concrete(AnswerValue::Text("对".to_owned()))
            ))
            .decision,
            RouteDecision::RuleOnly
        );

        let mut route_input = input(QuestionKind::Calculation, numeric());
        route_input.rubric_confirmed = true;
        assert_eq!(route(&route_input).rule_id, "R6");
        assert_eq!(route(&route_input).decision, RouteDecision::RubricItemized);

        assert_eq!(
            route(&input(QuestionKind::FillInBlank, numeric())).decision,
            RouteDecision::RuleThenModel
        );
        assert_eq!(
            route(&input(QuestionKind::Calculation, numeric())).decision,
            RouteDecision::RuleThenModel
        );

        assert_eq!(
            route(&input(
                QuestionKind::ShortAnswer,
                concrete(AnswerValue::Text("x".to_owned()))
            ))
            .decision,
            RouteDecision::ModelDirect
        );

        let mut route_input = input(QuestionKind::FillInBlank, numeric());
        route_input.backend_available = false;
        assert_eq!(route(&route_input).rule_id, "R10");
        assert!(route(&route_input).decision.is_manual());

        let mut route_input = input(QuestionKind::FillInBlank, numeric());
        route_input.kind_known = false;
        let outcome = route(&route_input);
        assert_eq!(outcome.rule_id, "R9");
        assert!(outcome.needs_classification);
    }

    #[test]
    fn key_kind_conflicts_are_detected() {
        assert!(key_kind_conflict(
            QuestionKind::SingleChoice,
            &concrete(AnswerValue::Text("x".to_owned()))
        ));
        assert!(key_kind_conflict(
            QuestionKind::NumericOnly,
            &concrete(AnswerValue::Text("x".to_owned()))
        ));
        assert!(key_kind_conflict(
            QuestionKind::NumericOnly,
            &concrete(AnswerValue::Text("2.5".to_owned()))
        ));
        assert!(!key_kind_conflict(
            QuestionKind::MultipleChoice,
            &concrete(AnswerValue::Options(vec!["A".to_owned()]))
        ));
    }
}
