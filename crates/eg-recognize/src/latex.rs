//! LaTeX 规范化与词法/结构校验（`SPEC-recognize.md` §3.3）。
//!
//! 只做确定性、幂等、不改语义的规范化，以及词法/结构级校验（平衡、环境配对、
//! 命令白名单与参数个数、禁止命令、长度与字符集）；**不做语义求值，也不猜测修复**。
//! 结论只有「合法（附规范化结果）」与「非法（附原因）」；非法即该块不可用，转人工。

/// 公式块 LaTeX 长度上限（与 `schema.rs` 一致）。
pub const MAX_LATEX_CHARS: usize = 2000;

/// LaTeX 校验结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LatexOutcome {
    /// 合法：给出规范化后的 LaTeX（已去定界符、Unicode 已映射、空白已修剪）。
    Valid {
        /// 规范化结果。
        latex: String,
    },
    /// 非法：给出原因（用于日志与人工解释，不写入 `Block.latex`）。
    Invalid {
        /// 非法原因。
        reason: String,
    },
}

impl LatexOutcome {
    /// 是否合法。
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        matches!(self, Self::Valid { .. })
    }

    /// 合法时的规范化 LaTeX。
    #[must_use]
    pub fn latex(&self) -> Option<&str> {
        match self {
            Self::Valid { latex } => Some(latex),
            Self::Invalid { .. } => None,
        }
    }

    /// 非法原因。
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Valid { .. } => None,
            Self::Invalid { reason } => Some(reason),
        }
    }
}

/// 常见 Unicode 数学符号到 LaTeX 命令的白名单映射。
const SYMBOL_MAP: &[(char, &str)] = &[
    ('×', "\\times "),
    ('÷', "\\div "),
    ('≤', "\\leq "),
    ('≥', "\\geq "),
    ('≠', "\\neq "),
    ('≈', "\\approx "),
    ('≡', "\\equiv "),
    ('±', "\\pm "),
    ('∓', "\\mp "),
    ('∞', "\\infty "),
    ('∑', "\\sum "),
    ('∏', "\\prod "),
    ('∫', "\\int "),
    ('√', "\\sqrt"),
    ('∂', "\\partial "),
    ('∇', "\\nabla "),
    ('→', "\\to "),
    ('←', "\\leftarrow "),
    ('↔', "\\leftrightarrow "),
    ('⇒', "\\Rightarrow "),
    ('⇐', "\\Leftarrow "),
    ('⇔', "\\Leftrightarrow "),
    ('∈', "\\in "),
    ('∉', "\\notin "),
    ('⊂', "\\subset "),
    ('⊆', "\\subseteq "),
    ('⊃', "\\supset "),
    ('⊇', "\\supseteq "),
    ('∪', "\\cup "),
    ('∩', "\\cap "),
    ('∅', "\\emptyset "),
    ('∀', "\\forall "),
    ('∃', "\\exists "),
    ('∠', "\\angle "),
    ('⊥', "\\perp "),
    ('∥', "\\parallel "),
    ('°', "^{\\circ}"),
    ('∴', "\\therefore "),
    ('∵', "\\because "),
    ('·', "\\cdot "),
    ('⋅', "\\cdot "),
    ('…', "\\ldots "),
    ('π', "\\pi "),
    ('α', "\\alpha "),
    ('β', "\\beta "),
    ('γ', "\\gamma "),
    ('δ', "\\delta "),
    ('ε', "\\epsilon "),
    ('ζ', "\\zeta "),
    ('η', "\\eta "),
    ('θ', "\\theta "),
    ('ι', "\\iota "),
    ('κ', "\\kappa "),
    ('λ', "\\lambda "),
    ('μ', "\\mu "),
    ('ν', "\\nu "),
    ('ξ', "\\xi "),
    ('ρ', "\\rho "),
    ('σ', "\\sigma "),
    ('τ', "\\tau "),
    ('φ', "\\phi "),
    ('χ', "\\chi "),
    ('ψ', "\\psi "),
    ('ω', "\\omega "),
    ('Γ', "\\Gamma "),
    ('Δ', "\\Delta "),
    ('Θ', "\\Theta "),
    ('Λ', "\\Lambda "),
    ('Ξ', "\\Xi "),
    ('Π', "\\Pi "),
    ('Σ', "\\Sigma "),
    ('Φ', "\\Phi "),
    ('Ψ', "\\Psi "),
    ('Ω', "\\Omega "),
];

/// 规范化 LaTeX：去定界符、Unicode 映射、全角转半角、修剪与压缩空白。
///
/// 幂等：`normalize(normalize(x)) == normalize(x)`；不改变语义、不补全内容。
#[must_use]
pub fn normalize_latex(raw: &str) -> String {
    let mut text = raw.trim().to_owned();
    loop {
        let stripped = text
            .strip_prefix("$$")
            .and_then(|body| body.strip_suffix("$$"))
            .or_else(|| {
                text.strip_prefix('$')
                    .and_then(|body| body.strip_suffix('$'))
            })
            .or_else(|| {
                text.strip_prefix("\\[")
                    .and_then(|body| body.strip_suffix("\\]"))
            })
            .or_else(|| {
                text.strip_prefix("\\(")
                    .and_then(|body| body.strip_suffix("\\)"))
            });
        match stripped {
            Some(body) => text = body.trim().to_owned(),
            None => break,
        }
    }
    let mut mapped = String::with_capacity(text.len() + 16);
    for ch in text.chars() {
        if let Some((_, command)) = SYMBOL_MAP.iter().find(|(symbol, _)| *symbol == ch) {
            mapped.push_str(command);
            continue;
        }
        mapped.push(fullwidth_to_ascii(ch));
    }
    // 压缩连续 ASCII 空白（保留单个空格；`\ ` 这类间距命令仍为「反斜杠 + 空格」）。
    let mut result = String::with_capacity(mapped.len());
    let mut last_space = false;
    for ch in mapped.chars() {
        if ch == ' ' || ch == '\t' || ch == '\n' || ch == '\r' {
            if !last_space {
                result.push(' ');
                last_space = true;
            }
        } else {
            result.push(ch);
            last_space = false;
        }
    }
    result.trim().to_owned()
}

const fn fullwidth_to_ascii(ch: char) -> char {
    match ch {
        '（' => '(',
        '）' => ')',
        '［' => '[',
        '］' => ']',
        '｛' => '{',
        '｝' => '}',
        '＋' => '+',
        '－' => '-',
        '＝' => '=',
        '＜' => '<',
        '＞' => '>',
        '，' | '、' => ',',
        '。' | '．' => '.',
        '／' => '/',
        '％' => '%',
        '＊' => '*',
        '＾' => '^',
        '＿' => '_',
        '｜' => '|',
        '～' => '~',
        '：' => ':',
        '；' => ';',
        '！' => '!',
        '？' => '?',
        '０' => '0',
        '１' => '1',
        '２' => '2',
        '３' => '3',
        '４' => '4',
        '５' => '5',
        '６' => '6',
        '７' => '7',
        '８' => '8',
        '９' => '9',
        other => other,
    }
}

/// 允许出现的非字母数字字符（ASCII 数学模式常用集合；其余一律非法）。
const ALLOWED_PUNCT: &[char] = &[
    '+', '-', '=', '*', '/', '(', ')', '[', ']', '{', '}', '^', '_', '.', ',', ';', ':', '!', '\'',
    '|', '<', '>', '~', '&', '\\', ' ',
];

/// 环境白名单（数学排版常用；不在表内即非法）。
const ENV_WHITELIST: &[&str] = &[
    "aligned",
    "alignedat",
    "array",
    "Bmatrix",
    "bmatrix",
    "cases",
    "dcases",
    "equation",
    "equation*",
    "gather",
    "gather*",
    "gathered",
    "matrix",
    "pmatrix",
    "smallmatrix",
    "split",
    "subarray",
    "Vmatrix",
    "vmatrix",
];

/// 禁止命令（读文件、写文件、定义宏、改类别码、调用外部代码一类）。
const FORBIDDEN_COMMANDS: &[&str] = &[
    "input",
    "include",
    "includeonly",
    "InputIfFileExists",
    "write",
    "read",
    "openin",
    "openout",
    "immediate",
    "def",
    "edef",
    "gdef",
    "xdef",
    "let",
    "futurelet",
    "catcode",
    "csname",
    "endcsname",
    "newcommand",
    "renewcommand",
    "providecommand",
    "DeclareMathOperator",
    "DeclareRobustCommand",
    "usepackage",
    "RequirePackage",
    "documentclass",
    "special",
    "directlua",
    "shellescape",
    "lstinputlisting",
    "verbatiminput",
    "verb",
    "href",
    "url",
    "includegraphics",
    "loop",
    "repeat",
];

/// 单字符命令白名单（反斜杠后一个非字母字符）。
const SINGLE_CHAR_COMMANDS: &[char] = &[
    '\\', ',', ';', ':', '!', ' ', '{', '}', '|', '_', '&', '%', '$', '#', '(', ')', '[', ']',
];

/// 需要「至少一个花括号参数」的命令（装饰/文本/根式等）。
const ONE_ARG_COMMANDS: &[&str] = &[
    "text",
    "textbf",
    "textit",
    "textrm",
    "textsf",
    "texttt",
    "mathrm",
    "mathbf",
    "mathit",
    "mathcal",
    "mathbb",
    "mathsf",
    "mathtt",
    "operatorname",
    "overline",
    "underline",
    "hat",
    "bar",
    "vec",
    "dot",
    "ddot",
    "tilde",
    "widehat",
    "widetilde",
    "overrightarrow",
    "overleftarrow",
    "overbrace",
    "underbrace",
    "boxed",
    "not",
    "pmod",
    "cancel",
    "sqrt",
];

/// 需要「两个花括号参数」的命令。
const TWO_ARG_COMMANDS: &[&str] = &[
    "frac", "dfrac", "tfrac", "cfrac", "binom", "dbinom", "tbinom", "overset", "underset",
    "stackrel", "substack",
];

/// 零参数命令（操作符/关系符/希腊字母/间距等）白名单。
const ZERO_ARG_COMMANDS: &[&str] = &[
    "left",
    "right",
    "middle",
    "big",
    "Big",
    "bigg",
    "Bigg",
    "bigl",
    "bigr",
    "Bigl",
    "Bigr",
    "biggl",
    "biggr",
    "Biggl",
    "Biggr",
    "quad",
    "qquad",
    "limits",
    "nolimits",
    "displaystyle",
    "textstyle",
    "scriptstyle",
    "scriptscriptstyle",
    "sum",
    "prod",
    "int",
    "iint",
    "iiint",
    "oint",
    "lim",
    "log",
    "ln",
    "lg",
    "exp",
    "sin",
    "cos",
    "tan",
    "cot",
    "sec",
    "csc",
    "arcsin",
    "arccos",
    "arctan",
    "sinh",
    "cosh",
    "tanh",
    "max",
    "min",
    "sup",
    "inf",
    "gcd",
    "lcm",
    "det",
    "dim",
    "ker",
    "bmod",
    "mod",
    "cdot",
    "times",
    "div",
    "pm",
    "mp",
    "ast",
    "star",
    "circ",
    "bullet",
    "oplus",
    "otimes",
    "leq",
    "geq",
    "neq",
    "approx",
    "equiv",
    "sim",
    "simeq",
    "cong",
    "propto",
    "infty",
    "partial",
    "nabla",
    "forall",
    "exists",
    "nexists",
    "in",
    "notin",
    "ni",
    "subset",
    "supset",
    "subseteq",
    "supseteq",
    "cup",
    "cap",
    "emptyset",
    "varnothing",
    "to",
    "rightarrow",
    "leftarrow",
    "leftrightarrow",
    "Rightarrow",
    "Leftarrow",
    "Leftrightarrow",
    "mapsto",
    "uparrow",
    "downarrow",
    "angle",
    "perp",
    "parallel",
    "therefore",
    "because",
    "ldots",
    "cdots",
    "vdots",
    "ddots",
    "dots",
    "prime",
    "alpha",
    "beta",
    "gamma",
    "delta",
    "epsilon",
    "varepsilon",
    "zeta",
    "eta",
    "theta",
    "vartheta",
    "iota",
    "kappa",
    "lambda",
    "mu",
    "nu",
    "xi",
    "omicron",
    "pi",
    "varpi",
    "rho",
    "varrho",
    "sigma",
    "varsigma",
    "tau",
    "upsilon",
    "phi",
    "varphi",
    "chi",
    "psi",
    "omega",
    "Gamma",
    "Delta",
    "Theta",
    "Lambda",
    "Xi",
    "Pi",
    "Sigma",
    "Upsilon",
    "Phi",
    "Psi",
    "Omega",
    "begin",
    "end",
    "textdegree",
    "degree",
];

/// 校验规范化后的 LaTeX（不做二次规范化）。
#[must_use]
pub fn validate_latex(latex: &str) -> LatexOutcome {
    match check_latex(latex) {
        Ok(()) => LatexOutcome::Valid {
            latex: latex.to_owned(),
        },
        Err(reason) => LatexOutcome::Invalid { reason },
    }
}

/// 规范化 + 校验（语义校验阶段使用）。
#[must_use]
pub fn normalize_and_validate(raw: &str) -> LatexOutcome {
    let normalized = normalize_latex(raw);
    if normalized.trim().is_empty() {
        return LatexOutcome::Invalid {
            reason: "LaTeX 为空".to_owned(),
        };
    }
    validate_latex(&normalized)
}

fn check_latex(latex: &str) -> Result<(), String> {
    let length = latex.chars().count();
    if length > MAX_LATEX_CHARS {
        return Err(format!("LaTeX 长度 {length} 超过上限 {MAX_LATEX_CHARS}"));
    }
    for ch in latex.chars() {
        let allowed = ch.is_ascii_alphanumeric() || ALLOWED_PUNCT.contains(&ch) || is_cjk(ch);
        if !allowed {
            return Err(format!(
                "包含不允许的字符 `{ch}`（字符集白名单：ASCII 字母数字与数学标点）"
            ));
        }
    }
    check_balanced(latex, '{', '}', "花括号")?;
    check_balanced(latex, '[', ']', "方括号")?;
    check_balanced(latex, '(', ')', "圆括号")?;
    check_commands(latex)
}

/// CJK 统一表意文字（允许出现在 `\text{...}` 里，例如「面积」）。
const fn is_cjk(ch: char) -> bool {
    matches!(ch, '\u{4E00}'..='\u{9FFF}')
}

fn check_balanced(latex: &str, open: char, close: char, name: &str) -> Result<(), String> {
    let mut depth = 0i32;
    for ch in latex.chars() {
        if ch == open {
            depth += 1;
        } else if ch == close {
            depth -= 1;
            if depth < 0 {
                return Err(format!("{name}不平衡：出现多余的 `{close}`"));
            }
        }
    }
    if depth != 0 {
        return Err(format!("{name}不平衡：缺少 {depth} 个 `{close}`"));
    }
    Ok(())
}

fn check_commands(latex: &str) -> Result<(), String> {
    let chars: Vec<char> = latex.chars().collect();
    let mut index = 0usize;
    let mut left_right_depth = 0i32;
    let mut env_stack: Vec<String> = Vec::new();
    while index < chars.len() {
        if chars[index] != '\\' {
            index += 1;
            continue;
        }
        let command_start = index + 1;
        if command_start >= chars.len() {
            return Err("LaTeX 以孤立的反斜杠结束".to_owned());
        }
        if !chars[command_start].is_ascii_alphabetic() {
            let ch = chars[command_start];
            if !SINGLE_CHAR_COMMANDS.contains(&ch) {
                return Err(format!("不支持的转义字符 `\\{ch}`"));
            }
            index = command_start + 1;
            continue;
        }
        let mut end = command_start;
        while end < chars.len() && chars[end].is_ascii_alphabetic() {
            end += 1;
        }
        let name: String = chars[command_start..end].iter().collect();
        index = end;

        if FORBIDDEN_COMMANDS.contains(&name.as_str()) {
            return Err(format!(
                "禁止命令 `\\{name}`（不得读写文件、定义宏或调用外部代码）"
            ));
        }
        match name.as_str() {
            "begin" | "end" => {
                let (env, next) = read_env_name(&chars, index)?;
                index = next;
                if !ENV_WHITELIST.contains(&env.as_str()) {
                    return Err(format!("环境 `{env}` 不在白名单内"));
                }
                if name == "begin" {
                    env_stack.push(env);
                } else {
                    let expected = env_stack.pop();
                    if expected.as_deref() != Some(env.as_str()) {
                        return Err(expected.map_or_else(
                            || format!("环境 `{env}` 的 `\\end` 没有对应的 `\\begin`"),
                            |open| format!("环境配对错误：`\\begin{{{open}}}` 与 `\\end{{{env}}}`"),
                        ));
                    }
                }
            }
            "left" => left_right_depth += 1,
            "right" => {
                left_right_depth -= 1;
                if left_right_depth < 0 {
                    return Err("`\\right` 多于 `\\left`".to_owned());
                }
            }
            _ => {
                if TWO_ARG_COMMANDS.contains(&name.as_str()) {
                    let next = require_brace_arg(&chars, index, &name, 1)?;
                    let next = require_brace_arg(&chars, next, &name, 2)?;
                    index = next;
                } else if ONE_ARG_COMMANDS.contains(&name.as_str()) {
                    let mut probe = index;
                    if name == "sqrt" {
                        while probe < chars.len() && chars[probe] == ' ' {
                            probe += 1;
                        }
                        if probe < chars.len() && chars[probe] == '[' {
                            // `\sqrt[n]{...}`：跳过可选参数，再要求花括号参数。
                            let mut depth = 0i32;
                            while probe < chars.len() {
                                if chars[probe] == '[' {
                                    depth += 1;
                                } else if chars[probe] == ']' {
                                    depth -= 1;
                                    if depth == 0 {
                                        probe += 1;
                                        break;
                                    }
                                }
                                probe += 1;
                            }
                        }
                    }
                    let next = require_brace_arg(&chars, probe, &name, 1)?;
                    index = next;
                } else if !ZERO_ARG_COMMANDS.contains(&name.as_str()) {
                    return Err(format!("未知命令 `\\{name}`（不在白名单内）"));
                }
            }
        }
    }
    if left_right_depth != 0 {
        return Err(format!(
            "`\\left`/`\\right` 未配对：缺少 {left_right_depth} 个 `\\right`"
        ));
    }
    if let Some(open) = env_stack.last() {
        return Err(format!("环境 `{open}` 缺少对应的 `\\end`"));
    }
    Ok(())
}

fn read_env_name(chars: &[char], start: usize) -> Result<(String, usize), String> {
    let mut index = start;
    while index < chars.len() && chars[index] == ' ' {
        index += 1;
    }
    if index >= chars.len() || chars[index] != '{' {
        return Err("环境命令必须紧跟 `{环境名}`".to_owned());
    }
    index += 1;
    let mut name = String::new();
    while index < chars.len() && chars[index] != '}' {
        let ch = chars[index];
        if !(ch.is_ascii_alphanumeric() || ch == '*' || ch == '-' || ch == '_' || ch == ' ') {
            return Err(format!("环境名包含非法字符 `{ch}`"));
        }
        name.push(ch);
        index += 1;
    }
    if index >= chars.len() {
        return Err("环境名缺少 `}`".to_owned());
    }
    let name = name.trim().to_owned();
    if name.is_empty() {
        return Err("环境名不得为空".to_owned());
    }
    Ok((name, index + 1))
}

fn require_brace_arg(
    chars: &[char],
    start: usize,
    command: &str,
    argument: usize,
) -> Result<usize, String> {
    let mut index = start;
    while index < chars.len() && chars[index] == ' ' {
        index += 1;
    }
    if index >= chars.len() || chars[index] != '{' {
        return Err(format!("命令 `\\{command}` 缺少第 {argument} 个花括号参数"));
    }
    let mut depth = 0i32;
    while index < chars.len() {
        if chars[index] == '{' {
            depth += 1;
        } else if chars[index] == '}' {
            depth -= 1;
            if depth == 0 {
                return Ok(index + 1);
            }
        }
        index += 1;
    }
    Err(format!("命令 `\\{command}` 的参数花括号未闭合"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_delimiters_and_maps_symbols() {
        assert_eq!(normalize_latex("$x^2$"), "x^2");
        assert_eq!(normalize_latex("$$x+1$$"), "x+1");
        assert_eq!(normalize_latex("\\[x+1\\]"), "x+1");
        assert_eq!(normalize_latex("\\(x+1\\)"), "x+1");
        assert_eq!(normalize_latex("  x  ×  y  "), "x \\times y");
        assert_eq!(normalize_latex("a≤b"), "a\\leq b");
        assert_eq!(normalize_latex("（x）"), "(x)");
        assert_eq!(normalize_latex("１２３"), "123");
    }

    #[test]
    fn normalize_is_idempotent() {
        for raw in [
            "$x^2+1$",
            "a×b≤c",
            "\\frac{1}{2}",
            "\\begin{aligned}x&=1\\end{aligned}",
            "（１＋２）×３",
        ] {
            let once = normalize_latex(raw);
            assert_eq!(normalize_latex(&once), once, "输入 {raw:?} 的规范化不幂等");
        }
    }

    #[test]
    fn validates_common_formulas() {
        for latex in [
            "x^2+1",
            "\\frac{1}{2}",
            "\\sqrt{x+1}",
            "\\sqrt[3]{x}",
            "\\left(x+y\\right)",
            "\\alpha+\\beta\\leq\\gamma",
            "\\begin{aligned}x&=1\\\\y&=2\\end{aligned}",
            "\\begin{pmatrix}1&2\\\\3&4\\end{pmatrix}",
            "\\sum_{i=1}^{n}i",
            "f(x)=\\begin{cases}1&x>0\\\\0&x\\leq 0\\end{cases}",
            "\\text{面积}",
            "\\,dx",
        ] {
            let outcome = validate_latex(&normalize_latex(latex));
            assert!(outcome.is_valid(), "{latex} 应合法：{outcome:?}");
        }
    }

    #[test]
    fn rejects_unbalanced_and_illegal() {
        let cases = [
            ("\\frac{1}{2", "花括号"),
            ("x^{2", "花括号"),
            ("x^}2{", "花括号"),
            ("\\begin{matrix}x\\end{aligned}", "环境配对错误"),
            ("\\begin{matrix}x", "缺少对应的"),
            ("\\end{matrix}", "没有对应的"),
            ("\\begin{unknownenv}x\\end{unknownenv}", "不在白名单内"),
            ("\\input{/etc/passwd}", "禁止命令"),
            ("\\def\\x{1}", "禁止命令"),
            ("\\frac{1}", "缺少第 2 个花括号参数"),
            ("\\frac12", "缺少第 1 个花括号参数"),
            ("\\foobar{1}", "未知命令"),
            ("x$y", "不允许的字符"),
            ("\\left(x", "圆括号不平衡"),
            ("\\right)", "圆括号不平衡"),
            ("\\left| x", "未配对"),
            ("\\right| x", "多于"),
        ];
        for (latex, needle) in cases {
            let outcome = validate_latex(latex);
            assert!(!outcome.is_valid(), "{latex} 应非法");
            let reason = outcome.reason().unwrap_or_default();
            assert!(
                reason.contains(needle),
                "{latex} 的非法原因应包含 {needle:?}，实际：{reason}"
            );
        }
    }

    #[test]
    fn rejects_long_and_empty() {
        let long = "x".repeat(MAX_LATEX_CHARS + 1);
        assert!(!validate_latex(&long).is_valid());
        assert!(
            normalize_and_validate("   ").reason().is_some(),
            "空公式必须非法"
        );
        assert!(normalize_and_validate("$\\frac{1}{2}$").is_valid());
    }
}
