//! 评分标准聚合（`SPEC-grade.md` §5.3）与多选部分给分（v0.2.2）。
//!
//! 只做确定性聚合，不调用模型；聚合结果与模型声明不一致由 `validate` 记为冲突。
//! 分步给分按依赖：某评分项得分当且仅当「满足」且「所有前置满足」。

use std::collections::BTreeSet;

use eg_domain::{MilliScore, PartialChoiceRule, Rubric, RubricItemId, ScoringMode};

/// 聚合错误（一律映射为不可判定/配置错误，绝不默认 0 分）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AggregateError {
    /// 评分项未知。
    #[error("评分项 {0} 不在评分标准中")]
    UnknownItem(RubricItemId),
    /// 分值溢出。
    #[error("分值聚合溢出")]
    Overflow,
    /// 分步给分依赖缺失（规则配置错误）。
    #[error("分步给分依赖缺失：{0}")]
    MissingDependency(RubricItemId),
    /// 部分给分模式不通过评分项聚合。
    #[error("部分给分模式由档位表聚合，不走评分项累加")]
    PartialChoiceUnsupported,
}

/// 聚合结果。
#[derive(Debug, Clone, PartialEq)]
pub struct AggregatedScore {
    /// 聚合得分。
    pub score: MilliScore,
    /// 实际生效的满足项（分步给分已按依赖裁剪）。
    pub satisfied: Vec<RubricItemId>,
}

/// 按评分标准聚合「满足项」得分。
///
/// # Errors
///
/// 未知评分项/溢出/依赖缺失返回 [`AggregateError`]。
pub fn aggregate(
    rubric: &Rubric,
    satisfied: &[RubricItemId],
) -> Result<AggregatedScore, AggregateError> {
    for item in satisfied {
        if !rubric
            .items
            .iter()
            .any(|rubric_item| rubric_item.id == *item)
        {
            return Err(AggregateError::UnknownItem(*item));
        }
    }
    let satisfied_set: BTreeSet<RubricItemId> = satisfied.iter().copied().collect();
    match rubric.mode {
        ScoringMode::Additive => aggregate_additive(rubric, &satisfied_set),
        ScoringMode::AllOrNothing => {
            let all = rubric
                .items
                .iter()
                .all(|item| satisfied_set.contains(&item.id));
            Ok(AggregatedScore {
                score: if all { rubric.total } else { MilliScore::ZERO },
                satisfied: if all {
                    rubric.items.iter().map(|item| item.id).collect()
                } else {
                    Vec::new()
                },
            })
        }
        ScoringMode::Stepwise => aggregate_stepwise(rubric, &satisfied_set),
        ScoringMode::PartialChoice => Err(AggregateError::PartialChoiceUnsupported),
    }
}

fn aggregate_additive(
    rubric: &Rubric,
    satisfied: &BTreeSet<RubricItemId>,
) -> Result<AggregatedScore, AggregateError> {
    let mut score = MilliScore::ZERO;
    let mut effective = Vec::new();
    for item in &rubric.items {
        if satisfied.contains(&item.id) {
            score = score
                .checked_add_score(item.points)
                .map_err(|_| AggregateError::Overflow)?;
            effective.push(item.id);
        }
    }
    Ok(AggregatedScore {
        score,
        satisfied: effective,
    })
}

fn aggregate_stepwise(
    rubric: &Rubric,
    satisfied: &BTreeSet<RubricItemId>,
) -> Result<AggregatedScore, AggregateError> {
    let mut effective: BTreeSet<RubricItemId> = BTreeSet::new();
    // 固定点：满足且所有前置已生效
    loop {
        let mut changed = false;
        for item in &rubric.items {
            if effective.contains(&item.id) || !satisfied.contains(&item.id) {
                continue;
            }
            let dependencies_ready = item.depends_on.iter().all(|dependency| {
                rubric
                    .items
                    .iter()
                    .find(|candidate| candidate.id == *dependency)
                    .is_some_and(|_| effective.contains(dependency))
            });
            if dependencies_ready {
                effective.insert(item.id);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let mut score = MilliScore::ZERO;
    for item in &rubric.items {
        if effective.contains(&item.id) {
            score = score
                .checked_add_score(item.points)
                .map_err(|_| AggregateError::Overflow)?;
        }
    }
    Ok(AggregatedScore {
        score,
        satisfied: effective.into_iter().collect(),
    })
}

/// 多选部分给分错误（缺档/非法 → 转人工，绝不默认 0 分）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PartialChoiceError {
    /// 规则档位非法/缺档。
    #[error("部分给分档位缺失或非法：{0}")]
    InvalidRule(String),
    /// 正确选项数与规则不一致。
    #[error("正确选项数 {actual} 与档位规则 {expected} 不一致")]
    OptionCountMismatch {
        /// 规则声明。
        expected: u32,
        /// 实际正确选项数。
        actual: u32,
    },
    /// 学生选项不可识别。
    #[error("学生选项不可识别：{0}")]
    UnrecognizedSelection(String),
    /// 分值聚合溢出。
    #[error("部分给分分值溢出")]
    Overflow,
}

/// 部分给分结果。
#[derive(Debug, Clone, PartialEq)]
pub struct PartialChoiceOutcome {
    /// 得分。
    pub score: MilliScore,
    /// 命中的正确选项（满足项）。
    pub satisfied: Vec<String>,
    /// 未选中的正确选项（缺失项）。
    pub missing: Vec<String>,
    /// 是否错选（错选归零）。
    pub wrong_selection: bool,
    /// 命中档位（`None` 表示满对/零分/错选）。
    pub tier_selected_correct: Option<u32>,
}

/// 计算多选部分给分（只读 `Rubric.partial_choice`）。
///
/// 规则（`SPEC-domain.md` §4 不变量 6–11）：
/// - 无错选时按「命中正确选项数」查档；命中数 = 正确选项数 → 满分；
/// - 存在错选 → 归零（恒为真，不可配置）；
/// - 缺档/档位非法/命中数越界 → 错误（转人工，绝不默认 0 分）。
///
/// # Errors
///
/// 档位规则非法、选项数不一致、选项不可识别返回 [`PartialChoiceError`]。
pub fn partial_choice(
    rule: &PartialChoiceRule,
    correct_options: &[String],
    selected_options: &[String],
) -> Result<PartialChoiceOutcome, PartialChoiceError> {
    if rule.correct_option_count != u32::try_from(correct_options.len()).unwrap_or(u32::MAX) {
        return Err(PartialChoiceError::OptionCountMismatch {
            expected: rule.correct_option_count,
            actual: u32::try_from(correct_options.len()).unwrap_or(u32::MAX),
        });
    }
    rule.validate(rule.total_milli, None)
        .map_err(|error| PartialChoiceError::InvalidRule(error.to_string()))?;
    let correct: BTreeSet<String> = correct_options.iter().cloned().collect();
    let selected: BTreeSet<String> = selected_options.iter().cloned().collect();
    let wrong_selection = selected.iter().any(|option| !correct.contains(option));
    let satisfied: Vec<String> = selected
        .iter()
        .filter(|option| correct.contains(*option))
        .cloned()
        .collect();
    let missing: Vec<String> = correct
        .iter()
        .filter(|option| !selected.contains(*option))
        .cloned()
        .collect();
    if wrong_selection {
        return Ok(PartialChoiceOutcome {
            score: MilliScore::ZERO,
            satisfied: Vec::new(),
            missing: correct.into_iter().collect(),
            wrong_selection: true,
            tier_selected_correct: None,
        });
    }
    let selected_correct = u32::try_from(satisfied.len()).unwrap_or(u32::MAX);
    if selected_correct > rule.correct_option_count {
        return Err(PartialChoiceError::InvalidRule(
            "命中正确选项数超过正确选项总数".to_owned(),
        ));
    }
    if selected_correct == rule.correct_option_count {
        return Ok(PartialChoiceOutcome {
            score: rule.total_milli,
            satisfied,
            missing,
            wrong_selection: false,
            tier_selected_correct: None,
        });
    }
    if selected_correct == 0 {
        return Ok(PartialChoiceOutcome {
            score: MilliScore::ZERO,
            satisfied,
            missing,
            wrong_selection: false,
            tier_selected_correct: None,
        });
    }
    let tier = rule
        .tiers
        .iter()
        .find(|tier| tier.selected_correct == selected_correct)
        .ok_or_else(|| {
            PartialChoiceError::InvalidRule(format!("缺少命中 {selected_correct} 项档位"))
        })?;
    Ok(PartialChoiceOutcome {
        score: tier.points,
        satisfied,
        missing,
        wrong_selection: false,
        tier_selected_correct: Some(selected_correct),
    })
}

/// 从归一化学科答案文本解析选项集合（规则路径用；归一化失败由路由转人工）。
///
/// 支持 `AB`、`A,B`、`A、B` 等写法；只接受 A–Z 单字母且去重排序。
#[must_use]
pub fn parse_option_letters(text: &str) -> Option<Vec<String>> {
    let mut letters: Vec<String> = Vec::new();
    for ch in text.chars() {
        if ch.is_ascii_alphabetic() {
            letters.push(ch.to_ascii_uppercase().to_string());
        } else if !matches!(ch, ' ' | ',' | '，' | '、' | '/' | ';' | '；') {
            return None;
        }
    }
    if letters.is_empty() || letters.len() > 26 {
        return None;
    }
    letters.sort();
    letters.dedup();
    Some(letters)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use eg_domain::{
        MilliScore, PartialChoiceRule, PartialChoiceSource, PartialChoiceTier, RubricItem,
        RubricItemId, RubricOrigin, Ulid,
    };

    fn make_rubric(mode: ScoringMode, points: &[i64]) -> Rubric {
        let items = points
            .iter()
            .enumerate()
            .map(|(index, points)| RubricItem {
                id: RubricItemId::from_ulid(Ulid::from_u128(u128::try_from(index).unwrap() + 1)),
                criterion: format!("项{index}"),
                points: MilliScore::from_milli(*points).unwrap(),
                required: true,
                depends_on: Vec::new(),
            })
            .collect::<Vec<_>>();
        let total = points.iter().sum::<i64>();
        Rubric {
            id: eg_domain::RubricId::from_ulid(Ulid::from_u128(99)),
            version: 1,
            items,
            mode,
            total: MilliScore::from_milli(total).unwrap(),
            origin: RubricOrigin::TeacherHandwritten {
                confirmed_by: eg_domain::LOCAL_TEACHER.to_owned(),
                confirmed_at: eg_domain::time::parse_rfc3339("2026-11-02T00:00:00Z").unwrap(),
            },
            partial_choice: None,
        }
    }

    #[test]
    fn additive_and_all_or_nothing() {
        let rubric = make_rubric(ScoringMode::Additive, &[1000, 2000]);
        let ids: Vec<RubricItemId> = rubric.items.iter().map(|item| item.id).collect();
        assert_eq!(aggregate(&rubric, &ids).unwrap().score.as_milli(), 3000);
        assert_eq!(
            aggregate(&rubric, &ids[..1]).unwrap().score.as_milli(),
            1000
        );
        let all = make_rubric(ScoringMode::AllOrNothing, &[3000]);
        let id = all.items[0].id;
        assert_eq!(aggregate(&all, &[id]).unwrap().score.as_milli(), 3000);
        assert_eq!(aggregate(&all, &[]).unwrap().score.as_milli(), 0);
    }

    #[test]
    fn stepwise_respects_dependencies() {
        let mut rubric = make_rubric(ScoringMode::Stepwise, &[1000, 2000]);
        rubric.items[1].depends_on = vec![rubric.items[0].id];
        let first = rubric.items[0].id;
        let second = rubric.items[1].id;
        // 只满足后置 → 后置不生效
        let result = aggregate(&rubric, &[second]).unwrap();
        assert_eq!(result.score.as_milli(), 0);
        assert_eq!(result.satisfied, [] as [eg_domain::RubricItemId; 0]);
        // 两项满足 → 都生效
        let result = aggregate(&rubric, &[first, second]).unwrap();
        assert_eq!(result.score.as_milli(), 3000);
        assert_eq!(result.satisfied.len(), 2);
    }

    #[test]
    fn partial_choice_tiers_and_wrong_selection() {
        let rule = PartialChoiceRule {
            correct_option_count: 2,
            tiers: vec![PartialChoiceTier {
                selected_correct: 1,
                points: MilliScore::from_milli(1000).unwrap(),
                label: None,
            }],
            wrong_selection_zero: true,
            source: PartialChoiceSource::TemplateData,
            source_note: None,
            total_milli: MilliScore::from_milli(2000).unwrap(),
        };
        let correct = vec!["A".to_owned(), "B".to_owned()];
        assert_eq!(
            partial_choice(&rule, &correct, &["A".to_owned()])
                .unwrap()
                .score
                .as_milli(),
            1000
        );
        assert_eq!(
            partial_choice(&rule, &correct, &["A".to_owned(), "B".to_owned()])
                .unwrap()
                .score
                .as_milli(),
            2000
        );
        let wrong = partial_choice(&rule, &correct, &["A".to_owned(), "C".to_owned()]).unwrap();
        assert_eq!(wrong.score.as_milli(), 0);
        assert!(wrong.wrong_selection);
        // 缺档 → 错误（转人工）
        let mut broken = rule.clone();
        broken.tiers.clear();
        // 档位数量不足时 validate 失败
        assert!(partial_choice(&broken, &correct, &["A".to_owned()]).is_err());
    }

    #[test]
    fn option_letters_parse() {
        assert_eq!(
            parse_option_letters("A,B").unwrap(),
            vec!["A".to_owned(), "B".to_owned()]
        );
        assert_eq!(
            parse_option_letters("ba").unwrap(),
            vec!["A".to_owned(), "B".to_owned()]
        );
        assert!(parse_option_letters("答案").is_none());
    }
}
