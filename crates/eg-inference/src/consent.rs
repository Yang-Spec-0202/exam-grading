//! 任务级授权（`SPEC-inference.md` §2.3/§9.6）：五维校验，**无授权 0 次外联**。
//!
//! 五个维度（任一不满足即拒绝，请求不会发出）：
//! 1. **任务**：授权必须属于当前 `task_id`；
//! 2. **后端**：授权只覆盖 `openai_compat`；
//! 3. **范围**：请求外发内容的 `scope_hash` 必须与授权一致（不做「裁剪后尽量发」）；
//! 4. **预算**：授权额度未用尽（由 [`crate::budget::BudgetLedger`] 记账，状态由服务置为用尽）；
//! 5. **状态**：授权处于生效中且未结束/撤销/过期。
//!
//! 生命周期：`app` 发起 → `platform` 登记（`ConsentStore`）→ 每次云端调用前校验 →
//! 任务到终态由 `platform` 结束；续跑必须重新授权。

use std::collections::BTreeMap;
use std::sync::Mutex;

use eg_domain::{BackendId, Clock, ConsentId, Sha256Hex, TaskId, Timestamp};

use crate::error::InferenceError;

/// 授权状态（与 `eg_platform::consent::ConsentState` 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConsentState {
    /// 已登记/生效中。
    Active,
    /// 预算用尽。
    Exhausted,
    /// 已撤销。
    Revoked,
    /// 已过期（任务终态之外的显式过期）。
    Expired,
    /// 任务结束而结束。
    Ended,
}

impl ConsentState {
    /// 稳定文本编码（与 platform 的持久化编码一致）。
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Exhausted => "exhausted",
            Self::Revoked => "revoked",
            Self::Expired => "expired",
            Self::Ended => "ended",
        }
    }

    /// 中文标签。
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Active => "生效中",
            Self::Exhausted => "已用尽",
            Self::Revoked => "已撤销",
            Self::Expired => "已过期",
            Self::Ended => "已结束",
        }
    }

    /// 是否可用于调用。
    #[must_use]
    pub const fn is_usable(self) -> bool {
        matches!(self, Self::Active)
    }
}

/// 一次任务级授权（模块自有类型，与 platform 记录以 `consent_id` 关联）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskConsent {
    /// 授权 id。
    pub consent_id: ConsentId,
    /// 归属任务。
    pub task_id: TaskId,
    /// 允许使用的后端（当前仅 `openai_compat`）。
    pub backend: BackendId,
    /// 允许外发内容的范围指纹。
    pub scope_hash: Sha256Hex,
    /// 该任务的云端费用上限（微元）。
    pub budget_micros: u64,
    /// 状态。
    pub state: ConsentState,
    /// 登记时间。
    pub granted_at: Timestamp,
    /// 结束时间（终态/撤销时填）。
    pub ended_at: Option<Timestamp>,
}

impl TaskConsent {
    /// 是否仍可用于调用（终态即失效）。
    #[must_use]
    pub fn is_usable(&self) -> bool {
        self.state.is_usable() && self.ended_at.is_none()
    }
}

/// 一次云端调用的授权校验输入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentCheck {
    /// 任务 id。
    pub task_id: TaskId,
    /// 目标后端。
    pub backend: BackendId,
    /// 本次外发内容的范围指纹。
    pub scope_hash: Sha256Hex,
    /// 当前时间（来自注入时钟，便于测试）。
    pub now: Timestamp,
}

/// 授权门禁抽象：生产接 `platform::consent::ConsentStore`，测试用内存实现。
///
/// `validate` 必须保证「拒绝时调用方 0 次外联」——它自身不做任何网络请求。
pub trait ConsentGate: Send + Sync + core::fmt::Debug {
    /// 校验一次云端调用授权（五维）。
    ///
    /// # Errors
    ///
    /// 无授权、任务不符、后端不符、范围不符、状态不可用、结束时间已到、
    /// 额度已用尽均返回 [`InferenceError`]（分类为 `Policy`）。
    fn validate(&self, check: &ConsentCheck) -> Result<TaskConsent, InferenceError>;

    /// 额度用尽回调：服务在扣费后调用，把授权置为「已用尽」。
    ///
    /// 默认不做事（只读门禁实现可忽略）；持久化实现应更新状态并写审计。
    ///
    /// # Errors
    ///
    /// 状态更新失败返回错误（调用方记录但不回滚已发生的调用）。
    fn mark_exhausted(&self, _task_id: TaskId) -> Result<(), InferenceError> {
        Ok(())
    }
}

/// 内存授权门禁（离线演示与测试；生产由 `platform_adapter` 持久化实现）。
#[derive(Debug, Default)]
pub struct MemoryConsentGate {
    grants: Mutex<BTreeMap<TaskId, TaskConsent>>,
}

impl MemoryConsentGate {
    /// 构造空门禁。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一次授权（同任务覆盖旧授权）。
    pub fn grant(&self, consent: TaskConsent) {
        self.lock().insert(consent.task_id, consent);
    }

    /// 结束任务授权（任务终态）。
    pub fn end_for_task(&self, task_id: TaskId) -> bool {
        if let Some(consent) = self.lock().get_mut(&task_id) {
            consent.state = ConsentState::Ended;
            consent.ended_at = Some(consent.granted_at);
            true
        } else {
            false
        }
    }

    /// 撤销授权。
    pub fn revoke(&self, task_id: TaskId) -> bool {
        if let Some(consent) = self.lock().get_mut(&task_id) {
            consent.state = ConsentState::Revoked;
            true
        } else {
            false
        }
    }

    /// 标记预算用尽（内部实现；门禁接口见 [`ConsentGate::mark_exhausted`]）。
    pub fn set_exhausted(&self, task_id: TaskId) -> bool {
        if let Some(consent) = self.lock().get_mut(&task_id) {
            consent.state = ConsentState::Exhausted;
            true
        } else {
            false
        }
    }

    /// 当前授权（仅测试/诊断读取）。
    #[must_use]
    pub fn get(&self, task_id: TaskId) -> Option<TaskConsent> {
        self.lock().get(&task_id).cloned()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<TaskId, TaskConsent>> {
        match self.grants.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

impl ConsentGate for MemoryConsentGate {
    fn mark_exhausted(&self, task_id: TaskId) -> Result<(), InferenceError> {
        self.set_exhausted(task_id);
        Ok(())
    }

    fn validate(&self, check: &ConsentCheck) -> Result<TaskConsent, InferenceError> {
        let consent = self.lock().get(&check.task_id).cloned().ok_or_else(|| {
            InferenceError::ConsentMissing {
                detail: format!("任务 {} 没有云端授权记录（按任务鉴权恒开）", check.task_id),
            }
        })?;

        // 维度 1：任务（记录以 task_id 为键，仍显式校验以防数据被串用）。
        if consent.task_id != check.task_id {
            return Err(InferenceError::ConsentMissing {
                detail: "授权记录的任务与当前任务不一致".to_owned(),
            });
        }
        // 维度 2：后端 —— 授权只覆盖云端后端。
        if consent.backend != check.backend {
            return Err(InferenceError::ConsentMissing {
                detail: format!(
                    "授权只覆盖后端 {}，不能用于 {}",
                    consent.backend, check.backend
                ),
            });
        }
        if check.backend != BackendId::openai_compat() {
            return Err(InferenceError::ConsentMissing {
                detail: format!("后端 {} 不需要也不允许使用云端授权", check.backend),
            });
        }
        // 维度 3：范围（硬拒绝，不裁剪后发送）。
        if consent.scope_hash != check.scope_hash {
            return Err(InferenceError::ScopeMismatch);
        }
        // 维度 4：状态（过期/撤销/结束/用尽）。
        if !consent.is_usable() {
            return Err(InferenceError::ConsentInactive {
                consent_id: consent.consent_id.to_string(),
                state: consent.state.code().to_owned(),
            });
        }
        // 维度 5：时间（结束时间早于当前时间即失效；正常结束由状态表达）。
        if let Some(ended_at) = consent.ended_at
            && ended_at <= check.now
        {
            return Err(InferenceError::ConsentInactive {
                consent_id: consent.consent_id.to_string(),
                state: "ended".to_owned(),
            });
        }
        Ok(consent)
    }
}

/// 云端总开关判断（`privacy.cloud_enabled`，默认关）。
///
/// # Errors
///
/// 开关关闭时返回 [`InferenceError::CloudDisabled`]（申请即拒绝，不排队、不预热）。
pub fn require_cloud_enabled(cloud_enabled: bool) -> Result<(), InferenceError> {
    if cloud_enabled {
        Ok(())
    } else {
        Err(InferenceError::CloudDisabled)
    }
}

/// 由注入时钟取当前时间（避免业务代码直接读系统时间）。
#[must_use]
pub fn now_of(clock: &dyn Clock) -> Timestamp {
    clock.now()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::sync::Arc;

    use eg_domain::{ConsentId, Sha256Hex, Ulid};

    use crate::testing::TestClock;

    use super::*;

    fn task(seed: u128) -> TaskId {
        TaskId::from_ulid(Ulid::from_u128(seed))
    }

    fn consent_id(seed: u128) -> ConsentId {
        ConsentId::from_ulid(Ulid::from_u128(seed))
    }

    fn consent(task_id: TaskId, clock: &TestClock, budget: u64) -> TaskConsent {
        TaskConsent {
            consent_id: consent_id(9),
            task_id,
            backend: BackendId::openai_compat(),
            scope_hash: Sha256Hex::of(b"scope:answer_crop+text_redacted"),
            budget_micros: budget,
            state: ConsentState::Active,
            granted_at: clock.now(),
            ended_at: None,
        }
    }

    fn check(task_id: TaskId, clock: &TestClock) -> ConsentCheck {
        ConsentCheck {
            task_id,
            backend: BackendId::openai_compat(),
            scope_hash: Sha256Hex::of(b"scope:answer_crop+text_redacted"),
            now: clock.now(),
        }
    }

    #[test]
    fn valid_consent_passes_all_five_dimensions() {
        let clock = Arc::new(TestClock::at_ms(1_000));
        let gate = MemoryConsentGate::new();
        let task_id = task(1);
        gate.grant(consent(task_id, &clock, 1_000_000));
        let validated = gate.validate(&check(task_id, &clock)).unwrap();
        assert_eq!(validated.consent_id, consent_id(9));
    }

    #[test]
    fn without_consent_zero_outbound_is_possible() {
        let clock = Arc::new(TestClock::at_ms(1_000));
        let gate = MemoryConsentGate::new();
        let error = gate.validate(&check(task(2), &clock)).unwrap_err();
        assert!(matches!(error, InferenceError::ConsentMissing { .. }));
        assert_eq!(error.class(), crate::error::FailureClass::Policy);
        // 门禁本身不产生任何网络请求：没有可观测的外联（由 service 层断言 0 次调用）。
    }

    #[test]
    fn revoked_expired_ended_and_exhausted_are_all_rejected() {
        for state in [
            ConsentState::Revoked,
            ConsentState::Expired,
            ConsentState::Ended,
            ConsentState::Exhausted,
        ] {
            let clock = Arc::new(TestClock::at_ms(1_000));
            let gate = MemoryConsentGate::new();
            let task_id = task(3);
            let mut grant = consent(task_id, &clock, 1_000_000);
            grant.state = state;
            gate.grant(grant);
            let error = gate.validate(&check(task_id, &clock)).unwrap_err();
            assert!(
                matches!(error, InferenceError::ConsentInactive { .. }),
                "{state:?} 必须拒绝"
            );
        }
    }

    #[test]
    fn task_backend_and_scope_dimensions_are_enforced() {
        let clock = Arc::new(TestClock::at_ms(1_000));
        let gate = MemoryConsentGate::new();
        let task_id = task(4);
        gate.grant(consent(task_id, &clock, 1_000_000));

        // 维度 1：另一个任务没有授权
        let error = gate.validate(&check(task(5), &clock)).unwrap_err();
        assert!(matches!(error, InferenceError::ConsentMissing { .. }));

        // 维度 2：本地后端不能借用云端授权
        let local = ConsentCheck {
            task_id,
            backend: BackendId::local_server(),
            ..check(task_id, &clock)
        };
        assert!(matches!(
            gate.validate(&local).unwrap_err(),
            InferenceError::ConsentMissing { .. }
        ));

        // 维度 3：范围不一致 → 硬拒绝
        let wide = ConsentCheck {
            task_id,
            scope_hash: Sha256Hex::of(b"scope:full_page"),
            ..check(task_id, &clock)
        };
        let error = gate.validate(&wide).unwrap_err();
        assert!(matches!(error, InferenceError::ScopeMismatch));
        assert_eq!(error.class(), crate::error::FailureClass::Policy);
    }

    #[test]
    fn lifecycle_transitions_invalidate_the_consent() {
        let clock = Arc::new(TestClock::at_ms(1_000));
        let gate = MemoryConsentGate::new();
        let task_id = task(6);
        gate.grant(consent(task_id, &clock, 1_000_000));
        assert!(gate.validate(&check(task_id, &clock)).is_ok());

        assert!(gate.end_for_task(task_id));
        assert!(gate.validate(&check(task_id, &clock)).is_err());

        // 续跑需重新授权
        gate.grant(consent(task_id, &clock, 1_000_000));
        assert!(gate.validate(&check(task_id, &clock)).is_ok());
        assert!(gate.revoke(task_id));
        assert!(gate.validate(&check(task_id, &clock)).is_err());

        gate.grant(consent(task_id, &clock, 1_000_000));
        assert!(gate.set_exhausted(task_id));
        assert!(gate.validate(&check(task_id, &clock)).is_err());
    }

    #[test]
    fn cloud_switch_off_rejects_immediately() {
        let error = require_cloud_enabled(false).unwrap_err();
        assert!(matches!(error, InferenceError::CloudDisabled));
        assert!(require_cloud_enabled(true).is_ok());
    }

    #[test]
    fn state_codes_are_stable() {
        assert_eq!(ConsentState::Active.code(), "active");
        assert_eq!(ConsentState::Ended.code(), "ended");
        assert_eq!(ConsentState::Revoked.label(), "已撤销");
        assert!(ConsentState::Active.is_usable());
        assert!(!ConsentState::Exhausted.is_usable());
    }
}
