//! 诊断包导出（`SPEC-platform.md` §6）。
//!
//! 单个压缩包（zip），内容：脱敏配置副本、指定时间窗日志切片、数据库 schema/迁移清单、
//! 环境信息、任务与错误摘要、资产与索引状态。
//! **不含**原图、识别全文、学生姓名、密钥；导出失败显式报错。

use std::io::Write as _;
use std::path::{Path, PathBuf};

use eg_domain::Timestamp;
use serde::{Deserialize, Serialize};

use crate::clock::SharedClock;
use crate::db::Database;
use crate::error::{PlatformError, Result};
use crate::logging::{LogLevel, LogQuery, LogSink};
use crate::paths::{CONFIG_FILE, DataDirs};
use crate::schema;

/// 诊断包请求。
#[derive(Debug, Clone, Default)]
pub struct DiagnosticRequest {
    /// 日志起始时间（含；None = 最近 30 天由调用方保证）。
    pub from: Option<Timestamp>,
    /// 日志结束时间（含）。
    pub to: Option<Timestamp>,
    /// 是否包含 schema/迁移清单（`diagnostics.include_schema`）。
    pub include_schema: bool,
    /// 输出目录（None = 数据根 `logs/diagnostics/`）。
    pub output_dir: Option<PathBuf>,
    /// 日志条数上限（防止包过大）。
    pub log_limit: usize,
}

/// 导出结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticBundle {
    /// zip 路径。
    pub path: PathBuf,
    /// 包内条目名（升序）。
    pub entries: Vec<String>,
    /// 文件字节数。
    pub bytes: u64,
    /// 创建时间。
    pub created_at: Timestamp,
}

/// 环境信息（不含用户名/机器名）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct EnvironmentInfo {
    os: String,
    arch: String,
    version: String,
    data_root: String,
    /// rustc 版本（编译期）。
    rustc: String,
}

/// 诊断包导出器。
#[derive(Clone)]
pub struct Diagnostics {
    dirs: DataDirs,
    db: std::sync::Arc<Database>,
    log: LogSink,
    clock: SharedClock,
}

impl std::fmt::Debug for Diagnostics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Diagnostics")
            .field("data_root", &self.dirs.root())
            .finish_non_exhaustive()
    }
}

impl Diagnostics {
    /// 构造导出器。
    #[must_use]
    pub fn new(
        dirs: DataDirs,
        db: std::sync::Arc<Database>,
        log: LogSink,
        clock: SharedClock,
    ) -> Self {
        Self {
            dirs,
            db,
            log,
            clock,
        }
    }

    /// 导出诊断包（≤10 秒；不读取原图/识别全文）。
    ///
    /// # Errors
    ///
    /// 目录创建、日志查询、数据库统计或 zip 写入失败时返回错误。
    pub fn export(&self, request: &DiagnosticRequest) -> Result<DiagnosticBundle> {
        let now = self.clock.now();
        let stamp = now.format("%Y%m%dT%H%M%S%.3f").to_string();
        let base = request
            .output_dir
            .clone()
            .unwrap_or_else(|| self.dirs.diagnostics_dir());
        let directory = base.join(stamp);
        std::fs::create_dir_all(&directory)
            .map_err(|source| PlatformError::io(directory.clone(), source))?;
        let path = directory.join("diagnostics.zip");

        let redactor = crate::logging::Redactor::new(true);
        let mut entries = Vec::new();
        let mut builder = BundleWriter::new(&path)?;

        // 1) README（说明内容与脱敏口径）。
        let readme = concat!(
            "exam-grading 诊断包\n\n",
            "内容：脱敏配置副本、日志切片、数据库 schema 与统计、环境信息、任务/授权/资产/索引摘要。\n",
            "不含：原图、识别全文、学生姓名、密钥。\n",
            "日志时间窗与条目上限见请求参数；所有文本均已脱敏。\n"
        );
        builder.add_text("README.txt", readme)?;

        // 2) 脱敏配置副本。
        let config_path = self.dirs.config_path();
        if config_path.exists() {
            let text = std::fs::read_to_string(&config_path)
                .map_err(|source| PlatformError::io(config_path.clone(), source))?;
            builder.add_text("config.redacted.toml", &redactor.redact_text(&text))?;
        } else {
            builder.add_text("config.redacted.toml", &format!("# {CONFIG_FILE} 不存在\n"))?;
        }

        // 3) 日志切片（已脱敏入库；按时间窗/级别过滤）。
        let limit = if request.log_limit == 0 {
            5_000
        } else {
            request.log_limit
        };
        let page = self.log.query(&LogQuery {
            level_min: Some(LogLevel::Info),
            from: request.from,
            to: request.to,
            limit,
            ..LogQuery::default()
        })?;
        let mut logs = String::new();
        for record in &page.records {
            let line = serde_json::to_string(record)
                .map_err(|error| PlatformError::Serialize(error.to_string()))?;
            logs.push_str(&line);
            logs.push('\n');
        }
        builder.add_text("logs.jsonl", &logs)?;

        // 4) schema/迁移清单（不含数据行）。
        if request.include_schema {
            builder.add_text("schema.txt", &schema::diagnostics_text())?;
        }

        // 5) 环境信息。
        let env_info = EnvironmentInfo {
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            data_root: self.dirs.root().display().to_string(),
            rustc: option_env!("RUSTC_VERSION").unwrap_or("unknown").to_owned(),
        };
        builder.add_json("environment.json", &env_info)?;

        // 6) 数据库统计（计数，不含业务数据）。
        let stats = self.database_stats()?;
        builder.add_json("database-stats.json", &stats)?;

        // 7) 任务/授权/资产/索引摘要。
        let task_counts = self.query_counts("SELECT state, COUNT(*) FROM task GROUP BY state")?;
        builder.add_json("tasks.json", &task_counts)?;
        let consent_counts =
            self.query_counts("SELECT state, COUNT(*) FROM task_consent GROUP BY state")?;
        builder.add_json("consent.json", &consent_counts)?;
        let asset_summary = self.query_row_json("asset-summary.json")?;
        builder.add_text("asset-summary.txt", &asset_summary)?;
        let index_state = self.log.index_state()?;
        builder.add_json("log-index.json", &index_state)?;

        // 8) 最近错误摘要（仅计数与错误码，不含消息正文中的 PII 已脱敏）。
        let errors = self.log.query(&LogQuery {
            level_min: Some(LogLevel::Error),
            limit: 100,
            ..LogQuery::default()
        })?;
        let error_summary: Vec<&str> = errors
            .records
            .iter()
            .map(|record| record.event_code.as_str())
            .collect();
        builder.add_json("recent-errors.json", &error_summary)?;

        entries.extend(builder.entries());
        builder.finish()?;
        let bytes = std::fs::metadata(&path).map_or(0, |meta| meta.len());
        Ok(DiagnosticBundle {
            path,
            entries,
            bytes,
            created_at: now,
        })
    }

    fn database_stats(&self) -> Result<serde_json::Value> {
        self.db.read(|conn| {
            let mut map = serde_json::Map::new();
            for table in schema::TABLES {
                if table == &"app_meta" {
                    continue;
                }
                let count: i64 = conn
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })
                    .map_err(PlatformError::from)?;
                map.insert(
                    (*table).to_owned(),
                    serde_json::Value::from(u64::try_from(count).unwrap_or(0)),
                );
            }
            map.insert(
                "schema_version".to_owned(),
                serde_json::Value::from(schema::read_version(conn)?),
            );
            Ok(serde_json::Value::Object(map))
        })
    }

    fn query_counts(&self, sql: &str) -> Result<serde_json::Value> {
        self.db.read(|conn| {
            let mut statement = conn.prepare(sql).map_err(PlatformError::from)?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })
                .map_err(PlatformError::from)?;
            let mut map = serde_json::Map::new();
            for row in rows {
                let (key, count) = row.map_err(PlatformError::from)?;
                map.insert(
                    key,
                    serde_json::Value::from(u64::try_from(count).unwrap_or(0)),
                );
            }
            Ok(serde_json::Value::Object(map))
        })
    }

    fn query_row_json(&self, _name: &str) -> Result<String> {
        self.db.read(|conn| {
            let (count, bytes): (i64, i64) = conn
                .query_row(
                    "SELECT COUNT(*), COALESCE(SUM(byte_len), 0) FROM asset",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(PlatformError::from)?;
            Ok(format!(
                "资产：{count} 个文件，共 {bytes} 字节（仅统计，不含内容）"
            ))
        })
    }
}

/// 最小 zip 写入封装（记录条目，统一错误类型）。
struct BundleWriter {
    writer: zip::ZipWriter<std::fs::File>,
    entries: Vec<String>,
}

impl BundleWriter {
    fn new(path: &Path) -> Result<Self> {
        let file = std::fs::File::create(path)
            .map_err(|source| PlatformError::io(path.to_path_buf(), source))?;
        Ok(Self {
            writer: zip::ZipWriter::new(file),
            entries: Vec::new(),
        })
    }

    fn add_text(&mut self, name: &str, text: &str) -> Result<()> {
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        self.writer
            .start_file(name, options)
            .map_err(|error| PlatformError::Internal(format!("zip 写入失败：{error}")))?;
        self.writer
            .write_all(text.as_bytes())
            .map_err(|source| PlatformError::Io {
                path: PathBuf::from(name),
                source,
            })?;
        self.entries.push(name.to_owned());
        Ok(())
    }

    fn add_json<T: Serialize>(&mut self, name: &str, value: &T) -> Result<()> {
        let text = serde_json::to_string_pretty(value)
            .map_err(|error| PlatformError::Serialize(error.to_string()))?;
        self.add_text(name, &text)
    }

    fn entries(&self) -> Vec<String> {
        self.entries.clone()
    }

    fn finish(self) -> Result<()> {
        self.writer
            .finish()
            .map(|_file| ())
            .map_err(|error| PlatformError::Internal(format!("zip 收尾失败：{error}")))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::clock::FixedClock;
    use crate::logging::{LogRecord, LogSettings};
    use std::sync::Arc;

    fn timestamp(text: &str) -> Timestamp {
        eg_domain::time::parse_rfc3339(text).unwrap()
    }

    fn setup() -> (tempfile::TempDir, Diagnostics, Arc<FixedClock>) {
        let temp = tempfile::tempdir().unwrap();
        let dirs = DataDirs::with_root(temp.path().join("data")).unwrap();
        dirs.ensure().unwrap();
        std::fs::write(
            dirs.config_path(),
            "[meta]\nschema_version = 1\n\n[log]\nlevel = \"info\"\n",
        )
        .unwrap();
        let db = Arc::new(Database::open(&dirs.database_path()).unwrap());
        let clock = Arc::new(FixedClock::new(timestamp("2026-11-02T12:00:00Z")));
        let index =
            Arc::new(crate::index::LogIndex::open(&dirs.log_dir().join("index.sqlite3")).unwrap());
        let sink = LogSink::new(
            dirs.log_dir(),
            LogSettings::default(),
            clock.clone(),
            Some(index),
        )
        .unwrap();
        let diagnostics = Diagnostics::new(dirs, db, sink, clock.clone());
        (temp, diagnostics, clock)
    }

    fn entries_of(path: &Path) -> Vec<String> {
        let file = std::fs::File::open(path).unwrap();
        let mut zip = zip::ZipArchive::new(file).unwrap();
        (0..zip.len())
            .map(|index| zip.by_index(index).unwrap().name().unwrap().to_string())
            .collect()
    }

    fn read_entry(path: &Path, name: &str) -> String {
        use std::io::Read as _;
        let file = std::fs::File::open(path).unwrap();
        let mut zip = zip::ZipArchive::new(file).unwrap();
        let mut entry = zip.by_name(name).unwrap();
        let mut text = String::new();
        entry.read_to_string(&mut text).unwrap();
        text
    }

    #[test]
    fn export_creates_zip_with_expected_entries() {
        let (_temp, diagnostics, _clock) = setup();
        diagnostics
            .log
            .write(
                LogRecord::new(
                    timestamp("2026-11-02T11:00:00Z"),
                    LogLevel::Info,
                    "eg_recognize",
                    "ocr.done",
                    "识别完成，学号 2026001234",
                )
                .field("student_name", "张三")
                .field("student_no", "2026001234"),
            )
            .unwrap();
        diagnostics.log.flush().unwrap();
        let bundle = diagnostics
            .export(&DiagnosticRequest {
                include_schema: true,
                ..DiagnosticRequest::default()
            })
            .unwrap();
        assert!(bundle.path.exists());
        assert!(bundle.bytes > 0);
        let names = entries_of(&bundle.path);
        for expected in [
            "README.txt",
            "config.redacted.toml",
            "logs.jsonl",
            "schema.txt",
            "environment.json",
            "database-stats.json",
            "tasks.json",
            "consent.json",
            "log-index.json",
        ] {
            assert!(
                names.contains(&expected.to_owned()),
                "缺少 {expected}：{names:?}"
            );
        }
        assert!(
            !names
                .iter()
                .any(|name| name.contains("image") || name.contains(".jpg"))
        );
        // 日志切片已脱敏。
        let logs = read_entry(&bundle.path, "logs.jsonl");
        assert!(!logs.contains("张三"));
        assert!(!logs.contains("2026001234"));
        assert!(logs.contains("****1234"));
        // schema 含迁移清单。
        let schema_text = read_entry(&bundle.path, "schema.txt");
        assert!(schema_text.contains("0001_initial"));
        // 环境信息不含密钥。
        let env = read_entry(&bundle.path, "environment.json");
        assert!(!env.contains("sk-"));
        assert!(env.contains("exam-grading") || env.contains("data_root"));
    }

    #[test]
    fn export_without_schema_respects_flag() {
        let (_temp, diagnostics, _clock) = setup();
        let bundle = diagnostics
            .export(&DiagnosticRequest {
                include_schema: false,
                ..DiagnosticRequest::default()
            })
            .unwrap();
        let names = entries_of(&bundle.path);
        assert!(!names.contains(&"schema.txt".to_owned()));
    }

    #[test]
    fn log_limit_is_respected() {
        let (_temp, diagnostics, _clock) = setup();
        for index in 0..10 {
            diagnostics
                .log
                .write(LogRecord::new(
                    timestamp("2026-11-02T11:00:00Z"),
                    LogLevel::Info,
                    "m",
                    "e",
                    format!("第 {index} 条"),
                ))
                .unwrap();
        }
        diagnostics.log.flush().unwrap();
        let bundle = diagnostics
            .export(&DiagnosticRequest {
                log_limit: 3,
                include_schema: false,
                ..DiagnosticRequest::default()
            })
            .unwrap();
        let logs = read_entry(&bundle.path, "logs.jsonl");
        assert_eq!(logs.lines().count(), 3, "应只导出上限内的日志");
    }

    #[test]
    fn default_output_directory_is_under_logs_diagnostics() {
        let (_temp, diagnostics, _clock) = setup();
        let bundle = diagnostics.export(&DiagnosticRequest::default()).unwrap();
        assert!(bundle.path.starts_with(diagnostics.dirs.diagnostics_dir()));
    }
}
