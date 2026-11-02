# 开源 OCR / 文档解析模型调研报告

> 调研时间：2026-11-01（Asia/Shanghai）
> 需求：开源、能识别**图像文字 + 表格 + 数学公式**的 OCR / 文档解析模型，回答「谁最夯」
> 数据口径：OmniDocBench v1.6 官方 README 榜单（opendatalab）；olmOCR-Bench；各模型卡与 GitHub/Hugging Face API 实测（2026-11-01）

---

## 0. 一句话结论

**「最夯」要分三个维度回答，而且答案不重叠：**

| 维度 | 答案 |
|---|---|
| **榜单最准**（图像/表格/公式综合） | **OXR-1.0**（1.3B，OmniDocBench v1.6 **97.03**、olmOCR-Bench **87.84**）——但许可证有商业门槛 |
| **社区最火 / 生态最大** | **百度 Unlimited-OCR**（GitHub **30,190★**，MIT，HF 4,935 likes / **412 万下载**；10-27 刚发 v2「无限长文档」）；其次 PaddleOCR（**92,055★**）、MinerU（**82,916★**）、GLM-OCR（2,194 likes / 177 万下载） |
| **最稳的可商用选择** | **PaddleOCR-VL-1.6**（Apache-2.0，96.34）、**TeleOCR**（Apache-2.0，96.91）、**Xiaomi-OCR-0**（0.8B，Apache-2.0，96.83）、**OvisOCR3**（0.8B，Apache-2.0，96.74）、**GLM-OCR**（MIT，95.22） |

**如果只选一个：中文为主、要商用、要表格+公式 → PaddleOCR-VL-1.6；追求极限精度且能接受许可证约束 → OXR-1.0；要最热生态+MIT → Unlimited-OCR。**

---

## 1. 先对齐概念：OCR 已经变成「文档解析」

2026 年的开源 OCR 主流已经不是「检测框 + 识别文字」，而是**端到端文档解析模型**：输入一页图像/PDF 页，一次前向输出

- Markdown / JSON（带版面坐标、阅读顺序）
- 表格 → **HTML**（保留合并单元格）或 Markdown
- 公式 → **LaTeX**
- 图表 → Markdown/表格；印章、手写、竖排、化学式等

所以「能识别图像、表格、数学公式」这个需求，对应的就是**OmniDocBench 这类文档解析基准**，三个核心指标正好一一对应：

| 需求 | 指标 | 说明 |
|---|---|---|
| 图像文字 | **Text Edit Distance ↓** | 文本编辑距离，越低越好 |
| 数学公式 | **Formula CDM ↑** | LaTeX 公式的字符级匹配 |
| 表格 | **Table TEDS ↑ / TEDS-S ↑** | 表格结构相似度；TEDS-S 是结构-only，更严 |
| 版面 | **Reading Order Edit ↓** | 多栏/报纸/试卷的阅读顺序错误率 |

另一个独立榜 **olmOCR-Bench**（AllenAI，1,403 页 / 7,010 条规则）更偏「真实难例」：arXiv 公式、老扫描件、长小字、多栏，常用来交叉验证。

---

## 2. 当前榜单：OmniDocBench v1.6（截至 2026-11-01）

| # | 模型 | 参数 | Overall↑ | Text Edit↓ | Formula CDM↑ | Table TEDS↑ | TEDS-S↑ | Order Edit↓ | 许可 |
|---:|---|---:|---:|---:|---:|---:|---:|---:|---|
| 1 | **OXR-1.0**（SoMark） | 1.3B | **97.03** | 0.0244 | 96.93 | 96.61 | 98.44 | 0.1216 | 自定义（Apache+附加条款） |
| 2 | **Youtu-Parsing-Omni**（腾讯） | 5B | 96.96 | 0.0271 | 96.80 | 96.79 | 98.20 | **0.1104** | 自定义（**禁欧盟**） |
| 3 | **TeleOCR**（XingChen-AGI） | 1.2B | 96.91 | 0.0267 | 96.59 | **96.82** | 98.18 | 0.1184 | **Apache-2.0** |
| 4 | **Xiaomi-OCR-0**（小米） | 0.8B | 96.83 | 0.0313 | **98.50** | 95.11 | 97.19 | 0.1221 | **Apache-2.0** |
| 5 | **OvisOCR3**（ATH-MaaS） | 0.8B | 96.74 | — | — | — | — | — | **Apache-2.0** |
| 6 | **OvisOCR2** | 0.8B | 96.58/96.47 | 0.0265 | 97.49 | 94.58 | 96.98 | 0.1120 | **Apache-2.0** |
| 7 | **MiniCPM-OCR 1.0**（面壁） | 1.3B | 96.51 | — | — | — | — | — | **Apache-2.0** |
| 8 | **PaddleOCR-VL-1.6**（百度） | 0.9B | 96.34 | 0.0326 | 97.53 | 94.76 | 97.10 | 0.1278 | **Apache-2.0** |
| 9 | **MinerU2.5-Pro**（上海 AI Lab） | 1.2B | 95.75 | 0.036 | 97.45 | 93.42 | 95.92 | 0.1200 | Apache-2.0+附加条款 |
| 10 | **GLM-OCR**（智谱） | 0.9B | 95.22 | 0.044 | 97.18 | 92.83 | 95.39 | 0.1330 | **MIT** |
| 11 | HunyuanOCR-1.5（腾讯） | 1B | 94.74 | 0.039 | 94.50 | 93.67 | 94.71 | 0.1290 | 腾讯许可 |
| 12 | Unlimited-OCR（百度，v1） | 3B | 94.00 | 0.0394 | 95.72 | 90.21 | 93.36 | 0.1281 | **MIT** |
| 13 | Qianfan-OCR（百度） | 4B | 93.90 | 0.040 | 95.08 | 90.53 | 93.31 | 0.1300 | 见模型卡 |
| 14 | FireRed-OCR（小红书） | 2B | 93.26 | 0.037 | 95.44 | 88.04 | 91.06 | 0.1310 | **Apache-2.0** |
| 15 | dots.ocr（小红书） | 3B | 90.77 | 0.048 | 89.95 | 87.18 | 90.58 | 0.1380 | **MIT** |
| 16 | DeepSeek-OCR 2 | 3B | 90.25 | 0.050 | 91.84 | 83.89 | 87.75 | 0.1440 | 见模型卡 |
| 17 | HunyuanOCR（v1） | 1B | 89.95 | 0.088 | 87.68 | 91.01 | 93.23 | 0.1710 | 腾讯许可 |

> 参考：Gemini 3 Pro 92.91、Gemini 3 Flash 92.62、Qwen3-VL-235B 89.78、GPT-5.2 86.59。**开源专用模型已经全面超过同期通用大模型**，这是 2026 年 OCR 赛道最大的变化。
> OXR 的 97.03 来自 OmniDocBench 官方 README；其自报 97.18（自测口径）。Youtu 96.96、OvisOCR2 96.58 为模型卡自报，官方表为 96.47，差异在噪声范围内。

**分项冠军**

- 文本：OXR（0.0244）
- 公式：Xiaomi-OCR-0（CDM 98.50）> PaddleOCR-VL-1.6（97.53）> OvisOCR2（97.49）> MinerU2.5-Pro（97.45）> GLM-OCR（97.18）
- 表格：TeleOCR（96.82）≈ Youtu（96.79）> OXR（96.61）> HunyuanOCR（93.67 TEDS-S 94.71）
- 阅读顺序：Youtu（0.1104）> OvisOCR2（0.1120）> TeleOCR（0.1184）

---

## 3. 第二个榜单：olmOCR-Bench（更偏真实难例）

| # | 模型 | Overall | 备注 |
|---:|---|---:|---|
| 1 | OXR-1.0 | **87.84** | 表格 93.05，arXiv 数学 92.76 |
| 2 | Infinity-Parser2-Pro（Infly） | 87.60 | Apache-2.0 |
| 3 | Chandra OCR 2（Datalab） | 85.80 | OpenRAIL-M；1.4M 下载 |
| 4 | dots.mocr（小红书） | 83.90 | 192 likes / 19 万下载 |
| 5 | Surya-OCR-2.5（Datalab） | 83.7 | OpenRAIL-M |
| 6 | LightOnOCR-2-1B | 83.2–83.4 | Apache-2.0 |
| 7 | olmOCR 2（AllenAI） | 82.4 | Apache-2.0 |
| 8 | MiniCPM-OCR 1.0 | 82.7 | Apache-2.0 |
| 9 | Xiaomi-OCR-0 | 82.1 | Apache-2.0 |
| 10 | PaddleOCR-VL-1.6 | 80.2 | Apache-2.0 |
| 11 | dots.ocr | 79.1 | MIT |
| 12 | GLM-OCR | 75.2 | MIT；在 OmniDocBench 强、olmOCR 弱 |

**为什么两个榜打架**：OmniDocBench 是「PDF 页 → Markdown」，公式/表格占比高、版面标准；olmOCR-Bench 塞了大量 arXiv 老扫描、长小字、多栏和表结构测试，更接近「脏数据」。**GLM-OCR 在 OmniDocBench 95.22、olmOCR 仅 75.2** 就是典型：它擅长规整文档，不擅长老扫描/复杂公式页。

---

## 4. 热度：谁真的在「夯」

### 4.1 GitHub（2026-11-01 实测）

| 项目 | ★ | 性质 | 说明 |
|---|---:|---|---|
| **PaddleOCR** | **92,055** | 工具链 + PaddleOCR-VL | 生态最大，文档最全，工业部署首选 |
| **MinerU** | **82,916** | PDF→Markdown 流水线 | 上海 AI Lab；RAG/Agent 数据清洗常用 |
| **docling**（IBM） | 69,941 | 文档转换框架 | MIT；结构解析+多格式 |
| **marker**（Datalab） | 40,736 | PDF→MD 流水线 | Apache-2.0；轻量 |
| **baidu/Unlimited-OCR** | **30,190** | OCR 模型 | MIT；2026-06 发布，10-27 发 v2 |
| DeepSeek-OCR | 24,106 | OCR 模型 | 2025-10，仍大量使用 |
| olmOCR | 19,813 | OCR 工具链 | AllenAI |
| Chandra | 12,656 | OCR 模型 | Datalab |
| **GLM-OCR** | 7,565 | OCR 模型 | 智谱；MIT |
| OXR | 2,792 | 文档解析 | 开源一周破千星（10 月） |
| MiniCPM-OCR | 729 | OCR 模型 | 2026-10-21 发布 |
| Youtu-Parsing | 383 | 文档解析 | 腾讯 |

### 4.2 Hugging Face（likes / 下载）

| 模型 | likes | 下载 | 说明 |
|---|---:|---:|---|
| **baidu/Unlimited-OCR** | **4,935** | **4,122,291** | 当前最热 OCR 模型 |
| deepseek-ai/DeepSeek-OCR | 3,461 | 2,055,501 | 2025 老将 |
| **zai-org/GLM-OCR** | **2,194** | **1,766,350** | 0.9B，MIT，1.86 页/秒 |
| Nanonets-OCR-s | 1,605 | 10,386 | 老模型 |
| GOT-OCR2 | 1,577 | 172,815 | 2024 老模型 |
| dots.ocr | 1,347 | 447,614 | 小红书，2025-07 |
| Qianfan-OCR | 1,216 | 35,106 | 百度 4B |
| DeepSeek-OCR-2 | 1,150 | 419,321 | 2026-01 |
| **TeleOCR** | **1,315** | 29,415 | 2026-08，Apache-2.0 |
| LightOnOCR-2-1B | 850 | 755,070 | Apache-2.0 |
| HunyuanOCR | 843 | 724,991 | 腾讯 |
| PaddleOCR-VL-1.6 | 565 | 85,301（+GGUF 98k） | Apache-2.0 |
| Chandra OCR 2 | 593 | 1,413,672 | OpenRAIL-M |
| **baidu/Unlimited-OCR-2** | 488 | 109,519 | 10-27 发布，4 天 |
| OXR-1.0 | 139 | 3,487 | 10 月新模型 |
| OvisOCR3 | 105 | 4,700 | 10-21，0.8B |
| MiniCPM-OCR 1.0 | 381 | 3,513（+GGUF 23.7k） | 10-21 |

**读法**：下载量受镜像/CI/GGUF 分流影响，likes 更接近「真人关注」；GitHub star 反映工程生态，不等于精度。**热度最高的 Unlimited-OCR，在 OmniDocBench 只有 94.00（v1），并不是最准的**——这和上一期 Jev 调研的结论一样：**热度 ≠ 实力**。

---

## 5. 第一梯队逐个看

### 🥇 OXR-1.0（SoMarkAI）——当前综合最强
- 1.3B；OmniDocBench **97.03**（官方）/ 97.18（自测）；olmOCR-Bench 87.84；自建 DocParsingBench 87.76。
- 13 类元素：文本、标题、表格（HTML/MD）、公式（LaTeX）、化学式（mhchem）、化学结构（SMILES）、代码块、印章、页眉页脚、图注、脚注……带 bbox 与阅读顺序。
- 速度：RTX PRO 6000 上 557 tokens/s、0.99 页/s；100 页约 101 秒（PaddleOCR-VL-1.6 约 145 秒、MinerU2.5-Pro 约 255 秒）。
- 架构：轻量版面模型 + 主模型（阅读顺序并入识别阶段），版面分析可插拔。
- **许可（关键）**：Apache-2.0 + 附加条款——**融资/年营收超过 200 万美元需单独商业许可**；**不得做与 SoMark 竞争的托管 API**；在线服务需署名。开源一周 2,792★。

### 🥈 Youtu-Parsing-Omni（腾讯）——最强多模态文档解析
- 5B；OmniDocBench 96.96，表格 96.79，阅读顺序 0.1104（最好）；支持文档、图片、图表、几何图形，甚至音频/视频转结构化 JSON。
- **许可**：自定义 Youtu-Parsing License，**明确「不适用于欧盟境内使用」**——出海/欧盟业务直接排除。

### 🥉 TeleOCR（XingChen-AGI）——Apache-2.0 里的表格冠军
- 1.2B；96.91；**Table TEDS 96.82 全场第一**；Apache-2.0；HF 1,315 likes。
- 主打「内容-结构解耦学习」处理表格与公式；Dr.DocBench 挑战赛表现优于 MinerU 2.5 Pro / PaddleOCR-VL 1.6。

### Xiaomi-OCR-0（小米）——0.8B 的性价比之王
- 0.8B（Qwen3.5-0.8B Base）；96.83；**Formula CDM 98.50 全场第一**；UniMER-Test 97.84、PubTabNet 92.27；Real5-OmniDocBench 95.24 #1。
- Apache-2.0，17k 下载；适合显存紧、公式多的场景。

### OvisOCR3 / OvisOCR2（ATH-MaaS，阿里 Ovis 系）
- 0.8B，Apache-2.0；OvisOCR3：OmniDocBench 96.74、**MDPBench 88.80、PureDocBench 82.31、Real5 93.63**——多任务最均衡（解析、通用 OCR、KIE、图注、跨页表格重建）。
- OvisOCR2：96.58，公式 97.49，首个「端到端模型登顶 OmniDocBench」的选手（后被 OXR 超过）。

### MiniCPM-OCR 1.0（面壁智能）
- 1.3B，Apache-2.0，2026-10-21 发布；OmniDocBench 96.51、Real5 93.68、olmOCR 82.7；中英双语，强在扫描/弯曲/翻拍/倾斜等「真实拍摄」场景；1.2 页/s。
- 另有 **MiniCPM-OCR-Jev**：给 OCR 结果做「正确性判断」的 2B 模型（AUROC 93.6），适合做质量门禁。

### PaddleOCR-VL-1.6（百度）——生产环境最稳
- 0.9B，Apache-2.0，2026-05-28 发布；96.34；公式 97.53（第二）、表格 94.76；支持表格/公式/图表/**印章**/古籍/生僻字。
- 背靠 PaddleOCR 92k★ 生态：训练、部署、量化、移动端、中文文档最全；1.5→1.6 架构兼容，可零成本迁移。

### GLM-OCR（智谱）——最流行的 0.9B MIT 模型
- 0.9B，**MIT**；OmniDocBench v1.5 94.62（发布时 #1）、v1.6 95.22；1.86 页/s；HF 2,194 likes / 177 万下载，GitHub 7.5k★。
- 适合高并发、边缘部署；短板：老扫描/复杂公式页（olmOCR 75.2）。

### Unlimited-OCR / Unlimited-OCR 2（百度）——最热 + 超长文档
- v1：3B，MIT，30,190★，4,935 likes / 412 万下载；OmniDocBench 94.00。
- v2（2026-10-27）：**SR-SWA 稀疏引用滑窗注意力**，把任意长文档压到固定 512 token 工作集，实现「无限长文档」解析；同时发布 **MarathonDocBench**（百页级超长文档基准）。适合整本书/超长 PDF 一次过，不适合追求单页最高精度。

### MinerU 2.5-Pro（上海 AI Lab）
- 1.2B，95.75；强项是**整条 PDF→Markdown 流水线**（版面、公式、表格、图片、OCR），82,916★；有 `MinerU Open Source License`：Apache-2.0 + 商业门槛（MAU > 1 亿或月收入 > 2000 万美元需商业许可）+ 在线服务署名。

### 其他值得知道
- **Infinity-Parser2-Pro**（Infly）：olmOCR 87.60（第二），Apache-2.0，中英多语。
- **Chandra OCR 2**（Datalab）：olmOCR 85.8，OpenRAIL-M，1.4M 下载；强在表格/表单/手写/多语（90+ 语言）。
- **Surya-OCR-2.5**：olmOCR 83.7，OpenRAIL-M；表格 HTML + 阅读顺序。
- **dots.ocr**（小红书）：MIT，1,347 likes；2025 老将，90.77，仍适合轻量中文文档。
- **FireRed-OCR**（小红书）：2B，Apache-2.0，93.26；Format-Constrained GRPO 减少「表格不闭合/公式语法错」。
- **Indra OCR**（Datalab）：**1M 参数、32 核 CPU 上约 44 页/秒**，OpenRAIL-M；极端边缘场景。

---

## 6. 怎么选：按场景

| 场景 | 首选 | 备选 |
|---|---|---|
| **中文为主、商用、要表格+公式+印章** | **PaddleOCR-VL-1.6**（Apache-2.0） | GLM-OCR（MIT）、MinerU2.5-Pro |
| **追求单页最高精度，可接受许可约束** | **OXR-1.0** | Youtu-Parsing-Omni（非欧盟） |
| **Apache-2.0 + 最高表格精度** | **TeleOCR** | OvisOCR3 |
| **0.8B 小模型、公式多** | **Xiaomi-OCR-0** | OvisOCR2 / OvisOCR3 |
| **多任务（解析+OCR+KIE+图注+跨页表）** | **OvisOCR3** | MiniCPM-OCR |
| **超长文档/整本书/百页 PDF** | **Unlimited-OCR-2**（MIT） | MinerU 流水线 |
| **高并发、低成本、边缘** | **GLM-OCR**（0.9B，1.86 页/s） | PaddleOCR-VL-1.6、Indra OCR（CPU） |
| **整条 PDF→Markdown 流水线** | **MinerU** | docling、marker |
| **表格结构（合并单元格/跨页）** | **TeleOCR** | Youtu、OXR、Chandra 2 |
| **数学公式（LaTeX）** | **Xiaomi-OCR-0**（CDM 98.5） | PaddleOCR-VL-1.6、OvisOCR2 |
| **自然场景文字/截图/票据** | **Unlimited-OCR**（MIT） | PaddleOCR-VL、GLM-OCR |
| **纯 CPU / 气隙** | **Indra OCR**（1M） | MinerU/PaddleOCR 移动端 |

---

## 7. 许可证陷阱（务必先看）

| 模型 | 许可 | 关键限制 |
|---|---|---|
| OXR | Apache-2.0 + 附加条款 | 融资/年营收 > **$2M** 需商业许可；**禁止竞争性托管 API**；在线服务需署名 |
| Youtu-Parsing-Omni | 腾讯自定义 | **不得在欧盟使用** |
| MinerU | Apache-2.0 + 附加条款 | MAU > **1 亿**或月收入 > **$2000 万**需商业许可；在线服务需署名 |
| Chandra / Surya / Indra | OpenRAIL-M（修改版） | 研究/个人/小额初创免费（Datalab 口径：融资或营收 < $5M 或 $20M）；**不得与其 API 竞争**；超过需商业授权 |
| PaddleOCR-VL | Apache-2.0 | 宽松 |
| GLM-OCR | MIT（模型卡）/ Apache-2.0（仓库） | 宽松 |
| Unlimited-OCR | MIT | 宽松 |
| TeleOCR / Xiaomi-OCR-0 / OvisOCR2/3 / MiniCPM-OCR / Infinity-Parser2-Pro / FireRed-OCR | Apache-2.0 | 宽松 |
| dots.ocr | MIT | 宽松 |

**结论**：真正「无条件商用」的是 Apache-2.0/MIT 那一批；如果法务严格，**优先 PaddleOCR-VL-1.6、GLM-OCR、Unlimited-OCR、TeleOCR、Xiaomi-OCR-0、OvisOCR3**。

---

## 8. 部署与效果上的坑

1. **公式输出格式不统一**：LaTeX（`$...$`/`\[...\]`）、OTSL、HTML、mhchem（化学）——下游入库前必须统一解析/渲染，否则「看起来对、程序读不了」。
2. **表格 TEDS ≠ 可用**：TEDS 对合并单元格/跨页表不敏感，要看 TEDS-S 和你自己的「单元格级」抽检。
3. **扫描件/手写/竖排/印章**：OmniDocBench 主要是数字 PDF；真实票据/档案要另测，MiniCPM-OCR、Chandra 2、Unlimited-OCR 在这方面更强。
4. **长文档**：多数模型按页处理；跨页表格、页眉页脚去重、阅读顺序要流水线补。Unlimited-OCR-2 和 MinerU 是整文档方案。
5. **推理框架**：部分模型（OXR、Youtu、Ovis、MiniCPM）需要 `trust_remote_code` 或专用 serving（vLLM/SGLang 版本敏感）；量化后精度可能掉，尤其公式。
6. **版本极快**：OXR 一周 2.8k★，Unlimited-OCR-2 四天 10 万下载；同一模型一个月内可能换版本/换榜，引用务必带日期。
7. **自报 vs 第三方**：OmniDocBench 官方表相对可信；模型卡里的「#1」多半是自测（不同评测脚本、不同后处理）。

---

## 9. 落地验证清单

1. 抽 **100–300 页真实业务文档**（含扫描、表格、公式、多栏、印章）。
2. 分别算：文本编辑距离、表格 TEDS/TEDS-S、公式 CDM、阅读顺序错误率。
3. 看 **P95 延迟、页/秒、显存**（同硬件对比，别用厂商数字）。
4. 检查**公式 LaTeX 能否被 KaTeX/MathJax 正常渲染**、表格 HTML 是否能被下游解析。
5. 做 **license 合规审查**（尤其 OXR/Youtu/MinerU/Datalab 系）。
6. 先影子运行：与现有 OCR/人工结果对比，统计「必须人工复核」的比例，再决定放量。

---

## 10. 结论

1. **2026 年开源文档解析模型已经超过通用大模型**（Gemini 3 Pro 92.91 vs 开源前五 96.3–97.0），这是今年 OCR 赛道最大的结构性变化。
2. **精度第一梯队是 0.8B–1.3B 的专用小模型**：OXR、TeleOCR、Xiaomi-OCR-0、OvisOCR2/3、MiniCPM-OCR、PaddleOCR-VL-1.6——不是越大越准。
3. **热度第一是百度 Unlimited-OCR**（30k★/412 万下载/MIT），但它不是最准的；**GLM-OCR 是最流行的 0.9B MIT 模型**；**PaddleOCR 生态最大**。
4. **「最夯 + 最好用」的平衡点**：中文生产环境 → **PaddleOCR-VL-1.6**；极限精度 → **OXR-1.0**（注意许可）；Apache-2.0 且表格强 → **TeleOCR**；小模型公式强 → **Xiaomi-OCR-0**；超长文档 → **Unlimited-OCR-2**。
5. **别只看一个榜**：OmniDocBench 强的 GLM-OCR 在 olmOCR-Bench 只有 75.2；用你自己的文档做最终裁决。

---

## 附录：数据来源

| 来源 | 用途 | 链接 |
|---|---|---|
| OmniDocBench（opendatalab） | v1.6 端到端榜单、文本/公式/表格/阅读顺序 | https://github.com/opendatalab/OmniDocBench |
| olmOCR-Bench（AllenAI） | 真实难例交叉验证 | https://huggingface.co/datasets/allenai/olmOCR-bench |
| OXR 官方 benchmark 文档 | OXR 自测与竞品对比 | https://github.com/SoMarkAI/OXR |
| 各模型卡 | 许可、参数、自报分数 | 见正文模型名 |
| GitHub / Hugging Face API | star、likes、下载量（2026-11-01 实测） | — |
| 量子位 OXR 报道（厂商供稿） | OXR 背景/速度/RSI | https://www.qbitai.com/2026/10/505635.html |

> 免责声明：榜单分数为第三方/厂商公开数据，版本与口径变化快；许可证以模型卡与 LICENSE 原文为准，商用前请法务复核。
