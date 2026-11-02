//! 端口：页面/识别结果/ID 生成（`eg-capture`/`eg-recognize` 并行实现期间的解耦缝）。
//!
//! TODO(适配器)：接口冻结后在此文件补两个真实适配器，核心算法不变。
//! - `CapturePages<S: AssetSink...>`：由 `eg_capture::ImportOutcome.submissions` 直接构造
//!   （当前 [`SubmissionPages`] 已消费其产物 `eg_domain::Submission`，实际无需新依赖）；
//! - `RecognizeRecognition<'a>`：由 `eg_recognize::RecognizeOutcome` 投影 `Recognition`
//!   （需其公开结构冻结；当前用 [`MapRecognitionSource`] 承接测试与组合根注入）。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use eg_domain::{AssetId, PageId, Recognition, SegmentId, Submission, Ulid};

use crate::error::AlignError;

/// 对齐所需的页面投影（不依赖 capture 的派生图/质量细节）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct PageRef {
    /// 页面 id。
    pub page_id: PageId,
    /// 页号（1 起，提交内唯一）。
    pub page_no: u32,
    /// 页面资产（内容寻址；区域 `crop` 暂用它占位，见 `region.rs` TODO）。
    pub asset: AssetId,
    /// 像素宽度。
    pub width: u32,
    /// 像素高度。
    pub height: u32,
}

/// 页面来源（单份提交内的有序页列表）。
pub trait PageSource {
    /// 页面列表（按 `page_no` 升序、无重复）。
    fn pages(&self) -> &[PageRef];
}

/// 由 `eg_domain::Submission`（capture 产物）构造页投影；同时校验页号唯一与正数。
#[derive(Debug, Clone)]
pub struct SubmissionPages {
    pages: Vec<PageRef>,
}

impl SubmissionPages {
    /// 从提交构造（页号必须 1 起且唯一）。
    ///
    /// # Errors
    ///
    /// 页号非法/重复时返回 [`AlignError::InputMismatch`]。
    pub fn from_submission(submission: &Submission) -> Result<Self, AlignError> {
        let mut pages = Vec::with_capacity(submission.pages.len());
        for page in &submission.pages {
            if page.page_no == 0 {
                return Err(AlignError::InputMismatch(format!(
                    "页面 {} 的页号为 0",
                    page.id
                )));
            }
            pages.push(PageRef {
                page_id: page.id,
                page_no: page.page_no,
                asset: page.asset,
                width: page.width,
                height: page.height,
            });
        }
        Self::from_pages(pages)
    }

    /// 由显式页投影构造（页号必须正且唯一）。
    ///
    /// # Errors
    ///
    /// 页号非法/重复时返回 [`AlignError::InputMismatch`]。
    pub fn from_pages(mut pages: Vec<PageRef>) -> Result<Self, AlignError> {
        pages.sort_by_key(|page| page.page_no);
        for window in pages.windows(2) {
            if window[0].page_no == window[1].page_no {
                return Err(AlignError::InputMismatch(format!(
                    "页号重复：{}",
                    window[0].page_no
                )));
            }
        }
        if pages.iter().any(|page| page.page_no == 0) {
            return Err(AlignError::InputMismatch("页号必须从 1 起".to_owned()));
        }
        Ok(Self { pages })
    }
}

impl PageSource for SubmissionPages {
    fn pages(&self) -> &[PageRef] {
        &self.pages
    }
}

/// 页级识别结果来源（按 `page_id` 查询；缺失即该页无识别结果）。
pub trait RecognitionSource {
    /// 该页的识别结果（`None` = 未识别/不可用）。
    fn recognition(&self, page_id: PageId) -> Option<&Recognition>;
}

/// 内存识别结果表（夹具与组合根注入用；同一页重复注册显式报错）。
#[derive(Debug, Clone, Default)]
pub struct MapRecognitionSource {
    map: BTreeMap<PageId, Recognition>,
}

impl MapRecognitionSource {
    /// 由识别结果列表构造。
    ///
    /// # Errors
    ///
    /// 同一页重复出现时返回 [`AlignError::InputMismatch`]。
    pub fn new(items: Vec<Recognition>) -> Result<Self, AlignError> {
        let mut map = BTreeMap::new();
        for item in items {
            if map.insert(item.page_id, item).is_some() {
                return Err(AlignError::InputMismatch(
                    "同一页存在多个识别结果".to_owned(),
                ));
            }
        }
        Ok(Self { map })
    }

    /// 已注册页数。
    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

impl RecognitionSource for MapRecognitionSource {
    fn recognition(&self, page_id: PageId) -> Option<&Recognition> {
        self.map.get(&page_id)
    }
}

impl RecognitionSource for BTreeMap<PageId, Recognition> {
    fn recognition(&self, page_id: PageId) -> Option<&Recognition> {
        self.get(&page_id)
    }
}

/// ID 生成端口（区域/人工修正需要新 id；测试用 [`SeqIds`]，生产用平台 `UlidFactory`）。
pub trait IdPort {
    /// 生成新的区域 id。
    ///
    /// # Errors
    ///
    /// id 生成失败时返回 [`AlignError`]（不得吞掉）。
    fn next_segment_id(&self) -> Result<SegmentId, AlignError>;
}

/// 确定性 id 源（单线程夹具；生产不得使用）。
#[derive(Debug)]
pub struct SeqIds {
    next: AtomicU64,
}

impl SeqIds {
    /// 从 `start` 开始递增。
    #[must_use]
    pub fn new(start: u128) -> Self {
        Self {
            next: AtomicU64::new(u64::try_from(start).unwrap_or(u64::MAX)),
        }
    }
}

impl Default for SeqIds {
    fn default() -> Self {
        Self::new(1)
    }
}

impl IdPort for SeqIds {
    fn next_segment_id(&self) -> Result<SegmentId, AlignError> {
        let value = self.next.fetch_add(1, Ordering::SeqCst);
        Ok(SegmentId::from_ulid(Ulid::from_u128(u128::from(value))))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn submission_pages_sort_and_validate() {
        let asset = AssetId::of(b"page");
        let page = |no: u32| PageRef {
            page_id: PageId::from_ulid(Ulid::from_u128(u128::from(no))),
            page_no: no,
            asset,
            width: 100,
            height: 200,
        };
        let pages = SubmissionPages::from_pages(vec![page(2), page(1)]).unwrap();
        assert_eq!(
            pages.pages().iter().map(|p| p.page_no).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert!(SubmissionPages::from_pages(vec![page(1), page(1)]).is_err());
        assert!(SubmissionPages::from_pages(vec![page(0)]).is_err());
    }

    #[test]
    fn seq_ids_are_monotonic() {
        let ids = SeqIds::new(10);
        let first = ids.next_segment_id().unwrap();
        let second = ids.next_segment_id().unwrap();
        assert_ne!(first, second);
    }
}
