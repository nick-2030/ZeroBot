use thiserror::Error;

pub type SdkResult<T> = Result<T, SdkError>;

#[derive(Debug, Error)]
pub enum SdkError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("provider error: {0}")]
    Provider(String),

    #[error("session error: {0}")]
    Session(String),

    #[error("tool error: {0}")]
    Tool(String),

    #[error("agent error: {0}")]
    Agent(String),

    #[error("query aborted")]
    Aborted,

    #[error(transparent)]
    Core(#[from] zerobot_core::ZeroBotError),
}

impl From<anyhow::Error> for SdkError {
    fn from(err: anyhow::Error) -> Self {
        SdkError::Agent(err.to_string())
    }
}
