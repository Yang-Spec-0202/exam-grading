//! S1 端到端自动化验收的 CI 集成测试（不依赖真实模型/网络/桌面）。
//!
//! 运行：`cargo test -p egctl --test acceptance`

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use egctl::acceptance::{Options, run};

fn temp_dir() -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    std::env::temp_dir().join(format!("egctl-acceptance-test-{nanos}"))
}

#[test]
fn acceptance_report_passes_with_expected_coverage() {
    let data_root = temp_dir();
    // 仓库级夹具落盘后默认使用外部夹具校验；缺失时退回内置夹具。
    let fixtures_root =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/paper-20q");
    let fixtures = fixtures_root.is_dir().then_some(fixtures_root);
    let report = run(&Options {
        fixtures,
        data_dir: Some(data_root.clone()),
        out: None,
    });
    let human = report.render_human();
    assert!(report.passed, "acceptance 必须 PASS：\n{human}");
    assert!(
        report.assertions >= 280,
        "断言数不足（{}；S1 外部 expected 全量接入后期望 ≥280）\n{human}",
        report.assertions
    );
    assert!(
        report.negatives.len() >= 4,
        "负向用例不足（{:?}）\n{human}",
        report.negatives
    );
    for required in [
        "negative_malformed_ocr",
        "negative_unresolved_identity",
        "negative_low_confidence_review",
        "negative_multi_choice_partial_credit",
        "negative_sampled_readonly",
    ] {
        assert!(
            report.negatives.iter().any(|name| name == required),
            "缺少负向用例 {required}：{:?}",
            report.negatives
        );
    }
    for step in [
        "env",
        "capture",
        "recognize",
        "align",
        "grade",
        "review",
        "report",
        "external-capture",
        "external-recognize",
        "external-align",
        "external-grade",
    ] {
        assert!(
            report.steps.iter().any(|item| item.id == step),
            "缺少步骤 {step}"
        );
    }
    assert!(
        report.failures.is_empty(),
        "失败断言：{:?}",
        report.failures
    );
    let external_grade = report
        .steps
        .iter()
        .find(|step| step.id == "external-grade")
        .expect("缺少 external-grade 步骤");
    assert_eq!(external_grade.status, "PASS");
    assert!(
        external_grade.assertions >= 180,
        "外部逐题断言不足（{}）",
        external_grade.assertions
    );
    let json = report.to_json();
    assert_eq!(json["status"], "pass");
    assert!(json["assertions"].as_u64().unwrap_or(0) >= 60);
    let _ = std::fs::remove_dir_all(&data_root);
}
