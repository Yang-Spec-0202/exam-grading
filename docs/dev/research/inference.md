# 研究记录：inference（注册表/夹具/许可门禁 · 本地后端生命周期 · 任务授权/熔断/预算）

> 状态：已完成（T19–T20）· 2026-11-02（Asia/Shanghai）· 2026-11-03 增补：platform v2 `ConsentState::Exhausted` 接线（§8-D2）· 2026-11-04 增补：仓库级 `fixtures/` 夹具接线（§5.3，task-53）· 对应规范：`SPEC-inference.md` v0.2.3（§2/§3/§4/§5/§9）、`CONFIG.md` v0.2.3（§4/§5/§10/§14）、`SPEC-domain.md` v0.2.2（§8）、`CAPABILITY-MAP.md` v0.2 §2/§3、`AGENTS.md`
> 实测环境：Apple M4 / 16 GB / macOS 27.0.1（26A434）/ rustc 1.99.1（`58cf8f9c3 2026-10-13`）
> 结论用于：`crates/eg-inference/**`（实现）、`crates/eg-inference/assets/models.registry.toml`（默认注册表）
> 关联记录：`models.md`（模型清单/许可/下载脚本）、`inference-serving.md`（serving 栈/端点/延迟/内存）、`platform.md`（平台接口）。本文**不重复**前两者已确认的模型与 serving 事实，只补本模块实现契约所需的结论。

## 1. 模块与范围

- 模块 id：`inference`（crate `eg-inference`）。
- 本次实现：
  - **T19**：模型注册表（字段/校验/`serve_unit` 合并/引擎指纹）、夹具回放后端、许可门禁（白名单 + 排除清单，fail-closed）。
  - **T20**：本地后端生命周期（端口分配、拉起、探活、重启一次、停止、无孤儿进程）、**任务级授权**五维门禁、熔断（3 次瞬态 → 60 s 打开 → 1 次探测）、进程内预算台账、以及把每次调用写成 `ModelCall`/审计的编排层。
- 明确不做：判分/识别业务语义与提示词渲染（`recognize`/`rubric`/`grade` 负责）；模板/Schema 版本管理；云端 SDK（只用 OpenAI 兼容 HTTP）；平台配置/仓储/审计实现（只经 `platform_adapter` 调用 `eg-platform`）。
- 交付面：`Backend`（`Local`/`Cloud`/`Fixture`）+ `InferenceService::complete` 单入口；`ModelCallStore`/`AuditSink`/`IdSource`/`ConsentGate`/`Transport`/`ProcessRunner`/`PortAllocator` 七个注入点（测试全部用桩，**不依赖真实权重**）。

## 2. 问题清单（实现前需要确认的外部事实）

| # | 问题 | 落点 |
|---|---|---|
| Q1 | llama.cpp `llama-server` 的 OpenAI 兼容契约：`/health` 载荷、`/v1/chat/completions` 请求/响应字段、`usage`/`finish_reason`、错误码语义？ | §4-Q1、§5.1 |
| Q2 | sidecar 生命周期：启动参数、就绪判定、停止信号、端口冲突处理；如何不产生孤儿进程？ | §4-Q2、§5.2 |
| Q3 | 多个角色共用一个 serve 单元（router/structurer 复用 judge 权重）时，注册表如何表达而不重复加载？ | §4-Q3、§5.2 |
| Q4 | 夹具回放的**可复现契约**：键怎么构成？未命中怎么办？如何证明「字节一致」？ | §4-Q4、§5.3 |
| Q5 | 许可门禁怎么做到 fail-closed？白名单/排除清单边界？人工评审引用如何生效？ | §4-Q5、§5.2 |
| Q6 | `TaskConsent` 五维（任务/后端/范围/预算/状态）在领域类型上如何表达？过期/撤销/用尽怎么区分？ | §4-Q6、§5.4 |
| Q7 | 熔断计数口径：哪些错误算「瞬态」、哪些不计入？打开期与探测期如何断言 0 次调用？ | §4-Q7、§5.4 |
| Q8 | 预算台账：微元/向上取整、空闲与高峰两档价格、按任务上限、失败是否记账？ | §4-Q8、§5.4 |
| Q9 | Qwen3.5 系列默认会输出 thinking，如何关闭？在哪一层落地（注册表还是提示词）？ | §4-Q9、§5.2 |
| Q10 | 云端 DeepSeek 的 base_url/鉴权/JSON 输出/价格档，与本模块的哪些字段对应？ | §4-Q10、§5.4 |
| Q11 | `eg-platform` 已提供哪些接口可支撑 `ModelCall` 落库、审计、授权校验、时钟与 id？是否有缺口？ | §4-Q11、§5.5 |
| Q12 | 传输层：std 无 TLS，云端 `https://` 如何处理才不违背「失败显式」？ | §4-Q12、§7-R1 |
| Q13 | 仓库级 `fixtures/ocr/**`、`fixtures/judge/**` 如何接入？键与记录格式、匹配优先级、畸形/负向样例如何隔离？ | §4-Q13、§5.3、§5.6 |

## 3. 来源

| # | 来源 | 版本/日期 | 类型 | 取用内容 |
|---|---|---|---|---|
| S1 | 本仓库 `SPEC-inference.md` | v0.2.3（草案，2026-11-02） | 项目规范 | 三角色/三后端、注册表字段、许可基线、五维授权、熔断 3→60 s→1 探测、预算、失败模式、§9 验收 |
| S2 | 本仓库 `CONFIG.md` | v0.2.3 | 项目规范 | `ocr.page_timeout_s=60`（10–180）、`grade.judge_timeout_s=20`（5–60）、`inference.registry_path`、`privacy.cloud_enabled=false` |
| S3 | 本仓库 `SPEC-domain.md` | v0.2.2 §8 | 项目规范 | `ModelCall`（云端必带 `consent_id`）、`EngineStamp`、`TaskConsent`、`JudgeOutcome` 不变量 |
| S4 | `docs/dev/research/inference-serving.md` | 2026-11-02 一手实测 | 本仓库研究记录 | `/health` → `{"status":"ok"}`；`/v1/chat/completions` 非流式响应含 `choices[0].message.content` 与 `usage`；`llama-server` 默认端口 9931/默认 host 127.0.0.1；`--chat-template-file` 坑；prompt cache 影响 |
| S5 | `docs/dev/research/models.md` | 2026-11-02（含 2026-11-01 内部调研） | 本仓库研究记录 | 全部登记模型的许可（MIT/Apache-2.0）；排除清单来源；`Qwen/Qwen3.5-4B` 的 `tokenizer_config.json` 中 `chat_template` 对 `enable_thinking is false` 的显式分支；DeepSeek base_url/模型名/1M 上下文/价格（空闲 vs 高峰，元/百万 token） |
| S6 | https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md · https://github.com/ggml-org/llama.cpp/pull/18825 | master@2026-11-02（转引 S4 抓取记录） | 官方文档/上游 PR | OpenAI 兼容端点与多模态图片格式；PaddleOCR-VL 的 chat template 要求 |
| S7 | https://spdx.org/licenses/ | 2026-11（标识符标准） | 官方标准 | 白名单标识符规范写法：`MIT`、`Apache-2.0`、`BSD-3-Clause`、`ISC`、`Unicode-3.0` |
| S8 | https://api-docs.deepseek.com/（「模型 & 价格」「JSON Output」「首次调用 API」） | 2026-11-02（转引 S5 §8） | 官方文档 | `https://api.deepseek.com` + OpenAI 兼容 `/chat/completions`、Bearer、`response_format={"type":"json_object"}`、价格档 |
| S9 | https://platform.openai.com/docs/api-reference/chat | 2026-11（兼容基线） | 官方文档 | 请求字段 `model`/`messages`/`max_tokens`/`temperature`/`stream=false`/`response_format`；响应 `choices[0].message.content`、`finish_reason`、`usage.prompt_tokens`/`completion_tokens`/`prompt_tokens_details.cached_tokens` |
| S10 | 本机 `cargo tree -p eg-inference --depth 1`、`Cargo.lock` | 2026-11-02 | 命令输出 | 依赖实测版本（§5.1） |
| S11 | 本机 `cargo test -p eg-inference`、`cargo clippy -p eg-inference --all-targets -- -D warnings`、`cargo fmt -p eg-inference --check` | 2026-11-02 | 命令输出 | 101 个测试全绿；clippy/fmt 通过（§6） |
| S12 | `docs/dev/research/platform.md` + `crates/eg-platform/src/**` | 2026-11-02（已稳定，128 测试） | 本仓库研究记录 + 源码 | `Platform::open`/`open_with`/`close`、`audit.business_write`、`db.read/write`、`repo::insert_model_call`/`list_model_calls`、`consents.register/is_valid/revoke` |
| S13 | `fixtures/README.md`（含 `MANIFEST.sha256`）· `fixtures/generator/keygen/src/main.rs` | 2026-11-04 | 本仓库夹具契约 + 生成器源码 | 索引字段（`kind`/`backend`/`records[]`/`negative[]`）、记录 schema（与 `FixtureEntry` 一致）、键 = `fixture_key(model_id, version, schema, prompt_bytes, input_bytes)`、负向样例语义（`ocr/negative` 结构/语义拒绝、`judge/negative` 打分非法） |

> 说明：本轮实现期**联网复核被沙箱拒绝**（`raw.githubusercontent.com` 解析到非公网地址），因此外部契约以
> 仓库内 2026-11-02 的**一手实测记录**（S4/S5）为准，不重新臆测上游版本；涉及「应该存在」的 API 一律未采用。

## 4. 结论

| 问题 | 结论 | 依据 |
|---|---|---|
| Q1 | `llama-server` 提供 `/health`（就绪返回 `{"status":"ok"}`）与 `/v1/chat/completions`（非流式：`choices[0].message.content`、`finish_reason`、`usage`）。本模块只依赖这个**兼容子集**：`model`/`messages`/`max_tokens`/`temperature`/`stream=false`/`response_format`；响应缺 `choices`、`content` 非字符串、空内容、`finish_reason=length` 一律判 `SchemaViolation`（截断结果绝不当作完整结果）。HTTP 429/5xx → 瞬态；其余 4xx → 永久。 | S4、S9 + `openai.rs`/`local.rs::status_error` |
| Q2 | 启动用显式参数：`-m <gguf>`（+ `--mmproj`）、`--host 127.0.0.1`、`--port <分配值>`、`-c 8192`、`--temp 0.0`、`-ngl 99`（Metal 全量卸载）、`-a <unit 名>`、`--no-webui`、可选 `--chat-template-file`，最后追加注册表 `extra_args`；参数顺序与 `scripts/model/serve.sh` 一致，`-ngl` 显式给出以免服务端默认值随版本漂移。端口从注册表 `port` 起向后试 `port_attempts=3` 个，**先 bind 再释放**确认可用。就绪循环：短轮询 `poll_interval_ms=100`，`startup_timeout_secs` 超时即失败；启动后立即退出（进程退出码可读）→ 显式 `BackendUnavailable`。停止：先 SIGTERM、`stop_timeout_ms=10_000` 内未退再 SIGKILL；`Drop` 兜底 kill，避免孤儿。 | S4/S15（`--help` 实测） + `local.rs` |
| Q3 | 注册表用 `local.serve_unit` 把「角色条目」映射到「服务单元」：`qwen3.5-4b-router`/`qwen3.5-4b-structurer` 共享 `serve_unit="qwen3.5-4b-instruct"`（端口 8082）。`serve_unit(id)` 聚合该单元全部**去重**权重与 extra_args，一个进程服务多个角色；加载期校验「同单元端口/权重一致」，不一致即 `RegistryInvalid`。 | S1 §2.2 + `registry.rs` 测试 |
| Q4 | 夹具键 = `sha256(len:model_id ‖ len:model_version ‖ len:prompt_schema_version ‖ len:prompt ‖ len:input_bytes)`（长度前缀避免拼接歧义），回放按键取录制 `output_raw`，**字节级原样返回**；未命中返回 `FixtureMiss`，绝不合成。引擎指纹标注 `backend=fixture`，产物不得混入真实成绩。同输入 100 次回放字节一致由集成测试断言。 | S1 §2.1/§9.10 + `fixture.rs`/`tests/service_flow.rs` |
| Q5 | 白名单 = `MIT`/`Apache-2.0`/`BSD-3-Clause`/`ISC`/`Unicode-3.0`；排除清单 = `OXR-Proprietary`/`Youtu-Parsing-License`/`MinerU-Open-Source-License`/`OpenRAIL-M`/`CC-BY-NC-4.0`。判定四值：白名单 → `Allowed`；命中排除且**有非空 `license_review`** → `AllowedWithReview`；命中排除无评审 → `DeniedNeedsReview`；其余（含未知）→ `DeniedUnknown`。注册表构造时逐条 `check`，任一 `allowed()==false` 即**加载失败**（fail-closed），并可由 `license_violations()` 做诊断/门禁扫描。 | S1 §2.4/§9.9、S5、S7 + `license.rs`（≥3 个排除样例测试） |
| Q6 | 五维在 `ConsentCheck{task_id, backend, scope_hash, now}` + `TaskConsent{task_id, backend, scope_hash, budget_micros, ended_at, state}` 上校验：①任务匹配 ②后端匹配 ③范围哈希相等（硬拒绝，不裁剪后发送）④`state` ∈ {active}（ended/revoked/exhausted/expired 全拒）⑤`ended_at > now`（早于当前即失效）。云端总开关 `privacy.cloud_enabled` 单独前置：关闭即 `CloudDisabled`，0 次外联。 | S1 §4、S3 §8 + `consent.rs` 测试矩阵 |
| Q7 | 只有 `FailureClass::Transient`（超时、后端不可用/429/5xx、传输失败）计入熔断，并**每失败只记一次**；永久错误（4xx、Schema、授权/许可/预算/注册表、`FixtureMiss`、`Cancelled`、`Internal`）不重试也不计数。连续 3 次 → `Open` 60 s（0 次实际调用、快速失败）→ 放 1 次探测：成功 `Closed` 且清零，失败重新 `Open`。 | S1 §4/§9.4 + `breaker.rs` 表驱动测试、`service.rs` |
| Q8 | 费用单位微元（1 元 = 1 000 000）；公式 `ceil((未命中输入×miss价 + 命中输入×hit价 + 输出×out价) / 1e6)`，`u128` 中间量，向上取整保证不低记。价格按 `PricingTier::{Idle,Peak}` 取注册表两档；本地后端**恒 0**。调用前 `authorize`（任务上限，超限 0 次外联），调用后 `charge`（**失败也照实记账**：已产生云端费用不得抹掉）；用尽后 `mark_exhausted` → platform 置 `exhausted` 并持久化（见 §8-D2）。 | S1 §4/§9.7、S5（DeepSeek 价格）、S8 + `budget.rs` |
| Q9 | 在**注册表 extra_args** 关闭 thinking：`--chat-template-kwargs '{"enable_thinking":false}'`（Qwen3.5 判定条目）。依据是 `Qwen/Qwen3.5-4B` 的 `tokenizer_config.json` 里 `chat_template` 对 `enable_thinking is false` 有显式分支；不放提示词层，避免被调用方渲染差异绕过。 | S5 §7（§4.2/§7-D2）+ `models.registry.toml` |
| Q10 | 云端条目 `backend="openai_compat"`、`base_url="https://api.deepseek.com"`、`credentials_env="DEEPSEEK_API_KEY"`（只存变量名，密钥由平台密钥库/环境注入）；请求 `POST {base_url}/chat/completions`，`Authorization: Bearer`，`response_format={"type":"json_object"}`；价格按官方文档换算成微元/百万 token 两档（flash：未命中输入 1/2、命中 0.02/0.04、输出 4/8 元）。上下文 1 048 576、输出上限 393 216 写入注册表。 | S5 §8、S8 + `registry.rs`/`openai.rs`/`service.rs` |
| Q11 | 平台已够用，无需改 `eg-platform`：`Platform::open(DataDirs::resolve()?)`（测试用 `open_with(dirs, Arc::new(FixedClock::new(ms)))`）、`close()`；`config()/db()/audit()/consents()/ids()/clock()` 覆盖所需。落库路径：`audit.business_write(entry, |tx| { repo::insert_model_call(tx, &call); audit.append_in_tx(tx,..) })`（同一事务）；读回 `db.read(|conn| repo::list_model_calls(conn, task_id))`；授权：`consents.register(...)` 后 `validate_call(...)`，撤销/结束用 `list_by_task` 区分。**额度用尽**：平台 v2 已提供 `ConsentState::Exhausted` 与 `mark_exhausted`/`is_exhausted`/`record_spend`；本 crate 已接 `mark_exhausted`（持久化、重启后仍拒绝），`record_spend`（部分余额持久化）尚未接（§8-D2 余项）。 | S12 + `platform_adapter.rs`（真实 `Platform` 集成测试 4 个） |
| Q12 | 内置传输仅 `http://`，`https://` 明确返回 `TransportUnsupported`（`Permanent`，不计熔断），**不静默降级、不绕过授权**。v1 云端默认关（`privacy.cloud_enabled=false`）+ 逐任务授权，真正开启前需显式接入 TLS 传输（见 §7-R1）。本地回环不需要 TLS。 | S1 §2.2/§4 + `http.rs`（https 显式失败测试） |
| Q13 | 夹具记录与 `FixtureEntry` **同 schema**（`schema_version/key/model_id/model_version/prompt_schema_version/output_raw`），键同样是 `fixture_key(model_id, version, schema, prompt_bytes, input_bytes)`；索引额外给出 `prompt_sha256`/`input_sha256`/`fixture_key`/`output_sha256` 与语义标识。`FixtureCatalog` 装载时**重算键并复核全部 sha256**（不一致即拒绝），三级匹配 = 精确键 → 组合键（角色+提示词 sha256+资产 sha256，模型 id 可放宽）→ 语义标识（OCR 学生号/stem+页码，judge 学生号+题号）；负向样例只暴露不索引。 | S13 + `fixtures_wiring.rs`（真实仓库夹具 5 个集成测试） |

## 5. 实现契约细节（实现事实源）

### 5.1 依赖与传输

| 依赖 | 实测版本 | 用途 |
|---|---|---|
| `eg-domain` / `eg-platform` | workspace 0.1.0 | 领域类型（`Clock`/`ModelCall`/`EngineStamp`/`TaskConsent`/`BackendId`）/平台注入点 |
| `chrono` | 0.4.45 | 时间戳/`DateTime<Utc>` |
| `serde` + `serde_json` | 1.0.229 / 1.0.154 | 注册表 TOML→结构体、请求/响应 JSON、审计字段 |
| `toml` | 1.1.8+spec-1.1.0 | 注册表解析（`deny_unknown_fields` 严格模式） |
| `thiserror` | 2.0.21 | `InferenceError` |
| `tempfile` | 3.27.0（dev） | 测试临时目录 |
| 传输 | 自研最小 HTTP/1.1（`std::net::TcpStream`） | 不引入 HTTP crate；支持 `Content-Length` 与 `Transfer-Encoding: chunked` 响应、连接/读/写超时；**不支持** TLS/重定向/gzip（见 §7-R8） |

### 5.2 注册表、许可、生命周期

- 默认注册表：`crates/eg-inference/assets/models.registry.toml`（`DEFAULT_REGISTRY_TOML`，编译期内嵌，`ModelRegistry::embedded()`）；也可 `load(path)` 读 `CONFIG.md` 的 `inference.registry_path`。首启由 `app` 决定是否把内嵌默认落到数据目录（集成注意，见 §8-D5）。
- 记录字段（严格拒绝未知键）：`id`、`version`、`role`、`backend`、`quantization`、`context_length`、`license`、`license_review`、`max_concurrency`、`timeout_secs`、`prompt_schema_version`、`description`、`local{...}`、`cloud{...}`、`weights[]`。
- 校验：`id` 只允许小写字母/数字/点/连字符且长度 1..=64；`version`/`quantization`/`prompt_schema_version` 非空；角色-后端组合合法（云端只能 `openai_compat`、本地只能 `local_server`）；`context_length ∈ 512..=1_048_576`；本地 `weights` 非空且 sha256 为 64 hex；端口 `≥1024`；`startup_timeout_secs ∈ 1..=120`；`timeout_secs` 本地按角色（OCR 10–180、Judge/Router 5–60、Structurer 5–120），云端 1–300（flash 允许长输出）；同一 `serve_unit` 的端口/权重必须一致。
- 引擎指纹：`EngineStamp{role, backend, model_id, model_version, quantization, params_hash, prompt_hash}`；`params_hash = sha256(规范化 JSON 映射：quantization/context_length/timeout_secs/max_concurrency/prompt_schema_version/implementation/backend + 本地 binary/port/extra_args/chat_template + 云端 base_url/model_name)`，参数任何变化都换指纹、旧缓存失效。
- 生命周期状态：`Stopped → Starting → Ready → Degraded →（重启一次）Ready | Unavailable`；`Unavailable` 后不再自动重启（连续 3 次探活失败即进入），必须显式干预，避免无限重启。
- 启动指标：记录冷启动（首次拉起）与热启动/就绪耗时（`StartupMetrics`），对应规范 §9.1 双指标。

### 5.3 夹具

- `FixtureEntry{key, model_id, model_version, prompt_schema_version, output_raw}`；`FixtureStore::insert` 拒绝重复键与非 JSON 对象输出（`output_raw` 必须是单个 JSON 对象字符串）；`load_dir` 只读 `*.json`，坏文件显式报错（不跳过）。
- 回放：命中即返回录制字节（`ValidatedOutput` 仍走同一 Schema 校验，录制内容不合法 → `SchemaViolation`），引擎指纹 `backend=fixture`。录制模式仅开发构建可用（规范 §9.11），本 crate 不提供录制入口。
- **仓库级夹具接线（task-53，`src/fixture_disk.rs`）**：`fixtures/ocr/**`、`fixtures/judge/**` 是仓库提交的确定性 E2E 夹具（7 条 OCR + 8 条 judge + 3 条 negative，`fixtures/README.md`）。`FixtureCatalog::load(fixtures_root)` 读取 `ocr/index.json`、`judge/index.json`，装载记录并建三级索引：
  | 优先级 | 匹配键 | 用途 |
  |---|---|---|
  | 1（最高） | 精确字节键（记录里的 `key`） | 输入图/输入 JSON 与录制时逐字节相同（推荐给 egctl/app 验收） |
  | 2 | 组合键 `(角色, prompt sha256, 输入资产 sha256)`，可再限定模型 id/版本 | 调用方注册表模型 id 与录制 id 不同（跨 crate E2E） |
  | 3 | 语义标识：OCR `(学生号或文件 stem, 页码)`；judge `(学生号, 题号)` | capture 派生图/重新序列化的输入（字节不同） |
  未命中一律 `FixtureMiss`；同一级命中多条 → `Internal`（夹具集损坏，不猜）。装载期做**完整性校验**：记录/提示词/资产文件 sha256 与索引一致、用仓库内 `fixture_key` 从「提示词字节 + 输入字节 + 索引 backend 身份」重算键必须等于记录 `key` 与索引 `fixture_key`、`output_raw` 的 sha256 必须等于索引 `output_sha256`；任一不符拒绝加载（防「回放错页」/夹具漂移）。`negative/**` 只经 `negatives()` 暴露用于拒绝断言，**不进入回放索引**。`FixtureBackend::with_catalog(...)` 让精确键 miss 时按组合键回退；`fixture_backend(role)` 直接用索引 backend 身份构造精确回放后端。示例：`cargo run -p eg-inference --example fixtures_load -- fixtures`（打印角色/条数/三级命中，不打印正文）。

### 5.4 授权/熔断/预算/云端

- 单次调用时序（`InferenceService::complete`）：① 注册表解析（未登记即失败）→ ② 上下文超限检查（`prompt_tokens_estimate + max_tokens > context_length` 即 `ContextOverflow`，不静默截断）→ ③ 云端总开关 → ④ 任务授权五维 + 预算 `authorize` → ⑤ 熔断 `allow`（打开期 0 次调用）→ ⑥ 调用（超时）→ ⑦ 严格 Schema 校验（失败也按已产生费用 `charge`）→ ⑧ 记账/配额用尽标记/熔断 `record_success` → ⑨ 写 `ModelCall`、审计 → ⑩ 返回领域产出。
- `ModelCall`：本地/夹具有失败调用也照写（`error` 标记 `{"error":{"class","message"}}`）；云端**策略拦截**（开关关/无授权/范围不符/预算超限）写 **audit-only** `inference.policy_blocked` 而不写 `ModelCall`（领域不变量要求云端 `ModelCall` 必带 `consent_id`，见 §8-D1）。
- 审计事件：`inference.cloud_call`（云端成功/失败）、`inference.call_failed`（本地失败）、`inference.policy_blocked`（未发生的云端调用）。
- 健康报告：不改变状态、不发起模型调用；本地给 `HealthState` + 最近探测延迟，夹具恒 `Ready`，云端给「开/关」与可用性。

### 5.5 平台适配（只改 `platform_adapter.rs` 即可替换）

| 本 crate 注入点 | 平台实现 | 行为 |
|---|---|---|
| `ModelCallStore` | `PlatformModelCallStore` | `audit.business_write` → `repo::insert_model_call` + `audit.append_in_tx`（同一事务，失败即回滚） |
| `AuditSink` | `PlatformAuditSink` | 业务写事务内追加审计事件 |
| `IdSource` | `PlatformIdSource` | `ids().next_model_call_id()`（ULID 单调） |
| `ConsentGate` | `PlatformConsentGate` | `consents.is_valid` → 失败时 `list_by_task` 区分撤销/结束/额度用尽；范围不符映射 `ScopeMismatch`；预算由本 crate 的 `BudgetLedger` 执行，触顶回调 `mark_exhausted` 由 platform 持久化（重启后仍拒绝，与撤销/结束同走策略拦截） |
| `Clock` | `platform.clock()` | 所有时间只经注入时钟（测试用 `FixedClock`/`TestClock`） |
| 授权登记 | `register_cloud_consent` | 包 `register` + `validate_call` 的便捷入口 |

### 5.6 夹具接入（给 recognize / grade / egctl）

`FixtureCatalog` 是**只读**装载器，三种接入方式（按需选一，不要复制读取逻辑）：

1. **egctl / app 验收（推荐，逐字节精确）**
   ```text
   catalog = FixtureCatalog::load("fixtures")?            // 7 OCR + 8 judge
   backend = catalog.fixture_backend(FixtureRole::Ocr)?   // 用索引 backend 身份 + 精确键
   service = InferenceService::new(registry_with_fixture_entries, Backend::Fixture(backend), ...)
   ```
   注册表条目用 `catalog.backend_ids(role)` 的 `model_id/model_version/prompt_schema_version`，
   `backend = "fixture"`（`ModelRegistry::parse_str` 可直接解析）；请求的 `prompt` 用
   `prompt_source` 的字节、`input` 用 `asset_source` 的字节。
2. **recognize / grade 的适配层（字节不同）**：`resolve(&FixtureLookup::new().page(...))`
   或 `by_question(...)` 取 `output_raw`，再交给各自 Schema/语义校验；
   种子数据来自 `fixtures/README.md` §3 的 `grades-expected.json`/`index.json`。
   OCR 注意 §7-R11 的 Schema 形状差异。
3. **跨注册表模型 id（不想改注册表）**：`catalog.fixture_store(role)` + 自己的
   `FixtureBackend::new(自己的 id/版本/schema, store).with_catalog(Arc::new(catalog), role)`
   → 精确键 miss 时按组合键回退（`tests/fixtures_wiring.rs` 已端到端覆盖）。

负向样例：`catalog.negatives()` 只用于断言 `OcrOutput::validate`/`JudgeOutput::validate` 必须拒绝
（`fixtures/ocr/negative/*` 结构/条码块，`fixtures/judge/negative/*` match 却 0 分），**不得**当正向回放。

## 6. 版本实测与实现地图

| 项 | 实测 |
|---|---|
| 测试 | `cargo test -p eg-inference`：**114 passed / 0 failed**（unit 92 + `tests/fixtures_wiring.rs` 5 + `tests/platform_adapter.rs` 4 + `tests/service_flow.rs` 13；`tests/live_smoke.rs` 1 个默认 `#[ignore]`，见下行）；夹具「同输入 100 次字节一致」、1000 个畸形判定输出全拒、授权矩阵（无授权/撤销/过期/范围/预算）全部断言 **0 次外联**（桩传输计数） |
| 静态检查 | `cargo clippy -p eg-inference --all-targets -- -D warnings` 通过；`cargo fmt -p eg-inference --check` 通过 |
| 真实 serving 冒烟 | **已执行 1 次**（`tests/live_smoke.rs`，默认 `#[ignore]`，真实 llama.cpp 0.8.0 + 真实权重 `qwen3.5-4b-instruct` Q4）：冷启动 → `/health ok` **2 940 ms**（≤8 s）、单次判定 **1 791 ms**、usage 83 in / 29 out、输出 JSON 通过 `JudgeOutput` Schema；测试末尾 SIGTERM 停止并断言 `Stopped`，`pgrep llama-server` 确认**无本进程孤儿**。真实模型判定质量不在断言范围（只断言输出是单个 JSON 对象） |

| 文件 | 职责 | 单元测试 |
|---|---|---|
| `src/error.rs` | `FailureClass`/`InferenceError`（分类、是否计入熔断/可重试） | 3 |
| `src/registry.rs` | TOML 解析、字段校验、`serve_unit` 合并、引擎指纹/缓存键、`resolve_configured` | 16 |
| `src/license.rs` | 白名单/排除清单/评审引用 | 4 |
| `src/http.rs` | 最小 HTTP/1.1 传输（含 chunked），https 显式拒绝 | 4 |
| `src/openai.rs` | 请求体、响应解析、usage、严格 JSON 提取 | 6 |
| `src/schema.rs` | OCR/判定/路由/规则整理输出校验（严格、拒绝修复） | 10 |
| `src/fixture.rs` | 夹具键/存储/回放/损坏拒绝；`FixtureBackend` 组合键回退 | 6 |
| `src/fixture_disk.rs` | 仓库夹具目录加载：索引解析、完整性校验、三级匹配、negative 隔离 | 7（另 `tests/fixtures_wiring.rs` 5 个仓库夹具集成） |
| `examples/fixtures_load.rs` | 仓库夹具装载/三级匹配演示（示例，非测试） | — |
| `src/local.rs` | 端口分配、拉起/探活/重启/停止、serve 参数、健康 | 11 |
| `src/breaker.rs` | 熔断状态机 | 5 |
| `src/budget.rs` | 微元计价（向上取整）与任务账本 | 5 |
| `src/consent.rs` | 五维授权矩阵与内存门禁 | 7 |
| `src/call.rs` | `ModelCall`/审计记录器（成功/失败/仅审计） | 5 |
| `src/service.rs` | 编排（后端/云端/夹具）、Schema 校验、策略门禁 | 0（经集成测试覆盖） |
| `src/testing.rs` | 测试桩（时钟/传输/进程/存储/审计/id） | 3 |
| `src/platform_adapter.rs` | 平台注入点实现 | 0（真实 `Platform` 集成测试 4，含 exhausted 重启持久化） |
| `tests/live_smoke.rs` | 真实 sidecar 冒烟（默认忽略，人工验收） | 1（ignored） |

## 7. 风险与限制

- **R1 无 TLS（v1 已知限制）**：内置传输遇 `https://` 返回 `TransportUnsupported`。v1 云端默认关，因此不影响离线主链路；开启云端前必须显式接入 TLS 传输（需新增依赖与许可审查，属 Ask first）。
- **R2 预算持久化只覆盖「已用尽」**：platform v2 的 `ConsentState::Exhausted` + `mark_exhausted` 已接线——本次会话内一旦触顶，授权落库为 exhausted，**重启后仍拒绝**（集成测试覆盖「重启后仍不可用」）。仍余：`record_spend`（部分花费余额）未接，进程内 `BudgetLedger` 重启从 0 起算，未触顶的任务会重新获得完整预算；接法需给 `ConsentGate` 增 `record_spend` 或启动时回填余额（§8-D2 余项）。
- **R3 策略拦截不写 `ModelCall`**：领域 `ModelCall` 对云端强制 `consent_id`，无法合法记录「没发生的调用」；改用审计留痕，保证「无 ModelCall 就出分」不被伪造。见 §8-D1。
- **R4 `JudgeOutput` 不接受 `ungradable`**：规范 §5 的 `ungradable` 由调用方按「输入缺失/OCR 不可用」构造，模型不得自行宣布不可判。`partial` 映射到 `JudgedUnit::Choice` 时以 `satisfied/missing` 明细表达，非完全等价。
- **R5 云端缓存命中价依赖 provider 字段**：只解析 `usage.prompt_tokens_details.cached_tokens`（OpenAI 兼容子集）。若某 provider 只给顶层字段，则命中 token 记 0、按未命中价计费（**只会多记不会少记**，符合「不低记费用」）；开启云端前用一次真实响应校准。
- **R6 夹具与提示词强绑定**：键含 `prompt_schema_version` + 提示词全文哈希，提示词改一个字即 `FixtureMiss`（有意为之：禁止旧录制冒充新提示词结果）。
- **R7 生命周期为进程内单实例**：同一 `LocalServer` 句柄内串行管理；跨任务并发由调用方排队（注册表 `max_concurrency` 仅声明，未做客户端限流）。多实例/跨进程复用留待 `app` 集成阶段。
- **R8 HTTP 客户端子集**：不支持 TLS/重定向/gzip/HTTP2/流式 SSE（`stream=false` 固定）；llama-server 与 DeepSeek 非流式端点均在本子集内，实测记录见 S4。
- **R9 熔断/预算状态在内存**：进程重启清零。规范未要求跨重启持久化熔断；预算持久化由平台账本承担（当前未接，见 R2）。
- **R11 仓库 OCR 夹具与 `OcrOutput` 形状不一致（跨模块已知偏差）**：`fixtures/ocr/**` 按 `SPEC-recognize` 产出 `{schema_version,page,blocks[{kind,bbox,text,latex,confidence}]}`，而当前 `inference::OcrOutput` 只认 `{blocks:[{kind,text,confidence}]}`（`recognize.md` 已记录「inference OCR 输出为中间版本」）。因此**经 `InferenceService` + `ExpectedSchema::Ocr` 回放 OCR 夹具会 `SchemaViolation`**；judge 不受影响（已端到端验证）。接线建议：recognize 阶段用 `FixtureCatalog` 直接取字节并自行结构校验；待两边 Schema 对齐后再切回服务层（需改规范/两 crate，属 Ask first）。
- **R12 组合键回退必须显式挂接**：只有 `FixtureBackend::with_catalog` 才启用回退；且同角色「同提示词+同输入」重复会在装载期失败（`index_record` 去重），不做静默择一。
- **R13 语义标识依赖索引字段**：OCR 记录必须有 `page_no` + `submission_stem`（`student_no` 可空），judge 必须有 `student_no` + `question_ordinal`；缺字段或重复即拒绝加载。夹具增删后需重跑 `fixtures/generator/verify_fixtures.py`。
- **R14 索引路径相对 `fixtures/` 根**：`FixtureCatalog::load(root)` 要求 `<root>/ocr|judge/index.json` 且记录路径为 `ocr/...`/`judge/...` 相对路径；自定义根需保持同布局（`resolve_relative` 拒绝绝对路径与 `..`）。
- **R10 真实 serving 只做了 1 次冒烟**：权重不在仓库（由 `pull.sh` provisioning，约 1.8 GB/2.7 GB），默认测试集不依赖真实模型；已用桩覆盖全部失败模式与生命周期，真实链路（拉起 → `/health` → `/v1/chat/completions` → JSON 提取 → 停止）由 `tests/live_smoke.rs` 覆盖并可复跑（§9）。冒烟只证明本机单模型通路，不代表生产并发/长稳。

## 8. 与规范的偏差（待裁定，均不改变验收行为）

| # | 偏差 | 原因 | 建议 |
|---|---|---|---|
| D1 | 云端**策略拦截**不写 `ModelCall`，只写审计 `inference.policy_blocked` | `SPEC-domain.md` §8 要求云端 `ModelCall` 必带 `consent_id`；而「没发生的调用」没有合法 `consent_id`，伪造会违反领域不变量。规范 §4「失败也留痕」由审计满足 | 在 `SPEC-domain.md` §8 注明「未发生的云端调用以审计记录，不产生 ModelCall」 |
| D2 | **已处置（2026-11-03 / platform v2）**：`ConsentGate::mark_exhausted` 已接到 platform 的 `mark_exhausted`，`ConsentState::Exhausted` 映射为「授权不可用」→ 策略拦截 / 0 次外联 / 不伪造 `ModelCall`；集成测试覆盖「重启后仍拒绝」。**余项**：`record_spend`（部分余额）未接，进程内账本重启从 0 起算 | 平台 v2 补了 exhausted 态（已消解原缺口）；余额持久化需扩 `ConsentGate` 或启动回填 | 后续任务按需补 `record_spend`；不阻塞 v1（云端默认关 + 本地费用恒 0） |
| D3 | 内置传输不支持 TLS，`https://` 显式失败 | 不引入未审查的 TLS 依赖；v1 云端默认关 | 云端启用前置任务里单独评估 TLS crate 与许可 |
| D4 | `JudgeOutput` 仅 `match`/`partial`/`mismatch`，`ungradable` 不在模型输出 Schema 内 | 规范 §5 明确 `ungradable` 是「输入不可判」而非模型可宣布的结论 | 在 `SPEC-inference.md` §3.3 明确 `ungradable` 由调用方构造 |
| D5 | 默认注册表**编译期内嵌**，同时支持数据目录 `registry_path` 覆盖 | 首启不能依赖「文件已存在」；规范只约定路径未约定来源 | 在 `CONFIG.md` §14 注明「内嵌默认 + 数据目录可覆盖」 |

## 9. 复现命令

```bash
# 1. 静态检查与测试（不依赖真实权重）
cargo test -p eg-inference
cargo clippy -p eg-inference --all-targets -- -D warnings
cargo fmt -p eg-inference --check

# 2. 真实 serving 冒烟（1 次已执行；复跑需先下发权重）
brew install llama.cpp && llama-server --version          # 期望 0.8.0 build 11960+
./scripts/model/pull.sh judge
cargo test -p eg-inference --test live_smoke -- --ignored --nocapture

# 3. 脚本层端到端（人工验收）
./scripts/model/pull.sh ocr judge
./scripts/model/verify.sh
./scripts/model/serve.sh start ocr
./scripts/model/serve.sh start judge
./scripts/model/health.sh                                  # 期望 health: OK
curl -s http://127.0.0.1:8082/v1/chat/completions -H 'Content-Type: application/json' \
  -d '{"model":"qwen3.5-4b-instruct","messages":[{"role":"user","content":"ping"}],"max_tokens":8}'
./scripts/model/serve.sh stop all
```
