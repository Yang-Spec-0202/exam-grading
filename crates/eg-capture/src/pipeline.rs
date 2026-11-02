//! 单文件流水线（`SPEC-capture.md` §3.1）：
//! 校验 → 入库 → 解码 → 方向 → 轮廓/矫正 → 派生 → 质量门控 → 产出页面。
//!
//! 端口全部注入（[`AssetSink`] / [`ImportEventSink`] / [`IdPort`] / [`CancelCheck`]），
//! 便于测试与将来接入 `platform`（见 `platform_bridge`）。

use std::sync::atomic::{AtomicU64, Ordering};

use eg_domain::{
    AssetId, ContentHash, ExifOrientation, Page, PageId, PageQuality, QualityVerdict, Rotation,
    SubmissionId, Timestamp, Ulid,
};
use image::RgbImage;

use crate::detect;
use crate::error::{CaptureError, PageNote, RejectReason};
use crate::event::{ImportEvent, ImportEventSink, SourceFingerprint};
use crate::exif;
use crate::imageops;
use crate::media::{self, MediaKind};
use crate::pdf;
use crate::quality::{self, QualityAssessment};
use crate::report::ImportKind;
use crate::settings::ImportSettings;
use crate::sink::AssetSink;

/// 可注入时钟端口（业务代码不直接读系统时间；`platform::Clock` 的适配点）。
pub trait ClockPort: Send + Sync {
    /// 当前 UTC 时间。
    fn now(&self) -> Timestamp;
}

/// 协作式取消端口（`platform::CancelToken` 的适配点；检查点在文件开始、每页渲染/预处理前）。
pub trait CancelCheck: Send + Sync {
    /// 是否已请求取消。
    fn is_cancelled(&self) -> bool;
}

/// 永不取消（批处理/测试）。
#[derive(Debug, Default, Clone, Copy)]
pub struct NeverCancel;

impl CancelCheck for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// id 生成端口（`platform::UlidFactory` 的适配点；`domain::Ulid` 单例语义由实现方保证）。
pub trait IdPort: Send + Sync {
    /// 生成下一个 ULID（进程内单例、同毫秒单调）。
    ///
    /// # Errors
    ///
    /// 持久化/时钟异常（`platform::UlidFactory` 可能失败）返回 [`CaptureError::Internal`]。
    fn next_ulid(&self) -> Result<Ulid, CaptureError>;
}

/// 本地自增 id 源（测试与无 platform 的 CLI；**不是**生产 id 源）。
#[derive(Debug, Default)]
pub struct SeqIdSource {
    next: AtomicU64,
}

impl SeqIdSource {
    /// 从 1 开始。
    pub fn new() -> Self {
        Self::default()
    }

    /// 从指定起点开始（测试用，便于断言 id）。
    pub fn with_start(start: u64) -> Self {
        Self {
            next: AtomicU64::new(start),
        }
    }
}

impl IdPort for SeqIdSource {
    fn next_ulid(&self) -> Result<Ulid, CaptureError> {
        let value = self.next.fetch_add(1, Ordering::Relaxed);
        Ok(Ulid::from_u128(u128::from(value) + 1))
    }
}

/// 单页产出（成功入页，可能被质量门禁拒收）。
#[derive(Debug, Clone)]
pub struct PreparedPage {
    /// 领域页面（含 `quality.verdict`；被拒页仍建记录，但不进识别）。
    pub page: Page,
    /// 拒收原因（空 = 未被拒）。
    pub reasons: Vec<RejectReason>,
    /// 非拒收提示码。
    pub notes: Vec<PageNote>,
}

/// 参考答案资产（**不建模成 `Page`**；`SPEC-domain.md` §15.3）。
#[derive(Debug, Clone)]
pub struct PreparedKeyAsset {
    /// 页号（文档文本为 0；页面为 1 起）。
    pub page_no: u32,
    /// 原图/页面资产。
    pub source_asset: AssetId,
    /// 派生资产（顺序同 `Page.derived`，去重后最多 4 项）。
    pub derived: Vec<AssetId>,
    /// 页宽（原图像素，未旋转）。
    pub width: u32,
    /// 页高（原图像素，未旋转）。
    pub height: u32,
    /// 已应用的方向校正。
    pub rotation_applied: Rotation,
    /// EXIF 方向。
    pub exif_orientation: ExifOrientation,
    /// 质量。
    pub quality: PageQuality,
    /// 提示码。
    pub notes: Vec<PageNote>,
}

/// 单文件产出。
#[derive(Debug, Clone)]
pub struct PreparedFile {
    /// 原始文件资产（先落盘后提交）。
    pub file_asset: AssetId,
    /// 原始字节的内容哈希（批内去重键）。
    pub content_hash: ContentHash,
    /// 导入种类。
    pub kind: ImportKind,
    /// 原图资产是否命中既有内容（历史重复）。
    pub source_deduped: bool,
    /// 文件名（含扩展名，无损）。
    pub file_name: String,
    /// 去扩展名文件名（无损）。
    pub source_stem: String,
    /// 成功的页面（含被拒页）。
    pub pages: Vec<PreparedPage>,
    /// 页面级失败（页号, 错误）；不拖垮其余页。
    pub page_failures: Vec<(u32, CaptureError)>,
    /// 参考答案资产（`KeyReferencePages`）。
    pub key_assets: Vec<PreparedKeyAsset>,
    /// 文件级提示码（如保留原图 PDF、降采样）。
    pub notes: Vec<PageNote>,
}

impl PreparedFile {
    /// 是否有页被质量门禁拒收。
    pub fn has_rejected(&self) -> bool {
        self.pages.iter().any(|page| !page.reasons.is_empty())
    }
}

/// 页面处理中间结果。
struct ProcessedPage {
    quality: PageQuality,
    reasons: Vec<RejectReason>,
    notes: Vec<PageNote>,
    derived: Vec<AssetId>,
}

/// 单文件流水线。
pub struct Pipeline<'a> {
    batch_id: Ulid,
    settings: &'a ImportSettings,
    sink: &'a dyn AssetSink,
    events: &'a dyn ImportEventSink,
    ids: &'a dyn IdPort,
    cancel: &'a dyn CancelCheck,
}

impl<'a> Pipeline<'a> {
    /// 构造流水线（端口由调用方注入）。
    pub fn new(
        batch_id: Ulid,
        settings: &'a ImportSettings,
        sink: &'a dyn AssetSink,
        events: &'a dyn ImportEventSink,
        ids: &'a dyn IdPort,
        cancel: &'a dyn CancelCheck,
    ) -> Self {
        Self {
            batch_id,
            settings,
            sink,
            events,
            ids,
            cancel,
        }
    }

    /// 处理一个文件（学生作业或参考答案页面）。
    ///
    /// # Errors
    ///
    /// 文件级错误（格式/大小/解码/PDF 打开/页数超限/取消/资产存储）返回 `Err`，
    /// 批次其余文件不受影响；页面级失败记录在 [`PreparedFile::page_failures`]。
    pub fn prepare(
        &self,
        file_name: &str,
        bytes: &[u8],
        kind: ImportKind,
        fingerprint: &SourceFingerprint,
        submission_id: Option<SubmissionId>,
    ) -> Result<PreparedFile, CaptureError> {
        self.check_cancel()?;
        let media = media::probe(file_name, bytes, self.settings)?;
        // 规范 §3.1：先落盘（内容寻址、只追加），再解码与建档。
        let put = self.sink.put(bytes, media.media_type())?;
        let source_stem = file_name
            .rsplit_once('.')
            .map_or_else(|| file_name.to_owned(), |(stem, _)| stem.to_owned());
        let mut prepared = PreparedFile {
            file_asset: put.asset,
            content_hash: put.asset.content_hash(),
            kind,
            source_deduped: put.deduped,
            file_name: file_name.to_owned(),
            source_stem,
            pages: Vec::new(),
            page_failures: Vec::new(),
            key_assets: Vec::new(),
            notes: Vec::new(),
        };
        match media {
            MediaKind::Pdf => {
                self.prepare_pdf(bytes, kind, fingerprint, submission_id, &mut prepared)?
            }
            MediaKind::Jpeg | MediaKind::Png | MediaKind::Tiff => {
                self.prepare_image(
                    bytes,
                    media,
                    kind,
                    fingerprint,
                    submission_id,
                    &mut prepared,
                )?;
            }
            MediaKind::Heic => return Err(CaptureError::HeicUnsupported),
        }
        Ok(prepared)
    }

    fn prepare_image(
        &self,
        bytes: &[u8],
        media: MediaKind,
        kind: ImportKind,
        fingerprint: &SourceFingerprint,
        submission_id: Option<SubmissionId>,
        prepared: &mut PreparedFile,
    ) -> Result<(), CaptureError> {
        self.check_cancel()?;
        let decoded = imageops::decode(bytes, media)?;
        let (width, height) = decoded.dimensions();
        let orientation = exif::read(bytes);
        let mut notes: Vec<PageNote> = Vec::new();
        if let Some(note) = orientation.note {
            notes.push(note);
        }
        if media == MediaKind::Tiff {
            // image 0.25 的 TIFF 解码只取首帧：多页 TIFF 显式告警（不静默）。
            notes.push(PageNote::MultiPageTiffFirstFrameOnly);
        }
        let rotated = exif::apply(&decoded, orientation.exif_orientation.value());
        drop(decoded);
        self.emit(ImportEvent::PageDecoded {
            batch_id: self.batch_id,
            fingerprint: fingerprint.clone(),
            page_no: 1,
            width,
            height,
        });
        let processed = self.process_page(1, rotated, prepared.file_asset, notes, fingerprint)?;
        match kind {
            ImportKind::StudentWork => {
                let submission_id = submission_id
                    .ok_or_else(|| CaptureError::Internal("学生作业缺少目标提交 id".to_owned()))?;
                prepared.pages.push(self.build_page(
                    submission_id,
                    1,
                    prepared.file_asset,
                    processed,
                    width,
                    height,
                    orientation.rotation,
                    orientation.exif_orientation,
                )?);
            }
            ImportKind::KeyReferencePages => {
                prepared.key_assets.push(self.build_key_asset(
                    1,
                    prepared.file_asset,
                    processed,
                    width,
                    height,
                    orientation.rotation,
                    orientation.exif_orientation,
                ));
            }
            ImportKind::KeyReferenceText => {
                return Err(CaptureError::NotYetImplemented(
                    "DOCX 参考答案文本（后续里程碑）",
                ));
            }
        }
        Ok(())
    }

    fn prepare_pdf(
        &self,
        bytes: &[u8],
        kind: ImportKind,
        fingerprint: &SourceFingerprint,
        submission_id: Option<SubmissionId>,
        prepared: &mut PreparedFile,
    ) -> Result<(), CaptureError> {
        self.check_cancel()?;
        let rendered = pdf::render_pages(bytes, self.settings)?;
        for result in rendered {
            self.check_cancel()?;
            match result {
                Err(error) => {
                    let page_no = match &error {
                        CaptureError::PdfRender { page, .. } => *page,
                        _ => 0,
                    };
                    self.emit(ImportEvent::PageFailed {
                        batch_id: self.batch_id,
                        fingerprint: fingerprint.clone(),
                        page_no,
                        code: error.code(),
                    });
                    prepared.page_failures.push((page_no, error));
                }
                Ok(page) => {
                    let page_no = page.page_no;
                    let (width, height) = page.image.dimensions();
                    let png = imageops::encode_png(&page.image)?;
                    let page_asset = self.sink.put(&png, "image/png")?.asset;
                    self.emit(ImportEvent::PageDecoded {
                        batch_id: self.batch_id,
                        fingerprint: fingerprint.clone(),
                        page_no,
                        width,
                        height,
                    });
                    let mut notes = pdf::render_notes(&page);
                    if page.downsampled && !notes.contains(&PageNote::Downsampled) {
                        notes.push(PageNote::Downsampled);
                    }
                    let processed =
                        self.process_page(page_no, page.image, page_asset, notes, fingerprint)?;
                    match kind {
                        ImportKind::StudentWork => {
                            let submission_id = submission_id.ok_or_else(|| {
                                CaptureError::Internal("学生作业缺少目标提交 id".to_owned())
                            })?;
                            prepared.pages.push(self.build_page(
                                submission_id,
                                page_no,
                                page_asset,
                                processed,
                                width,
                                height,
                                Rotation::NONE,
                                ExifOrientation::NONE,
                            )?);
                        }
                        ImportKind::KeyReferencePages => {
                            prepared.key_assets.push(self.build_key_asset(
                                page_no,
                                page_asset,
                                processed,
                                width,
                                height,
                                Rotation::NONE,
                                ExifOrientation::NONE,
                            ));
                        }
                        ImportKind::KeyReferenceText => {
                            return Err(CaptureError::NotYetImplemented(
                                "DOCX 参考答案文本（后续里程碑）",
                            ));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn process_page(
        &self,
        page_no: u32,
        rotated: RgbImage,
        page_asset: AssetId,
        mut notes: Vec<PageNote>,
        fingerprint: &SourceFingerprint,
    ) -> Result<ProcessedPage, CaptureError> {
        let rectified = detect::rectify_page(&rotated);
        notes.extend(rectified.notes.iter().copied());
        let assessment: QualityAssessment = quality::assess(&rectified.image, self.settings);
        // 派生资产顺序固定：方向校正图、透视矫正图、二值图、缩略图（去重后保留）。
        // 回退时 rectified == rotated：复用已编码资产，避免重复编码大图。
        let rotated_asset = self
            .sink
            .put(&imageops::encode_png(&rotated)?, "image/png")?
            .asset;
        let rectified_asset = if rectified.fallback {
            rotated_asset
        } else {
            self.sink
                .put(&imageops::encode_png(&rectified.image)?, "image/png")?
                .asset
        };
        let gray = imageops::to_gray(&rectified.image);
        let binary_asset = self
            .sink
            .put(
                &imageops::encode_png_gray(&imageops::binarize(&gray))?,
                "image/png",
            )?
            .asset;
        let thumbnail = imageops::thumbnail(&rectified.image, self.settings.thumbnail_long_edge);
        let thumbnail_asset = self
            .sink
            .put(&imageops::encode_png(&thumbnail)?, "image/png")?
            .asset;
        let mut derived = Vec::new();
        for asset in [
            rotated_asset,
            rectified_asset,
            binary_asset,
            thumbnail_asset,
        ] {
            if asset != page_asset && !derived.contains(&asset) {
                derived.push(asset);
            }
        }
        notes.sort();
        notes.dedup();
        if assessment.rejected() {
            self.emit(ImportEvent::PageRejected {
                batch_id: self.batch_id,
                fingerprint: fingerprint.clone(),
                page_no,
                reasons: assessment
                    .reasons
                    .iter()
                    .map(|reason| reason.code())
                    .collect(),
            });
        } else {
            let verdict = match assessment.verdict {
                QualityVerdict::Good => "good",
                _ => "acceptable",
            };
            self.emit(ImportEvent::PagePreprocessed {
                batch_id: self.batch_id,
                fingerprint: fingerprint.clone(),
                page_no,
                verdict,
                derived_count: u8::try_from(derived.len()).unwrap_or(u8::MAX),
            });
        }
        Ok(ProcessedPage {
            quality: assessment.page_quality(),
            reasons: assessment.reasons,
            notes,
            derived,
        })
    }

    fn build_page(
        &self,
        submission_id: SubmissionId,
        page_no: u32,
        page_asset: AssetId,
        processed: ProcessedPage,
        width: u32,
        height: u32,
        rotation: Rotation,
        exif_orientation: ExifOrientation,
    ) -> Result<PreparedPage, CaptureError> {
        let page = Page {
            id: PageId::from_ulid(self.ids.next_ulid()?),
            submission_id,
            page_no,
            asset: page_asset,
            width,
            height,
            rotation_applied: rotation,
            exif_orientation,
            quality: processed.quality,
            derived: processed.derived,
        };
        Ok(PreparedPage {
            page,
            reasons: processed.reasons,
            notes: processed.notes,
        })
    }

    fn build_key_asset(
        &self,
        page_no: u32,
        page_asset: AssetId,
        processed: ProcessedPage,
        width: u32,
        height: u32,
        rotation: Rotation,
        exif_orientation: ExifOrientation,
    ) -> PreparedKeyAsset {
        PreparedKeyAsset {
            page_no,
            source_asset: page_asset,
            derived: processed.derived,
            width,
            height,
            rotation_applied: rotation,
            exif_orientation,
            quality: processed.quality,
            notes: processed.notes,
        }
    }

    fn check_cancel(&self) -> Result<(), CaptureError> {
        if self.cancel.is_cancelled() {
            Err(CaptureError::Cancelled)
        } else {
            Ok(())
        }
    }

    fn emit(&self, event: ImportEvent) {
        self.events.emit(event);
    }
}
