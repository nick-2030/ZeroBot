use std::collections::BTreeMap;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use zerobot_core::{LlmProviderSettings, ZeroSettings};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub messages: Vec<LlmMessage>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct ChatResponse {
    pub provider: String,
    pub model: String,
    pub output: String,
    pub raw: Value,
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
        }],
        temperature: Some(0.2),
        max_tokens: Some(128),
    };
    let chat_resp = chat(settings, chat_req).await?;
    Ok(LlmTestResponse {
        provider: chat_resp.provider,
        model: chat_resp.model,
        output: chat_resp.output,
        raw: chat_resp.raw,
    })
}

pub async fn chat_stream<F>(
    settings: &ZeroSettings,
    request: ChatRequest,
    mut on_chunk: F,
) -> anyhow::Result<String>
where
    F: FnMut(&str) + Send,
{
    let provider = request
        .provider
        .clone()
        .or_else(|| settings.llm.default_provider.clone())
        .unwrap_or_else(|| "openai".to_string());

    let result = match provider.as_str() {
        "openai" => chat_openai_stream(settings, request.clone(), &mut on_chunk).await,
        "anthropic" => chat_anthropic_stream(settings, request.clone(), &mut on_chunk).await,
        other => Err(anyhow::anyhow!("unknown provider: {}", other)),
    };

    match result {
        Ok(output) => Ok(output),
        Err(_) => {
            let fallback = chat(settings, request).await?;
            emit_chunked(&fallback.output, &mut on_chunk);
            Ok(fallback.output)
        }
    }
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
    let body = serde_json::json!({
        "model": model,
        "messages": request.messages,
        "temperature": request.temperature.unwrap_or(0.2)
    });

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
    Ok(ChatResponse {
        provider: "openai".to_string(),
        model,
        output,
        raw: json,
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
        "messages": messages
    });
    if let Some(system) = system {
        if let Some(obj) = body.as_object_mut() {
            obj.insert("system".to_string(), Value::String(system));
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
    Ok(ChatResponse {
        provider: "anthropic".to_string(),
        model,
        output,
        raw: json,
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
    let provider = settings
        .llm
        .openai
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("openai provider not configured"))?;
    let (base_url, api_key, model) = resolve_provider(provider, settings, request.model)?;

    let url = format!("{}/v1/chat/completions", base_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": model,
        "messages": request.messages,
        "temperature": request.temperature.unwrap_or(0.2),
        "stream": true
    });

    let client = reqwest::Client::new();
    let mut req = client.post(url).bearer_auth(api_key).json(&body);
    req = apply_headers(req, &provider.headers);
    let resp = req.send().await?.error_for_status()?;
    let mut stream = resp.bytes_stream();

    let mut buffer = String::new();
    let mut output = String::new();
    while let Some(chunk) = futures::StreamExt::next(&mut stream).await {
        let chunk = chunk?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(idx) = buffer.find('\n') {
            let line = buffer[..idx].trim().to_string();
            buffer = buffer[idx + 1..].to_string();
            if let Some(data) = line.strip_prefix("data:") {
                let data = data.trim();
                if data == "[DONE]" {
                    return Ok(output);
                }
                if let Ok(value) = serde_json::from_str::<Value>(data) {
                    if let Some(text) = extract_openai_chunk(&value) {
                        on_chunk(text);
                        output.push_str(text);
                    }
                }
            }
        }
    }
    Ok(output)
}

async fn chat_anthropic_stream<F>(
    settings: &ZeroSettings,
    request: ChatRequest,
    on_chunk: &mut F,
) -> anyhow::Result<String>
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
        "messages": messages,
        "stream": true
    });
    if let Some(system) = system {
        if let Some(obj) = body.as_object_mut() {
            obj.insert("system".to_string(), Value::String(system));
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
    while let Some(chunk) = futures::StreamExt::next(&mut stream).await {
        let chunk = chunk?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(idx) = buffer.find('\n') {
            let line = buffer[..idx].trim().to_string();
            buffer = buffer[idx + 1..].to_string();
            if let Some(data) = line.strip_prefix("data:") {
                let data = data.trim();
                if data == "[DONE]" {
                    return Ok(output);
                }
                if let Ok(value) = serde_json::from_str::<Value>(data) {
                    if let Some(text) = extract_anthropic_chunk(&value) {
                        on_chunk(text);
                        output.push_str(text);
                    }
                }
            }
        }
    }
    Ok(output)
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

fn emit_chunked<F>(text: &str, on_chunk: &mut F)
where
    F: FnMut(&str),
{
    let mut buf = String::new();
    for ch in text.chars() {
        buf.push(ch);
        if buf.chars().count() >= 12 {
            on_chunk(&buf);
            buf.clear();
        }
    }
    if !buf.is_empty() {
        on_chunk(&buf);
    }
}
