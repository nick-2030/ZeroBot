# ZeroBot TUI 重设计规范

## 概述

完全重构 ZeroBot 的 TUI，参考 Claude-Code 的 TUI 设计（TypeScript/React/Ink），将其架构、布局、快捷键系统移植到 Rust/ratatui 生态。

**技术栈**：ratatui + crossterm（保持不变），在其上构建组件系统、上下文快捷键、虚拟滚动等。

**迁移策略**：完全重写，删除现有 `tui.rs`（5156 行），从零构建模块化的 `tui/` 目录。

---

## 1. 模块结构

```
crates/zerobot-cli/src/
  tui/
    mod.rs                 -- 入口：run_tui()、事件循环
    app.rs                 -- AppState：全局状态容器
    component.rs           -- Component trait 定义
    message.rs             -- Message enum：所有状态变更的消息类型
    command.rs             -- Command enum：副作用

    layout/
      mod.rs               -- FullscreenLayout 组件
      scroll_box.rs        -- ScrollBox：可滚动区域 + sticky scroll
      bottom_area.rs       -- 底部固定区域
      modal_overlay.rs     -- 模态弹窗层

    components/
      mod.rs
      messages.rs          -- 消息列表（虚拟化渲染）
      message_item.rs      -- 单条消息渲染
      input_line.rs        -- 输入行（光标、slash 补全）
      spinner.rs           -- 加载动画 + 状态指示
      status_bar.rs        -- 底部状态栏
      tool_output.rs       -- 工具调用输出（可折叠）
      permission_prompt.rs -- 权限审批弹窗
      user_input_overlay.rs -- 用户输入弹窗
      history_search.rs    -- 历史搜索覆盖层
      slash_suggestions.rs -- Slash 命令建议下拉
      new_messages_pill.rs -- "N 条新消息" 浮动提示
      help_overlay.rs      -- 快捷键帮助弹窗
      task_list.rs         -- 任务列表

    keybindings/
      mod.rs               -- KeybindingManager
      default_bindings.rs  -- 默认快捷键定义
      types.rs             -- KeyAction、KeyContext、KeyCombo

    theme.rs               -- 主题颜色定义
    markdown.rs            -- Markdown 渲染（保留现有逻辑）
    overlay.rs             -- Overlay trait 和通用覆盖层逻辑
```

---

## 2. 核心设计

### 2.1 Component Trait

```rust
pub trait Component {
    fn render(&self, area: Rect, buf: &mut Buffer, state: &AppState);
    fn handle_key(&mut self, key: KeyEvent, state: &mut AppState) -> Option<Message>;
    fn handle_mouse(&mut self, event: MouseEvent, state: &mut AppState) -> Option<Message>;
    fn is_dirty(&self) -> bool;
    fn clear_dirty(&mut self);
}
```

组件返回 `Option<Message>` 而非直接修改状态。消息由上层 `App::update()` 统一处理。

### 2.2 Message 枚举

```rust
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
    AgentDone,
    AgentError(String),
    SessionCost { input: u64, output: u64, cache_create: u64, cache_read: u64 },
    ContextUsage { used: usize, limit: Option<u32> },

    // 覆盖层
    ShowOverlay(OverlayType),
    CloseOverlay,
    OverlaySelect(usize),
    OverlayConfirm,
    OverlayCancel,

    // Slash
    SlashInput(char),
    SlashBackspace,
    SlashSelect(usize),
    SlashExecute(String),

    // 历史
    HistorySearch(String),
    HistorySelect(usize),

    Noop,
}
```

### 2.3 Command 枚举

```rust
pub enum Command {
    None,
    SpawnAgent { prompt: String },
    Quit,
    ClearScreen,
    CopyToClipboard(String),
    OpenExternalEditor,
}
```

`update()` 返回 `Command`，由事件循环处理副作用。

### 2.4 AppState

```rust
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
    pub scroll_offset: usize,
    pub stick_to_bottom: bool,
    pub total_lines: usize,

    // 输入
    pub input: String,
    pub cursor_pos: usize,

    // 统计
    pub usage: TokenUsage,
    pub context_used: usize,
    pub context_limit: Option<u32>,
    pub turn_costs: Vec<TurnCost>,
    pub turn_count: u32,

    // 覆盖层
    pub overlay: Option<OverlayType>,
    pub overlay_queue: VecDeque<OverlayType>,

    // Slash 补全
    pub slash_matches: Vec<SlashMatch>,
    pub slash_selected: usize,

    // 运行中工具
    pub running_tools: HashMap<String, RunningTool>,
    pub active_hooks: HashSet<String>,

    // 任务
    pub todos: Vec<Todo>,

    // 显示选项
    pub show_full_tool_output: bool,
    pub viewport_width: u16,
    pub viewport_height: u16,

    // 退出
    pub should_quit: bool,

    // 脏标记
    dirty: bool,
}
```

---

## 3. 布局系统

对标 Claude-Code 的 `FullscreenLayout`，三层结构：

```
┌──────────────────────────────────────────────┐
│  StickyPromptHeader (1行，滚动时显示)         │
├──────────────────────────────────────────────┤
│  ScrollBox (flex_grow=1, sticky_scroll)      │
│  ┌────────────────────────────────────────┐  │
│  │  Messages (虚拟化渲染)                  │  │
│  │  Overlay (权限弹窗等，在 ScrollBox 内)  │  │
│  └────────────────────────────────────────┘  │
│  [NewMessagesPill] ← 浮动提示               │
├──────────────────────────────────────────────┤
│  BottomArea (flex_shrink=0, max_height=50%)  │
│  ┌────────────────────────────────────────┐  │
│  │  Spinner (加载动画)                     │  │
│  │  InputLine (输入行)                     │  │
│  │  SlashSuggestions (补全下拉)            │  │
│  └────────────────────────────────────────┘  │
├──────────────────────────────────────────────┤
│  StatusBar (1行)                             │
└──────────────────────────────────────────────┘

ModalOverlay (绝对定位，覆盖在上层)
```

### 3.1 布局计算

```rust
fn compute_layout(area: Rect, state: &AppState) -> LayoutAreas {
    // 从下往上分配：固定高度优先
    let status_bar_height = 1;
    let bottom_max = (area.height / 2).max(3);
    let bottom_height = calculate_bottom_height(state).min(bottom_max);
    let sticky_height = if state.scroll_offset > 0 { 1 } else { 0 };
    let scroll_height = area.height - status_bar_height - bottom_height - sticky_height;

    LayoutAreas { sticky_prompt, scroll_box, new_messages_pill, bottom_area, status_bar, modal_overlay }
}
```

### 3.2 ScrollBox

- **Sticky scroll**：`stick_to_bottom = true` 时新消息自动滚到底部
- **虚拟化渲染**：只渲染可见行 ± 缓冲区
- **滚动控制**：PageUp/PageDown/鼠标滚轮/Home/End

```rust
pub struct ScrollBox {
    pub offset: usize,
    pub total_lines: usize,
    pub viewport_height: u16,
    pub sticky: bool,
}
```

### 3.3 NewMessagesPill

当用户向上滚动时，底部居中显示 "N 条新消息" 提示。滚到底部后消失。

---

## 4. 快捷键系统

### 4.1 KeyContext

```rust
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
```

### 4.2 KeyAction

```rust
pub enum KeyAction {
    Interrupt, Exit, Redraw, ToggleTodos, ToggleTranscript, CycleMode, ShowHelp,
    Cancel, Submit, Undo, ExternalEditor, Stash, ImagePaste,
    HistoryPrevious, HistoryNext, HistorySearch,
    PageUp, PageDown, ScrollToTop, ScrollToBottom, LineUp, LineDown, CopySelection,
    AutocompleteAccept, AutocompleteDismiss, AutocompletePrevious, AutocompleteNext,
    ConfirmYes, ConfirmNo, ConfirmPrevious, ConfirmNext, ConfirmToggle, ConfirmNextField,
    SelectorUp, SelectorDown, SelectorTop, SelectorBottom, SelectorSelect,
    SelectPrevious, SelectNext, SelectAccept, SelectCancel,
    Custom(String),
}
```

### 4.3 和弦序列

支持 `Ctrl+X Ctrl+K` 这样的多键组合。超时 900ms 自动重置。

```rust
pub struct KeybindingManager {
    bindings: HashMap<KeyContext, HashMap<KeyCombo, KeyAction>>,
    chord_state: Option<ChordState>,
    chord_timeout: u64, // 900ms
}
```

### 4.4 默认快捷键

| 上下文 | 按键 | 动作 |
|--------|------|------|
| Global | Ctrl+C | Interrupt（保留） |
| Global | Ctrl+D | Exit（保留） |
| Global | Ctrl+L | Redraw |
| Global | Ctrl+T | ToggleTodos |
| Global | Ctrl+O | ToggleTranscript |
| Global | Ctrl+R | HistorySearch |
| Chat | Esc | Cancel |
| Chat | Ctrl+X Ctrl+K | KillAgents（和弦） |
| Chat | Shift+Tab | CycleMode |
| Chat | Enter | Submit |
| Chat | Up | HistoryPrevious |
| Chat | Down | HistoryNext |
| Chat | Ctrl+_ | Undo |
| Chat | Ctrl+G | ExternalEditor |
| Chat | Ctrl+S | Stash |
| Chat | Ctrl+V | ImagePaste |
| Autocomplete | Tab | Accept |
| Autocomplete | Esc | Dismiss |
| Autocomplete | Up/Down | Previous/Next |
| Confirmation | Y/Enter | Yes |
| Confirmation | N/Esc | No |
| Confirmation | Up/Down | Previous/Next |
| Confirmation | Tab | NextField |
| Scroll | PageUp/Down | PageUp/Down |
| Scroll | WheelUp/Down | LineUp/Down |
| Scroll | Ctrl+Home/End | Top/Bottom |
| HistorySearch | Esc | Accept |
| HistorySearch | Enter | Execute |
| MessageSelector | J/K/Up/Down | Navigate |
| MessageSelector | Ctrl+Up/Down | Top/Bottom |
| MessageSelector | Enter | Select |

---

## 5. 主题系统

```rust
pub struct Theme {
    pub panel_bg: Color,          // (32, 36, 44)
    pub panel_border: Color,      // (70, 76, 88)
    pub text: Color,              // (220, 224, 232)
    pub text_dim: Color,          // (136, 142, 156)
    pub accent: Color,            // (186, 148, 255)
    pub success: Color,           // (124, 216, 168)
    pub error: Color,             // (236, 112, 104)
    pub warn: Color,              // (234, 196, 118)
    pub thinking: Color,          // (100, 100, 120)
    pub tool_border: Color,       // (80, 90, 110)
    pub permission: Color,        // (100, 149, 237)
    pub plan_mode: Color,         // (0, 191, 165)
    pub user_message_bg: Color,   // (38, 42, 52)
    pub selected_bg: Color,       // (48, 52, 64)
    pub input_prompt: Color,      // accent
    pub status_bg: Color,         // panel_bg
    pub modal_divider: Color,     // permission
}
```

完全匹配现有 `tui.rs` 的颜色常量。

---

## 6. 渲染管线

保持 ratatui 的即时模式渲染，脏标记驱动：

```
事件 → update(msg) → mark_dirty()
                          ↓
                     is_dirty()?
                          ↓ yes
                     terminal.draw(|frame| {
                         layout.render(frame, &app);
                     })
                          ↓
                     clear_dirty()
```

### 6.1 虚拟化渲染

消息列表只渲染可见行：

```rust
fn render_messages_virtualized(
    output: &[OutputItem],
    scroll_offset: usize,
    viewport_height: usize,
    buf: &mut Buffer,
    area: Rect,
) {
    let (start_item, start_line) = find_item_at_line(output, scroll_offset);
    let mut y = area.y;
    let mut line_in_item = start_line;

    for item in &output[start_item..] {
        let lines = item.rendered_lines();
        for line in &lines[line_in_item..] {
            if y >= area.y + area.height { return; }
            render_line(buf, (area.x, y), line, area.width);
            y += 1;
        }
        line_in_item = 0;
    }
}
```

### 6.2 Sticky Scroll

```rust
impl AppState {
    pub fn on_new_output(&mut self) {
        self.recalculate_total_lines();
        if self.stick_to_bottom {
            self.scroll_offset = self.total_lines
                .saturating_sub(self.viewport_height as usize);
        }
        self.mark_dirty();
    }

    pub fn user_scroll(&mut self, delta: i32) {
        self.stick_to_bottom = false;
        self.scroll_offset = (self.scroll_offset as i32 + delta)
            .max(0)
            .min(self.total_lines.saturating_sub(self.viewport_height as usize) as i32) as usize;
        if self.scroll_offset >= self.total_lines.saturating_sub(self.viewport_height as usize) {
            self.stick_to_bottom = true;
        }
        self.mark_dirty();
    }
}
```

---

## 7. 事件循环

```rust
pub async fn run_tui_inner(...) -> Result<String> {
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    let (ui_tx, mut ui_rx) = mpsc::unbounded_channel::<UiRequest>();

    let mut app = AppState::new(...);
    let mut keybindings = KeybindingManager::with_defaults();
    let mut terminal = setup_terminal(use_alt_screen)?;

    let interaction = Arc::new(UiInteractionHandler { tx: ui_tx });
    let mut runner: Option<JoinHandle<()>> = None;
    let mut tick = interval(Duration::from_millis(50));

    loop {
        tokio::select! {
            _ = tick.tick() => { app.tick(); }
            Some(Ok(event)) = reader.next() => {
                match event {
                    Event::Key(key) => {
                        let ctxs = app.active_contexts();
                        if let Some(action) = keybindings.resolve(key, &ctxs) {
                            let cmd = app.update(Message::from_action(action));
                            handle_cmd(cmd, &mut app, &interaction, &mut runner, &tx);
                        } else if let Some(msg) = app.default_key_handler(key) {
                            let cmd = app.update(msg);
                            handle_cmd(cmd, &mut app, &interaction, &mut runner, &tx);
                        }
                    }
                    Event::Mouse(mouse) => { app.handle_mouse(mouse); }
                    Event::Resize(w, h) => { app.resize(w, h); }
                    _ => {}
                }
            }
            Some(event) = rx.recv() => {
                app.update(Message::from_agent_event(event));
            }
            Some(request) = ui_rx.recv() => {
                handle_ui_request(request, &mut app);
            }
            result = async { runner.as_mut().unwrap().await }, if runner.is_some() => {
                runner = None;
                app.on_runner_done(result);
            }
        }

        if app.is_dirty() {
            terminal.draw(|frame| {
                let layout = FullscreenLayout::new(frame.area(), &app);
                layout.render(frame, &app);
            })?;
            app.clear_dirty();
        }

        if app.should_quit { break; }
    }

    restore_terminal(terminal)?;
    Ok(app.session_id.clone())
}
```

---

## 8. Overlay 系统

```rust
pub enum OverlayType {
    ToolApproval(ToolApprovalOverlay),
    UserInput(UserInputOverlay),
    HistorySearch(HistorySearchOverlay),
    Help(HelpOverlay),
    MessageSelector(MessageSelectorOverlay),
    TurnCost(TurnCostOverlay),
}

pub trait OverlayComponent {
    fn render(&self, area: Rect, buf: &mut Buffer, theme: &Theme);
    fn handle_key(&mut self, key: KeyEvent) -> Option<Message>;
    fn height_needed(&self, width: u16) -> u16;
}
```

覆盖层以队列管理：同时只能显示一个，其余排队。

---

## 9. 与外部接口的集成

### 9.1 AgentEvent → Message

```rust
impl Message {
    pub fn from_agent_event(event: AgentEvent) -> Self {
        match event {
            AgentEvent::AssistantDelta { content } => Message::AgentDelta(content),
            AgentEvent::ToolCallStarted { tool_call_id, name, input } =>
                Message::ToolStarted { id: tool_call_id, name, input },
            AgentEvent::Done => Message::AgentDone,
            AgentEvent::SessionCost { input_tokens, output_tokens, .. } =>
                Message::SessionCost { input: input_tokens, output: output_tokens, .. },
            // ...
        }
    }
}
```

### 9.2 InteractionHandler

保持现有的 `UiInteractionHandler` 模式，通过 `UiRequest` channel 桥接 agent 线程和 UI 线程。

### 9.3 Slash 命令

保留 `slash.rs` 的 `SlashRegistry`，TUI 层只负责 UI 渲染和交互。

---

## 10. 实现步骤（概要）

1. **Phase 1：骨架** - 创建 `tui/` 目录结构，定义 Component trait、Message、Command、AppState
2. **Phase 2：布局** - 实现 FullscreenLayout、ScrollBox、BottomArea、StatusBar
3. **Phase 3：核心组件** - InputLine、Messages（虚拟化）、Spinner
4. **Phase 4：快捷键** - 实现 KeybindingManager 和默认绑定
5. **Phase 5：覆盖层** - ToolApproval、UserInput、HistorySearch、Help
6. **Phase 6：高级特性** - NewMessagesPill、StickyPromptHeader、主题系统
7. **Phase 7：集成** - AgentEvent 映射、InteractionHandler、Slash 命令
8. **Phase 8：打磨** - 动画、鼠标支持、边角情况

---

## 11. 验收标准

- [ ] 布局与 Claude-Code 的 FullscreenLayout 一致
- [ ] 所有现有快捷键正常工作
- [ ] 支持和弦序列（Ctrl+X Ctrl+K）
- [ ] 上下文感知的快捷键映射
- [ ] 虚拟化渲染，大输出不卡顿
- [ ] Sticky scroll 正常工作
- [ ] 所有覆盖层（权限、用户输入、历史搜索、帮助）正常
- [ ] Slash 命令补全正常
- [ ] 主题颜色与现有一致
- [ ] Markdown 渲染保持不变
- [ ] 代码行数：每个文件 < 500 行
- [ ] 所有现有功能不丢失
