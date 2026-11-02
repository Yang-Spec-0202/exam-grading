# M10 验收方案与实测记录（S1–S14）

> 状态：验收方案 v0.1 · 2026-11-03 · **滚动记录（工作区未冻结）**
> 对应：`SPEC.md` §3（S1–S14）与 §14（审批清单）、`tasks/todo.md` T41、`CAPABILITY-MAP.md`、`CONFIG.md`
> 环境：macOS 27.0.1 / Apple M4 / 16 GB / arm64 / rustc 1.99.1 / cargo 1.99.1（§6 记录实测）
> 脚本：`scripts/acceptance/run.sh`（用法与判定语义见 `scripts/acceptance/README.md`）
> 数据纪律：**只记录真实执行结果**；未执行或不可执行的项一律标「未验证」，不编造数字；本轮无真实学生试卷，全部为代码内合成夹具与脱敏样本。

## 1. 范围、原则与术语

- **范围**：`SPEC.md` §3 的 S1–S14 全部成功标准；以及 §4 技术栈、§8 测试策略、§9 文档系统、§10 边界的可验收子集。
- **本轮定位**：M10 验收**方案 + 可执行门禁骨架**。本文件给出每条验收的「怎么做、怎么判定、当前到哪一步、证据在哪」；不是 M10 最终验收报告。
- **状态词**：
  - `已测`：该项的自动化判定已在当前工作区真实执行且结论明确（不等同于「用户场景已认可」）。
  - `部分已测`：crate/夹具级证据存在，但端到端/真实数据/人工交互未完成。
  - `待测`：方法已写明，但缺少前置数据（金标集、真实模型、UI 交互）而尚未执行。
  - `阻塞`：依赖未完成模块或不可自动化的外部条件（TCC、真实试卷、云端 TLS 等），当前无法给出结论。
- **证据优先级**：可复跑测试 > 研究记录实测 > 代码存在性；截图/日志占位必须替换为真实文件路径后才算证据。

## 2. 执行方式（一键门禁）

```bash
# 全量（代码门禁 + egctl 自检/基准）
scripts/acceptance/run.sh

# 机器可读 JSON（可同时写文件）
scripts/acceptance/run.sh --json --out /tmp/acceptance.json

# 快速回归（跳过全量测试）
scripts/acceptance/run.sh --quick --skip-bench
```

退出码：`0` = 全 PASS；`2` = 有 WARN（降级/未就绪/未接线）；`1` = 有 FAIL。步骤、判定与 WARN 语义见 `scripts/acceptance/README.md`。

## 3. S1–S14 逐条验收方案

### 3.0 覆盖总览（截至 2026-11-03 滚动快照）

| 条目 | 当前状态 | 关键依据（详见各条） |
|---|---|---|
| S1 全流程 | 阻塞 | UI 端到端未跑；无仓库级 `fixtures/` |
| S2 客观题确定性 | 待测 | 缺 500 条金标；规则/比较器夹具已有 |
| S3 解答题准确率 | 待测 | 缺 200 条金标与真实判定模型/校准 |
| S4 门禁有效 | 部分已测 | 门禁组合夹具；无真实改分数据 |
| S5 路由准确率 | 部分已测 | 路由表夹具；缺 300 题标注 |
| S6 性能 | 部分已测 | 分项实测（capture/OCR/判定/导出/启动）；端到端批次未测 |
| S7 复核效率 | 阻塞 | 需人工键盘验收（TCC 限制） |
| S8 可复现 | 部分已测 | 证据链/幂等/导出摘要已测；`egctl replay` 判分回放 TODO |
| S9 崩溃恢复 | 部分已测 | platform 真实强杀测试；端到端判分中断未测 |
| S10 隐私 | 部分已测 | crate 级 0 外联；端到端抓包未做；TLS 未实现 |
| S11 模式区分 | 部分已测 | 模式/抽检/导出夹具；UI/真实导出未人工核对 |
| S12 规则结构化 | 部分已测 | rubric 草稿/校验/模板夹具；真实模型与 UI 确认未测 |
| S13 日志与文档 | 部分已测 | `docs-check` PASS；trace 端到端未验证 |
| S14 多选部分给分 | 部分已测 | domain/rubric/grade 夹具；真实卷与导出未验证 |

### S1 全流程（20 题数学试卷 + 40 份作业，离线，无未处理崩溃）

- **目标**：参考答案 + 规则 → 导入 40 份作业 → 识别/对齐/两层判分 → 复核 → 导出，全程离线完成。
- **前置数据（夹具）**：1 份合成 20 题数学试卷（`eg-grade/tests/common` 的 19/20 题夹具可复用）；40 份合成提交（同一试卷、不同答案）；**无真实学生试卷**；`fixtures/` 仓库级目录当前不存在（见 §4-L9），用 crate 内合成夹具替代。
- **步骤**：① 建卷/答案键/规则并确认；② 批量导入 40 份（一次性/按班）；③ 观察任务队列完成与失败清单；④ 全部自动接受或转复核；⑤ 导出 XLSX/CSV/HTML；⑥ 全程断网。
- **判定**：40 份全部处理完成、无未处理崩溃；导出文件可打开、行数=40×20；失败与待复核项显式可见（允许人工，但不得静默丢题）。
- **当前状态**：`阻塞`（UI 端到端未执行；工作区并行收尾中；无仓库级夹具与真实试卷）。
- **证据位置**：分项证据：`crates/eg-grade/tests/fixtures_20.rs`、`eg-align/tests/*`、`eg-capture/tests/import.rs`、`eg-report/tests/export.rs`、`eg-app/src/pages/*`（页面代码存在，未跑 UI E2E）；`scripts/acceptance/run.sh` 的 `test` 步骤（当前 1063 passed）；**端到端运行记录仍缺**。
- **已知限制**：无真实试卷；UI 键鼠 E2E 未做；并行开发期间构建/测试有波动（§6）。

### S2 客观题确定性（500 条标注，规则判定一致率 100%）

- **目标**：有具体答案的客观题（单选/多选/判断/填空/纯数值）由确定性规则判分，与教师标注 100% 一致（OCR 高置信子集）。
- **前置数据（夹具）**：500 条标注样本（题面、答案键、学生答案、教师判定）——**当前未提供**；仅有 subject-math 比较器夹具与 grade 路由夹具。
- **步骤**：取高置信子集（识别置信度 ≥ `ocr.confidence_floor`）→ 跑 R5/R7 仅规则/规则优先路径 → 与标注逐题比对。
- **判定**：一致率 = 100%（不一致为 0 条）；比较器「无法判定」必须显式转人工，不得计为一致。
- **当前状态**：`待测`（缺 500 条标注集；规则与比较器逻辑已有夹具级证据）。
- **证据位置**：`crates/eg-subject-math/tests/{comparators_fixtures,normalize_fixtures,coverage_paths}.rs`；`eg-grade/tests/{routing_readiness,fixtures_20}.rs`；研究记录 `docs/dev/research/subject-math.md`、`grade.md`。
- **已知限制**：真实试卷的等价写法/单位/容差分布未知；标注集来源与标注规范待定。

### S3 解答题准确率（200 条，完全一致 ≥85%、半分内 ≥97%、ECE ≤0.08）

- **目标**：标准模式下解答题（计算/证明/简答）分数与教师一致，且置信度校准可用。
- **前置数据（夹具）**：200 条解答题标注（含教师给分）；真实 4B 判定模型与校准记录——**当前未提供**（doctor 显示 3 个本地模型未安装；`live_smoke` 默认 ignored）。
- **步骤**：固定模型/提示词/判定配置 → 逐题判分 → 统计完全一致率、半分内比例、ECE（置信度分桶）。
- **判定**：完全一致 ≥85%、半分内 ≥97%、ECE ≤0.08（标准模式；快速模式单独统计）。
- **当前状态**：`待测`（缺标注集与真实模型；判定逻辑已有夹具级证据）。
- **证据位置**：`crates/eg-grade/tests/fixtures_20.rs`、`modes_sampling.rs`；`docs/dev/research/models.md` §6.3（判定延迟与样例，非准确率验收）；`egctl bench` 的 `judge_question` 仍未接线（WARN）。
- **已知限制**：模型精度必须以校准集独立验收集验证；本地 4B 判定模型本轮未复测。

### S4 门禁有效（自动出分后被教师改分 ≤5%，按题，标准模式）

- **目标**：自动接受的结果质量足够高；被教师改分的比例可度量。
- **前置数据（夹具）**：真实教师复核记录 ≥ 一个班规模；门禁阈值取 `CONFIG.md` §5。当前只有夹具级门禁用例。
- **步骤**：标准模式批量判分 → 教师复核（接受/改分/转人工）→ 统计改分题数/自动接受题数。
- **判定**：改分率 ≤5%；快速模式单独统计并在导出中标注。
- **当前状态**：`部分已测`（门禁组合逻辑有夹具断言；真实改分统计未做）。
- **证据位置**：`crates/eg-grade/tests/{modes_sampling,coverage_paths}.rs`；`eg-review/tests/*`（命令与队列）；研究记录 `grade.md` §7。
- **已知限制**：无真实教师复核数据；快速模式与标准模式的统计口径需在导出侧对齐（S11）。

### S5 路由准确率（300 题标注，L1 ≥98%）

- **目标**：L1 有序路由表对「该题怎么判」的判定与人工标注一致。
- **前置数据（夹具）**：300 题标注（题型、答案键变体、规则状态、后端可用性、模式）——**当前未提供**；已有 19/20 题夹具与路由表全分支单测。
- **步骤**：逐题跑纯函数路由 → 与标注比对；R9 分类仅在其触发时调用分类后端（夹具）。
- **判定**：准确率 ≥98%；R9 分类置信度 <0.9 必须转人工。
- **当前状态**：`部分已测`（路由表逻辑与边界夹具通过；缺 300 题标注集）。
- **证据位置**：`crates/eg-grade/tests/routing_readiness.rs`（4 个用例）；`eg-grade/src/routing*`；研究记录 `grade.md`。
- **已知限制**：真实题型分布与「答案键与题型冲突」样本比例未知。

### S6 性能（单页 P50 ≤12s / P95 ≤25s；40 份×2 页 ≤25 分钟）

- **目标**：目标机 M4/16GB、0.9B OCR + 4B 判定下的端到端吞吐。
- **前置数据（夹具）**：40 份×2 页合成/脱敏提交；真实模型权重（3 个本地模型未安装，见 §4-L6）；当前分项数据来自研究记录与 `egctl bench`（debug）。
- **步骤**：① `scripts/acceptance/run.sh` 的 `egctl_bench`；② 预热模型后跑单页 50 页记录 P50/P95；③ 跑 40×2 页批次计时；④ 记录 release 构建差异。
- **判定**：单页 P50 ≤12s、P95 ≤25s；40×2 页批次 ≤25 分钟。
- **当前状态**：`部分已测`（分项链路达标证据存在；端到端批次未测）。
- **已测分项（引用研究记录/本轮 bench，不冒充端到端）**：
  - capture：12MP JPEG 单页预处理 P50 0.344s / P95 0.354s；A4@300DPI PDF 0.204s/页（release，`capture.md` §8-Q8）。
  - OCR：PaddleOCR-VL-1.6 BF16 A4@300DPI 整页 6.44s（llama.cpp 0.8.0 build 11960，`inference-serving.md` §6）。
  - 判定：4B 指令模型单题 0.95–1.55s（`models.md` §6.3，本 spike 未复测）。
  - report：1000 份×20 题导出（debug）统计构建 2524ms、单 XLSX 453ms、三格式 551ms（`egctl bench` 本轮 2026-11-03）。
  - app：冷启动 debug P50 135.6ms / P95 183.4ms；切页 P95 0.041ms；UI 启动 `egctl bench` 本轮 1168ms（`app.md` §8.1 + 本轮 bench）。
  - platform：冷启动 22.9/35.3ms、迁移 3.1ms、诊断包 51.1ms、检索 P95 0.2ms（`platform.md` §10）。
- **证据位置**：`scripts/acceptance/run.sh --skip-egctl` 之外的 `egctl_bench` 步骤；`docs/dev/research/{capture,inference-serving,models,platform,app}.md`；`crates/eg-report/tests/perf.rs`（ignored，需 `--release --ignored`）。
- **已知限制**：分项数字多来自 debug 或单机单次；GPU/前端合成未计；端到端包含模型加载与并发排队，不能由分项简单相加。

### S7 复核效率（纯键盘 ≥15 题/分钟，20 题样本）

- **目标**：复核工作台在不使用鼠标的情况下可完成接受/改分/转人工。
- **前置数据（夹具）**：20 题待复核样本 + 桌面环境（当前无 UI 键鼠 E2E；macOS TCC 拒绝自动化按键注入）。
- **步骤**：人工计时（或录屏）完成 20 题复核 → 统计题/分钟；记录错操作与快捷键冲突。
- **判定**：≥15 题/分钟；快捷方式与焦点行为符合 `SPEC-app.md` 快捷键表。
- **当前状态**：`阻塞`（需人工桌面验收；自动化按键不可行）。
- **证据位置**：`eg-review/tests/{commands,sampling_transfer}.rs`（视图模型/命令逻辑）；`eg-app/src/nav.rs`（快捷键解析单测）；`app.md` §8.2「未做」清单；人工记录待补（路径占位：`docs/dev/acceptance-evidence/S7-keyboard.md`，**尚未创建**）。
- **已知限制**：TCC/自动化限制；HiDPI、窗口缩放未验证。

### S8 可复现（历史判分按版本+哈希重放同一结果）

- **目标**：任一 `GradeRecord` 可由「答案键版本 + 规则版本 + 识别版本 + 模型版本 + 提示词哈希 + 输入哈希」重放出逐字段相同结果。
- **前置数据（夹具）**：已落库的判分记录 + 完整证据链；egctl `replay` 判分回放能力（当前 CLI 已声明 `replay`，其「判分回放」为 TODO）。
- **步骤**：选一条历史记录 → 取版本与哈希 → 在 fixture 后端重放 → 比对 `GradeRecord`（分数/判定/置信度/证据引用/引擎指纹）。
- **判定**：逐字段一致；无法重放时必须显式失败并指出缺失证据，不得伪造。
- **当前状态**：`部分已测`（crate 级证据链、幂等键、fixture 回放与导出摘要一致已测；端到端 `egctl replay` 判分回放未实现）。
- **证据位置**：`eg-grade/tests/idempotency_validation.rs`、`fixtures_20.rs`；`eg-inference/tests/service_flow.rs`（同输入 100 次回放一致）；`egctl doctor` 的 `report` 检查（双次导出摘要一致，本轮 PASS）；`egctl replay` 帮助文本明确「判分回放 TODO」。
- **已知限制**：真实模型概率的跨版本稳定性无法保证；重放依赖历史权重与提示词模板可用。

### S9 崩溃恢复（判分中强杀，重启断点续跑，不重复计费/丢分/重复成绩）

- **目标**：任务级崩溃恢复与幂等。
- **前置数据（夹具）**：真实 SIGKILL 场景（platform 已有 CP-B 集成测试）+ 端到端判分中的强杀（未做）。
- **步骤**：判分进行中强杀进程 → 重启 → 检查任务归位、幂等键、成绩/费用计数；重复两次。
- **判定**：运行中任务 100% 归位；无重复成绩、无重复计费、无丢分；审计链完整。
- **当前状态**：`部分已测`（platform 级真实强杀测试通过；端到端判分中断未测）。
- **证据位置**：`crates/eg-platform/tests/cp_b_sigkill.rs`（3 用例，研究记录 `platform.md` §10）；`egctl doctor` 单实例/DB 检查；端到端记录待补。
- **已知限制**：端到端恢复涉及 UI 重启与任务面板，需桌面环境。

### S10 隐私（断网完成 S1；抓包 0 外联，除显式授权任务）

- **目标**：默认离线；云端仅任务授权后可用；任何默认路径 0 外联。
- **前置数据（夹具）**：断网环境、抓包工具（如 `tcpdump`/Little Snitch，本轮未使用）；真实模型本地 sidecar（权重不全）。
- **步骤**：断网跑 S1；抓包观察；检查 `privacy.cloud_enabled=关` 时云端代码路径 0 请求；审计记录。
- **判定**：S1 全程无失败/降级依赖网络；除显式授权任务外 0 次外联；密钥/学生数据不进日志。
- **当前状态**：`部分已测`（crate 级：inference fixture/stub 测试不发真实网络、云端默认关；Keychain 冒烟默认忽略）；端到端断网与抓包未做；**HTTPS/TLS 在 v1 未实现**（`https://` 显式失败，见 `SPEC-inference.md`）。
- **证据位置**：`eg-inference/tests/{service_flow,platform_adapter}.rs`；`eg-platform/tests/cp_b_keychain.rs`（默认忽略）；`eg-platform` 日志脱敏测试；`SPEC.md` S10。
- **已知限制**：云端 TLS 未实现/未验证；Keychain 授权框需人工点击；抓包证据未采集。

### S11 模式区分（标准/快速分数、阈值、导出标注可分辨；快速 100% `mode=fast`）

- **目标**：两种模式在所有可见与可导出位置可区分。
- **前置数据（夹具）**：标准/快速各 ≥10 题夹具；导出文件检查；UI 模式切换（未人工验收）。
- **步骤**：同一提交分别以两种模式判分 → 检查判分记录/复核队列/导出文件/统计分组；快速模式抽检标记。
- **判定**：`mode` 字段 100% 出现；标准/快速阈值生效；快速结果 100% 带 `mode=fast` 标注；统计可分组。
- **当前状态**：`部分已测`（crate 级模式标记、阈值、抽检与导出夹具通过；UI/真实导出文件人工检查未做）。
- **证据位置**：`eg-grade/tests/modes_sampling.rs`；`eg-review/tests/sampling_transfer.rs`；`eg-report/tests/{export,stats}.rs`；研究记录 `grade.md` §7、`review.md`、`report.md`。
- **已知限制**：导出文件名/表头标注需在桌面端打开确认；`fast.export_label=关` 时的「仅去标注、元信息仍可分辨」需人工核对。

### S12 评分规则结构化（10 条手写规则，教师确认率 100%，静默校验通过）

- **目标**：教师自然语言规则经模型整理为草稿，教师逐条确认后生效；未确认 0 条生效。
- **前置数据（夹具）**：10 条教师手写规则（当前 `eg-rubric/tests/handwritten_rules.rs` 有夹具）；真实 structurer 模型（4B，权重不全/未复测）；规则确认 UI。
- **步骤**：录入 10 条规则 → 模型整理生成草稿 → 静态校验 → 教师确认/拒绝 → 检查生效版本与审计。
- **判定**：确认率 100%（无一条未经确认生效）；每份草稿通过静态校验或在确认前被显式拦下；审计含模型调用 id/提示词哈希。
- **当前状态**：`部分已测`（rubric 草稿/整理/校验/确认逻辑与模板夹具通过；真实模型整理与 UI 确认流程未验收）。
- **证据位置**：`eg-rubric/tests/{handwritten_rules,structurer,templates,partial_choice}.rs`；`eg-domain` 草稿类型测试；研究记录 `rubric.md` §4。
- **已知限制**：真实模型整理质量（拒绝率/一次通过率）未测；模型整理 Schema 与 inference 当前输出形状需 app 适配（`rubric.md` R6/D9）。

### S13 日志与文档（trace 全链路；docs 齐全 0 死链；每模块 ADR + 用户手册章节）

- **目标**：每次判分可按 `trace_id` 还原链路；文档系统完整可构建。
- **前置数据（夹具）**：完整一次判分（S1 未完成，trace 端到端不可得）；现有文档与测试。
- **步骤**：跑一次判分 → 用 `trace_id` 拉日志（app 日志查看器/`egctl diagnostics export`）→ 核对任务/模型调用/DB/UI 事件；文档链接检查与 mdBook 构建。
- **判定**：trace 链路字段齐全、可还原；`scripts/docs-check.sh` 通过（0 死链、规范 0 代码围栏、mdBook 成功）；用户手册覆盖 10 页；每模块有 ADR 或研究记录。
- **当前状态**：`部分已测`（`docs-check` 本轮 PASS；platform 日志/审计/诊断/索引测试 130+；ADR 0001–0009 与 8 篇用户手册存在；**端到端 trace 与 app 日志查看器人工验收未做**）。
- **证据位置**：`scripts/docs-check.sh`（本轮通过：15 个规范文件 0 围栏、57 条链接 0 死链、mdBook 构建成功）；`eg-platform/src/{logging,audit,diagnostics}.rs` + 测试；`docs/dev/research/platform.md` §8；`docs/user/*.md`、`docs/adr/*`。
- **已知限制**：app 日志查看器 UI（T38）与诊断包导出未人工验收；trace 全链路依赖端到端运行。

### S14 多选部分给分（按档位正确、错选归零、缺档转人工不默认 0）

- **目标**：多选题按 `Rubric.partial_choice` 档位给分；错选 0；缺档/非法档位转人工。
- **前置数据（夹具）**：2/3 正确选项两种口径的模板/规则数据（已有模板夹具）；真实多选答卷（未提供）。
- **步骤**：构造 2 选项/3 选项、无错选命中 1/2 档、含错选、缺档、跳档、模式不一致等样本 → 判分 → 核对得分与判定（`Judgement::Partial` 的 `satisfied/missing` 用 `JudgedUnit`）。
- **判定**：档位得分与规则数据逐项一致；错选一律 0 且判「不一致」；缺档/跳档/模式不一致 100% 转人工，**不得默认 0 分**。
- **当前状态**：`部分已测`（domain 不变量与 rubric 档位校验、grade 聚合夹具通过；真实答卷与导出展示未验证）。
- **证据位置**：`eg-domain` 部分给分不变量测试；`eg-rubric/tests/partial_choice.rs`（4 用例）；`eg-grade/tests/fixtures_20.rs`；`docs/dev/research/{domain,rubric,grade}.md`。
- **已知限制**：官方口径随年份变化，真实卷需模板 `source_note` 可追溯；导出/界面展示档位扣分说明未人工验收。

## 4. 未验证 / 阻塞清单（M10 前必须关闭或明确接受）

| # | 项 | 状态 | 原因/影响 | 关闭条件 |
|---|---|---|---|---|
| L1 | 中文 IME 输入 | 未验证（阻塞） | macOS TCC 拒绝自动化按键；Slint 无 headless IME 模拟 | `SPEC-app.md` §7.1 五项手工清单签字；记录路径占位 `docs/dev/acceptance-evidence/`（未创建） |
| L2 | 真实试卷 OCR 精度 | 未验证（阻塞） | 无真实学生卷；R1–R3 需 50/30/40 页金标 | 提供脱敏真实卷 + 标注；跑 `recognize` 夹具与真实模型 |
| L3 | 云端 TLS | 阻塞（未实现） | v1 内置传输无 TLS，`https://` 显式失败 | 立项评估 TLS crate/许可（Ask first）后实现 + 安全测试 |
| L4 | Keychain 授权框 | 未验证 | 需真实桌面点「允许」；`cp_b_keychain` 默认 ignored | `EXAM_GRADING_KEYCHAIN_SMOKE=1` 人工冒烟并记录 |
| L5 | HEIC / DOCX | 已确认不支持（v1 限制） | capture v1 显式拒绝（`heic_unsupported`/`unsupported_format`） | 若启用需新增依赖并审批（`SPEC-capture.md` v0.2.4） |
| L6 | 真实模型权重 | 未就绪 | doctor：4/7 本地模型已安装、3 个未安装（约 6.3 GiB 已安装）；`live_smoke` ignored | `scripts/model/pull.sh` 下发后跑 `cargo test -p eg-inference --test live_smoke -- --ignored` |
| L7 | 真实判分模型精度/校准 | 未验证 | 4B 判定未复测；无校准集 | 独立校准/验收集 + `grade` 校准链路 |
| L8 | UI 键鼠 / HiDPI / 深色模式 | 未验证 | 需桌面人工；app 研究记录 §8.2 已列「未做」 | 手工验收清单 + 截图/录屏证据 |
| L9 | 仓库级 `fixtures/` 与 `docs/dev/performance.md` | 缺失 | `SPEC.md` §6 期望 `fixtures/`；T39 要求性能基线文档 | 补目录与性能文档（M10 任务） |
| L10 | 端到端断网抓包 / 判分中断恢复 / 40×2 页性能 | 未验证 | 依赖 S1 与真实模型 | S1 可跑后按 §3 步骤执行并记录 |

## 5. 验收记录模板（每次执行复制一行）

| 日期时间 | 触发 | 环境/版本 | 结果摘要 | 证据位置 | 结论 |
|---|---|---|---|---|---|
| 2026-11-03T01:16+08:00 | `scripts/acceptance/run.sh --json` | macOS 27.0.1 / M4 / rustc 1.99.1 | 见 §6（WARN：仅 egctl 就绪度） | `/tmp/acceptance-final.json`（临时） | 滚动基线，非验收结论 |
| 待填 |  |  |  |  |  |

记录要求：附 `run.sh --json` 输出（或 `--out` 文件）与失败日志路径；人工项附截图/录屏；不得只写「通过」。

## 6. 冻结基线实测记录（2026-11-03T01:37+08:00，交付冻结）

> 本轮为**交付冻结前最后一次全量真实执行**：`bash scripts/acceptance/run.sh --json --skip-egctl --out /tmp/acceptance-frozen.json`。工作区在提交 `d2fc2a8`（core）/`dd1ca2c`（scripts+data）前后一致，无并行写入。历史滚动：00:36 全量 1015 passed → 01:03 1063 passed → 01:16 1063 passed → **01:37 冻结 1063 passed（本表）**。

环境：macOS-27.0.1-arm64 / arm64 / rustc 1.99.1 (58cf8f9c3 2026-10-13) / cargo 1.99.1 (5f94df478 2026-08-27)。

| 步骤 | 状态 | 退出码 | 耗时 | 摘要 |
|---|---|---|---|---|
| fmt | PASS | 0 | 538 ms | 所有文件已格式化 |
| clippy | PASS | 0 | 632 ms | 0 warning / 0 error（`--all-features`） |
| test | PASS | 0 | 238924 ms | **1063 passed / 0 failed / 2 ignored**（ignored：`eg-inference` `live_smoke` 需真实权重；`eg-report` `perf` 需 `--release --ignored`） |
| deny | PASS | 0 | 2166 ms | advisories ok、bans ok、licenses ok、sources ok |
| docs-check | PASS | 0 | 296 ms | 15 个规范文件 0 代码围栏；57 条相对链接 0 死链；mdBook 构建成功 |
| egctl_doctor | SKIP | 0 | 0 ms | `--skip-egctl` 跳过（就绪度提示见上一轮滚动记录：未装模型/未接线属预期 WARN） |
| egctl_bench | SKIP | 0 | 0 ms | `--skip-egctl` 跳过（同上） |
| **总体** | **PASS** | **0** | 242708 ms（约 243 s） | pass=5 / warn=0 / fail=0 / skip=2（egctl 两项按参数跳过） |

补充观察：

- 本轮为冻结基线：代码门禁 5/5 PASS、0 WARN、0 FAIL；egctl 两项因 `--skip-egctl` 跳过，未计入 WARN。
- 验收 JSON：`/tmp/acceptance-frozen.json`（临时路径；归档时复制到交付证据目录）。各步骤原始日志路径记录在该 JSON 的 `steps[].log` 字段。
- `cargo test --workspace` 的 2 个 ignored 用例为**设计内跳过**（需真实模型权重 / 需 release 性能模式），非缺陷。
- egctl 的 doctor/bench 就绪度 WARN 语义见 `scripts/acceptance/README.md`；UI 端到端（S1/S7）与真实试卷/金标（S2/S3/S5）仍是阻塞项，见 §7 与 §3 各条。

## 7. 结论与下一步

1. **当前仍不满足「M10 验收通过」**：截至最新滚动快照，代码门禁（fmt/clippy/test/deny/docs）已全 PASS（test 1063 passed / 0 failed），但 S1–S14 中仅 S4/S5/S6/S8/S9/S10/S11/S12/S13/S14 具备部分自动化证据；**S1（全流程）、S7（键鼠效率）、S10 端到端抓包、S2/S3 金标集与 L1–L10 阻塞项未关闭**。
2. **门禁脚本已可用**：`scripts/acceptance/run.sh` 已在本机真实执行并输出 JSON/TSV；最新一次总体为 **WARN（exit 2）**，WARN 仅来自 `egctl doctor/bench` 的就绪度提示（未接线/模型未装齐），代码门禁 0 失败。
3. **必须先冻结工作区**（并行任务全部合入、四个基础门禁稳定）后重跑脚本，并把结果按 §5 模板写入本文件；冻结前所有数字仅作滚动基线。
4. **建议关闭顺序**：L6 模型权重 → S1 端到端合成夹具 → L10 抓包/崩溃恢复 → S2/S3 金标集 → L1/L4/L8 人工 UI → L2 真实试卷 → L3 TLS（Ask first）。
5. **夹具/性能文档缺口**（L9）应由 M10 任务补齐：`fixtures/`（合成试卷、页面、预期结果）与 `docs/dev/performance.md`（T39 基线汇总；在此之前性能数字以 §3-S6 与研究记录为准）。
