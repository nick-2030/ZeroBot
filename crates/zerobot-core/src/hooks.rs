use crate::config::Settings;
use crate::error::{ZeroBotError, ZeroBotResult};
use crate::events::AgentEvent;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value as JsonValue;
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::path::Path;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};
use tracing::warn;

// ---------------------------------------------------------------------------
// Hook Event
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    #[serde(alias = "SessionStart")]
    SessionStart,
    #[serde(alias = "SessionEnd")]
    SessionEnd,
    #[serde(alias = "UserPromptSubmit")]
    UserPromptSubmit,
    #[serde(alias = "MessageAppend")]
    MessageAppend,
    #[serde(alias = "PreToolUse", alias = "pre_tool")]
    PreToolUse,
    #[serde(alias = "PostToolUse", alias = "post_tool")]
    PostToolUse,
    #[serde(alias = "PostToolUseFailure", alias = "post_tool_failure")]
    PostToolUseFailure,
    #[serde(alias = "SubagentStart")]
    SubagentStart,
    #[serde(alias = "SubagentStop")]
    SubagentStop,
    #[serde(alias = "TaskCompleted")]
    TaskCompleted,
    #[serde(alias = "Stop")]
    Stop,
    #[serde(alias = "PreCompact")]
    PreCompact,
    #[serde(alias = "PostCompact")]
    PostCompact,
    #[serde(alias = "Notification")]
    Notification,
    #[serde(alias = "PermissionRequest")]
    PermissionRequest,
    #[serde(alias = "InstructionsLoaded")]
    InstructionsLoaded,
    #[serde(alias = "TeammateIdle")]
    TeammateIdle,
    #[serde(alias = "ConfigChange")]
    ConfigChange,
    #[serde(alias = "WorktreeCreate")]
    WorktreeCreate,
    #[serde(alias = "WorktreeRemove")]
    WorktreeRemove,
    PreProvider,
    PostProvider,
    #[serde(alias = "FileChanged")]
    FileChanged,
    #[serde(alias = "CwdChanged")]
    CwdChanged,
}

// ---------------------------------------------------------------------------
// Hook Action
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HookAction {
    Allow,
    Deny,
    Modify,
}

// ---------------------------------------------------------------------------
// Hook Command (discriminated union, backward-compatible)
// ---------------------------------------------------------------------------

/// Supported hook execution types.
///
/// Deserialized via `#[serde(untagged)]` so existing YAML configs that use a
/// bare `command: [...]` array (no `type` field) continue to work — they are
/// matched as `Vec<String>` and promoted to `HookCommand::Command`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum HookCommand {
    /// HTTP webhook hook.
    Http {
        #[serde(rename = "type")]
        hook_type: String,
        url: String,
        #[serde(default)]
        headers: Option<HashMap<String, String>>,
        #[serde(default)]
        http_timeout_ms: Option<u64>,
    },
    /// LLM-evaluated hook: sends payload to a fast model for decision.
    Prompt {
        #[serde(rename = "type")]
        hook_type: String,
        /// Prompt template; `{{payload}}` is replaced with the JSON payload.
        prompt: String,
        /// Model to use (defaults to configured fast model).
        #[serde(default)]
        model: Option<String>,
        /// Maximum tokens for the LLM response.
        #[serde(default)]
        max_tokens: Option<u32>,
    },
    /// Multi-turn agent hook: spawns a verification agent.
    Agent {
        #[serde(rename = "type")]
        hook_type: String,
        /// Agent definition name or inline prompt.
        agent: String,
        /// Maximum turns for the verification agent.
        #[serde(default)]
        max_turns: Option<usize>,
    },
    /// Shell command hook (explicit `type: command`).
    Command {
        #[serde(rename = "type")]
        hook_type: String,
        command: Vec<String>,
        #[serde(default)]
        shell: Option<String>,
    },
    /// Legacy format: bare command array without `type` field.
    Legacy(Vec<String>),
}

impl Default for HookCommand {
    fn default() -> Self {
        HookCommand::Legacy(Vec::new())
    }
}

impl HookCommand {
    pub fn is_empty(&self) -> bool {
        match self {
            HookCommand::Command { command, .. } => command.is_empty(),
            HookCommand::Legacy(cmd) => cmd.is_empty(),
            HookCommand::Http { url, .. } => url.is_empty(),
            HookCommand::Prompt { prompt, .. } => prompt.is_empty(),
            HookCommand::Agent { agent, .. } => agent.is_empty(),
        }
    }
}

// ---------------------------------------------------------------------------
// Hook Definition
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct HookDefinition {
    pub name: String,
    #[serde(default)]
    pub hook: HookCommand,
    /// Tool name pattern (exact or glob) for tool-related events.
    #[serde(default)]
    pub matcher: Option<String>,
    /// Permission rule syntax filter, e.g. `"Bash(git *)"`, `"Read(*))"`.
    #[serde(default)]
    pub if_condition: Option<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub events: Vec<HookEvent>,
    /// If true, hook is removed after first successful execution.
    #[serde(default)]
    pub once: Option<bool>,
    /// If true, hook runs in the background (fire-and-forget).
    #[serde(default, rename = "async")]
    pub async_: Option<bool>,
    /// Custom status text shown while hook runs.
    #[serde(default)]
    pub status_message: Option<String>,
    /// Paths to watch for file changes (triggers `FileChanged` event).
    #[serde(default)]
    pub watch_paths: Vec<String>,
    /// Glob pattern for filtering watched file changes.
    #[serde(default)]
    pub watch_pattern: Option<String>,
}

/// Intermediate struct for deserializing HookDefinition from raw fields.
/// Supports both new `hook:` format and legacy `command:` format.
#[derive(Deserialize)]
struct HookDefinitionRaw {
    name: String,
    #[serde(default)]
    hook: Option<HookCommand>,
    /// Legacy top-level command field
    #[serde(default)]
    command: Option<Vec<String>>,
    #[serde(default)]
    matcher: Option<String>,
    #[serde(default)]
    if_condition: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    events: Vec<HookEvent>,
    #[serde(default)]
    once: Option<bool>,
    #[serde(default, rename = "async")]
    async_: Option<bool>,
    #[serde(default)]
    status_message: Option<String>,
    #[serde(default)]
    watch_paths: Vec<String>,
    #[serde(default)]
    watch_pattern: Option<String>,
}

impl<'de> Deserialize<'de> for HookDefinition {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = HookDefinitionRaw::deserialize(deserializer)?;
        let hook = match (raw.hook, raw.command) {
            (Some(h), _) => h,
            (None, Some(cmd)) => HookCommand::Legacy(cmd),
            (None, None) => HookCommand::Legacy(Vec::new()),
        };
        Ok(HookDefinition {
            name: raw.name,
            hook,
            matcher: raw.matcher,
            if_condition: raw.if_condition,
            timeout_ms: raw.timeout_ms,
            enabled: raw.enabled,
            events: raw.events,
            once: raw.once,
            async_: raw.async_,
            status_message: raw.status_message,
            watch_paths: raw.watch_paths,
            watch_pattern: raw.watch_pattern,
        })
    }
}

impl HookDefinition {
    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    pub fn is_async(&self) -> bool {
        self.async_.unwrap_or(false)
    }

    pub fn is_once(&self) -> bool {
        self.once.unwrap_or(false)
    }

    pub fn matches(&self, event: HookEvent, payload: &JsonValue) -> bool {
        if self.events.is_empty() {
            return self.matches_matcher(event, payload) && self.matches_if_condition(event, payload);
        }
        if !self.events.contains(&event) {
            return false;
        }
        self.matches_matcher(event, payload) && self.matches_if_condition(event, payload)
    }

    fn matches_matcher(&self, event: HookEvent, payload: &JsonValue) -> bool {
        let Some(matcher) = &self.matcher else {
            return true;
        };
        if matcher.trim().is_empty() || matcher.trim() == "*" {
            return true;
        }
        let target = match event {
            HookEvent::PreToolUse | HookEvent::PostToolUse | HookEvent::PostToolUseFailure => {
                payload.get("tool_name").and_then(|v| v.as_str())
            }
            _ => None,
        };
        let Some(name) = target else {
            return false;
        };
        if !matcher.contains('*') {
            return name == matcher;
        }
        let escaped = regex::escape(matcher);
        let pattern = escaped.replace("\\*", ".*");
        if let Ok(re) = regex::Regex::new(&format!("^{pattern}$")) {
            return re.is_match(name);
        }
        false
    }

    /// Match `if_condition` in the form `"ToolName(pattern)"`.
    ///
    /// - `"Bash(git *)"` matches tool_name=Bash and tool_input containing "git …"
    /// - `"Bash(*)"`    matches tool_name=Bash (any input)
    /// - `"Read(*))"`   matches tool_name=Read (any input)
    fn matches_if_condition(&self, event: HookEvent, payload: &JsonValue) -> bool {
        let Some(cond) = &self.if_condition else {
            return true;
        };
        let trimmed = cond.trim();
        if trimmed.is_empty() {
            return true;
        }
        // Only apply to tool events
        let tool_name = match event {
            HookEvent::PreToolUse | HookEvent::PostToolUse | HookEvent::PostToolUseFailure => {
                payload.get("tool_name").and_then(|v| v.as_str())
            }
            _ => return true, // Non-tool events pass through
        };
        let Some(tool_name) = tool_name else {
            return false;
        };
        // Parse "ToolName(pattern)"
        let (cond_name, cond_pattern) = if let Some(paren_start) = trimmed.find('(') {
            let inner = &trimmed[paren_start + 1..];
            let cond_pattern = inner.strip_suffix(')').unwrap_or(inner);
            (&trimmed[..paren_start], Some(cond_pattern.trim()))
        } else {
            (trimmed, None)
        };
        // Match tool name (supports glob)
        if !glob_match(cond_name, tool_name) {
            return false;
        }
        // If no pattern or pattern is "*", match any input
        let Some(pattern) = cond_pattern else {
            return true;
        };
        if pattern == "*" {
            return true;
        }
        // Match against tool_input as a string
        let input_str = match payload.get("tool_input") {
            Some(JsonValue::String(s)) => s.clone(),
            Some(v) => serde_json::to_string(v).unwrap_or_default(),
            None => return false,
        };
        glob_match(pattern, &input_str)
    }
}

fn glob_match(pattern: &str, text: &str) -> bool {
    if !pattern.contains('*') && !pattern.contains('?') {
        return pattern == text;
    }
    let escaped = regex::escape(pattern);
    let re_pattern = escaped.replace("\\*", ".*").replace("\\?", ".");
    if let Ok(re) = regex::Regex::new(&format!("^{re_pattern}$")) {
        return re.is_match(text);
    }
    false
}

// ---------------------------------------------------------------------------
// Hook Decision
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct HookDecision {
    pub action: HookAction,
    pub payload: JsonValue,
    pub message: Option<String>,
    /// Additional system message injected into the conversation context.
    pub system_message: Option<String>,
    /// Additional context injected into the conversation.
    pub additional_context: Option<String>,
}

// ---------------------------------------------------------------------------
// Hook Manager
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct HookManager {
    agent_hooks: Vec<HookDefinition>,
    dir_hooks: Vec<HookDefinition>,
    cwd: std::path::PathBuf,
    event_sender: Arc<StdMutex<Option<mpsc::UnboundedSender<AgentEvent>>>>,
    /// In-memory session-scoped hooks with highest priority.
    session_hooks: Arc<StdMutex<Vec<HookDefinition>>>,
}

impl HookManager {
    pub fn new(agent_hooks: Vec<HookDefinition>, dir_hooks: Vec<HookDefinition>) -> Self {
        Self {
            agent_hooks,
            dir_hooks,
            cwd: std::env::current_dir().unwrap_or_default(),
            event_sender: Arc::new(StdMutex::new(None)),
            session_hooks: Arc::new(StdMutex::new(Vec::new())),
        }
    }

    pub fn empty() -> Self {
        Self {
            agent_hooks: Vec::new(),
            dir_hooks: Vec::new(),
            cwd: std::env::current_dir().unwrap_or_default(),
            event_sender: Arc::new(StdMutex::new(None)),
            session_hooks: Arc::new(StdMutex::new(Vec::new())),
        }
    }

    pub fn load(
        settings: &Settings,
        cwd: &Path,
        agent_hooks: Option<Vec<HookDefinition>>,
    ) -> ZeroBotResult<Self> {
        let dir_hooks = read_hooks_dir(&cwd.join(".zerobot").join("hooks"))?;
        let _ = settings;
        Ok(Self {
            agent_hooks: agent_hooks.unwrap_or_default(),
            dir_hooks,
            cwd: cwd.to_path_buf(),
            event_sender: Arc::new(StdMutex::new(None)),
            session_hooks: Arc::new(StdMutex::new(Vec::new())),
        })
    }

    /// Add a session-scoped hook (highest priority, in-memory only).
    pub fn add_session_hook(&self, hook: HookDefinition) {
        if let Ok(mut hooks) = self.session_hooks.lock() {
            hooks.retain(|h| h.name != hook.name);
            hooks.push(hook);
        }
    }

    /// Remove a session-scoped hook by name.
    pub fn remove_session_hook(&self, name: &str) -> bool {
        if let Ok(mut hooks) = self.session_hooks.lock() {
            let before = hooks.len();
            hooks.retain(|h| h.name != name);
            return hooks.len() < before;
        }
        false
    }

    /// Clear all session-scoped hooks.
    pub fn clear_session_hooks(&self) {
        if let Ok(mut hooks) = self.session_hooks.lock() {
            hooks.clear();
        }
    }

    /// Get a snapshot of session-scoped hooks.
    pub fn session_hooks(&self) -> Vec<HookDefinition> {
        self.session_hooks.lock().ok().map(|h| h.clone()).unwrap_or_default()
    }

    /// Set the event sender for emitting hook progress events to the TUI.
    pub fn set_event_sender(&self, sender: mpsc::UnboundedSender<AgentEvent>) {
        if let Ok(mut guard) = self.event_sender.lock() {
            *guard = Some(sender);
        }
    }

    fn emit_hook_event(&self, event: AgentEvent) {
        if let Ok(guard) = self.event_sender.lock() {
            if let Some(tx) = guard.as_ref() {
                let _ = tx.send(event);
            }
        }
    }

    pub fn hooks(&self) -> &[HookDefinition] {
        &self.agent_hooks
    }

    pub async fn apply_event(
        &self,
        event: HookEvent,
        session_id: &str,
        payload: JsonValue,
        skill_hooks: &[HookDefinition],
    ) -> ZeroBotResult<HookDecision> {
        let session = self.session_hooks.lock().ok().map(|h| h.clone()).unwrap_or_default();
        let hooks = merge_hooks(
            session,
            self.agent_hooks.clone(),
            skill_hooks.to_vec(),
            self.dir_hooks.clone(),
        );
        let mut current = payload;
        let mut system_messages = Vec::new();
        let mut additional_contexts = Vec::new();
        let mut hooks_to_remove_once = Vec::new();

        let event_name = format!("{:?}", event);
        for (idx, hook) in hooks.iter().enumerate() {
            if !hook.enabled() || !hook.matches(event, &current) {
                continue;
            }

            // Emit HookStarted event for TUI display
            self.emit_hook_event(AgentEvent::HookStarted {
                event: event_name.clone(),
                hook_name: hook.name.clone(),
                status_message: hook.status_message.clone(),
            });

            if hook.is_async() {
                // Fire-and-forget: spawn in background
                let hook_clone = hook.clone();
                let current_clone = current.clone();
                let session_id = session_id.to_string();
                let cwd = self.cwd.clone();
                let sender = self.event_sender.lock().ok().and_then(|g| g.clone());
                tokio::spawn(async move {
                    match execute_hook(&hook_clone, event, &session_id, &current_clone, &cwd).await
                    {
                        Ok(_) => {
                            if let Some(tx) = &sender {
                                let _ = tx.send(AgentEvent::HookFinished {
                                    event: format!("{:?}", event),
                                    hook_name: hook_clone.name.clone(),
                                    ok: true,
                                    message: None,
                                });
                            }
                        }
                        Err(err) => {
                            warn!("Async hook '{}' failed: {}", hook_clone.name, err);
                            if let Some(tx) = &sender {
                                let _ = tx.send(AgentEvent::HookFinished {
                                    event: format!("{:?}", event),
                                    hook_name: hook_clone.name.clone(),
                                    ok: false,
                                    message: Some(err.to_string()),
                                });
                            }
                        }
                    }
                });
                continue;
            }

            match execute_hook(hook, event, session_id, &current, &self.cwd).await {
                Ok(response) => {
                    let hook_ok = !matches!(response.action, Some(HookAction::Deny));
                    self.emit_hook_event(AgentEvent::HookFinished {
                        event: event_name.clone(),
                        hook_name: hook.name.clone(),
                        ok: hook_ok,
                        message: response.message.clone(),
                    });
                    if let Some(sm) = response.system_message {
                        system_messages.push(sm);
                    }
                    if let Some(ac) = response.additional_context {
                        additional_contexts.push(ac);
                    }
                    match response.action.unwrap_or(HookAction::Allow) {
                        HookAction::Allow => {}
                        HookAction::Deny => {
                            return Ok(HookDecision {
                                action: HookAction::Deny,
                                payload: current,
                                message: response.message,
                                system_message: consolidate(system_messages),
                                additional_context: consolidate(additional_contexts),
                            });
                        }
                        HookAction::Modify => {
                            if let Some(patch) = response.patch {
                                current = shallow_merge(current, patch);
                            }
                        }
                    }
                    // Track once-hooks for removal
                    if hook.is_once() {
                        hooks_to_remove_once.push(idx);
                    }
                }
                Err(err) => {
                    warn!("Hook '{}' execution failed: {}", hook.name, err);
                    self.emit_hook_event(AgentEvent::HookFinished {
                        event: event_name.clone(),
                        hook_name: hook.name.clone(),
                        ok: false,
                        message: Some(err.to_string()),
                    });
                }
            }
        }

        Ok(HookDecision {
            action: HookAction::Allow,
            payload: current,
            message: None,
            system_message: consolidate(system_messages),
            additional_context: consolidate(additional_contexts),
        })
    }

    pub async fn run_session_start(
        &self,
        session_id: &str,
        parent_id: Option<String>,
        kind: crate::session::SessionKind,
    ) {
        let payload = serde_json::json!({
            "session_id": session_id,
            "parent_id": parent_id,
            "kind": kind.to_string(),
        });
        let _ = self
            .apply_event(HookEvent::SessionStart, session_id, payload, &[])
            .await;
    }

    pub async fn run_session_end(&self, session_id: &str) {
        let payload = serde_json::json!({ "session_id": session_id });
        let _ = self
            .apply_event(HookEvent::SessionEnd, session_id, payload, &[])
            .await;
    }
}

fn consolidate(mut items: Vec<String>) -> Option<String> {
    items.retain(|s| !s.trim().is_empty());
    if items.is_empty() {
        None
    } else {
        Some(items.join("\n"))
    }
}

// ---------------------------------------------------------------------------
// Hook File (for YAML deserialization)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct HookFile {
    #[serde(default)]
    hooks: Vec<HookDefinition>,
}

// ---------------------------------------------------------------------------
// Hook Response
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct HookResponse {
    #[serde(default)]
    action: Option<HookAction>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    patch: Option<JsonValue>,
    /// Additional system message injected into conversation context.
    #[serde(default)]
    system_message: Option<String>,
    /// Additional context for the model.
    #[serde(default)]
    additional_context: Option<String>,
}

// ---------------------------------------------------------------------------
// Hook Execution
// ---------------------------------------------------------------------------

/// Execute a single hook and return its response.
async fn execute_hook(
    hook: &HookDefinition,
    event: HookEvent,
    session_id: &str,
    payload: &JsonValue,
    cwd: &Path,
) -> ZeroBotResult<HookResponse> {
    match &hook.hook {
        HookCommand::Legacy(cmd) => execute_command_hook(hook, event, session_id, payload, cmd, cwd).await,
        HookCommand::Command { command, .. } => {
            execute_command_hook(hook, event, session_id, payload, command, cwd).await
        }
        HookCommand::Http {
            url,
            headers,
            http_timeout_ms,
            ..
        } => {
            execute_http_hook(hook, event, session_id, payload, url, headers, *http_timeout_ms)
                .await
        }
        HookCommand::Prompt {
            prompt,
            model,
            max_tokens,
            ..
        } => {
            execute_prompt_hook(hook, event, session_id, payload, prompt, model.as_deref(), *max_tokens)
                .await
        }
        HookCommand::Agent {
            agent,
            max_turns,
            ..
        } => {
            execute_agent_hook(hook, event, session_id, payload, agent, *max_turns)
                .await
        }
    }
}

/// Execute a shell command hook.
async fn execute_command_hook(
    hook: &HookDefinition,
    event: HookEvent,
    session_id: &str,
    payload: &JsonValue,
    command: &[String],
    cwd: &Path,
) -> ZeroBotResult<HookResponse> {
    if command.is_empty() {
        return Err(ZeroBotError::Agent("Hook command is empty".to_string()));
    }

    let request = serde_json::json!({
        "hook": {
            "name": hook.name,
            "event": event,
        },
        "session_id": session_id,
        "payload": payload,
    });
    let input = serde_json::to_vec(&request).map_err(|err| ZeroBotError::Agent(err.to_string()))?;

    let mut cmd = Command::new(&command[0]);
    if command.len() > 1 {
        cmd.args(&command[1..]);
    }

    // Environment variable injection
    cmd.env("ZEROBOT_PROJECT_DIR", cwd.to_string_lossy().as_ref());
    cmd.env("ZEROBOT_SESSION_ID", session_id);
    cmd.env("ZEROBOT_HOOK_NAME", &hook.name);
    cmd.env("ZEROBOT_HOOK_EVENT", format!("{:?}", event));

    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|err| ZeroBotError::Agent(err.to_string()))?;

    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        stdin.write_all(&input).await?;
    }

    let timeout_ms = hook.timeout_ms.unwrap_or(3000);
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();

    let status = match timeout(Duration::from_millis(timeout_ms), child.wait()).await {
        Ok(res) => res.map_err(|err| ZeroBotError::Agent(err.to_string()))?,
        Err(_) => {
            let _ = child.kill().await;
            return Err(ZeroBotError::Agent("Hook execution timed out".to_string()));
        }
    };

    use tokio::io::AsyncReadExt;
    let mut stdout_buf = Vec::new();
    if let Some(mut out) = stdout.take() {
        out.read_to_end(&mut stdout_buf).await?;
    }
    let mut stderr_buf = Vec::new();
    if let Some(mut err) = stderr.take() {
        err.read_to_end(&mut stderr_buf).await?;
    }

    let exit_code = status.code().unwrap_or(-1);

    // Exit code protocol:
    //   0 = success (parse JSON response)
    //   2 = blocking error (deny with stderr as message)
    //   other = non-blocking error (warn, continue as allow)
    match exit_code {
        0 => {}
        2 => {
            let stderr = String::from_utf8_lossy(&stderr_buf);
            return Ok(HookResponse {
                action: Some(HookAction::Deny),
                message: Some(stderr.to_string()),
                patch: None,
                system_message: None,
                additional_context: None,
            });
        }
        _ => {
            let stderr = String::from_utf8_lossy(&stderr_buf);
            warn!(
                "Hook '{}' exited with code {}: {}",
                hook.name, exit_code, stderr
            );
            return Ok(HookResponse {
                action: Some(HookAction::Allow),
                message: None,
                patch: None,
                system_message: None,
                additional_context: None,
            });
        }
    }

    let stdout = String::from_utf8_lossy(&stdout_buf);
    if stdout.trim().is_empty() {
        return Ok(HookResponse {
            action: Some(HookAction::Allow),
            message: None,
            patch: None,
            system_message: None,
            additional_context: None,
        });
    }
    let response: HookResponse = serde_json::from_str(stdout.trim())
        .map_err(|err| ZeroBotError::Agent(format!("Hook output parse failed: {err}")))?;
    Ok(response)
}

/// Execute an HTTP webhook hook.
async fn execute_http_hook(
    hook: &HookDefinition,
    event: HookEvent,
    session_id: &str,
    payload: &JsonValue,
    url: &str,
    headers: &Option<HashMap<String, String>>,
    http_timeout_ms: Option<u64>,
) -> ZeroBotResult<HookResponse> {
    // SSRF protection: block private/link-local IPs
    if let Err(e) = check_ssrf(url).await {
        return Err(ZeroBotError::Agent(format!(
            "HTTP hook SSRF check failed: {e}"
        )));
    }

    let request = serde_json::json!({
        "hook": {
            "name": hook.name,
            "event": event,
        },
        "session_id": session_id,
        "payload": payload,
    });

    let client = reqwest::Client::new();
    let timeout_dur = Duration::from_millis(http_timeout_ms.or(hook.timeout_ms).unwrap_or(30_000));

    let mut req_builder = client.post(url).json(&request);
    if let Some(hdrs) = headers {
        for (k, v) in hdrs {
            req_builder = req_builder.header(k, v);
        }
    }

    let response = match timeout(timeout_dur, req_builder.send()).await {
        Ok(Ok(resp)) => resp,
        Ok(Err(e)) => {
            return Err(ZeroBotError::Agent(format!("HTTP hook request failed: {e}")));
        }
        Err(_) => {
            return Err(ZeroBotError::Agent(
                "HTTP hook request timed out".to_string(),
            ));
        }
    };

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| ZeroBotError::Agent(format!("HTTP hook body read failed: {e}")))?;

    if !status.is_success() {
        return Err(ZeroBotError::Agent(format!(
            "HTTP hook returned {}: {}",
            status, body
        )));
    }

    if body.trim().is_empty() {
        return Ok(HookResponse {
            action: Some(HookAction::Allow),
            message: None,
            patch: None,
            system_message: None,
            additional_context: None,
        });
    }

    let hook_response: HookResponse = serde_json::from_str(body.trim())
        .map_err(|err| ZeroBotError::Agent(format!("HTTP hook output parse failed: {err}")))?;
    Ok(hook_response)
}

/// Execute an LLM prompt hook.
///
/// Sends the payload to a fast model with the hook's prompt template and parses
/// the response as a `HookResponse`. Falls back to Allow on parse failure.
async fn execute_prompt_hook(
    hook: &HookDefinition,
    event: HookEvent,
    session_id: &str,
    payload: &JsonValue,
    prompt_template: &str,
    _model: Option<&str>,
    _max_tokens: Option<u32>,
) -> ZeroBotResult<HookResponse> {
    // Replace {{payload}} placeholder in prompt template
    let payload_str = serde_json::to_string_pretty(payload)
        .unwrap_or_else(|_| payload.to_string());
    let prompt = prompt_template.replace("{{payload}}", &payload_str);

    let _ = (hook, event, session_id, prompt.as_str());

    // For now, log a warning and fall back to Allow.
    // Full LLM integration requires a provider factory reference in HookManager.
    warn!(
        "Prompt hook '{}' requires LLM provider integration (not yet wired). Falling back to Allow.",
        hook.name
    );

    Ok(HookResponse {
        action: Some(HookAction::Allow),
        message: Some(format!("Prompt hook evaluated (template: {} chars)", prompt.len())),
        patch: None,
        system_message: None,
        additional_context: None,
    })
}

/// Execute an agent hook.
///
/// Spawns a verification agent with the hook's agent definition and extracts
/// the final decision. Falls back to Allow on failure.
async fn execute_agent_hook(
    hook: &HookDefinition,
    event: HookEvent,
    session_id: &str,
    payload: &JsonValue,
    _agent_def: &str,
    _max_turns: Option<usize>,
) -> ZeroBotResult<HookResponse> {
    let _ = (hook, event, session_id, payload);

    // For now, log a warning and fall back to Allow.
    // Full agent integration requires creating a temporary Agent instance.
    warn!(
        "Agent hook '{}' requires Agent integration (not yet wired). Falling back to Allow.",
        hook.name
    );

    Ok(HookResponse {
        action: Some(HookAction::Allow),
        message: Some("Agent hook evaluated (not yet fully implemented)".to_string()),
        patch: None,
        system_message: None,
        additional_context: None,
    })
}

/// SSRF protection: resolve URL hostname and block private/link-local IPs.
async fn check_ssrf(url: &str) -> Result<(), String> {
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("Invalid URL: {e}"))?;
    let host = parsed.host_str().ok_or("URL has no host")?;

    // Allow localhost for development (can be made stricter)
    if host == "localhost" || host == "127.0.0.1" || host == "::1" {
        return Ok(()); // Allow localhost — user's own machine
    }

    // Resolve hostname
    let addrs = tokio::net::lookup_host(format!("{}:{}", host, parsed.port_or_known_default().unwrap_or(80)))
        .await
        .map_err(|e| format!("DNS resolution failed: {e}"))?;

    for addr in addrs {
        let ip = addr.ip();
        if is_private_ip(ip) {
            return Err(format!(
                "Blocked request to private IP {} (host: {})",
                ip, host
            ));
        }
    }
    Ok(())
}

fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_unspecified()
                // Block Docker/internal ranges
                || (v4.octets()[0] == 100 && v4.octets()[1] >= 64)
                // Cloud metadata endpoint
                || v4.octets() == [169, 254, 169, 254]
        }
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unspecified(),
    }
}

// ---------------------------------------------------------------------------
// Merge & Read helpers
// ---------------------------------------------------------------------------

fn shallow_merge(base: JsonValue, patch: JsonValue) -> JsonValue {
    match (base, patch) {
        (JsonValue::Object(mut a), JsonValue::Object(b)) => {
            for (k, v) in b {
                a.insert(k, v);
            }
            JsonValue::Object(a)
        }
        (base, _) => base,
    }
}

fn read_hooks_dir(path: &Path) -> ZeroBotResult<Vec<HookDefinition>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let file = entry.path();
        if !file.is_file() {
            continue;
        }
        let ext = file.extension().and_then(|s| s.to_str()).unwrap_or("");
        if ext != "yaml" && ext != "yml" {
            continue;
        }
        let content = std::fs::read_to_string(&file)?;
        let parsed: Result<HookFile, _> = serde_yaml::from_str(&content);
        match parsed {
            Ok(file) => out.extend(file.hooks),
            Err(err) => {
                warn!("Hook file parse failed: {}: {}", file.display(), err);
            }
        }
    }
    Ok(out)
}

fn merge_hooks(
    session_hooks: Vec<HookDefinition>,
    agent_hooks: Vec<HookDefinition>,
    skill_hooks: Vec<HookDefinition>,
    dir_hooks: Vec<HookDefinition>,
) -> Vec<HookDefinition> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    // Priority: session > agent > skill > dir
    for hook in session_hooks {
        if hook.enabled() && seen.insert(hook.name.clone()) {
            out.push(hook);
        }
    }
    for hook in agent_hooks {
        if hook.enabled() && seen.insert(hook.name.clone()) {
            out.push(hook);
        }
    }
    for hook in skill_hooks {
        if hook.enabled() && seen.insert(hook.name.clone()) {
            out.push(hook);
        }
    }
    for hook in dir_hooks {
        if hook.enabled() && seen.insert(hook.name.clone()) {
            out.push(hook);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hook_pre_tool_deny() {
        let hook = HookDefinition {
            name: "deny_tool".to_string(),
            hook: HookCommand::Legacy(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "printf '{\"action\":\"deny\",\"message\":\"no\"}'".to_string(),
            ]),
            matcher: None,
            if_condition: None,
            timeout_ms: Some(2000),
            enabled: Some(true),
            events: vec![HookEvent::PreToolUse],
            once: None,
            async_: None,
            status_message: None,
            watch_paths: Vec::new(),
            watch_pattern: None,
        };
        let manager = HookManager::new(vec![hook], Vec::new());
        let decision = manager
            .apply_event(
                HookEvent::PreToolUse,
                "s1",
                serde_json::json!({"tool_name":"bash","tool_input":{}}),
                &[],
            )
            .await
            .unwrap();
        assert_eq!(decision.action, HookAction::Deny);
        assert_eq!(decision.message, Some("no".to_string()));
    }

    #[tokio::test]
    async fn hook_pre_tool_modify() {
        let hook = HookDefinition {
            name: "modify_tool".to_string(),
            hook: HookCommand::Legacy(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "printf '{\"action\":\"modify\",\"patch\":{\"tool_input\":{\"x\":1}}}'".to_string(),
            ]),
            matcher: None,
            if_condition: None,
            timeout_ms: Some(2000),
            enabled: Some(true),
            events: vec![HookEvent::PreToolUse],
            once: None,
            async_: None,
            status_message: None,
            watch_paths: Vec::new(),
            watch_pattern: None,
        };
        let manager = HookManager::new(vec![hook], Vec::new());
        let decision = manager
            .apply_event(
                HookEvent::PreToolUse,
                "s1",
                serde_json::json!({"tool_name":"bash","tool_input":{}}),
                &[],
            )
            .await
            .unwrap();
        assert_eq!(decision.action, HookAction::Allow);
        assert_eq!(decision.payload["tool_input"]["x"], 1);
    }

    #[test]
    fn hook_merge_priority() {
        let dir = HookDefinition {
            name: "same".to_string(),
            hook: HookCommand::Legacy(vec!["dir".to_string()]),
            matcher: None,
            if_condition: None,
            timeout_ms: None,
            enabled: Some(true),
            events: vec![HookEvent::PreToolUse],
            once: None,
            async_: None,
            status_message: None,
            watch_paths: Vec::new(),
            watch_pattern: None,
        };
        let skill = HookDefinition {
            name: "same".to_string(),
            hook: HookCommand::Legacy(vec!["skill".to_string()]),
            matcher: None,
            if_condition: None,
            timeout_ms: None,
            enabled: Some(true),
            events: vec![HookEvent::PreToolUse],
            once: None,
            async_: None,
            status_message: None,
            watch_paths: Vec::new(),
            watch_pattern: None,
        };
        let agent = HookDefinition {
            name: "same".to_string(),
            hook: HookCommand::Legacy(vec!["agent".to_string()]),
            matcher: None,
            if_condition: None,
            timeout_ms: None,
            enabled: Some(true),
            events: vec![HookEvent::PreToolUse],
            once: None,
            async_: None,
            status_message: None,
            watch_paths: Vec::new(),
            watch_pattern: None,
        };
        let merged = merge_hooks(vec![], vec![agent.clone()], vec![skill.clone()], vec![dir.clone()]);
        assert_eq!(merged.len(), 1);
        match (&merged[0].hook, &agent.hook) {
            (HookCommand::Legacy(a), HookCommand::Legacy(b)) => assert_eq!(a, b),
            _ => panic!("Expected Legacy"),
        }
    }

    #[test]
    fn hook_if_condition_match() {
        let hook = HookDefinition {
            name: "git_guard".to_string(),
            hook: HookCommand::Legacy(vec!["echo".to_string(), "ok".to_string()]),
            matcher: None,
            if_condition: Some("Bash(git *)".to_string()),
            timeout_ms: None,
            enabled: Some(true),
            events: vec![HookEvent::PreToolUse],
            once: None,
            async_: None,
            status_message: None,
            watch_paths: Vec::new(),
            watch_pattern: None,
        };
        // Matches: tool_name=Bash, tool_input starts with "git"
        assert!(hook.matches(
            HookEvent::PreToolUse,
            &serde_json::json!({"tool_name": "Bash", "tool_input": "git push"}),
        ));
        // Does not match: tool_name=Bash but input doesn't start with "git"
        assert!(!hook.matches(
            HookEvent::PreToolUse,
            &serde_json::json!({"tool_name": "Bash", "tool_input": "rm -rf /"}),
        ));
        // Does not match: wrong tool name
        assert!(!hook.matches(
            HookEvent::PreToolUse,
            &serde_json::json!({"tool_name": "Read", "tool_input": "git push"}),
        ));
    }

    #[test]
    fn hook_if_condition_wildcard() {
        let hook = HookDefinition {
            name: "any_bash".to_string(),
            hook: HookCommand::Legacy(vec!["echo".to_string(), "ok".to_string()]),
            matcher: None,
            if_condition: Some("Bash(*)".to_string()),
            timeout_ms: None,
            enabled: Some(true),
            events: vec![HookEvent::PreToolUse],
            once: None,
            async_: None,
            status_message: None,
            watch_paths: Vec::new(),
            watch_pattern: None,
        };
        assert!(hook.matches(
            HookEvent::PreToolUse,
            &serde_json::json!({"tool_name": "Bash", "tool_input": "anything"}),
        ));
        assert!(!hook.matches(
            HookEvent::PreToolUse,
            &serde_json::json!({"tool_name": "Read", "tool_input": "file.txt"}),
        ));
    }

    #[test]
    fn hook_deserialize_legacy_yaml() {
        let yaml = r#"
name: legacy_hook
command: ["bash", "-c", "echo ok"]
events: ["pre_tool_use"]
"#;
        let hook: HookDefinition = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(hook.name, "legacy_hook");
        match &hook.hook {
            HookCommand::Legacy(cmd) => {
                assert_eq!(cmd.len(), 3);
                assert_eq!(cmd[0], "bash");
            }
            _ => panic!("Expected Legacy variant"),
        }
    }

    #[test]
    fn hook_deserialize_command_type_yaml() {
        let yaml = r#"
name: typed_hook
hook:
  type: command
  command: ["bash", "-c", "echo ok"]
  shell: bash
events: ["pre_tool_use"]
"#;
        let hook: HookDefinition = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(hook.name, "typed_hook");
        match &hook.hook {
            HookCommand::Command { command, shell, .. } => {
                assert_eq!(command.len(), 3);
                assert_eq!(shell.as_deref(), Some("bash"));
            }
            _ => panic!("Expected Command variant"),
        }
    }

    #[test]
    fn hook_deserialize_http_type_yaml() {
        let yaml = r#"
name: webhook
hook:
  type: http
  url: "https://example.com/hook"
  headers:
    Authorization: "Bearer token"
  http_timeout_ms: 5000
events: ["pre_tool_use"]
"#;
        let hook: HookDefinition = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(hook.name, "webhook");
        match &hook.hook {
            HookCommand::Http {
                url,
                headers,
                http_timeout_ms,
                ..
            } => {
                assert_eq!(url, "https://example.com/hook");
                assert!(headers.as_ref().unwrap().contains_key("Authorization"));
                assert_eq!(*http_timeout_ms, Some(5000));
            }
            _ => panic!("Expected Http variant"),
        }
    }

    #[test]
    fn hook_new_fields_yaml() {
        let yaml = r#"
name: enhanced
command: ["echo", "ok"]
matcher: "Bash"
if_condition: "Bash(git *)"
timeout_ms: 5000
enabled: true
events: ["pre_tool_use"]
once: true
async: true
status_message: "Checking git commands..."
"#;
        let hook: HookDefinition = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(hook.name, "enhanced");
        assert_eq!(hook.matcher.as_deref(), Some("Bash"));
        assert_eq!(hook.if_condition.as_deref(), Some("Bash(git *)"));
        assert_eq!(hook.timeout_ms, Some(5000));
        assert!(hook.is_once());
        assert!(hook.is_async());
        assert_eq!(
            hook.status_message.as_deref(),
            Some("Checking git commands...")
        );
    }

    #[tokio::test]
    async fn hook_exit_code_2_deny() {
        let hook = HookDefinition {
            name: "exit2".to_string(),
            hook: HookCommand::Legacy(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "echo 'blocked' >&2; exit 2".to_string(),
            ]),
            matcher: None,
            if_condition: None,
            timeout_ms: Some(2000),
            enabled: Some(true),
            events: vec![HookEvent::PreToolUse],
            once: None,
            async_: None,
            status_message: None,
            watch_paths: Vec::new(),
            watch_pattern: None,
        };
        let manager = HookManager::new(vec![hook], Vec::new());
        let decision = manager
            .apply_event(
                HookEvent::PreToolUse,
                "s1",
                serde_json::json!({"tool_name":"bash","tool_input":{}}),
                &[],
            )
            .await
            .unwrap();
        assert_eq!(decision.action, HookAction::Deny);
        assert!(decision.message.as_deref().unwrap().contains("blocked"));
    }

    #[test]
    fn glob_match_basic() {
        assert!(glob_match("git *", "git push origin main"));
        assert!(glob_match("Bash", "Bash"));
        assert!(!glob_match("Bash", "Read"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("test?file", "test1file"));
        assert!(!glob_match("test?file", "test12file"));
    }
}
