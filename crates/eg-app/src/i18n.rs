//! 界面文案（i18n 结构，中文默认；`SPEC-app.md` §2 / §8）。
//!
//! - v1 仅 `zh-CN`（`general.language` 唯一取值），但所有用户可见文案必须经本模块取词，
//!   `.slint` 文件里 **0 中文字面量**（门禁脚本/单测扫描 `ui/` 保证）；
//! - 文案只放静态词条与少量模板拼装函数，不做业务判断；
//! - 结构体字段在编译期强制齐全，新增页面/文案漏写会编译失败。

use crate::nav::PageId;

/// 单页文案（标题 / 副标题 / 占位正文 / 主操作按钮文字）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PageCopy {
    /// 页面标题（导航与页头）。
    pub(crate) title: &'static str,
    /// 页面副标题（页头说明）。
    pub(crate) subtitle: &'static str,
    /// 占位正文（说明该页将接入什么；M9 后续任务替换为真实内容）。
    pub(crate) body: &'static str,
    /// 页内主操作按钮文字（占位动作）。
    pub(crate) action: &'static str,
}

/// 中文（简体）词条。
#[derive(Debug, Clone, Copy)]
pub(crate) struct Strings {
    /// 应用标题（窗口标题栏）。
    pub(crate) app_title: &'static str,
    /// 十页文案（顺序与 [`PageId::ALL`] 一致）。
    pub(crate) pages: [PageCopy; 10],
    /// 标准模式徽标。
    pub(crate) mode_standard: &'static str,
    /// 快速模式徽标。
    pub(crate) mode_fast: &'static str,
    /// 顶栏模式切换按钮（标准→快速）。
    pub(crate) mode_switch_hint: &'static str,
    /// 顶栏模式切换按钮（快速→标准）。
    pub(crate) mode_switch_back_hint: &'static str,
    /// 二次确认：进入快速模式标题。
    pub(crate) confirm_enter_fast_title: &'static str,
    /// 二次确认：进入快速模式说明。
    pub(crate) confirm_enter_fast_body: &'static str,
    /// 二次确认：退出快速模式标题。
    pub(crate) confirm_exit_fast_title: &'static str,
    /// 二次确认：退出快速模式说明。
    pub(crate) confirm_exit_fast_body: &'static str,
    /// 二次确认：离开未保存编辑页标题。
    pub(crate) confirm_leave_title: &'static str,
    /// 二次确认：离开未保存编辑页说明。
    pub(crate) confirm_leave_body: &'static str,
    /// 确认按钮。
    pub(crate) confirm_yes: &'static str,
    /// 取消按钮。
    pub(crate) confirm_no: &'static str,
    /// 后端未接入（占位）。
    pub(crate) backend_unknown: &'static str,
    /// 后端就绪。
    pub(crate) backend_ready: &'static str,
    /// 后端不可用。
    pub(crate) backend_down: &'static str,
    /// 后端说明（占位）。
    pub(crate) backend_note: &'static str,
    /// 错误横幅标题。
    pub(crate) error_title: &'static str,
    /// 展开详情。
    pub(crate) error_show_detail: &'static str,
    /// 收起详情。
    pub(crate) error_hide_detail: &'static str,
    /// 重试按钮。
    pub(crate) error_retry: &'static str,
    /// 复制诊断 id。
    pub(crate) error_copy_id: &'static str,
    /// 诊断 id 前缀。
    pub(crate) diagnostic_id_label: &'static str,
    /// 设置按钮。
    pub(crate) open_settings: &'static str,
    /// 日志与诊断按钮。
    pub(crate) open_logs: &'static str,
    /// 关于区标题（设置页占位）。
    pub(crate) about_title: &'static str,
    /// 关于区正文（设置页占位）。
    pub(crate) about_body: &'static str,
    /// 通道满/操作过密提示。
    pub(crate) channel_full: &'static str,
    /// 模式切换被拒（有运行中任务）。
    pub(crate) mode_blocked_running: &'static str,
    /// 底部快捷键提示。
    pub(crate) shortcut_hint: &'static str,
    /// 日志页占位（查看器入口）。
    pub(crate) log_viewer_placeholder: &'static str,
    /// 状态栏就绪文案。
    pub(crate) status_ready: &'static str,
    /// 提示气泡关闭符号。
    pub(crate) dismiss_symbol: &'static str,
    /// 通知标题：操作被拒（通道满）。
    pub(crate) notice_busy_title: &'static str,
    /// 通知标题：模式切换。
    pub(crate) notice_mode_title: &'static str,
    /// 通知标题：后台任务。
    pub(crate) notice_worker_title: &'static str,
    /// 通知标题：占位功能。
    pub(crate) notice_placeholder_title: &'static str,
    /// 通知标题：一般提示。
    pub(crate) notice_info_title: &'static str,
    /// 启动诊断标题。
    pub(crate) startup_error_title: &'static str,
    /// 启动失败引导（打开数据目录/日志，退出）。
    pub(crate) startup_error_help: &'static str,
    /// 占位任务标签。
    pub(crate) worker_label: &'static str,
    /// 占位任务完成正文。
    pub(crate) worker_done_body: &'static str,
    /// 重试占位提示。
    pub(crate) retry_placeholder: &'static str,
    /// 退出按钮。
    pub(crate) quit_label: &'static str,
    /// 导入文件占位。
    pub(crate) import_files_placeholder: &'static str,
    /// 导入文件夹占位。
    pub(crate) import_folder_placeholder: &'static str,
    /// 复制诊断 id 占位。
    pub(crate) copy_placeholder: &'static str,
    /// 导入页：拖拽/选择提示。
    pub(crate) import_hint: &'static str,
    /// 导入页：未选择文件。
    pub(crate) import_no_files: &'static str,
    /// 导入页：命名提醒正文（仅一次）。
    pub(crate) import_naming: &'static str,
    /// 导入页：选择文件按钮。
    pub(crate) action_pick_files: &'static str,
    /// 导入页：选择文件夹按钮。
    pub(crate) action_pick_folder: &'static str,
    /// 导入页：打开进度页。
    pub(crate) action_open_progress: &'static str,
    /// 进度页：运行中。
    pub(crate) progress_running: &'static str,
    /// 进度页：待运行。
    pub(crate) progress_pending: &'static str,
    /// 进度页：失败。
    pub(crate) progress_failed: &'static str,
    /// 通用：刷新。
    pub(crate) action_refresh: &'static str,
    /// 进度页：取消任务。
    pub(crate) action_cancel_tasks: &'static str,
    /// 设置页：只读前缀。
    pub(crate) settings_read_only: &'static str,
    /// 设置页：未知键。
    pub(crate) settings_unknown_key: &'static str,
    /// 设置页：空值。
    pub(crate) settings_empty_value: &'static str,
    /// 设置页：非法值。
    pub(crate) settings_invalid_value: &'static str,
    /// 设置页：不支持的类型。
    pub(crate) settings_unsupported_kind: &'static str,
    /// 设置页：恢复默认/备份。
    pub(crate) action_restore_default: &'static str,
    /// 通用：打开数据目录。
    pub(crate) action_open_data_dir: &'static str,
    /// 诊断页：级别标签。
    pub(crate) diagnostics_level: &'static str,
    /// 诊断页：模块标签。
    pub(crate) diagnostics_module: &'static str,
    /// 诊断页：关键词标签。
    pub(crate) diagnostics_keyword: &'static str,
    /// 诊断页：全部。
    pub(crate) diagnostics_all: &'static str,
    /// 诊断页：无。
    pub(crate) diagnostics_none: &'static str,
    /// 诊断页：索引可用。
    pub(crate) diagnostics_index_ok: &'static str,
    /// 诊断页：检索按钮。
    pub(crate) action_search: &'static str,
    /// 诊断页：导出诊断包。
    pub(crate) action_export_diagnostics: &'static str,
    /// 复核页：队列总数。
    pub(crate) review_queue_total: &'static str,
    /// 复核页：批量。
    pub(crate) review_batch: &'static str,
    /// 复核页：导出后已修改提示。
    pub(crate) review_export_stale: &'static str,
    /// 复核页：抽检弹窗取消提示。
    pub(crate) review_sampling_cancelled: &'static str,
    /// 复核页：接受。
    pub(crate) action_accept: &'static str,
    /// 复核页：改分。
    pub(crate) action_override_score: &'static str,
    /// 复核页：修文本重判。
    pub(crate) action_regrade_text: &'static str,
    /// 复核页：批量接受。
    pub(crate) action_batch_accept: &'static str,
    /// 复核页：撤销。
    pub(crate) action_undo: &'static str,
    /// 复核页：抽检通过。
    pub(crate) action_sampling_pass: &'static str,
    /// 规则页：终态不可编辑。
    pub(crate) rules_terminal: &'static str,
    /// 规则页：需要原文。
    pub(crate) rules_need_raw_text: &'static str,
    /// 规则页：整理中。
    pub(crate) rules_busy: &'static str,
    /// 规则页：当前不是草稿。
    pub(crate) rules_not_draft: &'static str,
    /// 规则页：存在错误级问题。
    pub(crate) rules_has_errors: &'static str,
    /// 规则页：已确认。
    pub(crate) rules_confirmed: &'static str,
    /// 规则页：已拒绝。
    pub(crate) rules_rejected: &'static str,
    /// 规则页：整理按钮。
    pub(crate) action_structure: &'static str,
    /// 规则页：确认按钮。
    pub(crate) action_confirm_rubric: &'static str,
    /// 规则页：拒绝按钮。
    pub(crate) action_reject: &'static str,
    /// 规则页：重置按钮。
    pub(crate) action_reset: &'static str,
    /// 授权页：需要任务。
    pub(crate) consent_need_task: &'static str,
    /// 授权页：需要范围。
    pub(crate) consent_need_scope: &'static str,
    /// 授权页：预算非法。
    pub(crate) consent_bad_budget: &'static str,
    /// 授权页：TTL 非法。
    pub(crate) consent_bad_ttl: &'static str,
    /// 授权页：已登记。
    pub(crate) consent_registered: &'static str,
    /// 授权页：已撤销。
    pub(crate) consent_revoked: &'static str,
    /// 授权页：空态。
    pub(crate) consent_empty: &'static str,
    /// 授权页：额度用尽提示。
    pub(crate) consent_exhausted_hint: &'static str,
    /// 授权页：新建授权。
    pub(crate) action_register_consent: &'static str,
}

/// 中文（简体）默认词条。
pub(crate) static ZH_CN: Strings = Strings {
    app_title: "考试作业批改",
    pages: [
        PageCopy {
            title: "首页",
            subtitle: "待复核数、待处理导入、最近试卷、模式与后端状态",
            body: "占位：首页快照（待复核数 / 待处理导入 / 最近试卷 / 继续按钮）将在 M9 后续任务接入。\n当前外壳已具备：十页导航、全局状态、有界命令/事件通道、错误呈现与日志入口。",
            action: "继续上次工作",
        },
        PageCopy {
            title: "试卷与答案键",
            subtitle: "试卷列表、逐题编辑、答案键版本与校验问题",
            body: "占位：试卷列表、逐题答案键编辑、答案键版本与校验问题将在 T36/T38 接入。",
            action: "新建试卷",
        },
        PageCopy {
            title: "评分规则",
            subtitle: "规则原文 → 模型整理 → 草稿校验 → 教师确认",
            body: "占位：规则原文编辑、模型整理结果、草稿列表与状态、校验问题、确认/拒绝将在 T38 接入。",
            action: "整理规则",
        },
        PageCopy {
            title: "导入",
            subtitle: "拖拽/选择文件、命名提醒、文件名身份预览、名单 CSV、质量门禁",
            body: "占位：拖拽/选择导入、命名提醒（import.remind_naming）、文件名身份预览、名单 CSV、质量门禁与未匹配项将在 T36 接入。",
            action: "选择文件",
        },
        PageCopy {
            title: "进度",
            subtitle: "任务队列、阶段、耗时分布、失败项与重试、取消",
            body: "占位：任务队列视图、阶段进度、失败项重试/取消将在 T36 接入；当前可看到有界事件通道的进度事件。",
            action: "刷新队列",
        },
        PageCopy {
            title: "复核",
            subtitle: "题卡、原图裁剪、识别文本、规则条目、证据、改分/批注",
            body: "占位：三栏复核工作台、键盘复核、抽检弹窗将在 T37 接入。",
            action: "开始复核",
        },
        PageCopy {
            title: "报表",
            subtitle: "题目统计、错题标签、模式标注、导出",
            body: "占位：题目统计、错题标签、模式标注与 XLSX/CSV/HTML 导出将在 T38 接入。",
            action: "导出报表",
        },
        PageCopy {
            title: "云端授权",
            subtitle: "按任务授权、新建/撤销、范围与预算、已用额度",
            body: "占位：任务级云端授权列表、新建与撤销、范围/预算/已用额度将在 T38 接入。",
            action: "新建授权",
        },
        PageCopy {
            title: "设置",
            subtitle: "模型端点、模式阈值、隐私与云端总开关、存储、备份恢复、密钥状态",
            body: "占位：九个 CONFIG 分组（通用/导入/识别/判分/抽检/规则/CAS/日志/隐私）与备份恢复、密钥状态将在 T36 接入。\n关于：exam-grading 本地单机版 · GPL-3.0-only · 无账号、无登录、无打包分发。",
            action: "保存设置",
        },
        PageCopy {
            title: "日志与诊断",
            subtitle: "日志分页与过滤、复制诊断 id、导出诊断包、打开日志目录",
            body: "占位：日志查看器（级别/模块/trace_id/任务 id/时间过滤、关键词检索）与诊断包导出将在 T38 接入；当前提供日志目录与诊断入口占位。",
            action: "导出诊断包",
        },
    ],
    mode_standard: "标准模式",
    mode_fast: "快速模式",
    mode_switch_hint: "切换到快速模式",
    mode_switch_back_hint: "切回标准模式",
    confirm_enter_fast_title: "进入快速模式？",
    confirm_enter_fast_body: "快速模式会自动放行更多结果、判分记录带「快速」标记，并按比例抽检。确认后写入配置与审计；运行中的任务禁止切换。",
    confirm_exit_fast_title: "退出快速模式？",
    confirm_exit_fast_body: "退出后仅影响之后的新任务；历史判分的模式标记不会追溯修改。确认后写入配置与审计。",
    confirm_leave_title: "离开当前页？",
    confirm_leave_body: "该页有未保存的编辑。离开会丢失未保存内容，请先保存或确认放弃。",
    confirm_yes: "确认",
    confirm_no: "取消",
    backend_unknown: "本地后端未接入",
    backend_ready: "本地后端就绪",
    backend_down: "本地后端不可用",
    backend_note: "健康检查占位：推理模块接入后由组合根探测",
    error_title: "出现问题",
    error_show_detail: "显示详情",
    error_hide_detail: "收起详情",
    error_retry: "重试",
    error_copy_id: "复制诊断 id",
    diagnostic_id_label: "诊断 id",
    open_settings: "设置",
    open_logs: "日志与诊断",
    about_title: "关于",
    about_body: "exam-grading · 本地单机版 · GPL-3.0-only · 无账号、无登录、无打包分发。",
    channel_full: "操作过密，请稍候再试",
    mode_blocked_running: "有运行中的任务，等当前批任务结束再切换模式",
    shortcut_hint: "Cmd+1…0 切页 · Cmd+, 设置 · Cmd+L 日志与诊断 · Cmd+Shift+M 切换模式 · Esc 关闭",
    log_viewer_placeholder: "日志查看器占位：日志目录与诊断包导出在 T38 接入",
    status_ready: "就绪",
    dismiss_symbol: "×",
    notice_busy_title: "操作被拒绝",
    notice_mode_title: "模式切换",
    notice_worker_title: "后台任务",
    notice_placeholder_title: "占位功能",
    notice_info_title: "提示",
    startup_error_title: "启动失败",
    startup_error_help: "请打开日志目录查看详情，或从备份恢复后重试；界面不会白屏。",
    worker_label: "占位后台任务",
    worker_done_body: "占位后台任务已完成（10/10）；真实导入/判分任务在后续任务接入",
    retry_placeholder: "重试动作占位：后续任务接入真实重试",
    quit_label: "退出",
    import_files_placeholder: "导入文件占位：T36 接入文件选择与按文件名识别学生",
    import_folder_placeholder: "导入文件夹占位：T36 接入文件夹选择与命名提醒",
    copy_placeholder: "复制动作占位（T38 接入剪贴板）；诊断 id 见错误横幅",
    import_hint: "拖拽文件到此处，或点击按钮选择文件/文件夹；文件名即学生身份",
    import_no_files: "未选择文件：请拖入或选择至少一个文件",
    import_naming: "命名提醒：请按「学号-姓名-页码」命名文件名（文件名即身份来源；仅提醒一次）",
    action_pick_files: "选择文件",
    action_pick_folder: "选择文件夹",
    action_open_progress: "打开进度页",
    progress_running: "运行中",
    progress_pending: "待运行",
    progress_failed: "失败",
    action_refresh: "刷新",
    action_cancel_tasks: "取消未完成任务",
    settings_read_only: "不可配置",
    settings_unknown_key: "未知配置键",
    settings_empty_value: "值不能为空",
    settings_invalid_value: "值类型不合法",
    settings_unsupported_kind: "该类型暂不支持在界面编辑",
    action_restore_default: "恢复上一版配置",
    action_open_data_dir: "打开数据目录",
    diagnostics_level: "级别",
    diagnostics_module: "模块",
    diagnostics_keyword: "关键词",
    diagnostics_all: "全部",
    diagnostics_none: "无",
    diagnostics_index_ok: "全文索引可用（内容已脱敏）",
    action_search: "检索",
    action_export_diagnostics: "导出诊断包",
    review_queue_total: "队列",
    review_batch: "批量",
    review_export_stale: "导出后已有新的复核修改，请重新导出报表",
    review_sampling_cancelled: "抽检弹窗已取消（未选择）：保持待复核，未自动重跑",
    action_accept: "接受",
    action_override_score: "改分",
    action_regrade_text: "修文本重判",
    action_batch_accept: "批量接受",
    action_undo: "撤销",
    action_sampling_pass: "抽检通过",
    rules_terminal: "已确认/已拒绝的规则为终态，不可再编辑；如需修改请新建草稿",
    rules_need_raw_text: "请先填写规则原文",
    rules_busy: "模型整理进行中，请稍候",
    rules_not_draft: "当前没有待确认草稿",
    rules_has_errors: "校验存在错误级问题，确认已被禁用",
    rules_confirmed: "规则已确认并生成新版本",
    rules_rejected: "规则已拒绝（可留原因备查）",
    action_structure: "模型整理",
    action_confirm_rubric: "确认规则（Cmd+Enter）",
    action_reject: "拒绝",
    action_reset: "重新开始",
    consent_need_task: "请选择任务",
    consent_need_scope: "请填写允许外发的数据范围",
    consent_bad_budget: "预算必须为正整数（微货币单位）",
    consent_bad_ttl: "有效期必须为 1–168 小时",
    consent_registered: "授权已登记：",
    consent_revoked: "授权已撤销：",
    consent_empty: "当前没有授权记录；云端调用必须按任务授权",
    consent_exhausted_hint: "存在额度用尽的授权：该任务重启后仍需重新授权，判分已暂停",
    action_register_consent: "新建授权",
};

/// 取当前语言词条（v1 仅中文；`general.language` 为 `zh-CN`）。
#[must_use]
pub(crate) fn strings() -> &'static Strings {
    &ZH_CN
}

impl Strings {
    /// 设置分组标题（id 见 `pages::settings` 的 12 组）。
    #[must_use]
    pub(crate) fn settings_group(&self, id: &str) -> &'static str {
        match id {
            "general" => "通用与界面",
            "import" => "导入、身份与对齐",
            "ocr" => "识别",
            "grade" => "判分",
            "fast" => "快速模式与抽检",
            "rubric" => "评分规则与模板",
            "cas" => "符号计算（CAS）",
            "log" => "日志、审计与诊断",
            "privacy" => "隐私与云端授权",
            "storage" => "模型端点与存储",
            "docs" => "文档与开发",
            _ => "系统元数据（只读）",
        }
    }

    /// 规则页阶段标签（阶段机器码见 `pages::rules::RulesStage::code`）。
    #[must_use]
    pub(crate) fn rules_stage(&self, code: &str) -> &'static str {
        match code {
            "empty" => "未开始",
            "editing" => "编辑原文",
            "structuring" => "整理中",
            "draft" => "草稿待确认",
            "confirmed" => "已确认",
            "rejected" => "已拒绝",
            _ => "整理失败（可重试）",
        }
    }
}

impl Strings {
    /// 取某页文案。
    #[must_use]
    pub(crate) fn page(&self, page: PageId) -> &PageCopy {
        &self.pages[page.index()]
    }

    /// 队列状态行。
    #[must_use]
    pub(crate) fn queue_line(running: u32, pending: u32) -> String {
        format!("任务队列：运行中 {running} · 待运行 {pending}")
    }

    /// 通道状态行（合并/丢弃计数显式可见，不静默）。
    #[must_use]
    pub(crate) fn channel_line(
        merged: u64,
        dropped: u64,
        critical_dropped: u64,
        rejected: u64,
    ) -> String {
        format!(
            "事件合并 {merged} · 丢弃 {dropped} · 错误事件丢弃 {critical_dropped} · 命令拒绝 {rejected}"
        )
    }

    /// 模式徽标文案。
    #[must_use]
    pub(crate) fn mode_label(&self, fast: bool) -> &'static str {
        if fast {
            self.mode_fast
        } else {
            self.mode_standard
        }
    }

    /// 模式切换按钮文案。
    #[must_use]
    pub(crate) fn mode_switch_label(&self, fast: bool) -> &'static str {
        if fast {
            self.mode_switch_back_hint
        } else {
            self.mode_switch_hint
        }
    }

    /// 模式确认弹窗文案（标题, 正文）。
    #[must_use]
    pub(crate) fn mode_confirm(&self, to_fast: bool) -> (&'static str, &'static str) {
        if to_fast {
            (self.confirm_enter_fast_title, self.confirm_enter_fast_body)
        } else {
            (self.confirm_exit_fast_title, self.confirm_exit_fast_body)
        }
    }

    /// 占位动作提示（页内主操作按钮点击后的提示文案）。
    #[must_use]
    pub(crate) fn placeholder_action_note(&self, page: PageId) -> String {
        format!(
            "「{}」占位动作：{} 页在 M9 后续任务接入后生效",
            self.page(page).action,
            self.page(page).title
        )
    }

    /// 诊断包占位提示。
    #[must_use]
    pub(crate) fn diagnostics_placeholder() -> String {
        "诊断包导出占位：T38 接入 platform 诊断包（不含 PII/密钥）".to_owned()
    }
}

/// 页面交互控件标签（Slint `PageLabels` 的取值来源）。
pub(crate) struct PageLabels {
    pub(crate) save_setting: &'static str,
    pub(crate) setting_key_hint: &'static str,
    pub(crate) setting_value_hint: &'static str,
    pub(crate) review_score: &'static str,
    pub(crate) image_evidence: &'static str,
    pub(crate) sampling_title: &'static str,
    pub(crate) sampling_single: &'static str,
    pub(crate) sampling_whole: &'static str,
    pub(crate) rule_criterion_hint: &'static str,
    pub(crate) rule_points_hint: &'static str,
    pub(crate) rule_handwritten: &'static str,
    pub(crate) consent_task_hint: &'static str,
    pub(crate) consent_backend_hint: &'static str,
    pub(crate) consent_scope_hint: &'static str,
    pub(crate) consent_budget_hint: &'static str,
    pub(crate) consent_ttl_hint: &'static str,
    pub(crate) consent_register: &'static str,
}

/// 交互控件标签（中文默认；`.slint` 不写中文字面量）。
#[must_use]
pub(crate) fn page_labels() -> PageLabels {
    PageLabels {
        save_setting: "保存设置",
        setting_key_hint: "例：grade.score_granularity",
        setting_value_hint: "新值",
        review_score: "改分（千分之一分）",
        image_evidence: "图像证据按资产 id 懒加载（平台资产接口就绪后接入）",
        sampling_title: "抽检不通过：选择重跑范围",
        sampling_single: "仅本题（默认）",
        sampling_whole: "整卷",
        rule_criterion_hint: "评分项判据",
        rule_points_hint: "分值（如 5）",
        rule_handwritten: "手写直译草稿",
        consent_task_hint: "任务 id",
        consent_backend_hint: "后端",
        consent_scope_hint: "范围说明",
        consent_budget_hint: "预算",
        consent_ttl_hint: "TTL 小时",
        consent_register: "登记授权",
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn every_page_has_complete_copy() {
        for page in PageId::ALL {
            let copy = ZH_CN.page(page);
            assert!(!copy.title.trim().is_empty(), "{} 缺标题", page.code());
            assert!(!copy.subtitle.trim().is_empty(), "{} 缺副标题", page.code());
            assert!(!copy.body.trim().is_empty(), "{} 缺正文", page.code());
            assert!(!copy.action.trim().is_empty(), "{} 缺主操作", page.code());
            assert!(
                copy.body.contains("占位"),
                "{} 正文应显式标注占位，避免冒充已实现",
                page.code()
            );
        }
    }

    #[test]
    fn page_titles_are_distinct_and_chinese() {
        let mut titles: Vec<&str> = PageId::ALL.iter().map(|p| ZH_CN.page(*p).title).collect();
        let count = titles.len();
        titles.sort_unstable();
        titles.dedup();
        assert_eq!(titles.len(), count, "十页标题不应重复");
        for page in PageId::ALL {
            let title = ZH_CN.page(page).title;
            assert!(
                title
                    .chars()
                    .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
                "标题应为中文：{title}"
            );
        }
    }

    #[test]
    fn mode_and_channel_copy_are_explicit() {
        assert_eq!(ZH_CN.mode_label(false), "标准模式");
        assert_eq!(ZH_CN.mode_label(true), "快速模式");
        assert!(ZH_CN.mode_switch_label(false).contains("快速"));
        assert!(ZH_CN.mode_switch_label(true).contains("标准"));
        let line = Strings::channel_line(3, 1, 0, 2);
        assert!(line.contains('3') && line.contains('1') && line.contains('2'));
        assert!(Strings::queue_line(2, 5).contains('2'));
    }

    #[test]
    fn strings_are_static_and_complete() {
        let strings = strings();
        assert_eq!(strings.app_title, ZH_CN.app_title);
        assert!(strings.error_title.contains("问题"));
        assert!(strings.confirm_yes.contains("确认"));
        assert!(strings.confirm_no.contains("取消"));
    }
}
