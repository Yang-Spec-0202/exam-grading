//! `egctl` 公共工具：检查级别、命令执行、路径/磁盘/哈希辅助与输出转义。
//!
//! 边界（`SPEC.md` §7 运维与验收）：只做运维与验收，不写业务数据；除 `backup`/`restore`/
//! `diagnostics export` 外不修改数据目录；不联网；不伪造 OK（未实现/未就绪一律 WARN）。

#![allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use eg_domain::Timestamp;

/// 检查级别（退出码：OK=0、ERROR=1、WARN=2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// 通过。
    Ok,
    /// 可用但有降级/未就绪项。
    Warn,
    /// 失败（阻断验收）。
    Error,
}

impl Level {
    /// 文本标签。
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
        }
    }

    /// 小写标签（JSON）。
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

/// 单项检查结果。
#[derive(Debug, Clone)]
pub struct Check {
    /// 稳定 id（JSON 字段）。
    pub id: String,
    /// 标题。
    pub title: String,
    /// 级别。
    pub level: Level,
    /// 人类可读详情（不含敏感信息）。
    pub detail: String,
}

impl Check {
    /// 构造检查结果。
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        level: Level,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            level,
            detail: detail.into(),
        }
    }

    /// 转为 JSON 对象。
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "title": self.title,
            "status": self.level.key(),
            "detail": self.detail,
        })
    }
}

/// 仓库根目录（开发/验收环境；安装后不存在）。
#[must_use]
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// 当前时间（运维 CLI 允许直接读系统时钟；不写业务数据）。
#[must_use]
pub fn now() -> Timestamp {
    chrono::Utc::now()
}

/// 解析数据目录：`--data-dir` 优先，否则按平台默认（环境变量/系统数据目录）。
///
/// # Errors
///
/// 路径无法解析或无效时返回中文错误。
pub fn resolve_dirs(explicit: Option<&Path>) -> std::result::Result<eg_platform::DataDirs, String> {
    match explicit {
        Some(root) => eg_platform::DataDirs::with_root(root).map_err(|error| error.to_string()),
        None => eg_platform::DataDirs::resolve().map_err(|error| error.to_string()),
    }
}

/// 外部命令执行结果。
#[derive(Debug, Clone)]
pub struct CommandOutput {
    /// 是否成功（退出码 0）。
    pub ok: bool,
    /// 标准输出（截断）。
    pub stdout: String,
    /// 标准错误（截断）。
    pub stderr: String,
}

/// 运行外部命令（找不到程序返回 `None`；不 panic）。
#[must_use]
pub fn run_command(program: &str, args: &[&str]) -> Option<CommandOutput> {
    let output = Command::new(program).args(args).output().ok()?;
    Some(CommandOutput {
        ok: output.status.success(),
        stdout: truncate(&String::from_utf8_lossy(&output.stdout), 400),
        stderr: truncate(&String::from_utf8_lossy(&output.stderr), 400),
    })
}

/// 在 PATH 中查找可执行文件（返回完整路径）。
#[must_use]
pub fn find_in_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(program);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// 截断文本（保留首部，避免报告过长）。
#[must_use]
pub fn truncate(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_owned();
    }
    let head: String = trimmed.chars().take(max_chars).collect();
    format!("{head}…")
}

/// 目录可写探测（创建并删除临时文件；不修改既有内容）。
#[must_use]
pub fn dir_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".egctl-probe-{}", std::process::id()));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// 磁盘可用空间（MiB）：优先 `df -Pk`，不可用时返回 `None`（明确「无法测量」）。
#[must_use]
pub fn free_disk_mib(path: &Path) -> Option<u64> {
    let output = run_command("df", &["-Pk", &path.display().to_string()])?;
    if !output.ok {
        return None;
    }
    for line in output.stdout.lines().skip(1) {
        let mut parts = line.split_whitespace();
        let _filesystem = parts.next()?;
        let _total = parts.next()?;
        let _used = parts.next()?;
        let available = parts.next()?;
        if let Ok(kib) = available.parse::<u64>() {
            return Some(kib / 1024);
        }
    }
    None
}

/// 文件大小（字节）。
#[must_use]
pub fn file_size(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|meta| meta.len())
}

/// 全量 SHA-256（小文件）。
///
/// # Errors
///
/// 读取失败时返回中文错误。
pub fn sha256_file(path: &Path) -> std::result::Result<String, String> {
    use sha2::Digest as _;
    let mut file =
        std::fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(buffer.get(..read).unwrap_or_default());
    }
    Ok(to_hex(hasher.finalize().as_slice()))
}

/// 字节数组转小写十六进制。
#[must_use]
pub fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// 采样 SHA-256（首尾各 `chunk` 字节；仅用于报告「抽样指纹」，不等价于全量哈希）。
///
/// # Errors
///
/// 读取失败时返回中文错误。
pub fn sample_sha256(path: &Path, chunk: u64) -> std::result::Result<String, String> {
    use sha2::Digest as _;
    let mut file =
        std::fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut hasher = sha2::Sha256::new();
    let mut head = vec![0u8; usize::try_from(chunk.min(8 * 1024 * 1024)).unwrap_or(0)];
    let read = file
        .read(&mut head)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    hasher.update(head.get(..read).unwrap_or_default());
    let size = file_size(path).unwrap_or(0);
    if size > chunk {
        use std::io::{Seek as _, SeekFrom};
        let start = size.saturating_sub(chunk);
        file.seek(SeekFrom::Start(start))
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let mut tail = vec![0u8; usize::try_from(chunk.min(8 * 1024 * 1024)).unwrap_or(0)];
        let read = file
            .read(&mut tail)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        hasher.update(tail.get(..read).unwrap_or_default());
    }
    Ok(to_hex(hasher.finalize().as_slice()))
}

/// 文件修改时间（RFC3339 近似文本）。
#[must_use]
pub fn modified_text(path: &Path) -> String {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .map_or_else(
            || "未知".to_owned(),
            |time| {
                let datetime: chrono::DateTime<chrono::Utc> = time.into();
                datetime.to_rfc3339()
            },
        )
}

/// 目录文件数与字节数（只统计普通文件）。
#[must_use]
pub fn dir_stats(dir: &Path) -> (u64, u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, 0);
    };
    let mut files = 0u64;
    let mut bytes = 0u64;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            files = files.saturating_add(1);
            bytes = bytes.saturating_add(file_size(&path).unwrap_or(0));
        }
    }
    (files, bytes)
}

/// 人类可读字节数。
#[must_use]
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// 打印检查结果（人类可读）。
pub fn print_checks(checks: &[Check]) {
    for check in checks {
        println!("[{}] {:<28} {}", check.level.label(), check.id, check.title);
        if !check.detail.is_empty() {
            for line in check.detail.lines() {
                println!("       {line}");
            }
        }
    }
}

/// 汇总级别。
#[must_use]
pub fn worst_level(checks: &[Check]) -> Level {
    checks
        .iter()
        .map(|check| check.level)
        .max()
        .unwrap_or(Level::Ok)
}

/// 退出码（OK=0、ERROR=1、WARN=2）。
#[must_use]
pub const fn exit_code(level: Level) -> u8 {
    match level {
        Level::Ok => 0,
        Level::Warn => 2,
        Level::Error => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_and_exit_codes() {
        assert_eq!(exit_code(Level::Ok), 0);
        assert_eq!(exit_code(Level::Warn), 2);
        assert_eq!(exit_code(Level::Error), 1);
        assert_eq!(worst_level(&[]), Level::Ok);
        assert_eq!(
            worst_level(&[
                Check::new("a", "A", Level::Ok, ""),
                Check::new("b", "B", Level::Warn, "")
            ]),
            Level::Warn
        );
    }

    #[test]
    fn human_bytes_and_truncate() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(truncate("abcdef", 3), "abc…");
        assert_eq!(truncate("abc", 3), "abc");
    }

    #[test]
    fn sha_of_known_bytes() {
        let dir = std::env::temp_dir().join(format!("egctl-sha-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.bin");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
