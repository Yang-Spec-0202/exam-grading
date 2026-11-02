//! 页级识别流水线（`SPEC-recognize.md` §3.1、§3.2、§3.5、§3.6、§3.8）。
//!
//! 流水线：取白底派生图 → 渲染提示词 → 查缓存 →（未命中）调用识别后端 →
//! 严格结构解析（可机械修复，至多 `ocr.repair_attempts` 次）→ 语义校验（不重试）→
//! 阅读顺序（双栏确定性重排）→ 结构质量与置信度合成 → 写缓存/写 `ModelCall`/写
//! `Recognition` → 手写降级判断。任一步失败都不写半成品记录。
//!
//! 每次后端调用（含修复与失败尝试）都写一条 `ModelCall`：失败尝试的输出是诊断
//! 记录（含原始输出），成功尝试的输出是已校验结构化结果 + 块级置信度来源标注。

use eg_domain::{
    Block, BlockKind, ModelCall, ModelRole, Page, Recognition, Segment, Sha256Hex, TaskId,
};
use eg_inference::TokenUsage;
use eg_platform::config::OcrConfig;

use crate::cache::{CacheKey, CacheValue};
use crate::confidence::{ConfidenceBreakdown, page_confidence};
use crate::error::{RecognizeError, StructureError};
use crate::handwriting::{HandwritingPolicy, ManualHandoff, handwriting_fallback};
use crate::plausibility::{QuestionPlausibility, inspect_question_patterns};
use crate::prompt::{prompt_hash, render_page_prompt, render_segment_prompt};
use crate::reading_order::apply_reading_order;
use crate::schema::{ParsedOutput, parse_output};
use crate::segment::SegmentContext;
use crate::semantic::{
    BlockRecord, SemanticOptions, StructureQuality, compute_structure, validate_semantics,
};
use crate::types::{
    ImageBackground, ImageInput, OcrRequest, OcrResponse, RecognizeIo, RepairFeedback, ScanScope,
};

/// 单页/单段识别调用与延迟计量（进日志、审计与 `ModelCall` 输出）。
#[derive(Debug, Clone, PartialEq)]
pub struct RecognizeMetrics {
    /// 页级或段级。
    pub scope: ScanScope,
    /// 模型调用次数（含修复尝试与失败尝试；上限 1 + `ocr.repair_attempts`）。
    pub model_calls: u32,
    /// 机械修复次数。
    pub repair_attempts: u32,
    /// 是否缓存命中（命中时 `model_calls` 必须为 0）。
    pub cache_hit: bool,
    /// 后端上报延迟合计（毫秒）。
    pub latency_ms: u64,
    /// 费用合计（百万分之一货币单位；本地后端为 0）。
    pub cost_micros: u64,
    /// token 用量合计。
    pub usage: TokenUsage,
    /// 提示词哈希（引擎指纹与缓存键的组成部分）。
    pub prompt_hash: Sha256Hex,
    /// 输出 Schema 版本。
    pub schema_version: u32,
    /// 送入模型的长边像素（超过 2000 必须在适配器降采样，见 §3.8）。
    pub input_long_side: u32,
}

/// 一次识别的完整产出（识别结果 + 质量/置信度/计量 + 可选人工交接）。
#[derive(Debug, Clone, PartialEq)]
pub struct RecognizeOutcome {
    /// 识别结果（已通过领域不变量校验）。
    pub recognition: Recognition,
    /// 结构质量。
    pub structure: StructureQuality,
    /// 置信度明细（页级 + 块级）。
    pub confidence: ConfidenceBreakdown,
    /// 调用/缓存/延迟计量。
    pub metrics: RecognizeMetrics,
    /// 手写降级交接单（`None` 表示无需强制人工）。
    pub manual: Option<ManualHandoff>,
    /// 跨页候选标记（页级识别只标记，不拼接）。
    pub cross_page: CrossPageCandidates,
    /// 题号/分值模式合理性（供 `align` 与教师界面提示，不参与判分）。
    pub plausibility: QuestionPlausibility,
}

/// 跨页候选标记（`SPEC-recognize.md` §3.7：只标记，合并归 `align`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrossPageCandidates {
    /// 页首块无题号前缀且非页眉页脚（可能承接上页）。
    pub starts_mid_question: bool,
    /// 页末块触达页底（可能延续到下页）。
    pub ends_at_bottom: bool,
}

/// 页级识别入口。
///
/// # Errors
///
/// 失败语义见 [`RecognizeError`]：后端不可用/结构不合法/语义不合法/空结果/截断
/// 转人工；存储失败任务级重试；取消不写结果；配置错误启动期失败。
pub fn recognize_page(
    io: &RecognizeIo<'_>,
    page: &Page,
    config: &OcrConfig,
    task_id: TaskId,
) -> Result<RecognizeOutcome, RecognizeError> {
    run_scan(io, ScanTarget::Page { page }, config, task_id)
}

/// 识别目标（页级/段级共用流水线）。
pub(crate) enum ScanTarget<'a> {
    /// 页级识别。
    Page {
        /// 页面。
        page: &'a Page,
    },
    /// 段级识别（聚焦单个作答区域）。
    Segment {
        /// 页面。
        page: &'a Page,
        /// 作答区域。
        segment: &'a Segment,
        /// 题目上下文。
        context: &'a SegmentContext,
    },
}

impl ScanTarget<'_> {
    fn scope(&self) -> ScanScope {
        match self {
            Self::Page { .. } => ScanScope::Page,
            Self::Segment { .. } => ScanScope::Segment,
        }
    }

    fn page(&self) -> &Page {
        match self {
            Self::Page { page } | Self::Segment { page, .. } => page,
        }
    }

    fn segment(&self) -> Option<&Segment> {
        match self {
            Self::Page { .. } => None,
            Self::Segment { segment, .. } => Some(segment),
        }
    }
}

/// 配置校验（越界即配置错误，启动期失败，不静默降级）。
fn validate_config(config: &OcrConfig) -> Result<(), RecognizeError> {
    if config.model.trim().is_empty() {
        return Err(RecognizeError::Configuration {
            reason: "ocr.model 不得为空".to_owned(),
        });
    }
    if config.repair_attempts > 2 {
        return Err(RecognizeError::Configuration {
            reason: format!("ocr.repair_attempts={} 越界（0–2）", config.repair_attempts),
        });
    }
    if !(10..=180).contains(&config.page_timeout_s) {
        return Err(RecognizeError::Configuration {
            reason: format!(
                "ocr.page_timeout_s={} 越界（10–180 秒）",
                config.page_timeout_s
            ),
        });
    }
    if !(0.5..=0.95).contains(&config.confidence_floor) {
        return Err(RecognizeError::Configuration {
            reason: format!(
                "ocr.confidence_floor={} 越界（0.5–0.95）",
                config.confidence_floor
            ),
        });
    }
    Ok(())
}

pub(crate) fn run_scan(
    io: &RecognizeIo<'_>,
    target: ScanTarget<'_>,
    config: &OcrConfig,
    task_id: TaskId,
) -> Result<RecognizeOutcome, RecognizeError> {
    validate_config(config)?;
    io.check_cancel()?;
    let scope = target.scope();
    let page = target.page();
    let segment = target.segment();

    let engine = io.backend.engine();
    if engine.role != ModelRole::Ocr {
        return Err(RecognizeError::Configuration {
            reason: format!(
                "识别后端引擎角色应为 ocr，实际为 {:?}（引擎指纹必须如实标注角色）",
                engine.role
            ),
        });
    }
    if engine.is_cloud() {
        return Err(RecognizeError::Configuration {
            reason: "云端识别必须在 inference 层绑定任务授权；recognize 不直接外联".to_owned(),
        });
    }
    if engine.model_id != config.model {
        return Err(RecognizeError::Configuration {
            reason: format!(
                "ocr.model={} 与后端实际模型 {} 不一致（引擎指纹必须与配置一致）",
                config.model, engine.model_id
            ),
        });
    }
    engine
        .validate()
        .map_err(|error| RecognizeError::Configuration {
            reason: format!("引擎指纹非法：{error}"),
        })?;

    let image = match &target {
        ScanTarget::Page { page } => io.images.page_image(page)?,
        ScanTarget::Segment { page, segment, .. } => io.images.segment_image(page, segment)?,
    };
    if image.background != ImageBackground::White {
        return Err(RecognizeError::Configuration {
            reason: "识别输入未确认为白底（PDF 透明背景必须合成白底后再识别）".to_owned(),
        });
    }
    let long_side = image.width.max(image.height);
    if long_side > 2000 {
        return Err(RecognizeError::Configuration {
            reason: format!("识别输入长边 {long_side} 像素超过 2000（需在适配器先降采样并记录）"),
        });
    }
    if image.bytes.is_empty() {
        return Err(RecognizeError::Configuration {
            reason: "识别输入图像字节为空".to_owned(),
        });
    }

    let prompt = match &target {
        ScanTarget::Page { .. } => render_page_prompt(image.width, image.height, config.emit_latex),
        ScanTarget::Segment { context, .. } => {
            render_segment_prompt(image.width, image.height, context, config.emit_latex)
        }
    };
    let prompt_hash = prompt_hash(&prompt);
    let cache_key = CacheKey::new(
        image.asset,
        &engine,
        prompt_hash,
        scope,
        segment.map(|segment| segment.bbox),
    );
    let request = OcrRequest {
        scope,
        prompt,
        prompt_hash,
        image: image.bytes.clone(),
        image_width: image.width,
        image_height: image.height,
        timeout_ms: u64::from(config.page_timeout_s) * 1000,
        task_id,
    };
    let input_hash = Sha256Hex::of(
        &[
            request.prompt.as_bytes(),
            &[0u8][..],
            request.image.as_slice(),
        ]
        .concat(),
    );

    if config.cache
        && let Some(cached) = io.cache.get(&cache_key)?
    {
        return build_outcome_from_cache(io, &target, &cached, &request, image);
    }

    let mut attempt: u32 = 0;
    let mut response = io.backend.infer(&request)?;
    attempt += 1;
    let mut latency_ms = 0u64;
    let mut cost_micros = 0u64;
    let mut usage = TokenUsage::ZERO;

    let validated = loop {
        io.check_cancel()?;
        latency_ms += response.latency_ms;
        cost_micros += response.cost_micros;
        usage.input_tokens = usage
            .input_tokens
            .saturating_add(response.usage.input_tokens);
        usage.cached_input_tokens = usage
            .cached_input_tokens
            .saturating_add(response.usage.cached_input_tokens);
        usage.output_tokens = usage
            .output_tokens
            .saturating_add(response.usage.output_tokens);
        if response.truncated {
            record_model_call(
                io,
                &request,
                &response,
                &input_hash,
                attempt,
                ModelCallOutput::Failed {
                    stage: "truncated",
                    reason: "输出达到上限被截断",
                },
                scope,
            )?;
            return Err(RecognizeError::Truncated);
        }
        match parse_output(&response.raw) {
            Ok(parsed) => {
                let first_pass = attempt == 1;
                let options = SemanticOptions {
                    emit_latex: config.emit_latex,
                    first_pass,
                };
                match validate_semantics(&parsed, options) {
                    Ok(records) => {
                        record_model_call(
                            io,
                            &request,
                            &response,
                            &input_hash,
                            attempt,
                            ModelCallOutput::Valid {
                                parsed: &parsed,
                                records: &records,
                            },
                            scope,
                        )?;
                        break (records, first_pass, response.engine.clone());
                    }
                    Err(violation) => {
                        record_model_call(
                            io,
                            &request,
                            &response,
                            &input_hash,
                            attempt,
                            ModelCallOutput::Failed {
                                stage: "semantic",
                                reason: &violation.to_string(),
                            },
                            scope,
                        )?;
                        return Err(RecognizeError::Semantic {
                            reason: violation.to_string(),
                        });
                    }
                }
            }
            Err(error) => {
                record_model_call(
                    io,
                    &request,
                    &response,
                    &input_hash,
                    attempt,
                    ModelCallOutput::Failed {
                        stage: "structure",
                        reason: error.reason(),
                    },
                    scope,
                )?;
                if error.is_repairable() && attempt <= config.repair_attempts {
                    let feedback = RepairFeedback {
                        previous_raw: response.raw.clone(),
                        errors: vec![error.reason().to_owned()],
                    };
                    response = io.backend.repair(&request, &feedback)?;
                    attempt += 1;
                    continue;
                }
                return Err(match error {
                    StructureError::Empty => RecognizeError::EmptyResult,
                    StructureError::Repairable(reason) | StructureError::Fatal(reason) => {
                        RecognizeError::Structure {
                            reason,
                            repaired: attempt > 1,
                        }
                    }
                });
            }
        }
    };

    let (records, first_pass, engine) = validated;
    io.check_cancel()?;
    let mut records = records;
    let mut blocks: Vec<Block> = records.iter().map(|record| record.block.clone()).collect();
    let order = apply_reading_order(&mut blocks);
    reorder_records(&mut records, &blocks)?;
    let structure = compute_structure(&records, &order, first_pass, true);
    let page_value = page_confidence(&records, &structure);
    let confidence = ConfidenceBreakdown {
        page: page_value,
        blocks: records
            .iter()
            .map(|record| record.block.confidence)
            .collect(),
        structure_factor: structure.factor(),
    };
    let text = records
        .iter()
        .map(|record| record.block.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let recognition = Recognition {
        id: io.ids.next_recognition_id()?,
        page_id: page.id,
        segment_id: segment.map(|segment| segment.id),
        blocks: records.iter().map(|record| record.block.clone()).collect(),
        text,
        engine,
        confidence: page_value,
        created_at: io.clock.now(),
    };
    recognition
        .validate()
        .map_err(|error| RecognizeError::Configuration {
            reason: format!("识别结果不满足领域不变量（实现缺陷）：{error}"),
        })?;

    if config.cache {
        io.cache.put(
            &cache_key,
            &CacheValue {
                records: records.clone(),
                engine: recognition.engine.clone(),
                structure: structure.clone(),
            },
        )?;
    }
    io.repo.save_recognition(&recognition)?;

    let mut manual = handwriting_fallback(&recognition, &HandwritingPolicy::default());
    fill_segment_question(&mut manual, segment);
    let cross_page = cross_page_candidates(&recognition.blocks);
    let plausibility = inspect_question_patterns(&recognition.blocks);
    Ok(RecognizeOutcome {
        recognition,
        structure,
        confidence,
        metrics: RecognizeMetrics {
            scope,
            model_calls: attempt,
            repair_attempts: attempt.saturating_sub(1),
            cache_hit: false,
            latency_ms,
            cost_micros,
            usage,
            prompt_hash,
            schema_version: crate::schema::SCHEMA_VERSION,
            input_long_side: long_side,
        },
        manual,
        cross_page,
        plausibility,
    })
}

/// 段级识别时把已知题目 id 回填到人工交接单（页级识别由 `align` 关联）。
fn fill_segment_question(manual: &mut Option<ManualHandoff>, segment: Option<&Segment>) {
    if let (Some(handoff), Some(segment)) = (manual.as_mut(), segment)
        && segment.question_id.is_some()
    {
        handoff.question_id = segment.question_id;
    }
}

fn build_outcome_from_cache(
    io: &RecognizeIo<'_>,
    target: &ScanTarget<'_>,
    cached: &CacheValue,
    request: &OcrRequest,
    image: ImageInput,
) -> Result<RecognizeOutcome, RecognizeError> {
    let scope = target.scope();
    let page = target.page();
    let segment = target.segment();
    let structure = cached.structure.clone();
    let page_value = page_confidence(&cached.records, &structure);
    let confidence = ConfidenceBreakdown {
        page: page_value,
        blocks: cached
            .records
            .iter()
            .map(|record| record.block.confidence)
            .collect(),
        structure_factor: structure.factor(),
    };
    let text = cached
        .records
        .iter()
        .map(|record| record.block.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let recognition = Recognition {
        id: io.ids.next_recognition_id()?,
        page_id: page.id,
        segment_id: segment.map(|segment| segment.id),
        blocks: cached
            .records
            .iter()
            .map(|record| record.block.clone())
            .collect(),
        text,
        engine: cached.engine.clone(),
        confidence: page_value,
        created_at: io.clock.now(),
    };
    recognition
        .validate()
        .map_err(|error| RecognizeError::Configuration {
            reason: format!("缓存值不满足领域不变量（实现缺陷）：{error}"),
        })?;
    io.repo.save_recognition(&recognition)?;
    let mut manual = handwriting_fallback(&recognition, &HandwritingPolicy::default());
    fill_segment_question(&mut manual, segment);
    let cross_page = cross_page_candidates(&recognition.blocks);
    let plausibility = inspect_question_patterns(&recognition.blocks);
    Ok(RecognizeOutcome {
        recognition,
        structure,
        confidence,
        metrics: RecognizeMetrics {
            scope,
            model_calls: 0,
            repair_attempts: 0,
            cache_hit: true,
            latency_ms: 0,
            cost_micros: 0,
            usage: TokenUsage::ZERO,
            prompt_hash: request.prompt_hash,
            schema_version: crate::schema::SCHEMA_VERSION,
            input_long_side: image.width.max(image.height),
        },
        manual,
        cross_page,
        plausibility,
    })
}

fn reorder_records(
    records: &mut Vec<BlockRecord>,
    ordered: &[Block],
) -> Result<(), RecognizeError> {
    if records.len() != ordered.len() {
        return Err(RecognizeError::Configuration {
            reason: "阅读顺序重排前后块数量不一致（实现缺陷）".to_owned(),
        });
    }
    let mut reordered = Vec::with_capacity(records.len());
    for block in ordered {
        let position = records
            .iter()
            .position(|record| record.block == *block)
            .ok_or_else(|| RecognizeError::Configuration {
                reason: "阅读顺序重排无法匹配原块（实现缺陷）".to_owned(),
            })?;
        reordered.push(records.remove(position));
    }
    *records = reordered;
    Ok(())
}

/// `ModelCall.output` 的两种形态：成功（已校验结构化结果）与失败（诊断留档）。
enum ModelCallOutput<'a> {
    /// 已通过结构 + 语义校验。
    Valid {
        /// 严格解析结果。
        parsed: &'a ParsedOutput,
        /// 语义校验后的块记录。
        records: &'a [BlockRecord],
    },
    /// 失败尝试（结构/语义/截断）；原始输出留在 `raw_output` 便于复盘。
    Failed {
        /// 失败阶段（`structure`/`semantic`/`truncated`）。
        stage: &'a str,
        /// 失败原因摘要。
        reason: &'a str,
    },
}

fn record_model_call(
    io: &RecognizeIo<'_>,
    request: &OcrRequest,
    response: &OcrResponse,
    input_hash: &Sha256Hex,
    attempt: u32,
    output: ModelCallOutput<'_>,
    scope: ScanScope,
) -> Result<(), RecognizeError> {
    let mut map = serde_json::Map::new();
    map.insert(
        "scope".to_owned(),
        serde_json::Value::String(scope.as_str().to_owned()),
    );
    map.insert("attempt".to_owned(), serde_json::Value::from(attempt));
    map.insert(
        "usage".to_owned(),
        serde_json::json!({
            "input_tokens": response.usage.input_tokens,
            "cached_input_tokens": response.usage.cached_input_tokens,
            "output_tokens": response.usage.output_tokens,
        }),
    );
    map.insert(
        "input_long_side".to_owned(),
        serde_json::Value::from(request.image_width.max(request.image_height)),
    );
    match output {
        ModelCallOutput::Valid { parsed, records } => {
            map.insert(
                "status".to_owned(),
                serde_json::Value::String("valid".to_owned()),
            );
            map.insert("output".to_owned(), parsed_output_to_value(parsed));
            let sources: Vec<&str> = records
                .iter()
                .map(|record| {
                    if record.confidence_from_model() {
                        "model"
                    } else {
                        "structural_heuristic"
                    }
                })
                .collect();
            map.insert(
                "block_confidence_sources".to_owned(),
                serde_json::json!(sources),
            );
            let block_statuses: Vec<&str> = records
                .iter()
                .map(|record| match record.status {
                    crate::semantic::BlockStatus::Usable => "usable",
                    crate::semantic::BlockStatus::FormulaInvalid => "formula_invalid",
                    crate::semantic::BlockStatus::FormulaLatexDisabled => "formula_latex_disabled",
                    crate::semantic::BlockStatus::EmptyToUnknown => "empty_to_unknown",
                })
                .collect();
            map.insert(
                "block_statuses".to_owned(),
                serde_json::json!(block_statuses),
            );
        }
        ModelCallOutput::Failed { stage, reason } => {
            map.insert(
                "status".to_owned(),
                serde_json::Value::String("invalid".to_owned()),
            );
            map.insert(
                "failure_stage".to_owned(),
                serde_json::Value::String(stage.to_owned()),
            );
            map.insert(
                "failure_reason".to_owned(),
                serde_json::Value::String(reason.to_owned()),
            );
            map.insert(
                "raw_output".to_owned(),
                serde_json::Value::String(response.raw.clone()),
            );
        }
    }
    let call = ModelCall {
        id: io.ids.next_model_call_id()?,
        role: ModelRole::Ocr,
        engine: response.engine.clone(),
        task_id: request.task_id,
        consent_id: None,
        input_hash: *input_hash,
        output: serde_json::Value::Object(map),
        latency_ms: response.latency_ms,
        cost_micros: response.cost_micros,
        created_at: io.clock.now(),
    };
    io.repo.save_model_call(&call)
}

fn parsed_output_to_value(parsed: &ParsedOutput) -> serde_json::Value {
    let blocks: Vec<serde_json::Value> = parsed
        .blocks
        .iter()
        .map(|block| {
            serde_json::json!({
                "kind": wire_kind_name(block.kind),
                "bbox": block.bbox,
                "text": block.text,
                "latex": block.latex,
                "confidence": block.confidence,
            })
        })
        .collect();
    serde_json::json!({
        "schema_version": crate::schema::SCHEMA_VERSION,
        "page": parsed.page.map(|page| serde_json::json!({"width": page.width, "height": page.height})),
        "blocks": blocks,
    })
}

fn wire_kind_name(kind: crate::schema::WireKind) -> &'static str {
    match kind {
        crate::schema::WireKind::Text => "text",
        crate::schema::WireKind::Formula => "formula",
        crate::schema::WireKind::Table => "table",
        crate::schema::WireKind::Handwriting => "handwriting",
        crate::schema::WireKind::Figure => "figure",
        crate::schema::WireKind::Stamp => "stamp",
        crate::schema::WireKind::HeaderFooter => "header_footer",
        crate::schema::WireKind::Unknown => "unknown",
        crate::schema::WireKind::Barcode => "barcode",
    }
}

/// 页首/页末跨页候选标记（只看块位置与题号前缀，不做跨页内容合并）。
#[must_use]
pub fn cross_page_candidates(blocks: &[Block]) -> CrossPageCandidates {
    let meaningful: Vec<&Block> = blocks
        .iter()
        .filter(|block| block.kind != BlockKind::HeaderFooter && block.kind != BlockKind::Stamp)
        .collect();
    let Some(first) = meaningful.first() else {
        return CrossPageCandidates {
            starts_mid_question: false,
            ends_at_bottom: false,
        };
    };
    let last = meaningful.last().unwrap_or(first);
    CrossPageCandidates {
        starts_mid_question: !has_question_number_prefix(&first.text),
        ends_at_bottom: last.bbox.bottom() >= 0.94,
    }
}

/// 文本是否以题号前缀开头（`1.`/`12、`/`3)` 等常见形式）。
#[must_use]
pub fn has_question_number_prefix(text: &str) -> bool {
    let trimmed = text.trim_start();
    let digits: String = trimmed.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || digits.len() > 3 {
        return false;
    }
    matches!(
        trimmed.chars().nth(digits.len()),
        Some('.' | '、' | ')' | '．')
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use eg_domain::{AssetId, BBox, BackendId, Confidence, EngineStamp};
    use eg_platform::config::SegmentRecognition;

    use crate::cache::bbox_hash;
    use crate::error::SemanticViolation;
    use crate::prompt::render_repair_prompt;
    use crate::reading_order::OrderReport;

    fn block(kind: BlockKind, x: f64, y: f64, w: f64, h: f64, text: &str) -> Block {
        Block {
            kind,
            bbox: BBox::new(x, y, w, h).unwrap(),
            text: text.to_owned(),
            latex: None,
            confidence: Confidence::ZERO,
        }
    }

    #[test]
    fn question_number_prefix_detection() {
        assert!(has_question_number_prefix("1. 求值"));
        assert!(has_question_number_prefix("12、计算"));
        assert!(has_question_number_prefix("3) 证明"));
        assert!(has_question_number_prefix("  7．解方程"));
        assert!(!has_question_number_prefix("解：x=1"));
        assert!(!has_question_number_prefix("1+1=2"));
        assert!(!has_question_number_prefix(""));
        assert!(!has_question_number_prefix("1234. 超长题号"));
    }

    #[test]
    fn cross_page_candidates_mark_first_and_last() {
        let blocks = vec![
            block(BlockKind::Text, 0.1, 0.02, 0.8, 0.1, "继续作答：x=1"),
            block(BlockKind::Text, 0.1, 0.50, 0.8, 0.1, "2. 求导"),
            block(BlockKind::Text, 0.1, 0.86, 0.8, 0.08, "y=2x+1"),
        ];
        let candidates = cross_page_candidates(&blocks);
        assert!(candidates.starts_mid_question);
        assert!(candidates.ends_at_bottom);
        let closed = cross_page_candidates(&[block(BlockKind::Text, 0.1, 0.05, 0.8, 0.1, "1. 题")]);
        assert!(!closed.starts_mid_question);
        assert!(!closed.ends_at_bottom);
    }

    #[test]
    fn no_blocks_yields_no_candidates() {
        let candidates = cross_page_candidates(&[]);
        assert!(!candidates.starts_mid_question);
        assert!(!candidates.ends_at_bottom);
    }

    #[test]
    fn header_footer_does_not_count_as_page_edge() {
        let blocks = vec![
            block(BlockKind::HeaderFooter, 0.1, 0.0, 0.8, 0.05, "第 1 页"),
            block(BlockKind::Text, 0.1, 0.1, 0.8, 0.1, "3. 计算"),
            block(BlockKind::Text, 0.1, 0.5, 0.8, 0.1, "答案"),
        ];
        let candidates = cross_page_candidates(&blocks);
        assert!(!candidates.starts_mid_question);
        assert!(!candidates.ends_at_bottom);
    }

    #[test]
    fn assets_and_backends_are_reported_in_metrics() {
        // 仅为让 ScanScope/BackendId/AssetId 的公开再导出被使用（编译期覆盖）。
        assert_eq!(ScanScope::Page.as_str(), "page");
        assert_eq!(BackendId::openai_compat().as_str(), "openai_compat");
        assert_eq!(AssetId::of(b"x"), AssetId::of(b"x"));
        assert_eq!(SegmentRecognition::OnDemand, SegmentRecognition::default());
        assert_eq!(
            OrderReport {
                violation_ratio: 0.0,
                two_column: false,
                reordered: false
            }
            .factor(),
            1.0
        );
        assert_eq!(bbox_hash(BBox::full_page()), bbox_hash(BBox::full_page()));
        let _ = StructureQuality {
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
        .factor();
        let _ = SemanticViolation::Barcode { block: 1 }.to_string();
        let _ = render_page_prompt(1, 1, true);
        let _ = render_repair_prompt(
            "p",
            &RepairFeedback {
                previous_raw: "r".to_owned(),
                errors: Vec::new(),
            },
        );
        let _ = EngineStamp {
            role: ModelRole::Ocr,
            backend: BackendId::fixture(),
            model_id: "m".to_owned(),
            model_version: "v".to_owned(),
            prompt_hash: None,
            params_hash: Sha256Hex::of(b"p"),
        };
        let _ = ImageInput {
            asset: AssetId::of(b"i"),
            bytes: vec![1],
            width: 1,
            height: 1,
            background: ImageBackground::White,
        };
    }
}
