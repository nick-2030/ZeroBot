pub mod app;
pub mod command;
pub mod component;
pub mod components;
pub mod keybindings;
pub mod layout;
pub mod markdown;
pub mod message;
pub mod overlay;
pub mod theme;

// ---------------------------------------------------------------------------
// Event loop: run_tui
// ---------------------------------------------------------------------------

use anyhow::Result;
use std::collections::HashSet;
use std::sync::{Arc, RwLock as StdRwLock};
use tokio::sync::{mpsc, oneshot, RwLock as TokioRwLock};
use tokio::task::JoinHandle;
use tokio::time::{self, Duration};
use tokio_stream::StreamExt;

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use zerobot_core::agent::Agent;
use zerobot_core::config::Settings;
use zerobot_core::error::ZeroBotResult;
use zerobot_core::events::AgentEvent;
use zerobot_core::hooks::HookManager;
use zerobot_core::interaction::{
    InteractionHandler, ToolApprovalDecision, ToolApprovalRequest, ToolApprovalResponse,
    UserInputRequest, UserInputResponse,
};
use zerobot_core::plugin::PluginManager;
use zerobot_core::provider::ProviderFactory;
use zerobot_core::session::SessionStore;
use zerobot_core::tool::ToolRegistry;
use zerobot_core::{Curator, SelfReviewer};

use crate::tui::app::{AppState, DotColor, Status};
use crate::tui::command::Command;
use crate::tui::keybindings::types::KeyAction;
use crate::tui::keybindings::KeybindingManager;
use crate::tui::layout::FullscreenLayout;
use crate::tui::message::Message;
use crate::tui::overlay::{OverlayComponent, OverlayType};
use crate::tui::theme::THEME;

// ---------------------------------------------------------------------------
// UiRequest / UiInteractionHandler — agent -> UI communication
// ---------------------------------------------------------------------------

/// Requests sent from the agent thread back to the TUI event loop.
enum UiRequest {
    UserInput {
        request: UserInputRequest,
        respond_to: oneshot::Sender<UserInputResponse>,
    },
    ToolApproval {
        request: ToolApprovalRequest,
        respond_to: oneshot::Sender<ToolApprovalResponse>,
    },
}

/// Bridges `InteractionHandler` calls from the agent into `UiRequest` messages
/// that the TUI event loop can process.
struct UiInteractionHandler {
    tx: mpsc::UnboundedSender<UiRequest>,
}

#[async_trait::async_trait]
impl InteractionHandler for UiInteractionHandler {
    async fn request_user_input(
        &self,
        request: UserInputRequest,
    ) -> Result<UserInputResponse, zerobot_core::ZeroBotError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(UiRequest::UserInput {
                request,
                respond_to: tx,
            })
            .map_err(|_| {
                zerobot_core::ZeroBotError::Tool("无法发送用户输入请求".to_string())
            })?;
        rx.await
            .map_err(|_| zerobot_core::ZeroBotError::Tool("等待用户输入失败".to_string()))
    }

    async fn request_tool_approval(
        &self,
        request: ToolApprovalRequest,
    ) -> Result<ToolApprovalResponse, zerobot_core::ZeroBotError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(UiRequest::ToolApproval {
                request,
                respond_to: tx,
            })
            .map_err(|_| {
                zerobot_core::ZeroBotError::Tool("无法发送授权请求".to_string())
            })?;
        rx.await
            .map_err(|_| zerobot_core::ZeroBotError::Tool("等待授权失败".to_string()))
    }
}

// ---------------------------------------------------------------------------
// run_tui — the event loop entry point
// ---------------------------------------------------------------------------

/// Run the TUI with the Message/Command architecture.
///
/// This function initializes the terminal, creates the application state, and
/// runs the main event loop until the user quits.
pub async fn run_tui(
    settings: Settings,
    cwd: std::path::PathBuf,
    session_id: String,
    store: Arc<dyn SessionStore>,
    tools: ToolRegistry,
    provider_factory: ProviderFactory,
    model: String,
    provider_id: String,
    hooks: HookManager,
    resume: bool,
    use_alt_screen: bool,
    provider_state: Arc<StdRwLock<String>>,
    plugins: Option<Arc<PluginManager>>,
    tool_approvals: Arc<TokioRwLock<HashSet<String>>>,
    self_reviewer: Option<SelfReviewer>,
    curator: Option<Curator>,
) -> Result<String> {
    // -- Terminal setup -------------------------------------------------------
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    if use_alt_screen {
        crossterm::execute!(stdout, EnterAlternateScreen)?;
    }
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // -- Run the event loop ---------------------------------------------------
    let result = run_tui_inner(
        &mut terminal,
        settings,
        cwd,
        session_id,
        store,
        tools,
        provider_factory,
        model,
        provider_id,
        hooks,
        resume,
        provider_state,
        plugins,
        tool_approvals,
        self_reviewer,
        curator,
    )
    .await;

    // -- Terminal teardown ----------------------------------------------------
    disable_raw_mode()?;
    if use_alt_screen {
        crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    }
    terminal.show_cursor()?;

    result
}

/// Inner event loop (separated for terminal setup/teardown).
async fn run_tui_inner(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    settings: Settings,
    cwd: std::path::PathBuf,
    session_id: String,
    store: Arc<dyn SessionStore>,
    tools: ToolRegistry,
    provider_factory: ProviderFactory,
    model: String,
    provider_id: String,
    hooks: HookManager,
    _resume: bool,
    _provider_state: Arc<StdRwLock<String>>,
    plugins: Option<Arc<PluginManager>>,
    tool_approvals: Arc<TokioRwLock<HashSet<String>>>,
    _self_reviewer: Option<SelfReviewer>,
    _curator: Option<Curator>,
) -> Result<String> {
    // -- Application state ----------------------------------------------------
    let mut app = AppState::new(session_id.clone(), provider_id.clone(), model.clone());

    // Welcome message
    let (cols, _rows) = crossterm::terminal::size().unwrap_or((120, 40));
    let welcome = build_welcome_lines(
        env!("CARGO_PKG_VERSION"),
        &provider_id,
        &model,
        &cwd.display().to_string(),
        cols as usize,
    );
    app.push_lines(welcome);

    // TODO: resume session / refresh session state (Task 8+)

    // -- Keybinding manager ---------------------------------------------------
    let mut keybindings = KeybindingManager::with_defaults();

    // -- Channels and shared state --------------------------------------------
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    hooks.set_event_sender(tx.clone());
    let (ui_tx, mut ui_rx) = mpsc::unbounded_channel::<UiRequest>();
    let interaction: Arc<dyn InteractionHandler> = Arc::new(UiInteractionHandler {
        tx: ui_tx.clone(),
    });

    // -- Agent runner ---------------------------------------------------------
    let mut runner: Option<JoinHandle<ZeroBotResult<String>>> = None;

    // -- Input reader and tick timer ------------------------------------------
    let mut reader = EventStream::new();
    let mut tick = time::interval(Duration::from_millis(50));

    // Initial render
    app.mark_dirty();

    // ====================================================================
    // Event loop
    // ====================================================================
    loop {
        // -- Render if dirty --------------------------------------------------
        if app.is_dirty() {
            terminal.draw(|frame| {
                let areas = FullscreenLayout::compute(frame.size(), &app);

                // Main components
                components::messages::Messages::render(
                    frame.buffer_mut(),
                    areas.scroll_box,
                    &app,
                );
                components::input_line::InputLine::render(
                    frame.buffer_mut(),
                    areas.bottom_area,
                    &app,
                );
                components::status_bar::StatusBar::render(
                    frame.buffer_mut(),
                    areas.status_bar,
                    &app,
                );

                // Overlay (if any)
                if let (Some(ref overlay), Some(modal_area)) = (&app.overlay, areas.modal_overlay)
                {
                    render_overlay(overlay, modal_area, frame.buffer_mut(), &THEME);
                }

                // Cursor
                if app.overlay.is_none() {
                    if let Some((cx, cy)) =
                        components::input_line::InputLine::cursor_position(
                            areas.bottom_area,
                            &app,
                        )
                    {
                        frame.set_cursor(cx, cy);
                    }
                }
            })?;
            app.clear_dirty();
        }

        // -- Check quit -------------------------------------------------------
        if app.should_quit {
            break;
        }

        // -- Select on events -------------------------------------------------
        tokio::select! {
            // Tick: cursor blink
            _ = tick.tick() => {
                if app.tick() {
                    app.mark_dirty();
                }
            }

            // Terminal input events
            maybe_event = reader.next() => {
                if let Some(Ok(event)) = maybe_event {
                    match event {
                        Event::Key(key) if key.kind == KeyEventKind::Press => {
                            handle_key_event(
                                key,
                                &mut app,
                                &mut keybindings,
                                &mut runner,
                                &settings,
                                &cwd,
                                &store,
                                &tools,
                                &provider_factory,
                                &hooks,
                                &interaction,
                                &plugins,
                                &tool_approvals,
                                &tx,
                            );
                        }
                        Event::Resize(w, h) => {
                            app.resize(w, h);
                        }
                        _ => {}
                    }
                }
            }

            // Agent events
            Some(event) = rx.recv() => {
                let msg = Message::from_agent_event(event);
                let cmd = app.update(msg);
                handle_cmd(
                    cmd,
                    &mut app,
                    &mut runner,
                    &settings,
                    &cwd,
                    &store,
                    &tools,
                    &provider_factory,
                    &hooks,
                    &interaction,
                    &plugins,
                    &tool_approvals,
                    &tx,
                );
            }

            // UI requests from agent thread
            Some(request) = ui_rx.recv() => {
                handle_ui_request(request, &mut app);
            }

            // Agent runner completed
            result = async { runner.as_mut().unwrap().await }, if runner.is_some() => {
                runner = None;
                match result {
                    Ok(Ok(_)) => {
                        let cmd = app.update(Message::AgentDone);
                        handle_cmd(
                            cmd,
                            &mut app,
                            &mut runner,
                            &settings,
                            &cwd,
                            &store,
                            &tools,
                            &provider_factory,
                            &hooks,
                            &interaction,
                            &plugins,
                            &tool_approvals,
                            &tx,
                        );
                    }
                    Ok(Err(err)) => {
                        tracing::error!("[tui] runner error: {err}");
                        app.update(Message::AgentError(format!("{err}")));
                    }
                    Err(join_err) => {
                        tracing::error!("[tui] runner panic: {join_err}");
                        app.update(Message::AgentError(format!("runner panic: {join_err}")));
                    }
                }
            }
        }
    }

    Ok(app.session_id.clone())
}

// ---------------------------------------------------------------------------
// Key event handling
// ---------------------------------------------------------------------------

/// Dispatch a key press event through the overlay / keybinding / default chain.
fn handle_key_event(
    key: KeyEvent,
    app: &mut AppState,
    keybindings: &mut KeybindingManager,
    runner: &mut Option<JoinHandle<ZeroBotResult<String>>>,
    settings: &Settings,
    cwd: &std::path::PathBuf,
    store: &Arc<dyn SessionStore>,
    tools: &ToolRegistry,
    provider_factory: &ProviderFactory,
    hooks: &HookManager,
    interaction: &Arc<dyn InteractionHandler>,
    plugins: &Option<Arc<PluginManager>>,
    tool_approvals: &Arc<TokioRwLock<HashSet<String>>>,
    tx: &mpsc::UnboundedSender<AgentEvent>,
) {
    // 1. If an overlay is active, let the overlay handle the key first.
    if app.overlay.is_some() {
        if let Some(msg) = overlay_handle_key(app, key) {
            let cmd = app.update(msg);
            handle_cmd(
                cmd,
                app,
                runner,
                settings,
                cwd,
                store,
                tools,
                provider_factory,
                hooks,
                interaction,
                plugins,
                tool_approvals,
                tx,
            );
        }
        return;
    }

    // 2. Try keybinding resolution.
    let ctxs = app.active_contexts();
    if let Some(action) = keybindings.resolve(key, &ctxs) {
        let msg = map_action_to_message(action, app);
        let is_interrupt = matches!(msg, Message::Interrupt);
        let cmd = app.update(msg);

        // Special handling for Interrupt: abort running agent directly
        if is_interrupt {
            abort_runner(runner, app);
        }

        handle_cmd(
            cmd,
            app,
            runner,
            settings,
            cwd,
            store,
            tools,
            provider_factory,
            hooks,
            interaction,
            plugins,
            tool_approvals,
            tx,
        );
        return;
    }

    // 3. Default key handling (text input, cursor movement, etc.)
    if let Some(msg) = default_key_message(key, app) {
        let cmd = app.update(msg);
        handle_cmd(
            cmd,
            app,
            runner,
            settings,
            cwd,
            store,
            tools,
            provider_factory,
            hooks,
            interaction,
            plugins,
            tool_approvals,
            tx,
        );
    }
}

// ---------------------------------------------------------------------------
// Overlay key dispatch
// ---------------------------------------------------------------------------

/// Forward a key event to the active overlay's `handle_key` method.
///
/// Returns `Some(message)` if the overlay produced a message to process,
/// or `None` if the key was consumed without producing a message.
fn overlay_handle_key(app: &mut AppState, key: KeyEvent) -> Option<Message> {
    let overlay = app.overlay.as_mut()?;
    match overlay {
        OverlayType::ToolApproval(o) => o.handle_key(key),
        OverlayType::UserInput(o) => o.handle_key(key),
        OverlayType::HistorySearch(o) => o.handle_key(key),
        OverlayType::Help(o) => o.handle_key(key),
        OverlayType::MessageSelector(o) => o.handle_key(key),
        OverlayType::TurnCost(o) => o.handle_key(key),
    }
}

// ---------------------------------------------------------------------------
// Overlay rendering
// ---------------------------------------------------------------------------

/// Render an overlay into the given area.
fn render_overlay(
    overlay: &OverlayType,
    area: ratatui::layout::Rect,
    buf: &mut ratatui::buffer::Buffer,
    theme: &theme::Theme,
) {
    match overlay {
        OverlayType::ToolApproval(o) => o.render(area, buf, theme),
        OverlayType::UserInput(o) => o.render(area, buf, theme),
        OverlayType::HistorySearch(o) => o.render(area, buf, theme),
        OverlayType::Help(o) => o.render(area, buf, theme),
        OverlayType::MessageSelector(o) => o.render(area, buf, theme),
        OverlayType::TurnCost(o) => o.render(area, buf, theme),
    }
}

// ---------------------------------------------------------------------------
// map_action_to_message
// ---------------------------------------------------------------------------

/// Convert a semantic `KeyAction` (from the keybinding resolver) into a `Message`
/// that `AppState::update` can process.
fn map_action_to_message(action: KeyAction, _app: &AppState) -> Message {
    match action {
        KeyAction::Interrupt => Message::Interrupt,
        KeyAction::Exit => Message::Quit,
        KeyAction::Redraw => Message::Redraw,
        KeyAction::CycleMode => Message::CyclePermissionMode,
        KeyAction::ShowHelp => Message::ShowHelp,
        KeyAction::Cancel => Message::CloseOverlay,
        KeyAction::Submit => Message::InputSubmit,
        KeyAction::PageUp => Message::ScrollUp,
        KeyAction::PageDown => Message::ScrollDown,
        KeyAction::ScrollToTop => Message::ScrollToTop,
        KeyAction::ScrollToBottom => Message::ScrollToBottom,
        KeyAction::HistoryPrevious => Message::Noop, // TODO
        KeyAction::HistoryNext => Message::Noop,     // TODO
        KeyAction::HistorySearch => Message::ShowOverlay(
            message::OverlayKind::History,
        ),
        KeyAction::ToggleTodos => Message::Noop,       // TODO
        KeyAction::ToggleTranscript => Message::ToggleFullToolOutput,
        KeyAction::LineUp => Message::ScrollUp,
        KeyAction::LineDown => Message::ScrollDown,
        _ => Message::Noop,
    }
}

// ---------------------------------------------------------------------------
// default_key_message
// ---------------------------------------------------------------------------

/// Produce a `Message` for key events not handled by the keybinding system.
///
/// This primarily handles text input (character insertion, backspace, cursor
/// movement, etc.) when no overlay is active.
fn default_key_message(key: KeyEvent, app: &AppState) -> Option<Message> {
    // If an overlay is open, don't process text input.
    if app.overlay.is_some() {
        return None;
    }
    match key.code {
        KeyCode::Char(c)
            if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
        {
            Some(Message::InputChar(c))
        }
        KeyCode::Backspace if key.modifiers.is_empty() || key.modifiers == KeyModifiers::CONTROL => {
            if key.modifiers == KeyModifiers::CONTROL {
                Some(Message::InputDeleteWord)
            } else {
                Some(Message::InputBackspace)
            }
        }
        KeyCode::Delete => Some(Message::InputDelete),
        KeyCode::Left if key.modifiers.is_empty() => Some(Message::InputMoveCursor(-1)),
        KeyCode::Right if key.modifiers.is_empty() => Some(Message::InputMoveCursor(1)),
        KeyCode::Home if key.modifiers.is_empty() => Some(Message::CursorToStart),
        KeyCode::End if key.modifiers.is_empty() => Some(Message::CursorToEnd),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// handle_cmd — execute side-effects produced by AppState::update
// ---------------------------------------------------------------------------

/// Execute a `Command` returned from `AppState::update`.
fn handle_cmd(
    cmd: Command,
    app: &mut AppState,
    runner: &mut Option<JoinHandle<ZeroBotResult<String>>>,
    settings: &Settings,
    cwd: &std::path::PathBuf,
    store: &Arc<dyn SessionStore>,
    tools: &ToolRegistry,
    provider_factory: &ProviderFactory,
    hooks: &HookManager,
    interaction: &Arc<dyn InteractionHandler>,
    plugins: &Option<Arc<PluginManager>>,
    tool_approvals: &Arc<TokioRwLock<HashSet<String>>>,
    tx: &mpsc::UnboundedSender<AgentEvent>,
) {
    match cmd {
        Command::None => {}

        Command::Quit => {
            app.should_quit = true;
        }

        Command::ClearScreen => {
            // State already cleared inside `update`; nothing else to do.
        }

        Command::SpawnAgent { prompt } => {
            let provider = match (*provider_factory)() {
                Ok(p) => p,
                Err(e) => {
                    app.update(Message::AgentError(format!("Provider 错误: {e}")));
                    return;
                }
            };
            let agent = Agent::new(
                provider,
                app.model.clone(),
                settings.clone(),
                store.clone(),
                tools.clone(),
                cwd.clone(),
                hooks.clone(),
                Some(interaction.clone()),
                plugins.clone(),
                tool_approvals.clone(),
                None, // tool_route
                None, // outbound
                None, // task_id
                None, // parent_task_id
                None, // agent_type
                None, // iteration_budget
                None, // notification_tx
            );
            let session_id = app.session_id.clone();
            let tx_clone = tx.clone();
            *runner = Some(tokio::spawn(async move {
                agent.run_turn(&session_id, &prompt, Some(tx_clone)).await
            }));
        }

        Command::CopyToClipboard(text) => {
            // TODO: implement clipboard integration
            let _ = text;
        }

        Command::OpenExternalEditor => {
            // TODO: implement external editor integration
        }

        Command::ResumeSession { session_id } => {
            // TODO: implement session resume in new architecture
            let _ = session_id;
        }

        Command::RewindTo { message_id, input } => {
            // TODO: implement session rewind in new architecture
            let _ = (message_id, input);
        }
    }
}

// ---------------------------------------------------------------------------
// abort_runner — interrupt the running agent
// ---------------------------------------------------------------------------

/// Abort the currently running agent task and clean up state.
fn abort_runner(runner: &mut Option<JoinHandle<ZeroBotResult<String>>>, app: &mut AppState) {
    if let Some(handle) = runner.take() {
        handle.abort();
    }
    app.finalize_stream();
    // Mark any still-running tools as interrupted
    let running: Vec<_> = app.running_tools.drain().collect();
    for (_, rt) in running {
        if rt.output_idx < app.output.len() {
            let duration_ms = Some(rt.start_time.elapsed().as_millis() as u64);
            app.output[rt.output_idx] = app::OutputItem::ToolOutput {
                color: DotColor::Yellow,
                tool_name: "interrupted".to_string(),
                label: Some(rt.label),
                arguments: rt.arguments,
                output: "已中断".to_string(),
                expanded: false,
                duration_ms,
            };
        }
    }
    // Dismiss any active overlay (send deny/cancel responses).
    // For ToolApproval we explicitly send Deny; for UserInput we drop the
    // overlay which closes the oneshot channel (the agent sees a Recv error).
    if let Some(ref mut overlay) = app.overlay {
        if let OverlayType::ToolApproval(o) = overlay {
            o.finish(ToolApprovalDecision::Deny);
        }
    }
    app.overlay = None;
    if let Some(prev) = app.overlay_prev_status.take() {
        app.status = prev;
    }
    if !matches!(app.status, Status::Idle | Status::Error(_)) {
        app.status = Status::Idle;
    }
    app.push_block(DotColor::Yellow, "已中断当前执行");
    app.status_notice = None;
    app.mark_dirty();
}

// ---------------------------------------------------------------------------
// handle_ui_request — process requests from the agent thread
// ---------------------------------------------------------------------------

/// Process a `UiRequest` received from the agent's `InteractionHandler`.
fn handle_ui_request(request: UiRequest, app: &mut AppState) {
    match request {
        UiRequest::ToolApproval {
            request,
            respond_to,
        } => {
            app.overlay = Some(OverlayType::ToolApproval(
                overlay::ToolApprovalOverlay::new(request, respond_to),
            ));
            app.mark_dirty();
        }
        UiRequest::UserInput {
            request,
            respond_to,
        } => {
            app.overlay = Some(OverlayType::UserInput(
                overlay::UserInputOverlay::new(request, respond_to),
            ));
            app.mark_dirty();
        }
    }
}

// ---------------------------------------------------------------------------
// Welcome banner
// ---------------------------------------------------------------------------

/// Build the ASCII-art welcome banner lines.
fn build_welcome_lines(
    version: &str,
    provider: &str,
    model: &str,
    cwd: &str,
    term_width: usize,
) -> Vec<ratatui::text::Line<'static>> {
    use ratatui::style::Style;
    use ratatui::text::{Line, Span};

    let logo = [
        "███████╗███████╗██████╗  ██████╗ ██████╗  ██████╗ ████████╗",
        "╚══███╔╝██╔════╝██╔══██╗██╔═══██╗██╔══██╗██╔═══██╗╚══██╔══╝",
        "  ███╔╝ █████╗  ██████╔╝██║   ██║██████╔╝██║   ██║   ██║   ",
        " ███╔╝  ██╔════╝██╔══██╗██║   ██║██╔══██╗██║   ██║   ██║   ",
        "███████╗███████╗██║  ██║╚██████╔╝██████╔╝╚██████╔╝   ██║   ",
        "╚══════╝╚══════╝╚═╝  ╚═╝ ╚═════╝ ╚═════╝  ╚═════╝    ╚═╝   ",
    ];

    let title = format!(">_ zerobot (v{version})");
    let meta_line = format!("{provider} | {model}");
    let help = "输入 /help 查看命令";
    let box_lines = [title, meta_line, cwd.to_string(), help.to_string()];

    let logo_width = logo.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let box_width = box_lines
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(0)
        + 4;
    let min_width = logo_width + 2 + box_width;

    let mut out = Vec::new();
    let theme = &*THEME;

    if term_width >= min_width {
        let inner = box_width.saturating_sub(2);
        let top = format!("\u{256D}{}\u{256E}", "\u{2500}".repeat(inner));
        let bottom = format!("\u{2570}{}\u{256F}", "\u{2500}".repeat(inner));
        let mut box_rendered: Vec<(String, bool)> = Vec::new();
        box_rendered.push((top, true));
        for line in &box_lines {
            use unicode_width::UnicodeWidthStr;
            let pad = inner.saturating_sub(UnicodeWidthStr::width(line.as_str()));
            box_rendered.push((format!("\u{2502}{}{}\u{2502}", line, " ".repeat(pad)), false));
        }
        box_rendered.push((bottom, true));

        let rows = logo.len().max(box_rendered.len());
        for i in 0..rows {
            let left = *logo.get(i).unwrap_or(&"");
            let left_pad = logo_width.saturating_sub(left.chars().count());
            let right = box_rendered.get(i).map(|(s, _)| s.as_str()).unwrap_or("");
            let right_is_border = box_rendered.get(i).map(|(_, b)| *b).unwrap_or(false);
            let mut spans = Vec::new();
            spans.push(Span::styled(
                left.to_string(),
                Style::default().fg(theme.accent),
            ));
            spans.push(Span::raw(" ".repeat(left_pad)));
            spans.push(Span::raw("  "));
            if right_is_border {
                spans.push(Span::styled(
                    right.to_string(),
                    Style::default().fg(theme.panel_border),
                ));
            } else {
                let mut chars = right.chars();
                let left_border = chars.next().unwrap_or('\u{2502}').to_string();
                let right_border = right.chars().last().unwrap_or('\u{2502}').to_string();
                let middle: String = right
                    .chars()
                    .skip(1)
                    .take(right.chars().count().saturating_sub(2))
                    .collect();
                spans.push(Span::styled(
                    left_border,
                    Style::default().fg(theme.panel_border),
                ));
                spans.push(Span::raw(middle));
                spans.push(Span::styled(
                    right_border,
                    Style::default().fg(theme.panel_border),
                ));
            }
            out.push(Line::from(spans));
        }
    } else {
        let inner = box_width.saturating_sub(2).max(10);
        out.push(Line::from(Span::styled(
            format!("\u{256D}{}\u{256E}", "\u{2500}".repeat(inner)),
            Style::default().fg(theme.panel_border),
        )));
        for line in &box_lines {
            use unicode_width::UnicodeWidthStr;
            let pad = inner.saturating_sub(UnicodeWidthStr::width(line.as_str()));
            let mut spans = Vec::new();
            spans.push(Span::styled(
                "\u{2502}".to_string(),
                Style::default().fg(theme.panel_border),
            ));
            spans.push(Span::raw(format!("{}{}", line, " ".repeat(pad))));
            spans.push(Span::styled(
                "\u{2502}".to_string(),
                Style::default().fg(theme.panel_border),
            ));
            out.push(Line::from(spans));
        }
        out.push(Line::from(Span::styled(
            format!("\u{2570}{}\u{256F}", "\u{2500}".repeat(inner)),
            Style::default().fg(theme.panel_border),
        )));
    }
    out
}
