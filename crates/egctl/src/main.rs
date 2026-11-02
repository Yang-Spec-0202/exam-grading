//! `egctl`：诊断、夹具回放与基准命令行。
//!
//! 状态：M0 工程骨架；当前仅支持版本输出，子命令在后续里程碑按规范补齐。
//! 规范见仓库根目录 `SPEC.md（§5 命令）`。

#![allow(clippy::print_stdout, reason = "CLI 需要向标准输出报告结果")]

use std::process::ExitCode;

fn main() -> ExitCode {
    println!("egctl {}", env!("CARGO_PKG_VERSION"));
    ExitCode::SUCCESS
}
