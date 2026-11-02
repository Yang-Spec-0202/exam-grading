//! 导出性能预算（`SPEC-report.md` §3.4 性能预算）：1000 份提交 × 20 题。
//!
//! 默认忽略；用 `cargo test -p eg-report --release --test perf -- --ignored --nocapture` 实测。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::assert_is_empty,
    clippy::field_reassign_with_default,
    clippy::redundant_closure
)]

mod common;

use eg_report::{ExportFormat, ExportRequest, MemoryAuditor, Report, ReportSettings, export};
use std::time::Instant;

#[test]
#[ignore = "性能预算测试：--release --ignored 运行，记录实测耗时"]
fn thousand_submissions_export_within_budget() {
    let input = common::synthetic_input(1000, 20);

    let started = Instant::now();
    let report = Report::build(&input, ReportSettings::default()).unwrap();
    let build_time = started.elapsed();

    // 单 XLSX
    let xlsx_dir = common::temp_dir("perf-xlsx");
    let mut xlsx_request = ExportRequest::new(&xlsx_dir, common::timestamp());
    xlsx_request.formats = vec![ExportFormat::Xlsx];
    let xlsx_started = Instant::now();
    let xlsx_outcome = export(&report, &xlsx_request, &MemoryAuditor::new()).unwrap();
    let xlsx_time = xlsx_started.elapsed();

    // 三格式合计
    let all_dir = common::temp_dir("perf-all");
    let all_request = ExportRequest::new(&all_dir, common::timestamp());
    let all_started = Instant::now();
    let outcome = export(&report, &all_request, &MemoryAuditor::new()).unwrap();
    let all_time = all_started.elapsed();

    let total_bytes: u64 = outcome.files.iter().map(|file| file.bytes).sum();
    println!(
        "1000 份 × 20 题：统计构建 {build_time:?}；XLSX {xlsx_time:?}（{} 字节）；\
         三格式合计 {all_time:?}（{} 个文件，{total_bytes} 字节，{} 行）",
        xlsx_outcome
            .files
            .iter()
            .map(|file| file.bytes)
            .sum::<u64>(),
        outcome.files.len(),
        outcome.row_count
    );
    assert_eq!(xlsx_outcome.files.len(), 1);
    assert_eq!(outcome.files.len(), 10);

    // 目标：单 XLSX ≤ 8s、三格式合计 ≤ 15s（debug 构建放宽，仅供本地冒烟）。
    let (xlsx_limit, all_limit) = if cfg!(debug_assertions) {
        (120.0, 180.0)
    } else {
        (8.0, 15.0)
    };
    assert!(
        xlsx_time.as_secs_f64() <= xlsx_limit,
        "单 XLSX 超预算：{xlsx_time:?} > {xlsx_limit}s"
    );
    assert!(
        all_time.as_secs_f64() <= all_limit,
        "三格式合计超预算：{all_time:?} > {all_limit}s"
    );
    let _ = std::fs::remove_dir_all(&xlsx_dir);
    let _ = std::fs::remove_dir_all(&all_dir);
}
