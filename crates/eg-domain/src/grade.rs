//! 判分结果、运行模式、路由轨迹与复核分类（`SPEC-domain.md` §8）。
//!
//! 不变量（§8）：
//! 1. `awarded` 非负且 ≤ `max_score`；`match` 必须满分，`mismatch` 必须 0，
//!    `partial` 必须在开区间内；
//! 2. 自动接受的条件由 `grade` 的门禁执行（本模块提供结构可判定部分
//!    [`GradeRecord::auto_accept_blockers`]）；
//! 3. 每条判分记录至少 1 条证据；模型参与时必须含 `ModelCall`；云端调用必须含
//!    `consent_id`（`ModelCall` 层校验）；
//! 4. 判分记录只追加新版本，不原地修改（`revision` 从 1 起）；
//! 5. 快速模式记录必须 `sampled` 可辨。

use serde::{Deserialize, Serialize};

use crate::confidence::Confidence;
use crate::error::{DomainError, invalid_field};
use crate::evidence::{EvidenceRef, ModelCall};
use crate::hash::Sha256Hex;
use crate::ids::{GradeId, ModelCallId, QuestionId, RubricItemId, SubmissionId};
use crate::recognition::EngineStamp;
use crate::score::MilliScore;
use crate::time::Timestamp;
use crate::values::CalibrationId;

/// L1 路由结果（§8 `RouteDecision`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteDecision {
    /// 仅规则。
    RuleOnly,
    /// 规则优先再升级模型。
    RuleThenModel,
    /// 直接模型判定。
    ModelDirect,
    /// 按评分标准逐项判定。
    RubricItemized,
    /// 仅人工（附原因）。
    Manual(ManualReason),
}

impl RouteDecision {
    /// 是否仅人工。
    #[must_use]
    pub const fn is_manual(&self) -> bool {
        matches!(self, Self::Manual(_))
    }
}

/// 转人工原因（§8 `ManualReason`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManualReason {
    /// 答案键缺失。
    AnswerKeyMissing,
    /// 答案键省略。
    AnswerKeyOmitted,
    /// 规则缺失。
    RubricMissing,
    /// 识别不可用。
    RecognitionUnavailable,
    /// 置信度不足。
    LowConfidence,
    /// 超出范围。
    OutOfScope,
    /// 冲突未解决。
    ConflictUnresolved,
    /// 后端不可用。
    BackendUnavailable,
}

/// 不可判定原因（§8 `UngradableReason`，含 §15.1 的 `PolicyBlocked`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UngradableReason {
    /// 未作答。
    NotAnswered,
    /// 识别不可用。
    RecognitionUnavailable,
    /// 答案键不完整。
    AnswerKeyIncomplete,
    /// 超出范围。
    OutOfScope,
    /// 后端不可用。
    BackendUnavailable,
    /// 超时。
    Timeout,
    /// 预算超限。
    BudgetExceeded,
    /// Schema 不合法。
    InvalidSchema,
    /// 冲突。
    Conflict,
    /// 策略拦截。
    PolicyBlocked,
}

impl UngradableReason {
    /// 映射为复核队列原因分类（§8 `ReviewReasonKind`）。
    #[must_use]
    pub const fn review_reason(self) -> ReviewReasonKind {
        match self {
            Self::Conflict => ReviewReasonKind::Conflict,
            Self::BackendUnavailable => ReviewReasonKind::BackendUnavailable,
            _ => ReviewReasonKind::Ungradable,
        }
    }
}

/// 判定中的满足/缺失单元（§8 `Judgement::Partial` 的 `satisfied`/`missing`）。
///
/// 按评分标准判定时是评分项 id；按选项部分给分时是选项标签（规范 v0.2.2 §8）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JudgedUnit {
    /// 评分项。
    RubricItem(RubricItemId),
    /// 选项（如 `"A"`）。
    Choice(String),
}

/// L2 判定结果（§8 `Judgement`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Judgement {
    /// 一致（得分 + 理由 + 证据）。
    Match {
        /// 得分（必须等于满分）。
        score: MilliScore,
        /// 中文理由。
        reason: String,
        /// 证据引用。
        evidence: Vec<EvidenceRef>,
    },
    /// 部分一致（得分 + 满足项 + 缺失项 + 理由）。
    Partial {
        /// 得分（必须在 (0, 满分) 开区间内）。
        score: MilliScore,
        /// 满足项（评分项或命中选项）。
        satisfied: Vec<JudgedUnit>,
        /// 缺失项（评分项或未选中选项）。
        missing: Vec<JudgedUnit>,
        /// 中文理由。
        reason: String,
    },
    /// 不一致（得分 + 理由 + 证据）。
    Mismatch {
        /// 得分（必须为 0）。
        score: MilliScore,
        /// 中文理由。
        reason: String,
        /// 证据引用。
        evidence: Vec<EvidenceRef>,
    },
    /// 不可判定（原因）。
    Ungradable {
        /// 不可判定原因。
        reason: UngradableReason,
    },
}

impl Judgement {
    /// 判定标签（审计与日志用）。
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Match { .. } => "match",
            Self::Partial { .. } => "partial",
            Self::Mismatch { .. } => "mismatch",
            Self::Ungradable { .. } => "ungradable",
        }
    }

    /// 判定携带的得分（不可判定时为空）。
    #[must_use]
    pub const fn score(&self) -> Option<MilliScore> {
        match self {
            Self::Match { score, .. }
            | Self::Partial { score, .. }
            | Self::Mismatch { score, .. } => Some(*score),
            Self::Ungradable { .. } => None,
        }
    }

    /// 校验判定与满分的得分一致性（§8 不变量 1）。
    ///
    /// # Errors
    ///
    /// `match` 未满分、`mismatch` 非 0、`partial` 不在开区间、理由为空时返回
    /// [`DomainError`]。
    pub fn validate_against(&self, max_score: MilliScore) -> Result<(), DomainError> {
        match self {
            Self::Match { score, reason, .. } => {
                if *score != max_score {
                    return Err(DomainError::JudgeScoreInconsistent {
                        judgement: "match",
                        awarded: *score,
                        max_score,
                    });
                }
                require_reason(reason)
            }
            Self::Mismatch { score, reason, .. } => {
                if !score.is_zero() {
                    return Err(DomainError::JudgeScoreInconsistent {
                        judgement: "mismatch",
                        awarded: *score,
                        max_score,
                    });
                }
                require_reason(reason)
            }
            Self::Partial { score, reason, .. } => {
                require_reason(reason)?;
                if score.is_zero() || *score >= max_score {
                    return Err(DomainError::JudgeScoreInconsistent {
                        judgement: "partial",
                        awarded: *score,
                        max_score,
                    });
                }
                Ok(())
            }
            Self::Ungradable { .. } => Ok(()),
        }
    }
}

fn require_reason(reason: &str) -> Result<(), DomainError> {
    if reason.trim().is_empty() {
        return Err(invalid_field("judgement.reason", "判定理由不得为空"));
    }
    Ok(())
}

/// 模型判定产出（§8 `JudgeOutcome`）：含校准前置信度与校准 id。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JudgeOutcome {
    /// 判定。
    pub judgement: Judgement,
    /// 模型原始概率（校准前）。
    pub confidence: Confidence,
    /// 命中的候选序号（可空）。
    pub matched_alternative: Option<u32>,
    /// 校准记录 id；为空禁止自动放行。
    pub calibration_id: Option<CalibrationId>,
    /// 校准后置信度（表匹配时由 inference 填充；`None` = 未套用校准）。
    #[serde(default)]
    pub calibrated_confidence: Option<Confidence>,
    /// 是否已由可信校准表验证（推断层设置；伪造 id 不置位）。
    #[serde(default)]
    pub calibration_verified: bool,
}

impl JudgeOutcome {
    /// 是否已校准（为空时禁止自动放行，§8）。
    #[must_use]
    pub fn is_calibrated(&self) -> bool {
        self.calibration_id.is_some()
    }

    /// 是否允许进入自动放行候选（已校准且非不可判定）。
    #[must_use]
    pub fn permits_auto_accept(&self) -> bool {
        self.is_calibrated() && !matches!(self.judgement, Judgement::Ungradable { .. })
    }

    /// 是否已套用可信校准（id + 校准置信度 + 验证标志三者齐全）。
    #[must_use]
    pub fn is_calibration_applied(&self) -> bool {
        self.is_calibrated() && self.calibration_verified && self.calibrated_confidence.is_some()
    }

    /// 校验判定与满分一致性。
    ///
    /// # Errors
    ///
    /// 判定与得分不一致时返回 [`DomainError`]。
    pub fn validate(&self, max_score: MilliScore) -> Result<(), DomainError> {
        self.judgement.validate_against(max_score)
    }
}

/// 运行模式（§8 `GradeMode`：标准/快速）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GradeMode {
    /// 标准模式（默认，高门槛、可追溯）。
    Standard,
    /// 快速模式（低门槛、抽检、结果标注）。
    Fast,
}

/// L1 路由轨迹（§8 `RouteTrace`）：随判分记录持久化。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteTrace {
    /// 命中的规则编号（如 `R3`）。
    pub rule_id: String,
    /// 路由输入哈希。
    pub inputs_hash: Sha256Hex,
    /// 分类澄清调用（可空，仅分类澄清时有）。
    pub classifier_call: Option<ModelCallId>,
    /// 决策时间。
    pub decided_at: Timestamp,
}

impl RouteTrace {
    /// 校验路由轨迹（规则编号非空）。
    ///
    /// # Errors
    ///
    /// 失败返回 [`DomainError`]。
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.rule_id.trim().is_empty() {
            return Err(invalid_field("route_trace.rule_id", "规则编号不得为空"));
        }
        Ok(())
    }
}

/// 判分状态（§8 `GradeStatus`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GradeStatus {
    /// 自动接受。
    AutoAccepted,
    /// 待复核。
    NeedsReview,
    /// 已人工复核。
    ManuallyReviewed,
    /// 排除。
    Excluded,
}

/// 复核原因分类（§8 `ReviewReasonKind`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewReasonKind {
    /// 置信度不足。
    LowConfidence,
    /// 冲突。
    Conflict,
    /// 不可判定。
    Ungradable,
    /// 证据缺失。
    MissingEvidence,
    /// 未校准模型。
    UncalibratedModel,
    /// 后端不可用。
    BackendUnavailable,
    /// 分数非法。
    InvalidScore,
}

/// 判分记录（§8 `GradeRecord`）：只追加新版本，不原地修改。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GradeRecord {
    /// 判分记录 id。
    pub id: GradeId,
    /// 提交 id。
    pub submission_id: SubmissionId,
    /// 题目 id。
    pub question_id: QuestionId,
    /// 追加版本，从 1 起。
    pub revision: u32,
    /// 运行模式。
    pub mode: GradeMode,
    /// L1 路由结果。
    pub route: RouteDecision,
    /// L1 路由轨迹。
    pub route_trace: RouteTrace,
    /// L2 判定结果。
    pub judgement: Judgement,
    /// 得分（千分之一分）。
    pub awarded: MilliScore,
    /// 该题满分。
    pub max_score: MilliScore,
    /// 校准后置信度。
    pub confidence: Confidence,
    /// 判分状态。
    pub status: GradeStatus,
    /// 证据引用列表（至少 1 条）。
    pub evidence: Vec<EvidenceRef>,
    /// 参与判分的引擎指纹列表。
    pub engines: Vec<EngineStamp>,
    /// 创建时间。
    pub created_at: Timestamp,
    /// 复核时间（可空）。
    pub reviewed_at: Option<Timestamp>,
    /// 复核备注（可空）。
    pub review_note: Option<String>,
    /// 快速模式抽检标记。
    pub sampled: bool,
}

impl GradeRecord {
    /// 校验 §8 不变量 1、3、4、5 中结构可判定的部分。
    ///
    /// 自动接受阈值、模型校准等依赖 `grade` 上下文的门禁不在本方法内，
    /// 由 [`GradeRecord::auto_accept_blockers`] 与 `grade` 模块完成。
    ///
    /// # Errors
    ///
    /// 违反不变量时返回 [`DomainError`]。
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.revision == 0 {
            return Err(DomainError::InvalidRevision {
                revision: self.revision,
            });
        }
        if self.max_score.is_zero() {
            return Err(invalid_field("max_score", "满分必须大于 0"));
        }
        if self.awarded > self.max_score {
            return Err(DomainError::AwardedOutOfRange {
                awarded: self.awarded,
                max_score: self.max_score,
            });
        }
        self.judgement.validate_against(self.max_score)?;
        if let Some(score) = self.judgement.score()
            && score != self.awarded
        {
            return Err(DomainError::JudgeScoreInconsistent {
                judgement: self.judgement.label(),
                awarded: self.awarded,
                max_score: self.max_score,
            });
        }
        if self.evidence.is_empty() {
            return Err(DomainError::MissingEvidence);
        }
        self.route_trace.validate()?;
        for engine in &self.engines {
            engine.validate()?;
        }
        if !self.engines.is_empty()
            && !self
                .evidence
                .iter()
                .any(|evidence| matches!(evidence, EvidenceRef::ModelCall(_)))
        {
            return Err(DomainError::MissingModelCallEvidence);
        }
        if self.mode == GradeMode::Standard && self.sampled {
            return Err(DomainError::SampledInStandardMode);
        }
        match self.status {
            GradeStatus::AutoAccepted => {
                if self.route.is_manual() {
                    return Err(DomainError::AutoAcceptNotAllowed);
                }
                if matches!(self.judgement, Judgement::Ungradable { .. }) {
                    return Err(DomainError::AutoAcceptNotAllowed);
                }
            }
            GradeStatus::ManuallyReviewed => {
                if self.reviewed_at.is_none() {
                    return Err(DomainError::ReviewTimestampMissing);
                }
            }
            GradeStatus::NeedsReview | GradeStatus::Excluded => {}
        }
        if let Some(note) = &self.review_note
            && note.trim().is_empty()
        {
            return Err(invalid_field(
                "review_note",
                "复核备注不得为空字符串（应省略该字段）",
            ));
        }
        Ok(())
    }

    /// 自动接受的结构性阻塞原因（§8 不变量 2；阈值由调用方以配置传入）。
    ///
    /// 本方法只能检查记录内可见的事实；「模型未校准」需要 `JudgeOutcome`，
    /// 由 `grade` 模块合并。
    #[must_use]
    pub fn auto_accept_blockers(&self, threshold: Confidence) -> Vec<ReviewReasonKind> {
        let mut blockers = Vec::new();
        if !self.confidence.is_at_least(threshold) {
            blockers.push(ReviewReasonKind::LowConfidence);
        }
        if let Judgement::Ungradable { reason } = &self.judgement {
            blockers.push(reason.review_reason());
        }
        if self.evidence.is_empty() {
            blockers.push(ReviewReasonKind::MissingEvidence);
        }
        if self.awarded > self.max_score {
            blockers.push(ReviewReasonKind::InvalidScore);
        }
        if self.route.is_manual() {
            blockers.push(ReviewReasonKind::Ungradable);
        }
        blockers
    }

    /// 是否满足自动接受的结构条件（阈值已给定）。
    #[must_use]
    pub fn can_auto_accept(&self, threshold: Confidence) -> bool {
        self.auto_accept_blockers(threshold).is_empty()
    }

    /// 校验确定性判分记录伴随的模型调用（云端授权、角色、Schema 输出）。
    ///
    /// # Errors
    ///
    /// 调用记录非法时返回 [`DomainError`]。
    pub fn validate_model_call(&self, call: &ModelCall) -> Result<(), DomainError> {
        call.validate()
    }
}

/// 提交级排除原因（§8 `ExcludeReason`：缺考/无效提交/重复/超范围/其他）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExcludeReason {
    /// 缺考。
    Absent,
    /// 无效提交。
    Invalid,
    /// 重复。
    Duplicate,
    /// 超范围。
    OutOfScope,
    /// 其他。
    Other,
}

/// 提交级排除（§8 `SubmissionExclusion`）：区别于题级排除。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmissionExclusion {
    /// 提交 id。
    pub submission_id: SubmissionId,
    /// 排除原因。
    pub reason: ExcludeReason,
    /// 备注（可空）。
    pub note: String,
    /// 排除时间。
    pub at: Timestamp,
}

impl SubmissionExclusion {
    /// 校验排除记录（备注可为空；原因「其他」建议带说明，不强制）。
    ///
    /// # Errors
    ///
    /// 暂无失败条件（保留 `Result` 以统一调用风格）。
    pub fn validate(&self) -> Result<(), DomainError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::evidence::EvidenceRef;
    use crate::hash::Sha256Hex;
    use crate::ids::Ulid;
    use crate::recognition::ModelRole;
    use crate::values::BackendId;

    fn timestamp() -> Timestamp {
        crate::time::parse_rfc3339("2026-11-02T00:00:00Z").unwrap()
    }

    fn engine() -> EngineStamp {
        EngineStamp {
            role: ModelRole::Judge,
            backend: BackendId::local_server(),
            model_id: "judge-4b".to_owned(),
            model_version: "1".to_owned(),
            prompt_hash: None,
            params_hash: Sha256Hex::of(b"params"),
        }
    }

    fn route_trace() -> RouteTrace {
        RouteTrace {
            rule_id: "R3".to_owned(),
            inputs_hash: Sha256Hex::of(b"inputs"),
            classifier_call: None,
            decided_at: timestamp(),
        }
    }

    fn model_call_evidence() -> EvidenceRef {
        EvidenceRef::ModelCall(ModelCallId::from_ulid(Ulid::from_u128(7)))
    }

    fn grade_record(judgement: Judgement, awarded: i64) -> GradeRecord {
        GradeRecord {
            id: GradeId::from_ulid(Ulid::from_u128(1)),
            submission_id: SubmissionId::from_ulid(Ulid::from_u128(2)),
            question_id: QuestionId::from_ulid(Ulid::from_u128(3)),
            revision: 1,
            mode: GradeMode::Standard,
            route: RouteDecision::RuleThenModel,
            route_trace: route_trace(),
            judgement,
            awarded: MilliScore::from_milli(awarded).unwrap(),
            max_score: MilliScore::from_milli(5000).unwrap(),
            confidence: Confidence::new(0.9).unwrap(),
            status: GradeStatus::AutoAccepted,
            evidence: vec![model_call_evidence()],
            engines: vec![engine()],
            created_at: timestamp(),
            reviewed_at: None,
            review_note: None,
            sampled: false,
        }
    }

    fn match_judgement() -> Judgement {
        Judgement::Match {
            score: MilliScore::from_milli(5000).unwrap(),
            reason: "答案一致".to_owned(),
            evidence: vec![model_call_evidence()],
        }
    }

    #[test]
    fn valid_record_passes() {
        let record = grade_record(match_judgement(), 5000);
        assert!(record.validate().is_ok());
        assert!(record.can_auto_accept(Confidence::new(0.85).unwrap()));
    }

    #[test]
    fn judgement_score_consistency() {
        let full = MilliScore::from_milli(5000).unwrap();
        assert!(match_judgement().validate_against(full).is_ok());
        assert!(
            Judgement::Match {
                score: MilliScore::from_milli(4000).unwrap(),
                reason: "x".to_owned(),
                evidence: vec![]
            }
            .validate_against(full)
            .is_err()
        );
        assert!(
            Judgement::Mismatch {
                score: MilliScore::ZERO,
                reason: "不一致".to_owned(),
                evidence: vec![]
            }
            .validate_against(full)
            .is_ok()
        );
        assert!(
            Judgement::Mismatch {
                score: MilliScore::from_milli(1).unwrap(),
                reason: "不一致".to_owned(),
                evidence: vec![]
            }
            .validate_against(full)
            .is_err()
        );
        assert!(
            Judgement::Partial {
                score: MilliScore::from_milli(2000).unwrap(),
                satisfied: vec![JudgedUnit::Choice("A".to_owned())],
                missing: vec![JudgedUnit::Choice("B".to_owned())],
                reason: "少选".to_owned()
            }
            .validate_against(full)
            .is_ok()
        );
        assert!(
            Judgement::Partial {
                score: MilliScore::ZERO,
                satisfied: vec![],
                missing: vec![],
                reason: "少选".to_owned()
            }
            .validate_against(full)
            .is_err()
        );
        assert!(
            Judgement::Partial {
                score: full,
                satisfied: vec![],
                missing: vec![],
                reason: "少选".to_owned()
            }
            .validate_against(full)
            .is_err()
        );
    }

    #[test]
    fn awarded_out_of_range_is_rejected() {
        let mut record = grade_record(match_judgement(), 5000);
        record.awarded = MilliScore::from_milli(6000).unwrap();
        assert!(matches!(
            record.validate(),
            Err(DomainError::AwardedOutOfRange { .. })
        ));
    }

    #[test]
    fn evidence_and_model_call_requirements() {
        let mut record = grade_record(match_judgement(), 5000);
        record.evidence.clear();
        assert!(matches!(
            record.validate(),
            Err(DomainError::MissingEvidence)
        ));

        let mut record = grade_record(match_judgement(), 5000);
        record.evidence = vec![EvidenceRef::Asset(crate::hash::AssetId::of(b"crop"))];
        assert!(matches!(
            record.validate(),
            Err(DomainError::MissingModelCallEvidence)
        ));

        let mut record = grade_record(match_judgement(), 5000);
        record.engines.clear();
        record.evidence = vec![EvidenceRef::Asset(crate::hash::AssetId::of(b"crop"))];
        assert!(record.validate().is_ok());
    }

    #[test]
    fn review_and_sampling_rules() {
        let mut record = grade_record(match_judgement(), 5000);
        record.status = GradeStatus::ManuallyReviewed;
        assert!(matches!(
            record.validate(),
            Err(DomainError::ReviewTimestampMissing)
        ));
        record.reviewed_at = Some(timestamp());
        assert!(record.validate().is_ok());

        let mut record = grade_record(match_judgement(), 5000);
        record.sampled = true;
        assert!(matches!(
            record.validate(),
            Err(DomainError::SampledInStandardMode)
        ));
        record.mode = GradeMode::Fast;
        assert!(record.validate().is_ok());
    }

    #[test]
    fn auto_accept_not_allowed_for_manual_or_ungradable() {
        let mut manual = grade_record(match_judgement(), 5000);
        manual.route = RouteDecision::Manual(ManualReason::AnswerKeyMissing);
        assert!(matches!(
            manual.validate(),
            Err(DomainError::AutoAcceptNotAllowed)
        ));

        let ungradable = Judgement::Ungradable {
            reason: UngradableReason::Conflict,
        };
        let mut record = grade_record(ungradable, 0);
        assert!(matches!(
            record.validate(),
            Err(DomainError::AutoAcceptNotAllowed)
        ));
        record.status = GradeStatus::NeedsReview;
        assert!(record.validate().is_ok());
        let blockers = record.auto_accept_blockers(Confidence::new(0.85).unwrap());
        assert!(blockers.contains(&ReviewReasonKind::Conflict));
        assert!(!record.can_auto_accept(Confidence::new(0.85).unwrap()));
        assert_eq!(
            UngradableReason::BackendUnavailable.review_reason(),
            ReviewReasonKind::BackendUnavailable
        );
        assert_eq!(
            UngradableReason::Timeout.review_reason(),
            ReviewReasonKind::Ungradable
        );
    }

    #[test]
    fn low_confidence_and_invalid_score_blockers() {
        let mut record = grade_record(match_judgement(), 5000);
        record.confidence = Confidence::new(0.5).unwrap();
        assert!(
            record
                .auto_accept_blockers(Confidence::new(0.85).unwrap())
                .contains(&ReviewReasonKind::LowConfidence)
        );
        record.confidence = Confidence::new(0.9).unwrap();
        record.awarded = MilliScore::from_milli(6000).unwrap();
        assert!(
            record
                .auto_accept_blockers(Confidence::new(0.85).unwrap())
                .contains(&ReviewReasonKind::InvalidScore)
        );
    }

    #[test]
    fn judge_outcome_calibration_gate() {
        let outcome = JudgeOutcome {
            judgement: match_judgement(),
            confidence: Confidence::new(0.8).unwrap(),
            matched_alternative: Some(0),
            calibration_id: None,
            calibrated_confidence: None,
            calibration_verified: false,
        };
        assert!(!outcome.is_calibrated());
        assert!(!outcome.permits_auto_accept());
        let calibrated = JudgeOutcome {
            calibration_id: Some(crate::values::CalibrationId::new("cal-1").unwrap()),
            calibrated_confidence: Some(Confidence::new(0.8).unwrap()),
            calibration_verified: true,
            ..outcome
        };
        assert!(calibrated.is_calibrated());
        assert!(calibrated.permits_auto_accept());
        assert!(
            calibrated
                .validate(MilliScore::from_milli(5000).unwrap())
                .is_ok()
        );
    }

    #[test]
    fn revision_must_start_at_one() {
        let mut record = grade_record(match_judgement(), 5000);
        record.revision = 0;
        assert!(matches!(
            record.validate(),
            Err(DomainError::InvalidRevision { revision: 0 })
        ));
    }

    #[test]
    fn exclusion_and_tags_serde() {
        let exclusion = SubmissionExclusion {
            submission_id: SubmissionId::from_ulid(Ulid::from_u128(9)),
            reason: ExcludeReason::Absent,
            note: "缺考".to_owned(),
            at: timestamp(),
        };
        assert!(exclusion.validate().is_ok());
        assert_eq!(
            serde_json::to_string(&ExcludeReason::OutOfScope).unwrap(),
            "\"out_of_scope\""
        );
        assert_eq!(serde_json::to_string(&GradeMode::Fast).unwrap(), "\"fast\"");
        assert_eq!(
            serde_json::to_string(&RouteDecision::Manual(ManualReason::AnswerKeyMissing)).unwrap(),
            r#"{"manual":"answer_key_missing"}"#
        );
    }
}
