//! 严格输出 Schema 校验（`SPEC-inference.md` §3.3）。
//!
//! 模型输出一律视为不可信输入。本模块只做**形状与边界校验**，不修复、不补全、不改写：
//! - 只接受单个对象；未知字段、未知枚举值一律拒绝；
//! - 数量与长度有上限（块数、文本长度、评分项数量、依赖序号范围）；
//! - 判定分数必须与满分一致（`match`=满分、`mismatch`=0、`partial` 在开区间）；
//! - 规则整理的依赖不得成环、不得自引用；
//! - **LaTeX 字符串原样透传**（本模块不做归一化/修复，归 `recognize`）。
//!
//! 判定只接受三个**可判定**标签：不可判定（`ungradable`）由调用方按
//! `SPEC-inference.md` §5 的失败映射构造，不让模型自选领域原因（避免审计原因被模型决定）。

use std::collections::BTreeSet;

use eg_domain::{CalibrationId, Confidence, JudgeOutcome, JudgedUnit, Judgement, MilliScore};
use serde_json::{Map, Value};

use crate::error::InferenceError;

/// OCR 单页返回的块数上限。
pub const MAX_OCR_BLOCKS: usize = 500;
/// OCR 单块文本长度上限（字符）。
pub const MAX_BLOCK_TEXT_CHARS: usize = 20_000;
/// 判定理由长度上限（字符）。
pub const MAX_REASON_CHARS: usize = 1_000;
/// 规则整理草稿项数量上限。
pub const MAX_STRUCTURER_ITEMS: usize = 100;
/// 规则整理单项分值上限（千分之一分：100 分）。
pub const MAX_ITEM_SCORE_MILLI: i64 = 100_000;
/// 规则整理全部项分值合计上限（千分之一分：1000 分）。
pub const MAX_TOTAL_SCORE_MILLI: i64 = 1_000_000;

/// 判定标签（可判定部分）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JudgeTag {
    /// 一致（必须满分）。
    Match,
    /// 部分一致（开区间）。
    Partial,
    /// 不一致（必须 0 分）。
    Mismatch,
}

impl JudgeTag {
    /// 解析模型给出的标签。
    fn parse(text: &str) -> Option<Self> {
        match text {
            "match" => Some(Self::Match),
            "partial" => Some(Self::Partial),
            "mismatch" => Some(Self::Mismatch),
            _ => None,
        }
    }

    /// 规范文本。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Match => "match",
            Self::Partial => "partial",
            Self::Mismatch => "mismatch",
        }
    }
}

/// 校验后的判定产出（校准前置信度；`JudgeOutcome` 的严格前置形态）。
#[derive(Debug, Clone, PartialEq)]
pub struct JudgeOutput {
    /// 判定标签。
    pub judgement: JudgeTag,
    /// 得分（千分之一分）。
    pub score_milli: i64,
    /// 中文理由。
    pub reason: String,
    /// 校准前置信度。
    pub confidence: Confidence,
    /// 命中的候选序号（多选/多候选场景）。
    pub matched_alternative: Option<u32>,
    /// 校准 id（为空时调用方不得自动放行）。
    pub calibration_id: Option<CalibrationId>,
    /// 校准后置信度（推断层按可信表填充；原始 confidence 保留）。
    pub calibrated_confidence: Option<Confidence>,
    /// 是否已由可信校准表验证（防止模型伪报 id）。
    pub calibration_verified: bool,
    /// 满足项（评分项/选项标识，`partial` 时可能有）。
    pub satisfied: Vec<String>,
    /// 缺失项（评分项/选项标识，`partial` 时可能有）。
    pub missing: Vec<String>,
}

impl JudgeOutput {
    /// 校验模型输出为判定产出。
    ///
    /// # Errors
    ///
    /// 未知字段、类型错误、分数与满分不一致、理由为空、置信度越界时返回
    /// [`InferenceError::SchemaViolation`]。
    pub fn validate(value: &Value, max_score: MilliScore) -> Result<Self, InferenceError> {
        let object = object_with_allowed(
            value,
            "judge",
            &[
                "judgement",
                "score_milli",
                "reason",
                "confidence",
                "matched_alternative",
                "calibration_id",
                "satisfied",
                "missing",
            ],
        )?;
        let tag_text = require_str(object, "judgement", "judge")?;
        let tag = JudgeTag::parse(tag_text).ok_or_else(|| {
            schema(format!(
                "judgement 只能是 match/partial/mismatch（实际 {tag_text:?}）；\
                 不可判定由调用方按失败映射构造"
            ))
        })?;

        let score_milli = required_i64(object, "score_milli", "judge")?;
        let max_milli = max_score.as_milli();
        match tag {
            JudgeTag::Match if score_milli != max_milli => {
                return Err(schema(format!(
                    "match 判定必须给满分 {max_milli}，实际 {score_milli}"
                )));
            }
            JudgeTag::Mismatch if score_milli != 0 => {
                return Err(schema(format!(
                    "mismatch 判定必须给 0 分，实际 {score_milli}"
                )));
            }
            JudgeTag::Partial if score_milli <= 0 || score_milli >= max_milli => {
                return Err(schema(format!(
                    "partial 判定得分必须在 (0, {max_milli}) 开区间，实际 {score_milli}"
                )));
            }
            _ => {}
        }
        MilliScore::from_milli(score_milli)
            .map_err(|error| schema(format!("score_milli 非法：{error}")))?;

        let reason = require_str(object, "reason", "judge")?.to_owned();
        if reason.trim().is_empty() {
            return Err(schema("reason 不得为空"));
        }
        if reason.chars().count() > MAX_REASON_CHARS {
            return Err(schema(format!("reason 超过 {MAX_REASON_CHARS} 字符上限")));
        }

        let confidence_value = required_f64(object, "confidence", "judge")?;
        let confidence = Confidence::new(confidence_value)
            .map_err(|error| schema(format!("confidence 非法：{error}")))?;

        let matched_alternative = match object.get("matched_alternative") {
            None | Some(Value::Null) => None,
            Some(value) => {
                let number = value
                    .as_u64()
                    .ok_or_else(|| schema("matched_alternative 必须是非负整数"))?;
                Some(u32::try_from(number).map_err(|_| schema("matched_alternative 过大"))?)
            }
        };
        let calibration_id = match object.get("calibration_id") {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) => Some(
                CalibrationId::new(text.clone())
                    .map_err(|error| schema(format!("calibration_id 非法：{error}")))?,
            ),
            Some(_) => return Err(schema("calibration_id 必须是字符串")),
        };
        let satisfied = optional_str_array(object, "satisfied", "judge")?;
        let missing = optional_str_array(object, "missing", "judge")?;
        if tag != JudgeTag::Partial && (!satisfied.is_empty() || !missing.is_empty()) {
            return Err(schema("只有 partial 判定可以携带 satisfied/missing 列表"));
        }
        if tag == JudgeTag::Partial && satisfied.is_empty() && missing.is_empty() {
            return Err(schema(
                "partial 判定必须至少给出一个 satisfied 或 missing 项",
            ));
        }

        Ok(Self {
            judgement: tag,
            score_milli,
            reason,
            confidence,
            matched_alternative,
            calibration_id,
            calibrated_confidence: None,
            calibration_verified: false,
            satisfied,
            missing,
        })
    }

    /// 转换为领域 `JudgeOutcome`。
    ///
    /// 说明（与 `grade` 的责任边界）：`evidence` 由调用方补充（通常是本次
    /// `ModelCallId`）；`partial` 的满足/缺失项以 [`JudgedUnit::Choice`] 表达原始标识，
    /// 若模型返回的是评分项标识，`grade` 应改用 `self.satisfied/missing` 与 rubric 对照后
    /// 自行构造 `Judgement`。
    ///
    /// # Errors
    ///
    /// 分值构造失败（负数等）时返回 [`InferenceError::SchemaViolation`]。
    pub fn to_outcome(&self) -> Result<JudgeOutcome, InferenceError> {
        let score = MilliScore::from_milli(self.score_milli)
            .map_err(|error| schema(format!("score_milli 非法：{error}")))?;
        let judgement = match self.judgement {
            JudgeTag::Match => Judgement::Match {
                score,
                reason: self.reason.clone(),
                evidence: Vec::new(),
            },
            JudgeTag::Mismatch => Judgement::Mismatch {
                score,
                reason: self.reason.clone(),
                evidence: Vec::new(),
            },
            JudgeTag::Partial => Judgement::Partial {
                score,
                satisfied: self
                    .satisfied
                    .iter()
                    .map(|item| JudgedUnit::Choice(item.clone()))
                    .collect(),
                missing: self
                    .missing
                    .iter()
                    .map(|item| JudgedUnit::Choice(item.clone()))
                    .collect(),
                reason: self.reason.clone(),
            },
        };
        Ok(JudgeOutcome {
            judgement,
            confidence: self.confidence,
            matched_alternative: self.matched_alternative,
            calibration_id: self.calibration_id.clone(),
            calibrated_confidence: self.calibrated_confidence,
            calibration_verified: self.calibration_verified,
        })
    }
}

/// 校验后的路由产出。
#[derive(Debug, Clone, PartialEq)]
pub struct RouterOutput {
    /// 命中的枚举值（必须在调用方允许集合内）。
    pub decision: String,
    /// 置信度。
    pub confidence: Confidence,
}

impl RouterOutput {
    /// 校验模型输出为路由产出；`allowed` 是本次请求允许的枚举集合。
    ///
    /// # Errors
    ///
    /// 未知字段、枚举越界、置信度越界时返回 [`InferenceError::SchemaViolation`]。
    pub fn validate(value: &Value, allowed: &[&str]) -> Result<Self, InferenceError> {
        let object = object_with_allowed(value, "router", &["decision", "confidence"])?;
        let decision = require_str(object, "decision", "router")?.to_owned();
        if !allowed.contains(&decision.as_str()) {
            return Err(schema(format!(
                "decision {decision:?} 不在本次允许集合 {allowed:?} 内"
            )));
        }
        let confidence = Confidence::new(required_f64(object, "confidence", "router")?)
            .map_err(|error| schema(format!("confidence 非法：{error}")))?;
        Ok(Self {
            decision,
            confidence,
        })
    }
}

/// OCR 块的类型（受限枚举）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OcrBlockKind {
    /// 正文文本。
    Text,
    /// 标题。
    Title,
    /// 表格。
    Table,
    /// 公式（LaTeX）。
    Formula,
    /// 图形。
    Figure,
    /// 印章。
    Seal,
    /// 页眉。
    Header,
    /// 页脚。
    Footer,
    /// 列表。
    List,
    /// 图注。
    Caption,
}

impl OcrBlockKind {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "text" => Some(Self::Text),
            "title" => Some(Self::Title),
            "table" => Some(Self::Table),
            "formula" => Some(Self::Formula),
            "figure" => Some(Self::Figure),
            "seal" => Some(Self::Seal),
            "header" => Some(Self::Header),
            "footer" => Some(Self::Footer),
            "list" => Some(Self::List),
            "caption" => Some(Self::Caption),
            _ => None,
        }
    }

    /// 规范文本。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Title => "title",
            Self::Table => "table",
            Self::Formula => "formula",
            Self::Figure => "figure",
            Self::Seal => "seal",
            Self::Header => "header",
            Self::Footer => "footer",
            Self::List => "list",
            Self::Caption => "caption",
        }
    }
}

/// 一个 OCR 块。
#[derive(Debug, Clone, PartialEq)]
pub struct OcrBlock {
    /// 块类型。
    pub kind: OcrBlockKind,
    /// 文本（公式为 LaTeX，**原样透传**）。
    pub text: String,
    /// 置信度（可选）。
    pub confidence: Option<Confidence>,
}

/// 校验后的 OCR 产出。
#[derive(Debug, Clone, PartialEq)]
pub struct OcrOutput {
    /// 块列表（按阅读顺序）。
    pub blocks: Vec<OcrBlock>,
}

impl OcrOutput {
    /// 校验模型输出为 OCR 产出。
    ///
    /// # Errors
    ///
    /// 块数/文本长度越界、未知块类型、未知字段时返回 [`InferenceError::SchemaViolation`]。
    pub fn validate(value: &Value) -> Result<Self, InferenceError> {
        let object = object_with_allowed(value, "ocr", &["blocks"])?;
        let blocks_value = object
            .get("blocks")
            .ok_or_else(|| schema("ocr 缺少 blocks 数组"))?;
        let blocks_array = blocks_value
            .as_array()
            .ok_or_else(|| schema("ocr.blocks 必须是数组"))?;
        if blocks_array.is_empty() {
            return Err(schema("ocr.blocks 不得为空"));
        }
        if blocks_array.len() > MAX_OCR_BLOCKS {
            return Err(schema(format!(
                "ocr.blocks 数量 {} 超过上限 {MAX_OCR_BLOCKS}",
                blocks_array.len()
            )));
        }
        let mut blocks = Vec::with_capacity(blocks_array.len());
        let mut total_chars = 0_usize;
        for (index, block_value) in blocks_array.iter().enumerate() {
            let context = format!("ocr.blocks[{index}]");
            let block =
                object_with_allowed(block_value, &context, &["kind", "text", "confidence"])?;
            let kind_text = require_str(block, "kind", &context)?;
            let kind = OcrBlockKind::parse(kind_text)
                .ok_or_else(|| schema(format!("{context}.kind 未知：{kind_text:?}")))?;
            let text = require_str(block, "text", &context)?.to_owned();
            let chars = text.chars().count();
            if chars > MAX_BLOCK_TEXT_CHARS {
                return Err(schema(format!(
                    "{context}.text 长度 {chars} 超过上限 {MAX_BLOCK_TEXT_CHARS}"
                )));
            }
            total_chars += chars;
            if total_chars > MAX_BLOCK_TEXT_CHARS * 4 {
                return Err(schema("ocr 文本总量超过上限（单页 80000 字符）"));
            }
            let confidence =
                match block.get("confidence") {
                    None | Some(Value::Null) => None,
                    Some(value) => {
                        let number = value
                            .as_f64()
                            .ok_or_else(|| schema(format!("{context}.confidence 必须是数字")))?;
                        Some(Confidence::new(number).map_err(|error| {
                            schema(format!("{context}.confidence 非法：{error}"))
                        })?)
                    }
                };
            blocks.push(OcrBlock {
                kind,
                text,
                confidence,
            });
        }
        Ok(Self { blocks })
    }
}

/// 规则整理草稿项（仅草稿，永不直接生效）。
#[derive(Debug, Clone, PartialEq)]
pub struct StructurerItem {
    /// 评分项标题。
    pub title: String,
    /// 判据（自然语言）。
    pub criteria: String,
    /// 该项分值（千分之一分）。
    pub max_score_milli: i64,
    /// 是否必需项。
    pub required: bool,
    /// 依赖的其他评分项下标（0 起）。
    pub depends: Vec<u32>,
}

/// 校验后的规则整理产出。
#[derive(Debug, Clone, PartialEq)]
pub struct StructurerOutput {
    /// 草稿项。
    pub items: Vec<StructurerItem>,
}

impl StructurerOutput {
    /// 校验模型输出为规则整理草稿产出。
    ///
    /// # Errors
    ///
    /// 项数/分值越界、依赖越界、依赖成环或自引用、未知字段时返回
    /// [`InferenceError::SchemaViolation`]。
    pub fn validate(value: &Value) -> Result<Self, InferenceError> {
        let object = object_with_allowed(value, "structurer", &["items"])?;
        let items_value = object
            .get("items")
            .ok_or_else(|| schema("structurer 缺少 items 数组"))?;
        let items_array = items_value
            .as_array()
            .ok_or_else(|| schema("structurer.items 必须是数组"))?;
        if items_array.is_empty() {
            return Err(schema("structurer.items 不得为空"));
        }
        if items_array.len() > MAX_STRUCTURER_ITEMS {
            return Err(schema(format!(
                "structurer.items 数量 {} 超过上限 {MAX_STRUCTURER_ITEMS}",
                items_array.len()
            )));
        }
        let mut items = Vec::with_capacity(items_array.len());
        let mut total_score = 0_i64;
        for (index, item_value) in items_array.iter().enumerate() {
            let context = format!("structurer.items[{index}]");
            let item = object_with_allowed(
                item_value,
                &context,
                &[
                    "title",
                    "criteria",
                    "max_score_milli",
                    "required",
                    "depends",
                ],
            )?;
            let title = require_str(item, "title", &context)?.to_owned();
            if title.trim().is_empty() {
                return Err(schema(format!("{context}.title 不得为空")));
            }
            let criteria = require_str(item, "criteria", &context)?.to_owned();
            if criteria.trim().is_empty() {
                return Err(schema(format!("{context}.criteria 不得为空")));
            }
            let max_score_milli = required_i64(item, "max_score_milli", &context)?;
            if !(1..=MAX_ITEM_SCORE_MILLI).contains(&max_score_milli) {
                return Err(schema(format!(
                    "{context}.max_score_milli 必须在 1..={MAX_ITEM_SCORE_MILLI}"
                )));
            }
            total_score = total_score.saturating_add(max_score_milli);
            let required = match item.get("required") {
                None => true,
                Some(value) => value
                    .as_bool()
                    .ok_or_else(|| schema(format!("{context}.required 必须是布尔值")))?,
            };
            let depends: Vec<u32> = match item.get("depends") {
                None | Some(Value::Null) => Vec::new(),
                Some(value) => {
                    let array = value
                        .as_array()
                        .ok_or_else(|| schema(format!("{context}.depends 必须是数组")))?;
                    let mut deps = Vec::with_capacity(array.len());
                    for dependency in array {
                        let number = dependency.as_u64().ok_or_else(|| {
                            schema(format!("{context}.depends 元素必须是非负整数"))
                        })?;
                        deps.push(
                            u32::try_from(number)
                                .map_err(|_| schema(format!("{context}.depends 元素过大")))?,
                        );
                    }
                    deps
                }
            };
            items.push(StructurerItem {
                title,
                criteria,
                max_score_milli,
                required,
                depends,
            });
        }
        if total_score > MAX_TOTAL_SCORE_MILLI {
            return Err(schema(format!(
                "structurer 分值合计 {total_score} 超过上限 {MAX_TOTAL_SCORE_MILLI}"
            )));
        }
        validate_dependencies(&items)?;
        Ok(Self { items })
    }
}

fn validate_dependencies(items: &[StructurerItem]) -> Result<(), InferenceError> {
    let count = items.len();
    for (index, item) in items.iter().enumerate() {
        for dependency in &item.depends {
            let target = usize::try_from(*dependency).unwrap_or(usize::MAX);
            if target >= count {
                return Err(schema(format!(
                    "structurer.items[{index}].depends 引用越界：{dependency}（共 {count} 项）"
                )));
            }
            if target == index {
                return Err(schema(format!("structurer.items[{index}] 不得依赖自身")));
            }
        }
    }
    // 环检测：DFS 三色标记
    let mut visiting = BTreeSet::new();
    let mut done = BTreeSet::new();
    for start in 0..count {
        if !done.contains(&start) {
            walk(start, items, &mut visiting, &mut done)?;
        }
    }
    Ok(())
}

fn walk(
    index: usize,
    items: &[StructurerItem],
    visiting: &mut BTreeSet<usize>,
    done: &mut BTreeSet<usize>,
) -> Result<(), InferenceError> {
    if done.contains(&index) {
        return Ok(());
    }
    if !visiting.insert(index) {
        return Err(schema(format!("structurer.items 依赖成环：节点 {index}")));
    }
    for dependency in &items[index].depends {
        let target = usize::try_from(*dependency).unwrap_or(usize::MAX);
        walk(target, items, visiting, done)?;
    }
    visiting.remove(&index);
    done.insert(index);
    Ok(())
}

// ───────────────────────── 通用严格访问助手 ─────────────────────────

fn schema(reason: impl Into<String>) -> InferenceError {
    InferenceError::SchemaViolation {
        reason: reason.into(),
    }
}

fn object_with_allowed<'a>(
    value: &'a Value,
    context: &str,
    allowed: &[&str],
) -> Result<&'a Map<String, Value>, InferenceError> {
    let object = value
        .as_object()
        .ok_or_else(|| schema(format!("{context} 必须是 JSON 对象")))?;
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(schema(format!("{context} 出现未知字段 {key:?}")));
        }
    }
    Ok(object)
}

fn require_str<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<&'a str, InferenceError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| schema(format!("{context}.{key} 缺失或不是字符串")))
}

fn required_i64(
    object: &Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<i64, InferenceError> {
    object
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| schema(format!("{context}.{key} 缺失或不是整数")))
}

fn required_f64(
    object: &Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<f64, InferenceError> {
    object
        .get(key)
        .and_then(Value::as_f64)
        .ok_or_else(|| schema(format!("{context}.{key} 缺失或不是数字")))
}

fn optional_str_array(
    object: &Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<Vec<String>, InferenceError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(value) => {
            let array = value
                .as_array()
                .ok_or_else(|| schema(format!("{context}.{key} 必须是字符串数组")))?;
            if array.len() > MAX_STRUCTURER_ITEMS {
                return Err(schema(format!(
                    "{context}.{key} 元素过多（上限 {MAX_STRUCTURER_ITEMS}）"
                )));
            }
            array
                .iter()
                .map(|item| {
                    item.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| schema(format!("{context}.{key} 含非字符串元素")))
                })
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use serde_json::json;

    use super::*;

    fn max_score() -> MilliScore {
        MilliScore::from_points(5).unwrap()
    }

    fn valid_judge() -> Value {
        json!({
            "judgement": "match",
            "score_milli": 5000,
            "reason": "答案完全正确",
            "confidence": 0.92
        })
    }

    #[test]
    fn valid_judge_parses_and_converts_to_domain_outcome() {
        let output = JudgeOutput::validate(&valid_judge(), max_score()).unwrap();
        assert_eq!(output.judgement, JudgeTag::Match);
        assert_eq!(output.score_milli, 5_000);
        let outcome = output.to_outcome().unwrap();
        assert_eq!(outcome.judgement.label(), "match");
        assert!(outcome.calibration_id.is_none());
        assert!(!outcome.permits_auto_accept(), "无校准 id 不得自动放行");
        outcome.validate(max_score()).unwrap();
    }

    #[test]
    fn partial_requires_open_interval_and_unit_lists() {
        let partial = json!({
            "judgement": "partial",
            "score_milli": 2500,
            "reason": "只写了一个正确根",
            "confidence": 0.7,
            "satisfied": ["选对根"],
            "missing": ["缺另一个根"]
        });
        let output = JudgeOutput::validate(&partial, max_score()).unwrap();
        let outcome = output.to_outcome().unwrap();
        assert_eq!(outcome.judgement.label(), "partial");

        // partial 但没给任何列表 → 拒绝
        let empty_lists = json!({
            "judgement": "partial", "score_milli": 2500,
            "reason": "部分", "confidence": 0.5
        });
        assert!(JudgeOutput::validate(&empty_lists, max_score()).is_err());

        // partial 分数越界
        for score in [0, 5_000, 9_000] {
            let mut value = partial.clone();
            value["score_milli"] = json!(score);
            assert!(JudgeOutput::validate(&value, max_score()).is_err());
        }
    }

    #[test]
    fn score_must_match_the_judgement() {
        let mut mismatch = valid_judge();
        mismatch["judgement"] = json!("mismatch");
        assert!(JudgeOutput::validate(&mismatch, max_score()).is_err());

        let mut zero_mismatch = valid_judge();
        zero_mismatch["judgement"] = json!("mismatch");
        zero_mismatch["score_milli"] = json!(0);
        assert!(JudgeOutput::validate(&zero_mismatch, max_score()).is_ok());

        let mut bad_match = valid_judge();
        bad_match["score_milli"] = json!(4999);
        assert!(JudgeOutput::validate(&bad_match, max_score()).is_err());
    }

    #[test]
    fn unknown_fields_and_ungradable_are_rejected() {
        let mut unknown = valid_judge();
        unknown["extra"] = json!(1);
        assert!(JudgeOutput::validate(&unknown, max_score()).is_err());

        let mut ungradable = valid_judge();
        ungradable["judgement"] = json!("ungradable");
        let error = JudgeOutput::validate(&ungradable, max_score()).unwrap_err();
        assert!(error.to_string().contains("不可判定由调用方"));
    }

    #[test]
    fn router_enum_is_bounded_by_the_request() {
        let value = json!({"decision": "calculation", "confidence": 0.8});
        let output = RouterOutput::validate(&value, &["calculation", "proof"]).unwrap();
        assert_eq!(output.decision, "calculation");
        assert!(RouterOutput::validate(&value, &["proof"]).is_err());
        assert!(
            RouterOutput::validate(&json!({"decision": "x", "confidence": 1.5}), &["x"]).is_err()
        );
    }

    #[test]
    fn ocr_blocks_are_validated_and_latex_passes_through_untouched() {
        // 注意：LaTeX 必须**原样**保留（本模块不归一化、不修复）。
        let latex = r"\(x^{2} - 5x + 6 = 0\)";
        let value = json!({"blocks": [
            {"kind": "formula", "text": latex, "confidence": 0.88},
            {"kind": "text", "text": "解方程"}
        ]});
        let output = OcrOutput::validate(&value).unwrap();
        assert_eq!(output.blocks.len(), 2);
        assert_eq!(output.blocks[0].text, latex, "LaTeX 必须逐字透传");
        assert_eq!(output.blocks[0].kind, OcrBlockKind::Formula);

        assert!(OcrOutput::validate(&json!({"blocks": []})).is_err());
        assert!(
            OcrOutput::validate(&json!({"blocks": [{"kind": "diagram", "text": "x"}]})).is_err()
        );
        assert!(OcrOutput::validate(&json!({"blocks": [{"kind": "text"}]})).is_err());
        assert!(OcrOutput::validate(&json!({"blocks": [], "extra": 1})).is_err());
    }

    #[test]
    fn ocr_limits_are_enforced() {
        let block = json!({"kind": "text", "text": "x"});
        let many = json!({"blocks": vec![block; MAX_OCR_BLOCKS + 1]});
        assert!(OcrOutput::validate(&many).is_err());

        let long_text = "a".repeat(MAX_BLOCK_TEXT_CHARS + 1);
        let too_long = json!({"blocks": [{"kind": "text", "text": long_text}]});
        assert!(OcrOutput::validate(&too_long).is_err());
        // 上限本身允许
        let at_limit =
            json!({"blocks": [{"kind": "text", "text": "a".repeat(MAX_BLOCK_TEXT_CHARS)}]});
        assert!(OcrOutput::validate(&at_limit).is_ok());
    }

    #[test]
    fn structurer_items_validate_dependencies_and_cycles() {
        let valid = json!({"items": [
            {"title": "步骤 1", "criteria": "写出公式", "max_score_milli": 2000, "required": true},
            {"title": "步骤 2", "criteria": "代入计算", "max_score_milli": 3000, "depends": [0]}
        ]});
        let output = StructurerOutput::validate(&valid).unwrap();
        assert_eq!(output.items.len(), 2);
        assert_eq!(output.items[1].depends, vec![0]);

        // 自引用
        let self_ref = json!({"items": [
            {"title": "a", "criteria": "b", "max_score_milli": 1000, "depends": [0]}
        ]});
        assert!(StructurerOutput::validate(&self_ref).is_err());

        // 越界依赖
        let out_of_range = json!({"items": [
            {"title": "a", "criteria": "b", "max_score_milli": 1000, "depends": [5]}
        ]});
        assert!(StructurerOutput::validate(&out_of_range).is_err());

        // 成环
        let cyclic = json!({"items": [
            {"title": "a", "criteria": "b", "max_score_milli": 1000, "depends": [1]},
            {"title": "c", "criteria": "d", "max_score_milli": 1000, "depends": [0]}
        ]});
        let error = StructurerOutput::validate(&cyclic).unwrap_err();
        assert!(error.to_string().contains("成环"), "实际：{error}");

        // 分值合计超限（单项上限 100 分，合计上限 1000 分 → 11 项必然超）
        let over_items: Vec<Value> = (0..11)
            .map(|index| {
                json!({
                    "title": format!("项 {index}"),
                    "criteria": "判据",
                    "max_score_milli": MAX_ITEM_SCORE_MILLI
                })
            })
            .collect();
        let over_total = json!({"items": over_items});
        assert!(StructurerOutput::validate(&over_total).is_err());
    }

    #[test]
    fn thousand_malformed_judge_outputs_never_pass() {
        // SPEC-inference.md §9.5：1000 条畸形输出 → 0 条进入上层。
        let valid = valid_judge();
        let mut rejected = 0_usize;
        let mut produced = 0_usize;
        for index in 0..1000_usize {
            let mut value = valid.clone();
            match index % 10 {
                0 => {
                    value.as_object_mut().unwrap().remove("judgement");
                }
                1 => {
                    value["judgement"] = json!("maybe");
                }
                2 => {
                    value["score_milli"] = json!(max_score().as_milli() + 1);
                }
                3 => {
                    value["score_milli"] = json!("5000");
                }
                4 => {
                    value["reason"] = json!("");
                }
                5 => {
                    value["confidence"] =
                        json!(1.0 + f64::from(u8::try_from(index % 100).unwrap()) / 10.0);
                }
                6 => {
                    value["extra_field"] = json!(index);
                }
                7 => {
                    value["reason"] = json!("x".repeat(MAX_REASON_CHARS + 1));
                }
                8 => {
                    value["confidence"] = json!("high");
                }
                _ => {
                    value = json!([value]);
                }
            }
            produced += 1;
            if JudgeOutput::validate(&value, max_score()).is_err() {
                rejected += 1;
            }
        }
        assert_eq!(produced, 1000);
        assert_eq!(rejected, 1000, "1000 条畸形输出必须全部被拒绝");
    }

    #[test]
    fn non_object_inputs_are_rejected() {
        for value in [json!("text"), json!([1, 2]), json!(42), json!(null)] {
            assert!(JudgeOutput::validate(&value, max_score()).is_err());
            assert!(OcrOutput::validate(&value).is_err());
            assert!(StructurerOutput::validate(&value).is_err());
        }
    }
}
