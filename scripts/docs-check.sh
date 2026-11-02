#!/usr/bin/env bash
# 文档门禁（SPEC.md §8 测试策略「文档」行、§9）：规范无示例代码 + 文档相对链接 0 死链 + mdBook 构建。
#
# 用法：bash scripts/docs-check.sh
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

fail=0

printf '==> 检查：规范文件不得包含示例代码围栏\n'
# SPEC.md §9 与 CAPABILITY-MAP.md §7：规范（含配置总表）禁止代码块。
# 允许行内代码（反引号单个），只拒绝三反引号围栏。
spec_files=(SPEC.md SPEC-*.md CAPABILITY-MAP.md CONFIG.md)
found=0
for f in "${spec_files[@]}"; do
    [ -e "$f" ] || continue
    if grep -n '```' "$f" >/dev/null 2>&1; then
        printf '失败：%s 含三反引号代码围栏（规范禁止示例代码）\n' "$f" >&2
        grep -n '```' "$f" >&2 || true
        found=1
    fi
done
if [ "$found" -eq 1 ]; then fail=1; else printf 'OK：%s 个规范文件均无代码围栏\n' "${#spec_files[@]}"; fi

printf '\n==> 检查：docs/ 内相对链接\n'
python3 - <<'PY'
import os
import re
import sys

root = "docs"
skip_dirs = {os.path.join("docs", "book", "src"), os.path.join("docs", "book", "book")}
# SUMMARY.md 的链接相对 mdBook 的 src 目录（由 scripts/docs.sh 生成），不是相对 docs/book，
# 因此不在此检查；章节缺失由 mdbook build（create-missing = false）负责发现。
skip_files = {os.path.join("docs", "book", "SUMMARY.md")}

inline_link = re.compile(r"!?\[[^\]]*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
link_def = re.compile(r"^\s*\[[^\]]+\]:\s*(\S+)", re.MULTILINE)
scheme = re.compile(r"^[a-zA-Z][a-zA-Z0-9+.-]*:")

checked = 0
broken = []

for dirpath, dirnames, filenames in os.walk(root):
    dirnames[:] = sorted(d for d in dirnames if os.path.join(dirpath, d) not in skip_dirs)
    for name in sorted(filenames):
        if not name.endswith(".md"):
            continue
        path = os.path.join(dirpath, name)
        if path in skip_files:
            continue
        text = open(path, encoding="utf-8").read()
        targets = inline_link.findall(text) + link_def.findall(text)
        for raw in targets:
            target = raw.strip()
            if not target or target.startswith("#"):
                continue
            if scheme.match(target):
                continue
            target = target.split("#", 1)[0].split("?", 1)[0]
            if not target:
                continue
            checked += 1
            resolved = os.path.normpath(os.path.join(dirpath, target))
            if not os.path.exists(resolved):
                broken.append((path, raw, resolved))

print(f"已检查 {checked} 条相对链接，{len(broken)} 条死链")
for src, raw, resolved in broken:
    print(f"死链：{src} -> {raw}（解析为 {resolved}）", file=sys.stderr)
sys.exit(1 if broken else 0)
PY
[ $? -eq 0 ] || fail=1

if command -v mdbook >/dev/null 2>&1; then
    printf '\n==> 构建：mdBook 静态站\n'
    bash scripts/docs.sh
else
    printf '\n警告：未安装 mdbook（0.5.x），跳过静态站构建。\n'
    printf '      安装方式见 docs/dev/research/toolchain.md。\n'
fi

if [ "$fail" -ne 0 ]; then
    printf '\n文档门禁失败。\n' >&2
    exit 1
fi
printf '\n文档门禁通过。\n'
