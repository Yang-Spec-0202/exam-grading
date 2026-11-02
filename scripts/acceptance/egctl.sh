#!/usr/bin/env bash
# egctl 子脚本：doctor（环境/模型/DB 自检）与 bench（性能基准）。
# 未实现/不可用一律 WARN（不阻塞验收骨架）；egctl 返回 2 视为 WARN（语义降级）。
#
# 用法：egctl.sh [--skip-bench] [--data-dir DIR]
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
. "$HERE/lib.sh"

SKIP_BENCH=0
ACC_DATA_DIR="${ACC_DATA_DIR:-$ACC_ROOT/target/acceptance-data}"
while [ "$#" -gt 0 ]; do
    case "$1" in
        --skip-bench) SKIP_BENCH=1 ;;
        --data-dir)
            [ "$#" -ge 2 ] || { printf '参数 --data-dir 需要一个目录\n' >&2; exit 1; }
            ACC_DATA_DIR="$2"
            shift
            ;;
        --data-dir=*) ACC_DATA_DIR="${1#--data-dir=}" ;;
        *) printf '未知参数：%s\n' "$1" >&2 ;;
    esac
    shift
done
mkdir -p "$ACC_DATA_DIR"

EGCTL=(cargo run -q -p egctl --)

# 探测 CLI 是否可用且含子命令；失败/缺失一律 WARN。
probe_egctl() {
    local log="$ACC_TMP/egctl-help.log"
    "${EGCTL[@]}" --help >"$log" 2>&1
    local code=$?
    if [ "$code" -ne 0 ]; then
        acc_emit egctl_help warn "$code" 0 "egctl 不可用（exit=${code}，并行开发中）；详见日志" "$log"
        return 1
    fi
    if ! grep -q 'doctor' "$log"; then
        acc_emit egctl_help warn 0 0 "egctl 未实现子命令（当前版本仅打印版本号）" "$log"
        return 1
    fi
    acc_emit egctl_help pass 0 0 "doctor/bench 子命令可用" "$log"
    return 0
}

# 运行 egctl JSON 子命令并归一化状态：exit 0=pass，2=warn，其它=fail。
run_egctl_json() {
    local id="$1"
    shift
    local log="$ACC_TMP/$id.log"
    local start end code dur status detail
    start=$(acc_now_ms)
    "${EGCTL[@]}" --json --data-dir "$ACC_DATA_DIR" "$@" >"$log" 2>&1
    code=$?
    end=$(acc_now_ms)
    dur=$((end - start))
    if [ "$code" -eq 0 ]; then
        status=pass
    elif [ "$code" -eq 2 ]; then
        status=warn
    else
        status=fail
    fi
    detail=$(python3 - "$log" <<'PY'
import json, sys
path = sys.argv[1]
try:
    doc = json.load(open(path, encoding="utf-8"))
except Exception as error:
    print(f"JSON 解析失败：{error}")
    raise SystemExit(0)
if "checks" in doc:
    counts = {}
    for check in doc.get("checks", []):
        key = check.get("status", "?")
        counts[key] = counts.get(key, 0) + 1
    print(f"status={doc.get('status', '?')} " + " ".join(f"{k}={v}" for k, v in sorted(counts.items())))
elif "items" in doc:
    counts = {}
    for item in doc.get("items", []):
        key = item.get("status", "?")
        counts[key] = counts.get(key, 0) + 1
    print(f"status={doc.get('status', '?')} " + " ".join(f"{k}={v}" for k, v in sorted(counts.items())))
else:
    print(f"status={doc.get('status', '?')}")
PY
)
    [ -n "$detail" ] || detail="exit=$code"
    acc_emit "$id" "$status" "$code" "$dur" "$detail" "$log"
}

if probe_egctl; then
    run_egctl_json egctl_doctor doctor
    if [ "$SKIP_BENCH" -eq 0 ]; then
        run_egctl_json egctl_bench bench
    else
        acc_emit egctl_bench skip 0 0 "命令行 --skip-bench 跳过" ""
    fi
else
    acc_emit egctl_doctor warn 0 0 "egctl 不可用，跳过（并行开发中）" ""
    acc_emit egctl_bench warn 0 0 "egctl 不可用，跳过（并行开发中）" ""
fi
