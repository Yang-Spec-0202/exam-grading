//! 识别结果、识别块与引擎指纹（`SPEC-domain.md` §7、§8）。
//!
//! 规则（§7）：识别结果是**不可信输入**；公式必须能解析为合法 LaTeX 才进入学科包，
//! 解析失败按「不可判定」处理并进人工，不得猜测。本模块只承载结构与「公式块必须
//! 携带 LaTeX 文本」这一可判定约束；LaTeX 语法合法性由 `recognize` 校验。

use serde::{Deserialize, Serialize};

use crate::confidence::Confidence;
use crate::error::{DomainError, invalid_field};
use crate::geometry::BBox;
use crate::hash::Sha256Hex;
use crate::ids::{PageId, RecognitionId, SegmentId};
use crate::time::Timestamp;
use crate::values::BackendId;

/// 模型角色（§8 `ModelRole`：`ocr`/`router`/`judge`/`structurer`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
    /// OCR/文档解析。
    Ocr,
    /// L1 路由分类澄清。
    Router,
    /// L2 判定。
    Judge,
    /// 规则整理（结构化）。
    Structurer,
}

/// 识别结果（§7 `Recognition`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recognition {
    /// 识别结果 id。
    pub id: RecognitionId,
    /// 所属页面 id。
    pub page_id: PageId,
    /// 所属答案区域 id（页级识别为空）。
    pub segment_id: Option<SegmentId>,
    /// 识别块（按阅读顺序）。
    pub blocks: Vec<Block>,
    /// 拼接纯文本。
    pub text: String,
    /// 引擎指纹。
    pub engine: EngineStamp,
    /// 识别置信度。
    pub confidence: Confidence,
    /// 创建时间。
    pub created_at: Timestamp,
}

impl Recognition {
    /// 校验识别结果（块合法、公式块带 LaTeX、引擎指纹完整）。
    ///
    /// # Errors
    ///
    /// 失败返回 [`DomainError`]。
    pub fn validate(&self) -> Result<(), DomainError> {
        self.engine.validate()?;
        for (index, block) in self.blocks.iter().enumerate() {
            block.validate().map_err(|err| match err {
                DomainError::InvalidField { field, reason } => {
                    invalid_field(field, format!("第 {} 个识别块：{reason}", index + 1))
                }
                other => other,
            })?;
        }
        Ok(())
    }

    /// 公式块数量（无公式块时可直接走文本路径）。
    #[must_use]
    pub fn formula_block_count(&self) -> usize {
        self.blocks
            .iter()
            .filter(|block| block.kind == BlockKind::Formula)
            .count()
    }
}

/// 识别块（§7 `Block`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Block {
    /// 块类型。
    pub kind: BlockKind,
    /// 归一化坐标框。
    pub bbox: BBox,
    /// 文本。
    pub text: String,
    /// LaTeX（可空；公式块必填）。
    pub latex: Option<String>,
    /// 块级置信度。
    pub confidence: Confidence,
}

impl Block {
    /// 校验识别块（坐标合法；公式块必须带非空 LaTeX；LaTeX 若存在不得为空串）。
    ///
    /// # Errors
    ///
    /// 失败返回 [`DomainError`]。
    pub fn validate(&self) -> Result<(), DomainError> {
        self.bbox.validate()?;
        if let Some(latex) = &self.latex
            && latex.trim().is_empty()
        {
            return Err(invalid_field(
                "latex",
                "LaTeX 不得为空字符串（应省略该字段）",
            ));
        }
        if self.kind == BlockKind::Formula && self.latex.as_deref().is_none_or(str::is_empty) {
            return Err(invalid_field(
                "latex",
                "公式块必须携带 LaTeX 文本（否则按不可判定进人工）",
            ));
        }
        Ok(())
    }
}

/// 块类型（§7 `BlockKind`；`Barcode` 为保留变体，v1 不由 OCR 产出）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    /// 文本。
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
    /// 条码（保留，v1 不产出）。
    Barcode,
    /// 页眉页脚。
    HeaderFooter,
    /// 未知。
    Unknown,
}

/// 引擎指纹（§8 `EngineStamp`）：版本进证据，缺一不可复现。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineStamp {
    /// 模型角色。
    pub role: ModelRole,
    /// 后端 id（保留字 `local_server`/`openai_compat`/`fixture`）。
    pub backend: BackendId,
    /// 模型 id。
    pub model_id: String,
    /// 模型版本。
    pub model_version: String,
    /// 提示词哈希（可空）。
    pub prompt_hash: Option<Sha256Hex>,
    /// 参数/配置哈希。
    pub params_hash: Sha256Hex,
}

impl EngineStamp {
    /// 校验引擎指纹（模型 id/版本非空）。
    ///
    /// # Errors
    ///
    /// 失败返回 [`DomainError`]。
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.model_id.trim().is_empty() {
            return Err(invalid_field("model_id", "模型 id 不得为空"));
        }
        if self.model_version.trim().is_empty() {
            return Err(invalid_field("model_version", "模型版本不得为空"));
        }
        Ok(())
    }

    /// 是否为云端后端（`ModelCall.consent_id` 必填的依据）。
    #[must_use]
    pub fn is_cloud(&self) -> bool {
        self.backend.is_cloud()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::ids::Ulid;

    fn timestamp() -> Timestamp {
        crate::time::parse_rfc3339("2026-11-02T00:00:00Z").unwrap()
    }

    fn engine() -> EngineStamp {
        EngineStamp {
            role: ModelRole::Ocr,
            backend: BackendId::local_server(),
            model_id: "paddleocr-vl".to_owned(),
            model_version: "1.6-q4".to_owned(),
            prompt_hash: Some(Sha256Hex::of(b"prompt")),
            params_hash: Sha256Hex::of(b"params"),
        }
    }

    fn block(kind: BlockKind) -> Block {
        Block {
            kind,
            bbox: BBox::new(0.0, 0.0, 0.5, 0.2).unwrap(),
            text: "解：x=1".to_owned(),
            latex: Some("x=1".to_owned()),
            confidence: Confidence::new(0.9).unwrap(),
        }
    }

    #[test]
    fn valid_recognition_passes() {
        let recognition = Recognition {
            id: RecognitionId::from_ulid(Ulid::from_u128(1)),
            page_id: PageId::from_ulid(Ulid::from_u128(2)),
            segment_id: None,
            blocks: vec![block(BlockKind::Text), block(BlockKind::Formula)],
            text: "解：x=1".to_owned(),
            engine: engine(),
            confidence: Confidence::new(0.92).unwrap(),
            created_at: timestamp(),
        };
        assert!(recognition.validate().is_ok());
        assert_eq!(recognition.formula_block_count(), 1);
    }

    #[test]
    fn formula_block_requires_latex() {
        let mut formula = block(BlockKind::Formula);
        formula.latex = None;
        assert!(formula.validate().is_err());
        formula.latex = Some("  ".to_owned());
        assert!(formula.validate().is_err());
        formula.latex = Some("x^2".to_owned());
        assert!(formula.validate().is_ok());
    }

    #[test]
    fn empty_latex_is_rejected_even_for_text_block() {
        let mut text = block(BlockKind::Text);
        text.latex = Some(String::new());
        assert!(text.validate().is_err());
        text.latex = None;
        assert!(text.validate().is_ok());
    }

    #[test]
    fn engine_stamp_requires_model_identity() {
        let mut stamp = engine();
        stamp.model_id = " ".to_owned();
        assert!(stamp.validate().is_err());
        let mut stamp = engine();
        stamp.model_version = String::new();
        assert!(stamp.validate().is_err());
        assert!(!engine().is_cloud());
        let cloud = EngineStamp {
            backend: BackendId::openai_compat(),
            ..engine()
        };
        assert!(cloud.is_cloud());
    }

    #[test]
    fn serde_tags_match_reserved_words() {
        assert_eq!(serde_json::to_string(&ModelRole::Ocr).unwrap(), "\"ocr\"");
        assert_eq!(
            serde_json::to_string(&ModelRole::Structurer).unwrap(),
            "\"structurer\""
        );
        assert_eq!(
            serde_json::to_string(&BlockKind::HeaderFooter).unwrap(),
            "\"header_footer\""
        );
        assert_eq!(
            serde_json::to_string(&BlockKind::Barcode).unwrap(),
            "\"barcode\""
        );
    }
}
