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
    unreachable_pub,
    dead_code,
    reason = "测试夹具与断言：允许 unwrap/expect/索引与数值转换"
)]

//! `eg-capture` 集成测试：导入/拆页/去重/部分成功/质量门控/白底合成/EXIF（T21–T22）。
//!
//! 全部夹具由程序构造（`tests/support`），不依赖真实试卷照片。
//! 端口用内存实现（`platform` 尚未接入，见 `src/platform_bridge.rs`）。

mod support;

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use eg_capture::{
    AssetSink, CancelCheck, ClockPort, DuplicateCategory, FileInput, ImportEvent, ImportKind,
    ImportOptions, ImportOutcome, ImportRequest, ImportSettings, Importer, KnownSubmissions,
    MemoryAssetSink, MemoryEventSink, NoKnownSubmissions, SeqIdSource, SourceFingerprint,
    SubmissionFingerprint,
};
use eg_domain::{ClassroomId, PaperId, Timestamp, Ulid};

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
                eg_domain::time::parse_rfc3339("2026-11-02T00:00:00Z").expect("固定时钟"),
            ),
            ids: SeqIdSource::with_start(0),
            cancel: TestCancel::default(),
            known: TestKnown::default(),
        }
    }

    fn batch(
        &self,
        kind: ImportKind,
        files: Vec<FileInput>,
        options: ImportOptions,
    ) -> ImportOutcome {
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
                batch_id: Ulid::from_u128(7001),
                kind,
                classroom_id: ClassroomId::from_ulid(Ulid::from_u128(900)),
                paper_id: PaperId::from_ulid(Ulid::from_u128(901)),
                files,
                options,
            })
            .expect("批次级成功")
    }

    fn count_events(&self, predicate: impl Fn(&ImportEvent) -> bool) -> usize {
        self.events
            .events()
            .iter()
            .filter(|event| predicate(event))
            .count()
    }
}

fn default_options() -> ImportOptions {
    ImportOptions::default()
}

#[test]
fn imports_good_jpeg_and_emits_full_event_lifecycle() {
    let fixture = Fixture::new();
    let outcome = fixture.batch(
        ImportKind::StudentWork,
        vec![FileInput::from_bytes(
            "张三 期中.jpg",
            support::jpeg(&support::a4_page_hd()),
        )],
        default_options(),
    );
    let report = &outcome.report;
    assert_eq!(report.files_total, 1);
    assert_eq!(report.files_ok, 1);
    assert_eq!(report.files_failed, 0);
    assert_eq!(report.pages_total, 1);
    assert_eq!(report.pages_rejected, 0);
    assert_eq!(outcome.submissions.len(), 1);
    let submission = &outcome.submissions[0];
    assert_eq!(submission.source_file_name, "张三 期中.jpg");
    assert_eq!(submission.source_stem, "张三 期中");
    assert_eq!(submission.pages.len(), 1);
    assert!(report.naming_reminder_shown);
    assert!(!report.partial_success);
    // 事件生命周期完整且不丢。
    assert_eq!(
        fixture.count_events(|e| matches!(e, ImportEvent::BatchStarted { .. })),
        1
    );
    assert_eq!(
        fixture.count_events(|e| matches!(e, ImportEvent::FileStarted { .. })),
        1
    );
    assert_eq!(
        fixture.count_events(|e| matches!(e, ImportEvent::PageDecoded { .. })),
        1
    );
    assert_eq!(
        fixture.count_events(|e| matches!(e, ImportEvent::PagePreprocessed { .. })),
        1
    );
    assert_eq!(
        fixture.count_events(|e| matches!(e, ImportEvent::FileFinished { .. })),
        1
    );
    assert_eq!(
        fixture.count_events(|e| matches!(e, ImportEvent::Progress { .. })),
        1
    );
    assert_eq!(
        fixture.count_events(|e| matches!(e, ImportEvent::BatchFinished { .. })),
        1
    );
    // 命名提示只在批次开始出现一次（不解析/不改写文件名）。
    let started = fixture
        .events
        .events()
        .into_iter()
        .filter_map(|event| match event {
            ImportEvent::BatchStarted {
                naming_reminder, ..
            } => Some(naming_reminder),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(started, vec![true]);
}

#[test]
fn corrupt_file_does_not_break_batch() {
    let fixture = Fixture::new();
    let corrupt = {
        let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        bytes.extend_from_slice(b"not really a jpeg body");
        bytes
    };
    let outcome = fixture.batch(
        ImportKind::StudentWork,
        vec![
            FileInput::from_bytes("损坏.jpg", corrupt),
            FileInput::from_bytes("正常.png", support::png(&support::a4_page_hd())),
        ],
        default_options(),
    );
    let report = &outcome.report;
    assert_eq!(report.files_failed, 1);
    assert_eq!(report.files_ok, 1);
    assert!(report.partial_success);
    assert_eq!(report.file_failures.len(), 1);
    assert_eq!(report.file_failures[0].code, "decode_failed");
    assert!(!report.file_failures[0].message.is_empty());
    assert_eq!(outcome.submissions.len(), 1);
    assert_eq!(
        fixture.count_events(|e| matches!(e, ImportEvent::FileFailed { .. })),
        1
    );
}

#[test]
fn same_bytes_40_times_is_one_asset_and_one_submission() {
    let fixture = Fixture::new();
    let bytes = support::png(&support::a4_page_hd());
    let files: Vec<FileInput> = (0..40)
        .map(|index| FileInput::from_bytes(format!("重复-{index}.png"), bytes.clone()))
        .collect();
    let outcome = fixture.batch(ImportKind::StudentWork, files, default_options());
    let report = &outcome.report;
    assert_eq!(report.files_ok, 40);
    assert_eq!(outcome.submissions.len(), 1, "同批重复只处理一次");
    assert_eq!(report.duplicates.len(), 39);
    assert!(
        report
            .duplicates
            .iter()
            .all(|hit| hit.category == DuplicateCategory::SameBatch)
    );
    assert_eq!(report.pages_total, 1);
    let asset_count = fixture.sink.len();
    assert!(asset_count >= 3 && asset_count <= 5, "资产数={asset_count}");
}

#[test]
fn multipage_pdf_keeps_page_order_and_white_background() {
    let fixture = Fixture::new();
    let outcome = fixture.batch(
        ImportKind::StudentWork,
        vec![FileInput::from_bytes(
            "整班-扫描.pdf",
            support::build_pdf(3),
        )],
        default_options(),
    );
    let report = &outcome.report;
    assert_eq!(report.pages_total, 3);
    assert_eq!(outcome.submissions.len(), 1);
    let submission = &outcome.submissions[0];
    let page_numbers: Vec<u32> = submission.pages.iter().map(|page| page.page_no).collect();
    assert_eq!(page_numbers, vec![1, 2, 3], "页序必须等于 PDF 页序");
    assert_eq!(report.pages_failed, 0);
    // 白底合成断言：渲染页四个角必须是白色（hayro 默认透明背景）。
    for page in &submission.pages {
        let bytes = fixture.sink.get(page.asset).expect("页资产存在");
        let rendered = eg_capture::imageops::decode_png_rgb(&bytes).expect("页资产是 PNG");
        let (width, height) = rendered.dimensions();
        for (x, y) in [
            (0, 0),
            (width - 1, 0),
            (0, height - 1),
            (width - 1, height - 1),
        ] {
            let pixel = rendered.get_pixel(x, y).0;
            assert!(
                pixel.iter().all(|channel| *channel >= 250),
                "PDF 页角落应为白底，实际 {pixel:?}"
            );
        }
    }
}

#[test]
fn exif_orientation_records_rotation_and_keeps_raw_dimensions() {
    let fixture = Fixture::new();
    let page = support::text_page(1400, 2100);
    let bytes = support::jpeg_with_orientation(&page, 6);
    let outcome = fixture.batch(
        ImportKind::StudentWork,
        vec![FileInput::from_bytes("orient6.jpg", bytes)],
        default_options(),
    );
    assert_eq!(outcome.report.files_ok, 1);
    let submission = &outcome.submissions[0];
    let page = &submission.pages[0];
    // Page.width/height 记录**未旋转**的原图像素尺寸（规范 §3.2）。
    assert_eq!((page.width, page.height), (1400, 2100));
    assert_eq!(page.rotation_applied.degrees(), 90);
    assert_eq!(page.exif_orientation.value(), 6);
    assert!(page.exif_orientation.is_mirrored() == false);
    // 派生图应包含按方向校正后的图。
    assert!(!page.derived.is_empty());
}

#[test]
fn rejected_page_marks_submission_failed_and_is_not_silently_dropped() {
    let fixture = Fixture::new();
    let blurred = support::blurred(&support::a4_page_hd(), 15.0);
    let outcome = fixture.batch(
        ImportKind::StudentWork,
        vec![FileInput::from_bytes("模糊.png", support::png(&blurred))],
        default_options(),
    );
    let report = &outcome.report;
    assert_eq!(report.pages_rejected, 1);
    assert_eq!(report.rejected_pages.len(), 1);
    assert!(report.rejected_pages[0].reasons.contains(&"blur"));
    assert_eq!(outcome.submissions.len(), 1, "被拒页仍建页面记录");
    let submission = &outcome.submissions[0];
    assert!(
        matches!(submission.status, eg_domain::SubmissionStatus::Failed { stage, .. } if stage == eg_domain::Stage::Capture),
        "被拒页必须把提交置为失败（采集阶段）"
    );
    assert!(report.partial_success);
    assert_eq!(
        fixture.count_events(|e| matches!(e, ImportEvent::PageRejected { .. })),
        1
    );
}

#[test]
fn second_identical_import_hits_suspected_duplicate_and_adds_no_assets() {
    let fixture = Fixture::new();
    let bytes = support::png(&support::a4_page_hd());
    let first = fixture.batch(
        ImportKind::StudentWork,
        vec![FileInput::from_bytes("张三.png", bytes.clone())],
        default_options(),
    );
    let asset_count_after_first = fixture.sink.len();
    // 模拟 platform 仓储：登记第一次导入的提交页面集合。
    if let Ok(mut known) = fixture.known.0.lock() {
        for submission in &first.submissions {
            known.push(SubmissionFingerprint {
                submission_id: submission.id,
                pages: submission.pages.iter().map(|page| page.asset).collect(),
            });
        }
    }
    let second = fixture.batch(
        ImportKind::StudentWork,
        vec![FileInput::from_bytes("张三(再导入).png", bytes)],
        default_options(),
    );
    assert_eq!(second.submissions.len(), 0, "疑似重复不自动建档");
    assert_eq!(second.report.pages_total, 0);
    assert_eq!(fixture.sink.len(), asset_count_after_first, "资产数不增加");
    assert!(
        second
            .report
            .duplicates
            .iter()
            .any(|hit| hit.category == DuplicateCategory::SuspectedSubmission)
    );
}

#[test]
fn file_names_are_preserved_verbatim() {
    let fixture = Fixture::new();
    let page = support::a4_page_hd();
    let cases = [
        ("张三 期中 数学.jpg", support::jpeg(&page)),
        ("李四（1）.png", support::png(&page)),
        ("王五.期中.数学.tiff", {
            let mut out = Vec::new();
            page.write_to(
                &mut std::io::Cursor::new(&mut out),
                image::ImageFormat::Tiff,
            )
            .expect("TIFF 编码");
            out
        }),
    ];
    let files: Vec<FileInput> = cases
        .iter()
        .map(|(name, bytes)| FileInput::from_bytes(*name, bytes.clone()))
        .collect();
    let outcome = fixture.batch(ImportKind::StudentWork, files, default_options());
    assert_eq!(outcome.submissions.len(), 3);
    for (submission, (name, _)) in outcome.submissions.iter().zip(cases.iter()) {
        assert_eq!(&submission.source_file_name, name, "文件名必须逐字保留");
        let expected_stem = name.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(name);
        assert_eq!(submission.source_stem, expected_stem, "去扩展名不得改写");
    }
}

#[test]
fn cancelled_batch_stops_dispatch_and_reports() {
    let fixture = Fixture::new();
    fixture.cancel.0.store(true, Ordering::Relaxed);
    let outcome = fixture.batch(
        ImportKind::StudentWork,
        vec![FileInput::from_bytes(
            "a.png",
            support::png(&support::a4_page_hd()),
        )],
        default_options(),
    );
    assert!(outcome.report.cancelled);
    assert_eq!(outcome.report.files_ok, 0);
    assert_eq!(outcome.submissions.len(), 0);
    assert_eq!(
        fixture.count_events(|e| matches!(
            e,
            ImportEvent::BatchFinished {
                cancelled: true,
                ..
            }
        )),
        1
    );
}

#[test]
fn key_reference_pages_never_become_pages() {
    let fixture = Fixture::new();
    let outcome = fixture.batch(
        ImportKind::KeyReferencePages,
        vec![FileInput::from_bytes("参考答案.pdf", support::build_pdf(2))],
        default_options(),
    );
    assert_eq!(
        outcome.submissions.len(),
        0,
        "参考答案不得建模成 Page/Submission"
    );
    assert_eq!(outcome.key_assets.len(), 2);
    assert_eq!(outcome.report.key_assets.len(), 2);
    assert!(
        outcome
            .report
            .key_assets
            .iter()
            .all(|asset| asset.kind == "page")
    );
}

#[test]
fn batch_file_limit_is_checked_before_start() {
    let mut fixture = Fixture::new();
    fixture.settings.batch_file_limit = 2;
    let importer = Importer::new(
        &fixture.settings,
        &fixture.sink,
        &fixture.events,
        &fixture.clock,
        &fixture.ids,
        &fixture.cancel,
        &NoKnownSubmissions,
    );
    let files: Vec<FileInput> = (0..3)
        .map(|index| {
            FileInput::from_bytes(format!("{index}.png"), support::png(&support::a4_page()))
        })
        .collect();
    let error = importer
        .run(ImportRequest {
            batch_id: Ulid::from_u128(1),
            kind: ImportKind::StudentWork,
            classroom_id: ClassroomId::from_ulid(Ulid::from_u128(2)),
            paper_id: PaperId::from_ulid(Ulid::from_u128(3)),
            files,
            options: default_options(),
        })
        .expect_err("超限应在开始前拒绝");
    assert_eq!(error.code(), "batch_too_large");
    assert!(fixture.events.events().is_empty(), "拒绝时不得发批次事件");
}

#[test]
fn golden_set_30_pages_meets_reject_recall_and_false_reject_bounds() {
    let fixture = Fixture::new();
    let base = support::text_page(1200, 1700);
    // 每页加唯一标记：内容不同 → 不被批内去重折叠，30 份都进流水线。
    let unique = |index: usize| support::marked(&base, index as u32);
    let mut files: Vec<FileInput> = Vec::new();
    let mut expected_rejected = vec![false; 30];
    for index in 0..10 {
        files.push(FileInput::from_bytes(
            format!("良-{index}.jpg"),
            support::jpeg(&unique(index)),
        ));
    }
    for index in 0..5 {
        files.push(FileInput::from_bytes(
            format!("模糊-{index}.jpg"),
            support::jpeg(&support::blurred(&unique(10 + index), 15.0)),
        ));
        expected_rejected[10 + index] = true;
    }
    for index in 0..5 {
        files.push(FileInput::from_bytes(
            format!("欠曝-{index}.jpg"),
            support::jpeg(&support::darkened(&unique(15 + index), 0.05)),
        ));
        expected_rejected[15 + index] = true;
    }
    for index in 0..5 {
        files.push(FileInput::from_bytes(
            format!("倾斜-{index}.jpg"),
            support::jpeg(&support::rotated(&unique(20 + index), 10.0)),
        ));
        expected_rejected[20 + index] = true;
    }
    for index in 0..5 {
        files.push(FileInput::from_bytes(
            format!("低分辨率-{index}.jpg"),
            support::jpeg(&support::marked(
                &support::text_page(800, 1100),
                30 + index as u32,
            )),
        ));
        expected_rejected[25 + index] = true;
    }
    let outcome = fixture.batch(ImportKind::StudentWork, files, default_options());
    assert_eq!(outcome.report.files_ok, 30);
    // 页面顺序 = 文件顺序（每份文件 1 页）。
    let mut missed = 0;
    let mut false_rejects = 0;
    for (index, submission) in outcome.submissions.iter().enumerate() {
        let page = &submission.pages[0];
        let rejected = matches!(
            page.quality.verdict,
            eg_domain::QualityVerdict::Rejected { .. }
        );
        if expected_rejected[index] && !rejected {
            missed += 1;
        }
        if !expected_rejected[index] && rejected {
            false_rejects += 1;
        }
    }
    assert!(missed as f64 / 30.0 <= 0.05, "漏放 {missed}/30");
    assert!(
        false_rejects as f64 / 30.0 <= 0.10,
        "误杀 {false_rejects}/30"
    );
    assert_eq!(outcome.report.rejected_pages.len(), 20);
    // 指标分组可用。
    let counts = outcome.report.reject_reason_counts();
    assert!(counts.iter().any(|(reason, _)| *reason == "blur"));
    assert!(counts.iter().any(|(reason, _)| *reason == "resolution"));
}

#[test]
fn events_do_not_leak_paths_or_names() {
    let fixture = Fixture::new();
    let secret_name = "张三_20260101_期中数学.png";
    fixture.batch(
        ImportKind::StudentWork,
        vec![FileInput::from_bytes(
            secret_name,
            support::png(&support::a4_page_hd()),
        )],
        default_options(),
    );
    let serialized = fixture
        .events
        .events()
        .iter()
        .map(|event| serde_json::to_string(event).expect("事件可序列化"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!serialized.contains("张三"), "事件不得包含学生姓名");
    assert!(
        !serialized.contains(secret_name),
        "事件不得包含文件路径/文件名原文"
    );
    assert!(!serialized.contains("/Users/"), "事件不得包含路径");
    // 指纹存在且为 12 位十六进制。
    let fingerprint = SourceFingerprint::of_path("/Users/teacher/张三.png");
    assert_eq!(fingerprint.path_hash.len(), 12);
    assert!(
        fingerprint
            .path_hash
            .chars()
            .all(|ch| ch.is_ascii_hexdigit())
    );
}

#[test]
fn settings_options_hash_is_stable_and_sensitive() {
    let base = ImportSettings::default();
    let mut changed = base.clone();
    assert_eq!(base.options_hash(), base.clone().options_hash());
    changed.pdf_dpi = 150;
    assert_ne!(base.options_hash(), changed.options_hash());
    let mut invalid = base.clone();
    invalid.pdf_dpi = 999;
    assert!(invalid.validate().is_err());
}
