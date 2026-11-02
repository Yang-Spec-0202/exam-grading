//! 反例夹具：20 条静态校验码（11 基础 + 9 部分给分专项）逐一命中。
//!
//! 每个反例都断言：命中集合与期望完全一致（不多报、不漏报），
//! 且错误级问题会阻止草稿进入待确认；模板来源反例只走 `report_for`（不经服务层造来源）。
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

use std::collections::BTreeSet;

use common::{Harness, engine_with, load_rules, question, stored_draft};
use eg_domain::DraftStatus;
use eg_rubric::{CheckId, DraftSource, RubricError};

/// 全部 20 条校验码（`SPEC-rubric.md` §4：11 基础 + 9 部分给分专项）。
fn all_check_codes() -> BTreeSet<String> {
    [
        CheckId::R01Total,
        CheckId::R02Granularity,
        CheckId::R03PositivePoints,
        CheckId::R04AllOrNothingItems,
        CheckId::R05DependencyMissing,
        CheckId::R06DependencyCycle,
        CheckId::R07CriterionPlaceholder,
        CheckId::R08KindUnsupported,
        CheckId::R09StepwisePrefix,
        CheckId::R10DuplicateCriterion,
        CheckId::R11TemplateRefMissing,
        CheckId::P01ModeExclusive,
        CheckId::P02TierCoverage,
        CheckId::P03TierPoints,
        CheckId::P04TierGranularity,
        CheckId::P05TierSource,
        CheckId::P06SwitchOff,
        CheckId::P07KindOnlyMultiple,
        CheckId::P08FullItem,
        CheckId::P09WrongSelectionZero,
    ]
    .into_iter()
    .map(|code| CheckId::code(code).to_owned())
    .collect()
}

#[test]
fn counterexamples_hit_exactly_expected_codes() {
    let rules = load_rules("counterexamples.json");
    assert_eq!(rules.len(), 21, "反例夹具应为 21 条（含 R09 警告反例）");
    let harness = Harness::default();
    let mut covered: BTreeSet<String> = BTreeSet::new();

    for (index, rule) in rules.iter().enumerate() {
        let settings = rule.settings();
        let engine = engine_with(&harness, &settings);
        let ordinal = u32::try_from(index + 1).expect("夹具条数有限");
        let stored = stored_draft(rule, ordinal);
        let report = engine.report_for(&stored);

        let observed: BTreeSet<String> = report.codes().into_iter().map(str::to_owned).collect();
        let mut expected: BTreeSet<String> = BTreeSet::new();
        expected.extend(rule.expect.errors.iter().cloned());
        expected.extend(rule.expect.warnings.iter().cloned());
        assert_eq!(
            observed, expected,
            "反例「{}」命中码不符：期望 {expected:?}，实际 {observed:?}",
            rule.name
        );
        covered.extend(expected);
    }

    assert_eq!(
        covered,
        all_check_codes(),
        "夹具必须覆盖全部 20 条校验码，缺口 {:?}",
        all_check_codes().difference(&covered).collect::<Vec<_>>()
    );
}

#[test]
fn error_level_counterexamples_block_submission() {
    let rules = load_rules("counterexamples.json");
    let harness = Harness::default();

    for (index, rule) in rules.iter().enumerate() {
        if !rule.expect.errors.is_empty() {
            // 模板来源反例需要真实模板引用，无法从服务层构造，跳过（已由 report_for 覆盖）。
            if rule.origin.as_deref() == Some("template_applied") {
                continue;
            }
            let settings = rule.settings();
            let engine = engine_with(&harness, &settings);
            let ordinal = u32::try_from(index + 1).expect("夹具条数有限");
            let question = question(rule.kind, rule.max_score_milli, ordinal);
            let created = match engine.create_draft(
                &question,
                DraftSource::Handwritten {
                    raw_text: rule.name.clone(),
                    items: rule.items.clone(),
                    mode: rule.mode,
                    partial_choice: rule.partial_choice.clone(),
                },
            ) {
                Ok(created) => created,
                // 部分反例连领域草稿校验都过不了（例如 0 分评分项），
                // 属于更早的门禁：同样不得产生可确认草稿。
                Err(RubricError::Domain(_)) => continue,
                Err(error) => panic!("{} 建草稿失败：{error}", rule.name),
            };
            assert_eq!(
                created.draft.status,
                DraftStatus::Draft,
                "{} 存在错误级问题，状态不应是待确认",
                rule.name
            );
            assert!(
                !created.draft.validation.is_empty(),
                "{} 自动校验结果应落库",
                rule.name
            );
            assert!(
                matches!(
                    engine.submit_for_confirmation(created.draft.id),
                    Err(RubricError::DraftHasErrors { .. })
                ),
                "{} 存在错误级问题，提交待确认必须失败",
                rule.name
            );
            // 确认入口同样被拦（先提交失败，直接确认按状态拒绝）。
            assert!(engine.confirm(created.draft.id, None).is_err());
        }
    }
}

#[test]
fn warning_only_counterexamples_are_confirmable() {
    let rules = load_rules("counterexamples.json");
    let harness = Harness::default();

    for (index, rule) in rules.iter().enumerate() {
        if !rule.expect.errors.is_empty() {
            continue;
        }
        let settings = rule.settings();
        let engine = engine_with(&harness, &settings);
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
        assert!(
            !created.draft.validation.is_empty(),
            "{} 警告也应落库",
            rule.name
        );
        assert!(
            created
                .draft
                .validation
                .iter()
                .all(|issue| issue.severity == eg_domain::RubricIssueSeverity::Warning),
            "{} 只应包含警告级问题",
            rule.name
        );
        engine
            .submit_for_confirmation(created.draft.id)
            .unwrap_or_else(|error| panic!("{} 警告不应阻止提交：{error}", rule.name));
        let outcome = engine
            .confirm(created.draft.id, Some(0))
            .unwrap_or_else(|error| panic!("{} 警告不应阻止确认：{error}", rule.name));
        assert_eq!(outcome.rubric.version, 1);
    }
}
