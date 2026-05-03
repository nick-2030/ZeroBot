//! Spinner component — animated status indicator above the input area.
//!
//! Displays a braille-dots rotating animation with a random verb and elapsed
//! time, matching Claude Code's spinner style.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::tui::app::{AppState, Status};
use crate::tui::theme::THEME;

/// Braille dots rotating animation frames.
const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const SPINNER_INTERVAL_MS: u128 = 80;

pub struct Spinner;

impl Spinner {
    fn current_frame(elapsed_ms: u128) -> &'static str {
        let cycle = SPINNER_FRAMES.len() as u128 * SPINNER_INTERVAL_MS;
        let pos = elapsed_ms % cycle;
        let idx = (pos / SPINNER_INTERVAL_MS) as usize;
        SPINNER_FRAMES[idx.min(SPINNER_FRAMES.len() - 1)]
    }

    fn format_elapsed(elapsed: std::time::Duration) -> String {
        let secs = elapsed.as_secs();
        if secs < 60 {
            format!("{secs}s")
        } else {
            format!("{}m {}s", secs / 60, secs % 60)
        }
    }

    fn status_text(status: &Status) -> Option<&str> {
        match status {
            Status::Idle => None,
            Status::Thinking => Some("thinking"),
            Status::Tool(name) => Some(name.as_str()),
            Status::Hook(name) => Some(name.as_str()),
            Status::Error(msg) => Some(msg.as_str()),
            Status::WaitingUserInput => Some("waiting for input"),
            Status::WaitingApproval => Some("waiting for approval"),
        }
    }

    pub fn render(buf: &mut Buffer, area: Rect, state: &AppState) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let theme = &THEME;
        let status_text = match Self::status_text(&state.status) {
            Some(text) => text,
            None => return,
        };
        let elapsed = state
            .thinking_start
            .map(|start| start.elapsed())
            .unwrap_or_default();
        let frame = Self::current_frame(elapsed.as_millis());
        let elapsed_str = Self::format_elapsed(elapsed);
        let verb = if state.spinner_verb.is_empty() {
            status_text
        } else {
            state.spinner_verb.as_str()
        };

        let line = Line::from(vec![
            Span::styled(format!(" {frame} "), Style::default().fg(theme.accent)),
            Span::styled(verb.to_string(), Style::default().fg(theme.thinking)),
            Span::styled(
                format!(" ({elapsed_str})"),
                Style::default().fg(theme.text_muted),
            ),
        ]);
        ratatui::widgets::Paragraph::new(line)
            .style(Style::default().bg(theme.panel_bg))
            .render(area, buf);
    }
}
