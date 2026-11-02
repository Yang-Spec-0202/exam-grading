//! Platform 适配层集成测试：用**真实**的 `eg-platform`（临时目录 SQLite）验证
//! `ModelCall` 落库、审计写入、id 生成与任务授权五维校验。
//!
//! 不联网、不依赖真实模型；平台实例在临时目录内独立打开/关闭。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use eg_domain::{
    BackendId, EngineStamp, ModelCall, ModelCallId, ModelRole, Sha256Hex, TaskId, Ulid,
};
use eg_inference::platform_adapter::{PlatformAdapters, register_cloud_consent};
use eg_inference::{AuditSink, ConsentCheck, ConsentGate, IdSource, ModelCallStore};
use eg_platform::clock::FixedClock;
use eg_platform::paths::DataDirs;
use eg_platform::platform::Platform;
use eg_platform::repo;
use eg_platform::tasks::{NewTask, TaskKind};

fn timestamp() -> eg_domain::Timestamp {
    eg_domain::time::parse_rfc3339("2026-11-02T00:00:00Z").unwrap()
}

fn open_platform(directory: &tempfile::TempDir) -> Platform {
    let dirs = DataDirs::with_root(directory.path()).unwrap();
    Platform::open_with(dirs, Arc::new(FixedClock::new(timestamp()))).unwrap()
}

fn task_id(platform: &Platform) -> TaskId {
    platform
        .tasks()
        .enqueue(NewTask::new(
            TaskKind::PageRecognize,
            serde_json::json!({"model_id": "paddleocr-vl-1.6"}),
        ))
        .unwrap()
        .task_id
}

fn model_call(task_id: TaskId, id_seed: u128) -> ModelCall {
    ModelCall {
        id: ModelCallId::from_ulid(Ulid::from_u128(id_seed)),
        role: ModelRole::Ocr,
        engine: EngineStamp {
            role: ModelRole::Ocr,
            backend: BackendId::local_server(),
            model_id: "paddleocr-vl-1.6".to_owned(),
            model_version: "1.6".to_owned(),
            prompt_hash: Some(Sha256Hex::of(b"ocr.blocks.v1")),
            params_hash: Sha256Hex::of(b"params"),
        },
        task_id,
        consent_id: None,
        input_hash: Sha256Hex::of(b"page-bytes"),
        output: serde_json::json!({"blocks": [{"kind": "text", "text": "ok"}]}),
        latency_ms: 1_234,
        cost_micros: 0,
        created_at: timestamp(),
    }
}

#[test]
fn model_calls_are_persisted_with_audit_in_one_transaction() {
    let directory = tempfile::tempdir().unwrap();
    let platform = open_platform(&directory);
    let task_id = task_id(&platform);
    let adapters = PlatformAdapters::from_platform(&platform);

    let call = model_call(task_id, 1);
    adapters.store.record(&call).unwrap();

    let stored = platform
        .db()
        .read(|conn| repo::list_model_calls(conn, task_id))
        .unwrap();
    assert_eq!(stored.len(), 1, "ModelCall 必须落库");
    assert_eq!(stored[0].id, call.id);
    assert_eq!(stored[0].engine.model_id, "paddleocr-vl-1.6");
    assert_eq!(stored[0].output["blocks"][0]["text"], "ok");

    // id 生成走 platform 的单调工厂
    let generated = adapters.ids.next_model_call_id().unwrap();
    assert_ne!(generated.to_string(), "");

    // 审计写入可用（业务写同事务内已追加；此处再显式写一条）
    adapters
        .audit
        .record(&eg_inference::AuditEvent::new(
            "inference.test",
            "model",
            "paddleocr-vl-1.6",
            "适配层测试",
        ))
        .unwrap();

    platform.close().unwrap();
}

#[test]
fn consent_gate_maps_platform_states_to_five_dimension_checks() {
    let directory = tempfile::tempdir().unwrap();
    let platform = open_platform(&directory);
    let task_id = task_id(&platform);
    let adapters = PlatformAdapters::from_platform(&platform);
    let scope = Sha256Hex::of(b"scope:answer_crop");

    // 未登记授权 → 无授权（0 次外联由服务层保证）
    let check = ConsentCheck {
        task_id,
        backend: BackendId::openai_compat(),
        scope_hash: scope,
        now: timestamp(),
    };
    let error = adapters.consents.validate(&check).unwrap_err();
    assert!(matches!(
        error,
        eg_inference::InferenceError::ConsentMissing { .. }
    ));

    // 登记授权 → 通过
    let consent_id = register_cloud_consent(&adapters.consents, task_id, scope, 2_000_000).unwrap();
    let consent = adapters.consents.validate(&check).unwrap();
    assert_eq!(consent.consent_id, consent_id);
    assert_eq!(consent.task_id, task_id);
    assert_eq!(consent.scope_hash, scope);
    assert_eq!(consent.budget_micros, 2_000_000);
    assert!(consent.is_usable());

    // 范围不匹配 → 硬拒绝
    let wider = ConsentCheck {
        scope_hash: Sha256Hex::of(b"scope:full_page"),
        ..check.clone()
    };
    let error = adapters.consents.validate(&wider).unwrap_err();
    assert!(matches!(error, eg_inference::InferenceError::ScopeMismatch));

    // 本地后端不能借用云端授权
    let local = ConsentCheck {
        backend: BackendId::local_server(),
        ..check.clone()
    };
    assert!(adapters.consents.validate(&local).is_err());

    // 撤销后失效
    platform.consents().revoke(consent_id).unwrap();
    let error = adapters.consents.validate(&check).unwrap_err();
    assert!(matches!(
        error,
        eg_inference::InferenceError::ConsentInactive { .. }
    ));

    platform.close().unwrap();
}

#[test]
fn exhausted_consent_is_policy_blocked_and_survives_restart() {
    let directory = tempfile::tempdir().unwrap();
    let scope = Sha256Hex::of(b"scope:answer_crop");
    let task_id = {
        let platform = open_platform(&directory);
        let task_id = task_id(&platform);
        let adapters = PlatformAdapters::from_platform(&platform);
        register_cloud_consent(&adapters.consents, task_id, scope, 2_000_000).unwrap();

        // 服务在扣费达到上限后回调 mark_exhausted → platform 持久化「额度用尽」。
        adapters.consents.mark_exhausted(task_id).unwrap();
        assert!(platform.consents().is_exhausted(task_id).unwrap());

        let check = ConsentCheck {
            task_id,
            backend: BackendId::openai_compat(),
            scope_hash: scope,
            now: timestamp(),
        };
        match adapters.consents.validate(&check).unwrap_err() {
            eg_inference::InferenceError::ConsentInactive { state, .. } => {
                assert_eq!(state, "exhausted", "额度用尽必须映射为授权不可用");
            }
            other => panic!("额度用尽必须走授权不可用（策略拦截）路径，实际：{other:?}"),
        }

        // 历史记录映射保留 exhausted（诊断/审计展示与撤销/结束一致）。
        let record = platform
            .consents()
            .list_by_task(task_id)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(
            eg_inference::platform_adapter::consent_record_to_task_consent(&record)
                .state
                .code(),
            "exhausted"
        );
        platform.close().unwrap();
        task_id
    };

    // 重启后仍不可用（额度用尽持久化；0 次外联由服务层调用前拦截）。
    let platform = open_platform(&directory);
    let adapters = PlatformAdapters::from_platform(&platform);
    assert!(platform.consents().is_exhausted(task_id).unwrap());
    let error = adapters
        .consents
        .validate(&ConsentCheck {
            task_id,
            backend: BackendId::openai_compat(),
            scope_hash: scope,
            now: timestamp(),
        })
        .unwrap_err();
    assert!(
        matches!(error, eg_inference::InferenceError::ConsentInactive { .. }),
        "重启后额度用尽必须仍拒绝，实际：{error:?}"
    );
    platform.close().unwrap();
}

#[test]
fn separate_platform_instances_are_isolated() {
    let first_dir = tempfile::tempdir().unwrap();
    let second_dir = tempfile::tempdir().unwrap();
    let first = open_platform(&first_dir);
    let second = open_platform(&second_dir);
    let first_task = task_id(&first);
    let adapters = PlatformAdapters::from_platform(&first);
    adapters.store.record(&model_call(first_task, 5)).unwrap();

    let second_calls = second
        .db()
        .read(|conn| repo::list_model_calls(conn, first_task))
        .unwrap();
    assert!(second_calls.is_empty(), "两个平台实例的数据必须隔离");

    first.close().unwrap();
    second.close().unwrap();
}
