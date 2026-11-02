//! 规则静态校验（`SPEC-rubric.md` §2.4，v0.2.2）。
//!
//! 覆盖两段：
//! - **基础 11 条**（`CheckId::R01`–`R11`）：分值合计、粒度、正分值、全对才给分项数、
//!   依赖存在、依赖无环、判据非空非占位、题型受支持、分步给分前置语义、判据重复、
//!   模板来源引用（保证模板套用后仍可被确认并写进 `RubricOrigin`）；
//! - **多选部分给分专项 9 条**（`CheckId::P01`–`P09`）：模式互斥、档位覆盖不跳档、
//!   每档 0<分<满分且单调、档位粒度、来源可追溯、开关关闭即错误、仅多选、恰好 1 个
//!   全对项且分值等于满分、错选归零恒真。
//!
//! 语义：错误级问题阻止确认；警告只提示。校验只读，不修改草稿（调用方负责落库）。

use eg_domain::{
    MilliScore, PartialChoiceRule, PartialChoiceSource, QuestionKind, RubricDraft, RubricDraftItem,
    RubricIssue, RubricIssueSeverity, ScoringMode,
};

use crate::ports::{MAX_ITEMS, RubricSettings, TemplateRef};
use crate::templates::TEMPLATE_CRITERION_MARKER;

/// 校验项编号与错误码（测试与指标按码断言）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CheckId {
    /// R01 分值合计等于题目满分。
    R01Total,
    /// R02 分值粒度。
    R02Granularity,
    /// R03 分值非负且为正项。
    R03PositivePoints,
    /// R04 全对才给分模式只允许 1 项。
    R04AllOrNothingItems,
    /// R05 依赖项存在。
    R05DependencyMissing,
    /// R06 依赖无环。
    R06DependencyCycle,
    /// R07 判据非空且非模板占位。
    R07CriterionPlaceholder,
    /// R08 题型受学科包支持。
    R08KindUnsupported,
    /// R09 分步给分前置语义。
    R09StepwisePrefix,
    /// R10 判据重复。
    R10DuplicateCriterion,
    /// R11 模板来源草稿必须携带模板引用。
    R11TemplateRefMissing,
    /// P01 部分给分与计分模式互斥。
    P01ModeExclusive,
    /// P02 档位覆盖 1..=正确选项数-1，不跳档不重复。
    P02TierCoverage,
    /// P03 每档 0<分<满分且随命中数单调不减。
    P03TierPoints,
    /// P04 档位分值符合评分粒度。
    P04TierGranularity,
    /// P05 档位来源可追溯。
    P05TierSource,
    /// P06 部分给分总开关关闭时不得携带档位。
    P06SwitchOff,
    /// P07 部分给分仅限多选题。
    P07KindOnlyMultiple,
    /// P08 恰好 1 个全对项且其分值等于满分。
    P08FullItem,
    /// P09 错选归零恒为真。
    P09WrongSelectionZero,
}

impl CheckId {
    /// 稳定错误码（审计与统计用，不随文案变化）。
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::R01Total => "R01",
            Self::R02Granularity => "R02",
            Self::R03PositivePoints => "R03",
            Self::R04AllOrNothingItems => "R04",
            Self::R05DependencyMissing => "R05",
            Self::R06DependencyCycle => "R06",
            Self::R07CriterionPlaceholder => "R07",
            Self::R08KindUnsupported => "R08",
            Self::R09StepwisePrefix => "R09",
            Self::R10DuplicateCriterion => "R10",
            Self::R11TemplateRefMissing => "R11",
            Self::P01ModeExclusive => "P01",
            Self::P02TierCoverage => "P02",
            Self::P03TierPoints => "P03",
            Self::P04TierGranularity => "P04",
            Self::P05TierSource => "P05",
            Self::P06SwitchOff => "P06",
            Self::P07KindOnlyMultiple => "P07",
            Self::P08FullItem => "P08",
            Self::P09WrongSelectionZero => "P09",
        }
    }

    /// 中文可读名称。
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::R01Total => "分值合计等于题目满分",
            Self::R02Granularity => "分值粒度",
            Self::R03PositivePoints => "分值非负且为正项",
            Self::R04AllOrNothingItems => "全对才给分只允许 1 项",
            Self::R05DependencyMissing => "依赖项存在",
            Self::R06DependencyCycle => "依赖无环",
            Self::R07CriterionPlaceholder => "判据非空且非模板占位",
            Self::R08KindUnsupported => "题型受支持",
            Self::R09StepwisePrefix => "分步给分前置语义",
            Self::R10DuplicateCriterion => "判据重复",
            Self::R11TemplateRefMissing => "模板来源引用完整",
            Self::P01ModeExclusive => "部分给分与计分模式互斥",
            Self::P02TierCoverage => "档位覆盖不跳档",
            Self::P03TierPoints => "档位分值范围与单调",
            Self::P04TierGranularity => "档位分值粒度",
            Self::P05TierSource => "档位来源可追溯",
            Self::P06SwitchOff => "部分给分开关关闭即错误",
            Self::P07KindOnlyMultiple => "部分给分仅限多选",
            Self::P08FullItem => "恰好一个全对项且分值等于满分",
            Self::P09WrongSelectionZero => "错选归零恒为真",
        }
    }

    /// 是否为部分给分专项。
    #[must_use]
    pub const fn is_partial_choice(self) -> bool {
        matches!(
            self,
            Self::P01ModeExclusive
                | Self::P02TierCoverage
                | Self::P03TierPoints
                | Self::P04TierGranularity
                | Self::P05TierSource
                | Self::P06SwitchOff
                | Self::P07KindOnlyMultiple
                | Self::P08FullItem
                | Self::P09WrongSelectionZero
        )
    }
}

impl core::fmt::Display for CheckId {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{}", self.code())
    }
}

/// 一条校验发现：错误码 + 领域问题（严重级/定位/中文消息）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// 校验项编号。
    pub code: CheckId,
    /// 领域问题（可入库到 `RubricDraft.validation`）。
    pub issue: RubricIssue,
}

impl Finding {
    fn error(code: CheckId, item: Option<u32>, message: impl Into<String>) -> Self {
        Self {
            code,
            issue: RubricIssue {
                severity: RubricIssueSeverity::Error,
                item: item.map(eg_domain::RubricItemRef::Ordinal),
                message: format!("[{}] {}", code.code(), message.into()),
            },
        }
    }

    fn warning(code: CheckId, item: Option<u32>, message: impl Into<String>) -> Self {
        Self {
            code,
            issue: RubricIssue {
                severity: RubricIssueSeverity::Warning,
                item: item.map(eg_domain::RubricItemRef::Ordinal),
                message: format!("[{}] {}", code.code(), message.into()),
            },
        }
    }
}

/// 校验报告（错误级数量决定能否进入待确认/确认）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValidationReport {
    /// 全部发现（按检查顺序）。
    pub findings: Vec<Finding>,
}

impl ValidationReport {
    /// 错误级问题数量。
    #[must_use]
    pub fn error_count(&self) -> usize {
        self.findings
            .iter()
            .filter(|finding| finding.issue.severity == RubricIssueSeverity::Error)
            .count()
    }

    /// 警告级问题数量。
    #[must_use]
    pub fn warning_count(&self) -> usize {
        self.findings
            .iter()
            .filter(|finding| finding.issue.severity == RubricIssueSeverity::Warning)
            .count()
    }

    /// 是否无错误（允许确认的必要条件）。
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.error_count() == 0
    }

    /// 转为领域问题列表（写入 `RubricDraft.validation`）。
    #[must_use]
    pub fn issues(&self) -> Vec<RubricIssue> {
        self.findings
            .iter()
            .map(|finding| finding.issue.clone())
            .collect()
    }

    /// 命中过的错误码（去重、按检查顺序）。
    #[must_use]
    pub fn codes(&self) -> Vec<&'static str> {
        let mut codes: Vec<&'static str> = Vec::new();
        for finding in &self.findings {
            let code = finding.code.code();
            if !codes.contains(&code) {
                codes.push(code);
            }
        }
        codes
    }

    /// 是否命中指定检查项。
    #[must_use]
    pub fn has(&self, code: CheckId) -> bool {
        self.findings.iter().any(|finding| finding.code == code)
    }

    fn push(&mut self, finding: Finding) {
        self.findings.push(finding);
    }
}

/// 校验上下文（题目满分/题型/学科能力/配置/模板引用）。
#[derive(Debug, Clone, Copy)]
pub struct ValidationContext<'a> {
    /// 题目题型（决定 R08/P07）。
    pub question_kind: QuestionKind,
    /// 题目满分（R01/P03/P08 的基准）。
    pub max_score: MilliScore,
    /// 运行配置（粒度、部分给分开关、学科支持题型）。
    pub settings: &'a RubricSettings,
    /// 草稿关联模板（R11/P05 的追溯依据；非模板来源为 `None`）。
    pub template: Option<&'a TemplateRef>,
}

/// 校验草稿（基础 11 条 + 部分给分专项 9 条）。
#[must_use]
pub fn validate_draft(
    draft: &RubricDraft,
    mode: ScoringMode,
    partial_choice: Option<&PartialChoiceRule>,
    context: &ValidationContext<'_>,
) -> ValidationReport {
    let mut report = ValidationReport::default();
    let items = &draft.items;

    if items.len() > MAX_ITEMS {
        report.push(Finding::error(
            CheckId::R03PositivePoints,
            None,
            format!("评分项数量 {} 超过上限 {MAX_ITEMS}", items.len()),
        ));
    }

    // R01 分值合计 = 题目满分（不归一化、不四舍五入）。
    let mut total = MilliScore::ZERO;
    let mut overflow = false;
    for item in items {
        if let Ok(next) = total.checked_add_score(item.points) {
            total = next;
        } else {
            overflow = true;
            break;
        }
    }
    if overflow {
        report.push(Finding::error(
            CheckId::R01Total,
            None,
            "评分项分值相加溢出",
        ));
    } else if total != context.max_score {
        report.push(Finding::error(
            CheckId::R01Total,
            None,
            format!(
                "评分项分值合计 {} 不等于题目满分 {}（差额 {}）",
                total,
                context.max_score,
                context.max_score.as_milli() - total.as_milli()
            ),
        ));
    }

    let mut seen_criteria: Vec<(String, u32)> = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let ordinal = u32::try_from(index).map_or(u32::MAX, |value| value + 1);

        // R02 粒度。
        if !item
            .points
            .is_multiple_of(context.settings.granularity)
            .unwrap_or(false)
        {
            report.push(Finding::error(
                CheckId::R02Granularity,
                Some(ordinal),
                format!(
                    "第 {ordinal} 项分值 {} 不是粒度 {} 的整数倍",
                    item.points, context.settings.granularity
                ),
            ));
        }

        // R03 正分值。
        if item.points.is_zero() {
            report.push(Finding::error(
                CheckId::R03PositivePoints,
                Some(ordinal),
                format!("第 {ordinal} 项分值为 0；评分项必须为正"),
            ));
        }

        // R07 判据非空且非占位。
        validate_criterion(&mut report, item, ordinal);

        // R10 判据重复（规范化后比较）。
        let normalized = normalize_criterion(&item.criterion);
        if let Some((_, first)) = seen_criteria.iter().find(|(text, _)| *text == normalized) {
            report.push(Finding::warning(
                CheckId::R10DuplicateCriterion,
                Some(ordinal),
                format!("第 {ordinal} 项判据与第 {first} 项重复"),
            ));
        } else {
            seen_criteria.push((normalized, ordinal));
        }
    }

    // R04 全对才给分只允许 1 项。
    if mode == ScoringMode::AllOrNothing && items.len() != 1 {
        report.push(Finding::error(
            CheckId::R04AllOrNothingItems,
            None,
            format!("全对才给分模式只允许 1 个评分项，当前 {} 项", items.len()),
        ));
    }

    // R05 依赖存在。
    let length = items.len();
    for (index, item) in items.iter().enumerate() {
        let ordinal = u32::try_from(index).map_or(u32::MAX, |value| value + 1);
        for dependency in &item.depends_on {
            let valid = *dependency >= 1 && (*dependency as usize) <= length;
            if !valid {
                report.push(Finding::error(
                    CheckId::R05DependencyMissing,
                    Some(ordinal),
                    format!("第 {ordinal} 项依赖序号 {dependency} 不存在（评分项共 {length} 项）"),
                ));
            }
        }
    }

    // R06 依赖无环。
    for cycle in dependency_cycles(items) {
        report.push(Finding::error(
            CheckId::R06DependencyCycle,
            cycle.first().copied(),
            format!("依赖成环：{}", format_ordinals(&cycle)),
        ));
    }

    // R08 题型受学科包支持。
    if !context.settings.supports_kind(context.question_kind) {
        report.push(Finding::error(
            CheckId::R08KindUnsupported,
            None,
            format!(
                "题型 {:?} 不在学科包支持范围内；该题型不得用规则判分",
                context.question_kind
            ),
        ));
    }

    // R09 分步给分前置语义（无依赖/前向依赖属警告，缺失与成环已在 R05/R06 报错误）。
    if mode == ScoringMode::Stepwise {
        validate_stepwise(&mut report, items);
    }

    // R11 模板来源草稿必须携带模板引用。
    if draft.origin == eg_domain::DraftOrigin::TemplateApplied && context.template.is_none() {
        report.push(Finding::error(
            CheckId::R11TemplateRefMissing,
            None,
            "草稿来源为模板套用，但缺少模板 id/版本，无法写入规则来源与审计",
        ));
    }

    // 部分给分专项。
    validate_partial_choice(&mut report, items, mode, partial_choice, context);

    report
}

fn validate_criterion(report: &mut ValidationReport, item: &RubricDraftItem, ordinal: u32) {
    let trimmed = item.criterion.trim();
    if trimmed.is_empty() {
        report.push(Finding::error(
            CheckId::R07CriterionPlaceholder,
            Some(ordinal),
            format!("第 {ordinal} 项判据为空"),
        ));
    } else if trimmed.contains(TEMPLATE_CRITERION_MARKER) {
        report.push(Finding::error(
            CheckId::R07CriterionPlaceholder,
            Some(ordinal),
            format!(
                "第 {ordinal} 项判据仍是模板占位（含 {TEMPLATE_CRITERION_MARKER}），须替换为本题具体判据"
            ),
        ));
    }
}

fn validate_stepwise(report: &mut ValidationReport, items: &[RubricDraftItem]) {
    if items.is_empty() {
        return;
    }
    if items.len() > 1 && items.iter().all(|item| item.depends_on.is_empty()) {
        report.push(Finding::warning(
            CheckId::R09StepwisePrefix,
            None,
            "分步给分未标注任何前置依赖；若需要「前置必须命中」语义，请补 depends_on",
        ));
    }
    for (index, item) in items.iter().enumerate() {
        let ordinal = u32::try_from(index).map_or(u32::MAX, |value| value + 1);
        for dependency in &item.depends_on {
            if *dependency >= ordinal {
                report.push(Finding::warning(
                    CheckId::R09StepwisePrefix,
                    Some(ordinal),
                    format!(
                        "第 {ordinal} 项依赖了同序或更后的第 {dependency} 项；分步步骤应按卷面先后排列"
                    ),
                ));
            }
        }
    }
}

fn validate_partial_choice(
    report: &mut ValidationReport,
    items: &[RubricDraftItem],
    mode: ScoringMode,
    partial_choice: Option<&PartialChoiceRule>,
    context: &ValidationContext<'_>,
) {
    let is_partial_mode = mode == ScoringMode::PartialChoice;

    // P01 模式互斥：部分给分模式 ⇔ 档位非空。
    if is_partial_mode != partial_choice.is_some() {
        let message = if is_partial_mode {
            "计分模式为按选项部分给分，但缺少档位规则"
        } else {
            "计分模式不是按选项部分给分，却携带了档位规则"
        };
        report.push(Finding::error(CheckId::P01ModeExclusive, None, message));
        return;
    }
    let Some(rule) = partial_choice else {
        return;
    };

    // P06 总开关关闭即错误（不得静默降级为全对计分）。
    if !context.settings.partial_choice_enabled {
        report.push(Finding::error(
            CheckId::P06SwitchOff,
            None,
            "部分给分总开关已关闭（rubric.partial_choice_enabled=关），含档位的规则不得生效；\
             请去设置开启或删除档位后改按全对计分",
        ));
    }

    // P07 仅限多选。
    if context.question_kind != QuestionKind::MultipleChoice {
        report.push(Finding::error(
            CheckId::P07KindOnlyMultiple,
            None,
            format!(
                "部分给分仅适用于多选题，当前题型为 {:?}",
                context.question_kind
            ),
        ));
    }

    // P08 恰好 1 个全对项且分值等于满分（同时校验档位规则满分自洽）。
    if items.len() != 1 {
        report.push(Finding::error(
            CheckId::P08FullItem,
            None,
            format!(
                "部分给分必须有且仅有 1 个全对评分项，当前 {} 项",
                items.len()
            ),
        ));
    } else if items[0].points != context.max_score {
        report.push(Finding::error(
            CheckId::P08FullItem,
            Some(1),
            format!(
                "全对评分项分值 {} 必须等于题目满分 {}",
                items[0].points, context.max_score
            ),
        ));
    }
    if rule.total_milli != context.max_score {
        report.push(Finding::error(
            CheckId::P08FullItem,
            None,
            format!(
                "档位规则满分 {} 与题目满分 {} 不一致",
                rule.total_milli, context.max_score
            ),
        ));
    }

    // P02 档位覆盖：正确选项数 2 或 3；档位覆盖 1..=n-1、升序、不重复。
    if matches!(rule.correct_option_count, 2 | 3) {
        let expected = rule.correct_option_count - 1;
        let actual: Vec<u32> = rule
            .tiers
            .iter()
            .map(|tier| tier.selected_correct)
            .collect();
        let expected_sequence: Vec<u32> = (1..=expected).collect();
        if actual != expected_sequence {
            report.push(Finding::error(
                CheckId::P02TierCoverage,
                None,
                format!(
                    "档位必须恰好覆盖 1..={expected} 且不跳档，当前为 {}",
                    format_ordinals(&actual)
                ),
            ));
        }
    } else {
        report.push(Finding::error(
            CheckId::P02TierCoverage,
            None,
            format!(
                "正确选项数为 {}，现行官方口径仅支持 2 或 3；其它取值必须人工处理",
                rule.correct_option_count
            ),
        ));
    }

    // P03 每档 0<分<满分且单调不减。
    let mut previous: Option<MilliScore> = None;
    for tier in &rule.tiers {
        if tier.points.is_zero() || tier.points >= context.max_score {
            report.push(Finding::error(
                CheckId::P03TierPoints,
                Some(tier.selected_correct),
                format!(
                    "命中 {} 个正确选项的档位分值 {} 必须大于 0 且小于满分 {}",
                    tier.selected_correct, tier.points, context.max_score
                ),
            ));
        }
        if let Some(previous) = previous
            && tier.points < previous
        {
            report.push(Finding::error(
                CheckId::P03TierPoints,
                Some(tier.selected_correct),
                format!(
                    "档位分值必须随命中数单调不减：命中 {} 个得 {}，低于前一档 {}",
                    tier.selected_correct, tier.points, previous
                ),
            ));
        }
        previous = Some(tier.points);
    }

    // P04 档位粒度。
    for tier in &rule.tiers {
        if !tier
            .points
            .is_multiple_of(context.settings.granularity)
            .unwrap_or(false)
        {
            report.push(Finding::error(
                CheckId::P04TierGranularity,
                Some(tier.selected_correct),
                format!(
                    "命中 {} 个的档位分值 {} 不是粒度 {} 的整数倍",
                    tier.selected_correct, tier.points, context.settings.granularity
                ),
            ));
        }
    }

    // P05 来源可追溯。
    match rule.source {
        PartialChoiceSource::Official => {
            let has_note = rule
                .source_note
                .as_deref()
                .is_some_and(|note| !note.trim().is_empty());
            if !has_note {
                report.push(Finding::error(
                    CheckId::P05TierSource,
                    None,
                    "官方口径档位必须填写来源说明（source_note：出处 + 适用年份）",
                ));
            }
        }
        PartialChoiceSource::TemplateData => {
            if context.template.is_none() {
                report.push(Finding::error(
                    CheckId::P05TierSource,
                    None,
                    "档位来源为模板数据，但草稿缺少模板引用（template_id/version）",
                ));
            }
        }
        PartialChoiceSource::TeacherCustom => {}
    }

    // P09 错选归零恒真。
    if !rule.wrong_selection_zero {
        report.push(Finding::error(
            CheckId::P09WrongSelectionZero,
            None,
            "错选归零恒为真，不得关闭；如需改变口径必须由教师显式重写规则",
        ));
    }

    // 兜底：把领域层的档位校验作为最后一道防线（避免本模块漏项）。
    if report.is_ok()
        && let Err(error) = rule.validate(context.max_score, Some(context.settings.granularity))
    {
        report.push(Finding::error(
            CheckId::P03TierPoints,
            None,
            format!("领域档位校验未通过：{error}"),
        ));
    }
}

/// 返回依赖环上的序号列表（1 起；首尾不重复）。
fn dependency_cycles(items: &[RubricDraftItem]) -> Vec<Vec<u32>> {
    let length = items.len();
    let mut edges: Vec<Vec<usize>> = Vec::with_capacity(length);
    for item in items {
        let mut targets = Vec::new();
        for dependency in &item.depends_on {
            if *dependency >= 1 && (*dependency as usize) <= length {
                targets.push(*dependency as usize - 1);
            }
        }
        edges.push(targets);
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum State {
        Unvisited,
        Visiting,
        Done,
    }

    fn visit(
        node: usize,
        edges: &[Vec<usize>],
        states: &mut [State],
        stack: &mut Vec<usize>,
        cycles: &mut Vec<Vec<u32>>,
    ) {
        match states[node] {
            State::Done => return,
            State::Visiting => {
                if let Some(start) = stack.iter().position(|value| *value == node) {
                    let cycle: Vec<u32> = stack[start..]
                        .iter()
                        .map(|value| u32::try_from(*value).map_or(u32::MAX, |v| v + 1))
                        .collect();
                    cycles.push(cycle);
                }
                return;
            }
            State::Unvisited => {}
        }
        states[node] = State::Visiting;
        stack.push(node);
        for target in &edges[node] {
            visit(*target, edges, states, stack, cycles);
        }
        stack.pop();
        states[node] = State::Done;
    }

    let mut states = vec![State::Unvisited; length];
    let mut stack = Vec::new();
    let mut cycles = Vec::new();
    for node in 0..length {
        visit(node, &edges, &mut states, &mut stack, &mut cycles);
    }
    cycles
}

fn format_ordinals(ordinals: &[u32]) -> String {
    if ordinals.is_empty() {
        return "（空）".to_owned();
    }
    ordinals
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(" → ")
}

fn normalize_criterion(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use eg_domain::{
        DraftOrigin, DraftStatus, PartialChoiceSource, PartialChoiceTier, QuestionId, RubricDraft,
        RubricDraftItem, RubricId, TemplateId, Ulid,
    };

    use super::*;
    use crate::ports::{RubricSettings, TemplateRef, TemplateStatus, TemplateVersion};
    use eg_domain::time::parse_rfc3339;

    fn timestamp() -> eg_domain::Timestamp {
        parse_rfc3339("2026-11-02T00:00:00Z").unwrap()
    }

    pub(crate) fn draft(items: Vec<RubricDraftItem>, origin: DraftOrigin) -> RubricDraft {
        RubricDraft {
            id: RubricId::from_ulid(Ulid::from_u128(1)),
            question_id: QuestionId::from_ulid(Ulid::from_u128(2)),
            status: DraftStatus::Draft,
            items,
            raw_text: "教师手写规则".to_owned(),
            origin,
            model_call_id: None,
            validation: Vec::new(),
            created_at: timestamp(),
            confirmed_at: None,
        }
    }

    pub(crate) fn item(
        criterion: &str,
        points_milli: i64,
        depends_on: Vec<u32>,
    ) -> RubricDraftItem {
        RubricDraftItem {
            criterion: criterion.to_owned(),
            points: MilliScore::from_milli(points_milli).unwrap(),
            required: false,
            depends_on,
        }
    }

    fn settings() -> RubricSettings {
        RubricSettings::default()
    }

    fn template_ref() -> TemplateRef {
        TemplateRef {
            template_id: TemplateId::new("national-new-curriculum-1").unwrap(),
            set_id: "new-curriculum-1".to_owned(),
            display_name: "新课标Ⅰ卷".to_owned(),
            version: TemplateVersion::new(2026, 1).unwrap(),
            status: TemplateStatus::Active,
        }
    }

    fn validate(
        draft: &RubricDraft,
        kind: QuestionKind,
        max: i64,
        mode: ScoringMode,
        partial: Option<&PartialChoiceRule>,
        template: Option<&TemplateRef>,
    ) -> ValidationReport {
        let settings = settings();
        let context = ValidationContext {
            question_kind: kind,
            max_score: MilliScore::from_milli(max).unwrap(),
            settings: &settings,
            template,
        };
        validate_draft(draft, mode, partial, &context)
    }

    #[test]
    fn valid_additive_draft_passes() {
        let draft = draft(
            vec![
                item("列出方程", 1500, vec![]),
                item("解得 x=2", 1500, vec![]),
            ],
            DraftOrigin::HandwrittenTransliteration,
        );
        let report = validate(
            &draft,
            QuestionKind::Calculation,
            3000,
            ScoringMode::Additive,
            None,
            None,
        );
        assert!(report.is_ok(), "{:?}", report.findings);
        assert_eq!(report.error_count(), 0);
        assert_eq!(report.warning_count(), 0);
    }

    #[test]
    fn r01_total_mismatch_is_error() {
        let draft = draft(
            vec![item("步骤", 1000, vec![])],
            DraftOrigin::HandwrittenTransliteration,
        );
        let report = validate(
            &draft,
            QuestionKind::Calculation,
            2000,
            ScoringMode::Additive,
            None,
            None,
        );
        assert!(report.has(CheckId::R01Total));
        assert_eq!(report.error_count(), 1);
    }

    #[test]
    fn r02_granularity_is_error() {
        let draft = draft(
            vec![item("步骤", 1550, vec![])],
            DraftOrigin::HandwrittenTransliteration,
        );
        let report = validate(
            &draft,
            QuestionKind::Calculation,
            1550,
            ScoringMode::Additive,
            None,
            None,
        );
        assert!(report.has(CheckId::R02Granularity));
    }

    #[test]
    fn r03_zero_points_is_error() {
        let draft = draft(
            vec![item("步骤一", 0, vec![]), item("步骤二", 1000, vec![])],
            DraftOrigin::HandwrittenTransliteration,
        );
        let report = validate(
            &draft,
            QuestionKind::Calculation,
            1000,
            ScoringMode::Additive,
            None,
            None,
        );
        assert!(report.has(CheckId::R03PositivePoints));
    }

    #[test]
    fn r04_all_or_nothing_item_count_is_error() {
        let draft = draft(
            vec![item("A", 500, vec![]), item("B", 500, vec![])],
            DraftOrigin::HandwrittenTransliteration,
        );
        let report = validate(
            &draft,
            QuestionKind::SingleChoice,
            1000,
            ScoringMode::AllOrNothing,
            None,
            None,
        );
        assert!(report.has(CheckId::R04AllOrNothingItems));
    }

    #[test]
    fn r05_and_r06_dependency_issues_are_errors() {
        let missing = draft(
            vec![item("步骤", 1000, vec![3])],
            DraftOrigin::HandwrittenTransliteration,
        );
        let report = validate(
            &missing,
            QuestionKind::Calculation,
            1000,
            ScoringMode::Stepwise,
            None,
            None,
        );
        assert!(report.has(CheckId::R05DependencyMissing));

        let cycle = draft(
            vec![item("A", 500, vec![2]), item("B", 500, vec![1])],
            DraftOrigin::HandwrittenTransliteration,
        );
        let report = validate(
            &cycle,
            QuestionKind::Calculation,
            1000,
            ScoringMode::Stepwise,
            None,
            None,
        );
        assert!(report.has(CheckId::R06DependencyCycle));
    }

    #[test]
    fn r07_placeholder_criterion_is_error() {
        let draft = draft(
            vec![item("【模板骨架】关键步骤", 1000, vec![])],
            DraftOrigin::TemplateApplied,
        );
        let report = validate(
            &draft,
            QuestionKind::Calculation,
            1000,
            ScoringMode::Additive,
            None,
            Some(&template_ref()),
        );
        assert!(report.has(CheckId::R07CriterionPlaceholder));
    }

    #[test]
    fn r08_unsupported_kind_is_error() {
        let draft = draft(
            vec![item("结论", 1000, vec![])],
            DraftOrigin::HandwrittenTransliteration,
        );
        let mut settings = settings();
        settings.supported_kinds = vec![QuestionKind::SingleChoice];
        let context = ValidationContext {
            question_kind: QuestionKind::Essay,
            max_score: MilliScore::from_milli(1000).unwrap(),
            settings: &settings,
            template: None,
        };
        let report = validate_draft(&draft, ScoringMode::Additive, None, &context);
        assert!(report.has(CheckId::R08KindUnsupported));
    }

    #[test]
    fn r09_stepwise_without_dependencies_warns() {
        let draft = draft(
            vec![item("步骤一", 500, vec![]), item("步骤二", 500, vec![])],
            DraftOrigin::HandwrittenTransliteration,
        );
        let report = validate(
            &draft,
            QuestionKind::Calculation,
            1000,
            ScoringMode::Stepwise,
            None,
            None,
        );
        assert!(report.has(CheckId::R09StepwisePrefix));
        assert_eq!(report.error_count(), 0);
        assert!(report.warning_count() >= 1);
    }

    #[test]
    fn r10_duplicate_criterion_warns() {
        let draft = draft(
            vec![
                item("写出公式", 500, vec![]),
                item("写出 公式", 500, vec![]),
            ],
            DraftOrigin::HandwrittenTransliteration,
        );
        let report = validate(
            &draft,
            QuestionKind::Calculation,
            1000,
            ScoringMode::Additive,
            None,
            None,
        );
        assert!(report.has(CheckId::R10DuplicateCriterion));
        assert_eq!(report.error_count(), 0);
    }

    #[test]
    fn r11_template_origin_requires_template_ref() {
        let draft = draft(
            vec![item("步骤", 1000, vec![])],
            DraftOrigin::TemplateApplied,
        );
        let report = validate(
            &draft,
            QuestionKind::Calculation,
            1000,
            ScoringMode::Additive,
            None,
            None,
        );
        assert!(report.has(CheckId::R11TemplateRefMissing));
    }

    fn partial_rule(total_milli: i64) -> PartialChoiceRule {
        PartialChoiceRule {
            correct_option_count: 3,
            tiers: vec![
                PartialChoiceTier {
                    selected_correct: 1,
                    points: MilliScore::from_milli(2000).unwrap(),
                    label: Some("选对 1 个".to_owned()),
                },
                PartialChoiceTier {
                    selected_correct: 2,
                    points: MilliScore::from_milli(4000).unwrap(),
                    label: None,
                },
            ],
            wrong_selection_zero: true,
            source: PartialChoiceSource::Official,
            source_note: Some("2026 年全国卷评分细则".to_owned()),
            total_milli: MilliScore::from_milli(total_milli).unwrap(),
        }
    }

    #[test]
    fn valid_partial_choice_draft_passes() {
        let draft = draft(
            vec![item("全对", 6000, vec![])],
            DraftOrigin::HandwrittenTransliteration,
        );
        let rule = partial_rule(6000);
        let report = validate(
            &draft,
            QuestionKind::MultipleChoice,
            6000,
            ScoringMode::PartialChoice,
            Some(&rule),
            None,
        );
        assert!(report.is_ok(), "{:?}", report.findings);
    }

    #[test]
    fn partial_choice_mode_mutual_exclusion() {
        let draft = draft(
            vec![item("全对", 6000, vec![])],
            DraftOrigin::HandwrittenTransliteration,
        );
        let rule = partial_rule(6000);
        let report = validate(
            &draft,
            QuestionKind::MultipleChoice,
            6000,
            ScoringMode::AllOrNothing,
            Some(&rule),
            None,
        );
        assert!(report.has(CheckId::P01ModeExclusive));

        let report = validate(
            &draft,
            QuestionKind::MultipleChoice,
            6000,
            ScoringMode::PartialChoice,
            None,
            None,
        );
        assert!(report.has(CheckId::P01ModeExclusive));
    }

    #[test]
    fn partial_choice_tier_coverage_and_bounds() {
        let draft = draft(
            vec![item("全对", 6000, vec![])],
            DraftOrigin::HandwrittenTransliteration,
        );
        let mut rule = partial_rule(6000);
        rule.tiers.truncate(1);
        let report = validate(
            &draft,
            QuestionKind::MultipleChoice,
            6000,
            ScoringMode::PartialChoice,
            Some(&rule),
            None,
        );
        assert!(report.has(CheckId::P02TierCoverage));

        let mut rule = partial_rule(6000);
        rule.tiers[1].points = MilliScore::from_milli(6000).unwrap();
        let report = validate(
            &draft,
            QuestionKind::MultipleChoice,
            6000,
            ScoringMode::PartialChoice,
            Some(&rule),
            None,
        );
        assert!(report.has(CheckId::P03TierPoints));
    }

    #[test]
    fn partial_choice_switch_off_and_kind_and_source() {
        let draft = draft(
            vec![item("全对", 6000, vec![])],
            DraftOrigin::HandwrittenTransliteration,
        );
        let rule = partial_rule(6000);

        let mut settings = settings();
        settings.partial_choice_enabled = false;
        let context = ValidationContext {
            question_kind: QuestionKind::MultipleChoice,
            max_score: MilliScore::from_milli(6000).unwrap(),
            settings: &settings,
            template: None,
        };
        let report = validate_draft(&draft, ScoringMode::PartialChoice, Some(&rule), &context);
        assert!(report.has(CheckId::P06SwitchOff));

        let report = validate(
            &draft,
            QuestionKind::SingleChoice,
            6000,
            ScoringMode::PartialChoice,
            Some(&rule),
            None,
        );
        assert!(report.has(CheckId::P07KindOnlyMultiple));

        let mut rule = partial_rule(6000);
        rule.source_note = None;
        let report = validate(
            &draft,
            QuestionKind::MultipleChoice,
            6000,
            ScoringMode::PartialChoice,
            Some(&rule),
            None,
        );
        assert!(report.has(CheckId::P05TierSource));
    }

    #[test]
    fn partial_choice_granularity_and_wrong_selection() {
        let draft = draft(
            vec![item("全对", 6000, vec![])],
            DraftOrigin::HandwrittenTransliteration,
        );
        let mut rule = partial_rule(6000);
        rule.tiers[0].points = MilliScore::from_milli(2550).unwrap();
        let report = validate(
            &draft,
            QuestionKind::MultipleChoice,
            6000,
            ScoringMode::PartialChoice,
            Some(&rule),
            None,
        );
        assert!(report.has(CheckId::P04TierGranularity));

        let mut rule = partial_rule(6000);
        rule.wrong_selection_zero = false;
        let report = validate(
            &draft,
            QuestionKind::MultipleChoice,
            6000,
            ScoringMode::PartialChoice,
            Some(&rule),
            None,
        );
        assert!(report.has(CheckId::P09WrongSelectionZero));
    }
}
