# 研究记录：识别（`recognize`）——页级/段级流水线、严格 Schema、LaTeX、缓存与置信度

> 模块与范围：`recognize`（T23 页级识别 + T24 段级/LaTeX/手写/置信度）· M3
> 记录日期：2026-11-02（Asia/Shanghai）· 实现环境：Apple M4 / 16 GB / macOS 27.0.1
> 关联：`SPEC-recognize.md` v0.2.1（§2 契约、§3 数据流、§8 验收）、`SPEC-inference.md` v0.2.3（§3 输出契约/结构校验归属、§4 生命周期）、`SPEC-capture.md`（白底合成）、`CONFIG.md` §4（`ocr.*`）、`SPEC-domain.md` §7/§8（`Recognition`/`Block`/`EngineStamp`/`ModelCall`）、`AGENTS.md`
> 交付物：`crates/eg-recognize/**` + 本记录。**未修改**其他 crate（`eg-inference`/`eg-capture` 由并行任务实现，接口以端口 + TODO 隔离）。

---

## 1. 模块与范围

本轮实现两条流水线（`SPEC-recognize.md` §3.1）：

| 能力 | 实现位置 | 关键约束 |
|---|---|---|
| 页级识别 | `src/page.rs`（`recognize_page`） | 取白底透视矫正图 → 渲染提示词 → 缓存 → 模型 → 结构/语义校验 → 阅读顺序 → 置信度 → 写 `ModelCall`/`Recognition` |
| 段级按需识别 | `src/segment.rs`（`segment_decision`/`recognize_segment`） | 触发条件：低置信块、裁片长边 <800 px、公式密集、超大块；失败不推翻页级结果 |
| 严格 Schema | `src/schema.rs` | 单对象、拒绝未知字段/未知枚举、1–200 块、text ≤4000、latex ≤2000、confidence 0–1 |
| 语义校验 | `src/semantic.rs` | 坐标越界、条码块、重复块、页尺寸声明；违规**不重试** |
| 机械修复 | `src/page.rs` + `src/prompt.rs` | 只修格式（语法/围栏/未知字段/枚举拼写），预算 `ocr.repair_attempts`（0–2） |
| LaTeX | `src/latex.rs` | 去定界符、Unicode→命令、全角→半角、幂等；平衡/环境/命令白名单/参数个数/禁止命令/长度/字符集 |
| 阅读顺序 | `src/reading_order.rs` | 10 带单调性 + 横向重叠；双栏空隙 >8% 时确定性重排 |
| 缓存 | `src/cache.rs` | 键 = 资产 + 后端/模型/版本 + 参数哈希 + 提示词哈希 + Schema 版本 + 作用域 + 段框哈希 |
| 置信度 | `src/confidence.rs` | 块级模型值/结构启发值（来源标注）、页级字符加权 × 结构因子、题级规则 |
| 手写降级 | `src/handwriting.rs` | 页级 <0.55 或手写块 <0.5 → 人工交接单；人工答案 `AnswerSource::ManualCorrection`、`blocks` 空 |
| 题号/分值合理性 | `src/plausibility.rs` | 行首题号提取、重复题号、分值标注计数；只作 `align`/界面信号，不单独判失败 |
| 测试替身/夹具 | `src/testing.rs` | 脚本后端、内存 IO、`eg-inference::FixtureBackend` 回放适配器 |

边界：本模块不直接 HTTP、不直接 SQL；`Recognition` 的 id/时间经 `platform` 的 `UlidFactory`/`Clock` 注入产生；每次后端调用（含失败与修复）写一条 `ModelCall`。

---

## 2. 问题清单（实现前需确认的外部事实）

| # | 问题 | 结论落点 |
|---|---|---|
| Q1 | 结构校验归 `inference`，其 OCR 输出契约当前是否已含 bbox/页尺寸/原始 JSON？ | 结论 4.1（**尚未**，临时由本模块承载严格解析） |
| Q2 | 输出枚举白名单与「条码块」的归属：结构拒绝还是语义拒绝？ | 结论 4.2 |
| Q3 | serde_json 错误如何稳定区分「可机械修复」与「不可修复」？ | 结论 4.3 |
| Q4 | 缓存值能否只存块+引擎？命中时如何确定性重算页级置信度？ | 结论 4.4 |
| Q5 | 置信度上限、结构因子、题级规则的具体取值与来源？ | 结论 4.5 |
| Q6 | LaTeX 校验做到什么程度？PaddleOCR 实际输出什么定界符？ | 结论 4.6 |
| Q7 | 阅读顺序「显著早于」「双栏 8%」的可判定实现？ | 结论 4.7 |
| Q8 | 手写阈值 0.55/0.5 是否在 `CONFIG.md`？可配置性由谁承担？ | 结论 4.8 |
| Q9 | 印章遮挡「大文本块」「交并比 >0.3」如何落地？ | 结论 4.9 |
| Q10 | 图像输入前置条件（白底、长边 ≤2000、1 MP 下采样）来自哪里？ | 结论 4.10 |
| Q11 | 成本/延迟/token 记录到哪里？失败调用是否也留证据？ | 结论 4.11 |
| Q12 | 与并行实现的 `inference`/`capture` 如何解耦？ | 结论 4.12 |

---

## 3. 来源（类型 / 日期 / 用途）

| 来源 | 类型 | 日期 | 用途 |
|---|---|---|---|
| `SPEC-recognize.md` v0.2.1 §2/§3/§8 | 规范（本仓库） | 2026-11-02 | 契约、错误映射、缓存键、置信度规则、验收 R1–R8 |
| `SPEC-inference.md` v0.2.3 §3/§4 | 规范（本仓库） | 2026-11-02 | OCR 角色严格输出契约、结构校验归属、修复归调用方 |
| `SPEC-capture.md` 第 46/69 行 | 规范（本仓库） | 2026-11-02 | 「透明通道以白底合成」→ 识别输入必须白底 |
| `SPEC-domain.md` §7/§8；`crates/eg-domain/src/{recognition,geometry,confidence,grade,subject,hash,ids,values}.rs` | 规范 + 源码（一手） | 2026-11-02 | `Block` 不变量（公式块必须带非空 LaTeX）、`BBox` 校验边界、`ManualReason` 取值、`EngineStamp` 字段、`RawAnswer` 形状 |
| `crates/eg-platform/src/{config,ids,clock,repo}.rs` | 源码（一手） | 2026-11-02 | `OcrConfig` 默认值与取值范围、`UlidFactory` 方法、`FixedClock`、`repo` 需要事务（故由 app 适配） |
| `crates/eg-inference/src/{fixture,schema,error,types}.rs` | 源码（一手） | 2026-11-02 | 夹具键算法/回放接口、当前 `OcrOutput`（无 bbox 的中间版本）、`TokenUsage`、错误分类 |
| `docs/dev/research/inference-serving.md` §4/§6（S11–S14） | 本仓库实测记录 | 2026-11-02 | PaddleOCR-VL-1.6 BF16 A4@300DPI 整页 **6.44 s**；mmproj `image_max_pixels=1003520`（≈1 MP 下采样）；热启动 0.81–2.45 s、首次读盘 10–11 s；MLX 43.70 s；**透明底整页只识别出大标题、合成白底后全文正确** |
| `docs/dev/research/models.md` §4/§6/§7 | 本仓库实测记录 | 2026-11-02 | 官方 OCR GGUF 为 **BF16**（935,769,056 B + mmproj 881,770,560 B）；Q4_K_M 为第三方（300,219,360 B，默认不启用）；PaddleOCR 的 LaTeX 用 `\(...\)` 定界符；Qwen3 判定需 `enable_thinking=false`（非 OCR 角色） |
| `docs/dev/research/pdf.md` §4-Q5/§5.1 | 本仓库实测记录 | 2026-11-02 | hayro 默认透明背景（94% 像素 alpha=0）→ 必须白底合成 |
| `CONFIG.md` §4 第 56–62 行 | 规范（本仓库） | 2026-11-02 | `ocr.*` 默认值：BF16 默认、`emit_latex` 开、段级「按需」、超时 60 s、修复 1 次、`confidence_floor=0.75`、缓存开 |
| `tasks/todo.md` T23/T24 | 任务清单 | 2026-11-02 | 交付边界（仅 `crates/eg-recognize/**` + 研究记录） |

> 说明：本轮**没有**新增联网检索的模型事实：OCR/量化/延迟结论全部复用仓库内 2026-11-02 的一手实测记录；本轮新增的一手验证是 Rust 侧 API 行为（编译/测试/计时，见 §5）。

---

## 4. 结论

### 4.1 结构解析临时归属（Q1）

`eg-inference` 的 OCR 输出当前是中间版本：`OcrOutput { blocks: Vec<OcrBlock{kind,text,confidence}> }`，**没有 bbox、页尺寸与原始 JSON 回传**，而 `SPEC-inference.md` §3 要求结构校验在 `inference` 执行。为不阻塞 T23/T24 且不改并行 crate：
- 本模块在 `src/schema.rs` 实现严格解析（`deny_unknown_fields`、数量/长度/数值上限、单对象、拒绝围栏），并标注迁移 TODO；
- `inference` OCR Schema 定稿后，把「解析 + 结构错误分类」整体迁到 `inference`，本模块只保留语义校验与业务映射。
未改规范、未改并行 crate；见 §7 D1。

### 4.2 枚举白名单与条码（Q2）

`SPEC-recognize.md` §2.3 的允许集合是 8 类（text/formula/table/handwriting/figure/stamp/header_footer/unknown）；条码是 `BlockKind` 保留变体但识别阶段永不产出。实现采用**两段式**：
- 结构层：`barcode` 是**已知枚举值**（可解析），其它未知值（拼写错误）→ 结构可修复错误；
- 语义层：`barcode` → `SemanticViolation::Barcode`（直接转人工，不重试）。
这样既满足 §3.2「结构拒绝未知枚举、修复可纠拼写」，也满足 §2.2「条码块属语义违规」。

标题/列表/图注不是允许取值：模型若输出 `title` 等 → 未知枚举 → 结构错误（可修复）；提示词已写明只有 8 个取值。

### 4.3 修复分类（Q3）

用 `serde_json::Error::classify()` + 错误文本：`Syntax`/`Eof` → 可修复（补引号、补括号）；`Data` 中 `unknown field`/`unknown variant` → 可修复（字段名/枚举拼写）；其余 `Data`（缺失字段、类型错误）与长度/数量/版本上限 → **不可修复**（修复不得新增/删除/改写识别内容）；零块 → `EmptyResult`；围栏 → 可修复。`attempt <= ocr.repair_attempts` 才发起下一次调用，单页调用上限 1+attempts（默认 2），语义违规**不重试**。修复提示词回填「原始输出 + 错误摘要」，并明确禁止改内容。

### 4.4 缓存（Q4）

键按 `SPEC-recognize.md` §3.6：`AssetId`（内容哈希）+ 后端保留字 + 模型 id/版本 + 参数哈希 + 提示词哈希 + Schema 版本 + 作用域 + 段框哈希（固定 4 位精度序列化后 SHA-256）。缓存值存**已通过语义校验的块记录（含块级置信度与来源标注）+ 引擎指纹 + 结构质量**：
- 结构质量是页级置信度的**派生输入**（首次/修复、顺序可疑、未知块、非法公式、遮挡、覆盖率），不是合成后的页级/题级置信度，因此不违反 §3.6「值不存合成置信度」；
- 命中时零模型调用、零 `ModelCall`，块/文本/引擎/置信度与首次逐字节一致（id 与 `created_at` 每次新生成，属页面记录标识而非识别内容）。
缓存开关取 `ocr.cache`；关闭时仍写识别结果。并发单飞（同键仅一次调用）需平台缓存表支持，见 §7 D6。

### 4.5 置信度（Q5）

取值与来源（`src/confidence.rs` 常量 + 单元测试逐条断言）：

| 规则 | 取值 |
|---|---|
| 上报上限 | 0.95（模型未在本地校准集校准前） |
| 块级：模型给概率 | 用模型值并裁剪到 0.95，来源标注「模型概率」 |
| 块级：无概率 | 首次通过 0.90 / 修复后通过 0.75；未知块取 min 0.30；非公式空文本取 min 0.20；不可用块 0 |
| 页级 | 字符数（公式按 LaTeX 长度）加权平均，权重裁剪 1–200，再乘结构因子，裁剪到 0–0.95 |
| 结构因子 | 修复后 0.90、含未知块 0.85、顺序可疑 0.90、印章遮挡 0.85（相乘） |
| 题级 | 做过段级识别取段级值；否则页级 ×0.9；跨页取涉及页/段最小值；跨 ≥3 页强制人工（`LowConfidence`） |

来源标注写入 `ModelCall.output.block_confidence_sources` 与 `block_statuses`，禁止把启发值伪装成模型值。

### 4.6 LaTeX（Q6）

PaddleOCR-VL 的公式输出带 `\(...\)` 定界符（`models.md` §6.4 实测），因此规范化必须先剥离 `$`/`$$`/`\(\)`/`\[\]`（可叠加），再做 Unicode 白名单映射（×/≤/π 等）、全角转半角、空白压缩；转换幂等（属性测试式断言覆盖典型输入）。校验为词法/结构级：长度 ≤2000、字符集（ASCII 数学标点 + `\text{}` 内 CJK）、花括号/方括号/圆括号平衡、环境 `\begin/\end` 配对且名称在白名单、命令白名单（未知命令即非法）、参数个数（`\frac` 2、`\sqrt` 1 可带 `[n]`、装饰命令 1 等）、禁止命令（`\input`/`\def`/`\catcode`/`\write`/`\href`/`\includegraphics` 等）、`\left/\right` 配对。非法 → 该块不可用（降级 `Unknown` + 置信度 0，见 D3），**不自动补括号、不猜测**。v1 不做渲染验证（规范范围外）。

### 4.7 阅读顺序（Q7）

按纵向 10 带：后块带号比前块早 ≥2 且横向重叠 >0.5（相对较窄块宽度）记一次违规；违规比例 >20% 记顺序可疑（结构因子 0.90）。双栏判定：投影块 x 区间并合并后，若存在宽度 ≥8% 页宽、且左右各有 ≥2 块的空白列，则判定双栏并按「列（左先）→ y → x」稳定重排；重排后不再记顺序可疑。页级识别**不拼接**跨页内容，只标记页首无题号前缀、页末触达页底（`bottom ≥ 0.94`）两个候选，合并归 `align`。

### 4.8 手写降级（Q8）

阈值来自规范 §3.4：识别级 <0.55 且存在手写块 → 人工；或手写块 <0.5 → 人工。`CONFIG.md` §4 无对应键，因此实现为模块内 `HandwritingPolicy`（可注入，默认即规范值），未擅自新增配置项。交接单只含页面/段/题（题 id 页级未知，留空由 `align` 回填）+ 原因 + 触发置信度；人工转录输出 `RawAnswer{source: ManualCorrection, blocks: []}`，不生成 `Recognition`/`EngineStamp`。R6（召回 ≥0.90）需要真模型 + 标注集，见 §6。

### 4.9 表格与印章（Q9）

表格块文本行内连续 ≥2 空格 → 制表符（保留单空格），逐行修剪；不还原合并单元格（规范明确不承诺）。印章遮挡：印章块与「面积 ≥2% 的文本/公式/手写/表格块」的交并比 >0.3 即记遮挡并乘 0.85。该「2%」是规范「大文本块」的可判定解释，记录为解释项。

### 4.10 图像输入前置（Q10）

规范与实测共同要求：透明背景必须白底合成（hayro 实测整页只识别出大标题）；`image_max_pixels=1003520`（≈1 MP）意味着模型侧会下采样，300 DPI A4（8.7 MP）实际只进约 1 MP，密集小字需 capture 侧分块。识别侧因此 fail-closed：
- `ImageBackground::Unverified` → 配置错误（不猜背景）；
- 长边 >2000 px → 配置错误（§3.8 要求适配器先降采样并记录）——避免「把 8.7 MP 直接送模型」的伪通过；
- 这些前置由 `capture`/`app` 适配器保证，见 §7 D5。

### 4.11 成本/延迟/token（Q11）

每次后端调用（成功、结构失败、语义失败、截断）都写 `ModelCall`：`latency_ms`/`cost_micros` 来自响应，token 用量、作用域、尝试序号、输入长边、块级来源/状态放入 `output`。`RecognizeMetrics` 汇总调用数、修复数、缓存命中、延迟/费用/token、提示词哈希与 Schema 版本，供 app 写审计日志。失败尝试的 `ModelCall.output` 是诊断形态（`status=invalid` + `raw_output`），见 §7 D4。

### 4.12 与并行实现的解耦（Q12）

`src/types.rs` 定义最小端口：`OcrBackend`（engine/infer/repair）、`ImageSource`、`RecognitionRepo`、`RecognitionCache`、`IdSource`（`eg_platform::UlidFactory` 已实现）+ 注入时钟/取消令牌。真实适配器（sidecar 调用、资产读取、SQLite 事务写入、platform 缓存表）由 `app` 组合根实现，本 crate 不感知。`src/testing.rs` 提供脚本后端与 `eg-inference` 夹具回放适配器，已跑通「夹具录制 → 回放 → 解析 → 识别入库」全链路。

---

## 5. 版本实测（本机）

| 项目 | 实测值 | 命令/证据 |
|---|---|---|
| 工具链 | rustc/cargo **1.99.1**、clippy **0.1.99**、edition 2024 | `rustc --version` / `cargo --version`（与 `docs/dev/research/toolchain.md` §5 一致） |
| 依赖（直接） | eg-domain/eg-platform/eg-inference 0.1.0（path）、serde 1.0.229、serde_json 1.0.154、thiserror 2.0.21；dev：tempfile 3.27.0 | `cargo tree -p eg-recognize --depth 1` |
| 单元测试 | **67 passed / 0 failed**（覆盖 schema/latex/prompt/reading_order/confidence/semantic/cache/testing/handwriting/segment/page/plausibility/error） | `cargo test -p eg-recognize` |
| 集成测试 | **23 passed / 0 failed**（页级、缓存命中/失效、修复 0/1/2 次、非法输出矩阵、LaTeX 开关、存储失败、取消、手写、跨页、题号/分值合理性、段级、夹具回放、配置失败、超大图 fail-closed） | `cargo test -p eg-recognize --test pipeline` |
| 静态检查 | `cargo clippy -p eg-recognize --all-targets -- -D warnings` 通过；`cargo fmt -p eg-recognize` 已执行 | 本机运行 |
| 校验开销 | 200 块（规范上限）结构+语义+顺序+结构质量 **1.08 ms**（debug 构建，`validation_budget_for_max_blocks_is_bounded`） | 测试内 `Instant` 计时，可复现 |
| 单页模型延迟（引用） | PaddleOCR-VL-1.6 **BF16**、A4@300 DPI、llama.cpp 0.8.0 build 11960：**6.44 s/页**（P50 目标 ≤6 s 刚好达标；P95 分布未测） | `docs/dev/research/inference-serving.md` §6（S12） |
| 内存/启动（引用） | 权重 1.8 GB 双文件；热启动 `/health ok` 0.81–2.45 s；首次读盘 10–11 s；加载后 RSS ≈2040 MB | 同上（S12 + `models.md` §6.1） |
| 公式输出形态（引用） | PaddleOCR 用 `\(...\)`；Q4 第三方亦同；Xiaomi-OCR-0 输出纯文本（不满足 `emit_latex=开`） | `docs/dev/research/models.md` §6.4 |

**未做**：真实模型端到端 smoke（适配器未接线，属 `app`/`inference` 并行任务）；R1–R8 中依赖金标集/真模型的指标（R1 通过率、R2 肯德尔一致性、R3 字符匹配、R4 AUROC/ECE、R6 召回、R7 P95 分布）留待 M3 集成验收，本模块已提供确定性规则与可注入夹具路径。

---

## 6. 风险与限制

1. **结构校验临时双写**：`inference` 定稿前，严格解析在本模块；迁移时若两份上限不一致会出现「一处通过一处失败」。缓解：上限常量集中在 `src/schema.rs`，迁移后改为调用 `inference` 的同一套常量。
2. **模型自评未校准**：0.95 上限只是防过度自信；R4 的 AUROC/ECE 需标注集校准（`SPEC-recognize.md` §8）。
3. **P95 与 1 MP 下采样**：BF16 整页 6.44 s 是单样本；300 DPI A4 只进约 1 MP，密集小字会漏识。识别侧对「长边 >2000」fail-closed，但真正的分块/缩放策略属 `capture`，需在 M3 集成时按页型实测（`inference-serving.md` §4-Q6）。
4. **LaTeX 白名单偏保守**：未知但合法的命令会被判非法 → 转人工（安全侧失败，不伪造）；若金标显示误杀率高，按 §7 D3 的流程扩充白名单（需记录来源）。
5. **双栏/顺序启发式**：10 带 + 8% 空隙是确定性阈值，对多栏杂志式版式可能误判；规范允许「顺序错误率 ≤8%」，需金标验证。
6. **手写路径不提供模型级召回**：本模块只保证「低置信/手写必转人工」，R6 的召回率取决于模型与阈值，需标注集。
7. **缓存并发**：同键单飞未实现（平台缓存表缺失）；当前内存实现仅用于测试/单进程。若并发页级识别，重复计费风险由 `app` 调度串行（规范 §3.6 要求本地并发上限 1）。
8. **审计未接线**：`RecognizeOutcome` 暴露全部计量字段，审计写入（操作者/时间/题目）由 `app` 经 `platform::audit` 完成；本模块不持有审计端口，避免与并行 platform 实现耦合。

---

## 7. 与规范的偏差（需负责人确认/后续修订）

| # | 偏差/解释 | 原因 | 建议处置 |
|---|---|---|---|
| D1 | 结构校验（严格解析 + 错误分类）暂时实现在 `recognize`，而非 `inference` | `inference` 的 OCR 契约仍是中间版本（无 bbox/页尺寸/原始 JSON），并行实现中不可改 | `inference` OCR Schema 定稿后迁移；规范 §3.2 的归属不变，仅实现位置临时变化 |
| D2 | 缓存值额外保存「结构质量」 | 页级置信度 = 块级加权 × 结构因子；不存质量就无法在命中时确定性重算，只能把合成值也存进缓存（更违反 §3.6） | 规范 §3.6「值只存块与引擎」可补一句：允许存**合成所需的派生结构输入**，但不得存合成后的页级/题级置信度 |
| D3 | 公式非法或 `emit_latex=关` 时，块降级为 `BlockKind::Unknown`（保留 text、latex 空、置信度 0），而不是「`Block.latex` 为空但 kind 仍为 Formula」 | 领域不变量要求「公式块必须携带非空 LaTeX」（`eg_domain::Block::validate`），两者冲突 | 需在 `SPEC-domain.md` §7 与 `SPEC-recognize.md` §3.3 之间定一个：建议规范明确「不可用公式块以 Unknown + 状态标注承载」并新增块状态字段/模块类型；本轮用 `BlockStatus`（模块自有）实现 |
| D4 | 失败尝试的 `ModelCall.output` 是诊断对象（`status=invalid` + `raw_output`），不是「已通过 Schema 校验的结构化结果」 | 规范 §3.8 要求成本/token 留证据；失败调用也必须记费与原始输出 | 规范可补充：失败调用允许以诊断形态写入 `ModelCall.output`，并加 `failure_stage` 标注 |
| D5 | 段级按需条件「题型为计算/解答/证明」用「公式密集」近似 | `align`/`grade` 的题型信息尚不可注入（并行实现） | 待题型接口定稿后在 `SegmentContext` 增加 `question_kind`，保留现近似作为兜底 |
| D6 | 缓存并发「同键单飞」未实现 | `platform` 尚无识别缓存表 | 表结构定稿后由 `app` 适配器实现（Redis 式单飞或本地互斥） |
| D7 | `SPEC-recognize.md` §3.8 仍写「PaddleOCR-VL-1.6（0.9B Q4）」 | 该文件 v0.2.1 早于 T22 的量化修订；`CONFIG.md` §4 与 `models.md` 已改为官方 BF16 默认、Q4 第三方可选 | 建议把 `SPEC-recognize.md` 同步到 v0.2.2（任务外，需审批）；本轮实现按 `CONFIG.md`/模型注册表口径，不写死量化版本 |
| D8 | 手写阈值不在 `CONFIG.md`，实现为模块内可注入策略 | 规范 §3.4 给了默认 0.55/0.5 但未列入配置表 | 若产品需要教师可调，再按 Ask first 增 `ocr.handwriting_*` 配置项与范围 |
| D9 | 图像长边 >2000 直接配置错误 | 规范 §3.8 要求「超出先降采样并记录」；降采样属图像处理（capture），识别侧无图像库、也不应静默缩放 | 适配器负责降采样并记录；若希望容忍，可在 `ImageInput` 增加 `downscaled_from` 元数据（后续） |

---

## 8. 验收对照（`SPEC-recognize.md` §8）

| 条目 | 本轮覆盖 | 证据 |
|---|---|---|
| R1 结构健壮性 | 结构拒绝/修复/上限逻辑有测试；实际通过率需 50 页金标 + fixture 版本 | `schema::tests::*`（8 个）、`pipeline::malformed_outputs_are_rejected_or_degraded`、修复 0/1/2 次测试 |
| R2 结构正确 | 顺序校验/双栏重排/坐标越界 0/块数一致（不丢块）；题号/分值合理性作为 `align` 信号 | `reading_order::tests::*`、`semantic::rejects_coord_out_of_range_and_non_finite`、`plausibility::tests::*` |
| R3 公式 | 规范化 + 20 个非法构造拦截（本记录 §4.6 用例表）；字符匹配需人工金标 | `latex::tests::rejects_unbalanced_and_illegal`（17 例）、`invalid_formula_degrades...` |
| R4 置信度 | 规则表逐条断言；AUROC/ECE 需标注集 | `confidence::tests::*`（6 个） |
| R5 缓存 | 命中 0 调用 + 块级逐字节一致 + 提示词哈希变化必不命中 | `pipeline::cache_hit_calls_no_model_and_is_byte_identical`、`prompt_change_invalidates_cache` |
| R6 手写兜底 | 低置信/手写块必转人工；人工答案形状正确；召回 ≥0.90 需真模型 | `handwriting::tests::*`、`pipeline::handwriting_low_confidence_routes_to_manual` |
| R7 预算 | 调用上限 1+attempts 有测试；P50 引用 6.44 s；P95 需集成实测 | `repair_budget_zero/ exhausted`、§5 表 |
| R8 不编造/条码 | 非法公式 latex 空且置信度 0；猜测补全路径不存在；条码块语义拒绝 | `invalid_formula_degrades_block_without_fabrication`、`semantic::rejects_barcode_and_duplicates` |
