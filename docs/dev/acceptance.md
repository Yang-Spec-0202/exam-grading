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
scripts/acceptance/run.sh --json --out <output-dir>/acceptance.json

# 快速回归（跳过全量测试）
scripts/acceptance/run.sh --quick --skip-bench
```

退出码：`0` = 全 PASS；`2` = 有 WARN（降级/未就绪/未接线）；`1` = 有 FAIL。步骤、判定与 WARN 语义见 `scripts/acceptance/README.md`。

## 3. S1–S14 逐条验收方案

### 3.0 覆盖总览（截至 2026-11-03 滚动快照）

| 条目 | 当前状态 | 关键依据（详见各条） |
|---|---|---|
| S1 全流程 | 已自动化（backend） | `egctl acceptance --fixtures fixtures/paper-20q` 全链路 PASS（287 断言/9 负向/12 步骤，含外部夹具 capture 归组 + OCR/judge 回放 + expected 逐题断言）；UI 键鼠 E2E 仍待办 |
| S2 客观题确定性 | 已测（合成金标） | `egctl eval --gold fixtures/gold`：540 客观题，规则分数精确一致 100%、状态一致 100%、误给 0 = 0（2026-11-09） |
| S3 解答题准确率 | 已测（合成金标 + 真实 4B 采样） | fixture/规则基线：逐项 100%、总分 100%、半分内 100%（ECE 0.0933 仅规则标签）；真实 qwen3.5-4b Q4 采样 60 条：逐项 77.2%、总分精确 56.7%、半分内 83.3%、真实 ECE 0.1778、status 20%（无 calibration_id） |
| S4 门禁有效 | 部分已测 | 门禁组合夹具；无真实改分数据 |
| S5 路由准确率 | 已测（合成金标） | 770 题（540+230）：L1 规则命中 100%、route 标签 100%（阈值 ≥98%） |
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

- **目标**：参考答案 + 规则 → 导入作业 → 识别/对齐/两层判分 → 复核 → 导出，全程离线完成。
- **自动化状态**：`已自动化（backend）`。`egctl acceptance --fixtures fixtures/paper-20q` 一条命令跑通 12 个步骤：
  临时数据根 + migrations → capture（真实 PNG/JPEG、原生多页归组、重复字节去重、EXIF 90°）→
  recognize（FixtureCatalog 回放外部 OCR 夹具 + 负向拒绝）→ align（外部模板/名单/OCR → 逐题区域）→
  grade（fixture judge + expected 逐题断言）→ review（改分 + 撤销 + 抽样只读负向）→
  report（XLSX/CSV/HTML + 导出记录 + 重放摘要一致）→ 内置链路与外链 4 个 external-* 步骤。
  2026-11-05 实测：**287 断言 / 9 负向 / 0 失败 / 4 条非阻断 warning**，PASS。
  **UI 键鼠 E2E（S7）仍为人工待办**；本命令不启动 Slint 桌面、不依赖真实模型/网络。
- **前置数据**：仓库级 `fixtures/paper-20q`（`paper.json`/`rubric.json`/`template.json`/`students.json`/
  `expected/grades-expected.json`）+ `fixtures/images`（3 学生×2 页、重复字节、EXIF、名单外相机文件名）+
  `fixtures/ocr`（7 页记录 + 2 负向）+ `fixtures/judge`（8 条正常/低置信/冲突记录 + 1 负向）。
- **命令**：

  `egctl acceptance --fixtures fixtures/paper-20q --data-dir <临时目录> --json --out <output-dir>/acceptance-s1.json`

  退出码：`0` = PASS，`1` = FAIL；`--json` 输出步骤/断言数/失败/负向用例/产物路径；`--out` 另写 JSON 文件。
- **步骤（12 步，全部真实执行）**：
  ① env：临时数据根 + migrations（schema v3）+ 审计链 + 配置校验 + 基础行落库；
  ② fixtures：外部夹具只读契约校验（20 题/150000 毫元/3 学生/规则 active/期望 3×20/图片/OCR/judge 索引）；
  ③ capture（内置链路）：真实 PNG 导入 + 重复字节 + 名单外；
  ④ recognize（内置链路）：scripted OCR + 畸形输出负向；
  ⑤ align（内置链路）：文件名身份 + 区域/答案；
  ⑥ grade（内置链路）：内置 20 题夹具逐题路由/判定/得分/状态；
  ⑦ review：改分/撤销/抽样只读负向；
  ⑧ report：三格式导出 + 导出记录 + 重放摘要一致；
  ⑨ **external-capture**：`fixtures/images/*` 交给真实 `eg-capture`；`ImportReport.groups` 原生归组为
     5 组（3 名学生的 2 页组 + EXIF 单页 + 名单外单页，共 8 页；重复字节同批跳过）；
  ⑩ **external-recognize**：`FixtureCatalog` 按（student_no, page_no）语义回放 7 页 OCR 输出，
     bbox 按 OCR 输入/页高比例还原为页归一化坐标；负向（缺 blocks、barcode 块）必须被拒绝；
  ⑪ **external-align**：外部模板/名单/OCR 跑 `eg_align::align`（模板路径）；身份/区域/答案；
     名单外相机文件名必须未决；
  ⑫ **external-grade**：按 `expected/grades-expected.json` 逐学生逐题断言（185 断言）：
     - 总分：张三 132000、李四 77000、王五 109000（毫元）；
     - Q9–Q11 李四部分给分 3000/2000/3000；Q17 李四低置信 → `needs_review(low_confidence)`；
       Q16 王五「规则一致/模型不一致」冲突 → `needs_review(conflict)`；Q19 仅规则/分步；
       Q20 全部 `manual`（`answer_key_omitted`）；名单外身份未决；
     - judge 用例回放 `fixtures/judge` 的 normal/low_confidence/conflict/negative；Q16 李四/王五、Q17、Q19
       命中夹具；Q15、Q16 张三、Q18 属夹具覆盖缺口（expected `evidence=rule`），使用确定性规则回退，
       并在 warning 中如实标注（不伪造夹具命中）。
- **产物**：临时数据根下 migrations/DB、导出文件（XLSX/CSV/HTML）、审计记录；`--out` JSON；
  失败时 JSON `failures` 给出断言 id + 实际/期望定位。
- **已知限制（如实保留）**：
  1. 规模：端到端自动化夹具为 3 名学生 × 20 题；S1 目标的 40 份批量规模与真实学生试卷未执行。
  2. 真实模型未参与：OCR/judge 使用 fixture 后端（CI 可重复的前提）；真实模型精度见 S2/S3。
  3. UI 键鼠 E2E（导入拖拽→复核工作台→导出按钮）仍为人工验收（S7）。
  4. 链路经 crate 级端口直接驱动，不经过 `eg-app` 组合根的任务队列/取消/断点；external 链路
     未把 align/grade 结果持久化到 DB（组合根 TODO）。
  5. 夹具覆盖缺口：judge 仅 8 条（Q16 李四/王五、Q17、Q19），Q15/Q16 张三/Q18 规则回退；
     OCR 生成器 bbox 存在二次缩放（已由 acceptance 适配层还原，生成器 TODO）；逐题置信度按
     OCR 块/页级置信度适配（`TODO(fixtures)`），不等于真实识别置信度分布。
  6. expected 与 grade 路由在「Q16 李四」上存在口径差异：expected 要求模型 partial 覆盖规则
     mismatch（否则规范 §8 会判冲突）；acceptance 仅在「expected evidence=judge 且该题有 judge
     夹具」时把具体键降级为 rubric-only，不改 expected/夹具文件。

### S2 客观题确定性（540 条合成金标，规则判定一致率 100%）

- **目标**：有具体答案的客观题（单选/多选/判断/填空/纯数值）由确定性规则判分，与标注 100% 一致（OCR 高置信子集）。
- **前置数据**：仓库级 `fixtures/gold/objective-500.json`（540 条，含具体答案键/题型/扰动/期望；expected 由规范规则独立推导，见 `fixtures/gold/README.md`）；**全部为合成数据，非真实试卷**。
- **命令**：`egctl eval --gold fixtures/gold --json`（全量 objective 540 + solution 230；CI 子集 `cargo test -p egctl --test eval_gold` = 60 + 30）。
- **步骤**：按 `expected.route` 分流；rule_only/rule_then_model 用 `eg-subject-math` 归一化 + 比较器链（多选部分给分用 `eg-grade::aggregate::partial_choice`）；与 expected 比对 normalized/judgement/score/status；`手动`只断言路由/状态/原因。
- **实测（2026-11-09，fixtures/gold v1）**：540 条，**score 精确一致 100%**、normalized 100%、judgement 100%、status 100%、auto/review/manual = 410/38/92（75.9%/7.0%/17.0%）、**误给 0 = 0**、多选部分给分档位 33/33=100%、路由规则命中 100%。失败样本 0。
- **判定**：规则分数/判定/状态一致率 100%（高置信非 manual 子集）；比较器「无法判定」必须显式转人工，不得计为一致；「不应给 0 却给 0」必须为 0。
- **当前状态**：`已测（合成金标）`；真实试卷分布、真实 OCR 噪声与教师标注仍未测。
- **证据位置**：`crates/egctl/src/eval.rs`、`crates/egctl/tests/eval_gold.rs`；金标自检 `python3 scripts/fixtures/verify_gold.py`；规范 `SPEC-grade.md` §4/§5、`SPEC-subject-math.md`。
- **已知限制**：合成题面/答案与扰动分布；expected 由规则推导（非模型标注），只能证明「实现与规则推导一致」，不能证明教学正确性或真实模型精度。

### S3 解答题准确率（230 条合成金标，完全一致 ≥85%、半分内 ≥97%、ECE ≤0.08）

- **目标**：标准模式下解答题（计算/证明/简答）分步给分与教师一致，且置信度校准可用。
- **口径**：fixture/规则基线 = 用 gold 的合成 `expected.steps[].matched` 作 judge 输入（测实现聚合）；真实模型 = 本地 4B judge 逐评分项请求，模型 confidence 重算 ECE。**两者不可混用**。

| 分支 | 命令/样本 | 逐项一致 | 总分精确 | 半分内 | status 一致 | ECE |
|---|---|---|---|---|---|---|
| fixture/规则基线（2026-11-09） | `egctl eval --gold fixtures/gold`；230 条 / 675 评分项 | 675/675=**100%** | **100%** | **100%** | **100%** | 0.0933（规则标签，不可判定） |
| 真实 4B（2026-11-10） | `egctl eval --gold fixtures/gold --real-judge --judge-endpoint http://127.0.0.1:8082/v1 --model qwen3.5-4b-instruct --sample 60 --concurrency 3 --json --out target/acceptance/eval-real-judge-sample60-20261109.json`；60/225 条（抽样，跳过 manual 5）/180 次评分项请求 | 139/180=**77.2%**（Wilson 95% CI 70.6–82.7%） | **56.7%**（34/60） | **83.3%**（50/60） | **20%**（12/60；模型无 calibration_id，门禁全 review） | **0.1778**（n=180；fixture 基线 0.0933） |

- **真实模型附加结果（sample=60，seed=20261109，按 perturbation×expected.status 分层）**：
  - 服务：llama.cpp build `b11960-8642541b2`，qwen3.5-4b-instruct Q4_K_M，4 slots，temperature=0、max_tokens=256、stream=false、json_object、timeout=20s；thinking 关。
  - 失败/超时：**0**（180 次调用全部通过 `JudgeOutput::validate`）；无失败分类样本。
  - 混淆矩阵（expected↓/actual→）：expected match → match 24 / mismatch 5 / partial 6；expected mismatch → mismatch 2；expected partial → mismatch 14 / partial 9。无 expected mismatch 被模型判 match（未出现「错答给满分」），但大量 expected partial 被判 mismatch（漏给分）。
  - 延迟：mean **7211 ms**、P50 **7276 ms**、P95 **9103 ms**（**concurrency=3**，含排队；串行 smoke 约 2.5–2.6 s/评分项，首次 prefill 约 23 s；prompt cache 会掩盖真实成本）。
  - token：prompt 57,777（mean 963/次）、completion 8,612（mean 143.5/次）、total 66,389。
  - 全量命令：把 `--sample 60` 改 `--sample 0`（225 条 / ~675 次请求；单实例串行约 40–60 min，建议 2–3 并发并按本表口径标注并发）。
- **判定**：fixture 基线完全一致 100%（达标 ≥85%、半分内 100% 达标 ≥97%）；真实 4B 本次 **未达** S3 目标（总分精确 56.7% <85%，半分内 83.3% <97%，真实 ECE 0.1778 >0.08）。**如实记录，不调参、不改 gold/实现凑数**；status 20% 主要由「模型无 calibration_id → 门禁全部 needs_review」导致，非准确率指标本身。
- **当前状态**：`已测（合成金标）` + `真实 4B 采样已测（未达标）`；真实手写解答/真实教师标注仍缺。
- **证据位置**：`crates/egctl/src/eval.rs`（fixture 基线）、`crates/egctl/src/eval_real.rs`（真实 judge）、`crates/egctl/tests/eval_gold.rs`、`crates/egctl/tests/eval_real_judge.rs`（`#[ignore]`）；结果 JSON `target/acceptance/eval-real-judge-sample60-20261109.json`；`fixtures/gold/README.md` §0/§2.2；`docs/dev/research/models.md` §11.5。
- **已知限制**：gold 是合成规则标签（非真实教师标注）；真实模型仅 60 条抽样（`expected.steps[].matched` 不是模型标注）；无校准集/calibration_id，ECE 仅按模型自报 confidence 分桶（180 项，分桶不足标记 0）；延迟含并发排队与 prompt cache；不覆盖 OCR 链路/手写。

### S3 校准与提示词改进（2026-11-10，task-72）

- **命令**：
  `egctl eval --gold fixtures/gold --real-judge --sample 60 --calibrate --ab --ab-sample 30 --concurrency 3 --json --out target/acceptance/eval-real-judge-calibrated-20261110.json`
- **拆分（固定 seed=20261109，分层 perturbation×status，互斥无泄漏）**：calibration **60 条** / eval **60 条** / A/B **30 条**（总 eligible 225，跳过 manual 5）；共 180 条 entry、540 次评分项请求（实际成功 535，v2 有 5 次失败）。
- **校准方法**：`equal-frequency bins + PAV 单调合并`（Rust 实现，无新依赖）；校准 ID **`qwen35-4b-q4-iso-v1`**（写入报告/eval 输出；`JudgeOutcome` 结构暂未含 calibration_id，接入需规范扩展）。
- **只在校准集拟合、仅在 eval 集评估**（eval 60 条 / 180 评分项）：
  | 指标 | 校准前（raw） | 校准后 |
  |---|---|---|
  | ECE（评分项 confidence vs 正确率） | **0.1056** | **0.0667**（改善，低于 0.08 阈值） |
  | Brier | **0.1425** | **0.1358**（轻微改善） |
  | 可靠性表 | conf 0.9–1.0：n=180，avg_raw=0.950，acc=0.844 | 校准概率 0.7–0.8：n=180，avg_cal=0.778，acc=0.844 |
  | 逐项 strict / 总分精确 / 半分内 / status | 84.4% / 63.3% / 90.0% / 23.3% | 同左（校准只改 confidence/status，不改判定） |
  | partial 召回（entry） | 34.8%（8/23） | 34.8% |
  - 校准表（拟合 180 评分项）：1 桶（所有原始 confidence=0.95 并列；PAV 后 accuracy=0.778）；因校准概率 0.778<0.80，status 仍全 needs_review（无 calibration_id 语义接入），status 一致率未改善。
  - **结论**：校准把 ECE 从 0.1056 降到 0.0667（达标），但 Brier 仅轻微改善；判定准确率不变，距离 S3 总分目标（≥85%）仍远。
- **提示词 v2 A/B（同 30 条，配对后 25 条双方成功）**：v2 明确「关键要素等价即 match；部分满足→partial 且给出 satisfied/missing；只有关键结果错误才 mismatch」并给逐项判据。
  | 指标 | v1 baseline | v2 |
  |---|---|---|
  | 逐项 strict | 90.7%（68/75） | **100%**（75/75） |
  | partial 召回（entry） | 37.5%（3/8） | **100%**（8/8） |
  | 总分精确 | 72.0% | **100%** |
  | mismatch→match（必须 0） | **0** | **0** |
  | ECE / Brier | 0.0433 / 0.0865 | 0.0500 / 0.0025 |
  - v2 失败：**5/30**（3 次 `partial` 缺 satisfied/missing 的 schema_violation、2 次 timeout）；v1 0；因此 A/B 仅 25 条配对，v2 的 100% 为「配对成功子集」结果，存在选择偏差，**不宣称整体 100%**。
  - 耗时/成本：总 **30m42s**（并发 3；超 20 分钟预算，如实记录）；calibration+eval 360 次调用 token：prompt 116,412 / completion 17,173 / total 133,585（A/B 的 180 次调用未计入该 token 汇总）。
- **验收判定**：校准 ECE 达标（0.0667≤0.08），但 S3 准确率仍未达标（eval 总分精确 63.3%<85%、半分内 90%<97%）；v2 在配对子集上显著改善 partial 召回且 mismatch→match=0，但失败率/样本小，需全量复测才能下结论。**不调参、不改 gold/实现**。
- **证据**：结果 JSON `target/acceptance/eval-real-judge-calibrated-20261110.json`；代码 `crates/egctl/src/eval_real.rs`（Calibrator/PAV、`run_calibrated`、PromptVersion v2）；单元测试 `calibrator_is_monotonic_and_reduces_overconfidence`/`pav_pools_violating_bins`；真实模型测试 `crates/egctl/tests/eval_real_judge.rs`（`#[ignore]`）。
- **限制**：gold 合成标签；60/60/30 样本小；v2 5 次失败导致配对偏差；校准集与 eval 分布偏移（校准 77.8% vs eval 84.4%），单桶校准只处理 confidence=0.95 的并列；status 受 calibration_id 未接入影响；未跑全量 225 条。


### S3 v1 prompt + 80/120 重校准（2026-11-10，task-73；**原误标 v2，task-75 更正**）

> **更正（task-75）**：本节数字实际使用 **v1 prompt**（当时 `--prompt v2` 未接线，JSON 的 split.prompt_version 误标 v2）。
> 以下数字保留为 **v1 + 80/120 拆分** 参考，不再作为 v2 结果；真实 v2 见下一小节。

- **命令**（当时实际执行 v1；命令中的 `--prompt v2` 被忽略）：
  `egctl eval --gold fixtures/gold --real-judge --sample 80 --eval-sample 120 --calibrate --prompt v2 --concurrency 4 --json --out target/acceptance/eval-real-judge-v2-calibrated-20261110.json`
- **拆分/预算**：固定 seed=20261109 分层互斥，calibration **80 条** / eval **120 条**（eligible 225，跳过 manual 5）；**实际 v1 prompt**，600 次评分项请求、0 失败/0 超时；耗时 21m29s、token prompt 194,498 / completion 28,386 / total 222,884（并发 4）。
- **v2 schema 硬化**：prompt 明确 partial 必须带 `satisfied/missing` 列表，并实现「首次 partial 缺列表 → 1 次机械修复重试，严格校验不变，仍失败计入失败」。**本节是 v1 运行，未触发 v2 修复路径**（repair=0）；真实 v2 修复验证见下一小节。
- **v1 重校准（仅 calibration 80 拟合，仅 eval 120 评估）**：等频分桶 + PAV；校准表 1 桶（原始 confidence 全为 0.95，accuracy=0.7958，单桶局限如实标注）。
  | 指标 | raw | 校准后 |
  |---|---|---|
  | ECE（评分项 n=360） | **0.0833**（未达 ≤0.08） | **0.0708（达标）** |
  | Brier | 0.1225 | 0.1206 |
  | 可靠性 | conf 0.9–1.0：n=360，avg=0.950，acc=0.8667 | 校准后 0.7–0.8：n=360，avg=0.7958，acc=0.8667 |
- **eval 120 条指标（v1 raw）**：
  - 逐项 strict/lenient **86.67%**（312/360）；总分精确 **65.83%**（79/120）；半分内 **95.00%**（114/120）；status **23.33%**（无 calibration_id，门禁全 review）。
  - partial 召回（gold partial→actual partial）**42.22%**（19/45）；**mismatch→match = 0**（3 条 expected mismatch 全判 mismatch）。
  - 混淆矩阵（expected↓/actual→）：match→match 57 / partial 15；mismatch→mismatch 3；partial→mismatch 26 / partial 19。
- **与 v1/目标对比**：
  - v1 参考（task-72，**不同 eval 抽样**）：逐项 84.4%、总分精确 63.3%、半分内 90.0%、partial 召回 34.8%、ECE raw 0.1056/cal 0.0667；v2 本次逐项 86.7%（+2.3pp）、总分 65.8%（+2.5pp）、半分内 95.0%（+5pp）、partial 召回 42.2%（+7.4pp），但样本/拆分不同，仅供参考。
  - S3 目标：总分精确 ≥85% **未达标**（65.8%）；半分内 ≥97% **未达标**（95.0%）；ECE ≤0.08 **校准后达标**（0.0708，raw 0.0833 未达标）。
  - 结论：v2 schema 失败已消失、校准把 ECE 拉到阈值内；准确率仍显著低于目标，不能宣称 S3 达标。**未调参、未改 gold/实现**。
- **证据**：`target/acceptance/eval-real-judge-v2-calibrated-20261110.json`（**文件名误标 v2，内容实为 v1**）；代码 `crates/egctl/src/eval_real.rs`（partial 修复重试、`--prompt`、`--eval-sample`、重校准）；单测覆盖修复检测/校准单调性；真实测试 `#[ignore]`。
- **限制**：gold 合成规则标签；80/120 抽样；confidence 仍全 0.95 → 单桶校准，不能证明区间校准；status 受 calibration_id 未接入 grade 影响；未在同拆分上重跑 v1、未做 v2/v1 同批 A/B（上一轮 A/B 有小样本偏差）。

### S3 真实 v2 prompt + 重校准 + 120 条复测（2026-11-10，task-75 修复接线后）

- **缺陷修复**：`run`/`run_calibrated` 原先硬编码 `PromptVersion::V1`，CLI `--prompt v2` 被忽略；已改为统一 `selected_prompt(options)`（单测断言 V1/V2 选择与提示词内容不同；`split.prompt_version` 与实际一致）。
- **命令**：
  `egctl eval --gold fixtures/gold --real-judge --sample 80 --eval-sample 120 --calibrate --prompt v2 --concurrency 4 --json --out target/acceptance/eval-real-judge-v2-true-20261110.json`
- **实测**（qwen3.5-4b Q4_K_M，build b11960-8642541b2，并发 4，2026-11-10）：默认 80/120 拆分（互斥、seed=20261109，eligible 225，跳过 manual 5）。
  - 计划 600 次评分项请求；**repair attempted 11 / succeeded 11**（v2 partial 缺列表被真实触发并机械修复成功；另有 1 条修复重试仍失败计入 schema_violation；旧版 artifact 的 `failed` 计数未含该条，代码已修正）。失败：**timeout 33、schema_violation 1**（并发 4 下服务负载导致，失败分类见 JSON）。
  - 结果覆盖：calibration 80 条拟合 **204/240** 个评分项（部分失败）；eval 120 条中 **101 条成功**（303/360 评分项），**19 条失败/超时未进入分母** → 指标存在选择偏差，不能当作 120 条全量结论。
  - **eval（成功子集，n=101 条 / 303 评分项）**：逐项 strict **98.02%**（297/303）、lenient **100%**；总分精确 **94.06%**（95/101）；半分内 **100%**；partial 召回 **88.57%**（31/35）；**mismatch→match=0**（3 条 expected mismatch 全判 mismatch）；status raw **25.74%** / 校准后 **74.26%**。
  - **校准（v2 输出重新拟合）**：校准表 1 桶（confidence 0.8–0.95，accuracy=0.9216）；**ECE 0.05957→0.05863**（均 ≤0.08）、**Brier 0.01623→0.02285（变差）**；可靠性：raw 0.9–1.0 n=298 acc=0.9966；校准后 0.9–1.0 n=303 acc=0.9802。
  - 混淆矩阵（expected↓/actual→）：match→match 61 / partial 2；mismatch→mismatch 3；partial→mismatch 4 / partial 31。
  - 耗时 **37m10s**（超 30 min 预算；并发 4 排队+重试）；token（成功调用）prompt 325,738 / completion 32,906 / total 358,644。
- **是否达标**：成功子集上总分精确 94.06%≥85%、半分内 100%≥97%、ECE 0.0586≤0.08，**看似达标**；但 **19/120 条失败/超时（15.8%）** 未进入分母，且校准 Brier 变差、status 依赖校准接入，**不能据此宣称 S3 达标**，需降并发/重试后跑满 120 条再判定。
- **与 v1 对比**：v1（上一小节，同 80/120 拆分，120/120 成功）：总分精确 65.83%、partial 召回 42.22%、ECE 0.0833→0.0708；真实 v2 成功子集 94.06%/88.57%/0.0586，差异很大，但 v2 有 15.8% 条目失败的选择偏差，不能直接等价比较；需无失败复测。
- **证据**：`target/acceptance/eval-real-judge-v2-true-20261110.json`；单测 `prompt_version_selection_and_prompt_content`、`detects_partial_missing_list_violation`、校准单调性/PAV；真实测试 `#[ignore]`。
- **限制**：gold 合成标签；v2 失败率高导致选择偏差；confidence 仍集中；calibration_id 未接入 grade；未在无失败条件下重跑。

### S3 最终 v2 复测（并发 2 + 超时重试，**120/120 条成功**；2026-11-10，task-76）

- **实现**：仅对 timeout/连接重置类瞬态错误重试 **1 次**（退避 500ms，记录 `retries.attempted/succeeded/failed`）；schema_violation 不进入瞬态重试；partial 缺列表仍走 task-73 的 1 次机械修复。
- **命令**：
  `egctl eval --gold fixtures/gold --real-judge --sample 80 --eval-sample 120 --calibrate --prompt v2 --concurrency 2 --json --out target/acceptance/eval-real-judge-v2-final-20261110.json`
- **样本完整性**：**eval 120/120 条全部成功**（360/360 评分项进入指标）；calibration 80 条中 1 条 schema_violation（修复重试仍失败）导致拟合 237/240 项；**无 timeout**。因此本次 eval 指标无失败选择偏差。
- **重试/修复**：瞬态重试 attempted **0**（并发 2 下未出现 timeout；重试策略由单测覆盖）；partial 机械修复 attempted **15 / succeeded 14 / failed 1**（成功率 93.3%）；失败分类仅 `schema_violation=1`。
- **eval 120 条最终指标（v2 raw）**：
  - 逐项 strict **98.06%**（353/360）、lenient **99.44%**（358/360）；总分精确 **94.17%**（113/120）；半分内 **100.00%**（120/120）。
  - partial 召回 **91.11%**（41/45）；**mismatch→match = 0**（3 条 expected mismatch 全判 mismatch）；status raw **23.33%** / 校准后 **76.67%**。
  - 混淆矩阵（expected↓/actual→）：match→match 69 / partial 3；mismatch→mismatch 3；partial→mismatch 4 / partial 41。
  - 校准（calibration 拟合，eval 评估）：**ECE raw 0.05569 → calibrated 0.06072**（两者均 ≤0.08；校准未改善且 Brier 0.01655→0.02275 变差，已如实记录）；校准表 1 桶（0.8–1.0，accuracy=0.9198，confidence 仍集中）。
- **S3 明确判定（sample=120/120，v2）**：
  | 目标 | 实测 | 判定 |
  |---|---|---|
  | 总分精确 ≥85% | **94.17%** | ✅ 达标 |
  | 半分内 ≥97% | **100.00%** | ✅ 达标 |
  | ECE ≤0.08 | **raw 0.0557 / 校准 0.0607** | ✅ 达标 |
  ⇒ **在本次 120/120 合成金标样本上，S3 三项均达标**；但校准 Brier 变差、status 依赖 calibration_id 接入、单桶校准与合成标签限制仍需保留，不推广到真实考试。
- **与历史结果对比**：v1（同 80/120，120/120 成功）总分 65.83%、partial 召回 42.22%、ECE 0.0833→0.0708；真实 v2 成功子集（101/120）94.06%/88.57%/0.0586；本次完整 v2 120/120 94.17%/91.11%/0.0607，与成功子集接近，说明 task-75 的高指标并非仅由失败选择偏差造成（但仍受合成标签限制）。
- **耗时/token**：**47m44s**（并发 2，超 30–35min 预算；低并发换取 0 timeout）；成功调用 599 次，token prompt 345,068 / completion 35,009 / total 380,077；失败调用与修复重试的 token 未计入汇总。
- **证据**：`target/acceptance/eval-real-judge-v2-final-20261110.json`；单测 `transient_error_policy_is_limited_to_timeout_and_transport`、`prompt_version_selection_and_prompt_content`、partial 修复检测/校准单调性；真实测试 `#[ignore]`。
- **限制**：gold 为合成规则标签；校准 confidence 单桶（0.8–1.0）；Brier 校准后变差；status 受 calibration_id 未接入 grade 影响；47m44s 超预算；不覆盖真实手写/OCR。

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
| L7 | 真实判分模型精度/校准 | 部分已测（60 条采样，未达标） | 4B 判定已复测（§3-S3：逐项 77.2%/总分精确 56.7%/半分内 83.3%/ECE 0.1778）；仍无校准集与真实教师标注 | 独立校准/验收集（脱敏真实卷 + 教师标注）+ `grade` 校准链路 |
| L8 | UI 键鼠 / HiDPI / 深色模式 | 未验证 | 需桌面人工；app 研究记录 §8.2 已列「未做」 | 手工验收清单 + 截图/录屏证据 |
| L9 | 仓库级 `fixtures/` 与 `docs/dev/performance.md` | 缺失 | `SPEC.md` §6 期望 `fixtures/`；T39 要求性能基线文档 | 补目录与性能文档（M10 任务） |
| L10 | 端到端断网抓包 / 判分中断恢复 / 40×2 页性能 | 未验证 | 依赖 S1 与真实模型 | S1 可跑后按 §3 步骤执行并记录 |

## 5. 验收记录模板（每次执行复制一行）

| 日期时间 | 触发 | 环境/版本 | 结果摘要 | 证据位置 | 结论 |
|---|---|---|---|---|---|
| 2026-11-03T01:16+08:00 | `scripts/acceptance/run.sh --json` | macOS 27.0.1 / M4 / rustc 1.99.1 | 见 §6（WARN：仅 egctl 就绪度） | `<output-dir>/acceptance-final.json`（临时） | 滚动基线，非验收结论 |
| 待填 |  |  |  |  |  |

记录要求：附 `run.sh --json` 输出（或 `--out` 文件）与失败日志路径；人工项附截图/录屏；不得只写「通过」。

## 6. 冻结基线实测记录（2026-11-03T01:37+08:00，交付冻结）

> 本轮为**交付冻结前最后一次全量真实执行**：`bash scripts/acceptance/run.sh --json --skip-egctl --out <output-dir>/acceptance-frozen.json`。工作区在提交 `d2fc2a8`（core）/`dd1ca2c`（scripts+data）前后一致，无并行写入。历史滚动：00:36 全量 1015 passed → 01:03 1063 passed → 01:16 1063 passed → **01:37 冻结 1063 passed（本表）**。

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
- 验收 JSON：`<output-dir>/acceptance-frozen.json`（临时路径；归档时复制到交付证据目录）。各步骤原始日志路径记录在该 JSON 的 `steps[].log` 字段。
- `cargo test --workspace` 的 2 个 ignored 用例为**设计内跳过**（需真实模型权重 / 需 release 性能模式），非缺陷。
- egctl 的 doctor/bench 就绪度 WARN 语义见 `scripts/acceptance/README.md`；UI 端到端（S1/S7）与真实试卷/金标（S2/S3/S5）仍是阻塞项，见 §7 与 §3 各条。

## 7. 结论与下一步

1. **代码门禁已冻结为 PASS**：§6 冻结基线（`run.sh --json --skip-egctl`，2026-11-03T01:37+08:00，exit 0）显示 fmt / clippy / test（1063 passed、0 failed、2 ignored）/ deny / docs-check 全部 PASS，warn=0、fail=0；egctl doctor/bench 两项因 `--skip-egctl` 跳过，其「未接线/模型未装齐」属**就绪度 WARN 语义**（见 `scripts/acceptance/README.md`），不是代码门禁失败。
2. **产品级验收尚未完成**：S1–S14 中仅 S4/S5/S6/S8/S9/S10/S11/S12/S13/S14 具备部分自动化证据；**S1（全流程 UI 端到端）、S7（键鼠复核效率）、S2/S3/S5（金标集与准确率）、S10 端到端抓包与 L1–L10 阻塞项未关闭**。因此当前结论是「**代码门禁 PASS、产品级验收未完成**」，不可表述为「M10 整体验收通过」。已知限制另见：capture（HEIC/HEIF、DOCX 参考答案文本抽取显式不支持）、inference（v1 无 TLS，`https://` 显式不支持）、app（中文 IME 仅代码路径 + 手工冒烟，无自动化 E2E）。
3. **门禁脚本可复现**：`scripts/acceptance/run.sh` 已在冻结工作区真实执行并输出 JSON/TSV（§6）；如需 egctl 就绪度快照，去掉 `--skip-egctl` 重跑即得 WARN（exit 2）结果，按 §5 模板记录。工作区已冻结（并行任务全部合入、代码门禁稳定），后续重跑按 §5 模板追加记录。
4. **建议关闭顺序**：L6 模型权重 → S1 端到端合成夹具 → L10 抓包/崩溃恢复 → S2/S3 金标集 → L1/L4/L8 人工 UI → L2 真实试卷 → L3 TLS（Ask first）。
5. **夹具/性能文档缺口**（L9）：`fixtures/`（合成试卷、页面、预期结果）仍缺；`docs/dev/performance.md` 已建（性能基线），端到端批次数字待后续实测补充，在此之前性能数字以 §3-S6 与研究记录为准。
