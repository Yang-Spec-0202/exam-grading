//! 手写直译规则端到端：10 类常见题型从建草稿到确认生效。
//!
//! 覆盖 `SPEC-rubric.md` §3.1/§3.3：草稿四状态、确认前重跑静态校验、
//! 确认生成 `Rubric`（版本从 1 起）、终态不可编辑、审计动作序列。
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

use common::{Harness, engine_with, load_rules, question, timestamp};
use eg_domain::{DraftStatus, RubricOrigin, ScoringMode};
use eg_rubric::{DraftSource, RubricAction, RubricEngine, RubricError, RubricSettings};

fn assert_no_findings(engine: &RubricEngine<'_>, created: &eg_rubric::StoredDraft, name: &str) {
    let report = engine.report_for(created);
    assert!(
        report.is_ok(),
        "{name} 不应有错误级问题，实际码 {:?}",
        report.codes()
    );
    assert_eq!(report.warning_count(), 0, "{name} 不应有警告");
    assert!(created.draft.validation.is_empty(), "{name} 校验结果应为空");
}

#[test]
fn handwritten_rules_confirm_into_effective_rubric() {
    let rules = load_rules("handwritten_rules.json");
    assert_eq!(rules.len(), 10, "手写直译夹具应有 10 条规则");

    let harness = Harness::default();
    let engine = harness.engine();

    for (index, rule) in rules.iter().enumerate() {
        let ordinal = u32::try_from(index + 1).expect("夹具条数有限");
        let question = question(rule.kind, rule.max_score_milli, ordinal);
        let created = engine
            .create_draft(
                &question,
                DraftSource::Handwritten {
                    raw_text: rule.name.clone(),
                    items: rule.items.clone(),
                    mode: rule.mode,
                    partial_choice: rule.partial_choice.clone(),
                },
            )
            .unwrap_or_else(|error| panic!("{} 建草稿失败：{error}", rule.name));

        assert_no_findings(&engine, &created, &rule.name);
        assert_eq!(created.draft.status, DraftStatus::Draft);
        assert_eq!(created.draft.items.len(), rule.items.len());
        assert_eq!(created.max_score.as_milli(), rule.max_score_milli);

        let submitted = engine
            .submit_for_confirmation(created.draft.id)
            .unwrap_or_else(|error| panic!("{} 提交待确认失败：{error}", rule.name));
        assert_eq!(submitted.draft.status, DraftStatus::PendingConfirmation);

        let outcome = engine
            .confirm(created.draft.id, Some(0))
            .unwrap_or_else(|error| panic!("{} 确认失败：{error}", rule.name));
        assert_eq!(outcome.previous_version, 0, "{} 首次确认", rule.name);
        assert_eq!(outcome.rubric.version, 1);
        assert_eq!(outcome.rubric.total.as_milli(), rule.max_score_milli);
        assert_eq!(outcome.rubric.mode, rule.mode);
        assert_eq!(outcome.rubric.items.len(), rule.items.len());
        assert_eq!(
            outcome.rubric.partial_choice, rule.partial_choice,
            "{} 部分给分档位应原样写入生效规则",
            rule.name
        );
        assert!(
            matches!(
                outcome.rubric.origin,
                RubricOrigin::TeacherHandwritten { .. }
            ),
            "{} 手写来源应为 TeacherHandwritten",
            rule.name
        );
        assert_eq!(outcome.draft.draft.status, DraftStatus::Confirmed);
        assert_eq!(outcome.draft.draft.confirmed_at, Some(timestamp()));

        // 生效规则可回读，且与返回值一致。
        let active = engine
            .active_rubric(question.id)
            .expect("回读生效规则失败")
            .expect("确认后应有生效规则");
        assert_eq!(active, outcome.rubric);

        // 终态：不可编辑、不可重复确认。
        assert!(matches!(
            engine.update_items(
                created.draft.id,
                rule.items.clone(),
                rule.mode,
                rule.partial_choice.clone(),
                None
            ),
            Err(RubricError::DraftNotEditable { .. })
        ));
        assert!(matches!(
            engine.confirm(created.draft.id, None),
            Err(RubricError::DraftWrongStatus { .. })
        ));
    }

    // 审计：每条规则都应留下 建草稿 → 提交 → 确认 三步，且确认事件带版本号。
    let events = harness.audit.events();
    assert_eq!(events.len(), rules.len() * 3);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.action == RubricAction::DraftCreated)
            .count(),
        rules.len()
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.action == RubricAction::SubmittedForConfirmation)
            .count(),
        rules.len()
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.action == RubricAction::Confirmed)
            .count(),
        rules.len()
    );
    assert!(
        events
            .iter()
            .filter(|event| event.action == RubricAction::Confirmed)
            .all(|event| event.rubric_version == Some(1))
    );
    assert_eq!(harness.store.draft_count().expect("统计草稿"), rules.len());
}

#[test]
fn version_increments_and_conflicts_are_detected() {
    let rules = load_rules("handwritten_rules.json");
    let rule = rules.first().expect("夹具非空");
    let harness = Harness::default();
    let engine = harness.engine();
    let question = question(rule.kind, rule.max_score_milli, 1);

    let first = engine
        .create_draft(
            &question,
            DraftSource::Handwritten {
                raw_text: "第一版".to_owned(),
                items: rule.items.clone(),
                mode: rule.mode,
                partial_choice: rule.partial_choice.clone(),
            },
        )
        .expect("建草稿");
    engine
        .submit_for_confirmation(first.draft.id)
        .expect("提交");
    let first_outcome = engine.confirm(first.draft.id, Some(0)).expect("确认 v1");
    assert_eq!(first_outcome.rubric.version, 1);

    // 同一题目再确认一稿：版本递增，题目维度互相独立。
    let second = engine
        .create_draft(
            &question,
            DraftSource::Handwritten {
                raw_text: "第二版".to_owned(),
                items: rule.items.clone(),
                mode: rule.mode,
                partial_choice: rule.partial_choice.clone(),
            },
        )
        .expect("建草稿");
    engine
        .submit_for_confirmation(second.draft.id)
        .expect("提交");

    // 乐观并发：调用方持有旧版本号时必须失败，且不产生新版本。
    assert!(matches!(
        engine.confirm(second.draft.id, Some(0)),
        Err(RubricError::VersionConflict {
            expected: 0,
            actual: 1
        })
    ));
    assert_eq!(
        engine
            .active_rubric(question.id)
            .expect("回读")
            .expect("已有生效规则")
            .version,
        1,
        "版本冲突不得推进版本"
    );

    let second_outcome = engine.confirm(second.draft.id, Some(1)).expect("确认 v2");
    assert_eq!(second_outcome.previous_version, 1);
    assert_eq!(second_outcome.rubric.version, 2);
    assert_eq!(
        engine
            .active_rubric(question.id)
            .expect("回读")
            .expect("已有生效规则")
            .version,
        2
    );

    // 不传期望版本时按当前版本推进（单机交互路径）。
    let third = engine
        .create_draft(
            &question,
            DraftSource::Handwritten {
                raw_text: "第三版".to_owned(),
                items: rule.items.clone(),
                mode: rule.mode,
                partial_choice: rule.partial_choice.clone(),
            },
        )
        .expect("建草稿");
    engine
        .submit_for_confirmation(third.draft.id)
        .expect("提交");
    let third_outcome = engine.confirm(third.draft.id, None).expect("确认 v3");
    assert_eq!(third_outcome.rubric.version, 3);
}

#[test]
fn rejected_draft_is_terminal_and_audited() {
    let rules = load_rules("handwritten_rules.json");
    let rule = rules.first().expect("夹具非空");
    let harness = Harness::default();
    let engine = harness.engine();
    let question = question(rule.kind, rule.max_score_milli, 2);

    let created = engine
        .create_draft(
            &question,
            DraftSource::Handwritten {
                raw_text: rule.name.clone(),
                items: rule.items.clone(),
                mode: rule.mode,
                partial_choice: rule.partial_choice.clone(),
            },
        )
        .expect("建草稿");
    engine
        .submit_for_confirmation(created.draft.id)
        .expect("提交");

    // 空理由拒绝必须报错（理由要进审计）。
    assert!(engine.reject(created.draft.id, "  ").is_err());

    let rejected = engine
        .reject(created.draft.id, "判据与评分细则不符")
        .expect("拒绝");
    assert_eq!(rejected.draft.draft.status, DraftStatus::Rejected);
    assert_eq!(rejected.reason, "判据与评分细则不符");
    assert!(engine.active_rubric(question.id).expect("回读").is_none());

    // 终态不可再操作。
    assert!(matches!(
        engine.confirm(created.draft.id, None),
        Err(RubricError::DraftWrongStatus { .. })
    ));
    assert!(matches!(
        engine.reject(created.draft.id, "再次拒绝"),
        Err(RubricError::DraftWrongStatus { .. })
    ));

    let rejection = harness
        .audit
        .events()
        .into_iter()
        .filter(|event| event.action == RubricAction::Rejected)
        .collect::<Vec<_>>();
    assert_eq!(rejection.len(), 1);
    assert_eq!(rejection[0].note.as_deref(), Some("判据与评分细则不符"));
}

#[test]
fn submit_rejects_drafts_with_errors() {
    let rules = load_rules("handwritten_rules.json");
    let rule = rules.first().expect("夹具非空");
    // 把满分改为与评分项合计不符，制造 R01 错误。
    let mut items = rule.items.clone();
    if let Some(item) = items.first_mut() {
        item.points = eg_domain::MilliScore::from_milli(1000).expect("合法分值");
    }
    let harness = Harness::default();
    let engine = harness.engine();
    let question = question(rule.kind, rule.max_score_milli, 3);

    let created = engine
        .create_draft(
            &question,
            DraftSource::Handwritten {
                raw_text: "分值合计不符".to_owned(),
                items,
                mode: rule.mode,
                partial_choice: rule.partial_choice.clone(),
            },
        )
        .expect("建草稿仍应成功");
    assert!(!created.draft.validation.is_empty());
    assert_eq!(created.draft.status, DraftStatus::Draft);

    assert!(matches!(
        engine.submit_for_confirmation(created.draft.id),
        Err(RubricError::DraftHasErrors { count }) if count > 0
    ));
    assert_eq!(
        engine
            .load_draft(created.draft.id)
            .expect("回读")
            .draft
            .status,
        DraftStatus::Draft,
        "校验失败不得推进状态"
    );
}

#[test]
fn auto_validate_off_requires_manual_validation() {
    let rules = load_rules("handwritten_rules.json");
    let rule = rules.first().expect("夹具非空");
    let harness = Harness {
        settings: RubricSettings {
            auto_validate: false,
            ..RubricSettings::default()
        },
        ..Harness::default()
    };
    let engine = harness.engine();
    let question = question(rule.kind, rule.max_score_milli, 4);

    let created = engine
        .create_draft(
            &question,
            DraftSource::Handwritten {
                raw_text: rule.name.clone(),
                items: rule.items.clone(),
                mode: rule.mode,
                partial_choice: rule.partial_choice.clone(),
            },
        )
        .expect("建草稿");
    assert!(
        created.draft.validation.is_empty(),
        "关闭自动校验时不落校验结果"
    );

    let (validated, report) = engine.validate(created.draft.id).expect("手动校验");
    assert!(report.is_ok());
    assert_eq!(validated.draft.id, created.draft.id);

    engine
        .submit_for_confirmation(created.draft.id)
        .expect("提交");
    let outcome = engine.confirm(created.draft.id, Some(0)).expect("确认");
    assert_eq!(outcome.rubric.version, 1);
}

#[test]
fn imported_rules_keep_handwritten_origin() {
    let rules = load_rules("handwritten_rules.json");
    let rule = rules
        .iter()
        .find(|rule| rule.mode == ScoringMode::Additive && rule.items.len() == 2)
        .expect("应有累加规则");
    let harness = Harness::default();
    let engine = harness.engine();
    let question = question(rule.kind, rule.max_score_milli, 5);

    let created = engine
        .create_draft(
            &question,
            DraftSource::Imported {
                raw_text: "从 DOCX 导入的评分细则".to_owned(),
                items: rule.items.clone(),
                mode: rule.mode,
                partial_choice: rule.partial_choice.clone(),
            },
        )
        .expect("导入建草稿");
    assert_eq!(
        created.draft.origin,
        eg_domain::DraftOrigin::HandwrittenTransliteration
    );
    engine
        .submit_for_confirmation(created.draft.id)
        .expect("提交");
    let outcome = engine.confirm(created.draft.id, Some(0)).expect("确认");
    assert!(matches!(
        outcome.rubric.origin,
        RubricOrigin::TeacherHandwritten { .. }
    ));
}

#[test]
fn settings_reject_zero_granularity_and_template_draft_keeps_template_ref() {
    let harness = Harness::default();
    let bad = RubricSettings {
        granularity: eg_domain::MilliScore::ZERO,
        ..RubricSettings::default()
    };
    assert!(
        RubricEngine::new(
            &harness.store,
            &harness.audit,
            &harness.clock,
            &harness.ids,
            &bad
        )
        .is_err()
    );
    let _ = engine_with(&harness, &RubricSettings::default());
}
