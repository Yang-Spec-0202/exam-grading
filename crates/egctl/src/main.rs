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

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::unnecessary_wraps,
    reason = "CLI 入口需要向标准输出报告结果；子命令统一返回 Result<u8, String> 便于分发"
)]

use egctl::{bench, docs_check, doctor, eval, eval_real, replay, service, util};
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
    /// S1 端到端自动化验收。
    Acceptance,
    /// S2/S3/S5 合成金标评估。
    Eval,
}

/// 解析后的参数。
#[allow(clippy::struct_excessive_bools, reason = "CLI 开关天然为独立布尔参数")]
struct Args {
    data_dir: Option<PathBuf>,
    json: bool,
    deep: bool,
    max_sha_mib: u64,
    fixture: Option<PathBuf>,
    out: Option<PathBuf>,
    log_limit: usize,
    gold: Option<PathBuf>,
    limit_objective: Option<usize>,
    limit_solution: Option<usize>,
    real_judge: bool,
    judge_endpoint: Option<String>,
    model: Option<String>,
    sample: Option<usize>,
    concurrency: usize,
    calibrate: bool,
    ab: bool,
    ab_sample: Option<usize>,
    eval_sample: Option<usize>,
    prompt: String,
    write_calibration: bool,
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
        Command::Acceptance => run_acceptance(&args),
        Command::Eval => run_eval(&args),
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

fn run_acceptance(args: &Args) -> Result<u8, String> {
    let root = args.data_dir.clone().unwrap_or_else(|| {
        std::env::temp_dir().join(format!("egctl-acceptance-{}", std::process::id()))
    });
    let fixtures = args.fixture.clone().or_else(|| {
        let candidate = util::repo_root().join("fixtures/paper-20q");
        candidate.is_dir().then_some(candidate)
    });
    let options = egctl::acceptance::Options {
        fixtures,
        data_dir: Some(root),
        out: args.out.clone(),
    };
    let report = egctl::acceptance::run(&options);
    if let Some(out) = &args.out {
        if let Some(parent) = out.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(error) = std::fs::write(
            out,
            serde_json::to_string_pretty(&report.to_json()).unwrap_or_default(),
        ) {
            eprintln!("警告：写入 --out 失败（{error}）；结果仍输出到标准输出");
        } else {
            println!("结果已写入 {}", out.display());
        }
    }
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report.to_json()).unwrap_or_default()
        );
    } else {
        print!("{}", report.render_human());
    }
    Ok(u8::from(!report.passed))
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

fn run_eval(args: &Args) -> Result<u8, String> {
    let gold = args
        .gold
        .clone()
        .unwrap_or_else(|| PathBuf::from("fixtures/gold"));
    if args.real_judge {
        let options = eval_real::Options {
            gold,
            endpoint: args
                .judge_endpoint
                .clone()
                .unwrap_or_else(|| "http://127.0.0.1:8082/v1".to_owned()),
            model: args
                .model
                .clone()
                .unwrap_or_else(|| "qwen3.5-4b-instruct".to_owned()),
            sample: args.sample.or(Some(60)),
            concurrency: args.concurrency,
            timeout: std::time::Duration::from_secs(20),
            calibrate: args.calibrate,
            ab: args.ab,
            ab_sample: args.ab_sample.or(Some(30)),
            eval_sample: args.eval_sample,
            prompt: if args.prompt == "v2" {
                eval_real::PromptVersion::V2
            } else {
                eval_real::PromptVersion::V1
            },
            write_calibration: args.write_calibration,
        };
        if args.calibrate {
            let report = eval_real::run_calibrated(&options)?;
            if args.json {
                let value = report.to_json();
                let text = serde_json::to_string_pretty(&value).unwrap_or_default();
                if let Some(out) = &args.out {
                    std::fs::write(out, &text)
                        .map_err(|error| format!("写 {} 失败：{error}", out.display()))?;
                }
                println!("{text}");
            } else {
                println!("{}", report.render_human());
            }
            return Ok(0);
        }
        let report = eval_real::run(&options)?;
        if args.json {
            let value = report.to_json();
            let text = serde_json::to_string_pretty(&value).unwrap_or_default();
            if let Some(out) = &args.out {
                std::fs::write(out, &text)
                    .map_err(|error| format!("写 {} 失败：{error}", out.display()))?;
            }
            println!("{text}");
        } else {
            println!("{}", report.render_human());
        }
        return Ok(0);
    }
    let options = eval::Options {
        gold,
        max_objective: args.limit_objective,
        max_solution: args.limit_solution,
    };
    let report = eval::run(&options)?;
    if args.json {
        let value = report.to_json();
        let text = serde_json::to_string_pretty(&value).unwrap_or_default();
        if let Some(out) = &args.out {
            std::fs::write(out, &text)
                .map_err(|error| format!("写 {} 失败：{error}", out.display()))?;
        }
        println!("{text}");
    } else {
        println!("{}", report.render_human());
    }
    Ok(0)
}

#[allow(
    clippy::too_many_lines,
    reason = "CLI 参数解析集中列出全部子命令/选项，便于对照帮助文本"
)]
fn parse_args() -> Result<Args, String> {
    let mut data_dir = None;
    let mut json = false;
    let mut deep = false;
    let mut max_sha_mib = 64u64;
    let mut fixture = None;
    let mut out = None;
    let mut log_limit = 2000usize;
    let mut gold = None;
    let mut limit_objective = None;
    let mut limit_solution = None;
    let mut real_judge = false;
    let mut judge_endpoint = None;
    let mut model = None;
    let mut sample = None;
    let mut concurrency = 1usize;
    let mut calibrate = false;
    let mut ab = false;
    let mut ab_sample = None;
    let mut eval_sample = None;
    let mut prompt = "v1".to_owned();
    let mut write_calibration = false;
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
            "--fixture" | "--fixtures" => {
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
            "--real-judge" => real_judge = true,
            "--calibrate" => calibrate = true,
            "--ab" => ab = true,
            "--eval-sample" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--eval-sample 需要一个整数".to_owned())?;
                eval_sample = Some(
                    value
                        .parse()
                        .map_err(|_| format!("--eval-sample 非法：{value}"))?,
                );
            }
            "--write-calibration" => write_calibration = true,
            "--prompt" => {
                prompt = args
                    .next()
                    .ok_or_else(|| "--prompt 需要 v1 或 v2".to_owned())?;
                if prompt != "v1" && prompt != "v2" {
                    return Err(format!("--prompt 只支持 v1/v2：{prompt}"));
                }
            }
            "--ab-sample" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--ab-sample 需要一个整数".to_owned())?;
                ab_sample = Some(
                    value
                        .parse()
                        .map_err(|_| format!("--ab-sample 非法：{value}"))?,
                );
            }
            "--judge-endpoint" => {
                judge_endpoint = Some(
                    args.next()
                        .ok_or_else(|| "--judge-endpoint 需要一个 URL".to_owned())?,
                );
            }
            "--model" => {
                model = Some(
                    args.next()
                        .ok_or_else(|| "--model 需要一个模型名".to_owned())?,
                );
            }
            "--sample" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--sample 需要一个整数（0=全量）".to_owned())?;
                sample = Some(
                    value
                        .parse()
                        .map_err(|_| format!("--sample 非法：{value}"))?,
                );
            }
            "--concurrency" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--concurrency 需要一个整数".to_owned())?;
                concurrency = value
                    .parse()
                    .map_err(|_| format!("--concurrency 非法：{value}"))?;
            }
            "--gold" => {
                gold = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| "--gold 需要一个目录参数".to_owned())?,
                ));
            }
            "--limit-objective" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--limit-objective 需要一个整数".to_owned())?;
                limit_objective = Some(
                    value
                        .parse()
                        .map_err(|_| format!("--limit-objective 非法：{value}"))?,
                );
            }
            "--limit-solution" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--limit-solution 需要一个整数".to_owned())?;
                limit_solution = Some(
                    value
                        .parse()
                        .map_err(|_| format!("--limit-solution 非法：{value}"))?,
                );
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
        [one] if one == "acceptance" => Command::Acceptance,
        [one] if one == "eval" => Command::Eval,
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
        gold,
        limit_objective,
        limit_solution,
        real_judge,
        judge_endpoint,
        model,
        sample,
        concurrency,
        calibrate,
        ab,
        ab_sample,
        eval_sample,
        prompt,
        write_calibration,
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
           acceptance             S1 端到端自动化验收（fixtures→导入→识别→对齐→判分→复核→导出）\n\
           eval                   S2/S3/S5 合成金标评估（--gold fixtures/gold；--real-judge 接本地 4B）\n\
           version / --version    版本\n\
         \n\
         全局选项：\n\
           --json                 结构化输出（所有子命令）\n\
           --data-dir <目录>      覆盖数据根（默认按平台规则解析）\n\
           --deep                 doctor 额外执行 cargo check 检查其他 crate 编译状态\n\
           --max-sha-mib <MiB>    doctor 权重全量 SHA-256 阈值（默认 64）\n\
           --fixtures <目录>      replay/acceptance 夹具目录（acceptance 默认 fixtures/paper-20q）\n\
           --out <目录>           replay 保留导出 / diagnostics 输出目录\n\
           --log-limit <条数>     diagnostics 日志条数上限（默认 2000）\n\
           --gold <目录>          eval 金标目录（默认 fixtures/gold）\n\
           --limit-objective <N>  eval 仅取前 N 条客观题\n\
           --limit-solution <N>   eval 仅取前 N 条解答题\n\
           --real-judge           eval 走真实本地 judge（S3 ECE 复测）\n\
           --judge-endpoint <URL> 真实 judge /v1 端点（默认 http://127.0.0.1:8082/v1）\n\
           --model <name>         真实 judge 模型名（默认 qwen3.5-4b-instruct）\n\
           --sample <N>           eval --real-judge 抽样条数（默认 60；0=全量）\n\
           --concurrency <N>      eval --real-judge 并发请求数（默认 1，≤4）\n\
           --calibrate            eval --real-judge 校准模式（拆 calibration/eval，各 --sample 条）\n\
           --ab                   eval --real-judge 校准模式附带提示词 v1/v2 A/B（--ab-sample 条）\n\
           --ab-sample <N>        A/B 样本条数（默认 30）\n\
           --eval-sample <N>      eval 样本条数（v2 重校准用 120；默认=--sample）\n\
           --prompt <v1|v2>       calibration/eval 使用的提示词（默认 v1；A/B 始终双跑）\n\
           --write-calibration    校准后写 config/calibration/<id>.json（默认不写）\n\
         \n\
         退出码：0 = OK，2 = WARN（有降级/未就绪项），1 = ERROR",
        env!("CARGO_PKG_VERSION")
    );
}
