# 架构总览

> 状态：M0 骨架。本页只写结构性约束，具体契约以各模块规范为准（不复制规范条文）。

## 1. 分层与依赖方向

- 依赖**单向、禁止环**：`domain` ← `platform` ← 功能模块 ← `app`（组合根）。
- 跨模块只经 `domain` 的公开类型与各模块根导出的 trait 通信；不得读取彼此的表或内部结构。
- `app` 是唯一组合根；任何模块不得反向依赖 `app`。
- crate 与模块 id 一一对应（`eg-domain` ↔ `domain`，以此类推），依赖关系已在 `crates/*/Cargo.toml` 中声明，构建期即可发现成环。

## 2. 进程与线程

- 单机单用户，无服务端；单进程 + 后台工作线程。
- UI 线程零阻塞；并发用标准线程 + 有界通道 + CPU 线程池，不引入 async 运行时。
- SQLite 单写连接 + WAL；资产库内容寻址、原图只读。

## 3. 数据流（概念）

导入（`capture`）→ 识别（`recognize`，经 `inference` 调模型）→ 对齐（`align`）→ 判分（`grade`，依赖 `rubric`/`subject-math`）→ 复核（`review`）→ 导出（`report`）；每一步落证据、可重放。

## 4. 不可协商的横向不变量

见 `CAPABILITY-MAP.md` §5（原图不可变、判分可复现、证据先行、门禁优先、默认离线、文件名即身份、模式可分辨、类型归属）。这些属于规范，文档不重复定义。

## 5. 构建与质量门禁

- workspace：edition 2024、`resolver = "3"`（Rust-version aware，见官方 Edition Guide 的 cargo-resolver 一节）、`[workspace.lints]` 统一严格模式。
- 门禁命令见 `SPEC.md` §5 与 `scripts/ci.sh`；依赖许可白名单见根目录 `deny.toml`。
