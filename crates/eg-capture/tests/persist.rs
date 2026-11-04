#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    clippy::float_cmp,
    clippy::doc_markdown,
    clippy::manual_let_else,
    clippy::bool_assert_comparison,
    clippy::useless_vec,
    clippy::unreadable_literal,
    clippy::uninlined_format_args,
    clippy::needless_pass_by_value,
    clippy::type_complexity,
    unreachable_pub,
    reason = "真实 platform 落库测试：允许 unwrap/expect/索引与数值转换"
)]

//! 真实 `eg-platform` 落库测试（D1）：`Submission`/`Page`/`asset_link`/`audit_log` 单事务。
//!
//! 覆盖：
//! - 正常导入：提交/页面/链接/审计在同一事务写完，读回一致，审计链完整；
//! - 40 份同字节：批内去重后只建 1 份提交（与 `tests/platform_bridge.rs` 的资产侧结论对齐）；
//! - 被拒页：`Page` 行保留（质量结论 `Rejected`）、提交状态 `Failed{Capture}`；
//! - 部分成功：好文件落库、坏文件只进报告；
//! - 事务失败（第二份提交主键冲突）→ 之前写入的第一份提交整体回滚；
//! - 资产未先落盘 → 事务前拒绝，零行写入；
//! - 任务阶段：提交后推进 `Preprocess`；任务未 claim 时数据仍提交、显式报错；
//! - 参考答案页面：`key_page_asset` 链接落库，不建模成 `Page`。
//!
//! 前置：测试自行在 platform 里造 1 个班级 + 1 份试卷（FK 需要），不依赖其他模块。

mod support;

use std::sync::Arc;

use eg_capture::persist::{PersistRequest, persist};
use eg_capture::platform_bridge::{AssetStoreAdapter, CancelAdapter, ClockAdapter, IdAdapter};
use eg_capture::{
    CaptureError, FileInput, ImportKind, ImportOptions, ImportOutcome, ImportRequest,
    ImportSettings, Importer, MemoryEventSink, NoKnownSubmissions,
};
use eg_domain::{
    AnswerKey, AssetId, Classroom, ClassroomId, MilliScore, Paper, PaperId, QualityVerdict,
    Question, QuestionId, QuestionKind, Stage, Submission, SubmissionId, SubmissionStatus, TaskId,
    Ulid,
};
use eg_platform::{CancelToken, DataDirs, FixedClock, Platform, SharedClock, repo};

/// 打开真实 platform（固定时钟 → 审计时间/ULID 可预期）。
fn open_platform(root: &std::path::Path) -> (Platform, SharedClock) {
    let dirs = DataDirs::with_root(root).expect("测试数据目录");
    let clock: SharedClock =
        Arc::new(FixedClock::from_rfc3339("2026-11-02T00:00:00Z").expect("固定时钟"));
    let platform = Platform::open_with(dirs, clock.clone()).expect("打开 platform");
    (platform, clock)
}

/// 造 1 个班级 + 1 份带 1 道题的试卷（`submission.paper_id`/`classroom_id` 的外键前置）。
fn seed_classroom_and_paper(platform: &Platform, classroom: &Classroom) -> Paper {
    let paper_id = platform.ids().next_paper_id().expect("试卷 id");
    let question = Question {
        id: QuestionId::from_ulid(platform.ids().next_ulid().expect("题目 ulid")),
        paper_id,
        ordinal: 1,
        label: "1".to_owned(),
        kind: QuestionKind::NumericOnly,
        max_score: MilliScore::from_milli(1000).expect("1 分"),
        stem: None,
        key: AnswerKey::Missing,
        rubric: None,
        subject_hint: None,
        knowledge_tags: Vec::new(),
    };
    let paper = Paper {
        id: paper_id,
        classroom_id: classroom.id,
        title: "期中数学".to_owned(),
        subject: eg_domain::SubjectId::math(),
        total_score: MilliScore::from_milli(1000).expect("1 分"),
        questions: vec![question],
        key_version: 1,
        rubric_version: 1,
        created_at: platform.clock().now(),
    };
    paper.validate().expect("试卷合法");
    let at = platform.clock().now();
    let classroom = classroom.clone();
    let paper_for_db = paper.clone();
    platform
        .db()
        .write(move |tx| {
            repo::insert_classroom(tx, &classroom, Some("capture 落库测试"), at)?;
            repo::insert_paper(tx, &paper_for_db)?;
            Ok(())
        })
        .expect("写班级/试卷");
    paper
}

/// 用真实 platform 端口跑一次导入（资产真正落到 platform 资产库）。
fn run_import(
    platform: &Platform,
    clock: &SharedClock,
    classroom_id: ClassroomId,
    paper_id: PaperId,
    batch: Ulid,
    kind: ImportKind,
    files: Vec<FileInput>,
) -> ImportOutcome {
    let events = MemoryEventSink::new();
    let settings = ImportSettings::default();
    let assets = AssetStoreAdapter::new(platform.assets());
    let clock_adapter = ClockAdapter::new(clock.as_ref());
    let ids = IdAdapter::new(platform.ids());
    let cancel = CancelToken::new();
    let cancel_adapter = CancelAdapter::new(&cancel);
    let importer = Importer::new(
        &settings,
        &assets,
        &events,
        &clock_adapter,
        &ids,
        &cancel_adapter,
        &NoKnownSubmissions,
    );
    importer
        .run(ImportRequest {
            batch_id: batch,
            kind,
            classroom_id,
            paper_id,
            files,
            options: ImportOptions::default(),
        })
        .expect("导入成功")
}

/// 用给定提交替换导入结果里的提交列表（其余字段沿用），用于构造冲突/缺资产场景。
fn outcome_with(outcome: &ImportOutcome, submissions: Vec<Submission>) -> ImportOutcome {
    ImportOutcome {
        report: outcome.report.clone(),
        submissions,
        key_assets: Vec::new(),
    }
}

fn count_rows(platform: &Platform, table: &str) -> i64 {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    platform
        .db()
        .read(move |conn| {
            conn.query_row(&sql, [], |row| row.get::<_, i64>(0))
                .map_err(eg_platform::PlatformError::from)
        })
        .expect("计数")
}

fn count_where(platform: &Platform, sql: String) -> i64 {
    platform
        .db()
        .read(move |conn| {
            conn.query_row(&sql, [], |row| row.get::<_, i64>(0))
                .map_err(eg_platform::PlatformError::from)
        })
        .expect("条件计数")
}

fn read_submission(platform: &Platform, id: SubmissionId) -> Option<Submission> {
    platform
        .db()
        .read(move |conn| repo::get_submission(conn, id))
        .expect("读提交")
}

fn a4_hd_marked(seed: u32) -> Vec<u8> {
    support::png(&support::marked(&support::a4_page_hd(), seed))
}

#[test]
fn persist_writes_rows_links_and_audit_in_one_transaction() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (platform, clock) = open_platform(dir.path());
    let classroom = Classroom {
        id: platform.ids().next_classroom_id().expect("班级 id"),
        name: "高二（3）班".to_owned(),
        term: "2026 秋".to_owned(),
    };
    let paper = seed_classroom_and_paper(&platform, &classroom);
    let outcome = run_import(
        &platform,
        &clock,
        classroom.id,
        paper.id,
        Ulid::from_u128(101),
        ImportKind::StudentWork,
        vec![
            FileInput::from_bytes("学生甲.png", a4_hd_marked(1)),
            FileInput::from_bytes("学生乙.png", a4_hd_marked(2)),
        ],
    );
    assert_eq!(outcome.submissions.len(), 2);
    let expected_pages: u32 = outcome
        .submissions
        .iter()
        .map(|submission| submission.pages.len() as u32)
        .sum();
    let expected_links: u32 = outcome
        .submissions
        .iter()
        .flat_map(|submission| &submission.pages)
        .map(|page| 1 + page.derived.len() as u32)
        .sum();
    assert!(expected_pages >= 2, "两张良页都应有 Page 行");

    let summary = persist(
        &platform,
        PersistRequest {
            outcome: &outcome,
            task_id: None,
        },
    )
    .expect("落库成功");

    assert_eq!(summary.submissions, 2);
    assert_eq!(summary.pages, expected_pages);
    assert_eq!(summary.asset_links, expected_links);
    assert_eq!(summary.audit_entries, 1 + 2 + expected_pages);
    assert_eq!(summary.rejected_pages, 0);
    assert_eq!(summary.key_assets, 0);
    assert!(!summary.task_advanced);

    // 行数：提交/页面/链接/审计与摘要一致（同事务一起可见）。
    assert_eq!(count_rows(&platform, "submission"), 2);
    assert_eq!(count_rows(&platform, "page"), i64::from(expected_pages));
    assert_eq!(
        count_rows(&platform, "asset_link"),
        i64::from(expected_links)
    );
    assert_eq!(
        count_rows(&platform, "audit_log"),
        i64::from(summary.audit_entries)
    );
    assert_eq!(
        count_where(
            &platform,
            "SELECT COUNT(*) FROM audit_log WHERE action = 'import.batch'".into()
        ),
        1
    );

    // 读回：`repo::get_submission` / `repo::list_pages` 与内存对象一致。
    for submission in &outcome.submissions {
        let read = read_submission(&platform, submission.id).expect("提交可读回");
        assert_eq!(read.source_file_name, submission.source_file_name);
        assert_eq!(read.status, SubmissionStatus::Imported);
        assert_eq!(read.pages.len(), submission.pages.len());
        let pages = platform
            .db()
            .read(move |conn| repo::list_pages(conn, submission.id))
            .expect("读页面");
        assert_eq!(pages.len(), submission.pages.len());
        assert_eq!(pages[0].page_no, 1);
    }

    // 审计链在本事务内延伸后仍然完整。
    let verification = platform.audit().verify_chain().expect("校验审计链");
    assert!(verification.ok, "审计链断裂：{:?}", verification.broken);
    assert_eq!(
        verification.entries,
        u64::from(summary.audit_entries),
        "本测试库只有本次批次写入的审计"
    );

    platform.close().expect("关闭 platform");
}

#[test]
fn forty_identical_files_persist_as_one_submission() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (platform, clock) = open_platform(dir.path());
    let classroom = Classroom {
        id: platform.ids().next_classroom_id().expect("班级 id"),
        name: "高三（1）班".to_owned(),
        term: "2026 秋".to_owned(),
    };
    let paper = seed_classroom_and_paper(&platform, &classroom);
    let bytes = a4_hd_marked(7);
    let files = (0..40)
        .map(|index| FileInput::from_bytes(format!("重复-{index}.png"), bytes.clone()))
        .collect();
    let outcome = run_import(
        &platform,
        &clock,
        classroom.id,
        paper.id,
        Ulid::from_u128(102),
        ImportKind::StudentWork,
        files,
    );
    assert_eq!(outcome.report.files_total, 40);
    assert_eq!(outcome.submissions.len(), 1, "同字节批内去重只留 1 份提交");

    let summary = persist(
        &platform,
        PersistRequest {
            outcome: &outcome,
            task_id: None,
        },
    )
    .expect("落库成功");
    assert_eq!(summary.submissions, 1);
    assert_eq!(count_rows(&platform, "submission"), 1);
    assert_eq!(
        count_rows(&platform, "page"),
        i64::from(summary.pages),
        "页面数不随重复文件增加"
    );
    platform.close().expect("关闭 platform");
}

#[test]
fn rejected_page_is_persisted_with_verdict_and_failed_status() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (platform, clock) = open_platform(dir.path());
    let classroom = Classroom {
        id: platform.ids().next_classroom_id().expect("班级 id"),
        name: "高二（4）班".to_owned(),
        term: "2026 秋".to_owned(),
    };
    let paper = seed_classroom_and_paper(&platform, &classroom);
    let blurred = support::blurred(&support::a4_page_hd(), 15.0);
    let outcome = run_import(
        &platform,
        &clock,
        classroom.id,
        paper.id,
        Ulid::from_u128(103),
        ImportKind::StudentWork,
        vec![FileInput::from_bytes("模糊.png", support::png(&blurred))],
    );
    assert_eq!(outcome.report.pages_rejected, 1);
    let summary = persist(
        &platform,
        PersistRequest {
            outcome: &outcome,
            task_id: None,
        },
    )
    .expect("落库成功");
    assert_eq!(summary.rejected_pages, 1, "被拒页也要落库（供复核/补拍）");

    let submission = &outcome.submissions[0];
    let read = read_submission(&platform, submission.id).expect("提交可读回");
    assert!(
        matches!(
            read.status,
            SubmissionStatus::Failed {
                stage: Stage::Capture,
                ..
            }
        ),
        "存在被拒页的提交应为 Failed{{Capture}}，实际 {:?}",
        read.status
    );
    let page = &read.pages[0];
    assert!(
        matches!(&page.quality.verdict, QualityVerdict::Rejected { .. }),
        "被拒页必须保留质量结论，实际 {:?}",
        page.quality.verdict
    );
    // 拒收原因写进审计备注（教师可见，且不含文件名等身份信息）。
    let rejected_audits = count_where(
        &platform,
        "SELECT COUNT(*) FROM audit_log WHERE action = 'page.import' AND note LIKE '%拒收%'".into(),
    );
    assert_eq!(rejected_audits, 1);
    platform.close().expect("关闭 platform");
}

#[test]
fn partial_success_persists_good_files_and_keeps_failures_in_report() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (platform, clock) = open_platform(dir.path());
    let classroom = Classroom {
        id: platform.ids().next_classroom_id().expect("班级 id"),
        name: "高一（2）班".to_owned(),
        term: "2026 秋".to_owned(),
    };
    let paper = seed_classroom_and_paper(&platform, &classroom);
    let outcome = run_import(
        &platform,
        &clock,
        classroom.id,
        paper.id,
        Ulid::from_u128(104),
        ImportKind::StudentWork,
        vec![
            FileInput::from_bytes("好页.png", a4_hd_marked(11)),
            FileInput::from_bytes("坏页.png", b"not an image".to_vec()),
        ],
    );
    assert!(outcome.report.partial_success);
    assert_eq!(outcome.report.files_failed, 1);
    assert_eq!(outcome.submissions.len(), 1);
    let summary = persist(
        &platform,
        PersistRequest {
            outcome: &outcome,
            task_id: None,
        },
    )
    .expect("部分成功也要落库");
    assert_eq!(summary.submissions, 1, "失败文件只进报告，不建提交");
    assert_eq!(count_rows(&platform, "submission"), 1);
    platform.close().expect("关闭 platform");
}

#[test]
fn conflict_on_second_submission_rolls_back_the_first() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (platform, clock) = open_platform(dir.path());
    let classroom = Classroom {
        id: platform.ids().next_classroom_id().expect("班级 id"),
        name: "高二（5）班".to_owned(),
        term: "2026 秋".to_owned(),
    };
    let paper = seed_classroom_and_paper(&platform, &classroom);
    let outcome = run_import(
        &platform,
        &clock,
        classroom.id,
        paper.id,
        Ulid::from_u128(105),
        ImportKind::StudentWork,
        vec![
            FileInput::from_bytes("甲.png", a4_hd_marked(21)),
            FileInput::from_bytes("乙.png", a4_hd_marked(22)),
        ],
    );
    assert_eq!(outcome.submissions.len(), 2);
    let first = outcome.submissions[0].clone();
    let second = outcome.submissions[1].clone();
    let second_id = second.id;

    // 先只落第一份（成功）。
    let ok = persist(
        &platform,
        PersistRequest {
            outcome: &outcome_with(&outcome, vec![first.clone()]),
            task_id: None,
        },
    )
    .expect("第一份落库成功");
    assert_eq!(ok.submissions, 1);
    let audit_before = count_rows(&platform, "audit_log");

    // 再落「第二份 + 已存在的第一份」：第一份先写入，随后第二份主键冲突 → 整体回滚。
    let conflict = outcome_with(&outcome, vec![second.clone(), first.clone()]);
    let error = persist(
        &platform,
        PersistRequest {
            outcome: &conflict,
            task_id: None,
        },
    )
    .expect_err("重复主键必须失败");
    assert!(
        matches!(error, CaptureError::PersistFailed(_)),
        "应为落库错误，实际 {error:?}"
    );

    assert!(
        read_submission(&platform, second_id).is_none(),
        "冲突提交之前写入的第二份必须已回滚"
    );
    let second_pages = platform
        .db()
        .read(move |conn| repo::list_pages(conn, second_id))
        .expect("读页面");
    assert!(second_pages.is_empty(), "回滚后不得残留页面行");
    let second_links = count_where(
        &platform,
        format!(
            "SELECT COUNT(*) FROM asset_link WHERE owner_type = 'page' AND owner_id IN (SELECT id FROM page WHERE submission_id = '{second_id}')"
        ),
    );
    assert_eq!(second_links, 0, "回滚后不得残留资产链接");
    assert_eq!(
        count_rows(&platform, "submission"),
        1,
        "库里仍只有第一份提交"
    );
    assert_eq!(
        count_rows(&platform, "audit_log"),
        audit_before,
        "失败事务不得追加审计"
    );
    platform.close().expect("关闭 platform");
}

#[test]
fn missing_asset_is_rejected_before_any_row_is_written() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (platform, clock) = open_platform(dir.path());
    let classroom = Classroom {
        id: platform.ids().next_classroom_id().expect("班级 id"),
        name: "高二（6）班".to_owned(),
        term: "2026 秋".to_owned(),
    };
    let paper = seed_classroom_and_paper(&platform, &classroom);
    let outcome = run_import(
        &platform,
        &clock,
        classroom.id,
        paper.id,
        Ulid::from_u128(106),
        ImportKind::StudentWork,
        vec![FileInput::from_bytes("甲.png", a4_hd_marked(31))],
    );
    let mut broken = outcome_with(&outcome, outcome.submissions.clone());
    broken.submissions[0].pages[0].asset = AssetId::of(b"ghost-asset");
    let error = persist(
        &platform,
        PersistRequest {
            outcome: &broken,
            task_id: None,
        },
    )
    .expect_err("缺资产必须拒绝");
    assert!(
        matches!(&error, CaptureError::PersistFailed(message) if message.contains("资产未先落盘")),
        "应为资产前置错误，实际 {error:?}"
    );
    assert_eq!(count_rows(&platform, "submission"), 0);
    assert_eq!(count_rows(&platform, "page"), 0);
    assert_eq!(count_rows(&platform, "asset_link"), 0);
    assert_eq!(count_rows(&platform, "audit_log"), 0);
    platform.close().expect("关闭 platform");
}

#[test]
fn task_stage_advances_to_preprocess_after_commit() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (platform, clock) = open_platform(dir.path());
    let classroom = Classroom {
        id: platform.ids().next_classroom_id().expect("班级 id"),
        name: "高二（7）班".to_owned(),
        term: "2026 秋".to_owned(),
    };
    let paper = seed_classroom_and_paper(&platform, &classroom);
    let outcome = run_import(
        &platform,
        &clock,
        classroom.id,
        paper.id,
        Ulid::from_u128(107),
        ImportKind::StudentWork,
        vec![FileInput::from_bytes("甲.png", a4_hd_marked(41))],
    );
    let task_id: TaskId = platform
        .tasks()
        .enqueue(eg_platform::NewTask::new(
            eg_platform::TaskKind::ImportBatch,
            serde_json::json!({"batch": 107}),
        ))
        .expect("入队")
        .task_id;
    let claimed = platform
        .tasks()
        .claim("capture-test")
        .expect("claim 不报错")
        .expect("有可 claim 的任务");
    assert_eq!(claimed.id, task_id);

    let summary = persist(
        &platform,
        PersistRequest {
            outcome: &outcome,
            task_id: Some(task_id),
        },
    )
    .expect("落库成功");
    assert!(summary.task_advanced);
    let record = platform
        .tasks()
        .get(task_id)
        .expect("读任务")
        .expect("任务存在");
    assert_eq!(record.stage, Some(Stage::Preprocess));
    // 审计与任务关联（批次/提交/页面审计都带 task_id）。
    let linked = count_where(
        &platform,
        format!("SELECT COUNT(*) FROM audit_log WHERE task_id = '{task_id}'"),
    );
    assert_eq!(
        linked,
        i64::from(summary.audit_entries) + 1,
        "本批次审计 + 入队审计（都关联 task_id）"
    );
    platform.close().expect("关闭 platform");
}

#[test]
fn committed_data_is_kept_when_task_stage_cannot_advance() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (platform, clock) = open_platform(dir.path());
    let classroom = Classroom {
        id: platform.ids().next_classroom_id().expect("班级 id"),
        name: "高二（8）班".to_owned(),
        term: "2026 秋".to_owned(),
    };
    let paper = seed_classroom_and_paper(&platform, &classroom);
    let outcome = run_import(
        &platform,
        &clock,
        classroom.id,
        paper.id,
        Ulid::from_u128(108),
        ImportKind::StudentWork,
        vec![FileInput::from_bytes("甲.png", a4_hd_marked(51))],
    );
    // 只入队、不 claim：任务仍是 pending，`advance_stage` 会拒绝（InvalidTransition）。
    let task_id = platform
        .tasks()
        .enqueue(eg_platform::NewTask::new(
            eg_platform::TaskKind::ImportBatch,
            serde_json::json!({"batch": 108}),
        ))
        .expect("入队")
        .task_id;
    let error = persist(
        &platform,
        PersistRequest {
            outcome: &outcome,
            task_id: Some(task_id),
        },
    )
    .expect_err("阶段推进失败必须显式报错");
    let message = match &error {
        CaptureError::PersistFailed(message) => message.clone(),
        other => panic!("应为落库阶段错误，实际 {other:?}"),
    };
    assert!(
        message.contains("数据已提交"),
        "错误必须说明数据已提交，实际：{message}"
    );
    // 数据确实在库里（不要因阶段失败而重试落库）。
    assert_eq!(
        count_rows(&platform, "submission"),
        i64::from(u32::try_from(outcome.submissions.len()).unwrap()),
    );
    assert!(count_rows(&platform, "audit_log") > 0);
    platform.close().expect("关闭 platform");
}

#[test]
fn key_reference_pages_persist_as_key_page_assets() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (platform, clock) = open_platform(dir.path());
    let classroom = Classroom {
        id: platform.ids().next_classroom_id().expect("班级 id"),
        name: "高二（9）班".to_owned(),
        term: "2026 秋".to_owned(),
    };
    let paper = seed_classroom_and_paper(&platform, &classroom);
    let outcome = run_import(
        &platform,
        &clock,
        classroom.id,
        paper.id,
        Ulid::from_u128(109),
        ImportKind::KeyReferencePages,
        vec![FileInput::from_bytes("参考答案.pdf", support::build_pdf(2))],
    );
    assert!(
        outcome.key_assets.len() >= 2,
        "两页 PDF 应有 2 份参考页资产"
    );
    let expected_key_links: u32 = outcome
        .key_assets
        .iter()
        .map(|key| 1 + key.derived.len() as u32)
        .sum();
    let summary = persist(
        &platform,
        PersistRequest {
            outcome: &outcome,
            task_id: None,
        },
    )
    .expect("参考答案落库成功");
    assert_eq!(summary.submissions, 0, "参考答案不建模成提交");
    assert_eq!(summary.pages, 0, "参考答案不建模成 Page");
    assert_eq!(summary.asset_links, expected_key_links);
    assert_eq!(
        count_rows(&platform, "asset_link"),
        i64::from(expected_key_links)
    );
    assert_eq!(
        count_where(
            &platform,
            "SELECT COUNT(*) FROM asset_link WHERE owner_type = 'key_page_asset'".into()
        ),
        i64::from(expected_key_links)
    );
    assert_eq!(
        count_where(
            &platform,
            "SELECT COUNT(*) FROM audit_log WHERE action = 'key_page_asset.preprocess'".into()
        ),
        i64::try_from(outcome.key_assets.len()).unwrap()
    );
    platform.close().expect("关闭 platform");
}

#[test]
fn grouped_multipage_submission_persists_two_pages_and_group_audit() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (platform, clock) = open_platform(dir.path());
    let classroom = Classroom {
        id: platform.ids().next_classroom_id().expect("班级 id"),
        name: "高二（10）班".to_owned(),
        term: "2026 秋".to_owned(),
    };
    let paper = seed_classroom_and_paper(&platform, &classroom);
    // 同一学生 2 页：文件名带页码后缀 → 归组为 1 个提交 2 页。
    let outcome = run_import(
        &platform,
        &clock,
        classroom.id,
        paper.id,
        Ulid::from_u128(120),
        ImportKind::StudentWork,
        vec![
            FileInput::from_bytes(
                "张三_p2.png",
                support::png(&support::marked(&support::a4_page_hd(), 51)),
            ),
            FileInput::from_bytes(
                "张三_p1.png",
                support::png(&support::marked(&support::a4_page_hd(), 52)),
            ),
        ],
    );
    assert_eq!(outcome.submissions.len(), 1);
    assert_eq!(outcome.submissions[0].pages.len(), 2);
    assert_eq!(outcome.report.groups.len(), 1);

    let summary = persist(
        &platform,
        PersistRequest {
            outcome: &outcome,
            task_id: None,
        },
    )
    .expect("多页组落库成功");
    assert_eq!(summary.submissions, 1);
    assert_eq!(summary.pages, 2);
    assert_eq!(count_rows(&platform, "submission"), 1);
    assert_eq!(count_rows(&platform, "page"), 2);

    let submission = &outcome.submissions[0];
    let read_pages = platform
        .db()
        .read(move |conn| repo::list_pages(conn, submission.id))
        .expect("读页面");
    assert_eq!(
        read_pages
            .iter()
            .map(|page| page.page_no)
            .collect::<Vec<_>>(),
        vec![1, 2],
        "落库页号必须连续"
    );

    // 审计记录组信息（页数/页序/文件指纹），且不含姓名/文件名原文。
    let notes: Vec<String> = platform
        .db()
        .read(|conn| {
            let mut statement = conn
                .prepare("SELECT note FROM audit_log WHERE action = 'submission.import'")
                .map_err(eg_platform::PlatformError::from)?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(eg_platform::PlatformError::from)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.map_err(eg_platform::PlatformError::from)?);
            }
            Ok(out)
        })
        .expect("读审计");
    assert_eq!(notes.len(), 1);
    let note = &notes[0];
    assert!(note.contains("多页归组"), "{note}");
    assert!(note.contains("页数=2"), "{note}");
    assert!(note.contains("页序=[1, 2]"), "{note}");
    assert!(note.contains("文件数=2"), "{note}");
    assert!(
        !note.contains("张三"),
        "审计不得包含学生姓名/文件名原文：{note}"
    );
    assert!(!note.contains(".png"), "审计不得包含文件名原文：{note}");

    let batch_note: String = platform
        .db()
        .read(|conn| {
            conn.query_row(
                "SELECT note FROM audit_log WHERE action = 'import.batch'",
                [],
                |row| row.get(0),
            )
            .map_err(eg_platform::PlatformError::from)
        })
        .expect("读批次审计");
    assert!(batch_note.contains("组=1"), "{batch_note}");
    platform.close().expect("关闭 platform");
}
