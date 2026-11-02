//! 规则整理（structurer）：严格 Schema、机械修复至多 1 次、只产草稿、畸形输出拒绝。
//!
//! 端口用夹具模型输出替代 `eg-inference`（T25 的 app 适配器接线前）；
//! 夹具覆盖正常输出与各类畸形输出（语法错误/未知字段/缺字段/越界依赖/成环/占位判据/超量）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    clippy::items_after_statements,
    clippy::assigning_clones,
    clippy::assert_is_empty,
    clippy::semicolon_if_nothing_returned
)]

mod common;

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::Harness;
use eg_domain::{
    DraftOrigin, DraftStatus, ModelCallId, QuestionKind, RubricOrigin, Sha256Hex, Ulid,
};
use eg_rubric::{
    CheckId, RubricAction, RubricError, StructurerAttempt, StructurerInvocation, StructurerPort,
    StructurerReply, StructurerRequest, infer_mode, parse_structurer_output,
    render_structurer_prompt,
};

fn read_fixture(name: &str) -> String {
    let path = common::fixtures_dir().join("model_outputs").join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("读取模型输出夹具 {} 失败：{error}", path.display()))
}

#[derive(Debug, Clone, PartialEq)]
struct InvocationRecord {
    prompt_hash: Sha256Hex,
    prompt_len: usize,
    attempt: StructurerAttempt,
    previous_output: Option<String>,
    violations: Vec<String>,
}

/// 夹具 structurer：按顺序返回输出，记录每次调用。
struct FixtureStructurer {
    outputs: Vec<String>,
    calls: AtomicUsize,
    records: Mutex<Vec<InvocationRecord>>,
}

impl FixtureStructurer {
    fn new(outputs: Vec<String>) -> Self {
        Self {
            outputs,
            calls: AtomicUsize::new(0),
            records: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn records(&self) -> Vec<InvocationRecord> {
        self.records
            .lock()
            .map(|records| records.clone())
            .unwrap_or_default()
    }
}

impl StructurerPort for FixtureStructurer {
    fn structure(
        &self,
        invocation: &StructurerInvocation<'_>,
    ) -> eg_rubric::Result<StructurerReply> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut records) = self.records.lock() {
            records.push(InvocationRecord {
                prompt_hash: invocation.prompt.hash,
                prompt_len: invocation.prompt.text.chars().count(),
                attempt: invocation.attempt,
                previous_output: invocation.previous_output.map(ToOwned::to_owned),
                violations: invocation.violations.to_vec(),
            });
        }
        if self.outputs.is_empty() {
            return Err(RubricError::structurer("夹具未配置模型输出"));
        }
        let index = call.min(self.outputs.len() - 1);
        Ok(StructurerReply {
            model_call_id: ModelCallId::from_ulid(Ulid::from_u128(9_000 + call as u128)),
            raw_output: self.outputs[index].clone(),
        })
    }
}

#[test]
fn model_output_creates_draft_only_and_confirm_stays_manual() {
    let harness = Harness::default();
    let engine = harness.engine();
    let question = common::question(QuestionKind::Calculation, 13_000, 1);
    let port = FixtureStructurer::new(vec![read_fixture("valid_calculation.json")]);

    let draft = engine
        .create_draft_from_model(
            &question,
            "第一步 3.9 分，第二步 5.2 分，结论 3.9 分",
            &port,
            None,
        )
        .expect("合法输出应生成草稿");
    assert_eq!(draft.draft.origin, DraftOrigin::ModelStructured);
    assert_eq!(draft.draft.status, DraftStatus::Draft);
    assert!(draft.draft.model_call_id.is_some());
    assert_eq!(draft.mode, eg_domain::ScoringMode::Stepwise);
    assert!(draft.partial_choice.is_none());
    assert_eq!(draft.draft.items.len(), 3);
    assert_eq!(draft.draft.items[0].points.as_milli(), 3900);
    assert_eq!(draft.draft.items[1].depends_on, vec![1]);
    assert!(draft.draft.validation.is_empty(), "合法输出不应有校验问题");
    assert!(
        engine.active_rubric(question.id).expect("回读").is_none(),
        "模型整理绝不能直接生成生效规则"
    );

    // 审计：草稿创建 + 结构整理调用；模型调用 id 与提示词哈希可追溯。
    let events = harness.audit.events();
    assert_eq!(events.len(), 2);
    let structured = events
        .iter()
        .find(|event| event.action == RubricAction::ModelStructured)
        .expect("应有结构整理审计");
    assert_eq!(structured.draft_id, draft.draft.id);
    assert_eq!(structured.model_call_id, draft.draft.model_call_id);
    assert!(structured.prompt_hash.is_some());
    assert!(
        structured
            .note
            .as_deref()
            .is_some_and(|note| note.contains("Schema v1"))
    );

    // 教师显式确认后才生效。
    engine
        .submit_for_confirmation(draft.draft.id)
        .expect("提交待确认");
    let outcome = engine.confirm(draft.draft.id, Some(0)).expect("确认");
    assert_eq!(outcome.rubric.version, 1);
    match outcome.rubric.origin {
        RubricOrigin::ModelStructured { model_call_id, .. } => {
            assert_eq!(Some(model_call_id), draft.draft.model_call_id)
        }
        other => panic!("来源应为 ModelStructured，实际 {other:?}"),
    }
}

#[test]
fn malformed_output_is_rejected_after_single_repair() {
    let malformed = read_fixture("malformed_json.json");
    let harness = Harness::default();
    let engine = harness.engine();
    let question = common::question(QuestionKind::Calculation, 13_000, 2);
    let port = FixtureStructurer::new(vec![malformed.clone(), malformed.clone()]);

    let error = engine
        .create_draft_from_model(&question, "原文保留：第一步 …", &port, None)
        .expect_err("两次都不合法必须拒绝");
    match error {
        RubricError::ModelOutputRejected {
            attempts,
            violations,
            summary,
            prompt_hash,
            model_call_id,
        } => {
            assert_eq!(attempts, 2, "首次 + 机械修复一次");
            assert!(!violations.is_empty());
            assert!(!summary.is_empty());
            assert!(model_call_id.is_some());
            assert_eq!(
                prompt_hash,
                render_structurer_prompt(&StructurerRequest {
                    raw_text: "原文保留：第一步 …",
                    question_kind: QuestionKind::Calculation,
                    max_score: question.max_score,
                    template_skeleton: None,
                })
                .hash
            );
        }
        other => panic!("应为 ModelOutputRejected，实际 {other:?}"),
    }
    assert_eq!(port.calls(), 2, "机械修复至多 1 次，不得循环重试");
    // 不生成草稿（原文与模型输出由调用方/模型调用记录保留）。
    assert_eq!(harness.store.draft_count().expect("统计草稿"), 0);
    assert!(harness.audit.events().is_empty());

    let records = port.records();
    assert_eq!(records[0].attempt, StructurerAttempt::Initial);
    assert_eq!(records[1].attempt, StructurerAttempt::Repair);
    assert!(records[1].previous_output.is_some());
    assert!(!records[1].violations.is_empty());
}

#[test]
fn repair_once_succeeds_when_second_output_is_valid() {
    let harness = Harness::default();
    let engine = harness.engine();
    let question = common::question(QuestionKind::Calculation, 13_000, 3);
    let port = FixtureStructurer::new(vec![
        read_fixture("unknown_field.json"),
        read_fixture("valid_calculation.json"),
    ]);

    let draft = engine
        .create_draft_from_model(&question, "第一步 3.9 分…", &port, Some("三段骨架"))
        .expect("修复后合法应生成草稿");
    assert_eq!(port.calls(), 2);
    let records = port.records();
    assert_eq!(records[1].attempt, StructurerAttempt::Repair);
    assert!(
        records[1]
            .violations
            .iter()
            .any(|violation| violation.contains("严格 Schema")),
        "修复反馈应包含结构违规摘要"
    );
    assert!(
        records[0].prompt_hash == records[1].prompt_hash,
        "修复沿用同一提示词哈希"
    );

    let structured = harness
        .audit
        .events()
        .into_iter()
        .find(|event| event.action == RubricAction::ModelStructured)
        .expect("应有结构整理审计");
    assert!(
        structured
            .note
            .as_deref()
            .is_some_and(|note| note.contains("尝试 2 次")),
        "审计应记录尝试次数"
    );
    assert_eq!(draft.draft.items.len(), 3);
}

#[test]
fn strict_schema_rejects_each_malformation_without_draft() {
    let cases = [
        ("unknown_field.json", "未知字段"),
        ("missing_criterion.json", "criterion"),
        ("zero_points.json", "points"),
        ("out_of_range_depends.json", "depends_on"),
        ("cycle_depends.json", "成环"),
        ("placeholder_criterion.json", "模板占位"),
        ("too_many_items.json", "超过上限"),
    ];
    let harness = Harness::default();
    let engine = harness.engine();
    for (index, (file, keyword)) in cases.iter().enumerate() {
        let raw = read_fixture(file);
        let violations = parse_structurer_output(&raw).expect_err("畸形输出必须被严格 Schema 拒绝");
        assert!(
            violations
                .iter()
                .any(|violation| violation.contains(keyword)),
            "{file} 违规应包含「{keyword}」，实际 {violations:?}"
        );

        let ordinal = u32::try_from(index).expect("用例数有限") + 10;
        let question = common::question(QuestionKind::Calculation, 13_000, ordinal);
        let port = FixtureStructurer::new(vec![raw.clone(), raw]);
        assert!(
            engine
                .create_draft_from_model(&question, "原文", &port, None)
                .is_err(),
            "{file} 不得生成草稿"
        );
        assert_eq!(port.calls(), 2, "{file} 修复一次后放弃");
    }
    assert_eq!(harness.store.draft_count().expect("统计草稿"), 0);
}

#[test]
fn bad_total_is_a_draft_validation_issue_not_a_schema_rejection() {
    let harness = Harness::default();
    let engine = harness.engine();
    let question = common::question(QuestionKind::Calculation, 13_000, 20);
    let port = FixtureStructurer::new(vec![read_fixture("bad_total.json")]);

    let draft = engine
        .create_draft_from_model(&question, "两步合计 9 分，但本题 13 分", &port, None)
        .expect("结构合法但分值与满分不符，应生成草稿并留给教师修正");
    let report = engine.report_for(&draft);
    assert!(report.has(CheckId::R01Total));
    assert_ne!(draft.draft.validation, Vec::new());
    assert!(matches!(
        engine.submit_for_confirmation(draft.draft.id),
        Err(RubricError::DraftHasErrors { .. })
    ));
}

#[test]
fn multiple_choice_model_output_uses_all_or_nothing() {
    let harness = Harness::default();
    let engine = harness.engine();
    let question = common::question(QuestionKind::MultipleChoice, 6_000, 21);
    let port = FixtureStructurer::new(vec![read_fixture("valid_multiple_choice.json")]);

    let draft = engine
        .create_draft_from_model(&question, "全部选对得 6 分，有错选 0 分", &port, None)
        .expect("合法多选输出应生成草稿");
    assert_eq!(draft.mode, eg_domain::ScoringMode::AllOrNothing);
    assert!(draft.partial_choice.is_none(), "模型不产出部分给分档位");
    assert_eq!(harness.store.draft_count().expect("统计"), 1);
}

#[test]
fn empty_raw_text_and_port_failure_are_explicit() {
    let harness = Harness::default();
    let engine = harness.engine();
    let question = common::question(QuestionKind::Calculation, 1_000, 30);
    let port = FixtureStructurer::new(vec![read_fixture("valid_calculation.json")]);

    assert!(matches!(
        engine.create_draft_from_model(&question, "   ", &port, None),
        Err(RubricError::Config { .. })
    ));
    assert_eq!(port.calls(), 0, "原文为空不得发起模型调用");

    struct FailingPort;
    impl StructurerPort for FailingPort {
        fn structure(
            &self,
            _invocation: &StructurerInvocation<'_>,
        ) -> eg_rubric::Result<StructurerReply> {
            Err(RubricError::structurer("模型不可用（夹具）"))
        }
    }
    assert!(matches!(
        engine.create_draft_from_model(&question, "原文", &FailingPort, None),
        Err(RubricError::Structurer { .. })
    ));
    assert_eq!(harness.store.draft_count().expect("统计"), 0);
}

#[test]
fn prompt_is_deterministic_hashed_and_contains_context() {
    let request = StructurerRequest {
        raw_text: "第一步 2 分，第二步 3 分",
        question_kind: QuestionKind::Calculation,
        max_score: eg_domain::MilliScore::from_milli(5_000).expect("满分"),
        template_skeleton: None,
    };
    let first = render_structurer_prompt(&request);
    let second = render_structurer_prompt(&request);
    assert_eq!(first, second);
    assert_eq!(first.hash, Sha256Hex::of(first.text.as_bytes()));
    assert_eq!(first.schema_version, 1);
    assert!(first.text.contains("Schema v1"));
    assert!(first.text.contains("Calculation"));
    assert!(first.text.contains("5000"));
    assert!(first.text.contains("第一步 2 分"));

    let other = render_structurer_prompt(&StructurerRequest {
        raw_text: "另一份规则",
        ..request
    });
    assert_ne!(first.hash, other.hash);
}

#[test]
fn infer_mode_heuristic_matches_item_dependencies() {
    let item = |points: i64, depends: Vec<u32>| eg_domain::RubricDraftItem {
        criterion: "判据".to_owned(),
        points: eg_domain::MilliScore::from_milli(points).expect("分值"),
        required: false,
        depends_on: depends,
    };
    assert_eq!(
        infer_mode(QuestionKind::SingleChoice, &[item(5000, Vec::new())]),
        eg_domain::ScoringMode::AllOrNothing
    );
    assert_eq!(
        infer_mode(
            QuestionKind::Calculation,
            &[item(5000, Vec::new()), item(5000, vec![])]
        ),
        eg_domain::ScoringMode::Additive
    );
    assert_eq!(
        infer_mode(
            QuestionKind::Calculation,
            &[item(5000, Vec::new()), item(5000, vec![1])]
        ),
        eg_domain::ScoringMode::Stepwise
    );
}
