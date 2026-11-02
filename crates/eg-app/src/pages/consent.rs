//! 云端授权页视图模型（T38）：任务授权列表、新建/撤销、范围/预算/TTL、额度用尽提示。

use crate::i18n::strings;
use crate::pages::ActionVm;

/// 授权行视图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConsentRowVm {
    /// 授权 id。
    pub(crate) consent_id: String,
    /// 任务 id。
    pub(crate) task_id: String,
    /// 后端。
    pub(crate) backend: String,
    /// 状态机器码。
    pub(crate) state: String,
    /// 状态中文。
    pub(crate) state_label: String,
    /// 范围摘要（哈希前 12 位 + 说明）。
    pub(crate) scope: String,
    /// 预算文本。
    pub(crate) budget: String,
    /// 已用文本。
    pub(crate) spent: String,
    /// 剩余文本。
    pub(crate) remaining: String,
    /// 登记时间。
    pub(crate) granted_at: String,
    /// 是否额度用尽。
    pub(crate) exhausted: bool,
}

/// 授权页投影。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ConsentVm {
    /// 是否当前页。
    pub(crate) visible: bool,
    /// 行。
    pub(crate) rows: Vec<ConsentRowVm>,
    /// 空态提示。
    pub(crate) empty_hint: String,
    /// 额度用尽提示（有此类行时非空）。
    pub(crate) exhausted_hint: String,
    /// 新授权：任务 id。
    pub(crate) task_id: String,
    /// 新授权：后端。
    pub(crate) backend: String,
    /// 新授权：范围说明。
    pub(crate) scope: String,
    /// 新授权：预算（微货币单位）。
    pub(crate) budget: String,
    /// 新授权：有效期（TTL，小时）。
    pub(crate) ttl_hours: String,
    /// 错误。
    pub(crate) error: String,
    /// 回显。
    pub(crate) notice: String,
    /// 动作。
    pub(crate) actions: Vec<ActionVm>,
}

/// 新授权表单值（`worker` 用于登记）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct NewConsentVm {
    /// 任务 id。
    pub(crate) task_id: String,
    /// 后端。
    pub(crate) backend: String,
    /// 范围。
    pub(crate) scope: String,
    /// 预算。
    pub(crate) budget: String,
    /// 有效期（小时）。
    pub(crate) ttl_hours: String,
}

/// 授权页状态。
#[derive(Debug, Clone, Default)]
pub(crate) struct ConsentPage {
    rows: Vec<ConsentRowVm>,
    draft: NewConsentVm,
    error: String,
    notice: String,
}

impl ConsentPage {
    /// 用平台授权记录替换列表（`worker` 从 `ConsentStore::list_by_task` 汇总）。
    pub(crate) fn set_rows(&mut self, rows: Vec<ConsentRowVm>) {
        self.rows = rows;
        self.error.clear();
    }

    /// 行数。
    pub(crate) fn len(&self) -> usize {
        self.rows.len()
    }

    /// 设置新授权表单。
    pub(crate) fn set_draft(&mut self, draft: NewConsentVm) {
        self.draft = draft;
        self.error.clear();
    }

    /// 表单校验：任务/范围非空，预算为整数，TTL 1–168 小时。
    ///
    /// # Errors
    ///
    /// 任一字段非法返回中文原因。
    pub(crate) fn validate_draft(&self) -> Result<(), String> {
        let strings = strings();
        if self.draft.task_id.trim().is_empty() {
            return Err(strings.consent_need_task.to_owned());
        }
        if self.draft.scope.trim().is_empty() {
            return Err(strings.consent_need_scope.to_owned());
        }
        let Ok(budget) = self.draft.budget.trim().parse::<u64>() else {
            return Err(strings.consent_bad_budget.to_owned());
        };
        if budget == 0 {
            return Err(strings.consent_bad_budget.to_owned());
        }
        let Ok(ttl) = self.draft.ttl_hours.trim().parse::<u32>() else {
            return Err(strings.consent_bad_ttl.to_owned());
        };
        if !(1..=168).contains(&ttl) {
            return Err(strings.consent_bad_ttl.to_owned());
        }
        Ok(())
    }

    /// 登记成功回显。
    pub(crate) fn registered(&mut self, consent_id: &str) {
        self.notice = format!("{} {}", strings().consent_registered, consent_id);
        self.draft = NewConsentVm::default();
    }

    /// 撤销成功回显（撤销立即生效，列表由后台刷新）。
    pub(crate) fn revoked(&mut self, consent_id: &str) {
        self.notice = format!("{} {}", strings().consent_revoked, consent_id);
    }

    /// 操作失败。
    pub(crate) fn fail(&mut self, message: String) {
        self.error = message;
    }

    /// 投影。
    pub(crate) fn view(&self) -> ConsentVm {
        let strings = strings();
        let exhausted = self.rows.iter().any(|row| row.exhausted);
        ConsentVm {
            visible: false,
            rows: self.rows.clone(),
            empty_hint: if self.rows.is_empty() {
                strings.consent_empty.to_owned()
            } else {
                String::new()
            },
            exhausted_hint: if exhausted {
                strings.consent_exhausted_hint.to_owned()
            } else {
                String::new()
            },
            task_id: self.draft.task_id.clone(),
            backend: self.draft.backend.clone(),
            scope: self.draft.scope.clone(),
            budget: self.draft.budget.clone(),
            ttl_hours: self.draft.ttl_hours.clone(),
            error: self.error.clone(),
            notice: self.notice.clone(),
            actions: vec![
                ActionVm {
                    id: "consent.register".to_owned(),
                    label: strings.action_register_consent.to_owned(),
                    enabled: true,
                },
                ActionVm {
                    id: "consent.refresh".to_owned(),
                    label: strings.action_refresh.to_owned(),
                    enabled: true,
                },
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn row(state: &str, exhausted: bool) -> ConsentRowVm {
        ConsentRowVm {
            consent_id: "k1".to_owned(),
            task_id: "t1".to_owned(),
            backend: "openai_compat".to_owned(),
            state: state.to_owned(),
            state_label: if exhausted { "额度用尽" } else { "有效" }.to_owned(),
            scope: "assets:a,b".to_owned(),
            budget: "1000".to_owned(),
            spent: if exhausted { "1000" } else { "200" }.to_owned(),
            remaining: if exhausted { "0" } else { "800" }.to_owned(),
            granted_at: "2026-11-02T12:00:00Z".to_owned(),
            exhausted,
        }
    }

    fn draft() -> NewConsentVm {
        NewConsentVm {
            task_id: "t1".to_owned(),
            backend: "openai_compat".to_owned(),
            scope: "assets:a".to_owned(),
            budget: "1000".to_owned(),
            ttl_hours: "24".to_owned(),
        }
    }

    #[test]
    fn draft_validation_rejects_missing_or_invalid_fields() {
        let mut page = ConsentPage::default();
        page.set_draft(NewConsentVm::default());
        assert!(page.validate_draft().is_err());
        page.set_draft(draft());
        page.validate_draft().unwrap();
        let mut bad_budget = draft();
        bad_budget.budget = "0".to_owned();
        assert!(page_validate(&bad_budget).is_err());
        let mut bad_ttl = draft();
        bad_ttl.ttl_hours = "0".to_owned();
        assert!(page_validate(&bad_ttl).is_err());
    }

    fn page_validate(draft: &NewConsentVm) -> Result<(), String> {
        let mut page = ConsentPage::default();
        page.set_draft(draft.clone());
        page.validate_draft()
    }

    #[test]
    fn exhausted_rows_show_legend_and_remaining_zero() {
        let mut page = ConsentPage::default();
        page.set_rows(vec![row("active", false), row("exhausted", true)]);
        let view = page.view();
        assert_eq!(view.rows.len(), 2);
        assert!(view.rows[1].exhausted);
        assert_eq!(view.rows[1].remaining, "0");
        assert!(!view.exhausted_hint.is_empty());
        assert!(view.empty_hint.is_empty());
    }

    #[test]
    fn empty_state_and_actions_are_visible() {
        let page = ConsentPage::default();
        let view = page.view();
        assert!(view.rows.is_empty());
        assert!(!view.empty_hint.is_empty());
        assert!(view.actions.iter().any(|a| a.id == "consent.register"));
    }

    #[test]
    fn register_and_revoke_produce_explicit_notice() {
        let mut page = ConsentPage::default();
        page.registered("k9");
        assert!(page.view().notice.contains("k9"));
        page.revoked("k9");
        assert!(page.view().notice.contains("已撤销"));
        page.fail("授权不存在".to_owned());
        assert!(page.view().error.contains("不存在"));
    }
}
