//! 导入页视图模型（T36）：文件选择/拖拽、命名提醒仅一次、部分成功可视化。

use std::path::PathBuf;

use crate::i18n::strings;
use crate::pages::ActionVm;

/// 单个导入文件的行状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportRowStatus {
    /// 成功。
    Ok,
    /// 有页面被拒收。
    Rejected,
    /// 文件级失败。
    Failed,
    /// 同批/历史重复。
    Duplicate,
}

impl ImportRowStatus {
    /// 稳定机器码（UI 颜色映射）。
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
            Self::Duplicate => "duplicate",
        }
    }
}

/// 导入文件行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportRowVm {
    /// 文件名（不含路径）。
    pub(crate) name: String,
    /// 状态。
    pub(crate) status: ImportRowStatus,
    /// 页数/原因等短说明。
    pub(crate) detail: String,
}

/// 一次导入的汇总（由 `worker` 从 `eg_capture::ImportReport` 映射）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ImportReportVm {
    /// 成功文件数。
    pub(crate) ok: u32,
    /// 有拒收页的文件数。
    pub(crate) rejected: u32,
    /// 失败文件数。
    pub(crate) failed: u32,
    /// 重复命中数。
    pub(crate) duplicates: u32,
    /// 逐文件行。
    pub(crate) rows: Vec<ImportRowVm>,
    /// 汇总短句（已本地化）。
    pub(crate) summary: String,
}

/// 导入页投影。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ImportVm {
    /// 是否当前页。
    pub(crate) visible: bool,
    /// 拖拽/按钮提示。
    pub(crate) hint: String,
    /// 命名提醒正文（空 = 不显示）。
    pub(crate) naming_text: String,
    /// 是否显示命名提醒。
    pub(crate) naming_visible: bool,
    /// 是否导入中。
    pub(crate) running: bool,
    /// 已完成/总数。
    pub(crate) done: u32,
    /// 总数。
    pub(crate) total: u32,
    /// 成功/拒收/失败/重复计数。
    pub(crate) ok: u32,
    /// 拒收计数。
    pub(crate) rejected: u32,
    /// 失败计数。
    pub(crate) failed: u32,
    /// 重复计数。
    pub(crate) duplicates: u32,
    /// 文件行。
    pub(crate) rows: Vec<ImportRowVm>,
    /// 结果汇总（空 = 未完成）。
    pub(crate) summary: String,
    /// 动作。
    pub(crate) actions: Vec<ActionVm>,
}

/// 导入页状态。
#[derive(Debug, Clone)]
pub(crate) struct ImportPage {
    reminder_shown: bool,
    last_paths: Vec<PathBuf>,
    view: ImportVm,
}

impl Default for ImportPage {
    fn default() -> Self {
        Self {
            reminder_shown: false,
            last_paths: Vec::new(),
            view: ImportVm {
                hint: strings().import_hint.to_owned(),
                ..ImportVm::default()
            },
        }
    }
}

impl ImportPage {
    /// 当前投影（`visible` 由页面切换填充）。
    pub(crate) fn view(&self) -> &ImportVm {
        &self.view
    }

    /// 上次选择的路径（重试用）。
    pub(crate) fn last_paths(&self) -> &[PathBuf] {
        &self.last_paths
    }

    /// 选择/拖入文件：返回要导入的路径；命名提醒仅在首次出现（`import.remind_naming`）。
    pub(crate) fn pick(
        &mut self,
        paths: Vec<PathBuf>,
        remind_naming: bool,
    ) -> Option<Vec<PathBuf>> {
        if paths.is_empty() {
            self.view.summary = strings().import_no_files.to_owned();
            return None;
        }
        let show_reminder = remind_naming && !self.reminder_shown;
        self.reminder_shown |= show_reminder;
        self.view.naming_visible = show_reminder;
        self.view.naming_text = if show_reminder {
            strings().import_naming.to_owned()
        } else {
            String::new()
        };
        self.last_paths = paths.clone();
        self.view.running = true;
        self.view.done = 0;
        self.view.total = u32::try_from(paths.len()).unwrap_or(u32::MAX);
        self.view.rows.clear();
        self.view.summary.clear();
        self.view.ok = 0;
        self.view.rejected = 0;
        self.view.failed = 0;
        self.view.duplicates = 0;
        Some(paths)
    }

    /// 关闭命名提醒。
    pub(crate) fn dismiss_naming(&mut self) {
        self.view.naming_visible = false;
    }

    /// 进度更新（逐文件）。
    pub(crate) fn progress(&mut self, done: u32, total: u32) {
        self.view.done = done;
        self.view.total = total;
    }

    /// 导入完成：显示部分成功/拒收/失败明细。
    pub(crate) fn finish(&mut self, report: ImportReportVm) {
        self.view.running = false;
        self.view.done = report.ok + report.rejected + report.failed;
        self.view.total = self.view.total.max(self.view.done);
        self.view.ok = report.ok;
        self.view.rejected = report.rejected;
        self.view.failed = report.failed;
        self.view.duplicates = report.duplicates;
        self.view.rows = report.rows;
        self.view.summary = report.summary;
    }

    /// 整批失败（配置非法/超上限等）。
    pub(crate) fn fail(&mut self, message: String) {
        self.view.running = false;
        self.view.summary = message.clone();
        self.view.rows.push(ImportRowVm {
            name: String::new(),
            status: ImportRowStatus::Failed,
            detail: message,
        });
    }

    /// 生成页面动作（按钮 id 由 `state` 转发）。
    pub(crate) fn actions(&self) -> Vec<ActionVm> {
        let strings = strings();
        let mut actions = vec![
            ActionVm {
                id: "import.pick".to_owned(),
                label: strings.action_pick_files.to_owned(),
                enabled: !self.view.running,
            },
            ActionVm {
                id: "import.pick_folder".to_owned(),
                label: strings.action_pick_folder.to_owned(),
                enabled: !self.view.running,
            },
        ];
        if !self.view.rows.is_empty() {
            actions.push(ActionVm {
                id: "import.open_progress".to_owned(),
                label: strings.action_open_progress.to_owned(),
                enabled: true,
            });
        }
        actions
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn paths(count: usize) -> Vec<PathBuf> {
        (0..count)
            .map(|i| PathBuf::from(format!("张三-{i}.jpg")))
            .collect()
    }

    #[test]
    fn naming_reminder_is_shown_only_once_per_session() {
        let mut page = ImportPage::default();
        assert!(page.pick(paths(1), true).is_some());
        assert!(page.view().naming_visible);
        page.dismiss_naming();
        assert!(page.pick(paths(2), true).is_some());
        assert!(!page.view().naming_visible, "第二次选择文件不得再提醒");
        assert_eq!(page.view().total, 2);
        assert!(!page.view().running.eq(&false), "运行中标记应保持");
    }

    #[test]
    fn hint_reflects_disabled_reminder() {
        let mut page = ImportPage::default();
        page.pick(paths(1), false);
        assert!(!page.view().naming_visible);
    }

    #[test]
    fn partial_success_is_visible() {
        let mut page = ImportPage::default();
        page.pick(paths(3), false);
        page.progress(2, 3);
        page.finish(ImportReportVm {
            ok: 1,
            rejected: 1,
            failed: 1,
            duplicates: 0,
            rows: vec![
                ImportRowVm {
                    name: "a.jpg".to_owned(),
                    status: ImportRowStatus::Ok,
                    detail: "2 页".to_owned(),
                },
                ImportRowVm {
                    name: "b.jpg".to_owned(),
                    status: ImportRowStatus::Rejected,
                    detail: "模糊".to_owned(),
                },
                ImportRowVm {
                    name: "c.jpg".to_owned(),
                    status: ImportRowStatus::Failed,
                    detail: "无法解码".to_owned(),
                },
            ],
            summary: "成功 1 / 拒收 1 / 失败 1".to_owned(),
        });
        let view = page.view();
        assert!(!view.running);
        assert_eq!((view.ok, view.rejected, view.failed), (1, 1, 1));
        assert_eq!(view.rows.len(), 3);
        assert_eq!(view.rows[1].status.code(), "rejected");
    }

    #[test]
    fn empty_selection_reports_without_starting() {
        let mut page = ImportPage::default();
        assert!(page.pick(Vec::new(), true).is_none());
        assert!(!page.view().running);
    }
}
