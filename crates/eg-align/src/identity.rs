//! 身份匹配（`SPEC-align.md` §3.2：严格顺序，前级确定即停）。
//!
//! 顺序：文件名（v1 唯一启用）→ 条码（默认关，未实现）→ 学号 OCR（默认关）→
//! 姓名模糊（默认关）→ 人工（[`crate::manual`]）。
//!
//! 硬约束：
//! - 文件名精确命中恒为 `1.000`（不变量、不可配置）；低一级不得覆盖高一级；
//! - `identity.ocr_fallback=关` 时**不调用** [`IdentityHintSource`]（默认 0 次 OCR 调用），
//!   未命中直接按 `identity.unmatched_behavior` 进未决/名单外；
//! - 兜底开启后，文件名与 OCR 指向不同学生一律 [`crate::report::UnresolvedKind::IdentityConflict`]。

use eg_domain::{Confidence, Sha256Hex, StudentId, Submission};

use crate::confidence_milli;
use crate::error::AlignError;
use crate::normalize::{normalize_key, page_suffix, tokenize};
use crate::report::UnresolvedKind;
use crate::roster::Roster;
use crate::settings::{AlignSettings, IdentitySourceKind, UnmatchedBehavior};

/// 匹配依据（决定未决展示与审计口径；模型置信度与人工值必须可区分）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchBasis {
    /// 文件名归一后与学号精确相等。
    FileNameExactNo,
    /// 文件名归一后与姓名精确相等。
    FileNameExactName,
    /// 文件名分词后包含学号 token。
    FileNameTokenNo,
    /// 文件名分词后包含姓名 token。
    FileNameTokenName,
    /// 学号 OCR 兜底命中。
    OcrStudentNo,
    /// 姓名 OCR 兜底命中。
    OcrName,
    /// 文件名未命中（名单外路径使用）。
    FileNameUnmatched,
    /// 条码（v1 未实现）。
    Barcode,
    /// 教师人工确认。
    Manual,
}

/// 身份候选（脱敏展示：学号后 4 位 + 姓名打码；不含原始姓名/学号）。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct IdentityCandidate {
    /// 学生 id。
    pub student_id: StudentId,
    /// 学号后 4 位（脱敏）。
    pub student_no_tail: String,
    /// 姓名（打码）。
    pub name_masked: String,
    /// 该候选的置信度（文件名未命中的候选没有分数，为 `None`）。
    pub confidence: Option<Confidence>,
}

/// 身份结论。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum IdentityStatus {
    /// 已唯一确认。
    Confirmed {
        /// 学生 id。
        student_id: StudentId,
        /// 匹配依据。
        basis: MatchBasis,
        /// 置信度（文件名/人工恒 1.000）。
        confidence: Confidence,
    },
    /// 未决（必须人工处理）。
    Unresolved {
        /// 未决种类。
        kind: UnresolvedKind,
        /// 候选（可能为空 = 名单内没有相似项，由教师在名单中检索）。
        candidates: Vec<IdentityCandidate>,
    },
    /// 名单外（`identity.unmatched_behavior=自动归入名单外`；不参与统计、不自动判分）。
    OutsideRoster {
        /// 匹配依据（文件名未命中）。
        basis: MatchBasis,
    },
}

impl IdentityStatus {
    /// 已确认学生 id。
    #[must_use]
    pub fn student_id(&self) -> Option<StudentId> {
        match self {
            Self::Confirmed { student_id, .. } => Some(*student_id),
            Self::Unresolved { .. } | Self::OutsideRoster { .. } => None,
        }
    }

    /// 是否已确认（自动判分的身份前置）。
    #[must_use]
    pub fn is_confirmed(&self) -> bool {
        matches!(self, Self::Confirmed { .. })
    }
}

/// 身份证据（脱敏：只保留哈希与计数，不落原始文件名/姓名/学号）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct IdentityEvidence {
    /// 归一后基名（去页码后缀）的哈希。
    pub stem_hash: Sha256Hex,
    /// 解析出的页码后缀（有则记录；供分组/缺页诊断）。
    pub page_suffix: Option<u32>,
    /// 归一后 token 数。
    pub token_count: u32,
    /// OCR 兜底端口返回的候选条数（默认配置恒为 0）。
    pub hint_count: u32,
}

/// 身份结论 + 证据。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct IdentityConclusion {
    /// 提交 id。
    pub submission_id: eg_domain::SubmissionId,
    /// 结论。
    pub status: IdentityStatus,
    /// 启用中的身份来源（v1 恒为文件名）。
    pub source: IdentitySourceKind,
    /// 证据（脱敏）。
    pub evidence: IdentityEvidence,
}

/// OCR 兜底提示的种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityHintKind {
    /// 学号文本。
    StudentNo,
    /// 姓名文本。
    Name,
}

/// 一条身份兜底提示（来自 `recognize`；本 crate 不产生识别调用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityHint {
    /// 提示种类。
    pub kind: IdentityHintKind,
    /// 识别文本（不可信输入，使用前统一归一）。
    pub text: String,
}

/// OCR 兜底提示来源。
///
/// `identity.ocr_fallback=关` 时**不得调用**（默认路径 0 次调用由 `pipeline::align` 保证）。
pub trait IdentityHintSource {
    /// 该提交的兜底提示。
    fn hints(&self, submission: &Submission) -> Vec<IdentityHint>;
}

/// 无兜底来源（默认配置）。
#[derive(Debug, Clone, Copy, Default)]
pub struct NoIdentityHints;

impl IdentityHintSource for NoIdentityHints {
    fn hints(&self, _submission: &Submission) -> Vec<IdentityHint> {
        Vec::new()
    }
}

/// 执行身份匹配。
///
/// # Errors
///
/// 仅在内部不变量被破坏时返回 [`AlignError::Internal`]（本函数不产生 IO）。
pub fn resolve_identity(
    submission: &Submission,
    roster: &Roster,
    settings: &AlignSettings,
    hints: &dyn IdentityHintSource,
) -> Result<IdentityConclusion, AlignError> {
    let (base, suffix) = page_suffix(&submission.source_stem);
    let normalized_key = normalize_key(&base);
    let tokens = tokenize(&base);
    let evidence = IdentityEvidence {
        stem_hash: Sha256Hex::of(normalized_key.as_bytes()),
        page_suffix: suffix,
        token_count: u32::try_from(tokens.len()).unwrap_or(u32::MAX),
        hint_count: 0,
    };

    let filename = match_filename(roster, &normalized_key, &tokens);
    // 兜底关闭时绝不触碰识别文本（默认 0 次调用）。
    let ocr = if settings.ocr_fallback {
        let hints = hints.hints(submission);
        let mut evidence = evidence.clone();
        evidence.hint_count = u32::try_from(hints.len()).unwrap_or(u32::MAX);
        (Some(ocr_candidates(roster, &hints)), evidence)
    } else {
        (None, evidence)
    };
    let (ocr_candidates, evidence) = ocr;

    let status = match (filename, ocr_candidates) {
        (FilenameMatch::Unique { student_id, basis }, Some(candidates)) => {
            // 冲突判定：兜底开启且有可信 OCR 候选指向另一个学生 → 显式未决。
            let conflict = candidates
                .first()
                .filter(|(other, confidence, _)| {
                    *other != student_id && *confidence >= confidence_milli(settings.ocr_threshold)
                })
                .map(|(other, confidence, _)| (*other, *confidence));
            if let Some((other, confidence)) = conflict {
                let mut list = vec![
                    candidate(roster, student_id, Some(Confidence::ONE)),
                    candidate(roster, other, Some(confidence)),
                ];
                sort_candidates(&mut list);
                IdentityStatus::Unresolved {
                    kind: UnresolvedKind::IdentityConflict,
                    candidates: list,
                }
            } else {
                IdentityStatus::Confirmed {
                    student_id,
                    basis,
                    confidence: Confidence::ONE,
                }
            }
        }
        (FilenameMatch::Unique { student_id, basis }, None) => IdentityStatus::Confirmed {
            student_id,
            basis,
            confidence: Confidence::ONE,
        },
        (FilenameMatch::Ambiguous(ids), _) => IdentityStatus::Unresolved {
            kind: UnresolvedKind::IdentityAmbiguous,
            candidates: roster_candidates(roster, &ids),
        },
        (FilenameMatch::None, Some(candidates)) => {
            if let Some((student_id, confidence, basis)) = confident_ocr(&candidates, settings) {
                IdentityStatus::Confirmed {
                    student_id,
                    basis,
                    confidence,
                }
            } else if candidates.is_empty() {
                unmatched_status(settings)
            } else {
                let mut list: Vec<IdentityCandidate> = candidates
                    .iter()
                    .map(|(id, confidence, _)| candidate(roster, *id, Some(*confidence)))
                    .collect();
                sort_candidates(&mut list);
                IdentityStatus::Unresolved {
                    kind: UnresolvedKind::IdentityAmbiguous,
                    candidates: list,
                }
            }
        }
        (FilenameMatch::None, None) => unmatched_status(settings),
    };

    Ok(IdentityConclusion {
        submission_id: submission.id,
        status,
        source: settings.identity_source,
        evidence,
    })
}

fn unmatched_status(settings: &AlignSettings) -> IdentityStatus {
    match settings.unmatched_behavior {
        UnmatchedBehavior::Undecided => IdentityStatus::Unresolved {
            kind: UnresolvedKind::FileNameNotFound,
            candidates: Vec::new(),
        },
        UnmatchedBehavior::OutsideRoster => IdentityStatus::OutsideRoster {
            basis: MatchBasis::FileNameUnmatched,
        },
    }
}

enum FilenameMatch {
    Unique {
        student_id: StudentId,
        basis: MatchBasis,
    },
    Ambiguous(Vec<StudentId>),
    None,
}

fn match_filename(roster: &Roster, normalized_key: &str, tokens: &[String]) -> FilenameMatch {
    // 1) 归一后与学号精确相等。
    if let Some(student) = roster.by_student_no(normalized_key) {
        return FilenameMatch::Unique {
            student_id: student.id,
            basis: MatchBasis::FileNameExactNo,
        };
    }
    // 2) 归一后与姓名精确相等（同名多人 → 歧义）。
    let by_name = roster.by_name(normalized_key);
    match by_name.len() {
        0 => {}
        1 => {
            return FilenameMatch::Unique {
                student_id: by_name[0].id,
                basis: MatchBasis::FileNameExactName,
            };
        }
        _ => {
            return FilenameMatch::Ambiguous(by_name.iter().map(|s| s.id).collect());
        }
    }
    // 3) 分词后包含学号 / 姓名 token（如「张三_期中」/「20260001_张三」）。
    let mut matches: Vec<(StudentId, MatchBasis)> = Vec::new();
    for token in tokens {
        let key = normalize_key(token);
        if key.is_empty() {
            continue;
        }
        if let Some(student) = roster.by_student_no(&key) {
            push_unique(&mut matches, (student.id, MatchBasis::FileNameTokenNo));
        }
        for student in roster.by_name(&key) {
            push_unique(&mut matches, (student.id, MatchBasis::FileNameTokenName));
        }
    }
    match matches.len() {
        0 => FilenameMatch::None,
        1 => FilenameMatch::Unique {
            student_id: matches[0].0,
            basis: matches[0].1,
        },
        _ => FilenameMatch::Ambiguous(matches.into_iter().map(|(id, _)| id).collect()),
    }
}

fn push_unique(list: &mut Vec<(StudentId, MatchBasis)>, item: (StudentId, MatchBasis)) {
    if !list.iter().any(|(id, _)| *id == item.0) {
        list.push(item);
    }
}

/// OCR 候选（不自动确认，只给分数；排序后供阈值/分差判定与未决候选展示）。
fn ocr_candidates(
    roster: &Roster,
    hints: &[IdentityHint],
) -> Vec<(StudentId, Confidence, MatchBasis)> {
    let mut best: Vec<(StudentId, Confidence, MatchBasis)> = Vec::new();
    for hint in hints {
        let key = normalize_key(&hint.text);
        if key.is_empty() {
            continue;
        }
        for student in roster.students() {
            let no_key = normalize_key(&student.student_no);
            let name_key = normalize_key(&student.name);
            let scored = match hint.kind {
                IdentityHintKind::StudentNo => {
                    score_student_no(&key, &no_key).map(|score| (score, MatchBasis::OcrStudentNo))
                }
                IdentityHintKind::Name => {
                    score_name(&key, &name_key).map(|score| (score, MatchBasis::OcrName))
                }
            };
            if let Some((score, basis)) = scored {
                let rounded = confidence_milli(score);
                match best.iter_mut().find(|(id, _, _)| *id == student.id) {
                    Some(entry) => {
                        if rounded > entry.1 {
                            entry.1 = rounded;
                            entry.2 = basis;
                        }
                    }
                    None => best.push((student.id, rounded, basis)),
                }
            }
        }
    }
    best.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.0.to_string().cmp(&right.0.to_string()))
    });
    best
}

fn confident_ocr(
    candidates: &[(StudentId, Confidence, MatchBasis)],
    settings: &AlignSettings,
) -> Option<(StudentId, Confidence, MatchBasis)> {
    let first = candidates.first()?;
    let threshold = confidence_milli(settings.ocr_threshold);
    if first.1 < threshold {
        return None;
    }
    if let Some(second) = candidates.get(1) {
        let margin = confidence_milli(settings.ocr_margin);
        // 置信度是千分位舍入后的浮点：0.9 - 0.8 = 0.0999…，必须带容差比较。
        if (first.1.get() - second.1.get()) + 1e-9 < margin.get() {
            return None;
        }
    }
    Some(*first)
}

fn score_student_no(hint: &str, student_no: &str) -> Option<f64> {
    if hint == student_no {
        return Some(0.900);
    }
    if within_one_edit(hint, student_no) {
        return Some(0.800);
    }
    None
}

fn score_name(hint: &str, name: &str) -> Option<f64> {
    if hint == name {
        return Some(0.900);
    }
    // 同姓 + 编辑距离 1（如「张衫」对「张三」）。
    let hint_chars: Vec<char> = hint.chars().collect();
    let name_chars: Vec<char> = name.chars().collect();
    if hint_chars.first() == name_chars.first() && within_one_edit(hint, name) {
        return Some(0.700);
    }
    None
}

/// 编辑距离 ≤1 判定（插入/删除/替换其一）；长度差 >1 直接 false。
fn within_one_edit(left: &str, right: &str) -> bool {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    let (longer, shorter) = if left.len() >= right.len() {
        (&left, &right)
    } else {
        (&right, &left)
    };
    if longer.len() - shorter.len() > 1 {
        return false;
    }
    if longer.len() == shorter.len() {
        return longer
            .iter()
            .zip(shorter.iter())
            .filter(|(a, b)| a != b)
            .count()
            <= 1;
    }
    let mut index = 0;
    let mut skipped = false;
    for ch in longer {
        if index < shorter.len() && *ch == shorter[index] {
            index += 1;
        } else if skipped {
            return false;
        } else {
            skipped = true;
        }
    }
    index == shorter.len()
}

fn candidate(
    roster: &Roster,
    student_id: StudentId,
    confidence: Option<Confidence>,
) -> IdentityCandidate {
    IdentityCandidate {
        student_id,
        student_no_tail: roster.student_no_tail(student_id),
        name_masked: roster.masked_name(student_id),
        confidence,
    }
}

fn roster_candidates(roster: &Roster, ids: &[StudentId]) -> Vec<IdentityCandidate> {
    let mut list: Vec<IdentityCandidate> =
        ids.iter().map(|id| candidate(roster, *id, None)).collect();
    sort_candidates(&mut list);
    list
}

fn sort_candidates(list: &mut [IdentityCandidate]) {
    list.sort_by(|left, right| {
        right
            .confidence
            .map(Confidence::get)
            .unwrap_or(0.0)
            .partial_cmp(&left.confidence.map(Confidence::get).unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.student_no_tail.cmp(&right.student_no_tail))
    });
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::cell::RefCell;

    use eg_domain::{ClassroomId, Page, RosterSource, Student, Ulid};

    use super::*;

    fn student(id: u128, no: &str, name: &str) -> Student {
        Student {
            id: StudentId::from_ulid(Ulid::from_u128(id)),
            classroom_id: ClassroomId::from_ulid(Ulid::from_u128(100)),
            student_no: no.to_owned(),
            name: name.to_owned(),
            source: RosterSource::RosterImport,
        }
    }

    fn roster() -> Roster {
        Roster::new(
            ClassroomId::from_ulid(Ulid::from_u128(100)),
            vec![
                student(1, "20260001", "张三"),
                student(2, "20260002", "李四"),
                student(3, "20260003", "王五"),
            ],
        )
        .unwrap()
    }

    fn submission(stem: &str) -> Submission {
        Submission {
            id: eg_domain::SubmissionId::from_ulid(Ulid::from_u128(9)),
            paper_id: eg_domain::PaperId::from_ulid(Ulid::from_u128(10)),
            classroom_id: ClassroomId::from_ulid(Ulid::from_u128(100)),
            student_id: None,
            source_file_name: format!("{stem}.jpg"),
            source_stem: stem.to_owned(),
            pages: Vec::<Page>::new(),
            status: eg_domain::SubmissionStatus::Imported,
            created_at: eg_domain::time::parse_rfc3339("2026-11-02T00:00:00Z").unwrap(),
        }
    }

    #[derive(Default)]
    struct CountingHints {
        calls: RefCell<u32>,
        items: Vec<IdentityHint>,
    }

    impl IdentityHintSource for CountingHints {
        fn hints(&self, _submission: &Submission) -> Vec<IdentityHint> {
            *self.calls.borrow_mut() += 1;
            self.items.clone()
        }
    }

    fn hint(kind: IdentityHintKind, text: &str) -> IdentityHint {
        IdentityHint {
            kind,
            text: text.to_owned(),
        }
    }

    #[test]
    fn exact_name_and_student_no_confirm_at_one() {
        let roster = roster();
        let settings = AlignSettings::default();
        let no_hints = CountingHints::default();
        for (stem, expected) in [("张三", 1u128), ("20260001", 1), ("李四", 2)] {
            let outcome =
                resolve_identity(&submission(stem), &roster, &settings, &no_hints).unwrap();
            let IdentityStatus::Confirmed {
                student_id,
                basis,
                confidence,
            } = outcome.status
            else {
                panic!("{stem} 应确认，实际 {:?}", outcome.status)
            };
            assert_eq!(student_id, StudentId::from_ulid(Ulid::from_u128(expected)));
            assert_eq!(confidence, Confidence::ONE);
            assert!(matches!(
                basis,
                MatchBasis::FileNameExactName | MatchBasis::FileNameExactNo
            ));
        }
        assert_eq!(*no_hints.calls.borrow(), 0, "默认配置不得调用 OCR 兜底");
    }

    #[test]
    fn token_names_and_student_numbers_confirm() {
        let roster = roster();
        let settings = AlignSettings::default();
        let no_hints = NoIdentityHints;
        for stem in ["张三_期中", "20260002_李四_期中", "期中_王五"] {
            let outcome =
                resolve_identity(&submission(stem), &roster, &settings, &no_hints).unwrap();
            assert!(
                outcome.status.is_confirmed(),
                "{stem}: {:?}",
                outcome.status
            );
        }
        assert_eq!(
            resolve_identity(&submission("王五_期中"), &roster, &settings, &no_hints)
                .unwrap()
                .status
                .student_id(),
            Some(StudentId::from_ulid(Ulid::from_u128(3)))
        );
    }

    #[test]
    fn ambiguous_names_are_unresolved_with_candidates() {
        let roster = Roster::new(
            ClassroomId::from_ulid(Ulid::from_u128(100)),
            vec![
                student(1, "20260001", "张三"),
                student(2, "20260002", "张三"),
            ],
        )
        .unwrap();
        let outcome = resolve_identity(
            &submission("张三_期中"),
            &roster,
            &AlignSettings::default(),
            &NoIdentityHints,
        )
        .unwrap();
        let IdentityStatus::Unresolved { kind, candidates } = outcome.status else {
            panic!("同名必须未决");
        };
        assert_eq!(kind, UnresolvedKind::IdentityAmbiguous);
        assert_eq!(candidates.len(), 2);
        assert!(candidates.iter().all(|c| c.name_masked == "张*"));
    }

    #[test]
    fn unmatched_filenames_are_unresolved_by_default() {
        let outcome = resolve_identity(
            &submission("IMG_0032"),
            &roster(),
            &AlignSettings::default(),
            &NoIdentityHints,
        )
        .unwrap();
        assert_eq!(
            outcome.status,
            IdentityStatus::Unresolved {
                kind: UnresolvedKind::FileNameNotFound,
                candidates: Vec::new()
            }
        );
        assert_eq!(outcome.evidence.hint_count, 0);
    }

    #[test]
    fn outside_roster_behavior_is_explicit() {
        let settings = AlignSettings {
            unmatched_behavior: UnmatchedBehavior::OutsideRoster,
            ..AlignSettings::default()
        };
        let outcome =
            resolve_identity(&submission("李雷"), &roster(), &settings, &NoIdentityHints).unwrap();
        assert!(matches!(
            outcome.status,
            IdentityStatus::OutsideRoster {
                basis: MatchBasis::FileNameUnmatched
            }
        ));
    }

    #[test]
    fn ocr_fallback_disabled_never_calls_hints() {
        let hints = CountingHints {
            calls: RefCell::new(0),
            items: vec![hint(IdentityHintKind::StudentNo, "20260001")],
        };
        let outcome = resolve_identity(
            &submission("IMG_0032"),
            &roster(),
            &AlignSettings::default(),
            &hints,
        )
        .unwrap();
        assert_eq!(*hints.calls.borrow(), 0);
        assert!(matches!(
            outcome.status,
            IdentityStatus::Unresolved {
                kind: UnresolvedKind::FileNameNotFound,
                ..
            }
        ));
    }

    #[test]
    fn ocr_fallback_enabled_confirms_unique_student_no() {
        let settings = AlignSettings {
            ocr_fallback: true,
            ..AlignSettings::default()
        };
        let hints = CountingHints {
            calls: RefCell::new(0),
            items: vec![hint(IdentityHintKind::StudentNo, "2026-0002")],
        };
        let outcome =
            resolve_identity(&submission("IMG_0032"), &roster(), &settings, &hints).unwrap();
        assert_eq!(*hints.calls.borrow(), 1);
        let IdentityStatus::Confirmed {
            student_id,
            basis,
            confidence,
        } = outcome.status
        else {
            panic!("兜底应确认，实际 {:?}", outcome.status);
        };
        assert_eq!(student_id, StudentId::from_ulid(Ulid::from_u128(2)));
        assert_eq!(basis, MatchBasis::OcrStudentNo);
        assert_eq!(confidence, crate::confidence_milli(0.9));
        assert_eq!(outcome.evidence.hint_count, 1);
    }

    #[test]
    fn ocr_fallback_below_threshold_stays_unresolved() {
        let settings = AlignSettings {
            ocr_fallback: true,
            ..AlignSettings::default()
        };
        let hints = CountingHints {
            calls: RefCell::new(0),
            items: vec![hint(IdentityHintKind::Name, "张衫")],
        };
        let outcome =
            resolve_identity(&submission("IMG_0032"), &roster(), &settings, &hints).unwrap();
        let IdentityStatus::Unresolved { kind, candidates } = outcome.status else {
            panic!("0.700 < 0.900 必须未决");
        };
        assert_eq!(kind, UnresolvedKind::IdentityAmbiguous);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].confidence, Some(crate::confidence_milli(0.7)));
    }

    #[test]
    fn filename_and_ocr_conflict_is_unresolved() {
        let settings = AlignSettings {
            ocr_fallback: true,
            ..AlignSettings::default()
        };
        let hints = CountingHints {
            calls: RefCell::new(0),
            items: vec![hint(IdentityHintKind::StudentNo, "20260002")],
        };
        let outcome =
            resolve_identity(&submission("张三_期中"), &roster(), &settings, &hints).unwrap();
        let IdentityStatus::Unresolved { kind, candidates } = outcome.status else {
            panic!("文件名与 OCR 冲突必须未决");
        };
        assert_eq!(kind, UnresolvedKind::IdentityConflict);
        assert_eq!(candidates.len(), 2);
        assert_eq!(*hints.calls.borrow(), 1);
    }

    #[test]
    fn edit_distance_helper_is_strict() {
        assert!(within_one_edit("20260001", "20260001"));
        assert!(within_one_edit("20260001", "2026000"));
        assert!(within_one_edit("20260001", "20260002"));
        assert!(within_one_edit("20260001", "2026001"));
        assert!(!within_one_edit("20260001", "2026"));
        assert!(!within_one_edit("张三", "李四"));
    }
}
