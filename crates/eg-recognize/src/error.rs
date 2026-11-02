//! 识别错误分类、结构/语义违规类型与转人工映射（`SPEC-recognize.md` §2.2、§3.2）。

use eg_domain::ManualReason;
use thiserror::Error;

/// 识别错误（页级/段级共用的类型化失败）。
///
/// 映射规则（`SPEC-recognize.md` §2.2）：错误必须携带足够信息让上层决定
/// 「转人工」还是「任务级失败重试」；本模块不静默降级。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RecognizeError {
    /// 推理后端不可用：健康检查失败、熔断打开、超时（`inference` 侧汇报）。
    #[error("识别后端不可用：{reason}")]
    BackendUnavailable {
        /// 后端汇报的原因摘要。
        reason: String,
    },
    /// 结构不合法：严格 Schema 校验失败，且机械修复预算用尽。
    #[error("模型输出结构不合法（{repaired}）：{reason}", repaired = if *repaired { "修复后仍不合法" } else { "修复未启用或不可修复" })]
    Structure {
        /// 校验错误摘要（来自严格 Schema 解析）。
        reason: String,
        /// 是否已经发起过至少一次机械修复。
        repaired: bool,
    },
    /// 语义不合法：坐标越界、条码块、重复块、数量/长度超限等；不重试。
    #[error("模型输出语义不合法：{reason}")]
    Semantic {
        /// 语义违规摘要。
        reason: String,
    },
    /// 空结果：模型返回零个块或拒答。
    #[error("模型返回空结果（零个识别块或拒答）")]
    EmptyResult,
    /// 输出截断：达到输出上限被截断；显式失败，不得截断后当成功。
    #[error("模型输出被截断（达到输出上限）")]
    Truncated,
    /// 存储失败：缓存或结果仓储写入失败；任务级失败并重试（幂等）。
    #[error("识别结果存储失败：{reason}")]
    Storage {
        /// 存储层错误摘要。
        reason: String,
    },
    /// 取消：取消令牌触发；任务取消，不写结果。
    #[error("识别任务已取消")]
    Cancelled,
    /// 配置错误：能力缺失、配置值越界（启动期即失败，不静默降级）。
    #[error("识别配置错误：{reason}")]
    Configuration {
        /// 配置错误摘要。
        reason: String,
    },
}

impl RecognizeError {
    /// 映射为转人工原因（`SPEC-recognize.md` §2.2 表）。
    ///
    /// 返回 `None` 表示这不是「转人工」类错误：存储失败是任务级重试，取消是任务取消，
    /// 配置错误是启动期失败，都不应写入人工队列。
    #[must_use]
    pub fn manual_reason(&self) -> Option<ManualReason> {
        match self {
            Self::BackendUnavailable { .. } => Some(ManualReason::BackendUnavailable),
            Self::Structure { .. }
            | Self::Semantic { .. }
            | Self::EmptyResult
            | Self::Truncated => Some(ManualReason::RecognitionUnavailable),
            Self::Storage { .. } | Self::Cancelled | Self::Configuration { .. } => None,
        }
    }

    /// 是否应在任务层重试（幂等写入失败）。
    #[must_use]
    pub const fn is_task_level_retriable(&self) -> bool {
        matches!(self, Self::Storage { .. })
    }
}

/// 结构校验错误（严格 Schema 解析产出）。
///
/// 只有 [`StructureError::Repairable`] 会触发机械修复；修复只允许改格式
/// （`SPEC-recognize.md` §3.2），不得新增、删除或改写识别内容。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StructureError {
    /// 可机械修复：JSON 语法错误、代码围栏、未知字段、枚举拼写错误。
    #[error("结构错误（可机械修复）：{0}")]
    Repairable(String),
    /// 不可机械修复：缺失字段、类型错误、长度/数量上限、契约版本不符。
    #[error("结构错误（不可机械修复）：{0}")]
    Fatal(String),
    /// 空结果：零个块（不是结构错误，按 `SPEC-recognize.md` §2.2 单独映射）。
    #[error("模型输出零个识别块")]
    Empty,
}

impl StructureError {
    /// 是否允许发起机械修复。
    #[must_use]
    pub const fn is_repairable(&self) -> bool {
        matches!(self, Self::Repairable(_))
    }

    /// 回填给模型的校验错误摘要。
    #[must_use]
    pub fn reason(&self) -> &str {
        match self {
            Self::Repairable(reason) | Self::Fatal(reason) => reason,
            Self::Empty => "blocks 为空：至少输出一个识别块",
        }
    }
}

/// 语义违规（`SPEC-recognize.md` §2.2、§3.2）：语义校验不重试，直接转人工。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SemanticViolation {
    /// 坐标含非有限值（NaN/Inf）。
    #[error("第 {block} 个块坐标含非有限值")]
    NonFiniteCoord {
        /// 块序号（1 起）。
        block: usize,
    },
    /// 坐标框越界（归一化后必须落在 0–1）。
    #[error("第 {block} 个块坐标越界：{reason}")]
    CoordOutOfRange {
        /// 块序号（1 起）。
        block: usize,
        /// 领域几何校验原因。
        reason: String,
    },
    /// 条码块：`BlockKind::Barcode` 为保留变体，识别阶段永不产出。
    #[error("第 {block} 个块为条码块：v1 识别阶段不得产出条码")]
    Barcode {
        /// 块序号（1 起）。
        block: usize,
    },
    /// 重复块：坐标与文本完全相同的块（多为模型重复输出）。
    #[error("第 {block} 个块与第 {first} 个块重复（坐标与文本相同）")]
    DuplicateBlock {
        /// 块序号（1 起）。
        block: usize,
        /// 首次出现的块序号（1 起）。
        first: usize,
    },
    /// 识别块数量为 0（空结果）。
    #[error("识别块数量为 0")]
    NoBlocks,
    /// 页面坐标换算声明非法：`page` 宽高出现 0。
    #[error("page 声明非法：宽高必须为正")]
    InvalidPageDims,
}

/// 把领域错误摘要化为配置错误（供适配器包装平台/领域错误）。
#[must_use]
pub fn config_error(reason: impl Into<String>) -> RecognizeError {
    RecognizeError::Configuration {
        reason: reason.into(),
    }
}

/// 把存储层错误摘要化为存储失败（任务级重试）。
#[must_use]
pub fn storage_error(reason: impl Into<String>) -> RecognizeError {
    RecognizeError::Storage {
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_reason_mapping_matches_spec_table() {
        assert_eq!(
            RecognizeError::BackendUnavailable {
                reason: "熔断打开".to_owned()
            }
            .manual_reason(),
            Some(ManualReason::BackendUnavailable)
        );
        for error in [
            RecognizeError::Structure {
                reason: "unknown field".to_owned(),
                repaired: true,
            },
            RecognizeError::Semantic {
                reason: "坐标越界".to_owned(),
            },
            RecognizeError::EmptyResult,
            RecognizeError::Truncated,
        ] {
            assert_eq!(
                error.manual_reason(),
                Some(ManualReason::RecognitionUnavailable),
                "{error} 必须映射为「识别不可用」"
            );
        }
        for error in [
            RecognizeError::Storage {
                reason: "写入失败".to_owned(),
            },
            RecognizeError::Cancelled,
            RecognizeError::Configuration {
                reason: "缺缓存能力".to_owned(),
            },
        ] {
            assert_eq!(error.manual_reason(), None, "{error} 不得转人工");
        }
        assert!(
            RecognizeError::Storage {
                reason: "x".to_owned()
            }
            .is_task_level_retriable()
        );
        assert!(
            !RecognizeError::Semantic {
                reason: "x".to_owned()
            }
            .is_task_level_retriable()
        );
    }

    #[test]
    fn repairable_classification() {
        assert!(StructureError::Repairable("expected `,`".to_owned()).is_repairable());
        assert!(!StructureError::Fatal("missing field `blocks`".to_owned()).is_repairable());
        assert!(!StructureError::Empty.is_repairable());
        assert_eq!(
            StructureError::Empty.reason(),
            "blocks 为空：至少输出一个识别块"
        );
    }
}
