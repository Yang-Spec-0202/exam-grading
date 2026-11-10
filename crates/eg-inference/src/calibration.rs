//! 判定置信度校准表（可选产物；`config/calibration/<id>.json`）。
//!
//! 语义：
//! - 表由 `egctl eval --calibrate --write-calibration` 产出，**不含学生数据**；
//! - 只有 `id/model_id/model_version/quantization` 与当前注册表条目完全匹配时才套用；
//! - 表缺失/版本不符/解析失败 → 不套用，保持 uncalibrated（绝不静默用错表）；
//! - 原始 `confidence` 保留在 `JudgeOutput`/`ModelCall`，校准值单独字段。

use std::path::Path;

use eg_domain::Confidence;
use serde::{Deserialize, Serialize};

use crate::error::InferenceError;

/// 校准表文件 schema 版本。
pub const CALIBRATION_SCHEMA_VERSION: u32 = 1;

/// 校准表（JSON 根）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalibrationTable {
    /// 文件 schema 版本。
    #[serde(default = "default_schema")]
    pub schema_version: u32,
    /// 校准 id（写入 JudgeOutcome.calibration_id）。
    pub id: String,
    /// 模型注册表 id（如 `qwen3.5-4b-instruct`）。
    pub model_id: String,
    /// 模型版本（注册表 `version`）。
    pub model_version: String,
    /// 量化（注册表 `quantization`）。
    pub quantization: String,
    /// 拟合方法（如 `equal-frequency+PAV`）。
    pub method: String,
    /// 分桶表（按 `lower` 升序；`calibrated` 为映射概率）。
    pub table: Vec<CalibrationPoint>,
    /// 拟合样本说明（数量/gold hash/seed 等；不含学生数据）。
    #[serde(default)]
    pub fit: serde_json::Value,
    /// 产物生成时间（RFC3339）。
    pub created_at: String,
    /// 金标文件 SHA-256（可追溯）。
    pub gold_sha256: String,
}

fn default_schema() -> u32 {
    CALIBRATION_SCHEMA_VERSION
}

/// 一个校准分桶。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalibrationPoint {
    /// 原始 confidence 下界（含）。
    pub lower: f64,
    /// 原始 confidence 上界（含）。
    pub upper: f64,
    /// 校准后概率。
    pub calibrated: f64,
}

impl CalibrationTable {
    /// 从文件加载并校验（缺失/格式错误返回错误；调用方决定是否忽略）。
    ///
    /// # Errors
    ///
    /// 文件不可读、JSON 非法、版本不符、分桶越界/乱序时返回错误。
    pub fn load(path: &Path) -> Result<Self, InferenceError> {
        let text =
            std::fs::read_to_string(path).map_err(|error| InferenceError::RegistryInvalid {
                reason: format!("校准表 {} 不可读：{error}", path.display()),
            })?;
        let table: Self =
            serde_json::from_str(&text).map_err(|error| InferenceError::RegistryInvalid {
                reason: format!("校准表 {} JSON 非法：{error}", path.display()),
            })?;
        table.validate()?;
        Ok(table)
    }

    /// 校验 schema/分桶单调性。
    ///
    /// # Errors
    ///
    /// 版本不符或分桶非法时返回错误。
    pub fn validate(&self) -> Result<(), InferenceError> {
        if self.schema_version != CALIBRATION_SCHEMA_VERSION {
            return Err(InferenceError::RegistryInvalid {
                reason: format!(
                    "校准表 schema 版本 {} != {CALIBRATION_SCHEMA_VERSION}",
                    self.schema_version
                ),
            });
        }
        if self.id.trim().is_empty() || self.table.is_empty() {
            return Err(InferenceError::RegistryInvalid {
                reason: "校准表 id/table 不得为空".to_owned(),
            });
        }
        let mut previous = -1.0;
        for point in &self.table {
            let valid = (0.0..=1.0).contains(&point.lower)
                && (0.0..=1.0).contains(&point.upper)
                && point.lower <= point.upper
                && (0.0..=1.0).contains(&point.calibrated)
                && point.lower >= previous;
            if !valid {
                return Err(InferenceError::RegistryInvalid {
                    reason: format!(
                        "校准表分桶非法/乱序：lower={} upper={} calibrated={}",
                        point.lower, point.upper, point.calibrated
                    ),
                });
            }
            previous = point.lower;
        }
        Ok(())
    }

    /// 是否与注册表条目身份匹配（id/版本/量化三项全等，大小写不敏感量化）。
    #[must_use]
    pub fn matches(&self, model_id: &str, model_version: &str, quantization: &str) -> bool {
        self.model_id == model_id
            && self.model_version == model_version
            && self.quantization.eq_ignore_ascii_case(quantization)
    }

    /// 将原始 confidence 映射为校准概率；无匹配桶时取最近桶。
    #[must_use]
    pub fn calibrate(&self, confidence: Confidence) -> Option<Confidence> {
        let raw = confidence.get();
        let point = self
            .table
            .iter()
            .find(|point| raw >= point.lower && raw <= point.upper)
            .or_else(|| {
                self.table.iter().min_by(|left, right| {
                    distance(raw, left)
                        .partial_cmp(&distance(raw, right))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
            })?;
        Confidence::new(point.calibrated).ok()
    }
}

fn distance(value: f64, point: &CalibrationPoint) -> f64 {
    if value < point.lower {
        point.lower - value
    } else if value > point.upper {
        value - point.upper
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eg_domain::Confidence;

    fn make_table() -> CalibrationTable {
        CalibrationTable {
            schema_version: 1,
            id: "qwen35-4b-q4-iso-v1".to_owned(),
            model_id: "qwen3.5-4b-instruct".to_owned(),
            model_version: "3.5".to_owned(),
            quantization: "q4_k_m".to_owned(),
            method: "equal-frequency+PAV".to_owned(),
            table: vec![
                CalibrationPoint {
                    lower: 0.0,
                    upper: 0.8,
                    calibrated: 0.4,
                },
                CalibrationPoint {
                    lower: 0.8,
                    upper: 1.0,
                    calibrated: 0.92,
                },
            ],
            fit: serde_json::json!({"n": 180}),
            created_at: "2026-11-10T00:00:00Z".to_owned(),
            gold_sha256: "00".repeat(32),
        }
    }

    #[test]
    fn maps_boundaries_and_nearest_bucket() {
        let table = make_table();
        assert_eq!(
            table
                .calibrate(Confidence::new(0.8).unwrap())
                .unwrap()
                .get(),
            0.4
        );
        assert_eq!(
            table
                .calibrate(Confidence::new(0.95).unwrap())
                .unwrap()
                .get(),
            0.92
        );
        assert_eq!(
            table
                .calibrate(Confidence::new(0.5).unwrap())
                .unwrap()
                .get(),
            0.4
        );
        assert_eq!(
            table
                .calibrate(Confidence::new(1.0).unwrap())
                .unwrap()
                .get(),
            0.92
        );
    }

    #[test]
    fn rejects_wrong_schema_or_non_monotone() {
        let mut table = make_table();
        table.schema_version = 2;
        assert!(table.validate().is_err());
        let mut table = make_table();
        table.table[0].calibrated = 1.5;
        assert!(table.validate().is_err());
        let mut table = make_table();
        table.table[0].lower = 0.5;
        table.table[1].lower = 0.1;
        assert!(table.validate().is_err());
    }

    #[test]
    fn matches_identity_and_rejects_mismatch() {
        let table = make_table();
        assert!(table.matches("qwen3.5-4b-instruct", "3.5", "q4_k_m"));
        assert!(!table.matches("qwen3.5-4b-instruct", "9.9", "q4_k_m"));
        assert!(!table.matches("other", "3.5", "q4_k_m"));
        assert!(!table.matches("qwen3.5-4b-instruct", "3.5", "q8_0"));
    }

    #[test]
    fn loads_repository_calibration_artifact_and_matches_judge_entry() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../config/calibration/qwen35-4b-q4-iso-v1.json");
        let table = CalibrationTable::load(&path).expect("仓库校准表应可加载");
        assert_eq!(table.id, "qwen35-4b-q4-iso-v1");
        assert!(table.matches("qwen3.5-4b-instruct", "3.5", "q4_k_m"));
        let calibrated = table
            .calibrate(Confidence::new(0.95).unwrap())
            .expect("0.95 应命中校准桶");
        assert!((calibrated.get() - 0.919_831_223_628_692).abs() < 1e-9);
    }

    #[test]
    fn missing_file_is_error_but_caller_can_ignore() {
        assert!(CalibrationTable::load(Path::new("/nonexistent/cal.json")).is_err());
    }
}
