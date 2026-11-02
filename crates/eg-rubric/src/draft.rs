//! 草稿服务与状态机（`SPEC-rubric.md` §2.1/§3.1/§3.2，T15）。
//!
//! 三来源：手写直译（含导入的结构化项）、标准模板套用、模型整理（T25 接入）。
//! 四状态：草稿 → 待确认 → 已确认（终态）/ 已拒绝（终态）；已确认不可编辑，
//! 修改必须新建草稿并再次确认（`SPEC-domain.md` §5 不变量）。
//!
//! 本模块只做「草稿怎么来、是否合法」，不生成生效规则——生效只能由
//! [`crate::confirm::RubricEngine::confirm`] 在教师显式确认下产生。

use eg_domain::{
    Clock, DraftOrigin, DraftStatus, ModelCallId, Question, QuestionKind, RubricDraft,
    RubricDraftItem, RubricId, ScoringMode, Sha256Hex,
};

use crate::error::{Result, RubricError};
use crate::ports::{
    NewRubricAuditEvent, RubricAction, RubricAudit, RubricIdSource, RubricSettings, RubricStore,
    StoredDraft, TemplateRef, TemplateStatus,
};
use crate::templates::LoadedTemplate;
use crate::validate::{ValidationContext, ValidationReport, validate_draft};

/// 草稿来源（决定 `DraftOrigin`、模板引用与审计动作）。
#[derive(Debug)]
pub enum DraftSource<'a> {
    /// 教师手写并直接录入评分项（手写直译）。
    Handwritten {
        /// 规则原文（可为空）。
        raw_text: String,
        /// 评分项（可为空，教师随后编辑）。
        items: Vec<RubricDraftItem>,
        /// 计分方式。
        mode: ScoringMode,
        /// 部分给分档位（仅部分给分模式）。
        partial_choice: Option<eg_domain::PartialChoiceRule>,
    },
    /// 导入的结构化规则（CSV/DOCX 纯文本经界面转成评分项；来源仍记为手写直译，
    /// 因为 `DraftOrigin` 只有「手写直译/模型整理/模板套用」三值，导入不引入新枚举）。
    Imported {
        /// 规则原文。
        raw_text: String,
        /// 评分项。
        items: Vec<RubricDraftItem>,
        /// 计分方式。
        mode: ScoringMode,
        /// 部分给分档位。
        partial_choice: Option<eg_domain::PartialChoiceRule>,
    },
    /// 标准模板套用（全国卷模板包或自定义模板）。
    Template {
        /// 已加载并校验的模板。
        template: &'a LoadedTemplate,
        /// 是否允许套用 retired 历史卷。
        allow_retired: bool,
        /// 教师按 `kind_note` 改换的骨架 id（可空 = 用模板默认）。
        skeleton_override: Option<String>,
    },
    /// 模型整理产出（T25 接入 structurer 后启用；T15 仅供状态机与审计演练）。
    ModelStructured {
        /// 规则原文。
        raw_text: String,
        /// 模型整理的评分项。
        items: Vec<RubricDraftItem>,
        /// 计分方式。
        mode: ScoringMode,
        /// 部分给分档位。
        partial_choice: Option<eg_domain::PartialChoiceRule>,
        /// 模型调用 id（`SPEC-domain.md` §5：模型整理时必填，便于复现）。
        model_call_id: ModelCallId,
    },
}

/// 规则引擎：草稿/校验/确认共用的依赖集合。
///
/// 生命周期约定：`store`/`audit`/`clock`/`ids` 由组合根（`app`）注入，
/// 平台实现未就绪时使用 [`crate::memory`] 的内存适配器。
pub struct RubricEngine<'a> {
    store: &'a dyn RubricStore,
    audit: &'a dyn RubricAudit,
    clock: &'a dyn Clock,
    ids: &'a dyn RubricIdSource,
    settings: &'a RubricSettings,
}

impl core::fmt::Debug for RubricEngine<'_> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("RubricEngine")
            .field("settings", self.settings)
            .finish_non_exhaustive()
    }
}

impl<'a> RubricEngine<'a> {
    /// 构造引擎（校验配置：粒度必须为正）。
    ///
    /// # Errors
    ///
    /// 配置不合法返回 [`RubricError::Config`]。
    pub fn new(
        store: &'a dyn RubricStore,
        audit: &'a dyn RubricAudit,
        clock: &'a dyn Clock,
        ids: &'a dyn RubricIdSource,
        settings: &'a RubricSettings,
    ) -> Result<Self> {
        settings.validate()?;
        Ok(Self {
            store,
            audit,
            clock,
            ids,
            settings,
        })
    }

    /// 当前配置。
    #[must_use]
    pub const fn settings(&self) -> &RubricSettings {
        self.settings
    }

    /// 创建草稿（手写/导入/模板套用/模型整理）。
    ///
    /// # Errors
    ///
    /// 模板 retired 未允许、题号缺失、题型不匹配、模板数据折算失败、
    /// 领域草稿校验失败、存储或审计失败。
    pub fn create_draft(
        &self,
        question: &Question,
        source: DraftSource<'_>,
    ) -> Result<StoredDraft> {
        let (draft, mode, partial_choice, template) = match source {
            DraftSource::Handwritten {
                raw_text,
                items,
                mode,
                partial_choice,
            }
            | DraftSource::Imported {
                raw_text,
                items,
                mode,
                partial_choice,
            } => (
                self.assemble_draft(
                    question,
                    raw_text,
                    items,
                    DraftOrigin::HandwrittenTransliteration,
                    None,
                ),
                mode,
                partial_choice,
                None,
            ),
            DraftSource::Template {
                template,
                allow_retired,
                skeleton_override,
            } => {
                if let TemplateStatus::Retired { retired_since } = template.status
                    && !allow_retired
                {
                    return Err(RubricError::TemplateRetired {
                        set_id: template.set_id.clone(),
                        retired_since,
                    });
                }
                let plan = template.plan(
                    question.ordinal,
                    question.kind,
                    skeleton_override.as_deref(),
                    self.settings,
                )?;
                let raw_text = format!(
                    "模板套用：{}（{}）v{} 骨架 {}",
                    template.display_name, template.set_id, template.version, plan.skeleton_id
                );
                (
                    self.assemble_draft(
                        question,
                        raw_text,
                        plan.items,
                        DraftOrigin::TemplateApplied,
                        None,
                    ),
                    plan.mode,
                    plan.partial_choice,
                    Some(template.as_ref()),
                )
            }
            DraftSource::ModelStructured {
                raw_text,
                items,
                mode,
                partial_choice,
                model_call_id,
            } => (
                self.assemble_draft(
                    question,
                    raw_text,
                    items,
                    DraftOrigin::ModelStructured,
                    Some(model_call_id),
                ),
                mode,
                partial_choice,
                None,
            ),
        };

        let mut stored = StoredDraft {
            draft,
            question_kind: question.kind,
            max_score: question.max_score,
            mode,
            partial_choice,
            template,
        };
        // 领域层先校验草稿自身（模型来源必须有 model_call_id 等）。
        stored.draft.validate()?;

        if self.settings.auto_validate {
            let report = self.run_validation(&stored);
            stored.draft.validation = report.issues();
            if !report.is_ok() {
                stored.draft.status = DraftStatus::Draft;
            }
        }

        self.persist_new(&stored, draft_action(&stored))?;
        Ok(stored)
    }

    fn assemble_draft(
        &self,
        question: &Question,
        raw_text: String,
        items: Vec<RubricDraftItem>,
        origin: DraftOrigin,
        model_call_id: Option<ModelCallId>,
    ) -> RubricDraft {
        RubricDraft {
            id: self.ids.next_rubric_id(),
            question_id: question.id,
            status: DraftStatus::Draft,
            items,
            raw_text,
            origin,
            model_call_id,
            validation: Vec::new(),
            created_at: self.clock.now(),
            confirmed_at: None,
        }
    }

    fn persist_new(&self, stored: &StoredDraft, action: RubricAction) -> Result<()> {
        let after = content_hash(stored)?;
        // TODO(T12/T18)：平台接入后，草稿写入与审计追加必须同事务；当前先写草稿再审计。
        self.store.save_draft(stored)?;
        let event = NewRubricAuditEvent::new(
            action,
            stored.draft.id,
            stored.draft.question_id,
            eg_domain::LOCAL_TEACHER,
            self.clock.now(),
        )
        .with_hashes(None, Some(after))
        .with_template(stored.template.clone())
        .with_note(format!("来源 {:?}", stored.draft.origin));
        self.audit.append(event)?;
        Ok(())
    }

    /// 读取草稿。
    ///
    /// # Errors
    ///
    /// 不存在返回 [`RubricError::DraftNotFound`]；存储失败返回 [`RubricError::Store`]。
    pub fn load_draft(&self, draft_id: RubricId) -> Result<StoredDraft> {
        self.store
            .load_draft(draft_id)?
            .ok_or(RubricError::DraftNotFound(draft_id))
    }

    /// 列出某题目的全部草稿。
    ///
    /// # Errors
    ///
    /// 存储失败。
    pub fn list_drafts(&self, question_id: eg_domain::QuestionId) -> Result<Vec<StoredDraft>> {
        self.store.list_drafts(question_id)
    }

    /// 编辑草稿评分项（草稿/待确认可编辑；终态不可编辑，编辑即回到草稿态）。
    ///
    /// # Errors
    ///
    /// 终态、领域校验失败、存储或审计失败。
    pub fn update_items(
        &self,
        draft_id: RubricId,
        items: Vec<RubricDraftItem>,
        mode: ScoringMode,
        partial_choice: Option<eg_domain::PartialChoiceRule>,
        raw_text: Option<String>,
    ) -> Result<StoredDraft> {
        let mut stored = self.load_draft(draft_id)?;
        if stored.is_terminal() {
            return Err(RubricError::DraftNotEditable {
                status: stored.draft.status,
            });
        }
        let before = content_hash(&stored)?;
        stored.draft.items = items;
        stored.mode = mode;
        stored.partial_choice = partial_choice;
        if let Some(raw_text) = raw_text {
            stored.draft.raw_text = raw_text;
        }
        stored.draft.status = DraftStatus::Draft;
        stored.draft.confirmed_at = None;
        stored.draft.validate()?;
        if self.settings.auto_validate {
            let report = self.run_validation(&stored);
            stored.draft.validation = report.issues();
        }
        let after = content_hash(&stored)?;
        self.store.save_draft(&stored)?;
        self.audit.append(
            NewRubricAuditEvent::new(
                RubricAction::ItemsEdited,
                stored.draft.id,
                stored.draft.question_id,
                eg_domain::LOCAL_TEACHER,
                self.clock.now(),
            )
            .with_hashes(Some(before), Some(after)),
        )?;
        Ok(stored)
    }

    /// 运行静态校验并落库校验结果（`rubric.auto_validate` 关时用于手动校验）。
    ///
    /// # Errors
    ///
    /// 存储或审计失败。
    pub fn validate(&self, draft_id: RubricId) -> Result<(StoredDraft, ValidationReport)> {
        let mut stored = self.load_draft(draft_id)?;
        let before = content_hash(&stored)?;
        let report = self.run_validation(&stored);
        stored.draft.validation = report.issues();
        let after = content_hash(&stored)?;
        self.store.save_draft(&stored)?;
        self.audit.append(
            NewRubricAuditEvent::new(
                RubricAction::ValidationRun,
                stored.draft.id,
                stored.draft.question_id,
                eg_domain::LOCAL_TEACHER,
                self.clock.now(),
            )
            .with_hashes(Some(before), Some(after))
            .with_note(format!(
                "错误 {} 条、警告 {} 条；码 {}",
                report.error_count(),
                report.warning_count(),
                report.codes().join(",")
            )),
        )?;
        Ok((stored, report))
    }

    /// 提交待确认（错误级问题阻止进入待确认）。
    ///
    /// # Errors
    ///
    /// 终态、存在错误级问题、存储或审计失败。
    pub fn submit_for_confirmation(&self, draft_id: RubricId) -> Result<StoredDraft> {
        let mut stored = self.load_draft(draft_id)?;
        if stored.is_terminal() {
            return Err(RubricError::DraftNotEditable {
                status: stored.draft.status,
            });
        }
        let before = content_hash(&stored)?;
        // 必须用当前校验结果判定，不能信任历史 validation。
        let report = self.run_validation(&stored);
        stored.draft.validation = report.issues();
        if !report.is_ok() {
            self.store.save_draft(&stored)?;
            return Err(RubricError::DraftHasErrors {
                count: report.error_count(),
            });
        }
        stored.draft.status = DraftStatus::PendingConfirmation;
        let after = content_hash(&stored)?;
        self.store.save_draft(&stored)?;
        self.audit.append(
            NewRubricAuditEvent::new(
                RubricAction::SubmittedForConfirmation,
                stored.draft.id,
                stored.draft.question_id,
                eg_domain::LOCAL_TEACHER,
                self.clock.now(),
            )
            .with_hashes(Some(before), Some(after)),
        )?;
        Ok(stored)
    }

    /// 读取当前生效规则。
    ///
    /// # Errors
    ///
    /// 存储失败。
    pub fn active_rubric(
        &self,
        question_id: eg_domain::QuestionId,
    ) -> Result<Option<eg_domain::Rubric>> {
        self.store.active_rubric(question_id)
    }

    /// 计算草稿的内部校验报告（不做持久化）。
    #[must_use]
    pub fn report_for(&self, stored: &StoredDraft) -> ValidationReport {
        self.run_validation(stored)
    }

    fn run_validation(&self, stored: &StoredDraft) -> ValidationReport {
        let context = ValidationContext {
            question_kind: stored.question_kind,
            max_score: stored.max_score,
            settings: self.settings,
            template: stored.template.as_ref(),
        };
        validate_draft(
            &stored.draft,
            stored.mode,
            stored.partial_choice.as_ref(),
            &context,
        )
    }

    /// 暴露给确认流程使用（同 crate 模块间共享依赖）。
    pub(crate) const fn audit_sink(&self) -> &dyn RubricAudit {
        self.audit
    }

    pub(crate) const fn store_sink(&self) -> &dyn RubricStore {
        self.store
    }

    pub(crate) const fn clock_source(&self) -> &dyn Clock {
        self.clock
    }

    pub(crate) const fn id_source(&self) -> &dyn RubricIdSource {
        self.ids
    }
}

fn draft_action(stored: &StoredDraft) -> RubricAction {
    if stored.draft.origin == DraftOrigin::TemplateApplied {
        RubricAction::TemplateApplied
    } else {
        RubricAction::DraftCreated
    }
}

/// 内容哈希（审计前后比对；平台审计接入后用于哈希链）。
///
/// # Errors
///
/// 序列化失败返回 [`RubricError::Serialization`]。
pub(crate) fn content_hash(stored: &StoredDraft) -> Result<Sha256Hex> {
    let bytes = serde_json::to_vec(stored).map_err(|error| RubricError::Serialization {
        message: error.to_string(),
    })?;
    Ok(Sha256Hex::of(&bytes))
}

/// 供确认流程使用的模板引用解析（保证 `RubricOrigin::StandardTemplate` 可构造）。
///
/// # Errors
///
/// 模板来源草稿缺少模板引用时返回 [`RubricError::TemplateInvalid`]。
pub(crate) fn require_template_ref(stored: &StoredDraft) -> Result<TemplateRef> {
    stored
        .template
        .clone()
        .ok_or_else(|| RubricError::TemplateInvalid {
            message: "草稿来源为模板套用，但缺少模板 id/版本，无法确认".to_owned(),
        })
}

/// 供确认流程使用：多选题 + 部分给分的题型一致性（域校验的最后一道前置）。
///
/// # Errors
///
/// 部分给分用于非多选时返回 [`RubricError::Domain`]。
pub(crate) fn ensure_partial_choice_kind(stored: &StoredDraft) -> Result<()> {
    if stored.mode == ScoringMode::PartialChoice
        && stored.question_kind != QuestionKind::MultipleChoice
    {
        return Err(RubricError::Domain(
            eg_domain::DomainError::PartialChoiceKindUnsupported {
                kind: stored.question_kind,
            },
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::memory::{FixedClock, InMemoryRubricStore, RecordingAudit, SequentialIds};
    use crate::ports::{RubricSettings, RubricStore};
    use crate::templates::{TemplatePack, TemplateSelection, default_template_root};
    use eg_domain::time::parse_rfc3339;
    use eg_domain::{MilliScore, RubricId, Ulid};

    pub(crate) fn question(kind: QuestionKind, max_milli: i64) -> Question {
        use eg_domain::{AnswerKey, PaperId, QuestionId};
        Question {
            id: QuestionId::from_ulid(Ulid::from_u128(7)),
            paper_id: PaperId::from_ulid(Ulid::from_u128(8)),
            ordinal: 1,
            label: "1".to_owned(),
            kind,
            max_score: MilliScore::from_milli(max_milli).unwrap(),
            stem: None,
            key: AnswerKey::Missing,
            rubric: None,
            subject_hint: None,
            knowledge_tags: Vec::new(),
        }
    }

    fn item(criterion: &str, milli: i64) -> RubricDraftItem {
        RubricDraftItem {
            criterion: criterion.to_owned(),
            points: MilliScore::from_milli(milli).unwrap(),
            required: false,
            depends_on: Vec::new(),
        }
    }

    struct Fixture {
        store: InMemoryRubricStore,
        audit: RecordingAudit,
        clock: FixedClock,
        ids: SequentialIds,
        settings: RubricSettings,
    }

    fn fixture() -> Fixture {
        Fixture {
            store: InMemoryRubricStore::default(),
            audit: RecordingAudit::default(),
            clock: FixedClock::new(parse_rfc3339("2026-11-02T00:00:00Z").unwrap()),
            ids: SequentialIds::default(),
            settings: RubricSettings::default(),
        }
    }

    #[test]
    fn handwritten_draft_flow_and_state_machine() {
        let fixture = fixture();
        let engine = RubricEngine::new(
            &fixture.store,
            &fixture.audit,
            &fixture.clock,
            &fixture.ids,
            &fixture.settings,
        )
        .unwrap();
        let question = question(QuestionKind::Calculation, 3000);
        let draft = engine
            .create_draft(
                &question,
                DraftSource::Handwritten {
                    raw_text: "列出方程 1.5 分，解得 x=2 1.5 分".to_owned(),
                    items: vec![item("列出方程", 1500), item("解得 x=2", 1500)],
                    mode: ScoringMode::Additive,
                    partial_choice: None,
                },
            )
            .unwrap();
        assert_eq!(draft.draft.status, DraftStatus::Draft);
        assert_eq!(draft.draft.validation, Vec::new());
        assert_eq!(fixture.audit.events().len(), 1);

        let draft = engine.submit_for_confirmation(draft.draft.id).unwrap();
        assert_eq!(draft.draft.status, DraftStatus::PendingConfirmation);

        // 待确认仍可编辑，编辑后回到草稿态。
        let draft = engine
            .update_items(
                draft.draft.id,
                vec![item("列出方程", 3000)],
                ScoringMode::Additive,
                None,
                None,
            )
            .unwrap();
        assert_eq!(draft.draft.status, DraftStatus::Draft);
        assert!(
            draft
                .draft
                .validation
                .iter()
                .all(|issue| { issue.severity != eg_domain::RubricIssueSeverity::Error })
        );
    }

    #[test]
    fn bad_draft_cannot_be_submitted() {
        let fixture = fixture();
        let engine = RubricEngine::new(
            &fixture.store,
            &fixture.audit,
            &fixture.clock,
            &fixture.ids,
            &fixture.settings,
        )
        .unwrap();
        let question = question(QuestionKind::Calculation, 3000);
        let draft = engine
            .create_draft(
                &question,
                DraftSource::Handwritten {
                    raw_text: "漏了分值".to_owned(),
                    items: vec![item("只有一步", 1000)],
                    mode: ScoringMode::Additive,
                    partial_choice: None,
                },
            )
            .unwrap();
        assert_ne!(draft.draft.validation, Vec::new());
        let error = engine.submit_for_confirmation(draft.draft.id).unwrap_err();
        assert!(matches!(error, RubricError::DraftHasErrors { .. }));
    }

    #[test]
    fn template_draft_keeps_placeholder_and_blocks_submit() {
        let fixture = fixture();
        let engine = RubricEngine::new(
            &fixture.store,
            &fixture.audit,
            &fixture.clock,
            &fixture.ids,
            &fixture.settings,
        )
        .unwrap();
        let pack = TemplatePack::load(&default_template_root().unwrap()).unwrap();
        let template = pack
            .resolve(&TemplateSelection {
                key: "new-curriculum-1".to_owned(),
                allow_retired: false,
            })
            .unwrap();
        let mut question = question(QuestionKind::SingleChoice, 5000);
        question.ordinal = 1;
        question.label = "1".to_owned();
        let draft = engine
            .create_draft(
                &question,
                DraftSource::Template {
                    template,
                    allow_retired: false,
                    skeleton_override: None,
                },
            )
            .unwrap();
        assert_eq!(draft.draft.origin, DraftOrigin::TemplateApplied);
        assert!(draft.template.is_some());
        assert!(
            draft
                .draft
                .validation
                .iter()
                .any(|issue| { issue.message.contains("R07") })
        );
        assert!(matches!(
            engine.submit_for_confirmation(draft.draft.id).unwrap_err(),
            RubricError::DraftHasErrors { .. }
        ));
        assert_eq!(fixture.audit.events().len(), 1);
        assert_eq!(
            fixture.audit.events()[0].action,
            RubricAction::TemplateApplied
        );
    }

    #[test]
    fn template_retired_requires_explicit_allow() {
        let fixture = fixture();
        let engine = RubricEngine::new(
            &fixture.store,
            &fixture.audit,
            &fixture.clock,
            &fixture.ids,
            &fixture.settings,
        )
        .unwrap();
        let pack = TemplatePack::load(&default_template_root().unwrap()).unwrap();
        let template = pack.find("national-jia").unwrap();
        let mut question = question(QuestionKind::SingleChoice, 5000);
        question.ordinal = 1;
        let error = engine
            .create_draft(
                &question,
                DraftSource::Template {
                    template,
                    allow_retired: false,
                    skeleton_override: None,
                },
            )
            .unwrap_err();
        assert!(matches!(error, RubricError::TemplateRetired { .. }));
    }

    #[test]
    fn terminal_draft_is_not_editable() {
        let fixture = fixture();
        let engine = RubricEngine::new(
            &fixture.store,
            &fixture.audit,
            &fixture.clock,
            &fixture.ids,
            &fixture.settings,
        )
        .unwrap();
        let question = question(QuestionKind::Calculation, 1000);
        let draft = engine
            .create_draft(
                &question,
                DraftSource::Handwritten {
                    raw_text: String::new(),
                    items: vec![item("一步", 1000)],
                    mode: ScoringMode::AllOrNothing,
                    partial_choice: None,
                },
            )
            .unwrap();
        let mut confirmed = draft.clone();
        confirmed.draft.status = DraftStatus::Confirmed;
        confirmed.draft.confirmed_at = Some(parse_rfc3339("2026-11-02T01:00:00Z").unwrap());
        fixture.store.save_draft(&confirmed).unwrap();
        let error = engine
            .update_items(
                draft.draft.id,
                Vec::new(),
                ScoringMode::Additive,
                None,
                None,
            )
            .unwrap_err();
        assert!(matches!(error, RubricError::DraftNotEditable { .. }));
    }

    #[test]
    fn model_structured_requires_model_call_id() {
        let fixture = fixture();
        let engine = RubricEngine::new(
            &fixture.store,
            &fixture.audit,
            &fixture.clock,
            &fixture.ids,
            &fixture.settings,
        )
        .unwrap();
        let question = question(QuestionKind::Calculation, 1000);
        let draft = engine
            .create_draft(
                &question,
                DraftSource::ModelStructured {
                    raw_text: "每步 1 分".to_owned(),
                    items: vec![item("一步", 1000)],
                    mode: ScoringMode::Additive,
                    partial_choice: None,
                    model_call_id: ModelCallId::from_ulid(Ulid::from_u128(9)),
                },
            )
            .unwrap();
        assert_eq!(draft.draft.origin, DraftOrigin::ModelStructured);
        assert!(draft.draft.model_call_id.is_some());
        // T25 之前只演练状态机，不生成生效规则。
        assert!(engine.active_rubric(question.id).unwrap().is_none());
    }

    #[test]
    fn missing_draft_is_reported() {
        let fixture = fixture();
        let engine = RubricEngine::new(
            &fixture.store,
            &fixture.audit,
            &fixture.clock,
            &fixture.ids,
            &fixture.settings,
        )
        .unwrap();
        let error = engine
            .load_draft(RubricId::from_ulid(Ulid::from_u128(999)))
            .unwrap_err();
        assert!(matches!(error, RubricError::DraftNotFound(_)));
    }
}
