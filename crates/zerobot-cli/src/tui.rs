use std::io::{self, BufRead};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Terminal,
};
use serde_json::Value;

use zerobot_sdk::ZerobotClient;

#[derive(Clone, Copy)]
enum EntryKind {
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Copy)]
enum EntryStatus {
    Pending,
    Success,
    Error,
}

struct Entry {
    prefix: String,
    content: String,
    kind: EntryKind,
    status: EntryStatus,
    placeholder: bool,
}

enum UiEvent {
    Sse { event_type: String, data: Value },
    SendError(String),
}

pub fn run(server: String, api_key: String) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let rt = tokio::runtime::Runtime::new()?;
    let client = ZerobotClient::new(server.clone(), api_key.clone());
    let session_id = rt.block_on(client.create_session(Some("TUI Session".to_string())))?;

    let mut entries = load_initial_entries(&rt, &client, &session_id);
    let mut current_assistant: Option<usize> = None;
    let mut input = String::new();

    let (event_tx, event_rx) = mpsc::channel();
    let (send_tx, send_rx) = mpsc::channel();

    spawn_sender_thread(server.clone(), api_key.clone(), session_id.clone(), send_rx, event_tx.clone());
    spawn_sse_thread(server.clone(), api_key.clone(), session_id.clone(), event_tx.clone());

    loop {
        drain_events(&mut entries, &mut current_assistant, &event_rx);

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(3), Constraint::Length(3)].as_ref())
                .split(f.size());

            let lines = entries_to_lines(&entries, 300);
            let chat = Paragraph::new(lines)
                .block(Block::default().title("zerobot").borders(Borders::ALL))
                .wrap(Wrap { trim: true });
            f.render_widget(chat, chunks[0]);

            let input_widget = Paragraph::new(input.as_str())
                .block(Block::default().title("Input").borders(Borders::ALL))
                .style(Style::default().fg(Color::Yellow));
            f.render_widget(input_widget, chunks[1]);
        })?;

        if event::poll(Duration::from_millis(100))? {
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
                            entries.push(Entry {
                                prefix: "> ".to_string(),
                                content: content.clone(),
                                kind: EntryKind::User,
                                status: EntryStatus::Success,
                                placeholder: false,
                            });
                            entries.push(Entry {
                                prefix: "● ".to_string(),
                                content: "思考中...".to_string(),
                                kind: EntryKind::Assistant,
                                status: EntryStatus::Pending,
                                placeholder: true,
                            });
                            current_assistant = Some(entries.len() - 1);
                            let _ = send_tx.send(content);
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

fn load_initial_entries(
    rt: &tokio::runtime::Runtime,
    client: &ZerobotClient,
    session_id: &str,
) -> Vec<Entry> {
    if let Ok(state) = rt.block_on(client.get_session(session_id)) {
        return state
            .messages
            .iter()
            .map(|m| Entry {
                prefix: match m.role {
                    zerobot_core::Role::User => "> ".to_string(),
                    zerobot_core::Role::Assistant => "● ".to_string(),
                    zerobot_core::Role::Tool => "● ".to_string(),
                    zerobot_core::Role::System => "• ".to_string(),
                },
                content: m.content.clone(),
                kind: match m.role {
                    zerobot_core::Role::User => EntryKind::User,
                    zerobot_core::Role::Tool => EntryKind::Tool,
                    _ => EntryKind::Assistant,
                },
                status: EntryStatus::Success,
                placeholder: false,
            })
            .collect();
    }
    Vec::new()
}

fn spawn_sender_thread(
    server: String,
    api_key: String,
    session_id: String,
    rx: Receiver<String>,
    tx: Sender<UiEvent>,
) {
    thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let client = ZerobotClient::new(server, api_key);
        while let Ok(content) = rx.recv() {
            if let Err(err) = rt.block_on(client.send_message(&session_id, content)) {
                let _ = tx.send(UiEvent::SendError(err.to_string()));
            }
        }
    });
}

fn spawn_sse_thread(server: String, api_key: String, session_id: String, tx: Sender<UiEvent>) {
    thread::spawn(move || loop {
        let url = format!("{}/v1/sessions/{}/events", server.trim_end_matches('/'), session_id);
        let client = reqwest::blocking::Client::new();
        let resp = client
            .get(url)
            .header("x-zerobot-api-key", api_key.clone())
            .send();
        let resp = match resp {
            Ok(resp) => resp,
            Err(_) => {
                thread::sleep(Duration::from_millis(500));
                continue;
            }
        };
        let mut event_type = String::new();
        let mut data_buf = String::new();
        let reader = io::BufReader::new(resp);
        for line in reader.lines().flatten() {
            if line.is_empty() {
                if !event_type.is_empty() {
                    let data = serde_json::from_str(&data_buf).unwrap_or(Value::String(data_buf.clone()));
                    let _ = tx.send(UiEvent::Sse {
                        event_type: event_type.clone(),
                        data,
                    });
                }
                event_type.clear();
                data_buf.clear();
                continue;
            }
            if let Some(value) = line.strip_prefix("event:") {
                event_type = value.trim().to_string();
            } else if let Some(value) = line.strip_prefix("data:") {
                if !data_buf.is_empty() {
                    data_buf.push('\n');
                }
                data_buf.push_str(value.trim());
            }
        }
        thread::sleep(Duration::from_millis(300));
    });
}

fn drain_events(entries: &mut Vec<Entry>, current_assistant: &mut Option<usize>, rx: &Receiver<UiEvent>) {
    while let Ok(event) = rx.try_recv() {
        match event {
            UiEvent::SendError(err) => {
                if let Some(idx) = *current_assistant {
                    if let Some(entry) = entries.get_mut(idx) {
                        entry.status = EntryStatus::Error;
                        if entry.content.is_empty() {
                            entry.content = format!("发送失败: {}", err);
                        } else {
                            entry.content.push_str(&format!("\n发送失败: {}", err));
                        }
                    }
                }
            }
            UiEvent::Sse { event_type, data } => match event_type.as_str() {
                "token" => {
                    let chunk = data
                        .get("content")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if let Some(idx) = *current_assistant {
                        if let Some(entry) = entries.get_mut(idx) {
                            if entry.placeholder {
                                entry.content.clear();
                                entry.placeholder = false;
                            }
                            entry.content.push_str(chunk);
                        }
                    } else {
                        entries.push(Entry {
                            prefix: "● ".to_string(),
                            content: chunk.to_string(),
                            kind: EntryKind::Assistant,
                            status: EntryStatus::Pending,
                            placeholder: false,
                        });
                        *current_assistant = Some(entries.len() - 1);
                    }
                }
                "tool_result" => {
                    let name = data.get("name").and_then(|v| v.as_str()).unwrap_or("tool");
                    let output = data.get("output").cloned().unwrap_or(Value::Null);
                    let is_error = data.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false);
                    entries.push(Entry {
                        prefix: "● ".to_string(),
                        content: format!("[tool:{}] {}", name, output),
                        kind: EntryKind::Tool,
                        status: if is_error { EntryStatus::Error } else { EntryStatus::Success },
                        placeholder: false,
                    });
                }
                "agent_status" => {
                    let state = data.get("state").and_then(|v| v.as_str()).unwrap_or("");
                    match state {
                        "running" => {
                            if current_assistant.is_none() {
                                entries.push(Entry {
                                    prefix: "● ".to_string(),
                                    content: "思考中...".to_string(),
                                    kind: EntryKind::Assistant,
                                    status: EntryStatus::Pending,
                                    placeholder: true,
                                });
                                *current_assistant = Some(entries.len() - 1);
                            } else if let Some(idx) = *current_assistant {
                                if let Some(entry) = entries.get_mut(idx) {
                                    entry.status = EntryStatus::Pending;
                                }
                            }
                        }
                        "completed" => {
                            if let Some(idx) = *current_assistant {
                                if let Some(entry) = entries.get_mut(idx) {
                                    entry.status = EntryStatus::Success;
                                }
                            }
                        }
                        "failed" => {
                            if let Some(idx) = *current_assistant {
                                if let Some(entry) = entries.get_mut(idx) {
                                    entry.status = EntryStatus::Error;
                                    entry.placeholder = false;
                                    if let Some(err) = data.get("error").and_then(|v| v.as_str()) {
                                        if entry.content.is_empty() {
                                            entry.content = err.to_string();
                                        } else {
                                            entry.content.push_str(&format!("\n{}", err));
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            },
        }
    }
}

fn entries_to_lines(entries: &[Entry], max_lines: usize) -> Vec<Line<'_>> {
    let mut lines = Vec::new();
    for entry in entries {
        let prefix_style = prefix_style(entry);
        let content_style = content_style(entry);
        let content = if entry.content.is_empty() {
            vec![String::new()]
        } else {
            entry.content.lines().map(|s| s.to_string()).collect()
        };
        for (idx, line) in content.into_iter().enumerate() {
            let prefix = if idx == 0 {
                entry.prefix.clone()
            } else {
                "  ".to_string()
            };
            lines.push(Line::from(vec![
                Span::styled(prefix, prefix_style),
                Span::styled(line, content_style),
            ]));
        }
    }
    if lines.len() > max_lines {
        lines.split_off(lines.len() - max_lines)
    } else {
        lines
    }
}

fn prefix_style(entry: &Entry) -> Style {
    match entry.kind {
        EntryKind::User => Style::default().fg(Color::Cyan),
        EntryKind::Assistant | EntryKind::Tool => match entry.status {
            EntryStatus::Pending => Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::SLOW_BLINK),
            EntryStatus::Success => Style::default().fg(Color::Green),
            EntryStatus::Error => Style::default().fg(Color::Red),
        },
    }
}

fn content_style(_entry: &Entry) -> Style {
    Style::default().fg(Color::White)
}
