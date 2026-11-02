# 研究记录：存储与基础依赖（platform / domain / report）

> 状态：已完成 · 2026-11-02（Asia/Shanghai）· 对应规范：`SPEC-platform.md`（数据库/Keychain/日志/审计）、`SPEC.md` §4/§10、`CONFIG.md`、`AGENTS.md` §2/§4
> 实测机器：Apple M4 / 16 GB / macOS 27.0.1 / rustc 1.99.1 / Xcode CLT
> 结论用于：`Cargo.toml` 依赖清单与锁定版本、`deny.toml` 许可审批、`platform` 模块实现前的 API 事实

## 1. 模块与范围

- 模块 id：`platform`（SQLite/Keychain/日志）、`domain`（ULID/哈希）、`report`（XLSX 导出）、跨模块测试工具
- 本次要实现的能力：单写 SQLite（WAL/外键/busy_timeout）、Keychain 密钥读写、结构化 JSONL 日志与轮转、ULID 生成、属性测试与快照测试、图片解码/EXIF、XLSX 导出、BLAKE3 内容寻址
- 明确不做：PDF 渲染（见 `pdf.md`）、模型服务（见 `inference-serving.md`）、迁移文件内容、日志全文索引实现

## 2. 问题清单

| # | 需要确认的外部行为/API/版本/限制 | 影响 |
|---|---|---|
| Q1 | `rusqlite` 最新稳定版？`bundled` 是否真能编出 SQLite？WAL/外键/busy_timeout 可用？内置 SQLite 版本？ | 决定 `platform` 能否零系统依赖落地 |
| Q2 | `keyring` 4.x 的 macOS Keychain 读写是否真能跑通？API 形态与 feature 名？ | 决定密钥方案（`SPEC-platform.md` §4） |
| Q3 | `tracing` + `tracing-appender` 能否输出按天轮转的 JSONL、带自定义字段与中文而不乱码？ | 决定日志实现（`SPEC-platform.md` §6） |
| Q4 | `ulid` 3.x 是否提供进程内单调生成器？serde 形态？ | 决定 `domain::Ulid` 实现 |
| Q5 | `insta` / `proptest` 版本与工作方式（快照落盘、回归）？ | 决定测试基建 |
| Q6 | `image` / `kamadak-exif` 版本与 EXIF Orientation 读取方式？ | 决定 `capture` 解码与旋正 |
| Q7 | `rust_xlsxwriter` 版本与中文/格式/列宽能力？ | 决定 `report` 导出 |
| Q8 | 上述依赖引入后 `cargo deny`（仓库白名单）会拒绝哪些许可？ | 决定是否需负责人审批 + 规范变更 |

## 3. 来源

| # | URL | 文档版本/日期 | 类型 | 取用内容 |
|---|---|---|---|---|
| S1 | https://crates.io/api/v1/crates/rusqlite | 2026-11-02 查询，`max_stable_version = 0.41.1`（2026-10-28 更新） | 官方 registry | 版本 |
| S2 | 本机实测 `SELECT sqlite_version()` | 2026-11-02 | 命令输出 | `bundled` 编译出的 SQLite = **3.54.1**；`rusqlite::version()` 同值；`PRAGMA journal_mode` 返回 `wal` |
| S3 | https://crates.io/api/v1/crates/keyring | 2026-11-02，`4.2.0`（2026-08-29） | 官方 registry | 版本 |
| S4 | https://docs.rs/keyring/4.2.0/keyring/ 与 `/keyring/v1/` | 4.2.0 文档 | 官方 API 文档 | 4.x 两种模式：默认 **`v1` feature** 提供 v1 兼容 API（`Entry::new(service, username)`、`set_password`、`get_password`、`delete_credential`、`store_status()`）；后端拆成独立 crate（`apple-native-keyring-store` 等），由 `v1` 特性启用 |
| S5 | https://docs.rs/keyring/4.2.0/keyring/v1/struct.Entry.html | 4.2.0 文档 | 官方 API 文档 | 方法清单与 `NoDefaultStore` 错误语义 |
| S6 | https://crates.io/api/v1/crates/tracing · /tracing-appender | 2026-11-02 | 官方 registry | `tracing 0.1.44`、`tracing-appender 0.2.5` |
| S7 | https://crates.io/api/v1/crates/serde · /serde_json · /toml · /ulid · /insta · /proptest · /image · /kamadak-exif · /rust_xlsxwriter · /blake3 | 2026-11-02 | 官方 registry | 版本：serde 1.0.229、serde_json 1.0.154、toml 1.1.8+spec-1.1.0、ulid 3.0.0、insta 1.49.0、proptest 1.12.0、image 0.25.10、kamadak-exif 0.6.1、rust_xlsxwriter 0.102.0、blake3 1.8.7 |
| S8 | 本机 `/tmp/deps-spike`（验证后已删除） | 2026-11-02 | 命令输出 | 逐项功能冒烟结果（见 §5） |
| S9 | https://docs.rs/ulid/3.0.0/ulid/struct.Generator.html（随 crate 文档）+ 实测 | 3.0.0 | 官方 API 文档 | `Generator::new()` / `generate()` 同毫秒单调；`Ulid::from_parts(ts, rand: u128)` |
| S10 | 本机 `cargo deny check licenses`（仓库 `deny.toml` 白名单） | cargo-deny 0.20.2，2026-11-02 | 命令输出 | 候选依赖树仅 `Zlib`、`BSD-2-Clause` 被拒（Slint 树另有 BSL-1.0/Unlicense/GPL 等，见 `slint.md`） |

## 4. 结论

| 问题 | 结论 | 依据 |
|---|---|---|
| Q1 | 锁定 **`rusqlite = "0.41"`（实测 0.41.1）+ `features = ["bundled"]`**。`bundled` 通过 `libsqlite3-sys` 用 CC 编译 SQLite 源码（依赖 Xcode CLT，本机已具备），**不需要任何系统 SQLite 库**，编译产物内置版本 **3.54.1**。WAL、`foreign_keys=ON`、`busy_timeout=5000`、事务/`INSERT OR REPLACE` 均实测通过。 | S1 + S2 + S8 |
| Q2 | 锁定 **`keyring = "4"`（实测 4.2.0，默认特性）**。4.x 把后端拆成独立 crate，默认 `v1` 特性启用平台后端（macOS 为 `apple-native-keyring-store`，即 Keychain）。**实测局域网真实 Keychain 写入→读取→删除全通**（`roundtrip_ok=true`，14.6 ms），无 GUI 授权弹窗（登录钥匙串已解锁）。注意 3.x 的 `features = ["apple-native"]` 在 4.x **已不存在**（编译期报 `keyring does not have that feature`），迁移时必须用默认特性。 | S3–S5 + S8 |
| Q3 | `tracing 0.1.44` + `tracing-appender 0.2.5`：`rolling::daily(dir, prefix)` + `non_blocking` + `fmt().json()` 实测产出 `app.log.2026-11-02`，每行 JSON 含 `timestamp/level/fields`，自定义字段（`module`、`trace_id`、`event_code`、`latency_ms`）与中文消息均正常（UTF-8 无乱码）。与 `SPEC-platform.md` §6「JSONL + trace_id + 模块级过滤」一致；模块级级别用 `tracing_subscriber::EnvFilter` 的 `module=level` 语法即可。 | S6 + S8 |
| Q4 | `ulid 3.0.0` + `serde` 特性：`Generator` 10 000 次生成 **全部唯一且单调递增**（同毫秒内不回退），serde 序列化为 26 字符字符串（JSON 含引号 28 字节）并可反序列化。`Ulid::from_parts(timestamp_ms, random: u128)` 可用于测试注入。与 `SPEC-platform.md`「ULID 进程内单例、同毫秒单调」一致（单例与回拨保护需自己封装，crate 只提供生成器）。 | S9 + S8 |
| Q5 | `insta 1.49.0`（JSON/文本快照，`INSTA_UPDATE=always` 生成 `.snap`，回归即 `cargo test` 失败）与 `proptest 1.12.0`（`proptest!` + `ProptestConfig { cases }`，默认回归文件 `proptest-regressions/`）均可用于「性质 + 快照」测试门禁；本机 4 个测试全绿。 | S8 |
| Q6 | `image 0.25.10`（PNG/JPEG/WEBP/TIFF/BMP 特性可裁剪，`open`/`resize`/`save` 实测通过）+ `kamadak-exif 0.6.1`（`Reader::new().read_from_container()`；**实测从合成 EXIF 的 JPEG 读出 `Tag::Orientation`**，`display_as()` 给人类可读值）。注意 `kamadak-exif 0.6.1` 上次发布 2024-11，属成熟稳定而非高频维护。 | S7 + S8 |
| Q7 | `rust_xlsxwriter 0.102.0`：实测写中文工作表名（「成绩」）、加粗表头、数字、列宽并保存（5 588 B），无需外部依赖（内部用 `zip`）。`SPEC.md` §4「Rust XLSX 写入库」成立。 | S7 + S8 |
| Q8 | 候选依赖树（不含 Slint）在仓库白名单下**只需追加 `Zlib` 与 `BSD-2-Clause`** 即可通过（其余为 MIT/Apache-2.0/BSD-3-Clause/Unicode-3.0）。`Zlib` 来自 `zlib-rs`（经 `flate2`，被 `hayro-syntax`/`png`/`tiff`/`zip` 使用），`BSD-2-Clause` 来自 TLS/图像链。**Slint 另需 4 项**（见 `slint.md` §7）。 | S10 |

## 5. 版本实测

**构建环境**：`/tmp/deps-spike`（临时 crate，验证后删除），`cargo build` 依赖全量编译 **27.7 s**，release 43.9 s（含 `image`/`hayro`/`rusqlite` bundled 的 C 编译）。

| 依赖 | 实测版本 | 命令 | 结果摘要 |
|---|---|---|---|
| rusqlite | **0.41.1**（bundled） | `cargo build` + 运行时 `SELECT sqlite_version()` | `rows=2 sqlite=3.54.1 journal_mode=wal`；`rusqlite::version()=3.54.1`；3.2 ms |
| keyring | **4.2.0**（默认 `v1`） | `Entry::new("exam-grading-spike","tech-spike")` → `set_password` → `get_password` → `delete_credential` | `roundtrip_ok=true len=16 deleted`；14.6 ms；无弹窗 |
| tracing / tracing-appender | **0.1.44 / 0.2.5** | `rolling::daily` + `non_blocking` + `fmt().json()`，写 2 条（含中文） | 文件 `app.log.2026-11-02`、2 行 JSON、字段完整、UTF-8 正常；64 ms（含 flush） |
| serde / serde_json / toml | **1.0.229 / 1.0.154 / 1.1.8+spec-1.1.0** | 编译 + ULID 序列化 | 均通过；注意 `toml` 已是 1.x（配置解析代码按 1.x 写） |
| ulid | **3.0.0**（`serde`） | `Generator` 生成 10 000 个 | 单调=true、唯一=10000、serde 往返=true；9.1 ms |
| insta | **1.49.0**（`json`） | `assert_json_snapshot!` / `assert_snapshot!` | 生成 `tests/snapshots/test_tools__insta_snapshot_shape.snap`；重跑通过 |
| proptest | **1.12.0** | 2 条性质（blake3 稳定性、ULID 往返），256 cases | 全部通过；7 ms 级 |
| image | **0.25.10** | 解码 PNG 64×48 → `resize(320,240,Lanczos3)` → 存 JPEG | 通过；debug 下 41.7 ms（release 更快），JPEG 5 651 B |
| kamadak-exif | **0.6.1** | 合成带 EXIF `Orientation=6` 的 JPEG，`read_from_container` | `orientation=Some("row 0 at right and column 0 at top")`、fields=1；1.3 ms |
| rust_xlsxwriter | **0.102.0** | 中文表名 + 加粗表头 + 数字 + 列宽 → `.xlsx` | 5 588 B，保存成功；5.8 ms |
| blake3 | **1.8.7** | `hash` 与流式 `Hasher` 对比 | 一致；425 µs（含写盘） |

> 说明：`image`/`xlsx` 的时间为 **debug 构建**下的数字，仅用于「能跑通」证据，不作为性能指标；性能验收在 release + 真实素材上另测。

**`cargo deny check licenses`（仓库 `deny.toml`，候选依赖树，不含 Slint）**

```
error[rejected]: failed to satisfy license requirements
  license = "Zlib"           # zlib-rs 0.6.8 → flate2 1.1.10 → hayro-syntax / png / tiff / zip(rust_xlsxwriter)
  license = "BSD-2-Clause"   # TLS/图像链
licenses FAILED
```

在临时副本追加 `"Zlib"`、`"BSD-2-Clause"` 后，只剩「临时 crate 自身无 license 字段」的告警（仓库内 crate 由 `[licenses.private].ignore = true` 覆盖），即**候选依赖树可全绿**。

## 6. 风险与限制

| 风险 | 影响 | 缓解/预案 |
|---|---|---|
| **R1：`Zlib`、`BSD-2-Clause` 不在 `SPEC.md` §10 白名单** | `cargo deny check` 红；没有审批就不能引入 `image`/`hayro`/`rust_xlsxwriter`（都要 `flate2`） | 请负责人审批后在 `deny.toml` 追加并注明理由（本文件 §5 已给来源链）；**不放宽到 allow-all** |
| R2：`keyring` 4.x 破坏性变更（feature 重命名、后端拆 crate） | 按 3.x 文档写会编译失败（已实测） | 锁定 `keyring = "4"` 用默认特性；实现时只经 `platform::SecretStore` 封装，禁止上层直接 `keyring::Entry` |
| R3：Keychain 行为依赖「登录钥匙串已解锁」；用户拒绝/条目缺失时读取失败 | `SPEC-platform.md` 已定义「策略拦截」路径 | 首次读取失败只提示一次并禁用云端后端；不重试轰炸；测试用 `EXAM_GRADING_DATA_DIR`/假 `SecretStore` 桩，不依赖真 Keychain |
| R4：`bundled` SQLite 需要 C 编译器；首次编译慢（本次全量 27.7 s，其中 SQLite/amalgamation 占一部分） | CI/新机首次构建时间 | CI 缓存 `target/`；`rusqlite` 只允许 `platform` 依赖（`CAPABILITY-MAP.md` 已限） |
| R5：`tracing-appender` 的 `non_blocking` 队列满会丢日志 | 与 `SPEC-platform.md` §6「错误级必须落盘」冲突 | 错误级走同步 writer（`MakeWriter` 分流），低级别走 `non_blocking`；队列丢弃计数落指标 |
| R6：`kamadak-exif` 维护频率低（0.6.1 发布于 2024-11） | 长期风险（新 JPEG 变体/安全公告） | 该 crate 只做读取、输入为本地文件；若出现未维护迹象，候选替代 `rexif`/`img-parts`（未实测，不在本记录结论内） |
| R7：`toml` 已到 1.x（2026-10 发布） | 按 0.8 写的 API 会不兼容 | 锁 `toml = "1"`；配置解析先写夹具测试（`CONFIG.md` 默认值逐项对照）再实现 |
| R8：`image 0.25` 特性裁剪需显式列出 | 忘开 `jpeg`/`png` 特性会在运行时才发现解码失败 | `Cargo.toml` 固定 `default-features = false, features = ["png","jpeg","webp","tiff","bmp"]`，并在 `capture` 加格式矩阵测试 |

## 7. 与规范的偏差

| 规范位置 | 偏差 | 处理（改规范/改实现） | 审批状态 |
|---|---|---|---|
| `SPEC.md` §10 许可白名单 | 需追加 `Zlib`、`BSD-2-Clause`（本模块链）；Slint 还需 4 项（见 `slint.md`） | **改规范（白名单追加 + 理由）**，属 Ask first | **待负责人审批** |
| `SPEC.md` §4「SQLite（单写连接 + WAL）」 | 无偏差；`bundled` 内置 3.54.1，无系统依赖 | 不改 | — |
| `SPEC-platform.md` §4「密钥经 Keychain（`keyring`）」 | 无偏差；但 4.x 的 feature 名与 3.x 不同（实现细节） | 不改；锁定 `keyring = "4"` | — |
| `CONFIG.md`（未涉及） | 无 | — | — |

## 8. 复现命令

```bash
source "$HOME/.cargo/env"
# 临时 crate（验证后删除）：rusqlite bundled / keyring / tracing / serde / ulid / insta / proptest / image / exif / xlsx / blake3
mkdir -p /tmp/deps-spike/src && cd /tmp/deps-spike
# Cargo.toml 关键行：
#   rusqlite = { version = "0.41", features = ["bundled"] }
#   keyring = "4"                       # 不要写 features=["apple-native"]（4.x 无此特性）
#   image = { version = "0.25", default-features = false, features = ["png","jpeg","webp","tiff","bmp"] }
cargo build                                  # 27.7 s
cargo run                                    # 逐项冒烟：[OK] rusqlite ... [OK] blake3
cargo test                                   # insta 快照 + proptest 性质
cp <repo>/deny.toml . && cargo deny check licenses   # 仅 Zlib / BSD-2-Clause 被拒
```
