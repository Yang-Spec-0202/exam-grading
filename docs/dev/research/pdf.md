# 研究记录：图像 / PDF / DOCX（capture 输入管线）

> 状态：已完成（DOCX 一项待验证）· 2026-11-02（Asia/Shanghai）· 对应规范：`SPEC-capture.md`、`CONFIG.md`（`import.pdf_dpi`、`import.max_pdf_pages`）、`SPEC.md` §4（图像/PDF 行）、`AGENTS.md` §2/§4
> 实测机器：Apple M4 / 16 GB / macOS 27.0.1 / rustc 1.99.1
> 结论用于：PDF 渲染选型（go/no-go）、`capture` 依赖清单、打包与许可审批

## 1. 模块与范围

- 模块 id：`capture`（导入与拆页）
- 本次要实现的能力：图片解码/旋正、PDF 页面栅格化（→ 交给 OCR）、必要时抽取 PDF 文本层、后续可能解析 `.docx` 参考答案
- 明确不做：OCR 识别本身（见 `inference-serving.md`）、版面分析、图像增强算法效果评估

## 2. 问题清单

| # | 需要确认的外部行为/API/版本/限制 | 影响 |
|---|---|---|
| Q1 | 纯 Rust 的 PDF 栅格化候选（hayro）在 macOS arm64 能否构建、渲染质量与速度？ | 决定是否引入动态库 |
| Q2 | `pdfium-render` 在 macOS 的构建方式、动态库体积、渲染/文本抽取能力？ | 决定备选方案与打包代价 |
| Q3 | 两方案的许可是否满足 `SPEC.md` §10 白名单？随包二进制还带哪些第三方许可？ | 决定是否需审批 |
| Q4 | MuPDF / Poppler 绑定能否作为候选？ | 排除或纳入 |
| Q5 | 栅格化的边界条件：加密 PDF、透明背景、CJK 字体、300 DPI 的成本？ | 决定 `capture` 的错误路径与 `import.pdf_dpi` 落地 |
| Q6 | `.docx` 参考答案解析候选？ | 决定 `rubric`/`capture` 的输入格式支持 |

## 3. 来源

| # | URL | 文档版本/日期 | 类型 | 取用内容 |
|---|---|---|---|---|
| S1 | https://crates.io/api/v1/crates/hayro | 2026-11-02 查询：`0.8.0`（2026-10-04），license `Apache-2.0 OR MIT` | 官方 registry | 版本/许可 |
| S2 | https://docs.rs/hayro/0.8.0/hayro/ | 0.8.0 文档 | 官方 API 文档 | `render(page, &RenderCache, &InterpreterSettings, &RenderSettings, &PixmapSettings) -> Pixmap`、`render_into`、`RenderCache::new()`、re-export `hayro_interpret`/`hayro_syntax`/`vello_cpu` |
| S3 | https://docs.rs/hayro/0.8.0/hayro/struct.PixmapSettings.html | 0.8.0 文档 | 官方 API 文档 | `PixmapSettings { x_scale: f32, y_scale: f32, bg_color: AlphaColor<Srgb> }`（背景色可配，默认非白，见 §5 实测） |
| S4 | https://github.com/LaurenzV/hayro（README） | master@2026-11-02 | 官方 README | 纯 Rust、无 GPU 依赖；1600+ PDF 回归集；**不支持加密/口令 PDF**、混合/隔离组、knockout、color-key masking；明确「鲁棒性/性能/API 仍在演进」；MSRV 1.92 |
| S5 | https://crates.io/api/v1/crates/pdfium-render | 2026-11-02：`0.9.5`（2026-11-01），license `MIT OR Apache-2.0`；默认特性 `pdfium_latest`→`pdfium_8125` | 官方 registry | 版本/许可/ABI 版本选择 |
| S6 | https://github.com/bblanchon/pdfium-binaries/releases/tag/chromium/8125 | release chromium/8125（2026-10-26） | 官方预编译发布 | 资产：`pdfium-mac-arm64.tgz` 3 540 999 B、`pdfium-mac-univ.tgz` 7 166 697 B、`pdfium-v8-mac-arm64.tgz` 13 015 665 B |
| S7 | https://github.com/messense/mupdf-rs（README §License） | master@2026-11-02 | 官方 README | `mupdf` crate 为 **AGPL-3.0** |
| S8 | https://crates.io/api/v1/crates/poppler-rs | 2026-11-02：`0.26.0`（2026-02-21） | 官方 registry | 存在但为 poppler-glib 绑定（需系统 poppler + GPL 系许可，见 §6 R4） |
| S9 | https://raw.githubusercontent.com/bokuweb/docx-rs/main/README.md | master@2026-11-02 | 官方 README | `read_docx(&bytes)` 可解析 DOCX 包为 `Docx` 模型（读 + 写） |
| S10 | https://crates.io/api/v1/crates/docx-rs | 2026-11-02：`0.4.22`（2026-07-21），license `MIT` | 官方 registry | 版本/许可 |
| S11 | 本机 `<tmp-dir>/deps-spike`（hayro）与 `<tmp-dir>/pdfium-spike`（pdfium-render） | 2026-11-02 | 命令输出 + 渲染图 | 构建、渲染耗时、文本抽取、透明背景、CJK 质量（§5） |
| S12 | https://docs.rs/hayro/0.8.0/hayro/struct.RenderCache.html · https://docs.rs/vello_cpu/0.3.0/vello_cpu/struct.Pixmap.html | 0.8.0 / 0.3.0 | 官方 API 文档 | `RenderCache::new()`；`Pixmap::data_as_u8_slice()` / `width()` / `height()`（premultiplied RGBA8） |

## 4. 结论

| 问题 | 结论 | 依据 |
|---|---|---|
| Q1 | **hayro 0.8.0 可构建、可渲染、质量可用**：纯 Rust（无 C/系统库），在 macOS arm64 上 `cargo build` 直接通过；对真实中文公文 PDF（10 页、273 KB）第 1 页 A4@300 DPI（2480×3507）渲染 **14.8–25.0 ms**（release，3 次），与 pdfium 的渲染结果肉眼一致（红色版头、正文、红章、页码均正确）。 | S1–S4 + S11 |
| Q2 | **pdfium-render 0.9.5 可构建可运行**：需自带 `libpdfium.dylib`（从 bblanchon 预编译取，mac-arm64 包 3.54 MB → 解出 **7.0 MB** dylib；`VERSION` 显示 pdfium 158.0.8125.0，与 crates 默认 `pdfium_8125` 对齐）。`Pdfium::bind_to_library()` 动态加载；A4@300 DPI 渲染 **16–26 ms**，`page.text()?.all()` 文本层抽取可用（388 字符）。 | S5–S6 + S11 |
| Q3 | hayro 本身 **Apache-2.0 OR MIT**（白名单内）。pdfium-render crate 是 MIT/Apache-2.0，但**随包的 libpdfium.dylib 内含多份第三方许可**：freetype(FTL)、libjpeg_turbo(IJG)、libpng(libpng-2.0)、llvm-libc(Apache-2.0 WITH LLVM-exception)、libopenjpeg(BSD-2)、zlib(Zlib)、icu、highway、abseil 等（tarball `licenses/` 目录 18 个文件）——**均不在 `SPEC.md` §10 白名单**，若采用需负责人审批并在分发包内保留许可文本。 | S6 + S11（`ls licenses/`） |
| Q4 | `mupdf`(mupdf-rs 0.8.0) 为 **AGPL-3.0**；`poppler-rs` 为 GObject 绑定、依赖系统 poppler 且 GPL 系。两者都**不适合**本项目的许可与「无系统依赖」目标 → **no-go**。 | S7 + S8 |
| Q5 | 边界：hayro 明确**不支持加密/口令 PDF**（S4）→ `capture` 必须显式报「不支持的 PDF」而非静默失败；**hayro 输出默认透明背景**（实测 94% 像素 alpha=0），送 OCR 前必须合成白底（或设置 `PixmapSettings.bg_color`），否则 OCR 会把文字当黑底噪点（实测导致整页只识别出大标题）。`import.pdf_dpi=300`（`CONFIG.md`）可满足，但 300 DPI A4 = 8.7 MP；OCR 侧模型会把图缩到自身像素上限（见 `inference-serving.md`），因此**渲染 DPI 与 OCR 有效分辨率是两件事**。 | S3 + S4 + S11 |
| Q6 | `.docx` 参考答案解析候选 `docx-rs 0.4.22`（MIT，支持 `read_docx` 读）。**本次未构建/未实测**（时间与优先级），标记为待验证。 | S9 + S10 |

## 5. 版本实测

### 5.1 hayro（`<tmp-dir>/deps-spike`）

| 项 | 实测 |
|---|---|
| 版本 | **hayro 0.8.0**（+ `hayro-syntax`/`hayro-interpret` 0.8.0、`vello_cpu` 0.3.0） |
| 构建 | 随 deps crate 一起 `cargo build` 通过（debug 27.7 s 全量；hayro 部分含 `flate2/zlib-rs` 等）；release 构建 43.9 s |
| 合成 2 页 PDF | `pages=2`；2× 渲染 600×400 用时 19.0 ms（debug） |
| 真实中文 PDF（10 页，A4） | `pages=10`；A4@300 DPI **2480×3507，14.8/15.9/24.9 ms**（release 3 次）；PNG 534 KB |
| 质量 | 与 pdfium 渲染同页对照：版头红色标题、文号、正文、红章、页码均一致；无缺字/错位（附图证据 `<tmp-dir>/tech-spike-evidence/pdf-hayro.png`、`pdf-pdfium.png`） |
| **透明背景坑** | 保存出的 PNG 为 RGBA，**94.0% 像素 alpha=0**（背景透明）→ 转 RGB 后背景变黑；直接送 OCR 会严重降质。修法：`PixmapSettings.bg_color` 设白，或渲染后 `alpha_composite` 到白底（实测改白底后 OCR 全文正确） |
| API 事实 | `Pdf::new(Vec<u8>)` → `pdf.pages()` → `render(page, &RenderCache::new(), &InterpreterSettings::default(), &RenderSettings::default(), &PixmapSettings{..})` → `Pixmap::data_as_u8_slice()`；`RenderCache` 建议每文档建一次复用（S12） |

### 5.2 pdfium-render（`<tmp-dir>/pdfium-spike`）

| 项 | 实测 |
|---|---|
| 版本 | **pdfium-render 0.9.5** + pdfium **158.0.8125.0**（bblanchon `chromium/8125`） |
| 获取 | `curl -L .../pdfium-mac-arm64.tgz`（3 540 999 B）→ `lib/libpdfium.dylib` **7.0 MB**；tarball 还含 `include/`（C 头）、`licenses/`（18 个许可文件）、`PDFiumConfig.cmake`、`VERSION` |
| 加载 | `Pdfium::bind_to_library(env!("PDFIUM_DYNAMIC_LIB_PATH"))` → `Pdfium::new(bindings)`；**运行时 dlopen**，`otool -L` 不出现该 dylib |
| 渲染 | A4@300 DPI 2480×3507：**16.0/16.6/26.0 ms**（release 3 次） |
| 文本层 | `page.text()?.all()` → `text_len=388`（可用；`capture` 可据此判定「电子版 PDF」跳过 OCR） |
| 0.9.5 API 差异（实测踩坑） | `Pdfium::version()` **不存在**；`PdfBitmap::as_image()` 返回 `Result<DynamicImage, PdfiumError>`（需 `?`） |
| 打包代价 | 需要把 7 MB dylib 随包分发并在启动时定位（相对路径/`@rpath`/`Contents/Frameworks`）；Apple Silicon 与 Intel 需分别或 universal 包（universal 7.2 MB 压缩） |

### 5.3 排除项

| 方案 | 版本 | 结论与依据 |
|---|---|---|
| `mupdf`（mupdf-rs） | 0.8.0（2026-06-22） | **no-go**：AGPL-3.0（S7），且需构建 MuPDF C 源码 |
| `poppler-rs` | 0.26.0 | **no-go**：需系统 poppler-glib（brew 安装）、GPL 系；与「默认离线、零系统依赖」冲突 |

### 5.4 许可汇总（本模块相关）

| 组件 | 许可 | 与白名单 |
|---|---|---|
| hayro 0.8.0 / hayro-syntax / hayro-interpret | Apache-2.0 OR MIT | ✅ 通过 |
| pdfium-render 0.9.5（crate） | MIT OR Apache-2.0 | ✅ 通过 |
| libpdfium.dylib（随包二进制） | BSD-3-Clause + freetype FTL + IJG + libpng-2.0 + Apache-2.0 WITH LLVM-exception + BSD-2-Clause + Zlib + ICU… | ❌ **需审批**（若采用） |
| docx-rs 0.4.22 | MIT | ✅ 通过 |
| kamadak-exif 0.6.1（EXIF 旋正） | **BSD-2-Clause** | ❌ 需追加白名单（见 `deps.md`） |

## 6. 风险与限制

| 风险 | 影响 | 缓解/预案 |
|---|---|---|
| **R1：hayro 是 0.x，API 与能力仍在演进** | 升级可能破坏 API；部分 PDF 特性缺失 | 锁定 `hayro = "=0.8.0"`；`capture` 侧只依赖 `Pdf`/`render` 极小面并包一层 `PdfRasterizer` trait；升级前跑真实语料回归（渲染图 diff） |
| **R2：加密/口令 PDF 直接不支持（hayro）** | 教师可能导入加密 PDF | 显式失败 + 中文提示；如需支持再评估 pdfium（其源码支持口令，但本项目未实测口令路径） |
| **R3：透明背景导致 OCR 严重降质（已实测踩坑）** | 识别正确率崩到只剩大标题 | `capture` 统一在渲染后合成白底；加一条快照测试：渲染 1 页 → 断言背景像素 RGB≈255/alpha=255 |
| **R4：pdfium dylib 的第三方许可（FTL/IJG/libpng-2.0/LLVM-exception 等）不在白名单** | 若采用需规范变更 + 分发包附许可 | 默认**不引入** pdfium；仅当真实语料出现 hayro 无法渲染的页面时，按「Ask first」评估并审批 |
| R5：300 DPI A4 = 8.7 MP，内存与渲染成本上升（单页 ~35 MB RGBA） | 批量导入内存峰值；OCR 前还要缩放 | 渲染后立即按目标缩放到 OCR 分辨率并释放原图；逐页处理不并发持有全份 PDF 位图 |
| R6：CJK 字体的嵌入子集/非嵌入字体 | 缺字或替换字体导致字形错 | 已验证嵌入字体中文公文渲染正确；对非嵌入字体 PDF 加显式告警（`capture` 记录「字体缺失」事件） |
| R7：`.docx` 解析未实测 | 参考答案若要求读 docx 会缺依据 | 标记**待验证**：M5 前用 `docx-rs 0.4.22` + 真实 docx 夹具验证（含表格/公式） |

## 7. 与规范的偏差

| 规范位置 | 偏差 | 处理（改规范/改实现） | 审批状态 |
|---|---|---|---|
| `SPEC.md` §4「PDF 渲染库（优先纯 Rust 或随包动态库），Plan 阶段验证」 | 验证完成：**纯 Rust hayro 可用**，故 v1 只需 hayro，无需随包 dylib | 不改规范；在 Plan 中把「PDF 渲染 = hayro 0.8.0」写成结论，pdfium 仅作备选 | — |
| `CONFIG.md` `import.pdf_dpi = 300` | 300 DPI 可渲染，但 OCR 侧有效分辨率受模型像素上限约束（见 `inference-serving.md`）；300 DPI 渲染对「仅 OCR」不带来线性收益 | 不改默认值（300 DPI 对存档/对齐/裁切仍有价值）；在 `capture` 内按用途缩放，并在 `recognize` 侧实测有效分辨率 | — |
| `SPEC.md` §10 白名单 | 若引入 pdfium 需追加多项第三方许可；`kamadak-exif` 已需 `BSD-2-Clause` | pdfium 默认不引入（无需改）；`BSD-2-Clause` 走审批 | **待负责人审批** |

## 8. 复现命令

```bash
source "$HOME/.cargo/env"

# ── hayro（纯 Rust）
mkdir -p <tmp-dir>/deps-spike/src && cd <tmp-dir>/deps-spike   # Cargo.toml: hayro = "0.8"
cargo build --release
./target/release/deps-spike        # hayro-pdf: pages=2 render=600x400 ... / pages=10 render=2480x3507 in 14.8ms
# 透明背景检查（Python + Pillow）
python3 - <<'PY'
from PIL import Image; import numpy as np
im = Image.open("/var/folders/.../hayro-page1.png")
print(im.mode, (np.asarray(im)[...,3]==0).mean())   # RGBA, alpha0≈0.94
PY

# ── pdfium（动态库）
mkdir -p <tmp-dir>/pdfium-spike && cd <tmp-dir>/pdfium-spike
curl -sL -o pdfium.tgz https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/8125/pdfium-mac-arm64.tgz
tar xzf pdfium.tgz && ls -lh lib/libpdfium.dylib         # 7.0M
# Cargo.toml: pdfium-render = "0.9"
PDFIUM_DYNAMIC_LIB_PATH=$PWD/lib/libpdfium.dylib ./target/release/pdfium-spike
# → pages=2 / render 300dpi ... / text_len=388

# ── 许可证清点
ls <tmp-dir>/pdfium-spike/licenses/    # 18 个第三方许可文件
```
