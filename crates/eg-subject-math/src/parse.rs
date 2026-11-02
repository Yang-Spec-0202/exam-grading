//! LaTeX/数学文本的词法与递归下降解析（`SPEC-subject-math.md` §3 第 4 层、§4）。
//!
//! 手写解析器（无第三方解析库，选型见 `docs/dev/research/subject-math.md`）：
//! - 词法层识别数值、变量、LaTeX 命令、运算符、括号、矩阵环境、`\text{}` 单位；
//! - 语法层按优先级：关系/多解 → 和差 → 积商 → 幂 → 原子；
//! - 支持 `\frac`、`\sqrt[n]{}`、`\vec`、`\begin{pmatrix}`、区间/集合、
//!   带单位数值、绝对值、函数调用；
//! - **可识别但超出支持范围**的写法（三角/微积分等）解析为 [`Expr::Function`]，
//!   由比较器返回「无法判定」；真正的语法错误返回 [`ParseError`]（不是「答错」）。
//!
//! 本模块只负责语法，不做规范化（由 `canonical` 完成）。

use thiserror::Error;

use crate::ast::{Expr, Interval, IntervalEnd, RelOp, Unit};
use crate::number::Rational;

/// 最大嵌套深度（防御恶意/异常输入；规范 §10 快速失败）。
const MAX_DEPTH: usize = 64;

/// 解析错误（语法层面；调用方应把它映射为归一化「无法解析」）。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("第 {position} 个字符附近无法解析：{reason}")]
pub struct ParseError {
    /// 出错字符位置（字节偏移，从 0 起）。
    pub position: usize,
    /// 中文可读原因。
    pub reason: String,
}

impl ParseError {
    fn new(position: usize, reason: impl Into<String>) -> Self {
        Self {
            position,
            reason: reason.into(),
        }
    }
}

/// 解析数学文本为语法树。
///
/// # Errors
///
/// 语法非法、嵌套过深、矩阵行列不齐、括号不配平等返回 [`ParseError`]。
pub fn parse_expr(input: &str) -> Result<Expr, ParseError> {
    let tokens = tokenize(input)?;
    let mut parser = Parser::new(tokens);
    let expr = parser.parse_top()?;
    parser.expect_eof()?;
    Ok(expr)
}

// ---------------------------------------------------------------------------
// 词法
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum TokenKind {
    Number(String),
    Ident(String),
    Command(String),
    Text(String),
    BeginEnv(String),
    EndEnv,
    EmptySet,
    Infinity,
    Plus,
    Minus,
    Star,
    Slash,
    Caret,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Pipe,
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    SetOpen,
    SetClose,
    Comma,
    Semicolon,
    Amp,
    RowSep,
    Underscore,
    Union,
    Intersection,
    Or,
    And,
    Eof,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Token {
    kind: TokenKind,
    position: usize,
}

fn tokenize(input: &str) -> Result<Vec<Token>, ParseError> {
    let chars: Vec<(usize, char)> = input.char_indices().collect();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        let (position, ch) = chars[index];
        if ch.is_whitespace() {
            index += 1;
            continue;
        }
        match ch {
            '0'..='9' | '.' => {
                let (token, next) = scan_number(&chars, index)?;
                tokens.push(Token {
                    kind: TokenKind::Number(token),
                    position,
                });
                index = next;
            }
            'a'..='z' | 'A'..='Z' => {
                let (name, next) = scan_ident(&chars, index);
                tokens.push(Token {
                    kind: TokenKind::Ident(name),
                    position,
                });
                index = next;
            }
            'π' | 'α' | 'β' | 'γ' | 'θ' | 'λ' | 'μ' | 'φ' | 'ω' | 'Δ' | 'Ω' => {
                tokens.push(Token {
                    kind: TokenKind::Ident(ch.to_string()),
                    position,
                });
                index += 1;
            }
            '\\' => {
                let (token_kind, next) = scan_command(&chars, index)?;
                if let Some(kind) = token_kind {
                    tokens.push(Token { kind, position });
                }
                index = next;
            }
            '+' => {
                tokens.push(Token {
                    kind: TokenKind::Plus,
                    position,
                });
                index += 1;
            }
            '-' | '−' | '–' => {
                tokens.push(Token {
                    kind: TokenKind::Minus,
                    position,
                });
                index += 1;
            }
            '*' | '×' | '·' | '⋅' => {
                tokens.push(Token {
                    kind: TokenKind::Star,
                    position,
                });
                index += 1;
            }
            '/' | '÷' => {
                tokens.push(Token {
                    kind: TokenKind::Slash,
                    position,
                });
                index += 1;
            }
            '^' => {
                tokens.push(Token {
                    kind: TokenKind::Caret,
                    position,
                });
                index += 1;
            }
            '=' => {
                tokens.push(Token {
                    kind: TokenKind::Eq,
                    position,
                });
                index += 1;
            }
            '<' => {
                tokens.push(Token {
                    kind: TokenKind::Lt,
                    position,
                });
                index += 1;
            }
            '>' => {
                tokens.push(Token {
                    kind: TokenKind::Gt,
                    position,
                });
                index += 1;
            }
            '≤' | '⩽' => {
                tokens.push(Token {
                    kind: TokenKind::Le,
                    position,
                });
                index += 1;
            }
            '≥' | '⩾' => {
                tokens.push(Token {
                    kind: TokenKind::Ge,
                    position,
                });
                index += 1;
            }
            '≠' => {
                tokens.push(Token {
                    kind: TokenKind::Ne,
                    position,
                });
                index += 1;
            }
            '∞' => {
                tokens.push(Token {
                    kind: TokenKind::Infinity,
                    position,
                });
                index += 1;
            }
            '|' => {
                tokens.push(Token {
                    kind: TokenKind::Pipe,
                    position,
                });
                index += 1;
            }
            '(' => {
                tokens.push(Token {
                    kind: TokenKind::LParen,
                    position,
                });
                index += 1;
            }
            ')' => {
                tokens.push(Token {
                    kind: TokenKind::RParen,
                    position,
                });
                index += 1;
            }
            '[' => {
                tokens.push(Token {
                    kind: TokenKind::LBracket,
                    position,
                });
                index += 1;
            }
            ']' => {
                tokens.push(Token {
                    kind: TokenKind::RBracket,
                    position,
                });
                index += 1;
            }
            '{' => {
                tokens.push(Token {
                    kind: TokenKind::LBrace,
                    position,
                });
                index += 1;
            }
            '}' => {
                tokens.push(Token {
                    kind: TokenKind::RBrace,
                    position,
                });
                index += 1;
            }
            ',' | '，' | '、' => {
                tokens.push(Token {
                    kind: TokenKind::Comma,
                    position,
                });
                index += 1;
            }
            ';' | '；' => {
                tokens.push(Token {
                    kind: TokenKind::Semicolon,
                    position,
                });
                index += 1;
            }
            '&' => {
                tokens.push(Token {
                    kind: TokenKind::Amp,
                    position,
                });
                index += 1;
            }
            '_' => {
                tokens.push(Token {
                    kind: TokenKind::Underscore,
                    position,
                });
                index += 1;
            }
            '∪' => {
                tokens.push(Token {
                    kind: TokenKind::Union,
                    position,
                });
                index += 1;
            }
            '∩' => {
                tokens.push(Token {
                    kind: TokenKind::Intersection,
                    position,
                });
                index += 1;
            }
            '或' | '∨' => {
                tokens.push(Token {
                    kind: TokenKind::Or,
                    position,
                });
                index += 1;
            }
            '且' | '∧' => {
                tokens.push(Token {
                    kind: TokenKind::And,
                    position,
                });
                index += 1;
            }
            other => {
                return Err(ParseError::new(position, format!("不支持的字符 {other:?}")));
            }
        }
    }
    tokens.push(Token {
        kind: TokenKind::Eof,
        position: input.len(),
    });
    Ok(tokens)
}

fn scan_number(chars: &[(usize, char)], start: usize) -> Result<(String, usize), ParseError> {
    let mut index = start;
    let mut saw_digit = false;
    let mut text = String::new();
    while index < chars.len() {
        let ch = chars[index].1;
        if ch.is_ascii_digit() {
            saw_digit = true;
            text.push(ch);
            index += 1;
        } else if ch == '.' {
            text.push(ch);
            index += 1;
        } else {
            break;
        }
    }
    if !saw_digit {
        return Err(ParseError::new(
            chars[start].0,
            "小数点后缺少数字".to_owned(),
        ));
    }
    // 科学计数指数
    if index < chars.len() {
        let ch = chars[index].1;
        if ch == 'e' || ch == 'E' {
            let mut lookahead = index + 1;
            let mut exponent = String::from("e");
            if lookahead < chars.len() {
                let sign = chars[lookahead].1;
                if sign == '+' || sign == '-' {
                    exponent.push(sign);
                    lookahead += 1;
                }
                let mut has_digit = false;
                while lookahead < chars.len() && chars[lookahead].1.is_ascii_digit() {
                    exponent.push(chars[lookahead].1);
                    has_digit = true;
                    lookahead += 1;
                }
                if has_digit {
                    text.push_str(&exponent);
                    index = lookahead;
                }
            }
        }
    }
    Ok((text, index))
}

fn scan_ident(chars: &[(usize, char)], start: usize) -> (String, usize) {
    let mut index = start;
    let mut name = String::new();
    while index < chars.len() && chars[index].1.is_ascii_alphabetic() {
        name.push(chars[index].1);
        index += 1;
    }
    // 下标（如 x_1、x_n）：并入变量名
    if index < chars.len() && chars[index].1 == '_' {
        let mut lookahead = index + 1;
        let mut subscript = String::new();
        while lookahead < chars.len() && (chars[lookahead].1.is_ascii_alphanumeric()) {
            subscript.push(chars[lookahead].1);
            lookahead += 1;
        }
        if !subscript.is_empty() {
            name.push('_');
            name.push_str(&subscript);
            index = lookahead;
        }
    }
    (name, index)
}

fn scan_command(
    chars: &[(usize, char)],
    start: usize,
) -> Result<(Option<TokenKind>, usize), ParseError> {
    let position = chars[start].0;
    let mut index = start + 1;
    if index >= chars.len() {
        return Err(ParseError::new(position, "反斜杠后缺少命令名".to_owned()));
    }
    // `\\` 表示矩阵换行
    if chars[index].1 == '\\' {
        return Ok((Some(TokenKind::RowSep), index + 1));
    }
    // `\{`、`\}`、`\|` 等单字符命令
    let single = chars[index].1;
    if !single.is_ascii_alphabetic() {
        let kind = match single {
            '{' => TokenKind::SetOpen,
            '}' => TokenKind::SetClose,
            '|' => TokenKind::Pipe,
            ',' | ';' | '!' => {
                return Ok((None, index + 1));
            }
            other => {
                return Err(ParseError::new(position, format!("不支持的命令 \\{other}")));
            }
        };
        return Ok((Some(kind), index + 1));
    }
    let mut name = String::new();
    while index < chars.len() && chars[index].1.is_ascii_alphabetic() {
        name.push(chars[index].1);
        index += 1;
    }
    let kind = match name.as_str() {
        "frac" | "dfrac" | "tfrac" => TokenKind::Command("frac".to_owned()),
        "sqrt" => TokenKind::Command("sqrt".to_owned()),
        "vec" => TokenKind::Command("vec".to_owned()),
        "operatorname" => {
            let (content, next) = scan_braced_content(chars, index).map_err(|reason| {
                ParseError::new(position, format!("\\{name} 缺少花括号内容：{reason}"))
            })?;
            return Ok((Some(TokenKind::Ident(content.trim().to_owned())), next));
        }
        "text" | "mathrm" | "mbox" => {
            let (content, next) = scan_braced_content(chars, index).map_err(|reason| {
                ParseError::new(position, format!("\\{name} 缺少花括号内容：{reason}"))
            })?;
            let trimmed = content.trim();
            if trimmed == "或" {
                return Ok((Some(TokenKind::Or), next));
            }
            if trimmed == "且" {
                return Ok((Some(TokenKind::And), next));
            }
            return Ok((Some(TokenKind::Text(content)), next));
        }
        "begin" => {
            let (content, next) = scan_braced_content(chars, index).map_err(|reason| {
                ParseError::new(position, format!("\\begin 缺少环境名：{reason}"))
            })?;
            return Ok((Some(TokenKind::BeginEnv(content)), next));
        }
        "end" => {
            let (_, next) = scan_braced_content(chars, index).map_err(|reason| {
                ParseError::new(position, format!("\\end 缺少环境名：{reason}"))
            })?;
            return Ok((Some(TokenKind::EndEnv), next));
        }
        "left" | "right" | "displaystyle" | "textstyle" | "quad" | "qquad" | "limits"
        | "nolimits" => {
            return Ok((None, index));
        }
        "cup" | "bigcup" => TokenKind::Union,
        "cap" | "bigcap" => TokenKind::Intersection,
        "cdot" | "times" => TokenKind::Star,
        "div" => TokenKind::Slash,
        "le" | "leq" | "leqslant" => TokenKind::Le,
        "ge" | "geq" | "geqslant" => TokenKind::Ge,
        "ne" | "neq" => TokenKind::Ne,
        "lt" => TokenKind::Lt,
        "gt" => TokenKind::Gt,
        "infty" => TokenKind::Infinity,
        "pi" => TokenKind::Ident("π".to_owned()),
        "alpha" => TokenKind::Ident("α".to_owned()),
        "beta" => TokenKind::Ident("β".to_owned()),
        "gamma" => TokenKind::Ident("γ".to_owned()),
        "theta" => TokenKind::Ident("θ".to_owned()),
        "lambda" => TokenKind::Ident("λ".to_owned()),
        "mu" => TokenKind::Ident("μ".to_owned()),
        "varphi" | "phi" => TokenKind::Ident("φ".to_owned()),
        "omega" => TokenKind::Ident("ω".to_owned()),
        "Delta" => TokenKind::Ident("Δ".to_owned()),
        "Omega" => TokenKind::Ident("Ω".to_owned()),
        "lbrace" => TokenKind::SetOpen,
        "rbrace" => TokenKind::SetClose,
        "varnothing" | "emptyset" => TokenKind::EmptySet,
        other => TokenKind::Command(other.to_owned()),
    };
    Ok((Some(kind), index))
}

/// 读取 `{...}` 内容（支持一层嵌套花括号与转义），返回内容与结束位置。
fn scan_braced_content(chars: &[(usize, char)], start: usize) -> Result<(String, usize), String> {
    let mut index = start;
    while index < chars.len() && chars[index].1.is_whitespace() {
        index += 1;
    }
    if index >= chars.len() || chars[index].1 != '{' {
        return Err("缺少 {".to_owned());
    }
    index += 1;
    let mut depth = 1usize;
    let mut content = String::new();
    while index < chars.len() {
        let ch = chars[index].1;
        if ch == '\\' && index + 1 < chars.len() {
            content.push(ch);
            content.push(chars[index + 1].1);
            index += 2;
            continue;
        }
        if ch == '{' {
            depth += 1;
            content.push(ch);
        } else if ch == '}' {
            depth -= 1;
            if depth == 0 {
                return Ok((content, index + 1));
            }
            content.push(ch);
        } else {
            content.push(ch);
        }
        index += 1;
    }
    Err("花括号不配对".to_owned())
}

// ---------------------------------------------------------------------------
// 语法
// ---------------------------------------------------------------------------

struct Parser {
    tokens: Vec<Token>,
    index: usize,
    depth: usize,
    eof: Token,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        let eof = tokens.last().cloned().unwrap_or(Token {
            kind: TokenKind::Eof,
            position: 0,
        });
        Self {
            tokens,
            index: 0,
            depth: 0,
            eof,
        }
    }

    fn peek(&self) -> &Token {
        self.tokens.get(self.index).unwrap_or(&self.eof)
    }

    fn peek_kind(&self) -> &TokenKind {
        &self.peek().kind
    }

    fn peek_ahead(&self, offset: usize) -> &TokenKind {
        self.tokens
            .get(self.index + offset)
            .map_or(&TokenKind::Eof, |token| &token.kind)
    }

    fn position(&self) -> usize {
        self.peek().position
    }

    fn advance(&mut self) -> Token {
        let token = self.tokens.get(self.index).cloned().unwrap_or(Token {
            kind: TokenKind::Eof,
            position: 0,
        });
        if self.index + 1 < self.tokens.len() {
            self.index += 1;
        }
        token
    }

    fn expect(&mut self, expected: &TokenKind) -> Result<(), ParseError> {
        if self.peek_kind() == expected {
            self.advance();
            Ok(())
        } else {
            Err(ParseError::new(
                self.position(),
                format!("期望 {expected:?}，实际 {:?}", self.peek_kind()),
            ))
        }
    }

    fn expect_eof(&mut self) -> Result<(), ParseError> {
        if matches!(self.peek_kind(), TokenKind::Eof) {
            Ok(())
        } else {
            Err(ParseError::new(
                self.position(),
                format!("表达式后存在无法解析的内容：{:?}", self.peek_kind()),
            ))
        }
    }

    fn enter(&mut self) -> Result<(), ParseError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(ParseError::new(
                self.position(),
                format!("表达式嵌套过深（上限 {MAX_DEPTH} 层）"),
            ));
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    fn parse_top(&mut self) -> Result<Expr, ParseError> {
        match self.peek_kind() {
            TokenKind::BeginEnv(_) => self.parse_matrix(),
            TokenKind::SetOpen | TokenKind::EmptySet => {
                let first = self.parse_set()?;
                self.parse_set_expression_from(first)
            }
            TokenKind::LBrace => {
                let first = self.parse_brace_set()?;
                self.parse_set_expression_from(first)
            }
            _ => self.parse_set_expression(),
        }
    }

    fn parse_set_expression(&mut self) -> Result<Expr, ParseError> {
        let first = self.parse_solutions()?;
        self.parse_set_expression_from(first)
    }

    fn parse_set_expression_from(&mut self, first: Expr) -> Result<Expr, ParseError> {
        let mut intersections = vec![self.parse_intersection_from(first)?];
        while matches!(self.peek_kind(), TokenKind::Union) {
            self.advance();
            intersections.push(self.parse_intersection()?);
        }
        Ok(if intersections.len() == 1 {
            intersections.pop().unwrap_or_else(|| Expr::Set(Vec::new()))
        } else {
            Expr::SetUnion(intersections)
        })
    }

    fn parse_intersection(&mut self) -> Result<Expr, ParseError> {
        let first = self.parse_solutions()?;
        self.parse_intersection_from(first)
    }

    fn parse_intersection_from(&mut self, first: Expr) -> Result<Expr, ParseError> {
        let mut items = vec![first];
        while matches!(self.peek_kind(), TokenKind::Intersection) {
            self.advance();
            items.push(self.parse_solutions()?);
        }
        Ok(if items.len() == 1 {
            items.pop().unwrap_or_else(|| Expr::Set(Vec::new()))
        } else {
            Expr::SetIntersection(items)
        })
    }

    fn parse_solutions(&mut self) -> Result<Expr, ParseError> {
        let first = self.parse_relation()?;
        if !matches!(self.peek_kind(), TokenKind::Or) {
            return Ok(first);
        }
        let mut items = vec![first];
        while matches!(self.peek_kind(), TokenKind::Or) {
            self.advance();
            items.push(self.parse_relation()?);
        }
        Ok(Expr::Solutions(items))
    }

    fn parse_relation(&mut self) -> Result<Expr, ParseError> {
        let lhs = self.parse_additive()?;
        let Some(op) = self.relation_op() else {
            return Ok(lhs);
        };
        self.advance();
        let middle = self.parse_additive()?;
        // 连不等式 `1 <= x < 2` → 区间
        if let Some(second_op) = self.relation_op() {
            self.advance();
            let rhs = self.parse_additive()?;
            return build_chained_interval(lhs, op, middle, second_op, rhs).ok_or_else(|| {
                ParseError::new(
                    self.position(),
                    "连不等式形式不受支持（仅支持 a<x<b 或 a>x>b 且中间为变量）".to_owned(),
                )
            });
        }
        Ok(Expr::Relation {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(middle),
        })
    }

    fn relation_op(&self) -> Option<RelOp> {
        match self.peek_kind() {
            TokenKind::Eq => Some(RelOp::Eq),
            TokenKind::Ne => Some(RelOp::Ne),
            TokenKind::Lt => Some(RelOp::Lt),
            TokenKind::Le => Some(RelOp::Le),
            TokenKind::Gt => Some(RelOp::Gt),
            TokenKind::Ge => Some(RelOp::Ge),
            _ => None,
        }
    }

    fn parse_additive(&mut self) -> Result<Expr, ParseError> {
        let mut terms = vec![self.parse_multiplicative()?];
        loop {
            match self.peek_kind() {
                TokenKind::Plus => {
                    self.advance();
                    terms.push(self.parse_multiplicative()?);
                }
                TokenKind::Minus => {
                    self.advance();
                    let term = self.parse_multiplicative()?;
                    terms.push(negate(term));
                }
                _ => break,
            }
        }
        if terms.len() == 1 {
            Ok(terms.remove(0))
        } else {
            Ok(Expr::Sum(terms))
        }
    }

    fn parse_multiplicative(&mut self) -> Result<Expr, ParseError> {
        let mut factors = vec![self.parse_unary()?];
        loop {
            match self.peek_kind() {
                TokenKind::Star => {
                    self.advance();
                    factors.push(self.parse_unary()?);
                }
                TokenKind::Slash => {
                    self.advance();
                    let denominator = self.parse_unary()?;
                    let numerator = if factors.len() == 1 {
                        factors.remove(0)
                    } else {
                        Expr::Product(std::mem::take(&mut factors))
                    };
                    factors = vec![Expr::Fraction(Box::new(numerator), Box::new(denominator))];
                }
                kind if starts_implicit_factor(kind) => {
                    factors.push(self.parse_unary()?);
                }
                _ => break,
            }
        }
        if factors.len() == 1 {
            Ok(factors.remove(0))
        } else {
            Ok(Expr::Product(factors))
        }
    }

    fn parse_unary(&mut self) -> Result<Expr, ParseError> {
        match self.peek_kind() {
            TokenKind::Minus => {
                self.advance();
                let value = self.parse_unary()?;
                Ok(negate(value))
            }
            TokenKind::Plus => {
                self.advance();
                self.parse_unary()
            }
            _ => self.parse_power(),
        }
    }

    fn parse_power(&mut self) -> Result<Expr, ParseError> {
        let base = self.parse_atom()?;
        if !matches!(self.peek_kind(), TokenKind::Caret) {
            return Ok(base);
        }
        self.advance();
        let exponent = if matches!(self.peek_kind(), TokenKind::LBrace) {
            self.advance();
            let value = self.parse_additive()?;
            self.expect(&TokenKind::RBrace)?;
            value
        } else {
            self.parse_unary()?
        };
        Ok(Expr::Power(Box::new(base), Box::new(exponent)))
    }

    fn parse_atom(&mut self) -> Result<Expr, ParseError> {
        self.enter()?;
        let result = self.parse_atom_inner();
        self.leave();
        result
    }

    fn parse_atom_inner(&mut self) -> Result<Expr, ParseError> {
        let token = self.advance();
        match token.kind {
            TokenKind::Number(text) => {
                let value = Rational::parse(&text)
                    .map_err(|err| ParseError::new(token.position, format!("数值非法：{err}")))?;
                // 带单位数值：数字后紧跟 `\text{cm}` 或单位符号（含复合单位 km/h、m/s^2）
                if let Some(unit) = self.try_parse_unit() {
                    return Ok(Expr::Quantity {
                        value: Box::new(Expr::Number(value)),
                        unit,
                    });
                }
                Ok(Expr::Number(value))
            }
            TokenKind::Ident(name) => self.parse_ident_atom(name, token.position),
            TokenKind::Text(content) => Err(ParseError::new(
                token.position,
                format!("文本片段 {content:?} 不能单独作为数学表达式"),
            )),
            TokenKind::Command(name) => self.parse_command_atom(&name, token.position),
            TokenKind::EmptySet => Ok(Expr::Set(Vec::new())),
            TokenKind::Infinity => Ok(Expr::Variable("∞".to_owned())),
            TokenKind::LParen => {
                self.index = self.index.saturating_sub(1);
                self.parse_parenthesized()
            }
            TokenKind::LBracket => {
                self.index = self.index.saturating_sub(1);
                self.parse_bracketed_list()
            }
            TokenKind::LBrace => self.parse_brace_group(),
            TokenKind::SetOpen => {
                self.index = self.index.saturating_sub(1);
                self.parse_set()
            }
            TokenKind::Pipe => {
                let value = self.parse_additive()?;
                if matches!(self.peek_kind(), TokenKind::Pipe) {
                    self.advance();
                } else {
                    return Err(ParseError::new(
                        self.position(),
                        "绝对值缺少右竖线".to_owned(),
                    ));
                }
                Ok(Expr::Absolute(Box::new(value)))
            }
            other => Err(ParseError::new(
                token.position,
                format!("此处不能出现 {other:?}"),
            )),
        }
    }

    /// 尝试解析紧跟数值的单位（含 `/`、`*`、`^n` 复合形式）；失败则回滚。
    fn try_parse_unit(&mut self) -> Option<Unit> {
        let start = self.index;
        let mut symbol = String::new();
        match self.peek_kind() {
            TokenKind::Text(content) if Unit::parse(content).is_some() => {
                symbol.push_str(content);
                self.advance();
            }
            TokenKind::Ident(name) if Unit::parse(name).is_some() => {
                symbol.push_str(name);
                self.advance();
            }
            _ => return None,
        }
        loop {
            match self.peek_kind() {
                TokenKind::Slash | TokenKind::Star => {
                    let separator = if matches!(self.peek_kind(), TokenKind::Slash) {
                        '/'
                    } else {
                        '*'
                    };
                    if let TokenKind::Ident(name) = self.peek_ahead(1)
                        && Unit::parse(name).is_some()
                    {
                        symbol.push(separator);
                        symbol.push_str(name);
                        self.advance();
                        self.advance();
                        continue;
                    }
                    break;
                }
                TokenKind::Caret => {
                    if let TokenKind::Number(exponent) = self.peek_ahead(1)
                        && exponent.chars().all(|ch| ch.is_ascii_digit())
                    {
                        symbol.push('^');
                        symbol.push_str(exponent);
                        self.advance();
                        self.advance();
                        continue;
                    }
                    break;
                }
                _ => break,
            }
        }
        if let Some(unit) = Unit::parse(&symbol) {
            Some(unit)
        } else {
            self.index = start;
            None
        }
    }

    fn parse_ident_atom(&mut self, mut name: String, position: usize) -> Result<Expr, ParseError> {
        if matches!(self.peek_kind(), TokenKind::Underscore) {
            self.advance();
            let subscript = if matches!(self.peek_kind(), TokenKind::LBrace) {
                self.advance();
                let value = self.parse_additive()?;
                self.expect(&TokenKind::RBrace)?;
                value.to_text()
            } else {
                match self.advance().kind {
                    TokenKind::Number(text) | TokenKind::Ident(text) => text,
                    other => {
                        return Err(ParseError::new(
                            position,
                            format!("下标应为数字/标识符/花括号内容，实际 {other:?}"),
                        ));
                    }
                }
            };
            name.push('_');
            name.push_str(&subscript);
        }
        let is_function = matches!(
            name.as_str(),
            "sin"
                | "cos"
                | "tan"
                | "cot"
                | "sec"
                | "csc"
                | "arcsin"
                | "arccos"
                | "arctan"
                | "log"
                | "ln"
                | "lg"
                | "exp"
                | "abs"
                | "max"
                | "min"
                | "gcd"
                | "lcm"
        );
        if is_function && matches!(self.peek_kind(), TokenKind::LParen) {
            self.advance();
            let mut args = vec![self.parse_additive()?];
            while matches!(self.peek_kind(), TokenKind::Comma) {
                self.advance();
                args.push(self.parse_additive()?);
            }
            self.expect(&TokenKind::RParen)?;
            return Ok(Expr::Function { name, args });
        }
        if name == "∅" {
            return Ok(Expr::Set(Vec::new()));
        }
        if name.len() == 1 || name.contains('_') {
            return Ok(Expr::Variable(name));
        }
        if name.chars().all(|ch| ch.is_ascii_uppercase()) && name.len() <= 8 {
            // 连续大写字母（如 AB）交给选项解析层处理；作为数学表达式时按逐字母乘积理解。
            return Ok(Expr::Product(
                name.chars()
                    .map(|ch| Expr::Variable(ch.to_string()))
                    .collect(),
            ));
        }
        if !name.is_empty() && name.chars().all(char::is_alphabetic) {
            return Ok(Expr::Product(
                name.chars()
                    .map(|ch| Expr::Variable(ch.to_string()))
                    .collect(),
            ));
        }
        Err(ParseError::new(
            position,
            format!("无法识别的标识符 {name:?}"),
        ))
    }

    fn parse_command_atom(&mut self, name: &str, _position: usize) -> Result<Expr, ParseError> {
        match name {
            "frac" => {
                let numerator = self.parse_group()?;
                let denominator = self.parse_group()?;
                Ok(Expr::Fraction(Box::new(numerator), Box::new(denominator)))
            }
            "sqrt" => {
                let degree = if matches!(self.peek_kind(), TokenKind::LBracket) {
                    self.advance();
                    let value = self.parse_additive()?;
                    self.expect(&TokenKind::RBracket)?;
                    Some(Box::new(value))
                } else {
                    None
                };
                let radicand = self.parse_group()?;
                Ok(Expr::Radical {
                    degree,
                    radicand: Box::new(radicand),
                })
            }
            "vec" => self.parse_vector(),
            // 可识别但超出 T14 支持范围的函数/算子（三角、微积分等）：
            // 保留为 Function 节点，由比较器返回「无法判定」。
            other => {
                let mut args = Vec::new();
                while matches!(self.peek_kind(), TokenKind::LBrace | TokenKind::LParen) {
                    if matches!(self.peek_kind(), TokenKind::LBrace) {
                        args.push(self.parse_group()?);
                    } else {
                        self.advance();
                        args.push(self.parse_additive()?);
                        while matches!(self.peek_kind(), TokenKind::Comma) {
                            self.advance();
                            args.push(self.parse_additive()?);
                        }
                        self.expect(&TokenKind::RParen)?;
                    }
                }
                if args.is_empty() && matches!(self.peek_kind(), TokenKind::Ident(_)) {
                    // `\sin x` 形式：吞掉一个原子作为参数
                    args.push(self.parse_unary()?);
                }
                Ok(Expr::Function {
                    name: other.to_owned(),
                    args,
                })
            }
        }
    }

    fn parse_group(&mut self) -> Result<Expr, ParseError> {
        self.expect(&TokenKind::LBrace)?;
        let value = self.parse_additive()?;
        self.expect(&TokenKind::RBrace)?;
        Ok(value)
    }

    fn parse_brace_group(&mut self) -> Result<Expr, ParseError> {
        let value = self.parse_additive()?;
        self.expect(&TokenKind::RBrace)?;
        Ok(value)
    }

    fn parse_interval_endpoint(&mut self) -> Result<EndpointValue, ParseError> {
        let negative = matches!(self.peek_kind(), TokenKind::Minus);
        let offset = usize::from(negative);
        if matches!(self.peek_ahead(offset), TokenKind::Infinity) {
            if negative {
                self.advance();
            }
            self.advance();
            return Ok(if negative {
                EndpointValue::NegInfinity
            } else {
                EndpointValue::PosInfinity
            });
        }
        let value = self.parse_additive()?;
        Ok(EndpointValue::Value(value))
    }

    fn parse_parenthesized(&mut self) -> Result<Expr, ParseError> {
        self.expect(&TokenKind::LParen)?;
        // 区间端点（含无穷）优先：`(-\infty,2]`、`(-1,2)`
        let first_endpoint = self.parse_interval_endpoint()?;
        if !matches!(self.peek_kind(), TokenKind::Comma) {
            let value = match first_endpoint {
                EndpointValue::Value(value) => value,
                EndpointValue::NegInfinity | EndpointValue::PosInfinity => {
                    return Err(ParseError::new(
                        self.position(),
                        "无穷不能单独作为表达式".to_owned(),
                    ));
                }
            };
            // 括号内的集合表达式：`((-\infty,1) \cup (2,\infty))`
            if matches!(self.peek_kind(), TokenKind::Union | TokenKind::Intersection) {
                let expr = self.parse_set_expression_from(value)?;
                self.expect(&TokenKind::RParen)?;
                return Ok(expr);
            }
            self.expect(&TokenKind::RParen)?;
            return Ok(value);
        }
        self.advance();
        let second_endpoint = self.parse_interval_endpoint()?;
        // 三个及以上元素 → 向量（如 `(1,2,3)`）
        if matches!(self.peek_kind(), TokenKind::Comma) {
            let mut items: Vec<Expr> = Vec::new();
            for endpoint in [first_endpoint, second_endpoint] {
                match endpoint {
                    EndpointValue::Value(value) => items.push(value),
                    EndpointValue::NegInfinity | EndpointValue::PosInfinity => {
                        return Err(ParseError::new(
                            self.position(),
                            "向量分量不能为无穷".to_owned(),
                        ));
                    }
                }
            }
            while matches!(self.peek_kind(), TokenKind::Comma) {
                self.advance();
                items.push(self.parse_additive()?);
            }
            match self.peek_kind() {
                TokenKind::RParen | TokenKind::RBracket => {}
                other => {
                    return Err(ParseError::new(
                        self.position(),
                        format!("向量列表后应为 ) 或 ]，实际 {other:?}"),
                    ));
                }
            }
            self.advance();
            return Ok(Expr::Vector(items));
        }
        let end_closed = match self.peek_kind() {
            TokenKind::RParen => false,
            TokenKind::RBracket => true,
            other => {
                return Err(ParseError::new(
                    self.position(),
                    format!("括号/区间列表后应为 ) 或 ]，实际 {other:?}"),
                ));
            }
        };
        self.advance();
        let start = match first_endpoint {
            EndpointValue::Value(value) => IntervalEnd::Value {
                expr: Box::new(value),
                closed: false,
            },
            EndpointValue::NegInfinity => IntervalEnd::NegInfinity,
            EndpointValue::PosInfinity => {
                return Err(ParseError::new(
                    self.position(),
                    "区间左端点不能为正无穷".to_owned(),
                ));
            }
        };
        let end = match second_endpoint {
            EndpointValue::Value(value) => IntervalEnd::Value {
                expr: Box::new(value),
                closed: end_closed,
            },
            EndpointValue::PosInfinity => IntervalEnd::PosInfinity,
            EndpointValue::NegInfinity => {
                return Err(ParseError::new(
                    self.position(),
                    "区间右端点不能为负无穷".to_owned(),
                ));
            }
        };
        // 两个有限端点且没有更多元素时视为区间；三元素及以上视为向量
        if let (IntervalEnd::Value { expr: left, .. }, IntervalEnd::Value { .. }) = (&start, &end)
            && matches!(self.peek_kind(), TokenKind::Comma)
        {
            let mut items: Vec<Expr> = vec![*left.clone()];
            while matches!(self.peek_kind(), TokenKind::Comma) {
                self.advance();
                items.push(self.parse_additive()?);
            }
            match self.peek_kind() {
                TokenKind::RParen | TokenKind::RBracket => {}
                other => {
                    return Err(ParseError::new(
                        self.position(),
                        format!("向量列表后应为 ) 或 ]，实际 {other:?}"),
                    ));
                }
            }
            self.advance();
            return Ok(Expr::Vector(items));
        }
        Ok(Expr::Interval(Interval::new(start, end)))
    }

    fn parse_bracketed_list(&mut self) -> Result<Expr, ParseError> {
        self.expect(&TokenKind::LBracket)?;
        let start = self.parse_interval_endpoint()?;
        if !matches!(self.peek_kind(), TokenKind::Comma) {
            self.expect(&TokenKind::RBracket)?;
            return match start {
                EndpointValue::Value(value) => Ok(value),
                EndpointValue::NegInfinity | EndpointValue::PosInfinity => Err(ParseError::new(
                    self.position(),
                    "无穷不能单独作为表达式".to_owned(),
                )),
            };
        }
        self.advance();
        let end_raw = self.parse_interval_endpoint()?;
        let end_closed = match self.peek_kind() {
            TokenKind::RBracket => true,
            TokenKind::RParen => false,
            other => {
                return Err(ParseError::new(
                    self.position(),
                    format!("区间后应为 ] 或 )，实际 {other:?}"),
                ));
            }
        };
        self.advance();
        let start = match start {
            EndpointValue::Value(value) => IntervalEnd::Value {
                expr: Box::new(value),
                closed: true,
            },
            EndpointValue::NegInfinity => IntervalEnd::NegInfinity,
            EndpointValue::PosInfinity => {
                return Err(ParseError::new(
                    self.position(),
                    "区间左端点不能为正无穷".to_owned(),
                ));
            }
        };
        let end = match end_raw {
            EndpointValue::Value(value) => IntervalEnd::Value {
                expr: Box::new(value),
                closed: end_closed,
            },
            EndpointValue::PosInfinity => IntervalEnd::PosInfinity,
            EndpointValue::NegInfinity => {
                return Err(ParseError::new(
                    self.position(),
                    "区间右端点不能为负无穷".to_owned(),
                ));
            }
        };
        Ok(Expr::Interval(Interval::new(start, end)))
    }

    fn parse_set(&mut self) -> Result<Expr, ParseError> {
        if matches!(self.peek_kind(), TokenKind::EmptySet) {
            self.advance();
            return Ok(Expr::Set(Vec::new()));
        }
        let open = matches!(self.peek_kind(), TokenKind::SetOpen);
        self.advance();
        if open {
            if matches!(self.peek_kind(), TokenKind::SetClose) {
                self.advance();
                return Ok(Expr::Set(Vec::new()));
            }
            let mut items = vec![self.parse_additive()?];
            while matches!(self.peek_kind(), TokenKind::Comma) {
                self.advance();
                items.push(self.parse_additive()?);
            }
            self.expect(&TokenKind::SetClose)?;
            return Ok(Expr::Set(items));
        }
        // 顶层 `{...}` 视为集合（嵌套 `{...}` 是分组）
        if matches!(self.peek_kind(), TokenKind::RBrace) {
            self.advance();
            return Ok(Expr::Set(Vec::new()));
        }
        let mut items = vec![self.parse_additive()?];
        while matches!(self.peek_kind(), TokenKind::Comma) {
            self.advance();
            items.push(self.parse_additive()?);
        }
        self.expect(&TokenKind::RBrace)?;
        Ok(Expr::Set(items))
    }

    fn parse_brace_set(&mut self) -> Result<Expr, ParseError> {
        self.index = self.index.saturating_sub(0);
        self.parse_set()
    }

    fn parse_vector(&mut self) -> Result<Expr, ParseError> {
        let mut items = Vec::new();
        // 形式一：\vec{(1,2)} 或 \vec{1,2}
        self.expect(&TokenKind::LBrace)?;
        if matches!(self.peek_kind(), TokenKind::LParen) {
            self.advance();
            loop {
                items.push(self.parse_additive()?);
                if matches!(self.peek_kind(), TokenKind::Comma) {
                    self.advance();
                    continue;
                }
                break;
            }
            self.expect(&TokenKind::RParen)?;
        } else {
            loop {
                items.push(self.parse_additive()?);
                if matches!(self.peek_kind(), TokenKind::Comma) {
                    self.advance();
                    continue;
                }
                break;
            }
        }
        self.expect(&TokenKind::RBrace)?;
        Ok(Expr::Vector(items))
    }

    fn parse_matrix(&mut self) -> Result<Expr, ParseError> {
        let token = self.advance();
        let TokenKind::BeginEnv(environment) = token.kind else {
            unreachable!("parse_matrix 只在 BeginEnv 调用");
        };
        if !matches!(
            environment.as_str(),
            "pmatrix" | "bmatrix" | "vmatrix" | "matrix" | "array"
        ) {
            return Err(ParseError::new(
                token.position,
                format!("不支持的矩阵环境 {environment:?}"),
            ));
        }
        let mut rows: Vec<Vec<Expr>> = Vec::new();
        let mut row: Vec<Expr> = Vec::new();
        loop {
            if matches!(self.peek_kind(), TokenKind::EndEnv) {
                self.advance();
                if !row.is_empty() {
                    rows.push(row);
                }
                break;
            }
            if matches!(self.peek_kind(), TokenKind::RowSep) {
                self.advance();
                if row.is_empty() {
                    return Err(ParseError::new(self.position(), "矩阵存在空行".to_owned()));
                }
                rows.push(std::mem::take(&mut row));
                continue;
            }
            let cell = self.parse_additive()?;
            row.push(cell);
            match self.peek_kind() {
                TokenKind::Amp => {
                    self.advance();
                }
                TokenKind::RowSep | TokenKind::EndEnv => {}
                other => {
                    return Err(ParseError::new(
                        self.position(),
                        format!("矩阵单元后应为 & 或 \\\\，实际 {other:?}"),
                    ));
                }
            }
        }
        if rows.is_empty() {
            return Err(ParseError::new(token.position, "矩阵为空".to_owned()));
        }
        let columns = rows.first().map_or(0, Vec::len);
        if rows.iter().any(|row| row.len() != columns) {
            return Err(ParseError::new(
                token.position,
                "矩阵各行列数不一致".to_owned(),
            ));
        }
        Ok(Expr::Matrix(rows))
    }
}

enum EndpointValue {
    Value(Expr),
    NegInfinity,
    PosInfinity,
}

fn negate(value: Expr) -> Expr {
    match value {
        Expr::Number(number) => Expr::Number(number.neg()),
        other => Expr::Product(vec![Expr::Number(Rational::from_i64(-1)), other]),
    }
}

fn starts_implicit_factor(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Number(_)
            | TokenKind::Ident(_)
            | TokenKind::LParen
            | TokenKind::LBrace
            | TokenKind::SetOpen
            | TokenKind::BeginEnv(_)
            | TokenKind::Command(_)
            | TokenKind::EmptySet
    )
}

/// 构造连不等式对应的区间（`a op1 x op2 b`）。
fn build_chained_interval(
    lhs: Expr,
    first_op: RelOp,
    middle: Expr,
    second_op: RelOp,
    rhs: Expr,
) -> Option<Expr> {
    let Expr::Variable(name) = &middle else {
        return None;
    };
    // 中间必须是变量，且两端不含该变量
    if lhs.variables().iter().any(|variable| variable == name)
        || rhs.variables().iter().any(|variable| variable == name)
    {
        return None;
    }
    let lower_first = matches!(first_op, RelOp::Lt | RelOp::Le);
    let lower_second = matches!(second_op, RelOp::Lt | RelOp::Le);
    let upper_first = matches!(first_op, RelOp::Gt | RelOp::Ge);
    let upper_second = matches!(second_op, RelOp::Gt | RelOp::Ge);
    if lower_first && lower_second {
        return Some(Expr::Interval(Interval::new(
            IntervalEnd::Value {
                expr: Box::new(lhs),
                closed: first_op == RelOp::Le,
            },
            IntervalEnd::Value {
                expr: Box::new(rhs),
                closed: second_op == RelOp::Le,
            },
        )));
    }
    if upper_first && upper_second {
        return Some(Expr::Interval(Interval::new(
            IntervalEnd::Value {
                expr: Box::new(rhs),
                closed: second_op == RelOp::Ge,
            },
            IntervalEnd::Value {
                expr: Box::new(lhs),
                closed: first_op == RelOp::Ge,
            },
        )));
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::ast::{Expr, IntervalEnd};

    fn parse(input: &str) -> Expr {
        parse_expr(input).unwrap()
    }

    #[test]
    fn parses_numbers_and_arithmetic() {
        assert_eq!(parse("2.5"), Expr::Number(Rational::parse("2.5").unwrap()));
        assert_eq!(parse("1+2*3").to_text(), "1 + 2*3");
        assert_eq!(parse("-3").to_text(), "-3");
        assert_eq!(parse("2/3").to_text(), "2/3");
        assert_eq!(parse("x^2").to_text(), "x^2");
        assert_eq!(parse("2^3^2").to_text(), "2^3^2");
    }

    #[test]
    fn parses_latex_structures() {
        assert_eq!(parse("\\frac{1}{2}").to_text(), "1/2");
        assert_eq!(parse("\\sqrt{2}").to_text(), "\\sqrt{2}");
        assert_eq!(parse("\\sqrt[3]{8}").to_text(), "\\sqrt[3]{8}");
        assert_eq!(parse("\\left(1+2\\right)").to_text(), "1 + 2");
        assert_eq!(parse("|x|").to_text(), "|x|");
        assert_eq!(
            parse("\\begin{pmatrix}1&2\\\\3&4\\end{pmatrix}").to_text(),
            "\\begin{pmatrix}1&2\\\\3&4\\end{pmatrix}"
        );
        assert_eq!(parse("\\vec{(1,2,3)}").to_text(), "\\vec{(1,2,3)}");
    }

    #[test]
    fn parses_intervals_sets_relations() {
        let interval = parse("[1,2)");
        assert_eq!(
            interval,
            Expr::Interval(Interval::new(
                IntervalEnd::Value {
                    expr: Box::new(Expr::Number(Rational::from_i64(1))),
                    closed: true,
                },
                IntervalEnd::Value {
                    expr: Box::new(Expr::Number(Rational::from_i64(2))),
                    closed: false,
                },
            ))
        );
        assert_eq!(parse("(-\\infty,2]").to_text(), "(-\\infty,2]");
        assert_eq!(parse("\\{1,2,2\\}").to_text(), "\\{1,2,2\\}");
        assert_eq!(parse("x=1").to_text(), "x=1");
        assert_eq!(parse("x\\le 2").to_text(), "x\\le2");
        assert_eq!(parse("x=1 或 x=2").to_text(), "x=1 或 x=2");
    }

    #[test]
    fn parses_units_and_functions() {
        assert_eq!(parse("12\\text{cm}").to_text(), "12\\text{cm}");
        assert_eq!(parse("50 km/h").to_text(), "50\\text{km/h}");
        let function = parse("\\sin(x)");
        assert!(matches!(function, Expr::Function { .. }));
        let integral = parse("\\int x");
        assert!(matches!(integral, Expr::Function { .. }));
    }

    #[test]
    fn rejects_bad_syntax() {
        for bad in ["1+", "(1+2", "\\frac{1}", "1..2", "!"] {
            assert!(parse_expr(bad).is_err(), "{bad}");
        }
        // 未知命令按「可识别但超出支持范围」处理（函数节点），不是语法错误
        assert!(matches!(
            parse_expr("\\unknowncommand").unwrap(),
            Expr::Function { .. }
        ));
    }

    #[test]
    fn parse_error_has_position_and_reason() {
        let error = parse_expr("1+").unwrap_err();
        assert!(error.to_string().contains("无法解析"));
        assert_ne!(error.reason.len(), 0);
    }
}
