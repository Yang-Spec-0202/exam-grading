//! 门禁单测（`SPEC-app.md` §7「门禁」）：`.slint` 无中文硬编码、无 IO/业务逻辑；
//! `eg-app` 不直接依赖数据库驱动与 HTTP 客户端；视图投影不出现空文案。
//!
//! 这些是「结构性不变量」，不依赖 GUI，可在 CI 无窗口环境运行。

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use crate::i18n::strings;
    use crate::nav::PageId;
    use crate::state::AppState;

    const APP_SLINT: &str = include_str!("../ui/app.slint");

    /// 去掉行注释（本项目 `.slint` 只使用 `//`，字符串里没有 `//`）。
    fn strip_line_comments(source: &str) -> String {
        source
            .lines()
            .map(|line| match line.find("//") {
                Some(index) => &line[..index],
                None => line,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn has_cjk(text: &str) -> bool {
        text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
    }

    #[test]
    fn slint_code_has_no_hardcoded_chinese() {
        let code = strip_line_comments(APP_SLINT);
        assert!(
            !has_cjk(&code),
            "`.slint` 代码区不得出现中文字面量（文案必须来自 Rust i18n 属性）"
        );
    }

    #[test]
    fn slint_has_no_io_or_business_logic_tokens() {
        let code = strip_line_comments(APP_SLINT).to_lowercase();
        for token in [
            "http://", "https://", "sqlite", "rusqlite", "reqwest", "ureq", "std::", "println",
            "eprintln", "tokio", "async ",
        ] {
            assert!(
                !code.contains(token),
                "`.slint` 不得包含 IO/业务逻辑标记「{token}」"
            );
        }
    }

    #[test]
    fn app_does_not_depend_on_database_driver_or_http_client_directly() {
        let manifest = include_str!("../Cargo.toml");
        for token in [
            "rusqlite", "sqlx", "reqwest", "ureq", "hyper", "tokio", "curl",
        ] {
            assert!(
                !manifest.contains(token),
                "eg-app 不得直接依赖「{token}」（须经 eg-platform / eg-inference）"
            );
        }
    }

    #[test]
    fn view_model_has_no_empty_user_visible_text() {
        let state = AppState::new(eg_domain::GradeMode::Standard, true, 0, 0);
        for page in PageId::ALL {
            let mut state = state.clone();
            let _ = state.navigate_to(page);
            let view = state.view(strings());
            assert_eq!(view.nav.len(), 10);
            assert_ne!(view.page_title.trim(), "", "page_title 不得为空");
            assert_ne!(view.page_subtitle.trim(), "", "page_subtitle 不得为空");
            assert_ne!(view.page_body.trim(), "", "page_body 不得为空");
            assert_ne!(view.page_action.trim(), "", "page_action 不得为空");
            assert_ne!(view.hint.trim(), "", "hint 不得为空");
            assert_ne!(view.channel_text.trim(), "", "channel_text 不得为空");
            assert_ne!(view.status_left.trim(), "", "status_left 不得为空");
            assert!(
                view.nav.iter().all(|entry| !entry.title.trim().is_empty()),
                "导航标题不得为空"
            );
        }
    }

    #[test]
    fn settings_page_contains_about_text() {
        let mut state = AppState::new(eg_domain::GradeMode::Standard, true, 0, 0);
        let _ = state.navigate_to(PageId::Settings);
        let view = state.view(strings());
        assert!(view.show_about);
        assert!(view.about_body.contains("GPL-3.0-only"));
        assert_ne!(view.about_title.trim(), "", "about_title 不得为空");
    }
}
