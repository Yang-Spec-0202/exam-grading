//! 数学学科包：实现 `eg-domain` 的 `SubjectPack` 契约（`SPEC-subject-math.md` §2）。
//!
//! 能力声明必须与实际实现一致（禁止虚报）：
//! - 支持全部 10 种题型（归一化与基础比较可用；主观题仍需 L2 模型，由 `grade` 路由决定）；
//! - 比较器列表只列已实现项（T14 基础 + T26 三角/向量/矩阵/量纲 + CAS 三角）；
//! - CAS 通过 [`crate::cas::CasClient`] 注入：未配置/不可用/超时/崩溃 → 相关比较器
//!   返回「无法判定」；CAS 版本进 [`MathSubjectPack::engine_fingerprint`] 与
//!   [`MathSubjectPack::cas_engine_stamp`]（供 `grade` 写入证据 engines）；
//! - `supports_step_scoring = false`（分步证据抽取属 T15，后续里程碑）；
//! - 静态校验实现 §9 的可判定子集（分值/选项/数值/维度/容差/证明题提示 +
//!   「三角/矩阵题依赖 CAS 但 CAS 不可用」警告），完整 11 条规则由 T15 补齐。

use std::sync::Arc;

use eg_domain::{
    AnswerKey, AnswerValue, Comparator, ComparatorId, ConcreteKey, EngineStamp, NormalizedAnswer,
    Question, QuestionKind, RawAnswer, Rubric, RubricIssue, Sha256Hex, StepEvidence,
    SubjectCapabilities, SubjectId, SubjectPack,
};

use crate::ast::Expr;
use crate::cas::{CasClient, CasFingerprint, CasStats};
use crate::comparators::{
    Expected, IMPLEMENTED_COMPARATOR_IDS, comparator_chain_for_with_cas, expected_from_value,
};
use crate::normalize::normalize_raw;

/// 判定配置/提示词版本（与 `SPEC-subject-math.md` §8 绑定；提示词实现属 T17）。
pub const JUDGE_PROFILE_ID: &str = "math-judge-v1";

/// 提示词版本（升版本属 Ask first；T17 前为占位版本）。
pub const PROMPT_VERSION: &str = "math-judge-v1";

/// CAS 运行时状态（供 UI/静态校验提示；不触发网络/模型调用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CasStatus {
    /// 是否启用。
    pub enabled: bool,
    /// 是否可用（已探测到版本）。
    pub available: bool,
    /// sympy 版本。
    pub version: Option<String>,
    /// 不可用原因。
    pub reason: Option<String>,
}

impl CasStatus {
    /// 是否「依赖 CAS 的题会降级」。
    #[must_use]
    pub fn degrades(&self) -> bool {
        self.enabled && !self.available
    }
}

/// 学科引擎指纹：提示词版本 + 比较器集合 + CAS 指纹（CAS 版本变化需重放/复核）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MathEngineFingerprint {
    /// 提示词版本。
    pub prompt_version: String,
    /// 比较器 id 列表（顺序即链优先级）。
    pub comparators: Vec<String>,
    /// CAS 指纹（未启用/未探测时为 `None`）。
    pub cas: Option<CasFingerprint>,
}

impl MathEngineFingerprint {
    /// 规范文本（稳定排序，供哈希/审计）。
    #[must_use]
    pub fn to_text(&self) -> String {
        let cas = self.cas.as_ref().map_or_else(
            || "cas=none".to_owned(),
            |cas| {
                format!(
                    "cas=protocol:{};version:{};command_hash:{}",
                    cas.protocol,
                    cas.version.as_deref().unwrap_or("unknown"),
                    cas.command_hash
                )
            },
        );
        format!(
            "prompt={};comparators={};{cas}",
            self.prompt_version,
            self.comparators.join(",")
        )
    }

    /// 指纹哈希（SHA-256）。
    #[must_use]
    pub fn hash(&self) -> Sha256Hex {
        Sha256Hex::of(self.to_text().as_bytes())
    }
}

/// 数学学科包（CAS 客户端可注入；默认从环境变量探测，未配置则 CAS 不可用）。
#[derive(Debug, Clone, Default)]
pub struct MathSubjectPack {
    cas: Option<Arc<CasClient>>,
}

impl MathSubjectPack {
    /// 构造学科包（从环境变量探测 CAS；不联网，仅本地 sidecar）。
    #[must_use]
    pub fn new() -> Self {
        Self {
            cas: CasClient::from_env().map(Arc::new),
        }
    }

    /// 构造不带 CAS 的学科包（离线/测试路径：相关比较器返回无法判定）。
    #[must_use]
    pub fn without_cas() -> Self {
        Self { cas: None }
    }

    /// 注入 CAS 客户端（测试 stub 或平台按 `cas.*` 配置构造）。
    #[must_use]
    pub fn with_cas(client: Arc<CasClient>) -> Self {
        Self { cas: Some(client) }
    }

    /// CAS 客户端（未配置时为 `None`）。
    #[must_use]
    pub fn cas(&self) -> Option<&Arc<CasClient>> {
        self.cas.as_ref()
    }

    /// CAS 运行时状态（含版本探测；本地子进程，无网络）。
    #[must_use]
    pub fn cas_status(&self) -> CasStatus {
        match &self.cas {
            None => CasStatus {
                enabled: false,
                available: false,
                version: None,
                reason: Some("CAS 未启用（未提供 sidecar 配置）".to_owned()),
            },
            Some(client) => {
                let enabled = client.config().enabled;
                match client.ensure_version() {
                    Some(version) => CasStatus {
                        enabled,
                        available: true,
                        version: Some(version),
                        reason: None,
                    },
                    None => CasStatus {
                        enabled,
                        available: false,
                        version: None,
                        reason: Some("CAS 不可用（sidecar 启动失败/超时/未安装）".to_owned()),
                    },
                }
            }
        }
    }

    /// CAS 运行统计（诊断）。
    #[must_use]
    pub fn cas_stats(&self) -> Option<CasStats> {
        self.cas.as_ref().map(|client| client.stats())
    }

    /// 引擎指纹（提示词版本 + 比较器集合 + CAS 版本/命令哈希）。
    #[must_use]
    pub fn engine_fingerprint(&self) -> MathEngineFingerprint {
        MathEngineFingerprint {
            prompt_version: PROMPT_VERSION.to_owned(),
            comparators: IMPLEMENTED_COMPARATOR_IDS
                .iter()
                .map(|id| (*id).to_owned())
                .collect(),
            cas: self.cas.as_ref().map(|client| client.fingerprint()),
        }
    }

    /// CAS 的 `EngineStamp`（供 `grade` 追加到证据 engines；版本未探测到则 `None`）。
    #[must_use]
    pub fn cas_engine_stamp(&self) -> Option<EngineStamp> {
        self.cas.as_ref()?.engine_stamp()
    }

    fn capabilities_inner() -> SubjectCapabilities {
        SubjectCapabilities {
            kinds: vec![
                QuestionKind::SingleChoice,
                QuestionKind::MultipleChoice,
                QuestionKind::TrueFalse,
                QuestionKind::FillInBlank,
                QuestionKind::NumericOnly,
                QuestionKind::Calculation,
                QuestionKind::Proof,
                QuestionKind::ShortAnswer,
                QuestionKind::Essay,
                QuestionKind::Other,
            ],
            comparators: IMPLEMENTED_COMPARATOR_IDS
                .iter()
                .filter_map(|id| ComparatorId::new(*id).ok())
                .collect(),
            prompt_version: PROMPT_VERSION.to_owned(),
            supports_step_scoring: false,
        }
    }
}

impl SubjectPack for MathSubjectPack {
    fn id(&self) -> SubjectId {
        SubjectId::math()
    }

    fn display_name(&self) -> String {
        "数学".to_owned()
    }

    fn capabilities(&self) -> SubjectCapabilities {
        Self::capabilities_inner()
    }

    fn normalize(&self, raw: &RawAnswer) -> Result<NormalizedAnswer, eg_domain::NormalizeError> {
        normalize_raw(raw)
    }

    fn comparator_chain(&self, key: &ConcreteKey, kind: QuestionKind) -> Vec<Box<dyn Comparator>> {
        comparator_chain_for_with_cas(key, kind, self.cas.as_ref())
    }

    fn step_evidence(&self, _rubric: &Rubric, _answer: &NormalizedAnswer) -> Option<StepEvidence> {
        // 分步证据抽取属 T15（本里程碑不实现，能力声明为 false，禁止虚报）。
        None
    }

    fn judge_profile_id(&self) -> String {
        JUDGE_PROFILE_ID.to_owned()
    }

    fn validate_question(&self, question: &Question) -> Vec<RubricIssue> {
        let mut issues = Vec::new();
        validate_answer_key(question, &mut issues);
        validate_rubric(question, &mut issues);
        self.validate_cas_dependency(question, &mut issues);
        issues
    }
}

impl MathSubjectPack {
    /// §9 警告：「三角/矩阵题依赖 CAS 但 CAS 当前不可用」。
    fn validate_cas_dependency(&self, question: &Question, issues: &mut Vec<RubricIssue>) {
        let AnswerKey::Concrete(concrete) = &question.key else {
            return;
        };
        let Ok(Expected::Expr(expr)) = expected_from_value(&concrete.primary) else {
            return;
        };
        let depends_on_cas =
            crate::trig::expr_has_trig(&expr) || matches!(expr, Expr::Matrix(_) | Expr::Vector(_));
        if !depends_on_cas {
            return;
        }
        let status = self.cas_status();
        if !status.available {
            issues.push(RubricIssue::warning(format!(
                "三角/向量/矩阵题依赖 CAS，但 CAS 当前不可用：{}（相关判定将降级为无法判定）",
                status.reason.as_deref().unwrap_or("未知原因")
            )));
        }
    }
}

fn validate_answer_key(question: &Question, issues: &mut Vec<RubricIssue>) {
    match &question.key {
        AnswerKey::Concrete(concrete) => {
            let expected = match expected_from_value(&concrete.primary) {
                Ok(expected) => expected,
                Err(reason) => {
                    issues.push(RubricIssue::error(format!("具体答案键无法解析：{reason}")));
                    return;
                }
            };
            match question.kind {
                QuestionKind::SingleChoice | QuestionKind::MultipleChoice => match &expected {
                    Expected::Options(options) => {
                        let mut seen: Vec<&str> = Vec::new();
                        for option in options {
                            if seen.contains(&option.as_str()) {
                                issues
                                    .push(RubricIssue::error(format!("选择题选项重复：{option}")));
                            }
                            seen.push(option);
                        }
                        if question.kind == QuestionKind::SingleChoice && options.len() != 1 {
                            issues.push(RubricIssue::error("单选题答案键必须恰好 1 个选项"));
                        }
                    }
                    _ => issues.push(RubricIssue::error("选择题答案键必须为选项集合（A–Z 字母）")),
                },
                QuestionKind::TrueFalse => match &expected {
                    Expected::Boolean(_) => {}
                    Expected::Options(options) if options.len() == 1 => {}
                    _ => issues.push(RubricIssue::error(
                        "判断题答案键必须为对/错（或单个 T/F 选项）",
                    )),
                },
                QuestionKind::NumericOnly => match &expected {
                    Expected::Number { .. } => {}
                    Expected::Expr(expr) => {
                        if crate::canonical::evaluate(expr).is_err() {
                            issues.push(RubricIssue::error("纯数值题答案键必须是可精确求值的数值"));
                        }
                    }
                    _ => issues.push(RubricIssue::error("纯数值题答案键必须是数值")),
                },
                _ => {}
            }
            // 矩阵/向量维度声明检查（规范 §9「矩阵/向量题未声明维度」）
            if let Expected::Expr(expr) = &expected {
                check_dimensions(expr, issues);
            }
        }
        AnswerKey::RubricOnly => {
            if question.rubric.is_none() {
                issues.push(RubricIssue::error(
                    "答案键为「仅评分标准」时必须提供生效评分标准",
                ));
            }
        }
        AnswerKey::ExplicitlyOmitted { .. } | AnswerKey::Missing => {}
    }
}

fn validate_rubric(question: &Question, issues: &mut Vec<RubricIssue>) {
    if let Some(rubric) = &question.rubric {
        if rubric.mode == eg_domain::ScoringMode::Stepwise && rubric.items.len() < 2 {
            issues.push(RubricIssue::error(
                "分步给分评分项少于 2 个（无法体现步骤）",
            ));
        }
        if rubric.validate(question.kind).is_err() {
            // 领域层不变量错误已由 rubric 模块报告；此处只提示去规则页修正
            issues.push(RubricIssue::error(
                "评分标准未通过不变量校验，请先在规则页修正",
            ));
        }
    } else if question.kind == QuestionKind::Proof {
        issues.push(RubricIssue::warning(
            "证明题没有评分标准，只能由 L2 模型判定，质量风险较高",
        ));
    }
    if let AnswerKey::Concrete(concrete) = &question.key {
        check_relative_tolerance(&concrete.primary, issues);
    }
}

fn check_relative_tolerance(value: &AnswerValue, issues: &mut Vec<RubricIssue>) {
    if let AnswerValue::Number {
        tolerance: Some(tolerance),
        ..
    } = value
        && let Some(ppm) = tolerance.relative_ppm
    {
        // 规范 §9：相对容差 > 2% 记警告
        if ppm > 20_000 {
            issues.push(RubricIssue::warning(format!(
                "相对容差 {ppm} ppm 超过 2%，可能掩盖真实错误"
            )));
        }
    }
}

/// 维度检查（矩阵/向量题必须声明维度：行列/分量非空且一致）。
fn check_dimensions(expr: &Expr, issues: &mut Vec<RubricIssue>) {
    match expr {
        Expr::Matrix(rows) => {
            if rows.is_empty() {
                issues.push(RubricIssue::error("矩阵答案键没有数据行（未声明维度）"));
                return;
            }
            let columns = rows.first().map_or(0, Vec::len);
            if columns == 0 {
                issues.push(RubricIssue::error("矩阵答案键没有列（未声明维度）"));
            }
            if rows.iter().any(|row| row.len() != columns) {
                issues.push(RubricIssue::error("矩阵答案键各行列数不一致"));
            }
        }
        Expr::Vector(items) => {
            if items.is_empty() {
                issues.push(RubricIssue::error("向量答案键没有分量（未声明维度）"));
            } else if items.len() > 3 {
                issues.push(RubricIssue::warning(
                    "向量维度超过 3 维，需确认是否为本轮支持范围",
                ));
            }
        }
        Expr::Sum(items)
        | Expr::Product(items)
        | Expr::Solutions(items)
        | Expr::Set(items)
        | Expr::SetUnion(items)
        | Expr::SetIntersection(items) => {
            for item in items {
                check_dimensions(item, issues);
            }
        }
        Expr::Power(base, exponent) => {
            check_dimensions(base, issues);
            check_dimensions(exponent, issues);
        }
        Expr::Fraction(numerator, denominator) => {
            check_dimensions(numerator, issues);
            check_dimensions(denominator, issues);
        }
        Expr::Absolute(value) | Expr::Quantity { value, .. } => check_dimensions(value, issues),
        Expr::Radical { radicand, degree } => {
            check_dimensions(radicand, issues);
            if let Some(degree) = degree {
                check_dimensions(degree, issues);
            }
        }
        Expr::Relation { lhs, rhs, .. } => {
            check_dimensions(lhs, issues);
            check_dimensions(rhs, issues);
        }
        Expr::Number(_)
        | Expr::Variable(_)
        | Expr::Function { .. }
        | Expr::Interval(_)
        | Expr::Boolean(_)
        | Expr::Option(_) => {}
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use eg_domain::{
        AnswerSource, MilliScore, PaperId, QuestionId, RubricId, RubricItem, RubricItemId,
        RubricOrigin, ScoringMode, Timestamp, Ulid,
    };

    fn timestamp() -> Timestamp {
        eg_domain::time::parse_rfc3339("2026-11-02T00:00:00Z").unwrap()
    }

    fn question(kind: QuestionKind, key: AnswerKey) -> Question {
        Question {
            id: QuestionId::from_ulid(Ulid::from_u128(1)),
            paper_id: PaperId::from_ulid(Ulid::from_u128(2)),
            ordinal: 1,
            label: "1".to_owned(),
            kind,
            max_score: MilliScore::from_milli(5_000).unwrap(),
            stem: None,
            key,
            rubric: None,
            subject_hint: None,
            knowledge_tags: Vec::new(),
        }
    }

    fn options(values: &[&str]) -> AnswerKey {
        AnswerKey::Concrete(ConcreteKey {
            primary: AnswerValue::Options(values.iter().map(|value| (*value).to_owned()).collect()),
            alternatives: Vec::new(),
            comparators: Vec::new(),
        })
    }

    #[test]
    fn subject_pack_basics() {
        let pack = MathSubjectPack::new();
        assert_eq!(pack.id(), SubjectId::math());
        assert_eq!(pack.display_name(), "数学");
        let capabilities = pack.capabilities();
        assert!(capabilities.supports_kind(QuestionKind::NumericOnly));
        assert!(!capabilities.supports_step_scoring);
        assert_eq!(capabilities.prompt_version, PROMPT_VERSION);
        assert_eq!(
            capabilities.comparators.len(),
            IMPLEMENTED_COMPARATOR_IDS.len()
        );
        assert!(capabilities.validate().is_ok());
        assert_eq!(pack.judge_profile_id(), JUDGE_PROFILE_ID);
        assert!(
            pack.step_evidence(&dummy_rubric(), &dummy_answer())
                .is_none()
        );
    }

    fn dummy_rubric() -> Rubric {
        Rubric {
            id: RubricId::from_ulid(Ulid::from_u128(9)),
            version: 1,
            items: vec![RubricItem {
                id: RubricItemId::from_ulid(Ulid::from_u128(10)),
                criterion: "步骤".to_owned(),
                points: MilliScore::from_milli(1_000).unwrap(),
                required: true,
                depends_on: Vec::new(),
            }],
            mode: ScoringMode::Additive,
            total: MilliScore::from_milli(1_000).unwrap(),
            origin: RubricOrigin::TeacherHandwritten {
                confirmed_by: eg_domain::LOCAL_TEACHER.to_owned(),
                confirmed_at: timestamp(),
            },
            partial_choice: None,
        }
    }

    fn dummy_answer() -> NormalizedAnswer {
        NormalizedAnswer {
            text: "1".to_owned(),
            latex: Some("1".to_owned()),
            value: None,
            steps: Vec::new(),
        }
    }

    #[test]
    fn normalization_via_pack() {
        let pack = MathSubjectPack::new();
        let raw = RawAnswer {
            text: "解：1/2".to_owned(),
            latex: None,
            blocks: Vec::new(),
            source: AnswerSource::Recognition,
        };
        let answer = pack.normalize(&raw).unwrap();
        assert_eq!(answer.text, "0.5");
    }

    #[test]
    fn comparator_chain_defaults_and_declared() {
        let pack = MathSubjectPack::new();
        let empty = ConcreteKey {
            primary: AnswerValue::Text("1".to_owned()),
            alternatives: Vec::new(),
            comparators: Vec::new(),
        };
        let chain = pack.comparator_chain(&empty, QuestionKind::NumericOnly);
        assert_eq!(
            chain.first().map(|comparator| comparator.id().to_string()),
            Some("numeric_abs".to_owned())
        );
        let declared = ConcreteKey {
            comparators: vec![ComparatorId::new("expression_canonical").unwrap()],
            ..empty
        };
        let chain = pack.comparator_chain(&declared, QuestionKind::Calculation);
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].id().as_str(), "expression_canonical");
    }

    #[test]
    fn static_validation_flags_bad_rules() {
        let pack = MathSubjectPack::new();
        // 单选题答案键两个选项 → 错误
        let issues =
            pack.validate_question(&question(QuestionKind::SingleChoice, options(&["A", "B"])));
        assert!(issues.iter().any(eg_domain::RubricIssue::is_error));
        // 纯数值题答案键是文本 → 错误
        let issues = pack.validate_question(&question(
            QuestionKind::NumericOnly,
            AnswerKey::Concrete(ConcreteKey {
                primary: AnswerValue::Text("x".to_owned()),
                alternatives: Vec::new(),
                comparators: Vec::new(),
            }),
        ));
        assert!(issues.iter().any(eg_domain::RubricIssue::is_error));
        // 仅评分标准但无规则 → 错误
        let issues =
            pack.validate_question(&question(QuestionKind::Calculation, AnswerKey::RubricOnly));
        assert!(issues.iter().any(eg_domain::RubricIssue::is_error));
        // 证明题无规则 → 警告
        let issues = pack.validate_question(&question(QuestionKind::Proof, AnswerKey::Missing));
        assert!(
            issues
                .iter()
                .any(|issue| issue.severity == eg_domain::RubricIssueSeverity::Warning)
        );
        // 相对容差 > 2% → 警告
        let issues = pack.validate_question(&question(
            QuestionKind::NumericOnly,
            AnswerKey::Concrete(ConcreteKey {
                primary: AnswerValue::Number {
                    value: eg_domain::Decimal::parse("1").unwrap(),
                    unit: None,
                    tolerance: Some(eg_domain::Tolerance::new(None, Some(30_000)).unwrap()),
                },
                alternatives: Vec::new(),
                comparators: Vec::new(),
            }),
        ));
        assert!(
            issues
                .iter()
                .any(|issue| issue.severity == eg_domain::RubricIssueSeverity::Warning)
        );
    }

    #[test]
    fn static_validation_warns_when_cas_dependency_unavailable() {
        let pack = MathSubjectPack::without_cas();
        let trig_key = AnswerKey::Concrete(ConcreteKey {
            primary: AnswerValue::Expression("\\sin(x)^2+\\cos(x)^2".to_owned()),
            alternatives: Vec::new(),
            comparators: Vec::new(),
        });
        let issues = pack.validate_question(&question(QuestionKind::Calculation, trig_key));
        assert!(issues.iter().any(|issue| {
            issue.severity == eg_domain::RubricIssueSeverity::Warning
                && issue.message.contains("CAS")
        }));
        // 非 CAS 依赖题不应误报
        let numeric_key = AnswerKey::Concrete(ConcreteKey {
            primary: AnswerValue::Number {
                value: eg_domain::Decimal::parse("2.5").unwrap(),
                unit: None,
                tolerance: None,
            },
            alternatives: Vec::new(),
            comparators: Vec::new(),
        });
        let issues = pack.validate_question(&question(QuestionKind::NumericOnly, numeric_key));
        assert!(!issues.iter().any(|issue| issue.message.contains("CAS")));
    }

    #[test]
    fn engine_fingerprint_and_cas_state() {
        let pack = MathSubjectPack::without_cas();
        let fingerprint = pack.engine_fingerprint();
        assert_eq!(fingerprint.prompt_version, PROMPT_VERSION);
        assert_eq!(
            fingerprint.comparators.len(),
            IMPLEMENTED_COMPARATOR_IDS.len()
        );
        assert!(fingerprint.cas.is_none());
        assert_ne!(fingerprint.hash().to_hex(), "");
        assert!(pack.cas_engine_stamp().is_none());
        let status = pack.cas_status();
        assert!(!status.enabled);
        assert!(!status.available);
        assert!(!status.degrades());
    }

    #[test]
    fn static_validation_accepts_good_questions() {
        let pack = MathSubjectPack::new();
        for (kind, key) in [
            (QuestionKind::SingleChoice, options(&["A"])),
            (QuestionKind::MultipleChoice, options(&["A", "B"])),
            (
                QuestionKind::TrueFalse,
                AnswerKey::Concrete(ConcreteKey {
                    primary: AnswerValue::Text("正确".to_owned()),
                    alternatives: Vec::new(),
                    comparators: Vec::new(),
                }),
            ),
            (
                QuestionKind::NumericOnly,
                AnswerKey::Concrete(ConcreteKey {
                    primary: AnswerValue::Number {
                        value: eg_domain::Decimal::parse("2.5").unwrap(),
                        unit: None,
                        tolerance: None,
                    },
                    alternatives: Vec::new(),
                    comparators: Vec::new(),
                }),
            ),
        ] {
            let issues = pack.validate_question(&question(kind, key));
            assert!(
                !issues.iter().any(eg_domain::RubricIssue::is_error),
                "{kind:?} 不应有错误级问题"
            );
        }
    }
}
