//! 模型注册表轻量只读校验（`SPEC-inference.md` §3；`scripts/model/verify.sh` 的 Rust 只读版）。
//!
//! 为什么不用 `eg-inference::ModelRegistry`：本任务执行期间该 crate 未通过编译
//! （平台 `ConsentState::Exhausted` 未匹配，属并行收尾的临时状态），本 crate 不链接它；
//! 这里只解析注册表所需字段（id/角色/后端/权重/许可），不做许可策略判定、不启动模型。
//! TODO：`eg-inference` 恢复编译后改为 `ModelRegistry::load` + `license_violations`。

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::util::{file_size, free_disk_mib, human_bytes, sample_sha256, sha256_file, truncate};

/// 注册表文档（只取本工具需要的字段）。
#[derive(Debug, Deserialize)]
struct RegistryDoc {
    #[serde(default)]
    schema_version: u32,
    #[serde(default)]
    models: Vec<ModelEntry>,
}

/// 单个模型条目。
#[derive(Debug, Deserialize)]
struct ModelEntry {
    id: String,
    #[serde(default)]
    backend: String,
    #[serde(default)]
    license: String,
    #[serde(default)]
    local: Option<LocalSpec>,
    #[serde(default)]
    weights: Vec<WeightEntry>,
}

/// 本地服务参数。
#[derive(Debug, Deserialize)]
struct LocalSpec {
    #[serde(default)]
    binary: Option<String>,
    #[serde(default)]
    port: Option<u16>,
}

/// 权重文件条目。
#[derive(Debug, Deserialize)]
struct WeightEntry {
    #[serde(default)]
    component: String,
    file: String,
    #[serde(default)]
    sha256: String,
    #[serde(default)]
    bytes: Option<u64>,
}

/// 注册表校验报告。
#[derive(Debug, Default)]
pub struct RegistryReport {
    /// 实际使用的注册表路径。
    pub path: Option<PathBuf>,
    /// 来源说明（配置 / 仓库开发回退 / 未找到）。
    pub source: String,
    /// 注册表 schema 版本。
    pub schema_version: u32,
    /// 模型总数。
    pub total: usize,
    /// 本地模型数。
    pub local: usize,
    /// 云端模型数。
    pub cloud: usize,
    /// 已安装（权重目录存在）的本地模型数。
    pub installed: usize,
    /// 全量 SHA-256 已校验的权重文件数。
    pub hashed_files: usize,
    /// 因超过采样阈值只校验了大小/采样指纹的文件数。
    pub sampled_files: usize,
    /// 失败项（缺文件/大小不符/哈希不符/许可缺失）。
    pub failures: Vec<String>,
    /// 每个已安装本地模型的权重目录字节数。
    pub installed_bytes: u64,
}

/// 定位注册表：配置路径优先，其次仓库内嵌（开发环境）。
#[must_use]
pub fn locate_registry(configured: &Path, data_root: &Path) -> Option<(PathBuf, &'static str)> {
    let configured_abs = if configured.is_absolute() {
        configured.to_path_buf()
    } else {
        data_root.join(configured)
    };
    if configured_abs.is_file() {
        return Some((configured_abs, "配置路径"));
    }
    let repo_fallback =
        crate::util::repo_root().join("crates/eg-inference/assets/models.registry.toml");
    if repo_fallback.is_file() {
        return Some((repo_fallback, "仓库内嵌（开发环境回退）"));
    }
    None
}

/// 校验注册表与已安装权重。
///
/// `max_sha_mib`：单文件全量 SHA-256 的阈值（超过则只校验大小并记录采样指纹）。
#[must_use]
pub fn inspect(
    configured: &Path,
    data_root: &Path,
    models_dir: &Path,
    max_sha_mib: u64,
) -> RegistryReport {
    let mut report = RegistryReport::default();
    let Some((path, source)) = locate_registry(configured, data_root) else {
        report.source = "未找到".to_owned();
        report.failures.push(format!(
            "未找到模型注册表（配置 {}；仓库内也无内嵌副本）",
            configured.display()
        ));
        return report;
    };
    report.path = Some(path.clone());
    report.source = source.to_owned();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            report.failures.push(format!("读取注册表失败：{error}"));
            return report;
        }
    };
    let doc: RegistryDoc = match toml::from_str(&text) {
        Ok(doc) => doc,
        Err(error) => {
            report.failures.push(format!("解析注册表失败：{error}"));
            return report;
        }
    };
    report.schema_version = doc.schema_version;
    report.total = doc.models.len();
    let max_bytes = max_sha_mib.saturating_mul(1024 * 1024);
    for entry in &doc.models {
        if entry.license.trim().is_empty() {
            report
                .failures
                .push(format!("模型 {} 缺少许可声明", entry.id));
        }
        let is_local = entry.local.is_some() || entry.backend.contains("local");
        if !is_local {
            report.cloud = report.cloud.saturating_add(1);
            continue;
        }
        report.local = report.local.saturating_add(1);
        let unit_dir = models_dir.join(&entry.id);
        if !unit_dir.is_dir() {
            // 未安装不是失败：首次运行前需下载权重。
            continue;
        }
        report.installed = report.installed.saturating_add(1);
        for weight in &entry.weights {
            let weight_path = unit_dir.join(&weight.file);
            let Some(size) = file_size(&weight_path) else {
                report.failures.push(format!(
                    "{} 缺少权重文件 {}（{}）",
                    entry.id, weight.file, weight.component
                ));
                continue;
            };
            report.installed_bytes = report.installed_bytes.saturating_add(size);
            if let Some(expected) = weight.bytes
                && expected != size
            {
                report.failures.push(format!(
                    "{} {} 大小不符：期望 {expected} 字节，实际 {size} 字节",
                    entry.id, weight.file
                ));
                continue;
            }
            if weight.sha256.trim().is_empty() {
                report
                    .failures
                    .push(format!("{} {} 注册表缺少 sha256", entry.id, weight.file));
                continue;
            }
            if size <= max_bytes {
                match sha256_file(&weight_path) {
                    Ok(digest) if digest.eq_ignore_ascii_case(&weight.sha256) => {
                        report.hashed_files = report.hashed_files.saturating_add(1);
                    }
                    Ok(digest) => report.failures.push(format!(
                        "{} {} SHA-256 不符：期望 {}，实际 {}",
                        entry.id,
                        weight.file,
                        truncate(&weight.sha256, 16),
                        truncate(&digest, 16)
                    )),
                    Err(error) => report.failures.push(error),
                }
            } else {
                // 大文件：只校验大小 + 记录首尾采样指纹（不冒充全量哈希）。
                match sample_sha256(&weight_path, 8 * 1024 * 1024) {
                    Ok(_) => report.sampled_files = report.sampled_files.saturating_add(1),
                    Err(error) => report.failures.push(error),
                }
            }
        }
    }
    report
}

/// 注册表声明的本地端口（用于 `doctor` 端口占用检查）。
#[must_use]
pub fn declared_ports(configured: &Path, data_root: &Path) -> Vec<(String, u16)> {
    let Some((path, _)) = locate_registry(configured, data_root) else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(doc) = toml::from_str::<RegistryDoc>(&text) else {
        return Vec::new();
    };
    let mut ports = Vec::new();
    for entry in doc.models {
        if let Some(local) = entry.local
            && let Some(port) = local.port
        {
            ports.push((entry.id, port));
        }
    }
    ports
}

/// 注册表声明的本地二进制名（默认 `llama-server`）。
#[must_use]
pub fn declared_binary(configured: &Path, data_root: &Path) -> Option<String> {
    let (path, _) = locate_registry(configured, data_root)?;
    let text = std::fs::read_to_string(path).ok()?;
    let doc = toml::from_str::<RegistryDoc>(&text).ok()?;
    for entry in doc.models {
        if let Some(local) = entry.local
            && let Some(binary) = local.binary
            && !binary.trim().is_empty()
        {
            return Some(binary);
        }
    }
    None
}

/// 磁盘可用空间（MiB），供 doctor 展示模型目录余量。
#[must_use]
pub fn models_disk_free(models_dir: &Path) -> Option<u64> {
    if models_dir.exists() {
        free_disk_mib(models_dir)
    } else {
        models_dir.parent().and_then(free_disk_mib)
    }
}

/// 已安装模型总字节数的展示文本。
#[must_use]
pub fn installed_text(report: &RegistryReport) -> String {
    format!(
        "{} 已安装 / {} 本地（{} 权重）",
        report.installed,
        report.local,
        human_bytes(report.installed_bytes)
    )
}
