//! Slash command suggestions dropdown.
//!
//! Matches Claude Code's PromptInputFooterSuggestions:
//! - Two-column layout: name (40% width) + description (remaining)
//! - Selected item uses `suggestion` color, others are dimmed
//! - Rounded border with `accent` color

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
                Style::default().fg(theme.accent).bg(theme.panel_bg),
            );
        }

        // Content rows — two-column format matching Claude Code
        // Name column: 40% of inner width, padded
        // Description column: remaining width
        let inner_w = area.width.saturating_sub(2) as usize;
        let name_col_w = (inner_w as f64 * 0.4) as usize;

        for i in 0..max_visible {
            let m = &state.slash_matches[i as usize];
            let y = area.y + 1 + i;
            let is_selected = i as usize == state.slash_selected;

            // Left border
            buf.set_string(
                area.x,
                y,
                "\u{2502}",
                Style::default().fg(theme.accent).bg(theme.panel_bg),
            );

            // Build name column: " /name" padded to name_col_w
            let name_str = format!(" /{}", m.name);
            let name_padded = if name_str.chars().count() < name_col_w {
                format!(
                    "{name_str}{:<width$}",
                    "",
                    width = name_col_w - name_str.chars().count()
                )
            } else {
                name_str.chars().take(name_col_w).collect::<String>()
            };

            // Description column: fills remaining space
            let desc_w = inner_w.saturating_sub(name_col_w);
            let desc_str: String = m.description.chars().take(desc_w).collect();
            let desc_padded = if desc_str.chars().count() < desc_w {
                format!(
                    "{desc_str}{:<width$}",
                    "",
                    width = desc_w - desc_str.chars().count()
                )
            } else {
                desc_str
            };

            // Render name column
            let name_style = if is_selected {
                Style::default()
                    .fg(theme.suggestion)
                    .bg(theme.selected_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.text).bg(theme.panel_bg)
            };
            buf.set_string(area.x + 1, y, &name_padded, name_style);

            // Render description column
            let desc_style = if is_selected {
                Style::default()
                    .fg(theme.suggestion)
                    .bg(theme.selected_bg)
            } else {
                Style::default().fg(theme.text_dim).bg(theme.panel_bg)
            };
            buf.set_string(
                area.x + 1 + name_col_w as u16,
                y,
                &desc_padded,
                desc_style,
            );

            // Right border
            buf.set_string(
                area.x + area.width - 1,
                y,
                "\u{2502}",
                Style::default().fg(theme.accent).bg(theme.panel_bg),
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
                Style::default().fg(theme.accent).bg(theme.panel_bg),
            );
        }
    }
}
