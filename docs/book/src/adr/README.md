# 架构决策记录（ADR）

> 状态：M10 初稿（2026-11-02）。ADR 记录「为什么这样设计」；「必须/禁止什么」写在规范里，两者不互相复制。

## 约定

- 文件名：`NNNN-<kebab-case-标题>.md`，四位编号、递增、不复用。
- 状态取值：提议 / 已接受 / 已废弃 / 被取代（注明取代者编号）。
- 结构（0002 起）：背景 → 决策 → 后果 → 替代方案 → 证据链接；0001 使用早期四段结构。
- 与规范变更同一次提交；新增 ADR 需在下方索引登记，并同步 `docs/book/SUMMARY.md`。
- 证据链接必须指向仓库内可解析的路径（规范章节、研究记录或脚本）；文档门禁会检查死链。

## 索引

| 编号 | 标题 | 状态 |
|---|---|---|
| 0001 | [采用 ADR 记录架构决策](0001-record-architecture-decisions.md) | 已接受 |
| 0002 | [两层判分（L1 路由 + L2 判定）](0002-two-layer-grading.md) | 已接受 |
| 0003 | [标准/快速双模式与加权抽检](0003-dual-mode-and-sampling.md) | 已接受 |
| 0004 | [评分规则结构化：模型只产草稿、教师确认后生效](0004-rubric-draft-then-confirm.md) | 已接受 |
| 0005 | [文件名即学生身份](0005-filename-as-student-identity.md) | 已接受 |
| 0006 | [任务级云端授权（默认离线）](0006-task-scoped-cloud-consent.md) | 已接受 |
| 0007 | [CAS 扩展（sympy sidecar）与不可用即降级](0007-cas-adapter-with-degradation.md) | 已接受 |
| 0008 | [规范文件不含示例代码](0008-no-example-code-in-specs.md) | 已接受 |
| 0009 | [项目采用 GPL-3.0-only 与 Slint 的 GPL 分支](0009-gpl-only-and-slint-license.md) | 已接受 |

模板见 `_template.md`。

## 待办

- `SPEC.md` §9 要求覆盖的关键决策已全部成文（两层判分、模式、规则结构化、CAS 选型、文件名身份、按任务鉴权、无代码规范）；GPL/Slint 许可单列为 0009。
- 后续实现阶段如有重大偏离（例如更换 GUI 框架、改变授权模型），新增 ADR 并标注取代关系，不修改已接受的旧 ADR。
