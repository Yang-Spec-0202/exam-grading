//! 去重与幂等（`SPEC-capture.md` §3.4）：
//! 资产级去重（内容寻址）永远开启；批内重复只处理一次；提交级重合 ≥80% 视为疑似重复。

use std::collections::HashMap;

use eg_domain::{AssetId, ContentHash, SubmissionId};

/// 批内去重表（按内容哈希，保留首次出现的来源指纹）。
#[derive(Debug, Default)]
pub struct BatchDedupe {
    seen: HashMap<ContentHash, crate::event::SourceFingerprint>,
}

impl BatchDedupe {
    /// 新建空表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 命中则返回首次出现的来源指纹；未命中返回 `None`。
    pub fn hit(&self, hash: ContentHash) -> Option<&crate::event::SourceFingerprint> {
        self.seen.get(&hash)
    }

    /// 记录内容哈希（已存在则保留首次）。
    pub fn record(&mut self, hash: ContentHash, fingerprint: crate::event::SourceFingerprint) {
        self.seen.entry(hash).or_insert(fingerprint);
    }

    /// 已记录的不同内容数。
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

/// 既有提交的页面指纹（提交级判重输入；由 `platform` 查询后注入）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmissionFingerprint {
    /// 提交 id。
    pub submission_id: SubmissionId,
    /// 该提交的页面原图资产集合。
    pub pages: Vec<AssetId>,
}

impl SubmissionFingerprint {
    /// 与给定页面资产集合的重合比例（交集 / 新集合大小，0.0–1.0）。
    pub fn overlap(&self, page_assets: &[AssetId]) -> f64 {
        if page_assets.is_empty() {
            return 0.0;
        }
        let mut hits = 0_u32;
        for asset in page_assets {
            if self.pages.contains(asset) {
                hits += 1;
            }
        }
        f64::from(hits) / page_assets.len() as f64
    }
}

/// 既有提交查询端口（`platform` 仓储适配点；测试用空实现）。
pub trait KnownSubmissions: Send + Sync {
    /// 当前试卷 + 班级下既有提交的页面指纹。
    fn page_sets(&self) -> Vec<SubmissionFingerprint>;
}

/// 无既有提交（首次导入、测试）。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoKnownSubmissions;

impl KnownSubmissions for NoKnownSubmissions {
    fn page_sets(&self) -> Vec<SubmissionFingerprint> {
        Vec::new()
    }
}

/// 提交级疑似重复阈值（`SPEC-capture.md` §3.4：重合不少于 80%）。
pub const SUSPECTED_OVERLAP: f64 = 0.8;

/// 找出重合比例 ≥ [`SUSPECTED_OVERLAP`] 的既有提交（取重合最高者）。
pub fn suspicious_submission(
    page_assets: &[AssetId],
    known: &[SubmissionFingerprint],
) -> Option<(SubmissionId, f64)> {
    known
        .iter()
        .map(|fingerprint| (fingerprint.submission_id, fingerprint.overlap(page_assets)))
        .filter(|(_, overlap)| *overlap >= SUSPECTED_OVERLAP)
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::event::SourceFingerprint;

    fn asset(text: &str) -> AssetId {
        AssetId::of(text.as_bytes())
    }

    #[test]
    fn batch_dedupe_keeps_first_fingerprint() {
        let mut table = BatchDedupe::new();
        let hash = ContentHash::of(b"same");
        assert!(table.hit(hash).is_none());
        table.record(hash, SourceFingerprint::of_label("first"));
        table.record(hash, SourceFingerprint::of_label("second"));
        assert_eq!(table.len(), 1);
        assert_eq!(
            table.hit(hash).map(|fp| fp.path_hash.clone()),
            Some(SourceFingerprint::of_label("first").path_hash)
        );
    }

    #[test]
    fn overlap_and_suspected_threshold() {
        let pages = vec![
            asset("p1"),
            asset("p2"),
            asset("p3"),
            asset("p4"),
            asset("p5"),
        ];
        let known = vec![
            SubmissionFingerprint {
                submission_id: SubmissionId::from_ulid(eg_domain::Ulid::from_u128(1)),
                pages: vec![asset("p1"), asset("p2"), asset("p3"), asset("p4")],
            },
            SubmissionFingerprint {
                submission_id: SubmissionId::from_ulid(eg_domain::Ulid::from_u128(2)),
                pages: vec![asset("p1"), asset("x"), asset("y"), asset("z")],
            },
        ];
        assert!((known[0].overlap(&pages) - 0.8).abs() < 1e-9);
        assert!((known[1].overlap(&pages) - 0.2).abs() < 1e-9);
        let found = suspicious_submission(&pages, &known).unwrap();
        assert_eq!(
            found.0,
            SubmissionId::from_ulid(eg_domain::Ulid::from_u128(1))
        );
        // 新集合里只有 1 页且命中既有页 → 重合 100%，按规范（≥80%）仍算疑似；
        // 用完全不同的页验证「无重合 → 不疑似」。
        assert!(suspicious_submission(&[asset("only-new")], &known).is_none());
        assert!(suspicious_submission(&[], &known).is_none());
    }
}
