#!/usr/bin/env python3
"""CAS sidecar 测试桩（不依赖 sympy / 不联网）。

用法：`python3 stub_sidecar.py <mode>`（mode 由测试通过 `CasConfig` 传入）。
模式：
- `ok`：ping 返回版本；simplify_zero 依据 JSON 约定返回零/非零常量；
- `zero`：任何 simplify_zero 都返回 `zero:true`；
- `nonzero`：返回 `zero:false, constant:true`（用于判定「不相等」路径）；
- `slow`：sleep 0.4s 后按 `ok` 响应（用于超时测试，配合 <400ms 超时）；
- `hang`：sleep 60s 不响应（Rust 侧超时必须 kill）；
- `crash`：直接非零退出；
- `garbage`：输出非法 JSON；
- `counter`：环境变量 `STUB_COUNTER` 指定文件时，每次请求追加一行（缓存/调用次数测试）。
"""

from __future__ import annotations

import json
import os
import sys
import time


def log_request() -> None:
    path = sys.argv[2] if len(sys.argv) > 2 else os.environ.get("STUB_COUNTER")
    if path:
        with open(path, "a", encoding="utf-8") as handle:
            handle.write("1\n")


def respond(payload: dict) -> None:
    print(json.dumps(payload, ensure_ascii=False), flush=True)


def main() -> None:
    mode = sys.argv[1] if len(sys.argv) > 1 else "ok"
    if mode == "hang":
        time.sleep(60)
        return
    if mode == "crash":
        sys.exit(3)
    line = sys.stdin.readline()
    log_request()
    if mode == "garbage":
        print("not-json", flush=True)
        return
    if mode == "slow":
        time.sleep(0.4)
    try:
        request = json.loads(line) if line else {}
    except Exception:
        respond({"ok": False, "error": "非法请求 JSON"})
        return
    op = request.get("op")
    if op == "ping":
        respond({"ok": True, "sympy": "9.9.9-stub", "protocol": "1"})
        return
    if op == "simplify_zero":
        if mode == "zero":
            respond(
                {
                    "ok": True,
                    "zero": True,
                    "constant": False,
                    "diff": "0",
                    "sympy": "9.9.9-stub",
                }
            )
            return
        if mode == "nonzero":
            respond(
                {
                    "ok": True,
                    "zero": False,
                    "constant": True,
                    "diff": "1",
                    "sympy": "9.9.9-stub",
                }
            )
            return
        lhs = str(request.get("lhs", "")).strip()
        rhs = str(request.get("rhs", "")).strip()
        zero = lhs == rhs
        respond(
            {
                "ok": True,
                "zero": zero,
                "constant": zero,
                "diff": "0" if zero else f"({lhs})-({rhs})",
                "sympy": "9.9.9-stub",
            }
        )
        return
    if op == "matrix_det":
        respond({"ok": True, "det": "0", "sympy": "9.9.9-stub"})
        return
    respond({"ok": False, "error": f"未实现 op: {op}"})


if __name__ == "__main__":
    main()
