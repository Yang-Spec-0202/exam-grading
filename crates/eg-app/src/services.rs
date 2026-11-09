//! 组合根服务：把页面命令映射到下游 crate（只做装配/转发与错误映射，不写业务规则）。
//!
//! 说明（依赖缺口，见 `docs/dev/research/app.md`）：
//! - 复核队列/命令用 `eg-review` 的内存适配器（platform 的队列查询在 platform v3 接入）；
//! - 规则确认在 `eg-rubric` 内存存储上打通（platform 持久化适配器随 rubric 接入替换）；
//! - 导入预处理用 `eg-capture` 内存资源槽（落库/持久化随 platform v3 接入）。

use std::path::{Path, PathBuf};

use eg_capture::{
    FileInput, ImportKind, ImportOptions, ImportRequest, ImportSettings, Importer, MemoryEventSink,
    NeverCancel, NoKnownSubmissions, PutResult, SeqIdSource,
};
use eg_domain::{
    AnswerKey, GradeMode, MilliScore, Question, QuestionKind, RubricDraftItem, ScoringMode,
    Timestamp, Ulid,
};
use eg_platform::{AssetStore, LogLevel, LogQuery, Platform, TaskFilter, TaskState};
use eg_review::{
    CommandEnvelope, ReviewCommand, ReviewConfig, ReviewReasonCode, ReviewRepository, ReviewService,
};
use eg_rubric::{
    DraftSource, InMemoryRubricStore, RecordingAudit, RubricEngine, RubricSettings, RubricStore,
    SequentialIds,
    structurer::{
        StructurerAttempt, StructurerInvocation, StructurerPort, StructurerReply,
        StructurerRequest, infer_mode, parse_structurer_output, render_structurer_prompt,
    },
};

use crate::channel::{Event, EventBus, NoticeLevel};
use crate::i18n::strings;
use crate::pages::consent::ConsentRowVm;
use crate::pages::diagnostics::LogRowVm;
use crate::pages::import::{ImportReportVm, ImportRowStatus, ImportRowVm};
use crate::pages::progress::TaskRowVm;
use crate::pages::review::{EvidenceVm, QueueRowVm};
use crate::pages::rules::{FindingVm, RuleItemVm};

// ---------------------------------------------------------------------------
// 导入（eg-capture 内存槽）
// ---------------------------------------------------------------------------

/// 选择文件/文件夹（macOS 原生对话框；worker 线程执行，不阻塞 UI）。
pub(crate) fn pick_paths(folder: bool) -> Result<Vec<PathBuf>, String> {
    let script = if folder {
        "POSIX path of (choose folder with prompt \"选择导入文件夹\")"
    } else {
        "set theFiles to (choose file with prompt \"选择导入文件\" with multiple selections allowed)\n\
         set out to \"\"\n\
         repeat with f in theFiles\n\
         set out to out & (POSIX path of f) & linefeed\n\
         end repeat\n\
         return out"
    };
    let output = std::process::Command::new("/usr/bin/osascript")
        .args(["-e", script])
        .output()
        .map_err(|error| format!("无法打开文件选择器：{error}"))?;
    if !output.status.success() {
        return Err("文件选择已取消".to_owned());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut paths: Vec<PathBuf> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect();
    if folder {
        let mut files = Vec::new();
        if let Some(root) = paths.first() {
            collect_files(root, &mut files)?;
        }
        paths = files;
    }
    Ok(paths)
}

fn collect_files(root: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = std::fs::read_dir(root).map_err(|error| format!("读取文件夹失败：{error}"))?;
    let mut paths: Vec<PathBuf> = entries
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            collect_files(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

/// platform 资产库适配 `eg_capture::AssetSink`（内容寻址、去重）。
struct PlatformAssetSink<'a> {
    assets: &'a AssetStore,
}

impl eg_capture::AssetSink for PlatformAssetSink<'_> {
    fn put(&self, bytes: &[u8], media_type: &str) -> Result<PutResult, eg_capture::CaptureError> {
        self.assets
            .put(bytes, media_type)
            .map(|outcome| PutResult {
                asset: outcome.asset,
                deduped: outcome.deduped,
                byte_len: outcome.byte_len,
            })
            .map_err(|error| eg_capture::CaptureError::Asset(error.to_string()))
    }

    fn contains(&self, asset: eg_domain::AssetId) -> bool {
        self.assets.exists(asset)
    }

    fn get(&self, asset: eg_domain::AssetId) -> Result<Vec<u8>, eg_capture::CaptureError> {
        self.assets
            .get(asset)
            .map_err(|error| eg_capture::CaptureError::Asset(error.to_string()))
    }
}

/// 导入上下文：当前试卷与班级（platform 暂无 `list_papers`，此兼容 shim 待平台补 API 后替换）。
fn import_context(platform: &Platform) -> Option<(eg_domain::ClassroomId, eg_domain::PaperId)> {
    let row: Option<(String, String)> = platform
        .db()
        .read(|conn| {
            // 平台暂无 list_papers；用行查询并忽略「无行」（兼容 shim，待平台补 API）。
            Ok::<_, eg_platform::PlatformError>(
                conn.query_row(
                    "SELECT id, classroom_id FROM paper ORDER BY created_at LIMIT 1",
                    [],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .ok(),
            )
        })
        .ok()?;
    let (paper_id, classroom_id) = row?;
    Some((
        eg_domain::ClassroomId::parse(&classroom_id).ok()?,
        eg_domain::PaperId::parse(&paper_id).ok()?,
    ))
}

/// 导入核心：capture 预处理（资产直写 platform 资产库）→ `persist` 同事务落库。
fn import_core(
    platform: &Platform,
    paths: &[PathBuf],
) -> Result<(eg_capture::ImportOutcome, eg_capture::PersistSummary), String> {
    let Some((classroom_id, paper_id)) = import_context(platform) else {
        return Err("没有可导入的试卷：请先在试卷页创建试卷".to_owned());
    };
    let settings = ImportSettings::default();
    let sink = PlatformAssetSink {
        assets: platform.assets().as_ref(),
    };
    let import_events = MemoryEventSink::default();
    let ids = SeqIdSource::new();
    let cancel = NeverCancel;
    let known = NoKnownSubmissions;
    let clock = CaptureClock;
    let importer = Importer::new(
        &settings,
        &sink,
        &import_events,
        &clock,
        &ids,
        &cancel,
        &known,
    );
    let files: Vec<FileInput> = paths
        .iter()
        .map(|path| FileInput::from_path(path.clone()))
        .collect();
    let request = ImportRequest {
        batch_id: Ulid::from_u128(1),
        kind: ImportKind::StudentWork,
        classroom_id,
        paper_id,
        files,
        options: ImportOptions::default(),
    };
    let outcome = importer
        .run(request)
        .map_err(|error| format!("导入失败：{error}"))?;
    // 建导入任务并把提交阶段推进到「预处理完成」（platform v3 的任务队列 + persist 联动）。
    let expected = platform
        .tasks()
        .enqueue(eg_platform::NewTask::new(
            eg_platform::TaskKind::SubmissionGrade,
            serde_json::json!({"kind": "import", "files": paths.len()}),
        ))
        .ok()
        .map(|outcome| outcome.task_id);
    // platform `advance_stage` 只接受 running 状态：先 claim 自己刚入队的任务再 persist。
    let task_id = expected.and_then(|expected_id| {
        platform
            .tasks()
            .claim("eg-app-import")
            .ok()
            .flatten()
            .filter(|record| record.id == expected_id)
            .map(|record| record.id)
    });
    let summary = eg_capture::persist(
        platform,
        eg_capture::PersistRequest {
            outcome: &outcome,
            task_id,
        },
    )
    .map_err(|error| format!("导入已预处理但落库失败：{error}"))?;
    if let Some(task_id) = task_id {
        let _ = platform.tasks().complete(task_id);
    }
    Ok((outcome, summary))
}

/// 把 capture 报告映射为导入页视图模型（组/警告/明细/汇总；UI 与冒烟共用）。
fn import_vm(
    report: &eg_capture::ImportReport,
    persist: &eg_capture::PersistSummary,
) -> ImportReportVm {
    let mut rows = Vec::new();
    for failure in &report.file_failures {
        rows.push(ImportRowVm {
            name: format!(
                "{}…{}",
                failure
                    .fingerprint
                    .path_hash
                    .chars()
                    .take(8)
                    .collect::<String>(),
                failure.fingerprint.extension
            ),
            status: ImportRowStatus::Failed,
            detail: failure.message.clone(),
        });
    }
    for submission in &report.submissions {
        rows.push(ImportRowVm {
            name: submission.submission_id.to_string(),
            status: if submission.page_count == 0 {
                ImportRowStatus::Rejected
            } else {
                ImportRowStatus::Ok
            },
            detail: format!("{} 页", submission.page_count),
        });
    }
    let groups: Vec<crate::pages::import::ImportGroupVm> = report
        .groups
        .iter()
        .map(|group| crate::pages::import::ImportGroupVm {
            key: group.group_key.to_hex().chars().take(12).collect(),
            pages: group.page_count,
            page_order: group
                .page_order
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(","),
            warnings: group
                .warnings
                .iter()
                .map(|code| format!("{code:?}"))
                .collect::<Vec<_>>()
                .join("/"),
        })
        .collect();
    let warnings: Vec<String> = report
        .group_warnings
        .iter()
        .map(|warning| format!("{:?}：{}", warning.code, warning.detail))
        .collect();
    let summary = format!(
        "成功 {} / 拒收 {} / 失败 {} / 重复 {}；组 {}（多页 {}，警告 {}）；已落库：提交 {} / 页 {} / 资产链接 {} / 审计 {}",
        report.files_ok,
        report.pages_rejected,
        report.files_failed,
        report.duplicates.len(),
        groups.len(),
        groups.iter().filter(|group| group.pages > 1).count(),
        warnings.len(),
        persist.submissions,
        persist.pages,
        persist.asset_links,
        persist.audit_entries
    );
    ImportReportVm {
        ok: report.files_ok,
        rejected: report.pages_rejected,
        failed: report.files_failed,
        duplicates: u32::try_from(report.duplicates.len()).unwrap_or(0),
        rows,
        groups,
        warnings,
        summary,
    }
}

/// 执行一次导入并回投进度/报告（路径来自拖拽或文件选择器）。
pub(crate) fn run_import(platform: &Platform, paths: Vec<PathBuf>, events: &EventBus) {
    let total = u32::try_from(paths.len()).unwrap_or(u32::MAX);
    events.send(Event::ImportProgress { done: 0, total });
    match import_core(platform, &paths) {
        Ok((outcome, persist)) => {
            let report = &outcome.report;
            let total = report.files_total;
            let vm = import_vm(report, &persist);
            events.send(Event::ImportProgress { done: total, total });
            events.send(Event::ImportFinished { report: vm });
        }
        Err(message) => {
            events.send(Event::ImportFailed { message });
        }
    }
}

/// 撤销/重做持久栈长度（platform v4 `list_undo`/`list_redo`）。
pub(crate) fn review_history(platform: &Platform) -> (usize, usize) {
    let undo = platform
        .db()
        .read(|conn| eg_platform::repo::list_undo(conn, 1000))
        .map_or(0, |entries| entries.len());
    let redo = platform
        .db()
        .read(|conn| eg_platform::repo::list_redo(conn, 1000, None))
        .map_or(0, |entries| entries.len());
    (undo, redo)
}

/// 复核图像证据：取证据摘要第一张原图的 platform 资产路径（懒加载）。
pub(crate) fn review_image_event(
    platform: &Platform,
    grade_id: eg_domain::GradeId,
) -> Option<Event> {
    let summary = platform
        .db()
        .read(|conn| eg_platform::repo::load_grade_evidence_summary(conn, grade_id))
        .ok()
        .flatten()?;
    let asset = summary.assets.first()?;
    let path = platform.assets().path_of(asset.asset_id);
    Some(Event::ReviewImage {
        path: path.to_string_lossy().into_owned(),
    })
}

/// `eg-capture` 需要 `ClockPort`；用系统时间（导入报告时间）。
struct CaptureClock;

impl eg_capture::ClockPort for CaptureClock {
    fn now(&self) -> Timestamp {
        use eg_domain::Clock as _;
        eg_platform::SystemClock.now()
    }
}

// ---------------------------------------------------------------------------
// 进度（platform 任务队列）
// ---------------------------------------------------------------------------

fn stage_label(stage: Option<eg_domain::Stage>) -> String {
    stage.map_or_else(String::new, |stage| format!("{stage:?}"))
}

/// 读取任务队列快照。
pub(crate) fn load_tasks(platform: &Platform) -> Vec<TaskRowVm> {
    let filter = TaskFilter {
        state: None,
        kind: None,
        limit: 100,
    };
    platform
        .tasks()
        .list(&filter)
        .map(|records| {
            records
                .into_iter()
                .map(|record| TaskRowVm {
                    id: record.id.to_string(),
                    kind: record.kind.code().to_owned(),
                    state: record.state.code().to_owned(),
                    stage: stage_label(record.stage),
                    progress: String::new(),
                    done: 0,
                    total: 0,
                    attempts: record.attempts,
                    max_attempts: record.max_attempts,
                    error: record.last_error.unwrap_or_default(),
                    retryable: record.state == TaskState::Failed,
                    cancelable: matches!(record.state, TaskState::Pending | TaskState::Running),
                })
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// 设置（platform 配置：校验 + 原子保存）
// ---------------------------------------------------------------------------

/// 当前配置 TOML 文本。
pub(crate) fn settings_toml(platform: &Platform) -> String {
    toml::to_string_pretty(platform.config_document()).unwrap_or_default()
}

/// 校验并原子保存一个设置项；失败返回中文原因且旧配置不变。
pub(crate) fn save_setting(platform: &mut Platform, key: &str, raw: &str) -> Result<(), String> {
    let document = toml::Value::try_from(platform.config_document())
        .map_err(|error| format!("配置序列化失败：{error}"))?;
    let page = crate::pages::settings::SettingsPage::new(document);
    let patched = page.patch(key, raw)?;
    let text =
        toml::to_string_pretty(&patched).map_err(|error| format!("配置序列化失败：{error}"))?;
    let candidate: eg_platform::ConfigDocument =
        toml::from_str(&text).map_err(|error| format!("配置类型不匹配：{error}"))?;
    platform
        .save_config(candidate)
        .map_err(|error| format!("{error}"))?;
    let entry = eg_platform::NewAuditEntry::new("config.change", "config", key)
        .with_note(format!("界面修改配置：{key}"));
    platform
        .audit()
        .append(entry)
        .map(|_| ())
        .map_err(|error| format!("审计写入失败：{error}"))
}

/// 恢复上一版配置（`.bak`）。
pub(crate) fn restore_config(platform: &mut Platform) -> Result<String, String> {
    let store = eg_platform::ConfigStore::new(
        platform.dirs().clone(),
        platform.clock().clone(),
        platform.ids().clone(),
    );
    let document = store
        .restore_backup()
        .map_err(|error| format!("恢复配置失败：{error}"))?;
    platform
        .save_config(document)
        .map_err(|error| format!("恢复后保存失败：{error}"))?;
    Ok(settings_toml(platform))
}

// ---------------------------------------------------------------------------
// 授权（platform ConsentStore）
// ---------------------------------------------------------------------------

/// 读取最近任务的授权记录（按任务聚合，最多 100 条）。
pub(crate) fn load_consents(platform: &Platform) -> Vec<ConsentRowVm> {
    let tasks = platform
        .tasks()
        .list(&TaskFilter {
            state: None,
            kind: None,
            limit: 50,
        })
        .unwrap_or_default();
    let mut rows = Vec::new();
    for task in tasks {
        if let Ok(records) = platform.consents().list_by_task(task.id) {
            for record in records {
                rows.push(ConsentRowVm {
                    consent_id: record.consent_id.to_string(),
                    task_id: record.task_id.to_string(),
                    backend: record.backend.clone(),
                    state: record.state.code().to_owned(),
                    state_label: record.state.label().to_owned(),
                    scope: record.scope_hash.to_hex().chars().take(12).collect(),
                    budget: record.budget_micros.to_string(),
                    spent: record.spent_micros.to_string(),
                    remaining: record
                        .remaining_micros()
                        .map_or_else(|| "—".to_owned(), |value| value.to_string()),
                    granted_at: eg_domain::time::format_rfc3339(record.granted_at),
                    exhausted: record.state == eg_platform::ConsentState::Exhausted,
                });
            }
        }
    }
    rows
}

/// 新建授权（scope 文本按 SHA-256 落库；预算/有效期由 UI 校验）。
pub(crate) fn register_consent(
    platform: &Platform,
    task_id: &str,
    backend: &str,
    scope: &str,
    budget: &str,
) -> Result<String, String> {
    let task =
        eg_domain::TaskId::parse(task_id).map_err(|error| format!("任务 id 非法：{error}"))?;
    let budget = budget
        .trim()
        .parse::<u64>()
        .map_err(|_| strings().consent_bad_budget.to_owned())?;
    let record = platform
        .consents()
        .register(
            task,
            backend,
            eg_domain::Sha256Hex::of(scope.as_bytes()),
            budget,
        )
        .map_err(|error| format!("登记授权失败：{error}"))?;
    Ok(record.consent_id.to_string())
}

/// 撤销授权（立即生效）。
pub(crate) fn revoke_consent(platform: &Platform, consent_id: &str) -> Result<String, String> {
    let id = eg_domain::ConsentId::parse(consent_id)
        .map_err(|error| format!("授权 id 非法：{error}"))?;
    platform
        .consents()
        .revoke(id)
        .map_err(|error| format!("撤销失败：{error}"))?;
    Ok(id.to_string())
}

// ---------------------------------------------------------------------------
// 日志与诊断（platform LogSink/Diagnostics）
// ---------------------------------------------------------------------------

fn log_level(code: &str) -> Option<LogLevel> {
    match code {
        "debug" => Some(LogLevel::Debug),
        "info" => Some(LogLevel::Info),
        "warn" => Some(LogLevel::Warn),
        "error" => Some(LogLevel::Error),
        _ => None,
    }
}

/// 查询一页日志（关键词在内存过滤；索引状态显式提示）。
pub(crate) fn load_logs(
    platform: &Platform,
    level: &str,
    module: &str,
    keyword: &str,
    trace_id: &str,
    page: u32,
) -> (Vec<LogRowVm>, u32, String) {
    let page_size = 50usize;
    let offset = usize::try_from(page.saturating_sub(1)).unwrap_or(0) * page_size;
    let query = LogQuery {
        level_min: log_level(level),
        module: (!module.is_empty()).then(|| module.to_owned()),
        trace_id: None,
        task_id: None,
        from: None,
        to: None,
        offset,
        limit: page_size,
    };
    let index_note = if platform.index().is_some() {
        strings().diagnostics_index_ok.to_owned()
    } else {
        "全文索引不可用/重建中：已退化为字段过滤".to_owned()
    };
    match platform.log().query(&query) {
        Ok(result) => {
            let mut rows: Vec<LogRowVm> = result
                .records
                .into_iter()
                .filter(|record| keyword.is_empty() || record.message.contains(keyword))
                .filter(|record| {
                    trace_id.is_empty()
                        || record
                            .trace_id
                            .is_some_and(|id| id.to_string().contains(trace_id))
                })
                .map(|record| LogRowVm {
                    at: eg_domain::time::format_rfc3339(record.at),
                    level: record.level.code().to_owned(),
                    module: record.module.clone(),
                    message: record.message.clone(),
                    trace_id: record.trace_id.map(|id| id.to_string()).unwrap_or_default(),
                })
                .collect();
            let total = u32::try_from(result.total).unwrap_or(u32::MAX);
            if keyword.is_empty() && trace_id.is_empty() {
                // 单页内过滤不改变总数（字段过滤由查询完成）。
            } else {
                rows.retain(|row| row.message.contains(keyword) || keyword.is_empty());
            }
            (rows, total, index_note)
        }
        Err(error) => {
            let message = format!("日志读取失败：{error}");
            events_note(platform, &message);
            (Vec::new(), 0, message)
        }
    }
}

fn events_note(_platform: &Platform, _message: &str) {
    // 诊断页内联错误由调用方回投；这里不重复写日志（避免日志页递归）。
}

/// 导出诊断包并返回摘要。
pub(crate) fn export_diagnostics(platform: &Platform) -> String {
    let request = eg_platform::DiagnosticRequest {
        from: None,
        to: None,
        include_schema: true,
        output_dir: None,
        log_limit: 2_000,
    };
    match platform.diagnostics().export(&request) {
        Ok(bundle) => format!(
            "诊断包已导出：{}（{} 字节，{} 个条目）",
            bundle.path.display(),
            bundle.bytes,
            bundle.entries.len()
        ),
        Err(error) => format!("诊断包导出失败：{error}"),
    }
}

// ---------------------------------------------------------------------------
// 复核（platform v3 真实仓储 + eg-review ReviewService）
// ---------------------------------------------------------------------------

/// 复核运行时：队列快照（grade id 顺序）由 worker 持有，命令按 id 定位。
pub(crate) fn review_queue(
    platform: &Platform,
) -> (Vec<eg_domain::GradeId>, Vec<QueueRowVm>, String) {
    let filter = eg_platform::repo::GradeQueueFilter::default();
    let records = platform
        .db()
        .read(|conn| eg_platform::repo::list_grade_records_for_queue(conn, &filter, None))
        .unwrap_or_default();
    let total = platform
        .db()
        .read(|conn| eg_platform::repo::count_grade_records_for_queue(conn, &filter))
        .unwrap_or(0);
    let repo = crate::platform_adapters::PlatformReviewRepo::new(platform);
    let config = ReviewConfig::default();
    let mut rows = Vec::with_capacity(records.len());
    let mut ids = Vec::with_capacity(records.len());
    for record in &records {
        ids.push(record.id);
        let metadata = repo.metadata(record);
        let read_only = record.sampled && config.sampled_readonly;
        rows.push(QueueRowVm {
            grade_id: record.id.to_string(),
            student: metadata.student_display_name.clone(),
            question: metadata.question_label.clone(),
            score: format!(
                "{}/{}",
                record.awarded.as_milli() as f64 / 1000.0,
                record.max_score.as_milli() as f64 / 1000.0
            ),
            mode: mode_code(record.mode).to_owned(),
            sampled: record.sampled,
            status: format!("{:?}", record.status),
            reasons: record.evidence.len().to_string(),
            selected: false,
            read_only,
            batched: false,
        });
    }
    let counts = format!(
        "标准 {} · 抽检 {} · 合计 {total}（platform 队列）",
        rows.iter().filter(|row| !row.sampled).count(),
        rows.iter().filter(|row| row.sampled).count()
    );
    (ids, rows, counts)
}

/// 详情（platform 证据摘要 + eg-review build_detail）。
pub(crate) fn review_detail(
    platform: &Platform,
    grade_id: eg_domain::GradeId,
) -> Option<crate::channel::Event> {
    let repo = crate::platform_adapters::PlatformReviewRepo::new(platform);
    let audit = crate::platform_adapters::PlatformAuditSink::new(platform);
    let undo = crate::platform_adapters::PlatformUndoStore::new(platform, 50);
    let regrade = crate::platform_adapters::PlatformRegradeEngine::new(platform);
    let clock = eg_platform::SystemClock;
    let ids = crate::platform_adapters::PlatformIdGen::new(platform);
    let config = ReviewConfig::default();
    let service = ReviewService::new(&repo, &audit, &undo, &regrade, &clock, &ids, &config).ok()?;
    let detail = service.detail(grade_id).ok()?;
    let meta = format!(
        "学号 {} · 题 {} · 模式 {} · 修订 {}",
        detail.item.student_no,
        detail.item.question_label,
        mode_code(detail.item.mode),
        detail.item.revision
    );
    let answer = detail
        .answer
        .as_ref()
        .map_or_else(|| "（无识别文本）".to_owned(), |view| view.text.clone());
    let reference = detail
        .reference
        .as_ref()
        .map_or_else(|| "（无参考答案数据）".to_owned(), |view| view.text.clone());
    let judgement = format!("{:?}", detail.judgement);
    let score_text = format!(
        "{}/{}",
        detail.item.awarded.as_milli(),
        detail.item.max_score.as_milli()
    );
    let reasons: Vec<String> = [
        ReviewReasonCode::RecognitionError,
        ReviewReasonCode::AnswerKeyAmbiguity,
        ReviewReasonCode::ModelMisjudged,
        ReviewReasonCode::PartialCredit,
        ReviewReasonCode::UnitOrPrecision,
        ReviewReasonCode::Other,
    ]
    .iter()
    .map(|reason| reason.as_str().to_owned())
    .collect();
    let evidence: Vec<EvidenceVm> = detail
        .assets
        .iter()
        .enumerate()
        .map(|(index, asset)| EvidenceVm {
            label: format!("图像证据 {}", index + 1),
            value: asset.to_string(),
        })
        .chain(
            detail
                .evidence
                .iter()
                .enumerate()
                .map(|(index, kind)| EvidenceVm {
                    label: format!("判定证据 {}", index + 1),
                    value: format!("{kind:?}"),
                }),
        )
        .collect();
    Some(Event::ReviewDetail {
        meta,
        answer,
        reference,
        judgement,
        score_text,
        reasons,
        evidence,
        read_only: detail.read_only,
    })
}

/// 执行复核命令（platform 事务：追加修订 + 原因 + 审计 + 撤销栈）。
pub(crate) fn review_execute(
    platform: &Platform,
    grade_id: eg_domain::GradeId,
    kind: &str,
    score: Option<&str>,
    reason: Option<i32>,
    text: Option<&str>,
) -> Event {
    let repo = crate::platform_adapters::PlatformReviewRepo::new(platform);
    let audit = crate::platform_adapters::PlatformAuditSink::new(platform);
    let undo = crate::platform_adapters::PlatformUndoStore::new(platform, 50);
    let regrade = crate::platform_adapters::PlatformRegradeEngine::new(platform);
    let clock = eg_platform::SystemClock;
    let ids = crate::platform_adapters::PlatformIdGen::new(platform);
    let config = ReviewConfig::default();
    let Ok(service) = ReviewService::new(&repo, &audit, &undo, &regrade, &clock, &ids, &config)
    else {
        return Event::ReviewNotice {
            message: "复核服务初始化失败".to_owned(),
        };
    };
    let Some(record) = repo.get(grade_id) else {
        return Event::ReviewNotice {
            message: "判分记录不存在".to_owned(),
        };
    };
    let reason = reason_code(reason.unwrap_or(0));
    let command = match kind {
        "accept" => ReviewCommand::Accept { note: None },
        "override" => {
            let Some(score) = score.and_then(|text| text.trim().parse::<i64>().ok()) else {
                return Event::ReviewNotice {
                    message: "改分输入不合法（千分之一分整数）".to_owned(),
                };
            };
            ReviewCommand::OverrideScore {
                score_milli: score,
                reason,
                note: None,
            }
        }
        "regrade_text" => ReviewCommand::RegradeText {
            corrected: eg_review::CorrectedText {
                text: text.unwrap_or_default().to_owned(),
                latex: None,
            },
            note: None,
        },
        "sampling_pass" => ReviewCommand::SamplingPass { note: None },
        "regrade_whole" => ReviewCommand::SamplingFail {
            scope: eg_review::RegradeScope::WholeSubmission,
            confirmed: true,
            note: None,
        },
        "undo" => ReviewCommand::Undo,
        "redo" => ReviewCommand::Redo,
        other => {
            return Event::ReviewNotice {
                message: format!("未实现的复核命令：{other}"),
            };
        }
    };
    let envelope = CommandEnvelope {
        target: Some(grade_id),
        idempotency_key: format!(
            "ui-{grade_id}-{kind}-{}-{}-{}",
            record.revision,
            score.unwrap_or(""),
            text.unwrap_or("")
        ),
        expected_revision: Some(record.revision),
        command,
    };
    match service.execute(&envelope) {
        Ok(result) => {
            if let eg_review::CommandOutcome::SamplingPrompt { prompt } = result.outcome {
                return Event::ReviewSamplingPrompt {
                    message: prompt.message,
                };
            }
            Event::ReviewApplied {
                notice: format!("命令已执行：{kind}"),
                advance: matches!(kind, "accept" | "override" | "regrade_text"),
            }
        }
        Err(error) => Event::ReviewNotice {
            message: format!("命令被拒绝：{error}"),
        },
    }
}

fn mode_code(mode: GradeMode) -> &'static str {
    match mode {
        GradeMode::Standard => "standard",
        GradeMode::Fast => "fast",
    }
}

fn reason_code(index: i32) -> ReviewReasonCode {
    match index {
        1 => ReviewReasonCode::AnswerKeyAmbiguity,
        2 => ReviewReasonCode::ModelMisjudged,
        3 => ReviewReasonCode::PartialCredit,
        4 => ReviewReasonCode::UnitOrPrecision,
        5 => ReviewReasonCode::Other,
        _ => ReviewReasonCode::RecognitionError,
    }
}

// ---------------------------------------------------------------------------
// 规则（eg-rubric 内存存储；手写直译 + 校验 + 确认/拒绝）
// ---------------------------------------------------------------------------

/// 规则 bundle：内存存储 + 审计 + 顺序 id + 固定时钟。
pub(crate) struct RulesBundle {
    store: InMemoryRubricStore,
    audit: RecordingAudit,
    ids: SequentialIds,
    settings: RubricSettings,
    clock: eg_grade::ports::FixedClock,
    draft_id: Option<eg_domain::RubricId>,
}

impl RulesBundle {
    /// 构造（固定时钟便于确定性冒烟）。
    pub(crate) fn new() -> Self {
        let now =
            eg_domain::time::parse_rfc3339("2026-11-03T00:00:00Z").unwrap_or(Timestamp::UNIX_EPOCH);
        Self {
            store: InMemoryRubricStore::default(),
            audit: RecordingAudit::default(),
            ids: SequentialIds::default(),
            settings: RubricSettings::default(),
            clock: eg_grade::ports::FixedClock::new(now),
            draft_id: None,
        }
    }

    fn fixture_question() -> Question {
        Question {
            id: eg_domain::QuestionId::from_ulid(Ulid::from_u128(9_001)),
            paper_id: eg_domain::PaperId::from_ulid(Ulid::from_u128(9_002)),
            ordinal: 1,
            label: "17".to_owned(),
            kind: QuestionKind::Calculation,
            max_score: MilliScore::from_milli(5_000).unwrap_or(MilliScore::ZERO),
            stem: None,
            key: AnswerKey::ExplicitlyOmitted { note: None },
            rubric: None,
            subject_hint: None,
            knowledge_tags: Vec::new(),
        }
    }

    fn engine(&self) -> Option<RubricEngine<'_>> {
        RubricEngine::new(
            &self.store,
            &self.audit,
            &self.clock,
            &self.ids,
            &self.settings,
        )
        .ok()
    }

    /// 模型整理：渲染提示词 → structurer 端口（严格 Schema，至多 1 次机械修复）→
    /// 只组装 `ModelStructured` 草稿（绝不生效）；失败显式返回并保留原文。
    pub(crate) fn structured(&mut self, raw_text: &str) -> Event {
        let question = Self::fixture_question();
        let request = StructurerRequest {
            raw_text,
            question_kind: question.kind,
            max_score: question.max_score,
            template_skeleton: None,
        };
        let prompt = render_structurer_prompt(&request);
        let port = FixtureStructurer;
        let invocation = StructurerInvocation {
            prompt: &prompt,
            request: &request,
            attempt: StructurerAttempt::Initial,
            previous_output: None,
            violations: &[],
        };
        let reply = match port.structure(&invocation) {
            Ok(reply) => reply,
            Err(error) => {
                return Event::RulesFailed {
                    message: format!("规则整理调用失败（未降级到其他模型）：{error}"),
                    model_call_id: String::new(),
                };
            }
        };
        let items = match parse_structurer_output(&reply.raw_output) {
            Ok(items) => items,
            Err(violations) => {
                // 至多 1 次机械修复（只反馈结构违规，不重写语义）。
                let repair = StructurerInvocation {
                    prompt: &prompt,
                    request: &request,
                    attempt: StructurerAttempt::Repair,
                    previous_output: Some(&reply.raw_output),
                    violations: &violations,
                };
                match port.structure(&repair) {
                    Ok(repaired) => match parse_structurer_output(&repaired.raw_output) {
                        Ok(items) => items,
                        Err(repaired_violations) => {
                            return Event::RulesFailed {
                                message: format!(
                                    "规则整理输出不合法（已重试 1 次）：{}；原始输出已保留，原文未修改",
                                    repaired_violations.join("；")
                                ),
                                model_call_id: repaired.model_call_id.to_string(),
                            };
                        }
                    },
                    Err(error) => {
                        return Event::RulesFailed {
                            message: format!(
                                "规则整理修复调用失败：{error}；违规：{}；原始输出已保留",
                                violations.join("；")
                            ),
                            model_call_id: reply.model_call_id.to_string(),
                        };
                    }
                }
            }
        };
        let mode = infer_mode(question.kind, &items);
        let Some(engine) = self.engine() else {
            return Event::RulesFailed {
                message: "规则引擎初始化失败".to_owned(),
                model_call_id: reply.model_call_id.to_string(),
            };
        };
        let source = DraftSource::ModelStructured {
            raw_text: raw_text.to_owned(),
            items,
            mode,
            partial_choice: None,
            model_call_id: reply.model_call_id,
        };
        match engine.create_draft(&question, source) {
            Ok(stored) => {
                let draft_id = stored.draft.id;
                let findings: Vec<FindingVm> = engine
                    .report_for(&stored)
                    .findings
                    .iter()
                    .map(finding_vm)
                    .collect();
                let items: Vec<RuleItemVm> = stored
                    .draft
                    .items
                    .iter()
                    .map(|item| RuleItemVm {
                        criterion: item.criterion.clone(),
                        points: format!("{:.1}", item.points.as_milli() as f64 / 1000.0),
                        required: item.required,
                    })
                    .collect();
                self.draft_id = Some(draft_id);
                Event::RulesDraft {
                    draft_id: draft_id.to_string(),
                    model_call_id: stored
                        .draft
                        .model_call_id
                        .map_or_else(String::new, |id| id.to_string()),
                    items,
                    findings,
                }
            }
            Err(error) => Event::RulesFailed {
                message: format!("整理草稿创建失败：{error}"),
                model_call_id: String::new(),
            },
        }
    }

    /// 手写直译：由 UI 提供的判据/分值创建草稿并跑静态校验。
    pub(crate) fn handwritten(&mut self, raw_text: &str, criterion: &str, points: &str) -> Event {
        let Some(engine) = self.engine() else {
            return Event::RulesFailed {
                message: "规则引擎初始化失败".to_owned(),
                model_call_id: String::new(),
            };
        };
        let points = points.trim().parse::<f64>().unwrap_or(0.0);
        let points =
            MilliScore::from_points((points * 1000.0).round() as i64).unwrap_or(MilliScore::ZERO);
        let item = RubricDraftItem {
            criterion: criterion.to_owned(),
            points,
            required: true,
            depends_on: Vec::new(),
        };
        let question = Self::fixture_question();
        let source = DraftSource::Handwritten {
            raw_text: raw_text.to_owned(),
            items: vec![item],
            mode: ScoringMode::Additive,
            partial_choice: None,
        };
        match engine.create_draft(&question, source) {
            Ok(stored) => {
                let draft_id = stored.draft.id;
                let findings: Vec<FindingVm> = engine
                    .report_for(&stored)
                    .findings
                    .iter()
                    .map(finding_vm)
                    .collect();
                let items: Vec<RuleItemVm> = stored
                    .draft
                    .items
                    .iter()
                    .map(|item| RuleItemVm {
                        criterion: item.criterion.clone(),
                        points: format!("{:.1}", item.points.as_milli() as f64 / 1000.0),
                        required: item.required,
                    })
                    .collect();
                let model_call_id = stored
                    .draft
                    .model_call_id
                    .map_or_else(String::new, |id| id.to_string());
                self.draft_id = Some(draft_id);
                Event::RulesDraft {
                    draft_id: draft_id.to_string(),
                    model_call_id,
                    items,
                    findings,
                }
            }
            Err(error) => Event::RulesFailed {
                message: format!("创建草稿失败：{error}"),
                model_call_id: String::new(),
            },
        }
    }

    /// 确认当前草稿。
    pub(crate) fn confirm(&mut self) -> Event {
        let Some(draft_id) = self.draft_id else {
            return Event::RulesFailed {
                message: "当前没有待确认草稿".to_owned(),
                model_call_id: String::new(),
            };
        };
        let Some(engine) = self.engine() else {
            return Event::RulesFailed {
                message: "规则引擎初始化失败".to_owned(),
                model_call_id: String::new(),
            };
        };
        if let Err(error) = engine.submit_for_confirmation(draft_id) {
            return Event::RulesFailed {
                message: format!("提交确认失败：{error}"),
                model_call_id: String::new(),
            };
        }
        let version = self
            .store
            .rubric_version(Self::fixture_question().id)
            .unwrap_or(0);
        match engine.confirm(draft_id, Some(version)) {
            Ok(outcome) => Event::RulesConfirmed {
                version: outcome.rubric.version,
            },
            Err(error) => Event::RulesFailed {
                message: format!("确认失败：{error}"),
                model_call_id: String::new(),
            },
        }
    }

    /// 拒绝当前草稿。
    pub(crate) fn reject(&mut self, reason: &str) -> Event {
        let Some(draft_id) = self.draft_id else {
            return Event::RulesFailed {
                message: "当前没有待确认草稿".to_owned(),
                model_call_id: String::new(),
            };
        };
        let Some(engine) = self.engine() else {
            return Event::RulesFailed {
                message: "规则引擎初始化失败".to_owned(),
                model_call_id: String::new(),
            };
        };
        match engine.reject(draft_id, reason) {
            Ok(_) => Event::RulesRejected {
                reason: reason.to_owned(),
            },
            Err(error) => Event::RulesFailed {
                message: format!("拒绝失败：{error}"),
                model_call_id: String::new(),
            },
        }
    }
}

fn finding_vm(finding: &eg_rubric::Finding) -> FindingVm {
    FindingVm {
        check: finding.code.code().to_owned(),
        error: finding.issue.severity == eg_domain::RubricIssueSeverity::Error,
        message: finding.issue.message.clone(),
    }
}

/// 通知辅助：把服务错误显示为错误类事件（`worker` 用）。
pub(crate) fn notify_error(events: &EventBus, message: String) {
    events.send(Event::Notice {
        level: NoticeLevel::Error,
        title: strings().error_title.to_owned(),
        body: message,
    });
}

// ---------------------------------------------------------------------------
// 冒烟链路：夹具落库 → platform 队列/复核 → eg-report 导出
// ---------------------------------------------------------------------------

/// 冒烟用最小 1×1 PNG（让图像证据懒加载有真实文件可读）。
const SMOKE_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x62, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];

/// 读取 `fixtures/paper-20q` 的真实试卷/班级/名单（与 eg-domain 类型直接反序列化）。
fn load_fixture_records() -> Option<(
    eg_domain::Classroom,
    eg_domain::Paper,
    Vec<eg_domain::Student>,
)> {
    let read = |name: &str| std::fs::read_to_string(format!("fixtures/paper-20q/{name}")).ok();
    let classroom: eg_domain::Classroom = serde_json::from_str(&read("classroom.json")?).ok()?;
    let paper: eg_domain::Paper = serde_json::from_str(&read("paper.json")?).ok()?;
    let students: Vec<eg_domain::Student> = serde_json::from_str(&read("students.json")?).ok()?;
    Some((classroom, paper, students))
}

/// 冒烟夹具（只在空库时写入：课堂/学生/试卷/提交/判分记录/任务）。
pub(crate) fn seed_smoke_fixtures(platform: &Platform) -> Result<String, String> {
    use eg_domain::{
        AnswerKey, Classroom, ClassroomId, Confidence, EvidenceRef, ExifOrientation, GradeId,
        GradeRecord, GradeStatus, Judgement, MilliScore, Page, PageQuality, Paper, Question,
        QuestionId, RosterSource, Rotation, RouteDecision, RouteTrace, Sha256Hex, Student,
        StudentId, Submission, SubmissionId, SubmissionStatus,
    };
    let filter = eg_platform::repo::GradeQueueFilter::default();
    let existing = platform
        .db()
        .read(|conn| eg_platform::repo::count_grade_records_for_queue(conn, &filter))
        .map_err(|error| error.to_string())?;
    if existing > 0 {
        return Ok("已有数据（跳过种子）".to_owned());
    }
    if let Some((classroom, paper, students)) = load_fixture_records() {
        let question_count = paper.questions.len();
        let student_count = students.len();
        let now = platform.clock().now();
        platform
            .db()
            .write(move |tx| {
                eg_platform::repo::insert_classroom(tx, &classroom, Some("fixture"), now)?;
                for student in &students {
                    eg_platform::repo::insert_student(tx, student, now)?;
                }
                eg_platform::repo::insert_paper(tx, &paper)?;
                Ok(())
            })
            .map_err(|error| error.to_string())?;
        return Ok(format!(
            "fixtures/paper-20q 试卷（{question_count} 题 / {student_count} 学生）"
        ));
    }
    let now = platform.clock().now();
    let classroom = Classroom {
        id: ClassroomId::from_ulid(Ulid::from_u128(9_001)),
        name: "冒烟班".to_owned(),
        term: "2026 秋".to_owned(),
    };
    let paper_id = eg_domain::PaperId::from_ulid(Ulid::from_u128(9_002));
    let question = Question {
        id: QuestionId::from_ulid(Ulid::from_u128(9_003)),
        paper_id,
        ordinal: 1,
        label: "17".to_owned(),
        kind: QuestionKind::Calculation,
        max_score: MilliScore::from_milli(5_000).unwrap_or(MilliScore::ZERO),
        stem: None,
        key: AnswerKey::ExplicitlyOmitted { note: None },
        rubric: None,
        subject_hint: None,
        knowledge_tags: vec!["冒烟".to_owned()],
    };
    let paper = Paper {
        id: paper_id,
        classroom_id: classroom.id,
        title: "冒烟试卷".to_owned(),
        subject: eg_domain::SubjectId::math(),
        total_score: question.max_score,
        questions: vec![question.clone()],
        key_version: 1,
        rubric_version: 1,
        created_at: now,
    };
    let students: Vec<Student> = ["张三", "李四", "王五"]
        .iter()
        .enumerate()
        .map(|(index, name)| Student {
            id: StudentId::from_ulid(Ulid::from_u128(9_100 + index as u128)),
            classroom_id: classroom.id,
            student_no: format!("2026{:03}", index + 1),
            name: (*name).to_owned(),
            source: RosterSource::ManualAdd,
        })
        .collect();
    let mut submissions = Vec::new();
    let mut pages = Vec::new();
    let mut assets = Vec::new();
    for (index, student) in students.iter().enumerate() {
        let asset = platform
            .assets()
            .put(SMOKE_PNG, "image/png")
            .map_err(|error| error.to_string())?
            .asset;
        assets.push(asset);
        let submission_id = SubmissionId::from_ulid(Ulid::from_u128(9_200 + index as u128));
        let page = Page {
            id: eg_domain::PageId::from_ulid(Ulid::from_u128(9_300 + index as u128)),
            submission_id,
            page_no: 1,
            asset,
            width: 1200,
            height: 1600,
            rotation_applied: Rotation::new(0).unwrap_or_default(),
            exif_orientation: ExifOrientation::new(1).unwrap_or_default(),
            quality: PageQuality {
                blur_var: 200.0,
                exposure: 0.5,
                skew_deg: 1.0,
                resolution_ok: true,
                verdict: eg_domain::QualityVerdict::Acceptable,
            },
            derived: Vec::new(),
        };
        pages.push(page);
        submissions.push(Submission {
            id: submission_id,
            paper_id,
            classroom_id: classroom.id,
            student_id: Some(student.id),
            source_file_name: format!("{}-{}.jpg", student.student_no, student.name),
            source_stem: student.student_no.clone(),
            pages: Vec::new(),
            status: SubmissionStatus::Imported,
            created_at: now,
        });
    }
    let records: Vec<(SubmissionId, GradeRecord)> = submissions
        .iter()
        .enumerate()
        .map(|(index, submission)| {
            let awarded = MilliScore::from_milli(2_000).unwrap_or(MilliScore::ZERO);
            let record = GradeRecord {
                id: GradeId::from_ulid(Ulid::from_u128(9_400 + index as u128)),
                submission_id: submission.id,
                question_id: question.id,
                revision: 1,
                mode: GradeMode::Standard,
                route: RouteDecision::RuleThenModel,
                route_trace: RouteTrace {
                    rule_id: "R7".to_owned(),
                    inputs_hash: Sha256Hex::of(b"smoke"),
                    classifier_call: None,
                    decided_at: now,
                },
                judgement: Judgement::Partial {
                    score: awarded,
                    satisfied: Vec::new(),
                    missing: Vec::new(),
                    reason: "部分给分".to_owned(),
                },
                awarded,
                max_score: question.max_score,
                confidence: Confidence::new(0.62).unwrap_or(Confidence::ZERO),
                status: GradeStatus::NeedsReview,
                evidence: vec![EvidenceRef::Asset(assets[index])],
                engines: Vec::new(),
                created_at: now,
                reviewed_at: None,
                review_note: None,
                sampled: false,
            };
            (submission.id, record)
        })
        .collect();
    let task_payload = serde_json::json!({"smoke": true});
    platform
        .db()
        .write(move |tx| {
            eg_platform::repo::insert_classroom(tx, &classroom, Some("smoke"), now)?;
            for student in &students {
                eg_platform::repo::insert_student(tx, student, now)?;
            }
            eg_platform::repo::insert_paper(tx, &paper)?;
            for submission in &submissions {
                let mut submission = submission.clone();
                submission.pages = pages
                    .iter()
                    .filter(|page| page.submission_id == submission.id)
                    .cloned()
                    .collect();
                eg_platform::repo::insert_submission(tx, &submission)?;
            }
            for (_, record) in &records {
                eg_platform::repo::append_grade_record_with_reasons(tx, record, &[])?;
            }
            Ok(())
        })
        .map_err(|error| error.to_string())?;
    let _ = platform.tasks().enqueue(eg_platform::NewTask::new(
        eg_platform::TaskKind::SubmissionGrade,
        task_payload,
    ));
    Ok("内置夹具（3 提交 × 1 题）".to_owned())
}

/// 导出审计：platform 审计链 + `export_record` 落库（同一事务）。
struct PlatformExportAuditor<'a> {
    platform: &'a Platform,
}

impl eg_report::ExportAuditor for PlatformExportAuditor<'_> {
    fn record(&self, event: &eg_report::ExportAuditEvent) -> eg_report::Result<()> {
        let Ok(id) = self.platform.ids().next_ulid() else {
            return Err(eg_report::ReportError::Audit("导出 id 生成失败".to_owned()));
        };
        let record = eg_platform::repo::ExportRecord {
            id,
            kind: "paper_report".to_owned(),
            format: event.formats.join("+"),
            scope: serde_json::json!({"snapshot_hash": event.snapshot_hash.to_hex()}),
            summary_hash: event
                .after_hash
                .unwrap_or_else(|| eg_domain::Sha256Hex::of(b"export")),
            row_count: u64::try_from(event.row_count).unwrap_or(u64::MAX),
            body: serde_json::json!({"file_count": event.file_count, "masked": event.masked}),
            created_at: event.occurred_at,
            audit_id: None,
        };
        let entry = eg_platform::NewAuditEntry::new(
            event.action.clone(),
            event.entity_type.clone(),
            event.entity_id.clone(),
        )
        .with_note(event.note.clone())
        .with_hashes(None, event.after_hash);
        self.platform
            .audit()
            .command_write(entry, move |tx| {
                eg_platform::repo::insert_export_record(tx, &record)?;
                Ok(())
            })
            .map_err(|error| eg_report::ReportError::Audit(error.to_string()))
    }
}

/// 从 platform 读取一致快照并导出报表；返回中文摘要。
pub(crate) fn export_report(platform: &Platform) -> Result<String, String> {
    use eg_domain::PaperId;
    let now = platform.clock().now();
    let filter = eg_platform::repo::GradeQueueFilter::default();
    let records_latest = platform
        .db()
        .read(|conn| eg_platform::repo::list_grade_records_for_queue(conn, &filter, None))
        .map_err(|error| error.to_string())?;
    if records_latest.is_empty() {
        return Err("没有可导出的判分记录（请先导入并判分）".to_owned());
    }
    let paper_id = records_latest
        .first()
        .map(|record| record.submission_id)
        .and_then(|submission_id| {
            platform
                .db()
                .read(|conn| eg_platform::repo::get_submission(conn, submission_id))
                .ok()
                .flatten()
                .map(|submission| submission.paper_id)
        })
        .unwrap_or(PaperId::from_ulid(Ulid::from_u128(9_002)));
    let paper = platform
        .db()
        .read(|conn| eg_platform::repo::get_paper(conn, paper_id))
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "试卷不存在".to_owned())?;
    let students = platform
        .db()
        .read(|conn| eg_platform::repo::list_students(conn, paper.classroom_id))
        .map_err(|error| error.to_string())?;
    let classroom_name = platform
        .db()
        .read(eg_platform::repo::list_classrooms)
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|classroom| classroom.id == paper.classroom_id)
        .map_or_else(|| "未知班级".to_owned(), |classroom| classroom.name);
    let mut submission_ids: Vec<eg_domain::SubmissionId> = Vec::new();
    let mut records = Vec::new();
    let mut exclusions = Vec::new();
    let mut submissions = Vec::new();
    for record in &records_latest {
        if !submission_ids.contains(&record.submission_id) {
            submission_ids.push(record.submission_id);
        }
    }
    for submission_id in &submission_ids {
        if let Some(submission) = platform
            .db()
            .read(|conn| eg_platform::repo::get_submission(conn, *submission_id))
            .map_err(|error| error.to_string())?
        {
            submissions.push(eg_report::SubmissionSummary {
                id: submission.id,
                paper_id: submission.paper_id,
                student_id: submission.student_id,
                status: submission.status,
                source_stem: submission.source_stem,
                created_at: submission.created_at,
            });
            records.extend(
                platform
                    .db()
                    .read(|conn| eg_platform::repo::list_grade_records(conn, *submission_id))
                    .map_err(|error| error.to_string())?,
            );
            if let Some(exclusion) = platform
                .db()
                .read(|conn| eg_platform::repo::get_submission_exclusion(conn, *submission_id))
                .map_err(|error| error.to_string())?
            {
                exclusions.push(exclusion);
            }
        }
    }
    let questions: Vec<eg_report::QuestionSnapshot> = paper
        .questions
        .iter()
        .map(|question| eg_report::QuestionSnapshot {
            id: question.id,
            ordinal: question.ordinal,
            label: question.label.clone(),
            kind: question.kind,
            max_score: question.max_score,
            knowledge_tags: question.knowledge_tags.clone(),
            answer_options: Vec::new(),
            correct_options: Vec::new(),
        })
        .collect();
    let input = eg_report::ReportInput {
        paper: eg_report::PaperSnapshot {
            id: paper.id,
            classroom_id: paper.classroom_id,
            title: paper.title.clone(),
            classroom_name,
            total_score: paper.total_score,
            key_version: paper.key_version,
            rubric_version: paper.rubric_version,
            questions,
        },
        students,
        submissions,
        records,
        exclusions,
        choices: Vec::new(),
    };
    let report = eg_report::Report::build(&input, eg_report::ReportSettings::default())
        .map_err(|error| format!("报表构建失败：{error}"))?;
    let out_dir = platform.dirs().exports_dir();
    let mut request = eg_report::ExportRequest::new(out_dir.clone(), now);
    request.allow_overwrite = true;
    let auditor = PlatformExportAuditor { platform };
    let outcome = eg_report::export(&report, &request, &auditor)
        .map_err(|error| format!("导出失败：{error}"))?;
    Ok(format!(
        "报表已导出到 {}（{} 个文件，{} 行）",
        out_dir.display(),
        outcome.files.len(),
        outcome.row_count
    ))
}

/// 夹具图片目录（`fixtures/paper-20q` 由 tech-spike 并行生成；未就绪返回空）。
fn fixture_image_paths() -> Vec<PathBuf> {
    let candidates = [
        "fixtures/images",
        "fixtures/paper-20q/submissions",
        "fixtures/paper-20q/images",
        "fixtures/paper-20q",
    ];
    for candidate in candidates {
        let Ok(entries) = std::fs::read_dir(candidate) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension().is_some_and(|ext| {
                    matches!(
                        ext.to_string_lossy().to_ascii_lowercase().as_str(),
                        "jpg" | "jpeg" | "png" | "tif" | "tiff" | "pdf"
                    )
                })
            })
            .collect();
        // 确定性顺序：优先「规范文件名」再「-copy/重复副本」，保证同字节去重保留规范页
        // （否则 `张三_p2.png` 与 `张三_p2-copy.png` 谁先谁后取决于字典序，归组结果会抖动）。
        files.sort_by(|left, right| {
            let left_name = left
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
            let right_name = right
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
            let left_copy = u8::from(left_name.contains("copy") || left_name.contains("副本"));
            let right_copy = u8::from(right_name.contains("copy") || right_name.contains("副本"));
            left_copy
                .cmp(&right_copy)
                .then_with(|| left_name.len().cmp(&right_name.len()))
                .then_with(|| left_name.cmp(&right_name))
        });
        if !files.is_empty() {
            return files;
        }
    }
    Vec::new()
}

/// 首个题目（id/满分）——导入后补夹具判分记录用（平台暂无需的投影 API，兼容 shim）。
fn first_question(platform: &Platform) -> Option<(eg_domain::QuestionId, MilliScore)> {
    let row: Option<(String, i64)> = platform
        .db()
        .read(|conn| {
            Ok::<_, eg_platform::PlatformError>(
                conn.query_row(
                    "SELECT id, max_score FROM question ORDER BY paper_id, ordinal LIMIT 1",
                    [],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                )
                .ok(),
            )
        })
        .ok()?;
    let (id, max) = row?;
    Some((
        eg_domain::QuestionId::parse(&id).ok()?,
        MilliScore::from_milli(max).ok()?,
    ))
}

/// 为导入的提交补一条夹具判分记录（S1 链路：识别/对齐/判分流水线未接入的占位）。
fn append_fixture_grades(
    platform: &Platform,
    submissions: &[eg_domain::SubmissionId],
) -> Result<u32, String> {
    use eg_domain::{
        AssetId, Confidence, EvidenceRef, GradeId, GradeRecord, GradeStatus, Judgement,
        RouteDecision, RouteTrace, Sha256Hex,
    };
    let Some((question_id, max_score)) = first_question(platform) else {
        return Err("没有题目，无法补判分记录".to_owned());
    };
    let now = platform.clock().now();
    let mut written = 0;
    for (index, submission_id) in submissions.iter().enumerate() {
        let existing = platform
            .db()
            .read(|conn| {
                eg_platform::repo::get_latest_grade_record(conn, *submission_id, question_id)
            })
            .map_err(|error| error.to_string())?;
        if existing.is_some() {
            continue;
        }
        let Ok(id) = platform.ids().next_ulid() else {
            return Err("判分记录 id 生成失败".to_owned());
        };
        let awarded = MilliScore::from_milli(2_000).unwrap_or(MilliScore::ZERO);
        let record = GradeRecord {
            id: GradeId::from_ulid(id),
            submission_id: *submission_id,
            question_id,
            revision: 1,
            mode: GradeMode::Standard,
            route: RouteDecision::RuleThenModel,
            route_trace: RouteTrace {
                rule_id: "R7".to_owned(),
                inputs_hash: Sha256Hex::of(b"s1-fixture"),
                classifier_call: None,
                decided_at: now,
            },
            judgement: Judgement::Partial {
                score: awarded,
                satisfied: Vec::new(),
                missing: Vec::new(),
                reason: "S1 夹具判分（识别/判分流水线未接入）".to_owned(),
            },
            awarded,
            max_score,
            confidence: Confidence::new(0.62).unwrap_or(Confidence::ZERO),
            status: GradeStatus::NeedsReview,
            evidence: vec![EvidenceRef::Asset(AssetId::of(
                format!("s1-fixture-{index}").as_bytes(),
            ))],
            engines: Vec::new(),
            created_at: now,
            reviewed_at: None,
            review_note: None,
            sampled: false,
        };
        platform
            .db()
            .write(move |tx| {
                eg_platform::repo::append_grade_record_with_reasons(
                    tx,
                    &record,
                    &[eg_domain::ReviewReasonKind::LowConfidence],
                )
            })
            .map_err(|error| error.to_string())?;
        written += 1;
    }
    // 额外为第一个提交写一条 fast+sampled 修订：用于「抽检不通过 → 整卷重跑」真实任务断言。
    if let Some(first) = submissions.first() {
        let Ok(id) = platform.ids().next_ulid() else {
            return Ok(written);
        };
        let awarded = MilliScore::from_milli(2_000).unwrap_or(MilliScore::ZERO);
        let record = GradeRecord {
            id: GradeId::from_ulid(id),
            submission_id: *first,
            question_id,
            revision: 2,
            mode: GradeMode::Fast,
            route: RouteDecision::RuleThenModel,
            route_trace: RouteTrace {
                rule_id: "R7".to_owned(),
                inputs_hash: Sha256Hex::of(b"s1-fixture-fast"),
                classifier_call: None,
                decided_at: now,
            },
            judgement: Judgement::Partial {
                score: awarded,
                satisfied: Vec::new(),
                missing: Vec::new(),
                reason: "S1 夹具快速模式抽检记录".to_owned(),
            },
            awarded,
            max_score,
            confidence: Confidence::new(0.62).unwrap_or(Confidence::ZERO),
            // 抽检项要求 Fast + sampled + AutoAccepted（eg-review 抽样契约）。
            status: GradeStatus::AutoAccepted,
            evidence: vec![EvidenceRef::Asset(AssetId::of(b"s1-fixture-fast"))],
            engines: Vec::new(),
            created_at: now,
            reviewed_at: None,
            review_note: None,
            sampled: true,
        };
        platform
            .db()
            .write(move |tx| {
                eg_platform::repo::append_grade_record_with_reasons(
                    tx,
                    &record,
                    &[eg_domain::ReviewReasonKind::LowConfidence],
                )
            })
            .map_err(|error| error.to_string())?;
        written += 1;
    }
    Ok(written)
}

/// 冒烟链路：夹具落库 → 复核改分 → 导出；返回（汇总文本, 导出摘要）。
pub(crate) fn run_smoke_chain(
    platform: &Platform,
) -> (String, Option<String>, Option<ImportReportVm>) {
    let mut lines = Vec::new();
    match seed_smoke_fixtures(platform) {
        Ok(source) => lines.push(format!("基础夹具：{source}")),
        Err(error) => return (format!("夹具落库失败：{error}"), None, None),
    }
    let mut import_view: Option<ImportReportVm> = None;
    let mut regrade_submission: Option<eg_domain::SubmissionId> = None;
    let fixture_paths = fixture_image_paths();
    if fixture_paths.is_empty() {
        lines.push(
            "fixtures/paper-20q 未就绪：使用内置夹具（真实 import+persist 路径跳过，见 app.md §8.7）"
                .to_owned(),
        );
    } else {
        match import_core(platform, &fixture_paths) {
            Ok((outcome, summary)) => {
                let report = &outcome.report;
                import_view = Some(import_vm(report, &summary));
                let submission_ids: Vec<eg_domain::SubmissionId> =
                    outcome.submissions.iter().map(|s| s.id).collect();
                regrade_submission = submission_ids.first().copied();
                let graded = append_fixture_grades(platform, &submission_ids).unwrap_or(0);
                lines.push(format!(
                    "真实 import+persist：文件 {}（成功 {} / 失败 {} / 拒收页 {} / 重复 {}，命名提醒 shown={}）\
                     → 提交 {} / 页 {} / 资产链接 {} / 审计 {}；补夹具判分 {graded} 条",
                    fixture_paths.len(),
                    report.files_ok,
                    report.files_failed,
                    report.pages_rejected,
                    report.duplicates.len(),
                    report.naming_reminder_shown,
                    summary.submissions,
                    summary.pages,
                    summary.asset_links,
                    summary.audit_entries
                ));
                let enqueued = platform.tasks().enqueue(
                    eg_platform::NewTask::new(
                        eg_platform::TaskKind::SubmissionGrade,
                        serde_json::json!({"kind": "grade", "submissions": submission_ids.len()}),
                    )
                    .with_scope(vec!["s1-fixture-grade".to_owned()]),
                );
                if let Err(error) = &enqueued {
                    lines.push(format!("补判分任务失败：{error}"));
                }
                let task_rows = load_tasks(platform);
                lines.push(format!(
                    "任务队列：{} 条（kind/stage：{}）",
                    task_rows.len(),
                    task_rows
                        .iter()
                        .map(|row| format!("{}/{}", row.kind, row.stage))
                        .collect::<Vec<_>>()
                        .join("，")
                ));
                let multi_page_groups = report
                    .groups
                    .iter()
                    .filter(|group| group.page_count > 1)
                    .count();
                let single_page_groups = report.groups.len().saturating_sub(multi_page_groups);
                lines.push(format!(
                    "归组：{} 组（多页 {multi_page_groups} / 单页 {single_page_groups}，警告 {}）；\
                     提交 {} / 页 {}；明细：{}",
                    report.groups.len(),
                    report.group_warnings.len(),
                    report.submissions.len(),
                    report.pages_total,
                    report
                        .groups
                        .iter()
                        .map(|group| format!(
                            "{}页/{}文件/页序{:?}",
                            group.page_count, group.file_count, group.page_order
                        ))
                        .collect::<Vec<_>>()
                        .join("，")
                ));
                let mut assertions = 0;
                let mut failures = Vec::new();
                assertions += 1;
                if !report.naming_reminder_shown {
                    failures.push("命名提醒未显示".to_owned());
                }
                assertions += 1;
                if report.files_ok == 0 {
                    failures.push("没有成功文件".to_owned());
                }
                assertions += 1;
                if report.duplicates.is_empty() {
                    failures.push("未命中重复字节（夹具应含重复）".to_owned());
                }
                assertions += 1;
                // 多页归组：9 文件 → 3 个多页组（各 2 页）+ 2 个单页组（EXIF/不规范命名），重复字节不建提交。
                if report.groups.len() != 5 || multi_page_groups != 3 || single_page_groups != 2 {
                    failures.push(format!(
                        "多页归组不符：组 {}（多页 {multi_page_groups} / 单页 {single_page_groups}），期望 5（3+2）",
                        report.groups.len()
                    ));
                }
                assertions += 1;
                if report.groups.iter().any(|group| {
                    group.page_count > 1
                        && (group.page_order != vec![1_u32, 2] || group.file_count != 2)
                }) {
                    failures.push("多页组页序/文件数不符（期望页序 1,2、文件数 2）".to_owned());
                }
                assertions += 1;
                if report.submissions.len() != report.groups.len() {
                    failures.push(format!(
                        "提交数与组数不一致：提交 {} / 组 {}",
                        report.submissions.len(),
                        report.groups.len()
                    ));
                }
                assertions += 1;
                if report.pages_total != 8 {
                    failures.push(format!(
                        "页数不符：{}（9 文件含 1 重复，期望 8 页）",
                        report.pages_total
                    ));
                }
                if failures.is_empty() {
                    lines.push(format!("冒烟断言：{assertions}/{assertions} 通过"));
                } else {
                    lines.push(format!("冒烟断言失败：{}", failures.join("；")));
                }
            }
            Err(error) => lines.push(format!("真实导入失败（已容错继续）：{error}")),
        }
    }
    let (ids, rows, counts) = review_queue(platform);
    lines.push(format!("任务/复核队列：{} 条（{counts}）", rows.len()));
    // 重判：修文本（单题）与抽检不通过（整卷）→ 真实 platform 任务（失败保留原判）。
    let tasks_before = load_tasks(platform).len();
    let mut regrade_ok = 0_u32;
    if let Some(grade_id) = ids.first() {
        match review_execute(
            platform,
            *grade_id,
            "regrade_text",
            None,
            None,
            Some("修正文本"),
        ) {
            Event::ReviewApplied { notice, .. } => {
                regrade_ok += 1;
                lines.push(format!("修文本重判：{notice}"));
            }
            Event::ReviewNotice { message } => lines.push(format!("修文本重判被拒：{message}")),
            other => lines.push(format!("修文本重判：{other:?}")),
        }
    }
    if let Some((submission_id, question_id)) =
        regrade_submission.zip(first_question(platform).map(|(question_id, _)| question_id))
        && let Some(fast) = platform
            .db()
            .read(|conn| {
                eg_platform::repo::get_latest_grade_record(conn, submission_id, question_id)
            })
            .ok()
            .flatten()
            .filter(|record| record.mode == GradeMode::Fast && record.sampled)
    {
        match review_execute(platform, fast.id, "regrade_whole", None, None, None) {
            Event::ReviewApplied { notice, .. } => {
                regrade_ok += 1;
                lines.push(format!("整卷重跑：{notice}"));
            }
            Event::ReviewNotice { message } => lines.push(format!("整卷重跑被拒：{message}")),
            other => lines.push(format!("整卷重跑：{other:?}")),
        }
    }
    let tasks_after = load_tasks(platform).len();
    lines.push(format!(
        "重判任务：新增 {} 条（{} → {}；修文本+整卷各 1 条）",
        tasks_after.saturating_sub(tasks_before),
        tasks_before,
        tasks_after
    ));
    lines.push(if regrade_ok == 2 {
        "重判断言：2/2 通过（命令已登记 platform 任务，原判未改）".to_owned()
    } else {
        format!("重判断言失败：{regrade_ok}/2（命令未登记任务）")
    });
    if let Some(grade_id) = ids.first() {
        let record = platform
            .db()
            .read(|conn| eg_platform::repo::get_grade_record(conn, *grade_id))
            .ok()
            .flatten();
        let score = record.as_ref().map_or_else(String::new, |record| {
            record.max_score.as_milli().to_string()
        });
        let event = review_execute(platform, *grade_id, "override", Some(&score), Some(2), None);
        match event {
            Event::ReviewApplied { notice, .. } => lines.push(format!("复核改分：{notice}")),
            Event::ReviewNotice { message } => lines.push(format!("复核改分被拒：{message}")),
            other => lines.push(format!("复核改分：{other:?}")),
        }
    }
    // 撤销/重做持久化往返（platform v4 redo 表）：撤销→重做→撤销；新命令后 redo 失效。
    let mut redo_checks = Vec::new();
    if let Some(grade_id) = ids.first() {
        let _ = review_execute(platform, *grade_id, "undo", None, None, None);
        let after_undo = review_history(platform).1;
        let _ = review_execute(platform, *grade_id, "redo", None, None, None);
        let after_redo = review_history(platform).1;
        let _ = review_execute(platform, *grade_id, "undo", None, None, None);
        let after_second_undo = review_history(platform).1;
        if let Some(other) = ids.get(1) {
            let _ = review_execute(platform, *other, "accept", None, None, None);
        }
        let after_new_command = review_history(platform).1;
        let ok =
            after_undo >= 1 && after_redo == 0 && after_second_undo >= 1 && after_new_command == 0;
        redo_checks.push(ok);
        lines.push(format!(
            "redo 往返：撤销后 redo={after_undo}，重做后 redo={after_redo}，再撤销 redo={after_second_undo}，新命令后 redo={after_new_command}（{}）",
            if ok { "通过" } else { "失败" }
        ));
    }
    lines.push(if redo_checks.iter().all(|ok| *ok) && !redo_checks.is_empty() {
        "redo 断言：通过（持久化语义：撤销→push_redo、重做→pop_redo+push_undo、新命令 clear_redo）".to_owned()
    } else {
        "redo 断言失败".to_owned()
    });

    // structurer：原文→严格 Schema 草稿（ModelStructured，不生效）；故障注入→显式失败（至多 1 次修复）。
    let mut rules = RulesBundle::new();
    let structured = rules.structured("每题按步骤给分，每步 2 分（夹具整理）");
    let structurer_ok =
        matches!(&structured, Event::RulesDraft { model_call_id, .. } if !model_call_id.is_empty());
    let invalid = rules.structured("!invalid");
    let structurer_failed = matches!(invalid, Event::RulesFailed { .. });
    lines.push(format!(
        "structurer：合法原文→{}；故障注入→{}",
        if structurer_ok {
            "草稿（ModelStructured）"
        } else {
            "失败"
        },
        if structurer_failed {
            "显式失败"
        } else {
            "未失败"
        }
    ));
    lines.push(if structurer_ok && structurer_failed {
        "structurer 断言：2/2 通过（只产草稿，永不生效）".to_owned()
    } else {
        "structurer 断言失败".to_owned()
    });

    let mut exported = None;
    match export_report(platform) {
        Ok(summary) => {
            lines.push(summary.clone());
            exported = Some(summary);
        }
        Err(error) => lines.push(format!("导出失败：{error}")),
    }
    (lines.join("；"), exported, import_view)
}

// ---------------------------------------------------------------------------
// structurer 端口：内置 fixture 后端（无真实模型；不静默降级到其他模型）
// ---------------------------------------------------------------------------

/// 内置 fixture structurer：按固定 Schema 生成 1 个评分项（分值 = 题满分）。
///
/// 真实模型接入 TODO：用 `eg_inference::InferenceService::complete` +
/// `ExpectedSchema::Structurer` 替换本实现；未配置模型时 rules 页显式失败。
struct FixtureStructurer;

impl StructurerPort for FixtureStructurer {
    fn structure(
        &self,
        invocation: &StructurerInvocation<'_>,
    ) -> eg_rubric::Result<StructurerReply> {
        let raw = invocation.request.raw_text;
        // 测试/冒烟故障注入：原文含 `!invalid` 时返回不符合 Schema 的输出（验证 ≤1 次修复后显式失败）。
        let invalid = raw.contains("!invalid");
        let criterion = raw
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("评分项")
            .chars()
            .take(80)
            .collect::<String>();
        let payload = if invalid {
            serde_json::json!({ "items": [] }).to_string()
        } else {
            serde_json::json!({
                "items": [{
                    "criterion": criterion,
                    "points": invocation.request.max_score.as_milli(),
                    "required": true,
                    "depends_on": []
                }]
            })
            .to_string()
        };
        // 模型调用 id：由提示词哈希与原文确定性派生（fixture 无真实调用，但仍可复现）。
        let hash = invocation.prompt.hash.to_hex();
        let bytes = hash.as_bytes();
        let mut value = [0_u8; 16];
        for (index, byte) in bytes.iter().take(32).enumerate() {
            value[index % 16] ^= *byte;
        }
        let model_call_id =
            eg_domain::ModelCallId::from_ulid(Ulid::from_parts(0, u128::from_be_bytes(value)));
        Ok(StructurerReply {
            model_call_id,
            raw_output: payload,
        })
    }
}
