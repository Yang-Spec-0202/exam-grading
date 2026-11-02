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
    clippy::doc_markdown,
    clippy::bool_comparison,
    clippy::map_unwrap_or,
    clippy::cast_lossless,
    clippy::useless_vec,
    unreachable_pub,
    dead_code,
    reason = "测试夹具与断言：允许 unwrap/expect/索引与数值转换"
)]

//! 测试夹具（程序构造，无真实试卷照片）：图像页、EXIF、模糊/曝光/倾斜/低分辨率、PDF。

use image::{Rgb, RgbImage};

/// 合成「文字页」：浅灰白底 + 深色横条（有行结构、清晰、A4 纵横比）。
pub fn text_page(width: u32, height: u32) -> RgbImage {
    let mut image = RgbImage::from_pixel(width, height, Rgb([246, 246, 246]));
    let margin_x = width / 10;
    let mut y = height / 10;
    while y + 10 < height * 9 / 10 {
        for x in margin_x..width - margin_x {
            for dy in 0..10 {
                image.put_pixel(x, y + dy, Rgb([18, 18, 18]));
            }
        }
        y += 46;
    }
    image
}

/// A4 纵横比的「良页」：1240×1754（150 DPI），长边 ≥1600 由测试自行放大。
pub fn a4_page() -> RgbImage {
    text_page(1240, 1754)
}

/// A4 纵横比、长边 1754 px 的高清良页（≥ import.quality_min_long_edge 1600）。
pub fn a4_page_hd() -> RgbImage {
    text_page(1400, 1980)
}

/// 在页面上加唯一标记（让同尺寸夹具的内容各不相同，避免批内去重折叠）。
pub fn marked(image: &RgbImage, seed: u32) -> RgbImage {
    let mut out = image.clone();
    let (width, height) = out.dimensions();
    let x = 20 + (seed.wrapping_mul(37) % (width - 80));
    let y = 20 + (seed.wrapping_mul(53) % (height - 80));
    for dx in 0..12 {
        for dy in 0..12 {
            out.put_pixel(x + dx, y + dy, Rgb([90, 90, 90]));
        }
    }
    out
}

/// 模糊化（半径越大越糊）。
pub fn blurred(image: &RgbImage, radius: f32) -> RgbImage {
    image::imageops::blur(image, radius)
}

/// 压暗（欠曝）。
pub fn darkened(image: &RgbImage, factor: f32) -> RgbImage {
    let mut out = image.clone();
    for pixel in out.pixels_mut() {
        for channel in 0..3 {
            pixel[channel] = (f32::from(pixel[channel]) * factor) as u8;
        }
    }
    out
}

/// 旋转整幅图（用于倾斜夹具）。
pub fn rotated(image: &RgbImage, degrees: f64) -> RgbImage {
    let (width, height) = image.dimensions();
    let radians = degrees.to_radians();
    let (sin, cos) = (radians.sin(), radians.cos());
    let cx = f64::from(width) / 2.0;
    let cy = f64::from(height) / 2.0;
    let mut out = RgbImage::new(width, height);
    for (x, y, pixel) in out.enumerate_pixels_mut() {
        let dx = f64::from(x) - cx;
        let dy = f64::from(y) - cy;
        let sx = (dx * cos - dy * sin + cx).round();
        let sy = (dx * sin + dy * cos + cy).round();
        if sx >= 0.0 && sy >= 0.0 && sx < f64::from(width) && sy < f64::from(height) {
            *pixel = *image.get_pixel(sx as u32, sy as u32);
        } else {
            *pixel = Rgb([255, 255, 255]);
        }
    }
    out
}

/// 编码 PNG。
pub fn png(image: &RgbImage) -> Vec<u8> {
    let mut out = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .expect("PNG 编码失败");
    out
}

/// 编码 JPEG（质量 95，避免编码噪声影响质量门控）。
pub fn jpeg(image: &RgbImage) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 95);
    encoder.encode_image(image).expect("JPEG 编码失败");
    out
}

/// 编码 JPEG 并注入最小 EXIF APP1（Orientation 1–8）。
pub fn jpeg_with_orientation(image: &RgbImage, orientation: u8) -> Vec<u8> {
    let base = jpeg(image);
    let tiff: Vec<u8> = vec![
        b'I',
        b'I',
        0x2A,
        0x00,
        0x08,
        0x00,
        0x00,
        0x00, // 头
        0x01,
        0x00, // IFD0 条目数
        0x12,
        0x01,
        0x03,
        0x00,
        0x01,
        0x00,
        0x00,
        0x00,
        orientation,
        0x00,
        0x00,
        0x00, // Orientation
        0x00,
        0x00,
        0x00,
        0x00, // 下一个 IFD
    ];
    let mut app1 = vec![0xFF, 0xE1, 0x00, 0x00];
    app1.extend_from_slice(b"Exif\x00\x00");
    app1.extend_from_slice(&tiff);
    let segment_len = u16::try_from(app1.len() - 2).expect("APP1 长度");
    app1[2..4].copy_from_slice(&segment_len.to_be_bytes());
    let mut out = Vec::with_capacity(base.len() + app1.len());
    out.extend_from_slice(&base[..2]); // SOI
    out.extend_from_slice(&app1);
    out.extend_from_slice(&base[2..]);
    out
}

/// 构造最小合法 PDF（N 页 A4，含文字），供多页拆页夹具。
pub fn build_pdf(page_count: usize) -> Vec<u8> {
    let mut objects: Vec<Vec<u8>> = Vec::new();
    objects.push(b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());
    let kids: Vec<String> = (0..page_count)
        .map(|index| format!("{} 0 R", 3 + index * 3))
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
