# 研究记录：report（成绩汇总、统计与导出）

> 状态：实现记录 · 2026-11-03 · 对应规范：`SPEC-report.md`（草案 v0.2.1）· 任务：`tasks/todo.md` T33–T34
> 规则：本页必须写满下列表格；无来源的结论视为未完成。

## 1. 模块与范围

- 模块 id：`report`（crate `eg-report`）。
- 本次要实现的能力：
  - T33 统计：有效样本（最新修订、模式匹配、状态有效、提交未排除、名单内）、单题得分率/区分度、
    整卷折算分/得分率/排名、班级均分/标准差/及格率/分布、双模式分列统计、抽检说明。
  - T34 导出：XLSX（`rust_xlsxwriter`）、CSV（BOM + 追加整数列）、HTML（单文件内联样式）、
    命名规则（`试卷名_模式_时间戳`、`_脱敏`、冲突序号、超长哈希）、原子写、导出审计、重放对比、
    脱敏与导出前自检、1000 份提交性能预算。
- 明确不做：不判分/不改分/不写提交状态；不生成成绩单 PDF；不做跨班/跨学期汇总；不联网；
  不做图片图表；不实现数据库读写（经 `ReportSource` 端口由 `app` 注入）。

## 2. 问题清单

| # | 需要确认的外部行为/API/版本/限制 | 影响 |
|---|---|---|
| Q1 | `rust_xlsxwriter` 0.102 的工作表 API、数字格式、`save_to_buffer` 是否可用？ | XLSX 能否内存渲染后原子写 |
| Q2 | 该版本的依赖（`zip`）是否与 workspace 已锁定版本兼容（离线构建）？ | 是否需要新增网络下载 |
| Q3 | XLSX 字节是否确定（重放能否按文件哈希比较）？ | 重放一致性口径 |
| Q4 | `chrono 0.4.45` 在 `default-features=false + std` 下能否格式化时间戳？ | 命名时间戳 |
| Q5 | `eg-domain` 的 `GradeRecord::validate` 对夹具记录有哪些硬性要求？ | 测试夹具合法性 |
| Q6 | `MilliScore::from_milli` 是否可失败？ | 构建分数的方式 |
| Q7 | `GradeRecord` 是否实现 `Eq`？ | 快照类型的派生 |
| Q8 | `GradeMode` 是否实现 `Ord`？ | 模式集合的数据结构 |
| Q9 | `SubmissionExclusion`/`GradeStatus` 的语义与标签映射？ | 样本分类与状态列 |
| Q10 | 规范 §3.2 的时间戳「工作区时区」如何在无 UI 上下文中取得？ | 命名与元信息 |
| Q11 | 规范要求「850 分」等阈值/抽检率来自 `CONFIG.md`，但当前无 `report.*` 键 | 口径配置来源 |
| Q12 | 1000 份 × 20 题的三格式导出实际耗时？ | 是否满足 §3.4 性能预算 |

## 3. 来源

| # | URL | 文档版本/日期 | 类型 | 取用内容 |
|---|---|---|---|---|
| S1 | https://docs.rs/rust_xlsxwriter/0.102.0/rust_xlsxwriter/ | 0.102.0（本地 registry 同版源码核对） | 官方文档 | `Workbook::add_worksheet`、`write_string_with_format`、`write_number_with_format`、`set_column_width`、`set_freeze_panes`、`autofilter`、`Format::set_num_format`、`save_to_buffer` |
| S2 | `~/.cargo/registry/src/*/rust_xlsxwriter-0.102.0/src/workbook.rs`、`worksheet.rs`、`format.rs`、`properties.rs` | 0.102.0 | 源码 | 方法签名与返回 `Result<&mut Worksheet, XlsxError>`；`DocProperties::set_title/set_author/set_company` |
| S3 | https://docs.rs/chrono/0.4.45/chrono/ | 0.4.45 | 官方文档 | `DateTime<Utc>::format` 在 `std` 特性下可用；`TimeZone::with_ymd_and_hms`（测试构造） |
| S4 | https://docs.rs/zip/9.2.2/zip/ | 9.2.2（`Cargo.lock` 既有锁定） | 官方文档 | `rust_xlsxwriter` 唯一非可选依赖为 `zip 9.0`，9.2.2 满足 |
| S5 | `SPEC-report.md` v0.2.1 §3.1–§3.5、§4–§7、§9 | 2026-11-02 | 项目规范 | 统计口径、工作簿列序、命名规则、原子性、审计、性能预算、成功标准 |
| S6 | `SPEC-grade.md`、`SPEC-domain.md`、`crates/eg-domain/src/{grade,score,paper,evidence}.rs` | v0.2.x | 项目规范/源码 | `GradeRecord` 字段与不变量、`GradeMode/GradeStatus/SubmissionExclusion`、`MilliScore`、`Paper/Question/Student` |
| S7 | `CONFIG.md` §6/§10 | v0.2 | 项目规范 | `fast.sampling_rate=0.20`、`fast.low_confidence_weight=0.70`、`fast.export_label=开`、`privacy.export_mask=关` |
| S8 | `CAPABILITY-MAP.md` §2/§3 | v0.2 | 项目规范 | 依赖方向 `report → domain`，app 为组合根；端口由 app 实现 |
| S9 | `crates/eg-platform/src/{repo,audit,config}.rs` | 当前分支 | 源码 | `AuditLog::append`/`NewAuditEntry`、`repo::list_grade_records`、配置字段（只读参考，未直接依赖） |

## 4. 结论

| 问题 | 结论 | 依据 |
|---|---|---|
| Q1 | 该版本 API 与规范所需能力一一对应；`save_to_buffer()` 返回 `Vec<u8>`，满足「内存渲染 → 原子写」。所有写单元格方法返回 `Result`，需逐处映射错误。 | S1、S2 |
| Q2 | `rust_xlsxwriter 0.102.0` 依赖 `zip 9.0`，workspace 已锁 `zip 9.2.2`，`--offline` 可解析；新增依赖只改 `Cargo.lock` 的机械条目。 | S4、§5 实测 |
| Q3 | 不可靠：工作簿容器写入生成时间等元数据，两次导出的 XLSX 字节可不同。重放摘要改为对「可见内容（8 张表 canonical 文本 + 文件名集合）」取 SHA-256；逐文件哈希仍记入导出运行与审计。 | 实测（两次导出文件哈希不同）+ S1 |
| Q4 | 可用；命名时间戳格式 `yyyyMMdd-HHmmss`，由调用方传入 `eg_domain::Timestamp`，本 crate 不读系统时钟。 | S3、S5 §4 |
| Q5 | 必须满足：`revision>0`、`max_score>0`、`awarded<=max`、判定得分与 `awarded` 一致、证据非空、`route_trace.rule_id` 非空、标准模式禁止 `sampled=true`、`ManuallyReviewed` 必须有 `reviewed_at`。夹具按此构造。 | S6 |
| Q6 | 会失败（返回 `Result<MilliScore, ScoreError>`），夹具与实现统一用 `from_milli(...)` 后处理错误，不使用 `unwrap`。 | S6、编译验证 |
| Q7 | 否：`confidence` 为 `f64`，领域类型只派生 `PartialEq`；本 crate 的 `ReportInput`/`Report` 相应只派生 `PartialEq`。 | S6、编译验证 |
| Q8 | 否：`GradeMode` 无 `Ord`；模式集合改用 `Vec<GradeMode>` 按出现顺序去重。 | 编译验证 |
| Q9 | `NeedsReview` 不进样本；`Excluded` 题级记录不计分子分母；`SubmissionExclusion` 五类原因移出整卷样本并按原因计数（元信息/汇总列展示「排除」）。 | S5 §3.4、S6 |
| Q10 | 无 UI/时区上下文，统一按调用方提供的 `Timestamp` 以 UTC 格式化；`app` 负责把工作区时区换算后的时间传入（偏差见 §7-2）。 | S5 §3.2、S8 |
| Q11 | 口径默认值在 `ReportSettings::default()` 定义（与 `CONFIG.md` 数值一致），由 `app` 把 `fast.*`/`privacy.export_mask` 映射进来；待 `CONFIG.md` 增补 `report.*` 键后再改为读配置。 | S7、§7-3 |
| Q12 | release 构建实测：统计构建 126ms、单 XLSX 33ms、三格式合计 80ms（10 个文件、1.26MB、1063 行），远低于 8s/15s 预算。 | §5 实测 |

## 5. 版本实测

| 依赖/工具 | 实测版本 | 命令 | 结果摘要 |
|---|---|---|---|
| cargo / rustc | 1.99.1 / 1.99.1 | `cargo --version`、`rustc --version` | MSRV 1.97 满足 |
| eg-domain | 0.1.0（path） | `cargo tree -p eg-report --offline` | 版本与 workspace 一致 |
| rust_xlsxwriter | 0.102.0 | `cargo tree`、`Cargo.lock` | 离线解析成功；`save_to_buffer` 可用 |
| zip | 9.2.2 | `Cargo.lock` | 已有锁定，无需联网新增 |
| chrono | 0.4.45（`std`，无默认特性） | `cargo tree` | 格式化可用 |
| 单元测试 | 28 项 | `cargo test -p eg-report --offline --lib` | 全绿 |
| 统计集成测试 | 9 项 | `cargo test -p eg-report --offline --test stats` | 全绿（模式隔离、样本分类、排名、选项/标签/分布、重放、空班级、非法快照/配置） |
| 导出集成测试 | 6 项 | `cargo test -p eg-report --offline --test export` | 全绿（三格式、10 次重放、脱敏、原子/覆盖、审计回滚、草稿、快速标注关闭） |
| 静态检查 | — | `cargo clippy -p eg-report --all-targets --offline -- -D warnings` | 0 warning |
| 性能（release） | — | `cargo test -p eg-report --release --offline --test perf -- --ignored --nocapture` | 构建 126ms；XLSX 33ms/116KB；三格式 80ms/10 文件/1.26MB/1063 行 |

## 6. 风险与限制

| 风险 | 影响 | 缓解/预案 |
|---|---|---|
| XLSX 字节非确定 | 不能用文件哈希做重放对比 | 内容摘要按可见内容计算；逐文件哈希仍审计留痕 |
| 选项分布依赖 `align` 的作答投影 | 投影缺失时「选项分布」为空表 | 不伪造数据；元信息保留快照哈希，待 `align` 接入后由 `app` 提供 `ChoiceRecord` |
| 打印尺寸/列宽仅估算 | HTML/XLSX 观感差异 | 教师可在 Excel 调整；HTML 已用 `@media print` |
| 姓名子串误伤脱敏自检 | 试卷名含学生姓名片段时可能中止导出 | 规范要求「导出前自检」，中止优于泄露；提示改用中性试卷名 |
| 名单外/重复提交仅单列，不参与统计 | 与「一人一份」前提不一致时计数可见 | 重复提交行标「重复提交」并并入排除数；后续由 `align` 去重更合适 |
| 时间戳 UTC | 与「工作区时区」表述不一致 | `app` 传入本地化时间；规范待审批修订措辞 |
| 无 `report.*` CONFIG 键 | 阈值/抽检率来源在代码 | 默认值与 CONFIG 一致；`app` 适配层可覆盖；规范/配置待增补 |

## 7. 与规范的偏差

| 规范位置 | 偏差 | 处理（改规范/改实现） | 审批状态 |
|---|---|---|---|
| §1 依赖 `domain`、`grade` | 暂不依赖 `eg-grade`：并行开发中该 crate 未通过编译（`pipeline.rs` `error[E0425]`），为不阻塞本模块，判分记录经 `eg-domain::GradeRecord` 夹具驱动 | 待 `eg-grade` 通过编译后改为使用其公开查询类型；`Cargo.toml` 保留 TODO | 待审批 |
| §3.2 时间戳「工作区时区」 | 本 crate 只接受调用方 `Timestamp` 并按 UTC 格式化 | 由 `app` 传入工作区时区换算后的时间；或规范改为「调用方本地化时间」 | 待审批 |
| §1 配置项 `report.*` | `CONFIG.md` 尚无 `report.*` 键，口径默认值在 `ReportSettings::default()` | `app` 映射 `fast.*`/`privacy.export_mask`；建议 `CONFIG.md` 增补 `report.pass_rate`/`good_rate`/`excellent_rate`/`policy_version` | 待审批 |
| §3.2/§9.2 单学生成绩单命名 | 实现了 `ExportNaming::student_file_name` 与测试，但 v1 导出只产「整卷工作簿 + 成绩单工作表」，不逐生生成文件 | 若需要逐生文件，`app` 可按学生分组调用命名函数；规范开放问题 2 待裁定 | 待审批 |
| §3.2 快速标注「文件名与表头标快速」 | `fast.export_label=关` 时文件名省略「快速」词；工作表固定列名/表名不加入「快速」字样（列序属破坏性修改），模式仍由行内模式列、元信息与「快速抽检」表标注 | 若确需表头标记，建议在工作表顶部增加标题行（需规范批准列布局变更） | 待审批 |
| §3.4 内容摘要/重放「内容摘要相等」 | 摘要取可见内容哈希而非 XLSX 文件字节（原因见 Q3） | 规范措辞改为「可见内容摘要」或由本记录背书 | 待审批 |
| §3.4 重复提交 | 同一学生多份提交时取最新一份参与统计，其余单列为「重复提交」并计入排除数 | 规范未定义；建议由 `align` 去重，报告仅展示 | 待审批 |
| §3.5「名单版本」 | 元信息以成绩单行数（名单人数）展示，未引入独立版本号 | 待 `platform` 提供名单版本字段后替换 | 待审批 |
| §6 审计「导出运行 id」 | 由内容摘要前缀生成 `export-<12hex>`，未落库；落库由 `app` 的 `AuditLog` 适配完成 | `app` 接入时改为平台 id 生成器 | 待审批 |
