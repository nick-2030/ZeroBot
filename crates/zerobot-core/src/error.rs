use thiserror::Error;

pub type ZeroBotResult<T> = Result<T, ZeroBotError>;

#[derive(Debug, Error)]
pub enum ZeroBotError {
    #[error("配置错误: {0}")]
    Config(String),
    #[error("提供商错误: {0}")]
    Provider(String),
    #[error("会话存储错误: {0}")]
    SessionStore(String),
    #[error("工具执行错误: {0}")]
    Tool(String),
    #[error("代理执行错误: {0}")]
    Agent(String),
    #[error("MCP 错误: {0}")]
    Mcp(String),
    #[error("Skill 错误: {0}")]
    Skill(String),
    #[error("IO 错误: {0}")]
    Io(String),
    #[error("网络请求错误: {0}")]
    Http(String),
    #[error("任务错误: {0}")]
    Task(String),
    #[error("编排深度超限: {0}")]
    OrchestrationDepthExceeded(String),
    #[error("通知错误: {0}")]
    Notification(String),
    #[error("Kanban 错误: {0}")]
    Kanban(String),
    #[error("Swarm 错误: {0}")]
    Swarm(String),
}

impl From<std::io::Error> for ZeroBotError {
    fn from(err: std::io::Error) -> Self {
        ZeroBotError::Io(err.to_string())
    }
}

impl From<reqwest::Error> for ZeroBotError {
    fn from(err: reqwest::Error) -> Self {
        ZeroBotError::Http(err.to_string())
    }
}

impl From<sqlx::Error> for ZeroBotError {
    fn from(err: sqlx::Error) -> Self {
        ZeroBotError::SessionStore(err.to_string())
    }
}
