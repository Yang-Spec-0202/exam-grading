#!/usr/bin/env bash
# M10 验收门禁主脚本：汇总代码门禁 + egctl 自检/基准，输出人读表格与机读 JSON。
#
# 用法：
#   scripts/acceptance/run.sh [--quick] [--json] [--out FILE] [--data-dir DIR] [--skip-egctl] [--skip-bench]
#
# 退出码：0 = 全 PASS；2 = 有 WARN（降级/未就绪）；1 = 有 FAIL。
#
# 说明：本脚本只做“真实执行 + 汇总”，不生成/伪造任何验收数据；未实现命令记 WARN。
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ACC_ROOT="$(cd "$HERE/../.." && pwd)"
export ACC_ROOT
# shellcheck source=lib.sh
. "$HERE/lib.sh"

QUICK=0
JSON=0
OUT=""
SKIP_EGCTL=0
SKIP_BENCH=0
ACC_DATA_DIR="${ACC_DATA_DIR:-$ACC_ROOT/target/acceptance-data}"

usage() {
    sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//'
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --quick) QUICK=1 ;;
        --json) JSON=1 ;;
        --out)
            [ "$#" -ge 2 ] || { printf '参数 --out 需要一个文件路径\n' >&2; exit 1; }
            OUT="$2"
            shift
            ;;
        --out=*) OUT="${1#--out=}" ;;
        --data-dir)
            [ "$#" -ge 2 ] || { printf '参数 --data-dir 需要一个目录\n' >&2; exit 1; }
            ACC_DATA_DIR="$2"
            shift
            ;;
        --data-dir=*) ACC_DATA_DIR="${1#--data-dir=}" ;;
        --skip-egctl) SKIP_EGCTL=1 ;;
        --skip-bench) SKIP_BENCH=1 ;;
        --help|-h) usage; exit 0 ;;
        *) printf '未知参数：%s（--help 查看用法）\n' "$1" >&2; exit 1 ;;
    esac
    shift
done
export ACC_DATA_DIR

START_MS=$(acc_now_ms)
STEPS="$ACC_TMP/steps.tsv"
: >"$STEPS"

if [ "$QUICK" -eq 1 ]; then
    "$HERE/gates.sh" --quick >>"$STEPS"
else
    "$HERE/gates.sh" >>"$STEPS"
fi
if [ "$SKIP_EGCTL" -eq 0 ]; then
    if [ "$SKIP_BENCH" -eq 1 ]; then
        "$HERE/egctl.sh" --skip-bench --data-dir "$ACC_DATA_DIR" >>"$STEPS"
    else
        "$HERE/egctl.sh" --data-dir "$ACC_DATA_DIR" >>"$STEPS"
    fi
else
    acc_emit egctl_doctor skip 0 0 "命令行 --skip-egctl 跳过" "" >>"$STEPS"
    acc_emit egctl_bench skip 0 0 "命令行 --skip-egctl 跳过" "" >>"$STEPS"
fi
END_MS=$(acc_now_ms)
ELAPSED_MS=$((END_MS - START_MS))

python3 - "$STEPS" "$ELAPSED_MS" "$JSON" "$OUT" <<'PY'
import datetime
import json
import os
import platform
import subprocess
import sys

steps_path, elapsed_ms, as_json, out_path = sys.argv[1], int(sys.argv[2]), sys.argv[3], sys.argv[4]
steps = []
with open(steps_path, encoding="utf-8") as handle:
    for line in handle:
        line = line.rstrip("\n")
        if not line:
            continue
        parts = line.split("\t")
        while len(parts) < 6:
            parts.append("")
        steps.append({
            "id": parts[0],
            "status": parts[1],
            "exit": int(parts[2] or 0),
            "duration_ms": int(parts[3] or 0),
            "detail": parts[4],
            "log": parts[5],
        })

counts = {"pass": 0, "warn": 0, "fail": 0, "skip": 0}
for step in steps:
    counts[step["status"]] = counts.get(step["status"], 0) + 1
if counts.get("fail", 0) > 0:
    overall, exit_code = "fail", 1
elif counts.get("warn", 0) > 0:
    overall, exit_code = "warn", 2
else:
    overall, exit_code = "pass", 0

def cmd_version(cmd):
    try:
        return subprocess.run(cmd, capture_output=True, text=True, timeout=10).stdout.strip().splitlines()[0]
    except Exception:
        return "unknown"

result = {
    "schema": "exam-grading.acceptance/1",
    "generated_at": datetime.datetime.now().astimezone().isoformat(timespec="seconds"),
    "root": os.environ.get("ACC_ROOT", "."),
    "elapsed_ms": elapsed_ms,
    "environment": {
        "platform": platform.platform(),
        "machine": platform.machine(),
        "rustc": cmd_version(["rustc", "--version"]),
        "cargo": cmd_version(["cargo", "--version"]),
    },
    "overall": overall,
    "counts": counts,
    "steps": steps,
}

if as_json == "1" or out_path:
    text = json.dumps(result, ensure_ascii=False, indent=2)
    if out_path:
        with open(out_path, "w", encoding="utf-8") as handle:
            handle.write(text + "\n")
    if as_json == "1":
        print(text)
else:
    print(f"验收门禁摘要（{result['generated_at']}，耗时 {elapsed_ms} ms）")
    print(f"{'ID':<18}{'STATUS':<8}{'EXIT':<6}{'MS':<8}DETAIL")
    for step in steps:
        print(f"{step['id']:<18}{step['status']:<8}{step['exit']:<6}{step['duration_ms']:<8}{step['detail']}")
    print()
    print(f"总体：{overall.upper()}（pass={counts.get('pass', 0)} warn={counts.get('warn', 0)} fail={counts.get('fail', 0)} skip={counts.get('skip', 0)}）")
    if counts.get("fail", 0) > 0:
        print("提示：FAIL 项需修复；若因并行开发未冻结导致，请在冻结后重跑并写入 docs/dev/acceptance.md 实测记录。")
sys.exit(exit_code)
PY
