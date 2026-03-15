use crate::config::Settings;
use crate::error::{ZeroBotError, ZeroBotResult};
use crate::events::AgentEvent;
use crate::provider::{Provider, ProviderEvent, ProviderMessage, ProviderMessageRole, ProviderRequest, ToolCall};
use crate::session::{Message, MessageRole, SessionStore, StoredToolCall};
use crate::tool::{ToolContext, ToolRegistry};
use chrono::Utc;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use uuid::Uuid;

pub struct Agent {
    provider: Box<dyn Provider>,
    model: String,
    settings: Settings,
    store: Arc<dyn SessionStore>,
    tools: ToolRegistry,
    cwd: std::path::PathBuf,
}

impl Agent {
    pub fn new(
        provider: Box<dyn Provider>,
        model: String,
        settings: Settings,
        store: Arc<dyn SessionStore>,
        tools: ToolRegistry,
        cwd: std::path::PathBuf,
    ) -> Self {
        Self {
            provider,
            model,
            settings,
            store,
            tools,
            cwd,
        }
    }

    pub async fn run_turn(
        &self,
        session_id: &str,
        input: &str,
        events: Option<mpsc::UnboundedSender<AgentEvent>>,
    ) -> ZeroBotResult<String> {
        self.emit(&events, AgentEvent::UserMessage { content: input.to_string() });

        self.store
            .append_message(Message {
                id: Uuid::new_v4().to_string(),
                session_id: session_id.to_string(),
                role: MessageRole::User,
                content: input.to_string(),
                tool_call_id: None,
                tool_calls: None,
                created_at: Utc::now().timestamp(),
            })
            .await?;

        let mut steps = 0usize;
        let mut last_response = String::new();

        loop {
            steps += 1;
            if steps > self.settings.agent.max_steps {
                return Err(ZeroBotError::Agent("超过最大步骤限制".to_string()));
            }

            let history = self.store.list_messages(session_id).await?;
            let history = if history.len() > self.settings.session.max_history {
                history[history.len() - self.settings.session.max_history..].to_vec()
            } else {
                history
            };
            let mut messages = Vec::new();
            for message in history {
                let role = match message.role {
                    MessageRole::System => ProviderMessageRole::System,
                    MessageRole::User => ProviderMessageRole::User,
                    MessageRole::Assistant => ProviderMessageRole::Assistant,
                    MessageRole::Tool => ProviderMessageRole::Tool,
                };
                messages.push(ProviderMessage {
                    role,
                    content: message.content,
                    tool_call_id: message.tool_call_id,
                    name: None,
                    tool_calls: message
                        .tool_calls
                        .as_ref()
                        .map(|calls| calls.iter().map(StoredToolCall::to_provider_call).collect()),
                });
            }

            let tool_specs = self.tools.specs(&self.settings.tools.enabled);
            let request = ProviderRequest {
                model: self.model.clone(),
                system: self.settings.agent.system_prompt.clone(),
                messages,
                tools: tool_specs,
                max_tokens: None,
            };

            let mut tool_calls = Vec::new();
            let mut had_delta = false;
            let mut content = String::new();
            let mut stream = self.provider.stream(request);
            while let Some(event) = stream.next().await {
                match event? {
                    ProviderEvent::TextDelta(text) => {
                        content.push_str(&text);
                        had_delta = true;
                        self.emit(
                            &events,
                            AgentEvent::AssistantDelta {
                                content: text,
                            },
                        );
                    }
                    ProviderEvent::ToolCall(call) => {
                        tool_calls.push(call);
                    }
                    ProviderEvent::Done => {}
                }
            }

            if tool_calls.is_empty() {
                if !content.is_empty() {
                    last_response = content.clone();
                    self.store
                        .append_message(Message {
                            id: Uuid::new_v4().to_string(),
                            session_id: session_id.to_string(),
                            role: MessageRole::Assistant,
                            content: content.clone(),
                            tool_call_id: None,
                            tool_calls: None,
                            created_at: Utc::now().timestamp(),
                        })
                        .await?;
                    if !had_delta {
                        self.emit(&events, AgentEvent::AssistantMessage { content });
                    }
                }
                self.emit(&events, AgentEvent::Done);
                break;
            }

            let stored_calls = tool_calls
                .iter()
                .cloned()
                .map(StoredToolCall::from_provider_call)
                .collect();
            self.store
                .append_message(Message {
                    id: Uuid::new_v4().to_string(),
                    session_id: session_id.to_string(),
                    role: MessageRole::Assistant,
                    content: content.clone(),
                    tool_call_id: None,
                    tool_calls: Some(stored_calls),
                    created_at: Utc::now().timestamp(),
                })
                .await?;
            if !content.is_empty() {
                last_response = content.clone();
                if !had_delta {
                    self.emit(&events, AgentEvent::AssistantMessage { content });
                }
            }

            for call in tool_calls {
                self.handle_tool_call(session_id, call, &events).await?;
            }
        }

        Ok(last_response)
    }

    async fn handle_tool_call(
        &self,
        session_id: &str,
        call: ToolCall,
        events: &Option<mpsc::UnboundedSender<AgentEvent>>,
    ) -> ZeroBotResult<()> {
        self.emit(
            events,
            AgentEvent::ToolCallStarted {
                name: call.name.clone(),
                input: call.arguments.to_string(),
            },
        );

        let tool_call_id = self
            .store
            .record_tool_call(&call.id, session_id, &call.name, &call.arguments.to_string())
            .await?;

        let ctx = ToolContext::new(
            self.cwd.clone(),
            self.settings.tools.allow_paths.iter().map(std::path::PathBuf::from).collect(),
        );

        match self.tools.run(&ctx, &call.name, call.arguments).await {
            Ok(output) => {
                self.store
                    .record_tool_output(&tool_call_id, &output.content)
                    .await?;
                self.store
                    .append_message(Message {
                        id: Uuid::new_v4().to_string(),
                        session_id: session_id.to_string(),
                        role: MessageRole::Tool,
                        content: output.content.clone(),
                        tool_call_id: Some(tool_call_id.clone()),
                        tool_calls: None,
                        created_at: Utc::now().timestamp(),
                    })
                    .await?;

                self.emit(
                    events,
                    AgentEvent::ToolCallFinished {
                        name: call.name,
                        output: output.content,
                        ok: true,
                    },
                );

                Ok(())
            }
            Err(err) => {
                let message = err.to_string();
                let _ = self
                    .store
                    .record_tool_output(&tool_call_id, &message)
                    .await;
                let _ = self
                    .store
                    .append_message(Message {
                        id: Uuid::new_v4().to_string(),
                        session_id: session_id.to_string(),
                        role: MessageRole::Tool,
                        content: message.clone(),
                        tool_call_id: Some(tool_call_id),
                        tool_calls: None,
                        created_at: Utc::now().timestamp(),
                    })
                    .await;

                self.emit(
                    events,
                    AgentEvent::ToolCallFinished {
                        name: call.name,
                        output: message,
                        ok: false,
                    },
                );

                Err(err)
            }
        }
    }

    fn emit(&self, events: &Option<mpsc::UnboundedSender<AgentEvent>>, event: AgentEvent) {
        if let Some(tx) = events {
            let _ = tx.send(event);
        }
    }
}
