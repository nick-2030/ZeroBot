//! Bottom area rendering.
//!
//! The bottom area sits below the scroll box and above the status bar.  It
//! contains the text input prompt and, when the agent is active, a status
//! spinner line.

use ratatui::layout::Rect;
use ratatui::buffer::Buffer;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::tui::app::{AppState, Status};
use crate::tui::theme::THEME;

/// Renderer for the bottom input / spinner area.
///
/// Currently a placeholder implementation; full rendering will be added in a
/// subsequent task.
pub struct BottomArea;

impl BottomArea {
    /// Render the bottom area into `buf` within the given `area`.
    ///
    /// Draws a simple two-line placeholder: a spinner/status line (when the
    /// agent is active) and an input prompt line.
    pub fn render(buf: &mut Buffer, area: Rect, state: &AppState) {
        let theme = &*THEME;
        let mut y = area.y;

        // Spinner / status line (present for non-Idle statuses).
        if !matches!(state.status, Status::Idle) {
            if y < area.y + area.height {
                let label = match &state.status {
                    Status::Thinking => "... thinking".to_string(),
                    Status::Tool(name) => format!("... tool: {name}"),
                    Status::Hook(name) => format!("... hook: {name}"),
                    Status::Error(msg) => format!("! error: {msg}"),
                    Status::WaitingUserInput => "... waiting for input".to_string(),
                    Status::WaitingApproval => "... waiting for approval".to_string(),
                    Status::Idle => String::new(), // unreachable
                };
                let line = Line::from(Span::styled(label, Style::default().fg(theme.thinking)));
                let row = Rect::new(area.x, y, area.width, 1);
                Widget::render(
                    ratatui::widgets::Paragraph::new(line),
                    row,
                    buf,
                );
                y += 1;
            }
        }

        // Input prompt line.
        if y < area.y + area.height {
            let prompt = format!("> {}", state.input);
            let line = Line::from(Span::styled(prompt, Style::default().fg(theme.input_prompt)));
            let row = Rect::new(area.x, y, area.width, 1);
            Widget::render(
                ratatui::widgets::Paragraph::new(line),
                row,
                buf,
            );
        }
    }

    /// Compute the height (in rows) that the bottom area needs for the given
    /// application state.
    ///
    /// Always returns 3: the InputLine component uses top border + content +
    /// bottom border = 3 rows.  Status/spinner information is conveyed through
    /// the streaming buffer in the messages area instead.
    pub fn height_needed(_state: &AppState) -> u16 {
        3
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_state(status: Status) -> AppState {
        let mut state = AppState::new("test".into(), "provider".into(), "model".into());
        state.status = status;
        state
    }

    #[test]
    fn height_needed_idle() {
        let state = test_state(Status::Idle);
        assert_eq!(BottomArea::height_needed(&state), 3);
    }

    #[test]
    fn height_needed_thinking() {
        let state = test_state(Status::Thinking);
        assert_eq!(BottomArea::height_needed(&state), 3);
    }

    #[test]
    fn height_needed_error() {
        let state = test_state(Status::Error("test".into()));
        assert_eq!(BottomArea::height_needed(&state), 3);
    }
}
