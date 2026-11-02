//! 构建脚本：用 `slint-build` 把 `ui/app.slint` 编译成 Rust 模块（构建期校验 UI 语法）。
//!
//! 规范：`SPEC-app.md` §3（Slint 文件只做声明式绑定）· `docs/dev/research/slint.md` §4 Q7/R5。

fn main() -> Result<(), slint_build::CompileError> {
    slint_build::compile("ui/app.slint")
}
