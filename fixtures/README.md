# 仓库级端到端合成夹具（`fixtures/`）

> 状态：S1 端到端自动化夹具 · 2026-11-04 · 对应验收缺口 L9（`docs/dev/acceptance.md`）
> 版本：schema v1 · 生成器与环境见 §5 · 校验：`python3 fixtures/generator/verify_fixtures.py`

本目录提供一套**确定性、可重复生成**的「20 题数学试卷 + 3 名学生答卷 + 名单 + 答案键 +
评分规则 + OCR/判分 fixture + 逐题期望结果」，供 `eg-app`、`egctl` 端到端验收与集成测试复用。

## 0. 诚实声明（先读）

- **图片是程序渲染的印刷体，不是真实学生手写**：用系统字体（默认 macOS `Songti.ttc`）
  绘制**浅灰纸面 + 横线**的答题纸（浅灰而非纯白，避免被 capture 的质量门控判为过曝/眩光），
  包含姓名/学号/作答文本；数学表达式用 ASCII 记号（`x^2`）印刷，不是真实排版公式，
  也不是手写笔迹。
- **不含任何真实个人信息或真实试卷**：姓名（张三/李四/王五/赵六）、学号（SO2026-00x）、
  班级、题目文本全部为合成数据；题目题干仅写题型占位，不是任何真实考试原题。
- **不是官方评分细则**：rubric 判据为通用占位（如「因式分解正确」），只为覆盖
  「分步给分 / 多选部分给分 / 仅规则 / 略」等代码路径，不构成教学或考试依据。
- **字体不随仓库分发**：只提交渲染后的位图；再生成需要本机字体，路径可用
  `FIXTURES_FONT` 覆盖（见 §5）。不同机器的字体/渲染库可能产生不同字节，因此
  `MANIFEST.sha256` 是「本仓库交付字节」的校验基准，不是跨机器可复现承诺。
- 许可：合成数据与生成器脚本随仓库同一许可证（见仓库根 `LICENSE`）。

## 1. 目录结构

```
fixtures/
├── README.md                     # 本文件（字段语义、用法、再生成、待确认项）
├── MANIFEST.sha256               # 全部夹具文件 sha256（`sha256 相对路径`，按路径排序）
├── paper-20q/
│   ├── paper.json                # eg-domain Paper（20 题、总分 150000 毫元、内嵌 key/rubric）
│   ├── answer-key.json           # 逐题答案键（paper.json 中 key 的独立投影）
│   ├── rubric.json               # 逐题生效规则 + 激活信封（status=active、确认人/时间）
│   ├── template.json             # eg-domain PaperTemplate（2 页、逐题区域；align 模板路径）
│   ├── classroom.json            # eg-domain Classroom
│   ├── students.json             # eg-domain Student[]（3 名）
│   ├── roster.csv                # 人读名单（学号/姓名/文件名 stem/期望身份）
│   └── expected/
│       └── grades-expected.json  # 逐学生逐题期望（判定/得分/状态/复核原因/证据）
├── images/
│   ├── manifest.json             # 每个图片文件：sha256/字节数/尺寸/EXIF/期望 capture/align 结论
│   ├── 张三_p1.png, 张三_p2.png             # 规范命名（姓名 + 页码后缀），PNG
│   ├── 李四_期中_p1.jpg, 李四_期中_p2.jpg   # 规范命名（姓名_期中 + 页码后缀），JPEG
│   ├── SO2026-003_p1.png, SO2026-003_p2.png # 学号命名，PNG
│   ├── 王五_exif6.jpg                        # EXIF Orientation=6（存储逆时针 90°，需顺时针校正）
│   ├── 张三_p2-copy.png                      # 与 张三_p2.png **逐字节相同**（批内去重）
│   └── IMG_20261104_001.jpg                  # 相机文件名（规范外）：身份未决/提醒路径
├── ocr/
│   ├── index.json                # OCR 夹具索引（backend/记录/两种回放模式/负向样例）
│   ├── inputs/*.png              # 长边 2000px 的识别输入图（= `ocr.input_max_edge` 上限）
│   ├── prompts/*.txt             # 由 eg-recognize 渲染的确定性提示词（审计/复现用）
│   ├── pages/*.json              # eg-inference 夹具记录（key + 逐字节 output_raw）
│   └── negative/*.json           # 负向样例（缺 blocks、条码块），仅供拒绝路径测试
├── judge/
│   ├── index.json                # judge fixture 索引（input/prompt/响应/用例分类）
│   ├── prompt.txt                # 合成判分提示词（键计算的一部分；字节级可复现）
│   ├── inputs/*.json             # 规范化判分输入（学生答案 + 题/键/规则 + 置信度）
│   ├── responses/*.json          # eg-inference 夹具记录（match/partial/低置信/冲突）
│   └── negative/*.json           # 负向样例（match 却给 0 分）
└── generator/
    ├── generate_fixtures.py      # 图片 + 试卷/名单/规则/期望 + OCR/judge 规格
    ├── keygen/                   # Rust：渲染 OCR 提示词、算夹具键、写 MANIFEST
    ├── regenerate.sh             # 一键再生成（python → keygen → 自检）
    └── verify_fixtures.py        # 自检（结构/一致性/MANIFEST/图片白名单/EXIF/夹具键）
```

## 2. 数据模型与字段语义

### 2.1 试卷 / 名单 / 规则

| 文件 | 内容 | 校验要点 |
|---|---|---|
| `paper.json` | `eg-domain` 的 `Paper`（可直接 `serde_json::from_str::<Paper>`） | 20 题、题号 1–20 连续、标签唯一、`total_score=150000`（毫元）、`key_version=rubric_version=1` |
| `answer-key.json` | `{schema_version, paper_id, key_version, questions[{question_id,ordinal,kind,max_score_milli,answer_key}]}` | 与 `paper.json` 逐题 `key` 完全相同 |
| `rubric.json` | `{schema_version,paper_id,rubric_version,status,confirmed_by,confirmed_at,questions[{question_id,ordinal,kind,max_score_milli,rubric}]}` | `status="active"`、确认人 `local-teacher`；每题 rubric `total == max_score`；`partial_choice` 仅多选且档位 1..=n-1 唯一升序；`rubric_only` 题必有规则 |
| `template.json` | `eg-domain` 的 `PaperTemplate`（`version=paper.key_version`、2 页、逐题区域） | 区域页号 1–2、每题恰 1 个区域、bbox 归一化合法 |
| `students.json` / `roster.csv` | 3 名学生（`SO2026-001..003`）与文件名 stem | 学号唯一、姓名非空；`roster.csv` 列固定 |

题目构成（`paper.json`）：8 单选（5000 毫元/题）+ 3 多选（6000，含部分给分档位）+
4 填空（5000）+ 3 计算/解答（13000/15000/17000，分步 rubric）+ 1 证明（15000，仅规则）
+ 1「略」（12000，`AnswerKey::ExplicitlyOmitted`）= **150000 毫元**。

### 2.2 图片与 capture/align 期望（`images/manifest.json`）

每个条目：

```jsonc
{
  "file": "images/张三_p1.png",
  "sha256": "…", "bytes": 123456,
  "media_type": "image/png", "width": 2480, "height": 3507,
  "expected": {
    "kind": "student_page | duplicate_bytes | exif_rotated | irregular_name",
    "format": "png | jpg",              // 与 capture 白名单一致（jpg/png/tiff/pdf；HEIC 不支持）
    "source_stem": "张三_p1",           // capture 原样保留的 stem（align 身份输入）
    "identity": "confirmed | unresolved",
    "identity_student_no": "SO2026-001",// confirmed 时
    "identity_reason": "file_name_not_found", // unresolved 时
    "page_no": 1,                       // 页码后缀解析结果（1 起；无后缀为 1）
    "exif_orientation": 0,              // 6 = EXIF 旋转样例
    "rotation_applied": 0,              // 6 → 90（eg-capture 记录 CW90）
    "upright_width": 2480, "upright_height": 3507, // EXIF 样例校正后尺寸
    "duplicate": "unique | same_batch_duplicate",
    "quality_verdict": "good_or_acceptable"        // 断言非拒收；不锁定 Good/Acceptable
  }
}
```

EXIF 样例的 `width/height` 是**存储图尺寸**（3507×2480，逆时针旋转 90° 存储）；
`eg-capture` 的 `Page.width/height` 记录存储尺寸，`rotation_applied=90`，派生图应校正为
正向 2480×3507。

### 2.3 逐题期望（`expected/grades-expected.json`）

```jsonc
{
  "schema_version": 1,
  "paper": {"id":"…","total_score_milli":150000,"key_version":1,"rubric_version":1,"question_count":20,"page_count":2},
  "mode": "standard",
  "rubric_status": "active",
  "students": [{
    "student_no": "SO2026-001", "name": "张三",
    "identity": {"status":"confirmed","source":"file_name","stem":"张三","confidence":1.0},
    "submission_files": ["images/张三_p1.png","images/张三_p2.png"],
    "expected": {
      "status": "auto_accepted | needs_review",
      "total_milli": 132000, "max_total_milli": 150000,
      "questions": [{
        "question_id":"…","ordinal":1,"kind":"single_choice","max_score_milli":5000,
        "route": "rule_only | rubric_itemized | model_direct | manual",
        "judgement": "match | partial | mismatch | ungradable | null",
        "score_milli": 5000,                  // null = 未产出分数（冲突/仅人工）
        "status": "auto_accepted | needs_review",
        "review_reason": null,                // low_confidence | conflict | ungradable
        "manual_reason": null,                // answer_key_omitted 等（route=manual）
        "confidence": 1.0, "calibrated": true,
        "satisfied": [], "missing": [],       // rubric item id（partial 时）
        "evidence": "rule | judge | rule+judge | manual",
        "reason": "中文理由"
      }]
    }
  }],
  "extra_samples": [ … ]   // 相机文件名/重复字节/EXIF 样卷的期望（不判分）
}
```

语义约定：

- `status`：`auto_accepted`（可直接出分）/ `needs_review`（必须复核，`grade` 不得静默接受）。
- `route`/`judgement`/`score_milli` 与 `eg-domain` 的 `RouteDecision`/`Judgement`/`GradeStatus` 语义一致；
  冲突未决（Q16 王五）用 `judgement=null + status=needs_review + review_reason=conflict`。
- 覆盖点：Q9–Q11 多选部分给分（李四 3000/2000/3000）；Q17 李四低置信复核
  （confidence 0.62 < `ocr.confidence_floor=0.75`）；Q16 王五「规则一致 / 模型不一致」冲突；
  Q19 仅规则 + 分步 rubric；Q20「略」→ `manual` + `answer_key_omitted`。
- 每题 `score_milli` 与 `max_score_milli` 自洽：`match=满分`、`mismatch=0`、
  `partial∈(0,max)`、`ungradable=null`；分步题 `satisfied` 分值之和等于得分；
  `total_milli` 等于各题非空得分之和（自检脚本强制）。

### 2.4 OCR fixture（`ocr/`）

- 格式与 `eg-inference` fixture 后端一致：`{schema_version:1,key,model_id,model_version,prompt_schema_version,output_raw}`，
  `output_raw` 是**逐字节回放**的模型输出字符串（JSON 对象）。
- 键 = `eg_inference::fixture::fixture_key(model_id, model_version, prompt_schema_version, prompt, image_bytes)`；
  `prompt` 由 `eg_recognize::prompt::render_page_prompt(width,height,emit_latex)` 渲染，
  已存于 `ocr/prompts/<id>.txt` 供审计。
- 两种用法（`ocr/index.json.usage`）：
  1. **精确回放（推荐给 egctl/app 验收）**：`ImageInput.bytes = ocr/inputs/<id>.png` 的字节，
     宽高取 `index.json.records[].ocr_input_width/height`（长边 2000，满足
     `ocr.input_max_edge=2000`），`emit_latex=true` → 键逐字节命中。
  2. **语义映射（真实 capture 流水线）**：capture 的派生图字节与此处不同，按
     `records[].student_no + page_no`（或 `submission_stem + page_no`）取 `output_raw`，
     不要用键命中（否则会 FixtureMiss，属预期）。
- `output_raw` 形状：`{"schema_version":1,"page":{"width","height"},"blocks":[{"kind","bbox":[x,y,w,h],"text","latex","confidence"}]}`，
  块按阅读顺序；内容与图片上印刷内容一致（含 1 个 LaTeX 公式块、1 个低置信块 0.55）。
- `negative/`：缺 `blocks`（结构 Fatal）与 `barcode` 块（语义拒绝），只用于拒绝路径，
  **不得**当作正向回放。

### 2.5 Judge fixture（`judge/`）

- 同样是 eg-inference fixture 记录；键用 `judge/prompt.txt` 的字节 + `judge/inputs/<id>.json`
  的字节计算，二者均已落盘 → 可逐字节复现。
- `output_raw` 形状按 `eg_inference::schema::JudgeOutput::validate`：
  `{judgement, score_milli, reason, confidence, matched_alternative, calibration_id, satisfied, missing}`。
- 用例分类（`index.json.records[].case`）：`normal`（match/partial）、
  `low_confidence`（李四 Q17，confidence 0.62）、`conflict`（王五 Q16：模型 mismatch，
  与规则一致判定冲突）。
- `negative/`：`judgement=match` 但 `score_milli=0`，`JudgeOutput::validate` 必须拒绝。
- **待确认**：judge 提示词与输入封装目前没有产品级实现（`eg-grade` 只定义端口，组合根尚未
  定稿），本目录给出的是**合成约定**；app/egctl 接入时应以本目录 `prompt.txt`+`inputs/`
  作为回放键输入，或按 §6 反馈再对齐。

## 3. 与 app / egctl 的对接约定

1. **capture 导入**：把 `fixtures/images/*.{png,jpg}`（跳过 `manifest.json`）交给 `Importer`；
   期望见 `images/manifest.json`：
   - 6 份学生答卷 = 3 名学生 × 2 页（同批导入会各自建档）；
   - `张三_p2-copy.png` 与 `张三_p2.png` 逐字节相同 → 批内去重（`duplicates` 记 `same_batch`，
     不新建提交）；
   - `王五_exif6.jpg` → `exif_orientation=6`、`rotation_applied=90`、派生图正向；
   - `IMG_20261104_001.jpg` → 格式合法、但身份未命中（相机文件名，默认 OCR 兜底关闭）。
2. **align 身份**：stem 与 `roster.csv` 的 `file_stem` 对应；`王五_exif6` 通过 token「王五」
   命中 `SO2026-003`；`IMG_20261104_001` 预期 `file_name_not_found` 未决。
3. **recognize**：按 §2.4 的模式 A 精确回放或模式 B 语义映射；低置信块（李四 Q17 = 0.55）
   与 `grades-expected.json` 的 `low_confidence` 复核一致。
4. **grade**：以 `grades-expected.json` 为逐题断言口径；模型判定题（Q16 李四、Q17/Q19）
   用 `judge/` 回放；Q20 必须走 `manual`（`answer_key_omitted`），不得自动判分。
5. **路径约定**：消费者以 `fixtures/` 为根，不要硬编码绝对路径；图片文件名中的学生姓名
   仅用于合成夹具，真实产品禁止把姓名放进文件名逻辑。

## 4. 体积与文件数（本仓库交付）

| 项 | 值 |
|---|---|
| 总大小 | 约 2.3 MB（<5 MB 预算；精确值以 `verify_fixtures.py` 输出为准） |
| 文件数 | 70（含 `MANIFEST.sha256`）；最大单文件约 292 KB（EXIF 样例 JPEG） |
| 图片 | 9 张答题纸（2480×3507，浅灰纸面印刷体）+ 7 张 OCR 输入图（1414×2000） |
| OCR/judge 记录 | 7 条 OCR（6 学生页 + 1 名单外样卷）、8 条 judge（normal/low_confidence/conflict）；另有 4 个负向样例 |

## 5. 再生成

前置：`python3` + Pillow（本仓库实测 11.3.0）、Rust 工具链、中文字体（macOS 默认
`/System/Library/Fonts/Supplemental/Songti.ttc`；可用 `FIXTURES_FONT=/path/font.ttf` 覆盖）。

```bash
# 一键：图片/期望 → 夹具键/MANIFEST → 自检
fixtures/generator/regenerate.sh

# 只跑静态自检（不重新生成）
python3 fixtures/generator/verify_fixtures.py

# 真实 capture 导入 + 夹具键命中/负向拒绝 + align 身份冒烟（在 /tmp 构建，不进仓库）
fixtures/generator/smoke.sh

# 只验证 MANIFEST
shasum -a 256 -c fixtures/MANIFEST.sha256   # 在 fixtures/ 目录下执行，路径为相对本目录
```

`keygen` 在 `/tmp` 的临时 cargo 工程里构建运行，不向仓库写 `target/` 或 `Cargo.lock`；
它直接调用仓库内 `eg-recognize`/`eg-inference` 计算提示词与夹具键，避免在脚本里复制实现。

## 6. 待确认项 / 已知限制

1. **OCR 回放模式**：真实流水线送入识别的是 capture 派生图（去透视、白底），与本目录
   `ocr/inputs`（原图等比降采样）字节不同；验收若要求「真实派生图 + 精确回放」，需要
   在 capture 之后按派生图重算键（届时由 egctl 适配层完成，夹具只需保持 `index.json`
   的语义映射）。当前两种模式都已文档化。
2. **judge 提示词/输入封装**尚无产品实现；本目录的 `judge/prompt.txt` 与 `inputs/*.json`
   是合成约定，`app` 组合根定稿后若格式变化，需同步更新 `judge/index.json`（键会变化）。
3. **封面/题干**：答卷只含答案与页眉，不含题干原文；真实判分若需要题干上下文，应使用
   `paper.json` 的题目占位信息，不要把本夹具当真实试卷。
4. **手写**：全部为印刷体；手写识别/降级路径不在本夹具覆盖范围（需要真实手写样本，
   属 Ask first 获取范围）。
5. **字体差异**：不同机器/字体版本的渲染字节可能不同；`MANIFEST.sha256` 校验的是本仓库
   交付字节，再生成后如字节变化需接受并重新提交清单（生成器不会伪造稳定字节）。
6. **领域类型缺口**：`eg-domain::SegmentMethod` 无「题号文本」变体、`Rubric` 无 status 字段
   （以 `rubric.json` 信封 + `rubric_version` 表达生效）；与 `SPEC-align.md`/`SPEC-rubric.md`
   的偏差记录在各自研究记录中，本夹具不改变规范。
