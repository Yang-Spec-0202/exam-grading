# 研究记录（实现前置条件）

> 依据：`AGENTS.md` §2/§4 与 `CONFIG.md` 的 `dev.research_first`（恒开，不可配置）。
> 每个模块在写实现代码之前，必须先有对应研究记录，否则不具备准入资格。

## 约定

- 文件名：`<module-id>.md`（`domain`、`platform`、`capture`、`inference`、`recognize`、`align`、`rubric`、`grade`、`subject-math`、`review`、`report`、`app`），跨模块主题可用独立名字（如 `toolchain.md`）。
- 模板：`_template.md`；表格字段不得删减。
- 来源必须可追溯：URL + 文档版本/日期 + 类型（官方文档/源码/变更日志/issue/模型卡）。
- 版本以本机实测为准（`cargo add` / `--version` 输出摘要），不得写「应该存在」的 API。
- 检索结论与规范冲突时：先提规范变更（审批），再改实现；偏差写入「与规范的偏差」。
- 提交说明需链接对应研究记录（`AGENTS.md` §5）。

## 已有记录

| 记录 | 范围 | 状态 |
|---|---|---|
| `toolchain.md` | Rust/rustup/cargo-deny/mdBook 版本与 workspace 配置依据 | 已完成（M0） |
| `templates.md` | 全国卷模板数据调研 | 由模板任务维护 |
| `models.md` | 模型候选、许可、serving 实测 | 由模型任务维护 |
