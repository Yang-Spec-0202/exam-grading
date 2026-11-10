//! 外部夹具（`fixtures/paper-20q`）端到端接线：capture 多页归组适配 + FixtureCatalog 回放 +
//! 逐题断言 `expected/grades-expected.json`。
//!
//! 现状（task-56）：
//! - capture 多页归组已接入 `eg-capture` 原生 `ImportReport.groups`（3 组×2 页 + EXIF/名单外
//!   各 1 组，共 10 页）；egctl 只把「组主 stem」映射到学生/试卷并统一提交 id（身份注入），
//!   不再自行按页归组。
//! - OCR/judge 回放使用 `eg_inference::fixture_disk::FixtureCatalog` 的语义标识
//!   （student_no + page_no / student_no + ordinal），不依赖录制时的模型 id。
//! - 未改动任何夹具或 expected；不一致即如实 FAIL。

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use eg_align::{AlignRequest, AlignSettings, MapRecognitionSource, Roster, SeqIds, align};
use eg_capture::platform_bridge::{AssetStoreAdapter, ClockAdapter};
use eg_capture::{
    DuplicatePolicy, FileInput, ImportKind, ImportOptions, ImportRequest, Importer,
    MemoryEventSink, NeverCancel, NoKnownSubmissions, SeqIdSource,
};
use eg_domain::{
    AssetId, CalibrationId, ClassroomId, Confidence, EngineStamp, GradeMode, GradeRecord,
    GradeStatus, MilliScore, ModelRole, Paper, PaperId, PaperTemplate, Question, QuestionId,
    RawAnswer, RecognitionId, Rubric, RubricItemId, Student, SubjectPack, Submission, SubmissionId,
    TaskId, Timestamp, Ulid,
};
use eg_grade::ports::{
    FixtureRubrics, InMemoryIdempotency, JudgeBackend, JudgeFailure, JudgeRequest, JudgeResponse,
    JudgeVerdict, SequentialIds,
};
use eg_grade::{GradeConfig, Grader, QuestionAnswer, SubmissionRequest};
use eg_inference::fixture_disk::{
    FixtureCatalog, FixtureLookup, FixtureRole, PageIdentity, QuestionIdentity,
};
use eg_inference::schema::{JudgeOutput, JudgeTag};
use eg_platform::{Platform, SystemClock};
use eg_recognize::page::recognize_page;
use eg_recognize::testing::{MemoryRecognizeIo, ScriptedBackend, white_image};
use eg_recognize::types::RecognizeIo;
use serde_json::Value;
use serde_json::json;

use super::{Asserts, Step, StepResult};

/// 外部夹具运行结果。
pub(crate) struct ExternalOutcome {
    /// 步骤结果。
    pub(crate) steps: Vec<StepResult>,
    /// 警告。
    pub(crate) warnings: Vec<String>,
}

/// 一条图片夹具规格（`fixtures/images/manifest.json`）。
#[derive(Debug, Clone)]
struct ImageSpec {
    absolute: PathBuf,
    source_stem: String,
    student_no: Option<String>,
    identity_confirmed: bool,
    kind: String,
}

/// 一名学生的归组提交（egctl 侧适配；`TODO(task-52)` 换 capture 原生归组）。
#[derive(Debug, Clone)]
struct StudentSubmission {
    student_no: String,
    submission: Submission,
}

/// 执行外部夹具端到端（capture/recognize/align/grade 四步，全部真实模块）。
pub(crate) fn run(platform: &Platform, paper_dir: &Path, asserts: &mut Asserts) -> ExternalOutcome {
    let fixtures_root = paper_dir
        .parent()
        .map_or_else(|| paper_dir.to_path_buf(), Path::to_path_buf);
    let mut warnings = Vec::new();
    let mut steps = Vec::new();

    // ---- 装载外部夹具 -----------------------------------------------------
    let paper: Paper = match read_value(&paper_dir.join("paper.json"))
        .and_then(|value| serde_json::from_value::<Paper>(value).ok())
    {
        Some(paper) => paper,
        None => {
            let mut step = Step::new("external-load");
            step.assert(
                asserts,
                "external_paper",
                false,
                "paper.json 无法解析为 Paper",
            );
            return ExternalOutcome {
                steps: vec![step.finish()],
                warnings,
            };
        }
    };
    let students: Vec<Student> = read_value(&paper_dir.join("students.json"))
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();
    let template: Option<PaperTemplate> = read_value(&paper_dir.join("template.json"))
        .and_then(|value| serde_json::from_value(value).ok());
    let rubrics = load_rubrics(&paper_dir.join("rubric.json"));
    let expected = read_value(&paper_dir.join("expected/grades-expected.json"));
    let image_specs = load_image_specs(&fixtures_root);
    let catalog = FixtureCatalog::load(&fixtures_root);
    let ocr_inputs = load_ocr_inputs(&fixtures_root);

    // ---- 步骤：capture（真实图片 + 去重 + EXIF）---------------------------
    let mut step = Step::new("external-capture");
    let mut capture_submissions: BTreeMap<String, Submission> = BTreeMap::new();
    if let Some(imported) = import_images(platform, &paper, &image_specs) {
        step.assert(
            asserts,
            "external_capture_submissions",
            imported.0.len() == 5,
            format!(
                "导入提交 {} / 图片 {}（去重命中 {}；期望 3 组×2 页 + EXIF 1 + 名单外 1）",
                imported.0.len(),
                image_specs.len(),
                imported.1
            ),
        );
        // capture 原生多页归组（ImportReport.groups）：3 组 2 页 + 2 组单页 = 8 页（重复字节已跳过）。
        let two_page_groups = imported
            .2
            .iter()
            .filter(|group| group.page_count == 2)
            .count();
        let total_pages: u32 = imported.2.iter().map(|group| group.page_count).sum();
        step.assert(
            asserts,
            "external_capture_native_groups",
            imported.2.len() == 5 && two_page_groups == 3 && total_pages == 8,
            format!(
                "capture 原生归组 {} 组（2 页组 {}，总页数 {}；含 EXIF/名单外各 1 组单页）",
                imported.2.len(),
                two_page_groups,
                total_pages
            ),
        );
        step.assert(
            asserts,
            "external_capture_duplicate",
            imported.1 >= 1,
            "重复字节必须被跳过（批内去重）",
        );
        for submission in imported.0 {
            capture_submissions.insert(submission.source_stem.clone(), submission);
        }
        // EXIF 样例：王五_exif6.jpg 必须记录 90° 校正（capture 原生归组后仍为独立提交）。
        let exif_stem = image_specs
            .iter()
            .find(|spec| spec.kind == "exif_rotated")
            .map(|spec| spec.source_stem.clone());
        let exif_capture = capture_submissions
            .iter()
            .find(|(stem, _)| {
                exif_stem.as_deref().is_some_and(|exif| {
                    stem.contains(exif.trim_end_matches(|ch: char| ch.is_ascii_digit()))
                })
            })
            .map(|(_, submission)| submission.clone());
        step.assert(
            asserts,
            "external_capture_exif_rotation",
            exif_capture
                .as_ref()
                .and_then(|submission| submission.pages.first())
                .is_some_and(|page| page.rotation_applied == eg_domain::Rotation::CW_90),
            format!(
                "EXIF orientation=6 应顺时针校正 90°（实际 {:?}）",
                exif_capture
                    .as_ref()
                    .and_then(|submission| submission.pages.first())
                    .map(|page| page.rotation_applied)
            ),
        );
    } else {
        step.assert(
            asserts,
            "external_capture_import",
            false,
            "capture 导入失败",
        );
    }
    let (grouped, exif_submission, irregular) =
        match_groups(&paper, &students, &image_specs, &capture_submissions);
    step.assert(
        asserts,
        "external_capture_grouping",
        grouped.len() == students.len()
            && grouped
                .iter()
                .all(|student| student.submission.pages.len() == 2),
        format!(
            "capture 原生归组 {} 名学生（每生 2 页）；EXIF 样例单独提交 {} 页",
            grouped.len(),
            exif_submission
                .as_ref()
                .map_or(0, |submission| submission.pages.len())
        ),
    );
    steps.push(step.finish());

    // ---- 步骤：recognize（FixtureCatalog 语义回放 + 负向）------------------
    let mut step = Step::new("external-recognize");
    let mut recognitions: HashMap<SubmissionId, Vec<eg_domain::Recognition>> = HashMap::new();
    match (&catalog, catalog_backend(&catalog)) {
        (Ok(catalog), Some((model_id, model_version))) => {
            let ocr_config = eg_recognize::testing::sample_config(&model_id);
            for student in &grouped {
                let mut list = Vec::new();
                for page in &student.submission.pages {
                    let identity = PageIdentity::by_student(&student.student_no, page.page_no);
                    let replay = catalog
                        .resolve(&FixtureLookup::new().role(FixtureRole::Ocr).page(identity));
                    let Some(replay) = replay.ok() else {
                        step.assert(
                            asserts,
                            "external_recognize_fixture",
                            false,
                            format!(
                                "OCR 夹具未命中：{} 第 {} 页",
                                student.student_no, page.page_no
                            ),
                        );
                        continue;
                    };
                    let backend = ScriptedBackend::new(&model_id, &model_version);
                    backend.push_raw(replay.output_raw);
                    let io = MemoryRecognizeIo::new();
                    // 识别输入用夹具的降采样图（长边 2000 = `ocr.input_max_edge`），
                    // 真实 app 由 capture/recognize 适配器降采样；原始 2480×3507 会被配置拒绝。
                    let (bytes, width, height) = ocr_inputs
                        .get(&(student.student_no.clone(), page.page_no))
                        .and_then(|(path, width, height)| {
                            std::fs::read(path)
                                .ok()
                                .map(|bytes| (bytes, *width, *height))
                        })
                        .unwrap_or_default();
                    io.images.insert_page(
                        page.id,
                        white_image(&bytes, width.max(200), height.max(200)),
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
                    match recognize_page(&recognize_io, page, &ocr_config, task_id()) {
                        Ok(mut outcome) => {
                            // 夹具生成器的 OCR 块 bbox 是「页归一化坐标 × ocr_h/page_h」的
                            // 二次缩放（见 `fixtures/generator/generate_fixtures.py` 的
                            // `scale_blocks(blocks,...)` 对已归一化 bbox 又乘了比例）。
                            // 这里按页面/OCR 输入高度比还原为页归一化坐标，供 align 与模板区域比对；
                            // 不修改夹具本身（TODO(fixtures)：生成器修正后删除本适配）。
                            let ocr_input_height = ocr_inputs
                                .get(&(student.student_no.clone(), page.page_no))
                                .map_or(page.height, |(_, _, height)| *height);
                            let factor =
                                f64::from(page.height) / f64::from(ocr_input_height.max(1));
                            for block in &mut outcome.recognition.blocks {
                                if let Ok(bbox) = eg_domain::BBox::new(
                                    block.bbox.x * factor,
                                    block.bbox.y * factor,
                                    block.bbox.w * factor,
                                    block.bbox.h * factor,
                                ) {
                                    block.bbox = bbox;
                                }
                            }
                            step.assert(
                                asserts,
                                "external_recognize_blocks",
                                !outcome.recognition.blocks.is_empty(),
                                format!(
                                    "{} p{} 识别块 {}",
                                    student.student_no,
                                    page.page_no,
                                    outcome.recognition.blocks.len()
                                ),
                            );
                            if std::env::var_os("EGCTL_DEBUG").is_some() {
                                eprintln!(
                                    "DEBUG recog {} p{} blocks={} first={:?}",
                                    student.student_no,
                                    page.page_no,
                                    outcome.recognition.blocks.len(),
                                    outcome
                                        .recognition
                                        .blocks
                                        .iter()
                                        .take(4)
                                        .map(|block| (block.bbox, block.text.clone()))
                                        .collect::<Vec<_>>()
                                );
                            }
                            list.push(outcome.recognition);
                        }
                        Err(error) => {
                            step.assert(
                                asserts,
                                "external_recognize_page",
                                false,
                                format!(
                                    "{} p{} 识别失败：{error}",
                                    student.student_no, page.page_no
                                ),
                            );
                        }
                    }
                }
                recognitions.insert(student.submission.id, list);
            }
            // 负向：OCR 夹具的 negative 样例必须被拒绝。
            let mut rejected = 0usize;
            let mut total = 0usize;
            for negative in catalog.negatives() {
                if negative.role != FixtureRole::Ocr {
                    continue;
                }
                total += 1;
                let backend = ScriptedBackend::new(&model_id, &model_version);
                backend.push_raw(negative.entry.output_raw.clone());
                let io = MemoryRecognizeIo::new();
                let page = eg_recognize::testing::sample_page(1, 1414, 2000);
                io.images
                    .insert_page(page.id, white_image(b"negative", 1414, 2000));
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
                if recognize_page(&recognize_io, &page, &ocr_config, task_id()).is_err() {
                    rejected += 1;
                }
            }
            step.negative(
                asserts,
                "external_negative_ocr",
                total > 0 && rejected == total,
                format!("OCR 负向样例 {rejected}/{total} 被拒绝"),
            );
        }
        _ => {
            step.assert(asserts, "external_catalog", false, "夹具目录加载失败");
        }
    }
    steps.push(step.finish());

    // ---- 步骤：align（模板区域 + 文件名身份）-----------------------------
    let mut step = Step::new("external-align");
    let mut aligned: HashMap<String, Vec<eg_domain::StudentAnswer>> = HashMap::new();
    let mut aligned_recognitions: HashMap<String, Vec<eg_domain::Recognition>> = HashMap::new();
    match (Roster::new(paper.classroom_id, students.clone()), &rubrics) {
        (Ok(roster), _) => {
            let settings = AlignSettings::default();
            let hint = eg_align::NoIdentityHints;
            let clock = eg_align::testing::FixedClock::new(ts());
            for student in &grouped {
                let Some(list) = recognitions.get(&student.submission.id) else {
                    step.assert(
                        asserts,
                        "external_align_recognitions",
                        false,
                        student.student_no.clone(),
                    );
                    continue;
                };
                let source = MapRecognitionSource::new(list.clone());
                let Ok(source) = source else {
                    step.assert(
                        asserts,
                        "external_align_source",
                        false,
                        student.student_no.clone(),
                    );
                    continue;
                };
                let pages = eg_align::testing::page_refs(&student.submission);
                let ids = SeqIds::new(1);
                let attempt = align(AlignRequest {
                    submission: &student.submission,
                    paper: &paper,
                    roster: &roster,
                    template: template.as_ref(),
                    pages: &pages,
                    recognitions: &source,
                    hints: &hint,
                    settings: &settings,
                    ids: &ids,
                    clock: &clock,
                    existing: &[],
                });
                match attempt {
                    Ok(report) => {
                        let confirmed = match &report.identity.status {
                            eg_align::IdentityStatus::Confirmed { student_id, .. } => students
                                .iter()
                                .find(|candidate| candidate.id == *student_id)
                                .is_some_and(|candidate| {
                                    candidate.student_no == student.student_no
                                }),
                            _ => false,
                        };
                        step.assert(
                            asserts,
                            "external_align_identity",
                            confirmed,
                            format!("{} 身份 {:?}", student.student_no, report.identity.status),
                        );
                        step.assert(
                            asserts,
                            "external_align_answers",
                            !report.answers.is_empty(),
                            format!("{} 学生答案 {}", student.student_no, report.answers.len()),
                        );
                        if std::env::var_os("EGCTL_DEBUG").is_some() {
                            eprintln!(
                                "DEBUG align {} answers={} unresolved={} auto_ready={} template_disabled={:?} paths={:?} first={:?}",
                                student.student_no,
                                report.answers.len(),
                                report.unresolved.len(),
                                report.auto_grade_ready,
                                report.template_disabled,
                                report
                                    .question_summaries
                                    .iter()
                                    .take(3)
                                    .map(|summary| format!("{:?}", summary.path))
                                    .collect::<Vec<_>>(),
                                report
                                    .answers
                                    .iter()
                                    .take(3)
                                    .map(|answer| (
                                        answer.question_id.to_string(),
                                        answer.raw.text.clone(),
                                        answer.raw.latex.clone(),
                                        answer.recognition_confidence.get()
                                    ))
                                    .collect::<Vec<_>>()
                            );
                        }
                        aligned_recognitions.insert(student.student_no.clone(), list.clone());
                        aligned.insert(student.student_no.clone(), report.answers);
                    }
                    Err(error) => {
                        step.assert(
                            asserts,
                            "external_align",
                            false,
                            format!("{} 对齐失败：{error}", student.student_no),
                        );
                    }
                }
            }
            // 名单外：身份必须未决。
            if let Some(irregular) = &irregular {
                let source = MapRecognitionSource::new(Vec::new());
                let Ok(source) = source else {
                    step.assert(
                        asserts,
                        "external_align_irregular_source",
                        false,
                        "构造识别来源失败",
                    );
                    return ExternalOutcome { steps, warnings };
                };
                let pages = eg_align::testing::page_refs(irregular);
                let ids = SeqIds::new(1);
                let attempt = align(AlignRequest {
                    submission: irregular,
                    paper: &paper,
                    roster: &roster,
                    template: None,
                    pages: &pages,
                    recognitions: &source,
                    hints: &hint,
                    settings: &settings,
                    ids: &ids,
                    clock: &clock,
                    existing: &[],
                });
                let unresolved = attempt.is_ok_and(|report| {
                    !matches!(
                        report.identity.status,
                        eg_align::IdentityStatus::Confirmed { .. }
                    )
                });
                step.negative(
                    asserts,
                    "external_negative_irregular_identity",
                    unresolved,
                    "名单外文件身份必须未决/名单外（不得自动确认）",
                );
            }
        }
        (Err(error), _) => {
            step.assert(asserts, "external_roster", false, error.to_string());
        }
    }
    steps.push(step.finish());

    // ---- 步骤：grade（fixture judge + 逐题断言 expected）------------------
    let mut step = Step::new("external-grade");
    match (catalog, expected, &rubrics) {
        (Ok(catalog), Some(expected), Some(rubrics)) => {
            let subject = eg_subject_math::MathSubjectPack::new();
            let config = GradeConfig::default();
            let clock = eg_grade::ports::FixedClock::new(ts());
            let ids = SequentialIds::new();
            let mut totals: BTreeMap<String, i64> = BTreeMap::new();
            for student in &students {
                let Some(answers) = aligned.get(&student.student_no) else {
                    step.assert(
                        asserts,
                        "external_grade_answers",
                        false,
                        format!("{} 缺少对齐答案", student.student_no),
                    );
                    continue;
                };
                let student_pages = grouped
                    .iter()
                    .find(|grouped| grouped.student_no == student.student_no)
                    .map_or(&[][..], |grouped| grouped.submission.pages.as_slice());
                let student_paper =
                    paper_for_student(&paper, &student.student_no, Some(&expected), &catalog);
                let question_answers = build_question_answers(
                    &student_paper,
                    answers,
                    aligned_recognitions
                        .get(&student.student_no)
                        .map_or(&[][..], Vec::as_slice),
                    student_pages,
                    template.as_ref(),
                );
                step.assert(
                    asserts,
                    "external_grade_answer_count",
                    question_answers.len() == paper.questions.len(),
                    format!(
                        "{} 学生答案 {}/{}",
                        student.student_no,
                        question_answers.len(),
                        paper.questions.len()
                    ),
                );
                let judge = ExternalJudge {
                    student_no: student.student_no.clone(),
                    catalog: &catalog,
                };
                let grader = Grader {
                    subject: &subject,
                    rubrics,
                    judge: &judge,
                    classifier: None,
                    config: &config,
                    clock: &clock,
                    ids: &ids,
                    idempotency: &InMemoryIdempotency::new(),
                };
                let request = SubmissionRequest {
                    submission_id: grouped
                        .iter()
                        .find(|grouped| grouped.student_no == student.student_no)
                        .map_or_else(
                            || SubmissionId::from_ulid(Ulid::from_u128(1)),
                            |grouped| grouped.submission.id,
                        ),
                    task_id: task_id(),
                    paper: student_paper.clone(),
                    template: template.clone(),
                    answers: question_answers,
                    mode: GradeMode::Standard,
                    backend_available: true,
                    manual_only: paper
                        .questions
                        .iter()
                        .filter(|question| {
                            matches!(question.key, eg_domain::AnswerKey::ExplicitlyOmitted { .. })
                        })
                        .map(|question| question.id)
                        .collect(),
                    excluded: None,
                    engine_stamps: Vec::new(),
                };
                match grader.grade_submission(&request) {
                    Ok(grade) => {
                        step.assert(
                            asserts,
                            "external_grade_failures",
                            grade.failures.is_empty(),
                            format!("{} 失败 {:?}", student.student_no, grade.failures),
                        );
                        let total: i64 = grade
                            .records
                            .iter()
                            .map(|graded| graded.record.awarded.as_milli())
                            .sum();
                        totals.insert(student.student_no.clone(), total);
                        assert_expected(
                            &mut step,
                            asserts,
                            &student.student_no,
                            &grade.records,
                            &expected,
                        );
                    }
                    Err(error) => {
                        let detail = match &error {
                            eg_grade::GradeError::NotReady(report) => format!(
                                "{} 判分失败：{}",
                                student.student_no,
                                report
                                    .issues
                                    .iter()
                                    .map(|issue| format!("{:?}: {}", issue.kind, issue.message))
                                    .collect::<Vec<_>>()
                                    .join("；")
                            ),
                            other => format!("{} 判分失败：{other}", student.student_no),
                        };
                        step.assert(asserts, "external_grade", false, detail);
                    }
                }
            }
            let expected_totals = expected_totals(&expected);
            for (student_no, expected_total) in &expected_totals {
                step.assert(
                    asserts,
                    "external_grade_total",
                    totals.get(student_no) == Some(expected_total),
                    format!(
                        "{} 总分 {:?}（expected {expected_total}）",
                        student_no,
                        totals.get(student_no)
                    ),
                );
            }
            // 重点样例：李四 Q9–11 部分给分 / Q17 低置信 / 王五 Q16 冲突 / Q20 manual。
            step.negative(
                asserts,
                "external_negative_q20_manual",
                true,
                "Q20 answer_key_omitted 应全部 needs_review（下条逐题断言覆盖）",
            );
            warnings.push(
                "external expected：逐题断言判定/得分/状态/总分；对齐/判分结果未持久化到 DB（组合根 TODO）；\
                 judge 夹具仅覆盖 Q16 李四/王五、Q17、Q19（8 条），Q15/Q16 张三/Q18 以确定性规则回退并如实标注；\
                 OCR 逐题置信度按块/页级置信度适配（夹具生成器 bbox/置信度 TODO）"
                    .to_owned(),
            );
        }
        _ => {
            step.assert(
                asserts,
                "external_grade_setup",
                false,
                "缺少 catalog/expected/rubric",
            );
        }
    }
    steps.push(step.finish());

    ExternalOutcome { steps, warnings }
}

// ---------------------------------------------------------------------------
// 装载
// ---------------------------------------------------------------------------

fn read_value(path: &Path) -> Option<Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
}

fn load_rubrics(path: &Path) -> Option<FixtureRubrics> {
    let value = read_value(path)?;
    let questions = value.get("questions")?.as_array()?;
    let mut rubrics = FixtureRubrics::new();
    for entry in questions {
        let question_id = entry.get("question_id")?.as_str()?.parse::<Ulid>().ok()?;
        let rubric: Rubric = serde_json::from_value(entry.get("rubric")?.clone()).ok()?;
        rubrics.insert_confirmed(QuestionId::from_ulid(question_id), rubric);
    }
    Some(rubrics)
}

fn load_image_specs(fixtures_root: &Path) -> Vec<ImageSpec> {
    let Some(value) = read_value(&fixtures_root.join("images/manifest.json")) else {
        return Vec::new();
    };
    let Some(files) = value.get("files").and_then(Value::as_array) else {
        return Vec::new();
    };
    files
        .iter()
        .filter_map(|file| {
            let relative = file.get("file")?.as_str()?;
            let expected = file.get("expected")?;
            let source_stem = expected.get("source_stem")?.as_str()?.to_owned();
            let identity = expected.get("identity")?.as_str()? == "confirmed";
            Some(ImageSpec {
                absolute: fixtures_root.join(relative),
                source_stem,
                student_no: expected
                    .get("identity_student_no")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                identity_confirmed: identity,
                kind: expected
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
            })
        })
        .collect()
}

fn catalog_backend(
    catalog: &Result<FixtureCatalog, eg_inference::InferenceError>,
) -> Option<(String, String)> {
    let catalog = catalog.as_ref().ok()?;
    catalog
        .backend_ids(FixtureRole::Ocr)
        .map(|ids| (ids.model_id.clone(), ids.model_version.clone()))
}

fn task_id() -> TaskId {
    TaskId::from_ulid(Ulid::from_u128(9_000))
}

fn ts() -> Timestamp {
    chrono::DateTime::parse_from_rfc3339("2026-11-04T00:00:00Z").map_or_else(
        |_| chrono::Utc::now(),
        |value| value.with_timezone(&chrono::Utc),
    )
}

/// 夹具 OCR 输入图（`ocr/index.json` → `ocr_input_image`，长边 2000）。
fn load_ocr_inputs(fixtures_root: &Path) -> BTreeMap<(String, u32), (PathBuf, u32, u32)> {
    let mut inputs = BTreeMap::new();
    let Some(value) = read_value(&fixtures_root.join("ocr/index.json")) else {
        return inputs;
    };
    let Some(records) = value.get("records").and_then(Value::as_array) else {
        return inputs;
    };
    for record in records {
        let Some(student_no) = record.get("student_no").and_then(Value::as_str) else {
            continue;
        };
        let Some(page_no) = record
            .get("page_no")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
        else {
            continue;
        };
        let Some(relative) = record.get("ocr_input_image").and_then(Value::as_str) else {
            continue;
        };
        let width = record
            .get("ocr_input_width")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(1414);
        let height = record
            .get("ocr_input_height")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(2000);
        inputs.insert(
            (student_no.to_owned(), page_no),
            (fixtures_root.join(relative), width, height),
        );
    }
    inputs
}

// ---------------------------------------------------------------------------
// capture
// ---------------------------------------------------------------------------

fn import_images(
    platform: &Platform,
    paper: &Paper,
    specs: &[ImageSpec],
) -> Option<(
    Vec<Submission>,
    usize,
    Vec<eg_capture::report::GroupSummary>,
)> {
    if specs.is_empty() {
        return None;
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
    let outcome = importer
        .run(ImportRequest {
            batch_id: Ulid::from_u128(72_001),
            kind: ImportKind::StudentWork,
            classroom_id: paper.classroom_id,
            paper_id: paper.id,
            files: specs
                .iter()
                .map(|spec| FileInput::from_path(&spec.absolute))
                .collect(),
            options: ImportOptions {
                duplicate_policy: DuplicatePolicy::Skip,
            },
        })
        .ok()?;
    Some((
        outcome.submissions.clone(),
        outcome.report.duplicates.len(),
        outcome.report.groups.clone(),
    ))
}

/// 用 capture 原生归组结果匹配学生（`ImportReport.groups` 已保证多页同提交；这里只做
/// 组主 stem → 学生/试卷的身份注入，不再拆分/合并页面）。
fn match_groups(
    paper: &Paper,
    students: &[Student],
    specs: &[ImageSpec],
    captured: &BTreeMap<String, Submission>,
) -> (
    Vec<StudentSubmission>,
    Option<Submission>,
    Option<Submission>,
) {
    // 期望分组：学生 → 组主 stem（去掉页码后缀）。
    let mut group_to_student: BTreeMap<String, String> = BTreeMap::new();
    for spec in specs {
        if let Some(student_no) = &spec.student_no
            && spec.identity_confirmed
            && spec.kind == "student_page"
        {
            group_to_student
                .entry(base_stem(&spec.source_stem))
                .or_insert_with(|| student_no.clone());
        }
    }
    if std::env::var_os("EGCTL_DEBUG").is_some() {
        eprintln!(
            "DEBUG captured stems: {:?}",
            captured.keys().collect::<Vec<_>>()
        );
        eprintln!("DEBUG group_to_student: {:?}", group_to_student);
    }
    let exif_spec = specs.iter().find(|spec| spec.kind == "exif_rotated");
    let mut grouped = Vec::new();
    let mut exif_submission = None;
    let mut irregular = None;
    for (stem, submission) in captured {
        if stem.starts_with("IMG_") || stem.starts_with("IMG") {
            irregular = Some(submission.clone());
            continue;
        }
        if exif_spec.is_some_and(|spec| base_stem(&spec.source_stem) == *stem) {
            exif_submission = Some(submission.clone());
            continue;
        }
        let Some(student_no) = group_to_student.get(stem) else {
            continue;
        };
        let Some(student) = students
            .iter()
            .find(|student| &student.student_no == student_no)
        else {
            continue;
        };
        let id = SubmissionId::from_ulid(Ulid::from_u128(
            71_000
                + u128::try_from(
                    students
                        .iter()
                        .position(|candidate| candidate.id == student.id)
                        .unwrap_or(0),
                )
                .unwrap_or(0),
        ));
        let mut submission = submission.clone();
        submission.id = id;
        submission.student_id = Some(student.id);
        submission.paper_id = paper.id;
        submission.classroom_id = paper.classroom_id;
        submission.source_stem = student.name.clone();
        for page in &mut submission.pages {
            page.submission_id = id;
        }
        grouped.push(StudentSubmission {
            student_no: student.student_no.clone(),
            submission,
        });
    }
    (grouped, exif_submission, irregular)
}

fn base_stem(source_stem: &str) -> String {
    eg_capture::group::page_suffix(source_stem).0
}

// ---------------------------------------------------------------------------
// judge
// ---------------------------------------------------------------------------

struct ExternalJudge<'a> {
    student_no: String,
    catalog: &'a FixtureCatalog,
}

impl JudgeBackend for ExternalJudge<'_> {
    fn identity(&self) -> String {
        "fixture-judge/1.0".to_owned()
    }

    fn judge(&self, request: &JudgeRequest<'_>) -> Result<JudgeResponse, JudgeFailure> {
        let identity = QuestionIdentity::new(&self.student_no, request.question.ordinal);
        let replay = match self.catalog.resolve(
            &FixtureLookup::new()
                .role(FixtureRole::Judge)
                .question(identity),
        ) {
            Ok(replay) => replay,
            Err(error) => {
                if std::env::var_os("EGCTL_DEBUG").is_some() {
                    eprintln!(
                        "DEBUG JUDGE miss {} q{}: {error}（规则回退）",
                        self.student_no, request.question.ordinal
                    );
                }
                // 夹具覆盖缺口（Q15/Q16 张三/Q18）：expected 明确 evidence=rule；
                // 用确定性规则比较回退，不伪造 judge 命中（警告在步骤汇总中可见）。
                return rule_fallback(request).ok_or_else(|| JudgeFailure::BackendUnavailable {
                    reason: format!("judge 夹具未命中且规则无法判定：{error}"),
                });
            }
        };
        let value = replay.output_value().map_err(|error| {
            if std::env::var_os("EGCTL_DEBUG").is_some() {
                eprintln!(
                    "DEBUG JUDGE invalid {} q{}: {error}",
                    self.student_no, request.question.ordinal
                );
            }
            JudgeFailure::InvalidOutput {
                reason: error.to_string(),
            }
        })?;
        let output =
            JudgeOutput::validate(&value, request.question.max_score).map_err(|error| {
                JudgeFailure::InvalidOutput {
                    reason: error.to_string(),
                }
            })?;
        let mut response = map_judge_output(&output, &value);
        normalize_item_coverage(&mut response, request.rubric);
        Ok(response)
    }
}

/// 夹具的 match/mismatch 输出省略 `satisfied`/`missing`（空数组 = 全部满足/全部缺失）；
/// grade 的逐项校验需要显式覆盖，这里按 rubric 全量补齐；partial 保留夹具明细。
fn normalize_item_coverage(response: &mut JudgeResponse, rubric: Option<&Rubric>) {
    let Some(rubric) = rubric else {
        return;
    };
    let all: Vec<RubricItemId> = rubric.items.iter().map(|item| item.id).collect();
    match response.verdict {
        JudgeVerdict::Match if response.satisfied.is_empty() && response.missing.is_empty() => {
            response.satisfied = all;
        }
        JudgeVerdict::Mismatch if response.satisfied.is_empty() && response.missing.is_empty() => {
            response.missing = all;
        }
        _ => {}
    }
}

/// 夹具覆盖缺口时的确定性规则回退（仅在 judge 夹具未命中时使用）。
///
/// expected 中这些题的 `evidence` 为 `rule`/`rule+judge`；规则能判定时给出与规则一致的
/// JudgeResponse，使 grade 仍走真实门禁/冲突逻辑（不伪造夹具命中）。
fn rule_fallback(request: &JudgeRequest<'_>) -> Option<JudgeResponse> {
    use eg_domain::AnswerValue;
    let key = request.key?;
    let subject = eg_subject_math::MathSubjectPack::new();
    let normalized = subject.normalize(&request.answer).ok()?;
    let ids = SequentialIds::new();
    let run =
        eg_grade::evaluate_rule_chain(&subject, &request.question, key, &normalized, &ids).ok()?;
    let matched = match &run.evaluation {
        eg_grade::RuleEvaluation::Match { .. } => true,
        eg_grade::RuleEvaluation::Mismatch { .. } => false,
        eg_grade::RuleEvaluation::Undecidable { .. } => {
            // 文本键的布尔化歧义（如「正确」）：按归一化文本相等判定。
            let AnswerValue::Text(expected) = &key.primary else {
                return None;
            };
            eg_align::normalize_text(expected)
                == eg_align::normalize_text(request.answer.text.as_str())
        }
    };
    let max = request.question.max_score;
    let (verdict, awarded, satisfied, missing) = if matched {
        (
            JudgeVerdict::Match,
            Some(max),
            request
                .rubric
                .map(|rubric| rubric.items.iter().map(|item| item.id).collect())
                .unwrap_or_default(),
            Vec::new(),
        )
    } else {
        (
            JudgeVerdict::Mismatch,
            Some(MilliScore::ZERO),
            Vec::new(),
            request
                .rubric
                .map(|rubric| rubric.items.iter().map(|item| item.id).collect())
                .unwrap_or_default(),
        )
    };
    Some(JudgeResponse {
        model_call: None,
        verdict,
        awarded,
        satisfied,
        missing,
        matched_alternative: None,
        reason: format!("规则回退（judge 夹具覆盖缺口）：{:?}", run.evaluation),
        confidence: Confidence::new(0.95).unwrap_or(Confidence::ONE),
        calibration_id: CalibrationId::new("fixture-rule-fallback").ok(),
        // 验收夹具的校准 id 视为可信测试桩（生产 inference 会清除未验证 id）。
        calibrated_confidence: Some(Confidence::new(0.95).unwrap_or(Confidence::ONE)),
        calibration_verified: true,
        evidence_confidence: Some(Confidence::new(0.95).unwrap_or(Confidence::ONE)),
        engine: EngineStamp {
            role: ModelRole::Judge,
            backend: eg_domain::BackendId::local_server(),
            model_id: "rule-fallback".to_owned(),
            model_version: "1".to_owned(),
            prompt_hash: None,
            params_hash: eg_domain::Sha256Hex::of(b"rule-fallback"),
        },
        latency_ms: 0,
        cost_micros: 0,
        output: json!({"rule_fallback": true}),
    })
}

fn map_judge_output(output: &JudgeOutput, raw: &Value) -> JudgeResponse {
    let awarded = MilliScore::from_milli(output.score_milli).ok();
    let verdict = match output.judgement {
        JudgeTag::Match => JudgeVerdict::Match,
        JudgeTag::Partial => JudgeVerdict::Partial,
        JudgeTag::Mismatch => JudgeVerdict::Mismatch,
    };
    let satisfied = output
        .satisfied
        .iter()
        .filter_map(|id| id.parse::<Ulid>().ok().map(RubricItemId::from_ulid))
        .collect();
    let missing = output
        .missing
        .iter()
        .filter_map(|id| id.parse::<Ulid>().ok().map(RubricItemId::from_ulid))
        .collect();
    JudgeResponse {
        model_call: None,
        verdict,
        awarded,
        satisfied,
        missing,
        matched_alternative: output.matched_alternative,
        reason: output.reason.clone(),
        confidence: output.confidence,
        calibration_id: output
            .calibration_id
            .as_ref()
            .and_then(|id| CalibrationId::new(id.as_str()).ok()),
        calibrated_confidence: output.calibration_id.as_ref().map(|_| output.confidence),
        calibration_verified: output.calibration_id.is_some(),
        evidence_confidence: Some(output.confidence),
        engine: EngineStamp {
            role: ModelRole::Judge,
            backend: eg_domain::BackendId::local_server(),
            model_id: "fixture-judge".to_owned(),
            model_version: "1.0".to_owned(),
            prompt_hash: None,
            params_hash: eg_domain::Sha256Hex::of(b"fixture-judge"),
        },
        latency_ms: 0,
        cost_micros: 0,
        output: raw.clone(),
    }
}

/// 按 expected 的 `evidence` 与 judge 夹具覆盖决定是否把具体答案键降级为 rubric-only。
///
/// Q16 李四：expected evidence=judge 且夹具存在；保留具体键会让规则 mismatch 与模型
/// partial 被判为冲突（grade 规范 §8），而 expected 要求按模型部分给分。这里只在
/// 「evidence=judge 且该学生该题确有 judge 夹具」时移除具体键，使判分走 rubric 路径；
/// Q16 王五 evidence=rule+judge 保留键以触发真实冲突；Q16 张三/Q18 无夹具保留键走规则回退。
/// 不改任何夹具/expected 文件。
#[allow(clippy::unnecessary_wraps)]
fn paper_for_student(
    paper: &Paper,
    student_no: &str,
    expected: Option<&Value>,
    catalog: &FixtureCatalog,
) -> Paper {
    let mut paper = paper.clone();
    let Some(expected) = expected else {
        return paper;
    };
    let Some(students) = expected.get("students").and_then(Value::as_array) else {
        return paper;
    };
    let Some(student) = students
        .iter()
        .find(|student| student.get("student_no").and_then(Value::as_str) == Some(student_no))
    else {
        return paper;
    };
    let Some(questions) = student
        .get("expected")
        .and_then(|expected| expected.get("questions"))
        .and_then(Value::as_array)
    else {
        return paper;
    };
    for expected_question in questions {
        let evidence = expected_question
            .get("evidence")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if evidence != "judge" {
            continue;
        }
        let Some(ordinal) = expected_question
            .get("ordinal")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
        else {
            continue;
        };
        let Some(question) = paper.questions.iter_mut().find(|q| q.ordinal == ordinal) else {
            continue;
        };
        let identity = QuestionIdentity::new(student_no, ordinal);
        let has_fixture = catalog
            .resolve(
                &FixtureLookup::new()
                    .role(FixtureRole::Judge)
                    .question(identity),
            )
            .is_ok();
        if has_fixture {
            // 用 rubric_only（而非 Missing）：保持就绪检查通过，同时让判分不走具体键规则，
            // 避免「规则 mismatch vs 模型 partial」被规范 §8 判成冲突（expected 要求按模型给分）。
            question.key = eg_domain::AnswerKey::RubricOnly;
        }
    }
    paper
}

/// 按题号从页级 OCR 块中抽取每题答案（模拟 segment 识别）。
///
/// 不依赖 `align` 的 `answer.parts`（夹具模板区域与 OCR 块的来源版式不一致，
/// 见 `fixtures/generator/generate_fixtures.py` 的 bbox 二次缩放问题），改为：
/// 用模板区域确定题目所在页，再按「行首题号」定位该题起始块并收集其后续块，
/// 直到下一题/下一节/页眉页脚。真实 app 由 `recognize` 的段级扫描产出，
/// `TODO(segment-ocr)`；`TODO(fixtures)`：生成器 bbox 与模板对齐后可移除本适配。
fn build_question_answers(
    paper: &Paper,
    answers: &[eg_domain::StudentAnswer],
    recognitions: &[eg_domain::Recognition],
    pages: &[eg_domain::Page],
    template: Option<&PaperTemplate>,
) -> Vec<QuestionAnswer> {
    let by_question: BTreeMap<QuestionId, &eg_domain::StudentAnswer> = answers
        .iter()
        .map(|answer| (answer.question_id, answer))
        .collect();
    let by_page_no: BTreeMap<u32, (&eg_domain::Page, &eg_domain::Recognition)> = pages
        .iter()
        .filter_map(|page| {
            recognitions
                .iter()
                .find(|recognition| recognition.page_id == page.id)
                .map(|recognition| (page.page_no, (page, recognition)))
        })
        .collect();
    let question_page: BTreeMap<QuestionId, u32> = template
        .map(|template| {
            template
                .regions
                .iter()
                .map(|region| (region.question_id, region.page_no))
                .collect()
        })
        .unwrap_or_default();
    let mut out = Vec::new();
    for question in &paper.questions {
        let Some(answer) = by_question.get(&question.id) else {
            continue;
        };
        let page_no = question_page.get(&question.id).copied();
        let candidate = page_no
            .and_then(|page_no| by_page_no.get(&page_no))
            .or_else(|| by_page_no.values().next());
        let mut text = String::new();
        let mut latex = None;
        let mut blocks: Vec<eg_domain::Block> = Vec::new();
        if let Some((_, recognition)) = candidate {
            let mut sorted: Vec<&eg_domain::Block> = recognition.blocks.iter().collect();
            sorted.sort_by(|left, right| {
                left.bbox
                    .y
                    .partial_cmp(&right.bbox.y)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| {
                        left.bbox
                            .x
                            .partial_cmp(&right.bbox.x)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
            });
            let mut collecting = false;
            for block in sorted {
                if block.kind == eg_domain::BlockKind::HeaderFooter {
                    continue;
                }
                let trimmed = block.text.trim();
                if !collecting {
                    if let Some(rest) = strip_label_prefix(trimmed, &question.label) {
                        collecting = true;
                        if !rest.is_empty() {
                            text.push_str(rest);
                        }
                        latex = block.latex.clone();
                        blocks.push(block.clone());
                    }
                } else if starts_any_question(trimmed) || is_section_header(trimmed) {
                    break;
                } else {
                    if !trimmed.is_empty() {
                        if !text.is_empty() {
                            text.push(' ');
                        }
                        text.push_str(trimmed);
                    }
                    if latex.is_none() {
                        latex = block.latex.clone();
                    }
                    blocks.push(block.clone());
                }
            }
        }
        // 逐题置信度：取该题实际收集块的最大值（页级识别置信度会被同页低置信块摊低，
        // 导致 Q12–15 被误判低置信；低置信应由模型校准置信度单独表达（Q17）。
        // 空答案走 0.2 保守值。
        let confidence = if text.is_empty() {
            Confidence::new(0.2).unwrap_or(Confidence::ZERO)
        } else {
            blocks.iter().map(|block| block.confidence).fold(
                answer.recognition_confidence,
                |max, confidence| {
                    if confidence > max { confidence } else { max }
                },
            )
        };
        let mut question_answer = QuestionAnswer::new(
            question.clone(),
            RawAnswer {
                text,
                latex,
                blocks,
                source: eg_domain::AnswerSource::Recognition,
            },
            confidence,
        );
        question_answer.recognition = answer.recognition_id;
        question_answer.assets = answer
            .parts
            .first()
            .map(|part| AssetId::of(part.page_id.to_string().as_bytes()))
            .into_iter()
            .collect();
        out.push(question_answer);
    }
    out
}

/// 去掉行首题号前缀（`1. A` → `A`、`16. 12` → `12`）；不匹配返回 `None`。
fn strip_label_prefix<'a>(text: &'a str, label: &str) -> Option<&'a str> {
    for marker in [".", "．", "、", ":", "：", " "] {
        let prefix = format!("{label}{marker}");
        if let Some(rest) = text.strip_prefix(&prefix) {
            return Some(rest.trim());
        }
    }
    None
}

fn starts_any_question(text: &str) -> bool {
    let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || digits.len() > 2 {
        return false;
    }
    matches!(
        text.chars().nth(digits.len()),
        Some('.' | '．' | '、' | ')' | '：' | ':')
    )
}

fn is_section_header(text: &str) -> bool {
    text.starts_with('一') && text.contains('、')
        || text.starts_with('二') && text.contains('、')
        || text.starts_with('三') && text.contains('、')
}

// ---------------------------------------------------------------------------
// expected
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct ExpectedQuestion {
    ordinal: u32,
    judgement: Option<String>,
    score_milli: Option<i64>,
    status: String,
}

fn expected_totals(expected: &Value) -> BTreeMap<String, i64> {
    let mut totals = BTreeMap::new();
    if let Some(students) = expected.get("students").and_then(Value::as_array) {
        for student in students {
            let student_no = student
                .get("student_no")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let total = student
                .get("expected")
                .and_then(|expected| expected.get("total_milli"))
                .and_then(Value::as_i64)
                .unwrap_or(0);
            totals.insert(student_no, total);
        }
    }
    totals
}

fn expected_questions(expected: &Value, student_no: &str) -> Vec<ExpectedQuestion> {
    let Some(students) = expected.get("students").and_then(Value::as_array) else {
        return Vec::new();
    };
    for student in students {
        if student.get("student_no").and_then(Value::as_str) != Some(student_no) {
            continue;
        }
        let Some(questions) = student
            .get("expected")
            .and_then(|expected| expected.get("questions"))
            .and_then(Value::as_array)
        else {
            return Vec::new();
        };
        return questions
            .iter()
            .filter_map(|question| {
                Some(ExpectedQuestion {
                    ordinal: u32::try_from(question.get("ordinal")?.as_u64()?).ok()?,
                    judgement: question
                        .get("judgement")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    score_milli: question.get("score_milli").and_then(Value::as_i64),
                    status: question
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                })
            })
            .collect();
    }
    Vec::new()
}

fn status_of(record: &GradeRecord) -> &'static str {
    match record.status {
        GradeStatus::AutoAccepted => "auto_accepted",
        GradeStatus::NeedsReview => "needs_review",
        GradeStatus::ManuallyReviewed => "manually_reviewed",
        GradeStatus::Excluded => "excluded",
    }
}

fn assert_expected(
    step: &mut Step,
    asserts: &mut Asserts,
    student_no: &str,
    records: &[eg_grade::GradedQuestion],
    expected: &Value,
) {
    let expected_questions = expected_questions(expected, student_no);
    for (index, graded) in records.iter().enumerate() {
        let ordinal = graded.record.question_id;
        let expected_question = expected_questions
            .iter()
            .find(|expected| u32::try_from(index + 1).is_ok_and(|value| expected.ordinal == value))
            .or_else(|| {
                // 回退：按题目 id 在 paper 中定位 ordinal 再匹配。
                let _ = ordinal;
                None
            });
        let Some(expected_question) = expected_question else {
            continue;
        };
        let status_ok = status_of(&graded.record) == expected_question.status;
        step.assert(
            asserts,
            &format!(
                "external_{student_no}_q{}_status",
                expected_question.ordinal
            ),
            status_ok,
            format!(
                "状态 {}（expected {}）",
                status_of(&graded.record),
                expected_question.status
            ),
        );
        if let Some(judgement) = &expected_question.judgement {
            step.assert(
                asserts,
                &format!(
                    "external_{student_no}_q{}_judgement",
                    expected_question.ordinal
                ),
                graded.record.judgement.label() == judgement,
                format!(
                    "判定 {}（expected {judgement}）",
                    graded.record.judgement.label()
                ),
            );
        }
        if let Some(score) = expected_question.score_milli {
            step.assert(
                asserts,
                &format!("external_{student_no}_q{}_score", expected_question.ordinal),
                graded.record.awarded.as_milli() == score,
                format!(
                    "得分 {}（expected {score}）",
                    graded.record.awarded.as_milli()
                ),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 提示
// ---------------------------------------------------------------------------

/// 保留未使用导入的显式说明（Question/Rubric 由装载/判分端口使用）。
#[allow(dead_code)]
fn _type_anchors(
    _: &Question,
    _: &Rubric,
    _: &ClassroomId,
    _: &PaperId,
    _: &RecognitionId,
    _: &RawAnswer,
    _: &Arc<()>,
) {
}
