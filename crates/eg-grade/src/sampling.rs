//! 快速模式抽检（`SPEC-grade.md` §7、CONFIG §6、T18）。
//!
//! 候选集 = 快速模式下本次任务「自动接受」的题目；权重：低置信度 0.70、其余 0.30；
//! 抽取数量 = ⌈候选数 × 比例⌉（至少 1 题）；按权重无放回随机抽样；
//! 种子与结果写入审计与判分记录（本模块返回 seed + sampled 列表，调用方落库）。

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "加权抽样使用 f64 权重（配置为 f64）；候选数量规模远小于 2^52，\n              截断/精度损失不影响可复现性与业务正确性"
)]

use eg_domain::{Confidence, QuestionId};

use crate::config::GradeConfig;

/// 抽检候选。
#[derive(Debug, Clone, PartialEq)]
pub struct SamplingCandidate {
    /// 题目 id。
    pub question_id: QuestionId,
    /// 判定置信度。
    pub confidence: Confidence,
    /// 该题当前模式阈值（用于判定「低置信度」= 阈值 + 0.10）。
    pub threshold: Confidence,
}

/// 抽检结果（可复现：同种子 + 同候选顺序 → 同结果）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SamplingResult {
    /// 实际使用的种子（写入审计/记录）。
    pub seed: u64,
    /// 被抽中的题目（按抽取顺序）。
    pub sampled: Vec<QuestionId>,
    /// 抽检比例。
    pub rate_percent: u32,
    /// 候选数量。
    pub candidate_count: usize,
    /// 低置信度候选数量。
    pub low_confidence_count: usize,
}

impl SamplingResult {
    /// 是否抽中。
    #[must_use]
    pub fn contains(&self, question_id: QuestionId) -> bool {
        self.sampled.contains(&question_id)
    }
}

/// SplitMix64（确定性 PRNG，无外部依赖；同种子序列固定）。
#[derive(Debug, Clone)]
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn next_unit(&mut self) -> f64 {
        // [0,1) 均匀
        (self.next_u64() >> 11) as f64 / ((1u64 << 53) as f64)
    }
}

/// 计算抽检数量：⌈候选数 × 比例⌉，至少 1 题（候选非空时）。
#[must_use]
pub fn sample_count(candidate_count: usize, rate: f64) -> usize {
    if candidate_count == 0 {
        return 0;
    }
    let raw = (candidate_count as f64 * rate).ceil() as usize;
    raw.clamp(1, candidate_count)
}

/// 是否低置信度（置信度 < 当前模式阈值 + 0.10）。
#[must_use]
pub fn is_low_confidence(candidate: &SamplingCandidate) -> bool {
    let boundary = (candidate.threshold.get() + 0.10).min(1.0);
    candidate.confidence.get() < boundary
}

/// 执行加权无放回抽样。
///
/// `default_seed`：配置未指定种子时使用（调用方按提交 id/时钟派生并记录）。
#[must_use]
pub fn sample_fast(
    candidates: &[SamplingCandidate],
    config: &GradeConfig,
    default_seed: u64,
) -> SamplingResult {
    let seed = config.sampling_seed.unwrap_or(default_seed);
    let rate = config.sampling_rate;
    let count = sample_count(candidates.len(), rate);
    let low_confidence_count = candidates
        .iter()
        .filter(|candidate| is_low_confidence(candidate))
        .count();
    let low_weight = config.low_confidence_weight.clamp(0.0, 1.0);
    let high_weight = 1.0 - low_weight;
    let mut rng = SplitMix64::new(seed);
    let mut remaining: Vec<(usize, f64)> = candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            let weight = if is_low_confidence(candidate) {
                low_weight
            } else {
                high_weight
            };
            (index, weight)
        })
        .collect();
    let mut sampled: Vec<QuestionId> = Vec::with_capacity(count);
    for _ in 0..count {
        if remaining.is_empty() {
            break;
        }
        let total: f64 = remaining.iter().map(|(_, weight)| *weight).sum();
        let pick = if total <= 0.0 {
            // 全部权重为 0（配置异常）→ 均匀抽样，保证仍然可复现
            (rng.next_unit() * remaining.len() as f64) as usize
        } else {
            let mut target = rng.next_unit() * total;
            let mut selected = remaining.len() - 1;
            for (index, (_, weight)) in remaining.iter().enumerate() {
                if target < *weight {
                    selected = index;
                    break;
                }
                target -= *weight;
            }
            selected
        };
        let (index, _) = remaining.remove(pick.min(remaining.len() - 1));
        sampled.push(candidates[index].question_id);
    }
    SamplingResult {
        seed,
        sampled,
        rate_percent: (rate * 100.0).round() as u32,
        candidate_count: candidates.len(),
        low_confidence_count,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use eg_domain::Ulid;

    fn candidate(index: u128, confidence: f64) -> SamplingCandidate {
        SamplingCandidate {
            question_id: QuestionId::from_ulid(Ulid::from_u128(index)),
            confidence: Confidence::new(confidence).unwrap(),
            threshold: Confidence::new(0.80).unwrap(),
        }
    }

    #[test]
    fn sample_count_ceil_and_minimum() {
        assert_eq!(sample_count(0, 0.2), 0);
        assert_eq!(sample_count(1, 0.2), 1);
        assert_eq!(sample_count(5, 0.2), 1);
        assert_eq!(sample_count(10, 0.2), 2);
        assert_eq!(sample_count(9, 0.2), 2);
        assert_eq!(sample_count(3, 1.0), 3);
    }

    #[test]
    fn sampling_is_reproducible_with_seed() {
        let config = GradeConfig::default();
        let candidates: Vec<SamplingCandidate> = (1..=10)
            .map(|index| candidate(index, if index % 2 == 0 { 0.75 } else { 0.95 }))
            .collect();
        let first = sample_fast(&candidates, &config, 42);
        let second = sample_fast(&candidates, &config, 42);
        assert_eq!(first.sampled, second.sampled);
        assert_eq!(first.seed, 42);
        assert_eq!(
            first.sampled.len(),
            sample_count(candidates.len(), config.sampling_rate)
        );
        // 不同种子应可能产生不同集合（至少记录可复现）
        let third = sample_fast(&candidates, &config, 43);
        assert_eq!(third.seed, 43);
    }

    #[test]
    fn configured_seed_overrides_default() {
        let config = GradeConfig {
            sampling_seed: Some(7),
            ..GradeConfig::default()
        };
        let candidates: Vec<SamplingCandidate> =
            (1..=5).map(|index| candidate(index, 0.9)).collect();
        let result = sample_fast(&candidates, &config, 999);
        assert_eq!(result.seed, 7);
    }

    #[test]
    fn low_confidence_detection() {
        assert!(is_low_confidence(&candidate(1, 0.85)));
        assert!(!is_low_confidence(&candidate(1, 0.95)));
    }
}
