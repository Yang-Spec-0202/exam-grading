//! CAS 适配层夹具（T27）：stub sidecar 注入正常/超时/崩溃/非法响应，缓存与降级。
//!
//! 不联网：只用 `python3` + `scripts/cas/stub_sidecar.py`（仅标准库）。
//! 真实 sympy 测试在设置了 `EG_CAS_SCRIPT`/`EG_CAS_PYTHON` 时追加执行，否则跳过（打印提示）。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eg_domain::{AnswerValue, Comparator, ModelRole, NormalizedAnswer};
use eg_subject_math::{
    CasClient, CasConfig, CasFailure, CasZeroResult, MathSubjectPack, MatrixEquivalenceComparator,
    TrigIdentityComparator, VectorEquivalenceComparator,
};

fn python() -> String {
    std::env::var("EG_CAS_PYTHON").unwrap_or_else(|_| "python3".to_owned())
}

fn stub_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/cas/stub_sidecar.py")
}

fn counter_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "eg-cas-counter-{}-{:?}.txt",
        std::process::id(),
        std::thread::current().id()
    ))
}

fn stub_config(mode: &str, timeout_ms: u64, cache: bool, counter: Option<&PathBuf>) -> CasConfig {
    let mut command = vec![
        python(),
        stub_script().to_string_lossy().into_owned(),
        mode.to_owned(),
    ];
    if let Some(counter) = counter {
        command.push(counter.to_string_lossy().into_owned());
    }
    CasConfig::new(command, Duration::from_millis(timeout_ms), cache)
}

fn answer(latex: &str) -> NormalizedAnswer {
    NormalizedAnswer {
        text: latex.to_owned(),
        latex: Some(latex.to_owned()),
        value: None,
        steps: Vec::new(),
    }
}

fn expression(latex: &str) -> AnswerValue {
    AnswerValue::Expression(latex.to_owned())
}

fn stub_counter_lines(path: &PathBuf) -> usize {
    std::fs::read_to_string(path).map_or(0, |text| text.lines().count())
}

#[test]
fn ping_and_zero_results_via_stub() {
    let client = CasClient::new(stub_config("ok", 1_000, false, None));
    let info = client.ping().unwrap();
    assert_eq!(info.version, "9.9.9-stub");
    assert_eq!(client.ensure_version().as_deref(), Some("9.9.9-stub"));
    let zero = client.simplify_zero("x+1", "x+1");
    assert!(zero.is_zero());
    assert_eq!(zero.version(), Some("9.9.9-stub"));
    // 不同表达式 → 非零且非常量
    match client.simplify_zero("x+1", "x+2") {
        CasZeroResult::NonZero { constant, .. } => assert!(!constant),
        other => panic!("期望 NonZero，实际 {other:?}"),
    }
}

#[test]
fn cache_hit_avoids_second_spawn() {
    let counter = counter_path();
    let _ = std::fs::remove_file(&counter);
    let client = CasClient::new(stub_config("ok", 1_000, true, Some(&counter)));
    let first = client.simplify_zero("x+1", "x+1");
    assert!(first.is_zero());
    let second = client.simplify_zero("x+1", "x+1");
    assert!(second.is_zero());
    assert_eq!(client.cache_len(), 1);
    // 第二次命中缓存：只启动一次 sidecar（stub 只收到一次请求）
    assert_eq!(client.stats().cache_hits, 1);
    assert_eq!(client.stats().spawns, 1);
    assert_eq!(stub_counter_lines(&counter), 1);
    let _ = std::fs::remove_file(&counter);
}

#[test]
fn timeout_kills_sidecar_and_degrades() {
    let client = CasClient::new(stub_config("hang", 150, false, None));
    let started = Instant::now();
    let result = client.simplify_zero("x", "x");
    let elapsed = started.elapsed();
    assert!(matches!(result.failure(), Some(CasFailure::Timeout { .. })));
    assert!(
        elapsed < Duration::from_secs(3),
        "超时未及时返回：{elapsed:?}"
    );
    assert!(client.stats().timeouts >= 1);
}

#[test]
fn crash_and_garbage_are_typed_failures() {
    let crash = CasClient::new(stub_config("crash", 1_000, false, None));
    let result = crash.simplify_zero("x", "x");
    assert!(matches!(
        result.failure(),
        Some(CasFailure::Crashed { .. } | CasFailure::Malformed { .. })
    ));
    assert_ne!(result.failure().unwrap().reason(), "");

    let garbage = CasClient::new(stub_config("garbage", 1_000, false, None));
    let result = garbage.simplify_zero("x", "x");
    assert!(matches!(
        result.failure(),
        Some(CasFailure::Malformed { .. })
    ));
}

#[test]
fn disabled_and_missing_command_are_unavailable() {
    let disabled = CasClient::new(CasConfig::disabled());
    let result = disabled.simplify_zero("x", "x");
    assert!(matches!(result.failure(), Some(CasFailure::Disabled)));
    assert!(disabled.engine_stamp().is_none());

    let missing = CasClient::new(CasConfig::new(
        vec!["/nonexistent/cas-sidecar".to_owned()],
        Duration::from_millis(500),
        false,
    ));
    let result = missing.simplify_zero("x", "x");
    assert!(matches!(
        result.failure(),
        Some(CasFailure::Unavailable { .. })
    ));
}

#[test]
fn engine_stamp_carries_cas_version_and_backend() {
    let client = Arc::new(CasClient::new(stub_config("ok", 1_000, false, None)));
    let stamp = client.engine_stamp().expect("CAS 可用时应有引擎指纹");
    stamp.validate().unwrap();
    assert_eq!(stamp.role, ModelRole::Judge);
    assert_eq!(stamp.model_id, "sympy");
    assert_eq!(stamp.model_version, "9.9.9-stub");
    assert_eq!(stamp.backend.as_str(), "cas_sidecar");
    assert!(stamp.prompt_hash.is_none());
    // 指纹稳定且版本变化会改变哈希
    let fingerprint = client.fingerprint();
    assert_eq!(fingerprint.version.as_deref(), Some("9.9.9-stub"));
    assert_eq!(fingerprint.protocol, "1");
    let pack = MathSubjectPack::with_cas(client);
    let engine = pack.engine_fingerprint();
    assert!(engine.cas.is_some());
    assert_ne!(engine.hash().to_hex(), "");
    assert!(pack.cas_engine_stamp().is_some());
    assert!(pack.cas_status().available);
}

#[test]
fn trig_comparator_uses_cas_when_local_rules_insufficient() {
    // 本地规则无法处理的 tan 恒等式：CAS 返回 0 → 等价
    let client = Arc::new(CasClient::new(stub_config("zero", 1_000, false, None)));
    let comparator = TrigIdentityComparator::new(Some(client.clone()));
    let verdict = comparator.compare(&expression("\\tan(x)*\\cos(x)"), &answer("\\sin(x)"));
    assert!(verdict.is_equal_like(), "{verdict:?}");
    match verdict {
        eg_domain::ComparatorVerdict::Equivalent { note } => assert!(note.contains("CAS")),
        other => panic!("期望 CAS 等价，实际 {other:?}"),
    }
    // CAS 返回非零常量 → 不相等
    let client = Arc::new(CasClient::new(stub_config("nonzero", 1_000, false, None)));
    let comparator = TrigIdentityComparator::new(Some(client));
    assert!(
        comparator
            .compare(&expression("\\tan(x)*\\cos(x)"), &answer("\\sin(x)"))
            .is_not_equal()
    );
}

#[test]
fn vector_and_matrix_use_cas_for_symbolic_components() {
    let client = Arc::new(CasClient::new(stub_config("zero", 1_000, false, None)));
    let vector = VectorEquivalenceComparator::new(Some(client.clone()));
    assert!(
        vector
            .compare(
                &expression("\\vec{(\\tan(x)*\\cos(x),1)}"),
                &answer("\\vec{(\\sin(x),1)}")
            )
            .is_equal_like()
    );
    let matrix = MatrixEquivalenceComparator::new(Some(client));
    assert!(
        matrix
            .compare(
                &expression("\\begin{pmatrix}\\tan(x)*\\cos(x)&1\\\\2&3\\end{pmatrix}"),
                &answer("\\begin{pmatrix}\\sin(x)&1\\\\2&3\\end{pmatrix}")
            )
            .is_equal_like()
    );
}

#[test]
fn cas_failure_never_produces_not_equal() {
    // 超时/崩溃时三角比较器必须「无法判定」，绝不能判「不相等」
    for mode in ["hang", "crash", "garbage"] {
        let client = Arc::new(CasClient::new(stub_config(mode, 200, false, None)));
        let comparator = TrigIdentityComparator::new(Some(client));
        let verdict = comparator.compare(&expression("\\tan(x)*\\cos(x)"), &answer("\\sin(x)"));
        assert!(
            verdict.is_undecidable(),
            "{mode} 模式下应无法判定，实际 {verdict:?}"
        );
    }
    // CAS 未注入 → 本地不足即无法判定（带原因）
    let comparator = TrigIdentityComparator::default();
    let verdict = comparator.compare(&expression("\\tan(x)*\\cos(x)"), &answer("\\sin(x)"));
    assert!(verdict.is_undecidable());
    assert!(
        verdict
            .reason()
            .is_some_and(|reason| reason.contains("CAS"))
    );
}

#[test]
fn engine_stamp_absent_when_cas_unavailable() {
    let pack = MathSubjectPack::without_cas();
    assert!(pack.cas_engine_stamp().is_none());
    assert!(!pack.cas_status().available);
    assert!(pack.cas_status().reason.is_some());
    assert!(pack.cas_stats().is_none());
}

#[test]
fn real_sympy_sidecar_when_configured() {
    // 未配置时跳过（不联网/不要求本机装 sympy）
    let Ok(script) = std::env::var("EG_CAS_SCRIPT") else {
        eprintln!("跳过真实 sympy 测试：未设置 EG_CAS_SCRIPT");
        return;
    };
    let python = std::env::var("EG_CAS_PYTHON").unwrap_or_else(|_| "python3".to_owned());
    let client = CasClient::new(CasConfig::new(
        vec![python, script],
        Duration::from_secs(3),
        true,
    ));
    match client.ping() {
        Ok(info) => {
            assert_ne!(info.version, "");
            let zero = client.simplify_zero("sin(x)**2+cos(x)**2", "1");
            assert!(zero.is_zero(), "{zero:?}");
        }
        Err(error) => eprintln!("跳过真实 sympy 测试：sidecar 不可用（{error}）"),
    }
}
