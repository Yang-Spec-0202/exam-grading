#!/usr/bin/env bash
#
# verify.sh — size + SHA-256 verification for downloaded model weights.
#
# Checks the files on disk against scripts/model/registry.tsv, enforces the
# license allowlist from defaults.conf (SPEC-inference.md §2.4) and reports the
# install state per model. Nothing is downloaded or repaired here.
#
# usage: ./verify.sh [options] [MODEL|ROLE ...]
#   no argument      verify every registry model that is installed; models that
#                    are not installed are reported as such and are not a failure
#   ocr | judge      the configured default model for that role
#   <registry id>    that exact model (strict: a missing model is a failure)
# options:
#   --quick          size check only (skips SHA-256; for a fast pre-flight)
#   --require-all    treat a not-installed model as a failure
#   --json           machine-readable summary on stdout
#   -h, --help       this text
#
# exit codes: 0 = everything checked is good, 1 = at least one check failed.

set -euo pipefail

# shellcheck source=lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/lib.sh"

QUICK=0
REQUIRE_ALL=0
JSON=0
WANTED=""
STRICT=0

usage() {
  cat >&2 <<'USAGE'
verify.sh - verify downloaded model weights against the registry

usage: ./verify.sh [options] [MODEL|ROLE ...]

  no argument      verify installed models; not-installed ones are reported only
  ocr | judge      the configured default model for that role
  <registry id>    that exact model (a missing model is a failure)

options:
  --quick          size check only (no SHA-256 hashing)
  --require-all    treat a not-installed model as a failure
  --json           machine-readable summary on stdout
  -h, --help       this text
USAGE
}

while [ $# -gt 0 ]; do
  case "$1" in
    --quick)       QUICK=1 ;;
    --require-all) REQUIRE_ALL=1 ;;
    --json)        JSON=1 ;;
    -h|--help)     usage; exit 0 ;;
    --)            shift; while [ $# -gt 0 ]; do WANTED="$WANTED $1"; shift; done; break ;;
    -*)            err "unknown option: $1"; usage; exit 1 ;;
    *)             WANTED="$WANTED $1" ;;
  esac
  shift
done

[ -f "$REGISTRY_FILE" ] || die "registry not found: $REGISTRY_FILE"
have shasum || have sha256sum || die "need shasum or sha256sum"

# resolve the work list
model_list=""
for w in $WANTED; do
  mid=$(resolve_model "$w") || exit 1
  case " $model_list " in *" $mid "*) : ;; *) model_list="$model_list $mid" ;; esac
done
if [ -z "$model_list" ]; then
  model_list=$(registry_model_ids)
else
  STRICT=1
fi

fail_count=0
checked_count=0
missing_count=0
json_parts=""

[ "$JSON" -eq 1 ] || printf '%-22s %-6s %-10s %s\n' MODEL ROLE STATE DETAIL

for id in $model_list; do
  dest=$(model_dir "$id")
  role=$(role_of_model "$id")
  lic=$(model_license "$id")
  total=$(model_total_bytes "$id")
  nfiles=$(registry_rows "$id" | wc -l | tr -d ' ')

  model_state="ok"
  detail=""
  files_json=""

  if [ ! -d "$dest" ]; then
    missing_count=$((missing_count + 1))
    model_state="missing"
    detail="not installed (run ./pull.sh $id)"
    if [ "$STRICT" -eq 1 ] || [ "$REQUIRE_ALL" -eq 1 ]; then
      fail_count=$((fail_count + 1))
    fi
  elif ! license_allowed "$lic"; then
    fail_count=$((fail_count + 1))
    model_state="license-denied"
    detail="license '$lic' is outside the allowlist [$LICENSE_ALLOWLIST]"
  else
    while IFS="$(printf '\t')" read -r rid rrole comp repo rfile lfile bytes sha s_lic sup notes; do
      [ -n "$rid" ] || continue
      checked_count=$((checked_count + 1))
      f="$dest/$lfile"
      fstate="ok"
      fdetail=""

      if [ ! -f "$f" ]; then
        fstate="missing"; fdetail="missing file $lfile"
      else
        actual_size=$(file_size "$f")
        if [ "$actual_size" != "$bytes" ]; then
          fstate="size-mismatch"
          fdetail="$lfile size $actual_size != $bytes"
        elif [ "$QUICK" -eq 0 ]; then
          actual_sha=$(sha256_of "$f")
          if [ "$actual_sha" != "$sha" ]; then
            fstate="sha256-mismatch"
            fdetail="$lfile sha256 ${actual_sha%????????????????}... != ${sha%????????????????}..."
          fi
        fi
      fi

      if [ "$fstate" != "ok" ]; then
        model_state="corrupt"
        fail_count=$((fail_count + 1))
        detail="${detail}${detail:+; }$fdetail"
      fi

      files_json="${files_json}${files_json:+,}{\"component\":\"$comp\",\"file\":\"$lfile\",\"state\":\"$fstate\"}"
    done <<EOF
$(registry_rows "$id")
EOF

    if [ "$model_state" = "ok" ]; then
      if [ "$QUICK" -eq 1 ]; then
        detail="size ok ($nfiles file(s), $(human_bytes "$total"))"
      else
        detail="sha256 ok ($nfiles file(s), $(human_bytes "$total"))"
      fi
    fi

    # provenance is advisory: warn, never fail
    if [ -f "$dest/provenance.json" ]; then
      detail="$detail; provenance present"
    else
      detail="$detail; no provenance.json"
    fi
  fi

  [ "$JSON" -eq 1 ] || printf '%-22s %-6s %-10s %s\n' "$id" "$role" "$model_state" "$detail"
  json_parts="${json_parts}${json_parts:+,}{\"id\":\"$id\",\"role\":\"$role\",\"license\":\"$lic\",\"state\":\"$model_state\",\"files\":[$files_json]}"
done

if [ "$JSON" -eq 1 ]; then
  status="ok"
  [ "$fail_count" -eq 0 ] || status="failed"
  printf '{"status":"%s","quick":%s,"checked_files":%s,"failures":%s,"missing_models":%s,"models":[%s]}\n' \
    "$status" \
    "$([ "$QUICK" -eq 1 ] && echo true || echo false)" \
    "$checked_count" "$fail_count" "$missing_count" "$json_parts"
else
  printf '\n'
  if [ "$fail_count" -eq 0 ]; then
    info "verify: OK ($checked_count file(s) checked, $missing_count model(s) not installed)"
  else
    err "verify: FAILED ($fail_count problem(s), $checked_count file(s) checked)"
  fi
fi

[ "$fail_count" -eq 0 ] || exit 1
exit 0
