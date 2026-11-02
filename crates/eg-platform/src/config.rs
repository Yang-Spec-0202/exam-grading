//! 配置（`CONFIG.md` 全部键的强类型映射，`SPEC-platform.md` §2）。
//!
//! - 缺键补齐：反序列化时缺失字段取本节 `Default`。
//! - 非法值拒绝：未知键、越界值、非法枚举均在加载期报中文错误，绝不静默回默认。
//! - 原子写：临时文件 + `fsync` + `rename`，保留一代 `.bak`。
//! - 损坏处理：原文件改名 `config.toml.broken-<时间戳>`，返回带中文引导的错误，
//!   由调用方选择「修复 / 恢复上一版 / 重置默认」。
//! - `meta` 节只由本模块写；`audit.enabled`、`rubric.require_confirmation`、
//!   `privacy.per_task_consent`、`dev.research_first` 等不变量**不出现在配置结构**里。

use std::collections::BTreeMap;
use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use eg_domain::{GradeMode, MilliScore, Timestamp, Ulid};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::clock::SharedClock;
use crate::error::{ErrorClass, PlatformError, Result};
use crate::ids::UlidFactory;
use crate::logging::LogLevel;
use crate::paths::{CONFIG_FILE, DataDirs};

/// 配置文件的 schema 版本（`meta.schema_version`）。
pub const CONFIG_SCHEMA_VERSION: u32 = 1;

/// 允许的评分粒度（千分之一分）：0.01 / 0.1 / 0.5 / 1 分。
pub const SCORE_GRANULARITIES: [i64; 4] = [10, 100, 500, 1_000];

/// 未知配置键或非法值的诊断项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigIssue {
    /// 点分键路径（如 `log.retention_days`）。
    pub key: String,
    /// 中文说明（教师可读）。
    pub message: String,
}

impl ConfigIssue {
    /// 构造诊断项。
    #[must_use]
    pub fn new(key: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for ConfigIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}：{}", self.key, self.message)
    }
}

/// 配置错误。
#[derive(Debug)]
pub enum ConfigError {
    /// 一项或多项非法值。
    Invalid {
        /// 诊断项（按键路径）。
        issues: Vec<ConfigIssue>,
    },

    /// 配置文件损坏（已改名保留现场）。
    Corrupted {
        /// 原路径。
        path: PathBuf,
        /// 改名后的现场路径。
        broken_path: PathBuf,
        /// 解析错误摘要。
        message: String,
    },

    /// 文件读写失败。
    Io {
        /// 路径。
        path: PathBuf,
        /// 底层错误。
        source: std::io::Error,
    },

    /// TOML 解析失败（非损坏，例如类型不匹配）。
    Parse {
        /// 路径。
        path: PathBuf,
        /// 中文说明。
        message: String,
    },

    /// `.bak` 不存在，无法恢复上一版。
    BackupMissing {
        /// 备份路径。
        path: PathBuf,
    },

    /// 序列化失败。
    Serialize(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid { issues } => {
                write!(f, "配置有 {} 处非法：{}", issues.len(), summarize(issues))
            }
            Self::Corrupted {
                path,
                broken_path,
                message,
            } => write!(
                f,
                "配置文件损坏（{}）：{message}。已保留为 {}；请选择：修复该文件 / 恢复上一版 .bak / 重置为默认",
                path.display(),
                broken_path.display()
            ),
            Self::Io { path, source } => {
                write!(f, "配置文件读写失败（{}）：{source}", path.display())
            }
            Self::Parse { path, message } => {
                write!(f, "配置解析失败（{}）：{message}", path.display())
            }
            Self::BackupMissing { path } => {
                write!(f, "没有可恢复的上一版配置（{}）", path.display())
            }
            Self::Serialize(message) => write!(f, "配置序列化失败：{message}"),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl ConfigError {
    /// 构造「非法值」错误。
    #[must_use]
    pub fn invalid(issues: Vec<ConfigIssue>) -> Self {
        Self::Invalid { issues }
    }
}

impl ConfigError {
    /// 错误分类：配置问题均为教师可修复；损坏的文件保留现场供恢复。
    #[must_use]
    pub fn class(&self) -> ErrorClass {
        ErrorClass::TeacherFixable
    }
}

fn summarize(issues: &[ConfigIssue]) -> String {
    issues
        .iter()
        .take(3)
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("；")
}

// ---------------------------------------------------------------------------
// 枚举：均带中文展示名；序列化使用稳定机器码。
// ---------------------------------------------------------------------------

/// 界面语言（v1 仅 zh-CN）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Language {
    /// 中文（简体）。
    #[default]
    #[serde(rename = "zh-CN")]
    ZhCn,
}

impl Language {
    /// 中文展示名。
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ZhCn => "中文（zh-CN）",
        }
    }
}

/// 主题。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    /// 跟随系统。
    #[default]
    System,
    /// 浅色。
    Light,
    /// 深色。
    Dark,
}

impl Theme {
    /// 中文展示名。
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::System => "跟随系统",
            Self::Light => "浅色",
            Self::Dark => "深色",
        }
    }
}

/// 启动页（v1 仅首页；其余页面由 `app` 扩展）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartPage {
    /// 首页。
    #[default]
    Home,
}

/// 窗口几何记忆策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowGeometry {
    /// 自动记忆。
    #[default]
    Auto,
    /// 固定。
    Fixed,
}

/// 导入拒收行为。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectBehavior {
    /// 页级拒收、批次继续。
    #[default]
    PageReject,
    /// 整批中止。
    AbortBatch,
}

/// 身份来源（v1 仅文件名）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentitySource {
    /// 文件名（v1 唯一启用）。
    #[default]
    FileName,
}

/// 文件名未命中时的处理。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnmatchedBehavior {
    /// 未决（人工处理）。
    #[default]
    Undecided,
    /// 自动归入「名单外」（不自动判分）。
    RosterExternal,
}

/// 段级识别触发策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentRecognition {
    /// 关闭。
    Off,
    /// 按需（默认）。
    #[default]
    OnDemand,
    /// 全量。
    Full,
}

/// 规则与模型冲突时的策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    /// 升级人工（默认）。
    #[default]
    EscalateManual,
    /// 规则优先。
    RuleFirst,
}

/// 抽检随机种子。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SamplingSeed {
    /// 随机（记录实际种子）。
    #[default]
    Random,
    /// 固定整数（可复现）。
    Fixed(i64),
}

impl Serialize for SamplingSeed {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Self::Random => serializer.serialize_str("random"),
            Self::Fixed(value) => serializer.serialize_i64(*value),
        }
    }
}

impl<'de> Deserialize<'de> for SamplingSeed {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = SamplingSeed;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("整数种子或字符串 \"random\"")
            }

            fn visit_i64<E: serde::de::Error>(
                self,
                value: i64,
            ) -> std::result::Result<Self::Value, E> {
                Ok(SamplingSeed::Fixed(value))
            }

            fn visit_u64<E: serde::de::Error>(
                self,
                value: u64,
            ) -> std::result::Result<Self::Value, E> {
                i64::try_from(value)
                    .map(SamplingSeed::Fixed)
                    .map_err(|_| E::custom("随机种子超出 i64 范围"))
            }

            fn visit_str<E: serde::de::Error>(
                self,
                value: &str,
            ) -> std::result::Result<Self::Value, E> {
                if value.eq_ignore_ascii_case("random") {
                    Ok(SamplingSeed::Random)
                } else {
                    value
                        .parse::<i64>()
                        .map(SamplingSeed::Fixed)
                        .map_err(|_| E::custom("随机种子必须是整数或 \"random\""))
                }
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

/// 抽检不通过时的行为。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailAction {
    /// 弹窗询问（默认）。
    #[default]
    Ask,
    /// 自动整卷重跑。
    AutoWholePaper,
    /// 自动单题重跑。
    AutoSingleQuestion,
}

/// 抽检不通过后的默认重跑范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailScope {
    /// 单题（默认）。
    #[default]
    SingleQuestion,
    /// 整卷。
    WholePaper,
}

/// 评分规则模板包。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemplatePack {
    /// 全国卷模板包（默认）。
    #[default]
    NationalPack,
    /// 自定义模板。
    Custom,
    /// 两者并存。
    Both,
}

/// 日志轮转策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogRotation {
    /// 按天（默认）。
    #[default]
    Daily,
    /// 单文件 50MB。
    #[serde(rename = "size_50mb")]
    Size50Mb,
}

impl LogRotation {
    /// 中文展示名。
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Daily => "按天",
            Self::Size50Mb => "单文件 50MB",
        }
    }
}

/// 文档发布目标。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublishTarget {
    /// GitHub Pages（默认）。
    #[default]
    GitHubPages,
    /// 仅本地。
    Local,
}

/// 本地 sidecar 端口策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LocalPort {
    /// 自动分配（默认）。
    #[default]
    Auto,
    /// 固定端口。
    Fixed(u16),
}

impl Serialize for LocalPort {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Self::Auto => serializer.serialize_str("auto"),
            Self::Fixed(port) => serializer.serialize_u16(*port),
        }
    }
}

impl<'de> Deserialize<'de> for LocalPort {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = LocalPort;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("端口号或字符串 \"auto\"")
            }

            fn visit_u64<E: serde::de::Error>(
                self,
                value: u64,
            ) -> std::result::Result<Self::Value, E> {
                u16::try_from(value)
                    .map(LocalPort::Fixed)
                    .map_err(|_| E::custom("端口必须在 1-65535 之间"))
            }

            fn visit_i64<E: serde::de::Error>(
                self,
                value: i64,
            ) -> std::result::Result<Self::Value, E> {
                u16::try_from(value)
                    .map(LocalPort::Fixed)
                    .map_err(|_| E::custom("端口必须在 1-65535 之间"))
            }

            fn visit_str<E: serde::de::Error>(
                self,
                value: &str,
            ) -> std::result::Result<Self::Value, E> {
                if value.eq_ignore_ascii_case("auto") {
                    Ok(LocalPort::Auto)
                } else {
                    value
                        .parse::<u16>()
                        .map(LocalPort::Fixed)
                        .map_err(|_| E::custom("端口必须是 1-65535 的整数或 \"auto\""))
                }
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

// ---------------------------------------------------------------------------
// 各节配置结构：`#[serde(default)]` 实现「缺键补齐」。
// ---------------------------------------------------------------------------

/// `general.*`（CONFIG §1）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    /// `general.mode`：默认运行模式。
    pub mode: GradeMode,
    /// `general.language`：界面语言（v1 仅 zh-CN）。
    pub language: Language,
    /// `general.theme`：主题。
    pub theme: Theme,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            mode: GradeMode::Standard,
            language: Language::ZhCn,
            theme: Theme::System,
        }
    }
}

/// `ui.*`（CONFIG §1）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    /// `ui.page_size`：队列/列表每页条数（20–500）。
    pub page_size: u32,
    /// `ui.undo_depth`：复核撤销栈深度（10–200）。
    pub undo_depth: u32,
    /// `ui.start_page`：启动页。
    pub start_page: StartPage,
    /// `ui.confirm_before_leave_unsaved`：离开编辑页前确认。
    pub confirm_before_leave_unsaved: bool,
    /// `ui.window_geometry`：窗口几何记忆。
    pub window_geometry: WindowGeometry,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            page_size: 100,
            undo_depth: 50,
            start_page: StartPage::Home,
            confirm_before_leave_unsaved: true,
            window_geometry: WindowGeometry::Auto,
        }
    }
}

/// `import.*`（CONFIG §2）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImportConfig {
    /// `import.remind_naming`：选择文件时提示规范命名。
    pub remind_naming: bool,
    /// `import.batch_file_limit`：单批文件数上限（1–10000）。
    pub batch_file_limit: u32,
    /// `import.pdf_dpi`：PDF 拆页分辨率（150–600）。
    pub pdf_dpi: u32,
    /// `import.quality_blur_threshold`：模糊阈值（60–300）。
    pub quality_blur_threshold: u32,
    /// `import.quality_exposure_min`：欠曝下限（0.05–0.3）。
    pub quality_exposure_min: f64,
    /// `import.quality_exposure_max`：过曝上限（0.7–0.98）。
    pub quality_exposure_max: f64,
    /// `import.quality_skew_max`：倾斜上限（度，3–15）。
    pub quality_skew_max: f64,
    /// `import.quality_min_long_edge`：分辨率下限（1200–3000 px）。
    pub quality_min_long_edge: u32,
    /// `import.reject_behavior`：拒收行为。
    pub reject_behavior: RejectBehavior,
    /// `import.duplicate_notice`：重复导入提示。
    pub duplicate_notice: bool,
    /// `import.max_pdf_pages`：单份 PDF 最大页数（1–1000）。
    pub max_pdf_pages: u32,
    /// `import.max_file_mb`：单文件大小上限（1–512 MB）。
    pub max_file_mb: u32,
    /// `import.parallelism`：预处理并行度（1–8）。
    pub parallelism: u32,
}

impl Default for ImportConfig {
    fn default() -> Self {
        Self {
            remind_naming: true,
            batch_file_limit: 2_000,
            pdf_dpi: 300,
            quality_blur_threshold: 120,
            quality_exposure_min: 0.12,
            quality_exposure_max: 0.90,
            quality_skew_max: 8.0,
            quality_min_long_edge: 1_600,
            reject_behavior: RejectBehavior::PageReject,
            duplicate_notice: true,
            max_pdf_pages: 200,
            max_file_mb: 64,
            parallelism: 4,
        }
    }
}

/// `identity.*`（CONFIG §3）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct IdentityConfig {
    /// `identity.source`：身份来源（v1 唯一启用文件名）。
    pub source: IdentitySource,
    /// `identity.ocr_fallback`：文件名未命中时 OCR 兜底（默认关）。
    pub ocr_fallback: bool,
    /// `identity.barcode`：条码身份（v1 无解码器，默认关）。
    pub barcode: bool,
    /// `identity.unmatched_behavior`：未命中处理（默认未决）。
    pub unmatched_behavior: UnmatchedBehavior,
    /// `identity.ocr_threshold`：兜底自动确认下限（0.7–0.99）。
    pub ocr_threshold: f64,
    /// `identity.ocr_margin`：Top-1/Top-2 分差下限（0.02–0.3）。
    pub ocr_margin: f64,
}

impl Default for IdentityConfig {
    fn default() -> Self {
        Self {
            source: IdentitySource::FileName,
            ocr_fallback: false,
            barcode: false,
            unmatched_behavior: UnmatchedBehavior::Undecided,
            ocr_threshold: 0.90,
            ocr_margin: 0.10,
        }
    }
}

/// `align.*`（CONFIG §3）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AlignConfig {
    /// `align.template_region`：优先使用试卷模板区域。
    pub template_region: bool,
    /// `align.region_confidence_floor`：区域对齐可用下限（0.5–0.99）。
    pub region_confidence_floor: f64,
    /// `align.max_cross_pages`：单题跨页上限（1–5）。
    pub max_cross_pages: u32,
}

impl Default for AlignConfig {
    fn default() -> Self {
        Self {
            template_region: true,
            region_confidence_floor: 0.80,
            max_cross_pages: 3,
        }
    }
}

/// `ocr.*`（CONFIG §4）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OcrConfig {
    /// `ocr.model`：默认 OCR 模型。
    pub model: String,
    /// `ocr.emit_latex`：公式输出 LaTeX。
    pub emit_latex: bool,
    /// `ocr.segment_recognition`：段级识别触发策略（默认按需）。
    pub segment_recognition: SegmentRecognition,
    /// `ocr.page_timeout_s`：单页识别超时（10–180 秒）。
    pub page_timeout_s: u32,
    /// `ocr.repair_attempts`：机械修复次数（0–2）。
    pub repair_attempts: u32,
    /// `ocr.confidence_floor`：低于该值进人工（0.5–0.95）。
    pub confidence_floor: f64,
    /// `ocr.cache`：识别结果缓存。
    pub cache: bool,
}

impl Default for OcrConfig {
    fn default() -> Self {
        Self {
            model: "PaddleOCR-VL-1.6".to_owned(),
            emit_latex: true,
            segment_recognition: SegmentRecognition::OnDemand,
            page_timeout_s: 60,
            repair_attempts: 1,
            confidence_floor: 0.75,
            cache: true,
        }
    }
}

/// 标准模式阈值（CONFIG §5）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StandardThresholds {
    /// `grade.standard.rule_threshold`（0.5–0.99）。
    pub rule_threshold: f64,
    /// `grade.standard.rule_then_model_threshold`（0.5–0.99）。
    pub rule_then_model_threshold: f64,
    /// `grade.standard.model_threshold`（0.5–0.99）。
    pub model_threshold: f64,
    /// `grade.standard.rubric_threshold`（0.5–0.99）。
    pub rubric_threshold: f64,
}

impl Default for StandardThresholds {
    fn default() -> Self {
        Self {
            rule_threshold: 0.85,
            rule_then_model_threshold: 0.90,
            model_threshold: 0.85,
            rubric_threshold: 0.80,
        }
    }
}

/// 快速模式阈值（CONFIG §5；其余三类由 `grade` 按比例下调 0.10）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FastThresholds {
    /// `grade.fast.rule_threshold`（0.5–0.99）。
    pub rule_threshold: f64,
}

impl Default for FastThresholds {
    fn default() -> Self {
        Self {
            rule_threshold: 0.80,
        }
    }
}

/// `grade.*`（CONFIG §5）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GradeConfig {
    /// `grade.score_granularity`：判分粒度（10/100/500/1000 千分之一分）。
    pub score_granularity: MilliScore,
    /// `grade.standard.*`。
    pub standard: StandardThresholds,
    /// `grade.fast.*`。
    pub fast: FastThresholds,
    /// `grade.allow_uncalibrated_auto`：允许未校准模型自动放行（默认关）。
    pub allow_uncalibrated_auto: bool,
    /// `grade.conflict_policy`：冲突策略（默认升级人工）。
    pub conflict_policy: ConflictPolicy,
    /// `grade.max_repair_attempts`：判定输出修复次数（0–2）。
    pub max_repair_attempts: u32,
    /// `grade.judge_timeout_s`：单题判定超时（5–60 秒）。
    pub judge_timeout_s: u32,
    /// `grade.model_profile`：L2 判定配置名。
    pub model_profile: String,
}

impl Default for GradeConfig {
    fn default() -> Self {
        Self {
            score_granularity: MilliScore::from_milli(100).unwrap_or(MilliScore::ZERO),
            standard: StandardThresholds::default(),
            fast: FastThresholds::default(),
            allow_uncalibrated_auto: false,
            conflict_policy: ConflictPolicy::EscalateManual,
            max_repair_attempts: 1,
            judge_timeout_s: 20,
            model_profile: "4B 指令模型 + 受限输出".to_owned(),
        }
    }
}

/// `fast.*`（CONFIG §6）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FastConfig {
    /// `fast.enabled`：是否允许进入快速模式。
    pub enabled: bool,
    /// `fast.sampling_rate`：抽检比例（0.05–1.0）。
    pub sampling_rate: f64,
    /// `fast.low_confidence_weight`：低置信度权重（0–1）。
    pub low_confidence_weight: f64,
    /// `fast.sampling_seed`：随机种子（random / 固定整数）。
    pub sampling_seed: SamplingSeed,
    /// `fast.sampled_readonly`：抽检队列只读。
    pub sampled_readonly: bool,
    /// `fast.fail_action`：抽检不通过行为（默认弹窗询问）。
    pub fail_action: FailAction,
    /// `fast.fail_default_scope`：默认重跑范围（默认单题）。
    pub fail_default_scope: FailScope,
    /// `fast.export_label`：导出标注「快速」。
    pub export_label: bool,
}

impl Default for FastConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            sampling_rate: 0.20,
            low_confidence_weight: 0.70,
            sampling_seed: SamplingSeed::Random,
            sampled_readonly: true,
            fail_action: FailAction::Ask,
            fail_default_scope: FailScope::SingleQuestion,
            export_label: true,
        }
    }
}

/// 全国卷模板集合（现行 active / 历史快照 retired）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NationalSets {
    /// 现行卷（可套用）。
    pub active: Vec<String>,
    /// 历史快照（只读，仅历史试卷复现）。
    pub retired: Vec<String>,
}

impl Default for NationalSets {
    fn default() -> Self {
        Self {
            active: vec!["新课标Ⅰ卷".to_owned(), "新课标Ⅱ卷".to_owned()],
            retired: vec!["全国甲卷".to_owned(), "全国乙卷".to_owned()],
        }
    }
}

/// `rubric.*`（CONFIG §7）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RubricConfig {
    /// `rubric.template_pack`：模板包来源。
    pub template_pack: TemplatePack,
    /// `rubric.national_sets`：全国卷模板集合。
    pub national_sets: NationalSets,
    /// `rubric.partial_choice_enabled`：多选部分给分总开关。
    pub partial_choice_enabled: bool,
    /// `rubric.min_score_granularity`：评分项最小粒度（0.1/0.5/1 分）。
    pub min_score_granularity: MilliScore,
    /// `rubric.auto_validate`：保存草稿时自动静态校验。
    pub auto_validate: bool,
    /// `rubric.allow_custom_template`：允许自定义模板。
    pub allow_custom_template: bool,
    /// `rubric.template_snapshot`：模板包数据快照版本。
    pub template_snapshot: String,
    /// `rubric.repair_attempts`：规则整理修复次数（0–2）。
    pub repair_attempts: u32,
}

impl Default for RubricConfig {
    fn default() -> Self {
        Self {
            template_pack: TemplatePack::NationalPack,
            national_sets: NationalSets::default(),
            partial_choice_enabled: true,
            min_score_granularity: MilliScore::from_milli(100).unwrap_or(MilliScore::ZERO),
            auto_validate: true,
            allow_custom_template: true,
            template_snapshot: "latest".to_owned(),
            repair_attempts: 1,
        }
    }
}

/// `cas.*`（CONFIG §8）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CasConfig {
    /// `cas.enabled`：符号比较开关。
    pub enabled: bool,
    /// `cas.backend`：CAS 后端（v1 仅 sympy sidecar）。
    pub backend: String,
    /// `cas.timeout_s`：单次化简超时（1–10 秒）。
    pub timeout_s: u32,
    /// `cas.python_path`：解释器绝对路径（None = 自动探测）。
    pub python_path: Option<PathBuf>,
    /// `cas.cache`：表达式结果缓存。
    pub cache: bool,
}

impl Default for CasConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            backend: "sympy-sidecar".to_owned(),
            timeout_s: 3,
            python_path: None,
            cache: true,
        }
    }
}

/// 索引重建策略（`log.index_rebuild`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct IndexRebuildPolicy {
    /// 启动时校验索引与日志段一致性（缺失/截断即重建对应段）。
    pub on_startup_check: bool,
    /// 提供手动全量重建。
    pub manual: bool,
    /// 索引 schema 版本变化时强制重建。
    pub on_schema_change: bool,
}

impl Default for IndexRebuildPolicy {
    fn default() -> Self {
        Self {
            on_startup_check: true,
            manual: true,
            on_schema_change: true,
        }
    }
}

/// `log.*`（CONFIG §9）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LogConfig {
    /// `log.level`：全局默认级别。
    pub level: LogLevel,
    /// `log.per_module_level`：模块级覆盖（模块名 → 级别）。
    pub per_module_level: BTreeMap<String, LogLevel>,
    /// `log.retention_days`：日志保留天数（7–365）。
    pub retention_days: u32,
    /// `log.rotation`：轮转策略。
    pub rotation: LogRotation,
    /// `log.redaction`：脱敏开关（关闭需二次确认，由 `app` 负责交互）。
    pub redaction: bool,
    /// `log.search_index`：日志查看器全文索引。
    pub search_index: bool,
    /// `log.ui_viewer`：应用内日志查看器。
    pub ui_viewer: bool,
    /// `log.index_range_days`：全文索引覆盖范围（7–365）。
    pub index_range_days: u32,
    /// `log.index_rebuild`：索引重建策略。
    pub index_rebuild: IndexRebuildPolicy,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: LogLevel::Info,
            per_module_level: BTreeMap::new(),
            retention_days: 30,
            rotation: LogRotation::Daily,
            redaction: true,
            search_index: true,
            ui_viewer: true,
            index_range_days: 30,
            index_rebuild: IndexRebuildPolicy::default(),
        }
    }
}

/// `diagnostics.*`（CONFIG §9）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiagnosticsConfig {
    /// `diagnostics.include_schema`：诊断包是否含 schema/迁移清单。
    pub include_schema: bool,
}

impl Default for DiagnosticsConfig {
    fn default() -> Self {
        Self {
            include_schema: true,
        }
    }
}

/// `storage.*`（CONFIG §9.5）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    /// `storage.soft_quota_gb`：资产占用告警线（5–500 GB，只告警不删除）。
    pub soft_quota_gb: u32,
    /// `storage.verify_hash_on_read`：读取/导出/备份前校验哈希。
    pub verify_hash_on_read: bool,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            soft_quota_gb: 20,
            verify_hash_on_read: true,
        }
    }
}

/// `backup.*`（CONFIG §9.5）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BackupConfig {
    /// `backup.keep_generations`：备份保留代数（1–20）。
    pub keep_generations: u32,
}

impl Default for BackupConfig {
    fn default() -> Self {
        Self {
            keep_generations: 5,
        }
    }
}

/// `inference.*`（CONFIG §9.5）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InferenceConfig {
    /// `inference.local_port`：本地 sidecar 端口策略。
    pub local_port: LocalPort,
    /// `inference.registry_path`：模型注册表路径（相对数据根）。
    pub registry_path: PathBuf,
    /// `inference.openai_base_url`：云端 OpenAI 兼容端点（None = 云端不可用）。
    pub openai_base_url: Option<String>,
    /// `inference.model_user_selectable`：注册表中标记可选的模型才出现在设置页。
    pub model_user_selectable: bool,
}

impl Default for InferenceConfig {
    fn default() -> Self {
        Self {
            local_port: LocalPort::Auto,
            registry_path: PathBuf::from("models/registry.toml"),
            openai_base_url: None,
            model_user_selectable: true,
        }
    }
}

/// `privacy.*`（CONFIG §10）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct PrivacyConfig {
    /// `privacy.cloud_enabled`：云端总开关（默认关）。
    pub cloud_enabled: bool,
    /// `privacy.send_original_image`：云端是否可发原图（默认关）。
    pub send_original_image: bool,
    /// `privacy.export_mask`：导出默认脱敏（默认关）。
    pub export_mask: bool,
}

/// `docs.*`（CONFIG §11）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DocsConfig {
    /// `docs.site`：mdBook 静态站开关。
    pub site: bool,
    /// `docs.link_check`：文档链接检查门禁。
    pub link_check: bool,
    /// `docs.publish`：发布目标。
    pub publish: PublishTarget,
}

impl Default for DocsConfig {
    fn default() -> Self {
        Self {
            site: true,
            link_check: true,
            publish: PublishTarget::GitHubPages,
        }
    }
}

/// `dev.*`（CONFIG §11）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DevConfig {
    /// `dev.record_sources`：研究记录写入 `docs/dev/research/`。
    pub record_sources: bool,
}

impl Default for DevConfig {
    fn default() -> Self {
        Self {
            record_sources: true,
        }
    }
}

/// 全部用户可调配置（`CONFIG.md` §1–§11；不含 `meta` 与不变量）。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// `general.*`。
    pub general: GeneralConfig,
    /// `ui.*`。
    pub ui: UiConfig,
    /// `import.*`。
    pub import: ImportConfig,
    /// `identity.*`。
    pub identity: IdentityConfig,
    /// `align.*`。
    pub align: AlignConfig,
    /// `ocr.*`。
    pub ocr: OcrConfig,
    /// `grade.*`。
    pub grade: GradeConfig,
    /// `fast.*`。
    pub fast: FastConfig,
    /// `rubric.*`。
    pub rubric: RubricConfig,
    /// `cas.*`。
    pub cas: CasConfig,
    /// `log.*`。
    pub log: LogConfig,
    /// `diagnostics.*`。
    pub diagnostics: DiagnosticsConfig,
    /// `storage.*`。
    pub storage: StorageConfig,
    /// `backup.*`。
    pub backup: BackupConfig,
    /// `inference.*`。
    pub inference: InferenceConfig,
    /// `privacy.*`。
    pub privacy: PrivacyConfig,
    /// `docs.*`。
    pub docs: DocsConfig,
    /// `dev.*`。
    pub dev: DevConfig,
}

// ---------------------------------------------------------------------------
// 校验
// ---------------------------------------------------------------------------

fn range_u32(issues: &mut Vec<ConfigIssue>, key: &str, value: u32, min: u32, max: u32) {
    if !(min..=max).contains(&value) {
        issues.push(ConfigIssue::new(
            key,
            format!("取值 {value} 超出允许范围 {min}–{max}"),
        ));
    }
}

fn range_f64(issues: &mut Vec<ConfigIssue>, key: &str, value: f64, min: f64, max: f64) {
    if !value.is_finite() || value < min || value > max {
        issues.push(ConfigIssue::new(
            key,
            format!("取值 {value} 超出允许范围 {min}–{max}"),
        ));
    }
}

fn non_empty(issues: &mut Vec<ConfigIssue>, key: &str, value: &str) {
    if value.trim().is_empty() {
        issues.push(ConfigIssue::new(key, "不能为空"));
    }
}

fn allowed_granularity(
    issues: &mut Vec<ConfigIssue>,
    key: &str,
    value: MilliScore,
    allowed: &[i64],
) {
    if !allowed.contains(&value.as_milli()) {
        let shown = allowed
            .iter()
            .map(|milli| {
                format!(
                    "{} 分",
                    MilliScore::from_milli(*milli)
                        .map_or_else(|_| "?".to_owned(), MilliScore::to_score_string)
                )
            })
            .collect::<Vec<_>>()
            .join(" / ");
        issues.push(ConfigIssue::new(
            key,
            format!("只允许 {shown}（当前 {} 千分之一分）", value.as_milli()),
        ));
    }
}

impl AppConfig {
    /// 校验全部可调项（取值域 + 交叉约束）。
    ///
    /// # Errors
    ///
    /// 返回 [`ConfigError::Invalid`]，包含全部问题（按键路径、中文说明）。
    pub fn validate(&self) -> std::result::Result<(), ConfigError> {
        let mut issues = Vec::new();

        range_u32(&mut issues, "ui.page_size", self.ui.page_size, 20, 500);
        range_u32(&mut issues, "ui.undo_depth", self.ui.undo_depth, 10, 200);

        range_u32(
            &mut issues,
            "import.batch_file_limit",
            self.import.batch_file_limit,
            1,
            10_000,
        );
        range_u32(&mut issues, "import.pdf_dpi", self.import.pdf_dpi, 150, 600);
        range_u32(
            &mut issues,
            "import.quality_blur_threshold",
            self.import.quality_blur_threshold,
            60,
            300,
        );
        range_f64(
            &mut issues,
            "import.quality_exposure_min",
            self.import.quality_exposure_min,
            0.05,
            0.30,
        );
        range_f64(
            &mut issues,
            "import.quality_exposure_max",
            self.import.quality_exposure_max,
            0.70,
            0.98,
        );
        if self.import.quality_exposure_min.is_finite()
            && self.import.quality_exposure_max.is_finite()
            && self.import.quality_exposure_min >= self.import.quality_exposure_max
        {
            issues.push(ConfigIssue::new(
                "import.quality_exposure_min",
                "欠曝下限必须小于过曝上限",
            ));
        }
        range_f64(
            &mut issues,
            "import.quality_skew_max",
            self.import.quality_skew_max,
            3.0,
            15.0,
        );
        range_u32(
            &mut issues,
            "import.quality_min_long_edge",
            self.import.quality_min_long_edge,
            1_200,
            3_000,
        );
        range_u32(
            &mut issues,
            "import.max_pdf_pages",
            self.import.max_pdf_pages,
            1,
            1_000,
        );
        range_u32(
            &mut issues,
            "import.max_file_mb",
            self.import.max_file_mb,
            1,
            512,
        );
        range_u32(
            &mut issues,
            "import.parallelism",
            self.import.parallelism,
            1,
            8,
        );

        range_f64(
            &mut issues,
            "identity.ocr_threshold",
            self.identity.ocr_threshold,
            0.70,
            0.99,
        );
        range_f64(
            &mut issues,
            "identity.ocr_margin",
            self.identity.ocr_margin,
            0.02,
            0.30,
        );
        if self.identity.barcode {
            issues.push(ConfigIssue::new(
                "identity.barcode",
                "v1 无条码解码器，不能启用条码身份",
            ));
        }

        range_f64(
            &mut issues,
            "align.region_confidence_floor",
            self.align.region_confidence_floor,
            0.50,
            0.99,
        );
        range_u32(
            &mut issues,
            "align.max_cross_pages",
            self.align.max_cross_pages,
            1,
            5,
        );

        non_empty(&mut issues, "ocr.model", &self.ocr.model);
        range_u32(
            &mut issues,
            "ocr.page_timeout_s",
            self.ocr.page_timeout_s,
            10,
            180,
        );
        range_u32(
            &mut issues,
            "ocr.repair_attempts",
            self.ocr.repair_attempts,
            0,
            2,
        );
        range_f64(
            &mut issues,
            "ocr.confidence_floor",
            self.ocr.confidence_floor,
            0.50,
            0.95,
        );

        allowed_granularity(
            &mut issues,
            "grade.score_granularity",
            self.grade.score_granularity,
            &SCORE_GRANULARITIES,
        );
        for (key, value) in [
            (
                "grade.standard.rule_threshold",
                self.grade.standard.rule_threshold,
            ),
            (
                "grade.standard.rule_then_model_threshold",
                self.grade.standard.rule_then_model_threshold,
            ),
            (
                "grade.standard.model_threshold",
                self.grade.standard.model_threshold,
            ),
            (
                "grade.standard.rubric_threshold",
                self.grade.standard.rubric_threshold,
            ),
            ("grade.fast.rule_threshold", self.grade.fast.rule_threshold),
        ] {
            range_f64(&mut issues, key, value, 0.50, 0.99);
        }
        range_u32(
            &mut issues,
            "grade.max_repair_attempts",
            self.grade.max_repair_attempts,
            0,
            2,
        );
        range_u32(
            &mut issues,
            "grade.judge_timeout_s",
            self.grade.judge_timeout_s,
            5,
            60,
        );
        non_empty(
            &mut issues,
            "grade.model_profile",
            &self.grade.model_profile,
        );

        range_f64(
            &mut issues,
            "fast.sampling_rate",
            self.fast.sampling_rate,
            0.05,
            1.00,
        );
        range_f64(
            &mut issues,
            "fast.low_confidence_weight",
            self.fast.low_confidence_weight,
            0.0,
            1.0,
        );

        if !self.rubric.partial_choice_enabled {
            // 规则数据允许关闭部分给分；无需阻断，但记录到审计由 rubric 负责。
        }
        allowed_granularity(
            &mut issues,
            "rubric.min_score_granularity",
            self.rubric.min_score_granularity,
            &[100, 500, 1_000],
        );
        non_empty(
            &mut issues,
            "rubric.template_snapshot",
            &self.rubric.template_snapshot,
        );
        range_u32(
            &mut issues,
            "rubric.repair_attempts",
            self.rubric.repair_attempts,
            0,
            2,
        );

        non_empty(&mut issues, "cas.backend", &self.cas.backend);
        range_u32(&mut issues, "cas.timeout_s", self.cas.timeout_s, 1, 10);
        if let Some(path) = &self.cas.python_path
            && path.as_os_str().is_empty()
        {
            issues.push(ConfigIssue::new("cas.python_path", "不能为空路径"));
        }

        range_u32(
            &mut issues,
            "log.retention_days",
            self.log.retention_days,
            7,
            365,
        );
        range_u32(
            &mut issues,
            "log.index_range_days",
            self.log.index_range_days,
            7,
            365,
        );
        for module in self.log.per_module_level.keys() {
            if module.trim().is_empty() {
                issues.push(ConfigIssue::new("log.per_module_level", "模块名不能为空"));
            }
        }

        range_u32(
            &mut issues,
            "storage.soft_quota_gb",
            self.storage.soft_quota_gb,
            5,
            500,
        );
        range_u32(
            &mut issues,
            "backup.keep_generations",
            self.backup.keep_generations,
            1,
            20,
        );

        if let LocalPort::Fixed(port) = self.inference.local_port
            && port == 0
        {
            issues.push(ConfigIssue::new(
                "inference.local_port",
                "固定端口必须在 1-65535 之间",
            ));
        }
        if self.inference.registry_path.as_os_str().is_empty() {
            issues.push(ConfigIssue::new(
                "inference.registry_path",
                "注册表路径不能为空",
            ));
        }
        if let Some(url) = &self.inference.openai_base_url {
            if url.trim().is_empty() {
                issues.push(ConfigIssue::new(
                    "inference.openai_base_url",
                    "为空时请直接删除该键（空串视为非法）",
                ));
            } else if !(url.starts_with("http://") || url.starts_with("https://")) {
                issues.push(ConfigIssue::new(
                    "inference.openai_base_url",
                    "必须是 http:// 或 https:// 开头的 URL",
                ));
            }
        }

        if issues.is_empty() {
            Ok(())
        } else {
            Err(ConfigError::invalid(issues))
        }
    }
}

// ---------------------------------------------------------------------------
// 配置文件文档（meta + 用户配置平铺）
// ---------------------------------------------------------------------------

/// `meta` 节：只由 `platform` 写（`schema_version`、`workspace_id`、`created_at`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MetaConfig {
    /// 配置文件 schema 版本。
    pub schema_version: u32,
    /// 工作区 id（ULID）。
    pub workspace_id: Ulid,
    /// 工作区创建时间。
    pub created_at: Timestamp,
}

impl Default for MetaConfig {
    fn default() -> Self {
        Self {
            schema_version: CONFIG_SCHEMA_VERSION,
            workspace_id: Ulid::NIL,
            created_at: Timestamp::UNIX_EPOCH,
        }
    }
}

/// 配置文件全文：`[meta]` + 各用户节（`general`/`ui`/… 平铺在根）。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ConfigDocument {
    /// 只由本模块写的元数据。
    pub meta: MetaConfig,
    /// 用户可调配置（平铺）。
    #[serde(flatten)]
    pub config: AppConfig,
}

/// 收集未知配置键（相对 schema 递归；空表视为自由映射，如 `log.per_module_level`）。
fn collect_unknown_keys(
    parsed: &toml::Value,
    schema: &toml::Value,
    prefix: &str,
    issues: &mut Vec<ConfigIssue>,
) {
    let (Some(parsed_table), Some(schema_table)) = (parsed.as_table(), schema.as_table()) else {
        return;
    };
    if schema_table.is_empty() {
        return;
    }
    for (key, value) in parsed_table {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match schema_table.get(key) {
            Some(schema_value) => collect_unknown_keys(value, schema_value, &path, issues),
            None => issues.push(ConfigIssue::new(
                path,
                "未知配置键（已拒绝；请核对 CONFIG.md 中的键名）",
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// 配置存储（加载 / 原子保存 / 损坏隔离 / 备份恢复）
// ---------------------------------------------------------------------------

/// 配置文件存储与生命周期管理。
#[derive(Clone)]
pub struct ConfigStore {
    dirs: DataDirs,
    clock: SharedClock,
    ids: std::sync::Arc<UlidFactory>,
}

impl fmt::Debug for ConfigStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConfigStore")
            .field("path", &self.dirs.config_path())
            .finish_non_exhaustive()
    }
}

impl ConfigStore {
    /// 构造配置存储。
    #[must_use]
    pub fn new(dirs: DataDirs, clock: SharedClock, ids: std::sync::Arc<UlidFactory>) -> Self {
        Self { dirs, clock, ids }
    }

    /// 配置文件路径。
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.dirs.config_path()
    }

    /// 备份文件路径。
    #[must_use]
    pub fn backup_path(&self) -> PathBuf {
        self.dirs.config_backup_path()
    }

    /// 加载配置：不存在则写入默认；损坏则隔离现场并报错（不静默回默认）。
    ///
    /// # Errors
    ///
    /// 非法值、未知键、损坏文件、IO 失败均返回 [`ConfigError`]。
    pub fn load(&self) -> Result<ConfigDocument> {
        let path = self.path();
        if !path.exists() {
            let document = self.new_document()?;
            self.save(&document)?;
            return Ok(document);
        }
        let text = std::fs::read_to_string(&path).map_err(|source| ConfigError::Io {
            path: path.clone(),
            source,
        })?;
        let parsed: toml::Value = match toml::from_str(&text) {
            Ok(value) => value,
            Err(error) => {
                let broken = self.quarantine(&path, &format!("TOML 语法错误：{error}"))?;
                return Err(PlatformError::Config(ConfigError::Corrupted {
                    path,
                    broken_path: broken,
                    message: format!("TOML 语法错误：{error}"),
                }));
            }
        };
        let schema = toml::Value::try_from(ConfigDocument::default())
            .map_err(|error| ConfigError::Serialize(error.to_string()))?;
        let mut issues = Vec::new();
        collect_unknown_keys(&parsed, &schema, "", &mut issues);
        if !issues.is_empty() {
            return Err(ConfigError::invalid(issues).into());
        }

        let mut document: ConfigDocument =
            toml::from_str(&text).map_err(|error| ConfigError::Parse {
                path: path.clone(),
                message: format!("字段类型不匹配：{error}"),
            })?;
        if document.meta.schema_version != CONFIG_SCHEMA_VERSION {
            return Err(ConfigError::invalid(vec![ConfigIssue::new(
                "meta.schema_version",
                format!(
                    "配置文件版本 {} 与本程序版本 {CONFIG_SCHEMA_VERSION} 不一致",
                    document.meta.schema_version
                ),
            )])
            .into());
        }
        document.config.validate()?;

        // 缺失 meta（旧文件/手工文件）：补齐并落盘（meta 只由本模块写）。
        if document.meta.workspace_id.is_nil() || document.meta.created_at == Timestamp::UNIX_EPOCH
        {
            document.meta.workspace_id = self.ids.next_ulid()?;
            document.meta.created_at = self.clock.now();
            self.save(&document)?;
        }
        Ok(document)
    }

    /// 校验并原子保存（保留一代 `.bak`）。
    ///
    /// # Errors
    ///
    /// 非法值或写入失败时返回 [`ConfigError`]。
    pub fn save(&self, document: &ConfigDocument) -> Result<()> {
        document.config.validate()?;
        if document.meta.schema_version != CONFIG_SCHEMA_VERSION {
            return Err(ConfigError::invalid(vec![ConfigIssue::new(
                "meta.schema_version",
                format!("必须为 {CONFIG_SCHEMA_VERSION}"),
            )])
            .into());
        }
        let text = toml::to_string_pretty(document)
            .map_err(|error| ConfigError::Serialize(error.to_string()))?;
        self.atomic_write(&self.path(), &text, true)
    }

    /// 恢复上一版 `.bak`（恢复动作由调用方写审计）。
    ///
    /// # Errors
    ///
    /// 备份不存在或备份本身非法/损坏时返回错误。
    pub fn restore_backup(&self) -> Result<ConfigDocument> {
        let backup = self.backup_path();
        if !backup.exists() {
            return Err(ConfigError::BackupMissing { path: backup }.into());
        }
        let text = std::fs::read_to_string(&backup).map_err(|source| ConfigError::Io {
            path: backup.clone(),
            source,
        })?;
        let document: ConfigDocument =
            toml::from_str(&text).map_err(|error| ConfigError::Parse {
                path: backup.clone(),
                message: format!("备份文件无法解析：{error}"),
            })?;
        document.config.validate()?;
        let text = toml::to_string_pretty(&document)
            .map_err(|error| ConfigError::Serialize(error.to_string()))?;
        // 恢复时不覆盖 .bak（保留唯一的上一版来源）。
        self.atomic_write(&self.path(), &text, false)?;
        Ok(document)
    }

    /// 重置为默认配置（生成新的 workspace id）。
    ///
    /// # Errors
    ///
    /// 写入失败时返回错误。
    pub fn reset_to_default(&self) -> Result<ConfigDocument> {
        let document = self.new_document()?;
        self.save(&document)?;
        Ok(document)
    }

    /// 构造全新默认文档（meta 取当前时钟与新 ULID）。
    ///
    /// # Errors
    ///
    /// ULID 生成失败时返回错误。
    pub fn new_document(&self) -> Result<ConfigDocument> {
        Ok(ConfigDocument {
            meta: MetaConfig {
                schema_version: CONFIG_SCHEMA_VERSION,
                workspace_id: self.ids.next_ulid()?,
                created_at: self.clock.now(),
            },
            config: AppConfig::default(),
        })
    }

    /// 把损坏文件改名保留现场，返回新路径。
    fn quarantine(&self, path: &Path, _reason: &str) -> Result<PathBuf> {
        let stamp = self.clock.now().format("%Y%m%dT%H%M%S%.3f");
        let mut candidate = path.with_file_name(format!("{CONFIG_FILE}.broken-{stamp}"));
        let mut suffix = 1;
        while candidate.exists() {
            candidate = path.with_file_name(format!("{CONFIG_FILE}.broken-{stamp}-{suffix}"));
            suffix += 1;
        }
        std::fs::rename(path, &candidate).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(candidate)
    }

    /// 原子写：临时文件 + fsync + （可选）备份 + rename + 目录 fsync。
    fn atomic_write(&self, path: &Path, text: &str, make_backup: bool) -> Result<()> {
        let parent = path.parent().map_or_else(PathBuf::new, Path::to_path_buf);
        std::fs::create_dir_all(&parent).map_err(|source| ConfigError::Io {
            path: parent.clone(),
            source,
        })?;
        let temp = path.with_extension("toml.tmp");
        {
            let mut file = std::fs::File::create(&temp).map_err(|source| ConfigError::Io {
                path: temp.clone(),
                source,
            })?;
            file.write_all(text.as_bytes())
                .map_err(|source| ConfigError::Io {
                    path: temp.clone(),
                    source,
                })?;
            file.sync_all().map_err(|source| ConfigError::Io {
                path: temp.clone(),
                source,
            })?;
        }
        if make_backup && path.exists() {
            let backup = self.backup_path();
            std::fs::copy(path, &backup).map_err(|source| ConfigError::Io {
                path: backup,
                source,
            })?;
        }
        std::fs::rename(&temp, path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        // 目录 fsync：尽力而为（某些平台不支持）。
        if let Ok(dir) = std::fs::File::open(&parent) {
            let _ = dir.sync_all();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::sync::Arc;

    use super::*;
    use crate::clock::FixedClock;

    fn fixed_clock() -> Arc<FixedClock> {
        Arc::new(FixedClock::from_rfc3339("2026-11-02T12:00:00Z").unwrap())
    }

    fn store(root: &Path, clock: Arc<FixedClock>) -> ConfigStore {
        let dirs = DataDirs::with_root(root).unwrap();
        let ids = Arc::new(UlidFactory::new(clock.clone()));
        ConfigStore::new(dirs, clock, ids)
    }

    #[test]
    fn defaults_match_config_md() {
        // 逐项对照 CONFIG.md（v0.2.3）默认值：0 偏差。
        let config = AppConfig::default();

        assert_eq!(config.general.mode, GradeMode::Standard);
        assert_eq!(config.general.language, Language::ZhCn);
        assert_eq!(config.general.theme, Theme::System);

        assert_eq!(config.ui.page_size, 100);
        assert_eq!(config.ui.undo_depth, 50);
        assert_eq!(config.ui.start_page, StartPage::Home);
        assert!(config.ui.confirm_before_leave_unsaved);
        assert_eq!(config.ui.window_geometry, WindowGeometry::Auto);

        assert!(config.import.remind_naming);
        assert_eq!(config.import.batch_file_limit, 2_000);
        assert_eq!(config.import.pdf_dpi, 300);
        assert_eq!(config.import.quality_blur_threshold, 120);
        assert!((config.import.quality_exposure_min - 0.12).abs() < f64::EPSILON);
        assert!((config.import.quality_exposure_max - 0.90).abs() < f64::EPSILON);
        assert!((config.import.quality_skew_max - 8.0).abs() < f64::EPSILON);
        assert_eq!(config.import.quality_min_long_edge, 1_600);
        assert_eq!(config.import.reject_behavior, RejectBehavior::PageReject);
        assert!(config.import.duplicate_notice);
        assert_eq!(config.import.max_pdf_pages, 200);
        assert_eq!(config.import.max_file_mb, 64);
        assert_eq!(config.import.parallelism, 4);

        assert_eq!(config.identity.source, IdentitySource::FileName);
        assert!(!config.identity.ocr_fallback);
        assert!(!config.identity.barcode);
        assert_eq!(
            config.identity.unmatched_behavior,
            UnmatchedBehavior::Undecided
        );
        assert!((config.identity.ocr_threshold - 0.90).abs() < f64::EPSILON);
        assert!((config.identity.ocr_margin - 0.10).abs() < f64::EPSILON);

        assert!(config.align.template_region);
        assert!((config.align.region_confidence_floor - 0.80).abs() < f64::EPSILON);
        assert_eq!(config.align.max_cross_pages, 3);

        assert_eq!(config.ocr.model, "PaddleOCR-VL-1.6");
        assert!(config.ocr.emit_latex);
        assert_eq!(config.ocr.segment_recognition, SegmentRecognition::OnDemand);
        assert_eq!(config.ocr.page_timeout_s, 60);
        assert_eq!(config.ocr.repair_attempts, 1);
        assert!((config.ocr.confidence_floor - 0.75).abs() < f64::EPSILON);
        assert!(config.ocr.cache);

        assert_eq!(config.grade.score_granularity.as_milli(), 100);
        assert!((config.grade.standard.rule_threshold - 0.85).abs() < f64::EPSILON);
        assert!((config.grade.standard.rule_then_model_threshold - 0.90).abs() < f64::EPSILON);
        assert!((config.grade.standard.model_threshold - 0.85).abs() < f64::EPSILON);
        assert!((config.grade.standard.rubric_threshold - 0.80).abs() < f64::EPSILON);
        assert!((config.grade.fast.rule_threshold - 0.80).abs() < f64::EPSILON);
        assert!(!config.grade.allow_uncalibrated_auto);
        assert_eq!(config.grade.conflict_policy, ConflictPolicy::EscalateManual);
        assert_eq!(config.grade.max_repair_attempts, 1);
        assert_eq!(config.grade.judge_timeout_s, 20);
        assert_eq!(config.grade.model_profile, "4B 指令模型 + 受限输出");

        assert!(config.fast.enabled);
        assert!((config.fast.sampling_rate - 0.20).abs() < f64::EPSILON);
        assert!((config.fast.low_confidence_weight - 0.70).abs() < f64::EPSILON);
        assert_eq!(config.fast.sampling_seed, SamplingSeed::Random);
        assert!(config.fast.sampled_readonly);
        assert_eq!(config.fast.fail_action, FailAction::Ask);
        assert_eq!(config.fast.fail_default_scope, FailScope::SingleQuestion);
        assert!(config.fast.export_label);

        assert_eq!(config.rubric.template_pack, TemplatePack::NationalPack);
        assert_eq!(
            config.rubric.national_sets.active,
            vec!["新课标Ⅰ卷".to_owned(), "新课标Ⅱ卷".to_owned()]
        );
        assert_eq!(
            config.rubric.national_sets.retired,
            vec!["全国甲卷".to_owned(), "全国乙卷".to_owned()]
        );
        assert!(config.rubric.partial_choice_enabled);
        assert_eq!(config.rubric.min_score_granularity.as_milli(), 100);
        assert!(config.rubric.auto_validate);
        assert!(config.rubric.allow_custom_template);
        assert_eq!(config.rubric.template_snapshot, "latest");
        assert_eq!(config.rubric.repair_attempts, 1);

        assert!(config.cas.enabled);
        assert_eq!(config.cas.backend, "sympy-sidecar");
        assert_eq!(config.cas.timeout_s, 3);
        assert!(config.cas.python_path.is_none());
        assert!(config.cas.cache);

        assert_eq!(config.log.level, LogLevel::Info);
        assert!(config.log.per_module_level.is_empty());
        assert_eq!(config.log.retention_days, 30);
        assert_eq!(config.log.rotation, LogRotation::Daily);
        assert!(config.log.redaction);
        assert!(config.log.search_index);
        assert!(config.log.ui_viewer);
        assert_eq!(config.log.index_range_days, 30);
        assert!(config.log.index_rebuild.on_startup_check);
        assert!(config.log.index_rebuild.manual);
        assert!(config.log.index_rebuild.on_schema_change);

        assert!(config.diagnostics.include_schema);
        assert_eq!(config.storage.soft_quota_gb, 20);
        assert!(config.storage.verify_hash_on_read);
        assert_eq!(config.backup.keep_generations, 5);
        assert_eq!(config.inference.local_port, LocalPort::Auto);
        assert_eq!(
            config.inference.registry_path,
            PathBuf::from("models/registry.toml")
        );
        assert!(config.inference.openai_base_url.is_none());
        assert!(config.inference.model_user_selectable);

        assert!(!config.privacy.cloud_enabled);
        assert!(!config.privacy.send_original_image);
        assert!(!config.privacy.export_mask);

        assert!(config.docs.site);
        assert!(config.docs.link_check);
        assert_eq!(config.docs.publish, PublishTarget::GitHubPages);
        assert!(config.dev.record_sources);

        config.validate().expect("默认配置必须自洽");
    }

    #[test]
    fn defaults_snapshot() {
        insta::assert_json_snapshot!("config_defaults", AppConfig::default());
    }

    #[test]
    fn defaults_roundtrip_via_toml() {
        let text = toml::to_string_pretty(&ConfigDocument::default()).unwrap();
        let back: ConfigDocument = toml::from_str(&text).unwrap();
        assert_eq!(back.config, AppConfig::default());
    }

    #[test]
    fn save_and_load_roundtrip() {
        let temp = tempfile::tempdir().unwrap();
        let clock = fixed_clock();
        let store = store(temp.path(), clock);
        let document = store.load().unwrap();
        assert!(store.path().exists());
        assert!(document.meta.workspace_id != Ulid::NIL);
        let loaded = store.load().unwrap();
        assert_eq!(loaded, document);
    }

    #[test]
    fn missing_keys_filled_from_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let clock = fixed_clock();
        let store = store(temp.path(), clock);
        // 只写部分键（含 meta，避免额外写入路径干扰断言）。
        std::fs::write(
            store.path(),
            "[meta]\nschema_version = 1\n\n[log]\nlevel = \"debug\"\nretention_days = 90\n",
        )
        .unwrap();
        let document = store.load().unwrap();
        assert_eq!(document.config.log.level, LogLevel::Debug);
        assert_eq!(document.config.log.retention_days, 90);
        // 未写的键取默认值。
        assert_eq!(document.config.log.rotation, LogRotation::Daily);
        assert_eq!(document.config.general.mode, GradeMode::Standard);
        assert_eq!(document.config.import.pdf_dpi, 300);
    }

    #[test]
    fn invalid_value_rejected_with_chinese_message() {
        let temp = tempfile::tempdir().unwrap();
        let clock = fixed_clock();
        let store = store(temp.path(), clock);
        std::fs::write(
            store.path(),
            "[meta]\nschema_version = 1\n\n[log]\nretention_days = 3\n",
        )
        .unwrap();
        let error = store.load().unwrap_err();
        let message = error.to_string();
        assert!(message.contains("log.retention_days"), "{message}");
        assert!(message.contains("超出允许范围"), "{message}");
        assert_eq!(error.class(), ErrorClass::TeacherFixable);
    }

    #[test]
    fn unknown_key_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let clock = fixed_clock();
        let store = store(temp.path(), clock);
        std::fs::write(
            store.path(),
            "[meta]\nschema_version = 1\n\n[log]\nlevle = \"debug\"\n",
        )
        .unwrap();
        let error = store.load().unwrap_err().to_string();
        assert!(error.contains("log.levle"), "{error}");
        assert!(error.contains("未知配置键"), "{error}");
    }

    #[test]
    fn corrupted_file_is_quarantined_not_silently_reset() {
        let temp = tempfile::tempdir().unwrap();
        let clock = fixed_clock();
        let store = store(temp.path(), clock);
        std::fs::write(store.path(), "这不是 TOML [").unwrap();
        let error = store.load().unwrap_err().to_string();
        assert!(error.contains("配置文件损坏"), "{error}");
        assert!(error.contains("恢复上一版"), "{error}");
        assert!(!store.path().exists(), "损坏文件应被改名保留现场");
        let broken: Vec<_> = std::fs::read_dir(temp.path())
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("config.toml.broken-")
            })
            .collect();
        assert_eq!(broken.len(), 1, "应保留一个 .broken-<时间戳> 现场");
    }

    #[test]
    fn backup_kept_and_restorable() {
        let temp = tempfile::tempdir().unwrap();
        let clock = fixed_clock();
        let store = store(temp.path(), clock);
        let mut first = store.load().unwrap();
        first.config.log.level = LogLevel::Debug;
        store.save(&first).unwrap();

        let mut second = first.clone();
        second.config.log.level = LogLevel::Warn;
        store.save(&second).unwrap();
        assert!(store.backup_path().exists(), "应保留一代 .bak");

        let restored = store.restore_backup().unwrap();
        assert_eq!(restored.config.log.level, LogLevel::Debug);
        assert_eq!(store.load().unwrap().config.log.level, LogLevel::Debug);
    }

    #[test]
    fn restore_without_backup_fails_explicitly() {
        let temp = tempfile::tempdir().unwrap();
        let clock = fixed_clock();
        let store = store(temp.path(), clock);
        store.load().unwrap();
        let error = store.restore_backup().unwrap_err().to_string();
        assert!(error.contains("没有可恢复的上一版配置"), "{error}");
    }

    #[test]
    fn reset_to_default_regenerates_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let clock = fixed_clock();
        let store = store(temp.path(), clock.clone());
        let mut document = store.load().unwrap();
        let workspace = document.meta.workspace_id;
        document.config.log.level = LogLevel::Error;
        store.save(&document).unwrap();
        clock.advance(chrono::Duration::seconds(1)).unwrap();
        let reset = store.reset_to_default().unwrap();
        assert_eq!(reset.config, AppConfig::default());
        assert_ne!(reset.meta.workspace_id, workspace);
    }

    #[test]
    fn save_rejects_invalid_before_writing() {
        let temp = tempfile::tempdir().unwrap();
        let clock = fixed_clock();
        let store = store(temp.path(), clock);
        let mut document = store.load().unwrap();
        document.config.ui.page_size = 10_000;
        let error = store.save(&document).unwrap_err().to_string();
        assert!(error.contains("ui.page_size"), "{error}");
        // 磁盘上的仍是合法旧值。
        assert_eq!(store.load().unwrap().config.ui.page_size, 100);
    }

    #[test]
    fn atomic_write_leaves_no_temp_file() {
        let temp = tempfile::tempdir().unwrap();
        let clock = fixed_clock();
        let store = store(temp.path(), clock);
        store.load().unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(temp.path())
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "不应残留临时文件");
    }

    #[test]
    fn per_module_level_roundtrip() {
        let temp = tempfile::tempdir().unwrap();
        let clock = fixed_clock();
        let store = store(temp.path(), clock);
        let mut document = store.load().unwrap();
        document
            .config
            .log
            .per_module_level
            .insert("eg_recognize".to_owned(), LogLevel::Debug);
        store.save(&document).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(
            loaded.config.log.per_module_level.get("eg_recognize"),
            Some(&LogLevel::Debug)
        );
    }

    #[test]
    fn exposure_range_cross_check() {
        let mut config = AppConfig::default();
        config.import.quality_exposure_min = 0.95;
        config.import.quality_exposure_max = 0.90;
        let error = config.validate().unwrap_err().to_string();
        assert!(error.contains("欠曝下限必须小于过曝上限"), "{error}");
    }

    #[test]
    fn invalid_granularity_and_base_url_rejected() {
        let mut config = AppConfig::default();
        config.grade.score_granularity = MilliScore::from_milli(37).unwrap();
        config.inference.openai_base_url = Some("ftp://example.com".to_owned());
        let error = config.validate().unwrap_err().to_string();
        assert!(error.contains("grade.score_granularity"), "{error}");
        assert!(error.contains("inference.openai_base_url"), "{error}");
    }

    #[test]
    fn barcode_rejected_in_v1() {
        let mut config = AppConfig::default();
        config.identity.barcode = true;
        assert!(config.validate().is_err());
    }
}
