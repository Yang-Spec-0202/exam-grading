//! EXIF 方向（`SPEC-capture.md` §3.1「方向」、§3.3「原图尺寸不旋转」）。
//!
//! 读取 EXIF Orientation 1–8，映射为像素重采样（含镜像），并给出
//! [`eg_domain::Rotation`]（0/90/180/270）与 [`eg_domain::ExifOrientation`]。
//! 解析失败或方向越界按「无 EXIF」处理并记录提示，绝不因此失败。

use eg_domain::{ExifOrientation, Rotation};
use image::RgbImage;

use crate::error::PageNote;

/// 方向读取结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrientationRead {
    /// 领域 EXIF 方向（0 = 无）。
    pub exif_orientation: ExifOrientation,
    /// 需要应用的顺时针旋转（0/90/180/270；镜像信息在 `exif_orientation` 里）。
    pub rotation: Rotation,
    /// 是否有镜像（EXIF 2/4/5/7）。
    pub mirrored: bool,
    /// 解析告警（EXIF 损坏/缺失）。
    pub note: Option<PageNote>,
}

impl OrientationRead {
    /// 无 EXIF 方向（按 1 处理）。
    pub const fn none() -> Self {
        Self {
            exif_orientation: ExifOrientation::NONE,
            rotation: Rotation::NONE,
            mirrored: false,
            note: Some(PageNote::ExifMissing),
        }
    }
}

/// 读取 EXIF 方向（对不支持 EXIF 的容器返回 [`OrientationRead::none`]）。
pub fn read(bytes: &[u8]) -> OrientationRead {
    let mut cursor = std::io::Cursor::new(bytes);
    let container = match exif::Reader::new().read_from_container(&mut cursor) {
        Ok(container) => container,
        Err(_) => return OrientationRead::none(),
    };
    let Some(field) = container.get_field(exif::Tag::Orientation, exif::In::PRIMARY) else {
        return OrientationRead::none();
    };
    let Some(value) = field.value.get_uint(0) else {
        return OrientationRead::none();
    };
    let clamped = u8::try_from(value.min(8)).unwrap_or(1);
    if clamped == 0 {
        return OrientationRead::none();
    }
    match ExifOrientation::new(clamped) {
        Ok(orientation) => OrientationRead {
            exif_orientation: orientation,
            rotation: rotation_for(clamped),
            mirrored: orientation.is_mirrored(),
            note: None,
        },
        Err(_) => OrientationRead::none(),
    }
}

/// EXIF 方向对应的顺时针旋转分量（镜像在 `apply` 中处理）。
pub const fn rotation_for(orientation: u8) -> Rotation {
    match orientation {
        3 => Rotation::CW_180,
        5 | 6 => Rotation::CW_90,
        7 | 8 => Rotation::CW_270,
        _ => Rotation::NONE,
    }
}

/// 按 EXIF 方向重采样（1–8；其他值等价 1）。输出尺寸随转置方向交换。
pub fn apply(image: &RgbImage, orientation: u8) -> RgbImage {
    let (width, height) = image.dimensions();
    let transpose = matches!(orientation, 5 | 6 | 7 | 8);
    let (out_width, out_height) = if transpose {
        (height, width)
    } else {
        (width, height)
    };
    let mut out = RgbImage::new(out_width, out_height);
    for (x, y, pixel) in out.enumerate_pixels_mut() {
        let (sx, sy) = match orientation {
            2 => (width - 1 - x, y),
            3 => (width - 1 - x, height - 1 - y),
            4 => (x, height - 1 - y),
            // 5 = 主对角转置：dst(x,y) = src(y,x)
            5 => (y, x),
            // 6 = 顺时针 90°：dst(x,y) = src(y, height-1-x)
            6 => (y, height - 1 - x),
            // 7 = 副对角（transverse）：dst(x,y) = src(width-1-y, height-1-x)
            7 => (width - 1 - y, height - 1 - x),
            // 8 = 逆时针 90°：dst(x,y) = src(width-1-y, x)
            8 => (width - 1 - y, x),
            _ => (x, y),
        };
        *pixel = *image.get_pixel(sx, sy);
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// 2×3 图案：左上角标记 A，其余按行递增，便于断言 8 种方向。
    fn marked() -> RgbImage {
        let mut image = RgbImage::new(2, 3);
        for (x, y, pixel) in image.enumerate_pixels_mut() {
            let value = match (x, y) {
                (0, 0) => 255,
                _ => (y * 2 + x) as u8,
            };
            *pixel = image::Rgb([value, 0, 0]);
        }
        image
    }

    fn marker_position(image: &RgbImage) -> (u32, u32) {
        image
            .enumerate_pixels()
            .find(|(_, _, pixel)| pixel[0] == 255)
            .map(|(x, y, _)| (x, y))
            .unwrap()
    }

    #[test]
    fn eight_orientations_map_to_expected_geometry() {
        let source = marked();
        // 尺寸：1/2/3/4 保持 2×3；5/6/7/8 转置为 3×2。
        for orientation in [1_u8, 2, 3, 4] {
            let out = apply(&source, orientation);
            assert_eq!(out.dimensions(), (2, 3), "orientation {orientation}");
        }
        for orientation in [5_u8, 6, 7, 8] {
            let out = apply(&source, orientation);
            assert_eq!(out.dimensions(), (3, 2), "orientation {orientation}");
        }
        // 标记点（原图 (0,0)）在各方向下的落点。
        let expected = [
            (1_u8, (0_u32, 0_u32)),
            (2, (1, 0)),
            (3, (1, 2)),
            (4, (0, 2)),
            (5, (0, 0)),
            (6, (2, 0)),
            (7, (2, 1)),
            (8, (0, 1)),
        ];
        for (orientation, position) in expected {
            assert_eq!(
                marker_position(&apply(&source, orientation)),
                position,
                "orientation {orientation}"
            );
        }
    }

    #[test]
    fn rotation_component_matches_exif_semantics() {
        assert_eq!(rotation_for(1), Rotation::NONE);
        assert_eq!(rotation_for(3), Rotation::CW_180);
        assert_eq!(rotation_for(6), Rotation::CW_90);
        assert_eq!(rotation_for(8), Rotation::CW_270);
        assert_eq!(rotation_for(5), Rotation::CW_90);
        assert_eq!(rotation_for(7), Rotation::CW_270);
    }

    #[test]
    fn missing_exif_is_not_an_error() {
        let read = read(b"not a jpeg");
        assert_eq!(read.exif_orientation, ExifOrientation::NONE);
        assert_eq!(read.rotation, Rotation::NONE);
        assert_eq!(read.note, Some(PageNote::ExifMissing));
    }

    #[test]
    fn four_rotations_return_to_original() {
        let source = marked();
        let rotated = apply(&apply(&apply(&apply(&source, 6), 6), 6), 6);
        assert_eq!(rotated.dimensions(), source.dimensions());
        for (x, y, pixel) in source.enumerate_pixels() {
            assert_eq!(rotated.get_pixel(x, y), pixel);
        }
        // 镜像两次等于原图。
        let twice = apply(&apply(&source, 2), 2);
        for (x, y, pixel) in source.enumerate_pixels() {
            assert_eq!(twice.get_pixel(x, y), pixel);
        }
    }
}
