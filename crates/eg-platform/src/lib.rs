//! `eg-platform`：配置、密钥、数据库、资产库、持久任务、日志、审计、单实例、崩溃恢复。
//!
//! 状态：M0 工程骨架，仅保证可编译，不含任何业务实现。
//! 规范见仓库根目录 `SPEC-platform.md`；实现任务见 `tasks/todo.md` 对应里程碑。

#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    reason = "M0 骨架无公开 API；实现阶段按需在单项上补充文档"
)]
