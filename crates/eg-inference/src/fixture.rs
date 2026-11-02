//! 夹具回放后端（`SPEC-inference.md` §2.2/§7/§9.10）。
//!
//! 用途：全部下游测试与离线演示在**零外联**下跑通完整链路。
//! 规则：
//! - 键 = 输入哈希（模型 id/版本 + 提示词 Schema 版本 + 提示词 + 输入字节）；
//! - 命中时返回**录制时的原始字节**（同输入 100 次字节一致，不重新序列化）；
//! - 未命中 → [`InferenceError::FixtureMiss`]，**绝不合成输出**；
//! - 产物必须可被识别为夹具数据（`EngineStamp.backend = fixture`），不得混入真实成绩。

use std::collections::BTreeMap;
use std::path::Path;

use eg_domain::{BackendId, EngineStamp, Sha256Hex};
use serde::Deserialize;
use serde_json::Value;

use crate::error::InferenceError;

/// 夹具文件 schema 版本。
pub const FIXTURE_SCHEMA_VERSION: u32 = 1;

/// 一条夹具记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureEntry {
    /// 输入哈希键（64 位小写十六进制）。
    pub key: Sha256Hex,
    /// 录制时的模型 id。
    pub model_id: String,
    /// 录制时的模型版本。
    pub model_version: String,
    /// 录制时的提示词 Schema 版本。
    pub prompt_schema_version: String,
    /// 录制的原始输出文本（JSON 对象文本，**逐字节回放**）。
    pub output_raw: String,
}

impl FixtureEntry {
    /// 输出解析为 JSON 对象（回放时仍返回原始字节，这里只做校验/消费）。
    ///
    /// # Errors
    ///
    /// 原始输出不是 JSON 对象时返回 [`InferenceError::SchemaViolation`]。
    pub fn output_value(&self) -> Result<Value, InferenceError> {
        let value: Value = serde_json::from_str(&self.output_raw).map_err(|error| {
            InferenceError::SchemaViolation {
                reason: format!("夹具输出不是合法 JSON：{error}"),
            }
        })?;
        if !value.is_object() {
            return Err(InferenceError::SchemaViolation {
                reason: "夹具输出顶层必须是 JSON 对象".to_owned(),
            });
        }
        Ok(value)
    }
}

/// 夹具库（按输入哈希索引）。
#[derive(Debug, Clone, Default)]
pub struct FixtureStore {
    entries: BTreeMap<String, FixtureEntry>,
}

impl FixtureStore {
    /// 空夹具库。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 插入一条夹具；重复键即失败（避免静默覆盖导致回放不确定）。
    ///
    /// # Errors
    ///
    /// 键重复或输出不是 JSON 对象时返回错误。
    pub fn insert(&mut self, entry: FixtureEntry) -> Result<(), InferenceError> {
        entry.output_value()?;
        let key = entry.key.to_hex();
        if self.entries.contains_key(&key) {
            return Err(InferenceError::Internal {
                detail: format!("夹具键重复：{key}"),
            });
        }
        self.entries.insert(key, entry);
        Ok(())
    }

    /// 从目录加载 `*.json` 夹具文件（不递归）。
    ///
    /// # Errors
    ///
    /// 目录不可读、文件不是合法夹具（schema/键/输出非法）时返回错误。
    pub fn load_dir(path: &Path) -> Result<Self, InferenceError> {
        let mut store = Self::new();
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(path)
            .map_err(|error| InferenceError::Internal {
                detail: format!("无法读取夹具目录 {}：{error}", path.display()),
            })?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .collect();
        files.sort();
        for file in files {
            let text =
                std::fs::read_to_string(&file).map_err(|error| InferenceError::Internal {
                    detail: format!("无法读取夹具 {}：{error}", file.display()),
                })?;
            let raw: RawFixture =
                serde_json::from_str(&text).map_err(|error| InferenceError::Internal {
                    detail: format!("夹具 {} 不是合法 JSON：{error}", file.display()),
                })?;
            if raw.schema_version != FIXTURE_SCHEMA_VERSION {
                return Err(InferenceError::Internal {
                    detail: format!(
                        "夹具 {} 的 schema_version={} 不受支持",
                        file.display(),
                        raw.schema_version
                    ),
                });
            }
            let key =
                Sha256Hex::from_hex(raw.key.trim()).map_err(|_| InferenceError::Internal {
                    detail: format!("夹具 {} 的 key 不是 64 位小写十六进制", file.display()),
                })?;
            store.insert(FixtureEntry {
                key,
                model_id: raw.model_id,
                model_version: raw.model_version,
                prompt_schema_version: raw.prompt_schema_version,
                output_raw: raw.output_raw,
            })?;
        }
        Ok(store)
    }

    /// 按键取夹具。
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&FixtureEntry> {
        self.entries.get(key)
    }

    /// 夹具条数。
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 全部键（诊断用）。
    #[must_use]
    pub fn keys(&self) -> Vec<&str> {
        self.entries.keys().map(String::as_str).collect()
    }
}

/// 计算夹具键：`SHA-256(model_id | version | schema_version | prompt | input)`。
///
/// 用长度前缀分隔，避免拼接歧义（`ab|c` 与 `a|bc` 不会碰撞）。
#[must_use]
pub fn fixture_key(
    model_id: &str,
    model_version: &str,
    prompt_schema_version: &str,
    prompt: &str,
    input: &[u8],
) -> Sha256Hex {
    let mut buffer = Vec::new();
    for part in [model_id, model_version, prompt_schema_version, prompt] {
        buffer.extend_from_slice(format!("{}:", part.len()).as_bytes());
        buffer.extend_from_slice(part.as_bytes());
    }
    buffer.extend_from_slice(format!("{}:", input.len()).as_bytes());
    buffer.extend_from_slice(input);
    Sha256Hex::of(&buffer)
}

/// 一次回放的结果（原始字节 + 解析值）。
#[derive(Debug, Clone, PartialEq)]
pub struct FixtureReplay {
    /// 输入哈希键。
    pub key: Sha256Hex,
    /// 录制时的原始输出文本（逐字节回放）。
    pub output_raw: String,
}

impl FixtureReplay {
    /// 输出解析为 JSON（供上层做 Schema 校验）。
    ///
    /// # Errors
    ///
    /// 原始输出不是 JSON 对象时返回错误。
    pub fn output_value(&self) -> Result<Value, InferenceError> {
        let value: Value = serde_json::from_str(&self.output_raw).map_err(|error| {
            InferenceError::SchemaViolation {
                reason: format!("夹具输出不是合法 JSON：{error}"),
            }
        })?;
        if !value.is_object() {
            return Err(InferenceError::SchemaViolation {
                reason: "夹具输出顶层必须是 JSON 对象".to_owned(),
            });
        }
        Ok(value)
    }
}

/// 夹具回放后端。
#[derive(Debug, Clone)]
pub struct FixtureBackend {
    store: FixtureStore,
    model_id: String,
    model_version: String,
    prompt_schema_version: String,
}

impl FixtureBackend {
    /// 构造回放后端。
    #[must_use]
    pub fn new(
        model_id: impl Into<String>,
        model_version: impl Into<String>,
        prompt_schema_version: impl Into<String>,
        store: FixtureStore,
    ) -> Self {
        Self {
            store,
            model_id: model_id.into(),
            model_version: model_version.into(),
            prompt_schema_version: prompt_schema_version.into(),
        }
    }

    /// 后端 id（固定 `fixture`，便于上层识别夹具数据）。
    #[must_use]
    pub fn backend_id() -> BackendId {
        BackendId::fixture()
    }

    /// 引擎指纹（`backend=fixture`，下游据此标注「夹具数据」）。
    #[must_use]
    pub fn engine_stamp(&self, prompt_hash: Option<Sha256Hex>) -> EngineStamp {
        EngineStamp {
            role: eg_domain::ModelRole::Judge,
            backend: Self::backend_id(),
            model_id: self.model_id.clone(),
            model_version: self.model_version.clone(),
            prompt_hash,
            params_hash: Sha256Hex::of(
                format!("fixture:{}:{}", self.model_id, self.prompt_schema_version).as_bytes(),
            ),
        }
    }

    /// 回放一次调用。
    ///
    /// # Errors
    ///
    /// 未录制该输入时返回 [`InferenceError::FixtureMiss`]（绝不合成输出）。
    pub fn replay(&self, prompt: &str, input: &[u8]) -> Result<FixtureReplay, InferenceError> {
        let key = fixture_key(
            &self.model_id,
            &self.model_version,
            &self.prompt_schema_version,
            prompt,
            input,
        );
        let key_text = key.to_hex();
        let entry = self
            .store
            .get(&key_text)
            .ok_or_else(|| InferenceError::FixtureMiss {
                key: key_text.clone(),
            })?;
        Ok(FixtureReplay {
            key,
            output_raw: entry.output_raw.clone(),
        })
    }

    /// 夹具条数。
    #[must_use]
    pub fn len(&self) -> usize {
        self.store.len()
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFixture {
    schema_version: u32,
    key: String,
    model_id: String,
    model_version: String,
    prompt_schema_version: String,
    output_raw: String,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn entry(prompt: &str, input: &[u8], output: &str) -> FixtureEntry {
        FixtureEntry {
            key: fixture_key("m1", "1.0", "ocr.blocks.v1", prompt, input),
            model_id: "m1".to_owned(),
            model_version: "1.0".to_owned(),
            prompt_schema_version: "ocr.blocks.v1".to_owned(),
            output_raw: output.to_owned(),
        }
    }

    #[test]
    fn replay_is_byte_identical_for_one_hundred_replays() {
        // JSON 文本里 LaTeX 的反斜杠必须是 JSON 转义（\\)；回放要求逐字节一致。
        let output = r#"{"blocks":[{"kind":"formula","text":"\\(x^{2}\\)"}]}"#;
        let mut store = FixtureStore::new();
        store.insert(entry("OCR:", b"page-bytes", output)).unwrap();
        let backend = FixtureBackend::new("m1", "1.0", "ocr.blocks.v1", store);

        let first = backend.replay("OCR:", b"page-bytes").unwrap();
        assert_eq!(first.output_raw.as_bytes(), output.as_bytes());
        for _ in 0..100 {
            let again = backend.replay("OCR:", b"page-bytes").unwrap();
            assert_eq!(
                again.output_raw.as_bytes(),
                first.output_raw.as_bytes(),
                "同输入 100 次回放必须字节一致"
            );
            assert_eq!(again.key, first.key);
        }
    }

    #[test]
    fn different_prompt_or_input_misses_explicitly() {
        let mut store = FixtureStore::new();
        store
            .insert(entry(
                "OCR:",
                b"page-bytes",
                r#"{"blocks":[{"kind":"text","text":"x"}]}"#,
            ))
            .unwrap();
        let backend = FixtureBackend::new("m1", "1.0", "ocr.blocks.v1", store);

        // 换提示词 → 未命中
        let error = backend
            .replay("Formula Recognition:", b"page-bytes")
            .unwrap_err();
        assert!(matches!(error, InferenceError::FixtureMiss { .. }));
        assert_eq!(error.class(), crate::error::FailureClass::Fixture);

        // 换输入 → 未命中
        assert!(backend.replay("OCR:", b"other-bytes").is_err());

        // 换模型版本 → 未命中
        let other = FixtureBackend::new("m1", "2.0", "ocr.blocks.v1", FixtureStore::new());
        assert!(other.replay("OCR:", b"page-bytes").is_err());
    }

    #[test]
    fn fixture_backend_is_marked_as_fixture_data() {
        let backend = FixtureBackend::new("m1", "1.0", "judge.outcome.v1", FixtureStore::new());
        let stamp = backend.engine_stamp(None);
        assert_eq!(stamp.backend, BackendId::fixture());
        assert_eq!(stamp.backend, FixtureBackend::backend_id());
        assert!(!stamp.is_cloud());
    }

    #[test]
    fn load_dir_reads_files_and_rejects_corruption() {
        let directory = tempfile::tempdir().unwrap();
        let output = r#"{"blocks":[{"kind":"text","text":"ok"}]}"#;
        let key = fixture_key("m1", "1.0", "ocr.blocks.v1", "OCR:", b"img");
        let file = format!(
            r#"{{"schema_version":1,"key":"{}","model_id":"m1","model_version":"1.0",
                 "prompt_schema_version":"ocr.blocks.v1","output_raw":{}}}"#,
            key.to_hex(),
            serde_json::to_string(output).unwrap()
        );
        std::fs::write(directory.path().join("a.json"), file).unwrap();
        let store = FixtureStore::load_dir(directory.path()).unwrap();
        assert_eq!(store.len(), 1);

        // 坏 key
        let bad = directory.path().join("b.json");
        std::fs::write(
            &bad,
            r#"{"schema_version":1,"key":"zz","model_id":"m","model_version":"1",
                "prompt_schema_version":"s","output_raw":"{}"}"#,
        )
        .unwrap();
        assert!(FixtureStore::load_dir(directory.path()).is_err());
        std::fs::remove_file(&bad).unwrap();

        // 坏 schema_version
        let wrong_schema = directory.path().join("c.json");
        std::fs::write(
            &wrong_schema,
            r#"{"schema_version":99,"key":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "model_id":"m","model_version":"1","prompt_schema_version":"s","output_raw":"{}"}"#,
        )
        .unwrap();
        assert!(FixtureStore::load_dir(directory.path()).is_err());
    }

    #[test]
    fn duplicate_keys_are_rejected() {
        let mut store = FixtureStore::new();
        let item = entry("OCR:", b"img", r#"{"blocks":[{"kind":"text","text":"x"}]}"#);
        store.insert(item.clone()).unwrap();
        assert!(store.insert(item).is_err());
    }

    #[test]
    fn non_object_output_is_rejected_at_insert() {
        let mut store = FixtureStore::new();
        assert!(store.insert(entry("OCR:", b"img", "[1,2,3]")).is_err());
    }
}
