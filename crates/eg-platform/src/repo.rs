//! 仓储：首批表的类型化读写（`SPEC-platform.md` §4.1）。
//!
//! 约定：
//! - 领域对象以「版本化 JSON」落 `body` 列（`{"schema_version":N,"body":{…}}`），
//!   只追加不改写；查询字段冗余成列。
//! - 聚合写（`Paper`+`Question`、`Submission`+`Page`）在同一事务内完成；
//!   跨表半提交由调用方事务保证（本模块函数只接受 `&Transaction`/`&Connection`）。
//! - 原图资产行只由 [`insert_asset_row`] 写入（资产文件由 `assets` 模块管理）。

use eg_domain::{
    AnswerKey, AssetId, Classroom, ClassroomId, EngineStamp, GradeId, GradeMode, GradeRecord,
    GradeStatus, MilliScore, ModelCall, Page, Paper, PaperId, PartialChoiceRule, Question,
    QuestionId, QuestionKind, Recognition, RecognitionId, ReviewReasonKind, Rubric, RubricDraft,
    RuleText, SchemaVersioned, ScoringMode, Segment, SegmentId, Sha256Hex, Student, StudentId,
    Submission, SubmissionExclusion, SubmissionId, TemplateId, Timestamp, Ulid,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{PlatformError, Result};

/// 版本化 JSON 外层（`SPEC-domain.md` §10：持久化对象带 schema 版本）。
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Envelope<T> {
    schema_version: u32,
    body: T,
}

/// 序列化为版本化 JSON。
fn encode<T: Serialize + SchemaVersioned>(value: &T) -> Result<String> {
    serde_json::to_string(&Envelope {
        schema_version: T::SCHEMA_VERSION,
        body: value,
    })
    .map_err(|error| PlatformError::Serialize(error.to_string()))
}

/// 序列化为普通 JSON（用于冗余列：枚举/子结构，不承载 schema 版本）。
fn encode_raw<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|error| PlatformError::Serialize(error.to_string()))
}

/// 反序列化版本化 JSON；版本高于本程序支持版本时报「数据损坏」。
fn decode<T: DeserializeOwned + SchemaVersioned>(text: &str) -> Result<T> {
    let envelope: Envelope<T> = serde_json::from_str(text)
        .map_err(|error| PlatformError::DatabaseCorrupt(format!("JSON 数据无法解析：{error}")))?;
    if envelope.schema_version > T::SCHEMA_VERSION {
        return Err(PlatformError::DatabaseCorrupt(format!(
            "数据 schema 版本 {} 高于本程序支持的 {}（请升级程序）",
            envelope.schema_version,
            T::SCHEMA_VERSION
        )));
    }
    Ok(envelope.body)
}

/// 时间列写入格式（RFC3339 UTC）。
fn timestamp_text(value: Timestamp) -> String {
    eg_domain::time::format_rfc3339(value)
}

/// 时间列读取。
fn parse_timestamp(text: &str, column: &str) -> Result<Timestamp> {
    eg_domain::time::parse_rfc3339(text).map_err(|error| {
        PlatformError::DatabaseCorrupt(format!("列 {column} 的时间不合法：{text}（{error}）"))
    })
}

// ---------------------------------------------------------------------------
// 班级 / 学生
// ---------------------------------------------------------------------------

/// 写入班级；`source` 为名单来源说明（可空，`domain::Classroom` 无该字段）。
///
/// # Errors
///
/// 约束冲突或序列化失败时返回错误。
pub fn insert_classroom(
    tx: &Transaction<'_>,
    classroom: &Classroom,
    source: Option<&str>,
    created_at: Timestamp,
) -> Result<()> {
    tx.execute(
        "INSERT INTO classroom(id, name, term, source, body, created_at)          VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            classroom.id.to_string(),
            classroom.name,
            classroom.term,
            source,
            encode(classroom)?,
            timestamp_text(created_at)
        ],
    )
    .map_err(PlatformError::from)?;
    Ok(())
}

/// 按 id 读取班级。
///
/// # Errors
///
/// 查询或解析失败时返回错误。
pub fn get_classroom(conn: &Connection, id: ClassroomId) -> Result<Option<Classroom>> {
    let body: Option<String> = conn
        .query_row(
            "SELECT body FROM classroom WHERE id = ?1",
            [id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(PlatformError::from)?;
    body.map(|text| decode(&text)).transpose()
}

/// 列出全部班级（按名称排序）。
///
/// # Errors
///
/// 查询或解析失败时返回错误。
pub fn list_classrooms(conn: &Connection) -> Result<Vec<Classroom>> {
    let mut statement = conn
        .prepare("SELECT body FROM classroom ORDER BY name")
        .map_err(PlatformError::from)?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(PlatformError::from)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(decode(&row.map_err(PlatformError::from)?)?);
    }
    Ok(out)
}

/// 写入学生。
///
/// # Errors
///
/// 约束冲突（学号唯一、班级外键）或序列化失败时返回错误。
pub fn insert_student(
    tx: &Transaction<'_>,
    student: &Student,
    created_at: Timestamp,
) -> Result<()> {
    tx.execute(
        "INSERT INTO student(id, classroom_id, student_no, name, source, body, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            student.id.to_string(),
            student.classroom_id.to_string(),
            student.student_no,
            student.name,
            encode_raw(&student.source)?,
            encode(student)?,
            timestamp_text(created_at)
        ],
    )
    .map_err(PlatformError::from)?;
    Ok(())
}

/// 按学号读取学生（名单匹配用）。
///
/// # Errors
///
/// 查询或反序列化失败时返回错误。
pub fn get_student_by_no(conn: &Connection, student_no: &str) -> Result<Option<Student>> {
    let body: Option<String> = conn
        .query_row(
            "SELECT body FROM student WHERE student_no = ?1",
            [student_no],
            |row| row.get(0),
        )
        .optional()
        .map_err(PlatformError::from)?;
    body.map(|text| decode(&text)).transpose()
}

/// 列出班级学生。
///
/// # Errors
///
/// 查询或反序列化失败时返回错误。
pub fn list_students(conn: &Connection, classroom_id: ClassroomId) -> Result<Vec<Student>> {
    let mut statement = conn
        .prepare("SELECT body FROM student WHERE classroom_id = ?1 ORDER BY student_no")
        .map_err(PlatformError::from)?;
    let rows = statement
        .query_map([classroom_id.to_string()], |row| row.get::<_, String>(0))
        .map_err(PlatformError::from)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(decode(&row.map_err(PlatformError::from)?)?);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// 试卷 / 题目
// ---------------------------------------------------------------------------

/// 写入试卷与其全部题目（同一事务由调用方保证）。
///
/// # Errors
///
/// 约束冲突或序列化失败时返回错误。
pub fn insert_paper(tx: &Transaction<'_>, paper: &Paper) -> Result<()> {
    tx.execute(
        "INSERT INTO paper(id, classroom_id, title, subject, total_score, key_version, \
         rubric_version, body, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            paper.id.to_string(),
            paper.classroom_id.to_string(),
            paper.title,
            encode_raw(&paper.subject)?,
            paper.total_score.as_milli(),
            paper.key_version,
            paper.rubric_version,
            encode(paper)?,
            timestamp_text(paper.created_at)
        ],
    )
    .map_err(PlatformError::from)?;
    for question in &paper.questions {
        insert_question(tx, question)?;
    }
    Ok(())
}

/// 写入单题（试卷已存在）。
///
/// # Errors
///
/// 约束冲突或序列化失败时返回错误。
pub fn insert_question(tx: &Transaction<'_>, question: &Question) -> Result<()> {
    let kind = serde_json::to_string(&question.kind)
        .map_err(|error| PlatformError::Serialize(error.to_string()))?;
    tx.execute(
        "INSERT INTO question(id, paper_id, ordinal, label, kind, max_score, body) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            question.id.to_string(),
            question.paper_id.to_string(),
            question.ordinal,
            question.label,
            kind,
            question.max_score.as_milli(),
            encode(question)?
        ],
    )
    .map_err(PlatformError::from)?;
    Ok(())
}

/// 读取试卷（`body` 为权威，含题目列表）。
///
/// # Errors
///
/// 查询或反序列化失败时返回错误。
pub fn get_paper(conn: &Connection, id: PaperId) -> Result<Option<Paper>> {
    // `rubric_version` 是可变指针（版本升级只递增）；列是权威，`body` 内的同名字段
    // 可能落后于列（v1 时代的行），读取时以列为准覆盖，避免上层看到旧版本。
    let row: Option<(String, i64)> = conn
        .query_row(
            "SELECT body, rubric_version FROM paper WHERE id = ?1",
            [id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(PlatformError::from)?;
    match row {
        None => Ok(None),
        Some((body, version)) => {
            let mut paper: Paper = decode(&body)?;
            paper.rubric_version = u32::try_from(version).map_err(|_| {
                PlatformError::DatabaseCorrupt(format!("paper.rubric_version 越界：{version}"))
            })?;
            Ok(Some(paper))
        }
    }
}

/// 按卷面顺序列出题目。
///
/// # Errors
///
/// 查询或反序列化失败时返回错误。
pub fn list_questions(conn: &Connection, paper_id: PaperId) -> Result<Vec<Question>> {
    let mut statement = conn
        .prepare("SELECT body FROM question WHERE paper_id = ?1 ORDER BY ordinal")
        .map_err(PlatformError::from)?;
    let rows = statement
        .query_map([paper_id.to_string()], |row| row.get::<_, String>(0))
        .map_err(PlatformError::from)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(decode(&row.map_err(PlatformError::from)?)?);
    }
    Ok(out)
}

/// 写入答案键版本快照（只追加，不回溯改）。
///
/// # Errors
///
/// 约束冲突或序列化失败时返回错误。
pub fn insert_paper_key_revision(
    tx: &Transaction<'_>,
    paper_id: PaperId,
    key_version: u32,
    key: &AnswerKey,
    created_at: Timestamp,
) -> Result<()> {
    tx.execute(
        "INSERT INTO paper_key_revision(paper_id, key_version, body, created_at) \
         VALUES (?1, ?2, ?3, ?4)",
        params![
            paper_id.to_string(),
            key_version,
            encode(key)?,
            timestamp_text(created_at)
        ],
    )
    .map_err(PlatformError::from)?;
    Ok(())
}

/// 读取答案键版本快照。
///
/// # Errors
///
/// 查询或反序列化失败时返回错误。
pub fn get_paper_key_revision(
    conn: &Connection,
    paper_id: PaperId,
    key_version: u32,
) -> Result<Option<AnswerKey>> {
    let body: Option<String> = conn
        .query_row(
            "SELECT body FROM paper_key_revision WHERE paper_id = ?1 AND key_version = ?2",
            params![paper_id.to_string(), key_version],
            |row| row.get(0),
        )
        .optional()
        .map_err(PlatformError::from)?;
    body.map(|text| decode(&text)).transpose()
}

/// 写入评分标准版本快照。
///
/// # Errors
///
/// 约束冲突或序列化失败时返回错误。
pub fn insert_rubric_revision(
    tx: &Transaction<'_>,
    paper_id: PaperId,
    rubric_version: u32,
    rubric: &Rubric,
    created_at: Timestamp,
) -> Result<()> {
    tx.execute(
        "INSERT INTO rubric_revision(paper_id, rubric_version, body, created_at) \
         VALUES (?1, ?2, ?3, ?4)",
        params![
            paper_id.to_string(),
            rubric_version,
            encode(rubric)?,
            timestamp_text(created_at)
        ],
    )
    .map_err(PlatformError::from)?;
    Ok(())
}

/// 读取评分标准版本快照。
///
/// # Errors
///
/// 查询或反序列化失败时返回错误。
pub fn get_rubric_revision(
    conn: &Connection,
    paper_id: PaperId,
    rubric_version: u32,
) -> Result<Option<Rubric>> {
    let body: Option<String> = conn
        .query_row(
            "SELECT body FROM rubric_revision WHERE paper_id = ?1 AND rubric_version = ?2",
            params![paper_id.to_string(), rubric_version],
            |row| row.get(0),
        )
        .optional()
        .map_err(PlatformError::from)?;
    body.map(|text| decode(&text)).transpose()
}

/// 写入题目规则原文。
///
/// # Errors
///
/// 约束冲突时返回错误。
pub fn insert_rule_text(
    tx: &Transaction<'_>,
    rule: &RuleText,
    created_at: Timestamp,
) -> Result<()> {
    tx.execute(
        "INSERT INTO rule_text(question_id, text, source, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![
            rule.question_id.to_string(),
            rule.text,
            encode_raw(&rule.source)?,
            timestamp_text(created_at)
        ],
    )
    .map_err(PlatformError::from)?;
    Ok(())
}

/// 读取题目规则原文。
///
/// # Errors
///
/// 查询或反序列化失败时返回错误。
pub fn get_rule_text(conn: &Connection, question_id: QuestionId) -> Result<Option<RuleText>> {
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT text, source FROM rule_text WHERE question_id = ?1",
            [question_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(PlatformError::from)?;
    match row {
        None => Ok(None),
        Some((text, source)) => {
            let source = serde_json::from_str(&source)
                .map_err(|error| PlatformError::DatabaseCorrupt(error.to_string()))?;
            Ok(Some(RuleText {
                question_id,
                text,
                source,
            }))
        }
    }
}

/// 写入规则草稿。
///
/// # Errors
///
/// 约束冲突或序列化失败时返回错误。
pub fn insert_rubric_draft(tx: &Transaction<'_>, draft: &RubricDraft) -> Result<()> {
    let status = serde_json::to_string(&draft.status)
        .map_err(|error| PlatformError::Serialize(error.to_string()))?;
    let origin = serde_json::to_string(&draft.origin)
        .map_err(|error| PlatformError::Serialize(error.to_string()))?;
    let validation = serde_json::to_string(&draft.validation)
        .map_err(|error| PlatformError::Serialize(error.to_string()))?;
    tx.execute(
        "INSERT INTO rubric_draft(id, question_id, status, origin, model_call_id, validation, \
         body, created_at, confirmed_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            draft.id.to_string(),
            draft.question_id.to_string(),
            status,
            origin,
            draft.model_call_id.map(|id| id.to_string()),
            validation,
            encode(draft)?,
            timestamp_text(draft.created_at),
            draft.confirmed_at.map(timestamp_text)
        ],
    )
    .map_err(PlatformError::from)?;
    Ok(())
}

/// 读取规则草稿。
///
/// # Errors
///
/// 查询或反序列化失败时返回错误。
pub fn get_rubric_draft(
    conn: &Connection,
    draft_id: eg_domain::RubricId,
) -> Result<Option<RubricDraft>> {
    let body: Option<String> = conn
        .query_row(
            "SELECT body FROM rubric_draft WHERE id = ?1",
            [draft_id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(PlatformError::from)?;
    body.map(|text| decode(&text)).transpose()
}

// ---------------------------------------------------------------------------
// 规则草稿伴随负载 / 生效规则（按题目）
// ---------------------------------------------------------------------------

/// 规则草稿的伴随负载（schema v2 新增列）。
///
/// 对应 `eg-rubric::StoredDraft` 中**未进入** `eg_domain::RubricDraft` 的字段：
/// 题型、满分、计分方式、部分给分档位与模板引用。领域草稿本身仍以 `body`
/// 版本化 JSON 为权威，本结构只承载冗余列。
///
/// 旧（v1）草稿没有这些列，回读时全部为 `None`；调用方必须显式处理
/// “负载未记录”的情况，不得用默认值猜造。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RubricDraftPayload {
    /// 题目题型（确认与静态校验的输入）。
    pub question_kind: Option<QuestionKind>,
    /// 题目满分（千分之一分）。
    pub max_score: Option<MilliScore>,
    /// 计分方式。
    pub mode: Option<ScoringMode>,
    /// 部分给分档位。
    pub partial_choice: Option<PartialChoiceRule>,
    /// 模板 id（模板套用来源时填写）。
    pub template_id: Option<TemplateId>,
    /// 模板版本（编码规则见 `eg-rubric::TemplateVersion::as_u32`）。
    pub template_version: Option<u32>,
    /// 模板完整引用 JSON（由 `eg-rubric` 序列化，platform 不解析其内部结构）。
    pub template: Option<serde_json::Value>,
}

/// 草稿 + 伴随负载。
#[derive(Debug, Clone, PartialEq)]
pub struct StoredRubricDraft {
    /// 领域草稿（状态机与校验结果）。
    pub draft: RubricDraft,
    /// 伴随负载（v1 草稿为空）。
    pub payload: RubricDraftPayload,
}

/// 草稿行原始列（`body` + schema v2 负载列）。
type RawDraftRow = (
    String,
    String,
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<String>,
    String,
    String,
);

/// 查询列：`body` + 负载列 + 身份列（`created_at`/`question_id` 以列为权威）。
const DRAFT_COLUMNS: &str = "body, question_kind, max_score_milli, scoring_mode, partial_choice, \
                             template_id, template_version, template_ref, created_at, question_id";

/// 行映射（列顺序与 [`DRAFT_COLUMNS`] 一致）。
fn raw_draft_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawDraftRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
    ))
}

/// 序列化可选 JSON 列。
fn encode_optional_json<T: Serialize>(value: Option<&T>) -> Result<Option<String>> {
    value.map(encode_raw).transpose()
}

/// 反序列化可选 JSON 列（格式非法视为数据损坏）。
fn decode_optional_json<T: DeserializeOwned>(
    text: Option<&str>,
    column: &str,
) -> Result<Option<T>> {
    text.map(|value| {
        serde_json::from_str(value).map_err(|error| {
            PlatformError::DatabaseCorrupt(format!("列 {column} 的 JSON 无法解析：{error}"))
        })
    })
    .transpose()
}

/// 读取一行草稿（含 schema v2 负载列）。
fn draft_row_to_stored(row: &RawDraftRow) -> Result<StoredRubricDraft> {
    let (
        body,
        kind,
        max_score,
        mode,
        partial,
        template_id,
        template_version,
        template,
        created_at,
        question_id,
    ) = row;
    let mut draft: RubricDraft = decode(body)?;
    // 身份字段以列为权威：upsert 只更新状态/负载，不改写 question_id/created_at。
    draft.question_id = QuestionId::parse(question_id)
        .map_err(|error| PlatformError::DatabaseCorrupt(error.to_string()))?;
    draft.created_at = parse_timestamp(created_at, "created_at")?;
    let question_kind = if kind.is_empty() {
        None
    } else {
        Some(serde_json::from_str(kind).map_err(|error| {
            PlatformError::DatabaseCorrupt(format!("列 question_kind 的 JSON 无法解析：{error}"))
        })?)
    };
    let max_score = if *max_score > 0 {
        Some(MilliScore::from_milli(*max_score).map_err(|error| {
            PlatformError::DatabaseCorrupt(format!("列 max_score_milli 非法：{error}"))
        })?)
    } else {
        None
    };
    let template_id = template_id
        .as_deref()
        .map(TemplateId::new)
        .transpose()
        .map_err(|error| PlatformError::DatabaseCorrupt(format!("列 template_id 非法：{error}")))?;
    let template_version = template_version
        .map(|value| {
            u32::try_from(value).map_err(|_| {
                PlatformError::DatabaseCorrupt(format!("列 template_version 越界：{value}"))
            })
        })
        .transpose()?;
    Ok(StoredRubricDraft {
        draft,
        payload: RubricDraftPayload {
            question_kind,
            max_score,
            mode: decode_optional_json(mode.as_deref(), "scoring_mode")?,
            partial_choice: decode_optional_json(partial.as_deref(), "partial_choice")?,
            template_id,
            template_version,
            template: decode_optional_json(template.as_deref(), "template_ref")?,
        },
    })
}

/// 写入带伴随负载的规则草稿（schema v2）。
///
/// 与 [`insert_rubric_draft`] 的区别：同时写题型/满分/计分方式/部分给分/模板引用列。
/// 同一 `RubricId` 重复插入会触发主键冲突（草稿历史由审计承载，调用方应在事务内
/// 先确认不存在；`eg-rubric` 端口的 `save_draft` 语义是“同 id 覆盖”，适配层需
/// 通过业务事务 + 审计实现，platform 不提供隐式覆盖）。
///
/// # Errors
///
/// 约束冲突或序列化失败时返回错误。
pub fn insert_stored_rubric_draft(tx: &Transaction<'_>, stored: &StoredRubricDraft) -> Result<()> {
    let draft = &stored.draft;
    let payload = &stored.payload;
    let status = serde_json::to_string(&draft.status)
        .map_err(|error| PlatformError::Serialize(error.to_string()))?;
    let origin = serde_json::to_string(&draft.origin)
        .map_err(|error| PlatformError::Serialize(error.to_string()))?;
    let validation = serde_json::to_string(&draft.validation)
        .map_err(|error| PlatformError::Serialize(error.to_string()))?;
    let question_kind = match &payload.question_kind {
        Some(kind) => serde_json::to_string(kind)
            .map_err(|error| PlatformError::Serialize(error.to_string()))?,
        None => String::new(),
    };
    let max_score_milli = payload.max_score.map_or(0, MilliScore::as_milli);
    let scoring_mode = encode_optional_json(payload.mode.as_ref())?;
    let partial_choice = encode_optional_json(payload.partial_choice.as_ref())?;
    let template = encode_optional_json(payload.template.as_ref())?;
    tx.execute(
        "INSERT INTO rubric_draft(id, question_id, status, origin, model_call_id, validation, \
         body, created_at, confirmed_at, question_kind, max_score_milli, scoring_mode, \
         partial_choice, template_id, template_version, template_ref) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        params![
            draft.id.to_string(),
            draft.question_id.to_string(),
            status,
            origin,
            draft.model_call_id.map(|id| id.to_string()),
            validation,
            encode(draft)?,
            timestamp_text(draft.created_at),
            draft.confirmed_at.map(timestamp_text),
            question_kind,
            max_score_milli,
            scoring_mode,
            partial_choice,
            payload.template_id.as_ref().map(TemplateId::as_str),
            payload.template_version.map(i64::from),
            template,
        ],
    )
    .map_err(PlatformError::from)?;
    Ok(())
}

/// 保存草稿（同 id 覆盖为最新状态；`question_id`/`created_at` 身份字段不变）。
///
/// 对应 `eg-rubric::RubricStore::save_draft` 的「同 id 覆盖为最新状态」语义：
/// 状态迁移（草稿 → 待确认 → 已确认/已拒绝）、编辑评分项、重跑校验都走本函数。
/// 历史由审计承载，故这里覆盖当前行而不是追加新行；`created_at` 保留首次创建时间。
///
/// # Errors
///
/// 约束冲突（题目不存在）或序列化失败时返回错误。
pub fn upsert_stored_rubric_draft(tx: &Transaction<'_>, stored: &StoredRubricDraft) -> Result<()> {
    let draft = &stored.draft;
    let payload = &stored.payload;
    let status = serde_json::to_string(&draft.status)
        .map_err(|error| PlatformError::Serialize(error.to_string()))?;
    let origin = serde_json::to_string(&draft.origin)
        .map_err(|error| PlatformError::Serialize(error.to_string()))?;
    let validation = serde_json::to_string(&draft.validation)
        .map_err(|error| PlatformError::Serialize(error.to_string()))?;
    let question_kind = match &payload.question_kind {
        Some(kind) => serde_json::to_string(kind)
            .map_err(|error| PlatformError::Serialize(error.to_string()))?,
        None => String::new(),
    };
    let max_score_milli = payload.max_score.map_or(0, MilliScore::as_milli);
    let scoring_mode = encode_optional_json(payload.mode.as_ref())?;
    let partial_choice = encode_optional_json(payload.partial_choice.as_ref())?;
    let template = encode_optional_json(payload.template.as_ref())?;
    tx.execute(
        "INSERT INTO rubric_draft(id, question_id, status, origin, model_call_id, validation, \
         body, created_at, confirmed_at, question_kind, max_score_milli, scoring_mode, \
         partial_choice, template_id, template_version, template_ref) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16) \
         ON CONFLICT(id) DO UPDATE SET \
         status = excluded.status, origin = excluded.origin, \
         model_call_id = excluded.model_call_id, validation = excluded.validation, \
         body = excluded.body, confirmed_at = excluded.confirmed_at, \
         question_kind = excluded.question_kind, max_score_milli = excluded.max_score_milli, \
         scoring_mode = excluded.scoring_mode, partial_choice = excluded.partial_choice, \
         template_id = excluded.template_id, template_version = excluded.template_version, \
         template_ref = excluded.template_ref",
        params![
            draft.id.to_string(),
            draft.question_id.to_string(),
            status,
            origin,
            draft.model_call_id.map(|id| id.to_string()),
            validation,
            encode(draft)?,
            timestamp_text(draft.created_at),
            draft.confirmed_at.map(timestamp_text),
            question_kind,
            max_score_milli,
            scoring_mode,
            partial_choice,
            payload.template_id.as_ref().map(TemplateId::as_str),
            payload.template_version.map(i64::from),
            template,
        ],
    )
    .map_err(PlatformError::from)?;
    Ok(())
}

/// 读取草稿及其伴随负载（v1 草稿的负载字段全为 `None`）。
///
/// # Errors
///
/// 查询或反序列化失败时返回错误。
pub fn get_stored_rubric_draft(
    conn: &Connection,
    draft_id: eg_domain::RubricId,
) -> Result<Option<StoredRubricDraft>> {
    let row: Option<RawDraftRow> = conn
        .query_row(
            &format!("SELECT {DRAFT_COLUMNS} FROM rubric_draft WHERE id = ?1"),
            [draft_id.to_string()],
            raw_draft_row,
        )
        .optional()
        .map_err(PlatformError::from)?;
    row.as_ref().map(draft_row_to_stored).transpose()
}

/// 按题目列出全部草稿（创建时间升序；含 v1 旧行）。
///
/// # Errors
///
/// 查询或反序列化失败时返回错误。
pub fn list_stored_rubric_drafts_by_question(
    conn: &Connection,
    question_id: QuestionId,
) -> Result<Vec<StoredRubricDraft>> {
    let mut statement = conn
        .prepare(&format!(
            "SELECT {DRAFT_COLUMNS} FROM rubric_draft WHERE question_id = ?1 \
             ORDER BY created_at ASC, id ASC"
        ))
        .map_err(PlatformError::from)?;
    let rows = statement
        .query_map([question_id.to_string()], raw_draft_row)
        .map_err(PlatformError::from)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(draft_row_to_stored(&row.map_err(PlatformError::from)?)?);
    }
    Ok(out)
}

/// 按试卷列出全部草稿（题目序号升序、同题创建时间升序；含 v1 旧行）。
///
/// # Errors
///
/// 查询或反序列化失败时返回错误。
pub fn list_stored_rubric_drafts_by_paper(
    conn: &Connection,
    paper_id: PaperId,
) -> Result<Vec<StoredRubricDraft>> {
    let mut statement = conn
        .prepare(
            "SELECT d.body, d.question_kind, d.max_score_milli, d.scoring_mode, \
             d.partial_choice, d.template_id, d.template_version, d.template_ref, \
             d.created_at, d.question_id \
             FROM rubric_draft d JOIN question q ON q.id = d.question_id \
             WHERE q.paper_id = ?1 ORDER BY q.ordinal ASC, d.created_at ASC, d.id ASC",
        )
        .map_err(PlatformError::from)?;
    let rows = statement
        .query_map([paper_id.to_string()], raw_draft_row)
        .map_err(PlatformError::from)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(draft_row_to_stored(&row.map_err(PlatformError::from)?)?);
    }
    Ok(out)
}

/// 读取题目的现行生效规则（最近一次确认写入的版本；无则为 `None`）。
///
/// 说明：生效规则以「试卷级版本 + 题目规则内容」组织（`SPEC-rubric.md` §2.5，
/// `eg-rubric` 集成期记录的平台接入约定）。每条 `rubric_revision` 记录一道题在
/// 某个试卷规则版本下的规则内容；本题最近版本即现行生效规则。
///
/// # Errors
///
/// 查询或反序列化失败时返回错误。
pub fn get_active_rubric(conn: &Connection, question_id: QuestionId) -> Result<Option<Rubric>> {
    let body: Option<String> = conn
        .query_row(
            "SELECT body FROM rubric_revision WHERE question_id = ?1 \
             ORDER BY rubric_version DESC LIMIT 1",
            [question_id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(PlatformError::from)?;
    body.map(|text| decode(&text)).transpose()
}

/// 题目最近一次生效规则版本（无则为 0）。
///
/// # Errors
///
/// 查询失败时返回错误。
pub fn latest_question_rubric_version(conn: &Connection, question_id: QuestionId) -> Result<u32> {
    let version: Option<i64> = conn
        .query_row(
            "SELECT rubric_version FROM rubric_revision WHERE question_id = ?1 \
             ORDER BY rubric_version DESC LIMIT 1",
            [question_id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(PlatformError::from)?;
    match version {
        None => Ok(0),
        Some(value) => u32::try_from(value)
            .map_err(|_| PlatformError::DatabaseCorrupt(format!("rubric_version 越界：{value}"))),
    }
}

/// 题目所属试卷的当前规则版本（CAS 基准；无试卷时返回「数据不存在」）。
///
/// `eg-rubric` 的 `RubricStore::rubric_version` 应接本函数：确认时以试卷级版本
/// 作为 CAS 期望值（规范以 `Paper.rubric_version` 为基准）。
///
/// # Errors
///
/// 题目不存在或版本值非法时返回错误。
pub fn paper_rubric_version(conn: &Connection, question_id: QuestionId) -> Result<u32> {
    let version: Option<i64> = conn
        .query_row(
            "SELECT p.rubric_version FROM question q JOIN paper p ON p.id = q.paper_id \
             WHERE q.id = ?1",
            [question_id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(PlatformError::from)?;
    match version {
        None => Err(PlatformError::NotFound(format!(
            "题目不存在：{question_id}"
        ))),
        Some(value) => u32::try_from(value)
            .map_err(|_| PlatformError::DatabaseCorrupt(format!("rubric_version 越界：{value}"))),
    }
}

/// 写入「题目在某个试卷规则版本下的生效规则快照」。
///
/// 与 v1 的 [`insert_rubric_revision`] 区别：同时写 `question_id` 列，供
/// [`get_active_rubric`] 按题查询。历史行（v1，`question_id` 为 NULL）不受影响。
///
/// # Errors
///
/// 约束冲突或序列化失败时返回错误。
pub fn insert_question_rubric_revision(
    tx: &Transaction<'_>,
    question_id: QuestionId,
    paper_id: PaperId,
    rubric_version: u32,
    rubric: &Rubric,
    created_at: Timestamp,
) -> Result<()> {
    tx.execute(
        "INSERT INTO rubric_revision(paper_id, rubric_version, body, created_at, question_id) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            paper_id.to_string(),
            rubric_version,
            encode(rubric)?,
            timestamp_text(created_at),
            question_id.to_string()
        ],
    )
    .map_err(PlatformError::from)?;
    Ok(())
}

/// 以 CAS 递增试卷规则版本（返回新版本）。
///
/// 同一事务内的「版本 bump」原语：仅当 `paper.rubric_version == expected_version`
/// 时递增 1，并同步更新 `paper.body` 里的 `rubric_version`（列与版本化 JSON 一致）。
/// 通常由 [`save_active_rubric`] 调用；单独使用时调用方负责同事务写
/// `rubric_revision` 与审计。
///
/// # Errors
///
/// 试卷不存在、版本冲突或序列化失败时返回错误。
pub fn advance_paper_rubric_version(
    tx: &Transaction<'_>,
    paper_id: PaperId,
    expected_version: u32,
) -> Result<u32> {
    let row: Option<(String, i64)> = tx
        .query_row(
            "SELECT body, rubric_version FROM paper WHERE id = ?1",
            [paper_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(PlatformError::from)?;
    let (body, actual) =
        row.ok_or_else(|| PlatformError::NotFound(format!("试卷不存在：{paper_id}")))?;
    let actual = u32::try_from(actual).map_err(|_| {
        PlatformError::DatabaseCorrupt(format!("paper.rubric_version 越界：{actual}"))
    })?;
    if actual != expected_version {
        return Err(PlatformError::VersionConflict {
            expected: expected_version,
            actual,
        });
    }
    let next = actual.checked_add(1).ok_or_else(|| {
        PlatformError::InvalidArgument("试卷规则版本已达 u32 上限，无法继续递增".to_owned())
    })?;
    let mut paper: Paper = decode(&body)?;
    paper.rubric_version = next;
    let encoded = encode(&paper)?;
    let changed = tx
        .execute(
            "UPDATE paper SET rubric_version = ?1, body = ?2 \
             WHERE id = ?3 AND rubric_version = ?4",
            params![next, encoded, paper_id.to_string(), actual],
        )
        .map_err(PlatformError::from)?;
    if changed != 1 {
        // 单写连接下不应出现；出现即视为并发 CAS 失败，交由调用方重试。
        return Err(PlatformError::VersionConflict {
            expected: expected_version,
            actual,
        });
    }
    Ok(next)
}

/// 确认生效规则：CAS 递增试卷版本 + 同事务写题目规则快照。
///
/// 语义对应 `eg-rubric::RubricStore::save_active_rubric`：
/// - `expected_version` 是调用方读到的**试卷**规则版本（见 [`paper_rubric_version`]）；
/// - `rubric.version` 必须等于 `expected_version + 1`（版本只增不减）；
/// - 版本冲突返回 [`PlatformError::VersionConflict`]，适配层映射为
///   `RubricError::VersionConflict`；
/// - 审计由调用方在同一业务事务内追加（`AuditLog::business_write`）。
///
/// 返回新的试卷规则版本。
///
/// # Errors
///
/// 版本冲突、题目/试卷不存在、版本号非法或写入失败时返回错误。
pub fn save_active_rubric(
    tx: &Transaction<'_>,
    question_id: QuestionId,
    rubric: &Rubric,
    expected_version: u32,
    created_at: Timestamp,
) -> Result<u32> {
    let paper_id: Option<String> = tx
        .query_row(
            "SELECT paper_id FROM question WHERE id = ?1",
            [question_id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(PlatformError::from)?;
    let paper_id =
        paper_id.ok_or_else(|| PlatformError::NotFound(format!("题目不存在：{question_id}")))?;
    let paper_id = PaperId::parse(&paper_id)
        .map_err(|error| PlatformError::DatabaseCorrupt(error.to_string()))?;
    let next = expected_version.checked_add(1).ok_or_else(|| {
        PlatformError::InvalidArgument("试卷规则版本已达 u32 上限，无法继续递增".to_owned())
    })?;
    // 先校验「版本只增一」，再 bump，避免无效写入走到 CAS 更新。
    if rubric.version != next {
        return Err(PlatformError::InvalidArgument(format!(
            "生效规则版本必须为试卷当前版本加一（期望 {next}，实际 {}）",
            rubric.version
        )));
    }
    let bumped = advance_paper_rubric_version(tx, paper_id, expected_version)?;
    insert_question_rubric_revision(tx, question_id, paper_id, bumped, rubric, created_at)?;
    Ok(bumped)
}

// ---------------------------------------------------------------------------
// 提交 / 页面 / 区域 / 识别
// ---------------------------------------------------------------------------

/// 写入提交与其全部页面（同一事务由调用方保证）。
///
/// # Errors
///
/// 约束冲突（含页面引用的资产必须存在）或序列化失败时返回错误。
pub fn insert_submission(tx: &Transaction<'_>, submission: &Submission) -> Result<()> {
    tx.execute(
        "INSERT INTO submission(id, paper_id, classroom_id, student_id, source_file_name, \
         source_stem, status, body, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            submission.id.to_string(),
            submission.paper_id.to_string(),
            submission.classroom_id.to_string(),
            submission.student_id.map(|id| id.to_string()),
            submission.source_file_name,
            submission.source_stem,
            serde_json::to_string(&submission.status)
                .map_err(|error| PlatformError::Serialize(error.to_string()))?,
            encode(submission)?,
            timestamp_text(submission.created_at)
        ],
    )
    .map_err(PlatformError::from)?;
    for page in &submission.pages {
        insert_page(tx, page, submission.created_at)?;
    }
    Ok(())
}

/// 写入单页（提交已存在）。
///
/// # Errors
///
/// 约束冲突或序列化失败时返回错误。
pub fn insert_page(tx: &Transaction<'_>, page: &Page, created_at: Timestamp) -> Result<()> {
    tx.execute(
        "INSERT INTO page(id, submission_id, page_no, asset, width, height, exif_orientation, \
         quality, body, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            page.id.to_string(),
            page.submission_id.to_string(),
            page.page_no,
            page.asset.to_hex(),
            page.width,
            page.height,
            page.exif_orientation.value(),
            serde_json::to_string(&page.quality)
                .map_err(|error| PlatformError::Serialize(error.to_string()))?,
            encode(page)?,
            timestamp_text(created_at)
        ],
    )
    .map_err(PlatformError::from)?;
    Ok(())
}

/// 读取提交（`body` 为权威，含页面列表）。
///
/// # Errors
///
/// 查询或反序列化失败时返回错误。
pub fn get_submission(conn: &Connection, id: SubmissionId) -> Result<Option<Submission>> {
    let body: Option<String> = conn
        .query_row(
            "SELECT body FROM submission WHERE id = ?1",
            [id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(PlatformError::from)?;
    body.map(|text| decode(&text)).transpose()
}

/// 按页号列出页面。
///
/// # Errors
///
/// 查询或反序列化失败时返回错误。
pub fn list_pages(conn: &Connection, submission_id: SubmissionId) -> Result<Vec<Page>> {
    let mut statement = conn
        .prepare("SELECT body FROM page WHERE submission_id = ?1 ORDER BY page_no")
        .map_err(PlatformError::from)?;
    let rows = statement
        .query_map([submission_id.to_string()], |row| row.get::<_, String>(0))
        .map_err(PlatformError::from)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(decode(&row.map_err(PlatformError::from)?)?);
    }
    Ok(out)
}

/// 写入答案区域。
///
/// # Errors
///
/// 约束冲突或序列化失败时返回错误。
pub fn insert_segment(tx: &Transaction<'_>, segment: &Segment) -> Result<()> {
    let bbox = serde_json::to_string(&segment.bbox)
        .map_err(|error| PlatformError::Serialize(error.to_string()))?;
    let method = serde_json::to_string(&segment.method)
        .map_err(|error| PlatformError::Serialize(error.to_string()))?;
    tx.execute(
        "INSERT INTO segment(id, page_id, question_id, bbox, method, body) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            segment.id.to_string(),
            segment.page_id.to_string(),
            segment.question_id.map(|id| id.to_string()),
            bbox,
            method,
            encode(segment)?
        ],
    )
    .map_err(PlatformError::from)?;
    Ok(())
}

/// 按题目列出区域。
///
/// # Errors
///
/// 查询或反序列化失败时返回错误。
pub fn list_segments_by_question(
    conn: &Connection,
    question_id: QuestionId,
) -> Result<Vec<Segment>> {
    let mut statement = conn
        .prepare("SELECT body FROM segment WHERE question_id = ?1")
        .map_err(PlatformError::from)?;
    let rows = statement
        .query_map([question_id.to_string()], |row| row.get::<_, String>(0))
        .map_err(PlatformError::from)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(decode(&row.map_err(PlatformError::from)?)?);
    }
    Ok(out)
}

/// 写入识别结果。
///
/// # Errors
///
/// 约束冲突或序列化失败时返回错误。
pub fn insert_recognition(tx: &Transaction<'_>, recognition: &Recognition) -> Result<()> {
    let engine = serde_json::to_string(&recognition.engine)
        .map_err(|error| PlatformError::Serialize(error.to_string()))?;
    tx.execute(
        "INSERT INTO recognition(id, page_id, segment_id, confidence, engine, body, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            recognition.id.to_string(),
            recognition.page_id.to_string(),
            recognition.segment_id.map(|id| id.to_string()),
            recognition.confidence.get(),
            engine,
            encode(recognition)?,
            timestamp_text(recognition.created_at)
        ],
    )
    .map_err(PlatformError::from)?;
    Ok(())
}

/// 读取识别结果。
///
/// # Errors
///
/// 查询或反序列化失败时返回错误。
pub fn get_recognition(conn: &Connection, id: RecognitionId) -> Result<Option<Recognition>> {
    let body: Option<String> = conn
        .query_row(
            "SELECT body FROM recognition WHERE id = ?1",
            [id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(PlatformError::from)?;
    body.map(|text| decode(&text)).transpose()
}

// ---------------------------------------------------------------------------
// 判分记录 / 模型调用 / 比较轨迹
// ---------------------------------------------------------------------------

/// 追加判分记录（只追加，同一题同一提交同一 revision 唯一）。
///
/// # Errors
///
/// 约束冲突或序列化失败时返回错误。
pub fn append_grade_record(tx: &Transaction<'_>, record: &GradeRecord) -> Result<()> {
    tx.execute(
        "INSERT INTO grade_record(id, submission_id, question_id, revision, mode, sampled, \
         route, status, confidence, awarded, body, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            record.id.to_string(),
            record.submission_id.to_string(),
            record.question_id.to_string(),
            record.revision,
            serde_json::to_string(&record.mode)
                .map_err(|error| PlatformError::Serialize(error.to_string()))?,
            i64::from(record.sampled),
            serde_json::to_string(&record.route)
                .map_err(|error| PlatformError::Serialize(error.to_string()))?,
            serde_json::to_string(&record.status)
                .map_err(|error| PlatformError::Serialize(error.to_string()))?,
            record.confidence.get(),
            record.awarded.as_milli(),
            encode(record)?,
            timestamp_text(record.created_at)
        ],
    )
    .map_err(PlatformError::from)?;
    Ok(())
}

/// 读取某提交的最新判分记录（按题目、revision 升序）。
///
/// # Errors
///
/// 查询或反序列化失败时返回错误。
pub fn list_grade_records(
    conn: &Connection,
    submission_id: SubmissionId,
) -> Result<Vec<GradeRecord>> {
    let mut statement = conn
        .prepare(
            "SELECT body FROM grade_record WHERE submission_id = ?1 \
             ORDER BY question_id, revision",
        )
        .map_err(PlatformError::from)?;
    let rows = statement
        .query_map([submission_id.to_string()], |row| row.get::<_, String>(0))
        .map_err(PlatformError::from)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(decode(&row.map_err(PlatformError::from)?)?);
    }
    Ok(out)
}

/// 写入模型调用记录（云端调用必须带 `consent_id`，由领域类型校验）。
///
/// # Errors
///
/// 约束冲突或序列化失败时返回错误。
pub fn insert_model_call(tx: &Transaction<'_>, call: &ModelCall) -> Result<()> {
    tx.execute(
        "INSERT INTO model_call(id, task_id, consent_id, role, model_id, input_hash, \
         cost_micros, latency_ms, body, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            call.id.to_string(),
            call.task_id.to_string(),
            call.consent_id.map(|id| id.to_string()),
            serde_json::to_string(&call.role)
                .map_err(|error| PlatformError::Serialize(error.to_string()))?,
            call.engine.model_id,
            call.input_hash.to_hex(),
            i64::try_from(call.cost_micros).unwrap_or(i64::MAX),
            i64::try_from(call.latency_ms).unwrap_or(i64::MAX),
            encode(call)?,
            timestamp_text(call.created_at)
        ],
    )
    .map_err(PlatformError::from)?;
    Ok(())
}

/// 列出某任务的模型调用。
///
/// # Errors
///
/// 查询或反序列化失败时返回错误。
pub fn list_model_calls(conn: &Connection, task_id: eg_domain::TaskId) -> Result<Vec<ModelCall>> {
    let mut statement = conn
        .prepare("SELECT body FROM model_call WHERE task_id = ?1 ORDER BY created_at")
        .map_err(PlatformError::from)?;
    let rows = statement
        .query_map([task_id.to_string()], |row| row.get::<_, String>(0))
        .map_err(PlatformError::from)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(decode(&row.map_err(PlatformError::from)?)?);
    }
    Ok(out)
}

/// 写入比较轨迹（判分证据）。
///
/// # Errors
///
/// 约束冲突或序列化失败时返回错误。
pub fn insert_comparator_trace(
    tx: &Transaction<'_>,
    grade_id: eg_domain::GradeId,
    trace: &eg_domain::ComparatorTrace,
) -> Result<()> {
    tx.execute(
        "INSERT INTO comparator_trace(id, grade_id, body) VALUES (?1, ?2, ?3)",
        params![trace.id.to_string(), grade_id.to_string(), encode(trace)?],
    )
    .map_err(PlatformError::from)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 资产元数据（文件由 `assets` 模块管理）
// ---------------------------------------------------------------------------

/// 写入资产行（去重：同 id 重复写入不报错、不覆盖字节长度）。
///
/// # Errors
///
/// 序列化/查询失败时返回错误。
pub fn insert_asset_row(
    tx: &Transaction<'_>,
    asset: AssetId,
    byte_len: u64,
    media_type: &str,
    created_at: Timestamp,
) -> Result<bool> {
    let inserted = tx
        .execute(
            "INSERT OR IGNORE INTO asset(id, byte_len, media_type, created_at) \
             VALUES (?1, ?2, ?3, ?4)",
            params![
                asset.to_hex(),
                i64::try_from(byte_len).unwrap_or(i64::MAX),
                media_type,
                timestamp_text(created_at)
            ],
        )
        .map_err(PlatformError::from)?;
    Ok(inserted > 0)
}

/// 读取资产元数据。
///
/// # Errors
///
/// 查询失败时返回错误。
pub fn get_asset_row(
    conn: &Connection,
    asset: AssetId,
) -> Result<Option<(u64, String, Timestamp)>> {
    let row: Option<(i64, String, String)> = conn
        .query_row(
            "SELECT byte_len, media_type, created_at FROM asset WHERE id = ?1",
            [asset.to_hex()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(PlatformError::from)?;
    match row {
        None => Ok(None),
        Some((byte_len, media_type, created_at)) => {
            let byte_len = u64::try_from(byte_len)
                .map_err(|_| PlatformError::DatabaseCorrupt("资产字节数非法".to_owned()))?;
            Ok(Some((
                byte_len,
                media_type,
                parse_timestamp(&created_at, "asset.created_at")?,
            )))
        }
    }
}

/// 关联资产到所有者角色（`owner_type`/`owner_id`/`role` 唯一）。
///
/// # Errors
///
/// 约束冲突（资产不存在）时返回错误。
pub fn link_asset(
    tx: &Transaction<'_>,
    owner_type: &str,
    owner_id: &str,
    role: &str,
    asset: AssetId,
    created_at: Timestamp,
) -> Result<()> {
    tx.execute(
        "INSERT INTO asset_link(owner_type, owner_id, role, asset_id, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            owner_type,
            owner_id,
            role,
            asset.to_hex(),
            timestamp_text(created_at)
        ],
    )
    .map_err(PlatformError::from)?;
    Ok(())
}

/// 统计资产占用（行数与字节总数，`assets` 模块软配额告警使用）。
///
/// # Errors
///
/// 查询失败时返回错误。
pub fn asset_usage(conn: &Connection) -> Result<(u64, u64)> {
    let (count, bytes): (i64, i64) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(byte_len), 0) FROM asset",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(PlatformError::from)?;
    Ok((
        u64::try_from(count).unwrap_or(0),
        u64::try_from(bytes).unwrap_or(0),
    ))
}

/// 读取资产行（供 `assets` 模块校验引用完整性）。
///
/// # Errors
///
/// 查询失败时返回错误。
pub fn asset_row_exists(conn: &Connection, asset: AssetId) -> Result<bool> {
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM asset WHERE id = ?1)",
            [asset.to_hex()],
            |row| row.get(0),
        )
        .map_err(PlatformError::from)?;
    Ok(exists)
}

// ---------------------------------------------------------------------------
// v3（task-42）：复核队列 / 撤销栈 / 导出记录 / 提交级排除 /
//               判分幂等与断点 / 复核原因 / 证据摘要
// ---------------------------------------------------------------------------

/// 复核队列默认页大小。
pub const DEFAULT_QUEUE_PAGE_SIZE: usize = 100;

/// 复核队列单页上限。
pub const MAX_QUEUE_PAGE_SIZE: usize = 1000;

/// 复核队列过滤（只返回每个「提交 × 题」的最新修订）。
///
/// 空过滤 = 全部最新修订；各字段之间为「与」关系。原因为判分记录的多值属性
/// （`grade_review_reason` 表），由判分/复核适配层在同一事务内写入。
#[derive(Debug, Clone, Default)]
pub struct GradeQueueFilter {
    /// 模式白名单（空 = 不限制）。
    pub modes: Vec<GradeMode>,
    /// 状态白名单（空 = 不限制）。
    pub statuses: Vec<GradeStatus>,
    /// 抽检标记（`Some(true)` = 仅抽检；`Some(false)` = 非抽检）。
    pub sampled: Option<bool>,
    /// 复核原因白名单（空 = 不限制；命中任一即返回）。
    pub reasons: Vec<ReviewReasonKind>,
    /// 学生 id（经提交表关联）。
    pub student_id: Option<StudentId>,
    /// 题目 id。
    pub question_id: Option<QuestionId>,
    /// 提交 id。
    pub submission_id: Option<SubmissionId>,
    /// 试卷 id（经提交表关联）。
    pub paper_id: Option<PaperId>,
    /// 班级 id（经提交表关联）。
    pub classroom_id: Option<ClassroomId>,
    /// 页大小（0 = 默认；超过 [`MAX_QUEUE_PAGE_SIZE`] 截断）。
    pub limit: usize,
}

impl GradeQueueFilter {
    fn effective_limit(&self) -> usize {
        if self.limit == 0 {
            DEFAULT_QUEUE_PAGE_SIZE
        } else {
            self.limit.min(MAX_QUEUE_PAGE_SIZE)
        }
    }
}

/// 队列游标（稳定排序键：`created_at` + `id`，升序）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GradeCursor {
    /// 上一页最后一条记录的创建时间。
    pub created_at: Timestamp,
    /// 上一页最后一条记录 id（同一时间的次级排序键）。
    pub id: GradeId,
}

/// 判分记录幂等键（重启后重跑复用；键唯一）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GradeIdempotency {
    /// 幂等键（提交+题+版本+识别哈希+引擎指纹+模式；由 grade 生成）。
    pub key: String,
    /// 提交 id。
    pub submission_id: SubmissionId,
    /// 题目 id。
    pub question_id: QuestionId,
    /// 模式。
    pub mode: GradeMode,
    /// 已有判分记录 id（首次写入时可能为 None）。
    pub grade_id: Option<GradeId>,
    /// 结果摘要（供重跑核对，不应含明文学生作答）。
    pub result_summary: serde_json::Value,
    /// 写入时间。
    pub created_at: Timestamp,
}

/// 判分断点状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GradeProgressState {
    /// 待开始。
    Pending,
    /// 进行中（崩溃后需要归位/重试）。
    Running,
    /// 已完成。
    Done,
    /// 已失败（可重试）。
    Failed,
}

impl GradeProgressState {
    /// 落库文本。
    #[must_use]
    pub const fn as_text(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    /// 解析落库文本。
    ///
    /// # Errors
    ///
    /// 未知状态返回「数据损坏」。
    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "done" => Ok(Self::Done),
            "failed" => Ok(Self::Failed),
            other => Err(PlatformError::DatabaseCorrupt(format!(
                "未知判分断点状态：{other}"
            ))),
        }
    }
}

/// 判分断点（提交级 `question_id = None`；题级带题目 id）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GradeProgress {
    /// 提交 id。
    pub submission_id: SubmissionId,
    /// 题目 id（`None` = 提交级断点）。
    pub question_id: Option<QuestionId>,
    /// 当前状态。
    pub state: GradeProgressState,
    /// 已尝试次数。
    pub attempts: u32,
    /// 引擎指纹（CAS/OCR/判定等版本；便于判断缓存/幂等是否仍有效）。
    pub engine_fingerprint: Option<EngineStamp>,
    /// 附加诊断负载（可空；调用方定义）。
    pub detail: Option<serde_json::Value>,
    /// 更新时间。
    pub updated_at: Timestamp,
}

/// 撤销栈条目（复核命令反向操作；只由命令事务写入）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UndoEntry {
    /// 条目 id。
    pub id: Ulid,
    /// 命令幂等键（与审计条目对应）。
    pub command_id: String,
    /// 动作（如 `review.accept`）。
    pub action: String,
    /// 实体类型。
    pub entity_type: String,
    /// 实体 id。
    pub entity_id: String,
    /// 反向操作负载（由 review 定义；平台不解释）。
    pub payload: serde_json::Value,
    /// 入栈时间。
    pub created_at: Timestamp,
}

/// 导出记录（审计 + 摘要 + 重放比较）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExportRecord {
    /// 导出记录 id（ULID）。
    pub id: Ulid,
    /// 导出类型（如 `paper_report`/`student_report`）。
    pub kind: String,
    /// 格式（`xlsx`/`csv`/`html`）。
    pub format: String,
    /// 导出范围参数（调用方定义；不含 PII 明文）。
    pub scope: serde_json::Value,
    /// 内容摘要哈希（重放一致性比较）。
    pub summary_hash: Sha256Hex,
    /// 行数（学生/题目数量等）。
    pub row_count: u64,
    /// 结果摘要/重放负载（调用方定义；不含 PII 明文）。
    pub body: serde_json::Value,
    /// 创建时间。
    pub created_at: Timestamp,
    /// 对应审计条目 id（可空）。
    pub audit_id: Option<Ulid>,
}

/// 资产摘要（按判分记录取证据用）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AssetSummary {
    /// 资产 id。
    pub asset_id: AssetId,
    /// 所属页号（派生资产记所属页）。
    pub page_no: u32,
    /// 资产角色（`page` = 原图；`derived` = 派生物）。
    pub role: String,
    /// MIME 类型。
    pub media_type: String,
    /// 字节数。
    pub byte_len: u64,
}

/// 识别摘要（按判分记录取证据用）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RecognitionSummary {
    /// 识别结果 id。
    pub recognition_id: RecognitionId,
    /// 所属页号。
    pub page_no: u32,
    /// 关联区域 id（可空）。
    pub segment_id: Option<SegmentId>,
    /// 置信度。
    pub confidence: f64,
    /// 引擎指纹。
    pub engine: EngineStamp,
    /// 识别文本（供复核详情展示；调用方负责脱敏展示策略）。
    pub text: String,
}

/// 判分记录证据摘要（只读组合查询）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GradeEvidenceSummary {
    /// 判分记录 id。
    pub grade_id: GradeId,
    /// 提交 id。
    pub submission_id: SubmissionId,
    /// 题目 id。
    pub question_id: QuestionId,
    /// 证据资产（原图 + 派生物）。
    pub assets: Vec<AssetSummary>,
    /// 该题的识别结果。
    pub recognitions: Vec<RecognitionSummary>,
}

fn grade_mode_json(mode: GradeMode) -> Result<String> {
    serde_json::to_string(&mode).map_err(|error| PlatformError::Serialize(error.to_string()))
}

fn grade_status_json(status: GradeStatus) -> Result<String> {
    serde_json::to_string(&status).map_err(|error| PlatformError::Serialize(error.to_string()))
}

/// 复核原因的稳定落库文本（与 `serde_json` 的 snake_case 一致）。
#[must_use]
pub const fn review_reason_code(reason: ReviewReasonKind) -> &'static str {
    match reason {
        ReviewReasonKind::LowConfidence => "low_confidence",
        ReviewReasonKind::Conflict => "conflict",
        ReviewReasonKind::Ungradable => "ungradable",
        ReviewReasonKind::MissingEvidence => "missing_evidence",
        ReviewReasonKind::UncalibratedModel => "uncalibrated_model",
        ReviewReasonKind::BackendUnavailable => "backend_unavailable",
        ReviewReasonKind::InvalidScore => "invalid_score",
    }
}

fn json_text(value: &serde_json::Value) -> Result<String> {
    serde_json::to_string(value).map_err(|error| PlatformError::Serialize(error.to_string()))
}

fn json_value(text: &str, column: &str) -> Result<serde_json::Value> {
    serde_json::from_str(text).map_err(|error| {
        PlatformError::DatabaseCorrupt(format!("列 {column} 的 JSON 无法解析：{error}"))
    })
}

/// 构造队列 SQL（含最新修订窗口 + 过滤 + 游标；`count = true` 时只取总数）。
fn build_queue_query(
    filter: &GradeQueueFilter,
    after: Option<GradeCursor>,
    count: bool,
) -> Result<(String, Vec<rusqlite::types::Value>)> {
    use rusqlite::types::Value as SqlValue;
    use std::fmt::Write as _;
    let mut values: Vec<SqlValue> = Vec::new();
    let mut conditions = String::from(" WHERE l.rn = 1");
    if !filter.modes.is_empty() {
        let marks = vec!["?"; filter.modes.len()].join(", ");
        let _ = write!(conditions, " AND l.mode IN ({marks})");
        for mode in &filter.modes {
            values.push(SqlValue::Text(grade_mode_json(*mode)?));
        }
    }
    if !filter.statuses.is_empty() {
        let marks = vec!["?"; filter.statuses.len()].join(", ");
        let _ = write!(conditions, " AND l.status IN ({marks})");
        for status in &filter.statuses {
            values.push(SqlValue::Text(grade_status_json(*status)?));
        }
    }
    if let Some(sampled) = filter.sampled {
        conditions.push_str(" AND l.sampled = ?");
        values.push(SqlValue::Integer(i64::from(sampled)));
    }
    if !filter.reasons.is_empty() {
        let marks = vec!["?"; filter.reasons.len()].join(", ");
        let _ = write!(
            conditions,
            " AND EXISTS (SELECT 1 FROM grade_review_reason r \
             WHERE r.grade_id = l.id AND r.reason_code IN ({marks}))"
        );
        for reason in &filter.reasons {
            values.push(SqlValue::Text(review_reason_code(*reason).to_owned()));
        }
    }
    if let Some(student_id) = filter.student_id {
        conditions.push_str(" AND s.student_id = ?");
        values.push(SqlValue::Text(student_id.to_string()));
    }
    if let Some(question_id) = filter.question_id {
        conditions.push_str(" AND l.question_id = ?");
        values.push(SqlValue::Text(question_id.to_string()));
    }
    if let Some(submission_id) = filter.submission_id {
        conditions.push_str(" AND l.submission_id = ?");
        values.push(SqlValue::Text(submission_id.to_string()));
    }
    if let Some(paper_id) = filter.paper_id {
        conditions.push_str(" AND s.paper_id = ?");
        values.push(SqlValue::Text(paper_id.to_string()));
    }
    if let Some(classroom_id) = filter.classroom_id {
        conditions.push_str(" AND s.classroom_id = ?");
        values.push(SqlValue::Text(classroom_id.to_string()));
    }
    if let Some(cursor) = after {
        conditions.push_str(" AND (l.created_at > ? OR (l.created_at = ? AND l.id > ?))");
        let created = timestamp_text(cursor.created_at);
        values.push(SqlValue::Text(created.clone()));
        values.push(SqlValue::Text(created));
        values.push(SqlValue::Text(cursor.id.to_string()));
    }
    let head = if count {
        "WITH latest AS (SELECT id, submission_id, question_id, mode, sampled, status, \
         created_at, ROW_NUMBER() OVER (PARTITION BY submission_id, question_id \
         ORDER BY revision DESC) AS rn FROM grade_record) \
         SELECT COUNT(*) FROM latest l JOIN submission s ON s.id = l.submission_id"
    } else {
        "WITH latest AS (SELECT id, submission_id, question_id, mode, sampled, status, \
         created_at, body, ROW_NUMBER() OVER (PARTITION BY submission_id, question_id \
         ORDER BY revision DESC) AS rn FROM grade_record) \
         SELECT l.body FROM latest l JOIN submission s ON s.id = l.submission_id"
    };
    let mut sql = format!("{head}{conditions}");
    if !count {
        sql.push_str(" ORDER BY l.created_at ASC, l.id ASC LIMIT ?");
        values.push(SqlValue::Integer(
            i64::try_from(filter.effective_limit()).unwrap_or(i64::MAX),
        ));
    }
    Ok((sql, values))
}

/// 跨提交复核队列：每个「提交 × 题」只返回最新修订，按 `(created_at, id)` 升序。
///
/// 过滤字段见 [`GradeQueueFilter`]；分页用 [`GradeCursor`]（上一页最后一条），
/// 稳定排序键保证不重复；分页期间新修订可能改变排序键（调用方应在 UI 层刷新快照）。
///
/// # Errors
///
/// 查询或反序列化失败时返回错误。
pub fn list_grade_records_for_queue(
    conn: &Connection,
    filter: &GradeQueueFilter,
    after: Option<GradeCursor>,
) -> Result<Vec<GradeRecord>> {
    let (sql, values) = build_queue_query(filter, after, false)?;
    let mut statement = conn.prepare(&sql).map_err(PlatformError::from)?;
    let rows = statement
        .query_map(rusqlite::params_from_iter(values), |row| {
            row.get::<_, String>(0)
        })
        .map_err(PlatformError::from)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(decode(&row.map_err(PlatformError::from)?)?);
    }
    Ok(out)
}

/// 复核队列总数（与 [`list_grade_records_for_queue`] 同一过滤条件）。
///
/// # Errors
///
/// 查询失败时返回错误。
pub fn count_grade_records_for_queue(conn: &Connection, filter: &GradeQueueFilter) -> Result<u64> {
    let (sql, values) = build_queue_query(filter, None, true)?;
    let count: i64 = conn
        .query_row(&sql, rusqlite::params_from_iter(values), |row| row.get(0))
        .map_err(PlatformError::from)?;
    Ok(u64::try_from(count).unwrap_or(0))
}

/// 按 id 读取判分记录。
///
/// # Errors
///
/// 查询或反序列化失败时返回错误。
pub fn get_grade_record(conn: &Connection, id: GradeId) -> Result<Option<GradeRecord>> {
    let body: Option<String> = conn
        .query_row(
            "SELECT body FROM grade_record WHERE id = ?1",
            [id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(PlatformError::from)?;
    body.map(|text| decode(&text)).transpose()
}

/// 读取某提交某题的最新修订。
///
/// # Errors
///
/// 查询或反序列化失败时返回错误。
pub fn get_latest_grade_record(
    conn: &Connection,
    submission_id: SubmissionId,
    question_id: QuestionId,
) -> Result<Option<GradeRecord>> {
    let body: Option<String> = conn
        .query_row(
            "SELECT body FROM grade_record WHERE submission_id = ?1 AND question_id = ?2 \
             ORDER BY revision DESC LIMIT 1",
            params![submission_id.to_string(), question_id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(PlatformError::from)?;
    body.map(|text| decode(&text)).transpose()
}

/// 写入判分记录的复核原因（多值；同一事务内与记录/审计一起写）。
///
/// # Errors
///
/// 约束冲突时返回错误（重复原因按幂等忽略）。
pub fn insert_grade_review_reasons(
    tx: &Transaction<'_>,
    grade_id: GradeId,
    reasons: &[ReviewReasonKind],
) -> Result<()> {
    for reason in reasons {
        tx.execute(
            "INSERT INTO grade_review_reason(grade_id, reason_code) VALUES (?1, ?2) \
             ON CONFLICT(grade_id, reason_code) DO NOTHING",
            params![grade_id.to_string(), review_reason_code(*reason)],
        )
        .map_err(PlatformError::from)?;
    }
    Ok(())
}

/// 追加判分记录并写入复核原因（同一事务；供 grade 落库使用）。
///
/// # Errors
///
/// 同 [`append_grade_record`]；原因写入失败时整体回滚（由调用方事务保证）。
pub fn append_grade_record_with_reasons(
    tx: &Transaction<'_>,
    record: &GradeRecord,
    reasons: &[ReviewReasonKind],
) -> Result<()> {
    append_grade_record(tx, record)?;
    insert_grade_review_reasons(tx, record.id, reasons)
}

/// 读取某提交的提交级排除（可空）。
///
/// # Errors
///
/// 查询或反序列化失败时返回错误。
pub fn get_submission_exclusion(
    conn: &Connection,
    submission_id: SubmissionId,
) -> Result<Option<SubmissionExclusion>> {
    let body: Option<String> = conn
        .query_row(
            "SELECT body FROM submission_exclusion WHERE submission_id = ?1",
            [submission_id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(PlatformError::from)?;
    body.map(|text| decode(&text)).transpose()
}

/// 写入/覆盖某提交的提交级排除（同一提交只保留一条；事务由调用方保证）。
///
/// # Errors
///
/// 外键/序列化失败时返回错误。
pub fn upsert_submission_exclusion(
    tx: &Transaction<'_>,
    exclusion: &SubmissionExclusion,
) -> Result<()> {
    let reason = serde_json::to_string(&exclusion.reason)
        .map_err(|error| PlatformError::Serialize(error.to_string()))?;
    tx.execute(
        "INSERT INTO submission_exclusion(submission_id, reason, note, at, body) \
         VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT(submission_id) DO UPDATE SET reason = excluded.reason, \
         note = excluded.note, at = excluded.at, body = excluded.body",
        params![
            exclusion.submission_id.to_string(),
            reason,
            exclusion.note,
            timestamp_text(exclusion.at),
            encode(exclusion)?
        ],
    )
    .map_err(PlatformError::from)?;
    Ok(())
}

/// 删除提交级排除（撤销排除；返回是否确有删除）。
///
/// # Errors
///
/// 删除失败时返回错误。
pub fn delete_submission_exclusion(
    tx: &Transaction<'_>,
    submission_id: SubmissionId,
) -> Result<bool> {
    let changed = tx
        .execute(
            "DELETE FROM submission_exclusion WHERE submission_id = ?1",
            [submission_id.to_string()],
        )
        .map_err(PlatformError::from)?;
    Ok(changed > 0)
}

/// 读取判分幂等记录。
///
/// # Errors
///
/// 查询或解析失败时返回错误。
pub fn get_grade_idempotency(conn: &Connection, key: &str) -> Result<Option<GradeIdempotency>> {
    conn.query_row(
        "SELECT submission_id, question_id, mode, grade_id, result_summary, created_at \
         FROM grade_idempotency WHERE key = ?1",
        [key],
        |row| {
            let submission: String = row.get(0)?;
            let question: String = row.get(1)?;
            let mode: String = row.get(2)?;
            let grade_id: Option<String> = row.get(3)?;
            let summary: String = row.get(4)?;
            let created: String = row.get(5)?;
            Ok((submission, question, mode, grade_id, summary, created))
        },
    )
    .optional()
    .map_err(PlatformError::from)?
    .map(|(submission, question, mode, grade_id, summary, created)| {
        Ok(GradeIdempotency {
            key: key.to_owned(),
            submission_id: SubmissionId::parse(&submission).map_err(|error| {
                PlatformError::DatabaseCorrupt(format!("idempotency.submission_id：{error}"))
            })?,
            question_id: QuestionId::parse(&question).map_err(|error| {
                PlatformError::DatabaseCorrupt(format!("idempotency.question_id：{error}"))
            })?,
            mode: serde_json::from_str(&mode).map_err(|error| {
                PlatformError::DatabaseCorrupt(format!("idempotency.mode：{error}"))
            })?,
            grade_id: grade_id
                .map(|text| {
                    GradeId::parse(&text).map_err(|error| {
                        PlatformError::DatabaseCorrupt(format!("idempotency.grade_id：{error}"))
                    })
                })
                .transpose()?,
            result_summary: json_value(&summary, "grade_idempotency.result_summary")?,
            created_at: parse_timestamp(&created, "grade_idempotency.created_at")?,
        })
    })
    .transpose()
}

/// 写入判分幂等记录（键已存在时不覆盖，返回 `false`）。
///
/// 语义：幂等键是「这次判分已发生」的凭据；已存在时调用方应复用其
/// `grade_id`/`result_summary`，不要重复调用模型或重复写判分记录。
///
/// # Errors
///
/// 序列化/约束失败时返回错误。
pub fn put_grade_idempotency(tx: &Transaction<'_>, record: &GradeIdempotency) -> Result<bool> {
    let mode = grade_mode_json(record.mode)?;
    let summary = json_text(&record.result_summary)?;
    let changed = tx
        .execute(
            "INSERT INTO grade_idempotency(key, submission_id, question_id, mode, grade_id, \
             result_summary, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
             ON CONFLICT(key) DO NOTHING",
            params![
                record.key,
                record.submission_id.to_string(),
                record.question_id.to_string(),
                mode,
                record.grade_id.map(|id| id.to_string()),
                summary,
                timestamp_text(record.created_at)
            ],
        )
        .map_err(PlatformError::from)?;
    Ok(changed > 0)
}

/// 写入/更新判分断点（upsert；同一提交同一题目范围只保留一条）。
///
/// # Errors
///
/// 序列化/约束失败时返回错误。
pub fn put_grade_progress(tx: &Transaction<'_>, progress: &GradeProgress) -> Result<()> {
    let question_scope = progress
        .question_id
        .map_or_else(String::new, |id| id.to_string());
    let fingerprint = progress
        .engine_fingerprint
        .as_ref()
        .map(|stamp| {
            serde_json::to_string(stamp)
                .map_err(|error| PlatformError::Serialize(error.to_string()))
        })
        .transpose()?;
    let detail = progress.detail.as_ref().map(json_text).transpose()?;
    tx.execute(
        "INSERT INTO grade_progress(submission_id, question_scope, state, attempts, \
         engine_fingerprint, detail, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
         ON CONFLICT(submission_id, question_scope) DO UPDATE SET state = excluded.state, \
         attempts = excluded.attempts, engine_fingerprint = excluded.engine_fingerprint, \
         detail = excluded.detail, updated_at = excluded.updated_at",
        params![
            progress.submission_id.to_string(),
            question_scope,
            progress.state.as_text(),
            progress.attempts,
            fingerprint,
            detail,
            timestamp_text(progress.updated_at)
        ],
    )
    .map_err(PlatformError::from)?;
    Ok(())
}

type ProgressRow = (
    String,
    String,
    String,
    u32,
    Option<String>,
    Option<String>,
    String,
);

fn row_to_progress(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProgressRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
    ))
}

fn decode_progress(
    submission: String,
    scope: String,
    state: String,
    attempts: u32,
    fingerprint: Option<String>,
    detail: Option<String>,
    updated: String,
) -> Result<GradeProgress> {
    Ok(GradeProgress {
        submission_id: SubmissionId::parse(&submission).map_err(|error| {
            PlatformError::DatabaseCorrupt(format!("progress.submission_id：{error}"))
        })?,
        question_id: if scope.is_empty() {
            None
        } else {
            Some(QuestionId::parse(&scope).map_err(|error| {
                PlatformError::DatabaseCorrupt(format!("progress.question_scope：{error}"))
            })?)
        },
        state: GradeProgressState::parse(&state)?,
        attempts,
        engine_fingerprint: fingerprint
            .map(|text| {
                serde_json::from_str::<EngineStamp>(&text).map_err(|error| {
                    PlatformError::DatabaseCorrupt(format!("progress.engine_fingerprint：{error}"))
                })
            })
            .transpose()?,
        detail: detail
            .map(|text| json_value(&text, "grade_progress.detail"))
            .transpose()?,
        updated_at: parse_timestamp(&updated, "grade_progress.updated_at")?,
    })
}

/// 读取判分断点（`question_id = None` 表示提交级）。
///
/// # Errors
///
/// 查询或解析失败时返回错误。
pub fn get_grade_progress(
    conn: &Connection,
    submission_id: SubmissionId,
    question_id: Option<QuestionId>,
) -> Result<Option<GradeProgress>> {
    let scope = question_id.map_or_else(String::new, |id| id.to_string());
    let row = conn
        .query_row(
            "SELECT submission_id, question_scope, state, attempts, engine_fingerprint, \
             detail, updated_at FROM grade_progress \
             WHERE submission_id = ?1 AND question_scope = ?2",
            params![submission_id.to_string(), scope],
            row_to_progress,
        )
        .optional()
        .map_err(PlatformError::from)?;
    row.map(
        |(submission, scope, state, attempts, fingerprint, detail, updated)| {
            decode_progress(
                submission,
                scope,
                state,
                attempts,
                fingerprint,
                detail,
                updated,
            )
        },
    )
    .transpose()
}

/// 列出某提交的全部断点（提交级在前，题级按题目 id 排序）。
///
/// # Errors
///
/// 查询或解析失败时返回错误。
pub fn list_grade_progress(
    conn: &Connection,
    submission_id: SubmissionId,
) -> Result<Vec<GradeProgress>> {
    let mut statement = conn
        .prepare(
            "SELECT submission_id, question_scope, state, attempts, engine_fingerprint, \
             detail, updated_at FROM grade_progress WHERE submission_id = ?1 \
             ORDER BY (question_scope = '') DESC, question_scope",
        )
        .map_err(PlatformError::from)?;
    let rows = statement
        .query_map([submission_id.to_string()], row_to_progress)
        .map_err(PlatformError::from)?;
    let mut out = Vec::new();
    for row in rows {
        let (submission, scope, state, attempts, fingerprint, detail, updated) =
            row.map_err(PlatformError::from)?;
        out.push(decode_progress(
            submission,
            scope,
            state,
            attempts,
            fingerprint,
            detail,
            updated,
        )?);
    }
    Ok(out)
}

/// 列出「未完成」断点（`state != done`；崩溃恢复/重试扫描用），按更新时间升序。
///
/// # Errors
///
/// 查询或解析失败时返回错误。
pub fn list_incomplete_grade_progress(
    conn: &Connection,
    limit: usize,
) -> Result<Vec<GradeProgress>> {
    let limit = if limit == 0 {
        DEFAULT_QUEUE_PAGE_SIZE
    } else {
        limit
    };
    let mut statement = conn
        .prepare(
            "SELECT submission_id, question_scope, state, attempts, engine_fingerprint, \
             detail, updated_at FROM grade_progress WHERE state <> 'done' \
             ORDER BY updated_at ASC, submission_id ASC, question_scope ASC LIMIT ?1",
        )
        .map_err(PlatformError::from)?;
    let rows = statement
        .query_map([i64::try_from(limit).unwrap_or(i64::MAX)], row_to_progress)
        .map_err(PlatformError::from)?;
    let mut out = Vec::new();
    for row in rows {
        let (submission, scope, state, attempts, fingerprint, detail, updated) =
            row.map_err(PlatformError::from)?;
        out.push(decode_progress(
            submission,
            scope,
            state,
            attempts,
            fingerprint,
            detail,
            updated,
        )?);
    }
    Ok(out)
}

/// 撤销栈入栈（命令事务内调用）。
///
/// # Errors
///
/// 序列化/约束失败时返回错误。
pub fn push_undo(tx: &Transaction<'_>, entry: &UndoEntry) -> Result<()> {
    tx.execute(
        "INSERT INTO review_undo(id, command_id, action, entity_type, entity_id, payload, \
         created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            entry.id.to_string(),
            entry.command_id,
            entry.action,
            entry.entity_type,
            entry.entity_id,
            json_text(&entry.payload)?,
            timestamp_text(entry.created_at)
        ],
    )
    .map_err(PlatformError::from)?;
    Ok(())
}

type UndoRow = (String, String, String, String, String, String, String);

fn row_to_undo(row: &rusqlite::Row<'_>) -> rusqlite::Result<UndoRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
    ))
}

fn decode_undo(
    id: String,
    command_id: String,
    action: String,
    entity_type: String,
    entity_id: String,
    payload: String,
    created: String,
) -> Result<UndoEntry> {
    Ok(UndoEntry {
        id: Ulid::parse(&id)
            .map_err(|error| PlatformError::DatabaseCorrupt(format!("review_undo.id：{error}")))?,
        command_id,
        action,
        entity_type,
        entity_id,
        payload: json_value(&payload, "review_undo.payload")?,
        created_at: parse_timestamp(&created, "review_undo.created_at")?,
    })
}

/// 弹出最近一条撤销栈条目（LIFO：按 `(created_at, id)` 降序），并删除。
///
/// # Errors
///
/// 查询/删除/解析失败时返回错误。
pub fn pop_undo(tx: &Transaction<'_>) -> Result<Option<UndoEntry>> {
    let row = tx
        .query_row(
            "SELECT id, command_id, action, entity_type, entity_id, payload, created_at \
             FROM review_undo ORDER BY created_at DESC, id DESC LIMIT 1",
            [],
            row_to_undo,
        )
        .optional()
        .map_err(PlatformError::from)?;
    let Some((id, command_id, action, entity_type, entity_id, payload, created)) = row else {
        return Ok(None);
    };
    let entry = decode_undo(
        id,
        command_id,
        action,
        entity_type,
        entity_id,
        payload,
        created,
    )?;
    tx.execute(
        "DELETE FROM review_undo WHERE id = ?1",
        [entry.id.to_string()],
    )
    .map_err(PlatformError::from)?;
    Ok(Some(entry))
}

/// 列出撤销栈（最新在前；最多 `limit` 条，0 = 默认 50）。
///
/// # Errors
///
/// 查询或解析失败时返回错误。
pub fn list_undo(conn: &Connection, limit: usize) -> Result<Vec<UndoEntry>> {
    let limit = if limit == 0 { 50 } else { limit };
    let mut statement = conn
        .prepare(
            "SELECT id, command_id, action, entity_type, entity_id, payload, created_at \
             FROM review_undo ORDER BY created_at DESC, id DESC LIMIT ?1",
        )
        .map_err(PlatformError::from)?;
    let rows = statement
        .query_map([i64::try_from(limit).unwrap_or(i64::MAX)], row_to_undo)
        .map_err(PlatformError::from)?;
    let mut out = Vec::new();
    for row in rows {
        let (id, command_id, action, entity_type, entity_id, payload, created) =
            row.map_err(PlatformError::from)?;
        out.push(decode_undo(
            id,
            command_id,
            action,
            entity_type,
            entity_id,
            payload,
            created,
        )?);
    }
    Ok(out)
}

/// 写入导出记录（导出成功后调用；事务由调用方保证）。
///
/// # Errors
///
/// 序列化/约束失败时返回错误。
pub fn insert_export_record(tx: &Transaction<'_>, record: &ExportRecord) -> Result<()> {
    tx.execute(
        "INSERT INTO export_record(id, kind, format, scope, summary_hash, row_count, body, \
         created_at, audit_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            record.id.to_string(),
            record.kind,
            record.format,
            json_text(&record.scope)?,
            record.summary_hash.to_hex(),
            i64::try_from(record.row_count).unwrap_or(i64::MAX),
            json_text(&record.body)?,
            timestamp_text(record.created_at),
            record.audit_id.map(|id| id.to_string())
        ],
    )
    .map_err(PlatformError::from)?;
    Ok(())
}

struct ExportRowData {
    id: String,
    kind: String,
    format: String,
    scope: String,
    summary: String,
    row_count: i64,
    body: String,
    created: String,
    audit: Option<String>,
}

fn row_to_export(row: &rusqlite::Row<'_>) -> rusqlite::Result<ExportRowData> {
    Ok(ExportRowData {
        id: row.get(0)?,
        kind: row.get(1)?,
        format: row.get(2)?,
        scope: row.get(3)?,
        summary: row.get(4)?,
        row_count: row.get(5)?,
        body: row.get(6)?,
        created: row.get(7)?,
        audit: row.get(8)?,
    })
}

fn decode_export(row: ExportRowData) -> Result<ExportRecord> {
    Ok(ExportRecord {
        id: Ulid::parse(&row.id).map_err(|error| {
            PlatformError::DatabaseCorrupt(format!("export_record.id：{error}"))
        })?,
        kind: row.kind,
        format: row.format,
        scope: json_value(&row.scope, "export_record.scope")?,
        summary_hash: Sha256Hex::from_hex(&row.summary).map_err(|error| {
            PlatformError::DatabaseCorrupt(format!("export_record.summary_hash：{error}"))
        })?,
        row_count: u64::try_from(row.row_count).unwrap_or(0),
        body: json_value(&row.body, "export_record.body")?,
        created_at: parse_timestamp(&row.created, "export_record.created_at")?,
        audit_id: row
            .audit
            .map(|text| {
                Ulid::parse(&text).map_err(|error| {
                    PlatformError::DatabaseCorrupt(format!("export_record.audit_id：{error}"))
                })
            })
            .transpose()?,
    })
}

/// 按 id 读取导出记录。
///
/// # Errors
///
/// 查询或解析失败时返回错误。
pub fn get_export_record(conn: &Connection, id: Ulid) -> Result<Option<ExportRecord>> {
    let row = conn
        .query_row(
            "SELECT id, kind, format, scope, summary_hash, row_count, body, created_at, \
             audit_id FROM export_record WHERE id = ?1",
            [id.to_string()],
            row_to_export,
        )
        .optional()
        .map_err(PlatformError::from)?;
    row.map(decode_export).transpose()
}

/// 列出导出记录（最新在前；`limit` 为 0 时默认 100）。
///
/// # Errors
///
/// 查询或解析失败时返回错误。
pub fn list_export_records(conn: &Connection, limit: usize) -> Result<Vec<ExportRecord>> {
    let limit = if limit == 0 { 100 } else { limit };
    let mut statement = conn
        .prepare(
            "SELECT id, kind, format, scope, summary_hash, row_count, body, created_at, \
             audit_id FROM export_record ORDER BY created_at DESC, id DESC LIMIT ?1",
        )
        .map_err(PlatformError::from)?;
    let rows = statement
        .query_map([i64::try_from(limit).unwrap_or(i64::MAX)], row_to_export)
        .map_err(PlatformError::from)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(decode_export(row.map_err(PlatformError::from)?)?);
    }
    Ok(out)
}

/// 按摘要哈希查导出记录（重放一致性比较：新导出摘要命中即证明内容一致）。
///
/// # Errors
///
/// 查询或解析失败时返回错误。
pub fn find_export_records_by_summary(
    conn: &Connection,
    summary_hash: Sha256Hex,
) -> Result<Vec<ExportRecord>> {
    let mut statement = conn
        .prepare(
            "SELECT id, kind, format, scope, summary_hash, row_count, body, created_at, \
             audit_id FROM export_record WHERE summary_hash = ?1 \
             ORDER BY created_at DESC, id DESC",
        )
        .map_err(PlatformError::from)?;
    let rows = statement
        .query_map([summary_hash.to_hex()], row_to_export)
        .map_err(PlatformError::from)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(decode_export(row.map_err(PlatformError::from)?)?);
    }
    Ok(out)
}

/// 按判分记录取证据摘要（原图 + 派生物 + 该题识别结果）。
///
/// # Errors
///
/// 判分记录不存在返回 `Ok(None)`；查询/解析失败返回错误。
pub fn load_grade_evidence_summary(
    conn: &Connection,
    grade_id: GradeId,
) -> Result<Option<GradeEvidenceSummary>> {
    let Some(record) = get_grade_record(conn, grade_id)? else {
        return Ok(None);
    };
    let mut assets: Vec<AssetSummary> = Vec::new();
    let mut pages: Vec<(u32, String)> = Vec::new();
    {
        let mut statement = conn
            .prepare(
                "SELECT page_no, asset, body FROM page WHERE submission_id = ?1 ORDER BY page_no",
            )
            .map_err(PlatformError::from)?;
        let rows = statement
            .query_map([record.submission_id.to_string()], |row| {
                Ok((
                    row.get::<_, u32>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(PlatformError::from)?;
        for row in rows {
            let (page_no, asset, body) = row.map_err(PlatformError::from)?;
            pages.push((page_no, asset));
            let page: Page = decode(&body)?;
            for derived in &page.derived {
                assets.push(asset_summary(conn, *derived, page_no, "derived")?);
            }
        }
    }
    for (page_no, asset) in pages {
        let asset_id = AssetId::from_hex(&asset)
            .map_err(|error| PlatformError::DatabaseCorrupt(format!("page.asset：{error}")))?;
        assets.push(asset_summary(conn, asset_id, page_no, "page")?);
    }

    let mut recognitions = Vec::new();
    {
        let mut statement = conn
            .prepare(
                "SELECT r.id, p.page_no, r.segment_id, r.confidence, r.body \
                 FROM recognition r JOIN segment sg ON sg.id = r.segment_id \
                 JOIN page p ON p.id = r.page_id \
                 WHERE sg.question_id = ?1 ORDER BY p.page_no, r.id",
            )
            .map_err(PlatformError::from)?;
        let rows = statement
            .query_map([record.question_id.to_string()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u32>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, f64>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .map_err(PlatformError::from)?;
        for row in rows {
            let (id, page_no, segment_id, confidence, body) = row.map_err(PlatformError::from)?;
            let recognition: Recognition = decode(&body)?;
            recognitions.push(RecognitionSummary {
                recognition_id: RecognitionId::parse(&id).map_err(|error| {
                    PlatformError::DatabaseCorrupt(format!("recognition.id：{error}"))
                })?,
                page_no,
                segment_id: segment_id
                    .map(|text| {
                        SegmentId::parse(&text).map_err(|error| {
                            PlatformError::DatabaseCorrupt(format!(
                                "recognition.segment_id：{error}"
                            ))
                        })
                    })
                    .transpose()?,
                confidence,
                engine: recognition.engine,
                text: recognition.text,
            });
        }
    }
    Ok(Some(GradeEvidenceSummary {
        grade_id: record.id,
        submission_id: record.submission_id,
        question_id: record.question_id,
        assets,
        recognitions,
    }))
}

fn asset_summary(
    conn: &Connection,
    asset_id: AssetId,
    page_no: u32,
    role: &str,
) -> Result<AssetSummary> {
    let (media_type, byte_len): (String, i64) = conn
        .query_row(
            "SELECT media_type, byte_len FROM asset WHERE id = ?1",
            [asset_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(PlatformError::from)?
        .ok_or_else(|| {
            PlatformError::DatabaseCorrupt(format!("page/derived 引用的资产不存在：{asset_id}"))
        })?;
    Ok(AssetSummary {
        asset_id,
        page_no,
        role: role.to_owned(),
        media_type,
        byte_len: u64::try_from(byte_len).unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::error::PlatformError;
    use eg_domain::{
        AnswerKey, AssetId, BBox, Classroom, ClassroomId, ComparatorId, ComparatorTrace,
        ComparatorTraceId, ComparatorVerdict, Confidence, DraftOrigin, DraftStatus, EngineStamp,
        ExcludeReason, ExifOrientation, GradeId, GradeMode, GradeRecord, GradeStatus, Judgement,
        MilliScore, ModelCall, ModelCallId, ModelRole, Page, PageQuality, Paper, PaperId,
        PartialChoiceRule, PartialChoiceSource, PartialChoiceTier, QualityVerdict, Question,
        QuestionId, QuestionKind, Recognition, RecognitionId, ReviewReasonKind, RosterSource,
        Rotation, RouteDecision, RouteTrace, RubricDraftItem, RubricId, RubricItem, RubricItemId,
        RubricOrigin, ScoringMode, SegmentId, SegmentMethod, Sha256Hex, Student, Submission,
        SubmissionExclusion, SubmissionId, SubmissionStatus, TaskId, TemplateId, Timestamp, Ulid,
    };

    fn timestamp() -> Timestamp {
        eg_domain::time::parse_rfc3339("2026-11-02T12:00:00Z").unwrap()
    }

    fn ulid(seed: u128) -> Ulid {
        Ulid::from_u128(seed.max(1))
    }

    fn open_db() -> (tempfile::TempDir, Database) {
        let temp = tempfile::tempdir().unwrap();
        let db = Database::open(&temp.path().join("db.sqlite3")).unwrap();
        (temp, db)
    }

    struct Fixture {
        classroom: Classroom,
        student: Student,
        paper: Paper,
        submission: Submission,
        asset: AssetId,
    }

    fn fixture() -> Fixture {
        let classroom = Classroom {
            id: ClassroomId::from_ulid(ulid(1)),
            name: "高一(1)班".to_owned(),
            term: "2026 秋".to_owned(),
        };
        let student = Student {
            id: eg_domain::StudentId::from_ulid(ulid(2)),
            classroom_id: classroom.id,
            student_no: "2026001".to_owned(),
            name: "张三".to_owned(),
            source: RosterSource::ManualAdd,
        };
        let question = Question {
            id: eg_domain::QuestionId::from_ulid(ulid(3)),
            paper_id: eg_domain::PaperId::from_ulid(ulid(4)),
            ordinal: 1,
            label: "1".to_owned(),
            kind: QuestionKind::Calculation,
            max_score: MilliScore::from_milli(10_000).unwrap(),
            stem: None,
            key: AnswerKey::ExplicitlyOmitted { note: None },
            rubric: None,
            subject_hint: None,
            knowledge_tags: vec![],
        };
        let paper = Paper {
            id: eg_domain::PaperId::from_ulid(ulid(4)),
            classroom_id: classroom.id,
            title: "期中数学".to_owned(),
            subject: eg_domain::SubjectId::new("math").unwrap(),
            total_score: question.max_score,
            questions: vec![question],
            key_version: 1,
            rubric_version: 1,
            created_at: timestamp(),
        };
        let asset = AssetId::of(b"page-bytes");
        let page = Page {
            id: eg_domain::PageId::from_ulid(ulid(5)),
            submission_id: SubmissionId::from_ulid(ulid(6)),
            page_no: 1,
            asset,
            width: 1200,
            height: 1600,
            rotation_applied: Rotation::new(0).unwrap(),
            exif_orientation: ExifOrientation::new(1).unwrap(),
            quality: PageQuality {
                blur_var: 200.0,
                exposure: 0.5,
                skew_deg: 1.0,
                resolution_ok: true,
                verdict: QualityVerdict::Acceptable,
            },
            derived: vec![],
        };
        let submission = Submission {
            id: SubmissionId::from_ulid(ulid(6)),
            paper_id: paper.id,
            classroom_id: classroom.id,
            student_id: Some(student.id),
            source_file_name: "张三.jpg".to_owned(),
            source_stem: "张三".to_owned(),
            pages: vec![page],
            status: SubmissionStatus::Imported,
            created_at: timestamp(),
        };
        Fixture {
            classroom,
            student,
            paper,
            submission,
            asset,
        }
    }

    /// 写入完整聚合夹具（各值 clone 后 move 进写闭包）。
    fn write_fixture(db: &Database, fixture: Fixture) -> Fixture {
        let classroom = fixture.classroom.clone();
        let student = fixture.student.clone();
        let paper = fixture.paper.clone();
        let submission = fixture.submission.clone();
        let asset = fixture.asset;
        db.write(move |tx| {
            insert_asset_row(tx, asset, 10, "image/jpeg", timestamp())?;
            insert_classroom(tx, &classroom, Some("manual"), timestamp())?;
            insert_student(tx, &student, timestamp())?;
            insert_paper(tx, &paper)?;
            insert_submission(tx, &submission)?;
            Ok(())
        })
        .unwrap();
        fixture
    }

    #[test]
    fn classroom_and_student_crud() {
        let (_temp, db) = open_db();
        let fixture = write_fixture(&db, fixture());
        let classrooms = db.read(list_classrooms).unwrap();
        assert_eq!(classrooms, vec![fixture.classroom.clone()]);
        let student = db
            .read(|conn| get_student_by_no(conn, "2026001"))
            .unwrap()
            .expect("学生应存在");
        assert_eq!(student, fixture.student);
        let students = db
            .read(|conn| list_students(conn, fixture.classroom.id))
            .unwrap();
        assert_eq!(students.len(), 1);
        let duplicate_student = fixture.student.clone();
        let duplicate = db.write(move |tx| insert_student(tx, &duplicate_student, timestamp()));
        assert!(duplicate.is_err(), "学号唯一约束必须生效");
    }

    #[test]
    fn paper_and_questions_roundtrip() {
        let (_temp, db) = open_db();
        let fixture = write_fixture(&db, fixture());
        let paper = db
            .read(|conn| get_paper(conn, fixture.paper.id))
            .unwrap()
            .expect("试卷应存在");
        assert_eq!(paper.total_score, fixture.paper.total_score);
        assert_eq!(paper.questions.len(), 1);
        let questions = db
            .read(|conn| list_questions(conn, fixture.paper.id))
            .unwrap();
        assert_eq!(questions.len(), 1);
        assert_eq!(questions[0].label, "1");
    }

    #[test]
    fn submission_and_pages_roundtrip() {
        let (_temp, db) = open_db();
        let fixture = write_fixture(&db, fixture());
        let submission = db
            .read(|conn| get_submission(conn, fixture.submission.id))
            .unwrap()
            .expect("提交应存在");
        assert_eq!(submission.source_file_name, "张三.jpg");
        assert_eq!(submission.source_stem, "张三");
        let pages = db
            .read(|conn| list_pages(conn, fixture.submission.id))
            .unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].asset, fixture.asset);
    }

    #[test]
    fn page_requires_existing_asset() {
        let (_temp, db) = open_db();
        let fixture = fixture();
        let classroom = fixture.classroom.clone();
        let paper = fixture.paper.clone();
        let submission = fixture.submission.clone();
        let result = db.write(move |tx| {
            insert_classroom(tx, &classroom, None, timestamp())?;
            insert_paper(tx, &paper)?;
            insert_submission(tx, &submission)?;
            Ok(())
        });
        assert!(result.is_err(), "页面引用的资产必须存在（外键）");
    }

    #[test]
    fn segments_and_recognition_roundtrip() {
        let (_temp, db) = open_db();
        let fixture = write_fixture(&db, fixture());
        let question_id = fixture.paper.questions[0].id;
        let page_id = fixture.submission.pages[0].id;
        let segment = Segment {
            id: SegmentId::from_ulid(ulid(7)),
            page_id,
            question_id: Some(question_id),
            bbox: BBox::new(0.1, 0.2, 0.5, 0.3).unwrap(),
            crop: fixture.asset,
            method: SegmentMethod::TemplateRegion,
        };
        let recognition = Recognition {
            id: RecognitionId::from_ulid(ulid(8)),
            page_id,
            segment_id: Some(segment.id),
            blocks: vec![],
            text: "42".to_owned(),
            engine: EngineStamp {
                role: ModelRole::Ocr,
                backend: eg_domain::BackendId::fixture(),
                model_id: "ocr-fixture".to_owned(),
                model_version: "1".to_owned(),
                prompt_hash: None,
                params_hash: Sha256Hex::of(b"params"),
            },
            confidence: Confidence::new(0.9).unwrap(),
            created_at: timestamp(),
        };
        let segment_id = segment.id;
        let recognition_id = recognition.id;
        db.write(move |tx| {
            insert_segment(tx, &segment)?;
            insert_recognition(tx, &recognition)?;
            Ok(())
        })
        .unwrap();
        let segments = db
            .read(|conn| list_segments_by_question(conn, question_id))
            .unwrap();
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].id, segment_id);
        let got = db
            .read(|conn| get_recognition(conn, recognition_id))
            .unwrap()
            .expect("识别结果应存在");
        assert_eq!(got.text, "42");
    }

    #[test]
    fn grade_records_and_model_calls_roundtrip() {
        let (_temp, db) = open_db();
        let fixture = write_fixture(&db, fixture());
        let question = fixture.paper.questions[0].clone();
        let record = GradeRecord {
            id: GradeId::from_ulid(ulid(9)),
            submission_id: fixture.submission.id,
            question_id: question.id,
            revision: 1,
            mode: GradeMode::Standard,
            route: RouteDecision::RuleOnly,
            route_trace: RouteTrace {
                rule_id: "R1".to_owned(),
                inputs_hash: Sha256Hex::of(b"inputs"),
                classifier_call: None,
                decided_at: timestamp(),
            },
            judgement: Judgement::Match {
                score: MilliScore::from_milli(10_000).unwrap(),
                reason: "与答案键一致".to_owned(),
                evidence: vec![],
            },
            awarded: MilliScore::from_milli(10_000).unwrap(),
            max_score: question.max_score,
            confidence: Confidence::new(0.95).unwrap(),
            status: GradeStatus::AutoAccepted,
            evidence: vec![],
            engines: vec![],
            created_at: timestamp(),
            reviewed_at: None,
            review_note: None,
            sampled: false,
        };
        let call = ModelCall {
            id: ModelCallId::from_ulid(ulid(10)),
            role: ModelRole::Judge,
            engine: EngineStamp {
                role: ModelRole::Judge,
                backend: eg_domain::BackendId::fixture(),
                model_id: "judge-fixture".to_owned(),
                model_version: "1".to_owned(),
                prompt_hash: None,
                params_hash: Sha256Hex::of(b"params"),
            },
            task_id: TaskId::from_ulid(ulid(11)),
            consent_id: None,
            input_hash: Sha256Hex::of(b"input"),
            output: serde_json::json!({"judgement": "match"}),
            latency_ms: 12,
            cost_micros: 0,
            created_at: timestamp(),
        };
        let trace = ComparatorTrace {
            id: ComparatorTraceId::from_ulid(ulid(12)),
            comparator: ComparatorId::new("numeric").unwrap(),
            verdict: ComparatorVerdict::Equivalent {
                note: "数值相等".to_owned(),
            },
            detail: serde_json::json!({"diff": 0}),
        };
        let record_id = record.id;
        let submission_id = fixture.submission.id;
        let task_id = call.task_id;
        let duplicate_record = record.clone();
        db.write(move |tx| {
            append_grade_record(tx, &record)?;
            insert_model_call(tx, &call)?;
            insert_comparator_trace(tx, record_id, &trace)?;
            Ok(())
        })
        .unwrap();
        let records = db
            .read(|conn| list_grade_records(conn, submission_id))
            .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].awarded, MilliScore::from_milli(10_000).unwrap());
        let calls = db.read(|conn| list_model_calls(conn, task_id)).unwrap();
        assert_eq!(calls.len(), 1);
        let duplicate = db.write(move |tx| append_grade_record(tx, &duplicate_record));
        assert!(duplicate.is_err(), "同题同 revision 不能重复追加");
    }

    #[test]
    fn asset_rows_are_deduped_and_counted() {
        let (_temp, db) = open_db();
        let fixture = fixture();
        let asset = fixture.asset;
        let first = db
            .write(move |tx| insert_asset_row(tx, asset, 10, "image/jpeg", timestamp()))
            .unwrap();
        let second = db
            .write(move |tx| insert_asset_row(tx, asset, 10, "image/jpeg", timestamp()))
            .unwrap();
        assert!(first, "首次写入应插入");
        assert!(!second, "重复写入应去重");
        let row = db
            .read(|conn| get_asset_row(conn, asset))
            .unwrap()
            .expect("资产行应存在");
        assert_eq!(row.0, 10);
        let (count, bytes) = db.read(asset_usage).unwrap();
        assert_eq!((count, bytes), (1, 10));
        db.write(move |tx| link_asset(tx, "page", "p1", "original", asset, timestamp()))
            .unwrap();
        assert!(db.read(|conn| asset_row_exists(conn, asset)).unwrap());
    }

    #[test]
    fn paper_key_and_rubric_revisions_roundtrip() {
        let (_temp, db) = open_db();
        let fixture = write_fixture(&db, fixture());
        let key = fixture.paper.questions[0].key.clone();
        let paper_id = fixture.paper.id;
        let stored_key = key.clone();
        db.write(move |tx| insert_paper_key_revision(tx, paper_id, 1, &stored_key, timestamp()))
            .unwrap();
        let got = db
            .read(|conn| get_paper_key_revision(conn, paper_id, 1))
            .unwrap()
            .expect("答案键应存在");
        assert_eq!(got, key);
    }

    #[test]
    fn rule_text_roundtrip() {
        let (_temp, db) = open_db();
        let fixture = write_fixture(&db, fixture());
        let question_id = fixture.paper.questions[0].id;
        let rule = RuleText {
            question_id,
            text: "写出公式得 2 分".to_owned(),
            source: eg_domain::RuleTextSource::Handwritten,
        };
        let stored = rule.clone();
        db.write(move |tx| insert_rule_text(tx, &stored, timestamp()))
            .unwrap();
        let got = db
            .read(|conn| get_rule_text(conn, question_id))
            .unwrap()
            .expect("规则原文应存在");
        assert_eq!(got.text, rule.text);
    }

    #[test]
    fn timestamp_helpers_roundtrip() {
        let text = timestamp_text(timestamp());
        assert_eq!(parse_timestamp(&text, "x").unwrap(), timestamp());
        assert!(parse_timestamp("不是时间", "x").is_err());
    }
    fn make_rubric(id_seed: u128, version: u32) -> Rubric {
        Rubric {
            id: RubricId::from_ulid(ulid(id_seed)),
            version,
            items: vec![RubricItem {
                id: RubricItemId::from_ulid(ulid(id_seed + 1)),
                criterion: "写出正确结果".to_owned(),
                points: MilliScore::from_milli(10_000).unwrap(),
                required: true,
                depends_on: vec![],
            }],
            mode: ScoringMode::Additive,
            total: MilliScore::from_milli(10_000).unwrap(),
            origin: RubricOrigin::TeacherHandwritten {
                confirmed_by: eg_domain::LOCAL_TEACHER.to_owned(),
                confirmed_at: timestamp(),
            },
            partial_choice: None,
        }
    }

    fn make_draft(id_seed: u128, question_id: QuestionId) -> RubricDraft {
        RubricDraft {
            id: RubricId::from_ulid(ulid(id_seed)),
            question_id,
            status: DraftStatus::Draft,
            items: vec![RubricDraftItem {
                criterion: "写出正确结果".to_owned(),
                points: MilliScore::from_milli(10_000).unwrap(),
                required: true,
                depends_on: vec![],
            }],
            raw_text: "写出正确结果得满分".to_owned(),
            origin: DraftOrigin::HandwrittenTransliteration,
            model_call_id: Some(ModelCallId::from_ulid(ulid(id_seed + 100))),
            validation: vec![],
            created_at: timestamp(),
            confirmed_at: None,
        }
    }

    fn make_payload() -> RubricDraftPayload {
        RubricDraftPayload {
            question_kind: Some(QuestionKind::MultipleChoice),
            max_score: Some(MilliScore::from_milli(10_000).unwrap()),
            mode: Some(ScoringMode::PartialChoice),
            partial_choice: Some(PartialChoiceRule {
                correct_option_count: 2,
                tiers: vec![PartialChoiceTier {
                    selected_correct: 1,
                    points: MilliScore::from_milli(5_000).unwrap(),
                    label: Some("只选对 1 个".to_owned()),
                }],
                wrong_selection_zero: true,
                source: PartialChoiceSource::Official,
                source_note: Some("2026 评分细则".to_owned()),
                total_milli: MilliScore::from_milli(10_000).unwrap(),
            }),
            template_id: Some(TemplateId::new("national-new-1").unwrap()),
            template_version: Some(2_026_001),
            template: Some(serde_json::json!({
                "template_id": "national-new-1",
                "set_id": "new-curriculum-1",
                "display_name": "新课标Ⅰ卷",
                "version": {"snapshot_year": 2026, "revision": 1},
                "status": "active",
            })),
        }
    }

    /// 构造一张指定版本、含两道题的试卷（复用夹具的班级，避免额外外键）。
    fn insert_two_question_paper(
        db: &Database,
        fixture: &Fixture,
        paper_seed: u128,
        rubric_version: u32,
    ) -> Paper {
        let paper_id = PaperId::from_ulid(ulid(paper_seed));
        let make_question = |seed: u128, ordinal: u32, label: &str| Question {
            id: QuestionId::from_ulid(ulid(seed)),
            paper_id,
            ordinal,
            label: label.to_owned(),
            kind: QuestionKind::Calculation,
            max_score: MilliScore::from_milli(10_000).unwrap(),
            stem: None,
            key: AnswerKey::ExplicitlyOmitted { note: None },
            rubric: None,
            subject_hint: None,
            knowledge_tags: vec![],
        };
        let paper = Paper {
            id: paper_id,
            classroom_id: fixture.classroom.id,
            title: "规则版本测试卷".to_owned(),
            subject: eg_domain::SubjectId::new("math").unwrap(),
            total_score: MilliScore::from_milli(20_000).unwrap(),
            questions: vec![
                make_question(paper_seed + 1, 1, "1"),
                make_question(paper_seed + 2, 2, "2"),
            ],
            key_version: 1,
            rubric_version,
            created_at: timestamp(),
        };
        let stored = paper.clone();
        db.write(move |tx| insert_paper(tx, &stored)).unwrap();
        paper
    }

    #[test]
    fn stored_rubric_draft_roundtrip_and_listing() {
        let (_temp, db) = open_db();
        let fixture = write_fixture(&db, fixture());
        let question = fixture.paper.questions[0].clone();

        let stored = StoredRubricDraft {
            draft: make_draft(40, question.id),
            payload: make_payload(),
        };
        let expected = stored.clone();
        db.write(move |tx| insert_stored_rubric_draft(tx, &stored))
            .unwrap();
        let loaded = db
            .read(|conn| get_stored_rubric_draft(conn, expected.draft.id))
            .unwrap()
            .expect("草稿应存在");
        assert_eq!(loaded, expected);
        assert_eq!(
            loaded.payload.partial_choice,
            expected.payload.partial_choice
        );
        assert_eq!(loaded.payload.template, expected.payload.template);

        // v1 旧接口写入的草稿：负载列全为 None（不得猜造）。
        let legacy = make_draft(41, question.id);
        let legacy_id = legacy.id;
        db.write(move |tx| insert_rubric_draft(tx, &legacy))
            .unwrap();
        let loaded_legacy = db
            .read(|conn| get_stored_rubric_draft(conn, legacy_id))
            .unwrap()
            .expect("旧草稿应存在");
        assert_eq!(loaded_legacy.payload, RubricDraftPayload::default());

        let by_question = db
            .read(|conn| list_stored_rubric_drafts_by_question(conn, question.id))
            .unwrap();
        assert_eq!(by_question.len(), 2);
        assert_eq!(
            by_question[0].draft.id, expected.draft.id,
            "按创建时间/ID 升序"
        );
        assert_eq!(by_question[1].draft.id, legacy_id);
        let by_paper = db
            .read(|conn| list_stored_rubric_drafts_by_paper(conn, fixture.paper.id))
            .unwrap();
        assert_eq!(by_paper.len(), 2);
    }

    #[test]
    fn stored_rubric_drafts_by_paper_orders_by_question_ordinal() {
        let (_temp, db) = open_db();
        let fixture = write_fixture(&db, fixture());
        let paper = insert_two_question_paper(&db, &fixture, 70, 1);
        let second = paper.questions[1].clone();
        let first = paper.questions[0].clone();
        // 先写第 2 题，再写第 1 题；按试卷列出时仍应按题号排序。
        for (seed, question) in [(72_u128, second.clone()), (71_u128, first.clone())] {
            let stored = StoredRubricDraft {
                draft: make_draft(seed, question.id),
                payload: RubricDraftPayload::default(),
            };
            db.write(move |tx| insert_stored_rubric_draft(tx, &stored))
                .unwrap();
        }
        let listed = db
            .read(|conn| list_stored_rubric_drafts_by_paper(conn, paper.id))
            .unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].draft.question_id, first.id);
        assert_eq!(listed[1].draft.question_id, second.id);
    }

    #[test]
    fn paper_rubric_version_cas_and_active_rubric_lookup() {
        let (_temp, db) = open_db();
        let fixture = write_fixture(&db, fixture());
        let paper = insert_two_question_paper(&db, &fixture, 50, 0);
        let question_id = paper.questions[0].id;

        assert_eq!(
            db.read(|conn| paper_rubric_version(conn, question_id))
                .unwrap(),
            0
        );
        assert!(
            db.read(|conn| get_active_rubric(conn, question_id))
                .unwrap()
                .is_none()
        );

        let v1 = make_rubric(60, 1);
        let new_version = db
            .write(move |tx| save_active_rubric(tx, question_id, &v1, 0, timestamp()))
            .unwrap();
        assert_eq!(new_version, 1);
        assert_eq!(
            db.read(|conn| paper_rubric_version(conn, question_id))
                .unwrap(),
            1
        );
        let active = db
            .read(|conn| get_active_rubric(conn, question_id))
            .unwrap()
            .expect("应有生效规则");
        assert_eq!(active.version, 1);
        assert_eq!(active.id, RubricId::from_ulid(ulid(60)));

        let v2 = make_rubric(60, 2);
        let new_version = db
            .write(move |tx| save_active_rubric(tx, question_id, &v2, 1, timestamp()))
            .unwrap();
        assert_eq!(new_version, 2);
        assert_eq!(
            db.read(|conn| latest_question_rubric_version(conn, question_id))
                .unwrap(),
            2
        );
        let stored_paper = db
            .read(|conn| get_paper(conn, paper.id))
            .unwrap()
            .expect("试卷应存在");
        assert_eq!(stored_paper.rubric_version, 2, "body 与列必须同步");

        // 版本冲突：期望 1、实际 2。
        let stale = make_rubric(60, 2);
        let error = db
            .write(move |tx| save_active_rubric(tx, question_id, &stale, 1, timestamp()))
            .unwrap_err();
        match error {
            PlatformError::VersionConflict { expected, actual } => {
                assert_eq!(expected, 1);
                assert_eq!(actual, 2);
            }
            other => panic!("应为版本冲突，实际 {other:?}"),
        }

        // 版本只增一：期望 2 但 rubric.version = 5 必须拒绝，且不得 bump。
        let invalid = make_rubric(60, 5);
        let error = db
            .write(move |tx| save_active_rubric(tx, question_id, &invalid, 2, timestamp()))
            .unwrap_err();
        assert!(matches!(error, PlatformError::InvalidArgument(_)));
        assert_eq!(
            db.read(|conn| paper_rubric_version(conn, question_id))
                .unwrap(),
            2,
            "非法版本不得推进试卷版本"
        );

        // 低层 bump 原语独立可用；版本不匹配时 CAS 失败。
        assert_eq!(
            db.write(move |tx| advance_paper_rubric_version(tx, paper.id, 2))
                .unwrap(),
            3
        );
        let error = db
            .write(move |tx| advance_paper_rubric_version(tx, paper.id, 2))
            .unwrap_err();
        assert!(matches!(
            error,
            PlatformError::VersionConflict {
                expected: 2,
                actual: 3
            }
        ));
        assert_eq!(
            db.read(|conn| latest_question_rubric_version(conn, question_id))
                .unwrap(),
            2,
            "仅 bump 不写规则快照"
        );
        // 题目不存在：显式 NotFound，不静默返回 0。
        let missing = QuestionId::from_ulid(ulid(999));
        assert!(matches!(
            db.read(move |conn| paper_rubric_version(conn, missing)),
            Err(PlatformError::NotFound(_))
        ));
    }

    #[test]
    fn upsert_stored_rubric_draft_updates_state_and_keeps_identity() {
        let (_temp, db) = open_db();
        let fixture = write_fixture(&db, fixture());
        let question = fixture.paper.questions[0].clone();
        let stored = StoredRubricDraft {
            draft: make_draft(80, question.id),
            payload: make_payload(),
        };
        let draft_id = stored.draft.id;
        let original_created_at = stored.draft.created_at;
        db.write(move |tx| insert_stored_rubric_draft(tx, &stored))
            .unwrap();

        let mut updated = make_draft(80, question.id);
        updated.status = DraftStatus::Confirmed;
        updated.confirmed_at = Some(timestamp());
        // 覆盖时身份字段（created_at）不得被刷新。
        updated.created_at = eg_domain::time::parse_rfc3339("2026-11-03T12:00:00Z").unwrap();
        let updated_stored = StoredRubricDraft {
            draft: updated,
            payload: RubricDraftPayload {
                question_kind: Some(QuestionKind::Calculation),
                max_score: Some(MilliScore::from_milli(10_000).unwrap()),
                ..RubricDraftPayload::default()
            },
        };
        db.write(move |tx| upsert_stored_rubric_draft(tx, &updated_stored))
            .unwrap();

        let loaded = db
            .read(|conn| get_stored_rubric_draft(conn, draft_id))
            .unwrap()
            .expect("草稿应存在");
        assert_eq!(loaded.draft.status, DraftStatus::Confirmed);
        assert!(loaded.draft.confirmed_at.is_some());
        assert_eq!(loaded.draft.created_at, original_created_at);
        assert_eq!(
            loaded.payload.question_kind,
            Some(QuestionKind::Calculation)
        );
        assert_eq!(loaded.payload.partial_choice, None);
        assert_eq!(loaded.payload.template, None);
        let listed = db
            .read(|conn| list_stored_rubric_drafts_by_question(conn, question.id))
            .unwrap();
        assert_eq!(listed.len(), 1, "upsert 不追加新行");
    }

    fn grade_seed(
        fixture: &Fixture,
        question_id: QuestionId,
        seed: u128,
        revision: u32,
        mode: GradeMode,
        status: GradeStatus,
        sampled: bool,
    ) -> GradeRecord {
        let question = fixture
            .paper
            .questions
            .iter()
            .find(|question| question.id == question_id)
            .cloned()
            .unwrap_or_else(|| fixture.paper.questions[0].clone());
        GradeRecord {
            id: GradeId::from_ulid(ulid(seed)),
            submission_id: fixture.submission.id,
            question_id,
            revision,
            mode,
            route: RouteDecision::RuleOnly,
            route_trace: RouteTrace {
                rule_id: "R1".to_owned(),
                inputs_hash: Sha256Hex::of(b"inputs"),
                classifier_call: None,
                decided_at: timestamp(),
            },
            judgement: Judgement::Match {
                score: MilliScore::from_milli(5_000).unwrap(),
                reason: "与答案键一致".to_owned(),
                evidence: vec![],
            },
            awarded: MilliScore::from_milli(5_000).unwrap(),
            max_score: question.max_score,
            confidence: Confidence::new(0.9).unwrap(),
            status,
            evidence: vec![],
            engines: vec![],
            created_at: timestamp(),
            reviewed_at: None,
            review_note: None,
            sampled,
        }
    }

    #[test]
    fn grade_queue_returns_latest_revision_with_filters_and_cursor() {
        let (_temp, db) = open_db();
        let fixture = write_fixture(&db, fixture());
        let q1 = fixture.paper.questions[0].id;
        let q2 = QuestionId::from_ulid(ulid(30));
        let mut second = fixture.paper.questions[0].clone();
        second.id = q2;
        second.ordinal = 2;
        second.label = "2".to_owned();
        let rec_v1 = grade_seed(
            &fixture,
            q1,
            101,
            1,
            GradeMode::Standard,
            GradeStatus::NeedsReview,
            false,
        );
        let rec_v2 = grade_seed(
            &fixture,
            q1,
            102,
            2,
            GradeMode::Standard,
            GradeStatus::NeedsReview,
            false,
        );
        let rec_q2 = grade_seed(
            &fixture,
            q2,
            103,
            1,
            GradeMode::Fast,
            GradeStatus::AutoAccepted,
            true,
        );
        let rec_q2_v2 = grade_seed(
            &fixture,
            q2,
            104,
            2,
            GradeMode::Fast,
            GradeStatus::AutoAccepted,
            true,
        );
        let rec_v1_id = rec_v1.id;
        let rec_v2_id = rec_v2.id;
        let rec_q2_v2_id = rec_q2_v2.id;
        db.write(move |tx| {
            insert_question(tx, &second)?;
            append_grade_record_with_reasons(tx, &rec_v1, &[ReviewReasonKind::LowConfidence])?;
            append_grade_record_with_reasons(
                tx,
                &rec_v2,
                &[ReviewReasonKind::LowConfidence, ReviewReasonKind::Conflict],
            )?;
            append_grade_record(tx, &rec_q2)?;
            append_grade_record(tx, &rec_q2_v2)?;
            Ok(())
        })
        .unwrap();

        // 每个「提交 × 题」只返回最新修订：q1→v2、q2→v2。
        let all = db
            .read(|conn| list_grade_records_for_queue(conn, &GradeQueueFilter::default(), None))
            .unwrap();
        assert_eq!(all.len(), 2);
        let ids: Vec<GradeId> = all.iter().map(|record| record.id).collect();
        assert!(!ids.contains(&rec_v1_id), "旧修订不得出现在队列");
        assert!(ids.contains(&rec_q2_v2_id), "最新修订应出现");

        // 过滤：模式 / 状态 / 抽检 / 学生 / 题目 / 试卷 / 原因。
        let fast = GradeQueueFilter {
            modes: vec![GradeMode::Fast],
            ..GradeQueueFilter::default()
        };
        assert_eq!(
            db.read(|conn| list_grade_records_for_queue(conn, &fast, None))
                .unwrap()
                .len(),
            1
        );
        let review = GradeQueueFilter {
            statuses: vec![GradeStatus::NeedsReview],
            ..GradeQueueFilter::default()
        };
        assert_eq!(
            db.read(|conn| list_grade_records_for_queue(conn, &review, None))
                .unwrap()
                .len(),
            1
        );
        let sampled = GradeQueueFilter {
            sampled: Some(true),
            ..GradeQueueFilter::default()
        };
        assert_eq!(
            db.read(|conn| list_grade_records_for_queue(conn, &sampled, None))
                .unwrap()
                .len(),
            1
        );
        let conflict = GradeQueueFilter {
            reasons: vec![ReviewReasonKind::Conflict],
            ..GradeQueueFilter::default()
        };
        let conflict_records = db
            .read(|conn| list_grade_records_for_queue(conn, &conflict, None))
            .unwrap();
        assert_eq!(conflict_records.len(), 1);
        assert_eq!(conflict_records[0].id, rec_v2_id);
        let by_student = GradeQueueFilter {
            student_id: fixture.student.id.into(),
            ..GradeQueueFilter::default()
        };
        assert_eq!(
            db.read(|conn| list_grade_records_for_queue(conn, &by_student, None))
                .unwrap()
                .len(),
            2
        );
        let by_paper = GradeQueueFilter {
            paper_id: Some(fixture.paper.id),
            ..GradeQueueFilter::default()
        };
        assert_eq!(
            db.read(|conn| count_grade_records_for_queue(conn, &by_paper))
                .unwrap(),
            2
        );
        let by_question = GradeQueueFilter {
            question_id: Some(q2),
            ..GradeQueueFilter::default()
        };
        assert_eq!(
            db.read(|conn| list_grade_records_for_queue(conn, &by_question, None))
                .unwrap()
                .len(),
            1
        );

        // 游标分页：稳定排序、不重复。
        let page_filter = GradeQueueFilter {
            limit: 1,
            ..GradeQueueFilter::default()
        };
        let first_page = db
            .read(|conn| list_grade_records_for_queue(conn, &page_filter, None))
            .unwrap();
        assert_eq!(first_page.len(), 1);
        let cursor = GradeCursor {
            created_at: first_page[0].created_at,
            id: first_page[0].id,
        };
        let second_page = db
            .read(|conn| list_grade_records_for_queue(conn, &page_filter, Some(cursor)))
            .unwrap();
        assert_eq!(second_page.len(), 1);
        assert_ne!(second_page[0].id, first_page[0].id);

        // 单条/最新读取。
        let by_id = db
            .read(|conn| get_grade_record(conn, rec_v1_id))
            .unwrap()
            .expect("旧修订可按 id 读取");
        assert_eq!(by_id.revision, 1);
        let latest = db
            .read(|conn| get_latest_grade_record(conn, fixture.submission.id, q1))
            .unwrap()
            .expect("最新修订应存在");
        assert_eq!(latest.id, rec_v2_id);
    }

    #[test]
    fn submission_exclusion_roundtrip_and_delete() {
        let (_temp, db) = open_db();
        let fixture = write_fixture(&db, fixture());
        let exclusion = SubmissionExclusion {
            submission_id: fixture.submission.id,
            reason: ExcludeReason::Absent,
            note: "缺考".to_owned(),
            at: timestamp(),
        };
        db.write(move |tx| upsert_submission_exclusion(tx, &exclusion))
            .unwrap();
        let got = db
            .read(|conn| get_submission_exclusion(conn, fixture.submission.id))
            .unwrap()
            .expect("排除记录应存在");
        assert_eq!(got.reason, ExcludeReason::Absent);
        assert_eq!(got.note, "缺考");

        let updated = SubmissionExclusion {
            submission_id: fixture.submission.id,
            reason: ExcludeReason::Duplicate,
            note: "重复提交".to_owned(),
            at: timestamp(),
        };
        db.write(move |tx| upsert_submission_exclusion(tx, &updated))
            .unwrap();
        let got = db
            .read(|conn| get_submission_exclusion(conn, fixture.submission.id))
            .unwrap()
            .expect("排除记录应存在");
        assert_eq!(got.reason, ExcludeReason::Duplicate, "同提交只保留一条");

        let submission_id = fixture.submission.id;
        let deleted = db
            .write(move |tx| delete_submission_exclusion(tx, submission_id))
            .unwrap();
        assert!(deleted);
        assert!(
            db.read(|conn| get_submission_exclusion(conn, fixture.submission.id))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn undo_stack_is_lifo_and_persistent() {
        let (_temp, db) = open_db();
        let first = UndoEntry {
            id: ulid(201),
            command_id: "cmd-1".to_owned(),
            action: "review.accept".to_owned(),
            entity_type: "grade_record".to_owned(),
            entity_id: "g1".to_owned(),
            payload: serde_json::json!({"previous": null}),
            created_at: timestamp(),
        };
        let second = UndoEntry {
            id: ulid(202),
            command_id: "cmd-2".to_owned(),
            action: "review.change_score".to_owned(),
            entity_type: "grade_record".to_owned(),
            entity_id: "g1".to_owned(),
            payload: serde_json::json!({"previous": 5000}),
            created_at: timestamp(),
        };
        let (first_id, second_id) = (first.id, second.id);
        let first_for_push = first.clone();
        db.write(move |tx| push_undo(tx, &first_for_push)).unwrap();
        let second_for_push = second.clone();
        db.write(move |tx| push_undo(tx, &second_for_push)).unwrap();
        let listed = db.read(|conn| list_undo(conn, 10)).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, second_id, "最新命令在前");

        let popped = db
            .write(|tx| pop_undo(tx))
            .unwrap()
            .expect("应弹出最新条目");
        assert_eq!(popped.id, second_id);
        let popped = db
            .write(|tx| pop_undo(tx))
            .unwrap()
            .expect("应弹出剩余条目");
        assert_eq!(popped.id, first_id);
        assert!(
            db.write(|tx| pop_undo(tx)).unwrap().is_none(),
            "空栈弹出为 None"
        );
    }

    #[test]
    fn export_records_roundtrip_and_replay_lookup() {
        let (_temp, db) = open_db();
        let summary = Sha256Hex::of(b"report-bytes");
        let record = ExportRecord {
            id: ulid(301),
            kind: "paper_report".to_owned(),
            format: "xlsx".to_owned(),
            scope: serde_json::json!({"paper_id": "p1", "mode": "standard"}),
            summary_hash: summary,
            row_count: 3,
            body: serde_json::json!({"sheet": "成绩单"}),
            created_at: timestamp(),
            audit_id: Some(ulid(302)),
        };
        db.write(move |tx| insert_export_record(tx, &record))
            .unwrap();
        let got = db
            .read(|conn| get_export_record(conn, ulid(301)))
            .unwrap()
            .expect("导出记录应存在");
        assert_eq!(got.summary_hash, summary);
        assert_eq!(got.row_count, 3);
        let listed = db.read(|conn| list_export_records(conn, 10)).unwrap();
        assert_eq!(listed.len(), 1);
        let replay = db
            .read(|conn| find_export_records_by_summary(conn, summary))
            .unwrap();
        assert_eq!(replay.len(), 1, "同摘要可定位导出记录用于重放比较");
    }

    #[test]
    fn grade_idempotency_put_is_insert_once() {
        let (_temp, db) = open_db();
        let fixture = write_fixture(&db, fixture());
        let record = GradeIdempotency {
            key: "idem-key-1".to_owned(),
            submission_id: fixture.submission.id,
            question_id: fixture.paper.questions[0].id,
            mode: GradeMode::Standard,
            grade_id: Some(GradeId::from_ulid(ulid(401))),
            result_summary: serde_json::json!({"awarded": 5000}),
            created_at: timestamp(),
        };
        let record_for_first = record.clone();
        let inserted = db
            .write(move |tx| put_grade_idempotency(tx, &record_for_first))
            .unwrap();
        assert!(inserted);
        let record_for_second = record.clone();
        let duplicate = db
            .write(move |tx| put_grade_idempotency(tx, &record_for_second))
            .unwrap();
        assert!(!duplicate, "重复幂等键不得覆盖已有结果");
        let got = db
            .read(|conn| get_grade_idempotency(conn, "idem-key-1"))
            .unwrap()
            .expect("幂等记录应存在");
        assert_eq!(got.grade_id, record.grade_id);
        assert_eq!(got.result_summary, record.result_summary);
    }

    #[test]
    fn grade_progress_upsert_and_incomplete_scan() {
        let (_temp, db) = open_db();
        let fixture = write_fixture(&db, fixture());
        let question = fixture.paper.questions[0].id;
        let stamp = EngineStamp {
            role: ModelRole::Judge,
            backend: eg_domain::BackendId::fixture(),
            model_id: "judge-fixture".to_owned(),
            model_version: "1".to_owned(),
            prompt_hash: None,
            params_hash: Sha256Hex::of(b"params"),
        };
        let submission_level = GradeProgress {
            submission_id: fixture.submission.id,
            question_id: None,
            state: GradeProgressState::Running,
            attempts: 1,
            engine_fingerprint: Some(stamp.clone()),
            detail: Some(serde_json::json!({"done_questions": 2})),
            updated_at: timestamp(),
        };
        let question_level = GradeProgress {
            submission_id: fixture.submission.id,
            question_id: Some(question),
            state: GradeProgressState::Done,
            attempts: 2,
            engine_fingerprint: None,
            detail: None,
            updated_at: timestamp(),
        };
        let submission_for_put = submission_level.clone();
        db.write(move |tx| {
            put_grade_progress(tx, &submission_for_put)?;
            put_grade_progress(tx, &question_level)
        })
        .unwrap();
        let got = db
            .read(|conn| get_grade_progress(conn, fixture.submission.id, None))
            .unwrap()
            .expect("提交级断点应存在");
        assert_eq!(got.state, GradeProgressState::Running);
        assert_eq!(got.attempts, 1);
        assert_eq!(got.engine_fingerprint, Some(stamp.clone()));
        let listed = db
            .read(|conn| list_grade_progress(conn, fixture.submission.id))
            .unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].question_id, None, "提交级断点排在前");

        // 未完成扫描只返回 state != done。
        let incomplete = db
            .read(|conn| list_incomplete_grade_progress(conn, 10))
            .unwrap();
        assert_eq!(incomplete.len(), 1);
        assert_eq!(incomplete[0].question_id, None);

        // upsert 更新状态/尝试次数。
        let retried = GradeProgress {
            state: GradeProgressState::Failed,
            attempts: 3,
            ..submission_level
        };
        db.write(move |tx| put_grade_progress(tx, &retried))
            .unwrap();
        let got = db
            .read(|conn| get_grade_progress(conn, fixture.submission.id, None))
            .unwrap()
            .expect("断点应存在");
        assert_eq!(got.state, GradeProgressState::Failed);
        assert_eq!(got.attempts, 3);
    }

    #[test]
    fn grade_evidence_summary_includes_assets_and_recognitions() {
        let (_temp, db) = open_db();
        let fixture = write_fixture(&db, fixture());
        let question = fixture.paper.questions[0].id;
        let page_id = fixture.submission.pages[0].id;
        let segment = Segment {
            id: SegmentId::from_ulid(ulid(501)),
            page_id,
            question_id: Some(question),
            bbox: BBox::new(0.1, 0.2, 0.5, 0.3).unwrap(),
            crop: fixture.asset,
            method: SegmentMethod::TemplateRegion,
        };
        let recognition = Recognition {
            id: RecognitionId::from_ulid(ulid(502)),
            page_id,
            segment_id: Some(segment.id),
            blocks: vec![],
            text: "42".to_owned(),
            engine: EngineStamp {
                role: ModelRole::Ocr,
                backend: eg_domain::BackendId::fixture(),
                model_id: "ocr-fixture".to_owned(),
                model_version: "1".to_owned(),
                prompt_hash: None,
                params_hash: Sha256Hex::of(b"params"),
            },
            confidence: Confidence::new(0.9).unwrap(),
            created_at: timestamp(),
        };
        let record = grade_seed(
            &fixture,
            question,
            503,
            1,
            GradeMode::Standard,
            GradeStatus::AutoAccepted,
            false,
        );
        let grade_id = record.id;
        db.write(move |tx| {
            insert_segment(tx, &segment)?;
            insert_recognition(tx, &recognition)?;
            append_grade_record(tx, &record)
        })
        .unwrap();

        let summary = db
            .read(|conn| load_grade_evidence_summary(conn, grade_id))
            .unwrap()
            .expect("证据摘要应存在");
        assert_eq!(summary.grade_id, grade_id);
        assert_eq!(summary.question_id, question);
        assert_eq!(summary.assets.len(), 1);
        assert_eq!(summary.assets[0].role, "page");
        assert_eq!(summary.assets[0].media_type, "image/jpeg");
        assert_eq!(summary.recognitions.len(), 1);
        assert_eq!(summary.recognitions[0].text, "42");
        assert_eq!(summary.recognitions[0].page_no, 1);
    }
}
