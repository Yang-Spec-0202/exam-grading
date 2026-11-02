//! 脱敏导出（`SPEC-report.md` §3.2/§3.5/§9.6：掩码名、文件后缀、导出前自检）。
//!
//! 脱敏规则：
//! - 学生姓名替换为「学生NN」（按成绩单序号，从 1 起，10 次重放稳定）。
//! - 学号替换为「S-NNN」。
//! - 文件名追加 [`MASK_SUFFIX`]；内容自检命中真名/真学号即中止（不生成文件）。
//! - 试卷名保留，但元信息提示可能含个人信息（规范 §3.2）。
//!
//! 本模块是纯函数，不做 IO；自检针对「将要写出的可见文本」，而不是二进制字节。

use crate::error::{ReportError, Result};

/// 脱敏文件名后缀（规范 §3.2）。
pub const MASK_SUFFIX: &str = "_脱敏";

/// 脱敏提示：试卷名保留原样，可能含个人信息。
pub const MASK_NOTE: &str = "试卷名按原样保留，可能含班级或教师姓名，建议改用中性试卷名后重新导出";

/// 掩码姓名：`学生NN`（两名以上自然扩展为 `学生100`）。
#[must_use]
pub fn masked_name(index: usize) -> String {
    format!("学生{:02}", index.saturating_add(1))
}

/// 掩码学号：`S-NNN`。
#[must_use]
pub fn masked_student_no(index: usize) -> String {
    format!("S-{:03}", index.saturating_add(1))
}

/// 脱敏自检：可见文本中出现任一敏感原文即中止导出（规范 §5「脱敏自检失败」）。
///
/// # Errors
///
/// 命中时返回 [`ReportError::Invariant`]，只报告命中数量，不回显敏感值本身。
pub fn assert_no_leak(visible: &str, secrets: &[String]) -> Result<()> {
    let mut hits = 0usize;
    for secret in secrets {
        if secret.trim().is_empty() {
            continue;
        }
        if visible.contains(secret.as_str()) {
            hits += 1;
        }
    }
    if hits > 0 {
        return Err(ReportError::Invariant(format!(
            "脱敏自检失败：导出内容命中 {hits} 处学生原文，已中止且不保留任何文件"
        )));
    }
    Ok(())
}

/// 收集需要自检的敏感原文（姓名与学号）；去重并跳过空值。
#[must_use]
pub fn leak_secrets<'a>(
    names: impl IntoIterator<Item = &'a str>,
    student_numbers: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    let mut secrets: Vec<String> = names
        .into_iter()
        .chain(student_numbers)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .collect();
    secrets.sort_unstable();
    secrets.dedup();
    secrets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masked_values_are_stable() {
        assert_eq!(masked_name(0), "学生01");
        assert_eq!(masked_name(9), "学生10");
        assert_eq!(masked_name(99), "学生100");
        assert_eq!(masked_student_no(0), "S-001");
        assert_eq!(masked_student_no(999), "S-1000");
    }

    #[test]
    fn leak_check_detects_names_and_numbers() {
        let secrets = leak_secrets(["张三", "李四"], ["20240101"]);
        assert!(assert_no_leak("学生01 的成绩", &secrets).is_ok());
        assert!(assert_no_leak("张三 的成绩", &secrets).is_err());
        assert!(assert_no_leak("学号 20240101", &secrets).is_err());
        assert!(assert_no_leak("学生01", &[]).is_ok());
    }
}
