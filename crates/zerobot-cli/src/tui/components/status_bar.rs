//! Status bar component — renders the bottom status line.
//!
//! Claude Code style: right-aligned permission mode indicator with keybinding hint.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use zerobot_core::config::PermissionMode;

use crate::tui::app::AppState;
use crate::tui::theme::{Theme, THEME};

/// Renders the status bar at the bottom of the screen.
pub struct StatusBar;

impl StatusBar {
    pub fn render(buf: &mut Buffer, area: Rect, state: &AppState) {
        let theme = &THEME;
        let spans = Self::build_spans(state, theme);
        let line = Line::from(spans);
        ratatui::widgets::Paragraph::new(line)
            .style(Style::default().bg(theme.status_bg))
            .render(area, buf);
    }

    /// Build the styled spans for the status bar content.
    ///
    /// Format: `⏵⏵ {mode} on (shift+tab to cycle)`
    fn build_spans(state: &AppState, theme: &Theme) -> Vec<Span<'static>> {
        let (mode_label, mode_style) = match state.permission_mode {
            PermissionMode::Default => ("default permissions", Style::default().fg(theme.text)),
            PermissionMode::Plan => ("plan mode", Style::default().fg(Color::Yellow)),
            PermissionMode::AcceptEdits => (
                "auto-edit on",
                Style::default().fg(Color::Green),
            ),
            PermissionMode::BypassPermissions => (
                "bypass permissions on",
                Style::default().fg(Color::Red),
            ),
        };

        vec![
            Span::styled(
                "\u{23F5}\u{23F5} ",
                Style::default().fg(theme.text_muted),
            ),
            Span::styled(mode_label.to_string(), mode_style),
            Span::styled(
                " (shift+tab to cycle)",
                Style::default().fg(theme.text_muted),
            ),
        ]
    }
}
