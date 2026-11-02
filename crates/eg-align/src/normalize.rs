//! 文件名/题号文本归一（`SPEC-align.md` §3.2「文件名归一」、§3.3「题号归一」）。
//!
//! 纯函数、无外部依赖；所有匹配路径共用同一套归一结果，保证「同一输入同一结论」。

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

/// 归一匹配键：在 [`normalize_text`] 基础上去掉分隔符（`_ - . · * / \` 与空白）。
///
/// 用于学号/姓名的宽容相等判断；CJK 与字母数字保留。
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

/// 按分隔符切词（宽度折叠 + 小写后）。
#[must_use]
pub fn tokenize(text: &str) -> Vec<String> {
    normalize_text(text)
        .split(|ch: char| is_separator(ch) || ch.is_whitespace())
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .collect()
}

/// 去掉可解析的页码后缀：`_p1`、`-2`、`_第2页`、`p3`、`(_p1)`。
///
/// 返回（基名, 页码）。只识别「末尾 1–2 位数字 + 页码标记/分隔符」的保守形态；
/// 无把握时原样返回、页码 `None`（不猜）。
#[must_use]
pub fn page_suffix(stem: &str) -> (String, Option<u32>) {
    let trimmed = stem.trim();
    // 形态 1：`...第2页` / `...第 2 页`
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
    // 形态 2：末尾数字（可选页码标记 p/P 或分隔符前缀）。
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

/// 页码标记收尾剥离：只认 `p`/`page` 与下划线分隔（规范列出的 `_1`/`p1`/`第1页`）。
///
/// 刻意**不**把 `-`/`.`/空格 + 数字当页码：`2026-11` 这类日期/学号后缀不能被误当页序。
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

/// 题型题号的「主号」（如 `17(2)` → 17，`l7.` → 7）；找不到返回 `None`。
#[must_use]
pub fn main_number(label: &str) -> Option<u32> {
    let text = normalize_text(label);
    let digits: String = text
        .chars()
        .skip_while(|ch| anchor_noise(*ch))
        .take_while(char::is_ascii_digit)
        .collect();
    if digits.is_empty() || digits.len() > 3 {
        return None;
    }
    digits.parse::<u32>().ok().filter(|value| *value > 0)
}

/// 子号（`17(2)` → 2；`17(2)(3)` 只取第一个括号组）；没有返回 `None`。
#[must_use]
pub fn sub_number(label: &str) -> Option<u32> {
    let text = normalize_text(label);
    let open = text.find(['(', '（'])?;
    let rest = &text[open + 1..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<u32>().ok().filter(|value| *value > 0)
}

/// 识别块文本开头的题号锚点（容忍 OCR 噪声：`l7.`、`|7`、`17、`、`7)`）。
///
/// 只解析开头的数字；是否真的是某道题的题号由调用方用试卷题号集合校验。
#[must_use]
pub fn leading_anchor_number(text: &str) -> Option<u32> {
    main_number(text)
}

/// 常见 OCR 噪声字符归一为数字 1（仅在题号开头位置使用）。
fn anchor_noise(ch: char) -> bool {
    matches!(
        ch,
        'l' | 'L' | 'I' | '|' | '!' | 'i' | 'j' | 't' | 'f' | ' '
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn normalizes_width_case_and_spaces() {
        assert_eq!(normalize_text("  张三＿期中  "), "张三_期中");
        assert_eq!(normalize_text("ＡＢＣ　Ｄｅｆ"), "abc def");
        assert_eq!(normalize_key("张三_期中-2026"), "张三期中2026");
        assert_eq!(normalize_text("张  三"), "张 三");
    }

    #[test]
    fn tokenizes_on_separators() {
        assert_eq!(tokenize("张三_期中(1)"), vec!["张三", "期中", "1"]);
        assert_eq!(tokenize("IMG_0032"), vec!["img", "0032"]);
    }

    #[test]
    fn strips_page_suffixes() {
        assert_eq!(page_suffix("张三_p1"), ("张三".to_owned(), Some(1)));
        assert_eq!(page_suffix("张三-2"), ("张三-2".to_owned(), None));
        assert_eq!(page_suffix("张三_2"), ("张三".to_owned(), Some(2)));
        assert_eq!(page_suffix("张三_第3页"), ("张三".to_owned(), Some(3)));
        assert_eq!(page_suffix("张三p2"), ("张三".to_owned(), Some(2)));
        assert_eq!(page_suffix("张三"), ("张三".to_owned(), None));
        // 学号尾数字不能被误当页码（无标记/分隔符）。
        assert_eq!(page_suffix("20260001"), ("20260001".to_owned(), None));
        // 年份样式的四位数不当作页码。
        assert_eq!(page_suffix("张三_2026"), ("张三_2026".to_owned(), None));
    }

    #[test]
    fn parses_main_and_sub_numbers() {
        assert_eq!(main_number("17(2)"), Some(17));
        assert_eq!(main_number("17（2）"), Some(17));
        assert_eq!(main_number("l7."), Some(7));
        assert_eq!(main_number("|7、"), Some(7));
        assert_eq!(main_number("  8 "), Some(8));
        assert_eq!(main_number("第9题"), None);
        assert_eq!(sub_number("17(2)"), Some(2));
        assert_eq!(sub_number("17"), None);
    }

    #[test]
    fn page_suffix_does_not_break_clean_names() {
        for stem in ["张三", "李四_期中", "王五 期末 试卷", "2026-11"] {
            let (base, page) = page_suffix(stem);
            assert_eq!(page, None, "{stem}");
            assert_eq!(base, stem.trim().to_owned(), "{stem}");
        }
    }
}
