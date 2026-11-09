//! SQLite schema 与只追加迁移（`SPEC-platform.md` §4.1）。
//!
//! - 迁移按版本号顺序执行，每个迁移一个事务；`app_meta.schema_version` 记录已应用版本。
//! - **只追加**：禁止 DROP/RENAME 已发布列；新增列必须带默认值。
//! - 迁移前快照由 [`crate::db::Database`] 在打开时生成（见 `backup` 模块）。

use rusqlite::Connection;
use rusqlite::OptionalExtension;

use crate::error::{PlatformError, Result};

/// 当前 schema 目标版本。
///
/// - v1：首批 23 表（T12）。
/// - v2（task-37）：规则草稿负载列、生效规则按题索引、授权额度用尽、审计扩展字段。
/// - v3（task-42）：复核队列索引/撤销栈/导出记录/提交级排除/判分幂等与断点/复核原因/
///   审计命令字段。
/// - v4（task-66）：复核命令重做栈 `review_redo`（撤销后可重做；新命令清空）。
pub const SCHEMA_VERSION: u32 = 4;

/// 版本号在 `app_meta` 中的键。
pub const SCHEMA_VERSION_KEY: &str = "schema_version";

/// 一个迁移步骤。
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    /// 目标版本（从 1 递增）。
    pub version: u32,
    /// 文件名（不含目录），用于诊断包与快照测试。
    pub name: &'static str,
    /// 迁移 SQL（嵌入二进制）。
    pub sql: &'static str,
}

/// 全部迁移（只追加：新迁移 push 到末尾，不改历史）。
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "0001_initial",
        sql: include_str!("../migrations/0001_initial.sql"),
    },
    Migration {
        version: 2,
        name: "0002_rubric_payload_consent_audit",
        sql: include_str!("../migrations/0002_rubric_payload_consent_audit.sql"),
    },
    Migration {
        version: 3,
        name: "0003_review_undo_export_progress",
        sql: include_str!("../migrations/0003_review_undo_export_progress.sql"),
    },
    Migration {
        version: 4,
        name: "0004_review_redo",
        sql: include_str!("../migrations/0004_review_redo.sql"),
    },
];

/// 首批表清单（启动校验、诊断包与测试使用）。
pub const TABLES: &[&str] = &[
    "app_meta",
    "classroom",
    "student",
    "paper",
    "question",
    "paper_key_revision",
    "rubric_revision",
    "rule_text",
    "rubric_draft",
    "submission",
    "page",
    "segment",
    "recognition",
    "grade_record",
    "comparator_trace",
    "model_call",
    "review_action",
    "asset",
    "asset_link",
    "task",
    "task_attempt",
    "task_consent",
    "audit_log",
    "review_undo",
    "review_redo",
    "export_record",
    "submission_exclusion",
    "grade_idempotency",
    "grade_progress",
    "grade_review_reason",
];

/// 迁移结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationOutcome {
    /// 迁移前版本（0 = 空库）。
    pub from_version: u32,
    /// 迁移后版本。
    pub to_version: u32,
    /// 本次新应用的版本号（升序）。
    pub applied: Vec<u32>,
}

impl MigrationOutcome {
    /// 是否实际执行了迁移。
    #[must_use]
    pub fn migrated(&self) -> bool {
        !self.applied.is_empty()
    }
}

/// 读取数据库当前 schema 版本（`app_meta` 不存在视为 0）。
///
/// # Errors
///
/// 查询失败或版本值不是整数时返回错误（不静默当作 0，避免误迁移）。
pub fn read_version(conn: &Connection) -> Result<u32> {
    let has_meta: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='app_meta')",
            [],
            |row| row.get(0),
        )
        .map_err(PlatformError::from)?;
    if !has_meta {
        return Ok(0);
    }
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM app_meta WHERE key = ?1",
            [SCHEMA_VERSION_KEY],
            |row| row.get(0),
        )
        .optional()
        .map_err(PlatformError::from)?;
    match value {
        None => Ok(0),
        Some(text) => text.parse::<u32>().map_err(|error| {
            PlatformError::DatabaseCorrupt(format!(
                "app_meta.schema_version 不是合法整数：{text}（{error}）"
            ))
        }),
    }
}

/// 应用全部未执行的迁移（每个迁移独立事务）。
///
/// # Errors
///
/// 任一迁移失败即回滚该迁移并返回错误（不继续后续迁移）。
pub fn apply(conn: &mut Connection) -> Result<MigrationOutcome> {
    let from_version = read_version(conn)?;
    let mut applied = Vec::new();
    for migration in MIGRATIONS.iter().filter(|m| m.version > from_version) {
        let transaction = conn.transaction().map_err(PlatformError::from)?;
        transaction.execute_batch(migration.sql).map_err(|error| {
            PlatformError::DatabaseCorrupt(format!(
                "迁移 {}（v{}）失败：{error}",
                migration.name, migration.version
            ))
        })?;
        transaction
            .execute(
                "INSERT INTO app_meta(key, value) VALUES (?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [SCHEMA_VERSION_KEY, &migration.version.to_string()],
            )
            .map_err(PlatformError::from)?;
        transaction.commit().map_err(PlatformError::from)?;
        applied.push(migration.version);
    }
    Ok(MigrationOutcome {
        from_version,
        to_version: read_version(conn)?,
        applied,
    })
}

/// 执行 `PRAGMA integrity_check`，失败即「数据损坏」。
///
/// # Errors
///
/// 数据库不一致或查询失败时返回错误。
pub fn integrity_check(conn: &Connection) -> Result<()> {
    let result: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(PlatformError::from)?;
    if result == "ok" {
        Ok(())
    } else {
        Err(PlatformError::DatabaseCorrupt(format!(
            "PRAGMA integrity_check 失败：{result}"
        )))
    }
}

/// 校验首批表是否齐全（启动期快速自检）。
///
/// # Errors
///
/// 缺表时返回「数据损坏」（迁移未完成或库被破坏）。
pub fn verify_tables(conn: &Connection) -> Result<()> {
    let mut missing = Vec::new();
    for table in TABLES {
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                [table],
                |row| row.get(0),
            )
            .map_err(PlatformError::from)?;
        if !exists {
            missing.push(*table);
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(PlatformError::DatabaseCorrupt(format!(
            "数据库缺少表：{}（迁移不完整或库损坏）",
            missing.join("、")
        )))
    }
}

/// 供诊断包使用的 schema/迁移清单文本（不含数据行）。
#[must_use]
pub fn diagnostics_text() -> String {
    use std::fmt::Write as _;
    let mut text = String::new();
    let _ = writeln!(text, "schema_version = {SCHEMA_VERSION}");
    text.push_str("\n[迁移]\n");
    for migration in MIGRATIONS {
        let _ = writeln!(text, "v{} {}", migration.version, migration.name);
    }
    text.push_str("\n[表]\n");
    for table in TABLES {
        text.push_str(table);
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_are_ordered_and_unique() {
        let mut last = 0;
        for migration in MIGRATIONS {
            assert!(migration.version > last, "迁移版本必须递增");
            last = migration.version;
            assert_ne!(migration.sql.trim(), "");
            assert!(migration.name.starts_with('0'));
        }
        assert_eq!(MIGRATIONS.last().map(|m| m.version), Some(SCHEMA_VERSION));
    }

    #[test]
    fn empty_database_migrates_to_latest() {
        let mut conn = Connection::open_in_memory().unwrap();
        assert_eq!(read_version(&conn).unwrap(), 0);
        let outcome = apply(&mut conn).unwrap();
        assert_eq!(outcome.from_version, 0);
        assert_eq!(outcome.to_version, SCHEMA_VERSION);
        assert!(outcome.migrated());
        verify_tables(&conn).unwrap();
        integrity_check(&conn).unwrap();
        // 幂等：再次执行不重复应用。
        let again = apply(&mut conn).unwrap();
        assert_eq!(again.applied.len(), 0);
        assert_eq!(again.to_version, SCHEMA_VERSION);
    }

    #[test]
    fn diagnostics_text_has_no_data_rows() {
        let text = diagnostics_text();
        assert!(text.contains("schema_version"));
        assert!(text.contains("0001_initial"));
        assert!(text.contains("0002_rubric_payload_consent_audit"));
        assert!(text.contains("0003_review_undo_export_progress"));
        assert!(text.contains("audit_log"));
        assert!(!text.contains("INSERT INTO"));
    }
    /// 用 v1 SQL 建库并写入各表旧行（模拟已发布旧库）。
    fn seed_v1_database(conn: &Connection) {
        let v1 = &MIGRATIONS[0];
        conn.execute_batch(v1.sql).unwrap();
        conn.execute(
            "INSERT INTO app_meta(key, value) VALUES ('schema_version', '1')",
            [],
        )
        .unwrap();
        conn.execute_batch(
            "INSERT INTO classroom(id, name, term, source, body, created_at)                  VALUES ('c1', '高一(1)班', '2026 秋', 'manual', '{}', '2026-11-02T12:00:00Z'); \
             INSERT INTO paper(id, classroom_id, title, subject, total_score, key_version, \
                 rubric_version, body, created_at) \
                 VALUES ('p1', 'c1', '期中数学', '\"math\"', 10000, 1, 1, '{}', \
                         '2026-11-02T12:00:00Z'); \
             INSERT INTO question(id, paper_id, ordinal, label, kind, max_score, body) \
                 VALUES ('q1', 'p1', 1, '1', '\"calculation\"', 10000, '{}'); \
             INSERT INTO rubric_draft(id, question_id, status, origin, model_call_id, \
                 validation, body, created_at, confirmed_at) \
                 VALUES ('r1', 'q1', '\"draft\"', '\"handwritten_transliteration\"', NULL, '[]', \
                         '{}', '2026-11-02T12:00:00Z', NULL); \
             INSERT INTO rubric_revision(paper_id, rubric_version, body, created_at) \
                 VALUES ('p1', 1, '{}', '2026-11-02T12:00:00Z'); \
             INSERT INTO task(id, kind, state, stage, priority, idempotency_key, payload, \
                 attempts, max_attempts, next_run_at, lease_owner, lease_expires_at, \
                 last_error_class, last_error, created_at, updated_at) \
                 VALUES ('t1', 'submission_grade', 'pending', NULL, 0, 'k1', '{}', 0, 3, \
                         '2026-11-02T12:00:00Z', NULL, NULL, NULL, NULL, \
                         '2026-11-02T12:00:00Z', '2026-11-02T12:00:00Z'); \
             INSERT INTO task_consent(consent_id, task_id, backend, scope_hash, budget_micros, \
                 state, granted_at, ended_at) \
                 VALUES ('k1', 't1', 'openai_compat', \
                         '0000000000000000000000000000000000000000000000000000000000000000', \
                         1000, 'active', '2026-11-02T12:00:00Z', NULL); \
             INSERT INTO audit_log(id, at, actor, action, entity_type, entity_id, before_hash, \
                 after_hash, prev_hash, chain_hash, note, consent_id, task_id) \
                 VALUES ('01ARZ3NDEKTSV4RRFFQ69G5FAV', '2026-11-02T12:00:00Z', 'local-teacher', \
                         'submission.import', 'submission', 's1', NULL, NULL, \
                         '0000000000000000000000000000000000000000000000000000000000000000', \
                         '0000000000000000000000000000000000000000000000000000000000000000', \
                         '旧审计行', NULL, NULL);",
        )
        .unwrap();
    }

    fn column_names(conn: &Connection, table: &str) -> Vec<String> {
        let mut statement = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap();
        statement
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .map(|row| row.unwrap())
            .collect()
    }

    #[test]
    fn v1_to_v2_migration_preserves_rows_and_fills_defaults() {
        let mut conn = Connection::open_in_memory().unwrap();
        seed_v1_database(&conn);
        assert_eq!(read_version(&conn).unwrap(), 1);

        let outcome = apply(&mut conn).unwrap();
        assert_eq!(outcome.from_version, 1);
        assert_eq!(outcome.to_version, 4);
        assert_eq!(outcome.applied, vec![2, 3, 4]);
        verify_tables(&conn).unwrap();
        integrity_check(&conn).unwrap();

        // 旧行保留且新列取默认值（不猜造、不丢历史）。
        let (kind, max_score, mode, template): (String, i64, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT question_kind, max_score_milli, scoring_mode, template_ref \
                 FROM rubric_draft WHERE id = 'r1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(kind, "");
        assert_eq!(max_score, 0);
        assert_eq!(mode, None);
        assert_eq!(template, None);
        let draft_body: String = conn
            .query_row("SELECT body FROM rubric_draft WHERE id = 'r1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(draft_body, "{}", "旧 body 不得被改写");

        let (spent, exhausted, state): (i64, Option<String>, String) = conn
            .query_row(
                "SELECT spent_micros, exhausted_at, state FROM task_consent \
                 WHERE consent_id = 'k1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(spent, 0);
        assert_eq!(exhausted, None);
        assert_eq!(state, "active");

        let (rubric_version, template_id, prompt_hash, model_call): (
            Option<i64>,
            Option<String>,
            Option<String>,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT rubric_version, template_id, prompt_hash, model_call_id FROM audit_log \
                 WHERE entity_id = 's1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(rubric_version, None);
        assert_eq!(template_id, None);
        assert_eq!(prompt_hash, None);
        assert_eq!(model_call, None);
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM audit_log", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );

        // v2 新列存在；rubric_revision.question_id 可为空，且填写时外键生效。
        let draft_columns = column_names(&conn, "rubric_draft");
        for column in [
            "question_kind",
            "max_score_milli",
            "scoring_mode",
            "partial_choice",
            "template_id",
            "template_version",
            "template_ref",
        ] {
            assert!(
                draft_columns.contains(&column.to_owned()),
                "缺少列 {column}"
            );
        }
        assert!(column_names(&conn, "rubric_revision").contains(&"question_id".to_owned()));
        conn.execute(
            "UPDATE rubric_revision SET question_id = 'q1' WHERE paper_id = 'p1'",
            [],
        )
        .unwrap();
        let bad = conn.execute(
            "UPDATE rubric_revision SET question_id = 'missing' WHERE paper_id = 'p1'",
            [],
        );
        assert!(bad.is_err(), "question_id 外键必须生效");
    }

    #[test]
    fn failed_migration_rolls_back_every_statement() {
        let mut conn = Connection::open_in_memory().unwrap();
        seed_v1_database(&conn);
        // 预置同名索引，让 v2 的 CREATE INDEX 失败（此前的 ALTER 必须一并回滚）。
        conn.execute(
            "CREATE INDEX idx_rubric_draft_question_created ON rubric_draft(id)",
            [],
        )
        .unwrap();
        let error = apply(&mut conn).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("0002_rubric_payload_consent_audit"),
            "{error}"
        );
        assert_eq!(read_version(&conn).unwrap(), 1, "失败迁移不得推进版本");
        assert!(
            !column_names(&conn, "rubric_draft").contains(&"question_kind".to_owned()),
            "失败迁移的 ALTER 必须回滚"
        );
        assert!(
            !column_names(&conn, "task_consent").contains(&"spent_micros".to_owned()),
            "失败迁移不得留下部分列"
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM rubric_draft", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
            1,
            "失败迁移不得丢旧行"
        );

        // 移除冲突后重试可正常升级（可恢复，不需要重建库）。
        conn.execute("DROP INDEX idx_rubric_draft_question_created", [])
            .unwrap();
        let outcome = apply(&mut conn).unwrap();
        assert_eq!(outcome.applied, vec![2, 3, 4]);
        assert_eq!(read_version(&conn).unwrap(), 4);
        integrity_check(&conn).unwrap();
    }

    /// 用 v1+v2 SQL 建库并写入各类旧行（模拟已发布的 v2 库）。
    fn seed_v2_database(conn: &Connection) {
        conn.execute_batch(MIGRATIONS[0].sql).unwrap();
        conn.execute_batch(MIGRATIONS[1].sql).unwrap();
        conn.execute(
            "INSERT INTO app_meta(key, value) VALUES ('schema_version', '2')",
            [],
        )
        .unwrap();
        conn.execute_batch(
            "INSERT INTO classroom(id, name, term, source, body, created_at) \
                 VALUES ('c1', '高一(1)班', '2026 秋', 'manual', '{}', '2026-11-02T12:00:00Z'); \
             INSERT INTO student(id, classroom_id, student_no, name, source, body, created_at) \
                 VALUES ('s1', 'c1', '2026001', '张三', '\"manual_add\"', '{}', \
                         '2026-11-02T12:00:00Z'); \
             INSERT INTO paper(id, classroom_id, title, subject, total_score, key_version, \
                 rubric_version, body, created_at) \
                 VALUES ('p1', 'c1', '期中数学', '\"math\"', 10000, 1, 1, '{}', \
                         '2026-11-02T12:00:00Z'); \
             INSERT INTO question(id, paper_id, ordinal, label, kind, max_score, body) \
                 VALUES ('q1', 'p1', 1, '1', '\"calculation\"', 10000, '{}'); \
             INSERT INTO asset(id, byte_len, media_type, created_at) \
                 VALUES ('a1', 10, 'image/jpeg', '2026-11-02T12:00:00Z'); \
             INSERT INTO submission(id, paper_id, classroom_id, student_id, source_file_name, \
                 source_stem, status, body, created_at) \
                 VALUES ('sub1', 'p1', 'c1', 's1', '张三.jpg', '张三', '\"imported\"', '{}', \
                         '2026-11-02T12:00:00Z'); \
             INSERT INTO page(id, submission_id, page_no, asset, width, height, \
                 exif_orientation, quality, body, created_at) \
                 VALUES ('pg1', 'sub1', 1, 'a1', 1200, 1600, 1, '\"acceptable\"', '{}', \
                         '2026-11-02T12:00:00Z'); \
             INSERT INTO grade_record(id, submission_id, question_id, revision, mode, sampled, \
                 route, status, confidence, awarded, body, created_at) \
                 VALUES ('g1', 'sub1', 'q1', 1, '\"standard\"', 0, '\"rule_only\"', \
                         '\"needs_review\"', 0.5, 5000, '{}', '2026-11-02T12:00:00Z'); \
             INSERT INTO audit_log(id, at, actor, action, entity_type, entity_id, before_hash, \
                 after_hash, prev_hash, chain_hash, note, consent_id, task_id, rubric_version, \
                 template_id, template_version, template_ref, prompt_hash, model_call_id) \
                 VALUES ('01ARZ3NDEKTSV4RRFFQ69G5FAV', '2026-11-02T12:00:00Z', 'local-teacher', \
                         'review.accept', 'grade_record', 'g1', NULL, NULL, \
                         '0000000000000000000000000000000000000000000000000000000000000000', \
                         '0000000000000000000000000000000000000000000000000000000000000000', \
                         '旧审计行', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL);",
        )
        .unwrap();
    }

    #[test]
    #[allow(clippy::type_complexity)]
    fn v2_to_v3_migration_preserves_rows_and_creates_v3_objects() {
        let mut conn = Connection::open_in_memory().unwrap();
        seed_v2_database(&conn);
        assert_eq!(read_version(&conn).unwrap(), 2);

        let outcome = apply(&mut conn).unwrap();
        assert_eq!(outcome.from_version, 2);
        assert_eq!(outcome.to_version, 4);
        assert_eq!(outcome.applied, vec![3, 4]);
        verify_tables(&conn).unwrap();
        integrity_check(&conn).unwrap();

        // 旧行保留、旧 body 不改写。
        let (submission_status, grade_route): (String, String) = conn
            .query_row(
                "SELECT s.status, g.route FROM submission s \
                 JOIN grade_record g ON g.submission_id = s.id WHERE s.id = 'sub1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(submission_status, "\"imported\"");
        assert_eq!(grade_route, "\"rule_only\"");
        let grade_body: String = conn
            .query_row("SELECT body FROM grade_record WHERE id = 'g1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(grade_body, "{}", "旧 body 不得被改写");

        // 审计旧行：新列取 NULL，旧链哈希列不改写。
        let (command, submission, question, confirmed, verdict): (
            Option<String>,
            Option<String>,
            Option<String>,
            Option<i64>,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT command_id, submission_id, question_id, confirmed, sampling_verdict \
                 FROM audit_log WHERE entity_id = 'g1'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(command, None);
        assert_eq!(submission, None);
        assert_eq!(question, None);
        assert_eq!(confirmed, None);
        assert_eq!(verdict, None);
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM audit_log", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );

        // v3 新列存在。
        for column in [
            "command_id",
            "submission_id",
            "question_id",
            "reason_code",
            "scope",
            "confirmed",
            "sampling_verdict",
        ] {
            assert!(
                column_names(&conn, "audit_log").contains(&column.to_owned()),
                "缺少审计列 {column}"
            );
        }

        // v3 表存在且可用（含外键/唯一键）。
        conn.execute(
            "INSERT INTO review_undo(id, command_id, action, entity_type, entity_id, payload, \
                 created_at) VALUES ('u1', 'cmd-1', 'review.accept', 'grade_record', 'g1', '{}', \
                 '2026-11-02T12:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO export_record(id, kind, format, scope, summary_hash, row_count, body, \
                 created_at) VALUES ('e1', 'paper', 'xlsx', '{}', \
                 '0000000000000000000000000000000000000000000000000000000000000000', 3, '{}', \
                 '2026-11-02T12:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO submission_exclusion(submission_id, reason, note, at, body) \
                 VALUES ('sub1', '\"absent\"', '', '2026-11-02T12:00:00Z', '{}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO grade_idempotency(key, submission_id, question_id, mode, grade_id, \
                 result_summary, created_at) VALUES ('idem-1', 'sub1', 'q1', '\"standard\"', \
                 'g1', '{}', '2026-11-02T12:00:00Z')",
            [],
        )
        .unwrap();
        assert!(
            conn.execute(
                "INSERT INTO grade_idempotency(key, submission_id, question_id, mode, grade_id, \
                     result_summary, created_at) VALUES ('idem-1', 'sub1', 'q1', \
                     '\"standard\"', 'g1', '{}', '2026-11-02T12:00:00Z')",
                [],
            )
            .is_err(),
            "幂等键必须唯一"
        );
        conn.execute(
            "INSERT INTO grade_progress(submission_id, question_scope, state, attempts, \
                 updated_at) VALUES ('sub1', '', '\"running\"', 1, '2026-11-02T12:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO grade_progress(submission_id, question_scope, state, attempts, \
                 updated_at) VALUES ('sub1', 'q1', '\"done\"', 1, '2026-11-02T12:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO grade_review_reason(grade_id, reason_code) VALUES ('g1', 'low_confidence')",
            [],
        )
        .unwrap();
        let bad = conn.execute(
            "INSERT INTO grade_review_reason(grade_id, reason_code) VALUES ('missing', 'conflict')",
            [],
        );
        assert!(bad.is_err(), "grade_review_reason.grade_id 外键必须生效");

        // v3 查询索引存在。
        let index_exists = |name: &str| -> bool {
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='index' AND name=?1)",
                [name],
                |row| row.get(0),
            )
            .unwrap()
        };
        for name in [
            "idx_grade_mode_status_sampled",
            "idx_grade_status_question",
            "idx_grade_created_id",
            "idx_submission_student_status",
            "idx_export_summary",
            "idx_audit_command",
            "idx_audit_submission_question",
        ] {
            assert!(index_exists(name), "缺少索引 {name}");
        }
    }

    #[test]
    fn failed_v3_migration_rolls_back_alter_and_index_statements() {
        let mut conn = Connection::open_in_memory().unwrap();
        seed_v2_database(&conn);
        // 预置同名索引，让 v3 末尾的 CREATE INDEX 失败（此前的 ALTER 必须一并回滚）。
        conn.execute("CREATE INDEX idx_audit_command ON audit_log(entity_id)", [])
            .unwrap();
        let error = apply(&mut conn).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("0003_review_undo_export_progress"),
            "{error}"
        );
        assert_eq!(read_version(&conn).unwrap(), 2, "失败迁移不得推进版本");
        assert!(
            !column_names(&conn, "audit_log").contains(&"command_id".to_owned()),
            "失败迁移的 ALTER 必须回滚"
        );
        let review_undo_exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='review_undo')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!review_undo_exists, "失败迁移的建表必须回滚");

        // 移除冲突后重试可正常升级。
        conn.execute("DROP INDEX idx_audit_command", []).unwrap();
        let outcome = apply(&mut conn).unwrap();
        assert_eq!(outcome.applied, vec![3, 4]);
        assert_eq!(read_version(&conn).unwrap(), 4);
        integrity_check(&conn).unwrap();
    }
}
