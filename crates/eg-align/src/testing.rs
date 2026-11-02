//! 测试/组合根夹具（程序构造，不依赖真实图片或模型）。
//!
//! 生产代码不调用本模块；它让 `align` 的测试与上游（`recognize`/`grade`）集成时
//! 可以零资产构造一致的输入。

use std::cell::Cell;

use eg_domain::{
    AssetId, BBox, Block, BlockKind, ClassroomId, Confidence, EngineStamp, ModelRole, Page, PageId,
    PageQuality, Paper, PaperTemplate, QualityVerdict, Question, QuestionId, Recognition,
    RecognitionId, RosterSource, Rotation, Sha256Hex, Student, StudentId, Submission, SubmissionId,
    TemplateRegion, Timestamp, Ulid,
};

use crate::identity::{IdentityHint, IdentityHintKind, IdentityHintSource};
use crate::ports::PageRef;

/// 固定时钟（同一输入重跑的指纹时间也稳定）。
#[derive(Debug, Clone, Copy)]
pub struct FixedClock {
    at: Timestamp,
}

impl FixedClock {
    /// 用指定时间点构造。
    #[must_use]
    pub fn new(at: Timestamp) -> Self {
        Self { at }
    }

    /// 夹具默认时间（`2026-11-02T00:00:00Z`）。
    #[must_use]
    pub fn fixture() -> Self {
        Self {
            at: eg_domain::time::parse_rfc3339("2026-11-02T00:00:00Z")
                .unwrap_or(Timestamp::UNIX_EPOCH),
        }
    }
}

impl Default for FixedClock {
    fn default() -> Self {
        Self::fixture()
    }
}

impl eg_domain::Clock for FixedClock {
    fn now(&self) -> Timestamp {
        self.at
    }
}

/// 构造学生。
#[must_use]
pub fn student(classroom: ClassroomId, id: u128, student_no: &str, name: &str) -> Student {
    Student {
        id: StudentId::from_ulid(Ulid::from_u128(id)),
        classroom_id: classroom,
        student_no: student_no.to_owned(),
        name: name.to_owned(),
        source: RosterSource::RosterImport,
    }
}

/// 构造班级 id。
#[must_use]
pub fn classroom(id: u128) -> ClassroomId {
    ClassroomId::from_ulid(Ulid::from_u128(id))
}

/// 构造名单。
///
/// # Errors
///
/// 名单非法时返回 [`crate::AlignError::Roster`]。
pub fn roster(
    classroom: ClassroomId,
    students: Vec<Student>,
) -> Result<crate::roster::Roster, crate::AlignError> {
    crate::roster::Roster::new(classroom, students)
}

/// 构造试卷：每题 1 分、标签按 `labels` 顺序、`key_version=1`。
#[must_use]
pub fn paper(id: u128, classroom: ClassroomId, labels: &[&str]) -> Paper {
    let paper_id = eg_domain::PaperId::from_ulid(Ulid::from_u128(id));
    let questions: Vec<Question> = labels
        .iter()
        .enumerate()
        .map(|(index, label)| Question {
            id: QuestionId::from_ulid(Ulid::from_u128(1000 + index as u128)),
            paper_id,
            ordinal: u32::try_from(index + 1).unwrap_or(u32::MAX),
            label: (*label).to_owned(),
            kind: eg_domain::QuestionKind::NumericOnly,
            max_score: eg_domain::MilliScore::from_milli(1000)
                .unwrap_or(eg_domain::MilliScore::ZERO),
            stem: None,
            key: eg_domain::AnswerKey::Missing,
            rubric: None,
            subject_hint: None,
            knowledge_tags: Vec::new(),
        })
        .collect();
    Paper {
        id: paper_id,
        classroom_id: classroom,
        title: "夹具试卷".to_owned(),
        subject: eg_domain::SubjectId::math(),
        total_score: eg_domain::MilliScore::from_milli(
            1000 * i64::try_from(labels.len()).unwrap_or(1),
        )
        .unwrap_or(eg_domain::MilliScore::ZERO),
        questions,
        key_version: 1,
        rubric_version: 1,
        created_at: eg_domain::time::parse_rfc3339("2026-11-02T00:00:00Z")
            .unwrap_or(Timestamp::UNIX_EPOCH),
    }
}

/// 构造单页（内容资产由 `text` 的内容寻址得到；质量恒为良好）。
#[must_use]
pub fn page(submission_id: u128, page_no: u32, text: &str) -> Page {
    Page {
        id: PageId::from_ulid(Ulid::from_u128(
            u128::from(page_no) + submission_id.saturating_mul(100),
        )),
        submission_id: SubmissionId::from_ulid(Ulid::from_u128(submission_id)),
        page_no,
        asset: AssetId::of(text.as_bytes()),
        width: 1240,
        height: 1754,
        rotation_applied: Rotation::NONE,
        exif_orientation: eg_domain::ExifOrientation::NONE,
        quality: PageQuality {
            blur_var: 500.0,
            exposure: 0.5,
            skew_deg: 0.0,
            resolution_ok: true,
            verdict: QualityVerdict::Good,
        },
        derived: Vec::new(),
    }
}

/// 构造提交（页由 `pages` 描述，`asset_text` 决定内容哈希）。
#[must_use]
pub fn submission(
    id: u128,
    classroom: ClassroomId,
    paper_id: u128,
    stem: &str,
    page_texts: &[&str],
) -> Submission {
    let pages: Vec<Page> = page_texts
        .iter()
        .enumerate()
        .map(|(index, text)| page(id, u32::try_from(index + 1).unwrap_or(u32::MAX), text))
        .collect();
    Submission {
        id: SubmissionId::from_ulid(Ulid::from_u128(id)),
        paper_id: eg_domain::PaperId::from_ulid(Ulid::from_u128(paper_id)),
        classroom_id: classroom,
        student_id: None,
        source_file_name: format!("{stem}.jpg"),
        source_stem: stem.to_owned(),
        pages,
        status: eg_domain::SubmissionStatus::Imported,
        created_at: eg_domain::time::parse_rfc3339("2026-11-02T00:00:00Z")
            .unwrap_or(Timestamp::UNIX_EPOCH),
    }
}

/// 提交页投影。
#[must_use]
pub fn page_refs(submission: &Submission) -> Vec<PageRef> {
    submission
        .pages
        .iter()
        .map(|page| PageRef {
            page_id: page.id,
            page_no: page.page_no,
            asset: page.asset,
            width: page.width,
            height: page.height,
        })
        .collect()
}

/// 构造归一化坐标框（整宽条带）。
#[must_use]
pub fn strip(y: f64, height: f64) -> BBox {
    BBox {
        x: 0.0,
        y,
        w: 1.0,
        h: height,
    }
}

/// 构造文本识别块（整宽条带）。
#[must_use]
pub fn block(text: &str, y: f64, height: f64) -> Block {
    Block {
        kind: BlockKind::Text,
        bbox: strip(y, height),
        text: text.to_owned(),
        latex: None,
        confidence: Confidence::new(0.95).unwrap_or(Confidence::ZERO),
    }
}

/// 构造识别结果（fixture 引擎指纹）。
#[must_use]
pub fn recognition(page_id: PageId, blocks: Vec<Block>) -> Recognition {
    let text = blocks
        .iter()
        .map(|block| block.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    Recognition {
        id: RecognitionId::from_ulid(Ulid::from_u128(
            u128::try_from(page_id.to_string().len()).unwrap_or(0) + 7,
        )),
        page_id,
        segment_id: None,
        blocks,
        text,
        engine: EngineStamp {
            role: ModelRole::Ocr,
            backend: eg_domain::BackendId::fixture(),
            model_id: "fixture-ocr".to_owned(),
            model_version: "0".to_owned(),
            prompt_hash: None,
            params_hash: Sha256Hex::of(b"fixture"),
        },
        confidence: Confidence::new(0.95).unwrap_or(Confidence::ZERO),
        created_at: eg_domain::time::parse_rfc3339("2026-11-02T00:00:00Z")
            .unwrap_or(Timestamp::UNIX_EPOCH),
    }
}

/// 带题号锚点的页识别结果：`anchors` 为（题号文本, y, 高度）序列。
#[must_use]
pub fn recognition_with_anchors(page_id: PageId, anchors: &[(&str, f64, f64)]) -> Recognition {
    let blocks = anchors
        .iter()
        .map(|(text, y, height)| block(text, *y, *height))
        .collect();
    recognition(page_id, blocks)
}

/// 构造试卷模板（区域按（题序, 页号, bbox）描述）。
#[must_use]
pub fn template(
    paper: &Paper,
    version: u32,
    page_count: u32,
    regions: &[(usize, u32, BBox)],
) -> PaperTemplate {
    PaperTemplate {
        paper_id: paper.id,
        version,
        page_count,
        regions: regions
            .iter()
            .filter_map(|(index, page_no, bbox)| {
                paper.questions.get(*index).map(|question| TemplateRegion {
                    page_no: *page_no,
                    question_id: question.id,
                    bbox: *bbox,
                })
            })
            .collect(),
    }
}

/// 计数 OCR 兜底调用（证明默认配置 0 次调用）。
#[derive(Debug, Default)]
pub struct CountingHints {
    calls: Cell<u32>,
    items: Vec<IdentityHint>,
}

impl CountingHints {
    /// 构造空提示的计数端口。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 构造带提示的计数端口。
    #[must_use]
    pub fn with(items: Vec<IdentityHint>) -> Self {
        Self {
            calls: Cell::new(0),
            items,
        }
    }

    /// 追加一条提示。
    pub fn push(&mut self, item: IdentityHint) {
        self.items.push(item);
    }

    /// 已发生的调用次数。
    #[must_use]
    pub fn calls(&self) -> u32 {
        self.calls.get()
    }
}

impl IdentityHintSource for CountingHints {
    fn hints(&self, _submission: &Submission) -> Vec<IdentityHint> {
        self.calls.set(self.calls.get() + 1);
        self.items.clone()
    }
}

/// 构造一条兜底提示。
#[must_use]
pub fn hint(kind: IdentityHintKind, text: &str) -> IdentityHint {
    IdentityHint {
        kind,
        text: text.to_owned(),
    }
}

/// 构造默认空提示的计数端口。
#[must_use]
pub fn counting_hints() -> CountingHints {
    CountingHints::new()
}
