//! 导出端到端测试（`SPEC-report.md` §7/§9：命名、三格式、脱敏、原子性、审计、重放）。

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

use common::{Scenario, temp_dir};
use eg_report::{
    ExportFormat, ExportRequest, MemoryAuditor, Report, ReportSettings, export, verify_replay,
};
use std::fs;
use std::path::Path;

fn scenario_report() -> (common::Scenario, Report) {
    let scenario = Scenario::build();
    let report = Report::build(&scenario.input, ReportSettings::default()).unwrap();
    (scenario, report)
}

fn read(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap()
}

fn replay_row_count(dir: &Path, report: &Report, exported_at: eg_domain::Timestamp) -> usize {
    let outcome = export(
        report,
        &ExportRequest::new(dir.join("row-count"), exported_at),
        &MemoryAuditor::new(),
    )
    .unwrap();
    outcome.row_count
}

#[test]
fn exports_three_formats_atomically() {
    let (scenario, report) = scenario_report();
    let dir = temp_dir("three");
    let request = ExportRequest::new(&dir, scenario.exported_at);
    let auditor = MemoryAuditor::new();
    let outcome = export(&report, &request, &auditor).unwrap();

    // 1 个 XLSX + 8 个 CSV + 1 个 HTML。
    assert_eq!(outcome.files.len(), 10);
    assert_eq!(outcome.formats, ExportFormat::all().to_vec());
    assert!(outcome.files.iter().all(|file| file.path.exists()));
    assert!(outcome.files.iter().all(|file| file.bytes > 0));

    let names: Vec<&str> = outcome
        .files
        .iter()
        .map(|file| file.file_name.as_str())
        .collect();
    let base = "期中_数学_2026_全部_20261101-193000";
    assert!(names.contains(&format!("{base}.xlsx").as_str()));
    assert!(names.contains(&format!("{base}.html").as_str()));
    for sheet in [
        "元信息",
        "班级汇总",
        "成绩单",
        "题目统计",
        "选项分布",
        "错题标签",
        "班级分布",
        "快速抽检",
    ] {
        assert!(
            names.contains(&format!("{base}_{sheet}.csv").as_str()),
            "缺少 CSV：{sheet}"
        );
    }

    // CSV：UTF-8 BOM + CRLF；HTML：无外部引用。
    let csv = outcome
        .files
        .iter()
        .find(|file| file.format == ExportFormat::Csv && file.file_name.contains("成绩单"))
        .unwrap();
    let csv_bytes = read(&csv.path);
    assert_eq!(&csv_bytes[..3], b"\xEF\xBB\xBF");
    let csv_text = String::from_utf8_lossy(&csv_bytes[3..]);
    assert!(csv_text.contains("\r\n"));
    assert!(csv_text.contains("序号,学号,姓名,模式,状态"));
    assert!(csv_text.contains("得分_milli"));
    assert!(csv_text.contains("得分率_ppm"));

    let html = outcome
        .files
        .iter()
        .find(|file| file.format == ExportFormat::Html)
        .unwrap();
    let html_text = String::from_utf8(read(&html.path)).unwrap();
    assert!(!html_text.contains("http://"));
    assert!(!html_text.contains("https://"));
    assert!(!html_text.contains("<script"));
    assert!(html_text.contains("成绩单"));
    assert!(html_text.contains("抽样方法"));

    // 无临时文件残留。
    let leftovers: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp"))
        .collect();
    assert!(leftovers.is_empty());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn replay_is_deterministic_and_audited() {
    let (scenario, report) = scenario_report();
    let dir_a = temp_dir("replay-a");
    let dir_b = temp_dir("replay-b");
    let auditor = MemoryAuditor::new();
    let first = export(
        &report,
        &ExportRequest::new(&dir_a, scenario.exported_at),
        &auditor,
    )
    .unwrap();
    // 规范 §9.9：同一快照重放 10 次内容摘要全等。
    let mut digests = Vec::new();
    for index in 0..10 {
        let dir = dir_b.join(format!("replay-{index}"));
        let replay = export(
            &report,
            &ExportRequest::new(&dir, scenario.exported_at),
            &auditor,
        )
        .unwrap();
        verify_replay(&first, &replay).unwrap();
        digests.push(replay.content_digest);
    }
    assert!(digests.iter().all(|digest| *digest == first.content_digest));
    assert_eq!(
        first.row_count,
        replay_row_count(&dir_b, &report, scenario.exported_at)
    );

    // 审计：每次导出一条，actor 固定、含快照哈希与计数，不含学生原文。
    let events = auditor.events().unwrap();
    assert_eq!(events.len(), 11);
    for event in &events {
        assert_eq!(event.actor, "local-teacher");
        assert_eq!(event.action, "report.export");
        assert_eq!(event.snapshot_hash, report.snapshot_hash);
        assert_eq!(event.policy_version, report.settings.policy_version);
        assert_eq!(event.naming_rule_version, 1);
        assert_eq!(event.file_count, 10);
        assert_eq!(event.formats.len(), 3);
        assert!(!event.note.contains("张三"));
        assert!(!event.note.contains("20240001"));
    }
    let _ = fs::remove_dir_all(&dir_a);
    let _ = fs::remove_dir_all(&dir_b);
}

#[test]
fn masked_export_contains_no_student_identity() {
    let (scenario, report) = scenario_report();
    let dir = temp_dir("masked");
    let mut request = ExportRequest::new(&dir, scenario.exported_at);
    request.masked = true;
    let auditor = MemoryAuditor::new();
    let outcome = export(&report, &request, &auditor).unwrap();

    // 文件名带脱敏后缀。
    assert!(
        outcome
            .files
            .iter()
            .all(|file| file.file_name.contains("_脱敏"))
    );
    // CSV/HTML 可见文本中不得出现真名与真学号。
    let secrets = [
        "张三", "李四", "王五", "赵六", "考生07", "20240001", "20240002",
    ];
    for file in &outcome.files {
        if file.format == ExportFormat::Xlsx {
            continue;
        }
        let bytes = read(&file.path);
        let text = String::from_utf8_lossy(&bytes);
        for secret in secrets {
            assert!(
                !text.contains(secret),
                "{} 命中敏感原文 {secret}",
                file.file_name
            );
        }
    }
    // 掩码名出现在成绩单 CSV。
    let masked_csv = outcome
        .files
        .iter()
        .find(|file| file.format == ExportFormat::Csv && file.file_name.contains("成绩单"))
        .unwrap();
    let masked_bytes = read(&masked_csv.path);
    let text = String::from_utf8_lossy(&masked_bytes);
    assert!(text.contains("学生01"));
    assert!(text.contains("S-001"));
    // 审计记录脱敏开关。
    assert!(auditor.events().unwrap()[0].masked);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn refuses_overwrite_and_rolls_back_on_audit_failure() {
    let (scenario, report) = scenario_report();
    let dir = temp_dir("atomic");
    let request = ExportRequest::new(&dir, scenario.exported_at);
    export(&report, &request, &MemoryAuditor::new()).unwrap();
    // 默认拒绝覆盖。
    let error = export(&report, &request, &MemoryAuditor::new()).unwrap_err();
    assert!(matches!(error, eg_report::ReportError::OverwriteRefused(_)));
    // 显式允许覆盖。
    let mut overwrite = request.clone();
    overwrite.allow_overwrite = true;
    export(&report, &overwrite, &MemoryAuditor::new()).unwrap();

    // 审计失败 → 回滚，目录内无本次文件。
    let dir2 = temp_dir("audit-fail");
    let failing = MemoryAuditor::failing();
    let error = export(
        &report,
        &ExportRequest::new(&dir2, scenario.exported_at),
        &failing,
    )
    .unwrap_err();
    assert!(matches!(error, eg_report::ReportError::Audit(_)));
    let remaining: Vec<_> = fs::read_dir(&dir2)
        .map(|entries| entries.filter_map(Result::ok).collect())
        .unwrap_or_default();
    assert!(
        remaining.is_empty(),
        "审计失败后不应残留文件：{}",
        remaining.len()
    );
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&dir2);
}

#[test]
fn draft_export_is_marked() {
    let (scenario, report) = scenario_report();
    let dir = temp_dir("draft");
    let mut request = ExportRequest::new(&dir, scenario.exported_at);
    request.draft = true;
    let outcome = export(&report, &request, &MemoryAuditor::new()).unwrap();
    assert!(
        outcome
            .files
            .iter()
            .all(|file| file.file_name.contains("_草稿"))
    );
    let meta = outcome
        .files
        .iter()
        .find(|file| file.format == ExportFormat::Csv && file.file_name.contains("元信息"))
        .unwrap();
    let bytes = read(&meta.path);
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("草稿"));
    assert!(text.contains("20.0%"));
    assert!(text.contains("0.70"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn fast_export_label_off_omits_word_from_file_name() {
    let (scenario, _base_report) = scenario_report();
    let dir = temp_dir("fast-label");
    let settings = ReportSettings {
        fast_export_label: false,
        ..ReportSettings::default()
    };
    let report = Report::build(&scenario.input, settings).unwrap();
    let mut request = ExportRequest::new(&dir, scenario.exported_at);
    request.mode_token = eg_report::ModeToken::Fast;
    let outcome = export(&report, &request, &MemoryAuditor::new()).unwrap();
    assert!(
        outcome
            .files
            .iter()
            .all(|file| !file.file_name.contains("_快速_"))
    );
    // 元信息仍须标注快速模式开关与抽检说明（模式可分辨不可关闭）。
    let meta = outcome
        .files
        .iter()
        .find(|file| file.format == ExportFormat::Csv && file.file_name.contains("元信息"))
        .unwrap();
    let bytes = read(&meta.path);
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("快速"));
    let _ = fs::remove_dir_all(&dir);
}
