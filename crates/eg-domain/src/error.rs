//! 领域错误：值对象构造错误与不变量校验错误。
//!
//! 规范依据：`SPEC-domain.md` §11「构造失败一律返回类型化错误，不 panic、不 clamp」、
//! §9（`ScoreError`/`NormalizeError` 取值）、§14（构造错误全部类型化）。
//!
//! 约定：错误文案面向教师可读（中文），并尽量携带字段名；不变量错误附带
//! 可定位的字段或 id，供导入期校验与界面提示使用。

use thiserror::Error;

use crate::ids::RubricItemId;
use crate::paper::QuestionKind;
use crate::rubric::{DraftStatus, ScoringMode};
use crate::score::MilliScore;

/// 分数字符串与分数运算错误（`SPEC-domain.md` §9）。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ScoreError {
    /// 负分：`MilliScore` 不允许为负。
    #[error("分数不能为负：{value} 千分之一分")]
    Negative {
        /// 被拒绝的千分之一分原始值。
        value: i64,
    },
    /// 千分之一分整数运算溢出。
    #[error("分数运算溢出（千分之一分整数超出 i64 范围）")]
    Overflow,
    /// 分数字符串格式非法（允许整数或最多 3 位小数）。
    #[error("分数字符串格式非法：{input:?}；只允许整数或最多 3 位小数，且不接受负号")]
    Format {
        /// 原始输入。
        input: String,
    },
}

/// 置信度构造错误（`SPEC-domain.md` §2：0.0–1.0、非 NaN）。
#[derive(Debug, Clone, PartialEq, Error)]
pub enum ConfidenceError {
    /// 置信度为 NaN。
    #[error("置信度不能是 NaN")]
    NaN,
    /// 置信度越界。
    #[error("置信度必须在 0.0..=1.0 之间，实际为 {value}")]
    OutOfRange {
        /// 被拒绝的原始值。
        value: f64,
    },
}

/// ULID 与 id 文本解析错误（`SPEC-domain.md` §2）。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum IdError {
    /// 不是合法的 26 字符 Crockford Base32 ULID 文本。
    #[error("非法 ULID 文本：{input:?}（{reason}）")]
    InvalidUlid {
        /// 原始输入。
        input: String,
        /// 可读原因。
        reason: String,
    },
}

/// 内容哈希与十六进制指纹解析错误（`SPEC-domain.md` §2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum HashError {
    /// 十六进制文本不合法（长度、字符或大小写）。
    #[error("{kind} 必须是 64 位小写十六进制：{reason}")]
    InvalidHex {
        /// 出错类型名（如 `ContentHash`/`Sha256Hex`）。
        kind: &'static str,
        /// 可读原因。
        reason: &'static str,
    },
}

/// 精确十进制数构造错误（`AnswerValue` 数值与 `Tolerance` 的十进制表示）。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DecimalError {
    /// 文本为空或语法非法。
    #[error("十进制数格式非法：{input:?}")]
    Format {
        /// 原始输入。
        input: String,
    },
    /// 小数位超过上限。
    #[error("十进制数小数位过多（最多 {max_scale} 位）：{input:?}")]
    ScaleTooLarge {
        /// 原始输入。
        input: String,
        /// 允许的最大小数位。
        max_scale: u32,
    },
    /// 尾数超出 i128 范围。
    #[error("十进制数溢出（尾数超出 i128 可精确表示范围）")]
    Overflow,
}

/// 领域不变量校验与聚合构造错误。
///
/// 规范 §11 要求失败必须带字段名与中文可读原因；单字段错误用
/// [`DomainError::InvalidField`] 承载，跨字段不变量使用命名变体。
#[derive(Debug, Clone, PartialEq, Error)]
pub enum DomainError {
    /// 值对象分数字段错误。
    #[error(transparent)]
    Score(#[from] ScoreError),
    /// 置信度字段错误。
    #[error(transparent)]
    Confidence(#[from] ConfidenceError),
    /// id 文本错误。
    #[error(transparent)]
    Id(#[from] IdError),
    /// 哈希文本错误。
    #[error(transparent)]
    Hash(#[from] HashError),
    /// 十进制数错误。
    #[error(transparent)]
    Decimal(#[from] DecimalError),

    /// 单字段非法：携带字段名与可读原因。
    #[error("字段 {field} 非法：{reason}")]
    InvalidField {
        /// 字段名（与规范字段名一致）。
        field: &'static str,
        /// 中文可读原因。
        reason: String,
    },

    /// 试卷总分不等于各题满分之和（§3 不变量 1）。
    #[error("试卷总分不一致：声明 {declared:?}，各题满分之和 {computed:?}")]
    TotalScoreMismatch {
        /// 试卷声明的总分。
        declared: MilliScore,
        /// 各题满分之和。
        computed: MilliScore,
    },

    /// 题目标签在试卷内重复（§3 不变量 2）。
    #[error("题目标签在试卷内重复：{label}")]
    DuplicateQuestionLabel {
        /// 重复的标签。
        label: String,
    },

    /// 题目序号未从 1 连续（§3 不变量 2）。
    #[error("题目序号必须从 1 连续：期望 {expected}，实际 {found}")]
    NonContiguousOrdinal {
        /// 期望序号。
        expected: u32,
        /// 实际序号。
        found: u32,
    },

    /// 评分标准合计不等于评分项分值之和（§4 不变量 1）。
    #[error("评分标准合计不一致：声明 {declared:?}，各评分项之和 {computed:?}")]
    RubricTotalMismatch {
        /// 评分标准声明的合计。
        declared: MilliScore,
        /// 各评分项分值之和。
        computed: MilliScore,
    },

    /// 全对才给分模式评分项数量不为 1（§4 不变量 1）。
    #[error("全对才给分模式只允许 1 个评分项，实际 {count} 个")]
    AllOrNothingItemCount {
        /// 实际评分项数量。
        count: usize,
    },

    /// 评分标准没有任何评分项。
    #[error("评分标准没有任何评分项")]
    EmptyRubric,

    /// 评分项依赖不存在的前置项（§4 不变量 3）。
    #[error("评分项 {item} 依赖不存在的前置项 {missing}")]
    RubricDependencyMissing {
        /// 出错的评分项。
        item: RubricItemId,
        /// 不存在的前置项 id。
        missing: RubricItemId,
    },

    /// 评分项依赖成环（§4 不变量 3）。
    #[error("评分项依赖成环：{items:?}")]
    RubricDependencyCycle {
        /// 环上的评分项 id（首尾相同）。
        items: Vec<RubricItemId>,
    },

    /// 草稿评分项依赖序号超出范围（§5）。
    #[error("草稿评分项依赖序号 {index} 超出范围（允许 1..={len}）")]
    DraftDependencyOutOfRange {
        /// 越界序号。
        index: u32,
        /// 草稿评分项数量。
        len: usize,
    },

    /// 草稿评分项依赖成环（§5）。
    #[error("草稿评分项依赖成环：序号 {items:?}")]
    DraftDependencyCycle {
        /// 环上的草稿内序号（首尾相同）。
        items: Vec<u32>,
    },

    /// 计分方式与题型不兼容（§4 不变量 3、5）。
    #[error("计分方式 {mode:?} 不适用于题型 {kind:?}")]
    ScoringModeUnsupported {
        /// 计分方式。
        mode: ScoringMode,
        /// 题型。
        kind: QuestionKind,
    },

    /// 按选项部分给分模式缺少档位规则（§4 不变量 6）。
    #[error("按选项部分给分必须提供档位规则")]
    PartialChoiceMissing,

    /// 非部分给分模式携带了档位规则（§4 不变量 6）。
    #[error("非部分给分模式禁止携带档位规则")]
    PartialChoiceNotAllowed,

    /// 按选项部分给分只适用于多选题（§4 不变量 5）。
    #[error("按选项部分给分要求题型为多选，实际 {kind:?}")]
    PartialChoiceKindUnsupported {
        /// 实际题型。
        kind: QuestionKind,
    },

    /// 部分给分要求恰好 1 个全对评分项（§4 不变量 5）。
    #[error("按选项部分给分要求恰好 1 个全对评分项，实际 {count} 个")]
    PartialChoiceItemCount {
        /// 实际评分项数量。
        count: usize,
    },

    /// 正确选项数必须为 2 或 3（§4 不变量 5）。
    #[error("正确选项数必须为 2 或 3，实际 {count}")]
    PartialChoiceOptionCount {
        /// 实际正确选项数。
        count: u32,
    },

    /// 档位未覆盖全部命中数（§4 不变量 7）。
    #[error("档位必须覆盖 1..={expected} 的全部命中数，实际为 {actual:?}")]
    PartialChoiceTierCoverage {
        /// 期望覆盖的最大命中数。
        expected: u32,
        /// 实际档位命中数。
        actual: Vec<u32>,
    },

    /// 档位分值越界（§4 不变量 8）。
    #[error("档位 {tier} 分值 {points:?} 必须大于 0 且小于满分 {total:?}")]
    PartialChoiceTierPoints {
        /// 档位命中数。
        tier: u32,
        /// 档位分值。
        points: MilliScore,
        /// 满分。
        total: MilliScore,
    },

    /// 档位分值未随命中数单调不减（§4 不变量 8）。
    #[error("档位分值必须随命中数单调不减")]
    PartialChoiceTierOrder,

    /// 错选归零被设为假（§4 不变量 10）。
    #[error("错选归零恒为真，数据不得为假")]
    PartialChoiceWrongSelectionZero,

    /// 官方口径缺少出处说明（§4 不变量 11）。
    #[error("档位来源为官方口径时必须填写出处说明（source_note）")]
    PartialChoiceSourceNoteMissing,

    /// 档位规则满分与评分标准合计不一致（§4 不变量 9）。
    #[error("档位规则满分 {rule_total:?} 必须等于评分标准合计 {rubric_total:?}")]
    PartialChoiceTotalMismatch {
        /// 档位规则声明满分。
        rule_total: MilliScore,
        /// 评分标准合计。
        rubric_total: MilliScore,
    },

    /// 分值不是评分粒度的整数倍（§4 不变量 8、§8 得分粒度）。
    #[error("分值 {value:?} 不是评分粒度 {granularity:?} 的整数倍")]
    GranularityViolation {
        /// 违规分值。
        value: MilliScore,
        /// 评分粒度。
        granularity: MilliScore,
    },

    /// 判分记录得分超出范围（§8 不变量 1）。
    #[error("判分记录得分 {awarded:?} 超出 [0, {max_score:?}]")]
    AwardedOutOfRange {
        /// 记录得分。
        awarded: MilliScore,
        /// 满分。
        max_score: MilliScore,
    },

    /// 判定与得分不一致（§8 不变量 1）。
    #[error("判定 {judgement} 与得分不一致：得分 {awarded:?}，满分 {max_score:?}")]
    JudgeScoreInconsistent {
        /// 判定标签。
        judgement: &'static str,
        /// 得分。
        awarded: MilliScore,
        /// 满分。
        max_score: MilliScore,
    },

    /// 判分记录没有任何证据（§8 不变量 3）。
    #[error("每条判分记录至少需要 1 条证据")]
    MissingEvidence,

    /// 模型参与但证据缺少模型调用记录（§8 不变量 3）。
    #[error("模型参与判分时证据必须包含模型调用记录（ModelCall）")]
    MissingModelCallEvidence,

    /// 云端调用缺少任务授权 id（§8 不变量 3）。
    #[error("云端模型调用必须携带任务授权 id（consent_id）")]
    CloudConsentMissing,

    /// 标准模式记录被标记抽检（§8 不变量 5）。
    #[error("标准模式记录不允许标记抽检（sampled）")]
    SampledInStandardMode,

    /// 已人工复核但缺少复核时间（§8）。
    #[error("已人工复核状态必须填写复核时间 reviewed_at")]
    ReviewTimestampMissing,

    /// 判分记录 revision 不从 1 起（§8 不变量 4）。
    #[error("判分记录 revision 必须从 1 起，实际 {revision}")]
    InvalidRevision {
        /// 实际 revision。
        revision: u32,
    },

    /// 仅人工路由或不可判定结果不得自动接受（§8 不变量 2）。
    #[error("仅人工路由或不可判定结果不得自动接受")]
    AutoAcceptNotAllowed,

    /// 模板版本与答案键版本不一致，禁止自动对齐（§3 不变量 3）。
    #[error("模板版本 {template_version} 与答案键版本 {key_version} 不一致，禁止自动对齐")]
    TemplateVersionMismatch {
        /// 模板版本。
        template_version: u32,
        /// 试卷答案键版本。
        key_version: u32,
    },

    /// 模板所属试卷与目标试卷不一致。
    #[error("模板所属试卷与目标试卷不一致")]
    TemplatePaperMismatch,

    /// 草稿状态不允许确认（§5）。
    #[error("草稿状态 {status:?} 不允许确认")]
    DraftNotConfirmable {
        /// 当前状态。
        status: DraftStatus,
    },

    /// 草稿存在错误级校验问题，禁止确认（§5）。
    #[error("草稿存在 {count} 个错误级问题，禁止确认")]
    DraftHasErrors {
        /// 错误级问题数量。
        count: usize,
    },

    /// 模型整理草稿缺少模型调用 id（§5）。
    #[error("草稿来源为模型整理时必须记录模型调用 id")]
    MissingModelCallId,

    /// 已确认草稿缺少确认时间（§5）。
    #[error("已确认草稿必须填写确认时间 confirmed_at")]
    MissingConfirmationTimestamp,

    /// 未确认状态的草稿填写了确认时间（§5）。
    #[error("未确认状态的草稿不得填写确认时间 confirmed_at")]
    UnexpectedConfirmationTimestamp,

    /// 「仅评分标准」答案键缺少生效评分标准（§4 不变量 2）。
    #[error("答案键为「仅评分标准」时必须提供生效评分标准")]
    MissingRubricForAnswerKey,

    /// 存在错误级规则问题，禁止开始判分（§9 契约）。
    #[error("存在 {count} 个错误级规则问题，禁止开始判分")]
    RubricHasErrors {
        /// 错误级问题数量。
        count: usize,
    },
}

impl DomainError {
    /// 字段名（供导入期报告与界面定位）。
    #[must_use]
    pub const fn field_name(&self) -> &'static str {
        match self {
            Self::InvalidField { field, .. } => field,
            Self::Score(_) => "score",
            Self::Confidence(_) => "confidence",
            Self::Id(_) => "id",
            Self::Hash(_) => "hash",
            Self::Decimal(_) => "decimal",
            Self::TotalScoreMismatch { .. } => "total_score",
            Self::DuplicateQuestionLabel { .. } => "label",
            Self::NonContiguousOrdinal { .. } => "ordinal",
            Self::RubricTotalMismatch { .. } => "rubric.total",
            Self::AllOrNothingItemCount { .. }
            | Self::PartialChoiceItemCount { .. }
            | Self::EmptyRubric => "items",
            Self::RubricDependencyMissing { .. }
            | Self::RubricDependencyCycle { .. }
            | Self::DraftDependencyOutOfRange { .. }
            | Self::DraftDependencyCycle { .. } => "items.depends_on",
            Self::ScoringModeUnsupported { .. } => "mode",
            Self::PartialChoiceMissing
            | Self::PartialChoiceNotAllowed
            | Self::PartialChoiceOptionCount { .. }
            | Self::PartialChoiceTierCoverage { .. }
            | Self::PartialChoiceTierPoints { .. }
            | Self::PartialChoiceTierOrder
            | Self::PartialChoiceWrongSelectionZero
            | Self::PartialChoiceSourceNoteMissing
            | Self::PartialChoiceTotalMismatch { .. }
            | Self::PartialChoiceKindUnsupported { .. } => "partial_choice",
            Self::GranularityViolation { .. } => "granularity",
            Self::AwardedOutOfRange { .. } => "awarded",
            Self::JudgeScoreInconsistent { .. } => "judgement",
            Self::MissingEvidence | Self::MissingModelCallEvidence => "evidence",
            Self::CloudConsentMissing => "consent_id",
            Self::SampledInStandardMode => "sampled",
            Self::ReviewTimestampMissing => "reviewed_at",
            Self::InvalidRevision { .. } => "revision",
            Self::AutoAcceptNotAllowed | Self::DraftNotConfirmable { .. } => "status",
            Self::TemplateVersionMismatch { .. } => "version",
            Self::TemplatePaperMismatch => "paper_id",
            Self::DraftHasErrors { .. } | Self::RubricHasErrors { .. } => "validation",
            Self::MissingModelCallId => "model_call_id",
            Self::MissingConfirmationTimestamp | Self::UnexpectedConfirmationTimestamp => {
                "confirmed_at"
            }
            Self::MissingRubricForAnswerKey => "rubric",
        }
    }
}

/// 构造单字段错误（内部便捷函数；字段名与规范保持逐字一致）。
pub(crate) fn invalid_field(field: &'static str, reason: impl Into<String>) -> DomainError {
    DomainError::InvalidField {
        field,
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn score_errors_are_readable() {
        assert_eq!(
            ScoreError::Negative { value: -1 }.to_string(),
            "分数不能为负：-1 千分之一分"
        );
        assert!(ScoreError::Overflow.to_string().contains("溢出"));
        assert!(
            ScoreError::Format {
                input: "x".to_owned()
            }
            .to_string()
            .contains("格式非法")
        );
    }

    #[test]
    fn confidence_errors_are_readable() {
        assert_eq!(ConfidenceError::NaN.to_string(), "置信度不能是 NaN");
        assert!(
            ConfidenceError::OutOfRange { value: 1.5 }
                .to_string()
                .contains("0.0..=1.0")
        );
    }

    #[test]
    fn invalid_field_keeps_field_name() {
        let err = invalid_field("total_score", "必须等于各题满分之和");
        assert_eq!(err.field_name(), "total_score");
        assert!(err.to_string().contains("total_score"));
        assert!(err.to_string().contains("必须等于各题满分之和"));
    }

    #[test]
    fn invariant_errors_expose_field_names() {
        assert_eq!(DomainError::MissingEvidence.field_name(), "evidence");
        assert_eq!(DomainError::CloudConsentMissing.field_name(), "consent_id");
        assert_eq!(
            DomainError::InvalidRevision { revision: 0 }.field_name(),
            "revision"
        );
    }

    #[test]
    fn score_error_converts_into_domain_error() {
        let err: DomainError = ScoreError::Overflow.into();
        assert_eq!(err.field_name(), "score");
        assert!(err.to_string().contains("溢出"));
    }
}
