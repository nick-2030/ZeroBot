use std::collections::BTreeMap;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use zerobot_core::{LlmProviderSettings, ZeroSettings};

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
    let provider = request
        .provider
        .clone()
        .or_else(|| settings.llm.default_provider.clone())
        .unwrap_or_else(|| "openai".to_string());

    match provider.as_str() {
        "openai" => test_openai(settings, request).await,
        "anthropic" => test_anthropic(settings, request).await,
        other => anyhow::bail!("unknown provider: {}", other),
    }
}

async fn test_openai(settings: &ZeroSettings, request: LlmTestRequest) -> anyhow::Result<LlmTestResponse> {
    let provider = settings
        .llm
        .openai
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("openai provider not configured"))?;
    let (base_url, api_key, model) = resolve_provider(provider, settings, request.model)?;

    let url = format!("{}/v1/chat/completions", base_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "user", "content": request.prompt}
        ],
        "temperature": 0.2
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
    Ok(LlmTestResponse {
        provider: "openai".to_string(),
        model,
        output,
        raw: json,
    })
}

async fn test_anthropic(settings: &ZeroSettings, request: LlmTestRequest) -> anyhow::Result<LlmTestResponse> {
    let provider = settings
        .llm
        .anthropic
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("anthropic provider not configured"))?;
    let (base_url, api_key, model) = resolve_provider(provider, settings, request.model)?;

    let url = format!("{}/v1/messages", base_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": model,
        "max_tokens": 256,
        "messages": [
            {"role": "user", "content": request.prompt}
        ]
    });

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
    Ok(LlmTestResponse {
        provider: "anthropic".to_string(),
        model,
        output,
        raw: json,
    })
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
