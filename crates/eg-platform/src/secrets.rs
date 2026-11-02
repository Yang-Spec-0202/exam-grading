//! 密钥存取（`SPEC-platform.md` §2）。
//!
//! - 经系统 Keychain（`keyring` 4.x）；服务名固定 `exam-grading`。
//! - 账户名三段构造：`cloud` / 后端标识 / `api_key`（见 [`SecretRef::account`]）。
//! - 配置文件只存 [`SecretRef`]；密钥**不入库、不入日志、不入审计、不入诊断包**。
//! - 读取失败（用户拒绝/后端错误）→ [`crate::error::ErrorClass::PolicyBlocked`]，
//!   上层应禁用该云端后端并给中文引导；条目缺失返回 `Ok(None)`。

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Mutex;

use crate::error::{ErrorClass, PlatformError, Result};

/// Keychain 服务名（A7：不做打包，不与 bundle identity 绑定）。
pub const KEYRING_SERVICE: &str = "exam-grading";

/// 密钥作用域（v1 仅云端）。
pub const SECRET_SCOPE_CLOUD: &str = "cloud";

/// 密钥用途（v1 仅 API Key）。
pub const SECRET_KIND_API_KEY: &str = "api_key";

/// 密钥引用（可安全落配置/日志/审计；不含明文）。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SecretRef {
    backend: String,
}

impl SecretRef {
    /// 构造云端后端的 API Key 引用。
    ///
    /// # Errors
    ///
    /// 后端标识为空或含分隔符 `/` 时返回 [`SecretError::InvalidRef`]。
    pub fn cloud_api_key(backend: impl Into<String>) -> std::result::Result<Self, SecretError> {
        let backend = backend.into();
        let trimmed = backend.trim();
        if trimmed.is_empty() {
            return Err(SecretError::InvalidRef("后端标识不能为空".to_owned()));
        }
        if trimmed.contains('/') {
            return Err(SecretError::InvalidRef(format!(
                "后端标识不能包含 '/'：{trimmed}"
            )));
        }
        Ok(Self {
            backend: trimmed.to_owned(),
        })
    }

    /// 后端标识。
    #[must_use]
    pub fn backend(&self) -> &str {
        &self.backend
    }

    /// Keychain 账户名：`cloud/<backend>/api_key`。
    #[must_use]
    pub fn account(&self) -> String {
        format!(
            "{SECRET_SCOPE_CLOUD}/{}/{SECRET_KIND_API_KEY}",
            self.backend
        )
    }

    /// 从账户名解析（诊断/迁移用）。
    ///
    /// # Errors
    ///
    /// 段数或前缀不匹配时返回 [`SecretError::InvalidRef`]。
    pub fn from_account(account: &str) -> std::result::Result<Self, SecretError> {
        let mut parts = account.split('/');
        let scope = parts.next().unwrap_or_default();
        let backend = parts.next().unwrap_or_default();
        let kind = parts.next().unwrap_or_default();
        if scope != SECRET_SCOPE_CLOUD || kind != SECRET_KIND_API_KEY {
            return Err(SecretError::InvalidRef(format!(
                "账户名必须形如 cloud/<backend>/api_key，实际为 {account}"
            )));
        }
        if parts.next().is_some() {
            return Err(SecretError::InvalidRef(format!(
                "账户名段数多于 3：{account}"
            )));
        }
        Self::cloud_api_key(backend)
    }
}

impl fmt::Display for SecretRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.account())
    }
}

/// 密钥明文包装：`Debug` 永远脱敏，不实现 `Serialize`。
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// 包装明文。
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// 取明文（仅在调用外部后端时短暂使用，不得写入日志/配置）。
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// 明文字节长度（用于诊断，不泄露内容）。
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// 是否为空密钥。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secret(<已脱敏，{} 字节>)", self.0.len())
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<已脱敏>")
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self(value)
    }
}

/// 密钥错误（分类恒为策略拦截，见 `SPEC-platform.md` §5）。
#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    /// Keychain 拒绝访问或后端不可用。
    #[error("系统钥匙串拒绝访问（{account}）：{message}")]
    Denied {
        /// 账户名。
        account: String,
        /// 底层说明。
        message: String,
    },

    /// 系统钥匙串条目缺失。
    #[error("钥匙串中不存在该密钥（{account}），请先在设置页填写")]
    Missing {
        /// 账户名。
        account: String,
    },

    /// 密钥引用本身非法。
    #[error("密钥引用非法：{0}")]
    InvalidRef(String),

    /// 平台后端不支持（例如无 GUI 会话）。
    #[error("系统钥匙串不可用：{0}")]
    Unavailable(String),
}

impl SecretError {
    /// 错误分类：密钥问题一律「策略拦截」。
    #[must_use]
    pub fn class(&self) -> ErrorClass {
        ErrorClass::PolicyBlocked
    }

    /// 转 [`PlatformError`]。
    #[must_use]
    pub fn into_platform(self) -> PlatformError {
        PlatformError::Secret(self)
    }
}

/// 密钥存取端口（生产实现走 Keychain；测试用内存实现）。
pub trait SecretStore: Send + Sync {
    /// 读取密钥；条目不存在返回 `Ok(None)`，拒绝访问/后端错误返回 `Err`。
    ///
    /// # Errors
    ///
    /// 见 [`SecretError`]。
    fn get(&self, reference: &SecretRef) -> Result<Option<Secret>>;

    /// 写入/更新密钥。
    ///
    /// # Errors
    ///
    /// 见 [`SecretError`]。
    fn set(&self, reference: &SecretRef, secret: &Secret) -> Result<()>;

    /// 是否存在（不读取明文）。
    ///
    /// # Errors
    ///
    /// 见 [`SecretError`]。
    fn exists(&self, reference: &SecretRef) -> Result<bool>;

    /// 删除密钥（幂等）。
    ///
    /// # Errors
    ///
    /// 见 [`SecretError`]。
    fn delete(&self, reference: &SecretRef) -> Result<()>;
}

/// 生产实现：系统 Keychain（`keyring` 4.x，默认特性启用平台后端）。
#[derive(Debug, Clone)]
pub struct KeychainSecretStore {
    service: String,
}

impl KeychainSecretStore {
    /// 以固定服务名 `exam-grading` 创建。
    #[must_use]
    pub fn new() -> Self {
        Self {
            service: KEYRING_SERVICE.to_owned(),
        }
    }

    /// 以自定义服务名创建（仅测试/诊断使用；生产必须用固定服务名）。
    #[must_use]
    pub fn with_service(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    /// 服务名。
    #[must_use]
    pub fn service(&self) -> &str {
        &self.service
    }

    fn entry(&self, reference: &SecretRef) -> Result<keyring::Entry> {
        keyring::Entry::new(&self.service, &reference.account()).map_err(|error| {
            SecretError::Unavailable(format!("无法创建钥匙串条目：{error}")).into()
        })
    }
}

impl Default for KeychainSecretStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretStore for KeychainSecretStore {
    fn get(&self, reference: &SecretRef) -> Result<Option<Secret>> {
        let entry = self.entry(reference)?;
        match entry.get_password() {
            Ok(password) => Ok(Some(Secret::new(password))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(SecretError::Denied {
                account: reference.account(),
                message: error.to_string(),
            }
            .into()),
        }
    }

    fn set(&self, reference: &SecretRef, secret: &Secret) -> Result<()> {
        let entry = self.entry(reference)?;
        entry
            .set_password(secret.expose())
            .map_err(|error| SecretError::Denied {
                account: reference.account(),
                message: error.to_string(),
            })?;
        Ok(())
    }

    fn exists(&self, reference: &SecretRef) -> Result<bool> {
        Ok(self.get(reference)?.is_some())
    }

    fn delete(&self, reference: &SecretRef) -> Result<()> {
        let entry = self.entry(reference)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(SecretError::Denied {
                account: reference.account(),
                message: error.to_string(),
            }
            .into()),
        }
    }
}

/// 测试/无 Keychain 环境的内存实现（内容不落盘）。
#[derive(Debug, Default)]
pub struct InMemorySecretStore {
    entries: Mutex<BTreeMap<String, String>>,
}

impl InMemorySecretStore {
    /// 创建空存储。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretStore for InMemorySecretStore {
    fn get(&self, reference: &SecretRef) -> Result<Option<Secret>> {
        let guard = self
            .entries
            .lock()
            .map_err(|_| PlatformError::Internal("密钥存储锁中毒".to_owned()))?;
        Ok(guard
            .get(&reference.account())
            .map(|value| Secret::new(value.clone())))
    }

    fn set(&self, reference: &SecretRef, secret: &Secret) -> Result<()> {
        let mut guard = self
            .entries
            .lock()
            .map_err(|_| PlatformError::Internal("密钥存储锁中毒".to_owned()))?;
        guard.insert(reference.account(), secret.expose().to_owned());
        Ok(())
    }

    fn exists(&self, reference: &SecretRef) -> Result<bool> {
        let guard = self
            .entries
            .lock()
            .map_err(|_| PlatformError::Internal("密钥存储锁中毒".to_owned()))?;
        Ok(guard.contains_key(&reference.account()))
    }

    fn delete(&self, reference: &SecretRef) -> Result<()> {
        let mut guard = self
            .entries
            .lock()
            .map_err(|_| PlatformError::Internal("密钥存储锁中毒".to_owned()))?;
        guard.remove(&reference.account());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn secret_ref_account_format() {
        let reference = SecretRef::cloud_api_key("deepseek").unwrap();
        assert_eq!(reference.account(), "cloud/deepseek/api_key");
        assert_eq!(reference.to_string(), "cloud/deepseek/api_key");
        assert_eq!(reference.backend(), "deepseek");
        assert_eq!(
            SecretRef::from_account("cloud/deepseek/api_key").unwrap(),
            reference
        );
        assert!(SecretRef::from_account("cloud/deepseek").is_err());
        assert!(SecretRef::from_account("cloud//api_key").is_err());
        assert!(SecretRef::cloud_api_key("a/b").is_err());
        assert!(SecretRef::cloud_api_key("  ").is_err());
    }

    #[test]
    fn secret_debug_and_display_are_redacted() {
        let secret = Secret::new("sk-super-secret-value");
        let debug = format!("{secret:?}");
        let display = format!("{secret}");
        assert!(!debug.contains("sk-super"));
        assert!(!display.contains("sk-super"));
        assert!(debug.contains("已脱敏"));
        assert_eq!(secret.expose(), "sk-super-secret-value");
        assert_eq!(secret.len(), 21);
        assert!(!secret.is_empty());
    }

    #[test]
    fn in_memory_store_roundtrip() {
        let store = InMemorySecretStore::new();
        let reference = SecretRef::cloud_api_key("deepseek").unwrap();
        assert!(store.get(&reference).unwrap().is_none());
        assert!(!store.exists(&reference).unwrap());
        store.set(&reference, &Secret::new("sk-123")).unwrap();
        assert!(store.exists(&reference).unwrap());
        assert_eq!(store.get(&reference).unwrap().unwrap().expose(), "sk-123");
        store.delete(&reference).unwrap();
        assert!(store.get(&reference).unwrap().is_none());
        store.delete(&reference).unwrap();
    }

    #[test]
    fn keychain_store_has_fixed_service_name() {
        let store = KeychainSecretStore::new();
        assert_eq!(store.service(), KEYRING_SERVICE);
        assert_eq!(KEYRING_SERVICE, "exam-grading");
    }

    #[test]
    fn secret_error_is_policy_blocked() {
        let error = SecretError::Missing {
            account: "cloud/x/api_key".to_owned(),
        };
        assert_eq!(error.class(), ErrorClass::PolicyBlocked);
        assert_ne!(error.to_string(), "");
    }
}
