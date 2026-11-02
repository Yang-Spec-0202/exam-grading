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

//! 真实 `eg-platform` 接入测试：`AssetStoreAdapter`/`ClockAdapter`/`IdAdapter`/`CancelAdapter`。
//!
//! 目标：证明 capture 的端口契约与 `platform` 实现无阻抗不匹配——
//! 内容寻址去重（40 次同字节 = 1 份文件）、只读可读回、注入时钟/单例 ULID 可用。
//! **不写数据库**：建档落库由组合根在 `platform::db.write` 事务里完成（见
//! `docs/dev/research/capture.md` §6 TODO），本测试只覆盖资产与端口。

mod support;

use std::sync::Arc;

use eg_capture::platform_bridge::{
    AssetStoreAdapter, CancelAdapter, ClockAdapter, IdAdapter, PlatformBridgeState, STATE,
};
use eg_capture::{
    FileInput, ImportKind, ImportOptions, ImportRequest, ImportSettings, Importer, MemoryEventSink,
    NoKnownSubmissions,
};
use eg_domain::{ClassroomId, PaperId, Ulid};
use eg_platform::{CancelToken, DataDirs, FixedClock, Platform, SharedClock};

fn open_platform(root: &std::path::Path) -> (Platform, SharedClock) {
    let dirs = DataDirs::with_root(root).expect("测试数据目录");
    let clock: SharedClock =
        Arc::new(FixedClock::from_rfc3339("2026-11-02T00:00:00Z").expect("固定时钟"));
    let platform = Platform::open_with(dirs, clock.clone()).expect("打开 platform");
    (platform, clock)
}

fn count_asset_files(root: &std::path::Path) -> usize {
    fn walk(path: &std::path::Path, count: &mut usize) {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, count);
            } else if !path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with('.'))
            {
                *count += 1;
            }
        }
    }
    let mut count = 0;
    walk(&root.join("assets"), &mut count);
    count
}

#[test]
fn bridge_state_is_connected() {
    assert_eq!(STATE, PlatformBridgeState::Connected);
}

#[test]
fn forty_identical_files_yield_one_asset_file_via_real_platform() {
    // 第二次导入用独立目录，比对资产文件数量。
    let single_dir = tempfile::tempdir().expect("临时目录");
    let many_dir = tempfile::tempdir().expect("临时目录");
    let page = support::a4_page_hd();
    let bytes = support::png(&page);

    let run = |dir: &std::path::Path, copies: usize| -> (usize, usize, usize) {
        let (platform, clock) = open_platform(dir);
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
        let files = (0..copies)
            .map(|index| FileInput::from_bytes(format!("重复-{index}.png"), bytes.clone()))
            .collect();
        let outcome = importer
            .run(ImportRequest {
                batch_id: Ulid::from_u128(42),
                kind: ImportKind::StudentWork,
                classroom_id: ClassroomId::from_ulid(Ulid::from_u128(7)),
                paper_id: PaperId::from_ulid(Ulid::from_u128(8)),
                files,
                options: ImportOptions::default(),
            })
            .expect("导入成功");
        let asset_files = count_asset_files(dir);
        let submissions = outcome.submissions.len();
        let assets_count = platform.assets().usage().expect("资产统计").entries as usize;
        // 读回一个页面资产，确认平台侧字节可读且与内容寻址一致。
        let first_page_asset = outcome.submissions[0].pages[0].asset;
        let stored = platform
            .assets()
            .get(first_page_asset)
            .expect("平台可读回页资产");
        assert!(!stored.is_empty());
        assert_eq!(eg_domain::AssetId::of(&stored), first_page_asset);
        platform.close().expect("关闭 platform");
        (asset_files, submissions, assets_count)
    };

    let (single_files, single_submissions, single_entries) = run(single_dir.path(), 1);
    let (many_files, many_submissions, many_entries) = run(many_dir.path(), 40);
    assert_eq!(single_submissions, 1);
    assert_eq!(many_submissions, 1, "40 份同字节只建 1 份提交");
    assert_eq!(
        single_files, many_files,
        "40 次同字节写入必须只留 1 份资产文件（去重）"
    );
    assert_eq!(single_entries, many_entries, "资产表行数不随重复导入增加");
    assert!(many_files >= 2, "至少原图 + 派生图");
}

#[test]
fn multipage_pdf_assets_land_in_platform_store() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (platform, clock) = open_platform(dir.path());
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
    let outcome = importer
        .run(ImportRequest {
            batch_id: Ulid::from_u128(43),
            kind: ImportKind::StudentWork,
            classroom_id: ClassroomId::from_ulid(Ulid::from_u128(7)),
            paper_id: PaperId::from_ulid(Ulid::from_u128(8)),
            files: vec![FileInput::from_bytes("三页.pdf", support::build_pdf(3))],
            options: ImportOptions::default(),
        })
        .expect("导入成功");
    let submission = &outcome.submissions[0];
    assert_eq!(submission.pages.len(), 3);
    for page in &submission.pages {
        let stored = platform.assets().get(page.asset).expect("页资产在平台库");
        assert_eq!(eg_domain::AssetId::of(&stored), page.asset);
    }
    // 派生资产也都真实存在于平台资产库（只读、内容寻址）。
    for page in &submission.pages {
        for derived in &page.derived {
            assert!(
                platform.assets().exists(*derived),
                "派生资产缺失：{derived}"
            );
        }
    }
    platform.close().expect("关闭 platform");
}
