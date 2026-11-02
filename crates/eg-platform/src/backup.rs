//! 备份与恢复（`SPEC-platform.md` §2 `backups/`、§4.2、§9.11）。
//!
//! - 迁移前快照由 [`crate::db::Database`] 生成；本模块提供**手动备份**、清单、代数清理与恢复。
//! - 备份内容：`VACUUM INTO` 的数据库快照 + `manifest.json`（schema 版本、表计数、资产统计、哈希）。
//! - 备份前后校验资产（可选）：任一资产哈希不符即拒绝备份（数据损坏，先修再备）。
//! - 恢复：先对当前库做安全副本，再覆盖目标库，并清理旧库残留的 `-wal`/`-shm` 侧车文件
//!   （否则 SQLite 下次打开会把旧 WAL 重放到新快照上，导致 `database disk image is malformed`；
//!   清理失败显式报错）。恢复后调用 [`BackupService::verify_restored`] 校验完整性与资产哈希
//!   （调用方负责先关闭数据库再恢复）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use eg_domain::{Sha256Hex, Timestamp};
use serde::{Deserialize, Serialize};

use crate::assets::AssetStore;
use crate::clock::SharedClock;
use crate::db::Database;
use crate::error::{PlatformError, Result};
use crate::paths::{DATABASE_FILE, DataDirs};
use crate::schema;

/// 备份类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupKind {
    /// 迁移前自动快照（由 `Database` 写入，清单由本模块识别）。
    Migration,
    /// 教师手动备份。
    Manual,
}

/// 备份清单（`manifest.json`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupManifest {
    /// 清单格式版本。
    pub manifest_version: u32,
    /// 备份类型。
    pub kind: BackupKind,
    /// 创建时间。
    pub created_at: Timestamp,
    /// 数据库 schema 版本。
    pub schema_version: u32,
    /// 数据库快照文件名（相对备份目录）。
    pub database_file: String,
    /// 数据库快照 SHA-256。
    pub database_sha256: Sha256Hex,
    /// 数据库字节数。
    pub database_bytes: u64,
    /// 各表行数（不含 app_meta）。
    pub table_counts: BTreeMap<String, u64>,
    /// 资产行数。
    pub asset_count: u64,
    /// 资产字节总数。
    pub asset_bytes: u64,
}

/// 一次备份的记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupRecord {
    /// 备份目录。
    pub directory: PathBuf,
    /// 清单。
    pub manifest: BackupManifest,
}

/// 恢复报告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreReport {
    /// 使用的备份目录。
    pub backup_directory: PathBuf,
    /// 目标数据库路径。
    pub target: PathBuf,
    /// 当前库的安全副本路径（None = 原本不存在）。
    pub safety_copy: Option<PathBuf>,
    /// 完成时间。
    pub restored_at: Timestamp,
}

/// 恢复后校验报告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreVerification {
    /// 完整性检查通过。
    pub integrity_ok: bool,
    /// schema 版本。
    pub schema_version: u32,
    /// 表计数。
    pub table_counts: BTreeMap<String, u64>,
    /// 资产校验数量。
    pub assets_checked: u64,
    /// 资产校验失败数量。
    pub asset_failures: u64,
}

impl RestoreVerification {
    /// 是否全部通过（完整性 + 无资产失败）。
    #[must_use]
    pub fn ok(&self) -> bool {
        self.integrity_ok && self.asset_failures == 0
    }
}

/// 备份服务。
#[derive(Clone)]
pub struct BackupService {
    dirs: DataDirs,
    clock: SharedClock,
    keep_generations: u32,
}

impl std::fmt::Debug for BackupService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackupService")
            .field("backups_dir", &self.dirs.backups_dir())
            .field("keep_generations", &self.keep_generations)
            .finish_non_exhaustive()
    }
}

impl BackupService {
    /// 构造（`keep_generations` 来自 `backup.keep_generations`）。
    #[must_use]
    pub fn new(dirs: DataDirs, clock: SharedClock, keep_generations: u32) -> Self {
        Self {
            dirs,
            clock,
            keep_generations: keep_generations.max(1),
        }
    }

    /// 执行备份（数据库快照 + 清单；可选资产前置校验）。
    ///
    /// # Errors
    ///
    /// 资产校验失败（数据损坏）、快照失败或清单写入失败时返回错误。
    pub fn backup(
        &self,
        db: &Database,
        assets: Option<&AssetStore>,
        kind: BackupKind,
    ) -> Result<BackupRecord> {
        if let Some(assets) = assets {
            let report = assets.verify_all()?;
            if !report.ok() {
                let first = report.failures.first().map_or_else(
                    || "未知资产".to_owned(),
                    |(id, reason)| format!("{id}：{reason}"),
                );
                return Err(PlatformError::DatabaseCorrupt(format!(
                    "备份前资产校验失败（{}/{})，首个失败：{first}",
                    report.failures.len(),
                    report.checked
                )));
            }
        }

        let now = self.clock.now();
        let stamp = now.format("%Y%m%dT%H%M%S%.3f").to_string();
        let directory = self.dirs.backups_dir().join(stamp);
        std::fs::create_dir_all(&directory)
            .map_err(|source| PlatformError::io(directory.clone(), source))?;
        let database_path = directory.join(DATABASE_FILE);
        db.snapshot_to(&database_path)?;

        let database_bytes = std::fs::metadata(&database_path)
            .map(|meta| meta.len())
            .map_err(|source| PlatformError::io(database_path.clone(), source))?;
        let database_bytes_vec = std::fs::read(&database_path)
            .map_err(|source| PlatformError::io(database_path.clone(), source))?;
        let database_sha256 = Sha256Hex::of(&database_bytes_vec);

        let (schema_version, table_counts) = db.read(|conn| {
            let version = schema::read_version(conn)?;
            let mut counts = BTreeMap::new();
            for table in schema::TABLES {
                if table == &"app_meta" {
                    continue;
                }
                let count: i64 = conn
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })
                    .map_err(PlatformError::from)?;
                counts.insert((*table).to_owned(), u64::try_from(count).unwrap_or(0));
            }
            Ok((version, counts))
        })?;
        let (asset_count, asset_bytes) = db.read(crate::repo::asset_usage)?;

        let manifest = BackupManifest {
            manifest_version: 1,
            kind,
            created_at: now,
            schema_version,
            database_file: DATABASE_FILE.to_owned(),
            database_sha256,
            database_bytes,
            table_counts,
            asset_count,
            asset_bytes,
        };
        let manifest_text = serde_json::to_string_pretty(&manifest)
            .map_err(|error| PlatformError::Serialize(error.to_string()))?;
        let manifest_path = directory.join("manifest.json");
        std::fs::write(&manifest_path, manifest_text)
            .map_err(|source| PlatformError::io(manifest_path, source))?;
        Ok(BackupRecord {
            directory,
            manifest,
        })
    }

    /// 列出现有备份（按目录名倒序，最新在前）。
    ///
    /// # Errors
    ///
    /// 读取备份目录失败或清单损坏时返回错误。
    pub fn list(&self) -> Result<Vec<BackupRecord>> {
        let root = self.dirs.backups_dir();
        let entries = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(PlatformError::io(root, source)),
        };
        let mut records = Vec::new();
        for entry in entries {
            let entry =
                entry.map_err(|source| PlatformError::io(self.dirs.backups_dir(), source))?;
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let manifest_path = entry.path().join("manifest.json");
            if !manifest_path.exists() {
                continue;
            }
            let text = std::fs::read_to_string(&manifest_path)
                .map_err(|source| PlatformError::io(manifest_path.clone(), source))?;
            let manifest: BackupManifest = serde_json::from_str(&text).map_err(|error| {
                PlatformError::DatabaseCorrupt(format!(
                    "备份清单损坏（{}）：{error}",
                    manifest_path.display()
                ))
            })?;
            records.push(BackupRecord {
                directory: entry.path(),
                manifest,
            });
        }
        records.sort_by(|a, b| b.directory.cmp(&a.directory));
        Ok(records)
    }

    /// 按代数清理最旧的备份（`backup.keep_generations`）。
    ///
    /// # Errors
    ///
    /// 读取或删除失败时返回错误。
    pub fn prune(&self) -> Result<u64> {
        let records = self.list()?;
        let keep = usize::try_from(self.keep_generations).unwrap_or(usize::MAX);
        let mut deleted = 0u64;
        for record in records.into_iter().skip(keep) {
            std::fs::remove_dir_all(&record.directory)
                .map_err(|source| PlatformError::io(record.directory.clone(), source))?;
            deleted += 1;
        }
        Ok(deleted)
    }

    /// 从备份目录恢复目标数据库（调用方必须先关闭数据库）。
    ///
    /// 当前目标库（若存在）会先复制到 `<备份根>/<时间戳>-restore-safety/`。
    ///
    /// # Errors
    ///
    /// 清单缺失/损坏、安全副本或复制失败时返回错误。
    pub fn restore(&self, backup_directory: &Path, target_db: &Path) -> Result<RestoreReport> {
        let manifest_path = backup_directory.join("manifest.json");
        if !manifest_path.exists() {
            return Err(PlatformError::NotFound(format!(
                "备份目录不存在或缺少清单：{}",
                backup_directory.display()
            )));
        }
        let text = std::fs::read_to_string(&manifest_path)
            .map_err(|source| PlatformError::io(manifest_path.clone(), source))?;
        let manifest: BackupManifest = serde_json::from_str(&text)
            .map_err(|error| PlatformError::DatabaseCorrupt(format!("备份清单损坏：{error}")))?;
        let source_db = backup_directory.join(&manifest.database_file);
        if !source_db.exists() {
            return Err(PlatformError::NotFound(format!(
                "备份中缺少数据库快照：{}",
                source_db.display()
            )));
        }
        let now = self.clock.now();
        let safety_copy = if target_db.exists() {
            let stamp = now.format("%Y%m%dT%H%M%S%.3f").to_string();
            let safety_dir = self
                .dirs
                .backups_dir()
                .join(format!("{stamp}-restore-safety"));
            std::fs::create_dir_all(&safety_dir)
                .map_err(|source| PlatformError::io(safety_dir.clone(), source))?;
            let safety_path = safety_dir.join(DATABASE_FILE);
            std::fs::copy(target_db, &safety_path)
                .map_err(|source| PlatformError::io(safety_path.clone(), source))?;
            // 一并保留 WAL/SHM（若存在），便于人工取证。
            for suffix in ["-wal", "-shm"] {
                let sidecar = PathBuf::from(format!("{}{suffix}", target_db.display()));
                if sidecar.exists() {
                    let destination = safety_dir.join(format!("{DATABASE_FILE}{suffix}"));
                    std::fs::copy(&sidecar, &destination)
                        .map_err(|source| PlatformError::io(destination, source))?;
                }
            }
            Some(safety_path)
        } else {
            None
        };
        if let Some(parent) = target_db.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|source| PlatformError::io(parent.to_path_buf(), source))?;
        }
        std::fs::copy(&source_db, target_db)
            .map_err(|source| PlatformError::io(target_db.to_path_buf(), source))?;
        // 目标库刚被替换，旧库的 WAL/SHM 侧车文件必须清掉：SQLite 打开时会优先重放
        // 同目录的 `-wal`，把旧库事务帧应用到新快照上 ⇒ `database disk image is malformed`。
        // 正常关闭（含强杀）后侧车都可能残留，因此在恢复处统一清理；安全副本已一并保留
        // （取证用，见上）。删除失败显式报错，绝不静默跳过。
        for suffix in ["-wal", "-shm"] {
            let sidecar = PathBuf::from(format!("{}{suffix}", target_db.display()));
            match std::fs::remove_file(&sidecar) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => return Err(PlatformError::io(sidecar, source)),
            }
        }
        Ok(RestoreReport {
            backup_directory: backup_directory.to_path_buf(),
            target: target_db.to_path_buf(),
            safety_copy,
            restored_at: now,
        })
    }

    /// 恢复后校验：完整性检查 + 表计数 + 全量资产哈希。
    ///
    /// # Errors
    ///
    /// 打开数据库或查询失败时返回错误（校验不通过记录在报告里）。
    pub fn verify_restored(
        &self,
        target_db: &Path,
        assets: &AssetStore,
    ) -> Result<RestoreVerification> {
        let conn = rusqlite::Connection::open_with_flags(
            target_db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(PlatformError::from)?;
        let integrity_ok = schema::integrity_check(&conn).is_ok();
        let schema_version = schema::read_version(&conn)?;
        let mut table_counts = BTreeMap::new();
        for table in schema::TABLES {
            if table == &"app_meta" {
                continue;
            }
            let count: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .map_err(PlatformError::from)?;
            table_counts.insert((*table).to_owned(), u64::try_from(count).unwrap_or(0));
        }
        drop(conn);
        let report = assets.verify_all()?;
        Ok(RestoreVerification {
            integrity_ok,
            schema_version,
            table_counts,
            assets_checked: report.checked,
            asset_failures: u64::try_from(report.failures.len()).unwrap_or(u64::MAX),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::assets::{AssetSettings, AssetStore};
    use crate::clock::FixedClock;
    use eg_domain::{Classroom, ClassroomId, Ulid};
    use std::sync::Arc;

    fn timestamp(text: &str) -> Timestamp {
        eg_domain::time::parse_rfc3339(text).unwrap()
    }

    struct Harness {
        temp: tempfile::TempDir,
        dirs: DataDirs,
        db: Arc<Database>,
        assets: AssetStore,
        backup: BackupService,
        clock: Arc<FixedClock>,
    }

    fn harness() -> Harness {
        let temp = tempfile::tempdir().unwrap();
        let dirs = DataDirs::with_root(temp.path().join("data")).unwrap();
        dirs.ensure().unwrap();
        let clock = Arc::new(FixedClock::new(timestamp("2026-11-02T12:00:00Z")));
        let db = Arc::new(
            Database::open_with(
                &dirs.database_path(),
                &crate::db::DatabaseOptions {
                    backup_dir: Some(dirs.backups_dir()),
                    clock: Some(clock.clone()),
                },
            )
            .unwrap(),
        );
        let assets = AssetStore::new(
            dirs.clone(),
            db.clone(),
            clock.clone(),
            AssetSettings::default(),
        );
        let backup = BackupService::new(dirs.clone(), clock.clone(), 2);
        Harness {
            temp,
            dirs,
            db,
            assets,
            backup,
            clock,
        }
    }

    fn insert_classroom(h: &Harness, name: &str) {
        let classroom = Classroom {
            id: ClassroomId::from_ulid(Ulid::from_u128(1)),
            name: name.to_owned(),
            term: "2026 秋".to_owned(),
        };
        h.db.write(move |tx| {
            crate::repo::insert_classroom(tx, &classroom, None, timestamp("2026-11-02T12:00:00Z"))
        })
        .unwrap();
    }

    #[test]
    fn backup_creates_manifest_and_snapshot() {
        let h = harness();
        insert_classroom(&h, "高一(1)班");
        h.assets.put(b"page-bytes", "image/jpeg").unwrap();
        let record = h
            .backup
            .backup(&h.db, Some(&h.assets), BackupKind::Manual)
            .unwrap();
        assert!(record.directory.exists());
        assert_eq!(record.manifest.schema_version, schema::SCHEMA_VERSION);
        assert_eq!(record.manifest.table_counts["classroom"], 1);
        assert_eq!(record.manifest.asset_count, 1);
        assert!(record.manifest.database_bytes > 0);
        assert!(record.directory.join("manifest.json").exists());
        // 快照可独立打开且完整性通过。
        let snapshot = record.directory.join(DATABASE_FILE);
        let conn = rusqlite::Connection::open(&snapshot).unwrap();
        schema::integrity_check(&conn).unwrap();
        let listed = h.backup.list().unwrap();
        assert_eq!(listed.len(), 1);
    }

    #[test]
    fn backup_refuses_when_asset_is_corrupted() {
        let h = harness();
        let outcome = h.assets.put(b"bytes", "image/jpeg").unwrap();
        // 篡改资产文件（先恢复可写）。
        let path = h.assets.path_of(outcome.asset);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        std::fs::write(&path, b"tampered").unwrap();
        let error = h
            .backup
            .backup(&h.db, Some(&h.assets), BackupKind::Manual)
            .unwrap_err();
        assert_eq!(error.class(), crate::error::ErrorClass::DataCorruption);
        assert!(error.to_string().contains("备份前资产校验失败"));
    }

    #[test]
    fn restore_roundtrip_and_verify() {
        let h = harness();
        insert_classroom(&h, "高一(1)班");
        let asset = h.assets.put(b"page-bytes", "image/jpeg").unwrap().asset;
        let record = h
            .backup
            .backup(&h.db, Some(&h.assets), BackupKind::Manual)
            .unwrap();

        let target = h.temp.path().join("restored.sqlite3");
        let report = h.backup.restore(&record.directory, &target).unwrap();
        assert!(report.safety_copy.is_none(), "目标原本不存在时无安全副本");
        assert!(target.exists());
        let verification = h.backup.verify_restored(&target, &h.assets).unwrap();
        assert!(verification.ok(), "{verification:?}");
        assert_eq!(verification.schema_version, schema::SCHEMA_VERSION);
        assert_eq!(verification.table_counts["classroom"], 1);
        assert_eq!(verification.assets_checked, 1);
        assert_eq!(verification.asset_failures, 0);
        assert!(h.assets.verify(asset).is_ok());
    }

    #[test]
    fn restore_keeps_safety_copy_of_existing_target() {
        let h = harness();
        insert_classroom(&h, "原班级");
        let record = h
            .backup
            .backup(&h.db, Some(&h.assets), BackupKind::Manual)
            .unwrap();
        let target = h.dirs.database_path();
        // 目标存在（当前库）→ 恢复前应有安全副本。
        let report = h.backup.restore(&record.directory, &target).unwrap();
        let safety = report.safety_copy.expect("应生成安全副本");
        assert!(safety.exists());
        assert!(report.restored_at >= timestamp("2026-11-02T12:00:00Z"));
    }

    #[test]
    fn prune_keeps_configured_generations() {
        let h = harness();
        for _ in 0..3 {
            h.backup
                .backup(&h.db, Some(&h.assets), BackupKind::Manual)
                .unwrap();
            h.clock.advance(chrono::Duration::seconds(1)).unwrap();
        }
        assert_eq!(h.backup.list().unwrap().len(), 3);
        let deleted = h.backup.prune().unwrap();
        assert_eq!(deleted, 1, "keep_generations=2");
        assert_eq!(h.backup.list().unwrap().len(), 2);
    }

    #[test]
    fn missing_backup_reports_explicit_error() {
        let h = harness();
        let error = h
            .backup
            .restore(
                &h.temp.path().join("nope"),
                &h.temp.path().join("x.sqlite3"),
            )
            .unwrap_err();
        assert_eq!(error.class(), crate::error::ErrorClass::Permanent);
        assert!(error.to_string().contains("备份目录不存在或缺少清单"));
    }
}
