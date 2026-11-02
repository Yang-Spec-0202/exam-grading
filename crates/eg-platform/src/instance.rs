//! 单实例锁（`SPEC-platform.md` §4.5）。
//!
//! 数据根下锁文件 + 文件锁（非阻塞）；锁内容写进程号、启动时间与版本。
//! 第二个实例在 1 秒内给出中文提示并以非零码退出（退出由 `app` 负责，本模块只返回错误）。

use std::fs::{File, OpenOptions};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};

use eg_domain::Timestamp;
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::error::{PlatformError, Result};
use crate::paths::DataDirs;

/// 锁文件内容（诊断与第二实例提示用）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceInfo {
    /// 进程号。
    pub pid: u32,
    /// 启动时间（RFC3339）。
    pub started_at: Timestamp,
    /// 程序版本。
    pub version: String,
    /// 数据根。
    pub data_root: String,
}

impl InstanceInfo {
    /// 中文提示（含进程号与启动时间）。
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "已有实例在运行（进程 {}，启动于 {}，版本 {}，数据目录 {}）",
            self.pid,
            eg_domain::time::format_rfc3339(self.started_at),
            self.version,
            self.data_root
        )
    }
}

/// 单实例锁句柄（持有期间锁有效；`Drop` 释放）。
#[derive(Debug)]
pub struct InstanceLock {
    file: File,
    path: PathBuf,
    info: InstanceInfo,
}

impl InstanceLock {
    /// 获取数据根下的单实例锁。
    ///
    /// # Errors
    ///
    /// 已有实例持锁时返回 [`PlatformError::InstanceLocked`]（含占锁进程信息）；
    /// IO 失败时返回 [`PlatformError::Io`]。
    pub fn acquire(dirs: &DataDirs, started_at: Timestamp, version: &str) -> Result<Self> {
        dirs.ensure()?;
        let path = dirs.lock_path();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|source| PlatformError::io(path.clone(), source))?;

        if file.try_lock_exclusive().is_err() {
            let existing = read_info(&mut file);
            let detail = existing.map_or_else(
                || format!("锁文件被占用：{}", path.display()),
                |info| info.describe(),
            );
            return Err(PlatformError::InstanceLocked(detail));
        }

        let info = InstanceInfo {
            pid: std::process::id(),
            started_at,
            version: version.to_owned(),
            data_root: dirs.root().display().to_string(),
        };
        let payload = serde_json::to_vec_pretty(&info)
            .map_err(|error| PlatformError::Serialize(error.to_string()))?;
        file.set_len(0)
            .map_err(|source| PlatformError::io(path.clone(), source))?;
        file.seek(SeekFrom::Start(0))
            .map_err(|source| PlatformError::io(path.clone(), source))?;
        file.write_all(&payload)
            .map_err(|source| PlatformError::io(path.clone(), source))?;
        file.sync_all()
            .map_err(|source| PlatformError::io(path.clone(), source))?;

        Ok(Self { file, path, info })
    }

    /// 锁信息（当前进程）。
    #[must_use]
    pub fn info(&self) -> &InstanceInfo {
        &self.info
    }

    /// 锁文件路径。
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

/// 读取锁文件中的实例信息（可能为空/损坏，损坏视为未知）。
fn read_info(file: &mut File) -> Option<InstanceInfo> {
    let mut text = String::new();
    file.seek(SeekFrom::Start(0)).ok()?;
    file.read_to_string(&mut text).ok()?;
    serde_json::from_str(&text).ok()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::time::Instant;

    use super::*;

    fn dirs(temp: &tempfile::TempDir) -> DataDirs {
        DataDirs::with_root(temp.path().join("data")).unwrap()
    }

    fn timestamp() -> Timestamp {
        eg_domain::time::parse_rfc3339("2026-11-02T12:00:00Z").unwrap()
    }

    #[test]
    fn second_instance_fails_fast_with_pid_hint() {
        let temp = tempfile::tempdir().unwrap();
        let dirs = dirs(&temp);
        let first = InstanceLock::acquire(&dirs, timestamp(), "0.1.0").unwrap();
        assert_eq!(first.info().pid, std::process::id());
        assert!(first.path().exists());

        let started = Instant::now();
        let second = InstanceLock::acquire(&dirs, timestamp(), "0.1.0");
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "第二实例必须 1s 内失败"
        );
        let message = second.unwrap_err().to_string();
        assert!(message.contains("已有实例在运行"), "{message}");
        assert!(
            message.contains(&std::process::id().to_string()),
            "{message}"
        );
        assert!(message.contains("2026-11-02T12:00:00Z"), "{message}");
    }

    #[test]
    fn lock_is_released_on_drop() {
        let temp = tempfile::tempdir().unwrap();
        let dirs = dirs(&temp);
        {
            let _first = InstanceLock::acquire(&dirs, timestamp(), "0.1.0").unwrap();
        }
        let second = InstanceLock::acquire(&dirs, timestamp(), "0.1.0");
        assert!(second.is_ok(), "释放后应可再次获取");
    }

    #[test]
    fn stale_corrupt_lock_file_is_reusable() {
        let temp = tempfile::tempdir().unwrap();
        let dirs = dirs(&temp);
        dirs.ensure().unwrap();
        std::fs::write(dirs.lock_path(), b"not-json").unwrap();
        let lock = InstanceLock::acquire(&dirs, timestamp(), "0.1.0").unwrap();
        assert_eq!(lock.info().pid, std::process::id());
        let text = std::fs::read_to_string(dirs.lock_path()).unwrap();
        assert!(text.contains("\"pid\""), "锁文件应被重写为合法 JSON");
    }
}
