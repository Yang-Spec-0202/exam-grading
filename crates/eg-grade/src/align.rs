//! 对齐来源适配（T30/T29）：从 align 模块取学生答案、逐题门禁与整卷 ready。
//!
//! 生产实现见 [`crate::adapters::align::AlignReportSource`]（消费 `eg_align::AlignReport`）；
//! [`FixtureAlignSource`] 供测试/回放。未对齐题必须显式进未决/失败，**绝不伪装未作答**。

use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;

use eg_domain::{
    Confidence, EngineStamp, Question, QuestionId, RawAnswer, StudentAnswer, SubmissionId,
};

use crate::pipeline::QuestionAnswer;

/// 对齐来源错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AlignError {
    /// 对齐结果不存在（提交未对齐/未识别）。
    #[error("提交 {0} 没有对齐结果")]
    NotFound(SubmissionId),
    /// 读取/执行对齐失败（I/O/数据库/解析/align 错误）。
    #[error("对齐失败：{0}")]
    Read(String),
    /// 对齐结果非法（片段/置信度）。
    #[error("对齐结果非法：{0}")]
    Invalid(String),
}

/// 一条对齐未决（题级或整卷级）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlignIssue {
    /// 相关题目（整卷级为空：身份/重复提交等）。
    pub question_id: Option<QuestionId>,
    /// 未决种类稳定字符串（如 `region_uncertain`/`missing_page`）。
    pub kind: String,
    /// 教师可读说明。
    pub detail: String,
    /// 是否阻断自动判分（对齐门禁语义）。
    pub blocks_auto_grade: bool,
}

/// 一份提交的对齐结果（判分所需最小投影）。
#[derive(Debug, Clone, PartialEq)]
pub struct AlignedSubmission {
    /// 可构造的学生答案（只为已对齐题）。
    pub answers: Vec<StudentAnswer>,
    /// 逐题允许自动判分（题 id 集合）。
    pub allowed_questions: BTreeSet<QuestionId>,
    /// 整卷是否允许自动判分（身份确认/无重复/所有题放行/无阻断未决）。
    pub auto_grade_ready: bool,
    /// 未决列表（含题级与整卷级）。
    pub issues: Vec<AlignIssue>,
}

impl AlignedSubmission {
    /// 题级阻断未决的题 id（供流水线显式失败/澄清）。
    #[must_use]
    pub fn blocking_questions(&self) -> BTreeSet<QuestionId> {
        self.issues
            .iter()
            .filter(|issue| issue.blocks_auto_grade)
            .filter_map(|issue| issue.question_id)
            .collect()
    }

    /// 整卷级阻断未决（身份/重复等）。
    #[must_use]
    pub fn whole_submission_blocked(&self) -> bool {
        self.issues
            .iter()
            .any(|issue| issue.blocks_auto_grade && issue.question_id.is_none())
    }
}

/// 对齐来源（生产读 align report；测试 fixture）。
pub trait AlignSource: Send + Sync {
    /// 读取某提交的对齐结果。
    ///
    /// # Errors
    ///
    /// 未对齐/读取失败返回 [`AlignError`]。
    fn aligned_submission(
        &self,
        submission_id: SubmissionId,
    ) -> Result<AlignedSubmission, AlignError>;

    /// 来源身份（进引擎指纹）。
    fn identity(&self) -> String;

    /// 对齐算法/模型版本（进引擎指纹；无则空）。
    fn engine_stamp(&self) -> Option<EngineStamp> {
        None
    }

    /// 仅取学生答案（便捷方法；门禁由 [`Self::aligned_submission`] 提供）。
    ///
    /// # Errors
    ///
    /// 同 [`Self::aligned_submission`]。
    fn aligned_answers(
        &self,
        submission_id: SubmissionId,
    ) -> Result<Vec<StudentAnswer>, AlignError> {
        Ok(self.aligned_submission(submission_id)?.answers)
    }
}

/// fixture 对齐来源（测试/回放）。
#[derive(Debug, Default)]
pub struct FixtureAlignSource {
    submissions: Mutex<HashMap<SubmissionId, AlignedSubmission>>,
    failure: Mutex<Option<AlignError>>,
}

impl FixtureAlignSource {
    /// 空来源。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 注入某提交的对齐答案（视为全部放行、整卷 ready）。
    pub fn insert(&self, submission_id: SubmissionId, answers: Vec<StudentAnswer>) {
        let allowed = answers.iter().map(|answer| answer.question_id).collect();
        self.insert_submission(
            submission_id,
            AlignedSubmission {
                answers,
                allowed_questions: allowed,
                auto_grade_ready: true,
                issues: Vec::new(),
            },
        );
    }

    /// 注入带门禁/未决的完整对齐结果。
    pub fn insert_submission(&self, submission_id: SubmissionId, aligned: AlignedSubmission) {
        if let Ok(mut map) = self.submissions.lock() {
            map.insert(submission_id, aligned);
        }
    }

    /// 让所有读取失败（模拟 align 不可用）。
    pub fn fail(&self, error: AlignError) {
        if let Ok(mut failure) = self.failure.lock() {
            *failure = Some(error);
        }
    }
}

impl AlignSource for FixtureAlignSource {
    fn aligned_submission(
        &self,
        submission_id: SubmissionId,
    ) -> Result<AlignedSubmission, AlignError> {
        if let Some(error) = self.failure.lock().ok().and_then(|failure| failure.clone()) {
            return Err(error);
        }
        self.submissions
            .lock()
            .ok()
            .and_then(|map| map.get(&submission_id).cloned())
            .ok_or(AlignError::NotFound(submission_id))
    }

    fn identity(&self) -> String {
        "fixture-align-v1".to_owned()
    }
}

/// 将 `StudentAnswer` 映射为判分输入（保留识别证据；图片资产由平台懒加载）。
#[must_use]
pub fn to_question_answer(question: &Question, student: &StudentAnswer) -> QuestionAnswer {
    let recognition = student
        .recognition_id
        .or_else(|| student.parts.iter().map(|part| part.recognition_id).next());
    let mut answer = QuestionAnswer::new(
        question.clone(),
        RawAnswer {
            text: student.raw.text.clone(),
            latex: student.raw.latex.clone(),
            blocks: student.raw.blocks.clone(),
            source: student.raw.source,
        },
        student.recognition_confidence.min(Confidence::ONE),
    );
    answer.recognition = recognition;
    answer.assets = Vec::new();
    answer.kind_known = true;
    answer
}
