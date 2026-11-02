# 研究记录（实现前置条件）

> 依据：`AGENTS.md` §2/§4 与 `CONFIG.md` 的 `dev.research_first`（恒开，不可配置）。
> 每个模块在写实现代码之前，必须先有对应研究记录，否则不具备准入资格。

## 约定

- 文件名：`<module-id>.md`（`domain`、`platform`、`capture`、`inference`、`recognize`、`align`、`rubric`、`grade`、`subject-math`、`review`、`report`、`app`），跨模块主题可用独立名字（如 `toolchain.md`）。
- 模板：`_template.md`；表格字段不得删减。
- 来源必须可追溯：URL + 文档版本/日期 + 类型（官方文档/源码/变更日志/issue/模型卡）。
- 版本以本机实测为准（`cargo add` / `--version` 输出摘要），不得写「应该存在」的 API。
- 检索结论与规范冲突时：先提规范变更（审批），再改实现；偏差写入「与规范的偏差」。
- 提交说明需链接对应研究记录（`AGENTS.md` §5）。

## 已有记录

> 顺序与 `docs/book/SUMMARY.md` 的「研究记录」段一致；新增记录请同时补本表与 SUMMARY，再运行 `bash scripts/docs.sh`。

| 记录 | 范围 | 状态 |
|---|---|---|
| `toolchain.md` | Rust/rustup/cargo-deny/mdBook 版本与 workspace 配置依据 | 已完成（T01–T03 · M0） |
| `templates.md` | 全国卷模板数据调研（含 T25 档位/版本对齐后的数据说明） | 已完成（v0.2 · M5 数据） |
| `models.md` | 模型候选、许可、获取/校验/serving 实测 | 已完成（T19 前置 · M0/M4） |
| `slint.md` | Slint 版本/macOS 构建/窗口-模型-图片-线程 API/中文 IME/许可 | 已完成（T02 · M0） |
| `deps.md` | rusqlite(bundled)/keyring/tracing/serde/ulid/insta/proptest/image/exif/xlsx/blake3 实测与许可 | 已完成（T02 · M0） |
| `pdf.md` | PDF 栅格化选型（hayro 纯 Rust vs pdfium-render vs 排除项）+ docx 候选 | 已完成（DOCX 待验证）（T02 · M0） |
| `inference-serving.md` | llama.cpp/OpenAI 兼容端点/GGUF 可得性/M4 延迟与内存/MLX 对照 | 已完成（4B 判定未复测）（T02 · M0） |
| `cas.md` | sympy sidecar + uv/Python 3.12 可行性、协议与预算实测 | 已完成（T26 · M5） |
| `domain.md` | 领域类型/值对象/学科契约（T04–T06） | 已完成（M1） |
| `platform.md` | 配置/密钥、SQLite 迁移与仓储、资产库、任务队列、日志审计索引、备份时钟（T07–T12 + CP-B） | 已完成（M1；v2/v3 扩展见记录 §11/§12） |
| `subject-math.md` | 四层归一化、内部语法树、比较器链、全符号抽检（T13–T14 + T26–T27） | 已完成（M2/M5） |
| `inference.md` | 后端/注册表/许可门禁/预算熔断/任务级授权/严格 Schema（T19–T20） | 已完成（M4） |
| `capture.md` | 导入/拆页/去重/预处理/质量门控/白底合成/EXIF/透视矫正/单事务落库（T21–T22、D1） | 已完成（DOCX/HEIC 显式不支持）（M3） |
| `recognize.md` | 页级/段级识别、结构+语义校验、缓存、LaTeX、置信度合成（T23–T24） | 已完成（M3） |
| `align.md` | 文件名身份/未决、模板区域/版面/题号三级对齐、跨页/门禁/指纹（T28–T29） | 已完成（分组合并/条码/真实 OCR 兜底待审批）（M6） |
| `report.md` | 统计口径/双模式分列、XLSX/CSV/HTML 导出、重放与脱敏（T33–T34） | 已完成（M7） |
| `review.md` | 复核队列/命令/审计/撤销、抽检只读与转标准（T31–T32） | 已完成（M7） |
| `rubric.md` | 草稿/模板库/规则整理/静态校验/确认版本化（T15 + T25） | 已完成（M5） |
| `grade.md` | L1 路由/L2 判定/聚合/双模式门禁/抽检/幂等（T16–T18） | 已完成（M2/M6；T30 全量接入已落地） |
| `app.md` | Slint 外壳、十页导航、状态通道、性能预算（T35–T38） | 已完成（M9） |
| `egctl.md` | 运维与验收 CLI：doctor/replay/bench/docs check/backup/restore/diagnostics，`--json` 与退出码（T39） | 已完成（M10） |
