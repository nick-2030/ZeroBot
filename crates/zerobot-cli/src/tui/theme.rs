use ratatui::style::Color;
use std::sync::LazyLock;

/// Global theme instance used across the TUI.
pub static THEME: LazyLock<Theme> = LazyLock::new(Theme::default);

/// Color palette — values sourced from Claude Code `src/utils/theme.ts` dark theme.
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
    /// Shimmer color for spinner verb highlight
    pub accent_shimmer: Color,
    /// Autocomplete suggestion highlight color
    pub suggestion: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            // Background: near-black
            panel_bg: Color::Rgb(0, 0, 0),
            // Borders: medium gray
            panel_border: Color::Rgb(136, 136, 136),
            // Text: pure white
            text: Color::Rgb(255, 255, 255),
            // Dimmed text: light gray (inactive)
            text_dim: Color::Rgb(153, 153, 153),
            // Muted text: dark gray (subtle)
            text_muted: Color::Rgb(80, 80, 80),
            // Claude orange
            accent: Color::Rgb(215, 119, 87),
            // Claude shimmer (lighter orange)
            accent_dim: Color::Rgb(180, 95, 65),
            // Selection background: dark blue
            selected_bg: Color::Rgb(38, 79, 120),
            // Success: bright green
            success: Color::Rgb(78, 186, 101),
            // Error: bright red-pink
            error: Color::Rgb(255, 107, 128),
            // Warning: bright amber
            warn: Color::Rgb(255, 193, 7),
            // Thinking/spinner text
            thinking: Color::Rgb(153, 153, 153),
            // Tool output border
            tool_border: Color::Rgb(136, 136, 136),
            // Permission mode: light blue-purple
            permission: Color::Rgb(177, 185, 249),
            // Plan mode: muted sage green
            plan_mode: Color::Rgb(72, 150, 140),
            // User message background: lighter grey
            user_message_bg: Color::Rgb(55, 55, 55),
            // Input prompt color: default text (white)
            input_prompt: Color::Rgb(255, 255, 255),
            // Status bar background: black
            status_bg: Color::Rgb(0, 0, 0),
            // Modal divider
            modal_divider: Color::Rgb(136, 136, 136),
            // Shimmer: lighter claude orange
            accent_shimmer: Color::Rgb(235, 159, 127),
            // Suggestion: light blue-purple
            suggestion: Color::Rgb(177, 185, 249),
        }
    }
}
