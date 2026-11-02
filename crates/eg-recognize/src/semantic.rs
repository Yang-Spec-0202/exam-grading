//! 语义校验、块构建与结构质量（`SPEC-recognize.md` §2.2、§2.4、§3.2、§3.7）。
//!
//! 语义校验由本模块执行且**不重试**：块坐标范围、取值集合（条码块禁止）、重复块、
//! 数量与长度上限（数量/长度在严格 Schema 阶段已拦，这里复核页面声明与坐标）。
//! 语义违规直接转人工，绝不回写修改模型原文。
//!
//! 公式块的特殊处理：领域不变量要求「公式块必须携带非空 LaTeX」，因此 LaTeX 非法或
//! `ocr.emit_latex` 关闭时，该块降级为 `BlockKind::Unknown`、`latex` 为空、置信度置 0，
//! 并记录 [`BlockStatus`] 与非法公式计数——保留原始文本作证据，**不伪造公式**。

use eg_domain::{BBox, Block, BlockKind, Confidence};

use crate::confidence::{
    STRUCT_FACTOR_ORDER_SUSPECT, STRUCT_FACTOR_REPAIRED, STRUCT_FACTOR_STAMP_OCCLUDES,
    STRUCT_FACTOR_UNKNOWN_BLOCK, block_confidence,
};
use crate::error::SemanticViolation;
use crate::latex::normalize_and_validate;
use crate::reading_order::OrderReport;
use crate::schema::{ParsedOutput, WireKind};

/// 语义校验参数（来自 `ocr.emit_latex` 与本次结构是否首次通过）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemanticOptions {
    /// `ocr.emit_latex`：关闭时公式块按不可用处理。
    pub emit_latex: bool,
    /// 本次输出是否首次通过 Schema（false = 修复后通过）。
    pub first_pass: bool,
}

/// 块状态（保留不可用原因，供审计与题级转人工判断）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockStatus {
    /// 可用。
    Usable,
    /// 公式 LaTeX 非法（块不可用，置信度 0）。
    FormulaInvalid,
    /// `ocr.emit_latex` 关闭（公式块不可用，置信度 0）。
    FormulaLatexDisabled,
    /// 非公式块文本为空，按未知块处理。
    EmptyToUnknown,
}

/// 语义校验后的块记录（缓存值与置信度合成的最小单元）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BlockRecord {
    /// 领域块（满足 `Block::validate` 不变量）。
    pub block: Block,
    /// 块级置信度来源标注。
    pub source: crate::confidence::ConfidenceSource,
    /// 块状态。
    pub status: BlockStatus,
}

impl BlockRecord {
    /// 块级置信度是否来自模型（而非结构启发）。
    #[must_use]
    pub const fn confidence_from_model(&self) -> bool {
        matches!(self.source, crate::confidence::ConfidenceSource::Model)
    }
}

/// 结构质量（`SPEC-recognize.md` §2.4 模块自有类型）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StructureQuality {
    /// 是否首次通过 Schema。
    pub first_pass: bool,
    /// 是否修复后通过。
    pub repaired: bool,
    /// 阅读顺序违规比例（0–1）。
    pub order_violation_ratio: f64,
    /// 顺序是否可疑（超过 20% 且非双栏可解释）。
    pub order_suspect: bool,
    /// 是否为双栏版式（已确定性重排）。
    pub two_column_layout: bool,
    /// 是否含未知块。
    pub has_unknown_block: bool,
    /// 非法公式块数量。
    pub formula_invalid_count: usize,
    /// 因 `ocr.emit_latex` 关闭而不可用的公式块数量。
    pub formula_latex_disabled_count: usize,
    /// 印章是否遮挡大文本块（交并比超过 0.3）。
    pub stamp_occludes: bool,
    /// 内容覆盖率（32×32 网格中被子块覆盖的比例）。
    pub coverage_ratio: f64,
    /// 输入图像是否已确认白底（`SPEC-capture.md` 要求）。
    pub background_verified: bool,
}

impl StructureQuality {
    /// 结构因子：乘在页级加权平均上（见 `SPEC-recognize.md` §3.5）。
    #[must_use]
    pub fn factor(&self) -> f64 {
        let mut factor = 1.0;
        if !self.first_pass {
            factor *= STRUCT_FACTOR_REPAIRED;
        }
        if self.has_unknown_block {
            factor *= STRUCT_FACTOR_UNKNOWN_BLOCK;
        }
        if self.order_suspect {
            factor *= STRUCT_FACTOR_ORDER_SUSPECT;
        }
        if self.stamp_occludes {
            factor *= STRUCT_FACTOR_STAMP_OCCLUDES;
        }
        factor
    }

    /// 是否首次通过 Schema（规范字段别名的显式读取）。
    #[must_use]
    pub const fn schema_valid_first_try(&self) -> bool {
        self.first_pass
    }
}

/// 执行语义校验并把解析块构造成领域块记录。
///
/// # Errors
///
/// 坐标越界/非有限、条码块、重复块、零块、非法页声明返回 [`SemanticViolation`]；
/// 违规不重试，由调用方映射为「识别不可用」并转人工。
pub fn validate_semantics(
    parsed: &ParsedOutput,
    options: SemanticOptions,
) -> Result<Vec<BlockRecord>, SemanticViolation> {
    if parsed.blocks.is_empty() {
        return Err(SemanticViolation::NoBlocks);
    }
    if let Some(page) = parsed.page
        && (page.width == 0 || page.height == 0)
    {
        return Err(SemanticViolation::InvalidPageDims);
    }
    detect_duplicates(parsed)?;

    let mut records = Vec::with_capacity(parsed.blocks.len());
    for (index, parsed_block) in parsed.blocks.iter().enumerate() {
        let number = index + 1;
        if parsed_block.bbox.iter().any(|value| !value.is_finite()) {
            return Err(SemanticViolation::NonFiniteCoord { block: number });
        }
        let bbox = BBox::new(
            parsed_block.bbox[0],
            parsed_block.bbox[1],
            parsed_block.bbox[2],
            parsed_block.bbox[3],
        )
        .map_err(|error| SemanticViolation::CoordOutOfRange {
            block: number,
            reason: error.to_string(),
        })?;
        if parsed_block.kind == WireKind::Barcode {
            return Err(SemanticViolation::Barcode { block: number });
        }

        let mut kind = domain_kind(parsed_block.kind);
        let mut latex = None;
        let mut status = BlockStatus::Usable;
        let mut text = parsed_block.text.clone();
        if kind == BlockKind::Table {
            text = normalize_table_text(&text);
        }
        let mut usable = true;

        if kind == BlockKind::Formula {
            if options.emit_latex {
                match parsed_block
                    .latex
                    .as_deref()
                    .filter(|raw| !raw.trim().is_empty())
                    .map(normalize_and_validate)
                {
                    Some(outcome) if outcome.is_valid() => {
                        latex = outcome.latex().map(str::to_owned);
                    }
                    _ => {
                        status = BlockStatus::FormulaInvalid;
                        usable = false;
                        kind = BlockKind::Unknown;
                        latex = None;
                        if text.trim().is_empty() {
                            text = parsed_block.latex.clone().unwrap_or_default();
                        }
                    }
                }
            } else {
                status = BlockStatus::FormulaLatexDisabled;
                usable = false;
                kind = BlockKind::Unknown;
                latex = None;
            }
        } else if kind == BlockKind::Text && text.trim().is_empty() {
            kind = BlockKind::Unknown;
            status = BlockStatus::EmptyToUnknown;
        }

        let (confidence, source) = block_confidence(
            parsed_block.confidence,
            options.first_pass,
            kind == BlockKind::Unknown,
            text.trim().is_empty(),
            usable,
        );
        records.push(BlockRecord {
            block: Block {
                kind,
                bbox,
                text,
                latex,
                confidence,
            },
            source,
            status,
        });
    }
    Ok(records)
}

fn domain_kind(kind: WireKind) -> BlockKind {
    match kind {
        WireKind::Text => BlockKind::Text,
        WireKind::Formula => BlockKind::Formula,
        WireKind::Table => BlockKind::Table,
        WireKind::Handwriting => BlockKind::Handwriting,
        WireKind::Figure => BlockKind::Figure,
        WireKind::Stamp => BlockKind::Stamp,
        WireKind::HeaderFooter => BlockKind::HeaderFooter,
        WireKind::Unknown | WireKind::Barcode => BlockKind::Unknown,
    }
}

fn detect_duplicates(parsed: &ParsedOutput) -> Result<(), SemanticViolation> {
    let mut seen: Vec<(usize, &crate::schema::ParsedBlock)> = Vec::new();
    for (index, block) in parsed.blocks.iter().enumerate() {
        let duplicate = seen.iter().find(|(_, previous)| {
            previous.kind == block.kind
                && previous.text == block.text
                && bbox_equal(previous.bbox, block.bbox)
        });
        if let Some((first, _)) = duplicate {
            return Err(SemanticViolation::DuplicateBlock {
                block: index + 1,
                first: first + 1,
            });
        }
        seen.push((index, block));
    }
    Ok(())
}

fn bbox_equal(left: [f64; 4], right: [f64; 4]) -> bool {
    const EPSILON: f64 = 1e-6;
    left.iter()
        .zip(right.iter())
        .all(|(left, right)| (left - right).abs() <= EPSILON)
}

/// 表格块文本规范化：行内连续 2 个及以上空格转为制表符（单元格分隔），逐行修剪。
#[must_use]
pub fn normalize_table_text(text: &str) -> String {
    text.lines()
        .map(|line| {
            let mut cells: Vec<String> = Vec::new();
            let mut current = String::new();
            let mut spaces = 0usize;
            for ch in line.trim().chars() {
                if ch == ' ' || ch == '\t' {
                    spaces += 1;
                    continue;
                }
                if spaces >= 2 {
                    if !current.is_empty() {
                        cells.push(std::mem::take(&mut current));
                    }
                } else {
                    for _ in 0..spaces {
                        current.push(' ');
                    }
                }
                spaces = 0;
                current.push(ch);
            }
            if !current.is_empty() {
                cells.push(current);
            }
            cells.join("\t")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 计算结构质量（阅读顺序、覆盖率、印章遮挡在块构建后统一评估）。
#[must_use]
pub fn compute_structure(
    records: &[BlockRecord],
    order: &OrderReport,
    first_pass: bool,
    background_verified: bool,
) -> StructureQuality {
    let has_unknown_block = records
        .iter()
        .any(|record| record.block.kind == BlockKind::Unknown);
    let formula_invalid_count = records
        .iter()
        .filter(|record| record.status == BlockStatus::FormulaInvalid)
        .count();
    let formula_latex_disabled_count = records
        .iter()
        .filter(|record| record.status == BlockStatus::FormulaLatexDisabled)
        .count();
    let blocks: Vec<Block> = records.iter().map(|record| record.block.clone()).collect();
    StructureQuality {
        first_pass,
        repaired: !first_pass,
        order_violation_ratio: order.violation_ratio,
        order_suspect: order.order_suspect(),
        two_column_layout: order.two_column,
        has_unknown_block,
        formula_invalid_count,
        formula_latex_disabled_count,
        stamp_occludes: stamp_occludes(&blocks),
        coverage_ratio: coverage_ratio(&blocks),
        background_verified,
    }
}

/// 印章遮挡：印章块与任一大文本块（面积 ≥ 2%）交并比超过 0.3。
#[must_use]
pub fn stamp_occludes(blocks: &[Block]) -> bool {
    let stamps: Vec<&Block> = blocks
        .iter()
        .filter(|block| block.kind == BlockKind::Stamp)
        .collect();
    if stamps.is_empty() {
        return false;
    }
    let large_text: Vec<&Block> = blocks
        .iter()
        .filter(|block| {
            matches!(
                block.kind,
                BlockKind::Text | BlockKind::Formula | BlockKind::Handwriting | BlockKind::Table
            ) && block.bbox.w * block.bbox.h >= 0.02
        })
        .collect();
    stamps.iter().any(|stamp| {
        large_text
            .iter()
            .any(|text| intersection_over_union(stamp.bbox, text.bbox) > 0.3)
    })
}

fn intersection_over_union(left: BBox, right: BBox) -> f64 {
    let intersection_width = (left.right().min(right.right()) - left.x.max(right.x)).max(0.0);
    let intersection_height = (left.bottom().min(right.bottom()) - left.y.max(right.y)).max(0.0);
    let intersection = intersection_width * intersection_height;
    let union = left.w * left.h + right.w * right.h - intersection;
    if union <= 0.0 {
        0.0
    } else {
        intersection / union
    }
}

/// 内容覆盖率：32×32 网格中被子块覆盖的格子比例。
#[must_use]
pub fn coverage_ratio(blocks: &[Block]) -> f64 {
    const GRID: usize = 32;
    let mut covered = vec![false; GRID * GRID];
    for block in blocks {
        if block.kind == BlockKind::HeaderFooter {
            continue;
        }
        let start_x = grid_index(block.bbox.x);
        let end_x = grid_index(block.bbox.right());
        let start_y = grid_index(block.bbox.y);
        let end_y = grid_index(block.bbox.bottom());
        for y in start_y..=end_y.min(GRID - 1) {
            for x in start_x..=end_x.min(GRID - 1) {
                covered[y * GRID + x] = true;
            }
        }
    }
    let count = covered.iter().filter(|value| **value).count();
    count as f64 / (GRID * GRID) as f64
}

fn grid_index(value: f64) -> usize {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let index = (value.clamp(0.0, 1.0) * 32.0).floor() as usize;
    index.min(31)
}

/// 全页覆盖（供测试与单块识别使用）。
#[must_use]
pub fn full_page_record() -> Block {
    Block {
        kind: BlockKind::Text,
        bbox: BBox::full_page(),
        text: String::new(),
        latex: None,
        confidence: Confidence::ZERO,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confidence::ConfidenceSource;
    use crate::schema::{PageDims, ParsedBlock};

    const MAX_BLOCKS_FOR_TEST: usize = crate::schema::MAX_BLOCKS;

    fn parsed(blocks: Vec<ParsedBlock>) -> ParsedOutput {
        ParsedOutput { page: None, blocks }
    }

    fn text_block(x: f64, y: f64, w: f64, h: f64, text: &str) -> ParsedBlock {
        ParsedBlock {
            kind: WireKind::Text,
            bbox: [x, y, w, h],
            text: text.to_owned(),
            latex: None,
            confidence: Some(0.9),
        }
    }

    fn options() -> SemanticOptions {
        SemanticOptions {
            emit_latex: true,
            first_pass: true,
        }
    }

    #[test]
    fn rejects_coord_out_of_range_and_non_finite() {
        let out = validate_semantics(
            &parsed(vec![text_block(0.9, 0.0, 0.2, 0.1, "x")]),
            options(),
        );
        assert!(matches!(
            out,
            Err(SemanticViolation::CoordOutOfRange { block: 1, .. })
        ));
        let zero = validate_semantics(
            &parsed(vec![text_block(0.1, 0.1, 0.0, 0.1, "x")]),
            options(),
        );
        assert!(matches!(
            zero,
            Err(SemanticViolation::CoordOutOfRange { block: 1, .. })
        ));
        let non_finite = validate_semantics(
            &parsed(vec![ParsedBlock {
                kind: WireKind::Text,
                bbox: [f64::NAN, 0.1, 0.2, 0.1],
                text: "x".to_owned(),
                latex: None,
                confidence: None,
            }]),
            options(),
        );
        assert!(matches!(
            non_finite,
            Err(SemanticViolation::NonFiniteCoord { block: 1 })
        ));
    }

    #[test]
    fn rejects_barcode_and_duplicates() {
        let barcode = validate_semantics(
            &parsed(vec![ParsedBlock {
                kind: WireKind::Barcode,
                bbox: [0.1, 0.1, 0.2, 0.1],
                text: "690".to_owned(),
                latex: None,
                confidence: None,
            }]),
            options(),
        );
        assert!(matches!(
            barcode,
            Err(SemanticViolation::Barcode { block: 1 })
        ));
        let duplicates = validate_semantics(
            &parsed(vec![
                text_block(0.1, 0.1, 0.2, 0.1, "same"),
                text_block(0.1, 0.1, 0.2, 0.1, "same"),
            ]),
            options(),
        );
        assert!(matches!(
            duplicates,
            Err(SemanticViolation::DuplicateBlock { block: 2, first: 1 })
        ));
    }

    #[test]
    fn rejects_invalid_page_dims_and_empty() {
        let mut output = parsed(vec![text_block(0.1, 0.1, 0.2, 0.1, "x")]);
        output.page = Some(PageDims {
            width: 0,
            height: 100,
        });
        assert!(matches!(
            validate_semantics(&output, options()),
            Err(SemanticViolation::InvalidPageDims)
        ));
        assert!(matches!(
            validate_semantics(&parsed(Vec::new()), options()),
            Err(SemanticViolation::NoBlocks)
        ));
    }

    #[test]
    fn invalid_formula_becomes_unusable_unknown_block() {
        let records = validate_semantics(
            &parsed(vec![ParsedBlock {
                kind: WireKind::Formula,
                bbox: [0.1, 0.1, 0.5, 0.2],
                text: "x+1".to_owned(),
                latex: Some("\\frac{1}{2".to_owned()),
                confidence: Some(0.99),
            }]),
            options(),
        )
        .unwrap();
        assert_eq!(records[0].block.kind, BlockKind::Unknown);
        assert_eq!(records[0].block.latex, None);
        assert_eq!(records[0].block.confidence, Confidence::ZERO);
        assert_eq!(records[0].status, BlockStatus::FormulaInvalid);
        assert_eq!(records[0].source, ConfidenceSource::StructuralHeuristic);
        let structure = compute_structure(&records, &clean_order(), true, true);
        assert_eq!(structure.formula_invalid_count, 1);
        assert!(structure.has_unknown_block);
    }

    #[test]
    fn emit_latex_disabled_makes_formula_unusable() {
        let records = validate_semantics(
            &parsed(vec![ParsedBlock {
                kind: WireKind::Formula,
                bbox: [0.1, 0.1, 0.5, 0.2],
                text: "x+1".to_owned(),
                latex: Some("x+1".to_owned()),
                confidence: Some(0.99),
            }]),
            SemanticOptions {
                emit_latex: false,
                first_pass: true,
            },
        )
        .unwrap();
        assert_eq!(records[0].block.kind, BlockKind::Unknown);
        assert_eq!(records[0].status, BlockStatus::FormulaLatexDisabled);
        assert_eq!(records[0].block.confidence, Confidence::ZERO);
        assert_eq!(records[0].block.text, "x+1");
    }

    #[test]
    fn valid_formula_is_normalized_and_keeps_model_confidence() {
        let records = validate_semantics(
            &parsed(vec![ParsedBlock {
                kind: WireKind::Formula,
                bbox: [0.1, 0.1, 0.5, 0.2],
                text: "x+1".to_owned(),
                latex: Some("$x≤1$".to_owned()),
                confidence: Some(0.8),
            }]),
            options(),
        )
        .unwrap();
        assert_eq!(records[0].block.kind, BlockKind::Formula);
        assert_eq!(records[0].block.latex.as_deref(), Some("x\\leq 1"));
        assert_eq!(records[0].source, ConfidenceSource::Model);
    }

    #[test]
    fn empty_text_becomes_unknown_block() {
        // 模型没给概率：用结构启发值（空文本 0.20），并按未知块计入结构因子。
        let records = validate_semantics(
            &parsed(vec![ParsedBlock {
                kind: WireKind::Text,
                bbox: [0.1, 0.1, 0.2, 0.1],
                text: String::new(),
                latex: None,
                confidence: None,
            }]),
            options(),
        )
        .unwrap();
        assert_eq!(records[0].block.kind, BlockKind::Unknown);
        assert_eq!(records[0].status, BlockStatus::EmptyToUnknown);
        assert_eq!(records[0].block.confidence.get(), 0.20);
        // 模型给了概率时按规范用模型值（不得伪装成启发值）。
        let with_model = validate_semantics(
            &parsed(vec![ParsedBlock {
                kind: WireKind::Text,
                bbox: [0.1, 0.1, 0.2, 0.1],
                text: String::new(),
                latex: None,
                confidence: Some(0.66),
            }]),
            options(),
        )
        .unwrap();
        assert_eq!(with_model[0].source, ConfidenceSource::Model);
        assert_eq!(with_model[0].block.confidence.get(), 0.66);
    }

    #[test]
    fn table_text_is_tab_normalized() {
        assert_eq!(normalize_table_text("甲    乙\n1    2"), "甲\t乙\n1\t2");
        assert_eq!(normalize_table_text("单 个 空格"), "单 个 空格");
    }

    #[test]
    fn stamp_occlusion_and_coverage() {
        let mut stamp = text_block(0.2, 0.2, 0.4, 0.2, "");
        stamp.kind = WireKind::Stamp;
        let mut answer = text_block(0.2, 0.2, 0.4, 0.2, "答案");
        answer.kind = WireKind::Text;
        let records = validate_semantics(&parsed(vec![stamp, answer]), options()).unwrap();
        let blocks: Vec<Block> = records.iter().map(|record| record.block.clone()).collect();
        assert!(stamp_occludes(&blocks));
        assert!(coverage_ratio(&blocks) > 0.0);
        assert!(coverage_ratio(&blocks) <= 1.0);
    }

    fn clean_order() -> OrderReport {
        OrderReport {
            violation_ratio: 0.0,
            two_column: false,
            reordered: false,
        }
    }

    #[test]
    fn validation_budget_for_max_blocks_is_bounded() {
        // 200 块（规范上限）结构+语义校验的本地耗时远低于单页模型延迟（实测见研究记录）。
        let blocks: Vec<ParsedBlock> = (0..MAX_BLOCKS_FOR_TEST)
            .map(|index| ParsedBlock {
                kind: WireKind::Text,
                bbox: [0.01, (index as f64) * 0.004, 0.9, 0.003],
                text: format!("第 {index} 行作答"),
                latex: None,
                confidence: Some(0.9),
            })
            .collect();
        let output = parsed(blocks);
        let start = std::time::Instant::now();
        let records = validate_semantics(&output, options()).unwrap();
        let order = crate::reading_order::apply_reading_order(
            &mut records
                .iter()
                .map(|record| record.block.clone())
                .collect::<Vec<_>>(),
        );
        let quality = compute_structure(&records, &order, true, true);
        assert_eq!(records.len(), MAX_BLOCKS_FOR_TEST);
        assert!(quality.coverage_ratio > 0.0);
        eprintln!("200 块结构+语义+顺序+结构质量耗时：{:?}", start.elapsed());
        assert!(
            start.elapsed() < std::time::Duration::from_millis(100),
            "200 块校验耗时异常：{:?}",
            start.elapsed()
        );
    }

    #[test]
    fn structure_factor_composition() {
        let records = validate_semantics(
            &parsed(vec![text_block(0.1, 0.1, 0.5, 0.2, "答案")]),
            SemanticOptions {
                emit_latex: true,
                first_pass: false,
            },
        )
        .unwrap();
        let quality = compute_structure(&records, &clean_order(), false, true);
        assert!(quality.repaired);
        assert!(!quality.schema_valid_first_try());
        assert!((quality.factor() - 0.90).abs() < 1e-9);
    }
}
