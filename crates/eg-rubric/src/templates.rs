//! 全国卷模板库（`SPEC-rubric.md` §2.2/§3.5，`CONFIG.md` §7；`assets/templates/README.md`）。
//!
//! 职责：
//! 1. 读取 `assets/templates/national/<set-id>/template.toml` 四套全国卷模板（只读、只追加）；
//! 2. **读取兼容**：模板数据当前缺「多选部分给分档位」（README §11 已知限制），loader 支持
//!    `partial_choice` 段并可解析，缺失时记录 [`TemplateWarningCode::PartialChoiceTiersMissing`]，
//!    不擅自改数据（模板更新属 T25）；
//! 3. 加载期文件级校验（T1–T7：题量/题号连续/总分一致/题型合法/骨架存在/合并选做只计一次）；
//! 4. 套用：按题号把骨架比例折算成草稿评分项（整数精确、不四舍五入），保留占位判据
//!    （静态校验会以 R07 错误级拦截，迫使教师替换后才能确认）。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use eg_domain::{
    MilliScore, PartialChoiceRule, PartialChoiceSource, PartialChoiceTier, QuestionKind,
    RubricDraftItem, ScoringMode, TemplateId,
};
use serde::Deserialize;

use crate::error::{Result, RubricError};
use crate::ports::{MAX_ITEMS, RubricSettings, TemplateRef, TemplateStatus, TemplateVersion};

/// 模板骨架占位判据前缀（`assets/templates/README.md` §4.5；静态校验 R07 的识别标记）。
pub const TEMPLATE_CRITERION_MARKER: &str = "【模板骨架】";

/// 模板加载告警码（读取兼容缺口，进研究记录与审计）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TemplateWarningCode {
    /// 多选题骨架缺少部分给分档位（当前模板数据缺口，T25 维护）。
    PartialChoiceTiersMissing,
    /// 模板里出现未知的枚举取值（当前按兼容读取，不阻断加载）。
    UnknownEnumValue,
    /// 历史卷（retired）只读快照：可在显式允许时套用于历史试卷。
    RetiredTemplate,
}

impl TemplateWarningCode {
    /// 稳定告警码。
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::PartialChoiceTiersMissing => "TW01",
            Self::UnknownEnumValue => "TW02",
            Self::RetiredTemplate => "TW03",
        }
    }
}

/// 模板加载告警。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateWarning {
    /// 告警码。
    pub code: TemplateWarningCode,
    /// 卷型 id。
    pub set_id: String,
    /// 中文可读说明。
    pub message: String,
}

/// 置信度分级（`assets/templates/README.md` §4.1；未知取值降级为 [`ConfidenceLevel::Unknown`] 并告警）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfidenceLevel {
    /// 官方原文可核。
    High,
    /// 多份独立公开资料一致或官方间接可推。
    Medium,
    /// 单一来源或推断。
    Low,
    /// 未知取值（兼容读取，不阻断）。
    Unknown(String),
}

/// 模板来源与免责信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateProvenance {
    /// 最近人工核对日期（原样保留 `YYYY-MM-DD`）。
    pub verified_at: String,
    /// 整体置信度。
    pub confidence: ConfidenceLevel,
    /// 来源分级（`official` / `public-analysis` / `inferred` 等，未知值原样保留）。
    pub basis: String,
    /// 研究记录来源编号。
    pub source_ids: Vec<String>,
    /// 面向教师的免责声明。
    pub disclaimer: String,
}

/// 单题骨架。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateSkeleton {
    /// 骨架 id（模板内唯一）。
    pub id: String,
    /// 计分方式。
    pub mode: ScoringMode,
    /// 骨架名称。
    pub label: String,
    /// 计分说明。
    pub scoring_note: Option<String>,
    /// 评分项（占位）。
    pub items: Vec<TemplateSkeletonItem>,
    /// 多选部分给分档位（读取兼容；当前数据可能缺失）。
    pub partial_choice: Option<TemplatePartialChoice>,
}

/// 骨架评分项（比例表达，套用时按题目满分折算）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateSkeletonItem {
    /// 骨架内序号（1 起，连续）。
    pub ordinal: u32,
    /// 占位判据（以 [`TEMPLATE_CRITERION_MARKER`] 开头）。
    pub criterion: String,
    /// 占该题满分的千分比。
    pub ratio_permille: i64,
    /// 是否必须满足。
    pub required: bool,
    /// 依赖的骨架内序号。
    pub depends_on: Vec<u32>,
    /// 是否仍为占位（加载期要求为真）。
    pub placeholder: bool,
}

/// 模板内多选部分给分的档位金额表达（比例或绝对分值二选一）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateTierAmount {
    /// 占该题满分的千分比。
    RatioPermille(i64),
    /// 绝对分值（千分之一分；缺省时用比例）。
    Milli(i64),
}

/// 模板内多选部分给分档位。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplatePartialChoiceTier {
    /// 命中的正确选项数。
    pub selected_correct: u32,
    /// 档位金额。
    pub amount: TemplateTierAmount,
}

/// 模板内多选部分给分规则（T25 数据更新后启用；loader 已支持读取与校验）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplatePartialChoice {
    /// 正确选项数（现行官方口径 2 或 3）。
    pub correct_option_count: u32,
    /// 档位（覆盖 1..=正确选项数-1）。
    pub tiers: Vec<TemplatePartialChoiceTier>,
    /// 错选归零（模板数据必须为真）。
    pub wrong_selection_zero: bool,
    /// 口径来源。
    pub source: PartialChoiceSource,
    /// 口径出处与适用年份（官方口径必填）。
    pub source_note: Option<String>,
}

/// 模板题目（`[[questions]]`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateQuestion {
    /// 卷面顺序（从 1 起连续）。
    pub ordinal: u32,
    /// 题号显示文本（如 `17`、`22/23`、`17(2)`）。
    pub label: String,
    /// 题型。
    pub kind: QuestionKind,
    /// 本题满分。
    pub max_score: MilliScore,
    /// 引用的骨架 id。
    pub skeleton: String,
    /// 题型可替换提示。
    pub kind_note: Option<String>,
    /// 合并的二选一选做题分组 id（同组只计一题）。
    pub choice_group: Option<String>,
}

/// 已加载、已校验的单个模板。
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedTemplate {
    /// 模板稳定 id。
    pub template_id: TemplateId,
    /// 卷型 id（= 目录名）。
    pub set_id: String,
    /// 中文卷型名。
    pub display_name: String,
    /// 学科（v1 恒 `math`）。
    pub subject: String,
    /// 卷制（`new-gaokao` / `old-gaokao`）。
    pub paper_style: String,
    /// 状态（active / retired）。
    pub status: TemplateStatus,
    /// 模板版本。
    pub version: TemplateVersion,
    /// 快照年份。
    pub snapshot_year: u32,
    /// 结构快照说明。
    pub snapshot_label: String,
    /// 全套总分。
    pub total_score: MilliScore,
    /// 作答题目数。
    pub question_count: u32,
    /// 卷面最大题号（合并选做题时存在）。
    pub printed_max_ordinal: Option<u32>,
    /// 题型与分值分布说明。
    pub structure_note: String,
    /// 结构置信度。
    pub structure_confidence: ConfidenceLevel,
    /// 逐题分值置信度。
    pub score_split_confidence: ConfidenceLevel,
    /// 来源与免责。
    pub provenance: TemplateProvenance,
    /// 题目（按序号升序）。
    pub questions: Vec<TemplateQuestion>,
    /// 骨架库。
    pub skeletons: BTreeMap<String, TemplateSkeleton>,
    /// 源文件路径（审计与诊断）。
    pub source_path: String,
}

impl LoadedTemplate {
    /// 转成草稿/审计使用的模板引用。
    #[must_use]
    pub fn as_ref(&self) -> TemplateRef {
        TemplateRef {
            template_id: self.template_id.clone(),
            set_id: self.set_id.clone(),
            display_name: self.display_name.clone(),
            version: self.version,
            status: self.status,
        }
    }

    /// 按题号取题目。
    #[must_use]
    pub fn question(&self, ordinal: u32) -> Option<&TemplateQuestion> {
        self.questions
            .iter()
            .find(|question| question.ordinal == ordinal)
    }

    /// 全部题号（升序）。
    #[must_use]
    pub fn question_ordinals(&self) -> Vec<u32> {
        self.questions
            .iter()
            .map(|question| question.ordinal)
            .collect()
    }
}

/// 模板套用产出（草稿评分项 + 计分模式 + 部分给分档位）。
#[derive(Debug, Clone, PartialEq)]
pub struct TemplatePlan {
    /// 草稿评分项（占位判据保留）。
    pub items: Vec<RubricDraftItem>,
    /// 计分方式（来自骨架）。
    pub mode: ScoringMode,
    /// 多选部分给分档位（骨架携带时）。
    pub partial_choice: Option<PartialChoiceRule>,
    /// 使用的骨架 id（审计）。
    pub skeleton_id: String,
}

impl LoadedTemplate {
    /// 按题号套用骨架，产出草稿计划。
    ///
    /// - `kind` 必须与模板该题题型一致（不一致返回 [`RubricError::TemplateKindMismatch`]）；
    /// - `skeleton_override` 允许教师按 `kind_note` 改换骨架（如计算题当年为证明题）；
    /// - 比例折算必须整除，否则视为模板数据错误（不静默四舍五入）。
    ///
    /// # Errors
    ///
    /// 题号缺失、题型不匹配、骨架缺失、比例不能整除时返回对应 [`RubricError`]。
    pub fn plan(
        &self,
        ordinal: u32,
        kind: QuestionKind,
        skeleton_override: Option<&str>,
        settings: &RubricSettings,
    ) -> Result<TemplatePlan> {
        settings.validate()?;
        let question =
            self.question(ordinal)
                .ok_or_else(|| RubricError::TemplateQuestionMissing {
                    set_id: self.set_id.clone(),
                    ordinal,
                })?;
        if question.kind != kind {
            return Err(RubricError::TemplateKindMismatch {
                set_id: self.set_id.clone(),
                ordinal,
                template_kind: question.kind,
                question_kind: kind,
            });
        }
        let skeleton_id = skeleton_override.unwrap_or(&question.skeleton);
        let skeleton = self.skeletons.get(skeleton_id).ok_or_else(|| {
            RubricError::TemplateSkeletonInvalid {
                message: format!(
                    "模板 {} 题号 {ordinal} 引用的骨架 {skeleton_id} 不存在",
                    self.set_id
                ),
            }
        })?;
        if skeleton.items.len() > MAX_ITEMS {
            return Err(RubricError::TemplateSkeletonInvalid {
                message: format!(
                    "模板 {} 骨架 {skeleton_id} 评分项 {} 项，超过上限 {MAX_ITEMS}",
                    self.set_id,
                    skeleton.items.len()
                ),
            });
        }

        let mut items = Vec::with_capacity(skeleton.items.len());
        for item in &skeleton.items {
            let points = convert_ratio(question.max_score, item.ratio_permille)?;
            items.push(RubricDraftItem {
                criterion: item.criterion.clone(),
                points,
                required: item.required,
                depends_on: item.depends_on.clone(),
            });
        }

        let partial_choice = skeleton
            .partial_choice
            .as_ref()
            .map(|rule| plan_partial_choice(rule, question.max_score, self))
            .transpose()?;

        Ok(TemplatePlan {
            items,
            mode: skeleton.mode,
            partial_choice,
            skeleton_id: skeleton_id.to_owned(),
        })
    }
}

fn plan_partial_choice(
    rule: &TemplatePartialChoice,
    max_score: MilliScore,
    template: &LoadedTemplate,
) -> Result<PartialChoiceRule> {
    let mut tiers = Vec::with_capacity(rule.tiers.len());
    for tier in &rule.tiers {
        let points = match tier.amount {
            TemplateTierAmount::Milli(milli) => MilliScore::from_milli(milli)?,
            TemplateTierAmount::RatioPermille(ratio) => convert_ratio(max_score, ratio)?,
        };
        tiers.push(PartialChoiceTier {
            selected_correct: tier.selected_correct,
            points,
            label: None,
        });
    }
    let source_note = rule.source_note.clone().or_else(|| {
        Some(format!(
            "模板 {}（{}）v{} 骨架档位",
            template.display_name, template.set_id, template.version
        ))
    });
    Ok(PartialChoiceRule {
        correct_option_count: rule.correct_option_count,
        tiers,
        wrong_selection_zero: rule.wrong_selection_zero,
        source: rule.source,
        source_note,
        total_milli: max_score,
    })
}

fn convert_ratio(max_score: MilliScore, ratio_permille: i64) -> Result<MilliScore> {
    if ratio_permille <= 0 {
        return Err(RubricError::TemplateRatioInexact {
            max_milli: max_score.as_milli(),
            ratio_permille,
        });
    }
    let numerator = max_score
        .as_milli()
        .checked_mul(ratio_permille)
        .ok_or_else(|| RubricError::template_invalid("分值折算溢出"))?;
    if numerator % 1000 != 0 {
        return Err(RubricError::TemplateRatioInexact {
            max_milli: max_score.as_milli(),
            ratio_permille,
        });
    }
    MilliScore::from_milli(numerator / 1000).map_err(RubricError::from)
}

/// 模板选择（卷型 + 是否允许历史卷）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateSelection {
    /// `set_id` 或中文卷型名（两者都按精确匹配）。
    pub key: String,
    /// 是否允许套用 retired 历史卷（历史试卷复现时由教师显式开启）。
    pub allow_retired: bool,
}

/// 全国卷模板包（内置只读 + 可合并导入的自定义模板）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TemplatePack {
    /// 已加载模板（按 set_id 升序）。
    pub templates: Vec<LoadedTemplate>,
    /// 加载告警（读取兼容缺口）。
    pub warnings: Vec<TemplateWarning>,
    /// 模板根目录（诊断用）。
    pub root: PathBuf,
}

impl TemplatePack {
    /// 从 `<root>/national/<set-id>/template.toml` 加载全国卷模板包。
    ///
    /// 任一模板数据不合法即整体失败（启动期显式错误，不静默跳过）。
    ///
    /// # Errors
    ///
    /// 目录不存在、TOML 解析失败、文件级校验失败时返回 [`RubricError`]。
    pub fn load(root: &Path) -> Result<Self> {
        let national = root.join("national");
        let entries = std::fs::read_dir(&national).map_err(|error| RubricError::TemplateIo {
            path: national.display().to_string(),
            message: error.to_string(),
        })?;

        let mut directories: Vec<PathBuf> = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| RubricError::TemplateIo {
                path: national.display().to_string(),
                message: error.to_string(),
            })?;
            let path = entry.path();
            if path.is_dir() {
                directories.push(path);
            }
        }
        directories.sort();

        let mut pack = Self {
            templates: Vec::new(),
            warnings: Vec::new(),
            root: root.to_path_buf(),
        };
        for directory in directories {
            let set_dir = directory
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_owned();
            let path = directory.join("template.toml");
            let template = load_template_file(&path, &set_dir)?;
            pack.insert(template)?;
        }
        if pack.templates.is_empty() {
            return Err(RubricError::TemplateInvalid {
                message: format!("模板包为空：{} 下没有可用模板", national.display()),
            });
        }
        Ok(pack)
    }

    /// 导入单个模板文件（自定义模板；`allow_custom_template` 由调用方按配置判断）。
    ///
    /// 文件名所在目录名作为 `set_id` 的兼容依据；与文件内 `set_id` 不一致即错误。
    ///
    /// # Errors
    ///
    /// 读取/解析/校验失败返回 [`RubricError`]。
    pub fn load_single(path: &Path) -> Result<LoadedTemplate> {
        let set_dir = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned();
        load_template_file(path, &set_dir)
    }

    /// 合并一个模板（id / set_id / 中文名不得与既有模板冲突；只追加、不覆盖）。
    ///
    /// # Errors
    ///
    /// 冲突时返回 [`RubricError::TemplateInvalid`]。
    pub fn insert(&mut self, template: LoadedTemplate) -> Result<()> {
        if let Some(existing) = self
            .templates
            .iter()
            .find(|existing| existing.template_id == template.template_id)
        {
            return Err(RubricError::TemplateInvalid {
                message: format!(
                    "模板 id 冲突：{} 已存在（版本 {}），模板只追加不改写",
                    existing.template_id, existing.version
                ),
            });
        }
        if let Some(existing) = self
            .templates
            .iter()
            .find(|existing| existing.set_id == template.set_id)
        {
            return Err(RubricError::TemplateInvalid {
                message: format!(
                    "卷型 id 冲突：{} 已存在（版本 {}）；改内容必须升版本",
                    existing.set_id, existing.version
                ),
            });
        }
        if let Some(existing) = self
            .templates
            .iter()
            .find(|existing| existing.display_name == template.display_name)
        {
            return Err(RubricError::TemplateInvalid {
                message: format!("卷型名称冲突：{} 已存在", existing.display_name),
            });
        }
        self.record_warnings(&template);
        self.templates.push(template);
        self.templates
            .sort_by(|left, right| left.set_id.cmp(&right.set_id));
        Ok(())
    }

    fn record_warnings(&mut self, template: &LoadedTemplate) {
        if let TemplateStatus::Retired { retired_since } = template.status {
            self.warnings.push(TemplateWarning {
                code: TemplateWarningCode::RetiredTemplate,
                set_id: template.set_id.clone(),
                message: format!(
                    "{} 于 {retired_since} 年起停用，仅作历史快照；套用需显式允许 retired",
                    template.display_name
                ),
            });
        }
        let multi_choice_needs_tiers = template
            .questions
            .iter()
            .any(|question| question.kind == QuestionKind::MultipleChoice);
        let any_skeleton_has_tiers = template
            .skeletons
            .values()
            .any(|skeleton| skeleton.partial_choice.is_some());
        if multi_choice_needs_tiers && !any_skeleton_has_tiers {
            self.warnings.push(TemplateWarning {
                code: TemplateWarningCode::PartialChoiceTiersMissing,
                set_id: template.set_id.clone(),
                message: format!(
                    "{} 含多选题，但模板骨架未提供部分给分档位（assets/templates/README.md §11 已知限制）；\
                     v1 只能按全对给分，部分给分档位待 T25 模板数据更新后启用",
                    template.display_name
                ),
            });
        }
        if !matches!(
            template.provenance.basis.as_str(),
            "official" | "public-analysis" | "inferred"
        ) {
            self.warnings.push(TemplateWarning {
                code: TemplateWarningCode::UnknownEnumValue,
                set_id: template.set_id.clone(),
                message: format!(
                    "provenance.basis 取值 {} 不在 README 枚举（official/public-analysis/inferred）内，已按兼容读取",
                    template.provenance.basis
                ),
            });
        }
    }

    /// 按 `set_id` 或中文卷型名查找。
    #[must_use]
    pub fn find(&self, key: &str) -> Option<&LoadedTemplate> {
        self.templates
            .iter()
            .find(|template| template.set_id == key || template.display_name == key)
    }

    /// 解析选择（retired 需显式允许；缺失即错误，禁止"无模板硬套"）。
    ///
    /// # Errors
    ///
    /// 模板缺失返回 [`RubricError::TemplateMissing`]；历史卷未允许返回
    /// [`RubricError::TemplateRetired`]。
    pub fn resolve(&self, selection: &TemplateSelection) -> Result<&LoadedTemplate> {
        let template = self
            .find(&selection.key)
            .ok_or_else(|| RubricError::TemplateMissing(selection.key.clone()))?;
        if let TemplateStatus::Retired { retired_since } = template.status
            && !selection.allow_retired
        {
            return Err(RubricError::TemplateRetired {
                set_id: template.set_id.clone(),
                retired_since,
            });
        }
        Ok(template)
    }

    /// 现行卷（`active`）模板列表。
    #[must_use]
    pub fn active_templates(&self) -> Vec<&LoadedTemplate> {
        self.templates
            .iter()
            .filter(|template| template.status.is_active())
            .collect()
    }

    /// 历史卷（`retired`）模板列表。
    #[must_use]
    pub fn retired_templates(&self) -> Vec<&LoadedTemplate> {
        self.templates
            .iter()
            .filter(|template| !template.status.is_active())
            .collect()
    }

    /// 告警里是否包含指定码。
    #[must_use]
    pub fn has_warning(&self, code: TemplateWarningCode) -> bool {
        self.warnings.iter().any(|warning| warning.code == code)
    }
}

fn load_template_file(path: &Path, expected_set_id: &str) -> Result<LoadedTemplate> {
    let text = std::fs::read_to_string(path).map_err(|error| RubricError::TemplateIo {
        path: path.display().to_string(),
        message: error.to_string(),
    })?;
    let raw: RawTemplate = toml::from_str(&text).map_err(|error| RubricError::TemplateIo {
        path: path.display().to_string(),
        message: error.to_string(),
    })?;
    normalize(raw, expected_set_id, &path.display().to_string())
}

fn normalize(raw: RawTemplate, expected_set_id: &str, source_path: &str) -> Result<LoadedTemplate> {
    if raw.schema_version != 1 {
        return Err(RubricError::TemplateInvalid {
            message: format!(
                "{source_path}: schema_version 必须为 1，实际 {}",
                raw.schema_version
            ),
        });
    }
    if raw.set_id != expected_set_id {
        return Err(RubricError::TemplateInvalid {
            message: format!(
                "{source_path}: set_id 必须与目录名一致（目录 {expected_set_id}，文件 {}）",
                raw.set_id
            ),
        });
    }
    if raw.subject != "math" {
        return Err(RubricError::TemplateInvalid {
            message: format!("{source_path}: v1 仅支持 math 学科，实际 {}", raw.subject),
        });
    }
    let status = match raw.status.as_str() {
        "active" => {
            if raw.retired_since.is_some() {
                return Err(RubricError::TemplateInvalid {
                    message: format!("{source_path}: active 模板不得填写 retired_since"),
                });
            }
            TemplateStatus::Active
        }
        "retired" => {
            let retired_since = raw
                .retired_since
                .ok_or_else(|| RubricError::TemplateInvalid {
                    message: format!("{source_path}: retired 模板必须填写 retired_since"),
                })?;
            TemplateStatus::Retired { retired_since }
        }
        other => {
            return Err(RubricError::TemplateInvalid {
                message: format!("{source_path}: status 只能是 active 或 retired，实际 {other}"),
            });
        }
    };
    let version = TemplateVersion::parse(&raw.version)?;
    if raw.display_name.trim().is_empty() {
        return Err(RubricError::TemplateInvalid {
            message: format!("{source_path}: display_name 不得为空"),
        });
    }
    let template_id = TemplateId::new(raw.template_id.clone())?;
    let total_score = MilliScore::from_milli(raw.total_score_milli)?;
    if total_score.is_zero() {
        return Err(RubricError::TemplateInvalid {
            message: format!("{source_path}: total_score_milli 必须大于 0"),
        });
    }
    if raw.questions.is_empty() {
        return Err(RubricError::TemplateInvalid {
            message: format!("{source_path}: questions 不得为空"),
        });
    }
    let count = u32::try_from(raw.questions.len())
        .map_err(|_| RubricError::template_invalid("题目数量超出 u32 上限"))?;
    if raw.question_count != count {
        return Err(RubricError::TemplateInvalid {
            message: format!(
                "{source_path}: question_count {} 与 questions 条数 {count} 不一致",
                raw.question_count
            ),
        });
    }

    let mut skeletons: BTreeMap<String, TemplateSkeleton> = BTreeMap::new();
    for (id, raw_skeleton) in raw.skeletons {
        let skeleton = normalize_skeleton(&id, &raw_skeleton, source_path)?;
        skeletons.insert(id, skeleton);
    }

    let mut questions = Vec::with_capacity(raw.questions.len());
    let mut ordinal_sum = MilliScore::ZERO;
    for (index, raw_question) in raw.questions.iter().enumerate() {
        let expected_ordinal = u32::try_from(index).map_or(u32::MAX, |value| value + 1);
        if raw_question.ordinal != expected_ordinal {
            return Err(RubricError::TemplateInvalid {
                message: format!(
                    "{source_path}: ordinal 必须从 1 起连续，第 {} 条为 {}",
                    expected_ordinal, raw_question.ordinal
                ),
            });
        }
        let kind = parse_question_kind(&raw_question.kind).ok_or_else(|| {
            RubricError::TemplateInvalid {
                message: format!(
                    "{source_path}: 题号 {} 的题型 {:?} 不受支持",
                    raw_question.ordinal, raw_question.kind
                ),
            }
        })?;
        let max_score = MilliScore::from_milli(raw_question.max_score_milli)?;
        if max_score.is_zero() {
            return Err(RubricError::TemplateInvalid {
                message: format!(
                    "{source_path}: 题号 {} 的 max_score_milli 必须大于 0",
                    raw_question.ordinal
                ),
            });
        }
        ordinal_sum = ordinal_sum.checked_add_score(max_score)?;
        let skeleton =
            skeletons
                .get(&raw_question.skeleton)
                .ok_or_else(|| RubricError::TemplateInvalid {
                    message: format!(
                        "{source_path}: 题号 {} 引用的骨架 {} 不存在",
                        raw_question.ordinal, raw_question.skeleton
                    ),
                })?;
        for item in &skeleton.items {
            convert_ratio(max_score, item.ratio_permille).map_err(|_| {
                RubricError::TemplateInvalid {
                    message: format!(
                        "{source_path}: 题号 {} 骨架 {} 第 {} 项比例 {}‰ 无法按满分 {} 精确折算",
                        raw_question.ordinal,
                        raw_question.skeleton,
                        item.ordinal,
                        item.ratio_permille,
                        max_score
                    ),
                }
            })?;
        }
        if !raw_question.label.trim().is_empty()
            && questions
                .iter()
                .any(|existing: &TemplateQuestion| existing.label == raw_question.label)
        {
            return Err(RubricError::TemplateInvalid {
                message: format!("{source_path}: 题号标签 {} 重复", raw_question.label),
            });
        }
        questions.push(TemplateQuestion {
            ordinal: raw_question.ordinal,
            label: raw_question.label.clone(),
            kind,
            max_score,
            skeleton: raw_question.skeleton.clone(),
            kind_note: raw_question.kind_note.clone(),
            choice_group: raw_question.choice_group.clone(),
        });
    }
    if ordinal_sum != total_score {
        return Err(RubricError::TemplateInvalid {
            message: format!(
                "{source_path}: 各题满分之和 {ordinal_sum} 不等于 total_score {total_score}"
            ),
        });
    }

    Ok(LoadedTemplate {
        template_id,
        set_id: raw.set_id,
        display_name: raw.display_name,
        subject: raw.subject,
        paper_style: raw.paper_style,
        status,
        version,
        snapshot_year: raw.snapshot_year,
        snapshot_label: raw.snapshot_label,
        total_score,
        question_count: raw.question_count,
        printed_max_ordinal: raw.printed_max_ordinal,
        structure_note: raw.structure_note,
        structure_confidence: parse_confidence(&raw.structure_confidence),
        score_split_confidence: parse_confidence(&raw.score_split_confidence),
        provenance: TemplateProvenance {
            verified_at: raw.provenance.verified_at,
            confidence: parse_confidence(&raw.provenance.confidence),
            basis: raw.provenance.basis,
            source_ids: raw.provenance.source_ids,
            disclaimer: raw.provenance.disclaimer,
        },
        questions,
        skeletons,
        source_path: source_path.to_owned(),
    })
}

fn normalize_skeleton(id: &str, raw: &RawSkeleton, source_path: &str) -> Result<TemplateSkeleton> {
    let mode =
        parse_scoring_mode(&raw.scoring_mode).ok_or_else(|| RubricError::TemplateInvalid {
            message: format!(
                "{source_path}: 骨架 {id} 的 scoring_mode {:?} 不受支持",
                raw.scoring_mode
            ),
        })?;
    if raw.items.is_empty() || raw.items.len() > MAX_ITEMS {
        return Err(RubricError::TemplateInvalid {
            message: format!(
                "{source_path}: 骨架 {id} 评分项数量必须在 1..={MAX_ITEMS}，实际 {}",
                raw.items.len()
            ),
        });
    }
    let mut items = Vec::with_capacity(raw.items.len());
    let mut ratio_sum: i64 = 0;
    for (index, raw_item) in raw.items.iter().enumerate() {
        let expected_ordinal = u32::try_from(index).map_or(u32::MAX, |value| value + 1);
        if raw_item.ordinal != expected_ordinal {
            return Err(RubricError::TemplateInvalid {
                message: format!(
                    "{source_path}: 骨架 {id} 的 ordinal 必须从 1 起连续，第 {} 条为 {}",
                    expected_ordinal, raw_item.ordinal
                ),
            });
        }
        if raw_item.criterion.trim().is_empty()
            || !raw_item
                .criterion
                .trim()
                .starts_with(TEMPLATE_CRITERION_MARKER)
        {
            return Err(RubricError::TemplateInvalid {
                message: format!(
                    "{source_path}: 骨架 {id} 第 {} 项判据必须以 {TEMPLATE_CRITERION_MARKER} 开头（模板只放占位）",
                    raw_item.ordinal
                ),
            });
        }
        if !raw_item.placeholder {
            return Err(RubricError::TemplateInvalid {
                message: format!(
                    "{source_path}: 骨架 {id} 第 {} 项 placeholder 必须为 true",
                    raw_item.ordinal
                ),
            });
        }
        if raw_item.ratio_permille <= 0 {
            return Err(RubricError::TemplateInvalid {
                message: format!(
                    "{source_path}: 骨架 {id} 第 {} 项 ratio_permille 必须为正",
                    raw_item.ordinal
                ),
            });
        }
        for dependency in &raw_item.depends_on {
            if *dependency < 1 || (*dependency as usize) > raw.items.len() {
                return Err(RubricError::TemplateInvalid {
                    message: format!(
                        "{source_path}: 骨架 {id} 第 {} 项依赖序号 {dependency} 不存在",
                        raw_item.ordinal
                    ),
                });
            }
        }
        ratio_sum = ratio_sum
            .checked_add(raw_item.ratio_permille)
            .ok_or_else(|| RubricError::template_invalid("骨架比例相加溢出"))?;
        items.push(TemplateSkeletonItem {
            ordinal: raw_item.ordinal,
            criterion: raw_item.criterion.clone(),
            ratio_permille: raw_item.ratio_permille,
            required: raw_item.required,
            depends_on: raw_item.depends_on.clone(),
            placeholder: raw_item.placeholder,
        });
    }
    if ratio_sum != 1000 {
        return Err(RubricError::TemplateInvalid {
            message: format!(
                "{source_path}: 骨架 {id} 的 ratio_permille 合计必须为 1000‰，实际 {ratio_sum}‰"
            ),
        });
    }
    if mode == ScoringMode::AllOrNothing && items.len() != 1 {
        return Err(RubricError::TemplateInvalid {
            message: format!(
                "{source_path}: 骨架 {id} 为全对才给分，必须恰好 1 个评分项，实际 {}",
                items.len()
            ),
        });
    }
    if let Some(cycle) = skeleton_cycle(&items) {
        return Err(RubricError::TemplateInvalid {
            message: format!(
                "{source_path}: 骨架 {id} 依赖成环：{}",
                cycle
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(" → ")
            ),
        });
    }
    let partial_choice = raw
        .partial_choice
        .as_ref()
        .map(|rule| normalize_partial_choice(rule, id, source_path))
        .transpose()?;
    Ok(TemplateSkeleton {
        id: id.to_owned(),
        mode,
        label: raw.label.clone(),
        scoring_note: raw.scoring_note.clone(),
        items,
        partial_choice,
    })
}

fn normalize_partial_choice(
    raw: &RawPartialChoice,
    skeleton_id: &str,
    source_path: &str,
) -> Result<TemplatePartialChoice> {
    if !matches!(raw.correct_option_count, 2 | 3) {
        return Err(RubricError::TemplateInvalid {
            message: format!(
                "{source_path}: 骨架 {skeleton_id} 的 correct_option_count 只能是 2 或 3，实际 {}",
                raw.correct_option_count
            ),
        });
    }
    if !raw.wrong_selection_zero {
        return Err(RubricError::TemplateInvalid {
            message: format!(
                "{source_path}: 骨架 {skeleton_id} 的 wrong_selection_zero 恒为真，模板数据不得关闭"
            ),
        });
    }
    let source = match raw.source.as_str() {
        "official" => PartialChoiceSource::Official,
        "template_data" => PartialChoiceSource::TemplateData,
        "teacher_custom" => PartialChoiceSource::TeacherCustom,
        other => {
            return Err(RubricError::TemplateInvalid {
                message: format!(
                    "{source_path}: 骨架 {skeleton_id} 的 partial_choice.source {other:?} 不受支持"
                ),
            });
        }
    };
    if source == PartialChoiceSource::Official
        && raw
            .source_note
            .as_deref()
            .is_none_or(|note| note.trim().is_empty())
    {
        return Err(RubricError::TemplateInvalid {
            message: format!(
                "{source_path}: 骨架 {skeleton_id} 的官方口径档位必须填写 source_note（出处 + 年份）"
            ),
        });
    }
    let mut tiers = Vec::with_capacity(raw.tiers.len());
    for tier in &raw.tiers {
        let amount = match (tier.ratio_permille, tier.points_milli) {
            (Some(ratio), None) => TemplateTierAmount::RatioPermille(ratio),
            (None, Some(milli)) => TemplateTierAmount::Milli(milli),
            _ => {
                return Err(RubricError::TemplateInvalid {
                    message: format!(
                        "{source_path}: 骨架 {skeleton_id} 档位 {} 必须且只能给 ratio_permille 或 points_milli 之一",
                        tier.selected_correct
                    ),
                });
            }
        };
        tiers.push(TemplatePartialChoiceTier {
            selected_correct: tier.selected_correct,
            amount,
        });
    }
    Ok(TemplatePartialChoice {
        correct_option_count: raw.correct_option_count,
        tiers,
        wrong_selection_zero: raw.wrong_selection_zero,
        source,
        source_note: raw.source_note.clone(),
    })
}

fn skeleton_cycle(items: &[TemplateSkeletonItem]) -> Option<Vec<u32>> {
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
        result: &mut Option<Vec<u32>>,
    ) {
        if result.is_some() {
            return;
        }
        match states[node] {
            State::Done => return,
            State::Visiting => {
                if let Some(start) = stack.iter().position(|value| *value == node) {
                    *result = Some(
                        stack[start..]
                            .iter()
                            .map(|value| u32::try_from(*value).map_or(u32::MAX, |v| v + 1))
                            .collect(),
                    );
                }
                return;
            }
            State::Unvisited => {}
        }
        states[node] = State::Visiting;
        stack.push(node);
        for target in &edges[node] {
            visit(*target, edges, states, stack, result);
        }
        stack.pop();
        states[node] = State::Done;
    }
    let mut states = vec![State::Unvisited; length];
    let mut stack = Vec::new();
    let mut result = None;
    for node in 0..length {
        visit(node, &edges, &mut states, &mut stack, &mut result);
        if result.is_some() {
            break;
        }
    }
    result
}

fn parse_confidence(text: &str) -> ConfidenceLevel {
    match text {
        "high" => ConfidenceLevel::High,
        "medium" => ConfidenceLevel::Medium,
        "low" => ConfidenceLevel::Low,
        other => ConfidenceLevel::Unknown(other.to_owned()),
    }
}

/// 解析题型（中文取值与 `QuestionKind` 的 snake_case 都接受，便于自定义模板导入）。
#[must_use]
pub fn parse_question_kind(text: &str) -> Option<QuestionKind> {
    match text.trim() {
        "单选" | "single_choice" => Some(QuestionKind::SingleChoice),
        "多选" | "multiple_choice" => Some(QuestionKind::MultipleChoice),
        "判断" | "true_false" => Some(QuestionKind::TrueFalse),
        "填空" | "fill_in_blank" => Some(QuestionKind::FillInBlank),
        "纯数值" | "numeric_only" => Some(QuestionKind::NumericOnly),
        "计算" | "计算/解答" | "calculation" => Some(QuestionKind::Calculation),
        "证明" | "proof" => Some(QuestionKind::Proof),
        "简答" | "short_answer" => Some(QuestionKind::ShortAnswer),
        "论述" | "essay" => Some(QuestionKind::Essay),
        "其他" | "other" => Some(QuestionKind::Other),
        _ => None,
    }
}

/// 解析计分方式（中文取值与 snake_case 都接受）。
#[must_use]
pub fn parse_scoring_mode(text: &str) -> Option<ScoringMode> {
    match text.trim() {
        "累加" | "additive" => Some(ScoringMode::Additive),
        "全对才给分" | "all_or_nothing" => Some(ScoringMode::AllOrNothing),
        "分步给分" | "stepwise" => Some(ScoringMode::Stepwise),
        "按选项部分给分" | "partial_choice" => Some(ScoringMode::PartialChoice),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// TOML 读取层（宽松：未知字段忽略；可选字段 default，保证模板演进时向后兼容）
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RawTemplate {
    schema_version: u32,
    template_id: String,
    set_id: String,
    display_name: String,
    subject: String,
    paper_style: String,
    status: String,
    #[serde(default)]
    retired_since: Option<u32>,
    version: String,
    snapshot_year: u32,
    snapshot_label: String,
    total_score_milli: i64,
    question_count: u32,
    #[serde(default)]
    printed_max_ordinal: Option<u32>,
    structure_note: String,
    structure_confidence: String,
    score_split_confidence: String,
    provenance: RawProvenance,
    #[serde(default)]
    questions: Vec<RawQuestion>,
    #[serde(default)]
    skeletons: BTreeMap<String, RawSkeleton>,
}

#[derive(Debug, Deserialize)]
struct RawProvenance {
    verified_at: String,
    confidence: String,
    basis: String,
    #[serde(default)]
    source_ids: Vec<String>,
    disclaimer: String,
}

#[derive(Debug, Deserialize)]
struct RawQuestion {
    ordinal: u32,
    label: String,
    kind: String,
    max_score_milli: i64,
    skeleton: String,
    #[serde(default)]
    kind_note: Option<String>,
    #[serde(default)]
    choice_group: Option<String>,
    /// 兼容保留字段：模板里出现但当前不使用。
    #[serde(default)]
    #[allow(dead_code)]
    choice_note: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawSkeleton {
    scoring_mode: String,
    label: String,
    #[serde(default)]
    scoring_note: Option<String>,
    #[serde(default)]
    items: Vec<RawSkeletonItem>,
    #[serde(default)]
    partial_choice: Option<RawPartialChoice>,
}

#[derive(Debug, Deserialize)]
struct RawSkeletonItem {
    ordinal: u32,
    criterion: String,
    ratio_permille: i64,
    required: bool,
    #[serde(default)]
    depends_on: Vec<u32>,
    placeholder: bool,
}

#[derive(Debug, Deserialize)]
struct RawPartialChoice {
    correct_option_count: u32,
    #[serde(default)]
    tiers: Vec<RawPartialChoiceTier>,
    wrong_selection_zero: bool,
    source: String,
    #[serde(default)]
    source_note: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawPartialChoiceTier {
    selected_correct: u32,
    #[serde(default)]
    ratio_permille: Option<i64>,
    #[serde(default)]
    points_milli: Option<i64>,
}

/// 模板包默认根目录（工作区 `assets/templates`；可用环境变量 `EG_TEMPLATE_ROOT` 覆盖）。
///
/// # Errors
///
/// 路径无法由 UTF-8 表示时返回 [`RubricError::TemplateIo`]。
pub fn default_template_root() -> Result<PathBuf> {
    if let Ok(root) = std::env::var("EG_TEMPLATE_ROOT") {
        return Ok(PathBuf::from(root));
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    Ok(manifest.join("../../assets/templates"))
}

/// 仅收集模板中出现的骨架 id（诊断用，测试与日志）。
#[must_use]
pub fn skeleton_ids(template: &LoadedTemplate) -> BTreeSet<String> {
    template.skeletons.keys().cloned().collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::ports::{NATIONAL_SET_IDS, RubricSettings};

    fn pack() -> TemplatePack {
        let root = default_template_root().unwrap();
        TemplatePack::load(&root).unwrap()
    }

    #[test]
    fn loads_four_national_templates() {
        let pack = pack();
        assert_eq!(pack.templates.len(), NATIONAL_SET_IDS.len());
        let set_ids: Vec<&str> = pack
            .templates
            .iter()
            .map(|template| template.set_id.as_str())
            .collect();
        for expected in NATIONAL_SET_IDS {
            assert!(set_ids.contains(&expected), "缺少模板 {expected}");
        }
        // 现行两套 active、历史两套 retired。
        assert_eq!(pack.active_templates().len(), 2);
        assert_eq!(pack.retired_templates().len(), 2);
        for template in pack.retired_templates() {
            assert!(template.status.retired_since().is_some());
        }
        // 四套总分均为 150 分，题量与题目条数一致。
        for template in &pack.templates {
            assert_eq!(template.total_score.as_milli(), 150_000);
            assert_eq!(
                u32::try_from(template.questions.len()).unwrap(),
                template.question_count
            );
        }
    }

    #[test]
    fn active_multi_choice_skeletons_have_official_tiers() {
        let pack = pack();
        // 现行卷多选已内置官方部分给分档位：不再记录「档位缺失」缺口。
        assert!(!pack.has_warning(TemplateWarningCode::PartialChoiceTiersMissing));
        assert!(!pack.has_warning(TemplateWarningCode::UnknownEnumValue));

        let multi_choice_templates: Vec<&LoadedTemplate> = pack
            .templates
            .iter()
            .filter(|template| {
                template
                    .questions
                    .iter()
                    .any(|question| question.kind == QuestionKind::MultipleChoice)
            })
            .collect();
        assert_eq!(multi_choice_templates.len(), 2);

        for template in &multi_choice_templates {
            // 3 选项口径：选对 1 个 2 分、选对 2 个 4 分，错选归零。
            let three = template
                .skeletons
                .get("objective-multiple")
                .expect("应有 3 选项多选骨架")
                .partial_choice
                .as_ref()
                .expect("3 选项多选骨架必须带档位");
            assert_eq!(three.correct_option_count, 3);
            assert!(three.wrong_selection_zero);
            assert_eq!(three.source, PartialChoiceSource::Official);
            assert!(
                three
                    .source_note
                    .as_deref()
                    .is_some_and(|note| !note.is_empty())
            );
            let points: Vec<i64> = three
                .tiers
                .iter()
                .map(|tier| match tier.amount {
                    TemplateTierAmount::Milli(milli) => milli,
                    TemplateTierAmount::RatioPermille(ratio) => ratio,
                })
                .collect();
            assert_eq!(three.tiers.len(), 2);
            assert_eq!(
                three
                    .tiers
                    .iter()
                    .map(|tier| tier.selected_correct)
                    .collect::<Vec<_>>(),
                vec![1, 2]
            );
            assert_eq!(points, vec![2000, 4000]);

            // 2 选项备选口径：选对 1 个 3 分。
            let two = template
                .skeletons
                .get("objective-multiple-2")
                .expect("应有 2 选项备选骨架")
                .partial_choice
                .as_ref()
                .expect("2 选项骨架必须带档位");
            assert_eq!(two.correct_option_count, 2);
            assert_eq!(two.tiers.len(), 1);
            assert_eq!(two.tiers[0].selected_correct, 1);
            assert!(matches!(
                two.tiers[0].amount,
                TemplateTierAmount::Milli(3000)
            ));

            // 套用多选骨架时档位随计划产出，total_milli 由题目满分填充。
            let multi = template
                .questions
                .iter()
                .find(|question| question.kind == QuestionKind::MultipleChoice)
                .expect("现行卷应含多选题");
            let plan = template
                .plan(
                    multi.ordinal,
                    QuestionKind::MultipleChoice,
                    None,
                    &RubricSettings::default(),
                )
                .expect("多选骨架可套用");
            assert_eq!(plan.mode, ScoringMode::PartialChoice);
            let planned = plan.partial_choice.expect("套用计划应带档位");
            assert_eq!(planned.total_milli, multi.max_score);
            assert_eq!(planned.correct_option_count, 3);
            assert_eq!(planned.tiers.len(), 2);
        }

        // 老高考卷（甲/乙）不含多选，不应因档位缺口产生告警。
        for template in pack
            .templates
            .iter()
            .filter(|template| !template.status.is_active())
        {
            assert!(
                !template
                    .questions
                    .iter()
                    .any(|question| question.kind == QuestionKind::MultipleChoice),
                "{} 不应含多选题",
                template.set_id
            );
        }
    }

    #[test]
    fn active_and_retired_resolution_policy() {
        let pack = pack();
        let active = pack
            .resolve(&TemplateSelection {
                key: "新课标Ⅰ卷".to_owned(),
                allow_retired: false,
            })
            .unwrap();
        assert_eq!(active.set_id, "new-curriculum-1");
        assert_eq!(active.version.to_string(), "2026.2");

        // 历史卷默认拒绝，显式允许后可套用。
        let retired_key = TemplateSelection {
            key: "national-jia".to_owned(),
            allow_retired: false,
        };
        assert!(matches!(
            pack.resolve(&retired_key).unwrap_err(),
            RubricError::TemplateRetired { .. }
        ));
        let allowed = pack
            .resolve(&TemplateSelection {
                key: "national-jia".to_owned(),
                allow_retired: true,
            })
            .unwrap();
        assert_eq!(allowed.status.retired_since(), Some(2026));
    }

    #[test]
    fn missing_template_is_error() {
        let pack = pack();
        let error = pack
            .resolve(&TemplateSelection {
                key: "不存在的卷".to_owned(),
                allow_retired: true,
            })
            .unwrap_err();
        assert!(matches!(error, RubricError::TemplateMissing(_)));
    }

    #[test]
    fn plan_keeps_placeholder_and_exact_points() {
        let pack = pack();
        let template = pack
            .resolve(&TemplateSelection {
                key: "new-curriculum-1".to_owned(),
                allow_retired: false,
            })
            .unwrap();
        let settings = RubricSettings::default();
        let plan = template
            .plan(1, QuestionKind::SingleChoice, None, &settings)
            .unwrap();
        assert_eq!(plan.mode, ScoringMode::AllOrNothing);
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.items[0].points.as_milli(), 5000);
        assert!(
            plan.items[0]
                .criterion
                .starts_with(TEMPLATE_CRITERION_MARKER)
        );
        assert!(plan.partial_choice.is_none());

        // 解答题 3 步骨架按 17 分折算：2.9 分 + 4 分 + 5.1 分 = 12 分？不——按 17 分 1000‰ 合计。
        let plan = template
            .plan(19, QuestionKind::Calculation, None, &settings)
            .unwrap();
        let sum: i64 = plan.items.iter().map(|item| item.points.as_milli()).sum();
        assert_eq!(sum, 17_000);
        assert_eq!(plan.mode, ScoringMode::Stepwise);
    }

    #[test]
    fn plan_rejects_kind_mismatch_and_missing_ordinal() {
        let pack = pack();
        let template = pack.find("new-curriculum-1").expect("模板应存在");
        let settings = RubricSettings::default();
        assert!(matches!(
            template
                .plan(1, QuestionKind::Calculation, None, &settings)
                .unwrap_err(),
            RubricError::TemplateKindMismatch { .. }
        ));
        assert!(matches!(
            template
                .plan(99, QuestionKind::SingleChoice, None, &settings)
                .unwrap_err(),
            RubricError::TemplateQuestionMissing { .. }
        ));
    }

    #[test]
    fn plan_supports_skeleton_override_for_proof_kind_note() {
        let pack = pack();
        let template = pack.find("new-curriculum-1").expect("模板应存在");
        let settings = RubricSettings::default();
        // 题号 19 为计算/解答；kind_note 提示可按证明骨架套用——但题型不匹配仍拒绝。
        let error = template
            .plan(19, QuestionKind::Proof, Some("step-proof"), &settings)
            .unwrap_err();
        assert!(matches!(error, RubricError::TemplateKindMismatch { .. }));
    }

    #[test]
    fn parse_helpers_accept_chinese_and_snake_case() {
        assert_eq!(
            parse_question_kind("多选"),
            Some(QuestionKind::MultipleChoice)
        );
        assert_eq!(
            parse_question_kind("multiple_choice"),
            Some(QuestionKind::MultipleChoice)
        );
        assert_eq!(parse_question_kind("未知"), None);
        assert_eq!(parse_scoring_mode("分步给分"), Some(ScoringMode::Stepwise));
        assert_eq!(
            parse_scoring_mode("partial_choice"),
            Some(ScoringMode::PartialChoice)
        );
        assert_eq!(parse_scoring_mode("xx"), None);
    }

    #[test]
    fn loading_invalid_toml_fails_closed() {
        let directory =
            std::env::temp_dir().join(format!("eg-rubric-bad-template-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("template.toml");
        std::fs::write(&path, "schema_version = 1\n").unwrap();
        let result = TemplatePack::load_single(&path);
        assert!(matches!(result, Err(RubricError::TemplateIo { .. })));
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn duplicate_template_id_is_rejected() {
        let mut empty = TemplatePack::default();
        let template = pack().templates.first().cloned().expect("模板包非空");
        empty.insert(template.clone()).unwrap();
        let error = empty.insert(template).unwrap_err();
        assert!(matches!(error, RubricError::TemplateInvalid { .. }));
    }
}
