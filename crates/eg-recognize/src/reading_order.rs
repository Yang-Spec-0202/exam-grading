//! 阅读顺序校验与双栏重排（`SPEC-recognize.md` §3.7）。
//!
//! 采用模型给出的块顺序并做单调性校验：按纵向分带（10 带），后块所属带显著早于
//! 前块且横向重叠记为顺序违规；违规比例超过 20% 记为「顺序可疑」并降低结构因子。
//! 双栏（连续空白列宽度超过页宽 8%）按「先左栏到底再右栏」确定性重排并记录。
//! 重排只改变顺序，不改写任何块内容。

use eg_domain::{BBox, Block};

/// 顺序违规比例阈值（超过即顺序可疑）。
pub const ORDER_SUSPECT_RATIO: f64 = 0.20;
/// 双栏空白列最小宽度（占页宽比例）。
pub const TWO_COLUMN_GAP_MIN: f64 = 0.08;
/// 纵向分带数量。
const BAND_COUNT: f64 = 10.0;
/// 判定「带显著倒退」的最小带差。
const BAND_BACKSTEP: i32 = 2;
/// 认为两块横向重叠的最小重叠比例（相对较窄块宽度）。
const HORIZONTAL_OVERLAP_MIN: f64 = 0.5;
/// 坐标量化比例（排序键用，避免浮点不确定性）。
const QUANTIZE: f64 = 1_000_000.0;

/// 阅读顺序检查结果。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrderReport {
    /// 模型原始顺序的违规比例（0–1）。
    pub violation_ratio: f64,
    /// 是否检测到双栏版式（已确定性重排）。
    pub two_column: bool,
    /// 是否执行了重排。
    pub reordered: bool,
}

impl OrderReport {
    /// 顺序是否可疑（违规比例超阈值且不是可用双栏解释的版式）。
    #[must_use]
    pub fn order_suspect(&self) -> bool {
        !self.reordered && self.violation_ratio > ORDER_SUSPECT_RATIO
    }

    /// 结构因子（可疑时 0.90，否则 1.0）。
    #[must_use]
    pub fn factor(&self) -> f64 {
        if self.order_suspect() { 0.90 } else { 1.0 }
    }
}

/// 检查顺序；若检测到双栏则确定性重排。
#[must_use]
pub fn apply_reading_order(blocks: &mut [Block]) -> OrderReport {
    let violation_ratio = violation_ratio(blocks);
    detect_two_column_gap(blocks).map_or(
        OrderReport {
            violation_ratio,
            two_column: false,
            reordered: false,
        },
        |(_gap_start, gap_end)| {
            reorder_two_column(blocks, gap_end);
            OrderReport {
                violation_ratio,
                two_column: true,
                reordered: true,
            }
        },
    )
}

/// 计算顺序违规比例（模型给出的顺序，未重排）。
#[must_use]
pub fn violation_ratio(blocks: &[Block]) -> f64 {
    if blocks.len() < 2 {
        return 0.0;
    }
    let mut violations = 0usize;
    for window in blocks.windows(2) {
        let previous = &window[0];
        let current = &window[1];
        let band_previous = band(previous.bbox);
        let band_current = band(current.bbox);
        if band_current + BAND_BACKSTEP <= band_previous
            && horizontal_overlap_ratio(previous.bbox, current.bbox) > HORIZONTAL_OVERLAP_MIN
        {
            violations += 1;
        }
    }
    violations as f64 / (blocks.len() - 1) as f64
}

fn band(bbox: BBox) -> i32 {
    let raw = (bbox.y * BAND_COUNT).floor();
    let clamped = raw.clamp(0.0, BAND_COUNT - 1.0);
    #[allow(clippy::cast_possible_truncation)]
    let value = clamped as i32;
    value
}

fn horizontal_overlap_ratio(left: BBox, right: BBox) -> f64 {
    let overlap = (left.right().min(right.right()) - left.x.max(right.x)).max(0.0);
    let narrow = left.w.min(right.w).max(f64::EPSILON);
    (overlap / narrow).min(1.0)
}

fn quantize(value: f64) -> i64 {
    #[allow(clippy::cast_possible_truncation)]
    let value = (value * QUANTIZE).round() as i64;
    value
}

fn detect_two_column_gap(blocks: &[Block]) -> Option<(f64, f64)> {
    if blocks.len() < 4 {
        return None;
    }
    let mut intervals: Vec<(f64, f64)> = blocks
        .iter()
        .map(|block| (block.bbox.x, block.bbox.right()))
        .collect();
    intervals.sort_by(|left, right| left.0.total_cmp(&right.0));
    let mut merged: Vec<(f64, f64)> = Vec::with_capacity(intervals.len());
    for (start, end) in intervals {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    let mut best: Option<(f64, f64)> = None;
    for window in merged.windows(2) {
        let (gap_start, gap_end) = (window[0].1, window[1].0);
        let width = gap_end - gap_start;
        if width < TWO_COLUMN_GAP_MIN {
            continue;
        }
        if gap_start <= 0.02 || gap_end >= 0.98 {
            continue;
        }
        let left = blocks
            .iter()
            .filter(|block| block.bbox.right() <= gap_start + f64::EPSILON)
            .count();
        let right = blocks
            .iter()
            .filter(|block| block.bbox.x >= gap_end - f64::EPSILON)
            .count();
        if left < 2 || right < 2 {
            continue;
        }
        if best.is_none_or(|(start, end)| (end - start) < width) {
            best = Some((gap_start, gap_end));
        }
    }
    best
}

fn reorder_two_column(blocks: &mut [Block], gap_end: f64) {
    blocks.sort_by_key(|block| {
        let center = block.bbox.x + block.bbox.w / 2.0;
        let column = usize::from(center > gap_end);
        (column, quantize(block.bbox.y), quantize(block.bbox.x))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use eg_domain::{BlockKind, Confidence};

    fn block(kind: BlockKind, x: f64, y: f64, w: f64, h: f64, text: &str) -> Block {
        Block {
            kind,
            bbox: BBox::new(x, y, w, h).unwrap(),
            text: text.to_owned(),
            latex: None,
            confidence: Confidence::ONE,
        }
    }

    #[test]
    fn single_column_reading_order_is_not_suspicious() {
        let mut blocks = vec![
            block(BlockKind::Text, 0.05, 0.05, 0.9, 0.1, "1."),
            block(BlockKind::Text, 0.05, 0.2, 0.9, 0.1, "2."),
            block(BlockKind::Formula, 0.05, 0.4, 0.9, 0.1, "3."),
        ];
        let report = apply_reading_order(&mut blocks);
        assert_eq!(report.violation_ratio, 0.0);
        assert!(!report.two_column);
        assert!(!report.order_suspect());
        assert_eq!(report.factor(), 1.0);
    }

    #[test]
    fn backward_band_with_overlap_is_violation() {
        let blocks = vec![
            block(BlockKind::Text, 0.1, 0.8, 0.8, 0.1, "后"),
            block(BlockKind::Text, 0.1, 0.1, 0.8, 0.1, "前"),
        ];
        assert_eq!(violation_ratio(&blocks), 1.0);
        let report = OrderReport {
            violation_ratio: 1.0,
            two_column: false,
            reordered: false,
        };
        assert!(report.order_suspect());
        assert_eq!(report.factor(), 0.90);
    }

    #[test]
    fn non_overlapping_backstep_is_not_violation() {
        let blocks = vec![
            block(BlockKind::Text, 0.05, 0.8, 0.4, 0.1, "左"),
            block(BlockKind::Text, 0.55, 0.1, 0.4, 0.1, "右"),
        ];
        assert_eq!(violation_ratio(&blocks), 0.0);
    }

    #[test]
    fn two_column_layout_is_detected_and_reordered() {
        let mut blocks = vec![
            block(BlockKind::Text, 0.55, 0.05, 0.4, 0.1, "R1"),
            block(BlockKind::Text, 0.55, 0.45, 0.4, 0.1, "R2"),
            block(BlockKind::Text, 0.55, 0.85, 0.4, 0.1, "R3"),
            block(BlockKind::Text, 0.05, 0.05, 0.4, 0.1, "L1"),
            block(BlockKind::Text, 0.05, 0.45, 0.4, 0.1, "L2"),
            block(BlockKind::Text, 0.05, 0.85, 0.4, 0.1, "L3"),
        ];
        let report = apply_reading_order(&mut blocks);
        assert!(report.two_column, "双栏应被检出");
        assert!(report.reordered);
        assert!(!report.order_suspect(), "已解释的双栏不算顺序可疑");
        let order: Vec<&str> = blocks.iter().map(|block| block.text.as_str()).collect();
        assert_eq!(order, vec!["L1", "L2", "L3", "R1", "R2", "R3"]);
    }

    #[test]
    fn narrow_gap_is_not_two_column() {
        let mut blocks = vec![
            block(BlockKind::Text, 0.05, 0.05, 0.45, 0.1, "A"),
            block(BlockKind::Text, 0.05, 0.45, 0.45, 0.1, "B"),
            block(BlockKind::Text, 0.545, 0.05, 0.45, 0.1, "C"),
            block(BlockKind::Text, 0.545, 0.45, 0.45, 0.1, "D"),
        ];
        let report = apply_reading_order(&mut blocks);
        assert_eq!(report.violation_ratio, 0.0);
        // 间隙宽度小于 8%，不应重排。
        assert!(!report.reordered);
    }
}
