# 性能基准记录

> 状态：实测记录 · 2026-11-03 · 机器：macOS aarch64（Apple Silicon；内存未记录）· 工具链：rustc/cargo 1.99.1
> 用途：验收证据（`SPEC-report.md` §3.4 导出预算、`SPEC-app.md` §9 启动/切页预算、`SPEC.md` §7 运维命令）。
> 规则：只记录**实际跑出**的数字；未测项明确写「未测」，不用估算冒充实测。

## 1. 命令与口径

| 场景 | 命令 | 口径 |
|---|---|---|
| 报表导出（release） | `cargo test -p eg-report --release --offline --test perf -- --ignored --nocapture` | 1000 份提交 × 20 题；统计构建、单 XLSX、三格式合计；含临时目录写盘与审计 |
| 报表导出（release） | `cargo run --release -p egctl -- bench` | 1000 份提交 × 20 题；与 `eg-report` 性能测试同口径（经 CLI 路径） |
| 报表导出（debug，冒烟） | `cargo run -p egctl -- bench` | 同上，但 debug 构建；仅用于功能冒烟，不作预算依据 |
| UI 启动/切页 | `target/debug/eg-app --bench`（由 `egctl bench` 调用并解析） | `startup_ms` 各阶段、`nav_ms`/`event_ms`/`command_ms` 分位、`invoke_failures` |
| 文档门禁 | `cargo run -p egctl -- docs check --json` | 规范无代码围栏 + 57 条相对链接 + mdBook 构建 |
| doctor | `cargo run -p egctl -- --data-dir <临时目录> doctor` | 全量检查（含 report 自检双次导出、注册表/权重校验） |

## 2. 报表导出（`eg-report`，release）

| 指标 | 实测 | 预算 | 结论 |
|---|---|---|---|
| 统计构建（1000 × 20） | 126 ms | — | — |
| 单 XLSX（8 表，116 KB） | 33 ms | ≤ 8 s P95 | 达标（余量 ~240×） |
| 三格式合计（1 XLSX + 8 CSV + 1 HTML，1.26 MB，1063 行） | 80 ms | ≤ 15 s P95 | 达标（余量 ~180×） |

来源：task-35 `eg-report` 性能测试（release，`--ignored --nocapture`），2026-11-02 实测。

## 3. 报表导出（`egctl bench`）

| 指标 | 实测 release | 实测 debug（冒烟） | 预算 | 结论 |
|---|---|---|---|---|
| 统计构建（1000 × 20） | 111 ms | 1.96–2.55 s | — | — |
| 单 XLSX | 32 ms（116.8 KiB） | 332–454 ms | ≤ 8 s P95 | 达标 |
| 三格式合计 | 73 ms（10 文件 / 1.2 MiB / 1066 行） | 401–549 ms | ≤ 15 s P95 | 达标 |

说明：release 数字来自 `egctl bench`（与 `eg-report` 性能测试同口径，2026-11-03 实测，与 §2 的 126/33/80 ms 同量级）；
debug 数字只作功能冒烟，**不作为预算依据**。

## 3.1 `egctl bench` 全流程

未单独计时（命令含两轮 1000 份导出 + 一次 `eg-app --bench` 子进程；UI 子进程单次约 0.4–1.0 s，见 §4）。

## 4. UI 启动与切页（`eg-app --bench`，debug 二进制）

| 指标 | 实测 | 预算（`SPEC-app.md` §9） | 结论 |
|---|---|---|---|
| platform_open | 28.8 ms | — | — |
| ui_new | 204.1 ms | — | — |
| ui_show | 21.3 ms | — | — |
| 首帧 first_frame | 158.8 / 304.8 / 493.8 ms（三次运行） | ≤ 1.5 s（冷启动） | 达标 |
| 切页 nav P95 / P99 | 0.065 / 0.073 ms | ≤ 16 ms | 达标 |
| 事件 event P95 / P99 | 0.075 / 0.102 ms | ≤ 16 ms | 达标 |
| 命令 command P95 / P99 | 0.000 / 0.005 ms | — | 达标 |
| 命令失败数 | 0 | 0 | 达标 |

说明：`egctl` 优先调用 `target/release/eg-app`，缺失时回退 debug 并在输出中注明 profile。
`egctl bench` 会调用 `target/{release,debug}/eg-app --bench` 并解析上述字段。

## 5. 运维命令耗时

| 命令 | 实测 | 备注 |
|---|---|---|
| `doctor`（空数据目录，含 report 自检双次导出 + 注册表解析 + 端口探测） | < 1 s | 无 DB/日志/模型权重时；未计 cargo 启动时间 |
| `doctor`（已备份数据目录，含 DB 打开/迁移校验/审计链） | < 1 s | schema v3、438 KB DB |
| `replay`（report，60 学生 × 20 题，双次导出 10 文件 ×2） | < 1 s | 含两次完整 XLSX/CSV/HTML 渲染与写盘 |
| `docs check`（复用 `scripts/docs-check.sh`） | 294 ms | 57 条相对链接 + mdBook 构建（HTML）；不含 cargo 编译 |
| `backup`（首次，空库建库 + 快照 + 清单） | < 1 s | 438 KB DB，schema v3 |
| `diagnostics export` | < 1 s | 3.8 KB zip（日志/配置脱敏/环境/DB 统计） |
| `restore`（含安全副本 + 恢复后完整性校验） | < 1 s | 恢复后 `integrity_check` 通过 |

## 6. 未测项（如实登记）

| 项 | 原因 | 计划 |
|---|---|---|
| 单页预处理耗时 | `eg-capture` 未接入 `egctl`（并行开发中） | 链路就绪后用 `egctl bench` 补测 P50/P95 |
| 单题判分耗时 | `eg-grade` 当前未通过编译（并行收尾） | 同上 |
| 3000 份以上导出 | 规范开放问题 8，v1 仅要求 1000 份 | 需要时再评估流式写入 |
| 冷启动磁盘缓存未命中（模型首次加载 10–11 s） | 依赖已下载权重；当前环境未安装模型 | 模型下载后按 `docs/dev/research/models.md` 复测 |
