//! 预算与费用记账（`SPEC-inference.md` §4/§9.7）。
//!
//! 两条规则：
//! 1. **本地后端费用恒为 0**（离线运行费用累计必须为 0）；
//! 2. **云端费用按定价表逐次记账**，扣减发生在调用之后（失败也照实记账）；
//!    预算检查发生在调用之前，超限则**不发请求**。
//!
//! 记账是进程内账本；持久化由 `platform` 的审计/账本接口负责（见 `platform_adapter`）。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use eg_domain::{Clock, TaskId};

use crate::error::InferenceError;
use crate::registry::CloudPricing;
use crate::types::TokenUsage;

/// 定价时段（空闲/高峰）。云端定价表里两档不同，按调用时刻选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PricingTier {
    /// 空闲时段。
    Idle,
    /// 高峰时段。
    Peak,
}

/// 按定价表计算一次调用的费用（微元）。
///
/// 公式（整数计算，向上取整到微元）：
/// `未命中输入×输入价 + 命中输入×命中价 + 输出×输出价`，单价单位是「每百万 token」。
/// 向上取整保证账本不会低记费用。
#[must_use]
pub fn cost_micros(pricing: &CloudPricing, tier: PricingTier, usage: TokenUsage) -> u64 {
    let (input_price, hit_price, output_price) = match tier {
        PricingTier::Idle => (
            pricing.input_cache_miss_idle_micros,
            pricing.input_cache_hit_idle_micros,
            pricing.output_idle_micros,
        ),
        PricingTier::Peak => (
            pricing.input_cache_miss_peak_micros,
            pricing.input_cache_hit_peak_micros,
            pricing.output_peak_micros,
        ),
    };
    let billed_input = u128::from(usage.billed_input_tokens());
    let cached_input = u128::from(usage.cached_input_tokens);
    let output = u128::from(usage.output_tokens);
    let numerator = billed_input * u128::from(input_price)
        + cached_input * u128::from(hit_price)
        + output * u128::from(output_price);
    let micros = numerator.div_ceil(1_000_000);
    u64::try_from(micros).unwrap_or(u64::MAX)
}

#[derive(Debug, Clone, Copy)]
struct LedgerEntry {
    limit_micros: Option<u64>,
    spent_micros: u64,
}

/// 进程内预算账本（按任务）。
pub struct BudgetLedger {
    clock: Arc<dyn Clock>,
    entries: Mutex<BTreeMap<TaskId, LedgerEntry>>,
}

impl core::fmt::Debug for BudgetLedger {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("BudgetLedger")
            .field("tasks", &self.entries_mut().len())
            .finish_non_exhaustive()
    }
}

impl BudgetLedger {
    /// 构造空账本。
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            entries: Mutex::new(BTreeMap::new()),
        }
    }

    /// 登记任务（不限额度；本地后端用）。
    pub fn open_unlimited(&self, task_id: TaskId) {
        self.entries_mut().insert(
            task_id,
            LedgerEntry {
                limit_micros: None,
                spent_micros: 0,
            },
        );
    }

    /// 登记任务并设置云端费用上限（微元）。
    ///
    /// 与 `TaskConsent.budget_micros` 对齐；重复登记以最新额度覆盖，已花费保留。
    pub fn open_limited(&self, task_id: TaskId, limit_micros: u64) {
        let spent = self.spent_micros(task_id);
        self.entries_mut().insert(
            task_id,
            LedgerEntry {
                limit_micros: Some(limit_micros),
                spent_micros: spent,
            },
        );
    }

    /// 调用前预算检查：超限则**不发请求**。
    ///
    /// # Errors
    ///
    /// 已用 + 预计 > 上限时返回 [`InferenceError::BudgetExceeded`]。
    pub fn authorize(&self, task_id: TaskId, estimate_micros: u64) -> Result<(), InferenceError> {
        let entries = self.entries_mut();
        let Some(entry) = entries.get(&task_id) else {
            // 未登记任务视为离线/不限额度：本地调用费用为 0。
            return Ok(());
        };
        if let Some(limit) = entry.limit_micros {
            let needed = entry.spent_micros.saturating_add(estimate_micros);
            if needed > limit {
                return Err(InferenceError::BudgetExceeded {
                    spent_micros: entry.spent_micros,
                    limit_micros: limit,
                    needed_micros: estimate_micros,
                });
            }
        }
        Ok(())
    }

    /// 调用后记账（失败也照实记账）。
    pub fn charge(&self, task_id: TaskId, cost_micros: u64) {
        let mut entries = self.entries_mut();
        let entry = entries.entry(task_id).or_insert(LedgerEntry {
            limit_micros: None,
            spent_micros: 0,
        });
        entry.spent_micros = entry.spent_micros.saturating_add(cost_micros);
    }

    /// 已花费（未登记为 0）。
    #[must_use]
    pub fn spent_micros(&self, task_id: TaskId) -> u64 {
        self.entries_mut()
            .get(&task_id)
            .map_or(0, |entry| entry.spent_micros)
    }

    /// 剩余额度（不限额度时为 `None`）。
    #[must_use]
    pub fn remaining_micros(&self, task_id: TaskId) -> Option<u64> {
        self.entries_mut().get(&task_id).and_then(|entry| {
            entry
                .limit_micros
                .map(|limit| limit.saturating_sub(entry.spent_micros))
        })
    }

    /// 任务结束时清理账本（额度不跨任务复用）。
    pub fn close_task(&self, task_id: TaskId) {
        self.entries_mut().remove(&task_id);
    }

    /// 当前时间（供调用方判断定价时段）。
    #[must_use]
    pub fn now(&self) -> eg_domain::Timestamp {
        self.clock.now()
    }

    fn entries_mut(&self) -> std::sync::MutexGuard<'_, BTreeMap<TaskId, LedgerEntry>> {
        match self.entries.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::sync::Arc;

    use eg_domain::{TaskId, Ulid};

    use crate::registry::CloudPricing;
    use crate::testing::TestClock;

    use super::*;

    fn task(seed: u128) -> TaskId {
        TaskId::from_ulid(Ulid::from_u128(seed))
    }

    /// 与 `config/cloud.deepseek.toml` 一致的 DeepSeek flash 价格。
    fn deepseek_flash() -> CloudPricing {
        CloudPricing {
            input_cache_miss_idle_micros: 1_000_000,
            input_cache_miss_peak_micros: 2_000_000,
            input_cache_hit_idle_micros: 20_000,
            input_cache_hit_peak_micros: 40_000,
            output_idle_micros: 4_000_000,
            output_peak_micros: 8_000_000,
        }
    }

    #[test]
    fn cost_matches_price_table_exactly() {
        let pricing = deepseek_flash();
        // 100 万输入 token（峰时） = 2 元 = 2_000_000 微元
        let input_only = TokenUsage {
            input_tokens: 1_000_000,
            cached_input_tokens: 0,
            output_tokens: 0,
        };
        assert_eq!(
            cost_micros(&pricing, PricingTier::Peak, input_only),
            2_000_000
        );
        // 100 万输出 token（峰时） = 8 元
        let output_only = TokenUsage {
            input_tokens: 0,
            cached_input_tokens: 0,
            output_tokens: 1_000_000,
        };
        assert_eq!(
            cost_micros(&pricing, PricingTier::Peak, output_only),
            8_000_000
        );
        // 空闲时段是峰时的一半
        assert_eq!(
            cost_micros(&pricing, PricingTier::Idle, output_only),
            4_000_000
        );
        // 缓存命中输入 100 万（空闲） = 0.02 元 = 20_000 微元
        let cached = TokenUsage {
            input_tokens: 1_000_000,
            cached_input_tokens: 1_000_000,
            output_tokens: 0,
        };
        assert_eq!(cost_micros(&pricing, PricingTier::Idle, cached), 20_000);
        // 混合：500k 未命中输入 + 500k 命中 + 1k 输出（峰时）
        let mixed = TokenUsage {
            input_tokens: 1_000_000,
            cached_input_tokens: 500_000,
            output_tokens: 1_000,
        };
        // 500k*2e6/1e6 = 1_000_000；500k*40_000/1e6 = 20_000；1k*8e6/1e6 = 8_000
        assert_eq!(cost_micros(&pricing, PricingTier::Peak, mixed), 1_028_000);
        // 零用量不产生费用
        assert_eq!(
            cost_micros(&pricing, PricingTier::Peak, TokenUsage::ZERO),
            0
        );
    }

    #[test]
    fn cost_rounds_up_so_the_ledger_never_undercharges() {
        let pricing = deepseek_flash();
        // 1 个输出 token（峰时）= 8 微元（8_000_000/1_000_000 = 8 精确）
        let one = TokenUsage {
            input_tokens: 0,
            cached_input_tokens: 0,
            output_tokens: 1,
        };
        assert_eq!(cost_micros(&pricing, PricingTier::Peak, one), 8);
        // 1 个 token 的空闲输入价 = 1 微元（1_000_000/1_000_000）
        let one_in = TokenUsage {
            input_tokens: 1,
            cached_input_tokens: 0,
            output_tokens: 0,
        };
        assert_eq!(cost_micros(&pricing, PricingTier::Idle, one_in), 1);
    }

    #[test]
    fn limited_budget_blocks_before_the_call_and_keeps_spent_on_failure() {
        let clock = Arc::new(TestClock::at_ms(0));
        let ledger = BudgetLedger::new(clock);
        let task_id = task(1);
        ledger.open_limited(task_id, 1_000);
        assert!(ledger.authorize(task_id, 600).is_ok());
        ledger.charge(task_id, 600);
        assert_eq!(ledger.spent_micros(task_id), 600);
        assert_eq!(ledger.remaining_micros(task_id), Some(400));

        // 预计超限 → 必须在发请求前拒绝
        let error = ledger.authorize(task_id, 500).unwrap_err();
        assert!(matches!(error, InferenceError::BudgetExceeded { .. }));
        assert_eq!(error.class(), crate::error::FailureClass::Budget);

        // 一次失败调用仍然照实记账（不退还）
        ledger.charge(task_id, 400);
        assert_eq!(ledger.spent_micros(task_id), 1_000);
        assert_eq!(ledger.remaining_micros(task_id), Some(0));
        assert!(ledger.authorize(task_id, 1).is_err());
    }

    #[test]
    fn unlimited_and_unregistered_tasks_are_free() {
        let clock = Arc::new(TestClock::at_ms(0));
        let ledger = BudgetLedger::new(clock);
        let task_id = task(2);
        // 未登记 → 视为离线，允许且无额度概念
        assert!(ledger.authorize(task_id, u64::MAX).is_ok());
        assert_eq!(ledger.remaining_micros(task_id), None);
        ledger.open_unlimited(task_id);
        assert!(ledger.authorize(task_id, u64::MAX).is_ok());
        ledger.charge(task_id, 0);
        assert_eq!(ledger.spent_micros(task_id), 0, "离线费用必须为 0");
    }

    #[test]
    fn reopening_keeps_spent_and_applies_new_limit() {
        let clock = Arc::new(TestClock::at_ms(0));
        let ledger = BudgetLedger::new(clock);
        let task_id = task(3);
        ledger.open_limited(task_id, 1_000);
        ledger.charge(task_id, 700);
        ledger.open_limited(task_id, 2_000);
        assert_eq!(ledger.spent_micros(task_id), 700, "重开不得清零已花费");
        assert_eq!(ledger.remaining_micros(task_id), Some(1_300));
        ledger.close_task(task_id);
        assert_eq!(ledger.spent_micros(task_id), 0);
    }
}
