# 变更记录

> 记录规范与行为的变更（按版本倒序）。文档结构变更也在此登记。

## [0.1.0] - 2026-11-02（M0 工程骨架）

### 新增

- Rust workspace（edition 2024、`resolver = "3"`、`[workspace.lints]`）与 13 个空 crate：`eg-domain`、`eg-platform`、`eg-capture`、`eg-inference`、`eg-recognize`、`eg-align`、`eg-rubric`、`eg-grade`、`eg-subject-math`、`eg-review`、`eg-report`、`eg-app`、`egctl`。
- 工具链固定入口 `rust-toolchain.toml`（stable + rustfmt + clippy），实测版本见 `dev/research/toolchain.md`。
- 依赖许可门禁 `deny.toml`（白名单 MIT / Apache-2.0 / BSD-3-Clause / ISC / Unicode-3.0）。
- CI 门禁脚本 `scripts/ci.sh`（fmt + clippy + test + deny + 文档），文档脚本 `scripts/docs.sh`、`scripts/docs-check.sh`。
- 文档系统骨架：`docs/README.md`、`docs/user/`、`docs/dev/`、`docs/dev/research/`（含模板）、`docs/adr/`（含模板与 ADR-0001）、`docs/book/`（mdBook，GitHub Pages 目标）。
- `README.md` 项目骨架与 `docs/changelog.md`。

### 说明

- 本版本不含任何业务实现；各 crate 仅有文档注释与空入口。
- 依赖版本尚未引入；`Cargo.lock` 随 workspace 生成并入库。
