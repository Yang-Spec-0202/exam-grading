//! 四层归一化流水线（`SPEC-subject-math.md` §3）。
//!
//! 严格按序执行，任一阶段失败即返回类型化错误并保留原因：
//! 1. **字符层**：Unicode NFKC（全角/半角、数学字母、兼容字符）、去零宽字符、
//!    统一运算符（×/·/÷/−）、上标/下标展开为 `^`/`_`；
//! 2. **噪声层**：去「解：」「答：」「原式=」等前缀、去句末标点、合并空白；
//!    在纯数值上下文中修正 OCR 常见误识别（`l`/`I` → `1`、`O` → `0`），
//!    每次修正写入 [`NormalizedMath::corrections`]（供 platform 审计落库）；
//! 3. **LaTeX 规范层**：去 `$`/`\(`/`\)`/`\[`/`\]`、解包 `\text{}`/`\mathrm{}`
//!    （单位文本保留）、花括号配对校验；
//! 4. **结构层**：解析为语法树并规范化；失败返回 [`NormalizeError::Unparsable`]，
//!    超出支持范围的可识别写法解析为函数节点（比较器「无法判定」），不猜测。
//!
//! 产物：规范化文本、规范化 LaTeX、可求值时的结构化数值、步骤列表（T15 后续）。
//! 幂等：对产物再次归一化得到同一结果（测试覆盖）。

use eg_domain::{AnswerValue, Decimal, NormalizeError, NormalizedAnswer, RawAnswer};
use unicode_normalization::UnicodeNormalization as _;

use crate::ast::{Expr, Unit};
use crate::canonical::{canonicalize, evaluate};
use crate::comparators::{parse_boolean, parse_options};
use crate::number::Rational;
use crate::parse::parse_expr;

/// 归一化输入长度上限（规范 §10 快速失败；答案 ≤2KB）。
const MAX_INPUT_CHARS: usize = 2_000;

/// 文本答案长度上限（规范 §5 第 14 项：长度超限 → 无法判定）。
const MAX_TEXT_CHARS: usize = 40;

/// 修正所属层（写入审计/研究记录用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorrectionLayer {
    /// 字符层。
    Character,
    /// 噪声层（含 OCR 修正）。
    Noise,
    /// LaTeX 规范层。
    Latex,
}

/// 一条归一化修正记录（规范 §3 噪声层「每次修正必须记录进审计」）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Correction {
    /// 所属层。
    pub layer: CorrectionLayer,
    /// 可读说明（中文）。
    pub detail: String,
}

/// 归一化产物 + 修正记录（`SubjectPack::normalize` 只返回前者，审计由 platform 落库）。
#[derive(Debug, Clone, PartialEq)]
pub struct NormalizedMath {
    /// 领域层归一化答案。
    pub answer: NormalizedAnswer,
    /// 修正记录（按发生顺序）。
    pub corrections: Vec<Correction>,
}

/// 归一化原始答案（`SubjectPack::normalize` 的实现）。
///
/// # Errors
///
/// 无法解析返回 [`NormalizeError::Unparsable`]；超长/数值超限/超出支持范围返回
/// [`NormalizeError::Unsupported`]。
pub fn normalize_raw(raw: &RawAnswer) -> Result<NormalizedAnswer, NormalizeError> {
    normalize_with_trace(raw).map(|normalized| normalized.answer)
}

/// 归一化并返回修正记录（供 platform 审计；规范 §3 噪声层要求）。
///
/// # Errors
///
/// 同 [`normalize_raw`]。
pub fn normalize_with_trace(raw: &RawAnswer) -> Result<NormalizedMath, NormalizeError> {
    let mut corrections = Vec::new();
    let source = raw.latex.clone().unwrap_or_else(|| raw.text.clone());
    if source.trim().is_empty() {
        return Err(NormalizeError::Unparsable {
            input: source,
            reason: "答案为空".to_owned(),
        });
    }
    if source.chars().count() > MAX_INPUT_CHARS {
        return Err(NormalizeError::Unsupported {
            reason: format!("输入过长（超过 {MAX_INPUT_CHARS} 字符），转人工"),
        });
    }
    // 判断题/选项题在字符层符号统一之前识别（`×`/`√` 会被统一为运算符，之后无法区分）
    if let Some(answer) = early_non_math_answer(&source) {
        corrections.push(Correction {
            layer: CorrectionLayer::Noise,
            detail: "噪声层：识别为判断题/选项题答案（不走数学解析）".to_owned(),
        });
        return Ok(NormalizedMath {
            answer,
            corrections,
        });
    }
    let char_layer = character_layer(&source, &mut corrections);
    let noise_layer = noise_layer(&char_layer, &mut corrections);
    let latex_layer = latex_layer(&noise_layer, &mut corrections)?;
    if latex_layer.trim().is_empty() {
        return Err(NormalizeError::Unparsable {
            input: source,
            reason: "去除噪声后答案为空".to_owned(),
        });
    }
    let answer = structure_layer(&latex_layer, &source)?;
    Ok(NormalizedMath {
        answer,
        corrections,
    })
}

// ---------------------------------------------------------------------------
// 第 1 层：字符层
// ---------------------------------------------------------------------------

fn character_layer(input: &str, corrections: &mut Vec<Correction>) -> String {
    // 上标/下标先展开（NFKC 会把 ² 变成普通 2，破坏幂语义）
    let expanded = expand_scripts(input, corrections);
    let normalized: String = expanded.nfkc().collect();
    let mut output = String::with_capacity(normalized.len());
    for ch in normalized.chars() {
        match ch {
            // 零宽字符/方向控制符
            '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{feff}' | '\u{2060}' => {
                corrections.push(Correction {
                    layer: CorrectionLayer::Character,
                    detail: format!("去除零宽字符 U+{:04X}", u32::from(ch)),
                });
            }
            '×' | '·' | '⋅' | '∙' => output.push('*'),
            '÷' | '∕' => output.push('/'),
            '−' | '–' | '—' => output.push('-'),
            '（' => output.push('('),
            '）' => output.push(')'),
            '：' => output.push(':'),
            other => output.push(other),
        }
    }
    // `√` 后紧跟数字/字母时按根号处理；独立 `√` 保留给判断题比较器
    let mut final_output = String::with_capacity(output.len());
    let chars: Vec<char> = output.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '√' {
            let next = chars.get(index + 1).copied();
            let is_operand = next.is_some_and(|next| {
                next.is_ascii_digit()
                    || next.is_ascii_alphabetic()
                    || matches!(next, '(' | '{' | '[')
            });
            if is_operand {
                let rest: String = chars[index + 1..].iter().collect();
                let (operand, consumed) = take_math_operand(&rest);
                if let Some(operand) = operand {
                    final_output.push_str("\\sqrt{");
                    final_output.push_str(&operand);
                    final_output.push('}');
                    corrections.push(Correction {
                        layer: CorrectionLayer::Character,
                        detail: "字符层：√ 归一化为 \\sqrt{}".to_owned(),
                    });
                    index += 1 + consumed;
                    continue;
                }
                final_output.push_str("\\sqrt");
            } else {
                final_output.push('√');
            }
            index += 1;
            continue;
        }
        final_output.push(ch);
        index += 1;
    }
    if final_output != input {
        corrections.push(Correction {
            layer: CorrectionLayer::Character,
            detail: "字符层：Unicode 规范化/运算符统一/去零宽字符".to_owned(),
        });
    }
    final_output
}

fn expand_scripts(input: &str, corrections: &mut Vec<Correction>) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut output = String::with_capacity(input.len());
    let mut index = 0;
    let mut changed = false;
    while index < chars.len() {
        let Some((is_subscript, _)) = script_marker(chars[index]) else {
            output.push(chars[index]);
            index += 1;
            continue;
        };
        let mut digits = String::new();
        let mut negative = false;
        while index < chars.len() {
            match script_marker(chars[index]) {
                Some((kind, value)) if kind == is_subscript => {
                    match value {
                        '-' => negative = true,
                        '+' => {}
                        other => digits.push(other),
                    }
                    index += 1;
                    changed = true;
                }
                _ => break,
            }
        }
        if is_subscript {
            // 下标并入变量名（`x_1`），不加花括号，保持可再次解析
            output.push('_');
            output.push_str(&digits);
        } else {
            output.push('^');
            output.push('{');
            if negative {
                output.push('-');
            }
            output.push_str(&digits);
            output.push('}');
        }
    }
    if changed {
        corrections.push(Correction {
            layer: CorrectionLayer::Character,
            detail: "字符层：上标/下标展开为 ^/_".to_owned(),
        });
    }
    output
}

/// 返回（是否下标，数字字符）。
/// 取根号后的操作数：连续数字/小数，或单个字母/括号组（返回操作数文本与消耗字符数）。
fn take_math_operand(rest: &str) -> (Option<String>, usize) {
    let chars: Vec<char> = rest.chars().collect();
    if chars.is_empty() {
        return (None, 0);
    }
    if chars[0].is_ascii_digit() || chars[0] == '.' {
        let mut index = 0;
        while index < chars.len() && (chars[index].is_ascii_digit() || chars[index] == '.') {
            index += 1;
        }
        return (Some(chars[..index].iter().collect()), index);
    }
    if chars[0].is_ascii_alphabetic() {
        return (Some(chars[0].to_string()), 1);
    }
    if chars[0] == '(' {
        let mut depth = 0;
        for (index, ch) in chars.iter().enumerate() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return (Some(chars[..=index].iter().collect()), index + 1);
                    }
                }
                _ => {}
            }
        }
    }
    (None, 0)
}

fn script_marker(ch: char) -> Option<(bool, char)> {
    match ch {
        '⁰' => Some((false, '0')),
        '¹' => Some((false, '1')),
        '²' => Some((false, '2')),
        '³' => Some((false, '3')),
        '⁴' => Some((false, '4')),
        '⁵' => Some((false, '5')),
        '⁶' => Some((false, '6')),
        '⁷' => Some((false, '7')),
        '⁸' => Some((false, '8')),
        '⁹' => Some((false, '9')),
        '⁺' => Some((false, '+')),
        '⁻' => Some((false, '-')),
        'ⁿ' => Some((false, 'n')),
        '₀' => Some((true, '0')),
        '₁' => Some((true, '1')),
        '₂' => Some((true, '2')),
        '₃' => Some((true, '3')),
        '₄' => Some((true, '4')),
        '₅' => Some((true, '5')),
        '₆' => Some((true, '6')),
        '₇' => Some((true, '7')),
        '₈' => Some((true, '8')),
        '₉' => Some((true, '9')),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// 第 2 层：噪声层
// ---------------------------------------------------------------------------

const PREFIXES: &[&str] = &[
    "解:", "答:", "原式=", "原式:", "所以", "因此", "故", "则", "得", "解", "答",
];

fn noise_layer(input: &str, corrections: &mut Vec<Correction>) -> String {
    let mut text = input.trim().to_owned();
    let mut stripped = true;
    while stripped {
        stripped = false;
        for prefix in PREFIXES {
            if let Some(rest) = text.strip_prefix(prefix) {
                let rest = rest.trim_start();
                if !rest.is_empty() {
                    corrections.push(Correction {
                        layer: CorrectionLayer::Noise,
                        detail: format!("噪声层：去除前缀 {prefix:?}"),
                    });
                    text = rest.to_owned();
                    stripped = true;
                    break;
                }
            }
        }
    }
    // 句末标点
    let mut trimmed = text.trim_end();
    while let Some(last) = trimmed.chars().last() {
        if matches!(
            last,
            '。' | '；' | ';' | '，' | ',' | '、' | '！' | '!' | '?'
        ) {
            trimmed = trimmed[..trimmed.len() - last.len_utf8()].trim_end();
        } else {
            break;
        }
    }
    if trimmed.len() != text.len() {
        corrections.push(Correction {
            layer: CorrectionLayer::Noise,
            detail: "噪声层：去除句末标点".to_owned(),
        });
    }
    // 合并空白
    let collapsed: String = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut text = if collapsed.is_empty() {
        trimmed.to_owned()
    } else {
        collapsed
    };
    // 纯数值上下文 OCR 修正：1O.5 → 10.5、l2 → 12
    if let Some(fixed) = fix_numeric_ocr(&text) {
        corrections.push(Correction {
            layer: CorrectionLayer::Noise,
            detail: format!("噪声层：数值 OCR 修正 {text:?} → {fixed:?}"),
        });
        text = fixed;
    }
    // 中文十进制逗号：整段是「数字，数字」时视为小数点
    if let Some(fixed) = fix_decimal_comma(&text) {
        corrections.push(Correction {
            layer: CorrectionLayer::Noise,
            detail: format!("噪声层：十进制逗号修正 {text:?} → {fixed:?}"),
        });
        text = fixed;
    }
    text
}

fn fix_numeric_ocr(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut output = String::with_capacity(text.len());
    let mut index = 0;
    let mut changed = false;
    while index < chars.len() {
        let ch = chars[index];
        if ch.is_ascii_digit() || matches!(ch, '.' | 'l' | 'I' | 'O') {
            let start = index;
            while index < chars.len()
                && (chars[index].is_ascii_digit() || matches!(chars[index], '.' | 'l' | 'I' | 'O'))
            {
                index += 1;
            }
            let token: String = chars[start..index].iter().collect();
            let has_digit = token.chars().any(|ch| ch.is_ascii_digit());
            let has_letter = token.chars().any(|ch| matches!(ch, 'l' | 'I' | 'O'));
            if has_digit && has_letter {
                for ch in token.chars() {
                    output.push(match ch {
                        'l' | 'I' => '1',
                        'O' => '0',
                        other => other,
                    });
                }
                changed = true;
            } else {
                output.push_str(&token);
            }
        } else {
            output.push(ch);
            index += 1;
        }
    }
    changed.then_some(output)
}

fn fix_decimal_comma(text: &str) -> Option<String> {
    let mut parts = text.split(['，', ',']);
    let first = parts.next()?;
    let second = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    if first.chars().all(|ch| ch.is_ascii_digit())
        && !first.is_empty()
        && second.chars().all(|ch| ch.is_ascii_digit())
        && !second.is_empty()
    {
        return Some(format!("{first}.{second}"));
    }
    None
}

// ---------------------------------------------------------------------------
// 第 3 层：LaTeX 规范层
// ---------------------------------------------------------------------------

fn latex_layer(input: &str, corrections: &mut Vec<Correction>) -> Result<String, NormalizeError> {
    let mut text = input.trim().to_owned();
    let before = text.clone();
    for delimiter in ["$$", "$", "\\[", "\\]", "\\(", "\\)"] {
        if text.contains(delimiter) {
            text = text.replace(delimiter, "");
        }
    }
    // 解包 \text{...}/\mathrm{...}：单位文本保留为 \text{...}，其余展开
    let mut output = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '\\' {
            let (command, next) = read_command(&chars, index);
            if matches!(
                command.as_str(),
                "text" | "mathrm" | "mbox" | "operatorname"
            ) {
                let (content, after) =
                    read_braced(&chars, next).ok_or_else(|| NormalizeError::Unparsable {
                        input: before.clone(),
                        reason: format!("\\{command} 缺少花括号内容"),
                    })?;
                let is_unit = Unit::parse(&content).is_some();
                if is_unit {
                    output.push_str("\\text{");
                    output.push_str(&content);
                    output.push('}');
                } else {
                    output.push_str(&content);
                }
                index = after;
                continue;
            }
            if matches!(
                command.as_str(),
                "left" | "right" | "displaystyle" | "textstyle" | "limits" | "nolimits"
            ) {
                index = next;
                continue;
            }
            output.push('\\');
            output.push_str(&command);
            index = next;
            continue;
        }
        output.push(ch);
        index += 1;
    }
    let text = output;
    // 花括号配对校验
    let mut depth = 0i32;
    let mut escaped = false;
    for ch in text.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth < 0 {
                    return Err(NormalizeError::Unparsable {
                        input: before,
                        reason: "花括号不配对（多余的 }）".to_owned(),
                    });
                }
            }
            _ => {}
        }
    }
    if depth != 0 {
        return Err(NormalizeError::Unparsable {
            input: before,
            reason: "花括号不配对（缺少 }）".to_owned(),
        });
    }
    if text != before {
        corrections.push(Correction {
            layer: CorrectionLayer::Latex,
            detail: "LaTeX 规范层：去定界符/解包文本命令".to_owned(),
        });
    }
    Ok(text)
}

fn read_command(chars: &[char], start: usize) -> (String, usize) {
    let mut index = start + 1;
    let mut command = String::new();
    while index < chars.len() && chars[index].is_ascii_alphabetic() {
        command.push(chars[index]);
        index += 1;
    }
    if command.is_empty() && index < chars.len() {
        command.push(chars[index]);
        index += 1;
    }
    (command, index)
}

fn read_braced(chars: &[char], start: usize) -> Option<(String, usize)> {
    let mut index = start;
    while index < chars.len() && chars[index].is_whitespace() {
        index += 1;
    }
    if index >= chars.len() || chars[index] != '{' {
        return None;
    }
    index += 1;
    let mut depth = 1usize;
    let mut content = String::new();
    while index < chars.len() {
        let ch = chars[index];
        if ch == '\\' && index + 1 < chars.len() {
            content.push(ch);
            content.push(chars[index + 1]);
            index += 2;
            continue;
        }
        match ch {
            '{' => {
                depth += 1;
                content.push(ch);
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((content, index + 1));
                }
                content.push(ch);
            }
            other => content.push(other),
        }
        index += 1;
    }
    None
}

// ---------------------------------------------------------------------------
// 第 4 层：结构层
// ---------------------------------------------------------------------------

/// 在字符层符号统一前识别判断题/选项题（保留 `×`/`√` 等判断符号语义）。
fn early_non_math_answer(source: &str) -> Option<NormalizedAnswer> {
    let mut text = source.trim().to_owned();
    let mut stripped = true;
    while stripped {
        stripped = false;
        for prefix in PREFIXES {
            if let Some(rest) = text.strip_prefix(prefix) {
                let rest = rest.trim_start();
                if !rest.is_empty() {
                    text = rest.to_owned();
                    stripped = true;
                    break;
                }
            }
        }
    }
    let text = text.trim().to_owned();
    if let Some(value) = parse_boolean(&text) {
        return Some(NormalizedAnswer {
            text: if value { "true" } else { "false" }.to_owned(),
            latex: None,
            value: None,
            steps: Vec::new(),
        });
    }
    if let Some(options) = parse_options(&text) {
        return Some(NormalizedAnswer {
            text: options.join(""),
            latex: None,
            value: None,
            steps: Vec::new(),
        });
    }
    None
}

fn structure_layer(input: &str, original: &str) -> Result<NormalizedAnswer, NormalizeError> {
    let trimmed = input.trim();
    // 判断题：先于数学解析（避免「true」被当作字母乘积）
    if let Some(value) = parse_boolean(trimmed) {
        return Ok(NormalizedAnswer {
            text: if value { "true" } else { "false" }.to_owned(),
            latex: None,
            value: None,
            steps: Vec::new(),
        });
    }
    // 选项题：连续 A–Z 字母（可带分隔符）
    if let Some(options) = parse_options(trimmed) {
        return Ok(NormalizedAnswer {
            text: options.join(""),
            latex: None,
            value: None,
            steps: Vec::new(),
        });
    }
    // 纯字母单词（≥4 个字母、非数学函数/常量）按文本答案处理，避免被解析为字母乘积
    if is_word_answer(trimmed) {
        return Ok(NormalizedAnswer {
            text: collapse_whitespace(trimmed),
            latex: None,
            value: None,
            steps: Vec::new(),
        });
    }
    match parse_expr(trimmed) {
        Ok(expr) => {
            let canonical = canonicalize(&expr);
            Ok(build_answer(&canonical))
        }
        Err(error) => {
            if is_text_answer(trimmed) {
                Ok(NormalizedAnswer {
                    text: collapse_whitespace(trimmed),
                    latex: None,
                    value: None,
                    steps: Vec::new(),
                })
            } else {
                Err(NormalizeError::Unparsable {
                    input: original.to_owned(),
                    reason: error.to_string(),
                })
            }
        }
    }
}

fn build_answer(canonical: &Expr) -> NormalizedAnswer {
    let value = match evaluate(canonical) {
        Ok(rational) => rational_to_decimal(&rational).map(|decimal| AnswerValue::Number {
            value: decimal,
            unit: None,
            tolerance: None,
        }),
        Err(_) => None,
    };
    NormalizedAnswer {
        text: canonical.to_text(),
        latex: Some(canonical.to_latex()),
        value,
        steps: Vec::new(),
    }
}

/// 把有理数精确转成域层 `Decimal`（仅有限小数且不超精度）。
pub(crate) fn rational_to_decimal(value: &Rational) -> Option<Decimal> {
    let text = value.to_finite_decimal()?;
    Decimal::parse(&text).ok()
}

fn is_word_answer(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    chars.len() >= 4
        && chars.iter().all(char::is_ascii_alphabetic)
        && !matches!(
            text.to_ascii_lowercase().as_str(),
            "sqrt" | "sinx" | "cosx" | "true" | "false"
        )
}

fn is_text_answer(text: &str) -> bool {
    let length = text.chars().count();
    if length == 0 || length > MAX_TEXT_CHARS {
        return false;
    }
    if text.chars().any(|ch| {
        ch.is_ascii_digit()
            || matches!(
                ch,
                '+' | '-'
                    | '*'
                    | '/'
                    | '^'
                    | '_'
                    | '{'
                    | '}'
                    | '['
                    | ']'
                    | '('
                    | ')'
                    | '='
                    | '<'
                    | '>'
                    | '|'
                    | '\\'
                    | '√'
                    | '≤'
                    | '≥'
                    | '≠'
                    | '∞'
                    | 'π'
            )
    }) {
        return false;
    }
    // 纯空白/纯标点不算答案
    text.chars().any(|ch| ch.is_alphabetic() || !ch.is_ascii())
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use eg_domain::AnswerSource;

    fn raw(text: &str) -> RawAnswer {
        RawAnswer {
            text: text.to_owned(),
            latex: None,
            blocks: Vec::new(),
            source: AnswerSource::Recognition,
        }
    }

    fn normalize(text: &str) -> NormalizedAnswer {
        normalize_raw(&raw(text)).unwrap()
    }

    #[test]
    fn character_layer_normalizes_unicode_and_symbols() {
        assert_eq!(normalize("１２．５").text, "12.5");
        assert_eq!(normalize("2×3").text, "6");
        assert_eq!(normalize("6÷2").text, "3");
        assert_eq!(normalize("x−1").text, "x - 1");
        assert_eq!(normalize("x²").text, "x^2");
        assert_eq!(normalize("x₁").text, "x_1");
        assert_eq!(normalize("\\sqrt{9}").text, "3");
        assert_eq!(normalize("√9").text, "3");
    }

    #[test]
    fn noise_layer_strips_prefix_and_punctuation() {
        assert_eq!(normalize("解：x=1。").text, "x=1");
        assert_eq!(normalize("答: 2.5").text, "2.5");
        assert_eq!(normalize("原式=1+2").text, "3");
        assert_eq!(normalize("  1  +  2  ").text, "3");
    }

    #[test]
    fn ocr_numeric_fixes_are_recorded() {
        let normalized = normalize_with_trace(&raw("1O.5")).unwrap();
        assert_eq!(normalized.answer.text, "10.5");
        assert!(
            normalized
                .corrections
                .iter()
                .any(|correction| correction.detail.contains("OCR"))
        );
        let normalized = normalize_with_trace(&raw("1，5")).unwrap();
        assert_eq!(normalized.answer.text, "1.5");
        assert!(
            normalized
                .corrections
                .iter()
                .any(|correction| correction.detail.contains("逗号"))
        );
    }

    #[test]
    fn latex_layer_handles_delimiters_and_text() {
        assert_eq!(normalize("$\\frac{1}{2}$").text, "0.5");
        assert_eq!(normalize("\\(x+1\\)").text, "x + 1");
        assert_eq!(normalize("\\left(x+1\\right)").text, "x + 1");
        assert_eq!(normalize("12\\text{cm}").text, "12\\text{cm}");
        assert_eq!(normalize("12\\mathrm{cm}").text, "12\\text{cm}");
        assert!(normalize_raw(&raw("\\frac{1}{2")).is_err());
    }

    #[test]
    fn structure_layer_builds_values_and_steps() {
        let answer = normalize("2.5");
        assert_eq!(answer.text, "2.5");
        assert!(answer.latex.is_some());
        assert!(matches!(answer.value, Some(AnswerValue::Number { .. })));
        assert_eq!(answer.steps.len(), 0);

        let answer = normalize("\\frac{1}{2}");
        assert_eq!(answer.text, "0.5");
        assert!(matches!(answer.value, Some(AnswerValue::Number { .. })));

        let answer = normalize("x=1");
        assert_eq!(answer.text, "x=1");
        assert!(answer.value.is_none());
    }

    #[test]
    fn boolean_and_option_answers() {
        assert_eq!(normalize("对").text, "true");
        assert_eq!(normalize("√").text, "true");
        assert_eq!(normalize("错误").text, "false");
        assert_eq!(normalize("B A").text, "AB");
        assert_eq!(normalize("b,a").text, "AB");
    }

    #[test]
    fn text_answers_fall_back_to_text() {
        assert_eq!(normalize("氢").text, "氢");
        assert_eq!(normalize("answer").text, "answer");
        // 含公式/数字的无法解析输入 → 归一化错误，而不是「答错」
        assert!(normalize_raw(&raw("x^")).is_err());
        assert!(normalize_raw(&raw("1..2")).is_err());
    }

    #[test]
    fn unsupported_but_recognizable_constructs_normalize() {
        let answer = normalize("\\int x");
        assert!(answer.latex.is_some());
        // 三角表达式归一化成功，比较时由比较器返回无法判定
        assert!(normalize("\\sin(x)").latex.is_some());
    }

    #[test]
    fn normalization_is_idempotent() {
        for input in [
            "2.5",
            "\\frac{1}{2}",
            "x^2+2*x+1",
            "\\sqrt{12}",
            "x=1 或 x=2",
            "[1,2)",
            "\\{1,2,2\\}",
            "12\\text{cm}",
            "对",
            "AC",
            "氢",
            "\\sin(x)",
        ] {
            let once = normalize(input);
            let twice = normalize_raw(&RawAnswer {
                text: once.text.clone(),
                latex: once.latex.clone(),
                blocks: Vec::new(),
                source: AnswerSource::Recognition,
            })
            .unwrap();
            assert_eq!(once, twice, "{input}");
        }
    }

    #[test]
    fn oversized_input_is_unsupported() {
        let long = "1".repeat(MAX_INPUT_CHARS + 1);
        assert!(matches!(
            normalize_raw(&raw(&long)),
            Err(NormalizeError::Unsupported { .. })
        ));
    }
}
