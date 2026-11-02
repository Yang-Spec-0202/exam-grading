//! OpenAI 兼容协议层（llama.cpp sidecar 与云端共用）。
//!
//! 只负责**报文构造与解析**，不负责生命周期、授权、预算与记录（在 `local`/`service`）。
//! 依据 `docs/dev/research/inference-serving.md` Q2：`/v1/chat/completions` 接受
//! `content:[{"type":"text","text":"OCR:"},{"type":"image_url","image_url":{"url":"data:image/png;base64,…"}}]`。
//!
//! 严格性（`SPEC-inference.md` §3.3）：
//! - 只接受单个 JSON 对象；Markdown 围栏、说明文字、数组顶层一律判为不合法；
//! - `finish_reason == "length"` 视为**截断**，不得当作完整结果；
//! - 不修复、不补全模型输出。

use serde_json::{Map, Value, json};

use crate::error::InferenceError;
use crate::types::TokenUsage;

/// 一条聊天消息的内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatContent {
    /// 纯文本（判定/分类/规则整理）。
    Text(String),
    /// 文本 + 一张图片（OCR；`image_base64` 是 PNG 字节的 base64）。
    Image {
        /// 文本提示词（如 `OCR:`）。
        text: String,
        /// PNG 图片的 base64（不含前缀）。
        image_base64: String,
    },
}

/// 一条聊天消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    /// 角色：`system`/`user`/`assistant`。
    pub role: &'static str,
    /// 内容。
    pub content: ChatContent,
}

impl ChatMessage {
    /// 系统消息。
    #[must_use]
    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: "system",
            content: ChatContent::Text(text.into()),
        }
    }

    /// 用户文本消息。
    #[must_use]
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: "user",
            content: ChatContent::Text(text.into()),
        }
    }

    /// 用户「文本 + 图片」消息（OCR）。
    #[must_use]
    pub fn user_with_image(text: impl Into<String>, image_base64: impl Into<String>) -> Self {
        Self {
            role: "user",
            content: ChatContent::Image {
                text: text.into(),
                image_base64: image_base64.into(),
            },
        }
    }

    fn to_json(&self) -> Value {
        let content = match &self.content {
            ChatContent::Text(text) => Value::String(text.clone()),
            ChatContent::Image { text, image_base64 } => json!([
                {"type": "text", "text": text},
                {"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{image_base64}")}}
            ]),
        };
        json!({"role": self.role, "content": content})
    }
}

/// 一次 chat completions 请求的参数。
#[derive(Debug, Clone, PartialEq)]
pub struct ChatRequestSpec {
    /// 模型名（本地为 registry id；云端为 `model_name`）。
    pub model: String,
    /// 消息。
    pub messages: Vec<ChatMessage>,
    /// 输出上限（token）。
    pub max_tokens: u32,
    /// 采样温度（0 用于可复现）。
    pub temperature: f64,
    /// 是否要求 JSON 对象输出。
    pub json_object: bool,
}

impl ChatRequestSpec {
    /// 构造请求参数。
    #[must_use]
    pub fn new(model: impl Into<String>, messages: Vec<ChatMessage>, max_tokens: u32) -> Self {
        Self {
            model: model.into(),
            messages,
            max_tokens,
            temperature: 0.0,
            json_object: true,
        }
    }

    /// 关闭 JSON 模式（例如纯文本用例）。
    #[must_use]
    pub fn without_json_mode(mut self) -> Self {
        self.json_object = false;
        self
    }

    /// 序列化为请求 JSON 对象。
    #[must_use]
    pub fn to_body(&self) -> Value {
        let mut body = Map::new();
        body.insert("model".to_owned(), Value::String(self.model.clone()));
        body.insert(
            "messages".to_owned(),
            Value::Array(self.messages.iter().map(ChatMessage::to_json).collect()),
        );
        body.insert("max_tokens".to_owned(), json!(self.max_tokens));
        body.insert("temperature".to_owned(), json!(self.temperature));
        body.insert("stream".to_owned(), Value::Bool(false));
        if self.json_object {
            body.insert("response_format".to_owned(), json!({"type": "json_object"}));
        }
        Value::Object(body)
    }
}

/// 解析后的 chat completions 响应。
#[derive(Debug, Clone, PartialEq)]
pub struct ChatResponse {
    /// 模型返回的文本（`choices[0].message.content`）。
    pub content: String,
    /// `finish_reason`（`stop`/`length`/…）。
    pub finish_reason: Option<String>,
    /// token 用量（服务端未提供时为全 0）。
    pub usage: TokenUsage,
}

impl ChatResponse {
    /// 是否因长度被截断（截断结果不得当作完整结果）。
    #[must_use]
    pub fn is_truncated(&self) -> bool {
        self.finish_reason.as_deref() == Some("length")
    }
}

/// 解析 `/v1/chat/completions` 响应体。
///
/// # Errors
///
/// 非 JSON 对象、缺 `choices`、`content` 不是字符串、空内容、被截断时返回
/// [`InferenceError::SchemaViolation`]。
pub fn parse_chat_response(text: &str) -> Result<ChatResponse, InferenceError> {
    let value: Value =
        serde_json::from_str(text).map_err(|error| InferenceError::SchemaViolation {
            reason: format!("响应不是合法 JSON：{error}"),
        })?;
    let object = value
        .as_object()
        .ok_or_else(|| schema("响应顶层必须是 JSON 对象"))?;
    let choices = object
        .get("choices")
        .and_then(Value::as_array)
        .ok_or_else(|| schema("响应缺少 choices 数组"))?;
    if choices.is_empty() {
        return Err(schema("响应 choices 为空"));
    }
    let choice = choices[0]
        .as_object()
        .ok_or_else(|| schema("choices[0] 必须是对象"))?;
    let finish_reason = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let message = choice
        .get("message")
        .and_then(Value::as_object)
        .ok_or_else(|| schema("choices[0].message 必须是对象"))?;
    let content = message
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| schema("choices[0].message.content 必须是字符串"))?
        .to_owned();
    if content.trim().is_empty() {
        return Err(schema(
            "模型返回空内容（Qwen3 系列若未关闭 thinking 会只返回 reasoning，content 为空）",
        ));
    }
    let usage = object.get("usage").map_or(TokenUsage::ZERO, parse_usage);
    Ok(ChatResponse {
        content,
        finish_reason,
        usage,
    })
}

fn parse_usage(usage: &Value) -> TokenUsage {
    let Some(usage) = usage.as_object() else {
        return TokenUsage::ZERO;
    };
    let prompt = usage
        .get("prompt_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let completion = usage
        .get("completion_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cached = usage
        .get("prompt_tokens_details")
        .and_then(Value::as_object)
        .and_then(|details| details.get("cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    TokenUsage {
        input_tokens: u32::try_from(prompt).unwrap_or(u32::MAX),
        cached_input_tokens: u32::try_from(cached.min(prompt)).unwrap_or(0),
        output_tokens: u32::try_from(completion).unwrap_or(u32::MAX),
    }
}

/// 从严提取模型输出里的**单个 JSON 对象**。
///
/// 拒绝：Markdown 围栏（` ```json `）、前后说明文字、顶层数组/标量、空串。
/// 这是「模型输出一律视为不可信输入」的第一道闸（`SPEC-inference.md` §3.3）。
///
/// # Errors
///
/// 任一不满足即返回 [`InferenceError::SchemaViolation`]。
pub fn extract_json_object(content: &str) -> Result<Value, InferenceError> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err(schema("模型输出为空"));
    }
    if trimmed.starts_with("```") {
        return Err(schema("模型输出包含 Markdown 代码围栏，拒绝"));
    }
    if !trimmed.starts_with('{') {
        return Err(schema("模型输出不是以 { 开头的单个 JSON 对象"));
    }
    if !trimmed.ends_with('}') {
        return Err(schema("模型输出未以 } 结尾（可能被截断或含说明文字）"));
    }
    let value: Value = serde_json::from_str(trimmed).map_err(|error| {
        schema(format!(
            "模型输出不是合法 JSON（可能被截断或含说明文字）：{error}"
        ))
    })?;
    if !value.is_object() {
        return Err(schema("模型输出顶层必须是 JSON 对象"));
    }
    Ok(value)
}

fn schema(reason: impl Into<String>) -> InferenceError {
    InferenceError::SchemaViolation {
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn request_body_matches_openai_shape() {
        let spec = ChatRequestSpec::new(
            "qwen3.5-4b-instruct",
            vec![
                ChatMessage::system("只输出 JSON"),
                ChatMessage::user("判分"),
            ],
            128,
        );
        let body = spec.to_body();
        assert_eq!(body["model"], "qwen3.5-4b-instruct");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["content"], "判分");
        assert_eq!(body["max_tokens"], 128);
        assert_eq!(body["stream"], false);
        assert_eq!(body["response_format"]["type"], "json_object");
    }

    #[test]
    fn ocr_message_uses_multimodal_parts() {
        let message = ChatMessage::user_with_image("OCR:", "QUJD");
        let json = message.to_json();
        assert_eq!(json["content"][0]["type"], "text");
        assert_eq!(json["content"][0]["text"], "OCR:");
        assert_eq!(json["content"][1]["type"], "image_url");
        assert_eq!(
            json["content"][1]["image_url"]["url"],
            "data:image/png;base64,QUJD"
        );
    }

    #[test]
    fn response_parsing_extracts_content_and_usage() {
        let text = r#"{"choices":[{"message":{"content":"{\"score\":3}"},"finish_reason":"stop"}],
                        "usage":{"prompt_tokens":100,"completion_tokens":20,"prompt_tokens_details":{"cached_tokens":40}}}"#;
        let response = parse_chat_response(text).unwrap();
        assert_eq!(response.content, "{\"score\":3}");
        assert!(!response.is_truncated());
        assert_eq!(
            response.usage,
            TokenUsage {
                input_tokens: 100,
                cached_input_tokens: 40,
                output_tokens: 20
            }
        );
        assert_eq!(response.usage.billed_input_tokens(), 60);
    }

    #[test]
    fn truncated_or_empty_content_is_rejected() {
        let truncated =
            r#"{"choices":[{"message":{"content":"{\"score\":"},"finish_reason":"length"}]}"#;
        let response = parse_chat_response(truncated).unwrap();
        assert!(response.is_truncated(), "截断必须被识别");

        let empty = r#"{"choices":[{"message":{"content":""},"finish_reason":"stop"}]}"#;
        let error = parse_chat_response(empty).unwrap_err();
        assert!(matches!(error, InferenceError::SchemaViolation { .. }));
        assert!(error.to_string().contains("thinking"));
    }

    #[test]
    fn malformed_responses_are_rejected() {
        for text in [
            "not json",
            "[]",
            "{}",
            r#"{"choices":[]}"#,
            r#"{"choices":[{"message":{}}]}"#,
            r#"{"choices":[{"message":{"content":42}}]}"#,
        ] {
            let error = parse_chat_response(text).unwrap_err();
            assert!(
                matches!(error, InferenceError::SchemaViolation { .. }),
                "{text} 必须被拒绝"
            );
        }
    }

    #[test]
    fn json_object_extraction_is_strict() {
        assert!(extract_json_object(r#"{"a":1}"#).is_ok());
        let cases = [
            "",
            "   ",
            "```json\n{\"a\":1}\n```",
            "[1,2]",
            "{\"a\":1} 以上是结果",
            "结果如下：{\"a\":1}",
            "{\"a\":",
        ];
        for case in cases {
            assert!(
                extract_json_object(case).is_err(),
                "{case:?} 必须被判为不合法"
            );
        }
    }
}
