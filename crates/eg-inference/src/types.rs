//! 共享小类型：token 用量与健康报告（`SPEC-inference.md` §3.1/§6）。
//!
//! 放在独立模块是为了让 `http`/`budget`/`service`/`local` 之间不互相依赖。

use eg_domain::BackendId;

/// 一次调用的 token 用量（本地可得时记录；不可得时全 0 并标注限制）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenUsage {
    /// 输入 token（prompt）。
    pub input_tokens: u32,
    /// 命中提示词缓存的输入 token（云端计费更低一档）。
    pub cached_input_tokens: u32,
    /// 输出 token（completion）。
    pub output_tokens: u32,
}

impl TokenUsage {
    /// 全 0 用量（本地不可得或未发生调用）。
    pub const ZERO: Self = Self {
        input_tokens: 0,
        cached_input_tokens: 0,
        output_tokens: 0,
    };

    /// 是否为全 0（用于「token 不可得」判断）。
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.input_tokens == 0 && self.cached_input_tokens == 0 && self.output_tokens == 0
    }

    /// 计入计费的输入 token（未被缓存命中的部分）。
    #[must_use]
    pub const fn billed_input_tokens(self) -> u32 {
        self.input_tokens.saturating_sub(self.cached_input_tokens)
    }
}

/// 后端健康状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HealthState {
    /// 未启动（本地 sidecar 尚未拉起）。
    Stopped,
    /// 启动中 / 探活未通过但仍在等待。
    Starting,
    /// 就绪（可接受调用）。
    Ready,
    /// 降级（曾就绪，最近探活失败；停止新调用）。
    Degraded,
    /// 不可用（重试耗尽；任务必须显式失败）。
    Unavailable,
}

impl HealthState {
    /// 是否允许发起新调用。
    #[must_use]
    pub const fn accepts_calls(self) -> bool {
        matches!(self, Self::Ready)
    }

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
}

/// 健康报告（诊断包与界面用，`SPEC-inference.md` §6）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthReport {
    /// 后端 id。
    pub backend: BackendId,
    /// 状态。
    pub state: HealthState,
    /// 最近一次探活耗时（毫秒；未探测为 `None`）。
    pub probe_latency_ms: Option<u64>,
    /// 中文说明（失败原因或降级原因）。
    pub detail: String,
}

impl HealthReport {
    /// 就绪报告。
    #[must_use]
    pub fn ready(backend: BackendId, probe_latency_ms: Option<u64>) -> Self {
        Self {
            backend,
            state: HealthState::Ready,
            probe_latency_ms,
            detail: "就绪".to_owned(),
        }
    }

    /// 不可用报告。
    #[must_use]
    pub fn unavailable(backend: BackendId, detail: impl Into<String>) -> Self {
        Self {
            backend,
            state: HealthState::Unavailable,
            probe_latency_ms: None,
            detail: detail.into(),
        }
    }

    /// 是否可接受调用。
    #[must_use]
    pub fn accepts_calls(&self) -> bool {
        self.state.accepts_calls()
    }
}
