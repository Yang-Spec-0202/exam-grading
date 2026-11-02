//! 真实 serving 冒烟（默认 `#[ignore]`）：真实 llama.cpp sidecar + 真实权重。
//!
//! 目的：一次性验证「进程拉起 → `/health` 就绪 → `/v1/chat/completions` → JSON 提取」
//! 这条真实链路；默认测试集**不依赖**真实模型（其余测试全部用注入桩）。
//!
//! 运行（人工验收）：
//!
//! ```text
//! ./scripts/model/pull.sh judge
//! cargo test -p eg-inference --test live_smoke -- --ignored --nocapture
//! ```
//!
//! 说明：真实模型的判定质量不属于本冒烟断言范围，因此只断言「有 JSON 对象输出」，
//! Schema 判定结果打印出来供人工核对。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use eg_domain::{MilliScore, ModelRole};
use eg_inference::testing::TestClock;
use eg_inference::{
    ChatMessage, ChatRequestSpec, JudgeOutput, LifecycleConfig, LocalServer, LocalServerSpec,
    LoopbackPortAllocator, ModelRegistry, ServerState, SystemRunner, TcpTransport,
    default_models_dir, extract_json_object, unit_dir,
};

#[test]
#[ignore = "真实 llama.cpp sidecar：需本机 llama.cpp 与 scripts/model/pull.sh 下发的权重"]
fn live_judge_sidecar_start_health_and_chat() {
    let registry = ModelRegistry::embedded().expect("内嵌注册表必须可解析");
    let entry = registry
        .resolve_configured(
            "grade.model_profile",
            "qwen3.5-4b-instruct",
            ModelRole::Judge,
        )
        .expect("默认判定模型必须已在注册表登记");
    let unit = registry
        .serve_unit(&entry.id)
        .expect("判定模型必须能解析出本地服务单元");
    let models_dir = unit_dir(&default_models_dir(), &unit.unit);
    assert!(
        models_dir.join("model.gguf").exists(),
        "缺少权重 {}：先跑 ./scripts/model/pull.sh judge",
        models_dir.display()
    );

    let spec = LocalServerSpec::from_serve_unit(&unit, models_dir, entry.context_length, 0.0);
    let server = LocalServer::new(
        spec,
        LifecycleConfig::default(),
        Arc::new(TcpTransport::new()),
        Arc::new(SystemRunner),
        Arc::new(LoopbackPortAllocator),
        Arc::new(TestClock::at_ms(0)),
    );

    let metrics = server.start().expect("sidecar 必须能拉起并探活通过");
    assert_eq!(server.state(), ServerState::Ready);
    println!(
        "sidecar 就绪：port={} cold_start={} 用时 {} ms",
        metrics.port, metrics.cold_start, metrics.elapsed_ms
    );

    let messages = vec![
        ChatMessage::system("你是严格的判分助手。只输出单个 JSON 对象，不要 Markdown 围栏。"),
        ChatMessage::user(
            "学生答案：2；参考答案：2；满分 1000 毫分。\
             输出 {\"judgement\":\"match\",\"score_milli\":1000,\"reason\":\"与参考答案一致\",\"confidence\":0.95}",
        ),
    ];
    let request = ChatRequestSpec::new(entry.id.clone(), messages, 128);
    let (response, latency_ms) = server
        .chat(&request, Duration::from_secs(120))
        .expect("真实 sidecar 必须返回 200 且响应可解析");
    assert!(
        !response.content.trim().is_empty(),
        "真实模型返回了空内容（finish_reason={:?}）",
        response.finish_reason
    );
    let value = extract_json_object(&response.content).expect("真实模型应输出单个 JSON 对象");
    println!("模型输出：{value}");

    // 质量不属于冒烟断言：只记录 Schema 判定结果。
    match JudgeOutput::validate(
        &value,
        MilliScore::from_milli(1000).expect("满分 1000 微元"),
    ) {
        Ok(output) => println!("Schema 判定：通过（{output:?}）"),
        Err(error) => println!("Schema 判定：未通过（冒烟不断言）：{error}"),
    }
    println!(
        "单次 chat 延迟：{latency_ms} ms（usage={:?}）",
        response.usage
    );

    server.stop();
    assert_eq!(server.state(), ServerState::Stopped);
}
