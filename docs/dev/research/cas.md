# 研究记录：CAS sidecar（sympy + uv / Python 3.12）

> 状态：已完成 · 2026-11-02（Asia/Shanghai）· 对应规范：`SPEC-subject-math.md`（CAS 适配层：独立子进程、启动超时 10 s、单次 ≤3 s、失败降级「无法判定」）、`SPEC.md` §4（符号计算行）、`AGENTS.md` §2/§4
> 实测机器：Apple M4 / 16 GB / macOS 27.0.1 / 系统 python3 **3.9.6**（不能用）/ `uv 0.13.0`
> 结论用于：`subject-math` 的 sidecar 形态、Python 获取方式、协议草案、超时与降级实现

## 1. 模块与范围

- 模块 id：`subject-math`（CAS 适配层，`SPEC-subject-math.md` §96–106）
- 本次要实现的能力：用 sympy 判定「两表达式差是否为 0」（三角恒等/代数等价）、矩阵行列式、线性方程解集；JSON-lines 子进程协议；启动与超时预算
- 明确不做：完整符号计算能力矩阵、微分方程/级数、符号计算性能调优、Symbolica（规范已排除）

## 2. 问题清单

| # | 需要确认的外部行为/API/版本/限制 | 影响 |
|---|---|---|
| Q1 | 系统只有 Python 3.9.6，sympy 1.14 需要 ≥3.9 但规范要求 3.12；如何稳定获取 Python 3.12？ | 决定部署方式（不能要求用户手装） |
| Q2 | `uv` 的实际版本、离线/在线行为、venv 与包安装耗时？ | 决定首启动体验与 CI |
| Q3 | sympy 当前版本与许可？ | 决定白名单与版本锁定 |
| Q4 | sympy 能否覆盖规范要求的比较器（三角恒等为 0、矩阵、方程解）？单次耗时是否 ≤3 s？ | 决定 CAS 是否可用 |
| Q5 | sidecar 协议（stdin/stdout JSON 行）是否可行？启动到可用的时间？ | 决定 `subject-math` 集成方式 |
| Q6 | 超时/崩溃如何降级？ | 决定 `SPEC-subject-math.md` 的「无法判定」路径 |

## 3. 来源

| # | URL / 命令 | 版本/日期 | 类型 | 取用内容 |
|---|---|---|---|---|
| S1 | `uv --version`（本机） | 2026-11-02：**uv 0.13.0**（Homebrew 2026-10-09，aarch64-apple-darwin） | 命令输出 | 实际版本 |
| S2 | https://docs.astral.sh/uv/ | 2026-11 查阅 | 官方文档 | `uv python install <ver>` 下载托管 CPython；`uv venv` / `uv pip install` 语义 |
| S3 | `uv python install 3.12`（本机） | 2026-11-02 | 命令输出 | 下载 `cpython-3.12.15-macos-aarch64-none`，**9.37 s** 安装完成 |
| S4 | `uv venv --python 3.12` + `uv pip install sympy`（本机） | 2026-11-02 | 命令输出 | CPython **3.12.15**；sympy **1.14.0** + mpmath 1.3.0；准备 3.59 s、安装 16 ms、总 7.08 s |
| S5 | https://crates.io 无关；`importlib.metadata.metadata("sympy")["License"]`（本机） | sympy 1.14.0 | 命令输出 | `License: BSD`（BSD-3-Clause 系，白名单内） |
| S6 | https://www.sympy.org/ · https://docs.sympy.org/latest/ | 1.14 文档（2026-11 查阅） | 官方文档 | `simplify`、`Matrix.det()`、`solve`、`parse_expr` 与 `implicit_multiplication_application` 变换 |
| S7 | 本机 sidecar 冒烟（`/tmp/cas-spike`，验证后删除） | 2026-11-02 | 命令输出 | 协议、耗时、正确性（§5） |
| S8 | `SPEC-subject-math.md` §96–106、§135、§145、§167 | 规范 v0.2.2 | 内部规范 | sidecar 形态、10 s 启动、≤3 s 单次、失败降级「无法判定」 |

## 4. 结论

| 问题 | 结论 | 依据 |
|---|---|---|
| Q1 | **GO：用 `uv` 托管 Python，不依赖系统 Python**。`uv python install 3.12` 一条命令装出 **CPython 3.12.15**（9.37 s），不与系统 3.9.6 冲突；`uv venv --python 3.12` + `uv pip install sympy` 7.08 s 完成。部署只需 `uv`（单二进制）或首启由 `scripts/` 引导。 | S1–S4 |
| Q2 | `uv 0.13.0`（Homebrew 已装）。`uv python install` 走官方 python-build-standalone 下载；`uv pip install` 有本地缓存（本机 16 ms 命中）。CI 可缓存 `~/.cache/uv` 与 `.venv`。 | S1–S4 |
| Q3 | 锁定 **`sympy == 1.14.0`**（+ `mpmath 1.3.0` 传递），许可 **BSD**（BSD-3-Clause 系）→ 符合 `SPEC.md` §10 白名单，无需新审批。 | S4 + S5 |
| Q4 | sympy 覆盖规范要求的比较器：`sin²x+cos²x-1 → 0`、`tan(x)cos(x)-sin(x) → 0`、`(x+1)²-(x²+2x+1) → 0` 全部判定为 0；`Matrix([[1,2],[3,4]]).det() = -2`；`2x+3=7 → [2]`。**单次耗时 0.43–4.5 ms**（首请求 98.9 ms 含 import/缓存预热），远低于规范 ≤3 s。 | S6 + S7 |
| Q5 | JSON-lines sidecar 可行：`stdin` 每行一个请求、`stdout` 每行一个响应（`flush`），进程从启动到处理 6 个请求总耗时 **0.24 s（real）**，远低于规范启动超时 10 s。 → 常驻进程 + 每请求超时（Python 侧不可中断时由 Rust 侧 `kill` 重启）即可。 | S7 + S8 |
| Q6 | 降级路径可实现：Rust 侧对每次请求设 deadline（≤3 s）；超时/进程退出/异常响应 → 该比较器返回「无法判定」（规范已要求，不得猜）。sympy 自身对恶意/超大表达式可能长时间运行，**必须**由 Rust 侧强制超时 + 进程重启，不能只靠 Python 内部超时。 | S7 + S8 |

**go/no-go：GO（方案可行、许可干净、性能余量充足）**，但需在实现阶段补：① 表达式大小上限与解析白名单；② 超时强杀与重启；③ `sympy` 版本锁定 + 结果缓存键（`SPEC-subject-math.md` §135）。

## 5. 版本实测

### 5.1 安装链

| 步骤 | 命令 | 实测 |
|---|---|---|
| uv | `uv --version` | `uv 0.13.0 (Homebrew 2026-10-09 aarch64-apple-darwin)` |
| Python 3.12 | `uv python install 3.12` | `Installed Python 3.12.15 in 9.37s`（`cpython-3.12.15-macos-aarch64-none`） |
| venv | `uv venv --python 3.12 .venv` | `Using CPython 3.12.15` |
| sympy | `uv pip install --python .venv/bin/python sympy` | `sympy==1.14.0`、`mpmath==1.3.0`；Prepared 3.59 s、Installed 16 ms、总 **7.08 s** |
| 版本确认 | `./.venv/bin/python -VV` | `Python 3.12.15 (main, Oct 9 2026) [Clang 22.1.3]` |
| 许可 | `importlib.metadata.metadata("sympy")["License"]` | `BSD` |

### 5.2 sidecar 协议冒烟（JSON-lines）

请求/响应（每行一个 JSON）：

```
{"op":"ping"}                                                → {"ok":true,"sympy":"1.14.0"}
{"op":"simplify_zero","lhs":"sin(x)**2+cos(x)**2","rhs":"1"} → {"ok":true,"zero":true,"diff":"0","ms":98.85}
{"op":"simplify_zero","lhs":"tan(x)*cos(x)","rhs":"sin(x)"}  → {"ok":true,"zero":true,"diff":"0","ms":4.50}
{"op":"matrix_det","rows":[["1","2"],["3","4"]]}             → {"ok":true,"det":"-2","ms":0.43}
{"op":"solve_linear","lhs":"2*x+3","rhs":"7"}                → {"ok":true,"solutions":["2"],"ms":2.08}
{"op":"simplify_zero","lhs":"(x+1)**2","rhs":"x**2+2*x+1"}   → {"ok":true,"zero":true,"diff":"0","ms":2.75}
```

进程级：`/usr/bin/time -p` → `real 0.24 s`（含解释器启动、`import sympy`、6 次请求）。
错误路径：未实现 op 或解析失败返回 `{"ok":false,"error":...}`（测试脚本已含 try/except，`max_tokens` 类截断不适用）。

### 5.3 与规范预算对照

| 规范项（`SPEC-subject-math.md`） | 规范值 | 实测 | 结论 |
|---|---|---|---|
| sidecar 启动超时 | 10 s | 0.24 s（含 import）+ venv 首次创建另计（7 s，一次性） | ✅ 余量充足 |
| 单次化简 | ≤3 s | 0.43–4.5 ms（首请求 98.9 ms） | ✅ |
| 失败降级 | 无法判定 | 协议层可判（`ok:false` / 超时 / 进程退出） | ✅ 可实现 |
| 结果缓存键 | 规范化表达式 + CAS 版本 | 可用 `sympy 1.14.0` 作为版本段 | ✅ |

## 6. 风险与限制

| 风险 | 影响 | 缓解/预案 |
|---|---|---|
| **R1：sympy 对超大/恶意表达式可能长时间运行或内存爆炸** | 超时不可控（Python 无法安全中断 C/纯 Python 长计算） | Rust 侧强制 per-request deadline（≤3 s）+ 超时后 `kill` 进程并重启；限制输入长度/节点数；**绝不**在 Rust 侧 `await` 无超时的子进程 |
| **R2：`parse_expr` 内部使用 `eval` 类机制** | 若把教师输入直接交给 sympy 有代码执行面（本地单机、威胁模型低，但不合规） | 限制 `local_dict`（只暴露允许符号）、禁用 `eval` 危险变换、输入白名单正则（数字/运算符/允许函数名）；sidecar 以最小权限运行 |
| **R3：首启动 venv 创建 7 s / 首次 `uv python install` 9.4 s** | 首次使用体验（`SPEC-subject-math.md` 启动 10 s 只覆盖进程启动，未覆盖环境准备） | 由 `scripts/setup` 预装（M0/M1 阶段一次性），运行时只起进程；首启仍可用「CAS 暂不可用 → 无法判定」降级 |
| R4：Python 生态额外维护面（Python 版本/包升级、Apple Silicon wheel 可用性） | 长期维护成本 | 锁定 `sympy==1.14.0`、`python 3.12.15`；venv 记入 `provenance`（版本+哈希）；不引入其它 pip 包 |
| R5：结果缓存（规范 §135）跨 CAS 升级失效 | 缓存键需含版本 | 缓存键 = 规范化表达式 + `sympy.__version__` + 协议版本 |
| R6：本次未测压力/并发（多任务同时 CAS） | 吞吐未知 | 单进程串行 + 请求队列（`SPEC-inference` 的阻塞式模型一致）；并发需求出现再加进程池（`Ask first`） |

## 7. 与规范的偏差

| 规范位置 | 偏差 | 处理（改规范/改实现） | 审批状态 |
|---|---|---|---|
| `SPEC-subject-math.md` §96–106「sympy sidecar（需 Python 3.12）」 | 无偏差：uv 可稳定提供 3.12.15；但**部署依赖 `uv`/托管 Python**（系统 python 3.9.6 不可用），规范未写获取方式 | 不改规范；在 `scripts/` 增加 setup 说明（M1 实现） | — |
| `SPEC-subject-math.md` §100「启动超时 10s」 | 进程启动实测 0.24 s；但**环境首次准备**（uv 安装 Python + venv + sympy）约 16.5 s，不在启动预算内 | 规范无需改；把「环境准备」列为安装步骤（一次性） | — |
| `SPEC.md` §10 白名单 | sympy BSD-3-Clause 系、mpmath BSD → 无需新增白名单 | 不改 | — |

## 8. 复现命令

```bash
export PATH="/opt/homebrew/bin:$PATH"          # uv 0.13.0
uv python install 3.12                          # Installed Python 3.12.15 in 9.37s
uv venv --python 3.12 .venv
uv pip install --python .venv/bin/python sympy  # sympy==1.14.0 + mpmath==1.3.0，7.08 s
./.venv/bin/python -VV                          # Python 3.12.15
./.venv/bin/python -c "import sympy; print(sympy.__version__)"   # 1.14.0

# sidecar 协议冒烟（/tmp/cas-spike/sidecar.py，验证后删除）
/usr/bin/time -p ./.venv/bin/python sidecar.py <<'JSON'
{"op":"ping"}
{"op":"simplify_zero","lhs":"sin(x)**2+cos(x)**2","rhs":"1"}
{"op":"simplify_zero","lhs":"(x+1)**2","rhs":"x**2+2*x+1"}
{"op":"matrix_det","rows":[["1","2"],["3","4"]]}
{"op":"solve_linear","lhs":"2*x+3","rhs":"7"}
JSON
# → 每行 {"ok":true,...}；real 0.24 s
```
