# exam-grading 文档系统

> 状态：M0 骨架（2026-11-02）。文档门禁与阅读顺序已定，正文随里程碑补齐。
> 分工：规范（根目录 `SPEC*.md`）写「必须/禁止/验收」，文档写「怎么用/为什么这样设计」；两者不互相复制整段（`SPEC.md` §9）。

## 1. 阅读顺序

1. 用户：`user/` —— 怎么导入、怎么判分、怎么复核、怎么导出。
2. 开发：`dev/` —— 架构、模块边界、数据流、夹具、性能、发布与排查。
3. 决策：`adr/` —— 每个关键决策的背景、选项与结论。
4. 研究记录：`dev/research/` —— 实现前的检索证据与版本实测（`AGENTS.md` 强制）。
5. 规范：仓库根目录 `SPEC.md`、`SPEC-<module-id>.md`、`CAPABILITY-MAP.md`、`CONFIG.md`。

## 2. 目录

| 路径 | 内容 | 状态 |
|---|---|---|
| [`user/`](user/README.md) | 快速开始、命名约定、双模式说明、复核操作、导出说明、常见故障、隐私说明 | M0 占位，M9 补齐 |
| [`dev/`](dev/README.md) | 架构总览、模块边界、数据流、测试夹具、性能基线、发布与回滚、故障排查 | M0 骨架 |
| [`dev/research/`](dev/research/README.md) | 每模块实现前的研究记录（问题/来源/结论/版本实测/风险/偏差） | 已启用，模板见 `_template.md` |
| [`adr/`](adr/README.md) | 编号 ADR；至少覆盖两层判分、双模式、规则结构化、CAS 选型、文件名身份、按任务鉴权、规范无代码 | M0 建索引，随决策补齐 |
| [`changelog.md`](changelog.md) | 按版本记录规范与行为变更 | 已建 |
| [`book/`](book/README.md) | mdBook 静态站源码与配置（GitHub Pages 目标） | 已建 |

## 3. 文档构建与门禁

- 构建：`scripts/docs.sh`（先从 `docs/` 生成 mdBook 源，再 `mdbook build docs/book`）。
- 链接检查：`scripts/docs-check.sh`（死链检查 + mdBook 构建），纳入 `scripts/ci.sh`。
- 门禁：文档链接 0 死链；用户可见行为变更必须同步 `user/` 与 `changelog.md`；ADR 与规范变更同一次提交（`SPEC.md` §9）。
- 发布目标：GitHub Pages（构建产物 `docs/book/book/`，不提交）。

## 4. 写作约定

- 中文优先；命令与路径用行内代码；不写大段重复规范的验收条款。
- 文档可以包含少量命令示例；**规范文件内禁止任何示例代码**（`SPEC.md` §9、`CAPABILITY-MAP.md` §7）。
- 所有外部依赖结论必须能追溯到 `dev/research/` 中的来源与日期。
