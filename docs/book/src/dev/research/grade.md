# 研究记录：grade（两层判分、模式/门禁/抽检/幂等，T16–T18）

> 状态：已完成（T16–T18）· 2026-11-02 · 对应规范：`SPEC-grade.md` v0.2.1（§3–§13）、`SPEC-domain.md` v0.2.2 §8（判分记录/判定/路由/模式/证据）、`SPEC-inference.md`（judge 角色/校准/修复）、`CONFIG.md` §5/§6、`AGENTS.md`
> 规则：检索优先；本页记录来源、结论、版本、风险、接口 TODO 与规范偏差。

## 1. 模块与范围

- 模块 id：`grade`（crate `eg-grade`），依赖 `eg-domain` 契约；测试用 `eg-subject-math` 作为具体 `SubjectPack`（dev-dependency）。
- 本次实现（T16–T18）：
  1. **L1 路由（`src/route.rs`）**：R0–R11 有序纯函数，首个命中即返回；R9 分类澄清以注入的 `ClassifierBackend` fixture 实现（分类置信度 <0.9 转人工）；路由轨迹（规则编号、输入哈希、分类调用、决策时间）进 `RouteTrace`。
  2. **试卷就绪（`src/readiness.rs`）**：答案键/已确认规则/仅人工标记/答案键与规则版本/模板版本；草稿规则不算生效；错误级问题整卷拒绝判分。
  3. **L2 规则段（`src/l2.rs`）**：调用学科包比较器链（首个确定结论终止），多解候选逐个判定取最高分，全部尝试落 `ComparatorTrace`。
  4. **评分聚合（`src/aggregate.rs`）**：累加/全对/分步（依赖拓扑）/多选部分给分（只读 `Rubric.partial_choice`；无错选按命中数取档、错选归零、缺档/非法 → 不可判定转人工，绝不默认 0 分）。
  5. **模型输出硬校验（`src/validate.rs`）**：得分范围/粒度/与判定一致性/逐项覆盖/聚合一致/理由非空不超长/引擎指纹/原始输出/云端授权/命中候选范围；按评分标准判定时聚合 ≠ 声明 → 采用聚合值并记冲突（§8）。
  6. **门禁与置信度（`src/gate.rs`）**：标准/快速两套阈值；规则段/模型段置信度组合（模型段取最小值、证据块为空上限 0.5）；未校准模型标准模式禁止、快速模式需 ≥0.90 且强制抽检；冲突策略（默认升级人工 / 规则优先）。
  7. **快速模式抽检（`src/sampling.rs`）**：比例 0.20、低置信权重 0.70、加权无放回、⌈n×rate⌉ 至少 1 题、种子记录、同种子可复现（SplitMix64）。
  8. **幂等（`src/idempotent.rs`）**：键 = 提交+题+答案键版本+规则版本+识别哈希+引擎指纹+模式；命中返回既有记录，不重复判分/计费（`IdempotencyStore` 适配，生产接 platform repo 为 TODO）。
  9. **流水线（`src/pipeline.rs`）**：单题判分 + 提交级汇总（自动接受/待复核/失败/抽检/排除/缺考）；单题失败不影响其余题，失败显式记录。
- 明确不做（其他模块）：不生成答案/规则（rubric）、不实现模型调用（inference）、不做对齐（align）、不做 UI（review）、不做导出（report）。

## 2. 问题清单

| # | 需确认的行为/API/限制 | 影响 |
|---|---|---|
| Q1 | R0–R11 的准确条件与优先级；R9 分类澄清边界（只出题型/置信度、<0.9 转人工） | 决定路由纯函数与测试表 |
| Q2 | 就绪条件：草稿规则是否生效、缺答案键/「略」/仅规则/仅人工标记、版本与模板对齐 | 决定整卷拒绝规则 |
| Q3 | L2 链执行/多解/「不相等必须带原因」/「全部无法判定才升级」的领域契约 | 决定规则段与证据 |
| Q4 | 评分聚合四种模式（累加/全对/分步依赖/多选部分给分）与缺档处理 | 决定聚合算法 |
| Q5 | 模型输出硬校验清单与冲突仲裁（规则 vs 模型、聚合 vs 声明） | 决定校验与冲突策略 |
| Q6 | 标准/快速阈值表；未校准模型；抽检比例/权重/种子/可复现算法 | 决定模式与抽检 |
| Q7 | 快速模式抽检算法细节（候选集、低置信定义、无放回、至少 1 题） | 决定抽样实现 |
| Q8 | 幂等键字段与命中行为（不重复计费/判分） | 决定幂等适配层 |
| Q9 | 提交级汇总语义（未判/人工/排除/缺考；单题失败不丢题） | 决定流水线结果形状 |
| Q10 | `eg-rubric`/`eg-inference` 并行未稳定：如何在不改其他 crate 的前提下实现与测试？ | 决定 trait 适配层 |
| Q11 | 平台已提供 repo/config/ids/clock，但 grade 是否直接依赖 platform？ | 决定分层方式 |

## 3. 来源

| # | URL / 文件 | 版本/日期 | 类型 | 取用内容 |
|---|---|---|---|---|
| S1 | 仓库 `SPEC-grade.md` | v0.2.1（2026-11-02） | 项目规范 | §3 阈值/就绪；§4 R0–R11；§5 链/模型/聚合；§6 置信度；§7 门禁/模式/抽检/状态机；§8 冲突；§9 失败；§11 测试；§12 边界 |
| S2 | 仓库 `SPEC-domain.md` §8 与 `crates/eg-domain/src/grade.rs`/`evidence.rs` | v0.2.2（2026-11-02） | 项目规范 + 源码 | `GradeRecord`/`Judgement`/`JudgeOutcome`/`RouteDecision`/`RouteTrace`/`ManualReason`/`UngradableReason`/`GradeMode`/`GradeStatus`/`ReviewReasonKind`/`ComparatorTrace`/`ModelCall`/`EngineStamp`/`SubmissionExclusion` 不变量与校验方法 |
| S3 | 仓库 `SPEC-inference.md` | v0.2.1 | 项目规范 | judge 角色请求/响应、校准 id、修复重试、云端授权、温度 0/输出受限（由适配层执行） |
| S4 | 仓库 `CONFIG.md` §5/§6 | v0.2.1 | 项目规范 | 阈值默认（标准 0.85/0.90/0.85/0.80；快速 rule 0.80 其余下调 0.10）、粒度、修复次数、冲突策略、抽检 0.20/0.70/种子 |
| S5 | 仓库 `crates/eg-platform/src/config.rs` | 2026-11-02 源码 | 源码 | 平台 `GradeConfig`/`FastConfig`/`StandardThresholds`/`FastThresholds`/`ConflictPolicy`/`SamplingSeed` 字段与默认（本 crate 保持分层独立，未直接依赖 platform，映射 TODO） |
| S6 | 仓库 `SPEC-subject-math.md` 与 `docs/dev/research/subject-math.md` | v0.2.1 | 项目规范/研究 | 比较器链顺序、`SubjectPack` 契约、无法判定语义、CAS 版本（evidence） |
| S7 | 仓库 `crates/eg-domain/src/contract.rs`、`subject.rs` | 2026-11-02 源码 | 源码 | `run_comparator_chain`（首结论终止/attempts）、`SubjectPack` 方法签名、`ComparatorVerdict` |
| S8 | 仓库 `docs/dev/research/domain.md` | 2026-11-02 | 项目研究 | 领域不变量、记录只追加、证据要求 |
| S9 | https://prng.di.unimi.it/splitmix64.c | 2026-11-02 查阅 | 官方实现 | SplitMix64 确定性 PRNG（抽检可复现；无外部依赖） |
| S10 | 仓库 `AGENTS.md` | 2026-11-02 | 项目流程 | 检索优先、版本实测、研究记录、失败即停 |

> 本模块无外部网络/数据库依赖；外部检索仅用于确认 PRNG 算法（S9）与领域/平台现状（S2/S5）。无新增第三方库（除已有 `serde_json`/`thiserror`）。

## 4. 结论

| 问题 | 结论 | 依据 |
|---|---|---|
| Q1 | 路由为纯函数，顺序 R0→R11；每个分支只依赖「题型/答案键形态/规则确认/识别置信度/归一化/后端/模式」。R9 输出 `needs_classification`，由流水线调用一次分类器；分类置信度 <0.9、分类失败或后端不可用 → 人工。 | S1 §4；`route.rs` 表驱动测试（12 条规则逐条正例+边界） |
| Q2 | 就绪：答案键版本/规则版本必须 >0；每题要么可判定答案键（含学科静态校验通过）、要么已确认规则、要么教师明确标记仅人工；草稿 → 错误级；模板版本 != 答案键版本 → 错误级。 | S1 §3/§9；`readiness.rs`；就绪失败返回 `GradeError::NotReady` 整卷拒绝 |
| Q3 | 规则段复用 `eg-domain::contract::run_comparator_chain`；多解候选（主答案+备选）逐个执行，任一等价即满分并记录命中候选；全部确定不相等才判不一致；存在无法判定且无等价候选时按路由升级模型或人工。 | S1 §5.1/§5.4；S7；`l2.rs` |
| Q4 | 累加=满足项之和；全对=全部满足才满分；分步=满足且所有前置生效（固定点迭代，规则依赖图已由 domain 校验无环）；多选部分给分=只读 `Rubric.partial_choice`：无错选按命中数查档、错选归零、命中=正确数满分、缺档/非法/选项不可识别 → 不可判定转人工（绝不默认 0）。 | S1 §5.3；S2 §4 不变量 6–11；`aggregate.rs` |
| Q5 | 硬校验：得分范围/粒度/判定-得分一致性（domain `validate_against`）/逐项覆盖（满足+缺失=全部评分项，无重复）/聚合=声明（不等则采用聚合值+冲突）/理由非空且 ≤500 字/引擎指纹/原始输出非空/云端需授权/命中候选范围。冲突：规则不相等 vs 模型一致 → 人工；规则相等 vs 模型非一致 → 按策略（默认人工，规则优先且快速模式可自动但标记抽检）；聚合≠声明 → 采用聚合 + 人工。 | S1 §5.2/§5.3/§8；S2；`validate.rs`；`gate.rs`；20 题夹具 Q16/Q19 |
| Q6 | 标准阈值 0.85/0.90/0.85/0.80；快速 rule=0.80、其余=标准−0.10（0.80/0.75/0.70）；规则段置信度=识别置信度；模型段=min(模型原始、识别、证据块最低)，证据块为空上限 0.5；未校准模型标准模式禁止（可配置放行），快速模式需 ≥0.90 且强制抽检。 | S1 §3/§6/§7；S4；`config.rs`/`gate.rs`；阈值测试两套分别断言 |
| Q7 | 候选=快速模式自动接受题；低置信=置信度 < 当前模式阈值+0.10；权重 0.70/0.30；数量=⌈n×0.20⌉ 且 ≥1；按权重无放回抽样（SplitMix64）；种子=配置固定值或按提交 id 派生并记录；抽样只读标记（`sampled=true`），不改分。 | S1 §7；S4 §6；S9；`sampling.rs`；40 组随机数据重复运行 0 差异 |
| Q8 | 键 = 提交 + 题 + 答案键版本 + 规则版本 + 识别内容 SHA-256 + 引擎指纹（配置哈希+学科能力+判定/分类后端身份）+ 模式；命中直接返回既有 `GradeRecord`（不调用模型、不落新修订）。 | S1 §8；`idempotent.rs`；幂等测试断言第二次 judge 调用次数不变 |
| Q9 | 提交级：逐题判分（题序），单题失败进 `failures` 且不影响其余题；汇总含总题数/自动接受/待复核/失败/抽检数/排除/缺考；排除提交不判分、只回汇总。 | S1 §3/§9；`pipeline.rs`；汇总测试 |
| Q10 | 用 `ports` trait 适配并行依赖：`RubricSource`（仅返回已确认规则；草稿由 `RubricAvailability::Draft` 表达）、`JudgeBackend`（判定请求/响应 + 超时/不可用/内部错误）、`ClassifierBackend`（R9）、`IdGenerator`、`IdempotencyStore`。fixture 实现用于测试与 CI（无真实模型）。TODO：`eg-rubric`/`eg-inference` 稳定后实现对应适配器。 | AGENTS 并行约束；`ports.rs`；测试全部走 fixture |
| Q11 | 不在 `eg-grade` 直接依赖 `eg-platform`：判分引擎只需 domain 契约 + 注入的 id/clock/幂等 trait，保持「不做 UI/持久化」的分层；platform 的 `GradeConfig`/`FastConfig` 与本 crate 的运行时配置同构，由 `app` 接线时映射（TODO）。 | S1 §1；S5；`config.rs`（未依赖 platform） |

## 5. 版本实测

| 项目 | 实测 | 说明 |
|---|---|---|
| Rust / cargo | 1.99.1（stable，edition 2024） | `cargo test -p eg-grade` |
| 依赖 | `eg-domain`（workspace）、`serde_json 1.0.154`、`thiserror 2.0.21`；dev：`eg-subject-math` | 无网络/DB/模型依赖；无 `unsafe`；生产路径无 `unwrap/expect/panic`（clippy deny） |
| 依赖调整（相对 SPEC-grade §1） | 移除 `eg-align`/`eg-rubric`/`eg-inference` 的**直接依赖**（并行 WIP，且当前仅经 trait 适配；SPEC 的依赖意图仍保留为 TODO）；`eg-subject-math` 移到 dev-dependencies（测试夹具用，生产仅依赖 `SubjectPack` 契约） | 见 §7 偏差；集成时再恢复/接入适配器 |
| 测试 | **37 个测试函数**：lib 19、`fixtures_20` 1、`routing_readiness` 4、`modes_sampling` 5、`idempotency_validation` 4、`coverage_paths` 4 | 全绿；`cargo clippy -p eg-grade --all-targets -- -D warnings` 0 警告；`cargo fmt --check` 通过 |
| 覆盖 | 行 **87.66%**（TOTAL）；route 88.66%、gate 98.09%、sampling 96.06%、aggregate 92.16%、pipeline 86.02%、readiness 62.30% | `cargo llvm-cov -p eg-grade --summary-only`；分支覆盖需 nightly（stable 不支持 `--branch`），CI 建议用 nightly 或表驱动用例守护 |
| 路由表覆盖 | R0–R11 每条 ≥1 正例 + 1 边界（R0 低置信边界、R1 恰好阈值、R3 有规则边界、R5 三题型、R6/R7/R8 多题型、R9 未知/冲突、R10 后端不可用、R11 仅人工） | `tests/routing_readiness.rs` 表驱动 |
| 20 题夹具 | 1 个端到端测试，逐题断言（规则、判定、得分、状态）0 diff；覆盖「略/仅规则/多解/分步/低置信/多选部分给分/冲突/聚合不一致」 | `tests/fixtures_20.rs`（17 自动接受 + 3 待复核） |
| 抽检可复现 | 40 组随机数据重复运行 0 差异 + 提交级抽样两次同种子同集合 | `tests/modes_sampling.rs` |
| 非法输出拒绝 | 9 类非法响应全部拒绝（得分越界/不一致/粒度/理由空/输出空/云端缺授权/候选越界/覆盖缺失），且不给 0 分成功 | `tests/idempotency_validation.rs` |
| 幂等 | 二次判分命中既有记录，judge 调用次数保持 1 | 同上 |

## 6. 风险与限制

| 风险 | 影响 | 缓解/预案 |
|---|---|---|
| R1：`eg-rubric`/`eg-inference` 接口未稳定 | 真实规则来源/模型后端未接线 | trait 适配层 + fixture；TODO 明确；不改其他 crate |
| R2：归一化失败在 R1 的适用面 | 自由文本论述题若归一化失败不应被 R1 拦截 | 实现解释：仅「有具体答案键」（需要确定性比较）时归一化失败 → R1；仅规则/仅人工路由用原始文本交模型/人工（见 §7） |
| R3：平台配置映射未接线 | 阈值/抽检比例仍用本 crate 默认（与 CONFIG 一致） | `app` 接线时用 `GradeConfig::from_platform` 映射（TODO）；改默认值属 Ask first |
| R4：幂等持久化未接 platform repo | 进程重启后幂等失效 | `IdempotencyStore` 适配；生产接 `platform.repo`（TODO） |
| R5：抽检种子为派生值（未配置固定种子时） | 跨提交不可比，但同提交可复现 | 种子随结果记录；审计可复现；固定种子可配置 |
| R6：`RuleThenModel` 仅在规则无法判定时调模型 | 规则确定时不做模型交叉检查（除 ModelDirect/RubricItemized 会做交叉） | 与 SPEC §5.1 一致；冲突检测覆盖交叉路径 |
| R7：逐项覆盖要求模型返回全部评分项 | 模型漏项 → 无效输出/修复重试 | 硬校验 + 修复（`max_repair_attempts`）+ 最终人工 |
| R8：分支覆盖率未实测 | 验收项「分支覆盖 100%」缺数据 | stable 不支持 branch；建议 CI 用 nightly `cargo llvm-cov --branch`；路由表已表驱动全规则 |
| R9：未实现真实模型输出 Schema 反序列化 | 适配层需将 inference 的 JSON 映射为 `JudgeResponse` | `ports.rs` 收口；TODO 接 inference 时补映射与云端授权 |
| R10：Readiness 覆盖较低（62% 行） | 部分问题类型分支未覆盖 | 已在 `coverage_paths` 补主要分支；后续补「仅人工标记/模板不匹配」细分支 |

## 7. 与规范的偏差

| 规范位置 | 偏差/解释 | 处理 | 审批状态 |
|---|---|---|---|
| §1 依赖 | 直接依赖暂时移除 `align/rubric/inference`（并行 WIP；当前经 trait 适配，未使用其 API），`subject-math` 移到 dev-deps | 分层保持；待并行 crate 稳定后恢复依赖并实现适配器（TODO） | 待 Lead 确认 |
| §4 R1 | 归一化失败仅在「有具体答案键」时需要确定性比较时触发 R1；「仅评分标准/仅人工」路由使用原始文本，不因自由文本（含中文数字/文字）无法数学归一化而整题转人工 | 实现解释（否则论述题全部被 R1 拦截）；测试覆盖「仅规则」文本路径 | 待 Lead 裁定（如需严格按 R1 全量，需改实现） |
| §5.2 硬校验 | 理由长度上限未在 CONFIG 单列，采用 500 字常量（可配置于 `GradeConfig`） | 实现常量 + 配置项；改值属 Ask first | — |
| §7 抽检 | 抽检候选按「快速模式自动接受」记录，`force_sampled`（未校准模型放行）一并标记；抽样只写 `sampled`（只读语义由 review 保证） | 与规范一致 | — |
| §7 种子 | 未配置固定种子时按提交 id 派生并随结果记录；规范允许「随机（记录种子）」 | 实现解释 | — |
| §8 冲突 | 「规则不相等 vs 模型一致」一律人工；「规则相等 vs 模型非一致」默认人工，`grade.conflict_policy=规则优先` 且快速模式时规则优先并强制抽检 | 与规范一致 | — |
| §8 聚合冲突 | 聚合 ≠ 声明 → 采用聚合值（判定按聚合重建）并记冲突 → 人工（不采用模型分数） | 与规范一致；20 题夹具 Q19 断言 | — |
| §11/§13 测试 | 无真实模型/OCR 依赖：全部 fixture；分支覆盖未在 stable 量化 | TODO nightly 分支覆盖；接 inference 后补回放 | — |
| §3 平台配置 | 本 crate 自带与 CONFIG 同构默认值，未依赖 platform；映射 TODO | 分层解释 | 待 Lead 确认 |

## 8. 接口 TODO（并行依赖接线）

1. `RubricSource`：用 `eg-rubric` 的「已确认规则」查询实现；草稿返回 `Draft`（当前 fixture 已覆盖）。
2. `JudgeBackend`：用 `eg-inference` 的 judge 角色实现请求/响应映射、校准 id、修复重试、超时、云端授权（`consent_id`）；`ClassifierBackend` 用 router 角色实现 R9。
3. `IdGenerator`/`Clock`/`IdempotencyStore`：分别接 `platform::UlidFactory`/`SystemClock`/`repo`；`GradeConfig` 从 `platform::AppConfig { grade, fast }` 映射。
4. `grade` 之外的落库：`ComparatorTrace`/`ModelCall`/`GradeRecord` 由 platform repo 持久化；本 crate 返回完整对象（已验证域不变量）。
5. 分支覆盖：CI 增加 nightly `cargo llvm-cov --branch` 守护（目标 ≥90%）。

## 9. 复现命令

```bash
source "$HOME/.cargo/env"
cargo test -p eg-grade
cargo clippy -p eg-grade --all-targets -- -D warnings
cargo fmt -p eg-grade -- --check
cargo llvm-cov -p eg-grade --summary-only
# 局部：路由表 / 20 题夹具 / 抽检 / 幂等与非法输出
cargo test -p eg-grade --test routing_readiness
cargo test -p eg-grade --test fixtures_20
cargo test -p eg-grade --test modes_sampling
cargo test -p eg-grade --test idempotency_validation
```

---

# 附：T30 全量接入与提交级判分（M7，2026-11-03）

> 状态：已完成（T30）· 对应 `SPEC-grade.md` §5/§7/§8/§9、`SPEC-align.md`、`SPEC-rubric.md` §2.6、`SPEC-inference.md` §4/§9、`SPEC-platform.md`、`CONFIG.md` §5/§6。
> 本段承接上文（T16–T18 已交付）；记录真实适配器、提交流水线、断点/幂等、引擎指纹与平台缺口。

## A1. 新增能力与文件

| 文件 | 内容 |
|---|---|
| `src/adapters/rubric.rs` | `RubricStoreSource`：`RubricSource` ← `eg-rubric::ports::RubricStore`；已确认生效规则 → `Confirmed`，草稿 → `Draft`（就绪检查拦截），读取失败 fail-safe 为 `Missing`（规则题转人工，绝不猜规则）；`active_version` 进幂等键；来源身份 `rubric-store-v1` 进引擎指纹 |
| `src/adapters/inference.rs` | `InferenceJudge`/`InferenceClassifier`：`JudgeBackend`/`ClassifierBackend` ← `eg-inference::InferenceService`（`ExpectedSchema::Judge/Router`、temperature 0、超时来自 `grade.judge_timeout_s`、注册表指纹进引擎指纹）；真实服务已落库的 `ModelCall` 通过 `JudgeResponse.model_call` 回传，`grade` 不重复落库；Schema 错误映射为可修复重试，策略/授权/预算拦截映射为人工（`PolicyBlocked`），超时映射 `Timeout` |
| `src/adapters/platform.rs` | `PlatformIdGenerator`（`UlidFactory`，失败显式）、`PlatformClock`（`SharedClock`）、`PlatformGradeStore`（`grade_record`+`comparator_trace`+`model_call`+审计同事务 `business_write`，重复记录幂等跳过）、`PlatformIdempotencyStore`（进程内；平台无幂等表缺口） |
| `src/adapters/align.rs` | `AlignSource` ← `eg-align::AlignReport`（`AlignReportProvider` 组合根执行 `eg_align::align`；把 answers/逐题 `auto_grade_allowed`/整卷 `auto_grade_ready`/未决投影为判分门禁；未决题只进失败/澄清，不伪装未作答；align 引擎版本进指纹） |
| `src/align.rs` | `AlignSource` trait（`AlignedSubmission` = 答案 + 逐题门禁 + 整卷 ready + 未决）、`AlignIssue`、`FixtureAlignSource`：`StudentAnswer/parts` → `QuestionAnswer`（识别证据保留；图片资产由平台懒加载） |
| `src/submission.rs` | `SubmissionPipeline`：对齐 → 逐题判分（复用 `Grader`）→ 落库（`GradeStore`）→ 断点（`ProgressStore`）→ 抽检标记 → 提交汇总；`report_exported` 只置 `report_refresh_required`（不自动刷新）；`force` 支持强制重跑（仍走幂等键） |

`ports` 扩展：`IdGenerator::try_next_ulid`（平台失败显式）、`RubricSource::{active_version,identity}`、`JudgeResponse/ClassificationResponse.model_call`、`JudgeFailure::{InvalidOutput,PolicyBlocked}`、`JudgeRequest.judge_timeout_s`、`SubmissionRequest.engine_stamps`、`GradedQuestion.unpersisted_model_calls`（真实推理已落库调用不重复写）。

## A2. 结论

| 问题 | 结论 |
|---|---|
| rubric 适配 | 只把**已确认生效规则**当可用；草稿返回 `Draft`（就绪检查报错）；读取失败 fail-safe → `Missing`；生效版本写入幂等键，规则升版即缓存失效。 |
| inference 适配 | 一次判定由推理服务完成完整时序（注册表/上下文/预算/授权/熔断/Schema/ModelCall 落库/审计）；适配器只做请求组装与结果映射。真实 `ModelCall`（含 consent、engine、latency、cost）回传 grade 作为证据，避免重复记账；Schema 不合法 → `InvalidOutput` 触发 grade 修复重试一次，再失败转人工；策略/授权/许可 → `PolicyBlocked` 人工；超时 → `Timeout`。 |
| platform 适配 | id/时钟直接用平台；落库一个 `business_write` 事务内写 grade_record + comparator_trace + 未落库 model_call + 审计；重复记录（重跑/断点恢复）幂等跳过。 |
| 提交流水线 | 对齐结果缺失 → 整卷 `Align` 错误；单题判分失败进入 `grade.failures` 且其余题继续（不静默丢题）；完成后写断点；已完成且未 force 时从落库记录重建汇总（0 次判分/计费）；force 重跑命中幂等键仍不重复；抽检 `sampled` 标记随记录落库。 |
| 引擎指纹 | = 配置哈希 + 学科 id/提示词版本/比较器 + 规则来源身份 + judge/classifier 身份（inference 含注册表指纹/模型版本/提示词 Schema 版本）+ 对齐来源身份/引擎版本（`AlignSource::engine_stamp`，自动注入）+ 外部 `engine_stamps`（OCR/CAS 等，按内容排序）。任一变化 → 幂等键变化 → 缓存失效并重判。 |
| align 接入（T29 后收尾） | `AlignReportSource` 消费 `eg_align::AlignReport`：answers 仅含可构造题；逐题 `auto_grade_allowed` 决定是否送判分；未放行题缺失答案进入 failures 并把原因改写为「对齐未决（kind）：detail」；整卷级阻断未决（身份歧义/重复/名单外等 `question_id=None`）→ `SubmissionPipelineError::AlignGate` 整卷拒绝自动判分；整卷 ready 与阻断题通过 `SubmissionRun.align_gate` 暴露。 |
| 报表刷新 | `report_exported=true` 且本次有新落库时只置 `report_refresh_required`，不自动刷新（记录维度）；已完成提交再读取时为 false。 |

## A3. 测试与实测

| 项目 | 结果 |
|---|---|
| 端到端 | `tests/submission_t30.rs`：20 题 × 3 学生，逐题（路由、判定、得分、状态）与 T16–T18 夹具预期 0 diff；60 条记录全部落库；每生 17 自动接受 + 3 待复核 |
| 断点/幂等 | 已完成提交再跑：`persisted=0`、`resumed=20`、模型调用次数不变；`force` 重跑命中幂等键：`persisted=0`、模型调用不变；外部 engine stamp 变化：指纹变化、模型调用 +8、重新落库 20 |
| 失败隔离 | 缺 1 题对齐答案：`failures=1`（题 id 正确）、其余 19 题成功落库、断点记录 19 成功 + 1 失败，整卷不失败 |
| 抽检标记 | 快速模式运行后 `sampling` 有清单/种子；落库记录中抽中题 `sampled=true` |
| 适配器 | `tests/adapters_t30.rs`：rubric 生效/版本/Missing；inference judge/router 夹具回放 + 真实 `ModelCall` 落库计数；platform 落库（记录/轨迹/调用/审计）、重复幂等、id/时钟 |
| 依赖实测 | `eg-rubric`/`eg-inference`/`eg-platform` 均可编译；`eg-subject-math` 仍仅为 dev-dependency（生产仅依赖 `SubjectPack` 契约） |
| 测试总数 | 49 个测试函数（lib 19 + adapters_t30 4 + align_t30 3 + coverage_paths 4 + fixtures_20 1 + idempotency_validation 4 + modes_sampling 5 + routing_readiness 4 + submission_t30 5）全部通过 |
| align 集成测试 | `tests/align_t30.rs`：真实 `eg_align::align` 报告（模板 19/20 区域，1 题未决）× 2 学生 → grade 19 题全部 0 diff、未决题 1 条显式 failure（`对齐未决（missing_page）`，非未作答）、`align_gate.auto_grade_ready=false`/阻断题正确、落库 38 条；整卷身份未决（文件名不匹配）→ `AlignGate` 拒绝自动判分、0 落库、0 模型调用；align 引擎版本变化 → 引擎指纹变化 |
| 验证命令 | `cargo test -p eg-grade`、`cargo clippy -p eg-grade --all-targets -- -D warnings`、`cargo fmt -p eg-grade -- --check` 全绿 |
| 跨 crate | platform v2 增 `ConsentState::Exhausted` 曾短暂阻断 eg-inference 编译（已由 model-ops 修复，eg-grade 未改动其他 crate）；期间仅等待，无越界修改 |

## A4. 平台缺口（已报告 Lead，platform v2 / task-42 承接）

1. **无判分幂等表**（platform v3 正在补）：`grade_record` 无幂等键列；`PlatformIdempotencyStore` 目前进程内。重启后首次重跑可能多判一次（重复修订被唯一约束拒绝后由落库幂等跳过，不重复落库；但会产生一次模型调用）。建议 platform 增加 `grade_idempotency(key, grade_id)` 表或任务幂等键可复用。
2. **无判分断点/进度表**（platform v3 正在补）：`ProgressStore` 仍为内存 fixture；建议用 platform 任务队列（`task.payload` 记录提交/已处理题）或新增断点表。
3. **repo 查询能力**：`list_grade_records(conn, submission_id)` 只能按提交拉全部修订；无「按 grade_id 单条」「按状态/模式/题过滤」「最新修订视图」「游标分页」；review 的队列需求同样受阻（task-42）。
4. **资产/识别组合查询**：详情/证据懒加载需要按 `EvidenceRef` 批量取资产/识别摘要，当前需多次单查询。
5. **审计字段**：`NewAuditEntry` 无命令幂等键/原因码/范围等；grade 落库审计用 note/hash 承载，语义查询不足。

## A5. 偏差与限制

| 规范位置 | 偏差/解释 | 处理 |
|---|---|---|
| 依赖（SPEC-grade §1） | 新增直接依赖 `eg-rubric`、`eg-inference`、`eg-platform`（真实适配所需）；`eg-subject-math` 仍为 dev-dependency（生产用 `SubjectPack` trait；数学包由组合根注入） | 研究记录 + 组合根注入 |
| align（SPEC-align/T29） | 已接真实实现：`AlignReportSource` ← `eg_align::AlignReport`（组合根实现 `AlignReportProvider` 调 `eg_align::align`）；未决题/整卷门禁按 align 报告投影；`FixtureAlignSource` 仅测试/回放 | 与 align 交付一致（本 crate 不改 align） |
| 断点 | 按提交/题粒度记录进度；已完成提交直接从落库记录重建（0 重判）；`force` 可强制重跑（仍幂等） | 与规范一致；平台断点表缺口见 A4 |
| 报表刷新 | 不自动刷新导出报表，仅置 `report_refresh_required` 标记；导出系统由 report 模块消费 | 记录维度，符合规范 |
| 引擎指纹 | 规范要求含 CAS/OCR/模型版本：本实现由 `engine_stamps` 注入（OCR/对齐/CAS 由组合根提供），inference 注册表/模型版本自动纳入；数学 CAS 引擎 stamp 由调用方放入 `engine_stamps` | 组合根职责 |
| 平台幂等/断点 | 见 A4；当前可运行但重启后幂等不完全持久 | 待 platform v2 |
| inference 分类任务 id | `ClassifierBackend` 接口增加 `task_id`（推理服务必需）；fixture/测试已同步 | 契约扩展，记录于此 |

## A6. 复现

```bash
source "$HOME/.cargo/env"
cargo test -p eg-grade
cargo test -p eg-grade --test submission_t30
cargo test -p eg-grade --test adapters_t30
cargo clippy -p eg-grade --all-targets -- -D warnings
cargo fmt -p eg-grade -- --check
```
