#!/usr/bin/env bash
#
# health.sh — probe the local llama.cpp sidecar(s) and report OK / FAIL.
#
# Checks, per model: pid from the run directory, GET /health, GET /v1/models,
# and the resident memory of the serving process. Read-only; starts nothing.
#
# usage: ./health.sh [options] [MODEL|ROLE ...]
#   no argument   probe the default OCR model and the default judge model
#   ocr | judge   the configured default model for that role
#   <registry id> that exact model
# options:
#   --json        machine-readable output on stdout
#   --quiet       one line per model, no table header
#   -h, --help    this text
#
# exit codes: 0 = every probed sidecar is OK, 1 = at least one is not.

set -euo pipefail

# shellcheck source=lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/lib.sh"

JSON=0
QUIET=0
WANTED=""

usage() {
  cat >&2 <<'USAGE'
health.sh - probe the local llama.cpp sidecars

usage: ./health.sh [options] [MODEL|ROLE ...]

  no argument   probe the default OCR model and the default judge model
  ocr | judge   the configured default model for that role
  <registry id> that exact model

options:
  --json        machine-readable output on stdout
  --quiet       one line per model, no table header
  -h, --help    this text
USAGE
}

while [ $# -gt 0 ]; do
  case "$1" in
    --json)    JSON=1 ;;
    --quiet)   QUIET=1 ;;
    -h|--help) usage; exit 0 ;;
    --)        shift; while [ $# -gt 0 ]; do WANTED="$WANTED $1"; shift; done; break ;;
    -*)        err "unknown option: $1"; usage; exit 1 ;;
    *)         WANTED="$WANTED $1" ;;
  esac
  shift
done

require_cmd curl

model_list=""
for w in $WANTED; do
  mid=$(resolve_model "$w") || exit 1
  case " $model_list " in *" $mid "*) : ;; *) model_list="$model_list $mid" ;; esac
done
if [ -z "$model_list" ]; then
  for w in ocr judge; do
    mid=$(resolve_model "$w") || exit 1
    case " $model_list " in *" $mid "*) : ;; *) model_list="$model_list $mid" ;; esac
  done
fi

# probe <url> -- prints "<http_code> <time_total_seconds>"
probe() {
  local out
  out=$(curl -sS -o /dev/null -w '%{http_code} %{time_total}' -m "$HEALTH_TIMEOUT_S" "$1" 2>/dev/null) || true
  [ -n "$out" ] || out="000 0"
  printf '%s\n' "$out"
}

# rss_mb <pid> -- resident set size in MiB (0 when unavailable)
rss_mb() {
  local pid="$1" kb
  [ -n "$pid" ] || { printf '0\n'; return 0; }
  kb=$(ps -o rss= -p "$pid" 2>/dev/null | tr -d ' ' || true)
  [ -n "$kb" ] || kb=0
  awk -v kb="$kb" 'BEGIN { printf "%.0f", kb / 1024 }'
}

json_parts=""
fail_count=0
[ "$JSON" -eq 1 ] || { [ "$QUIET" -eq 1 ] || printf '%-22s %-6s %-4s %-6s %-5s %-6s %-7s %-7s %s\n' MODEL ROLE STATE PID PORT HEALTH MS RSS_MB DETAIL; }

for id in $model_list; do
  role=$(role_of_model "$id")
  port=$(model_port "$id")
  log=$(model_logfile "$id")
  pid=""
  if pid=$(read_pidfile "$(model_pidfile "$id")" 2>/dev/null); then :; else pid=""; fi
  alive=0
  if pid_alive "$pid"; then alive=1; fi

  state="DOWN"
  detail=""
  health_code="000"
  health_ms="0"
  models_ok=0
  rss="0"

  if [ "$alive" -eq 1 ]; then
    set -- $(probe "http://127.0.0.1:$port/health")
    health_code="$1"
    health_ms=$(awk -v t="${2:-0}" 'BEGIN { printf "%.1f", t * 1000 }')
    rss=$(rss_mb "$pid")
    set -- $(probe "http://127.0.0.1:$port/v1/models")
    [ "$1" = "200" ] && models_ok=1

    if [ "$health_code" = "200" ] && [ "$models_ok" -eq 1 ]; then
      state="OK"
    else
      state="FAIL"
      detail="health=$health_code v1/models=$([ "$models_ok" -eq 1 ] && echo ok || echo fail)"
    fi
  else
    state="DOWN"
    detail="not running (./serve.sh start $id)"
  fi

  [ "$state" = "OK" ] || fail_count=$((fail_count + 1))

  if [ "$JSON" -eq 1 ]; then
    json_parts="${json_parts}${json_parts:+,}{\"id\":\"$id\",\"role\":\"$role\",\"state\":\"$state\",\"pid\":\"${pid}\",\"port\":\"$port\",\"health_code\":\"$health_code\",\"health_ms\":$health_ms,\"rss_mb\":$rss}"
  else
    printf '%-22s %-6s %-4s %-6s %-5s %-6s %-7s %-7s %s\n' \
      "$id" "$role" "$state" "${pid:--}" "${port:--}" "$health_code" "$health_ms" "$rss" \
      "${detail:-base url http://127.0.0.1:$port/v1  log $log}"
  fi
done

if [ "$JSON" -eq 1 ]; then
  status="OK"
  [ "$fail_count" -eq 0 ] || status="FAIL"
  printf '{"status":"%s","failures":%s,"services":[%s]}\n' "$status" "$fail_count" "$json_parts"
else
  [ "$QUIET" -eq 1 ] || printf '\n'
  if [ "$fail_count" -eq 0 ]; then
    printf 'health: OK\n'
  else
    printf 'health: FAIL (%s service(s))\n' "$fail_count"
  fi
fi

[ "$fail_count" -eq 0 ] || exit 1
exit 0
