use crate::error::{ZeroBotError, ZeroBotResult};
use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_stream::Stream;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderMessageRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderMessage {
    pub role: ProviderMessageRole,
    pub content: String,
    pub tool_call_id: Option<String>,
    pub name: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: JsonValue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: JsonValue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderRequest {
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<ProviderMessage>,
    pub tools: Vec<ToolSpec>,
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderResponse {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub raw: JsonValue,
    pub usage: Option<TokenUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TokenUsage {
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub total_tokens: Option<u32>,
}

#[derive(Debug, Clone)]
pub enum ProviderEvent {
    TextDelta(String),
    ToolCall(ToolCall),
    Usage(TokenUsage),
    Done,
}

#[derive(Debug, Default, Clone)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> &str;
    async fn send(&self, request: ProviderRequest) -> ZeroBotResult<ProviderResponse>;

    fn stream(
        &self,
        request: ProviderRequest,
    ) -> Pin<Box<dyn Stream<Item = ZeroBotResult<ProviderEvent>> + Send + '_>> {
        let stream = stream::once(self.send(request)).flat_map(|result| match result {
            Ok(response) => {
                let mut events = Vec::new();
                if !response.content.is_empty() {
                    events.push(Ok(ProviderEvent::TextDelta(response.content)));
                }
                for call in response.tool_calls {
                    events.push(Ok(ProviderEvent::ToolCall(call)));
                }
                if let Some(usage) = response.usage {
                    events.push(Ok(ProviderEvent::Usage(usage)));
                }
                events.push(Ok(ProviderEvent::Done));
                stream::iter(events)
            }
            Err(err) => stream::iter(vec![Err(err)]),
        });
        Box::pin(stream)
    }
}

pub type ProviderFactory = Arc<dyn Fn() -> ZeroBotResult<Box<dyn Provider>> + Send + Sync>;

#[derive(Debug, Clone)]
pub struct OpenAIProvider {
    client: Client,
    api_key: String,
    base_url: String,
}

impl OpenAIProvider {
    pub fn new(api_key: String, base_url: Option<String>) -> Self {
        Self {
            client: Client::new(),
            api_key,
            base_url: base_url.unwrap_or_else(|| "https://api.openai.com/v1".to_string()),
        }
    }
}

#[async_trait]
impl Provider for OpenAIProvider {
    fn id(&self) -> &str {
        "openai"
    }

    async fn send(&self, request: ProviderRequest) -> ZeroBotResult<ProviderResponse> {
        if self.api_key.is_empty() {
            return Err(ZeroBotError::Provider("OpenAI API Key 为空".to_string()));
        }

        let url = format!("{}/chat/completions", self.base_url);
        let tools = request
            .tools
            .into_iter()
            .map(|tool| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.parameters,
                    }
                })
            })
            .collect::<Vec<_>>();

        let mut messages = Vec::new();
        if let Some(system) = request.system {
            messages.push(serde_json::json!({
                "role": "system",
                "content": system
            }));
        }
        for msg in request.messages {
            match msg.role {
                ProviderMessageRole::Tool => {
                    let Some(call_id) = msg.tool_call_id else {
                        return Err(ZeroBotError::Provider(
                            "工具消息缺少 tool_call_id".to_string(),
                        ));
                    };
                    messages.push(serde_json::json!({
                        "role": "tool",
                        "tool_call_id": call_id,
                        "content": msg.content
                    }));
                }
                _ => {
                    let mut message = serde_json::json!({
                        "role": match msg.role {
                            ProviderMessageRole::System => "system",
                            ProviderMessageRole::User => "user",
                            ProviderMessageRole::Assistant => "assistant",
                            ProviderMessageRole::Tool => "tool",
                        },
                        "content": msg.content,
                    });
                    if let Some(name) = msg.name {
                        if let Some(obj) = message.as_object_mut() {
                            obj.insert("name".to_string(), serde_json::Value::String(name));
                        }
                    }
                    if matches!(msg.role, ProviderMessageRole::Assistant) {
                        if let Some(calls) = msg.tool_calls {
                            let tool_calls = calls
                                .into_iter()
                                .map(|call| {
                                    serde_json::json!({
                                        "id": call.id,
                                        "type": "function",
                                        "function": {
                                            "name": call.name,
                                            "arguments": call.arguments.to_string()
                                        }
                                    })
                                })
                                .collect::<Vec<_>>();
                            if let Some(obj) = message.as_object_mut() {
                                obj.insert(
                                    "tool_calls".to_string(),
                                    serde_json::Value::Array(tool_calls),
                                );
                            }
                        }
                    }
                    messages.push(message);
                }
            }
        }

        let payload = serde_json::json!({
            "model": request.model,
            "messages": messages,
            "tools": tools,
            "tool_choice": "auto",
        });

        let response = self
            .client
            .post(url)
            .bearer_auth(&self.api_key)
            .json(&payload)
            .send()
            .await?;

        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            return Err(ZeroBotError::Provider(format!(
                "OpenAI 请求失败: {status} {text}"
            )));
        }

        let raw: JsonValue =
            serde_json::from_str(&text).map_err(|err| ZeroBotError::Provider(err.to_string()))?;

        let content = raw
            .get("choices")
            .and_then(|v| v.get(0))
            .and_then(|v| v.get("message"))
            .and_then(|v| v.get("content"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let mut tool_calls = Vec::new();
        if let Some(calls) = raw
            .get("choices")
            .and_then(|v| v.get(0))
            .and_then(|v| v.get("message"))
            .and_then(|v| v.get("tool_calls"))
            .and_then(|v| v.as_array())
        {
            for call in calls {
                let id = call
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = call
                    .get("function")
                    .and_then(|v| v.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let arguments_raw = call
                    .get("function")
                    .and_then(|v| v.get("arguments"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("{}");
                let arguments = serde_json::from_str(arguments_raw)
                    .unwrap_or(JsonValue::String(arguments_raw.to_string()));
                tool_calls.push(ToolCall {
                    id,
                    name,
                    arguments,
                });
            }
        }

        let usage = raw.get("usage").and_then(parse_openai_usage);

        Ok(ProviderResponse {
            content,
            tool_calls,
            raw,
            usage,
        })
    }

    fn stream(
        &self,
        request: ProviderRequest,
    ) -> Pin<Box<dyn Stream<Item = ZeroBotResult<ProviderEvent>> + Send + '_>> {
        let (tx, rx) = mpsc::unbounded_channel();
        let client = self.client.clone();
        let api_key = self.api_key.clone();
        let base_url = self.base_url.clone();

        tokio::spawn(async move {
            let url = format!("{}/chat/completions", base_url);
            let tools = request
                .tools
                .into_iter()
                .map(|tool| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.parameters,
                        }
                    })
                })
                .collect::<Vec<_>>();

            let mut messages = Vec::new();
            if let Some(system) = request.system {
                messages.push(serde_json::json!({
                    "role": "system",
                    "content": system
                }));
            }
            for msg in request.messages {
                match msg.role {
                    ProviderMessageRole::Tool => {
                        let Some(call_id) = msg.tool_call_id else {
                            let _ = tx.send(Err(ZeroBotError::Provider(
                                "工具消息缺少 tool_call_id".to_string(),
                            )));
                            return;
                        };
                        messages.push(serde_json::json!({
                            "role": "tool",
                            "tool_call_id": call_id,
                            "content": msg.content
                        }));
                    }
                    _ => {
                        let mut message = serde_json::json!({
                            "role": match msg.role {
                                ProviderMessageRole::System => "system",
                                ProviderMessageRole::User => "user",
                                ProviderMessageRole::Assistant => "assistant",
                                ProviderMessageRole::Tool => "tool",
                            },
                            "content": msg.content,
                        });
                        if let Some(name) = msg.name {
                            if let Some(obj) = message.as_object_mut() {
                                obj.insert("name".to_string(), serde_json::Value::String(name));
                            }
                        }
                        if matches!(msg.role, ProviderMessageRole::Assistant) {
                            if let Some(calls) = msg.tool_calls {
                                let tool_calls = calls
                                    .into_iter()
                                    .map(|call| {
                                        serde_json::json!({
                                            "id": call.id,
                                            "type": "function",
                                            "function": {
                                                "name": call.name,
                                                "arguments": call.arguments.to_string()
                                            }
                                        })
                                    })
                                    .collect::<Vec<_>>();
                                if let Some(obj) = message.as_object_mut() {
                                    obj.insert(
                                        "tool_calls".to_string(),
                                        serde_json::Value::Array(tool_calls),
                                    );
                                }
                            }
                        }
                        messages.push(message);
                    }
                }
            }

            let payload = serde_json::json!({
                "model": request.model,
                "messages": messages,
                "tools": tools,
                "tool_choice": "auto",
                "stream": true,
                "stream_options": { "include_usage": true }
            });

            let response = client
                .post(url)
                .bearer_auth(api_key)
                .json(&payload)
                .send()
                .await;
            let response = match response {
                Ok(resp) => resp,
                Err(err) => {
                    let _ = tx.send(Err(ZeroBotError::Provider(err.to_string())));
                    return;
                }
            };
            let status = response.status();
            if !status.is_success() {
                let text = response.text().await.unwrap_or_default();
                let _ = tx.send(Err(ZeroBotError::Provider(format!(
                    "OpenAI 请求失败: {status} {text}"
                ))));
                return;
            }

            let mut tool_calls: Vec<PartialToolCall> = Vec::new();
            let mut last_usage: Option<TokenUsage> = None;
            let mut done = false;

            let mut data_lines: Vec<String> = Vec::new();
            let mut line_buf = String::new();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(err) => {
                        let _ = tx.send(Err(ZeroBotError::Provider(err.to_string())));
                        return;
                    }
                };
                let text = String::from_utf8_lossy(&chunk);
                for ch in text.chars() {
                    if ch == '\n' {
                        let line = line_buf.trim_end_matches('\r').to_string();
                        line_buf.clear();
                        if line.is_empty() {
                            if !data_lines.is_empty() {
                                let data = data_lines.join("\n");
                                data_lines.clear();
                                if data.trim() == "[DONE]" {
                                    done = true;
                                    break;
                                }
                                if let Ok(raw) = serde_json::from_str::<JsonValue>(&data) {
                                    if raw.get("error").is_some() {
                                        let _ =
                                            tx.send(Err(ZeroBotError::Provider(raw.to_string())));
                                        return;
                                    }
                                    if let Some(choices) =
                                        raw.get("choices").and_then(|v| v.as_array())
                                    {
                                        for choice in choices {
                                            if let Some(delta) = choice.get("delta") {
                                                if let Some(text) =
                                                    delta.get("content").and_then(|v| v.as_str())
                                                {
                                                    let _ = tx.send(Ok(ProviderEvent::TextDelta(
                                                        text.to_string(),
                                                    )));
                                                }
                                                if let Some(calls) = delta
                                                    .get("tool_calls")
                                                    .and_then(|v| v.as_array())
                                                {
                                                    for call in calls {
                                                        let index = call
                                                            .get("index")
                                                            .and_then(|v| v.as_u64())
                                                            .unwrap_or(0)
                                                            as usize;
                                                        if tool_calls.len() <= index {
                                                            tool_calls.resize_with(
                                                                index + 1,
                                                                PartialToolCall::default,
                                                            );
                                                        }
                                                        let entry = &mut tool_calls[index];
                                                        if let Some(id) =
                                                            call.get("id").and_then(|v| v.as_str())
                                                        {
                                                            entry.id = id.to_string();
                                                        }
                                                        if let Some(func) = call.get("function") {
                                                            if let Some(name) = func
                                                                .get("name")
                                                                .and_then(|v| v.as_str())
                                                            {
                                                                entry.name = name.to_string();
                                                            }
                                                            if let Some(args) = func
                                                                .get("arguments")
                                                                .and_then(|v| v.as_str())
                                                            {
                                                                entry.arguments.push_str(args);
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    if let Some(usage) =
                                        raw.get("usage").and_then(parse_openai_usage)
                                    {
                                        last_usage = Some(usage.clone());
                                        let _ = tx.send(Ok(ProviderEvent::Usage(usage)));
                                    }
                                }
                            }
                        } else if let Some(rest) = line.strip_prefix("data:") {
                            data_lines.push(rest.trim_start().to_string());
                        }
                    } else {
                        line_buf.push(ch);
                    }
                }
                if done {
                    break;
                }
            }

            for call in tool_calls.into_iter() {
                if call.name.is_empty() {
                    continue;
                }
                let arguments = serde_json::from_str(&call.arguments)
                    .unwrap_or(JsonValue::String(call.arguments));
                let id = if call.id.is_empty() {
                    uuid::Uuid::new_v4().to_string()
                } else {
                    call.id
                };
                let _ = tx.send(Ok(ProviderEvent::ToolCall(ToolCall {
                    id,
                    name: call.name,
                    arguments,
                })));
            }
            if let Some(usage) = last_usage {
                let _ = tx.send(Ok(ProviderEvent::Usage(usage)));
            }
            let _ = tx.send(Ok(ProviderEvent::Done));
        });

        Box::pin(UnboundedReceiverStream::new(rx))
    }
}

#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    client: Client,
    api_key: String,
    base_url: String,
}

impl AnthropicProvider {
    pub fn new(api_key: String, base_url: Option<String>) -> Self {
        Self {
            client: Client::new(),
            api_key,
            base_url: base_url.unwrap_or_else(|| "https://api.anthropic.com/v1".to_string()),
        }
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn id(&self) -> &str {
        "anthropic"
    }

    async fn send(&self, request: ProviderRequest) -> ZeroBotResult<ProviderResponse> {
        if self.api_key.is_empty() {
            return Err(ZeroBotError::Provider("Anthropic API Key 为空".to_string()));
        }

        let url = format!("{}/messages", self.base_url);

        let tools = request
            .tools
            .into_iter()
            .map(|tool| {
                serde_json::json!({
                    "name": tool.name,
                    "description": tool.description,
                    "input_schema": tool.parameters,
                })
            })
            .collect::<Vec<_>>();

        let mut messages = Vec::new();
        for msg in request.messages {
            match msg.role {
                ProviderMessageRole::Tool => {
                    messages.push(serde_json::json!({
                        "role": "user",
                        "content": [{
                            "type": "tool_result",
                            "tool_use_id": msg.tool_call_id,
                            "content": msg.content,
                        }]
                    }));
                }
                ProviderMessageRole::System => {
                    // Anthropic system prompt 放在顶层字段
                }
                _ => {
                    messages.push(serde_json::json!({
                        "role": match msg.role {
                            ProviderMessageRole::User => "user",
                            ProviderMessageRole::Assistant => "assistant",
                            _ => "user",
                        },
                        "content": [{
                            "type": "text",
                            "text": msg.content
                        }]
                    }));
                }
            }
        }

        let payload = serde_json::json!({
            "model": request.model,
            "max_tokens": request.max_tokens.unwrap_or(1024),
            "system": request.system,
            "messages": messages,
            "tools": tools,
        });

        let response = self
            .client
            .post(url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&payload)
            .send()
            .await?;

        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            return Err(ZeroBotError::Provider(format!(
                "Anthropic 请求失败: {status} {text}"
            )));
        }

        let raw: JsonValue =
            serde_json::from_str(&text).map_err(|err| ZeroBotError::Provider(err.to_string()))?;

        let mut content = String::new();
        let mut tool_calls = Vec::new();
        if let Some(items) = raw.get("content").and_then(|v| v.as_array()) {
            for item in items {
                if let Some(kind) = item.get("type").and_then(|v| v.as_str()) {
                    match kind {
                        "text" => {
                            if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                                content.push_str(text);
                            }
                        }
                        "tool_use" => {
                            let id = item
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let name = item
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let arguments = item.get("input").cloned().unwrap_or(JsonValue::Null);
                            tool_calls.push(ToolCall {
                                id,
                                name,
                                arguments,
                            });
                        }
                        _ => {}
                    }
                }
            }
        }

        let usage = raw.get("usage").and_then(parse_anthropic_usage);

        Ok(ProviderResponse {
            content,
            tool_calls,
            raw,
            usage,
        })
    }

    fn stream(
        &self,
        request: ProviderRequest,
    ) -> Pin<Box<dyn Stream<Item = ZeroBotResult<ProviderEvent>> + Send + '_>> {
        let (tx, rx) = mpsc::unbounded_channel();
        let client = self.client.clone();
        let api_key = self.api_key.clone();
        let base_url = self.base_url.clone();

        tokio::spawn(async move {
            if api_key.is_empty() {
                let _ = tx.send(Err(ZeroBotError::Provider(
                    "Anthropic API Key 为空".to_string(),
                )));
                return;
            }

            let url = format!("{}/messages", base_url);
            let tools = request
                .tools
                .into_iter()
                .map(|tool| {
                    serde_json::json!({
                        "name": tool.name,
                        "description": tool.description,
                        "input_schema": tool.parameters,
                    })
                })
                .collect::<Vec<_>>();

            let mut messages = Vec::new();
            for msg in request.messages {
                match msg.role {
                    ProviderMessageRole::Tool => {
                        messages.push(serde_json::json!({
                            "role": "user",
                            "content": [{
                                "type": "tool_result",
                                "tool_use_id": msg.tool_call_id,
                                "content": msg.content,
                            }]
                        }));
                    }
                    ProviderMessageRole::System => {}
                    _ => {
                        messages.push(serde_json::json!({
                            "role": match msg.role {
                                ProviderMessageRole::User => "user",
                                ProviderMessageRole::Assistant => "assistant",
                                _ => "user",
                            },
                            "content": [{
                                "type": "text",
                                "text": msg.content
                            }]
                        }));
                    }
                }
            }

            let payload = serde_json::json!({
                "model": request.model,
                "max_tokens": request.max_tokens.unwrap_or(1024),
                "system": request.system,
                "messages": messages,
                "tools": tools,
                "stream": true,
            });

            let response = client
                .post(url)
                .header("x-api-key", &api_key)
                .header("anthropic-version", "2023-06-01")
                .json(&payload)
                .send()
                .await;
            let response = match response {
                Ok(resp) => resp,
                Err(err) => {
                    let _ = tx.send(Err(ZeroBotError::Provider(err.to_string())));
                    return;
                }
            };
            let status = response.status();
            if !status.is_success() {
                let text = response.text().await.unwrap_or_default();
                let _ = tx.send(Err(ZeroBotError::Provider(format!(
                    "Anthropic 请求失败: {status} {text}"
                ))));
                return;
            }

            let mut tool_calls: Vec<PartialToolCall> = Vec::new();
            let mut last_usage: TokenUsage = TokenUsage::default();
            let mut done = false;

            let mut event_name: Option<String> = None;
            let mut data_lines: Vec<String> = Vec::new();
            let mut line_buf = String::new();

            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(err) => {
                        let _ = tx.send(Err(ZeroBotError::Provider(err.to_string())));
                        return;
                    }
                };
                let text = String::from_utf8_lossy(&chunk);
                for ch in text.chars() {
                    if ch == '\n' {
                        let line = line_buf.trim_end_matches('\r').to_string();
                        line_buf.clear();
                        if line.is_empty() {
                            if !data_lines.is_empty() {
                                let data = data_lines.join("\n");
                                data_lines.clear();
                                let evt = event_name.take();
                                if let Ok(raw) = serde_json::from_str::<JsonValue>(&data) {
                                    if raw.get("error").is_some() {
                                        let _ =
                                            tx.send(Err(ZeroBotError::Provider(raw.to_string())));
                                        return;
                                    }
                                    let evt_type = evt
                                        .as_deref()
                                        .or_else(|| raw.get("type").and_then(|v| v.as_str()))
                                        .unwrap_or("");

                                    match evt_type {
                                        "message_start" => {
                                            if let Some(usage) = raw
                                                .get("message")
                                                .and_then(|v| v.get("usage"))
                                                .and_then(parse_anthropic_usage)
                                            {
                                                last_usage.input_tokens = usage.input_tokens;
                                                last_usage.output_tokens = usage.output_tokens;
                                                last_usage.total_tokens = usage.total_tokens;
                                                let _ = tx.send(Ok(ProviderEvent::Usage(
                                                    last_usage.clone(),
                                                )));
                                            }
                                        }
                                        "message_delta" => {
                                            if let Some(usage) =
                                                raw.get("usage").and_then(parse_anthropic_usage)
                                            {
                                                if usage.input_tokens.is_some() {
                                                    last_usage.input_tokens = usage.input_tokens;
                                                }
                                                if usage.output_tokens.is_some() {
                                                    last_usage.output_tokens = usage.output_tokens;
                                                }
                                                if usage.total_tokens.is_some() {
                                                    last_usage.total_tokens = usage.total_tokens;
                                                }
                                                let _ = tx.send(Ok(ProviderEvent::Usage(
                                                    last_usage.clone(),
                                                )));
                                            }
                                        }
                                        "content_block_start" => {
                                            if let Some(block) = raw.get("content_block") {
                                                let block_type = block
                                                    .get("type")
                                                    .and_then(|v| v.as_str())
                                                    .unwrap_or("");
                                                if block_type == "text" {
                                                    if let Some(text) =
                                                        block.get("text").and_then(|v| v.as_str())
                                                    {
                                                        if !text.is_empty() {
                                                            let _ = tx.send(Ok(
                                                                ProviderEvent::TextDelta(
                                                                    text.to_string(),
                                                                ),
                                                            ));
                                                        }
                                                    }
                                                } else if block_type == "tool_use" {
                                                    let index = raw
                                                        .get("index")
                                                        .and_then(|v| v.as_u64())
                                                        .unwrap_or(0)
                                                        as usize;
                                                    if tool_calls.len() <= index {
                                                        tool_calls.resize_with(
                                                            index + 1,
                                                            PartialToolCall::default,
                                                        );
                                                    }
                                                    let entry = &mut tool_calls[index];
                                                    if let Some(id) =
                                                        block.get("id").and_then(|v| v.as_str())
                                                    {
                                                        entry.id = id.to_string();
                                                    }
                                                    if let Some(name) =
                                                        block.get("name").and_then(|v| v.as_str())
                                                    {
                                                        entry.name = name.to_string();
                                                    }
                                                    if let Some(input) = block.get("input") {
                                                        if let Ok(text) =
                                                            serde_json::to_string(input)
                                                        {
                                                            entry.arguments = text;
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        "content_block_delta" => {
                                            if let Some(delta) = raw.get("delta") {
                                                let delta_type = delta
                                                    .get("type")
                                                    .and_then(|v| v.as_str())
                                                    .unwrap_or("");
                                                if delta_type == "text_delta" {
                                                    if let Some(text) =
                                                        delta.get("text").and_then(|v| v.as_str())
                                                    {
                                                        let _ =
                                                            tx.send(Ok(ProviderEvent::TextDelta(
                                                                text.to_string(),
                                                            )));
                                                    }
                                                } else if delta_type == "input_json_delta" {
                                                    let index = raw
                                                        .get("index")
                                                        .and_then(|v| v.as_u64())
                                                        .unwrap_or(0)
                                                        as usize;
                                                    if tool_calls.len() <= index {
                                                        tool_calls.resize_with(
                                                            index + 1,
                                                            PartialToolCall::default,
                                                        );
                                                    }
                                                    if let Some(partial) = delta
                                                        .get("partial_json")
                                                        .and_then(|v| v.as_str())
                                                    {
                                                        tool_calls[index]
                                                            .arguments
                                                            .push_str(partial);
                                                    }
                                                }
                                            }
                                        }
                                        "message_stop" => {
                                            done = true;
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        } else if let Some(rest) = line.strip_prefix("event:") {
                            event_name = Some(rest.trim().to_string());
                        } else if let Some(rest) = line.strip_prefix("data:") {
                            data_lines.push(rest.trim_start().to_string());
                        }
                    } else {
                        line_buf.push(ch);
                    }
                }
                if done {
                    break;
                }
            }

            for call in tool_calls.into_iter() {
                if call.name.is_empty() {
                    continue;
                }
                let arguments = serde_json::from_str(&call.arguments)
                    .unwrap_or(JsonValue::String(call.arguments));
                let id = if call.id.is_empty() {
                    uuid::Uuid::new_v4().to_string()
                } else {
                    call.id
                };
                let _ = tx.send(Ok(ProviderEvent::ToolCall(ToolCall {
                    id,
                    name: call.name,
                    arguments,
                })));
            }
            if last_usage.input_tokens.is_some()
                || last_usage.output_tokens.is_some()
                || last_usage.total_tokens.is_some()
            {
                let _ = tx.send(Ok(ProviderEvent::Usage(last_usage)));
            }
            let _ = tx.send(Ok(ProviderEvent::Done));
        });

        Box::pin(UnboundedReceiverStream::new(rx))
    }
}

fn parse_openai_usage(raw: &JsonValue) -> Option<TokenUsage> {
    let prompt_tokens = raw.get("prompt_tokens").and_then(|v| v.as_u64());
    let completion_tokens = raw.get("completion_tokens").and_then(|v| v.as_u64());
    let total_tokens = raw.get("total_tokens").and_then(|v| v.as_u64());
    if prompt_tokens.is_none() && completion_tokens.is_none() && total_tokens.is_none() {
        return None;
    }
    Some(TokenUsage {
        input_tokens: prompt_tokens.map(|v| v as u32),
        output_tokens: completion_tokens.map(|v| v as u32),
        total_tokens: total_tokens.map(|v| v as u32),
    })
}

fn parse_anthropic_usage(raw: &JsonValue) -> Option<TokenUsage> {
    let input_tokens = raw.get("input_tokens").and_then(|v| v.as_u64());
    let output_tokens = raw.get("output_tokens").and_then(|v| v.as_u64());
    if input_tokens.is_none() && output_tokens.is_none() {
        return None;
    }
    let total_tokens = match (input_tokens, output_tokens) {
        (Some(i), Some(o)) => Some(i + o),
        _ => None,
    };
    Some(TokenUsage {
        input_tokens: input_tokens.map(|v| v as u32),
        output_tokens: output_tokens.map(|v| v as u32),
        total_tokens: total_tokens.map(|v| v as u32),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::Method::POST;
    use httpmock::MockServer;
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn openai_provider_parses_tool_calls() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(200).json_body(serde_json::json!({
                "choices": [{
                    "message": {
                        "content": "ok",
                        "tool_calls": [
                            {
                                "id": "call-1",
                                "function": {
                                    "name": "read",
                                    "arguments": "{\"path\":\"README.md\"}"
                                }
                            }
                        ]
                    }
                }]
            }));
        });

        let provider = OpenAIProvider::new("key".to_string(), Some(server.base_url()));
        let response = provider
            .send(ProviderRequest {
                model: "gpt-test".to_string(),
                system: None,
                messages: vec![ProviderMessage {
                    role: ProviderMessageRole::User,
                    content: "hi".to_string(),
                    tool_call_id: None,
                    name: None,
                    tool_calls: None,
                }],
                tools: vec![],
                max_tokens: None,
            })
            .await
            .unwrap();

        mock.assert();
        assert_eq!(response.content, "ok");
        assert_eq!(response.tool_calls.len(), 1);
    }

    #[tokio::test]
    async fn anthropic_provider_parses_tool_calls() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/messages");
            then.status(200).json_body(serde_json::json!({
                "content": [
                    {"type": "text", "text": "ok"},
                    {"type": "tool_use", "id": "tool-1", "name": "grep", "input": {"pattern": "foo"}}
                ]
            }));
        });

        let provider = AnthropicProvider::new("key".to_string(), Some(server.base_url()));
        let response = provider
            .send(ProviderRequest {
                model: "claude-test".to_string(),
                system: None,
                messages: vec![ProviderMessage {
                    role: ProviderMessageRole::User,
                    content: "hi".to_string(),
                    tool_call_id: None,
                    name: None,
                    tool_calls: None,
                }],
                tools: vec![],
                max_tokens: None,
            })
            .await
            .unwrap();

        mock.assert();
        assert_eq!(response.content, "ok");
        assert_eq!(response.tool_calls.len(), 1);
    }
}
