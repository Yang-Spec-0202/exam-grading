//! 全国卷模板包与自定义模板：读取兼容、套用折算、占位判据拦截、来源追溯。
//!
//! 只读内置模板数据（`assets/templates` 归 T25 维护），
//! 缺口（多选部分给分档位缺失、`provenance.basis` 取值超枚举）只报告不改数据。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::assigning_clones,
    clippy::assert_is_empty,
    clippy::items_after_statements,
    clippy::too_many_lines
)]

mod common;

use common::{Harness, question, template_root};
use eg_domain::{DraftStatus, PartialChoiceSource, QuestionKind, RubricOrigin, ScoringMode};
use eg_rubric::{
    CheckId, DraftSource, LoadedTemplate, RubricAction, RubricError, StoredDraft, TemplatePack,
    TemplateSelection, TemplateWarningCode,
};

fn national_pack() -> TemplatePack {
    TemplatePack::load(&template_root()).expect("内置全国卷模板包必须可加载")
}

fn custom_template(name: &str) -> LoadedTemplate {
    let path = common::fixtures_dir()
        .join("custom-templates")
        .join(name)
        .join("template.toml");
    TemplatePack::load_single(&path).expect("自定义模板夹具必须可加载")
}

fn stored_by_id(drafts: &[StoredDraft], id: eg_domain::RubricId) -> &StoredDraft {
    drafts
        .iter()
        .find(|stored| stored.draft.id == id)
        .expect("草稿应存在于列表中")
}

#[test]
fn national_pack_data_is_aligned_with_spec() {
    let pack = national_pack();
    assert_eq!(pack.templates.len(), 4, "全国卷模板包应有四套");

    let active: Vec<&str> = pack
        .active_templates()
        .iter()
        .map(|template| template.set_id.as_str())
        .collect();
    assert_eq!(active, vec!["new-curriculum-1", "new-curriculum-2"]);

    let retired: Vec<(&str, u32)> = pack
        .retired_templates()
        .iter()
        .map(|template| {
            (
                template.set_id.as_str(),
                template.status.retired_since().expect("历史卷应有停用年份"),
            )
        })
        .collect();
    assert_eq!(retired, vec![("national-jia", 2026), ("national-yi", 2024)]);

    // 四套总分均为 150 分，题目满分合计与题量一致（数据完整性）。
    for template in &pack.templates {
        assert_eq!(
            template.total_score.as_milli(),
            150_000,
            "{}",
            template.set_id
        );
        assert!(!template.questions.is_empty());
        let sum: i64 = template
            .questions
            .iter()
            .map(|question| question.max_score.as_milli())
            .sum();
        assert_eq!(sum, template.total_score.as_milli(), "{}", template.set_id);
        // 题号连续。
        let ordinals = template.question_ordinals();
        assert_eq!(
            ordinals,
            (1..=u32::try_from(ordinals.len()).expect("题量有限")).collect::<Vec<_>>()
        );
    }

    // T25 模板数据对齐：现行卷多选已内置官方部分给分档位，读取缺口告警必须消失。
    assert!(!pack.has_warning(TemplateWarningCode::PartialChoiceTiersMissing));
    assert!(!pack.has_warning(TemplateWarningCode::UnknownEnumValue));
    assert!(
        pack.has_warning(TemplateWarningCode::RetiredTemplate),
        "历史卷仍应告警"
    );

    for template in pack.active_templates() {
        assert_eq!(template.version.to_string(), "2026.2");
        assert_eq!(template.provenance.basis, "public-analysis");
        let multi_questions: Vec<_> = template
            .questions
            .iter()
            .filter(|question| question.kind == QuestionKind::MultipleChoice)
            .collect();
        assert_eq!(multi_questions.len(), 3, "现行卷应含 3 道多选");
        for question in multi_questions {
            assert_eq!(question.max_score.as_milli(), 6000);
            let skeleton = template
                .skeletons
                .get(&question.skeleton)
                .expect("题目引用的骨架应存在");
            let tiers = skeleton
                .partial_choice
                .as_ref()
                .expect("多选骨架必须带部分给分档位");
            assert_eq!(tiers.correct_option_count, 3);
            assert!(tiers.wrong_selection_zero);
            assert_eq!(tiers.source, PartialChoiceSource::Official);
            assert_eq!(
                tiers
                    .tiers
                    .iter()
                    .map(|tier| tier.selected_correct)
                    .collect::<Vec<_>>(),
                vec![1, 2]
            );
        }
    }
}

#[test]
fn template_apply_keeps_placeholders_until_teacher_replaces_them() {
    let pack = national_pack();
    let harness = Harness::default();
    let engine = harness.engine();
    let template = pack.find("new-curriculum-1").expect("应有新课标Ⅰ卷");
    let multi = template
        .questions
        .iter()
        .find(|question| question.kind == QuestionKind::MultipleChoice)
        .expect("新课标Ⅰ卷应有多选题");
    let question = question(multi.kind, multi.max_score.as_milli(), multi.ordinal);

    let created = engine
        .create_draft(
            &question,
            DraftSource::Template {
                template,
                allow_retired: false,
                skeleton_override: None,
            },
        )
        .expect("模板套用建草稿");
    assert_eq!(
        created.draft.origin,
        eg_domain::DraftOrigin::TemplateApplied
    );
    assert_eq!(
        created.draft.status,
        DraftStatus::Draft,
        "占位判据应阻止进入待确认"
    );
    assert_eq!(
        created.mode,
        ScoringMode::PartialChoice,
        "T25 起多选模板按官方部分给分口径套用"
    );
    let created_rule = created
        .partial_choice
        .clone()
        .expect("模板多选草稿应带部分给分档位");
    assert_eq!(created_rule.correct_option_count, 3);
    assert_eq!(created_rule.total_milli.as_milli(), 6_000);
    assert_eq!(created_rule.tiers.len(), 2);
    assert_eq!(created_rule.tiers[0].points.as_milli(), 2_000);
    assert_eq!(created_rule.tiers[1].points.as_milli(), 4_000);
    let reference = created.template.as_ref().expect("模板来源应带引用");
    assert_eq!(reference.set_id, "new-curriculum-1");
    assert_eq!(reference.template_id, template.template_id);
    assert_eq!(reference.version.as_u32(), 2_026_002);

    let report = engine.report_for(&created);
    assert!(report.has(CheckId::R07CriterionPlaceholder));
    assert!(!report.is_ok());
    // 模板骨架按满分比例精确折算：评分项合计 == 满分。
    let sum: i64 = created
        .draft
        .items
        .iter()
        .map(|item| item.points.as_milli())
        .sum();
    assert_eq!(sum, multi.max_score.as_milli());
    assert!(matches!(
        engine.submit_for_confirmation(created.draft.id),
        Err(RubricError::DraftHasErrors { .. })
    ));

    // 教师替换占位判据后可确认。
    let replaced: Vec<eg_domain::RubricDraftItem> = created
        .draft
        .items
        .iter()
        .enumerate()
        .map(|(index, item)| eg_domain::RubricDraftItem {
            criterion: format!("本题具体判据第 {} 条（教师确认）", index + 1),
            points: item.points,
            required: item.required,
            depends_on: item.depends_on.clone(),
        })
        .collect();
    let updated = engine
        .update_items(
            created.draft.id,
            replaced,
            created.mode,
            created.partial_choice.clone(),
            Some("教师替换模板占位判据".to_owned()),
        )
        .expect("替换判据");
    assert!(engine.report_for(&updated).is_ok());
    engine
        .submit_for_confirmation(created.draft.id)
        .expect("提交");
    let outcome = engine.confirm(created.draft.id, Some(0)).expect("确认");

    match outcome.rubric.origin {
        RubricOrigin::StandardTemplate {
            template_id,
            template_version,
            ..
        } => {
            assert_eq!(template_id, template.template_id);
            assert_eq!(template_version, 2_026_002);
        }
        other => panic!("模板来源应为 StandardTemplate，实际 {other:?}"),
    }
    assert_eq!(outcome.rubric.total.as_milli(), multi.max_score.as_milli());
    assert_eq!(outcome.rubric.mode, ScoringMode::PartialChoice);
    assert_eq!(outcome.rubric.partial_choice, Some(created_rule));

    // 审计：套用事件带模板引用；确认事件带版本与模板引用。
    let events = harness.audit.events();
    let applied = events
        .iter()
        .filter(|event| {
            event.action == RubricAction::TemplateApplied && event.draft_id == created.draft.id
        })
        .count();
    assert_eq!(applied, 1);
    let confirmed = events
        .iter()
        .find(|event| event.action == RubricAction::Confirmed && event.draft_id == created.draft.id)
        .expect("应有确认审计");
    assert_eq!(confirmed.rubric_version, Some(1));
    assert_eq!(
        confirmed
            .template
            .as_ref()
            .map(|reference| reference.set_id.as_str()),
        Some("new-curriculum-1")
    );
}

#[test]
fn retired_template_requires_explicit_allow() {
    let pack = national_pack();
    let harness = Harness::default();
    let engine = harness.engine();

    assert!(matches!(
        pack.resolve(&TemplateSelection {
            key: "national-jia".to_owned(),
            allow_retired: false,
        }),
        Err(RubricError::TemplateRetired {
            retired_since: 2026,
            ..
        })
    ));
    let template = pack
        .resolve(&TemplateSelection {
            key: "全国甲卷".to_owned(),
            allow_retired: true,
        })
        .expect("显式允许后可解析历史卷");

    let first = template.questions.first().expect("模板应有题目");
    let question = question(first.kind, first.max_score.as_milli(), first.ordinal);
    assert!(matches!(
        engine.create_draft(
            &question,
            DraftSource::Template {
                template,
                allow_retired: false,
                skeleton_override: None,
            },
        ),
        Err(RubricError::TemplateRetired { .. })
    ));

    let created = engine
        .create_draft(
            &question,
            DraftSource::Template {
                template,
                allow_retired: true,
                skeleton_override: None,
            },
        )
        .expect("允许 retired 后可套用");
    let replaced: Vec<eg_domain::RubricDraftItem> = created
        .draft
        .items
        .iter()
        .enumerate()
        .map(|(index, item)| eg_domain::RubricDraftItem {
            criterion: format!("历史卷复现判据 {}", index + 1),
            points: item.points,
            required: item.required,
            depends_on: item.depends_on.clone(),
        })
        .collect();
    engine
        .update_items(
            created.draft.id,
            replaced,
            created.mode,
            created.partial_choice.clone(),
            None,
        )
        .expect("替换判据");
    engine
        .submit_for_confirmation(created.draft.id)
        .expect("提交");
    let outcome = engine.confirm(created.draft.id, Some(0)).expect("确认");
    match outcome.rubric.origin {
        RubricOrigin::StandardTemplate {
            template_version, ..
        } => assert_eq!(template_version, 2_025_001, "全国甲卷版本 2025.1"),
        other => panic!("模板来源应为 StandardTemplate，实际 {other:?}"),
    }
}

#[test]
fn template_plan_rejects_missing_question_and_kind_mismatch() {
    let pack = national_pack();
    let harness = Harness::default();
    let engine = harness.engine();
    let template = pack.find("new-curriculum-1").expect("应有新课标Ⅰ卷");
    let first = template.questions.first().expect("模板应有题目");

    let wrong_kind = question(
        if first.kind == QuestionKind::SingleChoice {
            QuestionKind::MultipleChoice
        } else {
            QuestionKind::SingleChoice
        },
        first.max_score.as_milli(),
        first.ordinal,
    );
    assert!(matches!(
        engine.create_draft(
            &wrong_kind,
            DraftSource::Template {
                template,
                allow_retired: false,
                skeleton_override: None,
            },
        ),
        Err(RubricError::TemplateKindMismatch { .. })
    ));

    let missing = question(first.kind, first.max_score.as_milli(), 99);
    assert!(matches!(
        engine.create_draft(
            &missing,
            DraftSource::Template {
                template,
                allow_retired: false,
                skeleton_override: None,
            },
        ),
        Err(RubricError::TemplateQuestionMissing { .. })
    ));
}

#[test]
fn custom_template_supports_partial_choice_tiers() {
    let template = custom_template("custom-multi");
    assert_eq!(template.set_id, "custom-multi");
    let skeleton = template
        .skeletons
        .get("objective-multi")
        .expect("自定义骨架应存在");
    let tiers = skeleton
        .partial_choice
        .as_ref()
        .expect("读取兼容应解析出档位");
    assert_eq!(tiers.correct_option_count, 2);
    assert_eq!(tiers.tiers.len(), 1);

    let harness = Harness::default();
    let engine = harness.engine();
    let first = template.questions.first().expect("模板应有题目");
    let question = question(first.kind, first.max_score.as_milli(), first.ordinal);
    let created = engine
        .create_draft(
            &question,
            DraftSource::Template {
                template: &template,
                allow_retired: false,
                skeleton_override: None,
            },
        )
        .expect("套用自定义模板");
    assert_eq!(created.mode, ScoringMode::PartialChoice);
    let rule = created.partial_choice.clone().expect("应带档位");
    assert_eq!(rule.correct_option_count, 2);
    assert_eq!(rule.total_milli.as_milli(), 6000);
    assert_eq!(rule.tiers.len(), 1);
    assert_eq!(rule.tiers[0].selected_correct, 1);
    assert_eq!(rule.tiers[0].points.as_milli(), 2000);

    let report = engine.report_for(&created);
    assert!(
        !report.has(CheckId::P01ModeExclusive)
            && !report.has(CheckId::P02TierCoverage)
            && !report.has(CheckId::P03TierPoints)
            && !report.has(CheckId::P04TierGranularity)
            && !report.has(CheckId::P05TierSource),
        "模板档位应通过部分给分专项校验，实际 {:?}",
        report.codes()
    );
    assert!(
        report.has(CheckId::R07CriterionPlaceholder),
        "骨架判据仍是占位"
    );

    let replaced: Vec<eg_domain::RubricDraftItem> = created
        .draft
        .items
        .iter()
        .map(|item| eg_domain::RubricDraftItem {
            criterion: "全部选对得 6 分；选对 1 个得 2 分；有错选 0 分".to_owned(),
            points: item.points,
            required: item.required,
            depends_on: item.depends_on.clone(),
        })
        .collect();
    engine
        .update_items(
            created.draft.id,
            replaced,
            created.mode,
            created.partial_choice.clone(),
            None,
        )
        .expect("替换判据");
    engine
        .submit_for_confirmation(created.draft.id)
        .expect("提交");
    let outcome = engine.confirm(created.draft.id, Some(0)).expect("确认");
    assert_eq!(outcome.rubric.mode, ScoringMode::PartialChoice);
    assert_eq!(outcome.rubric.total.as_milli(), 6000);
    assert_eq!(outcome.rubric.partial_choice, Some(rule));
}

#[test]
fn custom_template_loading_and_pack_insert_rules() {
    // 总分与题目合计不符的模板必须整体失败，不得静默读取。
    let bad_path = common::fixtures_dir()
        .join("custom-templates")
        .join("custom-bad-total")
        .join("template.toml");
    assert!(matches!(
        TemplatePack::load_single(&bad_path),
        Err(RubricError::TemplateInvalid { .. })
    ));

    let pack = national_pack();
    let demo = custom_template("custom-demo");
    assert_eq!(demo.total_score.as_milli(), 10_000);
    let mut merged = pack.clone();
    merged
        .insert(demo.clone())
        .expect("自定义模板可合并进模板包");
    assert!(merged.find("custom-demo").is_some());
    assert_eq!(merged.templates.len(), 5);

    // 只追加不覆盖：同 id/同 set_id/同名均拒绝。
    let mut duplicated = merged.clone();
    assert!(matches!(
        duplicated.insert(demo.clone()),
        Err(RubricError::TemplateInvalid { .. })
    ));
    assert_eq!(duplicated.templates.len(), 5);

    // 自定义模板也能走完套用 → 替换 → 确认，来源版本为 2026.1。
    let harness = Harness::default();
    let engine = harness.engine();
    let first = demo.questions.first().expect("模板应有题目");
    let question = question(first.kind, first.max_score.as_milli(), first.ordinal);
    let created = engine
        .create_draft(
            &question,
            DraftSource::Template {
                template: &demo,
                allow_retired: false,
                skeleton_override: None,
            },
        )
        .expect("套用自定义模板");
    let replaced: Vec<eg_domain::RubricDraftItem> = created
        .draft
        .items
        .iter()
        .map(|item| eg_domain::RubricDraftItem {
            criterion: "选出唯一正确选项，与答案键一致".to_owned(),
            points: item.points,
            required: item.required,
            depends_on: item.depends_on.clone(),
        })
        .collect();
    engine
        .update_items(
            created.draft.id,
            replaced,
            created.mode,
            created.partial_choice.clone(),
            None,
        )
        .expect("替换判据");
    engine
        .submit_for_confirmation(created.draft.id)
        .expect("提交");
    let outcome = engine.confirm(created.draft.id, Some(0)).expect("确认");
    assert_eq!(outcome.rubric.total.as_milli(), 10_000);
    match outcome.rubric.origin {
        RubricOrigin::StandardTemplate {
            template_version, ..
        } => assert_eq!(template_version, 2_026_001),
        other => panic!("模板来源应为 StandardTemplate，实际 {other:?}"),
    }
}

#[test]
fn template_pack_lists_drafts_per_question() {
    let pack = national_pack();
    let template = pack.find("new-curriculum-1").expect("应有新课标Ⅰ卷");
    let first = template.questions.first().expect("模板应有题目");
    let harness = Harness::default();
    let engine = harness.engine();
    let question = question(first.kind, first.max_score.as_milli(), first.ordinal);
    let created = engine
        .create_draft(
            &question,
            DraftSource::Template {
                template,
                allow_retired: false,
                skeleton_override: None,
            },
        )
        .expect("套用模板");
    let drafts = engine.list_drafts(question.id).expect("列出草稿");
    let stored = stored_by_id(&drafts, created.draft.id);
    assert_eq!(stored.draft.origin, eg_domain::DraftOrigin::TemplateApplied);
    assert_eq!(
        stored
            .template
            .as_ref()
            .map(|reference| reference.set_id.as_str()),
        Some("new-curriculum-1")
    );
}

#[test]
fn multi_choice_template_without_tiers_still_reports_gap_warning() {
    // 读取兼容：自定义模板若含多选但没有部分给分档位，合并时记录 TW01，不阻断加载。
    let gap = custom_template("custom-multi-gap");
    assert!(
        gap.skeletons
            .values()
            .all(|skeleton| skeleton.partial_choice.is_none())
    );
    let mut pack = national_pack();
    assert!(!pack.has_warning(TemplateWarningCode::PartialChoiceTiersMissing));
    pack.insert(gap).expect("缺口模板仍可加载（只报告）");
    assert!(pack.has_warning(TemplateWarningCode::PartialChoiceTiersMissing));
    let warned = pack
        .warnings
        .iter()
        .find(|warning| warning.code == TemplateWarningCode::PartialChoiceTiersMissing)
        .expect("应有 TW01 告警");
    assert_eq!(warned.set_id, "custom-multi-gap");
}
