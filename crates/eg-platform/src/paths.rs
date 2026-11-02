//! 数据根与子路径解析（`SPEC-platform.md` §2）。
//!
//! 生产路径：`~/Library/Application Support/exam-grading`；
//! 测试与便携模式由 `EXAM_GRADING_DATA_DIR` 覆盖，日志目录可另由
//! `EXAM_GRADING_LOG_DIR` 改指（覆盖后必须在日志首行声明，由 [`DataDirs::env_override_note`] 提供）。

use std::path::{Path, PathBuf};

use crate::error::{PlatformError, Result};

/// 应用目录名（数据根与缓存根的末级目录）。
pub const APP_DIR_NAME: &str = "exam-grading";

/// 数据根覆盖环境变量。
pub const DATA_DIR_ENV: &str = "EXAM_GRADING_DATA_DIR";

/// 日志目录覆盖环境变量。
pub const LOG_DIR_ENV: &str = "EXAM_GRADING_LOG_DIR";

/// 唯一业务库文件名。
pub const DATABASE_FILE: &str = "exam-grading.sqlite3";

/// 配置文件与一代备份。
pub const CONFIG_FILE: &str = "config.toml";
/// 配置备份文件名。
pub const CONFIG_BACKUP_FILE: &str = "config.toml.bak";

/// 单实例锁文件名。
pub const LOCK_FILE: &str = "instance.lock";

/// ULID 最近毫秒状态文件名。
pub const ULID_STATE_FILE: &str = "ulid.state";

/// ULID 最近毫秒状态文件名（重复定义给 `ids` 模块复用）。
pub use ULID_STATE_FILE as ULID_STATE;

/// 工作区目录布局。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataDirs {
    root: PathBuf,
    log_dir: PathBuf,
    cache_dir: PathBuf,
    overrides: Vec<String>,
}

impl DataDirs {
    /// 按环境变量与系统默认解析数据根（不创建目录）。
    ///
    /// # Errors
    ///
    /// 无法确定 HOME 或路径为空时返回 [`PlatformError::InvalidArgument`]。
    pub fn resolve() -> Result<Self> {
        let root = match std::env::var_os(DATA_DIR_ENV) {
            Some(value) if !value.is_empty() => PathBuf::from(value),
            _ => {
                let home = std::env::var_os("HOME")
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| {
                        PlatformError::InvalidArgument(
                            "无法确定用户主目录（HOME 未设置），请用 EXAM_GRADING_DATA_DIR 指定数据目录"
                                .to_owned(),
                        )
                    })?;
                PathBuf::from(home)
                    .join("Library")
                    .join("Application Support")
                    .join(APP_DIR_NAME)
            }
        };
        Self::with_root(root)
    }

    /// 以给定根目录构造（测试与便携模式使用）。
    ///
    /// # Errors
    ///
    /// 根目录为空时返回 [`PlatformError::InvalidArgument`]。
    pub fn with_root(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        if root.as_os_str().is_empty() {
            return Err(PlatformError::InvalidArgument(
                "数据目录不能为空".to_owned(),
            ));
        }
        let mut overrides = Vec::new();
        if std::env::var_os(DATA_DIR_ENV).is_some() {
            overrides.push(format!("数据目录由 {DATA_DIR_ENV} 覆盖"));
        }
        let log_dir = match std::env::var_os(LOG_DIR_ENV) {
            Some(value) if !value.is_empty() => {
                overrides.push(format!("日志目录由 {LOG_DIR_ENV} 覆盖"));
                PathBuf::from(value)
            }
            _ => root.join("logs"),
        };
        let cache_dir = match std::env::var_os("HOME") {
            Some(home) if !home.is_empty() => PathBuf::from(home)
                .join("Library")
                .join("Caches")
                .join(APP_DIR_NAME),
            _ => root.join("cache"),
        };
        Ok(Self {
            root,
            log_dir,
            cache_dir,
            overrides,
        })
    }

    /// 数据根。
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 缓存根（缩略图与解码中间产物，可删除）。
    #[must_use]
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// 日志目录。
    #[must_use]
    pub fn log_dir(&self) -> &Path {
        &self.log_dir
    }

    /// 配置文件路径。
    #[must_use]
    pub fn config_path(&self) -> PathBuf {
        self.root.join(CONFIG_FILE)
    }

    /// 配置备份路径。
    #[must_use]
    pub fn config_backup_path(&self) -> PathBuf {
        self.root.join(CONFIG_BACKUP_FILE)
    }

    /// 业务库路径。
    #[must_use]
    pub fn database_path(&self) -> PathBuf {
        self.root.join(DATABASE_FILE)
    }

    /// 资产库目录。
    #[must_use]
    pub fn assets_dir(&self) -> PathBuf {
        self.root.join("assets").join("blake3")
    }

    /// 模型权重目录（由 `inference` 写入）。
    #[must_use]
    pub fn models_dir(&self) -> PathBuf {
        self.root.join("models")
    }

    /// 诊断包目录。
    #[must_use]
    pub fn diagnostics_dir(&self) -> PathBuf {
        self.log_dir.join("diagnostics")
    }

    /// 备份目录。
    #[must_use]
    pub fn backups_dir(&self) -> PathBuf {
        self.root.join("backups")
    }

    /// 导出临时区。
    #[must_use]
    pub fn exports_dir(&self) -> PathBuf {
        self.root.join("exports")
    }

    /// 单实例锁文件。
    #[must_use]
    pub fn lock_path(&self) -> PathBuf {
        self.root.join(LOCK_FILE)
    }

    /// ULID 状态文件。
    #[must_use]
    pub fn ulid_state_path(&self) -> PathBuf {
        self.root.join(ULID_STATE_FILE)
    }

    /// 工作区目录是否已存在。
    #[must_use]
    pub fn exists(&self) -> bool {
        self.root.is_dir()
    }

    /// 创建全部必需子目录（幂等）。
    ///
    /// # Errors
    ///
    /// 目录创建失败时返回 [`PlatformError::Io`]。
    pub fn ensure(&self) -> Result<()> {
        for dir in [
            self.root.clone(),
            self.assets_dir(),
            self.log_dir.clone(),
            self.diagnostics_dir(),
            self.backups_dir(),
            self.exports_dir(),
            self.models_dir(),
        ] {
            std::fs::create_dir_all(&dir)
                .map_err(|source| PlatformError::io(dir.clone(), source))?;
        }
        Ok(())
    }

    /// 环境变量覆盖说明（需要写入日志首行）；未覆盖时返回 `None`。
    #[must_use]
    pub fn env_override_note(&self) -> Option<String> {
        if self.overrides.is_empty() {
            None
        } else {
            Some(format!(
                "存储路径覆盖生效：{}（数据根 {}）",
                self.overrides.join("；"),
                self.root.display()
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_under_root() {
        let dirs = DataDirs::with_root("/tmp/eg-test-root").unwrap();
        assert_eq!(dirs.root(), Path::new("/tmp/eg-test-root"));
        assert_eq!(
            dirs.config_path(),
            Path::new("/tmp/eg-test-root/config.toml")
        );
        assert!(dirs.database_path().ends_with("exam-grading.sqlite3"));
        assert!(dirs.assets_dir().ends_with("assets/blake3"));
        assert!(dirs.log_dir().ends_with("logs"));
        let note = dirs.env_override_note();
        if std::env::var_os(DATA_DIR_ENV).is_some() {
            assert!(note.unwrap_or_default().contains(DATA_DIR_ENV));
        }
    }

    #[test]
    fn ensure_creates_directories() {
        let temp = tempfile::tempdir().unwrap();
        let dirs = DataDirs::with_root(temp.path().join("data")).unwrap();
        dirs.ensure().unwrap();
        for path in [
            dirs.root(),
            &dirs.assets_dir(),
            dirs.log_dir(),
            &dirs.diagnostics_dir(),
            &dirs.backups_dir(),
            &dirs.exports_dir(),
            &dirs.models_dir(),
        ] {
            assert!(path.is_dir(), "{} 应存在", path.display());
        }
        dirs.ensure().unwrap();
    }

    #[test]
    fn empty_root_rejected() {
        assert!(DataDirs::with_root(PathBuf::new()).is_err());
    }
}
