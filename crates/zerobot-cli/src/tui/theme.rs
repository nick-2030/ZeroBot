use ratatui::style::Color;
use std::sync::LazyLock;

/// Global theme instance used across the TUI.
pub static THEME: LazyLock<Theme> = LazyLock::new(Theme::default);

/// Color palette for the entire TUI.
///
/// All semantic colors are centralized here so that changing the theme
/// propagates to every component automatically.
#[derive(Debug, Clone)]
pub struct Theme {
    pub panel_bg: Color,
    pub panel_border: Color,
    pub text: Color,
    pub text_dim: Color,
    pub text_muted: Color,
    pub accent: Color,
    pub accent_dim: Color,
    pub selected_bg: Color,
    pub success: Color,
    pub error: Color,
    pub warn: Color,
    pub thinking: Color,
    pub tool_border: Color,
    pub permission: Color,
    pub plan_mode: Color,
    pub user_message_bg: Color,
    pub input_prompt: Color,
    pub status_bg: Color,
    pub modal_divider: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            panel_bg: Color::Rgb(32, 36, 44),
            panel_border: Color::Rgb(70, 76, 88),
            text: Color::Rgb(220, 224, 232),
            text_dim: Color::Rgb(136, 142, 156),
            text_muted: Color::Rgb(100, 106, 120),
            accent: Color::Rgb(215, 119, 87),
            accent_dim: Color::Rgb(180, 95, 65),
            selected_bg: Color::Rgb(55, 60, 75),
            success: Color::Rgb(152, 195, 121),
            error: Color::Rgb(224, 108, 117),
            warn: Color::Rgb(229, 192, 123),
            thinking: Color::Rgb(120, 120, 140),
            tool_border: Color::Rgb(80, 90, 110),
            permission: Color::Rgb(100, 149, 237),
            plan_mode: Color::Rgb(0, 191, 165),
            user_message_bg: Color::Rgb(38, 42, 52),
            input_prompt: Color::Rgb(215, 119, 87),
            status_bg: Color::Rgb(32, 36, 44),
            modal_divider: Color::Rgb(100, 149, 237),
        }
    }
}
