#!/usr/bin/env bash
# gold 抽检：把规则路径条目喂给 eg-subject-math / eg-grade，验证金标期望与实现一致。
# 在 /tmp 临时 cargo 工程中构建运行（不写仓库 target/Cargo.lock）。
set -euo pipefail

# 允许从非登录 shell 直接运行（cargo 通常在 ~/.cargo/env）。
if [ -f "$HOME/.cargo/env" ]; then
  # shellcheck disable=SC1091
  source "$HOME/.cargo/env"
fi

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD="${GOLD_SMOKE_DIR:-$(mktemp -d /tmp/exam-grading-gold-smoke.XXXXXX)}"
mkdir -p "$BUILD/src"
cp "$REPO/scripts/fixtures/smoke_gold/src/main.rs" "$BUILD/src/main.rs"
cat > "$BUILD/Cargo.toml" <<EOF
[package]
name = "gold-smoke"
version = "0.1.0"
edition = "2021"
publish = false

[workspace]

[dependencies]
eg-domain = { path = "$REPO/crates/eg-domain" }
eg-subject-math = { path = "$REPO/crates/eg-subject-math" }
eg-grade = { path = "$REPO/crates/eg-grade" }
serde_json = "1.0.154"
EOF
cd "$BUILD"
cargo run --quiet -- "$REPO"
