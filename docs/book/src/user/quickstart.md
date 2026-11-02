# 快速开始

> 状态：占位（M0）。应用尚未实现（M9 才有 Slint 外壳），本页暂只列出预期路径，
> 具体步骤在功能可用后按实测补全，避免写出未经运行验证的说明。

## 计划中的最短路径

1. 准备参考答案与评分规则：手写规则或套用全国卷模板，经模型整理后**必须由教师确认**才会生效（`SPEC-rubric.md`）。
2. 按「学生姓名/学号」命名学生作业文件后批量导入（文件名即学生身份，`SPEC-align.md`）。
3. 选择标准/快速模式开始判分；低置信度结果进入复核队列（`SPEC-grade.md`、`SPEC-review.md`）。
4. 复核后导出成绩（`SPEC-report.md`）。

## 开发者方式（当前可用）

- 构建：`cargo build --workspace`
- 诊断 CLI：`cargo run -p egctl -- --help`（M0 仅输出版本）
- 门禁：`bash scripts/ci.sh`

开发环境准备见 `../dev/research/toolchain.md`。
