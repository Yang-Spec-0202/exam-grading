# 研究记录：review（双模式复核工作台，T31–T32）

> 状态：已完成（T31–T32）· 2026-11-02 · 对应规范：`SPEC-review.md` v0.2.1、`SPEC-domain.md` §8（判分记录/状态/排除）、`SPEC-platform.md`（审计/仓储）、`CONFIG.md` §1/§6、`SPEC-grade.md`（记录/模式）、`AGENTS.md`
> 规则：检索优先；本页记录来源、结论、版本实测、平台缺口、风险与规范偏差。

## 1. 模块与范围

- 模块 id：`review`（crate `eg-review`），依赖 `eg-domain` 与 `eg-grade`（记录/阈值/幂等语义）；数据源经 trait 注入，生产接 `eg-platform`（repo/audit）。
- 本次实现（T31–T32）：
  1. **队列（`src/queue.rs`）**：标准待复核/快速抽检/已确认三队列严格区分；标准队列按原因严重度（冲突 > 不可判定 > 置信度不足 > 证据缺失 > 未校准 > 后端不可用 > 分数非法）→ 置信度升序 → 分值降序 → 题序 → 学号；抽检队列按抽检顺序近似（题序/学号）；按题目/学生/模式/状态/原因/抽检过滤；稳定排序键游标分页（不重复）；队列计数。
  2. **详情（`src/detail.rs`）**：最新修订 + 历史修订、学生答案/参考答案视图、证据引用、资产 id（懒加载）、比较器轨迹、模型调用、判定/得分、模式/抽检标记、只读标记、允许命令集、是否已转标准。
  3. **命令（`src/commands.rs`）**：接受、改分（MilliScore 粒度/范围校验 + 原因码）、修文本重判（题级锁 + 人工修正 → 重判新修订）、题级排除、提交级排除、批量接受、批量改分、撤销/重做（≥50 步，追加反向修订）；命令幂等键；乐观锁（`expected_revision`）；每条命令（含被拒）落审计（actor 固定 `local-teacher`、前后快照、原因码、范围、确认标记、抽检结论）。
  4. **抽检（`src/sampling.rs`）**：快速抽检队列只读（编辑命令 100% 拒绝）；抽检通过只写结论与审计（判分不变）；抽检不通过未确认 → 返回弹窗协议事件（默认单题、可选整卷，不自动重跑）；确认后默认单题重跑，整卷仅显式调度任务；审计含范围/确认/结论。
  5. **转标准（`src/transfer.rs`）**：一键转标准（单题重新以标准模式生成新修订，原快速记录保留；整份提交调度整卷任务）；无法重判 → 降级为标准待复核修订（绝不写 0 分）。
  6. **撤销栈（`src/undo.rs` + `ports::UndoStore`）**：命令栈持久化 trait（生产接 platform；进程重启后仍可撤销），记录/排除/抽检结论三类动作。
  7. **端口与 fixture（`src/ports.rs`）**：`ReviewRepository`、`AuditSink`、`UndoStore`、`RegradeEngine` 四个 trait + 内存 fixture；`RegradeEngine` 请求携带经 grade 幂等语义的键，fixture 按幂等键去重。
- 明确不做：判分算法/阈值（grade）、OCR/识别（recognize）、导出统计（report）、数据库结构（platform）。

## 2. 问题清单

| # | 需确认的行为/API/限制 | 影响 |
|---|---|---|
| Q1 | 标准/快速队列来源、排序、只读约束、抽检通过/不通过语义 | 决定队列/详情/命令模型 |
| Q2 | 命令集合与参数（接受/改分/修文本重判/排除/批量/撤销/重做）、原因码 | 决定命令与审计 |
| Q3 | 乐观锁（`revision`）与命令幂等键；撤销深度 ≥50 且跨进程 | 决定服务与撤销栈适配 |
| Q4 | 审计字段（actor 固定、前后值、原因、幂等键）与 platform `AuditLog` 能力 | 决定审计端口/缺口 |
| Q5 | `eg-platform::repo` 是否提供队列/最新修订/排除/撤销栈查询 | 决定数据源适配与平台缺口 |
| Q6 | `eg-grade` 的判分记录/阈值/幂等键如何复用 | 决定依赖与重判适配 |
| Q7 | 抽检不通过弹窗协议（默认单题、整卷需选择、未确认不重跑） | 决定命令结果/事件 |
| Q8 | 转标准语义（重判 vs 仅转队列；无法重判降级） | 决定 transfer 实现 |
| Q9 | 抽检只读开关关闭是否允许改分（Ask first；Never 条款） | 决定配置处理 |
| Q10 | 导出后修改提示、无证据确认是否实现 | 决定偏差/待办 |

## 3. 来源

| # | URL / 文件 | 版本/日期 | 类型 | 取用内容 |
|---|---|---|---|---|
| S1 | 仓库 `SPEC-review.md` | v0.2.1（2026-11-02） | 项目规范 | §2 目标/无登录；§3 队列/详情/命令/抽检/转标准；§4 状态机/乐观锁；§5 失败模式；§7 测试策略；§8 Always/Ask first/Never；§9 成功标准 |
| S2 | 仓库 `SPEC-domain.md` §8 与 `crates/eg-domain/src/grade.rs` | v0.2.2（2026-11-02） | 规范+源码 | `GradeRecord`（revision/status/mode/sampled/reviewed_at/review_note）、`GradeStatus`、`ReviewReasonKind`、`Judgement`、`SubmissionExclusion`/`ExcludeReason`、`validate()` 不变量 |
| S3 | 仓库 `SPEC-platform.md` 与 `crates/eg-platform/src/{repo,audit}.rs` | 2026-11-02 源码 | 规范+源码 | `append_grade_record`、`list_grade_records(conn, submission_id)`、`AuditLog::{append,business_write,list}`、`NewAuditEntry`、`AuditEntry`、`AuditQuery` 字段 |
| S4 | 仓库 `CONFIG.md` §1/§6 | v0.2.1 | 项目规范 | `ui.page_size`、`ui.undo_depth`、`fast.sampled_readonly`、`fast.fail_action`、`fast.fail_default_scope`、`fast.sampling_rate`、`fast.low_confidence_weight` |
| S5 | 仓库 `crates/eg-grade/src/{config,gate,ports}.rs` | 2026-11-02 源码 | 源码 | `GradeConfig` 阈值/粒度、`IdGenerator`/`SequentialIds`/`FixedClock`、幂等键语义（提交+题+版本+识别哈希+引擎指纹+模式） |
| S6 | 仓库 `docs/dev/research/grade.md` | 2026-11-02 | 项目研究 | 判分记录不变量、门禁原因、抽检比例/权重/种子、幂等键 |
| S7 | 仓库 `docs/dev/research/domain.md` | 2026-11-02 | 项目研究 | 只追加、证据链、状态机约束 |
| S8 | 仓库 `AGENTS.md` | 2026-11-02 | 项目流程 | 检索优先、版本实测、研究记录、失败即停 |

> 无外部网络依赖；未新增第三方库（除测试用 `serde_json`，与平台/领域既有依赖一致）。所有平台能力以仓库源码实测为准（非模型记忆）。

## 4. 结论

| 问题 | 结论 | 依据 |
|---|---|---|
| Q1 | 队列三分类：标准=状态待复核；快速抽检=mode Fast + `sampled=true` + 自动接受；已确认=已人工复核。标准按风险排序（原因严重度→置信度→分值→题序→学号）；抽检只读（`fast.sampled_readonly` 默认开），仅允许通过/不通过/转标准。 | S1 §3.2；`queue.rs` 排序/过滤/分页测试 |
| Q2 | 命令集合按规范：接受、改分（范围+粒度+原因码）、修文本重判（锁+重判）、题级排除（排除原因+复核原因码）、提交级排除、批量接受、批量改分、摇测通过/不通过、转标准、撤销/重做；审计动作名 `review.*`。 | S1 §3.3；`model.rs`/`commands.rs` 测试 |
| Q3 | 每条单记录命令携带 `expected_revision`，与最新修订不符 → `StaleRevision` 拒绝（刷新提示）；命令幂等键命中返回既有结果不重复执行；撤销栈 trait 持久化（深度 ≥50，测试 55 命令撤销 50 步 + 重做 10 步），重启后（新 service 实例 + 同一 UndoStore）仍可撤销。 | S1 §3.3/§4/§5；`commands.rs` 测试 |
| Q4 | 审计端口记录 actor=local-teacher、动作、实体、提交/题、命令幂等键、前后快照、原因码、范围、确认标记、抽检结论；被拒命令也落审计（outcome=Rejected）。生产映射到 `platform::audit::NewAuditEntry` 的 note/hashes 字段（缺口见 §6）。 | S1 §3.3/§6；S3；`ports.rs` |
| Q5 | 平台 repo 缺口：只有自由函数 `append_grade_record(tx, record)` 与 `list_grade_records(conn, submission_id)`（需 rusqlite Connection、单提交维度、返回全部修订），缺少跨提交队列查询、最新修订视图、按模式/状态/抽检过滤、游标分页、提交级排除读写、撤销栈持久化；审计 `AuditLog` 需 Database+UlidFactory，且 `NewAuditEntry` 无命令幂等键/前后快照/原因码/范围/确认字段。故以 `ReviewRepository`/`AuditSink`/`UndoStore` trait 适配（fixture 测试），平台扩展任务另行提交。 | S3 源码实测；`ports.rs` 文档注释 |
| Q6 | 直接依赖 `eg-grade`：队列风险用 `GradeRecord::auto_accept_blockers(threshold)` + `GradeConfig::threshold(route, mode)`；分数粒度用 `GradeConfig::score_granularity`；id/时钟复用 `eg_grade::ports::{IdGenerator, SequentialIds, FixedClock}`；重判键按 grade 幂等语义（提交+题+模式+修正文本/范围）构造。 | S5；`commands::regrade_key` |
| Q7 | 抽检不通过：`confirmed=false` → 返回 `SamplingPrompt`（默认单题、可选整卷、提示文案），不调用重判、不写修订，审计记「待确认」；`confirmed=true` + 单题 → 当前模式重判并追加修订；整卷 → 调 `RegradeEngine` 调度任务，返回 task id；未确认 0 次自动重跑有测试断言。 | S1 §3.2/§5；`tests/sampling_transfer.rs` |
| Q8 | 一键转标准：单题以 Standard 模式重判生成新标准修订（保留原快速记录）；整份提交调度整卷任务；重判不可用 → 生成 Standard + NeedsReview 修订（进入标准队列），不写 0 分；测试覆盖单题/整卷/降级。 | S1 §3.3；`transfer.rs`；测试 |
| Q9 | `fast.sampled_readonly=false` 仍拒绝快速抽检编辑：`ensure_editable` 对快速记录返回 `FastModeReadOnly`（Never 条款「让抽检项被改分」优先于配置）；关闭只读需规范变更/Ask first，已在偏差表记录。 | S1 §8 Never；`sampling::ensure_editable` |
| Q10 | 导出后修改提示与「无证据确认」未实现：`GradeRecord`/`ReviewDetail` 无导出版本/导出时间字段，需 report/platform 联合；领域记录强制至少 1 条证据，故「无证据确认」在领域层不可能（若后续允许，需 grade/domain 变更）。记录为偏差/TODO。 | S2；S1 §5；偏差表 |

## 5. 版本实测

| 项目 | 实测 | 说明 |
|---|---|---|
| Rust / cargo | 1.99.1（stable，edition 2024） | `cargo test -p eg-review` |
| 依赖 | `eg-domain`、`eg-grade`（均 workspace）、`thiserror 2.0.21`；dev：`serde_json 1` | 无网络/DB/UI 依赖；生产代码无 `unsafe`；clippy deny `unwrap/expect/panic` |
| 依赖调整（相对 SPEC-review §1） | 移除 `eg-capture`/`eg-recognize` 直接依赖：本模块只读判分记录/证据 id，原图/裁片按需懒加载由 app/平台提供；且二者当前并行 WIP（eg-recognize 缺模块导致编译失败）。证据资产通过 `EvidenceRef::Asset` 暴露 id，不解析像素。 | 见 §7 偏差；接线时由 app 注入资产读取端口 |
| 测试 | **28 个测试函数**：`queue_detail` 5、`commands` 12、`sampling_transfer` 11（lib 无单测） | 全绿；clippy `-D warnings` 0 警告；fmt --check 通过 |
| 覆盖 | 行 **81.77%**（commands 87.44%、queue 81.82%、detail 80.77%、ports 76.65%、sampling 87.50%、transfer 66.67%） | `cargo llvm-cov -p eg-review --summary-only`；分支覆盖需 nightly（stable 不支持） |
| 队列规模 | 1000 条队列分页测试：每页 ≤ page_size、total=1000、翻页无重复无遗漏 | `queue_detail::thousand_item_queue_pages_without_duplicates` |
| 只读约束 | 抽检项上接受/改分/修文本/题级排除/提交排除/批量接受/批量改分全部拒绝；审计均落 Rejected；append_count=0 | `sampling_item_rejects_every_edit_command` 等 |
| 撤销深度 | 55 次改分后撤销 50 步恢复到第 5 步状态（awarded=500）；重做 10 步回到第 15 步（1500）；新 service 实例共享 UndoStore 仍可撤销 | `commands.rs` 测试 |
| 审计完整 | 成功/被拒/批量逐项/汇总审计；actor 固定；抽检结论/范围/确认均落审计 | 测试断言 |
| 重跑幂等 | 同一命令幂等键二次提交：重判执行次数 1、修订只追加 1 次；fixture 重判按 grade 幂等键去重 | `regrade_is_idempotent_by_command_key` |

## 6. 平台缺口（需 platform 扩展任务）

1. **仓储查询**：无跨提交队列查询（按 status/mode/sampled/reason/student/question 过滤）、无「最新修订」视图、无游标分页、无按 grade_id 单条读取、无提交级排除读写、无撤销栈持久化。现有 `list_grade_records(conn, submission_id)` 只能按提交拉全部修订。
2. **审计**：`AuditLog` 需要 `Database`/`SharedClock`/`UlidFactory` 且与业务写分离；`NewAuditEntry` 只有 note/before_hash/after_hash/task/consent，缺命令幂等键、实体提交/题、原因码、重跑范围、确认标记、抽检结论。复核审计需要通过 `business_write` 事务化（记录追加 + 审计同事务），否则存在「已改分未审计」窗口。
3. **资产/识别**：详情需要按 `EvidenceRef::Asset`/`Recognition` 懒加载图片与识别文本；平台资产库有读取 API，但缺「按判分记录批量取资产/识别摘要」的组合查询。
4. **整卷重跑任务**：`RegradeEngine` 仅返回 task id，任务表/调度由 platform tasks 提供（T19+ inference/整卷重判任务）。
5. **导出后修改提示**：需要 report 导出记录（提交级导出版本/时间）与 review 的提交状态查询。

## 7. 风险与限制

| 风险 | 影响 | 缓解/预案 |
|---|---|---|
| R1：平台 repo 未扩展 | 生产数据源需自己拼 SQL/多次调用 | trait 隔离 + 缺口清单；平台扩展任务；fixture 全测试 |
| R2：审计与记录非同一事务（适配层未接线） | 极端情况下记录成功但审计失败 | 生产实现用 `AuditLog::business_write`；本服务 append 后审计失败返回错误，恢复需平台事务 |
| R3：撤销栈/命令幂等缓存为内存 fixture | 进程重启后命令幂等缓存丢失（撤销栈可持久化） | UndoStore 持久化接口；命令幂等键可落 platform audit/命令表（TODO）；重复提交由审计键定位 |
| R4：游标基于稳定排序键 | 分页期间新增/变更记录可能落在游标之前而被漏掉（规范要求不重不漏） | 当前保证「不重复」；新修订会改变排序键，建议 UI 每页重新查询/快照总数据；记录限制 |
| R5：只读开关关闭仍拒绝快速编辑 | 与配置项语义不完全一致 | 遵循 Never 条款；如需开放必须改规范（Ask first） |
| R6：无导出标记/无证据确认 | 规范 §5 两条失败模式未覆盖 | 依赖 report/platform 扩展；领域记录强制证据；记录偏差 |
| R7：批量抽检通过/批量转标准未做专用命令 | UI 需逐条发命令（每项独立审计） | 可用重复单命令实现；后续可加 batch 变体（不改语义） |
| R8：分支覆盖未量化 | CI 门槛缺数据 | stable 不支持 `--branch`，建议 nightly；本模块命令分支由测试覆盖主要路径 |
| R9：`RegradeEngine` 未接真实 grade Grader | 重判行为仅 fixture | 接线时以 `eg_grade::Grader` + 幂等键实现；本服务只负责命令/审计/修订 |

## 8. 与规范的偏差

| 规范位置 | 偏差/解释 | 处理 | 审批状态 |
|---|---|---|---|
| §1 依赖 | 移除 `eg-capture`/`eg-recognize` 直接依赖（只读证据 id；原图/识别懒加载由 app/平台提供；且并行 WIP） | 证据资产用 id 传递；接线时 app 注入资产/识别读取端口 | 待 Lead 确认 |
| §3.2 抽检只读开关 | `fast.sampled_readonly` 读取但不放开编辑：快速记录编辑一律 `FastModeReadOnly`（Never 条款优先） | 配置仅影响详情只读标记/提示；开放需 Ask first + 规范变更 | 待 Lead 确认 |
| §3.3 批量抽检通过/批量转标准 | 未提供专用批量命令；以逐条命令 + 逐项审计实现 | 可在 app/后续任务加 batch 变体（语义不变） | — |
| §5 已导出后修改提示 | 未实现（无导出版本字段） | 依赖 report/platform 扩展 | 待后续任务 |
| §5 无证据确认 | 未实现（`GradeRecord::validate` 强制 ≥1 证据，域层不可能无证据） | 若产品要求允许，需 domain/grade 变更 | 待裁定 |
| §7 队列性能 1000 条 ≤500ms | 单测在 debug 下 0.45s 完成 1000 条全分页（含排序）；未做 release 基准 | 记录实测；如需硬指标用 criterion/release 基准 | — |
| §9 成功标准 6（日志 0 明文姓名） | 队列条目含 `student_no`（内部排序/过滤），日志脱敏由日志层保证；本模块未写日志 | 研究记录说明；app 日志使用掩码显示名 | — |

## 9. 接口 TODO（接线）

1. `ReviewRepository` ← platform repo 扩展：跨提交最新修订查询、过滤/游标分页、提交排除读写、撤销栈表；资产/识别摘要组合查询。
2. `AuditSink` ← `platform::audit::AuditLog`：映射 actor/action/实体/幂等键/前后快照/原因/范围/确认；用 `business_write` 与记录追加同事务。
3. `UndoStore` ← platform 命令栈表（持久化最近 50 步，跨重启）。
4. `RegradeEngine` ← `eg_grade::Grader`（单题/整卷、幂等键、模式切换、失败保留原判）；整卷任务 ← platform tasks。
5. `ReviewConfig` ← `AppConfig`（page_size/undo_depth/sampled_readonly/fail_action/fail_default_scope；grade 阈值/粒度）。
6. 导出后修改提示 ← report 导出记录；无证据确认（如产品要求）← domain/grade 放宽证据约束并审计显著标记。

## 10. 复现命令

```bash
source "$HOME/.cargo/env"
cargo test -p eg-review
cargo clippy -p eg-review --all-targets -- -D warnings
cargo fmt -p eg-review -- --check
cargo llvm-cov -p eg-review --summary-only
# 局部：队列/详情、命令、抽检/转标准
cargo test -p eg-review --test queue_detail
cargo test -p eg-review --test commands
cargo test -p eg-review --test sampling_transfer
```
