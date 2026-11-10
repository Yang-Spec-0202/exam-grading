//! `JudgeBackend`/`ClassifierBackend` ← `eg-inference`（注册表/严格 Schema/超时/预算/授权）。
//!
//! 真实调用一次即由推理服务完成：注册表解析、上下文/预算/授权/熔断门禁、Schema 校验、
//! `ModelCall` 落库与审计。本适配器只做「判定请求 ↔ CompletionRequest」与结果映射；
//! 已落库的 `ModelCall` 通过 `JudgeResponse.model_call` 回传，grade 不再重复落库。

use std::sync::Arc;
use std::time::Duration;

use eg_domain::{EngineStamp, MilliScore, Question, QuestionKind, RubricItemId, TaskId};
use eg_inference::{
    ChatMessage, ChatRequestSpec, CompletionRequest, ExpectedSchema, InferenceError,
    InferenceService, JudgeOutput, JudgeTag, ValidatedOutput,
};

use crate::ports::{
    ClassificationResponse, ClassifierBackend, JudgeBackend, JudgeFailure, JudgeRequest,
    JudgeResponse, JudgeVerdict,
};

/// 判定提示词 Schema 版本（进引擎指纹与夹具键）。
pub const JUDGE_PROMPT_SCHEMA_VERSION: &str = "judge.outcome.v1";
/// 路由提示词 Schema 版本。
pub const ROUTER_PROMPT_SCHEMA_VERSION: &str = "router.kind.v1";

const JUDGE_SYSTEM_PROMPT: &str = "你是本地判分助手。只输出 JSON 对象，字段为 judgement(match|partial|mismatch)、score_milli、reason、confidence、matched_alternative、calibration_id、satisfied、missing。禁止输出任何其他文本或猜测答案。";
const ROUTER_SYSTEM_PROMPT: &str = "你是本地题型分类助手。只输出 JSON 对象，字段为 decision、confidence。decision 必须来自给定允许集合。";

fn model_version(service: &InferenceService, model_id: &str) -> Result<String, JudgeFailure> {
    service
        .registry()
        .require(model_id)
        .map(|entry| entry.version.clone())
        .map_err(|error| JudgeFailure::Internal {
            reason: format!("模型 {model_id} 未在注册表：{error}"),
        })
}

/// 判定适配器。
#[derive(Clone)]
pub struct InferenceJudge {
    service: Arc<InferenceService>,
    model_id: String,
    model_version: String,
    max_tokens: u32,
}

impl std::fmt::Debug for InferenceJudge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InferenceJudge")
            .field("model_id", &self.model_id)
            .field("model_version", &self.model_version)
            .finish_non_exhaustive()
    }
}

impl InferenceJudge {
    /// 构造（模型必须已在注册表登记；未登记直接返回错误，不静默回退）。
    ///
    /// # Errors
    ///
    /// 模型未登记时返回 [`JudgeFailure::Internal`]。
    pub fn new(
        service: Arc<InferenceService>,
        model_id: impl Into<String>,
        max_tokens: u32,
    ) -> Result<Self, JudgeFailure> {
        let model_id = model_id.into();
        let model_version = model_version(&service, &model_id)?;
        Ok(Self {
            service,
            model_id,
            model_version,
            max_tokens,
        })
    }

    /// 引擎指纹文本（注册表指纹 + 模型版本 + 提示词版本；变化即缓存失效）。
    #[must_use]
    pub fn engine_fingerprint(&self) -> String {
        format!(
            "inference-judge-v1:{}:{}:{}:{}:{}",
            self.service.backend_id(),
            self.model_id,
            self.model_version,
            JUDGE_PROMPT_SCHEMA_VERSION,
            self.service.registry().fingerprint().to_hex()
        )
    }

    fn request_parts(&self, request: &JudgeRequest<'_>) -> (String, Vec<u8>, ChatRequestSpec) {
        let (system, user, input) = build_judge_prompt(request);
        let chat = ChatRequestSpec::new(
            self.model_id.clone(),
            vec![ChatMessage::system(system), ChatMessage::user(user.clone())],
            self.max_tokens,
        );
        (user, input, chat)
    }
}

/// 构造判定提示词（system、user、输入字节）；测试用同一函数计算夹具键。
#[must_use]
pub fn build_judge_prompt(request: &JudgeRequest<'_>) -> (String, String, Vec<u8>) {
    let payload = serde_json::json!({
        "prompt_version": JUDGE_PROMPT_SCHEMA_VERSION,
        "question": {
            "id": request.question.id.to_string(),
            "label": request.question.label,
            "kind": format!("{:?}", request.question.kind),
            "max_score_milli": request.question.max_score.as_milli(),
        },
        "answer": {
            "text": request.answer.text,
            "latex": request.answer.latex,
        },
        "reference": reference_json(
            request.key,
            request.normalized.map(|normalized| normalized.text.clone()),
        ),
        "rubric": rubric_json(request.rubric),
        "constraints": {
            "granularity_milli": request.granularity.as_milli(),
            "max_reason_chars": request.max_reason_chars,
            "attempt": request.attempt,
            "repair_hint": request.repair_hint,
        },
    });
    let user = payload.to_string();
    let input = format!("{JUDGE_SYSTEM_PROMPT}\n{user}").into_bytes();
    (JUDGE_SYSTEM_PROMPT.to_owned(), user, input)
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "归一化文本按值快照进提示词 JSON"
)]
fn reference_json(
    key: Option<&eg_domain::ConcreteKey>,
    normalized_text: Option<String>,
) -> serde_json::Value {
    match key {
        Some(key) => serde_json::json!({
            "primary": key.primary,
            "alternatives": key.alternatives,
            "comparators": key.comparators,
        }),
        None => serde_json::json!({ "student_text": normalized_text }),
    }
}

fn rubric_json(rubric: Option<&eg_domain::Rubric>) -> serde_json::Value {
    match rubric {
        Some(rubric) => serde_json::json!({
            "id": rubric.id.to_string(),
            "version": rubric.version,
            "mode": format!("{:?}", rubric.mode),
            "items": rubric.items.iter().map(|item| serde_json::json!({
                "id": item.id.to_string(),
                "criterion": item.criterion,
                "points_milli": item.points.as_milli(),
                "required": item.required,
            })).collect::<Vec<_>>(),
        }),
        None => serde_json::Value::Null,
    }
}

fn parse_item_ids(values: &[String]) -> Result<Vec<RubricItemId>, JudgeFailure> {
    values
        .iter()
        .map(|value| {
            RubricItemId::parse(value).map_err(|error| JudgeFailure::InvalidOutput {
                reason: format!("评分项 id 不合法：{value}（{error}）"),
            })
        })
        .collect()
}

impl JudgeBackend for InferenceJudge {
    fn identity(&self) -> String {
        self.engine_fingerprint()
    }

    fn judge(&self, request: &JudgeRequest<'_>) -> Result<JudgeResponse, JudgeFailure> {
        let (prompt, input, chat) = self.request_parts(request);
        let mut completion =
            CompletionRequest::new(request.task_id, self.model_id.clone(), prompt, input, chat);
        completion.expected = ExpectedSchema::Judge {
            max_score: request.question.max_score,
        };
        completion.timeout = Some(Duration::from_secs(u64::from(
            request.judge_timeout_s.max(1),
        )));
        let outcome = self
            .service
            .complete(completion)
            .map_err(map_inference_error)?;
        let ValidatedOutput::Judge(output) = outcome.validated else {
            return Err(JudgeFailure::InvalidOutput {
                reason: "推理服务返回了非判定 Schema 的产出".to_owned(),
            });
        };
        judge_response_from_output(
            &output,
            outcome.engine,
            outcome.model_call,
            outcome.cost_micros,
        )
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "判定输出按值映射为响应并保留字段所有权"
)]
fn judge_response_from_output(
    output: &JudgeOutput,
    engine: EngineStamp,
    model_call: eg_domain::ModelCall,
    cost_micros: u64,
) -> Result<JudgeResponse, JudgeFailure> {
    let verdict = match output.judgement {
        JudgeTag::Match => JudgeVerdict::Match,
        JudgeTag::Partial => JudgeVerdict::Partial,
        JudgeTag::Mismatch => JudgeVerdict::Mismatch,
    };
    Ok(JudgeResponse {
        model_call: Some(model_call.clone()),
        verdict,
        awarded: Some(MilliScore::from_milli(output.score_milli).map_err(|error| {
            JudgeFailure::InvalidOutput {
                reason: format!("模型得分非法：{error}"),
            }
        })?),
        satisfied: parse_item_ids(&output.satisfied)?,
        missing: parse_item_ids(&output.missing)?,
        matched_alternative: output.matched_alternative,
        reason: output.reason.clone(),
        confidence: output.confidence,
        calibration_id: output.calibration_id.clone(),
        calibrated_confidence: output.calibrated_confidence,
        calibration_verified: output.calibration_verified,
        evidence_confidence: Some(output.confidence),
        engine,
        latency_ms: model_call.latency_ms,
        cost_micros,
        output: serde_json::json!({
            "judgement": format!("{:?}", output.judgement).to_lowercase(),
            "score_milli": output.score_milli,
            "reason": output.reason,
            "confidence": output.confidence.get(),
            "calibrated_confidence": output.calibrated_confidence.map(eg_domain::Confidence::get),
            "calibration_id": output.calibration_id.as_ref().map(ToString::to_string),
            "calibration_verified": output.calibration_verified,
            "matched_alternative": output.matched_alternative,
            "satisfied": output.satisfied,
            "missing": output.missing,
        }),
    })
}

/// 分类适配器（R9；router 角色）。
#[derive(Clone)]
pub struct InferenceClassifier {
    service: Arc<InferenceService>,
    model_id: String,
    model_version: String,
    max_tokens: u32,
}

impl std::fmt::Debug for InferenceClassifier {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InferenceClassifier")
            .field("model_id", &self.model_id)
            .finish_non_exhaustive()
    }
}

impl InferenceClassifier {
    /// 构造。
    ///
    /// # Errors
    ///
    /// 模型未登记时返回 [`JudgeFailure::Internal`]。
    pub fn new(
        service: Arc<InferenceService>,
        model_id: impl Into<String>,
        max_tokens: u32,
    ) -> Result<Self, JudgeFailure> {
        let model_id = model_id.into();
        let model_version = model_version(&service, &model_id)?;
        Ok(Self {
            service,
            model_id,
            model_version,
            max_tokens,
        })
    }

    /// 引擎指纹。
    #[must_use]
    pub fn engine_fingerprint(&self) -> String {
        format!(
            "inference-router-v1:{}:{}:{}:{}:{}",
            self.service.backend_id(),
            self.model_id,
            self.model_version,
            ROUTER_PROMPT_SCHEMA_VERSION,
            self.service.registry().fingerprint().to_hex()
        )
    }
}

/// 题型 → router 标签。
#[must_use]
pub const fn kind_label(kind: QuestionKind) -> &'static str {
    match kind {
        QuestionKind::SingleChoice => "single_choice",
        QuestionKind::MultipleChoice => "multiple_choice",
        QuestionKind::TrueFalse => "true_false",
        QuestionKind::FillInBlank => "fill_in_blank",
        QuestionKind::NumericOnly => "numeric_only",
        QuestionKind::Calculation => "calculation",
        QuestionKind::Proof => "proof",
        QuestionKind::ShortAnswer => "short_answer",
        QuestionKind::Essay => "essay",
        QuestionKind::Other => "other",
    }
}

/// router 标签 → 题型。
#[must_use]
pub fn kind_from_label(label: &str) -> Option<QuestionKind> {
    [
        QuestionKind::SingleChoice,
        QuestionKind::MultipleChoice,
        QuestionKind::TrueFalse,
        QuestionKind::FillInBlank,
        QuestionKind::NumericOnly,
        QuestionKind::Calculation,
        QuestionKind::Proof,
        QuestionKind::ShortAnswer,
        QuestionKind::Essay,
        QuestionKind::Other,
    ]
    .into_iter()
    .find(|kind| kind_label(*kind) == label)
}

/// 构造分类提示词（system、user、输入字节）。
#[must_use]
pub fn build_router_prompt(question: &Question) -> (String, String, Vec<u8>) {
    let allowed: Vec<&str> = [
        QuestionKind::SingleChoice,
        QuestionKind::MultipleChoice,
        QuestionKind::TrueFalse,
        QuestionKind::FillInBlank,
        QuestionKind::NumericOnly,
        QuestionKind::Calculation,
        QuestionKind::Proof,
        QuestionKind::ShortAnswer,
        QuestionKind::Essay,
        QuestionKind::Other,
    ]
    .into_iter()
    .map(kind_label)
    .collect();
    let payload = serde_json::json!({
        "prompt_version": ROUTER_PROMPT_SCHEMA_VERSION,
        "question": {
            "id": question.id.to_string(),
            "label": question.label,
            "stem": question.stem,
        },
        "allowed": allowed,
    });
    let user = payload.to_string();
    let input = format!("{ROUTER_SYSTEM_PROMPT}\n{user}").into_bytes();
    (ROUTER_SYSTEM_PROMPT.to_owned(), user, input)
}

impl ClassifierBackend for InferenceClassifier {
    fn identity(&self) -> String {
        self.engine_fingerprint()
    }

    fn classify(
        &self,
        question: &Question,
        task_id: TaskId,
    ) -> Result<ClassificationResponse, JudgeFailure> {
        let (system, user, input) = build_router_prompt(question);
        let (_, _, _) = (system, user.clone(), input.clone());
        let chat = ChatRequestSpec::new(
            self.model_id.clone(),
            vec![
                ChatMessage::system(ROUTER_SYSTEM_PROMPT),
                ChatMessage::user(user.clone()),
            ],
            self.max_tokens,
        );
        let allowed: Vec<String> = [
            QuestionKind::SingleChoice,
            QuestionKind::MultipleChoice,
            QuestionKind::TrueFalse,
            QuestionKind::FillInBlank,
            QuestionKind::NumericOnly,
            QuestionKind::Calculation,
            QuestionKind::Proof,
            QuestionKind::ShortAnswer,
            QuestionKind::Essay,
            QuestionKind::Other,
        ]
        .into_iter()
        .map(|kind| kind_label(kind).to_owned())
        .collect();
        let mut completion =
            CompletionRequest::new(task_id, self.model_id.clone(), user, input, chat);
        completion.expected = ExpectedSchema::Router {
            allowed: allowed.clone(),
        };
        let outcome = self
            .service
            .complete(completion)
            .map_err(map_inference_error)?;
        let ValidatedOutput::Router(output) = outcome.validated else {
            return Err(JudgeFailure::InvalidOutput {
                reason: "推理服务返回了非路由 Schema 的产出".to_owned(),
            });
        };
        let kind =
            kind_from_label(&output.decision).ok_or_else(|| JudgeFailure::InvalidOutput {
                reason: format!("未知题型标签：{}", output.decision),
            })?;
        Ok(ClassificationResponse {
            model_call: Some(outcome.model_call),
            kind,
            confidence: output.confidence,
            engine: outcome.engine,
            latency_ms: 0,
        })
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "InferenceError 按值分类映射，便于保留字符串信息"
)]
fn map_inference_error(error: InferenceError) -> JudgeFailure {
    match error {
        InferenceError::Timeout { .. } => JudgeFailure::Timeout,
        InferenceError::SchemaViolation { reason } => JudgeFailure::InvalidOutput { reason },
        InferenceError::BackendUnavailable { .. }
        | InferenceError::Transport { .. }
        | InferenceError::Transient { .. }
        | InferenceError::CircuitOpen { .. } => JudgeFailure::BackendUnavailable {
            reason: error.to_string(),
        },
        InferenceError::CloudDisabled
        | InferenceError::ConsentMissing { .. }
        | InferenceError::ConsentInactive { .. }
        | InferenceError::ScopeMismatch
        | InferenceError::ConsentBudgetExhausted { .. }
        | InferenceError::BudgetExceeded { .. }
        | InferenceError::LicenseDenied { .. }
        | InferenceError::LicenseReviewRequired { .. } => JudgeFailure::PolicyBlocked {
            reason: error.to_string(),
        },
        _ => JudgeFailure::Internal {
            reason: error.to_string(),
        },
    }
}
