//! 提交、页面、页面质量与答案区域（`SPEC-domain.md` §6）。
//!
//! 不变量：
//! - 原图资产只读（`Page.asset` 是内容寻址 id，本模块不提供删除/改写路径）；
//! - 页面坐标一律归一化（`BBox` 校验）；
//! - 同一页面同一题最多一个主区域（跨页题按页多段，由 `align` 保证）。

use serde::{Deserialize, Serialize};

use crate::error::{DomainError, invalid_field};
use crate::geometry::BBox;
use crate::hash::AssetId;
use crate::ids::{ClassroomId, PageId, PaperId, QuestionId, SegmentId, StudentId, SubmissionId};
use crate::time::Timestamp;
use crate::values::{ExifOrientation, Rotation};

/// 流水线阶段（§6 `Stage`；用于失败状态与日志）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// 采集。
    Capture,
    /// 预处理。
    Preprocess,
    /// 识别。
    Recognize,
    /// 对齐。
    Align,
    /// 判分。
    Grade,
    /// 复核。
    Review,
    /// 导出。
    Export,
}

/// 提交状态（§6 `SubmissionStatus`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionStatus {
    /// 已导入。
    Imported,
    /// 已预处理。
    Preprocessed,
    /// 已识别。
    Recognized,
    /// 已对齐。
    Aligned,
    /// 已判分。
    Graded,
    /// 已复核。
    Reviewed,
    /// 已导出。
    Exported,
    /// 失败（附阶段与消息）。
    Failed {
        /// 失败阶段。
        stage: Stage,
        /// 面向教师的消息。
        message: String,
    },
}

impl SubmissionStatus {
    /// 失败阶段与消息（非失败状态为空）。
    #[must_use]
    pub fn failure(&self) -> Option<(Stage, &str)> {
        match self {
            Self::Failed { stage, message } => Some((*stage, message)),
            _ => None,
        }
    }
}

/// 提交（§6 `Submission`）：文件名即身份的第一来源。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Submission {
    /// 提交 id。
    pub id: SubmissionId,
    /// 试卷 id。
    pub paper_id: PaperId,
    /// 班级 id。
    pub classroom_id: ClassroomId,
    /// 学生 id（可空；未匹配时必须人工补）。
    pub student_id: Option<StudentId>,
    /// 原始文件名（含扩展名，不被改写）。
    pub source_file_name: String,
    /// 去扩展名文件名（身份匹配第一来源）。
    pub source_stem: String,
    /// 页面列表。
    pub pages: Vec<Page>,
    /// 状态。
    pub status: SubmissionStatus,
    /// 创建时间。
    pub created_at: Timestamp,
}

impl Submission {
    /// 校验提交（文件名非空、页号唯一且为正、页面自身校验）。
    ///
    /// # Errors
    ///
    /// 失败返回 [`DomainError`]。
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.source_file_name.trim().is_empty() {
            return Err(invalid_field("source_file_name", "原始文件名不得为空"));
        }
        if self.source_stem.trim().is_empty() {
            return Err(invalid_field("source_stem", "去扩展名文件名不得为空"));
        }
        if self.pages.is_empty() {
            return Err(invalid_field("pages", "提交至少需要 1 页"));
        }
        let mut seen: Vec<u32> = Vec::with_capacity(self.pages.len());
        for page in &self.pages {
            if seen.contains(&page.page_no) {
                return Err(invalid_field(
                    "page_no",
                    format!("页号 {} 重复", page.page_no),
                ));
            }
            seen.push(page.page_no);
            if page.submission_id != self.id {
                return Err(invalid_field(
                    "submission_id",
                    format!("页面 {} 的 submission_id 与提交不一致", page.id),
                ));
            }
            page.validate()?;
        }
        if let SubmissionStatus::Failed { message, .. } = &self.status
            && message.trim().is_empty()
        {
            return Err(invalid_field("status.message", "失败状态必须带可读消息"));
        }
        Ok(())
    }
}

/// 页面（§6 `Page`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Page {
    /// 页面 id。
    pub id: PageId,
    /// 所属提交 id。
    pub submission_id: SubmissionId,
    /// 页号（1 起）。
    pub page_no: u32,
    /// 原图资产（只读，内容寻址）。
    pub asset: AssetId,
    /// 像素宽度。
    pub width: u32,
    /// 像素高度。
    pub height: u32,
    /// 已应用的方向校正角度（0/90/180/270）。
    pub rotation_applied: Rotation,
    /// EXIF 方向（1–8，0 表示无 EXIF；镜像信息不可丢）。
    pub exif_orientation: ExifOrientation,
    /// 页面质量。
    pub quality: PageQuality,
    /// 派生资产列表，顺序固定为「方向校正图、透视矫正图、二值图、缩略图」。
    pub derived: Vec<AssetId>,
}

impl Page {
    /// 校验页面（页号、尺寸、派生资产数量与去重、质量字段）。
    ///
    /// # Errors
    ///
    /// 失败返回 [`DomainError`]。
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.page_no == 0 {
            return Err(invalid_field("page_no", "页号必须从 1 起"));
        }
        if self.width == 0 || self.height == 0 {
            return Err(invalid_field("width", "页面宽高必须大于 0"));
        }
        if self.derived.len() > 4 {
            return Err(invalid_field(
                "derived",
                "派生资产最多 4 项（方向校正图、透视矫正图、二值图、缩略图）",
            ));
        }
        let mut seen: Vec<AssetId> = Vec::with_capacity(self.derived.len());
        for asset in &self.derived {
            if seen.contains(asset) {
                return Err(invalid_field("derived", "派生资产重复"));
            }
            seen.push(*asset);
        }
        self.quality.validate()
    }
}

/// 页面质量（§6 `PageQuality`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageQuality {
    /// 模糊度（拉普拉斯方差），越大越清晰。
    pub blur_var: f64,
    /// 曝光度量。
    pub exposure: f64,
    /// 倾斜角（度）。
    pub skew_deg: f64,
    /// 分辨率是否达标。
    pub resolution_ok: bool,
    /// 质量结论。
    pub verdict: QualityVerdict,
}

impl PageQuality {
    /// 校验质量字段（有限数值、拒绝原因非空）。
    ///
    /// # Errors
    ///
    /// 失败返回 [`DomainError`]。
    pub fn validate(&self) -> Result<(), DomainError> {
        for (field, value) in [
            ("quality.blur_var", self.blur_var),
            ("quality.exposure", self.exposure),
            ("quality.skew_deg", self.skew_deg),
        ] {
            if !value.is_finite() {
                return Err(invalid_field("quality", format!("{field} 必须是有限数值")));
            }
        }
        if self.blur_var < 0.0 {
            return Err(invalid_field("quality.blur_var", "模糊度方差不得为负"));
        }
        if let QualityVerdict::Rejected { reason } = &self.verdict
            && reason.trim().is_empty()
        {
            return Err(invalid_field("quality.verdict", "拒绝必须附可读原因"));
        }
        Ok(())
    }
}

/// 质量结论（§6 `QualityVerdict`：良好/可接受/拒绝附原因）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityVerdict {
    /// 良好。
    Good,
    /// 可接受。
    Acceptable,
    /// 拒绝（附原因）。
    Rejected {
        /// 面向教师的原因。
        reason: String,
    },
}

/// 答案区域（§6 `Segment`）：页面坐标归一化。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    /// 区域 id。
    pub id: SegmentId,
    /// 所属页面 id。
    pub page_id: PageId,
    /// 对应题目 id（可空 = 未对齐）。
    pub question_id: Option<QuestionId>,
    /// 归一化坐标框。
    pub bbox: BBox,
    /// 裁片资产。
    pub crop: AssetId,
    /// 区域定位方式。
    pub method: SegmentMethod,
}

impl Segment {
    /// 校验区域（坐标框合法；题目可空表示未对齐）。
    ///
    /// # Errors
    ///
    /// 坐标框非法时返回 [`DomainError`]。
    pub fn validate(&self) -> Result<(), DomainError> {
        self.bbox.validate()
    }
}

/// 区域定位方式（§6 `SegmentMethod`：模板区域/版面检测/人工）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentMethod {
    /// 模板区域。
    TemplateRegion,
    /// 版面检测。
    LayoutDetection,
    /// 人工。
    Manual,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::ids::Ulid;

    fn timestamp() -> Timestamp {
        crate::time::parse_rfc3339("2026-11-02T00:00:00Z").unwrap()
    }

    fn asset(text: &str) -> AssetId {
        AssetId::of(text.as_bytes())
    }

    fn quality() -> PageQuality {
        PageQuality {
            blur_var: 180.0,
            exposure: 0.5,
            skew_deg: 1.2,
            resolution_ok: true,
            verdict: QualityVerdict::Good,
        }
    }

    fn page(page_no: u32) -> Page {
        Page {
            id: PageId::from_ulid(Ulid::from_u128(u128::from(page_no) + 1)),
            submission_id: SubmissionId::from_ulid(Ulid::from_u128(1)),
            page_no,
            asset: asset("original"),
            width: 1600,
            height: 1200,
            rotation_applied: Rotation::CW_90,
            exif_orientation: ExifOrientation::new(6).unwrap(),
            quality: quality(),
            derived: vec![asset("rotated"), asset("rectified")],
        }
    }

    fn submission() -> Submission {
        Submission {
            id: SubmissionId::from_ulid(Ulid::from_u128(1)),
            paper_id: PaperId::from_ulid(Ulid::from_u128(2)),
            classroom_id: ClassroomId::from_ulid(Ulid::from_u128(3)),
            student_id: Some(StudentId::from_ulid(Ulid::from_u128(4))),
            source_file_name: "张三_期中数学.jpg".to_owned(),
            source_stem: "张三_期中数学".to_owned(),
            pages: vec![page(1), page(2)],
            status: SubmissionStatus::Imported,
            created_at: timestamp(),
        }
    }

    #[test]
    fn valid_submission_passes() {
        assert!(submission().validate().is_ok());
    }

    #[test]
    fn filename_and_pages_are_validated() {
        let mut invalid = submission();
        invalid.source_file_name = "  ".to_owned();
        assert!(invalid.validate().is_err());

        let mut invalid = submission();
        invalid.pages.clear();
        assert!(invalid.validate().is_err());

        let mut invalid = submission();
        invalid.pages[1].page_no = 1;
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn failed_status_requires_message() {
        let mut invalid = submission();
        invalid.status = SubmissionStatus::Failed {
            stage: Stage::Recognize,
            message: " ".to_owned(),
        };
        assert!(invalid.validate().is_err());
        invalid.status = SubmissionStatus::Failed {
            stage: Stage::Recognize,
            message: "模型不可用".to_owned(),
        };
        assert!(invalid.validate().is_ok());
        assert_eq!(
            invalid.status.failure(),
            Some((Stage::Recognize, "模型不可用"))
        );
        assert_eq!(SubmissionStatus::Graded.failure(), None);
    }

    #[test]
    fn page_constraints() {
        let mut invalid = page(1);
        invalid.page_no = 0;
        assert!(invalid.validate().is_err());

        let mut invalid = page(1);
        invalid.width = 0;
        assert!(invalid.validate().is_err());

        let mut invalid = page(1);
        invalid.derived = vec![asset("a"), asset("b"), asset("c"), asset("d"), asset("e")];
        assert!(invalid.validate().is_err());

        let mut invalid = page(1);
        invalid.derived = vec![asset("a"), asset("a")];
        assert!(invalid.validate().is_err());

        let mut invalid = page(1);
        invalid.quality.verdict = QualityVerdict::Rejected {
            reason: String::new(),
        };
        assert!(invalid.validate().is_err());

        let mut invalid = page(1);
        invalid.quality.blur_var = f64::NAN;
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn segment_validates_bbox() {
        let segment = Segment {
            id: SegmentId::from_ulid(Ulid::from_u128(9)),
            page_id: PageId::from_ulid(Ulid::from_u128(10)),
            question_id: None,
            bbox: BBox::new(0.1, 0.1, 0.4, 0.4).unwrap(),
            crop: asset("crop"),
            method: SegmentMethod::LayoutDetection,
        };
        assert!(segment.validate().is_ok());
    }

    #[test]
    fn serde_tags_are_snake_case() {
        assert_eq!(
            serde_json::to_string(&Stage::Preprocess).unwrap(),
            "\"preprocess\""
        );
        assert_eq!(
            serde_json::to_string(&SegmentMethod::TemplateRegion).unwrap(),
            "\"template_region\""
        );
        assert_eq!(
            serde_json::to_string(&QualityVerdict::Rejected {
                reason: "模糊".to_owned()
            })
            .unwrap(),
            r#"{"rejected":{"reason":"模糊"}}"#
        );
    }
}
