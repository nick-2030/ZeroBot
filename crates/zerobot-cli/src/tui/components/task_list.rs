//! Task list rendering.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::tui::app::AppState;
use crate::tui::theme::THEME;

pub struct TaskList;

impl TaskList {
    pub fn render(buf: &mut Buffer, area: Rect, state: &AppState) {
        if state.todos.is_empty() {
            return;
        }
        let theme = &THEME;
        for (i, todo) in state.todos.iter().enumerate() {
            if i as u16 >= area.height {
                break;
            }
            let y = area.y + i as u16;
            let (icon, style) = match todo.status {
                zerobot_core::session::TodoStatus::Completed => {
                    ("\u{2713}", Style::default().fg(theme.success))
                }
                zerobot_core::session::TodoStatus::InProgress => {
                    ("\u{25B6}", Style::default().fg(theme.accent))
                }
                zerobot_core::session::TodoStatus::Pending => {
                    ("\u{25CB}", Style::default().fg(theme.text_dim))
                }
                zerobot_core::session::TodoStatus::Cancelled => {
                    ("\u{2717}", Style::default().fg(theme.text_muted))
                }
            };
            buf.set_string(area.x, y, format!("{icon} {}", todo.content), style);
        }
    }
}
