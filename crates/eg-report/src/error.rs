//! 报表层错误（`SPEC-report.md` §5：拒绝导出、不截断、不静默修正）。

use core::fmt;

/// 报表构建/导出错误。
///
/// 所有变体都带中文可读消息；技术细节由调用方写入 `tracing`（本 crate 不直接写日志）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportError {
    /// 数据源读取失败（端口实现方报告）。
    Source(String),
    /// 数据不满足规范不变量（总分不符、分数越界、记录与试卷不一致等）。
    Invariant(String),
    /// 口径/配置非法（抽检率、权重、阈值越界或顺序错误）。
    Config(String),
    /// 没有任何可导出的有效样本（允许导出空表，但由调用方决定是否提示）。
    Empty(String),
    /// 命名规则无法生成合法文件名（清洗后为空或超长且哈希不可用）。
    Naming(String),
    /// 文件系统错误（写临时文件、rename、权限、磁盘）。
    Io(String),
    /// XLSX 渲染错误。
    Xlsx(String),
    /// 导出审计写入失败（审计恒开；失败时导出视为失败并清理文件）。
    Audit(String),
    /// 目标文件已存在且未允许覆盖。
    OverwriteRefused(String),
    /// 重放结果与原始导出不一致。
    ReplayMismatch {
        /// 人类可读的差异摘要。
        detail: String,
    },
}

impl fmt::Display for ReportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source(message) => write!(f, "读取报表数据失败：{message}"),
            Self::Invariant(message) => write!(f, "报表数据不满足不变量：{message}"),
            Self::Config(message) => write!(f, "报表配置非法：{message}"),
            Self::Empty(message) => write!(f, "没有可导出的数据：{message}"),
            Self::Naming(message) => write!(f, "导出命名失败：{message}"),
            Self::Io(message) => write!(f, "文件操作失败：{message}"),
            Self::Xlsx(message) => write!(f, "XLSX 生成失败：{message}"),
            Self::Audit(message) => write!(f, "导出审计写入失败：{message}"),
            Self::OverwriteRefused(path) => write!(f, "目标文件已存在且未允许覆盖：{path}"),
            Self::ReplayMismatch { detail } => {
                write!(f, "重放结果与原始导出一致性校验失败：{detail}")
            }
        }
    }
}

impl std::error::Error for ReportError {}

impl From<std::io::Error> for ReportError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

impl From<rust_xlsxwriter::XlsxError> for ReportError {
    fn from(error: rust_xlsxwriter::XlsxError) -> Self {
        Self::Xlsx(error.to_string())
    }
}

/// 便捷别名。
pub type Result<T> = core::result::Result<T, ReportError>;
