//! 页面轮廓检测与透视矫正（`SPEC-capture.md` §3.1「轮廓与矫正」、§3.3 接受条件）。
//!
//! 实现是**保守的纯 CPU 检测**（无 Canny/Hough）：只在「深色背景上放一张亮色纸」的
//! 典型手机拍摄场景生效；扫描件（页面铺满画面）检测不到轮廓时按规范回退整图并记
//! [`PageNote::NoContour`]，不阻塞流程。
//!
//! 接受条件（规范 §3.3）：四边形面积 ≥ 原图 30%、长宽比 0.5–2.0、近似凸形；
//! 矫正后面积 < 原图 40% 或 A4 纵横比偏差 > 15% → 回退到方向校正图并记
//! [`PageNote::PerspectiveFallback`]。

use image::{GrayImage, RgbImage};

use crate::error::PageNote;
use crate::imageops;
use crate::transform::{Point, Quad, warp};

/// 检测阶段的降采样长边（速度优先）。
const DETECT_LONG_EDGE: u32 = 360;
/// 背景与纸张的最小亮度差。
const MIN_CONTRAST: f64 = 35.0;
/// 背景被视为「深色」的上限（高于此值不做亮纸检测，避免扫描件误判）。
const DARK_BACKGROUND_MAX: f64 = 140.0;
/// 最小候选面积占比（低于此视为没找到）。
const MIN_CANDIDATE_AREA: f64 = 0.10;
/// 接受条件：四边形面积占原图比例下限（规范 §3.3）。
const MIN_QUAD_AREA: f64 = 0.30;
/// 接受条件：长宽比范围（规范 §3.3）。
const ASPECT_MIN: f64 = 0.5;
/// 接受条件：长宽比范围（规范 §3.3）。
const ASPECT_MAX: f64 = 2.0;
/// 透视回退：矫正后面积占原图比例下限（规范 §3.3）。
const MIN_RECTIFIED_AREA: f64 = 0.40;
/// 透视回退：A4 纵横比允许偏差（规范 §3.3）。
const A4_ASPECT_TOLERANCE: f64 = 0.15;
/// A4 纵向长宽比（宽/高）。
const A4_PORTRAIT: f64 = 210.0 / 297.0;
/// A4 横向长宽比（宽/高）。
const A4_LANDSCAPE: f64 = 297.0 / 210.0;

/// 轮廓检测结果。
#[derive(Debug, Clone, PartialEq)]
pub struct Detection {
    /// 原图坐标下的四边形（TL、TR、BR、BL）。
    pub quad: Quad,
    /// 四边形面积 / 原图面积。
    pub area_ratio: f64,
    /// 提示（可能多页等）。
    pub notes: Vec<PageNote>,
}

/// 矫正结果。
#[derive(Debug, Clone)]
pub struct RectifiedPage {
    /// 矫正后的图（回退时为方向校正图的克隆）。
    pub image: RgbImage,
    /// 命中的四边形（回退为 `None`）。
    pub quad: Option<Quad>,
    /// 是否回退到整图。
    pub fallback: bool,
    /// 提示码。
    pub notes: Vec<PageNote>,
}

/// 检测页面四边形（深色背景 + 亮色纸张场景）。
///
/// 返回 `None` 表示未找到合格四边形（调用方回退整图）。
pub fn detect_document_quad(image: &RgbImage) -> Option<Detection> {
    let (original_width, original_height) = image.dimensions();
    if original_width < 32 || original_height < 32 {
        return None;
    }
    let small = imageops::resize_long_edge(image, DETECT_LONG_EDGE);
    let gray = imageops::to_gray(&small);
    let (width, height) = gray.dimensions();
    let background = border_mean(&gray);
    if background > DARK_BACKGROUND_MAX {
        return None;
    }
    let mut mask = vec![false; (width * height) as usize];
    for (x, y, pixel) in gray.enumerate_pixels() {
        let value = f64::from(pixel[0]);
        if value > background + MIN_CONTRAST {
            mask[(y * width + x) as usize] = true;
        }
    }
    let (components, labels) = connected_components(&mask, width, height);
    let mut largest: Option<(usize, u64)> = None;
    for (label, area, _bounds) in &components {
        if *area < u64::from(width * height) / 100 {
            continue;
        }
        if largest.is_none_or(|(_, best)| *area > best) {
            largest = Some((*label, *area));
        }
    }
    let (label, area) = largest?;
    let candidate_area = area as f64 / f64::from(width * height);
    if candidate_area < MIN_CANDIDATE_AREA {
        return None;
    }
    let bounds = components
        .iter()
        .find(|(component, _, _)| *component == label)
        .map(|(_, _, bounds)| *bounds)?;
    let quad_small = extreme_quad(&mask, &labels, width, &bounds, label);
    let scale_x = f64::from(original_width) / f64::from(width);
    let scale_y = f64::from(original_height) / f64::from(height);
    let quad = Quad::new(
        quad_small
            .points
            .map(|point| Point::new(point.x * scale_x, point.y * scale_y)),
    );
    let original_area = f64::from(original_width) * f64::from(original_height);
    let area_ratio = quad.area() / original_area;
    let aspect = quad.aspect();
    let sides = quad.side_lengths();
    let min_side = sides.iter().copied().fold(f64::MAX, f64::min);
    if !(MIN_QUAD_AREA..=0.99).contains(&area_ratio)
        || !(ASPECT_MIN..=ASPECT_MAX).contains(&aspect)
        || !quad.is_convex()
        || min_side < 20.0
    {
        return None;
    }
    let mut notes = Vec::new();
    // 同框多页：存在至少 2 个面积 ≥15% 的连通域。
    let large_components = components
        .iter()
        .filter(|(_, area, _)| *area as f64 / f64::from(width * height) >= 0.15)
        .count();
    if large_components >= 2 {
        notes.push(PageNote::MultiplePages);
    }
    Some(Detection {
        quad,
        area_ratio,
        notes,
    })
}

/// 检测 + 矫正；失败或异常时回退整图（不返回错误）。
pub fn rectify_page(rotated: &RgbImage) -> RectifiedPage {
    let Some(detection) = detect_document_quad(rotated) else {
        return RectifiedPage {
            image: rotated.clone(),
            quad: None,
            fallback: true,
            notes: vec![PageNote::NoContour],
        };
    };
    let (width, height) = detection.quad.mean_size();
    let output_width = width.round().clamp(50.0, 4000.0) as u32;
    let output_height = height.round().clamp(50.0, 4000.0) as u32;
    let warped = match warp(rotated, &detection.quad, output_width, output_height) {
        Ok(image) => image,
        Err(_) => {
            return RectifiedPage {
                image: rotated.clone(),
                quad: None,
                fallback: true,
                notes: vec![PageNote::PerspectiveFallback],
            };
        }
    };
    let original_area = f64::from(rotated.width()) * f64::from(rotated.height());
    let rectified_area = f64::from(warped.width()) * f64::from(warped.height());
    let area_ratio = rectified_area / original_area;
    let aspect = f64::from(warped.width()) / f64::from(warped.height());
    let portrait_deviation = (aspect - A4_PORTRAIT).abs() / A4_PORTRAIT;
    let landscape_deviation = (aspect - A4_LANDSCAPE).abs() / A4_LANDSCAPE;
    let a4_deviation = portrait_deviation.min(landscape_deviation);
    if area_ratio < MIN_RECTIFIED_AREA || a4_deviation > A4_ASPECT_TOLERANCE {
        let mut notes = detection.notes;
        notes.push(PageNote::PerspectiveFallback);
        return RectifiedPage {
            image: rotated.clone(),
            quad: None,
            fallback: true,
            notes,
        };
    }
    let mut notes = detection.notes;
    notes.sort();
    notes.dedup();
    RectifiedPage {
        image: warped,
        quad: Some(detection.quad),
        fallback: false,
        notes,
    }
}

fn border_mean(gray: &GrayImage) -> f64 {
    let (width, height) = gray.dimensions();
    let mut sum = 0.0_f64;
    let mut count = 0_u64;
    for x in 0..width {
        for y in [0, height - 1] {
            sum += f64::from(gray.get_pixel(x, y)[0]);
            count += 1;
        }
    }
    for y in 0..height {
        for x in [0, width - 1] {
            sum += f64::from(gray.get_pixel(x, y)[0]);
            count += 1;
        }
    }
    if count == 0 { 0.0 } else { sum / count as f64 }
}

/// 4 邻域连通域：返回 (label, area, bounds)；label 从 1 起。
fn connected_components(
    mask: &[bool],
    width: u32,
    height: u32,
) -> (Vec<(usize, u64, (u32, u32, u32, u32))>, Vec<usize>) {
    let mut labels = vec![0_usize; mask.len()];
    let mut components: Vec<(usize, u64, (u32, u32, u32, u32))> = Vec::new();
    let mut next_label = 1_usize;
    let mut stack: Vec<(u32, u32)> = Vec::new();
    for y in 0..height {
        for x in 0..width {
            let index = (y * width + x) as usize;
            if !mask[index] || labels[index] != 0 {
                continue;
            }
            let label = next_label;
            next_label += 1;
            let mut area = 0_u64;
            let mut min_x = width;
            let mut min_y = height;
            let mut max_x = 0_u32;
            let mut max_y = 0_u32;
            stack.push((x, y));
            labels[index] = label;
            while let Some((cx, cy)) = stack.pop() {
                area += 1;
                min_x = min_x.min(cx);
                min_y = min_y.min(cy);
                max_x = max_x.max(cx);
                max_y = max_y.max(cy);
                let neighbours = [
                    (cx.wrapping_sub(1), cy),
                    (cx + 1, cy),
                    (cx, cy.wrapping_sub(1)),
                    (cx, cy + 1),
                ];
                for (nx, ny) in neighbours {
                    if nx >= width || ny >= height {
                        continue;
                    }
                    let neighbour_index = (ny * width + nx) as usize;
                    if mask[neighbour_index] && labels[neighbour_index] == 0 {
                        labels[neighbour_index] = label;
                        stack.push((nx, ny));
                    }
                }
            }
            components.push((label, area, (min_x, min_y, max_x, max_y)));
        }
    }
    (components, labels)
}

fn extreme_quad(
    mask: &[bool],
    labels: &[usize],
    width: u32,
    bounds: &(u32, u32, u32, u32),
    label: usize,
) -> Quad {
    let (min_x, min_y, max_x, max_y) = *bounds;
    let mut tl = Point::new(f64::from(min_x), f64::from(min_y));
    let mut tr = Point::new(f64::from(max_x), f64::from(min_y));
    let mut br = Point::new(f64::from(max_x), f64::from(max_y));
    let mut bl = Point::new(f64::from(min_x), f64::from(max_y));
    let mut best_sum_min = f64::MAX;
    let mut best_sum_max = f64::MIN;
    let mut best_diff_max = f64::MIN;
    let mut best_diff_min = f64::MAX;
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let index = (y * width + x) as usize;
            if !mask[index] || labels[index] != label {
                continue;
            }
            let px = f64::from(x);
            let py = f64::from(y);
            let sum = px + py;
            let diff = px - py;
            if sum < best_sum_min {
                best_sum_min = sum;
                tl = Point::new(px, py);
            }
            if sum > best_sum_max {
                best_sum_max = sum;
                br = Point::new(px, py);
            }
            if diff > best_diff_max {
                best_diff_max = diff;
                tr = Point::new(px, py);
            }
            if diff < best_diff_min {
                best_diff_min = diff;
                bl = Point::new(px, py);
            }
        }
    }
    Quad::new([tl, tr, br, bl])
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// 深色背景上放一张旋转的亮色 A4 纸（无透视，只旋转）。
    fn rotated_paper(
        width: u32,
        height: u32,
        paper_w: f64,
        paper_h: f64,
        degrees: f64,
    ) -> RgbImage {
        let mut image = RgbImage::from_pixel(width, height, image::Rgb([38, 40, 42]));
        let center_x = f64::from(width) / 2.0;
        let center_y = f64::from(height) / 2.0;
        let radians = -degrees.to_radians();
        let (sin, cos) = (radians.sin(), radians.cos());
        for (x, y, pixel) in image.enumerate_pixels_mut() {
            let dx = f64::from(x) - center_x;
            let dy = f64::from(y) - center_y;
            let u = dx * cos - dy * sin;
            let v = dx * sin + dy * cos;
            if u.abs() <= paper_w / 2.0 && v.abs() <= paper_h / 2.0 {
                *pixel = image::Rgb([247, 247, 245]);
            }
        }
        image
    }

    fn text_like(image: &mut RgbImage) {
        let (width, height) = image.dimensions();
        for y in (height / 10..height * 9 / 10).step_by(40) {
            for x in width / 6..width * 5 / 6 {
                for dy in 0..5 {
                    let px = image.get_pixel(x, y + dy);
                    if px[0] > 200 {
                        image.put_pixel(x, y + dy, image::Rgb([30, 30, 30]));
                    }
                }
            }
        }
    }

    #[test]
    fn detects_rotated_a4_paper() {
        let image = rotated_paper(900, 1200, 600.0, 848.0, 7.0);
        let detection = detect_document_quad(&image).expect("应检测到纸张");
        // 面积占比接近 (600*848)/(900*1200)=0.47。
        assert!(
            (detection.area_ratio - 0.47).abs() < 0.08,
            "area_ratio={}",
            detection.area_ratio
        );
        assert!((detection.quad.aspect() - 210.0 / 297.0).abs() < 0.06);
        let rectified = rectify_page(&image);
        assert!(!rectified.fallback, "notes={:?}", rectified.notes);
        assert!(rectified.quad.is_some());
        // 矫正后应为接近 A4 的尺寸，且面积占原图比例 ≥ 40%。
        let aspect = f64::from(rectified.image.width()) / f64::from(rectified.image.height());
        assert!((aspect - 210.0 / 297.0).abs() < 0.15 * (210.0 / 297.0));
    }

    #[test]
    fn scan_filling_frame_falls_back_to_whole_image() {
        let mut image = RgbImage::from_pixel(800, 1100, image::Rgb([248, 248, 248]));
        text_like(&mut image);
        assert!(detect_document_quad(&image).is_none());
        let rectified = rectify_page(&image);
        assert!(rectified.fallback);
        assert!(rectified.notes.contains(&PageNote::NoContour));
        assert_eq!(rectified.image.dimensions(), image.dimensions());
    }

    #[test]
    fn bad_aspect_is_rejected() {
        // 3:1 的亮色长条（不是纸张）。
        let image = rotated_paper(900, 600, 800.0, 260.0, 0.0);
        assert!(detect_document_quad(&image).is_none());
        let rectified = rectify_page(&image);
        assert!(rectified.fallback);
    }

    #[test]
    fn tiny_image_never_panics() {
        let image = RgbImage::from_pixel(4, 4, image::Rgb([0, 0, 0]));
        assert!(detect_document_quad(&image).is_none());
        assert!(rectify_page(&image).fallback);
    }

    #[test]
    fn two_papers_flag_multiple_pages() {
        // 画布 1000×1000：左侧 500×700（35%），右侧 380×600（22.8%），互不相连。
        let mut image = RgbImage::from_pixel(1000, 1000, image::Rgb([36, 38, 40]));
        for y in 150..850 {
            for x in 0..500 {
                image.put_pixel(x, y, image::Rgb([248, 248, 246]));
            }
        }
        for y in 200..800 {
            for x in 620..1000 {
                image.put_pixel(x, y, image::Rgb([248, 248, 246]));
            }
        }
        let detection = detect_document_quad(&image).expect("应检测到纸张");
        assert!(
            detection.notes.contains(&PageNote::MultiplePages),
            "notes={:?}",
            detection.notes
        );
    }
}
