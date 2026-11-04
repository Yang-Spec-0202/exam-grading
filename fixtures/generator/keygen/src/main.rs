//! 夹具键生成器：把 `generate_fixtures.py` 产出的规格转成 **eg-inference 夹具 JSON**
//! 并写 `fixtures/MANIFEST.sha256`。
//!
//! 为什么需要这一步：`eg-inference` 的夹具键是
//! `SHA-256(model_id | model_version | prompt_schema_version | prompt | input)`，
//! 其中 OCR 的 `prompt` 由 `eg-recognize::prompt::render_page_prompt(width,height,emit_latex)`
//! 渲染。为避免在 Python 侧复制——导致与真实实现漂移——这里直接调用仓库代码计算键。
//!
//! 用法（由 `fixtures/generator/regenerate.sh` 调用）：
//! `cargo run --manifest-path fixtures/generator/keygen/Cargo.toml -- --repo <repo> --build <build>`

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use eg_domain::Sha256Hex;
use eg_inference::fixture::fixture_key;
use serde_json::{Value, json};

fn arg(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    let mut index = 0;
    while index + 1 < args.len() {
        if args[index] == name {
            return Some(args[index + 1].clone());
        }
        index += 1;
    }
    None
}

fn sha(path: &Path) -> String {
    Sha256Hex::of(&fs::read(path).expect("读取文件")).to_hex()
}

fn write_json(path: &Path, value: &Value) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("创建目录");
    }
    let mut text = serde_json::to_string_pretty(value).expect("序列化 JSON");
    text.push('\n');
    fs::write(path, text).expect("写 JSON");
}

fn fixture_entry(
    key: &str,
    model_id: &str,
    model_version: &str,
    prompt_schema_version: &str,
    output_raw: &str,
) -> Value {
    json!({
        "schema_version": 1,
        "key": key,
        "model_id": model_id,
        "model_version": model_version,
        "prompt_schema_version": prompt_schema_version,
        "output_raw": output_raw,
    })
}

fn reset_dir(path: &Path) {
    if path.exists() {
        fs::remove_dir_all(path).expect("清理旧生成目录");
    }
    fs::create_dir_all(path).expect("创建目录");
}

fn build_ocr(repo: &Path, _build: &Path, spec: &Value) -> Value {
    let model_id = spec["backend"]["model_id"].as_str().expect("model_id");
    let model_version = spec["backend"]["model_version"].as_str().expect("model_version");
    let schema_version = spec["backend"]["prompt_schema_version"]
        .as_str()
        .expect("prompt_schema_version");
    let emit_latex = spec["emit_latex"].as_bool().expect("emit_latex");
    let fixture_dir = repo.join("fixtures/ocr");
    let pages_dir = fixture_dir.join("pages");
    let prompts_dir = fixture_dir.join("prompts");
    let negative_dir = fixture_dir.join("negative");
    reset_dir(&pages_dir);
    reset_dir(&prompts_dir);
    reset_dir(&negative_dir);

    let mut records = Vec::new();
    for record in spec["records"].as_array().expect("records") {
        let id = record["id"].as_str().expect("id");
        let input_image = record["ocr_input_image"].as_str().expect("ocr_input_image");
        let width = record["ocr_input_width"].as_u64().expect("width") as u32;
        let height = record["ocr_input_height"].as_u64().expect("height") as u32;
        let output_raw = record["output_raw"].as_str().expect("output_raw");
        let image_path = repo.join("fixtures").join(input_image);
        let input_bytes = fs::read(&image_path).expect("读取 OCR 输入图");
        let prompt = render_ocr_prompt(width, height, emit_latex);
        let key = fixture_key(model_id, model_version, schema_version, &prompt, &input_bytes);
        let prompt_sha = Sha256Hex::of(prompt.as_bytes()).to_hex();
        let input_sha = Sha256Hex::of(&input_bytes).to_hex();
        let output_sha = Sha256Hex::of(output_raw.as_bytes()).to_hex();

        let prompt_path = prompts_dir.join(format!("{id}.txt"));
        fs::write(&prompt_path, prompt.as_bytes()).expect("写提示词");
        let fixture_path = pages_dir.join(format!("{id}.json"));
        write_json(
            &fixture_path,
            &fixture_entry(
                &key.to_hex(),
                model_id,
                model_version,
                schema_version,
                output_raw,
            ),
        );
        let block_count = serde_json::from_str::<Value>(output_raw)
            .ok()
            .and_then(|value| value.get("blocks").and_then(Value::as_array).map(Vec::len))
            .unwrap_or(0);
        records.push(json!({
            "id": id,
            "student_no": record["student_no"],
            "submission_stem": record["submission_stem"],
            "page_no": record["page_no"],
            "source_image": record["source_image"],
            "source_sha256": record["source_sha256"],
            "ocr_input_image": input_image,
            "ocr_input_sha256": input_sha,
            "ocr_input_width": width,
            "ocr_input_height": height,
            "prompt_file": format!("ocr/prompts/{id}.txt"),
            "prompt_sha256": prompt_sha,
            "fixture": format!("ocr/pages/{id}.json"),
            "fixture_key": key.to_hex(),
            "output_sha256": output_sha,
            "blocks_expected": block_count,
        }));
    }

    let mut negatives = Vec::new();
    for negative in spec["negative"].as_array().expect("negative") {
        let id = negative["id"].as_str().expect("negative id");
        let output_raw = negative["output_raw"].as_str().expect("negative output_raw");
        let marker = format!("negative:{id}");
        let key = fixture_key(model_id, model_version, schema_version, &marker, marker.as_bytes());
        let path = negative_dir.join(format!("{id}.json"));
        write_json(
            &path,
            &fixture_entry(
                &key.to_hex(),
                model_id,
                model_version,
                schema_version,
                output_raw,
            ),
        );
        negatives.push(json!({
            "id": id,
            "note": negative["note"],
            "file": format!("ocr/negative/{id}.json"),
            "expected": "recognize 结构/语义校验必须拒绝，不得作为正向回放",
        }));
    }

    write_json(
        &fixture_dir.join("index.json"),
        &json!({
            "schema_version": 1,
            "kind": "ocr",
            "backend": spec["backend"].clone(),
            "emit_latex": emit_latex,
            "key_rule": "eg_inference::fixture::fixture_key(model_id, model_version, prompt_schema_version, prompt, image_bytes)；prompt 由 eg_recognize::prompt::render_page_prompt(width, height, emit_latex) 确定性渲染",
            "usage": {
                "mode_a_exact_replay": "把 records[].ocr_input_image 的字节作为 ImageInput.bytes、按 records[].ocr_input_width/height 渲染提示词，交给 eg_inference FixtureBackend 回放：键逐字节命中。",
                "mode_b_semantic_mapping": "真实 capture 派生图字节与这里的输入不同（较大/去透视），按 records[].student_no + page_no 或 submission_stem + page_no 取 output_raw，不使用键。",
            },
            "records": records,
            "negative": negatives,
        }),
    );
    json!({ "ocr_records": spec["records"].as_array().map_or(0, Vec::len) })
}

fn render_ocr_prompt(width: u32, height: u32, emit_latex: bool) -> String {
    eg_recognize::prompt::render_page_prompt(width, height, emit_latex)
}

fn build_judge(repo: &Path, _build: &Path, spec: &Value) -> Value {
    let model_id = spec["backend"]["model_id"].as_str().expect("model_id");
    let model_version = spec["backend"]["model_version"].as_str().expect("model_version");
    let schema_version = spec["backend"]["prompt_schema_version"]
        .as_str()
        .expect("prompt_schema_version");
    let prompt = spec["prompt"].as_str().expect("prompt");
    let fixture_dir = repo.join("fixtures/judge");
    let prompt_path = fixture_dir.join("prompt.txt");
    let inputs_dir = fixture_dir.join("inputs");
    let responses_dir = fixture_dir.join("responses");
    let negative_dir = fixture_dir.join("negative");
    reset_dir(&inputs_dir);
    reset_dir(&responses_dir);
    reset_dir(&negative_dir);
    if let Some(parent) = prompt_path.parent() {
        fs::create_dir_all(parent).expect("创建目录");
    }
    fs::write(&prompt_path, prompt.as_bytes()).expect("写 judge 提示词");
    let prompt_sha = Sha256Hex::of(prompt.as_bytes()).to_hex();

    let mut records = Vec::new();
    for record in spec["records"].as_array().expect("records") {
        let id = record["id"].as_str().expect("id");
        let input_utf8 = record["input_utf8"].as_str().expect("input_utf8");
        let output_raw = record["output_raw"].as_str().expect("output_raw");
        let key = fixture_key(model_id, model_version, schema_version, prompt, input_utf8.as_bytes());
        let input_path = inputs_dir.join(format!("{id}.json"));
        fs::write(&input_path, input_utf8.as_bytes()).expect("写 judge 输入");
        let response_path = responses_dir.join(format!("{id}.json"));
        write_json(
            &response_path,
            &fixture_entry(
                &key.to_hex(),
                model_id,
                model_version,
                schema_version,
                output_raw,
            ),
        );
        records.push(json!({
            "id": id,
            "student_no": record["student_no"],
            "question_ordinal": record["question_ordinal"],
            "case": record["case"],
            "input_file": format!("judge/inputs/{id}.json"),
            "input_sha256": Sha256Hex::of(input_utf8.as_bytes()).to_hex(),
            "prompt_file": "judge/prompt.txt",
            "prompt_sha256": prompt_sha,
            "fixture": format!("judge/responses/{id}.json"),
            "fixture_key": key.to_hex(),
            "output_sha256": Sha256Hex::of(output_raw.as_bytes()).to_hex(),
        }));
    }

    let mut negatives = Vec::new();
    for negative in spec["negative"].as_array().expect("negative") {
        let id = negative["id"].as_str().expect("negative id");
        let output_raw = negative["output_raw"].as_str().expect("negative output_raw");
        let marker = format!("negative:{id}");
        let key = fixture_key(model_id, model_version, schema_version, &marker, marker.as_bytes());
        let path = negative_dir.join(format!("{id}.json"));
        write_json(
            &path,
            &fixture_entry(
                &key.to_hex(),
                model_id,
                model_version,
                schema_version,
                output_raw,
            ),
        );
        negatives.push(json!({
            "id": id,
            "note": negative["note"],
            "file": format!("judge/negative/{id}.json"),
            "expected": "eg_inference JudgeOutput::validate 必须拒绝",
        }));
    }

    write_json(
        &fixture_dir.join("index.json"),
        &json!({
            "schema_version": 1,
            "kind": "judge",
            "backend": spec["backend"].clone(),
            "prompt_file": "judge/prompt.txt",
            "prompt_sha256": prompt_sha,
            "key_rule": "eg_inference::fixture::fixture_key(model_id, model_version, prompt_schema_version, prompt_bytes, input_bytes)；prompt/输入文件都在本目录，可逐字节复现",
            "records": records,
            "negative": negatives,
        }),
    );
    json!({ "judge_records": spec["records"].as_array().map_or(0, Vec::len) })
}

fn collect_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(&directory).expect("遍历目录") {
            let entry = entry.expect("目录项");
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().and_then(|name| name.to_str()) != Some("MANIFEST.sha256") {
                files.push(path);
            }
        }
    }
    files
}

fn main() {
    let repo = PathBuf::from(arg("--repo").expect("缺少 --repo"));
    let build = PathBuf::from(arg("--build").expect("缺少 --build"));
    let ocr_spec: Value =
        serde_json::from_str(&fs::read_to_string(build.join("ocr-spec.json")).expect("读 ocr-spec"))
            .expect("解析 ocr-spec");
    let judge_spec: Value = serde_json::from_str(
        &fs::read_to_string(build.join("judge-spec.json")).expect("读 judge-spec"),
    )
    .expect("解析 judge-spec");

    let ocr_summary = build_ocr(&repo, &build, &ocr_spec);
    let judge_summary = build_judge(&repo, &build, &judge_spec);

    // MANIFEST：fixtures/ 下除 MANIFEST 自身外的全部文件，按相对路径排序。
    let fixtures = repo.join("fixtures");
    let mut entries: BTreeMap<String, String> = BTreeMap::new();
    for path in collect_files(&fixtures) {
        let relative = path
            .strip_prefix(&fixtures)
            .expect("相对路径")
            .to_string_lossy()
            .replace('\\', "/");
        entries.insert(relative, sha(&path));
    }
    let mut manifest = String::new();
    for (relative, digest) in &entries {
        manifest.push_str(digest);
        manifest.push_str("  ");
        manifest.push_str(relative);
        manifest.push('\n');
    }
    fs::write(fixtures.join("MANIFEST.sha256"), manifest).expect("写 MANIFEST");

    println!(
        "[ok] keygen 完成：{}，judge {}，MANIFEST {} 个文件",
        ocr_summary["ocr_records"],
        judge_summary["judge_records"],
        entries.len()
    );
}
