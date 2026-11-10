//! 模型注册表（`SPEC-inference.md` §3.2）：随发行版发布的只读清单。
//!
//! 职责：
//! - 解析并**严格校验**注册表（未知字段即失败、字段范围校验、serve_unit 一致性）；
//! - 许可门禁在加载前执行（fail-closed，命中排除清单必须有评审引用）；
//! - 提供按 id / 角色解析条目的唯一入口：**注册表外配置启动即失败，不静默回退**；
//! - 由条目导出 [`eg_domain::EngineStamp`]（模型 id/版本/量化/参数哈希）。
//!
//! 注册表数据文件：`assets/models.registry.toml`（与 `scripts/model/registry.tsv`、
//! `profiles.tsv` 同源，哈希与端口一致；见 `docs/dev/research/models.md`）。

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use eg_domain::{BackendId, EngineStamp, ModelRole, Sha256Hex};
use serde::Deserialize;

use crate::error::InferenceError;
use crate::license::{LicensePolicy, LicenseVerdict, LicenseViolation};

/// 内嵌的默认注册表（随发行版发布）。
pub const DEFAULT_REGISTRY_TOML: &str = include_str!("../assets/models.registry.toml");

/// 注册表 schema 版本（与数据文件的 `schema_version` 对应）。
pub const REGISTRY_SCHEMA_VERSION: u32 = 1;

/// 本地推理实现类型（`SPEC-inference.md` §3.2「实现类型」）。
///
/// v1 默认且仅默认 `llama.cpp`；`MLX` 为显式可选实现，必须独立登记并单独验收。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Implementation {
    /// llama.cpp（brew 0.8.0，OpenAI 兼容，默认）。
    LlamaCpp,
    /// MLX（mlx-vlm；显式可选，需单独验收）。
    Mlx,
}

impl Implementation {
    /// 规范文本标识。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LlamaCpp => "llama.cpp",
            Self::Mlx => "mlx",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "llama.cpp" | "llama_cpp" | "llamacpp" => Some(Self::LlamaCpp),
            "mlx" | "mlx-vlm" => Some(Self::Mlx),
            _ => None,
        }
    }
}

/// 一个权重文件（含哈希与字节数，加载前校验）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeightsFile {
    /// 组件名：`model`、`mmproj` 等。
    pub component: String,
    /// 文件在模型目录内的文件名。
    pub file: String,
    /// 文件 SHA-256。
    pub sha256: Sha256Hex,
    /// 文件字节数（先校验大小，再校验哈希）。
    pub bytes: u64,
}

/// 本地 sidecar 端点信息（`local_server`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalEndpoint {
    /// 可执行文件名（默认 `llama-server`）。
    pub binary: String,
    /// 固定端口（回环）。
    pub port: u16,
    /// 启动到 /health 就绪的超时（秒）。
    pub startup_timeout_secs: u32,
    /// 共享同一进程的权重单元名；为空时等于条目 id。
    pub serve_unit: Option<String>,
    /// 可选 chat template 文件路径（PaddleOCR 在 server 下建议传）。
    pub chat_template: Option<String>,
    /// 额外命令行参数（顺序敏感）。
    pub extra_args: Vec<String>,
}

impl LocalEndpoint {
    /// 该条目对应的服务单元名（未指定时等于条目 id）。
    #[must_use]
    pub fn serve_unit<'a>(&'a self, id: &'a str) -> &'a str {
        self.serve_unit.as_deref().unwrap_or(id)
    }
}

/// 云端定价（每百万 token 的微元；1 元 = 1_000_000 微元）。
///
/// 只用于离线费用账本；不参与任何模型行为。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct CloudPricing {
    /// 输入（缓存未命中）空闲时段单价。
    pub input_cache_miss_idle_micros: u64,
    /// 输入（缓存未命中）高峰时段单价。
    pub input_cache_miss_peak_micros: u64,
    /// 输入（缓存命中）空闲时段单价。
    pub input_cache_hit_idle_micros: u64,
    /// 输入（缓存命中）高峰时段单价。
    pub input_cache_hit_peak_micros: u64,
    /// 输出空闲时段单价。
    pub output_idle_micros: u64,
    /// 输出高峰时段单价。
    pub output_peak_micros: u64,
}

/// 云端端点信息（`openai_compat`）：**不含任何凭据**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudEndpoint {
    /// OpenAI 兼容 base URL（必须 https）。
    pub base_url: String,
    /// 模型名（请求体 `model` 字段）。
    pub model_name: String,
    /// 存放凭据的环境变量名（只存名字）。
    pub credentials_env: String,
    /// 定价表。
    pub pricing: CloudPricing,
}

/// 端点或服务单元信息（供本地后端启动进程）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeUnit {
    /// 服务单元名。
    pub unit: String,
    /// 可执行文件。
    pub binary: String,
    /// 端口。
    pub port: u16,
    /// 启动超时（秒）。
    pub startup_timeout_secs: u32,
    /// 该单元需要加载的权重（按组件排序）。
    pub weights: Vec<WeightsFile>,
    /// 额外参数。
    pub extra_args: Vec<String>,
    /// 可选 chat template。
    pub chat_template: Option<String>,
}

/// 注册表条目：一个「模型 × 角色 × 配置profile」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelEntry {
    /// 模型配置 id（注册表主键）。
    pub id: String,
    /// 模型版本（进 `EngineStamp`）。
    pub version: String,
    /// 角色。
    pub role: ModelRole,
    /// 后端 id。
    pub backend: BackendId,
    /// 实现类型（本地后端必填）。
    pub implementation: Option<Implementation>,
    /// 量化类型（如 `bf16`/`q4_k_m`/`remote`）。
    pub quantization: String,
    /// 上下文长度（token）。
    pub context_length: u32,
    /// 许可标识。
    pub license: String,
    /// 许可判定结果（加载前已过门禁）。
    pub license_verdict: LicenseVerdict,
    /// 并发上限（本模块据此排队）。
    pub max_concurrency: u16,
    /// 单次调用超时（秒）。
    pub timeout_secs: u32,
    /// 提示词 Schema 版本（进参数哈希）。
    pub prompt_schema_version: String,
    /// 中文说明（诊断包展示）。
    pub description: String,
    /// 权重文件（本地后端必填）。
    pub weights: Vec<WeightsFile>,
    /// 本地端点。
    pub local: Option<LocalEndpoint>,
    /// 云端端点。
    pub cloud: Option<CloudEndpoint>,
    /// 可选校准表路径（JSON；相对工作目录）；缺失/不匹配时保持 uncalibrated。
    pub calibration_table: Option<String>,
}

impl ModelEntry {
    /// 是否为云端后端。
    #[must_use]
    pub fn is_cloud(&self) -> bool {
        self.backend.is_cloud()
    }

    /// 是否为本地 sidecar 后端。
    #[must_use]
    pub fn is_local(&self) -> bool {
        self.backend == BackendId::local_server()
    }

    /// 权重总字节数。
    #[must_use]
    pub fn weights_total_bytes(&self) -> u64 {
        self.weights.iter().map(|weight| weight.bytes).sum()
    }

    /// 服务单元名（本地后端）。
    #[must_use]
    pub fn serve_unit(&self) -> Option<&str> {
        self.local
            .as_ref()
            .map(|local| local.serve_unit(self.id.as_str()))
    }

    /// 参数哈希：实现、量化、上下文、超时、并发、Schema 版本与端点参数的规范指纹。
    ///
    /// 用途：`EngineStamp.params_hash` 与缓存键；模型或参数一变即失效。
    #[must_use]
    pub fn params_hash(&self) -> Sha256Hex {
        let mut map = BTreeMap::new();
        map.insert("quantization", self.quantization.clone());
        map.insert("context_length", self.context_length.to_string());
        map.insert("timeout_secs", self.timeout_secs.to_string());
        map.insert("max_concurrency", self.max_concurrency.to_string());
        map.insert("prompt_schema_version", self.prompt_schema_version.clone());
        map.insert(
            "implementation",
            self.implementation.map_or_else(
                || "none".to_owned(),
                |implementation| implementation.as_str().to_owned(),
            ),
        );
        map.insert("backend", self.backend.to_string());
        if let Some(local) = &self.local {
            map.insert("binary", local.binary.clone());
            map.insert("port", local.port.to_string());
            map.insert(
                "extra_args",
                serde_json::to_string(&local.extra_args).unwrap_or_else(|_| "[]".to_owned()),
            );
            map.insert(
                "chat_template",
                local.chat_template.clone().unwrap_or_default(),
            );
        }
        if let Some(cloud) = &self.cloud {
            map.insert("base_url", cloud.base_url.clone());
            map.insert("model_name", cloud.model_name.clone());
        }
        let canonical = serde_json::to_string(&map).unwrap_or_default();
        Sha256Hex::of(canonical.as_bytes())
    }

    /// 构造引擎指纹（`EngineStamp`）。
    ///
    /// `prompt_hash` 由调用方按已渲染提示词计算（`recognize`/`grade` 渲染）。
    #[must_use]
    pub fn engine_stamp(&self, prompt_hash: Option<Sha256Hex>) -> EngineStamp {
        EngineStamp {
            role: self.role,
            backend: self.backend.clone(),
            model_id: self.id.clone(),
            model_version: self.version.clone(),
            prompt_hash,
            params_hash: self.params_hash(),
        }
    }

    /// 缓存键（`SPEC-inference.md` §4：模型切换即失效）。
    ///
    /// 由「引擎指纹 + 输入哈希」构成；同一模型版本 + 提示词版本 + 输入才可能命中。
    #[must_use]
    pub fn cache_key(&self, prompt_hash: Option<Sha256Hex>, input_hash: Sha256Hex) -> String {
        let stamp = self.engine_stamp(prompt_hash);
        let prompt = stamp
            .prompt_hash
            .map_or_else(|| "-".to_owned(), eg_domain::Sha256Hex::to_hex);
        format!(
            "{}:{}:{}:{}:{}:{}",
            self.id,
            self.version,
            self.backend,
            prompt,
            stamp.params_hash.to_hex(),
            input_hash.to_hex()
        )
    }
}

/// 模型注册表。
#[derive(Debug, Clone)]
pub struct ModelRegistry {
    entries: Vec<ModelEntry>,
    index: BTreeMap<String, usize>,
    policy: LicensePolicy,
    fingerprint: Sha256Hex,
}

impl ModelRegistry {
    /// 用默认许可策略解析注册表文本。
    ///
    /// # Errors
    ///
    /// 语法错误、字段不合法、许可违例、serve_unit 不一致时返回
    /// [`InferenceError::RegistryInvalid`]。
    pub fn parse_str(text: &str) -> Result<Self, InferenceError> {
        Self::parse_str_with_policy(text, LicensePolicy::default())
    }

    /// 用指定许可策略解析注册表文本（测试与将来调整用）。
    ///
    /// # Errors
    ///
    /// 同 [`Self::parse_str`]。
    pub fn parse_str_with_policy(
        text: &str,
        policy: LicensePolicy,
    ) -> Result<Self, InferenceError> {
        let document: RegistryDocument = toml::from_str(text)
            .map_err(|error| registry_invalid(format!("TOML 解析失败：{error}")))?;
        if document.schema_version != REGISTRY_SCHEMA_VERSION {
            return Err(registry_invalid(format!(
                "schema_version 为 {}，本程序只支持 {}",
                document.schema_version, REGISTRY_SCHEMA_VERSION
            )));
        }
        if document.models.is_empty() {
            return Err(registry_invalid("注册表至少需要一个模型条目"));
        }

        let mut entries = Vec::with_capacity(document.models.len());
        for raw in document.models {
            entries.push(raw.into_entry(&policy)?);
        }

        // 主键唯一
        let mut index = BTreeMap::new();
        for (position, entry) in entries.iter().enumerate() {
            if index.insert(entry.id.clone(), position).is_some() {
                return Err(registry_invalid(format!("模型 id 重复：{}", entry.id)));
            }
        }

        let registry = Self {
            fingerprint: fingerprint_of(&entries),
            entries,
            index,
            policy,
        };
        registry.validate_serve_units()?;
        Ok(registry)
    }

    /// 从文件加载注册表（`inference.registry_path`）。
    ///
    /// # Errors
    ///
    /// 读文件失败或内容不合法时返回错误。
    pub fn load(path: &Path) -> Result<Self, InferenceError> {
        let text = std::fs::read_to_string(path).map_err(|error| {
            registry_invalid(format!("无法读取注册表 {}：{error}", path.display()))
        })?;
        Self::parse_str(&text)
    }

    /// 内嵌的默认注册表（随发行版发布）。
    ///
    /// # Errors
    ///
    /// 内嵌数据损坏时返回错误（属编程错误，测试会覆盖）。
    pub fn embedded() -> Result<Self, InferenceError> {
        Self::parse_str(DEFAULT_REGISTRY_TOML)
    }

    /// 全部条目（注册表顺序）。
    #[must_use]
    pub fn entries(&self) -> &[ModelEntry] {
        &self.entries
    }

    /// 全部模型 id。
    #[must_use]
    pub fn ids(&self) -> Vec<&str> {
        self.entries.iter().map(|entry| entry.id.as_str()).collect()
    }

    /// 某角色的全部条目。
    #[must_use]
    pub fn by_role(&self, role: ModelRole) -> Vec<&ModelEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.role == role)
            .collect()
    }

    /// 当前许可策略。
    #[must_use]
    pub fn policy(&self) -> &LicensePolicy {
        &self.policy
    }

    /// 注册表指纹（审计「注册变更」用）。
    #[must_use]
    pub fn fingerprint(&self) -> Sha256Hex {
        self.fingerprint
    }

    /// 按 id 取条目；未登记即失败（不静默回退默认）。
    ///
    /// # Errors
    ///
    /// 未登记时返回 [`InferenceError::RegistryMiss`]。
    pub fn require(&self, model_id: &str) -> Result<&ModelEntry, InferenceError> {
        self.index
            .get(model_id)
            .map(|position| &self.entries[*position])
            .ok_or_else(|| InferenceError::RegistryMiss {
                model_id: model_id.to_owned(),
            })
    }

    /// 按 id 取条目并校验角色。
    ///
    /// # Errors
    ///
    /// 未登记返回 [`InferenceError::RegistryMiss`]；角色不符返回
    /// [`InferenceError::RegistryInvalid`]。
    pub fn require_role(
        &self,
        model_id: &str,
        role: ModelRole,
    ) -> Result<&ModelEntry, InferenceError> {
        let entry = self.require(model_id)?;
        if entry.role != role {
            return Err(registry_invalid(format!(
                "模型 {} 的角色为 {:?}，与请求的 {:?} 不一致",
                model_id, entry.role, role
            )));
        }
        Ok(entry)
    }

    /// 解析配置键指向的模型（`ocr.model` / `grade.model_profile` 等）。
    ///
    /// 空值视为「配置未解析」，未登记视为「注册表外配置」，两者都是启动期错误。
    ///
    /// # Errors
    ///
    /// 空值 → [`InferenceError::ConfigUnresolved`]；未登记 → [`InferenceError::RegistryMiss`]；
    /// 角色不符 → [`InferenceError::RegistryInvalid`]。
    pub fn resolve_configured(
        &self,
        config_key: &str,
        configured: &str,
        role: ModelRole,
    ) -> Result<&ModelEntry, InferenceError> {
        let trimmed = configured.trim();
        if trimmed.is_empty() {
            return Err(InferenceError::ConfigUnresolved {
                key: config_key.to_owned(),
            });
        }
        self.require_role(trimmed, role)
    }

    /// 本地服务单元（本地后端启动进程用）。
    ///
    /// # Errors
    ///
    /// 条目不是本地后端或缺少本地端点时返回 [`InferenceError::RegistryInvalid`]。
    pub fn serve_unit(&self, model_id: &str) -> Result<ServeUnit, InferenceError> {
        let entry = self.require(model_id)?;
        let local = entry.local.as_ref().ok_or_else(|| {
            registry_invalid(format!("模型 {model_id} 没有本地端点，不能启动 sidecar"))
        })?;
        let unit = local.serve_unit(entry.id.as_str()).to_owned();
        let mut weights = Vec::new();
        for other in self.entries.iter().filter(|candidate| {
            candidate
                .local
                .as_ref()
                .is_some_and(|endpoint| endpoint.serve_unit(candidate.id.as_str()) == unit)
        }) {
            for weight in &other.weights {
                if !weights.iter().any(|existing: &WeightsFile| {
                    existing.component == weight.component && existing.file == weight.file
                }) {
                    weights.push(weight.clone());
                }
            }
        }
        weights.sort_by(|left, right| left.component.cmp(&right.component));
        Ok(ServeUnit {
            unit,
            binary: local.binary.clone(),
            port: local.port,
            startup_timeout_secs: local.startup_timeout_secs,
            weights,
            extra_args: local.extra_args.clone(),
            chat_template: local.chat_template.clone(),
        })
    }

    /// 许可扫描（成功构造的注册表必为空；供诊断与测试断言）。
    #[must_use]
    pub fn license_violations(&self) -> Vec<LicenseViolation> {
        self.entries
            .iter()
            .filter_map(|entry| {
                if entry.license_verdict.allowed() {
                    None
                } else {
                    Some(LicenseViolation {
                        model_id: entry.id.clone(),
                        license: entry.license.clone(),
                        verdict: entry.license_verdict,
                    })
                }
            })
            .collect()
    }

    /// 校验共享 serve_unit 的条目在进程参数上完全一致。
    fn validate_serve_units(&self) -> Result<(), InferenceError> {
        let mut seen: BTreeMap<&str, (&LocalEndpoint, &ModelEntry)> = BTreeMap::new();
        for entry in &self.entries {
            let Some(local) = entry.local.as_ref() else {
                continue;
            };
            let unit = local.serve_unit(entry.id.as_str());
            if let Some(&(first_local, first_entry)) = seen.get(unit) {
                let same_weights =
                    weight_signature(&first_entry.weights) == weight_signature(&entry.weights);
                if first_local.binary != local.binary
                    || first_local.port != local.port
                    || first_local.extra_args != local.extra_args
                    || first_local.chat_template != local.chat_template
                    || !same_weights
                {
                    return Err(registry_invalid(format!(
                        "serve_unit {unit} 的条目 {} 与 {} 的进程参数或权重不一致",
                        first_entry.id, entry.id
                    )));
                }
            } else {
                seen.insert(unit, (local, entry));
            }
        }
        Ok(())
    }
}

fn weight_signature(weights: &[WeightsFile]) -> Vec<(String, String, String)> {
    weights
        .iter()
        .map(|weight| {
            (
                weight.component.clone(),
                weight.file.clone(),
                weight.sha256.to_hex(),
            )
        })
        .collect()
}

fn fingerprint_of(entries: &[ModelEntry]) -> Sha256Hex {
    let mut parts: Vec<String> = entries
        .iter()
        .map(|entry| {
            let weights: Vec<String> = entry
                .weights
                .iter()
                .map(|weight| weight.sha256.to_hex())
                .collect();
            format!(
                "{}|{}|{:?}|{}|{}|{}",
                entry.id,
                entry.version,
                entry.role,
                entry.license,
                entry.quantization,
                weights.join(",")
            )
        })
        .collect();
    parts.sort();
    Sha256Hex::of(parts.join("\n").as_bytes())
}

fn registry_invalid(reason: impl Into<String>) -> InferenceError {
    InferenceError::RegistryInvalid {
        reason: reason.into(),
    }
}

// ───────────────────────────── 原始 TOML 结构 ─────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryDocument {
    schema_version: u32,
    models: Vec<RawEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntry {
    id: String,
    version: String,
    role: String,
    backend: String,
    implementation: Option<String>,
    quantization: String,
    context_length: u32,
    license: String,
    #[serde(default)]
    license_review: String,
    max_concurrency: u16,
    timeout_secs: u32,
    prompt_schema_version: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    weights: Vec<RawWeight>,
    local: Option<RawLocal>,
    cloud: Option<RawCloud>,
    #[serde(default)]
    calibration_table: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWeight {
    component: String,
    file: String,
    sha256: String,
    bytes: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLocal {
    binary: String,
    port: u16,
    startup_timeout_secs: u32,
    #[serde(default)]
    serve_unit: Option<String>,
    #[serde(default)]
    chat_template: Option<String>,
    #[serde(default)]
    extra_args: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCloud {
    base_url: String,
    model_name: String,
    credentials_env: String,
    input_cache_miss_idle_micros: u64,
    input_cache_miss_peak_micros: u64,
    input_cache_hit_idle_micros: u64,
    input_cache_hit_peak_micros: u64,
    output_idle_micros: u64,
    output_peak_micros: u64,
}

impl RawEntry {
    fn into_entry(self, policy: &LicensePolicy) -> Result<ModelEntry, InferenceError> {
        let id = self.id.trim().to_owned();
        validate_model_id(&id)?;
        if self.version.trim().is_empty() {
            return Err(registry_invalid(format!("模型 {id} 缺少 version")));
        }
        let role = parse_role(&self.role)?;
        let backend = parse_backend(&self.backend)?;
        let license = self.license.trim().to_owned();
        let verdict = policy.check(&license, Some(self.license_review.as_str()));
        if !verdict.allowed() {
            return Err(match verdict {
                LicenseVerdict::DeniedNeedsReview => InferenceError::LicenseReviewRequired {
                    model_id: id.clone(),
                    license,
                },
                _ => InferenceError::LicenseDenied {
                    model_id: id.clone(),
                    license,
                },
            });
        }
        if self.quantization.trim().is_empty() {
            return Err(registry_invalid(format!("模型 {id} 缺少 quantization")));
        }
        if !(512..=1_048_576).contains(&self.context_length) {
            return Err(registry_invalid(format!(
                "模型 {id} 的 context_length={} 超出 512..=1048576",
                self.context_length
            )));
        }
        if self.max_concurrency == 0 {
            return Err(registry_invalid(format!(
                "模型 {id} 的 max_concurrency 必须 ≥ 1"
            )));
        }
        validate_timeout(&id, role, backend.is_cloud(), self.timeout_secs)?;
        if self.prompt_schema_version.trim().is_empty() {
            return Err(registry_invalid(format!(
                "模型 {id} 缺少 prompt_schema_version"
            )));
        }

        let is_local = backend == BackendId::local_server();
        let implementation = match (self.implementation.as_deref(), is_local) {
            (Some(text), true) => Some(Implementation::parse(text).ok_or_else(|| {
                registry_invalid(format!(
                    "模型 {id} 的实现类型 {text} 不受支持（llama.cpp/mlx）"
                ))
            })?),
            (None, true) => {
                return Err(registry_invalid(format!(
                    "模型 {id} 使用 local_server 但缺少 implementation"
                )));
            }
            (Some(_), false) => {
                return Err(registry_invalid(format!(
                    "模型 {id} 非本地后端却声明了 implementation"
                )));
            }
            (None, false) => None,
        };

        let weights = self
            .weights
            .into_iter()
            .map(|raw| {
                let sha = Sha256Hex::from_hex(raw.sha256.trim()).map_err(|_| {
                    registry_invalid(format!(
                        "模型 {id} 的权重 {} 的 sha256 不合法（需 64 位小写十六进制）",
                        raw.file
                    ))
                })?;
                if raw.bytes == 0 {
                    return Err(registry_invalid(format!(
                        "模型 {id} 的权重 {} 字节数为 0",
                        raw.file
                    )));
                }
                if raw.component.trim().is_empty() || raw.file.trim().is_empty() {
                    return Err(registry_invalid(format!(
                        "模型 {id} 的权重缺少 component 或 file"
                    )));
                }
                Ok(WeightsFile {
                    component: raw.component.trim().to_owned(),
                    file: raw.file.trim().to_owned(),
                    sha256: sha,
                    bytes: raw.bytes,
                })
            })
            .collect::<Result<Vec<_>, InferenceError>>()?;

        if is_local {
            if weights.is_empty() {
                return Err(registry_invalid(format!("本地模型 {id} 缺少权重文件")));
            }
            if !weights.iter().any(|weight| weight.component == "model") {
                return Err(registry_invalid(format!(
                    "本地模型 {id} 缺少 component=\"model\" 的权重"
                )));
            }
            let mut components = BTreeSet::new();
            for weight in &weights {
                if !components.insert(weight.component.clone()) {
                    return Err(registry_invalid(format!(
                        "模型 {id} 的权重组件重复：{}",
                        weight.component
                    )));
                }
            }
        } else if !weights.is_empty() {
            return Err(registry_invalid(format!(
                "模型 {id} 非本地后端却声明了权重文件"
            )));
        }

        let local = self
            .local
            .map(|raw| Self::local_endpoint(&id, raw))
            .transpose()?;
        let cloud = self
            .cloud
            .map(|raw| Self::cloud_endpoint(&id, raw))
            .transpose()?;
        match (is_local, &local, &cloud) {
            (true, None, _) => {
                return Err(registry_invalid(format!(
                    "本地模型 {id} 缺少 [models.local] 端点"
                )));
            }
            (false, Some(_), _) => {
                return Err(registry_invalid(format!(
                    "模型 {id} 不是本地后端却声明了 [models.local]"
                )));
            }
            _ => {}
        }
        if backend == BackendId::openai_compat() && cloud.is_none() {
            return Err(registry_invalid(format!(
                "云端模型 {id} 缺少 [models.cloud] 端点"
            )));
        }

        Ok(ModelEntry {
            id,
            version: self.version.trim().to_owned(),
            role,
            backend,
            implementation,
            quantization: self.quantization.trim().to_owned(),
            context_length: self.context_length,
            license,
            license_verdict: verdict,
            max_concurrency: self.max_concurrency,
            timeout_secs: self.timeout_secs,
            prompt_schema_version: self.prompt_schema_version.trim().to_owned(),
            description: self.description,
            weights,
            local,
            cloud,
            calibration_table: self.calibration_table,
        })
    }

    fn local_endpoint(id: &str, raw: RawLocal) -> Result<LocalEndpoint, InferenceError> {
        if raw.binary.trim().is_empty() {
            return Err(registry_invalid(format!("模型 {id} 的本地 binary 为空")));
        }
        if raw.port < 1024 {
            return Err(registry_invalid(format!(
                "模型 {id} 的端口 {} 需要 ≥1024（回环非特权端口）",
                raw.port
            )));
        }
        if !(1..=120).contains(&raw.startup_timeout_secs) {
            return Err(registry_invalid(format!(
                "模型 {id} 的 startup_timeout_secs={} 超出 1..=120",
                raw.startup_timeout_secs
            )));
        }
        Ok(LocalEndpoint {
            binary: raw.binary.trim().to_owned(),
            port: raw.port,
            startup_timeout_secs: raw.startup_timeout_secs,
            serve_unit: raw.serve_unit.map(|unit| unit.trim().to_owned()),
            chat_template: raw.chat_template.map(|path| path.trim().to_owned()),
            extra_args: raw.extra_args,
        })
    }

    fn cloud_endpoint(id: &str, raw: RawCloud) -> Result<CloudEndpoint, InferenceError> {
        if !raw.base_url.trim().starts_with("https://") {
            return Err(registry_invalid(format!(
                "云端模型 {id} 的 base_url 必须以 https:// 开头（明文不接受）"
            )));
        }
        if raw.model_name.trim().is_empty() {
            return Err(registry_invalid(format!("云端模型 {id} 缺少 model_name")));
        }
        validate_credentials_env(id, &raw.credentials_env)?;
        Ok(CloudEndpoint {
            base_url: raw.base_url.trim().trim_end_matches('/').to_owned(),
            model_name: raw.model_name.trim().to_owned(),
            credentials_env: raw.credentials_env.trim().to_owned(),
            pricing: CloudPricing {
                input_cache_miss_idle_micros: raw.input_cache_miss_idle_micros,
                input_cache_miss_peak_micros: raw.input_cache_miss_peak_micros,
                input_cache_hit_idle_micros: raw.input_cache_hit_idle_micros,
                input_cache_hit_peak_micros: raw.input_cache_hit_peak_micros,
                output_idle_micros: raw.output_idle_micros,
                output_peak_micros: raw.output_peak_micros,
            },
        })
    }
}

/// 凭据字段只允许是**环境变量名**，绝不能写入实际密钥（`SPEC-inference.md` §8 Never）。
fn validate_credentials_env(id: &str, value: &str) -> Result<(), InferenceError> {
    let value = value.trim();
    let shape_ok = !value.is_empty()
        && value.len() <= 64
        && value.chars().all(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
        })
        && value.starts_with(|character: char| character.is_ascii_uppercase());
    if !shape_ok {
        return Err(registry_invalid(format!(
            "云端模型 {id} 的 credentials_env 必须是环境变量名（大写字母/数字/下划线），不得写入实际密钥"
        )));
    }
    Ok(())
}

fn validate_model_id(id: &str) -> Result<(), InferenceError> {
    let ok = !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || character == '.'
                || character == '-'
        });
    if ok {
        Ok(())
    } else {
        Err(registry_invalid(format!(
            "模型 id {id:?} 不合法（允许小写字母/数字/点/连字符，长度 1..=64）"
        )))
    }
}

fn parse_role(text: &str) -> Result<ModelRole, InferenceError> {
    match text.trim() {
        "ocr" => Ok(ModelRole::Ocr),
        "router" => Ok(ModelRole::Router),
        "judge" => Ok(ModelRole::Judge),
        "structurer" => Ok(ModelRole::Structurer),
        other => Err(registry_invalid(format!(
            "未知角色 {other:?}（允许 ocr/router/judge/structurer）"
        ))),
    }
}

fn parse_backend(text: &str) -> Result<BackendId, InferenceError> {
    match text.trim() {
        "local_server" => Ok(BackendId::local_server()),
        "openai_compat" => Ok(BackendId::openai_compat()),
        "fixture" => Ok(BackendId::fixture()),
        other => Err(registry_invalid(format!(
            "未知后端 {other:?}（允许 local_server/openai_compat/fixture）"
        ))),
    }
}

/// 超时范围按角色区分（`CONFIG.md` §4/§5：ocr 10–180s、judge 5–60s）。
fn validate_timeout(
    id: &str,
    role: ModelRole,
    is_cloud: bool,
    timeout_secs: u32,
) -> Result<(), InferenceError> {
    // 本地：严格按角色（CONFIG.md §4/§5 的 10–180s / 5–60s）。
    // 云端：单次往返不受本地交互时延约束，放宽到 1–300s（见 config/cloud.deepseek.toml）。
    let (low, high) = if is_cloud { (1, 300) } else { (5, 180) };
    let role_ok = is_cloud
        || match role {
            ModelRole::Ocr => (10..=180).contains(&timeout_secs),
            ModelRole::Judge | ModelRole::Router => (5..=60).contains(&timeout_secs),
            ModelRole::Structurer => (5..=120).contains(&timeout_secs),
        };
    if !(low..=high).contains(&timeout_secs) || !role_ok {
        return Err(registry_invalid(format!(
            "模型 {id} 的 timeout_secs={timeout_secs} 超出角色 {role:?} 允许范围"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn embedded() -> ModelRegistry {
        ModelRegistry::embedded().expect("内嵌注册表必须合法")
    }

    #[test]
    fn embedded_registry_covers_all_roles_and_has_no_license_violation() {
        let registry = embedded();
        assert_eq!(registry.license_violations(), Vec::new());
        for role in [
            ModelRole::Ocr,
            ModelRole::Router,
            ModelRole::Judge,
            ModelRole::Structurer,
        ] {
            assert!(
                !registry.by_role(role).is_empty(),
                "角色 {role:?} 在默认注册表中必须有条目"
            );
        }
        let ocr = registry.by_role(ModelRole::Ocr);
        assert!(ocr.iter().any(|entry| entry.id == "paddleocr-vl-1.6"));
    }

    #[test]
    fn default_ocr_entry_matches_measured_weights() {
        let registry = embedded();
        let entry = registry
            .resolve_configured("ocr.model", "paddleocr-vl-1.6", ModelRole::Ocr)
            .unwrap();
        assert_eq!(entry.implementation, Some(Implementation::LlamaCpp));
        assert_eq!(entry.quantization, "bf16");
        assert_eq!(entry.context_length, 8_192);
        assert_eq!(entry.weights_total_bytes(), 935_769_056 + 881_770_560);
        let local = entry.local.as_ref().unwrap();
        assert_eq!(local.port, 8081);
        assert_eq!(local.binary, "llama-server");
    }

    #[test]
    fn judge_entry_disables_qwen_thinking() {
        let registry = embedded();
        let entry = registry
            .resolve_configured(
                "grade.model_profile",
                "qwen3.5-4b-instruct",
                ModelRole::Judge,
            )
            .unwrap();
        let local = entry.local.as_ref().unwrap();
        let joined = local.extra_args.join(" ");
        assert!(
            joined.contains("enable_thinking"),
            "Qwen3 判定条目必须强制关闭 thinking，实际参数：{joined}"
        );
    }

    #[test]
    fn unknown_configured_model_fails_instead_of_falling_back() {
        let registry = embedded();
        let error = registry
            .resolve_configured("ocr.model", "not-in-registry", ModelRole::Ocr)
            .unwrap_err();
        assert!(matches!(&error, InferenceError::RegistryMiss { .. }));
        assert_eq!(error.class(), crate::error::FailureClass::Registry);

        let empty = registry
            .resolve_configured("ocr.model", "   ", ModelRole::Ocr)
            .unwrap_err();
        assert!(matches!(empty, InferenceError::ConfigUnresolved { .. }));
    }

    #[test]
    fn role_mismatch_is_rejected() {
        let registry = embedded();
        let error = registry
            .require_role("paddleocr-vl-1.6", ModelRole::Judge)
            .unwrap_err();
        assert!(matches!(error, InferenceError::RegistryInvalid { .. }));
    }

    #[test]
    fn router_and_structurer_share_the_judge_serve_unit() {
        let registry = embedded();
        let judge = registry.serve_unit("qwen3.5-4b-instruct").unwrap();
        let router = registry.serve_unit("qwen3.5-4b-router").unwrap();
        let structurer = registry.serve_unit("qwen3.5-4b-structurer").unwrap();
        assert_eq!(judge.unit, router.unit);
        assert_eq!(judge.unit, structurer.unit);
        assert_eq!(judge.port, router.port);
        assert_eq!(judge.weights, router.weights);
    }

    #[test]
    fn engine_stamp_changes_when_model_switches() {
        let registry = embedded();
        let ocr = registry.require("paddleocr-vl-1.6").unwrap();
        let other = registry.require("glm-ocr").unwrap();
        let prompt = Sha256Hex::of(b"ocr prompt v1");
        let input = Sha256Hex::of(b"page bytes");
        let stamp_a = ocr.engine_stamp(Some(prompt));
        let stamp_b = other.engine_stamp(Some(prompt));
        assert_ne!(stamp_a.params_hash, stamp_b.params_hash);
        assert_ne!(
            ocr.cache_key(Some(prompt), input),
            other.cache_key(Some(prompt), input),
            "切换模型后缓存键必须变化"
        );
        assert_eq!(stamp_a.role, ModelRole::Ocr);
    }

    #[test]
    fn license_violation_prevents_registry_load() {
        let text = registry_toml_with_license("CC-BY-NC-4.0", None);
        let error = ModelRegistry::parse_str(&text).unwrap_err();
        assert!(matches!(
            error,
            InferenceError::LicenseReviewRequired { .. }
        ));
    }

    #[test]
    fn excluded_license_with_review_reference_loads() {
        let text = registry_toml_with_license("CC-BY-NC-4.0", Some("docs/reviews/rev-001.md"));
        let registry = ModelRegistry::parse_str(&text).unwrap();
        assert_eq!(
            registry.require("test-ocr").unwrap().license_verdict,
            LicenseVerdict::AllowedWithReview
        );
    }

    #[test]
    fn unknown_license_prevents_registry_load_even_with_review() {
        let text = registry_toml_with_license("Proprietary", Some("docs/reviews/rev-001.md"));
        let error = ModelRegistry::parse_str(&text).unwrap_err();
        assert!(matches!(error, InferenceError::LicenseDenied { .. }));
    }

    #[test]
    fn malformed_registry_fields_are_rejected() {
        let cases = [
            // 坏 sha256
            registry_toml(|text| text.replace(A64, "zz")),
            // 未知字段（deny_unknown_fields）
            registry_toml(|text| {
                text.replace("max_concurrency = 1", "max_concurrency = 1\nsurprise = 2")
            }),
            // 本地模型缺权重
            registry_toml(|text| {
                text.replace(
                    "[[models.weights]]\ncomponent = \"model\"\nfile = \"model.gguf\"\nsha256 = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\nbytes = 1024\n",
                    "",
                )
            }),
            // 端口 <1024
            registry_toml(|text| text.replace("port = 9001", "port = 80")),
            // 上下文过小
            registry_toml(|text| text.replace("context_length = 8192", "context_length = 64")),
            // 角色未知
            registry_toml(|text| text.replace("role = \"ocr\"", "role = \"oracle\"")),
        ];
        for text in cases {
            let error = ModelRegistry::parse_str(&text).unwrap_err();
            assert!(
                matches!(error, InferenceError::RegistryInvalid { .. }),
                "非法注册表必须被拒绝：{error}"
            );
        }
    }

    #[test]
    fn cloud_entry_rejects_plaintext_http_and_secret_like_credentials() {
        let http =
            registry_toml(|text| text.replace("https://api.example.com", "http://api.example.com"));
        assert!(matches!(
            ModelRegistry::parse_str(&http).unwrap_err(),
            InferenceError::RegistryInvalid { .. }
        ));

        let secret = registry_toml(|text| text.replace("TEST_API_KEY", "sk-live-abcdef123456"));
        let error = ModelRegistry::parse_str(&secret).unwrap_err();
        assert!(
            error.to_string().contains("credentials_env"),
            "凭据字段必须是环境变量名：{error}"
        );
    }

    #[test]
    fn serve_unit_inconsistency_is_rejected() {
        let text = registry_toml(|text| text);
        // 第二个条目共享 serve_unit 但端口不同 → 必须拒绝
        let conflicted = format!(
            "{text}\n{}",
            r#"[[models]]
id = "shadow-ocr"
version = "1"
role = "ocr"
backend = "local_server"
implementation = "llama.cpp"
quantization = "q4_k_m"
context_length = 8192
license = "MIT"
max_concurrency = 1
timeout_secs = 60
prompt_schema_version = "ocr.blocks.v1"

[models.local]
binary = "llama-server"
port = 9002
startup_timeout_secs = 12
serve_unit = "test-ocr"
extra_args = []

[[models.weights]]
component = "model"
file = "model.gguf"
sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
bytes = 1024
"#
        );
        let error = ModelRegistry::parse_str(&conflicted).unwrap_err();
        assert!(matches!(error, InferenceError::RegistryInvalid { .. }));
        assert!(error.to_string().contains("serve_unit"));
    }

    #[test]
    fn registry_without_entries_is_rejected() {
        let text = "schema_version = 1\nmodels = []\n";
        assert!(matches!(
            ModelRegistry::parse_str(text).unwrap_err(),
            InferenceError::RegistryInvalid { .. }
        ));
    }

    #[test]
    fn schema_version_mismatch_is_rejected() {
        let text = "schema_version = 99\nmodels = []\n";
        assert!(ModelRegistry::parse_str(text).is_err());
    }

    #[test]
    fn fingerprint_is_stable_and_changes_with_version() {
        let first = embedded().fingerprint();
        let second = embedded().fingerprint();
        assert_eq!(first, second);
        let text = registry_toml(|text| text.replace("version = \"1.6\"", "version = \"1.7\""));
        let changed = ModelRegistry::parse_str(&text).unwrap();
        assert_ne!(first, changed.fingerprint());
    }

    // ── 测试夹具：最小合法注册表（本地 + 可选云端） ──

    const A64: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn registry_toml(edit: impl Fn(String) -> String) -> String {
        let base = r#"schema_version = 1

[[models]]
id = "test-ocr"
version = "1.6"
role = "ocr"
backend = "local_server"
implementation = "llama.cpp"
quantization = "bf16"
context_length = 8192
license = "MIT"
max_concurrency = 1
timeout_secs = 60
prompt_schema_version = "ocr.blocks.v1"

[models.local]
binary = "llama-server"
port = 9001
startup_timeout_secs = 12
serve_unit = "test-ocr"
extra_args = []

[[models.weights]]
component = "model"
file = "model.gguf"
sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
bytes = 1024

[[models.weights]]
component = "mmproj"
file = "mmproj.gguf"
sha256 = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
bytes = 2048

[[models]]
id = "test-cloud"
version = "1"
role = "judge"
backend = "openai_compat"
quantization = "remote"
context_length = 65536
license = "MIT"
max_concurrency = 2
timeout_secs = 120
prompt_schema_version = "judge.outcome.v1"

[models.cloud]
base_url = "https://api.example.com"
model_name = "test-model"
credentials_env = "TEST_API_KEY"
input_cache_miss_idle_micros = 1000000
input_cache_miss_peak_micros = 2000000
input_cache_hit_idle_micros = 20000
input_cache_hit_peak_micros = 40000
output_idle_micros = 4000000
output_peak_micros = 8000000
"#;
        edit(base.to_owned())
    }

    fn registry_toml_with_license(license: &str, review: Option<&str>) -> String {
        registry_toml(|text| {
            // 只改本地条目（第一个 license 行），云端条目保持 MIT
            let text = text.replacen("license = \"MIT\"", &format!("license = \"{license}\""), 1);
            match review {
                Some(reference) => text.replace(
                    "prompt_schema_version = \"ocr.blocks.v1\"",
                    &format!("prompt_schema_version = \"ocr.blocks.v1\"\nlicense_review = \"{reference}\""),
                ),
                None => text,
            }
        })
    }
}
