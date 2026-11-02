//! `RubricSource` ← `eg-rubric::ports::RubricStore`（已确认生效规则；草稿不可用）。
//!
//! 失败语义：读取失败按「无生效规则」处理（safe fail：规则题转人工，绝不猜测规则）。
//! 读取错误由平台日志记录（本适配器不吞业务错误为「可用规则」）。

use std::sync::Arc;

use eg_domain::{QuestionId, Rubric};
use eg_rubric::ports::RubricStore;

use crate::ports::{RubricAvailability, RubricSource};

/// 规则仓储适配器。
#[derive(Clone)]
pub struct RubricStoreSource {
    store: Arc<dyn RubricStore>,
}

impl std::fmt::Debug for RubricStoreSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RubricStoreSource")
            .finish_non_exhaustive()
    }
}

impl RubricStoreSource {
    /// 构造。
    #[must_use]
    pub fn new(store: Arc<dyn RubricStore>) -> Self {
        Self { store }
    }

    /// 底层仓储（组合根诊断用）。
    #[must_use]
    pub fn store(&self) -> &Arc<dyn RubricStore> {
        &self.store
    }
}

impl RubricSource for RubricStoreSource {
    fn availability(&self, question_id: QuestionId) -> RubricAvailability {
        match self.store.active_rubric(question_id) {
            Ok(Some(rubric)) => RubricAvailability::Confirmed(rubric),
            Ok(None) => {
                // 无生效规则：若有草稿则明确为 Draft（就绪检查会拦下），否则 Missing
                match self.store.list_drafts(question_id) {
                    Ok(drafts) if !drafts.is_empty() => RubricAvailability::Draft,
                    _ => RubricAvailability::Missing,
                }
            }
            // 读取失败按「无规则」fail-safe；规则题将由就绪/路由转人工
            Err(_) => RubricAvailability::Missing,
        }
    }

    fn active_version(&self, question_id: QuestionId) -> u32 {
        self.store.rubric_version(question_id).unwrap_or(0)
    }

    fn identity(&self) -> String {
        "rubric-store-v1".to_owned()
    }
}

/// 供测试/组合根直接读取生效规则（与 [`RubricSource`] 同源）。
#[must_use]
pub fn confirmed_rubric(source: &RubricStoreSource, question_id: QuestionId) -> Option<Rubric> {
    source.store.active_rubric(question_id).ok().flatten()
}
