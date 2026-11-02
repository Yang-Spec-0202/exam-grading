//! 导出命名规则（`SPEC-report.md` §3.2，命名规则版本 [`crate::settings::NAMING_RULE_VERSION`]）。
//!
//! 规则要点：
//! - 整卷报表：`试卷名_模式_时间戳.扩展名`；两模式一起导出用「全部」。
//! - 单学生成绩单：`学生名_试卷名_模式_时间戳.扩展名`。
//! - 脱敏：上述名称追加 [`crate::masking::MASK_SUFFIX`]。
//! - 非法字符替换为下划线并折叠；主体超过 60 字符截断并追加内容哈希前 8 位。
//! - 同名冲突追加 `_2`、`_3`……（序号写入审计）。
//!
//! 时间戳格式固定 `yyyyMMdd-HHmmss`；本 crate 只接受调用方给出的时间（`eg-platform` 的
//! `Clock`），不自行读系统时间，也不依赖本地时区（偏差见 `docs/dev/research/report.md`）。

use crate::error::{ReportError, Result};
use crate::masking::MASK_SUFFIX;
use crate::settings::NAMING_RULE_VERSION;
use chrono::{DateTime, Utc};
use eg_domain::Sha256Hex;

/// 文件名主体最大字符数（超出截断并追加哈希前 8 位）。
pub const MAX_NAME_CHARS: usize = 60;

/// 时间戳格式（规范 §3.2）。
pub const TIMESTAMP_FORMAT: &str = "%Y%m%d-%H%M%S";

/// 模式 token（规范 §3.2：取「标准」「快速」「全部」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeToken {
    /// 标准模式。
    Standard,
    /// 快速模式。
    Fast,
    /// 两模式一起导出（仅对照，不产出混合分）。
    All,
}

impl ModeToken {
    /// 生成文件名中的模式 token。
    ///
    /// `fast_export_label == false` 时快速模式不出现「快速」字样（规范 §3.2），
    /// 此时该段省略；元信息与行内模式列仍标注快速模式。
    #[must_use]
    pub const fn token(self, fast_export_label: bool) -> Option<&'static str> {
        match self {
            Self::Standard => Some("标准"),
            Self::Fast => {
                if fast_export_label {
                    Some("快速")
                } else {
                    None
                }
            }
            Self::All => Some("全部"),
        }
    }
}

/// 命名器：承载命名规则版本与「快速标注开关」，供导出运行与审计记录。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportNaming {
    rule_version: u32,
    fast_export_label: bool,
}

impl Default for ExportNaming {
    fn default() -> Self {
        Self {
            rule_version: NAMING_RULE_VERSION,
            fast_export_label: true,
        }
    }
}

impl ExportNaming {
    /// 使用当前命名规则版本（快速标注默认开）。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            rule_version: NAMING_RULE_VERSION,
            fast_export_label: true,
        }
    }

    /// 按 `fast.export_label` 配置创建（关时文件名不出现「快速」，模式列仍标注）。
    #[must_use]
    pub const fn with_fast_label(fast_export_label: bool) -> Self {
        Self {
            rule_version: NAMING_RULE_VERSION,
            fast_export_label,
        }
    }

    /// 命名规则版本（写入导出运行与审计）。
    #[must_use]
    pub const fn rule_version(self) -> u32 {
        self.rule_version
    }

    /// 整卷报表文件名。
    ///
    /// # Errors
    ///
    /// 扩展名为空、主体清洗后不可用或含路径分隔符时返回 [`ReportError::Naming`]。
    pub fn class_file_name(
        self,
        paper_title: &str,
        token: ModeToken,
        exported_at: DateTime<Utc>,
        masked: bool,
        extension: &str,
    ) -> Result<String> {
        let mut parts = vec![clean_component(paper_title, "试卷名")?];
        if let Some(token) = token.token(self.fast_export_label) {
            parts.push(token.to_owned());
        }
        parts.push(format_timestamp(exported_at));
        compose(&parts, masked, extension)
    }

    /// 单学生成绩单文件名。
    ///
    /// # Errors
    ///
    /// 同 [`ExportNaming::class_file_name`]。
    pub fn student_file_name(
        self,
        student_name: &str,
        paper_title: &str,
        token: ModeToken,
        exported_at: DateTime<Utc>,
        masked: bool,
        extension: &str,
    ) -> Result<String> {
        let mut parts = vec![
            clean_component(student_name, "学生名")?,
            clean_component(paper_title, "试卷名")?,
        ];
        if let Some(token) = token.token(self.fast_export_label) {
            parts.push(token.to_owned());
        }
        parts.push(format_timestamp(exported_at));
        compose(&parts, masked, extension)
    }

    /// 同名冲突：把 `_2`、`_3`…… 追加到扩展名前，直到不与 `taken` 冲突。
    #[must_use]
    pub fn dedupe(&self, file_name: &str, taken: &[String]) -> String {
        dedupe_file_name(file_name, taken)
    }
}

/// 时间戳文本（UTC，`yyyyMMdd-HHmmss`）。
#[must_use]
pub fn format_timestamp(exported_at: DateTime<Utc>) -> String {
    exported_at.format(TIMESTAMP_FORMAT).to_string()
}

/// 清洗单个名称片段：非法字符→下划线、折叠下划线、去首尾下划线与空白、超长截断+哈希。
///
/// # Errors
///
/// 清洗后为空时返回 [`ReportError::Naming`]。
pub fn clean_component(raw: &str, field: &str) -> Result<String> {
    let mut out = String::with_capacity(raw.len());
    let mut last_was_underscore = false;
    for ch in raw.chars() {
        let mapped = if ch.is_control() || is_invalid(ch) {
            '_'
        } else {
            ch
        };
        if mapped == '_' {
            if !last_was_underscore && !out.is_empty() {
                out.push('_');
            }
            last_was_underscore = true;
        } else {
            out.push(mapped);
            last_was_underscore = false;
        }
    }
    let trimmed = out.trim_matches(|ch: char| ch == '_' || ch.is_whitespace());
    if trimmed.is_empty() {
        return Err(ReportError::Naming(format!(
            "{field}清洗后为空，无法生成合法文件名"
        )));
    }
    let trimmed = trimmed.to_owned();
    if trimmed.chars().count() <= MAX_NAME_CHARS {
        return Ok(trimmed);
    }
    let head: String = trimmed.chars().take(MAX_NAME_CHARS).collect();
    let hash = Sha256Hex::of(trimmed.as_bytes()).to_hex();
    Ok(format!("{head}_{}", &hash[..8]))
}

/// 非法字符集合（规范 §3.2）。
fn is_invalid(ch: char) -> bool {
    matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
}

/// 拼接主体并校验扩展名。
fn compose(parts: &[String], masked: bool, extension: &str) -> Result<String> {
    let ext = extension.trim().trim_start_matches('.');
    if ext.is_empty() || !ext.chars().all(|ch| ch.is_ascii_alphanumeric()) {
        return Err(ReportError::Naming(format!("扩展名不合法：{extension:?}")));
    }
    let mut stem = parts.join("_");
    if masked {
        stem.push_str(MASK_SUFFIX);
    }
    let file_name = format!("{stem}.{ext}");
    if file_name.contains(['/', '\\']) {
        return Err(ReportError::Naming(format!(
            "文件名含路径分隔符：{file_name}"
        )));
    }
    Ok(file_name)
}

/// 在扩展名前追加冲突序号。
fn dedupe_file_name(file_name: &str, taken: &[String]) -> String {
    if !taken.iter().any(|candidate| candidate == file_name) {
        return file_name.to_owned();
    }
    let (stem, ext) = split_extension(file_name);
    let mut sequence = 2u32;
    loop {
        let candidate = format!("{stem}_{sequence}.{ext}");
        if !taken.iter().any(|existing| existing == &candidate) {
            return candidate;
        }
        sequence = sequence.saturating_add(1);
    }
}

/// 拆分为（不含扩展名的主体, 扩展名）。
fn split_extension(file_name: &str) -> (&str, &str) {
    match file_name.rsplit_once('.') {
        Some((stem, ext)) => (stem, ext),
        None => (file_name, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone as _;

    fn at() -> DateTime<Utc> {
        match Utc.with_ymd_and_hms(2026, 11, 1, 19, 30, 0) {
            chrono::LocalResult::Single(value) => value,
            _ => panic!("夹具时间必须唯一"),
        }
    }

    #[test]
    fn class_names_follow_spec() {
        let naming = ExportNaming::new();
        let name = naming
            .class_file_name("期中数学", ModeToken::Standard, at(), false, "xlsx")
            .unwrap();
        assert_eq!(name, "期中数学_标准_20261101-193000.xlsx");
        let all = naming
            .class_file_name("期中数学", ModeToken::All, at(), false, "html")
            .unwrap();
        assert_eq!(all, "期中数学_全部_20261101-193000.html");
        let masked = naming
            .class_file_name("期中数学", ModeToken::Standard, at(), true, "xlsx")
            .unwrap();
        assert_eq!(masked, "期中数学_标准_20261101-193000_脱敏.xlsx");
    }

    #[test]
    fn fast_label_off_omits_token() {
        assert_eq!(ModeToken::Fast.token(true), Some("快速"));
        assert_eq!(ModeToken::Fast.token(false), None);
        let naming = ExportNaming::with_fast_label(false);
        let name = naming
            .class_file_name("期中数学", ModeToken::Fast, at(), false, "xlsx")
            .unwrap();
        assert_eq!(name, "期中数学_20261101-193000.xlsx");
    }

    #[test]
    fn student_names_follow_spec() {
        let naming = ExportNaming::new();
        let name = naming
            .student_file_name("张三", "期中数学", ModeToken::Standard, at(), false, "xlsx")
            .unwrap();
        assert_eq!(name, "张三_期中数学_标准_20261101-193000.xlsx");
    }

    #[test]
    fn sanitizes_invalid_characters() {
        let naming = ExportNaming::new();
        let name = naming
            .class_file_name("期中/数学:2026*?", ModeToken::Standard, at(), false, "xlsx")
            .unwrap();
        assert_eq!(name, "期中_数学_2026_标准_20261101-193000.xlsx");
    }

    #[test]
    fn truncates_long_names_with_hash() {
        let long = "数".repeat(80);
        let cleaned = clean_component(&long, "试卷名").unwrap();
        assert_eq!(cleaned.chars().count(), MAX_NAME_CHARS + 9);
        assert!(cleaned.ends_with(|ch: char| ch.is_ascii_hexdigit()));
        // 可复现
        assert_eq!(cleaned, clean_component(&long, "试卷名").unwrap());
    }

    #[test]
    fn dedupes_collisions() {
        let naming = ExportNaming::new();
        let first = "期中数学_全部_20261101-193000.xlsx".to_owned();
        let taken = vec![first.clone()];
        assert_eq!(
            naming.dedupe(&first, &taken),
            "期中数学_全部_20261101-193000_2.xlsx"
        );
        let taken = vec![
            first.clone(),
            "期中数学_全部_20261101-193000_2.xlsx".to_owned(),
        ];
        assert_eq!(
            naming.dedupe(&first, &taken),
            "期中数学_全部_20261101-193000_3.xlsx"
        );
    }

    #[test]
    fn rejects_empty_or_bad_extension() {
        let naming = ExportNaming::new();
        assert!(
            naming
                .class_file_name("///", ModeToken::Standard, at(), false, "xlsx")
                .is_err()
        );
        assert!(
            naming
                .class_file_name("数学", ModeToken::Standard, at(), false, "")
                .is_err()
        );
        assert!(
            naming
                .class_file_name("数学", ModeToken::Standard, at(), false, "x/")
                .is_err()
        );
    }
}
