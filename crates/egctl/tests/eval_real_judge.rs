//! 真实 4B judge 的 S3 复测（需本地 llama-server；默认忽略，不阻塞普通 CI）。
//!
//! 运行：`cargo test -p egctl --test eval_real_judge -- --ignored --nocapture`

#![allow(clippy::unwrap_used, clippy::expect_used)]

use egctl::eval_real::{Options, run};

#[test]
#[ignore = "需要本地 judge http://127.0.0.1:8082/v1；普通 CI 不依赖服务"]
fn real_judge_smoke_reports_metrics() {
    let report = run(&Options {
        gold: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/gold"),
        sample: Some(1),
        concurrency: 1,
        ..Options::default()
    })
    .expect("真实 judge 评估应成功");
    assert_eq!(report.counts["selected"], 1);
    assert_eq!(
        report.counts["success"], 1,
        "样本应成功（失败分类见 report）"
    );
    assert!(report.metrics["item_total"].as_u64().unwrap_or(0) > 0);
    assert!(report.latency["calls"].as_u64().unwrap_or(0) > 0);
}
