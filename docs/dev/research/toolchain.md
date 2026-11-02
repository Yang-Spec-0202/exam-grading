# 研究记录：工具链与 workspace 配置（M0）

> 状态：已完成 · 2026-11-02 · 对应规范：`SPEC.md`（§4 技术栈、§5 命令、§6 结构、§10 边界）、`tasks/todo.md` T01/T03
> 结论用于：`rust-toolchain.toml`、`Cargo.toml`（edition/resolver/lints）、`deny.toml`、`docs/book/book.toml`、`.github/workflows/*`。

## 1. 模块与范围

- 主题：Rust 工具链安装与版本固定、workspace 清单语义、依赖许可门禁、mdBook 静态站配置。
- 范围：只覆盖 M0 工程骨架需要的工具与配置语义；具体业务依赖（Slint、rusqlite、OCR/CAS 等）不在本记录内，由 T02 与各模块研究记录负责。
- 明确不做：Slint/数据库/推理库的选型（T02）、模型下载与 serving（模型任务）、GitHub 远端仓库创建与 Pages 实际发布。

## 2. 问题清单

| # | 需要确认的外部行为/API/版本/限制 | 影响 |
|---|---|---|
| Q1 | 本机（macOS arm64）如何安装 stable Rust 与 clippy/rustfmt？实测版本是什么？ | 决定 `rust-toolchain.toml` 与 CI 安装方式 |
| Q2 | edition 2024 与 `resolver = "3"` 的真实关系？虚拟 workspace 是否必须显式写 resolver？ | 决定根 `Cargo.toml` 是否写 `resolver = "3"` |
| Q3 | `rust-toolchain.toml` 的官方字段与语义（channel/profile/components）？ | 决定固定入口文件内容 |
| Q4 | cargo-deny 0.20.x 的 `deny.toml` 结构：`licenses.version` 是否还有效？workspace 私有 crate 如何处理？通配符依赖如何判定？ | 决定 `deny.toml` 能否通过 |
| Q5 | mdBook 0.5（v0.5.4）的 `book.toml` 有哪些合法字段？0.5 与 0.4 的破坏性变更？ | 决定 `docs/book/book.toml` 是否合法 |
| Q6 | 依赖许可白名单来源与 fail-closed 行为？ | 决定 `[licenses].allow` 与例外策略 |

## 3. 来源

| # | URL | 文档版本/日期 | 类型 | 取用内容 |
|---|---|---|---|---|
| S1 | https://doc.rust-lang.org/edition-guide/rust-2024/cargo-resolver.html | Edition Guide（对应 Rust 1.85 起，2026-11 查阅） | 官方文档 | edition 2024 隐含 resolver 3；resolver 是 workspace 级设置；**虚拟 workspace 仍需显式设置 `resolver`** |
| S2 | https://rust-lang.github.io/rustup/overrides.html | rustup book（rustup 1.29.x 文档，2026-11 查阅） | 官方文档 | `rust-toolchain.toml` 的 `[toolchain]` 段必填；`channel`/`profile`/`components`/`targets` 语义；文件可入库 |
| S3 | https://embarkstudios.github.io/cargo-deny/checks/licenses/cfg.html | cargo-deny 0.20.x 文档（2026-11 查阅） | 官方文档 | `[licenses].allow` 为白名单（未列出即拒绝）；`version` 字段已废弃（写未知字段会报错）；`private.ignore` 忽略未发布 workspace crate；SPDX 2.1 标识 |
| S4 | https://github.com/EmbarkStudios/cargo-deny/releases | 0.20.2（2026-11-02 查询 GitHub API `releases/latest`） | 官方 release | 提供 `aarch64-apple-darwin` 预编译产物，免去 `cargo install` 编译时间 |
| S5 | https://rust-lang.github.io/mdBook/format/configuration/general.html | mdBook 文档（对应 0.5.x，2026-11 查阅） | 官方文档 | `[book]`/`[build]`/`[rust]` 合法字段：title/authors/description/src/language/text-direction；build-dir/create-missing/use-default-preprocessors/extra-watch-dirs |
| S6 | https://rust-lang.github.io/mdBook/format/configuration/renderers.html | 同上 | 官方文档 | `[output.html]` 合法字段（site-url、git-repository-url、edit-url-template、additional-css/js 等） |
| S7 | https://raw.githubusercontent.com/rust-lang/mdBook/master/CHANGELOG.md | mdBook 0.5.0/0.5.4（2026-11 查阅） | 官方变更日志 | **0.5 起配置中的未知字段是错误**；新增 definition-lists、admonitions；标题 id 改为小写；推荐 linkcheck2 做链接检查 |
| S8 | https://github.com/rust-lang/mdBook/releases | v0.5.4（2026-11-02 查询） | 官方 release | 提供 `aarch64-apple-darwin` 预编译产物 |
| S9 | https://github.com/dtolnay/rust-toolchain（README） | 2026-11 查阅 | 官方 Action 文档 | CI 安装方式：`dtolnay/rust-toolchain@stable`；`components` 输入；默认按 `@rev` 选择通道 |
| S10 | 本机实测（见 §5） | 2026-11-02 | 命令输出 | 版本、构建/检查/deny/mdbook 结果 |

## 4. 结论

| 问题 | 结论 | 依据 |
|---|---|---|
| Q1 | 用官方 rustup 脚本安装 stable（profile=default，含 clippy/rustfmt）；本机实测 rustc/cargo 1.99.1、rustup 1.29.2、clippy 0.1.99、rustfmt 1.10.0-stable。 | S2 + §5 |
| Q2 | `edition = "2024"` 在**包**里隐含 resolver 3，但本项目是虚拟 workspace（根 `Cargo.toml` 无 `[package]`），因此必须在 `[workspace]` 显式写 `resolver = "3"`。 | S1 |
| Q3 | `rust-toolchain.toml` 采用 `[toolchain] channel = "stable"`、`profile = "default"`、`components = ["rustfmt", "clippy"]`；`targets` 省略（只构建宿主）。 | S2 |
| Q4 | cargo-deny 0.20.2：`licenses.version` 已废弃不可写；`[licenses].private.ignore = true` 处理 `publish = false` 的 workspace crate；`[bans].wildcards = "deny"` 要求依赖声明带版本 —— 仅写 `path` 会被判为通配符，因此内部 crate 统一在 `[workspace.dependencies]` 里同时给 `path` 与 `version`。 | S3 + 实测（见 §6 风险 R2） |
| Q5 | mdBook 0.5.4 配置严格：只允许官方文档列出的字段（未知字段直接报错）。本项目 `[book]` 用 title/authors/description/language/src，`[build]` 用 build-dir/create-missing，`[output.html]` 只用 site-url。 | S5/S6/S7 |
| Q6 | 白名单按 `SPEC.md` §10：MIT、Apache-2.0、BSD-3-Clause、ISC、Unicode-3.0；未列出的许可一律拒绝（fail-closed）。带例外许可（如 `Apache-2.0 WITH LLVM-exception`）视为不同标识，需要时单独加并在此记录理由。 | S3 + `SPEC.md` §10 |

## 5. 版本实测

| 依赖/工具 | 实测版本 | 命令 | 结果摘要 |
|---|---|---|---|
| OS | macOS 27.0.1（build 26A434），aarch64 | `uname -m`、`sw_vers` | arm64；Xcode CLT 位于 `/Library/Developer/CommandLineTools` |
| rustup | 1.29.2 (d8c92c676 2026-10-21) | `rustup --version` | 用官方 `sh.rustup.rs`，参数 `-y --default-toolchain stable --profile default --no-modify-path` |
| rustc | 1.99.1 (58cf8f9c3 2026-10-13) | `rustc --version` | stable-aarch64-apple-darwin |
| cargo | 1.99.1 (5f94df478 2026-08-27) | `cargo --version` | resolver 3 / edition 2024 支持 |
| clippy | 0.1.99 (58cf8f9c35 2026-10-13) | `cargo clippy --version` | 组件已装（profile=default 自带） |
| rustfmt | 1.10.0-stable (58cf8f9c35 2026-10-13) | `cargo fmt --version` | 组件已装 |
| cargo-deny | 0.20.2 | `cargo deny --version` | 官方 `cargo-deny-0.20.2-aarch64-apple-darwin.tar.gz` 解压到 `~/.cargo/bin` |
| mdBook | v0.5.4 | `mdbook --version` | 官方 `mdbook-v0.5.4-aarch64-apple-darwin.tar.gz` 解压到 `~/.cargo/bin` |

> 注：安装时使用了 `--no-modify-path`，未改动用户 shell 配置。新开的 shell 需要先执行 `source "$HOME/.cargo/env"` 才能使用 `cargo`；`rust-toolchain.toml` 会在首次调用时确保 stable 通道与组件就位。

## 6. 风险与限制

| 风险 | 影响 | 缓解/预案 |
|---|---|---|
| R1：`~/.cargo/bin` 未加入 PATH（`--no-modify-path`） | 其他代理/开发者新开 shell 找不到 `cargo` | README 与 `scripts/ci.sh` 文档注明 `source ~/.cargo/env`；CI 由 action 注入 PATH |
| R2：cargo-deny `wildcards = "deny"` 对仅 `path` 的依赖报错 | 首次运行 `cargo deny check` 失败（实测） | 内部依赖统一放 `[workspace.dependencies]`（path + version），crate 内用 `{ workspace = true }` |
| R3：许可白名单目前只覆盖 5 种 SPDX 标识 | 首批外部依赖（Slint、rusqlite、tracing 等）可能引入 `Apache-2.0 WITH LLVM-exception`、`Zlib`、`BSD-2-Clause` 等 | 引入依赖时逐条核实许可与条款，按 `SPEC.md` §10 决策后更新 `deny.toml` 并在本记录登记；不得为了过门禁而放宽到 allow-all |
| R4：mdBook 0.5 对未知配置字段直接报错 | 升级或加装插件时 `book.toml` 可能构建失败 | 只使用官方文档列出的字段；升级时同步本记录 |
| R5：`advisories` 检查依赖 RustSec 数据库网络获取 | 离线环境 `cargo deny check` 的 advisories 段失败 | CI 在线运行；本地离线时用 `cargo deny check bans licenses sources` 排查，advisories 单独在有网环境跑 |
| R6：GitHub Actions 工作流未在真实 runner 上执行 | 不能保证云端 CI 首次即绿 | 本地等价门禁已全绿（§5/§7）；首次推送后按实际报错修正，不改门禁强度 |
| R7：http 抓取文档时 `web_fetch` 工具受 DNS 限制，改用 `curl` | 不影响结论，但来源为 HTML 转文本 | 关键结论均来自官方站点原始页面；已注明查阅日期 |

## 7. 与规范的偏差

| 规范位置 | 偏差 | 处理（改规范/改实现） | 审批状态 |
|---|---|---|---|
| `SPEC.md` §6 项目结构（`docs/book/`） | mdBook 0.5 的 `src` 需包含 SUMMARY 与全部章节；为保持 `docs/user`、`docs/dev`、`docs/adr` 为唯一内容源，`docs/book/src/` 由 `scripts/docs.sh` 生成（不入库，见 `.gitignore`） | 实现细节，不改规范；已在 `docs/book/README.md` 说明 | — |
| `SPEC.md` §5（`egctl --help`） | M0 的 `egctl` 仅输出版本（骨架），`--help` 子命令在后续里程碑实现 | 实现排期，不改规范 | — |
| `SPEC.md` §4（MSRV） | 采用 `rust-version = "1.97"`（stable 1.99 向下 2 个小版本） | 解释性落地，不改规范 | — |

## 8. 复现命令

在仓库根执行（新 shell 先 `source "$HOME/.cargo/env"`）：

- `rustup show`
- `cargo build --workspace`
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace`
- `cargo deny check`
- `bash scripts/docs.sh`（生成 `docs/book/src` 并 `mdbook build docs/book`）
