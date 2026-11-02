#!/usr/bin/env bash
# M10 验收脚本共享工具（被 run.sh / gates.sh / egctl.sh source）。
# 只负责：临时目录、毫秒计时、TSV 行输出；不做任何业务判断。
# 退出码约定（与 egctl/CI 一致）：0 = PASS，2 = WARN（降级/未就绪），1 = FAIL。

set -uo pipefail

if [ -z "${ACC_ROOT:-}" ]; then
    printf '内部错误：ACC_ROOT 未设置（必须由 run.sh 设置）\n' >&2
    exit 1
fi

ACC_TMP="${ACC_TMP:-$(mktemp -d "${TMPDIR:-/tmp}/eg-acceptance.XXXXXX")}"
export ACC_TMP ACC_ROOT

# 稳健性：部分环境（含本机新 shell）PATH 不含 ~/.cargo/bin，自动补常见安装位置。
if ! command -v cargo >/dev/null 2>&1; then
    for candidate in "$HOME/.cargo/bin" "/opt/homebrew/bin" "/usr/local/bin"; do
        if [ -x "$candidate/cargo" ]; then
            PATH="$candidate:$PATH"
            export PATH
            break
        fi
    done
fi
if ! command -v cargo >/dev/null 2>&1; then
    printf '错误：找不到 cargo。请先安装 Rust 工具链（见 docs/dev/research/toolchain.md）。\n' >&2
    exit 1
fi

# 毫秒时间戳（bash 3.2 无 EPOCHREALTIME，统一用 python3）。
acc_now_ms() {
    python3 -c 'import time; print(int(time.time() * 1000))'
}

# 输出一行机读结果：id \t status \t exit \t duration_ms \t detail \t log_path
acc_emit() {
    printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$4" "$5" "$6"
}

# 通用单步执行（固定 detail：exit + 日志文件名）。用法：acc_run <id> <cmd...>
acc_run() {
    local id="$1"
    shift
    local log="$ACC_TMP/$id.log"
    local start end code dur status detail
    start=$(acc_now_ms)
    "$@" >"$log" 2>&1
    code=$?
    end=$(acc_now_ms)
    dur=$((end - start))
    if [ "$code" -eq 0 ]; then
        status=pass
    else
        status=fail
    fi
    detail="exit=${code}；日志 $(basename "$log")"
    acc_emit "$id" "$status" "$code" "$dur" "$detail" "$log"
}

# 截取日志最后一行非空文本（用于 WARN/FAIL 的 detail 摘要）。
acc_last_line() {
    local file="$1"
    tail -n 20 "$file" 2>/dev/null | sed -n '/[^[:space:]]/p' | tail -n 1
}
