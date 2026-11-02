//! 评分标准、规则草稿与部分给分档位（`SPEC-domain.md` §4、§5，v0.2.2）。
//!
//! 核心不变量：
//! 1. `Rubric.total` 等于评分项分值之和；全对才给分只允许 1 个评分项；
//! 2. 「仅评分标准/明确省略/缺失」答案键下禁止存在 `ConcreteKey`（由 `AnswerKey` 枚举结构保证）；
//! 3. 分值为 0、依赖成环/缺失、题型不支持 → 错误级校验问题，禁止开始判分；
//! 4. 规则生效必须经教师确认；模型整理只能产出草稿（`RubricDraft` 与 `Rubric` 是不同类型，
//!    不存在「草稿直接变生效规则」的构造路径）。
//!
//! v0.2.2 部分给分（§4 `PartialChoiceRule`）：
//! - 仅用于多选题；`partial_choice` 非空 ⇔ 模式为「按选项部分给分」；
//! - 档位必须覆盖 1..=`correct_option_count`-1，唯一、升序、不跳档；
//! - 错选归零恒为真；官方口径必须带出处说明；档位不是累加项。

use serde::{Deserialize, Serialize};

use crate::error::{DomainError, invalid_field};
use crate::ids::{ModelCallId, QuestionId, RubricId, RubricItemId};
use crate::paper::QuestionKind;
use crate::score::MilliScore;
use crate::subject::{RubricIssue, RubricIssueSeverity};
use crate::time::Timestamp;
use crate::values::TemplateId;

/// v1 确认人固定为本地教师（`SPEC-domain.md` §4、`SPEC-rubric.md` §2.5）。
pub const LOCAL_TEACHER: &str = "local-teacher";

/// 计分方式（§4 `ScoringMode`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoringMode {
    /// 累加：各评分项独立求和。
    Additive,
    /// 全对才给分：只允许 1 个评分项。
    AllOrNothing,
    /// 分步给分：某评分项得分当且仅当「判定满足」且「所有前置项满足」。
    Stepwise,
    /// 按选项部分给分（v0.2.2）：仅多选，按命中正确选项数查档位表。
    PartialChoice,
}

/// 规则来源（§4 `RubricOrigin`）：任何来源都必须记录确认人与确认时间。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RubricOrigin {
    /// 教师手写。
    TeacherHandwritten {
        /// 确认人（v1 固定 [`LOCAL_TEACHER`]）。
        confirmed_by: String,
        /// 确认时间。
        confirmed_at: Timestamp,
    },
    /// 标准模板。
    StandardTemplate {
        /// 模板 id。
        template_id: TemplateId,
        /// 模板版本。
        template_version: u32,
        /// 确认人（v1 固定 [`LOCAL_TEACHER`]）。
        confirmed_by: String,
        /// 确认时间。
        confirmed_at: Timestamp,
    },
    /// 模型整理后经教师确认。
    ModelStructured {
        /// 整理所用模型调用 id（可复现）。
        model_call_id: ModelCallId,
        /// 确认人（v1 固定 [`LOCAL_TEACHER`]）。
        confirmed_by: String,
        /// 确认时间。
        confirmed_at: Timestamp,
    },
}

impl RubricOrigin {
    /// 确认人。
    #[must_use]
    pub fn confirmed_by(&self) -> &str {
        match self {
            Self::TeacherHandwritten { confirmed_by, .. }
            | Self::StandardTemplate { confirmed_by, .. }
            | Self::ModelStructured { confirmed_by, .. } => confirmed_by,
        }
    }

    /// 确认时间。
    #[must_use]
    pub const fn confirmed_at(&self) -> Timestamp {
        match self {
            Self::TeacherHandwritten { confirmed_at, .. }
            | Self::StandardTemplate { confirmed_at, .. }
            | Self::ModelStructured { confirmed_at, .. } => *confirmed_at,
        }
    }

    fn validate(&self) -> Result<(), DomainError> {
        if self.confirmed_by().trim().is_empty() {
            return Err(invalid_field("origin.confirmed_by", "确认人不得为空"));
        }
        if let Self::StandardTemplate {
            template_version, ..
        } = self
            && *template_version == 0
        {
            return Err(invalid_field(
                "origin.template_version",
                "模板版本必须大于 0",
            ));
        }
        Ok(())
    }
}

/// 评分项（§4 `RubricItem`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RubricItem {
    /// 评分项 id。
    pub id: RubricItemId,
    /// 可读判据（面向教师）。
    pub criterion: String,
    /// 分值（千分之一分；必须大于 0）。
    pub points: MilliScore,
    /// 是否必须满足。
    pub required: bool,
    /// 前置评分项 id 列表（语义：前置必须命中）。
    pub depends_on: Vec<RubricItemId>,
}

impl RubricItem {
    fn validate_content(&self) -> Result<(), DomainError> {
        if self.criterion.trim().is_empty() {
            return Err(invalid_field("criterion", "评分项判据不得为空"));
        }
        if self.points.is_zero() {
            return Err(invalid_field(
                "points",
                format!("评分项 {} 分值必须大于 0", self.id),
            ));
        }
        if self.depends_on.contains(&self.id) {
            return Err(DomainError::RubricDependencyCycle {
                items: vec![self.id, self.id],
            });
        }
        Ok(())
    }
}

/// 部分给分档位来源（§4 `PartialChoiceSource`，v0.2.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartialChoiceSource {
    /// 官方口径（当次考试评分细则）。
    Official,
    /// 模板数据（内置全国卷模板随包）。
    TemplateData,
    /// 教师自定义。
    TeacherCustom,
}

/// 部分给分档位（§4 `PartialChoiceTier`，v0.2.2）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialChoiceTier {
    /// 命中的正确选项数。
    pub selected_correct: u32,
    /// 该档得分。
    pub points: MilliScore,
    /// 面向教师的说明（可空）。
    pub label: Option<String>,
}

/// 部分给分档位规则（§4 `PartialChoiceRule`，v0.2.2）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialChoiceRule {
    /// 正确选项数：现行官方口径为 2 或 3；其它取值超出范围，必须人工处理。
    pub correct_option_count: u32,
    /// 档位列表：覆盖 1..=`correct_option_count`-1，唯一、升序、不跳档。
    pub tiers: Vec<PartialChoiceTier>,
    /// 错选归零：**恒为真**（不可配置；写入数据只为审计可读，不得设为假）。
    pub wrong_selection_zero: bool,
    /// 口径来源。
    pub source: PartialChoiceSource,
    /// 口径出处与适用年份；`source` 为官方口径时必填。
    pub source_note: Option<String>,
    /// 该题满分，必须与 `Rubric.total` 一致。
    pub total_milli: MilliScore,
}

impl PartialChoiceRule {
    /// 校验档位规则（§4 不变量 6–9、11）。
    ///
    /// `granularity` 为评分粒度（可空）；给定时每档分值必须是其整数倍。
    ///
    /// # Errors
    ///
    /// 违反任一档位不变量时返回 [`DomainError`]。
    pub fn validate(
        &self,
        rubric_total: MilliScore,
        granularity: Option<MilliScore>,
    ) -> Result<(), DomainError> {
        if !matches!(self.correct_option_count, 2 | 3) {
            return Err(DomainError::PartialChoiceOptionCount {
                count: self.correct_option_count,
            });
        }
        if self.total_milli != rubric_total {
            return Err(DomainError::PartialChoiceTotalMismatch {
                rule_total: self.total_milli,
                rubric_total,
            });
        }
        if !self.wrong_selection_zero {
            return Err(DomainError::PartialChoiceWrongSelectionZero);
        }
        if self.source == PartialChoiceSource::Official {
            match &self.source_note {
                Some(note) if !note.trim().is_empty() => {}
                _ => return Err(DomainError::PartialChoiceSourceNoteMissing),
            }
        }
        let expected_count = self.correct_option_count - 1;
        let tiers_len = u32::try_from(self.tiers.len())
            .map_err(|_| invalid_field("tiers", "档位数量超出 u32 上限"))?;
        let actual: Vec<u32> = self
            .tiers
            .iter()
            .map(|tier| tier.selected_correct)
            .collect();
        let coverage_complete = tiers_len == expected_count
            && actual
                .iter()
                .enumerate()
                .all(|(index, value)| usize::try_from(*value) == Ok(index + 1));
        if self.tiers.is_empty() || !coverage_complete {
            return Err(DomainError::PartialChoiceTierCoverage {
                expected: expected_count,
                actual,
            });
        }
        let mut previous_points: Option<MilliScore> = None;
        for tier in &self.tiers {
            if tier.points.is_zero() || tier.points >= self.total_milli {
                return Err(DomainError::PartialChoiceTierPoints {
                    tier: tier.selected_correct,
                    points: tier.points,
                    total: self.total_milli,
                });
            }
            if let Some(previous) = previous_points
                && tier.points < previous
            {
                return Err(DomainError::PartialChoiceTierOrder);
            }
            previous_points = Some(tier.points);
            if let Some(label) = &tier.label
                && label.trim().is_empty()
            {
                return Err(invalid_field(
                    "tiers.label",
                    "档位说明不得为空字符串（应省略该字段）",
                ));
            }
        }
        self.check_granularity(granularity)?;
        Ok(())
    }

    fn check_granularity(&self, granularity: Option<MilliScore>) -> Result<(), DomainError> {
        if let Some(granularity) = granularity {
            for tier in &self.tiers {
                if !tier.points.is_multiple_of(granularity)? {
                    return Err(DomainError::GranularityViolation {
                        value: tier.points,
                        granularity,
                    });
                }
            }
        }
        Ok(())
    }
}

/// 评分标准（§4 `Rubric`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rubric {
    /// 评分标准 id。
    pub id: RubricId,
    /// 版本（升级只追加，不回改历史）。
    pub version: u32,
    /// 评分项列表。
    pub items: Vec<RubricItem>,
    /// 计分方式。
    pub mode: ScoringMode,
    /// 合计分值：等于各评分项分值之和；部分给分模式时为该题满分。
    pub total: MilliScore,
    /// 规则来源与确认信息。
    pub origin: RubricOrigin,
    /// 部分给分档位规则（v0.2.2；模式为部分给分时必填，其余模式必须为空）。
    pub partial_choice: Option<PartialChoiceRule>,
}

impl Rubric {
    /// 校验评分标准（§4 不变量 1、3、5–11）。
    ///
    /// # Errors
    ///
    /// 违反任一不变量时返回 [`DomainError`]，调用方必须阻止开始判分。
    pub fn validate(&self, kind: QuestionKind) -> Result<(), DomainError> {
        self.validate_with_granularity(kind, None)
    }

    /// 校验评分标准，并额外检查分值是否为给定评分粒度的整数倍。
    ///
    /// # Errors
    ///
    /// 与 [`Rubric::validate`] 相同，另加粒度违规。
    pub fn validate_with_granularity(
        &self,
        kind: QuestionKind,
        granularity: Option<MilliScore>,
    ) -> Result<(), DomainError> {
        if self.items.is_empty() {
            return Err(DomainError::EmptyRubric);
        }
        if self.version == 0 {
            return Err(invalid_field("version", "评分标准版本必须大于 0"));
        }
        self.origin.validate()?;
        let mut computed = MilliScore::ZERO;
        let mut ids: Vec<RubricItemId> = Vec::with_capacity(self.items.len());
        for item in &self.items {
            item.validate_content()?;
            if ids.contains(&item.id) {
                return Err(invalid_field(
                    "items",
                    format!("评分项 id 重复：{}", item.id),
                ));
            }
            ids.push(item.id);
            computed = computed.checked_add_score(item.points)?;
        }
        if computed != self.total {
            return Err(DomainError::RubricTotalMismatch {
                declared: self.total,
                computed,
            });
        }
        self.validate_dependencies()?;
        self.validate_mode(kind, granularity)?;
        Ok(())
    }

    fn validate_dependencies(&self) -> Result<(), DomainError> {
        let index_of = |id: RubricItemId| self.items.iter().position(|item| item.id == id);
        let mut edges: Vec<Vec<usize>> = Vec::with_capacity(self.items.len());
        for item in &self.items {
            let mut targets = Vec::with_capacity(item.depends_on.len());
            for dependency in &item.depends_on {
                match index_of(*dependency) {
                    Some(index) => targets.push(index),
                    None => {
                        return Err(DomainError::RubricDependencyMissing {
                            item: item.id,
                            missing: *dependency,
                        });
                    }
                }
            }
            edges.push(targets);
        }
        if let Some(cycle) = detect_cycle(&edges) {
            return Err(DomainError::RubricDependencyCycle {
                items: cycle
                    .into_iter()
                    .map(|index| self.items[index].id)
                    .collect(),
            });
        }
        Ok(())
    }

    fn validate_mode(
        &self,
        kind: QuestionKind,
        granularity: Option<MilliScore>,
    ) -> Result<(), DomainError> {
        match self.mode {
            ScoringMode::AllOrNothing => {
                if self.items.len() != 1 {
                    return Err(DomainError::AllOrNothingItemCount {
                        count: self.items.len(),
                    });
                }
                if self.partial_choice.is_some() {
                    return Err(DomainError::PartialChoiceNotAllowed);
                }
            }
            ScoringMode::PartialChoice => {
                if kind != QuestionKind::MultipleChoice {
                    return Err(DomainError::PartialChoiceKindUnsupported { kind });
                }
                let rule = self
                    .partial_choice
                    .as_ref()
                    .ok_or(DomainError::PartialChoiceMissing)?;
                if self.items.len() != 1 {
                    return Err(DomainError::PartialChoiceItemCount {
                        count: self.items.len(),
                    });
                }
                // 不变量 5 的「全对评分项分值 = 满分」由不变量 1（合计=各项之和）
                // 与「恰好 1 个评分项」自动保证，无需重复检查。
                rule.validate(self.total, granularity)?;
            }
            ScoringMode::Additive | ScoringMode::Stepwise => {
                if self.partial_choice.is_some() {
                    return Err(DomainError::PartialChoiceNotAllowed);
                }
            }
        }
        if let Some(granularity) = granularity {
            for item in &self.items {
                if !item.points.is_multiple_of(granularity)? {
                    return Err(DomainError::GranularityViolation {
                        value: item.points,
                        granularity,
                    });
                }
            }
        }
        Ok(())
    }

    /// 错误级校验问题数量（用于导入期门禁；本方法只做结构校验，不替代学科包静态校验）。
    #[must_use]
    pub fn has_error_issue(&self, kind: QuestionKind) -> bool {
        self.validate(kind).is_err()
    }
}

/// 规则原文来源（§5 `RuleTextSource`：手写/模板/导入）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleTextSource {
    /// 教师手写。
    Handwritten,
    /// 模板。
    Template,
    /// 导入。
    Imported,
}

/// 规则原文（§5 `RuleText`）：教师自然语言规则，可为空（= 使用模板）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleText {
    /// 题目 id。
    pub question_id: QuestionId,
    /// 规则原文（可为空）。
    pub text: String,
    /// 原文来源。
    pub source: RuleTextSource,
}

/// 草稿状态（§5 `DraftStatus`：草稿/待确认/已确认/已拒绝）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftStatus {
    /// 草稿。
    Draft,
    /// 待确认。
    PendingConfirmation,
    /// 已确认（终态）。
    Confirmed,
    /// 已拒绝（终态）。
    Rejected,
}

impl DraftStatus {
    /// 是否为终态（不可编辑）。
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Confirmed | Self::Rejected)
    }
}

/// 草稿来源（§5 `DraftOrigin`：手写直译/模型整理/模板套用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftOrigin {
    /// 手写直译。
    HandwrittenTransliteration,
    /// 模型整理。
    ModelStructured,
    /// 模板套用。
    TemplateApplied,
}

/// 草稿评分项（§5 `RubricDraftItem`）：`depends_on` 引用草稿内序号（1 起）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RubricDraftItem {
    /// 可读判据。
    pub criterion: String,
    /// 分值（必须大于 0）。
    pub points: MilliScore,
    /// 是否必须满足。
    pub required: bool,
    /// 依赖的草稿内序号（1 起，可为空）。
    pub depends_on: Vec<u32>,
}

/// 规则草稿（§5 `RubricDraft`）。
///
/// 不变量：模型整理只允许产出草稿；草稿校验不通过时确认操作被拒绝。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RubricDraft {
    /// 草稿 id（与生效 `Rubric` 共用 `RubricId`；确认时沿用）。
    pub id: RubricId,
    /// 题目 id。
    pub question_id: QuestionId,
    /// 状态。
    pub status: DraftStatus,
    /// 草稿评分项。
    pub items: Vec<RubricDraftItem>,
    /// 原始规则文本。
    pub raw_text: String,
    /// 草稿来源。
    pub origin: DraftOrigin,
    /// 模型调用 id（模型整理时必填，便于复现）。
    pub model_call_id: Option<ModelCallId>,
    /// 校验结果（警告/错误）。
    pub validation: Vec<RubricIssue>,
    /// 创建时间。
    pub created_at: Timestamp,
    /// 确认时间（仅「已确认」状态填写）。
    pub confirmed_at: Option<Timestamp>,
}

impl RubricDraft {
    /// 校验草稿自身（来源、状态与确认时间一致性、评分项结构）。
    ///
    /// # Errors
    ///
    /// 失败返回 [`DomainError`]。
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.origin == DraftOrigin::ModelStructured && self.model_call_id.is_none() {
            return Err(DomainError::MissingModelCallId);
        }
        self.validate_items()?;
        match self.status {
            DraftStatus::Confirmed => {
                if self.confirmed_at.is_none() {
                    return Err(DomainError::MissingConfirmationTimestamp);
                }
            }
            DraftStatus::Draft | DraftStatus::PendingConfirmation | DraftStatus::Rejected => {
                if self.confirmed_at.is_some() {
                    return Err(DomainError::UnexpectedConfirmationTimestamp);
                }
            }
        }
        Ok(())
    }

    /// 校验草稿评分项：判据非空、分值大于 0、依赖序号存在且无环。
    ///
    /// # Errors
    ///
    /// 失败返回 [`DomainError`]。
    pub fn validate_items(&self) -> Result<(), DomainError> {
        let length = self.items.len();
        let mut edges: Vec<Vec<usize>> = Vec::with_capacity(length);
        for (index, item) in self.items.iter().enumerate() {
            if item.criterion.trim().is_empty() {
                return Err(invalid_field(
                    "items.criterion",
                    format!("第 {} 个评分项判据为空", index + 1),
                ));
            }
            if item.points.is_zero() {
                return Err(invalid_field(
                    "items.points",
                    format!("第 {} 个评分项分值必须大于 0", index + 1),
                ));
            }
            let mut targets = Vec::with_capacity(item.depends_on.len());
            for dependency in &item.depends_on {
                if *dependency == 0 || *dependency as usize > length {
                    return Err(DomainError::DraftDependencyOutOfRange {
                        index: *dependency,
                        len: length,
                    });
                }
                targets.push(*dependency as usize - 1);
            }
            edges.push(targets);
        }
        if let Some(cycle) = detect_cycle(&edges) {
            return Err(DomainError::DraftDependencyCycle {
                items: cycle
                    .into_iter()
                    .map(|index| u32::try_from(index).map_or(u32::MAX, |value| value + 1))
                    .collect(),
            });
        }
        Ok(())
    }

    /// 错误级校验问题数量。
    #[must_use]
    pub fn error_issue_count(&self) -> usize {
        self.validation
            .iter()
            .filter(|issue| issue.severity == RubricIssueSeverity::Error)
            .count()
    }

    /// 是否允许确认（§5：错误级问题存在时禁止确认；状态必须为「待确认」）。
    ///
    /// # Errors
    ///
    /// 状态不是「待确认」返回 [`DomainError::DraftNotConfirmable`]；
    /// 存在错误级问题返回 [`DomainError::DraftHasErrors`]；
    /// 草稿没有评分项返回 [`DomainError::EmptyRubric`]。
    pub fn ensure_confirmable(&self) -> Result<(), DomainError> {
        if self.status != DraftStatus::PendingConfirmation {
            return Err(DomainError::DraftNotConfirmable {
                status: self.status,
            });
        }
        if self.items.is_empty() {
            return Err(DomainError::EmptyRubric);
        }
        let errors = self.error_issue_count();
        if errors > 0 {
            return Err(DomainError::DraftHasErrors { count: errors });
        }
        self.validate_items()?;
        Ok(())
    }
}

/// 在依赖图上检测环；返回环上的节点下标（首尾相同）。
fn detect_cycle(edges: &[Vec<usize>]) -> Option<Vec<usize>> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum State {
        Unvisited,
        Visiting,
        Done,
    }
    fn visit(
        node: usize,
        edges: &[Vec<usize>],
        states: &mut [State],
        stack: &mut Vec<usize>,
    ) -> Option<Vec<usize>> {
        match states[node] {
            State::Done => return None,
            State::Visiting => {
                let start = stack.iter().position(|item| *item == node)?;
                let mut cycle: Vec<usize> = stack[start..].to_vec();
                cycle.push(node);
                return Some(cycle);
            }
            State::Unvisited => {}
        }
        states[node] = State::Visiting;
        stack.push(node);
        for target in &edges[node] {
            if let Some(cycle) = visit(*target, edges, states, stack) {
                return Some(cycle);
            }
        }
        stack.pop();
        states[node] = State::Done;
        None
    }

    let mut states = vec![State::Unvisited; edges.len()];
    let mut stack = Vec::new();
    for node in 0..edges.len() {
        if let Some(cycle) = visit(node, edges, &mut states, &mut stack) {
            return Some(cycle);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::ids::Ulid;

    fn item(id: u128, criterion: &str, points: i64, depends_on: Vec<RubricItemId>) -> RubricItem {
        RubricItem {
            id: RubricItemId::from_ulid(Ulid::from_u128(id)),
            criterion: criterion.to_owned(),
            points: MilliScore::from_milli(points).unwrap(),
            required: false,
            depends_on,
        }
    }

    fn origin() -> RubricOrigin {
        RubricOrigin::TeacherHandwritten {
            confirmed_by: LOCAL_TEACHER.to_owned(),
            confirmed_at: crate::time::parse_rfc3339("2026-11-02T00:00:00Z").unwrap(),
        }
    }

    fn additive_rubric() -> Rubric {
        Rubric {
            id: RubricId::from_ulid(Ulid::from_u128(100)),
            version: 1,
            items: vec![
                item(1, "步骤一", 1000, vec![]),
                item(2, "步骤二", 1500, vec![]),
            ],
            mode: ScoringMode::Additive,
            total: MilliScore::from_milli(2500).unwrap(),
            origin: origin(),
            partial_choice: None,
        }
    }

    fn partial_choice_rubric() -> Rubric {
        Rubric {
            id: RubricId::from_ulid(Ulid::from_u128(200)),
            version: 1,
            items: vec![item(1, "全对", 6000, vec![])],
            mode: ScoringMode::PartialChoice,
            total: MilliScore::from_milli(6000).unwrap(),
            origin: origin(),
            partial_choice: Some(PartialChoiceRule {
                correct_option_count: 3,
                tiers: vec![
                    PartialChoiceTier {
                        selected_correct: 1,
                        points: MilliScore::from_milli(2000).unwrap(),
                        label: Some("选对 1 个".to_owned()),
                    },
                    PartialChoiceTier {
                        selected_correct: 2,
                        points: MilliScore::from_milli(4000).unwrap(),
                        label: None,
                    },
                ],
                wrong_selection_zero: true,
                source: PartialChoiceSource::Official,
                source_note: Some("2026 年全国卷评分细则".to_owned()),
                total_milli: MilliScore::from_milli(6000).unwrap(),
            }),
        }
    }

    #[test]
    fn additive_rubric_passes() {
        assert!(
            additive_rubric()
                .validate(QuestionKind::Calculation)
                .is_ok()
        );
    }

    #[test]
    fn total_mismatch_is_rejected() {
        let mut rubric = additive_rubric();
        rubric.total = MilliScore::from_milli(2000).unwrap();
        assert!(matches!(
            rubric.validate(QuestionKind::Calculation),
            Err(DomainError::RubricTotalMismatch { .. })
        ));
    }

    #[test]
    fn all_or_nothing_requires_single_item() {
        let mut rubric = additive_rubric();
        rubric.mode = ScoringMode::AllOrNothing;
        assert!(matches!(
            rubric.validate(QuestionKind::Calculation),
            Err(DomainError::AllOrNothingItemCount { count: 2 })
        ));
    }

    #[test]
    fn zero_point_item_is_rejected() {
        let mut rubric = additive_rubric();
        rubric.items[0].points = MilliScore::ZERO;
        assert!(rubric.validate(QuestionKind::Calculation).is_err());
    }

    #[test]
    fn dependency_missing_and_cycle_are_rejected() {
        let mut rubric = additive_rubric();
        rubric.items[0].depends_on = vec![RubricItemId::from_ulid(Ulid::from_u128(999))];
        assert!(matches!(
            rubric.validate(QuestionKind::Calculation),
            Err(DomainError::RubricDependencyMissing { .. })
        ));

        let mut rubric = additive_rubric();
        rubric.items[0].depends_on = vec![rubric.items[1].id];
        rubric.items[1].depends_on = vec![rubric.items[0].id];
        assert!(matches!(
            rubric.validate(QuestionKind::Calculation),
            Err(DomainError::RubricDependencyCycle { .. })
        ));
    }

    #[test]
    fn empty_rubric_is_rejected() {
        let mut rubric = additive_rubric();
        rubric.items.clear();
        rubric.total = MilliScore::ZERO;
        assert!(matches!(
            rubric.validate(QuestionKind::Calculation),
            Err(DomainError::EmptyRubric)
        ));
    }

    #[test]
    fn partial_choice_rules_v022() {
        assert!(
            partial_choice_rubric()
                .validate(QuestionKind::MultipleChoice)
                .is_ok()
        );

        // 题型必须为多选。
        assert!(matches!(
            partial_choice_rubric().validate(QuestionKind::SingleChoice),
            Err(DomainError::PartialChoiceKindUnsupported { .. })
        ));

        // 模式互斥：其它模式不得携带 partial_choice。
        let mut rubric = additive_rubric();
        rubric.partial_choice = partial_choice_rubric().partial_choice;
        assert!(matches!(
            rubric.validate(QuestionKind::Calculation),
            Err(DomainError::PartialChoiceNotAllowed)
        ));

        // 部分给分必须填写档位规则。
        let mut rubric = partial_choice_rubric();
        rubric.partial_choice = None;
        assert!(matches!(
            rubric.validate(QuestionKind::MultipleChoice),
            Err(DomainError::PartialChoiceMissing)
        ));

        // 档位覆盖必须完整（3 个正确选项需要 1、2 两档）。
        let mut rubric = partial_choice_rubric();
        if let Some(rule) = &mut rubric.partial_choice {
            rule.tiers.truncate(1);
        }
        assert!(matches!(
            rubric.validate(QuestionKind::MultipleChoice),
            Err(DomainError::PartialChoiceTierCoverage { .. })
        ));

        // 错选归零必须为真。
        let mut rubric = partial_choice_rubric();
        if let Some(rule) = &mut rubric.partial_choice {
            rule.wrong_selection_zero = false;
        }
        assert!(matches!(
            rubric.validate(QuestionKind::MultipleChoice),
            Err(DomainError::PartialChoiceWrongSelectionZero)
        ));

        // 官方口径必须带出处。
        let mut rubric = partial_choice_rubric();
        if let Some(rule) = &mut rubric.partial_choice {
            rule.source_note = None;
        }
        assert!(matches!(
            rubric.validate(QuestionKind::MultipleChoice),
            Err(DomainError::PartialChoiceSourceNoteMissing)
        ));

        // 档位分值必须严格小于满分且大于 0。
        let mut rubric = partial_choice_rubric();
        if let Some(rule) = &mut rubric.partial_choice {
            rule.tiers[1].points = MilliScore::from_milli(6000).unwrap();
        }
        assert!(matches!(
            rubric.validate(QuestionKind::MultipleChoice),
            Err(DomainError::PartialChoiceTierPoints { .. })
        ));
    }

    #[test]
    fn partial_choice_granularity_is_enforced() {
        let rubric = partial_choice_rubric();
        let granularity = MilliScore::from_milli(1000).unwrap();
        assert!(
            rubric
                .validate_with_granularity(QuestionKind::MultipleChoice, Some(granularity))
                .is_ok()
        );
        let odd = MilliScore::from_milli(1500).unwrap();
        assert!(matches!(
            rubric.validate_with_granularity(QuestionKind::MultipleChoice, Some(odd)),
            Err(DomainError::GranularityViolation { .. })
        ));
    }

    #[test]
    fn draft_state_machine_and_confirmation_gate() {
        let mut draft = RubricDraft {
            id: RubricId::from_ulid(Ulid::from_u128(300)),
            question_id: QuestionId::from_ulid(Ulid::from_u128(301)),
            status: DraftStatus::Draft,
            items: vec![RubricDraftItem {
                criterion: "写出公式".to_owned(),
                points: MilliScore::from_milli(1000).unwrap(),
                required: true,
                depends_on: vec![],
            }],
            raw_text: "写出公式得 1 分".to_owned(),
            origin: DraftOrigin::HandwrittenTransliteration,
            model_call_id: None,
            validation: Vec::new(),
            created_at: crate::time::parse_rfc3339("2026-11-02T00:00:00Z").unwrap(),
            confirmed_at: None,
        };
        assert!(draft.validate().is_ok());
        assert!(matches!(
            draft.ensure_confirmable(),
            Err(DomainError::DraftNotConfirmable { .. })
        ));

        draft.status = DraftStatus::PendingConfirmation;
        assert!(draft.ensure_confirmable().is_ok());

        draft.validation.push(RubricIssue {
            severity: RubricIssueSeverity::Error,
            item: None,
            message: "分值合计不等于题目满分".to_owned(),
        });
        assert!(matches!(
            draft.ensure_confirmable(),
            Err(DomainError::DraftHasErrors { count: 1 })
        ));

        draft.validation.clear();
        draft.status = DraftStatus::Confirmed;
        assert!(matches!(
            draft.validate(),
            Err(DomainError::MissingConfirmationTimestamp)
        ));
        draft.confirmed_at = Some(crate::time::parse_rfc3339("2026-11-02T01:00:00Z").unwrap());
        assert!(draft.validate().is_ok());
        assert!(draft.status.is_terminal());
    }

    #[test]
    fn model_structured_draft_requires_model_call() {
        let mut draft = RubricDraft {
            id: RubricId::from_ulid(Ulid::from_u128(400)),
            question_id: QuestionId::from_ulid(Ulid::from_u128(401)),
            status: DraftStatus::Draft,
            items: Vec::new(),
            raw_text: "每步 1 分".to_owned(),
            origin: DraftOrigin::ModelStructured,
            model_call_id: None,
            validation: Vec::new(),
            created_at: crate::time::parse_rfc3339("2026-11-02T00:00:00Z").unwrap(),
            confirmed_at: None,
        };
        assert!(matches!(
            draft.validate(),
            Err(DomainError::MissingModelCallId)
        ));
        draft.model_call_id = Some(ModelCallId::from_ulid(Ulid::from_u128(402)));
        assert!(draft.validate().is_ok());
    }

    #[test]
    fn draft_dependency_validation() {
        let mut draft = RubricDraft {
            id: RubricId::from_ulid(Ulid::from_u128(500)),
            question_id: QuestionId::from_ulid(Ulid::from_u128(501)),
            status: DraftStatus::Draft,
            items: vec![
                RubricDraftItem {
                    criterion: "A".to_owned(),
                    points: MilliScore::from_milli(1000).unwrap(),
                    required: false,
                    depends_on: vec![2],
                },
                RubricDraftItem {
                    criterion: "B".to_owned(),
                    points: MilliScore::from_milli(1000).unwrap(),
                    required: false,
                    depends_on: vec![1],
                },
            ],
            raw_text: String::new(),
            origin: DraftOrigin::TemplateApplied,
            model_call_id: None,
            validation: Vec::new(),
            created_at: crate::time::parse_rfc3339("2026-11-02T00:00:00Z").unwrap(),
            confirmed_at: None,
        };
        assert!(matches!(
            draft.validate_items(),
            Err(DomainError::DraftDependencyCycle { .. })
        ));
        draft.items[0].depends_on = vec![3];
        assert!(matches!(
            draft.validate_items(),
            Err(DomainError::DraftDependencyOutOfRange { index: 3, len: 2 })
        ));
    }

    #[test]
    fn rubric_origin_requires_confirmer() {
        let mut rubric = additive_rubric();
        rubric.origin = RubricOrigin::TeacherHandwritten {
            confirmed_by: "  ".to_owned(),
            confirmed_at: crate::time::parse_rfc3339("2026-11-02T00:00:00Z").unwrap(),
        };
        assert!(rubric.validate(QuestionKind::Calculation).is_err());
    }

    #[test]
    fn serde_tags_are_snake_case() {
        assert_eq!(
            serde_json::to_string(&ScoringMode::PartialChoice).unwrap(),
            "\"partial_choice\""
        );
        assert_eq!(
            serde_json::to_string(&DraftOrigin::ModelStructured).unwrap(),
            "\"model_structured\""
        );
        assert_eq!(
            serde_json::to_string(&PartialChoiceSource::Official).unwrap(),
            "\"official\""
        );
    }
}
