//! 批次导入驱动（`SPEC-capture.md` §2 契约、§3.4 去重、§3.5 状态机、§4 部分成功）。
//!
//! 职责：批次上限校验 → 逐文件读字节 → 批内去重 → 调用 [`Pipeline`] → 建 `Submission`
//! 或参考答案资产 → 汇总 [`ImportReport`] → 送事件。**文件级失败不拖垮批次**。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use eg_domain::{
    AssetId, ClassroomId, ContentHash, Page, PaperId, Sha256Hex, Stage, Submission, SubmissionId,
    SubmissionStatus, Timestamp, Ulid,
};

use crate::dedup::{self, BatchDedupe, KnownSubmissions};
use crate::error::CaptureError;
use crate::event::{ImportEvent, ImportEventSink, SourceFingerprint};
use crate::group;
use crate::pipeline::{CancelCheck, ClockPort, IdPort, Pipeline, PreparedFile, PreparedKeyAsset};
use crate::report::{
    DuplicateCategory, DuplicateHit, FileFailure, GroupSummary, GroupWarning, GroupWarningCode,
    ImportKind, ImportReport, KeyAssetSummary, RejectedPage, SubmissionSummary,
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

    /// 来源目录键（路径输入的父目录哈希；内存输入为 `None`）。
    ///
    /// 仅用于多页归组判别「不同目录同名」，不含路径原文、不落报告。
    pub fn directory_key(&self) -> Option<String> {
        match &self.source {
            InputSource::Bytes(_) => None,
            InputSource::Path(path) => path
                .parent()
                .map(|parent| Sha256Hex::of(parent.to_string_lossy().as_bytes()).to_hex()),
        }
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

/// 已读入并完成批内去重的待处理文件。
struct ReadyFile {
    file_name: String,
    source_stem: String,
    directory_key: Option<String>,
    bytes: Vec<u8>,
    fingerprint: SourceFingerprint,
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
        let mut first_group_by_content: HashMap<ContentHash, (Sha256Hex, bool)> = HashMap::new();
        let mut ready: Vec<ReadyFile> = Vec::new();
        let mut completed: usize = 0;

        // 阶段 1：读字节 + 批内去重（同字节只处理一次，保持既有语义与事件顺序）。
        for file in &request.files {
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
                    completed += 1;
                    self.progress(&mut report, completed, files_total);
                    continue;
                }
            };
            self.emit(ImportEvent::FileStarted {
                batch_id: request.batch_id,
                fingerprint: fingerprint.clone(),
                byte_len: bytes.len() as u64,
            });
            let content_hash = ContentHash::of(&bytes);
            let source_stem = file.source_stem();
            let group_key = group::stem_group_key(&source_stem, file.directory_key().as_deref());
            let has_page_suffix = group::page_suffix(&source_stem).1.is_some();
            if dedupe.hit(content_hash).is_some() {
                // 同批重复：只处理一次；若两个**带页码**的文件属于不同组，显式告知教师
                // （无页码的重复文件无法判定归属，例如同一页的副本，不告警）。
                if let Some((first_group, first_had_suffix)) =
                    first_group_by_content.get(&content_hash)
                    && first_had_suffix == &has_page_suffix
                    && has_page_suffix
                    && *first_group != group_key
                {
                    report.group_warnings.push(GroupWarning {
                        code: GroupWarningCode::CrossGroupDuplicateBytes,
                        group_key: Some(group_key),
                        fingerprint: Some(fingerprint.clone()),
                        detail:
                            "不同组出现逐字节相同的带页码文件：按批内去重保留首次，后者未建页；\
                                 若是不同学生的相同页请检查来源"
                                .to_owned(),
                    });
                }
                report.duplicates.push(DuplicateHit {
                    fingerprint: fingerprint.clone(),
                    category: DuplicateCategory::SameBatch,
                    asset: AssetId::of(&bytes),
                });
                report.files_ok += 1;
                self.emit(ImportEvent::FileFinished {
                    batch_id: request.batch_id,
                    fingerprint,
                    page_count: 0,
                    submission_id: None,
                    has_rejected: false,
                });
                completed += 1;
                self.progress(&mut report, completed, files_total);
                continue;
            }
            dedupe.record(content_hash, fingerprint.clone());
            first_group_by_content.insert(content_hash, (group_key, has_page_suffix));
            ready.push(ReadyFile {
                file_name: file.file_name.clone(),
                source_stem: file.source_stem(),
                directory_key: file.directory_key(),
                bytes,
                fingerprint,
            });
        }

        let mut submissions: Vec<Submission> = Vec::new();
        let mut key_assets: Vec<PreparedKeyAsset> = Vec::new();
        let mut batch_page_sets: Vec<dedup::SubmissionFingerprint> = Vec::new();
        if !report.cancelled {
            match request.kind {
                ImportKind::StudentWork => {
                    self.process_student_groups(
                        &request,
                        &pipeline,
                        &ready,
                        &mut completed,
                        &mut report,
                        &mut submissions,
                        &mut batch_page_sets,
                    )?;
                }
                ImportKind::KeyReferencePages => {
                    for ready_file in &ready {
                        if self.cancel.is_cancelled() {
                            report.cancelled = true;
                            break;
                        }
                        self.process_reference_file(
                            &request,
                            &pipeline,
                            ready_file,
                            &mut report,
                            &mut key_assets,
                        );
                        completed += 1;
                        self.progress(&mut report, completed, files_total);
                    }
                }
                ImportKind::KeyReferenceText => {}
            }
        }

        let end = self.clock.now();
        report.elapsed_ms = (end - start).num_milliseconds().max(0) as u64;
        let any_failure = report.files_failed > 0
            || report.pages_failed > 0
            || report.pages_rejected > 0
            || report.cancelled
            || !report.group_warnings.is_empty();
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

    #[allow(clippy::too_many_arguments)]
    fn process_student_groups(
        &self,
        request: &ImportRequest,
        pipeline: &Pipeline<'_>,
        ready: &[ReadyFile],
        completed: &mut usize,
        report: &mut ImportReport,
        submissions: &mut Vec<Submission>,
        batch_page_sets: &mut Vec<dedup::SubmissionFingerprint>,
    ) -> Result<(), CaptureError> {
        let inputs: Vec<group::GroupInput<'_>> = ready
            .iter()
            .enumerate()
            .map(|(position, file)| group::GroupInput {
                index: position,
                file_name: &file.file_name,
                source_stem: &file.source_stem,
                directory_key: file.directory_key.as_deref(),
            })
            .collect();
        let plan = group::plan(&inputs);
        for warning in &plan.warnings {
            report.group_warnings.push(GroupWarning {
                code: warning.code,
                group_key: Some(warning.group_key),
                fingerprint: None,
                detail: warning.detail.clone(),
            });
        }
        let files_total = usize::try_from(report.files_total).unwrap_or(usize::MAX);
        for planned in &plan.groups {
            if self.cancel.is_cancelled() {
                report.cancelled = true;
                return Ok(());
            }
            for code in &planned.warnings {
                report.group_warnings.push(GroupWarning {
                    code: *code,
                    group_key: Some(planned.group_key),
                    fingerprint: None,
                    detail: group_warning_detail(*code),
                });
            }
            if !planned.missing_pages.is_empty() {
                report.group_warnings.push(GroupWarning {
                    code: GroupWarningCode::MissingPageRange,
                    group_key: Some(planned.group_key),
                    fingerprint: None,
                    detail: format!(
                        "可判定页序时发现缺页：缺少第 {:?} 页",
                        planned.missing_pages
                    ),
                });
            }
            let submission_id = SubmissionId::from_ulid(self.ids.next_ulid()?);
            let mut prepared: Vec<(usize, PreparedFile)> = Vec::new();
            let mut failed_files: u32 = 0;
            for planned_file in &planned.files {
                if self.cancel.is_cancelled() {
                    report.cancelled = true;
                    return Ok(());
                }
                let ready = &ready[planned_file.index];
                match pipeline.prepare(
                    &ready.file_name,
                    &ready.bytes,
                    ImportKind::StudentWork,
                    &ready.fingerprint,
                    Some(submission_id),
                ) {
                    Ok(file) => prepared.push((planned_file.index, file)),
                    Err(error) => {
                        self.record_file_failure(report, &ready.fingerprint, &error);
                        self.emit(ImportEvent::FileFailed {
                            batch_id: request.batch_id,
                            fingerprint: ready.fingerprint.clone(),
                            code: error.code(),
                            message: error.to_string(),
                        });
                        failed_files += 1;
                        *completed += 1;
                        self.progress(report, *completed, files_total);
                    }
                }
            }
            if prepared.is_empty() {
                continue;
            }
            for (position, file) in &prepared {
                if file.source_deduped {
                    report.duplicates.push(DuplicateHit {
                        fingerprint: ready[*position].fingerprint.clone(),
                        category: DuplicateCategory::Historical,
                        asset: file.file_asset,
                    });
                }
            }

            // 合并页并按最终页序重编号（`Page.page_no` 必须 1 起连续）。
            let mut pages: Vec<Page> = Vec::new();
            let mut rejected: Vec<(u32, Vec<&'static str>)> = Vec::new();
            let mut page_failures: u32 = 0;
            for (_, file) in &prepared {
                for prepared_page in &file.pages {
                    let mut page = prepared_page.page.clone();
                    let page_no = u32::try_from(pages.len()).unwrap_or(u32::MAX) + 1;
                    page.page_no = page_no;
                    page.submission_id = submission_id;
                    if !prepared_page.reasons.is_empty() {
                        rejected.push((
                            page_no,
                            prepared_page
                                .reasons
                                .iter()
                                .map(|reason| reason.code())
                                .collect(),
                        ));
                    }
                    pages.push(page);
                }
                page_failures += u32::try_from(file.page_failures.len()).unwrap_or(0);
            }

            let page_assets: Vec<AssetId> = pages.iter().map(|page| page.asset).collect();
            let known = {
                let mut known = self.known.page_sets();
                known.extend_from_slice(batch_page_sets);
                known
            };
            if let Some((_other, _overlap)) = dedup::suspicious_submission(&page_assets, &known) {
                report.duplicates.push(DuplicateHit {
                    fingerprint: ready[prepared[0].0].fingerprint.clone(),
                    category: DuplicateCategory::SuspectedSubmission,
                    asset: page_assets
                        .first()
                        .copied()
                        .unwrap_or(prepared[0].1.file_asset),
                });
                if matches!(
                    request.options.duplicate_policy,
                    DuplicatePolicy::Ask | DuplicatePolicy::Skip
                ) {
                    report.files_ok += u32::try_from(prepared.len()).unwrap_or(u32::MAX);
                    for (position, _file) in &prepared {
                        self.emit(ImportEvent::FileFinished {
                            batch_id: request.batch_id,
                            fingerprint: ready[*position].fingerprint.clone(),
                            page_count: 0,
                            submission_id: None,
                            has_rejected: false,
                        });
                        *completed += 1;
                        self.progress(report, *completed, files_total);
                    }
                    continue;
                }
            }

            let missing = planned.missing_pages.clone();
            let status = if !rejected.is_empty()
                || page_failures > 0
                || failed_files > 0
                || !missing.is_empty()
            {
                SubmissionStatus::Failed {
                    stage: Stage::Capture,
                    message: group_failure_message(
                        &missing,
                        &rejected,
                        page_failures,
                        failed_files,
                    ),
                }
            } else {
                SubmissionStatus::Imported
            };
            let submission = Submission {
                id: submission_id,
                paper_id: request.paper_id,
                classroom_id: request.classroom_id,
                student_id: None,
                source_file_name: prepared[0].1.file_name.clone(),
                source_stem: planned.base_stem.clone(),
                pages: pages.clone(),
                status: status.clone(),
                created_at: self.clock.now(),
            };
            submission
                .validate()
                .map_err(|error| CaptureError::Internal(format!("提交校验失败：{error}")))?;

            report.pages_total += u32::try_from(pages.len()).unwrap_or(u32::MAX);
            report.pages_failed += page_failures;
            for page in &pages {
                match page.quality.verdict {
                    eg_domain::QualityVerdict::Good => report.pages_good += 1,
                    eg_domain::QualityVerdict::Acceptable => report.pages_acceptable += 1,
                    eg_domain::QualityVerdict::Rejected { .. } => {}
                }
            }
            for (page_no, reasons) in &rejected {
                report.pages_rejected += 1;
                report.rejected_pages.push(RejectedPage {
                    submission_id: Some(submission_id),
                    page_no: *page_no,
                    reasons: reasons.clone(),
                });
            }
            let page_order: Vec<u32> =
                (1..=u32::try_from(pages.len()).unwrap_or(u32::MAX)).collect();
            let file_fingerprints: Vec<SourceFingerprint> = planned
                .files
                .iter()
                .map(|file| ready[file.index].fingerprint.clone())
                .collect();
            report.groups.push(GroupSummary {
                group_key: planned.group_key,
                submission_id,
                page_count: u32::try_from(pages.len()).unwrap_or(u32::MAX),
                file_count: u32::try_from(planned.files.len()).unwrap_or(u32::MAX),
                page_order: page_order.clone(),
                suffix_pages: planned.suffix_pages.clone(),
                missing_pages: missing.clone(),
                warnings: planned.warnings.clone(),
                file_fingerprints,
            });
            report.submissions.push(SubmissionSummary {
                submission_id,
                page_count: u32::try_from(pages.len()).unwrap_or(u32::MAX),
                status,
                group_key: Some(planned.group_key),
                file_count: u32::try_from(planned.files.len()).unwrap_or(u32::MAX),
                page_order,
            });
            report.files_ok += u32::try_from(prepared.len()).unwrap_or(u32::MAX);
            batch_page_sets.push(dedup::SubmissionFingerprint {
                submission_id,
                pages: page_assets,
            });
            submissions.push(submission);
            for (position, file) in &prepared {
                self.emit(ImportEvent::FileFinished {
                    batch_id: request.batch_id,
                    fingerprint: ready[*position].fingerprint.clone(),
                    page_count: u32::try_from(file.pages.len()).unwrap_or(u32::MAX),
                    submission_id: Some(submission_id.as_ulid()),
                    has_rejected: file.has_rejected() || !file.page_failures.is_empty(),
                });
                *completed += 1;
                self.progress(report, *completed, files_total);
            }
        }
        Ok(())
    }

    fn process_reference_file(
        &self,
        request: &ImportRequest,
        pipeline: &Pipeline<'_>,
        ready: &ReadyFile,
        report: &mut ImportReport,
        key_assets: &mut Vec<PreparedKeyAsset>,
    ) {
        let prepared = match pipeline.prepare(
            &ready.file_name,
            &ready.bytes,
            ImportKind::KeyReferencePages,
            &ready.fingerprint,
            None,
        ) {
            Ok(prepared) => prepared,
            Err(error) => {
                self.record_file_failure(report, &ready.fingerprint, &error);
                self.emit(ImportEvent::FileFailed {
                    batch_id: request.batch_id,
                    fingerprint: ready.fingerprint.clone(),
                    code: error.code(),
                    message: error.to_string(),
                });
                return;
            }
        };
        if prepared.source_deduped {
            report.duplicates.push(DuplicateHit {
                fingerprint: ready.fingerprint.clone(),
                category: DuplicateCategory::Historical,
                asset: prepared.file_asset,
            });
        }
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
            fingerprint: ready.fingerprint.clone(),
            page_count: u32::try_from(prepared.key_assets.len()).unwrap_or(0),
            submission_id: None,
            has_rejected: false,
        });
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
}

fn group_failure_message(
    missing: &[u32],
    rejected: &[(u32, Vec<&'static str>)],
    failed_pages: u32,
    failed_files: u32,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !missing.is_empty() {
        parts.push(format!("缺页（缺少第 {missing:?} 页）"));
    }
    if !rejected.is_empty() {
        let pages: Vec<u32> = rejected.iter().map(|(page, _)| *page).collect();
        parts.push(format!("存在被拒页面（第 {pages:?} 页，质量未通过）"));
    }
    if failed_pages > 0 {
        parts.push(format!("存在处理失败页面 {failed_pages} 页"));
    }
    if failed_files > 0 {
        parts.push(format!("存在未成功解码的文件 {failed_files} 个"));
    }
    if parts.is_empty() {
        parts.push("存在未完成页面".to_owned());
    }
    format!(
        "多页归组未完整：{}；请补齐/补拍后重新导入",
        parts.join("，")
    )
}

fn group_warning_detail(code: GroupWarningCode) -> String {
    match code {
        GroupWarningCode::SameStemDifferentDirectory => {
            "同一归一化文件名基名出现在不同目录：已分别成组，未合并".to_owned()
        }
        GroupWarningCode::PageOrderFallback => {
            "无法从页码后缀确定页序：已按文件名自然序稳定排序".to_owned()
        }
        GroupWarningCode::DuplicatePageNumber => {
            "组内页码重复：页序不可判定，已按文件名自然序稳定排序".to_owned()
        }
        GroupWarningCode::InconsistentStems => {
            "组内文件名基名拼写不一致（大小写/空格等）：已合并，请确认是同一学生".to_owned()
        }
        GroupWarningCode::MissingPageRange => "可判定页序时发现缺页".to_owned(),
        GroupWarningCode::CrossGroupDuplicateBytes => {
            "不同组出现逐字节相同文件：批内去重保留首次".to_owned()
        }
    }
}

/// 报告时间戳（由调用方时钟提供，供审计/快照使用）。
pub fn report_timestamp(clock: &dyn ClockPort) -> Timestamp {
    clock.now()
}
