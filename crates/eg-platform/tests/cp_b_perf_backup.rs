//! CP-B 验收（R3 + 备份恢复端到端）：性能补测与 backup→restore→verify。
//!
//! 覆盖：
//! - 单实例启动时间（含 23 表迁移）与迁移本身的耗时；
//! - 资产去重 40 次同字节写入的耗时与结果（1 文件 1 行）；
//! - 诊断包导出耗时（目标 ≤10s，数据量见测试输出）；
//! - `FTS5` 关键字检索 P95（目标 ≤200ms，索引 1 万条）；
//! - backup → 改数据 → restore → 调用 `verify_restored` 全量校验（哈希/表计数/完整性）。
//!
//! 运行：`cargo test -p eg-platform --test cp_b_perf_backup -- --nocapture`
//!
//! 计时均用 `std::time::Instant`（本机实测，非仿真数据）；测试同时打印样本量，
//! 结果写入 `docs/dev/research/platform.md` 的「CP-B 验收记录」。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eg_domain::{Classroom, Clock as _};
use eg_platform::{
    BackupKind, BackupService, DataDirs, FixedClock, LogIndex, LogLevel, LogRecord, NewAuditEntry,
    NewTask, Platform, TaskKind, TaskState, repo, schema,
};

const CLOCK: &str = "2026-11-02T12:00:00Z";

fn clock() -> Arc<FixedClock> {
    Arc::new(FixedClock::from_rfc3339(CLOCK).unwrap())
}

fn ms(elapsed: Duration) -> f64 {
    elapsed.as_secs_f64() * 1000.0
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    let index = ((p * sorted.len() as f64).ceil() as usize)
        .saturating_sub(1)
        .min(sorted.len().saturating_sub(1));
    sorted[index]
}

fn pseudo_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(len);
    out
}

/// 单实例启动（含迁移）与迁移本身耗时。SPEC 未给数值目标，这里给实测值 + 10s 合理性上限。
/// 迁移表数随 schema 版本增长（v1：23 表；v2 只加列/索引，表数不变）。
#[test]
fn perf_startup_and_migration() {
    // A) 只测迁移：空目录第一次 `Database::open`（建 23 表 + 索引）。
    let temp_a = tempfile::tempdir().unwrap();
    let dirs_a = DataDirs::with_root(temp_a.path().join("data")).unwrap();
    dirs_a.ensure().unwrap();
    let started = Instant::now();
    let db = eg_platform::Database::open(&dirs_a.database_path()).unwrap();
    let migration = started.elapsed();
    let version = db.schema_version().unwrap();
    db.integrity_check().unwrap();
    db.close().unwrap();
    assert_eq!(
        version,
        eg_platform::schema::SCHEMA_VERSION,
        "迁移后 schema_version 应等于当前目标版本"
    );
    eprintln!(
        "[metric] 迁移 {} 表耗时：{:.1} ms（schema v{version}，integrity_check 通过）",
        schema::TABLES.len(),
        ms(migration)
    );

    // B) 平台冷启动：空数据根 → 配置/密钥/库迁移/日志/索引/单实例锁/启动恢复。
    let temp_b = tempfile::tempdir().unwrap();
    let dirs_b = DataDirs::with_root(temp_b.path().join("data")).unwrap();
    let started = Instant::now();
    let platform = Platform::open_with(dirs_b.clone(), clock()).unwrap();
    let cold_start = started.elapsed();
    let tables = schema::TABLES.len();
    platform.close().unwrap();
    eprintln!(
        "[metric] 平台冷启动耗时：{:.1} ms（含迁移 {tables} 张表）",
        ms(cold_start)
    );

    // C) 热启动：已有库 + 配置 + 索引。
    let started = Instant::now();
    let platform = Platform::open_with(dirs_b, clock()).unwrap();
    let warm_start = started.elapsed();
    platform.close().unwrap();
    eprintln!("[metric] 平台热启动耗时：{:.1} ms", ms(warm_start));

    assert_eq!(tables, 29, "schema 表数应为 29（v3）");
    assert!(
        cold_start < Duration::from_secs(10),
        "冷启动异常缓慢：{:.1} ms",
        ms(cold_start)
    );
    assert!(
        migration < Duration::from_secs(10),
        "迁移异常缓慢：{:.1} ms",
        ms(migration)
    );
}

/// 资产去重：同 1MiB 字节写 40 次 → 1 文件 1 行；记录总耗时。
#[test]
fn perf_asset_dedupe_40_puts() {
    let temp = tempfile::tempdir().unwrap();
    let dirs = DataDirs::with_root(temp.path().join("data")).unwrap();
    let platform = Platform::open_with(dirs, clock()).unwrap();

    let payload = pseudo_bytes(1024 * 1024, 42);
    let started = Instant::now();
    let mut deduped = 0_u32;
    for _ in 0..40 {
        let outcome = platform.assets().put(&payload, "image/png").unwrap();
        assert_eq!(outcome.byte_len, payload.len() as u64);
        if outcome.deduped {
            deduped += 1;
        }
    }
    let elapsed = started.elapsed();
    let usage = platform.assets().usage().unwrap();
    platform.close().unwrap();

    assert_eq!(usage.entries, 1, "同字节 40 次应只有 1 行资产记录");
    assert_eq!(usage.total_bytes, 1_048_576, "应只有 1MiB 落盘");
    assert_eq!(deduped, 39, "后 39 次应命中内容寻址去重");
    eprintln!(
        "[metric] 资产去重 40 次 × 1MiB：{:.1} ms（entries={} bytes={} deduped={}）",
        ms(elapsed),
        usage.entries,
        usage.total_bytes,
        deduped
    );
}

/// FTS5 检索 P95：目标 ≤200ms；索引 1 万条，100 次检索。
#[test]
fn perf_log_index_search_p95() {
    const N: usize = 10_000;
    const QUERIES: usize = 100;

    let temp = tempfile::tempdir().unwrap();
    let index_path = temp.path().join("log-index").join("log-index.sqlite3");
    let index = LogIndex::open(&index_path).unwrap();
    let at = eg_domain::time::parse_rfc3339("2026-11-02T12:00:00Z").unwrap();
    let mut seed_times = Vec::with_capacity(N);
    let started = Instant::now();
    for i in 0..N {
        let record = LogRecord::new(
            at,
            LogLevel::Info,
            "eg_recognize",
            "page.recognize",
            format!(
                "第 {} 页识别失败，已重试；submission=sub-{:05}，耗时 {} ms",
                i,
                i,
                i % 900
            ),
        );
        let item_start = Instant::now();
        index.add(&record).unwrap();
        seed_times.push(item_start.elapsed());
    }
    let seed_total = started.elapsed();
    let state = index.state().unwrap();
    assert_eq!(state.indexed_records, N as u64, "索引条数应为 1 万");

    let mut durations = Vec::with_capacity(QUERIES);
    for _ in 0..QUERIES {
        let started = Instant::now();
        let hits = index.search("识别失败", 20, 0).unwrap();
        durations.push(started.elapsed());
        assert!(!hits.is_empty(), "FTS5 检索应命中");
    }
    durations.sort();
    let p50 = percentile(&durations, 0.50);
    let p95 = percentile(&durations, 0.95);
    let max = *durations.last().unwrap();

    // 对照：<3 字关键词走 LIKE 回退（记录，不设目标）。
    let mut short_durations = Vec::with_capacity(20);
    for _ in 0..20 {
        let started = Instant::now();
        let hits = index.search("失败", 20, 0).unwrap();
        short_durations.push(started.elapsed());
        assert_ne!(hits.len(), 0);
    }
    short_durations.sort();
    let short_p95 = percentile(&short_durations, 0.95);

    assert!(
        p95 <= Duration::from_millis(200),
        "FTS5 检索 P95 未达标：{:.1} ms > 200 ms",
        ms(p95)
    );
    eprintln!(
        "[metric] FTS5 检索：索引 {N} 条；写入总耗时 {:.1} ms（单条 P95 {:.2} ms）；\
         检索 {QUERIES} 次 P50={:.1} ms P95={:.1} ms max={:.1} ms（目标 ≤200ms）",
        ms(seed_total),
        ms(percentile(&seed_times, 0.95)),
        ms(p50),
        ms(p95),
        ms(max)
    );
    eprintln!(
        "[metric] 短关键词「失败」LIKE 回退（20 次）P95={:.1} ms（仅为记录，无目标）",
        ms(short_p95)
    );
}

/// 诊断包导出：目标 ≤10s；样本 = 2 千条日志 + 100 任务 + 100 审计 + 10 个资产。
#[test]
fn perf_diagnostics_export_under_10s() {
    const LOGS: usize = 2_000;
    const TASKS: usize = 100;
    const AUDITS: usize = 100;

    let temp = tempfile::tempdir().unwrap();
    let dirs = DataDirs::with_root(temp.path().join("data")).unwrap();
    let platform = Platform::open_with(dirs.clone(), clock()).unwrap();
    let now = platform.clock().now();

    for i in 0..LOGS {
        platform
            .log()
            .write(LogRecord::new(
                now,
                LogLevel::Info,
                "eg_grade",
                "grade.item",
                format!("第 {i} 份提交判分完成，score={}", i % 100),
            ))
            .unwrap();
        if i % 200 == 0 {
            platform.log().flush().unwrap();
        }
    }
    platform.log().flush().unwrap();
    assert_eq!(platform.log().dropped_total(), 0, "种子日志不应被丢弃");

    for i in 0..TASKS {
        platform
            .tasks()
            .enqueue(
                NewTask::new(TaskKind::PageRecognize, serde_json::json!({"page": i}))
                    .with_scope(vec![format!("cp-b-perf-page-{i}")]),
            )
            .unwrap();
    }
    for i in 0..AUDITS {
        platform
            .audit()
            .append(NewAuditEntry::new(
                "cp_b.perf",
                "diagnostics",
                format!("sample-{i}"),
            ))
            .unwrap();
    }
    for i in 0..10 {
        platform
            .assets()
            .put(
                &pseudo_bytes(256 * 1024, u64::try_from(i).unwrap()),
                "image/png",
            )
            .unwrap();
    }
    let page = platform
        .log()
        .query(&eg_platform::LogQuery {
            limit: LOGS,
            ..eg_platform::LogQuery::default()
        })
        .unwrap();
    assert!(page.total >= LOGS, "日志样本不足：{}", page.total);

    let started = Instant::now();
    let bundle = platform
        .diagnostics()
        .export(&eg_platform::DiagnosticRequest {
            include_schema: true,
            log_limit: 5_000,
            ..eg_platform::DiagnosticRequest::default()
        })
        .unwrap();
    let elapsed = started.elapsed();
    platform.close().unwrap();

    assert!(
        elapsed <= Duration::from_secs(10),
        "诊断包导出未达标：{:.1} ms > 10000 ms",
        ms(elapsed)
    );
    assert!(bundle.bytes > 0);
    assert!(bundle.entries.contains(&"logs.jsonl".to_owned()));
    assert!(bundle.entries.contains(&"schema.txt".to_owned()));
    eprintln!(
        "[metric] 诊断包导出：{:.1} ms（目标 ≤10000 ms）；样本 日志{}条/任务{}个/审计{}条/资产10个；\
         包 {} 条目录、{} 字节",
        ms(elapsed),
        LOGS,
        TASKS,
        AUDITS,
        bundle.entries.len(),
        bundle.bytes
    );
}

/// 备份 → 改数据 → 恢复 → 校验（全量哈希/表计数/完整性/数据回归）。
#[test]
#[allow(clippy::too_many_lines)]
fn backup_restore_verify_end_to_end() {
    let temp = tempfile::tempdir().unwrap();
    let dirs = DataDirs::with_root(temp.path().join("data")).unwrap();
    let clock = clock();

    // 1) 造数据：1 个班级 + 3 个任务 + 3 个资产（审计由入队自动写）。
    let platform = Platform::open_with(dirs.clone(), clock.clone()).unwrap();
    let classroom = Classroom {
        id: platform.ids().next_classroom_id().unwrap(),
        name: "高三（1）班".to_owned(),
        term: "2026 秋".to_owned(),
    };
    let classroom_id = classroom.id;
    let insert_at = clock.now();
    platform
        .db()
        .write(move |tx| {
            repo::insert_classroom(tx, &classroom, Some("cp-b 验收"), insert_at)?;
            Ok(())
        })
        .unwrap();
    let task_ids: Vec<_> = (0..3)
        .map(|i| {
            platform
                .tasks()
                .enqueue(
                    NewTask::new(TaskKind::PageRecognize, serde_json::json!({"page": i}))
                        .with_scope(vec![format!("cp-b-page-{i}")]),
                )
                .unwrap()
                .task_id
        })
        .collect();
    for _ in 0..3 {
        platform
            .assets()
            .put(b"cp-b backup asset", "image/png")
            .unwrap();
    }
    let backup = platform
        .backup()
        .backup(platform.db(), Some(platform.assets()), BackupKind::Manual)
        .unwrap();
    assert_eq!(backup.manifest.table_counts.get("classroom"), Some(&1));
    assert_eq!(backup.manifest.table_counts.get("task"), Some(&3));
    assert_eq!(backup.manifest.asset_count, 1, "同字节资产应去重为 1 行");
    eprintln!(
        "[ok] 备份完成：{}（classroom=1 task=3 asset={}）",
        backup.directory.display(),
        backup.manifest.asset_count
    );
    platform.close().unwrap();

    // 2) 备份后改数据（模拟误删 + 新增）：删班级、加 1 个任务。
    let platform = Platform::open_with(dirs.clone(), clock.clone()).unwrap();
    let delete_id = classroom_id.to_string();
    platform
        .db()
        .write(move |tx| {
            tx.execute("DELETE FROM classroom WHERE id = ?1", [delete_id.clone()])
                .map_err(eg_platform::PlatformError::from)?;
            Ok(())
        })
        .unwrap();
    platform
        .tasks()
        .enqueue(NewTask::new(
            TaskKind::ReportExport,
            serde_json::json!({"after_backup": true}),
        ))
        .unwrap();
    let mutated_tasks = platform.tasks().pending_count().unwrap();
    assert_eq!(mutated_tasks, 4, "改数据后应有 4 个待运行任务");
    platform.close().unwrap();

    // 3) 恢复（当前库会被先存安全副本）。
    let db_path = dirs.database_path();
    let wal_path = PathBuf::from(format!("{}-wal", db_path.display()));
    let shm_path = PathBuf::from(format!("{}-shm", db_path.display()));
    // 关闭数据库后 WAL/SHM 仍会保留（SQLite 正常行为）；恢复实现必须清理，
    // 否则旧 WAL 会被重放到备份快照上 → `database disk image is malformed`（缺陷 D1）。
    assert!(
        wal_path.exists(),
        "前置条件：关闭数据库后应残留 -wal 侧车文件"
    );

    let backups = BackupService::new(dirs.clone(), clock.clone(), 3);
    let restore = backups.restore(&backup.directory, &db_path).unwrap();
    assert!(
        restore
            .safety_copy
            .as_ref()
            .is_some_and(|path| path.exists()),
        "恢复前应保留当前库的安全副本"
    );
    assert!(
        !wal_path.exists(),
        "恢复后不得残留旧库 -wal（缺陷 D1 回归）"
    );
    assert!(
        !shm_path.exists(),
        "恢复后不得残留旧库 -shm（缺陷 D1 回归）"
    );
    eprintln!("[ok] 恢复完成：安全副本已生成，旧库 WAL/SHM 侧车已清理");

    // 4) 打开平台并做恢复后全量校验。
    let platform = Platform::open_with(dirs.clone(), clock.clone()).unwrap();
    let verification = platform
        .backup()
        .verify_restored(&dirs.database_path(), platform.assets())
        .unwrap();
    assert!(verification.integrity_ok, "恢复后 integrity_check 应通过");
    assert_eq!(verification.schema_version, backup.manifest.schema_version);
    assert_eq!(
        verification.table_counts, backup.manifest.table_counts,
        "恢复后表计数应与备份清单一致"
    );
    assert_eq!(
        verification.assets_checked, backup.manifest.asset_count,
        "恢复后资产校验数量应与清单一致"
    );
    assert_eq!(verification.asset_failures, 0);
    assert!(verification.ok());

    // 5) 数据层回归：班级恢复、备份后新增的任务消失、备份任务仍在。
    let restored_classrooms = platform.db().read(repo::list_classrooms).unwrap();
    assert_eq!(restored_classrooms.len(), 1, "班级应从备份恢复");
    assert_eq!(restored_classrooms[0].id, classroom_id);
    let mut restored_task_ids: Vec<_> = platform
        .tasks()
        .list(&eg_platform::TaskFilter {
            state: Some(TaskState::Pending),
            ..eg_platform::TaskFilter::default()
        })
        .unwrap()
        .into_iter()
        .map(|record| record.id)
        .collect();
    restored_task_ids.sort();
    let mut expected = task_ids;
    expected.sort();
    assert_eq!(
        restored_task_ids, expected,
        "恢复后任务集合应回到备份时刻（新增任务消失）"
    );
    assert_eq!(platform.assets().usage().unwrap().entries, 1);
    let chain = platform.audit().verify_chain().unwrap();
    assert!(chain.ok, "恢复后审计链应完整：{:?}", chain.broken);
    platform.close().unwrap();

    eprintln!(
        "[ok] 备份恢复端到端通过：integrity=ok schema=v{} 表计数{} 资产{} 审计链{}条",
        verification.schema_version,
        verification.table_counts.len(),
        verification.assets_checked,
        chain.entries
    );
}
