//! 幂等键构造（`SPEC-grade.md` §8 重复判分）。
//!
//! 键 = 提交 + 题 + 答案键版本 + 规则版本 + 识别哈希 + 引擎指纹 + 模式；
//! 命中返回既有记录，不重复判分/计费。持久化由 `IdempotencyStore` 适配器负责
//! （TODO：生产接 `eg-platform::repo`）。

use eg_domain::{GradeMode, QuestionId, RawAnswer, Sha256Hex, SubmissionId};

use crate::ports::IdempotencyKey;

/// 构造幂等键（识别哈希只含答案文本/LaTeX，不含个人信息）。
#[must_use]
pub fn idempotency_key(
    submission_id: SubmissionId,
    question_id: QuestionId,
    key_version: u32,
    rubric_version: u32,
    raw: &RawAnswer,
    engine_fingerprint: &str,
    mode: GradeMode,
) -> IdempotencyKey {
    let material = format!(
        "text={};latex={}",
        raw.text,
        raw.latex.as_deref().unwrap_or("")
    );
    IdempotencyKey {
        submission_id,
        question_id,
        key_version,
        rubric_version,
        recognition_hash: Sha256Hex::of(material.as_bytes()).to_hex(),
        engine_fingerprint: engine_fingerprint.to_owned(),
        mode,
    }
}
