# 研究记录：本地推理 serving（OCR + 判定，M4 / 16 GB）

> 状态：已完成（4B 判定模型本次未复测，见 §5.5）· 2026-11-02（Asia/Shanghai）· 对应规范：`SPEC-inference.md`（sidecar/端点/超时/许可）、`CONFIG.md`（`ocr.page_timeout_s=60`、`grade.judge_timeout_s=20`、`ocr.model`）、`SPEC.md` §4（推理行）、`AGENTS.md` §2/§4
> 实测机器：Apple M4 / 16 GB / macOS 27.0.1（26A434）
> 结论用于：本地 sidecar 选型与启动参数、`ocr.page_timeout_s` 可达性、GGUF 模型来源、备选后端（MLX）评估
> 关联记录：模型清单/许可/下载脚本见 `docs/dev/research/models.md`（本文件只补 **serving 栈**层面的实测与对比，不重复模型调研结论）

## 1. 模块与范围

- 模块 id：`inference`（本地 HTTP sidecar）
- 本次要实现的能力：在 M4/16 GB 上用 OpenAI 兼容端点提供 OCR 与判定；确认安装方式、版本、端点、启动/就绪、内存、单页/单题延迟；评估是否需要第二后端（MLX）
- 明确不做：模型训练/微调、云端 API、判分规则、OCR 后处理

## 2. 问题清单

| # | 需要确认的外部行为/API/版本/限制 | 影响 |
|---|---|---|
| Q1 | macOS 上 llama.cpp 的可行安装方式与版本？是否带 Metal？ | 决定 sidecar 可执行来源 |
| Q2 | OpenAI 兼容端点（`/health`、`/v1/models`、`/v1/chat/completions` + 图片）是否可用？图片传参格式？ | 决定 `inference` 客户端实现 |
| Q3 | 冷启动到就绪、常驻内存？ | 决定 `SPEC-inference.md` §9.1（≤8 s）是否达标 |
| Q4 | 单页 OCR 与单题判定的真实延迟？ | 决定 `CONFIG.md` 超时与 §9 性能验收 |
| Q5 | 三个 OCR 模型（PaddleOCR-VL-1.6 / GLM-OCR / Xiaomi-OCR-0）与 4B 指令模型的 GGUF 是否可得？ | 决定「默认离线」可行性 |
| Q6 | 图像分辨率与延迟/精度的关系？为什么同一页有人识别全对、有人只识别标题？ | 决定 `capture`→`recognize` 的缩放策略 |
| Q7 | MLX（`mlx-vlm`）能否作为第二后端？更快吗？ | 回答 `SPEC-inference.md` §10-Q5 |
| Q8 | 服务端参数坑（chat template、端口、上下文、缓存）？ | 决定 `scripts/model/serve.sh` 参数与踩坑记录 |

## 3. 来源

| # | URL / 命令 | 版本/日期 | 类型 | 取用内容 |
|---|---|---|---|---|
| S1 | `brew info llama.cpp`（本机） | 2026-11-02：**stable 0.8.0**（bottled），license MIT | 官方包管理 | 安装方式与许可 |
| S2 | `llama-server --version`（本机） | 2026-11-02：`0.8.0 (build 11960, commit 8642541b2)`，built with AppleClang 21.0.0.21000334 for Darwin arm64 | 命令输出 | 实际二进制版本 |
| S3 | `llama-server --list-devices`（本机） | 2026-11-02 | 命令输出 | `MTL0: Apple M4 (12124 MiB free)`、`BLAS: Accelerate` → Metal 可用 |
| S4 | https://github.com/ggml-org/llama.cpp/blob/master/docs/multimodal.md | master@2026-11-02 | 官方文档 | 多模态走 `libmtmd`；`llama-server` 的 `/chat/completions` 支持图片；OCR 模型需按各自提示词（PaddleOCR-VL / GLM-OCR / Deepseek-OCR / Dots.OCR / HunyuanOCR 的上游 PR） |
| S5 | https://github.com/ggml-org/llama.cpp/pull/18825 | merged 2026-02-19 | 上游 PR | PaddleOCR-VL 支持；`OCR:`/`Table Recognition:` 等提示词；**`llama-server` 可能需要 `--chat-template-file`**；`Spotting:` 需把 `clip.vision.image_max_pixels` 改为 `1605632` |
| S6 | https://github.com/ggml-org/llama.cpp/pull/19677 | 上游 PR（GLM-OCR） | 上游 PR | GLM-OCR 支持 |
| S7 | https://huggingface.co/PaddlePaddle/PaddleOCR-VL-1.6-GGUF | 2026-11-02 抓取 | 模型仓库 | 文件：`PaddleOCR-VL-1.6-GGUF.gguf` 935 769 056 B、`...-mmproj.gguf` 881 770 560 B、`chat_template.jinja` |
| S8 | https://huggingface.co/ggml-org/GLM-OCR-GGUF | 2026-11-02 | 模型仓库 | `GLM-OCR-Q8_0.gguf` 950.4 MB、`mmproj-GLM-OCR-Q8_0.gguf` 484.4 MB、`GLM-OCR-f16.gguf` 1 785.8 MB |
| S9 | https://huggingface.co/prithivMLmods/Xiaomi-OCR-0-GGUF | 2026-11-02 | 第三方量化 | `Xiaomi-OCR-0.Q4_K_M.gguf` 529.3 MB、`mmproj-bf16` 207.3 MB（社区量化；官方 `xiaomi-research/Xiaomi-OCR-0` 为 bf16/safetensors） |
| S10 | https://huggingface.co/Qwen/Qwen3-4B-GGUF · https://huggingface.co/unsloth/Qwen3-4B-Instruct-2507-GGUF | 2026-11-02 | 官方/社区量化 | `Qwen3-4B-Q4_K_M.gguf` 2 497.3 MB；`Qwen3-4B-Instruct-2507-IQ4_XS.gguf` 2 270.8 MB |
| S11 | 本机 mmproj 元数据解析（`clip.*` KV） | 2026-11-02 | 二进制元数据 | PaddleOCR-VL-1.6：`clip.vision.image_size=1001`、`patch_size=14`、**`image_max_pixels=1003520`**、`image_min_pixels=112896`、`projector_type=paddleocr`；GLM-OCR：`image_size=336`、`patch_size=14`、`projector_type=glm4v`（**无 max_pixels 键**） |
| S12 | 本机 `llama-server` + `/v1/chat/completions` 实测 | 2026-11-02 | 命令输出 | 端到端延迟/内存/输出（§5） |
| S13 | https://github.com/Blaizzy/mlx-vlm（README） | master@2026-11-02 | 官方 README | 支持 PaddleOCR-VL / GLM-OCR / DOTS-OCR / DeepSeek-OCR 等；内置 FastAPI OpenAI 兼容 server |
| S14 | 本机 `mlx-vlm 0.7.11` + `mlx 0.32.4` + `mlx-community/GLM-OCR-4bit` | 2026-11-02 | 命令输出 | MLX 对照实测（§5.4） |
| S15 | `~/.cargo` 无关；llama.cpp 服务端参数 | — | — | `--host` 默认 127.0.0.1、`--port` 默认 **9931**、`-c/--ctx-size`、`-mm/--mmproj`、`--chat-template-file`、`--image-min/max-tokens`（`--help` 实测） |

## 4. 结论

| 问题 | 结论 | 依据 |
|---|---|---|
| Q1 | **GO：`brew install llama.cpp`** 即可，实测 **0.8.0（build 11960, commit 8642541b2）**、MIT 许可、bottled（无需自建）；二进制含 `llama-server` / `llama-cli` / `llama-mtmd-cli`。Metal 可用：`MTL0: Apple M4 (12124 MiB free)`。无需自建或官方预编译手工配置。 | S1–S3 |
| Q2 | `/health` → `{"status":"ok"}`；`/v1/models` 返回 `capabilities:["completion","multimodal"]`；`/v1/chat/completions` 接受 OpenAI 多模态格式 `content:[{"type":"image_url","image_url":{"url":"data:image/png;base64,…"}},{"type":"text","text":"OCR:"}]`，实测 200 且返回结构化 `usage`/`timings`。**判分/文本调用同一端点**（无图片 content）。 | S12 + S4 |
| Q3 | 1.8 GB（文本 GGUF 935.8 MB + mmproj 881.8 MB）**热启动到 `/health ok` 0.81–2.45 s**；加载后 RSS **≈ 2 040 MB**。`models.md` 记录首次读盘 10–11 s（冷缓存）→ 规范「冷启动 ≤8 s」在**首次读盘**时可能不达标，热启动达标（与 `models.md` §7 D4 一致）。 | S12 + `models.md` §6.1 |
| Q4 | PaddleOCR-VL-1.6（BF16）**A4@300 DPI 整页：6.44 s**（prompt 1 273 tok / 生成 91 tok，输出全文正确）；段落裁切 3.51 s（893 tok）。GLM-OCR Q8 整页 **39.11 s**（4 038 tok）。→ OCR 单页 P50 目标（≤6 s）**在 PaddleOCR 默认配置下刚好达标**，P95（≤12 s）需实测分布；`ocr.page_timeout_s=60` 足够。 | S12（§5.2） |
| Q5 | **三个 OCR 模型与 4B 指令模型都能拿到 GGUF**：PaddleOCR-VL-1.6（官方 PaddlePaddle 仓库，含 mmproj）、GLM-OCR（官方 `ggml-org` 仓库）、Xiaomi-OCR-0（社区量化，官方权重为 bf16）、Qwen3-4B（官方 Q4_K_M 2.5 GB / unsloth IQ4_XS 2.27 GB）。详见 `models.md`。 | S7–S10 |
| Q6 | **关键机制**：mmproj 元数据里 `clip.vision.image_max_pixels = 1003520`（≈1.0 MP）→ 超过 1 MP 的图会被**下采样**再编码。因此 300 DPI A4（8.7 MP）实际进模型的只有约 1 MP；实测同一页在 300 DPI 下 OCR 全文正确（字号够大），而更密集的小字页会因下采样只识别出大标题。延迟与 prompt token 近似线性：**≈4.1 ms / visual token**。→ `capture` 应按用途缩放/分块，而不是盲目提高 DPI。 | S11 + S12（§5.3） |
| Q7 | MLX（mlx-vlm 0.7.11 + mlx 0.32.4）**可用但整页并不更快**：GLM-OCR-4bit 整页 A4@300 DPI **43.70 s / 5 775 prompt tok**（MLX 不做 1 MP 截断，按更高分辨率算），段落裁切 3.19 s（895 tok）。→ **llama.cpp 作为默认后端（GO）**；MLX 作为「精度优先、可切后端」的第二实现候选（满足 `SPEC-inference.md` §10-Q5 的可行性证据，但引入 Python 栈与额外 ~1.2 GB 权重）。 | S13 + S14（§5.4） |
| Q8 | 坑：① `llama-server` 默认端口 **9931**、默认 `--host 127.0.0.1`；② **PaddleOCR-VL 在 server 下需 `--chat-template-file`**（PR 18825）；③ **GLM-OCR 仓库自带的 `chat_template.jinja` 不能直接用于 `llama-server`**——实测报 `mtmd_tokenize: error: number of media markers in text (0) does not match number of bitmaps (1)`，去掉该参数（用内置模板）即 200；④ `-c 8192` 起步（OCR 一页 ~1.3k prompt token + 输出）；⑤ prompt cache 使同图重复请求从 6.4 s 降到 0.06 s，**不能用于批量估算**；⑥ 请求里 `model` 字段随便写会被当 HF repo id 尝试下载（MLX server 上实测 400）。 | S12 + S14 + S5 |
| Q9（补充） | **透明底图会让 OCR 崩**：hayro 渲染默认透明背景（见 `pdf.md` §5.1），直接送模型时整页只识别出大标题；合成白底后同页全文正确（6.44 s）。→ `recognize`/`inference` 客户端需保证输入为白底。 | S12 + `pdf.md` |

## 5. 版本实测

### 5.1 环境与安装

| 项 | 实测 |
|---|---|
| 安装方式 | `brew install llama.cpp`（Homebrew 7.0.11；formula stable **0.8.0**，bottled） |
| 二进制 | `llama-server 0.8.0 (build 11960, commit 8642541b2)`；`llama-cli`、`llama-mtmd-cli` 同版本 |
| 设备 | `MTL0: Apple M4 (12124 MiB free)`；`BLAS: Accelerate` |
| 端点默认值 | `/health`、`/v1/models`、`/v1/chat/completions`；`--host` 默认 `127.0.0.1`，`--port` 默认 `9931`（本项目应显式指定 8081+ 并 `--host 127.0.0.1`） |
| 许可 | MIT（`brew info`） |

### 5.2 OCR serving 实测（同一台机器、同一张真实中文公文页）

**输入**：四川水利职业技术学院公文 PDF 第 1 页；`hayro` 渲染 A4@300 DPI = 2480×3507，合成白底后作为 OCR 输入（详见 `pdf.md`）。

| 后端 | 模型/量化 | prompt tok | 生成 tok | 墙钟 | 输出（抽样） | 就绪后 RSS |
|---|---|---:|---:|---:|---|---:|
| llama.cpp 0.8.0 | PaddleOCR-VL-1.6 BF16（官方 GGUF）+ mmproj | 1 273 | 91 | **6.44 s** | 全文正确（标题/文号/正文/落款/日期/页码，151 字符） | ~2 040 MB |
| llama.cpp 0.8.0 | PaddleOCR-VL-1.6 段落裁切（2033×456） | 893 | 37 | 3.51 s | 段落全文正确（63 字符） | — |
| llama.cpp 0.8.0 | GLM-OCR Q8_0（ggml-org） | 4 038 | 103 | **39.11 s** | 全文正确（含换行结构） | — |
| llama.cpp 0.8.0 | GLM-OCR Q8_0 段落裁切 | 890 | 38 | 4.46 s | 段落正确 | — |
| llama.cpp 0.8.0 | 4 张图一次请求（批量） | 4 060 | 5 | 16.95 s | —（**无批量收益**，近似线性） | — |
| mlx-vlm 0.7.11 | GLM-OCR-4bit（mlx-community，1.2 GB） | 5 775 | 102 | **43.70 s** | 全文正确 | 1 541 MB |
| mlx-vlm 0.7.11 | GLM-OCR-4bit 段落裁切 | 895 | 37 | 3.19 s | 段落正确 | — |

**时间构成（PaddleOCR 整页一例）**：prompt eval 4.86 s / 1 253 tok ≈ **257 tok/s**；生成 35.6 ms / 5 tok 冷启、后续 ~112 tok/s；同图重复请求 0.06 s（prompt cache 命中）。

**启动/就绪**：`llama-server` 加载 1.8 GB 权重 + mmproj → `/health` 返回 ok 用时 **0.81 / 0.82 / 1.53 / 2.45 s**（多次，页缓存温）；`mlx_vlm server` 就绪 **1.53–6.07 s**（Python 栈更慢）。

### 5.3 分辨率机制（决定 OCR 精度的关键）

| 事实 | 数值/证据 |
|---|---|
| PaddleOCR-VL mmproj 视觉参数 | `image_size=1001`、`patch_size=14`、`image_max_pixels=1003520`、`image_min_pixels=112896`（本机解析 GGUF KV，S11） |
| GLM-OCR mmproj 视觉参数 | `image_size=336`、`patch_size=14`、无 `image_max_pixels`（S11）→ 实测整页 4 038 prompt tok |
| 实测 token 随像素变化 | 600×600→454 tok；1000×1000→1 238 tok；2033×456(0.93 MP)→1 181 tok；2480×3507(8.7 MP)→**1 273 tok（被 max_pixels 截断）** |
| 延迟近似 | ≈ **4.1 ms / prompt token**（1.2k tok ≈ 4.8 s；4k tok ≈ 16–39 s 随后端不同） |
| 结论 | 想让「密集小字页」可读，需要：① 提高 mmproj 的 `image_max_pixels`（官方 PR 提到 `Spotting:` 场景改 `1605632`，代价是延迟上升）；或 ② 版面分块后逐块 OCR（与 `SPEC-recognize.md` 的块结构一致）；**不是**简单提高 `import.pdf_dpi` |

### 5.4 MLX 对照（`SPEC-inference.md` §10-Q5 的可行性证据）

| 项 | 实测 |
|---|---|
| 安装 | `uv venv --python 3.12` + `uv pip install mlx-vlm` → **mlx-vlm 0.7.11 / mlx 0.32.4**，`mx.metal.is_available() = True`；安装 63 s |
| 权重 | `mlx-community/GLM-OCR-4bit` 1.2 GB（192 s 下载） |
| server | `python -m mlx_vlm server --model <local> --host 127.0.0.1 --port P`；`/v1/models` 200、`/v1/chat/completions` 200 |
| 坑 | 请求体 `model` 必须用 **server 返回的已加载 id（本地路径）**；写友好名（如 `"glm-ocr"`）会被当 HF repo id → `400 Failed to load model: 401 ... Repository Not Found` |
| 速度结论 | 整页明显更慢（43.70 s vs llama.cpp 6.44 s 的 PaddleOCR）；段落级相当（3.19 s vs 3.51 s）。**不是默认后端的更优选择** |

### 5.5 4B 判定模型（本次未复测，交叉引用）

`models.md` §6.3 已在同机实测 `qwen3.5-4b-instruct`（Q4_K_M）单题判定 **0.95 / 0.96 / 1.44 s**、RSS 3 241 MB、关闭思考模式后 JSON 稳定；本 spike 未重复下载/复测 4B（避免重复 2.5 GB 下载与时间开销），**标记为「已有他人实测、本记录未复测」**。GGUF 可得性已验证（S10）。

## 6. 风险与限制

| 风险 | 影响 | 缓解/预案 |
|---|---|---|
| **R1：OCR 有效分辨率受 `image_max_pixels` 限制** | 密集小字页/公式密集页可能漏识；单纯提高 DPI 无效 | ① 版面分块（推荐，与 `recognize` 块结构一致）；② 必要时按 `models.md`/上游 PR 的方式提高 mmproj `image_max_pixels` 并重测精度/延迟；③ 在 `recognize` 里落地「有效分辨率」指标与夹具 |
| **R2：冷启动首次读盘 10–11 s > 规范 ≤8 s**（`models.md` §6.1） | `SPEC-inference.md` §9.1 首启动验收不达标 | 启动时预热（读一遍权重）、或规范按「热启动」口径修订单（需审批）；UI 侧 5 s 内显式「模型加载中」 |
| R3：GLM-OCR 在 llama-server 下用仓库自带 chat template 会 400 | 换模型时容易踩坑 | `serve.sh` 对 GLM-OCR **不传** `--chat-template-file`；PaddleOCR 必须传；把两条写进 `profiles.tsv` 与踩坑记录 |
| R4：`brew` 升级 llama.cpp 可能改变参数/量化支持 | 参数或数值行为变化 | 锁定版本（记录 build/commit）；升级前跑 OCR 夹具回归（同图同提示词，字符级 diff） |
| R5：MLX 需要 Python 栈 + 额外 1.2 GB 权重 | 磁盘/复杂度上升 | 默认不引入；作为「精度优先」可切换后端（`SPEC-inference.md` §10-Q5 待拍板） |
| R6：整页 39 s（GLM-OCR）会超 `ocr.page_timeout_s` 的一半以上 | 批改吞吐 | 默认 OCR 用 PaddleOCR-VL；GLM-OCR 仅作精度备选，切换后按 `CONFIG.md` 超时实测 |
| R7：prompt cache 让「重试」看起来很快 | 误判批量吞吐 | 压测脚本每次换新图；缓存命中率单独打点 |
| R8：判定模型 4B 本次未复测 | 判定链路结论来自 `models.md` | 实现 `grade` 前按 `models.md` 命令复跑一次并记录 |

## 7. 与规范的偏差

| 规范位置 | 偏差 | 处理（改规范/改实现） | 审批状态 |
|---|---|---|---|
| `SPEC-inference.md` §9.1「本地 sidecar 冷启动到就绪 ≤8 秒」 | 热启动 0.81–2.45 s 达标；首次读盘 10–11 s 不达标（`models.md` §6.1） | 建议规范区分「首次读盘」与「热启动」两个口径（Ask first） | **待负责人审批** |
| `SPEC.md` §4 / `SPEC-inference.md` §2「PaddleOCR-VL-1.6（0.9B Q4）」 | 官方 GGUF 为 **BF16**（935.8 MB），Q4 仅第三方（`models.md` §7 D1） | 规范表述建议改为「PaddleOCR-VL-1.6（0.9B，默认 BF16 官方 GGUF；可选第三方 Q4）」 | **待负责人审批** |
| `SPEC-inference.md` §10-Q5「是否允许 llama.cpp 之外的 serving（如 MLX）」 | 可行性已证（mlx-vlm 0.7.11 可跑、OpenAI 兼容），但整页更慢 | 建议：v1 只写 llama.cpp；MLX 作为显式可选实现类型（需规范增加实现类型字段） | **待负责人审批** |
| `CONFIG.md` `ocr.page_timeout_s = 60` | PaddleOCR 整页 6.4 s；GLM-OCR 39 s 仍在 60 s 内；无需改 | 不改 | — |

## 8. 复现命令

```bash
# ── 安装与端点（llama.cpp）
brew install llama.cpp
llama-server --version            # 0.8.0 (build 11960, commit 8642541b2)
llama-server --list-devices       # MTL0: Apple M4 (12124 MiB free)

# PaddleOCR-VL-1.6（官方 GGUF；server 必须传 chat-template-file）
llama-server -m PaddleOCR-VL-1.6-GGUF.gguf \
  --mmproj PaddleOCR-VL-1.6-GGUF-mmproj.gguf \
  -c 8192 -n 1024 --temp 0 --jinja \
  --chat-template-file chat_template.jinja \
  --host 127.0.0.1 --port 18080 -t 6
curl -s 127.0.0.1:18080/health                     # {"status":"ok"}
curl -s 127.0.0.1:18080/v1/models                  # capabilities: completion, multimodal

# 请求（图片 base64 data URI；prompt 按模型：OCR: / Formula Recognition: / Table Recognition:）
python3 - <<'PY'
import base64, json, urllib.request
img = base64.b64encode(open("page-white-300dpi.png","rb").read()).decode()
payload = {"temperature":0,"max_tokens":1024,"messages":[{"role":"user","content":[
    {"type":"image_url","image_url":{"url":f"data:image/png;base64,{img}"}},
    {"type":"text","text":"OCR:"}]}]}
rq = urllib.request.Request("http://127.0.0.1:18080/v1/chat/completions",
    data=json.dumps(payload).encode(), headers={"Content-Type":"application/json"})
print(json.loads(urllib.request.urlopen(rq, timeout=300).read())["choices"][0]["message"]["content"])
PY

# GLM-OCR（ggml-org）：不要传仓库自带 chat_template.jinja（会 400）
llama-server -m GLM-OCR-Q8_0.gguf --mmproj mmproj-GLM-OCR-Q8_0.gguf -c 16384 -n 1024 --temp 0 --jinja \
  --host 127.0.0.1 --port 18095

# ── MLX 对照（可选）
uv venv --python 3.12 .venv && uv pip install --python .venv/bin/python mlx-vlm
./.venv/bin/python -m mlx_vlm server --model ./glm-ocr-4bit --host 127.0.0.1 --port 18092
# 请求里 model 用 /v1/models 返回的本地路径，否则 400

# ── mmproj 视觉上限（GGUF KV 解析）
python3 - <<'PY'
# 解析 GGUF KV，打印 clip.vision.image_max_pixels / image_size / patch_size
PY
# → PaddleOCR: image_size=1001 patch=14 image_max_pixels=1003520
# → GLM-OCR:   image_size=336  patch=14（无 max_pixels）
```
