# 研究记录：platform（配置/密钥 · SQLite · 资产库 · 任务队列 · 日志/审计/索引 · 备份/时钟）

> 状态：已完成（T07–T12）；v2 扩展（task-37）见 §11、v3 扩展（task-42）见 §12、v4 重做栈（task-66）见 §13 · 2026-11-09 · 对应规范：`SPEC-platform.md` v0.2.1（§2/§4/§5/§6/§7）、`CONFIG.md` v0.2.3、`SPEC-domain.md` v0.2.2、`CAPABILITY-MAP.md` §2/§3/§5、`AGENTS.md`
> 实测环境：Apple M4 / macOS 27.0.1 / rustc 1.99.1 / bundled SQLite 3.54.1
> 结论用于：`crates/eg-platform/Cargo.toml` 依赖、`src/*` 实现、migrations/0001_initial.sql；上游依赖与许可见 `deps.md`。

## 1. 模块与范围

- 模块 id：`platform`（crate `eg-platform`）。
- 本次实现：T07 配置与密钥、T08 SQLite 迁移与仓储、T09 资产库与单实例、T10 任务队列与取消、T11 日志/审计/诊断/索引、T12 备份/恢复/时钟/ULID。
- 明确不做：任何业务判分/识别逻辑；`inference` 的授权语义（本模块只持久化 `consent_id` 与范围哈希）；账号/登录；原图清理（无删除 API）。

## 2. 问题清单

| # | 需要确认的外部行为/API/版本/限制 | 影响 |
|---|---|---|
| Q1 | `rusqlite 0.41` 的 bundled SQLite 是否含 FTS5？中文子串检索如何实现？ | 日志全文索引（T11） |
| Q2 | `keyring 4.2` 的 macOS Keychain API 形态（feature/方法名）？测试能否不依赖真实钥匙串？ | 密钥存取（T07） |
| Q3 | 文件锁 crate 选择与语义（第二实例 1s 内失败）？ | 单实例（T09） |
| Q4 | `zip 9.2.2` 的写/读 API（诊断包）？ | 诊断包（T11） |
| Q5 | `chrono 0.4.45` 的时间运算 API（背退避、保留期、租约）？ | 任务退避/日志保留（T10/T11） |
| Q6 | `ulid 3` 的单调性与跨重启保证？ | ULID 生成器（T12） |
| Q7 | 单写连接 actor 的事务边界与只读连接可行性（WAL 下）？ | SQLite（T08） |
| Q8 | 日志实现是否必须引入 `tracing`（`SPEC.md` §4 提到 `tracing`）？ | 日志（T11） |
| Q9 | 配置键映射与默认值如何保证与 `CONFIG.md` 0 偏差？ | 配置（T07） |

## 3. 来源

| # | URL | 文档版本/日期 | 类型 | 取用内容 |
|---|---|---|---|---|
| S1 | https://docs.rs/rusqlite/0.41.1/ | 0.41.1（2026-10-28） | 官方 API 文档 | `Connection`/`Transaction`/`params!`/`OptionalExtension`/`OpenFlags`/`ErrorCode` 语义 |
| S2 | https://www.sqlite.org/fts5.html | SQLite 3.54.1 文档（2026-11 查阅） | 官方文档 | FTS5 虚拟表、`tokenize='trigram'`（≥3.34）、`MATCH` 短语与转义、`UNINDEXED` 列 |
| S3 | https://docs.rs/keyring/4.2.0/keyring/v1/struct.Entry.html | 4.2.0（2026-08-29） | 官方 API 文档 | `Entry::new(service, user)`、`set_password`、`get_password`、`delete_credential`、`Error::NoEntry` |
| S4 | https://docs.rs/fs2/0.4.3/fs2/trait.FileExt.html | 0.4.3 | 官方 API 文档 | `try_lock_exclusive`/`unlock` 非阻塞文件锁 |
| S5 | https://docs.rs/zip/9.2.2/zip/read/struct.ZipArchive.html | 9.2.2 | 官方 API 文档 | 9.x 中 `by_index` 返回 `ZipResult<ZipFile>`，`name()` 返回 `ZipResult<Cow<str>>`（与 2.x 不同，实测踩坑） |
| S6 | https://docs.rs/chrono/0.4.45/chrono/struct.TimeDelta.html | 0.4.45 | 官方 API 文档 | 构造用 `Duration::milliseconds(i64)`/`days(i64)`（无 `from_millis`） |
| S7 | https://docs.rs/ulid/3.0.0/ulid/ | 3.0.0 | 官方 API 文档 | `Ulid::from_parts(ms, random)`；crate 不提供跨重启单调，需自行持久化 |
| S8 | 本机 `cargo add` 实测（Cargo.lock 锁定） | 2026-11-02 | 命令输出 | rusqlite 0.41.1、keyring 4.2.0、fs2 0.4.3、zip 9.2.2、serde 1.0.229、serde_json 1.0.154、toml 1.1.8、chrono 0.4.45、ulid 3.0.0、blake3 1.8.7、thiserror 2.0.21、tempfile 3.27.0、insta 1.49.0、proptest 1.12.0 |
| S9 | `docs/dev/research/deps.md`（技术选型研究） | 2026-11-02 | 项目内研究记录 | bundled SQLite=3.54.1、WAL/外键/busy_timeout 实测、keyring 4.x 默认 `v1` 特性无 `apple-native` |
| S10 | 本机 `cargo test -p eg-platform` / `cargo clippy -D warnings` / `cargo fmt --check` | 2026-11-02 | 命令输出 | 128 个测试全绿；clippy/fmt 通过 |

## 4. 结论

| 问题 | 结论 | 依据 |
|---|---|---|
| Q1 | bundled SQLite 含 FTS5（实测可 `CREATE VIRTUAL TABLE ... USING fts5(...)`）。中文子串检索：**`tokenize='trigram'`**（≥3 字符可用）；≤2 字符关键词回退 `LIKE`（仍只查脱敏索引文本）。 | S1/S2 + 实测 |
| Q2 | `keyring = "4"` 默认特性（`v1`）即可：`Entry::new(service, user)` → `get_password`（`NoEntry` 视为「缺失」而非错误）、`set_password`、`delete_credential`。**测试不触碰真实钥匙串**：`SecretStore` trait + `InMemorySecretStore`，Keychain 实现只做编译与接口测试（真实读写需登录钥匙串已解锁，属人工验收）。 | S3/S9 |
| Q3 | `fs2 0.4` 的 `try_lock_exclusive`（非阻塞）满足「第二实例 1s 内失败」；锁文件内容写 pid/启动时间/版本，可给出中文提示。 | S4 + 实测 |
| Q4 | zip 9.x API 与 2.x 不同：`by_index()` 返回 `Result`、`name()` 返回 `Result<Cow<str>>`；实现按 9.x 写并在测试中实读校验。 | S5 + 实测 |
| Q5 | 时间运算用 `chrono::Duration::milliseconds(i64)`/`days(i64)`（**没有** `from_millis`）；租约 30s、心跳 5s、退避 500ms×2^n 抖动封顶 30s 均以毫秒整数实现。 | S6 |
| Q6 | `ulid` crate 只提供 `from_parts`/随机生成；本项目自建 `UlidFactory`：`最近毫秒 + 最近 ULID` 持久化到 `ulid.state`，保证同毫秒严格递增、跨重启与时钟回拨不倒退（10000 次同毫秒与回拨测试通过）。 | S7 + 实测 |
| Q7 | 单写连接 = 专属线程 + 有界 `sync_channel(64)` 命令队列（满则反压调用方）；写闭包在事务中运行，`Err` 即回滚。只读连接用 `SQLITE_OPEN_READ_ONLY | NO_MUTEX` + `PRAGMA query_only=ON`，WAL 下读写并存实测可用。 | S1 + 实测 |
| Q8 | **偏差**：`SPEC.md` §4 技术栈写「`tracing` 结构化日志（JSONL）」。本实现用自研 JSONL writer 而非 `tracing`，原因：需要在注入时钟下确定性地测「按天/按 50MB 轮转、保留期清理、错误级同步落盘」，`tracing-appender` 的轮转基于系统时间且不暴露注入时钟。输出契约（JSONL 字段、级别、脱敏、索引）与规范 §6 一致。**记录为待裁定偏差**（见 §7），不改变任何验收行为。 | `deps.md` Q3 + 实测取舍 |
| Q9 | 配置结构逐键映射 `CONFIG.md`（含 §9.5/§10/§11 补充键），容器级 `#[serde(default)]` 实现「缺键补齐」；`Unknown keys` 用「默认配置序列化结果」作 schema 递归比对拒绝；值域/交叉约束集中在 `AppConfig::validate`；默认值用 `defaults_match_config_md` 逐项断言 + insta JSON 快照双保险。 | 实测（`config.rs` 测试） |

## 5. 版本实测与实现地图

| 依赖/工具 | 实测版本 | 用途 |
|---|---|---|
| rusqlite（bundled） | 0.41.1 / SQLite 3.54.1 | 业务库、迁移、FTS5 索引 |
| keyring | 4.2.0（默认 `v1` 特性） | macOS Keychain 密钥存取；`v1` 在 Linux 目标会带入 zbus Secret Service 后端（见 §6.1） |
| fs2 | 0.4.3 | 单实例文件锁 |
| zip | 9.2.2（`default-features = false, features = ["deflate-flate2-zlib-rs"]`） | 诊断包；显式关闭默认压缩后端，避免 bzip2/ppmd 许可与多余依赖（见 §6.1） |
| serde / serde_json / toml | 1.0.229 / 1.0.154 / 1.1.8 | 配置、JSON 落库、诊断包 |
| chrono | 0.4.45 | 时间戳/RFC3339/租约与保留期运算 |
| ulid | 3.0.0 | ULID 基础类型（单调与持久化自建） |
| blake3 | 1.8.7 | 资产 id、审计链、幂等键 |
| thiserror | 2.0.21 | 类型化错误 |
| tempfile / insta / proptest | 3.27.0 / 1.49.0 / 1.12.0 | 测试 |

实现文件（`crates/eg-platform/src/`，约 1.3 万行含测试）：

| 文件 | 内容 |
|---|---|
| `config.rs` | `AppConfig`（CONFIG 全部键 + 默认值 + 校验）、`ConfigStore`（原子写 + `.bak` + 损坏隔离 + 恢复默认）、`meta` |
| `secrets.rs` | `SecretRef`（`cloud/<backend>/api_key`）、`SecretStore` trait、Keychain/内存实现、脱敏 `Debug` |
| `error.rs` / `paths.rs` / `clock.rs` / `ids.rs` | 错误分类（5 档）、数据根布局、`SystemClock`/`FixedClock`、单调 ULID 工厂 |
| `db.rs` / `schema.rs` / `repo.rs` / `migrations/0001_initial.sql` / `migrations/0002_rubric_payload_consent_audit.sql` | 单写连接 actor、只读连接、迁移前快照、`schema_version`（v2）、23 张表与索引、类型化仓储（版本化 JSON body + 冗余列）；v2 追加规则草稿负载/生效规则按题索引/授权额度/审计扩展列（见 §11） |
| `assets.rs` / `instance.rs` | BLAKE3 内容寻址、去重、只读文件、软配额告警、无删除 API；单实例锁 |
| `tasks.rs` / `cancel.rs` | 状态机、幂等键、租约/心跳、退避、断点阶段、启动恢复、审计联动；协作式取消 |
| `logging.rs` / `index.rs` | JSONL 日志（级别覆盖/脱敏/轮转/保留/查询）、FTS5 全文索引（增量/按段校验/整体重建/关闭降级） |
| `audit.rs` | 只追加审计、BLAKE3 哈希链、`business_write`（审计失败回滚业务写） |
| `diagnostics.rs` / `backup.rs` / `consent.rs` / `platform.rs` | 诊断包（zip，脱敏）、备份/恢复/代数清理、任务级授权记录、组合入口 |

## 6. 风险与限制

| 风险 | 影响 | 缓解/预案 |
|---|---|---|
| R1：Keychain 真实读写未在自动测试中执行（需登录钥匙串解锁与 GUI 授权） | 首次真实使用云端后端时可能遇到权限问题 | `SecretStore` 抽象 + 内存实现覆盖逻辑；Keychain 实现按官方 API 编译验证；建议人工冒烟一次 |
| R2：崩溃恢复用「租约到期 + 重启 recover」模拟，未真跑 `SIGKILL` 子进程 | 与真实强杀路径略有差异 | 逻辑上等价（强杀后无心跳、租约过期即归位）；CP-B 人工验收时可补一次真实强杀 |
| R3：日志未用 `tracing`（见 §4 Q8） | 与 `SPEC.md` §4 技术栈表述不一致 | 记录为偏差待裁定；输出契约与验收行为不受影响 |
| R4：`task_consent.revoke` 以状态更新实现（`active → revoked`），而非新增一条记录 | 与「撤销写新记录、不修改历史」的字面要求有差异 | 审计链记录撤销动作；历史 `granted_at` 保留；如需严格 append-only 需 schema 追加 `consent_event` 表（Ask first） |
| R5：FTS5 中文 ≤2 字关键词走 `LIKE` 回退 | 大索引下短词检索 O(n) | 仅影响短关键词；UI 引导 ≥3 字；容量超限时按规范降级为字段过滤 |
| R6：日志队列满时丢弃低级别（计数告警） | 极端负载下可能丢 info/debug | 错误级同步落盘（必达）；`dropped_total` 计数可观测 |
| R7：`busy_timeout=5s` 下多进程并发写同一库会串行化 | 单实例约束下不构成问题 | 单实例锁保证只有一个进程 |
| R8：诊断包含 `data_root` 路径（家目录名可能出现在路径中） | 轻度路径信息外泄 | 导出配置/日志文本已脱敏家目录；`environment.json` 的 `data_root` 建议人工确认后再外发 |

### 6.1 依赖公告与许可处置（2026-11-02，task-18）

**A. `zip` 许可问题（已在本模块内解决）**

`zip 9.2.2` 默认特性会拉入 `bzip2` → `libbz2-rs-sys`（许可 `bzip2-1.0.6`）与 `ppmd` → `ppmd-rust`（许可 `CC0-1.0 OR MIT-0`），两者都不在 `deny.toml` 白名单，`cargo deny check licenses` 直接失败。本模块只用 zip 做诊断包的写入与读取测试，deflate 足够，因此改为：

`zip = { version = "9", default-features = false, features = ["deflate-flate2-zlib-rs"] }`

结果：依赖图中 `bzip2`/`libbz2-rs-sys`/`ppmd-rust`/`zopfli`/`lzma`/`zstd`/`xz` 全部消失，`licenses` 恢复通过；`zip` 的 API 未变，`diagnostics` 读写测试全绿。若将来需要读取 bzip2/ppmd 压缩的 zip，应改为在 `deny.toml` 追加许可并重新评估（当前规范无此需求）。

**B. keyring → Linux Secret Service 的 3 条 unmaintained 公告（已在 `deny.toml` 按 id 忽略）**

链路（`cargo tree` 实测）：

eg-platform → `keyring 4.2.0`（`v1` 特性）→ `zbus-secret-service-keyring-store 1.0.1` → `secret-service 5.2.0` → `zbus 5.19.0` → `zvariant 5.15.1` / `zbus_names 4.3.5` / `zvariant_derive 5.15.1`。

| 公告 | 包 | 类型 | 结论 |
|---|---|---|---|
| RUSTSEC-2026-0370 | `zvariant 5.15.1` | unmaintained | 上游 2026-08 将 zvariant 并入 zbus 6.0.0（issue [z-galaxy/zbus#2043](https://github.com/z-galaxy/zbus/issues/2043)，已关闭），5.x 不再单独维护；公告标注 “No safe upgrade is available!”，无 CVE。 |
| RUSTSEC-2026-0371 | `zbus_names 4.3.5` | unmaintained | 同上，随 zvariant 合并。 |
| RUSTSEC-2026-0372 | `zvariant_derive 5.15.1` | unmaintained | 同上，派生宏并入 zbus 6。 |

**为什么不影响发行包**：`keyring 4.2.0` 的 `v1` 特性按 `cfg` 选择各平台后端——macOS/iOS 走 `apple-native-keyring-store/keychain`，Windows 走 `windows-native-keyring-store`，只有「非 macOS/iOS/Android 的 unix」才编译 `zbus-secret-service-keyring-store`。本项目发行目标是 macOS（`SPEC.md` §11 环境），实际构建不含 zbus 分支，因此这 3 条公告对交付物没有运行时影响；`secret-service`/zbus 代码也不会进入 macOS 二进制。

**为什么用 ignore 而不是 filter**：`deny.toml` 的 `[graph]` 有意不限制 target（要让跨平台许可/来源检查也覆盖），所以不能靠 target 过滤消掉，只能按 `deny.toml` 既定的「确有不可避免的公告时按 id 忽略并写明 reason」机制处理。`ignore` 只精确列出这 3 个 id，未放宽其他 advisories 策略（未整体 ignore、未降级 unmaintained 检查），也未改 licenses 白名单。

**复跑结果**（2026-11-02）：`cargo deny check` → `advisories ok, bans ok, licenses ok, sources ok`；`cargo test -p eg-platform` 128 passed。

**后续条件**：若将来真正支持 Linux 桌面，必须重新评估——优先等 `keyring` 迁移到 zbus 6 系列，或改用其他 Secret Service 后端；在此之前不得删除 `deny.toml` 中的这 3 条 ignore，也不得把 Linux 纳入发行目标。上游公告页：<https://rustsec.org/advisories/RUSTSEC-2026-0370>、<https://rustsec.org/advisories/RUSTSEC-2026-0371>、<https://rustsec.org/advisories/RUSTSEC-2026-0372>。

## 7. 与规范的偏差

| 规范位置 | 偏差 | 处理 | 审批状态 |
|---|---|---|---|
| `SPEC.md` §4「`tracing` 结构化日志」 | 自研 JSONL writer（见 §4 Q8） | 待负责人裁定：保留自研（测试确定性）或后续替换为 `tracing` 订阅层；行为契约不变 | **待裁定** |
| `SPEC-platform.md` §4.4「撤销写新记录，不修改历史」 | `task_consent` 状态更新 + 审计留痕（R4） | 记录为已知偏差；如需严格追加式，加 `consent_event` 表（属 Ask first 的 schema 变更） | **待裁定** |
| `SPEC-platform.md` §6「诊断包 ≤10 秒 / 关键字检索 P95 ≤200ms」 | 本机测试未做性能计时（仅功能正确性 + 条数上限） | CP-B/性能任务补测；当前实现为单机小数据量设计 | 待补测 |
| `CONFIG.md` §9.5 补充键 | 全部按键名映射；`log.index_rebuild` 实现为三布尔策略结构（启动校验/手动/ schema 变更） | 语义一致，存储形态为结构体 | 已记录 |
| `SPEC-platform.md` §10 开放问题 2（内部键补登） | `storage.*`、`backup.*`、`inference.*` 已在 CONFIG §9.5 登记 | 无偏差 | — |

## 8. 验收对照（T07–T12）

| 任务 | 验收点 | 实现与测试 |
|---|---|---|
| T07 | CONFIG 默认值 0 偏差；非法值中文拒绝；原子写 + `.bak`；密钥不入库/日志 | `config::tests::defaults_match_config_md` 逐项断言 + insta 快照；`invalid_value_rejected_with_chinese_message`；`backup_kept_and_restorable`；`secrets` 脱敏 + `written_logs_contain_no_pii` |
| T08 | 迁移 v0→v1；`PRAGMA integrity_check`；事务边界；23 表 CRUD | `db::tests::*`（迁移/回滚/外键/WAL/快照）、`repo::tests::*`（班级/学生/试卷/提交/区域/识别/判分/资产/答案键/规则原文） |
| T09 | 同字节 40 次 → 1 文件 1 行；只读；软配额告警；第二实例 1s 内退出 | `assets::tests::same_bytes_40_times_is_one_file_one_row`、`files_are_read_only_after_write`、`soft_quota_warning_without_deleting`；`instance::tests::second_instance_fails_fast_with_pid_hint` |
| T10 | 状态机全迁移、幂等键、租约/心跳、取消、退避、启动恢复 | `tasks::tests::state_machine_transition_table`、`enqueue_is_idempotent_by_key`、`recover_expired_requeues_running_tasks`、`transient_failure_backs_off_then_fails_after_max_attempts`、`cancel_pending_and_running_but_not_terminal`；`cancel::tests::*` |
| T11 | 模块级级别、脱敏、JSONL 字段、轮转、保留 30 天、索引增量/重建、审计链与篡改检出、诊断包脱敏 | `logging::tests::*`（级别覆盖/脱敏/轮转/保留/查询/索引）、`index::tests::*`（增量/重建/schema 变更/独立性）、`audit::tests::*`（链/篡改/审计失败回滚）、`diagnostics::tests::*` |
| T12 | 迁移前/手动备份、恢复后校验、System/FixedClock、ULID 单调与回拨 | `backup::tests::*`（清单/恢复/安全副本/代数清理）、`clock::tests::*`、`ids::tests::*`（同毫秒 1 万次、回拨、跨重启持久化） |
| task-37（v2） | 只追加迁移 v1→v2；旧行保留；失败迁移整体回滚；迁移前快照为可读 v1 且可再升级；草稿负载/生效规则按题查询/试卷级 CAS；授权额度用尽持久化；审计扩展字段且旧链可验证 | `schema::tests::v1_to_v2_migration_preserves_rows_and_fills_defaults`、`failed_migration_rolls_back_every_statement`；`db::tests::v1_to_v2_snapshot_is_readable_and_data_retained`；`repo::tests::stored_rubric_draft_roundtrip_and_listing`、`stored_rubric_drafts_by_paper_orders_by_question_ordinal`、`paper_rubric_version_cas_and_active_rubric_lookup`；`consent::tests::record_spend_accumulates_and_auto_exhausts`、`exhausted_persists_across_restart` 等；`audit::tests::extended_fields_roundtrip_and_chain_verifies`、`legacy_rows_written_before_v2_still_verify`、`tampered_extension_field_is_detected` |

**测试总数**：143 个单元测试 + 10 个 CP-B 集成测试（见 §10；v2 新增 15 个单元测试）；`cargo test -p eg-platform` 全绿；`cargo clippy -p eg-platform --all-targets --all-features -- -D warnings` 与 `cargo fmt -p eg-platform -- --check` 通过。

## 9. 复现命令

```bash
source "$HOME/.cargo/env"
cargo test -p eg-platform   # 含 10 个 CP-B 集成测试（Keychain 两项默认跳过，见 §10）
EXAM_GRADING_KEYCHAIN_SMOKE=1 cargo test -p eg-platform --test cp_b_keychain -- --nocapture --test-threads=1
cargo test -p eg-platform --test cp_b_sigkill -- --nocapture --test-threads=1
cargo test -p eg-platform --test cp_b_perf_backup -- --nocapture --test-threads=1
cargo clippy -p eg-platform --all-targets --all-features -- -D warnings
cargo fmt -p eg-platform -- --check
cargo deny check
# v2 迁移/仓储/授权/审计定向回归
cargo test -p eg-platform --lib schema::tests::v1_to_v2
cargo test -p eg-platform --lib schema::tests::failed_migration
cargo test -p eg-platform --lib db::tests::v1_to_v2_snapshot
cargo test -p eg-platform --lib repo::tests::paper_rubric_version_cas
cargo test -p eg-platform --lib consent::tests::exhausted
cargo test -p eg-platform --lib audit::tests::legacy_rows_written_before_v2
```

> 备注 1：`cargo deny check` 的 `Zlib`/`BSD-2-Clause` 等许可为 `zip`/`rusqlite` 依赖链所需，已在 `deny.toml` 白名单（task-21 决策）。
>
> 备注 2：`advisories` 中的 `RUSTSEC-2026-0370/0371/0372` 为本模块 keyring 依赖链的 Linux-only 处置，理由与复跑结果见 §6.1；`zip` 已关闭默认压缩后端以避免许可问题。

## 10. CP-B 验收记录（2026-11-02）

> 目的：补齐 §6 的 R1（Keychain 真实读写）、R2（真实 SIGKILL 恢复）、R3（性能未测）
> 与「备份/恢复端到端」。**所有数字均为本机实测**（非估算），测量代码即
> `crates/eg-platform/tests/cp_b_*.rs`，无编造数据。
>
> 环境：macOS 27.0.1 (26A434) / Apple M4 / 16 GB / arm64；
> rustc 1.99.1（2026-10-13）/ cargo 1.99.1；rusqlite 0.41.1 内置 SQLite 3.54.1；
> 数据目录与时钟均由测试注入（`DataDirs::with_root` + `FixedClock`）。

### 10.1 R1：真实 Keychain 往返 + 密钥不泄漏（达标）

命令（默认跳过，需登录钥匙串；本次实测已执行）：

```bash
EXAM_GRADING_KEYCHAIN_SMOKE=1 cargo test -p eg-platform --test cp_b_keychain -- --nocapture --test-threads=1
```

实测结果（`tests/cp_b_keychain.rs`，2 个测试通过）：

| 步骤 | 结果 |
|---|---|
| `KeychainSecretStore::set` → `exists` → `get`（服务名 `exam-grading-smoke-<pid>`，账户 `cloud/smoke/api_key`） | 往返一致；证明 `keyring 4.2.0` → macOS Keychain Services 真实可用 |
| `security find-generic-password -s <service> -a <account>`（独立工具、属性查询） | 能查到条目 → 确认密钥真的落在登录钥匙串（非内存实现） |
| 覆盖写后再读 | 读到新值 |
| `delete` 两次（幂等）后再 `get` | `Ok(None)`；`security` 再查 → `item could not be found`（退出码 44） |
| 泄漏扫描：数据根全部文件（sqlite/WAL、JSONL 日志、config.toml、ulid.state、备份等） | 10 个文件，密钥明文 **0 命中** |
| 泄漏扫描：诊断包 zip 解压后的全部条目 | 11 个条目 / 约 3.8 KB（两次运行 3 783、3 785 字节），密钥明文 **0 命中** |

另用 `security` CLI 做了完全独立于本 crate 的往返验证（`add-generic-password` →
`find-generic-password -w` → `delete-generic-password` 均成功，登录钥匙串已解锁）。

**已知限制（非阻塞）**：
- `security find-generic-password -w` 读取「由本测试二进制写入的条目」会触发 macOS 授权框；
  无头环境无法自动点「允许」，因此自动化里只做属性查询交叉验证，明文交叉验证默认跳过；
  需要时设 `EXAM_GRADING_KEYCHAIN_SECURITY_READ=1` 并在桌面点一次「允许」。
  该行为是 macOS ACL 的预期安全设计，不是缺陷。
- 无登录态/无 GUI 的 CI 上两项 Keychain 测试会自动跳过（打印 `[skip]`），不影响其余测试。

### 10.2 R2：真实 SIGKILL 后恢复（达标）

命令：

```bash
cargo test -p eg-platform --test cp_b_sigkill -- --nocapture --test-threads=1
```

做法：父进程建任务 → 以子进程方式重跑同一测试二进制（`--exact sigkill_worker`），
子进程 `Platform::open_with` 认领任务并打印 `READY` → 父进程 `Child::kill()`（Unix 即
`kill -9`，实测 `signal=9`）→ 时钟 +31s 后重新 `open_with`（启动时 `recover_expired`）。

| 断言 | 实测 |
|---|---|
| 进程确实被强杀 | `ExitStatus::signal() == Some(9)` |
| 任务归位 | `running → pending`；`lease_owner=None`、`lease_expires_at=None`、`attempts` 保留=1 |
| 归位原因 | `last_error = "租约过期（进程崩溃/强杀）"` |
| 幂等键不重复执行 | 同 `NewTask` 再次入队：`inserted=false`，`task_id` 与首次相同（无重复行） |
| 恢复后可继续 | 退避到期再 `claim` 成功：同一 task、`attempts=2`、`state=running` |
| 审计链完整性 | `verify_chain().ok == true`；动作含 `task.enqueue`、`task.recover` |

### 10.3 R3：性能补测（全部达标）

命令：`cargo test -p eg-platform --test cp_b_perf_backup -- --nocapture --test-threads=1`
（计时均 `std::time::Instant`；启动/去重为单次测量，检索 P95 为 100 次样本）

| 指标 | 目标 | 样本量 | 实测 | 结论 |
|---|---|---|---|---|
| 迁移 23 表 | 无规范数值（合理性 <10s） | 空库 v0→v1 | **3.1 ms**（两次运行均 3.1 ms，`integrity_check` 通过） | 达标 |
| 平台冷启动（配置/迁移/日志/索引/单实例锁/启动恢复） | 无规范数值 | 空数据根 | **22.9 / 35.3 ms**（两次运行） | 达标 |
| 平台热启动 | 无规范数值 | 已有库+配置+索引 | **7.8 / 8.2 ms**（两次运行） | 达标 |
| 资产去重 40 次 × 1 MiB | 无规范数值 | 40 次同字节写入 | **520.7 ms**（`entries=1`、`total_bytes=1048576`、39 次命中去重） | 达标 |
| 诊断包导出 | **≤10 s**（`SPEC-platform.md` §6） | 2 000 条日志 + 100 任务 + 100 审计 + 10 资产；包 11 条目 / 10 159 字节 | **51.1 ms**（另一次 52.0 ms） | 达标（余量约 190×） |
| FTS5 关键字检索 P95 | **≤200 ms**（`SPEC-platform.md` §6） | 索引 10 000 条；关键词「识别失败」检索 100 次、limit 20 | **P50 = 0.1 ms，P95 = 0.2 ms，max = 0.2 ms**（建索引总耗时 3.7 s，单条写入 P95 0.31 ms） | 达标（余量约 1000×） |
| 短关键词 LIKE 回退（对照） | 无目标（仅记录） | 索引 10 000 条；2 字关键词「失败」检索 20 次 | P95 = 0.1 ms | 记录 |

说明：性能目标只对「诊断包 ≤10s」「检索 P95 ≤200ms」有规范数值，其余为交付前基线数据；
数据量为单机教师场景的典型量级（万级日志），更大规模（如 10 万条日志）未纳入本次验收。

### 10.4 备份/恢复端到端 + 缺陷 D1（已修复）

命令：`cargo test -p eg-platform --test cp_b_perf_backup backup_restore_verify_end_to_end -- --nocapture`

流程与结果：建 1 班级 + 3 任务 + 资产 → 手动备份（清单：classroom=1、task=3、asset=1）→
删班级 + 新增 1 任务（模拟误操作）→ 关闭数据库 → `restore`（自动保留安全副本）→ 重开平台 →
`verify_restored`：`integrity_ok=true`、`schema_version=1`、**表计数与备份清单完全一致**、
`assets_checked = 清单 asset_count`、`asset_failures=0`；数据层：班级恢复、备份后新增任务消失、
备份时刻任务集合恢复、审计链完整。

**缺陷 D1（CP-B 验收发现，已修复）**

| 项 | 内容 |
|---|---|
| 现象 | `Platform::close()` 后 `restore` 覆盖数据库，再 `Platform::open_with` 报 `DatabaseCorrupt: database disk image is malformed`（SQLITE_CORRUPT，extended_code 11）。 |
| 触发条件 | 目标库旁仍残留旧库的 `-wal`/`-shm` 侧车文件。实测：正常 `Platform::close()` 后两者仍存在（测试断言 `exists=true`）；强杀后必然残留。 |
| 根因 | `BackupService::restore` 只 `fs::copy` 主库文件，未清理旧库侧车；SQLite 打开时会重放同目录 `-wal` 的旧事务帧到新快照上 ⇒ 镜像损坏。SQLite 官方 WAL 文档明确：“If a database file is separated from its WAL file, then transactions that were previously committed to the database might be lost, or the database file might become corrupted.”（<https://sqlite.org/wal.html>） |
| 修复 | `crates/eg-platform/src/backup.rs` `restore`：复制快照到目标库后，删除目标库的 `-wal`/`-shm`；`NotFound` 视为已清理，其余删除失败显式返回 `PlatformError::Io`（不静默）；安全副本/取证逻辑不变，对外 API 不变。 |
| 回归测试 | `tests/cp_b_perf_backup.rs::backup_restore_verify_end_to_end`（前置断言 close 后 WAL 存在 → restore 后 sidecar 不存在 → integrity/表计数/资产/数据回归）；`tests/cp_b_sigkill.rs::restore_after_sigkill_with_stale_wal`（强杀后 WAL 残留 → 不先打开平台、直接 restore → 无 malformed、任务回 `pending`/`attempts=0`、表计数=清单、审计链完整）。 |
| 结论 | 修复后两个回归测试均通过；属真实缺陷修复（非规范偏差、非性能未达标），已记录为 CP-B 验收产出。 |

### 10.5 结论与遗留

- **R1/R2/R3 关闭**：Keychain 真实读写与密钥不泄漏、真实 SIGKILL 恢复与幂等、性能指标
  （诊断包 51.1 ms ≤10 s；检索 P95 0.2 ms ≤200 ms）均有实测证据；备份/恢复端到端通过，
  并修复了验收中发现的 D1（restore 未清理旧库 WAL/SHM）。
- **本次新增/修改**：`crates/eg-platform/tests/cp_b_keychain.rs`、
  `tests/cp_b_sigkill.rs`、`tests/cp_b_perf_backup.rs`；`src/backup.rs`（D1 修复）；
  本文件 §6.1、§8、§9、§10。
- **遗留（不阻塞 CP-B，建议后续补）**：
  1) 真实云端调用 + 真实 Keychain 授权框的一次人工冒烟（需教师桌面点击「允许」，
     自动化环境无法替代）；
  2) 更大数据量性能（10 万条日志/更大资产库）与多次重复测量的统计口径；
  3) 诊断包导出仍在应用线程同步执行（51 ms 级）；若将来日志量增长到 10 万级，
     建议改为后台任务并保留 ≤10 s 预算（本次未改设计）。

## 11. schema v2：规则草稿负载 / 生效规则按题 / 授权额度用尽 / 审计扩展（2026-11-03，task-37）

> 目的：T18 交付的 v1（23 表）在 rubric / inference / app 接入时暴露出四处缺口，
> 本任务以**只追加迁移**补齐，不改 `0001_initial.sql`、不新增表、不 DROP/RENAME。
> 迁移文件：`crates/eg-platform/migrations/0002_rubric_payload_consent_audit.sql`；
> `schema::SCHEMA_VERSION = 2`；`schema::MIGRATIONS` 末尾追加 v2（每个迁移独立事务，
> 失败只回滚该迁移且 `app_meta.schema_version` 保持 1）。

### 11.1 缺口与迁移映射

| 缺口 | 影响 | v2 处置 |
|---|---|---|
| `eg_domain::RubricDraft` 不承载题型/满分/计分方式/部分给分档位/模板引用 | 草稿确认与静态校验缺输入，无法从库完整重建 `eg-rubric::StoredDraft` | `rubric_draft` 追加 7 个负载列（`body` 仍是领域权威） |
| 生效规则只能按 `(paper_id, rubric_version)` 读写，无法按题查询 | `RubricStore::active_rubric(question_id)` 无法实现 | `rubric_revision` 追加可空 `question_id` + 按题索引 |
| 授权只有 active/ended/revoked，预算耗尽只能进程内记着 | 重启后「额度用尽」丢失，可能继续外发 | `task_consent` 追加 `spent_micros`/`exhausted_at` + 新状态值 `exhausted` |
| 审计没有规则版本/模板/提示词/模型调用字段 | 规则确认与模型整理无法按规范审计复现 | `audit_log` 追加 6 个可空扩展列 |

### 11.2 变更清单（只追加）

| 表 | 新增列 | 新增索引 | 旧行默认值 |
|---|---|---|---|
| `rubric_draft` | `question_kind TEXT NOT NULL DEFAULT ''`、`max_score_milli INTEGER NOT NULL DEFAULT 0`、`scoring_mode TEXT`、`partial_choice TEXT`、`template_id TEXT`、`template_version INTEGER`、`template_ref TEXT` | `idx_rubric_draft_question_created(question_id, created_at)`、`idx_rubric_draft_status_created(status, created_at)` | `''` / `0` / `NULL` |
| `rubric_revision` | `question_id TEXT REFERENCES question(id)` | `idx_rubric_revision_question(question_id, rubric_version DESC)` | `NULL`（v1 行只读保留） |
| `task_consent` | `spent_micros INTEGER NOT NULL DEFAULT 0`、`exhausted_at TEXT`；`state` 新增取值 `'exhausted'` | `idx_consent_task_spent(task_id, spent_micros)` | `0` / `NULL` / `active` 不变 |
| `audit_log` | `rubric_version INTEGER`、`template_id TEXT`、`template_version INTEGER`、`template_ref TEXT`、`prompt_hash TEXT`、`model_call_id TEXT` | `idx_audit_rubric(entity_type, entity_id, rubric_version)`、`idx_audit_model_call(model_call_id)` | `NULL` |

表数仍为 23（`schema::TABLES` 不变）；`rubric_draft`/`task_consent`/`audit_log` 的数据行不迁移、
不改写，仅补列。新增 `question_id` 外键在 `PRAGMA foreign_keys=ON` 下立即生效（测试覆盖）。

### 11.3 读写语义

**规则草稿（`repo::StoredRubricDraft`）**

- `body` 仍是 `RubricDraft` 的版本化 JSON（权威）；v2 负载列是冗余，服务查询与确认输入。
- 载荷结构：`RubricDraftPayload { question_kind, max_score, mode, partial_choice,
  template_id, template_version, template }`；`template` 为 `eg-rubric::TemplateRef`
  的 serde JSON（platform 不解析内部结构，只冗余 `template_id`/`template_version` 便于审计）。
- 读取时 `question_id`/`created_at` 以**列**为权威（覆盖 `body` 内的同名字段），
  保证 upsert 不改身份字段；v1 旧行负载列全为 `None`（`insert_rubric_draft` 写入的行）。
- 写入提供两条路径：`insert_stored_rubric_draft`（新草稿，主键冲突即拒绝）与
  `upsert_stored_rubric_draft`（同 id 覆盖状态/校验结果/负载，对应
  `RubricStore::save_draft` 的「同 id 覆盖为最新状态」；`created_at`/`question_id` 不变）。

**生效规则（试卷级版本 + 题目内容）**

- 版本基准是 `paper.rubric_version`（与 `SPEC-rubric.md` §2.5「以 `Paper.rubric_version`
  为基准」一致）；每次确认在**同一事务**内 CAS 递增试卷版本，并写一条带 `question_id`
  的 `rubric_revision` 快照（该题在该版本下的规则内容）。
- `get_active_rubric(question_id)` 取该题**最近**的 `rubric_revision`（按 `rubric_version`
  降序）即现行生效规则；`paper_rubric_version(question_id)` 返回题目所属试卷的当前版本，
  作为 `RubricStore::rubric_version` / CAS 期望值。
- CAS 失败返回 `PlatformError::VersionConflict { expected, actual }`（新增错误变体，
  `ErrorClass::Permanent`），适配层映射为 `RubricError::VersionConflict`。
- `advance_paper_rubric_version` 是低层 bump 原语（仅递增列 + 同步 `paper.body`）；
  `save_active_rubric` = 校验「版本只增一」+ bump + 写题目快照；审计由调用方在
  同一 `AuditLog::business_write` 事务内追加。
- `get_paper` 读取时用 `paper.rubric_version` 列覆盖 `body` 内的同名字段，避免上层看到旧版本。

**授权额度（`consent`）**

| 状态/操作 | 语义 |
|---|---|
| `active` | 唯一可用于云端调用的状态；`is_valid`/`validate_call` 只认它 |
| `ended` | 任务终态；`end_for_task` 只把 `active` 改为 `ended` |
| `revoked` | 教师撤销（历史保留） |
| `exhausted`（新增） | 预算耗尽；`mark_exhausted(task_id)` 幂等置位，`record_spend` 达到预算时自动置位；重启后仍拒绝 |
| `spent_micros` / `exhausted_at` | 累计花费（饱和累加）与用尽时间；`budget_micros = 0` 视为本地/无费用，永不置用尽 |
| `remaining_micros()` | `budget_micros - spent_micros`；预算为 0 时返回 `None` |
| `record_spend(consent_id, delta)` | 余额更新；`ended`/`revoked` 拒绝记账（`PolicyBlocked`） |
| `is_exhausted(task_id)` | 查询任务是否存在用尽记录（不受随后 ended 影响） |
| 兜底 | `find_active` 额外要求 `spent_micros < budget_micros`（预算 0 除外）；`validate_call` 对剩余为 0 的记录再拒绝一次，防进程崩溃留下「状态未翻但已超预算」 |

`end_for_task` **不覆盖** `exhausted`（保留用尽原因）；`revoke` 允许 `exhausted → revoked`。

**审计扩展字段与链兼容**

- `NewAuditEntry`/`AuditEntry` 新增 6 个可选字段，并加构造器：
  `with_rubric_version(u32)`、`with_template(TemplateId, u32)`、
  `with_template_ref(serde_json::Value)`、`with_prompt_hash(Sha256Hex)`、
  `with_model_call(ModelCallId)`；旧构造器行为不变。
- 链哈希规范化载荷对扩展字段使用 `#[serde(skip_serializing_if = "Option::is_none")]`：
  6 个字段全为 `None` 时序列化字节与 v1 **完全一致**，因此 v1 旧行仍能通过
  `verify_chain`；扩展字段一旦有值则进入链哈希（篡改可检出，测试覆盖）。
  不允许修改 v1 行的既有链算法/字段顺序。

### 11.4 对外 API（新增/变更）

```rust
// eg_platform::repo（均为 pub fn；tx 内调用，事务由调用方/Platform 保证）
pub struct RubricDraftPayload {
    pub question_kind: Option<QuestionKind>,
    pub max_score: Option<MilliScore>,
    pub mode: Option<ScoringMode>,
    pub partial_choice: Option<PartialChoiceRule>,
    pub template_id: Option<TemplateId>,
    pub template_version: Option<u32>,
    pub template: Option<serde_json::Value>,
}
pub struct StoredRubricDraft { pub draft: RubricDraft, pub payload: RubricDraftPayload }

insert_stored_rubric_draft(tx, &StoredRubricDraft) -> Result<()>
upsert_stored_rubric_draft(tx, &StoredRubricDraft) -> Result<()>
get_stored_rubric_draft(conn, RubricId) -> Result<Option<StoredRubricDraft>>
list_stored_rubric_drafts_by_question(conn, QuestionId) -> Result<Vec<StoredRubricDraft>>
list_stored_rubric_drafts_by_paper(conn, PaperId) -> Result<Vec<StoredRubricDraft>>

get_active_rubric(conn, QuestionId) -> Result<Option<Rubric>>
latest_question_rubric_version(conn, QuestionId) -> Result<u32>
paper_rubric_version(conn, QuestionId) -> Result<u32>          // 题目不存在 = NotFound
insert_question_rubric_revision(tx, QuestionId, PaperId, u32, &Rubric, Timestamp) -> Result<()>
advance_paper_rubric_version(tx, PaperId, expected: u32) -> Result<u32>
save_active_rubric(tx, QuestionId, &Rubric, expected: u32, Timestamp) -> Result<u32>

// eg_platform::consent
ConsentState::Exhausted（code = "exhausted"）
TaskConsentRecord { .., spent_micros: u64, exhausted_at: Option<Timestamp> }
mark_exhausted(task_id) -> Result<u64>
record_spend(consent_id, delta_micros) -> Result<TaskConsentRecord>
is_exhausted(task_id) -> Result<bool>
remaining_micros()/is_usable()（TaskConsentRecord 方法）

// eg_platform::error
PlatformError::VersionConflict { expected: u32, actual: u32 }   // Permanent

// eg_platform::audit（NewAuditEntry 构造器；AuditEntry 同名字段）
with_rubric_version / with_template / with_template_ref / with_prompt_hash / with_model_call
```

`lib.rs` 的既有 re-export 不变（`repo` 本来就按 `eg_platform::repo::*` 使用）；
`PlatformError` 经既有 `pub use error::...` 暴露新变体。

### 11.5 迁移/回滚/快照测试矩阵（本机实测全绿）

| 测试 | 断言 |
|---|---|
| `schema::tests::v1_to_v2_migration_preserves_rows_and_fills_defaults` | 手工建 v1 + 写入 classroom/paper/question/rubric_draft/rubric_revision/task/task_consent/audit_log 旧行 → `apply` 后版本 2；旧行字段与 `body` 不变；新列取默认值；7 个负载列与 `question_id` 存在；`question_id` 外键拒绝不存在的题目 |
| `schema::tests::failed_migration_rolls_back_every_statement` | 预置同名索引使 v2 的 `CREATE INDEX` 失败 → 返回带迁移名的错误；版本仍 1；`question_kind`/`spent_micros` 列不存在（ALTER 回滚）；旧行仍在；移除冲突后重试可升到 v2 |
| `db::tests::v1_to_v2_snapshot_is_readable_and_data_retained` | 已有 v1 文件库 → `Database::open_with(backup_dir)` 生成一份快照；迁移后旧数据在；快照为**可读 v1**（无 v2 列）、含旧数据；对快照再 `schema::apply` 可升级（恢复兼容） |
| `repo::tests::stored_rubric_draft_roundtrip_and_listing` | 负载（含部分给分档位/模板 JSON）往返；v1 旧行负载全 `None`；按题/按卷列出、顺序正确 |
| `repo::tests::stored_rubric_drafts_by_paper_orders_by_question_ordinal` | 先写第 2 题再写第 1 题，按卷列出仍按题号排序 |
| `repo::tests::upsert_stored_rubric_draft_updates_state_and_keeps_identity` | 同 id 覆盖为「已确认」；`created_at`/`question_id` 不被刷新；不新增行 |
| `repo::tests::paper_rubric_version_cas_and_active_rubric_lookup` | 0→1→2 CAS；冲突 `{expected:1, actual:2}`；非法版本被拒且不 bump；低层 bump 独立可用；仅 bump 不写规则；题目不存在显式 `NotFound`；`body` 与列同步 |
| `consent::tests::record_spend_accumulates_and_auto_exhausts` | 累计花费/剩余额度；达上限自动 `exhausted`；`is_valid` 拒绝、`validate_call` 策略拦截；计数含 `Exhausted`；重复记账幂等 |
| `consent::tests::budget_zero_never_exhausts_and_has_no_remaining` | 0 预算不置用尽、`remaining_micros() = None` |
| `consent::tests::ended_consent_rejects_further_spending` | 已结束授权拒绝记账（`PolicyBlocked`） |
| `consent::tests::end_for_task_keeps_exhausted_reason` | 终态不覆盖 `exhausted`，`ended_at` 保持空 |
| `consent::tests::exhausted_persists_across_restart` | 关闭并重开数据库后仍 `exhausted`、`is_valid` 拒绝、`validate_call` 拒绝 |
| `audit::tests::extended_fields_roundtrip_and_chain_verifies` | 6 个扩展字段往返，链校验通过 |
| `audit::tests::legacy_rows_written_before_v2_still_verify` | 用 v1 规范化载荷手写旧行（新列 NULL）→ 链校验通过；新行接续也通过 |
| `audit::tests::tampered_extension_field_is_detected` | 直接改 `rubric_version` → 链校验失败（扩展字段纳入链） |

### 11.6 与 rubric / inference / app 的集成清单

**rubric（`RubricStore`/`RubricAudit` 适配器，读 `eg-rubric::ports`）**

| 端口 | platform 实现 |
|---|---|
| `load_draft(RubricId)` | `get_stored_rubric_draft` → 组装 `StoredDraft`；负载字段为 `None`（v1 旧行）时返回 `RubricError::Store`（提示重建草稿），不得用默认值猜造 |
| `save_draft(&StoredDraft)` | `upsert_stored_rubric_draft`（状态迁移/编辑/重跑校验）；首次创建可用 `insert_stored_rubric_draft` |
| `list_drafts(QuestionId)` | `list_stored_rubric_drafts_by_question` |
| `active_rubric(QuestionId)` | `get_active_rubric` |
| `rubric_version(QuestionId)` | `paper_rubric_version`（**试卷级** CAS 期望值） |
| `save_active_rubric(question, rubric, expected)` | 在 `AuditLog::business_write(NewAuditEntry::new("rubric.confirm", "rubric", draft_id).with_rubric_version(next).with_template(...).with_template_ref(...), ...)` 内调用 `repo::save_active_rubric`；`VersionConflict` 映射为 `RubricError::VersionConflict` |
| `RubricAudit::append` | `AuditLog::business_write` + `with_rubric_version`/`with_template`/`with_prompt_hash`/`with_model_call`；审计动作名建议 `rubric.draft_*`/`rubric.confirm`/`rubric.reject` |

映射字段：`StoredDraft.draft` ↔ `StoredRubricDraft.draft`；`question_kind`/`max_score`/`mode`/
`partial_choice` 直接进负载；`template`（`TemplateRef`）serde 序列化为 `template_ref`，
并冗余 `template_id`/`template_version = TemplateVersion::as_u32()`。

**inference（`PlatformConsentGate`，读 `eg-inference::platform_adapter`）**

1. `map_state` 已补 `eg_platform::consent::ConsentState::Exhausted => ConsentState::Exhausted`
   （platform 新增枚举变体属编译期破坏性变更；inference 适配器已同步，`cargo check -p eg-inference` 通过。
   以后 platform 再增状态值时同样必须同步，否则下游 exhaustive match 编译失败）。
2. `mark_exhausted(task_id)` 已由 no-op 改为 `self.consents.mark_exhausted(task_id)`；
   **遗留**：额度记账 `record_spend(consent_id, delta_micros)` 尚未被 inference 适配器调用，
   当前花费仍只在进程内 `BudgetLedger` 累计（重启归零）。接入方式：每次云端调用成功后调用
   `record_spend`，达到预算时平台侧自动置 `exhausted`，与进程内账本可对账。
3. `consent_record_to_task_consent` 仍可直接复用（新增字段不参与 `TaskConsent` 结构）；
   如需展示余额，用 `record.remaining_micros()`。
4. 重启后 `is_valid` 对 `exhausted` 返回 `None`，`validate` 会得到
   `ConsentInactive { state: "exhausted" }`，界面应提示「额度用尽，需重新授权」。

**app**

- 授权设置/诊断页展示 `ConsentState::Exhausted`（label「额度用尽」）与 `count_by_state` 计数；
  不需要感知 schema v2（`Platform::open_with` 自动迁移，失败进入维护模式）。
- 规则确认流程按 §11.6 rubric 表格接 `platform::repo`；诊断包的 `schema.txt` 自动出现
  `0002_rubric_payload_consent_audit`（`diagnostics_text` 遍历 `MIGRATIONS`）。

### 11.7 风险与限制

1) **版本模型**：`paper.rubric_version` 是试卷级单调版本；不同题目的现行 `Rubric.version`
   是各自确认时的版本（可能小于当前试卷版本，因为其它题目随后确认推进了版本）。判分/幂等键
   仍取该题 `Rubric.version`；读写 `paper.rubric_version` 只用于确认 CAS 与就绪判断。
2) **v1 数据**：v1 从未发行，本机 v1 库仅测试数据；升级后旧草稿负载为空、旧 `rubric_revision`
   `question_id` 为 NULL（不参与按题查询），旧审计行链校验不受影响。若未来出现真实 v1 数据，
   需在适配器层显式提示「重建草稿/规则」，不得静默补默认值。
3) **upsert 与审计**：`upsert_stored_rubric_draft` 只改当前行（历史靠审计）；适配器必须保证
   「业务写 + 审计」同一事务，否则状态迁移会缺失审计。
4) **审计扩展字段暂不可过滤**：`AuditQuery` 仍只支持实体/时间过滤；按 `rubric_version`/
   `model_call_id` 查询留待界面需要时再加（索引已就绪）。
5) **迁移失败恢复**：见测试 `failed_migration_rolls_back_every_statement`——失败后库保持 v1，
   修复冲突原因后重试即可，不需要删库；生产启动失败应进入维护模式（既有行为）。

## 12. schema v3：复核队列 / 撤销栈 / 导出记录 / 提交级排除 / 判分幂等与断点 / 审计命令字段（2026-11-03，task-42）

### 12.1 背景与范围

v3 为 review（T31–T32）、report（T33–T34）、grade（T30）的真实落库补缺口（来源：`docs/dev/research/review.md` §6、`report.md`、`grade` 并行实现反馈）：

1. 跨提交队列查询（按模式/状态/抽检/原因/学生/题目/试卷/班级）、最新修订视图、稳定排序键游标分页、按 id/最新读取；
2. 提交级排除（`SubmissionExclusion`）持久化；
3. 撤销栈（跨重启）与导出记录（摘要 + 重放）；
4. 判分幂等键（重启后重跑不重复调用模型/不重复写记录）与判分断点（提交/题级状态、attempts、引擎指纹）；
5. 复核原因多值存储（队列按原因过滤）；
6. 审计命令扩展字段（命令幂等键、提交/题实体、原因码、范围、确认标记、抽检结论）。

只追加迁移：`migrations/0003_review_undo_export_progress.sql`；`SCHEMA_VERSION = 3`；`TABLES` 增至 29 张。

### 12.2 迁移 0003 内容

| 对象 | 内容 |
|---|---|
| `grade_record` 索引 | `(mode,status,sampled)`、`(status,question_id)`、`(created_at,id)`；`submission(student_id,status)` 供队列按学生过滤 |
| `review_undo` | 撤销栈：`id`、`command_id`、`action`、`entity_type`、`entity_id`、`payload`（JSON）、`created_at`；索引 `(created_at,id)`、`command_id` |
| `export_record` | 导出记录：`id`、`kind`、`format`、`scope`（JSON）、`summary_hash`、`row_count`、`body`（JSON）、`created_at`、`audit_id`；索引 `(created_at,id)`、`summary_hash` |
| `submission_exclusion` | 提交级排除：`submission_id`（主键、外键）、`reason`、`note`、`at`、`body`（版本化 JSON） |
| `grade_idempotency` | `key`（主键）、`submission_id`、`question_id`、`mode`、`grade_id`、`result_summary`（JSON）、`created_at`；索引 `(submission_id,question_id)` |
| `grade_progress` | `submission_id`、`question_scope`（空串 = 提交级，否则题目 id）、`state`、`attempts`、`engine_fingerprint`（JSON）、`detail`（JSON）、`updated_at`；主键 `(submission_id,question_scope)`；索引 `(state,updated_at)` |
| `grade_review_reason` | 判分记录复核原因多值：`(grade_id, reason_code)` 主键、`grade_id` 外键；索引 `reason_code` |
| `audit_log` 新列 | `command_id`、`submission_id`、`question_id`、`reason_code`、`scope`、`confirmed`、`sampling_verdict`；索引 `command_id`、`(submission_id,question_id)` |

### 12.3 repo 新接口（`eg_platform::repo`）

| 接口 | 说明 |
|---|---|
| `list_grade_records_for_queue(conn, &GradeQueueFilter, Option<GradeCursor>) -> Vec<GradeRecord>` | 每个「提交 × 题」只返回最新修订；按 `(created_at,id)` 升序；过滤字段见 `GradeQueueFilter`（modes/statuses/sampled/reasons/student/question/submission/paper/classroom/limit；limit 默认 100、上限 1000） |
| `count_grade_records_for_queue(conn, &filter) -> u64` | 同一过滤条件的总数（分页显示用） |
| `get_grade_record(conn, GradeId) -> Option<GradeRecord>` | 按判分记录 id 读取（含历史修订） |
| `get_latest_grade_record(conn, SubmissionId, QuestionId) -> Option<GradeRecord>` | 某提交某题最新修订 |
| `append_grade_record_with_reasons(tx, &GradeRecord, &[ReviewReasonKind])` | 追加记录 + 写复核原因（同事务） |
| `insert_grade_review_reasons(tx, GradeId, &[ReviewReasonKind])` | 单独写原因（重复幂等忽略） |
| `get_submission_exclusion(conn, SubmissionId)` / `upsert_submission_exclusion(tx, &SubmissionExclusion)` / `delete_submission_exclusion(tx, SubmissionId) -> bool` | 提交级排除读写/撤销 |
| `get_grade_idempotency(conn, &str)` / `put_grade_idempotency(tx, &GradeIdempotency) -> bool` | 幂等键读取/「首次写入」语义（已存在返回 false，不覆盖结果） |
| `get_grade_progress` / `put_grade_progress` / `list_grade_progress(conn, SubmissionId)` / `list_incomplete_grade_progress(conn, limit)` | 断点读写/列出/崩溃恢复扫描（`state != done`，按更新时间升序） |
| `push_undo(tx, &UndoEntry)` / `pop_undo(tx)` / `list_undo(conn, limit)` | 撤销栈 LIFO（`created_at,id` 降序）；`pop_undo` 返回并删除最近一条 |
| `insert_export_record` / `get_export_record` / `list_export_records` / `find_export_records_by_summary` | 导出记录写入/读取/列表/按摘要重放定位 |
| `load_grade_evidence_summary(conn, GradeId) -> Option<GradeEvidenceSummary>` | 按判分记录取原图 + 派生物 + 该题识别结果摘要（`AssetSummary`/`RecognitionSummary`） |

新增模块类型：`GradeQueueFilter`、`GradeCursor`、`GradeIdempotency`、`GradeProgress`/`GradeProgressState`、`UndoEntry`、`ExportRecord`、`AssetSummary`、`RecognitionSummary`、`GradeEvidenceSummary`；均派生 `Debug/Clone/PartialEq`，持久化型另外派生 serde。

### 12.4 audit 扩展

- `NewAuditEntry`/`AuditEntry` 新增：`command_id: Option<String>`、`submission_id`、`question_id`、`reason_code`、`scope`、`confirmed: Option<bool>`、`sampling_verdict`；构造器：`with_command_id`、`with_entities`、`with_reason_code`、`with_scope`、`with_confirmed`、`with_sampling_verdict`。
- `AuditQuery` 新增 `command_id`/`submission_id` 过滤（内存过滤，索引用于未来 SQL 过滤）。
- `AuditLog::command_write(entry, |tx| ...)`：等价 `business_write`，供复核/判分命令把「追加修订 + undo 入栈 + 排除写 + 审计」放同一事务；失败整体回滚（已测）。
- **链哈希向后兼容**：v3 新字段只在非空时进入规范化载荷（`skip_serializing_if`），v1/v2 旧行链哈希重算不变；混合 v2/v3 条目仍可 `verify_chain` 通过（测试 `command_fields_roundtrip_and_chain_verifies`）。

### 12.5 下游接入说明

**review**

1. 队列：`list_grade_records_for_queue` + `GradeQueueFilter`（mode/status/sampled/reason/student/question/paper），页码用上一页最后一条构造 `GradeCursor`；排序键是 `(created_at,id)`，review 若需按原因严重度/置信度排序，可在内存对一页重排（平台不跨页重排，避免游标语义破裂）。
2. 命令：用 `audit.command_write(entry, |tx| { append_grade_record_with_reasons(记录 + 原因) + upsert/delete_submission_exclusion + push_undo })`；命令幂等键放 `entry.command_id`，抽检结论放 `sampling_verdict`，重跑范围放 `scope`。
3. 撤销：`pop_undo` 取反向操作并删除；栈为空返回 `None`（UI「无可撤销」）。
4. 详情：`load_grade_evidence_summary` 一次取齐资产/识别摘要，避免多次往返；图片字节仍由 asset 读取 API 懒加载。

**report**

1. 统计：按试卷/班级用 `GradeQueueFilter{paper_id/classroom_id, ...}` 拿最新修订后自行聚合（平台不做统计口径）。
2. 导出：成功后在**同一事务**里 `insert_export_record` + 审计（可用 `command_write`）；重放校验用 `find_export_records_by_summary(新摘要)`，命中即内容一致。

**grade**

1. 判分前 `get_grade_idempotency(key)`；命中则复用 `grade_id`/`result_summary`，不再调用模型。
2. 判分开始/完成写 `put_grade_progress`（提交级 + 题级，`attempts` 递增、`engine_fingerprint` 记录 CAS/OCR/判定版本）；重启后 `list_incomplete_grade_progress` 扫描 pending/running/failed 归位重试。
3. 落库事务：`append_grade_record_with_reasons` + `put_grade_idempotency` + `put_grade_progress` + 审计建议包在一个事务（`Database::write`；审计用 `append_in_tx`/`command_write`）。

**app**

- 导入/复核/报表页无需感知 schema v3：`Platform::open_with` 自动迁移；诊断包 `schema.txt` 会列出 `0003_review_undo_export_progress` 与 29 张表。
- 复核工作台按 review 端口接入；「撤销」按钮直接走 review 服务（底层 `pop_undo`）。

### 12.6 测试与门禁（2026-11-03 实测）

| 项 | 结果 |
|---|---|
| `cargo test -p eg-platform` | **165 passed / 0 failed**（lib 154 + 集成 11：cp_b_perf 5、cp_b_sigkill/keychain 3、v3_backup_restore 1、其余 2） |
| 新增迁移测试 | `v2_to_v3_migration_preserves_rows_and_creates_v3_objects`（旧行/旧 body/旧审计列保留、新表/列/索引/唯一键/外键）、`failed_v3_migration_rolls_back_alter_and_index_statements`（失败回滚 + 修复后重试） |
| 新增 repo 测试 | 队列最新修订/全部过滤/游标分页、排除读写删除、撤销栈 LIFO、导出记录与摘要重放、幂等键 insert-once、断点 upsert/未完成扫描、证据摘要（资产 + 识别） |
| 新增 audit 测试 | 命令字段 roundtrip + 链验证、`command_write` 成功与失败回滚 |
| 备份/恢复 | `v3_backup_restore`：v3 四张新表随备份恢复、schema=3、表计数/审计链一致、摘要重放仍可定位 |
| `cargo clippy -p eg-platform --all-targets -- -D warnings` | 通过 |
| `cargo fmt -p eg-platform -- --check` | 通过 |
| `cargo deny check` | `advisories ok, bans ok, licenses ok, sources ok` |

### 12.7 风险与限制

1) **队列排序语义**：平台只提供稳定键 `(created_at,id)`；review 的原因严重度/置信度排序是页内重排，跨页严格顺序不等于业务优先级（与 review.md R4 一致）。若需跨页业务排序，需在 `grade_record` 落盘可排序的业务键（新增迁移，Ask first）。
2) **原因多值**：`grade_review_reason` 由判分/复核侧显式写入；未写入原因的记录不会命中 `reasons` 过滤。review 适配层需保证与记录同事务写。
3) **幂等键信任边界**：`key` 由 grade 生成（提交+题+版本+识别哈希+引擎指纹+模式）；平台只保证唯一与首次写入，不校验其内容。重复键结果不覆盖，避免「更差的第二次结果」覆盖首次。
4) **导出重放**：`summary_hash` 由 report 定义（内容摘要）；平台只做等值查找，不验证导出文件字节。重放一致性的语义归 report。
5) **断点状态文本**：`state` 目前为平台枚举 4 值（pending/running/done/failed）；若 grade 需要更细阶段，需升迁移/枚举（Ask first）。
6) **删除语义**：V1 只提供撤销栈 `pop` 与提交级排除删除；不提供历史修订删除/物理清理（只追加原则）。
7) **审计扩展字段过滤**：`AuditQuery` 的 `command_id`/`submission_id` 目前内存过滤（先按 rowid 取 limit 再过滤），数据量大时应改为 SQL 条件（索引已建）。


## 13. schema v4：复核重做栈 `review_redo`（2026-11-09，task-66）

> 背景：v3 只有 LIFO `review_undo`；app 侧 redo 只能内存、重启丢失。
> 本任务只追加迁移，不改历史 SQL；`SCHEMA_VERSION = 4`，`TABLES` 增至 30 张。

### 13.1 迁移 0004

文件：`crates/eg-platform/migrations/0004_review_redo.sql`（只追加，独立事务；失败整体回滚且版本不推进）。

| 表/索引 | 内容 |
|---|---|
| `review_redo` | 与 `review_undo` 对称：`id`(ULID)、`command_id`、`action`、`entity_type`、`entity_id`、`payload`(JSON)、`created_at`(RFC3339) |
| `idx_review_redo_created` | `(created_at, id)`：LIFO 与稳定分页 |
| `idx_review_redo_command` | `command_id`：按命令定位/审计关联 |

迁移测试：`crates/eg-platform/tests/v4_review_redo.rs`
- `migrates_v3_to_v4_preserving_data`：手工建 v3 库 + 旧 `review_undo` 行 → `Database::open` 升 v4，旧行保留、`review_redo` 为空、新 API 可写。
- `failed_v4_migration_rolls_back_and_keeps_version`：预置同名表使 `CREATE TABLE` 失败 → `schema::apply` 返回带迁移名的错误；`schema_version` 仍 3；旧行与旧表保留；失败建索引不残留。
- `v4_backup_manifest_includes_redo_table`：备份清单包含 `review_redo`（恢复兼容沿用 `BackupService` 全表逻辑）。
- `v3_backup_restore` 已改为当前版本动态断言并加入 `review_redo` 表检查。

### 13.2 repo API（`eg_platform::repo`）

| 签名 | 语义 |
|---|---|
| `push_redo(tx: &Transaction, entry: &RedoEntry) -> Result<()>` | 重做栈入栈（撤销命令事务内调用）。**不自动清空**。 |
| `pop_redo(tx: &Transaction) -> Result<Option<RedoEntry>>` | LIFO（`created_at DESC, id DESC`）弹出并删除；空栈返回 `None`。 |
| `list_redo(conn: &Connection, limit: usize, before: Option<&RedoCursor>) -> Result<Vec<RedoEntry>>` | 最新在前；`limit=0` 默认 50；`RedoCursor{created_at,id}` 严格小于上一页最后一条，同时间按 id 稳定分页、不重/不漏。 |
| `clear_redo(tx: &Transaction) -> Result<usize>` | 清空重做栈，返回删除条数；**新命令写入时必须与命令同事务调用**。 |

`RedoEntry` 字段与 `UndoEntry` 对称；`payload` 由 review 定义，平台不解释。

### 13.3 语义（必须遵守）

1. **撤销后可重做**：执行撤销（`pop_undo`）时，在同一事务内 `push_redo` 一条「前向操作」负载（即重做要重新应用的操作，不是撤销的反向负载）；应用反向操作与审计同事务。
2. **重做后撤销**：执行重做（`pop_redo`）时，在同一事务内 `push_undo` 一条反向负载，允许再次撤销。
3. **新操作使重做失效**：任何新的复核命令（接受/改分/排除/恢复等，非撤销/重做）在写入命令/审计的同一事务内先 `clear_redo(tx)`（顺序：清空 → 写业务/审计 → push_undo）；否则旧重做路径会覆盖新状态。`push_redo` 不自动清空，避免撤销时误清空自身历史。
4. **重启可读**：表持久化；`Platform::open_with` 自动迁移到 v4；app 启动时 `list_redo(conn, 50, None)` 恢复「可重做」状态与按钮可用性。
5. **失败回滚**：命令与 redo/undo/审计同一 `Database::write` 事务；任一失败整体回滚，避免「操作已生效但 redo 不匹配」。

### 13.4 app/review 接入说明

**review 命令事务模板**

```
db.write(|tx| {
    // 新命令：先清空 redo（撤销/重做命令除外）
    repo::clear_redo(tx)?;
    // ... 写业务（append_grade_record_with_reasons / review_action）
    // ... 审计（command_write / append_in_tx）
    repo::push_undo(tx, &undo_entry)?;   // 供撤销
    Ok(())
})
```

**撤销**

1. `pop_undo(tx)` 取 `UndoEntry{payload=反向负载}`；
2. 应用反向操作 + 审计；
3. `push_redo(tx, &RedoEntry{id:new_ulid, command_id:新命令, action, entity_type, entity_id, payload=前向负载, created_at})`（前向负载 = 重做时要重新应用的操作；由 review 适配层转换，平台只存）。

**重做**

1. `pop_redo(tx)` 取前向负载；2. 应用前向操作 + 审计；3. `push_undo(tx, &UndoEntry{payload=新的反向负载})`，允许再次撤销。

**UI**：重做按钮可用性 = `list_redo(...)` 非空；重启后从表恢复。错误/不可重做时保持栈不变（事务回滚）。

### 13.5 验证（2026-11-09 实测）

| 项 | 结果 |
|---|---|
| `cargo test -p eg-platform` | **170 passed / 0 failed**（lib 154 + 集成 16：cp_b_perf 5、cp_b_sigkill 3、cp_b_keychain 2、v3_backup_restore 1、v4_review_redo 5） |
| `cargo clippy -p eg-platform --all-targets -- -D warnings` | 通过 |
| `cargo fmt -p eg-platform -- --check` | 通过 |

### 13.6 风险与限制

1. **清空责任在调用方**：平台不在 `push_redo` 内自动清空，避免撤销时清掉刚入栈的重做；新命令必须显式 `clear_redo`（已写入 API 文档与本节模板）。
2. **payload 语义由 review 定义**：平台不校验「前向/反向」是否正确；review 适配层必须保证 undo/redo 负载成对，并有命令幂等键/审计可追溯。
3. **并发**：单写连接串行化；`clear_redo` 与命令同事务，避免多窗口竞态导致重做过期。
4. **无限增长**：redo 只在撤销时增长、pop 删除；若同一撤销链反复 undo/redo，表大小受命令数约束；未设置上限/保留期（与 undo 一致）。
