//! platform v3 备份/恢复兼容：新表（撤销栈/导出记录/判分幂等/断点）随备份恢复，
//! schema 版本、审计链与表计数一致（task-42）。
//!
//! 运行：`cargo test -p eg-platform --test v3_backup_restore`

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;

use eg_domain::{Clock as _, GradeMode, Sha256Hex, Ulid};
use eg_platform::repo::GradeProgressState;
use eg_platform::{
    BackupKind, BackupService, DataDirs, FixedClock, NewAuditEntry, Platform, repo, schema,
};

fn clock() -> Arc<FixedClock> {
    Arc::new(FixedClock::from_rfc3339("2026-11-02T12:00:00Z").unwrap())
}

fn ulid(seed: u128) -> Ulid {
    Ulid::from_u128(seed.max(1))
}

#[test]
#[allow(clippy::too_many_lines)]
fn v3_tables_survive_backup_and_restore() {
    let temp = tempfile::tempdir().unwrap();
    let dirs = DataDirs::with_root(temp.path().join("data")).unwrap();
    let clock = clock();
    assert_eq!(schema::SCHEMA_VERSION, 3, "本测试针对 schema v3");

    let platform = Platform::open_with(dirs.clone(), clock.clone()).unwrap();
    assert_eq!(platform.db().schema_version().unwrap(), 3);

    // 造 v3 数据：撤销栈 2 条、导出记录 1 条、幂等 1 条、断点 1 条、审计命令 1 条。
    let undo_first = repo::UndoEntry {
        id: ulid(101),
        command_id: "cmd-backup-1".to_owned(),
        action: "review.accept".to_owned(),
        entity_type: "grade_record".to_owned(),
        entity_id: "g-1".to_owned(),
        payload: serde_json::json!({"previous": "needs_review"}),
        created_at: clock.now(),
    };
    let undo_second = repo::UndoEntry {
        id: ulid(102),
        command_id: "cmd-backup-2".to_owned(),
        action: "review.change_score".to_owned(),
        entity_type: "grade_record".to_owned(),
        entity_id: "g-1".to_owned(),
        payload: serde_json::json!({"previous": 5000}),
        created_at: clock.now(),
    };
    let export = repo::ExportRecord {
        id: ulid(201),
        kind: "paper_report".to_owned(),
        format: "xlsx".to_owned(),
        scope: serde_json::json!({"paper_id": "p-1"}),
        summary_hash: Sha256Hex::of(b"v3-backup-report"),
        row_count: 3,
        body: serde_json::json!({"sheet": "成绩单"}),
        created_at: clock.now(),
        audit_id: None,
    };
    let idempotency = repo::GradeIdempotency {
        key: "idem-backup-1".to_owned(),
        submission_id: eg_domain::SubmissionId::from_ulid(ulid(301)),
        question_id: eg_domain::QuestionId::from_ulid(ulid(302)),
        mode: GradeMode::Standard,
        grade_id: None,
        result_summary: serde_json::json!({"awarded": 5000}),
        created_at: clock.now(),
    };
    let progress = repo::GradeProgress {
        submission_id: eg_domain::SubmissionId::from_ulid(ulid(301)),
        question_id: None,
        state: GradeProgressState::Running,
        attempts: 2,
        engine_fingerprint: None,
        detail: Some(serde_json::json!({"done_questions": 1})),
        updated_at: clock.now(),
    };
    platform
        .db()
        .write(move |tx| {
            repo::push_undo(tx, &undo_first)?;
            repo::push_undo(tx, &undo_second)?;
            repo::insert_export_record(tx, &export)?;
            repo::put_grade_idempotency(tx, &idempotency)?;
            repo::put_grade_progress(tx, &progress)?;
            Ok(())
        })
        .unwrap();
    platform
        .audit()
        .append(
            NewAuditEntry::new("review.change_score", "grade_record", "g-1")
                .with_command_id("cmd-backup-2")
                .with_reason_code("manual_override")
                .with_scope("single_question")
                .with_confirmed(true)
                .with_sampling_verdict("fail"),
        )
        .unwrap();

    let backup = platform
        .backup()
        .backup(platform.db(), Some(platform.assets()), BackupKind::Manual)
        .unwrap();
    for table in [
        "review_undo",
        "export_record",
        "grade_idempotency",
        "grade_progress",
        "audit_log",
    ] {
        assert!(
            backup.manifest.table_counts.contains_key(table),
            "备份清单应包含表 {table}"
        );
    }
    assert_eq!(backup.manifest.table_counts.get("review_undo"), Some(&2));
    assert_eq!(backup.manifest.table_counts.get("export_record"), Some(&1));
    platform.close().unwrap();

    // 备份后删数据，再恢复，验证 v3 行回来。
    let platform = Platform::open_with(dirs.clone(), clock.clone()).unwrap();
    platform
        .db()
        .write(|tx| {
            tx.execute("DELETE FROM review_undo", [])
                .map_err(eg_platform::PlatformError::from)?;
            tx.execute("DELETE FROM export_record", [])
                .map_err(eg_platform::PlatformError::from)?;
            Ok(())
        })
        .unwrap();
    platform.close().unwrap();

    let db_path = dirs.database_path();
    let backups = BackupService::new(dirs.clone(), clock.clone(), 3);
    backups.restore(&backup.directory, &db_path).unwrap();

    let platform = Platform::open_with(dirs.clone(), clock.clone()).unwrap();
    let verification = platform
        .backup()
        .verify_restored(&dirs.database_path(), platform.assets())
        .unwrap();
    assert!(verification.integrity_ok);
    assert_eq!(verification.schema_version, 3);
    assert_eq!(verification.table_counts, backup.manifest.table_counts);
    assert!(verification.ok());

    let undo = platform
        .db()
        .read(|conn| repo::list_undo(conn, 10))
        .unwrap();
    assert_eq!(undo.len(), 2, "撤销栈应随备份恢复");
    let exports = platform
        .db()
        .read(|conn| repo::list_export_records(conn, 10))
        .unwrap();
    assert_eq!(exports.len(), 1, "导出记录应随备份恢复");
    assert_eq!(exports[0].row_count, 3);
    let replay = platform
        .db()
        .read(|conn| repo::find_export_records_by_summary(conn, Sha256Hex::of(b"v3-backup-report")))
        .unwrap();
    assert_eq!(replay.len(), 1, "恢复后仍可按摘要重放定位");
    let idem = platform
        .db()
        .read(|conn| repo::get_grade_idempotency(conn, "idem-backup-1"))
        .unwrap();
    assert!(idem.is_some(), "幂等键应随备份恢复");
    let progress = platform
        .db()
        .read(|conn| repo::list_incomplete_grade_progress(conn, 10))
        .unwrap();
    assert_eq!(progress.len(), 1, "未完成断点应随备份恢复");
    assert_eq!(progress[0].attempts, 2);

    let commands = platform
        .audit()
        .list(&eg_platform::AuditQuery {
            command_id: Some("cmd-backup-2".to_owned()),
            ..eg_platform::AuditQuery::default()
        })
        .unwrap();
    assert_eq!(commands.len(), 1, "审计命令字段应随备份恢复");
    assert_eq!(commands[0].scope.as_deref(), Some("single_question"));
    assert_eq!(commands[0].sampling_verdict.as_deref(), Some("fail"));
    assert!(
        platform.audit().verify_chain().unwrap().ok,
        "恢复后审计链应完整"
    );
    platform.close().unwrap();
}
