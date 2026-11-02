//! 解码与派生图（`SPEC-capture.md` §3.3）：
//! 统一 8 位 RGB、透明通道**白底合成**、长边缩放、灰度、二值化、PNG 编码。
//!
//! 白底合成是硬要求：`hayro` 渲染的 PDF 页默认透明背景（见
//! `docs/dev/research/pdf.md` §5.1），不合成白底直接送 OCR 会严重降质
//! （`inference-serving.md` §4 Q9 实测只识别出大标题）。

use image::codecs::png::{CompressionType, FilterType as PngFilterType, PngEncoder};
use image::imageops::FilterType;
use image::{DynamicImage, ExtendedColorType, GrayImage, ImageEncoder as _, RgbImage};

use crate::error::CaptureError;
use crate::media::MediaKind;

/// 统一解码为 8 位 RGB（透明通道以白底合成；16 位线性缩放到 8 位；CMYK 由解码器转 RGB）。
///
/// # Errors
///
/// 字节损坏、格式不符或 HEIC 时返回 [`CaptureError::Decode`]。
pub fn decode(bytes: &[u8], kind: MediaKind) -> Result<RgbImage, CaptureError> {
    let format = match kind {
        MediaKind::Jpeg => image::ImageFormat::Jpeg,
        MediaKind::Png => image::ImageFormat::Png,
        MediaKind::Tiff => image::ImageFormat::Tiff,
        MediaKind::Pdf => {
            return Err(CaptureError::Decode(
                "PDF 不是位图，需先逐页渲染".to_owned(),
            ));
        }
        MediaKind::Heic => return Err(CaptureError::HeicUnsupported),
    };
    let decoded = image::load_from_memory_with_format(bytes, format)
        .map_err(|e| CaptureError::Decode(e.to_string()))?;
    Ok(to_rgb8_white(decoded))
}

/// `DynamicImage` → 8 位 RGB（透明通道与白底合成）。
pub fn to_rgb8_white(image: DynamicImage) -> RgbImage {
    match image {
        DynamicImage::ImageRgba8(rgba) => composite_on_white(&rgba),
        DynamicImage::ImageLumaA8(la) => {
            let rgba = DynamicImage::ImageLumaA8(la).to_rgba8();
            composite_on_white(&rgba)
        }
        DynamicImage::ImageRgb32F(float) => {
            let rgba = DynamicImage::ImageRgb32F(float).to_rgba8();
            composite_on_white(&rgba)
        }
        DynamicImage::ImageRgba32F(float) => {
            let rgba = DynamicImage::ImageRgba32F(float).to_rgba8();
            composite_on_white(&rgba)
        }
        other => other.to_rgb8(),
    }
}

/// 透明底合成白底：`out = src * a + 255 * (1 - a)`（预乘前的直通 alpha 合成）。
pub fn composite_on_white(rgba: &image::RgbaImage) -> RgbImage {
    let (width, height) = rgba.dimensions();
    let mut out = RgbImage::new(width, height);
    for (x, y, pixel) in rgba.enumerate_pixels() {
        let alpha = u32::from(pixel[3]);
        let blend = |channel: u8| -> u8 {
            let src = u32::from(channel) * alpha;
            let dst = 255 * (255 - alpha);
            ((src + dst) / 255) as u8
        };
        out.put_pixel(
            x,
            y,
            image::Rgb([blend(pixel[0]), blend(pixel[1]), blend(pixel[2])]),
        );
    }
    out
}

/// 按长边缩放到指定像素（放大不处理：只缩小）。
pub fn resize_long_edge(image: &RgbImage, long_edge: u32) -> RgbImage {
    let (width, height) = image.dimensions();
    let current = width.max(height);
    if long_edge == 0 || current <= long_edge {
        return image.clone();
    }
    let scale = f64::from(long_edge) / f64::from(current);
    let target_w = ((f64::from(width) * scale).round() as u32).max(1);
    let target_h = ((f64::from(height) * scale).round() as u32).max(1);
    image::imageops::resize(image, target_w, target_h, FilterType::Triangle)
}

/// 灰度图按长边缩放（质量度量的统一口径）。
pub fn resize_long_edge_gray(image: &GrayImage, long_edge: u32) -> GrayImage {
    let (width, height) = image.dimensions();
    let current = width.max(height);
    if long_edge == 0 || current <= long_edge {
        return image.clone();
    }
    let scale = f64::from(long_edge) / f64::from(current);
    let target_w = ((f64::from(width) * scale).round() as u32).max(1);
    let target_h = ((f64::from(height) * scale).round() as u32).max(1);
    image::imageops::resize(image, target_w, target_h, FilterType::Triangle)
}

/// 缩略图：长边缩放到 `thumbnail_long_edge`（小图不放大）。
pub fn thumbnail(image: &RgbImage, thumbnail_long_edge: u32) -> RgbImage {
    resize_long_edge(image, thumbnail_long_edge)
}

/// RGB → 8 位灰度（BT.601 亮度）。
pub fn to_gray(image: &RgbImage) -> GrayImage {
    image::imageops::grayscale(image)
}

/// Otsu 全局阈值二值化（返回黑白两值图：0 或 255）。
pub fn binarize(gray: &GrayImage) -> GrayImage {
    let threshold = otsu_threshold(gray);
    let (width, height) = gray.dimensions();
    let mut out = GrayImage::new(width, height);
    for (x, y, pixel) in gray.enumerate_pixels() {
        let value = if pixel[0] > threshold { 255 } else { 0 };
        out.put_pixel(x, y, image::Luma([value]));
    }
    out
}

/// Otsu 阈值（0–255）。
pub fn otsu_threshold(gray: &GrayImage) -> u8 {
    let mut histogram = [0_u64; 256];
    for pixel in gray.pixels() {
        histogram[usize::from(pixel[0])] += 1;
    }
    let total: u64 = histogram.iter().sum();
    if total == 0 {
        return 128;
    }
    let sum: f64 = histogram
        .iter()
        .enumerate()
        .map(|(value, count)| value as f64 * *count as f64)
        .sum();
    let mut sum_background = 0.0_f64;
    let mut weight_background = 0_u64;
    let mut best_threshold = 0_u8;
    let mut best_variance = -1.0_f64;
    for value in 0..256 {
        weight_background += histogram[value];
        if weight_background == 0 {
            continue;
        }
        let weight_foreground = total - weight_background;
        if weight_foreground == 0 {
            break;
        }
        sum_background += value as f64 * histogram[value] as f64;
        let mean_background = sum_background / weight_background as f64;
        let mean_foreground = (sum - sum_background) / weight_foreground as f64;
        let variance = weight_background as f64
            * weight_foreground as f64
            * (mean_background - mean_foreground).powi(2);
        if variance > best_variance {
            best_variance = variance;
            best_threshold = value as u8;
        }
    }
    best_threshold
}

/// RGB 编码为 PNG 字节（派生图入库用；编码确定 → 内容寻址稳定）。
pub fn encode_png(image: &RgbImage) -> Result<Vec<u8>, CaptureError> {
    let (width, height) = image.dimensions();
    let mut out = Vec::new();
    PngEncoder::new_with_quality(&mut out, CompressionType::Fast, PngFilterType::Adaptive)
        .write_image(image.as_raw(), width, height, ExtendedColorType::Rgb8)
        .map_err(|e| CaptureError::ImageOp(format!("PNG 编码失败：{e}")))?;
    Ok(out)
}

/// 灰度图编码为 PNG（二值图用）。
pub fn encode_png_gray(image: &GrayImage) -> Result<Vec<u8>, CaptureError> {
    let (width, height) = image.dimensions();
    let mut out = Vec::new();
    PngEncoder::new_with_quality(&mut out, CompressionType::Fast, PngFilterType::Adaptive)
        .write_image(image.as_raw(), width, height, ExtendedColorType::L8)
        .map_err(|e| CaptureError::ImageOp(format!("PNG 编码失败：{e}")))?;
    Ok(out)
}

/// 从 PNG 字节读回 RGB（测试与派生校验用）。
///
/// # Errors
///
/// 非 PNG 或维度不符时返回 [`CaptureError::Decode`]。
pub fn decode_png_rgb(bytes: &[u8]) -> Result<RgbImage, CaptureError> {
    let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .map_err(|e| CaptureError::Decode(e.to_string()))?;
    Ok(to_rgb8_white(image))
}

/// 从 PNG 字节读回灰度（测试用）。
///
/// # Errors
///
/// 非 PNG 时返回 [`CaptureError::Decode`]。
pub fn decode_png_gray(bytes: &[u8]) -> Result<GrayImage, CaptureError> {
    let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .map_err(|e| CaptureError::Decode(e.to_string()))?;
    Ok(image.to_luma8())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use image::Rgba;

    #[test]
    fn transparent_pixels_become_white() {
        let mut rgba = image::RgbaImage::new(4, 2);
        for pixel in rgba.pixels_mut() {
            *pixel = Rgba([0, 0, 0, 0]);
        }
        rgba.put_pixel(0, 0, Rgba([0, 0, 0, 255]));
        rgba.put_pixel(1, 0, Rgba([255, 0, 0, 128]));
        let rgb = composite_on_white(&rgba);
        assert_eq!(rgb.get_pixel(0, 0).0, [0, 0, 0]);
        // 半透明红：0.5*255 + 0.5*255 = 255 → 红通道保持 255，绿蓝为白底的一半。
        assert_eq!(rgb.get_pixel(1, 0).0, [255, 127, 127]);
        // 全透明像素 → 纯白（这正是 hayro 透明背景必须合成的理由）。
        assert_eq!(rgb.get_pixel(3, 1).0, [255, 255, 255]);
    }

    #[test]
    fn decode_png_with_alpha_is_composited() {
        let mut rgba = image::RgbaImage::new(2, 2);
        rgba.put_pixel(0, 0, Rgba([10, 20, 30, 0]));
        rgba.put_pixel(1, 0, Rgba([10, 20, 30, 255]));
        let mut bytes = Vec::new();
        rgba.write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
        let rgb = decode(&bytes, MediaKind::Png).unwrap();
        assert_eq!(rgb.get_pixel(0, 0).0, [255, 255, 255]);
        assert_eq!(rgb.get_pixel(1, 0).0, [10, 20, 30]);
    }

    #[test]
    fn resize_preserves_aspect_and_never_upscales() {
        let image = RgbImage::from_pixel(400, 200, image::Rgb([255, 255, 255]));
        let small = resize_long_edge(&image, 100);
        assert_eq!(small.dimensions(), (100, 50));
        let same = resize_long_edge(&image, 1000);
        assert_eq!(same.dimensions(), (400, 200));
        let thumb = thumbnail(&image, 512);
        assert_eq!(thumb.dimensions(), (400, 200));
    }

    #[test]
    fn encode_png_roundtrip_keeps_pixels() {
        let image = RgbImage::from_fn(8, 8, |x, y| image::Rgb([x as u8 * 8, y as u8 * 8, 7]));
        let bytes = encode_png(&image).unwrap();
        let back = decode_png_rgb(&bytes).unwrap();
        assert_eq!(back.dimensions(), (8, 8));
        assert_eq!(back.get_pixel(3, 5).0, image.get_pixel(3, 5).0);
    }

    #[test]
    fn binarize_splits_light_and_dark() {
        let mut gray = GrayImage::new(4, 2);
        for (x, _y, pixel) in gray.enumerate_pixels_mut() {
            *pixel = image::Luma([if x < 2 { 10 } else { 240 }]);
        }
        let binary = binarize(&gray);
        assert_eq!(binary.get_pixel(0, 0).0, [0]);
        assert_eq!(binary.get_pixel(3, 0).0, [255]);
    }
}
