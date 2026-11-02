//! 批次导入驱动（`SPEC-capture.md` §2 契约、§3.4 去重、§3.5 状态机、§4 部分成功）。
//!
//! 职责：批次上限校验 → 逐文件读字节 → 批内去重 → 调用 [`Pipeline`] → 建 `Submission`
//! 或参考答案资产 → 汇总 [`ImportReport`] → 送事件。**文件级失败不拖垮批次**。

use std::path::{Path, PathBuf};

use eg_domain::{
    ClassroomId, ContentHash, Page, PaperId, Stage, Submission, SubmissionId, SubmissionStatus,
    Timestamp, Ulid,
};

use crate::dedup::{self, BatchDedupe, KnownSubmissions};
use crate::error::CaptureError;
use crate::event::{ImportEvent, ImportEventSink, SourceFingerprint};
use crate::pipeline::{CancelCheck, ClockPort, IdPort, Pipeline, PreparedFile, PreparedKeyAsset};
use crate::report::{
    DuplicateCategory, DuplicateHit, FileFailure, ImportKind, ImportReport, KeyAssetSummary,
    RejectedPage, SubmissionSummary,
};
use crate::settings::ImportSettings;
use crate::sink::AssetSink;

/// 文件字节来源。
#[derive(Debug, Clone)]
pub enum InputSource {
    /// 已在内存中的字节（拖拽/测试/进程内导入）。
    Bytes(Vec<u8>),
    /// 文件路径（生产路径；只在此处读盘，读到的字节走 [`AssetSink`] 落盘）。
    Path(PathBuf),
}

/// 一个待导入文件（文件名是身份第一来源，**不改写、不规范化**）。
#[derive(Debug, Clone)]
pub struct FileInput {
    /// 原始文件名（含扩展名，逐字保留）。
    pub file_name: String,
    /// 字节来源。
    pub source: InputSource,
}

impl FileInput {
    /// 内存字节输入（测试/拖拽）。
    pub fn from_bytes(file_name: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            file_name: file_name.into(),
            source: InputSource::Bytes(bytes.into()),
        }
    }

    /// 文件路径输入（文件名取路径最后一段，保持原样）。
    pub fn from_path(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        Self {
            file_name,
            source: InputSource::Path(path),
        }
    }

    /// 去扩展名文件名（无扩展名时等于原文件名）。
    pub fn source_stem(&self) -> String {
        self.file_name
            .rsplit_once('.')
            .map_or_else(|| self.file_name.clone(), |(stem, _)| stem.to_owned())
    }

    fn read(&self) -> Result<Vec<u8>, CaptureError> {
        match &self.source {
            InputSource::Bytes(bytes) => Ok(bytes.clone()),
            InputSource::Path(path) => read_path(path),
        }
    }

    /// 来源指纹（路径输入按绝对路径哈希；内存输入按文件名标签）。
    pub fn fingerprint(&self) -> SourceFingerprint {
        match &self.source {
            InputSource::Bytes(_) => {
                SourceFingerprint::of_label(&self.file_name).with_extension(&self.file_name)
            }
            InputSource::Path(path) => {
                let text = path.to_string_lossy();
                SourceFingerprint::of_path(&text).with_extension(&self.file_name)
            }
        }
    }
}

fn read_path(path: &Path) -> Result<Vec<u8>, CaptureError> {
    // 脱敏：错误消息不带路径原文（来源只以指纹出现在报告/事件/日志）。
    std::fs::read(path).map_err(|e| CaptureError::Io(e.to_string()))
}

/// 重复处理策略（`SPEC-capture.md` §2「导入选项」：询问 / 跳过 / 仍创建）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DuplicatePolicy {
    /// 询问教师（默认）：疑似重复提交不自动建档。
    #[default]
    Ask,
    /// 跳过。
    Skip,
    /// 仍创建。
    CreateAnyway,
}

/// 导入选项。
#[derive(Debug, Clone, Copy, Default)]
pub struct ImportOptions {
    /// 重复处理策略。
    pub duplicate_policy: DuplicatePolicy,
}

/// 导入请求（`SPEC-capture.md` §2「导入请求」）。
#[derive(Debug, Clone)]
pub struct ImportRequest {
    /// 批次 id（调用方生成，审计与事件关联键）。
    pub batch_id: Ulid,
    /// 导入种类。
    pub kind: ImportKind,
    /// 班级 id（学生作业必填；参考答案也带上以便归档）。
    pub classroom_id: ClassroomId,
    /// 试卷 id。
    pub paper_id: PaperId,
    /// 待导入文件。
    pub files: Vec<FileInput>,
    /// 选项。
    pub options: ImportOptions,
}

/// 导入结果。
#[derive(Debug)]
pub struct ImportOutcome {
    /// 批次报告。
    pub report: ImportReport,
    /// 新建提交（含页面；参考答案为空）。
    pub submissions: Vec<Submission>,
    /// 参考答案资产（页面类别）。
    pub key_assets: Vec<PreparedKeyAsset>,
}

impl ImportOutcome {
    /// 是否有页面被拒收。
    pub fn has_rejected_pages(&self) -> bool {
        !self.report.rejected_pages.is_empty()
    }
}

/// 批次导入器（端口注入；`platform` 接入见 `platform_bridge`）。
pub struct Importer<'a> {
    settings: &'a ImportSettings,
    sink: &'a dyn AssetSink,
    events: &'a dyn ImportEventSink,
    clock: &'a dyn ClockPort,
    ids: &'a dyn IdPort,
    cancel: &'a dyn CancelCheck,
    known: &'a dyn KnownSubmissions,
}

impl<'a> Importer<'a> {
    /// 构造导入器。
    pub fn new(
        settings: &'a ImportSettings,
        sink: &'a dyn AssetSink,
        events: &'a dyn ImportEventSink,
        clock: &'a dyn ClockPort,
        ids: &'a dyn IdPort,
        cancel: &'a dyn CancelCheck,
        known: &'a dyn KnownSubmissions,
    ) -> Self {
        Self {
            settings,
            sink,
            events,
            clock,
            ids,
            cancel,
            known,
        }
    }

    /// 执行一个批次。
    ///
    /// # Errors
    ///
    /// 批次级错误：配置非法、文件数超上限、参考答案文本未实现。文件级失败进报告。
    pub fn run(&self, request: ImportRequest) -> Result<ImportOutcome, CaptureError> {
        self.settings.validate()?;
        let files_total = request.files.len();
        if files_total > self.settings.batch_file_limit as usize {
            return Err(CaptureError::BatchTooLarge {
                files: files_total,
                limit: self.settings.batch_file_limit,
            });
        }
        if request.kind == ImportKind::KeyReferenceText {
            return Err(CaptureError::NotYetImplemented(
                "DOCX 参考答案文本（后续里程碑）",
            ));
        }
        let options_hash = self.settings.options_hash();
        let naming_reminder = self.settings.remind_naming && files_total > 0;
        let start = self.clock.now();
        let mut report = ImportReport::empty(
            request.batch_id,
            request.kind,
            &options_hash,
            u32::try_from(files_total).unwrap_or(u32::MAX),
        );
        report.naming_reminder_shown = naming_reminder;
        self.emit(ImportEvent::BatchStarted {
            batch_id: request.batch_id,
            kind: request.kind,
            total_files: report.files_total,
            naming_reminder,
            options_hash: options_hash.clone(),
        });

        let pipeline = Pipeline::new(
            request.batch_id,
            self.settings,
            self.sink,
            self.events,
            self.ids,
            self.cancel,
        );
        let mut dedupe = BatchDedupe::new();
        let mut submissions: Vec<Submission> = Vec::new();
        let mut key_assets: Vec<PreparedKeyAsset> = Vec::new();
        let mut batch_page_sets: Vec<dedup::SubmissionFingerprint> = Vec::new();

        for (index, file) in request.files.iter().enumerate() {
            if self.cancel.is_cancelled() {
                report.cancelled = true;
                break;
            }
            let fingerprint = file.fingerprint();
            let bytes = match file.read() {
                Ok(bytes) => bytes,
                Err(error) => {
                    self.record_file_failure(&mut report, &fingerprint, &error);
                    self.emit(ImportEvent::FileFailed {
                        batch_id: request.batch_id,
                        fingerprint,
                        code: error.code(),
                        message: error.to_string(),
                    });
                    self.progress(&mut report, index + 1, files_total);
                    continue;
                }
            };
            self.emit(ImportEvent::FileStarted {
                batch_id: request.batch_id,
                fingerprint: fingerprint.clone(),
                byte_len: bytes.len() as u64,
            });
            let content_hash = ContentHash::of(&bytes);
            if let Some(previous) = dedupe.hit(content_hash) {
                // 同批重复：只处理一次，其余只记录（不重复写资产/不重复建页）。
                report.duplicates.push(DuplicateHit {
                    fingerprint: fingerprint.clone(),
                    category: DuplicateCategory::SameBatch,
                    asset: eg_domain::AssetId::of(&bytes),
                });
                let _ = previous;
                report.files_ok += 1;
                self.emit(ImportEvent::FileFinished {
                    batch_id: request.batch_id,
                    fingerprint,
                    page_count: 0,
                    submission_id: None,
                    has_rejected: false,
                });
                self.progress(&mut report, index + 1, files_total);
                continue;
            }
            dedupe.record(content_hash, fingerprint.clone());
            let submission_id = if request.kind == ImportKind::StudentWork {
                Some(SubmissionId::from_ulid(self.ids.next_ulid()?))
            } else {
                None
            };
            let prepared = match pipeline.prepare(
                &file.file_name,
                &bytes,
                request.kind,
                &fingerprint,
                submission_id,
            ) {
                Ok(prepared) => prepared,
                Err(error) => {
                    self.record_file_failure(&mut report, &fingerprint, &error);
                    self.emit(ImportEvent::FileFailed {
                        batch_id: request.batch_id,
                        fingerprint,
                        code: error.code(),
                        message: error.to_string(),
                    });
                    self.progress(&mut report, index + 1, files_total);
                    continue;
                }
            };
            if prepared.source_deduped {
                report.duplicates.push(DuplicateHit {
                    fingerprint: fingerprint.clone(),
                    category: DuplicateCategory::Historical,
                    asset: prepared.file_asset,
                });
            }
            match request.kind {
                ImportKind::StudentWork => {
                    let Some(submission_id) = submission_id else {
                        continue;
                    };
                    if self.is_suspected_duplicate(
                        &prepared,
                        &batch_page_sets,
                        &request.options,
                        &mut report,
                        &fingerprint,
                    ) {
                        report.files_ok += 1;
                        self.emit(ImportEvent::FileFinished {
                            batch_id: request.batch_id,
                            fingerprint,
                            page_count: 0,
                            submission_id: None,
                            has_rejected: false,
                        });
                        self.progress(&mut report, index + 1, files_total);
                        continue;
                    }
                    let (submission, summary) = self.build_submission(
                        &request,
                        submission_id,
                        &prepared,
                        &fingerprint,
                        &mut report,
                    )?;
                    batch_page_sets.push(dedup::SubmissionFingerprint {
                        submission_id: submission.id,
                        pages: submission.pages.iter().map(|page| page.asset).collect(),
                    });
                    report.files_ok += 1;
                    report.submissions.push(summary);
                    self.emit(ImportEvent::FileFinished {
                        batch_id: request.batch_id,
                        fingerprint,
                        page_count: u32::try_from(submission.pages.len()).unwrap_or(u32::MAX),
                        submission_id: Some(submission.id.as_ulid()),
                        has_rejected: prepared.has_rejected(),
                    });
                    submissions.push(submission);
                }
                ImportKind::KeyReferencePages => {
                    for asset in &prepared.key_assets {
                        report.key_assets.push(KeyAssetSummary {
                            page_no: asset.page_no,
                            asset: Some(asset.source_asset),
                            kind: "page",
                            rotation_applied: asset.rotation_applied.degrees(),
                        });
                    }
                    report.files_ok += 1;
                    report.pages_total += u32::try_from(prepared.key_assets.len()).unwrap_or(0);
                    key_assets.extend(prepared.key_assets.clone());
                    self.emit(ImportEvent::FileFinished {
                        batch_id: request.batch_id,
                        fingerprint,
                        page_count: u32::try_from(prepared.key_assets.len()).unwrap_or(0),
                        submission_id: None,
                        has_rejected: false,
                    });
                }
                ImportKind::KeyReferenceText => {}
            }
            self.progress(&mut report, index + 1, files_total);
        }

        let end = self.clock.now();
        report.elapsed_ms = (end - start).num_milliseconds().max(0) as u64;
        let any_failure = report.files_failed > 0
            || report.pages_failed > 0
            || report.pages_rejected > 0
            || report.cancelled;
        let any_success = report.files_ok > 0;
        report.partial_success = any_failure && any_success;
        self.emit(ImportEvent::BatchFinished {
            batch_id: request.batch_id,
            partial_success: report.partial_success,
            cancelled: report.cancelled,
        });
        Ok(ImportOutcome {
            report,
            submissions,
            key_assets,
        })
    }

    fn emit(&self, event: ImportEvent) {
        self.events.emit(event);
    }

    fn record_file_failure(
        &self,
        report: &mut ImportReport,
        fingerprint: &SourceFingerprint,
        error: &CaptureError,
    ) {
        report.files_failed += 1;
        report.file_failures.push(FileFailure {
            fingerprint: fingerprint.clone(),
            code: error.code(),
            message: error.to_string(),
        });
    }

    fn progress(&self, report: &mut ImportReport, completed: usize, total: usize) {
        self.emit(ImportEvent::Progress {
            batch_id: report.batch_id,
            completed: u32::try_from(completed).unwrap_or(u32::MAX),
            total: u32::try_from(total).unwrap_or(u32::MAX),
        });
    }

    fn is_suspected_duplicate(
        &self,
        prepared: &PreparedFile,
        batch_page_sets: &[dedup::SubmissionFingerprint],
        options: &ImportOptions,
        report: &mut ImportReport,
        fingerprint: &SourceFingerprint,
    ) -> bool {
        let page_assets: Vec<eg_domain::AssetId> =
            prepared.pages.iter().map(|page| page.page.asset).collect();
        let mut known = self.known.page_sets();
        known.extend_from_slice(batch_page_sets);
        let Some((submission_id, overlap)) = dedup::suspicious_submission(&page_assets, &known)
        else {
            return false;
        };
        // 记录疑似重复（提示开关关闭时只记录不弹窗；不静默建档）。
        let _ = submission_id;
        let _ = overlap;
        report.duplicates.push(DuplicateHit {
            fingerprint: fingerprint.clone(),
            category: DuplicateCategory::SuspectedSubmission,
            asset: page_assets.first().copied().unwrap_or(prepared.file_asset),
        });
        matches!(
            options.duplicate_policy,
            DuplicatePolicy::Ask | DuplicatePolicy::Skip
        )
    }

    fn build_submission(
        &self,
        request: &ImportRequest,
        submission_id: SubmissionId,
        prepared: &PreparedFile,
        _fingerprint: &SourceFingerprint,
        report: &mut ImportReport,
    ) -> Result<(Submission, SubmissionSummary), CaptureError> {
        let pages: Vec<Page> = prepared
            .pages
            .iter()
            .map(|page| page.page.clone())
            .collect();
        let rejected = prepared.has_rejected() || !prepared.page_failures.is_empty();
        let status = if rejected {
            let message = rejection_message(prepared);
            SubmissionStatus::Failed {
                stage: Stage::Capture,
                message,
            }
        } else {
            SubmissionStatus::Imported
        };
        let submission = Submission {
            id: submission_id,
            paper_id: request.paper_id,
            classroom_id: request.classroom_id,
            student_id: None,
            source_file_name: prepared.file_name.clone(),
            source_stem: prepared.source_stem.clone(),
            pages,
            status,
            created_at: self.clock.now(),
        };
        submission
            .validate()
            .map_err(|e| CaptureError::Internal(format!("提交校验失败：{e}")))?;
        // 统计页面结论。
        for page in &prepared.pages {
            report.pages_total += 1;
            match page.page.quality.verdict {
                eg_domain::QualityVerdict::Good => report.pages_good += 1,
                eg_domain::QualityVerdict::Acceptable => report.pages_acceptable += 1,
                eg_domain::QualityVerdict::Rejected { .. } => {
                    report.pages_rejected += 1;
                    report.rejected_pages.push(RejectedPage {
                        submission_id: Some(submission_id),
                        page_no: page.page.page_no,
                        reasons: page.reasons.iter().map(|reason| reason.code()).collect(),
                    });
                }
            }
        }
        report.pages_failed += u32::try_from(prepared.page_failures.len()).unwrap_or(0);
        let summary = SubmissionSummary {
            submission_id,
            page_count: u32::try_from(submission.pages.len()).unwrap_or(u32::MAX),
            status: submission.status.clone(),
        };
        Ok((submission, summary))
    }
}

fn rejection_message(prepared: &PreparedFile) -> String {
    if prepared.page_failures.is_empty() {
        "存在被拒页面（质量未通过），请补拍或重新导入该页".to_owned()
    } else {
        "存在处理失败的页面，请重新导入".to_owned()
    }
}

/// 报告时间戳（由调用方时钟提供，供审计/快照使用）。
pub fn report_timestamp(clock: &dyn ClockPort) -> Timestamp {
    clock.now()
}
