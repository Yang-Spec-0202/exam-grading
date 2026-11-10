# 变更记录

> 记录规范与行为的变更（按版本倒序）。文档结构变更也在此登记。

## [0.3.6] - 2026-11-10（最终 v2 复测：并发 2 + 超时重试，120/120，S3 达标）

### 新增（瞬态重试 + 最终复测）

- **超时/连接瞬态重试**（`crates/egctl/src/eval_real.rs`）：仅对 timeout/连接重置类错误重试 1 次，
  退避 500ms，记录 `retries.attempted/succeeded/failed`；schema_violation 不进入瞬态重试（partial 缺列表仍走
  task-73 的 1 次机械修复）；单测 `transient_error_policy_is_limited_to_timeout_and_transport`。
- **最终 v2 复测**：`egctl eval --gold fixtures/gold --real-judge --sample 80 --eval-sample 120 --calibrate
  --prompt v2 --concurrency 2 --json --out target/acceptance/eval-real-judge-v2-final-20261110.json`
  - **eval 120/120 条成功**（360/360 评分项；无 timeout）；calibration 80 条中 1 条 schema_violation（拟合 237/240 项）。
  - 瞬态重试 attempted **0**（并发 2 未出现 timeout，策略由单测覆盖）；partial 修复 **15/14/1**（成功率 93.3%）。
  - 指标：逐项 strict **98.06%**、lenient **99.44%**、总分精确 **94.17%**、半分内 **100.00%**、
    partial 召回 **91.11%**、mismatch→match **0**、status raw 23.33% / 校准后 76.67%。
  - 校准：ECE **0.05569→0.06072**（均 ≤0.08）、Brier **0.01655→0.02275（变差）**；单桶校准。
  - **S3 判定（本次 120/120）**：总分 94.17%≥85% ✅、半分内 100%≥97% ✅、ECE 0.0607≤0.08 ✅ → **达标**；
    限制：合成 gold、单桶/校准 Brier 变差、status 未接 calibration_id、耗时 **47m44s**（超预算）。
- **文档**：`docs/dev/acceptance.md` S3 新增「最终 v2 复测（并发2+超时重试，120/120）」节
  （样本完整性/重试修复/指标/逐项 S3 判定/历史对比/限制/证据）。

## [0.3.5] - 2026-11-10（v2 硬化 + 真实 v2 重校准 + 120 条复测；含接线更正）

### 更正（task-75）

- **先前误标 v2**：`eval_real.rs` 的 `run`/`run_calibrated` 原先硬编码 `PromptVersion::V1`，CLI `--prompt v2`
  被忽略，而 JSON `split.prompt_version` 按参数写成 v2；因此下面最初记录的「v2 600 次请求 / ECE 0.0833→0.0708 /
  eval 逐项 86.67%」实际是 **v1 prompt + 80/120 拆分**。已改为统一 `selected_prompt(options)`（run/run_calibrated），
  新增单测 `prompt_version_selection_and_prompt_content` 断言 V1/V2 选择与提示词内容不同；`split.prompt_version`
  与实际一致。旧结果 JSON 文件名（`eval-real-judge-v2-calibrated-20261110.json`）保留但内容实为 v1，文档已标注。

### 新增（v2 schema 硬化/重校准/真实重跑）

- **partial schema 硬化 + 1 次机械修复**：v2 prompt 明确 partial 必须带 `satisfied/missing`；首次违规反馈字段后
  做 1 次重试（严格 `JudgeOutput::validate` 不变），仍失败计入 `schema_violation`；记录 repair attempted/succeeded/failed。
- **真实 v2 重跑**（qwen3.5-4b Q4_K_M，b11960-8642541b2，并发 4，80/120 互斥拆分）：
  `target/acceptance/eval-real-judge-v2-true-20261110.json`；耗时 **37m10s**（超 30min 预算）。
  - **repair attempted 11 / succeeded 11**（partial 缺列表被真实触发并修复成功；另有 1 条修复后仍失败，旧 artifact 的
    failed 计数未含该条，代码已修正为 failed=1 统计）；失败：**timeout 33 + schema_violation 1**。
  - 结果覆盖：calibration 拟合 204/240 项；eval **101/120 条成功**（303/360 项），**19/120（15.8%）失败/超时未入分母**。
  - eval（成功子集）：逐项 strict **98.02%**、lenient 100%、总分精确 **94.06%**、半分内 **100%**、
    partial 召回 **88.57%**、mismatch→match **0**、status raw 25.74% / 校准后 74.26%。
  - 校准：ECE **0.05957→0.05863**（≤0.08）、Brier **0.01623→0.02285（变差）**；校准表 1 桶（0.9216）。
  - 结论：成功子集看似达标，但 15.8% 失败导致选择偏差，且 Brier 变差/status 未接入，**不宣称 S3 达标**；
    需降并发/加重试后无失败复测。
- **v1 + 80/120 参考（原误标 v2 的那组，2026-11-10）**：600 次请求、0 失败、21m29s、token total 222,884；
  eval 120 条逐项 86.67%、总分 65.83%、半分内 95.00%、partial 召回 42.22%、ECE 0.0833→0.0708；
  仅作 v1 基线参考，文件名与旧文档已注明修正。
- **文档**：`docs/dev/acceptance.md` S3 将原节改标「v1 prompt + 80/120」并新增「真实 v2 prompt + 重校准」
  一节（样本/repair/失败/指标/是否达标/限制/证据）。

## [0.3.4] - 2026-11-10（judge 校准 + 提示词 v2 A/B：S3 ECE 0.1056→0.0667）

### 新增（校准与 A/B）

- **`egctl eval --real-judge --calibrate [--ab]`**（`crates/egctl/src/eval_real.rs`）：固定 seed=20261109 按
  perturbation×status 分层拆分 calibration/eval/A-B（互斥无泄漏；默认 60/60/30），校准集拟合
  **等频分桶 + PAV 单调**校准表（无新依赖），仅 eval 集评估；输出校准前后 ECE/Brier、可靠性表、
  calibration_id `qwen35-4b-q4-iso-v1`、逐项 strict/lenient、partial 召回、mismatch→match、混淆矩阵、失败分类；
  提示词 v2 明确「关键要素等价即 match；部分满足→partial + satisfied/missing；仅关键结果错误 mismatch」。
  校准数学有单元测试（单调性/PAV/ECE 改善）；真实模型测试 `#[ignore]`。
- **实测（qwen3.5-4b-instruct Q4_K_M，llama.cpp b11960-8642541b2，并发 3，2026-11-10）**：
  calibration 60 / eval 60 / A/B 30，180 条 entry、540 次请求；总耗时 **30m42s**（超 20 min 预算，如实记录）。
  - 校准（eval 60 条/180 评分项）：ECE **0.1056→0.0667**（达标 ≤0.08）、Brier **0.1425→0.1358**；
    eval 逐项 strict 84.4%、总分精确 63.3%、半分内 90.0%、partial 召回 34.8%、status 23.3%（校准后 status 不变，
    因校准概率 0.778<0.80 且 calibration_id 未接入 grade）；校准表 1 桶（confidence=0.95 并列，accuracy=0.778）。
  - A/B（30 条，配对成功 25 条）：v1 逐项 90.7%/partial 召回 37.5%/总分精确 72.0%/ECE 0.0433；
    v2 **逐项 100%**/partial 召回 **100%**/总分精确 **100%**/mismatch→match **0**/ECE 0.0500；
    v2 失败 5/30（3 schema_violation: partial 缺 satisfied/missing；2 timeout），配对子集存在选择偏差，不宣称整体 100%。
  - 结论：ECE 达标但 S3 准确率仍不达标（总分精确 63.3%<85%、半分内 90%<97%）；v2 方向有效但需全量复测；
    **未调参、未改 gold/实现**。
- **文档**：`docs/dev/acceptance.md` S3 新增「校准与提示词改进（本轮）」段（拆分/方法/ID/前后 ECE/Brier/
  可靠性表/A-B/失败/耗时/限制）；结果 JSON `target/acceptance/eval-real-judge-calibrated-20261110.json`。

## [0.3.3] - 2026-11-10（真实 4B judge S3 复测：`egctl eval --real-judge`）

### 新增（评估）

- **`egctl eval --real-judge --judge-endpoint <URL> --model <name> [--sample N] [--concurrency N]`**：
  `crates/egctl/src/eval_real.rs` 逐评分项请求本地 OpenAI-compatible judge（`temperature=0`、`max_tokens=256`、
  `stream=false`、`response_format=json_object`、timeout 20s；输出必须过 `JudgeOutput::validate`），
  模型 `match` 视为评分项满足 → `eg-grade::aggregate` → 与 `fixtures/gold` 对比；输出逐项/总分/status/
  混淆矩阵/真实 ECE（模型 confidence 分桶）/延迟 P50/P95/token/失败分类，按 perturbation×status 固定 seed 分层抽样。
- **实测（2026-11-10，qwen3.5-4b-instruct Q4_K_M，llama.cpp build b11960-8642541b2，4 slots）**：
  `--sample 60 --concurrency 3`：60/225 条（跳过 manual 5），180 次评分项请求，**0 失败/超时**；
  逐项一致 139/180=**77.2%**（Wilson 95% CI 70.6–82.7%）、总分精确 **56.7%**、半分内 **83.3%**、
  status **20%**（模型无 calibration_id，门禁全 review）、真实 **ECE 0.1778**（n=180；fixture 基线 0.0933）；
  延迟 mean/P50/P95 = **7211/7276/9103 ms**（并发 3，含排队）、token prompt 57,777 / completion 8,612 / total 66,389；
  混淆矩阵：expected match→match24/mismatch5/partial6，expected mismatch→mismatch2，expected partial→mismatch14/partial9。
  结论：**真实 4B 未达 S3 目标**（低于 fixture/规则基线 100%），如实报告，不调参/不改 gold；
  结果 JSON：`target/acceptance/eval-real-judge-sample60-20261109.json`。
- **CI**：真实模型测试 `crates/egctl/tests/eval_real_judge.rs` 标 `#[ignore]`，普通 `cargo test -p egctl` 不依赖服务；
  fixture 版本全绿。
- **文档**：`docs/dev/acceptance.md` S3 拆「fixture/规则基线 100%」与「真实 4B 采样（未达标）」两行，
  写命令、样本量、指标、混淆矩阵、延迟/token、限制与证据路径。

## [0.3.2] - 2026-11-09（S2/S3/S5 合成金标评估器与基线）

### 新增（评估）

- **`egctl eval --gold fixtures/gold [--json] [--out FILE]`**：S2/S3/S5 合成金标评估器（`crates/egctl/src/eval.rs`）。
  按 `expected.route` 分流：规则路径用 `eg-subject-math` 归一化 + 比较器链（多选部分给分用 `eg-grade::aggregate::partial_choice`），
  rubric 路径用 `eg-grade::aggregate`（`expected.steps[].matched` 作 judge 逐项输入），manual 只断言路由/状态/原因；
  输出总体 + 按 kind/perturbation/status 分组指标、失败分类/样本、阈值达成（S2 100%、S3 完全一致 ≥85%/半分内 ≥97%/ECE ≤0.08、S5 ≥98%）。
  CI 子集 `cargo test -p egctl --test eval_gold`（60 客观 + 30 解答，约 0.04 s）。
- **实测基线（fixtures/gold v1，2026-11-09）**：540 客观 + 230 解答 = 770 题，耗时约 55 ms。
  - S2：分数精确一致 100%、归一化 100%、status 100%、auto/review/manual=410/38/92、误给 0=0、多选档位 33/33。
  - S3：逐评分项 675/675、总分精确 100%、半分内 100%、status 100%、manual 5/5、rubric_only 5/5、校准一致 225/225；ECE=0.0933 为规则推导置信度标签，**不可用于真实校准判定**（如实标注）。
  - S5：L1 规则命中 770/770=100%、route 标签 100%。
  - 失败样本 0、失败分类为空；**未发现实现 bug**；未修改 gold/expected 或实现凑数。
- **文档**：`docs/dev/acceptance.md` S2/S3/S5 改为「已测（合成金标）」，写明命令、样本量、实测数字、阈值与诚实声明。

## [0.3.1] - 2026-11-05（外部夹具 S1 端到端通过，CP-F ✅）

### 验收进展

- `egctl acceptance --fixtures fixtures/paper-20q --json` **PASS：287 断言 / 0 失败 / 9 负向 / 12 步（exit 0，约 1.8 s）**；`cargo test -p egctl` 全绿，clippy/fmt 通过。外部夹具 expected 逐题通过：张三 132000、李四 77000、王五 109000；Q9–11 部分给分、Q17 低置信、Q16 冲突、Q19 仅规则、Q20 manual、名单外未决。
- **capture 多页归组已交付**（93 测试，3 学生×2 页 → 3 提交×2 页），外部夹具「每学生 2 页同一提交」端到端跑通。
- 计划口径：CP-F 转 ✅（M8 同步完成）；M9 仍 🟡（UI 人工验收：键盘 E2E/IME/真实重判/redo）；T41 仍 🟡（剩余 S2/S3/S5/S7 与真实模型）。
- 剩余 app 侧：组合根接入 capture 多页归组接口（当前 app 真实导入仍单文件=单页提交）。

## [0.3.0] - 2026-11-05（fixtures、S1 自动化、app 集成收尾）

### 新增（夹具与验收）

- **仓库自带演示夹具** `fixtures/paper-20q`：20 题数学试卷（含 8 单选/3 多选部分给分/4 填空/3 计算/1 证明/1「略」，150000 毫元）× 3 名学生 + 名单外/不规范命名/重复字节/EXIF 样本；配套 `fixtures/images/`（9 张 2480×3507 合成印刷体答卷）、`fixtures/ocr/`（页级 OCR 期望与负向）、`fixtures/judge/`（逐题判定期望与负向）、`fixtures/expected/grades-expected.json` 与生成器 `fixtures/generator/`。`python3 fixtures/generator/verify_fixtures.py` 自检 **1826 项，0 失败**；`fixtures/MANIFEST.sha256` 记录完整性。
- **S1 backend 自动化**：`egctl acceptance --fixtures fixtures/paper-20q`（临时数据根）跑通
  「migrations → capture 导入 → recognize（fixture OCR）→ align → grade（fixture judge）→ review 改分/撤销 → report 三格式导出 + 重放一致」；
  实测 **PASS：83 条断言 / 0 失败 / 6 个负向用例，约 317 ms（release）/ 约 5 s（debug）**；CI 集成测试 `cargo test -p egctl --test acceptance`。记录见 `docs/dev/acceptance.md` §S1。
- **`eg-inference` fixture-key 回放**：`FixtureCatalog` 装载期重算并校验 `fixture_key`（模型 id/版本/输出 schema/提示词字节/输入字节）与全部 sha256，三级匹配（精确键 → 组合键 → 语义标识），未命中显式 `FixtureMiss`、歧义报 `Internal`；`fixtures_wiring` 集成测试（真实仓库夹具 5 项）通过。接入 `egctl acceptance` 仍为 TODO。

### 变更（app 集成收尾）

- **导入真实落库**：`eg-app` 导入从内存资产槽改为 `AssetStore::put`（资产文件+行）+ `eg_capture::persist` 单事务写 Submission/Page/审计；导入前 `enqueue→claim` 任务并用 `persist(task_id)` 推进阶段、完成后 `complete`。外部夹具实测：9 个图片文件 → 成功 9 / 失败 0 / 拒收页 0 / 同批重复 1（去重）→ **8 提交 / 8 页 / 资产链接 32 / 审计 17**，命名提醒仅一次；详见 `docs/dev/research/app.md` §8.8。
- **报表导出联动**：报表页新增「导出报表」命令（`Report::build/export` + `insert_export_record` 审计）；导出成功清 `report_refresh_required` 并显示导出摘要；导出后的复核改分/撤销置 stale 并在报表页提示「请重新导出」。
- **复核工作台真实适配**：队列/详情/命令/撤销/提交级排除/导出改走 platform v3（`list_grade_records_for_queue`、`load_grade_evidence_summary`、`append_grade_record_with_reasons`、`audit::command_write`、`push_undo/pop_undo`、`insert_export_record`）；页面 VM/通道协议不变。
- **图像证据懒加载 + 三栏最小版**：`Event::ReviewImage` 携带 platform 资产路径，UI 线程加载 `slint::Image`，加载失败显式提示；复核页为「队列 / 图像+证据 / 判定操作」三栏。
- 已知限制（未完成项，已在文档标注）：多页归组未接（capture 单文件=单页提交）、`RegradeEngine` 真实重判未接（显式失败保留原判）、redo 仍内存、规则模型整理未接、UI 键盘 E2E 与中文 IME 人工验收待办。

### 文档

- `tasks/plan.md`：M0–M10 与 CP-A…CP-H 增加状态列（M0–M9 完成；M10 进行中；CP-D/CP-E/CP-G/CP-H 部分）。
- `tasks/todo.md`：新增 T01–T41 状态总览（✅ 33 / 🟡 8 / ⬜ 0）与 T41 待办清单；新增「本轮追加工作」表（fixtures、S1 自动化、app 导入、多页归组待做）。
- `docs/README.md` 里程碑表同步；`docs/user/quick-start.md` 增加「仓库自带演示夹具」S1 后端自测小节；`docs/book/src/**` 重新生成。

## [0.2.4] - 2026-11-03（实现期偏差回写）

### 变更（规范补丁，零实现改动）

- `SPEC-recognize.md` v0.2.4：OCR 量化口径与 `CONFIG.md`/`SPEC-inference.md` 对齐（官方 GGUF BF16 默认、Q4_K_M 仅第三方默认不启用）；结构校验归属不变，但**实现期**严格解析暂由本模块承载（`eg-inference` OCR 契约定稿后迁回）；公式 LaTeX 非法或 `ocr.emit_latex` 关闭时，块降级为「未知」块（`latex` 空、置信度 0），与 `SPEC-domain.md` §7「公式块必带 LaTeX」不变量统一；手写阈值与输入长边上限登记为配置键。依据 `docs/dev/research/recognize.md` §7。
- `SPEC-inference.md` v0.2.4：云端策略拦截（总开关关/无授权/范围不符/预算超限）写审计 `inference.policy_blocked`，不伪造 `ModelCall`；预算用尽 → `ConsentState::Exhausted` 持久态，重启后仍失效；v1 内置传输无 TLS、`https://` 显式不支持（启用云端前单独评估 TLS 依赖）；注册表默认编译期内嵌、数据目录 `inference.registry_path` 可选覆盖，加载顺序与失败语义明确；模型输出 `ungradable` 由调用方构造。依据 `docs/dev/research/inference.md` §8。
- `SPEC-capture.md` v0.2.4：HEIC/HEIF 与 DOCX 参考答案文本抽取在 v1 **显式不支持**（返回明确错误码，不静默、不降级），列入已知限制并调整 C1 验收；落库定为「资产先行、业务单事务」（`platform` 的 db/repo/audit 同一写事务）。依据 `docs/dev/research/capture.md` §7。
- `SPEC-platform.md` v0.2.4：日志实现允许 `tracing` 订阅层或输出契约等价的**自研 JSONL writer**（字段/级别/脱敏/轮转/索引不变）；任务授权撤销 = 状态更新 + 审计留痕（不追加新记录），新增 `Exhausted` 持久态；登记迁移 v2（`rubric_draft` 载荷与按题目查询、`paper` 级版本 CAS 辅助、审计扩展、`task_consent.state` 增加 `exhausted`）；备份恢复必须先清理目标库 `-wal`/`-shm`（CP-B 缺陷 D1 经验规则）。依据 `docs/dev/research/platform.md` §4/§7/§10.4、`docs/dev/research/rubric.md` §7-R3、`docs/dev/research/inference.md` §8-D2。
- `SPEC-grade.md` v0.2.4：R1「学科归一化失败」适用面收窄为「有具体答案键、需确定性比较」的题；「仅评分标准/仅人工」使用原始文本；判定理由长度上限登记为配置项。依据 `docs/dev/research/grade.md` §7。
- `SPEC-domain.md` v0.2.4：§15.4 登记支撑类型与落地解释（`Timestamp`/`Decimal`/`SubjectId`/`ComparatorId`/`BackendId`/`TemplateId`/`CalibrationId`/`Rotation`/`ExifOrientation`/`ComparatorTraceId`/`JudgedUnit`/`RubricItemRef`）；`SchemaVersioned` 用关联常量 + 存储层外层封装（字段表不变）；`RubricDraft.id` 用 `RubricId`；`RubricDraftItem.depends_on` 从 1 起；`AnswerPart.segment_id`/`recognition_id` 非空；`Judgement::Partial` 的 `satisfied`/`missing` 用 `JudgedUnit`。依据 `docs/dev/research/domain.md` §7。
- `SPEC.md` v0.2.4：§4 日志措辞与 `platform` 实现一致；新增 S14 多选题部分给分成功标准（按档位给分、错选归零、缺档转人工不默认 0）；§10 许可白名单维持 10 项。
- `CONFIG.md` v0.2.4：新增 `ocr.input_max_edge`（默认 2000 px）、`recognize.handwriting.page_threshold`（0.55）/`block_threshold`（0.50）、`grade.reason_max_chars`（500）；`inference.registry_path` 说明改为「内嵌默认 + 数据目录覆盖」。
- 记录补登：`docs/changelog.md` 补 v0.2.2 条目（此前只有 0.2.1/0.2.3 条目，0.2.2 的变更散落在规范摘要中）。

### 说明

- 本版本为**规范补丁**：只改 `SPEC*.md`、`CONFIG.md`、本文件，不改实现代码与研究记录；零代码围栏。
- 偏差依据与逐条裁定见 `docs/dev/research/` 下 `recognize.md`、`inference.md`、`capture.md`、`platform.md`、`grade.md`、`domain.md` 的「与规范的偏差」节。

## [0.2.3] - 2026-11-02（规范修订 + 文档初稿）

### 变更（按 tech-spike 实测修订）

- `SPEC.md` v0.2.3：OCR 官方 GGUF 为 BF16（Q4 仅第三方、默认不启用）；OCR 有效分辨率受 mmproj 下采样限制（约 1MP，密集小字靠版面分块而非提高 DPI）；推理 serving 默认仅 llama.cpp；技术栈补实测版本：hayro 0.8.0、sympy 1.14 + Python 3.12、rust_xlsxwriter 0.102、Slint 1.18.1。
- 抽检语义确定：抽检比例默认 0.20（可调），低置信度权重 0.70；抽样随机种子记录并写入审计，保证可复现；抽检不通过弹窗询问重跑（默认单题、可选整卷）。
- 许可：依赖白名单扩展为 10 项，项目整体 GPL-3.0-only，Slint 走 GPL-3.0-only 分支（不启用 Royalty-free 归属、不购商业许可）；`deny.toml` 改为 fail-closed。
- `CONFIG.md`：新增 `identity.ocr_threshold`/`identity.ocr_margin`、`fast.sampling_seed`、`log.index_range_days`/`log.index_rebuild` 等键；`fast.sampling_rate` 由 0.70 调整为 0.20（权重仍 0.70）。
- 模块规范同步修订：`SPEC-align`（身份只看文件名、OCR 兜底默认关、阈值引用 `CONFIG.md`）、`SPEC-report`（快速统计单列、抽检率与方法写入报表说明、`fast.export_label`）、`SPEC-review`（抽检只读、抽检不通过弹窗重跑）、`SPEC-app`（十页导航、中文 IME 人工验收清单）、`SPEC-capture`/`SPEC-inference`/`SPEC-rubric`/`SPEC-platform` 等。

### 新增（文档）

- 用户手册 8 篇：`docs/user/quick-start.md`、`import-and-naming.md`、`modes.md`、`review.md`、`rubric.md`、`export.md`、`troubleshooting.md`、`privacy.md`（M10 初稿，未实现步骤标注「计划中」）。
- ADR 0002–0009：两层判分、双模式与加权抽检、规则草稿确认、文件名身份、任务级云端授权、CAS 扩展与降级、规范无示例代码、GPL/Slint 许可。
- `docs/book/SUMMARY.md` 增加「用户手册」与完整 ADR 导航；mdBook 静态站重新生成。

### 说明

- 本版本仍不含可运行的桌面应用：`platform`、`subject-math` 等模块在实现中，Slint 外壳为 M9 计划；用户手册描述目标行为，未实现步骤已标注「计划中」。

## [0.2.2] - 2026-11-02（多选部分给分与全国卷卷型状态）

### 变更

- `SPEC-domain.md` v0.2.2：`ScoringMode` 新增「按选项部分给分」；新增 `Rubric.partial_choice` 与 `PartialChoiceRule`/`PartialChoiceTier`/`PartialChoiceSource`；新增不变量 5–11（与题型/评分项自洽、模式互斥、档位完整性、档位数值约束、不超满分、错选归零、来源可追溯）；`grade` 只读档位数据、不推算也不默认 0 分。
- `SPEC-rubric.md`/模板数据：全国卷模板包区分 active/retired——现行仅新课标Ⅰ/Ⅱ（2024 年起 19 题/150 分），全国甲卷（2026 年起退出）、全国乙卷（2024 年起停止命制）标 `retired` 只读快照；多选档位按官方口径写入模板数据（2 选项选对 1 个得一半分；3 选项选对 1 个得三分之一、选对 2 个得三分之二；错选 0 分）。
- `CONFIG.md`：新增 `rubric.partial_choice_enabled`（默认开，关闭时含档位的规则在校验期错误级拦下）、`rubric.national_sets`（现行/历史分组）。

### 说明

- 本版本为规范与模板数据修订，实现见 `eg-domain`/`eg-rubric`/`eg-grade` 的 T13/T15/T18 任务；判分侧验收标准见 `SPEC.md` S14（v0.2.4 补登）。

## [0.2.1] - 2026-11-02（文件名身份与配置总表）

### 新增

- `CONFIG.md`（配置项总表与默认值）与 `AGENTS.md`（检索优先的实现纪律）进入仓库基线；`dev.research_first` 恒开。
- 身份改为「只看文件名」：`identity.ocr_fallback` 默认关，文件名未命中即未决人工；条码路径默认关；仅在选文件时提示规范命名。
- 用户答复：无用户/登录（审计 actor 固定 `local-teacher`）；标准/快速双模式；云端按任务鉴权；原图永久保留；不做分发打包；数学覆盖根式/三角/向量/矩阵/区间/集合。
- 规范禁止示例代码（含代码块、JSON、Rust/Slint 片段）；`scripts/docs-check.sh` 增加规范围栏扫描。

### 说明

- 本版本为中间修订，后续修订见 0.2.3。

## [0.2.0] - 2026-11-01（第二版规范）

### 新增

- 新增 `rubric` 模块（评分规则：手写/模板/模型整理 → 草稿 → 教师确认 → 生效），模块规范从 11 份增至 12 份。
- 标准/快速双模式（阈值差异、按比例抽检、模式写入每条判分记录）；新增 `GradeMode`、`sampled`、`SubmissionExclusion`、`PaperTemplate`、`Submission.source_file_name`/`source_stem` 等类型与字段。
- 任务级云端授权（`TaskConsent`：范围、预算、任务生命周期）；`ModelCall` 记录 `task_id`/`consent_id`。
- 完整日志与文档系统要求；`docs/` 目录、mdBook 静态站、文档门禁。
- 不支持项明确：成绩单 PDF、应用签名与分发打包、学生端、账号与权限、非数学学科判分实现。

### 变更

- 身份从「条码 → 学号 → 姓名」改为「文件名优先」（后续 0.2.1 进一步改为只看文件名）。
- 原图保留策略由「可配置保留期」改为「永久保留、不自动清理」。

## [0.1.0] - 2026-11-02（M0 工程骨架）

### 新增

- Rust workspace（edition 2024、`resolver = "3"`、`[workspace.lints]`）与 13 个空 crate：`eg-domain`、`eg-platform`、`eg-capture`、`eg-inference`、`eg-recognize`、`eg-align`、`eg-rubric`、`eg-grade`、`eg-subject-math`、`eg-review`、`eg-report`、`eg-app`、`egctl`。
- 工具链固定入口 `rust-toolchain.toml`（stable + rustfmt + clippy），实测版本见 `dev/research/toolchain.md`。
- 依赖许可门禁 `deny.toml`（白名单 MIT / Apache-2.0 / BSD-3-Clause / ISC / Unicode-3.0）。
- CI 门禁脚本 `scripts/ci.sh`（fmt + clippy + test + deny + 文档），文档脚本 `scripts/docs.sh`、`scripts/docs-check.sh`。
- 文档系统骨架：`docs/README.md`、`docs/user/`、`docs/dev/`、`docs/dev/research/`（含模板）、`docs/adr/`（含模板与 ADR-0001）、`docs/book/`（mdBook，GitHub Pages 目标）。
- `README.md` 项目骨架与 `docs/changelog.md`。

### 说明

- 本版本不含任何业务实现；各 crate 仅有文档注释与空入口。
- 依赖版本尚未引入；`Cargo.lock` 随 workspace 生成并入库。
