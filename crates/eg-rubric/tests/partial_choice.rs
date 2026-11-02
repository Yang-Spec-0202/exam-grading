//! 多选部分给分专项（`SPEC-rubric.md` §4 P01–P09；`SPEC-domain.md` §4 不变量 6–9/11）。
//!
//! 官方口径（现行全国卷 2/3 选项）与教师自定义口径各取正例，
//! 再用服务层走完 建草稿 → 提交 → 确认，断言档位原样进入生效规则。
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

use common::{Harness, engine_with, question, timestamp};
use eg_domain::{
    DraftOrigin, DraftStatus, MilliScore, PartialChoiceRule, PartialChoiceSource,
    PartialChoiceTier, RubricDraft, RubricDraftItem, RubricId, ScoringMode, Ulid,
};
use eg_rubric::{CheckId, DraftSource, RubricError, RubricSettings, StoredDraft};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct VariantFile {
    variants: Vec<Variant>,
}

#[derive(Debug, Deserialize)]
struct Variant {
    name: String,
    correct_option_count: u32,
    total_milli: i64,
    tiers: Vec<TierFixture>,
    source: PartialChoiceSource,
    source_note: Option<String>,
    wrong_selection_zero: bool,
    expect: String,
}

#[derive(Debug, Deserialize)]
struct TierFixture {
    selected_correct: u32,
    points: i64,
}

impl Variant {
    fn rule(&self) -> PartialChoiceRule {
        PartialChoiceRule {
            correct_option_count: self.correct_option_count,
            tiers: self
                .tiers
                .iter()
                .map(|tier| PartialChoiceTier {
                    selected_correct: tier.selected_correct,
                    points: MilliScore::from_milli(tier.points).expect("夹具分值合法"),
                    label: None,
                })
                .collect(),
            wrong_selection_zero: self.wrong_selection_zero,
            source: self.source,
            source_note: self.source_note.clone(),
            total_milli: MilliScore::from_milli(self.total_milli).expect("夹具满分合法"),
        }
    }

    fn items(&self) -> Vec<RubricDraftItem> {
        vec![RubricDraftItem {
            criterion: "全部选对得满分；有错选 0 分".to_owned(),
            points: MilliScore::from_milli(self.total_milli).expect("夹具满分合法"),
            required: true,
            depends_on: Vec::new(),
        }]
    }

    fn stored(&self, ordinal: u32) -> StoredDraft {
        StoredDraft {
            draft: RubricDraft {
                id: RubricId::from_ulid(Ulid::from_u128(u128::from(2000 + ordinal))),
                question_id: eg_domain::QuestionId::from_ulid(Ulid::from_u128(u128::from(
                    400 + ordinal,
                ))),
                status: DraftStatus::PendingConfirmation,
                items: self.items(),
                raw_text: self.name.clone(),
                origin: DraftOrigin::HandwrittenTransliteration,
                model_call_id: None,
                validation: Vec::new(),
                created_at: timestamp(),
                confirmed_at: None,
            },
            question_kind: eg_domain::QuestionKind::MultipleChoice,
            max_score: MilliScore::from_milli(self.total_milli).expect("夹具满分合法"),
            mode: ScoringMode::PartialChoice,
            partial_choice: Some(self.rule()),
            template: None,
        }
    }

    fn expected_error(&self) -> Option<&str> {
        self.expect.strip_prefix("error:")
    }
}

fn load_variants() -> Vec<Variant> {
    common::load_json::<VariantFile>("partial_choice.json").variants
}

#[test]
fn partial_choice_pass_variants_validate_and_confirm() {
    let variants = load_variants();
    assert_eq!(
        variants.len(),
        5,
        "部分给分夹具应为 5 条（3 正例 + 2 反例）"
    );
    let harness = Harness::default();

    let mut ordinal = 0_u32;
    for variant in variants.iter().filter(|variant| variant.expect == "pass") {
        ordinal += 1;
        let engine = harness.engine();
        let report = engine.report_for(&variant.stored(ordinal));
        assert!(
            report.is_ok(),
            "{} 不应有错误，实际 {:?}",
            variant.name,
            report.codes()
        );
        assert_eq!(
            report.codes(),
            Vec::<&'static str>::new(),
            "{} 不应产生任何发现",
            variant.name
        );

        // 端到端：建草稿 → 待确认 → 确认。
        let question = question(
            eg_domain::QuestionKind::MultipleChoice,
            variant.total_milli,
            100 + ordinal,
        );
        let created = engine
            .create_draft(
                &question,
                DraftSource::Handwritten {
                    raw_text: variant.name.clone(),
                    items: variant.items(),
                    mode: ScoringMode::PartialChoice,
                    partial_choice: Some(variant.rule()),
                },
            )
            .unwrap_or_else(|error| panic!("{} 建草稿失败：{error}", variant.name));
        assert_eq!(created.draft.status, DraftStatus::Draft);
        engine
            .submit_for_confirmation(created.draft.id)
            .unwrap_or_else(|error| panic!("{} 提交失败：{error}", variant.name));
        let outcome = engine
            .confirm(created.draft.id, Some(0))
            .unwrap_or_else(|error| panic!("{} 确认失败：{error}", variant.name));
        assert_eq!(outcome.rubric.mode, ScoringMode::PartialChoice);
        assert_eq!(outcome.rubric.total.as_milli(), variant.total_milli);
        assert_eq!(
            outcome.rubric.partial_choice,
            Some(variant.rule()),
            "{} 档位应原样写入生效规则",
            variant.name
        );
    }
}

#[test]
fn partial_choice_error_variants_hit_expected_codes() {
    let variants = load_variants();
    let harness = Harness::default();
    let engine = harness.engine();

    let mut ordinal = 0_u32;
    for variant in variants.iter().filter(|variant| variant.expect != "pass") {
        ordinal += 1;
        let expected = variant.expected_error().expect("反例必须声明期望错误码");
        let report = engine.report_for(&variant.stored(ordinal));
        assert!(!report.is_ok(), "{} 应命中错误级问题", variant.name);
        assert!(
            report.codes().contains(&expected),
            "{} 期望命中 {expected}，实际 {:?}",
            variant.name,
            report.codes()
        );
    }
}

#[test]
fn switch_off_blocks_partial_choice_drafts() {
    let variants = load_variants();
    let pass = variants
        .iter()
        .find(|variant| variant.expect == "pass")
        .expect("夹具应有正例");
    let harness = Harness::default();
    let settings = RubricSettings {
        partial_choice_enabled: false,
        ..RubricSettings::default()
    };
    let engine = engine_with(&harness, &settings);
    let rule = pass.rule();
    let stored = pass.stored(1);
    let report = engine.report_for(&stored);
    assert!(report.has(CheckId::P06SwitchOff));
    assert!(!report.is_ok());

    let question = question(
        eg_domain::QuestionKind::MultipleChoice,
        pass.total_milli,
        200,
    );
    let created = engine
        .create_draft(
            &question,
            DraftSource::Handwritten {
                raw_text: pass.name.clone(),
                items: pass.items(),
                mode: ScoringMode::PartialChoice,
                partial_choice: Some(rule),
            },
        )
        .expect("建草稿仍应成功（校验结果落库）");
    assert!(matches!(
        engine.submit_for_confirmation(created.draft.id),
        Err(RubricError::DraftHasErrors { .. })
    ));
}

#[test]
fn partial_choice_is_rejected_for_single_choice() {
    let variants = load_variants();
    let pass = variants
        .iter()
        .find(|variant| variant.expect == "pass")
        .expect("夹具应有正例");
    let harness = Harness::default();
    let engine = harness.engine();
    let rule = pass.rule();
    let question = question(eg_domain::QuestionKind::SingleChoice, pass.total_milli, 201);
    let created = engine
        .create_draft(
            &question,
            DraftSource::Handwritten {
                raw_text: "单选误用部分给分".to_owned(),
                items: pass.items(),
                mode: ScoringMode::PartialChoice,
                partial_choice: Some(rule),
            },
        )
        .expect("建草稿成功");
    assert!(
        created
            .draft
            .validation
            .iter()
            .any(|issue| issue.message.contains("[P07]"))
    );
    assert!(matches!(
        engine.submit_for_confirmation(created.draft.id),
        Err(RubricError::DraftHasErrors { .. })
    ));
}
