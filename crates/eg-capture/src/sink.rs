//! 资产写入端口（`AssetSink`）与内存/临时目录实现。
//!
//! 真实实现属于 `platform`（内容寻址资产库：先落盘、后提交、只追加、无删除）。
//! 本 crate 只依赖本端口的语义：
//! - **内容寻址**：同字节 → 同 [`AssetId`]，重复写入命中 `deduped = true`；
//! - 只追加：没有删除/覆盖方法；
//! - 先落盘：`put` 成功返回即字节已持久化（或内存实现中已持有）。
//!
//! `platform` 接入见 `src/platform_bridge.rs`（TODO）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use eg_domain::AssetId;

use crate::error::CaptureError;

/// 入库结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PutResult {
    /// 内容寻址 id（= 内容哈希）。
    pub asset: AssetId,
    /// 是否命中已有内容（去重）。
    pub deduped: bool,
    /// 字节数。
    pub byte_len: u64,
}

/// 资产写入端口（`platform::AssetStore` 的适配点）。
pub trait AssetSink: Send + Sync {
    /// 写入字节并返回内容寻址 id；同字节重复写入不得重复落盘。
    ///
    /// # Errors
    ///
    /// 写入失败（磁盘满、权限、IO）返回 [`CaptureError::Asset`]。
    fn put(&self, bytes: &[u8], media_type: &str) -> Result<PutResult, CaptureError>;

    /// 资产是否已存在（只读探测）。
    fn contains(&self, asset: AssetId) -> bool;

    /// 读取资产字节（派生图重建/测试用）。
    ///
    /// # Errors
    ///
    /// 资产不存在或读取失败返回 [`CaptureError::Asset`]。
    fn get(&self, asset: AssetId) -> Result<Vec<u8>, CaptureError>;
}

/// 内存资产库（测试/夹具回放用；不持久化）。
#[derive(Debug, Default)]
pub struct MemoryAssetSink {
    entries: Mutex<BTreeMap<AssetId, StoredAsset>>,
    puts: AtomicU64,
    deduped: AtomicU64,
}

#[derive(Debug, Clone)]
struct StoredAsset {
    bytes: Vec<u8>,
    media_type: String,
}

impl MemoryAssetSink {
    /// 新建空资产库。
    pub fn new() -> Self {
        Self::default()
    }

    /// 已写入的不同资产数。
    pub fn len(&self) -> usize {
        self.entries.lock().map(|g| g.len()).unwrap_or(0)
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `put` 调用总次数（含去重命中）。
    pub fn put_calls(&self) -> u64 {
        self.puts.load(Ordering::Relaxed)
    }

    /// 去重命中次数。
    pub fn dedup_hits(&self) -> u64 {
        self.deduped.load(Ordering::Relaxed)
    }

    /// 资产媒体类型（审计/断言用）。
    pub fn media_type(&self, asset: AssetId) -> Option<String> {
        self.entries
            .lock()
            .ok()
            .and_then(|g| g.get(&asset).map(|entry| entry.media_type.clone()))
    }

    /// 全部资产 id（按内容哈希排序，稳定）。
    pub fn assets(&self) -> Vec<AssetId> {
        self.entries
            .lock()
            .map(|g| g.keys().copied().collect())
            .unwrap_or_default()
    }
}

impl AssetSink for MemoryAssetSink {
    fn put(&self, bytes: &[u8], media_type: &str) -> Result<PutResult, CaptureError> {
        self.puts.fetch_add(1, Ordering::Relaxed);
        let asset = AssetId::of(bytes);
        let byte_len = bytes.len() as u64;
        let mut guard = self
            .entries
            .lock()
            .map_err(|_| CaptureError::Asset("内存资产库锁不可用".to_owned()))?;
        if guard.contains_key(&asset) {
            self.deduped.fetch_add(1, Ordering::Relaxed);
            return Ok(PutResult {
                asset,
                deduped: true,
                byte_len,
            });
        }
        guard.insert(
            asset,
            StoredAsset {
                bytes: bytes.to_vec(),
                media_type: media_type.to_owned(),
            },
        );
        Ok(PutResult {
            asset,
            deduped: false,
            byte_len,
        })
    }

    fn contains(&self, asset: AssetId) -> bool {
        self.entries
            .lock()
            .map(|g| g.contains_key(&asset))
            .unwrap_or(false)
    }

    fn get(&self, asset: AssetId) -> Result<Vec<u8>, CaptureError> {
        self.entries
            .lock()
            .map_err(|_| CaptureError::Asset("内存资产库锁不可用".to_owned()))?
            .get(&asset)
            .map(|entry| entry.bytes.clone())
            .ok_or_else(|| CaptureError::Asset(format!("资产不存在：{asset}")))
    }
}

/// 临时/本地目录资产库：分片路径 `blake3/<前2位>/<次2位>/<完整哈希>`，与
/// `SPEC-platform.md` 的资产目录布局一致，便于将来迁移为 `platform::AssetStore`。
#[derive(Debug)]
pub struct LocalDirAssetSink {
    root: PathBuf,
    puts: AtomicU64,
    deduped: AtomicU64,
}

impl LocalDirAssetSink {
    /// 指定根目录（首次写入时创建 `blake3/` 分片）。
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            puts: AtomicU64::new(0),
            deduped: AtomicU64::new(0),
        }
    }

    /// 根目录。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 资产文件路径（内容寻址）。
    pub fn path_of(&self, asset: AssetId) -> PathBuf {
        let hex = asset.to_hex();
        self.root
            .join("blake3")
            .join(&hex[0..2])
            .join(&hex[2..4])
            .join(&hex)
    }

    /// `put` 调用总次数。
    pub fn put_calls(&self) -> u64 {
        self.puts.load(Ordering::Relaxed)
    }

    /// 去重命中次数。
    pub fn dedup_hits(&self) -> u64 {
        self.deduped.load(Ordering::Relaxed)
    }
}

impl AssetSink for LocalDirAssetSink {
    fn put(&self, bytes: &[u8], _media_type: &str) -> Result<PutResult, CaptureError> {
        self.puts.fetch_add(1, Ordering::Relaxed);
        let asset = AssetId::of(bytes);
        let path = self.path_of(asset);
        let byte_len = bytes.len() as u64;
        if path.exists() {
            self.deduped.fetch_add(1, Ordering::Relaxed);
            return Ok(PutResult {
                asset,
                deduped: true,
                byte_len,
            });
        }
        let parent = path
            .parent()
            .ok_or_else(|| CaptureError::Asset("资产路径无父目录".to_owned()))?;
        std::fs::create_dir_all(parent)
            .map_err(|e| CaptureError::Asset(format!("创建资产目录失败：{e}")))?;
        // 先写临时文件再改名：不允许出现「有行无文件」或半截文件。
        let temp = parent.join(format!(".{}.tmp", asset.to_hex()));
        std::fs::write(&temp, bytes)
            .map_err(|e| CaptureError::Asset(format!("写临时资产失败：{e}")))?;
        std::fs::rename(&temp, &path).map_err(|e| {
            let _ = std::fs::remove_file(&temp);
            CaptureError::Asset(format!("提交资产失败：{e}"))
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let readonly = std::fs::Permissions::from_mode(0o444);
            std::fs::set_permissions(&path, readonly)
                .map_err(|e| CaptureError::Asset(format!("设置资产只读失败：{e}")))?;
        }
        Ok(PutResult {
            asset,
            deduped: false,
            byte_len,
        })
    }

    fn contains(&self, asset: AssetId) -> bool {
        self.path_of(asset).exists()
    }

    fn get(&self, asset: AssetId) -> Result<Vec<u8>, CaptureError> {
        let path = self.path_of(asset);
        std::fs::read(&path).map_err(|e| CaptureError::Asset(format!("读取资产失败：{e}")))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn memory_sink_is_content_addressed() {
        let sink = MemoryAssetSink::new();
        let first = sink.put(b"same bytes", "image/png").unwrap();
        let second = sink.put(b"same bytes", "image/png").unwrap();
        let third = sink.put(b"other bytes", "image/png").unwrap();
        assert!(!first.deduped);
        assert!(second.deduped);
        assert_eq!(first.asset, second.asset);
        assert_ne!(first.asset, third.asset);
        assert_eq!(sink.len(), 2);
        assert_eq!(sink.put_calls(), 3);
        assert_eq!(sink.dedup_hits(), 1);
        assert_eq!(sink.get(first.asset).unwrap(), b"same bytes");
        assert_eq!(sink.media_type(first.asset).as_deref(), Some("image/png"));
    }

    #[test]
    fn local_dir_sink_dedupes_and_is_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let sink = LocalDirAssetSink::new(dir.path());
        let first = sink.put(b"asset-bytes", "image/png").unwrap();
        let second = sink.put(b"asset-bytes", "image/png").unwrap();
        assert!(!first.deduped);
        assert!(second.deduped);
        let path = sink.path_of(first.asset);
        assert!(path.exists());
        assert_eq!(std::fs::read(&path).unwrap(), b"asset-bytes");
        assert_eq!(sink.get(first.asset).unwrap(), b"asset-bytes");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o444, "资产必须只读");
        }
        // 同字节不同 sink 得到同一 AssetId（跨库内容寻址一致）。
        let other = LocalDirAssetSink::new(dir.path().join("second"));
        assert_eq!(
            other.put(b"asset-bytes", "image/png").unwrap().asset,
            first.asset
        );
    }
}
