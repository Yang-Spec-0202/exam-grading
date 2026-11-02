//! 判分流水线：单题判分 + 提交级汇总（`SPEC-grade.md` §3/§5/§7/§8，T16–T18）。
//!
//! 一次单题判分：就绪 → 归一化 → L1 路由（含 R9 分类占位）→ L2 规则链/模型判定
//! → 校验/聚合/冲突仲裁 → 门禁 → 判分记录（证据/引擎指纹/模式/状态）。
//! 提交级：逐题判分（单题失败不影响其余题）→ 快速模式抽检 → 汇总。
//!
//! 失败语义（规范 §9）：任何失败路径都不给 0 分成功、不静默丢题、不丢失模式标记。

use std::collections::BTreeSet;

use eg_domain::{
    AssetId, Clock, ComparatorTrace, Confidence, EngineStamp, EvidenceRef, ExcludeReason, GradeId,
    GradeMode, GradeRecord, GradeStatus, JudgedUnit, Judgement, ManualReason, MilliScore,
    ModelCall, ModelCallId, ModelRole, NormalizedAnswer, Paper, PaperTemplate, Question,
    QuestionId, RawAnswer, RecognitionId, ReviewReasonKind, RouteDecision, RouteTrace, Rubric,
    ScoringMode, Sha256Hex, SubjectPack, SubmissionId, TaskId, Timestamp, UngradableReason,
};

use crate::aggregate::{parse_option_letters, partial_choice};
use crate::config::{ConfigError, GradeConfig};
use crate::gate::{GateInput, compose_confidence, gate};
use crate::idempotent::idempotency_key;
use crate::l2::{RuleEvaluation, RuleRun, evaluate_rule_chain};
use crate::ports::{
    IdGenerator, IdempotencyKey, IdempotencyStore, JudgeBackend, JudgeFailure, JudgeRequest,
    JudgeResponse, RubricAvailability, RubricSource,
};
use crate::readiness::{ReadinessIssue, ReadinessReport, check_readiness};
use crate::route::{self, RouteInput, key_kind_conflict, manual_to_ungradable};
use crate::sampling::{SamplingCandidate, SamplingResult, sample_fast};
use crate::validate::{ConflictKind, ValidatedJudge, ValidationError, validate_judge_response};

/// 单题学生答案输入。
#[derive(Debug, Clone)]
pub struct QuestionAnswer {
    /// 题目（来自试卷，含满分/题型/答案键）。
    pub question: Question,
    /// 识别后的原始答案。
    pub raw: RawAnswer,
    /// 答案级识别置信度。
    pub confidence: Confidence,
    /// 识别结果证据（可空）。
    pub recognition: Option<RecognitionId>,
    /// 原图/裁片等资产证据（至少一个证据引用必需）。
    pub assets: Vec<AssetId>,
    /// 题型是否已知（未知触发 R9 分类澄清）。
    pub kind_known: bool,
}

impl QuestionAnswer {
    /// 构造（默认题型已知）。
    #[must_use]
    pub fn new(question: Question, raw: RawAnswer, confidence: Confidence) -> Self {
        Self {
            question,
            raw,
            confidence,
            recognition: None,
            assets: Vec::new(),
            kind_known: true,
        }
    }
}

/// 提交级判分请求。
#[derive(Debug)]
pub struct SubmissionRequest {
    /// 提交 id。
    pub submission_id: SubmissionId,
    /// 任务 id（模型调用授权/审计）。
    pub task_id: TaskId,
    /// 试卷（含题目、答案键/规则版本）。
    pub paper: Paper,
    /// 试卷模板（可选；提供时校验版本对齐）。
    pub template: Option<PaperTemplate>,
    /// 学生答案（可少于题目数；缺失按单题失败记录）。
    pub answers: Vec<QuestionAnswer>,
    /// 运行模式。
    pub mode: GradeMode,
    /// 判定后端是否可用。
    pub backend_available: bool,
    /// 教师明确标记「仅人工」的题目。
    pub manual_only: Vec<QuestionId>,
    /// 提交级排除原因（缺考/无效等；排除后不判分）。
    pub excluded: Option<ExcludeReason>,
    /// 外部引擎指纹（OCR/对齐/CAS 等版本；进幂等键与引擎指纹，变化即缓存失效）。
    pub engine_stamps: Vec<EngineStamp>,
}

/// 单题判分结果。
#[derive(Debug, Clone)]
pub struct GradedQuestion {
    /// 判分记录。
    pub record: GradeRecord,
    /// 比较轨迹（证据落库由平台完成）。
    pub traces: Vec<ComparatorTrace>,
    /// 模型调用记录（含真实推理服务已落库的调用；证据引用用）。
    pub model_calls: Vec<ModelCall>,
    /// 尚未落库的模型调用（fixture/回放生成的记录；平台 sink 负责落库）。
    pub unpersisted_model_calls: Vec<ModelCall>,
    /// 门禁阻塞原因。
    pub blockers: Vec<ReviewReasonKind>,
    /// 是否命中幂等键（未重复执行/计费）。
    pub idempotent_hit: bool,
    /// 门禁要求强制抽检（未校准模型快速放行）。
    pub force_sampled: bool,
}

/// 单题失败（不影响其余题；显式记录，不静默丢弃）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionFailure {
    /// 题目 id。
    pub question_id: QuestionId,
    /// 中文原因。
    pub message: String,
}

/// 提交级汇总（规范 §3「提交级结果」）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SubmissionSummary {
    /// 题目总数。
    pub total_questions: usize,
    /// 自动接受。
    pub auto_accepted: usize,
    /// 待复核（含人工路由/低置信/冲突/不可判定）。
    pub needs_review: usize,
    /// 判分失败（可重试）。
    pub failed: usize,
    /// 快速模式抽检数。
    pub sampled: usize,
    /// 提交级排除原因。
    pub excluded: Option<ExcludeReason>,
    /// 是否缺考。
    pub absent: bool,
}

/// 提交级判分结果。
#[derive(Debug, Clone)]
pub struct SubmissionGrade {
    /// 提交 id。
    pub submission_id: SubmissionId,
    /// 成功判分的题目（按题序）。
    pub records: Vec<GradedQuestion>,
    /// 失败题目。
    pub failures: Vec<QuestionFailure>,
    /// 汇总。
    pub summary: SubmissionSummary,
    /// 抽检结果（快速模式且存在自动接受候选时）。
    pub sampling: Option<SamplingResult>,
    /// 就绪报告（就绪时为空）。
    pub readiness: ReadinessReport,
}

/// 判分入口错误（整卷级；单题失败走 [`QuestionFailure`]）。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum GradeError {
    /// 配置非法。
    #[error("判分配置非法：{0}")]
    Config(#[from] ConfigError),
    /// 试卷未就绪：整卷拒绝判分。
    #[error("试卷未就绪（{} 个错误级问题）", .0.issues.len())]
    NotReady(ReadinessReport),
    /// 快速模式已关闭。
    #[error("快速模式已关闭（fast.enabled=off）")]
    FastModeDisabled,
    /// 学科不匹配。
    #[error("试卷学科与判分学科包不一致")]
    SubjectMismatch,
    /// 提交 id/试卷 id 不一致。
    #[error("提交与试卷不匹配")]
    SubmissionMismatch,
}

/// 判分器（依赖注入；全部为 trait，便于 fixture 测试与后续接线）。
pub struct Grader<'a> {
    /// 学科包（比较器链/归一化/静态校验）。
    pub subject: &'a dyn SubjectPack,
    /// 评分标准来源（仅已确认规则生效）。
    pub rubrics: &'a dyn RubricSource,
    /// 模型判定后端（fixture / inference 适配）。
    pub judge: &'a dyn JudgeBackend,
    /// 分类澄清后端（R9；可空）。
    pub classifier: Option<&'a dyn crate::ports::ClassifierBackend>,
    /// 判分配置。
    pub config: &'a GradeConfig,
    /// 时钟。
    pub clock: &'a dyn Clock,
    /// id 生成器。
    pub ids: &'a dyn IdGenerator,
    /// 幂等存储。
    pub idempotency: &'a dyn IdempotencyStore,
}

impl std::fmt::Debug for Grader<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Grader")
            .field("subject", &self.subject.id())
            .field("judge", &self.judge.identity())
            .field("mode_config", &self.config.hash())
            .finish()
    }
}

#[allow(
    clippy::unused_self,
    reason = "判分辅助方法通过 self 访问注入的配置/时钟/学科包，部分方法本身不直接读取 self 字段"
)]
impl Grader<'_> {
    /// 整份提交判分（单题失败不影响其余题；就绪失败整卷拒绝）。
    ///
    /// # Errors
    ///
    /// 配置非法、快速模式关闭、学科不匹配、试卷未就绪时返回 [`GradeError`]。
    pub fn grade_submission(
        &self,
        request: &SubmissionRequest,
    ) -> Result<SubmissionGrade, GradeError> {
        self.config.validate()?;
        if request.mode == GradeMode::Fast && !self.config.fast_enabled {
            return Err(GradeError::FastModeDisabled);
        }
        if request.paper.subject != self.subject.id() {
            return Err(GradeError::SubjectMismatch);
        }
        if let Some(excluded) = request.excluded {
            let absent = excluded == ExcludeReason::Absent;
            return Ok(SubmissionGrade {
                submission_id: request.submission_id,
                records: Vec::new(),
                failures: Vec::new(),
                summary: SubmissionSummary {
                    total_questions: request.paper.questions.len(),
                    excluded: Some(excluded),
                    absent,
                    ..SubmissionSummary::default()
                },
                sampling: None,
                readiness: ReadinessReport::default(),
            });
        }
        let readiness = check_readiness(
            &request.paper,
            request.template.as_ref(),
            self.rubrics,
            &request.manual_only,
            self.subject,
        );
        if !readiness.is_ready() {
            return Err(GradeError::NotReady(readiness));
        }
        // 再校验题目自身（就绪已检查，此处防止调用方构造异常试卷）
        if let Err(error) = request.paper.validate() {
            return Err(GradeError::NotReady(ReadinessReport {
                issues: vec![ReadinessIssue {
                    question_id: None,
                    kind: crate::readiness::ReadinessIssueKind::InvalidQuestion,
                    message: format!("试卷校验失败：{error}"),
                }],
            }));
        }
        let answer_map: std::collections::HashMap<QuestionId, &QuestionAnswer> = request
            .answers
            .iter()
            .map(|answer| (answer.question.id, answer))
            .collect();
        let mut records: Vec<GradedQuestion> = Vec::new();
        let mut failures: Vec<QuestionFailure> = Vec::new();
        for question in &request.paper.questions {
            let Some(answer) = answer_map.get(&question.id) else {
                failures.push(QuestionFailure {
                    question_id: question.id,
                    message: "缺少学生答案记录（不静默丢弃）".to_owned(),
                });
                continue;
            };
            match self.grade_question(request, answer) {
                Ok(graded) => records.push(graded),
                Err(failure) => failures.push(failure),
            }
        }
        // 快速模式抽检（仅自动接受候选；标记只读，不改分）
        let sampling = if request.mode == GradeMode::Fast {
            self.apply_sampling(request, &mut records)
        } else {
            None
        };
        let mut summary = SubmissionSummary {
            total_questions: request.paper.questions.len(),
            failed: failures.len(),
            sampled: sampling.as_ref().map_or(0, |result| result.sampled.len()),
            ..SubmissionSummary::default()
        };
        for record in &records {
            match record.record.status {
                GradeStatus::AutoAccepted => summary.auto_accepted += 1,
                GradeStatus::ManuallyReviewed | GradeStatus::Excluded => {}
                GradeStatus::NeedsReview => summary.needs_review += 1,
            }
        }
        Ok(SubmissionGrade {
            submission_id: request.submission_id,
            records,
            failures,
            summary,
            sampling,
            readiness,
        })
    }

    fn apply_sampling(
        &self,
        request: &SubmissionRequest,
        records: &mut [GradedQuestion],
    ) -> Option<SamplingResult> {
        let mut candidates = Vec::new();
        for (index, graded) in records.iter().enumerate() {
            if graded.record.status != GradeStatus::AutoAccepted {
                continue;
            }
            candidates.push((
                index,
                SamplingCandidate {
                    question_id: graded.record.question_id,
                    confidence: graded.record.confidence,
                    threshold: self
                        .config
                        .threshold(&graded.record.route, graded.record.mode),
                },
            ));
        }
        if candidates.is_empty() {
            return None;
        }
        let sampling_candidates: Vec<SamplingCandidate> = candidates
            .iter()
            .map(|(_, candidate)| candidate.clone())
            .collect();
        let default_seed = self.default_seed(request.submission_id);
        let result = sample_fast(&sampling_candidates, self.config, default_seed);
        let sampled_set: BTreeSet<QuestionId> = result.sampled.iter().copied().collect();
        for graded in records.iter_mut() {
            if graded.force_sampled || sampled_set.contains(&graded.record.question_id) {
                graded.record.sampled = true;
                // 重新校验（sampled 字段在快速模式合法）
                let _ = graded.record.validate();
            }
        }
        Some(result)
    }

    fn default_seed(&self, submission_id: SubmissionId) -> u64 {
        let digest = Sha256Hex::of(format!("{submission_id}").as_bytes());
        let bytes = digest.as_bytes();
        u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ])
    }

    // ------------------------------------------------------------------
    // 单题判分
    // ------------------------------------------------------------------

    fn grade_question(
        &self,
        request: &SubmissionRequest,
        answer: &QuestionAnswer,
    ) -> Result<GradedQuestion, QuestionFailure> {
        let base_evidence = self.base_evidence(answer);
        if base_evidence.is_empty() {
            return Err(QuestionFailure {
                question_id: answer.question.id,
                message: "缺少证据引用（原图/裁片/识别结果至少一项，无法落证据链）".to_owned(),
            });
        }
        let normalization = self.subject.normalize(&answer.raw);
        // 归一化失败仅在需要确定性比较（有具体答案键）时按 R1 转人工；
        // 「仅评分标准/仅人工」路由按原始文本交模型/人工，不因自由文本无法
        // 归一化而阻塞（否则所有论述题都会被 R1 拦截）。
        let (normalized, normalization_failed) = match &normalization {
            Ok(normalized) => (Some(normalized.clone()), false),
            Err(_) => (None, answer.question.key.has_concrete()),
        };
        let availability = self.rubrics.availability(answer.question.id);
        let confirmed_rubric = match &availability {
            RubricAvailability::Confirmed(rubric) => Some(rubric.clone()),
            RubricAvailability::Draft | RubricAvailability::Missing => None,
        };
        let answer_empty = answer.raw.text.trim().is_empty()
            && answer
                .raw
                .latex
                .as_deref()
                .is_none_or(|latex| latex.trim().is_empty())
            && answer.raw.blocks.is_empty();
        let mut route_input = RouteInput {
            kind: answer.question.kind,
            kind_known: answer.kind_known,
            key: answer.question.key.clone(),
            rubric_confirmed: confirmed_rubric.is_some(),
            answer_empty,
            recognition_confidence: answer.confidence,
            recognition_threshold: self.config.recognition_threshold,
            normalization_failed,
            backend_available: request.backend_available,
            key_kind_conflict: key_kind_conflict(answer.question.kind, &answer.question.key),
            manual_only: request.manual_only.contains(&answer.question.id),
        };
        // 幂等检查（不重复判分/计费）
        let idempotency_key = self.idempotency_key(request, answer);
        if let Some(record) = self.idempotency.lookup(&idempotency_key) {
            let blockers =
                record.auto_accept_blockers(self.config.threshold(&record.route, record.mode));
            return Ok(GradedQuestion {
                record,
                traces: Vec::new(),
                model_calls: Vec::new(),
                unpersisted_model_calls: Vec::new(),
                blockers,
                idempotent_hit: true,
                force_sampled: false,
            });
        }
        let mut route_outcome = route::route(&route_input);
        let mut model_calls: Vec<ModelCall> = Vec::new();
        let mut generated_classifier_calls: Vec<ModelCall> = Vec::new();
        let mut classifier_call: Option<ModelCallId> = None;
        if route_outcome.needs_classification {
            match self.classify(request, answer) {
                Ok((response, call, generated)) => {
                    classifier_call = Some(call.id);
                    if generated {
                        generated_classifier_calls.push(call.clone());
                    }
                    model_calls.push(call);
                    route_input.kind = response.kind;
                    route_input.kind_known = true;
                    route_input.key_kind_conflict =
                        key_kind_conflict(route_input.kind, &route_input.key);
                    route_outcome = route::route(&route_input);
                    if route_outcome.needs_classification {
                        route_outcome = route::RouteOutcome {
                            decision: RouteDecision::Manual(ManualReason::OutOfScope),
                            rule_id: "R9",
                            needs_classification: false,
                        };
                    }
                }
                Err(failure) => {
                    route_outcome = route::RouteOutcome {
                        decision: RouteDecision::Manual(ManualReason::BackendUnavailable),
                        rule_id: "R9",
                        needs_classification: false,
                    };
                    let _ = failure;
                }
            }
        }
        let graded = self.execute_route(
            request,
            answer,
            &route_input,
            route_outcome,
            normalized,
            confirmed_rubric,
            base_evidence,
            model_calls,
            generated_classifier_calls,
            classifier_call,
        )?;
        self.idempotency
            .store(idempotency_key, graded.record.clone());
        Ok(graded)
    }

    #[allow(
        clippy::too_many_arguments,
        clippy::needless_pass_by_value,
        reason = "路线执行需要完整上下文；部分参数按值传入便于与记录构建共享所有权"
    )]
    fn execute_route(
        &self,
        request: &SubmissionRequest,
        answer: &QuestionAnswer,
        route_input: &RouteInput,
        route_outcome: route::RouteOutcome,
        normalized: Option<NormalizedAnswer>,
        confirmed_rubric: Option<Rubric>,
        base_evidence: Vec<EvidenceRef>,
        mut model_calls: Vec<ModelCall>,
        generated_classifier_calls: Vec<ModelCall>,
        classifier_call: Option<ModelCallId>,
    ) -> Result<GradedQuestion, QuestionFailure> {
        let question = &answer.question;
        let decision = route_outcome.decision.clone();
        let rule_id = route_outcome.rule_id;
        let key = question.key.concrete().cloned();
        let max_score = question.max_score;
        // 规则段（仅当有具体答案键且非「仅评分标准」路由）
        let needs_rule = key.is_some()
            && matches!(
                decision,
                RouteDecision::RuleOnly
                    | RouteDecision::RuleThenModel
                    | RouteDecision::ModelDirect
                    | RouteDecision::RubricItemized
            );
        let rule_run = match (needs_rule, &key, &normalized) {
            (true, Some(key), Some(normalized)) => Some(
                evaluate_rule_chain(self.subject, question, key, normalized, self.ids).map_err(
                    |error| QuestionFailure {
                        question_id: question.id,
                        message: format!("比较器轨迹 id 生成失败：{error}"),
                    },
                )?,
            ),
            _ => None,
        };
        let mut unpersisted_model_calls: Vec<ModelCall> = generated_classifier_calls;
        let traces: Vec<ComparatorTrace> = rule_run
            .as_ref()
            .map_or_else(Vec::new, |run| run.traces.clone());
        let mut rule_evidence: Vec<EvidenceRef> = traces
            .iter()
            .map(|trace| EvidenceRef::ComparatorTrace(trace.id))
            .collect();
        let mut evidence = base_evidence;
        evidence.append(&mut rule_evidence);
        let mut engines: Vec<EngineStamp> =
            model_calls.iter().map(|call| call.engine.clone()).collect();
        let mut conflicts: Vec<ConflictKind> = Vec::new();
        let mut used_model = false;
        let mut calibrated = true;
        let mut evidence_confidence: Option<Confidence> = None;
        let judgement: Judgement;
        let mut reviewed_note: Option<String> = None;
        match &decision {
            RouteDecision::Manual(reason) => {
                judgement = Judgement::Ungradable {
                    reason: manual_to_ungradable(*reason),
                };
            }
            RouteDecision::RuleOnly => {
                if rule_id == "R0" {
                    judgement = Judgement::Mismatch {
                        score: MilliScore::ZERO,
                        reason: "未作答（学生答案为空）".to_owned(),
                        evidence: Vec::new(),
                    };
                } else {
                    judgement = self.rule_judgement(
                        question,
                        key.as_ref(),
                        confirmed_rubric.as_ref(),
                        rule_run.as_ref(),
                        normalized.as_ref(),
                    );
                }
            }
            RouteDecision::RuleThenModel
            | RouteDecision::ModelDirect
            | RouteDecision::RubricItemized => {
                let rule_conclusive = rule_run.as_ref().is_some_and(|run| {
                    !matches!(run.evaluation, RuleEvaluation::Undecidable { .. })
                });
                if decision == RouteDecision::RuleThenModel && rule_conclusive {
                    judgement = self.rule_judgement(
                        question,
                        key.as_ref(),
                        confirmed_rubric.as_ref(),
                        rule_run.as_ref(),
                        normalized.as_ref(),
                    );
                } else {
                    // 模型判定（含修复重试）
                    let judge_result = self.judge_with_retry(
                        request,
                        answer,
                        key.as_ref(),
                        confirmed_rubric.as_ref(),
                        normalized.as_ref(),
                        &mut model_calls,
                        &mut unpersisted_model_calls,
                    );
                    match judge_result {
                        Ok(validated) => {
                            used_model = true;
                            calibrated = validated.outcome.is_calibrated();
                            evidence_confidence = validated.evidence_confidence;
                            conflicts.extend(validated.conflicts.clone());
                            if !engines.contains(&validated.engine) {
                                engines.push(validated.engine.clone());
                            }
                            let model_judgement = validated.judgement.clone();
                            let model_call_ref = model_calls.last().map_or_else(Vec::new, |call| {
                                vec![EvidenceRef::ModelCall(call.id)]
                            });
                            for evidence_ref in model_call_ref {
                                evidence.push(evidence_ref);
                            }
                            // 冲突检测：规则段确定结论 vs 模型结论（§8）
                            if let Some(run) = &rule_run {
                                match (&run.evaluation, &model_judgement) {
                                    (
                                        RuleEvaluation::Mismatch { .. },
                                        Judgement::Match { .. } | Judgement::Partial { .. },
                                    ) => {
                                        conflicts.push(ConflictKind::RuleNotEqualModelMatch);
                                    }
                                    (
                                        RuleEvaluation::Match { .. },
                                        Judgement::Mismatch { .. } | Judgement::Partial { .. },
                                    ) => {
                                        conflicts.push(ConflictKind::RuleMatchModelNotMatch);
                                    }
                                    _ => {}
                                }
                            }
                            let rule_conflict = conflicts.iter().any(|conflict| {
                                matches!(
                                    conflict,
                                    ConflictKind::RuleNotEqualModelMatch
                                        | ConflictKind::RuleMatchModelNotMatch
                                )
                            });
                            let aggregate_conflict = conflicts.iter().any(|conflict| {
                                matches!(conflict, ConflictKind::AggregateMismatch { .. })
                            });
                            if aggregate_conflict {
                                // §8：聚合 ≠ 模型声明 → 采用聚合值（validated 已按聚合修正），记冲突进人工
                                reviewed_note = Some(
                                    "模型声明得分与规则聚合不一致：采用聚合值并转人工".to_owned(),
                                );
                                judgement = model_judgement;
                            } else if rule_conflict {
                                let rule_first = self.config.conflict_policy
                                    == crate::config::ConflictPolicy::RuleFirst
                                    && matches!(request.mode, GradeMode::Fast)
                                    && conflicts.iter().all(|conflict| {
                                        matches!(conflict, ConflictKind::RuleMatchModelNotMatch)
                                    });
                                if rule_first {
                                    reviewed_note = Some(
                                        "规则与模型冲突：规则优先（快速模式标记抽检）".to_owned(),
                                    );
                                    judgement = self.rule_judgement(
                                        question,
                                        key.as_ref(),
                                        confirmed_rubric.as_ref(),
                                        rule_run.as_ref(),
                                        normalized.as_ref(),
                                    );
                                } else {
                                    judgement = Judgement::Ungradable {
                                        reason: UngradableReason::Conflict,
                                    };
                                }
                            } else {
                                judgement = model_judgement;
                            }
                        }
                        Err(failure) => {
                            judgement = Judgement::Ungradable { reason: failure };
                        }
                    }
                }
            }
        }
        // 所有模型调用（含 R9 分类）都要有证据引用；域层不变量：
        // engines 非空时证据必须含 ModelCall。
        for call in &model_calls {
            let reference = EvidenceRef::ModelCall(call.id);
            if !evidence.contains(&reference) {
                evidence.push(reference);
            }
        }
        let confidence =
            self.compose_record_confidence(answer.confidence, used_model, evidence_confidence);
        let gate_outcome = gate(&GateInput {
            route: &decision,
            confidence,
            mode: request.mode,
            judgement: &judgement,
            used_model,
            calibrated,
            has_model_evidence: model_calls.iter().any(|call| call.role == ModelRole::Judge),
            has_evidence_block: evidence_confidence.is_some(),
            rule_evidence_required: matches!(
                decision,
                RouteDecision::RuleOnly | RouteDecision::RuleThenModel
            ) && rule_id != "R0",
            has_rule_evidence: !traces.is_empty(),
            conflicts: &conflicts,
            config: self.config,
        });
        let force_sampled = gate_outcome.force_sampled();
        let status = if gate_outcome.is_auto_accept() {
            GradeStatus::AutoAccepted
        } else {
            GradeStatus::NeedsReview
        };
        let blockers = gate_outcome.blockers();
        let awarded = judgement.score().unwrap_or(MilliScore::ZERO);
        let record = GradeRecord {
            id: GradeId::from_ulid(self.ids.try_next_ulid().map_err(|error| QuestionFailure {
                question_id: question.id,
                message: format!("判分记录 id 生成失败：{error}"),
            })?),
            submission_id: request.submission_id,
            question_id: question.id,
            revision: 1,
            mode: request.mode,
            route: decision,
            route_trace: RouteTrace {
                rule_id: rule_id.to_owned(),
                inputs_hash: self.route_inputs_hash(route_input),
                classifier_call,
                decided_at: self.clock.now(),
            },
            judgement,
            awarded,
            max_score,
            confidence,
            status,
            evidence,
            engines,
            created_at: self.clock.now(),
            reviewed_at: None,
            review_note: reviewed_note,
            sampled: false,
        };
        record.validate().map_err(|error| QuestionFailure {
            question_id: question.id,
            message: format!("判分记录校验失败（内部错误）：{error}"),
        })?;
        Ok(GradedQuestion {
            record,
            traces,
            model_calls,
            unpersisted_model_calls,
            blockers,
            idempotent_hit: false,
            force_sampled,
        })
    }

    fn rule_judgement(
        &self,
        question: &Question,
        key: Option<&eg_domain::ConcreteKey>,
        rubric: Option<&Rubric>,
        rule_run: Option<&RuleRun>,
        normalized: Option<&NormalizedAnswer>,
    ) -> Judgement {
        let max_score = question.max_score;
        let evaluation = rule_run.map_or_else(
            || RuleEvaluation::Undecidable {
                reason: "没有可执行的规则段".to_owned(),
            },
            |run| run.evaluation.clone(),
        );
        match evaluation {
            RuleEvaluation::Match { alternative } => Judgement::Match {
                score: max_score,
                reason: format!("比较器判定一致（候选 {}）", alternative + 1),
                evidence: Vec::new(),
            },
            RuleEvaluation::Mismatch { reason } => {
                if let (Some(rubric), Some(key), Some(normalized)) = (rubric, key, normalized)
                    && rubric.mode == ScoringMode::PartialChoice
                    && question.kind == eg_domain::QuestionKind::MultipleChoice
                {
                    return self.partial_choice_judgement(question, key, rubric, normalized);
                }
                Judgement::Mismatch {
                    score: MilliScore::ZERO,
                    reason,
                    evidence: Vec::new(),
                }
            }
            RuleEvaluation::Undecidable { .. } => Judgement::Ungradable {
                reason: UngradableReason::OutOfScope,
            },
        }
    }

    fn partial_choice_judgement(
        &self,
        _question: &Question,
        key: &eg_domain::ConcreteKey,
        rubric: &Rubric,
        normalized: &NormalizedAnswer,
    ) -> Judgement {
        let Some(rule) = rubric.partial_choice.as_ref() else {
            return Judgement::Ungradable {
                reason: UngradableReason::OutOfScope,
            };
        };
        let eg_domain::AnswerValue::Options(correct_options) = &key.primary else {
            return Judgement::Ungradable {
                reason: UngradableReason::OutOfScope,
            };
        };
        let Some(selected) = parse_option_letters(&normalized.text) else {
            return Judgement::Ungradable {
                reason: UngradableReason::OutOfScope,
            };
        };
        match partial_choice(rule, correct_options, &selected) {
            Ok(outcome) => {
                let max_score = rule.total_milli;
                if outcome.wrong_selection || outcome.score.is_zero() {
                    return Judgement::Mismatch {
                        score: MilliScore::ZERO,
                        reason: if outcome.wrong_selection {
                            "多选错选归零".to_owned()
                        } else {
                            "多选未命中正确选项".to_owned()
                        },
                        evidence: Vec::new(),
                    };
                }
                if outcome.score == max_score {
                    return Judgement::Match {
                        score: max_score,
                        reason: "多选全对".to_owned(),
                        evidence: Vec::new(),
                    };
                }
                let satisfied: Vec<JudgedUnit> = outcome
                    .satisfied
                    .iter()
                    .map(|option| JudgedUnit::Choice(option.clone()))
                    .collect();
                let missing: Vec<JudgedUnit> = outcome
                    .missing
                    .iter()
                    .map(|option| JudgedUnit::Choice(option.clone()))
                    .collect();
                Judgement::Partial {
                    score: outcome.score,
                    satisfied,
                    missing,
                    reason: format!(
                        "多选按选项部分给分：命中 {} 个正确选项{}",
                        outcome.satisfied.len(),
                        outcome
                            .tier_selected_correct
                            .map_or_else(String::new, |count| format!("（档位 {count}）"))
                    ),
                }
            }
            Err(_) => Judgement::Ungradable {
                reason: UngradableReason::OutOfScope,
            },
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "判定重试需要完整请求上下文与落库/未落库调用双列表"
    )]
    fn judge_with_retry(
        &self,
        request: &SubmissionRequest,
        answer: &QuestionAnswer,
        key: Option<&eg_domain::ConcreteKey>,
        rubric: Option<&Rubric>,
        normalized: Option<&NormalizedAnswer>,
        model_calls: &mut Vec<ModelCall>,
        unpersisted_model_calls: &mut Vec<ModelCall>,
    ) -> Result<ValidatedJudge, UngradableReason> {
        let candidate_count = key.map_or(1, |key| {
            u32::try_from(key.alternatives.len() + 1).unwrap_or(u32::MAX)
        });
        let mut last_error: Option<ValidationError> = None;
        let max_attempts = self.config.max_repair_attempts.saturating_add(1);
        for attempt in 0..max_attempts {
            let judge_request = JudgeRequest {
                submission_id: request.submission_id,
                task_id: request.task_id,
                question: &answer.question,
                key,
                rubric,
                answer: &answer.raw,
                normalized,
                recognition_confidence: answer.confidence,
                mode: request.mode,
                attempt,
                repair_hint: last_error.as_ref().map(ValidationError::reason),
                granularity: self.config.score_granularity,
                max_reason_chars: self.config.max_reason_chars,
                judge_timeout_s: self.config.judge_timeout_s,
            };
            let response = match self.judge.judge(&judge_request) {
                Ok(response) => response,
                Err(failure) => match failure {
                    JudgeFailure::Timeout => return Err(UngradableReason::Timeout),
                    JudgeFailure::BackendUnavailable { .. } => {
                        return Err(UngradableReason::BackendUnavailable);
                    }
                    JudgeFailure::InvalidOutput { reason } => {
                        if attempt + 1 < max_attempts {
                            last_error = Some(ValidationError::Schema(reason));
                            continue;
                        }
                        return Err(UngradableReason::InvalidSchema);
                    }
                    JudgeFailure::PolicyBlocked { .. } => {
                        return Err(UngradableReason::PolicyBlocked);
                    }
                    JudgeFailure::Internal { .. } => return Err(UngradableReason::OutOfScope),
                },
            };
            match validate_judge_response(&response, &judge_request, rubric, candidate_count) {
                Ok(validated) => {
                    let (call, generated) = if let Some(call) = response.model_call.clone() {
                        (call, false)
                    } else {
                        (
                            self.model_call(request, answer, normalized, &response)?,
                            true,
                        )
                    };
                    if generated {
                        unpersisted_model_calls.push(call.clone());
                    }
                    model_calls.push(call);
                    return Ok(validated);
                }
                Err(error) => {
                    if !error.is_repairable() {
                        return Err(UngradableReason::InvalidSchema);
                    }
                    last_error = Some(error);
                }
            }
        }
        Err(UngradableReason::InvalidSchema)
    }

    fn model_call(
        &self,
        request: &SubmissionRequest,
        answer: &QuestionAnswer,
        normalized: Option<&NormalizedAnswer>,
        response: &JudgeResponse,
    ) -> Result<ModelCall, UngradableReason> {
        let material = format!(
            "{}:{}:{}",
            answer.question.id,
            answer.question.label,
            normalized.map_or_else(String::new, |value| value.text.clone())
        );
        let call = ModelCall {
            id: ModelCallId::from_ulid(
                self.ids
                    .try_next_ulid()
                    .map_err(|_| UngradableReason::InvalidSchema)?,
            ),
            role: ModelRole::Judge,
            engine: response.engine.clone(),
            task_id: request.task_id,
            consent_id: None,
            input_hash: Sha256Hex::of(material.as_bytes()),
            output: response.output.clone(),
            latency_ms: response.latency_ms,
            cost_micros: response.cost_micros,
            created_at: self.clock.now(),
        };
        call.validate().map_err(|error| {
            let _ = error;
            UngradableReason::InvalidSchema
        })?;
        Ok(call)
    }

    fn classify(
        &self,
        request: &SubmissionRequest,
        answer: &QuestionAnswer,
    ) -> Result<(crate::ports::ClassificationResponse, ModelCall, bool), JudgeFailure> {
        if !request.backend_available {
            return Err(JudgeFailure::BackendUnavailable {
                reason: "R9 分类需要模型但后端不可用".to_owned(),
            });
        }
        let Some(classifier) = self.classifier else {
            return Err(JudgeFailure::BackendUnavailable {
                reason: "未配置题型分类后端".to_owned(),
            });
        };
        let response = classifier.classify(&answer.question, request.task_id)?;
        if response.confidence.get() < 0.9 {
            return Err(JudgeFailure::Internal {
                reason: "分类置信度不足 0.9，转人工".to_owned(),
            });
        }
        if let Some(call) = response.model_call.clone() {
            call.validate().map_err(|error| JudgeFailure::Internal {
                reason: format!("分类调用证据非法：{error}"),
            })?;
            return Ok((response, call, false));
        }
        let call = ModelCall {
            id: ModelCallId::from_ulid(self.ids.try_next_ulid().map_err(|error| {
                JudgeFailure::Internal {
                    reason: format!("分类调用 id 生成失败：{error}"),
                }
            })?),
            role: ModelRole::Router,
            engine: response.engine.clone(),
            task_id: request.task_id,
            consent_id: None,
            input_hash: Sha256Hex::of(format!("classify:{}", answer.question.id).as_bytes()),
            output: serde_json::json!({
                "kind": format!("{:?}", response.kind),
                "confidence": response.confidence.get(),
            }),
            latency_ms: response.latency_ms,
            cost_micros: 0,
            created_at: self.clock.now(),
        };
        call.validate().map_err(|error| JudgeFailure::Internal {
            reason: format!("分类调用证据非法：{error}"),
        })?;
        Ok((response, call, true))
    }

    fn compose_record_confidence(
        &self,
        recognition: Confidence,
        used_model: bool,
        evidence_confidence: Option<Confidence>,
    ) -> Confidence {
        if used_model {
            compose_confidence(
                recognition,
                Some(evidence_confidence.unwrap_or(recognition)),
                evidence_confidence,
            )
        } else {
            recognition
        }
    }

    fn base_evidence(&self, answer: &QuestionAnswer) -> Vec<EvidenceRef> {
        let mut evidence = Vec::new();
        if let Some(recognition) = answer.recognition {
            evidence.push(EvidenceRef::Recognition(recognition));
        }
        for asset in &answer.assets {
            evidence.push(EvidenceRef::Asset(*asset));
        }
        evidence
    }

    fn idempotency_key(
        &self,
        request: &SubmissionRequest,
        answer: &QuestionAnswer,
    ) -> IdempotencyKey {
        idempotency_key(
            request.submission_id,
            answer.question.id,
            request.paper.key_version,
            request
                .paper
                .rubric_version
                .max(self.rubrics.active_version(answer.question.id)),
            &answer.raw,
            &self.engine_fingerprint(request),
            request.mode,
        )
    }

    /// 引擎指纹（公开给组合根/断点：含配置、学科能力、规则来源、判定/分类后端与外部版本）。
    #[must_use]
    pub fn engine_fingerprint_for(&self, engine_stamps: &[EngineStamp]) -> String {
        let request = SubmissionRequest {
            submission_id: SubmissionId::from_ulid(eg_domain::Ulid::NIL),
            task_id: TaskId::from_ulid(eg_domain::Ulid::NIL),
            paper: eg_domain::Paper {
                id: eg_domain::PaperId::from_ulid(eg_domain::Ulid::NIL),
                classroom_id: eg_domain::ClassroomId::from_ulid(eg_domain::Ulid::NIL),
                title: String::new(),
                subject: self.subject.id(),
                total_score: MilliScore::ZERO,
                questions: Vec::new(),
                key_version: 1,
                rubric_version: 1,
                created_at: self.clock.now(),
            },
            template: None,
            answers: Vec::new(),
            mode: GradeMode::Standard,
            backend_available: true,
            manual_only: Vec::new(),
            excluded: None,
            engine_stamps: engine_stamps.to_vec(),
        };
        self.engine_fingerprint(&request)
    }

    fn engine_fingerprint(&self, request: &SubmissionRequest) -> String {
        let capabilities = self.subject.capabilities();
        let mut stamps: Vec<String> = request
            .engine_stamps
            .iter()
            .map(|stamp| {
                format!(
                    "{}:{:?}:{}:{}:{}:{}",
                    stamp.role as u8 as char as u8,
                    stamp.backend,
                    stamp.model_id,
                    stamp.model_version,
                    stamp
                        .prompt_hash
                        .map_or_else(|| "-".to_owned(), eg_domain::Sha256Hex::to_hex),
                    stamp.params_hash.to_hex()
                )
            })
            .collect();
        stamps.sort();
        let classifier = self.classifier.map_or_else(
            || "none".to_owned(),
            super::ports::ClassifierBackend::identity,
        );
        let material = format!(
            "config={};subject={};prompt={};comparators={:?};judge={};classifier={};rubric={};stamps={:?}",
            self.config.hash(),
            self.subject.id(),
            capabilities.prompt_version,
            capabilities
                .comparators
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            self.judge.identity(),
            classifier,
            self.rubrics.identity(),
            stamps
        );
        Sha256Hex::of(material.as_bytes()).to_hex()
    }

    fn route_inputs_hash(&self, input: &RouteInput) -> Sha256Hex {
        let material = format!(
            "kind={:?};kind_known={};rubric_confirmed={};empty={};confidence={};threshold={};\
             normalization_failed={};backend={};conflict={};manual_only={};key={:?}",
            input.kind,
            input.kind_known,
            input.rubric_confirmed,
            input.answer_empty,
            input.recognition_confidence.get(),
            input.recognition_threshold.get(),
            input.normalization_failed,
            input.backend_available,
            input.key_kind_conflict,
            input.manual_only,
            input.key
        );
        Sha256Hex::of(material.as_bytes())
    }
}

/// 供测试/上报的时间戳辅助。
#[must_use]
pub fn timestamp_of(clock: &dyn Clock) -> Timestamp {
    clock.now()
}
