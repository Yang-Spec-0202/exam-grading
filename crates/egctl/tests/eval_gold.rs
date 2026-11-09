//! S2/S3/S5 合成金标 evaluator 的 CI 子集测试（客观 60 + 解答 30，快速；全量用 CLI）。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use egctl::eval::{Options, THRESHOLDS};

#[test]
fn eval_gold_subset_reports_s2_s3_s5_metrics() {
    let gold = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/gold");
    let report = egctl::eval::run(&Options {
        gold,
        max_objective: Some(60),
        max_solution: Some(30),
    })
    .expect("gold 评估应成功");
    assert_eq!(report.total_items, 90, "CI 子集 = 60 客观 + 30 解答");
    assert_eq!(report.objective.stats.total(), 60);
    assert_eq!(report.solution.stats.total(), 30);
    assert_eq!(THRESHOLDS.len(), 5);

    // S5：L1 规则路由（合成金标 expected 由规范推导）
    let route = f64::from(
        u32::try_from(report.objective.stats.route_ok() + report.solution.stats.route_ok())
            .unwrap(),
    ) / f64::from(u32::try_from(report.total_items).unwrap());
    assert!(route >= 0.98, "S5 路由准确率应 ≥98%，实际 {route}");

    // S2：客观题规则分精确一致 + 不得误给 0
    let s2 = f64::from(u32::try_from(report.objective.stats.score_ok()).unwrap())
        / f64::from(u32::try_from(report.objective.stats.score_total()).unwrap());
    assert!(s2 >= 0.95, "S2 分数精确一致率过低：{s2}");
    assert_eq!(report.objective.stats.false_zero(), 0);

    // S3：解答题聚合/分步/状态
    let s3 = f64::from(u32::try_from(report.solution.stats.score_ok()).unwrap())
        / f64::from(u32::try_from(report.solution.stats.score_total()).unwrap());
    assert!(s3 >= 0.85, "S3 分数精确一致率应 ≥85%，实际 {s3}");
    assert_eq!(report.solution.stats.false_zero(), 0);
    assert_eq!(report.solution.stats.step_total(), 90);
    assert_eq!(report.solution.stats.step_ok(), 90);
    assert!(
        !report
            .solution
            .failures
            .iter()
            .any(|failure| failure.dimension == "score_mismatch")
    );

    // JSON 契约：机器可读指标/分组/失败分类
    let json = report.to_json();
    assert_eq!(json["command"], "eval");
    assert_eq!(json["total_items"], 90);
    assert!(json["metrics"]["s2"]["score_exact"].as_f64().unwrap_or(0.0) >= 0.95);
    assert!(json["metrics"]["s3"]["score_exact"].as_f64().unwrap_or(0.0) >= 0.85);
    assert_eq!(json["metrics"]["s2"]["false_zero"], 0);
    assert!(
        json["objective"]["by_kind"]
            .as_object()
            .is_some_and(|map| !map.is_empty())
    );
    assert!(
        json["objective"]["by_perturbation"]
            .as_object()
            .is_some_and(|map| !map.is_empty())
    );
    assert!(
        json["solution"]["by_status"]
            .as_object()
            .is_some_and(|map| !map.is_empty())
    );
}
