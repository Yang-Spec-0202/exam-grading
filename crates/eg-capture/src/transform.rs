//! 单应变换与四边形几何（`SPEC-capture.md` §3.3「透视矫正」）。
//!
//! 只做纯 CPU 的 8 参数单应（输出矩形 → 原图四边形）与双线性采样，
//! 不引入 OpenCV 等重型依赖。

use image::RgbImage;

use crate::error::CaptureError;

/// 二维点（像素坐标，原点左上）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    /// x 坐标。
    pub x: f64,
    /// y 坐标。
    pub y: f64,
}

impl Point {
    /// 构造点。
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    /// 到另一点的欧氏距离。
    pub fn distance(self, other: Self) -> f64 {
        ((self.x - other.x).powi(2) + (self.y - other.y).powi(2)).sqrt()
    }
}

/// 四边形，顺序固定为「左上、右上、右下、左下」（输出矩形的对应角）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quad {
    /// 四个角点（TL、TR、BR、BL）。
    pub points: [Point; 4],
}

impl Quad {
    /// 构造四边形。
    pub const fn new(points: [Point; 4]) -> Self {
        Self { points }
    }

    /// 鞋带公式面积（绝对值）。
    pub fn area(&self) -> f64 {
        let p = &self.points;
        let mut sum = 0.0_f64;
        for index in 0..4 {
            let next = (index + 1) % 4;
            sum += p[index].x * p[next].y - p[next].x * p[index].y;
        }
        sum.abs() / 2.0
    }

    /// 四条边长（上、右、下、左）。
    pub fn side_lengths(&self) -> [f64; 4] {
        let [tl, tr, br, bl] = self.points;
        [
            tl.distance(tr),
            tr.distance(br),
            br.distance(bl),
            bl.distance(tl),
        ]
    }

    /// 平均宽度（上/下边）与平均高度（左/右边）。
    pub fn mean_size(&self) -> (f64, f64) {
        let [top, right, bottom, left] = self.side_lengths();
        ((top + bottom) / 2.0, (left + right) / 2.0)
    }

    /// 长宽比（宽 / 高）。
    pub fn aspect(&self) -> f64 {
        let (width, height) = self.mean_size();
        if height <= f64::EPSILON {
            0.0
        } else {
            width / height
        }
    }

    /// 是否为凸四边形（四个叉积同号且非退化）。
    pub fn is_convex(&self) -> bool {
        let p = &self.points;
        let mut sign = 0_i32;
        for index in 0..4 {
            let a = p[index];
            let b = p[(index + 1) % 4];
            let c = p[(index + 2) % 4];
            let cross = (b.x - a.x) * (c.y - b.y) - (b.y - a.y) * (c.x - b.x);
            if cross.abs() < 1e-9 {
                continue;
            }
            let current = if cross > 0.0 { 1 } else { -1 };
            if sign == 0 {
                sign = current;
            } else if sign != current {
                return false;
            }
        }
        sign != 0
    }
}

/// 由「输出矩形四角 → 原图四边形」解 8 参数单应（返回 u,v → x,y 的系数）。
///
/// # Errors
///
/// 四边形退化（线性方程组奇异）时返回 [`CaptureError::ImageOp`]。
pub fn homography(
    quad: &Quad,
    output_width: u32,
    output_height: u32,
) -> Result<[f64; 8], CaptureError> {
    let width = f64::from(output_width.max(1));
    let height = f64::from(output_height.max(1));
    let destination = [
        Point::new(0.0, 0.0),
        Point::new(width, 0.0),
        Point::new(width, height),
        Point::new(0.0, height),
    ];
    let mut matrix = [[0.0_f64; 9]; 8];
    for index in 0..4 {
        let u = destination[index].x;
        let v = destination[index].y;
        let x = quad.points[index].x;
        let y = quad.points[index].y;
        matrix[index * 2] = [u, v, 1.0, 0.0, 0.0, 0.0, -u * x, -v * x, x];
        matrix[index * 2 + 1] = [0.0, 0.0, 0.0, u, v, 1.0, -u * y, -v * y, y];
    }
    solve_linear_system(matrix)
}

fn solve_linear_system(mut matrix: [[f64; 9]; 8]) -> Result<[f64; 8], CaptureError> {
    const SIZE: usize = 8;
    for column in 0..SIZE {
        // 选主元
        let mut pivot = column;
        for row in column + 1..SIZE {
            if matrix[row][column].abs() > matrix[pivot][column].abs() {
                pivot = row;
            }
        }
        if matrix[pivot][column].abs() < 1e-12 {
            return Err(CaptureError::ImageOp(
                "单应矩阵求解失败：四边形退化".to_owned(),
            ));
        }
        matrix.swap(column, pivot);
        let divisor = matrix[column][column];
        for entry in column..=SIZE {
            matrix[column][entry] /= divisor;
        }
        for row in 0..SIZE {
            if row == column {
                continue;
            }
            let factor = matrix[row][column];
            if factor == 0.0 {
                continue;
            }
            for entry in column..=SIZE {
                matrix[row][entry] -= factor * matrix[column][entry];
            }
        }
    }
    let mut solution = [0.0_f64; 8];
    for (index, value) in solution.iter_mut().enumerate() {
        *value = matrix[index][SIZE];
    }
    Ok(solution)
}

/// 把原图四边形区域矫正为 `output_width × output_height` 的矩形（双线性采样）。
///
/// # Errors
///
/// 四边形退化或输出尺寸为 0 时返回 [`CaptureError::ImageOp`]。
pub fn warp(
    image: &RgbImage,
    quad: &Quad,
    output_width: u32,
    output_height: u32,
) -> Result<RgbImage, CaptureError> {
    if output_width == 0 || output_height == 0 {
        return Err(CaptureError::ImageOp("矫正输出尺寸为 0".to_owned()));
    }
    let coefficients = homography(quad, output_width, output_height)?;
    let mut out = RgbImage::new(output_width, output_height);
    for (x, y, pixel) in out.enumerate_pixels_mut() {
        let u = f64::from(x);
        let v = f64::from(y);
        let denominator = coefficients[6] * u + coefficients[7] * v + 1.0;
        if denominator.abs() < 1e-12 {
            *pixel = image::Rgb([255, 255, 255]);
            continue;
        }
        let source_x = (coefficients[0] * u + coefficients[1] * v + coefficients[2]) / denominator;
        let source_y = (coefficients[3] * u + coefficients[4] * v + coefficients[5]) / denominator;
        *pixel = sample_bilinear(image, source_x, source_y);
    }
    Ok(out)
}

/// 双线性采样（越界夹到边缘）。
pub fn sample_bilinear(image: &RgbImage, x: f64, y: f64) -> image::Rgb<u8> {
    let (width, height) = image.dimensions();
    let max_x = f64::from(width.saturating_sub(1));
    let max_y = f64::from(height.saturating_sub(1));
    let cx = x.clamp(0.0, max_x);
    let cy = y.clamp(0.0, max_y);
    let x0 = cx.floor() as u32;
    let y0 = cy.floor() as u32;
    let x1 = (x0 + 1).min(width.saturating_sub(1));
    let y1 = (y0 + 1).min(height.saturating_sub(1));
    let fx = cx - f64::from(x0);
    let fy = cy - f64::from(y0);
    let mut channels = [0_u8; 3];
    for channel in 0..3 {
        let p00 = f64::from(image.get_pixel(x0, y0)[channel]);
        let p10 = f64::from(image.get_pixel(x1, y0)[channel]);
        let p01 = f64::from(image.get_pixel(x0, y1)[channel]);
        let p11 = f64::from(image.get_pixel(x1, y1)[channel]);
        let top = p00 * (1.0 - fx) + p10 * fx;
        let bottom = p01 * (1.0 - fx) + p11 * fx;
        channels[channel] = (top * (1.0 - fy) + bottom * fy).round().clamp(0.0, 255.0) as u8;
    }
    image::Rgb(channels)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn quad_geometry() {
        let quad = Quad::new([
            Point::new(10.0, 20.0),
            Point::new(110.0, 20.0),
            Point::new(110.0, 220.0),
            Point::new(10.0, 220.0),
        ]);
        assert!((quad.area() - 20_000.0).abs() < 1e-6);
        assert!((quad.aspect() - 0.5).abs() < 1e-9);
        assert!(quad.is_convex());
        assert_eq!(quad.side_lengths(), [100.0, 200.0, 100.0, 200.0]);
        let bow_tie = Quad::new([
            Point::new(0.0, 0.0),
            Point::new(10.0, 10.0),
            Point::new(10.0, 0.0),
            Point::new(0.0, 10.0),
        ]);
        assert!(!bow_tie.is_convex());
    }

    #[test]
    fn axis_aligned_quad_roundtrips_pixels() {
        // 6×6 图案，按 2 倍放大矫正后中心像素应保持。
        let mut image = RgbImage::new(6, 6);
        for (x, y, pixel) in image.enumerate_pixels_mut() {
            *pixel = image::Rgb([(x * 40) as u8, (y * 40) as u8, 7]);
        }
        let quad = Quad::new([
            Point::new(0.0, 0.0),
            Point::new(6.0, 0.0),
            Point::new(6.0, 6.0),
            Point::new(0.0, 6.0),
        ]);
        let warped = warp(&image, &quad, 12, 12).unwrap();
        assert_eq!(warped.dimensions(), (12, 12));
        // 输出坐标 (u,v) 映射到原图 (u/2, v/2)；中心 (6,6) → (3,3)。
        let center = warped.get_pixel(6, 6).0;
        assert!(center[0].abs_diff(120) <= 12, "center={center:?}");
    }

    #[test]
    fn degenerate_quad_errors() {
        let image = RgbImage::new(4, 4);
        let degenerate = Quad::new([
            Point::new(1.0, 1.0),
            Point::new(1.0, 1.0),
            Point::new(1.0, 1.0),
            Point::new(1.0, 1.0),
        ]);
        assert!(warp(&image, &degenerate, 10, 10).is_err());
        assert!(warp(&image, &degenerate, 0, 10).is_err());
    }

    #[test]
    fn bilinear_clamps_outside() {
        let image = RgbImage::from_pixel(2, 2, image::Rgb([9, 8, 7]));
        assert_eq!(sample_bilinear(&image, -5.0, 99.0).0, [9, 8, 7]);
        assert_eq!(sample_bilinear(&image, 0.5, 0.5).0, [9, 8, 7]);
    }
}
