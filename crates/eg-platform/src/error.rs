//! 统一错误类型与错误分类（`SPEC-platform.md` §4.5、§5）。
//!
//! 错误分类 [`ErrorClass`] 有五档：教师可修复 / 瞬态 / 永久 / 数据损坏 / 策略拦截，
//! 决定重试策略、UI 文案与是否审计；所有对外错误都必须能映射到其中一档。

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::ConfigError;
use crate::secrets::SecretError;

/// 错误分类（`SPEC-platform.md` §4.5）。取值固定为五档，不得扩展。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    /// 教师可修复（配置非法、单实例冲突等）：给中文引导，不自动重试。
    TeacherFixable,
    /// 瞬态（磁盘忙、数据库忙、超时）：按退避重试。
    Transient,
    /// 永久（数据不存在、参数非法、约束冲突）：不重试，显式失败。
    Permanent,
    /// 数据损坏（哈希不符、数据库损坏）：停写并提示从备份恢复，不自动重建。
    DataCorruption,
    /// 策略拦截（Keychain 拒绝、授权无效、许可门禁）：禁用对应能力并引导。
    PolicyBlocked,
}

impl ErrorClass {
    /// 稳定机器码（落库 `task.last_error_class`、日志 `error_class` 字段）。
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::TeacherFixable => "teacher_fixable",
            Self::Transient => "transient",
            Self::Permanent => "permanent",
            Self::DataCorruption => "data_corruption",
            Self::PolicyBlocked => "policy_blocked",
        }
    }

    /// 教师可见中文文案。
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::TeacherFixable => "可修复：请按提示修正设置后重试",
            Self::Transient => "暂时性故障：稍后自动重试",
            Self::Permanent => "无法继续：此操作不可重试",
            Self::DataCorruption => "数据损坏：请从备份恢复，不建议继续写入",
            Self::PolicyBlocked => "已被策略拦截：请检查授权或系统钥匙串权限",
        }
    }

    /// 是否允许自动重试（仅瞬态）。
    #[must_use]
    pub const fn retryable(self) -> bool {
        matches!(self, Self::Transient)
    }

    /// 从机器码解析（日志/数据库回读）。
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "teacher_fixable" => Some(Self::TeacherFixable),
            "transient" => Some(Self::Transient),
            "permanent" => Some(Self::Permanent),
            "data_corruption" => Some(Self::DataCorruption),
            "policy_blocked" => Some(Self::PolicyBlocked),
            _ => None,
        }
    }
}

impl fmt::Display for ErrorClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// `platform` 统一错误。
#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    /// 配置错误（非法值、未知键、损坏文件）。
    #[error("配置错误：{0}")]
    Config(#[from] ConfigError),

    /// 密钥存取错误。
    #[error("密钥存取失败：{0}")]
    Secret(#[from] SecretError),

    /// `SQLite` 错误（由 [`classify_sqlite`] 分类）。
    #[error("数据库错误：{0}")]
    Database(#[from] rusqlite::Error),

    /// 数据库损坏（完整性检查失败或读取报错）。
    #[error("数据库已损坏：{0}")]
    DatabaseCorrupt(String),

    /// 文件读写失败。
    #[error("文件读写失败（{path}）：{source}")]
    Io {
        /// 相关路径。
        path: PathBuf,
        /// 底层错误。
        #[source]
        source: std::io::Error,
    },

    /// 资产内容哈希与记录不符。
    #[error("资产哈希不符：期望 {expected}，实际 {actual}")]
    AssetHashMismatch {
        /// `AssetId` 十六进制。
        expected: String,
        /// 实际计算值十六进制。
        actual: String,
    },

    /// 查询的记录不存在。
    #[error("数据不存在：{0}")]
    NotFound(String),

    /// 调用方参数非法。
    #[error("参数非法：{0}")]
    InvalidArgument(String),

    /// 审计写入失败：业务写必须一起回滚。
    #[error("审计写入失败（业务写已拒绝）：{0}")]
    Audit(String),

    /// 单实例锁被占用。
    #[error("已有一个实例在运行：{0}")]
    InstanceLocked(String),

    /// 任务状态迁移非法。
    #[error("任务状态迁移非法：{from} -> {to}")]
    InvalidTransition {
        /// 原状态机器码。
        from: String,
        /// 目标状态机器码。
        to: String,
    },

    /// 任务租约所有者不匹配。
    #[error("任务租约冲突：{0}")]
    LeaseConflict(String),

    /// 版本 CAS 冲突（如试卷规则版本已被并发推进）。
    #[error("版本冲突：期望 {expected}，实际 {actual}")]
    VersionConflict {
        /// 调用方期望的版本。
        expected: u32,
        /// 存储中的当前版本。
        actual: u32,
    },

    /// 协作式取消。
    #[error("操作已取消")]
    Cancelled,

    /// 策略拦截（授权、Keychain、许可门禁）。
    #[error("策略拦截：{0}")]
    PolicyBlocked(String),

    /// 序列化/反序列化失败。
    #[error("序列化失败：{0}")]
    Serialize(String),

    /// 其他内部错误（必须显式，不得静默）。
    #[error("内部错误：{0}")]
    Internal(String),
}

impl PlatformError {
    /// 映射到 [`ErrorClass`]（决定重试、文案与审计）。
    #[must_use]
    pub fn class(&self) -> ErrorClass {
        match self {
            Self::Config(e) => e.class(),
            Self::Secret(e) => e.class(),
            Self::Database(e) => classify_sqlite(e),
            Self::DatabaseCorrupt(_) | Self::AssetHashMismatch { .. } => ErrorClass::DataCorruption,
            Self::Io { .. } | Self::LeaseConflict(_) | Self::Cancelled => ErrorClass::Transient,
            Self::NotFound(_)
            | Self::Audit(_)
            | Self::InvalidArgument(_)
            | Self::InvalidTransition { .. }
            | Self::VersionConflict { .. }
            | Self::Serialize(_)
            | Self::Internal(_) => ErrorClass::Permanent,
            Self::InstanceLocked(_) => ErrorClass::TeacherFixable,
            Self::PolicyBlocked(_) => ErrorClass::PolicyBlocked,
        }
    }

    /// 稳定机器码（日志与数据库字段）。
    #[must_use]
    pub fn class_code(&self) -> &'static str {
        self.class().code()
    }

    /// 构造路径相关的 IO 错误。
    #[must_use]
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

/// 将 `rusqlite` 错误分类（损坏 vs 瞬态 vs 永久）。
#[must_use]
pub fn classify_sqlite(error: &rusqlite::Error) -> ErrorClass {
    match error {
        rusqlite::Error::SqliteFailure(err, _) => match err.code {
            rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase => {
                ErrorClass::DataCorruption
            }
            rusqlite::ErrorCode::DatabaseBusy
            | rusqlite::ErrorCode::DatabaseLocked
            | rusqlite::ErrorCode::SystemIoFailure
            | rusqlite::ErrorCode::DiskFull
            | rusqlite::ErrorCode::CannotOpen
            | rusqlite::ErrorCode::OperationInterrupted => ErrorClass::Transient,
            _ => ErrorClass::Permanent,
        },
        _ => ErrorClass::Permanent,
    }
}

/// `platform` 统一结果别名。
pub type Result<T> = std::result::Result<T, PlatformError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_class_codes_roundtrip() {
        let classes = [
            ErrorClass::TeacherFixable,
            ErrorClass::Transient,
            ErrorClass::Permanent,
            ErrorClass::DataCorruption,
            ErrorClass::PolicyBlocked,
        ];
        for class in classes {
            assert_eq!(ErrorClass::from_code(class.code()), Some(class));
            assert_ne!(class.label(), "");
        }
        assert_eq!(ErrorClass::from_code("nope"), None);
        assert!(ErrorClass::Transient.retryable());
        assert!(!ErrorClass::Permanent.retryable());
    }

    #[test]
    fn platform_error_classification() {
        assert_eq!(
            PlatformError::NotFound("x".to_owned()).class(),
            ErrorClass::Permanent
        );
        assert_eq!(
            PlatformError::DatabaseCorrupt("bad".to_owned()).class(),
            ErrorClass::DataCorruption
        );
        assert_eq!(
            PlatformError::PolicyBlocked("denied".to_owned()).class(),
            ErrorClass::PolicyBlocked
        );
        assert_eq!(PlatformError::Cancelled.class(), ErrorClass::Transient);
        assert_eq!(
            PlatformError::io("/tmp/x", std::io::Error::other("disk full")).class(),
            ErrorClass::Transient
        );
    }
}
