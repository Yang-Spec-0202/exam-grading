# ADR-0009：项目采用 GPL-3.0-only 与 Slint 的 GPL 分支

> 状态：已接受 · 2026-11-02 · 决策者：项目（单机匿名教师）· 相关规范：`SPEC.md` §10.1、`README.md` 许可节、`deny.toml`
> 说明：本 ADR 只解释许可决策；逐条依赖来源见研究记录与 `deny.toml` 注释。

## 1. 背景

- 项目使用 Slint 1.18.1 作为 GUI 框架；Slint 为三重许可：GPL-3.0-only、Slint Royalty-free 2.0、Slint Software 3.0（商业）。
- 项目定位是个人求职展示、源码公开、不做二进制分发（A7：不打包 .app/DMG/公证）。
- 依赖树经实测包含 GPL 之外的多种许可（含 Zlib、BSD-2-Clause、BSL-1.0、Unlicense 等），需要 fail-closed 白名单，避免「默认放行」。
- 模型权重不随仓库分发，其许可与代码许可需分开审查。

## 2. 决策

- **项目整体 GPL-3.0-only**：仓库内 `LICENSE` 为 GNU GPL v3 官方全文，仅此版本（不含 "or later"）；源码公开，不提供闭源分发路径。
- **Slint 走 GPL 分支**：不启用 Royalty-free 归属（attribution）方案，不购买商业许可；涉及 Slint 的代码与分发产物同受 GPL-3.0-only 义务约束。
- **白名单 fail-closed**：`deny.toml` 固定 10 项许可白名单（MIT、Apache-2.0、BSD-3-Clause、ISC、Unicode-3.0、GPL-3.0-only、Zlib、BSD-2-Clause、BSL-1.0、Unlicense）；未列入即构建失败，不放宽为 allow-all；新增/变更白名单属 Ask first。
- **不分发二进制**：交付形态为开发机 `cargo run`；不做打包、签名、公证，因此不涉及分发合规流水线。
- **模型权重分离**：模型不在仓库内，按各自模型卡许可使用；代码许可审查不覆盖权重。
- 研究记录必须记录许可来源与版本（`docs/dev/research/slint.md`、`deps.md`、`pdf.md`）。

## 3. 后果

- 正面：许可声明清晰、可审计；fail-closed 防止「悄悄引入新许可」；不产生 Slint 归属界面义务；与求职展示定位一致。
- 负面：项目不能改为闭源分发；下游若需闭源使用需自行获得 Slint 商业许可或替换 GUI；GPL 兼容性审查是每次新增依赖的固定成本。
- 后续动作：新增依赖先跑 `cargo deny check`；若未来需要分发或闭源，必须重新评估 Slint 许可分支并更新本 ADR（不得直接改代码口径）。

## 4. 替代方案

| 方案 | 优点 | 缺点/风险 | 结论 |
|---|---|---|---|
| A：Slint Royalty-free 分支 | 无 GPL 传染，可分闭源 | 归属义务与条款约束；与「源码公开」定位不符；需持续跟踪条款 | 放弃 |
| B：购买 Slint 商业许可 | 最灵活 | 成本与项目定位不符 | 放弃 |
| C：换用 MIT/Apache 的 GUI 框架 | 许可最简单 | 已选定 Slint 并完成 macOS/中文/线程实测，迁移成本高 | 放弃（保留为重大变更选项） |
| D：GPL-3.0-only + Slint GPL 分支 + fail-closed 白名单（采用） | 与项目定位一致、义务明确 | 不可闭源分发 | **采用** |

## 5. 证据链接

| 证据 | 链接 | 说明 |
|---|---|---|
| 许可白名单与裁定 | `../../SPEC.md` | §10.1 十项白名单、项目自身许可决策 |
| 项目许可声明 | `../../LICENSE` | GPL-3.0 官方全文 |
| 项目定位与许可说明 | `../../README.md` | 许可节、不分发、模型权重不入库 |
| 许可门禁配置 | `../../deny.toml` | 白名单与 fail-closed 设置 |
| Slint 许可实测 | `../dev/research/slint.md` | 1.18.1 三重许可表达式与依赖树许可 |
| 依赖许可来源 | `../dev/research/deps.md` | 逐 crate 许可与来源 |
