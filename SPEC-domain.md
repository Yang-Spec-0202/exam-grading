# SPEC-domain：领域模型与学科插件契约

> 模块 id：`domain` · 依赖：无（纯类型 + 纯函数，零 IO、零数据库、零网络）
> 上游：`CAPABILITY-MAP.md` · 全局约定见 `SPEC.md`（v0.2，**规范不含示例代码**）
> 状态：草案 v0.2.2 · 2026-11-02 · **待审批**（v0.2.2 新增「按选项部分给分」与全国卷 active/retired 语义）
> 说明：本文件用「类型 + 字段表 + 不变量」描述契约，不提供任何语言层示例。字段名为最终命名，实现时逐字使用。

## 1. 目标与边界

**目标**：用一套与学科无关的类型，描述「试卷—题目—参考答案/评分标准/规则草稿—学生提交—识别结果—判分结果—证据链—运行模式—任务授权」，并定义学科插件与比较器契约，使数学成为第一个实现、后续学科不改 core。

**包含**：实体与值对象、不变量、枚举与状态机、序列化约定、学科契约、规则草稿与标准模板、模式与授权类型。
**不包含**：数据库表结构（`platform`）、HTTP/模型调用（`inference`）、UI 类型（`review`/`app`）。

## 2. 基础值对象

| 类型 | 表示 | 不变量 |
|---|---|---|
| `Ulid` | 26 字符 Crockford Base32 文本 | 全局唯一、按时间可排序、单调不倒退 |
| `StudentId`/`ClassroomId`/`PaperId`/`QuestionId`/`SubmissionId`/`PageId`/`SegmentId`/`RecognitionId`/`GradeId`/`ModelCallId`/`RubricId`/`RubricItemId`/`ReviewActionId`/`TaskId`/`ConsentId` | 各自包装 `Ulid` 的新类型 | 一经生成不可变；跨模块不得用裸字符串 |
| `ContentHash` | BLAKE3-256，64 位小写十六进制 | 由字节内容决定，不可伪造 |
| `AssetId` | 包装 `ContentHash` | 资产即内容：同字节 → 同 `AssetId` |
| `MilliScore` | 千分之一分的整数（`2.5 分 = 2500`） | 非负；单题 ≤ 满分；加减不溢出；禁止用浮点比较/汇总 |
| `Confidence` | 0.0–1.0 的浮点包装 | 构造时校验非 NaN、在区间内；模型原始值与校准后值必须区分标注 |
| `Sha256Hex` | 64 位小写十六进制 | 用于提示词/输入/配置指纹 |
| `Clock` | 可注入时钟接口 | 业务代码不得直接读系统时间；测试注入固定时钟 |

辅助规则：`MilliScore` 提供「从分数字符串解析」「格式化回分数字符串」「受检加法」；解析失败返回类型化错误，不 panic、不 clamp。

## 3. 试卷与题目

**Classroom（班级）**：字段 `id`（`ClassroomId`）、`name`、`term`。

**Student（学生）**：字段 `id`、`classroom_id`、`student_no`、`name`、`source`（`RosterSource`：名单导入/手工新增/文件名推断）。

**Paper（试卷）**：字段 `id`、`classroom_id`、`title`、`subject`（`SubjectId`，v1 恒为 math）、`total_score`（`MilliScore`，等于各题满分之和）、`questions`（按 `ordinal` 升序）、`key_version`（答案键修订号，每次修改 +1）、`rubric_version`（规则版本，规则变更 +1）、`created_at`。

**Question（题目）**：字段 `id`、`paper_id`、`ordinal`（卷面顺序，从 1 起）、`label`（如 `17(2)`）、`kind`（`QuestionKind`）、`max_score`、`stem`（可选，`ContentRef`）、`key`（`AnswerKey`）、`rubric`（可选，`Rubric`）、`subject_hint`（可选）、`knowledge_tags`。

**QuestionKind（题型）**取值：单选、多选、判断、填空、纯数值、计算/解答、证明、简答、论述、其他。

**ContentRef（题干内容引用）**取值：纯文本、LaTeX 文本、资产引用（`AssetId`）。

**PaperTemplate（试卷模板）**：字段 `paper_id`、`version`（与 `Paper.key_version` 对齐）、`page_count`、`regions`（`TemplateRegion` 列表）。

**TemplateRegion（模板区域）**：字段 `page_no`、`question_id`、`bbox`（归一化 0–1，左上原点）。

不变量：

1. `Paper.total_score` 必须等于各题 `max_score` 之和，否则导入失败。
2. 题目 `label` 在试卷内唯一；`ordinal` 连续无空洞。
3. 模板 `version` 与 `Paper.key_version` 不一致时，禁止自动对齐（必须人工）。
4. 答案键或规则的任何修改都必须使对应版本号递增；历史判分保留旧版本引用，不回溯改历史。

## 4. 参考答案与评分标准

**AnswerKey（答案键）** 为四选一枚举：

| 变体 | 含义 | 附加约束 |
|---|---|---|
| 具体答案 | 有确定答案，可规则比较 | 必须给出 `ConcreteKey` |
| 仅评分标准 | 只给评分标准、无具体答案 | 必须存在有效 `Rubric` |
| 明确省略 | 写作「略」「见教材」等 | 不得同时有具体答案；可带备注 |
| 缺失 | 教师未提供 | 不得推断、不得让模型补 |

**ConcreteKey（具体答案键）**：字段 `primary`（`AnswerValue`）、`alternatives`（多解/等价写法列表）、`comparators`（比较器 id 列表；为空时由学科包按题型给默认链）。

**AnswerValue（答案值）** 取值：文本、数值（值 + 可选单位 + 容差）、表达式（LaTeX）、选项集合、分步值列表、结构化扩展（由学科包解释）。

**Tolerance（容差）**：绝对容差（十进制数，可选）、相对容差（百万分比整数，可选）。未配置时按严格相等，禁止默认放宽容差。

**Rubric（评分标准）**：字段 `id`（`RubricId`）、`version`、`items`（`RubricItem` 列表）、`mode`（`ScoringMode`）、`total`（必须等于各评分项分值之和；按选项部分给分时为该题满分）、`origin`（`RubricOrigin`）、`partial_choice`（可空；`mode` 为按选项部分给分时必填，其余模式必须为空）。

**RubricItem（评分项）**：字段 `id`、`criterion`（可读判据）、`points`（`MilliScore`）、`required`（是否必须满足）、`depends_on`（前置评分项 id 列表）。

**ScoringMode（计分方式）**取值：累加、全对才给分、分步给分、**按选项部分给分**。累加为各评分项独立求和；全对才给分只允许 1 个评分项；分步给分时某评分项得分当且仅当「判定满足」且「所有前置项满足」；按选项部分给分仅用于多选题，按「命中正确选项数」查档位表给分，任一错选该题计 0，档位来自 `PartialChoiceRule`，**禁止在判分时临时推断档位**。

**PartialChoiceRule（部分给分档位规则，v0.2.2 新增）**：仅用于多选题的部分给分，字段如下：

| 字段 | 类型 | 约束 |
|---|---|---|
| `correct_option_count` | 整数 | 正确选项数，取值 2 或 3（现行官方口径）；其它取值视为超出范围，必须人工处理 |
| `tiers` | `PartialChoiceTier` 列表 | 覆盖 1 至 `correct_option_count` 减 1 的全部命中数；`selected_correct` 唯一、升序、不得跳档 |
| `wrong_selection_zero` | 布尔 | **恒为真**（错选归零不可配置）；写入数据只为审计可读，不得设为假 |
| `source` | `PartialChoiceSource` | 口径来源：官方口径、模板数据、教师自定义；进判分证据与审计 |
| `source_note` | 文本（可空） | 口径出处与适用年份；`source` 为官方口径时必填 |
| `total_milli` | `MilliScore` | 该题满分，必须与 `Rubric.total` 一致 |

**PartialChoiceTier（档位）**：字段 `selected_correct`（命中的正确选项数）、`points`（该档得分，`MilliScore`）、`label`（可空，面向教师的说明）。

**PartialChoiceSource（档位来源）**取值：官方口径（当次考试评分细则）、模板数据（内置全国卷模板随包）、教师自定义。

口径与数据分工：现行官方部分给分口径为「2 个正确选项选对 1 个得该题一半分；3 个正确选项选对 1 个得三分之一、选对 2 个得三分之二；有错选得 0 分」。**具体分值由模板数据或规则数据给出，本规范只约束形状、不变量与来源可追溯**；档位数值不得写进判分代码或由模型推断。

**RubricOrigin（规则来源）**：教师手写、标准模板、模型整理后经教师确认；任何来源都必须记录确认人与确认时间（v1 确认人固定为本地教师）。

不变量：

1. `Rubric.total` 等于评分项分值之和；全对才给分模式只允许 1 个评分项。
2. 「仅评分标准」「明确省略」「缺失」三种答案键下禁止存在 `ConcreteKey`；反之亦然（防模型补答案）。
3. 规则不可执行（分值为 0/负、依赖成环、依赖不存在、题型不支持）时，校验返回错误级问题，禁止开始判分。
4. 规则生效必须经教师确认；模型整理结果在确认前只能作为草稿。
5. **部分给分与题型/评分项**：`mode` 为按选项部分给分时，题型必须为多选，且 `items` 必须有且仅有 1 个「全对」评分项（其分值等于满分，使 `Rubric.total` 与档位规则自洽）；单选题、判断题、填空题等使用该模式一律校验错误级（单选无部分给分语义）。
6. **模式互斥**：按选项部分给分 ⇔ `partial_choice` 非空；累加、全对才给分、分步给分 ⇒ `partial_choice` 必须为空；两者并存或缺失均校验错误级。
7. **档位完整性**：`tiers` 必须覆盖 1 至 `correct_option_count` 减 1 的每个命中数；缺档即校验错误级并禁止生效（**不得默认 0 分**，避免静默低估学生得分）。
8. **档位数值**：每档 `points` 严格大于 0 且严格小于 `total_milli`，随 `selected_correct` 单调不减，且为评分粒度的整数倍；全部选对由 `Judgement` 的「一致」给满分，**不走档位**。
9. **不超满分**：任一档分值 ≤ 该题满分；若规则数据声明「档位分值合计」，其值不得超过满分；部分给分只取命中的单档，**禁止把多档分值相加**（档位不是累加项）。
10. **错选归零**：存在任一错选时该题计 0 分，不查档位；错选不得被忽略、不得降级为「部分一致」。
11. **来源可追溯**：`source` 为官方口径时必须带 `source_note`（年份/文件），否则校验错误级；教师自定义档位必须经确认才能生效。

## 5. 规则草稿与标准模板（v0.2 新增）

**RuleText（规则原文）**：字段 `question_id`、`text`（教师自然语言规则，可为空 = 使用模板）、`source`（`RuleTextSource`：手写/模板/导入）。

**RubricDraft（规则草稿）**：字段 `id`、`question_id`、`status`（`DraftStatus`：草稿/待确认/已确认/已拒绝）、`items`（`RubricDraftItem` 列表）、`raw_text`（原始规则文本）、`origin`（`DraftOrigin`：手写直译/模型整理/模板套用）、`model_call_id`（模型整理时必填，便于复现）、`validation`（`RubricIssue` 列表）、`created_at`、`confirmed_at`。

**RubricDraftItem**：字段 `criterion`、`points`、`required`、`depends_on`（引用草稿内序号）。

**DraftOrigin** 取值：手写直译、模型整理、模板套用。

**模板**：按 `QuestionKind` 提供标准规则模板，模板本身有 `template_id` 与 `version`；套用模板生成的草稿仍需教师确认。

不变量：模型整理只允许产出草稿，**永远不能直接生成生效的 `Rubric`**；草稿校验不通过时确认操作被拒绝。

## 6. 提交、页面、答案区域

**Submission（提交）**：字段 `id`、`paper_id`、`classroom_id`、`student_id`（可空，未匹配时必须人工补）、`source_file_name`（原始文件名，含扩展名）、`source_stem`（去扩展名文件名，身份匹配第一来源）、`pages`、`status`、`created_at`。

**SubmissionStatus（提交状态）**取值：已导入、已预处理、已识别、已对齐、已判分、已复核、已导出、失败（附阶段与消息）。

**Stage（阶段）**取值：采集、预处理、识别、对齐、判分、复核、导出。

**Page（页面）**：字段 `id`、`submission_id`、`page_no`、`asset`（原图资产，只读）、`width`、`height`、`rotation_applied`（0/90/180/270）、`exif_orientation`（1–8，0 表示无 EXIF；镜像信息不可丢）、`quality`（`PageQuality`）、`derived`（派生资产列表，顺序固定为「方向校正图、透视矫正图、二值图、缩略图」）。

**PageQuality**：字段 `blur_var`、`exposure`、`skew_deg`、`resolution_ok`、`verdict`（`QualityVerdict`：良好/可接受/拒绝附原因）。

**Segment（答案区域）**：字段 `id`、`page_id`、`question_id`（可空 = 未对齐）、`bbox`（归一化 0–1）、`crop`（裁片资产）、`method`（`SegmentMethod`：模板区域/版面检测/人工）。

**BBox**：`x`、`y`、`w`、`h`，归一化 0–1，左上原点；`w`、`h` 必须为正。

不变量：原图资产只读；页面坐标一律归一化；同一页面同一题最多一个主区域（跨页题按页多段）。

## 7. 识别结果

**Recognition（识别结果）**：字段 `id`、`page_id`、`segment_id`（可空；页级识别为空）、`blocks`（按阅读顺序）、`text`（拼接纯文本）、`engine`（`EngineStamp`）、`confidence`、`created_at`。

**Block（识别块）**：字段 `kind`（`BlockKind`）、`bbox`、`text`、`latex`（可空）、`confidence`。

**BlockKind（块类型）**取值：文本、公式、表格、手写、图形、印章、条码（保留，v1 不产出）、页眉页脚、未知。

规则：识别结果是不可信输入；公式必须能解析为合法 LaTeX 才进入学科包，解析失败按「不可判定」处理并进人工，不得猜测。

## 8. 判分结果、模式与证据

**RouteDecision（L1 路由结果）** 取值：仅规则、规则优先再升级模型、直接模型判定、按评分标准逐项判定、仅人工（附原因 `ManualReason`）。

**ManualReason（转人工原因）** 取值：答案键缺失、答案键省略、规则缺失、识别不可用、置信度不足、超出范围、冲突未解决、后端不可用。

**Judgement（L2 判定结果）** 取值：一致（得分 + 理由 + 证据）、部分一致（得分 + 满足项 + 缺失项 + 理由）、不一致（得分 + 理由 + 证据）、不可判定（原因 `UngradableReason`）。按选项部分给分时：无错选且未全对 → 「部分一致」，`satisfied` 为命中的正确选项、`missing` 为未选中的正确选项，`awarded` 取档位分值；存在错选 → 「不一致」且 `awarded` 为 0（不得用「部分一致」表达错选）。

**UngradableReason（不可判定原因）** 取值：未作答、识别不可用、答案键不完整、超出范围、后端不可用、超时、预算超限、Schema 不合法、冲突、策略拦截。

**JudgeOutcome（模型判定产出）**：字段 `judgement`、`confidence`（模型原始概率，校准前）、`matched_alternative`（命中的候选序号，可空）、`calibration_id`（无校准记录时为空；为空禁止自动放行）。

**GradeMode（运行模式）**：标准、快速。模式写入每条判分记录，并决定阈值、抽检与导出标注。

**RouteTrace（路由轨迹）**：字段 `rule_id`（命中的规则编号）、`inputs_hash`、`classifier_call`（可空，仅分类澄清时有）、`decided_at`。

**GradeRecord（判分记录）**：字段 `id`、`submission_id`、`question_id`、`revision`（追加版本，从 1 起）、`mode`（`GradeMode`）、`route`、`route_trace`、`judgement`、`awarded`、`max_score`、`confidence`（校准后）、`status`（`GradeStatus`）、`evidence`（`EvidenceRef` 列表）、`engines`（`EngineStamp` 列表）、`created_at`、`reviewed_at`（可空）、`review_note`（可空）、`sampled`（快速模式抽检标记）。

**GradeStatus（判分状态）**取值：自动接受、待复核、已人工复核、排除。

**EvidenceRef（证据引用）** 取值：资产、识别结果、比较轨迹、模型调用。

**ComparatorTrace（比较轨迹）**：字段 `id`、`comparator`（比较器 id）、`verdict`、`detail`（结构化细节）。

**EngineStamp（引擎指纹）**：字段 `role`（`ModelRole`，取值标识 `ocr`/`router`/`judge`/`structurer`）、`backend`（`BackendId`，保留字 `local_server`/`openai_compat`/`fixture`）、`model_id`、`model_version`、`prompt_hash`（可空）、`params_hash`。

**ModelCall（模型调用记录）**：字段 `id`、`role`、`engine`、`task_id`（所属任务，按任务鉴权用）、`consent_id`（云端调用必填）、`input_hash`、`output`（已通过 Schema 校验的结构化结果）、`latency_ms`、`cost_micros`、`created_at`。

**云端授权有效期**：等于所属任务的生命周期（任务开始 → 终态），任务终态即失效；v1 不设跨任务时间窗，也不允许「整批一次授权」覆盖多个任务。

**ReviewReasonKind（复核原因分类）**取值：置信度不足、冲突、不可判定、证据缺失、未校准模型、后端不可用、分数非法。

**SubmissionExclusion（提交级排除）**：字段 `submission_id`、`reason`（`ExcludeReason`：缺考、无效提交、重复、超范围、其他）、`note`、`at`。

不变量：

1. `awarded` 非负且 ≤ `max_score`；`match` 必须满分，`mismatch` 必须 0，`partial` 必须在开区间内。
2. 自动接受仅当：置信度 ≥ 当前模式阈值、无冲突/不可判定、证据链完整、模型已校准（快速模式另有规则）。
3. 每条判分记录至少 1 条证据；模型参与时必须含 `ModelCall`；云端调用必须含 `consent_id`。
4. 判分记录只追加新版本，不原地修改；`review` 也写新版本。
5. 快速模式记录必须 `sampled` 可辨；导出与统计必须能按模式筛选。
6. **部分给分判分口径**：`grade` 判分时只读 `Rubric.partial_choice` 的档位数据，不自行推算或默认档位；无错选按命中数取对应档位给分，存在错选一律 0 分并判「不一致」；`partial_choice` 缺失、与 `ScoringMode` 不一致或档位不合法时必须判「不可判定」并转人工，**不得默认 0 分**。

## 9. 学科插件契约

**SubjectPack（学科包）** 必须提供：学科 id 与显示名；能力声明（支持题型、比较器列表、提示词版本、是否支持分步给分）；归一化（输入原始答案，输出可比较形式，失败必须显式报错）；比较器链（给定具体答案键与题型，返回按序尝试的比较器）；分步证据抽取（给定评分标准与归一化答案，输出步骤证据或空）；判定配置（返回判定 profile id）；静态校验（给定题目，返回规则问题列表）。

**Comparator（比较器）** 必须提供：比较器 id；比较（给定期望值与归一化答案，返回判定）。

**ComparatorVerdict（比较器判定）** 取值：相等、等价（附说明）、不相等（附原因）、无法判定（附原因）。链式规则：首个确定结论终止；全部无法判定 → 上游升级或人工；解析失败不得产生「不相等」。

**RawAnswer（原始答案）**：字段 `text`、`latex`（可空）、`blocks`、`source`（`AnswerSource`：识别/人工修正/电子文本）。

**StudentAnswer（对齐后的学生答案）**：字段 `submission_id`、`question_id`、`recognition_id`（主段，可空）、`segment_id`（主段，可空）、`raw`（已合并）、`parts`（每页一段 `AnswerPart`）、`recognition_confidence`（各段最小值）。

**AnswerPart（页面证据片段）**：字段 `page_id`、`segment_id`、`recognition_id`、`bbox`、`text`。

**NormalizedAnswer（归一化答案）**：字段 `text`、`latex`（可空）、`value`（可空，结构化值）、`steps`（步骤列表）。

**NormalizedStep**：字段 `ordinal`、`text`、`latex`（可空）。

**StepEvidence**：字段 `steps`（`StepMatch` 列表）。

**StepMatch**：字段 `rubric_item`、`matched`（指该评分点在答案中找到了可定位步骤，不等于满足）、`evidence`（可空，`BBox`）、`note`（可空）。

**SubjectCapabilities**：字段 `kinds`、`comparators`、`prompt_version`、`supports_step_scoring`。

**RubricIssue**：字段 `severity`（警告/错误）、`item`（可空）、`message`。

**RubricIssueSeverity** 取值：警告、错误。

**JudgeContext**：字段 `question`、`paper`。

**NormalizeError** 取值：无法解析（附输入与原因）、不支持（附原因）。

**ScoreError** 取值：负值、溢出、格式非法。

契约测试（所有学科实现必须通过）：比较器链顺序与终止规则；归一化幂等；解析失败不得判为不等；静态校验必须在导入期调用；有错误级问题时禁止开始判分。

## 10. 序列化与版本约定

- 所有持久化类型可序列化为 JSON，字段名 `snake_case`；仅**模型输出结构**使用严格模式（拒绝未知字段），历史记录使用宽容模式（忽略未知字段、未知枚举保留原值并告警）。
- 枚举使用带标签形式，便于审计阅读与模型输出校验。
- 存储：id 与枚举标签存 TEXT；分数存 INTEGER（千分之一分原始值）；时间存 RFC3339 文本；置信度存 REAL。
- 每类持久化对象带 `schema_version`；升级只新增字段并给默认值，禁止重命名后改历史数据。
- 版本号进证据：答案键版本、规则版本、识别引擎版本、模型版本、提示词哈希、配置哈希，缺一不可复现。

## 11. 失败语义（构造与校验）

- 构造失败一律返回类型化错误，**不 panic、不 clamp**：负分/溢出、置信度越界、坐标越界、空必填字段。
- 反序列化不信任：模型输出拒绝未知字段；历史记录宽容，不得因旧数据导致启动失败。
- 导入期校验（试卷/答案键/规则/模板）发现错误级问题时阻止开始判分；运行期不得把这类错误降级成 0 分。
- 不变量校验失败必须带字段名与中文可读原因，便于教师修正。

## 12. 测试策略（本模块）

- 单元：分数解析/加法/越界、置信度校验、坐标校验、试卷总分校验、规则合计校验、依赖成环检测、默认比较器链选择。
- 性质：分数加法结合律、分数往返解析、容差单调、模式枚举全覆盖、序列化往返稳定。
- 契约：`math` 实现与一个反例包（故意违规）分别通过/报错；所有端口的内存实现与真实实现跑同一套测试。
- 快照：每个公开枚举与结构的序列化形状；规则草稿确认前后的状态迁移。
- 部分给分：§4 不变量 5–11 逐条断言（2/3 选项官方档位、错选归零、缺档、跳档、超满分、模式互斥、来源缺失、与 `Rubric.total` 自洽）。

## 13. 边界

- **Always**：新增领域类型先写不变量与契约测试；分数只经 `MilliScore`；跨模块只暴露本文件类型；规则生效必须经教师确认；模式写入每条判分记录。
- **Ask first**：修改任何已发布类型的字段/枚举变体；改 `SubjectPack` 契约；改 JSON 标签；新增领域类型（先更新 §15 裁定表）。
- **Never**：在 `domain` 引入 IO/DB/HTTP 依赖；用浮点存分数；让模型输出直接构造具体答案键或生效规则；在规范中提供示例代码。

## 14. 成功标准（可验收）

1. 全部公开类型有契约测试；`math` 与反例包结果符合预期。
2. 每个枚举/结构体有序列化快照，字段变更即测试失败。
3. `eg-domain` 依赖闭包无 IO/DB/HTTP crate；禁止 unsafe；构造错误全部类型化（抽查 50 例无 panic）。
4. 不变量（§2–§8 列表）每条至少 1 个断言；行覆盖率 ≥ 95%。
5. 模式与授权字段在判分记录、模型调用记录中 100% 出现且非空。

## 15. Lead 类型裁定（2026-11-01，v0.2 更新）

### 15.1 已批准并入 domain（跨模块共享）

| 类型 | 说明 |
|---|---|
| `ContentHash`、`AssetId` | 资产内容寻址；`AssetId` 即内容哈希 |
| `GradeRecord.revision`、`GradeRecord.mode`、`GradeRecord.sampled` | 追加版本、双模式、快速模式抽检标记 |
| `Stage` | 流水线阶段，用于失败状态与日志 |
| `Page.exif_orientation` | 镜像信息可复现 |
| `BlockKind.Barcode` | 保留变体，v1 不由 OCR 产出 |
| `UngradableReason`、`PolicyBlocked` | 不可判定原因全集 |
| `JudgeOutcome` | 模型判定产出（含校准前置信度与校准 id） |
| `RouteTrace` | L1 路由轨迹，随判分记录持久化 |
| `PaperTemplate`、`TemplateRegion` | 模板区域，`align` 首选路径 |
| `SubmissionExclusion`、`ExcludeReason` | 提交级排除，区别于题级排除 |
| `ReviewReasonKind` | 复核队列排序与筛选 |
| `GradeMode` | 标准/快速 |
| `RuleText`、`RubricDraft`、`RubricDraftItem`、`DraftOrigin`、`DraftStatus` | 评分规则草稿与确认流程 |
| `Submission.source_file_name`、`Submission.source_stem` | 文件名即身份 |
| `Rubric.id`、`Rubric.version`、`Rubric.origin` | 规则版本化与来源追溯 |
| `ModelCall.task_id`、`ModelCall.consent_id` | 按任务鉴权与审计 |
| `ScoringMode` 的「按选项部分给分」、`Rubric.partial_choice`、`PartialChoiceRule`、`PartialChoiceTier`、`PartialChoiceSource` | v0.2.2：多选题部分给分档位能力（官方口径由模板/规则数据承载，错选归零，与全对模式互斥） |

### 15.2 已批准的模块自有公开类型（不进 domain）

| 模块 | 类型 |
|---|---|
| `platform` | 任务类型（`TaskKind`/`TaskState`/`TaskAttempt`）、配置结构、密钥引用、错误分类、`CancelToken`、`LogRecord`/`LogLevel`/`LogQuery`/`LogPage`/`LogIndexState`、`DiagnosticBundle`/`DiagnosticRequest`、`TaskConsentRecord`、`TraceId`、资产库/任务队列/密钥库/审计接口 |
| `inference` | 推理错误、OCR 请求与产出、判定请求（Rust 结构）、分类请求与产出、规则整理请求与产出（`RubricStructureRequest`/`RubricStructureOutput`）、健康报告、后端 id 保留字、任务授权结构（`TaskConsent`） |
| `capture` | 导入请求/事件/报告、参考答案页资产（`KeyPageAsset`）、流水线版本与派生图顺序 |
| `align` | 对齐输入/报告/指纹、候选学生、匹配依据、未决项与种类、`IdentitySource`（文件名/学号/姓名/条码/人工） |
| `recognize` | 识别错误、识别 IO、LaTeX 校验结果、结构质量、识别器接口、缓存键 |
| `rubric` | 规则草稿服务、模板库、结构化请求、校验报告、确认请求 |
| `grade` | 判分配置、模式阈值、判分错误、复核原因、提交级判分结果、题目失败；部分给分只读 `Rubric.partial_choice` 与 `PartialChoiceRule`，不新增自有类型 |
| `review` | 复核服务、队列过滤/分页/条目、复核命令与结果、复核动作、原因码、抽检请求 |
| `report` | 报表规格、导出运行、导出错误、报表行结构、导出命名规则 |

### 15.3 未获批准/推迟

- 任务类型不进 domain（UI 只拿进度快照）。
- 参考答案页不建模为 `Page`：由 `capture` 的 `KeyPageAsset`（模块自有）承载，一次性识别后由教师确认写入答案键。
- 脚本/条码解码、拼音匹配、CAS 实现细节：v1 不引入或按模块开放问题处理。
- §16 的开放问题保持开放，Plan 阶段按实测决定。

## 16. 开放问题

1. `AnswerValue` 的结构化扩展是否需要独立版本化 schema（v1 仅供数学使用）。
2. 多解答案与学生只给出部分解时的计分策略（当前在 `grade` 中定，需规则侧配合）。
3. `RubricItem.depends_on` 的语义是「前置必须命中」还是「仅排序」——当前按「前置必须命中」，确认后固化。
4. 快速模式的 `sampled` 由谁写入（`grade` 抽检还是 `review` 抽检）——当前约定 `grade` 写标记、`review` 只读。
5. 文件名与 OCR 身份冲突时的默认处置（未决 vs 文件名优先）——当前不一致即未决。
6. 部分给分档位随官方口径逐年变化，是否需要独立「赋分口径版本」类型（当前靠 `source_note` + 模板版本 + `Rubric.version` 追溯）。
7. 「不定项选择」等其它选择题型是否纳入部分给分（当前仅多选，`correct_option_count` 限 2 或 3）。
8. 错选以外的特殊口径（如错选扣分、少选得零但无错选可复查）是否需要建模；当前一律「错选归零」。
