# CONFIG：配置项清单与默认值

> 状态：草案 v0.2.2 · 2026-11-02 · **待审批**（v0.2.2：全国卷 active/retired 标注、多选部分给分开关）
> 用途：所有「可按需调整」的行为集中登记在这里；实现阶段按本表建立设置项，默认值以本表为准。
> 规则：**不变量不可配置**（证据、门禁、教师确认、审计、离线默认、模式可分辨）；其余尽量可配置；本表不含示例代码。
> 归属模块列表示「谁读取该配置」，不代表存储位置（存储统一在 `platform`）。

## 1. 通用与界面

| 键 | 默认值 | 可选范围 | 说明 | 归属 |
|---|---|---|---|---|
| `general.mode` | 标准模式 | 标准 / 快速 | 默认运行模式；任务运行中禁止切换 | app/grade |
| `general.language` | 中文（zh-CN） | 仅 zh-CN（v1） | 界面语言 | app |
| `general.theme` | 跟随系统 | 跟随系统 / 浅色 / 深色 | 主题 | app |
| `ui.page_size` | 100 | 20–500 | 队列/列表每页条数 | review/report |
| `ui.undo_depth` | 50 | 10–200 | 复核撤销栈深度 | review |
| `ui.start_page` | 首页 | 任一页面 | 启动页 | app |
| `ui.confirm_before_leave_unsaved` | 开 | 开 / 关 | 离开编辑页前确认 | app |

## 2. 导入与采集

| 键 | 默认值 | 可选范围 | 说明 | 归属 |
|---|---|---|---|---|
| `import.remind_naming` | 开 | 开 / 关 | 选择文件时提示「建议按 学生名/学号 命名」 | app/capture |
| `import.batch_file_limit` | 2000 | 1–10000 | 单批文件数上限（超出开始前拒绝） | capture |
| `import.pdf_dpi` | 300 | 150–600 | PDF 拆页分辨率 | capture |
| `import.quality_blur_threshold` | 120 | 60–300 | 低于即拒收（长边 1600px 口径） | capture |
| `import.quality_exposure_min` | 0.12 | 0.05–0.3 | 欠曝下限 | capture |
| `import.quality_exposure_max` | 0.90 | 0.7–0.98 | 过曝上限 | capture |
| `import.quality_skew_max` | 8° | 3–15° | 倾斜上限 | capture |
| `import.quality_min_long_edge` | 1600px | 1200–3000px | 分辨率下限 | capture |
| `import.reject_behavior` | 页级拒收、批次继续 | 页级拒收 / 整批中止 | 拒收页是否拖垮整批 | capture |
| `import.duplicate_notice` | 开 | 开 / 关 | 重复/疑似重复导入提示 | capture |
| `import.max_pdf_pages` | 200 | 1–1000 | 单份 PDF 最大页数 | capture |
| `import.max_file_mb` | 64 | 1–512 | 单文件大小上限 | capture |
| `import.parallelism` | 4 | 1–8 | 预处理并行度 | capture |

## 3. 身份与对齐

| 键 | 默认值 | 可选范围 | 说明 | 归属 |
|---|---|---|---|---|
| `identity.source` | 文件名 | 文件名（v1 唯一启用） | 身份来源；用户已确认「以文件名称为准」 | align |
| `identity.ocr_fallback` | 关 | 开 / 关 | 文件名未命中时是否用学号/姓名 OCR 兜底；默认关，未命中即人工 | align |
| `identity.barcode` | 关 | 开 / 关 | 条码身份（v1 无解码器，保留开关） | align |
| `identity.unmatched_behavior` | 未决 | 未决 / 自动归入「名单外」 | 未命中文件名的处理；归入名单外时**不自动判分**，仅留诊断待补名单 | align |
| `identity.ocr_threshold` | 0.90 | 0.7–0.99 | 仅在 `identity.ocr_fallback` 开启时生效的自动确认下限 | align |
| `identity.ocr_margin` | 0.10 | 0.02–0.3 | 仅在兜底开启时生效的 Top-1/Top-2 分差下限 | align |
| `align.template_region` | 开 | 开 / 关 | 优先使用试卷模板区域 | align |
| `align.region_confidence_floor` | 0.80 | 0.5–0.99 | 区域对齐可用下限 | align |
| `align.max_cross_pages` | 3 | 1–5 | 单题跨页上限 | align |

## 4. 识别（OCR/公式）

| 键 | 默认值 | 可选范围 | 说明 | 归属 |
|---|---|---|---|---|
| `ocr.model` | PaddleOCR-VL-1.6（0.9B Q4） | 注册表内任一 OCR 模型 | 默认识别模型；可换 GLM-OCR / Xiaomi-OCR-0 等 | inference |
| `ocr.emit_latex` | 开 | 开 / 关 | 公式输出 LaTeX（用户已确认可考虑；默认开） | recognize |
| `ocr.segment_recognition` | 按需 | 关闭 / 按需 / 全量 | 段级识别触发策略 | recognize |
| `ocr.page_timeout_s` | 60 | 10–180 | 单页识别超时 | inference |
| `ocr.repair_attempts` | 1 | 0–2 | 结构不合法时调用方发起的机械修复次数 | recognize |
| `ocr.confidence_floor` | 0.75 | 0.5–0.95 | 低于该值的答案进人工 | grade |
| `ocr.cache` | 开 | 开 / 关 | 识别结果缓存 | recognize |

## 5. 判分

| 键 | 默认值 | 可选范围 | 说明 | 归属 |
|---|---|---|---|---|
| `grade.score_granularity` | 0.1 分（100 千分之一分） | 0.01 / 0.1 / 0.5 / 1 分 | 用户已确认可设；默认 0.1 分 | grade |
| `grade.standard.rule_threshold` | 0.85 | 0.5–0.99 | 仅规则自动接受阈值 | grade |
| `grade.standard.rule_then_model_threshold` | 0.90 | 0.5–0.99 | 规则升级模型阈值 | grade |
| `grade.standard.model_threshold` | 0.85 | 0.5–0.99 | 直接模型判定阈值 | grade |
| `grade.standard.rubric_threshold` | 0.80 | 0.5–0.99 | 按评分标准判定阈值 | grade |
| `grade.fast.rule_threshold` | 0.80 | 0.5–0.99 | 快速模式阈值（其余三类同比例下调 0.10） | grade |
| `grade.allow_uncalibrated_auto` | 关 | 开 / 关 | 标准模式是否允许未校准模型自动放行 | grade |
| `grade.conflict_policy` | 升级人工 | 升级人工 / 规则优先 | 规则与模型冲突时的默认策略 | grade |
| `grade.max_repair_attempts` | 1 | 0–2 | 判定输出不合法时的修复次数 | grade |
| `grade.judge_timeout_s` | 20 | 5–60 | 单题判定超时 | inference |
| `grade.model_profile` | 4B 指令模型 + 受限输出 | 注册表内任一判定配置 | L2 判定配置 | inference |

## 6. 快速模式与抽检

| 键 | 默认值 | 可选范围 | 说明 | 归属 |
|---|---|---|---|---|
| `fast.enabled` | 开 | 开 / 关 | 是否允许进入快速模式 | grade/app |
| `fast.sampling_rate` | 0.20 | 0.05–1.00 | 抽检比例（默认 0.20，可调） | grade |
| `fast.low_confidence_weight` | 0.70 | 0–1 | 加权抽样时低置信度题目的权重（用户确认 0.7 为权重） | grade |
| `fast.sampling_seed` | 随机（记录种子） | 任意整数 / 随机 | 抽样随机种子；记录进审计与判分记录以便复现 | grade |
| `fast.sampled_readonly` | 开 | 开 / 关 | 抽检队列只读（只能通过/转标准） | review |
| `fast.fail_action` | 弹窗询问 | 弹窗询问 / 自动整卷重跑 / 自动单题重跑 | 抽检不通过时行为（用户要求弹窗） | review/app |
| `fast.fail_default_scope` | 单题 | 单题 / 整卷 | 弹窗确认后的默认重跑范围 | review |
| `fast.export_label` | 开 | 开 / 关 | 导出文件名与表头标注 `快速` | report |

## 7. 评分规则与模板

| 键 | 默认值 | 可选范围 | 说明 | 归属 |
|---|---|---|---|---|
| `rubric.template_pack` | 全国卷模板包 | 全国卷模板包 / 自定义模板 / 两者并存 | 用户确认：以当前几类全国卷为标准模板，外加自定义 | rubric |
| `rubric.national_sets` | 现行：新课标Ⅰ卷、新课标Ⅱ卷；历史快照：全国甲卷、全国乙卷 | 模板包内可选（按 active/retired 分组） | 全国卷模板集合；**现行卷仅新课标Ⅰ/Ⅱ**（2024 年起 19 题/150 分）；全国乙卷 2024 年起停止命制、全国甲卷 2026 年起退出，两者标 `retired` 只读快照，仅供历史试卷套用与复现；具体分值/结构由模板数据决定 | rubric |
| `rubric.partial_choice_enabled` | 开 | 开 / 关 | 多选题「按选项部分给分」总开关；关闭时含部分给分档位的规则在校验期即错误级拦下（不静默降级为全对）；档位数值来源为模板数据/规则数据（官方口径，如 2 选项选 1 得 3 分、3 选项选 1 得 2 分/选 2 得 4 分、错选 0 分），不在本表写死 | rubric/domain/grade |
| `rubric.require_confirmation` | **不可配置（恒开）** | — | 模型整理结果必须教师确认才生效 | rubric |
| `rubric.min_score_granularity` | 0.1 分 | 0.1 / 0.5 / 1 分 | 评分项最小粒度 | rubric |
| `rubric.auto_validate` | 开 | 开 / 关 | 草稿保存时自动静态校验 | rubric |
| `rubric.allow_custom_template` | 开 | 开 / 关 | 是否允许导入/新建自定义模板 | rubric |
| `rubric.template_snapshot` | 最新公开试卷快照 | 固定版本号 | 全国卷模板包的数据快照；不承诺维护，稳定优先 | rubric |
| `rubric.repair_attempts` | 1 | 0–2 | 规则整理输出不合法时的机械修复次数 | rubric |

## 8. 符号计算（CAS）

| 键 | 默认值 | 可选范围 | 说明 | 归属 |
|---|---|---|---|---|
| `cas.enabled` | 开 | 开 / 关 | 三角恒等/矩阵/集合符号比较 | subject-math |
| `cas.backend` | sympy sidecar | sympy sidecar | 用户确认可接受；项目为求职展示、开放许可 | subject-math |
| `cas.timeout_s` | 3 | 1–10 | 单次化简超时；超时即降级 | subject-math |
| `cas.python_path` | 自动探测 | 绝对路径 | sidecar 解释器路径（需 3.12） | subject-math |
| `cas.cache` | 开 | 开 / 关 | 表达式结果缓存 | subject-math |

## 9. 日志、审计与诊断

| 键 | 默认值 | 可选范围 | 说明 | 归属 |
|---|---|---|---|---|
| `log.level` | info | trace/debug/info/warn/error | 全局默认级别 | platform |
| `log.per_module_level` | 空（继承全局） | 模块 → 级别 | 按模块覆盖（用户确认「是」） | platform |
| `log.retention_days` | 30 | 7–365 | 日志保留 | platform |
| `log.rotation` | 按天 | 按天 / 单文件 50MB | 轮转策略 | platform |
| `log.redaction` | 开 | 开 / 关（关需二次确认） | 姓名/学号/路径脱敏 | platform |
| `log.search_index` | 开 | 开 / 关 | 日志查看器全文索引（用户确认「是」） | platform/app |
| `log.ui_viewer` | 开 | 开 / 关 | 应用内日志查看器 | app |
| `diagnostics.include_schema` | 开 | 开 / 关 | 诊断包含 schema/迁移清单 | platform |
| `audit.enabled` | **不可配置（恒开）** | — | 审计只追加、哈希链 | platform |

## 9.5 存储、推理端点与备份（补充）

| 键 | 默认值 | 可选范围 | 说明 | 归属 |
|---|---|---|---|---|
| `storage.soft_quota_gb` | 20 | 5–500 | 资产占用告警线；只告警不删除 | platform |
| `storage.verify_hash_on_read` | 开 | 开 / 关 | 复核/导出/备份前后校验哈希 | platform |
| `backup.keep_generations` | 5 | 1–20 | 备份保留代数 | platform |
| `log.index_range_days` | 30 | 7–365 | 全文索引覆盖范围（与日志保留一致） | platform |
| `log.index_rebuild` | 启动校验 + 手动 + schema 变更强制重建 | 启动校验 / 手动 / 定时 | 索引重建策略（见 `SPEC-platform.md` 日志节） | platform |
| `inference.local_port` | 自动分配 | 自动 / 固定端口 | 本地 sidecar 端口策略 | inference |
| `inference.registry_path` | 数据目录内 `models/registry.toml` | 绝对路径 | 模型注册表位置 | inference |
| `inference.openai_base_url` | 空 | URL | 云端 OpenAI 兼容端点；空则云端不可用 | inference |
| `inference.model_user_selectable` | 开 | 开 / 关 | 注册表中标记为可选的模型才出现在设置页 | inference |
| `ui.window_geometry` | 自动记忆 | 自动 / 固定 | 窗口位置与尺寸记忆 | app |

## 10. 隐私、云端与授权

| 键 | 默认值 | 可选范围 | 说明 | 归属 |
|---|---|---|---|---|
| `privacy.cloud_enabled` | 关 | 开 / 关 | 云端总开关；开启仍需逐任务授权 | inference |
| `privacy.per_task_consent` | **不可配置（恒开）** | — | 每次任务鉴权 | inference/platform |
| `privacy.consent_ttl` | 任务生命周期 | 任务生命周期 | 任务终态即失效 | platform |
| `privacy.send_original_image` | 关 | 开 / 关 | 云端是否可发原图（默认只发裁剪/脱敏） | inference |
| `privacy.export_mask` | 关 | 开 / 关 | 导出默认脱敏 | report |

## 11. 文档与开发

| 键 | 默认值 | 可选范围 | 说明 | 归属 |
|---|---|---|---|---|
| `docs.site` | 开（mdBook） | 开 / 关 | 用户确认需要静态站点 | docs |
| `docs.link_check` | 开 | 开 / 关 | 文档链接检查门禁 | docs/CI |
| `docs.publish` | GitHub Pages | GitHub Pages / 仅本地 | mdBook 构建产物发布目标（用户确认需要） | docs |
| `dev.research_first` | **不可配置（恒开）** | — | 实现前必须检索官方文档/版本/接口，见 `AGENTS.md` | 全员 |
| `dev.record_sources` | 开 | 开 / 关 | 研究记录写入 `docs/dev/research/` | 全员 |

## 12. 不可配置项（不变量）

以下行为**不提供设置开关**，修改需改规范并走审批：

1. 判分必须有证据；模型输出必须先校验。
2. 低置信度不得绕过门禁进入最终成绩。
3. 规则草稿必须教师确认才生效；模型不得生成生效规则。
4. 审计恒开、只追加、哈希链；写失败即拒绝业务写。
5. 云端默认关闭；每次任务鉴权；授权随任务结束失效。
6. 原图永久保留、无自动删除路径。
7. 标准/快速模式必须可分辨（记录、导出、统计）。
8. 规范文件不得包含示例代码。
9. 实现前必须检索（`AGENTS.md`），不得只凭模型记忆写代码。
