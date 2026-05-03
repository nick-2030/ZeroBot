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
        for i in 0..max_visible {
            let m = &state.slash_matches[i as usize];
            let y = area.y + i;
            let style = if i as usize == state.slash_selected {
                Style::default()
                    .fg(theme.text)
                    .bg(theme.selected_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.text)
            };
            let text = format!(" {} - {}", m.name, m.description);
            buf.set_string(area.x, y, &text, style);
        }
    }
}
