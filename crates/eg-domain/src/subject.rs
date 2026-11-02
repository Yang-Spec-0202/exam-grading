//! 学科插件契约（`SPEC-domain.md` §9）。
//!
//! 契约测试要求（所有学科实现必须通过）：比较器链顺序与终止规则；归一化幂等；
//! 解析失败不得判为「不相等」；静态校验必须在导入期调用；有错误级问题时禁止开始判分。
//! 可复用契约套件见 [`crate::contract`]。
//!
//! 本模块只定义**与学科无关**的类型与 trait；`math` 等具体实现在各自 crate。

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::answer::{AnswerValue, ConcreteKey};
use crate::confidence::Confidence;
use crate::error::{DomainError, invalid_field};
use crate::geometry::BBox;
use crate::ids::{PageId, QuestionId, RecognitionId, RubricItemId, SegmentId, SubmissionId};
use crate::paper::{Paper, Question, QuestionKind};
use crate::recognition::Block;
use crate::rubric::Rubric;
use crate::values::{ComparatorId, SubjectId};

/// 原始答案来源（§9 `AnswerSource`：识别/人工修正/电子文本）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerSource {
    /// 识别产出。
    Recognition,
    /// 人工修正（教师转录）。
    ManualCorrection,
    /// 电子文本（电子文档导入）。
    ElectronicText,
}

/// 原始答案（§9 `RawAnswer`）：进入归一化前的不可信输入。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawAnswer {
    /// 文本。
    pub text: String,
    /// LaTeX（可空）。
    pub latex: Option<String>,
    /// 识别块（按阅读顺序）。
    pub blocks: Vec<Block>,
    /// 来源。
    pub source: AnswerSource,
}

/// 页面证据片段（§9 `AnswerPart`）：跨页题每页一段。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnswerPart {
    /// 页面 id。
    pub page_id: PageId,
    /// 答案区域 id。
    pub segment_id: SegmentId,
    /// 识别结果 id。
    pub recognition_id: RecognitionId,
    /// 该页证据坐标。
    pub bbox: BBox,
    /// 该页文本。
    pub text: String,
}

impl AnswerPart {
    /// 校验证据坐标。
    ///
    /// # Errors
    ///
    /// 坐标非法时返回 [`DomainError`]。
    pub fn validate(&self) -> Result<(), DomainError> {
        self.bbox.validate()
    }
}

/// 对齐后的学生答案（§9 `StudentAnswer`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StudentAnswer {
    /// 提交 id。
    pub submission_id: SubmissionId,
    /// 题目 id。
    pub question_id: QuestionId,
    /// 主段识别结果 id（可空）。
    pub recognition_id: Option<RecognitionId>,
    /// 主段答案区域 id（可空）。
    pub segment_id: Option<SegmentId>,
    /// 已合并的原始答案。
    pub raw: RawAnswer,
    /// 每页一段证据。
    pub parts: Vec<AnswerPart>,
    /// 各段识别置信度的最小值（人工确认值为 1.0，界面必须与模型值区分）。
    pub recognition_confidence: Confidence,
}

impl StudentAnswer {
    /// 校验学生答案（片段坐标合法；主段 id 与片段一致性）。
    ///
    /// # Errors
    ///
    /// 失败返回 [`DomainError`]。
    pub fn validate(&self) -> Result<(), DomainError> {
        for part in &self.parts {
            part.validate()?;
        }
        if self.parts.is_empty() {
            return Err(invalid_field("parts", "学生答案至少需要 1 个页面证据片段"));
        }
        if let Some(segment_id) = self.segment_id
            && !self.parts.iter().any(|part| part.segment_id == segment_id)
        {
            return Err(invalid_field("segment_id", "主段 id 必须出现在 parts 中"));
        }
        Ok(())
    }

    /// 由各段识别置信度求最小值（§9「recognition_confidence（各段最小值）」）。
    #[must_use]
    pub fn min_confidence(values: impl IntoIterator<Item = Confidence>) -> Option<Confidence> {
        let mut iter = values.into_iter();
        let first = iter.next()?;
        Some(iter.fold(first, Confidence::min))
    }
}

/// 归一化步骤（§9 `NormalizedStep`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NormalizedStep {
    /// 步骤序号（1 起）。
    pub ordinal: u32,
    /// 步骤文本。
    pub text: String,
    /// LaTeX（可空）。
    pub latex: Option<String>,
}

/// 归一化答案（§9 `NormalizedAnswer`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NormalizedAnswer {
    /// 规范化文本。
    pub text: String,
    /// 规范化 LaTeX（可空）。
    pub latex: Option<String>,
    /// 结构化值（可空；由学科包解释）。
    pub value: Option<AnswerValue>,
    /// 步骤列表。
    pub steps: Vec<NormalizedStep>,
}

impl NormalizedAnswer {
    /// 校验步骤序号从 1 起连续（步骤列表由归一化产出，序号必须可定位）。
    ///
    /// # Errors
    ///
    /// 序号不连续时返回 [`DomainError`]。
    pub fn validate(&self) -> Result<(), DomainError> {
        for (index, step) in self.steps.iter().enumerate() {
            let expected = u32::try_from(index + 1)
                .map_err(|_| invalid_field("steps", "步骤数超出 u32 序号上限"))?;
            if step.ordinal != expected {
                return Err(invalid_field(
                    "steps.ordinal",
                    format!(
                        "步骤序号必须从 1 连续，期望 {expected}，实际 {}",
                        step.ordinal
                    ),
                ));
            }
        }
        Ok(())
    }
}

/// 步骤证据（§9 `StepEvidence`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepEvidence {
    /// 步骤匹配列表。
    pub steps: Vec<StepMatch>,
}

/// 步骤匹配（§9 `StepMatch`）：`matched` 只表示「找到可定位步骤」，不等于满足评分点。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepMatch {
    /// 对应评分项。
    pub rubric_item: RubricItemId,
    /// 是否在答案中找到可定位步骤（不等于满足）。
    pub matched: bool,
    /// 证据坐标（可空）。
    pub evidence: Option<BBox>,
    /// 备注（可空）。
    pub note: Option<String>,
}

/// 学科能力声明（§9 `SubjectCapabilities`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubjectCapabilities {
    /// 支持题型。
    pub kinds: Vec<QuestionKind>,
    /// 比较器列表（声明必须与实际实现一致，禁止虚报）。
    pub comparators: Vec<ComparatorId>,
    /// 提示词版本。
    pub prompt_version: String,
    /// 是否支持分步给分。
    pub supports_step_scoring: bool,
}

impl SubjectCapabilities {
    /// 校验能力声明（题型/比较器非空且不重复、提示词版本非空）。
    ///
    /// # Errors
    ///
    /// 失败返回 [`DomainError`]。
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.kinds.is_empty() {
            return Err(invalid_field("capabilities.kinds", "必须声明支持题型"));
        }
        if self.comparators.is_empty() {
            return Err(invalid_field(
                "capabilities.comparators",
                "必须声明比较器列表",
            ));
        }
        if self.prompt_version.trim().is_empty() {
            return Err(invalid_field(
                "capabilities.prompt_version",
                "提示词版本不得为空",
            ));
        }
        let mut seen_kinds: Vec<QuestionKind> = Vec::with_capacity(self.kinds.len());
        for kind in &self.kinds {
            if seen_kinds.contains(kind) {
                return Err(invalid_field("capabilities.kinds", "题型声明重复"));
            }
            seen_kinds.push(*kind);
        }
        let mut seen_comparators: Vec<&ComparatorId> = Vec::with_capacity(self.comparators.len());
        for comparator in &self.comparators {
            if seen_comparators.contains(&comparator) {
                return Err(invalid_field("capabilities.comparators", "比较器声明重复"));
            }
            seen_comparators.push(comparator);
        }
        Ok(())
    }

    /// 是否声明支持某题型。
    #[must_use]
    pub fn supports_kind(&self, kind: QuestionKind) -> bool {
        self.kinds.contains(&kind)
    }
}

/// 规则问题严重级（§9 `RubricIssueSeverity`：警告/错误）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RubricIssueSeverity {
    /// 警告。
    Warning,
    /// 错误（存在时禁止开始判分）。
    Error,
}

/// 规则问题定位（§9 `RubricIssue.item`）。
///
/// 规范未给出 `item` 的表示：草稿阶段用「草稿内序号」（`SPEC-rubric.md` §2.4），
/// 生效规则阶段用评分项 id。为同时满足两种上下文且不丢信息，用带标签枚举承载。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RubricItemRef {
    /// 生效规则中的评分项 id。
    Item(RubricItemId),
    /// 草稿内评分项序号（1 起）。
    Ordinal(u32),
}

/// 规则问题（§9 `RubricIssue`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RubricIssue {
    /// 严重级。
    pub severity: RubricIssueSeverity,
    /// 评分项定位（可空）。
    pub item: Option<RubricItemRef>,
    /// 中文可读消息。
    pub message: String,
}

impl RubricIssue {
    /// 构造警告。
    #[must_use]
    pub fn warning(message: impl Into<String>) -> Self {
        Self {
            severity: RubricIssueSeverity::Warning,
            item: None,
            message: message.into(),
        }
    }

    /// 构造错误。
    #[must_use]
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            severity: RubricIssueSeverity::Error,
            item: None,
            message: message.into(),
        }
    }

    /// 是否为错误级。
    #[must_use]
    pub fn is_error(&self) -> bool {
        self.severity == RubricIssueSeverity::Error
    }

    /// 消息是否非空（校验用）。
    #[must_use]
    pub fn is_message_valid(&self) -> bool {
        !self.message.trim().is_empty()
    }
}

/// 错误级问题数量（导入期门禁依据）。
#[must_use]
pub fn error_issue_count(issues: &[RubricIssue]) -> usize {
    issues.iter().filter(|issue| issue.is_error()).count()
}

/// 导入期门禁：存在错误级问题时禁止开始判分（§9 契约测试）。
///
/// # Errors
///
/// 存在错误级问题时返回 [`DomainError::RubricHasErrors`]。
pub fn ensure_no_error_issues(issues: &[RubricIssue]) -> Result<(), DomainError> {
    let count = error_issue_count(issues);
    if count > 0 {
        return Err(DomainError::RubricHasErrors { count });
    }
    Ok(())
}

/// 判定上下文（§9 `JudgeContext`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JudgeContext {
    /// 题目（含答案键与评分标准）。
    pub question: Question,
    /// 试卷（含版本号）。
    pub paper: Paper,
}

/// 归一化错误（§9 `NormalizeError`）。
#[derive(Debug, Clone, PartialEq, Eq, Error, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NormalizeError {
    /// 无法解析（附输入与原因）。
    #[error("无法解析：{input:?}（{reason}）")]
    Unparsable {
        /// 原始输入。
        input: String,
        /// 可读原因。
        reason: String,
    },
    /// 不支持（附原因）。
    #[error("不支持的写法：{reason}")]
    Unsupported {
        /// 可读原因。
        reason: String,
    },
}

/// 比较器判定（§9 `ComparatorVerdict`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparatorVerdict {
    /// 相等。
    Equal,
    /// 等价（附说明）。
    Equivalent {
        /// 说明（如恒等变形）。
        note: String,
    },
    /// 不相等（附原因）。
    NotEqual {
        /// 可读原因（不得用于「解析失败」，规范 §9）。
        reason: String,
    },
    /// 无法判定（附原因）。
    Undecidable {
        /// 可读原因。
        reason: String,
    },
}

impl ComparatorVerdict {
    /// 是否为确定结论（相等/等价/不相等）：链式规则中首个确定结论终止。
    #[must_use]
    pub const fn is_conclusive(&self) -> bool {
        !matches!(self, Self::Undecidable { .. })
    }

    /// 是否判为相等或等价。
    #[must_use]
    pub const fn is_equal_like(&self) -> bool {
        matches!(self, Self::Equal | Self::Equivalent { .. })
    }

    /// 是否判为不相等。
    #[must_use]
    pub const fn is_not_equal(&self) -> bool {
        matches!(self, Self::NotEqual { .. })
    }

    /// 是否无法判定。
    #[must_use]
    pub const fn is_undecidable(&self) -> bool {
        matches!(self, Self::Undecidable { .. })
    }

    /// 原因/说明文本（相等时为空）。
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Equal => None,
            Self::Equivalent { note } => Some(note),
            Self::NotEqual { reason } | Self::Undecidable { reason } => Some(reason),
        }
    }
}

/// 比较器（§9 `Comparator`）：必须提供比较器 id 与比较能力。
///
/// 实现必须是纯函数（无 IO、无网络、无时钟），拿不准必须返回
/// [`ComparatorVerdict::Undecidable`]，不得猜测。
pub trait Comparator: Send + Sync {
    /// 比较器 id。
    fn id(&self) -> ComparatorId;

    /// 比较期望值与归一化答案。
    fn compare(&self, expected: &AnswerValue, answer: &NormalizedAnswer) -> ComparatorVerdict;
}

/// 学科包（§9 `SubjectPack`）：与学科无关的插件契约。
///
/// 实现要求见 `SPEC-subject-math.md` §2；所有实现必须通过 [`crate::contract`] 的套件。
pub trait SubjectPack: Send + Sync {
    /// 学科 id。
    fn id(&self) -> SubjectId;

    /// 显示名（教师可见，中文优先）。
    fn display_name(&self) -> String;

    /// 能力声明（必须与实际实现一致，禁止虚报）。
    fn capabilities(&self) -> SubjectCapabilities;

    /// 归一化：输入原始答案，输出可比较形式；失败必须显式报错。
    ///
    /// # Errors
    ///
    /// 解析失败返回 [`NormalizeError::Unparsable`]；超范围返回
    /// [`NormalizeError::Unsupported`]。
    fn normalize(&self, raw: &RawAnswer) -> Result<NormalizedAnswer, NormalizeError>;

    /// 比较器链：给定具体答案键与题型，返回按序尝试的比较器。
    fn comparator_chain(&self, key: &ConcreteKey, kind: QuestionKind) -> Vec<Box<dyn Comparator>>;

    /// 分步证据抽取：给定评分标准与归一化答案，输出步骤证据或空（只抽证据，不给分）。
    fn step_evidence(&self, rubric: &Rubric, answer: &NormalizedAnswer) -> Option<StepEvidence>;

    /// 判定配置：返回判定 profile id（提示词版本与配置一致）。
    fn judge_profile_id(&self) -> String;

    /// 静态校验：给定题目，返回规则问题列表（导入期调用）。
    fn validate_question(&self, question: &Question) -> Vec<RubricIssue>;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::ids::Ulid;

    #[test]
    fn verdict_classification() {
        assert!(ComparatorVerdict::Equal.is_conclusive());
        assert!(ComparatorVerdict::Equal.is_equal_like());
        assert!(
            ComparatorVerdict::Equivalent {
                note: "恒等".to_owned()
            }
            .is_equal_like()
        );
        assert!(
            ComparatorVerdict::NotEqual {
                reason: "数值不同".to_owned()
            }
            .is_not_equal()
        );
        assert!(
            !ComparatorVerdict::Undecidable {
                reason: "超出范围".to_owned()
            }
            .is_conclusive()
        );
        assert!(
            ComparatorVerdict::Undecidable {
                reason: "超出范围".to_owned()
            }
            .is_undecidable()
        );
        assert_eq!(ComparatorVerdict::Equal.reason(), None);
        assert_eq!(
            ComparatorVerdict::NotEqual {
                reason: "不同".to_owned()
            }
            .reason(),
            Some("不同")
        );
    }

    #[test]
    fn capabilities_validation() {
        let capabilities = SubjectCapabilities {
            kinds: vec![QuestionKind::NumericOnly, QuestionKind::MultipleChoice],
            comparators: vec![ComparatorId::new("num_abs").unwrap()],
            prompt_version: "math-judge-v1".to_owned(),
            supports_step_scoring: true,
        };
        assert!(capabilities.validate().is_ok());
        assert!(capabilities.supports_kind(QuestionKind::NumericOnly));
        assert!(!capabilities.supports_kind(QuestionKind::Proof));

        let empty = SubjectCapabilities {
            kinds: Vec::new(),
            ..capabilities.clone()
        };
        assert!(empty.validate().is_err());
        let duplicate = SubjectCapabilities {
            kinds: vec![QuestionKind::NumericOnly, QuestionKind::NumericOnly],
            ..capabilities
        };
        assert!(duplicate.validate().is_err());
    }

    #[test]
    fn issue_helpers_and_gate() {
        let issues = [
            RubricIssue::warning("判据重复"),
            RubricIssue::error("分值合计不等于题目满分"),
        ];
        assert_eq!(error_issue_count(&issues), 1);
        assert!(ensure_no_error_issues(&issues).is_err());
        assert!(ensure_no_error_issues(&[RubricIssue::warning("提示")]).is_ok());
        assert!(!RubricIssue::error(" ").is_message_valid());
    }

    #[test]
    fn normalized_answer_steps_must_be_contiguous() {
        let answer = NormalizedAnswer {
            text: "x=1".to_owned(),
            latex: Some("x=1".to_owned()),
            value: None,
            steps: vec![NormalizedStep {
                ordinal: 1,
                text: "移项".to_owned(),
                latex: None,
            }],
        };
        assert!(answer.validate().is_ok());
        let broken = NormalizedAnswer {
            steps: vec![NormalizedStep {
                ordinal: 2,
                text: "移项".to_owned(),
                latex: None,
            }],
            ..answer
        };
        assert!(broken.validate().is_err());
    }

    #[test]
    fn student_answer_validation_and_min_confidence() {
        let part = AnswerPart {
            page_id: PageId::from_ulid(Ulid::from_u128(1)),
            segment_id: SegmentId::from_ulid(Ulid::from_u128(2)),
            recognition_id: RecognitionId::from_ulid(Ulid::from_u128(3)),
            bbox: BBox::new(0.1, 0.1, 0.5, 0.5).unwrap(),
            text: "x=1".to_owned(),
        };
        let answer = StudentAnswer {
            submission_id: SubmissionId::from_ulid(Ulid::from_u128(4)),
            question_id: QuestionId::from_ulid(Ulid::from_u128(5)),
            recognition_id: Some(part.recognition_id),
            segment_id: Some(part.segment_id),
            raw: RawAnswer {
                text: "x=1".to_owned(),
                latex: None,
                blocks: Vec::new(),
                source: AnswerSource::Recognition,
            },
            parts: vec![part],
            recognition_confidence: Confidence::new(0.8).unwrap(),
        };
        assert!(answer.validate().is_ok());

        let empty = StudentAnswer {
            parts: Vec::new(),
            ..answer.clone()
        };
        assert!(empty.validate().is_err());

        let min = StudentAnswer::min_confidence([
            Confidence::new(0.9).unwrap(),
            Confidence::new(0.4).unwrap(),
            Confidence::new(0.7).unwrap(),
        ])
        .unwrap();
        assert!((min.get() - 0.4).abs() < f64::EPSILON);
        assert_eq!(StudentAnswer::min_confidence([]), None);
    }

    #[test]
    fn normalize_error_is_typed() {
        let err = NormalizeError::Unparsable {
            input: "x^".to_owned(),
            reason: "括号不配对".to_owned(),
        };
        assert!(err.to_string().contains("括号不配对"));
        let unsupported = NormalizeError::Unsupported {
            reason: "微积分超出范围".to_owned(),
        };
        assert!(unsupported.to_string().contains("不支持"));
    }

    #[test]
    fn issue_item_reference_is_tagged() {
        let item = RubricIssue {
            severity: RubricIssueSeverity::Error,
            item: Some(RubricItemRef::Ordinal(2)),
            message: "依赖不存在".to_owned(),
        };
        let json = serde_json::to_string(&item).unwrap();
        assert!(json.contains("\"ordinal\""));
        let id_item = RubricIssue {
            item: Some(RubricItemRef::Item(RubricItemId::from_ulid(
                Ulid::from_u128(7),
            ))),
            ..item
        };
        let json = serde_json::to_string(&id_item).unwrap();
        assert!(json.contains("\"item\""));
    }
}
