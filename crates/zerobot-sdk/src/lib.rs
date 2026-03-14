use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct ZerobotClient {
    base_url: String,
    api_key: String,
    client: reqwest::Client,
}

impl ZerobotClient {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            client: reqwest::Client::new(),
        }
    }

    pub async fn create_session(&self, title: Option<String>) -> anyhow::Result<String> {
        let resp: CreateSessionResponse = self
            .client
            .post(format!("{}/v1/sessions", self.base_url))
            .header("x-zerobot-api-key", &self.api_key)
            .json(&CreateSessionRequest { title })
            .send()
            .await?
            .json()
            .await?;
        Ok(resp.session_id)
    }

    pub async fn get_session(&self, session_id: &str) -> anyhow::Result<zerobot_core::SessionState> {
        let resp = self
            .client
            .get(format!("{}/v1/sessions/{}", self.base_url, session_id))
            .header("x-zerobot-api-key", &self.api_key)
            .send()
            .await?
            .error_for_status()?;
        Ok(resp.json().await?)
    }

    pub async fn send_message(&self, session_id: &str, content: String) -> anyhow::Result<()> {
        self
            .client
            .post(format!("{}/v1/sessions/{}/messages", self.base_url, session_id))
            .header("x-zerobot-api-key", &self.api_key)
            .json(&MessageRequest { content })
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    pub async fn list_tools(&self) -> anyhow::Result<Vec<zerobot_core::ToolDefinition>> {
        let tools = self
            .client
            .get(format!("{}/v1/tools", self.base_url))
            .header("x-zerobot-api-key", &self.api_key)
            .send()
            .await?
            .json()
            .await?;
        Ok(tools)
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct CreateSessionRequest {
    title: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CreateSessionResponse {
    session_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct MessageRequest {
    content: String,
}
