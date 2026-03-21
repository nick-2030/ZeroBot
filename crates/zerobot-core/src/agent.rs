use crate::bus::OutboundMessage;
use crate::config::{Settings, ToolApprovalMode};
use crate::context::ContextManager;
use crate::error::{ZeroBotError, ZeroBotResult};
use crate::events::AgentEvent;
use crate::hooks::{HookAction, HookEvent, HookManager};
use crate::interaction::{InteractionHandler, ToolApprovalDecision, ToolApprovalRequest};
use crate::plugin::{PluginHookWarning, PluginManager};
use crate::provider::{Provider, ProviderEvent, ProviderRequest, ToolCall};
use crate::session::{Message, MessageRole, SessionStore, StoredToolCall};
use crate::skills::{format_skill_stack, SkillInfo};
use crate::tool::{ToolContext, ToolRegistry, ToolRouteContext};
use chrono::Utc;
use serde_json::Value as JsonValue;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::RwLock;
use tokio_stream::StreamExt;
use tracing::warn;
use uuid::Uuid;

const COMPACTION_PROMPT: &str = r#"请根据以下对话内容生成一份“可继续执行任务”的摘要。

摘要需尽量保留：
- 目标与范围
- 关键指令/约束/偏好
- 已完成的工作与当前状态
- 重要的技术决策与原因
- 涉及的关键文件/路径/接口
- 下一步计划

输出要求：
- 使用清晰的条目或分段
- 不要回答对话中的问题
- 只输出摘要内容"#;

pub struct Agent {
    provider: Box<dyn Provider>,
    model: String,
    settings: Settings,
    store: Arc<dyn SessionStore>,
    tools: ToolRegistry,
    cwd: std::path::PathBuf,
    hooks: HookManager,
    interaction: Option<Arc<dyn InteractionHandler>>,
    plugins: Option<Arc<PluginManager>>,
    tool_approvals: Arc<RwLock<HashSet<String>>>,
    tool_route: Option<ToolRouteContext>,
    outbound: Option<mpsc::UnboundedSender<OutboundMessage>>,
}

impl Agent {
    pub fn new(
        provider: Box<dyn Provider>,
        model: String,
        settings: Settings,
        store: Arc<dyn SessionStore>,
        tools: ToolRegistry,
        cwd: std::path::PathBuf,
        hooks: HookManager,
        interaction: Option<Arc<dyn InteractionHandler>>,
        plugins: Option<Arc<PluginManager>>,
        tool_approvals: Arc<RwLock<HashSet<String>>>,
        tool_route: Option<ToolRouteContext>,
        outbound: Option<mpsc::UnboundedSender<OutboundMessage>>,
    ) -> Self {
        Self {
            provider,
            model,
            settings,
            store,
            tools,
            cwd,
            hooks,
            interaction,
            plugins,
            tool_approvals,
            tool_route,
            outbound,
        }
    }

    pub async fn run_turn(
        &self,
        session_id: &str,
        input: &str,
        events: Option<mpsc::UnboundedSender<AgentEvent>>,
    ) -> ZeroBotResult<String> {
        self.emit(
            &events,
            AgentEvent::UserMessage {
                content: input.to_string(),
            },
        );

        let mut input_text = input.to_string();
        let skill_hooks = self.load_skill_hooks(session_id).await?;
        let decision = self
            .hooks
            .apply_event(
                HookEvent::UserPromptSubmit,
                session_id,
                serde_json::json!({ "prompt": input_text }),
                &skill_hooks,
            )
            .await?;
        if matches!(decision.action, HookAction::Deny) {
            let message = decision
                .message
                .unwrap_or_else(|| "输入被 Hook 拒绝".to_string());
            self.emit(
                &events,
                AgentEvent::Error {
                    message: message.clone(),
                },
            );
            return Err(ZeroBotError::Agent(message));
        }
        if let Some(prompt) = decision.payload.get("prompt").and_then(|v| v.as_str()) {
            input_text = prompt.to_string();
        }
        if let Some(plugins) = &self.plugins {
            let (output, warnings) = plugins
                .run_hook_with_warnings(
                    "chat.message",
                    serde_json::json!({
                        "session_id": session_id,
                        "agent": "primary",
                        "model": self.model.clone(),
                    }),
                    serde_json::json!({
                        "prompt": input_text.clone(),
                    }),
                )
                .await?;
            self.emit_plugin_warnings(&events, warnings);
            if let Some(prompt) = output.get("prompt").and_then(|v| v.as_str()) {
                input_text = prompt.to_string();
            }
        }

        let _ = self
            .append_message_with_hooks(Message {
                id: Uuid::new_v4().to_string(),
                session_id: session_id.to_string(),
                role: MessageRole::User,
                content: input_text.clone(),
                summary: false,
                tool_call_id: None,
                tool_calls: None,
                created_at: Utc::now().timestamp(),
            })
            .await?;
        let _ = self
            .maybe_record_user_summary(session_id, &input_text)
            .await?;

        let instruction_sources = crate::instruction::system_sources(&self.settings, &self.cwd);
        let url_instructions =
            crate::instruction::fetch_url_instructions(&instruction_sources.urls).await;
        let url_instruction_text = url_instructions
            .into_iter()
            .map(|item| item.content)
            .collect::<Vec<_>>();

        let mut steps = 0usize;
        let mut last_response = String::new();
        let mut warned_missing_limit = false;
        let mut overflow_compaction_attempted = false;

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
                Some(&url_instruction_text),
            );
            let skill_stack = self.store.get_skill_stack(session_id).await?;
            let mut system = context.system.unwrap_or_default();
            if !skill_stack.is_empty() {
                if !system.trim().is_empty() {
                    system.push_str("\n\n");
                }
                system.push_str(&format_skill_stack(&skill_stack));
            }

            if self.settings.context.compaction.enabled && self.settings.context.compaction.auto {
                if let Some(limit) = context.context_limit {
                    let reserved = self.settings.context.compaction.reserved_tokens as usize;
                    if context.estimated_tokens >= limit.saturating_sub(reserved as u32) as usize {
                        self.compact_session(session_id, &history).await?;
                        overflow_compaction_attempted = false;
                        continue;
                    }
                } else if !warned_missing_limit {
                    warn!(
                        "context.max_tokens 或 context.model_limits 未配置，自动 compaction 已跳过"
                    );
                    warned_missing_limit = true;
                }
            }

            self.emit_context_usage_values(
                &events,
                context.estimated_tokens,
                context.context_limit,
            );

            if let Some(plugins) = &self.plugins {
                let (output, warnings) = plugins
                    .run_hook_with_warnings(
                        "experimental.chat.system.transform",
                        serde_json::json!({
                            "session_id": session_id,
                            "model": self.model.clone(),
                        }),
                        serde_json::json!({
                            "system": system,
                        }),
                    )
                    .await?;
                self.emit_plugin_warnings(&events, warnings);
                if let Some(updated) = output.get("system").and_then(|v| v.as_str()) {
                    system = updated.to_string();
                }
            }

            let mut provider_messages = context.messages;
            if let Some(plugins) = &self.plugins {
                let (output, warnings) = plugins
                    .run_hook_with_warnings(
                        "experimental.chat.messages.transform",
                        serde_json::json!({
                            "session_id": session_id,
                            "model": self.model.clone(),
                        }),
                        serde_json::json!({
                            "messages": provider_messages,
                        }),
                    )
                    .await?;
                self.emit_plugin_warnings(&events, warnings);
                if let Ok(updated) = serde_json::from_value::<Vec<crate::provider::ProviderMessage>>(
                    output
                        .get("messages")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!([])),
                ) {
                    provider_messages = updated;
                }
            }

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
            if self.settings.plugins.auto_enable_tools {
                if let Some(plugins) = &self.plugins {
                    for tool in plugins.tools() {
                        if !enabled.contains(&tool.name) {
                            enabled.push(tool.name);
                        }
                    }
                }
            }
            let mut tool_specs = self.tools.specs(&enabled);
            if let Some(plugins) = &self.plugins {
                let mut updated_specs = Vec::with_capacity(tool_specs.len());
                for spec in tool_specs {
                    let (output, warnings) = plugins
                        .run_hook_with_warnings(
                            "tool.definition",
                            serde_json::json!({
                                "tool_id": spec.name,
                            }),
                            serde_json::json!({
                                "description": spec.description,
                                "parameters": spec.parameters,
                            }),
                        )
                        .await?;
                    self.emit_plugin_warnings(&events, warnings);
                    let description = output
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(ToString::to_string)
                        .unwrap_or(spec.description);
                    let parameters = output.get("parameters").cloned().unwrap_or(spec.parameters);
                    updated_specs.push(crate::provider::ToolSpec {
                        name: spec.name,
                        description,
                        parameters,
                    });
                }
                tool_specs = updated_specs;
            }
            let mut request = ProviderRequest {
                model: self.model.clone(),
                system: if system.trim().is_empty() {
                    None
                } else {
                    Some(system)
                },
                messages: provider_messages,
                tools: tool_specs,
                max_tokens: None,
                temperature: None,
                top_p: None,
                top_k: None,
                headers: std::collections::HashMap::new(),
                provider_options: serde_json::json!({}),
            };

            if let Some(plugins) = &self.plugins {
                let provider_options = plugins
                    .provider_options(self.provider.id(), &self.model)
                    .await?;
                request.provider_options = provider_options;

                let (params_output, warnings) = plugins
                    .run_hook_with_warnings(
                        "chat.params",
                        serde_json::json!({
                            "session_id": session_id,
                            "provider_id": self.provider.id(),
                            "model": self.model.clone(),
                        }),
                        serde_json::json!({
                            "temperature": request.temperature,
                            "top_p": request.top_p,
                            "top_k": request.top_k,
                            "provider_options": request.provider_options.clone(),
                        }),
                    )
                    .await?;
                self.emit_plugin_warnings(&events, warnings);
                request.temperature = params_output
                    .get("temperature")
                    .and_then(|v| v.as_f64())
                    .map(|v| v as f32);
                request.top_p = params_output
                    .get("top_p")
                    .and_then(|v| v.as_f64())
                    .map(|v| v as f32);
                request.top_k = params_output
                    .get("top_k")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32);
                request.provider_options = params_output
                    .get("provider_options")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));

                let (headers_output, warnings) = plugins
                    .run_hook_with_warnings(
                        "chat.headers",
                        serde_json::json!({
                            "session_id": session_id,
                            "provider_id": self.provider.id(),
                            "model": self.model.clone(),
                        }),
                        serde_json::json!({
                            "headers": request.headers.clone(),
                        }),
                    )
                    .await?;
                self.emit_plugin_warnings(&events, warnings);
                if let Some(map) = headers_output.get("headers").and_then(|v| v.as_object()) {
                    let mut headers = std::collections::HashMap::new();
                    for (k, v) in map {
                        if let Some(value) = v.as_str() {
                            headers.insert(k.clone(), value.to_string());
                        }
                    }
                    request.headers = headers;
                }
            }

            let skill_hooks = self.load_skill_hooks(session_id).await?;
            let decision = self
                .hooks
                .apply_event(
                    HookEvent::PreProvider,
                    session_id,
                    serde_json::to_value(&request)
                        .map_err(|err| ZeroBotError::Agent(err.to_string()))?,
                    &skill_hooks,
                )
                .await?;
            if matches!(decision.action, HookAction::Deny) {
                let message = decision
                    .message
                    .unwrap_or_else(|| "提供商调用被 Hook 拒绝".to_string());
                self.emit(
                    &events,
                    AgentEvent::Error {
                        message: message.clone(),
                    },
                );
                return Err(ZeroBotError::Agent(message));
            }
            if decision.payload != JsonValue::Null {
                if let Ok(updated) =
                    serde_json::from_value::<ProviderRequest>(decision.payload.clone())
                {
                    request = updated;
                }
            }

            let mut tool_calls = Vec::new();
            let mut had_delta = false;
            let mut content = String::new();
            let mut stream = self.provider.stream(request);
            let mut stream_error: Option<ZeroBotError> = None;
            while let Some(event) = stream.next().await {
                match event {
                    Ok(event) => match event {
                        ProviderEvent::TextDelta(text) => {
                            content.push_str(&text);
                            had_delta = true;
                            self.emit(&events, AgentEvent::AssistantDelta { content: text });
                        }
                        ProviderEvent::ToolCall(call) => {
                            tool_calls.push(call);
                        }
                        ProviderEvent::Usage(usage) => {
                            self.emit(&events, AgentEvent::Usage { usage });
                        }
                        ProviderEvent::Done => {}
                    },
                    Err(err) => {
                        stream_error = Some(err);
                        break;
                    }
                }
            }
            if let Some(err) = stream_error {
                if Self::is_context_overflow(&err)
                    && self.settings.context.compaction.enabled
                    && !overflow_compaction_attempted
                {
                    self.compact_session(session_id, &history).await?;
                    overflow_compaction_attempted = true;
                    continue;
                }
                return Err(err);
            }
            overflow_compaction_attempted = false;

            let post_payload = serde_json::json!({
                "content": content.clone(),
                "tool_calls": tool_calls.clone(),
            });
            let skill_hooks = self.load_skill_hooks(session_id).await?;
            let post_decision = self
                .hooks
                .apply_event(
                    HookEvent::PostProvider,
                    session_id,
                    post_payload,
                    &skill_hooks,
                )
                .await?;
            if matches!(post_decision.action, HookAction::Deny) {
                let message = post_decision
                    .message
                    .unwrap_or_else(|| "提供商输出被 Hook 拒绝".to_string());
                self.emit(
                    &events,
                    AgentEvent::Error {
                        message: message.clone(),
                    },
                );
                return Err(ZeroBotError::Agent(message));
            }
            if let Some(updated_content) = post_decision
                .payload
                .get("content")
                .and_then(|v| v.as_str())
            {
                content = updated_content.to_string();
            }
            if let Some(updated_calls) = post_decision.payload.get("tool_calls") {
                if let Ok(calls) = serde_json::from_value::<Vec<ToolCall>>(updated_calls.clone()) {
                    tool_calls = calls;
                }
            }

            if tool_calls.is_empty() {
                if !content.is_empty() {
                    let msg = self
                        .append_message_with_hooks(Message {
                            id: Uuid::new_v4().to_string(),
                            session_id: session_id.to_string(),
                            role: MessageRole::Assistant,
                            content: content.clone(),
                            summary: false,
                            tool_call_id: None,
                            tool_calls: None,
                            created_at: Utc::now().timestamp(),
                        })
                        .await?;
                    last_response = msg.content.clone();
                    let _ = self
                        .maybe_record_session_brief(session_id, &msg.content)
                        .await;
                    if !had_delta {
                        self.emit(
                            &events,
                            AgentEvent::AssistantMessage {
                                content: msg.content,
                            },
                        );
                    }
                }
                self.emit_context_usage(
                    session_id,
                    &events,
                    skill_list.as_deref(),
                    Some(&url_instruction_text),
                )
                .await;
                let skill_stack = self.store.get_skill_stack(session_id).await?;
                if !skill_stack.is_empty() {
                    let notice = format_skill_stack(&skill_stack);
                    let _ = self
                        .append_message_with_hooks(Message {
                            id: Uuid::new_v4().to_string(),
                            session_id: session_id.to_string(),
                            role: MessageRole::System,
                            content: format!(
                                "Skill 仍在执行中，请继续完成并调用 skill end。\n\n{notice}"
                            ),
                            summary: false,
                            tool_call_id: None,
                            tool_calls: None,
                            created_at: Utc::now().timestamp(),
                        })
                        .await?;
                    continue;
                }
                self.emit(&events, AgentEvent::Done);
                break;
            }

            let stored_calls = tool_calls
                .iter()
                .cloned()
                .map(StoredToolCall::from_provider_call)
                .collect();
            let msg = self
                .append_message_with_hooks(Message {
                    id: Uuid::new_v4().to_string(),
                    session_id: session_id.to_string(),
                    role: MessageRole::Assistant,
                    content: content.clone(),
                    summary: false,
                    tool_call_id: None,
                    tool_calls: Some(stored_calls),
                    created_at: Utc::now().timestamp(),
                })
                .await?;
            let _ = self
                .maybe_record_session_brief(session_id, &msg.content)
                .await;
            if !content.is_empty() {
                last_response = msg.content.clone();
                if !had_delta {
                    self.emit(
                        &events,
                        AgentEvent::AssistantMessage {
                            content: msg.content,
                        },
                    );
                }
            }

            for call in tool_calls {
                self.handle_tool_call(session_id, call, &events).await?;
                self.emit_context_usage(
                    session_id,
                    &events,
                    skill_list.as_deref(),
                    Some(&url_instruction_text),
                )
                .await;
            }
        }

        let skill_hooks = self.load_skill_hooks(session_id).await?;
        let _ = self
            .hooks
            .apply_event(
                HookEvent::TaskCompleted,
                session_id,
                serde_json::json!({ "last_response": last_response }),
                &skill_hooks,
            )
            .await;
        let _ = self
            .hooks
            .apply_event(
                HookEvent::Stop,
                session_id,
                serde_json::json!({ "last_response": last_response }),
                &skill_hooks,
            )
            .await;

        Ok(last_response)
    }

    pub async fn compact_now(&self, session_id: &str) -> ZeroBotResult<()> {
        if !self.settings.context.compaction.enabled {
            return Err(ZeroBotError::Agent("上下文压缩未启用".to_string()));
        }
        let history = self.store.list_messages(session_id).await?;
        self.compact_session(session_id, &history).await
    }

    async fn compact_session(&self, session_id: &str, history: &[Message]) -> ZeroBotResult<()> {
        let mut messages = Self::build_compaction_messages(history);
        if messages.is_empty() {
            return Ok(());
        }
        let mut model = self
            .settings
            .context
            .compaction
            .summary_model
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| self.model.clone());
        let mut system_prompt = COMPACTION_PROMPT.to_string();

        if let Some(plugins) = &self.plugins {
            let output = plugins
                .run_hook(
                    "experimental.session.compacting",
                    serde_json::json!({
                        "session_id": session_id,
                        "phase": "before",
                    }),
                    serde_json::json!({
                        "model": model.clone(),
                        "system": system_prompt.clone(),
                        "messages": messages.clone(),
                    }),
                )
                .await?;
            if let Some(updated) = output.get("model").and_then(|v| v.as_str()) {
                model = updated.to_string();
            }
            if let Some(updated) = output.get("system").and_then(|v| v.as_str()) {
                system_prompt = updated.to_string();
            }
            if let Some(updated_messages) = output.get("messages") {
                if let Ok(parsed) = serde_json::from_value::<Vec<crate::provider::ProviderMessage>>(
                    updated_messages.clone(),
                ) {
                    messages = parsed;
                }
            }
        }
        if messages.is_empty() {
            return Ok(());
        }

        let request = ProviderRequest {
            model,
            system: Some(system_prompt),
            messages,
            tools: Vec::new(),
            max_tokens: None,
            temperature: None,
            top_p: None,
            top_k: None,
            headers: std::collections::HashMap::new(),
            provider_options: serde_json::json!({}),
        };
        let mut content = String::new();
        let mut stream = self.provider.stream(request);
        while let Some(event) = stream.next().await {
            match event? {
                ProviderEvent::TextDelta(text) => content.push_str(&text),
                ProviderEvent::ToolCall(_) => {}
                ProviderEvent::Usage(_) => {}
                ProviderEvent::Done => {}
            }
        }
        let mut summary = content.trim().to_string();
        if let Some(plugins) = &self.plugins {
            let output = plugins
                .run_hook(
                    "experimental.session.compacting",
                    serde_json::json!({
                        "session_id": session_id,
                        "phase": "after",
                    }),
                    serde_json::json!({
                        "summary": summary.clone(),
                    }),
                )
                .await?;
            if let Some(updated) = output.get("summary").and_then(|v| v.as_str()) {
                summary = updated.to_string();
            }
        }
        if summary.is_empty() {
            return Err(ZeroBotError::Agent("上下文压缩失败：摘要为空".to_string()));
        }
        let _ = self
            .append_message_with_hooks(Message {
                id: Uuid::new_v4().to_string(),
                session_id: session_id.to_string(),
                role: MessageRole::Assistant,
                content: summary,
                summary: true,
                tool_call_id: None,
                tool_calls: None,
                created_at: Utc::now().timestamp(),
            })
            .await?;
        Ok(())
    }

    fn build_compaction_messages(history: &[Message]) -> Vec<crate::provider::ProviderMessage> {
        let start = history
            .iter()
            .rposition(|msg| msg.summary && matches!(msg.role, MessageRole::Assistant))
            .unwrap_or(0);
        history[start..]
            .iter()
            .map(|message| crate::provider::ProviderMessage {
                role: match message.role {
                    MessageRole::System => crate::provider::ProviderMessageRole::System,
                    MessageRole::User => crate::provider::ProviderMessageRole::User,
                    MessageRole::Assistant => crate::provider::ProviderMessageRole::Assistant,
                    MessageRole::Tool => crate::provider::ProviderMessageRole::Tool,
                },
                content: message.content.clone(),
                tool_call_id: message.tool_call_id.clone(),
                name: None,
                tool_calls: message
                    .tool_calls
                    .as_ref()
                    .map(|calls| calls.iter().map(StoredToolCall::to_provider_call).collect()),
            })
            .collect()
    }

    fn is_context_overflow(err: &ZeroBotError) -> bool {
        let text = err.to_string().to_lowercase();
        text.contains("context length")
            || text.contains("maximum context")
            || text.contains("context window")
            || text.contains("exceeds the context")
            || text.contains("context limit")
    }

    async fn append_message_with_hooks(&self, mut message: Message) -> ZeroBotResult<Message> {
        let skill_hooks = self.load_skill_hooks(&message.session_id).await?;
        let payload = serde_json::json!({
            "role": message.role.to_string(),
            "content": message.content.clone(),
            "summary": message.summary,
            "tool_call_id": message.tool_call_id.clone(),
        });
        let decision = self
            .hooks
            .apply_event(
                HookEvent::MessageAppend,
                &message.session_id,
                payload,
                &skill_hooks,
            )
            .await?;
        if matches!(decision.action, HookAction::Deny) {
            let message = decision
                .message
                .unwrap_or_else(|| "消息被 Hook 拒绝".to_string());
            return Err(ZeroBotError::Agent(message));
        }
        if let Some(content) = decision.payload.get("content").and_then(|v| v.as_str()) {
            message.content = content.to_string();
        }
        self.store.append_message(message.clone()).await?;
        Ok(message)
    }

    async fn maybe_record_session_brief(
        &self,
        session_id: &str,
        assistant_content: &str,
    ) -> ZeroBotResult<()> {
        let session = match self.store.get_session(session_id).await? {
            Some(session) => session,
            None => return Ok(()),
        };

        let mut first_ai = None;
        if session.first_ai_message.is_none() && !assistant_content.trim().is_empty() {
            first_ai = Some(assistant_content.trim().to_string());
        }

        let mut summary = None;
        if session.summary.is_none() {
            if let Some(user) = self
                .store
                .list_messages(session_id)
                .await?
                .into_iter()
                .find(|msg| matches!(msg.role, MessageRole::User) && !msg.content.trim().is_empty())
            {
                summary = Some(self.summarize_first_user(&user.content));
            }
        }

        if first_ai.is_some() || summary.is_some() {
            self.store
                .update_session_brief(session_id, first_ai.as_deref(), summary.as_deref())
                .await?;
        }

        Ok(())
    }

    async fn maybe_record_user_summary(
        &self,
        session_id: &str,
        user_content: &str,
    ) -> ZeroBotResult<()> {
        let session = match self.store.get_session(session_id).await? {
            Some(session) => session,
            None => return Ok(()),
        };
        if session.summary.is_some() {
            return Ok(());
        }
        let summary = self.summarize_first_user(user_content);
        self.store
            .update_session_brief(session_id, None, Some(&summary))
            .await?;
        Ok(())
    }

    fn summarize_first_user(&self, content: &str) -> String {
        let mut text = content.trim().replace('\n', " ").replace('\r', " ");
        if text.chars().count() > 20 {
            text = text.chars().take(20).collect();
        }
        text
    }

    async fn handle_tool_call(
        &self,
        session_id: &str,
        call: ToolCall,
        events: &Option<mpsc::UnboundedSender<AgentEvent>>,
    ) -> ZeroBotResult<()> {
        let tool_name = call.name.clone();
        let tool_call_external_id = call.id.clone();
        let mut args = call.arguments.clone();

        let pre_payload = serde_json::json!({
            "tool_name": tool_name.clone(),
            "tool_input": args.clone(),
        });
        let skill_hooks = self.load_skill_hooks(session_id).await?;
        let decision = self
            .hooks
            .apply_event(HookEvent::PreToolUse, session_id, pre_payload, &skill_hooks)
            .await?;
        if matches!(decision.action, HookAction::Deny) {
            let message = decision
                .message
                .unwrap_or_else(|| "工具调用被 Hook 拒绝".to_string());
            self.emit(
                events,
                AgentEvent::ToolCallStarted {
                    name: tool_name.clone(),
                    input: call.arguments.to_string(),
                },
            );
            let tool_call_id = self
                .store
                .record_tool_call(
                    &tool_call_external_id,
                    session_id,
                    &tool_name,
                    &call.arguments.to_string(),
                )
                .await?;
            let _ = self.store.record_tool_output(&tool_call_id, &message).await;
            let _ = self
                .append_message_with_hooks(Message {
                    id: Uuid::new_v4().to_string(),
                    session_id: session_id.to_string(),
                    role: MessageRole::Tool,
                    content: message.clone(),
                    summary: false,
                    tool_call_id: Some(tool_call_id),
                    tool_calls: None,
                    created_at: Utc::now().timestamp(),
                })
                .await;
            let skill_hooks = self.load_skill_hooks(session_id).await?;
            let _ = self
                .hooks
                .apply_event(
                    HookEvent::PostToolUseFailure,
                    session_id,
                    serde_json::json!({
                        "tool_name": tool_name.clone(),
                        "tool_input": call.arguments,
                        "tool_output": message,
                        "ok": false,
                    }),
                    &skill_hooks,
                )
                .await;
            self.emit(
                events,
                AgentEvent::ToolCallFinished {
                    name: tool_name,
                    output: message,
                    ok: false,
                },
            );
            return Ok(());
        }

        if let Some(updated_args) = decision.payload.get("tool_input") {
            args = updated_args.clone();
        }

        if let Some(plugins) = &self.plugins {
            let (output, warnings) = plugins
                .run_hook_with_warnings(
                    "tool.execute.before",
                    serde_json::json!({
                        "session_id": session_id,
                        "tool_name": tool_name.clone(),
                        "tool_call_id": tool_call_external_id.clone(),
                    }),
                    serde_json::json!({
                        "tool_input": args.clone(),
                    }),
                )
                .await?;
            self.emit_plugin_warnings(events, warnings);
            if let Some(updated_args) = output.get("tool_input") {
                args = updated_args.clone();
            }
        }

        let approval_key = approval_key(&tool_name, &args);
        let mut approval_mode = self.settings.tools.approval.mode_for(&tool_name);
        if is_bash_tool(&tool_name) {
            if let Some(command) = bash_command_from_args(&args) {
                if let Some(mode) = self.settings.tools.approval.bash_mode_for(command) {
                    approval_mode = mode;
                }
            }
        }
        if approval_mode != ToolApprovalMode::Deny
            && self.tool_approvals.read().await.contains(&approval_key)
        {
            approval_mode = ToolApprovalMode::Auto;
        }

        let mut approved = true;
        let mut deny_message: Option<String> = None;
        if approval_mode == ToolApprovalMode::Prompt {
            if let Some(handler) = self.interaction.clone() {
                let reason = if is_bash_tool(&tool_name) {
                    bash_command_from_args(&args).map(|cmd| format!("bash 命令: {cmd}"))
                } else {
                    None
                };
                let response = handler
                    .request_tool_approval(ToolApprovalRequest {
                        tool_name: tool_name.clone(),
                        arguments: args.clone(),
                        reason,
                    })
                    .await?;
                match response.decision {
                    ToolApprovalDecision::AllowOnce => {}
                    ToolApprovalDecision::AllowSession => {
                        self.tool_approvals
                            .write()
                            .await
                            .insert(approval_key.clone());
                    }
                    ToolApprovalDecision::AllowWorkspace => {
                        self.tool_approvals
                            .write()
                            .await
                            .insert(approval_key.clone());
                        self.store.insert_tool_approval(&approval_key).await?;
                    }
                    ToolApprovalDecision::Deny => {
                        approved = false;
                        deny_message = Some("工具调用被拒绝".to_string());
                    }
                }
            } else {
                approved = false;
                deny_message = Some("需要用户授权，但当前无交互处理器".to_string());
            }
        } else if approval_mode == ToolApprovalMode::Deny {
            approved = false;
            deny_message = Some("工具调用被策略拒绝".to_string());
        }

        self.emit(
            events,
            AgentEvent::ToolCallStarted {
                name: tool_name.clone(),
                input: args.to_string(),
            },
        );

        let tool_call_id = self
            .store
            .record_tool_call(
                &tool_call_external_id,
                session_id,
                &tool_name,
                &args.to_string(),
            )
            .await?;

        if !approved {
            let message = deny_message.unwrap_or_else(|| "工具调用被拒绝".to_string());
            let _ = self.store.record_tool_output(&tool_call_id, &message).await;
            let _ = self
                .append_message_with_hooks(Message {
                    id: Uuid::new_v4().to_string(),
                    session_id: session_id.to_string(),
                    role: MessageRole::Tool,
                    content: message.clone(),
                    summary: false,
                    tool_call_id: Some(tool_call_id),
                    tool_calls: None,
                    created_at: Utc::now().timestamp(),
                })
                .await;
            let skill_hooks = self.load_skill_hooks(session_id).await?;
            let _ = self
                .hooks
                .apply_event(
                    HookEvent::PostToolUseFailure,
                    session_id,
                    serde_json::json!({
                        "tool_name": tool_name.clone(),
                        "tool_input": args.clone(),
                        "tool_output": message,
                        "ok": false,
                    }),
                    &skill_hooks,
                )
                .await;
            self.emit(
                events,
                AgentEvent::ToolCallFinished {
                    name: tool_name,
                    output: message,
                    ok: false,
                },
            );
            return Ok(());
        }

        let ctx = ToolContext::new(
            self.cwd.clone(),
            self.settings
                .tools
                .allow_paths
                .iter()
                .map(std::path::PathBuf::from)
                .collect(),
            session_id,
            Some(self.store.clone()),
            self.interaction.clone(),
        )
        .with_plugins(self.plugins.clone())
        .with_route(self.tool_route.clone())
        .with_outbound(self.outbound.clone());

        match self
            .tools
            .run_with_settings(&ctx, &tool_name, args.clone(), &self.settings.tools.output)
            .await
        {
            Ok(output) => {
                let mut output_content = output.content.clone();
                let mut ok = true;
                let post_payload = serde_json::json!({
                    "tool_name": tool_name.clone(),
                    "tool_input": args.clone(),
                    "tool_output": output_content.clone(),
                    "ok": ok,
                });
                let skill_hooks = self.load_skill_hooks(session_id).await?;
                let post_decision = self
                    .hooks
                    .apply_event(
                        HookEvent::PostToolUse,
                        session_id,
                        post_payload,
                        &skill_hooks,
                    )
                    .await?;
                if matches!(post_decision.action, HookAction::Deny) {
                    ok = false;
                    output_content = post_decision
                        .message
                        .unwrap_or_else(|| "工具输出被 Hook 拒绝".to_string());
                } else if let Some(updated) = post_decision
                    .payload
                    .get("tool_output")
                    .and_then(|v| v.as_str())
                {
                    output_content = updated.to_string();
                }
                if let Some(plugins) = &self.plugins {
                    let (output, warnings) = plugins
                        .run_hook_with_warnings(
                            "tool.execute.after",
                            serde_json::json!({
                                "session_id": session_id,
                                "tool_name": tool_name.clone(),
                                "tool_call_id": tool_call_external_id.clone(),
                            }),
                            serde_json::json!({
                                "tool_input": args.clone(),
                                "tool_output": output_content.clone(),
                                "ok": ok,
                            }),
                        )
                        .await?;
                    self.emit_plugin_warnings(events, warnings);
                    if let Some(updated) = output.get("tool_output").and_then(|v| v.as_str()) {
                        output_content = updated.to_string();
                    }
                    ok = output.get("ok").and_then(|v| v.as_bool()).unwrap_or(ok);
                }

                self.store
                    .record_tool_output(&tool_call_id, &output_content)
                    .await?;
                let _ = self
                    .append_message_with_hooks(Message {
                        id: Uuid::new_v4().to_string(),
                        session_id: session_id.to_string(),
                        role: MessageRole::Tool,
                        content: output_content.clone(),
                        summary: false,
                        tool_call_id: Some(tool_call_id.clone()),
                        tool_calls: None,
                        created_at: Utc::now().timestamp(),
                    })
                    .await?;

                self.emit(
                    events,
                    AgentEvent::ToolCallFinished {
                        name: tool_name,
                        output: output_content,
                        ok,
                    },
                );

                Ok(())
            }
            Err(err) => {
                let message = err.to_string();
                let mut output_content = message.clone();
                let skill_hooks = self.load_skill_hooks(session_id).await?;
                let post_decision = self
                    .hooks
                    .apply_event(
                        HookEvent::PostToolUseFailure,
                        session_id,
                        serde_json::json!({
                            "tool_name": tool_name.clone(),
                            "tool_input": args.clone(),
                            "tool_output": output_content.clone(),
                            "ok": false,
                        }),
                        &skill_hooks,
                    )
                    .await?;
                if matches!(post_decision.action, HookAction::Deny) {
                    output_content = post_decision
                        .message
                        .unwrap_or_else(|| "工具输出被 Hook 拒绝".to_string());
                } else if let Some(updated) = post_decision
                    .payload
                    .get("tool_output")
                    .and_then(|v| v.as_str())
                {
                    output_content = updated.to_string();
                }
                if let Some(plugins) = &self.plugins {
                    let (output, warnings) = plugins
                        .run_hook_with_warnings(
                            "tool.execute.after",
                            serde_json::json!({
                                "session_id": session_id,
                                "tool_name": tool_name.clone(),
                                "tool_call_id": tool_call_external_id,
                            }),
                            serde_json::json!({
                                "tool_input": args.clone(),
                                "tool_output": output_content.clone(),
                                "ok": false,
                            }),
                        )
                        .await?;
                    self.emit_plugin_warnings(events, warnings);
                    if let Some(updated) = output.get("tool_output").and_then(|v| v.as_str()) {
                        output_content = updated.to_string();
                    }
                }
                let _ = self
                    .store
                    .record_tool_output(&tool_call_id, &output_content)
                    .await;
                let _ = self
                    .append_message_with_hooks(Message {
                        id: Uuid::new_v4().to_string(),
                        session_id: session_id.to_string(),
                        role: MessageRole::Tool,
                        content: output_content.clone(),
                        summary: false,
                        tool_call_id: Some(tool_call_id),
                        tool_calls: None,
                        created_at: Utc::now().timestamp(),
                    })
                    .await;

                self.emit(
                    events,
                    AgentEvent::ToolCallFinished {
                        name: tool_name,
                        output: output_content,
                        ok: false,
                    },
                );

                Ok(())
            }
        }
    }

    fn emit(&self, events: &Option<mpsc::UnboundedSender<AgentEvent>>, event: AgentEvent) {
        if let Some(tx) = events {
            let _ = tx.send(event);
        }
    }

    fn emit_plugin_warnings(
        &self,
        events: &Option<mpsc::UnboundedSender<AgentEvent>>,
        warnings: Vec<PluginHookWarning>,
    ) {
        for warning in warnings {
            self.emit(
                events,
                AgentEvent::PluginWarning {
                    plugin: warning.plugin,
                    hook: warning.hook,
                    message: warning.message,
                    degraded: warning.degraded,
                },
            );
        }
    }

    fn emit_context_usage_values(
        &self,
        events: &Option<mpsc::UnboundedSender<AgentEvent>>,
        used: usize,
        limit: Option<u32>,
    ) {
        self.emit(events, AgentEvent::ContextUsage { used, limit });
    }

    async fn emit_context_usage(
        &self,
        session_id: &str,
        events: &Option<mpsc::UnboundedSender<AgentEvent>>,
        skill_list: Option<&[SkillInfo]>,
        extra_instructions: Option<&[String]>,
    ) {
        let history = match self.store.list_messages(session_id).await {
            Ok(history) => history,
            Err(err) => {
                warn!("failed to refresh context usage: {err}");
                return;
            }
        };
        let context = ContextManager::new(&self.settings, self.cwd.clone()).build_with_skills(
            &self.model,
            &history,
            skill_list,
            extra_instructions,
        );
        self.emit_context_usage_values(events, context.estimated_tokens, context.context_limit);
    }

    async fn load_skill_hooks(
        &self,
        session_id: &str,
    ) -> ZeroBotResult<Vec<crate::hooks::HookDefinition>> {
        let stack = self.store.get_skill_stack(session_id).await?;
        let mut hooks = Vec::new();
        for entry in stack {
            hooks.extend(entry.hooks);
        }
        Ok(hooks)
    }
}

fn is_bash_tool(name: &str) -> bool {
    matches!(name, "bash" | "shell")
}

fn bash_command_from_args(args: &JsonValue) -> Option<&str> {
    args.get("command").and_then(|v| v.as_str())
}

fn approval_key(tool_name: &str, args: &JsonValue) -> String {
    if is_bash_tool(tool_name) {
        if let Some(command) = bash_command_from_args(args) {
            return format!("{tool_name}:{command}");
        }
    }
    tool_name.to_string()
}
