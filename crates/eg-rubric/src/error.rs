//! 规则模块错误类型（`SPEC-rubric.md` §5；面向教师中文可读，技术细节进日志）。
//!
//! 约定：库层不 panic、不 `unwrap`；所有错误都携带足以定位的中文信息。
//! 领域不变量错误用 [`RubricError::Domain`] 透传 `eg_domain::DomainError`，
//! 保证与 `SPEC-domain.md` 的错误分类一致。

use eg_domain::{
    DomainError, DraftStatus, ModelCallId, QuestionId, QuestionKind, RubricId, Sha256Hex,
};

/// 本模块统一 `Result`。
pub type Result<T> = core::result::Result<T, RubricError>;

/// 规则模块错误。
#[derive(Debug, thiserror::Error)]
pub enum RubricError {
    /// 草稿不存在。
    #[error("规则草稿不存在：{0}")]
    DraftNotFound(RubricId),

    /// 已确认草稿不允许再次编辑（终态，修改必须新建草稿）。
    #[error("草稿已进入终态（{status:?}），不可编辑；修改规则必须新建草稿并再次确认")]
    DraftNotEditable {
        /// 当前状态。
        status: DraftStatus,
    },

    /// 当前状态不允许该操作（如未提交待确认就确认）。
    #[error("草稿当前状态为 {status:?}，不能执行该操作")]
    DraftWrongStatus {
        /// 当前状态。
        status: DraftStatus,
    },

    /// 草稿存在错误级问题，禁止确认（`SPEC-domain.md` §5 不变量）。
    #[error("草稿存在 {count} 个错误级校验问题，禁止确认；请先按校验报告修正")]
    DraftHasErrors {
        /// 错误级问题数量。
        count: usize,
    },

    /// 并发确认冲突：试卷规则版本已被其他操作推进。
    #[error("规则版本冲突：期望基于版本 {expected}，当前为 {actual}；请重新加载最新版本")]
    VersionConflict {
        /// 调用方读取草稿时看到的版本。
        expected: u32,
        /// 存储中的当前版本。
        actual: u32,
    },

    /// 指定模板不存在（模板缺失禁止套用）。
    #[error("模板不存在：{0}；模板缺失时禁止套用（可改用手写直译草稿）")]
    TemplateMissing(String),

    /// 模板已停用（历史卷只读快照，仅供历史试卷套用）。
    #[error(
        "模板 {set_id} 已停用（{retired_since} 年起不再命制），仅供历史试卷套用；\
         如确需套用请在模板选择中显式允许 retired"
    )]
    TemplateRetired {
        /// 卷型 id。
        set_id: String,
        /// 停止命制年份。
        retired_since: u32,
    },

    /// 模板数据不合法（加载期或套用期错误级问题）。
    #[error("模板数据不合法：{message}")]
    TemplateInvalid {
        /// 中文可读原因（含文件/字段定位）。
        message: String,
    },

    /// 模板文件读取或 TOML 解析失败。
    #[error("模板文件读取失败：{path}：{message}")]
    TemplateIo {
        /// 文件路径（相对工作区或绝对路径）。
        path: String,
        /// 底层错误信息。
        message: String,
    },

    /// 模板中没有对应题号。
    #[error("模板 {set_id} 中没有题号 {ordinal}；模板与试卷不匹配，禁止套用")]
    TemplateQuestionMissing {
        /// 卷型 id。
        set_id: String,
        /// 卷面题号。
        ordinal: u32,
    },

    /// 模板题型与题目题型不匹配。
    #[error(
        "模板 {set_id} 题号 {ordinal} 的题型为 {template_kind:?}，与题目题型 {question_kind:?} 不匹配"
    )]
    TemplateKindMismatch {
        /// 卷型 id。
        set_id: String,
        /// 卷面题号。
        ordinal: u32,
        /// 模板题型。
        template_kind: QuestionKind,
        /// 题目实际题型。
        question_kind: QuestionKind,
    },

    /// 模板骨架比例折算不能整除（模板数据错误，不得静默截断）。
    #[error("模板分值折算不能整除：题目满分 {max_milli} × 比例 {ratio_permille}‰；模板数据须修正")]
    TemplateRatioInexact {
        /// 题目满分（千分之一分）。
        max_milli: i64,
        /// 骨架比例（千分比）。
        ratio_permille: i64,
    },

    /// 模板骨架结构不合法（多项定位信息）。
    #[error("模板骨架不合法：{message}")]
    TemplateSkeletonInvalid {
        /// 中文可读原因。
        message: String,
    },

    /// 规则整理（模型整理）输出不合法：严格 Schema 校验失败且机械修复一次后仍不合法。
    #[error(
        "模型整理输出不合法（尝试 {attempts} 次，模型调用 {model_call_id:?}）：{summary}；\n         本题未生成草稿，规则原文与模型原始输出保留供追溯"
    )]
    ModelOutputRejected {
        /// 首次/最后一次模型调用 id（如端口返回）。
        model_call_id: Option<ModelCallId>,
        /// 提示词哈希（复现用）。
        prompt_hash: Sha256Hex,
        /// 结构校验违规摘要（前 3 条，中文可读）。
        summary: String,
        /// 结构校验违规列表（中文可读）。
        violations: Vec<String>,
        /// 尝试次数（1 或 2）。
        attempts: u8,
    },

    /// 规则整理端口失败（inference 不可用/超时/授权拒绝等）。
    #[error("规则整理调用失败：{message}；本题未生成草稿")]
    Structurer {
        /// 底层可读原因。
        message: String,
    },

    /// 领域不变量失败（`SPEC-domain.md` 的错误分类透传）。
    #[error("规则不满足领域不变量：{0}")]
    Domain(#[from] DomainError),

    /// 分数值不合法（`eg_domain::ScoreError` 透传）。
    #[error("分数值不合法：{0}")]
    Score(#[from] eg_domain::ScoreError),

    /// 存储端口失败。
    #[error("规则存储失败：{message}")]
    Store {
        /// 底层错误信息。
        message: String,
    },

    /// 审计端口失败（审计恒开；写失败即拒绝业务写）。
    #[error("规则审计写入失败：{message}；本次操作未生效")]
    Audit {
        /// 底层错误信息。
        message: String,
    },

    /// 序列化失败（草稿/审计哈希计算）。
    #[error("规则序列化失败：{message}")]
    Serialization {
        /// 底层错误信息。
        message: String,
    },

    /// 配置值不合法（粒度、题数上限等）。
    #[error("规则配置不合法：{message}")]
    Config {
        /// 中文可读原因。
        message: String,
    },

    /// 题号与调用方给出的题目上下文不一致（防错配）。
    #[error("规则草稿属于题目 {draft_question}，与操作题目 {requested_question} 不一致")]
    QuestionMismatch {
        /// 草稿所属题目。
        draft_question: QuestionId,
        /// 请求中的题目。
        requested_question: QuestionId,
    },
}

impl RubricError {
    /// 存储错误构造（保留底层错误文本，便于日志定位）。
    pub(crate) fn store(message: impl core::fmt::Display) -> Self {
        Self::Store {
            message: message.to_string(),
        }
    }

    /// 审计错误构造。
    pub(crate) fn audit(message: impl core::fmt::Display) -> Self {
        Self::Audit {
            message: message.to_string(),
        }
    }

    /// 模板数据错误构造。
    pub(crate) fn template_invalid(message: impl core::fmt::Display) -> Self {
        Self::TemplateInvalid {
            message: message.to_string(),
        }
    }

    /// 配置错误构造。
    pub(crate) fn config(message: impl core::fmt::Display) -> Self {
        Self::Config {
            message: message.to_string(),
        }
    }

    /// 规则整理端口错误构造（供 `app` 的 inference 适配器映射超时/不可用/授权拒绝）。
    #[must_use]
    pub fn structurer(message: impl core::fmt::Display) -> Self {
        Self::Structurer {
            message: message.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use eg_domain::Ulid;

    #[test]
    fn errors_have_chinese_hints() {
        let missing = RubricError::DraftNotFound(RubricId::from_ulid(Ulid::from_u128(1)));
        assert!(missing.to_string().contains("草稿不存在"));

        let retired = RubricError::TemplateRetired {
            set_id: "national-jia".to_owned(),
            retired_since: 2026,
        };
        assert!(retired.to_string().contains("已停用"));

        let conflict = RubricError::VersionConflict {
            expected: 3,
            actual: 4,
        };
        assert!(conflict.to_string().contains("版本冲突"));
    }
}
