//! 试卷、题目与试卷模板（`SPEC-domain.md` §3）。
//!
//! 不变量（§3）：
//! 1. `Paper.total_score` 等于各题 `max_score` 之和；
//! 2. 题目 `label` 在试卷内唯一、`ordinal` 从 1 起连续无空洞；
//! 3. 模板 `version` 与 `Paper.key_version` 不一致时禁止自动对齐；
//! 4. 答案键/规则修改必须递增对应版本（由调用方执行，本模块只承载版本字段）。
//!
//! 聚合类型字段公开便于序列化与装配；使用前必须调用 `validate`，
//! 反序列化后同样需要校验（历史数据宽容，见规范 §10）。

use serde::{Deserialize, Serialize};

use crate::answer::AnswerKey;
use crate::error::{DomainError, invalid_field};
use crate::geometry::BBox;
use crate::hash::AssetId;
use crate::ids::{ClassroomId, PaperId, QuestionId, StudentId};
use crate::rubric::Rubric;
use crate::score::MilliScore;
use crate::time::Timestamp;
use crate::values::SubjectId;

/// 学生名单来源（§3 `RosterSource`：名单导入/手工新增/文件名推断）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RosterSource {
    /// 名单导入。
    RosterImport,
    /// 手工新增。
    ManualAdd,
    /// 文件名推断。
    FileNameInferred,
}

/// 班级（§3）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Classroom {
    /// 班级 id。
    pub id: ClassroomId,
    /// 班级名。
    pub name: String,
    /// 学期。
    pub term: String,
}

/// 学生（§3）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Student {
    /// 学生 id。
    pub id: StudentId,
    /// 所属班级 id。
    pub classroom_id: ClassroomId,
    /// 学号。
    pub student_no: String,
    /// 姓名。
    pub name: String,
    /// 名单来源。
    pub source: RosterSource,
}

/// 试卷（§3）。字段语义见 `SPEC-domain.md` §3；总分与题目顺序由 [`Paper::validate`] 校验。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Paper {
    /// 试卷 id。
    pub id: PaperId,
    /// 所属班级 id。
    pub classroom_id: ClassroomId,
    /// 试卷标题。
    pub title: String,
    /// 学科 id（v1 恒为 math）。
    pub subject: SubjectId,
    /// 总分，必须等于各题满分之和。
    pub total_score: MilliScore,
    /// 题目列表，按 `ordinal` 升序。
    pub questions: Vec<Question>,
    /// 答案键修订号，每次修改 +1。
    pub key_version: u32,
    /// 规则版本，规则变更 +1。
    pub rubric_version: u32,
    /// 创建时间。
    pub created_at: Timestamp,
}

impl Paper {
    /// 校验 §3 不变量（总分、题号连续、标签唯一、题目自身校验）。
    ///
    /// # Errors
    ///
    /// 违反任一不变量时返回带字段名/中文原因的 [`DomainError`]。
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.title.trim().is_empty() {
            return Err(invalid_field("title", "试卷标题不得为空"));
        }
        if self.questions.is_empty() {
            return Err(invalid_field("questions", "试卷至少需要 1 道题"));
        }
        let mut total = MilliScore::ZERO;
        let mut labels: Vec<&str> = Vec::with_capacity(self.questions.len());
        for (index, question) in self.questions.iter().enumerate() {
            let expected = u32::try_from(index + 1)
                .map_err(|_| invalid_field("questions", "题目数量超出 u32 序号上限"))?;
            if question.ordinal != expected {
                return Err(DomainError::NonContiguousOrdinal {
                    expected,
                    found: question.ordinal,
                });
            }
            if question.paper_id != self.id {
                return Err(invalid_field(
                    "paper_id",
                    format!("题目 {} 的 paper_id 与试卷不一致", question.id),
                ));
            }
            let label = question.label.trim();
            if label.is_empty() {
                return Err(invalid_field(
                    "label",
                    format!("题目 {} 的 label 为空", question.id),
                ));
            }
            if labels.contains(&label) {
                return Err(DomainError::DuplicateQuestionLabel {
                    label: label.to_owned(),
                });
            }
            labels.push(label);
            question.validate()?;
            total = total.checked_add_score(question.max_score)?;
        }
        if total != self.total_score {
            return Err(DomainError::TotalScoreMismatch {
                declared: self.total_score,
                computed: total,
            });
        }
        Ok(())
    }
}

/// 题目类型（§3 `QuestionKind`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionKind {
    /// 单选。
    SingleChoice,
    /// 多选。
    MultipleChoice,
    /// 判断。
    TrueFalse,
    /// 填空。
    FillInBlank,
    /// 纯数值。
    NumericOnly,
    /// 计算/解答。
    Calculation,
    /// 证明。
    Proof,
    /// 简答。
    ShortAnswer,
    /// 论述。
    Essay,
    /// 其他。
    Other,
}

/// 题干内容引用（§3 `ContentRef`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentRef {
    /// 纯文本。
    Text(String),
    /// LaTeX 文本。
    Latex(String),
    /// 资产引用（图片等）。
    Asset(AssetId),
}

/// 题目（§3）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Question {
    /// 题目 id。
    pub id: QuestionId,
    /// 所属试卷 id。
    pub paper_id: PaperId,
    /// 卷面顺序，从 1 起。
    pub ordinal: u32,
    /// 题号标签（如 `17(2)`），试卷内唯一。
    pub label: String,
    /// 题型。
    pub kind: QuestionKind,
    /// 满分。
    pub max_score: MilliScore,
    /// 题干（可选）。
    pub stem: Option<ContentRef>,
    /// 答案键。
    pub key: AnswerKey,
    /// 评分标准（可选；答案键为「仅评分标准」时必填）。
    pub rubric: Option<Rubric>,
    /// 学科提示（可选）。
    pub subject_hint: Option<SubjectId>,
    /// 知识点标签。
    pub knowledge_tags: Vec<String>,
}

impl Question {
    /// 校验题目自身与答案键/评分标准的一致性（§4 不变量 2、§4 不变量 1）。
    ///
    /// # Errors
    ///
    /// 失败返回带字段名/中文原因的 [`DomainError`]。
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.label.trim().is_empty() {
            return Err(invalid_field("label", "题目标签不得为空"));
        }
        if self.ordinal == 0 {
            return Err(DomainError::NonContiguousOrdinal {
                expected: 1,
                found: 0,
            });
        }
        if self.max_score.is_zero() {
            return Err(invalid_field("max_score", "题目满分必须大于 0"));
        }
        if self.key.requires_rubric() && self.rubric.is_none() {
            return Err(DomainError::MissingRubricForAnswerKey);
        }
        if let Some(rubric) = &self.rubric {
            rubric.validate(self.kind)?;
            if rubric.total != self.max_score {
                return Err(DomainError::RubricTotalMismatch {
                    declared: rubric.total,
                    computed: self.max_score,
                });
            }
        }
        self.key.validate()?;
        Ok(())
    }
}

/// 试卷模板（§3 `PaperTemplate`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaperTemplate {
    /// 所属试卷 id。
    pub paper_id: PaperId,
    /// 模板版本，必须与 `Paper.key_version` 对齐。
    pub version: u32,
    /// 页数。
    pub page_count: u32,
    /// 模板区域列表。
    pub regions: Vec<TemplateRegion>,
}

impl PaperTemplate {
    /// 校验模板自身（页数、区域页号范围、区域唯一性）。
    ///
    /// # Errors
    ///
    /// 失败返回 [`DomainError`]。
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.page_count == 0 {
            return Err(invalid_field("page_count", "页数必须大于 0"));
        }
        let mut seen: Vec<(u32, QuestionId)> = Vec::with_capacity(self.regions.len());
        for region in &self.regions {
            if region.page_no == 0 || region.page_no > self.page_count {
                return Err(invalid_field(
                    "page_no",
                    format!("区域页号 {} 超出 1..={}", region.page_no, self.page_count),
                ));
            }
            region.bbox.validate()?;
            let key = (region.page_no, region.question_id);
            if seen.contains(&key) {
                return Err(invalid_field(
                    "regions",
                    format!(
                        "第 {} 页题目 {} 存在重复模板区域",
                        region.page_no, region.question_id
                    ),
                ));
            }
            seen.push(key);
        }
        Ok(())
    }

    /// §3 不变量 3：模板版本与试卷答案键版本不一致时禁止自动对齐。
    ///
    /// # Errors
    ///
    /// 试卷不一致或版本不一致时返回错误；调用方应转为人工对齐。
    pub fn ensure_auto_align_allowed(&self, paper: &Paper) -> Result<(), DomainError> {
        if self.paper_id != paper.id {
            return Err(DomainError::TemplatePaperMismatch);
        }
        if self.version != paper.key_version {
            return Err(DomainError::TemplateVersionMismatch {
                template_version: self.version,
                key_version: paper.key_version,
            });
        }
        Ok(())
    }
}

/// 模板区域（§3 `TemplateRegion`）：归一化坐标，左上原点。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateRegion {
    /// 页号（1 起）。
    pub page_no: u32,
    /// 对应题目 id。
    pub question_id: QuestionId,
    /// 归一化坐标框。
    pub bbox: BBox,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::ids::Ulid;

    fn paper_id() -> PaperId {
        PaperId::from_ulid(Ulid::from_u128(1))
    }

    fn question(ordinal: u32, label: &str, max_score: i64) -> Question {
        Question {
            id: QuestionId::from_ulid(Ulid::from_u128(u128::from(ordinal) + 10)),
            paper_id: paper_id(),
            ordinal,
            label: label.to_owned(),
            kind: QuestionKind::NumericOnly,
            max_score: MilliScore::from_milli(max_score).unwrap(),
            stem: None,
            key: AnswerKey::Missing,
            rubric: None,
            subject_hint: None,
            knowledge_tags: Vec::new(),
        }
    }

    fn valid_paper() -> Paper {
        Paper {
            id: paper_id(),
            classroom_id: ClassroomId::from_ulid(Ulid::from_u128(2)),
            title: "期中数学".to_owned(),
            subject: SubjectId::math(),
            total_score: MilliScore::from_milli(5000).unwrap(),
            questions: vec![question(1, "1", 2000), question(2, "2", 3000)],
            key_version: 1,
            rubric_version: 1,
            created_at: crate::time::parse_rfc3339("2026-11-02T00:00:00Z").unwrap(),
        }
    }

    #[test]
    fn valid_paper_passes() {
        assert!(valid_paper().validate().is_ok());
    }

    #[test]
    fn total_score_mismatch_is_rejected() {
        let mut paper = valid_paper();
        paper.total_score = MilliScore::from_milli(4000).unwrap();
        assert!(matches!(
            paper.validate(),
            Err(DomainError::TotalScoreMismatch { .. })
        ));
    }

    #[test]
    fn ordinal_gap_is_rejected() {
        let mut paper = valid_paper();
        paper.questions[1].ordinal = 3;
        assert!(matches!(
            paper.validate(),
            Err(DomainError::NonContiguousOrdinal {
                expected: 2,
                found: 3
            })
        ));
    }

    #[test]
    fn duplicate_label_is_rejected() {
        let mut paper = valid_paper();
        paper.questions[1].label = "1".to_owned();
        assert!(matches!(
            paper.validate(),
            Err(DomainError::DuplicateQuestionLabel { .. })
        ));
    }

    #[test]
    fn empty_paper_is_rejected() {
        let mut paper = valid_paper();
        paper.questions.clear();
        paper.total_score = MilliScore::ZERO;
        assert!(paper.validate().is_err());
    }

    #[test]
    fn question_requires_rubric_for_rubric_only_key() {
        let mut q = question(1, "1", 2000);
        q.key = AnswerKey::RubricOnly;
        assert!(matches!(
            q.validate(),
            Err(DomainError::MissingRubricForAnswerKey)
        ));
    }

    #[test]
    fn question_rejects_zero_score() {
        let q = question(1, "1", 0);
        assert!(q.validate().is_err());
    }

    #[test]
    fn template_alignment_requires_matching_version() {
        let paper = valid_paper();
        let region = TemplateRegion {
            page_no: 1,
            question_id: paper.questions[0].id,
            bbox: BBox::new(0.0, 0.0, 0.5, 0.5).unwrap(),
        };
        let template = PaperTemplate {
            paper_id: paper.id,
            version: 1,
            page_count: 1,
            regions: vec![region.clone()],
        };
        assert!(template.validate().is_ok());
        assert!(template.ensure_auto_align_allowed(&paper).is_ok());

        let stale = PaperTemplate {
            version: 2,
            ..template.clone()
        };
        assert!(matches!(
            stale.ensure_auto_align_allowed(&paper),
            Err(DomainError::TemplateVersionMismatch { .. })
        ));

        let out_of_range = PaperTemplate {
            page_count: 0,
            ..template
        };
        assert!(out_of_range.validate().is_err());
    }

    #[test]
    fn template_duplicate_region_is_rejected() {
        let paper = valid_paper();
        let region = TemplateRegion {
            page_no: 1,
            question_id: paper.questions[0].id,
            bbox: BBox::new(0.0, 0.0, 0.5, 0.5).unwrap(),
        };
        let template = PaperTemplate {
            paper_id: paper.id,
            version: 1,
            page_count: 1,
            regions: vec![region.clone(), region],
        };
        assert!(template.validate().is_err());
    }

    #[test]
    fn serde_uses_snake_case_tags() {
        let json = serde_json::to_string(&QuestionKind::MultipleChoice).unwrap();
        assert_eq!(json, "\"multiple_choice\"");
        let json = serde_json::to_string(&RosterSource::FileNameInferred).unwrap();
        assert_eq!(json, "\"file_name_inferred\"");
        let paper = valid_paper();
        let json = serde_json::to_string(&paper).unwrap();
        assert!(json.contains("\"total_score\":5000"));
        assert!(json.contains("\"questions\":["));
        let back: Paper = serde_json::from_str(&json).unwrap();
        assert_eq!(back, paper);
    }
}
