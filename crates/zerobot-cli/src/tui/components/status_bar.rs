//! Status bar component — renders the bottom status line with model info,
//! permission mode, and context usage.

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
    fn build_spans(state: &AppState, theme: &Theme) -> Vec<Span<'static>> {
        let used = state
            .context_used
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".to_string());
        let limit = state
            .context_limit
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".to_string());
        let percent = match (state.context_used, state.context_limit) {
            (Some(used), Some(limit)) if limit > 0 => {
                format!("{:.1}%", (used as f64 / limit as f64) * 100.0)
            }
            _ => "-".to_string(),
        };

        let mode_label = match state.permission_mode {
            PermissionMode::Default => "",
            PermissionMode::Plan => "计划",
            PermissionMode::AcceptEdits => "自动编辑",
            PermissionMode::BypassPermissions => "绕过",
        };
        let mode_style = match state.permission_mode {
            PermissionMode::Default => Style::default().fg(theme.text),
            PermissionMode::Plan => Style::default().fg(Color::Yellow),
            PermissionMode::AcceptEdits => Style::default().fg(Color::Green),
            PermissionMode::BypassPermissions => Style::default().fg(Color::Red),
        };

        let mut spans = vec![
            Span::styled(
                " ZeroBot ",
                Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("{} ", state.model), Style::default().fg(theme.text)),
        ];

        if !mode_label.is_empty() {
            spans.push(Span::styled(" │ ", Style::default().fg(theme.text_dim)));
            spans.push(Span::styled(format!("{} ", mode_label), mode_style));
        }

        spans.push(Span::styled(" │ ", Style::default().fg(theme.text_dim)));
        spans.push(Span::styled(
            format!("{used}/{limit} ({percent}) "),
            Style::default().fg(theme.text),
        ));

        spans
    }
}
