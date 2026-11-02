//! 端口、配置与伴随记录（`SPEC-rubric.md` §2.6；`SPEC-domain.md` §15.2 模块自有类型）。
//!
//! `eg-platform` 的仓储/审计接口仍在并行实现（T12/T18），本模块先定义端口 + 内存实现，
//! 由 `app`/`platform` 在接入期提供适配器（TODO(T12/T18)：接 `platform::repo` 的
//! `rubric_revision`/`rubric_draft`/`rule_text` 与 `platform::audit::AuditLog`）。
//!
//! 说明：`eg_domain::RubricDraft` 不含 `mode`/`partial_choice`/模板引用，
//! 这些随草稿存在但尚未进入领域类型的字段由 [`StoredDraft`] 承载；
//! 确认时再组装 `RubricOrigin` 与生效 `Rubric`（见 [`crate::confirm`]）。

use eg_domain::{
    MilliScore, ModelCallId, PartialChoiceRule, QuestionId, QuestionKind, Rubric, RubricDraft,
    RubricId, RubricItemId, ScoringMode, Sha256Hex, TemplateId, Timestamp,
};

use crate::error::{Result, RubricError};

/// 默认评分粒度：0.1 分 = 100 千分之一分（`CONFIG.md` §7 `rubric.min_score_granularity`）。
pub const DEFAULT_GRANULARITY_MILLI: i64 = 100;

/// 单题评分项数量上限（`SPEC-rubric.md` §2.4）。
pub const MAX_ITEMS: usize = 50;

/// 规则模块运行配置（由 `app` 从 `CONFIG.md` 读取后注入；本模块不读配置文件）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RubricSettings {
    /// 最小分值粒度（对应 `rubric.min_score_granularity`，默认 0.1 分）。
    pub granularity: MilliScore,
    /// 部分给分总开关（对应 `rubric.partial_choice_enabled`，默认开）。
    pub partial_choice_enabled: bool,
    /// 草稿保存后是否自动静态校验（对应 `rubric.auto_validate`，默认开）。
    pub auto_validate: bool,
    /// 是否允许自定义模板导入/新建（对应 `rubric.allow_custom_template`，默认开）。
    pub allow_custom_template: bool,
    /// 学科包支持题型（由 `SubjectCapabilities.kinds` 注入；为空表示不限制，仅用于测试）。
    pub supported_kinds: Vec<QuestionKind>,
}

impl Default for RubricSettings {
    fn default() -> Self {
        Self {
            granularity: MilliScore::from_milli(DEFAULT_GRANULARITY_MILLI)
                .unwrap_or(MilliScore::ZERO),
            partial_choice_enabled: true,
            auto_validate: true,
            allow_custom_template: true,
            supported_kinds: all_question_kinds(),
        }
    }
}

impl RubricSettings {
    /// 校验配置值（粒度必须为正；题型声明非空或显式允许为空）。
    ///
    /// # Errors
    ///
    /// 粒度为零时返回 [`RubricError::Config`]。
    pub fn validate(&self) -> Result<()> {
        if self.granularity.is_zero() {
            return Err(RubricError::config("评分粒度必须大于 0"));
        }
        Ok(())
    }

    /// 题型是否受学科包支持（`supported_kinds` 为空视为不限制）。
    #[must_use]
    pub fn supports_kind(&self, kind: QuestionKind) -> bool {
        self.supported_kinds.is_empty() || self.supported_kinds.contains(&kind)
    }
}

/// 全部题型（`SPEC-domain.md` §3 `QuestionKind` 全集）。
#[must_use]
pub fn all_question_kinds() -> Vec<QuestionKind> {
    vec![
        QuestionKind::SingleChoice,
        QuestionKind::MultipleChoice,
        QuestionKind::TrueFalse,
        QuestionKind::FillInBlank,
        QuestionKind::NumericOnly,
        QuestionKind::Calculation,
        QuestionKind::Proof,
        QuestionKind::ShortAnswer,
        QuestionKind::Essay,
        QuestionKind::Other,
    ]
}

/// 全国卷模板包的四套卷型（`CONFIG.md` §7 `rubric.national_sets`）。
pub const NATIONAL_SET_IDS: [&str; 4] = [
    "new-curriculum-1",
    "new-curriculum-2",
    "national-jia",
    "national-yi",
];

/// 模板版本（模板数据用 `<snapshot_year>.<修订号>` 文本，如 `2026.1`）。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct TemplateVersion {
    /// 快照年份。
    pub snapshot_year: u32,
    /// 同一年度内的修订号。
    pub revision: u32,
}

impl TemplateVersion {
    /// 构造并校验（年份 2000–2100、修订号 0–999）。
    ///
    /// # Errors
    ///
    /// 越界返回 [`RubricError::TemplateInvalid`]。
    pub fn new(snapshot_year: u32, revision: u32) -> Result<Self> {
        if !(2000..=2100).contains(&snapshot_year) {
            return Err(RubricError::template_invalid(format!(
                "模板快照年份越界：{snapshot_year}"
            )));
        }
        if revision > 999 {
            return Err(RubricError::template_invalid(format!(
                "模板修订号越界：{revision}"
            )));
        }
        Ok(Self {
            snapshot_year,
            revision,
        })
    }

    /// 解析 `2026.1` 形式的版本文本。
    ///
    /// # Errors
    ///
    /// 格式不符返回 [`RubricError::TemplateInvalid`]。
    pub fn parse(text: &str) -> Result<Self> {
        let trimmed = text.trim();
        let (year, revision) = trimmed.split_once('.').ok_or_else(|| {
            RubricError::template_invalid(format!(
                "模板版本格式应为 <年份>.<修订号>，实际 {trimmed}"
            ))
        })?;
        let snapshot_year: u32 = year
            .parse()
            .map_err(|_| RubricError::template_invalid(format!("模板年份不是整数：{year}")))?;
        let revision: u32 = revision.parse().map_err(|_| {
            RubricError::template_invalid(format!("模板修订号不是整数：{revision}"))
        })?;
        Self::new(snapshot_year, revision)
    }

    /// 映射为 `eg_domain::RubricOrigin::StandardTemplate.template_version` 使用的 `u32`。
    ///
    /// 编码规则：`snapshot_year × 1000 + revision`（`2026.1` → `2_026_001`）。
    /// 领域类型只接受非零 `u32`，此编码保证可逆且稳定；界面仍显示原始文本。
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.snapshot_year * 1000 + self.revision
    }
}

impl core::fmt::Display for TemplateVersion {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{}.{}", self.snapshot_year, self.revision)
    }
}

/// 模板状态（`SPEC-rubric.md` §2.2；`CONFIG.md` §7 active/retired 分组）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemplateStatus {
    /// 现行卷。
    Active,
    /// 历史卷（只读快照）。
    Retired {
        /// 停止命制年份。
        retired_since: u32,
    },
}

impl TemplateStatus {
    /// 是否现行卷。
    #[must_use]
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Active)
    }

    /// 停用年份（现行卷返回 `None`）。
    #[must_use]
    pub const fn retired_since(self) -> Option<u32> {
        match self {
            Self::Active => None,
            Self::Retired { retired_since } => Some(retired_since),
        }
    }
}

/// 草稿关联的模板引用（进审计与确认时的 `RubricOrigin::StandardTemplate`）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TemplateRef {
    /// 模板稳定 id。
    pub template_id: TemplateId,
    /// 卷型 id（目录名）。
    pub set_id: String,
    /// 中文卷型名。
    pub display_name: String,
    /// 模板版本。
    pub version: TemplateVersion,
    /// 模板状态（含停用年份）。
    pub status: TemplateStatus,
}

/// 草稿伴随记录：领域 [`RubricDraft`] + 尚未进入领域的模式/部分给分/模板引用。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StoredDraft {
    /// 领域草稿（状态机与校验结果都在其中）。
    pub draft: RubricDraft,
    /// 题目题型（静态校验 R08/P07 与确认时 `Rubric::validate` 的输入）。
    pub question_kind: QuestionKind,
    /// 题目满分（静态校验 R01/P03/P08 的基准）。
    pub max_score: MilliScore,
    /// 计分方式（领域草稿未承载；确认时写入 `Rubric.mode`）。
    pub mode: ScoringMode,
    /// 部分给分档位（模式为部分给分时必填，其余必须为空）。
    pub partial_choice: Option<PartialChoiceRule>,
    /// 模板引用（仅模板套用来源需要）。
    pub template: Option<TemplateRef>,
}

impl StoredDraft {
    /// 是否终态（已确认/已拒绝不可编辑）。
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.draft.status.is_terminal()
    }
}

/// 规则模块审计动作（`SPEC-rubric.md` §5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RubricAction {
    /// 创建草稿（手写/导入）。
    DraftCreated,
    /// 规则整理（模型整理）调用完成并产出草稿。
    ModelStructured,
    /// 套用模板生成草稿。
    TemplateApplied,
    /// 编辑草稿评分项。
    ItemsEdited,
    /// 静态校验。
    ValidationRun,
    /// 提交待确认。
    SubmittedForConfirmation,
    /// 教师确认（生成生效规则、版本递增）。
    Confirmed,
    /// 教师拒绝。
    Rejected,
}

/// 审计事件（经 [`RubricAudit`] 落库；审计恒开，写失败即拒绝业务写）。
#[derive(Debug, Clone, PartialEq)]
pub struct RubricAuditEvent {
    /// 动作。
    pub action: RubricAction,
    /// 草稿 id。
    pub draft_id: RubricId,
    /// 题目 id。
    pub question_id: QuestionId,
    /// 操作者（v1 固定 [`eg_domain::LOCAL_TEACHER`]）。
    pub actor: String,
    /// 事件时间（注入时钟）。
    pub at: Timestamp,
    /// 本次操作后的生效规则版本（仅确认时填写）。
    pub rubric_version: Option<u32>,
    /// 模板引用（模板套用/模板来源确认时填写）。
    pub template: Option<TemplateRef>,
    /// 操作前内容哈希（编辑/确认）。
    pub before_hash: Option<Sha256Hex>,
    /// 操作后内容哈希（编辑/确认）。
    pub after_hash: Option<Sha256Hex>,
    /// 模型调用 id（规则整理等模型来源）。
    pub model_call_id: Option<ModelCallId>,
    /// 提示词哈希（规则整理：本模块渲染的提示词；用于复现）。
    pub prompt_hash: Option<Sha256Hex>,
    /// 附加说明（拒绝理由、校验摘要等）。
    pub note: Option<String>,
}

/// 审计事件构造器（避免调用方逐字段拼装）。
#[derive(Debug, Clone)]
pub struct NewRubricAuditEvent {
    /// 动作。
    pub action: RubricAction,
    /// 草稿 id。
    pub draft_id: RubricId,
    /// 题目 id。
    pub question_id: QuestionId,
    /// 操作者。
    pub actor: String,
    /// 事件时间。
    pub at: Timestamp,
    /// 生效规则版本。
    pub rubric_version: Option<u32>,
    /// 模板引用。
    pub template: Option<TemplateRef>,
    /// 操作前哈希。
    pub before_hash: Option<Sha256Hex>,
    /// 操作后哈希。
    pub after_hash: Option<Sha256Hex>,
    /// 模型调用 id。
    pub model_call_id: Option<ModelCallId>,
    /// 提示词哈希。
    pub prompt_hash: Option<Sha256Hex>,
    /// 说明。
    pub note: Option<String>,
}

impl NewRubricAuditEvent {
    /// 构造最小事件（动作 + 草稿 + 题目 + 操作者 + 时间）。
    #[must_use]
    pub fn new(
        action: RubricAction,
        draft_id: RubricId,
        question_id: QuestionId,
        actor: impl Into<String>,
        at: Timestamp,
    ) -> Self {
        Self {
            action,
            draft_id,
            question_id,
            actor: actor.into(),
            at,
            rubric_version: None,
            template: None,
            before_hash: None,
            after_hash: None,
            model_call_id: None,
            prompt_hash: None,
            note: None,
        }
    }

    /// 附带生效规则版本。
    #[must_use]
    pub const fn with_version(mut self, version: u32) -> Self {
        self.rubric_version = Some(version);
        self
    }

    /// 附带模板引用。
    #[must_use]
    pub fn with_template(mut self, template: Option<TemplateRef>) -> Self {
        self.template = template;
        self
    }

    /// 附带前后内容哈希。
    #[must_use]
    pub const fn with_hashes(
        mut self,
        before: Option<Sha256Hex>,
        after: Option<Sha256Hex>,
    ) -> Self {
        self.before_hash = before;
        self.after_hash = after;
        self
    }

    /// 附带模型调用 id（规则整理/模型整理来源）。
    #[must_use]
    pub const fn with_model_call(mut self, model_call_id: ModelCallId) -> Self {
        self.model_call_id = Some(model_call_id);
        self
    }

    /// 附带提示词哈希（规则整理复现用）。
    #[must_use]
    pub const fn with_prompt_hash(mut self, prompt_hash: Sha256Hex) -> Self {
        self.prompt_hash = Some(prompt_hash);
        self
    }

    /// 附带说明。
    #[must_use]
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    /// 转为审计事件。
    #[must_use]
    pub fn into_event(self) -> RubricAuditEvent {
        RubricAuditEvent {
            action: self.action,
            draft_id: self.draft_id,
            question_id: self.question_id,
            actor: self.actor,
            at: self.at,
            rubric_version: self.rubric_version,
            template: self.template,
            before_hash: self.before_hash,
            after_hash: self.after_hash,
            model_call_id: self.model_call_id,
            prompt_hash: self.prompt_hash,
            note: self.note,
        }
    }
}

/// 草稿与生效规则的持久化端口（平台实现前用内存适配器）。
pub trait RubricStore: Send + Sync {
    /// 读取草稿。
    ///
    /// # Errors
    ///
    /// 存储错误返回 [`RubricError::Store`]。
    fn load_draft(&self, draft_id: RubricId) -> Result<Option<StoredDraft>>;

    /// 保存草稿（同 id 覆盖为最新状态；历史由审计承载）。
    ///
    /// # Errors
    ///
    /// 存储错误返回 [`RubricError::Store`]。
    fn save_draft(&self, draft: &StoredDraft) -> Result<()>;

    /// 列出某题目的全部草稿（按创建时间先后）。
    ///
    /// # Errors
    ///
    /// 存储错误返回 [`RubricError::Store`]。
    fn list_drafts(&self, question_id: QuestionId) -> Result<Vec<StoredDraft>>;

    /// 读取当前生效规则。
    ///
    /// # Errors
    ///
    /// 存储错误返回 [`RubricError::Store`]。
    fn active_rubric(&self, question_id: QuestionId) -> Result<Option<Rubric>>;

    /// 当前生效规则版本（无生效规则时为 0）。
    ///
    /// # Errors
    ///
    /// 存储错误返回 [`RubricError::Store`]。
    fn rubric_version(&self, question_id: QuestionId) -> Result<u32>;

    /// 以 CAS 方式保存生效规则：仅当存储版本等于 `expected_version` 时写入并递增。
    ///
    /// 返回新版本号；版本冲突返回 [`RubricError::VersionConflict`]。
    ///
    /// # Errors
    ///
    /// 版本冲突或存储错误。
    fn save_active_rubric(
        &self,
        question_id: QuestionId,
        rubric: &Rubric,
        expected_version: u32,
    ) -> Result<u32>;
}

/// 审计端口（只追加；审计写失败即拒绝业务写）。
pub trait RubricAudit: Send + Sync {
    /// 追加一条审计事件。
    ///
    /// # Errors
    ///
    /// 审计存储错误返回 [`RubricError::Audit`]。
    fn append(&self, event: NewRubricAuditEvent) -> Result<RubricAuditEvent>;
}

/// id 生成端口（测试用确定性序列；生产接 `platform::UlidFactory`，TODO(T12)）。
pub trait RubricIdSource: Send + Sync {
    /// 生成草稿/规则 id。
    fn next_rubric_id(&self) -> RubricId;

    /// 生成评分项 id。
    fn next_rubric_item_id(&self) -> RubricItemId;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn template_version_roundtrip_and_encoding() {
        let version = TemplateVersion::parse("2026.1").unwrap();
        assert_eq!(version.snapshot_year, 2026);
        assert_eq!(version.revision, 1);
        assert_eq!(version.as_u32(), 2_026_001);
        assert_eq!(version.to_string(), "2026.1");

        assert!(TemplateVersion::parse("2026").is_err());
        assert!(TemplateVersion::parse("2026.x").is_err());
        assert!(TemplateVersion::new(1999, 1).is_err());
        assert!(TemplateVersion::new(2026, 1000).is_err());
    }

    #[test]
    fn settings_defaults_match_config() {
        let settings = RubricSettings::default();
        assert_eq!(settings.granularity.as_milli(), 100);
        assert!(settings.partial_choice_enabled);
        assert!(settings.auto_validate);
        assert!(settings.allow_custom_template);
        assert_eq!(settings.supported_kinds.len(), 10);
        assert!(settings.validate().is_ok());

        let mut bad = settings;
        bad.granularity = MilliScore::ZERO;
        assert!(bad.validate().is_err());
    }

    #[test]
    fn status_helpers() {
        assert!(TemplateStatus::Active.is_active());
        assert_eq!(TemplateStatus::Active.retired_since(), None);
        let retired = TemplateStatus::Retired {
            retired_since: 2026,
        };
        assert!(!retired.is_active());
        assert_eq!(retired.retired_since(), Some(2026));
    }
}
