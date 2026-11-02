# 研究记录：rubric（评分规则草稿、全国卷模板库、规则整理、静态校验与版本化，T15/T25）

> 状态：已完成 · 2026-11-02 · 对应规范：`SPEC-rubric.md` v0.2.2、`SPEC-inference.md` v0.2（structurer 角色）、`SPEC-domain.md` v0.2.2（§3–§5、§15.2）、`CONFIG.md` v0.2.3（§7）、`CAPABILITY-MAP.md` v0.2（§3）
> 规则：检索优先（`AGENTS.md` §2/§4）。本页记录来源、结论、版本实测、风险与规范偏差。

## 1. 模块与范围

- 模块 id：`rubric`（crate `eg-rubric`），任务 T15（无模型部分）+ T25（规则整理与模板数据对齐）。
- T15 实现的能力：
  1. **草稿服务与四状态机**：手写直译 / 导入结构化项 / 标准模板套用 / 模型整理；草稿 → 待确认 → 已确认（终态）/ 已拒绝（终态）；终态不可编辑，编辑回「草稿」。
  2. **全国卷模板包读取**：四套数据（`new-curriculum-1/2` active、`national-jia/yi` retired）；加载期文件级校验（题号连续、题量、各题满分合计 = 总分、骨架存在、比例可精确折算、依赖无环、合并选做只计一次）；套用按题目满分精确折算千分比，不四舍五入。
  3. **自定义模板**：单文件导入 + 合并（只追加不改写），与内置包同一套加载校验。
  4. **静态校验 20 条**：基础 11 条（R01 分值合计、R02 分值粒度、R03 正分值、R04 全对才给分只允许 1 项、R05 依赖存在、R06 依赖无环、R07 判据非空/非占位、R08 题型受学科包支持、R09 分步给分前置语义（警告）、R10 判据重复（警告）、R11 模板来源必须带模板引用）+ 部分给分专项 9 条（P01 模式互斥、P02 档位覆盖不跳档、P03 档位分值范围与单调、P04 档位粒度、P05 档位来源可追溯、P06 总开关关闭、P07 仅限多选、P08 恰好一个全对项且分值 = 满分、P09 错选归零恒为真）。
  5. **确认与版本化**：确认前重跑全部校验、要求「待确认」、乐观并发版本检查、生成 `Rubric`（评分项 id 补齐、依赖序号映射为 id）、版本递增、终态与审计；拒绝写理由与审计；确认恒开、无自动生效入口。
  6. **端口 + 内存实现**：`RubricStore`/`RubricAudit`/`RubricIdSource` 三端口 + 内存适配器（测试与 platform 适配前临时装配）。
- T25 实现的能力：
  7. **模板数据对齐**（`assets/templates/**`）：现行卷两套补官方多选部分给分档位（`partial_choice`）、`provenance.basis` 统一到 README 枚举、现行卷版本升 `2026.2`、总分/题量/档位完整性校验测试；历史卷数据仅核对不改。
  8. **规则整理（structurer）**：本模块渲染提示词并计算哈希、定义严格输出 Schema v1（拒绝未知字段）、调用端口（`StructurerPort`，由 `app` 用 `eg-inference` 适配）、结构不合法时**机械修复至多 1 次**、通过校验后只组装 `RubricDraft`（来源 ModelStructured，带 `model_call_id`）并跑静态校验；模型绝不直接生成生效规则，畸形输出拒绝且不生成草稿。
- 测试与验证：`cargo test -p eg-rubric` **86 个测试全绿**（lib 55 + 集成 31：手写 7、反例 3、部分给分 4、模板 8、规则整理 9）；夹具覆盖 10 条手写规则、21 条反例（恰好覆盖全部 20 个校验码）、5 条部分给分变体、4 套自定义模板、11 份模型输出（含 8 类畸形）。
- 明确不做：判分（`grade`）、模型调用实现/任务授权/预算（`inference`，由 `app` 适配）、规则编辑界面（`app`）、SQL 表结构与真实审计链（`platform`，本任务保持端口 + 内存实现，platform 迁移缺口另立 task-37）、修改历史卷数据与官方评分细则。

## 2. 问题清单

| # | 需要确认的外部行为/API/版本/限制 | 影响 |
|---|---|---|
| Q1 | TOML 解析 crate 的稳定版本与 `from_str`/serde 行为（未知字段、可选字段、中文枚举字符串） | 模板读取层与「只追加」兼容策略 |
| Q2 | serde 容器属性与枚举表示：`rename_all`、`default`、外部标签 | 夹具/审计 JSON 形状 |
| Q3 | `thiserror` 2 的 derive 用法与 `-D warnings` 兼容 | 错误枚举形态 |
| Q4 | `eg-domain` v0.2.2 草稿/规则/部分给分类型是否齐备；草稿缺哪些字段 | 是否需要模块自有伴随记录 |
| Q5 | 内置模板数据缺哪些字段；现行卷多选是 2 个还是 3 个正确选项；官方档位数值 | 决定读取兼容、缺口报告与 T25 数据补齐方式 |
| Q6 | 模板骨架千分比在任意满分下是否都能精确折算 | 套用失败语义（显式错误 vs 静默取整） |
| Q7 | 领域不变量（`RubricDraft::ensure_confirmable`/`Rubric::validate`）与静态校验如何分工 | 确认链路双重门禁 |
| Q8 | 版本语义：规范以 `Paper.rubric_version` 为基准，端口如何表达 CAS | 存储端口形状与平台适配 |
| Q9 | `eg-platform` 能否支撑真实仓储/审计适配 | T15 用内存实现还是真实接入（结论：不能无损适配，见 R3） |
| Q10 | 部分给分领域不变量（2/3 选项、覆盖、单调、粒度、来源、错选归零）落在哪层 | P01–P09 与 `PartialChoiceRule::validate` 的边界 |
| Q11 | 编辑/确认/拒绝的并发与幂等语义 | 状态机与错误码 |
| Q12 | 界面/审计是否要求本模块类型可序列化 | serde 派生范围 |
| Q13 | `eg-inference` 的 structurer 角色当前接口（`ExpectedSchema`/`ValidatedOutput`/`StructurerOutput`）与「本模块渲染提示词 + 哈希」的分工如何落地 | 决定 trait 端口设计、Schema 字段名与适配层位置 |
| Q14 | 规则整理输出的严格 Schema 字段名与上限（规范 §2.3 vs inference 已实现形状） | 决定严格解析器字段与适配映射（D9） |
| Q15 | 机械修复次数与失败语义（结构校验归属 inference） | 决定 1 次修复流程与错误结构 |
| Q16 | 模型整理结果不得直接生效、不得覆盖原文 | 决定服务层只返回草稿 + 审计字段 |
| Q17 | 现行卷多选档位写入后，模板版本如何演进（只追加不改写） | 版本升 `2026.2` 还是新建快照 |
| Q18 | `provenance.basis = official-analysis` 与 README 枚举不一致如何处理 | 改数据还是只告警（T25 决定改数据） |

## 3. 来源

> 说明：本机网络策略拒绝 `docs.rs`（域名解析到非公网地址），因此**结论以本地 vendored 源码复核**为准；官方文档 URL 仅登记查阅目标，不作为唯一依据（`AGENTS.md` §3 禁止只凭记忆/二手资料）。

| # | URL / 本地路径 | 文档版本/日期 | 类型 | 取用内容 |
|---|---|---|---|---|
| S1 | https://docs.rs/toml/1.1.8/toml/fn.from_str.html ；本地 `~/.cargo/registry/src/*/toml-1.1.8+spec-1.1.0/src/de/mod.rs:72`、`src/lib.rs:184` | toml 1.1.8+spec-1.1.0（2026-11-02 实测） | 官方文档 + 源码 | `from_str<'de, T: Deserialize<'de>>(&'de str) -> Result<T, Error>`；TOML 1.1.0 |
| S2 | https://serde.rs/container-attrs.html ；本地 `serde-1.0.229`/`serde_derive` | serde 1.0.229（2026-11-02 实测） | 官方文档 + 源码 | `rename_all`、`default`；未知字段默认忽略（未加 `deny_unknown_fields`）→ 模板向后兼容；本模块规则整理输出**显式** `deny_unknown_fields` |
| S3 | https://docs.rs/serde_json/1.0.154/serde_json/ ；本地 `serde_json-1.0.154` | serde_json 1.0.154（2026-11-02 实测） | 官方文档 + 源码 | 模型原始 JSON 严格解析、审计哈希载荷 |
| S4 | https://docs.rs/thiserror/2.0.21/thiserror/ ；本地 `thiserror-2.0.21` | thiserror 2.0.21（2026-11-02 实测） | 官方文档 + 源码 | `#[derive(Error)]`、`#[error]`、`#[from]`；与 `-D warnings` 兼容 |
| S5 | 工具链实测 `rustc --version` / `cargo --version` | rustc 1.99.1 / cargo 1.99.1（2026-10-13 / 2026-08-27） | 工具链 | edition 2024、let-chains、`is_none_or`/`is_multiple_of` |
| S6 | 仓库 `SPEC-rubric.md` | v0.2.2 · 2026-11-02 | 项目规范 | §2.1–§2.6、§2.4 校验表、§2.5 确认、§3.3 版本化、§3.4/§3.5 模板与整理、§5 审计、§6 验收 |
| S7 | 仓库 `SPEC-inference.md` | v0.2 · 2026-11-02 | 项目规范 | §2.3 规则整理角色、§2.4 调用时序（③ 提示词由 rubric 渲染、⑥ Schema 校验、⑦ ModelCall）、§3 契约（`RubricStructureRequest`/`RubricStructureOutput` 仅草稿） |
| S8 | 仓库 `SPEC-domain.md` | v0.2.2 | 项目规范 | §3 类型、§4 部分给分不变量、§5 草稿/确认不变量、§15.2 模块自有类型批准 |
| S9 | 仓库 `CONFIG.md` | v0.2.3 · 2026-11-02 | 项目规范 | §7 `rubric.min_score_granularity`/`partial_choice_enabled`/`auto_validate`/`allow_custom_template`/`template_snapshot`；`require_confirmation` 为不变量 |
| S10 | 仓库 `assets/templates/README.md` | v0.2 · 2026-11-02（本次更新） | 模板数据说明 | §2 四套一览（含版本）、§4 字段（新增 `partial_choice`/`tiers`）、§6 题型骨架对应、§7 版本与状态、§10 T1–T9 自检、§11 已知限制 |
| S11 | 仓库 `assets/templates/national/*/template.toml`（读取实测） | 2026-11-02 | 模板数据 | 四套：Ⅰ/Ⅱ `active` v`2026.2`（19 题、150 分、各 3 道多选、`partial_choice` 档位、basis `public-analysis`）；甲 `retired`/`2026` v`2025.1`、乙 `retired`/`2024` v`2023.1`（22 题作答、无多选） |
| S12 | 仓库 `crates/eg-domain/src/{rubric.rs,paper.rs,score.rs,ids.rs,evidence.rs}` | 本地源码（2026-11-02） | 源码 | `RubricDraft` 无 `mode`/`partial_choice`/模板引用；`PartialChoiceRule`/`Tier`/`Source` 字段；`MilliScore` 序列化为 i64 毫分；`ModelCallId::from_ulid`；`Sha256Hex::of` |
| S13 | 仓库 `crates/eg-inference/src/{schema.rs,service.rs,types.rs}` | 本地源码（2026-11-02，并行实现，`cargo check -p eg-inference` 通过） | 源码 | `ExpectedSchema::Structurer`、`ValidatedOutput::Structurer(StructurerOutput)`、`StructurerOutput::validate` 严格 Schema（字段 `title/criteria/max_score_milli/required/depends`，`depends` 0 起、依赖无环校验）、`InferenceService::complete(CompletionRequest)` |
| S14 | 仓库 `crates/eg-platform/src/{repo.rs,audit.rs,platform.rs}` | 本地源码（2026-11-02） | 源码 | `repo::insert_rubric_draft(&RubricDraft)`/`get_rubric_draft`、`insert_rubric_revision(paper_id, rubric_version, &Rubric)`；`audit.business_write(NewAuditEntry, |tx| ...)`；`Paper.rubric_version`（缺口见 R3，另立 task-37） |
| S15 | 仓库 `AGENTS.md` | v0.2 · 2026-11-02 | 项目纪律 | §2 检索协议、§4 研究记录模板、§5 准入与验收 |
| S16 | `docs/dev/research/{domain,platform,templates}.md` | 2026-11-02 | 上游研究记录 | `eg-domain` 类型与不变量、`eg-platform` 能力与限制、模板数据来源分级 |
| S17 | TOML 1.1.0 spec（https://toml.io/en/v1.1.0） | v1.1.0 | 官方规范 | `toml` 1.x 面向 TOML 1.1.0；本模块只用基本类型/数组/表 |

## 4. 结论

| 问题 | 结论 | 依据 |
|---|---|---|
| Q1 | `toml = "1"`（锁定 1.1.8+spec-1.1.0）。`toml::from_str::<RawTemplate>` 整份解析；未知字段默认忽略、可选字段 `#[serde(default)]`，模板只追加字段不破坏旧 loader；中文枚举在 loader 显式映射，未知取值错误/告警而非猜测。 | S1、S2、S6、S10 |
| Q2 | 夹具可直接反序列化为 `eg-domain` 类型：枚举 `rename_all = "snake_case"`，`MilliScore` 为整数毫分，`RubricDraftItem` 与夹具一一对应。 | S2、S3、S12 |
| Q3 | `RubricError` 用 thiserror 2 派生（`#[from]`/`transparent`/中文文案 + 字段）；`clippy -D warnings` 全绿。 | S4、S5 + 实测 |
| Q4 | `RubricDraft` 缺 `mode`/`partial_choice`/模板引用，但校验/确认/审计都需要 → 模块自有 `StoredDraft` 伴随记录（`draft` + `question_kind` + `max_score` + `mode` + `partial_choice` + `template`）。偏差 D1。 | S8、S12、S16 |
| Q5 | T15 时四套模板已有 `status`/`retired_since`，但**缺**多选部分给分档位、`basis = official-analysis` 超枚举；T25 按 README §11 官方口径补齐：Ⅰ/Ⅱ 多选默认 3 个正确选项（选对 1 个 2 分、2 个 4 分、错选 0 分），另给 2 选项备选骨架（选对 1 个 3 分）；`basis` 统一为 `public-analysis`。 | S10、S11、S16 + 数据实测 |
| Q6 | 折算 `points = 满分 × ratio_permille / 1000` 必须整除；加载期即校验，不整除 → `TemplateInvalid`（不静默取整）；档位用 `points_milli` 直接给毫分（6 分题 2/4 分精确）。 | S6 §2.2、S10 §4 + 实现 |
| Q7 | 双门禁：本模块 20 条静态校验 → 领域 `ensure_confirmable`/`Rubric::validate_with_granularity`；确认时重跑静态校验（不信任历史 validation）。 | S6 §2.5、S8 §5 + 集成测试 |
| Q8 | 规范以试卷级 `Paper.rubric_version` 为版本基准；T15 端口为题目级 CAS（`Rubric` 无 `paper_id`）。偏差 D2；platform 迁移（task-37）时统一。 | S6 §3.3、S12、S14 |
| Q9 | `eg-platform` 不足以无损适配端口：`rubric_draft` 只存领域 `RubricDraft`（无伴随字段）、无按题目查询/按题目生效规则、版本为 paper 级、`NewAuditEntry` 无 `rubric_version`/模板引用；结论保持 trait + 内存实现，platform 迁移另立 task-37。 | S14 + 源码实测（R3） |
| Q10 | 领域 `PartialChoiceRule::validate` 管档位自身；本模块 P01–P09 管草稿上下文（模式/满分/总开关/题型/全对项/来源）；确认时领域校验再防御一次。 | S8 §4 + `validate.rs` |
| Q11 | 重复确认 → `DraftWrongStatus` 且不产生新版本；版本冲突 → `VersionConflict` 且不写版本；编辑终态 → `DraftNotEditable`；拒绝空理由 → `Config`。 | S6 §2.5 + 测试 |
| Q12 | `StoredDraft`/`TemplateRef`/`RubricAuditEvent`/`RubricAction`/`TemplateVersion`/`TemplateStatus` 派生 serde；`RubricDraft` 由领域层序列化。 | S3、S12 |
| Q13 | `eg-inference` 已提供 structurer Schema 与 `ExpectedSchema::Structurer`，但 `InferenceService::complete` 需要任务 id/模型 id/预算/授权等组合根上下文；按 SPEC-inference §2.4「提示词由 rubric 渲染」，本模块**定义 `StructurerPort` trait + 渲染提示词/哈希**，`app` 用 inference 适配（TODO(T25)），依赖方向 rubric → trait ← app → inference，不把模型/授权耦合进 crate。 | S7、S13 + 任务约束 |
| Q14 | 严格 Schema v1 采用规范 §2.3 字段：`{"items":[{"criterion","points","required","depends_on"}]}`，`deny_unknown_fields`，`depends_on` 1 起、1..=项数、无自依赖/成环，item 1..=50、criterion ≤500 字且禁模板占位标记、points 1..=1,000,000 毫分、合计 ≤2,000,000。与 inference 当前 `StructurerOutput` 字段名/下标不同（`title/criteria/max_score_milli/depends` 0 起）→ 适配层需映射（D9）。 | S6 §2.3、S13 + `parse_structurer_output` |
| Q15 | 首次输出不合法 → 把违规列表反馈给同一角色机械修复**至多 1 次**；两次均不合法 → `ModelOutputRejected`（含模型调用 id/提示词哈希/违规列表/尝试次数），不生成草稿；端口调用本身不重试。 | S6 §3.4、S7 §2.4 + 测试 |
| Q16 | `create_draft_from_model` 只返回 `StoredDraft`（来源 ModelStructured + `model_call_id`），落库后仍需 `submit_for_confirmation` + `confirm`；审计 `ModelStructured` 事件记提示词哈希/模型调用 id/尝试次数，原文只在草稿 `raw_text`（日志只记长度与哈希）。 | S6 §3.4/§5、S7 §2.4 + 测试 |
| Q17 | 数据变更按 README §7「只追加不改写」升修订号：`2026.1 → 2026.2`（`template_id`/`set_id` 不变，历史卷保持 `2025.1`/`2023.1`）；`snapshot_label` 注明修订。 | S10 §7 + 数据 |
| Q18 | T25 直接改数据：`official-analysis → public-analysis`（Ⅰ/Ⅱ 逐题分值含推定，按 README 枚举选 `public-analysis` 更诚实）；loader 的 TW02 告警随数据修正消失，TW01 仅对「自定义模板含多选但无档位」保留（测试覆盖）。 | S10 §9、S11 + 测试 |

## 5. 版本实测

| 依赖/工具 | 实测版本 | 命令 | 结果摘要 |
|---|---|---|---|
| Rust 工具链 | rustc 1.99.1（58cf8f9c3 2026-10-13）、cargo 1.99.1（5f94df478 2026-08-27） | `rustc --version`、`cargo --version` | edition 2024；let-chains、`is_none_or` 等可用 |
| `toml` | 1.1.8+spec-1.1.0 | `Cargo.lock` + 本地 registry 源码 | `from_str` 返回 `Result<T, toml::de::Error>`；未知字段忽略 |
| `serde` / `serde_json` | 1.0.229 / 1.0.154 | 同上 | derive + 严格 JSON 解析（`deny_unknown_fields`）可用 |
| `thiserror` | 2.0.21 | 同上 | derive(Error) 与 `-D warnings` 兼容 |
| `eg-inference` | 本地并行实现（`cargo check -p eg-inference` 通过） | `cargo check -p eg-inference` | structurer Schema/服务接口存在；本 crate 暂不依赖（trait + TODO） |
| 测试 | `cargo test -p eg-rubric` | 见 §1 | **86 passed, 0 failed**（lib 55 + 集成 31） |
| 静态检查 | `cargo clippy -p eg-rubric --all-targets -- -D warnings` | 同上 | 0 warning / 0 error |
| 格式 | `cargo fmt -p eg-rubric -- --check` | 同上 | 通过 |
| 模板数据 | 4 套（active 2 `2026.2` / retired 2） | `cargo test -p eg-rubric --test templates`、`--lib templates::` | 全部可加载；合计 150 分；3 道多选均带官方档位；TW01/TW02 清零、TW03 保留 |
| 规则整理 | 11 份模型输出夹具 | `cargo test -p eg-rubric --test structurer` | 合法输出→草稿；8 类畸形全部拒绝；修复 1 次成功/失败路径覆盖 |

## 6. 风险与限制

| 风险 | 影响 | 缓解/预案 |
|---|---|---|
| R1 现行卷多选档位按「3 个正确选项」登记 | 若某题答案键为 2 个正确选项，默认档位（2/4 分）与该题不符 | 提供 `objective-multiple-2` 备选骨架（选对 1 个 3 分）；README §11 要求教师按答案键确认；模板不记录答案键 |
| R2 `provenance.basis` 已统一为 `public-analysis`（已解决） | 无 | 数据修正 + loader 告警 TW02 测试；T9 自检 |
| R3 `eg-platform` 仓储/审计无法无损承载 `StoredDraft` 与题目级 CAS（task-37） | 真实落库前只能内存实现；草稿伴随字段/模型调用审计字段无法持久化 | 端口已隔离；platform 迁移扩 `rubric_draft` 载荷/列、按题目查询与试卷级版本映射、审计字段 |
| R4 版本语义按题目而非试卷（D2） | 跨题目批量改规则时版本不统一 | 保留 `expected_version` 乐观并发；task-37 统一 |
| R5 模板比例精确折算可能对个别满分失败 | 该模板无法套用（显式错误而非错误分值） | 加载期整除校验；自定义模板可用 `points_milli` |
| R6 规则整理 Schema 与 inference 已实现形状不一致（D9） | `app` 适配器需做字段/下标映射，否则接错 | 本模块导出 `parse_structurer_output`/`render_structurer_prompt`/`StructurerPort` 与 v1 Schema 文本；建议 inference 后续对齐 §2.3 字段名或由 app 单点映射 |
| R7 无真实审计链（内存实现仅进程内） | 崩溃/重启丢审计与草稿；不满足生产审计要求 | 明确仅测试/临时装配；task-37 接 `audit.business_write` 同事务 |
| R8 模型整理仅「结构整理」 | 模型可能仍输出语义偏差（新增口径/改分值语义） | 提示词硬性约束 + 严格 Schema + 静态校验 + 教师确认四重门禁；模型不产答案键、不判分 |
| R9 模板档位与题号绑定依赖答案键确认 | 教师未按答案键改换骨架时档位不符 | 校验 P02/P03 会拦截档位与满分/覆盖不符；README §11 与 §6 明示切换方式 |
| R10 合并选做（`choice_group` 22–23）仅保留元数据 | 组内两题各自建草稿，无自动「只计一次」聚合 | 加载期按 README 只计一次读取题量；聚合策略待 `app`/后续任务决策 |

## 7. 与规范的偏差

| 规范位置 | 偏差 | 处理（改规范/改实现） | 审批状态 |
|---|---|---|---|
| D1 `SPEC-rubric.md` §2.1（草稿含计分方式/档位/模板引用） vs `SPEC-domain.md` §5 `RubricDraft` 字段 | 领域草稿缺 `mode`/`partial_choice`/模板引用；用 `StoredDraft` 伴随记录承载，不改 domain | 先改实现；建议 domain 后续版本评估并入或正式批准伴随记录 | 待审批（已登记） |
| D2 `SPEC-rubric.md` §3.3（`Paper.rubric_version` 基准） | 端口按 `QuestionId` 做版本 CAS；`Rubric.version` 仍从 1 递增 | platform 迁移（task-37）统一为试卷级版本；端口层隔离 | 待审批 |
| D3 `SPEC-domain.md` §5 `template_version: u32` vs 模板文本版本 `2026.2` | 编码 `snapshot_year×1000+revision`（`2026.2 → 2_026_002`，可逆、非零）；审计/界面仍显示文本 | 实现内约定 + 测试固定；domain 若改文本版本则移除编码 | 待审批 |
| D4 `SPEC-rubric.md` §2.1「导入文本」来源 | `DraftOrigin` 无「导入」枚举，导入结构化项记为「手写直译」 | 实现内映射 + 审计 note 记录导入；建议规范确认是否新增来源 | 待审批 |
| D5 `SPEC-rubric.md` 依赖 `inference`（规则整理） | 本 crate 不依赖 `eg-inference`；用 `StructurerPort` trait + `app` 适配（TODO(T25)），提示词/哈希按规范由 rubric 渲染 | trait 适配层 + 内存/夹具测试；app 接线属组合根 | 已按任务约束实现（接口未稳时 trait + TODO） |
| D6 `SPEC-rubric.md` §2.4「部分给分档位合法性」一条 | 细化为 P01–P09 九个错误码（与基础 11 条合计 20 条），便于逐条断言与统计 | 实现细化，语义不变；建议规范附录登记错误码 | 已按实现细化处理 |
| D7 `SPEC-rubric.md` §2.5 确认需「试卷当前规则版本」 | 只按题目版本判断并发 | 与 D2 同源，platform 迁移处理 | 待审批 |
| D8 模板数据 `basis` 取值（已解决） | 数据从 `official-analysis` 改为 `public-analysis`，符合 README 枚举 | 改数据 + README T9 自检；无残留告警 | 完成（T25） |
| D9 `SPEC-rubric.md` §2.3 严格输出字段（criterion/points/required/depends_on） vs `eg-inference` 当前 `StructurerOutput`（title/criteria/max_score_milli/depends，0 起） | 本模块按规范 §2.3 定义 Schema v1；`app` 适配器需在 inference 形状与规范形状之间映射，或由 inference 后续对齐 | 导出严格解析器与 Schema 文本；在适配层单点映射（TODO(T25)）；建议两模块负责人对齐字段名 | 待审批（需 inference 负责人确认） |
| D10 规范未定义模型输出的计分方式 | 由 `infer_mode` 启发式推断（客观题 1 项→全对才给分，否则累加；计算/证明有依赖→分步给分，否则累加），教师可编辑 | 实现约定 + 测试；建议规范补充「模型不输出 mode」的默认规则 | 待审批 |
| D11 模板数据版本 | T25 内容变更按 README §7 升修订号 `2026.1 → 2026.2`；新增 `objective-multiple-2` 备选骨架（题目默认仍引用 3 选项骨架） | 改数据 + README §2/§4/§6/§10/§11 同步 + 测试 | 完成（T25） |
| D12 模板 `total_milli` 在 TOML 中不写 | 由套用时题目满分填充并校验一致（`PartialChoiceRule.total_milli`） | 实现约定；README §4.5 明示 | 完成（实现约定） |
