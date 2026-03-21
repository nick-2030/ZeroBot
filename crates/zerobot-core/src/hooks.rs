use crate::config::Settings;
use crate::error::{ZeroBotError, ZeroBotResult};
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use std::collections::HashSet;
use std::path::Path;
use tokio::process::Command;
use tokio::time::{timeout, Duration};
use tracing::warn;

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
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HookAction {
    Allow,
    Deny,
    Modify,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookDefinition {
    pub name: String,
    pub command: Vec<String>,
    #[serde(default)]
    pub matcher: Option<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub events: Vec<HookEvent>,
}

impl HookDefinition {
    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    pub fn matches(&self, event: HookEvent, payload: &JsonValue) -> bool {
        if self.events.is_empty() {
            return self.matches_matcher(event, payload);
        }
        if !self.events.contains(&event) {
            return false;
        }
        self.matches_matcher(event, payload)
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
}

#[derive(Debug, Clone)]
pub struct HookDecision {
    pub action: HookAction,
    pub payload: JsonValue,
    pub message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HookManager {
    agent_hooks: Vec<HookDefinition>,
    dir_hooks: Vec<HookDefinition>,
}

impl HookManager {
    pub fn new(agent_hooks: Vec<HookDefinition>, dir_hooks: Vec<HookDefinition>) -> Self {
        Self {
            agent_hooks,
            dir_hooks,
        }
    }

    pub fn empty() -> Self {
        Self {
            agent_hooks: Vec::new(),
            dir_hooks: Vec::new(),
        }
    }

    pub fn load(
        settings: &Settings,
        cwd: &Path,
        agent_hooks: Option<Vec<HookDefinition>>,
    ) -> ZeroBotResult<Self> {
        let dir_hooks = read_hooks_dir(&cwd.join(".zerobot").join("hooks"))?;
        let _ = settings;
        let _ = cwd;
        Ok(Self {
            agent_hooks: agent_hooks.unwrap_or_default(),
            dir_hooks,
        })
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
        let hooks = merge_hooks(
            self.agent_hooks.clone(),
            skill_hooks.to_vec(),
            self.dir_hooks.clone(),
        );
        let mut current = payload;
        for hook in &hooks {
            if !hook.enabled() || !hook.matches(event, &current) {
                continue;
            }
            match execute_hook(hook, event, session_id, &current).await {
                Ok(response) => match response.action.unwrap_or(HookAction::Allow) {
                    HookAction::Allow => {}
                    HookAction::Deny => {
                        return Ok(HookDecision {
                            action: HookAction::Deny,
                            payload: current,
                            message: response.message,
                        });
                    }
                    HookAction::Modify => {
                        if let Some(patch) = response.patch {
                            current = shallow_merge(current, patch);
                        }
                    }
                },
                Err(err) => {
                    warn!("Hook 执行失败: {}: {}", hook.name, err);
                }
            }
        }

        Ok(HookDecision {
            action: HookAction::Allow,
            payload: current,
            message: None,
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

#[derive(Debug, Deserialize)]
struct HookFile {
    #[serde(default)]
    hooks: Vec<HookDefinition>,
}

#[derive(Debug, Deserialize)]
struct HookResponse {
    #[serde(default)]
    action: Option<HookAction>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    patch: Option<JsonValue>,
}

async fn execute_hook(
    hook: &HookDefinition,
    event: HookEvent,
    session_id: &str,
    payload: &JsonValue,
) -> ZeroBotResult<HookResponse> {
    if hook.command.is_empty() {
        return Err(ZeroBotError::Agent("Hook command 为空".to_string()));
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

    let mut cmd = Command::new(&hook.command[0]);
    if hook.command.len() > 1 {
        cmd.args(&hook.command[1..]);
    }
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
            return Err(ZeroBotError::Agent("Hook 执行超时".to_string()));
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

    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr_buf);
        return Err(ZeroBotError::Agent(format!("Hook 命令失败: {stderr}")));
    }

    let stdout = String::from_utf8_lossy(&stdout_buf);
    if stdout.trim().is_empty() {
        return Ok(HookResponse {
            action: Some(HookAction::Allow),
            message: None,
            patch: None,
        });
    }
    let response: HookResponse = serde_json::from_str(stdout.trim())
        .map_err(|err| ZeroBotError::Agent(format!("Hook 输出解析失败: {err}")))?;
    Ok(response)
}

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
                warn!("Hook 文件解析失败: {}: {}", file.display(), err);
            }
        }
    }
    Ok(out)
}

fn merge_hooks(
    agent_hooks: Vec<HookDefinition>,
    skill_hooks: Vec<HookDefinition>,
    dir_hooks: Vec<HookDefinition>,
) -> Vec<HookDefinition> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hook_pre_tool_deny() {
        let hook = HookDefinition {
            name: "deny_tool".to_string(),
            command: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "printf '{\"action\":\"deny\",\"message\":\"no\"}'".to_string(),
            ],
            matcher: None,
            timeout_ms: Some(2000),
            enabled: Some(true),
            events: vec![HookEvent::PreToolUse],
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
            command: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "printf '{\"action\":\"modify\",\"patch\":{\"tool_input\":{\"x\":1}}}'".to_string(),
            ],
            matcher: None,
            timeout_ms: Some(2000),
            enabled: Some(true),
            events: vec![HookEvent::PreToolUse],
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
            command: vec!["dir".to_string()],
            matcher: None,
            timeout_ms: None,
            enabled: Some(true),
            events: vec![HookEvent::PreToolUse],
        };
        let skill = HookDefinition {
            name: "same".to_string(),
            command: vec!["skill".to_string()],
            matcher: None,
            timeout_ms: None,
            enabled: Some(true),
            events: vec![HookEvent::PreToolUse],
        };
        let agent = HookDefinition {
            name: "same".to_string(),
            command: vec!["agent".to_string()],
            matcher: None,
            timeout_ms: None,
            enabled: Some(true),
            events: vec![HookEvent::PreToolUse],
        };
        let merged = merge_hooks(vec![agent.clone()], vec![skill.clone()], vec![dir.clone()]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].command, agent.command);
    }
}
