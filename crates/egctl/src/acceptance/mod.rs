//! `egctl acceptance`：S1 端到端自动化验收（backend 层，不依赖真实模型/网络/桌面）。
//!
//! 链路（每一步都执行真实模块，fixture 后端替代模型）：
//! 临时数据根 + migrations → capture 导入（真实 PNG、重复字节、名单外文件名）→
//! recognize（scripted OCR，含畸形输出负向断言）→ align（文件名身份、区域/答案）→
//! grade（fixture judge，逐题断言路由/判定/得分/状态）→ review（改分 + 撤销 + 抽样只读负向）→
//! report（XLSX/CSV/HTML + 导出记录 + 重放摘要一致）→ 汇总 JSON。
//!
//! 诚实声明：真实模型（OCR/judge）、真实手写试卷、UI 键鼠链路不在本命令范围；
//! 外部 `fixtures/` 尚未提供可解析的期望契约时，使用内置 20 题夹具，并在报告中注明 TODO。

pub mod external;
pub mod paper;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eg_align::{AlignRequest, AlignSettings, MapRecognitionSource, Roster, SeqIds, align};
use eg_capture::platform_bridge::{AssetStoreAdapter, ClockAdapter};
use eg_capture::{
    DuplicatePolicy, FileInput, ImportKind, ImportOptions, ImportRequest, Importer,
    MemoryEventSink, NeverCancel, NoKnownSubmissions, PersistRequest, SeqIdSource, persist,
};
use eg_domain::{
    AssetId, ClassroomId, EvidenceRef, GradeMode, GradeRecord, GradeStatus, MilliScore, Paper,
    SubmissionId, SubmissionStatus, TaskId, Timestamp, Ulid,
};
use eg_grade::ports::{
    FixtureRubrics, InMemoryIdempotency, ScriptedJudge, SequentialIds as GradeIds,
};
use eg_grade::{GradeConfig, Grader, SubmissionRequest};
use eg_platform::config::ConfigDocument;
use eg_platform::{DataDirs, Platform, SystemClock, repo};
use eg_recognize::page::recognize_page;
use eg_recognize::testing::{MemoryRecognizeIo, ScriptedBackend, white_image};
use eg_recognize::types::RecognizeIo;
use eg_review::ports::{
    FixtureRegradeEngine, InMemoryAuditSink, InMemoryReviewRepository, InMemoryUndoStore,
};
use eg_review::{
    CommandEnvelope, CommandOutcome, ReviewCommand, ReviewConfig, ReviewReasonCode,
    ReviewRepository, ReviewService,
};
use serde_json::json;

/// acceptance 选项。
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// 外部夹具目录（`fixtures/paper-20q`）；`None` = 内置夹具。
    pub fixtures: Option<PathBuf>,
    /// 数据根（`None` = 系统临时目录；调用方负责清理）。
    pub data_dir: Option<PathBuf>,
    /// 结果 JSON 输出文件（可选）。
    pub out: Option<PathBuf>,
}

/// 单个步骤结果。
#[derive(Debug, Clone)]
pub struct StepResult {
    /// 步骤 id。
    pub id: String,
    /// 状态（PASS/WARN/FAIL）。
    pub status: &'static str,
    /// 断言数。
    pub assertions: usize,
    /// 失败数。
    pub failed: usize,
    /// 耗时毫秒。
    pub elapsed_ms: u128,
    /// 摘要。
    pub detail: String,
}

/// 汇总报告。
#[derive(Debug, Clone)]
pub struct AcceptanceReport {
    /// 是否 PASS（无失败断言）。
    pub passed: bool,
    /// 总断言数。
    pub assertions: usize,
    /// 失败断言。
    pub failures: Vec<String>,
    /// 警告（未就绪/未覆盖项，不阻断 PASS）。
    pub warnings: Vec<String>,
    /// 负向用例 id 列表。
    pub negatives: Vec<String>,
    /// 步骤。
    pub steps: Vec<StepResult>,
    /// 总耗时毫秒。
    pub duration_ms: u128,
    /// 数据根。
    pub data_root: PathBuf,
    /// 产物路径。
    pub artifacts: Vec<PathBuf>,
    /// 外部夹具说明。
    pub fixture_note: String,
}

impl AcceptanceReport {
    /// JSON 输出。
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        json!({
            "tool": "egctl",
            "command": "acceptance",
            "scenario": "S1",
            "status": if self.passed { "pass" } else { "fail" },
            "assertions": self.assertions,
            "failures": self.failures,
            "warnings": self.warnings,
            "negatives": self.negatives,
            "duration_ms": self.duration_ms,
            "data_root": self.data_root.display().to_string(),
            "artifacts": self.artifacts.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
            "fixture_note": self.fixture_note,
            "steps": self.steps.iter().map(|step| json!({
                "id": step.id,
                "status": step.status,
                "assertions": step.assertions,
                "failed": step.failed,
                "elapsed_ms": step.elapsed_ms,
                "detail": step.detail,
            })).collect::<Vec<_>>(),
        })
    }

    /// 人类可读输出。
    #[must_use]
    pub fn render_human(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "S1 端到端验收：{}\n断言 {} 条；失败 {} 条；耗时 {} ms\n数据根 {}\n\n",
            if self.passed { "PASS" } else { "FAIL" },
            self.assertions,
            self.failures.len(),
            self.duration_ms,
            self.data_root.display()
        ));
        for step in &self.steps {
            out.push_str(&format!(
                "[{:<4}] {:<14} 断言 {:>3} / 失败 {:>2} / {:>5} ms  {}\n",
                step.status, step.id, step.assertions, step.failed, step.elapsed_ms, step.detail
            ));
        }
        if !self.negatives.is_empty() {
            out.push_str("\n负向用例：\n");
            for negative in &self.negatives {
                out.push_str(&format!("  - {negative}\n"));
            }
        }
        if !self.warnings.is_empty() {
            out.push_str("\n警告（不阻断 PASS，但 S1 产品级验收未覆盖）：\n");
            for warning in &self.warnings {
                out.push_str(&format!("  - {warning}\n"));
            }
        }
        if !self.failures.is_empty() {
            out.push_str("\n失败断言：\n");
            for failure in &self.failures {
                out.push_str(&format!("  - {failure}\n"));
            }
        }
        if !self.artifacts.is_empty() {
            out.push_str("\n产物：\n");
            for artifact in &self.artifacts {
                out.push_str(&format!("  - {}\n", artifact.display()));
            }
        }
        out
    }
}

/// 断言累加器。
#[derive(Default)]
pub(crate) struct Asserts {
    total: usize,
    failures: Vec<String>,
    negatives: Vec<String>,
}

impl Asserts {
    fn check(&mut self, name: &str, condition: bool, detail: impl Into<String>) {
        self.total += 1;
        if !condition {
            self.failures.push(format!("{name}：{}", detail.into()));
        }
    }

    fn negative(&mut self, name: &str, condition: bool, detail: impl Into<String>) {
        self.negatives.push(name.to_owned());
        self.check(name, condition, detail);
    }
}

/// 执行验收。
#[must_use]
pub fn run(options: &Options) -> AcceptanceReport {
    let started = Instant::now();
    let data_root = options.data_dir.clone().unwrap_or_else(|| {
        std::env::temp_dir().join(format!("egctl-acceptance-{}", std::process::id()))
    });
    if let Err(error) = std::fs::create_dir_all(&data_root) {
        return failed_report(&data_root, format!("创建临时数据根失败：{error}"), started);
    }
    let mut asserts = Asserts::default();
    let mut steps: Vec<StepResult> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut artifacts: Vec<PathBuf> = Vec::new();

    // 步骤 1：临时数据根 + migrations。
    let mut step = Step::new("env");
    let platform = match Platform::open(
        DataDirs::with_root(&data_root).unwrap_or_else(|_| unreachable_dirs()),
    ) {
        Ok(platform) => {
            let schema = platform.db().schema_version().ok();
            step.assert(
                &mut asserts,
                "schema_v3",
                schema.is_some_and(|version| version >= 3),
                format!("schema={schema:?}"),
            );
            step.assert(
                &mut asserts,
                "audit_chain_ok",
                platform
                    .audit()
                    .verify_chain()
                    .is_ok_and(|verification| verification.ok),
                "审计链校验",
            );
            step.assert(
                &mut asserts,
                "config_valid",
                ConfigDocument {
                    meta: platform.config_document().meta.clone(),
                    config: platform.config().clone(),
                }
                .config
                .validate()
                .is_ok(),
                "配置校验",
            );
            Some(platform)
        }
        Err(error) => {
            step.assert(&mut asserts, "platform_open", false, error.to_string());
            None
        }
    };
    // 为 capture 落库准备基础数据（FK：classroom/student/paper/question）。
    if let Some(platform) = &platform {
        let classroom = eg_domain::Classroom {
            id: ClassroomId::from_ulid(Ulid::from_u128(2)),
            name: "验收班级".to_owned(),
            term: "2026-2027-1".to_owned(),
        };
        let roster = students();
        let paper_domain = paper::paper();
        let seeded = platform.db().write(move |tx| {
            repo::insert_classroom(tx, &classroom, Some("acceptance"), paper::ts())?;
            for student in &roster {
                repo::insert_student(tx, student, paper::ts())?;
            }
            repo::insert_paper(tx, &paper_domain)?;
            Ok(())
        });
        step.assert(
            &mut asserts,
            "seed_base_rows",
            seeded.is_ok(),
            format!("{:?}", seeded.err()),
        );
        step.detail = "migrations + 基础行落库".to_owned();
    }
    steps.push(step.finish());
    let Some(platform) = platform else {
        return finish_report(
            data_root,
            asserts,
            steps,
            warnings,
            artifacts,
            started,
            String::new(),
        );
    };

    // 步骤 2：夹具（外部契约校验 vs 内置夹具）。
    let mut step = Step::new("fixtures");
    let fixture_note = match &options.fixtures {
        Some(dir) if dir.is_dir() => {
            let checked = validate_fixtures(dir, &mut asserts, &mut step, &mut warnings);
            format!("外部夹具 {} {checked}", dir.display())
        }
        Some(dir) => {
            step.assert(
                &mut asserts,
                "fixtures_dir_exists",
                false,
                format!("{} 不存在", dir.display()),
            );
            format!("外部夹具缺失：{}；使用内置夹具", dir.display())
        }
        None => "未指定外部夹具；使用内置 20 题夹具".to_owned(),
    };
    steps.push(step.finish());

    // 步骤 3：capture 导入（真实 PNG + 重复字节 + 名单外文件名）。
    let mut step = Step::new("capture");
    let students = students();
    let paper = paper::paper();
    let student_images: Vec<(String, Vec<u8>)> = students
        .iter()
        .map(|student| (student.name.clone(), png_for(&student.name)))
        .collect();
    let outside_image = png_for("名单外学生");
    let mut images_by_stem: BTreeMap<String, Vec<u8>> = student_images.iter().cloned().collect();
    images_by_stem.insert("名单外学生".to_owned(), outside_image);
    let mut files: Vec<FileInput> = student_images
        .iter()
        .map(|(name, bytes)| FileInput::from_bytes(format!("{name}.png"), bytes.clone()))
        .collect();
    files.push(FileInput::from_bytes(
        "名单外学生.png",
        png_for("名单外学生"),
    ));
    // 重复字节（与张三同内容）：Skip 策略下应被识别为重复而不新建提交。
    if let Some((_, bytes)) = student_images.first() {
        files.push(FileInput::from_bytes("张三-副本.png", bytes.clone()));
    }
    let settings = eg_capture::ImportSettings::default();
    let sink = AssetStoreAdapter::new(platform.assets().as_ref());
    let events = MemoryEventSink::new();
    let clock = SystemClock;
    let clock_port = ClockAdapter::new(&clock);
    let ids = SeqIdSource::with_start(1);
    let cancel = NeverCancel;
    let known = NoKnownSubmissions;
    let importer = Importer::new(
        &settings,
        &sink,
        &events,
        &clock_port,
        &ids,
        &cancel,
        &known,
    );
    let outcome = importer.run(ImportRequest {
        batch_id: Ulid::from_u128(70_001),
        kind: ImportKind::StudentWork,
        classroom_id: paper.classroom_id,
        paper_id: paper.id,
        files,
        options: ImportOptions {
            duplicate_policy: DuplicatePolicy::Skip,
        },
    });
    let mut submissions: Vec<eg_domain::Submission> = Vec::new();
    match outcome {
        Ok(outcome) => {
            step.assert(
                &mut asserts,
                "capture_submissions",
                outcome.submissions.len() == 4,
                format!(
                    "提交数 {}（文件 {}）",
                    outcome.submissions.len(),
                    outcome.report.files_total
                ),
            );
            step.assert(
                &mut asserts,
                "capture_duplicate_skipped",
                !outcome.report.duplicates.is_empty(),
                format!("重复命中 {}", outcome.report.duplicates.len()),
            );
            step.assert(
                &mut asserts,
                "capture_pages",
                outcome
                    .submissions
                    .iter()
                    .all(|submission| !submission.pages.is_empty()),
                "每份提交至少 1 页",
            );
            submissions = outcome.submissions.clone();
            persists(&platform, &outcome, &mut step, &mut asserts);
            artifacts.push(data_root.join("assets"));
        }
        Err(error) => {
            step.assert(&mut asserts, "capture_import", false, error.to_string());
        }
    }
    steps.push(step.finish());

    // 步骤 4：recognize（scripted OCR 回放 + 畸形输出负向）。
    let mut step = Step::new("recognize");
    let mut recognitions: BTreeMap<SubmissionId, eg_domain::Recognition> = BTreeMap::new();
    let ocr_config = eg_recognize::testing::sample_config("fixture-ocr");
    for (index, submission) in submissions.iter().enumerate() {
        let Some(page) = submission.pages.first() else {
            continue;
        };
        let Some(image_bytes) = images_by_stem.get(&submission.source_stem) else {
            continue;
        };
        let answer_texts = ocr_blocks_texts(index);
        let blocks: Vec<serde_json::Value> = answer_texts
            .iter()
            .enumerate()
            .map(|(index, text)| {
                let step = if answer_texts.is_empty() {
                    0.0
                } else {
                    0.9 / f64::from(u32::try_from(answer_texts.len()).unwrap_or(1))
                };
                let y = 0.04 + step * f64::from(u32::try_from(index).unwrap_or(0));
                json!({
                    "kind": "text",
                    "bbox": [0.05, y, 0.9, step.max(0.01)],
                    "text": format!("{} {}", paper.questions[index].label, text),
                    "confidence": 0.95,
                })
            })
            .collect();
        let raw = serde_json::to_string(&json!({
            "schema_version": 1,
            "page": {"width": 640, "height": 960},
            "blocks": blocks,
        }))
        .unwrap_or_default();
        let backend = ScriptedBackend::new("fixture-ocr", "1");
        backend.push_raw(raw);
        let io = MemoryRecognizeIo::new();
        io.images.insert_page(
            page.id,
            white_image(image_bytes, page.width.max(200), page.height.max(300)),
        );
        let clock = SystemClock;
        let cancel = eg_platform::CancelToken::new();
        let recognize_io = RecognizeIo {
            backend: &backend,
            images: &io.images,
            cache: &io.cache,
            repo: &io.repo,
            clock: &clock,
            ids: &io.ids,
            cancel: &cancel,
        };
        match recognize_page(&recognize_io, page, &ocr_config, paper::task_id()) {
            Ok(result) => {
                step.assert(
                    &mut asserts,
                    "recognize_blocks_20",
                    result.recognition.blocks.len() >= 20,
                    format!("识别块 {}", result.recognition.blocks.len()),
                );
                step.assert(
                    &mut asserts,
                    "recognize_confidence",
                    result.confidence.page.get() > 0.0,
                    format!("页置信度 {:?}", result.confidence.page),
                );
                recognitions.insert(submission.id, result.recognition);
            }
            Err(error) => {
                step.assert(
                    &mut asserts,
                    "recognize_page",
                    false,
                    format!("识别失败：{error}"),
                );
            }
        }
    }
    // 负向：畸形 OCR 输出必须显式失败（不得静默产出空识别）。
    if let Some(submission) = submissions.first()
        && let Some(page) = submission.pages.first()
    {
        let backend = ScriptedBackend::new("fixture-ocr", "1");
        backend.push_raw("{\"blocks\":\"not-an-array\"}");
        let io = MemoryRecognizeIo::new();
        io.images
            .insert_page(page.id, white_image(b"malformed", 800, 1200));
        let clock = SystemClock;
        let cancel = eg_platform::CancelToken::new();
        let recognize_io = RecognizeIo {
            backend: &backend,
            images: &io.images,
            cache: &io.cache,
            repo: &io.repo,
            clock: &clock,
            ids: &io.ids,
            cancel: &cancel,
        };
        let malformed = recognize_page(&recognize_io, page, &ocr_config, paper::task_id());
        step.negative(
            &mut asserts,
            "negative_malformed_ocr",
            malformed.is_err(),
            "畸形 OCR 应返回显式错误（人工/失败），不得静默成功",
        );
    }
    steps.push(step.finish());

    // 步骤 5：align（文件名身份 + 区域/答案；名单外未决负向）。
    let mut step = Step::new("align");
    let roster = Roster::new(paper.classroom_id, students.clone());
    let mut aligned_by_student: BTreeMap<SubmissionId, eg_align::AlignReport> = BTreeMap::new();
    match roster {
        Ok(roster) => {
            let align_settings = AlignSettings::default();
            let hint = eg_align::NoIdentityHints;
            let align_clock = eg_align::testing::FixedClock::new(paper::ts());
            for submission in &submissions {
                let Some(recognition) = recognitions.get(&submission.id) else {
                    continue;
                };
                let source = MapRecognitionSource::new(vec![recognition.clone()]);
                let Ok(source) = source else {
                    continue;
                };
                let pages = eg_align::testing::page_refs(submission);
                let ids = SeqIds::new(1);
                match align(AlignRequest {
                    submission,
                    paper: &paper,
                    roster: &roster,
                    template: None,
                    pages: &pages,
                    recognitions: &source,
                    hints: &hint,
                    settings: &align_settings,
                    ids: &ids,
                    clock: &align_clock,
                    existing: &[],
                }) {
                    Ok(report) => {
                        let is_known = matches!(
                            report.identity.status,
                            eg_align::IdentityStatus::Confirmed { .. }
                        );
                        let expected_known = submission.source_stem != "名单外学生";
                        step.assert(
                            &mut asserts,
                            &format!("align_identity_{}", submission.source_stem),
                            is_known == expected_known,
                            format!("状态 {:?}", report.identity.status),
                        );
                        if is_known {
                            step.assert(
                                &mut asserts,
                                "align_answers_nonempty",
                                !report.answers.is_empty(),
                                format!("{} 个学生答案", report.answers.len()),
                            );
                        } else {
                            step.negative(
                                &mut asserts,
                                "negative_unresolved_identity",
                                !report.auto_grade_ready,
                                "名单外文件不得自动判分",
                            );
                        }
                        aligned_by_student.insert(submission.id, report);
                    }
                    Err(error) => {
                        step.assert(
                            &mut asserts,
                            "align_run",
                            false,
                            format!("{} 对齐失败：{error}", submission.source_stem),
                        );
                    }
                }
            }
        }
        Err(error) => {
            step.assert(&mut asserts, "roster", false, error.to_string());
        }
    }
    steps.push(step.finish());

    // 步骤 6：grade（fixture judge，逐题断言）。
    let mut step = Step::new("grade");
    let mut records_by_submission: BTreeMap<SubmissionId, Vec<GradeRecord>> = BTreeMap::new();
    let subject = eg_subject_math::MathSubjectPack::new();
    let rubrics = rubric_source();
    let config = GradeConfig::default();
    let grade_clock = eg_grade::ports::FixedClock::new(paper::ts());
    let grade_ids = GradeIds::new();
    let idempotency = InMemoryIdempotency::new();
    for (index, submission) in submissions.iter().enumerate() {
        if submission.source_stem == "名单外学生" {
            continue;
        }
        let judge = ScriptedJudge::new(paper::judge_steps());
        let grader = Grader {
            subject: &subject,
            rubrics: &rubrics,
            judge: &judge,
            classifier: None,
            config: &config,
            clock: &grade_clock,
            ids: &grade_ids,
            idempotency: &idempotency,
        };
        let request = SubmissionRequest {
            submission_id: submission.id,
            task_id: paper::task_id(),
            paper: paper.clone(),
            template: None,
            answers: paper::answers(),
            mode: GradeMode::Standard,
            backend_available: true,
            manual_only: Vec::new(),
            excluded: None,
            engine_stamps: Vec::new(),
        };
        match grader.grade_submission(&request) {
            Ok(grade) => {
                step.assert(
                    &mut asserts,
                    "grade_no_failures",
                    grade.failures.is_empty(),
                    format!("失败 {:?}", grade.failures),
                );
                step.assert(
                    &mut asserts,
                    "grade_total_20",
                    grade.records.len() == 20,
                    format!("记录 {}", grade.records.len()),
                );
                if index == 0 {
                    let expected = paper::expected();
                    for (position, graded) in grade.records.iter().enumerate() {
                        if let Some(expected) = expected.get(position) {
                            step.assert(
                                &mut asserts,
                                &format!("grade_q{}", position + 1),
                                paper::assert_graded(
                                    graded,
                                    *expected,
                                    &format!("Q{}", position + 1),
                                )
                                .is_ok(),
                                format!("Q{}（路由/判定/得分/状态）", position + 1),
                            );
                        }
                    }
                    step.negative(
                        &mut asserts,
                        "negative_multi_choice_partial_credit",
                        grade.records.get(4).is_some_and(|graded| {
                            graded.record.awarded.as_milli() > 0
                                && graded.record.awarded.as_milli() < 5_000
                        }),
                        "多选题部分给分应为 0 < 得分 < 满分（Q5）",
                    );
                    step.negative(
                        &mut asserts,
                        "negative_low_confidence_review",
                        grade
                            .records
                            .last()
                            .is_some_and(|graded| graded.record.status == GradeStatus::NeedsReview),
                        "低置信题必须转待复核（Q20）",
                    );
                }
                records_by_submission.insert(
                    submission.id,
                    grade
                        .records
                        .iter()
                        .map(|graded| graded.record.clone())
                        .collect(),
                );
                step.assert(
                    &mut asserts,
                    "grade_summary_counts",
                    grade.summary.total_questions == 20,
                    format!("汇总 {:?}", grade.summary),
                );
            }
            Err(error) => {
                step.assert(
                    &mut asserts,
                    "grade_run",
                    false,
                    format!("{} 判分失败：{error}", submission.source_stem),
                );
            }
        }
    }
    // 负向：judge 后端不可用时单题失败显式记录（不静默 0 分成功）。
    {
        let judge = ScriptedJudge::new(vec![
            paper::failure_step(),
            paper::failure_step(),
            paper::failure_step(),
            paper::failure_step(),
            paper::failure_step(),
            paper::failure_step(),
            paper::failure_step(),
            paper::failure_step(),
        ]);
        let grader = Grader {
            subject: &subject,
            rubrics: &rubrics,
            judge: &judge,
            classifier: None,
            config: &config,
            clock: &grade_clock,
            ids: &grade_ids,
            idempotency: &InMemoryIdempotency::new(),
        };
        let request = SubmissionRequest {
            submission_id: SubmissionId::from_ulid(Ulid::from_u128(99_001)),
            task_id: paper::task_id(),
            paper: paper.clone(),
            template: None,
            answers: paper::answers(),
            mode: GradeMode::Standard,
            backend_available: true,
            manual_only: Vec::new(),
            excluded: None,
            engine_stamps: Vec::new(),
        };
        if let Ok(grade) = grader.grade_submission(&request) {
            step.negative(
                &mut asserts,
                "negative_judge_unavailable",
                !grade.failures.is_empty() || grade.summary.needs_review > 0,
                format!(
                    "judge 不可用时必须显式记录（失败 {} / 待复核 {}）",
                    grade.failures.len(),
                    grade.summary.needs_review
                ),
            );
        }
    }
    steps.push(step.finish());

    // 步骤 7：review（改分 + 审计 + 撤销 + 抽样只读负向）。
    let mut step = Step::new("review");
    let review_repo = InMemoryReviewRepository::new();
    let audit_sink = InMemoryAuditSink::new();
    let undo_store = InMemoryUndoStore::new(50);
    let regrade = FixtureRegradeEngine::new(Vec::new());
    let review_clock = eg_grade::ports::FixedClock::new(paper::ts());
    let review_ids = GradeIds::new();
    let review_config = ReviewConfig::default();
    let service = ReviewService::new(
        &review_repo,
        &audit_sink,
        &undo_store,
        &regrade,
        &review_clock,
        &review_ids,
        &review_config,
    );
    let service = match service {
        Ok(service) => Some(service),
        Err(error) => {
            step.assert(&mut asserts, "review_service", false, error.to_string());
            None
        }
    };
    if let Some(first) = submissions.first()
        && let Some(records) = records_by_submission.get(&first.id)
        && let Some(target) = records.first()
    {
        review_repo.insert(target.clone());
        let envelope = CommandEnvelope {
            target: Some(target.id),
            idempotency_key: "acceptance-override-1".to_owned(),
            expected_revision: Some(target.revision),
            command: ReviewCommand::OverrideScore {
                score_milli: 4_500,
                reason: ReviewReasonCode::PartialCredit,
                note: Some("验收：人工改分".to_owned()),
            },
        };
        match service
            .as_ref()
            .ok_or_else(|| "review service 不可用".to_owned())
            .and_then(|service| {
                service
                    .execute(&envelope)
                    .map_err(|error| error.to_string())
            }) {
            Ok(result) => {
                match result.outcome {
                    CommandOutcome::Record { status, .. } => {
                        step.assert(
                            &mut asserts,
                            "review_override_status",
                            status == GradeStatus::ManuallyReviewed,
                            format!("{status:?}"),
                        );
                    }
                    other => {
                        step.assert(
                            &mut asserts,
                            "review_override_outcome",
                            false,
                            format!("{other:?}"),
                        );
                    }
                }
                let changed_score = review_repo
                    .revisions(target.submission_id, target.question_id)
                    .last()
                    .map_or(0, |record| record.awarded.as_milli());
                step.assert(
                    &mut asserts,
                    "review_override_score",
                    changed_score == 4_500,
                    format!("得分 {changed_score}"),
                );
                step.assert(
                    &mut asserts,
                    "review_audit_written",
                    !audit_sink.is_empty(),
                    format!("审计 {} 条", audit_sink.len()),
                );
            }
            Err(error) => {
                step.assert(&mut asserts, "review_override", false, error.clone());
            }
        }
        // 撤销（追加反向修订）。
        let undo_envelope = CommandEnvelope {
            target: None,
            idempotency_key: "acceptance-undo-1".to_owned(),
            expected_revision: None,
            command: ReviewCommand::Undo,
        };
        match service
            .as_ref()
            .ok_or_else(|| "review service 不可用".to_owned())
            .and_then(|service| {
                service
                    .execute(&undo_envelope)
                    .map_err(|error| error.to_string())
            }) {
            Ok(result) => {
                let undone = matches!(result.outcome, CommandOutcome::Undone { undone: true, .. });
                step.assert(
                    &mut asserts,
                    "review_undo_outcome",
                    undone,
                    format!("{:?}", result.outcome),
                );
                let latest_score = review_repo
                    .revisions(target.submission_id, target.question_id)
                    .last()
                    .map_or(0, |record| record.awarded.as_milli());
                step.assert(
                    &mut asserts,
                    "review_undo_restores_score",
                    latest_score == target.awarded.as_milli(),
                    format!(
                        "撤销后得分 {latest_score}（原 {}）",
                        target.awarded.as_milli()
                    ),
                );
                step.assert(
                    &mut asserts,
                    "review_audit_after_undo",
                    audit_sink.len() >= 2,
                    format!("审计 {} 条", audit_sink.len()),
                );
            }
            Err(error) => {
                step.assert(&mut asserts, "review_undo", false, error.clone());
            }
        }
        // 负向：快速抽样记录只读，改分必须被拒绝。
        let mut sampled = target.clone();
        sampled.id = eg_domain::GradeId::from_ulid(Ulid::from_u128(95_000));
        sampled.mode = GradeMode::Fast;
        sampled.sampled = true;
        sampled.status = GradeStatus::AutoAccepted;
        review_repo.insert(sampled.clone());
        let sampled_envelope = CommandEnvelope {
            target: Some(sampled.id),
            idempotency_key: "acceptance-sampled-edit".to_owned(),
            expected_revision: Some(sampled.revision),
            command: ReviewCommand::OverrideScore {
                score_milli: 1_000,
                reason: ReviewReasonCode::PartialCredit,
                note: None,
            },
        };
        let rejected = service
            .as_ref()
            .is_some_and(|service| service.execute(&sampled_envelope).is_err());
        step.negative(
            &mut asserts,
            "negative_sampled_readonly",
            rejected,
            "快速抽样记录（sampled）编辑命令必须拒绝",
        );
    }
    steps.push(step.finish());

    // 步骤 8：report（三格式导出 + 导出记录 + 重放一致）。
    let mut step = Step::new("report");
    let report_input = report_input(
        &platform,
        &paper,
        &students,
        &submissions,
        &records_by_submission,
        &review_repo,
    );
    match eg_report::Report::build(&report_input, eg_report::ReportSettings::default()) {
        Ok(report) => {
            step.assert(
                &mut asserts,
                "report_build",
                !report.students.is_empty(),
                format!("成绩单行 {}", report.students.len()),
            );
            let run_a = data_root.join("exports/acceptance-a");
            let run_b = data_root.join("exports/acceptance-b");
            let exported_at = paper::ts();
            match (
                egctl_fixture_export(&report, &run_a, exported_at),
                egctl_fixture_export(&report, &run_b, exported_at),
            ) {
                (Ok(first), Ok(second)) => {
                    step.assert(
                        &mut asserts,
                        "export_files_10",
                        first.files.len() == 10,
                        format!("文件 {}", first.files.len()),
                    );
                    step.assert(
                        &mut asserts,
                        "export_replay_digest",
                        first.content_digest == second.content_digest
                            && eg_report::verify_replay(&first, &second).is_ok(),
                        "两次导出内容摘要一致",
                    );
                    artifacts.push(run_a.clone());
                    artifacts.push(run_b.clone());
                    // 导出记录 + 审计落库。
                    let record = repo::ExportRecord {
                        id: Ulid::from_u128(96_001),
                        kind: "paper_report".to_owned(),
                        format: "xlsx+csv+html".to_owned(),
                        scope: json!({"mode":"standard","masked":false,"naming_rule_version":first.naming_rule_version}),
                        summary_hash: first.content_digest,
                        row_count: u64::try_from(first.row_count).unwrap_or(0),
                        body: json!({"files": first.files.len(), "snapshot_hash": first.snapshot_hash.to_hex()}),
                        created_at: paper::ts(),
                        audit_id: None,
                    };
                    let audited = platform.audit().append(
                        eg_platform::NewAuditEntry::new(
                            "report.export",
                            "export_record",
                            record.id.to_string(),
                        )
                        .with_note(format!("acceptance export（{} 行）", record.row_count)),
                    );
                    match audited {
                        Ok(entry) => {
                            let mut record = record;
                            record.audit_id = Some(entry.id);
                            let record_for_write = record.clone();
                            let inserted = platform
                                .db()
                                .write(move |tx| repo::insert_export_record(tx, &record_for_write));
                            step.assert(
                                &mut asserts,
                                "export_record_insert",
                                inserted.is_ok(),
                                format!("{:?}", inserted.err()),
                            );
                            let listed = platform
                                .db()
                                .read(|conn| repo::list_export_records(conn, 10));
                            step.assert(
                                &mut asserts,
                                "export_record_list",
                                listed.as_ref().is_ok_and(|records| !records.is_empty()),
                                format!("导出记录 {:?}", listed.map(|records| records.len())),
                            );
                        }
                        Err(error) => {
                            step.assert(&mut asserts, "export_audit", false, error.to_string());
                        }
                    }
                }
                (Err(error), _) | (_, Err(error)) => {
                    step.assert(&mut asserts, "export_run", false, error);
                }
            }
        }
        Err(error) => {
            step.assert(&mut asserts, "report_build", false, error.to_string());
        }
    }
    steps.push(step.finish());

    // 外部夹具（fixtures/paper-20q）：多页归组适配 + FixtureCatalog 回放 + expected 逐题断言。
    if let Some(dir) = options.fixtures.as_ref().filter(|dir| dir.is_dir()) {
        let external = external::run(&platform, dir, &mut asserts);
        steps.extend(external.steps);
        warnings.extend(external.warnings);
    }
    warnings.push("UI 键鼠 E2E（S7）与真实试卷 OCR 精度不在本命令范围（人工验收待办）".to_owned());
    warnings.push("真实模型（OCR/judge）未参与；本命令使用 fixture 后端".to_owned());
    drop(platform);
    finish_report(
        data_root,
        asserts,
        steps,
        warnings,
        artifacts,
        started,
        fixture_note,
    )
}

fn failed_report(data_root: &Path, reason: String, started: Instant) -> AcceptanceReport {
    AcceptanceReport {
        passed: false,
        assertions: 1,
        failures: vec![reason],
        warnings: Vec::new(),
        negatives: Vec::new(),
        steps: Vec::new(),
        duration_ms: started.elapsed().as_millis(),
        data_root: data_root.to_path_buf(),
        artifacts: Vec::new(),
        fixture_note: String::new(),
    }
}

fn finish_report(
    data_root: PathBuf,
    asserts: Asserts,
    steps: Vec<StepResult>,
    warnings: Vec<String>,
    artifacts: Vec<PathBuf>,
    started: Instant,
    fixture_note: String,
) -> AcceptanceReport {
    AcceptanceReport {
        passed: asserts.failures.is_empty(),
        assertions: asserts.total,
        failures: asserts.failures,
        warnings,
        negatives: asserts.negatives,
        steps,
        duration_ms: started.elapsed().as_millis(),
        data_root,
        artifacts,
        fixture_note,
    }
}

/// 单步骤计时/计数。
struct Step {
    id: &'static str,
    started: Instant,
    assertions: usize,
    failed: usize,
    detail: String,
}

impl Step {
    fn new(id: &'static str) -> Self {
        Self {
            id,
            started: Instant::now(),
            assertions: 0,
            failed: 0,
            detail: String::new(),
        }
    }

    fn assert(
        &mut self,
        asserts: &mut Asserts,
        name: &str,
        condition: bool,
        detail: impl Into<String>,
    ) {
        let before = asserts.failures.len();
        asserts.check(name, condition, detail);
        self.assertions += 1;
        if asserts.failures.len() > before {
            self.failed += 1;
        }
    }

    fn negative(
        &mut self,
        asserts: &mut Asserts,
        name: &str,
        condition: bool,
        detail: impl Into<String>,
    ) {
        let before = asserts.failures.len();
        asserts.negative(name, condition, detail);
        self.assertions += 1;
        if asserts.failures.len() > before {
            self.failed += 1;
        }
    }

    fn finish(mut self) -> StepResult {
        if self.detail.is_empty() {
            self.detail = format!("{} 项断言", self.assertions);
        }
        StepResult {
            id: self.id.to_owned(),
            status: if self.failed == 0 { "PASS" } else { "FAIL" },
            assertions: self.assertions,
            failed: self.failed,
            elapsed_ms: self.started.elapsed().as_millis(),
            detail: self.detail,
        }
    }
}

/// 校验外部夹具契约（`fixtures/paper-20q` + images/ocr/judge 索引）。
///
/// 只做只读结构校验，不把外部夹具接入端到端流水线（原因见返回说明）。
fn validate_fixtures(
    dir: &Path,
    asserts: &mut Asserts,
    step: &mut Step,
    warnings: &mut Vec<String>,
) -> String {
    let paper_path = dir.join("paper.json");
    let paper_json = read_json(&paper_path);
    step.assert(
        asserts,
        "fixtures_paper",
        paper_json
            .as_ref()
            .and_then(|value| value.get("questions"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|questions| questions.len() == 20),
        "paper.json 20 题",
    );
    step.assert(
        asserts,
        "fixtures_total_score",
        paper_json
            .as_ref()
            .and_then(|value| value.get("total_score"))
            .and_then(serde_json::Value::as_i64)
            == Some(150_000),
        "paper.json total_score=150000",
    );
    let students = read_json(&dir.join("students.json"))
        .and_then(|value| serde_json::from_value::<Vec<eg_domain::Student>>(value).ok());
    step.assert(
        asserts,
        "fixtures_students",
        students
            .as_ref()
            .is_some_and(|students| students.len() == 3),
        format!("students.json 学生 {:?}", students.as_ref().map(Vec::len)),
    );
    let rubric = read_json(&dir.join("rubric.json"));
    step.assert(
        asserts,
        "fixtures_rubric",
        rubric
            .as_ref()
            .and_then(|value| value.get("questions"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|questions| !questions.is_empty())
            && rubric
                .as_ref()
                .and_then(|value| value.get("status"))
                .and_then(serde_json::Value::as_str)
                == Some("active"),
        "rubric.json 含逐题规则且 status=active",
    );
    // 期望：3 学生 × 20 题。
    let expected = read_json(&dir.join("expected/grades-expected.json"));
    let expected_students = expected
        .as_ref()
        .and_then(|value| value.get("students"))
        .and_then(serde_json::Value::as_array);
    let expected_ok = expected_students.is_some_and(|students| {
        students.len() == 3
            && students.iter().all(|student| {
                student
                    .get("expected")
                    .and_then(|expected| expected.get("questions"))
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|questions| questions.len() == 20)
            })
    });
    step.assert(
        asserts,
        "fixtures_expected",
        expected_ok,
        "expected/grades-expected.json 3×20",
    );
    // 图片清单：含重复字节/EXIF/规范外命名。
    let images_root = dir.parent().unwrap_or(dir).join("images");
    let image_manifest = read_json(&images_root.join("manifest.json"));
    let image_files = image_manifest
        .as_ref()
        .and_then(|value| value.get("files"))
        .and_then(serde_json::Value::as_array);
    let kinds: Vec<&str> = image_files
        .map(|files| {
            files
                .iter()
                .filter_map(|file| {
                    file.get("expected")
                        .and_then(|expected| expected.get("kind"))
                        .and_then(serde_json::Value::as_str)
                })
                .collect()
        })
        .unwrap_or_default();
    let has_exif = image_files.is_some_and(|files| {
        files.iter().any(|file| {
            file.get("expected")
                .and_then(|expected| expected.get("exif_orientation"))
                .and_then(serde_json::Value::as_u64)
                == Some(6)
        })
    });
    step.assert(
        asserts,
        "fixtures_images",
        kinds.len() >= 9,
        format!("images/manifest.json 文件 {}", kinds.len()),
    );
    step.assert(
        asserts,
        "fixtures_duplicate_bytes",
        kinds.contains(&"duplicate_bytes"),
        "含重复字节样例",
    );
    step.assert(
        asserts,
        "fixtures_exif_sample",
        has_exif,
        "含 EXIF 旋转样例",
    );
    step.assert(
        asserts,
        "fixtures_irregular_name",
        kinds.contains(&"irregular_name"),
        "含规范外命名样例",
    );
    // OCR / judge fixture 索引与负向样例。
    let ocr_root = dir.parent().unwrap_or(dir).join("ocr");
    let ocr_index = read_json(&ocr_root.join("index.json"));
    let judge_root = dir.parent().unwrap_or(dir).join("judge");
    let judge_index = read_json(&judge_root.join("index.json"));
    step.assert(
        asserts,
        "fixtures_ocr_index",
        ocr_index
            .as_ref()
            .and_then(|value| value.get("records"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|records| !records.is_empty())
            && ocr_index
                .as_ref()
                .and_then(|value| value.get("negative"))
                .and_then(serde_json::Value::as_array)
                .is_some_and(|negative| !negative.is_empty()),
        "OCR 索引含 records + negative",
    );
    step.assert(
        asserts,
        "fixtures_judge_index",
        judge_index
            .as_ref()
            .and_then(|value| value.get("records"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|records| !records.is_empty())
            && judge_index
                .as_ref()
                .and_then(|value| value.get("negative"))
                .and_then(serde_json::Value::as_array)
                .is_some_and(|negative| !negative.is_empty()),
        "judge 索引含 records + negative",
    );
    step.assert(
        asserts,
        "fixtures_question_count_matches_builtin",
        paper_json
            .as_ref()
            .and_then(|value| value.get("questions"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|questions| questions.len() == paper::questions().len()),
        "外部与内置夹具同为 20 题",
    );
    warnings.push(
        "外部夹具已通过只读契约校验（20 题/150000 毫元/3 学生/规则 active/期望 3×20/图片含重复+EXIF+规范外命名/OCR+judge 含负向）；\
         外部夹具已全量接线：capture 原生多页归组（ImportReport.groups）+ OCR/judge FixtureCatalog 回放 + expected 逐题断言；judge 夹具覆盖缺口以确定性规则回退（见 external-grade warning）"
            .to_owned(),
    );
    format!(
        "已校验纸卷/名单/规则/期望/图片/OCR/judge 契约（{} 个图片条目）",
        kinds.len()
    )
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
}

fn persists(
    platform: &Platform,
    outcome: &eg_capture::ImportOutcome,
    step: &mut Step,
    asserts: &mut Asserts,
) {
    match persist(
        platform,
        PersistRequest {
            outcome,
            task_id: None,
        },
    ) {
        Ok(summary) => {
            step.assert(
                asserts,
                "persist_submissions",
                summary.submissions == 4,
                format!("落库提交 {}", summary.submissions),
            );
            step.assert(
                asserts,
                "persist_audit",
                summary.audit_entries > 0,
                format!("审计 {}", summary.audit_entries),
            );
            if let Some(first) = outcome.submissions.first() {
                let read_back = platform
                    .db()
                    .read(|conn| repo::get_submission(conn, first.id));
                step.assert(
                    asserts,
                    "persist_read_back",
                    read_back.as_ref().is_ok_and(Option::is_some),
                    "DB 读回提交",
                );
            }
        }
        Err(error) => {
            step.assert(asserts, "persist", false, error.to_string());
        }
    }
}

fn rubric_source() -> FixtureRubrics {
    let mut rubrics = FixtureRubrics::new();
    let questions = paper::questions();
    if let Some(question) = questions.get(4) {
        rubrics.insert_confirmed(question.id, paper::partial_choice_rubric());
    }
    if let Some(question) = questions.get(5) {
        rubrics.insert_confirmed(question.id, paper::partial_choice_rubric());
    }
    for index in [16usize, 17, 18] {
        if let Some(question) = questions.get(index) {
            rubrics.insert_confirmed(question.id, paper::additive_rubric());
        }
    }
    rubrics
}

fn students() -> Vec<eg_domain::Student> {
    ["张三", "李四", "王五"]
        .iter()
        .enumerate()
        .map(|(index, name)| eg_domain::Student {
            id: eg_domain::StudentId::from_ulid(Ulid::from_u128(1_001 + index as u128)),
            classroom_id: ClassroomId::from_ulid(Ulid::from_u128(2)),
            student_no: format!("2026{:04}", index + 1),
            name: (*name).to_owned(),
            source: eg_domain::RosterSource::RosterImport,
        })
        .collect()
}

fn ocr_blocks_texts(student_index: usize) -> Vec<String> {
    // 3 名学生使用同一套答案文本（夹具重点是处理链路，不是区分答案）。
    let _ = student_index;
    paper::answers()
        .iter()
        .map(|answer| answer.raw.text.clone())
        .collect()
}

fn png_for(label: &str) -> Vec<u8> {
    use image::ImageEncoder as _;
    let width = 640u32;
    let height = 960u32;
    let hash = label
        .bytes()
        .fold(7u8, |acc, byte| acc.wrapping_mul(31).wrapping_add(byte));
    let mut buffer = Vec::with_capacity(usize::try_from(width * height * 3).unwrap_or(0));
    for y in 0..height {
        for x in 0..width {
            let inside = (80..width - 80).contains(&x) && (80..height - 80).contains(&y);
            let value = if inside {
                235u8
                    .saturating_sub(hash % 30)
                    .saturating_sub(((x + y) % 17) as u8)
            } else {
                250
            };
            buffer.extend_from_slice(&[value, value, value]);
        }
    }
    let mut out = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut out);
    let _ = encoder.write_image(&buffer, width, height, image::ExtendedColorType::Rgb8);
    out
}

fn report_input(
    platform: &Platform,
    paper: &Paper,
    students: &[eg_domain::Student],
    submissions: &[eg_domain::Submission],
    records_by_submission: &BTreeMap<SubmissionId, Vec<GradeRecord>>,
    review_repo: &InMemoryReviewRepository,
) -> eg_report::ReportInput {
    let paper_snapshot = eg_report::PaperSnapshot {
        id: paper.id,
        classroom_id: paper.classroom_id,
        title: paper.title.clone(),
        classroom_name: "验收班级".to_owned(),
        total_score: paper.total_score,
        key_version: paper.key_version,
        rubric_version: paper.rubric_version,
        questions: paper
            .questions
            .iter()
            .map(|question| {
                let (answer_options, correct_options) = match &question.key {
                    eg_domain::AnswerKey::Concrete(concrete) => match &concrete.primary {
                        eg_domain::AnswerValue::Options(options) => {
                            (options.clone(), options.clone())
                        }
                        _ => (Vec::new(), Vec::new()),
                    },
                    _ => (Vec::new(), Vec::new()),
                };
                eg_report::QuestionSnapshot {
                    id: question.id,
                    ordinal: question.ordinal,
                    label: question.label.clone(),
                    kind: question.kind,
                    max_score: question.max_score,
                    knowledge_tags: question.knowledge_tags.clone(),
                    answer_options,
                    correct_options,
                }
            })
            .collect(),
    };
    let student_by_name: BTreeMap<&str, &eg_domain::Student> = students
        .iter()
        .map(|student| (student.name.as_str(), student))
        .collect();
    let summaries: Vec<eg_report::SubmissionSummary> = submissions
        .iter()
        .map(|submission| eg_report::SubmissionSummary {
            id: submission.id,
            paper_id: paper.id,
            student_id: student_by_name
                .get(submission.source_stem.as_str())
                .map(|student| student.id),
            status: SubmissionStatus::Graded,
            source_stem: submission.source_stem.clone(),
            created_at: paper::ts(),
        })
        .collect();
    let mut records: Vec<GradeRecord> = Vec::new();
    for (index, submission) in submissions.iter().enumerate() {
        if submission.source_stem == "名单外学生" {
            continue;
        }
        let mut submission_records = records_by_submission
            .get(&submission.id)
            .cloned()
            .unwrap_or_default();
        if index == 0 {
            // 用复核后的最新修订覆盖第一条记录（改分 + 撤销）。
            if let Some(first) = submission_records.first_mut() {
                let revisions = review_repo.revisions(first.submission_id, first.question_id);
                if let Some(latest) = revisions.last() {
                    *first = latest.clone();
                }
            }
        }
        records.extend(submission_records);
    }
    let _ = platform;
    eg_report::ReportInput {
        paper: paper_snapshot,
        students: students.to_vec(),
        submissions: summaries,
        records,
        exclusions: Vec::new(),
        choices: Vec::new(),
    }
}

fn egctl_fixture_export(
    report: &eg_report::Report,
    out_dir: &Path,
    exported_at: Timestamp,
) -> Result<eg_report::ExportOutcome, String> {
    let request = eg_report::ExportRequest::new(out_dir, exported_at);
    eg_report::export(report, &request, &eg_report::MemoryAuditor::new())
        .map_err(|error| error.to_string())
}

fn unreachable_dirs() -> DataDirs {
    // `DataDirs::with_root` 仅在路径非法（空/非 UTF-8）时失败；此处兜底到当前目录。
    DataDirs::with_root(std::env::temp_dir()).unwrap_or_else(|_| unreachable!("temp dir 必须可用"))
}

/// 供集成测试/CLI 使用的显式任务 id（避免依赖内部常量）。
#[must_use]
pub fn acceptance_task_id() -> TaskId {
    paper::task_id()
}

/// 供集成测试断言证据：至少 1 条证据引用。
#[must_use]
pub fn has_evidence(record: &GradeRecord) -> bool {
    !record.evidence.is_empty() && matches!(record.evidence.first(), Some(EvidenceRef::Asset(_)))
}

/// 供集成测试断言分数粒度（默认 0.1 分 = 100 千分）。
#[must_use]
pub fn score_granularity() -> MilliScore {
    GradeConfig::default().score_granularity
}

/// 供集成测试断言毫秒时间上限（防止能力退化时测试仍通过）。保留备用。
#[must_use]
pub fn acceptance_budget() -> Duration {
    Duration::from_secs(120)
}

/// 供集成测试断言时间戳默认值（夹具固定时间）。
#[must_use]
pub fn acceptance_timestamp() -> Timestamp {
    paper::ts()
}

/// 供集成测试使用：AssetId 构造（页面证据）。
#[must_use]
pub fn acceptance_asset() -> AssetId {
    AssetId::of(b"acceptance-page")
}

/// 供集成测试使用：班级 id。
#[must_use]
pub fn acceptance_classroom() -> ClassroomId {
    ClassroomId::from_ulid(Ulid::from_u128(2))
}

/// 供集成测试使用：Arc 平台的类型别名（避免测试直接依赖 platform 版本）。
pub type PlatformHandle = Arc<Platform>;
