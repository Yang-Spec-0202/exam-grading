//! Platform 适配层（`SPEC-platform.md` 仓储/审计/授权/时间/id 接口）。
//!
//! 本 crate 的其余部分只依赖三个注入 trait（[`ModelCallStore`]、[`AuditSink`]、
//! [`IdSource`]）与 [`ConsentGate`]；本文件把它们接到 `eg-platform` 的真实接口上，
//! 是**唯一**接触 platform 具体类型的地方（平台接口变化时只改这里）。
//!
//! 已知限制（已向 Lead 报告，不改 platform）：
//! - platform 的 `ConsentState` 只有 `active/ended/revoked`，没有「额度用尽」状态，
//!   因此 [`PlatformConsentGate::mark_exhausted`] 目前是 no-op：额度由
//!   [`crate::budget::BudgetLedger`] 在进程内强制，授权状态不变（下次调用仍会被
//!   `validate_call` 放行）。若要持久化「用尽」，需要 platform 增加状态或专用接口。
//! - platform 的 `validate_call` 只返回 `PolicyBlocked(消息)`，无法区分「无授权」与
//!   「范围不匹配」；本适配层先 `is_valid` 再 `validate_call` 以区分两者，
//!   对剩余情况按消息内容兜底（TODO：待 platform 提供类型化错误后替换）。

use std::sync::Arc;

use eg_domain::{BackendId, ConsentId, ModelCall, ModelCallId, Sha256Hex, TaskId};
use eg_platform::audit::{AuditLog, NewAuditEntry};
use eg_platform::consent::ConsentStore;
use eg_platform::error::PlatformError;
use eg_platform::ids::UlidFactory;
use eg_platform::platform::Platform;
use eg_platform::repo;

use crate::call::{AuditEvent, AuditSink, IdSource, ModelCallStore};
use crate::consent::{ConsentCheck, ConsentGate, ConsentState, TaskConsent};
use crate::error::InferenceError;

/// 一组 platform 适配器（组合根在启动时构造一次并注入服务）。
#[derive(Debug, Clone)]
pub struct PlatformAdapters {
    /// `ModelCall` 落库（与审计同事务）。
    pub store: Arc<PlatformModelCallStore>,
    /// 审计写入。
    pub audit: Arc<PlatformAuditSink>,
    /// id 生成。
    pub ids: Arc<PlatformIdSource>,
    /// 云端任务授权校验。
    pub consents: Arc<PlatformConsentGate>,
}

impl PlatformAdapters {
    /// 从已打开的 `Platform` 构造全部适配器。
    #[must_use]
    pub fn from_platform(platform: &Platform) -> Self {
        Self {
            store: Arc::new(PlatformModelCallStore {
                audit: Arc::clone(platform.audit()),
            }),
            audit: Arc::new(PlatformAuditSink {
                audit: Arc::clone(platform.audit()),
            }),
            ids: Arc::new(PlatformIdSource {
                ids: Arc::clone(platform.ids()),
            }),
            consents: Arc::new(PlatformConsentGate {
                consents: platform.consents().clone(),
            }),
        }
    }
}

/// `ModelCall` 仓储适配：一次 `business_write` 内同时写 model_call 与审计。
#[derive(Debug, Clone)]
pub struct PlatformModelCallStore {
    audit: Arc<AuditLog>,
}

impl ModelCallStore for PlatformModelCallStore {
    fn record(&self, call: &ModelCall) -> Result<(), InferenceError> {
        let call = call.clone();
        let mut entry =
            NewAuditEntry::new("inference.model_call", "model_call", call.id.to_string())
                .with_note(format!(
                    "模型 {}（角色 {:?}）调用：{} ms，{} 微元",
                    call.engine.model_id, call.role, call.latency_ms, call.cost_micros
                ))
                .with_task(call.task_id);
        if let Some(consent_id) = call.consent_id {
            entry = entry.with_consent(consent_id);
        }
        self.audit
            .business_write(entry, move |tx| repo::insert_model_call(tx, &call))
            .map_err(platform_error)?;
        Ok(())
    }
}

/// 审计适配。
#[derive(Debug, Clone)]
pub struct PlatformAuditSink {
    audit: Arc<AuditLog>,
}

impl AuditSink for PlatformAuditSink {
    fn record(&self, event: &AuditEvent) -> Result<(), InferenceError> {
        let mut entry = NewAuditEntry::new(
            event.action.clone(),
            event.entity_type.clone(),
            event.entity_id.clone(),
        )
        .with_note(event.note.clone());
        if let Some(task_id) = event.task_id {
            entry = entry.with_task(task_id);
        }
        if let Some(consent_id) = event.consent_id {
            entry = entry.with_consent(consent_id);
        }
        self.audit.append(entry).map_err(platform_error)?;
        Ok(())
    }
}

/// id 生成适配（`platform::UlidFactory` 单调不倒退）。
#[derive(Debug, Clone)]
pub struct PlatformIdSource {
    ids: Arc<UlidFactory>,
}

impl IdSource for PlatformIdSource {
    fn next_model_call_id(&self) -> Result<ModelCallId, InferenceError> {
        self.ids.next_model_call_id().map_err(platform_error)
    }
}

/// 任务授权适配（`platform::consent::ConsentStore`）。
#[derive(Debug, Clone)]
pub struct PlatformConsentGate {
    consents: ConsentStore,
}

impl ConsentGate for PlatformConsentGate {
    fn validate(&self, check: &ConsentCheck) -> Result<TaskConsent, InferenceError> {
        if check.backend != BackendId::openai_compat() {
            return Err(InferenceError::ConsentMissing {
                detail: format!("后端 {} 不使用云端授权", check.backend),
            });
        }
        // 先区分「没有授权」与「范围不匹配」（platform 的 PolicyBlocked 不区分）。
        match self.consents.is_valid(check.task_id) {
            Err(error) => Err(platform_error(error)),
            Ok(None) => {
                // 没有「有效」授权时区分「从未授权」与「已撤销/已结束」，
                // 便于界面提示与审计（platform 只暴露 is_valid/validate_call）。
                let history = self
                    .consents
                    .list_by_task(check.task_id)
                    .unwrap_or_default();
                match history.last() {
                    Some(record) => Err(InferenceError::ConsentInactive {
                        consent_id: record.consent_id.to_string(),
                        state: map_state(record.state).code().to_owned(),
                    }),
                    None => Err(InferenceError::ConsentMissing {
                        detail: format!(
                            "任务 {} 没有有效的云端授权（按任务鉴权恒开）",
                            check.task_id
                        ),
                    }),
                }
            }
            Ok(Some(record)) => {
                match self.consents.validate_call(check.task_id, check.scope_hash) {
                    Ok(valid) => Ok(TaskConsent {
                        consent_id: valid.consent_id,
                        task_id: valid.task_id,
                        backend: BackendId::openai_compat(),
                        scope_hash: valid.scope_hash,
                        budget_micros: valid.budget_micros,
                        state: map_state(valid.state),
                        granted_at: valid.granted_at,
                        ended_at: valid.ended_at,
                    }),
                    Err(PlatformError::PolicyBlocked(message)) => {
                        if message.contains("范围") {
                            Err(InferenceError::ScopeMismatch)
                        } else {
                            Err(InferenceError::ConsentInactive {
                                consent_id: record.consent_id.to_string(),
                                state: map_state(record.state).code().to_owned(),
                            })
                        }
                    }
                    Err(other) => Err(platform_error(other)),
                }
            }
        }
    }

    fn mark_exhausted(&self, task_id: TaskId) -> Result<(), InferenceError> {
        // platform 持久化「额度用尽」：置为 exhausted 后重启仍不可用。
        // 本次扣费已发生（调用方不回滚），这里只更新授权状态。
        self.consents
            .mark_exhausted(task_id)
            .map(|_updated| ())
            .map_err(platform_error)
    }
}

fn map_state(state: eg_platform::consent::ConsentState) -> ConsentState {
    match state {
        eg_platform::consent::ConsentState::Active => ConsentState::Active,
        eg_platform::consent::ConsentState::Ended => ConsentState::Ended,
        eg_platform::consent::ConsentState::Revoked => ConsentState::Revoked,
        // 额度用尽与撤销/结束同等对待：授权不可用 → 策略拦截、0 次外联。
        eg_platform::consent::ConsentState::Exhausted => ConsentState::Exhausted,
    }
}

fn platform_error(error: PlatformError) -> InferenceError {
    InferenceError::Platform {
        detail: error.to_string(),
    }
}

/// 便捷：把一次云端授权记录映射为 `TaskConsent`（诊断/审计展示用）。
#[must_use]
pub fn consent_record_to_task_consent(
    record: &eg_platform::consent::TaskConsentRecord,
) -> TaskConsent {
    TaskConsent {
        consent_id: record.consent_id,
        task_id: record.task_id,
        backend: BackendId::openai_compat(),
        scope_hash: record.scope_hash,
        budget_micros: record.budget_micros,
        state: map_state(record.state),
        granted_at: record.granted_at,
        ended_at: record.ended_at,
    }
}

/// 便捷：注册一次云端授权（封装 platform 的 `register` 参数顺序）。
///
/// # Errors
///
/// 平台写入失败时返回 [`InferenceError::Platform`]。
pub fn register_cloud_consent(
    gate: &PlatformConsentGate,
    task_id: TaskId,
    scope_hash: Sha256Hex,
    budget_micros: u64,
) -> Result<ConsentId, InferenceError> {
    gate.consents
        .register(task_id, BackendId::OPENAI_COMPAT, scope_hash, budget_micros)
        .map(|record| record.consent_id)
        .map_err(platform_error)
}
