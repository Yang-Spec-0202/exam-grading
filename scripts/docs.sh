#!/usr/bin/env bash
# 构建 mdBook 静态站（GitHub Pages 目标）。
#
# 单一内容源：docs/README.md、docs/user/、docs/dev/、docs/adr/、docs/changelog.md。
# 本脚本把它们复制成 docs/book/src/（已 gitignore），再执行 mdbook build。
# 配置依据见 docs/book/book.toml 与 docs/dev/research/toolchain.md。
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

book_dir="docs/book"
src_dir="$book_dir/src"

if ! command -v mdbook >/dev/null 2>&1; then
    printf '错误：找不到 mdbook（0.5.x）。\n' >&2
    printf '      安装：cargo install mdbook --locked，或下载官方 release 到 ~/.cargo/bin（见 docs/dev/research/toolchain.md）\n' >&2
    exit 1
fi

printf '==> 生成 mdBook 源：%s\n' "$src_dir"
rm -rf "$src_dir"
mkdir -p "$src_dir"

cp docs/README.md "$src_dir/index.md"
cp -R docs/user docs/dev docs/adr "$src_dir/"
cp docs/changelog.md "$src_dir/changelog.md"
cp "$book_dir/SUMMARY.md" "$src_dir/SUMMARY.md"

printf '==> mdbook build %s\n' "$book_dir"
mdbook build "$book_dir"

printf '完成：%s/book/index.html\n' "$book_dir"
