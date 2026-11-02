//! 十页导航与全局快捷键解析（`SPEC-app.md` §2 / §2.1）。
//!
//! 纯逻辑、零 IO、零 Slint 依赖：Slint 侧只把按键文本与修饰键转发进来，由本模块决定
//! 「这是哪个动作」；导航本身只改内存状态，绝不在切换路径做同步 IO（§2 导航规则）。

/// 十页导航（编号即 `Cmd+数字` 顺序；`SPEC-app.md` §2 表格顺序）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum PageId {
    /// 首页。
    Home,
    /// 试卷与答案键。
    Papers,
    /// 评分规则。
    Rules,
    /// 导入。
    Import,
    /// 进度。
    Progress,
    /// 复核。
    Review,
    /// 报表。
    Report,
    /// 云端授权。
    Consent,
    /// 设置。
    Settings,
    /// 日志与诊断。
    Diagnostics,
}

impl PageId {
    /// 十页顺序（与 `Cmd+1`…`Cmd+0` 一一对应）。
    pub(crate) const ALL: [Self; 10] = [
        Self::Home,
        Self::Papers,
        Self::Rules,
        Self::Import,
        Self::Progress,
        Self::Review,
        Self::Report,
        Self::Consent,
        Self::Settings,
        Self::Diagnostics,
    ];

    /// 稳定机器码（日志 `page` 字段、状态快照键）。
    #[must_use]
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::Papers => "papers",
            Self::Rules => "rules",
            Self::Import => "import",
            Self::Progress => "progress",
            Self::Review => "review",
            Self::Report => "report",
            Self::Consent => "consent",
            Self::Settings => "settings",
            Self::Diagnostics => "diagnostics",
        }
    }

    /// 索引（0..10）。
    #[must_use]
    pub(crate) const fn index(self) -> usize {
        match self {
            Self::Home => 0,
            Self::Papers => 1,
            Self::Rules => 2,
            Self::Import => 3,
            Self::Progress => 4,
            Self::Review => 5,
            Self::Report => 6,
            Self::Consent => 7,
            Self::Settings => 8,
            Self::Diagnostics => 9,
        }
    }

    /// 从索引取页（越界返回 `None`）。
    #[must_use]
    pub(crate) const fn from_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(Self::Home),
            1 => Some(Self::Papers),
            2 => Some(Self::Rules),
            3 => Some(Self::Import),
            4 => Some(Self::Progress),
            5 => Some(Self::Review),
            6 => Some(Self::Report),
            7 => Some(Self::Consent),
            8 => Some(Self::Settings),
            9 => Some(Self::Diagnostics),
            _ => None,
        }
    }

    /// 快捷键数字字符（`home`→`1` … `diagnostics`→`0`）。
    #[must_use]
    pub(crate) const fn shortcut_digit(self) -> char {
        match self.index() {
            0 => '1',
            1 => '2',
            2 => '3',
            3 => '4',
            4 => '5',
            5 => '6',
            6 => '7',
            7 => '8',
            8 => '9',
            _ => '0',
        }
    }
}

/// 快捷键修饰键状态（macOS 上 `control` 即 Command ⌘，取自 Slint `KeyboardModifiers`）。
#[allow(
    clippy::struct_excessive_bools,
    reason = "镜像 Slint KeyboardModifiers 的四个布尔字段，拆成位标志反而降低可读性"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Modifiers {
    /// Command（macOS）/ Control（其它平台）。
    pub(crate) control: bool,
    /// Shift。
    pub(crate) shift: bool,
    /// Alt/Option。
    pub(crate) alt: bool,
    /// Control（macOS 上为 Control 键本身）。
    pub(crate) meta: bool,
}

impl Modifiers {
    /// 构造。
    #[allow(
        clippy::fn_params_excessive_bools,
        reason = "镜像 Slint KeyboardModifiers 的四个布尔字段"
    )]
    #[must_use]
    pub(crate) const fn new(control: bool, shift: bool, alt: bool, meta: bool) -> Self {
        Self {
            control,
            shift,
            alt,
            meta,
        }
    }
}

/// 解析后的全局快捷动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shortcut {
    /// 直达某页（`Cmd+1`…`Cmd+0`、`Cmd+,`、`Cmd+L`）。
    Navigate(PageId),
    /// 导入文件（`Cmd+O`；T35 先导航到导入页，T36 接文件选择）。
    ImportFiles,
    /// 导入文件夹（`Cmd+Shift+O`）。
    ImportFolder,
    /// 请求切换模式（`Cmd+Shift+M`，需二次确认且运行中任务时拒绝）。
    ToggleMode,
    /// 关闭弹层/取消当前操作（`Esc`）。
    Dismiss,
}

/// Esc 键在 Slint 中的 `text` 可能是转义字符（`\u{1b}`）或空串；两种都认。
fn is_escape(text: &str) -> bool {
    text == "\u{1b}" || text.eq_ignore_ascii_case("escape")
}

/// 单字符比较（忽略大小写；Slint 的 `event.text` 对字母键通常是小写）。
fn same_key(text: &str, expected: char) -> bool {
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => c.eq_ignore_ascii_case(&expected),
        _ => false,
    }
}

/// 解析全局快捷键；未命中返回 `None`（由 Slint 继续 `reject` 给下层控件）。
///
/// 规则（`SPEC-app.md` §2.1）：
/// - `Cmd+1`…`Cmd+0`：十页直达；
/// - `Cmd+,`：设置；`Cmd+L`：日志与诊断；
/// - `Cmd+O` / `Cmd+Shift+O`：导入文件/文件夹（T35 先导航占位）；
/// - `Cmd+Shift+M`：切换模式（避开系统最小化 ⌘M）；
/// - `Esc`：关闭弹层/取消。
///
/// 带 `meta`（macOS 上为 Control 键）的组合一律不接管，避免与系统/输入法冲突。
#[must_use]
pub(crate) fn resolve_shortcut(text: &str, modifiers: Modifiers) -> Option<Shortcut> {
    if !modifiers.control && !modifiers.alt && !modifiers.meta {
        return if is_escape(text) {
            Some(Shortcut::Dismiss)
        } else {
            None
        };
    }
    if modifiers.meta || modifiers.alt {
        return None;
    }
    if !modifiers.control {
        return None;
    }
    if is_escape(text) {
        return Some(Shortcut::Dismiss);
    }
    if same_key(text, ',') {
        return Some(Shortcut::Navigate(PageId::Settings));
    }
    if same_key(text, 'l') {
        return Some(Shortcut::Navigate(PageId::Diagnostics));
    }
    if same_key(text, 'm') && modifiers.shift {
        return Some(Shortcut::ToggleMode);
    }
    if same_key(text, 'o') {
        return Some(if modifiers.shift {
            Shortcut::ImportFolder
        } else {
            Shortcut::ImportFiles
        });
    }
    if modifiers.shift {
        return None;
    }
    let mut chars = text.chars();
    let (Some(digit), None) = (chars.next(), chars.next()) else {
        return None;
    };
    let index = match digit {
        '1'..='9' => usize::from(digit as u8 - b'1'),
        '0' => 9,
        _ => return None,
    };
    PageId::from_index(index).map(Shortcut::Navigate)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn cmd() -> Modifiers {
        Modifiers::new(true, false, false, false)
    }

    #[test]
    fn ten_pages_in_spec_order() {
        let expected = [
            "home",
            "papers",
            "rules",
            "import",
            "progress",
            "review",
            "report",
            "consent",
            "settings",
            "diagnostics",
        ];
        for (index, page) in PageId::ALL.into_iter().enumerate() {
            assert_eq!(page.index(), index);
            assert_eq!(page.code(), expected[index]);
            assert_eq!(PageId::from_index(index), Some(page));
        }
        assert_eq!(PageId::from_index(10), None);
    }

    #[test]
    fn cmd_digits_navigate_to_all_ten_pages() {
        for page in PageId::ALL {
            let text = page.shortcut_digit().to_string();
            assert_eq!(
                resolve_shortcut(&text, cmd()),
                Some(Shortcut::Navigate(page)),
                "Cmd+{text} 应到 {}",
                page.code()
            );
        }
    }

    #[test]
    fn cmd_shortcuts_for_settings_logs_import_mode_and_escape() {
        assert_eq!(
            resolve_shortcut(",", cmd()),
            Some(Shortcut::Navigate(PageId::Settings))
        );
        assert_eq!(
            resolve_shortcut("l", cmd()),
            Some(Shortcut::Navigate(PageId::Diagnostics))
        );
        assert_eq!(resolve_shortcut("o", cmd()), Some(Shortcut::ImportFiles));
        assert_eq!(
            resolve_shortcut("O", Modifiers::new(true, true, false, false)),
            Some(Shortcut::ImportFolder)
        );
        assert_eq!(
            resolve_shortcut("m", Modifiers::new(true, true, false, false)),
            Some(Shortcut::ToggleMode)
        );
        assert_eq!(
            resolve_shortcut("\u{1b}", Modifiers::default()),
            Some(Shortcut::Dismiss)
        );
        assert_eq!(
            resolve_shortcut("escape", Modifiers::default()),
            Some(Shortcut::Dismiss)
        );
    }

    #[test]
    fn unmodified_digits_do_not_hijack_typing() {
        assert_eq!(resolve_shortcut("1", Modifiers::default()), None);
        assert_eq!(resolve_shortcut("j", Modifiers::default()), None);
        assert_eq!(resolve_shortcut("m", Modifiers::default()), None);
    }

    #[test]
    fn control_or_alt_modifiers_are_not_hijacked() {
        assert_eq!(
            resolve_shortcut("1", Modifiers::new(false, false, false, true)),
            None,
            "macOS 的 Control 键组合不接管"
        );
        assert_eq!(
            resolve_shortcut("1", Modifiers::new(true, false, true, false)),
            None,
            "Cmd+Option 不接管"
        );
        assert_eq!(resolve_shortcut("m", cmd()), None, "Cmd+M 让给系统最小化");
        assert_eq!(resolve_shortcut("x", cmd()), None);
    }
}
