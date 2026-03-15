use crate::config::Settings;
use crate::context::ContextManager;
use crate::error::{ZeroBotError, ZeroBotResult};
use crate::events::AgentEvent;
use crate::provider::{Provider, ProviderEvent, ProviderRequest, ToolCall};
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
            let skill_list = if self.settings.skills.enabled {
                let manager = crate::skills::SkillManager::new(&self.settings, &self.cwd);
                manager.discover().ok()
            } else {
                None
            };
            let context = ContextManager::new(&self.settings, self.cwd.clone()).build_with_skills(
                &self.model,
                &history,
                skill_list.as_deref(),
            );

            let mut enabled = self.settings.tools.enabled.clone();
            if self.settings.skills.enabled && !enabled.iter().any(|t| t == "skill") {
                enabled.push("skill".to_string());
            }
            if self.settings.mcp.enabled {
                for name in self.tools.names() {
                    if name.starts_with("mcp__") && !enabled.contains(&name) {
                        enabled.push(name);
                    }
                }
            }
            let tool_specs = self.tools.specs(&enabled);
            let request = ProviderRequest {
                model: self.model.clone(),
                system: context.system,
                messages: context.messages,
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

        match self
            .tools
            .run_with_settings(&ctx, &call.name, call.arguments, &self.settings.tools.output)
            .await
        {
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
