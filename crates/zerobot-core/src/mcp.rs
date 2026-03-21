use crate::config::{McpLocalProtocol, McpServerConfig, Settings};
use crate::error::{ZeroBotError, ZeroBotResult};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tracing::warn;

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

#[derive(Debug, Clone)]
pub struct McpToolInfo {
    pub server: String,
    pub name: String,
    pub description: String,
    pub parameters: JsonValue,
}

#[async_trait]
trait McpClient: Send + Sync {
    async fn initialize(&self) -> ZeroBotResult<()>;
    async fn list_tools(&self) -> ZeroBotResult<Vec<McpToolInfo>>;
    async fn call_tool(&self, name: &str, arguments: JsonValue) -> ZeroBotResult<JsonValue>;
}

pub struct McpManager {
    clients: HashMap<String, Arc<dyn McpClient>>,
    tools: Vec<McpToolInfo>,
}

impl McpManager {
    pub async fn new(settings: &Settings, _cwd: &std::path::Path) -> ZeroBotResult<Option<Self>> {
        if !settings.mcp.enabled {
            return Ok(None);
        }

        let mut clients: HashMap<String, Arc<dyn McpClient>> = HashMap::new();
        let mut tools = Vec::new();
        let mut had_any = false;

        for server in &settings.mcp.servers {
            if !server.is_enabled() {
                continue;
            }
            had_any = true;
            let name = server.name().to_string();
            let client: Arc<dyn McpClient> = match server {
                McpServerConfig::Local {
                    command,
                    env,
                    protocol,
                    timeout_ms,
                    ..
                } => match LocalMcpClient::spawn(
                    command,
                    env,
                    protocol.unwrap_or_default(),
                    timeout_ms.unwrap_or(5000),
                )
                .await
                {
                    Ok(client) => Arc::new(client),
                    Err(err) => {
                        warn!("MCP 本地服务启动失败: {}: {}", name, err);
                        continue;
                    }
                },
                McpServerConfig::Remote {
                    url,
                    headers,
                    timeout_ms,
                    ..
                } => Arc::new(RemoteMcpClient::new(
                    url,
                    headers,
                    timeout_ms.unwrap_or(5000),
                )),
            };

            if let Err(err) = client.initialize().await {
                warn!("MCP 初始化失败: {}: {}", name, err);
                continue;
            }
            let mut server_tools = match client.list_tools().await {
                Ok(tools) => tools,
                Err(err) => {
                    warn!("MCP tools/list 失败: {}: {}", name, err);
                    continue;
                }
            };
            for tool in server_tools.iter_mut() {
                tool.server = name.clone();
            }
            tools.extend(server_tools);
            clients.insert(name, client);
        }

        if !had_any || clients.is_empty() {
            return Ok(None);
        }

        Ok(Some(Self { clients, tools }))
    }

    pub fn tools(&self) -> Vec<McpToolInfo> {
        self.tools.clone()
    }

    pub async fn call_tool(
        &self,
        server: &str,
        name: &str,
        arguments: JsonValue,
    ) -> ZeroBotResult<JsonValue> {
        let client = self
            .clients
            .get(server)
            .ok_or_else(|| ZeroBotError::Mcp(format!("未知 MCP 服务器: {server}")))?;
        client.call_tool(name, arguments).await
    }
}

#[derive(Debug, Serialize)]
struct JsonRpcRequest<'a> {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<u64>,
    method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<JsonValue>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    #[allow(dead_code)]
    jsonrpc: String,
    id: Option<u64>,
    result: Option<JsonValue>,
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

#[derive(Clone)]
struct RemoteMcpClient {
    url: String,
    headers: HashMap<String, String>,
    timeout: Duration,
    client: reqwest::Client,
    next_id: Arc<AtomicU64>,
}

impl RemoteMcpClient {
    fn new(url: &str, headers: &HashMap<String, String>, timeout_ms: u64) -> Self {
        Self {
            url: url.to_string(),
            headers: headers.clone(),
            timeout: Duration::from_millis(timeout_ms),
            client: reqwest::Client::new(),
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    async fn call(&self, method: &str, params: Option<JsonValue>) -> ZeroBotResult<JsonValue> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let request = JsonRpcRequest {
            jsonrpc: "2.0",
            id: Some(id),
            method,
            params,
        };
        let mut builder = self
            .client
            .post(&self.url)
            .json(&request)
            .timeout(self.timeout);
        if !self
            .headers
            .keys()
            .any(|key| key.eq_ignore_ascii_case("accept"))
        {
            builder = builder.header("Accept", "application/json, text/event-stream");
        }
        for (k, v) in &self.headers {
            builder = builder.header(k, v);
        }
        let response = builder.send().await?.json::<JsonRpcResponse>().await?;
        if let Some(err) = response.error {
            return Err(ZeroBotError::Mcp(format!(
                "远程 MCP 错误: {} ({})",
                err.message, err.code
            )));
        }
        response
            .result
            .ok_or_else(|| ZeroBotError::Mcp("远程 MCP 返回空结果".to_string()))
    }
}

#[async_trait]
impl McpClient for RemoteMcpClient {
    async fn initialize(&self) -> ZeroBotResult<()> {
        let params = serde_json::json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "zerobot", "version": "0.1.0" }
        });
        let _ = self.call("initialize", Some(params)).await?;
        Ok(())
    }

    async fn list_tools(&self) -> ZeroBotResult<Vec<McpToolInfo>> {
        let result = self.call("tools/list", None).await?;
        parse_tools(result)
    }

    async fn call_tool(&self, name: &str, arguments: JsonValue) -> ZeroBotResult<JsonValue> {
        let params = serde_json::json!({
            "name": name,
            "arguments": arguments
        });
        self.call("tools/call", Some(params)).await
    }
}

struct LocalMcpClient {
    stdin: Mutex<ChildStdin>,
    stdout: Mutex<BufReader<ChildStdout>>,
    protocol: McpLocalProtocol,
    next_id: AtomicU64,
    timeout: Duration,
}

impl LocalMcpClient {
    async fn spawn(
        command: &[String],
        env: &HashMap<String, String>,
        protocol: McpLocalProtocol,
        timeout_ms: u64,
    ) -> ZeroBotResult<Self> {
        if command.is_empty() {
            return Err(ZeroBotError::Mcp("本地 MCP command 为空".to_string()));
        }
        let mut cmd = Command::new(&command[0]);
        if command.len() > 1 {
            cmd.args(&command[1..]);
        }
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| ZeroBotError::Mcp("本地 MCP 子进程未打开 stdin".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ZeroBotError::Mcp("本地 MCP 子进程未打开 stdout".to_string()))?;
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut reader = BufReader::new(stderr);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line).await {
                        Ok(0) => break,
                        Ok(_) => {
                            let trimmed = line.trim_end();
                            if !trimmed.is_empty() {
                                warn!("mcp stderr: {}", trimmed);
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
        }
        Ok(Self {
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(BufReader::new(stdout)),
            protocol,
            next_id: AtomicU64::new(1),
            timeout: Duration::from_millis(timeout_ms),
        })
    }

    async fn call(&self, method: &str, params: Option<JsonValue>) -> ZeroBotResult<JsonValue> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let request = JsonRpcRequest {
            jsonrpc: "2.0",
            id: Some(id),
            method,
            params,
        };
        let payload =
            serde_json::to_string(&request).map_err(|err| ZeroBotError::Mcp(err.to_string()))?;
        {
            let mut stdin = self.stdin.lock().await;
            match self.protocol {
                McpLocalProtocol::ContentLength => {
                    let framed = format!(
                        "Content-Length: {}\r\n\r\n{}",
                        payload.as_bytes().len(),
                        payload
                    );
                    stdin.write_all(framed.as_bytes()).await?;
                }
                McpLocalProtocol::Line => {
                    stdin.write_all(payload.as_bytes()).await?;
                    stdin.write_all(b"\n").await?;
                }
            }
            stdin.flush().await?;
        }

        let mut stdout = self.stdout.lock().await;
        loop {
            let parsed = read_jsonrpc_response(&mut stdout, self.timeout).await?;
            if parsed.id != Some(id) {
                continue;
            }
            if let Some(err) = parsed.error {
                return Err(ZeroBotError::Mcp(format!(
                    "本地 MCP 错误: {} ({})",
                    err.message, err.code
                )));
            }
            return parsed
                .result
                .ok_or_else(|| ZeroBotError::Mcp("本地 MCP 返回空结果".to_string()));
        }
    }

    async fn notify_initialized(&self) -> ZeroBotResult<()> {
        let request = JsonRpcRequest {
            jsonrpc: "2.0",
            id: None,
            method: "notifications/initialized",
            params: None,
        };
        let payload =
            serde_json::to_string(&request).map_err(|err| ZeroBotError::Mcp(err.to_string()))?;
        let mut stdin = self.stdin.lock().await;
        match self.protocol {
            McpLocalProtocol::ContentLength => {
                let framed = format!(
                    "Content-Length: {}\r\n\r\n{}",
                    payload.as_bytes().len(),
                    payload
                );
                stdin.write_all(framed.as_bytes()).await?;
            }
            McpLocalProtocol::Line => {
                stdin.write_all(payload.as_bytes()).await?;
                stdin.write_all(b"\n").await?;
            }
        }
        stdin.flush().await?;
        Ok(())
    }
}

#[async_trait]
impl McpClient for LocalMcpClient {
    async fn initialize(&self) -> ZeroBotResult<()> {
        let params = serde_json::json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "zerobot", "version": "0.1.0" }
        });
        let _ = self.call("initialize", Some(params)).await?;
        let _ = self.notify_initialized().await;
        Ok(())
    }

    async fn list_tools(&self) -> ZeroBotResult<Vec<McpToolInfo>> {
        let result = self.call("tools/list", None).await?;
        parse_tools(result)
    }

    async fn call_tool(&self, name: &str, arguments: JsonValue) -> ZeroBotResult<JsonValue> {
        let params = serde_json::json!({
            "name": name,
            "arguments": arguments
        });
        self.call("tools/call", Some(params)).await
    }
}

fn parse_tools(result: JsonValue) -> ZeroBotResult<Vec<McpToolInfo>> {
    let tools = result
        .get("tools")
        .and_then(|value| value.as_array())
        .ok_or_else(|| ZeroBotError::Mcp("MCP tools/list 返回格式错误".to_string()))?;

    let mut out = Vec::new();
    for tool in tools {
        let name = tool
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ZeroBotError::Mcp("MCP 工具缺少 name".to_string()))?;
        let description = tool
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let parameters = tool
            .get("inputSchema")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({"type":"object"}));
        out.push(McpToolInfo {
            server: String::new(),
            name: name.to_string(),
            description,
            parameters,
        });
    }
    Ok(out)
}

async fn read_jsonrpc_response(
    stdout: &mut BufReader<ChildStdout>,
    timeout: Duration,
) -> ZeroBotResult<JsonRpcResponse> {
    let mut line = String::new();
    let mut content_length: Option<usize> = None;
    loop {
        line.clear();
        let read = tokio::time::timeout(timeout, stdout.read_line(&mut line)).await;
        let read = read.map_err(|_| ZeroBotError::Mcp("MCP 本地调用超时".to_string()))??;
        if read == 0 {
            return Err(ZeroBotError::Mcp("MCP 本地连接已关闭".to_string()));
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            if let Some(length) = content_length {
                let mut buf = vec![0u8; length];
                tokio::time::timeout(timeout, stdout.read_exact(&mut buf))
                    .await
                    .map_err(|_| ZeroBotError::Mcp("MCP 本地读取超时".to_string()))??;
                let text = String::from_utf8_lossy(&buf);
                let parsed: JsonRpcResponse = serde_json::from_str(&text)
                    .map_err(|err| ZeroBotError::Mcp(err.to_string()))?;
                return Ok(parsed);
            }
            continue;
        }

        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("content-length:") {
            let value = trimmed.splitn(2, ':').nth(1).unwrap_or("").trim();
            if let Ok(length) = value.parse::<usize>() {
                content_length = Some(length);
            }
            continue;
        }

        if trimmed.starts_with('{') {
            if let Ok(parsed) = serde_json::from_str::<JsonRpcResponse>(trimmed) {
                return Ok(parsed);
            }
        }
    }
}

pub fn format_tool_output(result: JsonValue) -> String {
    if let Some(content) = result.get("content").and_then(|v| v.as_array()) {
        let mut parts = Vec::new();
        for item in content {
            if item.get("type").and_then(|v| v.as_str()) == Some("text") {
                if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                    parts.push(text.to_string());
                }
            }
        }
        if !parts.is_empty() {
            return parts.join("\n");
        }
    }
    result.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::Method::POST;
    use httpmock::MockServer;
    use serde_json::json;

    #[tokio::test]
    async fn remote_mcp_list_and_call() {
        let server = MockServer::start();
        let _init_mock = server.mock(|when, then| {
            when.method(POST).body_contains("initialize");
            then.json_body(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"ok": true}
            }));
        });
        let list_mock = server.mock(|when, then| {
            when.method(POST).body_contains("tools/list");
            then.json_body(json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": {
                    "tools": [
                        {"name":"echo","description":"echo","inputSchema":{"type":"object"}}
                    ]
                }
            }));
        });
        let call_mock = server.mock(|when, then| {
            when.method(POST).body_contains("tools/call");
            then.json_body(json!({
                "jsonrpc": "2.0",
                "id": 3,
                "result": {
                    "content": [{"type":"text","text":"ok"}]
                }
            }));
        });

        let client = RemoteMcpClient::new(&server.url("/mcp"), &HashMap::new(), 5000);
        client.initialize().await.unwrap();
        let tools = client.list_tools().await.unwrap();
        assert_eq!(tools[0].name, "echo");
        let result = client
            .call_tool("echo", json!({"text":"hi"}))
            .await
            .unwrap();
        assert_eq!(format_tool_output(result), "ok");

        list_mock.assert_hits(1);
        call_mock.assert_hits(1);
    }
}
