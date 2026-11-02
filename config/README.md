# config/ — 预设配置文件

这里放**随发行版发布、可读、不含任何秘密**的配置预设。运行时可覆盖的值仍以
`CONFIG.md` 为唯一依据；本目录的文件只是把「默认关闭的云端兜底」这份定义固化下来，
方便应用、文档与测试引用同一份事实。

| 文件 | 用途 | 默认状态 |
|---|---|---|
| `cloud.deepseek.toml` | DeepSeek 作为 `openai_compat` 云端兜底后端的预设（端点、模型、超时、授权、价格、失败策略） | **关闭**（`[cloud] enabled = false`） |

模型权重不在这里，也不在仓库里：下载清单见 `scripts/model/registry.tsv`，权重落在
用户数据目录 `~/Library/Application Support/exam-grading/models/`（见
`docs/dev/research/models.md`）。

## 1. DeepSeek 预设怎么用

### 1.1 为什么默认关闭

项目默认**全离线**（`SPEC.md` A5/A6、`CONFIG.md` §10）。云端只在教师为**单个任务**
显式授权后才可用，这是一个不变量，不是可选项：

- `[cloud] enabled = false`：总开关默认关，关闭时后端不可实例化，授权申请直接被拒；
- `[cloud] requires_task_consent = true`：即使总开关打开，每次云端调用仍必须绑定该
  任务的有效 `TaskConsent`（`SPEC-inference.md` §2.3），逐次校验；
- `[consent] ttl = "task_lifetime"`：任务到终态即失效，续跑要重新授权；
- `[consent] send_original_image = false`：默认只允许外发裁剪后的答案区域与脱敏文本，
  原图不出本机。

### 1.2 启用步骤（两步，缺一不可）

1. 在环境里提供凭据，**不要写进任何文件**：

       export DEEPSEEK_API_KEY=<你的凭据>

   凭据只从环境变量 `DEEPSEEK_API_KEY` 读取（字段 `[endpoint] credentials_env` 只存
   变量名）。应用侧按 `SPEC-platform.md` 存进系统钥匙串，日志与诊断包必须脱敏。

2. 在设置页打开全局开关（对应 `CONFIG.md` §10 `privacy.cloud_enabled`），
   然后对具体任务点「授权」并确认外发范围与预算上限 ↔ `TaskConsent`。

未完成第 2 步时，云端请求数必须为 **0**；这是 `SPEC-inference.md` §9 第 6 条的验收项。

### 1.3 字段速查

| 段 | 字段 | 值 | 说明 |
|---|---|---|---|
| `[cloud]` | `enabled` | `false` | 总开关，对应 `privacy.cloud_enabled` |
| `[cloud]` | `requires_task_consent` | `true` | 逐任务授权（不变量） |
| `[cloud]` | `protocol` | `openai_compat` | 三种后端之一，见 `SPEC-inference.md` §2.2 |
| `[endpoint]` | `base_url` | `https://api.deepseek.com` | OpenAI 兼容端点，客户端追加 `/chat/completions` |
| `[endpoint]` | `credentials_env` | `DEEPSEEK_API_KEY` | 只存环境变量名 |
| `[model]` | `name` | `deepseek-flash` | 当前文档模型 id（服务 DeepSeek-V4.1-Flash-1017） |
| `[request]` | `timeout_s` | `120` | 单次云端调用接收超时 |
| `[request]` | `max_tokens` | `1024` | 判定输出上限；远小于模型 384K 的输出上限 |
| `[request]` | `response_format` | `json_object` | 结构化输出；应用仍自行做 Schema 校验 |
| `[consent]` | `budget_micros_per_task` | `2000000` | 单任务上限，微元（=2 元） |
| `[fallback]` | `policy` | `explicit_failure` | 失败显式，不静默降级 |
| `[pricing]` | 各单价 | 2026-11-02 官方价 | 只用于费用账本 |

来源（2026-11-02 核对）：`https://api-docs.deepseek.com/` 的「首次调用 API」与
「模型 & 价格」页，以及 `GET /models` 的模型列表示例。价格会变，落地前复核。

### 1.4 失败与降级策略

| 情况 | 行为 |
|---|---|
| 总开关关闭 | 授权申请直接拒绝，请求不发出 |
| 任务无有效授权 | 请求不发出，任务标「策略拦截」，引导到授权页 |
| 授权范围外内容 | 硬拒绝 + 审计，不裁剪后「尽量发送」 |
| 预算用尽 / 授权撤销 / 任务结束 | 停止该任务后续调用，0 次外联 |
| 网络错误、5xx、超时 | 有限重试（`max_retries`）后**显式失败** |
| 输出 Schema 不合法 | 显式失败，交调用方按 `grade.max_repair_attempts` 处理 |
| 云端不可用 | 任务显式失败并提示；**不**静默切回本地、**不**静默改用其它模型 |

## 2. 保密规则（硬性）

- 仓库内**不得出现任何凭据**。本目录只有环境变量名与端点。
- 本文件不含可执行代码、不含示例密钥、不含 `.env` 内容。
- 提交前自检：

      # 期望 0 行：任何像凭据的字面量
      grep -rniE 'sk-[A-Za-z0-9_-]{8,}|(api[_-]?token|secret|password)[[:space:]]*=' config/ || true

      # 期望只命中环境变量名 DEEPSEEK_API_KEY（以及文档中对它的文字引用）
      grep -rni 'key' config/

- 应用日志、审计、诊断包一律不得包含凭据、学生姓名、学号与识别全文（`SPEC-platform.md`）。

## 3. 与 `CONFIG.md` 的对应关系

本预设不新增配置语义，只是把 `CONFIG.md` 的默认值落成一个可读文件：

| `CONFIG.md` 键 | 本文件位置 | 默认值 |
|---|---|---|
| `privacy.cloud_enabled` | `[cloud] enabled` | 关 |
| `privacy.per_task_consent` | `[cloud] requires_task_consent` | 恒开（不可配置） |
| `privacy.consent_ttl` | `[consent] ttl` | 任务生命周期 |
| `privacy.send_original_image` | `[consent] send_original_image` | 关 |
| `inference.openai_base_url` | `[endpoint] base_url` | 空（本文件给出 DeepSeek 预设） |
| `grade.judge_timeout_s` | `[request] timeout_s`（云端放宽） | 本地 20 s；云端 120 s |

## 4. 本地模型脚本（相邻的运行时配置）

本地推理仍由 `scripts/model/` 管理，与本目录无耦合：

    ./scripts/model/pull.sh --list          # 注册表与安装状态
    ./scripts/model/pull.sh                 # 下载默认 OCR + 判定模型
    ./scripts/model/verify.sh               # 大小 + SHA-256 校验
    ./scripts/model/serve.sh start ocr      # 起本地 sidecar（OpenAI 兼容）
    ./scripts/model/health.sh               # 探活，输出 OK/FAIL
