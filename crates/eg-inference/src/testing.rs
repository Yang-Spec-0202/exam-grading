//! 测试注入用替身（**生产不使用**，`SPEC-inference.md` §7「测试环境不发起真实网络请求」）。
//!
//! 提供三类替身：
//! - [`TestClock`]：可推进的确定性时钟（熔断/预算/授权测试不睡眠）；
//! - [`StubTransport`]：脚本化 HTTP 响应，可注入 429/500/超时，并记录每次请求；
//! - [`RecordingModelCallStore`] / [`RecordingAuditSink`] / [`MemoryConsentGate`]：
//!   记录型 store 与内存授权门禁（见本模块后续小节）。
//!
//! 这些类型随库发布只是为了 `tests/` 集成测试能复用；生产代码不得依赖它们。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use eg_domain::{Clock, Timestamp};

use crate::error::InferenceError;
use crate::http::{HttpRequest, HttpResponse, Transport};
use crate::local::{ChildProcess, ProcessRunner, ServeCommand};

/// 可推进的确定性时钟。
#[derive(Debug)]
pub struct TestClock {
    now_ms: Mutex<i64>,
}

impl TestClock {
    /// 从指定毫秒时间戳开始。
    #[must_use]
    pub fn at_ms(millis: i64) -> Self {
        Self {
            now_ms: Mutex::new(millis),
        }
    }

    /// 推进时钟。
    pub fn advance_ms(&self, delta: i64) {
        let mut guard = self.lock();
        *guard = guard.saturating_add(delta);
    }

    /// 设置时钟。
    pub fn set_ms(&self, millis: i64) {
        *self.lock() = millis;
    }

    /// 当前毫秒时间戳。
    #[must_use]
    pub fn now_ms(&self) -> i64 {
        *self.lock()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, i64> {
        match self.now_ms.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

impl Clock for TestClock {
    fn now(&self) -> Timestamp {
        chrono::DateTime::<chrono::Utc>::from_timestamp_millis(self.now_ms())
            .unwrap_or(chrono::DateTime::UNIX_EPOCH)
    }
}

/// 脚本化的传输结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StubOutcome {
    /// 返回给定响应。
    Response(HttpResponse),
    /// 模拟超时。
    Timeout {
        /// 阶段说明。
        stage: String,
    },
    /// 模拟传输层错误（连接重置等）。
    Io {
        /// 说明。
        detail: String,
    },
}

#[derive(Debug, Default)]
struct StubInner {
    queue: VecDeque<StubOutcome>,
    routes: Vec<(String, StubOutcome)>,
    fallback: Option<StubOutcome>,
    requests: Vec<HttpRequest>,
}

/// 脚本化传输桩：按入队顺序返回结果，并记录每次请求。
///
/// 克隆共享同一份状态（测试里常需要「一份交给被测对象、一份留给自己入队/断言」）。
#[derive(Debug, Default)]
pub struct StubTransport {
    inner: Arc<Mutex<StubInner>>,
}

impl Clone for StubTransport {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl StubTransport {
    /// 构造空桩（未配置响应时返回显式错误，而不是静默成功）。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 入队一个响应。
    pub fn push(&self, outcome: StubOutcome) {
        self.lock().queue.push_back(outcome);
    }

    /// 入队一个 JSON 响应。
    pub fn push_json(&self, status: u16, body: &str) {
        let mut response = HttpResponse::new(status, body.as_bytes().to_vec());
        response
            .headers
            .push(("content-type".to_owned(), "application/json".to_owned()));
        self.push(StubOutcome::Response(response));
    }

    /// 入队一个超时。
    pub fn push_timeout(&self, stage: &str) {
        self.push(StubOutcome::Timeout {
            stage: stage.to_owned(),
        });
    }

    /// 按 URL 子串路由（优先于队列）；同一子串重复设置即覆盖。
    ///
    /// 用途：让 `/health` 与 `/v1/chat/completions` 互不干扰地各自脚本化。
    pub fn route(&self, url_contains: &str, outcome: StubOutcome) {
        let mut inner = self.lock();
        if let Some(slot) = inner
            .routes
            .iter_mut()
            .find(|(pattern, _)| pattern == url_contains)
        {
            slot.1 = outcome;
        } else {
            inner.routes.push((url_contains.to_owned(), outcome));
        }
    }

    /// 便捷：按 URL 子串返回 JSON。
    pub fn route_json(&self, url_contains: &str, status: u16, body: &str) {
        let mut response = HttpResponse::new(status, body.as_bytes().to_vec());
        response
            .headers
            .push(("content-type".to_owned(), "application/json".to_owned()));
        self.route(url_contains, StubOutcome::Response(response));
    }

    /// 设置兜底结果（队列耗尽后使用）。
    pub fn always(&self, outcome: StubOutcome) {
        self.lock().fallback = Some(outcome);
    }

    /// 已记录的请求（按调用顺序）。
    #[must_use]
    pub fn requests(&self) -> Vec<HttpRequest> {
        self.lock().requests.clone()
    }

    /// 已发起的请求数。
    #[must_use]
    pub fn call_count(&self) -> usize {
        self.lock().requests.len()
    }

    /// 最近一次请求。
    #[must_use]
    pub fn last_request(&self) -> Option<HttpRequest> {
        self.lock().requests.last().cloned()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, StubInner> {
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

impl Transport for StubTransport {
    fn execute(&self, request: &HttpRequest) -> Result<HttpResponse, InferenceError> {
        let mut inner = self.lock();
        inner.requests.push(request.clone());
        let routed = inner
            .routes
            .iter()
            .find(|(pattern, _)| request.url.contains(pattern.as_str()))
            .map(|(_, outcome)| outcome.clone());
        let outcome = routed
            .or_else(|| inner.queue.pop_front())
            .or_else(|| inner.fallback.clone())
            .ok_or_else(|| InferenceError::Transport {
                detail: "测试桩未配置响应".to_owned(),
            })?;
        match outcome {
            StubOutcome::Response(response) => Ok(response),
            StubOutcome::Timeout { stage } => Err(InferenceError::Timeout {
                stage,
                timeout_ms: u64::try_from(request.timeout.as_millis()).unwrap_or(0),
            }),
            StubOutcome::Io { detail } => Err(InferenceError::Transient { status: 0, detail }),
        }
    }
}

/// 假子进程的可观测状态（测试与句柄共享）。
#[derive(Debug, Default)]
pub struct FakeFlags {
    /// 是否已退出。
    pub exited: AtomicBool,
    /// 是否收到 SIGTERM。
    pub terminated: AtomicBool,
    /// 是否被 SIGKILL。
    pub killed: AtomicBool,
}

/// 假进程拉起器：记录拉起次数与每个子进程的终止状态。
#[derive(Debug, Default)]
pub struct FakeRunner {
    spawns: AtomicU32,
    fail_spawn: AtomicBool,
    children: Mutex<Vec<Arc<FakeFlags>>>,
}

impl FakeRunner {
    /// 已拉起的进程数。
    #[must_use]
    pub fn spawn_count(&self) -> u32 {
        self.spawns.load(Ordering::SeqCst)
    }

    /// 让后续 `spawn` 失败（模拟可执行文件缺失）。
    pub fn fail_spawns(&self) {
        self.fail_spawn.store(true, Ordering::SeqCst);
    }

    /// 最近一次拉起的子进程状态。
    #[must_use]
    pub fn last_child(&self) -> Option<Arc<FakeFlags>> {
        self.children.lock().ok()?.last().cloned()
    }
}

impl ProcessRunner for FakeRunner {
    fn spawn(&self, _command: &ServeCommand) -> Result<Box<dyn ChildProcess>, InferenceError> {
        if self.fail_spawn.load(Ordering::SeqCst) {
            return Err(InferenceError::BackendUnavailable {
                detail: "假进程拒绝启动".to_owned(),
            });
        }
        let flags = Arc::new(FakeFlags::default());
        let pid = {
            let mut children = match self.children.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            children.push(Arc::clone(&flags));
            u32::try_from(children.len()).unwrap_or(u32::MAX) + 100
        };
        self.spawns.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(FakeChildHandle { pid, flags }))
    }
}

#[derive(Debug)]
struct FakeChildHandle {
    pid: u32,
    flags: Arc<FakeFlags>,
}

impl ChildProcess for FakeChildHandle {
    fn pid(&self) -> u32 {
        self.pid
    }
    fn try_exit(&mut self) -> Result<Option<i32>, InferenceError> {
        Ok(if self.flags.exited.load(Ordering::SeqCst) {
            Some(0)
        } else {
            None
        })
    }
    fn terminate(&mut self) -> Result<(), InferenceError> {
        self.flags.terminated.store(true, Ordering::SeqCst);
        self.flags.exited.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn kill(&mut self) -> Result<(), InferenceError> {
        self.flags.killed.store(true, Ordering::SeqCst);
        self.flags.exited.store(true, Ordering::SeqCst);
        Ok(())
    }
}

/// 便捷构造：把桩放进 `Arc`（后端/服务持有 `Arc<dyn Transport>`）。
#[must_use]
pub fn shared_transport(transport: StubTransport) -> Arc<dyn Transport> {
    Arc::new(transport)
}

/// 记录型 `ModelCall` 仓库（测试用）。
#[derive(Debug, Default)]
pub struct RecordingStore {
    calls: Mutex<Vec<eg_domain::ModelCall>>,
}

impl RecordingStore {
    /// 已记录的调用。
    #[must_use]
    pub fn calls(&self) -> Vec<eg_domain::ModelCall> {
        match self.calls.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// 已记录条数。
    #[must_use]
    pub fn len(&self) -> usize {
        self.calls.lock().map_or(0, |guard| guard.len())
    }

    /// 是否一条记录都没有。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl crate::call::ModelCallStore for RecordingStore {
    fn record(&self, call: &eg_domain::ModelCall) -> Result<(), InferenceError> {
        match self.calls.lock() {
            Ok(mut guard) => guard.push(call.clone()),
            Err(poisoned) => poisoned.into_inner().push(call.clone()),
        }
        Ok(())
    }
}

/// 记录型审计（测试用）。
#[derive(Debug, Default)]
pub struct RecordingAudit {
    events: Mutex<Vec<crate::call::AuditEvent>>,
}

impl RecordingAudit {
    /// 已记录事件。
    #[must_use]
    pub fn events(&self) -> Vec<crate::call::AuditEvent> {
        match self.events.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

impl crate::call::AuditSink for RecordingAudit {
    fn record(&self, event: &crate::call::AuditEvent) -> Result<(), InferenceError> {
        match self.events.lock() {
            Ok(mut guard) => guard.push(event.clone()),
            Err(poisoned) => poisoned.into_inner().push(event.clone()),
        }
        Ok(())
    }
}

/// 计数器 id 生成（测试用，确定性）。
#[derive(Debug, Default)]
pub struct CounterIds {
    next: AtomicU32,
}

impl crate::call::IdSource for CounterIds {
    fn next_model_call_id(&self) -> Result<eg_domain::ModelCallId, InferenceError> {
        let value = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(eg_domain::ModelCallId::from_ulid(
            eg_domain::Ulid::from_u128(u128::from(value)),
        ))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::time::Duration;

    use super::*;

    #[test]
    fn test_clock_advances_deterministically() {
        let clock = TestClock::at_ms(1_000);
        assert_eq!(clock.now().timestamp_millis(), 1_000);
        clock.advance_ms(500);
        assert_eq!(clock.now().timestamp_millis(), 1_500);
        clock.set_ms(9_999);
        assert_eq!(clock.now_ms(), 9_999);
    }

    #[test]
    fn stub_transport_returns_scripted_outcomes_in_order() {
        let stub = StubTransport::new();
        stub.push_json(200, "{\"ok\":true}");
        stub.push_timeout("推理");

        let request = HttpRequest::get("http://127.0.0.1:8081/health", Duration::from_secs(1));
        let response = stub.execute(&request).unwrap();
        assert_eq!(response.status, 200);

        let error = stub.execute(&request).unwrap_err();
        assert!(matches!(error, InferenceError::Timeout { .. }));
        assert_eq!(stub.call_count(), 2);
        assert_eq!(stub.last_request().unwrap().url, request.url);
    }

    #[test]
    fn stub_transport_never_silently_succeeds() {
        let stub = StubTransport::new();
        let request = HttpRequest::get("http://127.0.0.1:1/health", Duration::from_millis(10));
        let error = stub.execute(&request).unwrap_err();
        assert!(error.to_string().contains("未配置响应"));
    }
}
