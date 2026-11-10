# exam-grading 文档系统

> 状态：随实现更新（2026-11-05）。M0–M8 已完成，M9 部分完成（UI 人工验收待办），M10 进行中：S1 backend 自动化已 PASS（`egctl acceptance`，83 断言/6 负向）；S3 合成金标 120/120 三项达标（总分精确 94.17%、半分内 100%、ECE 0.0557/校准 0.0607；真实教师标注/真实试卷仍待办，status 依赖 calibration_id 接入）；S2/S5/S7、capture 多页归组（task-52）与人工验收待办。最新变更见 [`changelog.md`](changelog.md) `[0.3.6]`。
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
| [`user/`](user/README.md) | 快速开始、命名约定、双模式说明、复核操作、评分规则、导出说明、常见故障、隐私说明 | M10 初稿（9 篇 + 旧链接页 `quickstart.md`）；`quick-start.md` 已补「仓库自带演示夹具」S1 后端自测步骤；UI 截图与人工验收待补 |
| [`dev/`](dev/README.md) | 架构总览、验收记录、性能基线、发布与回滚、故障排查 | 架构总览、[验收记录](dev/acceptance.md)、[性能基线](dev/performance.md) 已落盘；modules/data-model/fixtures/release/troubleshooting 随 M10 补齐 |
| [`dev/research/`](dev/research/README.md) | 每模块实现前的研究记录（问题/来源/结论/版本实测/风险/偏差） | 已启用；索引见 [研究记录约定与索引](dev/research/README.md)，共 21 条模块记录全部落盘（含 `align`/`report`/`app`/`egctl`） |
| [`adr/`](adr/README.md) | 编号 ADR：两层判分、双模式、规则结构化、CAS 选型、文件名身份、按任务鉴权、规范无代码、GPL/Slint 许可 | 已建索引；ADR-0001–0009 均为「已接受」 |
| [`changelog.md`](changelog.md) | 按版本记录规范与行为变更 | 最新 `0.3.0`（fixtures、S1 自动化、app 真实导入/报表联动/图像证据、inference fixture-key）；历史 `0.2.1`–`0.2.4` 保留 |
| [`book/`](book/README.md) | mdBook 静态站源码与配置（GitHub Pages 目标） | 已建并可构建（mdBook 0.5.4，`scripts/docs.sh`）；`SUMMARY.md` 已收录用户手册/ADR/全部研究记录 |

## 3. 实现里程碑状态

> 里程碑定义见 `tasks/plan.md` §4；任务明细见 `tasks/todo.md`。本表只反映实现与文档现状，不代表 M10 验收通过。

| 里程碑 | 范围 | 状态 | 证据/入口 |
|---|---|---|---|
| M0 环境与骨架 | 工具链、workspace、CI、文档骨架 | 已完成 | `dev/research/toolchain.md`、`scripts/ci.sh` |
| M1 领域与基础设施 | `domain`（T04–T06）、`platform`（T07–T12 + CP-B） | 已完成 | `dev/research/domain.md`、`dev/research/platform.md` |
| M2 判分核心（fixture） | `subject-math`（T13–T14）、`grade` L1/L2（T16–T18） | 已完成 | `dev/research/subject-math.md`、`dev/research/grade.md` |
| M3 采集与识别 | `capture`（T21–T22 + D1）、`recognize`（T23–T24） | 已完成（fixture OCR） | `dev/research/capture.md`、`dev/research/recognize.md` |
| M4 推理层 | `inference`（T19–T20） | 已完成（fixture 后端 + 许可/授权） | `dev/research/inference.md`、`dev/research/inference-serving.md` |
| M5 规则与学科 | `rubric`（T15 + T25）、`subject-math` 全符号/CAS（T26–T27）、模板数据 | 基本完成（模型整理/真实 CAS 未接） | `dev/research/rubric.md`、`dev/research/templates.md`、`dev/research/cas.md` |
| M6 对齐与判分全量 | `align`（T28–T29）、`grade` 全量接入（T30） | ✅ 已完成（capture 多页归组交付，外部夹具端到端 PASS） | `dev/research/align.md`、`dev/research/grade.md` |
| M7 复核与导出 | `review`（T31–T32）、`report`（T33–T34） | 已完成 | `dev/research/review.md`、`dev/research/report.md` |
| M8 服务层收口 | 端到端判分 → 复核 → 导出与 CP-F 验收 | ✅ 已完成（CP-F 通过：外部夹具 S1 287 断言/0 失败/9 负向/12 步） | `dev/acceptance.md` §S1、`egctl acceptance` |
| M9 桌面 UI | `app` 外壳（T35）、页面（T36–T38）、真实导入/报表联动 | 🟡 部分完成（外壳/页面/真实导入/导出联动已完成；UI 人工验收未完成：键盘 E2E/IME/真实重判/redo） | `dev/research/app.md`、`user/` |
| M10 验收与发布 | 用户手册定稿、性能基线、Pages 发布、S1–S14 实测 | 进行中（S1 backend 完成；S2/S5/S7 与 Pages 待办；**S3 附注（2026-11-10）**：合成金标 120/120 上 S3 三项达标（总分精确 94.17%、半分内 100%、ECE 0.0557/校准 0.0607）；真实教师标注/真实试卷仍待办；status 依赖 calibration_id 接入未完成（**M10 仍 🟡，不据此判 M10 通过**）） | `dev/acceptance.md`、`dev/performance.md` |

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
