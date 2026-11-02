#!/usr/bin/env bash
# 代码门禁子脚本：格式化 / 静态检查 / 全量测试 / 依赖合规 / 文档门禁。
# 输出 TSV（见 lib.sh acc_emit）；整体不退出非 0（由 run.sh 汇总判定）。
#
# 用法：gates.sh [--quick]
#   --quick  跳过全量测试（只跑 fmt/clippy/deny/docs，用于快速回归）
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
. "$HERE/lib.sh"

QUICK=0
for arg in "$@"; do
    case "$arg" in
        --quick) QUICK=1 ;;
        *) printf '未知参数：%s\n' "$arg" >&2 ;;
    esac
done

# 单步执行并按 id 提取可读 detail（保持机读列不变）。
run_gate() {
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
    case "$id" in
        test)
            local passed failed ignored
            passed=$(grep -oE 'test result: ok\. [0-9]+ passed' "$log" | awk '{s+=$4} END{print s+0}')
            failed=$(grep -oE '[0-9]+ failed' "$log" | awk '{s+=$1} END{print s+0}')
            ignored=$(grep -oE '[0-9]+ ignored' "$log" | awk '{s+=$1} END{print s+0}')
            detail="passed=$passed failed=$failed ignored=$ignored"
            ;;
        fmt)
            if [ "$code" -eq 0 ]; then
                detail="所有文件已格式化"
            else
                detail="存在未格式化文件（见日志 diff）"
            fi
            ;;
        clippy)
            if [ "$code" -eq 0 ]; then
                detail="0 warning / 0 error"
            else
                detail="clippy 未通过（见日志）"
            fi
            ;;
        deny)
            detail=$(grep -E 'advisories |licenses |bans |sources ' "$log" | tail -n 1)
            [ -n "$detail" ] || detail=$(acc_last_line "$log")
            ;;
        docs-check)
            detail=$(grep -E '文档门禁' "$log" | tail -n 1)
            [ -n "$detail" ] || detail=$(acc_last_line "$log")
            ;;
        *)
            detail="exit=$code"
            ;;
    esac
    acc_emit "$id" "$status" "$code" "$dur" "$detail" "$log"
}

run_gate fmt cargo fmt --all -- --check
run_gate clippy cargo clippy --workspace --all-targets --all-features -- -D warnings
if [ "$QUICK" -eq 0 ]; then
    run_gate test cargo test --workspace --no-fail-fast
else
    acc_emit test skip 0 0 "quick 模式跳过全量测试" ""
fi
run_gate deny cargo deny check
run_gate docs-check bash "$ACC_ROOT/scripts/docs-check.sh"
