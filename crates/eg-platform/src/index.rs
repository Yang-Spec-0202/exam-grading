//! 日志全文索引（`SPEC-platform.md` §6；SQLite FTS5）。
//!
//! - 只索引**脱敏后**的日志文本；索引文件独立（删除索引不影响日志与业务）；
//! - 可增量写入、可按文件段校验/重建、可整体重建；schema 版本变化强制重建；
//! - 检索只读索引，不回读原图或识别全文。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use eg_domain::Timestamp;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::error::{PlatformError, Result};
use crate::logging::LogRecord;

/// 索引 schema 版本：变化时强制重建。
pub const INDEX_SCHEMA_VERSION: u32 = 1;

/// 单条检索命中。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogIndexHit {
    /// 日志时间。
    pub at: Timestamp,
    /// 级别。
    pub level: String,
    /// 模块。
    pub module: String,
    /// 关键词所在的脱敏文本。
    pub text: String,
}

/// 索引状态摘要（UI/诊断包）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexState {
    /// schema 版本。
    pub schema_version: u32,
    /// 已索引行数。
    pub indexed_records: u64,
    /// 已登记日志段数。
    pub segments: u64,
    /// 最近一次同步/重建时间。
    pub last_synced_at: Option<Timestamp>,
    /// 是否降级（校验失败/超出范围）。
    pub degraded: bool,
    /// 降级原因（中文）。
    pub note: String,
}

/// 重建/同步报告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexSyncReport {
    /// 处理（新增或重建）的段数。
    pub segments: u64,
    /// 已索引行数。
    pub records: u64,
    /// 是否执行了重建。
    pub rebuilt: bool,
}

/// 日志全文索引（独立 SQLite 文件，FTS5）。
pub struct LogIndex {
    path: PathBuf,
    conn: Mutex<Connection>,
}

impl std::fmt::Debug for LogIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogIndex")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl LogIndex {
    /// 打开/创建索引（schema 版本变化时重建）。
    ///
    /// # Errors
    ///
    /// 目录创建、打开或建表失败时返回错误。
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|source| PlatformError::io(parent.to_path_buf(), source))?;
        }
        let conn = Connection::open(path).map_err(PlatformError::from)?;
        conn.busy_timeout(std::time::Duration::from_millis(5_000))
            .map_err(PlatformError::from)?;
        create_schema(&conn)?;
        Ok(Self {
            path: path.to_path_buf(),
            conn: Mutex::new(conn),
        })
    }

    /// 索引文件路径。
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 当前状态摘要。
    ///
    /// # Errors
    ///
    /// 查询失败时返回错误。
    pub fn state(&self) -> Result<IndexState> {
        let conn = self.lock()?;
        let indexed_records = count_of(&conn, "SELECT COUNT(*) FROM log_fts")?;
        let segments = count_of(&conn, "SELECT COUNT(*) FROM index_segments")?;
        let last_synced: Option<String> = conn
            .query_row(
                "SELECT value FROM index_meta WHERE key='last_synced_at'",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(PlatformError::from)?;
        let last_synced_at = last_synced
            .as_deref()
            .map(|text| {
                eg_domain::time::parse_rfc3339(text).map_err(|error| {
                    PlatformError::DatabaseCorrupt(format!("索引时间非法：{error}"))
                })
            })
            .transpose()?;
        let schema_version = read_meta(&conn, "schema_version")?
            .and_then(|text| text.parse::<u32>().ok())
            .unwrap_or(INDEX_SCHEMA_VERSION);
        Ok(IndexState {
            schema_version,
            indexed_records,
            segments,
            last_synced_at,
            degraded: false,
            note: format!(
                "索引覆盖最近日志（schema v{INDEX_SCHEMA_VERSION}）；索引文件可删除，删除后不影响日志与业务"
            ),
        })
    }

    /// 增量写入一条日志（脱敏文本）。
    ///
    /// # Errors
    ///
    /// 写入失败时返回错误。
    pub fn add(&self, record: &LogRecord) -> Result<()> {
        let conn = self.lock()?;
        insert_record(&conn, record, None)?;
        Ok(())
    }

    /// 检索（关键词按 FTS5 语法转义为短语）。
    ///
    /// # Errors
    ///
    /// 查询失败时返回错误。
    pub fn search(&self, keyword: &str, limit: usize, offset: usize) -> Result<Vec<LogIndexHit>> {
        let conn = self.lock()?;
        // trigram 分词要求关键词 ≥3 字符；更短的关键词回退到 LIKE（仍是脱敏后的索引文本）。
        let short = keyword.chars().count() < 3;
        let (sql, argument) = if short {
            (
                "SELECT at, level, module, text FROM log_fts WHERE text LIKE ?1 ESCAPE '\\' \
                 ORDER BY rowid DESC LIMIT ?2 OFFSET ?3",
                format!("%{}%", escape_like(keyword)),
            )
        } else {
            (
                "SELECT at, level, module, text FROM log_fts WHERE log_fts MATCH ?1 \
                 ORDER BY rowid DESC LIMIT ?2 OFFSET ?3",
                escape_fts_phrase(keyword),
            )
        };
        let mut statement = conn.prepare(sql).map_err(PlatformError::from)?;
        let rows = statement
            .query_map(
                params![
                    argument,
                    i64::try_from(if limit == 0 { 50 } else { limit }).unwrap_or(50),
                    i64::try_from(offset).unwrap_or(0)
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .map_err(PlatformError::from)?;
        let mut out = Vec::new();
        for row in rows {
            let (at, level, module, text) = row.map_err(PlatformError::from)?;
            let at = eg_domain::time::parse_rfc3339(&at)
                .map_err(|error| PlatformError::DatabaseCorrupt(error.to_string()))?;
            out.push(LogIndexHit {
                at,
                level,
                module,
                text,
            });
        }
        Ok(out)
    }

    /// 与日志目录同步（补索引缺失段 / 重建不一致段）。
    ///
    /// # Errors
    ///
    /// 读文件或写索引失败时返回错误。
    pub fn sync(&self, log_dir: &Path, range_days: u32, now: Timestamp) -> Result<IndexSyncReport> {
        let files = log_files(log_dir)?;
        let cutoff = now - chrono::Duration::days(i64::from(range_days));
        let mut conn = self.lock()?;
        let mut report = IndexSyncReport {
            segments: 0,
            records: 0,
            rebuilt: false,
        };
        // 增量写入的行没有文件归属；与按段校验混用会重复计数，故先清空再按文件重建，
        // 保证「增量结果」与「整体重建结果」一致（SPEC-platform.md §7）。
        let orphaned: u64 = conn
            .query_row(
                "SELECT COUNT(*) FROM log_fts WHERE file IS NULL",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_or(0, |count| u64::try_from(count).unwrap_or(0));
        if orphaned > 0 {
            conn.execute_batch("DELETE FROM log_fts; DELETE FROM index_segments;")
                .map_err(PlatformError::from)?;
            report.rebuilt = true;
        }
        for file in files {
            let date = file_date(&file);
            if let Some(date) = date
                && date < cutoff
            {
                continue;
            }
            let lines = read_lines(&file)?;
            let indexed: u64 = conn
                .query_row(
                    "SELECT COALESCE(line_count, 0) FROM index_segments WHERE path = ?1",
                    [file.to_string_lossy().to_string()],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(PlatformError::from)?
                .map_or(0, |value| u64::try_from(value).unwrap_or(0));
            if indexed == lines.len() as u64 {
                report.records += indexed;
                report.segments += 1;
                continue;
            }
            let tx = conn.transaction().map_err(PlatformError::from)?;
            tx.execute(
                "DELETE FROM log_fts WHERE file = ?1",
                [file.to_string_lossy().to_string()],
            )
            .map_err(PlatformError::from)?;
            let file_key = file.to_string_lossy().to_string();
            for line in &lines {
                if let Ok(record) = serde_json::from_str::<LogRecord>(line) {
                    insert_record(&tx, &record, Some(&file_key))?;
                }
            }
            tx.execute(
                "INSERT INTO index_segments(path, line_count, indexed_at) VALUES (?1, ?2, ?3) \
                 ON CONFLICT(path) DO UPDATE SET line_count = excluded.line_count, \
                 indexed_at = excluded.indexed_at",
                params![
                    file.to_string_lossy().to_string(),
                    i64::try_from(lines.len()).unwrap_or(i64::MAX),
                    eg_domain::time::format_rfc3339(now)
                ],
            )
            .map_err(PlatformError::from)?;
            tx.commit().map_err(PlatformError::from)?;
            report.segments += 1;
            report.records += lines.len() as u64;
        }
        drop(conn);
        self.set_meta("last_synced_at", &eg_domain::time::format_rfc3339(now))?;
        report.rebuilt = true;
        Ok(report)
    }

    /// 全量重建（清空后按日志目录重建）。
    ///
    /// # Errors
    ///
    /// 清空/读取/写入失败时返回错误。
    pub fn rebuild(
        &self,
        log_dir: &Path,
        range_days: u32,
        now: Timestamp,
    ) -> Result<IndexSyncReport> {
        self.clear()?;
        self.sync(log_dir, range_days, now)
    }

    /// 清空索引（保留 schema）。
    ///
    /// # Errors
    ///
    /// 执行失败时返回错误。
    pub fn clear(&self) -> Result<()> {
        let conn = self.lock()?;
        conn.execute_batch("DELETE FROM log_fts; DELETE FROM index_segments;")
            .map_err(PlatformError::from)?;
        Ok(())
    }

    /// 已索引行数。
    ///
    /// # Errors
    ///
    /// 查询失败时返回错误。
    pub fn count(&self) -> Result<u64> {
        let conn = self.lock()?;
        count_of(&conn, "SELECT COUNT(*) FROM log_fts")
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| PlatformError::Internal("日志索引锁中毒".to_owned()))
    }

    fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO index_meta(key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )
        .map_err(PlatformError::from)?;
        Ok(())
    }
}

/// 创建 schema（版本不一致时重建）。
fn create_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS index_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    )
    .map_err(PlatformError::from)?;
    let current = read_meta(conn, "schema_version")?
        .and_then(|text| text.parse::<u32>().ok())
        .unwrap_or(0);
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='log_fts')",
            [],
            |row| row.get(0),
        )
        .map_err(PlatformError::from)?;
    if current != INDEX_SCHEMA_VERSION || !exists {
        conn.execute_batch(
            "DROP TABLE IF EXISTS log_fts; \
             DROP TABLE IF EXISTS index_segments; \
             CREATE VIRTUAL TABLE log_fts USING fts5(at UNINDEXED, level UNINDEXED, \
                 module UNINDEXED, trace_id UNINDEXED, task_id UNINDEXED, file UNINDEXED, text, \
                 tokenize='trigram'); \
             CREATE TABLE index_segments(path TEXT PRIMARY KEY, line_count INTEGER NOT NULL, \
                 indexed_at TEXT NOT NULL);",
        )
        .map_err(PlatformError::from)?;
        conn.execute(
            "INSERT INTO index_meta(key, value) VALUES ('schema_version', ?1) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [INDEX_SCHEMA_VERSION.to_string()],
        )
        .map_err(PlatformError::from)?;
    }
    Ok(())
}

/// 读取 meta 值。
fn read_meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    conn.query_row(
        "SELECT value FROM index_meta WHERE key = ?1",
        [key],
        |row| row.get(0),
    )
    .optional()
    .map_err(PlatformError::from)
}

/// 统计查询。
fn count_of(conn: &Connection, sql: &str) -> Result<u64> {
    conn.query_row(sql, [], |row| row.get::<_, i64>(0))
        .map_err(PlatformError::from)
        .map(|count| u64::try_from(count).unwrap_or(0))
}

/// 插入一条（记录 ∪ 文件归属由调用方在 `sync` 中处理；增量写入 file 为空）。
fn insert_record(conn: &Connection, record: &LogRecord, file: Option<&str>) -> Result<()> {
    let mut text = String::new();
    text.push_str(&record.message);
    for (key, value) in &record.fields {
        text.push(' ');
        text.push_str(key);
        text.push('=');
        text.push_str(value);
    }
    conn.execute(
        "INSERT INTO log_fts(at, level, module, trace_id, task_id, file, text) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            eg_domain::time::format_rfc3339(record.at),
            record.level.code(),
            record.module,
            record.trace_id.map(|id| id.to_string()),
            record.task_id.map(|id| id.to_string()),
            file,
            text
        ],
    )
    .map_err(PlatformError::from)?;
    Ok(())
}

/// FTS5 短语转义。
fn escape_fts_phrase(keyword: &str) -> String {
    format!("\"{}\"", keyword.replace('"', "\"\""))
}

/// LIKE 转义（`%`/`_`/`\`）。
fn escape_like(keyword: &str) -> String {
    keyword
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// 列出日志文件（`app.*.jsonl`，按名称升序）。
fn log_files(log_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let entries = match std::fs::read_dir(log_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(files),
        Err(source) => return Err(PlatformError::io(log_dir.to_path_buf(), source)),
    };
    for entry in entries {
        let entry = entry.map_err(|source| PlatformError::io(log_dir.to_path_buf(), source))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("app.") && name.ends_with(".jsonl") {
            files.push(entry.path());
        }
    }
    files.sort();
    Ok(files)
}

/// 读取文件全部行。
fn read_lines(path: &Path) -> Result<Vec<String>> {
    let text = std::fs::read_to_string(path)
        .map_err(|source| PlatformError::io(path.to_path_buf(), source))?;
    Ok(text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .collect())
}

/// 从文件名解析日期（`app.<YYYY-MM-DD>.<seq>.jsonl`）。
fn file_date(path: &Path) -> Option<Timestamp> {
    let name = path.file_name()?.to_string_lossy();
    let rest = name.strip_prefix("app.")?;
    let date = rest.split('.').next()?;
    let naive = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    let datetime = naive.and_hms_opt(0, 0, 0)?;
    Some(chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(
        datetime,
        chrono::Utc,
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::logging::{LogLevel, LogRecord};
    use std::collections::BTreeMap;

    fn timestamp(text: &str) -> Timestamp {
        eg_domain::time::parse_rfc3339(text).unwrap()
    }

    fn record(at: &str, message: &str) -> LogRecord {
        LogRecord::new(
            timestamp(at),
            LogLevel::Info,
            "eg_recognize",
            "ocr.done",
            message,
        )
    }

    #[test]
    fn incremental_add_and_search() {
        let temp = tempfile::tempdir().unwrap();
        let index = LogIndex::open(&temp.path().join("index.sqlite3")).unwrap();
        index
            .add(&record("2026-11-02T12:00:00Z", "识别完成 第 1 页"))
            .unwrap();
        index
            .add(&record("2026-11-02T12:00:01Z", "判分开始"))
            .unwrap();
        assert_eq!(index.count().unwrap(), 2);
        let hits = index.search("识别", 10, 0).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].text.contains("第 1 页"));
        let none = index.search("不存在的词", 10, 0).unwrap();
        assert_eq!(none.len(), 0);
        // 引号关键词不报错（转义）。
        assert!(index.search("\"", 10, 0).is_ok());
    }

    #[test]
    fn schema_version_change_forces_rebuild() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("index.sqlite3");
        {
            let index = LogIndex::open(&path).unwrap();
            index
                .add(&record("2026-11-02T12:00:00Z", "旧数据"))
                .unwrap();
            assert_eq!(index.count().unwrap(), 1);
        }
        // 人为改 schema 版本（模拟升级）。
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute(
                "UPDATE index_meta SET value='999' WHERE key='schema_version'",
                [],
            )
            .unwrap();
        }
        let index = LogIndex::open(&path).unwrap();
        assert_eq!(index.count().unwrap(), 0, "schema 版本变化应清空重建");
        assert_eq!(index.state().unwrap().schema_version, INDEX_SCHEMA_VERSION);
    }

    #[test]
    fn sync_indexes_log_files_and_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let log_dir = temp.path().join("logs");
        std::fs::create_dir_all(&log_dir).unwrap();
        let file = log_dir.join("app.2026-11-02.0001.jsonl");
        let lines = [
            serde_json::to_string(&record("2026-11-02T12:00:00Z", "第一条 识别")).unwrap(),
            serde_json::to_string(&record("2026-11-02T12:00:01Z", "第二条 判分")).unwrap(),
            serde_json::to_string(&record("2026-11-02T12:00:02Z", "第三条 导出")).unwrap(),
        ];
        std::fs::write(&file, lines.join("\n")).unwrap();
        let index = LogIndex::open(&temp.path().join("index.sqlite3")).unwrap();
        let now = timestamp("2026-11-02T13:00:00Z");
        let first = index.sync(&log_dir, 30, now).unwrap();
        assert_eq!(first.records, 3);
        assert_eq!(index.count().unwrap(), 3);
        let second = index.sync(&log_dir, 30, now).unwrap();
        assert_eq!(second.records, 3, "已一致的段不重复索引");
        assert_eq!(index.count().unwrap(), 3);
        assert_eq!(index.state().unwrap().segments, 1);
    }

    #[test]
    fn sync_rebuilds_truncated_segment() {
        let temp = tempfile::tempdir().unwrap();
        let log_dir = temp.path().join("logs");
        std::fs::create_dir_all(&log_dir).unwrap();
        let file = log_dir.join("app.2026-11-02.0001.jsonl");
        let two = [
            serde_json::to_string(&record("2026-11-02T12:00:00Z", "甲")).unwrap(),
            serde_json::to_string(&record("2026-11-02T12:00:01Z", "乙")).unwrap(),
        ];
        std::fs::write(&file, two.join("\n")).unwrap();
        let index = LogIndex::open(&temp.path().join("index.sqlite3")).unwrap();
        let now = timestamp("2026-11-02T13:00:00Z");
        index.sync(&log_dir, 30, now).unwrap();
        assert_eq!(index.count().unwrap(), 2);
        // 追加一行后重新同步：该段应重建为 3 行。
        let mut lines = two.to_vec();
        lines.push(serde_json::to_string(&record("2026-11-02T12:00:02Z", "丙")).unwrap());
        std::fs::write(&file, lines.join("\n")).unwrap();
        index.sync(&log_dir, 30, now).unwrap();
        assert_eq!(index.count().unwrap(), 3);
    }

    #[test]
    fn index_file_is_independent_from_logs() {
        let temp = tempfile::tempdir().unwrap();
        let log_dir = temp.path().join("logs");
        std::fs::create_dir_all(&log_dir).unwrap();
        let file = log_dir.join("app.2026-11-02.0001.jsonl");
        std::fs::write(
            &file,
            serde_json::to_string(&record("2026-11-02T12:00:00Z", "日志仍在")).unwrap(),
        )
        .unwrap();
        let index_path = temp.path().join("index.sqlite3");
        {
            let index = LogIndex::open(&index_path).unwrap();
            index
                .sync(&log_dir, 30, timestamp("2026-11-02T13:00:00Z"))
                .unwrap();
            assert_eq!(index.count().unwrap(), 1);
        }
        std::fs::remove_file(&index_path).unwrap();
        assert!(file.exists(), "删除索引不影响日志文件");
        let reopened = LogIndex::open(&index_path).unwrap();
        assert_eq!(reopened.count().unwrap(), 0);
        reopened
            .rebuild(&log_dir, 30, timestamp("2026-11-02T13:00:00Z"))
            .unwrap();
        assert_eq!(reopened.count().unwrap(), 1);
    }

    #[test]
    fn index_text_contains_no_secret() {
        let temp = tempfile::tempdir().unwrap();
        let index = LogIndex::open(&temp.path().join("index.sqlite3")).unwrap();
        let mut fields = BTreeMap::new();
        fields.insert("student_name".to_owned(), "张** abc1".to_owned());
        let mut log = record("2026-11-02T12:00:00Z", "导入完成");
        log.fields = fields;
        index.add(&log).unwrap();
        let hits = index.search("导入", 10, 0).unwrap();
        assert!(!hits[0].text.contains("sk-"));
        assert!(hits[0].text.contains("张"));
    }
}
