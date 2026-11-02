//! CAS 适配层：sympy sidecar（`SPEC-subject-math.md` §7、T27）。
//!
//! 设计（依据 `docs/dev/research/cas.md` 的 tech-spike 实测）：
//! - **一次性子进程**：每个请求启动一个 sidecar 进程，写入一行 JSON 请求、读取一行
//!   JSON 响应后立即结束。理由：请求级隔离最简单、超时/崩溃清理无状态残留；
//!   实测启动 0.24 s、单次化简 0.43–4.5 ms，远低于 3 s 预算；常驻进程池作为
//!   后续吞吐优化（当前规模不需要，且常驻会引入重启/状态一致性风险）。
//! - **超时**：Rust 侧强制 deadline（默认 3 s，可配置 1–10 s）；超时 `kill` 子进程，
//!   该比较器返回「无法判定」，绝不降级为近似数值比较。
//! - **降级**：禁用/缺脚本/启动失败/超时/崩溃/响应非法/表达式超长 → 全部
//!   [`CasFailure`]，比较器转「无法判定」并写原因（规范 §11）。
//! - **安全**：只发送本 crate 生成的受限 ASCII 表达式（[`crate::ast::Expr::to_cas`]），
//!   不含学生个人信息；表达式长度上限 1000；sidecar 脚本对输入做字符集与函数名白名单
//!   校验（见 `scripts/cas/sidecar.py`）。
//! - **缓存**：键 = 规范化表达式（`to_cas` 结果）+ CAS 版本 + 协议版本；版本变化自动
//!   失效；缓存开关来自配置。
//! - **版本进引擎指纹**：[`CasClient::engine_stamp`] 生成 `EngineStamp`（role=Judge、
//!   backend=`cas_sidecar`、model=sympy、version=sympy 版本、params_hash 含协议/命令哈希），
//!   供 `grade` 写入证据的 engines；[`CasClient::fingerprint`] 供学科包引擎指纹。

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use eg_domain::{BackendId, EngineStamp, ModelRole, Sha256Hex};
use serde_json::{Value, json};

/// CAS 协议版本（请求/响应字段变化时必须升级；进缓存键与引擎指纹）。
pub const CAS_PROTOCOL_VERSION: &str = "1";

/// 默认单次超时（规范 §7：≤3 s）。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);

/// 表达式长度上限（防御超大输入；规范 §11 快速失败）。
pub const MAX_EXPRESSION_CHARS: usize = 1_000;

/// CAS 配置（对应 `CONFIG.md` §8 `cas.*` 的运行时视图）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CasConfig {
    /// 是否启用（`cas.enabled`）。
    pub enabled: bool,
    /// 启动命令（解释器 + sidecar 脚本 + 可选参数；`cas.python_path` 由平台注入）。
    pub command: Vec<String>,
    /// 单次超时（`cas.timeout_s`，1–10 s）。
    pub timeout: Duration,
    /// 是否启用结果缓存（`cas.cache`）。
    pub cache: bool,
}

impl CasConfig {
    /// 禁用 CAS（所有相关比较器返回无法判定）。
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            command: Vec::new(),
            timeout: DEFAULT_TIMEOUT,
            cache: false,
        }
    }

    /// 显式配置（供平台注入 `cas.python_path`/脚本路径）。
    #[must_use]
    pub fn new(command: Vec<String>, timeout: Duration, cache: bool) -> Self {
        Self {
            enabled: !command.is_empty(),
            command,
            timeout,
            cache,
        }
    }

    /// 从环境变量读取（无平台配置时的独立运行/测试路径）。
    ///
    /// - `EG_CAS_DISABLED=1` 强制禁用；
    /// - `EG_CAS_PYTHON` 解释器路径（默认 `python3`）；
    /// - `EG_CAS_SCRIPT` sidecar 脚本绝对路径（缺失则 CAS 不可用）；
    /// - `EG_CAS_TIMEOUT_MS` 超时毫秒（默认 3000，夹在 1000–10000）；
    /// - `EG_CAS_CACHE=0` 关闭缓存。
    #[must_use]
    pub fn from_env() -> Self {
        if std::env::var("EG_CAS_DISABLED").as_deref() == Ok("1") {
            return Self::disabled();
        }
        let Some(script) = std::env::var("EG_CAS_SCRIPT").ok() else {
            return Self::disabled();
        };
        let python = std::env::var("EG_CAS_PYTHON").unwrap_or_else(|_| "python3".to_owned());
        let timeout_ms = std::env::var("EG_CAS_TIMEOUT_MS")
            .ok()
            .and_then(|text| text.parse::<u64>().ok())
            .unwrap_or(3_000)
            .clamp(1_000, 10_000);
        let cache = std::env::var("EG_CAS_CACHE").as_deref() != Ok("0");
        Self::new(
            vec![python, script],
            Duration::from_millis(timeout_ms),
            cache,
        )
    }

    /// 设置超时（便于测试注入短超时）。
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// 命令/配置指纹（进引擎指纹，不含个人信息）。
    #[must_use]
    pub fn command_hash(&self) -> Sha256Hex {
        let material = format!(
            "protocol={CAS_PROTOCOL_VERSION};enabled={};cache={};command={}",
            self.enabled,
            self.cache,
            self.command.join("\u{1f}")
        );
        Sha256Hex::of(material.as_bytes())
    }
}

/// CAS 版本信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CasInfo {
    /// sympy 版本（如 `1.14.0`）。
    pub version: String,
    /// 协议版本。
    pub protocol: String,
}

/// CAS 失败原因（一律映射为「无法判定」，绝不猜）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CasFailure {
    /// 未启用。
    #[error("CAS 未启用（cas.enabled=off 或未配置脚本）")]
    Disabled,
    /// 启动失败/命令不可用。
    #[error("CAS 不可用：{reason}")]
    Unavailable {
        /// 可读原因。
        reason: String,
    },
    /// 超时（已强制结束子进程）。
    #[error("CAS 超时（{} ms），已终止子进程并降级为无法判定", timeout.as_millis())]
    Timeout {
        /// 超时阈值。
        timeout: Duration,
    },
    /// 进程异常退出/无响应。
    #[error("CAS 进程异常退出：{reason}")]
    Crashed {
        /// 可读原因。
        reason: String,
    },
    /// 响应不是合法 JSON 或字段缺失。
    #[error("CAS 响应非法：{reason}")]
    Malformed {
        /// 可读原因。
        reason: String,
    },
    /// 输入被拒绝（过长/含非法字符）。
    #[error("CAS 输入被拒绝：{reason}")]
    Rejected {
        /// 可读原因。
        reason: String,
    },
}

impl CasFailure {
    /// 面向比较器/人工的可读原因。
    #[must_use]
    pub fn reason(&self) -> String {
        self.to_string()
    }
}

/// 化简差式的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CasZeroResult {
    /// 差式化简为 0（等价）。
    Zero {
        /// 规范化差式文本（通常 `0`）。
        diff: String,
        /// CAS 版本。
        version: String,
    },
    /// 差式化简后不为 0。
    NonZero {
        /// 化简后的差式文本。
        diff: String,
        /// 差式是否不含自由变量（常量差非 0 时可安全判不相等）。
        constant: bool,
        /// CAS 版本。
        version: String,
    },
    /// 失败（无法判定）。
    Failure(CasFailure),
}

impl CasZeroResult {
    /// 是否判定为 0。
    #[must_use]
    pub fn is_zero(&self) -> bool {
        matches!(self, Self::Zero { .. })
    }

    /// 失败原因（成功时为 `None`）。
    #[must_use]
    pub fn failure(&self) -> Option<&CasFailure> {
        match self {
            Self::Failure(failure) => Some(failure),
            Self::Zero { .. } | Self::NonZero { .. } => None,
        }
    }

    /// CAS 版本（失败时为 `None`）。
    #[must_use]
    pub fn version(&self) -> Option<&str> {
        match self {
            Self::Zero { version, .. } | Self::NonZero { version, .. } => Some(version),
            Self::Failure(_) => None,
        }
    }
}

/// 缓存键：表达式规范形 + CAS 版本 + 协议版本。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    lhs: String,
    rhs: String,
    version: String,
}

/// CAS 运行统计（诊断/测试用）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CasStats {
    /// 启动子进程次数。
    pub spawns: usize,
    /// 缓存命中次数。
    pub cache_hits: usize,
    /// 失败次数（不可用/崩溃/非法响应）。
    pub failures: usize,
    /// 超时次数。
    pub timeouts: usize,
}

/// CAS 引擎指纹（进学科包引擎指纹/证据）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CasFingerprint {
    /// 协议版本。
    pub protocol: String,
    /// sympy 版本（未探测时为 `None`）。
    pub version: Option<String>,
    /// 命令/配置哈希（十六进制）。
    pub command_hash: String,
}

/// sympy sidecar 客户端（线程安全；可被比较器共享）。
#[derive(Debug)]
pub struct CasClient {
    config: CasConfig,
    cache: Mutex<HashMap<CacheKey, CasZeroResult>>,
    stats: Mutex<CasStats>,
    version: Mutex<Option<String>>,
    spawn_counter: AtomicUsize,
}

impl CasClient {
    /// 构造客户端。
    #[must_use]
    pub fn new(config: CasConfig) -> Self {
        Self {
            config,
            cache: Mutex::new(HashMap::new()),
            stats: Mutex::new(CasStats::default()),
            version: Mutex::new(None),
            spawn_counter: AtomicUsize::new(0),
        }
    }

    /// 从环境变量构造（未配置时返回 `None`，等价于 CAS 不可用）。
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let config = CasConfig::from_env();
        config.enabled.then(|| Self::new(config))
    }

    /// 配置。
    #[must_use]
    pub fn config(&self) -> &CasConfig {
        &self.config
    }

    /// 运行统计。
    #[must_use]
    pub fn stats(&self) -> CasStats {
        self.stats
            .lock()
            .map_or_else(|_| CasStats::default(), |stats| *stats)
    }

    /// 缓存条目数（测试/诊断用）。
    #[must_use]
    pub fn cache_len(&self) -> usize {
        self.cache.lock().map_or(0, |cache| cache.len())
    }

    /// 已知 CAS 版本（未探测时为 `None`）。
    #[must_use]
    pub fn version(&self) -> Option<String> {
        self.version.lock().ok().and_then(|version| version.clone())
    }

    /// 探测 CAS 版本（带超时；失败返回 `None`）。
    #[must_use]
    pub fn ensure_version(&self) -> Option<String> {
        if let Some(version) = self.version() {
            return Some(version);
        }
        self.ping().ok().map(|info| info.version)
    }

    /// 引擎指纹（未探测版本时尝试探测一次）。
    #[must_use]
    pub fn fingerprint(&self) -> CasFingerprint {
        CasFingerprint {
            protocol: CAS_PROTOCOL_VERSION.to_owned(),
            version: self.ensure_version(),
            command_hash: self.config.command_hash().to_hex(),
        }
    }

    /// 生成 CAS 的 `EngineStamp`（供 `grade` 写入证据 engines）。
    ///
    /// 版本探测失败/未启用时返回 `None`（证据里不出现未验证版本）。
    #[must_use]
    pub fn engine_stamp(&self) -> Option<EngineStamp> {
        let version = self.ensure_version()?;
        let params = Sha256Hex::of(
            format!(
                "protocol={CAS_PROTOCOL_VERSION};sympy={version};command_hash={}",
                self.config.command_hash().to_hex()
            )
            .as_bytes(),
        );
        Some(EngineStamp {
            role: ModelRole::Judge,
            backend: BackendId::new("cas_sidecar").ok()?,
            model_id: "sympy".to_owned(),
            model_version: version,
            prompt_hash: None,
            params_hash: params,
        })
    }

    /// 探测 sidecar（版本）。
    ///
    /// # Errors
    ///
    /// 禁用/启动失败/超时/崩溃/响应非法返回 [`CasFailure`]。
    pub fn ping(&self) -> Result<CasInfo, CasFailure> {
        let response = self.request(&json!({
            "op": "ping",
            "protocol": CAS_PROTOCOL_VERSION,
        }))?;
        let version = response
            .get("sympy")
            .and_then(Value::as_str)
            .ok_or_else(|| CasFailure::Malformed {
                reason: "ping 响应缺少 sympy 版本".to_owned(),
            })?
            .to_owned();
        let protocol = response
            .get("protocol")
            .and_then(Value::as_str)
            .unwrap_or(CAS_PROTOCOL_VERSION)
            .to_owned();
        if let Ok(mut known) = self.version.lock() {
            *known = Some(version.clone());
        }
        Ok(CasInfo { version, protocol })
    }

    /// 判定 `lhs - rhs` 是否恒等于 0（缓存命中直接返回）。
    ///
    /// # Errors
    ///
    /// 不返回 `Err`：失败以 [`CasZeroResult::Failure`] 表达（比较器统一转无法判定）。
    #[must_use]
    pub fn simplify_zero(&self, lhs: &str, rhs: &str) -> CasZeroResult {
        if !self.config.enabled {
            return CasZeroResult::Failure(CasFailure::Disabled);
        }
        if let Err(failure) = validate_expression(lhs).and_then(|()| validate_expression(rhs)) {
            return CasZeroResult::Failure(failure);
        }
        // 缓存：仅在已知版本时命中（版本未知则先请求）
        if self.config.cache
            && let Some(version) = self.version()
        {
            let key = CacheKey {
                lhs: lhs.to_owned(),
                rhs: rhs.to_owned(),
                version,
            };
            if let Ok(cache) = self.cache.lock()
                && let Some(cached) = cache.get(&key)
            {
                if let Ok(mut stats) = self.stats.lock() {
                    stats.cache_hits += 1;
                }
                return cached.clone();
            }
        }
        let response = match self.request(&json!({
            "op": "simplify_zero",
            "lhs": lhs,
            "rhs": rhs,
            "protocol": CAS_PROTOCOL_VERSION,
        })) {
            Ok(response) => response,
            Err(failure) => {
                if let Ok(mut stats) = self.stats.lock() {
                    stats.failures += 1;
                    if matches!(failure, CasFailure::Timeout { .. }) {
                        stats.timeouts += 1;
                    }
                }
                return CasZeroResult::Failure(failure);
            }
        };
        let version = response
            .get("sympy")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if version.is_empty() {
            return CasZeroResult::Failure(CasFailure::Malformed {
                reason: "simplify_zero 响应缺少 sympy 版本".to_owned(),
            });
        }
        // 版本变化 → 清空旧缓存（规范 §10：缓存键含 CAS 版本）
        if let Ok(mut known) = self.version.lock() {
            if known.as_deref().is_some_and(|known| known != version)
                && let Ok(mut cache) = self.cache.lock()
            {
                cache.clear();
            }
            *known = Some(version.clone());
        }
        let result = if response.get("ok").and_then(Value::as_bool).unwrap_or(false) {
            let diff = response
                .get("diff")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            if response
                .get("zero")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                CasZeroResult::Zero { diff, version }
            } else {
                CasZeroResult::NonZero {
                    diff,
                    constant: response
                        .get("constant")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    version,
                }
            }
        } else {
            let reason = response
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("sidecar 返回 ok=false")
                .to_owned();
            if let Ok(mut stats) = self.stats.lock() {
                stats.failures += 1;
            }
            return CasZeroResult::Failure(CasFailure::Unavailable { reason });
        };
        if self.config.cache {
            let key = CacheKey {
                lhs: lhs.to_owned(),
                rhs: rhs.to_owned(),
                version: result.version().unwrap_or_default().to_owned(),
            };
            if let Ok(mut cache) = self.cache.lock() {
                cache.insert(key, result.clone());
            }
        }
        result
    }

    fn request(&self, payload: &Value) -> Result<Value, CasFailure> {
        if !self.config.enabled {
            return Err(CasFailure::Disabled);
        }
        let Some(program) = self.config.command.first() else {
            return Err(CasFailure::Unavailable {
                reason: "未配置 sidecar 启动命令（cas.python_path/脚本缺失）".to_owned(),
            });
        };
        let mut child = Command::new(program)
            .args(&self.config.command[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| CasFailure::Unavailable {
                reason: format!("启动 sidecar 失败：{error}"),
            })?;
        self.spawn_counter.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut stats) = self.stats.lock() {
            stats.spawns += 1;
        }
        let stdout = child.stdout.take();
        let (sender, receiver) = mpsc::channel::<std::io::Result<String>>();
        let reader = stdout.map(|stdout| {
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stdout);
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => {
                        let _ = sender.send(Err(std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "sidecar 未输出响应",
                        )));
                    }
                    Ok(_) => {
                        let _ = sender.send(Ok(line));
                    }
                    Err(error) => {
                        let _ = sender.send(Err(error));
                    }
                }
            })
        });
        // 写入请求并关闭 stdin（sidecar 读到 EOF 后退出）
        let write_result = child.stdin.take().map_or_else(
            || {
                Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "无法写入 sidecar stdin",
                ))
            },
            |mut stdin| {
                let result = writeln!(stdin, "{payload}").and_then(|()| stdin.flush());
                drop(stdin);
                result
            },
        );
        if let Err(error) = write_result {
            let _ = child.kill();
            let _ = child.wait();
            if let Some(reader) = reader {
                let _ = reader.join();
            }
            return Err(CasFailure::Crashed {
                reason: format!("写入 sidecar 失败：{error}"),
            });
        }
        let outcome = receiver.recv_timeout(self.config.timeout);
        let _ = child.kill();
        let exit = child.wait().ok();
        if let Some(reader) = reader {
            let _ = reader.join();
        }
        match outcome {
            Ok(Ok(line)) => {
                serde_json::from_str(line.trim()).map_err(|error| CasFailure::Malformed {
                    reason: format!("响应不是合法 JSON：{error}"),
                })
            }
            Ok(Err(error)) => Err(CasFailure::Crashed {
                reason: format!(
                    "读取 sidecar 响应失败：{error}{}",
                    exit.map_or(String::new(), |status| format!("（退出状态 {status}）"))
                ),
            }),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(CasFailure::Timeout {
                timeout: self.config.timeout,
            }),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(CasFailure::Crashed {
                reason: format!(
                    "sidecar 提前退出{}",
                    exit.map_or(String::new(), |status| format!("（退出状态 {status}）"))
                ),
            }),
        }
    }
}

fn validate_expression(expression: &str) -> Result<(), CasFailure> {
    if expression.chars().count() > MAX_EXPRESSION_CHARS {
        return Err(CasFailure::Rejected {
            reason: format!("表达式超过 {MAX_EXPRESSION_CHARS} 字符"),
        });
    }
    if expression.contains('\n') || expression.contains('\r') {
        return Err(CasFailure::Rejected {
            reason: "表达式含换行（JSON-lines 协议不允许）".to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn disabled_client_reports_failure() {
        let client = CasClient::new(CasConfig::disabled());
        let result = client.simplify_zero("x", "x");
        assert!(result.failure().is_some());
        assert_eq!(
            result.failure().map(CasFailure::reason),
            Some(CasFailure::Disabled.reason())
        );
        assert!(client.engine_stamp().is_none());
        assert!(client.ensure_version().is_none());
    }

    #[test]
    fn rejects_oversized_expression() {
        let client = CasClient::new(CasConfig::new(
            vec!["/bin/true".to_owned()],
            DEFAULT_TIMEOUT,
            false,
        ));
        let long = "1".repeat(MAX_EXPRESSION_CHARS + 1);
        let result = client.simplify_zero(&long, "0");
        assert!(matches!(
            result.failure(),
            Some(CasFailure::Rejected { .. })
        ));
    }

    #[test]
    fn config_hash_is_stable_and_sensitive() {
        let first = CasConfig::new(
            vec!["python3".to_owned(), "a.py".to_owned()],
            DEFAULT_TIMEOUT,
            true,
        );
        let second = first.clone();
        assert_eq!(first.command_hash(), second.command_hash());
        let third = CasConfig::new(
            vec!["python3".to_owned(), "b.py".to_owned()],
            DEFAULT_TIMEOUT,
            true,
        );
        assert_ne!(first.command_hash(), third.command_hash());
    }
}
