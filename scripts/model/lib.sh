#!/usr/bin/env bash
#
# lib.sh — shared helpers for the exam-grading model scripts.
#
# Sourced by pull.sh / verify.sh / serve.sh / health.sh. Not meant to be run alone.
# Deliberately Bash 3.2 compatible: macOS ships /bin/bash 3.2.57, which has no
# associative arrays and no `mapfile`. Do not use those here.
#
# No secrets are read or written by this file. API keys live in the platform
# key store (SPEC-platform.md) or the environment, never on disk in this tree.

set -euo pipefail

# --- locate this script directory and the repository root --------------------
_lib_source="${BASH_SOURCE[0]:-$0}"
MODEL_SCRIPTS_DIR=$(cd "$(dirname "$_lib_source")" && pwd -P)
REPO_ROOT=$(cd "$MODEL_SCRIPTS_DIR/../.." && pwd -P)

# --- defaults (defaults.conf only sets values the environment did not) -------
if [ -f "$MODEL_SCRIPTS_DIR/defaults.conf" ]; then
  # shellcheck disable=SC1091
  . "$MODEL_SCRIPTS_DIR/defaults.conf"
fi

: "${DEFAULT_OCR_MODEL:=paddleocr-vl-1.6}"
: "${DEFAULT_JUDGE_MODEL:=qwen3.5-4b-instruct}"
: "${EG_DATA_DIR:=$HOME/Library/Application Support/exam-grading}"
: "${MODELS_DIR:=$EG_DATA_DIR/models}"
: "${MODEL_LOG_DIR:=$EG_DATA_DIR/logs/model}"
: "${MODEL_RUN_DIR:=$EG_DATA_DIR/run}"
: "${STAGING_DIR:=$MODELS_DIR/.downloads}"
: "${HF_ENDPOINT:=https://huggingface.co}"
: "${RETRY_MAX:=5}"
: "${RETRY_DELAY_S:=2}"
: "${CONNECT_TIMEOUT_S:=15}"
: "${STALL_SPEED_BYTES:=1024}"
: "${STALL_TIME_S:=60}"
: "${LARGE_DOWNLOAD_BYTES:=5368709120}"
: "${STARTUP_TIMEOUT_S:=90}"
: "${STOP_TIMEOUT_S:=10}"
: "${HEALTH_TIMEOUT_S:=5}"
: "${LICENSE_ALLOWLIST:=MIT Apache-2.0 BSD-3-Clause ISC Unicode-3.0}"

REGISTRY_FILE="${REGISTRY_FILE:-$MODEL_SCRIPTS_DIR/registry.tsv}"
PROFILES_FILE="${PROFILES_FILE:-$MODEL_SCRIPTS_DIR/profiles.tsv}"

# --- logging (everything goes to stderr so stdout stays machine-readable) ----
log()  { printf '%s\n' "$*" >&2; }
info() { printf '[info]  %s\n' "$*" >&2; }
warn() { printf '[warn]  %s\n' "$*" >&2; }
err()  { printf '[error] %s\n' "$*" >&2; }
die()  { err "$*"; exit 1; }

have() { command -v "$1" >/dev/null 2>&1; }

require_cmd() {
  local c
  for c in "$@"; do
    have "$c" || die "required command not found: $c"
  done
}

# --- formatting -------------------------------------------------------------
human_bytes() {
  awk -v b="${1:-0}" 'BEGIN {
    split("B KB MB GB TB", u, " "); i = 1
    while (b >= 1024 && i < 5) { b /= 1024; i++ }
    printf("%.1f %s", b, u[i])
  }'
}

# --- file facts -------------------------------------------------------------
file_size() {
  if stat -f%z "$1" >/dev/null 2>&1; then
    stat -f%z "$1"
  elif stat -c%s "$1" >/dev/null 2>&1; then
    stat -c%s "$1"
  else
    wc -c <"$1" | tr -d ' '
  fi
}

sha256_of() {
  if have shasum; then
    shasum -a 256 "$1" | awk '{print $1}'
  elif have sha256sum; then
    sha256sum "$1" | awk '{print $1}'
  else
    die "no sha256 implementation found (need shasum or sha256sum)"
  fi
}

# --- registry access --------------------------------------------------------
# registry.tsv columns (tab separated, '#' lines are comments):
#   1 id  2 role  3 component  4 repo  5 remote_file  6 local_file
#   7 bytes  8 sha256  9 license  10 support  11 notes

registry_rows() {
  local want="${1:-}"
  [ -f "$REGISTRY_FILE" ] || die "registry not found: $REGISTRY_FILE"
  awk -F'\t' -v want="$want" '
    /^[[:space:]]*#/ { next }
    /^[[:space:]]*$/ { next }
    NF < 9 { next }
    (want == "" || $1 == want) { print }
  ' "$REGISTRY_FILE"
}

registry_model_ids() {
  [ -f "$REGISTRY_FILE" ] || die "registry not found: $REGISTRY_FILE"
  awk -F'\t' '
    /^[[:space:]]*#/ { next }
    /^[[:space:]]*$/ { next }
    NF < 9 { next }
    !seen[$1]++ { print $1 }
  ' "$REGISTRY_FILE"
}

registry_roles() {
  [ -f "$REGISTRY_FILE" ] || die "registry not found: $REGISTRY_FILE"
  awk -F'\t' '
    /^[[:space:]]*#/ { next }
    /^[[:space:]]*$/ { next }
    NF < 9 { next }
    !seen[$1 "\t" $2]++ { print $1 "\t" $2 }
  ' "$REGISTRY_FILE"
}

model_total_bytes() {
  registry_rows "$1" | awk -F'\t' '{ s += $7 } END { printf "%d\n", s + 0 }'
}

model_license() {
  registry_rows "$1" | awk -F'\t' 'NR == 1 { print $9 }'
}

model_support() {
  registry_rows "$1" | awk -F'\t' 'NR == 1 { print $10 }'
}

license_allowed() {
  local lic="$1" a
  for a in $LICENSE_ALLOWLIST; do
    [ "$a" = "$lic" ] && return 0
  done
  return 1
}

# --- model / role resolution ------------------------------------------------
# resolve_model <role|id> -- prints the registry id, fails loudly when unknown.
# 'ocr' and 'judge' resolve through the configurable defaults in defaults.conf.
resolve_model() {
  local want="$1" id
  case "$want" in
    ocr)   id="$DEFAULT_OCR_MODEL" ;;
    judge) id="$DEFAULT_JUDGE_MODEL" ;;
    *)     id="$want" ;;
  esac
  if ! registry_model_ids | grep -qx -- "$id"; then
    err "unknown model or role: '$want' (resolved to '$id')"
    err "known model ids: $(registry_model_ids | tr '\n' ' ')"
    err "known roles: ocr judge"
    return 1
  fi
  printf '%s\n' "$id"
}

role_of_model() {
  registry_rows "$1" | awk -F'\t' 'NR == 1 { print $2 }'
}

# --- serving profiles -------------------------------------------------------
# profiles.tsv columns: 1 id  2 role  3 port  4 ctx  5 temp  6 extra_args
profile_field() {
  local id="$1" idx="$2"
  [ -f "$PROFILES_FILE" ] || die "profiles file not found: $PROFILES_FILE"
  awk -F'\t' -v id="$id" -v idx="$idx" '
    /^[[:space:]]*#/ { next }
    /^[[:space:]]*$/ { next }
    $1 == id { print $idx; exit }
  ' "$PROFILES_FILE"
}

model_port() { profile_field "$1" 3; }
model_ctx()  { profile_field "$1" 4; }
model_temp() { profile_field "$1" 5; }
model_extra() { profile_field "$1" 6; }

# --- paths ------------------------------------------------------------------
model_dir()  { printf '%s/%s\n' "$MODELS_DIR" "$1"; }
model_file() { printf '%s/%s/%s\n' "$MODELS_DIR" "$1" "$2"; }
model_pidfile() { printf '%s/llama-%s.pid\n' "$MODEL_RUN_DIR" "$1"; }
model_logfile() { printf '%s/llama-%s.log\n' "$MODEL_LOG_DIR" "$1"; }
model_health_url() { printf 'http://127.0.0.1:%s/health\n' "$(model_port "$1")"; }

ensure_data_dirs() {
  mkdir -p "$MODELS_DIR" "$MODEL_LOG_DIR" "$MODEL_RUN_DIR"
}

# --- process / port helpers -------------------------------------------------
pid_alive() {
  [ -n "${1:-}" ] || return 1
  kill -0 "$1" 2>/dev/null
}

read_pidfile() {
  local pf="$1"
  [ -f "$pf" ] || return 1
  head -n 1 "$pf" 2>/dev/null | tr -d '[:space:]'
}

port_in_use() {
  local port="$1"
  [ -n "$port" ] || return 1
  if have lsof; then
    lsof -nP -iTCP:"$port" -sTCP:LISTEN >/dev/null 2>&1
  else
    (exec 3<>"/dev/tcp/127.0.0.1/$port") >/dev/null 2>&1
  fi
}

# --- HTTP helpers -----------------------------------------------------------
# http_code <url> -- prints the HTTP status code, or 000 when unreachable.
http_code() {
  curl -sS -o /dev/null -w '%{http_code}' -m "${HEALTH_TIMEOUT_S}" "$1" 2>/dev/null || printf '000'
}

http_ok() {
  [ "$(http_code "$1")" = "200" ]
}
