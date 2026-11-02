//! 格式白名单与文件头校验（`SPEC-capture.md` §3.1「发现与校验」、§3.2 格式表）。
//!
//! 顺序固定：大小上限 → 扩展名白名单 → 文件头字节 → 解码（`Never`：仅凭扩展名解码）。

use crate::error::CaptureError;
use crate::settings::ImportSettings;

/// 支持的媒体种类（DOCX 参考答案后续里程碑加入，当前显式拒绝）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaKind {
    /// JPEG/JPG。
    Jpeg,
    /// PNG。
    Png,
    /// TIFF（含多页时仅取首帧并告警）。
    Tiff,
    /// PDF（逐页渲染）。
    Pdf,
    /// HEIC/HEIF：需要系统图像接口，当前构建不可用。
    Heic,
}

impl MediaKind {
    /// 媒体类型（写入资产库时记录）。
    pub const fn media_type(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Tiff => "image/tiff",
            Self::Pdf => "application/pdf",
            Self::Heic => "image/heic",
        }
    }

    /// 扩展名白名单（小写、不含点）。
    pub fn from_extension(extension: &str) -> Option<Self> {
        match extension {
            "jpg" | "jpeg" => Some(Self::Jpeg),
            "png" => Some(Self::Png),
            "tif" | "tiff" => Some(Self::Tiff),
            "pdf" => Some(Self::Pdf),
            "heic" | "heif" => Some(Self::Heic),
            _ => None,
        }
    }
}

/// 文件头指纹（错误文案用，不回显文件内容）。
fn magic_hex(bytes: &[u8]) -> String {
    let head = &bytes[..bytes.len().min(4)];
    head.iter().map(|b| format!("{b:02x}")).collect()
}

/// 由文件头字节判断真实格式（不信任扩展名）。
fn sniff(bytes: &[u8]) -> Option<MediaKind> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(MediaKind::Jpeg);
    }
    if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some(MediaKind::Png);
    }
    // TIFF little/big endian，以及 BigTIFF（43）。
    if bytes.starts_with(&[0x49, 0x49, 0x2A, 0x00])
        || bytes.starts_with(&[0x4D, 0x4D, 0x00, 0x2A])
        || bytes.starts_with(&[0x49, 0x49, 0x2B, 0x00])
        || bytes.starts_with(&[0x4D, 0x4D, 0x00, 0x2B])
    {
        return Some(MediaKind::Tiff);
    }
    // PDF：允许少量前置垃圾（规范 §3.2 只要求可打开）。
    let window = &bytes[..bytes.len().min(1024)];
    if window.windows(5).any(|w| w == b"%PDF-") {
        return Some(MediaKind::Pdf);
    }
    // HEIF 家族：box ftyp 的 major brand。
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        let brand = &bytes[8..12];
        let known: [&[u8; 4]; 11] = [
            b"heic", b"heix", b"hevc", b"hevx", b"heim", b"heis", b"hevm", b"hevs", b"mif1",
            b"msf1", b"avif",
        ];
        if known.iter().any(|k| *k == brand) {
            return Some(MediaKind::Heic);
        }
    }
    None
}

/// 校验文件：大小上限 → 扩展名白名单 → 文件头一致性。
///
/// # Errors
///
/// - [`CaptureError::FileTooLarge`]：超过 `import.max_file_mb`
/// - [`CaptureError::UnsupportedFormat`]：扩展名不在白名单，或文件头与扩展名不符
/// - [`CaptureError::HeicUnsupported`]：HEIC/HEIF（显式不支持，不静默跳过）
pub fn probe(
    file_name: &str,
    bytes: &[u8],
    settings: &ImportSettings,
) -> Result<MediaKind, CaptureError> {
    let limit = u64::from(settings.max_file_mb) * 1024 * 1024;
    if bytes.len() as u64 > limit {
        return Err(CaptureError::FileTooLarge {
            bytes: bytes.len() as u64,
            limit_mb: settings.max_file_mb,
        });
    }
    let extension = file_name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    let declared = MediaKind::from_extension(&extension);
    let actual = sniff(bytes);
    match (declared, actual) {
        (Some(declared), Some(actual)) if declared == actual => match actual {
            MediaKind::Heic => Err(CaptureError::HeicUnsupported),
            other => Ok(other),
        },
        (Some(MediaKind::Heic), None) => Err(CaptureError::UnsupportedFormat {
            extension,
            magic: magic_hex(bytes),
        }),
        (Some(_) | None, _) => Err(CaptureError::UnsupportedFormat {
            extension,
            magic: magic_hex(bytes),
        }),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn jpeg() -> Vec<u8> {
        vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]
    }

    #[test]
    fn accepts_matching_extension_and_magic() {
        let settings = ImportSettings::default();
        assert_eq!(probe("a.jpg", &jpeg(), &settings).unwrap(), MediaKind::Jpeg);
        assert_eq!(
            probe(
                "a.PNG",
                &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A],
                &settings
            )
            .unwrap(),
            MediaKind::Png
        );
        let pdf = b"%PDF-1.7\n";
        assert_eq!(probe("a.pdf", pdf, &settings).unwrap(), MediaKind::Pdf);
    }

    #[test]
    fn rejects_disguised_extension() {
        let settings = ImportSettings::default();
        let err = probe("a.jpg", b"%PDF-1.7", &settings).unwrap_err();
        assert_eq!(err.code(), "unsupported_format");
        let err = probe("a.pdf", &jpeg(), &settings).unwrap_err();
        assert_eq!(err.code(), "unsupported_format");
    }

    #[test]
    fn rejects_unknown_extension_even_with_known_magic() {
        let settings = ImportSettings::default();
        let err = probe("a.docx", &jpeg(), &settings).unwrap_err();
        assert_eq!(err.code(), "unsupported_format");
    }

    #[test]
    fn rejects_heic_explicitly() {
        let settings = ImportSettings::default();
        let mut bytes = vec![0, 0, 0, 0x18];
        bytes.extend_from_slice(b"ftypheic");
        bytes.extend_from_slice(&[0u8; 16]);
        let err = probe("IMG_0001.HEIC", &bytes, &settings).unwrap_err();
        assert_eq!(err.code(), "heic_unsupported");
        assert!(err.to_string().contains("转码"));
    }

    #[test]
    fn enforces_size_limit() {
        let mut settings = ImportSettings::default();
        settings.max_file_mb = 1;
        let big = vec![0u8; 1024 * 1024 + 1];
        let err = probe("a.png", &big, &settings).unwrap_err();
        assert_eq!(err.code(), "file_too_large");
    }

    #[test]
    fn extension_whitelist_matches_spec() {
        for ext in ["jpg", "jpeg", "png", "tif", "tiff", "pdf", "heic", "heif"] {
            assert!(MediaKind::from_extension(ext).is_some(), "{ext}");
        }
        for ext in ["docx", "doc", "gif", "webp", "bmp", "", "exe"] {
            assert!(MediaKind::from_extension(ext).is_none(), "{ext}");
        }
    }
}
