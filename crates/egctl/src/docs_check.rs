//! `egctl docs check`：文档门禁（规范无代码围栏 + 相对链接无死链 + mdBook 构建）。
//!
//! 直接复用仓库脚本 `scripts/docs-check.sh`（单一事实来源，避免两套实现漂移）；
//! 本模块只负责定位仓库根、执行、限时与结构化报告。

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::util::{Level, repo_root, truncate};

/// 文档检查结果。
#[derive(Debug, Clone)]
pub struct DocsCheckReport {
    /// 脚本路径。
    pub script: PathBuf,
    /// 是否通过。
    pub passed: bool,
    /// 退出码。
    pub exit_code: Option<i32>,
    /// 耗时（毫秒）。
    pub elapsed_ms: u128,
    /// 输出尾部（人类可读）。
    pub tail: String,
}

impl DocsCheckReport {
    /// 检查级别。
    #[must_use]
    pub const fn level(&self) -> Level {
        if self.passed { Level::Ok } else { Level::Error }
    }

    /// JSON。
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "script": self.script.display().to_string(),
            "passed": self.passed,
            "exit_code": self.exit_code,
            "elapsed_ms": self.elapsed_ms,
            "tail": self.tail,
        })
    }
}

/// 执行 `scripts/docs-check.sh`。
///
/// # Errors
///
/// 脚本不存在或无法执行时返回中文错误（不作为「通过」）。
pub fn run() -> std::result::Result<DocsCheckReport, String> {
    let root = repo_root();
    let script = root.join("scripts/docs-check.sh");
    if !script.is_file() {
        return Err(format!(
            "未找到文档门禁脚本：{}（请在仓库根运行或安装完整仓库）",
            script.display()
        ));
    }
    let started = Instant::now();
    let output = Command::new("bash")
        .arg(&script)
        .current_dir(&root)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .output()
        .map_err(|error| format!("执行 {} 失败：{error}", script.display()))?;
    let elapsed_ms = started.elapsed().as_millis();
    // 脚本可能长时间运行（mdbook build），这里不做强制超时：CI 与本地验收一致。
    let _ = Duration::from_secs(0);
    let mut tail = String::new();
    tail.push_str(&String::from_utf8_lossy(&output.stdout));
    if !output.stderr.is_empty() {
        tail.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    let tail_lines: Vec<&str> = tail.lines().rev().take(12).collect();
    let tail = tail_lines.into_iter().rev().collect::<Vec<_>>().join("\n");
    Ok(DocsCheckReport {
        script,
        passed: output.status.success(),
        exit_code: output.status.code(),
        elapsed_ms,
        tail: truncate(&tail, 3000),
    })
}

/// 人类可读输出。
#[must_use]
pub fn render_human(report: &DocsCheckReport) -> String {
    let mut out = format!(
        "[{}] 文档门禁（{}；退出码 {:?}；{} ms）\n",
        report.level().label(),
        report.script.display(),
        report.exit_code,
        report.elapsed_ms
    );
    for line in report.tail.lines() {
        out.push_str(&format!("  {line}\n"));
    }
    out
}
