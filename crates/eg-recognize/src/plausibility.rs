//! 题号/分值模式合理性（`SPEC-recognize.md` §3.1、T23 语义校验项）。
//!
//! 只做**页面级启发式检查**，不参与判分：从文本块的行首题号（`1.`/`12、`/`3)` 等）
//! 提取编号，统计重复编号；统计「分值标注」（如「（5分）」「5 分」）的出现次数。
//! 结果进 `RecognizeOutcome`，供 `align` 与教师界面提示，不单独构成失败（
//! 题号解析与题目对齐是 `align` 的职责，本模块只提供确定性候选信号）。

use eg_domain::{Block, BlockKind};

/// 题号/分值模式报告（确定性、可复现）。
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct QuestionPlausibility {
    /// 页内检出的题号（按块顺序，去重前）。
    pub question_numbers: Vec<u32>,
    /// 重复出现的题号（升序，去重）。
    pub duplicate_numbers: Vec<u32>,
    /// 分值标注出现次数（「x分」「（x 分）」）。
    pub score_annotations: usize,
}

impl QuestionPlausibility {
    /// 是否存在可疑重复题号（同一页同一题号出现两次以上）。
    #[must_use]
    pub fn has_duplicate_numbers(&self) -> bool {
        !self.duplicate_numbers.is_empty()
    }
}

/// 从页面块提取题号与分值标注。
#[must_use]
pub fn inspect_question_patterns(blocks: &[Block]) -> QuestionPlausibility {
    let mut numbers: Vec<u32> = Vec::new();
    let mut score_annotations = 0usize;
    for block in blocks {
        if matches!(
            block.kind,
            BlockKind::HeaderFooter | BlockKind::Stamp | BlockKind::Figure
        ) {
            continue;
        }
        if let Some(number) = leading_question_number(&block.text) {
            numbers.push(number);
        }
        score_annotations += count_score_annotations(&block.text);
    }
    let mut sorted = numbers.clone();
    sorted.sort_unstable();
    let mut duplicates: Vec<u32> = Vec::new();
    let mut index = 0usize;
    while index < sorted.len() {
        let number = sorted[index];
        let mut count = 1usize;
        while index + count < sorted.len() && sorted[index + count] == number {
            count += 1;
        }
        if count >= 2 && !duplicates.contains(&number) {
            duplicates.push(number);
        }
        index += count;
    }
    QuestionPlausibility {
        question_numbers: numbers,
        duplicate_numbers: duplicates,
        score_annotations,
    }
}

/// 行首题号：1–3 位数字 + `.`/`、`/`)`/`．`（与跨页候选判定共用同一形式）。
#[must_use]
pub fn leading_question_number(text: &str) -> Option<u32> {
    let trimmed = text.trim_start();
    let digits: String = trimmed.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || digits.len() > 3 {
        return None;
    }
    if !matches!(
        trimmed.chars().nth(digits.len()),
        Some('.' | '、' | ')' | '．')
    ) {
        return None;
    }
    digits.parse::<u32>().ok()
}

fn count_score_annotations(text: &str) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let mut count = 0usize;
    let mut index = 0usize;
    while index < chars.len() {
        if chars[index] == '分' {
            // 向前看相邻数字（允许一个空格）：形如「5分」「5 分」。
            let mut probe = index;
            while probe > 0 && chars[probe - 1] == ' ' {
                probe -= 1;
            }
            let digit_start = probe;
            while probe > 0 && chars[probe - 1].is_ascii_digit() {
                probe -= 1;
            }
            if probe < digit_start {
                count += 1;
            }
            index = digit_start.max(index + 1);
        } else {
            index += 1;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use eg_domain::{BBox, Confidence};

    fn block(text: &str) -> Block {
        Block {
            kind: BlockKind::Text,
            bbox: BBox::new(0.1, 0.1, 0.8, 0.1).unwrap(),
            text: text.to_owned(),
            latex: None,
            confidence: Confidence::ONE,
        }
    }

    #[test]
    fn extracts_question_numbers() {
        assert_eq!(leading_question_number("1. 求值"), Some(1));
        assert_eq!(leading_question_number("12、计算"), Some(12));
        assert_eq!(leading_question_number("3) 证明"), Some(3));
        assert_eq!(leading_question_number("  7．解方程"), Some(7));
        assert_eq!(leading_question_number("1+1=2"), None);
        assert_eq!(leading_question_number("1234. 超长"), None);
        assert_eq!(leading_question_number(""), None);
    }

    #[test]
    fn detects_duplicates_and_score_annotations() {
        let blocks = vec![
            block("1. 求值（5分）"),
            block("2. 计算（10 分）"),
            block("2. 重复题号"),
            block("解：x=1"),
        ];
        let report = inspect_question_patterns(&blocks);
        assert_eq!(report.question_numbers, vec![1, 2, 2]);
        assert_eq!(report.duplicate_numbers, vec![2]);
        assert!(report.has_duplicate_numbers());
        assert_eq!(report.score_annotations, 2);
    }

    #[test]
    fn ignores_header_footer_and_figures() {
        let mut header = block("1. 页眉里的数字");
        header.kind = BlockKind::HeaderFooter;
        let mut figure = block("2. 图注");
        figure.kind = BlockKind::Figure;
        let report = inspect_question_patterns(&[header, figure, block("3. 题")]);
        assert_eq!(report.question_numbers, vec![3]);
        assert_eq!(report.score_annotations, 0);
    }
}
