//! 进度页视图模型（T36）：任务队列、阶段、失败项与重试/取消入口。

use crate::i18n::strings;
use crate::pages::ActionVm;

/// 任务行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaskRowVm {
    /// 任务 id。
    pub(crate) id: String,
    /// 任务类型（机器码）。
    pub(crate) kind: String,
    /// 状态（机器码）。
    pub(crate) state: String,
    /// 阶段（可空）。
    pub(crate) stage: String,
    /// 进度文案。
    pub(crate) progress: String,
    /// 已完成。
    pub(crate) done: u32,
    /// 总数。
    pub(crate) total: u32,
    /// 已尝试次数/上限。
    pub(crate) attempts: u32,
    /// 最大尝试次数。
    pub(crate) max_attempts: u32,
    /// 失败原因（可空）。
    pub(crate) error: String,
    /// 是否可重试。
    pub(crate) retryable: bool,
    /// 是否可取消（非终态）。
    pub(crate) cancelable: bool,
}

/// 进度页投影。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ProgressVm {
    /// 是否当前页。
    pub(crate) visible: bool,
    /// 行。
    pub(crate) rows: Vec<TaskRowVm>,
    /// 运行中计数。
    pub(crate) running: u32,
    /// 待运行计数。
    pub(crate) pending: u32,
    /// 终态失败计数。
    pub(crate) failed: u32,
    /// 汇总短句。
    pub(crate) summary: String,
    /// 动作。
    pub(crate) actions: Vec<ActionVm>,
}

/// 进度页状态。
#[derive(Debug, Clone, Default)]
pub(crate) struct ProgressPage {
    rows: Vec<TaskRowVm>,
}

impl ProgressPage {
    /// 用后台快照整体替换（`platform::TaskQueue::list`）。
    pub(crate) fn set_tasks(&mut self, rows: Vec<TaskRowVm>) {
        self.rows = rows;
    }

    /// 逐任务进度更新（按 `task_id` 合并，不新增重复行）。
    pub(crate) fn progress(&mut self, task_id: &str, label: &str, done: u32, total: u32) {
        if let Some(row) = self.rows.iter_mut().find(|row| row.id == task_id) {
            row.done = done;
            row.total = total;
            row.progress = label.to_owned();
            row.cancelable = true;
            return;
        }
        self.rows.push(TaskRowVm {
            id: task_id.to_owned(),
            kind: String::new(),
            state: "running".to_owned(),
            stage: label.to_owned(),
            progress: label.to_owned(),
            done,
            total,
            attempts: 0,
            max_attempts: 0,
            error: String::new(),
            retryable: false,
            cancelable: true,
        });
    }

    /// 失败事件：标记行并附原因。
    pub(crate) fn fail(&mut self, task_id: &str, message: &str, retryable: bool) {
        if let Some(row) = self.rows.iter_mut().find(|row| row.id == task_id) {
            row.state = "failed".to_owned();
            row.error = message.to_owned();
            row.retryable = retryable;
            row.cancelable = false;
            return;
        }
        self.rows.push(TaskRowVm {
            id: task_id.to_owned(),
            kind: String::new(),
            state: "failed".to_owned(),
            stage: String::new(),
            progress: String::new(),
            done: 0,
            total: 0,
            attempts: 0,
            max_attempts: 0,
            error: message.to_owned(),
            retryable,
            cancelable: false,
        });
    }

    /// 投影（带计数与动作）。
    pub(crate) fn view(&self) -> ProgressVm {
        let running = u32::try_from(
            self.rows
                .iter()
                .filter(|row| row.state == "running")
                .count(),
        )
        .unwrap_or(0);
        let pending = u32::try_from(
            self.rows
                .iter()
                .filter(|row| row.state == "pending")
                .count(),
        )
        .unwrap_or(0);
        let failed = u32::try_from(self.rows.iter().filter(|row| row.state == "failed").count())
            .unwrap_or(0);
        let strings = strings();
        let summary = format!(
            "{} {} / {} {} / {} {}",
            strings.progress_running,
            running,
            strings.progress_pending,
            pending,
            strings.progress_failed,
            failed
        );
        ProgressVm {
            visible: false,
            rows: self.rows.clone(),
            running,
            pending,
            failed,
            summary,
            actions: vec![
                ActionVm {
                    id: "progress.refresh".to_owned(),
                    label: strings.action_refresh.to_owned(),
                    enabled: true,
                },
                ActionVm {
                    id: "progress.cancel_all".to_owned(),
                    label: strings.action_cancel_tasks.to_owned(),
                    enabled: running + pending > 0,
                },
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn row(id: &str, state: &str) -> TaskRowVm {
        TaskRowVm {
            id: id.to_owned(),
            kind: "submission_grade".to_owned(),
            state: state.to_owned(),
            stage: String::new(),
            progress: String::new(),
            done: 0,
            total: 0,
            attempts: 0,
            max_attempts: 3,
            error: String::new(),
            retryable: false,
            cancelable: state == "running" || state == "pending",
        }
    }

    #[test]
    fn progress_updates_existing_row_and_creates_unknown() {
        let mut page = ProgressPage::default();
        page.set_tasks(vec![row("t1", "running")]);
        page.progress("t1", "识别", 3, 40);
        page.progress("t2", "判分", 1, 10);
        let view = page.view();
        assert_eq!(view.rows.len(), 2);
        assert_eq!(view.rows[0].done, 3);
        assert_eq!(view.running, 2);
    }

    #[test]
    fn failure_marks_retryable_and_counts() {
        let mut page = ProgressPage::default();
        page.set_tasks(vec![row("t1", "running")]);
        page.fail("t1", "模型超时", true);
        let view = page.view();
        assert_eq!(view.failed, 1);
        assert_eq!(view.running, 0);
        assert!(view.rows[0].retryable);
        assert!(!view.rows[0].cancelable);
        assert!(view.actions.iter().any(|a| a.id == "progress.refresh"));
    }

    #[test]
    fn empty_queue_has_disabled_cancel() {
        let page = ProgressPage::default();
        let view = page.view();
        assert_eq!(view.rows.len(), 0);
        assert!(
            !view
                .actions
                .iter()
                .find(|a| a.id == "progress.cancel_all")
                .unwrap()
                .enabled
        );
    }
}
