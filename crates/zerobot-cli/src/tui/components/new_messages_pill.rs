//! "N new messages" floating pill indicator.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::tui::theme::THEME;

pub struct NewMessagesPill;

impl NewMessagesPill {
    /// Render a centered "N new messages" pill at the bottom of the area.
    pub fn render(buf: &mut Buffer, area: Rect, count: usize) {
        if count == 0 {
            return;
        }
        let theme = &THEME;
        let label = format!(" {count} \u{6761}\u{65b0}\u{6d88}\u{606f} "); // N 条新消息
        let x = area
            .x
            .saturating_add(area.width.saturating_sub(label.len() as u16) / 2);
        let y = area.y + area.height.saturating_sub(1);
        buf.set_string(
            x,
            y,
            &label,
            Style::default().fg(theme.accent).bg(theme.panel_bg),
        );
    }
}
