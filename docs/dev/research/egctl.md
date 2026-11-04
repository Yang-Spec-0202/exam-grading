# 研究记录：egctl（运维与验收 CLI）

> 状态：实现记录 · 2026-11-03 · 对应规范：`SPEC.md`（§5 命令、§7 运维与验收）· `SPEC-platform.md`（诊断包/备份/日志）· `SPEC-report.md`（重放）· 任务：`tasks/todo.md` T39
> 规则：本页必须写满下列表格；无来源的结论视为未完成。

## 1. 模块与范围

- 模块 id：`egctl`（bin crate，依赖 `eg-domain`/`eg-platform`/`eg-report`）。
- 本次要实现的能力：
  - `doctor`：数据目录可写/磁盘余量、DB 打开 + `integrity_check` + 迁移版本、单实例锁、模型注册表 +
    权重 SHA-256 抽查、llama.cpp 可执行与端口、Python/uv、日志、审计链、配置偏差、模块就绪度；
    人类可读 + `--json`，退出码区分 OK/WARN/ERROR。
  - `replay`：夹具回放框架（当前覆盖 `eg-report` 统计+导出+重放摘要；判分回放 TODO）。
  - `bench`：1000 份导出实测；单页预处理/单题判分/UI 启动（UI 调 `eg-app --bench`，其余未就绪 WARN）。
  - `docs check`：复用 `scripts/docs-check.sh`（无代码围栏/死链/mdBook 构建），支持 `--json`。
  - `backup` / `restore <dir>` / `diagnostics export`：`eg-platform` 服务薄封装。
- 明确不做：不判分、不改业务数据（备份/恢复/诊断包除外）；不联网；不启动/停止模型服务；
  不重复实现备份/诊断包/文档门禁逻辑；不伪造 OK（未实现一律 WARN）。

## 2. 问题清单

| # | 需要确认的外部行为/API/版本/限制 | 影响 |
|---|---|---|
| Q1 | 用 `clap` 还是手写参数解析？`clap` 是否在离线缓存中？ | 新增依赖与离线构建 |
| Q2 | doctor 如何在不创建/不迁移数据的前提下读配置？`ConfigStore::load` 会写默认配置吗？ | doctor 是否只读 |
| Q3 | 打开数据库是否必然触发迁移/快照（写副作用）？ | doctor 的副作用与验收口径 |
| Q4 | 单实例锁能否「探测」而不长时间占用？`InstanceLock::acquire` 失败信息是否够诊断？ | doctor 单实例检查 |
| Q5 | 审计链完整性校验 API？空库是否通过？ | 审计检查 |
| Q6 | 模型注册表位置、权重目录布局、SHA-256 字段？ | 模型检查 |
| Q7 | llama.cpp 二进制名与端口从哪里取？端口占用如何判定？ | 推理依赖检查 |
| Q8 | Python/uv 缺失是否影响 v1 数学判分？ | WARN 还是 ERROR |
| Q9 | `eg-report` 重放摘要能否跨目录/跨进程稳定？ | replay 判定 |
| Q10 | `eg-inference` 当前能否编译（模型注册表是否直接复用）？ | 注册表检查实现方式 |
| Q11 | `eg-app --bench` 输出格式与退出码？ | UI 启动基准解析 |
| Q12 | 退出码语义与 `--json` 结构？ | 验收脚本消费 |

## 3. 来源

| # | URL/路径 | 文档版本/日期 | 类型 | 取用内容 |
|---|---|---|---|---|
| S1 | `SPEC.md` §5、§7 | v0.2.3 | 项目规范 | 命令清单（doctor/replay/bench/docs/backup/diagnostics）、验收退出码要求 |
| S2 | `SPEC-platform.md` §2/§4/§5/§6 | v0.2.1 | 项目规范 | 数据目录布局、单实例、迁移、审计链、诊断包（≤10s、不含原图）、备份/恢复 |
| S3 | `SPEC-report.md` §3.1/§9.9 | v0.2.1 | 项目规范 | 重放定义：同一快照/时间戳 → 内容摘要相等 |
| S4 | `CONFIG.md` §9.5/§12 | v0.2 | 项目规范 | `inference.registry_path=models/registry.toml`、`backup.keep_generations=5`、日志保留 |
| S5 | `crates/eg-platform/src/{platform,paths,db,audit,backup,diagnostics,instance,config}.rs` | 当前分支 | 源码 | 构造/访问器、`Database::integrity_check/schema_version`、`AuditLog::verify_chain`、`BackupService`、`Diagnostics::export`、`InstanceLock` |
| S6 | `crates/eg-report/src/{export,stats,source}.rs` | 当前分支 | 源码 | `export()`、`verify_replay()`、`StaticReportSource`/`ReportInput` |
| S7 | `crates/eg-inference/assets/models.registry.toml`、`scripts/model/{pull,verify}.sh` | 2026-11-02 实测 | 源码/脚本 | 注册表字段（id/role/backend/local.port/local.binary/weights.sha256/bytes）、权重落盘目录 `models/<id>/` |
| S8 | `crates/eg-app/src/main.rs`、`src/bench.rs` | 当前分支（M9 T35 外壳） | 源码/实测 | `eg-app --bench` 输出 `[bench] startup_ms=...`、`nav_ms=...`、`invoke_failures=`；未达标退出码 2 |
| S9 | `scripts/docs-check.sh` | 当前分支 | 仓库脚本 | 文档门禁的单一事实来源（规范无围栏、相对链接、mdbook build） |
| S10 | https://docs.rs/toml/1.1.8、https://docs.rs/serde_json/1.0.154 | 1.1.8 / 1.0.154（`Cargo.lock`） | 官方文档 | 注册表/夹具/JSON 输出解析（已缓存，离线可用） |

## 4. 结论

| 问题 | 结论 | 依据 |
|---|---|---|
| Q1 | 手写参数解析（6 个子命令 + 少量全局选项），不新增 `clap`；依赖仅 `chrono/eg-domain/eg-platform/eg-report/serde/serde_json/sha2/toml`，全部在 `Cargo.lock` 既有缓存中，`--offline` 可构建。 | S5、§5 实测 |
| Q2 | doctor 直接读 `config.toml` → `toml::from_str::<ConfigDocument>` + `AppConfig::validate()`，**不构造 `ConfigStore`**，避免 doctor 写默认配置；配置文件缺失时只 WARN。 | S5 |
| Q3 | `Database::open_with` 会按需迁移，属写操作；doctor 用 `backup_dir: None` 关闭迁移前快照但仍会迁移（幂等），并在输出中说明；如库不存在则只 WARN 不建库。 | S5 |
| Q4 | `InstanceLock::acquire` 用 `try_lock_exclusive`，被占用时立即返回 `InstanceLocked`（含占锁 pid/版本/数据根）；doctor 获取成功后立即释放，属短探测。 | S5 |
| Q5 | `AuditLog::verify_chain()` 从创世哈希逐条校验 `prev_hash`/`chain_hash`，空库返回 `entries=0, ok=true`；doctor 直接展示条目数与断链位置。 | S5 |
| Q6 | 注册表路径取 `inference.registry_path`（相对数据根）；默认开发回退到仓库内嵌 `crates/eg-inference/assets/models.registry.toml`；权重目录 `models/<id>/<file>`；逐文件比对 `bytes` 与 `sha256`。 | S4、S7 |
| Q7 | 二进制名取注册表 `models.local.binary`（缺省 `llama-server`），`PATH` 查找 + `--version`；端口取 `models.local.port`（7 个本地条目，8081–8085），用 `TcpStream::connect_timeout(150ms)` 判「占用/空闲」。 | S7、实测 |
| Q8 | Python/uv 仅用于工具链脚本（模型下载/校验/CAS 相关），**不影响 v1 数学判分**；缺失记 WARN。 | S7、S1 |
| Q9 | `eg-report` 重放摘要对「可见内容 + 文件名集合」计算，跨目录稳定（同一时间戳/同快照）；`verify_replay` 同时比对快照/口径/命名版本/脱敏。 | S6、S3 |
| Q10 | 本任务期间 `eg-inference` 未通过编译（platform 新增 `ConsentState::Exhausted` 未匹配 + `AuditEntry` 新字段初始化未完成），故 **不链接** `eg-inference`；`doctor` 用轻量 TOML 解析只读校验注册表，TODO：编译恢复后换 `ModelRegistry::load`/`license_violations`。 | 编译实测 |
| Q11 | `eg-app --bench` 输出 `[bench] startup_ms={"platform_open":…,"first_frame":…}`、`[bench] nav_ms={…"p95":…}`、`[bench] invoke_failures=N`，未达标退出码 2；`egctl` 解析首帧/P95/失败数，无输出则 WARN「未实现」。 | S8、实测 |
| Q12 | 退出码：OK=0、WARN=2、ERROR=1；`--json` 统一 `{tool, command, status, ...}`，doctor 另含 `checks[]`（`{id,title,status,detail}`）。 | S1、实现 |

## 5. 版本实测

| 依赖/工具 | 实测版本 | 命令 | 结果摘要 |
|---|---|---|---|
| cargo / rustc | 1.99.1 / 1.99.1 | `cargo --version`、`rustc --version` | MSRV 1.97 满足 |
| egctl | 0.1.0 | `cargo run -p egctl -- version` | `egctl 0.1.0（exam-grading 运维/验收 CLI）`，退出 0 |
| doctor（空数据目录） | — | `cargo run -p egctl -- --data-dir <tmp-dir>/… doctor` | WARN（退出 2）：数据目录/配置/DB/日志未创建；模型注册表已定位（7 本地/1 云端，0 已安装）；llama-server 已找到（`/opt/homebrew/bin/llama-server`）；端口 8081–8085 空闲；report 自检 OK；5 个并行模块 WARN |
| doctor（备份后的数据目录） | — | 同上 | WARN（退出 2）：data_dir/config/db/instance/audit/cas/logs 均 OK（schema v3、integrity 通过、审计链 0 条完整）；models 仍 WARN（未下载权重）；模块 WARN |
| backup | — | `egctl backup --json --data-dir …` | 成功：schema 3、DB 438272 字节、SHA-256 `7a1e5da9…`、清单写入 `backups/<时间戳>/manifest.json`（直接走 `eg-platform`） |
| restore | — | `egctl restore <备份目录> --json` | 成功：恢复后 integrity 通过、schema 3、安全副本 `backups/<时间戳>-restore-safety/exam-grading.sqlite3` |
| diagnostics export | — | `egctl diagnostics export --out <目录> --json` | 成功：`diagnostics.zip`（3810 字节，条目 README/config.redacted/logs/schema/environment/database-stats/tasks/consent 等） |
| replay（report） | — | `egctl replay` | OK+WARN：60 学生×20 题，10 文件/110KiB，摘要 `a4b600b8…` 两次一致；判分/资产回放 WARN（TODO）；退出 2 |
| bench（debug） | — | `cargo run -p egctl -- bench` | 1000×20：统计构建 1.96–2.55s、单 XLSX 332–454ms、三格式 401–549ms；`eg-app --bench` 首帧 158.8ms、切页 P95 0.004ms、命令失败 0；预处理/判分 WARN；退出 2 |
| docs check | — | `egctl docs check --json` | 通过：`scripts/docs-check.sh` 退出 0、294ms；57 条相对链接 0 死链、mdBook 构建成功 |
| 单元测试 | 4 项 | `cargo test -p egctl --offline` | 全绿（退出码/级别、字节格式化、SHA-256 已知向量、配置偏差） |
| 静态检查 | — | `cargo clippy -p egctl --all-targets --offline -- -D warnings` | 0 warning |
| release 基准 | — | `cargo run --release -p egctl -- bench` | 达标：1000×20 统计构建 111ms、单 XLSX 32ms、三格式 73ms（预算 8s/15s）；`eg-app --bench` 首帧 493.8ms、切页 P95 0.004ms、命令失败 0；退出 2（预处理/判分未就绪） |
| 并行编译波动 | — | 多次 `cargo check -p eg-platform` | 执行期间平台 crate 被并行任务短时改坏（`AuditEntry` 新字段未初始化）后自行恢复；egctl 不受影响时门禁全绿 |

## 6. 风险与限制

| 风险 | 影响 | 缓解/预案 |
|---|---|---|
| doctor 打开 DB 会触发迁移（写副作用） | 「诊断」不再是纯只读 | 输出中说明；库不存在时不创建；需要严格只读时后续加 `--read-only` 并用 `rusqlite` 只读打开 |
| doctor 短暂获取单实例锁 | 与桌面端启动存在毫秒级竞争 | 获取后立即释放；被占用只 WARN 不重试 |
| `df -Pk` 依赖系统命令 | 部分环境无 `df` 时磁盘余量显示「无法测量」 | 明确 WARN 文本；不伪造数字 |
| 大模型权重全量哈希代价高 | 模型检查可能很慢 | `--max-sha-mib`（默认 64）超阈值只校验大小 + 首尾采样指纹，并分别计数「全量校验/采样」 |
| 模型注册表轻量解析未走 `eg-inference` | 许可策略/字段校验可能少于推理模块 | 仅只读校验（id/权重/sha256/许可非空）；TODO 编译恢复后切换 |
| 并行开发导致 workspace 临时编译失败 | egctl 可能短时无法构建 | 本 crate 不链接未就绪 crate；doctor 用 `--deep` 如实报告 cargo check 结果；后台收尾后重跑门禁 |
| `restore`/`backup`/`diagnostics` 需桌面端退出 | 单实例锁冲突时命令失败 | 明确提示「请先退出桌面端」；不绕过锁 |
| `eg-app --bench` 的 profile | egctl 优先 release、缺失回退 debug；不同 profile 数字不可混用 | 输出注明实际 profile；以 release 数字为准 |

## 7. 与规范的偏差 / TODO

| 规范位置 | 偏差 | 处理（改规范/改实现） | 审批状态 |
|---|---|---|---|
| `SPEC.md` §7 replay | 判分/资产回放未实现：`eg-grade` 未通过编译、`capture/align` 未接入；当前只回放 report（统计+导出+摘要） | 框架保留 `--fixture`（`fixture.json` 的 `kind`）+ TODO；依赖就绪后补 grade replay | 待审批 |
| `SPEC.md` §7 bench | 仅 1000 份导出与 UI 启动可实测；单页预处理/单题判分未测量（WARN） | 不伪造数字；链路就绪后接入 capture/grade | 待审批 |
| `SPEC.md` §7 doctor | 模型注册表用轻量 TOML 解析（未链接 `eg-inference`）；大文件 SHA-256 按阈值采样 | 编译恢复后换 `ModelRegistry::load` + 许可策略校验 | 待审批 |
| `SPEC.md` §7 diagnostics | 日志条数默认上限 2000（`--log-limit` 可调），低于「诊断包 ≤10s」所需时才截断 | 由平台 `DiagnosticRequest.log_limit` 决定；需要完整日志时显式调大 | 待审批 |
| `SPEC.md` §7 命令解析 | 手写解析而非 `clap` | 子命令稳定且参数少；如后续扩展子命令再评估 `clap`（需联网/缓存） | 待审批 |
| `SPEC.md` §7 退出码 | 采用 0/2/1（OK/WARN/ERROR） | 需在 `SPEC.md` 明确「WARN=2」以免脚本误判 | 待审批 |
| `SPEC.md` §7 docs check | 直接调用 `scripts/docs-check.sh`（单一事实来源），未在 Rust 内复刻 | 保持脚本为门禁权威；`--json` 只包装结果 | 待审批 |
| `SPEC.md` §7 backup/restore | `restore` 后不自动启动应用，只做平台打开+完整性校验 | 输出提示重启并 `doctor` 复核 | 已记录 |
