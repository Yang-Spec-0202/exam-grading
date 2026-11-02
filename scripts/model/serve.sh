#!/usr/bin/env bash
#
# serve.sh — start / stop / inspect the local llama.cpp OpenAI-compatible sidecar.
#
# Each registry model gets its own loopback llama-server process, port and log
# (SPEC-inference.md §4: local sidecar lifecycle). Serving parameters come from
# scripts/model/profiles.tsv; weights must already be installed by pull.sh.
#
# usage: ./serve.sh <command> [MODEL|ROLE]
#   commands:
#     start   [ocr|judge|id]   start a sidecar (default: ocr)
#     stop    [ocr|judge|id|all]
#     restart [ocr|judge|id]   stop then start
#     status  [ocr|judge|id|all]
#     logs    [ocr|judge|id]   tail -f the sidecar log
#   default target is 'ocr'; 'stop'/'status' without a target act on every known model.
#
# environment overrides: PORT, CTX, TEMP, STARTUP_TIMEOUT_S, STOP_TIMEOUT_S,
#                        LLAMA_SERVER, MODELS_DIR, MODEL_LOG_DIR, MODEL_RUN_DIR

set -euo pipefail

# shellcheck source=lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/lib.sh"

: "${LLAMA_SERVER:=llama-server}"
: "${HOST:=127.0.0.1}"      # loopback only: never expose inference to the LAN

usage() {
  cat >&2 <<'USAGE'
serve.sh - manage the local llama.cpp sidecar(s)

usage: ./serve.sh <command> [MODEL|ROLE]

  start   [ocr|judge|id]   start a sidecar (default: ocr)
  stop    [ocr|judge|id|all]
  restart [ocr|judge|id]
  status  [ocr|judge|id|all]
  logs    [ocr|judge|id]   tail -f the sidecar log

  default target is 'ocr'
  'stop' and 'status' without a target act on every known model
USAGE
}

cmd="${1:-}"
[ -n "$cmd" ] || { usage; exit 1; }
shift || true
target="${1:-}"

# all known model ids, registry order
all_ids() { registry_model_ids; }

# expand a target: '' -> ocr for start/restart/logs, all for stop/status
expand_targets() {
  local t="$1" default_mode="$2" id
  if [ -n "$t" ] && [ "$t" != "all" ]; then
    id=$(resolve_model "$t") || exit 1
    printf '%s\n' "$id"
    return 0
  fi
  if [ "$t" = "all" ] || [ "$default_mode" = "all" ]; then
    all_ids
    return 0
  fi
  id=$(resolve_model "ocr") || exit 1
  printf '%s\n' "$id"
}

# sidecar_pid <id> -- prints pid when the recorded process is alive
sidecar_pid() {
  local id="$1" pf pid
  pf=$(model_pidfile "$id")
  pid=$(read_pidfile "$pf" 2>/dev/null || true)
  if pid_alive "$pid"; then
    printf '%s\n' "$pid"
    return 0
  fi
  return 1
}

health_url_for() { printf 'http://127.0.0.1:%s/health\n' "$1"; }

wait_ready() {
  local id="$1" port="$2" pid="$3" waited=0 url
  url=$(health_url_for "$port")
  while [ "$waited" -lt "$STARTUP_TIMEOUT_S" ]; do
    if ! pid_alive "$pid"; then
      return 2   # died
    fi
    if http_ok "$url"; then
      printf '%s' "$waited"
      return 0
    fi
    sleep 1
    waited=$((waited + 1))
  done
  return 1       # timed out
}

cmd_start() {
  local id port ctx temp extra dest log pidfile url waited rc
  id="$1"
  dest=$(model_dir "$id")
  log=$(model_logfile "$id")
  pidfile=$(model_pidfile "$id")
  url=$(model_health_url "$id")

  have "$LLAMA_SERVER" || die "$LLAMA_SERVER not found; install llama.cpp (brew install llama.cpp)"

  if pid=$(sidecar_pid "$id"); then
    info "$id is already running (pid $pid, port $(model_port "$id"))"
    return 0
  fi
  [ -f "$pidfile" ] && rm -f "$pidfile"

  # weights must be present and match the registry before we serve them
  "$MODEL_SCRIPTS_DIR/verify.sh" --quick "$id" >/dev/null 2>&1 || {
    err "$id: weights are missing or do not match the registry"
    err "run: ./pull.sh $id"
    return 1
  }

  port="${PORT:-$(model_port "$id")}"
  ctx="${CTX:-$(model_ctx "$id")}"
  temp="${TEMP:-$(model_temp "$id")}"
  extra=$(model_extra "$id")
  [ -n "$port" ] || die "no port configured for $id in $PROFILES_FILE"
  [ -n "$ctx" ] || ctx=8192
  [ -n "$temp" ] || temp=0.0

  if port_in_use "$port"; then
    err "port $port on $HOST is already in use"
    err "stop the other sidecar, or choose another port: PORT=9001 ./serve.sh start $id"
    return 1
  fi

  ensure_data_dirs

  # argv (bash 3.2 arrays are fine)
  set -- "$LLAMA_SERVER" -m "$dest/model.gguf"
  if [ -f "$dest/mmproj.gguf" ]; then
    set -- "$@" --mmproj "$dest/mmproj.gguf"
  fi
  set -- "$@" --host "$HOST" --port "$port" -c "$ctx" --temp "$temp" -ngl 99 -a "$id" --no-webui
  if [ -n "$extra" ] && [ "$extra" != "-" ]; then
    # shellcheck disable=SC2086
    set -- "$@" $extra
  fi

  {
    printf '\n===== %s start %s =====\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$id"
    printf 'cmd: %s\n' "$*"
  } >>"$log"

  info "starting $id on $HOST:$port (ctx $ctx, temp $temp)"
  nohup "$@" >>"$log" 2>&1 &
  pid=$!
  printf '%s\n' "$pid" >"$pidfile"

  rc=0
  waited=$(wait_ready "$id" "$port" "$pid") || rc=$?
  if [ "$rc" -eq 0 ]; then
    info "$id is ready after ${waited}s (pid $pid, port $port)"
    info "openai-compatible base url: http://127.0.0.1:$port/v1"
    info "log: $log"
    return 0
  fi

  if [ "$rc" -eq 2 ]; then
    err "$id: llama-server exited during startup; last log lines:"
  else
    err "$id: not healthy after ${STARTUP_TIMEOUT_S}s; stopping it; last log lines:"
  fi
  tail -n 25 "$log" >&2 || true
  kill "$pid" 2>/dev/null || true
  rm -f "$pidfile"
  return 1
}

cmd_stop() {
  local id pidfile pid i
  id="$1"
  pidfile=$(model_pidfile "$id")
  if ! pid=$(sidecar_pid "$id"); then
    rm -f "$pidfile"
    info "$id is not running"
    return 0
  fi
  info "stopping $id (pid $pid)"
  kill -TERM "$pid" 2>/dev/null || true
  i=0
  while [ "$i" -lt "$STOP_TIMEOUT_S" ]; do
    pid_alive "$pid" || break
    sleep 1
    i=$((i + 1))
  done
  if pid_alive "$pid"; then
    warn "$id did not exit after ${STOP_TIMEOUT_S}s; sending SIGKILL"
    kill -KILL "$pid" 2>/dev/null || true
  fi
  rm -f "$pidfile"
  info "$id stopped"
}

cmd_status() {
  local id pid port rc=0
  for id in "$@"; do
    port=$(model_port "$id")
    if pid=$(sidecar_pid "$id"); then
      if http_ok "$(health_url_for "$port")"; then
        printf '%-22s %-6s running  pid=%-7s port=%-5s health=OK   log=%s\n' \
          "$id" "$(role_of_model "$id")" "$pid" "$port" "$(model_logfile "$id")"
      else
        printf '%-22s %-6s running  pid=%-7s port=%-5s health=DOWN log=%s\n' \
          "$id" "$(role_of_model "$id")" "$pid" "$port" "$(model_logfile "$id")"
        rc=1
      fi
    else
      printf '%-22s %-6s stopped  port=%-5s\n' "$id" "$(role_of_model "$id")" "$port"
    fi
  done
  return "$rc"
}

case "$cmd" in
  start)
    for id in $(expand_targets "$target" first); do cmd_start "$id"; done
    ;;
  stop)
    for id in $(expand_targets "$target" all); do cmd_stop "$id"; done
    ;;
  restart)
    for id in $(expand_targets "$target" first); do cmd_stop "$id"; cmd_start "$id"; done
    ;;
  status)
    # shellcheck disable=SC2046
    cmd_status $(expand_targets "$target" all)
    ;;
  logs)
    for id in $(expand_targets "$target" first); do
      info "tailing $(model_logfile "$id") (ctrl-c to stop)"
      exec tail -f "$(model_logfile "$id")"
    done
    ;;
  -h|--help|help)
    usage
    ;;
  *)
    err "unknown command: $cmd"
    usage
    exit 1
    ;;
esac
