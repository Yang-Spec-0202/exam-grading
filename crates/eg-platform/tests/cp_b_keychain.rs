//! CP-B 验收（R1）：真实 macOS Keychain 往返 + 密钥不泄漏扫描。
//!
//! 默认跳过：没有登录钥匙串/GUI 授权的环境（CI、无头）会出现系统授权框，
//! 因此本测试只在显式要求时执行：
//!
//! ```text
//! EXAM_GRADING_KEYCHAIN_SMOKE=1 cargo test -p eg-platform --test cp_b_keychain -- --nocapture
//! ```
//!
//! 覆盖：
//! 1. `KeychainSecretStore` 真实写入/覆盖/读取/删除（经 `keyring` → macOS Keychain）；
//! 2. 用独立工具 `security find-generic-password` 交叉确认条目真的落在登录钥匙串；
//! 3. 明文密钥不出现在数据根的任何文件、诊断包 zip 条目、日志/审计/配置/数据库里。
//!
//! 手工冒烟命令（不依赖本 crate，`security` CLI 直接往返）记录在
//! `docs/dev/research/platform.md` 的 CP-B 验收记录里。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
#[cfg(target_os = "macos")]
use std::process::Command;
use std::sync::Arc;

use eg_domain::Clock as _;
use eg_platform::{
    DataDirs, DiagnosticRequest, FixedClock, KeychainSecretStore, LogLevel, LogRecord, NewTask,
    Platform, Secret, SecretRef, SecretStore, TaskKind,
};

/// 冒烟用密钥明文（测试专用，非真实凭据）。
const SMOKE_SECRET: &str = "cp-b-keychain-smoke-7f3a9d2c";
/// 覆盖写之后的明文。
const SMOKE_SECRET_ROTATED: &str = "cp-b-keychain-rotated-1b8e4c6d";

/// 无论断言是否失败都删除 Keychain 条目，避免测试污染真实钥匙串。
struct KeychainCleanup {
    store: KeychainSecretStore,
    reference: SecretRef,
}

impl Drop for KeychainCleanup {
    fn drop(&mut self) {
        let _ = self.store.delete(&self.reference);
    }
}

fn smoke_enabled() -> bool {
    std::env::var("EXAM_GRADING_KEYCHAIN_SMOKE").as_deref() == Ok("1")
}

/// 独立于 keyring 的交叉验证：条目确实在登录钥匙串中。
///
/// 只用属性查询（不带 `-w`）：macOS 会为「其他程序读取密钥明文」弹授权框，
/// 无头环境无法自动点「允许」，因此明文交叉验证默认关闭，见 [`security_reads_value`]。
#[cfg(target_os = "macos")]
fn security_item_exists(service: &str, account: &str) -> bool {
    Command::new("security")
        .args(["find-generic-password", "-s", service, "-a", account])
        .output()
        .is_ok_and(|output| output.status.success())
}

/// 可选：用 `security -w` 读出明文交叉验证（会触发系统授权框，默认跳过）。
///
/// 手工验证时设置 `EXAM_GRADING_KEYCHAIN_SECURITY_READ=1` 并在桌面点击「允许」。
#[cfg(target_os = "macos")]
fn security_reads_value(service: &str, account: &str) -> Option<String> {
    let output = Command::new("security")
        .args(["find-generic-password", "-s", service, "-a", account, "-w"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    Some(text.trim_end_matches(['\r', '\n']).to_owned())
}

#[test]
fn keychain_real_roundtrip() {
    if !smoke_enabled() {
        eprintln!("[skip] 未设置 EXAM_GRADING_KEYCHAIN_SMOKE=1：跳过真实 Keychain 冒烟");
        return;
    }
    let service = format!("exam-grading-smoke-{}", std::process::id());
    let store = KeychainSecretStore::with_service(service.clone());
    let reference = SecretRef::cloud_api_key("smoke").expect("后端标识合法");
    let _cleanup = KeychainCleanup {
        store: store.clone(),
        reference: reference.clone(),
    };

    assert!(
        !store.exists(&reference).unwrap(),
        "冒烟用条目 {} 不应预先存在",
        reference.account()
    );

    // 1) 写入 → 存在 → 读取（真实 Keychain 往返）。
    store
        .set(&reference, &Secret::new(SMOKE_SECRET))
        .expect("写入真实 Keychain 失败（检查登录钥匙串是否解锁/授权）");
    assert!(store.exists(&reference).unwrap(), "写入后 exists 应为 true");
    let read_back = store.get(&reference).unwrap().expect("应读回密钥");
    assert_eq!(read_back.expose(), SMOKE_SECRET);
    eprintln!(
        "[ok] Keychain set/get 往返：service={service} account={}",
        reference.account()
    );

    // 2) 独立工具交叉验证：security CLI 能查到同一条目（属性查询，不读明文）。
    #[cfg(target_os = "macos")]
    {
        assert!(
            security_item_exists(&service, &reference.account()),
            "security CLI 查不到 keyring 刚写入的条目（说明可能未真正落盘）"
        );
        eprintln!("[ok] security CLI 交叉验证：条目存在于登录钥匙串");

        if std::env::var("EXAM_GRADING_KEYCHAIN_SECURITY_READ").as_deref() == Ok("1") {
            match security_reads_value(&service, &reference.account()) {
                Some(value) => {
                    assert_eq!(value, SMOKE_SECRET, "security CLI 读到的明文不一致");
                    eprintln!("[ok] security CLI 明文交叉验证通过");
                }
                None => eprintln!(
                    "[warn] security -w 未读到明文（ACL 授权框被拒/超时）；不影响 keyring 往返结论"
                ),
            }
        } else {
            eprintln!(
                "[info] 明文交叉验证默认跳过（macOS 会弹授权框）；\
                 需要时设 EXAM_GRADING_KEYCHAIN_SECURITY_READ=1 并在桌面点「允许」"
            );
        }
    }

    // 3) 覆盖写。
    store
        .set(&reference, &Secret::new(SMOKE_SECRET_ROTATED))
        .unwrap();
    assert_eq!(
        store.get(&reference).unwrap().unwrap().expose(),
        SMOKE_SECRET_ROTATED
    );
    eprintln!("[ok] Keychain 覆盖写");

    // 4) 删除（幂等）。
    store.delete(&reference).unwrap();
    assert_eq!(store.get(&reference).unwrap(), None, "删除后应读不到");
    store.delete(&reference).unwrap();
    #[cfg(target_os = "macos")]
    assert!(
        !security_item_exists(&service, &reference.account()),
        "删除后 security CLI 仍能查到条目"
    );
    eprintln!("[ok] Keychain 删除（含幂等；security CLI 确认条目已消失）");
}

/// 在真实 Keychain 存一把钥匙后，扫描平台的持久化产物，确认明文 0 命中。
#[test]
fn secret_never_leaks_to_disk() {
    if !smoke_enabled() {
        eprintln!("[skip] 未设置 EXAM_GRADING_KEYCHAIN_SMOKE=1：跳过密钥泄漏扫描");
        return;
    }
    let service = format!("exam-grading-smoke-{}", std::process::id());
    let store = KeychainSecretStore::with_service(service);
    let reference = SecretRef::cloud_api_key("smoke").expect("后端标识合法");
    let _cleanup = KeychainCleanup {
        store: store.clone(),
        reference: reference.clone(),
    };
    store.set(&reference, &Secret::new(SMOKE_SECRET)).unwrap();

    // 平台正常使用：写日志、任务、审计、资产，再导出诊断包。
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("data");
    let dirs = DataDirs::with_root(&root).unwrap();
    let clock = Arc::new(FixedClock::from_rfc3339("2026-11-02T12:00:00Z").unwrap());
    let platform = Platform::open_with(dirs.clone(), clock.clone()).unwrap();

    platform
        .log()
        .write(LogRecord::new(
            clock.now(),
            LogLevel::Info,
            "eg_inference",
            "cloud.call.start",
            format!("准备调用云端后端 backend=smoke secret_ref={reference}"),
        ))
        .unwrap();
    platform.log().flush().unwrap();

    let task = NewTask::new(
        TaskKind::PageRecognize,
        serde_json::json!({"backend": "smoke"}),
    );
    platform.tasks().enqueue(task).unwrap();
    platform
        .assets()
        .put(b"cp-b asset bytes", "image/png")
        .unwrap();

    // 诊断包（zip 条目解压后扫描明文）。
    let bundle = platform
        .diagnostics()
        .export(&DiagnosticRequest {
            include_schema: true,
            ..DiagnosticRequest::default()
        })
        .unwrap();
    let zip_hits = scan_zip_for_plaintext(&bundle.path, SMOKE_SECRET);
    assert!(
        zip_hits.is_empty(),
        "密钥明文出现在诊断包条目中：{zip_hits:?}"
    );
    eprintln!(
        "[ok] 诊断包 {} 个条目 0 命中密钥明文（{} 字节）",
        bundle.entries.len(),
        bundle.bytes
    );
    platform.close().unwrap();

    // 扫描数据根下所有文件（sqlite/WAL、JSONL 日志、配置、ulid 状态、备份……）。
    let mut files_scanned = 0_usize;
    let hits = scan_tree_for_plaintext(dirs.root(), SMOKE_SECRET, &mut files_scanned);
    assert!(
        hits.is_empty(),
        "密钥明文出现在持久化文件中：{hits:?}（扫描 {files_scanned} 个文件）"
    );
    eprintln!("[ok] 数据根 {files_scanned} 个文件 0 命中密钥明文");
    eprintln!("[ok] 密钥不泄漏检查完成：日志/数据库/审计/配置/诊断包均 0 命中");
}

fn scan_tree_for_plaintext(root: &Path, needle: &str, files: &mut usize) -> Vec<String> {
    let mut hits = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return hits;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            hits.extend(scan_tree_for_plaintext(&path, needle, files));
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        *files += 1;
        if contains(&bytes, needle.as_bytes()) {
            hits.push(path.display().to_string());
        }
    }
    hits
}

fn scan_zip_for_plaintext(zip_path: &Path, needle: &str) -> Vec<String> {
    let file = std::fs::File::open(zip_path).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    let mut hits = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).unwrap();
        let name = entry.name().unwrap_or_default().to_string();
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut bytes).unwrap();
        if contains(&bytes, needle.as_bytes()) {
            hits.push(name.clone());
        }
    }
    hits
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}
