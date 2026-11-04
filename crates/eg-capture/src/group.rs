//! 多页归组（同一学生的多页答卷 → 同一 `Submission` 多 `Page`）。
//!
//! 规则（`docs/dev/research/capture.md` §「多页归组」）：
//! - **组键**：`source_stem` 去掉可解析页码后缀后的基名，再按 `align` 同一套语义归一
//!   （全/半角、大小写、分隔符）；**再叠加来源目录哈希**，避免不同目录同名（很可能是
//!   不同学生/不同班级）被错误合并。
//! - **页序**：组内文件都有页码后缀且唯一时按页码升序；否则按文件名自然序稳定排序，
//!   并记 `page_order_fallback` 警告（不猜）。
//! - **缺页**：能确定页序时，页码不从 1 开始或中间跳号 → 记 `missing_page_range`，
//!   对应的提交状态必须是 `Failed{Capture}`（缺页不得静默当完整卷）。
//! - **重复字节**：批内去重保持「同字节只处理一次」（在 [`crate::dedup::BatchDedupe`]），
//!   本模块只对去重后仍存在的唯一文件分组。
//! - **单文件**：仍是单页提交（组内 1 个文件），不产生页序警告。
//!
//! 与 `eg-align` 的一致性：`page_suffix`/`normalize_key` 是 `eg-align::normalize` 的
//! 同语义拷贝（`eg-capture` 是上游，不能反向依赖 `align`；跨 crate 抽公共模块需先审批）。
//! 集成测试 `tests/grouping.rs` 用同一张表逐项比对两个实现，防止语义漂移。

use std::collections::BTreeMap;
use std::fmt::Write as _;

use eg_domain::Sha256Hex;

use crate::report::GroupWarningCode;

/// 归组输入（调用方已读完字节、完成批内去重）。
#[derive(Debug, Clone, Copy)]
pub struct GroupInput<'a> {
    /// 调用方数组下标（回报给调用方定位原始文件）。
    pub index: usize,
    /// 原始文件名（含扩展名；仅用于稳定排序，不落报告）。
    pub file_name: &'a str,
    /// 去扩展名文件名。
    pub source_stem: &'a str,
    /// 来源目录哈希（路径输入）；内存输入为 `None`。
    pub directory_key: Option<&'a str>,
}

/// 组内文件（按最终页序排列）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedFile {
    /// 调用方数组下标。
    pub index: usize,
    /// 原始文件名（排序依据；不落报告）。
    pub file_name: String,
    /// 去扩展名文件名（逐字保留）。
    pub source_stem: String,
    /// 去页码后缀后的基名（逐字保留）。
    pub base_stem: String,
    /// 解析出的页码后缀（1 起；无法解析为 `None`）。
    pub page_no: Option<u32>,
}

/// 一个归组（一个将来提交）。
#[derive(Debug, Clone)]
pub struct PlannedGroup {
    /// 组键哈希（归一基名 + 目录哈希；不含原文，可入报告/审计）。
    pub group_key: Sha256Hex,
    /// 组主 stem（用于 `Submission.source_stem`；取最终页序首个文件的基名）。
    pub base_stem: String,
    /// 来源目录哈希（`None` = 内存输入）。
    pub directory_key: Option<String>,
    /// 组内文件（已按最终页序）。
    pub files: Vec<PlannedFile>,
    /// 页码后缀序列（与 `files` 对齐；供报告展示页序依据）。
    pub suffix_pages: Vec<Option<u32>>,
    /// 确定页序时可判定的缺页（1 起；空 = 无缺页或页序不可判定）。
    pub missing_pages: Vec<u32>,
    /// 本组警告。
    pub warnings: Vec<GroupWarningCode>,
}

/// 跨组警告（如同 stem 出现在不同目录）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanWarning {
    /// 警告码。
    pub code: GroupWarningCode,
    /// 相关组键。
    pub group_key: Sha256Hex,
    /// 教师可读说明（不含姓名/路径原文）。
    pub detail: String,
}

/// 归组结果。
#[derive(Debug, Clone, Default)]
pub struct GroupPlan {
    /// 组（按首次出现顺序）。
    pub groups: Vec<PlannedGroup>,
    /// 跨组警告。
    pub warnings: Vec<PlanWarning>,
}

/// 为一批唯一文件规划归组。
#[must_use]
pub fn plan(inputs: &[GroupInput<'_>]) -> GroupPlan {
    // 1) 逐个文件解析基名/页号，按（归一基名, 目录哈希）归组。
    let mut buckets: BTreeMap<(String, Option<String>), Vec<PlannedFile>> = BTreeMap::new();
    let mut order: Vec<(String, Option<String>)> = Vec::new();
    for input in inputs {
        let (base, page_no) = page_suffix(input.source_stem);
        let normalized = normalize_key(&base);
        let key = if normalized.is_empty() {
            normalize_key(input.source_stem)
        } else {
            normalized
        };
        let directory = input.directory_key.map(str::to_owned);
        let bucket = (key, directory);
        if !buckets.contains_key(&bucket) {
            order.push(bucket.clone());
        }
        buckets.entry(bucket).or_default().push(PlannedFile {
            index: input.index,
            file_name: input.file_name.to_owned(),
            source_stem: input.source_stem.to_owned(),
            base_stem: base,
            page_no,
        });
    }

    // 2) 组内排序、缺页与警告。
    let mut groups = Vec::with_capacity(order.len());
    for key in order {
        let files = buckets.remove(&key).unwrap_or_default();
        let group_key = group_key(&key.0, key.1.as_deref());
        groups.push(order_group(files, group_key, key.1));
    }

    // 3) 跨组同名：同一归一基名出现在多个目录/输入方式 → 不合并，显式警告。
    let mut by_base: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for input in inputs {
        let (base, _) = page_suffix(input.source_stem);
        let normalized = normalize_key(&base);
        let key = if normalized.is_empty() {
            normalize_key(input.source_stem)
        } else {
            normalized
        };
        by_base.entry(key).or_default().push(input.index);
    }
    let mut warnings = Vec::new();
    for (base, indices) in by_base {
        let group_count = groups
            .iter()
            .filter(|group| group.files.iter().any(|file| indices.contains(&file.index)))
            .count();
        if group_count > 1 {
            let base_hash = Sha256Hex::of(base.as_bytes());
            for group in groups
                .iter()
                .filter(|group| group.files.iter().any(|file| indices.contains(&file.index)))
            {
                warnings.push(PlanWarning {
                    code: GroupWarningCode::SameStemDifferentDirectory,
                    group_key: group.group_key,
                    detail: format!(
                        "同一归一化文件名基名（hash={}）出现在不同目录或不同来源：已分别成组，未合并",
                        &base_hash.to_hex()[..12]
                    ),
                });
            }
        }
    }

    GroupPlan { groups, warnings }
}

fn order_group(
    mut files: Vec<PlannedFile>,
    group_key: Sha256Hex,
    directory_key: Option<String>,
) -> PlannedGroup {
    let mut warnings: Vec<GroupWarningCode> = Vec::new();

    // 基名逐字不一致（如「张三 p1」与「张三_p2」）：仍同组但提示。
    let mut bases: Vec<&str> = files.iter().map(|file| file.base_stem.as_str()).collect();
    bases.sort_unstable();
    bases.dedup();
    if bases.len() > 1 {
        warnings.push(GroupWarningCode::InconsistentStems);
    }

    let all_pages: Option<Vec<u32>> = files.iter().map(|file| file.page_no).collect();
    let mut missing_pages: Vec<u32> = Vec::new();
    match all_pages {
        Some(pages) if pages.len() > 1 => {
            let mut unique = pages.clone();
            unique.sort_unstable();
            unique.dedup();
            if unique.len() == pages.len() {
                files.sort_by_key(|file| file.page_no);
                missing_pages = missing_in(&unique);
            } else {
                warnings.push(GroupWarningCode::DuplicatePageNumber);
                sort_natural(&mut files);
            }
        }
        Some(pages) => {
            // 单文件：若后缀明确 >1，说明它只是多页卷中的一页。
            if let Some(page_no) = pages.first().copied()
                && page_no > 1
            {
                missing_pages = (1..page_no).collect();
            }
        }
        None => {
            if files.len() > 1 {
                warnings.push(GroupWarningCode::PageOrderFallback);
            }
            sort_natural(&mut files);
        }
    }
    // 混合（部分有后缀、部分没有）也走稳定序并警告。
    if warnings.contains(&GroupWarningCode::PageOrderFallback) {
        // 已排序，无需额外处理；保留与其他警告的先后稳定。
    }

    let base_stem = files
        .first()
        .map(|file| file.base_stem.clone())
        .unwrap_or_default();
    let suffix_pages = files.iter().map(|file| file.page_no).collect();
    PlannedGroup {
        group_key,
        base_stem,
        directory_key,
        files,
        suffix_pages,
        missing_pages,
        warnings,
    }
}

/// 能确定页序时的缺页（1 起、升序）。
fn missing_in(sorted_pages: &[u32]) -> Vec<u32> {
    let Some(last) = sorted_pages.last().copied() else {
        return Vec::new();
    };
    let present: Vec<u32> = sorted_pages.to_vec();
    (1..=last).filter(|page| !present.contains(page)).collect()
}

fn sort_natural(files: &mut [PlannedFile]) {
    files.sort_by(|left, right| {
        natural_key(&left.file_name)
            .cmp(&natural_key(&right.file_name))
            .then_with(|| left.index.cmp(&right.index))
    });
}

/// 文件名自然序键（数字段补零到 20 位；小写化）。
fn natural_key(name: &str) -> String {
    let lower = name.to_lowercase();
    let mut key = String::with_capacity(lower.len() + 8);
    let mut digits = String::new();
    for ch in lower.chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
        } else {
            if !digits.is_empty() {
                let _ = write!(key, "{digits:0>20}");
                digits.clear();
            }
            key.push(ch);
        }
    }
    if !digits.is_empty() {
        let _ = write!(key, "{digits:0>20}");
    }
    key
}

/// 归一化基名 + 目录哈希的组键（用于批内跨组重复字节判定；可入报告）。
#[must_use]
pub fn stem_group_key(source_stem: &str, directory_key: Option<&str>) -> Sha256Hex {
    let (base, _) = page_suffix(source_stem);
    let normalized = normalize_key(&base);
    let key = if normalized.is_empty() {
        normalize_key(source_stem)
    } else {
        normalized
    };
    group_key(&key, directory_key)
}

fn group_key(normalized_base: &str, directory_key: Option<&str>) -> Sha256Hex {
    let material = format!(
        "capture-group-1\u{1f}{normalized_base}\u{1f}{}",
        directory_key.unwrap_or("-")
    );
    Sha256Hex::of(material.as_bytes())
}

// ---------------------------------------------------------------------------
// 与 `eg-align::normalize` 同语义的归一（拷贝理由见模块文档；集成测试逐项比对）。
// ---------------------------------------------------------------------------

/// 全角 ASCII（U+FF01–U+FF5E）与全角空格的宽度折叠。
fn fold_width(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\u{3000}' => out.push(' '),
            '\u{FF01}'..='\u{FF5E}' => {
                let base = u32::from(ch) - 0xFEE0;
                out.push(char::from_u32(base).unwrap_or(ch));
            }
            _ => out.push(ch),
        }
    }
    out
}

/// 归一显示文本：宽度折叠 → 小写 → 连续空白折叠 → 去首尾空白。
#[must_use]
pub fn normalize_text(text: &str) -> String {
    let folded = fold_width(text);
    let mut out = String::with_capacity(folded.len());
    let mut last_space = false;
    for ch in folded.chars() {
        if ch.is_whitespace() {
            if !last_space {
                out.push(' ');
            }
            last_space = true;
        } else {
            for lower in ch.to_lowercase() {
                out.push(lower);
            }
            last_space = false;
        }
    }
    out.trim().to_owned()
}

/// 归一匹配键：显示归一后去掉分隔符（与 `eg-align::normalize::normalize_key` 一致）。
#[must_use]
pub fn normalize_key(text: &str) -> String {
    normalize_text(text)
        .chars()
        .filter(|ch| !is_separator(*ch))
        .collect()
}

fn is_separator(ch: char) -> bool {
    matches!(
        ch,
        '_' | '-' | '.' | '·' | '*' | '/' | '\\' | ' ' | ':' | '：' | '#' | '（' | '）' | '(' | ')'
    ) || ch.is_whitespace()
}

/// 去掉可解析的页码后缀（`_p1`/`p2`/`page1`/`_2`/`第N页`）；语义与 `eg-align` 相同。
#[must_use]
pub fn page_suffix(stem: &str) -> (String, Option<u32>) {
    let trimmed = stem.trim();
    if let Some(without_page) = trimmed.strip_suffix('页') {
        let chars: Vec<char> = without_page.chars().collect();
        let mut index = chars.len();
        while index > 0 && chars[index - 1].is_ascii_digit() {
            index -= 1;
        }
        if index < chars.len() && index > 0 {
            let digits: String = chars[index..].iter().collect();
            let head: String = chars[..index].iter().collect();
            let head = head.trim_end();
            if let Some(base) = head.strip_suffix('第')
                && let Ok(value) = digits.parse::<u32>()
                && (1..=99).contains(&value)
            {
                return (trim_trailing_separators(base).to_owned(), Some(value));
            }
            if let Some(base) = strip_page_marker(head)
                && let Ok(value) = digits.parse::<u32>()
                && (1..=99).contains(&value)
            {
                return (base.to_owned(), Some(value));
            }
        }
        return (trimmed.to_owned(), None);
    }
    let chars: Vec<char> = trimmed.chars().collect();
    let mut index = chars.len();
    while index > 0 && chars[index - 1].is_ascii_digit() {
        index -= 1;
    }
    if index == chars.len() || index == 0 {
        return (trimmed.to_owned(), None);
    }
    let digits: String = chars[index..].iter().collect();
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() || digits.len() > 2 {
        return (trimmed.to_owned(), None);
    }
    let head: String = chars[..index].iter().collect();
    let Some(base) = strip_page_marker(&head) else {
        return (trimmed.to_owned(), None);
    };
    let Ok(value) = digits.parse::<u32>() else {
        return (trimmed.to_owned(), None);
    };
    if !(1..=99).contains(&value) {
        return (trimmed.to_owned(), None);
    }
    (base.to_owned(), Some(value))
}

fn strip_page_marker(head: &str) -> Option<&str> {
    let head = head.trim_end();
    let lowered = head.to_lowercase();
    for marker in ["page", "p"] {
        if lowered.ends_with(marker) {
            let cut = head.len() - marker.len();
            if head.is_char_boundary(cut) {
                let base = head[..cut].trim_end();
                let base = trim_trailing_separators(base);
                if !base.is_empty() {
                    return Some(base);
                }
            }
        }
    }
    let base = head.trim_end_matches(|ch: char| ch == '_' || ch.is_whitespace());
    if base.len() == head.len() || base.is_empty() {
        None
    } else {
        Some(base)
    }
}

fn trim_trailing_separators(text: &str) -> &str {
    let mut end = text.len();
    for (index, ch) in text.char_indices().rev() {
        if is_separator(ch) {
            end = index;
        } else {
            break;
        }
    }
    text[..end].trim_end()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn input<'a>(
        index: usize,
        name: &'a str,
        stem: &'a str,
        dir: Option<&'a str>,
    ) -> GroupInput<'a> {
        GroupInput {
            index,
            file_name: name,
            source_stem: stem,
            directory_key: dir,
        }
    }

    #[test]
    fn groups_page_suffix_files_and_orders_by_page() {
        let inputs = [
            input(0, "张三_p2.png", "张三_p2", None),
            input(1, "张三_p1.png", "张三_p1", None),
        ];
        let plan = plan(&inputs);
        assert_eq!(plan.groups.len(), 1);
        let group = &plan.groups[0];
        assert_eq!(group.base_stem, "张三");
        assert_eq!(
            group
                .files
                .iter()
                .map(|file| file.file_name.as_str())
                .collect::<Vec<_>>(),
            vec!["张三_p1.png", "张三_p2.png"]
        );
        assert_eq!(group.missing_pages.len(), 0);
        assert_eq!(group.warnings.len(), 0);
        assert_eq!(plan.warnings.len(), 0);
    }

    #[test]
    fn single_file_stays_single_page() {
        let inputs = [input(0, "李四.png", "李四", None)];
        let plan = plan(&inputs);
        assert_eq!(plan.groups.len(), 1);
        assert_eq!(plan.groups[0].files.len(), 1);
        assert_eq!(plan.groups[0].missing_pages.len(), 0);
        assert_eq!(plan.groups[0].warnings.len(), 0);
    }

    #[test]
    fn single_file_with_page_two_reports_missing_first_page() {
        let plan = plan(&[input(0, "张三_p2.png", "张三_p2", None)]);
        assert_eq!(plan.groups[0].missing_pages, vec![1]);
    }

    #[test]
    fn missing_middle_page_is_detected() {
        let inputs = [
            input(0, "张三_p1.png", "张三_p1", None),
            input(1, "张三_p3.png", "张三_p3", None),
        ];
        let plan = plan(&inputs);
        assert_eq!(plan.groups[0].missing_pages, vec![2]);
    }

    #[test]
    fn unsuffixed_multi_file_falls_back_to_natural_order() {
        // 同一基名但都没有页码后缀（含空格差异）→ 无法判定页序，走自然序并警告。
        let inputs = [
            input(0, "张三.png", "张三", None),
            input(1, "张三 .jpg", "张三 ", None),
        ];
        let plan = plan(&inputs);
        assert_eq!(plan.groups.len(), 1);
        assert_eq!(
            plan.groups[0]
                .files
                .iter()
                .map(|file| file.file_name.as_str())
                .collect::<Vec<_>>(),
            vec!["张三 .jpg", "张三.png"]
        );
        assert!(
            plan.groups[0]
                .warnings
                .contains(&GroupWarningCode::PageOrderFallback)
        );
    }

    #[test]
    fn duplicate_page_numbers_are_flagged_and_sorted_stably() {
        let inputs = [
            input(0, "张三_p1.png", "张三_p1", None),
            input(1, "张三_1.jpg", "张三_1", None),
        ];
        let plan = plan(&inputs);
        assert!(
            plan.groups[0]
                .warnings
                .contains(&GroupWarningCode::DuplicatePageNumber)
        );
    }

    #[test]
    fn same_stem_in_different_directories_is_not_merged() {
        let inputs = [
            input(0, "张三_p1.png", "张三_p1", Some("dir-a-hash")),
            input(1, "张三_p2.png", "张三_p2", Some("dir-b-hash")),
        ];
        let plan = plan(&inputs);
        assert_eq!(plan.groups.len(), 2, "不同目录同名不得合并");
        assert_eq!(plan.warnings.len(), 2);
        assert!(
            plan.warnings
                .iter()
                .all(|warning| warning.code == GroupWarningCode::SameStemDifferentDirectory)
        );
    }

    #[test]
    fn inconsistent_base_spellings_join_one_group_with_warning() {
        let inputs = [
            input(0, "张 三_p1.png", "张 三_p1", None),
            input(1, "张三_p2.png", "张三_p2", None),
        ];
        let plan = plan(&inputs);
        assert_eq!(plan.groups.len(), 1);
        assert!(
            plan.groups[0]
                .warnings
                .contains(&GroupWarningCode::InconsistentStems)
        );
    }

    #[test]
    fn page_suffix_cases_match_align_semantics() {
        for (stem, expected_base, expected_page) in [
            ("张三_p1", "张三", Some(1)),
            ("张三p2", "张三", Some(2)),
            ("张三_第3页", "张三", Some(3)),
            ("张三_2", "张三", Some(2)),
            ("张三", "张三", None),
            ("2026-11", "2026-11", None),
            ("20260001", "20260001", None),
            ("IMG_0032", "IMG", Some(32)),
        ] {
            assert_eq!(
                page_suffix(stem),
                (expected_base.to_owned(), expected_page),
                "{stem}"
            );
        }
    }
}
