use async_trait::async_trait;

/// MCP 扩展点：目前只预留接口
#[async_trait]
pub trait McpClient: Send + Sync {
    async fn list_tools(&self) -> Vec<String>;
}
