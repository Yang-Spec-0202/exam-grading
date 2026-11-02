#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    clippy::uninlined_format_args,
    clippy::bool_assert_comparison,
    clippy::useless_vec,
    clippy::manual_let_else,
    clippy::float_cmp,
    clippy::too_many_arguments,
    dead_code,
    unreachable_pub,
    reason = "夹具与断言：允许 unwrap/索引/数值转换"
)]

//! T29 验收：三级区域对齐、置信度公式、模板版本漂移降级、跨页 parts、缺页显式、
//! 门禁拦截、指纹可复现。

use eg_align::testing::{
    FixedClock, classroom, page, page_refs, paper, recognition, recognition_with_anchors, roster,
    strip, student, submission, template,
};
use eg_align::{
    AlignRequest, AlignSettings, IdPort, MapRecognitionSource, RegionPath, SeqIds, UnresolvedKind,
    align,
};
use eg_domain::{Submission, Ulid};

fn classroom_id() -> eg_domain::ClassroomId {
    classroom(7)
}

fn fixed_clock() -> FixedClock {
    FixedClock::new(eg_domain::time::parse_rfc3339("2026-11-02T00:00:00Z").expect("固定时间"))
}

/// 3 题 2 页的标准环境；`recognitions`/`template` 决定路径。
fn run(
    submission: &Submission,
    settings: &AlignSettings,
    template: Option<&eg_domain::PaperTemplate>,
    recognitions: &MapRecognitionSource,
    ids: &dyn IdPort,
) -> eg_align::AlignReport {
    let roster = roster(
        classroom_id(),
        vec![student(classroom_id(), 1, "20260001", "张三")],
    )
    .expect("名单");
    let paper = paper(10, classroom_id(), &["1", "2", "3"]);
    let pages = page_refs(submission);
    let clock = fixed_clock();
    align(AlignRequest {
        submission,
        paper: &paper,
        roster: &roster,
        template,
        pages: &pages,
        recognitions,
        hints: &eg_align::NoIdentityHints,
        settings,
        ids,
        clock: &clock,
        existing: &[],
    })
    .expect("对齐不失败")
}

fn two_page_submission() -> Submission {
    submission(
        100,
        classroom_id(),
        10,
        "张三",
        &["第 1 页内容", "第 2 页内容"],
    )
}

fn recognitions_for(submission: &Submission) -> MapRecognitionSource {
    let pages = page_refs(submission);
    MapRecognitionSource::new(vec![
        recognition_with_anchors(pages[0].page_id, &[("1 计算", 0.10, 0.05)]),
        recognition_with_anchors(
            pages[1].page_id,
            &[("2 计算", 0.10, 0.05), ("3 计算", 0.40, 0.05)],
        ),
    ])
    .expect("识别夹具")
}

#[test]
fn template_path_uses_versioned_regions_and_unlocks_auto_grade() {
    let submission = two_page_submission();
    let paper = paper(10, classroom_id(), &["1", "2", "3"]);
    let regions = vec![
        (0usize, 1u32, strip(0.10, 0.20)),
        (1, 2, strip(0.10, 0.20)),
        (2, 2, strip(0.40, 0.20)),
    ];
    let template = template(&paper, 1, 2, &regions);
    let recognitions = recognitions_for(&submission);
    let settings = AlignSettings::default();
    let ids = SeqIds::new(1);
    let report = run(&submission, &settings, Some(&template), &recognitions, &ids);

    assert!(report.identity.status.is_confirmed());
    assert_eq!(report.template_disabled, None);
    assert!(report.metrics.template_used);
    assert_eq!(report.question_summaries.len(), 3);
    for summary in &report.question_summaries {
        assert_eq!(summary.path, RegionPath::Template);
        assert_eq!(summary.confidence.get(), 1.0);
        assert!(summary.auto_grade_allowed);
    }
    assert!(
        report.unresolved.is_empty(),
        "模板路径不应有未决：{:?}",
        report.unresolved
    );
    assert!(
        report.auto_grade_ready,
        "模板 + 身份确认 + 区域 1.0 应放行自动判分"
    );
    assert_eq!(report.answers.len(), 3);
}

#[test]
fn template_version_mismatch_disables_template_and_falls_back_to_layout() {
    let submission = two_page_submission();
    let paper = paper(10, classroom_id(), &["1", "2", "3"]);
    let regions = vec![
        (0usize, 1u32, strip(0.10, 0.20)),
        (1, 2, strip(0.10, 0.20)),
        (2, 2, strip(0.40, 0.20)),
    ];
    let stale = template(&paper, 2, 2, &regions); // 模板版本 ≠ key_version(1)
    let recognitions = recognitions_for(&submission);
    let settings = AlignSettings::default();
    let ids = SeqIds::new(1);
    let report = run(&submission, &settings, Some(&stale), &recognitions, &ids);

    let reason = report.template_disabled.expect("模板必须被显式禁用");
    assert!(
        reason.contains("版本不匹配") || reason.contains("TemplateVersionMismatch"),
        "禁用原因需可读：{reason}"
    );
    assert!(!report.metrics.template_used);
    // 3/3 锚点、顺序单调 → 0.5+0.4+0.1=1.0，规范上限 0.950。
    for summary in &report.question_summaries {
        assert_eq!(summary.path, RegionPath::Layout, "应降级到版面检测");
        assert_eq!(summary.confidence.get(), 0.950);
        assert!(summary.auto_grade_allowed);
    }
    assert_eq!(
        report.segments[0].method,
        eg_domain::SegmentMethod::LayoutDetection
    );
    assert!(report.auto_grade_ready);
}

#[test]
fn sparse_anchors_below_floor_block_auto_grade() {
    let submission = two_page_submission();
    let pages = page_refs(&submission);
    let recognitions = MapRecognitionSource::new(vec![
        recognition_with_anchors(pages[0].page_id, &[("1 计算", 0.10, 0.05)]),
        recognition(
            pages[1].page_id,
            vec![eg_align::testing::block("没有题号", 0.1, 0.05)],
        ),
    ])
    .expect("识别夹具");
    let settings = AlignSettings::default();
    let ids = SeqIds::new(1);
    let report = run(&submission, &settings, None, &recognitions, &ids);

    // 精度 1/3、单调 1.0 → 0.5+0.1333+0.1 = 0.733，低于 0.80 门限。
    let aligned = report
        .question_summaries
        .iter()
        .find(|summary| summary.ordinal == 1)
        .expect("第 1 题摘要");
    assert_eq!(aligned.path, RegionPath::Layout);
    assert_eq!(aligned.confidence.get(), 0.733);
    assert!(!aligned.auto_grade_allowed, "低于门限必须拦截");
    assert!(
        report
            .unresolved
            .iter()
            .any(|item| item.kind == UnresolvedKind::RegionUncertain)
    );
    assert!(!report.auto_grade_ready);
    // 未对齐题目必须可见（path=None, 无答案），不能当成「未作答」。
    let missing = report
        .question_summaries
        .iter()
        .find(|summary| summary.ordinal == 2)
        .expect("第 2 题摘要");
    assert_eq!(missing.path, RegionPath::None);
    assert!(!missing.auto_grade_allowed);
    assert_eq!(report.answers.len(), 1);
}

#[test]
fn cross_page_question_keeps_one_part_per_page_in_order() {
    let submission = two_page_submission();
    let pages = page_refs(&submission);
    let recognitions = MapRecognitionSource::new(vec![
        recognition_with_anchors(
            pages[0].page_id,
            &[("1 计算", 0.05, 0.05), ("2 解答", 0.60, 0.05)],
        ),
        recognition_with_anchors(
            pages[1].page_id,
            &[("2 解答（续）", 0.05, 0.05), ("3 计算", 0.40, 0.05)],
        ),
    ])
    .expect("识别夹具");
    let settings = AlignSettings::default();
    let ids = SeqIds::new(1);
    let report = run(&submission, &settings, None, &recognitions, &ids);

    let answer = report
        .answers
        .iter()
        .find(|answer| {
            report
                .question_summaries
                .iter()
                .any(|summary| summary.question_id == answer.question_id && summary.ordinal == 2)
        })
        .expect("第 2 题应构造答案");
    assert_eq!(answer.parts.len(), 2, "跨页题每页一段");
    assert_eq!(
        answer.parts[0].page_id, pages[0].page_id,
        "parts 必须按页序（先第 1 页）"
    );
    assert_eq!(answer.parts[1].page_id, pages[1].page_id);
    assert!(answer.raw.text.contains("2 解答"), "合并文本按页序");
    assert!(answer.recognition_confidence.get() > 0.0);
    assert!(
        report
            .unresolved
            .iter()
            .all(|item| item.kind != UnresolvedKind::MissingPage),
        "连续两页不应报缺页"
    );
}

/// 第 2 题跨「第 1 页 + 第 3 页」（提交缺第 2 页）：不合并 + 显式缺页。
fn gapped_submission() -> Submission {
    let mut base = submission(200, classroom_id(), 10, "张三", &["占位"]);
    base.pages = vec![page(200, 1, "第 1 页内容"), page(200, 3, "第 3 页内容")];
    base
}

#[test]
fn missing_middle_page_is_explicit_and_not_merged() {
    let submission = gapped_submission();
    let pages = page_refs(&submission);
    let recognitions = MapRecognitionSource::new(vec![
        recognition_with_anchors(
            pages[0].page_id,
            &[("1 计算", 0.05, 0.05), ("2 解答", 0.60, 0.05)],
        ),
        recognition_with_anchors(
            pages[1].page_id,
            &[("2 解答（续）", 0.05, 0.05), ("3 计算", 0.60, 0.05)],
        ),
    ])
    .expect("识别夹具");
    let settings = AlignSettings::default();
    let ids = SeqIds::new(1);
    let report = run(&submission, &settings, None, &recognitions, &ids);

    let gap = report
        .unresolved
        .iter()
        .find(|item| item.kind == UnresolvedKind::MissingPage)
        .expect("缺页必须显式");
    assert!(
        report.unresolved.iter().any(
            |item| item.kind == UnresolvedKind::MissingPage && item.detail.contains("缺中间页")
        ),
        "跨页缺中间页需单独说明：{:?}",
        report.unresolved
    );
    let _ = gap;
    let answer = report
        .answers
        .iter()
        .find(|answer| {
            report
                .question_summaries
                .iter()
                .any(|summary| summary.question_id == answer.question_id && summary.ordinal == 2)
        })
        .expect("证据仍保留");
    assert_eq!(answer.parts.len(), 2);
    assert!(answer.raw.text.is_empty(), "缺中间页不得合并文本");
    assert!(!report.auto_grade_ready);
}

#[test]
fn fingerprint_is_reproducible_and_independent_of_generated_ids() {
    let submission = two_page_submission();
    let recognitions = recognitions_for(&submission);
    let settings = AlignSettings::default();
    let ids_a = SeqIds::new(1);
    let first = run(&submission, &settings, None, &recognitions, &ids_a);
    let ids_b = SeqIds::new(10_000);
    let second = run(&submission, &settings, None, &recognitions, &ids_b);
    assert_eq!(first.fingerprint.input_hash, second.fingerprint.input_hash);
    assert_eq!(
        first.fingerprint.output_hash, second.fingerprint.output_hash,
        "生成 id 属存储坐标，不得影响语义输出哈希"
    );
    assert_eq!(
        first.metrics.aligned_questions,
        second.metrics.aligned_questions
    );
    assert_eq!(
        first.fingerprint.algorithm_version,
        eg_align::ALGORITHM_VERSION
    );
}

#[test]
fn duplicate_submission_blocks_auto_grade() {
    let submission = two_page_submission();
    let recognitions = recognitions_for(&submission);
    let settings = AlignSettings::default();
    let ids = SeqIds::new(1);
    let roster = roster(
        classroom_id(),
        vec![student(classroom_id(), 1, "20260001", "张三")],
    )
    .expect("名单");
    let paper = paper(10, classroom_id(), &["1", "2", "3"]);
    let pages = page_refs(&submission);
    let clock = fixed_clock();
    let existing = vec![eg_align::ExistingSubmission {
        submission_id: eg_domain::SubmissionId::from_ulid(Ulid::from_u128(999)),
        page_assets: submission.pages.iter().map(|page| page.asset).collect(),
    }];
    let report = align(AlignRequest {
        submission: &submission,
        paper: &paper,
        roster: &roster,
        template: None,
        pages: &pages,
        recognitions: &recognitions,
        hints: &eg_align::NoIdentityHints,
        settings: &settings,
        ids: &ids,
        clock: &clock,
        existing: &existing,
    })
    .expect("对齐");
    assert_eq!(
        report.duplicate,
        eg_align::DuplicateVerdict::Identical {
            first: eg_domain::SubmissionId::from_ulid(Ulid::from_u128(999))
        }
    );
    assert!(!report.auto_grade_ready, "重复提交必须拦截自动判分");
    assert!(
        report
            .unresolved
            .iter()
            .any(|item| item.kind == UnresolvedKind::DuplicateSubmission)
    );
}
