#!/usr/bin/env python3
"""自检 S2/S3/S5 合成金标集（`fixtures/gold/`）。

**独立重算**：本脚本不复用 `generate_gold.py` 的推导函数，而是按
`SPEC-grade.md`（L1 路由 R0–R11、比较器语义、§5.3 聚合、门禁阈值）与
`SPEC-subject-math.md`（归一化）重新实现一遍，逐条比对存储的 `expected`。

检查项：
1. 文件/条目数（objective ≥500、solution ≥200）、字段完整、qid/学生唯一；
2. AnswerKey / Rubric / RawAnswer JSON 形状与不变量（分值、依赖、档位）；
3. 逐条重算 route/rule_trace/normalized/judgement/score/status/review_reason；
4. 扰动标签分布（≥6 类）与状态/路由覆盖、覆盖矩阵与 index.json/README 一致；
5. 期望与答案键自洽、分数不超满分、partial 命中档位；
6. 无真实姓名/学校；无图片；总大小 <3MB；
7. 生成器确定性：重新生成到临时目录并逐字节比对（默认启用）。

用法：`python3 scripts/fixtures/verify_gold.py [--skip-bytes]`
退出码 0 = 全部通过。
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
import tempfile
import unicodedata
from collections import Counter
from decimal import Decimal, InvalidOperation
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
GOLD = REPO / "fixtures" / "gold"
GENERATOR = REPO / "scripts" / "fixtures" / "generate_gold.py"

FAILS: list[str] = []
CHECKS = 0

OBJECTIVE_KINDS = {
    "single_choice",
    "multiple_choice",
    "true_false",
    "numeric_only",
    "fill_in_blank",
}
ANSWER_FORMS = {"number", "expression", "text"}
SOLUTION_KINDS = {"calculation", "proof", "short_answer"}
OBJECTIVE_PERTURBATIONS = {
    "clean",
    "ocr_noise",
    "ocr_noise_review",
    "fullwidth",
    "spacing",
    "latex_variant",
    "wrong_choice",
    "partial_choice_2of3",
    "partial_choice_2opt",
    "missing",
    "missing_low_confidence",
}
SOLUTION_PERTURBATIONS = {
    "clean",
    "symbol_variant",
    "multi_solution",
    "missing_step",
    "wrong_intermediate",
    "unit_missing",
    "answer_only",
    "low_confidence",
    "uncalibrated",
    "rubric_only",
    "answer_key_omitted",
}
ULID_RE = re.compile(r"^[0-9A-HJKMNP-TV-Z]{26}$")
BANNED_NAMES = ("张三", "李四", "王五", "赵六", "某某中学", "实验中学")
ALLOWED_STUDENTS = {"学生A", "学生B", "学生C", "学生D", "学生E", "学生F"}
ALLOWED_STUDENT_NOS = {f"GOLD-{index:04d}" for index in range(1, 7)}
ALLOWED_ANSWER_SOURCES = {"recognition", "manual_correction", "electronic_text"}
ALLOWED_BLOCK_KINDS = {
    "text",
    "formula",
    "table",
    "handwriting",
    "figure",
    "stamp",
    "barcode",
    "header_footer",
    "unknown",
}
ALLOWED_QUESTION_KINDS = {
    "single_choice",
    "multiple_choice",
    "true_false",
    "fill_in_blank",
    "numeric_only",
    "calculation",
    "proof",
    "short_answer",
    "essay",
    "other",
}
ALLOWED_RUBRIC_MODES = {"additive", "all_or_nothing", "stepwise", "partial_choice"}


def check(condition: bool, message: str) -> None:
    global CHECKS
    CHECKS += 1
    if not condition:
        FAILS.append(message)


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


# ---------------------------------------------------------------------------
# JSON 形状校验（eg-domain serde 表示）
# ---------------------------------------------------------------------------


def is_ulid(text: object) -> bool:
    return isinstance(text, str) and bool(ULID_RE.match(text))


def validate_answer_value(value: object, where: str) -> None:
    check(isinstance(value, dict) and len(value) == 1, f"{where}: AnswerValue 必须恰好一个变体")
    if not isinstance(value, dict):
        return
    variant, body = next(iter(value.items()))
    if variant == "text":
        check(isinstance(body, str) and body.strip() != "", f"{where}: text 非空")
    elif variant == "number":
        check(isinstance(body, dict), f"{where}: number 必须是对象")
        if isinstance(body, dict):
            check(set(body) <= {"value", "unit", "tolerance"}, f"{where}: number 字段多余")
            check(isinstance(body.get("value"), str) and _decimal(body["value"]) is not None, f"{where}: number.value 非十进制")
            check(body.get("unit") is None or isinstance(body.get("unit"), str), f"{where}: unit")
            tolerance = body.get("tolerance")
            check(tolerance is None or isinstance(tolerance, dict), f"{where}: tolerance")
    elif variant == "expression":
        check(isinstance(body, str) and body.strip() != "", f"{where}: expression 非空")
    elif variant == "options":
        check(isinstance(body, list) and body, f"{where}: options 非空数组")
        if isinstance(body, list):
            check(all(isinstance(letter, str) and len(letter) == 1 and letter.isalpha() for letter in body), f"{where}: options 必须是单字母")
            check(len(set(body)) == len(body), f"{where}: options 不得重复")
    elif variant == "steps":
        check(isinstance(body, list) and body, f"{where}: steps 非空")
    elif variant == "structured":
        check(isinstance(body, dict), f"{where}: structured 对象")
    else:
        check(False, f"{where}: 未知 AnswerValue 变体 {variant}")


def validate_answer_key(key: object, where: str) -> str:
    if key == "rubric_only":
        return "rubric_only"
    if key == "missing":
        return "missing"
    check(isinstance(key, dict) and len(key) == 1, f"{where}: AnswerKey 必须恰好一个变体")
    if not isinstance(key, dict):
        return "invalid"
    variant, body = next(iter(key.items()))
    if variant == "concrete":
        check(isinstance(body, dict), f"{where}: concrete 对象")
        if isinstance(body, dict):
            check(set(body) <= {"primary", "alternatives", "comparators"}, f"{where}: concrete 字段多余")
            check("primary" in body, f"{where}: concrete.primary 缺失")
            if "primary" in body:
                validate_answer_value(body["primary"], f"{where}.primary")
            alternatives = body.get("alternatives", [])
            check(isinstance(alternatives, list), f"{where}: alternatives 数组")
            if isinstance(alternatives, list):
                for index, alternative in enumerate(alternatives):
                    validate_answer_value(alternative, f"{where}.alternatives[{index}]")
            comparators = body.get("comparators", [])
            check(isinstance(comparators, list), f"{where}: comparators 数组")
        return "concrete"
    if variant == "explicitly_omitted":
        check(isinstance(body, dict) and isinstance(body.get("note"), str), f"{where}: explicitly_omitted.note")
        return "explicitly_omitted"
    check(False, f"{where}: 未知 AnswerKey 变体 {variant}")
    return "invalid"


def validate_rubric(rubric: object, max_score: int, where: str) -> None:
    check(isinstance(rubric, dict), f"{where}: rubric 必须是对象")
    if not isinstance(rubric, dict):
        return
    check(set(rubric) <= {"id", "version", "items", "mode", "total", "origin", "partial_choice"}, f"{where}: rubric 字段多余")
    check(is_ulid(rubric.get("id")), f"{where}: rubric.id 必须 ULID")
    check(rubric.get("version") == 1, f"{where}: rubric.version 必须 1")
    check(rubric.get("mode") in ALLOWED_RUBRIC_MODES, f"{where}: rubric.mode 非法")
    total = rubric.get("total")
    check(total == max_score, f"{where}: rubric.total 必须等于满分（{max_score}）")
    origin = rubric.get("origin")
    check(
        isinstance(origin, dict)
        and "teacher_handwritten" in origin
        and isinstance(origin["teacher_handwritten"].get("confirmed_by"), str)
        and isinstance(origin["teacher_handwritten"].get("confirmed_at"), str),
        f"{where}: rubric.origin 形状",
    )
    items = rubric.get("items")
    check(isinstance(items, list) and items, f"{where}: rubric.items 非空")
    item_ids = []
    points_sum = 0
    if isinstance(items, list):
        for index, item in enumerate(items):
            check(isinstance(item, dict), f"{where}.items[{index}] 对象")
            if not isinstance(item, dict):
                continue
            check(set(item) <= {"id", "criterion", "points", "required", "depends_on"}, f"{where}.items[{index}] 字段多余")
            check(is_ulid(item.get("id")), f"{where}.items[{index}].id 必须 ULID")
            item_ids.append(item.get("id"))
            check(isinstance(item.get("criterion"), str) and item["criterion"].strip(), f"{where}.items[{index}].criterion")
            check(isinstance(item.get("points"), int) and item["points"] > 0, f"{where}.items[{index}].points 正数")
            if isinstance(item.get("points"), int):
                points_sum += item["points"]
            check(isinstance(item.get("required"), bool), f"{where}.items[{index}].required 布尔")
            depends = item.get("depends_on")
            check(isinstance(depends, list), f"{where}.items[{index}].depends_on 数组")
            if isinstance(depends, list):
                check(len(set(depends)) == len(depends), f"{where}.items[{index}].depends_on 重复")
        check(len(set(item_ids)) == len(item_ids), f"{where}: 评分项 id 重复")
        check(points_sum == total, f"{where}: 评分项分值之和必须等于 total")
    mode = rubric.get("mode")
    partial = rubric.get("partial_choice")
    if mode == "partial_choice":
        check(isinstance(partial, dict), f"{where}: partial_choice 模式必须带档位规则")
        if isinstance(partial, dict):
            check(set(partial) <= {"correct_option_count", "tiers", "wrong_selection_zero", "source", "source_note", "total_milli"}, f"{where}: partial_choice 字段多余")
            count = partial.get("correct_option_count")
            check(count in (2, 3), f"{where}: correct_option_count 必须 2/3")
            tiers = partial.get("tiers")
            check(isinstance(tiers, list) and tiers, f"{where}: tiers 非空")
            selected = []
            if isinstance(tiers, list):
                for tier in tiers:
                    check(isinstance(tier, dict), f"{where}: tier 对象")
                    if isinstance(tier, dict):
                        selected.append(tier.get("selected_correct"))
                        check(isinstance(tier.get("points"), int) and 0 < tier["points"] < total, f"{where}: tier.points 必须在 (0,total)")
                        check(isinstance(tier.get("label"), str) and tier["label"], f"{where}: tier.label")
            if isinstance(count, int):
                check(selected == list(range(1, count)), f"{where}: 档位覆盖 1..{count - 1}")
            check(partial.get("wrong_selection_zero") is True, f"{where}: wrong_selection_zero 必须 true")
            check(partial.get("total_milli") == total, f"{where}: partial_choice.total_milli")
            check(partial.get("source") in {"official", "template_data", "teacher_custom"}, f"{where}: partial_choice.source")
    else:
        check(partial is None, f"{where}: 非 partial_choice 模式不得有档位规则")


def validate_raw_answer(raw: object, where: str) -> None:
    check(isinstance(raw, dict), f"{where}: raw_answer 对象")
    if not isinstance(raw, dict):
        return
    check(set(raw) <= {"text", "latex", "blocks", "source"}, f"{where}: raw_answer 字段多余")
    check(isinstance(raw.get("text"), str), f"{where}: raw_answer.text 字符串")
    check(raw.get("latex") is None or isinstance(raw.get("latex"), str), f"{where}: raw_answer.latex")
    blocks = raw.get("blocks")
    check(isinstance(blocks, list), f"{where}: raw_answer.blocks 数组")
    if isinstance(blocks, list):
        for index, block in enumerate(blocks):
            check(isinstance(block, dict), f"{where}.blocks[{index}] 对象")
            if isinstance(block, dict):
                check(block.get("kind") in ALLOWED_BLOCK_KINDS, f"{where}.blocks[{index}].kind")
                check(isinstance(block.get("text"), str), f"{where}.blocks[{index}].text")
                confidence = block.get("confidence")
                check(isinstance(confidence, (int, float)) and 0 <= confidence <= 1, f"{where}.blocks[{index}].confidence")
    check(raw.get("source") in ALLOWED_ANSWER_SOURCES, f"{where}: raw_answer.source")


# ---------------------------------------------------------------------------
# 独立归一化/比较（不依赖 crate，也不复用生成器）
# ---------------------------------------------------------------------------


def _decimal(text: str) -> Decimal | None:
    try:
        value = Decimal(text.strip())
    except (InvalidOperation, ValueError):
        return None
    return value if value.is_finite() else None


def strip_noise(text: str) -> str:
    value = unicodedata.normalize("NFKC", text).strip()
    for prefix in ("解:", "答:", "原式=", "原式:", "所以", "因此", "故", "则", "得", "解", "答"):
        if value.startswith(prefix):
            value = value[len(prefix):].lstrip()
    while value and value[-1] in "。；;，,、！!?":
        value = value[:-1].rstrip()
    return value


def parse_decimal(raw: dict) -> Decimal | None:
    latex = raw.get("latex")
    if isinstance(latex, str) and latex.strip():
        text = latex.strip()
        match = re.fullmatch(r"\s*(-?)\\frac\{(-?\d+)\}\{(\d+)\}\s*", text)
        if match:
            sign = -1 if match.group(1) == "-" else 1
            numerator = Decimal(match.group(2)) * sign
            denominator = Decimal(match.group(3))
            if denominator != 0:
                return numerator / denominator
        return _decimal(strip_noise(text))
    return _decimal(strip_noise(raw.get("text", "")))


def parse_boolean(raw: dict) -> bool | None:
    text = strip_noise(raw.get("text", ""))
    compact = re.sub(r"\s+", "", text)
    if compact in {"对", "正确", "√", "✓", "是", "真", "true", "t", "1"}:
        return True
    if compact in {"错", "错误", "×", "✗", "否", "假", "false", "f", "0"}:
        return False
    return None


def parse_options(raw: dict) -> list[str] | None:
    text = strip_noise(raw.get("text", ""))
    letters = [ch.upper() for ch in text if ch.isascii() and ch.isalpha()]
    if not letters or any(letter not in "ABCDEFGH" for letter in letters):
        return None
    return sorted(set(letters))


def normalize_expression(raw: dict) -> str | None:
    text = raw.get("latex")
    if not isinstance(text, str) or not text.strip():
        text = raw.get("text", "")
    value = text.strip()
    if not value:
        return None
    value = unicodedata.normalize("NFKC", value)
    value = value.replace(" ", "").replace("*", "")
    value = value.replace("^{", "^").replace("}", "")
    return value


def normalize_text(raw: dict) -> str:
    return " ".join(strip_noise(raw.get("text", "")).split())


def key_primary(key: dict) -> dict:
    return key["concrete"]["primary"]


def expected_objective_semantics(item: dict) -> tuple[str, object]:
    """返回（语义 kind, value）：options / boolean / number / expression / text / empty / not_asserted。"""
    raw = item["raw_answer"]
    confidence = item["recognition"]["confidence"]
    perturbation = item["perturbations"][0]
    if confidence < 0.75:
        return "not_asserted", None
    text = strip_noise(raw.get("text", ""))
    if not text and not raw.get("latex"):
        return "empty", None
    kind = item["kind"]
    form = item.get("answer_form")
    if kind in {"single_choice", "multiple_choice"}:
        options = parse_options(raw)
        return "options", options
    if kind == "true_false":
        return "boolean", parse_boolean(raw)
    if form == "number":
        return "number", parse_decimal(raw)
    if form == "expression":
        return "expression", normalize_expression(raw)
    if form == "text":
        return "text", normalize_text(raw)
    raise AssertionError(perturbation)


def derive_objective_expectation(item: dict) -> dict:
    kind = item["kind"]
    form = item.get("answer_form")
    confidence = item["recognition"]["confidence"]
    perturbation = item["perturbations"][0]
    key_variant = validate_answer_key(item["answer_key"], item["qid"])
    text_empty = not item["raw_answer"].get("text", "").strip() and not item["raw_answer"].get("latex")
    if confidence < 0.75:
        route, trace = "manual", "R1"
        return {
            "route": route,
            "rule_trace": trace,
            "judgement": "ungradable",
            "score_milli": None,
            "status": "manual",
            "review_reason": "ungradable",
            "manual_reason": "recognition_unavailable",
        }
    if text_empty and confidence >= 0.75:
        route, trace = "rule_only", "R0"
    elif key_variant == "rubric_only" or key_variant == "explicitly_omitted":
        route, trace = "rubric_itemized", "R4"
    elif kind in {"single_choice", "multiple_choice", "true_false"}:
        route, trace = "rule_only", "R5"
    elif kind in {"numeric_only", "fill_in_blank"}:
        route, trace = "rule_then_model", "R7"
    else:
        route, trace = "rule_then_model", "R7"

    # 比较语义
    judgement = "match"
    score = item["max_score_milli"]
    tier = None
    if route == "rule_only" and trace == "R0":
        judgement = "mismatch"
        score = 0
    elif key_variant != "concrete":
        judgement = "ungradable"
        score = None
    else:
        primary = key_primary(item["answer_key"])
        raw = item["raw_answer"]
        if kind in {"single_choice", "multiple_choice"}:
            expected_options = sorted(letter.upper() for letter in primary["options"])
            actual = parse_options(raw) or []
            rubric = item["rubric"]
            if rubric is not None and rubric["mode"] == "partial_choice":
                rule = rubric["partial_choice"]
                wrong = any(option not in expected_options for option in actual)
                selected_correct = len([option for option in actual if option in expected_options])
                if wrong:
                    judgement, score = "mismatch", 0
                elif selected_correct == len(expected_options):
                    judgement, score = "match", item["max_score_milli"]
                elif selected_correct == 0:
                    judgement, score = "mismatch", 0
                else:
                    tier = next(
                        (entry for entry in rule["tiers"] if entry["selected_correct"] == selected_correct),
                        None,
                    )
                    if tier is None:
                        judgement, score = "ungradable", None
                    else:
                        judgement, score = "partial", tier["points"]
            else:
                if expected_options == actual:
                    judgement, score = "match", item["max_score_milli"]
                else:
                    judgement, score = "mismatch", 0
        elif kind == "true_false":
            expected = parse_boolean({"text": primary["text"], "latex": None})
            actual = parse_boolean(raw)
            judgement = "match" if expected is not None and actual == expected else "mismatch"
            score = item["max_score_milli"] if judgement == "match" else 0
        elif form == "number":
            expected_value = _decimal(primary["number"]["value"])
            actual = parse_decimal(raw)
            if expected_value is None or actual is None:
                judgement, score = "ungradable", None
            elif actual == expected_value:
                judgement, score = "match", item["max_score_milli"]
            else:
                judgement, score = "mismatch", 0
        elif form == "expression":
            expected = normalize_expression({"latex": primary["expression"], "text": ""})
            actual = normalize_expression(raw)
            if expected is None or actual is None:
                judgement, score = "ungradable", None
            elif actual == expected:
                judgement, score = "match", item["max_score_milli"]
            else:
                judgement, score = "mismatch", 0
        elif form == "text":
            expected = " ".join(primary["text"].split())
            actual = normalize_text(raw)
            judgement = "match" if actual == expected else "mismatch"
            score = item["max_score_milli"] if judgement == "match" else 0
        else:
            raise AssertionError(kind)

    # 门禁（标准模式阈值：仅规则 0.85 / 规则优先 0.90 / 逐项 0.80）
    threshold = {"rule_only": 0.85, "rule_then_model": 0.90, "rubric_itemized": 0.80}[route]
    if confidence < threshold:
        status, review_reason = "needs_review", "low_confidence"
    elif judgement == "ungradable":
        status, review_reason = "needs_review", "ungradable"
    else:
        status, review_reason = "auto_accept", None
    return {
        "route": route,
        "rule_trace": trace,
        "judgement": judgement,
        "score_milli": score,
        "status": status,
        "review_reason": review_reason,
        "manual_reason": None,
        "partial_choice_tier": tier["selected_correct"] if tier else None,
    }


# ---------------------------------------------------------------------------
# 解答题独立聚合
# ---------------------------------------------------------------------------


def derive_solution_steps(item: dict) -> tuple[list[dict], int, str, str]:
    perturbation = item["perturbations"][0]
    rubric = item["rubric"]
    mode = rubric["mode"]
    matched = {
        "clean": [True, True, True],
        "symbol_variant": [True, True, True],
        "multi_solution": [True, True, True],
        "low_confidence": [True, True, True],
        "uncalibrated": [True, True, True],
        "rubric_only": [True, True, True],
        "missing_step": [True, False, True],
        "wrong_intermediate": [True, False, False],
        "unit_missing": [True, True, False],
        "answer_only": [False, False, True],
    }[perturbation]
    if mode == "all_or_nothing":
        all_matched = all(matched)
        effective = [all_matched] * 3
        score = rubric["total"] if all_matched else 0
    elif mode == "stepwise":
        effective = [False, False, False]
        changed = True
        while changed:
            changed = False
            for index, rubric_item in enumerate(rubric["items"]):
                if effective[index] or not matched[index]:
                    continue
                deps_ready = all(
                    any(
                        candidate["id"] == dependency and effective[position]
                        for position, candidate in enumerate(rubric["items"])
                    )
                    for dependency in rubric_item["depends_on"]
                )
                if deps_ready:
                    effective[index] = True
                    changed = True
        score = sum(
            rubric["items"][index]["points"]
            for index, ok in enumerate(effective)
            if ok
        )
    else:
        effective = list(matched)
        score = sum(
            rubric["items"][index]["points"]
            for index, ok in enumerate(effective)
            if ok
        )
    steps = [
        {
            "item_id": rubric["items"][index]["id"],
            "matched": matched[index],
            "effective": effective[index],
        }
        for index in range(3)
    ]
    return steps, score, mode, perturbation


def derive_solution_expectation(item: dict) -> dict:
    perturbation = item["perturbations"][0]
    if perturbation == "answer_key_omitted":
        return {
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
        }
    steps, score, mode, _ = derive_solution_steps(item)
    total = item["max_score_milli"]
    judgement = "match" if score == total else ("mismatch" if score == 0 else "partial")
    judge = item["judge"]
    confidence = round(
        min(
            item["recognition"]["confidence"],
            min(step["confidence"] for step in item["student_steps"]),
            judge["raw_confidence"],
        ),
        4,
    )
    route = "rubric_itemized"
    if item["kind"] in {"calculation", "proof"} and item["answer_key"] != "rubric_only":
        trace = "R6"
    else:
        trace = "R4"
    if confidence < 0.80:
        status, review_reason = "needs_review", "low_confidence"
    elif judge["calibration_id"] is None:
        status, review_reason = "needs_review", "uncalibrated_model"
    else:
        status, review_reason = "auto_accept", None
    return {
        "route": route,
        "rule_trace": trace,
        "aggregation": mode,
        "steps": steps,
        "score_milli": score,
        "judgement": judgement,
        "status": status,
        "review_reason": review_reason,
        "manual_reason": None,
        "calibration_id": judge["calibration_id"],
        "confidence": confidence,
    }


# ---------------------------------------------------------------------------
# 主检查
# ---------------------------------------------------------------------------


def matrix_count(items: list[dict]) -> dict:
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
        "kind_x_perturbation": {kind: dict(sorted(counter.items())) for kind, counter in sorted(table.items())},
    }


def check_students(document: dict, where: str) -> None:
    students = document.get("students")
    check(isinstance(students, list) and len(students) == 6, f"{where}: 合成学生应为 6 名")
    if not isinstance(students, list):
        return
    names = set()
    numbers = set()
    for student in students:
        check(isinstance(student, dict), f"{where}: 学生对象")
        if not isinstance(student, dict):
            continue
        check(set(student) <= {"student_id", "name", "student_no"}, f"{where}: 学生字段多余")
        names.add(student.get("name"))
        numbers.add(student.get("student_no"))
        check(student.get("student_id", "").startswith("stu-"), f"{where}: student_id 前缀")
    check(names == ALLOWED_STUDENTS, f"{where}: 学生姓名必须是 学生A..F")
    check(numbers == ALLOWED_STUDENT_NOS, f"{where}: 学号必须是 GOLD-000x")


def check_objective(document: dict, matrix: dict) -> None:
    items = document["items"]
    check(len(items) >= 500, f"objective 条目数 {len(items)} < 500")
    qids = set()
    kinds = Counter()
    perturbations = Counter()
    expected_kind_counts = matrix["by_kind"]
    check(set(expected_kind_counts) == OBJECTIVE_KINDS, f"objective 题型集合不符：{sorted(expected_kind_counts)}")
    for item in items:
        qid = item.get("qid")
        check(qid not in qids, f"{qid}: qid 重复")
        qids.add(qid)
        kind = item.get("kind")
        check(kind in OBJECTIVE_KINDS, f"{qid}: 题型非法 {kind}")
        kinds[kind] += 1
        form = item.get("answer_form")
        check(form is None or form in ANSWER_FORMS, f"{qid}: answer_form 非法 {form}")
        if kind in {"single_choice", "multiple_choice", "true_false"}:
            check(form is None, f"{qid}: 选择/判断题 answer_form 必须为 null")
        if kind == "numeric_only":
            check(form == "number", f"{qid}: numeric_only 的 answer_form 必须是 number")
        if kind == "fill_in_blank":
            check(form in {"expression", "text"}, f"{qid}: fill_in_blank 的 answer_form 必须是 expression/text")
        check(item.get("max_score_milli") == (6000 if kind == "multiple_choice" else 5000), f"{qid}: 满分不符")
        check(isinstance(item.get("stem_summary"), str) and item["stem_summary"], f"{qid}: stem_summary")
        check(item.get("student_id") in {student["student_id"] for student in document["students"]}, f"{qid}: student_id 非法")
        validate_answer_key(item.get("answer_key"), qid)
        validate_raw_answer(item.get("raw_answer"), qid)
        rubric = item.get("rubric")
        if rubric is not None:
            validate_rubric(rubric, item["max_score_milli"], qid)
            check(rubric["mode"] == "partial_choice", f"{qid}: objective 只允许 partial_choice 内嵌规则")
            primary = key_primary(item["answer_key"])
            check(len(primary["options"]) == rubric["partial_choice"]["correct_option_count"], f"{qid}: 档位正确选项数不一致")
        tags = item.get("perturbations")
        check(isinstance(tags, list) and len(tags) == 1, f"{qid}: perturbations 必须恰好 1 个")
        tag = tags[0] if isinstance(tags, list) and tags else None
        check(tag in OBJECTIVE_PERTURBATIONS, f"{qid}: 未知扰动 {tag}")
        perturbations[tag] += 1
        recognition = item.get("recognition")
        check(isinstance(recognition, dict) and 0 <= recognition.get("confidence", -1) <= 1, f"{qid}: recognition.confidence")
        expected = item.get("expected")
        check(isinstance(expected, dict), f"{qid}: expected 对象")
        if not isinstance(expected, dict):
            continue
        check(set(expected) <= {
            "route", "rule_trace", "normalized", "judgement", "score_milli", "status",
            "review_reason", "manual_reason", "confidence", "partial_choice_tier", "reason",
        }, f"{qid}: expected 字段多余")
        recomputed = derive_objective_expectation(item)
        for field in ("route", "rule_trace", "judgement", "score_milli", "status", "review_reason", "manual_reason"):
            check(expected.get(field) == recomputed[field], f"{qid}: expected.{field}={expected.get(field)!r} 应为 {recomputed[field]!r}")
        if expected.get("partial_choice_tier") != recomputed.get("partial_choice_tier"):
            check(False, f"{qid}: partial_choice_tier 应为 {recomputed.get('partial_choice_tier')}")
        score = expected.get("score_milli")
        if score is not None:
            check(isinstance(score, int) and 0 <= score <= item["max_score_milli"], f"{qid}: 分数越界")
        normalized = expected.get("normalized")
        check(isinstance(normalized, dict) and "kind" in normalized, f"{qid}: normalized 形状")
        if isinstance(normalized, dict):
            semantic_kind, semantic_value = expected_objective_semantics(item)
            check(normalized.get("kind") == semantic_kind, f"{qid}: normalized.kind={normalized.get('kind')} 应为 {semantic_kind}")
            if semantic_kind in {"options", "boolean", "number", "text"}:
                stored = normalized.get("value")
                if semantic_kind == "number":
                    stored_value = _decimal(str(stored)) if stored is not None else None
                    check(stored_value == semantic_value, f"{qid}: normalized.number 值不符")
                elif semantic_kind == "options":
                    check(stored == semantic_value, f"{qid}: normalized.options 值不符")
                else:
                    check(stored == semantic_value, f"{qid}: normalized value 不符")
        check(isinstance(item.get("split"), str) and item["split"] in {"dev", "eval"}, f"{qid}: split 非法")
        check(0 <= expected.get("confidence", -1) <= 1, f"{qid}: expected.confidence 越界")
    check(kinds == Counter(matrix["by_kind"]), "objective 题型计数与 index.json 不一致")
    check(len(perturbations) >= 6, f"objective 扰动类别不足 6：{sorted(perturbations)}")
    check(perturbations == Counter(matrix["by_perturbation"]), "objective 扰动分布与 index.json 不一致")
    for tag in ("partial_choice_2of3", "partial_choice_2opt", "ocr_noise", "missing", "latex_variant", "fullwidth", "spacing", "wrong_choice"):
        check(perturbations.get(tag, 0) > 0, f"objective 缺少扰动覆盖：{tag}")
    for item in items:
        tag = item["perturbations"][0]
        if tag in {"partial_choice_2of3", "partial_choice_2opt"}:
            check(item["kind"] == "multiple_choice", f"{item['qid']}: 部分给分只允许多选")
            check(item["rubric"] is not None, f"{item['qid']}: 部分给分必须有档位规则")
        if tag == "latex_variant":
            check(item.get("answer_form") in {"number", "expression"}, f"{item['qid']}: latex_variant 仅数值/表达式")
        if tag == "ocr_noise":
            check(item["recognition"]["confidence"] == 0.70, f"{item['qid']}: ocr_noise 置信度应为 0.70")
        if tag == "ocr_noise_review":
            check(item["recognition"]["confidence"] == 0.80, f"{item['qid']}: ocr_noise_review 置信度应为 0.80")


def check_solution(document: dict, matrix: dict) -> None:
    items = document["items"]
    check(len(items) >= 200, f"solution 条目数 {len(items)} < 200")
    qids = set()
    kinds = Counter()
    perturbations = Counter()
    for item in items:
        qid = item.get("qid")
        check(qid not in qids, f"{qid}: qid 重复")
        qids.add(qid)
        kind = item.get("kind")
        check(kind in SOLUTION_KINDS, f"{qid}: 题型非法 {kind}")
        kinds[kind] += 1
        max_score = item.get("max_score_milli")
        check(isinstance(max_score, int) and max_score > 0, f"{qid}: max_score")
        tags = item.get("perturbations")
        check(isinstance(tags, list) and len(tags) == 1, f"{qid}: perturbations 必须恰好 1 个")
        tag = tags[0] if isinstance(tags, list) and tags else None
        check(tag in SOLUTION_PERTURBATIONS, f"{qid}: 未知扰动 {tag}")
        perturbations[tag] += 1
        validate_answer_key(item.get("answer_key"), qid)
        validate_raw_answer(item.get("raw_answer"), qid)
        rubric = item.get("rubric")
        if tag == "answer_key_omitted":
            check(rubric is None, f"{qid}: 「略」题不应有规则")
        else:
            check(rubric is not None, f"{qid}: 解答题必须有分步 rubric")
            if rubric is not None:
                validate_rubric(rubric, max_score, qid)
                check(len(rubric["items"]) == 3, f"{qid}: 本数据集固定 3 个评分项")
                check(rubric["mode"] in {"stepwise", "additive", "all_or_nothing"}, f"{qid}: 聚合模式非法")
                if rubric["mode"] == "stepwise":
                    check(rubric["items"][1]["depends_on"] == [rubric["items"][0]["id"]], f"{qid}: 步骤2依赖")
                    check(rubric["items"][2]["depends_on"] == [rubric["items"][1]["id"]], f"{qid}: 步骤3依赖")
        steps = item.get("student_steps")
        check(isinstance(steps, list), f"{qid}: student_steps 数组")
        if isinstance(steps, list) and steps:
            check([step.get("ordinal") for step in steps] == list(range(1, len(steps) + 1)), f"{qid}: 步骤序号必须连续")
            for step in steps:
                check(isinstance(step.get("text"), str) and step["text"].strip(), f"{qid}: 步骤文本非空")
                check(0 <= step.get("confidence", -1) <= 1, f"{qid}: 步骤置信度")
        judge = item.get("judge")
        check(isinstance(judge, dict), f"{qid}: judge 对象")
        if isinstance(judge, dict):
            check(set(judge) <= {"raw_confidence", "calibration_id", "model_id", "model_version"}, f"{qid}: judge 字段多余")
            check(0 <= judge.get("raw_confidence", -1) <= 1, f"{qid}: judge.raw_confidence")
            check(judge.get("calibration_id") is None or isinstance(judge.get("calibration_id"), str), f"{qid}: calibration_id")
        expected = item.get("expected")
        check(isinstance(expected, dict), f"{qid}: expected 对象")
        if not isinstance(expected, dict):
            continue
        check(set(expected) <= {
            "route", "rule_trace", "aggregation", "steps", "score_milli", "judgement",
            "status", "review_reason", "manual_reason", "calibration_id", "confidence", "reason",
        }, f"{qid}: expected 字段多余")
        recomputed = derive_solution_expectation(item)
        for field in ("route", "rule_trace", "aggregation", "score_milli", "judgement", "status", "review_reason", "manual_reason", "calibration_id", "confidence"):
            check(expected.get(field) == recomputed[field], f"{qid}: expected.{field}={expected.get(field)!r} 应为 {recomputed[field]!r}")
        if tag != "answer_key_omitted":
            check(expected.get("steps") == recomputed["steps"], f"{qid}: expected.steps 与独立聚合不一致")
            score = expected.get("score_milli")
            check(isinstance(score, int) and 0 <= score <= max_score, f"{qid}: 分数越界")
            if expected.get("judgement") == "match":
                check(score == max_score, f"{qid}: match 必须满分")
            if expected.get("judgement") == "mismatch":
                check(score == 0, f"{qid}: mismatch 必须 0 分")
        else:
            check(expected.get("score_milli") is None, f"{qid}: manual 不应有分数")
            check(expected.get("status") == "manual", f"{qid}: 略题状态应为 manual")
    check(kinds == Counter(matrix["by_kind"]), "solution 题型计数与 index.json 不一致")
    check(len(perturbations) >= 6, f"solution 扰动类别不足 6：{sorted(perturbations)}")
    check(perturbations == Counter(matrix["by_perturbation"]), "solution 扰动分布与 index.json 不一致")
    for tag in ("symbol_variant", "multi_solution", "missing_step", "wrong_intermediate", "unit_missing", "answer_only", "low_confidence", "uncalibrated", "rubric_only", "answer_key_omitted"):
        check(perturbations.get(tag, 0) > 0, f"solution 缺少扰动覆盖：{tag}")
    for item in items:
        if (
            item["kind"] in {"calculation", "proof"}
            and item["perturbations"][0] not in {"answer_key_omitted", "rubric_only"}
        ):
            check(item["answer_key"] != "rubric_only", f"{item['qid']}: R6 需要具体答案键")
        if item["kind"] == "short_answer" and item["perturbations"][0] not in {"rubric_only", "answer_key_omitted"}:
            check(item["answer_key"] == "rubric_only", f"{item['qid']}: 简答题应为仅评分标准")


def check_index(index: dict, objective: dict, solution: dict) -> None:
    check(index.get("schema_version") == 1, "index.schema_version")
    check(index["objective"]["count"] == len(objective["items"]), "index.objective.count")
    check(index["solution"]["count"] == len(solution["items"]), "index.solution.count")
    obj_matrix = matrix_count(objective["items"])
    sol_matrix = matrix_count(solution["items"])
    for field in ("by_kind", "by_perturbation", "by_status", "by_route"):
        check(index["objective"][field] == obj_matrix[field], f"index.objective.{field} 不一致")
        check(index["solution"][field] == sol_matrix[field], f"index.solution.{field} 不一致")
    check(index["objective"]["kind_x_perturbation"] == obj_matrix["kind_x_perturbation"], "index.objective.matrix")
    check(index["solution"]["kind_x_perturbation"] == sol_matrix["kind_x_perturbation"], "index.solution.matrix")
    for name in ("objective-500.json", "solution-200.json"):
        check(index["files"].get(name) == sha256_file(GOLD / name), f"index.files[{name}] sha256 不一致")


def check_readme(objective: dict, solution: dict) -> None:
    text = (GOLD / "README.md").read_text(encoding="utf-8")
    check("合成数据" in text and "不是真实试卷" in text, "README 缺少诚实声明")
    check(str(len(objective["items"])) in text, "README 缺少 objective 条目数")
    check(str(len(solution["items"])) in text, "README 缺少 solution 条目数")
    for label in sorted(OBJECTIVE_PERTURBATIONS | SOLUTION_PERTURBATIONS):
        check(label in text, f"README 缺少扰动标签 {label}")
    for kind in sorted(OBJECTIVE_KINDS | SOLUTION_KINDS | {"answer_form"}):
        check(kind in text, f"README 缺少题型 {kind}")
    check("R0" in text and "R4" in text and "R7" in text, "README 缺少规则映射")
    check("auto_accept" in text and "needs_review" in text and "manual" in text, "README 缺少状态定义")


def check_determinism() -> None:
    with tempfile.TemporaryDirectory(prefix="gold-verify-") as temporary:
        temporary_path = Path(temporary)
        result = subprocess.run(
            [sys.executable, str(GENERATOR), "--out", str(temporary_path)],
            capture_output=True,
            text=True,
            check=False,
        )
        check(result.returncode == 0, f"重新生成失败：{result.stderr[-400:]}")
        if result.returncode != 0:
            return
        for name in ("objective-500.json", "solution-200.json", "index.json", "README.md"):
            generated = temporary_path / name
            stored = GOLD / name
            check(generated.is_file(), f"重新生成缺少 {name}")
            if generated.is_file():
                check(sha256_file(generated) == sha256_file(stored), f"{name} 字节不稳定（重新生成与仓库不一致）")


def check_hygiene(objective: dict, solution: dict) -> None:
    for document, where in ((objective, "objective"), (solution, "solution")):
        text = json.dumps(document, ensure_ascii=False)
        for banned in BANNED_NAMES:
            check(banned not in text, f"{where}: 出现真实/样例姓名 {banned}")
        check("学校" not in text or "学校" in document.get("notes", [""])[0], f"{where}: 不应含学校信息")
    files = list(GOLD.glob("*"))
    check(all(path.suffix in {".json", ".md"} for path in files), "gold 目录不得包含图片/二进制")
    total = sum(path.stat().st_size for path in files)
    check(total < 3 * 1024 * 1024, f"gold 总大小 {total} 超过 3MB")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--skip-bytes", action="store_true", help="跳过重新生成/字节比对")
    args = parser.parse_args()

    required = ["objective-500.json", "solution-200.json", "index.json", "README.md"]
    for name in required:
        check((GOLD / name).is_file(), f"缺少文件 fixtures/gold/{name}")
    if FAILS:
        for failure in FAILS:
            print(f"[FAIL] {failure}")
        return 1

    objective = json.loads((GOLD / "objective-500.json").read_text(encoding="utf-8"))
    solution = json.loads((GOLD / "solution-200.json").read_text(encoding="utf-8"))
    index = json.loads((GOLD / "index.json").read_text(encoding="utf-8"))
    check(objective.get("schema_version") == 1 and solution.get("schema_version") == 1, "schema_version 必须为 1")
    check(objective.get("dataset") == "objective" and solution.get("dataset") == "solution", "dataset 字段")
    check_students(objective, "objective")
    check_students(solution, "solution")
    check_objective(objective, matrix_count(objective["items"]))
    check_solution(solution, matrix_count(solution["items"]))
    check_index(index, objective, solution)
    check_readme(objective, solution)
    check_hygiene(objective, solution)
    if not args.skip_bytes:
        check_determinism()

    total = sum(path.stat().st_size for path in GOLD.glob("*"))
    print(f"检查项：{CHECKS}，失败：{len(FAILS)}")
    print(f"条目：objective={len(objective['items'])}，solution={len(solution['items'])}；体积={total / 1024:.1f} KiB")
    if FAILS:
        for failure in FAILS[:40]:
            print(f"[FAIL] {failure}")
        if len(FAILS) > 40:
            print(f"... 其余 {len(FAILS) - 40} 条略")
        return 1
    print("[ok] gold 自检全部通过")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
