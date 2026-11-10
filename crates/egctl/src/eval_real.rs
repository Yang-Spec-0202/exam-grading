//! `egctl eval --real-judge`：真实本地 4B judge 的 S3 复测（task-70）。
//!
//! - 只处理 `solution-200.json` 的 `rubric_itemized` 条目；manual/rubric_only 分流单独统计（不假判）。
//! - 逐评分项请求本地 OpenAI-compatible `/v1/chat/completions`（temperature=0、max_tokens=256、
//!   stream=false、response_format=json_object、超时≥20s），输出必须过 `JudgeOutput::validate`。
//! - 模型给出的 `match` 视为该评分项满足；`partial/mismatch` 视为不满足（严格二值项）；
//!   满足项喂 `eg-grade::aggregate`（stepwise 依赖/累加/全对）后与 gold expected 对比。
//! - gold 的 `expected.steps[].matched` 是合成规则标签，不是模型标注；真实 ECE 用模型实际 confidence。
//! - 不调参、不改 gold；失败/超时逐项分类并保留样本（脱敏）。

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eg_domain::{AnswerKey, MilliScore, QuestionKind, Rubric, RubricItemId};
use eg_grade::aggregate::aggregate;
use eg_inference::schema::{JudgeOutput, JudgeTag};
use serde::Deserialize;
use serde_json::{Value, json};

const SEED: &str = "20261109";
const SYSTEM_PROMPT: &str = "你是数学判分器。只输出一个 JSON 对象，不要 Markdown/解释。只输出四个字段：judgement（match 或 mismatch）、score_milli（match=该评分项满分，mismatch=0）、reason（中文，≤120字）、confidence（0–1，表示对本次判断的把握）。不要输出 satisfied/missing/matched_alternative/calibration_id。只判断给定评分项是否被学生步骤满足。";

/// 真实 judge 评估选项。
#[derive(Debug, Clone)]
pub struct Options {
    /// gold 目录。
    pub gold: PathBuf,
    /// `/v1` 端点（如 `http://127.0.0.1:8082/v1`）。
    pub endpoint: String,
    /// 模型名（llama-server 不校验，用于日志）。
    pub model: String,
    /// 抽样条数；`None`=默认 60；`Some(0)`=全量。
    pub sample: Option<usize>,
    /// 并发请求数（默认 1；>1 时延迟含排队，报告标注）。
    pub concurrency: usize,
    /// 单次请求超时。
    pub timeout: Duration,
    /// 校准模式：固定 seed 拆 calibration/eval（各 sample 条），校准后仅 eval 评估。
    pub calibrate: bool,
    /// 提示词 A/B：在独立的 ab_sample 条上同批对比 v1/v2。
    pub ab: bool,
    /// A/B 样本条数（默认 30）。
    pub ab_sample: Option<usize>,
    /// 校准模式下 eval 样本条数（默认 = sample；v2 复测用 120）。
    pub eval_sample: Option<usize>,
    /// 校准/正式评估使用的提示词版本（A/B 始终同时跑 v1/v2）。
    pub prompt: PromptVersion,
    /// 拟合后把校准表写到 `config/calibration/<id>.json`（默认不写）。
    pub write_calibration: bool,
    /// production 校准表路径（`--apply-calibration`）；严格 identity 匹配后用于门禁模拟。
    pub apply_calibration: Option<PathBuf>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            gold: PathBuf::from("fixtures/gold"),
            endpoint: "http://127.0.0.1:8082/v1".to_owned(),
            model: "qwen3.5-4b-instruct".to_owned(),
            sample: Some(60),
            concurrency: 1,
            timeout: Duration::from_secs(20),
            calibrate: false,
            ab: false,
            ab_sample: Some(30),
            eval_sample: None,
            prompt: PromptVersion::V1,
            write_calibration: false,
            apply_calibration: None,
        }
    }
}

/// 失败分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FailureKind {
    /// 连接/读写超时。
    Timeout,
    /// 连接失败/断开。
    Transport,
    /// HTTP 非 200。
    HttpStatus,
    /// 响应体不是 JSON。
    InvalidJson,
    /// 缺少 choices[0].message.content。
    MissingContent,
    /// 模型内容不是 JSON。
    ContentNotJson,
    /// JudgeOutput::validate 失败。
    SchemaViolation,
}

impl FailureKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Transport => "transport",
            Self::HttpStatus => "http_status",
            Self::InvalidJson => "invalid_json",
            Self::MissingContent => "missing_content",
            Self::ContentNotJson => "content_not_json",
            Self::SchemaViolation => "schema_violation",
        }
    }
}

/// 提示词版本（A/B）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptVersion {
    /// 现有提示词（二值）。
    V1,
    /// v2：明确 match/partial/mismatch 判据与逐项要求。
    V2,
}

/// 当前 run/run_calibrated 使用的提示词版本（CLI `--prompt` 的唯一入口）。
#[must_use]
fn selected_prompt(options: &Options) -> PromptVersion {
    options.prompt
}

/// 稳定标签（JSON/日志）。
#[must_use]
fn prompt_version_label(version: PromptVersion) -> &'static str {
    match version {
        PromptVersion::V1 => "v1",
        PromptVersion::V2 => "v2",
    }
}

/// 失败样本。
#[derive(Debug, Clone, serde::Serialize)]
pub struct FailureSample {
    /// 题目 id。
    pub qid: String,
    /// 扰动。
    pub perturbation: String,
    /// 评分项 id。
    pub item_id: String,
    /// 分类。
    pub kind: String,
    /// 脱敏诊断（不含学生文本）。
    pub detail: String,
}

/// 单条解答题的真实判定结果。
#[derive(Debug, Clone)]
struct EntryResult {
    qid: String,
    perturbation: String,
    expected_score: i64,
    max_score: i64,
    actual_score: i64,
    expected_judgement: String,
    actual_judgement: String,
    expected_status: String,
    actual_status: String,
    item_total: usize,
    item_match_ok: usize,
    item_effective_ok: usize,
    confidence: f64,
    calibration_present: bool,
    call_latencies: Vec<u128>,
    prompt_tokens: u64,
    completion_tokens: u64,
    ece_pairs: Vec<(f64, bool)>,
    /// (expected_matched, actual_matched, judgement, confidence) —— A/B 与校准用。
    observations: Vec<(bool, bool, String, f64)>,
    /// 模型输出 partial 的次数。
    partial_calls: usize,
    /// schema 修复重试次数/成功次数/失败次数。
    repair_attempted: u64,
    repair_succeeded: u64,
    repair_failed: u64,
    /// 瞬态超时/连接错误重试次数（成功/失败）。
    transport_retries: u64,
    transport_retry_succeeded: u64,
    transport_retry_failed: u64,
    failures: Vec<FailureSample>,
}

/// 真实模型评估报告。
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// 服务元数据。
    pub service: Value,
    /// 抽样信息。
    pub sample: Value,
    /// 计数。
    pub counts: Value,
    /// 指标。
    pub metrics: Value,
    /// 延迟。
    pub latency: Value,
    /// token。
    pub tokens: Value,
    /// 失败分类与样本。
    pub failures: Value,
    /// 逐条样本（脱敏，最多 80 条）。
    pub samples: Vec<Value>,
    /// 警告。
    pub warnings: Vec<String>,
    /// 耗时。
    pub duration_ms: u128,
    /// gold sha256。
    pub gold_hash: String,
}

impl Report {
    /// JSON 输出。
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "tool": "egctl",
            "command": "eval",
            "mode": "real_judge",
            "service": self.service,
            "sample": self.sample,
            "counts": self.counts,
            "metrics": self.metrics,
            "latency": self.latency,
            "tokens": self.tokens,
            "failures": self.failures,
            "samples": self.samples,
            "warnings": self.warnings,
            "duration_ms": self.duration_ms,
            "gold_sha256": self.gold_hash,
        })
    }

    /// 人读输出。
    #[must_use]
    pub fn render_human(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "egctl eval --real-judge\n服务：{} 模型 {}（temperature=0, max_tokens=256, timeout={}）\n",
            self.service["endpoint"].as_str().unwrap_or("?"),
            self.service["model"].as_str().unwrap_or("?"),
            self.service["timeout_secs"]
        ));
        out.push_str(&format!(
            "抽样：{} / {}（seed={}；分层 perturbation×status）\n",
            self.sample["selected"], self.sample["eligible"], self.sample["seed"]
        ));
        out.push_str(&format!(
            "计数：成功 {} / 失败 {} / 评分项 {}；跳过 manual={}\n",
            self.counts["success"],
            self.counts["failed_entries"],
            self.counts["item_calls"],
            self.counts["skipped_manual"]
        ));
        out.push_str(&format!(
            "S3 真实模型：逐项 matched={:.2}% effective={:.2}%；总分精确={:.2}%（n={}）；半分内={:.2}%；status={:.2}%\n",
            self.metrics["item_match_accuracy"].as_f64().unwrap_or(0.0) * 100.0,
            self.metrics["item_effective_accuracy"].as_f64().unwrap_or(0.0) * 100.0,
            self.metrics["score_exact_accuracy"].as_f64().unwrap_or(0.0) * 100.0,
            self.metrics["scored_entries"],
            self.metrics["within_half_accuracy"].as_f64().unwrap_or(0.0) * 100.0,
            self.metrics["status_consistency"].as_f64().unwrap_or(0.0) * 100.0,
        ));
        out.push_str(&format!(
            "真实 ECE（评分项 confidence vs 判断正确率）：{:.4}（n={}，分桶不足标记：{}）；\
             gold 标签 ECE（fixture 基线）=0.0933\n",
            self.metrics["ece"].as_f64().unwrap_or(0.0),
            self.metrics["ece_items"],
            self.metrics["ece_bins_insufficient"]
        ));
        out.push_str(&format!(
            "延迟 ms：mean={:.1} P50={:.1} P95={:.1}（calls={}，并发={}，含排队）\n",
            self.latency["mean_ms"].as_f64().unwrap_or(0.0),
            self.latency["p50_ms"].as_f64().unwrap_or(0.0),
            self.latency["p95_ms"].as_f64().unwrap_or(0.0),
            self.latency["calls"],
            self.service["concurrency"]
        ));
        out.push_str(&format!(
            "token：prompt={} completion={} 总={}（mean/entry）\n",
            self.tokens["prompt_total"], self.tokens["completion_total"], self.tokens["total"]
        ));
        out.push_str("混淆矩阵（expected↓ / actual→）：\n");
        if let Some(matrix) = self.metrics["confusion_matrix"].as_object() {
            for (expected, row) in matrix {
                if let Some(row) = row.as_object() {
                    out.push_str(&format!(
                        "  {expected:<10} match={:<4} partial={:<4} mismatch={:<4}\n",
                        row.get("match").and_then(Value::as_u64).unwrap_or(0),
                        row.get("partial").and_then(Value::as_u64).unwrap_or(0),
                        row.get("mismatch").and_then(Value::as_u64).unwrap_or(0)
                    ));
                }
            }
        }
        out.push_str(&format!("失败分类：{}\n", self.failures["by_kind"]));
        for warning in &self.warnings {
            out.push_str(&format!("WARN: {warning}\n"));
        }
        out
    }
}

// ---------------------------------------------------------------------------
// gold schema（与 eval.rs 的 solution 结构一致；只读，不改）
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SolutionDataset {
    items: Vec<SolutionItem>,
}

#[derive(Debug, Deserialize)]
struct SolutionItem {
    qid: String,
    kind: QuestionKind,
    max_score_milli: i64,
    answer_key: AnswerKey,
    rubric: Option<Rubric>,
    #[serde(default)]
    student_steps: Vec<StudentStep>,
    #[serde(default)]
    perturbations: Vec<String>,
    expected: Expected,
}

#[derive(Debug, Deserialize)]
struct StudentStep {
    #[serde(default)]
    ordinal: u32,
    text: String,
    #[serde(default)]
    latex: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Expected {
    route: String,
    #[serde(default)]
    judgement: Option<String>,
    #[serde(default)]
    score_milli: Option<i64>,
    status: String,
    #[serde(default)]
    steps: Vec<ExpectedStep>,
}

#[derive(Debug, Deserialize)]
struct ExpectedStep {
    item_id: String,
    matched: bool,
    effective: bool,
}

#[derive(Debug, Deserialize)]
struct GoldIndex {
    #[serde(default)]
    files: BTreeMap<String, String>,
}

/// 执行真实 judge 评估。
///
/// # Errors
///
/// gold 不可读/损坏、抽样后无样本、HTTP 端点非法或全部请求失败时返回错误。
pub fn run(options: &Options) -> Result<Report, String> {
    let started = Instant::now();
    let solution_path = options.gold.join("solution-200.json");
    let bytes = std::fs::read(&solution_path)
        .map_err(|error| format!("读取 {} 失败：{error}", solution_path.display()))?;
    let hash = eg_domain::Sha256Hex::of(&bytes).to_hex();
    if let Ok(text) = std::fs::read_to_string(options.gold.join("index.json"))
        && let Ok(index) = serde_json::from_str::<GoldIndex>(&text)
        && let Some(expected) = index.files.get("solution-200.json")
        && expected != &hash
    {
        return Err(format!(
            "gold solution sha256 不匹配：expected={expected} actual={hash}"
        ));
    }
    let dataset: SolutionDataset = serde_json::from_slice(&bytes)
        .map_err(|error| format!("解析 solution-200.json 失败：{error}"))?;
    let mut rubric_items: Vec<&SolutionItem> = dataset
        .items
        .iter()
        .filter(|item| item.expected.route == "rubric_itemized")
        .collect();
    let skipped_manual = dataset
        .items
        .iter()
        .filter(|item| item.expected.route == "manual")
        .count();
    let eligible = rubric_items.len();
    let requested = options.sample.unwrap_or(60);
    let selected = if requested == 0 {
        rubric_items.clone()
    } else {
        stratified_sample(&mut rubric_items, requested)
    };
    if selected.is_empty() {
        return Err("抽样后没有可评估的 rubric_itemized 条目".to_owned());
    }
    let props = fetch_props(&options.endpoint);
    let client = Arc::new(JudgeClient::new(
        &options.endpoint,
        options.model.clone(),
        options.timeout,
    )?);
    let concurrency = options.concurrency.clamp(1, 4);
    let entries = call_entries(&client, &selected, concurrency, selected_prompt(options));
    let mut confusion: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    let mut prompt_tokens = 0u64;
    let mut completion_tokens = 0u64;
    let mut item_match_ok = 0usize;
    let mut item_effective_ok = 0usize;
    let mut item_total = 0usize;
    let mut score_exact = 0usize;
    let mut half_ok = 0usize;
    let mut status_ok = 0usize;
    let mut scored = 0usize;
    let mut successes = 0usize;
    let mut failures = 0usize;
    let mut ece_pairs = Vec::new();
    let mut call_latencies = Vec::new();
    let mut failure_samples = Vec::new();
    let mut per_kind: BTreeMap<String, usize> = BTreeMap::new();
    let mut samples = Vec::new();
    for entry in &entries {
        if !entry.failures.is_empty() {
            failures += 1;
            for failure in &entry.failures {
                *per_kind.entry(failure.kind.clone()).or_default() += 1;
                failure_samples.push(failure.clone());
            }
            continue;
        }
        successes += 1;
        scored += 1;
        call_latencies.extend(&entry.call_latencies);
        prompt_tokens += entry.prompt_tokens;
        completion_tokens += entry.completion_tokens;
        item_total += entry.item_total;
        item_match_ok += entry.item_match_ok;
        item_effective_ok += entry.item_effective_ok;
        if entry.actual_score == entry.expected_score {
            score_exact += 1;
        }
        if entry.actual_score.abs_diff(entry.expected_score)
            <= u64::try_from(entry.max_score.max(1) / 2).unwrap_or(0)
        {
            half_ok += 1;
        }
        if entry.actual_status == entry.expected_status {
            status_ok += 1;
        }
        confusion
            .entry(entry.expected_judgement.clone())
            .or_default()
            .entry(entry.actual_judgement.clone())
            .and_modify(|count| *count += 1)
            .or_insert(1);
        // ECE 用每个评分项的实际 confidence 与「该项判断是否正确」。
        ece_pairs.extend(&entry.ece_pairs);
        if samples.len() < 80 {
            samples.push(json!({
                "qid": entry.qid,
                "perturbation": entry.perturbation,
                "expected_score": entry.expected_score,
                "actual_score": entry.actual_score,
                "expected_judgement": entry.expected_judgement,
                "actual_judgement": entry.actual_judgement,
                "expected_status": entry.expected_status,
                "actual_status": entry.actual_status,
                "confidence": entry.confidence,
                "calibration_present": entry.calibration_present,
                "item_match": format!("{}/{}", entry.item_match_ok, entry.item_total),
                "item_effective": format!("{}/{}", entry.item_effective_ok, entry.item_total),
            }));
        }
    }
    let (ece, insufficient_bins) = expected_calibration_error(&ece_pairs);
    let winner_ci = wilson_interval(item_match_ok, item_total);
    let mut report = Report {
        service: json!({
            "endpoint": options.endpoint,
            "model": options.model,
            "temperature": 0,
            "max_tokens": 256,
            "stream": false,
            "response_format": "json_object",
            "timeout_secs": options.timeout.as_secs(),
            "concurrency": concurrency,
            "quantization": "Q4_K_M",
            "engine": "llama.cpp / llama-server",
            "build_info": props
                .as_ref()
                .and_then(|value| value.get("build_info"))
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            "server_slots": props
                .as_ref()
                .and_then(|value| value.get("total_slots"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
        }),
        sample: json!({
            "requested": requested,
            "selected": selected.len(),
            "eligible": eligible,
            "skipped_manual": skipped_manual,
            "seed": SEED,
            "strata": "perturbation×expected.status",
        }),
        counts: json!({
            "success": successes,
            "failed_entries": failures,
            "entries": entries.len(),
            "item_calls": entries.iter().map(|entry| entry.item_total).sum::<usize>(),
            "scored_entries": scored,
            "skipped_manual": skipped_manual,
        }),
        metrics: json!({
            "item_match_accuracy": ratio(item_match_ok, item_total),
            "item_effective_accuracy": ratio(item_effective_ok, item_total),
            "item_total": item_total,
            "item_match_ok": item_match_ok,
            "item_effective_ok": item_effective_ok,
            "accuracy_ci95_wilson": [winner_ci.0, winner_ci.1],
            "score_exact_accuracy": ratio(score_exact, scored),
            "within_half_accuracy": ratio(half_ok, scored),
            "status_consistency": ratio(status_ok, scored),
            "scored_entries": scored,
            "ece": ece,
            "ece_items": ece_pairs.len(),
            "ece_bins_insufficient": insufficient_bins,
            "confusion_matrix": confusion,
            "calibration_present_entries": entries.iter().filter(|entry| entry.failures.is_empty() && entry.calibration_present).count(),
        }),
        latency: latency_stats(&call_latencies),
        tokens: json!({
            "prompt_total": prompt_tokens,
            "completion_total": completion_tokens,
            "total": prompt_tokens + completion_tokens,
            "prompt_mean": if successes == 0 { 0.0 } else { prompt_tokens as f64 / successes as f64 },
            "completion_mean": if successes == 0 { 0.0 } else { completion_tokens as f64 / successes as f64 },
        }),
        failures: json!({
            "by_kind": per_kind,
            "samples": failure_samples.iter().take(40).collect::<Vec<_>>(),
            "failed_entries": failures,
        }),
        samples,
        warnings: Vec::new(),
        duration_ms: 0,
        gold_hash: hash,
    };
    if ece_pairs.len() < 30 {
        report.warnings.push(format!(
            "真实 ECE 样本仅 {} 条评分项（<30）：分桶不足，ECE 不可判定，仅作诊断",
            ece_pairs.len()
        ));
    }
    if report.metrics["calibration_present_entries"]
        .as_u64()
        .unwrap_or(0)
        == 0
    {
        report.warnings.push(
            "真实模型未提供 calibration_id：按 grade 门禁全部 needs_review（uncalibrated_model）；status 一致率低属预期，真实校准需另做校准集"
                .to_owned(),
        );
    }
    report.warnings.push(
        "gold expected 由规范规则推导（合成标签），非模型标注；真实模型指标低于 fixture 基线（100%）为正常，禁止据此调参/改 gold"
            .to_owned(),
    );
    if concurrency > 1 {
        report.warnings.push(format!(
            "并发={concurrency}：延迟含排队/竞争（服务 4 slots），与串行单题延迟不可直接比较"
        ));
    }
    if failures > 0 {
        report.warnings.push(format!(
            "失败/超时条目 {failures} 条已从准确率分母排除，分类见 failures.by_kind"
        ));
    }
    report.duration_ms = started.elapsed().as_millis();
    Ok(report)
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn stratified_sample<'a>(items: &mut Vec<&'a SolutionItem>, count: usize) -> Vec<&'a SolutionItem> {
    let mut buckets: BTreeMap<String, Vec<&SolutionItem>> = BTreeMap::new();
    for item in items.drain(..) {
        let perturbation = item
            .perturbations
            .first()
            .cloned()
            .unwrap_or_else(|| "none".to_owned());
        let key = format!("{}|{}", perturbation, item.expected.status);
        buckets.entry(key).or_default().push(item);
    }
    for bucket in buckets.values_mut() {
        bucket.sort_by_key(|item| stable_hash(&format!("{SEED}:{}", item.qid)));
    }
    let keys: Vec<String> = buckets.keys().cloned().collect();
    let mut out = Vec::with_capacity(count.min(keys.iter().map(|key| buckets[key].len()).sum()));
    let mut indices = BTreeMap::new();
    while out.len() < count {
        let mut picked = false;
        for key in &keys {
            let Some(bucket) = buckets.get(key) else {
                continue;
            };
            let index = indices.entry(key.clone()).or_insert(0usize);
            if let Some(item) = bucket.get(*index) {
                out.push(*item);
                *index += 1;
                picked = true;
                if out.len() >= count {
                    break;
                }
            }
        }
        if !picked {
            break;
        }
    }
    out
}

fn stable_hash(text: &str) -> u64 {
    let hash = eg_domain::Sha256Hex::of(text.as_bytes()).to_hex();
    u64::from_str_radix(&hash[..16], 16).unwrap_or(0)
}

fn category(item: &SolutionItem) -> String {
    item.perturbations.first().cloned().unwrap_or_default()
}

fn call_entries(
    client: &Arc<JudgeClient>,
    items: &[&SolutionItem],
    concurrency: usize,
    version: PromptVersion,
) -> Vec<EntryResult> {
    if concurrency <= 1 {
        return items
            .iter()
            .map(|item| judge_item_entry(client, item, version))
            .collect();
    }
    let total = items.len();
    let chunk = total.div_ceil(concurrency);
    let mut results = Vec::with_capacity(total);
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for part in items.chunks(chunk) {
            let client = Arc::clone(client);
            handles.push(scope.spawn(move || {
                part.iter()
                    .map(|item| judge_item_entry(&client, item, version))
                    .collect::<Vec<_>>()
            }));
        }
        for handle in handles {
            results.extend(handle.join().unwrap_or_default());
        }
    });
    results
}

fn judge_item_entry(
    client: &JudgeClient,
    item: &SolutionItem,
    version: PromptVersion,
) -> EntryResult {
    let Some(rubric) = item.rubric.as_ref() else {
        return EntryResult {
            qid: item.qid.clone(),
            perturbation: category(item),
            expected_score: item.expected.score_milli.unwrap_or(0),
            max_score: item.max_score_milli,
            actual_score: 0,
            expected_judgement: item.expected.judgement.clone().unwrap_or_default(),
            actual_judgement: "ungradable".to_owned(),
            expected_status: item.expected.status.clone(),
            actual_status: "needs_review".to_owned(),
            item_total: 0,
            item_match_ok: 0,
            item_effective_ok: 0,
            confidence: 0.0,
            calibration_present: false,
            call_latencies: Vec::new(),
            prompt_tokens: 0,
            completion_tokens: 0,
            ece_pairs: Vec::new(),
            observations: Vec::new(),
            partial_calls: 0,
            repair_attempted: 0,
            repair_succeeded: 0,
            repair_failed: 0,
            transport_retries: 0,
            transport_retry_succeeded: 0,
            transport_retry_failed: 0,
            failures: vec![FailureSample {
                qid: item.qid.clone(),
                perturbation: category(item),
                item_id: String::new(),
                kind: "data_missing".to_owned(),
                detail: "rubric_itemized 条目缺 rubric".to_owned(),
            }],
        };
    };
    let mut satisfied: Vec<RubricItemId> = Vec::new();
    let mut failures = Vec::new();
    let mut call_latencies = Vec::new();
    let mut prompt_tokens = 0u64;
    let mut completion_tokens = 0u64;
    let mut confidences = Vec::new();
    let mut ece_pairs = Vec::new();
    let mut observations = Vec::new();
    let mut partial_calls = 0usize;
    let mut repair_attempted = 0u64;
    let mut repair_succeeded = 0u64;
    let mut repair_failed = 0u64;
    let mut transport_retries = 0u64;
    let mut transport_retry_succeeded = 0u64;
    let mut transport_retry_failed = 0u64;
    let mut calibration_present = false;
    let mut item_total = 0usize;
    let mut item_match_ok = 0usize;
    for rubric_item in &rubric.items {
        item_total += 1;
        let expected_item = item
            .expected
            .steps
            .iter()
            .find(|step| step.item_id == rubric_item.id.to_string());
        let prompt = item_prompt(
            item,
            rubric_item.criterion.as_str(),
            rubric_item.points.as_milli(),
            version,
        );
        match client.judge(&prompt, rubric_item.points) {
            Ok(response) => {
                call_latencies.push(response.latency_ms);
                prompt_tokens += response.prompt_tokens;
                completion_tokens += response.completion_tokens;
                confidences.push(response.confidence);
                calibration_present |= response.calibration_present;
                repair_attempted += u64::from(response.repair_attempted);
                repair_succeeded += u64::from(response.repair_succeeded);
                transport_retries += u64::from(response.transport_retried);
                transport_retry_succeeded +=
                    u64::from(response.transport_retried && response.transport_succeeded);
                let matched = response.judgement == "match";
                if response.judgement == "partial" {
                    partial_calls += 1;
                }
                if matched {
                    satisfied.push(rubric_item.id);
                }
                let expected_matched = expected_item.is_some_and(|step| step.matched);
                if expected_matched == matched {
                    item_match_ok += 1;
                }
                ece_pairs.push((response.confidence, expected_matched == matched));
                observations.push((
                    expected_matched,
                    matched,
                    response.judgement.clone(),
                    response.confidence,
                ));
            }
            Err(error) => {
                if error.detail.contains("修复重试仍失败") {
                    repair_attempted += 1;
                    repair_failed += 1;
                }
                if error.detail.contains("瞬态错误重试 1 次仍失败") {
                    transport_retries += 1;
                    transport_retry_failed += 1;
                }
                failures.push(FailureSample {
                    qid: item.qid.clone(),
                    perturbation: category(item),
                    item_id: rubric_item.id.to_string(),
                    kind: error.kind.as_str().to_owned(),
                    detail: error.detail,
                });
            }
        }
    }
    let (actual_score, actual_judgement, item_effective_ok) = match aggregate(rubric, &satisfied) {
        Ok(aggregated) => {
            let actual_effective: std::collections::BTreeSet<String> = aggregated
                .satisfied
                .iter()
                .map(ToString::to_string)
                .collect();
            // 逐评分项比较 effective（不是整条 all-or-nothing）。
            let effective_ok = item
                .expected
                .steps
                .iter()
                .filter(|step| actual_effective.contains(&step.item_id) == step.effective)
                .count();
            let score = aggregated.score.as_milli();
            let judgement = if score >= item.max_score_milli {
                "match"
            } else if score <= 0 {
                "mismatch"
            } else {
                "partial"
            };
            (score, judgement.to_owned(), effective_ok)
        }
        Err(_) => (0, "ungradable".to_owned(), 0),
    };
    let confidence = if confidences.is_empty() {
        0.0
    } else {
        confidences.iter().sum::<f64>() / confidences.len() as f64
    };
    let (actual_status, _reason) = if calibration_present && confidence >= 0.80 {
        ("auto_accept", None)
    } else {
        ("needs_review", Some("uncalibrated_model"))
    };
    EntryResult {
        qid: item.qid.clone(),
        perturbation: category(item),
        expected_score: item.expected.score_milli.unwrap_or(0),
        max_score: item.max_score_milli,
        actual_score,
        expected_judgement: item.expected.judgement.clone().unwrap_or_default(),
        actual_judgement,
        expected_status: item.expected.status.clone(),
        actual_status: actual_status.to_owned(),
        item_total,
        item_match_ok,
        item_effective_ok,
        confidence,
        calibration_present,
        call_latencies,
        prompt_tokens,
        completion_tokens,
        ece_pairs,
        observations,
        partial_calls,
        repair_attempted,
        repair_succeeded,
        repair_failed,
        transport_retries,
        transport_retry_succeeded,
        transport_retry_failed,
        failures,
    }
}

fn item_prompt(
    item: &SolutionItem,
    criterion: &str,
    points_milli: i64,
    version: PromptVersion,
) -> String {
    match version {
        PromptVersion::V1 => item_prompt_v1(item, criterion, points_milli),
        PromptVersion::V2 => item_prompt_v2(item, criterion, points_milli),
    }
}

fn item_prompt_v1(item: &SolutionItem, criterion: &str, points_milli: i64) -> String {
    let steps: Vec<Value> = item
        .student_steps
        .iter()
        .map(|step| json!({"ordinal": step.ordinal, "text": step.text, "latex": step.latex}))
        .collect();
    json!({
        "task": "判断学生步骤是否满足指定评分项",
        "question": {
            "qid": item.qid,
            "kind": format!("{:?}", item.kind).to_lowercase(),
            "max_score_milli": item.max_score_milli,
            "answer_key": item.answer_key,
        },
        "rubric_item": {"criterion": criterion, "points_milli": points_milli},
        "student_steps": steps,
        "output_contract": {
            "judgement": "match 或 mismatch（只判断本评分项）",
            "score_milli": format!("match={points_milli}；mismatch=0"),
            "reason": "中文理由，≤120 字",
            "confidence": "0–1，表示对本次判断的把握"
        }
    })
    .to_string()
}

fn item_prompt_v2(item: &SolutionItem, criterion: &str, points_milli: i64) -> String {
    let steps: Vec<Value> = item
        .student_steps
        .iter()
        .map(|step| json!({"ordinal": step.ordinal, "text": step.text, "latex": step.latex}))
        .collect();
    let partial_score = ((points_milli / 2) / 1000 * 1000)
        .max(1000)
        .min(points_milli - 1000);
    json!({
        "task": "判断学生步骤是否满足指定评分项（逐项独立判定）",
        "question": {
            "qid": item.qid,
            "kind": format!("{:?}", item.kind).to_lowercase(),
            "max_score_milli": item.max_score_milli,
            "answer_key": item.answer_key,
        },
        "rubric_item": {"criterion": criterion, "points_milli": points_milli},
        "student_steps": steps,
        "judgement_criteria": {
            "match": "学生步骤包含该评分项的关键要素或等价表达（允许简化、不同书写/公式排版、无明确步骤标题）；且没有相反/错误内容",
            "partial": "部分覆盖但不足以完全满足（缺关键要素或过程不完整）。此时 score_milli 取 (0,满分) 之间的值（建议 partial_score_milli），satisfied 写 [criterion]，missing 写缺失要素",
            "mismatch": "完全缺失，或关键结果/推理错误，或与 criterion 相反",
            "important": "只有关键结果错误才 mismatch；书写简略、公式步骤被合并但关键要素存在时应 match 或 partial，不要直接 mismatch"
        },
        "partial_score_milli": partial_score,
        "example_partial": {
            "judgement": "partial",
            "score_milli": partial_score,
            "reason": "部分满足：缺少关键步骤",
            "confidence": 0.7,
            "satisfied": [criterion],
            "missing": ["缺失的关键要素"]
        },
        "output_contract": {
            "judgement": "match | partial | mismatch",
            "score_milli": format!("match={points_milli}；partial=(0,{points_milli})（建议 {partial_score}）；mismatch=0"),
            "reason": "中文理由，≤120 字",
            "confidence": "0–1，表示对本次判断的把握",
            "satisfied": "仅 partial 时非空（如 [criterion]）",
            "missing": "仅 partial 时非空（缺失要素）"
        }
    })
    .to_string()
}

fn latency_stats(values: &[u128]) -> Value {
    if values.is_empty() {
        return json!({"calls": 0, "mean_ms": 0.0, "p50_ms": 0.0, "p95_ms": 0.0});
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let mean = sorted.iter().sum::<u128>() as f64 / sorted.len() as f64;
    let p = |percent: f64| -> f64 {
        let index = ((sorted.len() as f64 - 1.0) * percent).round() as usize;
        sorted[index.min(sorted.len() - 1)] as f64
    };
    json!({
        "calls": values.len(),
        "mean_ms": mean,
        "p50_ms": p(0.50),
        "p95_ms": p(0.95),
    })
}

fn expected_calibration_error(pairs: &[(f64, bool)]) -> (f64, u64) {
    if pairs.is_empty() {
        return (0.0, 0);
    }
    let mut bins = [(0u64, 0u64, 0.0f64); 10];
    for (confidence, correct) in pairs {
        let index = ((confidence.clamp(0.0, 0.999_999) * 10.0) as usize).min(9);
        bins[index].0 += 1;
        if *correct {
            bins[index].1 += 1;
        }
        bins[index].2 += confidence;
    }
    let total = pairs.len() as f64;
    let mut insufficient = 0u64;
    let ece = bins
        .iter()
        .filter(|(count, _, _)| *count > 0)
        .map(|(count, correct, sum)| {
            if *count < 5 {
                insufficient += 1;
            }
            let n = *count as f64;
            let accuracy = *correct as f64 / n;
            let avg = *sum / n;
            n / total * (accuracy - avg).abs()
        })
        .sum();
    (ece, insufficient)
}

fn wilson_interval(successes: usize, total: usize) -> (f64, f64) {
    if total == 0 {
        return (0.0, 1.0);
    }
    let n = total as f64;
    let p = successes as f64 / n;
    let z = 1.96;
    let denominator = 1.0 + z * z / n;
    let center = (p + z * z / (2.0 * n)) / denominator;
    let margin = z * ((p * (1.0 - p) / n + z * z / (4.0 * n * n)).sqrt()) / denominator;
    ((center - margin).max(0.0), (center + margin).min(1.0))
}

// ---------------------------------------------------------------------------
// 校准 + 提示词 A/B（task-72）
// ---------------------------------------------------------------------------

/// 校准 ID（写入报告/eval 输出；JudgeOutcome 结构暂无该字段，见限制）。
pub const CALIBRATION_ID: &str = "qwen35-4b-q4-iso-v1";

/// 等频分桶 + PAV 单调校准表。
#[derive(Debug, Clone)]
pub struct Calibrator {
    bins: Vec<CalBin>,
    fallback: f64,
    id: &'static str,
}

#[derive(Debug, Clone)]
struct CalBin {
    lower: f64,
    upper: f64,
    count: u64,
    accuracy: f64,
}

impl Calibrator {
    /// 空校准表（恒等映射，ECE/Brier 用原始 confidence）。
    #[must_use]
    pub fn identity() -> Self {
        Self {
            bins: Vec::new(),
            fallback: 0.5,
            id: CALIBRATION_ID,
        }
    }

    /// 等频分桶（`bin_count` 个近似等量桶）后用 PAV 合并违反单调的相邻桶。
    #[must_use]
    pub fn fit(pairs: &[(f64, bool)], bin_count: usize) -> Self {
        if pairs.is_empty() {
            return Self::identity();
        }
        let bins = if bin_count <= 1 {
            1
        } else {
            bin_count.min(pairs.len())
        };
        let mut sorted = pairs.to_vec();
        sorted.sort_by(|left, right| {
            left.0
                .partial_cmp(&right.0)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        // 先按 confidence 去重成组（同值不拆桶，否则相同 confidence 会被分到不同
        // 训练桶，映射结果依赖桶顺序）；再在组粒度上合并到 ≤bin_count 个近等频桶。
        let mut groups: Vec<CalBin> = Vec::new();
        for (confidence, correct) in &sorted {
            if let Some(last) = groups.last_mut()
                && (last.upper - *confidence).abs() <= f64::EPSILON
            {
                last.count += 1;
                let correct = u64::from(*correct);
                last.accuracy =
                    (last.accuracy * (last.count - 1) as f64 + correct as f64) / last.count as f64;
                continue;
            }
            groups.push(CalBin {
                lower: *confidence,
                upper: *confidence,
                count: 1,
                accuracy: f64::from(u32::from(*correct)),
            });
        }
        let total = sorted.len();
        let target = total.div_ceil(bins);
        let mut raw: Vec<CalBin> = Vec::with_capacity(bins);
        let mut current: Option<CalBin> = None;
        for group in groups {
            match current.take() {
                None => current = Some(group),
                Some(mut bin) => {
                    let can_start_new = raw.len() + 1 < bins
                        && bin.count >= u64::try_from(target).unwrap_or(u64::MAX);
                    if can_start_new {
                        raw.push(bin);
                        current = Some(group);
                    } else {
                        let weight = bin.count + group.count;
                        bin.accuracy = (bin.accuracy * bin.count as f64
                            + group.accuracy * group.count as f64)
                            / weight as f64;
                        bin.count = weight;
                        bin.upper = group.upper;
                        current = Some(bin);
                    }
                }
            }
        }
        if let Some(bin) = current {
            raw.push(bin);
        }
        // PAV：相邻桶 accuracy 必须非降；违反则按样本加权合并。
        let mut merged: Vec<CalBin> = Vec::with_capacity(raw.len());
        for bin in raw {
            merged.push(bin);
            while merged.len() >= 2 {
                let length = merged.len();
                let left = &merged[length - 2];
                let right = &merged[length - 1];
                if left.accuracy <= right.accuracy + f64::EPSILON {
                    break;
                }
                let left_weight = left.count as f64;
                let right_weight = right.count as f64;
                let count = left.count + right.count;
                let accuracy =
                    (left.accuracy * left_weight + right.accuracy * right_weight) / count as f64;
                let combined = CalBin {
                    lower: left.lower.min(right.lower),
                    upper: left.upper.max(right.upper),
                    count,
                    accuracy,
                };
                merged.truncate(length - 2);
                merged.push(combined);
            }
        }
        let fallback =
            pairs.iter().filter(|(_, correct)| *correct).count() as f64 / pairs.len() as f64;
        Self {
            bins: merged,
            fallback,
            id: CALIBRATION_ID,
        }
    }

    /// 将原始 confidence 映射为校准概率（单调非降；查不到桶时取最近区间）。
    #[must_use]
    pub fn calibrate(&self, confidence: f64) -> f64 {
        let confidence = confidence.clamp(0.0, 1.0);
        if self.bins.is_empty() {
            return self.fallback;
        }
        for bin in &self.bins {
            if confidence >= bin.lower && confidence <= bin.upper {
                return bin.accuracy;
            }
        }
        // 落在桶间空隙/边界之外：取区间距离最近者。
        self.bins
            .iter()
            .min_by(|left, right| {
                distance(confidence, left)
                    .partial_cmp(&distance(confidence, right))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map_or(self.fallback, |bin| bin.accuracy)
    }

    /// 校准 ID。
    #[must_use]
    pub const fn id(&self) -> &'static str {
        self.id
    }

    /// 校准后可靠性表（按校准后概率分桶：n/平均校准值/准确率）。
    #[must_use]
    pub fn reliability_table_after(&self, pairs: &[(f64, bool)]) -> Value {
        let mapped: Vec<(f64, bool)> = pairs
            .iter()
            .map(|(confidence, correct)| (self.calibrate(*confidence), *correct))
            .collect();
        let mut bins = vec![(0u64, 0.0f64, 0u64); 10];
        for (confidence, correct) in &mapped {
            let index = ((confidence.clamp(0.0, 0.999_999) * 10.0) as usize).min(9);
            bins[index].0 += 1;
            bins[index].1 += confidence;
            if *correct {
                bins[index].2 += 1;
            }
        }
        bins.iter()
            .enumerate()
            .filter(|(_, (count, _, _))| *count > 0)
            .map(|(index, (count, sum, correct))| {
                let n = *count as f64;
                json!({
                    "range": [index as f64 / 10.0, (index + 1) as f64 / 10.0],
                    "n": count,
                    "avg_calibrated": sum / n,
                    "accuracy": f64::from(u32::try_from(*correct).unwrap_or(0)) / n,
                })
            })
            .collect::<Vec<_>>()
            .into()
    }

    /// 校准前可靠性表（按原始 confidence 分桶：n/平均原始 confidence/准确率/平均校准值）。
    #[must_use]
    pub fn reliability_table(&self, pairs: &[(f64, bool)]) -> Value {
        let mut bins = vec![(0u64, 0.0f64, 0u64, 0.0f64); 10];
        for (confidence, correct) in pairs {
            let index = ((confidence.clamp(0.0, 0.999_999) * 10.0) as usize).min(9);
            let bin = &mut bins[index];
            bin.0 += 1;
            bin.1 += confidence;
            if *correct {
                bin.2 += 1;
            }
            bin.3 += self.calibrate(*confidence);
        }
        bins.iter()
            .enumerate()
            .filter(|(_, (count, _, _, _))| *count > 0)
            .map(|(index, (count, sum, correct, calibrated_sum))| {
                let n = *count as f64;
                json!({
                    "range": [index as f64 / 10.0, (index + 1) as f64 / 10.0],
                    "n": count,
                    "avg_raw_confidence": sum / n,
                    "accuracy": f64::from(u32::try_from(*correct).unwrap_or(0)) / n,
                    "avg_calibrated": calibrated_sum / n,
                })
            })
            .collect::<Vec<_>>()
            .into()
    }
}

fn distance(confidence: f64, bin: &CalBin) -> f64 {
    if confidence < bin.lower {
        bin.lower - confidence
    } else if confidence > bin.upper {
        confidence - bin.upper
    } else {
        0.0
    }
}

/// 校准/AB 报告。
#[derive(Debug, Clone)]
pub struct CalibratedReport {
    /// 服务/提示词信息。
    pub service: Value,
    /// 数据拆分。
    pub split: Value,
    /// 校准表。
    pub calibration: Value,
    /// 仅 eval 集的校准前后指标。
    pub eval: Value,
    /// A/B 结果（`None` = 未启用）。
    pub ab: Option<Value>,
    /// 失败分类。
    pub failures: Value,
    /// 机械修复重试统计。
    pub repairs: Value,
    /// 瞬态超时/连接重试统计。
    pub retries: Value,
    /// production 校准表门禁模拟（raw vs calibrated status）。
    pub calibration_status: Value,
    /// 耗时/token。
    pub duration_ms: u128,
    /// token 用量。
    pub tokens: Value,
    /// 警告。
    pub warnings: Vec<String>,
    /// gold hash。
    pub gold_hash: String,
    /// 逐条 eval 样本（脱敏，最多 40）。
    pub samples: Vec<Value>,
}

impl CalibratedReport {
    /// JSON 输出。
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "tool": "egctl",
            "command": "eval",
            "mode": "real_judge_calibrated",
            "calibration_id": CALIBRATION_ID,
            "service": self.service,
            "split": self.split,
            "calibration": self.calibration,
            "eval": self.eval,
            "ab": self.ab,
            "failures": self.failures,
            "repairs": self.repairs,
            "retries": self.retries,
            "calibration_status": self.calibration_status,
            "duration_ms": self.duration_ms,
            "tokens": self.tokens,
            "warnings": self.warnings,
            "gold_sha256": self.gold_hash,
            "samples": self.samples,
        })
    }

    /// 人读输出。
    #[must_use]
    pub fn render_human(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "egctl eval --real-judge --calibrate（calibration_id={CALIBRATION_ID}）\n\
             拆分：{}；服务：{} {}\n",
            self.split, self.service["endpoint"], self.service["model"]
        ));
        if let Some(eval) = self.eval.as_object() {
            out.push_str(&format!(
                "eval n={}：raw ECE={:.4} Brier={:.4}；校准后 ECE={:.4} Brier={:.4}\n",
                eval.get("n").and_then(Value::as_u64).unwrap_or(0),
                self.eval
                    .pointer("/raw/ece")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0),
                self.eval
                    .pointer("/raw/brier")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0),
                self.eval
                    .pointer("/calibrated/ece")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0),
                self.eval
                    .pointer("/calibrated/brier")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0),
            ));
            out.push_str(&format!(
                "  逐项 strict={:.2}% lenient={:.2}%；总分精确={:.2}%；半分内={:.2}%；status raw/cal={:.2}%/{:.2}%\n",
                self.eval.pointer("/raw/item_match_accuracy").and_then(Value::as_f64).unwrap_or(0.0) * 100.0,
                self.eval.pointer("/raw/item_lenient_accuracy").and_then(Value::as_f64).unwrap_or(0.0) * 100.0,
                self.eval.pointer("/raw/score_exact_accuracy").and_then(Value::as_f64).unwrap_or(0.0) * 100.0,
                self.eval.pointer("/raw/within_half_accuracy").and_then(Value::as_f64).unwrap_or(0.0) * 100.0,
                self.eval.pointer("/raw/status_consistency").and_then(Value::as_f64).unwrap_or(0.0) * 100.0,
                self.eval.pointer("/calibrated/status_consistency").and_then(Value::as_f64).unwrap_or(0.0) * 100.0,
            ));
        }
        if let Some(ab) = self.ab.as_ref().and_then(Value::as_object) {
            for version in ["v1", "v2"] {
                if let Some(metrics) = ab.get(version).and_then(Value::as_object) {
                    out.push_str(&format!(
                        "A/B {version}: 逐项 strict={:.2}% lenient={:.2}% partial召回={:.2}% mismatch→match={} 总分精确={:.2}%\n",
                        metrics.get("item_match_accuracy").and_then(Value::as_f64).unwrap_or(0.0)*100.0,
                        metrics.get("item_lenient_accuracy").and_then(Value::as_f64).unwrap_or(0.0)*100.0,
                        metrics.get("partial_recall").and_then(Value::as_f64).unwrap_or(0.0)*100.0,
                        metrics.get("mismatch_to_match").and_then(Value::as_u64).unwrap_or(0),
                        metrics.get("score_exact_accuracy").and_then(Value::as_f64).unwrap_or(0.0)*100.0,
                    ));
                }
            }
        }
        out.push_str(&format!(
            "耗时 {} ms；token {}；失败 {}\n",
            self.duration_ms, self.tokens, self.failures
        ));
        for warning in &self.warnings {
            out.push_str(&format!("WARN: {warning}\n"));
        }
        out
    }
}

/// `--calibrate` 模式：拆 calibration/eval（固定 seed 分层）+ 可选 A/B，严格无泄漏。
pub fn run_calibrated(options: &Options) -> Result<CalibratedReport, String> {
    let started = Instant::now();
    let solution_path = options.gold.join("solution-200.json");
    let bytes = std::fs::read(&solution_path)
        .map_err(|error| format!("读取 {} 失败：{error}", solution_path.display()))?;
    let hash = eg_domain::Sha256Hex::of(&bytes).to_hex();
    let dataset: SolutionDataset = serde_json::from_slice(&bytes)
        .map_err(|error| format!("解析 solution-200.json 失败：{error}"))?;
    let mut eligible: Vec<&SolutionItem> = dataset
        .items
        .iter()
        .filter(|item| item.expected.route == "rubric_itemized")
        .collect();
    let skipped_manual = dataset
        .items
        .iter()
        .filter(|item| item.expected.route == "manual")
        .count();
    let sample = options.sample.unwrap_or(60);
    let eval_sample = options.eval_sample.unwrap_or(sample);
    if sample == 0 || eval_sample == 0 {
        return Err("--calibrate 需要固定样本（--sample/--eval-sample >0），不支持全量".to_owned());
    }
    let ab_n = if options.ab {
        options.ab_sample.unwrap_or(30)
    } else {
        0
    };
    let total = sample + eval_sample + ab_n;
    if total > eligible.len() {
        return Err(format!(
            "拆分需要 {total} 条（calibration {sample}+eval {eval_sample}+ab {ab_n}），仅 {} 条可用",
            eligible.len()
        ));
    }
    let ordered = stratified_sample(&mut eligible, total);
    let calibration_items: Vec<&SolutionItem> = ordered[..sample].to_vec();
    let eval_items: Vec<&SolutionItem> = ordered[sample..sample + eval_sample].to_vec();
    let ab_items: Vec<&SolutionItem> = ordered[sample + eval_sample..].to_vec();
    let props = fetch_props(&options.endpoint);
    let client = Arc::new(JudgeClient::new(
        &options.endpoint,
        options.model.clone(),
        options.timeout,
    )?);
    let concurrency = options.concurrency.clamp(1, 4);
    let calibration_entries = call_entries(
        &client,
        &calibration_items,
        concurrency,
        selected_prompt(options),
    );
    let eval_entries = call_entries(&client, &eval_items, concurrency, selected_prompt(options));
    let calibration_pairs = item_pairs(&calibration_entries);
    let calibrator = Calibrator::fit(&calibration_pairs, 10);
    let eval_raw = eval_metrics(&eval_entries, None);
    let eval_calibrated = eval_metrics(&eval_entries, Some(&calibrator));
    let (ab, ab_failures) = if options.ab {
        let v1_all = call_entries(&client, &ab_items, concurrency, PromptVersion::V1);
        let v2_all = call_entries(&client, &ab_items, concurrency, PromptVersion::V2);
        // 配对：只比较两个提示词都成功（无 item 失败）的同一批条目，避免分母不同。
        let paired_qids: BTreeSet<String> = v1_all
            .iter()
            .filter(|entry| entry.failures.is_empty())
            .map(|entry| entry.qid.clone())
            .collect();
        let paired_qids: BTreeSet<String> = paired_qids
            .into_iter()
            .filter(|qid| {
                v2_all
                    .iter()
                    .any(|entry| &entry.qid == qid && entry.failures.is_empty())
            })
            .collect();
        let v1: Vec<EntryResult> = v1_all
            .iter()
            .filter(|entry| paired_qids.contains(&entry.qid))
            .cloned()
            .collect();
        let v2: Vec<EntryResult> = v2_all
            .iter()
            .filter(|entry| paired_qids.contains(&entry.qid))
            .cloned()
            .collect();
        let v1_metrics = eval_metrics(&v1, None);
        let v2_metrics = eval_metrics(&v2, None);
        let ab_failures = json!({
            "v1": collect_entries_failures(&v1_all),
            "v2": collect_entries_failures(&v2_all),
            "paired_entries": paired_qids.len(),
            "v1_raw_entries": v1_all.len(),
            "v2_raw_entries": v2_all.len(),
        });
        let ab = Some(json!({
            "n": ab_items.len(),
            "paired_entries": paired_qids.len(),
            "v1": v1_metrics,
            "v2": v2_metrics,
            "delta": {
                "item_match_accuracy": v2_metrics["item_match_accuracy"].as_f64().unwrap_or(0.0) - v1_metrics["item_match_accuracy"].as_f64().unwrap_or(0.0),
                "item_lenient_accuracy": v2_metrics["item_lenient_accuracy"].as_f64().unwrap_or(0.0) - v1_metrics["item_lenient_accuracy"].as_f64().unwrap_or(0.0),
                "partial_recall": v2_metrics["partial_recall"].as_f64().unwrap_or(0.0) - v1_metrics["partial_recall"].as_f64().unwrap_or(0.0),
                "score_exact_accuracy": v2_metrics["score_exact_accuracy"].as_f64().unwrap_or(0.0) - v1_metrics["score_exact_accuracy"].as_f64().unwrap_or(0.0),
                "ece": v2_metrics["ece"].as_f64().unwrap_or(0.0) - v1_metrics["ece"].as_f64().unwrap_or(0.0),
            },
            "v2_partial_calls": v2_metrics["partial_calls"],
            "v1_partial_calls": v1_metrics["partial_calls"],
            "failures": ab_failures,
        }));
        (ab, ab_failures)
    } else {
        (None, Value::Null)
    };
    let repairs = {
        let attempted: u64 = calibration_entries
            .iter()
            .chain(&eval_entries)
            .map(|entry| entry.repair_attempted)
            .sum();
        let succeeded: u64 = calibration_entries
            .iter()
            .chain(&eval_entries)
            .map(|entry| entry.repair_succeeded)
            .sum();
        let failed: u64 = calibration_entries
            .iter()
            .chain(&eval_entries)
            .map(|entry| entry.repair_failed)
            .sum();
        json!({
            "attempted": attempted,
            "succeeded": succeeded,
            "failed": failed,
            "success_rate": if attempted == 0 { 1.0 } else { succeeded as f64 / attempted as f64 },
        })
    };
    let calibration_status = production_calibration_status(&eval_entries, options);
    let retries = {
        let attempted: u64 = calibration_entries
            .iter()
            .chain(&eval_entries)
            .map(|entry| entry.transport_retries)
            .sum();
        let succeeded: u64 = calibration_entries
            .iter()
            .chain(&eval_entries)
            .map(|entry| entry.transport_retry_succeeded)
            .sum();
        let failed: u64 = calibration_entries
            .iter()
            .chain(&eval_entries)
            .map(|entry| entry.transport_retry_failed)
            .sum();
        json!({
            "attempted": attempted,
            "succeeded": succeeded,
            "failed": failed,
            "success_rate": if attempted == 0 { 1.0 } else { succeeded as f64 / attempted as f64 },
        })
    };
    let mut failures = collect_failures(&calibration_entries, &eval_entries);
    if options.ab {
        failures["ab"] = ab_failures;
    }
    let mut warnings = vec![
        "gold expected 为合成规则标签，非真实教师标注；校准/eval 拆分固定 seed 但样本小，指标仅作基线".to_owned(),
    ];
    if calibration_status["applied"].as_bool().unwrap_or(false) {
        warnings.push(
            "production 校准表已按严格 identity 套用；raw status 为未校准模拟，calibrated status 为生产门禁模拟（不改变逐项/总分指标）"
                .to_owned(),
        );
    } else {
        warnings.push(
            "未套用 production 校准表（--apply-calibration 未指定或 identity 不匹配）：status 仍为 uncalibrated 模拟"
                .to_owned(),
        );
    }
    if eval_calibrated["ece"].as_f64().unwrap_or(0.0) >= eval_raw["ece"].as_f64().unwrap_or(0.0) {
        warnings.push(
            "校准未降低 eval ECE（可能为样本小/校准集与 eval 分布偏移），如实保留未调参结果"
                .to_owned(),
        );
    }
    if let Some(ab) = &ab {
        let v2_failures = ab
            .pointer("/failures/v2")
            .and_then(|value| value.get("by_kind"))
            .and_then(Value::as_object)
            .map_or(0, |kinds| {
                kinds.values().filter_map(Value::as_u64).sum::<u64>()
            });
        if v2_failures > 0 {
            warnings.push(format!(
                "v2 有 {v2_failures} 次请求失败（schema/timeout），A/B 仅对 {} 条双方成功条目配对比较",
                ab.pointer("/paired_entries").and_then(Value::as_u64).unwrap_or(0)
            ));
        }
    }
    if ab.as_ref().is_some_and(|ab| {
        ab.pointer("/v2/mismatch_to_match")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            > 0
    }) {
        warnings.push(
            "v2 出现 mismatch→match（错答给对），不满足「必须 0」要求，A/B 判无效".to_owned(),
        );
    }
    if concurrency > 1 {
        warnings.push(format!(
            "并发={concurrency}：延迟含排队/竞争，与串行不可直接比较"
        ));
    }
    let tokens = token_totals(&calibration_entries, &eval_entries);
    let samples = eval_entries
        .iter()
        .filter(|entry| entry.failures.is_empty())
        .take(40)
        .map(|entry| {
            json!({
                "qid": entry.qid,
                "perturbation": entry.perturbation,
                "expected_score": entry.expected_score,
                "actual_score": entry.actual_score,
                "expected_judgement": entry.expected_judgement,
                "actual_judgement": entry.actual_judgement,
                "raw_confidence": entry.confidence,
                "calibrated_confidence": calibrator.calibrate(entry.confidence),
                "calibration_id": CALIBRATION_ID,
            })
        })
        .collect();
    let report = CalibratedReport {
        service: json!({
            "endpoint": options.endpoint,
            "model": options.model,
            "temperature": 0,
            "max_tokens": 256,
            "stream": false,
            "response_format": "json_object",
            "timeout_secs": options.timeout.as_secs(),
            "concurrency": concurrency,
            "quantization": "Q4_K_M",
            "engine": "llama.cpp / llama-server",
            "build_info": props.as_ref().and_then(|value| value.get("build_info")).and_then(Value::as_str).unwrap_or("unknown"),
            "server_slots": props.as_ref().and_then(|value| value.get("total_slots")).and_then(Value::as_u64).unwrap_or(0),
            "prompt_versions": ["v1", "v2"],
        }),
        split: json!({
            "seed": SEED,
            "method": "stratified perturbation×status；calibration/eval/ab 互斥",
            "eligible": dataset.items.iter().filter(|item| item.expected.route == "rubric_itemized").count(),
            "calibration": calibration_items.len(),
            "eval": eval_items.len(),
            "ab": ab_items.len(),
            "skipped_manual": skipped_manual,
            "prompt_version": prompt_version_label(selected_prompt(options)),
        }),
        calibration: json!({
            "id": CALIBRATION_ID,
            "method": "equal-frequency bins (10) + PAV monotone pooling",
            "fit_items": calibration_pairs.len(),
            "bins": calibrator.bins.len(),
            "table": calibrator.bins.iter().map(|bin| json!({
                "lower": bin.lower,
                "upper": bin.upper,
                "n": bin.count,
                "accuracy": bin.accuracy,
            })).collect::<Vec<_>>(),
            "reliability_calibration": calibrator.reliability_table(&calibration_pairs),
        }),
        eval: {
            let eval_pairs = item_pairs(&eval_entries);
            json!({
                "n": eval_items.len(),
                "item_pairs": eval_pairs.len(),
                "raw": eval_raw,
                "calibrated": eval_calibrated,
                "calibration_id": CALIBRATION_ID,
                "method": "equal-frequency bins (10) + PAV monotone pooling",
                "reliability_before": calibrator.reliability_table(&eval_pairs),
                "reliability_after": calibrator.reliability_table_after(&eval_pairs),
                "ece_raw": ece_value(&eval_pairs),
                "ece_calibrated": ece_value(&eval_pairs.iter().map(|(confidence, correct)| (calibrator.calibrate(*confidence), *correct)).collect::<Vec<_>>()),
                "brier_raw": brier_value(&eval_pairs),
                "brier_calibrated": brier_value(&eval_pairs.iter().map(|(confidence, correct)| (calibrator.calibrate(*confidence), *correct)).collect::<Vec<_>>()),
            })
        },
        ab,
        failures,
        repairs,
        retries,
        calibration_status,
        duration_ms: 0,
        tokens,
        warnings,
        gold_hash: hash,
        samples,
    };
    let mut report = report;
    report.duration_ms = started.elapsed().as_millis();
    if options.write_calibration {
        write_calibration_file(&report, options)?;
    }
    Ok(report)
}

fn registry_identity(model: &str) -> Result<(String, String), String> {
    let registry = eg_inference::registry::ModelRegistry::parse_str(
        eg_inference::registry::DEFAULT_REGISTRY_TOML,
    )
    .map_err(|error| format!("注册表解析失败：{error}"))?;
    let entry = registry
        .require(model)
        .map_err(|error| format!("模型 {model} 不在注册表：{error}"))?;
    Ok((entry.version.clone(), entry.quantization.clone()))
}

/// 将本次拟合的校准表写到 `config/calibration/<calibration_id>.json`（不含学生数据）。
fn write_calibration_file(report: &CalibratedReport, options: &Options) -> Result<(), String> {
    let points = report.calibration["table"]
        .as_array()
        .ok_or_else(|| "报告缺少校准表".to_owned())?
        .iter()
        .map(|bin| {
            json!({
                "lower": bin["lower"],
                "upper": bin["upper"],
                "calibrated": bin["accuracy"],
            })
        })
        .collect::<Vec<_>>();
    let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let (model_version, quantization) = registry_identity(&options.model)?;
    let file = json!({
        "schema_version": 1,
        "id": CALIBRATION_ID,
        "model_id": options.model,
        "model_version": model_version,
        "quantization": quantization,
        "method": "equal-frequency bins (10) + PAV monotone pooling",
        "table": points,
        "fit": {
            "n": report.eval["item_pairs"],
            "seed": SEED,
            "prompt_version": report.split["prompt_version"],
            "calibration_entries": report.split["calibration"],
            "eval_entries": report.split["eval"],
        },
        "created_at": created_at,
        "gold_sha256": report.gold_hash,
    });
    let text = serde_json::to_string_pretty(&file)
        .map_err(|error| format!("序列化校准表失败：{error}"))?;
    let dir = std::path::Path::new("config/calibration");
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("创建 {} 失败：{error}", dir.display()))?;
    let path = dir.join(format!("{CALIBRATION_ID}.json"));
    std::fs::write(&path, text).map_err(|error| format!("写 {} 失败：{error}", path.display()))?;
    eprintln!("校准表已写入 {}", path.display());
    Ok(())
}

fn item_pairs(entries: &[EntryResult]) -> Vec<(f64, bool)> {
    entries
        .iter()
        .filter(|entry| entry.failures.is_empty())
        .flat_map(|entry| entry.ece_pairs.iter().copied())
        .collect()
}

fn eval_metrics(entries: &[EntryResult], calibrator: Option<&Calibrator>) -> Value {
    let successful: Vec<&EntryResult> = entries
        .iter()
        .filter(|entry| entry.failures.is_empty())
        .collect();
    let mut scored = 0usize;
    let mut score_ok = 0usize;
    let mut half_ok = 0usize;
    let mut status_ok = 0usize;
    let mut item_total = 0usize;
    let mut item_match_ok = 0usize;
    let mut item_lenient_ok = 0usize;
    let mut partial_expected = 0usize;
    let mut partial_recalled = 0usize;
    let mut mismatch_expected = 0usize;
    let mut mismatch_to_match = 0usize;
    let mut partial_calls = 0usize;
    let mut pairs = Vec::new();
    let mut confusion: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    for entry in successful {
        scored += 1;
        if entry.actual_score == entry.expected_score {
            score_ok += 1;
        }
        if entry.actual_score.abs_diff(entry.expected_score)
            <= u64::try_from((entry.max_score / 2).max(1)).unwrap_or(0)
        {
            half_ok += 1;
        }
        item_total += entry.item_total;
        item_match_ok += entry.item_match_ok;
        item_lenient_ok += entry
            .observations
            .iter()
            .filter(|(expected, _, judgement, _)| {
                if *expected {
                    judgement == "match" || judgement == "partial"
                } else {
                    judgement == "mismatch"
                }
            })
            .count();
        partial_calls += entry.partial_calls;
        if entry.expected_judgement == "partial" {
            partial_expected += 1;
            if entry.actual_judgement == "partial" {
                partial_recalled += 1;
            }
        }
        if entry.expected_judgement == "mismatch" {
            mismatch_expected += 1;
            if entry.actual_judgement == "match" {
                mismatch_to_match += 1;
            }
        }
        let calibrated_confidence =
            calibrator.map_or(entry.confidence, |cal| cal.calibrate(entry.confidence));
        let status = if calibrator.is_some() {
            if calibrated_confidence >= 0.80 {
                "auto_accept"
            } else {
                "needs_review"
            }
        } else {
            "needs_review"
        };
        if status == entry.expected_status {
            status_ok += 1;
        }
        pairs.extend(entry.ece_pairs.iter().map(|(confidence, correct)| {
            let mapped = calibrator.map_or(*confidence, |cal| cal.calibrate(*confidence));
            (mapped, *correct)
        }));
        confusion
            .entry(entry.expected_judgement.clone())
            .or_default()
            .entry(entry.actual_judgement.clone())
            .and_modify(|count| *count += 1)
            .or_insert(1);
    }
    let ece = ece_value(&pairs);
    let brier = brier_value(&pairs);
    json!({
        "scored_entries": scored,
        "score_exact_accuracy": ratio(score_ok, scored),
        "within_half_accuracy": ratio(half_ok, scored),
        "status_consistency": ratio(status_ok, scored),
        "item_total": item_total,
        "item_match_accuracy": ratio(item_match_ok, item_total),
        "item_lenient_accuracy": ratio(item_lenient_ok, item_total),
        "item_match_ok": item_match_ok,
        "item_lenient_ok": item_lenient_ok,
        "partial_calls": partial_calls,
        "partial_expected_entries": partial_expected,
        "partial_recalled_entries": partial_recalled,
        "partial_recall": ratio(partial_recalled, partial_expected),
        "mismatch_expected_entries": mismatch_expected,
        "mismatch_to_match": mismatch_to_match,
        "ece": ece,
        "brier": brier,
        "confusion_matrix": confusion,
    })
}

fn ece_value(pairs: &[(f64, bool)]) -> f64 {
    if pairs.is_empty() {
        return 0.0;
    }
    let mut bins = [(0u64, 0u64, 0.0f64); 10];
    for (confidence, correct) in pairs {
        let index = ((confidence.clamp(0.0, 0.999_999) * 10.0) as usize).min(9);
        bins[index].0 += 1;
        if *correct {
            bins[index].1 += 1;
        }
        bins[index].2 += confidence;
    }
    let total = pairs.len() as f64;
    bins.iter()
        .filter(|(count, _, _)| *count > 0)
        .map(|(count, correct, sum)| {
            let n = *count as f64;
            n / total * ((*correct as f64 / n) - (*sum / n)).abs()
        })
        .sum()
}

fn brier_value(pairs: &[(f64, bool)]) -> f64 {
    if pairs.is_empty() {
        return 0.0;
    }
    pairs
        .iter()
        .map(|(confidence, correct)| {
            let truth = if *correct { 1.0 } else { 0.0 };
            (confidence - truth).powi(2)
        })
        .sum::<f64>()
        / pairs.len() as f64
}

fn token_totals(calibration: &[EntryResult], eval: &[EntryResult]) -> Value {
    let mut prompt = 0u64;
    let mut completion = 0u64;
    let mut calls = 0u64;
    for entry in calibration.iter().chain(eval) {
        prompt += entry.prompt_tokens;
        completion += entry.completion_tokens;
        calls += u64::try_from(entry.call_latencies.len()).unwrap_or(0);
    }
    json!({"prompt_total": prompt, "completion_total": completion, "total": prompt + completion, "calls": calls})
}

fn production_status(calibrated_confidence: f64) -> &'static str {
    if calibrated_confidence >= 0.85 {
        "auto_accept"
    } else {
        "needs_review"
    }
}

/// eval 门禁模拟：用 production 校准表按 `is_calibration_applied + ModelDirect 标准阈值(0.85)` 判定。
fn production_calibration_status(entries: &[EntryResult], options: &Options) -> Value {
    let Some(path) = options.apply_calibration.as_deref() else {
        return json!({"applied": false, "reason": "未指定 --apply-calibration"});
    };
    let identity = (|| -> Result<(String, String), String> {
        let registry = eg_inference::registry::ModelRegistry::parse_str(
            eg_inference::registry::DEFAULT_REGISTRY_TOML,
        )
        .map_err(|error| format!("注册表解析失败：{error}"))?;
        let entry = registry
            .require(&options.model)
            .map_err(|error| format!("模型 {} 不在注册表：{error}", options.model))?;
        Ok((entry.version.clone(), entry.quantization.clone()))
    })();
    let Ok((version, quantization)) = identity else {
        return json!({"applied": false, "reason": identity.err().unwrap_or_default()});
    };
    let table = match eg_inference::calibration::CalibrationTable::load(path) {
        Ok(table) => table,
        Err(error) => {
            return json!({
                "applied": false,
                "reason": format!("校准表不套用：{error}"),
                "path": path.display().to_string(),
                "identity": {"model": options.model, "version": version, "quantization": quantization},
            });
        }
    };
    if !table.matches(&options.model, &version, &quantization) {
        return json!({
            "applied": false,
            "reason": "校准表 identity 与模型/版本/量化不一致，显式不套用",
            "path": path.display().to_string(),
            "table_id": table.id,
            "identity": {"model": options.model, "version": version, "quantization": quantization},
        });
    }
    let threshold = 0.85;
    let successful: Vec<&EntryResult> = entries
        .iter()
        .filter(|entry| entry.failures.is_empty())
        .collect();
    let mut raw_auto = 0usize;
    let mut raw_review = 0usize;
    let mut raw_ok = 0usize;
    let mut cal_auto = 0usize;
    let mut cal_review = 0usize;
    let mut cal_ok = 0usize;
    let mut pairs = Vec::new();
    for entry in &successful {
        let raw_status = "needs_review";
        if raw_status == entry.expected_status {
            raw_ok += 1;
        }
        if raw_status == "auto_accept" {
            raw_auto += 1;
        } else {
            raw_review += 1;
        }
        let calibrated = eg_domain::Confidence::new(entry.confidence)
            .ok()
            .and_then(|confidence| table.calibrate(confidence))
            .map_or(entry.confidence, eg_domain::Confidence::get);
        let cal_status = production_status(calibrated);
        if cal_status == entry.expected_status {
            cal_ok += 1;
        }
        if cal_status == "auto_accept" {
            cal_auto += 1;
        } else {
            cal_review += 1;
        }
        pairs.extend(entry.ece_pairs.iter().map(|(confidence, correct)| {
            let mapped = eg_domain::Confidence::new(*confidence)
                .ok()
                .and_then(|value| table.calibrate(value))
                .map_or(*confidence, eg_domain::Confidence::get);
            (mapped, *correct)
        }));
    }
    let total = successful.len();
    json!({
        "applied": true,
        "path": path.display().to_string(),
        "table_id": table.id,
        "identity": {"model": options.model, "version": version, "quantization": quantization},
        "threshold": threshold,
        "n": total,
        "raw": {
            "auto_accept": raw_auto,
            "needs_review": raw_review,
            "manual": 0,
            "status_consistency": ratio(raw_ok, total),
        },
        "calibrated": {
            "auto_accept": cal_auto,
            "needs_review": cal_review,
            "manual": 0,
            "status_consistency": ratio(cal_ok, total),
            "ece": ece_value(&pairs),
        },
        "delta_status_consistency": ratio(cal_ok, total) - ratio(raw_ok, total),
        "auto_accept_delta": i64::try_from(cal_auto).unwrap_or(i64::MAX)
            - i64::try_from(raw_auto).unwrap_or(i64::MAX),
    })
}

fn collect_entries_failures(entries: &[EntryResult]) -> Value {
    let mut by_kind: BTreeMap<String, u64> = BTreeMap::new();
    let mut samples = Vec::new();
    for entry in entries {
        for failure in &entry.failures {
            *by_kind.entry(failure.kind.clone()).or_default() += 1;
            if samples.len() < 20 {
                samples.push(failure.clone());
            }
        }
    }
    json!({"by_kind": by_kind, "samples": samples})
}

fn collect_failures(calibration: &[EntryResult], eval: &[EntryResult]) -> Value {
    let mut by_kind: BTreeMap<String, u64> = BTreeMap::new();
    let mut samples = Vec::new();
    for entry in calibration.iter().chain(eval) {
        for failure in &entry.failures {
            *by_kind.entry(failure.kind.clone()).or_default() += 1;
            if samples.len() < 40 {
                samples.push(failure.clone());
            }
        }
    }
    json!({"by_kind": by_kind, "samples": samples})
}

// ---------------------------------------------------------------------------
// HTTP（仅 http://127.0.0.1；无 TLS/鉴权；std 实现，无新增依赖）
// ---------------------------------------------------------------------------

/// 读取 llama-server `/props`（服务版本/槽位；只读诊断，不参与判定）。
fn fetch_props(endpoint: &str) -> Option<Value> {
    let rest = endpoint.strip_prefix("http://")?;
    let (host_port, path) = rest.split_once('/').unwrap_or((rest, ""));
    let (host, port) = host_port
        .rsplit_once(':')
        .and_then(|(host, port)| port.parse::<u16>().ok().map(|port| (host, port)))
        .unwrap_or((host_port, 80));
    let root = format!("/{}", path.trim_end_matches('/'));
    let props_path = if let Some(root) = root.strip_suffix("/v1") {
        format!("{root}/props")
    } else {
        format!("{root}/props")
    };
    let mut stream = TcpStream::connect((host, port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .ok()?;
    let request =
        format!("GET {props_path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).ok()?;
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let (_, body) = text.split_once("\r\n\r\n")?;
    serde_json::from_str(body.trim()).ok()
}

/// 识别「partial 缺 satisfied/missing」这一类可机械修复的 Schema 违规。
fn is_partial_missing_lists(detail: &str) -> bool {
    detail.contains("partial 判定必须至少给出一个 satisfied 或 missing 项")
}

/// 仅 timeout/连接重置/断开类错误可重试；schema/HTTP 状态错误不重试。
fn is_transient(error: &JudgeError) -> bool {
    matches!(error.kind, FailureKind::Timeout | FailureKind::Transport)
}

struct JudgeClient {
    host: String,
    port: u16,
    path: String,
    model: String,
    timeout: Duration,
}

struct JudgeResponse {
    judgement: String,
    confidence: f64,
    latency_ms: u128,
    prompt_tokens: u64,
    completion_tokens: u64,
    calibration_present: bool,
    repair_attempted: bool,
    repair_succeeded: bool,
    transport_retried: bool,
    transport_succeeded: bool,
}

#[derive(Debug)]
struct JudgeError {
    kind: FailureKind,
    detail: String,
}

impl JudgeClient {
    fn new(endpoint: &str, model: String, timeout: Duration) -> Result<Self, String> {
        let Some(rest) = endpoint.strip_prefix("http://") else {
            return Err(
                "judge-endpoint 必须是 http://127.0.0.1:PORT/v1（不支持 https/网络）".to_owned(),
            );
        };
        let (host_port, path) = match rest.split_once('/') {
            Some((host_port, path)) => (host_port, format!("/{path}")),
            None => (rest, String::new()),
        };
        let (host, port) = match host_port.rsplit_once(':') {
            Some((host, port)) => (
                host.to_owned(),
                port.parse::<u16>()
                    .map_err(|_| format!("judge-endpoint 端口非法：{port}"))?,
            ),
            None => (host_port.to_owned(), 80),
        };
        let path = format!("{}/chat/completions", path.trim_end_matches('/'));
        Ok(Self {
            host,
            port,
            path,
            model,
            timeout,
        })
    }

    /// 瞬态错误（超时/连接重置）→ 退避 500ms 后重试 1 次；schema_violation 不在此路径重试。
    fn call_with_transient_retry(
        &self,
        prompt: &str,
        max_score: MilliScore,
    ) -> Result<(JudgeOutput, u64, u64, bool), JudgeError> {
        match self.call_once(prompt, max_score) {
            Ok(result) => Ok((result.0, result.1, result.2, false)),
            Err(error) if is_transient(&error) => {
                std::thread::sleep(Duration::from_millis(500));
                match self.call_once(prompt, max_score) {
                    Ok(result) => Ok((result.0, result.1, result.2, true)),
                    Err(second) => Err(JudgeError {
                        kind: second.kind,
                        detail: format!(
                            "瞬态错误重试 1 次仍失败：首次={}；重试={}",
                            error.detail, second.detail
                        ),
                    }),
                }
            }
            Err(error) => Err(error),
        }
    }

    fn judge(&self, prompt: &str, max_score: MilliScore) -> Result<JudgeResponse, JudgeError> {
        let started = Instant::now();
        let (
            output,
            prompt_tokens,
            completion_tokens,
            repair_attempted,
            repair_succeeded,
            transport_retried,
            transport_succeeded,
        ) = match self.call_with_transient_retry(prompt, max_score) {
            Ok((output, prompt_tokens, completion_tokens, retried)) => (
                output,
                prompt_tokens,
                completion_tokens,
                false,
                false,
                retried,
                retried,
            ),
            Err(error)
                if error.kind == FailureKind::SchemaViolation
                    && is_partial_missing_lists(&error.detail) =>
            {
                // 机械修复：只反馈违规字段并允许一次重试；严格校验不变（仍失败计入失败）。
                let repair_prompt = format!(
                    "{prompt}\n\n上一次输出违反 Schema：{}。请修正并只输出 JSON：若 judgement 为 partial，satisfied 与 missing 必须都是非空数组（写出满足/缺失要素）；若无法给出列表，请输出 match 或 mismatch。",
                    error.detail
                );
                match self.call_with_transient_retry(&repair_prompt, max_score) {
                    Ok((output, prompt_tokens, completion_tokens, retried)) => (
                        output,
                        prompt_tokens,
                        completion_tokens,
                        true,
                        true,
                        retried,
                        retried,
                    ),
                    Err(second) => {
                        return Err(JudgeError {
                            kind: second.kind,
                            detail: format!(
                                "partial schema 违规且 1 次修复重试仍失败：首次={}；重试={}",
                                error.detail, second.detail
                            ),
                        });
                    }
                }
            }
            Err(error) => return Err(error),
        };
        let elapsed = started.elapsed();
        let judgement = match output.judgement {
            JudgeTag::Match => "match",
            JudgeTag::Partial => "partial",
            JudgeTag::Mismatch => "mismatch",
        };
        Ok(JudgeResponse {
            judgement: judgement.to_owned(),
            confidence: output.confidence.get(),
            latency_ms: elapsed.as_millis(),
            prompt_tokens,
            completion_tokens,
            calibration_present: output.calibration_id.is_some(),
            repair_attempted,
            repair_succeeded,
            transport_retried,
            transport_succeeded,
        })
    }

    fn call_once(
        &self,
        prompt: &str,
        max_score: MilliScore,
    ) -> Result<(JudgeOutput, u64, u64), JudgeError> {
        let body = json!({
            "model": self.model,
            "messages": [
                {"role": "system", "content": SYSTEM_PROMPT},
                {"role": "user", "content": prompt}
            ],
            "temperature": 0.0,
            "max_tokens": 256,
            "stream": false,
            "response_format": {"type": "json_object"}
        })
        .to_string();
        let response = self.post(&body)?;
        let root: Value = serde_json::from_str(&response).map_err(|error| JudgeError {
            kind: FailureKind::InvalidJson,
            detail: format!("响应不是 JSON：{error}"),
        })?;
        let content = root
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("message"))
            .and_then(|message| message.get("content"))
            .and_then(Value::as_str)
            .ok_or_else(|| JudgeError {
                kind: FailureKind::MissingContent,
                detail: "缺少 choices[0].message.content".to_owned(),
            })?;
        let value: Value = serde_json::from_str(content.trim()).map_err(|error| JudgeError {
            kind: FailureKind::ContentNotJson,
            detail: format!("模型内容不是 JSON：{error}"),
        })?;
        let output = JudgeOutput::validate(&value, max_score).map_err(|error| JudgeError {
            kind: FailureKind::SchemaViolation,
            detail: error.to_string(),
        })?;
        let prompt_tokens = root
            .get("usage")
            .and_then(|usage| usage.get("prompt_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let completion_tokens = root
            .get("usage")
            .and_then(|usage| usage.get("completion_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        Ok((output, prompt_tokens, completion_tokens))
    }

    fn post(&self, body: &str) -> Result<String, JudgeError> {
        let mut stream =
            TcpStream::connect((self.host.as_str(), self.port)).map_err(|error| JudgeError {
                kind: FailureKind::Transport,
                detail: format!("连接 {}:{} 失败：{error}", self.host, self.port),
            })?;
        stream
            .set_write_timeout(Some(self.timeout))
            .map_err(|error| JudgeError {
                kind: FailureKind::Transport,
                detail: error.to_string(),
            })?;
        stream
            .set_read_timeout(Some(self.timeout))
            .map_err(|error| JudgeError {
                kind: FailureKind::Transport,
                detail: error.to_string(),
            })?;
        let request = format!(
            "POST {} HTTP/1.1\r\nHost: {}:{}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
            self.path,
            self.host,
            self.port,
            body.len(),
            body
        );
        stream.write_all(request.as_bytes()).map_err(|error| {
            let kind = if error.kind() == std::io::ErrorKind::TimedOut {
                FailureKind::Timeout
            } else {
                FailureKind::Transport
            };
            JudgeError {
                kind,
                detail: format!("写请求失败：{error}"),
            }
        })?;
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).map_err(|error| {
            let kind = if matches!(
                error.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            ) {
                FailureKind::Timeout
            } else {
                FailureKind::Transport
            };
            JudgeError {
                kind,
                detail: format!("读响应失败：{error}"),
            }
        })?;
        let text = String::from_utf8_lossy(&bytes);
        let (head, body) = text.split_once("\r\n\r\n").ok_or_else(|| JudgeError {
            kind: FailureKind::InvalidJson,
            detail: "HTTP 响应缺少头体分隔".to_owned(),
        })?;
        let status = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse::<u16>().ok())
            .ok_or_else(|| JudgeError {
                kind: FailureKind::InvalidJson,
                detail: "无法解析 HTTP 状态行".to_owned(),
            })?;
        if status != 200 {
            return Err(JudgeError {
                kind: FailureKind::HttpStatus,
                detail: format!(
                    "HTTP {status}：{}",
                    body.chars().take(200).collect::<String>()
                ),
            });
        }
        let chunked = head
            .to_ascii_lowercase()
            .contains("transfer-encoding: chunked");
        Ok(if chunked {
            decode_chunked(body)?
        } else {
            body.to_owned()
        })
    }
}

fn decode_chunked(body: &str) -> Result<String, JudgeError> {
    let mut out = String::new();
    let mut rest = body;
    while let Some((size_line, after)) = rest.split_once("\r\n") {
        let size = usize::from_str_radix(size_line.trim(), 16).map_err(|error| JudgeError {
            kind: FailureKind::InvalidJson,
            detail: format!("chunk 长度非法：{error}"),
        })?;
        if size == 0 {
            break;
        }
        if after.len() < size {
            return Err(JudgeError {
                kind: FailureKind::InvalidJson,
                detail: "chunk 数据不完整".to_owned(),
            });
        }
        out.push_str(&after[..size]);
        rest = after.get(size + 2..).unwrap_or("");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_chunked_body() {
        let body = "4\r\n{\"a\"\r\n3\r\n:1}\r\n0\r\n\r\n";
        assert_eq!(decode_chunked(body).unwrap(), "{\"a\":1}");
    }

    #[test]
    fn prompt_version_selection_and_prompt_content() {
        let v1 = Options::default();
        assert_eq!(selected_prompt(&v1), PromptVersion::V1);
        assert_eq!(prompt_version_label(selected_prompt(&v1)), "v1");
        let v2 = Options {
            prompt: PromptVersion::V2,
            ..Options::default()
        };
        assert_eq!(selected_prompt(&v2), PromptVersion::V2);
        assert_eq!(prompt_version_label(selected_prompt(&v2)), "v2");

        // 不依赖服务：真实 gold 条目上生成提示词，确认 V1/V2 内容确实不同。
        let bytes = std::fs::read(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/gold/solution-200.json"),
        )
        .unwrap();
        let dataset: SolutionDataset = serde_json::from_slice(&bytes).unwrap();
        let item = dataset
            .items
            .iter()
            .find(|item| item.expected.route == "rubric_itemized")
            .unwrap();
        let rubric = item.rubric.as_ref().unwrap();
        let criterion = rubric.items[0].criterion.as_str();
        let points = rubric.items[0].points.as_milli();
        let prompt_v1 = item_prompt(item, criterion, points, PromptVersion::V1);
        let prompt_v2 = item_prompt(item, criterion, points, PromptVersion::V2);
        assert_ne!(prompt_v1, prompt_v2, "V1/V2 提示词必须不同（接线证据）");
        assert!(prompt_v1.contains("match 或 mismatch"));
        assert!(prompt_v2.contains("judgement_criteria"));
        assert!(prompt_v2.contains("partial_score_milli"));
        assert!(!prompt_v1.contains("judgement_criteria"));
    }

    #[test]
    fn production_status_simulation_thresholds() {
        assert_eq!(production_status(0.9198), "auto_accept");
        assert_eq!(production_status(0.85), "auto_accept");
        assert_eq!(production_status(0.8499), "needs_review");
        assert_eq!(production_status(0.0), "needs_review");
    }

    #[test]
    fn production_calibration_rejects_identity_mismatch_and_applies_repo_table() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../config/calibration/qwen35-4b-q4-iso-v1.json");
        let mut options = Options::default();
        options.apply_calibration = Some(path.clone());
        let status = production_calibration_status(&[], &options);
        assert_eq!(status["applied"], true);
        assert_eq!(status["table_id"], "qwen35-4b-q4-iso-v1");
        // 错误模型身份：显式不套用。
        options.model = "qwen3.5-4b-router".to_owned();
        let status = production_calibration_status(&[], &options);
        assert_eq!(status["applied"], false);
        assert!(
            status["reason"]
                .as_str()
                .unwrap_or_default()
                .contains("identity")
        );
    }

    #[test]
    fn transient_error_policy_is_limited_to_timeout_and_transport() {
        assert!(is_transient(&JudgeError {
            kind: FailureKind::Timeout,
            detail: "timeout".to_owned()
        }));
        assert!(is_transient(&JudgeError {
            kind: FailureKind::Transport,
            detail: "reset".to_owned()
        }));
        assert!(!is_transient(&JudgeError {
            kind: FailureKind::SchemaViolation,
            detail: "partial".to_owned()
        }));
        assert!(!is_transient(&JudgeError {
            kind: FailureKind::HttpStatus,
            detail: "500".to_owned()
        }));
    }

    #[test]
    fn detects_partial_missing_list_violation() {
        assert!(is_partial_missing_lists(
            "partial 判定必须至少给出一个 satisfied 或 missing 项"
        ));
        assert!(!is_partial_missing_lists(
            "match 判定必须给满分 5000，实际 0"
        ));
        assert!(!is_partial_missing_lists("transport timeout"));
    }

    #[test]
    fn calibrator_is_monotonic_and_reduces_overconfidence() {
        // 过自信合成数据：confidence 0.9 但仅 50% 正确。
        let pairs: Vec<(f64, bool)> = (0..100).map(|index| (0.9, index % 2 == 0)).collect();
        let calibrator = Calibrator::fit(&pairs, 10);
        let mut previous = 0.0;
        for bin in &calibrator.bins {
            assert!(bin.accuracy + 1e-12 >= previous, "PAV 必须单调非降");
            previous = bin.accuracy;
        }
        let raw = ece_value(&pairs);
        let mapped: Vec<(f64, bool)> = pairs
            .iter()
            .map(|(confidence, correct)| (calibrator.calibrate(*confidence), *correct))
            .collect();
        let calibrated = ece_value(&mapped);
        assert!(
            calibrated <= raw + 1e-12,
            "校准后 ECE 不应变差：raw={raw} cal={calibrated}"
        );
        assert!(
            (calibrator.calibrate(0.9) - 0.5).abs() < 0.2,
            "0.9 应被拉回约 0.5"
        );
        assert_eq!(calibrator.id(), CALIBRATION_ID);
    }

    #[test]
    fn pav_pools_violating_bins() {
        // 人工构造单调违例：低 confidence 桶准确率高、高 confidence 桶准确率低。
        let pairs: Vec<(f64, bool)> = vec![
            (0.1, true),
            (0.2, true),
            (0.3, true),
            (0.4, true),
            (0.7, false),
            (0.8, false),
            (0.9, false),
            (0.95, false),
        ];
        let calibrator = Calibrator::fit(&pairs, 4);
        for pair in calibrator.bins.windows(2) {
            assert!(
                pair[0].accuracy <= pair[1].accuracy + 1e-12,
                "PAV 合并后必须单调"
            );
        }
    }

    #[test]
    fn stratified_sample_covers_strata_and_is_deterministic() {
        // 用真实 gold 的 solution 数据集做确定性抽样检查（只读）。
        let bytes = std::fs::read(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/gold/solution-200.json"),
        )
        .unwrap();
        let dataset: SolutionDataset = serde_json::from_slice(&bytes).unwrap();
        let mut items: Vec<&SolutionItem> = dataset
            .items
            .iter()
            .filter(|item| item.expected.route == "rubric_itemized")
            .collect();
        let first: Vec<String> = stratified_sample(&mut items, 60)
            .into_iter()
            .map(|item| item.qid.clone())
            .collect();
        let mut items2: Vec<&SolutionItem> = dataset
            .items
            .iter()
            .filter(|item| item.expected.route == "rubric_itemized")
            .collect();
        let second: Vec<String> = stratified_sample(&mut items2, 60)
            .into_iter()
            .map(|item| item.qid.clone())
            .collect();
        assert_eq!(first, second);
        assert_eq!(first.len(), 60);
    }
}
