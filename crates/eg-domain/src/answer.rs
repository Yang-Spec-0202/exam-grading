//! 参考答案与容差（`SPEC-domain.md` §4）。
//!
//! - `AnswerKey` 为四选一枚举：具体答案 / 仅评分标准 / 明确省略 / 缺失。
//! - 「仅评分标准」「明确省略」「缺失」下**不允许**存在具体答案键（结构上由枚举保证）。
//! - `AnswerValue` 取值：文本、数值（值 + 可选单位 + 容差）、表达式（LaTeX）、
//!   选项集合、分步值列表、结构化扩展（由学科包解释）。
//! - `Tolerance`：绝对容差（精确十进制）+ 相对容差（百万分比整数）；未配置时严格相等。

use serde::{Deserialize, Deserializer, Serialize};

use crate::decimal::Decimal;
use crate::error::{DomainError, invalid_field};
use crate::values::ComparatorId;

/// 答案键（§4）：四选一枚举。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerKey {
    /// 具体答案：有确定答案，可规则比较。
    Concrete(ConcreteKey),
    /// 仅评分标准：只给评分标准、无具体答案（必须存在有效 `Rubric`）。
    RubricOnly,
    /// 明确省略：写作「略」「见教材」等；可带备注。
    ExplicitlyOmitted {
        /// 备注（可空）。
        note: Option<String>,
    },
    /// 缺失：教师未提供（不得推断、不得让模型补）。
    Missing,
}

impl AnswerKey {
    /// 是否携带具体答案键。
    #[must_use]
    pub const fn has_concrete(&self) -> bool {
        matches!(self, Self::Concrete(_))
    }

    /// 是否要求题目存在生效评分标准（「仅评分标准」）。
    #[must_use]
    pub const fn requires_rubric(&self) -> bool {
        matches!(self, Self::RubricOnly)
    }

    /// 是否为「缺失」。
    #[must_use]
    pub const fn is_missing(&self) -> bool {
        matches!(self, Self::Missing)
    }

    /// 是否为「明确省略」。
    #[must_use]
    pub const fn is_explicitly_omitted(&self) -> bool {
        matches!(self, Self::ExplicitlyOmitted { .. })
    }

    /// 具体答案键引用（非具体答案时为空）。
    #[must_use]
    pub const fn concrete(&self) -> Option<&ConcreteKey> {
        match self {
            Self::Concrete(key) => Some(key),
            _ => None,
        }
    }

    /// 校验答案键与「禁止在缺失/省略下携带具体答案」不变量（结构保证，此处补充内容校验）。
    ///
    /// # Errors
    ///
    /// 具体答案键内容非法时返回 [`DomainError`]。
    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::Concrete(key) => key.validate(),
            Self::ExplicitlyOmitted { note } => {
                if let Some(note) = note
                    && note.trim().is_empty()
                {
                    return Err(invalid_field(
                        "note",
                        "省略备注不得为空字符串（应省略该字段）",
                    ));
                }
                Ok(())
            }
            Self::RubricOnly | Self::Missing => Ok(()),
        }
    }
}

/// 具体答案键（§4）：主答案 + 多解/等价写法 + 比较器 id 列表。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConcreteKey {
    /// 主答案。
    pub primary: AnswerValue,
    /// 多解/等价写法列表。
    pub alternatives: Vec<AnswerValue>,
    /// 比较器 id 列表；为空时由学科包按题型给默认链。
    pub comparators: Vec<ComparatorId>,
}

impl ConcreteKey {
    /// 校验主答案与候选答案（各自内容校验）。
    ///
    /// # Errors
    ///
    /// 任一带答案值/比较器 id 非法时返回 [`DomainError`]。
    pub fn validate(&self) -> Result<(), DomainError> {
        self.primary.validate()?;
        for alternative in &self.alternatives {
            alternative.validate()?;
        }
        Ok(())
    }
}

/// 答案值（§4）：具体答案或等价写法的结构化表示。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerValue {
    /// 文本。
    Text(String),
    /// 数值：精确十进制值 + 可选单位 + 可选容差。
    Number {
        /// 精确十进制数值（禁止二进制浮点）。
        value: Decimal,
        /// 单位（可空）。
        unit: Option<String>,
        /// 容差（可空；未配置时严格相等）。
        tolerance: Option<Tolerance>,
    },
    /// 表达式（LaTeX）。
    Expression(String),
    /// 选项集合（多选忽略顺序）。
    Options(Vec<String>),
    /// 分步值列表。
    Steps(Vec<AnswerValue>),
    /// 结构化扩展（由学科包解释，v1 仅供数学）。
    Structured(serde_json::Value),
}

impl AnswerValue {
    /// 内容校验（非空文本/表达式、合法容差、非空选项集合等）。
    ///
    /// # Errors
    ///
    /// 失败返回 [`DomainError`]。
    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::Text(text) => {
                if text.trim().is_empty() {
                    return Err(invalid_field("primary", "答案文本不得为空"));
                }
                Ok(())
            }
            Self::Number {
                unit, tolerance, ..
            } => {
                if let Some(unit) = unit
                    && unit.trim().is_empty()
                {
                    return Err(invalid_field("unit", "单位不得为空字符串（应省略该字段）"));
                }
                if let Some(tolerance) = tolerance {
                    tolerance.validate()?;
                }
                Ok(())
            }
            Self::Expression(latex) => {
                if latex.trim().is_empty() {
                    return Err(invalid_field("Expression", "LaTeX 表达式不得为空"));
                }
                Ok(())
            }
            Self::Options(options) => {
                if options.is_empty() {
                    return Err(invalid_field("Options", "选项集合不得为空"));
                }
                let mut seen: Vec<&str> = Vec::with_capacity(options.len());
                for option in options {
                    let trimmed = option.trim();
                    if trimmed.is_empty() {
                        return Err(invalid_field("Options", "选项不得为空"));
                    }
                    if seen.contains(&trimmed) {
                        return Err(invalid_field("Options", format!("选项重复：{trimmed}")));
                    }
                    seen.push(trimmed);
                }
                Ok(())
            }
            Self::Steps(steps) => {
                if steps.is_empty() {
                    return Err(invalid_field("Steps", "分步值列表不得为空"));
                }
                for step in steps {
                    step.validate()?;
                }
                Ok(())
            }
            Self::Structured(_) => Ok(()),
        }
    }
}

/// 容差（§4）：绝对容差（精确十进制）+ 相对容差（百万分比整数）。
///
/// **未配置时严格相等，禁止默认放宽容差**（规范 §4）。两个字段都为空表示严格相等。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct Tolerance {
    /// 绝对容差（可空）。
    pub absolute: Option<Decimal>,
    /// 相对容差，百万分比整数（可空）。
    pub relative_ppm: Option<u32>,
}

impl Tolerance {
    /// 严格相等（未配置任何容差）。
    pub const STRICT: Self = Self {
        absolute: None,
        relative_ppm: None,
    };

    /// 构造容差并校验非负。
    ///
    /// # Errors
    ///
    /// 绝对容差为负时返回 [`DomainError`]。
    pub fn new(absolute: Option<Decimal>, relative_ppm: Option<u32>) -> Result<Self, DomainError> {
        let tolerance = Self {
            absolute,
            relative_ppm,
        };
        tolerance.validate()?;
        Ok(tolerance)
    }

    /// 是否严格相等（两个容差字段都未配置）。
    #[must_use]
    pub const fn is_strict(&self) -> bool {
        self.absolute.is_none() && self.relative_ppm.is_none()
    }

    /// 校验容差（绝对容差不得为负）。
    ///
    /// # Errors
    ///
    /// 绝对容差为负时返回 [`DomainError`]。
    pub fn validate(&self) -> Result<(), DomainError> {
        if let Some(absolute) = self.absolute
            && absolute.is_negative()
        {
            return Err(invalid_field(
                "tolerance.absolute",
                "绝对容差不得为负（禁止默认放宽）",
            ));
        }
        Ok(())
    }
}

/// 反序列化用的线格式（严格校验非负容差）。
#[derive(Deserialize)]
struct ToleranceWire {
    absolute: Option<Decimal>,
    relative_ppm: Option<u32>,
}

impl<'de> Deserialize<'de> for Tolerance {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = ToleranceWire::deserialize(deserializer)?;
        Self::new(wire.absolute, wire.relative_ppm).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::values::ComparatorId;

    fn decimal(text: &str) -> Decimal {
        Decimal::parse(text).unwrap()
    }

    #[test]
    fn answer_key_variants_are_exclusive() {
        let concrete = AnswerKey::Concrete(ConcreteKey {
            primary: AnswerValue::Text("2".to_owned()),
            alternatives: vec![AnswerValue::Expression("\\frac{2}{1}".to_owned())],
            comparators: vec![ComparatorId::new("num_abs").unwrap()],
        });
        assert!(concrete.has_concrete());
        assert!(!concrete.requires_rubric());
        assert!(!concrete.is_missing());
        assert!(concrete.concrete().is_some());
        assert!(concrete.validate().is_ok());

        assert!(!AnswerKey::Missing.has_concrete());
        assert!(AnswerKey::Missing.is_missing());
        assert!(AnswerKey::RubricOnly.requires_rubric());
        assert!(!AnswerKey::RubricOnly.has_concrete());
        assert!(AnswerKey::ExplicitlyOmitted { note: None }.is_explicitly_omitted());
        assert!(
            AnswerKey::ExplicitlyOmitted {
                note: Some("见教材".to_owned())
            }
            .validate()
            .is_ok()
        );
        assert!(
            AnswerKey::ExplicitlyOmitted {
                note: Some("  ".to_owned())
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn answer_values_validate() {
        assert!(AnswerValue::Text("x".to_owned()).validate().is_ok());
        assert!(AnswerValue::Text(" ".to_owned()).validate().is_err());
        assert!(AnswerValue::Expression("x^2".to_owned()).validate().is_ok());
        assert!(AnswerValue::Expression(String::new()).validate().is_err());
        assert!(
            AnswerValue::Options(vec!["A".to_owned(), "B".to_owned()])
                .validate()
                .is_ok()
        );
        assert!(AnswerValue::Options(vec![]).validate().is_err());
        assert!(
            AnswerValue::Options(vec!["A".to_owned(), "A".to_owned()])
                .validate()
                .is_err()
        );
        let number = AnswerValue::Number {
            value: decimal("2.5"),
            unit: Some("m".to_owned()),
            tolerance: Some(Tolerance::new(Some(decimal("0.01")), None).unwrap()),
        };
        assert!(number.validate().is_ok());
        let negative_tolerance = AnswerValue::Number {
            value: decimal("2.5"),
            unit: None,
            tolerance: Some(Tolerance {
                absolute: Some(decimal("-0.1")),
                relative_ppm: None,
            }),
        };
        assert!(negative_tolerance.validate().is_err());
    }

    #[test]
    fn tolerance_strict_by_default() {
        assert!(Tolerance::STRICT.is_strict());
        assert!(Tolerance::default().is_strict());
        assert!(!Tolerance::new(None, Some(1000)).unwrap().is_strict());
        assert!(Tolerance::new(None, Some(0)).unwrap().validate().is_ok());
        assert!(
            Tolerance::new(Some(decimal("0.01")), Some(500))
                .unwrap()
                .validate()
                .is_ok()
        );
        assert!(
            Tolerance {
                absolute: None,
                relative_ppm: Some(u32::MAX),
            }
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn serde_tags_are_snake_case_and_structured() {
        let key = AnswerKey::RubricOnly;
        assert_eq!(serde_json::to_string(&key).unwrap(), "\"rubric_only\"");
        let omitted = AnswerKey::ExplicitlyOmitted {
            note: Some("略".to_owned()),
        };
        let json = serde_json::to_string(&omitted).unwrap();
        assert!(json.contains("explicitly_omitted"));
        let back: AnswerKey = serde_json::from_str(&json).unwrap();
        assert_eq!(back, omitted);

        let number = AnswerValue::Number {
            value: decimal("1.50"),
            unit: None,
            tolerance: None,
        };
        let json = serde_json::to_string(&number).unwrap();
        assert!(json.contains("\"number\""));
        assert!(json.contains("\"1.5\""));
        let back: AnswerValue = serde_json::from_str(&json).unwrap();
        assert_eq!(back, number);
    }

    #[test]
    fn tolerance_deserialization_rejects_negative() {
        let json = r#"{"absolute":"-0.1","relative_ppm":null}"#;
        assert!(serde_json::from_str::<Tolerance>(json).is_err());
        let json = r#"{"absolute":"0.1","relative_ppm":1000}"#;
        let tolerance: Tolerance = serde_json::from_str(json).unwrap();
        assert!(!tolerance.is_strict());
    }

    #[test]
    fn recursive_steps_are_supported() {
        let value = AnswerValue::Steps(vec![
            AnswerValue::Text("x=1".to_owned()),
            AnswerValue::Number {
                value: decimal("1"),
                unit: None,
                tolerance: None,
            },
        ]);
        assert!(value.validate().is_ok());
        assert!(AnswerValue::Steps(vec![]).validate().is_err());
    }
}
