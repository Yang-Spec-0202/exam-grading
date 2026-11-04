# Task List：exam-grading 实现任务

> 版本：v0.2 · 2026-11-05 · 状态：执行中（T01–T41 逐项状态见下方「状态总览」；S1 backend 自动化已 PASS）
> 规范基线：`SPEC.md` v0.2.1 + 12 份模块规范 + `CONFIG.md` + `AGENTS.md`
> 规则：每个任务先写 `docs/dev/research/<module>.md`（研究记录）再写代码；任务完成必须通过该任务的「验证」；每 3–4 个任务一个检查点（见 `tasks/plan.md` §6）。
> 规模：S=1–2 文件，M=3–5 文件；所有任务均须 ≤5 文件、单次会话可完成。

## 状态总览（2026-11-05，滚动快照）

> 标记：✅ 完成（验收项已满足并有证据）· 🟡 部分完成（主路径可用，存在明确 TODO）· ⬜ 未开始。
> 状态不等同于 M10 验收通过；S1 backend 之外的真实数据/人工项见 T41 与 `docs/dev/acceptance.md`。

| 任务 | 状态 | 证据（一行） |
|---|---|---|
| T01 | ✅ | `cargo build --workspace`；`rust-toolchain.toml`/`Cargo.toml` |
| T02 | ✅ | `docs/dev/research/{deps,toolchain,slint,models,pdf}.md` 版本实测 |
| T03 | ✅ | `scripts/ci.sh`、`scripts/docs.sh`/`docs-check.sh`、`docs/book` |
| T04 | ✅ | `cargo test -p eg-domain`（score/ids/hash/time） |
| T05 | ✅ | `eg-domain` paper/answer/rubric/grade + insta 快照 |
| T06 | ✅ | `crates/eg-domain/tests/contract.rs` 契约套件 |
| T07 | ✅ | `cargo test -p eg-platform`（config/secrets 0 偏差） |
| T08 | ✅ | schema v3 + `migrations/` + repo 测试；`PRAGMA integrity_check` |
| T09 | ✅ | 资产去重/单实例测试（同字节 40 次 1 文件） |
| T10 | ✅ | tasks 状态机/租约/重试 + 强杀恢复（CP-B） |
| T11 | ✅ | log/audit/index/diagnostics 测试；审计链哈希校验 |
| T12 | ✅ | backup/restore/clock 测试；`.bak` 恢复 |
| T13 | ✅ | `cargo test -p eg-subject-math`（归一化） |
| T14 | ✅ | 基础比较器测试（数值/表达式/选项） |
| T15 | ✅ | `cargo test -p eg-rubric`（草稿/模板/静态校验） |
| T16 | ✅ | `cargo test -p eg-grade`（L1 路由/就绪） |
| T17 | ✅ | L2 比较器、聚合、输出校验测试 |
| T18 | ✅ | 模式/门禁/抽检/幂等测试（含抽检只读负向） |
| T19 | ✅ | inference 注册表/fixture/许可门禁测试 |
| T20 | 🟡 | sidecar 生命周期与任务授权测试通过（fixture/模拟）；真实 llama.cpp 未验收 |
| T21 | ✅ | `cargo test -p eg-capture`（导入/拆页/PDF） |
| T22 | ✅ | 预处理/质量门控/拒收测试（D1 已修） |
| T23 | ✅ | recognize 页级识别+缓存（fixture OCR 回放，0 diff） |
| T24 | ✅ | 段级/置信度合成；畸形输出 0 流入上层 |
| T25 | 🟡 | 全国卷模板 active/retired + 草稿确认已测；模型整理真实后端未接（app 显式失败） |
| T26 | ✅ | subject-math 全符号比较器测试（抽检 ≥95% 夹具） |
| T27 | 🟡 | CAS 适配/降级测试通过；真实 sympy sidecar 未参与验收 |
| T28 | ✅ | 文件名身份/未决测试（零错配，名单外 100% 未决） |
| T29 | ✅ | 区域对齐/跨页测试 |
| T30 | ✅ | grade 全量接入 + 提交级判分；platform 适配器测试 |
| T31 | ✅ | review 服务/审计（命令落库可撤销） |
| T32 | ✅ | 抽检只读/转标准/重跑；负向用例 `negative_sampled_readonly` |
| T33 | ✅ | report 统计口径 + 双模式单列测试 |
| T34 | ✅ | 导出 XLSX/CSV/HTML + 重放一致（acceptance 两次导出摘要一致） |
| T35 | ✅ | Slint 外壳/导航/通道；`--bench` p99 门禁；`--smoke` 十页 |
| T36 | ✅ | 导入/进度/设置；真实导入 9 文件→8 提交（app.md §8.8） |
| T37 | 🟡 | 三栏/键盘/改分/原因/抽检完成；真实 RegradeEngine、redo 持久化、IME 人工验收未做 |
| T38 | 🟡 | 授权/诊断接真实 platform；规则模型整理未接（显式失败），页面为最小可用 |
| T39 | 🟡 | egctl doctor/replay/bench/acceptance 可用；判分回放与 bench 全项 TODO |
| T40 | ✅ | Pages 已上线并验证：[首页](https://yang-spec-0202.github.io/exam-grading/) / 研究页 / acceptance 页均 200；workflow `pages.yml`（CI run 41497887203 / Docs run 41497887182） |
| T41 | 🟡 | S1 外部夹具端到端 PASS（287 断言/9 负向/12 步，~1.8s）；S2/S3/S5/S7 与 UI 人工、真实模型待办 |

> 统计：✅ 34 · 🟡 7 · ⬜ 0（T41 内 S2/S3/S5/S7 与人工项按 ⬜ 计，见 T41 说明与本文件末「待做」；CP-F 已 ✅，见 `tasks/plan.md` §6）。

## M0 环境与骨架

### T01 工具链与 workspace 骨架
- 描述：安装 Rust stable（rustup + clippy/rustfmt），创建 `Cargo.toml` workspace 与 13 个空 crate（domain/platform/capture/inference/recognize/align/rubric/grade/subject-math/review/report/app/egctl），统一 edition 2024、`[workspace.lints]`、`rust-toolchain.toml`。
- 验收：`cargo build --workspace` 成功；13 个 crate 可被 `cargo metadata` 识别；无警告。
- 验证：`cargo build --workspace`；`cargo metadata --no-deps -q | 断言 crate 数`。
- 依赖：无。文件：`Cargo.toml`、`rust-toolchain.toml`、`crates/*/Cargo.toml`。
- 规模：M。

### T02 依赖选型技术验证（检索优先）
- 描述：按 `AGENTS.md` 逐个验证：Slint 版本与窗口/图片/线程 API、rusqlite、PDF 渲染候选、图像/EXIF、XLSX、keyring、日志库、模型 serving、sympy sidecar；结论写入 `docs/dev/research/platform.md` 等；记录实测版本。
- 验收：每个依赖有来源、版本、限制、失败预案；PDF/CAS/serving 三项有 go/no-go 结论。
- 验证：研究记录齐全；最小示例可编译（验证后删除示例代码，不留在仓库）。
- 依赖：T01。文件：`docs/dev/research/*.md`、临时验证工程（不入库）。
- 规模：M。

### T03 CI、质量门禁与文档骨架
- 描述：`deny.toml`（许可白名单）、CI 脚本（fmt/clippy/test/deny/docs 链接检查）、覆盖率脚本、`docs/` 目录树与 `docs/book/` mdBook 配置（GitHub Pages）。
- 验收：CI 本地可跑；`cargo deny check` 通过；`mdbook build` 成功；文档链接检查脚本可运行。
- 验证：`bash scripts/ci.sh`；`cargo deny check`；`mdbook build docs/book`。
- 依赖：T01。文件：`deny.toml`、`scripts/ci.sh`、`docs/book/book.toml`、`docs/README.md`。
- 规模：M。

## M1 领域与基础设施

### T04 domain 值对象与错误
- 描述：`MilliScore`（千分之一分、解析/格式化/受检加法）、`Confidence`、`ContentHash`/`AssetId`、ULID 新类型、`Sha256Hex`、`Clock`、`ScoreError`。
- 验收：解析/越界/溢出用例齐全；`from_points` 往返；构造失败返回错误不 panic。
- 验证：`cargo test -p eg-domain`；性质测试 ≥200 例。
- 依赖：T01。文件：`crates/eg-domain/src/{ids,score,confidence,hash,time,error}.rs`。
- 规模：M。

### T05 domain 试卷、答案键、规则、判分、模式类型
- 描述：按 `SPEC-domain.md` §3–§8 实现 `Paper/Question/AnswerKey/AnswerValue/Rubric/RubricDraft/GradeRecord/Judgement/RouteDecision/Evidence/ModelCall` 与 `GradeMode`、`ReviewReasonKind`、`SubmissionExclusion`；含不变量校验与序列化。
- 验收：字段与规范逐字一致；不变量有断言；序列化快照（insta）稳定；草稿不可直接构造生效规则。
- 验证：`cargo test -p eg-domain`；快照审核。
- 依赖：T04。文件：`crates/eg-domain/src/{paper,answer,rubric,grade,evidence}.rs`。
- 规模：M。

### T06 domain 学科契约与契约测试套件
- 描述：`SubjectPack`/`Comparator`/`ComparatorVerdict`/`RawAnswer`/`StudentAnswer`/`NormalizedAnswer`/`StepEvidence`/`RubricIssue`；写契约测试（含反例包）。
- 验收：契约测试可对不同实现复用；解析失败不得产出「不相等」；反例包被正确拒绝。
- 验证：`cargo test -p eg-domain`。
- 依赖：T05。文件：`crates/eg-domain/src/subject.rs`、`crates/eg-domain/tests/contract.rs`。
- 规模：S。

### T07 platform 配置与密钥
- 描述：`CONFIG.md` 全部键的强类型映射与默认值（缺键补齐、非法值拒绝）、配置原子写与 `.bak`、Keychain 密钥存取（不入库/日志）。
- 验收：默认值与 CONFIG 0 偏差；非法配置在启动期报错并给中文提示；密钥不出现在日志/数据库。
- 验证：`cargo test -p eg-platform`；对数据目录 `grep` 密钥 0 命中。
- 依赖：T05。文件：`crates/eg-platform/src/{config,secrets}.rs`。
- 规模：M。

### T08 platform SQLite schema、迁移与仓储
- 描述：首批表（classroom/student/paper/question/submission/page/segment/recognition/grade_record/model_call/audit_log/task/task_consent/asset）与索引、只追加迁移、单写连接 actor、只读查询连接。
- 验收：v0→v1 迁移成功；迁移前快照；外键开启；事务边界正确；仓储 CRUD 测试。
- 验证：`cargo test -p eg-platform`；`PRAGMA integrity_check`。
- 依赖：T07。文件：`crates/eg-platform/src/{db,repo,schema}.rs`、`migrations/`。
- 规模：M。

### T09 platform 资产库与单实例
- 描述：BLAKE3 内容寻址写读校验、去重、原图只读、无删除路径、软配额告警、单实例锁。
- 验收：同字节 40 次导入 1 文件；文件先落盘后提交；无删除 API；第二实例 1s 内退出。
- 验证：`cargo test -p eg-platform`；子进程模拟第二实例。
- 依赖：T08。文件：`crates/eg-platform/src/{assets,instance}.rs`。
- 规模：S。

### T10 platform 任务队列
- 描述：状态机、幂等键、租约与心跳、取消令牌、分阶段断点恢复、重试退避、优先级。
- 验收：强杀重启后 running 归位；幂等键不重复执行；取消保留证据；并发不超限。
- 验证：`cargo test -p eg-platform`；SIGKILL 集成测试。
- 依赖：T09。文件：`crates/eg-platform/src/{tasks,cancel}.rs`。
- 规模：M。

### T11 platform 日志、审计、诊断与索引
- 描述：JSONL 日志（级别/模块覆盖/脱敏/轮转/保留 30 天）、审计哈希链（恒开）、诊断包（脱敏）、近 30 天全文索引与启动校验/手动重建。
- 验收：PII 扫描 0 命中；审计写失败拒绝业务写；索引增量与重建一致；诊断包不含原图/姓名/密钥。
- 验证：`cargo test -p eg-platform`；日志脱敏夹具；索引重建对比。
- 依赖：T10。文件：`crates/eg-platform/src/{logging,audit,diagnostics,index}.rs`。
- 规模：M。

### T12 platform 备份、恢复与时钟
- 描述：迁移前/手动备份、恢复后全量哈希校验、`SystemClock`/`FixedClock`、ULID 单例单调。
- 验收：恢复后业务可读；时钟回拨不倒退；测试用固定时钟。
- 验证：`cargo test -p eg-platform`。
- 依赖：T11。文件：`crates/eg-platform/src/{backup,clock,ids}.rs`。
- 规模：S。

**检查点 CP-A / CP-B**：见 `tasks/plan.md` §6；`cargo test --workspace` + 崩溃恢复/去重/日志索引/审计链测试全绿，人工验收后进 M2。

## M2 判分核心（fixture 先行）

### T13 subject-math 归一化
- 描述：字符/噪声/LaTeX/结构四层归一化、内部语法树（数值/变量/和积幂/分式/根式/方程/区间/集合/向量/矩阵/单位/布尔/选项）、错误语义。
- 验收：幂等；解析失败显式错误；≥30 条归一化夹具通过。
- 验证：`cargo test -p eg-subject-math`。
- 依赖：T06。文件：`crates/eg-subject-math/src/{normalize,ast}.rs`。
- 规模：M。

### T14 subject-math 基础比较器
- 描述：选择/布尔/数值容差/带单位/分数/表达式规范形/解集/区间/集合；比较器链顺序与终止规则。
- 验收：数值/单位/分数/解集/区间夹具全通过；「无法判定」均带原因；容差默认严格。
- 验证：`cargo test -p eg-subject-math`。
- 依赖：T13。文件：`crates/eg-subject-math/src/{compare,chain}.rs`。
- 规模：M。

### T15 rubric 草稿、模板与静态校验（无模型）
- 描述：草稿三来源/四状态、模板库（全国卷包快照+自定义）、11 条静态校验、确认/拒绝、版本递增、审计。
- 验收：坏规则全部拦下；确认后才生成生效 Rubric；模板缺失禁止套用；草稿不可编辑已确认版本。
- 验证：`cargo test -p eg-rubric`。
- 依赖：T12、T14。文件：`crates/eg-rubric/src/{draft,templates,validate,confirm}.rs`。
- 规模：M。

### T16 grade L1 路由与就绪检查
- 描述：R0–R11 有序路由表、路由轨迹、试卷就绪条件、分类澄清占位（fixture）。
- 验收：每行至少 1 正例 + 边界；草稿规则不参与；缺答案不调模型。
- 验证：`cargo test -p eg-grade`；路由表分支覆盖 100%。
- 依赖：T15。文件：`crates/eg-grade/src/{route,readiness}.rs`。
- 规模：M。

### T17 grade L2 比较器、聚合与输出校验
- 描述：比较器执行、规则聚合（累加/全对/分步）、模型输出硬校验（得分/粒度/一致性/逐项覆盖）、冲突检测。
- 验收：非法输出 100% 拒绝；聚合与声明不一致进冲突；得分边界测试。
- 验证：`cargo test -p eg-grade`。
- 依赖：T16。文件：`crates/eg-grade/src/{l2,aggregate,validate}.rs`。
- 规模：M。

### T18 grade 模式、门禁、抽检与幂等
- 描述：标准/快速阈值表、门禁原因、抽检算法（比例 0.20/权重 0.70/种子记录）、冲突策略、幂等键、提交级汇总。
- 验收：两套阈值分别断言；抽检集合可复现；冲突必进人工；幂等键命中不重复计费。
- 验证：`cargo test -p eg-grade`；抽检 40 组随机数据 0 差异。
- 依赖：T17。文件：`crates/eg-grade/src/{mode,gate,sampling,idempotent}.rs`。
- 规模：M。

**检查点 CP-C**：20 题夹具（含「略」/仅规则/多解/分步/低置信）判分 0 diff；路由/聚合/模式/抽检测试全绿。

## M3–M4 采集、识别与推理

### T19 inference 注册表、fixture 后端与许可门禁
- 描述：模型注册表（角色/版本/量化/哈希/许可/超时/并发）、fixture 回放、许可白名单与排除清单 fail-closed。
- 验收：注册表外配置启动失败；许可违例加载失败；fixture 同输入字节一致。
- 验证：`cargo test -p eg-inference`。
- 依赖：T12。文件：`crates/eg-inference/src/{registry,fixture,license}.rs`。
- 规模：M。

### T20 inference 本地后端生命周期与任务授权
- 描述：sidecar 启停/端口/探活/熔断/预算、任务级授权校验（五维）、`ModelCall` 落库、云端默认关。
- 验收：无授权 0 外联；熔断状态机测试；sidecar 无孤儿进程；费用记账。
- 验证：`cargo test -p eg-inference`；stub HTTP 服务注入 429/500/超时。
- 依赖：T19。文件：`crates/eg-inference/src/{local,consent,cost,circuit}.rs`。
- 规模：M。

### T21 capture 导入与拆页
- 描述：格式白名单+magic 校验、文件名元数据无损入库、命名提醒（仅提示）、BLAKE3 去重、PDF 拆页、部分成功报告。
- 验收：同字节 40 次 1 资产；一页失败不拖垮批次；文件名不被改写；PDF 页序正确。
- 验证：`cargo test -p eg-capture`；夹具含损坏文件/重复文件。
- 依赖：T12。文件：`crates/eg-capture/src/{import,names,pdf,dedupe}.rs`。
- 规模：M。

### T22 capture 预处理与质量门控
- 描述：EXIF 方向/镜像、透视矫正、派生图顺序、质量阈值（CONFIG）、Reject 行为、流水线版本。
- 验收：阈值边界两侧行为正确；派生可重建；镜像不丢；原图只读。
- 验证：`cargo test -p eg-capture`；30 页金标集（构造）。
- 依赖：T21。文件：`crates/eg-capture/src/{preprocess,quality,pipeline}.rs`。
- 规模：M。

### T23 recognize 页级识别与缓存
- 描述：页级识别请求、提示词渲染（`ocr.emit_latex`）、结构校验（inference）→ 语义校验（recognize）、缓存键、置信度合成初版。
- 验收：Schema 违规 100% 拒绝；缓存命中不重复调用；关闭 LaTeX 时公式转人工。
- 验证：`cargo test -p eg-recognize`；fixture 回放。
- 依赖：T20、T22。文件：`crates/eg-recognize/src/{page,validate,cache,confidence}.rs`。
- 规模：M。

### T24 recognize 段级与置信度合成
- 描述：段级识别按需触发、公式 LaTeX 规范化与合法性、手写降级（RawAnswer 人工修正）、页/题级置信度合成。
- 验收：段级触发条件生效；非法 LaTeX 转人工；手写不伪造 Recognition；置信度规则表 100% 断言。
- 验证：`cargo test -p eg-recognize`。
- 依赖：T23。文件：`crates/eg-recognize/src/{segment,latex,handwriting}.rs`。
- 规模：M。

**检查点 CP-D**：图片→识别夹具 0 diff；畸形模型输出 0 流入；缓存/许可/熔断测试通过。

## M5–M6 规则整理、全符号与对齐

### T25 rubric 模型整理与全国卷模板数据
- 描述：structurer 角色调用、严格输出校验、草稿生成、确认版本化、四套全国卷模板数据文件（快照）与自定义导入。
- 验收：模型只产草稿；不合法输出不生成草稿；模板数据校验通过；确认后版本递增且审计可追溯。
- 验证：`cargo test -p eg-rubric`；fixture 模型输出含畸形样例。
- 依赖：T19、T24。文件：`crates/eg-rubric/src/{structure,version}.rs`、`assets/templates/`。
- 规模：M。

### T26 subject-math 全符号比较器
- 描述：三角（恒等）、向量、矩阵、集合/区间的等价比较；覆盖表与限制说明同步。
- 验收：四类新夹具通过；超出范围明确「无法判定」；比较器链顺序稳定。
- 验证：`cargo test -p eg-subject-math`；抽检 20 题/类。
- 依赖：T14。文件：`crates/eg-subject-math/src/{trig,vector,matrix,setlike}.rs`。
- 规模：M。

### T27 subject-math CAS 适配
- 描述：sympy sidecar 启停/超时/降级、等价请求协议、结果缓存、版本入引擎指纹。
- 验收：不可用/超时均降级为无法判定；缓存命中一致；CAS 调用不含个人信息。
- 验证：`cargo test -p eg-subject-math`；stub sidecar 超时/崩溃。
- 依赖：T26。文件：`crates/eg-subject-math/src/cas.rs`、`scripts/cas/`。
- 规模：M。

### T28 align 文件名身份与未决
- 描述：文件名归一与名单匹配、未决列表、人工选择、审计、重复/缺页检测。
- 验收：零错配（500 份合成夹具）；未命中 100% 进未决；OCR 兜底默认 0 调用。
- 验证：`cargo test -p eg-align`。
- 依赖：T22、T24。文件：`crates/eg-align/src/{identity,unresolved}.rs`。
- 规模：M。

### T29 align 区域对齐与跨页
- 描述：模板区域（版本校验）、版面检测、题号文本、人工；跨页 `StudentAnswer.parts`；门禁。
- 验收：三级路径置信度符合规范；模板版本不匹配禁用模板；跨页拼接正确；缺页显式。
- 验证：`cargo test -p eg-align`。
- 依赖：T28。文件：`crates/eg-align/src/{region,template,crosspage}.rs`。
- 规模：M。

**检查点 CP-E**：10 条手写规则→草稿→确认全链路；数学抽检 ≥95%；文件名未命中 100% 未决。

## M7–M8 判分全量、复核与导出

### T30 grade 全量接入与提交级判分
- 描述：接入 align/rubric/subject-math，提交级判分、断点/幂等、快速抽检标记、失败汇总。
- 验收：端到端 fixture 0 diff；单题失败不影响其余题；重跑幂等；抽检标记写入记录。
- 验证：`cargo test -p eg-grade`；`egctl replay`。
- 依赖：T25、T27、T29。文件：`crates/eg-grade/src/{submission,pipeline}.rs`。
- 规模：M。

### T31 review 服务与审计
- 描述：队列过滤/分页、详情、命令（接受/改分/修文本重判/排除/批量）、撤销/重做、审计与乐观锁。
- 验收：每条命令落审计；改分只追加修订；撤销 50 步；冲突刷新。
- 验证：`cargo test -p eg-review`。
- 依赖：T30。文件：`crates/eg-review/src/{queue,detail,commands,undo}.rs`。
- 规模：M。

### T32 review 抽检只读、转标准与重跑
- 描述：抽检队列只读、抽检通过/不通过、转标准、重跑范围（单题/整卷）与弹窗协议。
- 验收：只读队列 100% 拒绝编辑；转标准生成新修订；未确认不自动重跑。
- 验证：`cargo test -p eg-review`。
- 依赖：T31。文件：`crates/eg-review/src/{sampling,transfer}.rs`。
- 规模：S。

### T33 report 统计口径与双模式
- 描述：有效样本定义、单题/整卷/班级统计、区分度、分布、双模式单列、抽检说明。
- 验收：公式与四舍五入边界测试；混合模式不入统计；n<30 区分度 n/a。
- 验证：`cargo test -p eg-report`。
- 依赖：T30。文件：`crates/eg-report/src/{aggregate,stats,mode}.rs`。
- 规模：M。

### T34 report 导出与重放
- 描述：XLSX/CSV/HTML 列定义、文件名规则、原子写、导出审计、重放比较。
- 验收：1000 份 ≤15s；半文件 0；重放摘要一致；脱敏模式可用。
- 验证：`cargo test -p eg-report`；`egctl report replay`。
- 依赖：T33。文件：`crates/eg-report/src/{xlsx,csv,html,naming,atomic}.rs`。
- 规模：M。

**检查点 CP-F**：端到端 20 题×3 学生 0 diff；导出重放一致；抽检/重跑审计完整。

## M9 桌面 UI

### T35 app 外壳、导航与通道
- 描述：Slint 主窗口、十页导航、全局状态、有界命令/事件通道、`invoke_from_event_loop`、启动与切页预算。
- 验收：冷启动 P95 ≤1.5s；切页 P95 ≤100ms；UI 线程 p99 ≤16ms；通道满显式提示。
- 验证：手动冒烟 + `egctl bench ui` 初版。
- 依赖：T31。文件：`crates/eg-app/src/{main,state,channel,nav}.rs`、`ui/app.slint`。
- 规模：M。

### T36 app 导入、进度与设置
- 描述：拖拽/对话框导入（命名提醒）、进度页、设置页覆盖 CONFIG 12 组（含只读不变量）、配置校验与回滚。
- 验收：设置项全部生效；非法值拒绝并提示；命名提醒只出现一次；导入失败可见。
- 验证：手动清单 + 配置夹具。
- 依赖：T35、T22。文件：`crates/eg-app/src/pages/{import,progress,settings}.rs`、`ui/pages/*.slint`。
- 规模：M。

### T37 app 复核工作台
- 描述：三栏布局、图片缩放/证据联动、键盘快捷键、改分/原因、修文本重判、批量、撤销、抽检弹窗。
- 验收：纯键盘 20 题 ≥15 题/分钟；抽检只读；重跑弹窗默认单题；证据联动正确。
- 验证：手动键盘 E2E 清单 + 性能记录。
- 依赖：T36、T32。文件：`crates/eg-app/src/pages/review.rs`、`ui/pages/review.slint`。
- 规模：M。

### T38 app 规则页、授权页与日志诊断页
- 描述：规则原文→整理→草稿→校验→确认/拒绝；任务授权列表/新建/撤销；日志检索与诊断包导出。
- 验收：规则确认流程完整；无授权时判分暂停并提示；日志可检索/过滤；诊断包不含敏感数据。
- 验证：手动清单 + 审计抽查。
- 依赖：T37、T25、T20。文件：`crates/eg-app/src/pages/{rules,consent,diagnostics}.rs`、`ui/pages/*.slint`。
- 规模：M。

**检查点 CP-G**：键盘复核 ≥15 题/分钟；UI 线程无阻塞；模式/设置不静默切换。

## M10 验收与发布

### T39 egctl 回放、基准与文档检查
- 描述：`replay`（夹具 0 diff）、`bench`（单页/单题/批量/导出/UI）、`docs check`（链接与无代码块检查）、`doctor`（环境/模型/DB 自检）。
- 验收：命令可跑；基线数据写入 `docs/dev/performance.md`。
- 验证：`cargo run -p egctl -- doctor` 等。
- 依赖：T34、T38。文件：`crates/egctl/src/*`。
- 规模：M。

### T40 文档与 GitHub Pages
- 描述：`docs/user/`（快速开始/导入命名/双模式/复核/导出/故障/隐私）、`docs/dev/`（架构/模块/测试/性能/发布/研究记录）、ADR（两层判分/模式/规则整理/CAS/文件名/任务授权/无代码）、changelog；mdBook 构建并发布 GitHub Pages。
- 验收：链接 0 死链；用户手册覆盖所有页面；ADR 与规范一致；Pages 可访问。
- 验证：`egctl docs check`；`mdbook build`；Pages URL 检查。
- 依赖：T39。文件：`docs/**`、`.github/workflows/`。
- 规模：M。

### T41 端到端验收与缺陷修复
- 描述：按 `SPEC.md` S1–S13 实测：全流程、准确率、门禁、路由、性能、复核速度、复现、崩溃恢复、隐私、模式、规则结构化、日志文档；修复阻塞缺陷。
- 验收：S1–S13 有实测数据与结论；阻塞缺陷清零；已知限制写入文档。
- 验证：验收报告 + `cargo test --workspace` + 手动清单。
- 依赖：T40。文件：`docs/dev/acceptance.md`。
- 规模：M。
- **当前状态（2026-11-05，🟡 部分）**：S1 **外部夹具**端到端已完成——`egctl acceptance --fixtures fixtures/paper-20q --json` PASS（**287 断言 / 0 失败 / 9 负向 / 12 步**，约 1.8 s；`cargo test -p egctl` 全绿）；逐题 expected：张三 132000、李四 77000、王五 109000，Q9–11 部分给分、Q17 低置信、Q16 冲突、Q19 仅规则、Q20 manual、名单外未决。剩余限制：真实 OCR/judge 未参与（S2/S3）、UI 键鼠 E2E（S7）与中文 IME 人工验收待办、app 组合根尚未接 capture 多页归组接口。
- **待办清单**：⬜ S2 客观题确定性（500 条金标）· ⬜ S3 解答题准确率（200 条金标+校准）· ⬜ S5 路由准确率（300 题标注）· ⬜ S7 键盘复核 ≥15 题/分钟 + IME 人工 · ⬜ S8 判分回放（egctl replay TODO）· ⬜ app 组合根接入 capture 多页归组接口（capture 已交付）。

**检查点 CP-H**：S1–S13 报告完成；文档发布；可交付试用。

## 本轮追加工作（2026-11-04 ～ 2026-11-05）

| 项 | 状态 | 说明/证据 |
|---|---|---|
| `fixtures/paper-20q` 外部夹具 | ✅ | 20 题×3 学生 + 图片/OCR/judge/expected + 生成器；自检 1826 项 0 失败（`fixtures/generator/verify_fixtures.py`） |
| S1 端到端自动化（外部夹具） | ✅ | `egctl acceptance --fixtures fixtures/paper-20q --json` PASS 287 断言 / 0 失败 / 9 负向 / 12 步（~1.8 s）；`cargo test -p egctl` 全绿；`docs/dev/acceptance.md` §S1 |
| app 真实导入 | ✅ | capture→platform assets→persist 单事务；`fixtures/images` 9 文件→8 提交/8 页/资产 32/审计 17；`docs/dev/research/app.md` §8.8 |
| app 报表导出联动 | ✅ | 导出清 stale、复核修改置 stale 并提示重导出；10 文件/59 行 |
| app 图像证据懒加载 + 三栏 | ✅ | platform 资产路径→`slint::Image`；失败显式提示（app.md §8.7） |
| app 复核真实适配器 | ✅ | platform v3 队列/详情/命令/撤销/排除/导出（app.md §8.6） |
| `eg-inference` fixture-key 回放 | ✅ | `FixtureCatalog` 精确键/组合键/语义三级匹配 + sha256 完整性校验；`fixtures_wiring` 集成测试 |
| capture 多页归组（task-52 / tech-spike） | ✅ | 已交付：93 测试，3 学生×2 页 → 3 提交×2 页；外部夹具 S1 端到端已 PASS。app 组合根接入为剩余项（app 真实导入当前仍单文件=单页提交） |
| 真实模型（OCR/judge）接入验收 | ⬜ | 当前全部为 fixture 后端；S2/S3 需要真实金标与模型 |
| GitHub Pages 线上发布 | ✅ | [yang-spec-0202.github.io/exam-grading](https://yang-spec-0202.github.io/exam-grading/) 首页/研究页/acceptance 页均 200；workflow `pages.yml`（CI run 41497887203 / Docs run 41497887182） |

**多页归组（已交付）**：capture 多页归组已完成（93 测试，3 学生×2 页 → 3 提交×2 页），外部夹具 S1 端到端 PASS，CP-F 已转 ✅。剩余：app 组合根接入该接口后再复跑 `eg-app --smoke`，并更新 `app.md` §8.8 的导入明细。

## 检查点汇总（与 `tasks/plan.md` §6 对应）

- CP-A：T01–T03 后（构建/CI/文档骨架）
- CP-B：T04–T12 后（domain 契约 + platform 可靠性）
- CP-C：T13–T18 后（判分核心夹具 0 diff）
- CP-D：T19–T24 后（识别/推理/采集）
- CP-E：T25–T29 后（规则/全符号/对齐）
- CP-F：T30–T34 后（全量判分/复核/导出）——✅ 外部夹具 paper-20q 端到端 287 断言 / 0 失败 / 9 负向 / 12 步
- CP-G：T35–T38 后（UI）
- CP-H：T39–T41 后（验收/文档/发布）
