//! 规则整理（structurer）调用与严格输出 Schema（`SPEC-rubric.md` §2.3/§3.4；
//! `SPEC-inference.md` §2.3；T25）。
//!
//! 分工（与 `SPEC-inference.md` 一致）：
//! 1. 本模块**渲染提示词并计算哈希**（inference 只消费渲染好的提示词）；
//! 2. inference 的 `structurer` 角色执行调用并做结构校验；
//! 3. 本模块对返回的原始 JSON 再做一次**防御性严格解析**（Schema v1，拒绝未知字段），
//!    结构不合法时发起**至多 1 次机械修复**；
//! 4. 通过校验后只组装 `RubricDraft`（来源 ModelStructured）并落静态校验结果——
//!    **绝不生成生效 `Rubric`**，教师确认恒开。
//!
//! TODO(T25 接 `eg-inference`)：`app` 组合根实现 [`StructurerPort`]，用
//! `ExpectedSchema::Structurer` 调用 `eg_inference::InferenceService::complete`：
//! 本模块给出 `prompt.text`/`prompt.hash` 与原始原文，`app` 负责任务 id、任务授权、
//! 预算、模型 id 与 `ModelCall` 落库；返回 `ValidatedOutput::Structurer` 的原始 JSON 文本。
//! 依赖方向保持 rubric → trait ← app → inference，避免把模型/授权耦合进本 crate。

use eg_domain::{
    DraftOrigin, MilliScore, ModelCallId, Question, QuestionKind, RubricDraftItem, ScoringMode,
    Sha256Hex,
};
use serde::Deserialize;

use crate::draft::{DraftSource, RubricEngine, content_hash};
use crate::error::{Result, RubricError};
use crate::ports::{MAX_ITEMS, NewRubricAuditEvent, RubricAction, StoredDraft};
use crate::templates::TEMPLATE_CRITERION_MARKER;

/// 规则整理输出 Schema 版本（写进提示词与审计；不兼容变更时递增）。
pub const STRUCTURER_SCHEMA_VERSION: u32 = 1;

/// 单条判据字符上限（`SPEC-rubric.md` §2.3）。
pub const MAX_CRITERION_CHARS: usize = 500;

/// 单个评分项分值上限（千分之一分，1000 分）。
pub const MAX_ITEM_POINTS_MILLI: i64 = 1_000_000;

/// 一次整理输出全部分值合计上限（千分之一分，2000 分）。
pub const MAX_TOTAL_POINTS_MILLI: i64 = 2_000_000;

/// 机械修复次数上限（`SPEC-rubric.md` §3.4：至多 1 次）。
pub const MAX_REPAIR_ATTEMPTS: u8 = 1;

/// 渲染后的规则整理提示词（本模块渲染，inference 只消费）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructurerPrompt {
    /// 完整提示词文本。
    pub text: String,
    /// 提示词哈希（SHA-256，进 `ModelCall`/审计，用于复现）。
    pub hash: Sha256Hex,
    /// 输出 Schema 版本。
    pub schema_version: u32,
}

/// 一次规则整理的输入上下文（原文 + 题目上下文 + 可选模板骨架提示）。
#[derive(Debug, Clone, Copy)]
pub struct StructurerRequest<'a> {
    /// 规则原文（教师自然语言；不得包含学生作答）。
    pub raw_text: &'a str,
    /// 题目题型（决定提示词与默认计分方式）。
    pub question_kind: QuestionKind,
    /// 题目满分（模型分值的合计目标，仅提示，不强制）。
    pub max_score: MilliScore,
    /// 可选模板骨架提示（模板套用时给出结构参考；无模板为 `None`）。
    pub template_skeleton: Option<&'a str>,
}

/// 本次调用属于首次整理还是机械修复。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructurerAttempt {
    /// 首次整理。
    Initial,
    /// 机械修复（至多一次；只反馈结构违规，不重写语义）。
    Repair,
}

/// 一次规则整理调用（端口输入）。
#[derive(Debug)]
pub struct StructurerInvocation<'a> {
    /// 已渲染提示词（含哈希）。
    pub prompt: &'a StructurerPrompt,
    /// 输入上下文。
    pub request: &'a StructurerRequest<'a>,
    /// 首次/修复。
    pub attempt: StructurerAttempt,
    /// 首次输出原文（修复时提供，便于模型只修结构）。
    pub previous_output: Option<&'a str>,
    /// 结构校验违规（修复时提供）。
    pub violations: &'a [String],
}

/// 模型整理调用结果（原始 JSON 文本原样保留，不做截断/猜测）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructurerReply {
    /// 模型调用 id（`ModelCall.id`，用于审计与复现）。
    pub model_call_id: ModelCallId,
    /// 原始输出文本（严格校验的唯一输入）。
    pub raw_output: String,
}

/// 规则整理端口：由 `app` 组合根用 `eg-inference` 适配（本 crate 不依赖模型实现）。
///
/// 契约：
/// - 成功返回原始输出与 `ModelCall` id；失败返回显式错误（本模块不重试调用本身）；
/// - 端口必须把 `prompt.text` 原样作为提示词、`prompt.hash` 写入 `ModelCall`；
/// - `attempt = Repair` 表示本模块最多发起一次机械修复，端口不得自行多次重试。
pub trait StructurerPort: Send + Sync {
    /// 发起一次规则整理调用。
    ///
    /// # Errors
    ///
    /// 模型不可用、超时、授权拒绝等返回 [`RubricError::Structurer`] 或底层错误。
    fn structure(&self, invocation: &StructurerInvocation<'_>) -> Result<StructurerReply>;
}

/// 渲染规则整理提示词（确定性：同一输入 → 同一文本与哈希）。
#[must_use]
pub fn render_structurer_prompt(request: &StructurerRequest<'_>) -> StructurerPrompt {
    let template_hint = request.template_skeleton.map_or_else(
        || "（无模板骨架，请按题目上下文自行拆分）".to_owned(),
        |skeleton| format!("（模板骨架参考，仅用于结构，不得照抄占位文本）{skeleton}"),
    );
    let text = format!(
        "[角色] 规则整理（structurer）：把教师评分规则原文拆成评分项草稿。\n\
         [硬性约束]\n\
         1. 只输出一个 JSON 对象，不要 Markdown、不要代码围栏、不要解释文字；\n\
         2. 不得新增原文没有的判分口径，不得修改分值语义，不得生成或修改答案键，不得判断学生作答；\n\
         3. 输出仅为草稿，生效必须由教师确认；\n\
         4. 判据不得使用模板占位标记 {marker}。\n\
         [题目上下文] 题型={kind:?}；满分={max_milli}（千分之一分）\n\
         [输出 Schema v{schema}] {{\"items\":[{{\"criterion\":\"<判据>\",\"points\":<正整数，千分之一分>,\
         \"required\":<true|false>,\"depends_on\":[<1 起的评分项序号>]}}]}}\n\
         [字段约束] items 1..={max_items} 项；criterion 非空、不超过 {max_chars} 字符；\
         points 1..={max_item}，合计不超过 {max_total}；depends_on 只能引用更小序号、不得成环；\
         全部分值合计应等于题目满分 {max_milli}。\n\
         [模板骨架] {template_hint}\n\
         [规则原文]\n{raw_text}",
        marker = TEMPLATE_CRITERION_MARKER,
        kind = request.question_kind,
        max_milli = request.max_score.as_milli(),
        schema = STRUCTURER_SCHEMA_VERSION,
        max_items = MAX_ITEMS,
        max_chars = MAX_CRITERION_CHARS,
        max_item = MAX_ITEM_POINTS_MILLI,
        max_total = MAX_TOTAL_POINTS_MILLI,
        raw_text = request.raw_text,
    );
    let hash = Sha256Hex::of(text.as_bytes());
    StructurerPrompt {
        text,
        hash,
        schema_version: STRUCTURER_SCHEMA_VERSION,
    }
}

/// 严格 Schema 的原始输出（`deny_unknown_fields`：未知字段即不合法）。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOutput {
    items: Vec<RawItem>,
}

/// 严格 Schema 的评分项（字段名与 `SPEC-rubric.md` §2.3 一致）。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawItem {
    criterion: String,
    points: i64,
    #[serde(default = "default_required")]
    required: bool,
    #[serde(default)]
    depends_on: Vec<u32>,
}

const fn default_required() -> bool {
    true
}

/// 严格解析并校验规则整理输出。
///
/// 返回值：合法时给出草稿评分项（`depends_on` 保持 1 起序号）；不合法时给出全部违规
/// 摘要（供机械修复反馈与错误报告），**不做任何自动修补/截断**。
///
/// # Errors
///
/// 本函数不返回 `Result`，用 `Err(Vec<String>)` 表示「不合法 + 违规列表」。
pub fn parse_structurer_output(
    raw: &str,
) -> core::result::Result<Vec<RubricDraftItem>, Vec<String>> {
    let parsed: RawOutput = serde_json::from_str(raw).map_err(|error| {
        vec![format!(
            "输出不是严格 Schema v{STRUCTURER_SCHEMA_VERSION} 的单个 JSON 对象（未知字段/缺失字段/语法错误）：{error}"
        )]
    })?;
    validate_items(parsed.items)
}

fn validate_items(items: Vec<RawItem>) -> core::result::Result<Vec<RubricDraftItem>, Vec<String>> {
    let mut violations: Vec<String> = Vec::new();
    if items.is_empty() {
        violations.push("items 不得为空（至少 1 个评分项）".to_owned());
    }
    if items.len() > MAX_ITEMS {
        violations.push(format!("items 数量 {} 超过上限 {MAX_ITEMS}", items.len()));
    }
    let length = items.len();
    let mut total: i64 = 0;
    let mut result: Vec<RubricDraftItem> = Vec::with_capacity(length);
    for (index, item) in items.into_iter().enumerate() {
        let ordinal = u32::try_from(index).map_or(u32::MAX, |value| value + 1);
        let criterion = item.criterion.trim().to_owned();
        if criterion.is_empty() {
            violations.push(format!("第 {ordinal} 项 criterion 为空"));
        } else if criterion.chars().count() > MAX_CRITERION_CHARS {
            violations.push(format!(
                "第 {ordinal} 项 criterion 长度 {} 超过 {MAX_CRITERION_CHARS}",
                criterion.chars().count()
            ));
        }
        if criterion.contains(TEMPLATE_CRITERION_MARKER) {
            violations.push(format!(
                "第 {ordinal} 项 criterion 含模板占位标记 {TEMPLATE_CRITERION_MARKER}，须替换为本题具体判据"
            ));
        }
        if (1..=MAX_ITEM_POINTS_MILLI).contains(&item.points) {
            total = total.saturating_add(item.points);
        } else {
            violations.push(format!(
                "第 {ordinal} 项 points {} 必须在 1..={MAX_ITEM_POINTS_MILLI}（千分之一分）",
                item.points
            ));
        }
        for dependency in &item.depends_on {
            if *dependency == 0 || usize::try_from(*dependency).map_or(true, |value| value > length)
            {
                violations.push(format!(
                    "第 {ordinal} 项 depends_on 引用 {dependency} 越界（评分项共 {length} 项，序号从 1 起）"
                ));
            } else if *dependency == ordinal {
                violations.push(format!("第 {ordinal} 项不得依赖自身"));
            }
        }
        result.push(RubricDraftItem {
            criterion,
            points: MilliScore::from_milli(item.points).unwrap_or(MilliScore::ZERO),
            required: item.required,
            depends_on: item.depends_on,
        });
    }
    if total > MAX_TOTAL_POINTS_MILLI {
        violations.push(format!(
            "分值合计 {total} 超过上限 {MAX_TOTAL_POINTS_MILLI}（千分之一分）"
        ));
    }
    if violations.is_empty()
        && let Some(cycle) = dependency_cycle(&result)
    {
        violations.push(format!(
            "depends_on 依赖成环：{}",
            cycle
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(" → ")
        ));
    }
    if violations.is_empty() {
        Ok(result)
    } else {
        Err(violations)
    }
}

/// 依赖环检测（1 起序号；返回环上的序号）。
fn dependency_cycle(items: &[RubricDraftItem]) -> Option<Vec<u32>> {
    let length = items.len();
    let mut state = vec![0_u8; length];
    let mut stack: Vec<u32> = Vec::new();

    fn visit(
        ordinal: u32,
        items: &[RubricDraftItem],
        state: &mut [u8],
        stack: &mut Vec<u32>,
    ) -> Option<Vec<u32>> {
        let index = usize::try_from(ordinal).ok()?;
        let position = index.checked_sub(1)?;
        if position >= items.len() {
            return None;
        }
        match state.get(position).copied().unwrap_or(0) {
            1 => {
                let start = stack
                    .iter()
                    .position(|value| *value == ordinal)
                    .unwrap_or(0);
                return Some(stack.get(start..).unwrap_or(&[]).to_vec());
            }
            2 => return None,
            _ => {}
        }
        if let Some(slot) = state.get_mut(position) {
            *slot = 1;
        }
        stack.push(ordinal);
        let dependencies: &[u32] = items
            .get(position)
            .map_or(&[], |item| item.depends_on.as_slice());
        for dependency in dependencies {
            if let Some(cycle) = visit(*dependency, items, state, stack) {
                return Some(cycle);
            }
        }
        stack.pop();
        if let Some(slot) = state.get_mut(position) {
            *slot = 2;
        }
        None
    }

    for item in 1..=u32::try_from(length).ok()? {
        let position = usize::try_from(item).ok()?.checked_sub(1)?;
        if state.get(position).copied() == Some(0)
            && let Some(cycle) = visit(item, items, &mut state, &mut stack)
        {
            return Some(cycle);
        }
    }
    None
}

/// 依据题型与依赖推断默认计分方式（模型不输出 `mode`；教师可随后编辑）。
#[must_use]
pub fn infer_mode(kind: QuestionKind, items: &[RubricDraftItem]) -> ScoringMode {
    let objective = matches!(
        kind,
        QuestionKind::SingleChoice
            | QuestionKind::MultipleChoice
            | QuestionKind::TrueFalse
            | QuestionKind::FillInBlank
            | QuestionKind::NumericOnly
    );
    if objective {
        if items.len() == 1 {
            ScoringMode::AllOrNothing
        } else {
            ScoringMode::Additive
        }
    } else if matches!(kind, QuestionKind::Calculation | QuestionKind::Proof)
        && items.iter().any(|item| !item.depends_on.is_empty())
    {
        ScoringMode::Stepwise
    } else {
        ScoringMode::Additive
    }
}

impl RubricEngine<'_> {
    /// 规则整理：规则原文 → 草稿（来源 `ModelStructured`）。
    ///
    /// 流程：渲染提示词 → 调用端口 → 严格 Schema 校验 →（不合法时）机械修复至多 1 次 →
    /// 组装 `RubricDraft` + 静态校验 + 审计。**只产出草稿**，生效仍需教师显式确认。
    ///
    /// # Errors
    ///
    /// 端口失败返回 [`RubricError::Structurer`]；Schema 两次均不合法返回
    /// [`RubricError::ModelOutputRejected`]（不生成草稿）；领域校验/存储/审计失败按原样返回。
    pub fn create_draft_from_model(
        &self,
        question: &Question,
        raw_text: &str,
        port: &dyn StructurerPort,
        template_skeleton: Option<&str>,
    ) -> Result<StoredDraft> {
        if raw_text.trim().is_empty() {
            return Err(RubricError::config("规则原文为空，无法发起规则整理"));
        }
        let request = StructurerRequest {
            raw_text,
            question_kind: question.kind,
            max_score: question.max_score,
            template_skeleton,
        };
        let prompt = render_structurer_prompt(&request);

        let first = port.structure(&StructurerInvocation {
            prompt: &prompt,
            request: &request,
            attempt: StructurerAttempt::Initial,
            previous_output: None,
            violations: &[],
        })?;
        let (items, attempts, model_call_id) = match parse_structurer_output(&first.raw_output) {
            Ok(items) => (items, 1_u8, first.model_call_id),
            Err(violations) => {
                let repaired = port.structure(&StructurerInvocation {
                    prompt: &prompt,
                    request: &request,
                    attempt: StructurerAttempt::Repair,
                    previous_output: Some(&first.raw_output),
                    violations: &violations,
                })?;
                let repaired_id = repaired.model_call_id;
                match parse_structurer_output(&repaired.raw_output) {
                    Ok(items) => (items, 1 + MAX_REPAIR_ATTEMPTS, repaired_id),
                    Err(violations) => {
                        return Err(RubricError::ModelOutputRejected {
                            model_call_id: Some(repaired_id),
                            prompt_hash: prompt.hash,
                            summary: summarize(&violations),
                            violations,
                            attempts: 1 + MAX_REPAIR_ATTEMPTS,
                        });
                    }
                }
            }
        };

        let mode = infer_mode(question.kind, &items);
        let stored = self.create_draft(
            question,
            DraftSource::ModelStructured {
                raw_text: raw_text.to_owned(),
                items,
                mode,
                partial_choice: None,
                model_call_id,
            },
        )?;
        // 结构整理调用审计（草稿写入后的补充事件；失败不撤销草稿，TODO(T18) 同事务）。
        let hash = content_hash(&stored)?;
        self.audit_sink().append(
            NewRubricAuditEvent::new(
                RubricAction::ModelStructured,
                stored.draft.id,
                stored.draft.question_id,
                eg_domain::LOCAL_TEACHER,
                self.clock_source().now(),
            )
            .with_hashes(None, Some(hash))
            .with_model_call(model_call_id)
            .with_prompt_hash(prompt.hash)
            .with_note(format!(
                "规则整理：尝试 {attempts} 次；题型 {:?}；原文 {} 字；Schema v{}",
                question.kind,
                raw_text.chars().count(),
                prompt.schema_version
            )),
        )?;
        debug_assert_eq!(stored.draft.origin, DraftOrigin::ModelStructured);
        Ok(stored)
    }
}

fn summarize(violations: &[String]) -> String {
    let head: Vec<&str> = violations.iter().take(3).map(String::as_str).collect();
    if violations.len() > head.len() {
        format!("{} 等 {} 条", head.join("；"), violations.len())
    } else {
        head.join("；")
    }
}

/// 端口未就绪时的占位说明（供 `app` 组合根接线时参考，不参与运行）。
pub(crate) const _INFERENCE_ADAPTER_NOTE: &str = "TODO(T25): app 用 eg-inference ExpectedSchema::Structurer 实现 StructurerPort；\
     本 crate 不依赖 inference，任务授权/预算/模型 id 全在 app/inference 侧。";
