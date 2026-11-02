//! 门禁与置信度（`SPEC-grade.md` §6/§7、T18）。
//!
//! 自动接受条件（全部满足）：路由不是仅人工；置信度 ≥ 当前模式阈值；
//! 无冲突/不可判定/Schema 修复失败；证据链完整；得分合法；未校准模型按模式限制。
//! 否则待复核并携带结构化原因（供复核队列排序/筛选）。

use eg_domain::{Confidence, GradeMode, Judgement, ReviewReasonKind, RouteDecision};

use crate::config::GradeConfig;
use crate::validate::ConflictKind;

/// 门禁输入。
#[derive(Debug, Clone)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "门禁条件即规范 §7 的独立布尔事实（是否模型/校准/证据/规则结论），拆结构会降低可读性"
)]
pub struct GateInput<'a> {
    /// 路由决策。
    pub route: &'a RouteDecision,
    /// 组合后的判定置信度。
    pub confidence: Confidence,
    /// 运行模式。
    pub mode: GradeMode,
    /// 判定。
    pub judgement: &'a Judgement,
    /// 是否使用模型判定。
    pub used_model: bool,
    /// 模型是否已校准。
    pub calibrated: bool,
    /// 是否含模型调用证据（`ModelCall`）。
    pub has_model_evidence: bool,
    /// 是否含模型证据块（证据块最低置信度存在）。
    pub has_evidence_block: bool,
    /// 规则段是否已作出确定性结论（R0/比较器）。
    pub rule_evidence_required: bool,
    /// 规则段是否有比较轨迹。
    pub has_rule_evidence: bool,
    /// 冲突列表。
    pub conflicts: &'a [ConflictKind],
    /// 配置。
    pub config: &'a GradeConfig,
}

/// 门禁结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateOutcome {
    /// 自动接受（`force_sampled` 为真时即使快速模式也强制抽检）。
    AutoAccept {
        /// 是否强制抽检（未校准模型在快速模式放行）。
        force_sampled: bool,
    },
    /// 待复核（结构化原因，已去重）。
    NeedsReview {
        /// 阻塞原因。
        blockers: Vec<ReviewReasonKind>,
    },
}

impl GateOutcome {
    /// 是否自动接受。
    #[must_use]
    pub fn is_auto_accept(&self) -> bool {
        matches!(self, Self::AutoAccept { .. })
    }

    /// 是否强制抽检。
    #[must_use]
    pub fn force_sampled(&self) -> bool {
        matches!(
            self,
            Self::AutoAccept {
                force_sampled: true
            }
        )
    }

    /// 阻塞原因。
    #[must_use]
    pub fn blockers(&self) -> Vec<ReviewReasonKind> {
        match self {
            Self::NeedsReview { blockers } => blockers.clone(),
            Self::AutoAccept { .. } => Vec::new(),
        }
    }
}

fn push_unique(blockers: &mut Vec<ReviewReasonKind>, reason: ReviewReasonKind) {
    if !blockers.contains(&reason) {
        blockers.push(reason);
    }
}

/// 执行门禁判定。
#[must_use]
pub fn gate(input: &GateInput<'_>) -> GateOutcome {
    let mut blockers: Vec<ReviewReasonKind> = Vec::new();
    let mut force_sampled = false;
    if input.route.is_manual() {
        push_unique(&mut blockers, ReviewReasonKind::Ungradable);
    }
    let threshold = input.config.threshold(input.route, input.mode);
    if !input.confidence.is_at_least(threshold) {
        push_unique(&mut blockers, ReviewReasonKind::LowConfidence);
    }
    if let Judgement::Ungradable { reason } = input.judgement {
        push_unique(&mut blockers, reason.review_reason());
    }
    // 冲突仲裁（§8）
    if !input.conflicts.is_empty() {
        let only_rule_first_case = input
            .conflicts
            .iter()
            .all(|conflict| matches!(conflict, ConflictKind::RuleMatchModelNotMatch));
        let rule_first_fast = only_rule_first_case
            && input.config.conflict_policy == crate::config::ConflictPolicy::RuleFirst
            && input.mode == GradeMode::Fast
            && !matches!(input.judgement, Judgement::Ungradable { .. });
        if rule_first_fast {
            force_sampled = true;
        } else {
            push_unique(&mut blockers, ReviewReasonKind::Conflict);
        }
    }
    // 证据链
    if input.rule_evidence_required && !input.has_rule_evidence {
        push_unique(&mut blockers, ReviewReasonKind::MissingEvidence);
    }
    if input.used_model {
        if !input.has_model_evidence || !input.has_evidence_block {
            push_unique(&mut blockers, ReviewReasonKind::MissingEvidence);
        }
        if !input.calibrated {
            match input.mode {
                GradeMode::Standard => {
                    if !input.config.allow_uncalibrated_auto {
                        push_unique(&mut blockers, ReviewReasonKind::UncalibratedModel);
                    }
                }
                GradeMode::Fast => {
                    // 快速模式允许未校准放行，但需 ≥0.90 并强制抽检
                    if input.confidence.get() >= 0.90 {
                        force_sampled = true;
                    } else {
                        push_unique(&mut blockers, ReviewReasonKind::UncalibratedModel);
                    }
                }
            }
        }
    }
    if blockers.is_empty() {
        GateOutcome::AutoAccept { force_sampled }
    } else {
        GateOutcome::NeedsReview { blockers }
    }
}

/// 组合判定置信度（`SPEC-grade.md` §6）。
///
/// 规则段取识别置信度；模型段取「模型原始置信度、识别置信度、证据块最低置信度」
/// 三者最小值；证据块为空时上限 0.5。
#[must_use]
pub fn compose_confidence(
    recognition: Confidence,
    model_confidence: Option<Confidence>,
    evidence_confidence: Option<Confidence>,
) -> Confidence {
    let Some(model_confidence) = model_confidence else {
        return recognition;
    };
    let mut confidence = model_confidence.min(recognition);
    if let Some(evidence) = evidence_confidence {
        confidence = confidence.min(evidence);
    } else {
        confidence = confidence.min(Confidence::new(0.5).unwrap_or(Confidence::ZERO));
    }
    confidence
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::config::GradeConfig;
    use eg_domain::{ManualReason, MilliScore, UngradableReason};

    fn match_judgement(max: i64) -> Judgement {
        Judgement::Match {
            score: MilliScore::from_milli(max).unwrap(),
            reason: "一致".to_owned(),
            evidence: Vec::new(),
        }
    }

    fn input<'a>(
        route: &'a RouteDecision,
        judgement: &'a Judgement,
        confidence: Confidence,
        mode: GradeMode,
        calibrated: bool,
        conflicts: &'a [ConflictKind],
        config: &'a GradeConfig,
    ) -> GateInput<'a> {
        GateInput {
            route,
            confidence,
            mode,
            judgement,
            used_model: false,
            calibrated,
            has_model_evidence: false,
            has_evidence_block: false,
            rule_evidence_required: true,
            has_rule_evidence: true,
            conflicts,
            config,
        }
    }

    #[test]
    fn threshold_and_confidence_gate() {
        let config = GradeConfig::default();
        let route = RouteDecision::RuleOnly;
        let judgement = match_judgement(2000);
        let high = Confidence::new(0.90).unwrap();
        let low = Confidence::new(0.70).unwrap();
        assert!(
            gate(&input(
                &route,
                &judgement,
                high,
                GradeMode::Standard,
                true,
                &[],
                &config
            ))
            .is_auto_accept()
        );
        assert!(
            gate(&input(
                &route,
                &judgement,
                low,
                GradeMode::Standard,
                true,
                &[],
                &config
            ))
            .blockers()
            .contains(&ReviewReasonKind::LowConfidence)
        );
        // 快速模式低阈值放行
        assert!(
            gate(&input(
                &route,
                &judgement,
                Confidence::new(0.82).unwrap(),
                GradeMode::Fast,
                true,
                &[],
                &config
            ))
            .is_auto_accept()
        );
    }

    #[test]
    fn manual_and_ungradable_never_auto_accept() {
        let config = GradeConfig::default();
        let route = RouteDecision::Manual(ManualReason::AnswerKeyMissing);
        let judgement = match_judgement(2000);
        let outcome = gate(&input(
            &route,
            &judgement,
            Confidence::ONE,
            GradeMode::Standard,
            true,
            &[],
            &config,
        ));
        assert!(!outcome.is_auto_accept());
        let ungradable = Judgement::Ungradable {
            reason: UngradableReason::OutOfScope,
        };
        let route = RouteDecision::RuleOnly;
        let outcome = gate(&input(
            &route,
            &ungradable,
            Confidence::ONE,
            GradeMode::Standard,
            true,
            &[],
            &config,
        ));
        assert!(outcome.blockers().contains(&ReviewReasonKind::Ungradable));
    }

    #[test]
    fn conflicts_follow_policy() {
        let mut config = GradeConfig::default();
        let route = RouteDecision::RuleThenModel;
        let judgement = match_judgement(2000);
        let conflicts = vec![ConflictKind::RuleMatchModelNotMatch];
        let outcome = gate(&input(
            &route,
            &judgement,
            Confidence::ONE,
            GradeMode::Standard,
            true,
            &conflicts,
            &config,
        ));
        assert!(outcome.blockers().contains(&ReviewReasonKind::Conflict));
        config.conflict_policy = crate::config::ConflictPolicy::RuleFirst;
        let outcome = gate(&input(
            &route,
            &judgement,
            Confidence::ONE,
            GradeMode::Fast,
            true,
            &conflicts,
            &config,
        ));
        assert!(outcome.is_auto_accept());
        assert!(outcome.force_sampled());
        // 聚合冲突始终进人工
        let aggregate = vec![ConflictKind::AggregateMismatch {
            declared: MilliScore::from_milli(1000).unwrap(),
            aggregated: MilliScore::from_milli(2000).unwrap(),
        }];
        let outcome = gate(&input(
            &route,
            &judgement,
            Confidence::ONE,
            GradeMode::Fast,
            true,
            &aggregate,
            &config,
        ));
        assert!(outcome.blockers().contains(&ReviewReasonKind::Conflict));
    }

    #[test]
    fn uncalibrated_model_rules() {
        let config = GradeConfig::default();
        let route = RouteDecision::ModelDirect;
        let judgement = match_judgement(2000);
        let mut input = input(
            &route,
            &judgement,
            Confidence::new(0.95).unwrap(),
            GradeMode::Standard,
            false,
            &[],
            &config,
        );
        input.used_model = true;
        input.has_model_evidence = true;
        input.has_evidence_block = true;
        assert!(input.config.allow_uncalibrated_auto.eq(&false));
        let outcome = gate(&input);
        assert!(
            outcome
                .blockers()
                .contains(&ReviewReasonKind::UncalibratedModel)
        );
        // 快速模式 ≥0.90 放行但强制抽检
        input.mode = GradeMode::Fast;
        let outcome = gate(&input);
        assert!(outcome.is_auto_accept());
        assert!(outcome.force_sampled());
    }

    #[test]
    fn confidence_composition_uses_minimum_and_cap() {
        let recognition = Confidence::new(0.9).unwrap();
        let model = Confidence::new(0.8).unwrap();
        let evidence = Confidence::new(0.7).unwrap();
        assert_eq!(
            compose_confidence(recognition, Some(model), Some(evidence)).get(),
            0.7
        );
        assert_eq!(
            compose_confidence(recognition, Some(model), None).get(),
            0.5
        );
        assert_eq!(compose_confidence(recognition, None, None).get(), 0.9);
    }
}
