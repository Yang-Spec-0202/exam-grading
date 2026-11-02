//! 集成测试公共设施：内存适配器 + 夹具读取 + 草稿构造。
//!
//! 只使用 `eg-rubric` 的公开 API（与 `app` 的接线方式一致），
//! 平台接入后本文件可直接替换为真实仓储/审计实现。

#![allow(dead_code, unreachable_pub, clippy::assigning_clones)]

use std::path::PathBuf;

use eg_domain::{
    AnswerKey, DraftOrigin, DraftStatus, MilliScore, PaperId, PartialChoiceRule, Question,
    QuestionId, QuestionKind, RubricDraft, RubricDraftItem, RubricId, ScoringMode, Timestamp, Ulid,
};
use eg_rubric::{
    FixedClock, InMemoryRubricStore, RecordingAudit, RubricEngine, RubricSettings, SequentialIds,
    StoredDraft,
};
use serde::Deserialize;

/// 固定测试时间点（审计与 `confirmed_at`）。
pub fn timestamp() -> Timestamp {
    eg_domain::time::parse_rfc3339("2026-11-02T00:00:00Z").expect("时间夹具必须合法")
}

/// 内存依赖集合（引擎按引用借用）。
pub struct Harness {
    pub store: InMemoryRubricStore,
    pub audit: RecordingAudit,
    pub clock: FixedClock,
    pub ids: SequentialIds,
    pub settings: RubricSettings,
}

impl Default for Harness {
    fn default() -> Self {
        Self {
            store: InMemoryRubricStore::default(),
            audit: RecordingAudit::default(),
            clock: FixedClock::new(timestamp()),
            ids: SequentialIds::default(),
            settings: RubricSettings::default(),
        }
    }
}

impl Harness {
    /// 用默认配置构造引擎。
    pub fn engine(&self) -> RubricEngine<'_> {
        RubricEngine::new(
            &self.store,
            &self.audit,
            &self.clock,
            &self.ids,
            &self.settings,
        )
        .expect("默认配置必须合法")
    }
}

/// 用给定配置构造引擎（测试个别配置项，如部分给分开关、支持题型）。
pub fn engine_with<'a>(harness: &'a Harness, settings: &'a RubricSettings) -> RubricEngine<'a> {
    RubricEngine::new(
        &harness.store,
        &harness.audit,
        &harness.clock,
        &harness.ids,
        settings,
    )
    .expect("测试配置必须合法")
}

/// 构造题目（`ordinal` 与题目 id 由序号派生，保证每个用例独立）。
pub fn question(kind: QuestionKind, max_milli: i64, ordinal: u32) -> Question {
    Question {
        id: QuestionId::from_ulid(Ulid::from_u128(u128::from(100 + ordinal))),
        paper_id: PaperId::from_ulid(Ulid::from_u128(8)),
        ordinal,
        label: ordinal.to_string(),
        kind,
        max_score: MilliScore::from_milli(max_milli).expect("满分必须合法"),
        stem: None,
        key: AnswerKey::Missing,
        rubric: None,
        subject_hint: None,
        knowledge_tags: Vec::new(),
    }
}

/// 夹具根目录。
pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

/// 读取 JSON 夹具。
pub fn load_json<T: for<'de> Deserialize<'de>>(name: &str) -> T {
    let path = fixtures_dir().join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("读取夹具 {} 失败：{error}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("解析夹具 {} 失败：{error}", path.display()))
}

/// 仓库内置全国卷模板根目录。
pub fn template_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/templates")
}

/// 期望的校验结果（错误码/警告码）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Expect {
    #[serde(default)]
    pub errors: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// 用例级配置覆盖。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SettingsFixture {
    #[serde(default)]
    pub partial_choice_enabled: Option<bool>,
    #[serde(default)]
    pub supported_kinds: Option<Vec<QuestionKind>>,
    #[serde(default)]
    pub granularity_milli: Option<i64>,
}

impl SettingsFixture {
    /// 套用到默认配置上。
    pub fn apply(&self) -> RubricSettings {
        let mut settings = RubricSettings::default();
        if let Some(enabled) = self.partial_choice_enabled {
            settings.partial_choice_enabled = enabled;
        }
        if let Some(kinds) = &self.supported_kinds {
            settings.supported_kinds = kinds.clone();
        }
        if let Some(granularity) = self.granularity_milli {
            settings.granularity = MilliScore::from_milli(granularity).expect("夹具粒度必须合法");
        }
        settings
    }
}

/// 单条规则夹具。
#[derive(Debug, Clone, Deserialize)]
pub struct RuleFixture {
    pub name: String,
    pub kind: QuestionKind,
    pub max_score_milli: i64,
    pub mode: ScoringMode,
    #[serde(default)]
    pub origin: Option<String>,
    pub items: Vec<RubricDraftItem>,
    #[serde(default)]
    pub partial_choice: Option<PartialChoiceRule>,
    #[serde(default)]
    pub settings: Option<SettingsFixture>,
    pub expect: Expect,
}

impl RuleFixture {
    /// 期望值套用后的配置。
    pub fn settings(&self) -> RubricSettings {
        self.settings
            .as_ref()
            .map_or_else(RubricSettings::default, SettingsFixture::apply)
    }
}

/// 规则夹具文件。
#[derive(Debug, Clone, Deserialize)]
pub struct RuleFile {
    pub rules: Vec<RuleFixture>,
}

/// 读取规则夹具文件。
pub fn load_rules(name: &str) -> Vec<RuleFixture> {
    load_json::<RuleFile>(name).rules
}

/// 手搓一份草稿伴随记录（可直接验证期望的校验码，不经过服务层）。
pub fn stored_draft(fixture: &RuleFixture, ordinal: u32) -> StoredDraft {
    let origin = match fixture.origin.as_deref() {
        Some("template_applied") => DraftOrigin::TemplateApplied,
        Some("model_structured") => DraftOrigin::ModelStructured,
        _ => DraftOrigin::HandwrittenTransliteration,
    };
    StoredDraft {
        draft: RubricDraft {
            id: RubricId::from_ulid(Ulid::from_u128(u128::from(1000 + ordinal))),
            question_id: QuestionId::from_ulid(Ulid::from_u128(u128::from(100 + ordinal))),
            status: DraftStatus::PendingConfirmation,
            items: fixture.items.clone(),
            raw_text: fixture.name.clone(),
            origin,
            model_call_id: None,
            validation: Vec::new(),
            created_at: timestamp(),
            confirmed_at: None,
        },
        question_kind: fixture.kind,
        max_score: MilliScore::from_milli(fixture.max_score_milli).expect("满分必须合法"),
        mode: fixture.mode,
        partial_choice: fixture.partial_choice.clone(),
        template: None,
    }
}
