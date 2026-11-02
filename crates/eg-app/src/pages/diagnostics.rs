//! 日志与诊断页视图模型（T38）：字段过滤、分页、关键词检索降级、诊断包导出。

use crate::i18n::strings;
use crate::pages::ActionVm;

/// 日志行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LogRowVm {
    /// 时间（RFC3339）。
    pub(crate) at: String,
    /// 级别（机器码）。
    pub(crate) level: String,
    /// 模块。
    pub(crate) module: String,
    /// 消息。
    pub(crate) message: String,
    /// 链路 id（可复制）。
    pub(crate) trace_id: String,
}

/// 日志过滤条件（UI 状态）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LogFilter {
    /// 最低级别（空 = 全部）。
    pub(crate) level: String,
    /// 模块（空 = 全部）。
    pub(crate) module: String,
    /// 关键词（空 = 字段过滤）。
    pub(crate) keyword: String,
    /// 链路 id。
    pub(crate) trace_id: String,
}

/// 诊断页投影。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DiagnosticsVm {
    /// 是否当前页。
    pub(crate) visible: bool,
    /// 行。
    pub(crate) rows: Vec<LogRowVm>,
    /// 当前页（1 起）。
    pub(crate) page: u32,
    /// 总页数。
    pub(crate) pages: u32,
    /// 命中总数。
    pub(crate) total: u32,
    /// 过滤条件回显。
    pub(crate) filter_text: String,
    /// 索引状态/降级提示。
    pub(crate) index_note: String,
    /// 诊断包导出结果（成功/失败均写入）。
    pub(crate) export_summary: String,
    /// 错误（读取失败不阻塞业务页）。
    pub(crate) error: String,
    /// 动作。
    pub(crate) actions: Vec<ActionVm>,
}

/// 诊断页状态。
#[derive(Debug, Clone)]
pub(crate) struct DiagnosticsPage {
    filter: LogFilter,
    page: u32,
    page_size: u32,
    rows: Vec<LogRowVm>,
    total: u32,
    index_note: String,
    export_summary: String,
    error: String,
}

impl Default for DiagnosticsPage {
    fn default() -> Self {
        Self {
            filter: LogFilter::default(),
            page: 1,
            page_size: 50,
            rows: Vec::new(),
            total: 0,
            index_note: String::new(),
            export_summary: String::new(),
            error: String::new(),
        }
    }
}

impl DiagnosticsPage {
    /// 当前过滤条件。
    pub(crate) fn filter(&self) -> &LogFilter {
        &self.filter
    }

    /// 当前页（1 起）。
    pub(crate) fn page(&self) -> u32 {
        self.page
    }

    /// 每页条数。
    pub(crate) fn page_size(&self) -> u32 {
        self.page_size
    }

    /// 更新过滤条件并回到第 1 页。
    pub(crate) fn set_filter(&mut self, filter: LogFilter) {
        self.filter = filter;
        self.page = 1;
        self.error.clear();
    }

    /// 应用一页结果（`worker` 查询后回投）。
    pub(crate) fn apply_page(&mut self, rows: Vec<LogRowVm>, total: u32, index_note: String) {
        self.rows = rows;
        self.total = total;
        self.index_note = index_note;
        self.error.clear();
    }

    /// 日志读取失败：内联错误 + 提示打开日志目录（不影响业务页）。
    pub(crate) fn fail(&mut self, message: String) {
        self.error = message;
        self.rows.clear();
    }

    /// 翻页（边界钳制；调用方据 `page()` 重新查询）。
    pub(crate) fn next_page(&mut self) {
        let pages = self.pages();
        if self.page < pages {
            self.page += 1;
        }
    }

    /// 上一页。
    pub(crate) fn prev_page(&mut self) {
        if self.page > 1 {
            self.page -= 1;
        }
    }

    /// 诊断包导出结果。
    pub(crate) fn set_export(&mut self, summary: String) {
        self.export_summary = summary;
    }

    /// 总页数（至少 1）。
    pub(crate) fn pages(&self) -> u32 {
        if self.total == 0 {
            return 1;
        }
        self.total.div_ceil(self.page_size.max(1))
    }

    /// 投影。
    pub(crate) fn view(&self) -> DiagnosticsVm {
        let strings = strings();
        let filter_text = format!(
            "{}={} / {}={} / {}={}",
            strings.diagnostics_level,
            if self.filter.level.is_empty() {
                strings.diagnostics_all
            } else {
                &self.filter.level
            },
            strings.diagnostics_module,
            if self.filter.module.is_empty() {
                strings.diagnostics_all
            } else {
                &self.filter.module
            },
            strings.diagnostics_keyword,
            if self.filter.keyword.is_empty() {
                strings.diagnostics_none
            } else {
                &self.filter.keyword
            }
        );
        let index_note = if self.index_note.is_empty() {
            strings.diagnostics_index_ok.to_owned()
        } else {
            self.index_note.clone()
        };
        DiagnosticsVm {
            visible: false,
            rows: self.rows.clone(),
            page: self.page,
            pages: self.pages(),
            total: self.total,
            filter_text,
            index_note,
            export_summary: self.export_summary.clone(),
            error: self.error.clone(),
            actions: vec![
                ActionVm {
                    id: "diagnostics.search".to_owned(),
                    label: strings.action_search.to_owned(),
                    enabled: true,
                },
                ActionVm {
                    id: "diagnostics.export".to_owned(),
                    label: strings.action_export_diagnostics.to_owned(),
                    enabled: true,
                },
                ActionVm {
                    id: "diagnostics.open_dir".to_owned(),
                    label: strings.action_open_data_dir.to_owned(),
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

    fn row(index: u32) -> LogRowVm {
        LogRowVm {
            at: "2026-11-02T12:00:00Z".to_owned(),
            level: "info".to_owned(),
            module: "eg_app".to_owned(),
            message: format!("第 {index} 条"),
            trace_id: format!("trace-{index}"),
        }
    }

    #[test]
    fn pagination_clamps_and_reports_pages() {
        let mut page = DiagnosticsPage::default();
        page.apply_page(vec![row(1)], 201, String::new());
        assert_eq!(page.pages(), 5);
        page.next_page();
        assert_eq!(page.page(), 2);
        page.next_page();
        page.next_page();
        page.next_page();
        assert_eq!(page.page(), 5);
        page.next_page();
        assert_eq!(page.page(), 5, "不得越过末页");
        page.prev_page();
        assert_eq!(page.page(), 4);
        let view = page.view();
        assert_eq!(view.total, 201);
        assert!(
            view.actions
                .iter()
                .any(|action| action.id == "diagnostics.export")
        );
    }

    #[test]
    fn filter_change_resets_to_first_page() {
        let mut page = DiagnosticsPage::default();
        page.apply_page(vec![row(1)], 100, String::new());
        page.next_page();
        page.set_filter(LogFilter {
            level: "warn".to_owned(),
            keyword: "识别失败".to_owned(),
            ..LogFilter::default()
        });
        assert_eq!(page.page(), 1);
        let view = page.view();
        assert!(view.filter_text.contains("warn"));
        assert!(view.filter_text.contains("识别失败"));
    }

    #[test]
    fn degraded_index_and_export_errors_are_explicit() {
        let mut page = DiagnosticsPage::default();
        page.apply_page(vec![row(1)], 2, "索引重建中：已退化为字段过滤".to_owned());
        page.fail("日志读取失败：权限不足".to_owned());
        page.set_export("诊断包已导出到 /tmp/diag.zip".to_owned());
        let view = page.view();
        assert!(view.error.contains("权限不足"));
        assert!(view.export_summary.contains("diag.zip"));
        assert!(
            view.actions
                .iter()
                .find(|a| a.id == "diagnostics.export")
                .unwrap()
                .enabled
        );
        // 无结果时总页数至少为 1。
        let page = DiagnosticsPage::default();
        assert_eq!(page.pages(), 1);
    }
}
