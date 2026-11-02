//! 对齐错误（`SPEC-align.md` §5 失败模式；报告只携带稳定错误码 + 中文文案）。

use thiserror::Error;

/// 对齐错误。
#[derive(Debug, Error, Clone)]
pub enum AlignError {
    /// `identity.*`/`align.*` 配置越界或不支持（启动/批次前拒绝）。
    #[error("对齐配置非法：{0}")]
    InvalidSettings(String),
    /// 名单不可用（空名单、学号重复、姓名为空）。
    #[error("名单不可用：{0}")]
    Roster(String),
    /// 试卷/模板不可用（版本不一致等；需要人工处理而非崩溃）。
    #[error("试卷或模板不可用：{0}")]
    Template(String),
    /// 输入不一致（页 id 重复、识别结果与页不对应等）。
    #[error("输入不一致：{0}")]
    InputMismatch(String),
    /// 人工修正请求非法（目标学生不在名单、区域越界、命令哈希不符等）。
    #[error("人工修正非法：{0}")]
    InvalidManual(String),
    /// 落库/审计失败。
    #[error("审计写入失败：{0}")]
    Audit(String),
    /// 内部不变量被破坏（属缺陷，不是教师输入问题）。
    #[error("内部错误：{0}")]
    Internal(String),
    /// 明确未实现的通道（条码身份等）：显式失败，不静默跳过。
    #[error("该能力尚未实现：{0}")]
    NotYetImplemented(&'static str),
}

impl AlignError {
    /// 稳定错误码（报告/日志用；界面不显示）。
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidSettings(_) => "invalid_settings",
            Self::Roster(_) => "roster_invalid",
            Self::Template(_) => "template_invalid",
            Self::InputMismatch(_) => "input_mismatch",
            Self::InvalidManual(_) => "invalid_manual",
            Self::Audit(_) => "audit_failed",
            Self::Internal(_) => "internal",
            Self::NotYetImplemented(_) => "not_implemented",
        }
    }

    /// 是否可重试（对齐是纯计算 + 追加审计，只有落库类错误可整体重试）。
    pub const fn retryable(&self) -> bool {
        matches!(self, Self::Audit(_))
    }
}
