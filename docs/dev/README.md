# 开发者文档

> 状态：M0 骨架。架构与流程原则已定，细节随实现补齐。

## 目录（计划）

| 页面 | 内容 | 交付里程碑 |
|---|---|---|
| `architecture.md` | 分层、依赖方向、组合根、线程模型、数据流总览 | M0（已建骨架） |
| `modules.md` | 13 个 crate 的职责与公开边界 | M1 起 |
| `data-model.md` | SQLite 表、资产库、迁移策略 | M1 |
| `fixtures.md` | 脱敏夹具结构、回放方式、期望输出格式 | M2 |
| `performance.md` | 性能基线与回归门禁 | M10 |
| `release.md` | 版本、回滚、发布检查单 | M10 |
| `troubleshooting.md` | 本地故障排查（日志、诊断包、数据库） | M1 起 |

## 实现纪律（强制）

- 先检索再编码：见根目录 `AGENTS.md` 与 `CONFIG.md` 的 `dev.research_first`（不可关闭）。
- 每个模块实现前写 `research/<module>.md`：问题、来源（URL+版本/日期）、结论、版本实测、风险、与规范的偏差。
- 依赖版本以实测为准并记录；规范中的版本号只是下限。
- 每次提交前：`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --all-features -- -D warnings`、`cargo test --workspace`、`cargo deny check`。

## 工具链

- 固定入口 `rust-toolchain.toml`（stable + rustfmt + clippy）；实测版本与来源见 `research/toolchain.md`。
- 文档站：`scripts/docs.sh`；链接检查：`scripts/docs-check.sh`。
