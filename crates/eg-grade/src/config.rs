//! 判分配置与模式阈值（`CONFIG.md` §5/§6；`SPEC-grade.md` §3/§7）。
//!
//! 与平台配置的适配（TODO）：`eg-platform` 的 `GradeConfig`/`FastConfig` 是持久化视图；
//! 本 crate 保持分层独立，提供同构的运行时配置 + 默认值（与 `CONFIG.md` 一致），
//! 由 `app`/`grade` 注入时做一次映射（映射属后续接线任务，避免本 crate 依赖 platform）。
//! 修改阈值/模式/抽检比例属 Ask first（规范 §12）。

use eg_domain::{Confidence, GradeMode, MilliScore, RouteDecision, Sha256Hex};

/// 识别置信度下限（`SPEC-grade.md` §3；CONFIG 未单列键，按规范默认 0.75）。
pub const RECOGNITION_THRESHOLD: f64 = 0.75;

/// 冲突策略（`grade.conflict_policy`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConflictPolicy {
    /// 升级人工（默认）。
    #[default]
    EscalateManual,
    /// 规则优先（仅用于「规则相等 vs 模型不一致」，且快速模式可自动但标记）。
    RuleFirst,
}

/// 单模式四类路由阈值（`SPEC-grade.md` §3 表）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModeThresholds {
    /// 仅规则。
    pub rule: Confidence,
    /// 规则优先再升级模型。
    pub rule_then_model: Confidence,
    /// 直接模型判定。
    pub model: Confidence,
    /// 按评分标准逐项判定。
    pub rubric: Confidence,
}

impl ModeThresholds {
    /// 标准模式默认阈值（CONFIG §5）。
    #[must_use]
    pub fn standard() -> Self {
        Self {
            rule: confidence(0.85),
            rule_then_model: confidence(0.90),
            model: confidence(0.85),
            rubric: confidence(0.80),
        }
    }

    /// 快速模式默认阈值：`grade.fast.rule_threshold=0.80`，其余三类在标准基础上下调 0.10。
    #[must_use]
    pub fn fast(standard: Self) -> Self {
        Self {
            rule: confidence(0.80),
            rule_then_model: lower(standard.rule_then_model, 0.10),
            model: lower(standard.model, 0.10),
            rubric: lower(standard.rubric, 0.10),
        }
    }
}

fn confidence(value: f64) -> Confidence {
    // 常量均在 [0,1]，构造失败只可能是编码错误；返回 0 保守值而非 panic。
    Confidence::new(value).unwrap_or(Confidence::ZERO)
}

fn lower(base: Confidence, delta: f64) -> Confidence {
    let value = (base.get() - delta).max(0.0);
    confidence(value)
}

/// 判分运行时配置（对应 `grade.*` 与 `fast.*`）。
#[derive(Debug, Clone, PartialEq)]
pub struct GradeConfig {
    /// 识别置信度下限（R0/R1 使用）。
    pub recognition_threshold: Confidence,
    /// 分数粒度（默认 0.1 分 = 100 千分之一分）。
    pub score_granularity: MilliScore,
    /// 模型输出修复重试次数（默认 1）。
    pub max_repair_attempts: u32,
    /// 单题判定超时秒数（默认 20；由 inference 执行，本 crate 仅记录）。
    pub judge_timeout_s: u32,
    /// 标准模式是否允许未校准模型自动放行（默认关）。
    pub allow_uncalibrated_auto: bool,
    /// 冲突策略（默认升级人工）。
    pub conflict_policy: ConflictPolicy,
    /// 标准模式阈值。
    pub standard: ModeThresholds,
    /// 快速模式阈值。
    pub fast: ModeThresholds,
    /// 是否允许快速模式（`fast.enabled`）。
    pub fast_enabled: bool,
    /// 抽检比例（默认 0.20，范围 0.05–1.00）。
    pub sampling_rate: f64,
    /// 低置信度权重（默认 0.70）。
    pub low_confidence_weight: f64,
    /// 抽样种子；`None` 表示按提交 id 派生并记录（可复现）。
    pub sampling_seed: Option<u64>,
    /// 理由长度上限（字），默认 500（规范要求「不超长」，CONFIG 未单列）。
    pub max_reason_chars: usize,
}

impl Default for GradeConfig {
    fn default() -> Self {
        let standard = ModeThresholds::standard();
        Self {
            recognition_threshold: confidence(RECOGNITION_THRESHOLD),
            score_granularity: MilliScore::from_milli(100).unwrap_or(MilliScore::ZERO),
            max_repair_attempts: 1,
            judge_timeout_s: 20,
            allow_uncalibrated_auto: false,
            conflict_policy: ConflictPolicy::EscalateManual,
            standard,
            fast: ModeThresholds::fast(standard),
            fast_enabled: true,
            sampling_rate: 0.20,
            low_confidence_weight: 0.70,
            sampling_seed: None,
            max_reason_chars: 500,
        }
    }
}

/// 配置错误。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ConfigError {
    /// 阈值越界。
    #[error("阈值 {field}={value} 超出允许范围 0.5–0.99")]
    ThresholdOutOfRange {
        /// 字段名。
        field: &'static str,
        /// 实际值。
        value: f64,
    },
    /// 抽检比例越界。
    #[error("抽检比例 {0} 超出 0.05–1.00")]
    SamplingRateOutOfRange(f64),
    /// 低置信权重越界。
    #[error("低置信度权重 {0} 超出 0–1")]
    LowConfidenceWeightOutOfRange(f64),
    /// 分数粒度为 0。
    #[error("分数粒度必须大于 0")]
    ZeroGranularity,
    /// 修复次数越界。
    #[error("模型输出修复次数 {0} 超出 0–2")]
    RepairAttemptsOutOfRange(u32),
    /// 理由长度上限为 0。
    #[error("理由长度上限必须大于 0")]
    ZeroReasonLimit,
}

impl GradeConfig {
    /// 校验配置（范围与 CONFIG.md 一致）。
    ///
    /// # Errors
    ///
    /// 任一字段越界返回 [`ConfigError`]。
    pub fn validate(&self) -> Result<(), ConfigError> {
        for (field, value) in [
            ("grade.standard.rule_threshold", self.standard.rule.get()),
            (
                "grade.standard.rule_then_model_threshold",
                self.standard.rule_then_model.get(),
            ),
            ("grade.standard.model_threshold", self.standard.model.get()),
            (
                "grade.standard.rubric_threshold",
                self.standard.rubric.get(),
            ),
            ("grade.fast.rule_threshold", self.fast.rule.get()),
            (
                "grade.fast.rule_then_model_threshold",
                self.fast.rule_then_model.get(),
            ),
            ("grade.fast.model_threshold", self.fast.model.get()),
            ("grade.fast.rubric_threshold", self.fast.rubric.get()),
        ] {
            if !(0.5..=0.99).contains(&value) {
                return Err(ConfigError::ThresholdOutOfRange { field, value });
            }
        }
        if !(0.05..=1.0).contains(&self.sampling_rate) {
            return Err(ConfigError::SamplingRateOutOfRange(self.sampling_rate));
        }
        if !(0.0..=1.0).contains(&self.low_confidence_weight) {
            return Err(ConfigError::LowConfidenceWeightOutOfRange(
                self.low_confidence_weight,
            ));
        }
        if self.score_granularity.is_zero() {
            return Err(ConfigError::ZeroGranularity);
        }
        if self.max_repair_attempts > 2 {
            return Err(ConfigError::RepairAttemptsOutOfRange(
                self.max_repair_attempts,
            ));
        }
        if self.max_reason_chars == 0 {
            return Err(ConfigError::ZeroReasonLimit);
        }
        Ok(())
    }

    /// 路由对应的自动接受阈值。
    #[must_use]
    pub fn threshold(&self, route: &RouteDecision, mode: GradeMode) -> Confidence {
        let thresholds = match mode {
            GradeMode::Standard => self.standard,
            GradeMode::Fast => self.fast,
        };
        match route {
            RouteDecision::RuleOnly => thresholds.rule,
            RouteDecision::RuleThenModel => thresholds.rule_then_model,
            RouteDecision::ModelDirect => thresholds.model,
            RouteDecision::RubricItemized => thresholds.rubric,
            RouteDecision::Manual(_) => Confidence::ONE,
        }
    }

    /// 配置指纹（进幂等键/引擎指纹；字段顺序固定）。
    #[must_use]
    pub fn hash(&self) -> Sha256Hex {
        let text = format!(
            "recognition={};granularity={};repair={};timeout={};allow_uncalibrated={:?};\
             conflict={:?};standard={:?};fast={:?};fast_enabled={};rate={};low_weight={};\
             seed={:?};reason_limit={}",
            self.recognition_threshold.get(),
            self.score_granularity.as_milli(),
            self.max_repair_attempts,
            self.judge_timeout_s,
            self.allow_uncalibrated_auto,
            self.conflict_policy,
            self.standard,
            self.fast,
            self.fast_enabled,
            self.sampling_rate,
            self.low_confidence_weight,
            self.sampling_seed,
            self.max_reason_chars
        );
        Sha256Hex::of(text.as_bytes())
    }
}

/// 供测试/展示的阈值差（避免外部依赖 platform 类型）。
#[must_use]
pub fn is_fast_rule_threshold(config: &GradeConfig) -> bool {
    config.fast.rule.get() < config.standard.rule.get()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use eg_domain::ManualReason;

    #[test]
    fn defaults_match_config_md() {
        let config = GradeConfig::default();
        config.validate().unwrap();
        assert_eq!(config.sampling_rate, 0.20);
        assert_eq!(config.low_confidence_weight, 0.70);
        assert!((config.standard.rule.get() - 0.85).abs() < f64::EPSILON);
        assert!((config.fast.rule.get() - 0.80).abs() < f64::EPSILON);
        assert!((config.fast.model.get() - 0.75).abs() < f64::EPSILON);
        assert!((config.fast.rubric.get() - 0.70).abs() < f64::EPSILON);
        assert!(is_fast_rule_threshold(&config));
    }

    #[test]
    fn thresholds_per_route_and_mode() {
        let config = GradeConfig::default();
        assert!(
            config.threshold(&RouteDecision::RuleOnly, GradeMode::Standard)
                > config.threshold(&RouteDecision::RuleOnly, GradeMode::Fast)
        );
        assert!(
            config.threshold(&RouteDecision::RuleThenModel, GradeMode::Standard)
                > config.threshold(&RouteDecision::RuleThenModel, GradeMode::Fast)
        );
        assert!(
            config.threshold(
                &RouteDecision::Manual(ManualReason::OutOfScope),
                GradeMode::Fast
            ) > config.threshold(&RouteDecision::RuleOnly, GradeMode::Standard)
        );
    }

    #[test]
    fn invalid_config_is_rejected() {
        let bad_rate = GradeConfig {
            sampling_rate: 0.01,
            ..GradeConfig::default()
        };
        assert!(bad_rate.validate().is_err());
        let mut config = GradeConfig::default();
        config.standard.rule = Confidence::new(0.4).unwrap();
        assert!(config.validate().is_err());
    }
}
