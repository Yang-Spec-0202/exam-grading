//! 组合入口：把配置、数据库、资产库、任务、日志、审计、授权、备份装配为 [`Platform`]
//! （`SPEC-platform.md` §3：上层模块禁止直连 SQLite/资产目录/Keychain）。
//!
//! 启动顺序（失败显式报错，不静默降级）：
//! 数据目录 → 单实例锁 → 配置 → 数据库（迁移前快照）→ 索引（可选）→ 日志 → 资产库 →
//! 审计 → 任务队列 → 授权 → 备份服务 → 启动恢复（租约过期任务归位）。

use std::path::PathBuf;
use std::sync::Arc;

use crate::assets::{AssetSettings, AssetStore};
use crate::audit::AuditLog;
use crate::backup::BackupService;
use crate::clock::{SharedClock, SystemClock};
use crate::config::{AppConfig, ConfigDocument, ConfigStore};
use crate::consent::ConsentStore;
use crate::db::{Database, DatabaseOptions};
use crate::diagnostics::Diagnostics;
use crate::error::Result;
use crate::ids::UlidFactory;
use crate::index::LogIndex;
use crate::instance::InstanceLock;
use crate::logging::{LogLevel, LogRecord, LogSettings, LogSink};
use crate::paths::DataDirs;
use crate::tasks::TaskQueue;

/// 单机应用的基础设施组合根。
pub struct Platform {
    dirs: DataDirs,
    clock: SharedClock,
    ids: Arc<UlidFactory>,
    config_store: ConfigStore,
    config: ConfigDocument,
    db: Arc<Database>,
    index: Option<Arc<LogIndex>>,
    log: LogSink,
    assets: Arc<AssetStore>,
    audit: Arc<AuditLog>,
    tasks: TaskQueue,
    consents: ConsentStore,
    backup: BackupService,
    instance: InstanceLock,
}

impl std::fmt::Debug for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Platform")
            .field("data_root", &self.dirs.root())
            .field("schema_version", &self.config.meta.schema_version)
            .finish_non_exhaustive()
    }
}

impl Platform {
    /// 用系统时钟打开平台（生产入口）。
    ///
    /// # Errors
    ///
    /// 单实例冲突、配置非法/损坏、数据库迁移或打开失败、日志/索引初始化失败时返回错误。
    pub fn open(dirs: DataDirs) -> Result<Self> {
        Self::open_with(dirs, Arc::new(SystemClock))
    }

    /// 用注入时钟打开平台（测试/回放）。
    ///
    /// # Errors
    ///
    /// 同 [`Platform::open`]。
    pub fn open_with(dirs: DataDirs, clock: SharedClock) -> Result<Self> {
        dirs.ensure()?;
        let instance = InstanceLock::acquire(&dirs, clock.now(), env!("CARGO_PKG_VERSION"))?;
        let ids = Arc::new(UlidFactory::with_persistence(
            clock.clone(),
            dirs.ulid_state_path(),
        )?);
        let config_store = ConfigStore::new(dirs.clone(), clock.clone(), ids.clone());
        let config = config_store.load()?;
        let db = Arc::new(Database::open_with(
            &dirs.database_path(),
            &DatabaseOptions {
                backup_dir: Some(dirs.backups_dir()),
                clock: Some(clock.clone()),
            },
        )?);

        // 全文索引（失败不阻断启动：显式降级，检索退化为字段过滤）。
        let index = if config.config.log.search_index {
            match LogIndex::open(&dirs.log_dir().join("log-index.sqlite3")) {
                Ok(index) => {
                    let index = Arc::new(index);
                    let now = clock.now();
                    let _ = index.sync(dirs.log_dir(), config.config.log.index_range_days, now);
                    Some(index)
                }
                Err(_) => None,
            }
        } else {
            None
        };

        let log = LogSink::new(
            dirs.log_dir(),
            LogSettings::from(&config.config.log),
            clock.clone(),
            index.clone(),
        )?;

        // 启动首行：声明路径覆盖（若存在）。
        if let Some(note) = dirs.env_override_note() {
            let record = LogRecord::new(
                clock.now(),
                LogLevel::Warn,
                "eg_platform",
                "storage.override",
                note,
            );
            log.write(record)?;
        }

        let quota_bytes =
            u64::from(config.config.storage.soft_quota_gb).saturating_mul(1024 * 1024 * 1024);
        let assets = Arc::new(AssetStore::new(
            dirs.clone(),
            db.clone(),
            clock.clone(),
            AssetSettings {
                verify_hash_on_read: config.config.storage.verify_hash_on_read,
                soft_quota_bytes: quota_bytes,
            },
        ));
        let audit = Arc::new(AuditLog::new(db.clone(), clock.clone(), ids.clone()));
        let tasks = TaskQueue::new(db.clone(), clock.clone(), ids.clone(), audit.clone());
        let consents = ConsentStore::new(db.clone(), clock.clone(), ids.clone());
        let backup = BackupService::new(
            dirs.clone(),
            clock.clone(),
            config.config.backup.keep_generations,
        );

        let platform = Self {
            dirs,
            clock,
            ids,
            config_store,
            config,
            db,
            index,
            log,
            assets,
            audit,
            tasks,
            consents,
            backup,
            instance,
        };

        // 启动恢复：租约过期的运行中任务回待运行（强杀/崩溃后的归位）。
        let recovered = platform.tasks.recover_expired()?;
        let record = LogRecord::new(
            platform.clock.now(),
            LogLevel::Info,
            "eg_platform",
            "platform.start",
            format!(
                "平台已启动（schema v{}，恢复 {} 个过期任务）",
                platform.config.meta.schema_version, recovered
            ),
        )
        .with_attempt(u32::try_from(recovered).unwrap_or(u32::MAX));
        platform.log.write(record)?;
        Ok(platform)
    }

    /// 数据目录布局。
    #[must_use]
    pub fn dirs(&self) -> &DataDirs {
        &self.dirs
    }

    /// 用户配置（只读快照；修改请经 [`Platform::save_config`]）。
    #[must_use]
    pub fn config(&self) -> &AppConfig {
        &self.config.config
    }

    /// 配置文档（含 `meta`）。
    #[must_use]
    pub fn config_document(&self) -> &ConfigDocument {
        &self.config
    }

    /// 保存配置（校验 + 原子写 + `.bak`；调用方负责写审计）。
    ///
    /// # Errors
    ///
    /// 配置非法或写入失败时返回错误。
    pub fn save_config(&mut self, document: ConfigDocument) -> Result<()> {
        self.config_store.save(&document)?;
        self.config = document;
        Ok(())
    }

    /// 业务库。
    #[must_use]
    pub fn db(&self) -> &Arc<Database> {
        &self.db
    }

    /// 资产库。
    #[must_use]
    pub fn assets(&self) -> &Arc<AssetStore> {
        &self.assets
    }

    /// 审计。
    #[must_use]
    pub fn audit(&self) -> &Arc<AuditLog> {
        &self.audit
    }

    /// 任务队列。
    #[must_use]
    pub fn tasks(&self) -> &TaskQueue {
        &self.tasks
    }

    /// 云端授权存储。
    #[must_use]
    pub fn consents(&self) -> &ConsentStore {
        &self.consents
    }

    /// 日志落盘器。
    #[must_use]
    pub fn log(&self) -> &LogSink {
        &self.log
    }

    /// 日志全文索引（未开启/不可用为 `None`）。
    #[must_use]
    pub fn index(&self) -> Option<&Arc<LogIndex>> {
        self.index.as_ref()
    }

    /// 备份服务。
    #[must_use]
    pub fn backup(&self) -> &BackupService {
        &self.backup
    }

    /// 单实例锁信息。
    #[must_use]
    pub fn instance(&self) -> &InstanceLock {
        &self.instance
    }

    /// ULID 工厂。
    #[must_use]
    pub fn ids(&self) -> &Arc<UlidFactory> {
        &self.ids
    }

    /// 时钟。
    #[must_use]
    pub fn clock(&self) -> &SharedClock {
        &self.clock
    }

    /// 构造诊断包导出器。
    #[must_use]
    pub fn diagnostics(&self) -> Diagnostics {
        Diagnostics::new(
            self.dirs.clone(),
            self.db.clone(),
            self.log.clone(),
            self.clock.clone(),
        )
    }

    /// 关闭：落盘日志、关闭数据库写线程、释放单实例锁。
    ///
    /// # Errors
    ///
    /// 日志或数据库关闭失败时返回错误。
    pub fn close(self) -> Result<()> {
        let record = LogRecord::new(
            self.clock.now(),
            LogLevel::Info,
            "eg_platform",
            "platform.stop",
            "平台正常关闭",
        );
        let _ = self.log.write(record);
        let _ = self.log.flush();
        let log_result = self.log.close();
        let db_result = self.db.close();
        drop(self.instance);
        log_result?;
        db_result?;
        Ok(())
    }

    /// 数据根路径（诊断/日志查看入口用）。
    #[must_use]
    pub fn data_root(&self) -> PathBuf {
        self.dirs.root().to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::clock::FixedClock;
    use crate::tasks::{NewTask, TaskKind};
    use eg_domain::Timestamp;

    fn timestamp(text: &str) -> Timestamp {
        eg_domain::time::parse_rfc3339(text).unwrap()
    }

    fn open(temp: &tempfile::TempDir) -> Platform {
        let dirs = DataDirs::with_root(temp.path().join("data")).unwrap();
        let clock: SharedClock = Arc::new(FixedClock::new(timestamp("2026-11-02T12:00:00Z")));
        Platform::open_with(dirs, clock).unwrap()
    }

    #[test]
    fn open_creates_config_database_and_logs() {
        let temp = tempfile::tempdir().unwrap();
        let platform = open(&temp);
        assert!(platform.dirs().config_path().exists());
        assert!(platform.dirs().database_path().exists());
        assert_eq!(platform.config().log.retention_days, 30);
        assert_eq!(
            platform.config().general.mode,
            eg_domain::GradeMode::Standard
        );
        assert_eq!(
            platform.db().schema_version().unwrap(),
            crate::schema::SCHEMA_VERSION
        );
        assert!(platform.log().log_dir().is_dir());
        platform.close().unwrap();
    }

    #[test]
    fn end_to_end_smoke() {
        let temp = tempfile::tempdir().unwrap();
        let platform = open(&temp);

        // 资产入库 + 读取校验。
        let put = platform.assets().put(b"page-bytes", "image/jpeg").unwrap();
        assert_eq!(platform.assets().get(put.asset).unwrap(), b"page-bytes");

        // 任务：提交 → 认领 → 心跳 → 完成（自动写审计）。
        let task = platform
            .tasks()
            .enqueue(NewTask::new(
                TaskKind::PageRecognize,
                serde_json::json!({"page": 1}),
            ))
            .unwrap();
        let claimed = platform.tasks().claim("worker-1").unwrap().unwrap();
        assert_eq!(claimed.id, task.task_id);
        platform
            .tasks()
            .heartbeat(task.task_id, "worker-1")
            .unwrap();
        platform.tasks().complete(task.task_id).unwrap();
        assert_eq!(
            platform.tasks().get(task.task_id).unwrap().unwrap().state,
            crate::tasks::TaskState::Succeeded
        );

        // 日志：写入 + 字段查询。
        platform
            .log()
            .write(
                LogRecord::new(
                    platform.clock().now(),
                    LogLevel::Info,
                    "eg_grade",
                    "grade.done",
                    "判分完成",
                )
                .with_task(task.task_id),
            )
            .unwrap();
        platform.log().flush().unwrap();
        let page = platform
            .log()
            .query(&crate::logging::LogQuery {
                module: Some("eg_grade".to_owned()),
                ..crate::logging::LogQuery::default()
            })
            .unwrap();
        assert!(page.total >= 1);

        // 备份 + 诊断包。
        let backup = platform
            .backup()
            .backup(
                platform.db(),
                Some(platform.assets()),
                crate::backup::BackupKind::Manual,
            )
            .unwrap();
        assert!(backup.directory.exists());
        let bundle = platform
            .diagnostics()
            .export(&crate::diagnostics::DiagnosticRequest::default())
            .unwrap();
        assert!(bundle.path.exists());

        // 审计链完整。
        assert!(platform.audit().verify_chain().unwrap().ok);

        platform.close().unwrap();
    }

    #[test]
    fn second_instance_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let first = open(&temp);
        let dirs = DataDirs::with_root(temp.path().join("data")).unwrap();
        let clock: SharedClock = Arc::new(FixedClock::new(timestamp("2026-11-02T12:00:00Z")));
        let error = Platform::open_with(dirs, clock).unwrap_err();
        assert_eq!(error.class(), crate::error::ErrorClass::TeacherFixable);
        assert!(error.to_string().contains("已有实例在运行"));
        first.close().unwrap();
        // 关闭后可重新打开。
        let reopened = open(&temp);
        reopened.close().unwrap();
    }

    #[test]
    fn corrupt_config_blocks_startup_explicitly() {
        let temp = tempfile::tempdir().unwrap();
        let dirs = DataDirs::with_root(temp.path().join("data")).unwrap();
        dirs.ensure().unwrap();
        std::fs::write(dirs.config_path(), "这不是 TOML [").unwrap();
        let clock: SharedClock = Arc::new(FixedClock::new(timestamp("2026-11-02T12:00:00Z")));
        let error = Platform::open_with(dirs, clock).unwrap_err();
        assert_eq!(error.class(), crate::error::ErrorClass::TeacherFixable);
        assert!(error.to_string().contains("配置文件损坏"));
    }

    #[test]
    fn config_roundtrip_via_platform() {
        let temp = tempfile::tempdir().unwrap();
        let mut platform = open(&temp);
        let mut document = platform.config_document().clone();
        document.config.log.level = LogLevel::Debug;
        document.config.ui.page_size = 200;
        platform.save_config(document).unwrap();
        assert_eq!(platform.config().log.level, LogLevel::Debug);
        assert_eq!(platform.config().ui.page_size, 200);
        platform.close().unwrap();
        // 重新打开后配置保持。
        let platform = open(&temp);
        assert_eq!(platform.config().log.level, LogLevel::Debug);
        platform.close().unwrap();
    }
}
