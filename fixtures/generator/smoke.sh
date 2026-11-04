#!/usr/bin/env bash
# fixtures/ 冒烟程序：真实 capture 导入 + eg-inference 夹具键命中/负向拒绝 + align 身份。
# 在 /tmp 的临时 cargo 工程里构建运行（不向仓库写 target/Cargo.lock）。
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD="${FIXTURES_SMOKE_DIR:-$(mktemp -d /tmp/exam-grading-smoke.XXXXXX)}"
mkdir -p "$BUILD/src"
cp "$REPO/fixtures/generator/smoke/src/main.rs" "$BUILD/src/main.rs"
cat > "$BUILD/Cargo.toml" <<EOF
[package]
name = "fixtures-smoke"
version = "0.1.0"
edition = "2021"
publish = false

[workspace]

[dependencies]
eg-domain = { path = "$REPO/crates/eg-domain" }
eg-platform = { path = "$REPO/crates/eg-platform" }
eg-capture = { path = "$REPO/crates/eg-capture" }
eg-align = { path = "$REPO/crates/eg-align" }
eg-recognize = { path = "$REPO/crates/eg-recognize" }
eg-inference = { path = "$REPO/crates/eg-inference" }
serde_json = "1.0.154"
tempfile = "3.27.0"
EOF
cargo run --quiet --manifest-path "$BUILD/Cargo.toml" -- --repo "$REPO"
