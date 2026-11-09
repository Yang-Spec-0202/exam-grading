//! gold 抽检冒烟：把 `fixtures/gold/objective-500.json` 的规则路径条目喂给
//! `eg-subject-math`（归一化 + 比较器链）与 `eg-grade`（多选档位、rubric 聚合），
//! 验证金标 `expected` 与实现一致；不一致打印 `[DIFF]` 并以非零退出。
//!
//! 由 `scripts/fixtures/smoke_gold.sh` 在 /tmp 临时 cargo 工程中构建运行，
//! 不写入仓库 target/Cargo.lock。

use std::path::PathBuf;

use eg_domain::{AnswerKey, AnswerValue, QuestionKind, RawAnswer, Rubric, SubjectPack};
use eg_grade::aggregate::{aggregate, parse_option_letters, partial_choice};
use eg_subject_math::{MathSubjectPack, comparators::comparator_chain_for};
use serde_json::Value;

fn main() {
    let repo = PathBuf::from(std::env::args().nth(1).expect("用法：smoke_gold <repo>"));
    let pack = MathSubjectPack::without_cas();
    let objective: Value = read_json(&repo.join("fixtures/gold/objective-500.json"));
    let solution: Value = read_json(&repo.join("fixtures/gold/solution-200.json"));

    let mut checked = 0usize;
    let mut skipped_manual = 0usize;
    let mut skipped_empty = 0usize;
    let mut problems: Vec<String> = Vec::new();

    for item in objective["items"].as_array().expect("items") {
        let qid = item["qid"].as_str().unwrap_or("?");
        let expected = &item["expected"];
        let route = expected["route"].as_str().unwrap_or("");
        if route == "manual" {
            skipped_manual += 1;
            continue;
        }
        let kind: QuestionKind = serde_json::from_value(item["kind"].clone()).expect("kind");
        let answer_form = item["answer_form"].as_str();
        let raw: RawAnswer = serde_json::from_value(item["raw_answer"].clone()).expect("raw");
        let normalized = match pack.normalize(&raw) {
            Ok(answer) => answer,
            Err(error) => {
                if expected["normalized"]["kind"].as_str() == Some("empty") {
                    skipped_empty += 1;
                    continue;
                }
                problems.push(format!("{qid}: 归一化失败 {error}"));
                continue;
            }
        };
        match expected["normalized"]["kind"].as_str().unwrap_or("") {
            "options" => {
                let expected_text = expected["normalized"]["text"].as_str().unwrap_or("");
                if normalized.text != expected_text {
                    problems.push(format!(
                        "{qid}: options 归一化 {:?} != {expected_text:?}",
                        normalized.text
                    ));
                }
            }
            "boolean" => {
                let expected_text = expected["normalized"]["text"].as_str().unwrap_or("");
                if normalized.text != expected_text {
                    problems.push(format!(
                        "{qid}: boolean 归一化 {:?} != {expected_text:?}",
                        normalized.text
                    ));
                }
            }
            "number" => {
                if answer_form != Some("number") {
                    problems.push(format!("{qid}: answer_form 应为 number"));
                }
                let expected_value = expected["normalized"]["value"].as_str().unwrap_or("");
                match normalized.value.as_ref() {
                    Some(AnswerValue::Number { value, .. }) => {
                        if value.to_string() != expected_value {
                            problems.push(format!("{qid}: number {value} != {expected_value}"));
                        }
                    }
                    other => problems.push(format!("{qid}: number 归一化值缺失 {other:?}")),
                }
            }
            "text" => {
                if answer_form != Some("text") {
                    problems.push(format!("{qid}: answer_form 应为 text"));
                }
                let expected_text = expected["normalized"]["text"].as_str().unwrap_or("");
                if normalized.text != expected_text {
                    problems.push(format!(
                        "{qid}: text 归一化 {:?} != {expected_text:?}",
                        normalized.text
                    ));
                }
            }
            "expression" => {
                if answer_form != Some("expression") {
                    problems.push(format!("{qid}: answer_form 应为 expression"));
                }
            }
            other => problems.push(format!("{qid}: 未知 normalized kind {other}")),
        }

        let key: AnswerKey = serde_json::from_value(item["answer_key"].clone()).expect("key");
        let AnswerKey::Concrete(concrete) = key else {
            problems.push(format!("{qid}: 规则路径必须有具体答案键"));
            continue;
        };
        let rubric_json = &item["rubric"];
        if !rubric_json.is_null() && rubric_json["mode"] == "partial_choice" {
            let rubric: Rubric = serde_json::from_value(rubric_json.clone()).expect("rubric");
            let rule = rubric.partial_choice.as_ref().expect("partial rule");
            let AnswerValue::Options(correct) = concrete.primary.clone() else {
                problems.push(format!("{qid}: 多选答案键必须是 options"));
                continue;
            };
            let selected = parse_option_letters(&normalized.text).unwrap_or_default();
            match partial_choice(rule, &correct, &selected) {
                Ok(outcome) => {
                    let expected_score = expected["score_milli"].as_i64().unwrap_or(-1);
                    if outcome.score.as_milli() != expected_score {
                        problems.push(format!(
                            "{qid}: partial 分数 {} != {expected_score}",
                            outcome.score
                        ));
                    }
                }
                Err(error) => problems.push(format!("{qid}: partial 计算失败 {error}")),
            }
            checked += 1;
            continue;
        }

        let chain = comparator_chain_for(&concrete, kind);
        let outcome =
            eg_domain::contract::run_comparator_chain(&chain, &concrete.primary, &normalized);
        let verdict = match &outcome.verdict {
            eg_domain::ComparatorVerdict::Equal
            | eg_domain::ComparatorVerdict::Equivalent { .. } => "match",
            eg_domain::ComparatorVerdict::NotEqual { .. } => "mismatch",
            eg_domain::ComparatorVerdict::Undecidable { .. } => "undecidable",
        };
        let expected_judgement = expected["judgement"].as_str().unwrap_or("");
        if expected_judgement != verdict {
            problems.push(format!(
                "{qid}: {kind:?} 比较器判定 {verdict} != 期望 {expected_judgement}"
            ));
        }
        checked += 1;
    }

    let mut sol_checked = 0usize;
    for item in solution["items"].as_array().expect("items") {
        let qid = item["qid"].as_str().unwrap_or("?");
        let expected = &item["expected"];
        if expected["route"] == "manual" {
            continue;
        }
        let rubric_json = &item["rubric"];
        if rubric_json.is_null() {
            problems.push(format!("{qid}: 非 manual 条目缺少 rubric"));
            continue;
        }
        let rubric: Rubric = serde_json::from_value(rubric_json.clone()).expect("rubric");
        let matched_ids: Vec<eg_domain::RubricItemId> = expected["steps"]
            .as_array()
            .expect("steps")
            .iter()
            .filter(|step| step["matched"].as_bool() == Some(true))
            .map(|step| {
                serde_json::from_value(step["item_id"].clone()).expect("rubric item id")
            })
            .collect();
        match aggregate(&rubric, &matched_ids) {
            Ok(result) => {
                let expected_score = expected["score_milli"].as_i64().unwrap_or(-1);
                if result.score.as_milli() != expected_score {
                    problems.push(format!(
                        "{qid}: 聚合分数 {} != 期望 {expected_score}",
                        result.score
                    ));
                }
                let expected_effective: Vec<String> = expected["steps"]
                    .as_array()
                    .expect("steps")
                    .iter()
                    .filter(|step| step["effective"].as_bool() == Some(true))
                    .map(|step| step["item_id"].as_str().unwrap_or("").to_owned())
                    .collect();
                let actual_effective: Vec<String> = result
                    .satisfied
                    .iter()
                    .map(std::string::ToString::to_string)
                    .collect();
                if expected_effective != actual_effective {
                    problems.push(format!(
                        "{qid}: 生效评分项 {actual_effective:?} != 期望 {expected_effective:?}"
                    ));
                }
            }
            Err(error) => problems.push(format!("{qid}: 聚合失败 {error}")),
        }
        sol_checked += 1;
    }

    println!(
        "objective 抽检 {checked} 条（manual 跳过 {skipped_manual}、R0 空答案跳过 {skipped_empty}）、\
         solution 聚合抽检 {sol_checked} 条；不一致 {} 条",
        problems.len()
    );
    for problem in problems.iter().take(25) {
        println!("[DIFF] {problem}");
    }
    if !problems.is_empty() {
        std::process::exit(1);
    }
    println!("[ok] gold 与 eg-subject-math / eg-grade 抽检一致");
}

fn read_json(path: &std::path::Path) -> Value {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("读取 {path:?}：{error}"));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("解析 {path:?}：{error}"))
}
