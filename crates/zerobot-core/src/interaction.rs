use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::HashMap;

use crate::config::{PermissionMode, PermissionSource, ToolApprovalMode};
use crate::error::ZeroBotResult;

/// Tracks WHY a permission decision was made.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PermissionReason {
    /// Decision based on session-level permission mode.
    Mode(PermissionMode),
    /// Decision based on per-tool approval rule.
    ToolRule {
        source: PermissionSource,
        tool_name: String,
    },
    /// Decision based on content-level rule (tool name + input pattern).
    ContentRule {
        pattern: String,
        source: PermissionSource,
    },
    /// Decision based on bash/skill command rule.
    CommandRule {
        pattern: String,
        source: PermissionSource,
    },
    /// Tool was approved in current session.
    SessionApproval {
        key: String,
    },
    /// Tool was approved at workspace level (persisted to disk).
    WorkspaceApproval {
        key: String,
    },
    /// Decision came from a hook.
    HookDecision {
        hook_name: String,
    },
    /// Denial threshold exceeded, falling back to interactive.
    DenialThreshold {
        consecutive: u32,
        total: u32,
    },
}

impl std::fmt::Display for PermissionReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PermissionReason::Mode(mode) => write!(f, "Mode: {mode}"),
            PermissionReason::ToolRule { tool_name, .. } => {
                write!(f, "Tool rule: {tool_name}")
            }
            PermissionReason::ContentRule { pattern, .. } => {
                write!(f, "Content rule: {pattern}")
            }
            PermissionReason::CommandRule { pattern, .. } => {
                write!(f, "Command rule: {pattern}")
            }
            PermissionReason::SessionApproval { key } => {
                write!(f, "Session approval: {key}")
            }
            PermissionReason::WorkspaceApproval { key } => {
                write!(f, "Workspace approval: {key}")
            }
            PermissionReason::HookDecision { hook_name } => {
                write!(f, "Hook: {hook_name}")
            }
            PermissionReason::DenialThreshold {
                consecutive,
                total,
            } => {
                write!(
                    f,
                    "Denial threshold: {consecutive} consecutive, {total} total"
                )
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserInputRequest {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub questions: Vec<UserInputQuestion>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserInputQuestion {
    pub id: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<UserInputOption>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserInputOption {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct UserInputAnswer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
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
    /// Pre-computed auto-decision (if the permission engine already determined a result).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_decision: Option<ToolApprovalMode>,
    /// Why the auto-decision was made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_reason: Option<PermissionReason>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolApprovalDecision {
    AllowOnce,
    AllowSession,
    AllowWorkspace,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolApprovalResponse {
    pub decision: ToolApprovalDecision,
    /// Reason for the user's decision (for audit trail).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<PermissionReason>,
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
