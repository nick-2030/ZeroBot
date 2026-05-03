//! Slash command suggestions dropdown.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::tui::app::AppState;
use crate::tui::theme::THEME;

pub struct SlashSuggestions;

impl SlashSuggestions {
    pub fn render(buf: &mut Buffer, area: Rect, state: &AppState) {
        if state.slash_query.is_none() || state.slash_matches.is_empty() {
            return;
        }
        let theme = &THEME;
        let max_visible = area.height.min(state.slash_matches.len() as u16);

        // Draw top border: ╭────╮
        if area.height >= 2 {
            let top = format!(
                "\u{256D}{}\u{256E}",
                "\u{2500}".repeat(area.width.saturating_sub(2) as usize)
            );
            buf.set_string(
                area.x,
                area.y,
                &top,
                Style::default().fg(theme.panel_border).bg(theme.panel_bg),
            );
        }

        // Draw content rows with side borders
        for i in 0..max_visible {
            let m = &state.slash_matches[i as usize];
            let y = area.y + 1 + i;
            let content_style = if i as usize == state.slash_selected {
                Style::default()
                    .fg(theme.text)
                    .bg(theme.selected_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.text).bg(theme.panel_bg)
            };
            let text = format!(" {} - {}", m.name, m.description);
            // Left border
            buf.set_string(
                area.x,
                y,
                "\u{2502}",
                Style::default().fg(theme.panel_border).bg(theme.panel_bg),
            );
            // Content (padded to fill width)
            let inner_w = area.width.saturating_sub(2) as usize;
            let padded = if text.len() < inner_w {
                format!("{text}{:<width$}", "", width = inner_w - text.len())
            } else {
                text.chars().take(inner_w).collect()
            };
            buf.set_string(area.x + 1, y, &padded, content_style);
            // Right border
            buf.set_string(
                area.x + area.width - 1,
                y,
                "\u{2502}",
                Style::default().fg(theme.panel_border).bg(theme.panel_bg),
            );
        }

        // Draw bottom border: ╰────╯
        let bottom_y = area.y + 1 + max_visible;
        if bottom_y < area.y + area.height {
            let bottom = format!(
                "\u{2570}{}\u{256F}",
                "\u{2500}".repeat(area.width.saturating_sub(2) as usize)
            );
            buf.set_string(
                area.x,
                bottom_y,
                &bottom,
                Style::default().fg(theme.panel_border).bg(theme.panel_bg),
            );
        }
    }
}
