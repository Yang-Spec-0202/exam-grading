//! `egctl doctor`：环境自检（数据目录、DB、单实例、模型、推理依赖、CAS、日志/审计、配置、模块就绪度）。
//!
//! 原则（`SPEC.md` §7、AGENTS.md）：**不伪造 OK**——未实现/未就绪的模块一律 WARN 并写明原因；
//! 只有实际执行成功的检查才给 OK。退出码由 [`crate::util::exit_code`] 决定（OK=0、WARN=2、ERROR=1）。

use std::net::{TcpStream, ToSocketAddrs as _};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use eg_domain::Clock as _;
use eg_platform::config::ConfigDocument;
use eg_platform::{
    AppConfig, AuditLog, DataDirs, Database, DatabaseOptions, InstanceLock, SystemClock,
    UlidFactory, repo,
};

use crate::registry;
use crate::util::{
    Check, Level, dir_stats, dir_writable, find_in_path, free_disk_mib, human_bytes, modified_text,
    repo_root, run_command, truncate,
};

/// doctor 选项。
pub struct Options<'a> {
    /// 数据目录。
    pub dirs: &'a DataDirs,
    /// 是否深度检查其他 crate 的编译状态（慢；默认关闭）。
    pub deep: bool,
    /// 权重文件全量 SHA-256 阈值（MiB）。
    pub max_sha_mib: u64,
}

/// 未接入/未就绪的并行模块（doctor 必须如实报告，不用 OK 掩盖）。
const PENDING_MODULES: [(&str, &str); 5] = [
    ("eg-capture", "导入/PDF 拆页/预处理"),
    ("eg-recognize", "OCR/结构识别"),
    ("eg-align", "身份与题目区域对齐"),
    ("eg-grade", "判分流水线"),
    ("eg-app", "Slint 桌面应用"),
];

/// 运行全部检查。
#[must_use]
pub fn diagnose(options: &Options<'_>) -> Vec<Check> {
    let dirs = options.dirs;
    let clock: eg_platform::SharedClock = Arc::new(SystemClock);
    let mut checks = Vec::new();
    checks.push(check_environment(dirs));
    checks.push(check_data_dir(dirs));
    let config = check_config(dirs);
    let config_values = config.1;
    checks.push(config.0);
    let database = open_database(dirs, &clock);
    checks.push(database.0);
    let db = database.1;
    checks.push(check_instance(dirs));
    checks.push(check_audit(db.as_ref(), &clock));
    checks.push(check_assets(db.as_ref(), dirs));
    checks.push(check_logs(dirs, &config_values));
    checks.push(check_models(dirs, &config_values, options.max_sha_mib));
    checks.push(check_llama_cpp(dirs, &config_values));
    checks.push(check_python_uv());
    checks.push(check_report_self_test());
    checks.extend(check_modules(options.deep));
    checks
}

fn check_environment(dirs: &DataDirs) -> Check {
    let detail = format!(
        "egctl {} · {} {} · 数据根 {}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        dirs.root().display()
    );
    Check::new("env", "运行环境", Level::Ok, detail)
}

fn check_data_dir(dirs: &DataDirs) -> Check {
    let root = dirs.root();
    if !root.exists() {
        return Check::new(
            "data_dir",
            "数据目录",
            Level::Warn,
            format!("尚未创建：{}（首次启动时自动创建）", root.display()),
        );
    }
    let writable = dir_writable(root);
    let free = free_disk_mib(root);
    let detail = format!(
        "路径 {}；可写 {}；可用空间 {}",
        root.display(),
        if writable { "是" } else { "否" },
        free.map_or_else(
            || "无法测量（df 不可用）".to_owned(),
            |mib| format!("{mib} MiB")
        )
    );
    let level = if !writable {
        Level::Error
    } else if free.is_some_and(|mib| mib < 1024) {
        Level::Warn
    } else {
        Level::Ok
    };
    Check::new("data_dir", "数据目录", level, detail)
}

/// 配置检查：返回（检查结果, 配置值；解析失败时为默认值）。
fn check_config(dirs: &DataDirs) -> (Check, AppConfig) {
    let path = dirs.config_path();
    if !path.is_file() {
        return (
            Check::new(
                "config",
                "配置",
                Level::Warn,
                format!("尚未生成：{}（首次启动写入默认值）", path.display()),
            ),
            AppConfig::default(),
        );
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            return (
                Check::new("config", "配置", Level::Error, format!("读取失败：{error}")),
                AppConfig::default(),
            );
        }
    };
    let document: ConfigDocument = match toml::from_str(&text) {
        Ok(document) => document,
        Err(error) => {
            return (
                Check::new("config", "配置", Level::Error, format!("解析失败：{error}")),
                AppConfig::default(),
            );
        }
    };
    let mut detail = format!(
        "路径 {}；schema_version {}；",
        path.display(),
        document.meta.schema_version
    );
    let level = match document.config.validate() {
        Ok(()) => {
            let deviations = deviations(&document.config);
            if deviations.is_empty() {
                detail.push_str("校验通过；全部取默认值");
                Level::Ok
            } else {
                detail.push_str(&format!("校验通过；非默认项：{}", deviations.join("、")));
                Level::Ok
            }
        }
        Err(error) => {
            detail.push_str(&format!("校验失败：{error}"));
            Level::Error
        }
    };
    (Check::new("config", "配置", level, detail), document.config)
}

fn deviations(config: &AppConfig) -> Vec<String> {
    let default = AppConfig::default();
    let mut out = Vec::new();
    let mut push = |name: &str, differs: bool| {
        if differs {
            out.push(name.to_owned());
        }
    };
    push("general", config.general != default.general);
    push("ui", config.ui != default.ui);
    push("import", config.import != default.import);
    push("identity", config.identity != default.identity);
    push("align", config.align != default.align);
    push("ocr", config.ocr != default.ocr);
    push("grade", config.grade != default.grade);
    push("fast", config.fast != default.fast);
    push("rubric", config.rubric != default.rubric);
    push("cas", config.cas != default.cas);
    push("log", config.log != default.log);
    push("diagnostics", config.diagnostics != default.diagnostics);
    push("storage", config.storage != default.storage);
    push("backup", config.backup != default.backup);
    push("inference", config.inference != default.inference);
    push("privacy", config.privacy != default.privacy);
    push("docs", config.docs != default.docs);
    push("dev", config.dev != default.dev);
    out
}

/// 打开数据库并做 integrity_check + 迁移版本检查；返回（检查, 数据库句柄）。
fn open_database(
    dirs: &DataDirs,
    clock: &eg_platform::SharedClock,
) -> (Check, Option<Arc<Database>>) {
    let path = dirs.database_path();
    if !path.is_file() {
        return (
            Check::new(
                "db",
                "数据库",
                Level::Warn,
                format!("尚未创建：{}（首次启动时迁移建库）", path.display()),
            ),
            None,
        );
    }
    let options = DatabaseOptions {
        backup_dir: None,
        clock: Some(clock.clone()),
    };
    match Database::open_with(&path, &options) {
        Ok(db) => {
            let version = db.schema_version();
            let integrity = db.integrity_check();
            let mut detail = format!("路径 {}；", path.display());
            match (version, integrity) {
                (Ok(version), Ok(())) => {
                    detail.push_str(&format!("integrity_check 通过；schema 版本 {version}"));
                    let counts = db.read(|conn| repo::asset_usage(conn)).ok();
                    if let Some((assets, bytes)) = counts {
                        detail.push_str(&format!("；资产 {assets} 个（{}）", human_bytes(bytes)));
                    }
                    (
                        Check::new("db", "数据库", Level::Ok, detail),
                        Some(Arc::new(db)),
                    )
                }
                (version, integrity) => {
                    detail.push_str(&format!(
                        "schema {}；integrity {}",
                        version.map_or_else(
                            |error| format!("读取失败：{error}"),
                            |version| version.to_string()
                        ),
                        integrity
                            .map_or_else(|error| format!("失败：{error}"), |()| "通过".to_owned())
                    ));
                    (Check::new("db", "数据库", Level::Error, detail), None)
                }
            }
        }
        Err(error) => (
            Check::new("db", "数据库", Level::Error, format!("打开失败：{error}")),
            None,
        ),
    }
}

fn check_instance(dirs: &DataDirs) -> Check {
    if !dirs.root().exists() {
        return Check::new(
            "instance",
            "单实例锁",
            Level::Warn,
            "数据目录尚未创建，无法检查",
        );
    }
    let started_at = SystemClock.now();
    match InstanceLock::acquire(
        dirs,
        started_at,
        concat!("egctl ", env!("CARGO_PKG_VERSION")),
    ) {
        Ok(lock) => {
            let path = lock.path().display().to_string();
            drop(lock);
            Check::new(
                "instance",
                "单实例锁",
                Level::Ok,
                format!("无其他实例持锁（{}；已释放本检查临时锁）", path),
            )
        }
        Err(error) => Check::new(
            "instance",
            "单实例锁",
            Level::Warn,
            format!("锁被占用（桌面端可能在运行）：{error}"),
        ),
    }
}

fn check_audit(db: Option<&Arc<Database>>, clock: &eg_platform::SharedClock) -> Check {
    let Some(db) = db else {
        return Check::new(
            "audit",
            "审计链",
            Level::Warn,
            "数据库不可用，跳过审计链校验",
        );
    };
    let ids = Arc::new(UlidFactory::new(clock.clone()));
    let log = AuditLog::new(db.clone(), clock.clone(), ids);
    match log.verify_chain() {
        Ok(verification) if verification.ok => Check::new(
            "audit",
            "审计链",
            Level::Ok,
            format!("哈希链完整（{} 条）", verification.entries),
        ),
        Ok(verification) => Check::new(
            "audit",
            "审计链",
            Level::Error,
            format!(
                "哈希链在第 {} 条断裂：{}",
                verification.entries,
                verification.broken.map_or_else(
                    || "未知原因".to_owned(),
                    |(id, reason)| format!("{id}：{reason}")
                )
            ),
        ),
        Err(error) => Check::new(
            "audit",
            "审计链",
            Level::Error,
            format!("校验失败：{error}"),
        ),
    }
}

fn check_assets(db: Option<&Arc<Database>>, dirs: &DataDirs) -> Check {
    let assets_dir = dirs.assets_dir();
    let (files, bytes) = dir_stats(&assets_dir);
    let mut detail = format!(
        "CAS 目录 {}；{} 个文件（{}）",
        assets_dir.display(),
        files,
        human_bytes(bytes)
    );
    let mut level = Level::Ok;
    if !assets_dir.exists() {
        detail.push_str("；尚未创建（导入首份答卷前为空）");
        level = Level::Warn;
    } else if !dir_writable(&assets_dir) {
        detail.push_str("；不可写");
        level = Level::Error;
    }
    if let Some(db) = db
        && let Ok((count, asset_bytes)) = db.read(repo::asset_usage)
    {
        detail.push_str(&format!(
            "；DB 登记 {count} 个资产（{}）",
            human_bytes(asset_bytes)
        ));
    }
    Check::new("cas", "资产库（CAS）", level, detail)
}

fn check_logs(dirs: &DataDirs, config: &AppConfig) -> Check {
    let log_dir = dirs.log_dir();
    if !log_dir.exists() {
        return Check::new(
            "logs",
            "日志",
            Level::Warn,
            format!("日志目录尚未创建：{}", log_dir.display()),
        );
    }
    let (files, bytes) = dir_stats(log_dir);
    let writable = dir_writable(log_dir);
    let latest = latest_file(log_dir);
    let detail = format!(
        "{}；{files} 个文件（{}）；可写 {}；最新 {}；保留 {} 天",
        log_dir.display(),
        human_bytes(bytes),
        if writable { "是" } else { "否" },
        latest.map_or_else(
            || "无".to_owned(),
            |path| format!(
                "{} @ {}",
                path.file_name()
                    .map_or_else(String::new, |name| name.to_string_lossy().to_string()),
                modified_text(&path)
            )
        ),
        config.log.retention_days
    );
    let level = if writable { Level::Ok } else { Level::Error };
    Check::new("logs", "日志", level, detail)
}

fn latest_file(dir: &Path) -> Option<std::path::PathBuf> {
    let mut best: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|meta| meta.modified()) else {
            continue;
        };
        if best.as_ref().is_none_or(|(time, _)| modified > *time) {
            best = Some((modified, path));
        }
    }
    best.map(|(_, path)| path)
}

fn check_models(dirs: &DataDirs, config: &AppConfig, max_sha_mib: u64) -> Check {
    let report = registry::inspect(
        &config.inference.registry_path,
        dirs.root(),
        &dirs.models_dir(),
        max_sha_mib,
    );
    if report.path.is_none() {
        return Check::new(
            "models",
            "模型注册表",
            Level::Warn,
            report.failures.join("；"),
        );
    }
    let mut detail = format!(
        "注册表 {}（{}）；schema {}；模型 {}（本地 {} / 云端 {}）；{}；全量校验 {} 个、大文件采样 {} 个",
        report
            .path
            .as_ref()
            .map_or_else(String::new, |path| path.display().to_string()),
        report.source,
        report.schema_version,
        report.total,
        report.local,
        report.cloud,
        registry::installed_text(&report),
        report.hashed_files,
        report.sampled_files
    );
    if !report.failures.is_empty() {
        detail.push_str(&format!("；失败：{}", report.failures.join("；")));
        return Check::new("models", "模型注册表", Level::Error, detail);
    }
    if report.local > report.installed {
        detail.push_str(&format!(
            "；{} 个本地模型未安装（首次运行需 scripts/model/pull.sh 下载）",
            report.local.saturating_sub(report.installed)
        ));
        if let Some(free) = registry::models_disk_free(&dirs.models_dir()) {
            detail.push_str(&format!("；模型目录可用空间 {free} MiB"));
        }
        return Check::new("models", "模型注册表", Level::Warn, detail);
    }
    Check::new("models", "模型注册表", Level::Ok, detail)
}

fn check_llama_cpp(dirs: &DataDirs, config: &AppConfig) -> Check {
    let binary = registry::declared_binary(&config.inference.registry_path, dirs.root())
        .unwrap_or_else(|| "llama-server".to_owned());
    let mut detail = String::new();
    let mut level = Level::Ok;
    match find_in_path(&binary) {
        Some(path) => {
            detail.push_str(&format!("{} 已找到（{}）", binary, path.display()));
            if let Some(output) = run_command(&binary, &["--version"]) {
                let version = if output.ok {
                    output.stdout.lines().next().unwrap_or_default().to_owned()
                } else {
                    truncate(&output.stderr, 120)
                };
                if !version.is_empty() {
                    detail.push_str(&format!("；版本/输出：{version}"));
                }
            }
        }
        None => {
            detail.push_str(&format!(
                "{binary} 不在 PATH（本地推理不可用；需安装 llama.cpp 或改为云端授权）"
            ));
            level = Level::Warn;
        }
    }
    let ports = registry::declared_ports(&config.inference.registry_path, dirs.root());
    if !ports.is_empty() {
        let mut occupancy = Vec::new();
        for (model, port) in &ports {
            let addr = format!("127.0.0.1:{port}");
            let occupied = addr
                .to_socket_addrs()
                .ok()
                .and_then(|mut addrs| addrs.next())
                .is_some_and(|socket| {
                    TcpStream::connect_timeout(&socket, Duration::from_millis(150)).is_ok()
                });
            occupancy.push(format!(
                "{model}:{port} {}",
                if occupied { "占用" } else { "空闲" }
            ));
        }
        detail.push_str(&format!("；注册表端口：{}", occupancy.join("、")));
    }
    Check::new("llama_cpp", "llama.cpp", level, detail)
}

fn check_python_uv() -> Check {
    let python = run_command("python3", &["--version"]);
    let uv = run_command("uv", &["--version"]);
    let mut detail = String::new();
    let mut level = Level::Ok;
    match &python {
        Some(output) if output.ok => {
            detail.push_str(&format!("python3 {}", truncate(&output.stdout, 60)));
        }
        _ => {
            detail.push_str("未找到 python3（CAS/工具链脚本不可用）");
            level = Level::Warn;
        }
    }
    match &uv {
        Some(output) if output.ok => {
            detail.push_str(&format!("；uv {}", truncate(&output.stdout, 60)));
        }
        _ => {
            detail.push_str("；未找到 uv（工具链脚本不可用）");
            level = Level::Warn;
        }
    }
    Check::new("python_uv", "Python / uv", level, detail)
}

fn check_report_self_test() -> Check {
    match crate::fixture::report_self_test() {
        Ok(summary) => Check::new(
            "report",
            "报表模块自检",
            Level::Ok,
            format!("eg-report 已接入：构建 + 双次导出 + 重放摘要一致（{summary}）"),
        ),
        Err(error) => Check::new(
            "report",
            "报表模块自检",
            Level::Error,
            format!("eg-report 自检失败：{error}"),
        ),
    }
}

fn check_modules(deep: bool) -> Vec<Check> {
    let mut checks = vec![Check::new(
        "module_domain",
        "模块就绪：eg-domain / eg-platform",
        Level::Ok,
        "已链接（本 CLI 依赖并通过编译/测试）",
    )];
    for (crate_name, description) in PENDING_MODULES {
        if crate_name == "eg-app" && !deep {
            checks.push(match crate::bench::app_binary() {
                Some(path) => Check::new(
                    "module_eg_app",
                    "模块就绪：eg-app",
                    Level::Warn,
                    format!(
                        "桌面端已构建（{}），egctl 仅经 `bench` 调用 `--bench`；UI 子命令/页面验收未集成",
                        path.display()
                    ),
                ),
                None => Check::new(
                    "module_eg_app",
                    "模块就绪：eg-app",
                    Level::Warn,
                    "Slint 桌面应用：未构建（无 target/{debug,release}/eg-app）；未接入 egctl",
                ),
            });
            continue;
        }
        if deep {
            checks.push(deep_module_check(crate_name, description));
        } else {
            checks.push(Check::new(
                format!("module_{}", crate_name.replace('-', "_")),
                format!("模块就绪：{crate_name}"),
                Level::Warn,
                format!(
                    "{description}：未接入 egctl（并行开发中）；用 `egctl doctor --deep` 检查编译状态"
                ),
            ));
        }
    }
    checks
}

fn deep_module_check(crate_name: &str, description: &str) -> Check {
    let root = repo_root();
    let output = std::process::Command::new("cargo")
        .args(["check", "--offline", "-q", "-p", crate_name])
        .current_dir(&root)
        .output();
    match output {
        Ok(output) if output.status.success() => Check::new(
            format!("module_{}", crate_name.replace('-', "_")),
            format!("模块就绪：{crate_name}"),
            Level::Ok,
            format!("{description}：cargo check 通过（尚未接入 egctl 子命令）"),
        ),
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Check::new(
                format!("module_{}", crate_name.replace('-', "_")),
                format!("模块就绪：{crate_name}"),
                Level::Warn,
                format!(
                    "{description}：cargo check 失败（并行收尾） — {}",
                    truncate(&stderr, 240)
                ),
            )
        }
        Err(error) => Check::new(
            format!("module_{}", crate_name.replace('-', "_")),
            format!("模块就绪：{crate_name}"),
            Level::Warn,
            format!("无法执行 cargo（{error}）"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deviations_detects_non_default() {
        let mut config = AppConfig::default();
        assert!(deviations(&config).is_empty());
        config.fast.sampling_rate = 0.5;
        assert_eq!(deviations(&config), vec!["fast".to_owned()]);
    }
}
