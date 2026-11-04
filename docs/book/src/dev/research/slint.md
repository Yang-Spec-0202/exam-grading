# 研究记录：Slint（GUI 外壳）

> 状态：已完成 · 2026-11-02（Asia/Shanghai）· 对应规范：`SPEC.md` §4（技术栈）、`SPEC-app.md`（外壳与线程模型）、`CONFIG.md`、`AGENTS.md` §2/§4
> 实测机器：Apple M4 / 16 GB / macOS 27.0.1（26A434）/ rustc 1.99.1 / Xcode CLT
> 结论用于：`eg-app` 依赖与版本锁定、`ui/` 目录结构、线程与模型契约、`deny.toml` 许可审批

## 1. 模块与范围

- 模块 id：`app`（GUI 外壳；`SPEC-app.md`）
- 本次要实现的能力：窗口与首帧、中文文本渲染、`Image` 图片显示、列表/模型绑定、后台线程 → UI 线程的跨线程更新（`invoke_from_event_loop`）、定时器、回调、按钮/输入框控件。
- 明确不做：业务逻辑、页面导航与编号、快捷键、中文 IME 的人工验收（见 §6 R1）、跨平台构建（仅 macOS arm64 实测）。

## 2. 问题清单

| # | 需要确认的外部行为/API/版本/限制 | 影响 |
|---|---|---|
| Q1 | 当前稳定 1.x 的实际版本是多少？`SPEC.md` 写「1.18.x（不低于 1.14）」是否成立？ | 决定锁定版本 |
| Q2 | macOS（arm64）能否直接构建并运行？需要哪些系统依赖/框架？ | 决定构建与打包代价 |
| Q3 | 窗口、图片、模型（`Model`）、定时器的真实 API 形态？ | 决定 `ui/` 与 Rust 侧契约写法 |
| Q4 | 后台线程更新 UI 的官方机制（`invoke_from_event_loop`）签名与线程语义？ | 决定 `eg-app` 的线程模型（禁止 UI 线程阻塞） |
| Q5 | Slint 是否真的实现了 macOS 中文 IME（组字/候选窗）？已知缺陷有哪些？ | 决定中文输入是否可用与风险预案 |
| Q6 | 许可（Slint 三重许可）与 `deny.toml` 白名单是否冲突？ | 决定是否需要负责人审批 + 规范变更 |
| Q7 | 构建耗时与二进制体积（工程可接受度）？ | 决定 CI 时间与包体 |

## 3. 来源

| # | URL | 文档版本/日期 | 类型 | 取用内容 |
|---|---|---|---|---|
| S1 | https://crates.io/api/v1/crates/slint | API 查询 2026-11-02，`max_stable_version = 1.18.1`，`updated_at = 2026-09-21` | 官方 registry | 最新稳定版 |
| S2 | `cargo search slint`（本机） | 2026-11-02 | 命令输出 | 实际可解析版本 `slint = "1.18.1"`、`slint-build = "1.18.1"` |
| S3 | https://raw.githubusercontent.com/slint-ui/slint/master/CHANGELOG.md | master@2026-11-02 抓取（1.19.0 Unreleased） | 官方变更日志 | 1.19 尚未发布；1.19 段列出 macOS 默认字体修复（#13540）、CJK 斜体修复（#10178/#8788）、`TextInput`/IME 相关修复 |
| S4 | https://docs.rs/slint/1.18.1/slint/fn.invoke_from_event_loop.html | 1.18.1 文档 | 官方 API 文档 | 签名 `pub fn invoke_from_event_loop(func: impl FnOnce() + Send + 'static) -> Result<(), EventLoopError>`；线程安全、仅事件循环线程执行 |
| S5 | https://docs.rs/slint/1.18.1/slint/trait.Model.html | 1.18.1 文档 | 官方 API 文档 | `Model` trait：`row_count` / `row_data` / `model_tracker`，并提供 `push_row` / `set_row_data` / `iter` 等默认方法 |
| S6 | https://raw.githubusercontent.com/slint-ui/slint/master/LICENSE.md | master@2026-11-02 | 官方许可说明 | 框架三重许可：`LicenseRef-Slint-Royalty-free-2.0` / `GPL-3.0-only` / `LicenseRef-Slint-Software-3.0`（商业） |
| S7 | https://raw.githubusercontent.com/slint-ui/slint/master/LICENSES/LicenseRef-Slint-Royalty-free-2.0.md | 2.0 版 | 官方许可全文 | 桌面/移动/Web 专有应用可免费使用；需显示 `AboutSlint` 或 "Made with Slint" 徽标（§2 归属条件）；嵌入式除外 |
| S8 | 本机源码 `~/.cargo/registry/src/.../i-slint-backend-winit-1.18.1/winitwindowadapter.rs` | 1.18.1（构建解析结果） | 上游源码 | L1429 `WinitWindowEvent::Ime(Ime::Preedit)` → `KeyEventType::UpdateComposition`；L1438 `Ime::Commit` → `CommitComposition`；L2119 `set_ime_allowed(true/false)`；L2136 `set_ime_cursor_area(...)` |
| S9 | 本机源码 `~/.cargo/registry/src/.../winit-0.30.13/src/platform_impl/macos/view.rs` | winit 0.30.13 | 上游源码 | macOS `NSTextInputClient`：`setMarkedText:` → `Ime::Preedit`（L334）、`insertText:` → `Ime::Commit`（L413） |
| S10 | https://github.com/slint-ui/slint/issues/10861 | 2026-09-19 更新，open | 官方 issue | 窗口在 IME 组字期间失焦会丢最后一个字符 |
| S11 | https://github.com/slint-ui/slint/issues/14032 | 2026-10-13 更新，open | 官方 issue | iOS 上 `LineEdit` 无法接受中文 IME（英文正常）；移动端问题 |
| S12 | https://github.com/slint-ui/slint/issues/14292 | 2026-10-31 更新，open | 官方 issue | `FocusScope` 缺少 IME API（焦点与 IME 联动的缺口） |
| S13 | https://raw.githubusercontent.com/slint-ui/slint/master/README.md | master@2026-11-02 | 官方 README | 支持平台：Windows / macOS / Linux（桌面） |
| S14 | 本机实测（`<tmp-dir>/slint-spike`，验证后已删除） | 2026-11-02 | 命令输出 + 截图 | 构建、运行、跨线程更新、截图（见 §5） |

## 4. 结论

| 问题 | 结论 | 依据 |
|---|---|---|
| Q1 | 最新稳定为 **1.18.1**（`slint` / `slint-build` 同版本），1.19.0 尚未发布（Unreleased）。规范「1.18.x」成立，建议锁定 `1.18.1`。 | S1 + S2 + S3 |
| Q2 | macOS arm64 可直接构建运行；默认特性自带 `backend-winit` + FemtoVG + 软件渲染，链接的系统框架仅 OpenGL/AppKit/CoreText/CoreGraphics/CoreVideo/Carbon/QuartzCore/ColorSync 等，**不引入任何第三方 dylib**，无需额外系统包。 | S14（`otool -L`） |
| Q3 | 窗口用 `export component ... inherits Window`；`@image-url("...")` 在编译期把图片打包进二进制；列表用 `for` + `[T]` 属性（`ModelRc`）；定时器用 `slint::Timer`（`SingleShot`/`Repeated`）在事件循环上触发。 | S4/S5 + S14（编译期即校验，未知元素/非法赋值直接编译失败） |
| Q4 | `invoke_from_event_loop` 是官方跨线程入口：`FnOnce() + Send + 'static`，返回 `Result<(), EventLoopError>`，函数只在启动事件循环的线程执行。本机 5 次调用全部 `Ok(())`，UI 属性按序更新（counter 0→5）。 | S4 + S14 |
| Q5 | Slint 1.18.1 **在代码层面实现了 macOS IME**：winit 后端把 `Ime::Preedit/Commit` 转成 `UpdateComposition/CommitComposition`，并调用 `set_ime_allowed`、`set_ime_cursor_area`（候选窗定位），底层 winit 0.30.13 走 `NSTextInputClient`。但存在未关闭缺陷：组字期间失焦丢末字（#10861）、`FocusScope` 缺 IME API（#14292）。**端到端中文输入未在本机自动验收**（macOS TCC 拒绝 osascript 注入按键，见 §6 R1），必须由人在 M1 手测。 | S8 + S9 + S10–S12 |
| Q6 | **许可与当前 `deny.toml` 冲突**：`slint` 的 SPDX 为 `GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0`，三者都不在 5 项白名单内；依赖树还引入 `BSD-2-Clause`、`BSL-1.0`、`Zlib`、`Unlicense`。**需要负责人审批**（见 §7 与 `deps.md` §6）。 | S6/S7 + 本机 `cargo deny check licenses` 实测（§5） |
| Q7 | 干净 release 构建 71 s、release 二进制 14 MB（debug 54 MB），3755 个依赖文件/1.9 GiB 中间产物；构建期有 `slint-build` 编译 `.slint`（编译期校验，错误在构建期暴露）。 | S14 |

## 5. 版本实测

**环境**：`source ~/.cargo/env`；rustc/cargo 1.99.1。

| 依赖/工具 | 实测版本 | 命令 | 结果摘要 |
|---|---|---|---|
| `slint` | **1.18.1** | `cargo search slint` / `Cargo.lock` | 解析到 1.18.1；默认特性：`std`、`backend-default`、`renderer-femtovg`、`renderer-software`、`compat-1-2`…`compat-1-18`、`accessibility`、`system-tray` |
| `slint-build` | **1.18.1** | 同上 | build-dependency，编译 `ui/app.slint` |
| 传递依赖（关键） | winit 0.30.13、femtovg、i-slint-core 1.18.1、softbuffer 0.4.8、accesskit_winit 0.33.2、arboard 3.7.0、muda 0.19.3 | `cargo tree` | 均为 MIT/Apache-2.0 系；许可例外见 `deps.md` |
| 构建耗时 | 干净 release：**71 s**；增量重编译 `.slint`+bin：1–4 s | `cargo clean && time cargo build --release` | 3755 文件 / 1.9 GiB `target/` |
| 二进制体积 | release **14 MB**；debug 54 MB | `ls -lh target/release/slint-spike` | 静态链接 Slint 运行时，无额外 dylib |
| 动态依赖 | 仅系统框架 | `otool -L` | OpenGL、ApplicationServices、CoreGraphics、CoreVideo、Carbon、AppKit、Foundation、CoreFoundation、QuartzCore、CoreText、ColorSync、CoreServices、libSystem、libobjc、libiconv |

**最小窗口 spike（`<tmp-dir>/slint-spike`，验证后已删除）做了什么**

1. `ui/app.slint`：`Window` + 中文标题 `Text` + `@image-url` 图片 + `ListView`（`[string]` 模型）+ `Button` 回调 + `LineEdit`。
2. `src/main.rs`：`include_modules!()` 生成 `AppWindow`；后台 `std::thread` 循环 5 次 `slint::invoke_from_event_loop` 更新 `counter`/`status`；`ui.on_ping(...)` 回调；`slint::Timer::SingleShot` 3.5 s 后 `quit_event_loop`。
3. 运行结果（`run.log`）：

```
window shown, elapsed=0ms
invoke_from_event_loop -> Ok(())   ×5
final counter=5
model rows len=3
image ok=后台线程已更新 4 次
event loop exited cleanly
```

4. 截图（`screencapture -x`，窗口可见）：中文标题「Slint 最小窗口 · 中文 IME 检查」正常渲染、渐变 `Image` 正常显示、`ListView` 三行中文/英文正常、两个按钮可见。
   - 证据文件归档：`<tmp-dir>/tech-spike-evidence/slint-window.png`、`slint-run.log`（临时目录，不入库）。

**编译期行为（实测到的「真实 API 约束」，不是文档推测）**：

| 我写的 | 编译器/运行结果 | 说明 |
|---|---|---|
| `ListView` / `Button` 直接用 | `error: Unknown element 'ListView'` | 标准控件必须先 `import { ... } from "std-widgets.slint";` |
| `in property <string> status` 在 `.slint` 里赋值 | `error: Assignment on an 'in' property` | 组件内部要改的属性必须 `in-out` |
| `build.rs` + `slint_build::compile()` 但 Rust 侧不 `include_modules!()` | `error[E0433]: cannot find type 'AppWindow'` | 生成代码需 `slint::include_modules!();` |
| `text: "Ping ({counter})"` | 按钮显示字面量 `Ping ({counter})` | 字符串插值不能靠 `{}`；应写 `"Ping (" + counter + ")"` 或格式化到中间属性（`SPEC-app.md` 也要求 Slint 文件只做极薄格式化） |

**许可实测（`<tmp-dir>/slint-lic`，仅 `slint = "1.18"` + 仓库 `deny.toml`）**

```
error[rejected]: failed to satisfy license requirements
  license = "BSD-2-Clause"
  license = "BSL-1.0"                                    (×2)
  license = "Zlib"                                       (×3)
  license = "Unlicense"                                  (×1)
  license = "GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0"  (×11)
licenses FAILED
```

## 6. 风险与限制

| 风险 | 影响 | 缓解/预案 |
|---|---|---|
| **R1：中文 IME 无法自动验收**。本机 `osascript` 被 macOS TCC 拒绝（`execution error: ... 不允许辅助访问 (-1719)`），无法注入拼音按键；Slint 无 `SLINT_BACKEND=headless` 的 IME 模拟能力。 | 不能声称「中文输入已验证」 | 结论只到「代码路径存在 + 上游 issue 跟踪」；M1 由人工在真机手测：拼音组字、候选窗位置、失焦丢字（#10861）、`TextInput`/`LineEdit` 的 `has-focus` 切换。验收项写进 `SPEC-app.md` 的手测清单。 |
| R2：Slint 1.18.1 仍有 IME 相关未关闭 issue（#10861、#14292；#14032 为 iOS） | 教师录入中文批注时可能丢字符/候选窗错位 | UI 侧「保存前校验草稿文本 + 失焦即提交」；如 M1 手测失败，评估升级到 1.19（未发布）或对输入框做「点击外部先 200ms 缓冲」的规避。**不静默降级**（`SPEC-app.md` Never）。 |
| R3：许可三重表达与 `deny.toml` 白名单冲突（GPL-3.0-only / LicenseRef / BSD-2-Clause / BSL-1.0 / Zlib / Unlicense） | `cargo deny check` 永久红，CI 门禁失败；许可义务未定 | 需负责人审批（§7）。推荐：项目按 **GPL-3.0-only** 开源（与「求职展示、开放许可」一致），并在 `deny.toml` 追加 `GPL-3.0-only`、`LicenseRef-Slint-Royalty-free-2.0`、`BSD-2-Clause`、`BSL-1.0`、`Zlib`、`Unlicense` 并逐条记录理由；**不选商业许可**（无预算、非商业分发）。 |
| R4：默认渲染器为 FemtoVG（OpenGL）。macOS 27 上已实测可用（窗口/图片/文字正常），但长期 Apple 弃用 OpenGL 的风险存在 | 未来系统版本可能出现渲染异常 | 保留切换能力：`SLINT_BACKEND=winit-software` 作为兜底（默认特性已含 `renderer-software`，实测随构建产出了该渲染器）；升级 Slint 时回归截图检查。 |
| R5：`slint-build` 在构建期执行 `.slint` 编译器；`.slint` 语法错误会让整个 crate 构建失败（含 CI） | 改动 `ui/` 的门槛变高（错误在构建期而非运行期） | 视为优点并写进 `CONTRIBUTING`；把「`cargo build -p eg-app` 即校验 UI」纳入 CI 第一段。 |
| R6：未验证 HiDPI/多显示器缩放、系统深色模式、无障碍（AccessKit） | 交付外观与可达性风险 | 1.18 默认开启 `accessibility`（已随构建产出）；M2 UI 联调时补截图回归（Retina 下 `SCALE_FACTOR=2`）。 |
| R7：macOS 最低版本未从官方文档取得明确声明（残余未知） | 影响未来分发（A7 阶段） | 记录为未知；A7 打包阶段实测最低系统版本后再定。 |

## 7. 与规范的偏差

| 规范位置 | 偏差 | 处理（改规范/改实现） | 审批状态 |
|---|---|---|---|
| `SPEC.md` §4「Slint 1.18.x（不低于 1.14）」 | 实测 `slint`/`slint-build` 最新稳定 = **1.18.1**，无偏差；建议把「1.18.x」落实为精确锁定 `=1.18.1`（`[workspace.dependencies]` 写法） | 实现细节，不改规范；锁定版本写进 `Cargo.toml` 与本文 | — |
| `SPEC.md` §10 许可白名单（MIT / Apache-2.0 / BSD-3-Clause / ISC / Unicode-3.0） | Slint 三重许可 + 依赖树新增 `BSD-2-Clause`、`BSL-1.0`、`Zlib`、`Unlicense` 均不在白名单，`cargo deny` 实测失败 | **改规范（§10 白名单追加 6 项并写明理由）**：属 `SPEC.md` 「Ask first」类变更 | **待负责人审批**（见汇报） |
| `SPEC-app.md`（Slint 文件中禁止业务逻辑） | 实测 `in` 属性不能在 `.slint` 内赋值、`{}` 不做字符串插值 → 与「极薄格式化」一致，无需偏差 | 不改 | — |
| `SPEC-app.md` §4「Slint 组件实例化与首帧 ≤500ms」 | 本次只测到 `ui.show()` 到「窗口可见」的日志与截图，未做精确首帧计时（无 instrumentation） | 不改规范；M1 用 `tracing` 打点实测再验收 | — |

## 8. 复现命令

```bash
source "$HOME/.cargo/env"
# 1) 版本与文档一致性
cargo search slint --limit 1                     # → slint = "1.18.1"
# 2) 最小窗口（在 /tmp 临时 crate 中；仓库不入依赖）
mkdir -p <tmp-dir>/slint-spike/{src,ui/assets} && cd <tmp-dir>/slint-spike
#    Cargo.toml: slint = "1.18" / [build-dependencies] slint-build = "1.18"
#    build.rs:   slint_build::compile("ui/app.slint")
#    ui/app.slint: import { Button, LineEdit, ListView } from "std-widgets.slint"; export component AppWindow inherits Window { ... }
#    src/main.rs: slint::include_modules!(); 后台线程调用 slint::invoke_from_event_loop(...)
cargo build --release                            # 干净构建 71 s
otool -L target/release/slint-spike              # 仅系统框架
./target/release/slint-spike                     # 窗口可见；日志 "event loop exited cleanly"
screencapture -x run.png                         # 截图证据
# 3) 许可冲突复现
cp <repo>/deny.toml <tmp-dir>/slint-lic/deny.toml
cd <tmp-dir>/slint-lic && cargo deny check licenses   # slint + BSD-2-Clause/BSL-1.0/Zlib/Unlicense 被拒
```
