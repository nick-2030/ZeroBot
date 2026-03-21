use crate::config::{
    PluginEntryConfig, PluginFailureMode, PluginManifest, ProviderSettings, Settings,
};
use crate::error::{ZeroBotError, ZeroBotResult};
use crate::workspace::resolve_workspace_root;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tokio::time::{timeout, Duration};
use tracing::warn;

const DEFAULT_HOOK_TIMEOUT_MS: u64 = 3000;
const DEFAULT_INIT_TIMEOUT_MS: u64 = 10_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginToolInfo {
    pub plugin: String,
    pub name: String,
    pub description: String,
    pub parameters: JsonValue,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginHookWarning {
    pub plugin: String,
    pub hook: String,
    pub message: String,
    pub degraded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginAuthMethod {
    pub plugin: String,
    pub provider: String,
    pub index: usize,
    pub label: String,
    #[serde(rename = "type")]
    pub method_type: String,
    #[serde(default)]
    pub prompts: Vec<PluginAuthPrompt>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginAuthPrompt {
    #[serde(rename = "type")]
    pub prompt_type: String,
    pub key: String,
    pub message: String,
    #[serde(default)]
    pub placeholder: Option<String>,
    #[serde(default)]
    pub options: Vec<PluginAuthPromptOption>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginAuthPromptOption {
    pub label: String,
    pub value: String,
    #[serde(default)]
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginAuthAuthorizeResult {
    pub plugin: String,
    pub provider: String,
    pub method_index: usize,
    pub url: String,
    pub instructions: String,
    pub method: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginAuthCallbackResult {
    pub plugin: String,
    pub provider: String,
    #[serde(default)]
    pub data: JsonValue,
}

#[derive(Debug, Clone)]
struct PluginRuntimeConfig {
    name: String,
    command: Vec<String>,
    env: HashMap<String, String>,
    hook_timeout_ms: u64,
    tool_timeout_ms: u64,
    failure_mode: PluginFailureMode,
}

#[derive(Debug)]
struct PluginProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

#[derive(Debug)]
struct PluginClient {
    cfg: PluginRuntimeConfig,
    cwd: PathBuf,
    process: Mutex<Option<PluginProcess>>,
    request_id: AtomicU64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PluginAuthStore {
    #[serde(default)]
    providers: HashMap<String, JsonValue>,
}

impl Default for PluginAuthStore {
    fn default() -> Self {
        Self {
            providers: HashMap::new(),
        }
    }
}

#[derive(Clone)]
pub struct PluginManager {
    clients: Vec<Arc<PluginClient>>,
    tools: HashMap<String, PluginToolInfo>,
    provider_settings: HashMap<String, ProviderSettings>,
    auth_store_path: PathBuf,
    auth_store: Arc<Mutex<PluginAuthStore>>,
}

impl PluginManager {
    pub async fn new(settings: &Settings, cwd: &Path) -> ZeroBotResult<Option<Arc<Self>>> {
        if !settings.plugins.enabled {
            return Ok(None);
        }

        let resolved = resolve_entries(settings, cwd)?;
        if resolved.is_empty() {
            return Ok(None);
        }

        let mut clients = Vec::new();
        let mut tools = HashMap::new();

        for entry in resolved {
            let client = Arc::new(PluginClient::new(entry, cwd.to_path_buf()));
            if let Err(err) = client.initialize().await {
                if client.cfg.failure_mode == PluginFailureMode::Closed {
                    return Err(err);
                }
                warn!("插件初始化失败，已降级继续: {}: {}", client.cfg.name, err);
            }
            match client.list_tools().await {
                Ok(items) => {
                    for tool in items {
                        tools.insert(tool.name.clone(), tool);
                    }
                }
                Err(err) => {
                    if client.cfg.failure_mode == PluginFailureMode::Closed {
                        return Err(err);
                    }
                    warn!(
                        "插件 tools/list 失败，已降级继续: {}: {}",
                        client.cfg.name, err
                    );
                }
            }
            clients.push(client);
        }

        let auth_store_path = auth_store_path();
        let auth_store = Arc::new(Mutex::new(load_auth_store(&auth_store_path).await));

        let manager = Self {
            clients,
            tools,
            provider_settings: settings.providers.clone(),
            auth_store_path,
            auth_store,
        };

        Ok(Some(Arc::new(manager)))
    }

    pub fn tools(&self) -> Vec<PluginToolInfo> {
        self.tools.values().cloned().collect()
    }

    pub async fn shutdown(&self) {
        for client in &self.clients {
            let result = client
                .call_method(
                    "plugin.shutdown",
                    json!({ "plugin": client.cfg.name }),
                    DEFAULT_HOOK_TIMEOUT_MS,
                )
                .await;
            if let Err(err) = result {
                warn!("插件 shutdown 调用失败: {}: {}", client.cfg.name, err);
            }
            let _ = client.kill().await;
        }
    }

    pub async fn run_hook(
        &self,
        hook_name: &str,
        input: JsonValue,
        output: JsonValue,
    ) -> ZeroBotResult<JsonValue> {
        let (output, _warnings) = self
            .run_hook_with_warnings(hook_name, input, output)
            .await?;
        Ok(output)
    }

    pub async fn run_hook_with_warnings(
        &self,
        hook_name: &str,
        input: JsonValue,
        mut output: JsonValue,
    ) -> ZeroBotResult<(JsonValue, Vec<PluginHookWarning>)> {
        let mut warnings = Vec::new();
        for client in &self.clients {
            let params = json!({
                "hook": hook_name,
                "input": input,
                "output": output,
            });
            let result = client
                .call_method("plugin.hook.call", params, client.cfg.hook_timeout_ms)
                .await;
            match result {
                Ok(value) => {
                    if value.is_object() {
                        output = merge_json(output, value);
                    }
                }
                Err(err) => {
                    if client.cfg.failure_mode == PluginFailureMode::Closed {
                        return Err(err);
                    }
                    let message = err.to_string();
                    warn!(
                        "插件 hook 执行失败，已降级继续: plugin={}, hook={}, err={}",
                        client.cfg.name, hook_name, message
                    );
                    warnings.push(PluginHookWarning {
                        plugin: client.cfg.name.clone(),
                        hook: hook_name.to_string(),
                        message,
                        degraded: true,
                    });
                }
            }
        }
        Ok((output, warnings))
    }

    pub async fn call_tool(
        &self,
        plugin: &str,
        tool_name: &str,
        args: JsonValue,
        context: JsonValue,
    ) -> ZeroBotResult<JsonValue> {
        let client = self
            .clients
            .iter()
            .find(|c| c.cfg.name == plugin)
            .ok_or_else(|| ZeroBotError::Tool(format!("未知插件: {plugin}")))?;

        client
            .call_method(
                "plugin.tools.call",
                json!({
                    "name": tool_name,
                    "args": args,
                    "context": context,
                }),
                client.cfg.tool_timeout_ms,
            )
            .await
    }

    pub async fn list_auth_methods(&self, provider: &str) -> ZeroBotResult<Vec<PluginAuthMethod>> {
        let mut out = Vec::new();
        for client in &self.clients {
            let result = client
                .call_method(
                    "plugin.auth.methods",
                    json!({ "provider": provider }),
                    client.cfg.hook_timeout_ms,
                )
                .await;
            let value = match result {
                Ok(value) => value,
                Err(err) => {
                    if client.cfg.failure_mode == PluginFailureMode::Closed {
                        return Err(err);
                    }
                    warn!(
                        "插件 auth methods 调用失败，已降级继续: plugin={}, provider={}, err={}",
                        client.cfg.name, provider, err
                    );
                    continue;
                }
            };
            let list = value
                .get("methods")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            for (idx, item) in list.into_iter().enumerate() {
                let label = item
                    .get("label")
                    .and_then(|v| v.as_str())
                    .unwrap_or("auth")
                    .to_string();
                let method_type = item
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("api")
                    .to_string();
                let prompts = item
                    .get("prompts")
                    .cloned()
                    .and_then(|v| serde_json::from_value::<Vec<PluginAuthPrompt>>(v).ok())
                    .unwrap_or_default();
                out.push(PluginAuthMethod {
                    plugin: client.cfg.name.clone(),
                    provider: provider.to_string(),
                    index: idx,
                    label,
                    method_type,
                    prompts,
                });
            }
        }
        Ok(out)
    }

    pub async fn authorize(
        &self,
        provider: &str,
        plugin: &str,
        method_index: usize,
        inputs: HashMap<String, String>,
    ) -> ZeroBotResult<PluginAuthAuthorizeResult> {
        let client = self
            .clients
            .iter()
            .find(|c| c.cfg.name == plugin)
            .ok_or_else(|| ZeroBotError::Tool(format!("未知插件: {plugin}")))?;
        let value = client
            .call_method(
                "plugin.auth.authorize",
                json!({
                    "provider": provider,
                    "method_index": method_index,
                    "inputs": inputs,
                }),
                client.cfg.hook_timeout_ms,
            )
            .await?;
        let url = value
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let instructions = value
            .get("instructions")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let method = value
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("api")
            .to_string();
        Ok(PluginAuthAuthorizeResult {
            plugin: plugin.to_string(),
            provider: provider.to_string(),
            method_index,
            url,
            instructions,
            method,
        })
    }

    pub async fn callback(
        &self,
        provider: &str,
        plugin: &str,
        method_index: usize,
        code: Option<String>,
    ) -> ZeroBotResult<PluginAuthCallbackResult> {
        let client = self
            .clients
            .iter()
            .find(|c| c.cfg.name == plugin)
            .ok_or_else(|| ZeroBotError::Tool(format!("未知插件: {plugin}")))?;
        let value = client
            .call_method(
                "plugin.auth.callback",
                json!({
                    "provider": provider,
                    "method_index": method_index,
                    "code": code,
                }),
                client.cfg.hook_timeout_ms,
            )
            .await?;

        let data = value
            .get("auth")
            .cloned()
            .unwrap_or_else(|| json!({ "provider": provider, "plugin": plugin }));

        {
            let mut store = self.auth_store.lock().await;
            store.providers.insert(provider.to_string(), data.clone());
            let _ = save_auth_store(&self.auth_store_path, &store).await;
        }

        Ok(PluginAuthCallbackResult {
            plugin: plugin.to_string(),
            provider: provider.to_string(),
            data,
        })
    }

    pub async fn provider_options(
        &self,
        provider_id: &str,
        model: &str,
    ) -> ZeroBotResult<JsonValue> {
        let auth = {
            let store = self.auth_store.lock().await;
            store.providers.get(provider_id).cloned()
        };
        let provider_cfg = self
            .provider_settings
            .get(provider_id)
            .cloned()
            .map(|info| {
                json!({
                    "kind": info.kind,
                    "base_url": info.base_url,
                    "model": info.model,
                })
            })
            .unwrap_or_else(|| json!({}));

        let output = self
            .run_hook(
                "plugin.auth.loader",
                json!({
                    "provider": provider_id,
                    "model": model,
                    "provider_config": provider_cfg,
                    "auth": auth,
                }),
                json!({ "provider_options": {} }),
            )
            .await?;

        Ok(output
            .get("provider_options")
            .cloned()
            .unwrap_or_else(|| json!({})))
    }
}

impl PluginClient {
    fn new(cfg: PluginRuntimeConfig, cwd: PathBuf) -> Self {
        Self {
            cfg,
            cwd,
            process: Mutex::new(None),
            request_id: AtomicU64::new(1),
        }
    }

    async fn initialize(&self) -> ZeroBotResult<()> {
        let _ = self
            .call_method(
                "plugin.initialize",
                json!({
                    "name": self.cfg.name,
                    "cwd": self.cwd,
                }),
                DEFAULT_INIT_TIMEOUT_MS,
            )
            .await?;
        Ok(())
    }

    async fn list_tools(&self) -> ZeroBotResult<Vec<PluginToolInfo>> {
        let value = self
            .call_method("plugin.tools.list", json!({}), self.cfg.hook_timeout_ms)
            .await?;
        let mut out = Vec::new();
        let list = value
            .get("tools")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        for item in list {
            let name = item
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            if name.is_empty() {
                continue;
            }
            let description = item
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let parameters = item
                .get("parameters")
                .cloned()
                .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
            let timeout_ms = item
                .get("timeout_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(self.cfg.tool_timeout_ms);
            out.push(PluginToolInfo {
                plugin: self.cfg.name.clone(),
                name,
                description,
                parameters,
                timeout_ms,
            });
        }
        Ok(out)
    }

    async fn kill(&self) -> ZeroBotResult<()> {
        let mut guard = self.process.lock().await;
        if let Some(process) = guard.as_mut() {
            let _ = process.child.kill().await;
        }
        *guard = None;
        Ok(())
    }

    async fn call_method(
        &self,
        method: &str,
        params: JsonValue,
        timeout_ms: u64,
    ) -> ZeroBotResult<JsonValue> {
        let id = self.request_id.fetch_add(1, Ordering::Relaxed);
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let timeout_ms = timeout_ms.max(1);
        let mut guard = self.process.lock().await;
        if guard.is_none() {
            *guard = Some(self.spawn_process().await?);
        }
        let process = guard
            .as_mut()
            .ok_or_else(|| ZeroBotError::Agent("插件进程不可用".to_string()))?;

        let request_text = serde_json::to_string(&request)
            .map_err(|err| ZeroBotError::Agent(format!("插件请求序列化失败: {err}")))?;
        let write = timeout(Duration::from_millis(timeout_ms), async {
            process.stdin.write_all(request_text.as_bytes()).await?;
            process.stdin.write_all(b"\n").await?;
            process.stdin.flush().await
        })
        .await;

        match write {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                *guard = None;
                return Err(ZeroBotError::Agent(format!("插件写入失败: {err}")));
            }
            Err(_) => {
                *guard = None;
                return Err(ZeroBotError::Agent(format!(
                    "插件调用超时（写入）: {}",
                    self.cfg.name
                )));
            }
        }

        let mut line = String::new();
        loop {
            line.clear();
            let read = timeout(
                Duration::from_millis(timeout_ms),
                process.stdout.read_line(&mut line),
            )
            .await;
            let size = match read {
                Ok(Ok(size)) => size,
                Ok(Err(err)) => {
                    *guard = None;
                    return Err(ZeroBotError::Agent(format!("插件读取失败: {err}")));
                }
                Err(_) => {
                    *guard = None;
                    return Err(ZeroBotError::Agent(format!(
                        "插件调用超时（读取）: {}",
                        self.cfg.name
                    )));
                }
            };
            if size == 0 {
                *guard = None;
                return Err(ZeroBotError::Agent(format!(
                    "插件进程已退出: {}",
                    self.cfg.name
                )));
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let value: JsonValue = match serde_json::from_str(trimmed) {
                Ok(value) => value,
                Err(_) => continue,
            };
            let resp_id = value.get("id").and_then(|v| v.as_u64());
            if resp_id != Some(id) {
                continue;
            }

            if let Some(err) = value.get("error") {
                let message = err
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("插件返回错误");
                return Err(ZeroBotError::Agent(format!(
                    "插件调用失败: {}: {message}",
                    self.cfg.name
                )));
            }

            return Ok(value.get("result").cloned().unwrap_or(JsonValue::Null));
        }
    }

    async fn spawn_process(&self) -> ZeroBotResult<PluginProcess> {
        if self.cfg.command.is_empty() {
            return Err(ZeroBotError::Config(format!(
                "插件 command 为空: {}",
                self.cfg.name
            )));
        }

        let mut cmd = Command::new(&self.cfg.command[0]);
        if self.cfg.command.len() > 1 {
            cmd.args(&self.cfg.command[1..]);
        }
        cmd.current_dir(&self.cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .envs(self.cfg.env.clone());

        let mut child = cmd
            .spawn()
            .map_err(|err| ZeroBotError::Agent(format!("插件进程启动失败: {err}")))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| ZeroBotError::Agent("插件进程未提供 stdin".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ZeroBotError::Agent("插件进程未提供 stdout".to_string()))?;
        if let Some(stderr) = child.stderr.take() {
            spawn_stderr_logger(self.cfg.name.clone(), stderr);
        }

        Ok(PluginProcess {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        })
    }
}

fn spawn_stderr_logger(name: String, stderr: ChildStderr) {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();
        loop {
            line.clear();
            let read = reader.read_line(&mut line).await;
            match read {
                Ok(0) => break,
                Ok(_) => {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        warn!("plugin stderr [{}]: {}", name, trimmed);
                    }
                }
                Err(_) => break,
            }
        }
    });
}

fn merge_json(base: JsonValue, patch: JsonValue) -> JsonValue {
    match (base, patch) {
        (JsonValue::Object(mut a), JsonValue::Object(b)) => {
            for (k, v) in b {
                let current = a.remove(&k).unwrap_or(JsonValue::Null);
                a.insert(k, merge_json(current, v));
            }
            JsonValue::Object(a)
        }
        (_, overlay) => overlay,
    }
}

fn resolve_entries(settings: &Settings, cwd: &Path) -> ZeroBotResult<Vec<PluginRuntimeConfig>> {
    let mut entries = Vec::new();
    let workspace = resolve_workspace_root(cwd);

    entries.extend(
        settings
            .plugins
            .entries
            .iter()
            .filter(|entry| entry.enabled.unwrap_or(true))
            .cloned(),
    );

    let mut dirs = Vec::new();
    dirs.push(home_dir().join(".zerobot").join("plugins"));
    dirs.push(workspace.join(".zerobot").join("plugins"));
    for path in &settings.plugins.paths {
        dirs.push(resolve_plugin_path(path)?);
    }

    for dir in dirs {
        let mut manifests = load_manifest_entries(&dir)?;
        entries.append(&mut manifests);
    }

    let deduped = deduplicate_entries(entries);
    let mut out = Vec::new();
    for entry in deduped {
        if entry.command.is_empty() {
            return Err(ZeroBotError::Config(format!(
                "插件 command 为空: {}",
                entry.name
            )));
        }
        out.push(PluginRuntimeConfig {
            name: entry.name,
            command: entry.command,
            env: entry.env,
            hook_timeout_ms: entry
                .hook_timeout_ms
                .unwrap_or(settings.plugins.default_hook_timeout_ms),
            tool_timeout_ms: entry
                .tool_timeout_ms
                .unwrap_or(settings.plugins.default_tool_timeout_ms),
            failure_mode: entry.failure_mode.unwrap_or(settings.plugins.failure_mode),
        });
    }

    Ok(out)
}

fn load_manifest_entries(dir: &Path) -> ZeroBotResult<Vec<PluginEntryConfig>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if ext != "yaml" && ext != "yml" {
            continue;
        }
        files.push(path);
    }
    files.sort();

    let mut out = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(&file)?;
        let mut parsed: PluginManifest = serde_yaml::from_str(&text).map_err(|err| {
            ZeroBotError::Config(format!("插件清单解析失败: {}: {err}", file.display()))
        })?;
        if parsed.name.trim().is_empty() {
            let fallback = file
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("plugin")
                .to_string();
            parsed.name = fallback;
        }
        let entry = parsed.into_entry();
        if !entry.enabled.unwrap_or(true) {
            continue;
        }
        out.push(entry);
    }

    Ok(out)
}

fn deduplicate_entries(entries: Vec<PluginEntryConfig>) -> Vec<PluginEntryConfig> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for entry in entries.into_iter().rev() {
        if seen.insert(entry.name.clone()) {
            out.push(entry);
        }
    }
    out.reverse();
    out
}

fn expand_home(path: &str) -> PathBuf {
    if path == "~" {
        return home_dir();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    PathBuf::from(path)
}

fn resolve_plugin_path(path: &str) -> ZeroBotResult<PathBuf> {
    if let Some(rest) = path.strip_prefix("file://") {
        return Ok(PathBuf::from(rest));
    }
    if path.contains("://") {
        return Err(ZeroBotError::Config(format!(
            "插件路径仅支持本地路径与 file://: {path}"
        )));
    }
    Ok(expand_home(path))
}

fn home_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home);
    }
    if let Ok(home) = std::env::var("USERPROFILE") {
        return PathBuf::from(home);
    }
    PathBuf::from(".")
}

fn auth_store_path() -> PathBuf {
    home_dir().join(".zerobot").join("plugin-auth.json")
}

async fn load_auth_store(path: &Path) -> PluginAuthStore {
    let text = tokio::fs::read_to_string(path).await;
    match text {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => PluginAuthStore::default(),
    }
}

async fn save_auth_store(path: &Path, store: &PluginAuthStore) -> ZeroBotResult<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|err| ZeroBotError::Io(err.to_string()))?;
    }
    let text = serde_json::to_string_pretty(store)
        .map_err(|err| ZeroBotError::Config(format!("插件认证存储序列化失败: {err}")))?;
    tokio::fs::write(path, text)
        .await
        .map_err(|err| ZeroBotError::Io(err.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deduplicate_keeps_last() {
        let list = vec![
            PluginEntryConfig {
                name: "a".to_string(),
                command: vec!["echo".to_string(), "1".to_string()],
                env: HashMap::new(),
                enabled: Some(true),
                hook_timeout_ms: None,
                tool_timeout_ms: None,
                failure_mode: None,
            },
            PluginEntryConfig {
                name: "a".to_string(),
                command: vec!["echo".to_string(), "2".to_string()],
                env: HashMap::new(),
                enabled: Some(true),
                hook_timeout_ms: None,
                tool_timeout_ms: None,
                failure_mode: None,
            },
        ];
        let out = deduplicate_entries(list);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].command[1], "2");
    }

    #[test]
    fn merge_json_recursively() {
        let base = json!({"a": 1, "b": {"x": 1, "y": 2}});
        let patch = json!({"b": {"y": 3, "z": 4}, "c": true});
        let merged = merge_json(base, patch);
        assert_eq!(merged["b"]["x"], 1);
        assert_eq!(merged["b"]["y"], 3);
        assert_eq!(merged["b"]["z"], 4);
        assert_eq!(merged["c"], true);
    }
}
