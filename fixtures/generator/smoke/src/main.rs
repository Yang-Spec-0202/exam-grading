//! `fixtures/` 冒烟程序（开发/验收用；不进入产品 crate，也不在仓库内构建）。
//!
//! 验证三件事（README §5/§6）：
//! 1. 领域层可直接消费：`paper.json`/`template.json`/`students.json` 反序列化 + 校验通过；
//! 2. eg-inference 夹具**键命中**：按 index 的 prompt/图片字节重算 `fixture_key`，回放字节一致；
//!    负向样例必须被 `eg-recognize` 结构校验 / `eg-inference` Judge Schema 拒绝；
//! 3. 真实 capture 导入：9 张图 → 期望的提交/去重/EXIF/质量结论；align 文件名身份符合期望。
//!
//! 由 `fixtures/generator/smoke.sh` 在 /tmp 临时 cargo 工程中构建运行：
//! `cargo run -- --repo <repo>`

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use eg_align::{AlignSettings, IdentityStatus, resolve_identity};
use eg_capture::platform_bridge::{AssetStoreAdapter, CancelAdapter, ClockAdapter, IdAdapter};
use eg_capture::{
    FileInput, ImportKind, ImportOptions, ImportRequest, ImportSettings, Importer, MemoryEventSink,
    NoKnownSubmissions,
};
use eg_domain::{
    Classroom, MilliScore, Paper, PaperTemplate, QualityVerdict, Rotation, Student,
};
use eg_inference::fixture::{FixtureStore, fixture_key};
use eg_inference::schema::JudgeOutput;
use eg_platform::{CancelToken, DataDirs, FixedClock, Platform, SharedClock};
use serde_json::Value;

const PAGE_W: u32 = 2480;
const PAGE_H: u32 = 3507;

fn fail(message: &str) -> ! {
    eprintln!("[FAIL] {message}");
    std::process::exit(1);
}

fn read_json(path: &Path) -> Value {
    let text = std::fs::read_to_string(path).unwrap_or_else(|error| fail(&format!("读取 {path:?}：{error}")));
    serde_json::from_str(&text).unwrap_or_else(|error| fail(&format!("解析 {path:?}：{error}")))
}

fn main() {
    let repo = match std::env::args().nth(2) {
        Some(repo) => PathBuf::from(repo),
        None => fail("用法：smoke --repo <repo>"),
    };
    let fixtures = repo.join("fixtures");

    // ------------------------------------------------------------ 1. 领域 JSON 可直接消费
    let paper_text = std::fs::read_to_string(fixtures.join("paper-20q/paper.json")).expect("paper");
    let paper: Paper = serde_json::from_str(&paper_text).unwrap_or_else(|error| fail(&format!("paper.json 反序列化失败：{error}")));
    paper.validate().unwrap_or_else(|error| fail(&format!("paper 校验失败：{error}")));
    assert_eq!(paper.questions.len(), 20);
    assert_eq!(paper.total_score.as_milli(), 150_000);

    let template_text = std::fs::read_to_string(fixtures.join("paper-20q/template.json")).expect("template");
    let template: PaperTemplate = serde_json::from_str(&template_text)
        .unwrap_or_else(|error| fail(&format!("template.json 反序列化失败：{error}")));
    template.validate().unwrap_or_else(|error| fail(&format!("模板校验失败：{error}")));
    template
        .ensure_auto_align_allowed(&paper)
        .unwrap_or_else(|error| fail(&format!("模板版本与试卷不匹配：{error}")));

    let students_text = std::fs::read_to_string(fixtures.join("paper-20q/students.json")).expect("students");
    let students: Vec<Student> = serde_json::from_str(&students_text)
        .unwrap_or_else(|error| fail(&format!("students.json 反序列化失败：{error}")));
    let classroom_text = std::fs::read_to_string(fixtures.join("paper-20q/classroom.json")).expect("classroom");
    let classroom: Classroom = serde_json::from_str(&classroom_text)
        .unwrap_or_else(|error| fail(&format!("classroom.json 反序列化失败：{error}")));
    let roster = eg_align::Roster::new(classroom.id, students.clone())
        .unwrap_or_else(|error| fail(&format!("名单构造失败：{error}")));

    // ------------------------------------------------------------ 2. eg-inference 夹具键
    let ocr_dir = fixtures.join("ocr");
    let ocr_index = read_json(&ocr_dir.join("index.json"));
    let backend = &ocr_index["backend"];
    let model_id = backend["model_id"].as_str().unwrap_or_default();
    let model_version = backend["model_version"].as_str().unwrap_or_default();
    let prompt_schema = backend["prompt_schema_version"].as_str().unwrap_or_default();
    let emit_latex = ocr_index["emit_latex"].as_bool().unwrap_or(true);
    let ocr_store = FixtureStore::load_dir(&ocr_dir.join("pages"))
        .unwrap_or_else(|error| fail(&format!("加载 ocr/pages 失败：{error}")));
    let mut ocr_hits = 0_usize;
    for record in ocr_index["records"].as_array().expect("records") {
        let id = record["id"].as_str().expect("id");
        let width = u32::try_from(record["ocr_input_width"].as_u64().expect("width")).expect("width");
        let height = u32::try_from(record["ocr_input_height"].as_u64().expect("height")).expect("height");
        let input_path = fixtures.join(record["ocr_input_image"].as_str().expect("input"));
        let input = std::fs::read(&input_path).expect("读 OCR 输入图");
        let prompt = eg_recognize::prompt::render_page_prompt(width, height, emit_latex);
        let key = fixture_key(model_id, model_version, prompt_schema, &prompt, &input);
        let key_text = key.to_hex();
        let replay = ocr_store
            .get(&key_text)
            .unwrap_or_else(|| fail(&format!("OCR fixture 键未命中：{id} {key_text}")));
        let parsed = eg_recognize::schema::parse_output(&replay.output_raw)
            .unwrap_or_else(|error| fail(&format!("OCR 输出结构校验失败（{id}）：{error}")));
        eg_recognize::semantic::validate_semantics(
            &parsed,
            eg_recognize::semantic::SemanticOptions {
                emit_latex: true,
                first_pass: true,
            },
        )
        .unwrap_or_else(|error| fail(&format!("OCR 语义校验失败（{id}）：{error}")));
        assert!(!parsed.blocks.is_empty(), "OCR 输出至少 1 个块");
        let page = parsed.page.unwrap_or_else(|| fail("OCR 输出应声明 page 尺寸"));
        assert_eq!((page.width, page.height), (width, height));
        ocr_hits += 1;
    }
    assert_eq!(ocr_hits, 7, "OCR 记录数应为 7");

    // 负向 OCR：缺 blocks / 条码块必须被拒绝。
    let ocr_negative = FixtureStore::load_dir(&ocr_dir.join("negative"))
        .unwrap_or_else(|error| fail(&format!("加载 ocr/negative 失败：{error}")));
    let mut rejected = 0_usize;
    for key in ocr_negative.keys() {
        let entry = ocr_negative.get(key).expect("负向夹具");
        let rejected_here = match eg_recognize::schema::parse_output(&entry.output_raw) {
            Err(_) => true,
            Ok(parsed) => eg_recognize::semantic::validate_semantics(
                &parsed,
                eg_recognize::semantic::SemanticOptions {
                    emit_latex: true,
                    first_pass: true,
                },
            )
            .is_err(),
        };
        if rejected_here {
            rejected += 1;
        }
    }
    assert_eq!(rejected, 2, "两个 OCR 负向样例都必须被结构/语义校验拒绝");

    let judge_dir = fixtures.join("judge");
    let judge_index = read_json(&judge_dir.join("index.json"));
    let judge_backend = &judge_index["backend"];
    let judge_prompt = std::fs::read(&judge_dir.join("prompt.txt")).expect("judge prompt");
    let judge_store = FixtureStore::load_dir(&judge_dir.join("responses"))
        .unwrap_or_else(|error| fail(&format!("加载 judge/responses 失败：{error}")));
    let max_by_ordinal: BTreeMap<u32, MilliScore> = paper
        .questions
        .iter()
        .map(|question| (question.ordinal, question.max_score))
        .collect();
    let mut judge_hits = 0_usize;
    for record in judge_index["records"].as_array().expect("records") {
        let id = record["id"].as_str().expect("id");
        let ordinal = u32::try_from(record["question_ordinal"].as_u64().expect("ordinal")).expect("ordinal");
        let input_path = fixtures.join(record["input_file"].as_str().expect("input_file"));
        let input = std::fs::read(&input_path).expect("读 judge 输入");
        let key = fixture_key(
            judge_backend["model_id"].as_str().unwrap_or_default(),
            judge_backend["model_version"].as_str().unwrap_or_default(),
            judge_backend["prompt_schema_version"].as_str().unwrap_or_default(),
            std::str::from_utf8(&judge_prompt).expect("prompt utf8"),
            &input,
        );
        let key_text = key.to_hex();
        let replay = judge_store
            .get(&key_text)
            .unwrap_or_else(|| fail(&format!("judge fixture 键未命中：{id} {key_text}")));
        let value: Value = serde_json::from_str(&replay.output_raw).expect("judge 输出 JSON");
        let max_score = *max_by_ordinal.get(&ordinal).expect("题目满分");
        JudgeOutput::validate(&value, max_score)
            .unwrap_or_else(|error| fail(&format!("judge 输出 Schema 校验失败（{id}）：{error}")));
        judge_hits += 1;
    }
    assert_eq!(judge_hits, 8, "judge 记录数应为 8");

    let judge_negative = FixtureStore::load_dir(&judge_dir.join("negative"))
        .unwrap_or_else(|error| fail(&format!("加载 judge/negative 失败：{error}")));
    let mut judge_rejected = 0_usize;
    for key in judge_negative.keys() {
        let entry = judge_negative.get(key).expect("负向夹具");
        let value: Value = serde_json::from_str(&entry.output_raw).expect("JSON");
        if JudgeOutput::validate(&value, MilliScore::from_milli(5000).expect("5000")).is_err() {
            judge_rejected += 1;
        }
    }
    assert_eq!(judge_rejected, 1, "judge 负向样例必须被拒绝");

    // ------------------------------------------------------------ 3. 真实 capture 导入
    let temp = tempfile::tempdir().expect("临时目录");
    let dirs = DataDirs::with_root(temp.path()).expect("数据目录");
    let clock: SharedClock = Arc::new(FixedClock::from_rfc3339("2026-11-04T00:00:00Z").expect("固定时钟"));
    let platform = Platform::open_with(dirs, clock.clone()).expect("打开 platform");
    let images_manifest = read_json(&fixtures.join("images/manifest.json"));
    let entries = images_manifest["files"].as_array().expect("files");
    let files: Vec<FileInput> = entries
        .iter()
        .map(|entry| {
            let relative = entry["file"].as_str().expect("file");
            let bytes = std::fs::read(fixtures.join(relative)).expect("读图片");
            let name = Path::new(relative)
                .file_name()
                .and_then(|name| name.to_str())
                .expect("文件名")
                .to_owned();
            FileInput::from_bytes(name, bytes)
        })
        .collect();
    let settings = ImportSettings::default();
    let events = MemoryEventSink::new();
    let assets = AssetStoreAdapter::new(platform.assets());
    let clock_adapter = ClockAdapter::new(clock.as_ref());
    let ids = IdAdapter::new(platform.ids());
    let cancel = CancelToken::new();
    let cancel_adapter = CancelAdapter::new(&cancel);
    let importer = Importer::new(
        &settings,
        &assets,
        &events,
        &clock_adapter,
        &ids,
        &cancel_adapter,
        &NoKnownSubmissions,
    );
    let outcome = importer
        .run(ImportRequest {
            batch_id: eg_domain::Ulid::from_u128(49),
            kind: ImportKind::StudentWork,
            classroom_id: classroom.id,
            paper_id: paper.id,
            files,
            options: ImportOptions::default(),
        })
        .unwrap_or_else(|error| fail(&format!("capture 导入失败：{error}")));

    assert_eq!(outcome.report.files_total, 9, "应处理 9 个文件");
    assert_eq!(outcome.report.files_ok, 9, "9 个文件都应成功解码");
    assert_eq!(outcome.submissions.len(), 8, "6 学生页 + 名单外 + EXIF = 8 份提交");
    assert_eq!(
        outcome
            .report
            .duplicates
            .iter()
            .filter(|hit| matches!(hit.category, eg_capture::DuplicateCategory::SameBatch))
            .count(),
        1,
        "张三_p2-copy 必须命中批内去重"
    );

    let mut exif_checked = false;
    let mut stems = Vec::new();
    for submission in &outcome.submissions {
        stems.push(submission.source_stem.clone());
        for page in &submission.pages {
            if matches!(page.quality.verdict, QualityVerdict::Rejected { .. }) {
                let reasons: Vec<&str> = outcome
                    .report
                    .rejected_pages
                    .iter()
                    .filter(|rejected| rejected.submission_id == Some(submission.id))
                    .flat_map(|rejected| rejected.reasons.clone())
                    .collect();
                fail(&format!(
                    "夹具页面被质量门控拒收：{}（reasons={reasons:?}，quality={:?}）",
                    submission.source_stem, page.quality
                ));
            }
            if submission.source_stem == "王五_exif6" {
                assert_eq!(page.rotation_applied, Rotation::CW_90, "EXIF 6 必须校正 90°");
                assert_eq!(page.exif_orientation.value(), 6);
                exif_checked = true;
            }
        }
    }
    assert!(exif_checked, "必须覆盖 EXIF 旋转样例");
    let has_stem = |prefix: &str| stems.iter().any(|stem| stem.starts_with(prefix));
    assert!(has_stem("张三_p"), "缺少张三答卷页：{stems:?}");
    assert!(has_stem("李四_期中_p"), "缺少李四答卷页：{stems:?}");
    assert!(has_stem("SO2026-003_p"), "缺少王五答卷页：{stems:?}");
    assert!(has_stem("IMG_20261104_001"), "缺少相机文件名样卷：{stems:?}");

    // align 文件名身份：3 名学生确认、相机名未决、EXIF 样卷按 token「王五」确认。
    let settings_align = AlignSettings::default();
    let no_hints = eg_align::NoIdentityHints;
    let mut confirmed = 0_usize;
    let mut unresolved = 0_usize;
    for submission in &outcome.submissions {
        let conclusion = resolve_identity(submission, &roster, &settings_align, &no_hints)
            .unwrap_or_else(|error| fail(&format!("身份匹配失败：{error}")));
        match conclusion.status {
            IdentityStatus::Confirmed { .. } => {
                confirmed += 1;
                if submission.source_stem == "IMG_20261104_001" {
                    fail("相机文件名不应被自动确认");
                }
            }
            IdentityStatus::Unresolved { .. } => {
                unresolved += 1;
                assert_eq!(
                    submission.source_stem, "IMG_20261104_001",
                    "只有相机文件名应未决：{}",
                    submission.source_stem
                );
            }
            IdentityStatus::OutsideRoster { .. } => fail("默认配置不应归入名单外"),
        }
    }
    assert_eq!(confirmed, 7, "6 学生页 + EXIF 样卷应确认");
    assert_eq!(unresolved, 1, "名单外相机文件名应未决");

    // 页面尺寸 sanity：非 EXIF 页 = 2480×3507。
    for submission in &outcome.submissions {
        if submission.source_stem == "王五_exif6" {
            continue;
        }
        let page = &submission.pages[0];
        assert_eq!((page.width, page.height), (PAGE_W, PAGE_H), "页面存储尺寸应为 A4@300DPI");
    }
    platform.close().expect("关闭 platform");

    println!(
        "[ok] smoke 通过：paper 20 题/150000；OCR 7 命中 + 2 拒绝；judge 8 命中 + 1 拒绝；\
         capture 9 文件/8 提交（含 1 去重、1 EXIF）；align 7 确认 + 1 未决"
    );
}
