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
        // Format: │ /name     description    │  (description right-aligned)
        let inner_w = area.width.saturating_sub(2) as usize;
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

            // Left border
            buf.set_string(
                area.x,
                y,
                "\u{2502}",
                Style::default().fg(theme.panel_border).bg(theme.panel_bg),
            );

            // Build the content: " /name  description" padded to fill width
            let cmd_part = format!(" /{}", m.name);
            let desc_part = format!("{} ", m.description);
            // Fill between cmd and desc with spaces
            let used = cmd_part.chars().count() + desc_part.chars().count();
            let padding = if used < inner_w {
                " ".repeat(inner_w - used)
            } else {
                String::new()
            };
            let full_text = format!("{cmd_part}{padding}{desc_part}");

            // Truncate if too long
            let display: String = full_text.chars().take(inner_w).collect();
            let padded = if display.chars().count() < inner_w {
                format!(
                    "{display}{:<width$}",
                    "",
                    width = inner_w - display.chars().count()
                )
            } else {
                display
            };

            // Render with two styles: name part dimmed, description part normal
            let name_len = cmd_part.chars().count();
            let name_str: String = padded.chars().take(name_len).collect();
            let rest_str: String = padded.chars().skip(name_len).collect();

            let name_style = if i as usize == state.slash_selected {
                Style::default()
                    .fg(theme.text_dim)
                    .bg(theme.selected_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.text_dim).bg(theme.panel_bg)
            };

            buf.set_string(area.x + 1, y, &name_str, name_style);
            buf.set_string(
                area.x + 1 + name_len as u16,
                y,
                &rest_str,
                content_style,
            );

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
