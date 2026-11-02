# exam-grading：单机自动作业批改系统

> 状态：**M0 工程骨架**（2026-11-02）。规范已冻结，业务模块尚未实现。
> 规范基线：`SPEC.md` v0.2.1 + 12 份模块规范 + `CONFIG.md` + `AGENTS.md`。

教师侧单机桌面应用：教师录入参考答案与评分规则（手写 → 模型整理 → **教师确认**，或套用全国卷模板），
批量导入学生作业（**文件名即学生**），本地完成识别、对齐、两层判分（标准 / 快速双模式），
再复核与导出成绩。默认离线，云端推理仅在任务级授权下启用。

## 当前进度

| 里程碑 | 内容 | 状态 |
|---|---|---|
| M0 | 工具链、workspace、13 个空 crate、CI 与文档骨架 | ✅ 本提交 |
| M1 | `domain` 类型与契约、`platform` 配置/DB/资产/任务/日志 | 未开始 |
| M2–M9 | 判分核心、采集识别、推理、规则、对齐、复核导出、桌面 UI | 未开始 |
| M10 | 端到端验收、性能基线、文档发布 | 未开始 |

任务拆分与验收标准见 `tasks/todo.md`，里程碑计划见 `tasks/plan.md`。

## 技术栈（`SPEC.md` §4）

- 语言：Rust，edition 2024，stable（固定入口 `rust-toolchain.toml`）。
- GUI：Slint（1.18.x，以 T02 实测锁定；**M0 尚未引入任何依赖**）。
- 存储：SQLite（单写连接 + WAL）+ BLAKE3 内容寻址资产库；迁移只追加。
- 并发：标准线程 + 有界通道 + CPU 线程池，不引入 async 运行时。
- 推理：本地 sidecar（默认离线）+ OpenAI 兼容云端（逐任务授权）；模型按角色注册、许可门禁。
- 质量：fmt / clippy（`-D warnings`）/ test / cargo-deny 许可白名单。

## 目录结构

```
crates/
  eg-domain/        领域类型、不变量、学科契约（零 IO）
  eg-platform/      配置、密钥、SQLite、资产库、任务队列、日志、审计、单实例
  eg-capture/       导入、EXIF、去畸变、质量门控、PDF 拆页
  eg-inference/     推理后端抽象、模型注册表、健康、预算、许可门禁
  eg-recognize/     页面 → 区块结构化、Schema 校验、缓存
  eg-align/         文件名优先的身份匹配、页面 → 题目区域对齐、未决项
  eg-rubric/        评分规则结构化（草稿 → 确认）、标准模板、静态校验
  eg-grade/         两层判分、双模式、置信度门禁、冲突仲裁、证据
  eg-subject-math/  数学学科包：归一化、比较器链、分步给分、CAS 适配
  eg-review/        复核工作台视图模型与命令
  eg-report/        汇总、统计、导出（XLSX/CSV/HTML）
  eg-app/           Slint 外壳与组合根（bin）
  egctl/            诊断、夹具回放、基准 CLI（bin）
docs/               文档系统（user / dev / adr / research / book）
scripts/            ci.sh、docs.sh、docs-check.sh（模型脚本在 scripts/model/，见对应任务）
```

依赖方向单向且禁止成环（`domain` ← `platform` ← 功能模块 ← `app`），已在各 `crates/*/Cargo.toml` 落实。

## 快速开始

### 1. 环境

需要 Rust stable（rustup）。本机实测版本与安装方式见 `docs/dev/research/toolchain.md`。

```bash
# 安装（官方脚本；已装可跳过）
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile default

# 每个新 shell 都需要（安装时未修改 PATH）：
source "$HOME/.cargo/env"
```

`cargo-deny` 与 `mdbook` 为门禁/文档工具，安装方式见研究记录。

### 2. 构建与运行

```bash
cargo build --workspace     # 编译全部 13 个 crate
cargo run -p egctl -- --version   # 诊断 CLI（M0 仅输出版本）
```

### 3. 质量门禁

```bash
bash scripts/ci.sh          # fmt + clippy + test + deny + 文档门禁
```

单独的等价命令（与 `SPEC.md` §5 一致）：

| 用途 | 命令 |
|---|---|
| 格式检查 | `cargo fmt --all -- --check` |
| 静态检查 | `cargo clippy --workspace --all-targets --all-features -- -D warnings` |
| 全量测试 | `cargo test --workspace` |
| 依赖合规 | `cargo deny check` |
| 文档构建 | `bash scripts/docs.sh`（mdBook → `docs/book/book/`） |

## 文档

| 入口 | 内容 |
|---|---|
| [`docs/README.md`](docs/README.md) | 文档索引与阅读顺序 |
| [`docs/user/`](docs/user/README.md) | 用户文档（M0 占位，随功能补齐） |
| [`docs/dev/`](docs/dev/README.md) | 架构、模块边界、夹具、性能、发布 |
| [`docs/dev/research/`](docs/dev/research/README.md) | 实现前的研究记录与版本实测（强制） |
| [`docs/adr/`](docs/adr/README.md) | 架构决策记录 |
| [`docs/book/`](docs/book/README.md) | mdBook 静态站（GitHub Pages 目标） |

## 实现纪律（强制）

- **先检索、再编码**：任何外部库/框架/模型/平台 API，先查官方文档与版本说明，写进
  `docs/dev/research/<module>.md` 后再实现（`AGENTS.md`；`CONFIG.md` 的 `dev.research_first` 不可关闭）。
- **版本以实测为准**：规范里的版本号只是下限；不得编造 API/版本。
- **规范禁止示例代码**：`SPEC*.md`、`CAPABILITY-MAP.md`、`CONFIG.md` 内不得出现代码围栏（CI 已检查）。
- **证据先行、门禁优先、默认离线**：没有证据不出分；低置信度不进最终成绩；未授权不发学生数据。

## 隐私与数据

- 默认全部本地处理；云端推理默认关闭，需按任务授权（`SPEC-inference.md`）。
- 原图按内容哈希只读保存、永久保留；日志与诊断包脱敏；API Key 只从环境变量/Keychain 读取，不入库、不入日志。

## 许可

- **本项目（exam-grading）整体以 GPL-3.0-only 发布**：GNU General Public License v3.0，仅此版本（不含 "or later"），
  完整官方文本见仓库根目录 [`LICENSE`](LICENSE)。口径：个人求职展示、源码公开，不采用商业许可。
- **Slint（GUI 框架）**：Slint 1.18.1 为三重许可
  （`GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0`）。
  本项目按其 **GPL-3.0-only 分支**使用：不启用 Slint Royalty-free 的归属（attribution）方案，也不购买商业许可；
  涉及 Slint 的代码与分发产物同时受 GPL-3.0-only 义务约束。依据：`docs/dev/research/slint.md`。
- **第三方依赖**：各自保留原始版权与许可，版本由 `Cargo.lock` 固定。合规由 `deny.toml` 白名单 +
  `cargo deny check` 门禁执行（本地 `scripts/ci.sh` 与 CI 都会跑），未列入白名单的许可默认拒绝（fail-closed）：
  - 规范基线（`SPEC.md` §10）：MIT、Apache-2.0、BSD-3-Clause、ISC、Unicode-3.0；
  - 因 Slint 与图像/PDF/压缩链追加：GPL-3.0-only、Zlib、BSD-2-Clause、BSL-1.0、Unlicense
    （逐条来源与理由见 `deny.toml` 注释及 `docs/dev/research/{slint,deps,pdf}.md`）。
- **模型权重**：OCR / 判定模型不在本仓库分发，其许可与使用条件见 `docs/dev/research/models.md` 与对应模型卡。
- **免责**：本项目为个人求职展示，按「现状」提供；不承诺持续维护，也不承诺适用于任何特定用途，使用前请自行评估。
