# M10 验收门禁脚本（骨架）

> 状态：v0.1 · 2026-11-03 · 对应 `docs/dev/acceptance.md`、`SPEC.md` §3（S1–S14）、`tasks/todo.md` T41
> 定位：把「可自动执行的验收门禁」固化为一键脚本；**只运行真实命令并汇总**，不生成/伪造数据。
> 人工/阻塞项（中文 IME、真实试卷 OCR 精度、云端 TLS、Keychain 授权框、HEIC/DOCX）见 `docs/dev/acceptance.md` §4。

## 用法

```text
scripts/acceptance/run.sh [--quick] [--json] [--out FILE] [--data-dir DIR] [--skip-egctl] [--skip-bench]
```

| 参数 | 说明 |
|---|---|
| `--quick` | 跳过全量测试（只跑 fmt/clippy/deny/docs，用于快速回归） |
| `--json` | 向 stdout 输出单对象 JSON（schema `exam-grading.acceptance/1`） |
| `--out FILE` | 额外把同一 JSON 写入文件（可放临时目录；仓库内请勿提交结果文件） |
| `--data-dir DIR` | 传给 `egctl doctor/bench` 的数据根（默认 `target/acceptance-data`，自动创建） |
| `--skip-egctl` | 跳过 egctl 自检/基准 |
| `--skip-bench` | 保留 doctor，跳过 bench |

退出码：`0` = 全 PASS；`2` = 有 WARN（降级/未就绪/todo）；`1` = 有 FAIL。

## 步骤与判定

| 步骤 id | 命令 | PASS | WARN | FAIL |
|---|---|---|---|---|
| `fmt` | `cargo fmt --all -- --check` | 已格式化 | — | 有未格式化文件 |
| `clippy` | `cargo clippy --workspace --all-targets --all-features -- -D warnings` | 0 warning | — | 有 warning/error |
| `test` | `cargo test --workspace --no-fail-fast` | 0 failed | — | 有 failed（或编译失败） |
| `deny` | `cargo deny check` | advisories/bans/licenses/sources 均 ok | — | 任一不过 |
| `docs-check` | `bash scripts/docs-check.sh` | 文档门禁通过 | — | 链接/围栏/mdBook 失败 |
| `egctl_doctor` | `egctl --json doctor` | exit 0 | exit 2（语义降级/未就绪）或 egctl 不可用 | exit 1/编译失败 |
| `egctl_bench` | `egctl --json bench` | exit 0 | exit 2（分项未实现/未接线，符合当前阶段） | exit 1/编译失败 |

说明：

- `egctl` 若不可用或未实现 `doctor/bench`，一律记 **WARN**（不阻塞骨架）；`egctl.sh` 会先探测 `--help`。
- `doctor` 使用全新 `--data-dir` 时，`config/db/models/module_*` 出现 WARN 属预期（首次启动前/并行收尾），语义为「就绪度提示」而非门禁失败。
- `bench` 中 `preprocess_page`、`judge_question` 在 capture/grade 未接入 egctl 时给 WARN（见 egctl 输出 note）。
- 结果行格式：`id \t status \t exit \t duration_ms \t detail \t log_path`；日志在 `$TMPDIR/eg-acceptance.*/`，随系统清理。

## 当前滚动基线（2026-11-03，非冻结结论）

见 `docs/dev/acceptance.md` §6「本轮实测记录」。最新快照（2026-11-03T01:16+08:00）：`fmt`、`clippy`、`test`（1063 passed / 0 failed / 2 ignored）、`deny`、`docs-check` 全 PASS；总体 **WARN（exit 2）**，WARN 仅来自 `egctl doctor/bench` 的就绪度提示（未接线/未安装模型，属预期）。**验收结论须在工作区冻结后重跑本脚本并以 `docs/dev/acceptance.md` 记录为准。**

## 文件

- `run.sh`：编排、汇总、JSON 输出、退出码。
- `gates.sh`：代码门禁子脚本（fmt/clippy/test/deny/docs-check）。
- `egctl.sh`：egctl doctor/bench 子脚本（可用性探测 + JSON 归一化）。
- `lib.sh`：临时目录/计时/TSV 输出工具。
