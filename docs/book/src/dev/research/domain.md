# 研究记录：domain（领域类型、值对象与学科契约，T04–T06）

> 状态：已完成 · 2026-11-02 · 对应规范：`SPEC-domain.md` v0.2.2、`SPEC.md` v0.2（§4/§7/§10）、`CAPABILITY-MAP.md` v0.2（§3/§5）、`CONFIG.md` v0.2.1
> 规则：检索优先（`AGENTS.md`）：先查官方文档与实测版本，再写实现；本页记录来源、结论、版本、风险与规范偏差。

## 1. 模块与范围

- 模块 id：`domain`（crate `eg-domain`）。
- 本次实现的能力（`tasks/todo.md` T04–T06）：
  1. **T04 值对象**：`Ulid` 与 id 新类型族、`MilliScore`、`Confidence`、`ContentHash`/`AssetId`、`Sha256Hex`、`Clock`、`Timestamp`、`BBox`、精确十进制 `Decimal`、四种 spec 中点名但未给表示的字符串 id（`SubjectId`/`ComparatorId`/`BackendId`/`TemplateId`/`CalibrationId`）与 `Rotation`/`ExifOrientation`、错误枚举（`ScoreError`/`ConfidenceError`/`IdError`/`HashError`/`DecimalError`/`DomainError`）。
  2. **T05 领域类型**：`SPEC-domain.md` §3–§8 全部类型（试卷/题目/模板、答案键/容差、评分标准/规则草稿/部分给分档位、提交/页面/区域、识别结果、判分记录/路由/判定/模式/证据/模型调用/提交级排除），含 serde（snake_case 标签）、不变量校验、insta 快照。
  3. **T06 学科契约**：`SubjectPack`/`Comparator`/`ComparatorVerdict`/`RawAnswer`/`StudentAnswer`/`AnswerPart`/`NormalizedAnswer`/`StepEvidence`/`RubricIssue` 等 + 可复用契约套件 `eg_domain::contract` + 被拒反例包（`tests/contract.rs`）。
- 测试与验证：单元测试内嵌各模块（lib 130 个测试函数，其中 14 条 proptest 性质 × 256 例）、`tests/contract.rs`（合规实现 + 16 个反例包）、`tests/paths.rs`（分支/错误/展示补覆盖）、`tests/snapshots.rs`（111 个序列化快照）；`cargo llvm-cov` 实测 TOTAL 行覆盖 95.38%。
- 明确不做：数据库表结构（`platform`）、HTTP/模型调用（`inference`）、UI 类型（`review`/`app`）、ULID 系统时钟生成器（`platform` T12）、持久化封装与迁移（`platform` T08）。

## 2. 问题清单

| # | 需要确认的外部行为/API/版本/限制 | 影响 |
|---|---|---|
| Q1 | serde 枚举「带标签形式」如何落地？单元枚举与带数据枚举的 JSON 形状分别是什么？ | 决定 §10 JSON 标签与全部枚举快照 |
| Q2 | serde_json 默认浮点解析是否 bit-exact 往返？`Confidence`/`BBox` 存 REAL 时会不会丢 ULP？ | 决定是否启用 `float_roundtrip` 特性与快照稳定性 |
| Q3 | `ulid` 3.0 的 API（构造/解析/格式化/比较/increment）与 serde 表示；默认特性引入了哪些依赖？ | 决定 id 族实现与依赖闭包 |
| Q4 | `chrono` 0.4 的 `DateTime<Utc>` serde 序列化格式是否满足「时间存 RFC3339 文本」？禁用 `clock` 特性后是否仍能解析/格式化？ | 决定时间类型与存储格式 |
| Q5 | `blake3` 1.8 的 API（`hash`/`to_hex`/`from_hex`、输出长度）与许可；`sha2` 0.11 的 `Digest::digest` 返回类型 | 决定 `ContentHash`/`Sha256Hex` 实现与许可门禁 |
| Q6 | thiserror 2 的 `#[error]`/`#[from]`/`transparent` 用法；是否与 `-D warnings` 兼容 | 决定错误枚举形态 |
| Q7 | insta 1.49 的 JSON 快照宏、快照文件命名与更新方式；proptest 1.12 的 `cases` 配置方式 | 决定快照与性质测试实现 |
| Q8 | Rust 稳定的 let-chains（`if let ... && ...`）可用版本与 edition 限制 | 决定校验代码写法（clippy `collapsible_if` 要求） |
| Q9 | 规范 v0.2.2 新增的部分给分类型与不变量：`PartialChoiceRule` 字段与 1–11 条不变量如何落入类型/校验？ | 决定 T05 范围与校验函数 |
| Q10 | 规范未给出、但字段必需的表示：`Timestamp`、`SubjectId`/`ComparatorId`/`BackendId`/`TemplateId`/`CalibrationId`、`JudgedUnit`、`RubricItemRef`、`Judgement::Partial.satisfied/missing` 元素类型、`RubricOrigin` 确认信息、`Annotation` 等 | 决定新增支撑类型与偏差登记 |

## 3. 来源

| # | URL | 文档版本/日期 | 类型 | 取用内容 |
|---|---|---|---|---|
| S1 | https://serde.rs/enum-representations.html | serde.rs 文档（2026-11-02 查阅） | 官方文档 | 枚举默认外部标签；单元枚举序列化为字符串；带数据枚举为 `{"variant": {...}}` |
| S2 | https://serde.rs/container-attrs.html | 同上 | 官方文档 | `#[serde(rename_all = "snake_case")]` 容器属性；字段名默认保持 Rust 标识符 |
| S3 | https://docs.rs/serde_json/1.0.154/serde_json/ | serde_json 1.0.154（2026-11-02 查阅） | 官方文档 + 源码 Cargo.toml | 特性 `float_roundtrip` 存在；启用后浮点解析按最短往返精度处理 |
| S4 | https://docs.rs/thiserror/2.0.21/thiserror/ | thiserror 2.0.21（2026-11-02 查阅） | 官方文档 | `derive(Error)`、`#[error("…")]`、`#[from]`、`transparent` |
| S5 | https://docs.rs/ulid/3.0.0/ulid/ | ulid 3.0.0（2026-11-02 查阅） | 官方文档 | `Ulid`、`from_string`、`from_parts`、`timestamp_ms`、`increment`、`Display` |
| S6 | https://github.com/dylanhart/ulid-rs | ulid-rs 仓库（2026-11-02 查阅；本地源码 `ulid-3.0.0/src/{lib,serde,base32}.rs`） | 源码 | `pub struct Ulid(pub u128)`；默认特性 `std` 拉入 `rand`（仅生成器用）；serde 默认 26 字符字符串；解析大小写不敏感、输出大写 |
| S7 | https://docs.rs/chrono/0.4.45/chrono/ | chrono 0.4.45（2026-11-02 查阅；本地源码 `src/datetime/serde.rs`） | 官方文档 + 源码 | `DateTime<Tz>` 的 `Serialize` 用 RFC3339（`SecondsFormat::AutoSi`、UTC 用 `Z`）；特性 `std`/`serde`；`clock` 特性不启用（不读系统时间） |
| S8 | https://docs.rs/blake3/1.8.7/blake3/ | blake3 1.8.7（2026-11-02 查阅；本地源码 `src/lib.rs`、`Cargo.toml`） | 官方文档 + 源码 | `hash(&[u8]) -> Hash`、`Hash::to_hex()`/`from_hex()`、32 字节输出；许可 `CC0-1.0 OR Apache-2.0 OR Apache-2.0 WITH LLVM-exception` |
| S9 | https://github.com/BLAKE3-team/BLAKE3/blob/master/test_vectors/test_vectors.json | BLAKE3 官方测试向量（2026-11-02 查阅） | 官方测试向量 | 空输入 BLAKE3-256 = `af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262`（单测断言用） |
| S10 | https://docs.rs/sha2/0.11.1/sha2/ | sha2 0.11.1（2026-11-02 查阅；本地源码 `digest-0.11.3`） | 官方文档 + 源码 | `Sha256::digest` 返回 `Output`（`hybrid_array::Array<u8, U32>`，可解引用为 `[u8]`）；FIPS 180-4 空输入向量 `e3b0c442…b855` |
| S11 | https://docs.rs/insta/1.49.0/insta/ | insta 1.49.0（2026-11-02 查阅；本地 `src/macros.rs`） | 官方文档 + 源码 | `assert_json_snapshot!(name, value)`、`assert_debug_snapshot!`；快照文件放 `tests/snapshots/`；`INSTA_UPDATE=always` 生成/更新 |
| S12 | https://docs.rs/proptest/1.12.0/proptest/ | proptest 1.12.0（2026-11-02 查阅） | 官方文档 | `proptest!` 宏与 `ProptestConfig { cases, .. }`；失败用例回归文件 `proptest-regressions/` |
| S13 | https://blog.rust-lang.org/2025/06/26/Rust-1.88.0/ | Rust 1.88.0 release（2026-11-02 查阅） | 官方 release | let-chains 稳定；仅 Rust 2024 edition 可用 |
| S14 | https://embarkstudios.github.io/cargo-deny/checks/licenses/cfg.html | cargo-deny 0.20 文档（见 `toolchain.md`） | 官方文档 | `[licenses].allow` 白名单；SPDX 表达式含 OR 时按满足性判定 |
| S15 | 仓库内规范 | `SPEC-domain.md` v0.2.2 / `SPEC-subject-math.md` v0.2.1 / `SPEC-rubric.md` v0.2.1 / `SPEC-grade.md` / `SPEC-align.md` | 项目规范 | 类型、字段、不变量、契约测试要求 |

## 4. 结论

| 问题 | 结论 | 依据 |
|---|---|---|
| Q1 | 枚举一律 `#[serde(rename_all = "snake_case")]`；单元变体序列化为字符串（如 `"partial_choice"`），带数据变体用 serde 默认外部标签（如 `{"concrete": {...}}` / `{"manual":"answer_key_missing"}`）。这正是规范 §10 的「带标签形式」，审计可读且便于模型输出校验。 | S1、S2；快照 `tests/snapshots/snapshots__*.snap` |
| Q2 | serde_json 默认浮点解析不保证 bit-exact（实测 `0.15468893240155127` 序列化后再解析差 1 ULP）。启用 `float_roundtrip` 后往返精确，`Confidence`/`BBox` 的快照与反序列化稳定。 | S3 + 实测（性质测试失败→启用特性→通过） |
| Q3 | `ulid` 3.0：`from_string` 严格 26 字符、大小写不敏感、输出大写；`increment()` 返回 `Result`（已到上限为 `Err`），可用于「单调不倒退」，但本 crate 只暴露 `checked_increment`（`Option`），生成器仍在 `platform`。默认 `std` 特性会引入 `rand`/`getrandom`（仅生成器路径使用）；`domain` 从不调用 `Ulid::new()`，因此不读系统时间。 | S5、S6；`cargo tree` 实测 |
| Q4 | `DateTime<Utc>` 的 serde 表示为 RFC3339 文本（`Z` 结尾、按需小数秒），满足 §10「时间存 RFC3339 文本」；`default-features=false, features=["std","serde"]` 不启用 `clock`，因此解析/格式化纯函数，不读系统时间；另提供 `format_rfc3339`/`parse_rfc3339` 统一入口。 | S7；`time.rs` 单测 |
| Q5 | `ContentHash` = BLAKE3-256（`blake3::hash` → 32 字节 → 64 位小写十六进制）；`Sha256Hex` = SHA-256（`Sha256::digest` → 32 字节）；解析严格 64 位小写十六进制。blake3 许可表达式含 Apache-2.0 分支，可满足 `deny.toml` 白名单（默认 OR 满足性），但其另含 `CC0-1.0`/`WITH LLVM-exception` 分支，需 T03 负责人确认 cargo-deny 实际判定（见风险 R1）。 | S8、S9、S10、S14 |
| Q6 | 错误枚举 `derive(Debug, Clone, PartialEq, Error)`；`#[error(transparent)]` 包装子错误、`#[from]` 自动转换、`#[error("字段 {field} 非法：{reason}")]` 承载中文文案。`DomainError::field_name()` 提供字段定位，满足 §11「带字段名与中文可读原因」。 | S4；`error.rs` 单测 |
| Q7 | 快照用 `assert_json_snapshot!("name", value)`（持久化/值类型）与 `assert_debug_snapshot!`（错误枚举，不可序列化但需锁定文案）；快照文件命名 `snapshots__<name>.snap`（insta 自动加 test 前缀）；性质测试统一 `ProptestConfig { cases: 256, .. }` ≥ 200 例/属性。 | S11、S12；实测 111 个快照文件、14 条性质 |
| Q8 | let-chains（`if let Some(x) = … && cond`）自 Rust 1.88 稳定且仅 edition 2024 可用；本项目 edition 2024 / rustc 1.99.1，可用，且 clippy `collapsible_if` 在 `-D warnings` 下要求此写法。 | S13；实测 clippy |
| Q9 | v0.2.2 部分给分落到：`ScoringMode::PartialChoice` + `Rubric.partial_choice: Option<PartialChoiceRule>`；`PartialChoiceRule{correct_option_count,tiers,wrong_selection_zero,source,source_note,total_milli}`、`PartialChoiceTier{selected_correct,points,label}`、`PartialChoiceSource{Official,TemplateData,TeacherCustom}`。校验覆盖不变量 5–11：题型必须多选、恰好 1 个全对评分项且其分值=满分、档位覆盖 1..=n-1 且升序唯一、每档 0<points<满分、单调不减、错选归零恒真、官方口径必须带 `source_note`、`total_milli`=Rubric.total、粒度整数倍（可选参数）。 | S15（§4 不变量 5–11）；`rubric.rs` 单测覆盖每个不变量 |
| Q10 | 规范未给表示的字段，按「最小必要支撑类型」实现并登记为解释项（§7 偏差表）：`Timestamp`=chrono UTC 别名；5 个字符串 id 新类型；`Decimal` 精确十进制；`Rotation`/`ExifOrientation` 值对象；`JudgedUnit{RubricItem,Choice}`（v0.2.2 的 `satisfied/missing` 在部分给分模式下装选项）；`RubricItemRef{Item,Ordinal}`（§9 未给类型而 `SPEC-rubric.md` §2.4 要求草稿序号）；`RubricOrigin` 用带确认信息的枚举。 | S15；`values.rs`/`decimal.rs`/`grade.rs`/`subject.rs`/`rubric.rs` |

## 5. 版本实测

| 依赖/工具 | 实测版本 | 命令 | 结果摘要 |
|---|---|---|---|
| Rust / cargo | 1.99.1 (58cf8f9c3 2026-10-13) / 1.99.1 (5f94df478 2026-08-27) | `rustc --version`、`cargo --version` | edition 2024 / resolver 3 可用；`rust-version = 1.97` 约束下 cargo 自动选「最高 1.97 兼容版本」 |
| serde | 1.0.229（features: derive） | `cargo add --package eg-domain serde --features derive` | 依赖 `serde_derive`、`proc-macro2`、`quote`、`syn`、`unicode-ident` |
| serde_json | 1.0.154（features: float_roundtrip） | `cargo add --package eg-domain serde_json --features float_roundtrip` | 默认 `std`；`float_roundtrip` 为纯编译期特性 |
| thiserror | 2.0.21 | `cargo add --package eg-domain thiserror` | 依赖 `thiserror-impl` |
| ulid | 3.0.0（features: serde） | `cargo add --package eg-domain ulid --features serde` | 默认 `std` → `rand 0.10.3`/`getrandom 0.4.3`（仅生成器路径） |
| chrono | 0.4.45（default-features=false, features: std, serde） | `cargo add --package eg-domain chrono --no-default-features --features std,serde` | 无 `clock`，不读系统时间；MSRV 1.62 |
| blake3 | 1.8.7 | `cargo add --package eg-domain blake3` | 32 字节输出；构建依赖 `cc`（仅构建期） |
| sha2 | 0.11.1 | `cargo add --package eg-domain sha2` | 走 `digest 0.11` / `hybrid-array 0.4` |
| insta（dev） | 1.49.0（features: json） | `cargo add --dev --package eg-domain insta --features json` | Rust 1.66+；快照写入 `tests/snapshots/` |
| proptest（dev） | 1.12.0 | `cargo add --dev --package eg-domain proptest` | Rust 1.88+；`ProptestConfig.cases` 支持 256 |
| 依赖闭包（normal） | 无文件/网络/数据库 crate | `cargo tree -p eg-domain --edges normal --prefix none` | `serde`/`serde_json`/`thiserror`/`ulid`/`chrono`/`blake3`/`sha2` + 其纯计算传递依赖（`rand`/`getrandom` 仅来自 `ulid` 默认生成器特性，未被 `domain` 调用） |
| 测试 | lib 130 + `contract.rs` 16 + `paths.rs` 13 + `snapshots.rs` 11 = 170 个测试函数；性质 14 条 × 256 例 = 3584 例；快照 111 个文件 | `cargo test -p eg-domain` | 全绿 |
| 静态检查 | clippy 0 警告 | `cargo clippy -p eg-domain --all-targets -- -D warnings` | 通过（含 `pedantic`） |
| 格式化 | rustfmt 1.10.0-stable | `cargo fmt -p eg-domain -- --check` | 通过 |
| 覆盖率 | cargo-llvm-cov 0.9.1（`cargo install cargo-llvm-cov --locked`） | `cargo llvm-cov -p eg-domain --summary-only` | TOTAL：行 95.38%（3573 行，缺 165）、区域 95.39%、函数 97.03%；单文件 `rubric.rs` 91.22%、`grade.rs` 93.54%、`paper.rs` 92.37%、`contract.rs` 93.09% 低于 95%，其余均 ≥95% |

## 6. 风险与限制

| 风险 | 影响 | 缓解/预案 |
|---|---|---|
| R1：`blake3` 许可表达式含 `CC0-1.0`/`Apache-2.0 WITH LLVM-exception` 分支 | `cargo deny check` 若按「每个分支都必须允许」判定会失败 | 其表达式为 OR，`Apache-2.0` 已在白名单（S14）；若实测被拒，请 T03 负责人在 `deny.toml` 增补并在研究记录登记理由（不得放宽为 allow-all） |
| R2：`serde_json` 默认浮点解析非 bit-exact | `Confidence`/`BBox` 往返丢 ULP，快照/重放不稳定 | 已在 crate 内启用 `float_roundtrip`；下游 crate 处理 REAL 字段时应共用 serde_json 配置 |
| R3：`ulid` 默认 `std` 特性引入 `rand`/`getrandom` | 依赖面变大；误用 `Ulid::new()` 会读系统时间 | 本 crate 不暴露 `Ulid::new()`，只提供 `from_u128`/`from_parts`/`parse`/`checked_increment`；生成器唯一入口在 `platform`（T12）；如后续需最小闭包，可评估 `default-features=false` |
| R4：聚合类型字段公开 + `validate()` 分离 | 直接构造可绕过不变量，直到调用 `validate` | 值对象（id/分数/置信度/哈希/坐标/十进制/枚举）在构造器与 `Deserialize` 双重校验；聚合类型反序列化后必须调用 `validate`（文档已写明）；导入期门禁由 `rubric`/`grade` 调用 |
| R5：快照 111 个 | 字段变更会同时失败大量快照 | 这正是 §14 成功标准 2 的目的；变更时用 `INSTA_UPDATE=always` 生成后**人工审核 diff** 再提交 |
| R6：单文件覆盖率不足 | §14 要求行覆盖 ≥95%；TOTAL 已 95.38%，但 `rubric.rs`（91.22%）、`grade.rs`（93.54%）、`paper.rs`（92.37%）、`contract.rs`（93.09%）仍低于该线 | 已在 `tests/paths.rs` 补齐大量分支；剩余缺口主要是部分给分/草稿/判分门禁的组合分支，建议后续（T15/T17/T18 实现时）在其模块测试中继续覆盖，或为 domain 增补组合用例；CI 用 `cargo llvm-cov --summary-only` 守护 TOTAL 不下降 |
| R7：`chrono` 反序列化接受任意时区偏移并归一为 UTC | 手写 JSON 用 `+08:00` 会按 UTC 等价时间载入 | 存储/导出统一用 `format_rfc3339`（`Z` 形式）；快照锁定 `Z` 形式 |
| R8：`Confidence`/`BBox` 用 f64 | 极端浮点值（如次正规数）跨平台打印可能有差异 | 领域语义不依赖位级相等（置信度只比较大小、坐标只做范围校验）；序列化固定由 serde_json + float_roundtrip 处理 |
| R9：`DomainError` 不派生 `Eq`（含 f64） | 测试只能用 `PartialEq`/`matches!` | 可接受；错误比较按字段/`field_name` 断言 |
| R10：`Judgement::Partial.satisfied/missing` 语义双关（评分项 vs 选项） | 下游误用类型 | 用带标签 `JudgedUnit` 区分，并在文档与快照中给出两种形态 |

## 7. 与规范的偏差

> 原则：规范优先。以下均为「规范未给表示/未给字段类型」处的实现解释，或为满足 §10 的落地方式；未修改任何规范文件。**请 Lead 裁定是否需要更新 `SPEC-domain.md` §15 裁定表或 §10 措辞。**

| 规范位置 | 偏差/解释 | 处理（改规范/改实现） | 审批状态 |
|---|---|---|---|
| §2 基础值对象 | 规范只列 `Ulid`、id 族、`ContentHash`、`AssetId`、`MilliScore`、`Confidence`、`Sha256Hex`、`Clock`；实现新增支撑类型 `Timestamp`（chrono UTC 别名）、`Decimal`（精确十进制，用于 `AnswerValue::Number` 与 `Tolerance.absolute`）、`Rotation`、`ExifOrientation` | 实现解释（§4/§6 字段所需）；未改规范。若 Lead 要求，可把 `Decimal`/`Rotation` 等列入 §15.1 | 待裁定 |
| §3/§9 字段类型 | `subject`（`SubjectId`）、`comparators`（比较器 id）、`BackendId`、模板 `template_id`、`calibration_id` 规范点名但未给表示；实现为「非空字符串新类型」 | 实现解释；`SubjectId::math()` 固定 `"math"` | 待裁定 |
| §8 `ComparatorTrace.id` | §2 id 族未列 `ComparatorTraceId`，但 §8 要求该实体有 `id` 且 `EvidenceRef` 需引用 | 实现新增 `ComparatorTraceId` ULID 新类型 | 待裁定（建议补入 §2/§15.1） |
| §10 `schema_version` | 规范要求「每类持久化对象带 `schema_version`」，但 §3–§8 字段表逐字列出且不含该字段 | 用 `SchemaVersioned::SCHEMA_VERSION` 关联常量 + 存储层 JSON 外层封装实现，字段表保持逐字 | 待裁定（若要求字段，需改 §3–§8 字段表） |
| §4 `RubricOrigin` | 规范只给三种来源，未给字段；但要求记录确认人与确认时间、模板/模型可追溯 | 实现为带数据的枚举：三种来源各自携带 `confirmed_by`+`confirmed_at`，模板携带 `template_id`+`template_version`，模型携带 `model_call_id` | 实现解释 |
| §4 `Rubric.partial_choice`（v0.2.2） | 规范新增字段与 `PartialChoiceRule` 形状；本实现逐字对齐，`correct_option_count`/`selected_correct` 用 `u32`，`tiers` 用列表 | 无偏差（按 v0.2.2 实现） | — |
| §4 不变量 10「错选归零」 | `wrong_selection_zero` 恒真：类型允许 `false`（为可审计读出），校验返回错误 | 与规范一致（规范要求「写入数据只为审计可读，不得设为假」） | — |
| §5 `RubricDraft.id` | §2 未定义 `DraftId`；草稿转生效规则沿用同一 id | 实现用 `RubricId` | 实现解释 |
| §5 `RubricDraftItem.depends_on` | 「引用草稿内序号」未说明基址 | 实现定为 **1 起**（与 `Question.ordinal`/`StepMatch` 序号风格一致），越界/成环类型化报错 | 实现解释（建议写入规范） |
| §8 `Judgement::Partial` | v0.2.2 明确 `satisfied`/`missing` 在部分给分模式装「选项」，但字段类型未给 | 实现 `Vec<JudgedUnit>`：`JudgedUnit::RubricItem(id)` 或 `JudgedUnit::Choice(label)` | 待裁定（建议补 §15.1） |
| §8 `JudgeOutcome.calibration_id` | 类型未给 | 实现 `Option<CalibrationId>`（非空字符串新类型） | 实现解释 |
| §9 `RubricIssue.item` | §9 只写「`item`（可空）」；`SPEC-rubric.md` §2.4 要求「可空的评分项序号」 | 实现 `Option<RubricItemRef>`（`Item(RubricItemId)` / `Ordinal(u32)`），两种上下文都不丢信息 | 待裁定（建议补 §15.1） |
| §9 `AnswerPart` | §9 字段未标可空 | 实现 `segment_id: SegmentId`、`recognition_id: RecognitionId` 均非空；人工修正的「主识别 id 为空」落在 `StudentAnswer.recognition_id` | 实现解释 |
| §9 `SubjectPack` 方法签名 | 规范只给能力清单，未给具体签名 | 见 `subject.rs`：`id/display_name/capabilities/normalize/comparator_chain/step_evidence/judge_profile_id/validate_question`；比较器链返回 `Vec<Box<dyn Comparator>>`（按序尝试），静态校验返回 `Vec<RubricIssue>` | 待 Lead 确认（§13「改 `SubjectPack` 契约」属 Ask first） |
| §8 `GradeRecord` 不变量 2 | 自动接受还依赖模式阈值与「模型已校准」，后者在 `JudgeOutcome` 而非 `GradeRecord` | `GradeRecord::validate()` 只校验结构可判定部分；`auto_accept_blockers(threshold)` 给出记录内可见原因；校准由 `JudgeOutcome::is_calibrated()` + `grade` 模块合并 | 实现解释（`grade` T18 必须合并两者） |
| §8 不变量 5 | 「快速模式记录必须 `sampled` 可辨」 | 实现额外要求「标准模式不得 `sampled=true`」（`SampledInStandardMode`），作为快速模式抽检标记的强约束 | 实现解释（若过于严格可放宽） |
| §8 `ModelCall` | `cost_micros`/`latency_ms` 未给类型；`output` 未给类型 | 实现 `u64`/`u64`；`output: serde_json::Value`（已通过 Schema 校验但不在 domain 重复 strict 定义） | 实现解释 |
| §11 构造失败语义 | 「构造失败一律返回类型化错误」 | 值对象用私有字段 + 校验构造器；聚合类型用公开字段 + `validate()`（便于 serde 与遍历），文档与测试要求使用前校验 | 实现解释 |
| §14 覆盖率 | 行覆盖 ≥95% | 已实测 TOTAL 行 95.38%（≥95%）；单文件 `rubric.rs`/`grade.rs`/`paper.rs`/`contract.rs` 仍低于 95%（见风险 R6），后续迭代补齐 | 部分达成（TOTAL 达标） |

## 8. 复现命令

在仓库根执行（新 shell 先 `source "$HOME/.cargo/env"`）：

- 单元/集成测试：`cargo test -p eg-domain`
- 静态检查：`cargo clippy -p eg-domain --all-targets -- -D warnings`
- 格式化：`cargo fmt -p eg-domain -- --check`
- 快照生成/更新（**必须人工审核 diff**）：`INSTA_UPDATE=always cargo test -p eg-domain --test snapshots`
- 依赖闭包（核对零 IO/DB/HTTP）：`cargo tree -p eg-domain --edges normal --prefix none`
- 覆盖率（可选，需先 `cargo install cargo-llvm-cov`）：`cargo llvm-cov -p eg-domain --summary-only`
