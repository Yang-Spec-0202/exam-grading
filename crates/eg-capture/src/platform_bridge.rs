//! `eg-platform` 接入适配层（`platform` 已实现，接口稳定）。
//!
//! 本 crate 只依赖端口，`platform` 的具体实现在此适配：
//!
//! | 端口 | `platform` 能力 |
//! |---|---|
//! | [`crate::AssetSink`] | `AssetStore::{put,get,exists}`（BLAKE3 内容寻址、先落盘后提交、无删除） |
//! | [`crate::ClockPort`] | `eg_domain::Clock`（`SystemClock`/`FixedClock`，禁止业务代码读系统时间） |
//! | [`crate::IdPort`] | `UlidFactory::next_ulid`（进程内单例、同毫秒单调、回拨保护） |
//! | [`crate::CancelCheck`] | `eg_platform::CancelToken`（协作式取消） |
//! | [`crate::KnownSubmissions`] | 由组合根查询 `platform` 仓储后注入（本 crate 不直连 SQLite） |
//!
//! **建档落库不在本 crate**：`Importer` 产出 `Submission`/`Page`/派生资产 id 与审计所需的
//! 字段，由组合根经 `platform::db.write(...)` + `repo::insert_submission/insert_page` +
//! `audit::append_in_tx` 在同一事务内落库（见 `docs/dev/research/capture.md` §6 TODO）。

use eg_domain::{AssetId, Timestamp, Ulid};
use eg_platform::{AssetStore, CancelToken, UlidFactory};

use crate::error::CaptureError;
use crate::pipeline::{CancelCheck, ClockPort, IdPort};
use crate::sink::{AssetSink, PutResult};

/// `eg_platform::AssetStore` → [`AssetSink`]。
#[derive(Debug, Clone, Copy)]
pub struct AssetStoreAdapter<'a> {
    store: &'a AssetStore,
}

impl<'a> AssetStoreAdapter<'a> {
    /// 包装平台资产库（只读借用；生命周期由组合根保证）。
    pub fn new(store: &'a AssetStore) -> Self {
        Self { store }
    }
}

impl AssetSink for AssetStoreAdapter<'_> {
    fn put(&self, bytes: &[u8], media_type: &str) -> Result<PutResult, CaptureError> {
        let outcome = self
            .store
            .put(bytes, media_type)
            .map_err(|error| CaptureError::Asset(error.to_string()))?;
        Ok(PutResult {
            asset: outcome.asset,
            deduped: outcome.deduped,
            byte_len: outcome.byte_len,
        })
    }

    fn contains(&self, asset: AssetId) -> bool {
        self.store.exists(asset)
    }

    fn get(&self, asset: AssetId) -> Result<Vec<u8>, CaptureError> {
        self.store
            .get(asset)
            .map_err(|error| CaptureError::Asset(error.to_string()))
    }
}

/// `eg_domain::Clock` → [`ClockPort`]。
#[derive(Clone, Copy)]
pub struct ClockAdapter<'a> {
    clock: &'a dyn eg_domain::Clock,
}

impl std::fmt::Debug for ClockAdapter<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ClockAdapter")
    }
}

impl<'a> ClockAdapter<'a> {
    /// 包装注入时钟。
    pub fn new(clock: &'a dyn eg_domain::Clock) -> Self {
        Self { clock }
    }
}

impl ClockPort for ClockAdapter<'_> {
    fn now(&self) -> Timestamp {
        self.clock.now()
    }
}

/// `eg_platform::UlidFactory` → [`IdPort`]。
#[derive(Debug, Clone, Copy)]
pub struct IdAdapter<'a> {
    ids: &'a UlidFactory,
}

impl<'a> IdAdapter<'a> {
    /// 包装平台 id 工厂（单例/单调由 `platform` 保证）。
    pub fn new(ids: &'a UlidFactory) -> Self {
        Self { ids }
    }
}

impl IdPort for IdAdapter<'_> {
    fn next_ulid(&self) -> Result<Ulid, CaptureError> {
        self.ids
            .next_ulid()
            .map_err(|error| CaptureError::Internal(format!("ULID 生成失败：{error}")))
    }
}

/// `eg_platform::CancelToken` → [`CancelCheck`]。
#[derive(Debug, Clone, Copy)]
pub struct CancelAdapter<'a> {
    token: &'a CancelToken,
}

impl<'a> CancelAdapter<'a> {
    /// 包装取消令牌。
    pub fn new(token: &'a CancelToken) -> Self {
        Self { token }
    }
}

impl CancelCheck for CancelAdapter<'_> {
    fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }
}

/// 接入状态（供诊断/文档断言用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformBridgeState {
    /// 适配层已接入 `platform`（当前状态；`tests/platform_bridge.rs` 覆盖）。
    Connected,
}

/// 当前接入状态。
pub const STATE: PlatformBridgeState = PlatformBridgeState::Connected;
