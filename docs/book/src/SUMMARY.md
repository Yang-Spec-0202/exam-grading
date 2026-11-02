# Summary

[首页](index.md)

# 用户手册

- [用户手册总览](user/README.md)
- [快速开始](user/quick-start.md)
- [导入与命名](user/import-and-naming.md)
- [标准与快速模式](user/modes.md)
- [复核工作台](user/review.md)
- [评分规则](user/rubric.md)
- [成绩导出](user/export.md)
- [常见故障](user/troubleshooting.md)
- [隐私与数据](user/privacy.md)

# 开发者文档

- [开发者文档总览](dev/README.md)
- [架构总览](dev/architecture.md)
- [验收记录（M10）](dev/acceptance.md)
- [性能基线（M10）](dev/performance.md)

# 研究记录（检索优先）

- [研究记录约定与索引](dev/research/README.md)
- [工具链与 workspace 配置（M0）](dev/research/toolchain.md)
- [全国卷数学模板数据（M5 数据）](dev/research/templates.md)
- [本地推理模型获取、校验与 serving（M0/M4）](dev/research/models.md)
- [Slint（GUI 外壳）](dev/research/slint.md)
- [存储与基础依赖（platform / domain / report）](dev/research/deps.md)
- [图像 / PDF / DOCX（capture 输入管线）](dev/research/pdf.md)
- [本地推理 serving（OCR + 判定，M4 / 16 GB）](dev/research/inference-serving.md)
- [CAS sidecar（sympy + uv / Python 3.12）](dev/research/cas.md)
- [domain（领域类型、值对象与学科契约，M1）](dev/research/domain.md)
- [platform（配置/存储/任务/审计/备份，M1）](dev/research/platform.md)
- [subject-math（归一化/语法树/比较器，M2/M5）](dev/research/subject-math.md)
- [inference（后端/注册表/授权/严格 Schema，M4）](dev/research/inference.md)
- [capture（导入/拆页/预处理/落库，M3）](dev/research/capture.md)
- [recognize（识别/LaTeX/缓存/置信度，M3）](dev/research/recognize.md)
- [align（身份匹配与区域对齐，M6）](dev/research/align.md)
- [report（成绩汇总/统计/导出，M7）](dev/research/report.md)
- [review（复核队列/命令/撤销/抽检，M7）](dev/research/review.md)
- [rubric（草稿/模板/规则整理/确认，M5）](dev/research/rubric.md)
- [grade（路由/判定/双模式/抽检，M2）](dev/research/grade.md)
- [app（Slint 外壳与页面，M9）](dev/research/app.md)
- [egctl（运维与验收 CLI，M10）](dev/research/egctl.md)

# 架构决策

- [ADR 索引](adr/README.md)
- [ADR-0001 采用 ADR 记录架构决策](adr/0001-record-architecture-decisions.md)
- [ADR-0002 两层判分（L1 路由 + L2 判定）](adr/0002-two-layer-grading.md)
- [ADR-0003 标准/快速双模式与加权抽检](adr/0003-dual-mode-and-sampling.md)
- [ADR-0004 评分规则结构化：草稿 → 确认](adr/0004-rubric-draft-then-confirm.md)
- [ADR-0005 文件名即学生身份](adr/0005-filename-as-student-identity.md)
- [ADR-0006 任务级云端授权](adr/0006-task-scoped-cloud-consent.md)
- [ADR-0007 CAS 扩展（sympy sidecar）与降级](adr/0007-cas-adapter-with-degradation.md)
- [ADR-0008 规范文件不含示例代码](adr/0008-no-example-code-in-specs.md)
- [ADR-0009 GPL-3.0-only 与 Slint 许可分支](adr/0009-gpl-only-and-slint-license.md)

# 变更

- [变更记录](changelog.md)
