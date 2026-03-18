use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::HashMap;

use crate::error::ZeroBotResult;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInputRequest {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub questions: Vec<UserInputQuestion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInputQuestion {
    pub id: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<UserInputOption>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInputOption {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UserInputAnswer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UserInputResponse {
    pub answers: HashMap<String, UserInputAnswer>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub cancelled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolApprovalRequest {
    pub tool_name: String,
    pub arguments: JsonValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolApprovalDecision {
    AllowOnce,
    AllowSession,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolApprovalResponse {
    pub decision: ToolApprovalDecision,
}

#[async_trait]
pub trait InteractionHandler: Send + Sync {
    async fn request_user_input(
        &self,
        request: UserInputRequest,
    ) -> ZeroBotResult<UserInputResponse>;
    async fn request_tool_approval(
        &self,
        request: ToolApprovalRequest,
    ) -> ZeroBotResult<ToolApprovalResponse>;
}
