//! 证据链、比较轨迹与模型调用记录（`SPEC-domain.md` §8）。
//!
//! 不变量：
//! - 每条判分记录至少 1 条证据；模型参与时必须含 `ModelCall`；
//! - 云端调用必须含 `consent_id`（`ModelCall` 层校验）；
//! - 版本号进证据：模型版本、提示词哈希、配置哈希（`EngineStamp`）。

use serde::{Deserialize, Serialize};

use crate::error::{DomainError, invalid_field};
use crate::hash::{AssetId, Sha256Hex};
use crate::ids::{ComparatorTraceId, ConsentId, ModelCallId, RecognitionId, TaskId};
use crate::recognition::{EngineStamp, ModelRole};
use crate::subject::ComparatorVerdict;
use crate::time::Timestamp;
use crate::values::ComparatorId;

/// 证据引用（§8 `EvidenceRef`）：资产、识别结果、比较轨迹、模型调用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceRef {
    /// 资产（原图/裁片/派生图）。
    Asset(AssetId),
    /// 识别结果。
    Recognition(RecognitionId),
    /// 比较轨迹。
    ComparatorTrace(ComparatorTraceId),
    /// 模型调用记录。
    ModelCall(ModelCallId),
}

impl EvidenceRef {
    /// 证据类型名（日志/审计用）。
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Asset(_) => "asset",
            Self::Recognition(_) => "recognition",
            Self::ComparatorTrace(_) => "comparator_trace",
            Self::ModelCall(_) => "model_call",
        }
    }
}

/// 比较轨迹（§8 `ComparatorTrace`）：可复现「为什么判为等价/不等」。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComparatorTrace {
    /// 轨迹 id。
    pub id: ComparatorTraceId,
    /// 比较器 id。
    pub comparator: ComparatorId,
    /// 判定。
    pub verdict: ComparatorVerdict,
    /// 结构化细节（由比较器填写）。
    pub detail: serde_json::Value,
}

impl ComparatorTrace {
    /// 校验轨迹（确定结论必须带原因/说明，结构细节不得为空）。
    ///
    /// # Errors
    ///
    /// 失败返回 [`DomainError`]。
    pub fn validate(&self) -> Result<(), DomainError> {
        match &self.verdict {
            ComparatorVerdict::Equivalent { note } if note.trim().is_empty() => {
                return Err(invalid_field("verdict.note", "等价说明不得为空"));
            }
            ComparatorVerdict::NotEqual { reason } if reason.trim().is_empty() => {
                return Err(invalid_field("verdict.reason", "不相等原因不得为空"));
            }
            ComparatorVerdict::Undecidable { reason } if reason.trim().is_empty() => {
                return Err(invalid_field("verdict.reason", "无法判定原因不得为空"));
            }
            _ => {}
        }
        if self.detail.is_null() {
            return Err(invalid_field("detail", "比较轨迹细节不得为空"));
        }
        Ok(())
    }
}

/// 模型调用记录（§8 `ModelCall`）：按任务鉴权与审计的最小单位。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelCall {
    /// 调用记录 id。
    pub id: ModelCallId,
    /// 模型角色。
    pub role: ModelRole,
    /// 引擎指纹。
    pub engine: EngineStamp,
    /// 所属任务 id（按任务鉴权用）。
    pub task_id: TaskId,
    /// 云端调用授权 id（云端调用必填）。
    pub consent_id: Option<ConsentId>,
    /// 输入哈希（提示词 + 输入内容）。
    pub input_hash: Sha256Hex,
    /// 已通过 Schema 校验的结构化结果。
    pub output: serde_json::Value,
    /// 延迟（毫秒）。
    pub latency_ms: u64,
    /// 费用（百万分之一货币单位）。
    pub cost_micros: u64,
    /// 创建时间。
    pub created_at: Timestamp,
}

impl ModelCall {
    /// 是否为云端调用（`openai_compat` 后端）。
    #[must_use]
    pub fn is_cloud(&self) -> bool {
        self.engine.is_cloud()
    }

    /// 校验调用记录（引擎指纹完整；云端调用必须带 `consent_id`；输出不得为空）。
    ///
    /// # Errors
    ///
    /// 失败返回 [`DomainError`]。
    pub fn validate(&self) -> Result<(), DomainError> {
        self.engine.validate()?;
        if self.engine.role != self.role {
            return Err(invalid_field("role", "调用记录角色必须与引擎指纹角色一致"));
        }
        if self.is_cloud() && self.consent_id.is_none() {
            return Err(DomainError::CloudConsentMissing);
        }
        if self.output.is_null() {
            return Err(invalid_field(
                "output",
                "模型输出必须是通过 Schema 校验的结构化结果",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::ids::Ulid;
    use crate::values::BackendId;

    fn timestamp() -> Timestamp {
        crate::time::parse_rfc3339("2026-11-02T00:00:00Z").unwrap()
    }

    fn engine(role: ModelRole, cloud: bool) -> EngineStamp {
        EngineStamp {
            role,
            backend: if cloud {
                BackendId::openai_compat()
            } else {
                BackendId::local_server()
            },
            model_id: "judge-4b".to_owned(),
            model_version: "1".to_owned(),
            prompt_hash: Some(Sha256Hex::of(b"prompt")),
            params_hash: Sha256Hex::of(b"params"),
        }
    }

    fn model_call(cloud: bool) -> ModelCall {
        ModelCall {
            id: ModelCallId::from_ulid(Ulid::from_u128(1)),
            role: ModelRole::Judge,
            engine: engine(ModelRole::Judge, cloud),
            task_id: TaskId::from_ulid(Ulid::from_u128(2)),
            consent_id: if cloud {
                Some(ConsentId::from_ulid(Ulid::from_u128(3)))
            } else {
                None
            },
            input_hash: Sha256Hex::of(b"input"),
            output: serde_json::json!({"judgement": "match"}),
            latency_ms: 120,
            cost_micros: 0,
            created_at: timestamp(),
        }
    }

    #[test]
    fn evidence_refs_are_tagged() {
        let asset = EvidenceRef::Asset(AssetId::of(b"page"));
        assert_eq!(asset.kind(), "asset");
        let json = serde_json::to_string(&asset).unwrap();
        assert!(json.contains("\"asset\""));
        let call = EvidenceRef::ModelCall(ModelCallId::from_ulid(Ulid::from_u128(1)));
        assert_eq!(call.kind(), "model_call");
        let trace = EvidenceRef::ComparatorTrace(ComparatorTraceId::from_ulid(Ulid::from_u128(2)));
        assert_eq!(trace.kind(), "comparator_trace");
        let recognition = EvidenceRef::Recognition(RecognitionId::from_ulid(Ulid::from_u128(3)));
        assert_eq!(recognition.kind(), "recognition");
    }

    #[test]
    fn model_call_validation() {
        assert!(model_call(false).validate().is_ok());
        assert!(model_call(true).validate().is_ok());

        let mut missing_consent = model_call(true);
        missing_consent.consent_id = None;
        assert!(matches!(
            missing_consent.validate(),
            Err(DomainError::CloudConsentMissing)
        ));

        let mut role_mismatch = model_call(false);
        role_mismatch.role = ModelRole::Ocr;
        assert!(role_mismatch.validate().is_err());

        let mut null_output = model_call(false);
        null_output.output = serde_json::Value::Null;
        assert!(null_output.validate().is_err());
    }

    #[test]
    fn comparator_trace_requires_reasons() {
        let trace = ComparatorTrace {
            id: ComparatorTraceId::from_ulid(Ulid::from_u128(1)),
            comparator: ComparatorId::new("num_abs").unwrap(),
            verdict: ComparatorVerdict::NotEqual {
                reason: "差值超出容差".to_owned(),
            },
            detail: serde_json::json!({"expected": "2.5", "actual": "3"}),
        };
        assert!(trace.validate().is_ok());

        let empty_reason = ComparatorTrace {
            verdict: ComparatorVerdict::NotEqual {
                reason: " ".to_owned(),
            },
            ..trace.clone()
        };
        assert!(empty_reason.validate().is_err());

        let null_detail = ComparatorTrace {
            detail: serde_json::Value::Null,
            ..trace
        };
        assert!(null_detail.validate().is_err());
    }

    #[test]
    fn model_call_serde_roundtrip() {
        let call = model_call(true);
        let json = serde_json::to_string(&call).unwrap();
        let back: ModelCall = serde_json::from_str(&json).unwrap();
        assert_eq!(back, call);
    }
}
