use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub name: String,
    pub output: Value,
    pub is_error: bool,
}

pub fn builtin_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "read".to_string(),
            description: "Read a file from the workspace".to_string(),
            input_schema: serde_json::json!({"type":"object","properties":{"path":{"type":"string"}}}),
        },
        ToolDefinition {
            name: "write".to_string(),
            description: "Write a file to the workspace".to_string(),
            input_schema: serde_json::json!({"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}}}),
        },
        ToolDefinition {
            name: "edit".to_string(),
            description: "Edit a file by replacing a string".to_string(),
            input_schema: serde_json::json!({"type":"object","properties":{"path":{"type":"string"},"old":{"type":"string"},"new":{"type":"string"}}}),
        },
        ToolDefinition {
            name: "grep".to_string(),
            description: "Search for a pattern in files".to_string(),
            input_schema: serde_json::json!({"type":"object","properties":{"pattern":{"type":"string"},"glob":{"type":"string"}}}),
        },
        ToolDefinition {
            name: "glob".to_string(),
            description: "List files matching a glob".to_string(),
            input_schema: serde_json::json!({"type":"object","properties":{"pattern":{"type":"string"}}}),
        },
        ToolDefinition {
            name: "find".to_string(),
            description: "Find files by name".to_string(),
            input_schema: serde_json::json!({"type":"object","properties":{"name":{"type":"string"}}}),
        },
        ToolDefinition {
            name: "delete".to_string(),
            description: "Delete a file".to_string(),
            input_schema: serde_json::json!({"type":"object","properties":{"path":{"type":"string"}}}),
        },
        ToolDefinition {
            name: "bash".to_string(),
            description: "Execute a shell command".to_string(),
            input_schema: serde_json::json!({"type":"object","properties":{"cmd":{"type":"string"}}}),
        },
        ToolDefinition {
            name: "todo_write".to_string(),
            description: "Create a TODO list".to_string(),
            input_schema: serde_json::json!({"type":"object","properties":{"items":{"type":"array","items":{"type":"string"}}}}),
        },
        ToolDefinition {
            name: "todo_update".to_string(),
            description: "Update TODO items".to_string(),
            input_schema: serde_json::json!({"type":"object","properties":{"items":{"type":"array","items":{"type":"string"}}}}),
        },
    ]
}
