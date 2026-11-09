# S2/S3/S5 合成金标集（`fixtures/gold/`）

> 状态：v1 · 2026-11-09T00:00:00Z · 生成器 `scripts/fixtures/generate_gold.py`（seed=20261109）· 自检 `scripts/fixtures/verify_gold.py`
> 用途：S2 客观题判分准确率、S3 解答题分步判分、S5 规则/分档聚合的**文本级**金标集；不替代真实试卷评估。

## 0. 诚实声明（先读）

- **全部为合成数据**：题干是占位描述（"合成单选题 #1"），**不是真实试卷/试题**；学生是 `学生A..F`/`GOLD-000x`
  合成身份，不含任何真实姓名、学校、班级或试卷原文。
- **期望值由规范规则推导，不是模型标注**：每条 `expected` 都可由 `SPEC-grade.md`
  的 L1 路由表（R0–R11）、比较器链、§5.3 聚合规则与 `SPEC-subject-math.md` 的归一化
  规则复算（`verify_gold.py` 独立重算并比对）。`confidence` 字段是按规范组合规则
  （规则段取识别置信度；模型段取 min(模型/识别/证据)）推导的**标签**，不是模型实测值。
- **不是准确率目标**：本数据集故意包含低置信、缺步、错中间值、单位缺失、未校准等
  扰动；eval 应报告「分扰动类别的准确率/复核率」，而不是追求 100%。
- **不含图片**：图像链路已由 S1 夹具 `fixtures/paper-20q` 覆盖；本目录只有文本 JSON。

## 1. 文件

| 文件 | 内容 | 条目数 | sha256 |
|---|---|---|---|
| `objective-500.json` | 客观题（单选/多选/判断/数值/表达式/文本填空） | 540 | `cf411e632c3d022a8198b1c4b35ab8748be8ca1c624240f4df298b9f83b2bbac` |
| `solution-200.json` | 解答/计算/证明/简答（分步 rubric + 学生步骤文本） | 230 | `b2c7ce3444284f431ec8ad643c2fd0793c0208938e4a3492f08c7afcc0aee97f` |
| `index.json` | 机器可读索引：计数、覆盖矩阵、规则映射、sha256 | — | — |
| `README.md` | 本文件（schema/覆盖/再生成/接入约定） | — | — |

规模：纯 JSON 文本，总 < 3 MB（由 `verify_gold.py` 检查）；无图片。

## 2. Schema（v1）

两个数据文件共享顶层骨架：

```jsonc
{
  "schema_version": 1,
  "dataset": "objective" | "solution",
  "generator_version": "1.0.0",
  "seed": 20261109,
  "created_at": "2026-11-09T00:00:00Z",
  "students": [{"student_id": "stu-A", "name": "学生A", "student_no": "GOLD-0001"}],
  "items": [ ... ]
}
```

### 2.1 客观题条目（`objective-500.json`）

| 字段 | 说明 |
|---|---|
| `qid` | 稳定 id（`obj-0001`，按生成顺序） |
| `kind` | 领域层 `QuestionKind`：`single_choice`/`multiple_choice`/`true_false`/`numeric_only`/`fill_in_blank`（可直接反序列化） |
| `answer_form` | 填空/数值的答案形态：`number`/`expression`/`text`（非填空为 null；用于 eval 分组与归一化断言） |
| `stem_summary` | 占位题干摘要（合成、非真实试题） |
| `max_score_milli` | 满分（毫元；单选/判断/填空 5000，多选 6000） |
| `answer_key` | `eg-domain` AnswerKey 的 JSON 形状（`concrete`/`rubric_only`/`explicitly_omitted`/`missing`） |
| `rubric` | 仅多选部分给分条目内嵌 `PartialChoiceRule`（其余为 null） |
| `raw_answer` | `eg-domain` `RawAnswer` 形状：`text`/`latex`/`blocks`/`source` |
| `recognition.confidence` | 识别置信度（规则推导用；`<0.75` 触发 R1） |
| `perturbations` | 本条的扰动标签（见 §3） |
| `expected.route` | `rule_only`/`rule_then_model`/`rubric_itemized`/`manual`（SPEC L1 输出） |
| `expected.rule_trace` | 命中的 L1 规则编号（`R0`/`R1`/`R5`/`R7`…） |
| `expected.normalized` | 归一化的**语义**期望：`{kind: options\|boolean\|number\|expression\|text\|empty, ...}`；表达式用 `compare:"expression_equivalence"`（不锁定逐字符文本） |
| `expected.judgement` | `match`/`partial`/`mismatch`/`ungradable` |
| `expected.score_milli` | 毫元；`ungradable` 为 null；partial 必须落在档位 |
| `expected.status` | `auto_accept`/`needs_review`/`manual`（映射见 §4） |
| `expected.review_reason`/`manual_reason` | 待复核/仅人工原因（`low_confidence`/`ungradable`/`recognition_unavailable`…） |
| `expected.confidence` | 按 SPEC §6 组合规则推导的置信度标签 |
| `expected.partial_choice_tier` | 多选部分给分命中档位（无则 null） |

### 2.2 解答题条目（`solution-200.json`）

在客观题字段基础上：

| 字段 | 说明 |
|---|---|
| `rubric` | `eg-domain` `Rubric`（`mode` = `stepwise`/`additive`/`all_or_nothing`；`items[]` 含 `criterion`/`points`/`depends_on`） |
| `student_steps[]` | 学生分步文本（`ordinal`/`text`/`latex`/`confidence`）——对应 `StepEvidence` 的文本来源 |
| `judge` | 合成判定元数据：`raw_confidence`/`calibration_id`/`model_id`/`model_version` |
| `expected.aggregation` | 本条的聚合模式（与 rubric.mode 一致） |
| `expected.steps[]` | 逐评分项：`item_id`/`matched`（找到可定位步骤）/`effective`（分步依赖裁剪后实际计分） |
| `expected.score_milli` | 聚合得分（毫元；`manual` 为 null） |
| `expected.calibration_id` | 与 `judge.calibration_id` 一致（未校准为 null） |

## 3. 扰动标签与覆盖

Objective 扰动：`clean`（无扰动）、`ocr_noise`（OCR 噪声 + 置信度 0.70 → R1 manual）、
`ocr_noise_review`（噪声但置信度 0.80 → 待复核 low_confidence）、`fullwidth`（全角）、
`spacing`（多余空白）、`latex_variant`（等价 LaTeX/分数写法）、`wrong_choice`（错答）、
`partial_choice_2of3`（3 个正确选项选 2 个 → 档位 4000/6000）、`partial_choice_2opt`
（2 个正确选项选 1 个 → 3000/6000）、`missing`（空答 + 高置信 → R0）、
`missing_low_confidence`（空答 + 低置信 → R1）。

Solution 扰动：`clean`、`symbol_variant`（符号等价写法）、`multi_solution`（另一解法）、
`missing_step`（漏中间步骤）、`wrong_intermediate`（错误中间值）、`unit_missing`（缺单位）、
`answer_only`（仅最终答案）、`low_confidence`（判定置信度 0.72）、`uncalibrated`（无校准记录）；
另含特殊样例 `rubric_only`（仅评分标准，R4 `rubric_itemized`）与 `answer_key_omitted`
（「略」且无规则，R3 `manual`）。

### 3.1 Objective 覆盖矩阵（kind × perturbation；`fill_in_blank` 含表达式/文本两种 `answer_form`）

| kind \ perturbation | clean | fullwidth | latex_variant | missing | missing_low_confidence | ocr_noise | ocr_noise_review | partial_choice_2of3 | partial_choice_2opt | spacing | wrong_choice | 合计 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `fill_in_blank` | 14 | 8 | 8 | 14 | 0 | 13 | 6 | 0 | 0 | 14 | 13 | 90 |
| `multiple_choice` | 16 | 16 | 0 | 16 | 0 | 16 | 0 | 16 | 17 | 17 | 16 | 130 |
| `numeric_only` | 13 | 13 | 14 | 14 | 0 | 13 | 0 | 0 | 0 | 14 | 14 | 95 |
| `single_choice` | 16 | 16 | 0 | 16 | 17 | 17 | 16 | 0 | 0 | 16 | 16 | 130 |
| `true_false` | 16 | 0 | 0 | 16 | 0 | 16 | 16 | 0 | 0 | 16 | 15 | 95 |
| **合计** | 75 | 53 | 22 | 76 | 17 | 75 | 38 | 16 | 17 | 77 | 74 | 540 |

### 3.2 Solution 覆盖矩阵（kind × perturbation）

| kind \ perturbation | answer_key_omitted | answer_only | clean | low_confidence | missing_step | multi_solution | rubric_only | symbol_variant | uncalibrated | unit_missing | wrong_intermediate | 合计 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `calculation` | 0 | 14 | 15 | 15 | 14 | 15 | 0 | 14 | 14 | 14 | 15 | 130 |
| `proof` | 0 | 0 | 10 | 10 | 10 | 0 | 3 | 10 | 0 | 0 | 10 | 53 |
| `short_answer` | 5 | 8 | 8 | 0 | 0 | 8 | 2 | 8 | 8 | 0 | 0 | 47 |
| **合计** | 5 | 22 | 33 | 25 | 24 | 23 | 5 | 32 | 22 | 14 | 25 | 230 |

### 3.3 状态/路由分布

| 数据集 | status | route |
|---|---|---|
| objective | `{"auto_accept": 410, "manual": 92, "needs_review": 38}` | `{"manual": 92, "rule_only": 317, "rule_then_model": 131}` |
| solution | `{"auto_accept": 178, "manual": 5, "needs_review": 47}` | `{"manual": 5, "rubric_itemized": 225}` |

## 4. 规则 → 期望的推导（eval 应对齐）

| 规则 | 条件 | 期望 |
|---|---|---|
| R0 | 空答案且识别置信度 ≥0.75 | `rule_only`，judgement=`mismatch`，score=0，status=`auto_accept` |
| R1 | 识别置信度 <0.75（或归一化失败） | `manual`，judgement=`ungradable`，score=null，status=`manual`，reason=`recognition_unavailable` |
| R3 | 答案键「略」且无确认规则 | `manual`，`answer_key_omitted`，score=null |
| R4 | 仅评分标准（或「略」+确认规则） | `rubric_itemized`（**不是 manual**），按 rubric 聚合 |
| R5 | 具体答案 + 单选/多选/判断 | `rule_only`；比较器 Equal/Equivalent→满分；NotEqual→0；多选 partial 按档位 |
| R6 | 具体答案 + 分步 rubric + 计算/证明 | `rubric_itemized`，按依赖聚合 |
| R7 | 具体答案 + 填空/数值 | `rule_then_model`；比较器结论明确时不升级模型 |
| 自动接受 | route≠manual 且 confidence ≥ 阈值（仅规则 0.85、规则优先 0.90、规则逐项 0.80，标准模式）且校准 id 存在 | `auto_accept`；否则 `needs_review` |
| 聚合 | stepwise=依赖固定点后满足项之和；additive=满足项之和；all_or_nothing=全满足才满分 | `expected.score_milli`/`expected.steps[].effective` |

> 多选部分给分：无错选时按命中正确选项数查档（3 选正确选项规则：1 个→2000、2 个→4000、
> 3 个→6000；2 选正确选项规则：1 个→3000、2 个→6000）；有错选一律 0 分。

## 5. 再生成与自检

```bash
python3 scripts/fixtures/generate_gold.py          # 重新生成（确定性 seed=20261109）
python3 scripts/fixtures/verify_gold.py            # 自检：数量/字段/自洽/覆盖/字节稳定/体积
python3 scripts/fixtures/verify_gold.py --skip-bytes  # 只做结构/规则自检（跳过字节比对）

# 抽检与实现一致（Rust：eg-subject-math 归一化+比较器链 / eg-grade 聚合；/tmp 构建）
scripts/fixtures/smoke_gold.sh
```

`verify_gold.py` 独立重算期望（不复用生成器函数），校验：条目数 ≥500/≥200、字段完整、
答案键形状、rubric 分值/依赖/档位合法、期望 judgement/score/status 与规则自洽、
扰动覆盖 ≥6、状态/路由分布、README 矩阵一致、无真实姓名、总大小 <3MB。

## 6. 给 eval 任务的接入约定

eval 读取 `index.json`（包含两个数据文件的 sha256 与计数）或直接读两个数据文件。建议流程：

1. 按 `kind`/`perturbation` 分组统计；每条用 `expected.route` 选择判分路径：
   - `rule_only`/`rule_then_model`：用学科包比较器链跑 `raw_answer`，与 `expected.judgement/score_milli` 对比；
   - `rubric_itemized`：用 `expected.steps[].matched` 作为判定输入（模拟 judge 输出），
     走 `eg-grade` 聚合（`aggregate`/`partial_choice`），与 `expected.steps[].effective`、
     `expected.score_milli` 对比；同时把 `judge.calibration_id` 传给门控；
   - `manual`：只断言 route/status/manual_reason，不断言 score。
2. 归一化断言：`rule_*` 路径比较 `expected.normalized`（options 集合/boolean/number 文本/
   expression 语义等价/text 去空白）；`manual` 条目只作诊断，不判失败。
3. 自动接受：按 `expected.status` 断言；`needs_review` 条目同时断言 `review_reason`。
4. 输出按 (§3) 扰动类别的 accuracy / auto_accept_rate / review_rate，不用总体 100% 作目标。

### 6.1 已知边界（非 bug）

- `expected.normalized.expression` 只承诺**语义等价**（CSS 逐字符规范化可能不同）；
  逐字符文本以 `eg-subject-math` 实现为准，eval 不应对表达式锁定字符串。
- 合成学生 `学生A..F` 仅用于 eval 分组，不代表真实身份；`split` 为确定性划分
  （每 5 条 1 条 `dev`，其余 `eval`），可按需重划分。
- `confidence` 是规则推导标签；真实评测时应替换为被测实现给出的置信度再与
  `expected.status` 门限比较。
