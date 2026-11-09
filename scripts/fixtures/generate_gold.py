#!/usr/bin/env python3
"""生成 S2/S3/S5 文本级合成金标集（`fixtures/gold/`）。

设计目标（见 `fixtures/gold/README.md`）：
- 数据是**规则可推导**的：每条期望由 `SPEC-grade.md` 的 L1 路由表、比较器链、
  聚合规则与 `SPEC-subject-math.md` 的归一化规则显式推导，不由模型标注。
- 覆盖已知扰动：clean / ocr_noise / fullwidth / spacing / latex_variant /
  wrong_choice / partial_choice_2of3 / partial_choice_2opt / missing /
  missing_low_confidence / ocr_noise_review（objective）；
  clean / symbol_variant / multi_solution / missing_step / wrong_intermediate /
  unit_missing / answer_only / low_confidence / uncalibrated（solution）。
- 确定性：固定 seed，不使用当前时间/随机系统熵；输出 JSON 字节稳定。

用法：
    python3 scripts/fixtures/generate_gold.py            # 写入 fixtures/gold/
    python3 scripts/fixtures/generate_gold.py --out DIR  # 写到临时目录（自检用）
"""

from __future__ import annotations

import argparse
import hashlib
import json
import random
import unicodedata
from collections import Counter
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
DEFAULT_OUT = REPO / "fixtures" / "gold"

SEED = 20261109
GENERATOR_VERSION = "1.0.0"
CREATED_AT = "2026-11-09T00:00:00Z"
TIMEOUT_TS = "2026-11-09T00:00:00Z"

# 合成学生（明确非真实身份；学号为 GOLD- 前缀）。
STUDENTS = [
    {"student_id": "stu-A", "name": "学生A", "student_no": "GOLD-0001"},
    {"student_id": "stu-B", "name": "学生B", "student_no": "GOLD-0002"},
    {"student_id": "stu-C", "name": "学生C", "student_no": "GOLD-0003"},
    {"student_id": "stu-D", "name": "学生D", "student_no": "GOLD-0004"},
    {"student_id": "stu-E", "name": "学生E", "student_no": "GOLD-0005"},
    {"student_id": "stu-F", "name": "学生F", "student_no": "GOLD-0006"},
]

ALPHABET = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"


def ulid(value: int) -> str:
    chars = []
    n = value
    for _ in range(26):
        chars.append(ALPHABET[n & 31])
        n >>= 5
    return "".join(reversed(chars))


def sha256_text(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


# ---------------------------------------------------------------------------
# AnswerKey / Rubric / RawAnswer 形状（与 eg-domain serde 表示一致）
# ---------------------------------------------------------------------------


def key_options(options: list[str]) -> dict:
    return {"concrete": {"primary": {"options": options}, "alternatives": [], "comparators": []}}


def key_number(value: str, absolute: str | None = None) -> dict:
    tolerance = None
    if absolute is not None:
        tolerance = {"absolute": absolute, "relative_ppm": None}
    return {
        "concrete": {
            "primary": {"number": {"value": value, "unit": None, "tolerance": tolerance}},
            "alternatives": [],
            "comparators": [],
        }
    }


def key_expression(latex: str) -> dict:
    return {"concrete": {"primary": {"expression": latex}, "alternatives": [], "comparators": []}}


def key_text(text: str) -> dict:
    return {"concrete": {"primary": {"text": text}, "alternatives": [], "comparators": []}}


def key_rubric_only() -> str:
    return "rubric_only"


def key_omitted(note: str = "略") -> dict:
    return {"explicitly_omitted": {"note": note}}


def rubric_origin() -> dict:
    return {"teacher_handwritten": {"confirmed_by": "gold-synthetic", "confirmed_at": CREATED_AT}}


def rubric_item(item_id: str, criterion: str, points_milli: int, depends_on: list[str]) -> dict:
    return {
        "id": item_id,
        "criterion": criterion,
        "points": points_milli,
        "required": True,
        "depends_on": depends_on,
    }


def partial_choice_rule(correct_count: int, tiers: list[tuple[int, int]], total: int) -> dict:
    return {
        "correct_option_count": correct_count,
        "tiers": [
            {
                "selected_correct": selected,
                "points": points,
                "label": f"选对 {selected} 个",
            }
            for selected, points in tiers
        ],
        "wrong_selection_zero": True,
        "source": "template_data",
        "source_note": None,
        "total_milli": total,
    }


def rubric(
    rubric_id: int,
    mode: str,
    items: list[dict],
    total: int,
    partial_choice: dict | None = None,
) -> dict:
    return {
        "id": ulid(rubric_id),
        "version": 1,
        "items": items,
        "mode": mode,
        "total": total,
        "origin": rubric_origin(),
        "partial_choice": partial_choice,
    }


def raw_answer(text: str, latex: str | None = None) -> dict:
    return {
        "text": text,
        "latex": latex,
        "blocks": [],
        "source": "recognition",
    }


# ---------------------------------------------------------------------------
# 文本扰动工具
# ---------------------------------------------------------------------------


def fullwidth(text: str) -> str:
    out = []
    for ch in text:
        code = ord(ch)
        if 0x21 <= code <= 0x7E:
            out.append(chr(code + 0xFEE0))
        elif ch == " ":
            out.append("\u3000")
        else:
            out.append(ch)
    return "".join(out)


def spaced_answer(answer: str, kind: str) -> str:
    if kind in {"single_choice", "multiple_choice"}:
        return " ".join(answer)
    if kind == "true_false":
        return answer[0] + " " + answer[1:] if len(answer) > 1 else " " + answer + " "
    return f"  {answer}  "


def noisy_answer(answer: str, rnd: random.Random) -> str:
    prefix = rnd.choice(["答:", "解：", "所以", "原式="])
    suffix = rnd.choice(["。", "；", ""])
    return f"{prefix}{answer}{suffix}"


# ---------------------------------------------------------------------------
# 客观题（S2/S5）
# ---------------------------------------------------------------------------

OBJECTIVE_PLANS: list[tuple[str, int, list[str]]] = [
    (
        "single_choice",
        130,
        [
            "clean",
            "wrong_choice",
            "fullwidth",
            "spacing",
            "ocr_noise",
            "ocr_noise_review",
            "missing",
            "missing_low_confidence",
        ],
    ),
    (
        "multiple_choice",
        130,
        [
            "clean",
            "partial_choice_2of3",
            "partial_choice_2opt",
            "wrong_choice",
            "fullwidth",
            "spacing",
            "ocr_noise",
            "missing",
        ],
    ),
    (
        "true_false",
        95,
        ["clean", "wrong_choice", "spacing", "ocr_noise", "ocr_noise_review", "missing"],
    ),
    (
        "fill_numeric",
        95,
        [
            "clean",
            "wrong_choice",
            "spacing",
            "fullwidth",
            "latex_variant",
            "ocr_noise",
            "missing",
        ],
    ),
    (
        "fill_expression",
        55,
        [
            "clean",
            "latex_variant",
            "spacing",
            "fullwidth",
            "wrong_choice",
            "ocr_noise",
            "missing",
        ],
    ),
    (
        "fill_text",
        35,
        ["clean", "spacing", "ocr_noise", "ocr_noise_review", "wrong_choice", "missing"],
    ),
]

NUMERIC_VALUES = ["2.5", "0.75", "-0.5", "12", "130", "0.125", "-2.5", "1.5"]
EXPRESSIONS = [
    ("x^{2}-1", "x^2-1", "x ^ 2 - 1"),
    ("2x+3", "2x+3", "2x + 3"),
    ("x^{2}+2x+1", "x^2+2x+1", "x ^ 2 + 2x + 1"),
    ("2(x+1)", "2(x+1)", "2 * (x + 1)"),
]
TEXT_ANSWERS = ["单调递增", "平行四边形", "存在", "无解", "正比例", "互相垂直"]
MAX_OBJECTIVE_SCORE = 5000
MULTI_PARTIAL_SCORE = 6000

# 领域层 QuestionKind 只允许这些取值；填空的三种答案形态用 `answer_form` 区分。
DOMAIN_KIND = {
    "single_choice": "single_choice",
    "multiple_choice": "multiple_choice",
    "true_false": "true_false",
    "fill_numeric": "numeric_only",
    "fill_expression": "fill_in_blank",
    "fill_text": "fill_in_blank",
}
ANSWER_FORM = {
    "single_choice": None,
    "multiple_choice": None,
    "true_false": None,
    "fill_numeric": "number",
    "fill_expression": "expression",
    "fill_text": "text",
}


def objective_perturbation(kind: str, index: int, allowed: list[str]) -> str:
    return allowed[(index * 5 + len(kind) * 3) % len(allowed)]


def build_objective_item(kind: str, index: int, perturbation: str, rnd: random.Random) -> dict:
    qid_number = OBJECTIVE_COUNTER[0]
    OBJECTIVE_COUNTER[0] += 1
    student = STUDENTS[qid_number % len(STUDENTS)]
    max_score = MULTI_PARTIAL_SCORE if kind == "multiple_choice" else MAX_OBJECTIVE_SCORE
    option_letters = "ABCD"

    if kind == "single_choice":
        correct = option_letters[index % 4]
        key = key_options([correct])
        rubric_obj = None
        correct_answer = correct
    elif kind == "multiple_choice":
        pattern = index % 4
        two_correct = [["A", "B"], ["A", "C"], ["B", "D"], ["C", "D"]][pattern]
        three_correct = [["A", "B", "C"], ["A", "B", "D"], ["A", "C", "D"], ["B", "C", "D"]][
            pattern
        ]
        if perturbation in {"partial_choice_2of3", "partial_choice_2opt"}:
            if perturbation == "partial_choice_2of3":
                correct_set = three_correct
                selected = correct_set[:2]
                key = key_options(correct_set)
                rule = partial_choice_rule(3, [(1, 2000), (2, 4000)], max_score)
            else:
                correct_set = two_correct
                selected = correct_set[:1]
                key = key_options(correct_set)
                rule = partial_choice_rule(2, [(1, 3000)], max_score)
            rubric_obj = rubric(
                700_000 + qid_number,
                "partial_choice",
                [
                    rubric_item(
                        ulid(800_000 + qid_number),
                        "选对全部选项给满分；无错选时按命中数查档位",
                        max_score,
                        [],
                    )
                ],
                max_score,
                rule,
            )
            correct_answer = "".join(selected)
        elif pattern % 2 == 0:
            correct_set = two_correct
            key = key_options(correct_set)
            rubric_obj = None
            correct_answer = "".join(correct_set)
        else:
            correct_set = three_correct
            key = key_options(correct_set)
            rubric_obj = None
            correct_answer = "".join(correct_set)
    elif kind == "true_false":
        value = index % 2 == 0
        key = key_text("正确" if value else "错误")
        rubric_obj = None
        correct_answer = "正确" if value else "错误"
    elif kind == "fill_numeric":
        value = NUMERIC_VALUES[index % len(NUMERIC_VALUES)]
        key = key_number(value)
        rubric_obj = None
        correct_answer = value
    elif kind == "fill_expression":
        latex, plain, spaced = EXPRESSIONS[index % len(EXPRESSIONS)]
        key = key_expression(latex)
        rubric_obj = None
        correct_answer = plain
    else:  # fill_text
        text = TEXT_ANSWERS[index % len(TEXT_ANSWERS)]
        key = key_text(text)
        rubric_obj = None
        correct_answer = text

    # 学生原始文本 + 识别置信度（扰动只改文本/置信度，期望由规则推导）。
    confidence = 0.97
    latex: str | None = None
    wrong = False
    manual = False
    low_confidence_review = False
    expected_judgement = "match"
    expected_score: int | None = max_score
    expected_route = "rule_only" if kind in {"single_choice", "multiple_choice", "true_false"} else "rule_then_model"
    rule_trace = "R5" if expected_route == "rule_only" else "R7"

    if perturbation == "clean":
        student_text = correct_answer
    elif perturbation == "fullwidth":
        student_text = fullwidth(correct_answer)
    elif perturbation == "spacing":
        if kind == "fill_expression":
            student_text = EXPRESSIONS[index % len(EXPRESSIONS)][2]
        else:
            student_text = spaced_answer(correct_answer, kind)
    elif perturbation == "ocr_noise":
        student_text = noisy_answer(correct_answer, rnd)
        confidence = 0.70
        manual = True
    elif perturbation == "ocr_noise_review":
        student_text = noisy_answer(correct_answer, rnd)
        confidence = 0.80
        low_confidence_review = True
    elif perturbation == "latex_variant":
        student_text = correct_answer
        if kind == "fill_numeric":
            fraction = {
                "2.5": "\\frac{5}{2}",
                "0.75": "\\frac{3}{4}",
                "-0.5": "-\\frac{1}{2}",
                "12": "\\frac{24}{2}",
                "130": "\\frac{260}{2}",
                "0.125": "\\frac{1}{8}",
                "-2.5": "-\\frac{5}{2}",
                "1.5": "\\frac{3}{2}",
            }[correct_answer]
            latex = fraction
        else:
            latex = correct_answer.replace(" ", "")
    elif perturbation == "wrong_choice":
        wrong = True
        expected_judgement = "mismatch"
        expected_score = 0
        if kind == "single_choice":
            student_text = next(letter for letter in option_letters if letter != correct_answer)
        elif kind == "multiple_choice":
            student_text = "".join(
                sorted({"A", "B", "C", "D"} - set(correct_answer)) or {"A"}
            )
        elif kind == "true_false":
            student_text = "错误" if correct_answer == "正确" else "正确"
        elif kind == "fill_numeric":
            student_text = str(float(correct_answer) + 1.0)
        elif kind == "fill_expression":
            student_text = "x^2+1" if "x" in correct_answer else correct_answer + "+1"
        else:
            student_text = "不存在"
    elif perturbation in {"partial_choice_2of3", "partial_choice_2opt"}:
        student_text = correct_answer
        if perturbation == "partial_choice_2of3":
            expected_judgement = "partial"
            expected_score = 4000
        else:
            expected_judgement = "partial"
            expected_score = 3000
    elif perturbation == "missing":
        student_text = ""
        expected_judgement = "mismatch"
        expected_score = 0
        rule_trace = "R0"
        expected_route = "rule_only"
    elif perturbation == "missing_low_confidence":
        student_text = ""
        confidence = 0.70
        manual = True
    else:  # pragma: no cover - 计划表已穷举
        raise SystemExit(f"未知扰动：{perturbation}")

    if manual:
        expected_judgement = "ungradable"
        expected_score = None
        expected_route = "manual"
        rule_trace = "R1"
        status = "manual"
        review_reason = "ungradable"
        manual_reason = "recognition_unavailable"
    elif low_confidence_review:
        status = "needs_review"
        review_reason = "low_confidence"
        manual_reason = None
    else:
        status = "auto_accept"
        review_reason = None
        manual_reason = None

    normalized = expected_normalized_objective(kind, key, student_text, latex, manual)
    expected_confidence = round(confidence, 4)
    return {
        "qid": f"obj-{qid_number:04d}",
        "kind": DOMAIN_KIND[kind],
        "answer_form": ANSWER_FORM[kind],
        "difficulty": ["easy", "medium", "hard"][index % 3],
        "student_id": student["student_id"],
        "split": "dev" if qid_number % 5 == 0 else "eval",
        "stem_summary": objective_stem(kind, index),
        "max_score_milli": max_score,
        "answer_key": key,
        "rubric": rubric_obj,
        "raw_answer": raw_answer(student_text, latex),
        "recognition": {"confidence": expected_confidence, "source": "fixture_ocr"},
        "perturbations": [perturbation],
        "expected": {
            "route": expected_route,
            "rule_trace": rule_trace,
            "normalized": normalized,
            "judgement": expected_judgement,
            "score_milli": expected_score,
            "status": status,
            "review_reason": review_reason,
            "manual_reason": manual_reason,
            "confidence": expected_confidence,
            "partial_choice_tier": {
                "partial_choice_2of3": 2,
                "partial_choice_2opt": 1,
            }.get(perturbation),
            "reason": expected_reason(kind, perturbation, expected_judgement),
        },
    }


def objective_stem(kind: str, index: int) -> str:
    labels = {
        "single_choice": "合成单选题（四选一，占位题干）",
        "multiple_choice": "合成多选题（选项集合，含部分给分档位）",
        "true_false": "合成判断题（对/错）",
        "fill_numeric": "合成数值填空题（精确十进制）",
        "fill_expression": "合成表达式填空题（等价形式）",
        "fill_text": "合成文本填空题（短语）",
    }
    return f"{labels[kind]} #{index + 1}"


def strip_expected_noise(text: str) -> str:
    value = unicodedata.normalize("NFKC", text).strip()
    for prefix in ("解:", "答:", "原式=", "原式:", "所以", "因此", "故", "则", "得", "解", "答"):
        if value.startswith(prefix):
            value = value[len(prefix):].lstrip()
    while value and value[-1] in "。；;，,、！!?":
        value = value[:-1].rstrip()
    return value


def expected_normalized_objective(
    kind: str, key: dict, student_text: str, latex: str | None, manual: bool
) -> dict:
    if manual:
        return {"kind": "not_asserted", "reason": "low_confidence_route"}
    if not student_text and latex is None:
        return {"kind": "empty"}
    primary = key["concrete"]["primary"]
    if kind in {"single_choice", "multiple_choice"}:
        cleaned = strip_expected_noise(student_text)
        letters = sorted({ch.upper() for ch in cleaned if ch.isascii() and ch.isalpha() and ch.upper() in "ABCDEFGH"})
        return {"kind": "options", "value": letters, "text": "".join(letters)}
    if kind == "true_false":
        compact = "".join(strip_expected_noise(student_text).split())
        value = compact in {"正确", "对", "√", "是", "真", "true", "TRUE", "T", "1"}
        return {"kind": "boolean", "value": value, "text": "true" if value else "false"}
    if kind == "fill_numeric":
        value = primary["number"]["value"]
        if latex is not None:
            return {"kind": "number", "value": normalize_decimal(value), "text": normalize_decimal(value)}
        cleaned = strip_expected_noise(student_text).replace(" ", "")
        try:
            return {"kind": "number", "value": normalize_decimal(cleaned), "text": normalize_decimal(cleaned)}
        except (ValueError, ArithmeticError):
            return {"kind": "number", "value": normalize_decimal(value), "text": normalize_decimal(value)}
    if kind == "fill_expression":
        printed = latex if latex is not None else student_text
        return {
            "kind": "expression",
            "latex": unicodedata.normalize("NFKC", printed).replace(" ", ""),
            "compare": "expression_equivalence",
        }
    cleaned = strip_expected_noise(student_text)
    return {"kind": "text", "value": " ".join(cleaned.split()), "text": " ".join(cleaned.split())}


def normalize_decimal(text: str) -> str:
    from decimal import Decimal

    value = Decimal(text.strip())
    normalized = format(value.normalize(), "f")
    if "." in normalized:
        normalized = normalized.rstrip("0").rstrip(".")
    return normalized or "0"


def expected_reason(kind: str, perturbation: str, judgement: str) -> str:
    if perturbation == "ocr_noise":
        return "识别置信度 0.70 低于下限 0.75：按 R1 转人工（先修文本）"
    if perturbation == "missing_low_confidence":
        return "空答案且识别置信度 0.70：低置信不得按 R0 判 0 分，转人工"
    if perturbation == "missing":
        return "空答案且识别置信度 0.97：按 R0 判不一致（0 分），不调模型"
    if judgement == "partial":
        return "无错选且命中正确选项数落在档位：按档位部分给分"
    if judgement == "mismatch":
        return "确定性比较器判定不一致：0 分"
    if perturbation == "ocr_noise_review":
        return "答案可判定但识别置信度低于自动接受阈值：进待复核（低置信）"
    return f"{kind} 答案经归一化后与答案键一致：满分"


OBJECTIVE_COUNTER = [1]


# ---------------------------------------------------------------------------
# 解答题（S3/S5）
# ---------------------------------------------------------------------------

SOLUTION_PLANS: list[tuple[str, int, list[str]]] = [
    (
        "calculation",
        130,
        [
            "clean",
            "symbol_variant",
            "multi_solution",
            "missing_step",
            "wrong_intermediate",
            "unit_missing",
            "answer_only",
            "low_confidence",
            "uncalibrated",
        ],
    ),
    (
        "proof",
        50,
        ["clean", "symbol_variant", "missing_step", "wrong_intermediate", "low_confidence"],
    ),
    (
        "short_answer",
        40,
        ["clean", "symbol_variant", "multi_solution", "answer_only", "uncalibrated"],
    ),
]

SOLUTION_POINT_SPLITS = [
    (12000, [3000, 5000, 4000]),
    (15000, [4000, 6000, 5000]),
    (18000, [5000, 7000, 6000]),
    (20000, [6000, 8000, 6000]),
]
STEP_CRITERIA = [
    "步骤1：正确写出已知条件/目标式",
    "步骤2：正确完成中间推理或计算",
    "步骤3：得到最终答案并写出结论",
]
STEP_TEXTS = [
    "步骤1：由题意列出已知条件，写出目标式。",
    "步骤2：代入公式完成中间推导与计算。",
    "步骤3：得到最终结果，写出结论。",
]


def solution_rubric(seed: int, mode: str, total: int, points: list[int]) -> tuple[dict, list[str]]:
    item_ids = [ulid(900_000 + seed * 7 + offset) for offset in range(3)]
    if mode == "stepwise":
        depends = [[], [item_ids[0]], [item_ids[1]]]
    else:
        depends = [[], [], []]
    items = [
        rubric_item(item_ids[index], STEP_CRITERIA[index], points[index], depends[index])
        for index in range(3)
    ]
    return rubric(900_000 + seed * 7, mode, items, total), item_ids


def build_solution_item(kind: str, index: int, perturbation: str, rnd: random.Random) -> dict:
    qid_number = SOLUTION_COUNTER[0]
    SOLUTION_COUNTER[0] += 1
    student = STUDENTS[qid_number % len(STUDENTS)]
    total, points = SOLUTION_POINT_SPLITS[index % len(SOLUTION_POINT_SPLITS)]

    mode = "stepwise"
    if kind == "proof" and index % 3 == 0:
        mode = "all_or_nothing"
    if perturbation in {"unit_missing", "answer_only"}:
        mode = "additive"
    if perturbation == "multi_solution":
        mode = "additive"
    rubric_obj, item_ids = solution_rubric(qid_number, mode, total, points)

    recognition_confidence = 0.96
    step_confidence = 0.95
    judge_confidence = 0.93
    calibration_id: str | None = f"cal-gold-{1 + qid_number % 3}"
    matched = [True, True, True]
    step_texts = list(STEP_TEXTS)
    final_text = "最终结果：合成占位答案（见 criteria）。"
    expected_route = "rubric_itemized"
    key_for_route = solution_answer_key(kind, index)
    if kind in {"calculation", "proof"}:
        rule_trace = "R6"
    elif key_for_route == "rubric_only":
        rule_trace = "R4"
    else:
        rule_trace = "R8"
    manual = False
    manual_reason = None

    if perturbation == "clean":
        pass
    elif perturbation == "symbol_variant":
        step_texts = [
            "步骤1：依据题意整理条件并列出关系式。",
            "步骤2：用等价变形完成中间推导（符号写法不同）。",
            "步骤3：得到最终结果与结论。",
        ]
    elif perturbation == "multi_solution":
        step_texts = [
            "步骤1：给出另一种解法的出发点与已知条件。",
            "步骤2：沿该解法完成中间推理。",
            "步骤3：得到同一最终结论。",
        ]
    elif perturbation == "missing_step":
        step_texts = [STEP_TEXTS[0], STEP_TEXTS[2]]
        matched = [True, False, True]
    elif perturbation == "wrong_intermediate":
        step_texts = [STEP_TEXTS[0], "步骤2：中间计算出现错误（合成错误示范）。", "步骤3：结论错误。"]
        matched = [True, False, False]
    elif perturbation == "unit_missing":
        final_text = "最终结果：12.5（缺少单位 cm）。"
        step_texts = [STEP_TEXTS[0], STEP_TEXTS[1], "步骤3：写出数值 12.5，未写单位。"]
        matched = [True, True, False]
    elif perturbation == "answer_only":
        step_texts = ["步骤3：只写最终答案。"]
        matched = [False, False, True]
    elif perturbation == "low_confidence":
        step_confidence = 0.72
        judge_confidence = 0.72
    elif perturbation == "uncalibrated":
        calibration_id = None
    else:  # pragma: no cover
        raise SystemExit(f"未知解答题扰动：{perturbation}")

    effective = effective_items(mode, rubric_obj, matched)
    score = sum(
        rubric_obj["items"][offset]["points"] for offset, ok in enumerate(effective) if ok
    )
    if mode == "all_or_nothing":
        score = total if all(effective) else 0
        if not all(effective):
            effective = [False, False, False]
    confidence = round(min(recognition_confidence, step_confidence, judge_confidence), 4)
    if manual:
        status = "manual"
        review_reason = "ungradable"
    elif confidence < 0.80:
        status = "needs_review"
        review_reason = "low_confidence"
    elif calibration_id is None:
        status = "needs_review"
        review_reason = "uncalibrated_model"
    else:
        status = "auto_accept"
        review_reason = None

    steps = [
        {
            "ordinal": ordinal + 1,
            "text": text,
            "latex": None,
            "confidence": round(step_confidence, 4),
        }
        for ordinal, text in enumerate(step_texts)
    ]
    return {
        "qid": f"sol-{qid_number:04d}",
        "kind": kind,
        "difficulty": ["easy", "medium", "hard"][index % 3],
        "student_id": student["student_id"],
        "split": "dev" if qid_number % 5 == 0 else "eval",
        "stem_summary": f"合成{kind}题（占位题干）：按评分标准分步判定 #{index + 1}",
        "max_score_milli": total,
        "answer_key": solution_answer_key(kind, index),
        "rubric": rubric_obj,
        "raw_answer": raw_answer(final_text),
        "student_steps": steps,
        "recognition": {"confidence": round(recognition_confidence, 4), "source": "fixture_ocr"},
        "judge": {
            "raw_confidence": round(judge_confidence, 4),
            "calibration_id": calibration_id,
            "model_id": "fixture-judge-gold",
            "model_version": "1.0",
        },
        "perturbations": [perturbation],
        "expected": {
            "route": expected_route,
            "rule_trace": rule_trace,
            "aggregation": mode,
            "steps": [
                {
                    "item_id": item_ids[offset],
                    "matched": matched[offset],
                    "effective": effective[offset],
                }
                for offset in range(3)
            ],
            "score_milli": score,
            "judgement": "match" if score == total else ("mismatch" if score == 0 else "partial"),
            "status": status,
            "review_reason": review_reason,
            "manual_reason": manual_reason,
            "calibration_id": calibration_id,
            "confidence": confidence,
            "reason": solution_reason(perturbation, mode, score, total),
        },
    }


def solution_answer_key(kind: str, index: int) -> dict:
    if kind == "calculation":
        value = NUMERIC_VALUES[index % len(NUMERIC_VALUES)]
        return key_number(value)
    if kind == "proof":
        return key_expression("x=1")
    # short_answer：无具体答案键，按评分标准判定（R4）。
    return key_rubric_only()


def effective_items(mode: str, rubric_obj: dict, matched: list[bool]) -> list[bool]:
    if mode == "all_or_nothing":
        return [all(matched)] * 3
    if mode == "stepwise":
        effective = [False, False, False]
        changed = True
        while changed:
            changed = False
            for offset, item in enumerate(rubric_obj["items"]):
                if effective[offset] or not matched[offset]:
                    continue
                dependencies_ready = True
                for dependency in item["depends_on"]:
                    dep_index = next(
                        (
                            position
                            for position, candidate in enumerate(rubric_obj["items"])
                            if candidate["id"] == dependency
                        ),
                        None,
                    )
                    if dep_index is None or not effective[dep_index]:
                        dependencies_ready = False
                        break
                if dependencies_ready:
                    effective[offset] = True
                    changed = True
        return effective
    return list(matched)


def solution_reason(perturbation: str, mode: str, score: int, total: int) -> str:
    if perturbation == "low_confidence":
        return "步骤判定置信度 0.72 低于自动接受阈值 0.80：进待复核（低置信）"
    if perturbation == "uncalibrated":
        return "模型无校准记录：标准模式不得自动放行，转待复核"
    if perturbation == "missing_step":
        return "缺少中间步骤：分步依赖使后续评分项不生效，按实际满足项聚合"
    if perturbation == "wrong_intermediate":
        return "中间步骤错误：依赖链后续评分项不生效"
    if perturbation == "unit_missing":
        return "最终答案缺少单位：仅最终答案评分项不满足（累加模式）"
    if perturbation == "answer_only":
        return "只给最终答案：过程评分项不满足（累加模式）"
    return f"{mode} 聚合：满足项分值之和 = {score} / {total}"


SOLUTION_COUNTER = [1]


# ---------------------------------------------------------------------------
# 特殊样例：仅评分标准（R4）与「略」（R3/manual）
# ---------------------------------------------------------------------------


def build_special_items() -> tuple[list[dict], list[dict]]:
    objective_specials: list[dict] = []
    solution_specials: list[dict] = []
    for offset in range(5):
        # 仅评分标准：key=rubric_only，已确认规则 → R4 rubric_itemized（不是 manual）。
        seed = 50_000 + offset
        total = 15000
        points = [4000, 6000, 5000]
        rubric_obj, item_ids = solution_rubric(seed, "stepwise", total, points)
        matched = [True, True, True]
        effective = effective_items("stepwise", rubric_obj, matched)
        score = sum(
            rubric_obj["items"][position]["points"]
            for position, ok in enumerate(effective)
            if ok
        )
        confidence = 0.93
        solution_specials.append(
            {
                "qid": f"sol-special-rubric-{offset + 1:02d}",
                "kind": "proof" if offset % 2 == 0 else "short_answer",
                "difficulty": "medium",
                "student_id": STUDENTS[offset % len(STUDENTS)]["student_id"],
                "split": "eval",
                "stem_summary": "合成仅评分标准题（无具体答案键，占位题干）",
                "max_score_milli": total,
                "answer_key": key_rubric_only(),
                "rubric": rubric_obj,
                "raw_answer": raw_answer("按评分标准作答（合成占位文本）。"),
                "student_steps": [
                    {
                        "ordinal": 1,
                        "text": "步骤1：按评分标准写出第一步。",
                        "latex": None,
                        "confidence": confidence,
                    },
                    {
                        "ordinal": 2,
                        "text": "步骤2：按评分标准写出第二步。",
                        "latex": None,
                        "confidence": confidence,
                    },
                ],
                "recognition": {"confidence": confidence, "source": "fixture_ocr"},
                "judge": {
                    "raw_confidence": confidence,
                    "calibration_id": "cal-gold-1",
                    "model_id": "fixture-judge-gold",
                    "model_version": "1.0",
                },
                "perturbations": ["rubric_only"],
                "expected": {
                    "route": "rubric_itemized",
                    "rule_trace": "R4",
                    "aggregation": "stepwise",
                    "steps": [
                        {
                            "item_id": item_ids[position],
                            "matched": matched[position],
                            "effective": effective[position],
                        }
                        for position in range(3)
                    ],
                    "score_milli": score,
                    "judgement": "match" if score == total else ("mismatch" if score == 0 else "partial"),
                    "status": "auto_accept",
                    "review_reason": None,
                    "manual_reason": None,
                    "calibration_id": "cal-gold-1",
                    "confidence": confidence,
                    "reason": "答案键为仅评分标准：按 R4 逐项判定（不是 manual）",
                },
            }
        )
    for offset in range(5):
        solution_specials.append(
            {
                "qid": f"sol-special-omitted-{offset + 1:02d}",
                "kind": "short_answer",
                "difficulty": "easy",
                "student_id": STUDENTS[offset % len(STUDENTS)]["student_id"],
                "split": "eval",
                "stem_summary": "合成「略」题（答案键明确省略，无规则）",
                "max_score_milli": 10000,
                "answer_key": key_omitted("略"),
                "rubric": None,
                "raw_answer": raw_answer("略"),
                "student_steps": [],
                "recognition": {"confidence": 0.96, "source": "fixture_ocr"},
                "judge": {
                    "raw_confidence": 0.0,
                    "calibration_id": None,
                    "model_id": "fixture-judge-gold",
                    "model_version": "1.0",
                },
                "perturbations": ["answer_key_omitted"],
                "expected": {
                    "route": "manual",
                    "rule_trace": "R3",
                    "aggregation": None,
                    "steps": [],
                    "score_milli": None,
                    "judgement": "ungradable",
                    "status": "manual",
                    "review_reason": "ungradable",
                    "manual_reason": "answer_key_omitted",
                    "calibration_id": None,
                    "confidence": 0.0,
                    "reason": "答案键为「略」且无已确认规则：按 R3 仅人工，不得自动判分",
                },
            }
        )
    return objective_specials, solution_specials


# ---------------------------------------------------------------------------
# 数据集装配 / README
# ---------------------------------------------------------------------------


def build_objective() -> list[dict]:
    OBJECTIVE_COUNTER[0] = 1
    rnd = random.Random(SEED)
    items: list[dict] = []
    for kind, count, allowed in OBJECTIVE_PLANS:
        for index in range(count):
            perturbation = objective_perturbation(kind, index, allowed)
            items.append(build_objective_item(kind, index, perturbation, rnd))
    return items


def build_solution() -> tuple[list[dict], list[dict]]:
    SOLUTION_COUNTER[0] = 1
    rnd = random.Random(SEED + 1)
    items: list[dict] = []
    for kind, count, allowed in SOLUTION_PLANS:
        for index in range(count):
            perturbation = allowed[(index * 7 + len(kind) * 2) % len(allowed)]
            items.append(build_solution_item(kind, index, perturbation, rnd))
    _, specials = build_special_items()
    return items, specials


def coverage_matrix(items: list[dict]) -> dict:
    by_kind = Counter(item["kind"] for item in items)
    by_perturbation = Counter(label for item in items for label in item["perturbations"])
    by_status = Counter(item["expected"]["status"] for item in items)
    by_route = Counter(item["expected"]["route"] for item in items)
    table: dict[str, Counter] = {}
    for item in items:
        for label in item["perturbations"]:
            table.setdefault(item["kind"], Counter())[label] += 1
    return {
        "total": len(items),
        "by_kind": dict(sorted(by_kind.items())),
        "by_perturbation": dict(sorted(by_perturbation.items())),
        "by_status": dict(sorted(by_status.items())),
        "by_route": dict(sorted(by_route.items())),
        "kind_x_perturbation": {
            kind: dict(sorted(counter.items())) for kind, counter in sorted(table.items())
        },
    }


def matrix_markdown(matrix: dict) -> str:
    kinds = sorted(matrix["kind_x_perturbation"])
    perturbations = sorted(matrix["by_perturbation"])
    lines = ["| kind \\ perturbation | " + " | ".join(perturbations) + " | 合计 |"]
    lines.append("|" + "---|" * (len(perturbations) + 2))
    for kind in kinds:
        row = matrix["kind_x_perturbation"][kind]
        cells = [str(row.get(label, 0)) for label in perturbations]
        lines.append(f"| `{kind}` | " + " | ".join(cells) + f" | {matrix['by_kind'][kind]} |")
    totals = [str(matrix["by_perturbation"][label]) for label in perturbations]
    lines.append("| **合计** | " + " | ".join(totals) + f" | {matrix['total']} |")
    return "\n".join(lines)


def build_readme(
    objective_count: int,
    objective_matrix: dict,
    solution_count: int,
    solution_matrix: dict,
    index: dict,
) -> str:
    obj = objective_matrix
    sol = solution_matrix
    return f"""# S2/S3/S5 合成金标集（`fixtures/gold/`）

> 状态：v1 · {CREATED_AT} · 生成器 `scripts/fixtures/generate_gold.py`（seed={SEED}）· 自检 `scripts/fixtures/verify_gold.py`
> 用途：S2 客观题判分准确率、S3 解答题分步判分、S5 规则/分档聚合的**文本级**金标集；不替代真实试卷评估。

## 0. 诚实声明（先读）

- **全部为合成数据**：题干是占位描述（"合成单选题 #1"），**不是真实试卷/试题**；学生是 `学生A..F`/`GOLD-000x`
  合成身份，不含任何真实姓名、学校、班级或试卷原文。
- **期望值由规范规则推导，不是模型标注**：每条 `expected` 都可由 `SPEC-grade.md`
  的 L1 路由表（R0–R11）、比较器链、§5.3 聚合规则与 `SPEC-subject-math.md` 的归一化
  规则复算（`verify_gold.py` 独立重算并比对）。`confidence` 字段是按规范组合规则
  （规则段取识别置信度；模型段取 min(模型/识别/证据)）推导的**标签**，不是模型实测值。
- **不是准确率目标**：本数据集故意包含低置信、缺步、错中间值、单位缺失、未校准等
  扰动；eval 应报告「分扰动类别的准确率/复核率」，而不是追求 100%。
- **不含图片**：图像链路已由 S1 夹具 `fixtures/paper-20q` 覆盖；本目录只有文本 JSON。

## 1. 文件

| 文件 | 内容 | 条目数 | sha256 |
|---|---|---|---|
| `objective-500.json` | 客观题（单选/多选/判断/数值/表达式/文本填空） | {objective_count} | `{index['files']['objective-500.json']}` |
| `solution-200.json` | 解答/计算/证明/简答（分步 rubric + 学生步骤文本） | {solution_count} | `{index['files']['solution-200.json']}` |
| `index.json` | 机器可读索引：计数、覆盖矩阵、规则映射、sha256 | — | — |
| `README.md` | 本文件（schema/覆盖/再生成/接入约定） | — | — |

规模：纯 JSON 文本，总 < 3 MB（由 `verify_gold.py` 检查）；无图片。

## 2. Schema（v1）

两个数据文件共享顶层骨架：

```jsonc
{{
  "schema_version": 1,
  "dataset": "objective" | "solution",
  "generator_version": "{GENERATOR_VERSION}",
  "seed": {SEED},
  "created_at": "{CREATED_AT}",
  "students": [{{"student_id": "stu-A", "name": "学生A", "student_no": "GOLD-0001"}}],
  "items": [ ... ]
}}
```

### 2.1 客观题条目（`objective-500.json`）

| 字段 | 说明 |
|---|---|
| `qid` | 稳定 id（`obj-0001`，按生成顺序） |
| `kind` | 领域层 `QuestionKind`：`single_choice`/`multiple_choice`/`true_false`/`numeric_only`/`fill_in_blank`（可直接反序列化） |
| `answer_form` | 填空/数值的答案形态：`number`/`expression`/`text`（非填空为 null；用于 eval 分组与归一化断言） |
| `stem_summary` | 占位题干摘要（合成、非真实试题） |
| `max_score_milli` | 满分（毫元；单选/判断/填空 5000，多选 6000） |
| `answer_key` | `eg-domain` AnswerKey 的 JSON 形状（`concrete`/`rubric_only`/`explicitly_omitted`/`missing`） |
| `rubric` | 仅多选部分给分条目内嵌 `PartialChoiceRule`（其余为 null） |
| `raw_answer` | `eg-domain` `RawAnswer` 形状：`text`/`latex`/`blocks`/`source` |
| `recognition.confidence` | 识别置信度（规则推导用；`<0.75` 触发 R1） |
| `perturbations` | 本条的扰动标签（见 §3） |
| `expected.route` | `rule_only`/`rule_then_model`/`rubric_itemized`/`manual`（SPEC L1 输出） |
| `expected.rule_trace` | 命中的 L1 规则编号（`R0`/`R1`/`R5`/`R7`…） |
| `expected.normalized` | 归一化的**语义**期望：`{{kind: options\\|boolean\\|number\\|expression\\|text\\|empty, ...}}`；表达式用 `compare:"expression_equivalence"`（不锁定逐字符文本） |
| `expected.judgement` | `match`/`partial`/`mismatch`/`ungradable` |
| `expected.score_milli` | 毫元；`ungradable` 为 null；partial 必须落在档位 |
| `expected.status` | `auto_accept`/`needs_review`/`manual`（映射见 §4） |
| `expected.review_reason`/`manual_reason` | 待复核/仅人工原因（`low_confidence`/`ungradable`/`recognition_unavailable`…） |
| `expected.confidence` | 按 SPEC §6 组合规则推导的置信度标签 |
| `expected.partial_choice_tier` | 多选部分给分命中档位（无则 null） |

### 2.2 解答题条目（`solution-200.json`）

在客观题字段基础上：

| 字段 | 说明 |
|---|---|
| `rubric` | `eg-domain` `Rubric`（`mode` = `stepwise`/`additive`/`all_or_nothing`；`items[]` 含 `criterion`/`points`/`depends_on`） |
| `student_steps[]` | 学生分步文本（`ordinal`/`text`/`latex`/`confidence`）——对应 `StepEvidence` 的文本来源 |
| `judge` | 合成判定元数据：`raw_confidence`/`calibration_id`/`model_id`/`model_version` |
| `expected.aggregation` | 本条的聚合模式（与 rubric.mode 一致） |
| `expected.steps[]` | 逐评分项：`item_id`/`matched`（找到可定位步骤）/`effective`（分步依赖裁剪后实际计分） |
| `expected.score_milli` | 聚合得分（毫元；`manual` 为 null） |
| `expected.calibration_id` | 与 `judge.calibration_id` 一致（未校准为 null） |

## 3. 扰动标签与覆盖

Objective 扰动：`clean`（无扰动）、`ocr_noise`（OCR 噪声 + 置信度 0.70 → R1 manual）、
`ocr_noise_review`（噪声但置信度 0.80 → 待复核 low_confidence）、`fullwidth`（全角）、
`spacing`（多余空白）、`latex_variant`（等价 LaTeX/分数写法）、`wrong_choice`（错答）、
`partial_choice_2of3`（3 个正确选项选 2 个 → 档位 4000/6000）、`partial_choice_2opt`
（2 个正确选项选 1 个 → 3000/6000）、`missing`（空答 + 高置信 → R0）、
`missing_low_confidence`（空答 + 低置信 → R1）。

Solution 扰动：`clean`、`symbol_variant`（符号等价写法）、`multi_solution`（另一解法）、
`missing_step`（漏中间步骤）、`wrong_intermediate`（错误中间值）、`unit_missing`（缺单位）、
`answer_only`（仅最终答案）、`low_confidence`（判定置信度 0.72）、`uncalibrated`（无校准记录）；
另含特殊样例 `rubric_only`（仅评分标准，R4 `rubric_itemized`）与 `answer_key_omitted`
（「略」且无规则，R3 `manual`）。

### 3.1 Objective 覆盖矩阵（kind × perturbation；`fill_in_blank` 含表达式/文本两种 `answer_form`）

{matrix_markdown(obj)}

### 3.2 Solution 覆盖矩阵（kind × perturbation）

{matrix_markdown(sol)}

### 3.3 状态/路由分布

| 数据集 | status | route |
|---|---|---|
| objective | `{json.dumps(obj['by_status'], ensure_ascii=False)}` | `{json.dumps(obj['by_route'], ensure_ascii=False)}` |
| solution | `{json.dumps(sol['by_status'], ensure_ascii=False)}` | `{json.dumps(sol['by_route'], ensure_ascii=False)}` |

## 4. 规则 → 期望的推导（eval 应对齐）

| 规则 | 条件 | 期望 |
|---|---|---|
| R0 | 空答案且识别置信度 ≥0.75 | `rule_only`，judgement=`mismatch`，score=0，status=`auto_accept` |
| R1 | 识别置信度 <0.75（或归一化失败） | `manual`，judgement=`ungradable`，score=null，status=`manual`，reason=`recognition_unavailable` |
| R3 | 答案键「略」且无确认规则 | `manual`，`answer_key_omitted`，score=null |
| R4 | 仅评分标准（或「略」+确认规则） | `rubric_itemized`（**不是 manual**），按 rubric 聚合 |
| R5 | 具体答案 + 单选/多选/判断 | `rule_only`；比较器 Equal/Equivalent→满分；NotEqual→0；多选 partial 按档位 |
| R6 | 具体答案 + 分步 rubric + 计算/证明 | `rubric_itemized`，按依赖聚合 |
| R7 | 具体答案 + 填空/数值 | `rule_then_model`；比较器结论明确时不升级模型 |
| 自动接受 | route≠manual 且 confidence ≥ 阈值（仅规则 0.85、规则优先 0.90、规则逐项 0.80，标准模式）且校准 id 存在 | `auto_accept`；否则 `needs_review` |
| 聚合 | stepwise=依赖固定点后满足项之和；additive=满足项之和；all_or_nothing=全满足才满分 | `expected.score_milli`/`expected.steps[].effective` |

> 多选部分给分：无错选时按命中正确选项数查档（3 选正确选项规则：1 个→2000、2 个→4000、
> 3 个→6000；2 选正确选项规则：1 个→3000、2 个→6000）；有错选一律 0 分。

## 5. 再生成与自检

```bash
python3 scripts/fixtures/generate_gold.py          # 重新生成（确定性 seed={SEED}）
python3 scripts/fixtures/verify_gold.py            # 自检：数量/字段/自洽/覆盖/字节稳定/体积
python3 scripts/fixtures/verify_gold.py --skip-bytes  # 只做结构/规则自检（跳过字节比对）

# 抽检与实现一致（Rust：eg-subject-math 归一化+比较器链 / eg-grade 聚合；/tmp 构建）
scripts/fixtures/smoke_gold.sh
```

`verify_gold.py` 独立重算期望（不复用生成器函数），校验：条目数 ≥500/≥200、字段完整、
答案键形状、rubric 分值/依赖/档位合法、期望 judgement/score/status 与规则自洽、
扰动覆盖 ≥6、状态/路由分布、README 矩阵一致、无真实姓名、总大小 <3MB。

## 6. 给 eval 任务的接入约定

eval 读取 `index.json`（包含两个数据文件的 sha256 与计数）或直接读两个数据文件。建议流程：

1. 按 `kind`/`perturbation` 分组统计；每条用 `expected.route` 选择判分路径：
   - `rule_only`/`rule_then_model`：用学科包比较器链跑 `raw_answer`，与 `expected.judgement/score_milli` 对比；
   - `rubric_itemized`：用 `expected.steps[].matched` 作为判定输入（模拟 judge 输出），
     走 `eg-grade` 聚合（`aggregate`/`partial_choice`），与 `expected.steps[].effective`、
     `expected.score_milli` 对比；同时把 `judge.calibration_id` 传给门控；
   - `manual`：只断言 route/status/manual_reason，不断言 score。
2. 归一化断言：`rule_*` 路径比较 `expected.normalized`（options 集合/boolean/number 文本/
   expression 语义等价/text 去空白）；`manual` 条目只作诊断，不判失败。
3. 自动接受：按 `expected.status` 断言；`needs_review` 条目同时断言 `review_reason`。
4. 输出按 (§3) 扰动类别的 accuracy / auto_accept_rate / review_rate，不用总体 100% 作目标。

### 6.1 已知边界（非 bug）

- `expected.normalized.expression` 只承诺**语义等价**（CSS 逐字符规范化可能不同）；
  逐字符文本以 `eg-subject-math` 实现为准，eval 不应对表达式锁定字符串。
- 合成学生 `学生A..F` 仅用于 eval 分组，不代表真实身份；`split` 为确定性划分
  （每 5 条 1 条 `dev`，其余 `eval`），可按需重划分。
- `confidence` 是规则推导标签；真实评测时应替换为被测实现给出的置信度再与
  `expected.status` 门限比较。
"""


def write_dataset(out: Path) -> dict:
    objective_items = build_objective()
    solution_regular, solution_specials = build_solution()
    solution_items = solution_regular + solution_specials

    objective_matrix = coverage_matrix(objective_items)
    solution_matrix = coverage_matrix(solution_items)
    objective = {
        "schema_version": 1,
        "dataset": "objective",
        "generator_version": GENERATOR_VERSION,
        "seed": SEED,
        "created_at": CREATED_AT,
        "students": STUDENTS,
        "notes": [
            "合成数据（占位题干/学生A-F），不是真实试卷或学生作答。",
            "期望值由 SPEC-grade/SPEC-subject-math 规则推导，见 README §4。",
            "confidence 是规则推导标签，不是模型实测置信度。",
        ],
        "items": objective_items,
    }
    solution = {
        "schema_version": 1,
        "dataset": "solution",
        "generator_version": GENERATOR_VERSION,
        "seed": SEED,
        "created_at": CREATED_AT,
        "students": STUDENTS,
        "notes": [
            "合成数据（占位题干/学生A-F），不是真实试卷或学生作答。",
            "step 的 matched 是「找到可定位步骤」；effective 是分步依赖裁剪后实际计分。",
            "judge.calibration_id 为 null 表示未校准：标准模式不得自动放行。",
        ],
        "items": solution_items,
    }

    objective_text = json.dumps(objective, ensure_ascii=False, indent=2) + "\n"
    solution_text = json.dumps(solution, ensure_ascii=False, indent=2) + "\n"
    files = {
        "objective-500.json": sha256_text(objective_text),
        "solution-200.json": sha256_text(solution_text),
    }
    index = {
        "schema_version": 1,
        "generator_version": GENERATOR_VERSION,
        "seed": SEED,
        "created_at": CREATED_AT,
        "students": STUDENTS,
        "files": files,
        "objective": {
            "count": len(objective_items),
            **objective_matrix,
            "file": "objective-500.json",
        },
        "solution": {
            "count": len(solution_items),
            **solution_matrix,
            "file": "solution-200.json",
        },
    }
    readme = build_readme(
        len(objective_items),
        objective_matrix,
        len(solution_items),
        solution_matrix,
        index,
    )

    out.mkdir(parents=True, exist_ok=True)
    (out / "objective-500.json").write_text(objective_text, encoding="utf-8")
    (out / "solution-200.json").write_text(solution_text, encoding="utf-8")
    (out / "index.json").write_text(
        json.dumps(index, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    (out / "README.md").write_text(readme, encoding="utf-8")
    return {
        "objective": len(objective_items),
        "solution": len(solution_items),
        "out": str(out),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT, help="输出目录（默认 fixtures/gold）")
    args = parser.parse_args()
    summary = write_dataset(args.out)
    print(
        f"[ok] gold 已生成：objective={summary['objective']}，solution={summary['solution']}，"
        f"输出={summary['out']}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
