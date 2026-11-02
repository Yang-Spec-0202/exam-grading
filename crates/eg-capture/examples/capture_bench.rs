//! 采集性能基准（`SPEC-capture.md` §8 C6）：1200 万像素单页预处理与 300 DPI A4 PDF 每页拆页。
//!
//! 运行：`cargo run -p eg-capture --release --example capture_bench`
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "基准示例：断言/夹具失败即应 panic，长度转换在本机范围内"
)]

use std::time::Instant;

use eg_capture::platform_bridge::PlatformBridgeState;
use eg_capture::{
    CancelCheck, ClockPort, FileInput, IdPort, ImportKind, ImportOptions, ImportRequest,
    ImportSettings, Importer, MemoryAssetSink, MemoryEventSink, NoKnownSubmissions,
};
use eg_domain::{ClassroomId, PaperId, Timestamp, Ulid};
use image::{Rgb, RgbImage};

struct FixedClock;
impl ClockPort for FixedClock {
    fn now(&self) -> Timestamp {
        eg_domain::time::parse_rfc3339("2026-11-02T00:00:00Z").expect("固定时间")
    }
}

#[derive(Default)]
struct SeqIds(std::sync::atomic::AtomicU64);
impl IdPort for SeqIds {
    fn next_ulid(&self) -> Result<Ulid, eg_capture::CaptureError> {
        Ok(Ulid::from_u128(
            u128::from(self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed)) + 1,
        ))
    }
}

#[derive(Default)]
struct NoCancel;
impl CancelCheck for NoCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
}

fn text_page(width: u32, height: u32) -> RgbImage {
    let mut image = RgbImage::from_pixel(width, height, Rgb([246, 246, 246]));
    let mut y = height / 20;
    while y + 12 < height * 19 / 20 {
        for x in width / 12..width * 11 / 12 {
            for dy in 0..12 {
                image.put_pixel(x, y + dy, Rgb([20, 20, 20]));
            }
        }
        y += 40;
    }
    image
}

fn jpeg_bytes(image: &RgbImage) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 92);
    encoder.encode_image(image).expect("JPEG 编码");
    out
}

fn pdf_bytes(pages: usize) -> Vec<u8> {
    let mut objects: Vec<Vec<u8>> = Vec::new();
    objects.push(b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());
    let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", 3 + i * 3)).collect();
    objects.push(
        format!(
            "<< /Type /Pages /Kids [{}] /Count {pages} >>",
            kids.join(" ")
        )
        .into_bytes(),
    );
    for index in 0..pages {
        let page_object = 3 + index * 3;
        objects.push(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 {} 0 R >> >> /Contents {} 0 R >>",
                page_object + 2,
                page_object + 1
            )
            .into_bytes(),
        );
        let content = format!(
            "0.1 0.1 0.1 rg 60 700 475 40 re f\nBT /F1 20 Tf 70 720 Td (Page {}) Tj ET\n",
            index + 1
        );
        objects.push(
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            )
            .into_bytes(),
        );
        objects.push(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec());
    }
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        out.extend_from_slice(object);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

fn percentile(values: &[f64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let index = ((p / 100.0) * (sorted.len() as f64 - 1.0)).round() as usize;
    sorted[index.min(sorted.len() - 1)]
}

fn main() {
    println!("bridge_state={:?}", PlatformBridgeState::Connected);
    let settings = ImportSettings::default();
    let sink = MemoryAssetSink::new();
    let events = MemoryEventSink::new();
    let clock = FixedClock;
    let ids = SeqIds::default();
    let cancel = NoCancel;
    let importer = Importer::new(
        &settings,
        &sink,
        &events,
        &clock,
        &ids,
        &cancel,
        &NoKnownSubmissions,
    );

    // 1) 1200 万像素 JPEG 单页预处理（4000×3000）。
    let page12mp = text_page(4000, 3000);
    let jpeg12mp = jpeg_bytes(&page12mp);
    let mut timings = Vec::new();
    for _ in 0..5 {
        let start = Instant::now();
        let outcome = importer
            .run(ImportRequest {
                batch_id: Ulid::from_u128(1),
                kind: ImportKind::StudentWork,
                classroom_id: ClassroomId::from_ulid(Ulid::from_u128(2)),
                paper_id: PaperId::from_ulid(Ulid::from_u128(3)),
                files: vec![FileInput::from_bytes("12mp.jpg", jpeg12mp.clone())],
                options: ImportOptions::default(),
            })
            .expect("导入");
        timings.push(start.elapsed().as_secs_f64());
        assert_eq!(outcome.report.files_ok, 1);
    }
    println!(
        "12MP JPEG 单页预处理: P50={:.3}s P95={:.3}s (n=5, 含资产写入内存与派生图)",
        percentile(&timings, 50.0),
        percentile(&timings, 95.0)
    );

    // 2) 300 DPI A4 PDF 每页拆页。
    let pdf = pdf_bytes(3);
    let start = Instant::now();
    let outcome = importer
        .run(ImportRequest {
            batch_id: Ulid::from_u128(4),
            kind: ImportKind::StudentWork,
            classroom_id: ClassroomId::from_ulid(Ulid::from_u128(5)),
            paper_id: PaperId::from_ulid(Ulid::from_u128(6)),
            files: vec![FileInput::from_bytes("a4.pdf", pdf)],
            options: ImportOptions::default(),
        })
        .expect("导入");
    let total = start.elapsed().as_secs_f64();
    let pages = outcome.report.pages_total.max(1);
    println!(
        "300 DPI A4 PDF: 总 {total:.3}s / {pages} 页 = {:.3}s/页",
        total / f64::from(pages)
    );
}
