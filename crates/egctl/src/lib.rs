//! `egctl` 库：诊断、夹具回放、验收基准与 acceptance 全链路。
//!
//! bin 入口（`src/main.rs`）只做参数解析与输出；可复用的验收/诊断逻辑放在本库，
//! 便于集成测试（`tests/acceptance.rs`）在 CI 中直接调用，不依赖桌面或真实模型。

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]
#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::format_push_string,
    clippy::doc_markdown,
    clippy::redundant_closure,
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::struct_excessive_bools,
    clippy::option_if_let_else,
    clippy::manual_let_else,
    clippy::single_match_else,
    clippy::significant_drop_tightening,
    clippy::items_after_statements,
    clippy::unreadable_literal,
    clippy::uninlined_format_args,
    clippy::needless_borrow,
    clippy::if_not_else,
    clippy::match_same_arms,
    clippy::assigning_clones,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::unnecessary_wraps,
    clippy::assert_is_empty,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::module_name_repetitions,
    clippy::vec_init_then_push,
    clippy::field_reassign_with_default,
    unreachable_pub,
    reason = "运维/验收 CLI 与端到端夹具：中文报告字符串、数值转换与逐题断言需要直白写法；\
              错误文档由中文错误类型与 CLI 输出承载"
)]

pub mod acceptance;
pub mod bench;
pub mod docs_check;
pub mod doctor;
pub mod eval;
pub mod fixture;
pub mod registry;
pub mod replay;
pub mod service;
pub mod util;
