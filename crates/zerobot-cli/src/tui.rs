use std::io;
use std::time::{Duration, Instant};

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph, Wrap},
    Terminal,
};

use zerobot_sdk::ZerobotClient;

pub fn run(server: String, api_key: String) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let rt = tokio::runtime::Runtime::new()?;
    let client = ZerobotClient::new(server, api_key);
    let session_id = rt.block_on(client.create_session(Some("TUI Session".to_string())))?;

    let mut input = String::new();
    let mut messages: Vec<String> = Vec::new();
    let mut last_refresh = Instant::now();

    loop {
        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(3), Constraint::Length(3)].as_ref())
                .split(f.size());

            let text: Vec<Line> = messages.iter().rev().take(200).rev().map(|m| Line::from(m.clone())).collect();
            let chat = Paragraph::new(text)
                .block(Block::default().title("zerobot" ).borders(Borders::ALL))
                .wrap(Wrap { trim: true });
            f.render_widget(chat, chunks[0]);

            let input_widget = Paragraph::new(input.as_str())
                .block(Block::default().title("Input").borders(Borders::ALL))
                .style(Style::default().fg(Color::Yellow));
            f.render_widget(input_widget, chunks[1]);
        })?;

        if last_refresh.elapsed() > Duration::from_millis(800) {
            if let Ok(state) = rt.block_on(client.get_session(&session_id)) {
                messages = state
                    .messages
                    .iter()
                    .map(|m| format!("{:?}: {}", m.role, m.content))
                    .collect();
            }
            last_refresh = Instant::now();
        }

        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                        break;
                    }
                    KeyCode::Char(ch) => {
                        input.push(ch);
                    }
                    KeyCode::Backspace => {
                        input.pop();
                    }
                    KeyCode::Enter => {
                        let content = input.trim().to_string();
                        if !content.is_empty() {
                            let _ = rt.block_on(client.send_message(&session_id, content.clone()));
                        }
                        input.clear();
                    }
                    _ => {}
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;
    Ok(())
}
