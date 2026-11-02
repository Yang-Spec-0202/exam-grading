# 研究记录：采集与预处理（capture / T21–T22）

> 状态：已完成（T21–T22 + D1 落库已批准并落地；DOCX 参考答案、HEIC、并发未实现，见 §7）· 2026-11-02（Asia/Shanghai）· 对应规范：`SPEC-capture.md` v0.2.1、`CONFIG.md` §2（`import.*`）、`SPEC-domain.md`（`Page`/`PageQuality`/`AssetId`）、`SPEC-platform.md`（资产库/仓储/审计）
> 实测机器：Apple M4 / 16 GB / macOS 27.0.1（26A434）/ rustc 1.99.1
> 代码：`crates/eg-capture/`（17 个模块 + 1 个基准示例 + 4 组测试）；关联研究：`pdf.md`（hayro/透明底）、`deps.md`（image/exif）、`inference-serving.md`（白底与 OCR 有效分辨率）

## 1. 模块与范围

- 模块 id：`capture`（crate `eg-capture`）
- 本次实现（T21）：格式白名单 + 文件头校验、文件名元数据无损、BLAKE3 内容寻址去重（先落盘后建档）、PDF 逐页栅格化（DPI 取配置）、多页/部分成功、导入事件与报告、取消检查点。
- 本次实现（T22）：EXIF 方向（含镜像）与旋转记录、页面轮廓检测 + 单应矫正（保守实现，失败回退）、白底合成、派生图（方向校正/透视矫正/二值/缩略图）、质量门控（模糊/曝光/高光/死黑/倾斜/分辨率/行结构）、拒收原因码、流水线版本 + 选项哈希。
- 本次实现（D1，负责人已批准）：`src/persist.rs` —— 一次导入批次的 `Submission`/`Page`/`asset_link`/`audit_log` 在**单个 `platform::db.write` 事务**内落库；失败整体回滚；提交成功后把任务阶段推进到 `Preprocess`。
- 明确不做：OCR/版面结构化（`recognize`）、身份匹配（`align`）、`platform` 的资产库/仓储实现（并行任务）、DOCX 参考答案（后续里程碑）、HEIC 解码（需 Ask first 新增依赖）、真机照片调参（用户无样本）。

## 2. 问题清单

| # | 需要确认的外部行为/API/版本/限制 | 影响 |
|---|---|---|
| Q1 | `hayro 0.8.0` 的实际 API（打开/页数/逐页渲染/尺寸）与透明背景行为？ | PDF 拆页与白底合成 |
| Q2 | `image 0.25.10` 解码 JPEG/PNG/TIFF（16 位、CMYK、alpha）后如何统一 8 位 RGB？ | 解码正确性 |
| Q3 | `kamadak-exif 0.6.1` 读取方向 1–8 的 API？8 种方向如何映射旋转/镜像？ | EXIF 方向 |
| Q4 | 质量门控阈值（`CONFIG.md` §2）默认值与边界行为？模糊度量口径？ | 拒收判定 |
| Q5 | 透视矫正的接受/回退条件如何落地？纯 Rust 无 OpenCV 能做到什么程度？ | 几何矫正 |
| Q6 | 去重与幂等：批内同字节、历史重复、疑似重复提交的判定？ | 资产/提交不重复 |
| Q7 | `eg-platform`（已实现）的资产库/时钟/id/取消接口如何适配？ | 生产接入 |
| Q8 | 性能是否满足 `SPEC-capture.md` §8 C6（1200 万像素 P50 ≤1.2s；300 DPI A4 ≤1.5s/页）？ | 验收 |
| Q9 | 30 页金标集（C4：漏放 ≤5%、误杀 ≤10%）能否满足？ | 门控质量 |
| Q10 | D1 落库：`eg-platform` 事务内 API（谁写 `page`？审计链怎么延伸？任务阶段何时推进？）与回滚/重试语义？ | 建档正确性与可审计性 |

## 3. 来源

| # | URL / 位置 | 版本/日期 | 类型 | 取用内容 |
|---|---|---|---|---|
| S1 | `SPEC-capture.md` v0.2.1（仓库内） | 2026-11-02 | 内部规范 | §2 契约、§3 数据流/门控表、§4 失败模式、§5 可观测性、§6 夹具、§8 验收 |
| S2 | `CONFIG.md` §2（仓库内） | v0.2.1 | 内部规范 | `import.*` 默认值与取值范围（DPI 300、blur 120、exposure 0.12/0.90、skew 8°、min_long_edge 1600、batch 2000、max_pdf_pages 200、max_file_mb 64、parallelism 4） |
| S3 | https://docs.rs/hayro/0.8.0/hayro/ | 0.8.0（2026-10-04） | 官方 API 文档 | `render(page, &RenderCache, &InterpreterSettings, &RenderSettings, &PixmapSettings)`、`PixmapSettings{x_scale,y_scale,bg_color}`、`RenderCache::new()` |
| S4 | https://github.com/LaurenzV/hayro（README） | master@2026-11-02 | 官方 README | 纯 Rust、1600+ PDF 回归集；不支持加密 PDF；鲁棒性/性能/API 仍在演进 |
| S5 | 本机源码 `~/.cargo/registry/.../hayro-syntax-0.8.0/src/{pdf,page}.rs` | 0.8.0 | 上游源码 | `Pdf::new(Vec<u8>)`、`pages() -> &Pages`（Deref 到切片）、`Page::render_dimensions()`、`Page<'a>` 生命周期形态 |
| S6 | https://docs.rs/image/0.25.10/image/ | 0.25.10（2026-03-10） | 官方 API 文档 | `load_from_memory_with_format`、`DynamicImage::{to_rgb8,to_rgba8}`、`imageops::{resize,grayscale,blur}`、`PngEncoder::new_with_quality` |
| S7 | https://docs.rs/kamadak-exif/0.6.1/exif/ | 0.6.1（2024-11-06） | 官方 API 文档 | `Reader::new().read_from_container(&mut Cursor)`、`Tag::Orientation`、`get_uint` |
| S8 | https://exiftool.org/TagNames/EXIF.html | 2026-11-02 查阅 | 官方标签表 | Orientation 1–8 与旋转/镜像语义 |
| S9 | 本机 `crates/eg-platform`（并行任务已交付） | 0.1.0，2026-11-02 | 上游源码 | `Platform::{open,open_with,assets,ids,clock,close}`、`AssetStore::{put,get,exists,usage}`、`UlidFactory::next_ulid`、`CancelToken`、`DataDirs::with_root`、`FixedClock::from_rfc3339` |
| S10 | 本机实测（`cargo test -p eg-capture`、`cargo run --release --example capture_bench`） | 2026-11-02 | 命令输出 | 64 个测试全绿；性能数据（§5）；真实 platform 接入（`tests/platform_bridge.rs`） |
| S11 | `docs/dev/research/pdf.md`、`deps.md`、`inference-serving.md`（本仓库） | 2026-11-02（T02） | 先前研究记录 | hayro 透明背景、image/exif 版本、白底与 OCR 有效分辨率 |
| S12 | 本机 `crates/eg-platform/src/{repo,audit,tasks}.rs` + `migrations/0001_initial.sql` | 0.1.0，2026-11-02 | 上游源码（分工前实现） | `repo::insert_submission` **内含**全部 `page` 写入；`repo::link_asset(tx,owner_type,owner_id,role,asset,created_at)`（PK=`owner_type+owner_id+role`，重复即报错）；`AuditLog::append_in_tx(tx,id,at,&NewAuditEntry)`（actor 由模块常量固定、链在本事务内延伸）；`Database::write<T,F>`（单写线程 + 单事务 + commit）；`TaskQueue::advance_stage` 只允许 `state='running'` |
| S13 | 本机实测（`cargo test -p eg-capture --test persist`） | 2026-11-02 | 命令输出 | 9 个真实 platform 落库用例全绿（见 §5） |

## 4. 结论

| 问题 | 结论 | 依据 |
|---|---|---|
| Q1 | hayro 0.8.0 可直接用：`Pdf::new(bytes.to_vec())` → `pdf.pages()`（Deref 为 `[Page]`）→ 每页 `render(page, &cache, &InterpreterSettings::default(), &RenderSettings::default(), &PixmapSettings{ x_scale: dpi/72, y_scale: dpi/72, ..Default })`；`page.render_dimensions()` 可在渲染前预限制尺寸（长边 > 4000 时按比例降 scale 并记 `downsampled`）。**渲染结果是透明背景**：必须 `composite_on_white`，集成测试断言 PDF 页四角 ≥250（白底）。 | S3/S4/S5 + S10 |
| Q2 | `image::load_from_memory_with_format` 解码后统一 `to_rgb8_white`：`Rgba8/LumaA8/Rgb32F/Rgba32F` 先转 RGBA 再按 `out = src*a + 255*(1-a)` 白底合成，其余走 `to_rgb8()`（16 位线性缩放、CMYK 由解码器转 RGB）。TIFF 只取首帧并显式记 `multipage_tiff_first_frame_only`（规范允许）。 | S6 + S10 |
| Q3 | `kamadak-exif 0.6.1` 的 `read_from_container` 读 `Tag::Orientation`；解析失败/缺失按「无 EXIF」处理并记 `exif_missing`（不失败）。8 种方向按 EXIF 语义逐像素重采样（含镜像），映射为 `Rotation`（3→180、5/6→90、7/8→270，镜像信息保留在 `ExifOrientation`）；`Page.width/height` 记录**未旋转**尺寸。表驱动单测覆盖 8 种方向的尺寸与标记点位置，另有「转 4 次 90° 回到原图」「镜像两次回原图」性质测试。 | S7/S8 + S10 |
| Q4 | 阈值全部来自 [`ImportSettings`]（默认 = `CONFIG.md` §2）：blur<120 拒、exposure<0.12 欠曝/ >0.90 过曝、高光>25% 眩光、近黑>40% 死黑、skew>8° 倾斜、长边<1600 分辨率、无行结构拒收；良好线按规范表格内部默认（blur≥300、0.25–0.75、≤2°、≥2400px、高光≤10%）。边界两侧各有用例（119.99/120、0.119/0.12、0.901/0.90、8.01/8.0、1599/1600）。模糊方差在长边 1600 口径上计算。 | S1 + S2 + S10 |
| Q5 | 纯 Rust 保守实现：降采样到长边 360 → 深色背景（边界均值 ≤140）上取「亮度 > 背景+35」掩码 → 4 邻域最大连通域 → `min(x+y)/max(x−y)/max(x+y)/min(x−y)` 取四角 → 校验面积 ≥30%、纵横比 0.5–2.0、凸性、边长 ≥20px；单应 8 参数高斯消元 + 双线性采样。**回退**：矫正后面积 <原图 40% 或 A4 纵横比偏差 >15% → 回退方向校正图并记 `perspective_fallback`；扫描件（铺满画面）检测不到轮廓 → `no_contour` 回退整图。合同夹具（深底 + 旋转 A4 纸）检测成功且矫正后纵横比接近 A4。 | S1 + S10 |
| Q6 | 资产级去重靠内容寻址（`AssetSink::put` 返回 `deduped`）；批内同字节只处理一次（其余记 `SameBatch`）；历史重复记 `Historical`；提交级判重：新提交页面资产集合与既有（含同批已建）提交重合 ≥80% 记 `SuspectedSubmission`，`DuplicatePolicy::{Ask,Skip}` 时不自动建档、`CreateAnyway` 仍建档。实测：40 份同字节 → 1 份提交、39 条同批重复；真实 `platform` 资产库下 40 份与 1 份导入的资产文件数/表行数完全一致；第二次相同内容导入（已知提交页面集合）→ 不新建提交、资产数不增加。 | S1 + S10 |
| Q7 | 适配层已落地（`src/platform_bridge.rs`）：`AssetStoreAdapter`（`put/get/exists` → 内容寻址、只追加）、`ClockAdapter`（`eg_domain::Clock`）、`IdAdapter`（`UlidFactory::next_ulid`，可失败已传导为 `CaptureError::Internal`）、`CancelAdapter`（`CancelToken`）。集成测试用 `DataDirs::with_root(tmp) + Platform::open_with(dirs, FixedClock)` 跑通真实资产库（读回字节 `AssetId::of(bytes)==asset`、派生资产存在）。**建档落库**按 D1 在 `src/persist.rs` 内直接调用 `platform` 的 db/repo/audit，见 Q10。 | S9 + S10 |
| Q8 | release 基准（`examples/capture_bench.rs`）：**1200 万像素 JPEG 单页预处理 P50 = 0.344s、P95 = 0.354s**（n=5，含解码/方向/矫正/4 张派生/质量/内存资产写入）→ C6 要求 P50 ≤1.2s、P95 ≤2.5s，**大幅达标**；**300 DPI A4 PDF = 0.204s/页**（3 页 0.611s）→ C6 要求 ≤1.5s/页，达标。 | S10 |
| Q9 | 30 页构造金标集（10 良 + 5 模糊 + 5 欠曝 + 5 倾斜 10° + 5 低分辨率，每页内容唯一避免被批内去重折叠）：**漏放 0/30（0%）、误杀 0/30（0%）**，拒收页 20、原因计数含 `blur`/`resolution` 等 → 满足 C4（≤5% / ≤10%）。注意这是**合成夹具**，真实照片需在 M4 用真实语料复标（用户当前无样本）。 | S1 + S10 |
| Q10 | D1 落库按「资产先行、业务单事务」落地（`src/persist.rs::persist`）：① 事务前校验全部 `AssetId` 已在 platform 资产库（`AssetStore::exists`），缺失即 `persist_failed` 且零行写入；② **一个** `db.write` 内按「提交（`repo::insert_submission` 会一并写全部 `page`，**不要再单独插 page**）→ `asset_link`（页面 `original` + `derived_<i>`；参考答案 `key_page_asset`）→ 审计（批次 1 条 + 每提交/每页/每参考答案资产各 1 条，`append_in_tx` 让审计链在本事务内延伸）」顺序写入，任一步失败整体回滚；③ 审计备注不含文件名/学号/姓名（可能含学生身份），批次审计 `after_hash = 选项哈希`、页面审计 `after_hash = 质量结论`；④ 提交成功后才 `advance_stage(task, Preprocess)`，该步失败返回显式错误且**数据已提交**（错误文案提示不要重试落库）；⑤ 事务失败可整体重试（回滚干净），重试撞主键会再报 `persist_failed` 而不是半份数据。实测 9 用例：正常 2 提交/多页读回一致 + 审计链完整；40 同字节→1 提交；被拒页保留 `Rejected` 且提交 `Failed{Capture}`；部分成功只落好文件；「第二份冲突」时第一份整体回滚（无 page/asset_link/audit 残留）；缺资产零行；任务阶段成功/任务未 claim 时显式失败；参考答案 `key_page_asset` 链接数量正确。 | S12 + S13 |

## 5. 版本实测

| 依赖/工具 | 实测版本 | 命令 | 结果摘要 |
|---|---|---|---|
| `hayro` / `hayro-syntax` / `hayro-interpret` | **0.8.0** | `cargo test -p eg-capture`（PDF 用例） | 3 页 PDF 拆页、页序 1→3、A4@300DPI≈2480×3507、白底四角 ≥250；损坏 PDF 报 `pdf_open_failed`；页数超限报 `pdf_page_limit` |
| `image` | **0.25.10**（features: png/jpeg/tiff） | 同上（解码/派生/编码） | JPEG/PNG/TIFF 解码、白底合成、缩放、灰度、Otsu 二值、快速 PNG 编码（`CompressionType::Fast`） |
| `kamadak-exif` | **0.6.1** | `exif` 单测 | 8 种方向映射正确；缺失 EXIF 不失败 |
| `eg-platform` | 0.1.0（并行任务交付） | `cargo test -p eg-capture --test platform_bridge` | 真实 `AssetStore` 内容寻址/去重/读回；`FixedClock`/`UlidFactory`/`CancelToken` 适配可用 |
| `eg-platform` 事务内落库 API | 0.1.0 | `cargo test -p eg-capture --test persist` | `db.write` + `repo::insert_submission`（含 page）/`repo::link_asset`/`audit::append_in_tx`/`tasks::advance_stage` 在真实 SQLite 上按 D1 语义工作；约束冲突回滚干净 |
| `eg-domain` | 0.1.0 | 全测试 | `Page`/`PageQuality`/`QualityVerdict`/`Rotation`/`ExifOrientation`/`AssetId` 校验通过 |
| `serde` / `serde_json` / `thiserror` | 1.0.229 / 1.0.154 / 2.0.21 | 全测试 | 事件与报告可序列化（脱敏断言用 JSON 序列化扫描） |
| 测试规模 | **73 个测试**：lib 47 + import 14 + persist 9 + platform_bridge 3 | `cargo test -p eg-capture` | 全绿；debug 全量约 4 分钟（30 页金标集 + 真实 platform 落库各占一部分） |
| 静态检查 | clippy / fmt | `cargo clippy -p eg-capture --all-targets -- -D warnings`、`cargo fmt -p eg-capture -- --check` | 均 0 输出（通过） |
| 性能基准 | release | `cargo run -p eg-capture --release --example capture_bench` | 12MP JPEG P50 0.344s / P95 0.354s；A4@300DPI 0.204s/页 |

## 6. 风险与限制

| 风险 | 影响 | 缓解/预案 |
|---|---|---|
| **R1：轮廓检测是保守实现**：只覆盖「深色背景 + 亮色纸」场景；扫描件/桌面与纸张同色/阴影遮挡会走 `no_contour` 回退 | 手机拍摄的透视矫正收益有限；规范 §6「倾斜 7°/12° 顶点误差 ≤2px」只对合成夹具验证 | 合成夹具已验证；M4 起用真实样本（教师提供/自拍测试页）复标，按需引入更强算法（仍优先纯 Rust）；回退路径不阻塞识别 |
| **R2：质量指标未在真实试卷照片上校准** | 手写页、折痕、阴影可能误杀/漏放 | 门控只做「几何 + 亮度」，阈值全走 `CONFIG`（可调、需审批）；30 页合成金标集 0 漏放/0 误杀；真实语料到位后跑 C4 复标 |
| **R3：HEIC 未支持**（显式 `heic_unsupported`） | iPhone 直出照片不能导入 | 规范允许「解码器不可用时显式报不支持」；新增解码依赖属 Ask first（macOS 系统图像接口或 libheif），需负责人审批后实现 |
| **R4：DOCX 参考答案未实现**（显式 `not_implemented`） | 参考答案文本导入不可用 | 任务标注「后续」；M5 前补 `docx-rs`（`pdf.md` §4 Q6）并接 `KeyPageAsset` 文档文本通道 |
| **R5：`import.parallelism` 未启用**（当前串行逐文件） | 大批量吞吐未达「并行 4」设计值 | 串行实现已满足 C6 性能（P50 0.34s/页）；并发留到 M4 性能调优（需保持事件顺序与取消语义）；记录为偏差 D4 |
| **R6：批内去重会折叠「同字节不同文件名」** | 教师用同一张图改文件名提交多份时只处理 1 份（规范 §3.4 要求如此） | 报告 `duplicates` 明示 39 条 `SameBatch`；不静默（事件 + 报告 + 可审计） |
| **R7：`KeyPageAsset` 未进 `domain`**（`PreparedKeyAsset` 为模块自有类型，符合 `SPEC-domain.md` §15.3「模块自有类型不进 domain」） | 参考答案资产没有独立表；落库只用 `asset_link(owner_type='key_page_asset', owner_id='<batch>:<page_no>')` 表达归属 | D1 已落地该表达（`persist.rs`），链接可审计；若后续要独立表/版本化，需规范审批（当前不建表） |
| **R8：`Page.derived` 的语义角色不落库** | `asset_link.role` 只能记 `original` + `derived_<i>`；`Page.derived` 是压缩后定序列表（缺项跳过），单看链接无法断言 `derived_0` 一定是方向校正图 | 读方按 `Page.derived` 列表顺序 + `SPEC-domain.md` §6 定序规则解释；如需严格角色，需在 capture 侧保留角色位（改契约，走审批） |
| **R9：debug 测试较慢**（30 页金标集约 2.5 分钟 + 真实 platform 落库约 25 秒） | CI 时间上升 | 门禁跑 debug 全量；性能验收用 release 基准（`capture_bench`）；必要时把金标集拆为 `--release` 专项任务 |
| R10：`hayro` 0.x API 可能变动 | 升级破坏编译 | `Cargo.toml` 固定 `hayro = "0.8"`；升级前跑 PDF/白底/页序用例 |

## 7. 与规范的偏差

| 规范位置 | 偏差 | 处理（改规范/改实现） | 审批状态 |
|---|---|---|---|
| **D1**：`SPEC-capture.md` §3.1「建档：写 `Submission`/`Page`」 | capture **直接依赖** `eg-platform` 的 db/repo/audit 落库（不抽新层）：`src/persist.rs::persist` 在**单个事务**内写 `Submission`（含 `Page`）/`asset_link`/`audit_log`，资产字节仍由 `AssetStore` 先行写入；失败整体回滚，提交成功后才推进任务阶段 | 负责人已批准（D1 approved，2026-11-02）：按「资产先行、业务单事务」实现；规范分工按本实现更新（不再要求组合根落库） | **已批准并实现** |
| **D2**：`SPEC-capture.md` §3.2「HEIC 经 macOS 系统图像接口解码」 | 未实现，返回显式 `heic_unsupported`（不静默跳过） | 新增解码依赖属 Ask first；批准后实现（M3） | **待负责人审批** |
| **D3**：`SPEC-capture.md` §3.2「DOCX 参考答案（段落 + 表格纯文本）」 | 未实现，返回显式 `not_implemented` | 后续里程碑实现（M5 前）；当前错误码可被 UI 识别 | 任务已标注「后续」 |
| **D4**：`CONFIG.md` `import.parallelism = 4` | 未用于并发（串行） | 实现细节；串行已达标 C6；并发调优留 M4 | — |
| **D5**：`SPEC-capture.md` §3.3「倾斜夹具顶点误差 ≤2px」 | 仅合成夹具验证（旋转纸检测 + A4 纵横比）；真实照片未验证 | 与 R1 同；M4 真机复标 | — |
| `SPEC-capture.md` §2「导入选项：重排页序 / 目标提交追加」 | 页序重排与「追加到既有提交」未实现（`target_submission` 字段未纳入请求） | 属 app 交互侧 + M3 追加页功能；当前按文件自然序、不支持追加 | — |

## 8. 复现命令

```bash
source "$HOME/.cargo/env"
cd <repo>

# 1) 依赖版本（Cargo.lock 实测）
cargo tree -p eg-capture --depth 1      # hayro 0.8.0 / image 0.25.10 / kamadak-exif 0.6.1 / eg-platform 0.1.0 …

# 2) 测试（73 个：接口/门控/PDF/EXIF/白底/去重/部分成功/金标集/真实 platform 资产库/真实落库）
cargo test -p eg-capture
cargo test -p eg-capture --test import           # 14 个集成用例
cargo test -p eg-capture --test persist          # 9 个真实 platform 落库用例（D1）
cargo test -p eg-capture --test platform_bridge  # 3 个真实 platform 资产库用例

# 3) 质量门禁
cargo clippy -p eg-capture --all-targets -- -D warnings
cargo fmt -p eg-capture -- --check

# 4) 性能（SPEC C6 验收口径，release）
cargo run -p eg-capture --release --example capture_bench
# → 12MP JPEG 单页预处理 P50=0.344s P95=0.354s
# → 300 DPI A4 PDF: 0.611s / 3 页 = 0.204s/页

# 5) 夹具说明（全部程序构造，无真实试卷）
#    tests/support/mod.rs: text_page / a4_page_hd / blurred / darkened / rotated / marked /
#                          png / jpeg / jpeg_with_orientation(1–8) / build_pdf(N)
```
