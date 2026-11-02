//! 导出编排（`SPEC-report.md` §4 数据流、§5 失败、§6 可观测性、§9.9 重放）。
//!
//! 一次导出：构建行集 → 脱敏自检 → 渲染三格式（内存）→ 命名（含冲突序号）→ 原子写 →
//! 计算内容摘要 → 写审计。任一环节失败都不留文件；审计失败回滚已写文件。

use crate::atomic;
use crate::audit::{AUDIT_ACTION, AUDIT_ACTOR, AUDIT_ENTITY_TYPE, ExportAuditEvent, ExportAuditor};
use crate::csv;
use crate::error::{ReportError, Result};
use crate::html;
use crate::masking::assert_no_leak;
use crate::naming::{ExportNaming, ModeToken};
use crate::stats::Report;
use crate::table::{self, Table};
use crate::xlsx;
use eg_domain::{Sha256Hex, Timestamp};
use std::path::PathBuf;

/// 导出格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    /// XLSX（一个工作簿 8 张表）。
    Xlsx,
    /// CSV（每张表一个文件，UTF-8 BOM）。
    Csv,
    /// HTML（单文件、内联样式）。
    Html,
}

impl ExportFormat {
    /// 扩展名。
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Xlsx => "xlsx",
            Self::Csv => "csv",
            Self::Html => "html",
        }
    }

    /// 展示名（日志/审计）。
    #[must_use]
    pub const fn label(self) -> &'static str {
        self.extension()
    }

    /// 全部格式（默认导出集合）。
    #[must_use]
    pub const fn all() -> [Self; 3] {
        [Self::Xlsx, Self::Csv, Self::Html]
    }
}

/// 导出请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportRequest {
    /// 输出目录（不存在时创建）。
    pub out_dir: PathBuf,
    /// 格式集合（顺序不影响结果）。
    pub formats: Vec<ExportFormat>,
    /// 是否脱敏（姓名/学号掩码 + 文件名后缀 + 内容自检）。
    pub masked: bool,
    /// 是否允许覆盖已存在文件（默认拒绝）。
    pub allow_overwrite: bool,
    /// 批次时间（命名与元信息；由 `eg-platform` 的 `Clock` 提供）。
    pub exported_at: Timestamp,
    /// 模式 token（整卷导出默认「全部」）。
    pub mode_token: ModeToken,
    /// 草稿导出（存在待复核记录时；文件名与元信息标「草稿」）。
    pub draft: bool,
}

impl ExportRequest {
    /// 默认请求：三格式、非脱敏、不覆盖、「全部」模式。
    #[must_use]
    pub fn new(out_dir: impl Into<PathBuf>, exported_at: Timestamp) -> Self {
        Self {
            out_dir: out_dir.into(),
            formats: ExportFormat::all().to_vec(),
            masked: false,
            allow_overwrite: false,
            exported_at,
            mode_token: ModeToken::All,
            draft: false,
        }
    }
}

/// 单个导出文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedFile {
    /// 文件名（不含目录）。
    pub file_name: String,
    /// 完整路径。
    pub path: PathBuf,
    /// 格式。
    pub format: ExportFormat,
    /// 字节数。
    pub bytes: u64,
    /// 文件 SHA-256。
    pub sha256: Sha256Hex,
}

/// 导出结果（导出运行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportOutcome {
    /// 文件列表（按格式与工作表顺序）。
    pub files: Vec<ExportedFile>,
    /// 内容摘要（文件名 + 文件哈希的 canonical 哈希；重放对比用）。
    pub content_digest: Sha256Hex,
    /// 快照哈希。
    pub snapshot_hash: Sha256Hex,
    /// 是否脱敏。
    pub masked: bool,
    /// 口径版本。
    pub policy_version: u32,
    /// 命名规则版本。
    pub naming_rule_version: u32,
    /// 格式集合。
    pub formats: Vec<ExportFormat>,
    /// 行数（全部工作表合计）。
    pub row_count: usize,
}

/// 执行导出。
///
/// # Errors
///
/// 渲染、命名、写盘、脱敏自检或审计失败时返回对应 [`ReportError`]；
/// 失败时不会留下任何本次产物（审计失败会回滚已写文件）。
pub fn export(
    report: &Report,
    request: &ExportRequest,
    auditor: &dyn ExportAuditor,
) -> Result<ExportOutcome> {
    if request.formats.is_empty() {
        return Err(ReportError::Config("导出格式集合为空".to_owned()));
    }
    let mut formats: Vec<ExportFormat> = Vec::with_capacity(request.formats.len());
    for format in &request.formats {
        if !formats.contains(format) {
            formats.push(*format);
        }
    }
    let naming = ExportNaming::with_fast_label(report.settings.fast_export_label);
    let mut tables = table::build_tables(
        report,
        request.exported_at,
        request.masked,
        naming.rule_version(),
    );
    if request.draft
        && let Some(meta) = tables.first_mut()
    {
        meta.rows.push(vec![
            crate::table::Cell::Text("导出类型".to_owned()),
            crate::table::Cell::Text("草稿（存在待复核记录，未计入正式统计）".to_owned()),
        ]);
    }
    if request.masked {
        self_check(report, &tables)?;
    }

    let mut rendered: Vec<(String, Vec<u8>, ExportFormat)> = Vec::new();
    let mut taken: Vec<String> = Vec::new();
    for format in &formats {
        match format {
            ExportFormat::Xlsx => {
                let bytes = xlsx::render(&tables, &report.paper.title)?;
                let name = naming.class_file_name(
                    &report.paper.title,
                    request.mode_token,
                    request.exported_at,
                    request.masked,
                    format.extension(),
                )?;
                let name = naming.dedupe(&apply_draft(&name, request.draft), &taken);
                taken.push(name.clone());
                rendered.push((name, bytes, *format));
            }
            ExportFormat::Html => {
                let footer = table::footer_note(report);
                let bytes = html::render(&tables, &report.paper.title, &footer);
                let name = naming.class_file_name(
                    &report.paper.title,
                    request.mode_token,
                    request.exported_at,
                    request.masked,
                    format.extension(),
                )?;
                let name = naming.dedupe(&apply_draft(&name, request.draft), &taken);
                taken.push(name.clone());
                rendered.push((name, bytes, *format));
            }
            ExportFormat::Csv => {
                for table in &tables {
                    let base = naming.class_file_name(
                        &report.paper.title,
                        request.mode_token,
                        request.exported_at,
                        request.masked,
                        format.extension(),
                    )?;
                    let stem = base.trim_end_matches(".csv").to_owned();
                    let name = format!("{stem}_{}.csv", table.sheet.name());
                    let name = naming.dedupe(&apply_draft(&name, request.draft), &taken);
                    taken.push(name.clone());
                    rendered.push((name, csv::render(table), *format));
                }
            }
        }
    }

    let pending: atomic::PendingFiles = rendered
        .iter()
        .map(|(name, bytes, _)| (name.clone(), bytes.clone()))
        .collect();
    let paths = atomic::write_all(&request.out_dir, &pending, request.allow_overwrite)?;

    let mut files: Vec<ExportedFile> = Vec::with_capacity(rendered.len());
    for (index, (name, bytes, format)) in rendered.iter().enumerate() {
        let path = paths
            .get(index)
            .cloned()
            .unwrap_or_else(|| request.out_dir.join(name));
        files.push(ExportedFile {
            file_name: name.clone(),
            path,
            format: *format,
            bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            sha256: Sha256Hex::of(bytes),
        });
    }
    let content_digest = content_digest(&tables, &taken);
    let row_count = tables.iter().map(|table| table.rows.len()).sum();
    let outcome = ExportOutcome {
        files,
        content_digest,
        snapshot_hash: report.snapshot_hash,
        masked: request.masked,
        policy_version: report.settings.policy_version,
        naming_rule_version: naming.rule_version(),
        formats: formats.clone(),
        row_count,
    };

    let event = ExportAuditEvent {
        action: AUDIT_ACTION.to_owned(),
        actor: AUDIT_ACTOR.to_owned(),
        entity_type: AUDIT_ENTITY_TYPE.to_owned(),
        entity_id: format!("export-{}", &content_digest.to_hex()[..12]),
        note: audit_note(report, request, &formats, row_count),
        after_hash: Some(content_digest),
        occurred_at: request.exported_at,
        policy_version: report.settings.policy_version,
        naming_rule_version: naming.rule_version(),
        masked: request.masked,
        draft: request.draft,
        snapshot_hash: report.snapshot_hash,
        file_count: outcome.files.len(),
        row_count,
        formats: formats
            .iter()
            .map(|format| format.label().to_owned())
            .collect(),
    };
    if let Err(error) = auditor.record(&event) {
        let written: Vec<PathBuf> = outcome.files.iter().map(|file| file.path.clone()).collect();
        atomic::remove_files(&written);
        return Err(error);
    }
    Ok(outcome)
}

/// 内容摘要：对可见内容（文件名集合 + 8 张表的 canonical 文本）取 SHA-256。
///
/// 不用 XLSX 原始字节：工作簿容器会写入生成时间等非内容元数据，按字节比较会造成
/// 假不一致；可见内容相同即视为重放一致（逐文件哈希仍写入导出运行与审计）。
#[must_use]
pub(crate) fn content_digest(tables: &[Table], file_names: &[String]) -> Sha256Hex {
    let mut names: Vec<&String> = file_names.iter().collect();
    names.sort();
    let mut canonical = String::new();
    for name in names {
        canonical.push_str("file\t");
        canonical.push_str(name);
        canonical.push('\n');
    }
    for table in tables {
        canonical.push_str("sheet\t");
        canonical.push_str(table.sheet.name());
        canonical.push('\n');
        canonical.push_str(&table.visible_text());
    }
    Sha256Hex::of(canonical.as_bytes())
}

/// 重放一致性校验：快照、口径、脱敏与内容摘要必须完全一致。
///
/// # Errors
///
/// 任一维度不一致时返回 [`ReportError::ReplayMismatch`]。
pub fn verify_replay(original: &ExportOutcome, replay: &ExportOutcome) -> Result<()> {
    let mut differences: Vec<&str> = Vec::new();
    if original.snapshot_hash != replay.snapshot_hash {
        differences.push("快照哈希");
    }
    if original.policy_version != replay.policy_version {
        differences.push("口径版本");
    }
    if original.naming_rule_version != replay.naming_rule_version {
        differences.push("命名规则版本");
    }
    if original.masked != replay.masked {
        differences.push("脱敏开关");
    }
    if original.content_digest != replay.content_digest {
        differences.push("内容摘要");
    }
    if differences.is_empty() {
        return Ok(());
    }
    Err(ReportError::ReplayMismatch {
        detail: format!("以下维度不一致：{}", differences.join("、")),
    })
}

/// 脱敏自检：文件名与全部可见文本不得命中姓名/学号原文。
fn self_check(report: &Report, tables: &[Table]) -> Result<()> {
    let secrets = crate::masking::leak_secrets(
        report.students.iter().map(|student| student.name.as_str()),
        report
            .students
            .iter()
            .filter(|student| student.student_no != "—")
            .map(|student| student.student_no.as_str()),
    );
    let mut visible = String::new();
    for table in tables {
        visible.push_str(&table.visible_text());
    }
    assert_no_leak(&visible, &secrets)
}

/// 草稿后缀：插入到扩展名前（脱敏后缀之后）。
fn apply_draft(file_name: &str, draft: bool) -> String {
    if !draft {
        return file_name.to_owned();
    }
    match file_name.rsplit_once('.') {
        Some((stem, extension)) => format!("{stem}_草稿.{extension}"),
        None => format!("{file_name}_草稿"),
    }
}

fn audit_note(
    report: &Report,
    request: &ExportRequest,
    formats: &[ExportFormat],
    row_count: usize,
) -> String {
    let mode = match request.mode_token {
        ModeToken::Standard => "标准",
        ModeToken::Fast => "快速",
        ModeToken::All => "全部",
    };
    format!(
        "模式={mode}；格式={}；行数={row_count}；脱敏={}；草稿={}；待复核={}；口径版本={}；命名规则版本={}",
        formats
            .iter()
            .map(|format| format.label())
            .collect::<Vec<_>>()
            .join("+"),
        request.masked,
        request.draft,
        report.pending_count(),
        report.settings.policy_version,
        crate::settings::NAMING_RULE_VERSION
    )
}
