//! 内容寻址资产库（BLAKE3，永久只读；`SPEC-platform.md` §4.2）。
//!
//! 写路径：算内容哈希（即 `AssetId`）→ 已存在只建引用（去重）→ 不存在则临时文件、
//! `fsync`、改名到分片路径、置只读、**最后**插入资产行（文件先落盘、DB 后提交）。
//!
//! **本模块不提供任何删除 API**：无 GC、无保留期、无「导出后清理」。软配额超限只告警。

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use eg_domain::{AssetId, Timestamp};
use serde::{Deserialize, Serialize};

use crate::clock::SharedClock;
use crate::db::Database;
use crate::error::{PlatformError, Result};
use crate::paths::DataDirs;
use crate::repo;

/// 资产库设置（来自 `AppConfig.storage`）。
#[derive(Debug, Clone, Copy)]
pub struct AssetSettings {
    /// 读取时校验哈希（`storage.verify_hash_on_read`）。
    pub verify_hash_on_read: bool,
    /// 软配额（字节；`storage.soft_quota_gb` × 1 GiB）。
    pub soft_quota_bytes: u64,
}

impl Default for AssetSettings {
    fn default() -> Self {
        Self {
            verify_hash_on_read: true,
            soft_quota_bytes: 20 * 1024 * 1024 * 1024,
        }
    }
}

/// 入库结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PutOutcome {
    /// 内容哈希（`AssetId`）。
    pub asset: AssetId,
    /// 是否命中已有内容（去重）。
    pub deduped: bool,
    /// 字节数。
    pub byte_len: u64,
}

/// 占用统计与软配额告警。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssetUsage {
    /// 资产行数。
    pub entries: u64,
    /// 字节总数。
    pub total_bytes: u64,
    /// 软配额（字节）。
    pub soft_quota_bytes: u64,
    /// 是否超过软配额（只告警，不删除）。
    pub over_quota: bool,
}

/// 全量校验报告（备份前后、恢复后使用）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AssetVerifyReport {
    /// 已校验数量。
    pub checked: u64,
    /// 校验失败项（`AssetId` 与原因）。
    pub failures: Vec<(String, String)>,
}

impl AssetVerifyReport {
    /// 是否全部通过。
    #[must_use]
    pub fn ok(&self) -> bool {
        self.failures.is_empty()
    }
}

/// 资产库（文件 + 元数据行）。
#[derive(Clone)]
pub struct AssetStore {
    dirs: DataDirs,
    db: Arc<Database>,
    clock: SharedClock,
    settings: AssetSettings,
    temp_counter: Arc<AtomicU64>,
}

impl std::fmt::Debug for AssetStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssetStore")
            .field("root", &self.root())
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl AssetStore {
    /// 构造资产库（不创建目录；首次写入时创建）。
    #[must_use]
    pub fn new(
        dirs: DataDirs,
        db: Arc<Database>,
        clock: SharedClock,
        settings: AssetSettings,
    ) -> Self {
        Self {
            dirs,
            db,
            clock,
            settings,
            temp_counter: Arc::new(AtomicU64::new(0)),
        }
    }

    /// 资产文件路径：`assets/blake3/<前2位>/<次2位>/<完整哈希>`。
    #[must_use]
    pub fn path_of(&self, asset: AssetId) -> PathBuf {
        let hex = asset.to_hex();
        let (prefix, _rest) = hex.split_at(4);
        self.dirs
            .assets_dir()
            .join(&prefix[0..2])
            .join(&prefix[2..4])
            .join(&hex)
    }

    /// 入库字节（内容寻址；同字节去重）。
    ///
    /// # Errors
    ///
    /// 文件写入/改名/权限设置失败或数据库行写入失败时返回错误；
    /// 失败时可能留下孤儿文件（规范允许），但**不会**出现「有行无文件」。
    pub fn put(&self, bytes: &[u8], media_type: &str) -> Result<PutOutcome> {
        let asset = AssetId::of(bytes);
        let path = self.path_of(asset);
        let existed = path.exists();

        if !existed {
            self.write_file_atomically(&path, bytes)?;
        }

        let at = self.clock.now();
        let byte_len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let media_type = media_type.to_owned();
        let inserted = self
            .db
            .write(move |tx| repo::insert_asset_row(tx, asset, byte_len, &media_type, at))?;

        Ok(PutOutcome {
            asset,
            // 文件已存在或行已存在都算去重命中。
            deduped: existed || !inserted,
            byte_len,
        })
    }

    /// 读取资产字节；按设置校验哈希。
    ///
    /// # Errors
    ///
    /// 文件不存在返回 [`PlatformError::NotFound`]；哈希不符返回
    /// [`PlatformError::AssetHashMismatch`]（数据损坏）。
    pub fn get(&self, asset: AssetId) -> Result<Vec<u8>> {
        let path = self.path_of(asset);
        let bytes = std::fs::read(&path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                PlatformError::NotFound(format!(
                    "资产文件缺失：{}（数据库有行、文件不存在，需从备份恢复）",
                    path.display()
                ))
            } else {
                PlatformError::io(path.clone(), source)
            }
        })?;
        if self.settings.verify_hash_on_read {
            self.verify_bytes(asset, &bytes)?;
        }
        Ok(bytes)
    }

    /// 是否存在（只查文件；数据库行缺失由 [`AssetStore::verify_all`] 发现）。
    #[must_use]
    pub fn exists(&self, asset: AssetId) -> bool {
        self.path_of(asset).exists()
    }

    /// 校验单个资产内容哈希。
    ///
    /// # Errors
    ///
    /// 文件缺失或哈希不符时返回错误。
    pub fn verify(&self, asset: AssetId) -> Result<()> {
        let bytes = self.get(asset)?;
        self.verify_bytes(asset, &bytes)
    }

    /// 静态校验：给定字节的哈希是否等于 `asset`。
    ///
    /// # Errors
    ///
    /// 哈希不符时返回 [`PlatformError::AssetHashMismatch`]。
    pub fn verify_bytes(&self, asset: AssetId, bytes: &[u8]) -> Result<()> {
        let actual = AssetId::of(bytes);
        if actual == asset {
            Ok(())
        } else {
            Err(PlatformError::AssetHashMismatch {
                expected: asset.to_hex(),
                actual: actual.to_hex(),
            })
        }
    }

    /// 占用统计（含软配额告警）。
    ///
    /// # Errors
    ///
    /// 查询失败时返回错误。
    pub fn usage(&self) -> Result<AssetUsage> {
        let (entries, total_bytes) = self.db.read(repo::asset_usage)?;
        Ok(AssetUsage {
            entries,
            total_bytes,
            soft_quota_bytes: self.settings.soft_quota_bytes,
            over_quota: total_bytes > self.settings.soft_quota_bytes,
        })
    }

    /// 关联资产到所有者角色（`owner_type`/`owner_id`/`role` 唯一）。
    ///
    /// # Errors
    ///
    /// 资产不存在或约束冲突时返回错误。
    pub fn link(&self, owner_type: &str, owner_id: &str, role: &str, asset: AssetId) -> Result<()> {
        let at = self.clock.now();
        let owner_type = owner_type.to_owned();
        let owner_id = owner_id.to_owned();
        let role = role.to_owned();
        self.db
            .write(move |tx| repo::link_asset(tx, &owner_type, &owner_id, &role, asset, at))
    }

    /// 全量校验：数据库每个资产行都必须有文件且哈希一致。
    ///
    /// # Errors
    ///
    /// 查询失败时返回错误（单个资产失败记录在报告里，不中止）。
    pub fn verify_all(&self) -> Result<AssetVerifyReport> {
        let ids: Vec<String> = self.db.read(|conn| {
            let mut statement = conn
                .prepare("SELECT id FROM asset ORDER BY id")
                .map_err(PlatformError::from)?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(PlatformError::from)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.map_err(PlatformError::from)?);
            }
            Ok(out)
        })?;

        let mut report = AssetVerifyReport::default();
        for text in ids {
            let Ok(asset) = AssetId::from_hex(&text) else {
                report
                    .failures
                    .push((text.clone(), "资产 id 不是合法哈希".to_owned()));
                continue;
            };
            report.checked += 1;
            if let Err(error) = self.verify(asset) {
                report.failures.push((text, error.to_string()));
            }
        }
        Ok(report)
    }

    /// 写入文件：同目录临时文件 → fsync → 改名 → 置只读（0444）。
    fn write_file_atomically(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent)
            .map_err(|source| PlatformError::io(parent.to_path_buf(), source))?;
        let counter = self.temp_counter.fetch_add(1, Ordering::Relaxed);
        let temp = parent.join(format!(".tmp-{}-{counter}", std::process::id()));
        {
            let mut file = std::fs::File::create(&temp)
                .map_err(|source| PlatformError::io(temp.clone(), source))?;
            file.write_all(bytes)
                .map_err(|source| PlatformError::io(temp.clone(), source))?;
            file.sync_all()
                .map_err(|source| PlatformError::io(temp.clone(), source))?;
        }
        std::fs::rename(&temp, path).map_err(|source| {
            let _ = std::fs::remove_file(&temp);
            PlatformError::io(path.to_path_buf(), source)
        })?;
        set_read_only(path)?;
        Ok(())
    }

    /// 资产库目录（诊断/备份使用）。
    #[must_use]
    pub fn root(&self) -> PathBuf {
        self.dirs.assets_dir()
    }

    /// 当前时间（调用方写清单时可能复用）。
    #[must_use]
    pub fn now(&self) -> Timestamp {
        self.clock.now()
    }
}

/// 置文件只读（unix 0444；其他平台尽力而为）。
fn set_read_only(path: &Path) -> Result<()> {
    let mut permissions = std::fs::metadata(path)
        .map_err(|source| PlatformError::io(path.to_path_buf(), source))?
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        permissions.set_mode(0o444);
    }
    #[cfg(not(unix))]
    {
        permissions.set_readonly(true);
    }
    std::fs::set_permissions(path, permissions)
        .map_err(|source| PlatformError::io(path.to_path_buf(), source))
}

/// 资产字节长度（诊断用；不读取内容）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetStat {
    /// 资产 id 十六进制。
    pub id: String,
    /// 字节数。
    pub byte_len: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;
    use eg_domain::Ulid;
    use std::sync::Arc;

    fn setup() -> (tempfile::TempDir, AssetStore) {
        let temp = tempfile::tempdir().unwrap();
        let dirs = DataDirs::with_root(temp.path().join("data")).unwrap();
        dirs.ensure().unwrap();
        let db = Arc::new(Database::open(&dirs.database_path()).unwrap());
        let clock: SharedClock =
            Arc::new(FixedClock::from_rfc3339("2026-11-02T12:00:00Z").unwrap());
        let store = AssetStore::new(dirs, db, clock, AssetSettings::default());
        (temp, store)
    }

    #[test]
    fn same_bytes_40_times_is_one_file_one_row() {
        let (_temp, store) = setup();
        let bytes = b"fake-jpeg-bytes-for-dedup";
        let mut deduped = 0;
        for _ in 0..40 {
            let outcome = store.put(bytes, "image/jpeg").unwrap();
            assert_eq!(outcome.byte_len, bytes.len() as u64);
            if outcome.deduped {
                deduped += 1;
            }
        }
        assert_eq!(deduped, 39, "40 次导入应 39 次去重命中");
        let usage = store.usage().unwrap();
        assert_eq!(usage.entries, 1, "资产行应为 1");
        assert_eq!(usage.total_bytes, bytes.len() as u64);
        let asset = AssetId::of(bytes);
        assert!(store.exists(asset));
        // 分片路径：assets/blake3/<2>/<2>/<hash>
        let path = store.path_of(asset);
        assert!(path.ends_with(asset.to_hex()));
        let components: Vec<_> = path.components().rev().take(4).collect();
        assert_eq!(components.len(), 4);
    }

    #[test]
    fn read_verifies_hash_and_detects_corruption() {
        let (_temp, store) = setup();
        let bytes = b"page-1";
        let asset = store.put(bytes, "image/png").unwrap().asset;
        assert_eq!(store.get(asset).unwrap(), bytes);
        store.verify(asset).unwrap();
        // 篡改文件（先改回可写）。
        let path = store.path_of(asset);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        std::fs::write(&path, b"tampered").unwrap();
        let error = store.get(asset).unwrap_err();
        assert_eq!(error.class(), crate::error::ErrorClass::DataCorruption);
        assert_eq!(store.verify_all().unwrap().failures.len(), 1);
    }

    #[test]
    fn missing_file_is_not_found_but_row_exists() {
        let (_temp, store) = setup();
        let bytes = b"missing-later";
        let asset = store.put(bytes, "image/jpeg").unwrap().asset;
        std::fs::remove_file(store.path_of(asset)).unwrap();
        let error = store.get(asset).unwrap_err();
        assert_eq!(error.class(), crate::error::ErrorClass::Permanent);
        assert!(error.to_string().contains("从备份恢复"));
        assert!(!store.verify_all().unwrap().ok());
    }

    #[test]
    fn files_are_read_only_after_write() {
        let (_temp, store) = setup();
        let asset = store.put(b"readonly", "image/jpeg").unwrap().asset;
        let metadata = std::fs::metadata(store.path_of(asset)).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(metadata.permissions().mode() & 0o777, 0o444);
        }
        #[cfg(not(unix))]
        assert!(metadata.permissions().readonly());
    }

    #[test]
    fn soft_quota_warning_without_deleting() {
        let temp = tempfile::tempdir().unwrap();
        let dirs = DataDirs::with_root(temp.path().join("data")).unwrap();
        dirs.ensure().unwrap();
        let db = Arc::new(Database::open(&dirs.database_path()).unwrap());
        let clock: SharedClock =
            Arc::new(FixedClock::from_rfc3339("2026-11-02T12:00:00Z").unwrap());
        let store = AssetStore::new(
            dirs,
            db,
            clock,
            AssetSettings {
                verify_hash_on_read: true,
                soft_quota_bytes: 8,
            },
        );
        store.put(b"12345", "image/jpeg").unwrap();
        store.put(b"67890", "image/jpeg").unwrap();
        let usage = store.usage().unwrap();
        assert_eq!(usage.entries, 2);
        assert!(usage.over_quota, "超过软配额应告警");
        assert!(store.exists(AssetId::of(b"12345")), "只告警不删除");
        assert_eq!(usage.soft_quota_bytes, 8);
    }

    #[test]
    fn link_and_verify_all_ok() {
        let (_temp, store) = setup();
        let asset = store.put(b"linked", "image/jpeg").unwrap().asset;
        store.link("page", "p1", "original", asset).unwrap();
        let report = store.verify_all().unwrap();
        assert!(report.ok());
        assert_eq!(report.checked, 1);
        // 同一角色不能重复关联
        assert!(store.link("page", "p1", "original", asset).is_err());
    }

    #[test]
    fn asset_stat_and_ulid_placeholder() {
        let stat = AssetStat {
            id: AssetId::of(b"x").to_hex(),
            byte_len: 1,
        };
        assert_eq!(stat.byte_len, 1);
        assert_ne!(Ulid::NIL, Ulid::from_u128(1));
    }
}
