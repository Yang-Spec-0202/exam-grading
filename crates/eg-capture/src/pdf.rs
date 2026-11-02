//! PDF 逐页栅格化（`SPEC-capture.md` §3.2：PDF 按页渲染，页序不可重排；加密/损坏文件级失败）。
//!
//! 使用纯 Rust 的 `hayro`（0.8.0，见 `docs/dev/research/pdf.md`）：
//! - 打开失败（加密/损坏）→ [`CaptureError::PdfOpen`]，不产半页；
//! - 单页渲染失败 → 该页失败，其余页继续（部分成功）；
//! - **hayro 输出的位图是透明背景**：这里统一合成白底，否则送 OCR 严重降质；
//! - 渲染后长边超过 4000 像素时降采样并记 [`PageNote::Downsampled`]（规范 §4）。

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::hayro_syntax::page::Page;
use hayro::{PixmapSettings, RenderCache, RenderSettings, render};
use image::RgbImage;

use crate::error::{CaptureError, PageNote};
use crate::imageops;
use crate::settings::ImportSettings;

/// 单页渲染上限（长边像素；超过则降采样，规范 §4「超大图」）。
pub const MAX_RENDER_LONG_EDGE: u32 = 4_000;

/// 渲染结果。
#[derive(Debug, Clone)]
pub struct RenderedPage {
    /// 页号（1 起，等于 PDF 页序）。
    pub page_no: u32,
    /// 白底合成的 8 位 RGB 位图。
    pub image: RgbImage,
    /// 是否因超限被降采样。
    pub downsampled: bool,
}

/// 读取 PDF 页数（不渲染）。
///
/// # Errors
///
/// 无法打开（加密/损坏）返回 [`CaptureError::PdfOpen`]。
pub fn page_count(bytes: &[u8]) -> Result<usize, CaptureError> {
    let pdf = Pdf::new(bytes.to_vec()).map_err(|e| CaptureError::PdfOpen(format!("{e:?}")))?;
    Ok(pdf.pages().len())
}

/// 逐页渲染（页序 = PDF 页序）。
///
/// 返回 `Ok(pages)`：每页是 `Ok(RenderedPage)` 或 `Err(错误)`（单页失败不拖垮整份文件）。
///
/// # Errors
///
/// - [`CaptureError::PdfPageLimit`]：页数超过 `import.max_pdf_pages`
/// - [`CaptureError::PdfOpen`]：加密/损坏
pub fn render_pages(
    bytes: &[u8],
    settings: &ImportSettings,
) -> Result<Vec<Result<RenderedPage, CaptureError>>, CaptureError> {
    let pdf = Pdf::new(bytes.to_vec()).map_err(|e| CaptureError::PdfOpen(format!("{e:?}")))?;
    let pages = pdf.pages();
    if pages.len() > settings.max_pdf_pages as usize {
        return Err(CaptureError::PdfPageLimit {
            pages: pages.len(),
            limit: settings.max_pdf_pages,
        });
    }
    let requested_scale = settings.pdf_dpi as f32 / 72.0;
    let cache = RenderCache::new();
    let mut rendered = Vec::with_capacity(pages.len());
    for (index, page) in pages.iter().enumerate() {
        let page_no = index as u32 + 1;
        // 预限制渲染尺寸：长边不超过 MAX_RENDER_LONG_EDGE，避免 600 DPI A4 撑爆内存。
        let (page_width, page_height) = page.render_dimensions();
        let long_edge = page_width.max(page_height) as f32 * requested_scale;
        let scale = if long_edge > MAX_RENDER_LONG_EDGE as f32 {
            requested_scale * (MAX_RENDER_LONG_EDGE as f32 / long_edge)
        } else {
            requested_scale
        };
        let downsampled = scale < requested_scale;
        let result = render_one(page, &cache, scale, downsampled);
        rendered.push(match result {
            Ok(mut rendered_page) => {
                rendered_page.page_no = page_no;
                Ok(rendered_page)
            }
            Err(message) => Err(CaptureError::PdfRender {
                page: page_no,
                message,
            }),
        });
    }
    Ok(rendered)
}

fn render_one<'a>(
    page: &'a Page<'a>,
    cache: &RenderCache<'a>,
    scale: f32,
    downsampled: bool,
) -> Result<RenderedPage, String> {
    let mut settings = PixmapSettings::default();
    settings.x_scale = scale;
    settings.y_scale = scale;
    // hayro 可能对畸形 PDF panic；用 catch_unwind 转成单页错误，避免拖垮整个批次。
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        render(
            page,
            cache,
            &InterpreterSettings::default(),
            &RenderSettings::default(),
            &settings,
        )
    }))
    .map_err(|payload| format!("渲染线程 panic：{}", panic_message(&payload)))?;
    let width = outcome.width();
    let height = outcome.height();
    let rgba = image::RgbaImage::from_raw(
        u32::from(width),
        u32::from(height),
        outcome.data_as_u8_slice().to_vec(),
    )
    .ok_or_else(|| "渲染结果缓冲区尺寸不符".to_owned())?;
    // 白底合成（hayro 背景是透明的，见 docs/dev/research/pdf.md §5.1）。
    let mut rgb = imageops::composite_on_white(&rgba);
    // 兜底：极端纵横比或缩放误差导致的超限再降一次。
    let mut downsampled = downsampled;
    if rgb.width().max(rgb.height()) > MAX_RENDER_LONG_EDGE {
        rgb = imageops::resize_long_edge(&rgb, MAX_RENDER_LONG_EDGE);
        downsampled = true;
    }
    let image = rgb;
    if image.width() == 0 || image.height() == 0 {
        return Err("渲染结果为空".to_owned());
    }
    Ok(RenderedPage {
        page_no: 0,
        image,
        downsampled,
    })
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "未知 panic".to_owned()
    }
}

/// 渲染页提示码列表（降采样时给出）。
pub fn render_notes(page: &RenderedPage) -> Vec<PageNote> {
    if page.downsampled {
        vec![PageNote::Downsampled]
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// 构造最小合法 PDF（N 页，每页 A4），供夹具使用。
    pub(crate) fn build_pdf(page_count: usize) -> Vec<u8> {
        let mut objects: Vec<Vec<u8>> = Vec::new();
        objects.push(b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());
        let kids: Vec<String> = (0..page_count)
            .map(|i| format!("{} 0 R", 3 + i * 3))
            .collect();
        objects.push(
            format!(
                "<< /Type /Pages /Kids [{}] /Count {page_count} >>",
                kids.join(" ")
            )
            .into_bytes(),
        );
        for index in 0..page_count {
            let page_object = 3 + index * 3;
            let content_object = page_object + 1;
            let font_object = page_object + 2;
            objects.push(
                format!(
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] \
                     /Resources << /Font << /F1 {font_object} 0 R >> >> /Contents {content_object} 0 R >>"
                )
                .into_bytes(),
            );
            let content = format!(
                "0.1 0.1 0.1 rg 60 700 475 60 re f\nBT /F1 24 Tf 70 720 Td (Page {}) Tj ET\n",
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
        let mut out: Vec<u8> = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
        let mut offsets = Vec::with_capacity(objects.len());
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

    #[test]
    fn counts_and_renders_pages_in_order() {
        let bytes = build_pdf(3);
        assert_eq!(page_count(&bytes).unwrap(), 3);
        let settings = ImportSettings::default();
        let pages = render_pages(&bytes, &settings).unwrap();
        assert_eq!(pages.len(), 3);
        for (index, page) in pages.iter().enumerate() {
            let page = page.as_ref().unwrap();
            assert_eq!(page.page_no, index as u32 + 1);
            // A4@300dpi ≈ 2480×3507（允许少量取整差异）。
            assert!(page.image.width() > 2300, "width={}", page.image.width());
            assert!(page.image.height() > 3300, "height={}", page.image.height());
            // 白底合成断言：四角必须纯白（透明背景已合成）。
            for (x, y) in [
                (0, 0),
                (page.image.width() - 1, 0),
                (0, page.image.height() - 1),
                (page.image.width() - 1, page.image.height() - 1),
            ] {
                let pixel = page.image.get_pixel(x, y).0;
                assert!(
                    pixel.iter().all(|channel| *channel >= 250),
                    "角落应为白底，实际 {pixel:?}"
                );
            }
        }
    }

    #[test]
    fn page_limit_is_enforced() {
        let bytes = build_pdf(3);
        let mut settings = ImportSettings::default();
        settings.max_pdf_pages = 2;
        let error = render_pages(&bytes, &settings).unwrap_err();
        assert_eq!(error.code(), "pdf_page_limit");
    }

    #[test]
    fn damaged_pdf_fails_at_file_level() {
        let error = page_count(b"not a pdf at all").unwrap_err();
        assert_eq!(error.code(), "pdf_open_failed");
        let settings = ImportSettings::default();
        let error = render_pages(b"%PDF-1.7 garbage", &settings).unwrap_err();
        assert_eq!(error.code(), "pdf_open_failed");
    }
}
