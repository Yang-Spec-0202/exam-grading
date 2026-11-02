//! `eg-domain`：领域类型、不变量、分数与证据模型、学科契约、规则草稿（纯类型，零 IO）。
//!
//! 状态：M0 工程骨架，仅保证可编译，不含任何业务实现。
//! 规范见仓库根目录 `SPEC-domain.md`；实现任务见 `tasks/todo.md` 对应里程碑。

#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    reason = "M0 骨架无公开 API；实现阶段按需在单项上补充文档"
)]
