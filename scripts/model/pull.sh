#!/usr/bin/env bash
#
# pull.sh — download model weights for exam-grading into the user data directory.
#
# Reads scripts/model/registry.tsv and fetches every file for the requested
# model ids or roles. Weights land in
#     ~/Library/Application Support/exam-grading/models/<id>/
# and are never committed to the repository.
#
# Guarantees:
#   * exact size check + SHA-256 check before a file is considered good
#   * resumable downloads (curl -C -) into a staging dir, retried with backoff
#   * all-or-nothing install per model: a failed download leaves no half model
#   * explicit failure with a non-zero exit code; nothing is silently skipped
#   * refuses any single model larger than 5 GiB unless --allow-large is given
#
# usage: ./pull.sh [options] [MODEL|ROLE ...]
#   no argument      -> download the default OCR model and the default judge model
#   ocr | judge      -> the configured default model for that role
#   <registry id>    -> that exact model (see --list)
# options:
#   --dry-run        print the plan, validate registry/license/size, download nothing
#   --list           list registry entries with install state and exit
#   --force          re-download and replace an already installed model
#   --allow-large    permit a single model whose total size exceeds 5 GiB
#   -q, --quiet      less progress chatter
#   -h, --help       this text

set -euo pipefail

# shellcheck source=lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/lib.sh"

DRY_RUN=0
FORCE=0
ALLOW_LARGE=0
QUIET=0
LIST=0
WANTED=""

usage() {
  cat >&2 <<'USAGE'
pull.sh - download exam-grading model weights into the user data directory

usage: ./pull.sh [options] [MODEL|ROLE ...]

  no argument      download the default OCR model and the default judge model
  ocr | judge      the configured default model for that role
  <registry id>    that exact model (see --list)

options:
  --dry-run        print the plan, validate registry/license/size, download nothing
  --list           list registry entries with install state and exit
  --force          re-download and replace an already installed model
  --allow-large    permit a single model whose total size exceeds 5 GiB
  -q, --quiet      less progress chatter
  -h, --help       this text
USAGE
}

while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run)     DRY_RUN=1 ;;
    --list)        LIST=1 ;;
    --force)       FORCE=1 ;;
    --allow-large) ALLOW_LARGE=1 ;;
    -q|--quiet)    QUIET=1 ;;
    -h|--help)     usage; exit 0 ;;
    --)            shift; while [ $# -gt 0 ]; do WANTED="$WANTED $1"; shift; done; break ;;
    -*)            err "unknown option: $1"; usage >&2; exit 1 ;;
    *)             WANTED="$WANTED $1" ;;
  esac
  shift
done

: "${LIST:=0}"

require_cmd curl awk
have shasum || have sha256sum || die "need shasum or sha256sum"

# --- --list -----------------------------------------------------------------
if [ "$LIST" -eq 1 ]; then
  printf '%-22s %-6s %-10s %-12s %-10s %s\n' ID ROLE SIZE LICENSE SUPPORT STATE
  for id in $(registry_model_ids); do
    role=$(role_of_model "$id")
    total=$(model_total_bytes "$id")
    lic=$(model_license "$id")
    sup=$(model_support "$id")
    dest=$(model_dir "$id")
    state="missing"
    if [ -d "$dest" ]; then
      want=$(registry_rows "$id" | wc -l | tr -d ' ')
      got=0
      for row in $(registry_rows "$id" | cut -f6); do
        [ -f "$dest/$row" ] && got=$((got + 1))
      done
      if [ "$got" = "$want" ]; then state="installed"; else state="partial($got/$want)"; fi
    fi
    if license_allowed "$lic"; then licflag="$lic"; else licflag="$lic!DENY"; fi
    printf '%-22s %-6s %-10s %-12s %-10s %s\n' "$id" "$role" "$(human_bytes "$total")" "$licflag" "$sup" "$state"
  done
  printf '\nweights directory: %s\n' "$MODELS_DIR"
  exit 0
fi

# --- resolve the work list --------------------------------------------------
# a bare role means "the configured default model for that role"
model_list=""
for w in $WANTED; do
  mid=$(resolve_model "$w") || exit 1
  case " $model_list " in
    *" $mid "*) : ;;                 # already queued
    *) model_list="$model_list $mid" ;;
  esac
done
if [ -z "$model_list" ]; then
  for w in ocr judge; do
    mid=$(resolve_model "$w") || exit 1
    case " $model_list " in
      *" $mid "*) : ;;
      *) model_list="$model_list $mid" ;;
    esac
  done
fi

# --- print the plan ---------------------------------------------------------
printf '\n%-4s %-22s %-6s %-10s %-10s %s\n' '#' ID ROLE SIZE LICENSE SUPPORT
i=0
for id in $model_list; do
  i=$((i + 1))
  total=$(model_total_bytes "$id")
  printf '%-4s %-22s %-6s %-10s %-10s %s\n' "$i" "$id" "$(role_of_model "$id")" \
    "$(human_bytes "$total")" "$(model_license "$id")" "$(model_support "$id")"
done
printf '\nweights directory: %s\n' "$MODELS_DIR"

# --- pre-flight: licenses, size guard, disk space ---------------------------
grand_total=0
for id in $model_list; do
  total=$(model_total_bytes "$id")
  grand_total=$((grand_total + total))

  lic=$(model_license "$id")
  if ! license_allowed "$lic"; then
    err "license gate failed for $id: '$lic' is not in the allowlist [$LICENSE_ALLOWLIST]"
    err "per SPEC-inference.md §2.4 this model must not be loaded; refusing."
    exit 1
  fi

  if [ "$total" -gt "$LARGE_DOWNLOAD_BYTES" ] && [ "$ALLOW_LARGE" -eq 0 ]; then
    err "$id is $(human_bytes "$total"), above the 5 GiB reporting threshold"
    err "report it first, then re-run with --allow-large (nothing was downloaded)"
    exit 1
  fi

  sup=$(model_support "$id")
  if [ "$sup" != "verified" ] && [ "$QUIET" -eq 0 ]; then
    warn "$id is marked 'unverified' for llama.cpp: it may not load in this serving stack"
  fi
done

free_bytes=$(df -k "$HOME" 2>/dev/null | awk 'NR==2 {print $4 * 1024}' || echo 0)
if [ -n "$free_bytes" ] && [ "$free_bytes" -gt 0 ]; then
  need=$((grand_total + grand_total / 10))
  if [ "$free_bytes" -lt "$need" ]; then
    err "not enough free disk space: need about $(human_bytes "$need"), have $(human_bytes "$free_bytes")"
    exit 1
  fi
fi

if [ "$DRY_RUN" -eq 1 ]; then
  printf '\n--dry-run: no files will be written, no network requests made\n'
  for id in $model_list; do
    dest=$(model_dir "$id")
    inst="not installed"
    [ -d "$dest" ] && inst="present at $dest"
    printf '\n[plan] %s (%s) -> %s  [%s]\n' "$id" "$(role_of_model "$id")" "$dest" "$inst"
    registry_rows "$id" | while IFS="$(printf '\t')" read -r rid role comp repo rfile lfile bytes sha lic sup notes; do
      printf '  %-6s %-10s %12s  sha256:%s..\n' "$comp" "$lfile" "$bytes" "$(printf '%s' "$sha" | cut -c1-16)"
      printf '         url: %s/%s/resolve/main/%s\n' "$HF_ENDPOINT" "$repo" "$rfile"
    done
  done
  printf '\ntotal download: %s\n' "$(human_bytes "$grand_total")"
  exit 0
fi

# --- real download ----------------------------------------------------------
ensure_data_dirs
mkdir -p "$STAGING_DIR"

current_install=""
cleanup() {
  # roll back a half-built install directory; keep staging for resume
  if [ -n "$current_install" ] && [ -d "$current_install" ]; then
    rm -rf "$current_install"
  fi
}
trap cleanup EXIT INT TERM

# fetch_one <url> <outfile> -- resumable, retried, optional HF token
fetch_one() {
  local url="$1" out="$2" prog
  # interactive terminals get a progress bar; logs/pipes get our own lines only
  if [ -t 2 ]; then prog="--progress-bar"; else prog="--no-progress-meter"; fi
  if [ -n "${HF_TOKEN:-}" ]; then
    curl -fL $prog \
      --retry "$RETRY_MAX" --retry-delay "$RETRY_DELAY_S" --retry-all-errors \
      --connect-timeout "$CONNECT_TIMEOUT_S" \
      --speed-limit "$STALL_SPEED_BYTES" --speed-time "$STALL_TIME_S" \
      -H "Authorization: Bearer $HF_TOKEN" \
      -C - -o "$out" "$url"
  else
    curl -fL $prog \
      --retry "$RETRY_MAX" --retry-delay "$RETRY_DELAY_S" --retry-all-errors \
      --connect-timeout "$CONNECT_TIMEOUT_S" \
      --speed-limit "$STALL_SPEED_BYTES" --speed-time "$STALL_TIME_S" \
      -C - -o "$out" "$url"
  fi
}

failed=0
for id in $model_list; do
  dest=$(model_dir "$id")
  stage="$STAGING_DIR/$id"
  total=$(model_total_bytes "$id")

  # already installed?
  if [ -d "$dest" ] && [ "$FORCE" -eq 0 ]; then
    all_ok=1
    while IFS="$(printf '\t')" read -r rid role comp repo rfile lfile bytes sha lic sup notes; do
      f="$dest/$lfile"
      if [ ! -f "$f" ] || [ "$(file_size "$f")" != "$bytes" ] || [ "$(sha256_of "$f")" != "$sha" ]; then
        all_ok=0
      fi
    done <<EOF
$(registry_rows "$id")
EOF
    if [ "$all_ok" -eq 1 ]; then
      info "$id already installed and verified at $dest (use --force to re-download)"
      continue
    fi
    err "$id exists at $dest but does not match the registry"
    err "re-run with --force to replace it (nothing was changed)"
    failed=1
    continue
  fi

  printf '\n==> %s (%s, %s)\n' "$id" "$(role_of_model "$id")" "$(human_bytes "$total")"
  mkdir -p "$stage"

  model_failed=0
  while IFS="$(printf '\t')" read -r rid role comp repo rfile lfile bytes sha lic sup notes; do
    out="$stage/$lfile"
    url="$HF_ENDPOINT/$repo/resolve/main/$rfile"

    # a resumed file must not exceed the expected size
    if [ -f "$out" ]; then
      have_b=$(file_size "$out")
      if [ "$have_b" -gt "$bytes" ]; then
        warn "$lfile: local staging file is larger than expected, restarting it"
        rm -f "$out"
      elif [ "$have_b" = "$bytes" ]; then
        info "$lfile: already fully downloaded in staging, skipping fetch"
      fi
    fi

    if [ ! -f "$out" ] || [ "$(file_size "$out")" != "$bytes" ]; then
      info "$lfile: fetching $(human_bytes "$bytes") from $repo"
      if ! fetch_one "$url" "$out"; then
        err "$lfile: download failed ($url)"
        model_failed=1
        break
      fi
    fi

    actual=$(file_size "$out")
    if [ "$actual" != "$bytes" ]; then
      err "$lfile: size mismatch (expected $bytes, got $actual)"
      rm -f "$out"
      model_failed=1
      break
    fi

    info "$lfile: verifying sha256"
    actual_sha=$(sha256_of "$out")
    if [ "$actual_sha" != "$sha" ]; then
      err "$lfile: sha256 mismatch"
      err "  expected $sha"
      err "  actual   $actual_sha"
      rm -f "$out"
      model_failed=1
      break
    fi
  done <<EOF
$(registry_rows "$id")
EOF

  if [ "$model_failed" -ne 0 ]; then
    err "$id: download or verification failed; nothing was installed"
    failed=1
    continue
  fi

  # assemble the model directory away from its final name, then swap it in
  current_install="$MODELS_DIR/.install-$id-$$"
  rm -rf "$current_install"
  mkdir -p "$current_install"
  while IFS="$(printf '\t')" read -r rid role comp repo rfile lfile bytes sha lic sup notes; do
    mv "$stage/$lfile" "$current_install/$lfile"
  done <<EOF
$(registry_rows "$id")
EOF

  {
    printf '{\n'
    printf '  "id": "%s",\n' "$id"
    printf '  "role": "%s",\n' "$(role_of_model "$id")"
    printf '  "license": "%s",\n' "$(model_license "$id")"
    printf '  "support": "%s",\n' "$(model_support "$id")"
    printf '  "installed_at": "%s",\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
    printf '  "source": "%s",\n' "$HF_ENDPOINT"
    printf '  "files": [\n'
    first=1
    while IFS="$(printf '\t')" read -r rid role comp repo rfile lfile bytes sha lic sup notes; do
      [ "$first" -eq 1 ] || printf ',\n'
      first=0
      printf '    { "component": "%s", "repo": "%s", "remote_file": "%s", "local_file": "%s", "bytes": %s, "sha256": "%s" }' \
        "$comp" "$repo" "$rfile" "$lfile" "$bytes" "$sha"
    done <<EOF
$(registry_rows "$id")
EOF
    printf '\n  ]\n}\n'
  } >"$current_install/provenance.json"

  if [ -e "$dest" ]; then
    bak="$MODELS_DIR/.backup-$id-$$"
    rm -rf "$bak"
    mv "$dest" "$bak"
    if mv "$current_install" "$dest"; then
      rm -rf "$bak"
    else
      mv "$bak" "$dest"
      err "$id: could not move the new download into place; previous files restored"
      failed=1
      continue
    fi
  else
    mv "$current_install" "$dest" || { err "$id: could not install to $dest"; failed=1; continue; }
  fi
  current_install=""
  rm -rf "$stage"
  info "$id: installed and verified at $dest"
done

printf '\n'
if [ "$failed" -ne 0 ]; then
  err "one or more models failed; see the messages above (no partial model was installed)"
  exit 1
fi
info "all requested models are installed under $MODELS_DIR"
