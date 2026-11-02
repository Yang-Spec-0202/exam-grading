//! `ModelCall` 记录与审计注入点（`SPEC-inference.md` §3.1/§6/§8）。
//!
//! 硬性规则：**没有 `ModelCall` 就不出结果**（规范 §8 Never）。
//! 因此记录失败即调用失败；`ModelCall` 与审计分两个注入点：
//! - [`ModelCallStore`]：写 `model_call` 表（生产经 `platform_adapter` 落 SQLite）；
//! - [`AuditSink`]：写只追加审计（云端授权校验、许可评审引用等）。
//!
//! 失败调用也要留痕：领域 `ModelCall.output` 不允许为空，因此失败时写入
//! `{"error":{"class":…,"message":…}}` 标记对象（不含学生数据、图像字节或密钥）。

use std::sync::Arc;

use eg_domain::{
    Clock, ConsentId, EngineStamp, ModelCall, ModelCallId, ModelRole, Sha256Hex, TaskId, Timestamp,
};
use serde_json::{Value, json};

use crate::error::InferenceError;

/// `ModelCall` 持久化注入点。
pub trait ModelCallStore: Send + Sync + core::fmt::Debug {
    /// 写入一条调用记录；失败即视为调用失败（禁止无记录出结果）。
    ///
    /// # Errors
    ///
    /// 存储失败返回 [`InferenceError::Platform`]。
    fn record(&self, call: &ModelCall) -> Result<(), InferenceError>;
}

/// 审计注入点（只追加；云端授权、许可评审、模型注册变更）。
pub trait AuditSink: Send + Sync + core::fmt::Debug {
    /// 写一条审计事件。
    ///
    /// # Errors
    ///
    /// 写入失败返回 [`InferenceError::Platform`]。
    fn record(&self, event: &AuditEvent) -> Result<(), InferenceError>;
}

/// id 生成注入点（生产用 `platform::UlidFactory`；测试用计数器）。
pub trait IdSource: Send + Sync + core::fmt::Debug {
    /// 生成 `ModelCallId`。
    ///
    /// # Errors
    ///
    /// 生成失败返回错误。
    fn next_model_call_id(&self) -> Result<ModelCallId, InferenceError>;
}

/// 审计事件（与 `platform::audit::NewAuditEntry` 对接）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEvent {
    /// 动作（如 `inference.cloud_call`）。
    pub action: String,
    /// 实体类型（如 `model_call`）。
    pub entity_type: String,
    /// 实体 id。
    pub entity_id: String,
    /// 中文备注（不含敏感内容）。
    pub note: String,
    /// 关联任务。
    pub task_id: Option<TaskId>,
    /// 关联授权。
    pub consent_id: Option<ConsentId>,
}

impl AuditEvent {
    /// 构造最简事件。
    #[must_use]
    pub fn new(
        action: impl Into<String>,
        entity_type: impl Into<String>,
        entity_id: impl Into<String>,
        note: impl Into<String>,
    ) -> Self {
        Self {
            action: action.into(),
            entity_type: entity_type.into(),
            entity_id: entity_id.into(),
            note: note.into(),
            task_id: None,
            consent_id: None,
        }
    }

    /// 关联任务。
    #[must_use]
    pub fn with_task(mut self, task_id: TaskId) -> Self {
        self.task_id = Some(task_id);
        self
    }

    /// 关联授权。
    #[must_use]
    pub fn with_consent(mut self, consent_id: ConsentId) -> Self {
        self.consent_id = Some(consent_id);
        self
    }
}

/// 一次调用的上下文（记录 `ModelCall` 所需的最小信息）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallContext {
    /// 所属任务。
    pub task_id: TaskId,
    /// 角色。
    pub role: ModelRole,
    /// 云端授权 id（云端调用必填）。
    pub consent_id: Option<ConsentId>,
    /// 提示词哈希。
    pub prompt_hash: Option<Sha256Hex>,
}

impl CallContext {
    /// 本地调用上下文（无授权）。
    #[must_use]
    pub fn local(task_id: TaskId, role: ModelRole, prompt_hash: Option<Sha256Hex>) -> Self {
        Self {
            task_id,
            role,
            consent_id: None,
            prompt_hash,
        }
    }

    /// 云端调用上下文（必须带授权 id）。
    #[must_use]
    pub fn cloud(
        task_id: TaskId,
        role: ModelRole,
        consent_id: ConsentId,
        prompt_hash: Option<Sha256Hex>,
    ) -> Self {
        Self {
            task_id,
            role,
            consent_id: Some(consent_id),
            prompt_hash,
        }
    }
}

/// 调用记录器：把一次调用的结果写成 `ModelCall`（并做领域不变量校验）。
pub struct CallRecorder {
    store: Arc<dyn ModelCallStore>,
    audit: Arc<dyn AuditSink>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdSource>,
}

impl core::fmt::Debug for CallRecorder {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("CallRecorder")
            .finish_non_exhaustive()
    }
}

impl CallRecorder {
    /// 构造记录器。
    #[must_use]
    pub fn new(
        store: Arc<dyn ModelCallStore>,
        audit: Arc<dyn AuditSink>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdSource>,
    ) -> Self {
        Self {
            store,
            audit,
            clock,
            ids,
        }
    }

    /// 记录一次成功调用。
    ///
    /// # Errors
    ///
    /// id 生成失败、领域校验失败、存储失败时返回错误（调用方必须让本次调用失败）。
    pub fn record_success(
        &self,
        context: &CallContext,
        engine: &EngineStamp,
        input_hash: Sha256Hex,
        output: Value,
        latency_ms: u64,
        cost_micros: u64,
    ) -> Result<ModelCall, InferenceError> {
        self.record(
            context,
            engine,
            input_hash,
            output,
            latency_ms,
            cost_micros,
            None,
        )
    }

    /// 记录一次失败调用（写入错误标记对象，保持 `ModelCall` 可校验）。
    ///
    /// # Errors
    ///
    /// 同 [`Self::record_success`]。
    pub fn record_failure(
        &self,
        context: &CallContext,
        engine: &EngineStamp,
        input_hash: Sha256Hex,
        error: &InferenceError,
        latency_ms: u64,
    ) -> Result<ModelCall, InferenceError> {
        let marker = json!({
            "error": {
                "class": error.class_label(),
                "message": error.to_string(),
            }
        });
        self.record(
            context,
            engine,
            input_hash,
            marker,
            latency_ms,
            0,
            Some(error),
        )
    }

    // 参数是三个调用点各自独立的裸字段（输出/时延/费用/错误），
    // 打包成结构体只会把归属关系藏起来；参数顺序与 `ModelCall` 字段一致。
    #[allow(clippy::too_many_arguments)]
    fn record(
        &self,
        context: &CallContext,
        engine: &EngineStamp,
        input_hash: Sha256Hex,
        output: Value,
        latency_ms: u64,
        cost_micros: u64,
        error: Option<&InferenceError>,
    ) -> Result<ModelCall, InferenceError> {
        let call = ModelCall {
            id: self.ids.next_model_call_id()?,
            role: context.role,
            engine: engine.clone(),
            task_id: context.task_id,
            consent_id: context.consent_id,
            input_hash,
            output,
            latency_ms,
            cost_micros,
            created_at: self.clock.now(),
        };
        call.validate().map_err(|error| InferenceError::Internal {
            detail: format!("ModelCall 未通过领域校验：{error}"),
        })?;
        self.store.record(&call)?;

        // 云端调用与失败调用写审计（本地成功调用只靠 ModelCall 即可追溯）。
        let needs_audit = error.is_some() || engine.is_cloud();
        if needs_audit {
            let mut event = AuditEvent::new(
                if engine.is_cloud() {
                    "inference.cloud_call"
                } else {
                    "inference.call_failed"
                },
                "model_call",
                call.id.to_string(),
                format!(
                    "模型 {} 角色 {:?} 调用{}（{} ms）",
                    engine.model_id,
                    context.role,
                    if error.is_some() { "失败" } else { "成功" },
                    latency_ms
                ),
            )
            .with_task(context.task_id);
            if let Some(consent_id) = context.consent_id {
                event = event.with_consent(consent_id);
            }
            self.audit.record(&event)?;
        }
        Ok(call)
    }

    /// 只写审计、不写 `ModelCall`。
    ///
    /// 用于「调用根本没有发生」的拦截（如云端无授权）：领域 `ModelCall` 要求云端调用
    /// 必须带 `consent_id`，此时写 ModelCall 会违反领域不变量；而 SPEC §4 的「失败也留痕」
    /// 由审计承担（`inference.policy_blocked`），不伪造一次调用记录。
    ///
    /// # Errors
    ///
    /// 审计写入失败返回错误。
    pub fn audit_only(&self, event: &AuditEvent) -> Result<(), InferenceError> {
        self.audit.record(event)
    }

    /// 当前时间（测试/诊断用）。
    #[must_use]
    pub fn now(&self) -> Timestamp {
        self.clock.now()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::sync::Mutex;

    use eg_domain::{BackendId, ModelRole, Ulid};

    use crate::testing::TestClock;

    use super::*;

    #[derive(Debug, Default)]
    struct RecordingStore {
        calls: Mutex<Vec<ModelCall>>,
    }

    impl ModelCallStore for RecordingStore {
        fn record(&self, call: &ModelCall) -> Result<(), InferenceError> {
            self.calls.lock().unwrap().push(call.clone());
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct RecordingAudit {
        events: Mutex<Vec<AuditEvent>>,
    }

    impl AuditSink for RecordingAudit {
        fn record(&self, event: &AuditEvent) -> Result<(), InferenceError> {
            self.events.lock().unwrap().push(event.clone());
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct CounterIds {
        next: Mutex<u128>,
    }

    impl IdSource for CounterIds {
        fn next_model_call_id(&self) -> Result<ModelCallId, InferenceError> {
            let mut guard = self.next.lock().unwrap();
            *guard += 1;
            Ok(ModelCallId::from_ulid(Ulid::from_u128(*guard)))
        }
    }

    #[derive(Debug, Default)]
    struct FailingStore;

    impl ModelCallStore for FailingStore {
        fn record(&self, _call: &ModelCall) -> Result<(), InferenceError> {
            Err(InferenceError::Platform {
                detail: "数据库不可写".to_owned(),
            })
        }
    }

    fn engine(backend: BackendId, role: ModelRole) -> EngineStamp {
        EngineStamp {
            role,
            backend,
            model_id: "m1".to_owned(),
            model_version: "1.0".to_owned(),
            prompt_hash: None,
            params_hash: Sha256Hex::of(b"params"),
        }
    }

    fn recorder(store: Arc<dyn ModelCallStore>) -> (CallRecorder, Arc<RecordingAudit>) {
        let audit = Arc::new(RecordingAudit::default());
        let recorder = CallRecorder::new(
            store,
            audit.clone(),
            Arc::new(TestClock::at_ms(1_000)),
            Arc::new(CounterIds::default()),
        );
        (recorder, audit)
    }

    fn task() -> TaskId {
        TaskId::from_ulid(Ulid::from_u128(7))
    }

    #[test]
    fn success_is_recorded_without_audit_for_local_calls() {
        let store = Arc::new(RecordingStore::default());
        let (recorder, audit) = recorder(store.clone());
        let context = CallContext::local(task(), ModelRole::Ocr, Some(Sha256Hex::of(b"p")));
        let call = recorder
            .record_success(
                &context,
                &engine(BackendId::local_server(), ModelRole::Ocr),
                Sha256Hex::of(b"input"),
                json!({"blocks": []}),
                120,
                0,
            )
            .unwrap();
        assert_eq!(call.latency_ms, 120);
        assert_eq!(call.cost_micros, 0);
        assert_eq!(store.calls.lock().unwrap().len(), 1);
        assert!(audit.events.lock().unwrap().is_empty(), "本地成功不写审计");
    }

    #[test]
    fn cloud_calls_always_write_audit_and_carry_consent() {
        let store = Arc::new(RecordingStore::default());
        let (recorder, audit) = recorder(store.clone());
        let consent_id = ConsentId::from_ulid(Ulid::from_u128(11));
        let context = CallContext::cloud(task(), ModelRole::Judge, consent_id, None);
        let call = recorder
            .record_success(
                &context,
                &engine(BackendId::openai_compat(), ModelRole::Judge),
                Sha256Hex::of(b"input"),
                json!({"judgement": "match"}),
                900,
                1_500,
            )
            .unwrap();
        assert_eq!(call.consent_id, Some(consent_id));
        let events = audit.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "inference.cloud_call");
        assert_eq!(events[0].consent_id, Some(consent_id));
    }

    #[test]
    fn cloud_call_without_consent_is_rejected_by_domain_validation() {
        let store = Arc::new(RecordingStore::default());
        let (recorder, _audit) = recorder(store.clone());
        let context = CallContext::local(task(), ModelRole::Judge, None);
        let error = recorder
            .record_success(
                &context,
                &engine(BackendId::openai_compat(), ModelRole::Judge),
                Sha256Hex::of(b"input"),
                json!({"a": 1}),
                10,
                0,
            )
            .unwrap_err();
        assert!(error.to_string().contains("consent"), "实际：{error}");
        assert!(store.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn failure_is_recorded_with_error_marker() {
        let store = Arc::new(RecordingStore::default());
        let (recorder, audit) = recorder(store.clone());
        let context = CallContext::local(task(), ModelRole::Ocr, None);
        let error = InferenceError::Transient {
            status: 503,
            detail: "重启中".to_owned(),
        };
        let call = recorder
            .record_failure(
                &context,
                &engine(BackendId::local_server(), ModelRole::Ocr),
                Sha256Hex::of(b"input"),
                &error,
                42,
            )
            .unwrap();
        assert_eq!(call.output["error"]["class"], "瞬态失败");
        assert_eq!(store.calls.lock().unwrap().len(), 1);
        assert_eq!(
            audit.events.lock().unwrap()[0].action,
            "inference.call_failed"
        );
    }

    #[test]
    fn store_failure_fails_the_call() {
        let (recorder, _audit) = recorder(Arc::new(FailingStore));
        let context = CallContext::local(task(), ModelRole::Ocr, None);
        let error = recorder
            .record_success(
                &context,
                &engine(BackendId::local_server(), ModelRole::Ocr),
                Sha256Hex::of(b"input"),
                json!({"ok": true}),
                1,
                0,
            )
            .unwrap_err();
        assert!(matches!(error, InferenceError::Platform { .. }));
    }
}
