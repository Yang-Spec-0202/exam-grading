# 开源「Jev 类」判断模型调研报告

> 调研时间：2026-11-01（Asia/Shanghai）
> 对象：OpenAI 前研究员 Diogo Almeida 创办的 TypeSafe AI 于 2026-09-15 发布的 **Jev**（System One / 决策模型）之后，开源社区复刻、微调、蒸馏出来的**同类「判断模型」**
> 数据口径：JevBench / Benchmark Heaven 页面实时抓取（2026-11-01）；Hugging Face Decision Index 数据文件（生成于 2026-10-29 14:04 UTC）；GitHub / Hugging Face API 实测（2026-11-01）

---

## 0. 一句话结论

**Jev 本身是闭源托管 API；开源侧已经卷出 3000+ 个仓库、两个独立榜单上 240+ 个可排名系统。当前「最能打」的是 4B 级性价比路线（Torchcast Decision 4B v1.3、H2O-Lightning-4B）与 27–31B 能力路线（mammoth-1、KnowLine 31B、Fastino GLiDE 1.2、Turion Jev 31B）；「最火」的却是另一个模型——Laya（34.8k GitHub star / 5.9k HF likes），但它在两个榜单上都排到 200 名开外。**

**热度 ≠ 实力，这是这份调研里最重要的判断。**

---

## 1. Jev 是什么，为什么值得单独看

Jev 不是聊天模型。它把「判断」做成一个工程原语：

| 维度 | 通用 LLM | Jev 类判断模型 |
|---|---|---|
| 输出 | 逐 token 生成文本 | **一次前向**，直接返回「类型化答案 + 每个选项的概率」 |
| 原语 | 任意 | `choice`（从给定选项里选）/ `noul`（是/否）/ `score`（有序量表） |
| 延迟 | 数百 ms–数十秒 | 官方 70–500 ms；开源 4B 可到 30–70 ms |
| 计费 | 输入 + 输出 token | 只算输入 token，输出免费（一个选项一个 token） |
| 幻觉 | 可能编 | 答案被选项集封死 |
| 权重 | — | **闭源，只有 API**（2026-09 未向中国大陆开放） |

它解决的不是「模型会不会想」，而是「**已经知道怎么判的判据，能不能用 1/400 的成本、1/200 的时间执行**」。Agent 循环里的高频原子判断（工单路由、工具调用批准、内容过滤、RAG 重排、下一步动作选择）都是它的目标场景。

复刻门槛极低，这是生态爆炸的原因：把选项映射成 tokenizer 里的单个字母 token → 组织 prompt → 跑一次前向 → 读答案位置 logits → 在选项集上 softmax。几个小时能写出原型，几十美元能训一版 LoRA。

---

## 2. 当前格局：两个榜单，三个梯队

### 2.1 JevBench（Benchmark Heaven）——「工程性价比」榜

- 页面实时版本：**v1.6.1**（抓取于 2026-11-01），主榜 **241 个已排名系统**，语言补充表共 352 个系统。
- 计分：Intelligence / Calibration / Speed / Cost 四轴，等权调和平均；任一轴低于 50 会按 `(axis/50)^2` 额外惩罚 → **「最弱一轴说了算」**，所以小模型只要够快够便宜就能冲到前面。
- v1.6 每次评测 **1,500 道题（1,200 道密封 + 300 道公开）**，且含 23 道 7.7 万–8.1 万 token 的超长输入——**短上下文模型会直接拒答并按答错计分**。

**综合分（Composite）前十（2026-11-01）**

| # | 模型 | 综合 | 智能 I | 校准 C | 速度 S | 成本 K | 备注 |
|---:|---|---:|---:|---:|---:|---:|---|
| 1 | **Torchcast Decision 4B v1.3** | 80.49 | 67.1 | 87.8 | 92.4 | 79.4 | Apache-2.0，Qwen3.5-4B 全参微调 |
| 2 | **H2O-Lightning-4B v1.1** | 77.94 | 60.0 | 90.0 | 92.4 | 78.6 | Apache-2.0，Qwen3.5-4B |
| 3 | **Wald 26B-A4B** | 77.55 | 65.5 | 88.1 | 92.0 | 71.0 | Apache-2.0，Gemma-4-26B-A4B |
| 4 | Torchcast Decision 4B v1.2 | 76.97 | 60.3 | 85.0 | 90.9 | 79.4 | 上一版 |
| 5 | **Fastino GLiDE 1.2** | 75.90 | 76.4 | 93.5 | 85.5 | 58.1 | Qwen3.8-27B；Fastino 社区许可 |
| 6 | Bobcat Flash 1.2 | 75.79 | 65.6 | 80.5 | 90.6 | 71.1 | Apache-2.0，Gemma-4-26B-A4B |
| 7 | **H2O-Lightning-31B v1.1** | 75.73 | 79.3 | 89.4 | 89.5 | 56.0 | 许可为自定义（other） |
| 8 | **StartLux-Decision v1.1 27B Instant** | 75.63 | 73.2 | 91.9 | 90.3 | 57.9 | **CC-BY-NC-4.0，不可商用** |
| 9 | **mammoth-1（Baseten/Base Labs）** | 75.37 | 76.2 | 94.3 | 84.4 | 57.2 | **MIT**，Qwen3.5-27B |
| 10 | Decisio Gemma-4-12B tuned | 75.16 | 62.8 | 88.6 | 90.3 | 67.0 | Qwen/Gemma 系 |

参考行：Jev 1.13.0 综合 71.45、Jev 1.14.0 综合 70.15（榜上作 reference，不参与排名）。

**纯质量（Capability = 智能+校准）前十**

| # | 模型 | Capability | 成本 $/千次 |
|---:|---|---:|---:|
| 1 | mammoth-1 | 85.23 | 0.0268 |
| 2 | KnowLine 31B Gen1（PelaAI） | 85.21 | 0.0608 |
| 3 | Fastino GLiDE 1.2 | 84.92 | 0.0249 |
| 4 | H2O-Lightning-31B | 84.34 | 0.0292 |
| 5 | Perplexity Decider v1.1 27B（API） | 82.71 | 0.0264 |
| 6 | vLLM-SR d3-turbo（27B） | 82.59 | 0.0266 |
| 7 | StartLux-Decision 27B Instant | 82.56 | 0.0254 |
| 8 | vLLM-SR d3（27B） | 82.15 | 0.0266 |
| 9 | basal-omni-1.0-ultra | 82.13 | 0.0295 |
| 10 | dopest-1.0-31b | 82.03 | 0.0379 |

### 2.2 Hugging Face Decision Index 0.4.1——「纯决策能力」榜

- 数据文件 `data/index.json` 生成于 **2026-10-29 14:04 UTC**；**247 个模型**、42 个公开基准、110,201 次请求，单张 RTX PRO 6000 上跑，Jev 1.14 作参照。
- 分数 = `balanced_skill`（对 42 个基准做技能平衡），**不含速度/成本轴**，所以大模型占优。

| # | 模型 | 分数 | 基座 | 许可 |
|---:|---|---:|---|---|
| 1 | **KnowLine 31B Gen1**（PelaAI） | 71.86 | Gemma-4-31B | Apache-2.0 |
| 2 | **Fastino GLiDE 1.2** | 71.52 | Qwen3.8-27B | Fastino 社区许可（other） |
| 3 | **Sieve 31B v3** | 71.10 | Gemma-4-31B | Apache-2.0 |
| 4 | **Turion Jev 31B** | 70.47 | Gemma-4-31B LoRA | Apache-2.0 |
| 5 | vLLM-SR d3-ultra（27B） | 70.18 | Qwen3.8-27B | Apache-2.0 |
| 6 | Jeffan-31b-1710 | 69.75 | Gemma-4-31B | Apache-2.0 |
| 7 | H2O-Lightning-31B v1.1 | 68.64 | Gemma-4-31B | other |
| 8 | WeMM-JEV-27B | 68.49 | Qwen3.8-27B | Apache-2.0 |
| 9 | **mammoth-1** | 68.44 | Qwen3.5-27B | MIT |
| 10 | Ainvest-Decision-27B | 68.23 | Qwen3.8-27B | — |

参考：Jev 1.14 在该榜为 60.24；Jev 1.13 为 59.22（第 56 名）。

### 2.3 两个榜为什么打架（重要）

1. **JevBench 的 composite 含速度/成本**：4B 模型只要够快够便宜，就能压过 27B。Torchcast 4B 在 JevBench #1，但 Decision Index 的 247 个模型里根本没有它（该榜只测了 Torchcast 27B 版）。
2. **Decision Index 只测技能**，27–31B 全面占优；同一模型跨榜差异可以极大：Wald-26B-A4B 在 JevBench 综合 #3（77.55），在 Decision Index 只有 55.35（第 75 名）。
3. **JevBench v1.6 的超长上下文题**会惩罚 512–8k 上下文的模型；Laya 这类编码器模型因此接近 0 分，而它在自己的 2,000 条 typed-decisions 集上是 0.766。
4. **榜单仍在快速变动**：JevBench 页面自标 BETA，Torchcast 卡片明确写「anchor runs pending，分数可能被重新锚定」；同一模型卡片自报 76.16，而 11-01 页面为 80.49（成本口径/锚定不同）。

> 结论：**不要把任何一个单一分数当作采购依据**；至少做「一个性价比榜 + 一个能力榜 + 自己的 200 条盲测」三件套。

---

## 3. 值得关注的模型清单（按定位分）

### A. 性价比 / 小模型路线（4B 及以下，延迟 30–100 ms）

| 模型 | 规模/基座 | 许可 | 亮点 | 注意 |
|---|---|---|---|---|
| **Torchcast Decision 4B v1.3** | 4B / Qwen3.5-4B 全参微调 | Apache-2.0 | JevBench 综合 #1；A40 上 31.9 ms/次；自带 CUDA graph 服务端 | **必须用官方 server**：prompt 用了双向注意力，vLLM / llama.cpp / MLX 直接加载会得到不同概率 |
| **H2O-Lightning-4B v1.1** | 4B / Qwen3.5-4B | Apache-2.0 | 大厂出品，JevBench 综合 #2、ImageJevBench 开放权重第一；支持图片 | 31B 版许可是 other，需单独确认 |
| **Wald 4B / 26B-A4B** | Qwen3.5-4B / Gemma-4-26B-A4B | Apache-2.0 | 4B 有 4.3 万 HF 下载；26B 在 JevBench 综合 #3 | 26B 在 Decision Index 只排 75，跨榜差异大 |
| **bobcat-flash-1.2 / bobcat-1.3** | Gemma-4-26B-A4B / 31B | Apache-2.0 | 综合 #6；31B 版在 DI 第 31 | 4B 级别社区关注度低 |
| **vLLM-SR d3 家族**（d3-turbo / d3 / d3-mini / d3-nano / d3-edge） | 27B → 0.6B | Apache-2.0 | 一整个尺寸梯队，覆盖 27B 到边缘设备；DI 第 5 | 命名混乱，按尺寸选 |
| **chon-1.0-e4b** | 4B 级 | — | 成本轴 83.5（极便宜），综合 #14 | 智能轴一般 |

### B. 能力天花板路线（27–31B，适合离线批处理 / 高质量判断）

| 模型 | 规模/基座 | 许可 | 亮点 |
|---|---|---|---|
| **mammoth-1**（Baseten Base Labs） | 27B / Qwen3.5-27B | **MIT** | JevBench Capability #1（85.23）；支持图片；MIT 最干净；有 9B 兄弟 elephant-1 |
| **KnowLine 31B Gen1** | 31B / Gemma-4-31B | Apache-2.0 | Decision Index #1（71.86）；JevBench Capability #2 |
| **Fastino GLiDE 1.2** | 27B / Qwen3.8-27B | Fastino 社区许可 | DI #2（71.52）；校准 93.5；Fastino（GLiNER 团队）出品 |
| **Sieve 31B v3** | 31B / Gemma-4-31B | Apache-2.0 | DI #3（71.10） |
| **Turion Jev 31B** | 31B / Gemma-4-31B LoRA | Apache-2.0 | 分数第 4（官方并列 #2 分组，70.47）；校准误差 1.9 点，前 20 最低；新域私有题第 2 |
| **H2O-Lightning-31B** | 31B / Gemma-4-31B | other | JevBench 智能轴 79.3，Capability #4 |
| **StartLux-Decision 27B Instant** | 27B / Qwen3.8-27B | **CC-BY-NC-4.0** | 综合 #8、能力 #7；有 0.8B–27B 全家族 + GGUF/MLX | **禁止商用**，只能研究 |
| **basal-omni-1.0-ultra** | Gemma-4-31B | Apache-2.0 | 能力 #9；多模态 |

### C. 零训练 / 免微调路线

- **AnyJev（诺基亚应用研究院 + 腾讯混元）**：Apache-2.0，一个参数都不动。L0 用「选项循环移位 + 无标签先验校正」把顺序翻转率从 23% 降到 7.3%，ECE 0.240 → 0.095；L2 用闭式线性 head 提前退出（Qwen3-1.7B 只用 64% 深度达到 0.730）。适合「手头只有现成 LLM、先跑起来」。
- **纯 logits 读出框架**（Jobe、NInfer、local-jev 等）：不训练，直接读 logits；简单但位置/标签偏置严重，必须自己做校准。

### D. 超小 / CPU / 本地 / 中文

| 模型 | 规模 | 许可 | 说明 |
|---|---|---|---|
| **Laya**（Convai Innovations） | ModernBERT-large 421M（+322M 多语言） | Apache-2.0 | 社区最火（34.8k star / 5.9k HF likes / 6.1 万下载）；CPU/Apple Silicon 可跑，33 ms；**但基座 checkpoint 接近随机，必须用微调版**，且在 JevBench v1.6 / DI 上排名很差（长上下文与泛化短板） |
| **Intern-Decision**（上海 AI Lab / InternLM） | 0.8B / 2B / 4B | Apache-2.0 | 国产最强候选；Qwen3.5 微调，支持多模态；厂商自报 4B 七套件均分 90.02（未经第三方复现）；HF 93 likes / 5k 下载 |
| **Chinese Decision**（yanqiangmiffy） | 0.8B / 2B / 4B | 见仓库 | 中文专用，CJ-Dataset（1300 万条中文决策数据）+ CJ-Bench，arXiv 2610.23170；有 GGUF / ModelScope |
| **Phocinae-Largha-150M** | 150M / mmBERT-small | Apache-2.0 | 中英双语、CPU、选项顺序不变性，445 star 的新项目 |
| **GLiNER2.5-Decide**（Fastino） | 340M | 见模型卡 | GLiNER 团队，分类/抽取路线，可 CPU |

### E. 多模态

- **mammoth-1**（MIT）、**H2O-Lightning-4B/31B**、**Intern-Decision**、**Jev-Omni**、**StartLux 27B** 都支持「文本 + 图片 + 类型化问题 → 概率」。

---

## 4. 社区热度：谁在「夯」

按 GitHub star / HF likes（2026-11-01 实测）：

| 项目 | GitHub ★ | HF likes / 下载 | 性质 | 判断 |
|---|---:|---|---|---|
| **Laya** | 34,782 | 5,874 / 61,034 | 421M 编码器决策引擎 | 热度第一，但榜单表现与热度严重背离 |
| browser-use/jev-ultrafast | 24,003 | — | 浏览器 Agent（用 Jev，不是判断模型本身） | 应用层爆款，不算模型 |
| **jaredpalmer/kev** | 9,744 | — | 可自训的 Jev 类家族（0.6B–27B） | 星多，但 4B 版 JevBench 只有 6.78；27B 版 71.26（第 51） |
| Phocinae-Largha-150M | 445 | — | 150M 中英双语 | 新锐小模型 |
| **StartLux-Decision** | 1,631 | 4,825 下载 | 0.8B–27B 家族 | 有论文、有 GGUF/MLX；但 CC-BY-NC |
| **AnyJev** | 1,547 | — | 零训练方案 | 学术/工程价值高 |
| **jeff**（firelex） | 1,571 | — | 0.8B/2B 笔记本可跑 | 热度高、分数低 |
| **Mapika/decider** | 1,168 | 9,926 下载 | 0.8B–35B 家族 | 早期首个压过 Jev 的复现 |
| **von**（wfzyx） | 923 | — | 395M，sub-15ms，本地 | 超低延迟路线 |
| **ollaya** | 1,430 | — | Jev 兼容服务端（Ollama for decision models） | 基础设施 |
| autotrust/JEV-27B-VL | — | **2,906 likes / 278 万下载** | Qwen3.8-27B LoRA 蒸馏 | 下载量异常高（可能有 CI/镜像），DI 57.35 |

**一句话**：GitHub 热度最高的是「好上手、能本地跑、有故事」的项目（Laya、kev、jeff、AnyJev）；榜单最强的是「认真做数据 + 校准 + 服务端」的项目（Torchcast、H2O、mammoth、KnowLine、GLiDE、Turion）。两者几乎不重叠。

---

## 5. 选型建议

### 5.1 按场景

| 场景 | 首选 | 备选 |
|---|---|---|
| **在线高并发、成本敏感**（路由、审核、工具批准） | Torchcast 4B v1.3（Apache-2.0，31.9 ms） | H2O-Lightning-4B、Wald-4B、d3-mini |
| **质量优先、可离线批处理** | mammoth-1（MIT，Capability #1） | KnowLine 31B、GLiDE 1.2、Turion Jev 31B |
| **要最可信的概率（设阈值自动放行）** | mammoth-1（C 94.3）、GLiDE（C 93.5）、Turion（校准误差 1.9） | KnowLine、Sieve 31B |
| **完全不想训练** | AnyJev L0（零标签） | 直接 logits 读出 + 自行温度校准 |
| **纯本地 / 无 GPU / 气隙** | Laya 微调版（421M）、Phocinae 150M、GLiNER2.5-Decide 340M | d3-edge / d3-nano |
| **中文 / 国产** | Intern-Decision 4B（Apache-2.0） | Chinese Decision 4B、Phocinae（中英） |
| **多模态（图 + 文）** | mammoth-1、H2O-Lightning-4B | Intern-Decision、StartLux 27B（仅研究） |
| **多跳推理 / 数值 / 日期比较** | **不要用 Jev 类小模型**，直接上大模型 | — |

### 5.2 按「你信谁」

- 信 **JevBench**（含成本/速度、1,500 题、1,200 密封）→ Torchcast 4B / H2O-Lightning-4B / Wald。
- 信 **Decision Index**（42 基准、纯技能）→ KnowLine 31B / GLiDE 1.2 / Sieve 31B / Turion Jev 31B。
- 信 **社区口碑** → Laya / kev / AnyJev，但务必自己验。

---

## 6. 六个坑（选型前必读）

1. **榜单版本与锚定**：JevBench 页面自标 BETA，分数每天动；同一模型卡片自报 76.16、页面 80.49。引用必须带日期 + release 号。
2. **长上下文是一票否决**：v1.6 有 23 道 7.7 万–8.1 万 token 的题，短上下文模型拒答即算错。Laya（512/1024 token）因此接近 0 分。
3. **服务端不是通用的**：Torchcast、StartLux、mammoth 都用了「prompt 双向注意力 / block-bidirectional mask」，用 vLLM、llama.cpp、MLX 直接加载会得到**不同概率**，必须用官方 serving 包。
4. **许可证是三个独立事实**：代码许可、权重许可、基座许可。典型陷阱：
   - StartLux = **CC-BY-NC-4.0**（禁商用）；
   - GLiDE = Fastino 社区许可（非 OSI）；
   - Gemma 系（Turion / Winnow / basal / Quyet / Bobcat）= Apache-2.0 **但附 Google Prohibited Use Policy**；
   - H2O-Lightning-31B = other。
5. **「最火」可能是营销**：Laya 34.8k star 但 DI 第 234、JevBench 0.216；kev 9.7k star 但 4B 版 6.78。star 可以买、可以刷，榜单至少是第三方断网重跑。
6. **密封题的历史教训**：v1.4 时代所有 Jev 类模型（含原版）在密封题上从 ~86% 崩到 ~35%，而通用大模型仍有 90%+。v1.6 已把一半智能分放到密封题上，情况改善，但仍要**用你自己领域的新题做盲测**。

---

## 7. 落地验证清单（建议照做）

1. 用 200–500 条**自己业务**的真实决策做盲测（不是教师模型一致率）。
2. 测**概率校准**：ECE / Brier；决定你能不能设阈值。
3. 测**选项顺序轮换**：同一题把 A/B 对调，翻转率应 < 10%。
4. 测**长输入拒绝率**（你的 P95 输入长度）。
5. 测目标硬件的 **P95 延迟、显存、每千次成本**。
6. 先**影子运行**：并行打分、不执行动作，记录分歧，用已发生的业务结果核验，再逐步放量。

---

## 8. 结论

1. **生态已经饱和**：Jev 发布 6 周，3000+ 仓库、两个榜单 240+ 个可排名系统，训练成本被抹平到几十美元。
2. **开源在「成本-速度-校准」上已经追平甚至超过 Jev**：JevBench 综合前 10 **全部是开放权重**；Capability 前 10 也全部是开放权重。但**智能轴**上 Jev 仍在第一梯队（JevBench 1.13 I=63.6，高于多数 4B 复现，但已低于最强 Torchcast 4B 的 67.1）。
3. **真正的分水岭不是「开源 vs 闭源」，而是「4B 性价比路线 vs 27–31B 能力路线」**：前者 30–80 ms、$0.005–0.03/千次；后者质量高一个档，但延迟/成本高 3–10 倍。
4. **最稳的选型**：线上高频用 Torchcast 4B / H2O-Lightning-4B；离线高质量用 mammoth-1（MIT）或 Turion Jev 31B（Apache-2.0）；中文用 Intern-Decision；本地/CPU 用 Laya 微调版或 Phocinae 150M。
5. **不要相信任何一个「第一名」**——这个赛道每周都在换榜首，且榜单口径差异巨大。用你自己的数据做最终裁决。

---

## 附录：主要数据来源

| 来源 | 用途 | 链接 |
|---|---|---|
| JevBench / Benchmark Heaven | 241 系统综合/能力排名、成本、版本 | https://benchmarkheaven.com/jev-models |
| Hugging Face Decision Index（multimodalart） | 247 模型、42 基准、0.4.1 分数 | https://huggingface.co/spaces/multimodalart/jev-decision-index |
| JevBench GitHub | 评测 harness / 历史结果 | https://github.com/fstandhartinger/jevbench |
| TypeSafe 官方 | Jev 规格、版本 | https://docs.typesafe.ai |
| 各模型卡 | 许可、基座、自报分数 | 见正文模型名 |
| GitHub / HF API | star、likes、下载量（2026-11-01 实测） | — |
| 早期生态调研（2026-10-01） | v1.4 时代全量榜单与密封题坍塌数据 | https://github.com/freezetheflame/jev-ecosystem-research |

> 免责声明：榜单分数为第三方页面抓取值，可能因版本/口径/锚定变化；许可证信息以模型卡为准，商用前请自行复核。
