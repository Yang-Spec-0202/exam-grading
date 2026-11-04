#!/usr/bin/env bash
# 重新生成 fixtures/（图片 + 期望 + eg-inference 夹具键 + MANIFEST + 自检）。
#
# 依赖：
#   - python3 + Pillow（图片渲染；字体可用 FIXTURES_FONT 覆盖）
#   - Rust 工具链（keygen 用仓库内 crate 当前实现渲染 OCR 提示词并算夹具键）
#
# 用法：fixtures/generator/regenerate.sh
# 说明：keygen 在 /tmp 的临时 cargo 工程里构建运行，不在仓库内产生 target/Cargo.lock。
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD="${FIXTURES_BUILD_DIR:-$(mktemp -d /tmp/exam-grading-fixtures.XXXXXX)}"
KEYGEN_DIR="$BUILD/keygen"

echo "[1/3] 生成图片/试卷/期望（Pillow）…"
FIXTURES_BUILD_DIR="$BUILD" python3 "$REPO/fixtures/generator/generate_fixtures.py"

echo "[2/3] 计算 eg-inference 夹具键并写 MANIFEST（Rust keygen）…"
mkdir -p "$KEYGEN_DIR/src"
cp "$REPO/fixtures/generator/keygen/src/main.rs" "$KEYGEN_DIR/src/main.rs"
cat > "$KEYGEN_DIR/Cargo.toml" <<EOF
[package]
name = "fixtures-keygen"
version = "0.1.0"
edition = "2021"
publish = false

[workspace]

[dependencies]
eg-domain = { path = "$REPO/crates/eg-domain" }
eg-inference = { path = "$REPO/crates/eg-inference" }
eg-recognize = { path = "$REPO/crates/eg-recognize" }
serde_json = "1.0.154"
EOF
cargo run --quiet --manifest-path "$KEYGEN_DIR/Cargo.toml" -- \
  --repo "$REPO" --build "$BUILD"

echo "[3/3] 自检…"
python3 "$REPO/fixtures/generator/verify_fixtures.py"

echo "[ok] 完成：$REPO/fixtures"
echo "[ok] 构建中间目录：$BUILD"
