# 研究记录：本地推理模型（OCR + 判定）获取、校验与 serving

> 模块与范围：`inference`（模型注册表/本地 sidecar）· 任务 T19 前置（M0 并行）
> 记录日期：2026-11-02（Asia/Shanghai）· 实测机器：Apple M4 / 16 GB / macOS 27.0.1
> 关联：`SPEC-inference.md` §2（角色/注册表/许可门禁）§4（sidecar 生命周期）、`CONFIG.md` §4/§5/§9.5/§10、`AGENTS.md`
> 交付物：`scripts/model/{pull,verify,serve,health}.sh` + `registry.tsv` + `profiles.tsv` + `defaults.conf`、`config/cloud.deepseek.toml`

---

## 1. 问题清单（实现前需要确认的外部事实）

| # | 问题 | 结论落点 |
|---|---|---|
| Q1 | `ocr.model` 默认的 PaddleOCR-VL-1.6 到底有没有可直接下载的 GGUF？谁发布的？ | §4.1 |
| Q2 | 官方 GGUF 的**量化类型**是什么？（规范写「0.9B Q4」） | §4.1、§7 偏差 D1 |
| Q3 | llama.cpp 是否支持 PaddleOCR-VL / GLM-OCR / Xiaomi-OCR-0 的架构？ | §4.1、§5.1 |
| Q4 | 判定用的「4B 指令模型 Q4」选哪个基座、哪个 GGUF、什么许可？ | §4.2 |
| Q5 | macOS 上怎么拿到 llama.cpp？版本与 OpenAI 兼容 serving 命令是什么？ | §3、§5.2 |
| Q6 | M4/16 GB 上真实的内存占用、冷启动、单页 OCR 与单题判定的延迟？ | §6 |
| Q7 | 下载流程如何做到断点续传、sha256 校验、失败回滚、>5 GB 先报告？ | §5.3 |
| Q8 | DeepSeek 作为云端兜底的当前 base_url、模型名、上下文、价格、结构化输出能力？ | §8 |
| Q9 | 调研报告里的「Jev 类判定模型」能否在本项目的 llama.cpp 后端里用？ | §4.2、§7 偏差 D3 |

---

## 2. 来源（类型 / 日期 / 用途）

| 来源 | 类型 | 日期 | 用途 |
|---|---|---|---|
| https://huggingface.co/PaddlePaddle/PaddleOCR-VL-1.6-GGUF | 模型卡/仓库（官方发布方 PaddlePaddle） | 2026-11-02 抓取 | Q1/Q2/Q3：官方 GGUF 文件、大小、许可、llama.cpp 用法 |
| https://huggingface.co/api/models/PaddlePaddle/PaddleOCR-VL-1.6-GGUF | Hugging Face API（`x-linked-etag` 即 SHA-256） | 2026-11-02 | 哈希来源 |
| https://github.com/ggml-org/llama.cpp/pull/18825 | 上游 PR（PaddleOCR-VL 支持） | 2026-11-02 读取 | Q3 |
| https://github.com/ggml-org/llama.cpp/blob/master/docs/multimodal.md | 上游文档 | 2026-11-02 | OCR 模型在 llama.cpp 的支持清单与提示词注意 |
| https://github.com/ggml-org/llama.cpp/pull/19677 | 上游 PR（GLM-OCR 支持） | 2026-11-02 | Q3 |
| https://huggingface.co/ggml-org/GLM-OCR-GGUF | 上游组织的现成量化 | 2026-11-02 | 备选 OCR |
| https://huggingface.co/Qwen/Qwen3.5-4B · https://huggingface.co/unsloth/Qwen3.5-4B-GGUF | 模型卡 + 社区量化 | 2026-11-02 | Q4 默认判定模型与许可（Apache-2.0） |
| https://huggingface.co/h2oai/h2o-lightning-4b-gguf | 官方 GGUF | 2026-11-02 | 备选判定模型 |
| https://huggingface.co/LunarOilRig/PaddleOCR-VL-1.6-GGUF-Q4 | 第三方量化 | 2026-11-02 | Q2：规范要的 Q4 只有第三方版本 |
| https://api-docs.deepseek.com/（「首次调用 API」「模型 & 价格」「获取模型列表」） | 官方 API 文档 | 2026-11-02 | Q8：base_url、模型名、1M 上下文、价格、JSON output |
| `ocr-open-models-report.md` / `jev-open-decision-models-report.md`（仓库内） | 内部调研（2026-11-01） | 2026-11-01 | 候选清单与许可排除项，**不重复联网调研** |
| 本机实测：`brew install llama.cpp`、脚本运行日志 | 一手实测 | 2026-11-02 | Q5/Q6/Q7 |

> 说明：本次**只补**「可下载性 / 版本 / 许可 / serving / 硬件占用」这类落地事实，没有重做模型效果调研；
> 精度结论仍引用仓库内两份报告的日期与口径（OmniDocBench v1.6、JevBench v1.6.1，2026-11-01）。

---

## 3. 版本实测（本机）

| 组件 | 版本 / 实测值 | 获取方式 |
|---|---|---|
| 硬件 | Apple M4，10 核，16 GiB 统一内存（`hw.memsize=17179869184`） | `sysctl` |
| 系统 | macOS 27.0.1（Darwin 27.0.0，arm64） | `sw_vers` / `uname -a` |
| 推理服务 | **llama.cpp 0.8.0**，build **11960**，commit `8642541b2`，AppleClang 21.0.0.21000334 for Darwin arm64 | `brew install llama.cpp`；`llama-server --version` |
| 下载/校验 | curl 8.7.1、`shasum -a 256`、`jq`、`awk`（无额外依赖） | 系统自带 |
| 观测辅助 | `ps -o rss`、`vm_stat`、`memory_pressure` | 系统自带 |

**为什么用 llama.cpp 而不是 MLX/vLLM**：`SPEC-inference.md` §2.2 要求 `local_server` 是 OpenAI 兼容 HTTP sidecar，且 §10-Q5 尚未决定是否允许 MLX；
llama.cpp 是上游对 PaddleOCR-VL/GLM-OCR **有正式 PR 支持**的框架，Homebrew 一行安装、arm64 原生带 Metal，最符合「先跑通再换」的策略。
注册表/脚本已把 serving 命令与模型解耦（`profiles.tsv`），将来换 MLX 不需要改 `pull.sh`/`verify.sh`。

---

## 4. 实际可下载的版本、来源、大小与许可

所有哈希取自 Hugging Face 的 `x-linked-etag`（LFS 对象的 SHA-256），并在下载后由 `verify.sh` 重算比对。
**权重的权威清单是 `scripts/model/registry.tsv`**，本节是它的可读说明。

### 4.1 OCR 角色

| 注册表 id | 文件 | 字节数 | 量化 | 许可 | 来源仓库 | llama.cpp 支持 |
|---|---|---:|---|---|---|---|
| `paddleocr-vl-1.6`（**默认**） | `PaddleOCR-VL-1.6-GGUF.gguf` | 935,769,056 | **BF16**（`general.file_type=32`，LM 467M 参数） | Apache-2.0 | `PaddlePaddle/PaddleOCR-VL-1.6-GGUF`（官方） | ✅ PR #18825，模型卡直接给 `llama-server` 用法 |
| `paddleocr-vl-1.6`（默认） | `PaddleOCR-VL-1.6-GGUF-mmproj.gguf` | 881,770,560 | BF16 视觉投影器 | Apache-2.0 | 同上 | ✅ 必需 |
| `paddleocr-vl-1.6-q4` | `PaddleOCR-VL-1.6-Q4_K_M.gguf` | 300,219,360 | **Q4_K_M**（第三方） | Apache-2.0（仓库标注） | `LunarOilRig/PaddleOCR-VL-1.6-GGUF-Q4`（第三方） | ✅ 本机加载并 OCR 成功（§6.4）；来源非官方 |
| `paddleocr-vl-1.6-q4` | `mmproj-Q8_0.gguf` | 597,565,600 | Q8_0 视觉投影器 | 同上 | 同上 | ⚠️ |
| `glm-ocr` | `GLM-OCR-Q8_0.gguf` | 950,433,408 | Q8_0 | MIT（上游 `zai-org/GLM-OCR`；GGUF 仓库无 license 标签） | `ggml-org/GLM-OCR-GGUF`（上游组织） | ✅ PR #19677 + 本机加载实测 |
| `glm-ocr` | `mmproj-GLM-OCR-Q8_0.gguf` | 484,403,648 | Q8_0 | MIT | 同上 | ✅ |
| `xiaomi-ocr-0` | `Xiaomi-OCR-0.Q4_K_M.gguf` | 529,297,664 | Q4_K_M | Apache-2.0 | `prithivMLmods/Xiaomi-OCR-0-GGUF`（第三方量化；上游 `xiaomi-research/Xiaomi-OCR-0` 只有 safetensors） | ✅ 本机加载成功，但公式输出非 LaTeX（§6.4） |
| `xiaomi-ocr-0` | `Xiaomi-OCR-0.mmproj-bf16.gguf` | 207,346,624 | BF16 视觉投影器 | Apache-2.0 | 同上 | ⚠️ |

**关键发现（与规范表述不一致，见 §7 D1）**：官方发布的 PaddleOCR-VL-1.6 GGUF 是 **BF16**，不是 Q4。
用 `gguf` Python 包读元数据确认：主权重 `general.file_type=32`、`general.size_label=467M`（语言模型部分），
视觉编码器另在 mmproj 里，合计约 0.9B 参数、1.8 GB 落盘。规范里「0.9B Q4」对应的是**第三方**量化（~857 MB），
官方仓库里不存在 Q4。两条都已登记，默认走官方 BF16（来源可验证、精度无损），Q4 作为可选紧凑版。

**许可门禁**：上述全部命中白名单（MIT / Apache-2.0）。按规定默认排除的 OXR（营收条款）、Youtu（地域限制）、
MinerU（规模条款）、Datalab/OpenRAIL-M、CC-BY-NC 系**一律未登记**——`pull.sh` 在下载前就会拒绝非白名单许可。

### 4.2 判定 / 路由 / 规则整理角色（共用 4B 指令模型）

| 注册表 id | 文件 | 字节数 | 量化 | 许可 | 来源 | 说明 |
|---|---|---:|---|---|---|---|
| `qwen3.5-4b-instruct`（**默认**） | `Qwen3.5-4B-Q4_K_M.gguf` | 2,740,937,888 | **Q4_K_M**（`general.file_type=15` 实测确认） | Apache-2.0 | `unsloth/Qwen3.5-4B-GGUF`（基座 `Qwen/Qwen3.5-4B`） | 通用指令模型 + 受限 JSON 输出，对应 `CONFIG.md` §5 `grade.model_profile` 的「4B 指令模型」 |
| `h2o-lightning-4b` | `h2o-lightning-4b-Q4_K_M.gguf` | 2,708,808,736 | Q4_K_M | Apache-2.0 | `h2oai/h2o-lightning-4b-gguf`（官方） | 决策专用模型（JevBench v1.6.1 综合第 2，报告口径），需要 decision 风格提示词；登记为备选 |

**为什么默认不是「Torchcast Decision 4B」或 Torchcast/mammoth 这类榜首模型**（对应 §7 D3）：
报告明确指出 Torchcast、StartLux、mammoth 使用 **prompt 双向注意力**，用 vLLM/llama.cpp/MLX 直接加载会得到不同概率，
必须用官方 server。这与 `SPEC-inference.md` §2.2「本地后端为通用 OpenAI 兼容服务」冲突（该规范 §10-Q1 也把此列为开放问题）。
因此 v1 默认走**通用 4B 指令模型 + 受限输出 Schema**（规范 §2.4 的默认判定路径），把「是否需要决策服务子后端」留待规范拍板。

---

## 5. 实现要点（脚本如何使用）

### 5.1 目录与不变量

- 权重落在用户数据目录：`~/Library/Application Support/exam-grading/models/<id>/{model.gguf,mmproj.gguf,provenance.json}`；
  **永不进仓库**（`.gitignore` 已含 `models/`、`*.gguf`、`*.ggml`，由 env-bootstrap 维护，本任务未修改）。
- 注册表是唯一事实源：`scripts/model/registry.tsv`（每行一个文件：id/角色/组件/仓库/远端文件名/本地文件名/字节数/sha256/许可/support/备注）。
- serving 参数单独放 `profiles.tsv`（端口、ctx、temp、额外参数），默认值放 `defaults.conf`，全部可用环境变量覆盖。

### 5.2 serving 命令（实测可用）

```bash
# OCR（需要 mmproj，temp 0 保证可复现）
llama-server -m <models>/paddleocr-vl-1.6/model.gguf \
             --mmproj <models>/paddleocr-vl-1.6/mmproj.gguf \
             --host 127.0.0.1 --port 8081 -c 8192 --temp 0.0 -ngl 99 \
             -a paddleocr-vl-1.6 --no-webui

# 判定（必须关闭 Qwen3.5 的思考模式，否则输出为空，见 §5.4）
llama-server -m <models>/qwen3.5-4b-instruct/model.gguf \
             --host 127.0.0.1 --port 8082 -c 8192 --temp 0.0 -ngl 99 \
             -a qwen3.5-4b-instruct --no-webui \
             --chat-template-kwargs {"enable_thinking":false}
```

脚本入口：`./scripts/model/serve.sh start ocr|judge|<id>`、`stop`、`restart`、`status`、`logs`；
`./scripts/model/health.sh` 探 `/health` 与 `/v1/models` 并打印进程 RSS；`./scripts/model/verify.sh` 做大小 + SHA-256 校验。

### 5.3 下载流程的行为（`pull.sh`）

| 行为 | 实现 |
|---|---|
| 断点续传 | 先写到 `models/.downloads/<id>/`，`curl -C -` 续传；中断后重跑自动接着下 |
| 重试 | `curl --retry 5 --retry-delay 2 --retry-all-errors` + 慢速中断（`--speed-limit/--speed-time`） |
| 校验 | **先比字节数，再比 SHA-256**；任一不符即删除该文件并显式失败 |
| 失败回滚 | 全部文件校验通过后才在 `.install-<id>-<pid>` 组装并原子换入；失败时 trap 清理，**不会留下半个模型** |
| >5 GB 先报告 | 单模型总量超过 `LARGE_DOWNLOAD_BYTES`（默认 5 GiB）直接拒绝，需显式 `--allow-large` |
| 许可门禁 | 下载前按 `LICENSE_ALLOWLIST` 校验，非白名单拒绝 |
| 显式失败 | 未知 id/角色、端口占用、哈希不符、磁盘不足都返回非 0 并给出中文说明；**不静默换模型** |
| `--dry-run` | 只做注册表解析 + 许可/体量/空间校验并打印计划，不写盘、不联网 |

`provenance.json` 记录 id/角色/许可/安装时间/来源端点/逐文件 sha256，供审计与诊断包引用。

### 5.4 踩到的坑（按严重度）

1. **Qwen3.5-4B 默认开启思考模式**。直接请求时 `message.content` 为空、`finish_reason=length`，
   256 token 全被 `reasoning_content` 吃掉——判定直接拿不到 JSON。修复：启动参数
   `--chat-template-kwargs '{"enable_thinking":false}'`（已写进 `profiles.tsv` 的判定行）。
   依据：`Qwen/Qwen3.5-4B` 的 `tokenizer_config.json` 里 `chat_template` 显式判断 `enable_thinking is false`；
   本机 build 也提供 `--reasoning on|off|auto` 与 `--reasoning-budget`，但模板变量是最直接的开关。
2. **官方 OCR GGUF 是 BF16，不是 Q4**（见 §4.1 / §7 D1）。若必须 Q4，只能接受第三方量化，需另行精度验收。
3. **OCR 必须带 `--mmproj`**，且提示词是按元素类型设计的：`OCR:`（文本）、`Formula Recognition:`（公式）、
   `Table Recognition:`（表格）、`Chart Recognition:`、`Seal Recognition:`（印章）、`Spotting:`。
   其中 `Spotting:` 需要先把 `clip.vision.image_max_pixels` 改成 `1605632`（官方 README 给的 `gguf_set_metadata.py` 命令）；
   本项目的默认路径只用 `OCR:`/公式/表格三种，未改元数据。
4. **prompt cache 会让「同一页第二次」快 3 倍**：同一张图重复请求 2.01 s，新页面 7.18 s（§6）。批量批改按「每页冷启动」估算，别用缓存命中数字。
5. **llama-server 默认端口是 9931，且不设 API key 时会警告 CORS 全开**。本项目固定 `--host 127.0.0.1` + 每模型独立端口（8081+），
   仅本机回环可达；不要把端口暴露到局域网（`SPEC-inference.md` §8 Never：密钥/数据外泄）。
6. **`-c 8192` 对 OCR 是必要的**：一张 1240×1754 的 A4 页面在默认 `image_max_pixels` 下就吃掉 **1272 个 prompt token**；
   上下文不足会被静默截断图片（规范 §5 明确禁止截断，所以宁大勿小）。
7. **macOS 会把 mmap 的权重页回收**：两个模型同时常驻时，观测到 OCR 进程 RSS 从 1988 MB 掉到 18 MB，
   这是文件页被回收（再次访问会从 SSD 重读），不是进程崩溃；用 `/health` 与延迟判断健康，不要用 RSS 判断。
8. `brew install llama.cpp` 触发 `brew cleanup` 清掉了部分旧缓存（Blender/VS Code 安装包等），属正常行为。

---

## 6. M4 / 16 GB 实测

方法：`scripts/model/serve.sh start <id>` 后用 `health.sh` 取就绪时刻，`ps -o rss` 取常驻内存，
再发真实请求计延迟。OCR 用一张 1240×1754 的中文数学试卷页（PDF 级文本 + 公式 + 表格，见下）。

### 6.1 内存与启动

| 模型 | 量化 | 权重落盘 | 就绪后 RSS | 冷启动（首次读盘） | 热启动（页缓存命中） |
|---|---|---:|---:|---:|---:|
| `paddleocr-vl-1.6` | BF16 | 1.70 GB | **1,988 MB**（推理 3 次后 2,091 MB） | 10–11 s（首次，且有并发下载争用） | **2 s** |
| `qwen3.5-4b-instruct` | Q4_K_M | 2.55 GB | **3,241 MB** | 7–8 s | **1–2 s** |
| 两者同时常驻 | — | 4.25 GB | 工作集约 **5.2 GB** / 16 GB | — | — |

> 16 GB 结论：默认组合可常态双开，仍有 ~10 GB 余量给应用、SQLite、原图与系统。
> `SPEC-inference.md` §9.1 的「冷启动 ≤8 s」在**首次读盘**时未达标（10–11 s），热启动 2 s 达标——见 §7 D4。

### 6.2 OCR 单页延迟（`paddleocr-vl-1.6`，BF16，ctx 8192，temp 0）

| 场景 | 墙钟 | prompt | 生成 | 备注 |
|---|---:|---:|---|---|
| 新页面（冷 prompt） | **7.18 s** | 1272 tok / 5.15 s（247 tok/s） | 212 tok / 2.00 s（**105–108 tok/s**） | 真实批量场景按此估算 |
| 同一页重复（prompt cache 命中） | **2.01 s** | 1 tok（命中缓存） | 212 tok / 1.96 s | 仅用于同页重试 |
| 首次含下载争用 | 7.47 s | 1272 tok / 5.53 s | 212 tok / 1.91 s（110 tok/s） | 与冷 page 一致 |

输出质量抽样（同一请求）：中文标题/姓名行、`\(3 + 5 \times 2 = (\ )\)`、`\(x^2 - 5x + 6 = 0\)`、
`\(\sin \alpha = 3/5\)`、表格行「题目/答案」「6. \(2 + 2 =\) 4」均正确，公式输出为 **LaTeX**——满足 `ocr.emit_latex` 默认开 的预期。

### 6.3 单题判定延迟（`qwen3.5-4b-instruct`，Q4_K_M，关闭思考）

| 场景 | 墙钟 | 输出 | 生成速度 | 结果 |
|---|---:|---:|---:|---|
| 数学判分（评分标准 + 学生答案 → JSON） | **0.95 / 0.96 / 1.44 s** | 23 tok | 25 tok/s | 三次输出完全一致：`{"score":3,"confidence":0.8,"reason":"只写一个正确根得3分"}`，符合「只写一个正确根得 3 分」的评分标准 |

远低于 `SPEC-inference.md` §9.3 的「判定单题 ≤20 s」。**思考模式若误开**，单题会变成 >5.6 s 且拿不到 JSON（§5.4-1），必须关闭。

### 6.4 备选 OCR 的实测（同一张页面、同一提示词）

| 模型 | 量化 | 落盘 | RSS | 新页面延迟 | 生成 | 输出观察 | 结论 |
|---|---|---:|---:|---:|---:|---|---|
| `paddleocr-vl-1.6`（默认） | BF16 | 1.70 GB | 1,988 MB | 7.18 s | 105–108 tok/s | 全对，LaTeX 用 `\(...\)` | 精度与来源最稳，内存/延迟居中 |
| `paddleocr-vl-1.6-q4` | Q4_K_M（第三方） | 0.86 GB | **1,154 MB** | **6.17 s** | **217 tok/s** | 全对，LaTeX 用 `\(...\)`（`x^{2}` 形式） | **唯一对上「0.9B Q4」的版本**，内存减半、更快；来源为第三方，需精度验收 |
| `glm-ocr` | Q8_0 | 1.30 GB | 1,803 MB | 15.89 s | 102 tok/s | 文本/公式可用，LaTeX 用 `$...$`，表格输出 Markdown | MIT + 上游 `ggml-org` 量化，许可最省心，但单页最慢（图片拆成 2782 prompt token） |
| `xiaomi-ocr-0` | Q4_K_M（第三方） | 0.70 GB | 1,126 MB | 5.57 s | 104 tok/s | **公式输出为纯文本**（`x²`、`sin α`），未给 LaTeX；本次还漏了标题/姓名两行 | 加载可用但**不满足 `ocr.emit_latex=开`**；报告称其公式 CDM 98.50，需换提示词再评 |

> 六个模型（4 个 OCR + 2 个判定）全部**实际下载并通过 SHA-256 校验**；上表全部来自本机真实推理，
> 未测到的不写估计值。`registry.tsv` 的 `support` 列语义：`verified` = 本机 llama.cpp 0.8.0 实际加载成功并跑通，
> 而非「上游文档声称支持」。

**结论（对 D1 的意义）**：Q4 版本不是「只能将就」的替代——它比官方 BF16 **内存少 42%、新页快 14%**，
说明「0.9B Q4」这个规范表述在工程上是站得住的，缺的只是**官方没有发布 Q4**。
若负责人确认可接受第三方量化，把 `DEFAULT_OCR_MODEL` 改为 `paddleocr-vl-1.6-q4` 即可，无需改脚本。

---

## 7. 风险、限制与与规范的偏差

### 7.1 偏差（需规范/负责人拍板，本任务**未擅自改规范**）

| # | 规范原文 | 实测事实 | 影响与建议 |
|---|---|---|---|
| **D1** | `CONFIG.md` §4 / `SPEC-inference.md` §2.1/§9.9：默认 `PaddleOCR-VL-1.6（0.9B Q4）` | 官方 GGUF 只有 **BF16**；Q4 仅第三方 `LunarOilRig` 有，但实测 **1,154 MB / 6.17 s**，优于官方 BF16 的 1,988 MB / 7.18 s | 两者都已登记并实测。当前默认仍是官方 BF16（来源可验证）。**建议**二选一后改规范：① 保持官方 BF16，把规范里的「Q4」改成「官方 BF16（或注册表内等价量化）」；② 接受第三方 Q4 并跑 100–300 页自建集验收，改 `DEFAULT_OCR_MODEL=paddleocr-vl-1.6-q4`（一行，无需改脚本） |
| **D2** | `SPEC-inference.md` §2.4：判定默认「4B 指令模型 + 受限输出」 | 选型为 Qwen3.5-4B（Apache-2.0）Q4_K_M；实测 JSON 可用、确定性好 | 建议在注册表评审时确认基座与版本（`EngineStamp` 要记录 `Qwen3.5-4B` + 量化 + 提示词哈希） |
| **D3** | `SPEC-inference.md` §2.2 本地后端是通用 OpenAI 兼容服务；§10-Q1 未决 | Jev 类榜首模型（Torchcast/StartLux/mammoth）需官方 server，llama.cpp 加载概率不同 | v1 不接这类模型；若接，需新增「决策服务」子后端与注册表实现类型字段（§10-Q5） |
| **D4** | §9.1 冷启动 ≤8 s；§9.2 OCR 单页 P50 ≤6 s、P95 ≤12 s | 实测冷启动 10–11 s（首次读盘）、热 2 s；OCR 新页 7.2 s、同页重试 2.0 s | 若按 §9 验收需放宽或优化：降 ctx/image_max_pixels、换 Q4、或把 SPEC 的 8 s 定义为「热启动」。**建议先记录基线再谈门槛** |

### 7.2 风险与缓解

| 风险 | 说明 | 缓解 |
|---|---|---|
| 第三方量化不可信 | `paddleocr-vl-1.6-q4`、`xiaomi-ocr-0` 的 GGUF 由个人账号量化 | 默认不用；启用前跑 100–300 页自建集对比精度（报告 §9 的验收清单） |
| 上游版本漂移 | HF 仓库可覆盖同名文件 | 注册表锁 SHA-256；`verify.sh` 每次校验；`provenance.json` 留安装时间与来源 |
| 许可变更 | 模型卡许可可能改 | `deny`-style 白名单在 `defaults.conf`；命中排除清单即拒绝加载（规范 §2.4） |
| 16 GB 被双开挤爆 | 双模型约 5.2 GB，理论上宽裕，但用户同时开浏览器/IDE 会紧张 | `serve.sh` 每模型独立起停；OCR 与判定可按任务串行；监控 RSS（`health.sh`） |
| 图像 token 过多导致截断 | 上下文不足会静默丢图 | 固定 `-c 8192`；规范禁止截断，超限应显式拒绝（§5） |
| 端口冲突 | 固定端口便于调试但易撞 | `serve.sh` 起前检查占用并报错；`PORT=...` 覆盖 |
| 无 API key/CORS | llama-server 警告 | 只绑 `127.0.0.1`；不暴露 LAN；如未来跨进程共享再挂 `--api-key` |
| 模型全部私有化 | 许可含附加条款的模型不可用 | 已排除；只用 MIT/Apache-2.0 系 |

### 7.3 失败预案（脚本层面的显式失败）

| 症状 | 处理 |
|---|---|
| 404 / 需要登录 / 许可受限 | `pull.sh` 显式报错退出；改用注册表内 MIT/Apache-2.0 备选（如 `glm-ocr`），**不伪造成功** |
| 哈希或大小不符 | 删除该文件、非 0 退出、不安装半个模型 |
| 单模型 >5 GB | 默认拒绝，提示先报告再 `--allow-large` |
| 架构不被 llama.cpp 支持 | `registry.tsv` 的 `support` 列标 `unverified`，`pull.sh` 打警告；加载失败即换同角色备选并显式记录 |
| 加载 OOM / 启动超时 | `serve.sh` 打印最后 25 行日志、杀掉进程、非 0 退出；可降 ctx 或换 Q4 |
| 磁盘不足 | 下载前按总量 +10% 预检，不足即拒绝 |
| 云端不可用 | 按 `config/cloud.deepseek.toml`：有限重试后**显式失败**，不静默切回本地/其它模型 |

---

## 8. DeepSeek 云端兜底（`config/cloud.deepseek.toml`）

来源：官方文档 2026-11-02 核对（「首次调用 API」「模型 & 价格」「获取模型列表」）。

| 事实 | 实测/文档值 |
|---|---|
| OpenAI 兼容 base_url | `https://api.deepseek.com`（Anthropic 格式另有 `/anthropic`） |
| 模型 id | `deepseek-flash`（服务 DeepSeek-V4.1-Flash-1017；旧名 `deepseek-v4-flash` 仍可用） 与 `deepseek-v4-pro` |
| 上下文 / 输出上限 | 1,048,576 / 393,216 token；支持 JSON output、tool calls |
| 图像理解 | `deepseek-flash` 支持（`input_modalities=[text,image]`），`deepseek-v4-pro` 不支持 |
| 价格（每百万 token，元） | flash：输入缓存未命中 空闲 1 / 高峰 2；输出 空闲 4 / 高峰 8；缓存命中 0.02 / 0.04 |
| 凭据 | 只从环境变量 `DEEPSEEK_API_KEY` 读取（文件里只存变量名 `credentials_env`） |

落地约束（写进配置，不靠实现自觉）：`enabled=false`、`requires_task_consent=true`、`ttl=task_lifetime`、
`send_original_image=false`、`budget_micros_per_task=2000000`（2 元）、`fallback.policy=explicit_failure`、`silent_downgrade=false`。
即「先开总开关，再逐任务授权」双重门；关闭时云端请求 0 次。详见 `config/README.md`。

---

## 9. 复现命令（照抄可复现本次结论）

```bash
# 1. 服务端（一次性）
brew install llama.cpp && llama-server --version    # 期望 0.8.0 build 11960+

# 2. 计划与下载（权重入用户数据目录，不进仓库）
./scripts/model/pull.sh --list
./scripts/model/pull.sh --dry-run
./scripts/model/pull.sh ocr judge                  # 默认 OCR + 默认判定
./scripts/model/verify.sh                          # 大小 + SHA-256

# 3. 起停与健康
./scripts/model/serve.sh start ocr
./scripts/model/serve.sh start judge
./scripts/model/health.sh                          # 期望 health: OK
./scripts/model/serve.sh stop all

# 4. 单页 / 单题的真实请求（OpenAI 兼容）
curl -s http://127.0.0.1:8081/v1/chat/completions -H 'Content-Type: application/json' \
  -d '{"model":"paddleocr-vl-1.6","messages":[{"role":"user","content":[
       {"type":"text","text":"OCR:"},{"type":"image_url","image_url":{"url":"data:image/png;base64,<...>"}}]}],
       "max_tokens":512,"temperature":0}'
```

---

## 10. 待办 / 开放问题

1. **D1 需拍板**：默认 OCR 用官方 BF16（1,988 MB / 7.18 s）还是第三方 Q4（1,154 MB / 6.17 s，`DEFAULT_OCR_MODEL` 一行切换）。建议先用 100–300 页自建集比精度再定；延迟与内存已测，Q4 全面占优，唯一顾虑是来源。
2. 是否把 `h2o-lightning-4b`（决策专用）接入判定路径：它的提示词协议与「受限 JSON」不同，需要 `grade` 侧配合；当前只登记不下发。
3. `ocr.segment_recognition` 一旦启用（表/公式元素级调用），单页会变成多次推理，§6.2 的 7.2 s 不再是页级预算，需重测。
4. 判定置信度是否需要温度/校准表（`SPEC-inference.md` §10-Q7）：本机 temp 0 下三次同题输出完全一致，说明确定性够，但校准另算。
5. 若换 MLX/vLLM（§10-Q5），`profiles.tsv` 需要增加「实现类型」字段，`serve.sh` 相应分支。
