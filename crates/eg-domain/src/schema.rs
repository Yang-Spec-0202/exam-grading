//! 持久化对象的 schema 版本约定（`SPEC-domain.md` §10）。
//!
//! 规范 §10：「每类持久化对象带 `schema_version`；升级只新增字段并给默认值，
//! 禁止重命名后改历史数据」。
//!
//! 领域层的字段表（§3–§8）是逐字权威的字段清单，`schema_version` 不在其中；
//! 为不引入与规范字段表冲突的字段，本 crate 用 [`SchemaVersioned`] 关联常量
//! 承载版本号，由存储层（`platform`）在 JSON 外层封装 `schema_version` 键。
//! 该解释已在研究记录中登记为「规范解释项」，待 Lead 裁定是否改为字段。

/// 持久化对象的 schema 版本关联常量。
pub trait SchemaVersioned {
    /// 当前 schema 版本；升级只新增带默认值的字段。
    const SCHEMA_VERSION: u32 = 1;
}

/// 领域层当前 schema 版本（所有持久化对象初始为 1）。
pub const DOMAIN_SCHEMA_VERSION: u32 = 1;

/// 为持久化类型实现 [`SchemaVersioned`]（默认版本 1）。
macro_rules! impl_schema_versioned {
    ($($type:ty),* $(,)?) => {
        $(
            impl SchemaVersioned for $type {}
        )*
    };
}

impl_schema_versioned!(
    crate::answer::AnswerKey,
    crate::evidence::ComparatorTrace,
    crate::evidence::ModelCall,
    crate::grade::GradeRecord,
    crate::grade::SubmissionExclusion,
    crate::page::Page,
    crate::page::PageQuality,
    crate::page::Segment,
    crate::page::Submission,
    crate::paper::Classroom,
    crate::paper::Paper,
    crate::paper::PaperTemplate,
    crate::paper::Question,
    crate::paper::Student,
    crate::recognition::Recognition,
    crate::rubric::Rubric,
    crate::rubric::RubricDraft,
    crate::rubric::RuleText,
    crate::subject::StudentAnswer,
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_persisted_types_share_initial_version() {
        assert_eq!(DOMAIN_SCHEMA_VERSION, 1);
        assert_eq!(crate::paper::Paper::SCHEMA_VERSION, 1);
        assert_eq!(crate::rubric::Rubric::SCHEMA_VERSION, 1);
        assert_eq!(crate::grade::GradeRecord::SCHEMA_VERSION, 1);
        assert_eq!(crate::evidence::ModelCall::SCHEMA_VERSION, 1);
        assert_eq!(crate::recognition::Recognition::SCHEMA_VERSION, 1);
    }
}
