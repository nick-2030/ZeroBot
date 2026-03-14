use std::collections::BTreeMap;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use zerobot_core::{LlmProviderSettings, ZeroSettings};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "tool_call_id")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<OpenAiToolCall>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiToolFunction {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: OpenAiToolFunction,
}

#[derive(Debug, Clone, Default)]
struct OpenAiFunctionCallBuilder {
    name: String,
    arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub messages: Vec<LlmMessage>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub tools: Vec<ToolSpec>,
    pub tool_choice: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ChatResponse {
    pub provider: String,
    pub model: String,
    pub output: String,
    pub raw: Value,
    pub tool_calls: Vec<ToolCall>,
}

#[derive(Debug, Serialize)]
pub struct LlmTestResponse {
    pub provider: String,
    pub model: String,
    pub output: String,
    pub raw: Value,
}

#[derive(Debug, Deserialize)]
pub struct LlmTestRequest {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub prompt: String,
}

pub async fn test_llm(
    settings: &ZeroSettings,
    request: LlmTestRequest,
) -> anyhow::Result<LlmTestResponse> {
    let chat_req = ChatRequest {
        provider: request.provider.clone(),
        model: request.model.clone(),
        messages: vec![LlmMessage {
            role: "user".to_string(),
            content: request.prompt,
            name: None,
            tool_call_id: None,
            tool_calls: None,
        }],
        temperature: Some(0.2),
        max_tokens: Some(128),
        tools: Vec::new(),
        tool_choice: None,
    };
    let chat_resp = chat(settings, chat_req).await?;
    Ok(LlmTestResponse {
        provider: chat_resp.provider,
        model: chat_resp.model,
        output: chat_resp.output,
        raw: chat_resp.raw,
    })
}

pub async fn chat_stream_with_tools<F>(
    settings: &ZeroSettings,
    request: ChatRequest,
    mut on_chunk: F,
) -> anyhow::Result<ChatResponse>
where
    F: FnMut(&str) + Send,
{
    let provider = request
        .provider
        .clone()
        .or_else(|| settings.llm.default_provider.clone())
        .unwrap_or_else(|| "openai".to_string());

    let result = match provider.as_str() {
        "openai" => chat_openai_stream_with_tools(settings, request.clone(), &mut on_chunk).await,
        "anthropic" => chat_anthropic_stream_with_tools(settings, request.clone(), &mut on_chunk).await,
        other => Err(anyhow::anyhow!("unknown provider: {}", other)),
    };

    match result {
        Ok(resp) => Ok(resp),
        Err(_) => chat(settings, request).await,
    }
}

pub async fn chat_stream<F>(
    settings: &ZeroSettings,
    request: ChatRequest,
    mut on_chunk: F,
) -> anyhow::Result<String>
where
    F: FnMut(&str) + Send,
{
    let resp = chat_stream_with_tools(settings, request, &mut on_chunk).await?;
    Ok(resp.output)
}

pub async fn chat(settings: &ZeroSettings, request: ChatRequest) -> anyhow::Result<ChatResponse> {
    let provider = request
        .provider
        .clone()
        .or_else(|| settings.llm.default_provider.clone())
        .unwrap_or_else(|| "openai".to_string());

    match provider.as_str() {
        "openai" => chat_openai(settings, request).await,
        "anthropic" => chat_anthropic(settings, request).await,
        other => anyhow::bail!("unknown provider: {}", other),
    }
}

async fn chat_openai(settings: &ZeroSettings, request: ChatRequest) -> anyhow::Result<ChatResponse> {
    let provider = settings
        .llm
        .openai
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("openai provider not configured"))?;
    let (base_url, api_key, model) = resolve_provider(provider, settings, request.model)?;

    let url = format!("{}/v1/chat/completions", base_url.trim_end_matches('/'));
    let mut body = serde_json::json!({
        "model": model,
        "messages": request.messages,
        "temperature": request.temperature.unwrap_or(0.2),
    });
    if !request.tools.is_empty() {
        let tools = request
            .tools
            .iter()
            .map(|tool| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.input_schema
                    }
                })
            })
            .collect::<Vec<_>>();
        if let Some(obj) = body.as_object_mut() {
            obj.insert("tools".to_string(), Value::Array(tools));
            obj.insert(
                "tool_choice".to_string(),
                Value::String(request.tool_choice.unwrap_or_else(|| "auto".to_string())),
            );
        }
    }

    let client = reqwest::Client::new();
    let mut req = client.post(url).bearer_auth(api_key).json(&body);
    req = apply_headers(req, &provider.headers);
    let resp = req.send().await?.error_for_status()?;
    let json: Value = resp.json().await?;
    let output = json
        .get("choices")
        .and_then(|v| v.get(0))
        .and_then(|v| v.get("message"))
        .and_then(|v| v.get("content"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let tool_calls = parse_openai_tool_calls(&json);
    Ok(ChatResponse {
        provider: "openai".to_string(),
        model,
        output,
        raw: json,
        tool_calls,
    })
}

async fn chat_anthropic(settings: &ZeroSettings, request: ChatRequest) -> anyhow::Result<ChatResponse> {
    let provider = settings
        .llm
        .anthropic
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("anthropic provider not configured"))?;
    let (base_url, api_key, model) = resolve_provider(provider, settings, request.model)?;

    let (system, messages) = split_system_messages(request.messages);
    let url = format!("{}/v1/messages", base_url.trim_end_matches('/'));
    let mut body = serde_json::json!({
        "model": model,
        "max_tokens": request.max_tokens.unwrap_or(256),
        "messages": build_anthropic_messages(&messages)
    });
    if let Some(system) = system {
        if let Some(obj) = body.as_object_mut() {
            obj.insert("system".to_string(), Value::String(system));
        }
    }
    if !request.tools.is_empty() {
        let tools = request
            .tools
            .iter()
            .map(|tool| {
                serde_json::json!({
                    "name": tool.name,
                    "description": tool.description,
                    "input_schema": tool.input_schema
                })
            })
            .collect::<Vec<_>>();
        if let Some(obj) = body.as_object_mut() {
            obj.insert("tools".to_string(), Value::Array(tools));
        }
    }

    let client = reqwest::Client::new();
    let mut req = client
        .post(url)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .json(&body);
    req = apply_headers(req, &provider.headers);
    let resp = req.send().await?.error_for_status()?;
    let json: Value = resp.json().await?;
    let output = json
        .get("content")
        .and_then(|v| v.get(0))
        .and_then(|v| v.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let tool_calls = parse_anthropic_tool_calls(&json);
    Ok(ChatResponse {
        provider: "anthropic".to_string(),
        model,
        output,
        raw: json,
        tool_calls,
    })
}

async fn chat_openai_stream_with_tools<F>(
    settings: &ZeroSettings,
    request: ChatRequest,
    on_chunk: &mut F,
) -> anyhow::Result<ChatResponse>
where
    F: FnMut(&str) + Send,
{
    let provider = settings
        .llm
        .openai
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("openai provider not configured"))?;
    let (base_url, api_key, model) = resolve_provider(provider, settings, request.model)?;

    let url = format!("{}/v1/chat/completions", base_url.trim_end_matches('/'));
    let mut body = serde_json::json!({
        "model": model,
        "messages": request.messages,
        "temperature": request.temperature.unwrap_or(0.2),
        "stream": true
    });
    if !request.tools.is_empty() {
        let tools = request
            .tools
            .iter()
            .map(|tool| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.input_schema
                    }
                })
            })
            .collect::<Vec<_>>();
        if let Some(obj) = body.as_object_mut() {
            obj.insert("tools".to_string(), Value::Array(tools));
            obj.insert(
                "tool_choice".to_string(),
                Value::String(request.tool_choice.unwrap_or_else(|| "auto".to_string())),
            );
        }
    }

    let client = reqwest::Client::new();
    let mut req = client.post(url).bearer_auth(api_key).json(&body);
    req = apply_headers(req, &provider.headers);
    let resp = req.send().await?.error_for_status()?;
    let mut stream = resp.bytes_stream();

    let mut buffer = String::new();
    let mut output = String::new();
    let mut tool_builders: Vec<OpenAiToolCallBuilder> = Vec::new();
    let mut function_builder = OpenAiFunctionCallBuilder::default();
    while let Some(chunk) = futures::StreamExt::next(&mut stream).await {
        let chunk = chunk?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(idx) = buffer.find('\n') {
            let line = buffer[..idx].trim().to_string();
            buffer = buffer[idx + 1..].to_string();
            if let Some(data) = line.strip_prefix("data:") {
                let data = data.trim();
                if data == "[DONE]" {
                    let mut tool_calls = finalize_openai_tool_calls(&tool_builders);
                    if tool_calls.is_empty() {
                        if let Some(call) = finalize_openai_function_call(&function_builder) {
                            tool_calls.push(call);
                        }
                    }
                    return Ok(ChatResponse {
                        provider: "openai".to_string(),
                        model,
                        output,
                        raw: Value::Null,
                        tool_calls,
                    });
                }
                if let Ok(value) = serde_json::from_str::<Value>(data) {
                    let delta = value
                        .get("choices")
                        .and_then(|v| v.get(0))
                        .and_then(|v| v.get("delta"));
                    if let Some(delta) = delta {
                        if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
                            on_chunk(text);
                            output.push_str(text);
                        }
                        if let Some(calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                            update_openai_tool_builders(&mut tool_builders, calls);
                        }
                        if let Some(function_call) = delta.get("function_call") {
                            update_openai_function_builder(&mut function_builder, function_call);
                        }
                    } else if let Some(text) = extract_openai_chunk(&value) {
                        on_chunk(text);
                        output.push_str(text);
                    }
                }
            }
        }
    }
    let mut tool_calls = finalize_openai_tool_calls(&tool_builders);
    if tool_calls.is_empty() {
        if let Some(call) = finalize_openai_function_call(&function_builder) {
            tool_calls.push(call);
        }
    }
    Ok(ChatResponse {
        provider: "openai".to_string(),
        model,
        output,
        raw: Value::Null,
        tool_calls,
    })
}

async fn chat_anthropic_stream_with_tools<F>(
    settings: &ZeroSettings,
    request: ChatRequest,
    on_chunk: &mut F,
) -> anyhow::Result<ChatResponse>
where
    F: FnMut(&str) + Send,
{
    let provider = settings
        .llm
        .anthropic
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("anthropic provider not configured"))?;
    let (base_url, api_key, model) = resolve_provider(provider, settings, request.model)?;

    let (system, messages) = split_system_messages(request.messages);
    let url = format!("{}/v1/messages", base_url.trim_end_matches('/'));
    let mut body = serde_json::json!({
        "model": model,
        "max_tokens": request.max_tokens.unwrap_or(256),
        "messages": build_anthropic_messages(&messages),
        "stream": true
    });
    if let Some(system) = system {
        if let Some(obj) = body.as_object_mut() {
            obj.insert("system".to_string(), Value::String(system));
        }
    }
    if !request.tools.is_empty() {
        let tools = request
            .tools
            .iter()
            .map(|tool| {
                serde_json::json!({
                    "name": tool.name,
                    "description": tool.description,
                    "input_schema": tool.input_schema
                })
            })
            .collect::<Vec<_>>();
        if let Some(obj) = body.as_object_mut() {
            obj.insert("tools".to_string(), Value::Array(tools));
        }
    }

    let client = reqwest::Client::new();
    let mut req = client
        .post(url)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .json(&body);
    req = apply_headers(req, &provider.headers);
    let resp = req.send().await?.error_for_status()?;
    let mut stream = resp.bytes_stream();

    let mut buffer = String::new();
    let mut output = String::new();
    let mut tool_builders: Vec<AnthropicToolCallBuilder> = Vec::new();
    while let Some(chunk) = futures::StreamExt::next(&mut stream).await {
        let chunk = chunk?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(idx) = buffer.find('\n') {
            let line = buffer[..idx].trim().to_string();
            buffer = buffer[idx + 1..].to_string();
            if let Some(data) = line.strip_prefix("data:") {
                let data = data.trim();
                if data == "[DONE]" {
                    let tool_calls = finalize_anthropic_tool_calls(&tool_builders);
                    return Ok(ChatResponse {
                        provider: "anthropic".to_string(),
                        model,
                        output,
                        raw: Value::Null,
                        tool_calls,
                    });
                }
                if let Ok(value) = serde_json::from_str::<Value>(data) {
                    if let Some(text) = extract_anthropic_chunk(&value) {
                        on_chunk(text);
                        output.push_str(text);
                    }
                    update_anthropic_tool_builders(&mut tool_builders, &value);
                }
            }
        }
    }
    let tool_calls = finalize_anthropic_tool_calls(&tool_builders);
    Ok(ChatResponse {
        provider: "anthropic".to_string(),
        model,
        output,
        raw: Value::Null,
        tool_calls,
    })
}

async fn chat_openai_stream<F>(
    settings: &ZeroSettings,
    request: ChatRequest,
    on_chunk: &mut F,
) -> anyhow::Result<String>
where
    F: FnMut(&str) + Send,
{
    let resp = chat_openai_stream_with_tools(settings, request, on_chunk).await?;
    Ok(resp.output)
}

async fn chat_anthropic_stream<F>(
    settings: &ZeroSettings,
    request: ChatRequest,
    on_chunk: &mut F,
) -> anyhow::Result<String>
where
    F: FnMut(&str) + Send,
{
    let resp = chat_anthropic_stream_with_tools(settings, request, on_chunk).await?;
    Ok(resp.output)
}

fn resolve_provider(
    provider: &LlmProviderSettings,
    settings: &ZeroSettings,
    override_model: Option<String>,
) -> anyhow::Result<(String, String, String)> {
    let base_url = provider
        .base_url
        .clone()
        .ok_or_else(|| anyhow::anyhow!("provider base_url missing"))?;
    let api_key = provider
        .api_key
        .clone()
        .ok_or_else(|| anyhow::anyhow!("provider api_key missing"))?;
    let model = override_model
        .or_else(|| provider.model.clone())
        .or_else(|| settings.llm.default_model.clone())
        .ok_or_else(|| anyhow::anyhow!("model not provided"))?;
    Ok((base_url, api_key, model))
}

fn apply_headers(
    req: reqwest::RequestBuilder,
    headers: &BTreeMap<String, String>,
) -> reqwest::RequestBuilder {
    if headers.is_empty() {
        return req;
    }
    let mut header_map = HeaderMap::new();
    for (key, value) in headers {
        if let (Ok(name), Ok(val)) = (
            HeaderName::from_bytes(key.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            header_map.insert(name, val);
        }
    }
    req.headers(header_map)
}

fn split_system_messages(messages: Vec<LlmMessage>) -> (Option<String>, Vec<LlmMessage>) {
    let mut system_parts = Vec::new();
    let mut rest = Vec::new();
    for msg in messages {
        if msg.role == "system" {
            system_parts.push(msg.content);
        } else {
            rest.push(msg);
        }
    }
    if system_parts.is_empty() {
        (None, rest)
    } else {
        (Some(system_parts.join("\n")), rest)
    }
}

fn extract_openai_chunk(value: &Value) -> Option<&str> {
    value
        .get("choices")
        .and_then(|v| v.get(0))
        .and_then(|v| v.get("delta"))
        .and_then(|v| v.get("content"))
        .and_then(|v| v.as_str())
        .or_else(|| {
            value
                .get("choices")
                .and_then(|v| v.get(0))
                .and_then(|v| v.get("message"))
                .and_then(|v| v.get("content"))
                .and_then(|v| v.as_str())
        })
        .or_else(|| {
            value
                .get("choices")
                .and_then(|v| v.get(0))
                .and_then(|v| v.get("text"))
                .and_then(|v| v.as_str())
        })
}

fn extract_anthropic_chunk(value: &Value) -> Option<&str> {
    match value.get("type").and_then(|v| v.as_str()) {
        Some("content_block_delta") => value
            .get("delta")
            .and_then(|v| v.get("text"))
            .and_then(|v| v.as_str()),
        Some("content_block_start") => value
            .get("content_block")
            .and_then(|v| v.get("text"))
            .and_then(|v| v.as_str()),
        _ => None,
    }
}

fn parse_openai_tool_calls(value: &Value) -> Vec<ToolCall> {
    let mut calls = Vec::new();
    let Some(message) = value
        .get("choices")
        .and_then(|v| v.get(0))
        .and_then(|v| v.get("message"))
    else {
        return calls;
    };
    if let Some(array) = message.get("tool_calls").and_then(|v| v.as_array()) {
        for call in array {
            let id = call.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let function = call.get("function");
            let name = function
                .and_then(|v| v.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let args_raw = function.and_then(|v| v.get("arguments"));
            let arguments = match args_raw {
                Some(Value::String(s)) => serde_json::from_str::<Value>(s).unwrap_or(Value::String(s.clone())),
                Some(v) => v.clone(),
                None => Value::Null,
            };
            if !name.is_empty() {
                calls.push(ToolCall { id, name, arguments });
            }
        }
    }
    if calls.is_empty() {
        if let Some(call) = parse_openai_function_call(message) {
            calls.push(call);
        }
    }
    calls
}

fn parse_openai_function_call(message: &Value) -> Option<ToolCall> {
    let function = message.get("function_call")?;
    let name = function.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if name.is_empty() {
        return None;
    }
    let args_raw = function.get("arguments");
    let arguments = match args_raw {
        Some(Value::String(s)) => serde_json::from_str::<Value>(s).unwrap_or(Value::String(s.clone())),
        Some(v) => v.clone(),
        None => Value::Null,
    };
    Some(ToolCall {
        id: String::new(),
        name,
        arguments,
    })
}

fn parse_anthropic_tool_calls(value: &Value) -> Vec<ToolCall> {
    let mut calls = Vec::new();
    let Some(content) = value.get("content").and_then(|v| v.as_array()) else {
        return calls;
    };
    for block in content {
        if block.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
            let id = block.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let arguments = block.get("input").cloned().unwrap_or(Value::Null);
            if !name.is_empty() {
                calls.push(ToolCall { id, name, arguments });
            }
        }
    }
    calls
}

fn build_anthropic_messages(messages: &[LlmMessage]) -> Vec<Value> {
    let mut output = Vec::new();
    for msg in messages {
        if msg.role == "tool" {
            let tool_id = msg.tool_call_id.clone().unwrap_or_default();
            let content = vec![serde_json::json!({
                "type": "tool_result",
                "tool_use_id": tool_id,
                "content": msg.content,
            })];
            output.push(serde_json::json!({
                "role": "user",
                "content": content
            }));
        } else if msg.role == "assistant"
            && msg.tool_calls.as_ref().map(|v| !v.is_empty()).unwrap_or(false)
        {
            let mut content_blocks = Vec::new();
            if !msg.content.is_empty() {
                content_blocks.push(serde_json::json!({
                    "type": "text",
                    "text": msg.content
                }));
            }
            if let Some(calls) = &msg.tool_calls {
                for call in calls {
                    let input = serde_json::from_str::<Value>(&call.function.arguments)
                        .unwrap_or_else(|_| Value::String(call.function.arguments.clone()));
                    content_blocks.push(serde_json::json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.function.name,
                        "input": input
                    }));
                }
            }
            output.push(serde_json::json!({
                "role": "assistant",
                "content": content_blocks
            }));
        } else {
            output.push(serde_json::json!({
                "role": msg.role,
                "content": msg.content
            }));
        }
    }
    output
}

fn tool_call_to_openai(call: &ToolCall) -> OpenAiToolCall {
    let arguments =
        serde_json::to_string(&call.arguments).unwrap_or_else(|_| call.arguments.to_string());
    OpenAiToolCall {
        id: call.id.clone(),
        call_type: "function".to_string(),
        function: OpenAiToolFunction {
            name: call.name.clone(),
            arguments,
        },
    }
}

pub fn to_openai_tool_calls(calls: &[ToolCall]) -> Vec<OpenAiToolCall> {
    calls.iter().map(tool_call_to_openai).collect()
}

#[derive(Debug, Clone, Default)]
struct OpenAiToolCallBuilder {
    id: String,
    name: String,
    arguments: String,
}

fn update_openai_tool_builders(builders: &mut Vec<OpenAiToolCallBuilder>, calls: &[Value]) {
    for call in calls {
        let index = call
            .get("index")
            .and_then(|v| v.as_u64())
            .unwrap_or(builders.len() as u64);
        let idx = index as usize;
        if builders.len() <= idx {
            builders.resize_with(idx + 1, OpenAiToolCallBuilder::default);
        }
        let builder = &mut builders[idx];
        if let Some(id) = call.get("id").and_then(|v| v.as_str()) {
            builder.id = id.to_string();
        }
        if let Some(function) = call.get("function") {
            if let Some(name) = function.get("name").and_then(|v| v.as_str()) {
                builder.name = name.to_string();
            }
            if let Some(args) = function.get("arguments").and_then(|v| v.as_str()) {
                builder.arguments.push_str(args);
            }
        }
    }
}

fn finalize_openai_tool_calls(builders: &[OpenAiToolCallBuilder]) -> Vec<ToolCall> {
    let mut calls = Vec::new();
    for builder in builders {
        if builder.name.is_empty() {
            continue;
        }
        let args = if builder.arguments.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str::<Value>(&builder.arguments)
                .unwrap_or(Value::String(builder.arguments.clone()))
        };
        calls.push(ToolCall {
            id: builder.id.clone(),
            name: builder.name.clone(),
            arguments: args,
        });
    }
    calls
}

fn update_openai_function_builder(builder: &mut OpenAiFunctionCallBuilder, value: &Value) {
    if let Some(name) = value.get("name").and_then(|v| v.as_str()) {
        builder.name = name.to_string();
    }
    if let Some(args) = value.get("arguments").and_then(|v| v.as_str()) {
        builder.arguments.push_str(args);
    }
}

fn finalize_openai_function_call(builder: &OpenAiFunctionCallBuilder) -> Option<ToolCall> {
    if builder.name.is_empty() {
        return None;
    }
    let arguments = if builder.arguments.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str::<Value>(&builder.arguments)
            .unwrap_or(Value::String(builder.arguments.clone()))
    };
    Some(ToolCall {
        id: String::new(),
        name: builder.name.clone(),
        arguments,
    })
}

#[derive(Debug, Clone, Default)]
struct AnthropicToolCallBuilder {
    id: String,
    name: String,
    input_raw: String,
    input: Option<Value>,
}

fn update_anthropic_tool_builders(builders: &mut Vec<AnthropicToolCallBuilder>, value: &Value) {
    let event_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match event_type {
        "content_block_start" => {
            let content_block = value.get("content_block");
            if content_block
                .and_then(|v| v.get("type"))
                .and_then(|v| v.as_str())
                != Some("tool_use")
            {
                return;
            }
            let index = value
                .get("index")
                .and_then(|v| v.as_u64())
                .unwrap_or(builders.len() as u64);
            let idx = index as usize;
            if builders.len() <= idx {
                builders.resize_with(idx + 1, AnthropicToolCallBuilder::default);
            }
            let builder = &mut builders[idx];
            if let Some(id) = content_block.and_then(|v| v.get("id")).and_then(|v| v.as_str()) {
                builder.id = id.to_string();
            }
            if let Some(name) = content_block
                .and_then(|v| v.get("name"))
                .and_then(|v| v.as_str())
            {
                builder.name = name.to_string();
            }
            if let Some(input) = content_block.and_then(|v| v.get("input")) {
                builder.input = Some(input.clone());
            }
        }
        "content_block_delta" => {
            let index = value.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
            let idx = index as usize;
            if builders.len() <= idx {
                builders.resize_with(idx + 1, AnthropicToolCallBuilder::default);
            }
            let builder = &mut builders[idx];
            if let Some(delta) = value.get("delta") {
                if let Some(partial) = delta.get("partial_json").and_then(|v| v.as_str()) {
                    builder.input_raw.push_str(partial);
                } else if delta.get("type").and_then(|v| v.as_str()) == Some("input_json_delta") {
                    if let Some(partial) = delta.get("partial_json").and_then(|v| v.as_str()) {
                        builder.input_raw.push_str(partial);
                    }
                }
            } else if let Some(partial) = value.get("partial_json").and_then(|v| v.as_str()) {
                builder.input_raw.push_str(partial);
            }
        }
        _ => {}
    }
}

fn finalize_anthropic_tool_calls(builders: &[AnthropicToolCallBuilder]) -> Vec<ToolCall> {
    let mut calls = Vec::new();
    for builder in builders {
        if builder.name.is_empty() {
            continue;
        }
        let arguments = if !builder.input_raw.trim().is_empty() {
            serde_json::from_str::<Value>(&builder.input_raw)
                .unwrap_or(Value::String(builder.input_raw.clone()))
        } else {
            builder.input.clone().unwrap_or(Value::Null)
        };
        calls.push(ToolCall {
            id: builder.id.clone(),
            name: builder.name.clone(),
            arguments,
        });
    }
    calls
}
