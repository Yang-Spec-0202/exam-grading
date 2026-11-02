//! 设置页视图模型（T36）：12 个 CONFIG 分组、只读不变量、类型化编辑与拒绝提示。
//!
//! `worker` 用 [`SettingsPage::patch`] 生成候选 TOML，再交 `platform::ConfigStore` 校验并
//! 原子保存；失败时保留旧文档（`ConfigStore` 的 `.bak`/atomic write 语义）。

use crate::i18n::strings;
use crate::pages::ActionVm;

/// 设置分组（id, 覆盖的顶层表）。
const GROUP_TABLES: &[(&str, &[&str])] = &[
    ("general", &["general", "ui"]),
    ("import", &["import", "identity", "align"]),
    ("ocr", &["ocr"]),
    ("grade", &["grade"]),
    ("fast", &["fast"]),
    ("rubric", &["rubric"]),
    ("cas", &["cas"]),
    ("log", &["log", "diagnostics"]),
    ("privacy", &["privacy"]),
    ("storage", &["inference", "storage", "backup"]),
    ("docs", &["docs", "dev"]),
    ("meta", &["meta"]),
];

/// 不可配置项（`CONFIG.md` §12；只读展示 + 原因）。
const READ_ONLY: &[(&str, &str)] = &[
    (
        "rubric.require_confirmation",
        "恒开：规则必须教师确认才生效",
    ),
    ("audit.enabled", "恒开：审计只追加、哈希链"),
    ("privacy.per_task_consent", "恒开：每次任务鉴权"),
    ("dev.research_first", "恒开：实现前必须检索官方文档"),
    ("meta.schema_version", "由程序写入"),
    ("meta.workspace_id", "由程序写入"),
    ("meta.created_at", "由程序写入"),
];

/// 单个设置项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SettingEntryVm {
    /// 点分键（如 `grade.auto_accept_threshold`）。
    pub(crate) key: String,
    /// 显示值。
    pub(crate) value: String,
    /// 类型（`bool`/`int`/`float`/`string`/`list`）。
    pub(crate) kind: String,
    /// 是否只读。
    pub(crate) read_only: bool,
    /// 只读原因（可空）。
    pub(crate) note: String,
}

/// 设置分组。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SettingGroupVm {
    /// 分组 id。
    pub(crate) id: String,
    /// 标题。
    pub(crate) title: String,
    /// 表名（用于分组说明）。
    pub(crate) tables: String,
    /// 设置项。
    pub(crate) entries: Vec<SettingEntryVm>,
}

/// 设置页投影。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SettingsVm {
    /// 是否当前页。
    pub(crate) visible: bool,
    /// 分组。
    pub(crate) groups: Vec<SettingGroupVm>,
    /// 错误提示（保存/校验失败）。
    pub(crate) error: String,
    /// 是否保存中。
    pub(crate) saving: bool,
    /// 动作。
    pub(crate) actions: Vec<ActionVm>,
}

/// 设置页状态。
#[derive(Debug, Clone)]
pub(crate) struct SettingsPage {
    document: toml::Value,
    error: String,
}

impl SettingsPage {
    /// 用配置快照构造（`toml::Value::try_from(&ConfigDocument)` 由 worker 提供）。
    pub(crate) fn new(document: toml::Value) -> Self {
        Self {
            document,
            error: String::new(),
        }
    }

    /// 当前配置值。
    pub(crate) fn document(&self) -> &toml::Value {
        &self.document
    }

    /// 用保存成功后的文档替换。
    pub(crate) fn reload(&mut self, document: toml::Value) {
        self.document = document;
        self.error.clear();
    }

    /// 记录保存/校验失败（旧文档保留）。
    pub(crate) fn reject(&mut self, message: String) {
        self.error = message;
    }

    /// 生成候选文档（类型转换 + 只读/未知键拒绝；不修改自身）。
    ///
    /// # Errors
    ///
    /// 只读键、未知键、类型不符、空值时返回中文原因。
    pub(crate) fn patch(&self, key: &str, raw: &str) -> Result<toml::Value, String> {
        if let Some((_, reason)) = READ_ONLY.iter().find(|(read_only, _)| *read_only == key) {
            return Err(format!(
                "{}：{}（{}）",
                strings().settings_read_only,
                key,
                reason
            ));
        }
        let mut candidate = self.document.clone();
        let segments: Vec<&str> = key.split('.').collect();
        if segments.len() < 2 {
            return Err(format!("{}：{key}", strings().settings_unknown_key));
        }
        let mut cursor = &mut candidate;
        for (index, segment) in segments.iter().enumerate() {
            let Some(table) = cursor.as_table_mut() else {
                return Err(format!("{}：{key}", strings().settings_unknown_key));
            };
            if index + 1 == segments.len() {
                let Some(current) = table.get(*segment) else {
                    return Err(format!("{}：{key}", strings().settings_unknown_key));
                };
                let value = parse_value(current, raw)?;
                table.insert((*segment).to_owned(), value);
                return Ok(candidate);
            }
            let Some(next) = table.get_mut(*segment) else {
                return Err(format!("{}：{key}", strings().settings_unknown_key));
            };
            cursor = next;
        }
        Err(format!("{}：{key}", strings().settings_unknown_key))
    }

    /// 投影（分组 + 动作）。
    pub(crate) fn view(&self) -> SettingsVm {
        let strings = strings();
        SettingsVm {
            visible: false,
            groups: groups_from_document(&self.document),
            error: self.error.clone(),
            saving: false,
            actions: vec![
                ActionVm {
                    id: "settings.restore_backup".to_owned(),
                    label: strings.action_restore_default.to_owned(),
                    enabled: true,
                },
                ActionVm {
                    id: "settings.open_dir".to_owned(),
                    label: strings.action_open_data_dir.to_owned(),
                    enabled: true,
                },
            ],
        }
    }
}

/// 文档 → 分组视图（键值展平，只读项标注原因）。
pub(crate) fn groups_from_document(document: &toml::Value) -> Vec<SettingGroupVm> {
    let strings = strings();
    let mut groups = Vec::with_capacity(GROUP_TABLES.len());
    for (group_id, tables) in GROUP_TABLES {
        let mut entries = Vec::new();
        for table_name in *tables {
            if let Some(table) = document.get(*table_name) {
                flatten(table, table_name, &mut entries);
            }
        }
        groups.push(SettingGroupVm {
            id: (*group_id).to_owned(),
            title: strings.settings_group(group_id).to_owned(),
            tables: tables.join(", "),
            entries,
        });
    }
    groups
}

fn flatten(value: &toml::Value, prefix: &str, out: &mut Vec<SettingEntryVm>) {
    if let Some(table) = value.as_table() {
        for (key, child) in table {
            let path = format!("{prefix}.{key}");
            flatten(child, &path, out);
        }
        return;
    }
    let strings = strings();
    let read_only = READ_ONLY.iter().find(|(key, _)| *key == prefix);
    out.push(SettingEntryVm {
        key: prefix.to_owned(),
        value: format_value(value),
        kind: kind_of(value).to_owned(),
        read_only: read_only.is_some(),
        note: read_only.map_or_else(String::new, |(_, reason)| (*reason).to_owned()),
    });
    let _ = strings;
}

fn kind_of(value: &toml::Value) -> &'static str {
    match value {
        toml::Value::Boolean(_) => "bool",
        toml::Value::Integer(_) => "int",
        toml::Value::Float(_) => "float",
        toml::Value::String(_) => "string",
        toml::Value::Array(_) => "list",
        toml::Value::Datetime(_) => "datetime",
        toml::Value::Table(_) => "table",
    }
}

fn format_value(value: &toml::Value) -> String {
    match value {
        toml::Value::Boolean(flag) => {
            if *flag {
                "true".to_owned()
            } else {
                "false".to_owned()
            }
        }
        toml::Value::Integer(number) => number.to_string(),
        toml::Value::Float(number) => number.to_string(),
        toml::Value::String(text) => text.clone(),
        toml::Value::Array(items) => items
            .iter()
            .map(|item| match item {
                toml::Value::String(text) => text.clone(),
                other => other.to_string(),
            })
            .collect::<Vec<_>>()
            .join(", "),
        other => other.to_string(),
    }
}

fn parse_value(current: &toml::Value, raw: &str) -> Result<toml::Value, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(strings().settings_empty_value.to_owned());
    }
    let invalid = || format!("{}：{raw}", strings().settings_invalid_value);
    match current {
        toml::Value::Boolean(_) => match raw {
            "true" | "是" | "开" | "1" => Ok(toml::Value::Boolean(true)),
            "false" | "否" | "关" | "0" => Ok(toml::Value::Boolean(false)),
            _ => Err(invalid()),
        },
        toml::Value::Integer(_) => raw
            .parse::<i64>()
            .map(toml::Value::Integer)
            .map_err(|_| invalid()),
        toml::Value::Float(_) => raw
            .parse::<f64>()
            .map(toml::Value::Float)
            .map_err(|_| invalid()),
        toml::Value::String(_) => Ok(toml::Value::String(raw.to_owned())),
        toml::Value::Array(items) if items.iter().all(toml::Value::is_str) => {
            let values: Vec<toml::Value> = raw
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(|part| toml::Value::String(part.to_owned()))
                .collect();
            Ok(toml::Value::Array(values))
        }
        _ => Err(format!(
            "{}：{}",
            strings().settings_unsupported_kind,
            kind_of(current)
        )),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn document() -> toml::Value {
        toml::Value::Table(toml::toml! {
            [meta]
            schema_version = 1
            workspace_id = "01ARZ3NDEKTSV4RRFFQ69G5FAV"
            created_at = "2026-11-02T12:00:00Z"

            [general]
            mode = "standard"
            language = "zh-CN"

            [ui]
            page_size = 100
            undo_depth = 50

            [grade]
            score_granularity = 0.1
            auto_accept = 1.0

            [rubric]
            require_confirmation = true
            national_sets = ["new-curriculum-1", "new-curriculum-2"]

            [privacy]
            per_task_consent = true
            cloud_enabled = false
        })
    }

    #[test]
    fn twelve_groups_are_always_visible() {
        let page = SettingsPage::new(document());
        let view = page.view();
        assert_eq!(view.groups.len(), 12);
        let general = view
            .groups
            .iter()
            .find(|group| group.id == "general")
            .unwrap();
        assert!(
            general
                .entries
                .iter()
                .any(|entry| entry.key == "general.mode")
        );
        assert!(
            general
                .entries
                .iter()
                .any(|entry| entry.key == "ui.page_size")
        );
        assert!(view.groups.iter().all(|group| !group.title.is_empty()));
    }

    #[test]
    fn read_only_invariants_are_rejected_with_reason() {
        let page = SettingsPage::new(document());
        for key in [
            "rubric.require_confirmation",
            "privacy.per_task_consent",
            "meta.schema_version",
        ] {
            let error = page.patch(key, "false").unwrap_err();
            assert!(
                error.contains("恒开") || error.contains("程序写入"),
                "{key}: {error}"
            );
        }
        let view = page.view();
        let entry = view
            .groups
            .iter()
            .flat_map(|group| &group.entries)
            .find(|entry| entry.key == "rubric.require_confirmation")
            .unwrap();
        assert!(entry.read_only);
        assert!(!entry.note.is_empty());
    }

    #[test]
    fn patch_keeps_original_on_invalid_value() {
        let page = SettingsPage::new(document());
        assert_eq!(
            page.patch("general.mode", "fast").unwrap()["general"]["mode"]
                .as_str()
                .unwrap(),
            "fast"
        );
        assert!(page.patch("ui.page_size", "巨大").is_err());
        assert!(page.patch("meta.workspace_id", "x").is_err());
        assert!(page.patch("unknown.key", "1").is_err());
        // 原始文档不变（失败即回滚由 platform 的原子保存保证）。
        assert_eq!(
            page.document()["general"]["mode"].as_str().unwrap(),
            "standard"
        );
    }

    #[test]
    fn array_values_are_edited_as_comma_list() {
        let page = SettingsPage::new(document());
        let patched = page
            .patch("rubric.national_sets", "new-curriculum-1")
            .unwrap();
        let items = patched["rubric"]["national_sets"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].as_str().unwrap(), "new-curriculum-1");
    }

    #[test]
    fn invalid_value_message_is_explicit() {
        let page = SettingsPage::new(document());
        let error = page.patch("grade.score_granularity", "abc").unwrap_err();
        assert!(error.contains("abc"));
    }
}
