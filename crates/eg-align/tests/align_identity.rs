#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    clippy::uninlined_format_args,
    clippy::bool_assert_comparison,
    clippy::useless_vec,
    clippy::manual_let_else,
    clippy::float_cmp,
    dead_code,
    unreachable_pub,
    reason = "夹具与断言：允许 unwrap/索引/数值转换"
)]

//! T28 验收：文件名身份、未决、OCR 兜底默认 0 调用、冲突显式未决。
//!
//! 500 份合成文件名夹具（含 10% 同名同姓、20% 相机文件名）→ 自动确认 0 错配；
//! 未命中 100% 进未决；默认配置 OCR 兜底 0 调用。

use eg_align::testing::{classroom, counting_hints, hint, roster, student, submission};
use eg_align::{
    AlignSettings, IdentityHintKind, IdentityStatus, MatchBasis, NoIdentityHints, UnresolvedKind,
    resolve_identity,
};
use eg_domain::{ClassroomId, StudentId};

fn classroom_id() -> ClassroomId {
    classroom(7)
}

/// 50 名学生（其中 10% 同名同姓：5 对「张伟」不同学号）。
fn fixture_roster() -> eg_align::Roster {
    let mut students = Vec::new();
    let names = ["张三", "李四", "王五", "赵六", "钱七", "孙八"];
    for (index, name) in names.iter().enumerate() {
        let id = 100 + u128::try_from(index).unwrap_or(0);
        students.push(student(
            classroom_id(),
            id,
            &format!("2026{:04}", id - 100 + 1),
            name,
        ));
    }
    // 同名同姓 10%（5 对，共 10 人）。
    for pair in 0..5u128 {
        for slot in 0..2u128 {
            students.push(student(
                classroom_id(),
                200 + pair * 2 + slot,
                &format!("2027{:04}", pair * 2 + slot + 1),
                "张伟",
            ));
        }
    }
    // 其余 34 人。
    for index in 0..34u128 {
        students.push(student(
            classroom_id(),
            300 + index,
            &format!("2028{:04}", index + 1),
            &format!("学生{}", index + 1),
        ));
    }
    roster(classroom_id(), students).expect("名单合法")
}

#[test]
fn five_hundred_synthetic_filenames_zero_misidentification() {
    let roster = fixture_roster();
    let settings = AlignSettings::default();
    let no_hints = NoIdentityHints;
    let mut confirmed: Vec<(StudentId, MatchBasis)> = Vec::new();
    let mut unresolved = 0u32;
    let total = 500u32;

    // 500 份提交：50%「姓名_期中」、20% 相机文件名、10% 同名同姓、10% 学号、
    // 10% 名单外姓名。默认配置下前两类+学号应全部自动确认，其余 100% 未决。
    for index in 0..total {
        let bucket = index % 10;
        let (stem, expected) = match bucket {
            0..=4 => {
                let student = &roster.students()[usize::try_from(index).unwrap() % 6];
                (format!("{}_期中", student.name), Some(student.id))
            }
            5 | 6 => (format!("IMG_{:04}", index + 1), None),
            7 => ("张伟_期中".to_owned(), None),
            8 => {
                let student = &roster.students()[usize::try_from(index).unwrap() % 6];
                (student.student_no.clone(), Some(student.id))
            }
            _ => ("李雷_期中".to_owned(), None),
        };
        let submission = submission(
            1_000 + u128::from(index),
            classroom_id(),
            10,
            &stem,
            &["第 1 页"],
        );
        let conclusion =
            resolve_identity(&submission, &roster, &settings, &no_hints).expect("纯计算不失败");
        match (conclusion.status, expected) {
            (
                IdentityStatus::Confirmed {
                    student_id, basis, ..
                },
                Some(expected),
            ) => {
                assert_eq!(student_id, expected, "文件名 `{stem}` 错配");
                assert!(matches!(
                    basis,
                    MatchBasis::FileNameExactName
                        | MatchBasis::FileNameExactNo
                        | MatchBasis::FileNameTokenName
                        | MatchBasis::FileNameTokenNo
                ));
                confirmed.push((student_id, basis));
            }
            (IdentityStatus::Unresolved { .. }, None) => unresolved += 1,
            (status, expected) => {
                panic!("文件名 `{stem}` 结论异常：{status:?}（期望 {expected:?}）")
            }
        }
    }
    // 300 份规范化/学号文件全部自动确认，且零错配；200 份相机/同名全部未决。
    assert_eq!(confirmed.len(), 300, "规范化命名命中率应 100%");
    assert_eq!(unresolved, 200, "相机文件名 + 同名同姓必须 100% 未决");
}

#[test]
fn ocr_fallback_disabled_makes_zero_ocr_calls() {
    let roster = fixture_roster();
    let settings = AlignSettings::default();
    assert!(!settings.ocr_fallback);
    let hints = counting_hints();
    let submission = submission(1, classroom_id(), 10, "IMG_9999", &["第 1 页"]);
    let conclusion = resolve_identity(&submission, &roster, &settings, &hints).expect("身份计算");
    assert_eq!(hints.calls(), 0, "默认配置不得调用 OCR 兜底");
    assert_eq!(conclusion.evidence.hint_count, 0);
    assert!(matches!(
        conclusion.status,
        IdentityStatus::Unresolved {
            kind: UnresolvedKind::FileNameNotFound,
            ..
        }
    ));
}

#[test]
fn ocr_fallback_enabled_confirms_but_conflicts_stay_unresolved() {
    let roster = fixture_roster();
    let settings = AlignSettings {
        ocr_fallback: true,
        ..AlignSettings::default()
    };

    // 相机文件名 + 唯一学号 OCR → 自动确认（证明开关生效）。
    let target = &roster.students()[1];
    let mut hints = counting_hints();
    hints.push(hint(IdentityHintKind::StudentNo, &target.student_no));
    let camera = submission(2, classroom_id(), 10, "IMG_0001", &["第 1 页"]);
    let conclusion = resolve_identity(&camera, &roster, &settings, &hints).expect("身份计算");
    assert_eq!(hints.calls(), 1);
    let IdentityStatus::Confirmed {
        student_id, basis, ..
    } = conclusion.status
    else {
        panic!("兜底应确认，实际 {:?}", conclusion.status);
    };
    assert_eq!(student_id, target.id);
    assert!(matches!(
        basis,
        MatchBasis::OcrStudentNo | MatchBasis::OcrName
    ));

    // 文件名说 A、OCR 说 B → 显式未决（不静默择优）。
    let other = &roster.students()[2];
    let mut hints = counting_hints();
    hints.push(hint(IdentityHintKind::StudentNo, &other.student_no));
    let named = submission(3, classroom_id(), 10, "张三_期中", &["第 1 页"]);
    let conclusion = resolve_identity(&named, &roster, &settings, &hints).expect("身份计算");
    let IdentityStatus::Unresolved { kind, candidates } = conclusion.status else {
        panic!("冲突必须未决，实际 {:?}", conclusion.status);
    };
    assert_eq!(kind, UnresolvedKind::IdentityConflict);
    assert_eq!(candidates.len(), 2, "双方证据必须并列");
}

#[test]
fn same_stem_page_suffix_is_groupable_and_reported_in_evidence() {
    let roster = fixture_roster();
    let student = &roster.students()[0];
    let settings = AlignSettings::default();
    let submission = submission(
        4,
        classroom_id(),
        10,
        &format!("{}_p2", student.name),
        &["第 2 页"],
    );
    let conclusion =
        resolve_identity(&submission, &roster, &settings, &NoIdentityHints).expect("身份计算");
    assert!(conclusion.status.is_confirmed(), "带页码后缀也要命中");
    assert_eq!(conclusion.evidence.page_suffix, Some(2));
}
