# exam-grading 文档系统

> 状态：随实现更新（2026-11-02）。文档门禁与阅读顺序已定；研究记录、用户手册、ADR 已按实现进度补齐，验收报告与性能基线待 M10。
> 分工：规范（根目录 `SPEC*.md`）写「必须/禁止/验收」，文档写「怎么用/为什么这样设计」；两者不互相复制整段（`SPEC.md` §9）。

## 1. 阅读顺序

1. 用户：`user/` —— 怎么导入、怎么判分、怎么复核、怎么导出。
2. 开发：`dev/` —— 架构、模块边界、数据流、夹具、性能、发布与排查。
3. 决策：`adr/` —— 每个关键决策的背景、选项与结论。
4. 研究记录：`dev/research/` —— 实现前的检索证据与版本实测（`AGENTS.md` 强制）。
5. 规范：仓库根目录 `SPEC.md`、`SPEC-<module-id>.md`、`CAPABILITY-MAP.md`、`CONFIG.md`。

## 2. 目录与状态

| 路径 | 内容 | 当前状态（2026-11-02） |
|---|---|---|
| [`user/`](user/README.md) | 快速开始、命名约定、双模式说明、复核操作、评分规则、导出说明、常见故障、隐私说明 | M10 初稿（9 篇，含旧链接页 `quickstart.md`）；描述目标行为，未实现步骤标「计划中」，UI 上线后回填截图与实测 |
| [`dev/`](dev/README.md) | 架构总览、模块边界、数据流、测试夹具、性能基线、发布与回滚、故障排查 | 架构总览与文档约定已建；modules/data-model/fixtures/performance/release/troubleshooting 随 M9–M10 补齐 |
| [`dev/research/`](dev/research/README.md) | 每模块实现前的研究记录（问题/来源/结论/版本实测/风险/偏差） | 已启用；索引见 [研究记录约定与索引](dev/research/README.md)，共 20 条（18 条已落盘，`align`/`report` 进行中） |
| [`adr/`](adr/README.md) | 编号 ADR：两层判分、双模式、规则结构化、CAS 选型、文件名身份、按任务鉴权、规范无代码、GPL/Slint 许可 | 已建索引；ADR-0001–0009 均为「已接受」 |
| [`changelog.md`](changelog.md) | 按版本记录规范与行为变更 | 已建；最新条目 `0.2.3`（规范修订 + 文档初稿），`0.2.2`/`0.2.4` 规范补丁随 task-41 回填 |
| [`book/`](book/README.md) | mdBook 静态站源码与配置（GitHub Pages 目标） | 已建并可构建（mdBook 0.5.4，`scripts/docs.sh`）；`SUMMARY.md` 已收录用户手册/ADR/全部研究记录 |

## 3. 实现里程碑状态

> 里程碑定义见 `tasks/plan.md` §4；任务明细见 `tasks/todo.md`。本表只反映实现与文档现状，不代表 M10 验收通过。

| 里程碑 | 范围 | 状态 | 证据/入口 |
|---|---|---|---|
| M0 环境与骨架 | 工具链、workspace、CI、文档骨架 | 已完成 | `dev/research/toolchain.md`、`scripts/ci.sh` |
| M1 领域与基础设施 | `domain`（T04–T06）、`platform`（T07–T12 + CP-B） | 已完成 | `dev/research/domain.md`、`dev/research/platform.md` |
| M2 判分核心（fixture） | `subject-math`（T13–T14）、`grade` L1/L2（T16–T18） | 已完成 | `dev/research/subject-math.md`、`dev/research/grade.md` |
| M3 采集与识别 | `capture`（T21–T22 + D1）、`recognize`（T23–T24） | 已完成 | `dev/research/capture.md`、`dev/research/recognize.md` |
| M4 推理层 | `inference`（T19–T20） | 已完成 | `dev/research/inference.md`、`dev/research/inference-serving.md` |
| M5 规则与学科 | `rubric`（T15 + T25）、`subject-math` 全符号/CAS（T26–T27）、模板数据 v0.2 | 已完成 | `dev/research/rubric.md`、`dev/research/templates.md` |
| M6 对齐与判分全量 | `align`（T28–T29）、`grade` 全量接入（T30） | 已实现（收尾任务进行中） | `dev/research/align.md`（待落盘）、`dev/research/grade.md` |
| M7 复核与导出 | `review`（T31–T32）、`report`（T33–T34） | 已实现（review 完成；report 收尾进行中） | `dev/research/review.md`、`dev/research/report.md`（待落盘） |
| M8 服务层收口 | 端到端判分 → 复核 → 导出与 CP-F 验收 | 进行中 | 待 M6/M7 收尾后回填验收记录 |
| M9 桌面 UI | `app` 外壳（T35）、页面（T36–T38） | 进行中（外壳完成，页面未完成） | `dev/research/app.md`、`user/` |
| M10 验收与发布 | 用户手册定稿、性能基线、Pages 发布、S1–S13 实测 | 未开始 | 待 M9 完成后启动 |

## 4. 文档构建与门禁

- 构建：`scripts/docs.sh`（先从 `docs/` 生成 mdBook 源，再 `mdbook build docs/book`）。
- 链接检查：`scripts/docs-check.sh`（死链检查 + 规范围栏扫描 + mdBook 构建），纳入 `scripts/ci.sh`。
- 门禁：文档链接 0 死链；用户可见行为变更必须同步 `user/` 与 `changelog.md`；ADR 与规范变更同一次提交（`SPEC.md` §9）。
- 发布目标：GitHub Pages（构建产物 `docs/book/book/`，不提交）。

## 5. 写作约定

- 中文优先；命令与路径用行内代码；不写大段重复规范的验收条款。
- 文档可以包含少量命令示例；**规范文件内禁止任何示例代码**（`SPEC.md` §9、`CAPABILITY-MAP.md` §7）。
- 所有外部依赖结论必须能追溯到 `dev/research/` 中的来源与日期。
- 新增研究记录必须同时更新 `dev/research/README.md` 索引与 `docs/book/SUMMARY.md`，再运行 `bash scripts/docs.sh`。
