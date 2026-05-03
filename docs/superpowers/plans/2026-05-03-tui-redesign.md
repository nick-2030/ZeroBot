# ZeroBot TUI 重设计实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 完全重构 ZeroBot 的 TUI，从单文件 5156 行的 `tui.rs` 迁移到模块化的 `tui/` 目录，参考 Claude-Code 的 FullscreenLayout 布局、上下文/和弦快捷键系统、虚拟滚动等。

**Architecture:** Trait-based 组件系统，Message 驱动状态更新，Command 处理副作用。ratatui + crossterm 不变。

**Tech Stack:** ratatui 0.26, crossterm 0.27, pulldown-cmark, syntect, unicode-width, tokio

---

## 文件结构

```
crates/zerobot-cli/src/
  tui/
    mod.rs                 -- 入口 run_tui()
    app.rs                 -- AppState
    component.rs           -- Component trait
    message.rs             -- Message enum
    command.rs             -- Command enum
    theme.rs               -- Theme
    overlay.rs             -- OverlayType, OverlayComponent trait
    markdown.rs            -- Markdown 渲染（从 tui.rs 迁移）
    layout/
      mod.rs               -- FullscreenLayout
      scroll_box.rs        -- ScrollBox
      bottom_area.rs       -- BottomArea
      modal_overlay.rs     -- ModalOverlay
    components/
      mod.rs
      messages.rs          -- 消息列表虚拟化渲染
      message_item.rs      -- 单条消息
      input_line.rs        -- 输入行
      spinner.rs           -- 加载动画
      status_bar.rs        -- 状态栏
      tool_output.rs       -- 工具输出
      permission_prompt.rs -- 权限弹窗
      user_input_overlay.rs -- 用户输入弹窗
      history_search.rs    -- 历史搜索
      slash_suggestions.rs -- Slash 补全
      new_messages_pill.rs -- 新消息提示
      help_overlay.rs      -- 帮助弹窗
      task_list.rs         -- 任务列表
    keybindings/
      mod.rs               -- KeybindingManager
      default_bindings.rs  -- 默认绑定
      types.rs             -- KeyAction, KeyContext, KeyCombo
```

---

## Task 1: 创建 tui 模块骨架和核心类型

**Files:**
- Create: `crates/zerobot-cli/src/tui/mod.rs`
- Create: `crates/zerobot-cli/src/tui/theme.rs`
- Create: `crates/zerobot-cli/src/tui/component.rs`
- Create: `crates/zerobot-cli/src/tui/message.rs`
- Create: `crates/zerobot-cli/src/tui/command.rs`
- Modify: `crates/zerobot-cli/src/main.rs` (改 `mod tui;` 为 `mod tui;` 指向目录)

- [ ] **Step 1: 创建 tui 目录结构**

```bash
mkdir -p crates/zerobot-cli/src/tui/layout
mkdir -p crates/zerobot-cli/src/tui/components
mkdir -p crates/zerobot-cli/src/tui/keybindings
```

- [ ] **Step 2: 创建 theme.rs**

```rust
// crates/zerobot-cli/src/tui/theme.rs
use ratatui::style::Color;

#[derive(Debug, Clone)]
pub struct Theme {
    pub panel_bg: Color,
    pub panel_border: Color,
    pub text: Color,
    pub text_dim: Color,
    pub text_muted: Color,
    pub accent: Color,
    pub accent_dim: Color,
    pub selected_bg: Color,
    pub success: Color,
    pub error: Color,
    pub warn: Color,
    pub thinking: Color,
    pub tool_border: Color,
    pub permission: Color,
    pub plan_mode: Color,
    pub user_message_bg: Color,
    pub input_prompt: Color,
    pub status_bg: Color,
    pub modal_divider: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            panel_bg: Color::Rgb(32, 36, 44),
            panel_border: Color::Rgb(70, 76, 88),
            text: Color::Rgb(220, 224, 232),
            text_dim: Color::Rgb(136, 142, 156),
            text_muted: Color::Rgb(100, 106, 120),
            accent: Color::Rgb(186, 148, 255),
            accent_dim: Color::Rgb(132, 112, 190),
            selected_bg: Color::Rgb(48, 52, 64),
            success: Color::Rgb(124, 216, 168),
            error: Color::Rgb(236, 112, 104),
            warn: Color::Rgb(234, 196, 118),
            thinking: Color::Rgb(100, 100, 120),
            tool_border: Color::Rgb(80, 90, 110),
            permission: Color::Rgb(100, 149, 237),
            plan_mode: Color::Rgb(0, 191, 165),
            user_message_bg: Color::Rgb(38, 42, 52),
            input_prompt: Color::Rgb(186, 148, 255),
            status_bg: Color::Rgb(32, 36, 44),
            modal_divider: Color::Rgb(100, 149, 237),
        }
    }
}

pub static THEME: once_cell::sync::Lazy<Theme> = once_cell::sync::Lazy::new(Theme::default);
```

- [ ] **Step 3: 创建 component.rs**

```rust
// crates/zerobot-cli/src/tui/component.rs
use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::layout::Rect;
use ratatui::buffer::Buffer;
use super::app::AppState;
use super::message::Message;

pub trait Component {
    fn render(&self, area: Rect, buf: &mut Buffer, state: &AppState);
    fn handle_key(&mut self, key: KeyEvent, state: &mut AppState) -> Option<Message> { None }
    fn handle_mouse(&mut self, event: MouseEvent, state: &mut AppState) -> Option<Message> { None }
    fn is_dirty(&self) -> bool { true }
    fn clear_dirty(&mut self) {}
}
```

- [ ] **Step 4: 创建 message.rs**

从 `tui.rs` 提取所有消息类型：

```rust
// crates/zerobot-cli/src/tui/message.rs
use zerobot_core::events::AgentEvent;
use super::overlay::OverlayType;

#[derive(Debug, Clone)]
pub enum Message {
    // 输入
    InputChar(char),
    InputBackspace,
    InputDelete,
    InputSubmit,
    InputMoveCursor(i16),
    InputClear,
    InputDeleteWord,
    InputDeleteToEnd,
    CursorToStart,
    CursorToEnd,
    InputPaste(String),

    // 滚动
    ScrollUp(u16),
    ScrollDown(u16),
    ScrollToTop,
    ScrollToBottom,
    StickToBottom(bool),

    // 应用
    Quit,
    Interrupt,
    ClearScreen,
    Redraw,
    CyclePermissionMode,
    ToggleFullToolOutput,
    ShowTurnCost,
    ShowHelp,

    // Agent 事件
    AgentDelta(String),
    AgentMessage(String),
    ToolStarted { id: String, name: String, input: String },
    ToolFinished { id: String, name: String, output: String, ok: bool },
    ToolBatchStarted { ids: Vec<String>, parallel: bool },
    AgentDone,
    AgentError(String),
    SessionCost { input: u64, output: u64, cache_create: u64, cache_read: u64, turns: u32 },
    ContextUsage { used: usize, limit: Option<u32> },
    PermissionDenied { tool: String, reason: String },
    HookStarted { event: String, name: String, status: Option<String> },
    HookFinished { event: String, name: String, ok: bool, message: Option<String> },
    PluginWarning { plugin: String, hook: String, message: String, degraded: bool },
    SelfReviewCompleted { summary: String, memory_changes: usize, skill_changes: usize },

    // 覆盖层
    ShowOverlay(OverlayType),
    CloseOverlay,
    OverlaySelect(usize),
    OverlayConfirm,
    OverlayCancel,
    OverlayNextField,
    OverlayInput(String),

    // Slash
    SlashQuery(Option<String>),
    SlashSelect(usize),
    SlashExecute(String),
    SlashPage(i16),

    // 历史
    HistorySearch(String),
    HistorySelect(usize),

    // 会话
    SessionLoaded { session_id: String, messages: Vec<String> },
    RewindTo { message_id: String },

    Noop,
}

impl Message {
    pub fn from_agent_event(event: AgentEvent) -> Self {
        match event {
            AgentEvent::AssistantDelta { content } => Message::AgentDelta(content),
            AgentEvent::AssistantMessage { content } => Message::AgentMessage(content),
            AgentEvent::ToolCallStarted { tool_call_id, name, input } =>
                Message::ToolStarted { id: tool_call_id, name, input },
            AgentEvent::ToolCallFinished { tool_call_id, name, output, ok } =>
                Message::ToolFinished { id: tool_call_id, name, output, ok },
            AgentEvent::ToolBatchStarted { tool_call_ids, parallel } =>
                Message::ToolBatchStarted { ids: tool_call_ids, parallel },
            AgentEvent::Done => Message::AgentDone,
            AgentEvent::Error { message } => Message::AgentError(message),
            AgentEvent::SessionCost { input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens, turn_count } =>
                Message::SessionCost {
                    input: input_tokens,
                    output: output_tokens,
                    cache_create: cache_creation_tokens,
                    cache_read: cache_read_tokens,
                    turns: turn_count,
                },
            AgentEvent::ContextUsage { used, limit } =>
                Message::ContextUsage { used, limit },
            AgentEvent::PermissionDenied { tool_name, reason, .. } =>
                Message::PermissionDenied { tool: tool_name, reason },
            AgentEvent::HookStarted { event, hook_name, status_message } =>
                Message::HookStarted { event, name: hook_name, status: status_message },
            AgentEvent::HookFinished { event, hook_name, ok, message } =>
                Message::HookFinished { event, name: hook_name, ok, message },
            AgentEvent::PluginWarning { plugin, hook, message, degraded } =>
                Message::PluginWarning { plugin, hook, message, degraded },
            AgentEvent::SelfReviewCompleted { summary, memory_changes, skill_changes } =>
                Message::SelfReviewCompleted { summary, memory_changes, skill_changes },
            _ => Message::Noop,
        }
    }
}
```

- [ ] **Step 5: 创建 command.rs**

```rust
// crates/zerobot-cli/src/tui/command.rs

pub enum Command {
    None,
    SpawnAgent { prompt: String },
    Quit,
    ClearScreen,
    CopyToClipboard(String),
    OpenExternalEditor,
    ResumeSession { session_id: String },
    RewindTo { message_id: String, input: String },
}
```

- [ ] **Step 6: 创建 mod.rs 占位**

```rust
// crates/zerobot-cli/src/tui/mod.rs
pub mod app;
pub mod component;
pub mod message;
pub mod command;
pub mod theme;
pub mod overlay;
pub mod markdown;
pub mod layout;
pub mod components;
pub mod keybindings;

// 公共入口将在后续 Task 中实现
```

- [ ] **Step 7: 修改 main.rs 的 mod 声明**

将 `crates/zerobot-cli/src/main.rs` 中的：
```rust
mod tui;
```
改为指向目录模块（`mod tui;` 已经可以指向 `tui/mod.rs`，无需改动）。

- [ ] **Step 8: 验证编译**

```bash
cd /Volumes/nick-disk/projects/ai/ZeroBot
cargo check -p zerobot-cli 2>&1 | head -30
```

预期：会有一些未使用的警告，但不应有错误。

- [ ] **Step 9: 提交**

```bash
git add crates/zerobot-cli/src/tui/
git commit -m "feat(tui): 创建模块骨架和核心类型（Theme, Component, Message, Command）"
```

---

## Task 2: 创建 AppState

**Files:**
- Create: `crates/zerobot-cli/src/tui/app.rs`

- [ ] **Step 1: 创建 app.rs**

从 `tui.rs` 的 `App` struct 迁移所有字段，重命名为 `AppState`：

```rust
// crates/zerobot-cli/src/tui/app.rs
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;
use ratatui::style::Color;
use zerobot_core::config::{PermissionMode, Settings};
use zerobot_core::interaction::{ToolApprovalRequest, ToolApprovalResponse, UserInputRequest, UserInputResponse};
use zerobot_core::provider::TokenUsage;
use zerobot_core::session::TodoItem;
use crate::slash::{SlashMatch, SlashRegistry};
use super::message::Message;
use super::command::Command;
use super::overlay::OverlayType;
use super::theme::Theme;

#[derive(Clone)]
pub enum Status {
    Idle,
    Thinking,
    Tool(String),
    Hook(String),
    Error(String),
    WaitingUserInput,
    WaitingApproval,
}

#[derive(Clone)]
pub enum DotColor {
    White,
    Green,
    Yellow,
    Red,
}

#[derive(Clone)]
pub struct TurnCost {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation: u64,
    pub cache_read: u64,
}

#[derive(Clone)]
pub enum OutputItem {
    Lines(Vec<ratatui::text::Line<'static>>),
    Block { color: DotColor, text: String },
    Markdown(String),
    ToolRunning { label: String },
    ToolOutput {
        color: DotColor,
        tool_name: String,
        label: Option<String>,
        arguments: String,
        output: String,
        expanded: bool,
        duration_ms: Option<u64>,
    },
    HookRunning { label: String },
    HookOutput { ok: bool, label: String },
}

pub struct RunningTool {
    pub output_idx: usize,
    pub label: String,
    pub arguments: String,
    pub start_time: Instant,
}

pub struct AppState {
    // 身份
    pub session_id: String,
    pub provider_id: String,
    pub model: String,

    // 状态
    pub status: Status,
    pub permission_mode: PermissionMode,

    // 输出
    pub output: Vec<OutputItem>,
    pub stream_buffer: String,
    pub streaming: bool,
    pub scroll: u16,
    pub stick_to_bottom: bool,
    pub total_lines: usize,

    // 输入
    pub input: String,
    pub cursor: usize,

    // 统计
    pub usage: Option<TokenUsage>,
    pub context_used: Option<usize>,
    pub context_limit: Option<u32>,
    pub session_input_tokens: u64,
    pub session_output_tokens: u64,
    pub session_cache_creation_tokens: u64,
    pub session_cache_read_tokens: u64,
    pub session_turn_count: u32,
    pub turn_costs: Vec<TurnCost>,

    // 覆盖层
    pub overlay: Option<OverlayType>,
    pub overlay_queue: VecDeque<OverlayType>,

    // Slash 补全
    pub slash_query: Option<String>,
    pub slash_matches: Vec<SlashMatch>,
    pub slash_selected: usize,
    pub slash_page: usize,
    pub slash_hint: String,

    // 运行中工具
    pub running_tools: HashMap<String, RunningTool>,
    pub running_hook_output_idx: Option<usize>,
    pub active_hooks: Vec<String>,

    // 任务
    pub todos: Vec<TodoItem>,

    // 显示选项
    pub show_full_tool_output: bool,
    pub viewport_width: u16,
    pub viewport_height: u16,

    // 退出
    pub should_quit: bool,

    // 按键去抖
    pub last_idle_esc: Option<Instant>,
    pub last_idle_ctrl_c: Option<Instant>,

    // 状态通知
    pub status_notice: Option<String>,

    // 脏标记
    dirty: bool,

    // 剪贴板
    pub last_copyable_output: Option<String>,
}

const DOUBLE_PRESS_WINDOW_MS: u64 = 900;

impl AppState {
    pub fn new(session_id: String, provider_id: String, model: String) -> Self {
        Self {
            session_id,
            provider_id,
            model,
            permission_mode: PermissionMode::Default,
            status: Status::Idle,
            output: Vec::new(),
            stream_buffer: String::new(),
            streaming: false,
            input: String::new(),
            cursor: 0,
            scroll: 0,
            stick_to_bottom: true,
            total_lines: 0,
            usage: None,
            context_used: None,
            context_limit: None,
            session_input_tokens: 0,
            session_output_tokens: 0,
            session_cache_creation_tokens: 0,
            session_cache_read_tokens: 0,
            session_turn_count: 0,
            turn_costs: Vec::new(),
            overlay: None,
            overlay_queue: VecDeque::new(),
            slash_query: None,
            slash_matches: Vec::new(),
            slash_selected: 0,
            slash_page: 0,
            slash_hint: String::new(),
            running_tools: HashMap::new(),
            running_hook_output_idx: None,
            active_hooks: Vec::new(),
            todos: Vec::new(),
            show_full_tool_output: false,
            viewport_width: 80,
            viewport_height: 24,
            should_quit: false,
            last_idle_esc: None,
            last_idle_ctrl_c: None,
            status_notice: None,
            dirty: true,
            last_copyable_output: None,
        }
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn clear_dirty(&mut self) {
        self.dirty = false;
    }

    pub fn resize(&mut self, w: u16, h: u16) {
        self.viewport_width = w;
        self.viewport_height = h;
        self.mark_dirty();
    }

    pub fn tick(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(Instant::now()) >= std::time::Duration::from_millis(500) {
            self.mark_dirty();
            return true;
        }
        false
    }

    /// 返回当前活跃的快捷键上下文列表
    pub fn active_contexts(&self) -> Vec<super::keybindings::types::KeyContext> {
        use super::keybindings::types::KeyContext;
        let mut ctxs = vec![KeyContext::Global];

        if self.overlay.is_some() {
            match &self.overlay {
                Some(OverlayType::HistorySearch(_)) => ctxs.push(KeyContext::HistorySearch),
                Some(OverlayType::Help(_)) => ctxs.push(KeyContext::Help),
                Some(OverlayType::ToolApproval(_)) | Some(OverlayType::UserInput(_)) | Some(OverlayType::MessageSelector(_)) | Some(OverlayType::TurnCost(_)) => {
                    ctxs.push(KeyContext::Confirmation);
                }
                None => {}
            }
        } else if self.slash_query.is_some() {
            ctxs.push(KeyContext::Autocomplete);
        } else {
            ctxs.push(KeyContext::Chat);
        }

        ctxs
    }

    pub fn update(&mut self, msg: Message) -> Command {
        // 完整的 update 逻辑将在 Task 7 (集成) 中实现
        // 这里先提供框架
        match msg {
            Message::Quit => {
                self.should_quit = true;
                Command::Quit
            }
            Message::ClearScreen => {
                self.output.clear();
                self.mark_dirty();
                Command::ClearScreen
            }
            Message::Redraw => {
                self.mark_dirty();
                Command::None
            }
            _ => Command::None,
        }
    }
}
```

- [ ] **Step 2: 更新 mod.rs**

```rust
// crates/zerobot-cli/src/tui/mod.rs
pub mod app;
pub mod component;
pub mod message;
pub mod command;
pub mod theme;
pub mod overlay;
pub mod markdown;
pub mod layout;
pub mod components;
pub mod keybindings;
```

- [ ] **Step 3: 验证编译**

```bash
cargo check -p zerobot-cli 2>&1 | head -30
```

- [ ] **Step 4: 提交**

```bash
git add crates/zerobot-cli/src/tui/app.rs
git commit -m "feat(tui): 添加 AppState 全局状态容器"
```

---

## Task 3: 创建 Overlay 系统和 Markdown 渲染

**Files:**
- Create: `crates/zerobot-cli/src/tui/overlay.rs`
- Create: `crates/zerobot-cli/src/tui/markdown.rs`

- [ ] **Step 1: 创建 overlay.rs**

从 `tui.rs` 提取所有 overlay 类型：

```rust
// crates/zerobot-cli/src/tui/overlay.rs
use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::buffer::Buffer;
use zerobot_core::interaction::{ToolApprovalRequest, ToolApprovalResponse, UserInputRequest, UserInputResponse};
use tokio::sync::oneshot;
use super::message::Message;
use super::theme::Theme;

pub trait OverlayComponent {
    fn render(&self, area: Rect, buf: &mut Buffer, theme: &Theme);
    fn handle_key(&mut self, key: KeyEvent) -> Option<Message>;
    fn height_needed(&self, width: u16) -> u16;
}

pub enum OverlayType {
    ToolApproval(ToolApprovalOverlay),
    UserInput(UserInputOverlay),
    HistorySearch(HistorySearchOverlay),
    Help(HelpOverlay),
    MessageSelector(MessageSelectorOverlay),
    TurnCost(TurnCostOverlay),
}

// 从 tui.rs 迁移 ToolApprovalOverlay
pub struct ToolApprovalOverlay {
    pub request: ToolApprovalRequest,
    pub selected: usize,
    pub respond_to: Option<oneshot::Sender<ToolApprovalResponse>>,
}

impl OverlayComponent for ToolApprovalOverlay {
    fn render(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        // 渲染逻辑将在 Task 5 实现
    }
    fn handle_key(&mut self, key: KeyEvent) -> Option<Message> {
        None // 将在 Task 5 实现
    }
    fn height_needed(&self, width: u16) -> u16 {
        10
    }
}

// 从 tui.rs 迁移 UserInputOverlay
pub struct UserInputOverlay {
    pub request: UserInputRequest,
    pub current: usize,
    pub selected: usize,
    pub focus: UserInputFocus,
    pub notes: std::collections::HashMap<(String, Option<String>), String>,
    pub answers: std::collections::HashMap<String, UserInputAnswer>,
    pub respond_to: Option<oneshot::Sender<UserInputResponse>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum UserInputFocus {
    Options,
    Input,
}

pub struct HistorySearchOverlay {
    pub query: String,
    pub cursor: usize,
    pub results: Vec<SearchResult>,
    pub selected: usize,
}

pub struct SearchResult {
    pub message_id: String,
    pub role: String,
    pub preview: String,
}

pub struct HelpOverlay;
pub struct MessageSelectorOverlay;
pub struct TurnCostOverlay;
```

- [ ] **Step 2: 创建 markdown.rs**

从 `tui.rs` 提取 `render_markdown_line` 和相关函数：

```rust
// crates/zerobot-cli/src/tui/markdown.rs
use ratatui::text::{Line, Span};
use ratatui::style::{Color, Modifier, Style};
use pulldown_cmark::{Options, Parser, Event as MdEvent, Tag, TagEnd, CodeBlockKind, Alignment};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme as SyntectTheme, ThemeSet};
use syntect::parsing::SyntaxSet;
use std::sync::OnceLock;
use super::theme::THEME;

// 语法高亮的全局状态
fn syntax_set() -> &'static SyntaxSet {
    static SS: OnceLock<SyntaxSet> = OnceLock::new();
    SS.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn syntect_theme() -> &'static SyntectTheme {
    static TS: OnceLock<ThemeSet> = OnceLock::new();
    &TS.get_or_init(ThemeSet::load_defaults)
        .themes["base16-ocean.dark"]
}

/// 将 Markdown 文本渲染为 ratatui Line 列表
pub fn render_markdown(text: &str, width: u16) -> Vec<Line<'static>> {
    // 从 tui.rs 的 render_markdown_line 函数迁移完整逻辑
    // 包括：标题、粗体、斜体、代码块、链接、列表等
    // 这里保留原有实现
    let theme = &*THEME;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current_spans: Vec<Span<'static>> = Vec::new();
    let mut in_code_block = false;
    let mut code_lang = String::new();
    let mut code_lines: Vec<String> = Vec::new();
    let mut highlighter: Option<HighlightLines> = None;

    let parser = Parser::new_ext(text, Options::all());
    for event in parser {
        // ... 从 tui.rs 完整迁移
    }
    if !current_spans.is_empty() {
        lines.push(Line::from(current_spans));
    }
    lines
}
```

注意：`markdown.rs` 的完整实现需要从 `tui.rs` 的 `render_markdown_line` 函数（约 200 行）完整迁移。这是一个较大的步骤。

- [ ] **Step 3: 验证编译**

```bash
cargo check -p zerobot-cli 2>&1 | head -30
```

- [ ] **Step 4: 提交**

```bash
git add crates/zerobot-cli/src/tui/overlay.rs crates/zerobot-cli/src/tui/markdown.rs
git commit -m "feat(tui): 添加 Overlay 系统和 Markdown 渲染模块"
```

---

## Task 4: 创建快捷键系统

**Files:**
- Create: `crates/zerobot-cli/src/tui/keybindings/types.rs`
- Create: `crates/zerobot-cli/src/tui/keybindings/default_bindings.rs`
- Create: `crates/zerobot-cli/src/tui/keybindings/mod.rs`

- [ ] **Step 1: 创建 types.rs**

```rust
// crates/zerobot-cli/src/tui/keybindings/types.rs
use crossterm::event::{KeyCode, KeyModifiers, KeyEvent};
use std::fmt;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyContext {
    Global,
    Chat,
    Autocomplete,
    Confirmation,
    HistorySearch,
    Scroll,
    MessageSelector,
    Help,
    Select,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyAction {
    Interrupt,
    Exit,
    Redraw,
    ToggleTodos,
    ToggleTranscript,
    CycleMode,
    ShowHelp,
    Cancel,
    Submit,
    Undo,
    ExternalEditor,
    Stash,
    ImagePaste,
    HistoryPrevious,
    HistoryNext,
    HistorySearch,
    PageUp,
    PageDown,
    ScrollToTop,
    ScrollToBottom,
    LineUp,
    LineDown,
    CopySelection,
    AutocompleteAccept,
    AutocompleteDismiss,
    AutocompletePrevious,
    AutocompleteNext,
    ConfirmYes,
    ConfirmNo,
    ConfirmPrevious,
    ConfirmNext,
    ConfirmToggle,
    ConfirmNextField,
    SelectorUp,
    SelectorDown,
    SelectorTop,
    SelectorBottom,
    SelectorSelect,
    SelectPrevious,
    SelectNext,
    SelectAccept,
    SelectCancel,
    Custom(String),
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct KeyCombo {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl KeyCombo {
    pub fn from_event(key: KeyEvent) -> Self {
        Self {
            code: key.code,
            modifiers: key.modifiers,
        }
    }

    pub fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self { code, modifiers }
    }

    pub fn is_chord_prefix(&self, other: &KeyCombo) -> bool {
        // 检查 other 是否可能是和弦的第二个键
        // 前缀键通常是一个特殊的 Ctrl+X 组合
        false // 简化版，完整实现在 mod.rs
    }
}

impl fmt::Display for KeyCombo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.modifiers.contains(KeyModifiers::CONTROL) {
            write!(f, "Ctrl+")?;
        }
        if self.modifiers.contains(KeyModifiers::SHIFT) {
            write!(f, "Shift+")?;
        }
        if self.modifiers.contains(KeyModifiers::ALT) {
            write!(f, "Alt+")?;
        }
        match self.code {
            KeyCode::Char(c) => write!(f, "{}", c),
            KeyCode::Enter => write!(f, "Enter"),
            KeyCode::Esc => write!(f, "Esc"),
            KeyCode::Tab => write!(f, "Tab"),
            KeyCode::BackTab => write!(f, "Shift+Tab"),
            KeyCode::Up => write!(f, "Up"),
            KeyCode::Down => write!(f, "Down"),
            KeyCode::Left => write!(f, "Left"),
            KeyCode::Right => write!(f, "Right"),
            KeyCode::Home => write!(f, "Home"),
            KeyCode::End => write!(f, "End"),
            KeyCode::PageUp => write!(f, "PageUp"),
            KeyCode::PageDown => write!(f, "PageDown"),
            KeyCode::Backspace => write!(f, "Backspace"),
            KeyCode::Delete => write!(f, "Delete"),
            KeyCode::F(n) => write!(f, "F{}", n),
            _ => write!(f, "{:?}", self.code),
        }
    }
}

pub struct ChordState {
    pub prefix: KeyCombo,
    pub timestamp: Instant,
}
```

- [ ] **Step 2: 创建 default_bindings.rs**

```rust
// crates/zerobot-cli/src/tui/keybindings/default_bindings.rs
use crossterm::event::{KeyCode, KeyModifiers};
use std::collections::HashMap;
use super::types::{KeyContext, KeyAction, KeyCombo};

pub fn default_bindings() -> HashMap<KeyContext, HashMap<KeyCombo, KeyAction>> {
    let mut bindings = HashMap::new();

    // Global
    let mut global = HashMap::new();
    global.insert(KeyCombo::new(KeyCode::Char('c'), KeyModifiers::CONTROL), KeyAction::Interrupt);
    global.insert(KeyCombo::new(KeyCode::Char('d'), KeyModifiers::CONTROL), KeyAction::Exit);
    global.insert(KeyCombo::new(KeyCode::Char('l'), KeyModifiers::CONTROL), KeyAction::Redraw);
    global.insert(KeyCombo::new(KeyCode::Char('t'), KeyModifiers::CONTROL), KeyAction::ToggleTodos);
    global.insert(KeyCombo::new(KeyCode::Char('o'), KeyModifiers::CONTROL), KeyAction::ToggleTranscript);
    global.insert(KeyCombo::new(KeyCode::Char('r'), KeyModifiers::CONTROL), KeyAction::HistorySearch);
    global.insert(KeyCombo::new(KeyCode::Char('h'), KeyModifiers::CONTROL), KeyAction::ShowHelp);
    bindings.insert(KeyContext::Global, global);

    // Chat
    let mut chat = HashMap::new();
    chat.insert(KeyCombo::new(KeyCode::Esc, KeyModifiers::NONE), KeyAction::Cancel);
    chat.insert(KeyCombo::new(KeyCode::Enter, KeyModifiers::NONE), KeyAction::Submit);
    chat.insert(KeyCombo::new(KeyCode::Up, KeyModifiers::NONE), KeyAction::HistoryPrevious);
    chat.insert(KeyCombo::new(KeyCode::Down, KeyModifiers::NONE), KeyAction::HistoryNext);
    chat.insert(KeyCombo::new(KeyCode::BackTab, KeyModifiers::NONE), KeyAction::CycleMode);
    chat.insert(KeyCombo::new(KeyCode::Char('_'), KeyModifiers::CONTROL), KeyAction::Undo);
    chat.insert(KeyCombo::new(KeyCode::Char('g'), KeyModifiers::CONTROL), KeyAction::ExternalEditor);
    chat.insert(KeyCombo::new(KeyCode::Char('s'), KeyModifiers::CONTROL), KeyAction::Stash);
    chat.insert(KeyCombo::new(KeyCode::Char('v'), KeyModifiers::CONTROL), KeyAction::ImagePaste);
    // 和弦: Ctrl+X Ctrl+K
    // 注意：和弦的处理在 KeybindingManager 中特殊处理
    bindings.insert(KeyContext::Chat, chat);

    // Autocomplete
    let mut ac = HashMap::new();
    ac.insert(KeyCombo::new(KeyCode::Tab, KeyModifiers::NONE), KeyAction::AutocompleteAccept);
    ac.insert(KeyCombo::new(KeyCode::Esc, KeyModifiers::NONE), KeyAction::AutocompleteDismiss);
    ac.insert(KeyCombo::new(KeyCode::Up, KeyModifiers::NONE), KeyAction::AutocompletePrevious);
    ac.insert(KeyCombo::new(KeyCode::Down, KeyModifiers::NONE), KeyAction::AutocompleteNext);
    bindings.insert(KeyContext::Autocomplete, ac);

    // Confirmation
    let mut conf = HashMap::new();
    conf.insert(KeyCombo::new(KeyCode::Char('y'), KeyModifiers::NONE), KeyAction::ConfirmYes);
    conf.insert(KeyCombo::new(KeyCode::Enter, KeyModifiers::NONE), KeyAction::ConfirmYes);
    conf.insert(KeyCombo::new(KeyCode::Char('n'), KeyModifiers::NONE), KeyAction::ConfirmNo);
    conf.insert(KeyCombo::new(KeyCode::Esc, KeyModifiers::NONE), KeyAction::ConfirmNo);
    conf.insert(KeyCombo::new(KeyCode::Up, KeyModifiers::NONE), KeyAction::ConfirmPrevious);
    conf.insert(KeyCombo::new(KeyCode::Down, KeyModifiers::NONE), KeyAction::ConfirmNext);
    conf.insert(KeyCombo::new(KeyCode::Tab, KeyModifiers::NONE), KeyAction::ConfirmNextField);
    bindings.insert(KeyContext::Confirmation, conf);

    // HistorySearch
    let mut hs = HashMap::new();
    hs.insert(KeyCombo::new(KeyCode::Esc, KeyModifiers::NONE), KeyAction::SelectAccept);
    hs.insert(KeyCombo::new(KeyCode::Enter, KeyModifiers::NONE), KeyAction::SelectAccept);
    bindings.insert(KeyContext::HistorySearch, hs);

    // Scroll
    let mut scroll = HashMap::new();
    scroll.insert(KeyCombo::new(KeyCode::PageUp, KeyModifiers::NONE), KeyAction::PageUp);
    scroll.insert(KeyCombo::new(KeyCode::PageDown, KeyModifiers::NONE), KeyAction::PageDown);
    scroll.insert(KeyCombo::new(KeyCode::Home, KeyModifiers::CONTROL), KeyAction::ScrollToTop);
    scroll.insert(KeyCombo::new(KeyCode::End, KeyModifiers::CONTROL), KeyAction::ScrollToBottom);
    bindings.insert(KeyContext::Scroll, scroll);

    // MessageSelector
    let mut sel = HashMap::new();
    sel.insert(KeyCombo::new(KeyCode::Char('j'), KeyModifiers::NONE), KeyAction::SelectorDown);
    sel.insert(KeyCombo::new(KeyCode::Char('k'), KeyModifiers::NONE), KeyAction::SelectorUp);
    sel.insert(KeyCombo::new(KeyCode::Up, KeyModifiers::NONE), KeyAction::SelectorUp);
    sel.insert(KeyCombo::new(KeyCode::Down, KeyModifiers::NONE), KeyAction::SelectorDown);
    sel.insert(KeyCombo::new(KeyCode::Up, KeyModifiers::CONTROL), KeyAction::SelectorTop);
    sel.insert(KeyCombo::new(KeyCode::Down, KeyModifiers::CONTROL), KeyAction::SelectorBottom);
    sel.insert(KeyCombo::new(KeyCode::Enter, KeyModifiers::NONE), KeyAction::SelectorSelect);
    bindings.insert(KeyContext::MessageSelector, sel);

    // Help
    let mut help = HashMap::new();
    help.insert(KeyCombo::new(KeyCode::Esc, KeyModifiers::NONE), KeyAction::Cancel);
    help.insert(KeyCombo::new(KeyCode::Char('q'), KeyModifiers::NONE), KeyAction::Cancel);
    bindings.insert(KeyContext::Help, help);

    // Select
    let mut select = HashMap::new();
    select.insert(KeyCombo::new(KeyCode::Up, KeyModifiers::NONE), KeyAction::SelectPrevious);
    select.insert(KeyCombo::new(KeyCode::Down, KeyModifiers::NONE), KeyAction::SelectNext);
    select.insert(KeyCombo::new(KeyCode::Enter, KeyModifiers::NONE), KeyAction::SelectAccept);
    select.insert(KeyCombo::new(KeyCode::Esc, KeyModifiers::NONE), KeyAction::SelectCancel);
    bindings.insert(KeyContext::Select, select);

    bindings
}
```

- [ ] **Step 3: 创建 mod.rs (KeybindingManager)**

```rust
// crates/zerobot-cli/src/tui/keybindings/mod.rs
pub mod types;
pub mod default_bindings;

use crossterm::event::KeyEvent;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use types::{KeyContext, KeyAction, KeyCombo, ChordState};

pub struct KeybindingManager {
    bindings: HashMap<KeyContext, HashMap<KeyCombo, KeyAction>>,
    chord_state: Option<ChordState>,
    chord_timeout: Duration,
    // 和弦前缀映射：(prefix_combo, second_combo) -> action
    chords: HashMap<(KeyCombo, KeyCombo), KeyAction>,
}

impl KeybindingManager {
    pub fn with_defaults() -> Self {
        let mut manager = Self {
            bindings: default_bindings::default_bindings(),
            chord_state: None,
            chord_timeout: Duration::from_millis(900),
            chords: HashMap::new(),
        };
        // 注册和弦：Ctrl+X Ctrl+K -> Interrupt (KillAgents)
        manager.chords.insert(
            (
                KeyCombo::new(crossterm::event::KeyCode::Char('x'), crossterm::event::KeyModifiers::CONTROL),
                KeyCombo::new(crossterm::event::KeyCode::Char('k'), crossterm::event::KeyModifiers::CONTROL),
            ),
            KeyAction::Interrupt,
        );
        manager
    }

    pub fn resolve(&mut self, key: KeyEvent, active_contexts: &[KeyContext]) -> Option<KeyAction> {
        let combo = KeyCombo::from_event(key);

        // 1. 检查是否在和弦序列中
        if let Some(ref chord) = self.chord_state {
            if chord.timestamp.elapsed() > self.chord_timeout {
                self.chord_state = None;
            } else {
                // 查找完整的和弦
                let full_key = (chord.prefix.clone(), combo.clone());
                if let Some(action) = self.chords.get(&full_key) {
                    self.chord_state = None;
                    return Some(action.clone());
                }
                self.chord_state = None;
                return None; // 和弦不匹配
            }
        }

        // 2. 检查普通按键（按上下文优先级，后添加的优先）
        for ctx in active_contexts.iter().rev() {
            if let Some(bindings) = self.bindings.get(ctx) {
                // 检查是否是和弦前缀
                let is_prefix = self.chords.keys().any(|(prefix, _)| *prefix == combo);
                if is_prefix {
                    self.chord_state = Some(ChordState {
                        prefix: combo,
                        timestamp: Instant::now(),
                    });
                    return None; // 等待下一个键
                }

                if let Some(action) = bindings.get(&combo) {
                    return Some(action.clone());
                }
            }
        }

        None
    }

    pub fn is_chord_prefix(&self, combo: &KeyCombo) -> bool {
        self.chords.keys().any(|(prefix, _)| prefix == combo)
    }
}
```

- [ ] **Step 4: 验证编译**

```bash
cargo check -p zerobot-cli 2>&1 | head -30
```

- [ ] **Step 5: 提交**

```bash
git add crates/zerobot-cli/src/tui/keybindings/
git commit -m "feat(tui): 添加上下文/和弦快捷键系统"
```

---

## Task 5: 创建布局系统

**Files:**
- Create: `crates/zerobot-cli/src/tui/layout/mod.rs`
- Create: `crates/zerobot-cli/src/tui/layout/scroll_box.rs`
- Create: `crates/zerobot-cli/src/tui/layout/bottom_area.rs`
- Create: `crates/zerobot-cli/src/tui/layout/modal_overlay.rs`

- [ ] **Step 1: 创建 scroll_box.rs**

```rust
// crates/zerobot-cli/src/tui/layout/scroll_box.rs
use ratatui::layout::Rect;

pub struct ScrollBoxState {
    pub offset: u16,
    pub total_lines: u16,
    pub viewport_height: u16,
    pub sticky: bool,
}

impl ScrollBoxState {
    pub fn new() -> Self {
        Self {
            offset: 0,
            total_lines: 0,
            viewport_height: 0,
            sticky: true,
        }
    }

    pub fn scroll_down(&mut self, lines: u16) {
        self.sticky = false;
        self.offset = self.offset.saturating_add(lines)
            .min(self.total_lines.saturating_sub(self.viewport_height));
        if self.offset >= self.total_lines.saturating_sub(self.viewport_height) {
            self.sticky = true;
        }
    }

    pub fn scroll_up(&mut self, lines: u16) {
        self.sticky = false;
        self.offset = self.offset.saturating_sub(lines);
    }

    pub fn scroll_to_top(&mut self) {
        self.sticky = false;
        self.offset = 0;
    }

    pub fn scroll_to_bottom(&mut self) {
        self.sticky = true;
        self.offset = self.total_lines.saturating_sub(self.viewport_height);
    }

    pub fn stick_to_bottom(&mut self) {
        self.sticky = true;
        self.offset = self.total_lines.saturating_sub(self.viewport_height);
    }

    pub fn visible_range(&self) -> (u16, u16) {
        let start = self.offset;
        let end = (self.offset + self.viewport_height).min(self.total_lines);
        (start, end)
    }
}
```

- [ ] **Step 2: 创建 bottom_area.rs**

```rust
// crates/zerobot-cli/src/tui/layout/bottom_area.rs
use ratatui::layout::Rect;
use ratatui::buffer::Buffer;
use crate::tui::app::AppState;
use crate::tui::theme::THEME;

pub struct BottomArea;

impl BottomArea {
    pub fn render(buf: &mut Buffer, area: Rect, state: &AppState) {
        // 渲染 spinner + input line + slash suggestions
        // 完整实现在后续 Task
    }

    pub fn height_needed(state: &AppState) -> u16 {
        match &state.status {
            super::super::app::Status::Idle => 3,
            super::super::app::Status::Thinking => 4,
            _ => 4,
        }
    }
}
```

- [ ] **Step 3: 创建 modal_overlay.rs**

```rust
// crates/zerobot-cli/src/tui/layout/modal_overlay.rs
use ratatui::layout::Rect;
use ratatui::buffer::Buffer;
use ratatui::widgets::{Block, Borders, BorderType, Clear, Paragraph};
use ratatui::style::Style;
use ratatui::text::Text;
use crate::tui::app::AppState;
use crate::tui::theme::THEME;

pub struct ModalOverlay;

impl ModalOverlay {
    pub fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
        let vertical = ratatui::layout::Layout::default()
            .direction(ratatui::layout::Direction::Vertical)
            .constraints([
                ratatui::layout::Constraint::Percentage((100 - percent_y) / 2),
                ratatui::layout::Constraint::Percentage(percent_y),
                ratatui::layout::Constraint::Percentage((100 - percent_y) / 2),
            ])
            .split(area);
        let horizontal = ratatui::layout::Layout::default()
            .direction(ratatui::layout::Direction::Horizontal)
            .constraints([
                ratatui::layout::Constraint::Percentage((100 - percent_x) / 2),
                ratatui::layout::Constraint::Percentage(percent_x),
                ratatui::layout::Constraint::Percentage((100 - percent_x) / 2),
            ])
            .split(vertical[1]);
        horizontal[1]
    }

    pub fn render_modal_divider(buf: &mut Buffer, area: Rect) {
        let theme = &*THEME;
        let divider = "▔".repeat(area.width as usize);
        buf.set_string(area.x, area.y, &divider, Style::default().fg(theme.modal_divider));
    }
}
```

- [ ] **Step 4: 创建 mod.rs (FullscreenLayout)**

```rust
// crates/zerobot-cli/src/tui/layout/mod.rs
pub mod scroll_box;
pub mod bottom_area;
pub mod modal_overlay;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::buffer::Buffer;
use crate::tui::app::AppState;
use crate::tui::theme::THEME;

pub struct LayoutAreas {
    pub sticky_prompt: Rect,
    pub scroll_box: Rect,
    pub bottom_area: Rect,
    pub status_bar: Rect,
    pub modal_overlay: Option<Rect>,
}

pub struct FullscreenLayout;

impl FullscreenLayout {
    pub fn compute(area: Rect, state: &AppState) -> LayoutAreas {
        let status_bar_height = 1;
        let bottom_max = (area.height / 2).max(3);
        let bottom_height = bottom_area::BottomArea::height_needed(state).min(bottom_max);
        let has_sticky = state.scroll > 0;
        let sticky_height: u16 = if has_sticky { 1 } else { 0 };
        let scroll_height = area.height
            .saturating_sub(status_bar_height)
            .saturating_sub(bottom_height)
            .saturating_sub(sticky_height);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(
                [
                    Constraint::Length(sticky_height),
                    Constraint::Length(scroll_height),
                    Constraint::Length(bottom_height),
                    Constraint::Length(status_bar_height),
                ]
                .as_ref(),
            )
            .split(area);

        LayoutAreas {
            sticky_prompt: if has_sticky { chunks[0] } else { Rect::new(0, 0, 0, 0) },
            scroll_box: chunks[if has_sticky { 1 } else { 0 }],
            bottom_area: chunks[if has_sticky { 2 } else { 1 }],
            status_bar: chunks[if has_sticky { 3 } else { 2 }],
            modal_overlay: if state.overlay.is_some() {
                Some(Self::centered_rect(60, 70, area))
            } else {
                None
            },
        }
    }

    fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
        modal_overlay::ModalOverlay::centered_rect(percent_x, percent_y, area)
    }
}
```

- [ ] **Step 5: 验证编译**

```bash
cargo check -p zerobot-cli 2>&1 | head -30
```

- [ ] **Step 6: 提交**

```bash
git add crates/zerobot-cli/src/tui/layout/
git commit -m "feat(tui): 添加 FullscreenLayout 布局系统"
```

---

## Task 6: 创建核心组件

**Files:**
- Create: `crates/zerobot-cli/src/tui/components/mod.rs`
- Create: `crates/zerobot-cli/src/tui/components/input_line.rs`
- Create: `crates/zerobot-cli/src/tui/components/messages.rs`
- Create: `crates/zerobot-cli/src/tui/components/message_item.rs`
- Create: `crates/zerobot-cli/src/tui/components/spinner.rs`
- Create: `crates/zerobot-cli/src/tui/components/status_bar.rs`

- [ ] **Step 1: 创建 input_line.rs**

从 `tui.rs` 提取输入行渲染和光标处理：

```rust
// crates/zerobot-cli/src/tui/components/input_line.rs
use ratatui::layout::Rect;
use ratatui::buffer::Buffer;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_width::UnicodeWidthStr;
use crate::tui::app::AppState;
use crate::tui::theme::THEME;

pub struct InputLine;

impl InputLine {
    pub fn render(buf: &mut Buffer, area: Rect, state: &AppState) {
        let theme = &*THEME;
        let block = Block::default()
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_style(Style::default().fg(theme.panel_border));

        let inner = block.inner(area);
        block.render(area, buf);

        let line = Line::from(vec![
            Span::styled(">", Style::default().fg(theme.input_prompt)),
            Span::raw(" "),
            Span::styled(&state.input, Style::default().fg(theme.text)),
        ]);

        let para = Paragraph::new(line);
        para.render(inner, buf);

        // 设置光标位置
        let cursor_offset = UnicodeWidthStr::width(
            state.input.chars().take(state.cursor).collect::<String>().as_str(),
        ) as u16;
        let cursor_x = inner.x + 2 + cursor_offset;
        let cursor_x = cursor_x.min(inner.x + inner.width.saturating_sub(1));
        buf.set_cursor(cursor_x, inner.y);
    }
}
```

- [ ] **Step 2: 创建 messages.rs**

从 `tui.rs` 提取消息列表的虚拟化渲染：

```rust
// crates/zerobot-cli/src/tui/components/messages.rs
use ratatui::layout::Rect;
use ratatui::buffer::Buffer;
use crate::tui::app::{AppState, OutputItem};

pub struct Messages;

impl Messages {
    pub fn render(buf: &mut Buffer, area: Rect, state: &AppState) {
        let all_lines = Self::collect_all_lines(state, area.width);
        let total = all_lines.len();
        let visible = area.height as usize;

        // 计算滚动
        let max_scroll = total.saturating_sub(visible);
        let scroll = if state.stick_to_bottom {
            max_scroll
        } else {
            (state.scroll as usize).min(max_scroll)
        };

        // 渲染可见行
        for (i, line) in all_lines.iter().skip(scroll).take(visible).enumerate() {
            let y = area.y + i as u16;
            buf.set_line(area.x, y, line, area.width);
        }
    }

    fn collect_all_lines(state: &AppState, width: u16) -> Vec<ratatui::text::Line<'static>> {
        // 从 tui.rs 的 display_lines 函数迁移
        // 这里保留原有的渲染逻辑
        Vec::new() // 占位
    }
}
```

- [ ] **Step 3: 创建 status_bar.rs**

从 `tui.rs` 的 `build_status_bar` 迁移：

```rust
// crates/zerobot-cli/src/tui/components/status_bar.rs
use ratatui::layout::Rect;
use ratatui::buffer::Buffer;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use crate::tui::app::AppState;
use crate::tui::theme::THEME;
use zerobot_core::config::PermissionMode;

pub struct StatusBar;

impl StatusBar {
    pub fn render(buf: &mut Buffer, area: Rect, state: &AppState) {
        let theme = &*THEME;
        let spans = Self::build_spans(state, theme);
        let line = Line::from(spans);
        buf.set_line(area.x, area.y, &line, area.width);
    }

    fn build_spans(state: &AppState, theme: &super::super::theme::Theme) -> Vec<Span<'static>> {
        let used = state.context_used.map(|v| v.to_string()).unwrap_or_else(|| "-".to_string());
        let limit = state.context_limit.map(|v| v.to_string()).unwrap_or_else(|| "-".to_string());
        let percent = match (state.context_used, state.context_limit) {
            (Some(used), Some(limit)) if limit > 0 => format!("{:.1}%", (used as f64 / limit as f64) * 100.0),
            _ => "-".to_string(),
        };

        let mode_label = match state.permission_mode {
            PermissionMode::Default => "",
            PermissionMode::Plan => "计划",
            PermissionMode::AcceptEdits => "自动编辑",
            PermissionMode::BypassPermissions => "绕过",
        };

        let mut spans = vec![
            Span::styled(" ZeroBot ", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)),
            Span::styled(format!("{} ", state.model), Style::default().fg(theme.text)),
        ];
        if !mode_label.is_empty() {
            spans.push(Span::styled("| ", Style::default().fg(theme.text_dim)));
            spans.push(Span::styled(format!("{} ", mode_label), Style::default().fg(theme.warn)));
        }
        spans.push(Span::styled("| ", Style::default().fg(theme.text_dim)));
        spans.push(Span::styled(format!("{used}/{limit} ({percent}) "), Style::default().fg(theme.text)));

        spans
    }
}
```

- [ ] **Step 4: 创建 spinner.rs 和 message_item.rs 占位**

```rust
// crates/zerobot-cli/src/tui/components/spinner.rs
pub struct Spinner;
impl Spinner {
    pub fn render(buf: &mut ratatui::buffer::Buffer, area: ratatui::layout::Rect, state: &crate::tui::app::AppState) {}
}

// crates/zerobot-cli/src/tui/components/message_item.rs
pub struct MessageItem;
impl MessageItem {
    pub fn render(item: &crate::tui::app::OutputItem, buf: &mut ratatui::buffer::Buffer, area: ratatui::layout::Rect) {}
}
```

- [ ] **Step 5: 创建 mod.rs**

```rust
// crates/zerobot-cli/src/tui/components/mod.rs
pub mod messages;
pub mod message_item;
pub mod input_line;
pub mod spinner;
pub mod status_bar;
pub mod tool_output;
pub mod permission_prompt;
pub mod user_input_overlay;
pub mod history_search;
pub mod slash_suggestions;
pub mod new_messages_pill;
pub mod help_overlay;
pub mod task_list;
```

- [ ] **Step 6: 验证编译**

```bash
cargo check -p zerobot-cli 2>&1 | head -30
```

- [ ] **Step 7: 提交**

```bash
git add crates/zerobot-cli/src/tui/components/
git commit -m "feat(tui): 添加核心组件（InputLine, Messages, StatusBar, Spinner）"
```

---

## Task 7: 创建事件循环和集成

**Files:**
- Modify: `crates/zerobot-cli/src/tui/mod.rs` (实现 run_tui)
- Modify: `crates/zerobot-cli/src/tui/app.rs` (完整 update 逻辑)

- [ ] **Step 1: 实现 run_tui 入口**

从 `tui.rs` 的 `run_tui` 和 `run_tui_inner` 迁移：

```rust
// crates/zerobot-cli/src/tui/mod.rs
pub mod app;
pub mod component;
pub mod message;
pub mod command;
pub mod theme;
pub mod overlay;
pub mod markdown;
pub mod layout;
pub mod components;
pub mod keybindings;

use anyhow::Result;
use crossterm::event::{Event, EventStream};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::execute;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::collections::HashSet;
use std::sync::{Arc, RwLock as StdRwLock};
use tokio::sync::mpsc;
use tokio::sync::RwLock as TokioRwLock;
use tokio::time::{self, Duration};
use tokio_stream::StreamExt;
use zerobot_core::agent::Agent;
use zerobot_core::config::{PermissionMode, Settings};
use zerobot_core::events::AgentEvent;
use zerobot_core::hooks::HookManager;
use zerobot_core::interaction::{
    InteractionHandler, ToolApprovalDecision, ToolApprovalResponse,
    UserInputResponse,
};
use zerobot_core::plugin::PluginManager;
use zerobot_core::provider::ProviderFactory;
use zerobot_core::session::SessionStore;
use zerobot_core::tool::ToolRegistry;
use zerobot_core::{Curator, SelfReviewer};

use app::{AppState, Status};
use command::Command;
use keybindings::KeybindingManager;
use layout::FullscreenLayout;
use message::Message;
use overlay::OverlayType;
use crate::slash::SlashRegistry;

// UiRequest 和 UiInteractionHandler 从 tui.rs 迁移
enum UiRequest {
    UserInput {
        request: zerobot_core::interaction::UserInputRequest,
        respond_to: tokio::sync::oneshot::Sender<zerobot_core::interaction::UserInputResponse>,
    },
    ToolApproval {
        request: zerobot_core::interaction::ToolApprovalRequest,
        respond_to: tokio::sync::oneshot::Sender<zerobot_core::interaction::ToolApprovalResponse>,
    },
    ResumeSelected { session_id: String },
    RewindSelected { message_id: String, input: String },
}

struct UiInteractionHandler {
    tx: mpsc::UnboundedSender<UiRequest>,
}

#[async_trait::async_trait]
impl InteractionHandler for UiInteractionHandler {
    async fn request_user_input(
        &self,
        request: zerobot_core::interaction::UserInputRequest,
    ) -> Result<zerobot_core::interaction::UserInputResponse, zerobot_core::ZeroBotError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.tx.send(UiRequest::UserInput { request, respond_to: tx })
            .map_err(|_| zerobot_core::ZeroBotError::Tool("无法发送用户输入请求".to_string()))?;
        rx.await.map_err(|_| zerobot_core::ZeroBotError::Tool("等待用户输入失败".to_string()))
    }

    async fn request_tool_approval(
        &self,
        request: zerobot_core::interaction::ToolApprovalRequest,
    ) -> Result<zerobot_core::interaction::ToolApprovalResponse, zerobot_core::ZeroBotError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.tx.send(UiRequest::ToolApproval { request, respond_to: tx })
            .map_err(|_| zerobot_core::ZeroBotError::Tool("无法发送授权请求".to_string()))?;
        rx.await.map_err(|_| zerobot_core::ZeroBotError::Tool("等待授权失败".to_string()))
    }
}

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
    // 终端设置
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    if use_alt_screen {
        execute!(stdout, EnterAlternateScreen)?;
    }
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_tui_inner(
        &mut terminal,
        settings, cwd, session_id, store, tools, provider_factory,
        model, provider_id, hooks, resume, use_alt_screen,
        provider_state, plugins, tool_approvals, self_reviewer, curator,
    ).await;

    // 恢复终端
    disable_raw_mode()?;
    if use_alt_screen {
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    }
    terminal.show_cursor()?;

    result
}

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
    resume: bool,
    use_alt_screen: bool,
    provider_state: Arc<StdRwLock<String>>,
    plugins: Option<Arc<PluginManager>>,
    tool_approvals: Arc<TokioRwLock<HashSet<String>>>,
    self_reviewer: Option<SelfReviewer>,
    curator: Option<Curator>,
) -> Result<String> {
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    let (ui_tx, mut ui_rx) = mpsc::unbounded_channel::<UiRequest>();

    let mut app = AppState::new(session_id, provider_id, model);
    let mut keybindings = KeybindingManager::with_defaults();
    let slash_registry = SlashRegistry::extended(/* dynamic */);

    let interaction = Arc::new(UiInteractionHandler { tx: ui_tx });
    let mut runner: Option<tokio::task::JoinHandle<()>> = None;
    let mut tick = time::interval(Duration::from_millis(50));
    let mut reader = EventStream::new();

    loop {
        tokio::select! {
            _ = tick.tick() => {
                app.tick();
            }
            Some(Ok(event)) = reader.next() => {
                match event {
                    Event::Key(key) => {
                        let ctxs = app.active_contexts();
                        if let Some(action) = keybindings.resolve(key, &ctxs) {
                            let msg = map_action_to_message(action);
                            let cmd = app.update(msg);
                            handle_cmd(cmd, &mut app, &interaction, &mut runner, &tx, &settings, &cwd, &store, &tools, &provider_factory, &hooks, &plugins, &tool_approvals, &self_reviewer, &curator, &provider_state);
                        }
                    }
                    Event::Mouse(mouse) => {
                        // 鼠标事件处理
                    }
                    Event::Resize(w, h) => {
                        app.resize(w, h);
                    }
                    _ => {}
                }
            }
            Some(event) = rx.recv() => {
                let msg = Message::from_agent_event(event);
                app.update(msg);
            }
            Some(request) = ui_rx.recv() => {
                handle_ui_request(request, &mut app);
            }
            result = async { runner.as_mut().unwrap().await }, if runner.is_some() => {
                runner = None;
                app.update(Message::AgentDone);
            }
        }

        if app.is_dirty() {
            terminal.draw(|frame| {
                let areas = FullscreenLayout::compute(frame.area(), &app);
                // 渲染各组件
                // 完整渲染逻辑在后续 Task
            })?;
            app.clear_dirty();
        }

        if app.should_quit { break; }
    }

    Ok(app.session_id.clone())
}

fn map_action_to_message(action: keybindings::types::KeyAction) -> Message {
    use keybindings::types::KeyAction;
    match action {
        KeyAction::Interrupt => Message::Interrupt,
        KeyAction::Exit => Message::Quit,
        KeyAction::Redraw => Message::Redraw,
        KeyAction::ToggleTodos => Message::Noop, // TODO
        KeyAction::ToggleTranscript => Message::Noop, // TODO
        KeyAction::CycleMode => Message::CyclePermissionMode,
        KeyAction::ShowHelp => Message::ShowHelp,
        KeyAction::Cancel => Message::CloseOverlay,
        KeyAction::Submit => Message::InputSubmit,
        _ => Message::Noop,
    }
}

fn handle_cmd(
    cmd: Command,
    app: &mut AppState,
    interaction: &Arc<UiInteractionHandler>,
    runner: &mut Option<tokio::task::JoinHandle<()>>,
    tx: &mpsc::UnboundedSender<AgentEvent>,
    settings: &Settings,
    cwd: &std::path::PathBuf,
    store: &Arc<dyn SessionStore>,
    tools: &ToolRegistry,
    provider_factory: &ProviderFactory,
    hooks: &HookManager,
    plugins: &Option<Arc<PluginManager>>,
    tool_approvals: &Arc<TokioRwLock<HashSet<String>>>,
    self_reviewer: &Option<SelfReviewer>,
    curator: &Option<Curator>,
    provider_state: &Arc<StdRwLock<String>>,
) {
    match cmd {
        Command::Quit => {
            app.should_quit = true;
        }
        Command::SpawnAgent { prompt } => {
            // 创建 Agent 并启动 run_turn
            // 从 tui.rs 的 submit 逻辑迁移
        }
        Command::ClearScreen => {
            app.output.clear();
            app.mark_dirty();
        }
        _ => {}
    }
}

fn handle_ui_request(request: UiRequest, app: &mut AppState) {
    match request {
        UiRequest::ToolApproval { request, respond_to } => {
            app.overlay = Some(OverlayType::ToolApproval(overlay::ToolApprovalOverlay {
                request,
                selected: 0,
                respond_to: Some(respond_to),
            }));
            app.mark_dirty();
        }
        UiRequest::UserInput { request, respond_to } => {
            app.overlay = Some(OverlayType::UserInput(overlay::UserInputOverlay {
                request,
                current: 0,
                selected: 0,
                focus: overlay::UserInputFocus::Options,
                notes: std::collections::HashMap::new(),
                answers: std::collections::HashMap::new(),
                respond_to: Some(respond_to),
            }));
            app.mark_dirty();
        }
        _ => {}
    }
}
```

- [ ] **Step 2: 完善 app.rs 的 update 逻辑**

在 `app.rs` 中实现完整的 `update()` 方法，处理所有 Message 变体。从 `tui.rs` 的 `handle_event` 和 `handle_agent_event` 迁移核心逻辑。

- [ ] **Step 3: 验证编译**

```bash
cargo check -p zerobot-cli 2>&1 | head -30
```

- [ ] **Step 4: 提交**

```bash
git add crates/zerobot-cli/src/tui/mod.rs crates/zerobot-cli/src/tui/app.rs
git commit -m "feat(tui): 实现事件循环和 Agent 集成"
```

---

## Task 8: 创建剩余组件和覆盖层

**Files:**
- Create: `crates/zerobot-cli/src/tui/components/tool_output.rs`
- Create: `crates/zerobot-cli/src/tui/components/permission_prompt.rs`
- Create: `crates/zerobot-cli/src/tui/components/user_input_overlay.rs`
- Create: `crates/zerobot-cli/src/tui/components/history_search.rs`
- Create: `crates/zerobot-cli/src/tui/components/slash_suggestions.rs`
- Create: `crates/zerobot-cli/src/tui/components/new_messages_pill.rs`
- Create: `crates/zerobot-cli/src/tui/components/help_overlay.rs`
- Create: `crates/zerobot-cli/src/tui/components/task_list.rs`

- [ ] **Step 1: 创建 tool_output.rs**

从 `tui.rs` 提取工具输出渲染（可折叠）：

```rust
// crates/zerobot-cli/src/tui/components/tool_output.rs
use ratatui::layout::Rect;
use ratatui::buffer::Buffer;
use crate::tui::app::AppState;

pub struct ToolOutput;

impl ToolOutput {
    pub fn render_collapsed(buf: &mut Buffer, area: Rect, name: &str, ok: bool) {
        // 渲染折叠的工具输出（单行摘要）
    }

    pub fn render_expanded(buf: &mut Buffer, area: Rect, name: &str, output: &str, ok: bool) {
        // 渲染展开的工具输出（多行）
    }
}
```

- [ ] **Step 2: 创建 permission_prompt.rs**

从 `tui.rs` 的 `render_permission_prompt` 迁移：

```rust
// crates/zerobot-cli/src/tui/components/permission_prompt.rs
use ratatui::layout::Rect;
use ratatui::buffer::Buffer;
use crate::tui::app::AppState;

pub struct PermissionPrompt;

impl PermissionPrompt {
    pub fn render(buf: &mut Buffer, area: Rect, state: &AppState) {
        // 从 tui.rs 迁移 render_permission_prompt 逻辑
    }
}
```

- [ ] **Step 3: 创建剩余覆盖层组件**

每个覆盖层组件实现 `OverlayComponent` trait，从 `tui.rs` 对应的 `handle_overlay_key` 分支迁移。

- [ ] **Step 4: 创建 slash_suggestions.rs**

从 `tui.rs` 的 slash 补全逻辑迁移：

```rust
// crates/zerobot-cli/src/tui/components/slash_suggestions.rs
use ratatui::layout::Rect;
use ratatui::buffer::Buffer;
use crate::tui::app::AppState;

pub struct SlashSuggestions;

impl SlashSuggestions {
    pub fn render(buf: &mut Buffer, area: Rect, state: &AppState) {
        if state.slash_query.is_none() { return; }
        // 渲染补全列表
    }
}
```

- [ ] **Step 5: 验证编译**

```bash
cargo check -p zerobot-cli 2>&1 | head -30
```

- [ ] **Step 6: 提交**

```bash
git add crates/zerobot-cli/src/tui/components/
git commit -m "feat(tui): 添加剩余组件（ToolOutput, PermissionPrompt, SlashSuggestions 等）"
```

---

## Task 9: 完善渲染和删除旧文件

**Files:**
- Modify: `crates/zerobot-cli/src/tui/mod.rs` (完整渲染逻辑)
- Modify: `crates/zerobot-cli/src/tui/components/messages.rs` (完整虚拟化渲染)
- Delete: `crates/zerobot-cli/src/tui.rs` (旧文件)

- [ ] **Step 1: 完善 FullscreenLayout 渲染**

在 `mod.rs` 的 `terminal.draw` 闭包中，调用所有组件的 render：

```rust
terminal.draw(|frame| {
    let areas = FullscreenLayout::compute(frame.area(), &app);

    // Sticky prompt header
    if areas.sticky_prompt.height > 0 {
        // 渲染粘性提示头
    }

    // Messages (虚拟化)
    components::messages::Messages::render(frame.buffer_mut(), areas.scroll_box, &app);

    // Bottom area
    components::spinner::Spinner::render(frame.buffer_mut(), areas.bottom_area, &app);
    components::input_line::InputLine::render(frame.buffer_mut(), areas.bottom_area, &app);

    // Status bar
    components::status_bar::StatusBar::render(frame.buffer_mut(), areas.status_bar, &app);

    // Modal overlay
    if let Some(modal_area) = areas.modal_overlay {
        layout::modal_overlay::ModalOverlay::render_modal_divider(frame.buffer_mut(), modal_area);
        // 渲染覆盖层内容
    }

    // Cursor
    let cursor_offset = unicode_width::UnicodeWidthStr::width(
        app.input.chars().take(app.cursor).collect::<String>().as_str(),
    ) as u16;
    let cursor_x = areas.bottom_area.x + 2 + cursor_offset;
    frame.set_cursor(cursor_x, areas.bottom_area.y + 1);
})?;
```

- [ ] **Step 2: 完善 messages.rs 的虚拟化渲染**

从 `tui.rs` 的 `display_lines` 完整迁移，实现 `collect_all_lines`：

```rust
impl Messages {
    fn collect_all_lines(state: &AppState, width: u16) -> Vec<ratatui::text::Line<'static>> {
        let mut lines = Vec::new();
        for item in &state.output {
            match item {
                OutputItem::Lines(l) => lines.extend(l.clone()),
                OutputItem::Block { color, text } => {
                    // 渲染带颜色的块
                }
                OutputItem::Markdown(text) => {
                    lines.extend(markdown::render_markdown(text, width));
                }
                // ... 其他 OutputItem 变体
            }
        }
        lines
    }
}
```

- [ ] **Step 3: 删除旧的 tui.rs**

```bash
rm crates/zerobot-cli/src/tui.rs
```

- [ ] **Step 4: 验证编译**

```bash
cargo check -p zerobot-cli 2>&1 | head -50
```

预期：需要修复一些导入路径问题。

- [ ] **Step 5: 修复编译错误**

逐一修复所有编译错误，确保所有功能正确迁移。

- [ ] **Step 6: 运行测试**

```bash
cargo test -p zerobot-cli 2>&1 | tail -20
```

- [ ] **Step 7: 提交**

```bash
git add -A
git commit -m "feat(tui): 完成 TUI 重设计，删除旧 tui.rs"
```

---

## Task 10: 打磨和边界情况

- [ ] **Step 1: 动画和光标闪烁**

确保光标闪烁定时器正常工作。

- [ ] **Step 2: 鼠标支持**

实现鼠标滚轮滚动。

- [ ] **Step 3: 和弦序列完整测试**

测试 `Ctrl+X Ctrl+K` 和弦是否正常工作。

- [ ] **Step 4: 所有覆盖层功能验证**

逐一验证：ToolApproval、UserInput、HistorySearch、Help、MessageSelector、TurnCost。

- [ ] **Step 5: Slash 命令补全验证**

验证 `/` 触发补全、上下选择、Tab 补全、Enter 执行。

- [ ] **Step 6: 提交**

```bash
git add -A
git commit -m "feat(tui): TUI 重设计打磨完成"
```
