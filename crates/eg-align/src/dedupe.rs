//! 重复提交检测（`SPEC-align.md` §3.4「重复提交」、§5「同学生同卷已有提交」）。
//!
//! 判定只看页面内容资产（内容寻址 → 同字节必然同 `AssetId`）：
//! 全页集合相同 = 完全重复（可复用首份，不新建）；部分重叠 = 未决。

use std::collections::BTreeSet;

use eg_domain::{AssetId, Submission, SubmissionId};

/// 已存在的同卷提交（来自平台查询的投影）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingSubmission {
    /// 提交 id。
    pub submission_id: SubmissionId,
    /// 该提交的页面资产（顺序无关）。
    pub page_assets: Vec<AssetId>,
}

/// 重复判定结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DuplicateVerdict {
    /// 无重复。
    New,
    /// 全页内容相同：可复用首份，不新建（不自动判分，等教师确认）。
    Identical {
        /// 首份提交 id。
        first: SubmissionId,
    },
    /// 部分页面重叠：未决，不自行去重。
    PartialOverlap {
        /// 重叠的既有提交 id。
        first: SubmissionId,
    },
}

/// 检测当前提交是否与既有提交重复。
#[must_use]
pub fn check_duplicate(
    submission: &Submission,
    existing: &[ExistingSubmission],
) -> DuplicateVerdict {
    let current: BTreeSet<AssetId> = submission.pages.iter().map(|page| page.asset).collect();
    let mut partial: Option<SubmissionId> = None;
    for candidate in existing {
        let other: BTreeSet<AssetId> = candidate.page_assets.iter().copied().collect();
        if current == other {
            return DuplicateVerdict::Identical {
                first: candidate.submission_id,
            };
        }
        if !current.is_disjoint(&other) && partial.is_none() {
            partial = Some(candidate.submission_id);
        }
    }
    match partial {
        Some(first) => DuplicateVerdict::PartialOverlap { first },
        None => DuplicateVerdict::New,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use eg_domain::{Page, PageId, SubmissionStatus, Ulid};

    use super::*;

    fn submission(id: u128, assets: &[&str]) -> Submission {
        let pages: Vec<Page> = assets
            .iter()
            .enumerate()
            .map(|(index, text)| Page {
                id: PageId::from_ulid(Ulid::from_u128(id * 10 + index as u128)),
                submission_id: SubmissionId::from_ulid(Ulid::from_u128(id)),
                page_no: u32::try_from(index + 1).unwrap(),
                asset: AssetId::of(text.as_bytes()),
                width: 100,
                height: 100,
                rotation_applied: eg_domain::Rotation::new(0).unwrap(),
                exif_orientation: eg_domain::ExifOrientation::new(1).unwrap(),
                quality: eg_domain::PageQuality {
                    blur_var: 100.0,
                    exposure: 0.5,
                    skew_deg: 0.0,
                    resolution_ok: true,
                    verdict: eg_domain::QualityVerdict::Good,
                },
                derived: Vec::new(),
            })
            .collect();
        Submission {
            id: SubmissionId::from_ulid(Ulid::from_u128(id)),
            paper_id: eg_domain::PaperId::from_ulid(Ulid::from_u128(1)),
            classroom_id: eg_domain::ClassroomId::from_ulid(Ulid::from_u128(2)),
            student_id: None,
            source_file_name: "张三.jpg".to_owned(),
            source_stem: "张三".to_owned(),
            pages,
            status: SubmissionStatus::Imported,
            created_at: eg_domain::time::parse_rfc3339("2026-11-02T00:00:00Z").unwrap(),
        }
    }

    #[test]
    fn identical_pages_reuse_first_and_overlap_is_unresolved() {
        let current = submission(3, &["p1", "p2"]);
        let existing = vec![
            ExistingSubmission {
                submission_id: SubmissionId::from_ulid(Ulid::from_u128(1)),
                page_assets: vec![AssetId::of(b"p1"), AssetId::of(b"p2")],
            },
            ExistingSubmission {
                submission_id: SubmissionId::from_ulid(Ulid::from_u128(2)),
                page_assets: vec![AssetId::of(b"p9")],
            },
        ];
        assert_eq!(
            check_duplicate(&current, &existing),
            DuplicateVerdict::Identical {
                first: SubmissionId::from_ulid(Ulid::from_u128(1))
            }
        );
        let partial = vec![ExistingSubmission {
            submission_id: SubmissionId::from_ulid(Ulid::from_u128(2)),
            page_assets: vec![AssetId::of(b"p1"), AssetId::of(b"p9")],
        }];
        assert_eq!(
            check_duplicate(&current, &partial),
            DuplicateVerdict::PartialOverlap {
                first: SubmissionId::from_ulid(Ulid::from_u128(2))
            }
        );
        assert_eq!(check_duplicate(&current, &[]), DuplicateVerdict::New);
    }
}
