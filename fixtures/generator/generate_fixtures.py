#!/usr/bin/env python3
"""生成仓库级端到端合成夹具（`fixtures/paper-20q`）。

确定性要求：
- 同一台机器 + 同一 Pillow/字体版本重复运行，输出图片字节稳定（JPEG/PNG 均不带时间戳）；
- 所有 id/时间/文本来自本文件常量，不使用随机数或当前时间；
- 生成后用 `fixtures/generator/keygen`（Rust）计算 eg-inference 夹具键并写
  `fixtures/MANIFEST.sha256`；再做 `verify_fixtures.py` 自检。

诚实声明：图片内容是**印刷体合成**（系统字体渲染），不是真实学生手写；不包含任何真实
姓名/学校/试卷原文，仅用于软件自动化测试。
"""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import struct
import subprocess
import sys
from pathlib import Path

try:
    from PIL import Image, ImageDraw, ImageFont
except ImportError:  # pragma: no cover - 依赖提示
    sys.stderr.write("需要 Pillow：python3 -m pip install --user pillow\n")
    raise SystemExit(2)

# ---------------------------------------------------------------------------
# 路径与常量
# ---------------------------------------------------------------------------

REPO = Path(__file__).resolve().parents[2]
FIX = REPO / "fixtures"
BUILD = Path(os.environ.get("FIXTURES_BUILD_DIR", "/tmp/exam-grading-fixtures-build"))

ALPHABET = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"
TS = "2026-11-04T00:00:00Z"
PAGE_W, PAGE_H = 2480, 3507  # A4 @ 300 DPI 等价尺寸
OCR_LONG_EDGE = 2000  # ocr.input_max_edge 默认上限
MARGIN_X = 150
FIRST_Y = 150
ROW_H = 120
FONT_SIZE = 64
PAPER_BG = (222, 222, 222)   # 浅灰纸面：避免纯白触发 capture 的高光/过曝拒收
INK = (30, 30, 30)
RULE_INK = (198, 198, 198)
HEADER_SIZE = 44


def ulid(value: int) -> str:
    """128 位整数 → 26 字符 Crockford Base32 ULID（大写，与 eg-domain Ulid Display 一致）。"""
    chars = []
    n = value
    for _ in range(26):
        chars.append(ALPHABET[n & 31])
        n >>= 5
    return "".join(reversed(chars))


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    return sha256_bytes(path.read_bytes())


CLASROOM_ID = ulid(2)
PAPER_ID = ulid(1)
QUESTION_IDS = {ordinal: ulid(1000 + ordinal) for ordinal in range(1, 21)}
RUBRIC_IDS = {ordinal: ulid(2000 + ordinal) for ordinal in (9, 10, 11, 16, 17, 18, 19)}
RUBRIC_ITEM_IDS = {
    ordinal: [ulid(3000 + ordinal * 10 + index) for index in range(1, 6)]
    for ordinal in (9, 10, 11, 16, 17, 18, 19)
}
STUDENT_IDS = {
    "SO2026-001": ulid(4001),
    "SO2026-002": ulid(4002),
    "SO2026-003": ulid(4003),
}
CLASSROOM = {
    "id": CLASROOM_ID,
    "name": "合成夹具班级（印刷体）",
    "term": "2026 秋",
}

# ---------------------------------------------------------------------------
# 试卷定义（20 题 / 总分 150000 毫元）
# ---------------------------------------------------------------------------

CHOICES = ["A", "B", "C", "D"]


def key_concrete(value: dict) -> dict:
    return {"concrete": {"primary": value, "alternatives": [], "comparators": []}}


def val_options(options: list[str]) -> dict:
    return {"options": options}


def val_text(text: str) -> dict:
    return {"text": text}


def val_number(value: str, absolute: str | None = None) -> dict:
    tolerance = None
    if absolute is not None:
        tolerance = {"absolute": absolute, "relative_ppm": None}
    return {"number": {"value": value, "unit": None, "tolerance": tolerance}}


def val_expression(latex: str) -> dict:
    return {"expression": latex}


def key_rubric_only() -> str:
    # serde 单元变体序列化为字符串（不是 {"rubric_only": null}）。
    return "rubric_only"


def key_omitted(note: str) -> dict:
    return {"explicitly_omitted": {"note": note}}


def item(item_id: str, criterion: str, points: int, depends_on: list[str] | None = None) -> dict:
    return {
        "id": item_id,
        "criterion": criterion,
        "points": points,
        "required": True,
        "depends_on": depends_on or [],
    }


def origin() -> dict:
    return {
        "teacher_handwritten": {
            "confirmed_by": "local-teacher",
            "confirmed_at": TS,
        }
    }


def partial_choice_rule(correct_count: int, tiers: list[tuple[int, int, str]], total: int) -> dict:
    return {
        "correct_option_count": correct_count,
        "tiers": [
            {"selected_correct": selected, "points": points, "label": label}
            for selected, points, label in tiers
        ],
        "wrong_selection_zero": True,
        "source": "template_data",
        "source_note": None,
        "total_milli": total,
    }


def rubric(
    ordinal: int,
    mode: str,
    items: list[dict],
    total: int,
    partial_choice: dict | None = None,
) -> dict:
    return {
        "id": RUBRIC_IDS[ordinal],
        "version": 1,
        "items": items,
        "mode": mode,
        "total": total,
        "origin": origin(),
        "partial_choice": partial_choice,
    }


# 题目清单：8 单选（5000）+ 3 多选（6000，含部分给分）+ 4 填空（5000）
# + 3 计算/解答（13000/15000/17000，分步 rubric）+ 1 证明（15000，仅规则）
# + 1「略」（12000，明确省略） = 150000 毫元。
QUESTIONS = [
    ("single_choice", 5000, key_concrete(val_options(["A"])), None, ["集合"]),
    ("single_choice", 5000, key_concrete(val_options(["B"])), None, ["函数"]),
    ("single_choice", 5000, key_concrete(val_options(["C"])), None, ["函数"]),
    ("single_choice", 5000, key_concrete(val_options(["D"])), None, ["三角"]),
    ("single_choice", 5000, key_concrete(val_options(["A"])), None, ["数列"]),
    ("single_choice", 5000, key_concrete(val_options(["B"])), None, ["数列"]),
    ("single_choice", 5000, key_concrete(val_options(["C"])), None, ["向量"]),
    ("single_choice", 5000, key_concrete(val_options(["D"])), None, ["立体几何"]),
    ("multiple_choice", 6000, key_concrete(val_options(["A", "C"])), None, ["概率"]),
    ("multiple_choice", 6000, key_concrete(val_options(["A", "B", "D"])), None, ["概率"]),
    ("multiple_choice", 6000, key_concrete(val_options(["B", "C"])), None, ["统计"]),
    ("fill_in_blank", 5000, key_concrete(val_number("2.5", "0.1")), None, ["函数"]),
    ("fill_in_blank", 5000, key_concrete(val_number("-0.75")), None, ["三角"]),
    ("fill_in_blank", 5000, key_concrete(val_expression("x^{2}-1")), None, ["函数"]),
    ("fill_in_blank", 5000, key_concrete(val_text("正确")), None, ["逻辑"]),
    ("calculation", 13000, key_concrete(val_number("12")), "stepwise", ["计算"]),
    ("calculation", 15000, key_rubric_only(), "stepwise", ["解答"]),
    ("calculation", 17000, key_concrete(val_expression("2x+3")), "stepwise", ["解答"]),
    ("proof", 15000, key_rubric_only(), "additive", ["证明"]),
    ("short_answer", 12000, key_omitted("略"), None, ["综合"]),
]

PARTIAL_CHOICE = {
    9: partial_choice_rule(2, [(1, 3000, "选对 1 个")], 6000),
    10: partial_choice_rule(3, [(1, 2000, "选对 1 个"), (2, 4000, "选对 2 个")], 6000),
    11: partial_choice_rule(2, [(1, 3000, "选对 1 个")], 6000),
}

STEP_RUBRICS = {
    16: [
        item(RUBRIC_ITEM_IDS[16][0], "列出方程并正确代入", 4000),
        item(RUBRIC_ITEM_IDS[16][1], "完成求解步骤", 5000, [RUBRIC_ITEM_IDS[16][0]]),
        item(RUBRIC_ITEM_IDS[16][2], "写出最终答案", 4000, [RUBRIC_ITEM_IDS[16][1]]),
    ],
    17: [
        item(RUBRIC_ITEM_IDS[17][0], "因式分解正确", 5000),
        item(RUBRIC_ITEM_IDS[17][1], "求出两个零点", 5000, [RUBRIC_ITEM_IDS[17][0]]),
        item(RUBRIC_ITEM_IDS[17][2], "写出完整结论", 5000, [RUBRIC_ITEM_IDS[17][1]]),
    ],
    18: [
        item(RUBRIC_ITEM_IDS[18][0], "合并同类项", 6000),
        item(RUBRIC_ITEM_IDS[18][1], "化简过程正确", 6000, [RUBRIC_ITEM_IDS[18][0]]),
        item(RUBRIC_ITEM_IDS[18][2], "写出最终表达式", 5000, [RUBRIC_ITEM_IDS[18][1]]),
    ],
    19: [
        item(RUBRIC_ITEM_IDS[19][0], "写出已知与求证", 6000),
        item(RUBRIC_ITEM_IDS[19][1], "关键推理步骤", 5000),
        item(RUBRIC_ITEM_IDS[19][2], "结论与证毕", 4000),
    ],
}


def build_question(ordinal: int) -> dict:
    kind, max_score, key, rubric_kind, tags = QUESTIONS[ordinal - 1]
    rubric_json = None
    if rubric_kind == "stepwise":
        rubric_json = rubric(
            ordinal,
            "stepwise",
            STEP_RUBRICS[ordinal],
            max_score,
        )
    elif rubric_kind == "additive":
        rubric_json = rubric(ordinal, "additive", STEP_RUBRICS[ordinal], max_score)
    elif ordinal in PARTIAL_CHOICE:
        rubric_json = rubric(
            ordinal,
            "partial_choice",
            [item(RUBRIC_ITEM_IDS[ordinal][0], "全对给满分；按档位部分给分", max_score)],
            max_score,
            PARTIAL_CHOICE[ordinal],
        )
    return {
        "id": QUESTION_IDS[ordinal],
        "paper_id": PAPER_ID,
        "ordinal": ordinal,
        "label": str(ordinal),
        "kind": kind,
        "max_score": max_score,
        "stem": None,
        "key": key,
        "rubric": rubric_json,
        "subject_hint": "math",
        "knowledge_tags": tags,
    }


def build_paper() -> dict:
    questions = [build_question(ordinal) for ordinal in range(1, 21)]
    total = sum(question["max_score"] for question in questions)
    assert total == 150000, total
    return {
        "id": PAPER_ID,
        "classroom_id": CLASROOM_ID,
        "title": "合成夹具·数学 20 题（印刷体）",
        "subject": "math",
        "total_score": total,
        "questions": questions,
        "key_version": 1,
        "rubric_version": 1,
        "created_at": TS,
    }


# ---------------------------------------------------------------------------
# 名单与作答内容
# ---------------------------------------------------------------------------

FORMAT_FOR = {
    "SO2026-001": "png",
    "SO2026-002": "jpg",
    "SO2026-003": "png",
}

STUDENTS = [
    {"student_no": "SO2026-001", "name": "张三", "file_stem": "张三"},
    {"student_no": "SO2026-002", "name": "李四", "file_stem": "李四_期中"},
    {"student_no": "SO2026-003", "name": "王五", "file_stem": "SO2026-003"},
]

# 每题作答：文本行（页面打印内容 = OCR 夹具内容 = 期望判分依据）。
# 行格式：(kind, text, latex) —— kind=text/formula。
ANSWERS = {
    "SO2026-001": {
        1: "A", 2: "B", 3: "C", 4: "D", 5: "A", 6: "B", 7: "C", 8: "D",
        9: "AC", 10: "ABD", 11: "A",
        12: "2.5", 13: "-0.75", 14: "x^{2}-1", 15: "正确",
        16: "12",
        17: [
            ("formula", "x^{2}-1=(x-1)(x+1)", "x^{2}-1=(x-1)(x+1)"),
            ("text", "所以零点为 x=1 或 x=-1。", None),
        ],
        18: "2x+3",
        19: [("text", "证明：由已知条件可得结论成立。", None), ("text", "证毕。", None)],
        20: "略",
    },
    "SO2026-002": {
        1: "A", 2: "B", 3: "C", 4: "D", 5: "A", 6: "C", 7: "C", 8: "A",
        9: "A", 10: "A", 11: "B",
        12: "2.5", 13: "-0.8", 14: "x^{2}-2", 15: "正确",
        16: "10",
        17: [("text", "由 x^{2}-1 得 x=±1。", None)],
        18: "2x+2",
        19: [("text", "证明：由已知条件可得结论成立。", None)],
        20: "略",
    },
    "SO2026-003": {
        1: "A", 2: "B", 3: "C", 4: "D", 5: "A", 6: "C", 7: "C", 8: "D",
        9: "AC", 10: "ABD", 11: "BC",
        12: "2.5", 13: "-0.75", 14: "x^{2}-1", 15: "错误",
        16: "12",
        17: [("formula", "x^{2}-1=(x-1)(x+1)", "x^{2}-1=(x-1)(x+1)"), ("text", "所以零点为 x=±1。", None)],
        18: "2x+3",
        19: [("text", "证明：由已知条件可得结论成立。", None), ("text", "关键推理成立，证毕。", None)],
        20: "略",
    },
}

# OCR 置信度覆盖（(student_no, ordinal) → confidence）；低置信样例用于复核路径。
OCR_CONFIDENCE = {
    ("SO2026-002", 17): 0.55,
    ("SO2026-003", 16): 0.72,
}

# 第 4 份「不规范命名」样卷：名单外学生，只作答第 1–5 题（用于未决/提醒路径）。
IRREGULAR = {
    "file": "images/IMG_20261104_001.jpg",
    "header": "姓名：赵六　学号：SO2026-004　（名单外样卷）",
    "answers": {1: "A", 2: "B", 3: "C", 4: "D", 5: "A"},
}

PAGE_1_ORDINALS = list(range(1, 12))
PAGE_2_ORDINALS = list(range(12, 21))


# 印刷体数学用 ASCII 记号（^ 表示幂）：系统字体缺上标字形，避免图片与 OCR 文本不一致。
PRINTED_MATH = {
    "x^{2}-1": "x^2 - 1",
    "x^{2}-1=(x-1)(x+1)": "x^2 - 1 = (x - 1)(x + 1)",
}


def answer_lines(student_no: str, ordinal: int) -> list[tuple[str, str, str | None]]:
    raw = ANSWERS[student_no][ordinal]
    if isinstance(raw, str):
        if ordinal == 14:
            return [("formula", f"{ordinal}. {PRINTED_MATH.get(raw, raw)}", raw)]
        return [("text", f"{ordinal}. {raw}", None)]
    lines = []
    for index, (kind, text, latex) in enumerate(raw):
        prefix = f"{ordinal}. " if index == 0 else "　　"
        printed = PRINTED_MATH.get(text, text) if kind == "formula" else text
        lines.append((kind, prefix + printed, latex))
    return lines


# ---------------------------------------------------------------------------
# 图片渲染（确定性）
# ---------------------------------------------------------------------------

FONT_CANDIDATES = [
    "/System/Library/Fonts/Supplemental/Songti.ttc",
    "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    "/System/Library/Fonts/STHeiti Medium.ttc",
]


def resolve_font() -> str:
    override = os.environ.get("FIXTURES_FONT")
    if override:
        return override
    for path in FONT_CANDIDATES:
        if Path(path).is_file():
            return path
    raise SystemExit(
        "找不到中文字体；请设置 FIXTURES_FONT=/path/to/font.ttf(.ttc)"
    )


def scale_blocks(blocks: list[dict], sx: float, sy: float) -> list[dict]:
    scaled = []
    for block in blocks:
        x, y, w, h = block["bbox"]
        scaled.append(
            {
                **block,
                "bbox": [round(x * sx, 4), round(y * sy, 4), round(w * sx, 4), round(h * sy, 4)],
            }
        )
    return scaled


def render_page(
    header: str,
    lines: list[tuple[str, str, str | None]],
) -> tuple[Image.Image, list[dict], list[tuple[int, tuple[float, float, float, float]]]]:
    """渲染一页答题纸并返回（图片, OCR 块, 逐题行 bbox 列表）。

    版式：页眉 + 横线答题纸；每条作答内容占一个横线格。空余横线保留（更像真实答题纸，
    也让质量门控的「行结构」有稳定输入）。返回的 bbox 为源图像素坐标，供模板区域使用。
    """
    font_path = resolve_font()
    font = ImageFont.truetype(font_path, FONT_SIZE)
    header_font = ImageFont.truetype(font_path, HEADER_SIZE)
    image = Image.new("RGB", (PAGE_W, PAGE_H), PAPER_BG)
    draw = ImageDraw.Draw(image)

    # 页眉。
    draw.text((MARGIN_X, FIRST_Y), header, font=header_font, fill=INK)
    hb = header_font.getbbox(header)
    header_bbox = (
        float(MARGIN_X + hb[0]),
        float(FIRST_Y + hb[1]),
        float(MARGIN_X + hb[2]),
        float(FIRST_Y + hb[3]),
    )

    grid_top = FIRST_Y + 170
    row_h = ROW_H
    rows = int((PAGE_H - 220 - grid_top) // row_h)
    if len(lines) > rows:
        raise SystemExit(f"页面内容 {len(lines)} 行超过答题纸 {rows} 行，请调整版式")

    # 横线（答题纸风格）。
    for index in range(rows + 1):
        y = grid_top + index * row_h
        draw.line((MARGIN_X, y, PAGE_W - MARGIN_X, y), fill=RULE_INK, width=3)

    blocks: list[dict] = [
        {
            "kind": "header_footer",
            "bbox": norm_bbox(
                header_bbox[0], header_bbox[1], header_bbox[2] - header_bbox[0], header_bbox[3] - header_bbox[1]
            ),
            "text": header,
            "confidence": 0.99,
        }
    ]
    question_boxes: dict[int, list[tuple[float, float, float, float]]] = {}
    current_question: int | None = None
    for index, (kind, text, latex) in enumerate(lines):
        y = grid_top + index * row_h - 64
        draw.text((MARGIN_X, y), text, font=font, fill=INK)
        box = font.getbbox(text)
        x0 = float(MARGIN_X + box[0])
        y0 = float(y + box[1])
        x1 = float(MARGIN_X + box[2])
        y1 = float(y + box[3])
        blocks.append(
            {
                "kind": "formula" if kind == "formula" else "text",
                "bbox": norm_bbox(x0, y0, x1 - x0, y1 - y0),
                "text": text,
                "latex": latex,
                "confidence": 0.97,
            }
        )
        digits = ""
        for char in text:
            if char.isdigit():
                digits += char
            else:
                break
        if digits:
            current_question = int(digits)
            question_boxes.setdefault(current_question, []).append((x0, y0, x1, y1))
        elif current_question is not None:
            question_boxes.setdefault(current_question, []).append((x0, y0, x1, y1))

    per_question: list[tuple[int, tuple[float, float, float, float]]] = []
    for ordinal, boxes in sorted(question_boxes.items()):
        x0 = min(box[0] for box in boxes)
        y0 = min(box[1] for box in boxes)
        x1 = max(box[2] for box in boxes)
        y1 = max(box[3] for box in boxes)
        per_question.append((ordinal, (x0, y0, x1, y1)))
    return image, blocks, per_question


def norm_bbox(x: float, y: float, w: float, h: float) -> list[float]:
    """像素 bbox → 归一化 bbox（保留 4 位小数；确保在页面内）。"""
    nx = round(x / PAGE_W, 4)
    ny = round(y / PAGE_H, 4)
    nw = round(w / PAGE_W, 4)
    nh = round(h / PAGE_H, 4)
    nx = min(max(nx, 0.0), 0.9999)
    ny = min(max(ny, 0.0), 0.9999)
    nw = min(max(nw, 0.0001), 1.0 - nx)
    nh = min(max(nh, 0.0001), 1.0 - ny)
    return [nx, ny, round(nw, 4), round(nh, 4)]


# ---------------------------------------------------------------------------
# EXIF（仅 Orientation=6）写入
# ---------------------------------------------------------------------------


def exif_orientation_6_app1() -> bytes:
    """构造最小 EXIF APP1 段：TIFF 小端 + IFD0 一个 Orientation(0x0112)=6 条目。"""
    tiff = bytearray()
    tiff += b"II"  # little endian
    tiff += struct.pack("<HI", 42, 8)  # magic, IFD0 offset
    tiff += struct.pack("<H", 1)  # 1 entry
    tiff += struct.pack("<HHIHH", 0x0112, 3, 1, 6, 0)  # SHORT count=1 value=6 (2 bytes + pad)
    tiff += struct.pack("<I", 0)  # next IFD = 0
    payload = b"Exif\x00\x00" + bytes(tiff)
    return struct.pack(">H", len(payload) + 2) + payload


def jpeg_with_exif(image: Image.Image, quality: int = 82) -> bytes:
    from io import BytesIO

    buffer = BytesIO()
    image.save(buffer, format="JPEG", quality=quality, subsampling=0, optimize=True)
    data = bytearray(buffer.getvalue())
    assert data[:2] == b"\xff\xd8"
    app1 = exif_orientation_6_app1()
    return bytes(data[:2]) + b"\xff\xe1" + app1 + bytes(data[2:])


def save_png(image: Image.Image) -> bytes:
    from io import BytesIO

    buffer = BytesIO()
    image.save(buffer, format="PNG", optimize=True, compress_level=9)
    return buffer.getvalue()


def save_jpeg(image: Image.Image, quality: int = 82) -> bytes:
    from io import BytesIO

    buffer = BytesIO()
    image.save(buffer, format="JPEG", quality=quality, subsampling=0, optimize=True)
    return buffer.getvalue()


def ocr_input_bytes(image: Image.Image) -> tuple[bytes, int, int]:
    """生成识别输入图（长边 ≤ ocr.input_max_edge，灰度 PNG）。"""
    scale = OCR_LONG_EDGE / max(image.size)
    size = (max(1, round(image.width * scale)), max(1, round(image.height * scale)))
    small = image.convert("L").resize(size, Image.LANCZOS)
    return save_png(small), small.width, small.height


# ---------------------------------------------------------------------------
# 期望判分（由作答内容推导；模型判定项显式给出）
# ---------------------------------------------------------------------------

EXPECTED_JUDGE = {
    # (student_no, ordinal): dict(judgement, score_milli, satisfied, missing, confidence, calibrated, reason)
    ("SO2026-001", 17): dict(judgement="match", score_milli=15000, satisfied=None, missing=None,
                              confidence=0.96, calibrated=True, reason="三个步骤均满足"),
    ("SO2026-002", 17): dict(judgement="partial", score_milli=10000,
                              satisfied=[RUBRIC_ITEM_IDS[17][0], RUBRIC_ITEM_IDS[17][1]],
                              missing=[RUBRIC_ITEM_IDS[17][2]],
                              confidence=0.62, calibrated=True, reason="缺最终结论步骤（低置信）"),
    ("SO2026-003", 17): dict(judgement="match", score_milli=15000, satisfied=None, missing=None,
                              confidence=0.95, calibrated=True, reason="三个步骤均满足"),
    ("SO2026-001", 19): dict(judgement="match", score_milli=15000, satisfied=None, missing=None,
                              confidence=0.94, calibrated=True, reason="证明三项均满足"),
    ("SO2026-002", 19): dict(judgement="partial", score_milli=10000,
                              satisfied=[RUBRIC_ITEM_IDS[19][0], RUBRIC_ITEM_IDS[19][2]],
                              missing=[RUBRIC_ITEM_IDS[19][1]],
                              confidence=0.90, calibrated=True, reason="缺关键推理步骤"),
    ("SO2026-003", 19): dict(judgement="partial", score_milli=9000,
                              satisfied=[RUBRIC_ITEM_IDS[19][1], RUBRIC_ITEM_IDS[19][2]],
                              missing=[RUBRIC_ITEM_IDS[19][0]],
                              confidence=0.90, calibrated=True, reason="缺已知与求证书写"),
    ("SO2026-002", 16): dict(judgement="partial", score_milli=9000,
                              satisfied=[RUBRIC_ITEM_IDS[16][0], RUBRIC_ITEM_IDS[16][1]],
                              missing=[RUBRIC_ITEM_IDS[16][2]],
                              confidence=0.93, calibrated=True, reason="前两步正确，答案错误"),
}

# 王五第 16 题：规则判为一致、模型判为不一致 → 冲突未决（expected 记 null）。
CONFLICT = ("SO2026-003", 16)


def norm_key(value: str) -> str:
    return "".join(ch for ch in value.upper() if ch.isalnum())


def selected_correct_count(answer: str, correct: list[str]) -> tuple[int, bool]:
    selected = [char for char in answer.upper() if char in CHOICES]
    wrong = [char for char in selected if char not in correct]
    correct_selected = [char for char in selected if char in correct]
    return len(correct_selected), bool(wrong)


def expected_question(student_no: str, ordinal: int) -> dict:
    kind, max_score, key, rubric_kind, _tags = QUESTIONS[ordinal - 1]
    answer = ANSWERS[student_no][ordinal]
    answer_text = answer if isinstance(answer, str) else answer[0][1]
    base = {
        "question_id": QUESTION_IDS[ordinal],
        "ordinal": ordinal,
        "kind": kind,
        "max_score_milli": max_score,
        "route": "rule_only",
        "judgement": None,
        "score_milli": None,
        "status": "auto_accepted",
        "review_reason": None,
        "manual_reason": None,
        "confidence": 1.0,
        "calibrated": True,
        "satisfied": [],
        "missing": [],
        "evidence": "rule",
        "reason": "",
    }
    if ordinal <= 8:
        correct = key["concrete"]["primary"]["options"]
        match = norm_key(answer_text) == norm_key(correct[0])
        base["judgement"] = "match" if match else "mismatch"
        base["score_milli"] = max_score if match else 0
        base["reason"] = "选项与答案键一致" if match else "选项与答案键不一致"
        return base
    if 9 <= ordinal <= 11:
        correct = key["concrete"]["primary"]["options"]
        chosen, wrong = selected_correct_count(answer_text, correct)
        if not wrong and chosen == len(correct):
            base["judgement"] = "match"
            base["score_milli"] = max_score
            base["reason"] = "全部选对"
        elif wrong:
            base["judgement"] = "mismatch"
            base["score_milli"] = 0
            base["reason"] = "存在错选，按规则归零"
        else:
            rule = PARTIAL_CHOICE[ordinal]
            tier = next(tier for tier in rule["tiers"] if tier["selected_correct"] == chosen)
            base["judgement"] = "partial"
            base["score_milli"] = tier["points"]
            base["reason"] = f"选对 {chosen} 个，按档位部分给分"
        return base
    if 12 <= ordinal <= 15:
        correct_text = {
            12: "2.5",
            13: "-0.75",
            14: "x^{2}-1",
            15: "正确",
        }[ordinal]
        if ordinal == 12:
            match = abs(float(answer_text) - 2.5) <= 0.1
        elif ordinal == 13:
            match = abs(float(answer_text) - (-0.75)) <= 1e-9
        else:
            match = norm_key(answer_text) == norm_key(correct_text)
        base["judgement"] = "match" if match else "mismatch"
        base["score_milli"] = max_score if match else 0
        base["reason"] = "与答案键一致" if match else "与答案键不一致"
        return base
    if ordinal == 16:
        if (student_no, ordinal) == CONFLICT:
            base.update(
                route="rubric_itemized",
                judgement=None,
                score_milli=None,
                status="needs_review",
                review_reason="conflict",
                confidence=0.55,
                calibrated=False,
                evidence="rule+judge",
                reason="规则判为一致，模型判为不一致：冲突未解决，转人工",
            )
            return base
        case = EXPECTED_JUDGE.get((student_no, ordinal))
        if case:
            base.update(
                route="rubric_itemized",
                judgement=case["judgement"],
                score_milli=case["score_milli"],
                satisfied=case["satisfied"] or [],
                missing=case["missing"] or [],
                confidence=case["confidence"],
                calibrated=case["calibrated"],
                evidence="judge",
                reason=case["reason"],
            )
            return base
        match = norm_key(answer_text) == norm_key("12")
        base.update(
            judgement="match" if match else "mismatch",
            score_milli=max_score if match else 0,
            reason="计算结果与答案键一致" if match else "计算结果与答案键不一致",
        )
        return base
    if ordinal in (17, 18, 19):
        if ordinal == 18:
            match = norm_key(answer_text) == norm_key("2x+3")
            base.update(
                route="rubric_itemized",
                judgement="match" if match else "mismatch",
                score_milli=max_score if match else 0,
                evidence="judge",
                confidence=0.95,
                reason="表达式与答案键一致" if match else "表达式与答案键不一致",
            )
            return base
        case = EXPECTED_JUDGE[(student_no, ordinal)]
        base.update(
            route="rubric_itemized",
            judgement=case["judgement"],
            score_milli=case["score_milli"],
            satisfied=case["satisfied"] or [],
            missing=case["missing"] or [],
            confidence=case["confidence"],
            calibrated=case["calibrated"],
            evidence="judge",
            reason=case["reason"],
        )
        if case["confidence"] < 0.75:
            base["status"] = "needs_review"
            base["review_reason"] = "low_confidence"
        return base
    # Q20：答案键「明确省略」→ 仅人工，不得自动判分。
    base.update(
        route="manual",
        judgement="ungradable",
        score_milli=None,
        status="needs_review",
        review_reason="ungradable",
        manual_reason="answer_key_omitted",
        confidence=0.0,
        calibrated=False,
        evidence="manual",
        reason="答案键为「略」（明确省略），按规范只能人工判分",
    )
    return base


def build_expected(paper: dict) -> dict:
    students = []
    for student in STUDENTS:
        questions = [expected_question(student["student_no"], ordinal) for ordinal in range(1, 21)]
        total = sum(question["score_milli"] or 0 for question in questions)
        needs_review = any(question["status"] == "needs_review" for question in questions)
        students.append(
            {
                "student_no": student["student_no"],
                "name": student["name"],
                "identity": {
                    "status": "confirmed",
                    "source": "file_name",
                    "stem": student["file_stem"],
                    "confidence": 1.0,
                },
                "submission_files": [
                    f"images/{student['file_stem']}_p1.{FORMAT_FOR[student['student_no']]}",
                    f"images/{student['file_stem']}_p2.{FORMAT_FOR[student['student_no']]}",
                ],
                "expected": {
                    "status": "needs_review" if needs_review else "auto_accepted",
                    "total_milli": total,
                    "max_total_milli": paper["total_score"],
                    "questions": questions,
                },
            }
        )
    return {
        "schema_version": 1,
        "generated_by": "fixtures/generator/generate_fixtures.py",
        "paper": {
            "id": paper["id"],
            "title": paper["title"],
            "total_score_milli": paper["total_score"],
            "key_version": paper["key_version"],
            "rubric_version": paper["rubric_version"],
            "question_count": len(paper["questions"]),
            "page_count": 2,
        },
        "mode": "standard",
        "rubric_status": "active",
        "students": students,
        "extra_samples": [
            {
                "file": IRREGULAR["file"],
                "expected": {
                    "identity": "unresolved",
                    "identity_reason": "file_name_not_found",
                    "grade": "not_run",
                    "note": "相机文件名不在名单：必须人工选择后才可判分（默认 OCR 兜底关闭）",
                },
            },
            {
                "file": "images/张三_p2-copy.png",
                "expected": {
                    "identity": "confirmed",
                    "grade": "not_run",
                    "duplicate": "same_batch_duplicate",
                    "note": "与 images/张三_p2.png 字节完全相同：批内去重，不新建提交",
                },
            },
            {
                "file": "images/王五_exif6.jpg",
                "expected": {
                    "identity": "confirmed",
                    "grade": "not_run",
                    "exif_orientation": 6,
                    "rotation_applied": 90,
                    "note": "EXIF Orientation=6 旋转样例：capture 需校正为正向后再识别",
                },
            },
        ],
        "notes": [
            "图片为程序渲染的印刷体，不是真实手写；不含任何真实学生/学校信息。",
            "Q16 王五 为「规则一致 / 模型不一致」冲突未决样例；Q17 李四 为低置信复核样例。",
            "Q20 答案键为「明确省略（略）」：只能人工判分，expected 记 ungradable + needs_review。",
        ],
    }


# ---------------------------------------------------------------------------
# 输出文件
# ---------------------------------------------------------------------------


def write_json(path: Path, value) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    text = json.dumps(value, ensure_ascii=False, indent=2) + "\n"
    path.write_text(text, encoding="utf-8")


def write_bytes(path: Path, data: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)


def build_roster_csv() -> str:
    lines = ["student_no,name,file_stem,expected_identity,note"]
    notes = {
        "SO2026-001": "规范命名（姓名 + 页码后缀），PNG",
        "SO2026-002": "规范命名（姓名_期中 + 页码后缀），JPEG",
        "SO2026-003": "学号命名（学号 + 页码后缀），PNG；另有 EXIF 旋转样例",
    }
    for student in STUDENTS:
        lines.append(
            f"{student['student_no']},{student['name']},{student['file_stem']},confirmed,{notes[student['student_no']]}"
        )
    return "\n".join(lines) + "\n"


def build_students() -> list[dict]:
    return [
        {
            "id": STUDENT_IDS[student["student_no"]],
            "classroom_id": CLASROOM_ID,
            "student_no": student["student_no"],
            "name": student["name"],
            "source": "roster_import",
        }
        for student in STUDENTS
    ]


def build_rubric_document(paper: dict) -> dict:
    questions = []
    for question in paper["questions"]:
        if question["rubric"] is None:
            continue
        questions.append(
            {
                "question_id": question["id"],
                "ordinal": question["ordinal"],
                "kind": question["kind"],
                "max_score_milli": question["max_score"],
                "rubric": question["rubric"],
            }
        )
    return {
        "schema_version": 1,
        "paper_id": PAPER_ID,
        "rubric_version": paper["rubric_version"],
        "status": "active",
        "confirmed_by": "local-teacher",
        "confirmed_at": TS,
        "note": "Rubric 领域类型本身无 status/active 字段：生效由 rubric_version + 本文件的确认信封表达（见 fixtures/README.md）。",
        "questions": questions,
    }


def build_answer_key_document(paper: dict) -> dict:
    return {
        "schema_version": 1,
        "paper_id": PAPER_ID,
        "key_version": paper["key_version"],
        "questions": [
            {
                "question_id": question["id"],
                "ordinal": question["ordinal"],
                "kind": question["kind"],
                "max_score_milli": question["max_score"],
                "answer_key": question["key"],
            }
            for question in paper["questions"]
        ],
    }


def build_template(paper: dict, regions: dict[int, dict[int, list[float]]]) -> dict:
    entries = []
    for page_no in (1, 2):
        for ordinal, bbox in sorted(regions.get(page_no, {}).items()):
            entries.append(
                {
                    "page_no": page_no,
                    "question_id": QUESTION_IDS[ordinal],
                    "bbox": bbox,
                }
            )
    return {
        "paper_id": PAPER_ID,
        "version": paper["key_version"],
        "page_count": 2,
        "regions": entries,
    }


def main() -> int:
    BUILD.mkdir(parents=True, exist_ok=True)
    paper = build_paper()
    expected = build_expected(paper)

    # 1) 静态数据文件。
    write_json(FIX / "paper-20q" / "paper.json", paper)
    write_json(FIX / "paper-20q" / "classroom.json", CLASSROOM)
    write_json(FIX / "paper-20q" / "students.json", build_students())
    write_json(FIX / "paper-20q" / "answer-key.json", build_answer_key_document(paper))
    write_json(FIX / "paper-20q" / "rubric.json", build_rubric_document(paper))
    write_json(FIX / "paper-20q" / "expected" / "grades-expected.json", expected)
    (FIX / "paper-20q" / "roster.csv").write_text(build_roster_csv(), encoding="utf-8")

    # 2) 图片 + OCR 规格 + 模板区域。
    images_dir = FIX / "images"
    ocr_inputs_dir = FIX / "ocr" / "inputs"
    if images_dir.exists():
        shutil.rmtree(images_dir)
    if ocr_inputs_dir.exists():
        shutil.rmtree(ocr_inputs_dir)
    images_dir.mkdir(parents=True)
    ocr_inputs_dir.mkdir(parents=True)

    image_entries: list[dict] = []
    ocr_specs: list[dict] = []
    regions: dict[int, dict[int, list[float]]] = {1: {}, 2: {}}

    def render_student_page(student: dict, page: int) -> tuple[Image.Image, list[dict], list]:
        header = f"姓名：{student['name']}　学号：{student['student_no']}　第 {page} 页 / 共 2 页"
        lines: list[tuple[str, str, str | None]] = []
        if page == 1:
            lines.append(("text", "一、选择题（第 1-11 题）", None))
            for ordinal in PAGE_1_ORDINALS:
                lines.extend(answer_lines(student["student_no"], ordinal))
        else:
            for ordinal in PAGE_2_ORDINALS:
                if ordinal == 12:
                    lines.append(("text", "二、填空题（第 12-15 题）", None))
                if ordinal == 16:
                    lines.append(("text", "三、解答题（第 16-20 题）", None))
                lines.extend(answer_lines(student["student_no"], ordinal))
        image, blocks, boxes = render_page(header, lines)
        # 低置信覆盖：按题号找到该题的行块。
        for ordinal, box in boxes:
            confidence = OCR_CONFIDENCE.get((student["student_no"], ordinal))
            if confidence is None:
                continue
            for block in blocks:
                if block["kind"] in ("text", "formula") and block["text"].startswith(f"{ordinal}."):
                    block["confidence"] = confidence
        return image, blocks, boxes

    for student in STUDENTS:
        fmt = FORMAT_FOR[student["student_no"]]
        for page in (1, 2):
            image, blocks, boxes = render_student_page(student, page)
            relative = f"images/{student['file_stem']}_p{page}.{fmt}"
            if fmt == "png":
                data = save_png(image)
            else:
                data = save_jpeg(image)
            write_bytes(FIX / relative, data)
            for ordinal, bbox in boxes:
                x0, y0, x1, y1 = bbox
                regions[page][ordinal] = norm_bbox(x0, y0, x1 - x0, y1 - y0)
            ocr_data, ocr_w, ocr_h = ocr_input_bytes(image)
            ocr_relative = f"ocr/inputs/{student['file_stem']}_p{page}.png"
            write_bytes(FIX / ocr_relative, ocr_data)
            ocr_specs.append(
                {
                    "id": f"{student['student_no'].lower()}-p{page}",
                    "student_no": student["student_no"],
                    "submission_stem": student["file_stem"],
                    "page_no": page,
                    "source_image": relative,
                    "source_sha256": sha256_bytes(data),
                    "ocr_input_image": ocr_relative,
                    "ocr_input_width": ocr_w,
                    "ocr_input_height": ocr_h,
                    "blocks": scale_blocks(blocks, ocr_w / PAGE_W, ocr_h / PAGE_H),
                }
            )
            image_entries.append(
                {
                    "file": relative,
                    "sha256": sha256_bytes(data),
                    "bytes": len(data),
                    "media_type": "image/png" if fmt == "png" else "image/jpeg",
                    "width": PAGE_W,
                    "height": PAGE_H,
                    "expected": {
                        "kind": "student_page",
                        "format": fmt,
                        "source_stem": f"{student['file_stem']}_p{page}",
                        "identity": "confirmed",
                        "identity_student_no": student["student_no"],
                        "page_no": page,
                        "exif_orientation": 0,
                        "rotation_applied": 0,
                        "duplicate": "unique",
                        "quality_verdict": "good_or_acceptable",
                    },
                }
            )

    # 3) 不规范命名样卷（名单外、相机文件名）。
    irregular_lines = [("text", IRREGULAR["header"], None)]
    irregular_lines.append(("text", "一、选择题（第 1-11 题）", None))
    for ordinal in range(1, 12):
        irregular_lines.extend(answer_lines("SO2026-001", ordinal))
    irregular_image, irregular_blocks, _ = render_page(IRREGULAR["header"], irregular_lines[1:])
    irregular_data = save_jpeg(irregular_image)
    write_bytes(FIX / IRREGULAR["file"], irregular_data)
    irregular_ocr, irregular_w, irregular_h = ocr_input_bytes(irregular_image)
    irregular_ocr_relative = "ocr/inputs/irregular_p1.png"
    write_bytes(FIX / irregular_ocr_relative, irregular_ocr)
    ocr_specs.append(
        {
            "id": "irregular-p1",
            "student_no": None,
            "submission_stem": "IMG_20261104_001",
            "page_no": 1,
            "source_image": IRREGULAR["file"],
            "source_sha256": sha256_bytes(irregular_data),
            "ocr_input_image": irregular_ocr_relative,
            "ocr_input_width": irregular_w,
            "ocr_input_height": irregular_h,
            "blocks": scale_blocks(irregular_blocks, irregular_w / PAGE_W, irregular_h / PAGE_H),
        }
    )
    image_entries.append(
        {
            "file": IRREGULAR["file"],
            "sha256": sha256_bytes(irregular_data),
            "bytes": len(irregular_data),
            "media_type": "image/jpeg",
            "width": PAGE_W,
            "height": PAGE_H,
            "expected": {
                "kind": "irregular_name",
                "format": "jpg",
                "source_stem": "IMG_20261104_001",
                "identity": "unresolved",
                "identity_reason": "file_name_not_found",
                "page_no": 1,
                "exif_orientation": 0,
                "rotation_applied": 0,
                "duplicate": "unique",
                "quality_verdict": "good_or_acceptable",
            },
        }
    )

    # 4) 重复字节文件（与张三 p2 完全相同）。
    duplicate_data = (FIX / "images" / "张三_p2.png").read_bytes()
    duplicate_relative = "images/张三_p2-copy.png"
    write_bytes(FIX / duplicate_relative, duplicate_data)
    image_entries.append(
        {
            "file": duplicate_relative,
            "sha256": sha256_bytes(duplicate_data),
            "bytes": len(duplicate_data),
            "media_type": "image/png",
            "width": PAGE_W,
            "height": PAGE_H,
            "expected": {
                "kind": "duplicate_bytes",
                "format": "png",
                "source_stem": "张三_p2-copy",
                "identity": "confirmed",
                "identity_student_no": "SO2026-001",
                "page_no": 2,
                "exif_orientation": 0,
                "rotation_applied": 0,
                "duplicate": "same_batch_duplicate",
                "quality_verdict": "good_or_acceptable",
            },
        }
    )

    # 5) EXIF Orientation=6 旋转样例：内容为「王五」第 2 页，存储时逆时针 90°。
    wangwu_image, _, _ = render_student_page(
        next(student for student in STUDENTS if student["student_no"] == "SO2026-003"), 2
    )
    rotated = wangwu_image.rotate(90, expand=True)  # 逆时针 90°（显示时需顺时针校正）
    exif_data = jpeg_with_exif(rotated)
    exif_relative = "images/王五_exif6.jpg"
    write_bytes(FIX / exif_relative, exif_data)
    image_entries.append(
        {
            "file": exif_relative,
            "sha256": sha256_bytes(exif_data),
            "bytes": len(exif_data),
            "media_type": "image/jpeg",
            "stored_width": rotated.width,
            "stored_height": rotated.height,
            "width": rotated.width,
            "height": rotated.height,
            "expected": {
                "kind": "exif_rotated",
                "format": "jpg",
                "source_stem": "王五_exif6",
                "identity": "confirmed",
                "identity_student_no": "SO2026-003",
                "page_no": 2,
                "exif_orientation": 6,
                "rotation_applied": 90,
                "upright_width": PAGE_W,
                "upright_height": PAGE_H,
                "duplicate": "unique",
                "quality_verdict": "good_or_acceptable",
            },
        }
    )

    write_json(FIX / "images" / "manifest.json", {
        "schema_version": 1,
        "note": "合成印刷体答卷页（非真实手写）；期望值为 capture/align 的可验证结论。",
        "page_width": PAGE_W,
        "page_height": PAGE_H,
        "files": image_entries,
    })

    write_json(FIX / "paper-20q" / "template.json", build_template(paper, regions))

    # 6) 生成 spec → 交给 Rust keygen 计算 eg-inference 夹具键并写最终夹具/清单。
    write_json(BUILD / "ocr-spec.json", {
        "schema_version": 1,
        "emit_latex": True,
        "backend": {
            "model_id": "fixture-ocr",
            "model_version": "1.0",
            "prompt_schema_version": "1",
        },
        "records": [
            {
                "id": spec["id"],
                "student_no": spec["student_no"],
                "submission_stem": spec["submission_stem"],
                "page_no": spec["page_no"],
                "source_image": spec["source_image"],
                "source_sha256": spec["source_sha256"],
                "ocr_input_image": spec["ocr_input_image"],
                "ocr_input_width": spec["ocr_input_width"],
                "ocr_input_height": spec["ocr_input_height"],
                "output_raw": json.dumps(
                    {
                        "schema_version": 1,
                        "page": {
                            "width": spec["ocr_input_width"],
                            "height": spec["ocr_input_height"],
                        },
                        "blocks": spec["blocks"],
                    },
                    ensure_ascii=False,
                    separators=(",", ":"),
                ),
            }
            for spec in ocr_specs
        ],
        "negative": [
            {
                "id": "missing-blocks",
                "note": "缺少 blocks 字段：结构校验应 Fatal 拒绝，不得触发修复重试",
                "output_raw": json.dumps({"schema_version": 1}, separators=(",", ":")),
            },
            {
                "id": "barcode-block",
                "note": "条码块属保留变体：语义校验必须拒绝（v1 不产出条码）",
                "output_raw": json.dumps(
                    {
                        "schema_version": 1,
                        "blocks": [
                            {
                                "kind": "barcode",
                                "bbox": [0.1, 0.1, 0.2, 0.05],
                                "text": "SO2026-001",
                            }
                        ],
                    },
                    ensure_ascii=False,
                    separators=(",", ":"),
                ),
            },
        ],
    })

    judge_prompt = (
        "你是数学判分器（judge）。只输出一个 JSON 对象，字段：judgement（match/partial/mismatch）、"
        "score_milli（千分之一分整数）、reason（中文理由）、confidence（0–1 校准前置信度）、"
        "calibration_id（校准记录 id，可空）、satisfied/missing（partial 时给出评分项或选项标识数组）。"
        "不得改写学生答案，不得猜测答案键；看不清或超出范围时不要伪造满分。"
    )
    judge_records = []
    for (student_no, ordinal), case in EXPECTED_JUDGE.items():
        answer = ANSWERS[student_no][ordinal]
        answer_text = answer if isinstance(answer, str) else "\n".join(text for _kind, text, _latex in answer)
        input_doc = {
            "question_id": QUESTION_IDS[ordinal],
            "ordinal": ordinal,
            "kind": QUESTIONS[ordinal - 1][0],
            "max_score_milli": QUESTIONS[ordinal - 1][1],
            "answer_key": QUESTIONS[ordinal - 1][2],
            "rubric": build_question(ordinal)["rubric"],
            "answer": answer_text,
            "recognition_confidence": OCR_CONFIDENCE.get((student_no, ordinal), 0.97),
            "mode": "standard",
        }
        input_bytes = json.dumps(input_doc, ensure_ascii=False, separators=(",", ":")).encode()
        output = {
            "judgement": case["judgement"],
            "score_milli": case["score_milli"],
            "reason": case["reason"],
            "confidence": case["confidence"],
            "matched_alternative": None,
            "calibration_id": "cal-fixture-1" if case["calibrated"] else None,
            "satisfied": case["satisfied"] or [],
            "missing": case["missing"] or [],
        }
        record_id = f"{student_no.lower()}-q{ordinal:02d}"
        case = "low_confidence" if case["confidence"] < 0.75 else "normal"
        judge_records.append(
            {
                "id": record_id,
                "student_no": student_no,
                "question_ordinal": ordinal,
                "case": case,
                "input_bytes_sha256": sha256_bytes(input_bytes),
                "input_utf8": input_bytes.decode("utf-8"),
                "output_raw": json.dumps(output, ensure_ascii=False, separators=(",", ":")),
            }
        )
    # 冲突样例：王五 Q16 模型判不一致（与规则一致冲突）。
    conflict_input = {
        "question_id": QUESTION_IDS[16],
        "ordinal": 16,
        "kind": "calculation",
        "max_score_milli": 13000,
        "answer_key": QUESTIONS[15][2],
        "rubric": build_question(16)["rubric"],
        "answer": ANSWERS["SO2026-003"][16],
        "recognition_confidence": OCR_CONFIDENCE[("SO2026-003", 16)],
        "mode": "standard",
        "case": "conflict_rule_vs_model",
    }
    conflict_bytes = json.dumps(conflict_input, ensure_ascii=False, separators=(",", ":")).encode()
    judge_records.append(
        {
            "id": "so2026-003-q16-conflict",
            "student_no": "SO2026-003",
            "question_ordinal": 16,
            "case": "conflict",
            "input_bytes_sha256": sha256_bytes(conflict_bytes),
            "input_utf8": conflict_bytes.decode("utf-8"),
            "output_raw": json.dumps(
                {
                    "judgement": "mismatch",
                    "score_milli": 0,
                    "reason": "模型认为最终答案不一致（与规则判定冲突）",
                    "confidence": 0.55,
                    "matched_alternative": None,
                    "calibration_id": None,
                    "satisfied": [],
                    "missing": [],
                },
                ensure_ascii=False,
                separators=(",", ":"),
            ),
        }
    )
    write_json(BUILD / "judge-spec.json", {
        "schema_version": 1,
        "backend": {
            "model_id": "fixture-judge",
            "model_version": "1.0",
            "prompt_schema_version": "judge.outcome.v1",
        },
        "prompt": judge_prompt,
        "records": judge_records,
        "negative": [
            {
                "id": "match-with-wrong-score",
                "note": "judgement=match 但 score_milli=0：Schema 校验必须拒绝",
                "output_raw": json.dumps(
                    {
                        "judgement": "match",
                        "score_milli": 0,
                        "reason": "非法样例",
                        "confidence": 0.9,
                    },
                    ensure_ascii=False,
                    separators=(",", ":"),
                ),
            }
        ],
    })

    print(f"[ok] 静态夹具已生成：{FIX}")
    print(f"[ok] 待 Rust keygen 处理的规格：{BUILD}/ocr-spec.json, judge-spec.json")
    print("[next] 运行 fixtures/generator/regenerate.sh（或 cargo run keygen）生成夹具键与 MANIFEST")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
