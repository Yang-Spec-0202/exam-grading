//! 复核工作台视图模型（T37）：三栏数据、键盘映射、批量、抽检弹窗、导出后修改提示。

use crate::i18n::strings;
use crate::pages::ActionVm;

/// 复核页键盘动作（纯映射，焦点/编辑态守卫在 [`map_key`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReviewKey {
    /// 下一题。
    Next,
    /// 上一题。
    Prev,
    /// 确认并前进（`Enter`）。
    Accept,
    /// 聚焦改分输入（`E`）。
    FocusScore,
    /// 展开/收起证据（`Space`）。
    ToggleEvidence,
    /// 排除当前题（`X`，二次确认由服务负责）。
    Exclude,
    /// 撤销（`Cmd+Z`）。
    Undo,
    /// 数字改分（`0..9`，千分之一分的十分位输入见 [`ReviewPage::digit`]）。
    Digit(u32),
    /// 关闭弹层（`Esc`）。
    Escape,
}

/// 键盘文本 → 复核动作。
///
/// 规则（`SPEC-app.md` §2.1）：文本编辑态不触发单字母快捷键；`Cmd` 组合优先；
/// `E`/`J`/`K`/`X`/`Space` 只在非编辑态生效；`Enter`/`Esc` 在编辑态也可用。
#[must_use]
pub(crate) fn map_key(text: &str, command: bool, editing_text: bool) -> Option<ReviewKey> {
    if command {
        if text.eq_ignore_ascii_case("z") {
            return Some(ReviewKey::Undo);
        }
        return None;
    }
    match text {
        "Enter" | "\r" | "\n" => Some(ReviewKey::Accept),
        "Escape" | "Esc" => Some(ReviewKey::Escape),
        " " | "Space" if !editing_text => Some(ReviewKey::ToggleEvidence),
        _ if editing_text => None,
        "j" | "J" => Some(ReviewKey::Next),
        "k" | "K" => Some(ReviewKey::Prev),
        "e" | "E" => Some(ReviewKey::FocusScore),
        "x" | "X" => Some(ReviewKey::Exclude),
        _ if text.len() == 1 && text.chars().all(|c| c.is_ascii_digit()) => {
            text.parse::<u32>().ok().map(ReviewKey::Digit)
        }
        _ => None,
    }
}

/// 队列行视图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QueueRowVm {
    /// 判分记录 id（命令目标）。
    pub(crate) grade_id: String,
    /// 学生显示名（掩码）。
    pub(crate) student: String,
    /// 题号。
    pub(crate) question: String,
    /// 得分/满分文本。
    pub(crate) score: String,
    /// 模式（标准/快速）。
    pub(crate) mode: String,
    /// 抽检标记。
    pub(crate) sampled: bool,
    /// 状态。
    pub(crate) status: String,
    /// 复核原因。
    pub(crate) reasons: String,
    /// 是否当前选中。
    pub(crate) selected: bool,
    /// 是否只读（快速抽检）。
    pub(crate) read_only: bool,
    /// 是否加入批量。
    pub(crate) batched: bool,
}

/// 证据行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvidenceVm {
    /// 证据种类（如「识别文本」「比较器」）。
    pub(crate) label: String,
    /// 内容（文本摘要；图片按资产 id 懒加载）。
    pub(crate) value: String,
}

/// 抽检不通过弹窗视图（默认单题，取消不重跑）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SamplingVm {
    /// 差异摘要。
    pub(crate) message: String,
    /// 默认范围（`single`）。
    pub(crate) default_scope: String,
    /// 可选范围（`single`/`paper`）。
    pub(crate) options: Vec<String>,
    /// 是否可见。
    pub(crate) visible: bool,
}

/// 复核页投影。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ReviewVm {
    /// 是否当前页。
    pub(crate) visible: bool,
    /// 队列行。
    pub(crate) rows: Vec<QueueRowVm>,
    /// 队列计数文本。
    pub(crate) counts: String,
    /// 当前题标题。
    pub(crate) item_title: String,
    /// 元信息（学号/题号/模式/修订）。
    pub(crate) meta: String,
    /// 学生答案。
    pub(crate) answer: String,
    /// 参考答案。
    pub(crate) reference: String,
    /// 判定摘要。
    pub(crate) judgement: String,
    /// 当前得分文本。
    pub(crate) score_text: String,
    /// 改分输入框内容。
    pub(crate) score_input: String,
    /// 原因码选项。
    pub(crate) reasons: Vec<String>,
    /// 已选原因下标。
    pub(crate) reason_selected: i32,
    /// 证据行。
    pub(crate) evidence: Vec<EvidenceVm>,
    /// 证据面板是否可见。
    pub(crate) evidence_visible: bool,
    /// 是否只读。
    pub(crate) read_only: bool,
    /// 批量数。
    pub(crate) batch_count: u32,
    /// 是否可撤销。
    pub(crate) can_undo: bool,
    /// 是否可重做。
    pub(crate) can_redo: bool,
    /// 可撤销步数。
    pub(crate) undo_len: u32,
    /// 可重做步数。
    pub(crate) redo_len: u32,
    /// 抽检弹窗。
    pub(crate) sampling: SamplingVm,
    /// 导出后已修改提示（空 = 无）。
    pub(crate) export_stale: String,
    /// 最近操作回显。
    pub(crate) notice: String,
    /// 动作。
    pub(crate) actions: Vec<ActionVm>,
}

impl Default for SamplingVm {
    fn default() -> Self {
        Self {
            message: String::new(),
            default_scope: "single".to_owned(),
            options: vec!["single".to_owned(), "paper".to_owned()],
            visible: false,
        }
    }
}

/// 复核页状态。
#[derive(Debug, Clone, Default)]
pub(crate) struct ReviewPage {
    rows: Vec<QueueRowVm>,
    counts: String,
    selected: usize,
    meta: String,
    answer: String,
    reference: String,
    judgement: String,
    score_text: String,
    score_input: String,
    reasons: Vec<String>,
    reason_selected: i32,
    evidence: Vec<EvidenceVm>,
    evidence_visible: bool,
    read_only: bool,
    batch: Vec<usize>,
    undo_available: bool,
    undo_len: u32,
    redo_len: u32,
    sampling: SamplingVm,
    export_stale: bool,
    notice: String,
}

impl ReviewPage {
    /// 载入队列（选中第 1 行）。
    pub(crate) fn set_queue(&mut self, rows: Vec<QueueRowVm>) {
        self.rows = rows;
        self.selected = 0;
        self.batch.clear();
    }

    /// 队列计数说明（worker 从 `QueueCounts` 映射）。
    pub(crate) fn set_counts(&mut self, counts: String) {
        self.counts = counts;
    }

    /// 撤销/重做可用性（platform 持久栈；重启后仍有效）。
    pub(crate) fn set_history(&mut self, undo_len: usize, redo_len: usize) {
        self.undo_len = u32::try_from(undo_len).unwrap_or(u32::MAX);
        self.redo_len = u32::try_from(redo_len).unwrap_or(u32::MAX);
        self.undo_available = self.undo_len > 0;
    }

    /// 不改变判定的提示（命令被服务的只读/冲突规则拒绝等）。
    pub(crate) fn notice(&mut self, message: String) {
        self.notice = message;
    }

    /// 队列行数。
    pub(crate) fn len(&self) -> usize {
        self.rows.len()
    }

    /// 当前下标。
    pub(crate) fn selected(&self) -> usize {
        self.selected
    }

    /// 当前行。
    pub(crate) fn current(&self) -> Option<&QueueRowVm> {
        self.rows.get(self.selected)
    }

    /// 选中指定行（越界忽略）。
    pub(crate) fn select(&mut self, index: usize) {
        if index < self.rows.len() {
            self.selected = index;
            self.score_input.clear();
            self.notice.clear();
        }
    }

    /// 下一题（到末尾停住，不循环，避免误操作）。
    pub(crate) fn next(&mut self) {
        if self.selected + 1 < self.rows.len() {
            self.selected += 1;
            self.score_input.clear();
        }
    }

    /// 上一题。
    pub(crate) fn prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
        self.score_input.clear();
    }

    /// 应用详情（`worker` 从 `review::ReviewDetail` 映射）。
    pub(crate) fn set_detail(
        &mut self,
        meta: String,
        answer: String,
        reference: String,
        judgement: String,
        score_text: String,
        reasons: Vec<String>,
        evidence: Vec<EvidenceVm>,
        read_only: bool,
    ) {
        self.meta = meta;
        self.answer = answer;
        self.reference = reference;
        self.judgement = judgement;
        self.score_text = score_text;
        self.reasons = reasons;
        self.reason_selected = 0;
        self.evidence = evidence;
        self.read_only = read_only;
        if read_only {
            self.score_input.clear();
            self.batch.clear();
        }
        self.undo_available = !read_only;
    }

    /// 键盘动作 → 页面状态变化（命令由调用方转发）。
    pub(crate) fn on_key(&mut self, key: ReviewKey) {
        match key {
            ReviewKey::Next => self.next(),
            ReviewKey::Prev => self.prev(),
            ReviewKey::ToggleEvidence => self.evidence_visible = !self.evidence_visible,
            ReviewKey::Digit(digit) => {
                if !self.read_only {
                    self.score_input
                        .push(char::from_digit(digit, 10).unwrap_or('0'));
                }
            }
            ReviewKey::FocusScore | ReviewKey::Accept | ReviewKey::Exclude | ReviewKey::Undo => {}
            ReviewKey::Escape => self.sampling.visible = false,
        }
    }

    /// 改分输入变化（UI 文本框 → 页面）。
    pub(crate) fn set_score_input(&mut self, text: String) {
        if !self.read_only {
            self.score_input = text;
        }
    }

    /// 当前改分输入（空 = 未编辑）。
    pub(crate) fn score_input(&self) -> &str {
        &self.score_input
    }

    /// 已选原因码下标（0 = 未选）。
    pub(crate) fn reason_selected(&self) -> i32 {
        self.reason_selected
    }

    /// 选择原因码（UI 下拉）。
    pub(crate) fn select_reason(&mut self, index: i32) {
        if index >= 0 && (index as usize) < self.reasons.len().max(1) {
            self.reason_selected = index;
        }
    }

    /// 按原因码文本选择（ComboBox 回传的是字符串）。
    pub(crate) fn select_reason_by_text(&mut self, reason: &str) {
        if let Some(index) = self.reasons.iter().position(|item| item == reason) {
            self.reason_selected = i32::try_from(index).unwrap_or(0);
        }
    }

    /// 当前行加入/移出批量。
    pub(crate) fn toggle_batch(&mut self) {
        if self.read_only {
            return;
        }
        if let Some(position) = self.batch.iter().position(|index| *index == self.selected) {
            self.batch.remove(position);
        } else {
            self.batch.push(self.selected);
        }
    }

    /// 批量数量。
    pub(crate) fn batch_count(&self) -> u32 {
        u32::try_from(self.batch.len()).unwrap_or(u32::MAX)
    }

    /// 清空批量。
    pub(crate) fn clear_batch(&mut self) {
        self.batch.clear();
    }

    /// 批量目标下标。
    pub(crate) fn batch_indices(&self) -> &[usize] {
        &self.batch
    }

    /// 应用命令结果：接受/改分后前进；抽检不通过弹窗；撤销可用。
    pub(crate) fn applied(&mut self, notice: String, advance: bool) {
        self.notice = notice;
        self.undo_available = true;
        self.export_stale = true;
        if advance {
            self.next();
        }
    }

    /// 撤销可用时执行成功。
    pub(crate) fn undone(&mut self, notice: String) {
        self.notice = notice;
        self.export_stale = true;
    }

    /// 显示抽检不通过弹窗（默认单题）。
    pub(crate) fn show_sampling(&mut self, message: String) {
        self.sampling = SamplingVm {
            message,
            visible: true,
            ..SamplingVm::default()
        };
    }

    /// 关闭抽检弹窗：不重跑、保持待复核（`SPEC-app.md` §4）。
    pub(crate) fn cancel_sampling(&mut self) {
        self.sampling.visible = false;
        self.notice = strings().review_sampling_cancelled.to_owned();
    }

    /// 选择重跑范围并关闭弹窗；返回 `single`/`paper`。
    pub(crate) fn choose_sampling_scope(&mut self, scope: &str) -> Option<String> {
        if !self.sampling.visible || !self.sampling.options.iter().any(|item| item == scope) {
            return None;
        }
        self.sampling.visible = false;
        Some(scope.to_owned())
    }

    /// 导出成功：清除「导出后已修改」提示。
    pub(crate) fn exported(&mut self) {
        self.export_stale = false;
    }

    /// 是否已有导出后的修改。
    pub(crate) fn export_stale(&self) -> bool {
        self.export_stale
    }

    /// 投影。
    pub(crate) fn view(&self) -> ReviewVm {
        let strings = strings();
        let mut rows = self.rows.clone();
        for (index, row) in rows.iter_mut().enumerate() {
            row.selected = index == self.selected;
            row.batched = self.batch.contains(&index);
        }
        let can_undo = self.undo_available && !self.read_only;
        let can_redo = self.redo_len > 0 && !self.read_only;
        ReviewVm {
            visible: false,
            rows,
            counts: if self.counts.is_empty() {
                format!(
                    "{} {} / {} {}",
                    strings.review_queue_total,
                    self.rows.len(),
                    strings.review_batch,
                    self.batch.len()
                )
            } else {
                self.counts.clone()
            },
            item_title: self.current().map_or_else(String::new, |row| {
                format!("{} · {}", row.student, row.question)
            }),
            meta: self.meta.clone(),
            answer: self.answer.clone(),
            reference: self.reference.clone(),
            judgement: self.judgement.clone(),
            score_text: self.score_text.clone(),
            score_input: self.score_input.clone(),
            reasons: self.reasons.clone(),
            reason_selected: self.reason_selected,
            evidence: self.evidence.clone(),
            evidence_visible: self.evidence_visible,
            read_only: self.read_only,
            batch_count: u32::try_from(self.batch.len()).unwrap_or(u32::MAX),
            can_undo,
            can_redo,
            undo_len: self.undo_len,
            redo_len: self.redo_len,
            sampling: self.sampling.clone(),
            export_stale: if self.export_stale {
                strings.review_export_stale.to_owned()
            } else {
                String::new()
            },
            notice: self.notice.clone(),
            actions: {
                let mut actions = vec![
                    ActionVm {
                        id: "review.accept".to_owned(),
                        label: strings.action_accept.to_owned(),
                        enabled: !self.read_only,
                    },
                    ActionVm {
                        id: "review.override".to_owned(),
                        label: strings.action_override_score.to_owned(),
                        enabled: !self.read_only,
                    },
                    ActionVm {
                        id: "review.regrade_text".to_owned(),
                        label: strings.action_regrade_text.to_owned(),
                        enabled: !self.read_only,
                    },
                    ActionVm {
                        id: "review.batch_accept".to_owned(),
                        label: strings.action_batch_accept.to_owned(),
                        enabled: !self.read_only && !self.batch.is_empty(),
                    },
                    ActionVm {
                        id: "review.undo".to_owned(),
                        label: strings.action_undo.to_owned(),
                        enabled: can_undo,
                    },
                    ActionVm {
                        id: "review.redo".to_owned(),
                        label: strings.action_redo.to_owned(),
                        enabled: can_redo,
                    },
                ];
                if self.read_only {
                    actions.insert(
                        0,
                        ActionVm {
                            id: "review.sampling_pass".to_owned(),
                            label: strings.action_sampling_pass.to_owned(),
                            enabled: true,
                        },
                    );
                }
                actions
            },
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn row(index: usize) -> QueueRowVm {
        QueueRowVm {
            grade_id: format!("g{index}"),
            student: format!("学生{index}"),
            question: "17".to_owned(),
            score: "2/5".to_owned(),
            mode: "standard".to_owned(),
            sampled: false,
            status: "needs_review".to_owned(),
            reasons: "低置信度".to_owned(),
            selected: false,
            read_only: false,
            batched: false,
        }
    }

    #[test]
    fn keyboard_mapping_respects_edit_state_and_command() {
        assert_eq!(map_key("j", false, false), Some(ReviewKey::Next));
        assert_eq!(map_key("K", false, false), Some(ReviewKey::Prev));
        assert_eq!(map_key("j", false, true), None, "编辑态不触发单字母");
        assert_eq!(map_key("Enter", false, true), Some(ReviewKey::Accept));
        assert_eq!(map_key("Escape", false, true), Some(ReviewKey::Escape));
        assert_eq!(map_key("z", true, false), Some(ReviewKey::Undo));
        assert_eq!(map_key("5", false, false), Some(ReviewKey::Digit(5)));
        assert_eq!(map_key(" ", false, false), Some(ReviewKey::ToggleEvidence));
        assert_eq!(map_key("q", false, false), None);
    }

    #[test]
    fn jk_navigation_clamps_at_ends() {
        let mut page = ReviewPage::default();
        page.set_queue(vec![row(0), row(1), row(2)]);
        page.next();
        assert_eq!(page.selected(), 1);
        page.next();
        page.next();
        assert_eq!(page.selected(), 2, "末尾不循环");
        page.prev();
        page.prev();
        page.prev();
        assert_eq!(page.selected(), 0, "开头不下溢");
    }

    #[test]
    fn digits_fill_score_input_and_readonly_suppresses_edits() {
        let mut page = ReviewPage::default();
        page.set_queue(vec![row(0)]);
        page.set_detail(
            "meta".to_owned(),
            "答案".to_owned(),
            "参考".to_owned(),
            "部分".to_owned(),
            "2".to_owned(),
            vec!["低置信度".to_owned()],
            vec![],
            false,
        );
        page.on_key(ReviewKey::Digit(3));
        page.on_key(ReviewKey::Digit(0));
        assert_eq!(page.score_input(), "30");
        page.set_detail(
            "meta".to_owned(),
            String::new(),
            String::new(),
            String::new(),
            "5".to_owned(),
            vec![],
            vec![],
            true,
        );
        page.on_key(ReviewKey::Digit(1));
        page.toggle_batch();
        page.set_score_input("99".to_owned());
        assert_eq!(page.score_input(), "", "只读队列不得改分/批量");
        assert_eq!(page.batch_count(), 0);
    }

    #[test]
    fn batch_toggle_is_per_row() {
        let mut page = ReviewPage::default();
        page.set_queue(vec![row(0), row(1)]);
        page.toggle_batch();
        page.next();
        page.toggle_batch();
        page.toggle_batch();
        assert_eq!(page.batch_count(), 1);
        assert_eq!(page.batch_indices(), &[0]);
        page.clear_batch();
        assert_eq!(page.batch_count(), 0);
    }

    #[test]
    fn sampling_prompt_defaults_to_single_and_cancel_does_not_regrade() {
        let mut page = ReviewPage::default();
        page.set_queue(vec![row(0)]);
        page.show_sampling("抽检判定为不通过".to_owned());
        assert!(page.view().sampling.visible);
        assert_eq!(page.view().sampling.default_scope, "single");
        page.cancel_sampling();
        assert!(!page.view().sampling.visible);
        assert!(page.view().notice.contains("未选择"));
        // 再次弹窗后显式选择整卷。
        page.show_sampling("again".to_owned());
        assert_eq!(
            page.choose_sampling_scope("paper"),
            Some("paper".to_owned())
        );
        assert_eq!(page.choose_sampling_scope("paper"), None);
    }

    #[test]
    fn export_becomes_stale_after_review_command() {
        let mut page = ReviewPage::default();
        page.set_queue(vec![row(0), row(1)]);
        page.exported();
        assert!(!page.export_stale());
        page.applied("已确认".to_owned(), true);
        assert_eq!(page.selected(), 1, "接受后前进");
        assert!(page.export_stale());
        assert!(!page.view().export_stale.is_empty());
        page.exported();
        assert!(page.view().export_stale.is_empty());
    }
}
