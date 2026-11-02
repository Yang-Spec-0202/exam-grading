//! 本地 sidecar 生命周期（`SPEC-inference.md` §4/§9.1）。
//!
//! 状态机：`Stopped → Starting → Ready → Degraded → Unavailable`。
//! - 启动：分配端口（失败重试 3 次）→ 拉起进程 → 轮询 `/health` 至超时 → 就绪；
//!   失败置「不可用」并**显式上报**，绝不静默换模型/换后端；
//! - 降级：探活失败即停止接受新调用，重启一次；连续 3 次失败保持不可用；
//! - 退出：先 SIGTERM，超时后 SIGKILL；`Drop` 兜底避免孤儿进程；进程输出落独立日志。
//!
//! 与 `scripts/model/serve.sh` 的关系：两者是**等价实现**——脚本面向运维手工起停，
//! 本模块面向应用内托管。参数来自同一份注册表（端口、ctx、额外参数）。
//!
//! 测试不依赖真实模型：注入 [`ProcessRunner`] 与 [`Transport`] 桩。

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eg_domain::{BackendId, Clock};

use crate::error::InferenceError;
use crate::http::{HttpRequest, Transport};
use crate::openai::{ChatRequestSpec, ChatResponse, parse_chat_response};
use crate::registry::{ServeUnit, WeightsFile};
use crate::types::{HealthReport, HealthState};

/// 待执行的 sidecar 命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeCommand {
    /// 可执行文件。
    pub program: String,
    /// 参数（含 `-m`/`--mmproj`/端口等）。
    pub args: Vec<String>,
    /// 进程输出追加到的日志文件（`None` 时丢弃）。
    pub log_file: Option<PathBuf>,
}

/// 子进程句柄抽象（测试注入假进程）。
pub trait ChildProcess: Send + Sync {
    /// 进程 id。
    fn pid(&self) -> u32;
    /// 非阻塞查询退出码。
    ///
    /// # Errors
    ///
    /// 查询失败返回错误。
    fn try_exit(&mut self) -> Result<Option<i32>, InferenceError>;
    /// 请求终止（SIGTERM）。
    ///
    /// # Errors
    ///
    /// 发送失败返回错误。
    fn terminate(&mut self) -> Result<(), InferenceError>;
    /// 强制终止（SIGKILL）。
    ///
    /// # Errors
    ///
    /// 发送失败返回错误。
    fn kill(&mut self) -> Result<(), InferenceError>;
}

/// 进程拉起抽象。
pub trait ProcessRunner: Send + Sync + core::fmt::Debug {
    /// 拉起进程。
    ///
    /// # Errors
    ///
    /// 可执行文件不存在、权限不足等返回 [`InferenceError::BackendUnavailable`]。
    fn spawn(&self, command: &ServeCommand) -> Result<Box<dyn ChildProcess>, InferenceError>;
}

/// 端口分配抽象。
pub trait PortAllocator: Send + Sync + core::fmt::Debug {
    /// 分配端口：优先 `preferred`，被占用则重试 `attempts` 次（含首选共 attempts 个）。
    ///
    /// # Errors
    ///
    /// 全部尝试失败返回 [`InferenceError::BackendUnavailable`]。
    fn pick(&self, preferred: u16, attempts: u8) -> Result<u16, InferenceError>;
}

/// 系统进程拉起（`std::process`）。
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemRunner;

impl ProcessRunner for SystemRunner {
    fn spawn(&self, command: &ServeCommand) -> Result<Box<dyn ChildProcess>, InferenceError> {
        let mut process = std::process::Command::new(&command.program);
        process.args(&command.args);
        process.stdin(std::process::Stdio::null());
        if let Some(path) = &command.log_file {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    InferenceError::BackendUnavailable {
                        detail: format!("无法创建日志目录 {}：{error}", parent.display()),
                    }
                })?;
            }
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|error| InferenceError::BackendUnavailable {
                    detail: format!("无法打开 sidecar 日志 {}：{error}", path.display()),
                })?;
            let stderr = file
                .try_clone()
                .map_err(|error| InferenceError::BackendUnavailable {
                    detail: format!("无法复制日志句柄：{error}"),
                })?;
            process.stdout(std::process::Stdio::from(file));
            process.stderr(std::process::Stdio::from(stderr));
        } else {
            process.stdout(std::process::Stdio::null());
            process.stderr(std::process::Stdio::null());
        }
        let child = process
            .spawn()
            .map_err(|error| InferenceError::BackendUnavailable {
                detail: format!("无法启动 {}：{error}", command.program),
            })?;
        Ok(Box::new(SystemChild { child }))
    }
}

/// 真实子进程。
#[derive(Debug)]
struct SystemChild {
    child: std::process::Child,
}

impl ChildProcess for SystemChild {
    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn try_exit(&mut self) -> Result<Option<i32>, InferenceError> {
        match self.child.try_wait() {
            Ok(Some(status)) => Ok(Some(status.code().unwrap_or(-1))),
            Ok(None) => Ok(None),
            Err(error) => Err(InferenceError::BackendUnavailable {
                detail: format!("查询子进程状态失败：{error}"),
            }),
        }
    }

    fn terminate(&mut self) -> Result<(), InferenceError> {
        #[cfg(unix)]
        {
            // 工作区禁止 unsafe：用系统 `kill` 命令发送 SIGTERM（macOS/Linux 均有）。
            let pid = self.child.id().to_string();
            let status = std::process::Command::new("kill")
                .arg("-15")
                .arg(&pid)
                .status();
            match status {
                Ok(status) if status.success() => Ok(()),
                _ => Err(InferenceError::BackendUnavailable {
                    detail: format!("向 pid {pid} 发送 SIGTERM 失败"),
                }),
            }
        }
        #[cfg(not(unix))]
        {
            self.kill()
        }
    }

    fn kill(&mut self) -> Result<(), InferenceError> {
        self.child
            .kill()
            .map_err(|error| InferenceError::BackendUnavailable {
                detail: format!("强制终止子进程失败：{error}"),
            })
    }
}

/// 回环端口分配（绑定探测后释放；与进程启动之间的竞争可接受，失败会显式报错）。
#[derive(Debug, Default, Clone, Copy)]
pub struct LoopbackPortAllocator;

impl PortAllocator for LoopbackPortAllocator {
    fn pick(&self, preferred: u16, attempts: u8) -> Result<u16, InferenceError> {
        let attempts = attempts.max(1);
        for offset in 0..attempts {
            let candidate = preferred.saturating_add(u16::from(offset));
            if candidate == 0 {
                continue;
            }
            let address = format!("127.0.0.1:{candidate}");
            if let Ok(listener) = std::net::TcpListener::bind(&address) {
                drop(listener);
                return Ok(candidate);
            }
        }
        Err(InferenceError::BackendUnavailable {
            detail: format!(
                "端口分配失败：{preferred} 起 {attempts} 个端口均被占用（请停止其它 sidecar）"
            ),
        })
    }
}

/// 本地服务端点的运行时配置（由注册表条目 + 应用配置组合而成）。
#[derive(Debug, Clone, PartialEq)]
pub struct LocalServerSpec {
    /// 服务单元名（共享同权重的条目同名）。
    pub unit: String,
    /// 可执行文件。
    pub binary: String,
    /// 模型目录（权重所在目录），默认 `~/Library/Application Support/exam-grading/models/<unit>`。
    pub models_dir: PathBuf,
    /// 权重文件（component → file）。
    pub weights: Vec<WeightsFile>,
    /// 端口策略首选值。
    pub preferred_port: u16,
    /// 监听地址（固定回环）。
    pub host: String,
    /// 上下文长度。
    pub context_length: u32,
    /// 采样温度（0 保证可复现）。
    pub temperature: f64,
    /// 可选 chat template 文件（PaddleOCR 在 server 下建议传；GLM-OCR 不要传）。
    pub chat_template: Option<String>,
    /// 额外参数（来自注册表，顺序敏感）。
    pub extra_args: Vec<String>,
    /// 启动超时（毫秒）。
    pub startup_timeout_ms: u64,
}

impl LocalServerSpec {
    /// 由注册表服务单元构造。
    #[must_use]
    pub fn from_serve_unit(
        unit: &ServeUnit,
        models_dir: impl Into<PathBuf>,
        context_length: u32,
        temperature: f64,
    ) -> Self {
        Self {
            unit: unit.unit.clone(),
            binary: unit.binary.clone(),
            models_dir: models_dir.into(),
            weights: unit.weights.clone(),
            preferred_port: unit.port,
            host: "127.0.0.1".to_owned(),
            context_length,
            temperature,
            chat_template: unit.chat_template.clone(),
            extra_args: unit.extra_args.clone(),
            startup_timeout_ms: u64::from(unit.startup_timeout_secs) * 1_000,
        }
    }

    /// 生成 llama-server 命令行（与 `scripts/model/serve.sh` 同参数）。
    #[must_use]
    pub fn command(&self, port: u16) -> ServeCommand {
        let mut args: Vec<String> = Vec::new();
        let model_component = self
            .weights
            .iter()
            .find(|weight| weight.component == "model");
        if let Some(model) = model_component {
            args.push("-m".to_owned());
            args.push(
                self.models_dir
                    .join(&model.file)
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        if let Some(projector) = self
            .weights
            .iter()
            .find(|weight| weight.component == "mmproj")
        {
            args.push("--mmproj".to_owned());
            args.push(
                self.models_dir
                    .join(&projector.file)
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        args.push("--host".to_owned());
        args.push(self.host.clone());
        args.push("--port".to_owned());
        args.push(port.to_string());
        args.push("-c".to_owned());
        args.push(self.context_length.to_string());
        args.push("--temp".to_owned());
        args.push(format!("{:.1}", self.temperature));
        // 与 `scripts/model/serve.sh` 一致：Apple Silicon 上把全部层卸载到 Metal。
        // 显式给出而不依赖服务端默认值，避免不同 llama.cpp 版本默认行为漂移。
        args.push("-ngl".to_owned());
        args.push("99".to_owned());
        args.push("-a".to_owned());
        args.push(self.unit.clone());
        args.push("--no-webui".to_owned());
        if let Some(template) = &self.chat_template {
            args.push("--chat-template-file".to_owned());
            args.push(template.clone());
        }
        args.extend(self.extra_args.iter().cloned());
        ServeCommand {
            program: self.binary.clone(),
            args,
            log_file: Some(self.models_dir.join(format!("serve-{}.log", self.unit))),
        }
    }
}

/// 生命周期配置（默认值与 `CONFIG.md`/规范一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LifecycleConfig {
    /// 轮询间隔（毫秒）。
    pub poll_interval_ms: u64,
    /// 停止时等待 SIGTERM 的时长（毫秒），超时强杀。
    pub stop_timeout_ms: u64,
    /// 端口分配尝试次数。
    pub port_attempts: u8,
}

impl Default for LifecycleConfig {
    fn default() -> Self {
        Self {
            poll_interval_ms: 100,
            stop_timeout_ms: 10_000,
            port_attempts: 3,
        }
    }
}

/// 启动指标（`SPEC-inference.md` §9.1 冷/热双指标）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartupMetrics {
    /// 从拉起到 `/health` 就绪的耗时（毫秒）。
    pub elapsed_ms: u64,
    /// 是否首次读盘（冷启动）。依据是模型目录里的就绪标记文件。
    pub cold_start: bool,
    /// 实际使用端口。
    pub port: u16,
}

/// 本地 sidecar 状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerState {
    /// 未启动。
    Stopped,
    /// 启动中。
    Starting,
    /// 就绪。
    Ready,
    /// 降级（探活失败，重启中）。
    Degraded,
    /// 不可用（重试耗尽）。
    Unavailable,
}

impl ServerState {
    /// 中文标签。
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Stopped => "未启动",
            Self::Starting => "启动中",
            Self::Ready => "就绪",
            Self::Degraded => "降级",
            Self::Unavailable => "不可用",
        }
    }

    fn health(self) -> HealthState {
        match self {
            Self::Stopped => HealthState::Stopped,
            Self::Starting => HealthState::Starting,
            Self::Ready => HealthState::Ready,
            Self::Degraded => HealthState::Degraded,
            Self::Unavailable => HealthState::Unavailable,
        }
    }
}

struct Running {
    child: Box<dyn ChildProcess>,
    port: u16,
}

/// 本地 sidecar 托管（启动/探活/重启/停止）。
pub struct LocalServer {
    spec: LocalServerSpec,
    lifecycle: LifecycleConfig,
    transport: Arc<dyn Transport>,
    runner: Arc<dyn ProcessRunner>,
    ports: Arc<dyn PortAllocator>,
    clock: Arc<dyn Clock>,
    marker_path: Option<PathBuf>,
    state: Mutex<LocalState>,
}

struct LocalState {
    phase: ServerState,
    running: Option<Running>,
    consecutive_failures: u32,
    last_detail: String,
}

impl core::fmt::Debug for LocalServer {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("LocalServer")
            .field("unit", &self.spec.unit)
            .field("port", &self.spec.preferred_port)
            .field("state", &self.state_label())
            .finish_non_exhaustive()
    }
}

impl LocalServer {
    /// 构造托管对象。
    #[must_use]
    pub fn new(
        spec: LocalServerSpec,
        lifecycle: LifecycleConfig,
        transport: Arc<dyn Transport>,
        runner: Arc<dyn ProcessRunner>,
        ports: Arc<dyn PortAllocator>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let marker_path = Some(spec.models_dir.join(format!(".ready-{}", spec.unit)));
        Self {
            spec,
            lifecycle,
            transport,
            runner,
            ports,
            clock,
            marker_path,
            state: Mutex::new(LocalState {
                phase: ServerState::Stopped,
                running: None,
                consecutive_failures: 0,
                last_detail: "未启动".to_owned(),
            }),
        }
    }

    /// 当前状态。
    #[must_use]
    pub fn state(&self) -> ServerState {
        self.lock().phase
    }

    /// 状态标签（日志用）。
    #[must_use]
    pub fn state_label(&self) -> &'static str {
        self.state().label()
    }

    /// 服务单元名。
    #[must_use]
    pub fn unit(&self) -> &str {
        &self.spec.unit
    }

    /// 探活（不改变状态；供诊断/健康报告使用）。
    #[must_use]
    pub fn probe_health(&self) -> HealthReport {
        let state = self.state();
        let port = {
            let guard = self.lock();
            guard.running.as_ref().map(|running| running.port)
        };
        let Some(port) = port else {
            return HealthReport {
                backend: BackendId::local_server(),
                state: state.health(),
                probe_latency_ms: None,
                detail: self.lock().last_detail.clone(),
            };
        };
        let started = Instant::now();
        let url = format!("http://{}:{port}/health", self.spec.host);
        let request = HttpRequest::get(url, Duration::from_secs(2));
        match self.transport.execute(&request) {
            Ok(response) if response.is_success() => HealthReport {
                backend: BackendId::local_server(),
                state: HealthState::Ready,
                probe_latency_ms: Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(0)),
                detail: "就绪".to_owned(),
            },
            Ok(response) => {
                let mut guard = self.lock();
                guard.phase = ServerState::Degraded;
                guard.last_detail = format!("健康检查返回 HTTP {}", response.status);
                HealthReport {
                    backend: BackendId::local_server(),
                    state: HealthState::Degraded,
                    probe_latency_ms: None,
                    detail: guard.last_detail.clone(),
                }
            }
            Err(error) => {
                let mut guard = self.lock();
                guard.phase = ServerState::Degraded;
                guard.last_detail = format!("健康检查失败：{error}");
                HealthReport {
                    backend: BackendId::local_server(),
                    state: HealthState::Degraded,
                    probe_latency_ms: None,
                    detail: guard.last_detail.clone(),
                }
            }
        }
    }

    /// 启动 sidecar（幂等：已就绪直接返回）。
    ///
    /// # Errors
    ///
    /// 端口分配失败、进程拉起失败、启动超时（含崩溃）返回
    /// [`InferenceError::BackendUnavailable`] / [`InferenceError::Timeout`]，
    /// 并把状态置为「不可用」。
    pub fn start(&self) -> Result<StartupMetrics, InferenceError> {
        if let Some(metrics) = self.reuse_if_ready() {
            return Ok(metrics);
        }
        let cold_start = self.marker_path.as_ref().is_none_or(|path| !path.exists());
        let port = self
            .ports
            .pick(self.spec.preferred_port, self.lifecycle.port_attempts)?;
        let command = self.spec.command(port);
        let child = self.runner.spawn(&command)?;
        {
            let mut guard = self.lock();
            guard.phase = ServerState::Starting;
            guard.last_detail = format!("启动中（pid {}，端口 {port}）", child.pid());
            guard.running = Some(Running { child, port });
        }
        let started = Instant::now();
        let timeout = Duration::from_millis(self.spec.startup_timeout_ms);
        loop {
            if self.child_exited()? {
                self.stop();
                let detail = self.lock().last_detail.clone();
                return Err(InferenceError::BackendUnavailable {
                    detail: format!("sidecar 启动后立即退出：{detail}"),
                });
            }
            if self.health_ok(port) {
                let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(0);
                if let Some(marker) = &self.marker_path
                    && let Ok(mut file) = std::fs::File::create(marker)
                {
                    let _ = writeln!(file, "{}", self.clock.now().timestamp_millis());
                }
                let mut guard = self.lock();
                guard.phase = ServerState::Ready;
                guard.consecutive_failures = 0;
                guard.last_detail = format!("就绪（{elapsed_ms} ms）");
                return Ok(StartupMetrics {
                    elapsed_ms,
                    cold_start,
                    port,
                });
            }
            if started.elapsed() >= timeout {
                self.stop();
                let mut guard = self.lock();
                guard.phase = ServerState::Unavailable;
                guard.last_detail = format!("启动超时（{} ms）", self.spec.startup_timeout_ms);
                return Err(InferenceError::Timeout {
                    stage: "sidecar 启动探活".to_owned(),
                    timeout_ms: self.spec.startup_timeout_ms,
                });
            }
            std::thread::sleep(Duration::from_millis(self.lifecycle.poll_interval_ms));
        }
    }

    /// 确保可调用：未启动则启动；降级则重启一次；连续 3 次失败置不可用。
    ///
    /// # Errors
    ///
    /// 不可用或重启失败时返回 [`InferenceError::BackendUnavailable`]。
    pub fn ensure_ready(&self) -> Result<(), InferenceError> {
        match self.state() {
            ServerState::Ready => {
                if self.probe_health().state == HealthState::Ready {
                    let mut guard = self.lock();
                    guard.consecutive_failures = 0;
                    return Ok(());
                }
                // 连续 3 次探活失败 → 保持不可用（不再重启）。
                let failures = {
                    let mut guard = self.lock();
                    guard.consecutive_failures = guard.consecutive_failures.saturating_add(1);
                    guard.consecutive_failures
                };
                if failures >= 3 {
                    let mut guard = self.lock();
                    guard.phase = ServerState::Unavailable;
                    "连续 3 次探活失败，后端不可用".clone_into(&mut guard.last_detail);
                    return Err(InferenceError::BackendUnavailable {
                        detail: guard.last_detail.clone(),
                    });
                }
                self.restart_once()
            }
            ServerState::Stopped => self.start().map(|_| ()),
            ServerState::Degraded => self.restart_once(),
            ServerState::Starting => Err(InferenceError::BackendUnavailable {
                detail: "sidecar 仍在启动中".to_owned(),
            }),
            ServerState::Unavailable => Err(InferenceError::BackendUnavailable {
                detail: self.lock().last_detail.clone(),
            }),
        }
    }

    fn restart_once(&self) -> Result<(), InferenceError> {
        {
            let mut guard = self.lock();
            guard.phase = ServerState::Degraded;
        }
        self.stop();
        match self.start() {
            Ok(_) => {
                let mut guard = self.lock();
                guard.consecutive_failures = 0;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    /// 停止 sidecar（先 SIGTERM，超时 SIGKILL）。
    pub fn stop(&self) {
        let running = {
            let mut guard = self.lock();
            guard.phase = ServerState::Stopped;
            "已停止".clone_into(&mut guard.last_detail);
            guard.running.take()
        };
        if let Some(mut running) = running {
            let _ = running.child.terminate();
            let deadline = Instant::now() + Duration::from_millis(self.lifecycle.stop_timeout_ms);
            loop {
                match running.child.try_exit() {
                    Ok(Some(_)) | Err(_) => break,
                    Ok(None) => {}
                }
                if Instant::now() >= deadline {
                    let _ = running.child.kill();
                    break;
                }
                std::thread::sleep(Duration::from_millis(
                    self.lifecycle.poll_interval_ms.max(10),
                ));
            }
        }
    }

    /// 发起一次 chat completions 调用（调用方负责预算/授权/记录）。
    ///
    /// # Errors
    ///
    /// 后端不可用、HTTP 瞬态/永久失败、输出被截断时返回对应错误。
    pub fn chat(
        &self,
        spec: &ChatRequestSpec,
        timeout: Duration,
    ) -> Result<(ChatResponse, u64), InferenceError> {
        self.ensure_ready()?;
        let port = self
            .lock()
            .running
            .as_ref()
            .map(|running| running.port)
            .ok_or_else(|| InferenceError::BackendUnavailable {
                detail: "sidecar 未运行".to_owned(),
            })?;
        let url = format!("http://{}:{port}/v1/chat/completions", self.spec.host);
        let body = spec.to_body();
        let payload = serde_json::to_vec(&body).map_err(|error| InferenceError::Internal {
            detail: format!("请求序列化失败：{error}"),
        })?;
        let started = Instant::now();
        let request = HttpRequest::post_json(url, payload, timeout);
        let response = self.transport.execute(&request)?;
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(0);
        if !response.is_success() {
            return Err(status_error(response.status));
        }
        let text = response.body_text()?;
        let parsed = parse_chat_response(&text)?;
        if parsed.is_truncated() {
            return Err(InferenceError::SchemaViolation {
                reason: "模型输出被 max_tokens 截断（finish_reason=length），拒绝作为完整结果"
                    .to_owned(),
            });
        }
        Ok((parsed, latency_ms))
    }

    fn reuse_if_ready(&self) -> Option<StartupMetrics> {
        let port = {
            let guard = self.lock();
            if guard.phase != ServerState::Ready {
                return None;
            }
            guard.running.as_ref().map(|running| running.port)?
        };
        if self.health_ok(port) {
            Some(StartupMetrics {
                elapsed_ms: 0,
                cold_start: false,
                port,
            })
        } else {
            None
        }
    }

    fn child_exited(&self) -> Result<bool, InferenceError> {
        let mut guard = self.lock();
        match guard.running.as_mut() {
            Some(running) => match running.child.try_exit() {
                Ok(Some(code)) => {
                    guard.last_detail = format!("进程已退出（退出码 {code}）");
                    Ok(true)
                }
                Ok(None) => Ok(false),
                Err(error) => Err(error),
            },
            None => Ok(true),
        }
    }

    fn health_ok(&self, port: u16) -> bool {
        let request = HttpRequest::get(
            format!("http://{}:{port}/health", self.spec.host),
            Duration::from_millis(1_000),
        );
        self.transport
            .execute(&request)
            .is_ok_and(|response| response.is_success())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LocalState> {
        match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

impl Drop for LocalServer {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.state.lock()
            && let Some(mut running) = guard.running.take()
        {
            let _ = running.child.terminate();
            let _ = running.child.kill();
        }
    }
}

/// HTTP 状态码 → 失败分类（429/5xx 瞬态，其余 4xx 永久）。
fn status_error(status: u16) -> InferenceError {
    let detail = format!("sidecar 返回 HTTP {status}");
    if status == 429 || (500..600).contains(&status) {
        InferenceError::Transient { status, detail }
    } else {
        InferenceError::Permanent { status, detail }
    }
}

/// 默认模型目录（与 `scripts/model/defaults.conf` 的 `MODELS_DIR` 一致）。
#[must_use]
pub fn default_models_dir() -> PathBuf {
    let base = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
    base.join("Library/Application Support/exam-grading/models")
}

/// 模型目录下某个服务单元的目录。
#[must_use]
pub fn unit_dir(models_dir: &Path, unit: &str) -> PathBuf {
    models_dir.join(unit)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use crate::http::HttpResponse;
    use crate::testing::{FakeRunner, StubOutcome, StubTransport, TestClock};
    use crate::types::TokenUsage;

    use super::*;

    #[derive(Debug)]
    struct FixedPorts(u16);

    impl PortAllocator for FixedPorts {
        fn pick(&self, _preferred: u16, _attempts: u8) -> Result<u16, InferenceError> {
            Ok(self.0)
        }
    }

    fn health_ok_response() -> StubOutcome {
        StubOutcome::Response(HttpResponse::new(200, br#"{"status":"ok"}"#.to_vec()))
    }

    fn spec(models_dir: &Path) -> LocalServerSpec {
        LocalServerSpec {
            unit: "test-unit".to_owned(),
            binary: "llama-server".to_owned(),
            models_dir: models_dir.to_path_buf(),
            weights: vec![WeightsFile {
                component: "model".to_owned(),
                file: "model.gguf".to_owned(),
                sha256: eg_domain::Sha256Hex::of(b"w"),
                bytes: 10,
            }],
            preferred_port: 8_181,
            host: "127.0.0.1".to_owned(),
            context_length: 8_192,
            temperature: 0.0,
            chat_template: None,
            extra_args: vec!["--chat-template-kwargs".to_owned(), "{}".to_owned()],
            startup_timeout_ms: 200,
        }
    }

    fn server(
        transport: Arc<dyn Transport>,
        runner: Arc<dyn ProcessRunner>,
        models_dir: &Path,
    ) -> LocalServer {
        LocalServer::new(
            spec(models_dir),
            LifecycleConfig {
                poll_interval_ms: 1,
                stop_timeout_ms: 10,
                port_attempts: 3,
            },
            transport,
            runner,
            Arc::new(FixedPorts(8_181)),
            Arc::new(TestClock::at_ms(0)),
        )
    }

    #[test]
    fn command_matches_the_documented_serving_flags() {
        let spec = spec(Path::new("/models"));
        let command = spec.command(8_181);
        assert_eq!(command.program, "llama-server");
        let joined = command.args.join(" ");
        assert!(joined.contains("-m /models/model.gguf"));
        assert!(joined.contains("--host 127.0.0.1"));
        assert!(joined.contains("--port 8181"));
        assert!(joined.contains("-c 8192"));
        assert!(joined.contains("--temp 0.0"));
        assert!(joined.contains("-ngl 99"), "Metal 层数必须显式声明");
        assert!(joined.contains("--no-webui"));
        assert!(joined.contains("--chat-template-kwargs {}"));
    }

    #[test]
    fn start_polls_until_health_is_ok() {
        let directory = tempfile::tempdir().unwrap();
        let stub = StubTransport::new();
        // 第一次 503（加载中），随后 200
        stub.push_json(503, "{}");
        stub.always(health_ok_response());
        let runner = Arc::new(FakeRunner::default());
        let server = server(Arc::new(stub), runner.clone(), directory.path());

        let metrics = server.start().unwrap();
        assert_eq!(server.state(), ServerState::Ready);
        assert_eq!(metrics.port, 8_181);
        assert!(metrics.cold_start, "首次启动应为冷启动");
        assert_eq!(runner.spawn_count(), 1);
        // 第二次启动复用就绪进程
        let again = server.start().unwrap();
        assert!(!again.cold_start);
        assert_eq!(runner.spawn_count(), 1);
    }

    #[test]
    fn start_timeout_reports_unavailable_and_kills_the_child() {
        let directory = tempfile::tempdir().unwrap();
        let stub = StubTransport::new();
        stub.always(StubOutcome::Response(HttpResponse::new(503, Vec::new())));
        let runner = Arc::new(FakeRunner::default());
        let mut config_spec = spec(directory.path());
        config_spec.startup_timeout_ms = 30;
        let server = LocalServer::new(
            config_spec,
            LifecycleConfig {
                poll_interval_ms: 1,
                stop_timeout_ms: 5,
                port_attempts: 1,
            },
            Arc::new(stub),
            runner,
            Arc::new(FixedPorts(8_181)),
            Arc::new(TestClock::at_ms(0)),
        );
        let error = server.start().unwrap_err();
        assert!(matches!(error, InferenceError::Timeout { .. }));
        assert_eq!(server.state(), ServerState::Unavailable);
    }

    #[test]
    fn spawn_failure_is_explicit() {
        let directory = tempfile::tempdir().unwrap();
        let runner = Arc::new(FakeRunner::default());
        runner.fail_spawns();
        let server = server(Arc::new(StubTransport::new()), runner, directory.path());
        let error = server.start().unwrap_err();
        assert!(matches!(error, InferenceError::BackendUnavailable { .. }));
    }

    #[test]
    fn probe_detects_degradation_after_ready() {
        let directory = tempfile::tempdir().unwrap();
        let stub = StubTransport::new();
        stub.route("/health", health_ok_response());
        let runner = Arc::new(FakeRunner::default());
        let server = server(Arc::new(stub.clone()), runner, directory.path());
        server.start().unwrap();
        assert_eq!(server.probe_health().state, HealthState::Ready);

        stub.route_json("/health", 503, "{}");
        let report = server.probe_health();
        assert_eq!(report.state, HealthState::Degraded);
        assert_eq!(server.state(), ServerState::Degraded);
    }

    #[test]
    fn chat_maps_http_status_and_parses_content() {
        let directory = tempfile::tempdir().unwrap();
        let stub = StubTransport::new();
        stub.route("/health", health_ok_response());
        stub.push_json(
            200,
            r#"{"choices":[{"message":{"content":"{\"ok\":true}"},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":10,"completion_tokens":2}}"#,
        );
        let server = server(
            Arc::new(stub.clone()),
            Arc::new(FakeRunner::default()),
            directory.path(),
        );
        let spec = ChatRequestSpec::new("m", vec![crate::openai::ChatMessage::user("hi")], 16);
        let (response, latency) = server.chat(&spec, Duration::from_secs(1)).unwrap();
        assert_eq!(response.content, "{\"ok\":true}");
        assert_eq!(
            response.usage,
            TokenUsage {
                input_tokens: 10,
                cached_input_tokens: 0,
                output_tokens: 2
            }
        );
        assert!(latency <= 5_000);

        // 429 → 瞬态
        stub.push_json(429, "{}");
        let error = server.chat(&spec, Duration::from_secs(1)).unwrap_err();
        assert!(matches!(
            error,
            InferenceError::Transient { status: 429, .. }
        ));

        // 400 → 永久
        stub.push_json(400, "{}");
        let error = server.chat(&spec, Duration::from_secs(1)).unwrap_err();
        assert!(matches!(
            error,
            InferenceError::Permanent { status: 400, .. }
        ));
    }

    #[test]
    fn truncated_output_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let stub = StubTransport::new();
        stub.route("/health", health_ok_response());
        stub.push_json(
            200,
            r#"{"choices":[{"message":{"content":"{\"a\":"},"finish_reason":"length"}]}"#,
        );
        let server = server(
            Arc::new(stub),
            Arc::new(FakeRunner::default()),
            directory.path(),
        );
        let spec = ChatRequestSpec::new("m", vec![crate::openai::ChatMessage::user("hi")], 8);
        let error = server.chat(&spec, Duration::from_secs(1)).unwrap_err();
        assert!(matches!(error, InferenceError::SchemaViolation { .. }));
    }

    #[test]
    fn stop_terminates_the_process() {
        let directory = tempfile::tempdir().unwrap();
        let stub = StubTransport::new();
        stub.always(health_ok_response());
        let runner = Arc::new(FakeRunner::default());
        let server = server(Arc::new(stub), runner.clone(), directory.path());
        server.start().unwrap();
        server.stop();
        assert_eq!(server.state(), ServerState::Stopped);
        let child = runner.last_child().expect("应记录子进程");
        assert!(
            child.terminated.load(std::sync::atomic::Ordering::SeqCst),
            "必须发送终止信号"
        );
    }

    #[test]
    fn probe_failure_restarts_once_and_recovers() {
        let directory = tempfile::tempdir().unwrap();
        let stub = StubTransport::new();
        stub.route("/health", health_ok_response());
        let runner = Arc::new(FakeRunner::default());
        let server = server(Arc::new(stub.clone()), runner.clone(), directory.path());
        server.start().unwrap();
        assert_eq!(runner.spawn_count(), 1);

        // 探活失败 → 降级 → 重启一次（新进程 + health ok）
        stub.route_json("/health", 500, "{}");
        let _ = server.probe_health();
        stub.route("/health", health_ok_response());
        assert!(server.ensure_ready().is_ok());
        assert_eq!(server.state(), ServerState::Ready);
        assert_eq!(runner.spawn_count(), 2, "必须重启一次");
    }

    #[test]
    fn failed_restart_marks_the_backend_unavailable() {
        let directory = tempfile::tempdir().unwrap();
        let stub = StubTransport::new();
        stub.route("/health", health_ok_response());
        let runner = Arc::new(FakeRunner::default());
        let server = server(Arc::new(stub.clone()), runner.clone(), directory.path());
        server.start().unwrap();

        // 探活失败 + 重启后一直 500 → 启动超时 → 显式不可用
        stub.route_json("/health", 500, "{}");
        let error = server.ensure_ready().unwrap_err();
        assert!(matches!(error, InferenceError::Timeout { .. }));
        assert_eq!(server.state(), ServerState::Unavailable);
    }

    #[test]
    fn default_models_dir_matches_scripts_default() {
        let path = default_models_dir();
        assert!(path.ends_with("Library/Application Support/exam-grading/models"));
        assert_eq!(
            unit_dir(Path::new("/models"), "paddleocr-vl-1.6"),
            PathBuf::from("/models/paddleocr-vl-1.6")
        );
    }
}
