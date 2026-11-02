//! `eg-subject-math`：数学学科包（归一化、比较器链、分步给分、CAS 适配）。
//!
//! 当前里程碑：T13（四层归一化 + 内部语法树）与 T14（基础比较器 + 比较器链）。
//! 规范：`SPEC-subject-math.md` v0.2.1（§3 归一化流水线、§4 AST、§5 比较器链、
//! §11 失败模式、§12 测试策略）；契约来自 `eg-domain`（`SubjectPack`/`Comparator`）。
//!
//! 边界：
//! - 纯函数、零 IO/网络/时钟；比较器不调用模型（CAS 在 T27 经独立适配层）。
//! - 精确数值只用 [`number::Rational`]（大整数/大分数），**禁止 f64 比较**。
//! - 解析失败是归一化错误（`NormalizeError`），不是「答错」；超出支持范围的
//!   可识别写法解析为 [`ast::Expr::Function`]，由比较器返回「无法判定」。
//! - 解析器手写（词法 + 递归下降/优先级），未引入第三方解析库；选型依据见
//!   `docs/dev/research/subject-math.md`。
//!
//! 扩展点（T26/T27 后续）：
//! - `ast::Expr` 已含向量/矩阵/集合/区间/单位/函数节点；
//! - [`comparators::comparator_by_id`] 对尚未实现的比较器 id 返回保守的
//!   「无法判定」占位；
//! - [`canonical`] 的多项式/根式规范化可被 CAS 适配层补充。

#![deny(unsafe_code)]
#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::doc_markdown,
    clippy::must_use_candidate,
    clippy::return_self_not_must_use,
    clippy::missing_const_for_fn,
    clippy::module_name_repetitions,
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    reason = "错误文档由错误枚举的中文文案承载；数值/渲染辅助函数接受所有权便于实现；\
              中文与专有名词不强制反引号；其余为纯风格类 pedantic 提示"
)]

pub mod ast;
pub mod canonical;
pub mod cas;
pub mod comparators;
pub mod equiv;
pub mod matrix;
pub mod normalize;
pub mod number;
pub mod pack;
pub mod parse;
pub mod setlike;
pub mod trig;
pub mod vector;

pub use ast::{Dim, Expr, Interval, IntervalEnd, RelOp, Unit};
pub use cas::{
    CAS_PROTOCOL_VERSION, CasClient, CasConfig, CasFailure, CasFingerprint, CasInfo, CasStats,
    CasZeroResult, MAX_EXPRESSION_CHARS,
};
pub use comparators::{
    BooleanExactComparator, DimensionConsistencyComparator, ExpressionCanonicalComparator,
    FractionExactComparator, IMPLEMENTED_COMPARATOR_IDS, IntervalComparator,
    NumericAbsoluteComparator, NumericUnitComparator, OptionsExactComparator, SetExactComparator,
    SolutionSetComparator, TextNormalizedComparator, UNIMPLEMENTED_COMPARATOR_IDS,
    comparator_by_id, comparator_by_id_with_cas, comparator_chain_for,
    comparator_chain_for_with_cas, default_chain_ids, unimplemented_comparator,
};
pub use matrix::MatrixEquivalenceComparator;
pub use normalize::{
    Correction, CorrectionLayer, NormalizedMath, normalize_raw, normalize_with_trace,
};
pub use number::{MAX_BITS, NumError, Rational};
pub use pack::{
    CasStatus, JUDGE_PROFILE_ID, MathEngineFingerprint, MathSubjectPack, PROMPT_VERSION,
};
pub use parse::{ParseError, parse_expr};
pub use trig::TrigIdentityComparator;
pub use vector::VectorEquivalenceComparator;
