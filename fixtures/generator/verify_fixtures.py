#!/usr/bin/env python3
"""fixtures/ 自检：结构、期望一致性、图片/EXIF、夹具键格式、MANIFEST、体积。

只做静态与 Python 可验证的检查；「真实 capture 导入」「eg-inference 键命中/负向拒绝」
由 `fixtures/generator/smoke/` 的 Rust 冒烟程序完成（见 README §5）。

退出码 0 = 全部通过；非 0 = 有失败项（逐条打印）。
"""

from __future__ import annotations

import hashlib
import json
import struct
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
FIX = REPO / "fixtures"
ALPHABET = set("0123456789ABCDEFGHJKMNPQRSTVWXYZ")
FAILS: list[str] = []
CHECKS = 0


def check(condition: bool, message: str) -> None:
    global CHECKS
    CHECKS += 1
    if not condition:
        FAILS.append(message)


def load(path: Path):
    return json.loads(path.read_text(encoding="utf-8"))


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def is_ulid(text: str) -> bool:
    return isinstance(text, str) and len(text) == 26 and all(ch in ALPHABET for ch in text)


def png_size(data: bytes) -> tuple[int, int]:
    assert data[:8] == b"\x89PNG\r\n\x1a\n"
    width, height = struct.unpack(">II", data[16:24])
    return width, height


def jpeg_size(data: bytes) -> tuple[int, int]:
    assert data[:2] == b"\xff\xd8"
    index = 2
    while index < len(data) - 9:
        if data[index] != 0xFF:
            index += 1
            continue
        marker = data[index + 1]
        if marker in (0xC0, 0xC1, 0xC2):
            height, width = struct.unpack(">HH", data[index + 5 : index + 9])
            return width, height
        length = struct.unpack(">H", data[index + 2 : index + 4])[0]
        index += 2 + length
    raise ValueError("JPEG 中未找到 SOF 尺寸")


def jpeg_exif_orientation(data: bytes) -> int | None:
    """读取 APP1 Exif 中的 Orientation（0x0112）；没有则 None。"""
    assert data[:2] == b"\xff\xd8"
    index = 2
    while index < len(data) - 4:
        if data[index] != 0xFF:
            index += 1
            continue
        marker = data[index + 1]
        if marker == 0xE1:
            length = struct.unpack(">H", data[index + 2 : index + 4])[0]
            segment = data[index + 4 : index + 2 + length]
            if segment.startswith(b"Exif\x00\x00"):
                tiff = segment[6:]
                endian = tiff[:2]
                if endian == b"II":
                    fmt = "<"
                elif endian == b"MM":
                    fmt = ">"
                else:
                    return None
                ifd_offset = struct.unpack(fmt + "I", tiff[4:8])[0]
                count = struct.unpack(fmt + "H", tiff[ifd_offset : ifd_offset + 2])[0]
                for entry in range(count):
                    base = ifd_offset + 2 + entry * 12
                    tag, typ, _count = struct.unpack(fmt + "HHI", tiff[base : base + 8])
                    if tag == 0x0112 and typ == 3:
                        return struct.unpack(fmt + "H", tiff[base + 8 : base + 10])[0]
            return None
        if marker in (0xDA, 0xD9):
            return None
        length = struct.unpack(">H", data[index + 2 : index + 4])[0]
        index += 2 + length
    return None


def main() -> int:  # noqa: C901 - 自检按域分块，保持顺序可读
    # ---------------------------------------------------------------- 0. 必需文件
    required = [
        "README.md",
        "MANIFEST.sha256",
        "paper-20q/paper.json",
        "paper-20q/answer-key.json",
        "paper-20q/rubric.json",
        "paper-20q/template.json",
        "paper-20q/classroom.json",
        "paper-20q/students.json",
        "paper-20q/roster.csv",
        "paper-20q/expected/grades-expected.json",
        "images/manifest.json",
        "ocr/index.json",
        "judge/index.json",
        "generator/generate_fixtures.py",
        "generator/regenerate.sh",
        "generator/verify_fixtures.py",
        "generator/keygen/src/main.rs",
        "generator/smoke/src/main.rs",
        "generator/smoke.sh",
    ]
    for relative in required:
        check((FIX / relative).is_file(), f"缺少必需文件：{relative}")

    # ---------------------------------------------------------------- 1. 试卷
    paper = load(FIX / "paper-20q/paper.json")
    classroom = load(FIX / "paper-20q/classroom.json")
    students_doc = load(FIX / "paper-20q/students.json")
    questions = paper["questions"]
    check(len(questions) == 20, f"题目数应为 20，实际 {len(questions)}")
    check(paper["total_score"] == 150000, f"总分应为 150000 毫元，实际 {paper['total_score']}")
    check(paper["key_version"] == 1 and paper["rubric_version"] == 1, "key/rubric 版本应为 1")
    check(paper["subject"] == "math", "学科应为 math")
    check(paper["classroom_id"] == classroom["id"], "paper.classroom_id 与 classroom.json 不一致")
    check([q["ordinal"] for q in questions] == list(range(1, 21)), "题号必须 1–20 连续")
    check(len({q["label"] for q in questions}) == 20, "题目标签必须唯一")
    check(len({q["id"] for q in questions}) == 20, "题目 id 必须唯一")
    kinds = {}
    for question in questions:
        kinds[question["kind"]] = kinds.get(question["kind"], 0) + 1
        check(question["paper_id"] == paper["id"], f"题目 {question['ordinal']} paper_id 不一致")
        check(is_ulid(question["id"]), f"题目 {question['ordinal']} id 不是合法 ULID")
        check(question["max_score"] > 0, f"题目 {question['ordinal']} 满分必须为正")
    expected_kinds = {
        "single_choice": 8,
        "multiple_choice": 3,
        "fill_in_blank": 4,
        "calculation": 3,
        "proof": 1,
        "short_answer": 1,
    }
    check(kinds == expected_kinds, f"题型构成不符：{kinds} != {expected_kinds}")
    check(
        sum(q["max_score"] for q in questions) == paper["total_score"],
        "各题满分之和必须等于总分",
    )

    # 答案键投影
    answer_key = load(FIX / "paper-20q/answer-key.json")
    check(answer_key["paper_id"] == paper["id"], "answer-key.paper_id 不一致")
    check(answer_key["key_version"] == paper["key_version"], "answer-key.key_version 不一致")
    check(len(answer_key["questions"]) == 20, "answer-key 应覆盖 20 题")
    key_by_ordinal = {entry["ordinal"]: entry for entry in answer_key["questions"]}
    for question in questions:
        entry = key_by_ordinal.get(question["ordinal"])
        check(entry is not None, f"answer-key 缺题 {question['ordinal']}")
        if entry:
            check(entry["question_id"] == question["id"], "answer-key question_id 不一致")
            check(entry["answer_key"] == question["key"], f"题 {question['ordinal']} 答案键与 paper 不一致")
    check(any(q["key"] == "rubric_only" for q in questions), "必须存在「仅规则」题")
    omitted = [q for q in questions if isinstance(q["key"], dict) and "explicitly_omitted" in q["key"]]
    check(len(omitted) == 1, "必须恰好 1 道「略」题（explicitly_omitted）")
    if omitted:
        check(omitted[0]["kind"] == "short_answer", "「略」题题型应为 short_answer")

    # ---------------------------------------------------------------- 2. 规则
    rubric_doc = load(FIX / "paper-20q/rubric.json")
    check(rubric_doc["paper_id"] == paper["id"], "rubric.paper_id 不一致")
    check(rubric_doc["rubric_version"] == paper["rubric_version"], "rubric_version 不一致")
    check(rubric_doc["status"] == "active", "rubric.json status 必须 active")
    check(rubric_doc["confirmed_by"] == "local-teacher", "rubric 确认人应为 local-teacher")
    check(rubric_doc["confirmed_at"].endswith("Z"), "rubric 确认时间应为 RFC3339 UTC")
    question_by_id = {q["id"]: q for q in questions}
    rubric_by_id = {}
    for entry in rubric_doc["questions"]:
        rubric = entry["rubric"]
        question = question_by_id.get(entry["question_id"])
        check(question is not None, f"rubric 引用未知题目 {entry['question_id']}")
        if not question:
            continue
        rubric_by_id[entry["question_id"]] = rubric
        check(entry["ordinal"] == question["ordinal"], "rubric ordinal 不一致")
        check(entry["kind"] == question["kind"], "rubric kind 不一致")
        check(entry["max_score_milli"] == question["max_score"], "rubric max_score 不一致")
        check(rubric["total"] == question["max_score"], "rubric.total 必须等于题目满分")
        check(len(rubric["items"]) >= 1, "rubric 至少 1 个评分项")
        check(all(item["points"] > 0 for item in rubric["items"]), "评分项分值必须为正")
        check(
            sum(item["points"] for item in rubric["items"]) == rubric["total"],
            f"题 {entry['ordinal']} rubric 各评分项之和必须等于 total",
        )
        item_ids = {item["id"] for item in rubric["items"]}
        check(len(item_ids) == len(rubric["items"]), "评分项 id 必须唯一")
        for item in rubric["items"]:
            check(set(item["depends_on"]) <= item_ids, "评分项依赖必须指向同题评分项")
        mode = rubric["mode"]
        check(mode in {"additive", "all_or_nothing", "stepwise", "partial_choice"}, "未知计分模式")
        partial = rubric["partial_choice"]
        check(
            (partial is not None) == (mode == "partial_choice"),
            f"题 {entry['ordinal']} partial_choice 必须与非空 iff partial_choice 模式",
        )
        if partial is not None:
            check(question["kind"] == "multiple_choice", "部分给分只允许多选题")
            count = partial["correct_option_count"]
            check(count in (2, 3), "correct_option_count 必须为 2 或 3")
            tiers = partial["tiers"]
            selected = [tier["selected_correct"] for tier in tiers]
            check(selected == list(range(1, count)), f"档位必须覆盖 1..{count - 1} 且升序不跳档：{selected}")
            check(partial["wrong_selection_zero"] is True, "错选归零必须为 true")
            check(partial["total_milli"] == rubric["total"], "partial_choice.total_milli 必须等于 rubric.total")
            check(partial["source"] in {"official", "template_data", "teacher_custom"}, "未知档位来源")
            for tier in tiers:
                check(0 < tier["points"] < rubric["total"], "档位得分必须在 (0, 满分) 开区间")
        # 仅规则题必须有规则；「略」题不要求规则。
        if question["key"] == "rubric_only":
            check(True, "rubric_only 题已由 rubric 条目覆盖")
    for question in questions:
        if question["key"] == "rubric_only":
            check(question["id"] in rubric_by_id, "rubric_only 题必须有生效规则")
        if isinstance(question["key"], dict) and "explicitly_omitted" in question["key"]:
            check(question["id"] not in rubric_by_id, "「略」题不应携带规则（答案键已明确省略）")
    check(
        any(rubric["mode"] == "stepwise" for rubric in rubric_by_id.values()),
        "必须存在分步给分规则",
    )
    check(
        sum(1 for rubric in rubric_by_id.values() if rubric["mode"] == "partial_choice") == 3,
        "必须有 3 道多选部分给分规则",
    )

    # 模板
    template = load(FIX / "paper-20q/template.json")
    check(template["paper_id"] == paper["id"], "template.paper_id 不一致")
    check(template["version"] == paper["key_version"], "template.version 必须等于 paper.key_version")
    check(template["page_count"] == 2, "template.page_count 应为 2")
    regions = template["regions"]
    check(len(regions) == 20, f"模板应覆盖 20 题区域，实际 {len(regions)}")
    per_question = {}
    for region in regions:
        bbox = region["bbox"]
        check(len(bbox) == 4, "bbox 必须 4 个数")
        x, y, w, h = bbox
        check(0 <= x < 1 and 0 <= y < 1 and 0 < w <= 1 and 0 < h <= 1, "bbox 必须在 0–1")
        check(x + w <= 1.0001 and y + h <= 1.0001, "bbox 不得越界")
        check(region["page_no"] in (1, 2), "模板页号必须 1/2")
        check(region["question_id"] in question_by_id, "模板区域引用未知题目")
        per_question.setdefault(region["question_id"], []).append(region["page_no"])
    check(all(len(pages) == 1 for pages in per_question.values()), "每题必须恰有 1 个模板区域")
    page1 = {region["question_id"] for region in regions if region["page_no"] == 1}
    page2 = {region["question_id"] for region in regions if region["page_no"] == 2}
    check(len(page1) == 11 and len(page2) == 9, f"页区域数应为 11/9，实际 {len(page1)}/{len(page2)}")
    check(page1 | page2 == set(question_by_id), "两页区域必须覆盖全部题目")

    # ---------------------------------------------------------------- 3. 名单
    check(len(students_doc) == 3, "必须 3 名学生")
    check(len({student["student_no"] for student in students_doc}) == 3, "学号必须唯一")
    for student in students_doc:
        check(student["classroom_id"] == classroom["id"], "学生班级不一致")
        check(is_ulid(student["id"]), "学生 id 不是 ULID")
        check(student["name"].strip() != "" and student["student_no"].strip() != "", "姓名/学号不得为空")
        check(student["source"] == "roster_import", "学生来源应为 roster_import")
    roster_lines = (FIX / "paper-20q/roster.csv").read_text(encoding="utf-8").strip().splitlines()
    check(roster_lines[0] == "student_no,name,file_stem,expected_identity,note", "roster.csv 表头不符")
    check(len(roster_lines) == 4, "roster.csv 应有 3 行数据")
    roster_nos = {line.split(",")[0] for line in roster_lines[1:]}
    check(roster_nos == {student["student_no"] for student in students_doc}, "roster.csv 学号与学生表不一致")

    # ---------------------------------------------------------------- 4. 期望结果
    expected = load(FIX / "paper-20q/expected/grades-expected.json")
    check(expected["paper"]["id"] == paper["id"], "expected.paper.id 不一致")
    check(expected["paper"]["total_score_milli"] == 150000, "expected 总分不一致")
    check(expected["paper"]["question_count"] == 20, "expected 题数不一致")
    check(expected["rubric_status"] == "active", "expected.rubric_status 应为 active")
    check(len(expected["students"]) == 3, "expected 必须覆盖 3 名学生")
    expected_nos = {student["student_no"] for student in expected["students"]}
    check(expected_nos == {student["student_no"] for student in students_doc}, "expected 学生名单不一致")
    rubric_item_ids = {
        question_id: {item["id"] for item in rubric["items"]}
        for question_id, rubric in rubric_by_id.items()
    }
    partial_choice_tiers = {
        question_id: {tier["points"] for tier in rubric["partial_choice"]["tiers"]}
        for question_id, rubric in rubric_by_id.items()
        if rubric["partial_choice"]
    }
    saw_partial_choice = False
    saw_low_confidence = False
    saw_conflict = False
    saw_manual_omitted = False
    saw_rubric_only = False
    for student in expected["students"]:
        identity = student["identity"]
        check(identity["status"] == "confirmed", "3 名主学生身份应为 confirmed")
        check(identity["source"] == "file_name", "身份来源应为 file_name")
        check(identity["confidence"] == 1.0, "文件名身份置信度必须 1.0")
        body = student["expected"]
        check(body["max_total_milli"] == 150000, "max_total_milli 应为 150000")
        questions_expected = body["questions"]
        check(len(questions_expected) == 20, f"{student['student_no']} 期望必须覆盖 20 题")
        check([q["ordinal"] for q in questions_expected] == list(range(1, 21)), "期望题号必须 1–20")
        total = 0
        for item in questions_expected:
            question = question_by_id[item["question_id"]]
            check(item["ordinal"] == question["ordinal"], "期望 ordinal 与 question_id 不一致")
            check(item["max_score_milli"] == question["max_score"], "期望 max_score 不一致")
            check(item["kind"] == question["kind"], "期望 kind 不一致")
            judgement = item["judgement"]
            score = item["score_milli"]
            if judgement == "match":
                check(score == question["max_score"], "match 必须满分")
            elif judgement == "mismatch":
                check(score == 0, "mismatch 必须 0 分")
            elif judgement == "partial":
                check(
                    score is not None and 0 < score < question["max_score"],
                    "partial 必须在 (0, 满分) 开区间",
                )
                if item["route"] == "rule_only":
                    # 多选部分给分：得分必须等于某档位分值
                    check(
                        question["id"] in partial_choice_tiers
                        and score in partial_choice_tiers[question["id"]],
                        f"{student['student_no']} 题 {item['ordinal']} 部分给分不在档位内",
                    )
                    saw_partial_choice = True
                else:
                    items = rubric_item_ids.get(question["id"], set())
                    used = set(item["satisfied"]) | set(item["missing"])
                    check(used <= items, "satisfied/missing 必须引用同题评分项")
                    satisfied_points = sum(
                        entry["points"]
                        for entry in rubric_by_id[question["id"]]["items"]
                        if entry["id"] in set(item["satisfied"])
                    )
                    check(satisfied_points == score, "partial 得分必须等于 satisfied 评分项之和")
                    check(len(item["missing"]) > 0, "partial 必须给出 missing 项")
                    saw_rubric_only = True
            elif judgement == "ungradable":
                check(score is None, "ungradable 不应有分数")
                check(item["route"] == "manual", "ungradable 必须走 manual 路由")
                check(item["manual_reason"] == "answer_key_omitted", "「略」题 manual_reason 应为 answer_key_omitted")
                saw_manual_omitted = True
            else:
                check(judgement is None, "judgement 只允许 match/partial/mismatch/ungradable/null")
                check(score is None, "null judgement 不应有分数")
                check(item["status"] == "needs_review", "null judgement 必须 needs_review")
                check(item["review_reason"] == "conflict", "null judgement 应记 conflict")
                saw_conflict = True
            if item["status"] == "needs_review" and item["review_reason"] == "low_confidence":
                check(item["confidence"] < 0.75, "low_confidence 期望的置信度应低于 0.75")
                saw_low_confidence = True
            if item["status"] == "auto_accepted":
                check(item["review_reason"] is None, "auto_accepted 不应带 review_reason")
            if not item["calibrated"] and item["status"] == "auto_accepted":
                check(False, "未校准模型判定不得自动接受")
            check(item["evidence"] in {"rule", "judge", "rule+judge", "manual"}, "未知 evidence")
            total += score or 0
        check(total == body["total_milli"], f"{student['student_no']} 总分与逐题得分之和不一致")
        needs_review = any(item["status"] == "needs_review" for item in questions_expected)
        check(
            body["status"] == ("needs_review" if needs_review else "auto_accepted"),
            f"{student['student_no']} 学生级状态应由逐题状态推导",
        )
    check(saw_partial_choice, "缺少多选部分给分期望")
    check(saw_low_confidence, "缺少低置信复核期望")
    check(saw_conflict, "缺少冲突未决期望")
    check(saw_manual_omitted, "缺少「略」题仅人工期望")
    check(saw_rubric_only, "缺少分步/规则判定期望")
    extra = expected["extra_samples"]
    check(len(extra) == 3, "extra_samples 应有 3 条（相机名/重复/EXIF）")
    kinds = {entry["expected"].get("duplicate") or entry["expected"].get("exif_orientation") for entry in extra}
    check(any(entry["expected"].get("identity") == "unresolved" for entry in extra), "缺少身份未决样例")
    check(any(entry["expected"].get("duplicate") == "same_batch_duplicate" for entry in extra), "缺少重复字节样例")
    check(any(entry["expected"].get("exif_orientation") == 6 for entry in extra), "缺少 EXIF 旋转样例")
    del kinds

    # ---------------------------------------------------------------- 5. 图片
    images_doc = load(FIX / "images/manifest.json")
    check(images_doc["page_width"] == 2480 and images_doc["page_height"] == 3507, "答卷页尺寸应为 A4@300DPI（2480×3507）")
    files = images_doc["files"]
    check(len(files) == 9, f"应有 9 张图片，实际 {len(files)}")
    seen_names = set()
    duplicates = {}
    for entry in files:
        path = FIX / entry["file"]
        check(path.is_file(), f"图片缺失：{entry['file']}")
        if not path.is_file():
            continue
        data = path.read_bytes()
        check(sha256(path) == entry["sha256"], f"{entry['file']} sha256 与 manifest 不一致")
        check(len(data) == entry["bytes"], f"{entry['file']} 字节数与 manifest 不一致")
        check(entry["file"] not in seen_names, f"图片重复登记：{entry['file']}")
        seen_names.add(entry["file"])
        duplicates.setdefault(sha256(path), []).append(entry["file"])
        is_png = data[:8] == b"\x89PNG\r\n\x1a\n"
        is_jpeg = data[:2] == b"\xff\xd8"
        check(is_png or is_jpeg, f"{entry['file']} 不是 PNG/JPEG（capture 白名单内）")
        if is_png:
            width, height = png_size(data)
            check(entry["media_type"] == "image/png", f"{entry['file']} media_type 不一致")
        else:
            width, height = jpeg_size(data)
            check(entry["media_type"] == "image/jpeg", f"{entry['file']} media_type 不一致")
        check((width, height) == (entry["width"], entry["height"]), f"{entry['file']} 尺寸与 manifest 不一致")
        expected_image = entry["expected"]
        check(expected_image["format"] in {"png", "jpg"}, "图片期望格式非法")
        check(expected_image["quality_verdict"] == "good_or_acceptable", "质量期望应非拒收")
        if expected_image["kind"] == "exif_rotated":
            orientation = jpeg_exif_orientation(data)
            check(orientation == 6, f"{entry['file']} EXIF Orientation 应为 6，实际 {orientation}")
            check(
                (entry["width"], entry["height"]) == (PAGE_SWAPPED := (3507, 2480)),
                f"{entry['file']} EXIF 样例存储尺寸应为 3507×2480",
            )
            check(expected_image["rotation_applied"] == 90, "EXIF 样例期望 rotation_applied=90")
            check(
                expected_image["upright_width"] == 2480 and expected_image["upright_height"] == 3507,
                "EXIF 样例校正后应为 2480×3507",
            )
            del PAGE_SWAPPED
        elif is_jpeg:
            check(jpeg_exif_orientation(data) in (None, 1), f"{entry['file']} 不应带旋转 EXIF")
    duplicate_files = [names for names in duplicates.values() if len(names) > 1]
    check(len(duplicate_files) == 1, f"应恰有 1 组重复字节图片，实际 {duplicate_files}")
    if duplicate_files:
        check(
            set(duplicate_files[0]) == {"images/张三_p2.png", "images/张三_p2-copy.png"},
            f"重复字节组应为张三_p2 与副本：{duplicate_files[0]}",
        )

    # ---------------------------------------------------------------- 6. OCR 夹具
    ocr_index = load(FIX / "ocr/index.json")
    check(ocr_index["kind"] == "ocr", "ocr/index.json kind 应为 ocr")
    backend = ocr_index["backend"]
    check(backend["prompt_schema_version"] == "1", "OCR 提示词 schema 版本应为 1")
    records = ocr_index["records"]
    check(len(records) == 7, f"OCR 记录应为 7 条，实际 {len(records)}")
    image_by_file = {entry["file"]: entry for entry in files}
    keys = set()
    formula_blocks = 0
    for record in records:
        fixture_path = FIX / record["fixture"]
        prompt_path = FIX / record["prompt_file"]
        input_path = FIX / record["ocr_input_image"]
        check(fixture_path.is_file(), f"OCR fixture 缺失：{record['fixture']}")
        check(prompt_path.is_file(), f"OCR prompt 缺失：{record['prompt_file']}")
        check(input_path.is_file(), f"OCR 输入图缺失：{record['ocr_input_image']}")
        if not (fixture_path.is_file() and prompt_path.is_file() and input_path.is_file()):
            continue
        check(sha256(prompt_path) == record["prompt_sha256"], "OCR prompt_sha256 不一致")
        check(sha256(input_path) == record["ocr_input_sha256"], "OCR input_sha256 不一致")
        source = image_by_file.get(record["source_image"])
        check(source is not None, f"OCR 记录引用未登记图片：{record['source_image']}")
        if source:
            check(sha256(FIX / record["source_image"]) == record["source_sha256"], "OCR source_sha256 不一致")
        width, height = png_size(input_path.read_bytes())
        check((width, height) == (record["ocr_input_width"], record["ocr_input_height"]), "OCR 输入图尺寸与索引不一致")
        check(max(width, height) == 2000, "OCR 输入图长边必须为 2000（ocr.input_max_edge）")
        fixture = load(fixture_path)
        check(fixture["schema_version"] == 1, "OCR fixture schema_version 应为 1")
        check(len(fixture["key"]) == 64 and all(ch in "0123456789abcdef" for ch in fixture["key"]), "OCR fixture key 必须 64 位小写十六进制")
        check(fixture["key"] not in keys, "OCR fixture key 重复")
        keys.add(fixture["key"])
        check(fixture["model_id"] == backend["model_id"], "OCR model_id 不一致")
        check(fixture["model_version"] == backend["model_version"], "OCR model_version 不一致")
        check(fixture["prompt_schema_version"] == backend["prompt_schema_version"], "OCR prompt_schema_version 不一致")
        check(sha256_bytes_local := hashlib.sha256(fixture["output_raw"].encode()).hexdigest() == record["output_sha256"], "OCR output_sha256 不一致")
        del sha256_bytes_local
        output = json.loads(fixture["output_raw"])
        check(output["schema_version"] == 1, "OCR output schema_version 应为 1")
        check(output["page"]["width"] == width and output["page"]["height"] == height, "OCR output page 尺寸不一致")
        blocks = output["blocks"]
        check(1 <= len(blocks) <= 200, "OCR blocks 数量必须在 1–200")
        check(len(blocks) == record["blocks_expected"], "OCR blocks 数与索引不一致")
        allowed = {"text", "formula", "table", "handwriting", "figure", "stamp", "header_footer", "unknown"}
        for block in blocks:
            check(block["kind"] in allowed, f"OCR 块类型非法：{block['kind']}")
            bbox = block["bbox"]
            check(len(bbox) == 4, "OCR bbox 必须 4 个数")
            x, y, w, h = bbox
            check(0 <= x < 1 and 0 <= y < 1 and 0 < w <= 1 and 0 < h <= 1, "OCR bbox 越界")
            check(x + w <= 1.0001 and y + h <= 1.0001, "OCR bbox 超页")
            check(isinstance(block.get("text"), str), "OCR 块 text 必须为字符串")
            if block["kind"] == "formula":
                check(bool(block.get("latex")), "公式块必须带 latex")
            check(0 <= block.get("confidence", 1) <= 1, "OCR 块 confidence 越界")
        formula_blocks += sum(1 for block in blocks if block["kind"] == "formula")
    low = [
        block["confidence"]
        for record in records
        for block in json.loads(load(FIX / record["fixture"])["output_raw"])["blocks"]
        if block.get("confidence") is not None
    ]
    check(min(low) < 0.75, "必须存在低置信 OCR 块（供低置信复核路径）")
    check(formula_blocks >= 1, "OCR 夹具必须包含 LaTeX 公式块样例")
    negative = ocr_index["negative"]
    check(len(negative) == 2, "OCR 负向样例应为 2 个")
    for entry in negative:
        path = FIX / entry["file"]
        check(path.is_file(), f"OCR 负向样例缺失：{entry['file']}")
        if path.is_file():
            fixture = load(path)
            check(fixture["schema_version"] == 1 and len(fixture["key"]) == 64, "负向样例仍是合法 FixtureEntry")

    # ---------------------------------------------------------------- 7. Judge 夹具
    judge_index = load(FIX / "judge/index.json")
    check(judge_index["kind"] == "judge", "judge/index.json kind 应为 judge")
    check((FIX / judge_index["prompt_file"]).is_file(), "judge/prompt.txt 缺失")
    check(sha256(FIX / judge_index["prompt_file"]) == judge_index["prompt_sha256"], "judge prompt_sha256 不一致")
    judge_records = judge_index["records"]
    check(len(judge_records) == 8, f"judge 记录应为 8 条，实际 {len(judge_records)}")
    judge_keys = set()
    cases = set()
    for record in judge_records:
        fixture_path = FIX / record["fixture"]
        input_path = FIX / record["input_file"]
        check(fixture_path.is_file(), f"judge fixture 缺失：{record['fixture']}")
        check(input_path.is_file(), f"judge input 缺失：{record['input_file']}")
        if not (fixture_path.is_file() and input_path.is_file()):
            continue
        check(sha256(input_path) == record["input_sha256"], "judge input_sha256 不一致")
        fixture = load(fixture_path)
        check(len(fixture["key"]) == 64, "judge fixture key 必须 64 位小写十六进制")
        check(fixture["key"] not in judge_keys, "judge fixture key 重复")
        judge_keys.add(fixture["key"])
        check(
            hashlib.sha256(fixture["output_raw"].encode()).hexdigest() == record["output_sha256"],
            "judge output_sha256 不一致",
        )
        output = json.loads(fixture["output_raw"])
        allowed_fields = {
            "judgement",
            "score_milli",
            "reason",
            "confidence",
            "matched_alternative",
            "calibration_id",
            "satisfied",
            "missing",
        }
        check(set(output) <= allowed_fields, f"judge 输出含未知字段：{set(output) - allowed_fields}")
        check(output["judgement"] in {"match", "partial", "mismatch"}, "judge judgement 非法")
        check(output["reason"].strip() != "", "judge reason 不得为空")
        check(0 <= output["confidence"] <= 1, "judge confidence 越界")
        question = question_by_id[record["question_id"]] if "question_id" in record else question_by_id[
            next(q["id"] for q in questions if q["ordinal"] == record["question_ordinal"])
        ]
        max_score = question["max_score"]
        if output["judgement"] == "match":
            check(output["score_milli"] == max_score, "judge match 必须满分")
        elif output["judgement"] == "mismatch":
            check(output["score_milli"] == 0, "judge mismatch 必须 0 分")
        else:
            check(0 < output["score_milli"] < max_score, "judge partial 必须在 (0, 满分)")
            check(bool(output["satisfied"]) or bool(output["missing"]), "judge partial 必须给出 satisfied/missing")
        cases.add(record["case"])
        input_doc = load(input_path)
        check(input_doc["ordinal"] == record["question_ordinal"], "judge input ordinal 与索引不一致")
        check(input_doc["max_score_milli"] == max_score, "judge input 满分与试卷不一致")
        check(input_doc["question_id"] == question["id"], "judge input question_id 不一致")
    check({"normal", "low_confidence", "conflict"} <= cases, f"judge 用例必须覆盖 normal/low_confidence/conflict：{cases}")
    judge_negative = judge_index["negative"]
    check(len(judge_negative) == 1, "judge 负向样例应为 1 个")
    for entry in judge_negative:
        path = FIX / entry["file"]
        check(path.is_file(), f"judge 负向样例缺失：{entry['file']}")
        if path.is_file():
            fixture = load(path)
            output = json.loads(fixture["output_raw"])
            check(output["judgement"] == "match" and output["score_milli"] == 0, "judge 负向样例应为 match+0 分")

    # ---------------------------------------------------------------- 8. MANIFEST 与体积
    manifest_path = FIX / "MANIFEST.sha256"
    manifest_lines = [line for line in manifest_path.read_text(encoding="utf-8").splitlines() if line]
    manifest_entries = {}
    for line in manifest_lines:
        digest, relative = line.split("  ", 1)
        manifest_entries[relative] = digest
    actual_files = {
        str(path.relative_to(FIX))
        for path in FIX.rglob("*")
        if path.is_file() and path.name != "MANIFEST.sha256"
    }
    check(set(manifest_entries) == actual_files, f"MANIFEST 文件集合不一致：多 {sorted(set(manifest_entries) - actual_files)} 少 {sorted(actual_files - set(manifest_entries))}")
    for relative, digest in manifest_entries.items():
        path = FIX / relative
        if path.is_file():
            check(sha256(path) == digest, f"MANIFEST 校验失败：{relative}")
    total_bytes = sum(path.stat().st_size for path in FIX.rglob("*") if path.is_file())
    check(total_bytes < 5 * 1024 * 1024, f"fixtures 体积超预算：{total_bytes} bytes")
    check(len(manifest_entries) <= 80, f"文件数过多：{len(manifest_entries)}")

    # ---------------------------------------------------------------- 9. 生成器脚本
    for script in ("regenerate.sh", "smoke.sh"):
        check(
            (REPO / "fixtures/generator" / script).stat().st_mode & 0o111 != 0,
            f"{script} 必须可执行",
        )
    generate_source = (FIX / "generator/generate_fixtures.py").read_text(encoding="utf-8")
    check("FIXTURES_FONT" in generate_source, "生成器必须支持 FIXTURES_FONT 字体覆盖")
    check("resolve_font" in generate_source, "生成器必须包含字体解析")
    keygen_source = (FIX / "generator/keygen/src/main.rs").read_text(encoding="utf-8")
    check("render_page_prompt" in keygen_source, "keygen 必须调用 eg-recognize 渲染提示词")
    check("fixture_key" in keygen_source, "keygen 必须调用 eg-inference 计算夹具键")
    check("MANIFEST.sha256" in keygen_source, "keygen 必须写 MANIFEST")

    # ---------------------------------------------------------------- 汇总
    print(f"检查项：{CHECKS}，失败：{len(FAILS)}")
    print(f"体积：{total_bytes / 1024:.1f} KiB（{total_bytes} bytes），文件：{len(actual_files)}")
    if FAILS:
        for failure in FAILS:
            print(f"[FAIL] {failure}")
        return 1
    print("[ok] fixtures 自检全部通过")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
