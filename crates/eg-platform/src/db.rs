//! 数据库连接：单写连接 actor（有界命令队列，满则反压）+ 只读查询连接
//! （`SPEC-platform.md` §4.1）。
//!
//! 关键不变量：
//! - 写操作只在专属线程的**单写连接**上执行，调用方拿到的闭包在事务中运行；
//! - 读连接以 `query_only=ON` 打开，与写连接分离；
//! - 打开时检查 `schema_version`，需要迁移则先做数据库快照（迁移前快照）；
//! - 迁移失败即返回错误并进入调用方的「维护模式」判定。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use eg_domain::Clock;
use rusqlite::{Connection, Transaction};

use crate::clock::SharedClock;
use crate::error::{PlatformError, Result};
use crate::schema;

/// busy timeout（毫秒），`SPEC-platform.md` §4.1：5s。
pub const BUSY_TIMEOUT_MS: u64 = 5_000;

/// 写命令队列深度（满则背压调用方）。
pub const WRITER_QUEUE_DEPTH: usize = 64;

type BoxedJob = Box<dyn FnOnce(&mut Connection) + Send + 'static>;

enum WriteCommand {
    Run(BoxedJob),
    Shutdown,
}

struct WriterHandle {
    sender: SyncSender<WriteCommand>,
    join: Mutex<Option<JoinHandle<()>>>,
}

/// 打开数据库的选项。
#[derive(Clone, Default)]
pub struct DatabaseOptions {
    /// 迁移前快照目录（None = 不快照）。
    pub backup_dir: Option<PathBuf>,
    /// 快照命名用时钟（None = 系统时钟）。
    pub clock: Option<SharedClock>,
}

impl std::fmt::Debug for DatabaseOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DatabaseOptions")
            .field("backup_dir", &self.backup_dir)
            .finish_non_exhaustive()
    }
}

/// 业务库（单写连接 + 只读连接）。
pub struct Database {
    path: PathBuf,
    writer: Arc<WriterHandle>,
    read: Mutex<Connection>,
    closing: Arc<AtomicBool>,
}

impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Database")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl Database {
    /// 打开数据库（不生成迁移前快照）。
    ///
    /// # Errors
    ///
    /// 打开、迁移、完整性/表校验失败时返回错误。
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with(path, &DatabaseOptions::default())
    }

    /// 打开数据库：按需生成迁移前快照 → 迁移 → 启动校验 → 启动写线程与读连接。
    ///
    /// # Errors
    ///
    /// 打开、快照、迁移或校验失败时返回错误（调用方应进入只读维护模式）。
    pub fn open_with(path: &Path, options: &DatabaseOptions) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|source| PlatformError::io(parent.to_path_buf(), source))?;
        }
        let existed = path.exists();

        // 1) 迁移前快照（仅当已有库且版本落后）。
        if existed {
            let probe = Connection::open(path).map_err(PlatformError::from)?;
            probe
                .busy_timeout(Duration::from_millis(BUSY_TIMEOUT_MS))
                .map_err(PlatformError::from)?;
            let before = schema::read_version(&probe)?;
            if before < schema::SCHEMA_VERSION
                && let Some(backup_dir) = &options.backup_dir
            {
                let clock = options
                    .clock
                    .clone()
                    .unwrap_or_else(|| Arc::new(crate::clock::SystemClock));
                snapshot_database(&probe, backup_dir, clock.as_ref())?;
            }
            drop(probe);
        }

        // 2) 写连接：配置 → 迁移 → 启动校验。
        let mut writer_conn = Connection::open(path).map_err(PlatformError::from)?;
        configure_connection(&writer_conn)?;
        let outcome = schema::apply(&mut writer_conn)?;
        if outcome.migrated() {
            schema::verify_tables(&writer_conn)?;
            schema::integrity_check(&writer_conn)?;
        }

        // 3) 写线程。
        let (sender, receiver) = mpsc::sync_channel(WRITER_QUEUE_DEPTH);
        let join = std::thread::Builder::new()
            .name("eg-db-writer".to_owned())
            .spawn(move || writer_loop(&mut writer_conn, &receiver))
            .map_err(|source| PlatformError::io(path.to_path_buf(), source))?;

        // 4) 只读连接（query_only 强制只读）。
        let read = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(PlatformError::from)?;
        read.busy_timeout(Duration::from_millis(BUSY_TIMEOUT_MS))
            .map_err(PlatformError::from)?;
        read.pragma_update(None, "query_only", true)
            .map_err(PlatformError::from)?;

        Ok(Self {
            path: path.to_path_buf(),
            writer: Arc::new(WriterHandle {
                sender,
                join: Mutex::new(Some(join)),
            }),
            read: Mutex::new(read),
            closing: Arc::new(AtomicBool::new(false)),
        })
    }

    /// 业务库路径。
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 当前 schema 版本。
    ///
    /// # Errors
    ///
    /// 查询失败时返回错误。
    pub fn schema_version(&self) -> Result<u32> {
        self.read(schema::read_version)
    }

    /// 在单写连接的事务中执行闭包；闭包返回 `Err` 即回滚。
    ///
    /// # Errors
    ///
    /// 写线程不可用、事务开始/提交失败或闭包错误时返回错误。
    pub fn write<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Transaction<'_>) -> Result<T> + Send + 'static,
    {
        if self.closing.load(Ordering::SeqCst) {
            return Err(PlatformError::Internal("数据库正在关闭".to_owned()));
        }
        let (reply_tx, reply_rx) = mpsc::sync_channel::<Result<T>>(1);
        let job: BoxedJob = Box::new(move |conn: &mut Connection| {
            let mut transaction = match conn.transaction() {
                Ok(transaction) => transaction,
                Err(error) => {
                    let _ = reply_tx.send(Err(PlatformError::from(error)));
                    return;
                }
            };
            let result = match f(&mut transaction) {
                Ok(value) => transaction
                    .commit()
                    .map(|()| value)
                    .map_err(PlatformError::from),
                Err(error) => Err(error),
            };
            let _ = reply_tx.send(result);
        });
        self.writer
            .sender
            .send(WriteCommand::Run(job))
            .map_err(|_| PlatformError::Internal("数据库写线程不可用".to_owned()))?;
        reply_rx
            .recv()
            .map_err(|_| PlatformError::Internal("数据库写线程未返回结果".to_owned()))?
    }

    /// 在单写连接上执行原始闭包（不使用事务；用于 `VACUUM`/checkpoint）。
    ///
    /// # Errors
    ///
    /// 写线程不可用或闭包错误时返回错误。
    pub fn write_raw<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        if self.closing.load(Ordering::SeqCst) {
            return Err(PlatformError::Internal("数据库正在关闭".to_owned()));
        }
        let (reply_tx, reply_rx) = mpsc::sync_channel::<Result<T>>(1);
        let job: BoxedJob = Box::new(move |conn: &mut Connection| {
            let _ = reply_tx.send(f(conn));
        });
        self.writer
            .sender
            .send(WriteCommand::Run(job))
            .map_err(|_| PlatformError::Internal("数据库写线程不可用".to_owned()))?;
        reply_rx
            .recv()
            .map_err(|_| PlatformError::Internal("数据库写线程未返回结果".to_owned()))?
    }

    /// 在只读连接上执行查询闭包（禁止在闭包内再调用 `read`，否则死锁）。
    ///
    /// # Errors
    ///
    /// 锁中毒或闭包错误时返回错误。
    pub fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let guard = self
            .read
            .lock()
            .map_err(|_| PlatformError::Internal("数据库读连接锁中毒".to_owned()))?;
        f(&guard)
    }

    /// 完整性检查。
    ///
    /// # Errors
    ///
    /// 不一致时返回「数据损坏」。
    pub fn integrity_check(&self) -> Result<()> {
        self.read(schema::integrity_check)
    }

    /// 生成数据库快照到指定文件（`VACUUM INTO`，不阻塞读）。
    ///
    /// # Errors
    ///
    /// 目标目录创建失败或 `VACUUM` 失败时返回错误。
    pub fn snapshot_to(&self, destination: &Path) -> Result<()> {
        if destination.exists() {
            return Err(PlatformError::InvalidArgument(format!(
                "快照目标已存在（拒绝覆盖）：{}",
                destination.display()
            )));
        }
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|source| PlatformError::io(parent.to_path_buf(), source))?;
        }
        let destination = destination.to_path_buf();
        self.write_raw(move |conn| {
            conn.execute(
                "VACUUM INTO ?1",
                [destination.to_string_lossy().to_string()],
            )
            .map_err(PlatformError::from)?;
            Ok(())
        })
    }

    /// WAL checkpoint（TRUNCATE），确保单文件副本一致。
    ///
    /// # Errors
    ///
    /// checkpoint 失败时返回错误。
    pub fn checkpoint(&self) -> Result<()> {
        self.write_raw(|conn| {
            conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
                .map_err(PlatformError::from)
        })
    }

    /// 关闭写线程（幂等）；之后所有写操作返回错误。
    ///
    /// # Errors
    ///
    /// 写线程无法正常退出时返回错误。
    pub fn close(&self) -> Result<()> {
        if self.closing.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let _ = self.writer.sender.send(WriteCommand::Shutdown);
        let handle = self
            .writer
            .join
            .lock()
            .map_err(|_| PlatformError::Internal("写线程句柄锁中毒".to_owned()))?
            .take();
        if let Some(handle) = handle {
            handle
                .join()
                .map_err(|_| PlatformError::Internal("数据库写线程 panic".to_owned()))?;
        }
        Ok(())
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        self.closing.store(true, Ordering::SeqCst);
        let _ = self.writer.sender.send(WriteCommand::Shutdown);
        if let Ok(mut guard) = self.writer.join.lock()
            && let Some(handle) = guard.take()
        {
            let _ = handle.join();
        }
    }
}

/// 配置连接 pragma（WAL/外键/busy_timeout/synchronous）。
fn configure_connection(conn: &Connection) -> Result<()> {
    conn.busy_timeout(Duration::from_millis(BUSY_TIMEOUT_MS))
        .map_err(PlatformError::from)?;
    let journal_mode: String = conn
        .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
        .map_err(PlatformError::from)?;
    if !journal_mode.eq_ignore_ascii_case("wal") {
        return Err(PlatformError::Internal(format!(
            "无法启用 WAL（当前 journal_mode={journal_mode}）"
        )));
    }
    conn.pragma_update(None, "foreign_keys", true)
        .map_err(PlatformError::from)?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(PlatformError::from)?;
    Ok(())
}

/// 写线程主循环：顺序执行命令，`Shutdown` 时退出。
fn writer_loop(conn: &mut Connection, receiver: &mpsc::Receiver<WriteCommand>) {
    while let Ok(command) = receiver.recv() {
        match command {
            WriteCommand::Run(job) => job(conn),
            WriteCommand::Shutdown => break,
        }
    }
}

/// 迁移前快照：`VACUUM INTO <backup_dir>/<时间戳>/exam-grading.sqlite3`。
fn snapshot_database(conn: &Connection, backup_dir: &Path, clock: &dyn Clock) -> Result<PathBuf> {
    let stamp = clock.now().format("%Y%m%dT%H%M%S%.3f").to_string();
    let directory = backup_dir.join(stamp);
    std::fs::create_dir_all(&directory)
        .map_err(|source| PlatformError::io(directory.clone(), source))?;
    let destination = directory.join(crate::paths::DATABASE_FILE);
    conn.execute(
        "VACUUM INTO ?1",
        [destination.to_string_lossy().to_string()],
    )
    .map_err(PlatformError::from)?;
    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;

    fn fixed_clock() -> SharedClock {
        Arc::new(FixedClock::from_rfc3339("2026-11-02T00:00:00Z").unwrap())
    }

    fn open_temp(temp: &tempfile::TempDir) -> Database {
        let options = DatabaseOptions {
            backup_dir: Some(temp.path().join("backups")),
            clock: Some(fixed_clock()),
        };
        Database::open_with(&temp.path().join("exam-grading.sqlite3"), &options).unwrap()
    }

    #[test]
    fn fresh_database_migrates_and_verifies() {
        let temp = tempfile::tempdir().unwrap();
        let db = open_temp(&temp);
        assert_eq!(db.schema_version().unwrap(), schema::SCHEMA_VERSION);
        db.integrity_check().unwrap();
        db.read(schema::verify_tables).unwrap();
        let journal: String = db
            .read(|conn| {
                conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))
                    .map_err(PlatformError::from)
            })
            .unwrap();
        assert_eq!(journal.to_lowercase(), "wal");
        let foreign_keys: i64 = db
            .read(|conn| {
                conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))
                    .map_err(PlatformError::from)
            })
            .unwrap();
        assert_eq!(foreign_keys, 1);
        db.close().unwrap();
    }

    #[test]
    fn transaction_commits_and_rolls_back() {
        let temp = tempfile::tempdir().unwrap();
        let db = open_temp(&temp);
        db.write(|tx| {
            tx.execute(
                "INSERT INTO app_meta(key, value) VALUES ('test_key', 'v1')",
                [],
            )
            .map_err(PlatformError::from)?;
            Ok(())
        })
        .unwrap();
        let value: Option<String> = db
            .read(|conn| {
                conn.query_row(
                    "SELECT value FROM app_meta WHERE key='test_key'",
                    [],
                    |row| row.get(0),
                )
                .map_err(PlatformError::from)
            })
            .ok();
        assert_eq!(value.as_deref(), Some("v1"));

        let failure = db.write::<(), _>(|tx| {
            tx.execute(
                "INSERT INTO app_meta(key, value) VALUES ('rollback_key', 'x')",
                [],
            )
            .map_err(PlatformError::from)?;
            Err(PlatformError::InvalidArgument("故意失败".to_owned()))
        });
        assert!(failure.is_err());
        let exists: bool = db
            .read(|conn| {
                conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM app_meta WHERE key='rollback_key')",
                    [],
                    |row| row.get(0),
                )
                .map_err(PlatformError::from)
            })
            .unwrap();
        assert!(!exists, "失败的闭包必须回滚");
        db.close().unwrap();
    }

    #[test]
    fn foreign_keys_are_enforced() {
        let temp = tempfile::tempdir().unwrap();
        let db = open_temp(&temp);
        let error = db.write(|tx| {
            tx.execute(
                "INSERT INTO student(id, classroom_id, student_no, name, source, created_at) \
                 VALUES ('01AAA', '01MISSING', '001', '张三', 'manual', '2026-11-02T00:00:00Z')",
                [],
            )
            .map_err(PlatformError::from)?;
            Ok(())
        });
        assert!(error.is_err(), "外键必须阻止悬空引用");
        db.close().unwrap();
    }

    #[test]
    fn snapshot_before_migration_is_created_for_existing_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("exam-grading.sqlite3");
        // 空文件模拟「上一版本留下的库」：版本 0 < 目标版本。
        std::fs::write(&path, b"").unwrap();
        let options = DatabaseOptions {
            backup_dir: Some(temp.path().join("backups")),
            clock: Some(fixed_clock()),
        };
        let db = Database::open_with(&path, &options).unwrap();
        assert_eq!(db.schema_version().unwrap(), schema::SCHEMA_VERSION);
        let snapshots: Vec<_> = std::fs::read_dir(temp.path().join("backups"))
            .unwrap()
            .filter_map(std::result::Result::ok)
            .collect();
        assert_eq!(snapshots.len(), 1, "迁移前应生成一份快照");
        db.close().unwrap();
    }

    #[test]
    fn reopen_keeps_schema_version_and_no_remigration() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("exam-grading.sqlite3");
        {
            let db = Database::open(&path).unwrap();
            assert_eq!(db.schema_version().unwrap(), schema::SCHEMA_VERSION);
            db.close().unwrap();
        }
        let db = Database::open(&path).unwrap();
        assert_eq!(db.schema_version().unwrap(), schema::SCHEMA_VERSION);
        db.close().unwrap();
    }

    #[test]
    fn writes_after_close_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let db = open_temp(&temp);
        db.close().unwrap();
        let result = db.write(|_tx| Ok(()));
        assert!(result.is_err());
    }

    #[test]
    fn snapshot_to_vacuum_produces_readable_copy() {
        let temp = tempfile::tempdir().unwrap();
        let db = open_temp(&temp);
        db.write(|tx| {
            tx.execute(
                "INSERT INTO app_meta(key, value) VALUES ('snap', 'yes')",
                [],
            )
            .map_err(PlatformError::from)?;
            Ok(())
        })
        .unwrap();
        let destination = temp.path().join("copy.sqlite3");
        db.snapshot_to(&destination).unwrap();
        assert!(destination.exists());
        let copy = Connection::open(&destination).unwrap();
        schema::integrity_check(&copy).unwrap();
        let value: String = copy
            .query_row("SELECT value FROM app_meta WHERE key='snap'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(value, "yes");
        // 不允许覆盖已存在的快照。
        assert!(db.snapshot_to(&destination).is_err());
        db.close().unwrap();
    }
    /// v1 → v2：迁移前快照必须是可读的 v1 库；迁移后旧数据保留；快照可再次升级（回滚/恢复兼容）。
    #[test]
    fn v1_snapshot_is_readable_and_data_retained() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(crate::paths::DATABASE_FILE);
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(schema::MIGRATIONS[0].sql).unwrap();
            conn.execute(
                "INSERT INTO app_meta(key, value) VALUES ('schema_version', '1')",
                [],
            )
            .unwrap();
            conn.execute_batch(
                "INSERT INTO classroom(id, name, term, source, body, created_at) \
                 VALUES ('c1', '高一(1)班', '2026 秋', 'manual', '{}', \
                         '2026-11-02T12:00:00Z'); \
                 INSERT INTO paper(id, classroom_id, title, subject, total_score, key_version, \
                     rubric_version, body, created_at) \
                 VALUES ('p1', 'c1', '期中数学', '\"math\"', 10000, 1, 1, '{}', \
                         '2026-11-02T12:00:00Z');",
            )
            .unwrap();
        }
        let backup_dir = temp.path().join("backups");
        let options = DatabaseOptions {
            backup_dir: Some(backup_dir.clone()),
            clock: Some(fixed_clock()),
        };
        let db = Database::open_with(&path, &options).unwrap();
        assert_eq!(db.schema_version().unwrap(), schema::SCHEMA_VERSION);
        let name: String = db
            .read(|conn| {
                conn.query_row("SELECT name FROM classroom WHERE id = 'c1'", [], |row| {
                    row.get(0)
                })
                .map_err(PlatformError::from)
            })
            .unwrap();
        assert_eq!(name, "高一(1)班", "迁移必须保留旧数据");
        db.close().unwrap();

        let entries: Vec<_> = std::fs::read_dir(&backup_dir)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .collect();
        assert_eq!(entries.len(), 1, "迁移前应生成一份快照");
        let snapshot = entries[0].path().join(crate::paths::DATABASE_FILE);
        assert!(snapshot.exists(), "快照路径 {snapshot:?}");
        let snapshot_conn = Connection::open(&snapshot).unwrap();
        assert_eq!(
            schema::read_version(&snapshot_conn).unwrap(),
            1,
            "快照必须是迁移前的 v1"
        );
        let has_new_column: bool = snapshot_conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('rubric_draft') \
                 WHERE name = 'question_kind')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!has_new_column, "快照不得包含 v2 迁移结果");
        let name: String = snapshot_conn
            .query_row("SELECT name FROM classroom WHERE id = 'c1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(name, "高一(1)班", "快照必须包含旧数据");
        drop(snapshot_conn);

        // 快照可被当前程序重新迁移（恢复后继续升级）。
        let mut snapshot_conn = Connection::open(&snapshot).unwrap();
        let outcome = schema::apply(&mut snapshot_conn).unwrap();
        assert_eq!(outcome.applied, vec![2, 3, 4]);
        schema::integrity_check(&snapshot_conn).unwrap();
    }
}
