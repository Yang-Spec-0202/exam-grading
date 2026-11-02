//! `egctl replay`：夹具回放（当前可用：`eg-report` 统计+导出+重放摘要；判分链路 TODO）。
//!
//! 回放定义（`SPEC-report.md` §3.1/§9.9）：同一快照、同一批次时间重复构建与导出，
//! 内容摘要必须逐字节一致。判分回放依赖 `eg-grade`（并行收尾中），未就绪时**不伪造**
//! 通过，而是输出 WARN + TODO 列表。

use std::path::PathBuf;

use serde::Deserialize;

use crate::fixture;
use crate::util::{Check, Level, human_bytes, repo_root};

/// 回放选项。
#[derive(Debug, Default)]
pub struct Options {
    /// 夹具目录（内含 `fixture.json`）；为空时使用内置报表夹具。
    pub fixture: Option<PathBuf>,
    /// 输出目录（为空时使用系统临时目录并在结束后清理）。
    pub out_dir: Option<PathBuf>,
}

/// 回放结果。
#[derive(Debug)]
pub struct ReplayReport {
    /// 回放类型（report / grade / ...）。
    pub kind: String,
    /// 内容摘要（十六进制）。
    pub digest: String,
    /// 快照哈希（十六进制）。
    pub snapshot_hash: String,
    /// 行数。
    pub row_count: usize,
    /// 文件数。
    pub files: usize,
    /// 字节数。
    pub bytes: u64,
    /// 两次运行摘要是否一致。
    pub replay_equal: bool,
    /// 检查项。
    pub checks: Vec<Check>,
    /// 未完成项（TODO）。
    pub todo: Vec<String>,
}

/// 夹具清单（`fixture.json`；当前仅支持报表夹具）。
#[derive(Debug, Deserialize)]
struct FixtureManifest {
    kind: String,
    #[serde(default = "default_students")]
    students: usize,
    #[serde(default = "default_questions")]
    questions: usize,
    #[serde(default = "default_fast_every")]
    fast_every: usize,
}

fn default_students() -> usize {
    60
}

fn default_questions() -> usize {
    20
}

fn default_fast_every() -> usize {
    5
}

/// 执行回放。
///
/// # Errors
///
/// 夹具缺失/损坏、导出失败或读取 `fixture.json` 失败时返回中文错误。
pub fn run(options: &Options) -> std::result::Result<ReplayReport, String> {
    let mut todo = Vec::new();
    let (students, questions, fast_every, kind) = match &options.fixture {
        Some(dir) => {
            let manifest_path = dir.join("fixture.json");
            if !manifest_path.is_file() {
                return Err(format!(
                    "夹具目录缺少 fixture.json：{}（参考 docs/dev/research/egctl.md 的夹具格式）",
                    manifest_path.display()
                ));
            }
            let text = std::fs::read_to_string(&manifest_path)
                .map_err(|error| format!("读取夹具清单失败：{error}"))?;
            let manifest: FixtureManifest = serde_json::from_str(&text)
                .map_err(|error| format!("解析夹具清单失败：{error}"))?;
            (
                manifest.students.max(1),
                manifest.questions.max(1),
                manifest.fast_every,
                manifest.kind,
            )
        }
        None => (60, 20, 5, "report".to_owned()),
    };

    if kind != "report" {
        todo.push(format!(
            "{kind} 回放依赖 eg-grade/eg-capture（并行收尾中，未接入 egctl）"
        ));
        let checks = vec![Check::new(
            "replay_kind",
            "回放类型",
            Level::Warn,
            format!(
                "当前仅实现 report 回放；`{kind}` 回放未就绪（TODO：接入 eg-grade 判分流水线）"
            ),
        )];
        return Ok(ReplayReport {
            kind,
            digest: String::new(),
            snapshot_hash: String::new(),
            row_count: 0,
            files: 0,
            bytes: 0,
            replay_equal: false,
            checks,
            todo,
        });
    }

    let input = fixture::report_input(students, questions, fast_every);
    let report = fixture::build_report(&input)?;
    let base = options.out_dir.clone().unwrap_or_else(|| {
        std::env::temp_dir().join(format!("egctl-replay-{}", std::process::id()))
    });
    let dir_a = base.join("run-a");
    let dir_b = base.join("run-b");
    let first = fixture::export_report(&report, &dir_a, fixture::fixture_time(), false)?;
    let second = fixture::export_report(&report, &dir_b, fixture::fixture_time(), false)?;
    let replay_equal = first.content_digest == second.content_digest
        && eg_report::verify_replay(&first, &second).is_ok();
    let bytes: u64 = first.files.iter().map(|file| file.bytes).sum();
    let mut checks = vec![
        Check::new(
            "replay_build",
            "回放构建",
            Level::Ok,
            format!(
                "{students} 名学生 × {questions} 题；快照哈希 {}",
                &report.snapshot_hash.to_hex()[..12]
            ),
        ),
        Check::new(
            "replay_export",
            "导出运行",
            Level::Ok,
            format!(
                "{} 个文件（{}）；摘要 {}",
                first.files.len(),
                human_bytes(bytes),
                &first.content_digest.to_hex()[..12]
            ),
        ),
        Check::new(
            "replay_equal",
            "重放一致性",
            if replay_equal {
                Level::Ok
            } else {
                Level::Error
            },
            if replay_equal {
                "两次运行内容摘要相等（可见内容一致）".to_owned()
            } else {
                "两次运行内容摘要不一致".to_owned()
            },
        ),
    ];
    todo.push("判分回放（eg-grade → GradeRecord 夹具）未接入；见任务 TODO".to_owned());
    todo.push("资产/证据回放（原图 + CAS 指纹）未接入".to_owned());
    checks.push(Check::new(
        "replay_grade",
        "判分回放",
        Level::Warn,
        "未就绪：eg-grade 未通过编译，回放只覆盖 report（统计+导出+重放摘要）",
    ));
    checks.push(Check::new(
        "replay_assets",
        "资产回放",
        Level::Warn,
        "未就绪：原图/CAS 资产指纹回放待 capture/align 接入",
    ));

    if options.out_dir.is_none() {
        let _ = std::fs::remove_dir_all(&base);
    }
    Ok(ReplayReport {
        kind: "report".to_owned(),
        digest: first.content_digest.to_hex(),
        snapshot_hash: report.snapshot_hash.to_hex(),
        row_count: first.row_count,
        files: first.files.len(),
        bytes,
        replay_equal,
        checks,
        todo,
    })
}

/// 自动发现的仓库夹具目录（开发环境：`tests/fixtures/egctl`）。
#[must_use]
pub fn default_fixture_dir() -> Option<PathBuf> {
    let candidate = repo_root().join("tests/fixtures/egctl");
    candidate.is_dir().then_some(candidate)
}
