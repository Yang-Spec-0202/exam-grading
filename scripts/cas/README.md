# CAS sidecar（sympy 1.14.0 + uv 托管 Python 3.12）

`SPEC-subject-math.md` §7 / T27 的 CAS 适配层。仅用于**判定等价**（三角恒等、向量/矩阵
符号分量、复杂代数变形），不生成答案、不解方程、不发送学生个人信息。

研究依据：`docs/dev/research/cas.md`（tech-spike 实测：Python 3.12.15、sympy 1.14.0、
启动 0.24 s、单次化简 0.43–4.5 ms）。

## 安装（一次性）

```bash
# uv（单二进制；Homebrew: brew install uv）
uv --version

# 托管 Python 3.12（不动系统 Python；实测 9.37 s）
uv python install 3.12

# 只创建 venv 并安装锁定依赖（sympy==1.14.0；实测 7.08 s）
cd scripts/cas
uv sync
```

## 运行

`crates/eg-subject-math` 默认按环境变量探测 sidecar（未配置则 CAS 不可用 → 相关
比较器返回「无法判定」）：

```bash
export EG_CAS_SCRIPT="$PWD/scripts/cas/sidecar.py"
export EG_CAS_PYTHON="$PWD/scripts/cas/.venv/bin/python"
export EG_CAS_TIMEOUT_MS=3000   # 1–10 s，默认 3000
# EG_CAS_DISABLED=1 可强制关闭；EG_CAS_CACHE=0 可关闭结果缓存
```

也可以直接手工冒烟：

```bash
printf '%s\n' \
  '{"op":"ping"}' \
  '{"op":"simplify_zero","lhs":"sin(x)**2+cos(x)**2","rhs":"1"}' \
  '{"op":"matrix_det","rows":[["1","2"],["3","4"]]}' \
  | "$EG_CAS_PYTHON" scripts/cas/sidecar.py
```

## 协议（JSON-lines，protocol=1）

| 请求 | 响应 |
|---|---|
| `{"op":"ping"}` | `{"ok":true,"sympy":"1.14.0","protocol":"1"}` |
| `{"op":"simplify_zero","lhs":"...","rhs":"..."}` | `{"ok":true,"zero":bool,"constant":bool,"diff":"...","sympy":"1.14.0"}` |
| `{"op":"matrix_det","rows":[["1","2"],["3","4"]]}` | `{"ok":true,"det":"-2","sympy":"1.14.0"}` |
| 解析/计算失败 | `{"ok":false,"error":"原因","sympy":"1.14.0"}` |

Rust 侧（`crates/eg-subject-math/src/cas.rs`）对每次请求：

- 启动独立子进程（请求级隔离），写入一行请求、读取一行响应；
- 默认 3 s 超时，超时 `kill` 并降级为「无法判定」；
- 禁用/缺脚本/启动失败/崩溃/非法 JSON/表达式过长 → 全部降级「无法判定」；
- 结果缓存键 = 表达式规范形 + sympy 版本 + 协议版本；
- CAS 版本与命令哈希进 `EngineStamp`/引擎指纹（`MathSubjectPack::cas_engine_stamp`）。

## 安全

- 输入只允许 `[A-Za-z0-9_+\-*/().,\s^]`，长度 ≤1000，拒绝 `__`；
- 标识符白名单（sin/cos/tan/cot/sec/csc/log/ln/exp/sqrt/root/Abs/pi + 单字母变量）；
- 固定脚本入口，不接受任意代码/文件路径参数；sidecar 不读学生信息（请求只含表达式）。

## 测试

不联网即可跑：集成测试使用 `scripts/cas/stub_sidecar.py` 注入正常/超时/崩溃/非法响应；
真实 sympy 测试仅在设置 `EG_CAS_SCRIPT` 与 `EG_CAS_PYTHON` 时执行，否则跳过并打印提示。
