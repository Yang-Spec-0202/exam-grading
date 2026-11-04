#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::as_conversions,
    clippy::float_cmp,
    clippy::unwrap_in_result,
    clippy::assert_is_empty,
    clippy::manual_assert_eq,
    clippy::manual_range_contains,
    clippy::doc_markdown,
    clippy::bool_comparison,
    clippy::map_unwrap_or,
    clippy::cast_lossless,
    clippy::useless_vec,
    clippy::uninlined_format_args,
    unreachable_pub,
    dead_code,
    reason = "集成测试夹具与断言：允许 unwrap/expect/索引与数值转换"
)]

//! 多页归组集成测试（任务 T52）：
//! - 仓库夹具 `fixtures/paper-20q`：3 学生 × 2 页 → 3 提交 × 2 页（页序按后缀）；
//! - 逐字节重复仍去重；缺页 → 提交 Failed + 显式 warning；
//! - 同 stem 不同目录不合并；40 文件批次归 20 组；
//! - 单文件仍是单页提交；
//! - 与 `eg-align` 的页码后缀/归一化语义逐项一致（dev 依赖仅测试用）。

mod support;

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use eg_capture::{
    AssetSink, CancelCheck, ClockPort, DuplicateCategory, FileInput, GroupWarningCode, ImportKind,
    ImportOptions, ImportOutcome, ImportRequest, ImportSettings, Importer, KnownSubmissions,
    MemoryAssetSink, MemoryEventSink, NoKnownSubmissions, SeqIdSource, SubmissionFingerprint,
};
use eg_domain::{ClassroomId, PaperId, SubmissionStatus, Timestamp, Ulid};

struct FixedClock(Timestamp);

impl ClockPort for FixedClock {
    fn now(&self) -> Timestamp {
        self.0
    }
}

#[derive(Default)]
struct TestCancel(AtomicBool);

impl CancelCheck for TestCancel {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

#[derive(Default)]
struct TestKnown(Mutex<Vec<SubmissionFingerprint>>);

impl KnownSubmissions for TestKnown {
    fn page_sets(&self) -> Vec<SubmissionFingerprint> {
        self.0.lock().map(|guard| guard.clone()).unwrap_or_default()
    }
}

struct Fixture {
    settings: ImportSettings,
    sink: MemoryAssetSink,
    events: MemoryEventSink,
    clock: FixedClock,
    ids: SeqIdSource,
    cancel: TestCancel,
    known: TestKnown,
}

impl Fixture {
    fn new() -> Self {
        Self {
            settings: ImportSettings::default(),
            sink: MemoryAssetSink::new(),
            events: MemoryEventSink::new(),
            clock: FixedClock(
                eg_domain::time::parse_rfc3339("2026-11-04T00:00:00Z").expect("固定时钟"),
            ),
            ids: SeqIdSource::with_start(0),
            cancel: TestCancel::default(),
            known: TestKnown::default(),
        }
    }

    fn batch(&self, files: Vec<FileInput>) -> ImportOutcome {
        let importer = Importer::new(
            &self.settings,
            &self.sink,
            &self.events,
            &self.clock,
            &self.ids,
            &self.cancel,
            &self.known,
        );
        importer
            .run(ImportRequest {
                batch_id: Ulid::from_u128(52_001),
                kind: ImportKind::StudentWork,
                classroom_id: ClassroomId::from_ulid(Ulid::from_u128(52_100)),
                paper_id: PaperId::from_ulid(Ulid::from_u128(52_101)),
                files,
                options: ImportOptions::default(),
            })
            .expect("批次级成功")
    }
}

fn fixtures_images() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/images")
}

fn fixture_file(name: &str) -> Vec<u8> {
    std::fs::read(fixtures_images().join(name)).expect("读取仓库夹具图片")
}

fn page_order(submission: &eg_domain::Submission) -> Vec<u32> {
    submission.pages.iter().map(|page| page.page_no).collect()
}

#[test]
fn fixtures_three_students_two_pages_become_three_submissions() {
    let fixture = Fixture::new();
    let files = vec![
        FileInput::from_bytes("张三_p2.png", fixture_file("张三_p2.png")),
        FileInput::from_bytes("张三_p1.png", fixture_file("张三_p1.png")),
        FileInput::from_bytes("李四_期中_p1.jpg", fixture_file("李四_期中_p1.jpg")),
        FileInput::from_bytes("李四_期中_p2.jpg", fixture_file("李四_期中_p2.jpg")),
        FileInput::from_bytes("SO2026-003_p1.png", fixture_file("SO2026-003_p1.png")),
        FileInput::from_bytes("SO2026-003_p2.png", fixture_file("SO2026-003_p2.png")),
    ];
    let outcome = fixture.batch(files);
    let report = &outcome.report;

    assert_eq!(report.files_total, 6);
    assert_eq!(report.files_ok, 6);
    assert_eq!(report.files_failed, 0);
    assert_eq!(report.groups.len(), 3, "3 名学生 → 3 个提交");
    assert_eq!(outcome.submissions.len(), 3);
    assert_eq!(report.pages_total, 6, "每名学生 2 页");
    assert!(!report.partial_success, "正常夹具批次不应部分成功");
    assert!(
        report.group_warnings.is_empty(),
        "{:?}",
        report.group_warnings
    );

    let mut stems: Vec<&str> = outcome
        .submissions
        .iter()
        .map(|submission| submission.source_stem.as_str())
        .collect();
    stems.sort_unstable();
    assert_eq!(stems, vec!["SO2026-003", "张三", "李四_期中"]);

    for submission in &outcome.submissions {
        assert_eq!(submission.pages.len(), 2);
        assert_eq!(page_order(submission), vec![1, 2], "页号必须重编号为 1..N");
        assert_eq!(submission.status, SubmissionStatus::Imported);
        // 页序按页码后缀：第 1 页必须是 p1 文件的资产（夹具 p1/p2 内容不同）。
        let expected_p1 = match submission.source_stem.as_str() {
            "张三" => fixture_file("张三_p1.png"),
            "李四_期中" => fixture_file("李四_期中_p1.jpg"),
            _ => fixture_file("SO2026-003_p1.png"),
        };
        assert_eq!(
            submission.pages[0].asset,
            eg_domain::AssetId::of(&expected_p1),
            "第 1 页必须是 p1 文件的资产"
        );
        let first_page_bytes = fixture
            .sink
            .get(submission.pages[0].asset)
            .expect("资产可读");
        assert_eq!(first_page_bytes, expected_p1);
    }

    for group in &report.groups {
        assert_eq!(group.page_count, 2);
        assert_eq!(group.file_count, 2);
        assert_eq!(group.page_order, vec![1, 2]);
        assert_eq!(group.suffix_pages.len(), 2);
        assert_eq!(group.missing_pages, Vec::<u32>::new());
        assert_eq!(group.file_fingerprints.len(), 2);
        assert!(group.warnings.is_empty(), "{:?}", group.warnings);
        let summary = report
            .submissions
            .iter()
            .find(|summary| summary.submission_id == group.submission_id)
            .expect("组必须有提交摘要");
        assert_eq!(summary.page_count, 2);
        assert_eq!(summary.file_count, 2);
        assert_eq!(summary.page_order, vec![1, 2]);
        assert_eq!(summary.group_key, Some(group.group_key));
    }

    // 事件：每文件一条 FileStarted/FileFinished，解到页的合计 = 6。
    let finished_pages: u32 = fixture
        .events
        .events()
        .iter()
        .filter_map(|event| match event {
            eg_capture::ImportEvent::FileFinished { page_count, .. } => Some(*page_count),
            _ => None,
        })
        .sum();
    assert_eq!(finished_pages, 6);
}

#[test]
fn duplicate_bytes_are_deduped_within_group() {
    let fixture = Fixture::new();
    let files = vec![
        FileInput::from_bytes("张三_p1.png", fixture_file("张三_p1.png")),
        FileInput::from_bytes("张三_p2.png", fixture_file("张三_p2.png")),
        FileInput::from_bytes("张三_p2-copy.png", fixture_file("张三_p2-copy.png")),
    ];
    let outcome = fixture.batch(files);
    let report = &outcome.report;
    assert_eq!(outcome.submissions.len(), 1, "逐字节重复不得新建提交");
    assert_eq!(outcome.submissions[0].pages.len(), 2);
    assert_eq!(
        report
            .duplicates
            .iter()
            .filter(|hit| hit.category == DuplicateCategory::SameBatch)
            .count(),
        1
    );
    assert!(
        !report
            .group_warnings
            .iter()
            .any(|warning| warning.code == GroupWarningCode::CrossGroupDuplicateBytes),
        "同一组的副本不应报跨组重复"
    );
}

#[test]
fn missing_middle_page_marks_submission_failed_with_warning() {
    let fixture = Fixture::new();
    let files = vec![
        FileInput::from_bytes(
            "张三_p1.png",
            support::png(&support::marked(&support::a4_page_hd(), 1)),
        ),
        FileInput::from_bytes(
            "张三_p3.png",
            support::png(&support::marked(&support::a4_page_hd(), 3)),
        ),
    ];
    let outcome = fixture.batch(files);
    let report = &outcome.report;
    assert_eq!(outcome.submissions.len(), 1);
    let submission = &outcome.submissions[0];
    assert_eq!(submission.pages.len(), 2);
    assert_eq!(page_order(submission), vec![1, 2], "缺页也重编号为连续页号");
    assert!(
        matches!(
            submission.status,
            SubmissionStatus::Failed {
                stage: eg_domain::Stage::Capture,
                ..
            }
        ),
        "缺页提交必须 Failed{{Capture}}，实际 {:?}",
        submission.status
    );
    let group = &report.groups[0];
    assert_eq!(group.missing_pages, vec![2]);
    let warning = report
        .group_warnings
        .iter()
        .find(|warning| warning.code == GroupWarningCode::MissingPageRange)
        .expect("缺页必须有显式 warning");
    assert!(
        warning.detail.contains("缺少第 [2] 页"),
        "{}",
        warning.detail
    );
    assert!(report.partial_success, "缺页应显式导致部分成功");
}

#[test]
fn single_page_of_multi_page_set_reports_missing_first_page() {
    let fixture = Fixture::new();
    let outcome = fixture.batch(vec![FileInput::from_bytes(
        "张三_p2.png",
        support::png(&support::marked(&support::a4_page_hd(), 2)),
    )]);
    assert_eq!(outcome.submissions.len(), 1);
    assert_eq!(outcome.submissions[0].pages.len(), 1);
    let group = &outcome.report.groups[0];
    assert_eq!(group.missing_pages, vec![1]);
    assert!(matches!(
        outcome.submissions[0].status,
        SubmissionStatus::Failed { .. }
    ));
}

#[test]
fn single_file_without_suffix_stays_single_page_without_warnings() {
    let fixture = Fixture::new();
    let outcome = fixture.batch(vec![FileInput::from_bytes(
        "王五.png",
        support::png(&support::marked(&support::a4_page_hd(), 9)),
    )]);
    assert_eq!(outcome.submissions.len(), 1);
    assert_eq!(outcome.submissions[0].pages.len(), 1);
    assert_eq!(outcome.submissions[0].source_stem, "王五");
    assert!(outcome.report.group_warnings.is_empty());
    assert_eq!(outcome.report.groups[0].file_count, 1);
    assert_eq!(outcome.report.groups[0].page_order, vec![1]);
}

#[test]
fn same_stem_in_different_directories_is_not_merged() {
    let directory = tempfile::tempdir().expect("临时目录");
    let dir_a = directory.path().join("甲班");
    let dir_b = directory.path().join("乙班");
    std::fs::create_dir_all(&dir_a).expect("建目录");
    std::fs::create_dir_all(&dir_b).expect("建目录");
    let path_a = dir_a.join("张三_p1.png");
    let path_b = dir_b.join("张三_p1.png");
    std::fs::write(
        &path_a,
        support::png(&support::marked(&support::a4_page_hd(), 11)),
    )
    .expect("写文件");
    std::fs::write(
        &path_b,
        support::png(&support::marked(&support::a4_page_hd(), 12)),
    )
    .expect("写文件");

    let fixture = Fixture::new();
    let outcome = fixture.batch(vec![
        FileInput::from_path(&path_a),
        FileInput::from_path(&path_b),
    ]);
    let report = &outcome.report;
    assert_eq!(outcome.submissions.len(), 2, "不同目录同名不得合并");
    assert_eq!(report.groups.len(), 2);
    assert_eq!(
        report
            .group_warnings
            .iter()
            .filter(|warning| warning.code == GroupWarningCode::SameStemDifferentDirectory)
            .count(),
        2
    );
    assert!(report.partial_success, "同名跨目录需显式提示");
}

#[test]
fn cross_group_duplicate_page_bytes_are_flagged() {
    let fixture = Fixture::new();
    let bytes = support::png(&support::marked(&support::a4_page_hd(), 21));
    let outcome = fixture.batch(vec![
        FileInput::from_bytes("张三_p1.png", bytes.clone()),
        FileInput::from_bytes("李四_p1.png", bytes),
    ]);
    let report = &outcome.report;
    // 同批同字节只处理一次：只有张三成组（李四没有可用文件）。
    assert_eq!(outcome.submissions.len(), 1);
    assert!(
        report
            .group_warnings
            .iter()
            .any(|warning| warning.code == GroupWarningCode::CrossGroupDuplicateBytes),
        "不同组的同字节带页码文件必须显式 warning"
    );
}

#[test]
fn forty_files_group_into_twenty_submissions_of_two_pages() {
    let fixture = Fixture::new();
    let mut files = Vec::with_capacity(40);
    for student in 0..20u32 {
        for page in 1..=2u32 {
            let seed = student * 2 + page;
            files.push(FileInput::from_bytes(
                format!("学生{student:02}_p{page}.png"),
                support::png(&support::marked(&support::text_page(900, 1200), seed)),
            ));
        }
    }
    let outcome = fixture.batch(files);
    let report = &outcome.report;
    assert_eq!(report.files_total, 40);
    assert_eq!(report.files_ok, 40);
    assert_eq!(report.files_failed, 0);
    assert_eq!(outcome.submissions.len(), 20, "20 名学生 → 20 提交");
    assert_eq!(report.groups.len(), 20);
    assert_eq!(report.pages_total, 40);
    assert!(
        report.pages_rejected > 0,
        "小尺寸夹具页会被分辨率门控拒收（与归组无关）"
    );
    assert!(
        outcome
            .submissions
            .iter()
            .all(|submission| submission.pages.len() == 2 && page_order(submission) == vec![1, 2])
    );
    assert!(
        report
            .groups
            .iter()
            .all(|group| group.file_count == 2 && group.page_count == 2)
    );
}

#[test]
fn normalization_and_page_suffix_match_eg_align() {
    // 逐项比对：capture 的归组实现与 eg-align 的身份归一必须同语义（防漂移）。
    let cases = [
        "张三_p1",
        "张三p2",
        "张三_第3页",
        "张三_2",
        "张三",
        "2026-11",
        "20260001",
        "IMG_0032",
        "李四_期中 数学",
        "ＡＢＣ＿ｐ１",
    ];
    for stem in cases {
        assert_eq!(
            eg_capture::group::page_suffix(stem),
            eg_align::page_suffix(stem),
            "page_suffix 语义漂移：{stem}"
        );
        assert_eq!(
            eg_capture::group::normalize_key(stem),
            eg_align::normalize_key(stem),
            "normalize_key 语义漂移：{stem}"
        );
    }
}

#[test]
fn invariant_helpers_are_stable() {
    // stem_group_key 不含原文且对同一输入稳定。
    let key = eg_capture::group::stem_group_key("张三_p1", None);
    assert_eq!(key, eg_capture::group::stem_group_key("张三_p2", None));
    assert_ne!(
        key,
        eg_capture::group::stem_group_key("张三_p1", Some("dir"))
    );
    assert!(!format!("{key:?}").contains("张三"));
    let _ = NoKnownSubmissions;
}
