//! `egctl`：诊断、夹具回放与验收基准命令行（`SPEC.md` §7 运维与验收 · T39）。
//!
//! 子命令：
//! - `doctor`：环境自检（数据目录/DB/单实例/模型/llama.cpp/Python-uv/CAS/日志/审计/配置/模块就绪度）。
//! - `replay`：夹具回放（当前覆盖 `eg-report` 统计+导出+重放摘要；判分回放 TODO）。
//! - `bench`：基准测量（1000 份导出已实测；预处理/判分/UI 未就绪时 WARN）。
//! - `docs check`：文档门禁（复用 `scripts/docs-check.sh`）。
//! - `backup` / `restore <dir>` / `diagnostics export`：`eg-platform` 服务薄封装。
//!
//! 退出码：`0` = OK，`2` = WARN（可用但有降级/未就绪项），`1` = ERROR。
//! 约定：**不伪造 OK**——未实现的模块/检查明确输出 WARN 并写明原因。

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]
#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::format_push_string,
    clippy::doc_markdown,
    clippy::redundant_closure,
    clippy::too_many_lines,
    clippy::struct_excessive_bools,
    clippy::option_if_let_else,
    clippy::manual_let_else,
    clippy::single_match_else,
    clippy::significant_drop_tightening,
    clippy::items_after_statements,
    clippy::unreadable_literal,
    clippy::uninlined_format_args,
    clippy::needless_borrow,
    clippy::if_not_else,
    clippy::match_same_arms,
    clippy::assigning_clones,
    clippy::cast_precision_loss,
    clippy::unnecessary_wraps,
    clippy::assert_is_empty,
    unreachable_pub,
    reason = "bin crate 只能向标准输出报告结果；内部模块无外部使用者；\
              报告字符串拼接与中文术语不需要 pedantic 级别的风格改写"
)]

mod bench;
mod docs_check;
mod doctor;
mod fixture;
mod registry;
mod replay;
mod service;
mod util;

use std::path::PathBuf;
use std::process::ExitCode;

use util::{Level, exit_code};

/// 子命令。
enum Command {
    /// 帮助。
    Help,
    /// 版本。
    Version,
    /// 环境自检。
    Doctor,
    /// 夹具回放。
    Replay,
    /// 基准测量。
    Bench,
    /// 文档门禁。
    DocsCheck,
    /// 手动备份。
    Backup,
    /// 从备份目录恢复。
    Restore(PathBuf),
    /// 导出诊断包。
    DiagnosticsExport,
}

/// 解析后的参数。
struct Args {
    data_dir: Option<PathBuf>,
    json: bool,
    deep: bool,
    max_sha_mib: u64,
    fixture: Option<PathBuf>,
    out: Option<PathBuf>,
    log_limit: usize,
    command: Command,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(message) => {
            eprintln!("错误：{message}");
            ExitCode::from(exit_code(Level::Error))
        }
    }
}

fn run() -> Result<u8, String> {
    let args = parse_args()?;
    match &args.command {
        Command::Help => {
            print_help();
            Ok(0)
        }
        Command::Version => {
            println!(
                "egctl {}（exam-grading 运维/验收 CLI）",
                env!("CARGO_PKG_VERSION")
            );
            Ok(0)
        }
        Command::Doctor => run_doctor(&args),
        Command::Replay => run_replay(&args),
        Command::Bench => run_bench(&args),
        Command::DocsCheck => run_docs_check(&args),
        Command::Backup => run_backup(&args),
        Command::Restore(dir) => run_restore(&args, dir),
        Command::DiagnosticsExport => run_diagnostics(&args),
    }
}

fn run_doctor(args: &Args) -> Result<u8, String> {
    let dirs = util::resolve_dirs(args.data_dir.as_deref())?;
    let options = doctor::Options {
        dirs: &dirs,
        deep: args.deep,
        max_sha_mib: args.max_sha_mib,
    };
    let checks = doctor::diagnose(&options);
    let level = util::worst_level(&checks);
    if args.json {
        let value = serde_json::json!({
            "tool": "egctl",
            "command": "doctor",
            "version": env!("CARGO_PKG_VERSION"),
            "data_root": dirs.root().display().to_string(),
            "status": level.key(),
            "checks": checks.iter().map(util::Check::to_json).collect::<Vec<_>>(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_default()
        );
    } else {
        util::print_checks(&checks);
        println!(
            "\n结论：{}（数据根 {}）",
            level.label(),
            dirs.root().display()
        );
        println!("提示：WARN 表示可用但有未就绪/降级项；ERROR 需处理后再验收。");
    }
    Ok(exit_code(level))
}

fn run_replay(args: &Args) -> Result<u8, String> {
    let options = replay::Options {
        fixture: args.fixture.clone().or_else(replay::default_fixture_dir),
        out_dir: args.out.clone(),
    };
    let report = replay::run(&options)?;
    let level = util::worst_level(&report.checks);
    if args.json {
        let value = serde_json::json!({
            "tool": "egctl",
            "command": "replay",
            "kind": report.kind,
            "status": level.key(),
            "digest": report.digest,
            "snapshot_hash": report.snapshot_hash,
            "row_count": report.row_count,
            "files": report.files,
            "bytes": report.bytes,
            "replay_equal": report.replay_equal,
            "checks": report.checks.iter().map(util::Check::to_json).collect::<Vec<_>>(),
            "todo": report.todo,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_default()
        );
    } else {
        util::print_checks(&report.checks);
        if !report.digest.is_empty() {
            println!("\n回放类型：{}；内容摘要：{}", report.kind, report.digest);
            println!(
                "行数 {}；文件 {}；字节 {}；重放一致 {}",
                report.row_count,
                report.files,
                report.bytes,
                if report.replay_equal { "是" } else { "否" }
            );
        }
        if !report.todo.is_empty() {
            println!("\nTODO：");
            for item in &report.todo {
                println!("  - {item}");
            }
        }
    }
    Ok(exit_code(level))
}

fn run_bench(args: &Args) -> Result<u8, String> {
    let report = bench::run();
    let level = report
        .items
        .iter()
        .map(|item| item.level)
        .max()
        .unwrap_or(Level::Ok);
    if args.json {
        let value = bench::render_json(&report);
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_default()
        );
    } else {
        print!("{}", bench::render_table(&report));
        println!("\n结论：{}（未就绪项见 WARN/TODO）", level.label());
    }
    Ok(exit_code(level))
}

fn run_docs_check(args: &Args) -> Result<u8, String> {
    let report = docs_check::run()?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report.to_json()).unwrap_or_default()
        );
    } else {
        print!("{}", docs_check::render_human(&report));
    }
    Ok(exit_code(report.level()))
}

fn run_backup(args: &Args) -> Result<u8, String> {
    let dirs = util::resolve_dirs(args.data_dir.as_deref())?;
    let info = service::backup(&dirs)?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "tool": "egctl",
                "command": "backup",
                "directory": info.directory.display().to_string(),
                "manifest": info.manifest_path.display().to_string(),
                "kind": info.kind,
                "schema_version": info.schema_version,
                "database_bytes": info.database_bytes,
                "database_sha256": info.database_sha256,
            }))
            .unwrap_or_default()
        );
    } else {
        println!("{}", service::render_backup(&info));
    }
    Ok(0)
}

fn run_restore(args: &Args, backup_directory: &std::path::Path) -> Result<u8, String> {
    let dirs = util::resolve_dirs(args.data_dir.as_deref())?;
    let info = service::restore(&dirs, backup_directory)?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "tool": "egctl",
                "command": "restore",
                "backup_directory": info.backup_directory.display().to_string(),
                "target": info.target.display().to_string(),
                "safety_copy": info.safety_copy.as_ref().map(|path| path.display().to_string()),
                "integrity_ok": info.integrity_ok,
                "schema_version": info.schema_version,
            }))
            .unwrap_or_default()
        );
    } else {
        println!("{}", service::render_restore(&info));
        println!("提示：请重启桌面端并运行 `egctl doctor` 复核。");
    }
    Ok(0)
}

fn run_diagnostics(args: &Args) -> Result<u8, String> {
    let dirs = util::resolve_dirs(args.data_dir.as_deref())?;
    let info = service::diagnostics(&dirs, args.out.clone(), args.log_limit)?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "tool": "egctl",
                "command": "diagnostics export",
                "path": info.path.display().to_string(),
                "bytes": info.bytes,
                "entries": info.entries,
            }))
            .unwrap_or_default()
        );
    } else {
        println!("{}", service::render_diagnostics(&info));
    }
    Ok(0)
}

fn parse_args() -> Result<Args, String> {
    let mut data_dir = None;
    let mut json = false;
    let mut deep = false;
    let mut max_sha_mib = 64u64;
    let mut fixture = None;
    let mut out = None;
    let mut log_limit = 2000usize;
    let mut tokens: Vec<String> = Vec::new();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => json = true,
            "--deep" => deep = true,
            "--data-dir" => {
                data_dir = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| "--data-dir 需要一个目录参数".to_owned())?,
                ));
            }
            "--fixture" => {
                fixture = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| "--fixture 需要一个目录参数".to_owned())?,
                ));
            }
            "--out" => {
                out = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| "--out 需要一个目录参数".to_owned())?,
                ));
            }
            "--max-sha-mib" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--max-sha-mib 需要一个整数".to_owned())?;
                max_sha_mib = value
                    .parse()
                    .map_err(|_| format!("--max-sha-mib 非法：{value}"))?;
            }
            "--log-limit" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--log-limit 需要一个整数".to_owned())?;
                log_limit = value
                    .parse()
                    .map_err(|_| format!("--log-limit 非法：{value}"))?;
            }
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("egctl {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            other if other.starts_with("--data-dir=") => {
                data_dir = Some(PathBuf::from(
                    other.trim_start_matches("--data-dir=").to_owned(),
                ));
            }
            other => tokens.push(other.to_owned()),
        }
    }

    let command = match tokens.as_slice() {
        [] => Command::Help,
        [one] if one == "doctor" => Command::Doctor,
        [one] if one == "replay" => Command::Replay,
        [one] if one == "bench" => Command::Bench,
        [one] if one == "backup" => Command::Backup,
        [one] if one == "version" => Command::Version,
        [first, second] if first == "docs" && second == "check" => Command::DocsCheck,
        [first, second] if first == "diagnostics" && second == "export" => {
            Command::DiagnosticsExport
        }
        [first, dir] if first == "restore" => Command::Restore(PathBuf::from(dir)),
        other => {
            return Err(format!(
                "无法识别的子命令：{}（用 `egctl --help` 查看用法）",
                other.join(" ")
            ));
        }
    };
    Ok(Args {
        data_dir,
        json,
        deep,
        max_sha_mib,
        fixture,
        out,
        log_limit,
        command,
    })
}

fn print_help() {
    println!(
        "egctl {} — exam-grading 运维/验收 CLI\n\
         \n\
         用法：egctl [全局选项] <子命令> [参数]\n\
         \n\
         子命令：\n\
           doctor                 环境自检（数据目录/DB/单实例/模型/推理依赖/CAS/日志/审计/配置/模块）\n\
           replay                 夹具回放：eg-report 统计+导出+重放摘要（判分回放 TODO）\n\
           bench                  基准：1000 份导出实测；预处理/判分/UI 未就绪时 WARN\n\
           docs check             文档门禁（复用 scripts/docs-check.sh）\n\
           backup                 手动备份数据库（需桌面端退出）\n\
           restore <备份目录>     从备份恢复数据库（需桌面端退出）\n\
           diagnostics export     导出诊断包（需桌面端退出）\n\
           version / --version    版本\n\
         \n\
         全局选项：\n\
           --json                 结构化输出（所有子命令）\n\
           --data-dir <目录>      覆盖数据根（默认按平台规则解析）\n\
           --deep                 doctor 额外执行 cargo check 检查其他 crate 编译状态\n\
           --max-sha-mib <MiB>    doctor 权重全量 SHA-256 阈值（默认 64）\n\
           --fixture <目录>       replay 夹具目录（含 fixture.json）\n\
           --out <目录>           replay 保留导出 / diagnostics 输出目录\n\
           --log-limit <条数>     diagnostics 日志条数上限（默认 2000）\n\
         \n\
         退出码：0 = OK，2 = WARN（有降级/未就绪项），1 = ERROR",
        env!("CARGO_PKG_VERSION")
    );
}
