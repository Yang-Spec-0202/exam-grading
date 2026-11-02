//! 许可门禁（`SPEC-inference.md` §2.4，**fail-closed**）。
//!
//! 规则：
//! - 白名单：`MIT`、`Apache-2.0`、`BSD-3-Clause`、`ISC`、`Unicode-3.0`（规范基线）；
//! - 排除清单：`OXR`（营收条款）、`Youtu-Parsing`（地域限制）、`MinerU`（规模条款）、
//!   `Datalab`/OpenRAIL-M、以及一切 `CC-BY-NC` 系；
//! - 命中排除清单时**必须**同时提供人工评审引用（`license_review`）才允许加载；
//! - 未在白名单内的其他许可一律拒绝（不猜测、不放行）。
//!
//! 依据：`SPEC-inference.md` §2.4/§9.9、`ocr-open-models-report.md` §7（许可陷阱）。
//!
//! 说明：这里的白名单是**模型权重许可**，与 `deny.toml` 的 **crate 依赖许可**不是同一个
//! 清单（后者还包含项目自身 GPL-3.0-only 与 Slint 依赖链的宽松许可）。

use std::collections::BTreeSet;

/// 许可判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LicenseVerdict {
    /// 白名单内，允许加载。
    Allowed,
    /// 命中排除清单且提供了人工评审引用，允许加载（须审计）。
    AllowedWithReview,
    /// 命中排除清单但缺人工评审引用，拒绝。
    DeniedNeedsReview,
    /// 不在白名单内（含未知许可），拒绝。
    DeniedUnknown,
}

impl LicenseVerdict {
    /// 是否允许加载。
    #[must_use]
    pub const fn allowed(self) -> bool {
        matches!(self, Self::Allowed | Self::AllowedWithReview)
    }

    /// 中文说明（审计与界面用）。
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Allowed => "白名单许可",
            Self::AllowedWithReview => "排除清单许可（已附人工评审引用）",
            Self::DeniedNeedsReview => "排除清单许可（缺人工评审引用）",
            Self::DeniedUnknown => "白名单外许可",
        }
    }
}

/// 许可策略：白名单 + 排除清单。默认值即规范基线。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LicensePolicy {
    allow: BTreeSet<String>,
    excluded: BTreeSet<String>,
}

impl Default for LicensePolicy {
    fn default() -> Self {
        let allow = ["MIT", "Apache-2.0", "BSD-3-Clause", "ISC", "Unicode-3.0"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let excluded = [
            // 报告 §7 的许可陷阱；用可识别的前缀/全名登记。
            "OXR-Proprietary",
            "Youtu-Parsing-License",
            "MinerU-Open-Source-License",
            "OpenRAIL-M",
            "CC-BY-NC-4.0",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        Self { allow, excluded }
    }
}

impl LicensePolicy {
    /// 以给定白名单与排除清单构造（测试与将来扩展用）。
    #[must_use]
    pub fn new<I, E>(allow: I, excluded: E) -> Self
    where
        I: IntoIterator<Item = String>,
        E: IntoIterator<Item = String>,
    {
        Self {
            allow: allow.into_iter().collect(),
            excluded: excluded.into_iter().collect(),
        }
    }

    /// 白名单（只读）。
    #[must_use]
    pub fn allow_list(&self) -> Vec<String> {
        self.allow.iter().cloned().collect()
    }

    /// 排除清单（只读）。
    #[must_use]
    pub fn excluded_list(&self) -> Vec<String> {
        self.excluded.iter().cloned().collect()
    }

    /// 判断许可是否可加载。
    ///
    /// `review_ref` 是人工评审引用（非空即为已评审）；只有命中排除清单时才需要它。
    #[must_use]
    pub fn check(&self, license: &str, review_ref: Option<&str>) -> LicenseVerdict {
        let license = license.trim();
        let has_review = review_ref.is_some_and(|text| !text.trim().is_empty());
        if self.is_excluded(license) {
            return if has_review {
                LicenseVerdict::AllowedWithReview
            } else {
                LicenseVerdict::DeniedNeedsReview
            };
        }
        if self.allow.contains(license) {
            LicenseVerdict::Allowed
        } else {
            LicenseVerdict::DeniedUnknown
        }
    }

    /// 是否命中排除清单（大小写敏感的全名匹配 + `CC-BY-NC` 前缀族）。
    #[must_use]
    pub fn is_excluded(&self, license: &str) -> bool {
        let license = license.trim();
        self.excluded.contains(license)
            || license.starts_with("CC-BY-NC")
            || license.starts_with("CC-BY-NC-")
    }
}

/// 注册表许可扫描结果中的一条违例。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LicenseViolation {
    /// 模型 id。
    pub model_id: String,
    /// 许可标识。
    pub license: String,
    /// 判定。
    pub verdict: LicenseVerdict,
}

impl LicenseViolation {
    /// 中文说明。
    #[must_use]
    pub fn message(&self) -> String {
        format!(
            "模型 {} 的许可 {} 未通过门禁：{}",
            self.model_id,
            self.license,
            self.verdict.label()
        )
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn whitelist_licenses_are_allowed() {
        let policy = LicensePolicy::default();
        for license in ["MIT", "Apache-2.0", "BSD-3-Clause", "ISC", "Unicode-3.0"] {
            assert_eq!(
                policy.check(license, None),
                LicenseVerdict::Allowed,
                "{license} 应在白名单内"
            );
        }
    }

    #[test]
    fn unknown_license_fails_closed() {
        let policy = LicensePolicy::default();
        for license in ["Proprietary", "AGPL-3.0-only", "CC-BY-4.0", "GPL-2.0-only"] {
            assert_eq!(
                policy.check(license, None),
                LicenseVerdict::DeniedUnknown,
                "{license} 不在白名单时必须拒绝"
            );
            // 白名单外即使有评审引用也不放行：评审只对排除清单生效。
            assert!(!policy.check(license, Some("REV-1")).allowed());
        }
    }

    #[test]
    fn excluded_licenses_require_review_reference() {
        let policy = LicensePolicy::default();
        let excluded = [
            "OXR-Proprietary",
            "Youtu-Parsing-License",
            "MinerU-Open-Source-License",
            "OpenRAIL-M",
            "CC-BY-NC-4.0",
            "CC-BY-NC-SA-3.0",
        ];
        assert!(excluded.len() >= 3, "规范要求至少 3 个测试样例");
        for license in excluded {
            assert!(policy.is_excluded(license), "{license} 应命中排除清单");
            assert_eq!(
                policy.check(license, None),
                LicenseVerdict::DeniedNeedsReview,
                "{license} 无评审引用时必须拒绝"
            );
            assert_eq!(
                policy.check(license, Some("docs/reviews/rev-001.md")),
                LicenseVerdict::AllowedWithReview,
                "{license} 有评审引用时可加载"
            );
        }
    }

    #[test]
    fn whitespace_is_trimmed_and_empty_review_does_not_count() {
        let policy = LicensePolicy::default();
        assert_eq!(policy.check("  MIT  ", None), LicenseVerdict::Allowed);
        assert_eq!(
            policy.check("CC-BY-NC-4.0", Some("   ")),
            LicenseVerdict::DeniedNeedsReview
        );
    }
}
