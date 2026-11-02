#!/usr/bin/env python3
"""sympy CAS sidecar（JSON-lines 协议，`SPEC-subject-math.md` §7 / T27）。

一行一个请求（stdin），一行一个响应（stdout，flush）。只做「判定等价」：
- `ping`：返回 sympy 版本；
- `simplify_zero`：判定 lhs - rhs 是否恒等于 0；
- `matrix_det`：数值/符号矩阵行列式（诊断用）。

安全边界（对应 docs/dev/research/cas.md R2）：
- 输入只允许受限 ASCII 字符集与白名单函数/符号；
- 拒绝 `__`、引号、反斜杠、方括号等可构造代码的字符；
- 输入长度上限 1000；解析失败返回 `ok:false`（Rust 侧降级为无法判定）。

部署：`scripts/cas/README.md`（uv 托管 Python 3.12 + sympy 1.14.0）。
"""

from __future__ import annotations

import json
import re
import sys

try:
    import sympy
    from sympy.parsing.sympy_parser import (
        convert_xor,
        implicit_multiplication_application,
        parse_expr,
        standard_transformations,
    )
except Exception as exc:  # pragma: no cover - 部署缺失时由 Rust 侧降级
    print(json.dumps({"ok": False, "error": f"sympy 不可用: {exc}"}, ensure_ascii=False), flush=True)
    sys.exit(2)

PROTOCOL = "1"
MAX_INPUT_CHARS = 1000
ALLOWED_CHARS = re.compile(r"^[A-Za-z0-9_+\-*/().,\s^]+$")
ALLOWED_NAMES = {
    "sin",
    "cos",
    "tan",
    "cot",
    "sec",
    "csc",
    "asin",
    "acos",
    "atan",
    "log",
    "ln",
    "lg",
    "exp",
    "sqrt",
    "root",
    "Abs",
    "pi",
}
FUNCTIONS = {
    "sin": sympy.sin,
    "cos": sympy.cos,
    "tan": sympy.tan,
    "cot": sympy.cot,
    "sec": sympy.sec,
    "csc": sympy.csc,
    "asin": sympy.asin,
    "acos": sympy.acos,
    "atan": sympy.atan,
    "log": sympy.log,
    "ln": sympy.log,
    "lg": sympy.log,
    "exp": sympy.exp,
    "sqrt": sympy.sqrt,
    "root": lambda value, degree: sympy.root(value, degree),
    "Abs": sympy.Abs,
    "pi": sympy.pi,
}
TRANSFORMATIONS = (
    standard_transformations + (implicit_multiplication_application, convert_xor)
)


def safe_parse(text: str):
    if not isinstance(text, str):
        raise ValueError("表达式必须是字符串")
    if len(text) > MAX_INPUT_CHARS:
        raise ValueError("表达式超过 1000 字符")
    if not ALLOWED_CHARS.match(text):
        raise ValueError("表达式含不允许的字符")
    if "__" in text:
        raise ValueError("表达式含不允许的标识符")
    for name in re.findall(r"[A-Za-z_][A-Za-z0-9_]*", text):
        if name not in ALLOWED_NAMES and len(name) != 1 and name != "pi":
            raise ValueError(f"不允许的标识符: {name}")
    local_dict = dict(FUNCTIONS)
    return parse_expr(text, local_dict=local_dict, transformations=TRANSFORMATIONS, evaluate=True)


def response(payload: dict) -> None:
    payload.setdefault("protocol", PROTOCOL)
    payload.setdefault("sympy", sympy.__version__)
    print(json.dumps(payload, ensure_ascii=False), flush=True)


def handle(request: dict) -> None:
    op = request.get("op")
    if op == "ping":
        response({"ok": True})
        return
    if op == "simplify_zero":
        lhs = safe_parse(request.get("lhs", ""))
        rhs = safe_parse(request.get("rhs", ""))
        diff = sympy.simplify(lhs - rhs)
        zero = diff == 0
        constant = not diff.free_symbols
        response(
            {
                "ok": True,
                "zero": bool(zero),
                "constant": bool(constant),
                "diff": sympy.sstr(diff),
            }
        )
        return
    if op == "matrix_det":
        rows = request.get("rows", [])
        matrix = sympy.Matrix([[safe_parse(cell) for cell in row] for row in rows])
        response({"ok": True, "det": sympy.sstr(matrix.det())})
        return
    response({"ok": False, "error": f"未实现 op: {op}"})


def main() -> None:
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            request = json.loads(line)
            if not isinstance(request, dict):
                raise ValueError("请求必须是 JSON 对象")
            handle(request)
        except Exception as exc:  # 解析/计算失败 → Rust 侧降级
            response({"ok": False, "error": str(exc)})


if __name__ == "__main__":
    main()
