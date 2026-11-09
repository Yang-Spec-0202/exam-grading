//! platform v4：`review_redo` 持久化 API、v3→v4 迁移/回滚、重启读取（task-66）。
//!
//! 运行：`cargo test -p eg-platform --test v4_review_redo`

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::sync::Arc;

use eg_domain::{Clock as _, Ulid};
use eg_platform::{DataDirs, FixedClock, Platform, repo, schema};
use rusqlite::Connection;

fn clock() -> Arc<FixedClock> {
    Arc::new(FixedClock::from_rfc3339("2026-11-09T00:00:00Z").unwrap())
}

fn ulid(seed: u128) -> Ulid {
    Ulid::from_u128(seed.max(1))
}

fn redo(seed: u128, command: &str) -> repo::RedoEntry {
    repo::RedoEntry {
        id: ulid(seed),
        command_id: command.to_owned(),
        action: "review.accept".to_owned(),
        entity_type: "grade_record".to_owned(),
        entity_id: format!("g-{seed}"),
        payload: serde_json::json!({"previous": "needs_review"}),
        created_at: clock().now(),
    }
}

#[test]
fn redo_push_pop_list_cursor_and_clear_semantics() {
    let temp = tempfile::tempdir().unwrap();
    let dirs = DataDirs::with_root(temp.path().join("data")).unwrap();
    let platform = Platform::open_with(dirs.clone(), clock()).unwrap();
    assert_eq!(
        platform.db().schema_version().unwrap(),
        schema::SCHEMA_VERSION
    );
    assert_eq!(schema::SCHEMA_VERSION, 4);
    platform.db().read(schema::verify_tables).unwrap();

    platform
        .db()
        .write(|tx| {
            repo::push_redo(tx, &redo(1, "cmd-redo-1"))?;
            repo::push_redo(tx, &redo(2, "cmd-redo-2"))?;
            repo::push_redo(tx, &redo(3, "cmd-redo-3"))?;
            Ok(())
        })
        .unwrap();

    // 最新在前 + limit。
    let page_one = platform
        .db()
        .read(|conn| repo::list_redo(conn, 2, None))
        .unwrap();
    assert_eq!(page_one.len(), 2);
    assert_eq!(page_one[0].id, ulid(3));
    assert_eq!(page_one[1].id, ulid(2));

    // (created_at,id) 游标：严格小于上一页最后一条。
    let cursor = repo::RedoCursor {
        created_at: page_one[1].created_at,
        id: page_one[1].id,
    };
    let page_two = platform
        .db()
        .read(|conn| repo::list_redo(conn, 2, Some(&cursor)))
        .unwrap();
    assert_eq!(page_two.len(), 1);
    assert_eq!(page_two[0].id, ulid(1));

    // LIFO pop 并删除。
    let popped = platform.db().write(|tx| repo::pop_redo(tx)).unwrap();
    assert_eq!(popped.map(|entry| entry.id), Some(ulid(3)));
    let remaining = platform
        .db()
        .read(|conn| repo::list_redo(conn, 10, None))
        .unwrap();
    assert_eq!(remaining.len(), 2);

    // clear_redo：新命令后重做失效；返回删除条数。
    let cleared = platform.db().write(|tx| repo::clear_redo(tx)).unwrap();
    assert_eq!(cleared, 2);
    assert_eq!(
        platform
            .db()
            .read(|conn| repo::list_redo(conn, 10, None))
            .unwrap()
            .len(),
        0
    );
    platform.close().unwrap();
}

#[test]
fn redo_survives_platform_restart_and_clear_keeps_undo() {
    let temp = tempfile::tempdir().unwrap();
    let dirs = DataDirs::with_root(temp.path().join("data")).unwrap();
    let platform = Platform::open_with(dirs.clone(), clock()).unwrap();
    platform
        .db()
        .write(|tx| {
            repo::push_undo(
                tx,
                &repo::UndoEntry {
                    id: ulid(11),
                    command_id: "cmd-undo-1".to_owned(),
                    action: "review.accept".to_owned(),
                    entity_type: "grade_record".to_owned(),
                    entity_id: "g-11".to_owned(),
                    payload: serde_json::json!({"previous": "needs_review"}),
                    created_at: clock().now(),
                },
            )?;
            repo::push_redo(tx, &redo(12, "cmd-redo-12"))?;
            Ok(())
        })
        .unwrap();
    platform.close().unwrap();

    // 重启后 redo/undo 均可读。
    let platform = Platform::open_with(dirs.clone(), clock()).unwrap();
    let redo_after_restart = platform
        .db()
        .read(|conn| repo::list_redo(conn, 10, None))
        .unwrap();
    assert_eq!(redo_after_restart.len(), 1);
    assert_eq!(redo_after_restart[0].id, ulid(12));
    let undo_after_restart = platform
        .db()
        .read(|conn| repo::list_undo(conn, 10))
        .unwrap();
    assert_eq!(undo_after_restart.len(), 1);

    // clear_redo 只清重做，不动撤销栈。
    platform.db().write(|tx| repo::clear_redo(tx)).unwrap();
    assert_eq!(
        platform
            .db()
            .read(|conn| repo::list_redo(conn, 10, None))
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        platform
            .db()
            .read(|conn| repo::list_undo(conn, 10))
            .unwrap()
            .len(),
        1
    );
    platform.close().unwrap();
}

fn build_v3_database(path: &std::path::Path) -> Connection {
    let conn = Connection::open(path).unwrap();
    for migration in schema::MIGRATIONS
        .iter()
        .filter(|migration| migration.version <= 3)
    {
        conn.execute_batch(migration.sql).unwrap();
        conn.execute(
            "INSERT INTO app_meta(key, value) VALUES ('schema_version', ?1) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [migration.version.to_string()],
        )
        .unwrap();
    }
    assert_eq!(schema::read_version(&conn).unwrap(), 3);
    conn.execute(
        "INSERT INTO review_undo(id, command_id, action, entity_type, entity_id, payload, created_at) \
         VALUES (?1, 'cmd-v3', 'review.accept', 'grade_record', 'g-v3', '{}', '2026-11-09T00:00:00Z')",
        [ulid(31).to_string()],
    )
    .unwrap();
    conn
}

#[test]
fn migrates_v3_to_v4_preserving_data() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("v3.db");
    let conn = build_v3_database(&path);
    drop(conn);

    let db = eg_platform::Database::open(&path).unwrap();
    assert_eq!(db.schema_version().unwrap(), 4);
    db.read(schema::verify_tables).unwrap();
    let undo = db.read(|conn| repo::list_undo(conn, 10)).unwrap();
    assert_eq!(undo.len(), 1, "v3 数据必须保留");
    let redo_entries = db.read(|conn| repo::list_redo(conn, 10, None)).unwrap();
    assert_eq!(redo_entries.len(), 0, "新表初始为空");
    db.write(|tx| repo::push_redo(tx, &redo(32, "cmd-v4")))
        .unwrap();
    assert_eq!(
        db.read(|conn| repo::list_redo(conn, 10, None))
            .unwrap()
            .len(),
        1
    );
    db.close().unwrap();
}

#[test]
fn failed_v4_migration_rolls_back_and_keeps_version() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("v3-rollback.db");
    let mut conn = build_v3_database(&path);
    // 预置同名表使 0004 的 CREATE TABLE 失败 → 迁移事务回滚。
    conn.execute("CREATE TABLE review_redo(id TEXT PRIMARY KEY)", [])
        .unwrap();
    let error = schema::apply(&mut conn).unwrap_err();
    assert!(
        error.to_string().contains("0004") || error.to_string().contains("迁移"),
        "错误应定位迁移失败：{error}"
    );
    assert_eq!(schema::read_version(&conn).unwrap(), 3, "失败不得推进版本");
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM review_undo", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        1,
        "既有数据必须保留"
    );
    // 失败的迁移没有留下索引（事务回滚）。
    let index: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_review_redo_created'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(index, 0);
    drop(conn);
}

#[test]
fn v4_backup_manifest_includes_redo_table() {
    let temp = tempfile::tempdir().unwrap();
    let dirs = DataDirs::with_root(temp.path().join("data")).unwrap();
    let platform = Platform::open_with(dirs, clock()).unwrap();
    let backup = platform
        .backup()
        .backup(
            platform.db(),
            Some(platform.assets()),
            eg_platform::BackupKind::Manual,
        )
        .unwrap();
    assert!(
        backup.manifest.table_counts.contains_key("review_redo"),
        "备份清单必须包含 review_redo"
    );
    platform.close().unwrap();
}
