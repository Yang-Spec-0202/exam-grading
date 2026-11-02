# 研究记录：subject-math（数学学科包：归一化/AST/比较器链 + 全符号比较器 + CAS 适配，T13–T14 / T26–T27）

> 状态：已完成（T13–T14 + T26–T27）· 2026-11-02 · 对应规范：`SPEC-subject-math.md` v0.2.1（§3–§5、§7、§9–§12）、`SPEC-grade.md` §5.1（比较器链语义）、`SPEC-domain.md` §9（`SubjectPack`/`Comparator` 契约）、`CONFIG.md` 数学键（`cas.*` 见 `CONFIG.md` §7）
> 规则：检索优先（`AGENTS.md`）：先查官方文档与实测版本，再写实现；本页记录来源、结论、版本、风险与规范偏差。

## 1. 模块与范围

- 模块 id：`subject-math`（crate `eg-subject-math`），依赖 `eg-domain`（只依赖契约类型，不依赖 `grade`/`inference`）。
- 本次实现的能力（`tasks/todo.md` T13–T14）：
  1. **T13 四层归一化**：字符层（Unicode NFKC、全角/半角、数学字母、运算符统一、上下标展开、零宽字符）、噪声层（「解：/答：/原式=」前缀、句末标点、空白合并、纯数值 OCR 修正 `l/I→1`、`O→0`、十进制逗号）、LaTeX 规范层（去 `$`/`\(\)`/`\[\]`、`\left/\right`、解包 `\text{}`/`\mathrm{}`、花括号配对校验）、结构层（解析为内部语法树并规范化）；产物为规范化文本/LaTeX/可求值结构化数值；解析失败显式报错、不猜测。
  2. **内部语法树**（§4）：数值（精确有理数）、变量、和、积、幂、分式、根式、方程/不等式、集合、区间、向量、矩阵、单位、布尔、选项；函数节点保留「可识别但超出 T14 范围」的写法（三角/微积分等），供 T26/T27 扩展。
  3. **T14 基础比较器与链**（§5 优先级 1/2/3/4/5/6/8/9/10/14）：选项精确、布尔精确、数值绝对容差（默认严格）、数值带单位（量纲/换算）、分数精确、表达式规范形（展开/合并同类项/常数折叠）、解集（一元 ≤2 次有理根）、区间等价（含区间↔不等式）、集合等价、文本规范化；链规则「首个确定结论终止、全部无法判定保留原因」。
- 明确不做（后续里程碑）：CAS 适配（T27）与三角/向量/矩阵/集合全符号比较（T26）；分步证据抽取（T15）；判定提示词与判定配置（T17）；规则静态校验完整 11 条（T15 的 `rubric`）。
- 质量门禁：`cargo test -p eg-subject-math` 全绿（98 个测试函数）、`clippy -D warnings`、`fmt --check`；行覆盖率 **84.03%**（TOTAL，`SPEC.md` §8 核心 crate ≥80% 达标）。

## 2. 问题清单

| # | 需要确认的外部行为/API/版本/限制 | 影响 |
|---|---|---|
| Q1 | 解析库选型：手写解析器还是引入 nom/pest/winnow/chumsky？各自版本、许可、MSRV、对 LaTeX 数学语法/错误定位的适配度？ | 决定可维护性与依赖面 |
| Q2 | 精确数值：如何实现「整数/小数/分数/科学计数，禁止二进制浮点比较」，并满足 §10「大整数/分数位数上限（建议 256 位）」？ | 决定数值类型与溢出策略 |
| Q3 | Unicode 规范化：NFKC 是否把全角数字、Unicode 数学字母映射为 ASCII？是否会破坏上下标语义（`x²`）？ | 决定字符层顺序与特殊处理 |
| Q4 | 单位换算需要哪些基础量纲与换算因子？量纲不同为何判「不相等」？不可识别单位如何处理？ | 决定单位比较器行为 |
| Q5 | 比较器链顺序/终止/「无法判定」语义在 `SPEC-grade.md` §5.1 与领域契约中的确切要求？ | 决定 `comparator_chain` 与链执行 |
| Q6 | 容差语义：绝对容差（十进制）与相对容差（百万分比整数）如何组合？默认严格相等的实现？ | 决定数值比较器与边界夹具 |
| Q7 | 解集/区间比较的可行边界（T14 不做 CAS）：一元一次/二次有理根、二次不等式、无理根、超越方程如何降级？ | 决定实现深度与「无法判定」原因 |
| Q8 | 归一化修正记录（§3 噪声层「每次修正必须记录进审计」）在领域契约（`NormalizedAnswer` 无审计字段）中如何承载？ | 决定 `normalize_with_trace` API |
| Q9 | 表达式规范形需要做到什么程度（展开/合并同类项/常数折叠），何时必须返回「无法判定」而非「不相等」？ | 决定多项式规范化与比较器判定边界 |
| Q10 | 与 `eg-domain` 契约（`SubjectPack`/`Comparator`）的对接：能力声明、`comparator_chain` 返回 trait 对象、静态校验签名 | 决定 `pack.rs` |

## 3. 来源

| # | URL | 文档版本/日期 | 类型 | 取用内容 |
|---|---|---|---|---|
| S1 | 仓库 `SPEC-subject-math.md` v0.2.1 | 2026-11-02 | 项目规范 | §3 四层归一化、§4 AST 节点、§5 14 级比较器与链规则、§9 静态校验、§11 失败模式、§12 测试策略 |
| S2 | 仓库 `SPEC-grade.md` v0.2.1 §5.1、§5.4 | 2026-11-02 | 项目规范 | 确定性比较器链「只规则/规则优先第一段」语义、多解与容差、单位默认要求一致 |
| S3 | 仓库 `SPEC-domain.md` v0.2.2 §9 | 2026-11-02 | 项目规范 | `SubjectPack`/`Comparator`/`ComparatorVerdict`/`NormalizeError` 契约与链式规则 |
| S4 | https://docs.rs/num-bigint/0.4.8/num_bigint/ | num-bigint 0.4.8（2026-11-02 查阅） | 官方文档 + 本地源码 | `BigInt::bits/sign/magnitude`、`BigUint::sqrt/nth_root`、`to_u64_digits` |
| S5 | https://docs.rs/num-rational/0.4.2/num_rational/ | num-rational 0.4.2（2026-11-02 查阅） | 官方文档 + 本地源码 | `BigRational`/`Ratio`：约分、`new`（分母 0 panic，需前置校验）、`numer/denom`、`Hash/Ord` |
| S6 | https://docs.rs/num-integer/0.1.47/num_integer/ | num-integer 0.1.47（2026-11-02 查阅） | 官方文档 | 整数 GCD/根运算支撑（随 num-rational/num-bigint 引入） |
| S7 | https://docs.rs/unicode-normalization/0.1.26/unicode_normalization/ | unicode-normalization 0.1.26（2026-11-02 查阅） | 官方文档 + 本地源码 | `UnicodeNormalization::nfkc` 迭代器；NFKC 兼容分解/重组合 |
| S8 | https://www.unicode.org/reports/tr15/ | UAX #15 Unicode Normalization Forms（2026-11-02 查阅） | 官方标准 | NFKC 语义：全角→半角、数学字母数字符号→ASCII、兼容字符规范化 |
| S9 | https://www.ams.org/arc/tex/amsmath/amsldoc.pdf | amsmath 用户指南（LaTeX 环境 `pmatrix`/`bmatrix`/`vmatrix`、`\frac`、`\sqrt`、`\vec`） | 官方文档 | LaTeX 数学命令与环境的最小覆盖集 |
| S10 | https://doc.rust-lang.org/std/primitive.char.html | Rust std `char`（2026-11-02 查阅） | 官方文档 | `char_indices`/`len_utf8` 等用于词法与位置跟踪 |
| S11 | `cargo info nom` / `pest` / `winnow` / `chumsky`（2026-11-02 实测） | nom 8.0.0（MIT, MSRV 1.65）、pest 2.9.5（MIT OR Apache-2.0, MSRV 1.83）、winnow 1.0.4（MIT）、chumsky 1.0.0-alpha.8（MIT, alpha） | 官方 registry 元数据 | 解析库候选版本/许可/MSRV（用于选型对比，未引入） |
| S12 | https://docs.rs/thiserror/2.0.21/thiserror/ | thiserror 2.0.21（2026-11-02 查阅） | 官方文档 | `derive(Error)` 与 `#[error]` 中文错误文案 |
| S13 | https://docs.rs/proptest/1.12.0/proptest/ | proptest 1.12.0（2026-11-02 查阅） | 官方文档 | 性质测试 `ProptestConfig { cases: 256 }` |
| S14 | https://docs.rs/serde_json/1.0.154/serde_json/ | serde_json 1.0.154（2026-11-02 查阅） | 官方文档 | 测试夹具构造 `Structured` 答案值（dev-dependency） |
| S15 | `docs/dev/research/domain.md` §3/§6 | 2026-11-02 | 本项目研究记录 | 领域契约类型、`float_roundtrip`、`Decimal`/`MilliScore` 约定 |

## 4. 结论

| 问题 | 结论 | 依据 |
|---|---|---|
| Q1 | **手写词法 + 递归下降/优先级解析器**，不引入第三方解析库。理由：①数学文法小且 LaTeX 特化，nom/winnow 仍需自写 AST/优先级层，pest 语法文件对中缀优先级与 LaTeX 环境不直观；②需要精确中文错误位置与「无法解析 vs 支持范围外」的分类控制；③chumsky 稳定版仍为 1.0.0-alpha，项目要求 stable；④减少依赖/许可面（虽候选均为 MIT/Apache 合规）。候选版本与许可见 S11，记录在案备 T26 扩展时复核。 | S1 §3、S11；实现 `src/parse.rs` |
| Q2 | 用 `num-bigint`/`num-rational` 的 `BigRational` 包装为 `Rational`（i128 不满足 256 位建议）；每次运算后检查分子/分母 `bits() ≤ 256`，超限返回 `NumError::TooLarge`，上层转「无法判定」；解析支持整数/小数/科学计数，精确等价（`1e-3` = `0.001`）。**代码中不出现 f64**。 | S4–S6、规范 §4/§10 |
| Q3 | NFKC 会把全角数字/字母、数学字母符号映射为 ASCII（S8），但也会把上标 `²` 变成普通 `2`——因此**先展开上下标**（`²`→`^{2}`、`₁`→`_1`），再做 NFKC；零宽字符与运算符统一在 NFKC 后处理；`√` 仅在后接操作数时归一化为 `\sqrt{}`（独立 `√` 保留给判断题比较器）。 | S7、S8；`normalize.rs` 测试 |
| Q4 | 基础量纲：长度（m/cm/mm/dm/km/μm）、质量（kg/g/mg/t）、时间（s/ms/min/h/d），面积体积与复合单位由量纲幂/商表达（`cm^2`、`m/s`、`km/h`、`m/s^2`）；换算因子到 SI；**量纲不同 = 不相等**（规范 §5 第 4 项），单位不可识别/不可换算 = 无法判定。无量纲计数单位（个/人/次…）按符号区分，符号不同 = 不相等。 | S1 §4/§5、`ast.rs` 单位表 |
| Q5 | 链按 `SPEC-subject-math.md` §5 顺序建默认链；若答案键显式给出 `comparators`，严格按声明顺序执行（契约要求顺序稳定，顺序变化须升版本）；链语义由 `eg_domain::contract::run_comparator_chain` 实现（首个 `is_conclusive()` 终止，全部无法判定保留最后一个原因）；「不相等必须给原因」「无法判定必须给原因」在单测/夹具中断言。 | S2 §5.1、S3 §9、`comparators.rs` |
| Q6 | 绝对容差来自 `Tolerance.absolute`（域层 `Decimal` → 精确有理数）；相对容差 = `|expected| × ppm / 1_000_000`；两者都配置时**任一满足即通过**（更宽松，且保证「容差单调」性质）；未配置容差 = 严格相等（差值必须为 0），**绝不默认放宽**。边界：差 = 容差通过、差 > 容差拒绝（夹具断言）。 | S1 §4/§5、S3 §4、`comparators.rs` |
| Q7 | T14 边界：解集支持一元一次/二次、判别式为有理平方（有理根）与无解（负判别式）；无理根、次数 >2、多变量 → **无法判定**（原因写明需 CAS/T26/T27）；区间支持一元一次不等式与区间语法（含无穷端点、开闭），区间↔不等式互通；二次不等式（并集）、端点含变量 → 无法判定；方程/集合只判**精确有理数解集**。 | S1 §4/§5/§11 |
| Q8 | `SubjectPack::normalize` 只返回 `NormalizedAnswer`（领域契约无法扩展字段），因此额外提供 **`normalize_with_trace`** 返回 `NormalizedMath { answer, corrections }`；`corrections` 记录字符层/噪声层/LaTeX 层每次修正（含 OCR 修正与十进制逗号），供 `platform` 落审计。`SubjectPack::normalize` 丢弃 trace（文档注明）。 | S1 §3、S3 §9；`normalize.rs` |
| Q9 | 表达式规范形：常数折叠 → 展开（含和式幂）→ 合并同类项（系数+单项式分组）→ 确定性排序（次数降序、结构序、常数最后）；多项式相等 = 展开后差为零；多项式差非零 → 可判「不相等」（多项式恒等式可靠）；非多项式（根式含变量/三角/函数/有理式变量分母）→ **无法判定**（不猜测因式分解/三角变形）；常量非多项式可精确求值时按精确值判定（`√8 = 2√2`）。 | S1 §4/§5、`canonical.rs` |
| Q10 | 实现 `SubjectPack`：`id`/`display_name`/`capabilities`（题型 10 种，比较器只列已实现 10 个，`supports_step_scoring=false` 诚实声明）/`normalize`/`comparator_chain`/`step_evidence`（T15 前返回 `None`）/`judge_profile_id`（`math-judge-v1` 占位，T17 实现提示词）/`validate_question`（§9 的 T14 可判定子集）；已通过 `eg-domain` 契约套件（`tests/contract.rs`）。 | S3、`pack.rs`、`tests/contract.rs` |

## 5. 版本实测

| 依赖/工具 | 实测版本 | 命令 | 结果摘要 |
|---|---|---|---|
| Rust / cargo | 1.99.1 / 1.99.1 | `rustc --version`、`cargo --version` | edition 2024 / resolver 3 |
| eg-domain | 0.1.0（workspace path） | `cargo build -p eg-subject-math` | 契约类型可用：`SubjectPack`/`Comparator`/`AnswerValue`/`Decimal`/`Tolerance`/`NormalizedAnswer` |
| num-bigint | 0.4.8 | `cargo add num-bigint@0.4.8` | `BigInt`/`BigUint`（bits/sign/magnitude/sqrt/nth_root/to_u64_digits） |
| num-rational | 0.4.2（依赖 num-bigint ^0.4） | `cargo add num-rational` | `BigRational`；`Ratio::new` 分母 0 会 panic → 封装时先校验 |
| num-integer | 0.1.47 | `cargo add num-integer@0.1.47` | 整数运算支撑（随 num-rational 引入） |
| num-traits | 0.2.19 | `cargo add num-traits@0.2.19` | `Zero/One/Signed` 等 trait |
| unicode-normalization | 0.1.26 | `cargo add unicode-normalization` | NFKC 迭代器（依赖 `tinyvec 1.13.3`） |
| thiserror | 2.0.21 | `cargo add thiserror` | 类型化错误与中文文案 |
| serde_json（dev） | 1.0.154 | `cargo add --dev serde_json` | 测试构造 `AnswerValue::Structured` |
| proptest（dev） | 1.12.0 | `cargo add --dev proptest` | 8 条性质 × 256 例 |
| 依赖闭包 | 无 IO/DB/HTTP/时钟库 | `cargo tree -p eg-subject-math --edges normal` | 仅 `eg-domain` + 精确数值 + Unicode 规范化 + 错误派生；许可证均为 MIT/Apache-2.0（在白名单内） |
| 测试 | 98 个测试函数：lib 57（含 3 条 proptest）+ `normalize_fixtures` 9 + `comparators_fixtures` 11 + `contract` 5 + `properties` 8（8×256 例）+ `coverage_paths` 8 | `cargo test -p eg-subject-math` | 全绿；归一化夹具 62 条（≥30），比较器夹具 68 条（≥40），每条「无法判定」断言原因非空 |
| 静态检查 | clippy 0 警告 | `cargo clippy -p eg-subject-math --all-targets -- -D warnings` | 通过（含 pedantic） |
| 格式化 | rustfmt stable | `cargo fmt -p eg-subject-math -- --check` | 通过 |
| 覆盖率 | cargo-llvm-cov 0.9.1 | `cargo llvm-cov -p eg-subject-math --summary-only` | TOTAL 行 **84.03%**（5247 行，缺 838）、区域 85.24%；`parse.rs` 79.80%、`canonical.rs` 77.84% 低于 80%（其余 ≥83%） |

## 6. 风险与限制

| 风险 | 影响 | 缓解/预案 |
|---|---|---|
| R1：解析器为手写子集 | LaTeX/文本写法超出子集即「无法解析」（转人工），不会误判 | 失败为类型化错误（`NormalizeError::Unparsable`），`grade` R1 转人工；错误原因含位置。新增文法需同步 §4 覆盖表（§13 Always） |
| R2：解集仅一元 ≤2 次有理根 | 无理根/高次/超越方程 → 无法判定 | 原因写明「需 CAS，T27/T26 后续」；CAS 接入时替换/补充 `equation_solutions` |
| R3：根式化简试除上限 `p·q ≤ 10^9` | 超大整数根式不化简 → 可能无法判定 | 不化简而非误判（规范化对未化简形式保留结构）；超限返回无法判定 |
| R4：单位表为有限白名单 | 未知单位/换算 → 无法判定；计数量词需在白名单 | 白名单集中在 `ast::COUNT_UNITS` 与 `base_unit`，扩展需同步测试与规范 §4 覆盖 |
| R5：大写字母组合按选项解释 | 数学中的大写变量乘积（如 `AB`）会被归一化为选项 | 归一化在数学解析前短路选项；文档记录该启发式；若冲突可在题目上下文层修正（`grade` 路由已知题型） |
| R6：`√` 启发式（后接操作数才视为根号） | 独立 `√` 保留给判断题；`√x` 归一化 `\sqrt{x}` | 已用字符层操作数扫描实现，测试覆盖 |
| R7：文本答案启发式（≥4 个纯字母单词 → 文本） | 极少数数学多字母变量可能被当文本 | 仅纯 ASCII 字母且无数字/运算符；数学多字母变量建议用 LaTeX 或下标 |
| R8：数值位长上限 256 位 | 超限 → 归一化 `Unsupported`/比较「无法判定」 | 快速失败，不静默近似（规范 §10） |
| R9：未实现比较器占位 | 答案键若声明 T26/T27 比较器，链会返回「无法判定」；能力声明未包含这些 id（禁止虚报） | 占位比较器保留 id 与原因；T26/T27 实现后加入 `IMPLEMENTED_COMPARATOR_IDS` 并升版本 |
| R10：分步证据/提示词未实现 | `supports_step_scoring=false`、`step_evidence=None`；`judge_profile_id`/`prompt_version` 为占位 | 属 T15/T17；能力声明诚实反映当前实现 |
| R11：性能预算未实测 | §10 要求归一化 P95 ≤5ms、链 ≤20ms | 当前实现为纯 CPU、无分配热点优化；建议集成阶段用 `egctl` 基准测量（T18/CP-C） |
| R12：覆盖率 84.03%（TOTAL） | `parse.rs`/`canonical.rs` 单文件低于 80% | 已通过 `coverage_paths` 补齐分支；后续随 T26 扩展继续补测；CI 用 `cargo llvm-cov` 守护 |

## 7. 与规范的偏差

> 原则：规范优先；以下为「规范未给实现方式」或「当前里程碑范围」的落地解释，未修改任何规范文件。

| 规范位置 | 偏差/解释 | 处理（改规范/改实现） | 审批状态 |
|---|---|---|---|
| §3 归一化四层 | `SubjectPack::normalize` 签名只返回 `NormalizedAnswer`（领域契约），无审计字段；实现以 `normalize_with_trace` 额外返回修正记录，`normalize` 丢弃 trace | 实现解释；建议平台层调用 trace 版落审计（platform T11）。若要求接口化，需 `SPEC-domain.md` 变更（Ask first） | 待 Lead 裁定 |
| §3 层序 | 判断题/选项题在**字符层符号统一之前**短路识别（否则 `×`/`√` 被统一为运算符，语义丢失）；噪声前缀剥离在短路前执行 | 实现顺序差异、语义等价；已在代码注释与测试说明 | — |
| §3 LaTeX 层 | `\left/\right`、`\,`、`\displaystyle` 由**解析器词法跳过**而非预处理删除；`\text{}`/`\mathrm{}` 在规范层解包（单位文本保留为 `\text{unit}` 以支持再次解析） | 实现解释（保证渲染↔解析往返） | — |
| §4 覆盖范围 | 向量/矩阵/集合/区间/单位节点已建模，但 T14 只做基础比较（向量/矩阵比较属 T26），函数节点保留未解析语义 | 与任务范围一致（T26/T27 后续） | — |
| §4 根式化简 | 仅对 `p·q ≤ 10^9` 试除提取完全平方因子；更大整数不化简（保留根式） | 实现限制；扩展需改进因式分解或引入 CAS | — |
| §5 比较器 7/11/12/13 | 三角恒等（CAS）、向量、矩阵、量纲一致比较器**未实现**，以「无法判定」占位；能力声明不列出 | 与 T26/T27 排期一致；`IMPLEMENTED_COMPARATOR_IDS` 只含已实现项 | — |
| §5 第 3 项 | 期望带单位而走数值比较器时返回「无法判定」（规范明确不判），带单位比较器处理换算；答案带单位而期望无单位同样无法判定 | 与规范一致 | — |
| §5 第 8 项解集 | 仅一元 ≤2 次且有理根；无理根/高次/多变量 → 无法判定（不是「不相等」） | 与规范「超出即无法判定」一致 | — |
| §5 第 9 项区间 | 二次不等式（并集）与端点含变量 → 无法判定；不做区间并集运算 | T26 扩展（符号集合/区间） | — |
| §5 链顺序 | 默认链按题型筛选（选项/判断只走对应比较器；计算题全链），顺序严格遵循 §5 优先级；答案键声明顺序优先 | 实现解释；顺序变化需升 `prompt_version`/记录在引擎指纹（T17 接入） | — |
| §9 静态校验 | 只实现 T14 可判定子集（分值/选项/数值/维度/容差 >2% 警告/证明题无规则警告）；完整 11 条属 T15 `rubric` | 与排期一致 | — |
| §2 能力声明 | `supports_step_scoring=false`、比较器列表只含 10 个已实现项；`judge_profile_id`/`prompt_version` 暂用 `math-judge-v1` 占位（T17 实现提示词，改版本属 Ask first） | 诚实声明（禁止虚报）；T15/T17 更新 | — |
| §11 大位数上限 | 规范「建议 256 位」；实现按 256 位（分子/分母各自）上限，超限快速失败 | 与建议一致 | — |
| §13 Ask first | 未引入 CAS、未改默认容差策略、未改提示词版本 | 遵守 | — |

## 8. 复现命令

在仓库根执行（新 shell 先 `source "$HOME/.cargo/env"`）：

- 全量测试：`cargo test -p eg-subject-math`
- 静态检查：`cargo clippy -p eg-subject-math --all-targets -- -D warnings`
- 格式化：`cargo fmt -p eg-subject-math -- --check`
- 覆盖率：`cargo llvm-cov -p eg-subject-math --summary-only`
- 依赖闭包（核对零 IO/DB/HTTP）：`cargo tree -p eg-subject-math --edges normal --prefix none`
- 契约套件（下游复用示例）：`cargo test -p eg-subject-math --test contract`

---

# 增量记录：T26 全符号比较器 + T27 CAS 适配（2026-11-02）

> 本段记录 T26/T27 的检索结论、实现决策、夹具、限制与偏差；前文 §1–§8 为 T13–T14 记录。
> 参考：`docs/dev/research/cas.md`（tech-spike 实测 sympy 1.14.0 + Python 3.12.15、启动 0.24 s、单次 0.4–4.5 ms）。

## 9. 问题清单（T26–T27）

| # | 需要确认的外部行为/API/版本/限制 | 影响 |
|---|---|---|
| Q11 | 三角恒等本地能覆盖到什么程度？何时必须转 CAS？ | 决定本地规则与降级边界 |
| Q12 | 向量/矩阵比较的「维度不同」语义（不相等还是无法判定）？符号分量如何处理？转置/行列式是否接受？ | 决定判定结论与风险 |
| Q13 | 集合/区间比较如何支持并集/交集/无穷端点且可安全判「不相等」？ | 决定 AST 与规范化形式 |
| Q14 | CAS sidecar 形态：常驻进程还是每请求进程？超时/崩溃如何强杀与重启？ | 决定 `cas.rs` 结构 |
| Q15 | 缓存键与失效：规范要求「规范化表达式 + CAS 版本」；版本如何进入引擎指纹？ | 决定缓存与 `EngineStamp` |
| Q16 | CAS 安全边界：输入白名单、长度上限、不发送个人信息 | 决定 sidecar 脚本校验 |
| Q17 | 测试如何离线注入超时/崩溃/非法响应？真实 sympy 测试如何跳过？ | 决定 stub 与测试策略 |

## 10. 来源（增量）

| # | URL / 文件 | 版本/日期 | 类型 | 取用内容 |
|---|---|---|---|---|
| S16 | `docs/dev/research/cas.md` | 2026-11-02 | 项目研究记录 | sympy 1.14.0 / Python 3.12.15 / uv 0.13.0 实测；协议冒烟；单次 0.43–4.5 ms；降级要求 |
| S17 | https://docs.sympy.org/latest/modules/simplify/simplify.html | sympy 1.14 文档（2026-11-02 查阅） | 官方文档 | `simplify`、`sstr`、`free_symbols` 语义（差式化简与常量判定） |
| S18 | https://docs.sympy.org/latest/modules/parsing.html | sympy 1.14 文档（2026-11-02 查阅） | 官方文档 | `parse_expr` + `standard_transformations`/`implicit_multiplication_application`/`convert_xor`（`^` 需 convert_xor） |
| S19 | https://docs.astral.sh/uv/ | uv 官方文档（2026-11-02 查阅） | 官方文档 | `uv python install 3.12`、`uv sync` 部署方式（scripts/cas） |
| S20 | `SPEC-subject-math.md` §5 第 7/11/12/13 项、§7 CAS 适配、§10 预算、§11 失败模式、§12 测试策略 | v0.2.1 | 内部规范 | 比较器规则/不判条件、CAS 超时与降级、缓存键、测试类别 |
| S21 | `CONFIG.md` §8 `cas.*` | v0.2.1 | 内部规范 | `cas.enabled/timeout_s/python_path/cache` 默认与范围 |
| S22 | https://doc.rust-lang.org/std/process/struct.Command.html · https://doc.rust-lang.org/std/sync/mpsc/ | Rust std（2026-11-02 查阅） | 官方文档 | 子进程 stdout 读取线程 + `recv_timeout` + `kill/wait` 实现强超时 |

## 11. 结论（T26–T27）

| 问题 | 结论 | 依据 |
|---|---|---|
| Q11 | **本地规则优先，CAS 补足**。本地用「sin/cos 原子 + 约化 sin²→1−cos²」的多项式规范形，覆盖：平方和、和/差角、倍角、`π/2` 整数倍周期与象限、奇偶性、特殊角 0；`tan/cot/sec/csc` 先改写为 sin/cos。本地无法证明（变量分母、反三角、高倍角、复杂恒等）→ CAS；CAS 也不可用/超时 → 「无法判定」。本地只能证明相等；不相等仅在「两边常量精确不等」或「CAS 差式为常量非 0」时给出（Never 不猜）。 | S16、S20 §5/§12；`src/trig.rs` |
| Q12 | **维度不同 → 无法判定**（规范 §5 第 11/12 项明确「维度不同不判」，不是答错）。同维度逐分量/逐元素用统一 `expressions_equivalent`（规范形→多项式恒等→精确常量→CAS）。符号分量支持：恒等意义下不同（`x` vs `y`）→ 不相等；无法证明（含未支持函数/需变形）→ 无法判定。**转置不默认接受**：仅作诊断写入原因；元素确定不同时仍判不相等并提示转置。**行列式只作诊断**（精确有理数高斯消元），不用于接受不同矩阵；支持 2/3 阶（>3 阶无法判定）。矩阵运算化简（乘法/求逆）不支持 → 无法判定。 | S20 §4/§5/§15 开放问题 3；`src/vector.rs`、`src/matrix.rs`、`src/equiv.rs` |
| Q13 | AST 新增 `SetUnion`/`SetIntersection` 节点（可嵌套、可含区间），解析 `\cup`/`\cap`/`∪`/`∩`（`∩` 优先级高于 `∪`）。区间比较统一转「区间并集规范形」：排序、去空、相邻可合并则合并（闭合端点逻辑）；交集逐对求交。规范形式唯一 ⇒ 形式不同可安全判「不相等」；端点含变量/二次不等式并集/混合集合 → 无法判定。有限集合（含并/交）按规范化元素集合比较，忽略顺序/去重；符号元素不同 → 无法判定（不假设变量取值）。 | S20 §4/§5 第 9/10 项；`src/setlike.rs`、`src/parse.rs` |
| Q14 | **每请求一次性子进程**（非常驻）：写入一行 JSON、读一行响应、响应后 `kill` 并 `wait` 清理。理由：请求级隔离、无状态残留、超时/崩溃清理最简单；实测启动 0.24 s + 单次 0.4–4.5 ms 远低于 3 s 预算。超时由 Rust 侧 `recv_timeout` 强制，超时/崩溃/非法 JSON/启动失败全部映射 `CasFailure` → 比较器「无法判定」。常驻进程池留作后续吞吐优化。 | S16、S22；`src/cas.rs` |
| Q15 | 缓存键 = `(lhs 规范形, rhs 规范形, sympy 版本)`（协议版本进命令哈希/指纹）；版本变化自动清空旧缓存；`EG_CAS_CACHE=0` 可关。CAS 版本经 `CasClient::engine_stamp()` 生成 `EngineStamp`（role=Judge、backend=`cas_sidecar`、model_id=sympy、model_version=版本、params_hash=协议+版本+命令哈希）供 `grade` 写入证据 engines；`MathSubjectPack::engine_fingerprint()` 汇总提示词版本+比较器集合+CAS 指纹并给 SHA-256。`grade` 未改（属其 crate），merge 点是公开方法。 | S16、S20 §7/§10；`src/cas.rs`、`src/pack.rs` |
| Q16 | 只发送本 crate `Expr::to_cas()` 生成的受限 ASCII（字母/数字/`+-*/^().,`/空格），变量 π→`pi`、根式→`sqrt()/root()`，长度 ≤1000，拒绝换行；sidecar 再做字符集 + 标识符白名单（sin/cos/tan/cot/sec/csc/asin/acos/atan/log/ln/lg/exp/sqrt/root/Abs/pi + 单字母变量），拒绝 `__`；请求不含学生姓名/学号/图片等任何个人信息。 | S20 §7 安全；`scripts/cas/sidecar.py` |
| Q17 | `scripts/cas/stub_sidecar.py`（仅标准库）提供 `ok/zero/nonzero/slow/hang/crash/garbage` 模式 + 计数文件，测试注入超时/崩溃/非法响应/缓存命中；真实 sympy 测试仅在设置 `EG_CAS_SCRIPT`/`EG_CAS_PYTHON` 时执行，否则打印「跳过」并返回（不联网、CI 无需安装 sympy）。 | S16；`tests/cas_fixtures.rs` |

### 11.1 比较器链更新（§5 顺序，T26/T27 后）

默认链（计算/证明/简答/论述/其他）：`numeric_abs → numeric_unit → fraction_exact → expression_canonical → cas_trig_identity → solution_set → interval_equivalence → set_exact → vector_equivalence → matrix_equivalence → dimension_consistency → text_normalized`；填空在此基础上保留集合/区间/向量/矩阵/量纲；纯数值题仍只走数值类。能力声明比较器由 10 个增至 **14 个**（新增 4 个真实实现，无占位虚报）；`UNIMPLEMENTED_COMPARATOR_IDS` 现为空。

### 11.2 夹具（新增，全部离线通过）

| 类别 | 夹具数（判定断言） | 覆盖要点 |
|---|---|---|
| 三角恒等 `tests/trig_fixtures.rs` | 29 | 17 条本地恒等（平方和/和差/倍角/周期/象限/奇偶/特殊角）、常量与不适用、5 条无法判定、CAS 零/非零/超时 |
| 向量 `tests/vector_fixtures.rs` | 20 | 数值/符号/写法等价、顺序敏感、维度不同/超维、未支持分量、CAS 符号分量、模/点积诊断 |
| 矩阵 `tests/matrix_fixtures.rs` | 20 | 逐元素、维度、1×1/2×2/3×3、>3 阶、转置提示、行列式诊断、符号/未支持、CAS |
| 集合/区间 `tests/setlike_fixtures.rs` | 42 | 开闭/无穷/不等式互通、并集合并、交集、空集、单点↔集合、端点变量、二次不等式、有限集合并/交/去重/符号 |
| CAS `tests/cas_fixtures.rs` | 11 个测试 | ping/零/非零、缓存命中（子进程只启动一次）、150 ms 超时强杀、崩溃/非法 JSON、禁用/缺命令、EngineStamp/指纹、比较器 CAS 集成、失败绝不 NotEqual、真实 sympy 可选 |

累计测试函数 **146**（lib 77 + 集成 69），`cargo test -p eg-subject-math` 全绿；clippy `-D warnings`、fmt 通过；行覆盖率 **83.38%**（TOTAL，≥80%）。

### 11.3 版本实测（T26–T27 增量）

| 依赖/工具 | 实测版本 | 说明 |
|---|---|---|
| sympy / Python | sympy **1.14.0** / CPython **3.12.15**（uv 托管） | 来自 `docs/dev/research/cas.md` tech-spike；许可 BSD（白名单） |
| uv | 0.13.0 | 部署用 `uv python install 3.12` + `uv sync`（`scripts/cas/`） |
| serde_json | 1.0.154 | 新增普通依赖（CAS JSON-lines 协议；MIT/Apache-2.0） |
| Rust 进程/线程 | std `Command` + `mpsc::recv_timeout` | 无新增超时 crate（避免额外依赖/许可面） |
| 测试桩 | `python3` + 标准库 | stub 离线运行，不依赖 sympy |

## 12. 风险与限制（T26–T27 增量）

| 风险/限制 | 影响 | 缓解 |
|---|---|---|
| R13 本地三角规则不完整 | 部分恒等（反三角、高倍角、复杂分式）需 CAS，否则无法判定 | 明确写原因；CAS 可用时补足；规范 §15 已允许试用后扩展 CAS 能力（Ask first） |
| R14 每请求启动进程 | CAS 频繁调用时吞吐受限（每次 ~0.24 s 启动） | 当前为单题低频调用；P95 预算 3.2 s 内；后续可换常驻池（需 Ask first 评审） |
| R15 缓存仅进程内 | 重启后缓存丢失；跨进程不共享 | 键已含版本；持久化缓存建议由 platform 负责（可在 T11 接入） |
| R16 CAS 版本未接入 `grade` 证据 | 证据 engines 暂不含 CAS 指纹 | 提供 `MathSubjectPack::cas_engine_stamp()` 公开 merge 点；`grade` 修改属其 crate（需另任务） |
| R17 矩阵「转置」不默认接受 | 若题目要求转置形式，当前返回不相等并提示（不静默接受） | 规范未声明接受转置；提示人工/规则确认 |
| R18 符号向量/矩阵元素判「不相等」 | 若题目上下文给变量约束（x=y），恒等意义下不同可能不等于学生错 | 恒等意义下判不相等符合「答案等价」定义；无法证明时仍无法判定；如需上下文等价需 L2/规则 |
| R19 集合/区间规范形仅支持一维实数区间 | 复数集合、抽象集合运算不支持 | 无法判定并写原因（规范不承诺复数） |
| R20 三角展开节点上限 600/深度 6 | 超大三角式无法判定 | 快速失败带原因（规范 §11 超大表达式） |
| R21 覆盖率单文件略低 | `equiv.rs`/`setlike.rs`/`parse.rs` 79–80% | 总量 83.38% 达标；后续补边界测试 |

## 13. 与规范的偏差（T26–T27 增量）

| 规范位置 | 偏差/解释 | 处理 | 审批状态 |
|---|---|---|---|
| §7 CAS 形态 | 规范只要求独立子进程 + 超时；实现选择「每请求一次性进程」而非常驻（tech-spike 建议常驻池为优化项） | 实现解释（隔离/清理更简单，性能在预算内）；常驻池属后续优化 | — |
| §7 缓存 | 规范「同一表达式结果缓存」；实现为进程内缓存（键含规范形+版本），未做跨进程持久化 | 实现限制；持久化归 platform 缓存层（T11），接口已预留 `cache` 开关 | 待 Lead 裁定持久化归属 |
| §7 引擎指纹 | 规范要求 CAS 版本入 EngineStamp；`grade` 未修改（不在本任务范围） | 提供 `cas_engine_stamp()`/`engine_fingerprint()`；需 `grade` 后续合并 | 待 Lead 协调 |
| §5 第 11/12 项 | 维度不同判「无法判定」（严格按规范），转置不默认接受 | 与规范一致；转置/行列式仅写入原因 | — |
| §4 集合节点 | 新增 `SetUnion`/`SetIntersection` 节点与 `∪`/`∩` 文法（规范 §4 只列「集合/区间」） | 为支持「并集/交集表达」（T26 要求）扩展 AST；已同步覆盖表/限制说明（本记录） | — |
| §5 第 13 项 | 实现量纲一致比较器（只判量纲，数值由第 4 项）；期望无单位 → 无法判定 | 与规范一致 | — |
| §12 测试 | CAS 真实 sympy 测试为可选（环境未配置时跳过并标注）；离线 stub 覆盖超时/崩溃/非法响应 | 符合「不联网即可跑」要求 | — |
