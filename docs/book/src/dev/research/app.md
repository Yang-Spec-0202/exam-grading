# 研究记录：app（Slint 外壳 · 十页导航 · 状态与通道 · 性能预算）

> 状态：T35 实现完成（M9 第一段） · 2026-11-02（Asia/Shanghai）
> 对应规范：`SPEC-app.md` v0.2.3（§2 页面/快捷键、§3 视图模型边界、§4 启动与线程、§5 失败模式、§6 可观测性、§7 测试策略与 IME 清单、§9 成功标准）
> 上游研究：`docs/dev/research/slint.md`（Slint 1.18.1 版本/构建/API/许可/IME 现状，2026-11-02）、`docs/dev/research/platform.md`（平台能力）
> 实测机器：macOS 27.0.1（26A434）/ Apple M4 / 16 GB / arm64 / rustc 1.99.1 / Xcode CLT

## 1. 模块与范围

- 模块 id：`app`（crate `eg-app`，bin）；本段只做 **T35 外壳**：主窗口、十页导航骨架、全局状态、有界命令/事件通道、线程边界、错误呈现、日志入口占位、i18n 结构、快捷键骨架、启动/切页性能实测。
- 页面内容：按任务约定先占位（每页正文显式标注「占位」与后续任务编号），导航与生命周期完整。
- 明确不做：页面业务（导入/复核/规则/授权/日志查看器在 T36–T38）、打包/签名/分发（A7 不做）、多语言（v1 仅 zh-CN）、主题定制。

## 2. 问题清单（本段需要确认的外部行为）

| # | 问题 | 影响 |
|---|---|---|
| Q1 | Slint 1.18.1 的 `Window`/`FocusScope` 键盘 API 形态；macOS 上 `KeyboardModifiers.control` 是否等于 Command ⌘ | 决定全局快捷键（Cmd+1…0 等）实现 |
| Q2 | 生成代码（`include_modules!`）与工作区 `unsafe_code = "forbid"` 的冲突 | 决定 crate lint 配置 |
| Q3 | 生成代码触发的 clippy 拒绝级 lint（unwrap/panic/todo）如何隔离 | 决定 `-D warnings` 是否可过 |
| Q4 | Slint 1.18.1 是否提供公开剪贴板 API（复制诊断 id） | 决定错误横幅「复制」是实现还是显式占位 |
| Q5 | 引入 Slint 后 `cargo deny` 的许可/公告结果 | 决定是否需要负责人决策 |
| Q6 | 启动/切页/UI 线程耗时的可测量口径（Slint 不暴露帧计时） | 决定性能验收方法与限制说明 |
| Q7 | 中文 IME 能否自动验收 | 决定 §7.1 人工清单的状态 |

## 3. 来源

| # | URL / 位置 | 版本/日期 | 类型 | 取用内容 |
|---|---|---|---|---|
| S1 | <https://docs.rs/slint/1.18.1/slint/fn.invoke_from_event_loop.html> | 1.18.1（2026-11-02 查询） | 官方 API 文档 | 跨线程投递签名 `FnOnce() + Send + 'static → Result<(), EventLoopError>` |
| S2 | 本机 `~/.cargo/registry/src/.../i-slint-compiler-1.18.1/builtin_elements.rs` | 1.18.1 | 上游源码 | `FocusScope` 的 `key-pressed(event: KeyEvent) -> EventResult`（L1425）、`capture-key-pressed`、`KeyBinding` |
| S3 | 本机 `~/.cargo/registry/src/.../i-slint-common-1.18.1/builtin_structs.rs` | 1.18.1 | 上游源码 | `KeyboardModifiers { alt, control, shift, meta }`，其中 `control` 在 macOS 是 Command（L65–74）；`KeyEvent { text, modifiers, repeat }`（L104–112） |
| S4 | 本机 `~/.cargo/registry/src/.../slint-1.18.1/lib.rs` | 1.18.1 | 上游源码 | `show()` / `hide()` / `run_event_loop()` / `run_event_loop_until_quit()`（L254–271） |
| S5 | 本机 `~/.cargo/registry/src/.../i-slint-core-1.18.1/timers.rs` | 1.18.1 | 上游源码 | `Timer::single_shot(Duration, impl FnOnce() + 'static)`（L148）；`TimerMode::Repeated` |
| S6 | 本机 `~/.cargo/registry/src/.../i-slint-core-1.18.1/platform.rs` | 1.18.1 | 上游源码 | 剪贴板仅在内部 `Platform` trait（`set_clipboard_text`），`slint::Window` **无公开剪贴板 API** → Q4 结论 |
| S7 | `docs/dev/research/slint.md` | 2026-11-02 实测 | 内部研究记录 | 版本锁定 1.18.1、构建 71s、`otool -L` 仅系统框架、IME 代码路径与已知 issue、许可结论 |
| S8 | <https://rustsec.org/advisories/RUSTSEC-2026-0192> | 2026-11-02（cargo-deny 查询） | 官方公告 | `ttf-parser` unmaintained（经 `winit → sctk-adwaita`，Linux Wayland 装饰后端） |
| S9 | 本机实测（本文件 §5–§7） | 2026-11-02 | 命令输出 | 构建、冒烟、性能分位、deny 结果 |

## 4. 结论

| 问题 | 结论 | 依据 |
|---|---|---|
| Q1 | 全局快捷键用**根级 `FocusScope`**：Slint 把 `key-pressed(event)` 转发给 Rust 回调（`text` + 四个修饰键布尔），动作解析放在 `nav.rs`（纯逻辑可单测）。macOS 上 `modifiers.control` 即 Command ⌘（`modifiers.meta` 才是 Control 键），因此 Cmd 组合判断用 `control && !meta && !alt`；`Cmd+M` 不接管（让给系统最小化），`Cmd+Shift+M` 才切模式。窗口启动后由 Rust 调 `take-focus()` 让快捷键作用域获得焦点，切页后重新聚焦。 | S2 + S3 + 本机冒烟 |
| Q2 | **冲突**：工作区 `unsafe_code = "forbid"` 与 Slint 生成代码里的 `#[allow(unsafe_code)]`（VTable 实现）编译期冲突（E0453）。Cargo 不允许在继承 `workspace = true` 的同时覆盖单个 lint，故 `eg-app` 改为手动复制工作区 lint 清单，仅把 `unsafe_code` 放宽为 `allow`（本 crate 手写代码不含 unsafe）。 | 本机编译错误 + Cargo 报错「cannot override workspace.lints」 |
| Q3 | 生成代码含大量 `unwrap/panic/todo` 与 `pub` 内部项。用**独立 module 包住 `include_modules!` 并 allow 生成代码的 lint 组**，手写代码继续走 `unwrap/expect/panic = deny`；冒烟与性能代码路径不触碰生成代码的 lint 豁免。 | 本机 clippy `-D warnings` 实测 |
| Q4 | Slint 1.18.1 `Window` 无公开剪贴板 API → 「复制诊断 id」在 T35 为**显式占位**（提示文案 + 诊断 id 显示在横幅里），不伪造「已复制」。真实剪贴板在 T38 随日志查看器接入（Ask first：引入界面依赖之外的剪贴板 crate 需审批）。 | S6 |
| Q5 | `cargo deny check`：**licenses/bans/sources 通过**（`GPL-3.0-only` 分支满足 Slint 三重许可表达式，其余 `BSD-2-Clause/BSL-1.0/Zlib/Unlicense` 已在白名单）；**advisories 新增 1 条** `RUSTSEC-2026-0192`（`ttf-parser` unmaintained，Linux-only `sctk-adwaita` 链），已上报负责人（deny.toml 不在本任务写权限内）。 | S8 + §7.6 |
| Q6 | Slint 不暴露「首帧/帧耗时」API；采用可复现的三段口径：① 冷启动 = 进程启动 → 事件循环首个回调（10 次脚本汇总 P50/P95）；② 切页 = 「状态迁移 + 属性投影」同步耗时（200 样本 P95）；③ UI 线程 = 导航/事件应用/命令投递三类同步工作 p99。**GPU 渲染/合成时间未计**，如实标注为限制。 | S4 + S5 + §7 |
| Q7 | 仍**无法自动验收**（macOS TCC 拒绝 `osascript` 注入按键；Slint 无 headless IME 模拟）→ §7.1 五项全部标「未验收（阻塞）」，附手工步骤；不得宣称中文输入可用。 | S7（slint.md R1）+ §7.5 |

## 5. 版本实测

| 依赖/工具 | 实测版本 | 说明 |
|---|---|---|
| `slint` | **1.18.1**（`=1.18.1` 精确锁定） | 默认特性：winit 后端 + FemtoVG/软件渲染 + accessibility |
| `slint-build` | **1.18.1** | `build.rs` 编译 `ui/app.slint`，构建期校验 |
| winit / i-slint-core / backend | 0.30.13 / 1.18.1 / 1.18.1 | `cargo tree` 实测 |
| 构建（debug，增量） | 首次 34.5s（本机，依赖已缓存） | `cargo build -p eg-app` |
| 构建（release） | 干净 release 61s | `cargo build -p eg-app --release`（依赖 + crate，含 `.slint` 编译） |
| 二进制体积 | release **18 MB** | 静态链接 Slint 运行时 + 系统框架动态链接（与 `slint.md` 的 14 MB spike 同量级） |

实现文件（`crates/eg-app/`）：

| 文件 | 内容 |
|---|---|
| `ui/app.slint` | 主窗口：顶栏（模式徽标/后端状态/设置/日志入口）、错误横幅（摘要/详情/诊断 id/重试）、二次确认条、左侧十页导航、页面骨架、提示气泡、状态栏（队列/通道计数/进度/快捷键）；全部文案来自 Rust 属性（0 中文字面量） |
| `src/main.rs` | 组合根：启动序列、线程装配、回调接线、`--smoke` / `--bench` 模式、启动失败诊断界面 |
| `src/state.rs` | 全局状态与迁移：导航守卫、模式切换守卫/确认、错误横幅、提示气泡（有界）、队列/通道视图、`ViewModel` 投影 |
| `src/nav.rs` | 十页枚举/顺序/快捷键数字、修饰键模型、全局快捷键解析 |
| `src/channel.rs` | 有界命令通道（64）/事件通道（256）、合并与危险事件暂存、计数快照、拒绝语义 |
| `src/ui_bridge.rs` | `ViewModel → Slint` 属性投影、`UiDispatcher`（`invoke_from_event_loop` + 弱引用计数） |
| `src/worker.rs` | 后台 worker：独占 `Platform`、模式切换写配置+审计、占位后台任务、启动日志 |
| `src/i18n.rs` | 中文默认词条（十页文案/模式/错误/提示/占位），编译期强制字段齐全 |
| `src/perf.rs` | UI 线程耗时采样（上限 4096）与分位统计 |
| `src/bench.rs` | 切页 200 样本 / 事件 200 样本 / 命令 50 样本的预算判定与机器可读输出 |
| `src/gates.rs` | 结构门禁单测：`.slint` 无中文硬编码/无 IO 标记、`eg-app` 不直接依赖 DB 驱动/HTTP 客户端、视图文案非空 |

## 6. 风险与限制

| 风险 | 影响 | 缓解 |
|---|---|---|
| R1：GPU 渲染/合成时间无公开计时 API | 「UI 线程 p99 ≤16ms」只覆盖同步工作，不含绘制 | 记录口径；后续如需帧计时，用 `SLINT_DEBUG_PERFORMANCE`/帧回调（未验证）再补 |
| R2：快捷键作用域依赖 `FocusScope` 焦点；点击输入框后焦点转移，靠事件冒泡兜底 | 某些控件（文本编辑）可能吞掉部分组合键 | 文本编辑态不触发单字母快捷键（§2.1 要求）；后续 T37/T38 在复核/规则页加页面级 KeyBinding；冒烟确认 Cmd 组合仍被根 FocusScope 接收（待人工键盘验证） |
| R3：Slint 生成代码的 lint 豁免可能掩盖生成代码问题 | 与手写代码质量门禁分离 | 生成代码由 1.18.1 编译器生成，可信；手写代码仍全量 `-D warnings` |
| R4：页面内容为占位 | 无法完成 §9 的业务类成功标准 | 按任务约定由 T36–T38 补齐；导航/线程/通道/错误呈现可先行验收 |
| R5：IME 未验收 | 不能宣称中文输入可用 | 见 §7.5；在 M1/真机由人完成五项清单 |
| R6：`ttf-parser` 公告（Linux-only 链） | CI deny 红 | 上报负责人；macOS 发行不含该链 |

## 7. 与规范的偏差

| 规范位置 | 偏差 | 处理 | 审批状态 |
|---|---|---|---|
| `SPEC.md` §7「库 crate 禁止 unsafe」/ 工作区 `unsafe_code = "forbid"` | `eg-app` 手动复制 lint 清单并把 `unsafe_code` 放宽为 `allow`（Slint 生成代码含 `#[allow(unsafe_code)]`，forbid 无法覆盖） | 已在本文件 §4 Q2 记录；手写代码 0 unsafe；后续如 Slint 生成代码去掉该属性可恢复 forbid | **已记录，待负责人确认** |
| `SPEC-app.md` §3「Slint 文件禁止中文硬编码」 | 生成代码不在扫描范围；手写 `ui/app.slint` 0 中文字面量（门禁单测） | 无偏差（门禁已覆盖） | — |
| `SPEC-app.md` §2「十页」 | 规范十页含 `home`，无独立「关于」页；任务描述提到「关于」。实现按规范十页，**关于信息作为设置页内区块**（不新增页面，避免 Ask first） | 已记录 | — |
| `SPEC-app.md` §2.1 页面级快捷键（J/K/Enter/E/Space/X/Cmd+Z/Cmd+Enter/Cmd+E） | T35 只实现全局快捷键骨架（Cmd+1…0、Cmd+,、Cmd+L、Cmd+Shift+M、Esc、Cmd+O/Shift+O 占位导航）；页面级快捷键属复核/规则/报表页（T37/T38） | 已记录；`nav.rs` 的解析器可扩展 | 按任务边界 |
| `SPEC-app.md` §4 启动序列「探测本地后端健康（不阻塞首帧）」 | T35 未接 inference 健康探测，后端徽标如实显示「未接入」占位，不伪造就绪 | 已记录；T36/T38 接入 | — |
| `SPEC-app.md` §9 标准 7/8/11/12（授权/日志/抽检/设置业务） | 页面内容占位，未实现 | 按任务边界由后续任务完成 | — |
| `eg-app` 依赖（组合根） | 开发过程中 `eg-report` 由并行代理修复前曾暂时注释三个下游依赖；**现已恢复** `eg-inference`/`eg-review`/`eg-report`，`cargo build -p eg-app` 通过 | 无偏差 | — |

## 8. CP-B/T35 验收记录

### 8.1 启动与切页性能（实测）

环境：macOS 27.0.1 / M4 / 16GB / arm64；debug 构建（`cargo build -p eg-app`）；数据目录 `EXAM_GRADING_DATA_DIR` 指向临时目录；`--smoke` 模式在首帧后自动退出。

| 指标 | 规范目标 | 测量方法 | 实测 | 结论 |
|---|---|---|---|---|
| 冷启动（进程启动 → 首帧回调） | P50 ≤1.0s、P95 ≤1.5s | 同一数据目录连续 10 次 `--smoke`，取 `first_frame` 分位 | **debug：P50 135.6ms / P95 183.4ms**（首次含 Slint 初始化 561ms；10 次 120.5–183.4ms）；**release：P50 77.4ms / P95 136.9ms**（10 次 68.8–136.9ms） | 达标 |
| 切页（状态迁移 + 属性投影） | P95 ≤100ms、无一次因 IO >200ms | `--bench`：10 页 × 20 轮 = 200 样本 | **debug：P50 0.039ms / P95 0.041ms / P99 0.046ms / max 0.046ms**；**release：P50 0.003ms / P95 0.003ms / max 0.014ms** | 达标（无 IO，纯内存） |
| UI 线程同步工作 p99 | p99 ≤16ms | `--bench`：导航 200 + 事件 203 + 命令 50 样本 | **debug：事件 p99 0.062ms / 命令 p99 0.003ms / 导航 p99 0.046ms**；**release：事件 p99 0.005ms / 命令 p99 0.002ms / 导航 p99 0.003ms** | 达标 |
| 通道满显式提示 | 不静默丢 | 单测：填满命令通道 64 → `Busy` 计数；事件 256 满 → 进度合并/错误暂存/普通计数 | `channel::tests::*` 全过；状态栏显示合并/丢弃/错误丢弃/命令拒绝 | 达标 |
| UI 线程 invoke 失败 | 丢弃并计数 | `--bench` 输出 `invoke_failures` | 0 | 达标 |

启动分阶段（同一次 `--bench`）：debug：`platform_open=25.1ms`、`ui_new=83.6ms`、`ui_show=8.6ms`、`first_frame=128.0ms`；release：`platform_open=28.9ms`、`ui_new=72.4ms`、`ui_show=0.6ms`、`first_frame=115.8ms`（release `ui_new` 仍含 Slint 组件初始化，属一次性成本）。

**测量口径与限制**：`first_frame` = 进程启动到事件循环首个回调（Slint 不暴露真实 present 时间，差值在 1 帧级）；切页/事件样本为同步代码耗时，不含 GPU 绘制。数据为单机单次会话，未做跨机器校准。

### 8.2 手动冒烟记录（可复现命令）

| 场景 | 命令 | 观察结果 |
|---|---|---|
| 启动 + 十页外壳 | `EXAM_GRADING_DATA_DIR=<data-dir>/eg-app-smoke cargo run -p eg-app -- --smoke` | 窗口显示；`error_visible=false`；`page_index=0`；模式「标准模式」；通道计数全 0；提示行正常 |
| 后台进度（worker → 事件通道 → UI） | 同上（冒烟自动发 `RunPlaceholderJob`） | `progress="占位后台任务 10/10"`，说明 worker 的 10 条进度事件已回投并投影 |
| 错误呈现 + 详情折叠 | `... -- --smoke --demo-error` | `error_visible=true`、`error_detail_visible=false`（默认折叠）、模式/页面/通道状态正常 |
| 性能 | `... -- --bench` | 输出见 §8.1，`verdict=pass`，退出码 0 |
| 启动失败诊断 | 制造平台启动失败（如数据目录不可写） | 显示「启动失败」横幅 + 中文引导 + 退出按钮（`run_startup_error`）；本次未注入失败，代码路径由类型系统/日志覆盖（T36 补失败注入测试） |

未做（需要人工/交互）：鼠标点击导航、键盘真实按键（TCC 拒绝自动化注入）、窗口缩放/HiDPI、深色模式。

### 8.3 中文 IME 人工验收清单（`SPEC-app.md` §7.1）

**状态：全部「未验收（阻塞）」**。阻塞原因（实测，来自 `slint.md` R1）：本机 macOS TCC 拒绝 `osascript` 注入按键（`-1719 不允许辅助访问`）；Slint 1.18.1 无 headless IME 模拟；因此无法由代理自动完成端到端中文输入验收。**不得据此声称「中文输入可用」。**

人工步骤（在真机桌面执行，任一项失败即记缺陷并在界面显式提示，不静默降级）：

1. 打开任一带输入框页面（T37 复核批注/T38 规则原文），用拼音输入法连续输入「zhongguo」，确认预编辑串逐字母显示、无乱码；
2. 观察候选窗跟随光标、不遮挡输入行、不越出窗口；
3. 组字未上屏时点击窗口其他区域/切换焦点，确认末字不丢（追踪上游 issue #10861）；
4. 在两个输入框间交替聚焦，确认仍可输入且候选窗重新定位；
5. 连续输入 ≥200 字中文，确认不卡顿、不丢字、撤销/粘贴正常。

结论：**5/5 未验收**；移交 M1 真机人工，或后续引入可注入按键的自动化环境（需系统授权）后再补。

### 8.4 cargo deny 结果（Slint 引入后）

```
cargo deny check
→ bans ok, licenses ok, sources ok
→ advisories FAILED: RUSTSEC-2026-0192（ttf-parser unmaintained）
```

- **licenses 通过**：`slint` 的 `GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0` 由 `GPL-3.0-only` 分支满足；`BSD-2-Clause/BSL-1.0/Zlib/Unlicense` 已在白名单（task-21 决策）。
- **advisories 新增 1 条（已按批准处置）**：`RUSTSEC-2026-0192` ttf-parser（unmaintained，无安全升级、非漏洞、无 CVE）。链路：`eg-app → slint 1.18.1 → i-slint-backend-winit → winit 0.30.13 → sctk-adwaita 0.10.1（Linux/Wayland 窗口装饰后端）→ ab_glyph → owned_ttf_parser → ttf-parser 0.25.1`。`sctk-adwaita` 只在 Linux/Wayland 编译，macOS 发行构建不含该分支；但 `deny.toml [graph]` 有意不限制 target，故整图被检查。**处置**：经负责人批准，在 `deny.toml [advisories].ignore` 追加 `"RUSTSEC-2026-0192"` 并写明链路/Linux-only/发行目标 macOS/上游无安全升级/将来支持 Linux 需重评；**未放宽其他 advisories 策略、未改 licenses 白名单**。这与 platform 的 keyring→zbus 3 条（RUSTSEC-2026-0370/0371/0372）处置同口径，属「Linux-only 依赖链的 unmaintained 公告」先例。
- **复跑结果**：`cargo deny check` → `advisories ok, bans ok, licenses ok, sources ok`（Slint 引入后 licenses 全绿：`GPL-3.0-only` 分支满足其三重许可表达式）。

### 8.5 T36–T38 页面实现记录（2026-11-03，task-43 第二轮）

**本轮交付（仅 `crates/eg-app/**`）**

| 层 | 内容 |
|---|---|
| 页面视图模型 | `src/pages/{mod,import,progress,settings,review,rules,consent,diagnostics}.rs`：导入（命名提醒仅一次/部分成功/拒收失败明细）、进度（任务行/进度合并/失败重试）、设置（12 分组、只读不变量带原因、类型化 patch 与非法值拒绝、数组编辑）、复核（队列/详情/键盘映射含编辑态抑制、改分输入、原因码、批量、抽检弹窗默认单题、导出后修改提示）、规则（原文→整理失败显式→草稿→校验→确认/拒绝守卫）、授权（列表/新建校验/撤销/额度用尽文案）、诊断（字段过滤/分页/索引降级/导出）。 |
| 服务层 | `src/services.rs`：eg-capture 真实导入（内存资产槽 + osascript 文件/文件夹选择 + 拖拽路径）；platform 设置原子保存/审计/`.bak` 恢复；platform 任务重试/取消；platform 授权列表/登记/撤销；platform 日志查询与诊断包导出；eg-review 内存适配器（队列/详情/命令/撤销/抽检提示）；eg-rubric 内存引擎（手写直译→校验→确认/拒绝）。 |
| 通道/状态 | 新命令 20+、新事件 25+（容量 64/256 与合并/错误类策略不变）；`AppState` 集成页面 VM、事件回投、按页文本投影、`page-actions` 动作列表。 |
| UI | `ui/app.slint` 新增动作按钮行 + 交互控件：设置键值编辑、复核改分输入/原因 ComboBox/抽检范围按钮、规则手写判据+分值、授权新建表单（任务/后端/范围/预算/TTL）。所有文案经 `i18n::page_labels()`（`.slint` 0 中文字面量）。 |

**逐页冒烟（可复现）**

```bash
cargo build -p eg-app --release
./target/release/eg-app --smoke
./target/release/eg-app --smoke --demo-error
```

实测输出（2026-11-03，macOS 27.0.1 / M4 / 16GB）：

| 页 | 冒烟首行数据 |
|---|---|
| home | 首页快照占位（T36 后续接 report/platform 快照） |
| papers | 试卷页占位（未在本任务范围） |
| rules | `状态：未开始`（规则页状态机已接） |
| import | 拖拽/选择提示（`import_hint`） |
| progress | `运行中 0 / 待运行 0 / 失败 0` |
| review | `标准 3 · 抽检 0 · 已确认 0 · 合计 3`（eg-review 夹具队列） |
| report | 报表页占位（report 整页不在 T36–T38 范围） |
| consent | 空态提示（platform 无授权记录） |
| settings | `配置分组 12 组`（TOML 实读，只读项带原因） |
| diagnostics | `级别=全部 / 模块=全部 / 关键词=无 · 第 1/1 页 · 共 N`（platform 日志实读） |

`--smoke --demo-error` 仍输出 `injected_error=1`、`error_visible=true`、`error_detail_visible=false`，错误呈现未回退。启动/切页性能见 §8.1（`--bench` 仍通过）。

**门禁**

- `cargo test -p eg-app`：67 passed / 0 failed（页面 VM 单测：键盘映射、命名提醒一次、设置只读/非法值、抽检默认单题/取消不重跑、导出后修改提示、绑定与命令映射）。
- `cargo fmt -p eg-app -- --check`：通过。
- `cargo clippy -p eg-app --all-targets --no-deps -- -D warnings`：**通过**。
- 全依赖 clippy（`cargo clippy -p eg-app --all-targets -- -D warnings`）：**通过**（0 errors；期间 eg-platform v3 并行改动一度阻断，spec-ingest 修复后复跑全绿）。本 crate 未修改 platform。
- `cargo build -p eg-app --release`：通过（release 二进制可运行 `--smoke`）。

**与规范的偏差 / 剩余 TODO**

1. **UI 形态**：当前页面内容以「文本投影 + 动作按钮/表单」呈现，未做三栏图像证据可视化；图像证据按资产 id 占位（界面提示），待 platform 资产只读接口接入后懒加载。不是规范放弃，而是本轮 UI 收尾折中。
2. **数据源**：复核队列/命令用 `eg-review` 内存适配器；导入用 capture 内存资产槽；规则确认用 rubric 内存存储。platform v3（review 队列/撤销栈/导出记录、grade 幂等）完成后替换为真实实现（第三轮）。
3. **规则整理**：模型整理路径显式失败（「规则整理需要本地模型…」），符合 `SPEC-app.md` §5 失败模式；手写直译路径可完成「草稿→校验→确认/拒绝」全流程。inference 整理角色接入后替换为真实调用。
4. **报表导出联动**：复核页「导出后修改提示」状态逻辑已实现，但真实导出由报表页/report 适配接入（report 整页不在 T36–T38）。
5. **未做人工验收**：20 题纯键盘 E2E（≥15 题/分钟）、中文 IME §7.1（TCC 阻塞，见 §8.3）、窗口缩放/HiDPI、深色模式。

### 8.6 platform v3 真实化与端到端冒烟（2026-11-03，task-43 第三轮）

**已真实化的适配器（`crates/eg-app/src/platform_adapters.rs` + `services.rs`，未改 platform）**

| 能力 | 实现 |
|---|---|
| 复核队列 | `repo::list_grade_records_for_queue`（`GradeQueueFilter`/`count_grade_records_for_queue`）+ `PlatformReviewRepo` 适配 `eg-review::ReviewRepository`；`eg-review::ReviewService` 业务规则不变 |
| 复核详情 | `repo::load_grade_evidence_summary`（资产/识别摘要）+ `get_submission`/`list_questions`；`ReviewMetadata`/`AnswerView` 真实映射；比较器轨迹/模型调用按 id 查询 platform 暂无 getter → 返回 None（UI 降级） |
| 复核命令 | 仍由 `ReviewService` 生成修订；落库走 `repo::append_grade_record_with_reasons`（platform 事务）+ `audit::command_write`/`AuditLog::append`（命令幂等键/原因码/范围/抽检结论）+ 提交级排除 `upsert/delete_submission_exclusion` |
| 撤销/重做 | undo 持久化 `repo::push_undo`/`pop_undo`（payload 编码 before/after 快照）；redo 暂存进程内（platform 无 redo 表，TODO） |
| 报表导出 | `ReportInput` 由 platform 读取（试卷/名单/提交/全部修订/排除）→ `eg_report::Report::build` + `export`（XLSX+CSV+HTML）→ `PlatformExportAuditor` 在 `audit::command_write` 内写 `repo::insert_export_record` + 审计链 |
| 规则页整理 | `StructurerPort` 未接（inference 整理角色未装配）→ 保持显式失败（手写直译路径完整）；已标 TODO |
| 导入落库 | 仍用 capture 内存资产槽（未接 `eg_capture::persist`：需先落 platform 资产库与提交/页面/任务阶段，TODO） |

**端到端冒烟（新增 `--smoke` 链路：夹具落库 → platform 队列 → 复核改分 → 报表导出）**

```bash
cargo build -p eg-app --release
rm -rf <data-dir>/eg-app-final-smoke
EXAM_GRADING_DATA_DIR=<data-dir>/eg-app-final-smoke ./target/release/eg-app --smoke
```

实测（2026-11-03，macOS 27.0.1 / M4；`first_frame=189.5ms`）：

```
[smoke-chain] 导入/落库：夹具 3 提交 × 1 题已写入 platform；任务/复核队列：3 条（标准 3 · 抽检 0 · 合计 3（platform 队列））；复核改分：命令已执行：override；报表已导出到 …/exports（10 个文件，43 行）
```

- 导出产物：`冒烟试卷_全部_<时间>.xlsx/.html` + 8 个 CSV（成绩单/班级汇总/班级分布/题目统计/错题标签/选项分布/快速抽检/元信息）；
- 复核页显示 platform 队列 3 条；进度页显示待运行 1（冒烟夹具任务）；诊断页共 3 条（导出/复核审计日志）；
- `command_rejected=0 / event_merged=0 / event_dropped=0 / critical_dropped=0`；
- 过程中修复 1 个真实缺陷：`PlatformReviewRepo::metadata` 在 `db.read` 闭包内再次 `db.read` 造成读连接 Mutex 自死锁（现象：`--smoke` 无输出挂起）；改为单次读事务内取提交+题目标签后正常。

**门禁（本轮实测）**

- `cargo test -p eg-app`：67 passed / 0 failed；
- `cargo clippy -p eg-app --all-targets -- -D warnings`：0 errors（含依赖；platform v3 由 spec-ingest 自测）；
- `cargo fmt -p eg-app -- --check`：通过；
- `cargo build -p eg-app --release`：通过；`--smoke` 退出码 0（控制台输出见上）。

**剩余 TODO（第四轮/后续）**

1) 导入真实落库：`eg_capture::persist` + platform 资产库/提交/页面/任务阶段联动（当前内存槽仅供 UI 冒烟）；
2) `eg-report` 的「导出后修改→重新导出」联动：`export_record` 已落库，`review` 页 `export_stale` 已有状态逻辑，待报表页接入 report 动作（T34 页面）；
3) redo 栈持久化（platform 仅 undo 表）、比较器轨迹/模型调用按 id 查询 getter；
4) 规则整理 `StructurerPort`（inference 装配）与图像证据懒加载；
5) 单题重判 `RegradeEngine` 真实装配（当前显式失败，需 grade 流水线）；
6) 人工验收：20 题键盘 E2E、中文 IME §7.1（TCC 阻塞）、缩放/HiDPI。

### 8.7 S1 UI 端到端冒烟与集成收尾（2026-11-04，task-50）

**本轮集成**

| 项 | 状态 |
|---|---|
| 导入真实落库 | 已改：`PlatformAssetSink`（capture `AssetSink` → `AssetStore::put`，资产文件+行同写）→ `eg_capture::persist` 同事务写 Submission/Page/审计；导入时先建 `TaskQueue` 任务并 `persist(task_id)` 推进到预处理阶段；失败/部分成功/命名提醒逻辑不变。**真实路径依赖已存在的试卷**（platform 暂无 `list_papers`，`import_context` 用行查询兼容 shim，待平台补 API）；`fixtures/paper-20q` 未就绪时冒烟走内置夹具并显式记录 |
| 报表导出联动 | `Command::ExportReport`（报表页主操作/动作按钮）→ `Report::build + export + insert_export_record` → `Event::ReportExported`：清 `review.export_stale`、报表页显示导出摘要；复核改分/撤销后若曾导出则报表页显示「导出后已有新的复核修改，请重新导出」；`state::tests::report_page_tracks_export_and_stale_after_review_edit` 覆盖跳出/复位 |
| 图像证据懒加载 | `Event::ReviewImage`（platform 资产路径）→ UI 线程 `slint::Image::load_from_path`；失败写 `review-image-note` 显式提示；复核页改三栏最小布局（左：队列；中：图像 + 证据提示；右：改分/原因/抽检/动作） |
| 重判 | 未接真实 `RegradeEngine`（需 grade 流水线 + subject/inference 装配）；保持显式失败「单题重判需要 grade 流水线装配」，原判定保留（TODO） |
| redo | 仍进程内（platform 无 redo 表；undo 已持久化）；如实记录 TODO |

**S1 冒烟（可复现）**

```bash
cargo build -p eg-app --release
rm -rf <data-dir>/eg-app-s1
EXAM_GRADING_DATA_DIR=<data-dir>/eg-app-s1 ./target/release/eg-app --smoke
```

实测（2026-11-04，macOS / M4；`first_frame=423.2ms`）：

```
[smoke-chain] 基础夹具：3 提交 × 1 题已写入 platform；fixtures/paper-20q 未就绪：使用内置夹具…；
              任务/复核队列：3 条（标准 3 · 抽检 0 · 合计 3（platform 队列））；
              复核改分：命令已执行：override；报表已导出到 .../exports（10 个文件，43 行）
[smoke] page report: 报表已导出到 .../exports（10 个文件，43 行）
[smoke] ... image_note=""（证据图像加载成功）
[eg-app] exit: command_rejected=0 event_merged=0 event_dropped=0 critical_dropped=0
```

链路：内置/夹具数据落库 → 任务队列（待运行 1）→ platform 复核队列 3 条 → override 改分（审计+修订）→ 报表导出（xlsx+html+8 CSV，43 行）→ 报表页显示导出摘要；图像证据 1×1 PNG 懒加载成功（无失败提示）。
`fixtures/paper-20q` 未就绪时容错：若目录存在图片（`submissions/`、`images/` 或目录下 jpg/png/pdf），自动走真实 `import+persist` 并为导入提交补夹具判分记录再入队；否则用内置夹具（本次实测即此分支）。

**门禁（本轮实测）**

- `cargo test -p eg-app`：68 passed / 0 failed（新增报表 stale 用例）；
- `cargo clippy -p eg-app --all-targets -- -D warnings`：0 errors；
- `cargo fmt -p eg-app -- --check`：通过；
- `cargo build -p eg-app --release`：通过；`--smoke` 退出码 0。

**剩余 TODO**

1) `fixtures/paper-20q` 就绪后复跑真实 import+persist 分支并核对 20 题夹具的队列/导出统计；
2) platform 补 `list_papers` 后移除 `import_context` 行查询 shim，并在导入页加试卷/班级选择；
3) RegradeEngine 真实装配（eg-review + eg-grade：幂等键、失败保留原判）；
4) redo 持久化（platform 增加 redo 表或复用 undo 反向记录）；
5) 人工验收：20 题键盘 E2E、中文 IME §7.1（TCC 阻塞）、窗口缩放/HiDPI。

### 8.8 外部夹具 `fixtures/paper-20q` 真实导入冒烟（2026-11-04，task-54）

**夹具与命令**

- 试卷/班级/名单：`fixtures/paper-20q/{paper,classroom,students}.json`（20 题、3 学生；与 eg-domain 类型直接反序列化后落 platform）；
- 图片：`fixtures/images/`（9 个文件：张三 p1/p2、李四 p1/p2、SO2026-003 p1/p2、`IMG_20261104_001.jpg` 不规范命名、`王五_exif6.jpg` EXIF、`张三_p2-copy.png` 与 `张三_p2.png` 同字节重复）；
- 命令：`EXAM_GRADING_DATA_DIR=<data-dir>/eg-app-s1fix target/release/eg-app --smoke`。

**实测结果（数据根 `<data-dir>/eg-app-s1fix`，本机 macOS/M4，首发 160.1ms）**

```
[smoke-chain] 基础夹具：fixtures/paper-20q 试卷（20 题 / 3 学生）；
              真实 import+persist：文件 9（成功 9 / 失败 0 / 拒收页 0 / 重复 1，命名提醒 shown=true）
              → 提交 8 / 页 8 / 资产链接 32 / 审计 17；补夹具判分 8 条；
              任务队列：2 条（submission_grade/Grade，submission_grade/）；
              冒烟断言：3/3 通过；
              任务/复核队列：8 条（标准 8 · 抽检 0 · 合计 8（platform 队列））；
              复核改分：命令已执行：override；
              报表已导出到 <data-dir>/eg-app-s1fix/exports（10 个文件，59 行）
[smoke] page progress: 运行中 0 / 待运行 1 / 失败 0
[smoke] page review: 标准 8 · 抽检 0 · 合计 8（platform 队列）
[smoke] page report: 报表已导出到 <data-dir>/eg-app-s1fix/exports（10 个文件，59 行）
[smoke] image_note=""（图像证据加载成功）
```

`--smoke --demo-error`（数据根 `<data-dir>/eg-app-s1err`）：`injected_error=1`、`error_visible=true`、`error_detail_visible=false`，复核队列同为 8 条。

**断言（冒烟内嵌 3/3 通过 + 人工核对）**

| 断言 | 实测 |
|---|---|
| 命名提醒仅一次 | `report.naming_reminder_shown=true`（一次导入批次内只提示一次） |
| 导入成功/失败/重复明细 | 成功 9 / 失败 0 / 拒收页 0 / 同批重复 1（`张三_p2-copy.png` 命中去重，不重复建页） |
| 真实落库 | 提交 8 / 页 8 / 资产链接 32 / 审计 17（`eg_capture::persist` 单事务） |
| 任务队列推进 | 导入任务 enqueue→claim→`persist(task_id)` 推进到预处理→complete；另入队 1 条待判分任务（进度页「待运行 1」） |
| 复核队列条数 | platform 队列 8 条（标准 8） |
| 错误呈现 | `--demo-error` 下 `error_visible=true`、详情默认折叠、无崩溃 |
| 导出 | XLSX+HTML+8 CSV，10 文件，59 行（8 提交 × 20 题口径下按有判分记录的题统计） |

**本轮修复的 app 侧缺陷**

1. **夹具图片目录**：真实分支原只查 `fixtures/paper-20q/**`，实际图片在 `fixtures/images/`；候选目录已加 `fixtures/images`。
2. **夹具试卷来源**：冒烟原先落内置 3 提交夹具，现改为优先读取 `fixtures/paper-20q` 的 classroom/paper/students（20 题/3 学生），失败才回退内置。
3. **任务阶段推进非法**：platform `advance_stage` 仅接受 running；capture persist 会直接推进 pending 任务导致 `任务状态迁移非法：pending -> stage`（业务数据已提交但报错）。修复：导入前 `enqueue` → `claim` 校验拿到自己刚入队的 id → `persist(task_id)` → `complete`；claim 不到则不传 task_id（不阻塞导入）。
4. **冒烟时序**：链路（9 张 2480×3507 图解码 + persist + 导出）未完成时就逐页采集，导致复核页采到 0 条；`--smoke` 增加 3.2s 链路等待屏障 + 6.2s 总结采集。
5. **任务幂等键**：`NewTask` 幂等键不含 payload，补的待判分任务与导入任务同 kind/scope 会被去重成 1 条；改为 `.with_scope(["s1-fixture-grade"])` 后任务队列 2 条。

**已知限制（如实记录）**

1. **多页归组未接**：capture 当前一文件一提交，9 文件 → 8 提交（重复文件去重）；同学生 p1/p2 尚未合并为一个提交，待 align/tech-spike 多页归组接口就绪后复跑。
2. **身份/名单匹配未接**：`IMG_20261104_001.jpg`（不规范命名）与名单外文件按文件成功导入，学号匹配属 align 阶段，本轮不做判定；EXIF 旋转只由 capture 质量/方向处理，未接 OCR/align 验证。
3. 补判分记录为夹具（`append_fixture_grades`，原因 `LowConfidence`），用于驱动复核/导出链路；真实判分流水线未接。
4. 报表 59 行与 20 题口径的差异来自当前仅有题 1 的判分记录（夹具判分统一挂首题），非统计缺陷，真实判分后复测。

## 9. 复现命令

```bash
source "$HOME/.cargo/env"
cd <repo-root>

# 1) 构建（含 .slint 构建期编译校验）
cargo build -p eg-app

# 2) 单测（状态机/通道/导航/i18n/门禁）
cargo test -p eg-app

# 3) 冒烟：启动 + 进度 + 错误呈现
EXAM_GRADING_DATA_DIR=<data-dir>/eg-app-smoke cargo run -p eg-app -- --smoke
EXAM_GRADING_DATA_DIR=<data-dir>/eg-app-error cargo run -p eg-app -- --smoke --demo-error

# 4) 性能（切页/事件/命令分位；未达标退出码 2）
EXAM_GRADING_DATA_DIR=<data-dir>/eg-app-bench cargo run -p eg-app -- --bench

# 5) 冷启动 P95（10 次）
for i in $(seq 1 10); do
  EXAM_GRADING_DATA_DIR=<data-dir>/eg-app-cold cargo run -q -p eg-app -- --smoke 2>/dev/null | grep -o 'first_frame":[0-9.]*'
done

# 6) 依赖合规
cargo deny check
```

## 10. 后续任务接口约定（T36–T38）

- 新页面逻辑放 `src/pages/<page>.rs`（按 `tasks/todo.md` T36–T38 文件清单），只依赖 `state`/`ui_bridge`/`channel` 投影，不直接 IO；
- 新的后台动作 = `channel::Command` 变体 + `worker.rs` 分支 + `channel::Event` 回投，UI 线程不等待；
- 新快捷键在 `nav.rs` 增补并加单测；页面级快捷键放对应页的 `FocusScope`（不污染根作用域）；
- 所有用户可见文案加到 `i18n.rs`（编译期强制齐全），不得写进 `.slint`；
- 新事件默认不可合并；进度/快照类事件在 `Event::merge_key` 注册合并键；错误类事件保持 `is_critical`；
- `eg-capture`/`eg-grade`/`eg-inference`/`eg-review`/`eg-report`/`eg-rubric` 依赖已在 `crates/eg-app/Cargo.toml` 启用；当前组合根装配为：capture 内存槽 + review 内存适配器 + rubric 内存存储 + platform 真实配置/授权/日志/任务；
- 页面实现约定（T36–T38 已落地）：每页一个 `pages/<page>.rs` VM + `state` 投影 + `worker` 命令分派；页面动作 id 统一为 `<page>.<action>`，由 `main::page_action_commands` 转 `channel::Command`；表单类回调（设置/复核/规则/授权）一一对应命令，不写业务规则；
- platform v3 已完成替换：复核队列/详情/命令/撤销、提交级排除、报表导出（`insert_export_record`）均走真实 platform/eg-report；页面 VM/通道协议未变。剩余见 §8.6 TODO（导入 persist、report 修改联动、redо 持久化、structurer、regrade、图像懒加载）。
