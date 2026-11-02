//! 推理层错误与失败分类（`SPEC-inference.md` §5「一律显式失败」）。
//!
//! 分类决定三件事：是否计入熔断、是否可重试、能否进入自动流程。
//! 永久错误（4xx 参数错、Schema 不合法、许可拒绝）**不计入熔断也不重试**；
//! 瞬态错误（429、5xx、超时、连接中断）才计入熔断并允许有限重试。
//!
//! 边界：本模块只做错误建模，不做重试/降级决策（那是 `service` 与调用方的事）。

use core::fmt;

/// 失败分类（`SPEC-inference.md` §4/§5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailureClass {
    /// 瞬态：限流、5xx、超时、连接中断。计入熔断，可有限重试。
    Transient,
    /// 永久：请求参数错误、模型拒绝、不可恢复的 4xx。不计入熔断，不重试。
    Permanent,
    /// 策略拦截：云端开关、任务授权、外发范围。0 次外联，转人工/设置页。
    Policy,
    /// 结构不合法：模型输出未通过 Schema 校验。不重试（修复次数归调用方）。
    Schema,
    /// 预算超限：停止该任务后续云端调用。
    Budget,
    /// 夹具未命中：绝不合成输出。
    Fixture,
    /// 许可门禁：加载前拒绝。
    License,
    /// 注册表/配置错误：启动即失败。
    Registry,
    /// 取消：调用方 `CancelToken` 已触发。
    Cancelled,
    /// 内部错误：序列化、传输实现缺失等。
    Internal,
}

impl FailureClass {
    /// 是否计入熔断器（只有瞬态计入）。
    #[must_use]
    pub const fn counts_toward_breaker(self) -> bool {
        matches!(self, Self::Transient)
    }

    /// 是否允许有限重试（只有瞬态允许）。
    #[must_use]
    pub const fn retryable(self) -> bool {
        matches!(self, Self::Transient)
    }

    /// 中文标签（日志与界面用）。
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Transient => "瞬态失败",
            Self::Permanent => "永久失败",
            Self::Policy => "策略拦截",
            Self::Schema => "结构不合法",
            Self::Budget => "预算超限",
            Self::Fixture => "夹具未命中",
            Self::License => "许可门禁",
            Self::Registry => "注册表错误",
            Self::Cancelled => "已取消",
            Self::Internal => "内部错误",
        }
    }
}

/// 推理层错误。
///
/// 所有变体都携带足够的中文上下文，便于直接写日志与界面提示；
/// 不包含图像字节、识别全文、学生姓名/学号或密钥（`SPEC-inference.md` §6）。
#[derive(Debug, thiserror::Error)]
pub enum InferenceError {
    /// 模型注册表无法解析或字段不合法。
    #[error("模型注册表无效：{reason}")]
    RegistryInvalid {
        /// 具体原因（中文）。
        reason: String,
    },
    /// 配置指向的模型不在注册表内（启动即失败，不静默回退）。
    #[error("模型不在注册表内：{model_id}")]
    RegistryMiss {
        /// 请求的模型 id。
        model_id: String,
    },
    /// 许可不在白名单内（fail-closed）。
    #[error("许可门禁不通过：模型 {model_id} 的许可 {license} 不在允许清单内")]
    LicenseDenied {
        /// 模型 id。
        model_id: String,
        /// 许可标识。
        license: String,
    },
    /// 命中排除清单且缺人工评审引用。
    #[error("许可门禁不通过：模型 {model_id} 的许可 {license} 在排除清单内，且未提供人工评审引用")]
    LicenseReviewRequired {
        /// 模型 id。
        model_id: String,
        /// 许可标识。
        license: String,
    },
    /// 上下文超限（显式拒绝，禁止静默截断）。
    #[error("上下文超限：需要 {needed} token，上限 {limit} token")]
    ContextOverflow {
        /// 需要量。
        needed: u32,
        /// 上限。
        limit: u32,
    },
    /// 后端不可用（sidecar 未就绪或已退出）。
    #[error("后端不可用：{detail}")]
    BackendUnavailable {
        /// 说明。
        detail: String,
    },
    /// 单次调用超时。
    #[error("{stage} 超时：{timeout_ms} 毫秒内未完成")]
    Timeout {
        /// 阶段（`启动探活`、`推理` 等）。
        stage: String,
        /// 超时阈值（毫秒）。
        timeout_ms: u64,
    },
    /// 瞬态失败（计入熔断）。
    #[error("瞬态失败（HTTP {status}）：{detail}")]
    Transient {
        /// HTTP 状态码；非 HTTP 场景用 0。
        status: u16,
        /// 说明。
        detail: String,
    },
    /// 永久失败（不计入熔断）。
    #[error("永久失败（HTTP {status}）：{detail}")]
    Permanent {
        /// HTTP 状态码；非 HTTP 场景用 0。
        status: u16,
        /// 说明。
        detail: String,
    },
    /// 传输层不支持或不可用。
    #[error("传输失败：{detail}")]
    Transport {
        /// 说明。
        detail: String,
    },
    /// 传输实现不支持该目标（例如内置传输不提供 TLS）。
    #[error("传输不支持：{detail}")]
    TransportUnsupported {
        /// 说明。
        detail: String,
    },
    /// 模型输出未通过严格 Schema 校验。
    #[error("模型输出不符合 Schema：{reason}")]
    SchemaViolation {
        /// 原因（中文）。
        reason: String,
    },
    /// 任务预算超限。
    #[error("预算超限：任务已用 {spent_micros}，上限 {limit_micros}，本次需要 {needed_micros}")]
    BudgetExceeded {
        /// 已用（微元）。
        spent_micros: u64,
        /// 上限（微元）。
        limit_micros: u64,
        /// 本次预计花费（微元）。
        needed_micros: u64,
    },
    /// 云端总开关关闭（申请即拒绝，0 次外联）。
    #[error("云端总开关关闭：请在设置页开启后重新授权")]
    CloudDisabled,
    /// 无有效任务授权（0 次外联）。
    #[error("任务没有有效的云端授权：{detail}")]
    ConsentMissing {
        /// 说明。
        detail: String,
    },
    /// 授权状态已不可用。
    #[error("授权 {consent_id} 状态为 {state}，已不可用于调用")]
    ConsentInactive {
        /// 授权 id。
        consent_id: String,
        /// 状态。
        state: String,
    },
    /// 请求内容超出授权范围（硬拒绝）。
    #[error("请求超出授权范围：范围哈希不匹配，已硬拒绝")]
    ScopeMismatch,
    /// 授权预算不足。
    #[error("授权预算不足：授权上限 {limit_micros}，已用 {spent_micros}")]
    ConsentBudgetExhausted {
        /// 上限。
        limit_micros: u64,
        /// 已用。
        spent_micros: u64,
    },
    /// 熔断打开。
    #[error("熔断打开：{retry_after_ms} 毫秒后才允许探测")]
    CircuitOpen {
        /// 剩余等待（毫秒）。
        retry_after_ms: u64,
    },
    /// 夹具未命中（绝不合成输出）。
    #[error("夹具未命中：{key}")]
    FixtureMiss {
        /// 输入哈希键。
        key: String,
    },
    /// 调用方已取消。
    #[error("调用已取消")]
    Cancelled,
    /// 平台适配层错误。
    #[error("平台错误：{detail}")]
    Platform {
        /// 说明。
        detail: String,
    },
    /// 必需配置键未解析。
    #[error("配置项未解析：{key}")]
    ConfigUnresolved {
        /// 配置键。
        key: String,
    },
    /// 内部错误。
    #[error("内部错误：{detail}")]
    Internal {
        /// 说明。
        detail: String,
    },
}

impl InferenceError {
    /// 失败分类。
    #[must_use]
    pub const fn class(&self) -> FailureClass {
        match self {
            Self::Transient { .. } | Self::Timeout { .. } | Self::Transport { .. } => {
                FailureClass::Transient
            }
            Self::Permanent { .. }
            | Self::ContextOverflow { .. }
            | Self::TransportUnsupported { .. } => FailureClass::Permanent,
            Self::SchemaViolation { .. } => FailureClass::Schema,
            Self::CloudDisabled
            | Self::ConsentMissing { .. }
            | Self::ConsentInactive { .. }
            | Self::ScopeMismatch
            | Self::ConsentBudgetExhausted { .. } => FailureClass::Policy,
            Self::BudgetExceeded { .. } => FailureClass::Budget,
            Self::FixtureMiss { .. } => FailureClass::Fixture,
            Self::LicenseDenied { .. } | Self::LicenseReviewRequired { .. } => {
                FailureClass::License
            }
            Self::RegistryInvalid { .. }
            | Self::RegistryMiss { .. }
            | Self::ConfigUnresolved { .. } => FailureClass::Registry,
            Self::CircuitOpen { .. } => FailureClass::Transient,
            Self::Cancelled => FailureClass::Cancelled,
            Self::BackendUnavailable { .. } | Self::Platform { .. } | Self::Internal { .. } => {
                FailureClass::Internal
            }
        }
    }

    /// 是否计入熔断。
    #[must_use]
    pub const fn counts_toward_breaker(&self) -> bool {
        self.class().counts_toward_breaker()
    }

    /// 是否允许有限重试。
    #[must_use]
    pub const fn retryable(&self) -> bool {
        self.class().retryable()
    }

    /// 中文分类标签。
    #[must_use]
    pub const fn class_label(&self) -> &'static str {
        self.class().label()
    }
}

impl fmt::Display for FailureClass {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn transient_classes_count_toward_breaker() {
        let cases = [
            InferenceError::Transient {
                status: 429,
                detail: "限流".to_owned(),
            },
            InferenceError::Timeout {
                stage: "推理".to_owned(),
                timeout_ms: 20_000,
            },
            InferenceError::Transport {
                detail: "连接重置".to_owned(),
            },
        ];
        for error in cases {
            assert_eq!(error.class(), FailureClass::Transient);
            assert!(error.counts_toward_breaker());
            assert!(error.retryable());
        }
    }

    #[test]
    fn permanent_and_policy_classes_do_not_count() {
        let cases = [
            (
                InferenceError::Permanent {
                    status: 400,
                    detail: "参数错".to_owned(),
                },
                FailureClass::Permanent,
            ),
            (InferenceError::CloudDisabled, FailureClass::Policy),
            (InferenceError::ScopeMismatch, FailureClass::Policy),
            (
                InferenceError::SchemaViolation {
                    reason: "未知字段".to_owned(),
                },
                FailureClass::Schema,
            ),
            (
                InferenceError::FixtureMiss {
                    key: "abc".to_owned(),
                },
                FailureClass::Fixture,
            ),
            (
                InferenceError::LicenseDenied {
                    model_id: "m".to_owned(),
                    license: "CC-BY-NC-4.0".to_owned(),
                },
                FailureClass::License,
            ),
        ];
        for (error, class) in cases {
            assert_eq!(error.class(), class);
            assert!(!error.counts_toward_breaker());
            assert!(!error.retryable());
        }
    }

    #[test]
    fn context_overflow_is_explicit_and_permanent() {
        let error = InferenceError::ContextOverflow {
            needed: 9_000,
            limit: 8_192,
        };
        assert_eq!(error.class(), FailureClass::Permanent);
        assert!(error.to_string().contains("9000"));
    }
}
