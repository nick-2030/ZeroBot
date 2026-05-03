//! Permission prompt rendering helper.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Block, Borders, Widget};

use crate::tui::theme::THEME;

pub struct PermissionPrompt;

impl PermissionPrompt {
    /// Render a permission prompt block with the given title and options.
    pub fn render(
        buf: &mut Buffer,
        area: Rect,
        title: &str,
        options: &[&str],
        selected: usize,
    ) {
        let theme = &THEME;
        let block = Block::default()
            .title(format!(" {title} "))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.permission));
        let inner = block.inner(area);
        Widget::render(block, area, buf);

        for (i, opt) in options.iter().enumerate() {
            if i as u16 >= inner.height {
                break;
            }
            let y = inner.y + i as u16;
            let style = if i == selected {
                Style::default().fg(theme.text).bg(theme.selected_bg)
            } else {
                Style::default().fg(theme.text_dim)
            };
            let prefix = if i == selected { "> " } else { "  " };
            buf.set_string(inner.x, y, format!("{prefix}{opt}"), style);
        }
    }
}
