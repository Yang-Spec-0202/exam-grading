//! `egctl` 平台薄封装：`backup` / `restore` / `diagnostics export`。
//!
//! 全部走 `eg-platform` 的既有服务（不重复实现备份/恢复/诊断包逻辑）；本模块只做参数、
//! 单实例守卫与人类/JSON 输出。恢复后**不自动启动应用**，由教师重启后复核。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use eg_platform::{
    BackupKind, BackupService, DataDirs, Database, DatabaseOptions, DiagnosticRequest,
    InstanceLock, Platform, SystemClock,
};

use crate::util::now;

/// 备份结果。
#[derive(Debug)]
pub struct BackupInfo {
    /// 备份目录。
    pub directory: PathBuf,
    /// 数据库字节数。
    pub database_bytes: u64,
    /// 数据库 SHA-256。
    pub database_sha256: String,
    /// schema 版本。
    pub schema_version: u32,
    /// 备份类型。
    pub kind: String,
    /// 清单文件路径。
    pub manifest_path: PathBuf,
}

/// 执行一次手动备份（需要独占平台：桌面端运行时会因单实例锁失败并提示退出）。
///
/// # Errors
///
/// 单实例被占用、备份或清单写入失败时返回中文错误。
pub fn backup(dirs: &DataDirs) -> std::result::Result<BackupInfo, String> {
    let platform = Platform::open(dirs.clone())
        .map_err(|error| format!("打开平台失败（若桌面端正在运行，请先退出）：{error}"))?;
    let record = platform
        .backup()
        .backup(
            platform.db().as_ref(),
            Some(platform.assets().as_ref()),
            BackupKind::Manual,
        )
        .map_err(|error| error.to_string());
    let close = platform.close().map_err(|error| error.to_string());
    let record = record?;
    close?;
    Ok(BackupInfo {
        directory: record.directory.clone(),
        database_bytes: record.manifest.database_bytes,
        database_sha256: record.manifest.database_sha256.to_hex(),
        schema_version: record.manifest.schema_version,
        kind: format!("{:?}", record.manifest.kind),
        manifest_path: record.directory.join("manifest.json"),
    })
}

/// 恢复结果。
#[derive(Debug)]
pub struct RestoreInfo {
    /// 使用的备份目录。
    pub backup_directory: PathBuf,
    /// 恢复目标数据库。
    pub target: PathBuf,
    /// 恢复前的安全副本（None = 原本无库）。
    pub safety_copy: Option<PathBuf>,
    /// 恢复后 integrity_check 是否通过。
    pub integrity_ok: bool,
    /// 恢复后 schema 版本。
    pub schema_version: Option<u32>,
}

/// 从备份目录恢复数据库（先取得单实例锁，确保桌面端未运行）。
///
/// # Errors
///
/// 备份缺失/损坏、目标不可写、恢复后完整性检查失败时返回中文错误。
pub fn restore(
    dirs: &DataDirs,
    backup_directory: &Path,
) -> std::result::Result<RestoreInfo, String> {
    let clock: eg_platform::SharedClock = Arc::new(SystemClock);
    let lock = InstanceLock::acquire(dirs, now(), concat!("egctl ", env!("CARGO_PKG_VERSION")))
        .map_err(|error| format!("无法取得单实例锁（桌面端可能正在运行）：{error}"))?;
    let service = BackupService::new(dirs.clone(), clock.clone(), 5);
    let report = service
        .restore(backup_directory, &dirs.database_path())
        .map_err(|error| error.to_string())?;
    // 恢复后立刻用平台打开一次（迁移 + 完整性校验），失败则如实报告。
    let verification = Database::open_with(
        &dirs.database_path(),
        &DatabaseOptions {
            backup_dir: None,
            clock: Some(clock),
        },
    );
    let info = match verification {
        Ok(db) => {
            let integrity_ok = db.integrity_check().is_ok();
            let schema_version = db.schema_version().ok();
            let _ = db.close();
            RestoreInfo {
                backup_directory: report.backup_directory,
                target: report.target,
                safety_copy: report.safety_copy,
                integrity_ok,
                schema_version,
            }
        }
        Err(error) => {
            drop(lock);
            return Err(format!("恢复后打开数据库失败：{error}"));
        }
    };
    drop(lock);
    if !info.integrity_ok {
        return Err("恢复后数据库 integrity_check 未通过（已保留安全副本，请联系维护）".to_owned());
    }
    Ok(info)
}

/// 诊断包结果。
#[derive(Debug)]
pub struct DiagnosticsInfo {
    /// 诊断包 zip 路径。
    pub path: PathBuf,
    /// 包内条目。
    pub entries: Vec<String>,
    /// 字节数。
    pub bytes: u64,
}

/// 导出诊断包（需要独占平台；不读取原图/识别全文）。
///
/// # Errors
///
/// 单实例被占用或诊断包导出失败时返回中文错误。
pub fn diagnostics(
    dirs: &DataDirs,
    output_dir: Option<PathBuf>,
    log_limit: usize,
) -> std::result::Result<DiagnosticsInfo, String> {
    let platform = Platform::open(dirs.clone())
        .map_err(|error| format!("打开平台失败（若桌面端正在运行，请先退出）：{error}"))?;
    let request = DiagnosticRequest {
        output_dir,
        include_schema: true,
        log_limit,
        ..DiagnosticRequest::default()
    };
    let bundle = platform
        .diagnostics()
        .export(&request)
        .map_err(|error| error.to_string());
    let close = platform.close().map_err(|error| error.to_string());
    let bundle = bundle?;
    close?;
    Ok(DiagnosticsInfo {
        path: bundle.path,
        entries: bundle.entries,
        bytes: bundle.bytes,
    })
}

/// 人类可读：备份结果。
#[must_use]
pub fn render_backup(info: &BackupInfo) -> String {
    format!(
        "备份完成：{}\n  类型：{}；schema {}；数据库 {} 字节；SHA-256 {}\n  清单：{}",
        info.directory.display(),
        info.kind,
        info.schema_version,
        info.database_bytes,
        info.database_sha256,
        info.manifest_path.display()
    )
}

/// 人类可读：恢复结果。
#[must_use]
pub fn render_restore(info: &RestoreInfo) -> String {
    format!(
        "恢复完成：{} → {}\n  安全副本：{}\n  完整性：{}；schema {}",
        info.backup_directory.display(),
        info.target.display(),
        info.safety_copy.as_ref().map_or_else(
            || "无（原本无库）".to_owned(),
            |path| path.display().to_string()
        ),
        if info.integrity_ok {
            "通过"
        } else {
            "失败"
        },
        info.schema_version
            .map_or_else(|| "未知".to_owned(), |version| version.to_string())
    )
}

/// 人类可读：诊断包结果。
#[must_use]
pub fn render_diagnostics(info: &DiagnosticsInfo) -> String {
    format!(
        "诊断包：{}（{} 字节，{} 个条目）\n  {}",
        info.path.display(),
        info.bytes,
        info.entries.len(),
        info.entries.join("\n  ")
    )
}
