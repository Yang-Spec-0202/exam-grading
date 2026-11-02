//! 统计口径配置（`SPEC-report.md` §3.4/§3.5：口径版本、抽检率、阈值全部写入元信息）。
//!
//! 本模块只定义**报表口径**，不读取 `CONFIG.md` 的文件：由 `app` 把
//! `fast.sampling_rate`、`fast.low_confidence_weight`、`fast.export_label`、
//! `privacy.export_mask` 映射为 [`ReportSettings`]（TODO：`eg-platform` 接入时在 app 适配）。

use crate::error::{ReportError, Result};
use crate::rounding::{PPM_SCALE, round_ppm_to_tenth_percent};

/// 抽样方法说明（写入元信息、HTML 页脚、CSV 元信息分表）。
pub const SAMPLING_METHOD: &str =
    "加权随机抽样：低置信度题目优先，权重由 fast.low_confidence_weight 指定";

/// 命名规则版本（规范 §3.2；命名规则变更必须 +1 并写入导出运行与审计）。
pub const NAMING_RULE_VERSION: u32 = 1;

/// 报表口径配置。
///
/// 默认值对齐 `CONFIG.md`：`fast.sampling_rate=0.20`、`fast.low_confidence_weight=0.70`、
/// `fast.export_label=开`、`privacy.export_mask=关`；及格线 60%、良好线 70%、优秀线 85%。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportSettings {
    /// 快速模式抽检率（ppm，默认 200_000 = 20%）。
    pub sampling_rate_ppm: u32,
    /// 低置信度题目的加权抽样权重（ppm，默认 700_000 = 0.70）。
    pub low_confidence_weight_ppm: u32,
    /// `fast.export_label`：快速导出是否在文件名与表头标「快速」。
    pub fast_export_label: bool,
    /// `privacy.export_mask`：默认是否脱敏导出。
    pub default_masked: bool,
    /// 及格线（整卷得分率 ppm，默认 600_000）。
    pub pass_rate_ppm: u32,
    /// 良好线（ppm，默认 700_000）；仅用于元信息与班级汇总展示。
    pub good_rate_ppm: u32,
    /// 优秀线（ppm，默认 850_000）。
    pub excellent_rate_ppm: u32,
    /// 统计口径版本：公式/样本定义/分箱变更时 +1。
    pub policy_version: u32,
}

impl Default for ReportSettings {
    fn default() -> Self {
        Self {
            sampling_rate_ppm: 200_000,
            low_confidence_weight_ppm: 700_000,
            fast_export_label: true,
            default_masked: false,
            pass_rate_ppm: 600_000,
            good_rate_ppm: 700_000,
            excellent_rate_ppm: 850_000,
            policy_version: 1,
        }
    }
}

impl ReportSettings {
    /// 校验口径配置（规范 §5：抽检率 0.05–1.00、权重 0–1、阈值单调且合法）。
    ///
    /// # Errors
    ///
    /// 任一项越界时返回 [`ReportError::Config`]，并指出配置项。
    pub fn validate(&self) -> Result<()> {
        if !(50_000..=PPM_SCALE).contains(&self.sampling_rate_ppm) {
            return Err(ReportError::Config(format!(
                "fast.sampling_rate 必须在 0.05–1.00 之间，当前为 {}",
                self.sampling_rate_ppm
            )));
        }
        if self.low_confidence_weight_ppm > PPM_SCALE {
            return Err(ReportError::Config(format!(
                "fast.low_confidence_weight 必须在 0–1 之间，当前为 {}",
                self.low_confidence_weight_ppm
            )));
        }
        for (name, value) in [
            ("pass_rate", self.pass_rate_ppm),
            ("good_rate", self.good_rate_ppm),
            ("excellent_rate", self.excellent_rate_ppm),
        ] {
            if value > PPM_SCALE {
                return Err(ReportError::Config(format!(
                    "{name} 必须在 0–1 之间，当前为 {value}"
                )));
            }
        }
        if !(self.pass_rate_ppm <= self.good_rate_ppm
            && self.good_rate_ppm <= self.excellent_rate_ppm)
        {
            return Err(ReportError::Config(
                "及格线/良好线/优秀线必须单调不减".to_owned(),
            ));
        }
        if self.policy_version == 0 {
            return Err(ReportError::Config("口径版本必须大于 0".to_owned()));
        }
        Ok(())
    }

    /// 展示用及格线（0.1% 粒度）。
    pub(crate) fn display_pass(&self) -> String {
        crate::rounding::display_percent(round_ppm_to_tenth_percent(self.pass_rate_ppm))
    }

    /// 展示用优秀线（0.1% 粒度）。
    pub(crate) fn display_excellent(&self) -> String {
        crate::rounding::display_percent(round_ppm_to_tenth_percent(self.excellent_rate_ppm))
    }

    /// 展示用抽检率（0.1% 粒度）。
    pub(crate) fn display_sampling_rate(&self) -> String {
        crate::rounding::display_percent(round_ppm_to_tenth_percent(self.sampling_rate_ppm))
    }

    /// 展示用低置信度权重（保留 2 位小数，按百分比形态给出）。
    pub(crate) fn display_low_confidence_weight(&self) -> String {
        let hundredths = (u64::from(self.low_confidence_weight_ppm) * 100) / u64::from(PPM_SCALE);
        format!("0.{:02}", hundredths.min(100))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_config() {
        let settings = ReportSettings::default();
        assert!(settings.validate().is_ok());
        assert_eq!(settings.sampling_rate_ppm, 200_000);
        assert_eq!(settings.low_confidence_weight_ppm, 700_000);
        assert!(settings.fast_export_label);
        assert!(!settings.default_masked);
        assert_eq!(settings.policy_version, 1);
    }

    #[test]
    fn rejects_out_of_range_values() {
        assert!(
            ReportSettings {
                sampling_rate_ppm: 49_999,
                ..ReportSettings::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            ReportSettings {
                low_confidence_weight_ppm: 1_000_001,
                ..ReportSettings::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            ReportSettings {
                pass_rate_ppm: 900_000,
                ..ReportSettings::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            ReportSettings {
                policy_version: 0,
                ..ReportSettings::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn display_helpers() {
        let settings = ReportSettings::default();
        assert_eq!(settings.display_pass(), "60.0%");
        assert_eq!(settings.display_excellent(), "85.0%");
        assert_eq!(settings.display_sampling_rate(), "20.0%");
        assert_eq!(settings.display_low_confidence_weight(), "0.70");
    }
}
