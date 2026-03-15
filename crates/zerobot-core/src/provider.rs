use crate::error::{ZeroBotError, ZeroBotResult};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use futures::stream::{self, StreamExt};
use std::pin::Pin;
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
}

#[derive(Debug, Clone)]
pub enum ProviderEvent {
    TextDelta(String),
    ToolCall(ToolCall),
    Done,
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
                events.push(Ok(ProviderEvent::Done));
                stream::iter(events)
            }
            Err(err) => stream::iter(vec![Err(err)]),
        });
        Box::pin(stream)
    }
}

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
        let tools = request.tools.into_iter().map(|tool| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.parameters,
                }
            })
        }).collect::<Vec<_>>();

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
                    messages.push(serde_json::json!({
                        "role": "tool",
                        "tool_call_id": msg.tool_call_id,
                        "content": msg.content
                    }));
                }
                _ => {
                    messages.push(serde_json::json!({
                        "role": match msg.role {
                            ProviderMessageRole::System => "system",
                            ProviderMessageRole::User => "user",
                            ProviderMessageRole::Assistant => "assistant",
                            ProviderMessageRole::Tool => "tool",
                        },
                        "content": msg.content,
                        "name": msg.name,
                    }));
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

        let raw: JsonValue = serde_json::from_str(&text)
            .map_err(|err| ZeroBotError::Provider(err.to_string()))?;

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
                let id = call.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
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
                let arguments = serde_json::from_str(arguments_raw).unwrap_or(JsonValue::String(arguments_raw.to_string()));
                tool_calls.push(ToolCall { id, name, arguments });
            }
        }

        Ok(ProviderResponse {
            content,
            tool_calls,
            raw,
        })
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

        let raw: JsonValue = serde_json::from_str(&text)
            .map_err(|err| ZeroBotError::Provider(err.to_string()))?;

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
                            let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let arguments = item.get("input").cloned().unwrap_or(JsonValue::Null);
                            tool_calls.push(ToolCall { id, name, arguments });
                        }
                        _ => {}
                    }
                }
            }
        }

        Ok(ProviderResponse {
            content,
            tool_calls,
            raw,
        })
    }
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
