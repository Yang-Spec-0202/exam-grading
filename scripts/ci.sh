#!/usr/bin/env bash
# 本地质量门禁（SPEC.md §10、tasks/todo.md T03）
#
# 覆盖：格式化 / 静态检查 / 全量测试 / 依赖合规 / workspace 结构 / 文档门禁。
# 用法：bash scripts/ci.sh
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

step() { printf '\n==> %s\n' "$1"; }

if ! command -v cargo >/dev/null 2>&1; then
    printf '错误：找不到 cargo。\n' >&2
    printf '      新开的 shell 请先执行： source "$HOME/.cargo/env"\n' >&2
    printf '      工具链说明见 docs/dev/research/toolchain.md\n' >&2
    exit 1
fi

step "cargo fmt --all -- --check"
cargo fmt --all -- --check

step "cargo clippy --workspace --all-targets --all-features -- -D warnings"
cargo clippy --workspace --all-targets --all-features -- -D warnings

step "cargo test --workspace"
cargo test --workspace

step "cargo deny check"
if ! command -v cargo-deny >/dev/null 2>&1; then
    printf '错误：找不到 cargo-deny。\n' >&2
    printf '      安装：cargo install cargo-deny，或下载官方 release（见 docs/dev/research/toolchain.md）\n' >&2
    exit 1
fi
cargo deny check

step "workspace crate 数量校验（期望 13，见 CAPABILITY-MAP.md §2）"
crate_count="$(cargo metadata --no-deps --format-version 1 \
    | python3 -c 'import json,sys; print(len(json.load(sys.stdin)["workspace_members"]))')"
if [ "$crate_count" -ne 13 ]; then
    printf '错误：workspace 成员数为 %s，期望 13。\n' "$crate_count" >&2
    exit 1
fi
printf 'OK：%s 个 crate\n' "$crate_count"

step "文档门禁（链接检查 + 规范无示例代码 + mdBook 构建）"
bash scripts/docs-check.sh

printf '\nCI 全部通过。\n'
