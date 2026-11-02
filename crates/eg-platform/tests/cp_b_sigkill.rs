//! CP-B 验收（R2）：真实 `SIGKILL` 后任务归位与幂等（不重复建任务）。
//!
//! 父进程先建任务，再以子进程方式重跑本测试二进制（`--exact sigkill_worker`），
//! 子进程用 `Platform` 认领任务并打印 `READY`，随后被父进程 `Child::kill()`
//! （Unix 上即 `SIGKILL`，`kill -9`）。父进程把注入时钟推进到租约过期后重新
//! `Platform::open_with`（启动时自动 `recover_expired`），断言：
//!
//! 1. 任务从 `running` 归位为 `pending`，租约清空、尝试次数保留；
//! 2. 同输入再次入队命中幂等键（不新增任务行）；
//! 3. 退避到期后可再次认领（attempts=2），证明恢复后可继续执行；
//! 4. 强杀后审计哈希链仍完整（`verify_chain` 通过）。
//!
//! 运行：`cargo test -p eg-platform --test cp_b_sigkill -- --nocapture`

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::io::{BufRead, BufReader, Write as _};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use eg_platform::{
    BackupKind, BackupService, DataDirs, FixedClock, NewTask, Platform, TaskKind, TaskState,
};

const CLOCK: &str = "2026-11-02T12:00:00Z";
/// 子进程模式的环境变量：值是数据根路径。
const WORKER_ENV: &str = "EG_CP_B_CRASH_DIR";
/// 租约时长（`TaskQueue` 固定 30s；这里推进 31s 让它过期）。
const LEASE_ADVANCE_SECS: i64 = 31;

fn clock_at_base() -> Arc<FixedClock> {
    Arc::new(FixedClock::from_rfc3339(CLOCK).unwrap())
}

/// 启动子进程（同一测试二进制的 `sigkill_worker`），等 READY 后 `SIGKILL`，返回 READY 行。
///
/// 用子进程而不是线程，确保是**进程级强杀**：数据库写线程、日志线程、文件锁都随进程消失，
/// 等价于教师机器上任务执行中被强制结束/断电后的重启路径。
fn spawn_worker_and_sigkill(root: &std::path::Path) -> String {
    let exe = std::env::current_exe().unwrap();
    let mut child = Command::new(exe)
        .args(["--exact", "sigkill_worker", "--nocapture"])
        .env(WORKER_ENV, root.as_os_str())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (line_tx, line_rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if line_tx.send(line).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut ready_line = None;
    while Instant::now() < deadline {
        match line_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(line) if line.starts_with("READY ") => {
                ready_line = Some(line.trim().to_owned());
                break;
            }
            Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let Some(ready_line) = ready_line else {
        let _ = child.kill();
        let _ = child.wait();
        panic!("worker 未在 30s 内打印 READY（无法验证 SIGKILL 场景）");
    };
    eprintln!("[worker] {ready_line}");
    child.kill().unwrap(); // Unix：SIGKILL
    let status = child.wait().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        assert_eq!(
            status.signal(),
            Some(9),
            "worker 应被 SIGKILL 终止（kill -9），实际 {status:?}"
        );
    }
    eprintln!("[ok] worker 已被 SIGKILL（signal=9）");
    ready_line
}

/// 子进程入口：认领一个任务后打印 READY，然后阻塞等待父进程 SIGKILL。
#[test]
fn sigkill_worker() {
    let Ok(root) = std::env::var(WORKER_ENV) else {
        return; // 正常 `cargo test` 时直接跳过
    };
    let dirs = DataDirs::with_root(PathBuf::from(root)).unwrap();
    let platform = Platform::open_with(dirs, clock_at_base()).unwrap();
    let task = platform
        .tasks()
        .claim("cp-b-crash-worker")
        .unwrap()
        .expect("worker 应能认领父进程创建的待运行任务");
    println!(
        "READY task_id={} attempts={} state={}",
        task.id,
        task.attempts,
        task.state.code()
    );
    std::io::stdout().flush().unwrap();
    // 模拟耗时任务；父进程应在 ~1s 内 SIGKILL。
    std::thread::sleep(Duration::from_secs(120));
    panic!("worker 未被 SIGKILL（测试异常）");
}

#[test]
fn sigkill_recovery_requeues_running_task() {
    if std::env::var(WORKER_ENV).is_ok() {
        return; // 子进程里只跑 sigkill_worker
    }

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("data");
    let dirs = DataDirs::with_root(&root).unwrap();
    let clock = clock_at_base();

    // 1) 父进程建任务（真实 SQLite 行 + 审计）。
    let platform = Platform::open_with(dirs.clone(), clock.clone()).unwrap();
    let new_task = NewTask::new(
        TaskKind::PageRecognize,
        serde_json::json!({"submission_id": "sub-cp-b-1"}),
    )
    .with_scope(vec!["sub-cp-b-1".to_owned()])
    .with_input_hashes(vec!["blake3:cp-b-input-hash".to_owned()])
    .with_max_attempts(3);
    let enqueued = platform.tasks().enqueue(new_task.clone()).unwrap();
    assert!(enqueued.inserted, "首次入队应新建任务");
    let task_id = enqueued.task_id;
    platform.close().unwrap();

    // 2) 子进程认领任务后 SIGKILL（真实 kill -9）。
    let _ready = spawn_worker_and_sigkill(&root);

    // 3) 租约过期后重启：Platform::open_with 启动恢复应把 running 归位。
    clock
        .advance(chrono::Duration::seconds(LEASE_ADVANCE_SECS))
        .unwrap();
    let platform = Platform::open_with(dirs.clone(), clock.clone()).unwrap();
    let recovered = platform.tasks().get(task_id).unwrap().expect("任务应存在");
    assert_eq!(
        recovered.state,
        TaskState::Pending,
        "SIGKILL 后任务应回待运行（实际 {}）",
        recovered.state.code()
    );
    assert_eq!(recovered.attempts, 1, "尝试次数应保留");
    assert!(recovered.lease_owner.is_none(), "租约持有者应清空");
    assert!(recovered.lease_expires_at.is_none(), "租约到期时间应清空");
    assert_eq!(
        recovered.last_error.as_deref(),
        Some("租约过期（进程崩溃/强杀）")
    );
    eprintln!(
        "[ok] 重启恢复：task={} state=pending attempts=1 last_error={:?}",
        recovered.id, recovered.last_error
    );

    // 4) 幂等键不重复执行：同输入再次入队 → 命中既有任务，不新增行。
    let again = platform.tasks().enqueue(new_task).unwrap();
    assert!(!again.inserted, "同输入再次入队不应新建任务");
    assert_eq!(again.task_id, task_id, "幂等键应命中同一任务");
    eprintln!("[ok] 幂等键命中：未重复建任务（task_id={task_id}）");

    // 5) 退避到期后可再次认领（attempts=2），证明是「恢复后继续」而非重复投递。
    clock.advance(chrono::Duration::seconds(10)).unwrap();
    let claimed = platform
        .tasks()
        .claim("cp-b-restarted")
        .unwrap()
        .expect("退避到期后应可再认领同一任务");
    assert_eq!(claimed.id, task_id);
    assert_eq!(claimed.attempts, 2, "再次认领后 attempts 应为 2");
    assert_eq!(claimed.state, TaskState::Running);
    eprintln!(
        "[ok] 恢复后可继续执行：task={} state=running attempts=2 owner={:?}",
        claimed.id, claimed.lease_owner
    );

    // 6) 强杀后审计链仍完整（建任务 + 启动恢复 至少两条）。
    let chain = platform.audit().verify_chain().unwrap();
    assert!(chain.ok, "强杀重启后审计链应完整：{:?}", chain.broken);
    let actions: Vec<String> = platform
        .audit()
        .read_all()
        .unwrap()
        .into_iter()
        .map(|entry| entry.action)
        .collect();
    assert!(
        actions.iter().any(|action| action == "task.enqueue"),
        "审计缺少 task.enqueue：{actions:?}"
    );
    assert!(
        actions.iter().any(|action| action == "task.recover"),
        "审计缺少 task.recover：{actions:?}"
    );
    eprintln!("[ok] 审计链完整：{} 条，动作={actions:?}", chain.entries);

    platform.close().unwrap();
    eprintln!("[ok] SIGKILL 恢复验收通过（task={task_id}）");
}

/// D1 回归：强杀后 WAL 残留（running 状态在 WAL 里未 checkpoint）→ 直接用备份恢复 →
/// 若不清理旧库 `-wal`/`-shm`，下次打开会 `database disk image is malformed`。
///
/// 本测试覆盖「坏场景在最坏时刻恢复」：先备份，任务跑起来后被 `SIGKILL`（WAL 残留），
/// 不先打开平台做恢复，而是直接 `restore` 覆盖数据库，再验证完整性/表计数/任务状态。
#[test]
fn restore_after_sigkill_with_stale_wal() {
    if std::env::var(WORKER_ENV).is_ok() {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("data");
    let dirs = DataDirs::with_root(&root).unwrap();
    let clock = clock_at_base();

    // 1) 建任务 + 打备份（此时库干净、任务 pending）。
    let platform = Platform::open_with(dirs.clone(), clock.clone()).unwrap();
    let new_task = NewTask::new(
        TaskKind::PageRecognize,
        serde_json::json!({"submission_id": "sub-cp-b-wal"}),
    )
    .with_scope(vec!["sub-cp-b-wal".to_owned()]);
    let task_id = platform.tasks().enqueue(new_task).unwrap().task_id;
    let backup = platform
        .backup()
        .backup(platform.db(), Some(platform.assets()), BackupKind::Manual)
        .unwrap();
    platform.close().unwrap();
    let db_path = dirs.database_path();
    let wal_path = PathBuf::from(format!("{}-wal", db_path.display()));
    let shm_path = PathBuf::from(format!("{}-shm", db_path.display()));
    assert!(wal_path.exists(), "前置条件：关闭数据库后应残留 -wal");

    // 2) 子进程认领任务后被 SIGKILL：running 状态可能只存在于 WAL 中。
    let ready = spawn_worker_and_sigkill(&root);
    assert!(
        ready.contains("state=running"),
        "worker 应已进入 running：{ready}"
    );
    assert!(wal_path.exists(), "强杀后应残留 -wal（WAL 未 checkpoint）");

    // 3) 直接恢复（不先打开平台清 WAL）：restore 必须清理旧库侧车。
    let restored = BackupService::new(dirs.clone(), clock.clone(), 3)
        .restore(&backup.directory, &db_path)
        .unwrap();
    assert!(restored.safety_copy.is_some(), "覆盖已有库时应保留安全副本");
    assert!(!wal_path.exists(), "恢复后不得残留旧库 -wal（D1 回归）");
    assert!(!shm_path.exists(), "恢复后不得残留旧库 -shm（D1 回归）");

    // 4) 打开平台：不再报 malformed；数据回到备份时刻（任务 pending、attempts=0）。
    let platform = Platform::open_with(dirs.clone(), clock.clone()).unwrap();
    let verification = platform
        .backup()
        .verify_restored(&db_path, platform.assets())
        .unwrap();
    assert!(verification.ok(), "恢复后校验应通过：{verification:?}");
    assert_eq!(
        verification.table_counts, backup.manifest.table_counts,
        "恢复后表计数应与备份清单一致"
    );
    let task = platform.tasks().get(task_id).unwrap().expect("任务应存在");
    assert_eq!(task.state, TaskState::Pending, "恢复后任务应回到 pending");
    assert_eq!(task.attempts, 0, "恢复后尝试次数应回到 0（备份时刻）");
    let chain = platform.audit().verify_chain().unwrap();
    assert!(chain.ok, "恢复后审计链应完整：{:?}", chain.broken);
    platform.close().unwrap();
    eprintln!(
        "[ok] 强杀 + WAL 残留 + 恢复通过：无 malformed，表计数={}，审计链={} 条",
        verification.table_counts.len(),
        chain.entries
    );
}
