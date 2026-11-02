//! 严格输出 Schema 与结构解析（`SPEC-recognize.md` §2.3、§3.2）。
//!
//! 结构校验的**归属**：`SPEC-recognize.md` §3.2 规定结构校验由 `inference` 执行；
//! 并行实现期间 `eg-inference` 的 OCR 输出契约仍是无 bbox/无原始 JSON 的中间版本，
//! 因此本模块先承载「严格解析」这一段，等 `inference` 的 OCR Schema 定稿后整体迁移，
//! 本模块只保留语义校验与业务映射（迁移点见本文件 TODO）。
//!
//! 严格模式：单对象、拒绝未知字段与未知枚举、数值与长度有上限；错误分为
//! 「可机械修复」（语法/围栏/未知字段/枚举拼写）与「不可机械修复」（缺失字段、
//! 类型错误、上限、契约版本），只有前者会触发修复调用。

use serde::Deserialize;

use crate::error::StructureError;

/// 输出契约版本（当前固定 1）。
pub const SCHEMA_VERSION: u32 = 1;
/// 单页/单段块数量上限。
pub const MAX_BLOCKS: usize = 200;
/// 单块文本长度上限（字符）。
pub const MAX_TEXT_CHARS: usize = 4000;
/// 单块 LaTeX 长度上限（字符）。
pub const MAX_LATEX_CHARS: usize = 2000;

/// 页级声明的图像尺寸（可选字段，用于坐标换算校验）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageDims {
    /// 宽度（像素）。
    pub width: u32,
    /// 高度（像素）。
    pub height: u32,
}

/// 块类型白名单（含保留取值 `Barcode`：结构上可识别、语义上必须拒绝）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireKind {
    /// 正文文本。
    Text,
    /// 公式。
    Formula,
    /// 表格。
    Table,
    /// 手写。
    Handwriting,
    /// 图形。
    Figure,
    /// 印章。
    Stamp,
    /// 页眉页脚。
    HeaderFooter,
    /// 未知。
    Unknown,
    /// 条码（保留变体；识别阶段永不产出，语义校验拒绝）。
    Barcode,
}

/// 解析后的单个块（尚未建领域对象、尚未语义校验）。
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedBlock {
    /// 块类型。
    pub kind: WireKind,
    /// 归一化坐标 `[x, y, 宽, 高]`。
    pub bbox: [f64; 4],
    /// 文本。
    pub text: String,
    /// LaTeX（公式块可为空字符串表示「无」）。
    pub latex: Option<String>,
    /// 模型自评置信度（未经校准）。
    pub confidence: Option<f64>,
}

/// 解析后的模型输出。
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedOutput {
    /// 可选页尺寸声明。
    pub page: Option<PageDims>,
    /// 块列表（模型给出的顺序）。
    pub blocks: Vec<ParsedBlock>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOutput {
    schema_version: u32,
    #[serde(default)]
    page: Option<RawPage>,
    blocks: Vec<RawBlock>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPage {
    width: u32,
    height: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBlock {
    kind: WireKind,
    bbox: [f64; 4],
    #[serde(default)]
    text: String,
    #[serde(default)]
    latex: Option<String>,
    #[serde(default)]
    confidence: Option<f64>,
}

/// 严格解析模型原始输出。
///
/// # Errors
///
/// - [`StructureError::Repairable`]：JSON 语法错误、代码围栏、未知字段、未知枚举；
/// - [`StructureError::Fatal`]：缺失字段、类型错误、数量/长度/置信度越界、版本不符；
/// - [`StructureError::Empty`]：零个块或拒答（空输出）。
pub fn parse_output(raw: &str) -> Result<ParsedOutput, StructureError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(StructureError::Empty);
    }
    if trimmed.starts_with("```") || trimmed.contains("\n```") {
        return Err(StructureError::Repairable(
            "输出包含代码围栏：只允许一个 JSON 对象，不得输出围栏或解释文字".to_owned(),
        ));
    }
    let parsed: RawOutput = serde_json::from_str(trimmed).map_err(|error| {
        let category = error.classify();
        let message = error.to_string();
        let repairable = match category {
            serde_json::error::Category::Syntax | serde_json::error::Category::Eof => true,
            serde_json::error::Category::Data => {
                message.contains("unknown field") || message.contains("unknown variant")
            }
            serde_json::error::Category::Io => false,
        };
        if repairable {
            StructureError::Repairable(message)
        } else {
            StructureError::Fatal(message)
        }
    })?;

    if parsed.schema_version != SCHEMA_VERSION {
        return Err(StructureError::Fatal(format!(
            "schema_version 必须为 {SCHEMA_VERSION}，实际为 {}",
            parsed.schema_version
        )));
    }
    if parsed.blocks.is_empty() {
        return Err(StructureError::Empty);
    }
    if parsed.blocks.len() > MAX_BLOCKS {
        return Err(StructureError::Fatal(format!(
            "blocks 数量 {} 超过上限 {MAX_BLOCKS}",
            parsed.blocks.len()
        )));
    }
    let mut blocks = Vec::with_capacity(parsed.blocks.len());
    for (index, block) in parsed.blocks.into_iter().enumerate() {
        let text_len = block.text.chars().count();
        if text_len > MAX_TEXT_CHARS {
            return Err(StructureError::Fatal(format!(
                "第 {} 个块 text 长度 {text_len} 超过上限 {MAX_TEXT_CHARS}",
                index + 1
            )));
        }
        if let Some(latex) = &block.latex {
            let latex_len = latex.chars().count();
            if latex_len > MAX_LATEX_CHARS {
                return Err(StructureError::Fatal(format!(
                    "第 {} 个块 latex 长度 {latex_len} 超过上限 {MAX_LATEX_CHARS}",
                    index + 1
                )));
            }
        }
        if let Some(confidence) = block.confidence
            && !(0.0..=1.0).contains(&confidence)
        {
            return Err(StructureError::Fatal(format!(
                "第 {} 个块 confidence={confidence} 越界（必须 0 到 1）",
                index + 1
            )));
        }
        if block.bbox.iter().any(|value| !value.is_finite()) {
            return Err(StructureError::Fatal(format!(
                "第 {} 个块 bbox 含非有限值",
                index + 1
            )));
        }
        blocks.push(ParsedBlock {
            kind: block.kind,
            bbox: block.bbox,
            text: block.text,
            latex: block.latex,
            confidence: block.confidence,
        });
    }
    Ok(ParsedOutput {
        page: parsed.page.map(|page| PageDims {
            width: page.width,
            height: page.height,
        }),
        blocks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrap(blocks: &str) -> String {
        format!("{{\"schema_version\":1,\"blocks\":[{blocks}]}}")
    }

    fn block(kind: &str, text: &str) -> String {
        format!(
            "{{\"kind\":\"{kind}\",\"bbox\":[0.1,0.1,0.2,0.1],\"text\":\"{text}\",\"confidence\":0.9}}"
        )
    }

    #[test]
    fn parses_valid_output() {
        let raw = format!(
            "{{\"schema_version\":1,\"page\":{{\"width\":1600,\"height\":2262}},\"blocks\":[{},{}]}}",
            block("text", "1."),
            "{\"kind\":\"formula\",\"bbox\":[0.1,0.2,0.3,0.1],\"text\":\"x+1\",\"latex\":\"x+1\",\"confidence\":0.8}"
        );
        let parsed = parse_output(&raw).unwrap();
        assert_eq!(parsed.blocks.len(), 2);
        assert_eq!(
            parsed.page,
            Some(PageDims {
                width: 1600,
                height: 2262
            })
        );
        assert_eq!(parsed.blocks[1].kind, WireKind::Formula);
        assert_eq!(parsed.blocks[1].latex.as_deref(), Some("x+1"));
    }

    #[test]
    fn rejects_unknown_field_as_repairable() {
        let raw = format!(
            "{{\"schema_version\":1,\"extra\":1,\"blocks\":[{}]}}",
            block("text", "hi")
        );
        let error = parse_output(&raw).unwrap_err();
        assert!(error.is_repairable(), "{error}");
        assert!(error.reason().contains("unknown field"));
    }

    #[test]
    fn rejects_unknown_kind_as_repairable() {
        let raw = wrap(&block("diagram", "x"));
        let error = parse_output(&raw).unwrap_err();
        assert!(error.is_repairable());
        assert!(error.reason().contains("unknown variant"));
    }

    #[test]
    fn rejects_wrong_type_and_missing_field_as_fatal() {
        let wrong_type = wrap("{\"kind\":\"text\",\"bbox\":\"0.1,0.1,0.2,0.1\",\"text\":\"x\"}");
        assert!(!parse_output(&wrong_type).unwrap_err().is_repairable());
        let missing = wrap("{\"kind\":\"text\",\"bbox\":[0.1,0.1,0.2,0.1]}");
        assert_eq!(
            parse_output(&missing).unwrap(),
            ParsedOutput {
                page: None,
                blocks: vec![ParsedBlock {
                    kind: WireKind::Text,
                    bbox: [0.1, 0.1, 0.2, 0.1],
                    text: String::new(),
                    latex: None,
                    confidence: None,
                }]
            }
        );
        let missing_blocks = "{\"schema_version\":1}";
        assert!(!parse_output(missing_blocks).unwrap_err().is_repairable());
    }

    #[test]
    fn empty_outputs_are_empty_result() {
        assert_eq!(parse_output("").unwrap_err(), StructureError::Empty);
        assert_eq!(parse_output("   ").unwrap_err(), StructureError::Empty);
        assert_eq!(parse_output(&wrap("")).unwrap_err(), StructureError::Empty);
    }

    #[test]
    fn rejects_code_fence_as_repairable() {
        let fenced = format!("```json\n{}\n```", wrap(&block("text", "x")));
        assert!(parse_output(&fenced).unwrap_err().is_repairable());
        let trailing = format!("{}\n解释文字", wrap(&block("text", "x")));
        assert!(parse_output(&trailing).unwrap_err().is_repairable());
    }

    #[test]
    fn rejects_limits_and_version_as_fatal() {
        let version = format!(
            "{{\"schema_version\":2,\"blocks\":[{}]}}",
            block("text", "x")
        );
        assert!(!parse_output(&version).unwrap_err().is_repairable());

        let long_text = "字".repeat(MAX_TEXT_CHARS + 1);
        let too_long = wrap(&block("text", &long_text));
        assert!(!parse_output(&too_long).unwrap_err().is_repairable());

        let bad_confidence = wrap(
            "{\"kind\":\"text\",\"bbox\":[0.1,0.1,0.2,0.1],\"text\":\"x\",\"confidence\":1.5}",
        );
        assert!(!parse_output(&bad_confidence).unwrap_err().is_repairable());

        let block_json = block("text", "x");
        let too_many = format!(
            "{{\"schema_version\":1,\"blocks\":[{}]}}",
            std::iter::repeat_n(block_json, MAX_BLOCKS + 1)
                .collect::<Vec<_>>()
                .join(",")
        );
        assert!(!parse_output(&too_many).unwrap_err().is_repairable());
    }

    #[test]
    fn barcode_is_parsed_but_not_rejected_here() {
        // 条码是「结构可识别、语义必拒绝」：结构层放行，语义层给出违规。
        let raw = wrap(&block("barcode", "6901234567890"));
        let parsed = parse_output(&raw).unwrap();
        assert_eq!(parsed.blocks[0].kind, WireKind::Barcode);
    }
}
