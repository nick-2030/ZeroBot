use async_trait::async_trait;
use futures::future::BoxFuture;
use serde_json::Value as JsonValue;
use std::sync::Arc;
use zerobot_core::tool::{Tool, ToolContext, ToolOutput};

/// A function that handles a tool invocation.
pub type ToolHandler =
    Arc<dyn Fn(JsonValue) -> BoxFuture<'static, Result<String, String>> + Send + Sync>;

/// Defines a custom tool for the SDK.
///
/// Build via `ToolDefinition::new(name, description, parameters, handler)`.
pub struct ToolDefinition {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) parameters: JsonValue,
    pub(crate) handler: ToolHandler,
    pub(crate) read_only: bool,
}

impl ToolDefinition {
    /// Create a new tool definition.
    ///
    /// - `name`: Tool name (must be unique).
    /// - `description`: Human-readable description for the LLM.
    /// - `parameters`: JSON Schema for the tool's parameters.
    /// - `handler`: Async function that receives the arguments and returns output.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: JsonValue,
        handler: ToolHandler,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
            handler,
            read_only: false,
        }
    }

    /// Mark this tool as read-only (allows concurrent execution).
    pub fn read_only(mut self) -> Self {
        self.read_only = true;
        self
    }
}

/// Internal adapter that bridges SDK `ToolDefinition` to core's `Tool` trait.
pub(crate) struct SdkToolAdapter {
    def: ToolDefinition,
}

impl SdkToolAdapter {
    pub fn from_definition(def: ToolDefinition) -> Self {
        Self { def }
    }
}

#[async_trait]
impl Tool for SdkToolAdapter {
    fn name(&self) -> &str {
        &self.def.name
    }

    fn description(&self) -> &str {
        &self.def.description
    }

    fn parameters(&self) -> JsonValue {
        self.def.parameters.clone()
    }

    async fn run(
        &self,
        _ctx: &ToolContext,
        args: JsonValue,
    ) -> zerobot_core::ZeroBotResult<ToolOutput> {
        let handler = self.def.handler.clone();
        let output = handler(args)
            .await
            .map_err(zerobot_core::ZeroBotError::Tool)?;
        Ok(ToolOutput::new(output))
    }

    fn is_read_only(&self) -> bool {
        self.def.read_only
    }
}
