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
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    dead_code,
    unreachable_pub,
    reason = "真实 platform 集成测试：允许 unwrap/索引/数值转换"
)]

//! 真实 `eg-platform` 集成（配置快照 / 名单仓储 / 平台 ULID / 人工修正审计）。
//!
//! 覆盖：`settings_from_platform` 默认值一致；`load_roster` 从真实 SQLite 名单加载；
//! 身份未命中 → 人工确认 → 门禁放行；人工审计写入平台审计链且链完整；
//! 人工画框后 `requires_realign=true`。

use std::sync::Arc;

use eg_align::testing::{
    FixedClock, page_refs, paper, recognition, recognition_with_anchors, strip, student,
    submission, template,
};
use eg_align::{
    AlignRequest, AlignSettings, ManualCommand, ManualRequest, PlatformIds, RegionPath, align,
    apply_manual, load_roster, manual_segment_ids, settings_from_platform, verify_audit_chain,
    write_alignment_audit, write_manual_audit,
};
use eg_domain::{Classroom, Student};
use eg_platform::{DataDirs, FixedClock as PlatformClock, Platform, SharedClock, repo};

fn open_platform(root: &std::path::Path) -> (Platform, SharedClock) {
    let dirs = DataDirs::with_root(root).expect("测试数据目录");
    let clock: SharedClock =
        Arc::new(PlatformClock::from_rfc3339("2026-11-02T00:00:00Z").expect("固定时钟"));
    let platform = Platform::open_with(dirs, clock.clone()).expect("打开 platform");
    (platform, clock)
}

fn seed_roster(platform: &Platform, clock: &SharedClock) -> Classroom {
    let classroom = Classroom {
        id: platform.ids().next_classroom_id().expect("班级 id"),
        name: "高二（3）班".to_owned(),
        term: "2026 秋".to_owned(),
    };
    let students: Vec<Student> = vec![
        student(classroom.id, 1, "20260001", "张三"),
        student(classroom.id, 2, "20260002", "李四"),
    ];
    let at = clock.now();
    let classroom_for_db = classroom.clone();
    platform
        .db()
        .write(move |tx| {
            repo::insert_classroom(tx, &classroom_for_db, Some("align 集成测试"), at)?;
            for student in &students {
                repo::insert_student(tx, student, at)?;
            }
            Ok(())
        })
        .expect("写班级/学生");
    classroom
}

#[test]
fn settings_and_roster_come_from_real_platform() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (platform, clock) = open_platform(dir.path());
    let settings = settings_from_platform(&platform).expect("配置快照");
    assert_eq!(settings, AlignSettings::default(), "平台默认值应与规范一致");
    settings.validate().expect("默认配置合法");

    let classroom = seed_roster(&platform, &clock);
    let roster = load_roster(&platform, classroom.id).expect("加载名单");
    assert_eq!(roster.len(), 2);
    assert_eq!(roster.students()[0].student_no, "20260001");
    assert!(roster.by_student_no("2026-0002").is_some(), "归一学号可查");
    platform.close().expect("关闭 platform");
}

#[test]
fn manual_confirmation_flips_gate_and_writes_audit_chain() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (platform, clock) = open_platform(dir.path());
    let classroom = seed_roster(&platform, &clock);
    let roster = load_roster(&platform, classroom.id).expect("加载名单");
    let settings = settings_from_platform(&platform).expect("配置快照");

    // 相机文件名 → 默认未命中；模板路径保证区域全部对齐。
    let submission = submission(300, classroom.id, 10, "IMG_7777", &["第 1 页", "第 2 页"]);
    let paper = paper(10, classroom.id, &["1", "2", "3"]);
    let regions = vec![
        (0usize, 1u32, strip(0.10, 0.20)),
        (1, 2, strip(0.10, 0.20)),
        (2, 2, strip(0.40, 0.20)),
    ];
    let template = template(&paper, paper.key_version, 2, &regions);
    let pages = page_refs(&submission);
    let recognitions = eg_align::MapRecognitionSource::new(vec![
        recognition_with_anchors(pages[0].page_id, &[("1 计算", 0.1, 0.05)]),
        recognition_with_anchors(
            pages[1].page_id,
            &[("2 计算", 0.1, 0.05), ("3 计算", 0.4, 0.05)],
        ),
    ])
    .expect("识别夹具");
    let ids = PlatformIds::new(platform.ids());
    let align_clock = FixedClock::new(clock.now());
    let report = align(AlignRequest {
        submission: &submission,
        paper: &paper,
        roster: &roster,
        template: Some(&template),
        pages: &pages,
        recognitions: &recognitions,
        hints: &eg_align::NoIdentityHints,
        settings: &settings,
        ids: &ids,
        clock: &align_clock,
        existing: &[],
    })
    .expect("对齐");
    assert!(!report.identity.status.is_confirmed());
    assert!(!report.auto_grade_ready, "身份未确认必须拦截");
    let before_hash = report.fingerprint.output_hash;

    // 对齐结果审计（action=align.alignment，after=输出哈希）。
    let alignment_entry = write_alignment_audit(&platform, &report).expect("对齐审计");
    assert_eq!(alignment_entry.action, "align.alignment");
    assert_eq!(alignment_entry.after_hash, Some(before_hash));

    // 教师人工确认名单内学生。
    let target = roster.students()[0].id;
    let request = ManualRequest {
        submission_id: submission.id,
        command: ManualCommand::ConfirmIdentity { student_id: target },
        reason: Some("相机文件名，教师人工确认".to_owned()),
    };
    let outcome = apply_manual(&report, &request, &roster, &ids, &align_clock).expect("人工确认");
    assert_eq!(outcome.report.student_id(), Some(target));
    assert!(!outcome.requires_realign);
    assert_eq!(outcome.report.manual_history.len(), 1);
    assert_ne!(outcome.report.fingerprint.output_hash, before_hash);
    assert!(
        outcome.report.auto_grade_ready,
        "身份确认后门禁应放行（区域 1.0、无缺页）：{:?}",
        outcome.report.unresolved
    );

    // 人工审计写入平台审计链。
    let entry = write_manual_audit(&platform, &request, &outcome).expect("审计写入");
    assert_eq!(entry.action, "align.manual_correction");
    assert_eq!(entry.actor, "local-teacher");
    let entries = verify_audit_chain(&platform).expect("审计链完整");
    assert_eq!(entries, 2, "对齐审计 + 人工修正审计各一条");
    let count = platform
        .db()
        .read(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM audit_log WHERE action = 'align.manual_correction'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(eg_platform::PlatformError::from)
        })
        .expect("计数");
    assert_eq!(count, 1);
    let alignment_count = platform
        .db()
        .read(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM audit_log WHERE action = 'align.alignment'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(eg_platform::PlatformError::from)
        })
        .expect("计数");
    assert_eq!(alignment_count, 1);

    // 人工画框：区域替换为 Manual，且标记需要重跑（答案需按新区域重建）。
    let question_id = paper.questions[2].id;
    let rebind = ManualRequest {
        submission_id: submission.id,
        command: ManualCommand::RebindRegion {
            question_id,
            page_no: 2,
            bbox: strip(0.55, 0.30),
        },
        reason: Some("题号区域偏移，人工画框".to_owned()),
    };
    let rebound =
        apply_manual(&outcome.report, &rebind, &roster, &ids, &align_clock).expect("画框");
    assert!(rebound.requires_realign);
    let summary = rebound
        .report
        .question_summaries
        .iter()
        .find(|summary| summary.question_id == question_id)
        .expect("题目摘要");
    assert_eq!(summary.path, RegionPath::Manual);
    assert_eq!(summary.confidence.get(), 1.0);
    assert_eq!(summary.pages, vec![2]);
    let segment_ids = manual_segment_ids(&rebound.report, question_id);
    assert_eq!(segment_ids.len(), 1);
    let segment = rebound
        .report
        .segments
        .iter()
        .find(|segment| segment.id == segment_ids[0])
        .expect("人工区域");
    assert_eq!(segment.method, eg_domain::SegmentMethod::Manual);
    assert_eq!(segment.bbox.y, 0.55);

    platform.close().expect("关闭 platform");
}

#[test]
fn manual_confirmation_rejects_student_outside_roster() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (platform, clock) = open_platform(dir.path());
    let classroom = seed_roster(&platform, &clock);
    let roster = load_roster(&platform, classroom.id).expect("加载名单");
    let settings = settings_from_platform(&platform).expect("配置快照");
    let submission = submission(301, classroom.id, 10, "IMG_8888", &["第 1 页"]);
    let paper = paper(10, classroom.id, &["1"]);
    let pages = page_refs(&submission);
    let recognitions = eg_align::MapRecognitionSource::new(vec![recognition(
        pages[0].page_id,
        vec![eg_align::testing::block("1 计算", 0.1, 0.05)],
    )])
    .expect("识别夹具");
    let ids = PlatformIds::new(platform.ids());
    let align_clock = FixedClock::new(clock.now());
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
        clock: &align_clock,
        existing: &[],
    })
    .expect("对齐");
    let ghost = eg_domain::StudentId::from_ulid(eg_domain::Ulid::from_u128(4242));
    let request = ManualRequest {
        submission_id: submission.id,
        command: ManualCommand::ConfirmIdentity { student_id: ghost },
        reason: None,
    };
    let error =
        apply_manual(&report, &request, &roster, &ids, &align_clock).expect_err("名单外学生");
    assert_eq!(error.code(), "invalid_manual");
    // 未写任何审计。
    assert_eq!(verify_audit_chain(&platform).expect("审计链"), 0);
    platform.close().expect("关闭 platform");
}
