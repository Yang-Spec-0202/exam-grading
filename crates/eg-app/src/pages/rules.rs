//! 规则页视图模型（T38）：原文 → 整理 → 草稿 → 校验 → 确认/拒绝状态机。

use crate::i18n::strings;
use crate::pages::ActionVm;

/// 规则页阶段（`SPEC-app.md` §4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RulesStage {
    /// 未选择/空。
    Empty,
    /// 编辑原文（或手写评分项）。
    Editing,
    /// 模型整理中。
    Structuring,
    /// 草稿待确认（含校验问题）。
    Draft,
    /// 已确认（终态）。
    Confirmed,
    /// 已拒绝（终态）。
    Rejected,
    /// 整理/校验失败（保留原文，可重试）。
    Failed,
}

impl RulesStage {
    /// 稳定机器码。
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Editing => "editing",
            Self::Structuring => "structuring",
            Self::Draft => "draft",
            Self::Confirmed => "confirmed",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
        }
    }
}

/// 草稿评分项行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuleItemVm {
    /// 可读判据。
    pub(crate) criterion: String,
    /// 分值文本。
    pub(crate) points: String,
    /// 是否必须满足。
    pub(crate) required: bool,
}

/// 校验问题行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FindingVm {
    /// 检查项 id。
    pub(crate) check: String,
    /// 是否错误（false = 警告）。
    pub(crate) error: bool,
    /// 中文说明。
    pub(crate) message: String,
}

/// 规则页投影。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RulesVm {
    /// 是否当前页。
    pub(crate) visible: bool,
    /// 阶段机器码。
    pub(crate) stage: String,
    /// 阶段中文标签。
    pub(crate) stage_label: String,
    /// 原文。
    pub(crate) raw_text: String,
    /// 模型调用 id（整理失败/成功都显示，便于复现）。
    pub(crate) model_call_id: String,
    /// 草稿 id。
    pub(crate) draft_id: String,
    /// 评分项。
    pub(crate) items: Vec<RuleItemVm>,
    /// 校验问题。
    pub(crate) findings: Vec<FindingVm>,
    /// 错误数。
    pub(crate) errors: u32,
    /// 警告数。
    pub(crate) warnings: u32,
    /// 确认按钮可用（无错误 + 草稿阶段）。
    pub(crate) confirm_enabled: bool,
    /// 拒绝按钮可用。
    pub(crate) reject_enabled: bool,
    /// 已确认后的规则版本。
    pub(crate) version: String,
    /// 回显/错误提示。
    pub(crate) notice: String,
    /// 动作。
    pub(crate) actions: Vec<ActionVm>,
}

/// 规则页状态。
#[derive(Debug, Clone)]
pub(crate) struct RulesPage {
    stage: RulesStage,
    raw_text: String,
    model_call_id: String,
    draft_id: String,
    items: Vec<RuleItemVm>,
    findings: Vec<FindingVm>,
    version: u32,
    notice: String,
}

impl Default for RulesPage {
    fn default() -> Self {
        Self {
            stage: RulesStage::Empty,
            raw_text: String::new(),
            model_call_id: String::new(),
            draft_id: String::new(),
            items: Vec::new(),
            findings: Vec::new(),
            version: 0,
            notice: String::new(),
        }
    }
}

impl RulesPage {
    /// 当前阶段。
    pub(crate) fn stage(&self) -> RulesStage {
        self.stage
    }

    /// 原文。
    pub(crate) fn raw_text(&self) -> &str {
        &self.raw_text
    }

    /// 编辑原文（非终态）。
    pub(crate) fn edit_raw(&mut self, text: String) {
        if matches!(self.stage, RulesStage::Confirmed | RulesStage::Rejected) {
            self.notice = strings().rules_terminal.to_owned();
            return;
        }
        self.raw_text = text;
        self.stage = if self.raw_text.trim().is_empty() {
            RulesStage::Empty
        } else {
            RulesStage::Editing
        };
        self.notice.clear();
    }

    /// 开始模型整理（需要原文非空且不在整理中）。
    ///
    /// # Errors
    ///
    /// 原文为空或正在整理时返回中文原因。
    pub(crate) fn begin_structuring(&mut self) -> Result<(), String> {
        if self.raw_text.trim().is_empty() {
            return Err(strings().rules_need_raw_text.to_owned());
        }
        if self.stage == RulesStage::Structuring {
            return Err(strings().rules_busy.to_owned());
        }
        self.stage = RulesStage::Structuring;
        self.notice.clear();
        Ok(())
    }

    /// 整理失败：保留原文、显示错误与模型调用 id（不生成可确认草稿）。
    pub(crate) fn structuring_failed(&mut self, message: String, model_call_id: String) {
        self.stage = RulesStage::Failed;
        self.model_call_id = model_call_id;
        self.items.clear();
        self.findings.clear();
        self.notice = message;
    }

    /// 草稿就绪（来自 rubric 引擎）。
    pub(crate) fn draft_ready(
        &mut self,
        draft_id: String,
        model_call_id: String,
        items: Vec<RuleItemVm>,
        findings: Vec<FindingVm>,
    ) {
        self.stage = RulesStage::Draft;
        self.draft_id = draft_id;
        self.model_call_id = model_call_id;
        self.items = items;
        self.findings = findings;
        self.notice.clear();
    }

    /// 更新校验问题（改草稿后重跑校验）。
    pub(crate) fn set_findings(&mut self, findings: Vec<FindingVm>) {
        if self.stage == RulesStage::Draft {
            self.findings = findings;
        }
    }

    /// 错误数。
    pub(crate) fn errors(&self) -> u32 {
        u32::try_from(self.findings.iter().filter(|finding| finding.error).count()).unwrap_or(0)
    }

    /// 确认请求：有错误级问题时拒绝（返回 `Err` 给调用方提示）。
    ///
    /// # Errors
    ///
    /// 阶段不允许或有错误级问题时返回中文原因。
    pub(crate) fn request_confirm(&mut self) -> Result<(), String> {
        if self.stage != RulesStage::Draft {
            return Err(strings().rules_not_draft.to_owned());
        }
        if self.errors() > 0 {
            return Err(strings().rules_has_errors.to_owned());
        }
        Ok(())
    }

    /// 确认成功（生成新规则版本）。
    pub(crate) fn confirmed(&mut self, version: u32) {
        self.stage = RulesStage::Confirmed;
        self.version = version;
        self.notice = strings().rules_confirmed.to_owned();
    }

    /// 拒绝成功（保留原因）。
    pub(crate) fn rejected(&mut self, reason: String) {
        self.stage = RulesStage::Rejected;
        self.notice = if reason.is_empty() {
            strings().rules_rejected.to_owned()
        } else {
            format!("{}：{reason}", strings().rules_rejected)
        };
    }

    /// 回到可编辑状态（重新开始）。
    pub(crate) fn reset(&mut self) {
        if matches!(self.stage, RulesStage::Confirmed | RulesStage::Rejected) {
            self.notice = strings().rules_terminal.to_owned();
            return;
        }
        self.stage = if self.raw_text.trim().is_empty() {
            RulesStage::Empty
        } else {
            RulesStage::Editing
        };
        self.draft_id.clear();
        self.items.clear();
        self.findings.clear();
        self.notice.clear();
    }

    /// 投影。
    pub(crate) fn view(&self) -> RulesVm {
        let strings = strings();
        let errors = self.errors();
        let warnings = u32::try_from(self.findings.len())
            .unwrap_or(0)
            .saturating_sub(errors);
        let confirm_enabled = self.stage == RulesStage::Draft && errors == 0;
        let reject_enabled = self.stage == RulesStage::Draft;
        let stage_label = strings.rules_stage(self.stage.code()).to_owned();
        RulesVm {
            visible: false,
            stage: self.stage.code().to_owned(),
            stage_label,
            raw_text: self.raw_text.clone(),
            model_call_id: self.model_call_id.clone(),
            draft_id: self.draft_id.clone(),
            items: self.items.clone(),
            findings: self.findings.clone(),
            errors,
            warnings,
            confirm_enabled,
            reject_enabled,
            version: if self.version == 0 {
                String::new()
            } else {
                self.version.to_string()
            },
            notice: self.notice.clone(),
            actions: vec![
                ActionVm {
                    id: "rules.structure".to_owned(),
                    label: strings.action_structure.to_owned(),
                    enabled: matches!(self.stage, RulesStage::Editing | RulesStage::Failed),
                },
                ActionVm {
                    id: "rules.confirm".to_owned(),
                    label: strings.action_confirm_rubric.to_owned(),
                    enabled: confirm_enabled,
                },
                ActionVm {
                    id: "rules.reject".to_owned(),
                    label: strings.action_reject.to_owned(),
                    enabled: reject_enabled,
                },
                ActionVm {
                    id: "rules.reset".to_owned(),
                    label: strings.action_reset.to_owned(),
                    enabled: !matches!(self.stage, RulesStage::Confirmed | RulesStage::Rejected),
                },
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn finding(error: bool) -> FindingVm {
        FindingVm {
            check: "R01".to_owned(),
            error,
            message: "分值合计与满分不一致".to_owned(),
        }
    }

    #[test]
    fn structuring_requires_raw_text() {
        let mut page = RulesPage::default();
        assert!(page.begin_structuring().is_err());
        page.edit_raw("第 17 题：写出结果得 5 分".to_owned());
        assert_eq!(page.stage(), RulesStage::Editing);
        page.begin_structuring().unwrap();
        assert_eq!(page.stage(), RulesStage::Structuring);
        assert!(page.begin_structuring().is_err(), "整理中不得重复发起");
    }

    #[test]
    fn failed_structuring_keeps_raw_text_and_blocks_confirm() {
        let mut page = RulesPage::default();
        page.edit_raw("原文".to_owned());
        page.begin_structuring().unwrap();
        page.structuring_failed("模型不可用".to_owned(), "call-1".to_owned());
        assert_eq!(page.stage(), RulesStage::Failed);
        assert_eq!(page.raw_text(), "原文");
        assert!(!page.view().confirm_enabled);
        assert_eq!(page.view().model_call_id, "call-1");
    }

    #[test]
    fn confirm_requires_draft_without_errors() {
        let mut page = RulesPage::default();
        page.edit_raw("原文".to_owned());
        assert!(page.request_confirm().is_err(), "无草稿不得确认");
        page.draft_ready(
            "draft-1".to_owned(),
            "call-1".to_owned(),
            vec![RuleItemVm {
                criterion: "写出正确结果".to_owned(),
                points: "5".to_owned(),
                required: true,
            }],
            vec![finding(true), finding(false)],
        );
        assert_eq!(page.stage(), RulesStage::Draft);
        assert_eq!(page.errors(), 1);
        assert!(!page.view().confirm_enabled);
        assert!(page.request_confirm().is_err(), "有错误级问题必须禁用确认");
        page.set_findings(vec![finding(false)]);
        assert!(page.view().confirm_enabled);
        page.request_confirm().unwrap();
        page.confirmed(3);
        assert_eq!(page.stage(), RulesStage::Confirmed);
        assert_eq!(page.view().version, "3");
        assert!(!page.view().confirm_enabled);
    }

    #[test]
    fn reject_and_terminal_state_are_visible() {
        let mut page = RulesPage::default();
        page.edit_raw("原文".to_owned());
        page.draft_ready("d".to_owned(), String::new(), vec![], vec![]);
        page.rejected("分值不清".to_owned());
        assert_eq!(page.stage(), RulesStage::Rejected);
        assert!(page.view().notice.contains("分值不清"));
        page.edit_raw("新原文".to_owned());
        assert_eq!(page.stage(), RulesStage::Rejected, "终态不可再编辑");
        page.reset();
        assert_eq!(page.stage(), RulesStage::Rejected, "终态不得重置为可编辑");
    }
}
