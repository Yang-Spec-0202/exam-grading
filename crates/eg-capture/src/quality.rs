//! 质量门控（`SPEC-capture.md` §3.3）：模糊、曝光、高光/死黑、倾斜、分辨率、文字行结构。
//!
//! 阈值全部来自 [`ImportSettings`]（快照自 `CONFIG.md` §2）；内部「良好」线按规范 §3.3
//! 表格的内置默认值（300 / 0.25–0.75 / 2° / 2400px / 10% 高光）。
//! 模糊方差统一在长边 `import.quality_min_long_edge` 尺度上计算，避免分辨率影响阈值。

use eg_domain::{PageQuality, QualityVerdict};
use image::{GrayImage, RgbImage};

use crate::error::RejectReason;
use crate::imageops;
use crate::settings::ImportSettings;

/// 良好线：模糊方差（内部默认，规范 §3.3）。
const GOOD_BLUR_VAR: f64 = 300.0;
/// 良好线：曝光度量下界（内部默认）。
const GOOD_EXPOSURE_MIN: f64 = 0.25;
/// 良好线：曝光度量上界（内部默认）。
const GOOD_EXPOSURE_MAX: f64 = 0.75;
/// 良好线：倾斜角上限（内部默认）。
const GOOD_SKEW_MAX: f64 = 2.0;
/// 良好线：分辨率长边（内部默认）。
const GOOD_LONG_EDGE: u32 = 2_400;
/// 良好线：高光溢出占比上限（内部默认）。
const GOOD_HIGHLIGHT_MAX: f64 = 0.10;
/// 拒收线：高光溢出占比 25%。
const REJECT_HIGHLIGHT: f64 = 0.25;
/// 拒收线：近黑像素占比 40%。
const REJECT_DARK: f64 = 0.40;
/// 倾斜角搜索步长（度）。
const SKEW_STEP_DEG: f64 = 0.5;
/// 行结构峰度下限（投影方差 / 均值²）。
const LINE_STRUCTURE_MIN_PEAKINESS: f64 = 0.20;
/// 倾斜估计的降采样长边（速度与精度平衡）。
const SKEW_WORK_LONG_EDGE: u32 = 600;

/// 质量度量（`PageQuality` + 原因码与提示）。
#[derive(Debug, Clone, PartialEq)]
pub struct QualityMetrics {
    /// 拉普拉斯方差（越大越清晰；长边 `quality_min_long_edge` 口径）。
    pub blur_var: f64,
    /// 曝光度量（平均亮度 0–1）。
    pub exposure: f64,
    /// 高光溢出像素占比（亮度 ≥ 250）。
    pub highlight_ratio: f64,
    /// 近黑像素占比（亮度 ≤ 5）。
    pub dark_ratio: f64,
    /// 倾斜角（度，绝对值）。
    pub skew_deg: f64,
    /// 原图长边（像素）。
    pub long_edge: u32,
    /// 是否有文字行结构（空白/倒置/噪声为 false）。
    pub line_structure: bool,
}

/// 质量判定结果。
#[derive(Debug, Clone, PartialEq)]
pub struct QualityAssessment {
    /// 度量。
    pub metrics: QualityMetrics,
    /// 判定（良好/可接受/拒绝）。
    pub verdict: QualityVerdict,
    /// 拒收原因（按严重度稳定排序；`verdict` 为拒绝时非空）。
    pub reasons: Vec<RejectReason>,
}

impl QualityAssessment {
    /// 是否被拒收（不进识别）。
    pub fn rejected(&self) -> bool {
        matches!(self.verdict, QualityVerdict::Rejected { .. })
    }

    /// 领域质量对象（写入 `Page.quality`）。
    pub fn page_quality(&self) -> PageQuality {
        PageQuality {
            blur_var: self.metrics.blur_var,
            exposure: self.metrics.exposure,
            skew_deg: self.metrics.skew_deg,
            resolution_ok: !self.reasons.contains(&RejectReason::Resolution),
            verdict: self.verdict.clone(),
        }
    }
}

/// 计算质量度量（输入为方向校正 + 透视矫正后的页面图）。
pub fn measure(image: &RgbImage, settings: &ImportSettings) -> QualityMetrics {
    let (width, height) = image.dimensions();
    let long_edge = width.max(height);
    // 模糊/曝光/高光在统一长边尺度上度量（规范 §3.3）。
    let work = imageops::resize_long_edge(image, settings.quality_min_long_edge);
    let gray = imageops::to_gray(&work);
    let blur_var = laplacian_variance(&gray);
    let mut sum = 0_u64;
    let mut highlight = 0_u64;
    let mut dark = 0_u64;
    let total = u64::from(gray.width()) * u64::from(gray.height());
    for pixel in gray.pixels() {
        let value = u64::from(pixel[0]);
        sum += value;
        if value >= 250 {
            highlight += 1;
        }
        if value <= 5 {
            dark += 1;
        }
    }
    let exposure = if total == 0 {
        0.0
    } else {
        sum as f64 / total as f64 / 255.0
    };
    let (skew_deg, line_structure) = estimate_skew(&gray, settings.quality_skew_max);
    QualityMetrics {
        blur_var,
        exposure,
        highlight_ratio: ratio(highlight, total),
        dark_ratio: ratio(dark, total),
        skew_deg,
        long_edge,
        line_structure,
    }
}

fn ratio(count: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        count as f64 / total as f64
    }
}

/// 3×3 拉普拉斯核的响应方差。
pub fn laplacian_variance(gray: &GrayImage) -> f64 {
    let (width, height) = gray.dimensions();
    if width < 3 || height < 3 {
        return 0.0;
    }
    let mut sum = 0.0_f64;
    let mut sum_sq = 0.0_f64;
    let mut count = 0_u64;
    for y in 1..height - 1 {
        for x in 1..width - 1 {
            let center = f64::from(gray.get_pixel(x, y)[0]);
            let up = f64::from(gray.get_pixel(x, y - 1)[0]);
            let down = f64::from(gray.get_pixel(x, y + 1)[0]);
            let left = f64::from(gray.get_pixel(x - 1, y)[0]);
            let right = f64::from(gray.get_pixel(x + 1, y)[0]);
            let response = up + down + left + right - 4.0 * center;
            sum += response;
            sum_sq += response * response;
            count += 1;
        }
    }
    if count == 0 {
        return 0.0;
    }
    let mean = sum / count as f64;
    (sum_sq / count as f64) - mean * mean
}

/// 估计倾斜角（度）与是否存在文字行结构。
///
/// 方法：Otsu 二值化取暗像素 → 在 ±max_deg 内按 [`SKEW_STEP_DEG`] 扫描，
/// 以「行投影方差 / 均值²」最大者为最佳对齐角（峰度同时作为行结构判据）。
pub fn estimate_skew(gray: &GrayImage, max_deg: f64) -> (f64, bool) {
    let work = imageops::resize_long_edge_gray(gray, SKEW_WORK_LONG_EDGE);
    let (width, height) = work.dimensions();
    if width < 8 || height < 8 {
        return (0.0, false);
    }
    let threshold = imageops::otsu_threshold(&work);
    let mut points: Vec<(f64, f64)> = Vec::new();
    for (x, y, pixel) in work.enumerate_pixels() {
        if pixel[0] <= threshold {
            points.push((f64::from(x), f64::from(y)));
        }
    }
    let dark_ratio = points.len() as f64 / (f64::from(width) * f64::from(height));
    if points.is_empty() || dark_ratio > 0.60 {
        return (0.0, false);
    }
    let scan = max_deg.clamp(0.0, 15.0) + 2.0;
    let mut best_angle = 0.0_f64;
    let mut best_peakiness = 0.0_f64;
    let mut angle = -scan;
    while angle <= scan {
        let radians = angle.to_radians();
        let (sin, cos) = (radians.sin(), radians.cos());
        let mut rows = vec![0.0_f64; (width + height) as usize];
        for &(x, y) in &points {
            let row = y * cos - x * sin + f64::from(width);
            let index = row.round();
            if index >= 0.0 && index < rows.len() as f64 {
                rows[index as usize] += 1.0;
            }
        }
        let peakiness = peakiness(&rows);
        if peakiness > best_peakiness {
            best_peakiness = peakiness;
            best_angle = angle;
        }
        angle += SKEW_STEP_DEG;
    }
    let line_structure = best_peakiness >= LINE_STRUCTURE_MIN_PEAKINESS && dark_ratio <= 0.60;
    (best_angle.abs(), line_structure)
}

fn peakiness(values: &[f64]) -> f64 {
    let count = values.len() as f64;
    if count == 0.0 {
        return 0.0;
    }
    let mean = values.iter().sum::<f64>() / count;
    if mean <= f64::EPSILON {
        return 0.0;
    }
    let variance = values
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f64>()
        / count;
    variance / (mean * mean)
}

/// 按规范 §3.3 门控表判定（任一拒收条件命中即拒绝并附**全部**原因码）。
pub fn classify(metrics: &QualityMetrics, settings: &ImportSettings) -> QualityAssessment {
    let mut reasons = Vec::new();
    if metrics.blur_var < settings.quality_blur_threshold {
        reasons.push(RejectReason::Blur);
    }
    if metrics.exposure < settings.quality_exposure_min {
        reasons.push(RejectReason::Underexposed);
    }
    if metrics.exposure > settings.quality_exposure_max {
        reasons.push(RejectReason::Overexposed);
    }
    if metrics.highlight_ratio > REJECT_HIGHLIGHT {
        reasons.push(RejectReason::Glare);
    }
    if metrics.dark_ratio > REJECT_DARK {
        reasons.push(RejectReason::DeadBlack);
    }
    if metrics.skew_deg > settings.quality_skew_max {
        reasons.push(RejectReason::Skew);
    }
    if metrics.long_edge < settings.quality_min_long_edge {
        reasons.push(RejectReason::Resolution);
    }
    if !metrics.line_structure {
        reasons.push(RejectReason::NoLineStructure);
    }
    reasons.sort();
    reasons.dedup();
    let verdict = if !reasons.is_empty() {
        let reason = reasons
            .iter()
            .map(|reason| reason.message())
            .collect::<Vec<_>>()
            .join("；");
        QualityVerdict::Rejected { reason }
    } else if is_good(metrics) {
        QualityVerdict::Good
    } else {
        QualityVerdict::Acceptable
    };
    QualityAssessment {
        metrics: metrics.clone(),
        verdict,
        reasons,
    }
}

fn is_good(metrics: &QualityMetrics) -> bool {
    metrics.blur_var >= GOOD_BLUR_VAR
        && (GOOD_EXPOSURE_MIN..=GOOD_EXPOSURE_MAX).contains(&metrics.exposure)
        && metrics.skew_deg <= GOOD_SKEW_MAX
        && metrics.long_edge >= GOOD_LONG_EDGE
        && metrics.highlight_ratio <= GOOD_HIGHLIGHT_MAX
}

/// 从图像一次完成「度量 + 判定」。
pub fn assess(image: &RgbImage, settings: &ImportSettings) -> QualityAssessment {
    classify(&measure(image, settings), settings)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use image::{Luma, Rgb};

    /// 合成「文字页」：白底 + 深色横条（有行结构、清晰）。
    fn text_page(width: u32, height: u32, line_gap: u32) -> RgbImage {
        let mut image = RgbImage::from_pixel(width, height, Rgb([246, 246, 246]));
        let mut y = height / 10;
        while y + 8 < height {
            for x in width / 10..width * 9 / 10 {
                for dy in 0..8 {
                    image.put_pixel(x, y + dy, Rgb([20, 20, 20]));
                }
            }
            y += line_gap;
        }
        image
    }

    fn box_blur(image: &RgbImage, radius: u32) -> RgbImage {
        image::imageops::blur(image, radius as f32)
    }

    fn uniform(width: u32, height: u32, value: u8) -> RgbImage {
        RgbImage::from_pixel(width, height, Rgb([value, value, value]))
    }

    #[test]
    fn sharp_text_page_has_line_structure_and_low_skew() {
        let settings = ImportSettings::default();
        let image = text_page(1200, 1700, 60);
        let metrics = measure(&image, &settings);
        assert!(metrics.blur_var > 120.0, "blur_var={}", metrics.blur_var);
        assert!(metrics.line_structure, "应检测到行结构");
        assert!(metrics.skew_deg <= 2.0, "skew={}", metrics.skew_deg);
        let assessment = classify(&metrics, &settings);
        assert!(!assessment.rejected(), "{:?}", assessment.reasons);
    }

    #[test]
    fn blurred_page_is_rejected_with_blur_reason() {
        let settings = ImportSettings::default();
        let sharp = text_page(1200, 1700, 60);
        let blurred = box_blur(&sharp, 12);
        let metrics = measure(&blurred, &settings);
        assert!(metrics.blur_var < 120.0, "blur_var={}", metrics.blur_var);
        let assessment = classify(&metrics, &settings);
        assert!(assessment.reasons.contains(&RejectReason::Blur));
        assert!(assessment.rejected());
    }

    #[test]
    fn dark_and_bright_pages_hit_exposure_reasons() {
        let settings = ImportSettings::default();
        let dark = classify(&measure(&uniform(1600, 1600, 4), &settings), &settings);
        assert!(
            dark.reasons.contains(&RejectReason::Underexposed),
            "{:?}",
            dark.reasons
        );
        let bright = classify(&measure(&uniform(1600, 1600, 252), &settings), &settings);
        assert!(
            bright.reasons.contains(&RejectReason::Overexposed),
            "{:?}",
            bright.reasons
        );
        assert!(
            bright.reasons.contains(&RejectReason::Glare),
            "{:?}",
            bright.reasons
        );
    }

    #[test]
    fn blank_page_has_no_line_structure() {
        let settings = ImportSettings::default();
        let metrics = measure(&uniform(1600, 1600, 246), &settings);
        assert!(!metrics.line_structure);
        let assessment = classify(&metrics, &settings);
        assert!(assessment.reasons.contains(&RejectReason::NoLineStructure));
    }

    #[test]
    fn small_image_hits_resolution_reason() {
        let settings = ImportSettings::default();
        let metrics = measure(&text_page(800, 1100, 40), &settings);
        assert_eq!(metrics.long_edge, 1100);
        let assessment = classify(&metrics, &settings);
        assert!(assessment.reasons.contains(&RejectReason::Resolution));
    }

    #[test]
    fn exposure_threshold_boundary_both_sides() {
        let mut settings = ImportSettings::default();
        settings.quality_exposure_min = 0.12;
        settings.quality_exposure_max = 0.90;
        let mut metrics = QualityMetrics {
            blur_var: 500.0,
            exposure: 0.119,
            highlight_ratio: 0.0,
            dark_ratio: 0.0,
            skew_deg: 0.0,
            long_edge: 2400,
            line_structure: true,
        };
        assert!(
            classify(&metrics, &settings)
                .reasons
                .contains(&RejectReason::Underexposed)
        );
        metrics.exposure = 0.12;
        assert!(
            !classify(&metrics, &settings)
                .reasons
                .contains(&RejectReason::Underexposed)
        );
        metrics.exposure = 0.901;
        assert!(
            classify(&metrics, &settings)
                .reasons
                .contains(&RejectReason::Overexposed)
        );
        metrics.exposure = 0.90;
        assert!(
            !classify(&metrics, &settings)
                .reasons
                .contains(&RejectReason::Overexposed)
        );
    }

    #[test]
    fn blur_threshold_boundary_both_sides() {
        let settings = ImportSettings::default();
        let mut metrics = QualityMetrics {
            blur_var: 119.99,
            exposure: 0.5,
            highlight_ratio: 0.0,
            dark_ratio: 0.0,
            skew_deg: 0.0,
            long_edge: 2400,
            line_structure: true,
        };
        assert!(
            classify(&metrics, &settings)
                .reasons
                .contains(&RejectReason::Blur)
        );
        metrics.blur_var = 120.0;
        assert!(
            !classify(&metrics, &settings)
                .reasons
                .contains(&RejectReason::Blur)
        );
    }

    #[test]
    fn skew_threshold_boundary_both_sides() {
        let settings = ImportSettings::default();
        let mut metrics = QualityMetrics {
            blur_var: 500.0,
            exposure: 0.5,
            highlight_ratio: 0.0,
            dark_ratio: 0.0,
            skew_deg: 8.01,
            long_edge: 2400,
            line_structure: true,
        };
        assert!(
            classify(&metrics, &settings)
                .reasons
                .contains(&RejectReason::Skew)
        );
        metrics.skew_deg = 8.0;
        assert!(
            !classify(&metrics, &settings)
                .reasons
                .contains(&RejectReason::Skew)
        );
    }

    #[test]
    fn resolution_threshold_boundary_both_sides() {
        let settings = ImportSettings::default();
        let mut metrics = QualityMetrics {
            blur_var: 500.0,
            exposure: 0.5,
            highlight_ratio: 0.0,
            dark_ratio: 0.0,
            skew_deg: 0.0,
            long_edge: 1599,
            line_structure: true,
        };
        assert!(
            classify(&metrics, &settings)
                .reasons
                .contains(&RejectReason::Resolution)
        );
        metrics.long_edge = 1600;
        assert!(
            !classify(&metrics, &settings)
                .reasons
                .contains(&RejectReason::Resolution)
        );
    }

    #[test]
    fn good_requires_all_good_lines() {
        let settings = ImportSettings::default();
        let metrics = QualityMetrics {
            blur_var: 400.0,
            exposure: 0.5,
            highlight_ratio: 0.0,
            dark_ratio: 0.05,
            skew_deg: 1.0,
            long_edge: 2400,
            line_structure: true,
        };
        assert_eq!(classify(&metrics, &settings).verdict, QualityVerdict::Good);
        let acceptable = QualityMetrics {
            blur_var: 200.0,
            ..metrics.clone()
        };
        assert_eq!(
            classify(&acceptable, &settings).verdict,
            QualityVerdict::Acceptable
        );
    }

    #[test]
    fn rotation_by_known_angle_is_detected() {
        let settings = ImportSettings::default();
        let page = text_page(1200, 1700, 60);
        let angle = 10.0_f64.to_radians();
        let rotated = rotate(page, angle);
        let metrics = measure(&rotated, &settings);
        assert!(
            (metrics.skew_deg - 10.0).abs() <= 1.5,
            "估计角={} 期望≈10°",
            metrics.skew_deg
        );
        let assessment = classify(&metrics, &settings);
        assert!(
            assessment.reasons.contains(&RejectReason::Skew),
            "{:?}",
            assessment.reasons
        );
        // 阈值内（6°）应可接受、不被拒。
        let mild = measure(
            &rotate(text_page(1200, 1700, 60), 6.0_f64.to_radians()),
            &settings,
        );
        assert!(
            !classify(&mild, &settings)
                .reasons
                .contains(&RejectReason::Skew)
        );
    }

    /// 以最近邻+双线性混合方式旋转整幅 RGB（测试夹具）。
    fn rotate(image: RgbImage, radians: f64) -> RgbImage {
        let (width, height) = image.dimensions();
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

    #[test]
    fn laplacian_variance_is_zero_on_flat_image() {
        let gray = GrayImage::from_pixel(16, 16, Luma([128]));
        assert_eq!(laplacian_variance(&gray), 0.0);
        assert_eq!(laplacian_variance(&GrayImage::new(2, 2)), 0.0);
    }
}
