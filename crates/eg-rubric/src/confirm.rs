//! 确认、拒绝与版本化（`SPEC-rubric.md` §2.5/§3.3，T15）。
//!
//! 恒开不变量：生效 `Rubric` **只能**由教师显式确认产生，没有「自动生效/批量放行」入口；
//! 确认前必须重跑全部静态校验（不信任草稿里历史 validation）、校验通过、状态为待确认、
//! 且试卷规则版本未被并发推进。确认成功即：生成新版本 `Rubric` + 递增版本 + 草稿转已确认 + 写审计。
//!
//! 版本语义偏差（记录于 `docs/dev/research/rubric.md`）：`SPEC-rubric.md` 以
//! `Paper.rubric_version` 为基准，但本模块的存储端口按题目维度提供 CAS；
//! 平台接入时应改为「试卷级版本 + 题目规则内容」的 CAS（TODO(T12/T18)）。

use eg_domain::{
    DomainError, DraftOrigin, DraftStatus, MilliScore, Rubric, RubricItem, RubricOrigin,
};

use crate::draft::{RubricEngine, content_hash, ensure_partial_choice_kind, require_template_ref};
use crate::error::{Result, RubricError};
use crate::ports::NewRubricAuditEvent;

/// 确认成功后的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct ConfirmOutcome {
    /// 新生成的生效规则（版本 = 确认前版本 + 1）。
    pub rubric: Rubric,
    /// 转「已确认」后的草稿（终态）。
    pub draft: crate::ports::StoredDraft,
    /// 确认前的规则版本（无生效规则时为 0）。
    pub previous_version: u32,
}

/// 拒绝成功后的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct RejectOutcome {
    /// 转「已拒绝」后的草稿（终态）。
    pub draft: crate::ports::StoredDraft,
    /// 拒绝理由（写入审计）。
    pub reason: String,
}

impl RubricEngine<'_> {
    /// 教师确认：校验通过才生成生效规则并递增版本。
    ///
    /// `expected_version` 为调用方读取草稿时看到的规则版本（乐观并发）；传 `None` 表示
    /// 不检查版本（单机单教师交互场景仍建议传入，避免与后台任务互相覆盖）。
    ///
    /// # Errors
    ///
    /// 草稿不存在、非待确认、存在错误级问题、版本冲突、领域校验失败、存储或审计失败。
    pub fn confirm(
        &self,
        draft_id: eg_domain::RubricId,
        expected_version: Option<u32>,
    ) -> Result<ConfirmOutcome> {
        let mut stored = self.load_draft(draft_id)?;
        if stored.draft.status == DraftStatus::Confirmed
            || stored.draft.status == DraftStatus::Rejected
        {
            return Err(RubricError::DraftWrongStatus {
                status: stored.draft.status,
            });
        }
        if stored.draft.status != DraftStatus::PendingConfirmation {
            return Err(RubricError::DraftWrongStatus {
                status: stored.draft.status,
            });
        }

        // 重新跑静态校验：不信任草稿里的历史 validation（可能来自旧配置）。
        let report = self.report_for(&stored);
        if !report.is_ok() {
            stored.draft.validation = report.issues();
            self.store_sink().save_draft(&stored)?;
            return Err(RubricError::DraftHasErrors {
                count: report.error_count(),
            });
        }

        // 领域层最终门禁：确保与 `SPEC-domain.md` 不变量一致。
        stored
            .draft
            .ensure_confirmable()
            .map_err(map_domain_error)?;
        ensure_partial_choice_kind(&stored)?;

        let current = self.store_sink().rubric_version(stored.draft.question_id)?;
        if let Some(expected) = expected_version
            && expected != current
        {
            return Err(RubricError::VersionConflict {
                expected,
                actual: current,
            });
        }
        let new_version = current.saturating_add(1);
        let rubric = self.build_rubric(&stored, new_version)?;
        rubric
            .validate_with_granularity(stored.question_kind, Some(self.settings().granularity))?;

        let before = content_hash(&stored)?;
        let actual =
            self.store_sink()
                .save_active_rubric(stored.draft.question_id, &rubric, current)?;
        if actual != new_version {
            return Err(RubricError::config(format!(
                "存储返回版本 {actual} 与本次确认版本 {new_version} 不一致"
            )));
        }

        stored.draft.status = DraftStatus::Confirmed;
        stored.draft.confirmed_at = Some(self.clock_source().now());
        let after = content_hash(&stored)?;
        self.store_sink().save_draft(&stored)?;
        self.audit_sink().append(
            NewRubricAuditEvent::new(
                crate::ports::RubricAction::Confirmed,
                stored.draft.id,
                stored.draft.question_id,
                eg_domain::LOCAL_TEACHER,
                self.clock_source().now(),
            )
            .with_version(new_version)
            .with_template(stored.template.clone())
            .with_hashes(Some(before), Some(after)),
        )?;

        Ok(ConfirmOutcome {
            rubric,
            draft: stored,
            previous_version: current,
        })
    }

    /// 教师拒绝草稿（保留草稿与理由，写审计；终态不可再编辑）。
    ///
    /// # Errors
    ///
    /// 草稿不存在、已确认/已拒绝、理由为空、存储或审计失败。
    pub fn reject(&self, draft_id: eg_domain::RubricId, reason: &str) -> Result<RejectOutcome> {
        if reason.trim().is_empty() {
            return Err(RubricError::config("拒绝理由不得为空（需写入审计）"));
        }
        let mut stored = self.load_draft(draft_id)?;
        if stored.is_terminal() {
            return Err(RubricError::DraftWrongStatus {
                status: stored.draft.status,
            });
        }
        let before = content_hash(&stored)?;
        stored.draft.status = DraftStatus::Rejected;
        stored.draft.confirmed_at = None;
        let after = content_hash(&stored)?;
        self.store_sink().save_draft(&stored)?;
        self.audit_sink().append(
            NewRubricAuditEvent::new(
                crate::ports::RubricAction::Rejected,
                stored.draft.id,
                stored.draft.question_id,
                eg_domain::LOCAL_TEACHER,
                self.clock_source().now(),
            )
            .with_hashes(Some(before), Some(after))
            .with_note(reason),
        )?;
        Ok(RejectOutcome {
            draft: stored,
            reason: reason.to_owned(),
        })
    }

    fn build_rubric(&self, stored: &crate::ports::StoredDraft, version: u32) -> Result<Rubric> {
        // 先按顺序生成评分项 id，再把 depends_on（草稿序号）映射成 id。
        let item_ids: Vec<eg_domain::RubricItemId> = stored
            .draft
            .items
            .iter()
            .map(|_| self.id_source().next_rubric_item_id())
            .collect();
        let length = stored.draft.items.len();
        let mut items = Vec::with_capacity(length);
        for (index, draft_item) in stored.draft.items.iter().enumerate() {
            let mut depends_on = Vec::with_capacity(draft_item.depends_on.len());
            for ordinal in &draft_item.depends_on {
                let position = usize::try_from(*ordinal)
                    .ok()
                    .filter(|value| *value >= 1 && *value <= length)
                    .ok_or(RubricError::Domain(
                        DomainError::DraftDependencyOutOfRange {
                            index: *ordinal,
                            len: length,
                        },
                    ))?;
                depends_on.push(item_ids[position - 1]);
            }
            items.push(RubricItem {
                id: item_ids[index],
                criterion: draft_item.criterion.clone(),
                points: draft_item.points,
                required: draft_item.required,
                depends_on,
            });
        }

        let total = if stored.mode == eg_domain::ScoringMode::PartialChoice {
            stored.max_score
        } else {
            MilliScore::checked_sum(items.iter().map(|item| item.points))?
        };

        let origin = match stored.draft.origin {
            DraftOrigin::HandwrittenTransliteration => RubricOrigin::TeacherHandwritten {
                confirmed_by: eg_domain::LOCAL_TEACHER.to_owned(),
                confirmed_at: self.clock_source().now(),
            },
            DraftOrigin::TemplateApplied => {
                let reference = require_template_ref(stored)?;
                RubricOrigin::StandardTemplate {
                    template_id: reference.template_id,
                    template_version: reference.version.as_u32(),
                    confirmed_by: eg_domain::LOCAL_TEACHER.to_owned(),
                    confirmed_at: self.clock_source().now(),
                }
            }
            DraftOrigin::ModelStructured => RubricOrigin::ModelStructured {
                model_call_id: stored
                    .draft
                    .model_call_id
                    .ok_or(RubricError::Domain(DomainError::MissingModelCallId))?,
                confirmed_by: eg_domain::LOCAL_TEACHER.to_owned(),
                confirmed_at: self.clock_source().now(),
            },
        };

        Ok(Rubric {
            id: stored.draft.id,
            version,
            items,
            mode: stored.mode,
            total,
            origin,
            partial_choice: stored.partial_choice.clone(),
        })
    }
}

fn map_domain_error(error: DomainError) -> RubricError {
    match error {
        DomainError::DraftNotConfirmable { status } => RubricError::DraftWrongStatus { status },
        DomainError::DraftHasErrors { count } => RubricError::DraftHasErrors { count },
        other => RubricError::Domain(other),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use eg_domain::{
        AnswerKey, MilliScore, ModelCallId, PaperId, PartialChoiceRule, PartialChoiceSource,
        PartialChoiceTier, Question, QuestionId, QuestionKind, RubricDraftItem, ScoringMode, Ulid,
    };

    use super::*;
    use crate::draft::DraftSource;
    use crate::memory::{FixedClock, InMemoryRubricStore, RecordingAudit, SequentialIds};
    use crate::ports::RubricStore;
    use crate::ports::{RubricAction, RubricSettings};
    use crate::templates::{TemplatePack, TemplateSelection, default_template_root};
    use eg_domain::time::parse_rfc3339;

    fn question(kind: QuestionKind, max_milli: i64) -> Question {
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

    fn item(criterion: &str, milli: i64, depends_on: Vec<u32>) -> RubricDraftItem {
        RubricDraftItem {
            criterion: criterion.to_owned(),
            points: MilliScore::from_milli(milli).unwrap(),
            required: false,
            depends_on,
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

    fn engine(fixture: &Fixture) -> crate::draft::RubricEngine<'_> {
        crate::draft::RubricEngine::new(
            &fixture.store,
            &fixture.audit,
            &fixture.clock,
            &fixture.ids,
            &fixture.settings,
        )
        .unwrap()
    }

    fn valid_draft(engine: &crate::draft::RubricEngine<'_>) -> crate::ports::StoredDraft {
        let question = question(QuestionKind::Calculation, 3000);
        let draft = engine
            .create_draft(
                &question,
                DraftSource::Handwritten {
                    raw_text: "列出方程 1.5 分，解得 x=2 1.5 分".to_owned(),
                    items: vec![
                        item("列出方程", 1500, vec![]),
                        item("解得 x=2", 1500, vec![]),
                    ],
                    mode: ScoringMode::Additive,
                    partial_choice: None,
                },
            )
            .unwrap();
        engine.submit_for_confirmation(draft.draft.id).unwrap()
    }

    #[test]
    fn confirm_generates_rubric_and_bumps_version() {
        let fixture = fixture();
        let engine = engine(&fixture);
        let draft = valid_draft(&engine);
        assert_eq!(draft.draft.status, DraftStatus::PendingConfirmation);

        let outcome = engine.confirm(draft.draft.id, Some(0)).unwrap();
        assert_eq!(outcome.previous_version, 0);
        assert_eq!(outcome.rubric.version, 1);
        assert_eq!(outcome.rubric.total.as_milli(), 3000);
        assert_eq!(outcome.rubric.items.len(), 2);
        assert_eq!(outcome.draft.draft.status, DraftStatus::Confirmed);
        assert!(outcome.draft.draft.confirmed_at.is_some());
        assert!(matches!(
            outcome.rubric.origin,
            RubricOrigin::TeacherHandwritten { .. }
        ));
        assert!(outcome.rubric.partial_choice.is_none());

        // 生效规则可读、版本可查。
        let active = engine
            .active_rubric(draft.draft.question_id)
            .unwrap()
            .unwrap();
        assert_eq!(active.version, 1);
        let actions: Vec<RubricAction> = fixture
            .audit
            .events()
            .iter()
            .map(|event| event.action)
            .collect();
        assert!(actions.contains(&RubricAction::DraftCreated));
        assert!(actions.contains(&RubricAction::SubmittedForConfirmation));
        assert_eq!(actions.last(), Some(&RubricAction::Confirmed));
    }

    #[test]
    fn confirm_requires_pending_state() {
        let fixture = fixture();
        let engine = engine(&fixture);
        let question = question(QuestionKind::Calculation, 1000);
        let draft = engine
            .create_draft(
                &question,
                DraftSource::Handwritten {
                    raw_text: String::new(),
                    items: vec![item("一步", 1000, vec![])],
                    mode: ScoringMode::AllOrNothing,
                    partial_choice: None,
                },
            )
            .unwrap();
        // 未提交待确认。
        let error = engine.confirm(draft.draft.id, None).unwrap_err();
        assert!(matches!(error, RubricError::DraftWrongStatus { .. }));
    }

    #[test]
    fn confirm_blocks_drafts_with_errors() {
        let fixture = fixture();
        let engine = engine(&fixture);
        let question = question(QuestionKind::Calculation, 3000);
        let draft = engine
            .create_draft(
                &question,
                DraftSource::Handwritten {
                    raw_text: String::new(),
                    items: vec![item("只有一步", 1000, vec![])],
                    mode: ScoringMode::Additive,
                    partial_choice: None,
                },
            )
            .unwrap();
        // 手工把状态改成待确认（模拟绕过提交接口），确认时必须重新校验并拒绝。
        let mut stored = draft.clone();
        stored.draft.status = DraftStatus::PendingConfirmation;
        fixture.store.save_draft(&stored).unwrap();
        let error = engine.confirm(draft.draft.id, None).unwrap_err();
        assert!(matches!(error, RubricError::DraftHasErrors { .. }));
        assert_eq!(
            fixture
                .store
                .rubric_version(draft.draft.question_id)
                .unwrap(),
            0
        );
    }

    #[test]
    fn second_confirmation_increments_version() {
        let fixture = fixture();
        let engine = engine(&fixture);
        let first = valid_draft(&engine);
        engine.confirm(first.draft.id, Some(0)).unwrap();

        let second = valid_draft(&engine);
        let outcome = engine.confirm(second.draft.id, Some(1)).unwrap();
        assert_eq!(outcome.previous_version, 1);
        assert_eq!(outcome.rubric.version, 2);
    }

    #[test]
    fn version_conflict_is_rejected() {
        let fixture = fixture();
        let engine = engine(&fixture);
        let first = valid_draft(&engine);
        engine.confirm(first.draft.id, Some(0)).unwrap();

        let second = valid_draft(&engine);
        let error = engine.confirm(second.draft.id, Some(0)).unwrap_err();
        assert!(matches!(
            error,
            RubricError::VersionConflict {
                expected: 0,
                actual: 1
            }
        ));
        // 冲突时不得产生版本推进。
        assert_eq!(
            engine
                .active_rubric(second.draft.question_id)
                .unwrap()
                .unwrap()
                .version,
            1
        );
    }

    #[test]
    fn template_draft_confirms_after_placeholder_replaced() {
        let fixture = fixture();
        let engine = engine(&fixture);
        let pack = TemplatePack::load(&default_template_root().unwrap()).unwrap();
        let template = pack
            .resolve(&TemplateSelection {
                key: "new-curriculum-1".to_owned(),
                allow_retired: false,
            })
            .unwrap();
        let question = question(QuestionKind::SingleChoice, 5000);
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
        assert!(
            draft
                .draft
                .validation
                .iter()
                .any(|issue| issue.message.contains("R07"))
        );

        // 教师替换占位判据后可通过校验并确认。
        let draft = engine
            .update_items(
                draft.draft.id,
                vec![item("选出唯一正确选项，与答案键一致", 5000, vec![])],
                ScoringMode::AllOrNothing,
                None,
                Some("教师按 2026 年评分细则改写".to_owned()),
            )
            .unwrap();
        assert_eq!(draft.draft.validation, Vec::new());
        let draft = engine.submit_for_confirmation(draft.draft.id).unwrap();
        let outcome = engine.confirm(draft.draft.id, Some(0)).unwrap();
        match outcome.rubric.origin {
            RubricOrigin::StandardTemplate {
                template_id,
                template_version,
                ..
            } => {
                assert_eq!(template_id.as_str(), "national-new-curriculum-1");
                assert_eq!(template_version, 2_026_002);
            }
            other => panic!("期望模板来源，实际 {other:?}"),
        }
    }

    #[test]
    fn partial_choice_confirms_with_tiers() {
        let fixture = fixture();
        let engine = engine(&fixture);
        let question = question(QuestionKind::MultipleChoice, 6000);
        let rule = PartialChoiceRule {
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
        };
        let draft = engine
            .create_draft(
                &question,
                DraftSource::Handwritten {
                    raw_text: "多选部分给分".to_owned(),
                    items: vec![item("全对得满分，错选 0 分", 6000, vec![])],
                    mode: ScoringMode::PartialChoice,
                    partial_choice: Some(rule),
                },
            )
            .unwrap();
        let draft = engine.submit_for_confirmation(draft.draft.id).unwrap();
        let outcome = engine.confirm(draft.draft.id, Some(0)).unwrap();
        assert_eq!(outcome.rubric.mode, ScoringMode::PartialChoice);
        assert_eq!(outcome.rubric.total.as_milli(), 6000);
        let rule = outcome.rubric.partial_choice.expect("档位应保留");
        assert_eq!(rule.tiers.len(), 2);
        assert_eq!(rule.tiers[1].points.as_milli(), 4000);
        assert!(rule.wrong_selection_zero);
    }

    #[test]
    fn model_structured_confirmation_uses_model_call() {
        let fixture = fixture();
        let engine = engine(&fixture);
        let question = question(QuestionKind::Calculation, 1000);
        let draft = engine
            .create_draft(
                &question,
                DraftSource::ModelStructured {
                    raw_text: "每步 1 分".to_owned(),
                    items: vec![item("一步", 1000, vec![])],
                    mode: ScoringMode::AllOrNothing,
                    partial_choice: None,
                    model_call_id: ModelCallId::from_ulid(Ulid::from_u128(9)),
                },
            )
            .unwrap();
        let draft = engine.submit_for_confirmation(draft.draft.id).unwrap();
        let outcome = engine.confirm(draft.draft.id, Some(0)).unwrap();
        assert!(matches!(
            outcome.rubric.origin,
            RubricOrigin::ModelStructured { .. }
        ));
    }

    #[test]
    fn reject_flow_and_terminal_immutability() {
        let fixture = fixture();
        let engine = engine(&fixture);
        let question = question(QuestionKind::Calculation, 1000);
        let draft = engine
            .create_draft(
                &question,
                DraftSource::Handwritten {
                    raw_text: String::new(),
                    items: vec![item("一步", 1000, vec![])],
                    mode: ScoringMode::AllOrNothing,
                    partial_choice: None,
                },
            )
            .unwrap();
        let outcome = engine.reject(draft.draft.id, "判据与评分标准不符").unwrap();
        assert_eq!(outcome.draft.draft.status, DraftStatus::Rejected);
        assert!(outcome.draft.draft.confirmed_at.is_none());
        assert_eq!(outcome.reason, "判据与评分标准不符");

        // 终态拒绝再次确认/拒绝/编辑。
        assert!(matches!(
            engine.confirm(draft.draft.id, None).unwrap_err(),
            RubricError::DraftWrongStatus { .. }
        ));
        assert!(matches!(
            engine.reject(draft.draft.id, "再次拒绝").unwrap_err(),
            RubricError::DraftWrongStatus { .. }
        ));
        assert!(matches!(
            engine
                .update_items(
                    draft.draft.id,
                    Vec::new(),
                    ScoringMode::Additive,
                    None,
                    None
                )
                .unwrap_err(),
            RubricError::DraftNotEditable { .. }
        ));
    }

    #[test]
    fn reject_requires_reason() {
        let fixture = fixture();
        let engine = engine(&fixture);
        let question = question(QuestionKind::Calculation, 1000);
        let draft = engine
            .create_draft(
                &question,
                DraftSource::Handwritten {
                    raw_text: String::new(),
                    items: vec![item("一步", 1000, vec![])],
                    mode: ScoringMode::AllOrNothing,
                    partial_choice: None,
                },
            )
            .unwrap();
        let error = engine.reject(draft.draft.id, "  ").unwrap_err();
        assert!(matches!(error, RubricError::Config { .. }));
    }

    #[test]
    fn confirmed_draft_cannot_be_edited_or_rejected() {
        let fixture = fixture();
        let engine = engine(&fixture);
        let draft = valid_draft(&engine);
        engine.confirm(draft.draft.id, Some(0)).unwrap();
        assert!(matches!(
            engine
                .update_items(
                    draft.draft.id,
                    Vec::new(),
                    ScoringMode::Additive,
                    None,
                    None
                )
                .unwrap_err(),
            RubricError::DraftNotEditable { .. }
        ));
    }

    #[test]
    fn dependency_ordinals_map_to_generated_item_ids() {
        let fixture = fixture();
        let engine = engine(&fixture);
        let question = question(QuestionKind::Calculation, 3000);
        let draft = engine
            .create_draft(
                &question,
                DraftSource::Handwritten {
                    raw_text: "分步".to_owned(),
                    items: vec![item("步骤一", 1000, vec![]), item("步骤二", 2000, vec![1])],
                    mode: ScoringMode::Stepwise,
                    partial_choice: None,
                },
            )
            .unwrap();
        let draft = engine.submit_for_confirmation(draft.draft.id).unwrap();
        let outcome = engine.confirm(draft.draft.id, Some(0)).unwrap();
        assert_eq!(
            outcome.rubric.items[1].depends_on,
            vec![outcome.rubric.items[0].id]
        );
    }
}
