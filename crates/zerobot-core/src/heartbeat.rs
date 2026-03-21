use crate::error::{ZeroBotError, ZeroBotResult};
use crate::provider::{
    ProviderFactory, ProviderMessage, ProviderMessageRole, ProviderRequest, ToolSpec,
};
use futures::future::BoxFuture;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::{sleep, Duration};

pub type HeartbeatExecuteHandler =
    Arc<dyn Fn(String) -> BoxFuture<'static, ZeroBotResult<String>> + Send + Sync>;
pub type HeartbeatNotifyHandler =
    Arc<dyn Fn(String) -> BoxFuture<'static, ZeroBotResult<()>> + Send + Sync>;

#[derive(Debug, Clone)]
pub struct HeartbeatDecision {
    pub action: String,
    pub tasks: String,
}

#[derive(Clone)]
pub struct HeartbeatService {
    workspace: PathBuf,
    heartbeat_file: String,
    provider_factory: ProviderFactory,
    model: String,
    interval_s: u64,
    enabled: bool,
    on_execute: Option<HeartbeatExecuteHandler>,
    on_notify: Option<HeartbeatNotifyHandler>,
    running: Arc<AtomicBool>,
    task: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl HeartbeatService {
    pub fn new(
        workspace: PathBuf,
        provider_factory: ProviderFactory,
        model: String,
        heartbeat_file: String,
        interval_s: u64,
        enabled: bool,
        on_execute: Option<HeartbeatExecuteHandler>,
        on_notify: Option<HeartbeatNotifyHandler>,
    ) -> Self {
        Self {
            workspace,
            heartbeat_file,
            provider_factory,
            model,
            interval_s,
            enabled,
            on_execute,
            on_notify,
            running: Arc::new(AtomicBool::new(false)),
            task: Arc::new(Mutex::new(None)),
        }
    }

    pub fn heartbeat_file(&self) -> PathBuf {
        let path = PathBuf::from(&self.heartbeat_file);
        if path.is_absolute() {
            path
        } else {
            self.workspace.join(path)
        }
    }

    pub async fn start(&self) -> ZeroBotResult<()> {
        if !self.enabled {
            return Ok(());
        }
        if self.running.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        let this = self.clone();
        let mut guard = self.task.lock().await;
        *guard = Some(tokio::spawn(async move {
            while this.running.load(Ordering::SeqCst) {
                sleep(Duration::from_secs(this.interval_s)).await;
                if !this.running.load(Ordering::SeqCst) {
                    break;
                }
                if let Err(err) = this.tick().await {
                    tracing::warn!("heartbeat tick error: {}", err);
                }
            }
        }));
        Ok(())
    }

    pub async fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        let mut guard = self.task.lock().await;
        if let Some(task) = guard.take() {
            task.abort();
        }
    }

    pub async fn trigger_now(&self) -> ZeroBotResult<Option<String>> {
        let content = self.read_heartbeat_file().await?;
        let Some(content) = content else {
            return Ok(None);
        };
        let decision = self.decide(&content).await?;
        if decision.action != "run" {
            return Ok(None);
        }
        let Some(exec) = self.on_execute.clone() else {
            return Ok(None);
        };
        let output = exec(decision.tasks.clone()).await?;
        if !output.trim().is_empty() {
            if let Some(notify) = self.on_notify.clone() {
                notify(output.clone()).await?;
            }
        }
        Ok(Some(output))
    }

    pub async fn tick(&self) -> ZeroBotResult<()> {
        let content = self.read_heartbeat_file().await?;
        let Some(content) = content else {
            return Ok(());
        };

        let decision = self.decide(&content).await?;
        if decision.action != "run" {
            return Ok(());
        }

        let Some(exec) = self.on_execute.clone() else {
            return Ok(());
        };

        let output = exec(decision.tasks.clone()).await?;
        if output.trim().is_empty() {
            return Ok(());
        }
        if let Some(notify) = self.on_notify.clone() {
            notify(output).await?;
        }
        Ok(())
    }

    async fn read_heartbeat_file(&self) -> ZeroBotResult<Option<String>> {
        read_file_if_exists(&self.heartbeat_file()).await
    }

    pub async fn decide(&self, content: &str) -> ZeroBotResult<HeartbeatDecision> {
        let provider = (self.provider_factory)()?;
        let request = ProviderRequest {
            model: self.model.clone(),
            system: Some("You are a heartbeat agent. Call the heartbeat tool to report decision.".to_string()),
            messages: vec![ProviderMessage {
                role: ProviderMessageRole::User,
                content: format!(
                    "Current Time: {}\n\nReview the following HEARTBEAT.md and decide whether there are active tasks.\n\n{}",
                    chrono::Local::now().to_rfc3339(),
                    content,
                ),
                tool_call_id: None,
                name: None,
                tool_calls: None,
            }],
            tools: vec![ToolSpec {
                name: "heartbeat".to_string(),
                description: "Report heartbeat decision after reviewing tasks.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "string",
                            "enum": ["skip", "run"]
                        },
                        "tasks": {
                            "type": "string"
                        }
                    },
                    "required": ["action"]
                }),
            }],
            max_tokens: None,
        };

        let response = provider.send(request).await?;
        let Some(call) = response.tool_calls.first() else {
            return Ok(HeartbeatDecision {
                action: "skip".to_string(),
                tasks: String::new(),
            });
        };

        let action = call
            .arguments
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("skip")
            .to_string();
        let tasks = call
            .arguments
            .get("tasks")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        Ok(HeartbeatDecision { action, tasks })
    }
}

async fn read_file_if_exists(path: &Path) -> ZeroBotResult<Option<String>> {
    match tokio::fs::read_to_string(path).await {
        Ok(content) => {
            let trimmed = content.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            Ok(Some(content))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(ZeroBotError::Io(err.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{Provider, ProviderEvent, ProviderResponse, TokenUsage};
    use async_trait::async_trait;
    use serde_json::json;
    use std::pin::Pin;
    use tokio_stream::Stream;

    struct MockProvider {
        response: ProviderResponse,
    }

    #[async_trait]
    impl Provider for MockProvider {
        fn id(&self) -> &str {
            "mock"
        }

        async fn send(&self, _request: ProviderRequest) -> ZeroBotResult<ProviderResponse> {
            Ok(self.response.clone())
        }

        fn stream(
            &self,
            _request: ProviderRequest,
        ) -> Pin<Box<dyn Stream<Item = ZeroBotResult<ProviderEvent>> + Send + '_>> {
            Box::pin(tokio_stream::iter(vec![Ok(ProviderEvent::Done)]))
        }
    }

    #[tokio::test]
    async fn decide_returns_skip_when_no_tool_call() {
        let factory: ProviderFactory = Arc::new(|| {
            Ok(Box::new(MockProvider {
                response: ProviderResponse {
                    content: "none".to_string(),
                    tool_calls: Vec::new(),
                    raw: json!({}),
                    usage: Some(TokenUsage::default()),
                },
            }) as Box<dyn Provider>)
        });
        let service = HeartbeatService::new(
            PathBuf::from("."),
            factory,
            "model".to_string(),
            "HEARTBEAT.md".to_string(),
            60,
            true,
            None,
            None,
        );
        let decision = service.decide("hi").await.unwrap();
        assert_eq!(decision.action, "skip");
    }
}
