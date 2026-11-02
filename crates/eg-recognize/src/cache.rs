//! 识别缓存键/值与内存实现（`SPEC-recognize.md` §3.6）。
//!
//! 键组成：输入资产 id（内容哈希）+ 角色/后端 + 模型 id/版本 + 参数哈希 + 提示词哈希 +
//! Schema 版本 + 作用域 +（仅段级）段框哈希。值只存**已通过语义校验的块**（含块级
//! 置信度与来源标注）与引擎指纹，**不存合成后的页级置信度**——合成规则升级只需重算。
//! 缓存表由 `platform` 拥有；本文件提供键值类型、端口与内存实现（测试/单页复用）。
//!
//! TODO(接口接缝)：`eg-platform` 尚无识别缓存表；表结构定稿后由 `app` 组合根实现
//! [`RecognitionCache`] 并保证「同一键并发只允许一次模型调用」。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use eg_domain::{AssetId, BBox, BackendId, EngineStamp, Sha256Hex};
use serde::{Deserialize, Serialize};

use crate::error::{RecognizeError, storage_error};
use crate::semantic::{BlockRecord, StructureQuality};
use crate::types::ScanScope;

/// 识别缓存 Schema 版本（输出契约变化必须换键）。
pub const CACHE_SCHEMA_VERSION: u16 = 1;

/// 段框哈希的坐标精度（固定格式序列化后哈希）。
const BBOX_PRECISION: usize = 4;

/// 识别缓存键。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey {
    /// 送入模型的图像资产 id（内容寻址，天然等价于内容键）。
    pub asset: AssetId,
    /// 后端保留字。
    pub backend: BackendId,
    /// 模型 id。
    pub model_id: String,
    /// 模型版本。
    pub model_version: String,
    /// 参数/配置哈希（温度、输出上限、图像缩放等）。
    pub params_hash: Sha256Hex,
    /// 提示词哈希（模板字节 + 渲染参数）。
    pub prompt_hash: Sha256Hex,
    /// 输出 Schema 版本。
    pub schema_version: u16,
    /// 页级或段级。
    pub scope: ScanScope,
    /// 段框哈希（仅段级；页级为空）。
    pub segment_bbox_hash: Option<Sha256Hex>,
}

impl CacheKey {
    /// 由实际输入资产与引擎指纹构造缓存键。
    #[must_use]
    pub fn new(
        asset: AssetId,
        engine: &EngineStamp,
        prompt_hash: Sha256Hex,
        scope: ScanScope,
        segment_bbox: Option<BBox>,
    ) -> Self {
        Self {
            asset,
            backend: engine.backend.clone(),
            model_id: engine.model_id.clone(),
            model_version: engine.model_version.clone(),
            params_hash: engine.params_hash,
            prompt_hash,
            schema_version: CACHE_SCHEMA_VERSION,
            scope,
            segment_bbox_hash: segment_bbox.map(bbox_hash),
        }
    }

    /// 稳定文本摘要（日志与审计用）。
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "asset={} backend={} model={}@{} params={} prompt={} schema={} scope={} bbox={}",
            self.asset.to_hex(),
            self.backend,
            self.model_id,
            self.model_version,
            self.params_hash.to_hex(),
            self.prompt_hash.to_hex(),
            self.schema_version,
            self.scope.as_str(),
            self.segment_bbox_hash
                .map_or_else(|| "-".to_owned(), Sha256Hex::to_hex),
        )
    }
}

/// 段框哈希：归一化坐标按固定精度序列化后取 SHA-256。
#[must_use]
pub fn bbox_hash(bbox: BBox) -> Sha256Hex {
    let text = format!(
        "bbox:{:.*},{:.*},{:.*},{:.*}",
        BBOX_PRECISION,
        bbox.x,
        BBOX_PRECISION,
        bbox.y,
        BBOX_PRECISION,
        bbox.w,
        BBOX_PRECISION,
        bbox.h
    );
    Sha256Hex::of(text.as_bytes())
}

/// 识别缓存值（只含已通过语义校验的块与引擎指纹）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CacheValue {
    /// 已校验的块记录（含块级置信度与来源标注）。
    pub records: Vec<BlockRecord>,
    /// 引擎指纹（含本次提示词哈希）。
    pub engine: EngineStamp,
    /// 结构质量（首次/修复通过、顺序、未知块、非法公式、遮挡等派生输入）。
    ///
    /// 缓存不存**合成后的页级/题级置信度**；结构质量是合成所需的派生输入，
    /// 存下来保证命中时能确定性重算，规则升级只需重算而不失效缓存。
    pub structure: StructureQuality,
}

/// 识别缓存端口。
pub trait RecognitionCache {
    /// 读缓存。
    ///
    /// # Errors
    ///
    /// 存储不可用返回 [`RecognizeError::Storage`]（不静默跳过缓存）。
    fn get(&self, key: &CacheKey) -> Result<Option<CacheValue>, RecognizeError>;

    /// 写缓存。
    ///
    /// # Errors
    ///
    /// 存储不可用返回 [`RecognizeError::Storage`]。
    fn put(&self, key: &CacheKey, value: &CacheValue) -> Result<(), RecognizeError>;
}

/// 内存缓存（测试、单进程短生命周期复用；真实缓存由 `platform` 落库）。
#[derive(Debug, Default)]
pub struct MemoryRecognitionCache {
    entries: RefCell<HashMap<CacheKey, CacheValue>>,
    gets: Cell<u64>,
    hits: Cell<u64>,
    puts: Cell<u64>,
    fail_writes: Cell<bool>,
}

impl MemoryRecognitionCache {
    /// 新建空缓存。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 读次数。
    #[must_use]
    pub fn gets(&self) -> u64 {
        self.gets.get()
    }

    /// 命中次数。
    #[must_use]
    pub fn hits(&self) -> u64 {
        self.hits.get()
    }

    /// 写次数。
    #[must_use]
    pub fn puts(&self) -> u64 {
        self.puts.get()
    }

    /// 条目数。
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.borrow().len()
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.borrow().is_empty()
    }

    /// 让后续写入失败（测试存储失败路径）。
    pub fn fail_writes(&self, fail: bool) {
        self.fail_writes.set(fail);
    }
}

impl RecognitionCache for MemoryRecognitionCache {
    fn get(&self, key: &CacheKey) -> Result<Option<CacheValue>, RecognizeError> {
        self.gets.set(self.gets.get() + 1);
        let value = self.entries.borrow().get(key).cloned();
        if value.is_some() {
            self.hits.set(self.hits.get() + 1);
        }
        Ok(value)
    }

    fn put(&self, key: &CacheKey, value: &CacheValue) -> Result<(), RecognizeError> {
        self.puts.set(self.puts.get() + 1);
        if self.fail_writes.get() {
            return Err(storage_error("识别缓存写入失败（注入）"));
        }
        self.entries.borrow_mut().insert(key.clone(), value.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eg_domain::{Confidence, ModelRole};

    fn engine() -> EngineStamp {
        EngineStamp {
            role: ModelRole::Ocr,
            backend: BackendId::fixture(),
            model_id: "PaddleOCR-VL-1.6".to_owned(),
            model_version: "test-1".to_owned(),
            prompt_hash: None,
            params_hash: Sha256Hex::of(b"params"),
        }
    }

    fn empty_structure() -> StructureQuality {
        StructureQuality {
            first_pass: true,
            repaired: false,
            order_violation_ratio: 0.0,
            order_suspect: false,
            two_column_layout: false,
            has_unknown_block: false,
            formula_invalid_count: 0,
            formula_latex_disabled_count: 0,
            stamp_occludes: false,
            coverage_ratio: 0.0,
            background_verified: true,
        }
    }

    fn key(prompt: &str) -> CacheKey {
        CacheKey::new(
            AssetId::of(b"image"),
            &engine(),
            Sha256Hex::of(prompt.as_bytes()),
            ScanScope::Page,
            None,
        )
    }

    #[test]
    fn key_changes_with_prompt_and_engine() {
        let base = key("prompt-a");
        assert_eq!(base, key("prompt-a"));
        assert_ne!(base, key("prompt-b"));

        let mut changed_model = engine();
        changed_model.model_version = "test-2".to_owned();
        let other = CacheKey::new(
            AssetId::of(b"image"),
            &changed_model,
            Sha256Hex::of(b"prompt-a"),
            ScanScope::Page,
            None,
        );
        assert_ne!(base, other);

        let mut changed_params = engine();
        changed_params.params_hash = Sha256Hex::of(b"params-2");
        assert_ne!(
            base,
            CacheKey::new(
                AssetId::of(b"image"),
                &changed_params,
                Sha256Hex::of(b"prompt-a"),
                ScanScope::Page,
                None
            )
        );

        assert_ne!(
            base,
            CacheKey::new(
                AssetId::of(b"image"),
                &engine(),
                Sha256Hex::of(b"prompt-a"),
                ScanScope::Segment,
                None
            )
        );
    }

    #[test]
    fn segment_bbox_participates_in_key() {
        let bbox = BBox::new(0.1, 0.2, 0.3, 0.4).unwrap();
        let with_bbox = CacheKey::new(
            AssetId::of(b"image"),
            &engine(),
            Sha256Hex::of(b"p"),
            ScanScope::Segment,
            Some(bbox),
        );
        assert!(with_bbox.segment_bbox_hash.is_some());
        assert_eq!(with_bbox.segment_bbox_hash, Some(bbox_hash(bbox)));
        let without = CacheKey::new(
            AssetId::of(b"image"),
            &engine(),
            Sha256Hex::of(b"p"),
            ScanScope::Segment,
            None,
        );
        assert_ne!(with_bbox, without);
        let moved = BBox::new(0.2, 0.2, 0.3, 0.4).unwrap();
        assert_ne!(with_bbox.segment_bbox_hash, Some(bbox_hash(moved)));
    }

    #[test]
    fn memory_cache_roundtrip_and_counters() {
        let cache = MemoryRecognitionCache::new();
        let key = key("prompt");
        assert!(cache.get(&key).unwrap().is_none());
        assert_eq!(cache.gets(), 1);
        assert_eq!(cache.hits(), 0);

        let value = CacheValue {
            records: Vec::new(),
            engine: engine(),
            structure: empty_structure(),
        };
        cache.put(&key, &value).unwrap();
        assert_eq!(cache.puts(), 1);
        assert_eq!(cache.len(), 1);
        let hit = cache.get(&key).unwrap().unwrap();
        assert_eq!(hit, value);
        assert_eq!(cache.hits(), 1);
        assert!(!cache.is_empty());
        assert!(
            !value
                .records
                .iter()
                .any(|record| record.block.confidence.is_at_least(Confidence::ONE))
        );
    }

    #[test]
    fn storage_failure_is_retriable_not_manual() {
        let cache = MemoryRecognitionCache::new();
        cache.fail_writes(true);
        let error = cache
            .put(
                &key("p"),
                &CacheValue {
                    records: Vec::new(),
                    engine: engine(),
                    structure: empty_structure(),
                },
            )
            .unwrap_err();
        assert!(error.is_task_level_retriable());
        assert_eq!(error.manual_reason(), None);
    }
}
