//! 页面视图模型（T36–T38）：纯逻辑、无 Slint/无 IO，便于单测。
//!
//! 每个页面一个子模块；`state` 聚合投影为 Slint 属性，`worker` 负责把命令映射到
//! 下游服务（capture / rubric / review / platform）。页面只做展示与命令转发。

pub(crate) mod consent;
pub(crate) mod diagnostics;
pub(crate) mod import;
pub(crate) mod progress;
pub(crate) mod review;
pub(crate) mod rules;
pub(crate) mod settings;

/// 页面动作按钮（id 稳定，UI 回传后由 `state`/`worker` 解析）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActionVm {
    /// 稳定动作 id（如 `import.pick`）。
    pub(crate) id: String,
    /// 按钮文字（已本地化）。
    pub(crate) label: String,
    /// 是否可用。
    pub(crate) enabled: bool,
}
