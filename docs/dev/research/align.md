# 研究记录：身份匹配与区域对齐（align / T28–T29）

> 状态：已完成（分组合并、条码、真实 OCR 兜底未启用，见 §7）· 2026-11-03（Asia/Shanghai）· 对应规范：`SPEC-align.md` v0.2.1、`CONFIG.md` §3（`identity.*`/`align.*`）、`SPEC-domain.md`（`Submission`/`StudentAnswer`/`PaperTemplate`/`Segment`）、`SPEC-platform.md`（仓储/审计）
> 实测机器：Apple M4 / 16 GB / macOS 27.0.1（26A434）/ rustc 1.99.1
> 代码：`crates/eg-align/`（14 个模块 + 4 组测试）；关联研究：`capture.md`（页/质量/去重）、`recognize.md`（识别结果与块）、`platform.md`（仓储/审计）、`templates.md`（模板数据）

## 1. 模块与范围

- 模块 id：`align`（crate `eg-align`）
- 本次实现（T28）：`source_stem` 归一（宽度折叠/大小写/分隔符/页码后缀）、名单匹配（归一精确 → 分词 token；同名/多候选显式未决）、未命中按 `identity.unmatched_behavior` 处理、OCR 兜底端口（默认 0 调用）、文件名与兜底冲突显式未决、重复提交检测（全页内容哈希相同/部分重叠）、人工确认与审计。
- 本次实现（T29）：模板区域（`PaperTemplate.version` 必须等于 `Paper.key_version`，不匹配显式禁用并降级）→ 版面锚点（几何 y 边界）→ 卷面题号文本（文本顺序边界）→ 人工画框；置信度公式与上限；区域重叠 >20% 不猜；跨页 `StudentAnswer.parts`；缺中间页不合并；门禁（`auto_grade_ready` 与逐题 `auto_grade_allowed`）；对齐指纹（算法版本/名单哈希/模板版本与哈希/输入哈希/输出哈希/时间）。
- 明确不做：识别与条码解码（`recognize`；`BlockKind.Barcode` 保留）；判分/导出（`grade`/`report`）；纠正或改写用户文件名；自行排除提交；拼音匹配（`SPEC-domain.md` §15.3）；真实 OCR 兜底调用（需 Ask first 开启）。
- 端口解耦：`PageSource`/`RecognitionSource`/`IdPort` 三个端口承接 capture/recognize/platform，核心算法不依赖其内部类型；夹具与计数端口在 `src/testing.rs`。

## 2. 问题清单

| # | 需要确认的外部行为/API/版本/限制 | 影响 |
|---|---|---|
| Q1 | `eg-platform`（已交付）的配置快照/名单仓储/ULID/审计 API 形态与职责边界？ | 生产接入与审计 |
| Q2 | `identity.source`/`ocr_fallback`/`barcode`/`unmatched_behavior`、`align.*` 的默认值与取值范围？ | 身份与门禁口径 |
| Q3 | 文件名归一要做到什么程度？页码后缀如何保守识别而不误吃学号/日期？ | 零错配 |
| Q4 | 名单匹配的层级（精确/归一/分词）与同名同姓的处理？ | 召回 vs 零错配 |
| Q5 | 区域三级路径的置信度公式与「题号文本」在 domain 中的方法位？ | 区域 F1 与落库 |
| Q6 | 跨页题 `StudentAnswer.parts` 与缺页/超上限的合并规则？ | 判分正确性 |
| Q7 | 门禁三条件在报告里的可消费形态（逐题/整卷）？ | `grade` 硬契约 |
| Q8 | 同一输入重跑能否得到同一指纹（生成 id 是否参与）？ | 可复现（S8） |
| Q9 | 500 份合成夹具能否零错配？默认配置 OCR 兜底是否 0 调用？ | T28 验收 |

## 3. 来源

| # | URL / 位置 | 版本/日期 | 类型 | 取用内容 |
|---|---|---|---|---|
| S1 | `SPEC-align.md` v0.2.1（仓库内） | 2026-11-02 | 内部规范 | §3.2 身份顺序与阈值、§3.3 区域优先级/公式/跨页、§3.4 未决种类、§3.5 门禁、§3.6 指纹与审计、§7 测试策略、§9 验收 |
| S2 | `CONFIG.md` §3（仓库内） | v0.2.1 | 内部规范 | `identity.source=文件名`、`ocr_fallback=关`、`barcode=关`、`unmatched_behavior=未决`、`ocr_threshold=0.90`、`ocr_margin=0.10`、`template_region=开`、`region_confidence_floor=0.80`、`max_cross_pages=3` |
| S3 | 本机 `crates/eg-platform/src/config.rs` | 0.1.0（2026-11-02 交付） | 上游源码 | `AppConfig{identity: IdentityConfig, align: AlignConfig}`、`IdentityConfig` 6 字段、`AlignConfig` 3 字段与序数校验 |
| S4 | 本机 `crates/eg-platform/src/repo.rs` | 0.1.0 | 上游源码 | `list_students(conn, ClassroomId)`、`insert_student(tx, &Student, Timestamp)`、`insert_classroom`、`get_paper` |
| S5 | 本机 `crates/eg-platform/src/{audit.rs,ids.rs,platform.rs}` | 0.1.0 | 上游源码 | `AuditLog::append(NewAuditEntry)->AuditEntry`（actor 固定 `local-teacher`、链式哈希）、`UlidFactory::next_segment_id()`、`Platform::{config,db,audit,ids,clock}`、`FixedClock` |
| S6 | 本机 `crates/eg-domain/src/{page.rs,paper.rs,subject.rs,geometry.rs}` | 0.1.0 | 上游源码 | `Submission.source_stem`、`Page`、`PaperTemplate::ensure_auto_align_allowed`（paper id + version 校验）、`TemplateRegion`、`StudentAnswer::validate`（parts 非空、主段 id 必须在 parts）、`AnswerPart`、`BBox` 不变量 |
| S7 | 本机实测（`cargo test -p eg-align`） | 2026-11-03 | 命令输出 | 35 个测试全绿（§5）；500 份文件名零错配、0 次 OCR 调用、模板降级/跨页/缺页/指纹/真实审计链 |
| S8 | `docs/dev/research/{capture.md,recognize.md,platform.md,templates.md}` | 2026-11-02/03 | 先前研究记录 | capture 产 `Submission/Page`（含 `source_stem` 原样保留）、recognize 产 `Recognition/Block`、platform 事务/审计语义 |

## 4. 结论

| 问题 | 结论 | 依据 |
|---|---|---|
| Q1 | 适配层落地在 `src/platform_bridge.rs`：`settings_from_platform`（`Platform::config()` → 本 crate 配置快照 + `validate`）、`load_roster`（`repo::list_students` → `Roster::new` 校验）、`PlatformIds`（`UlidFactory::next_segment_id` → `IdPort`）、`write_alignment_audit`（`align.alignment`，after=输出哈希）、`write_manual_audit`（`align.manual_correction`，before/after 哈希；actor 由平台固定 `local-teacher`）。核心算法不引用平台类型。实测：真实 SQLite 名单 2 人可加载；对齐审计 + 人工确认审计写入后 `verify_chain().ok=true`、`audit_log` 各恰 1 条（`align.alignment`/`align.manual_correction`）。 | S3/S4/S5/S7 |
| Q2 | 默认值全部与 `CONFIG §3` 一致（`AlignSettings::default` = 平台 `IdentityConfig::default` + `AlignConfig::default`，实测 `assert_eq!`）。`validate` 在批次前拒绝：`identity.source≠文件名`、`barcode=开`（显式 `not_implemented`，需 Ask first）、`ocr_threshold` 越界、`ocr_margin` 越界、`region_confidence_floor` 越界（0.5–0.99）、`max_cross_pages` 越界（1–5）。配置快照哈希进输入指纹（浮点 6 位小数，避免格式化抖动）。 | S2/S3/S7 |
| Q3 | 归一：全角 ASCII/全角空格折半角 → 小写 → 连续空白折叠 → 去首尾；匹配键再去分隔符（`_ - . · * / \ : （）` 等）。页码后缀只认规范列出的形态（`_p1`/`p1`/`page1`/`_2`/`第N页`），**不**把 `-`/`.`/空格 + 数字当页码（否则 `2026-11`、学号尾数字会被误吃）；无把握返回原基名。题号归一：主号取开头数字（容忍 `l7.`/`|7、` 噪声），子号取括号内数字。 | S1 + S7 |
| Q4 | 匹配层级：① 归一后等于学号（1.000，`FileNameExactNo`）；② 归一后等于姓名（1.000，同名多人 → `identity_ambiguous` 未决）；③ 分词 token 等于学号/姓名（1.000，`FileNameTokenNo/Name`，兼容「张三_期中」「20260001_张三」）；④ 未命中 → 按 `unmatched_behavior` 进未决/名单外。任何多候选一律未决、候选脱敏（学号后 4 位 + 姓名首字打码），不按相似度自动排序。500 份夹具中 300 份规范化/学号文件 100% 确认且 0 错配，200 份相机文件名/同名/名单外 100% 未决。 | S1/S6/S7 |
| Q5 | 模板路径：`PaperTemplate::validate` + `ensure_auto_align_allowed(paper)`（paper id 与 `version==paper.key_version`），通过则每区域 `Segment{method=TemplateRegion,confidence=1.000}`；不通过时原因进 `template_disabled` 并降级。版面/题号文本路径：锚点 = 识别块文本开头主号且该主号在试卷内唯一（同主号多题 → 重复题号未决；同页重复锚点 → `cross_page_conflict`），`precision=锚点数/题数`、`monotonic=按题序递增的相邻锚点比例`，`conf=min(0.950, 0.5+0.4·precision+0.1·monotonic)`；版面路径要求锚点几何 y 单调（按页号+y），否则改用文本顺序边界（`RegionPath::TextOrder`）。区域重叠（交集/较小面积）>20% 记未决且不猜。**偏差 D1**：`eg_domain::SegmentMethod` 只有 `TemplateRegion/LayoutDetection/Manual`，无「题号文本」；本模块用 `RegionPath` 保留真实路径，落库 method 记 `LayoutDetection`。 | S1/S6/S7 |
| Q6 | 跨页：按页号聚合该题 `Segment`；每页一段 `AnswerPart`（page_id/segment_id/recognition_id/bbox/text），主段取首页；`raw.text` 按页号升序以 `\n` 合并（单页时保留 `blocks`）；`recognition_confidence` 取各段最小值；页号不连续 → 记 `missing_page`、`raw.text` 留空（证据仍保留、门禁拦截）；页数 > `max_cross_pages` → `cross_page_conflict` 且不构造答案；同页多段 → `cross_page_conflict` 不合并。`StudentAnswer::validate` 全量通过（主段 id 必在 parts）。 | S1/S6/S7 |
| Q7 | 报告给出两档门禁：逐题 `QuestionRegionSummary.auto_grade_allowed = confidence ≥ floor && 无该题阻断未决`；整卷 `AlignReport.auto_grade_ready = 身份确认 && 无重复提交 && 所有题放行 && 无阻断未决`（`UnresolvedKind::blocks_auto_grade`：多页不阻断，其余阻断）。未对齐题仍进摘要（`RegionPath::None`、置信度 0、`auto_grade_allowed=false`），绝不表现为「未作答」；`answers` 只含可构造的题。 | S1/S7 |
| Q8 | 指纹：`input_hash` 覆盖算法版本、配置哈希、名单哈希、提交 id、stem 哈希、页码后缀、页资产/页号、试卷 id/key_version/题号、模板版本/哈希、每页识别（id/置信度/块哈希/文本哈希）；`output_hash` 覆盖身份结论、重复判定、`template_disabled`、整卷门禁、区域（页号/题号/bbox/method 字符串）、逐题摘要、答案（文本/parts bbox/置信度）、未决项（kind/题号/页号/说明）、人工历史命令码。**生成 id 不参与语义输出哈希**（属存储坐标）；实测同输入用不同 id 起点（1 vs 10000）重跑，`input_hash` 与 `output_hash` 全等。`generated_at` 由注入时钟生成，不进哈希。 | S1/S7 |
| Q9 | 500 份合成夹具（300 规范化/学号 + 100 相机文件名 + 50 同名同姓 + 50 名单外）：自动确认 300、错配 0、未决 200（100% 覆盖）；默认配置 `CountingHints.calls()==0` 且 `evidence.hint_count==0`；开启兜底（仅在测试显式构造的端口上）可确认唯一学号，文件名与兜底冲突 → 双方候选并列的 `identity_conflict`。 | S1/S7 |

## 5. 版本实测

| 依赖/工具 | 实测版本 | 命令 | 结果摘要 |
|---|---|---|---|
| `eg-domain` | 0.1.0 | `cargo test -p eg-align` | `Submission/PaperTemplate/StudentAnswer/AnswerPart/BBox` 校验全通过；`Rotation/ExifOrientation` 夹具可用 |
| `eg-platform` | 0.1.0（并行交付） | `cargo test -p eg-align --test platform_audit` | 配置快照与默认值一致；真实 SQLite 名单加载；平台 ULID 生成区域 id；人工审计链完整（1 条） |
| `serde` / `serde_json` / `thiserror` | 1.0.229 / 1.0.154 / 2.0.21 | 全测试 | 指纹 canonical JSON 稳定；错误中文文案与稳定码 |
| 测试规模 | **35 个测试**：lib 21 + align_identity 4 + align_region 7 + platform_audit 3 | `cargo test -p eg-align` | 全绿（debug 全量 < 1 秒） |
| 静态检查 | clippy / fmt | `cargo clippy -p eg-align --all-targets --no-deps -- -D warnings`、`cargo fmt -p eg-align -- --check` | 本 crate 0 警告、fmt 通过。注：不带 `--no-deps` 时 clippy 会同时 lint 同 workspace 的 `eg-platform`，其并行迁移（task-37）期间有自身 clippy 告警/临时编译错误，与本 crate 无关（见 §6 R5） |

## 6. 风险与限制

| 风险 | 影响 | 缓解/预案 |
|---|---|---|
| **R1：分组合并未实现**（`SPEC-align.md` §3.3「同 stem 多文件 + 页码后缀 → 合并为一份」） | 当前对齐以「单份提交 = 一个 capture 交付的源文件」为前提；同 stem 多文件仍各自独立，只解析页码后缀进证据 | capture 的 `ImportRequest` 没有 `target_submission`/分组字段（`capture.md` §7 已记）；待 app/组合根引入「分组」后在本 crate 增加 `GroupInput`，用 `split_page_suffix` 的页序 + 重复页检测；当前行为记录为偏差 D2 |
| **R2：区域裁片未生成**（`Segment.crop` 当前复用页面资产占位） | `crop` 不是真实裁片；下游按 `Segment.bbox` 裁剪前不能直接送识别 | 内容寻址资产有效、坐标已给；裁片属图像 IO（capture/image），本 crate 不做；TODO 在 `region.rs` 模块文档，`grade`/`recognize` 组合根按 bbox 裁剪 |
| **R3：真实 OCR 兜底未接入**（默认关） | 相机文件名等未命中只能人工（符合用户决策） | 端口 `IdentityHintSource` 已定义、阈值/分差逻辑已实现并被测试覆盖；开启需 Ask first（`CONFIG` 阈值已登记但行为未经真实模型校准） |
| **R4：题号文本路径与版面路径共用锚点抽取** | 两条路径差异只在边界规则（几何 y vs 文本顺序）；若识别块顺序与阅读顺序不一致，文本路径同样受影响 | `recognize` 保证 blocks 按阅读顺序（`SPEC-recognize §3`），测试覆盖几何非单调 → `TextOrder`；真实噪声题号（`l7.`）有单测 |
| **R5：并行编辑导致的构建噪声** | 实现期间 `eg-platform` 被 task-37（迁移 v2）并发修改，`cargo check/test/clippy -p eg-align` 曾被其临时编译错误/自身 clippy 告警阻塞 | 不修改他人 crate；本 crate 验证以「平台可编译」时点为准，并用 `--no-deps` 单独确认本 crate 0 警告（§5）；平台任务修完后应再跑一次无 `--no-deps` 的全量门禁 |
| **R6：同名同姓/同姓近似只做保守判定** | 重名必须人工，召回受限（规范要求零错配优先） | 候选脱敏并列展示；不按相似度排序、不自动择优；`identity_ambiguous` 可被 `review` 逐项处理 |
| **R7：门禁以「题」为粒度，未含 OCR 识别可用性** | 识别不可用时本模块会以 `region_uncertain` 记未决并拦题；但「识别置信度低」由 `recognize`/`grade` 侧处理 | 本模块已把「该页无识别结果」转未决；`grade` 仍需自行判定识别可用性（`SPEC-grade` 硬契约） |
| **R8：debug 夹具全部程序构造** | 未在真实扫描/照片上校准区域 F1 | 合成夹具覆盖模板/版面/文本/跨页/缺页/重叠；真实样本到位后按 `SPEC-align §9.4`（模板 ≥0.99、版面 ≥0.90、文本 ≥0.85）复标 |

## 7. 与规范的偏差

| 规范位置 | 偏差 | 处理（改规范/改实现） | 审批状态 |
|---|---|---|---|
| **D1**：`SPEC-align.md` §3.3 列「模板/版面/卷面题号文本/人工」四种方法 | `eg_domain::SegmentMethod` 只有 `TemplateRegion/LayoutDetection/Manual` 三个变体，无「题号文本」 | 本 crate 用 `RegionPath::{Template,Layout,TextOrder,Manual,None}` 保留真实路径，落库 method 记 `LayoutDetection`；需要规范确认「题号文本」是否新增 domain 变体（属 `SPEC-domain` 变更，Ask first） | 待负责人确认 |
| **D2**：`SPEC-align.md` §3.3「同一 stem 多文件且页码后缀可解析 → 合并为一份」 | 未实现（依赖 capture/组合根的导入分组；当前单文件=单提交） | 已解析页码后缀并进证据；分组留待 app/组合根引入后在本 crate 加 `GroupInput`（不改规范语义） | 任务标注 T28 范围外（需 capture 请求扩展） |
| **D3**：`SPEC-align.md` §3.5「区域置信度低于 floor 必须输出未决」 | 已实现；但模板路径恒 1.000，不做重叠外的人工复核提示 | 与规范一致 | — |
| **D4**：`SPEC-align.md` §3.6「人工修正按命令哈希幂等，撤销即追加反向记录」 | 人工修正是纯函数 + 追加 `ManualRecord`（before/after 输出哈希）；幂等/撤销 API 未提供（无命令库） | 本 crate 提供 `apply_manual` 与审计条目；幂等键/撤销栈属 `review` 命令层（`SPEC-review`），由其按 before/after 哈希追加反向记录 | 任务标注 T31 范围 |
| **D5**：`SPEC-align.md` §3.2 条码（第 2 级） | 未实现；`barcode=开` 时 `validate` 显式 `not_implemented` | 条码解码属 `recognize` 且 v1 无解码器；开启需 Ask first | 默认关，符合用户决策 |

## 8. 复现命令

```bash
source "$HOME/.cargo/env"
cd <repo>

# 1) 全量测试（35 个）；真实 platform 集成会建临时 SQLite（tempfile）
cargo test -p eg-align
cargo test -p eg-align --test align_identity   # T28：500 份夹具/零错配/0 次 OCR
cargo test -p eg-align --test align_region     # T29：三级路径/跨页/缺页/门禁/指纹
cargo test -p eg-align --test platform_audit   # 真实配置快照/名单/审计链

# 2) 质量门禁
cargo clippy -p eg-align --all-targets -- -D warnings
cargo fmt -p eg-align -- --check

# 3) 夹具说明（全部程序构造，无真实图片/模型）
#    src/testing.rs：student/roster/paper/submission/page_refs/
#                    recognition(+_with_anchors)/template/strip/block/CountingHints
```
