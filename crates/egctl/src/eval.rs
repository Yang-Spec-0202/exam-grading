//! `egctl eval --gold fixtures/gold`：S2/S3/S5 合成金标评估器。
//!
//! 不调用真实模型/网络：按 `expected.route` 分流，规则路径走 `eg-subject-math` 归一化 +
//! 比较器链，评分标准路径用 `eg-grade::aggregate` 聚合（`expected.steps[].matched` 作为
//! judge 逐项输入），manual 只断言路由/状态/原因。**不修改 gold/实现凑数**；失败按维度分类。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use eg_domain::{
    AnswerKey, AnswerValue, Confidence, MilliScore, PaperId, Question, QuestionId, QuestionKind,
    RawAnswer, RouteDecision, Rubric, RubricItemId, SubjectId, SubjectPack, Ulid,
};
use eg_grade::aggregate::{aggregate, parse_option_letters, partial_choice};
use eg_grade::{GradeConfig, RouteInput, evaluate_rule_chain, route};
use eg_subject_math::MathSubjectPack;
use serde::Deserialize;
use serde_json::{Value, json};

/// 评估选项。
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// `fixtures/gold` 目录。
    pub gold: PathBuf,
    /// 仅取前 N 条客观题（CI 子集）。
    pub max_objective: Option<usize>,
    /// 仅取前 N 条解答题（CI 子集）。
    pub max_solution: Option<usize>,
}

/// SPEC 阈值（报告必须列明）。
pub const THRESHOLDS: [(&str, &str); 5] = [
    ("S2_objective_accuracy", "100%"),
    ("S3_score_exact", "≥85%"),
    ("S3_within_half", "≥97%"),
    ("S3_ece", "≤0.08"),
    ("S5_route_accuracy", "≥98%"),
];

/// 分组统计（总体/kind/扰动/status）。
#[derive(Debug, Default, Clone)]
pub struct Stats {
    total: u64,
    route_ok: u64,
    route_label_ok: u64,
    normalized_ok: u64,
    normalized_total: u64,
    judgement_ok: u64,
    judgement_total: u64,
    score_ok: u64,
    score_total: u64,
    half_ok: u64,
    status_ok: u64,
    reason_ok: u64,
    reason_total: u64,
    step_ok: u64,
    step_total: u64,
    tier_ok: u64,
    tier_total: u64,
    auto_accept: u64,
    needs_review: u64,
    manual: u64,
    false_zero: u64,
    calibration_ok: u64,
    calibration_total: u64,
    manual_total: u64,
    manual_ok: u64,
}

impl Stats {
    /// 样本数。
    #[must_use]
    pub const fn total(&self) -> u64 {
        self.total
    }

    /// 路由规则命中数。
    #[must_use]
    pub const fn route_ok(&self) -> u64 {
        self.route_ok
    }

    /// 分数精确一致数。
    #[must_use]
    pub const fn score_ok(&self) -> u64 {
        self.score_ok
    }

    /// 参与分数断言数。
    #[must_use]
    pub const fn score_total(&self) -> u64 {
        self.score_total
    }

    /// 误给 0 数。
    #[must_use]
    pub const fn false_zero(&self) -> u64 {
        self.false_zero
    }

    /// 逐评分项一致数。
    #[must_use]
    pub const fn step_ok(&self) -> u64 {
        self.step_ok
    }

    /// 逐评分项总数。
    #[must_use]
    pub const fn step_total(&self) -> u64 {
        self.step_total
    }

    fn ratio(numerator: u64, denominator: u64) -> f64 {
        if denominator == 0 {
            1.0
        } else {
            f64::from(u32::try_from(numerator).unwrap_or(u32::MAX))
                / f64::from(u32::try_from(denominator).unwrap_or(u32::MAX))
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "total": self.total,
            "route_rule_accuracy": Self::ratio(self.route_ok, self.total),
            "route_label_accuracy": Self::ratio(self.route_label_ok, self.total),
            "normalized_accuracy": Self::ratio(self.normalized_ok, self.normalized_total),
            "normalized_total": self.normalized_total,
            "judgement_accuracy": Self::ratio(self.judgement_ok, self.judgement_total),
            "judgement_total": self.judgement_total,
            "score_exact_accuracy": Self::ratio(self.score_ok, self.score_total),
            "score_total": self.score_total,
            "within_half_accuracy": Self::ratio(self.half_ok, self.score_total),
            "status_consistency": Self::ratio(self.status_ok, self.total),
            "review_reason_consistency": Self::ratio(self.reason_ok, self.reason_total),
            "step_effect_accuracy": Self::ratio(self.step_ok, self.step_total),
            "step_total": self.step_total,
            "partial_choice_tier_accuracy": Self::ratio(self.tier_ok, self.tier_total),
            "tier_total": self.tier_total,
            "auto_accept": self.auto_accept,
            "needs_review": self.needs_review,
            "manual": self.manual,
            "false_zero": self.false_zero,
            "calibration_consistency": Self::ratio(self.calibration_ok, self.calibration_total),
            "calibration_total": self.calibration_total,
            "manual_route_correct": Self::ratio(self.manual_ok, self.manual_total),
            "manual_total": self.manual_total,
        })
    }
}

/// 一条失败样本（脱敏：仅 qid/kind/扰动/期望-实际）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct FailureSample {
    /// 数据集（objective/solution）。
    pub dataset: String,
    /// 题目 id。
    pub qid: String,
    /// 题型。
    pub kind: String,
    /// 扰动标签。
    pub perturbation: String,
    /// 失败维度（route/normalized/judgement/score/status/reason/manual/step/tier/calibration/false_zero）。
    pub dimension: String,
    /// 期望值（文本）。
    pub expected: String,
    /// 实际值（文本）。
    pub actual: String,
    /// 分类：implementation_bug_suspect / expected_vs_implementation / data_missing。
    pub origin: String,
    /// 诊断。
    pub detail: String,
}

/// 单数据集报告。
#[derive(Debug, Clone, Default)]
pub struct DatasetReport {
    /// 总体统计。
    pub stats: Stats,
    /// 按 kind。
    pub by_kind: BTreeMap<String, Stats>,
    /// 按扰动。
    pub by_perturbation: BTreeMap<String, Stats>,
    /// 按 expected.status。
    pub by_status: BTreeMap<String, Stats>,
    /// 失败样本。
    pub failures: Vec<FailureSample>,
    /// ECE（分数正确性 vs expected.confidence）。
    pub ece: f64,
}

/// 完整评估报告。
#[derive(Debug, Clone, Default)]
pub struct EvalReport {
    /// 客观题。
    pub objective: DatasetReport,
    /// 解答题。
    pub solution: DatasetReport,
    /// 总样本。
    pub total_items: usize,
    /// 失败分类计数。
    pub failure_classes: BTreeMap<String, usize>,
    /// gold 文件 sha256 校验。
    pub gold_files: BTreeMap<String, String>,
    /// 阈值达成。
    pub targets_met: BTreeMap<String, bool>,
    /// 警告/限制。
    pub warnings: Vec<String>,
    /// 耗时毫秒。
    pub duration_ms: u128,
}

impl EvalReport {
    /// JSON 输出。
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "tool": "egctl",
            "command": "eval",
            "scenario": "S2/S3/S5",
            "gold_files": self.gold_files,
            "total_items": self.total_items,
            "duration_ms": self.duration_ms,
            "thresholds": THRESHOLDS.iter().map(|(name, target)| json!({"metric": name, "target": target})).collect::<Vec<_>>(),
            "targets_met": self.targets_met,
            "objective": Self::dataset_json(&self.objective),
            "solution": Self::dataset_json(&self.solution),
            "metrics": self.metrics_json(),
            "failure_classes": self.failure_classes,
            "warnings": self.warnings,
        })
    }

    fn metrics_json(&self) -> Value {
        let objective = &self.objective.stats;
        let solution = &self.solution.stats;
        let rubric_only = self
            .solution
            .by_perturbation
            .get("rubric_only")
            .cloned()
            .unwrap_or_default();
        json!({
            "s2": {
                "accuracy": Stats::ratio(objective.score_ok, objective.score_total),
                "normalized_accuracy": Stats::ratio(objective.normalized_ok, objective.normalized_total),
                "score_exact": Stats::ratio(objective.score_ok, objective.score_total),
                "status_consistency": Stats::ratio(objective.status_ok, objective.total),
                "auto_accept_rate": Stats::ratio(objective.auto_accept, objective.total),
                "review_rate": Stats::ratio(objective.needs_review, objective.total),
                "manual_rate": Stats::ratio(objective.manual, objective.total),
                "false_zero": objective.false_zero,
                "partial_choice_tier_accuracy": Stats::ratio(objective.tier_ok, objective.tier_total),
            },
            "s3": {
                "step_effect_accuracy": Stats::ratio(solution.step_ok, solution.step_total),
                "score_exact": Stats::ratio(solution.score_ok, solution.score_total),
                "within_half": Stats::ratio(solution.half_ok, solution.score_total),
                "status_consistency": Stats::ratio(solution.status_ok, solution.total),
                "ece": self.solution.ece,
                "manual_route_correct": Stats::ratio(solution.manual_ok, solution.manual_total),
                "rubric_only_score_exact": Stats::ratio(rubric_only.score_ok, rubric_only.score_total),
            },
            "s5": {
                "route_rule_accuracy": Stats::ratio(objective.route_ok + solution.route_ok, objective.total + solution.total),
                "route_label_accuracy": Stats::ratio(objective.route_label_ok + solution.route_label_ok, objective.total + solution.total),
            },
        })
    }

    fn dataset_json(dataset: &DatasetReport) -> Value {
        json!({
            "overall": dataset.stats.to_json(),
            "by_kind": dataset.by_kind.iter().map(|(key, value)| (key.clone(), value.to_json())).collect::<BTreeMap<_, _>>(),
            "by_perturbation": dataset.by_perturbation.iter().map(|(key, value)| (key.clone(), value.to_json())).collect::<BTreeMap<_, _>>(),
            "by_status": dataset.by_status.iter().map(|(key, value)| (key.clone(), value.to_json())).collect::<BTreeMap<_, _>>(),
            "ece": dataset.ece,
            "failures": dataset.failures,
        })
    }

    /// 人读输出。
    #[must_use]
    pub fn render_human(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "egctl eval（S2/S3/S5 合成金标）：{} 条；gold sha256 已校验：{}；耗时 {} ms\n",
            self.total_items,
            self.gold_files.len(),
            self.duration_ms
        ));
        for (name, target) in THRESHOLDS {
            let met = self.targets_met.get(name).copied().unwrap_or(false);
            out.push_str(&format!(
                "  阈值 {name} {target} → {}\n",
                if met { "达成" } else { "未达成" }
            ));
        }
        let objective = &self.objective.stats;
        let solution = &self.solution.stats;
        out.push_str(&format!(
            "S2 客观题：score_exact={:.2}% normalized={:.2}% status={:.2}% auto/review/manual={}/{}/{} false_zero={}\n",
            Stats::ratio(objective.score_ok, objective.score_total) * 100.0,
            Stats::ratio(objective.normalized_ok, objective.normalized_total) * 100.0,
            Stats::ratio(objective.status_ok, objective.total) * 100.0,
            objective.auto_accept, objective.needs_review, objective.manual, objective.false_zero
        ));
        out.push_str(&format!(
            "S3 解答题：step_effect={:.2}% score_exact={:.2}% within_half={:.2}% status={:.2}% ECE={:.4} manual_route={:.2}%\n",
            Stats::ratio(solution.step_ok, solution.step_total) * 100.0,
            Stats::ratio(solution.score_ok, solution.score_total) * 100.0,
            Stats::ratio(solution.half_ok, solution.score_total) * 100.0,
            Stats::ratio(solution.status_ok, solution.total) * 100.0,
            self.solution.ece,
            Stats::ratio(solution.manual_ok, solution.manual_total) * 100.0
        ));
        out.push_str(&format!(
            "S5 路由：rule_accuracy={:.2}% label_accuracy={:.2}%\n",
            Stats::ratio(
                objective.route_ok + solution.route_ok,
                objective.total + solution.total
            ) * 100.0,
            Stats::ratio(
                objective.route_label_ok + solution.route_label_ok,
                objective.total + solution.total
            ) * 100.0
        ));
        for (label, dataset) in [("objective", &self.objective), ("solution", &self.solution)] {
            let stats = &dataset.stats;
            out.push_str(&format!(
                "\n[{label}] n={} route_rule={:.2}% route_label={:.2}% score_exact={:.2}% status={:.2}% auto/review/manual={}/{}/{} false_zero={} ECE={:.4}\n",
                stats.total,
                Stats::ratio(stats.route_ok, stats.total) * 100.0,
                Stats::ratio(stats.route_label_ok, stats.total) * 100.0,
                Stats::ratio(stats.score_ok, stats.score_total) * 100.0,
                Stats::ratio(stats.status_ok, stats.total) * 100.0,
                stats.auto_accept,
                stats.needs_review,
                stats.manual,
                stats.false_zero,
                dataset.ece
            ));
            for (kind, stats) in &dataset.by_kind {
                out.push_str(&format!(
                    "  kind {kind:<18} n={:<4} score_exact={:.2}% status={:.2}% route_rule={:.2}%\n",
                    stats.total,
                    Stats::ratio(stats.score_ok, stats.score_total) * 100.0,
                    Stats::ratio(stats.status_ok, stats.total) * 100.0,
                    Stats::ratio(stats.route_ok, stats.total) * 100.0
                ));
            }
            for (perturbation, stats) in &dataset.by_perturbation {
                out.push_str(&format!(
                    "  perturb {perturbation:<22} n={:<4} score_exact={:.2}% status={:.2}%\n",
                    stats.total,
                    Stats::ratio(stats.score_ok, stats.score_total) * 100.0,
                    Stats::ratio(stats.status_ok, stats.total) * 100.0
                ));
            }
        }
        out.push_str("\n失败分类：");
        if self.failure_classes.is_empty() {
            out.push_str("无\n");
        } else {
            out.push('\n');
            for (class, count) in &self.failure_classes {
                out.push_str(&format!("  {class}: {count}\n"));
            }
        }
        for warning in &self.warnings {
            out.push_str(&format!("WARN: {warning}\n"));
        }
        out
    }
}

// ---------------------------------------------------------------------------
// gold schema
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct GoldIndex {
    #[serde(default)]
    files: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct Dataset<T> {
    items: Vec<T>,
}

#[derive(Debug, Deserialize)]
struct RecognitionMeta {
    confidence: f64,
}

#[derive(Debug, Deserialize)]
struct ObjectiveItem {
    qid: String,
    kind: QuestionKind,
    max_score_milli: i64,
    answer_key: AnswerKey,
    #[serde(default)]
    rubric: Option<Rubric>,
    raw_answer: RawAnswer,
    recognition: RecognitionMeta,
    #[serde(default)]
    perturbations: Vec<String>,
    expected: ObjectiveExpected,
}

#[derive(Debug, Deserialize)]
struct ObjectiveExpected {
    route: String,
    rule_trace: String,
    #[serde(default)]
    normalized: Value,
    #[serde(default)]
    judgement: Option<String>,
    #[serde(default)]
    score_milli: Option<i64>,
    status: String,
    #[serde(default)]
    review_reason: Option<String>,
    #[serde(default)]
    manual_reason: Option<String>,
    confidence: f64,
    #[serde(default)]
    partial_choice_tier: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct SolutionItem {
    qid: String,
    kind: QuestionKind,
    max_score_milli: i64,
    answer_key: AnswerKey,
    #[serde(default)]
    rubric: Option<Rubric>,
    raw_answer: RawAnswer,
    recognition: RecognitionMeta,
    #[serde(default)]
    perturbations: Vec<String>,
    #[serde(default)]
    judge: Option<JudgeMeta>,
    expected: SolutionExpected,
}

#[derive(Debug, Deserialize)]
struct JudgeMeta {
    #[serde(default)]
    calibration_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SolutionExpected {
    route: String,
    rule_trace: String,
    #[serde(default)]
    judgement: Option<String>,
    #[serde(default)]
    score_milli: Option<i64>,
    status: String,
    #[serde(default)]
    review_reason: Option<String>,
    #[serde(default)]
    manual_reason: Option<String>,
    confidence: f64,
    #[serde(default)]
    calibration_id: Option<String>,
    #[serde(default)]
    steps: Vec<ExpectedStep>,
}

#[derive(Debug, Deserialize)]
struct ExpectedStep {
    item_id: String,
    matched: bool,
    effective: bool,
}

#[derive(Debug, Default)]
struct Outcome {
    route_ok: bool,
    route_label_ok: bool,
    normalized: Option<bool>,
    judgement: Option<bool>,
    score: Option<bool>,
    half: Option<bool>,
    status_ok: bool,
    reason: Option<bool>,
    step_ok: u64,
    step_total: u64,
    tier: Option<bool>,
    calibration: Option<bool>,
    false_zero: bool,
    auto: bool,
    review: bool,
    manual: bool,
    manual_total: u64,
    manual_ok: bool,
}

impl Stats {
    fn record(&mut self, outcome: &Outcome) {
        self.total += 1;
        if outcome.route_ok {
            self.route_ok += 1;
        }
        if outcome.route_label_ok {
            self.route_label_ok += 1;
        }
        if let Some(ok) = outcome.normalized {
            self.normalized_total += 1;
            if ok {
                self.normalized_ok += 1;
            }
        }
        if let Some(ok) = outcome.judgement {
            self.judgement_total += 1;
            if ok {
                self.judgement_ok += 1;
            }
        }
        if let Some(ok) = outcome.score {
            self.score_total += 1;
            if ok {
                self.score_ok += 1;
            }
        }
        if outcome.half == Some(true) {
            self.half_ok += 1;
        }
        if outcome.status_ok {
            self.status_ok += 1;
        }
        if let Some(ok) = outcome.reason {
            self.reason_total += 1;
            if ok {
                self.reason_ok += 1;
            }
        }
        self.step_ok += outcome.step_ok;
        self.step_total += outcome.step_total;
        if let Some(ok) = outcome.tier {
            self.tier_total += 1;
            if ok {
                self.tier_ok += 1;
            }
        }
        if let Some(ok) = outcome.calibration {
            self.calibration_total += 1;
            if ok {
                self.calibration_ok += 1;
            }
        }
        if outcome.auto {
            self.auto_accept += 1;
        }
        if outcome.review {
            self.needs_review += 1;
        }
        if outcome.manual {
            self.manual += 1;
        }
        if outcome.false_zero {
            self.false_zero += 1;
        }
        self.manual_total += outcome.manual_total;
        if outcome.manual_ok {
            self.manual_ok += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// 运行
// ---------------------------------------------------------------------------

/// 执行评估。
///
/// # Errors
///
/// gold 目录/JSON/sha256/字段不合法时返回错误。
pub fn run(options: &Options) -> Result<EvalReport, String> {
    let started = Instant::now();
    let index_path = options.gold.join("index.json");
    let index: GoldIndex = read_json(&index_path)?;
    let mut gold_files = BTreeMap::new();
    for (name, expected) in &index.files {
        let path = options.gold.join(name);
        let bytes = std::fs::read(&path)
            .map_err(|error| format!("读取 {} 失败：{error}", path.display()))?;
        let actual = eg_domain::Sha256Hex::of(&bytes).to_hex();
        if &actual != expected {
            return Err(format!(
                "gold 文件 sha256 不匹配：{name} expected={expected} actual={actual}"
            ));
        }
        gold_files.insert(name.clone(), actual);
    }
    let objective: Dataset<ObjectiveItem> = read_json(&options.gold.join("objective-500.json"))?;
    let solution: Dataset<SolutionItem> = read_json(&options.gold.join("solution-200.json"))?;
    let mut objective_items = objective.items;
    let mut solution_items = solution.items;
    if let Some(limit) = options.max_objective {
        objective_items.truncate(limit);
    }
    if let Some(limit) = options.max_solution {
        solution_items.truncate(limit);
    }
    let mut report = EvalReport {
        gold_files,
        ..EvalReport::default()
    };
    let mut failures = Vec::new();
    evaluate_objective(&objective_items, &mut report.objective, &mut failures);
    evaluate_solution(&solution_items, &mut report.solution, &mut failures);
    report.total_items = objective_items.len() + solution_items.len();
    report.failure_classes = failures.iter().fold(BTreeMap::new(), |mut map, failure| {
        *map.entry(failure.dimension.clone()).or_insert(0) += 1;
        map
    });
    // 保留每个数据集前 40 条样本，避免 JSON 膨胀。
    report.objective.failures.truncate(40);
    report.solution.failures.truncate(40);
    let s2 = Stats::ratio(
        report.objective.stats.score_ok,
        report.objective.stats.score_total,
    );
    let s3_exact = Stats::ratio(
        report.solution.stats.score_ok,
        report.solution.stats.score_total,
    );
    let s3_half = Stats::ratio(
        report.solution.stats.half_ok,
        report.solution.stats.score_total,
    );
    let s5 = Stats::ratio(
        report.objective.stats.route_ok + report.solution.stats.route_ok,
        report.objective.stats.total + report.solution.stats.total,
    );
    report
        .targets_met
        .insert("S2_objective_accuracy".to_owned(), s2 >= 1.0);
    report
        .targets_met
        .insert("S3_score_exact".to_owned(), s3_exact >= 0.85);
    report
        .targets_met
        .insert("S3_within_half".to_owned(), s3_half >= 0.97);
    // ECE 用 gold 的「规则推导置信度标签」计算，不是模型实测置信度（README §0），
    // 因此只报告数值、不作为模型校准目标判定；真实 ECE 需真实模型输出。
    report.targets_met.insert("S3_ece".to_owned(), false);
    report.warnings.push(format!(
        "S3 ECE={:.4}（阈值 0.08）：以 gold 规则推导置信度标签计算，非模型实测，不能据此判定真实校准；标记为未达成/不可判定",
        report.solution.ece
    ));
    report
        .targets_met
        .insert("S5_route_accuracy".to_owned(), s5 >= 0.98);
    report.warnings.push(
        "合成金标：expected 由规范规则推导，不是模型标注；报告的是实现与规则推导的一致性，不代表真实试卷/模型精度"
            .to_owned(),
    );
    if report.objective.stats.route_ok < report.objective.stats.total {
        report.warnings.push(format!(
            "objective 路由规则命中 {}/{}（差异见 failures.route_mismatch）",
            report.objective.stats.route_ok, report.objective.stats.total
        ));
    }
    if report.solution.stats.tier_total == 0 {
        report.warnings.push(
            "缺档/非法档位样本：gold 未覆盖（0 条），无法评估该路径；由 eg-grade aggregate 单测覆盖"
                .to_owned(),
        );
    } else {
        report.warnings.push(format!(
            "partial_choice 档位命中 {}/{}（含 3 选 2、2 选 1、错选归零）",
            report.solution.stats.tier_ok + report.objective.stats.tier_ok,
            report.solution.stats.tier_total + report.objective.stats.tier_total
        ));
    }
    report.warnings.push(format!(
        "manual 分流正确 {}/{}；rubric_only 5 条聚合见 by_perturbation.rubric_only",
        report.objective.stats.manual_ok + report.solution.stats.manual_ok,
        report.objective.stats.manual_total + report.solution.stats.manual_total
    ));
    report.duration_ms = started.elapsed().as_millis();
    let _ = &mut failures;
    Ok(report)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("读取 {} 失败：{error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("解析 {} 失败：{error}", path.display()))
}

fn evaluate_objective(
    items: &[ObjectiveItem],
    report: &mut DatasetReport,
    failures: &mut Vec<FailureSample>,
) {
    let subject = MathSubjectPack::new();
    let config = GradeConfig::default();
    let mut confidence_pairs = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let question = question_of(
            index,
            &item.qid,
            item.kind,
            item.max_score_milli,
            &item.answer_key,
            item.rubric.as_ref(),
        );
        let confidence = confidence_of(item.recognition.confidence);
        let normalized = subject.normalize(&item.raw_answer).ok();
        let answer_empty = item.raw_answer.text.trim().is_empty()
            && item
                .raw_answer
                .latex
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty()
            && item.raw_answer.blocks.is_empty();
        let route_input = RouteInput {
            kind: item.kind,
            kind_known: true,
            key: item.answer_key.clone(),
            rubric_confirmed: item.rubric.is_some(),
            answer_empty,
            recognition_confidence: confidence,
            recognition_threshold: config.recognition_threshold,
            normalization_failed: normalized.is_none(),
            backend_available: true,
            key_kind_conflict: false,
            manual_only: false,
        };
        let route_outcome = route(&route_input);
        let mut outcome = Outcome {
            route_ok: route_outcome.rule_id == item.expected.rule_trace,
            route_label_ok: route_label(&route_outcome.decision) == item.expected.route,
            ..Outcome::default()
        };
        let actual_route = route_label(&route_outcome.decision).to_owned();
        if !outcome.route_ok {
            failures.push(failure(
                item,
                "route_mismatch",
                &item.expected.rule_trace,
                route_outcome.rule_id,
                "implementation_bug_suspect",
                "implementation_bug_suspect",
                format!(
                    "expected route={} rule={}",
                    item.expected.route, item.expected.rule_trace
                ),
            ));
        }
        if !outcome.route_label_ok {
            failures.push(failure(
                item,
                "route_label_mismatch",
                &item.expected.route,
                &actual_route,
                &item.expected.rule_trace,
                "expected_vs_implementation",
                String::new(),
            ));
        }
        let expected_manual = item.expected.route == "manual";
        if expected_manual {
            outcome.manual = true;
            outcome.status_ok = item.expected.status == "manual";
            let manual_ok = route_outcome.decision
                == RouteDecision::Manual(manual_reason(item.expected.manual_reason.as_deref()));
            outcome.reason = Some(manual_ok);
            outcome.manual_total = 1;
            outcome.manual_ok =
                outcome.route_ok && outcome.route_label_ok && outcome.status_ok && manual_ok;
            let mut effective_status = "manual";
            if !matches!(route_outcome.decision, RouteDecision::Manual(_)) {
                effective_status = "auto_or_review";
            }
            if !outcome.status_ok || !manual_ok {
                failures.push(failure(
                    item,
                    "manual_mismatch",
                    format!(
                        "status={} reason={:?}",
                        item.expected.status, item.expected.manual_reason
                    ),
                    format!("status={effective_status} rule={}", route_outcome.rule_id),
                    &item.expected.rule_trace,
                    "implementation_bug_suspect",
                    String::new(),
                ));
            }
        } else {
            let score = match item.expected.score_milli {
                Some(value) => milli(value),
                None => milli(0),
            };
            let (actual_score, actual_judgement, eval_matched) = evaluate_objective_score(
                &subject,
                &question,
                &item.raw_answer,
                normalized.as_ref(),
                &item.answer_key,
                item.rubric.as_ref(),
                route_outcome.rule_id,
            );
            if let Some(normalized_value) = normalized.as_ref() {
                if item.expected.normalized.get("kind").and_then(Value::as_str)
                    != Some("not_asserted")
                {
                    let ok = normalized_assertion(
                        &item.expected.normalized,
                        normalized_value,
                        eval_matched,
                    );
                    outcome.normalized = Some(ok);
                    if !ok {
                        failures.push(failure(
                            item,
                            "normalization_mismatch",
                            item.expected.normalized.to_string(),
                            format!(
                                "text={:?} latex={:?}",
                                normalized_value.text, normalized_value.latex
                            ),
                            &item.expected.rule_trace,
                            "implementation_bug_suspect",
                            String::new(),
                        ));
                    }
                }
            } else if item.expected.normalized.get("kind").and_then(Value::as_str) == Some("empty")
            {
                outcome.normalized = Some(true);
            } else {
                failures.push(failure(
                    item,
                    "normalization_missing",
                    "归一化成功",
                    "NormalizeError",
                    &item.expected.rule_trace,
                    "data_missing",
                    "gold raw_answer 无法被学科包归一化".to_owned(),
                ));
            }
            outcome.judgement =
                Some(actual_judgement == item.expected.judgement.as_deref().unwrap_or(""));
            outcome.score = Some(actual_score == score);
            let expected_max = milli(item.max_score_milli);
            outcome.half = Some(
                actual_score.as_milli().abs_diff(score.as_milli())
                    <= u64::try_from(expected_max.as_milli() / 2).unwrap_or(0),
            );
            if actual_score == MilliScore::ZERO && score != MilliScore::ZERO {
                outcome.false_zero = true;
                failures.push(failure(
                    item,
                    "false_zero",
                    format!("{}", score.as_milli()),
                    "0",
                    &item.expected.rule_trace,
                    "implementation_bug_suspect",
                    "不应给 0 却给 0".to_owned(),
                ));
            }
            if actual_judgement != item.expected.judgement.as_deref().unwrap_or("") {
                failures.push(failure(
                    item,
                    "judgement_mismatch",
                    item.expected.judgement.as_deref().unwrap_or(""),
                    &actual_judgement,
                    &item.expected.rule_trace,
                    "implementation_bug_suspect",
                    String::new(),
                ));
            }
            if actual_score != score {
                failures.push(failure(
                    item,
                    "score_mismatch",
                    score.as_milli().to_string(),
                    actual_score.as_milli().to_string(),
                    &item.expected.rule_trace,
                    "implementation_bug_suspect",
                    String::new(),
                ));
            }
            if let Some(expected_tier) = item.expected.partial_choice_tier {
                let actual_tier = partial_tier(item, &item.raw_answer);
                outcome.tier = Some(actual_tier == Some(expected_tier));
                if outcome.tier == Some(false) {
                    failures.push(failure(
                        item,
                        "partial_choice_tier_mismatch",
                        expected_tier.to_string(),
                        format!("{actual_tier:?}"),
                        &item.expected.rule_trace,
                        "implementation_bug_suspect",
                        format!("raw={:?}", item.raw_answer.text),
                    ));
                }
            }
            let threshold = threshold_for(&item.expected.route, &config);
            let (status, reason) = computed_status(
                item.expected.route == "manual",
                confidence,
                threshold,
                None,
                false,
            );
            outcome.status_ok = status == item.expected.status;
            outcome.reason = Some(reason.as_deref() == item.expected.review_reason.as_deref());
            outcome.auto = status == "auto_accept";
            outcome.review = status == "needs_review";
            if !outcome.status_ok {
                failures.push(failure(
                    item,
                    "status_mismatch",
                    &item.expected.status,
                    status,
                    &item.expected.rule_trace,
                    "implementation_bug_suspect",
                    format!("confidence={:.2}", item.expected.confidence),
                ));
            }
            if outcome.reason == Some(false) {
                failures.push(failure(
                    item,
                    "review_reason_mismatch",
                    format!("{:?}", item.expected.review_reason),
                    format!("{reason:?}"),
                    &item.expected.rule_trace,
                    "implementation_bug_suspect",
                    String::new(),
                ));
            }
            confidence_pairs.push((item.expected.confidence, actual_score == score));
        }
        record_groups(
            &mut report.stats,
            &outcome,
            item.kind,
            &item.perturbations,
            &item.expected.status,
        );
        report_stats_groups(
            &mut report.by_kind,
            &mut report.by_perturbation,
            &mut report.by_status,
            &outcome,
            item.kind,
            &item.perturbations,
            &item.expected.status,
        );
        if !outcome.status_ok {
            // 记录见上；分组计数已包含。
        }
    }
    report.ece = ece(&confidence_pairs);
}

#[allow(
    clippy::too_many_arguments,
    reason = "分组统计需要 kind/扰动/状态三组键与 outcome"
)]
fn report_stats_groups(
    by_kind: &mut BTreeMap<String, Stats>,
    by_perturbation: &mut BTreeMap<String, Stats>,
    by_status: &mut BTreeMap<String, Stats>,
    outcome: &Outcome,
    kind: QuestionKind,
    perturbations: &[String],
    status: &str,
) {
    by_kind
        .entry(kind_name(kind).to_owned())
        .or_default()
        .record(outcome);
    for perturbation in perturbations {
        by_perturbation
            .entry(perturbation.clone())
            .or_default()
            .record(outcome);
    }
    by_status
        .entry(status.to_owned())
        .or_default()
        .record(outcome);
}

fn record_groups(
    stats: &mut Stats,
    outcome: &Outcome,
    _kind: QuestionKind,
    _perturbations: &[String],
    _status: &str,
) {
    stats.record(outcome);
}

fn evaluate_solution(
    items: &[SolutionItem],
    report: &mut DatasetReport,
    failures: &mut Vec<FailureSample>,
) {
    let config = GradeConfig::default();
    let mut confidence_pairs = Vec::new();
    for item in items {
        let confidence = confidence_of(item.recognition.confidence);
        let answer_empty = item.raw_answer.text.trim().is_empty()
            && item
                .raw_answer
                .latex
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty()
            && item.raw_answer.blocks.is_empty();
        let route_input = RouteInput {
            kind: item.kind,
            kind_known: true,
            key: item.answer_key.clone(),
            rubric_confirmed: item.rubric.is_some(),
            answer_empty,
            recognition_confidence: confidence,
            recognition_threshold: config.recognition_threshold,
            // 解答题走 rubric/manual：评分标准不依赖数学归一化；expected 按规则推导不引入 R1。
            normalization_failed: false,
            backend_available: true,
            key_kind_conflict: false,
            manual_only: false,
        };
        let route_outcome = route(&route_input);
        let mut outcome = Outcome {
            route_ok: route_outcome.rule_id == item.expected.rule_trace,
            route_label_ok: route_label(&route_outcome.decision) == item.expected.route,
            ..Outcome::default()
        };
        if !outcome.route_ok {
            failures.push(solution_failure(
                item,
                "route_mismatch",
                &item.expected.rule_trace,
                route_outcome.rule_id,
                "implementation_bug_suspect",
                String::new(),
            ));
        }
        if !outcome.route_label_ok {
            failures.push(solution_failure(
                item,
                "route_label_mismatch",
                &item.expected.route,
                route_label(&route_outcome.decision),
                "expected_vs_implementation",
                String::new(),
            ));
        }
        if item.expected.route == "manual" {
            outcome.manual = true;
            outcome.status_ok = item.expected.status == "manual";
            let manual_ok = route_outcome.decision
                == RouteDecision::Manual(manual_reason(item.expected.manual_reason.as_deref()));
            outcome.reason = Some(manual_ok);
            outcome.manual_total = 1;
            outcome.manual_ok =
                outcome.route_ok && outcome.route_label_ok && outcome.status_ok && manual_ok;
            if !outcome.status_ok || !manual_ok {
                failures.push(solution_failure(
                    item,
                    "manual_mismatch",
                    format!(
                        "{:?}/{:?}",
                        item.expected.status, item.expected.manual_reason
                    ),
                    format!(
                        "{:?} rule={}",
                        route_outcome.decision, route_outcome.rule_id
                    ),
                    "implementation_bug_suspect",
                    String::new(),
                ));
            }
        } else {
            let Some(rubric) = item.rubric.as_ref() else {
                failures.push(solution_failure(
                    item,
                    "data_missing",
                    "rubric",
                    "None",
                    "data_missing",
                    "rubric_itemized 条目缺少 rubric".to_owned(),
                ));
                record_groups(
                    &mut report.stats,
                    &outcome,
                    item.kind,
                    &item.perturbations,
                    &item.expected.status,
                );
                report_stats_groups(
                    &mut report.by_kind,
                    &mut report.by_perturbation,
                    &mut report.by_status,
                    &outcome,
                    item.kind,
                    &item.perturbations,
                    &item.expected.status,
                );
                continue;
            };
            let matched: Vec<RubricItemId> = item
                .expected
                .steps
                .iter()
                .filter(|step| step.matched)
                .filter_map(|step| {
                    step.item_id
                        .parse::<Ulid>()
                        .ok()
                        .map(RubricItemId::from_ulid)
                })
                .collect();
            match aggregate(rubric, &matched) {
                Ok(aggregated) => {
                    let expected_score = item.expected.score_milli.unwrap_or(0);
                    let actual_score = aggregated.score.as_milli();
                    outcome.score = Some(actual_score == expected_score);
                    outcome.half = Some(
                        actual_score.abs_diff(expected_score)
                            <= u64::try_from(item.max_score_milli / 2).unwrap_or(0),
                    );
                    outcome.judgement = Some(
                        judgement_for(actual_score, item.max_score_milli)
                            == item.expected.judgement.as_deref().unwrap_or(""),
                    );
                    if actual_score == 0 && expected_score != 0 {
                        outcome.false_zero = true;
                        failures.push(solution_failure(
                            item,
                            "false_zero",
                            expected_score.to_string(),
                            "0",
                            "implementation_bug_suspect",
                            "不应给 0 却给 0".to_owned(),
                        ));
                    }
                    if actual_score != expected_score {
                        failures.push(solution_failure(
                            item,
                            "score_mismatch",
                            expected_score.to_string(),
                            actual_score.to_string(),
                            "implementation_bug_suspect",
                            String::new(),
                        ));
                    }
                    if outcome.judgement == Some(false) {
                        failures.push(solution_failure(
                            item,
                            "judgement_mismatch",
                            item.expected.judgement.clone().unwrap_or_default(),
                            judgement_for(actual_score, item.max_score_milli),
                            "implementation_bug_suspect",
                            String::new(),
                        ));
                    }
                    let effective: BTreeSet<RubricItemId> =
                        aggregated.satisfied.iter().copied().collect();
                    for step in &item.expected.steps {
                        outcome.step_total += 1;
                        let ok = step
                            .item_id
                            .parse::<Ulid>()
                            .ok()
                            .map(RubricItemId::from_ulid)
                            .is_some_and(|id| effective.contains(&id) == step.effective);
                        if ok {
                            outcome.step_ok += 1;
                        } else {
                            failures.push(solution_failure(
                                item,
                                "step_effect_mismatch",
                                format!("{} effective={}", step.item_id, step.effective),
                                format!("effective_set={effective:?}"),
                                "implementation_bug_suspect",
                                String::new(),
                            ));
                        }
                    }
                    let calibration_ok = item
                        .judge
                        .as_ref()
                        .and_then(|judge| judge.calibration_id.as_deref())
                        == item.expected.calibration_id.as_deref();
                    outcome.calibration = Some(calibration_ok);
                    if !calibration_ok {
                        failures.push(solution_failure(
                            item,
                            "calibration_mismatch",
                            format!("{:?}", item.expected.calibration_id),
                            format!(
                                "{:?}",
                                item.judge
                                    .as_ref()
                                    .and_then(|judge| judge.calibration_id.clone())
                            ),
                            "data_missing",
                            String::new(),
                        ));
                    }
                    let threshold = threshold_for(&item.expected.route, &config);
                    let calibration = item
                        .judge
                        .as_ref()
                        .and_then(|judge| judge.calibration_id.as_deref());
                    let (status, reason) = computed_status(
                        false,
                        confidence_of(item.expected.confidence),
                        threshold,
                        calibration,
                        true,
                    );
                    outcome.status_ok = status == item.expected.status;
                    outcome.reason =
                        Some(reason.as_deref() == item.expected.review_reason.as_deref());
                    outcome.auto = status == "auto_accept";
                    outcome.review = status == "needs_review";
                    if !outcome.status_ok {
                        failures.push(solution_failure(
                            item,
                            "status_mismatch",
                            &item.expected.status,
                            status,
                            "implementation_bug_suspect",
                            format!("confidence={:.2}", item.expected.confidence),
                        ));
                        confidence_pairs
                            .push((item.expected.confidence, actual_score == expected_score));
                    } else {
                        confidence_pairs
                            .push((item.expected.confidence, actual_score == expected_score));
                    }
                    if outcome.reason == Some(false) {
                        failures.push(solution_failure(
                            item,
                            "review_reason_mismatch",
                            format!("{:?}", item.expected.review_reason),
                            format!("{reason:?}"),
                            "implementation_bug_suspect",
                            String::new(),
                        ));
                    }
                }
                Err(error) => {
                    failures.push(solution_failure(
                        item,
                        "aggregate_error",
                        "聚合成功",
                        error.to_string(),
                        "implementation_bug_suspect",
                        String::new(),
                    ));
                }
            }
        }
        record_groups(
            &mut report.stats,
            &outcome,
            item.kind,
            &item.perturbations,
            &item.expected.status,
        );
        report_stats_groups(
            &mut report.by_kind,
            &mut report.by_perturbation,
            &mut report.by_status,
            &outcome,
            item.kind,
            &item.perturbations,
            &item.expected.status,
        );
    }
    report.ece = ece(&confidence_pairs);
}

fn evaluate_objective_score(
    subject: &MathSubjectPack,
    question: &Question,
    raw: &RawAnswer,
    normalized: Option<&eg_domain::NormalizedAnswer>,
    key: &AnswerKey,
    rubric: Option<&Rubric>,
    rule_id: &str,
) -> (MilliScore, String, Option<bool>) {
    let max = question.max_score;
    if rule_id == "R0" {
        return (MilliScore::ZERO, "mismatch".to_owned(), Some(false));
    }
    if let Some(rubric) = rubric
        && rubric.mode == eg_domain::ScoringMode::PartialChoice
        && let (AnswerKey::Concrete(concrete), Some(rule)) = (key, rubric.partial_choice.as_ref())
    {
        let correct = match &concrete.primary {
            AnswerValue::Options(options) => options.clone(),
            _ => Vec::new(),
        };
        let selected = parse_option_letters(&raw.text).unwrap_or_default();
        if let Ok(outcome) = partial_choice(rule, &correct, &selected) {
            let judgement = if outcome.score == max {
                "match"
            } else if outcome.score == MilliScore::ZERO {
                "mismatch"
            } else {
                "partial"
            };
            return (
                outcome.score,
                judgement.to_owned(),
                Some(!outcome.wrong_selection),
            );
        }
    }
    if let (Some(normalized), AnswerKey::Concrete(concrete)) = (normalized, key) {
        {
            let ids = eg_grade::ports::SequentialIds::new();
            if let Ok(run) = evaluate_rule_chain(subject, question, concrete, normalized, &ids) {
                return match run.evaluation {
                    eg_grade::RuleEvaluation::Match { .. } => (max, "match".to_owned(), Some(true)),
                    eg_grade::RuleEvaluation::Mismatch { .. } => {
                        (MilliScore::ZERO, "mismatch".to_owned(), Some(false))
                    }
                    eg_grade::RuleEvaluation::Undecidable { .. } => {
                        (MilliScore::ZERO, "ungradable".to_owned(), None)
                    }
                };
            }
        }
    }
    (MilliScore::ZERO, "ungradable".to_owned(), None)
}

fn partial_tier(item: &ObjectiveItem, raw: &RawAnswer) -> Option<u32> {
    let rubric = item.rubric.as_ref()?;
    let rule = rubric.partial_choice.as_ref()?;
    let correct = match &item.answer_key {
        AnswerKey::Concrete(concrete) => match &concrete.primary {
            AnswerValue::Options(options) => options.clone(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    let selected = parse_option_letters(&raw.text)?;
    partial_choice(rule, &correct, &selected)
        .ok()?
        .tier_selected_correct
}

fn normalized_assertion(
    expected: &Value,
    normalized: &eg_domain::NormalizedAnswer,
    eval: Option<bool>,
) -> bool {
    match expected
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "empty" => normalized.text.trim().is_empty(),
        "boolean" => expected
            .get("text")
            .and_then(Value::as_str)
            .is_some_and(|text| normalized.text.trim().eq_ignore_ascii_case(text)),
        "options" => {
            let expected: Vec<String> = expected
                .get("value")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            let actual: String = normalized
                .text
                .chars()
                .filter(char::is_ascii_uppercase)
                .collect();
            expected
                .iter()
                .map(|option| option.to_ascii_uppercase())
                .collect::<BTreeSet<_>>()
                == actual
                    .chars()
                    .map(|ch| ch.to_string())
                    .collect::<BTreeSet<_>>()
        }
        "number" => expected
            .get("value")
            .and_then(Value::as_str)
            .and_then(|value| value.parse::<f64>().ok())
            .is_some_and(|value| {
                normalized
                    .text
                    .trim()
                    .parse::<f64>()
                    .is_ok_and(|actual| (actual - value).abs() <= 1e-9)
            }),
        "text" => expected
            .get("value")
            .and_then(Value::as_str)
            .is_some_and(|value| normalized.text.contains(value)),
        "expression" => eval.is_some(),
        _ => true,
    }
}

fn computed_status(
    manual: bool,
    confidence: Confidence,
    threshold: Option<Confidence>,
    calibration_id: Option<&str>,
    require_calibration: bool,
) -> (&'static str, Option<String>) {
    if manual {
        return ("manual", Some("ungradable".to_owned()));
    }
    let Some(threshold) = threshold else {
        return ("needs_review", Some("ungradable".to_owned()));
    };
    if require_calibration && calibration_id.is_none() {
        return ("needs_review", Some("uncalibrated_model".to_owned()));
    }
    if confidence.is_at_least(threshold) {
        ("auto_accept", None)
    } else {
        ("needs_review", Some("low_confidence".to_owned()))
    }
}

fn threshold_for(route: &str, config: &GradeConfig) -> Option<Confidence> {
    match route {
        "rule_only" => Some(config.standard.rule),
        "rule_then_model" => Some(config.standard.rule_then_model),
        "rubric_itemized" => Some(config.standard.rubric),
        _ => None,
    }
}

fn judgment_for(score: i64, max: i64) -> &'static str {
    if score >= max {
        "match"
    } else if score <= 0 {
        "mismatch"
    } else {
        "partial"
    }
}

fn judgement_for(score: i64, max: i64) -> String {
    judgment_for(score, max).to_owned()
}

fn kind_name(kind: QuestionKind) -> &'static str {
    match kind {
        QuestionKind::SingleChoice => "single_choice",
        QuestionKind::MultipleChoice => "multiple_choice",
        QuestionKind::TrueFalse => "true_false",
        QuestionKind::FillInBlank => "fill_in_blank",
        QuestionKind::NumericOnly => "numeric_only",
        QuestionKind::Calculation => "calculation",
        QuestionKind::Proof => "proof",
        QuestionKind::ShortAnswer => "short_answer",
        QuestionKind::Essay => "essay",
        QuestionKind::Other => "other",
    }
}

fn route_label(decision: &RouteDecision) -> &'static str {
    match decision {
        RouteDecision::RuleOnly => "rule_only",
        RouteDecision::RuleThenModel => "rule_then_model",
        RouteDecision::ModelDirect => "model_direct",
        RouteDecision::RubricItemized => "rubric_itemized",
        RouteDecision::Manual(_) => "manual",
    }
}

fn manual_reason(reason: Option<&str>) -> eg_domain::ManualReason {
    match reason {
        Some("recognition_unavailable") => eg_domain::ManualReason::RecognitionUnavailable,
        Some("answer_key_missing") => eg_domain::ManualReason::AnswerKeyMissing,
        Some("answer_key_omitted") => eg_domain::ManualReason::AnswerKeyOmitted,
        Some("rubric_missing") => eg_domain::ManualReason::RubricMissing,
        Some("backend_unavailable") => eg_domain::ManualReason::BackendUnavailable,
        Some("out_of_scope") => eg_domain::ManualReason::OutOfScope,
        _ => eg_domain::ManualReason::OutOfScope,
    }
}

fn question_of(
    index: usize,
    qid: &str,
    kind: QuestionKind,
    max_score_milli: i64,
    key: &AnswerKey,
    rubric: Option<&Rubric>,
) -> Question {
    let _ = qid;
    Question {
        id: QuestionId::from_ulid(Ulid::from_u128(
            900_000 + u128::try_from(index).unwrap_or(0),
        )),
        paper_id: PaperId::from_ulid(Ulid::from_u128(1)),
        ordinal: u32::try_from(index + 1).unwrap_or(u32::MAX),
        label: (index + 1).to_string(),
        kind,
        max_score: milli(max_score_milli),
        stem: None,
        key: key.clone(),
        rubric: rubric.cloned(),
        subject_hint: Some(SubjectId::math()),
        knowledge_tags: Vec::new(),
    }
}

fn confidence_of(value: f64) -> Confidence {
    Confidence::new(value.clamp(0.0, 1.0)).unwrap_or(Confidence::ZERO)
}

fn milli(value: i64) -> MilliScore {
    MilliScore::from_milli(value).unwrap_or(MilliScore::ZERO)
}

fn ece(pairs: &[(f64, bool)]) -> f64 {
    if pairs.is_empty() {
        return 0.0;
    }
    let mut bins = [(0u64, 0u64, 0.0f64); 10];
    for (confidence, correct) in pairs {
        let index = ((confidence.clamp(0.0, 0.999_999) * 10.0) as usize).min(9);
        let bin = &mut bins[index];
        bin.0 += 1;
        if *correct {
            bin.1 += 1;
        }
        bin.2 += confidence;
    }
    let total = f64::from(u32::try_from(pairs.len()).unwrap_or(u32::MAX));
    bins.iter()
        .filter(|(count, _, _)| *count > 0)
        .map(|(count, correct, sum_confidence)| {
            let n = f64::from(u32::try_from(*count).unwrap_or(u32::MAX));
            let accuracy = f64::from(u32::try_from(*correct).unwrap_or(u32::MAX)) / n;
            let avg_confidence = *sum_confidence / n;
            n / total * (accuracy - avg_confidence).abs()
        })
        .sum()
}

#[allow(clippy::needless_pass_by_value, reason = "失败样本按值拼接可读文本")]
fn failure(
    item: &ObjectiveItem,
    dimension: &str,
    expected: impl ToString,
    actual: impl ToString,
    _rule: &str,
    origin: &str,
    detail: String,
) -> FailureSample {
    FailureSample {
        dataset: "objective".to_owned(),
        qid: item.qid.clone(),
        kind: kind_name(item.kind).to_owned(),
        perturbation: item.perturbations.first().cloned().unwrap_or_default(),
        dimension: dimension.to_owned(),
        expected: expected.to_string(),
        actual: actual.to_string(),
        origin: origin.to_owned(),
        detail,
    }
}

#[allow(clippy::needless_pass_by_value, reason = "失败样本按值拼接可读文本")]
fn solution_failure(
    item: &SolutionItem,
    dimension: &str,
    expected: impl ToString,
    actual: impl ToString,
    origin: &str,
    detail: String,
) -> FailureSample {
    FailureSample {
        dataset: "solution".to_owned(),
        qid: item.qid.clone(),
        kind: kind_name(item.kind).to_owned(),
        perturbation: item.perturbations.first().cloned().unwrap_or_default(),
        dimension: dimension.to_owned(),
        expected: expected.to_string(),
        actual: actual.to_string(),
        origin: origin.to_owned(),
        detail,
    }
}
