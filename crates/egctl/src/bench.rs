//! `egctl bench`：基准测量（单页预处理、单题判分、1000 份导出、UI 启动）。
//!
//! 已实现：`eg-report` 的 1000 份 × 20 题统计构建与三格式导出实测（规范预算：单 XLSX ≤8s、
//! 三格式合计 ≤15s，M4/16GB）。未接入的链路（capture/grade/app）输出 WARN + TODO，
//! 不给出伪造数字。

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::fixture;
use crate::util::{Level, human_bytes, repo_root};

/// 单项基准结果。
#[derive(Debug, Clone)]
pub struct BenchItem {
    /// 稳定 id。
    pub id: &'static str,
    /// 标题。
    pub title: &'static str,
    /// 级别（Ok = 已实测；Warn = 未就绪）。
    pub level: Level,
    /// 关键指标（人类可读）。
    pub metric: String,
    /// 备注/原因。
    pub note: String,
}

/// 基准报告。
#[derive(Debug, Clone)]
pub struct BenchReport {
    /// 测量时间（RFC3339）。
    pub measured_at: String,
    /// 机器/版本摘要。
    pub environment: String,
    /// 各项结果。
    pub items: Vec<BenchItem>,
}

/// 执行基准测量。
#[must_use]
pub fn run() -> BenchReport {
    let mut items = Vec::new();
    items.push(bench_report_export());
    items.push(BenchItem {
        id: "preprocess_page",
        title: "单页预处理（capture）",
        level: Level::Warn,
        metric: "未测量".to_owned(),
        note: "eg-capture 未接入 egctl（并行开发中，TODO：拆页/旋正/去噪链路就绪后接入）"
            .to_owned(),
    });
    items.push(BenchItem {
        id: "judge_question",
        title: "单题判分（grade）",
        level: Level::Warn,
        metric: "未测量".to_owned(),
        note: "eg-grade 当前未通过编译（并行收尾），TODO：判分流水线就绪后测量单题 P50/P95"
            .to_owned(),
    });
    items.push(bench_ui_startup());
    BenchReport {
        measured_at: crate::util::now().to_rfc3339(),
        environment: format!(
            "{} {} · egctl {} · {}",
            std::env::consts::OS,
            std::env::consts::ARCH,
            env!("CARGO_PKG_VERSION"),
            if cfg!(debug_assertions) {
                "debug 构建（数字仅供冒烟）"
            } else {
                "release 构建"
            }
        ),
        items,
    }
}

fn bench_report_export() -> BenchItem {
    let input = fixture::report_input(1000, 20, 0);
    let started = Instant::now();
    let report = match fixture::build_report(&input) {
        Ok(report) => report,
        Err(error) => {
            return BenchItem {
                id: "export_1000",
                title: "1000 份 × 20 题导出（report）",
                level: Level::Error,
                metric: "失败".to_owned(),
                note: format!("构建报表失败：{error}"),
            };
        }
    };
    let build = started.elapsed();
    let base = std::env::temp_dir().join(format!("egctl-bench-{}", std::process::id()));
    let xlsx_dir = base.join("xlsx");
    let all_dir = base.join("all");

    let xlsx_started = Instant::now();
    let xlsx_result = export_single(&report, &xlsx_dir, eg_report::ExportFormat::Xlsx);
    let xlsx_time = xlsx_started.elapsed();
    let all_started = Instant::now();
    let all_result = export_all(&report, &all_dir);
    let all_time = all_started.elapsed();
    let _ = std::fs::remove_dir_all(&base);

    match (xlsx_result, all_result) {
        (Ok(xlsx), Ok(all)) => {
            let xlsx_bytes: u64 = xlsx.files.iter().map(|file| file.bytes).sum();
            let all_bytes: u64 = all.files.iter().map(|file| file.bytes).sum();
            let within = xlsx_time <= Duration::from_secs(8) && all_time <= Duration::from_secs(15);
            BenchItem {
                id: "export_1000",
                title: "1000 份 × 20 题导出（report）",
                level: if within || cfg!(debug_assertions) {
                    Level::Ok
                } else {
                    Level::Warn
                },
                metric: format!(
                    "统计构建 {} ms；单 XLSX {} ms（{}）；三格式 {} ms（{} 个文件 / {} / {} 行）",
                    build.as_millis(),
                    xlsx_time.as_millis(),
                    human_bytes(xlsx_bytes),
                    all_time.as_millis(),
                    all.files.len(),
                    human_bytes(all_bytes),
                    all.row_count
                ),
                note: if cfg!(debug_assertions) {
                    "debug 构建，仅作冒烟；预算以 release 实测为准".to_owned()
                } else {
                    format!(
                        "预算：单 XLSX ≤8s、三格式 ≤15s（实测 {}）",
                        if within { "达标" } else { "超预算" }
                    )
                },
            }
        }
        (Err(error), _) | (_, Err(error)) => BenchItem {
            id: "export_1000",
            title: "1000 份 × 20 题导出（report）",
            level: Level::Error,
            metric: "失败".to_owned(),
            note: error,
        },
    }
}

fn export_single(
    report: &eg_report::Report,
    dir: &std::path::Path,
    format: eg_report::ExportFormat,
) -> std::result::Result<eg_report::ExportOutcome, String> {
    let mut request = eg_report::ExportRequest::new(dir, fixture::fixture_time());
    request.formats = vec![format];
    eg_report::export(report, &request, &eg_report::MemoryAuditor::new())
        .map_err(|error| error.to_string())
}

fn export_all(
    report: &eg_report::Report,
    dir: &std::path::Path,
) -> std::result::Result<eg_report::ExportOutcome, String> {
    fixture::export_report(report, dir, fixture::fixture_time(), false)
}

/// 查找已构建的 `eg-app` 可执行文件（release 优先）。
#[must_use]
pub fn app_binary() -> Option<std::path::PathBuf> {
    ["release", "debug"]
        .iter()
        .map(|profile| repo_root().join(format!("target/{profile}/eg-app")))
        .find(|path| path.is_file())
}

fn bench_ui_startup() -> BenchItem {
    let binary = app_binary();
    let Some(binary) = binary else {
        return BenchItem {
            id: "ui_startup",
            title: "UI 启动（eg-app）",
            level: Level::Warn,
            metric: "未测量".to_owned(),
            note:
                "未找到 target/{debug,release}/eg-app 可执行文件（eg-app 未构建或未接入 --bench）"
                    .to_owned(),
        };
    };
    let started = Instant::now();
    match run_with_timeout(&binary, &["--bench"], Duration::from_secs(20)) {
        Some(output) => {
            let elapsed = started.elapsed();
            let stdout = String::from_utf8_lossy(&output.stdout);
            let bench_lines: Vec<&str> = stdout
                .lines()
                .filter(|line| line.starts_with("[bench]"))
                .collect();
            if bench_lines.is_empty() {
                return BenchItem {
                    id: "ui_startup",
                    title: "UI 启动（eg-app）",
                    level: Level::Warn,
                    metric: format!(
                        "{} ms（退出码 {:?}）",
                        elapsed.as_millis(),
                        output.status.code()
                    ),
                    note: format!(
                        "eg-app 未实现 --bench 或未输出基准数据（{}）",
                        binary.display()
                    ),
                };
            }
            let first_frame = extract_number(&stdout, "first_frame").unwrap_or(0.0);
            let nav_p95 = extract_number(&stdout, "p95").unwrap_or(0.0);
            let failures = extract_integer(&stdout, "invoke_failures").unwrap_or(0);
            let ok = output.status.success();
            BenchItem {
                id: "ui_startup",
                title: "UI 启动（eg-app）",
                level: if ok { Level::Ok } else { Level::Warn },
                metric: format!(
                    "首帧 {first_frame:.1} ms；切页 P95 {nav_p95:.3} ms；命令失败 {failures}；进程耗时 {} ms",
                    elapsed.as_millis()
                ),
                note: if ok {
                    let profile = if binary.to_string_lossy().contains("/release/") {
                        "release"
                    } else {
                        "debug"
                    };
                    format!("来自 eg-app --bench（{profile} 二进制；启动分位/切页/事件/命令）")
                } else {
                    "eg-app --bench 退出码非 0（可能存在未达标项，见桌面端输出）".to_owned()
                },
            }
        }
        None => BenchItem {
            id: "ui_startup",
            title: "UI 启动（eg-app）",
            level: Level::Warn,
            metric: "未测量".to_owned(),
            note: format!(
                "无法执行 {}（eg-app 未构建、未实现 --bench 或无 GUI 权限）",
                binary.display()
            ),
        },
    }
}

/// 从 `[bench]` 输出中取第一个 `"key":数字`。
fn extract_number(text: &str, key: &str) -> Option<f64> {
    let needle = format!("\"{key}\":");
    let start = text.find(&needle)? + needle.len();
    let rest = text.get(start..)?;
    let number: String = rest
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || *ch == '.')
        .collect();
    number.parse().ok()
}

/// 从 `[bench]` 输出中取第一个 `key=整数`。
fn extract_integer(text: &str, key: &str) -> Option<u64> {
    let needle = format!("{key}=");
    let start = text.find(&needle)? + needle.len();
    let rest = text.get(start..)?;
    let number: String = rest.chars().take_while(char::is_ascii_digit).collect();
    number.parse().ok()
}

fn run_with_timeout(
    program: &std::path::Path,
    args: &[&str],
    timeout: Duration,
) -> Option<std::process::Output> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    return child.wait_with_output().ok();
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => return None,
        }
    }
}

/// 人类可读的基准汇总（表格）。
#[must_use]
pub fn render_table(report: &BenchReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "基准时间：{}\n环境：{}\n\n",
        report.measured_at, report.environment
    ));
    for item in &report.items {
        out.push_str(&format!(
            "[{}] {:<22} {}\n       {}\n",
            item.level.label(),
            item.id,
            item.title,
            item.metric
        ));
        if !item.note.is_empty() {
            out.push_str(&format!("       备注：{}\n", item.note));
        }
    }
    out
}

/// 结果 JSON。
#[must_use]
pub fn render_json(report: &BenchReport) -> serde_json::Value {
    serde_json::json!({
        "measured_at": report.measured_at,
        "environment": report.environment,
        "items": report.items.iter().map(|item| serde_json::json!({
            "id": item.id,
            "title": item.title,
            "status": item.level.key(),
            "metric": item.metric,
            "note": item.note,
        })).collect::<Vec<_>>(),
    })
}
