use crate::bus::OutboundMessage;
use crate::config::{PermissionMode, Settings, ToolApprovalMode};
use crate::context::ContextManager;
use crate::error::{ZeroBotError, ZeroBotResult};
use crate::events::AgentEvent;
use crate::hooks::{HookAction, HookEvent, HookManager};
use crate::interaction::{InteractionHandler, ToolApprovalDecision, ToolApprovalRequest};
use crate::plugin::{PluginHookWarning, PluginManager};
use crate::provider::{Provider, ProviderEvent, ProviderRequest, ToolCall};
use crate::session::{Message, MessageRole, SessionStore, StoredToolCall};
use crate::skills::{SkillInfo, SkillManager};
use crate::tool::{ToolContext, ToolRegistry, ToolRouteContext};
use chrono::Utc;
use serde_json::Value as JsonValue;
use std::collections::HashSet;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::RwLock;
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;
use tracing::warn;
use uuid::Uuid;

use crate::notification::{Notification, NotificationSender, NotificationStatus};
use crate::task::{TaskId, TaskManager, TaskStatus, TaskUsage};

/// Tracks consecutive and total permission denials for fallback logic.
/// Uses AtomicU32 for thread-safe interior mutability.
#[derive(Clone)]
struct DenialCounts {
    consecutive: Arc<AtomicU32>,
    total: Arc<AtomicU32>,
}

impl DenialCounts {
    fn new() -> Self {
        Self {
            consecutive: Arc::new(AtomicU32::new(0)),
            total: Arc::new(AtomicU32::new(0)),
        }
    }

    fn record_approval(&self) {
        self.consecutive.store(0, Ordering::Relaxed);
    }

    fn record_denial(&self) {
        self.consecutive.fetch_add(1, Ordering::Relaxed);
        self.total.fetch_add(1, Ordering::Relaxed);
    }

    /// Check if denial thresholds are exceeded.
    fn exceeded(&self, max_consecutive: u32, max_total: u32) -> bool {
        self.consecutive.load(Ordering::Relaxed) >= max_consecutive
            || self.total.load(Ordering::Relaxed) >= max_total
    }
}

const COMPACTION_PROMPT: &str = r#"请根据以下对话内容生成一份结构化摘要，确保后续对话可以无缝继续执行任务。

<summary>
## 目标与范围
[任务的最终目标和约束条件]

## 已完成的工作
[按时间顺序列出关键操作和结果]

## 当前状态
[正在做什么、进展到哪一步]

## 关键决策
[做出的技术选择及原因]

## 重要文件与路径
[涉及的关键文件路径、接口、配置]

## 待办事项
[明确的下一步计划]

## 用户偏好
[用户表达的风格/习惯/约束]

## 关键上下文
[其他需要保留的信息]
</summary>

要求：
- 不要回答对话中的问题，只输出摘要
- 保留具体的技术细节（函数名、文件路径、错误信息）
- 如果有未完成的任务，明确标注进度和阻塞点"#;

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
    denial_counts: DenialCounts,
    // 多智能体支持
    task_id: Option<TaskId>,
    parent_task_id: Option<TaskId>,
    abort_token: CancellationToken,
    notification_tx: Option<NotificationSender>,
    agent_type: String,
    iteration_budget: Option<u32>,
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
        task_id: Option<TaskId>,
        parent_task_id: Option<TaskId>,
        agent_type: Option<String>,
        iteration_budget: Option<u32>,
        notification_tx: Option<NotificationSender>,
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
            denial_counts: DenialCounts::new(),
            task_id,
            parent_task_id,
            abort_token: CancellationToken::new(),
            notification_tx,
            agent_type: agent_type.unwrap_or_else(|| "default".to_string()),
            iteration_budget,
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

        // Cost tracking accumulators
        let mut total_input_tokens: u64 = 0;
        let mut total_output_tokens: u64 = 0;
        let mut total_cache_creation_tokens: u64 = 0;
        let mut total_cache_read_tokens: u64 = 0;
        let mut turn_count: u32 = 0;

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
            // 检查中断令牌
            if self.abort_token.is_cancelled() {
                let _ = self.emit(&events, AgentEvent::Stop);
                return Ok("任务被中断".to_string());
            }

            steps += 1;
            if steps > self.settings.agent.max_steps {
                return Err(ZeroBotError::Agent("超过最大步骤限制".to_string()));
            }

            // 迭代预算检查
            if let Some(budget) = self.iteration_budget {
                if steps > budget as usize {
                    let _ = self.emit(&events, AgentEvent::Stop);
                    return Ok(format!("迭代预算耗尽 ({} 步)", budget));
                }
            }

            let history = self.store.list_messages(session_id).await?;
            let skill_list = if self.settings.skills.enabled {
                let manager = crate::skills::SkillManager::new(&self.settings, &self.cwd);
                manager.discover().ok()
            } else {
                None
            };
            let memory_block = self.tools.memory_manager().and_then(|mgr| {
                // Get frozen snapshot block from MemoryManager (blocking since we're in async context)
                let mgr = mgr.try_lock().ok()?;
                mgr.build_system_prompt_block()
            });
            let context = ContextManager::new(&self.settings, self.cwd.clone())
                .with_tools(self.tools.clone())
                .with_memory_block(memory_block)
                .build_with_skills(
                    &self.model,
                    &history,
                    skill_list.as_deref(),
                    Some(&url_instruction_text),
                );
            let mut system = context.system.unwrap_or_default();

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
                            total_input_tokens += usage.input_tokens.unwrap_or(0) as u64;
                            total_output_tokens += usage.output_tokens.unwrap_or(0) as u64;
                            total_cache_creation_tokens += usage.cache_creation_input_tokens.unwrap_or(0) as u64;
                            total_cache_read_tokens += usage.cache_read_input_tokens.unwrap_or(0) as u64;
                            turn_count += 1;
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
                self.emit(&events, AgentEvent::SessionCost {
                    input_tokens: total_input_tokens,
                    output_tokens: total_output_tokens,
                    cache_creation_tokens: total_cache_creation_tokens,
                    cache_read_tokens: total_cache_read_tokens,
                    turn_count,
                });
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

            // Partition tool calls into parallel (read-only) and serial (write) batches
            let executor = ToolExecutor::from_agent(self);
            let batches = partition_tool_calls(tool_calls, &executor.tools);
            for batch in batches {
                let (call_ids, is_parallel) = match &batch {
                    ToolBatch::Parallel(calls) => {
                        (calls.iter().map(|c| c.id.clone()).collect::<Vec<_>>(), true)
                    }
                    ToolBatch::Serial(calls) => {
                        (calls.iter().map(|c| c.id.clone()).collect::<Vec<_>>(), false)
                    }
                };
                self.emit(
                    &events,
                    AgentEvent::ToolBatchStarted {
                        tool_call_ids: call_ids,
                        parallel: is_parallel,
                    },
                );
                match batch {
                    ToolBatch::Parallel(calls) => {
                        let mut join_set = tokio::task::JoinSet::new();
                        for call in calls {
                            let exec = executor.clone();
                            let sid = session_id.to_string();
                            let evts = events.clone();
                            join_set.spawn(async move {
                                exec.handle_tool_call(&sid, call, &evts).await
                            });
                        }
                        while let Some(result) = join_set.join_next().await {
                            match result {
                                Ok(inner) => inner?,
                                Err(join_err) => {
                                    return Err(ZeroBotError::Agent(format!(
                                        "并行工具执行失败: {join_err}"
                                    )));
                                }
                            }
                        }
                    }
                    ToolBatch::Serial(calls) => {
                        for call in calls {
                            executor
                                .handle_tool_call(session_id, call, &events)
                                .await?;
                        }
                    }
                }
                self.emit_context_usage(
                    session_id,
                    &events,
                    skill_list.as_deref(),
                    Some(&url_instruction_text),
                )
                .await;
            }

            // 发送进度通知
            if let Some(ref tx) = self.notification_tx {
                let notification = Notification {
                    task_id: self.task_id.clone().unwrap_or_else(|| TaskId::new("a_")),
                    agent_type: self.agent_type.clone(),
                    description: format!("已完成 {} 步", steps),
                    status: NotificationStatus::Progress {
                        summary: format!("步骤 {} 完成", steps),
                    },
                    result: None,
                    usage: None,
                    timestamp: std::time::Instant::now(),
                };
                let _ = tx.send(notification);
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
        _session_id: &str,
    ) -> ZeroBotResult<Vec<crate::hooks::HookDefinition>> {
        Ok(Vec::new())
    }
}

/// Cloneable executor for tool calls, extracted from Agent to support parallel execution.
#[derive(Clone)]
struct ToolExecutor {
    tools: ToolRegistry,
    settings: Settings,
    store: Arc<dyn SessionStore>,
    hooks: HookManager,
    interaction: Option<Arc<dyn InteractionHandler>>,
    plugins: Option<Arc<PluginManager>>,
    tool_approvals: Arc<RwLock<HashSet<String>>>,
    cwd: std::path::PathBuf,
    tool_route: Option<ToolRouteContext>,
    outbound: Option<mpsc::UnboundedSender<OutboundMessage>>,
    denial_counts: DenialCounts,
}

impl ToolExecutor {
    fn from_agent(agent: &Agent) -> Self {
        Self {
            tools: agent.tools.clone(),
            settings: agent.settings.clone(),
            store: agent.store.clone(),
            hooks: agent.hooks.clone(),
            interaction: agent.interaction.clone(),
            plugins: agent.plugins.clone(),
            tool_approvals: agent.tool_approvals.clone(),
            cwd: agent.cwd.clone(),
            tool_route: agent.tool_route.clone(),
            outbound: agent.outbound.clone(),
            denial_counts: DenialCounts::new(),
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

    fn resolve_tool_alias(
        &self,
        tool_name: String,
        args: &mut JsonValue,
    ) -> ZeroBotResult<String> {
        if self.tools.get(&tool_name).is_some() {
            return Ok(tool_name);
        }
        if !self.settings.skills.enabled
            || tool_name == "skill"
            || self.tools.get("skill").is_none()
        {
            return Ok(tool_name);
        }

        let manager = SkillManager::new(&self.settings, &self.cwd);
        let skills = match manager.discover() {
            Ok(skills) => skills,
            Err(_) => return Ok(tool_name),
        };
        let skill_names = skills.into_iter().map(|s| s.name).collect::<HashSet<_>>();
        if let Some(mapped) = rewrite_skill_alias_call(&tool_name, args, &skill_names) {
            return Ok(mapped);
        }
        Ok(tool_name)
    }

    async fn load_skill_hooks(
        &self,
        _session_id: &str,
    ) -> ZeroBotResult<Vec<crate::hooks::HookDefinition>> {
        Ok(Vec::new())
    }

    async fn handle_tool_call(
        &self,
        session_id: &str,
        call: ToolCall,
        events: &Option<mpsc::UnboundedSender<AgentEvent>>,
    ) -> ZeroBotResult<()> {
        let mut tool_name = call.name.clone();
        let tool_call_external_id = call.id.clone();
        let mut args = call.arguments.clone();
        tool_name = self.resolve_tool_alias(tool_name, &mut args)?;

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
                    tool_call_id: tool_call_external_id.clone(),
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
                    tool_call_id: Some(tool_call_id.clone()),
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
                    tool_call_id: tool_call_id.clone(),
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
        let args_str = args.to_string();

        // Step 1: Check PermissionMode override
        let permission_mode = self.settings.tools.approval.effective_permission_mode();
        let mut approval_mode = match permission_mode {
            PermissionMode::BypassPermissions => {
                // Auto-approve everything
                ToolApprovalMode::Auto
            }
            PermissionMode::Plan => {
                // In Plan mode, deny all write/execute tools
                if is_write_or_execute_tool(&tool_name) {
                    ToolApprovalMode::Deny
                } else {
                    ToolApprovalMode::Auto
                }
            }
            PermissionMode::AcceptEdits => {
                // Auto-approve file edits, prompt for bash/execute
                if is_bash_tool(&tool_name) {
                    ToolApprovalMode::Prompt
                } else {
                    ToolApprovalMode::Auto
                }
            }
            PermissionMode::Default => {
                // Step 4: Check per-tool mode (existing logic)
                self.settings.tools.approval.mode_for(&tool_name)
            }
        };

        // Step 2: Check content rules (tool name + input pattern)
        if approval_mode != ToolApprovalMode::Deny {
            if let Some(content_mode) = self.settings.tools.approval.content_rule_for(&tool_name, &args_str) {
                approval_mode = content_mode;
            }
        }

        // Step 3: Check bash/skill command rules
        if approval_mode != ToolApprovalMode::Deny {
            if is_bash_tool(&tool_name) {
                if let Some(command) = bash_command_from_args(&args) {
                    if let Some(mode) = self.settings.tools.approval.bash_mode_for(command) {
                        approval_mode = mode;
                    }
                }
            } else if tool_name == "skill" {
                if let Some(skill_name) = skill_name_from_args(&args) {
                    if let Some(mode) = self.settings.tools.approval.skill_mode_for(skill_name) {
                        approval_mode = mode;
                    }
                }
            }
        }

        // Step 5: Check session/workspace approval cache
        if approval_mode != ToolApprovalMode::Deny
            && self.tool_approvals.read().await.contains(&approval_key)
        {
            approval_mode = ToolApprovalMode::Auto;
        }

        // Step 6: Apply denial tracking
        let denial_settings = &self.settings.tools.approval.denial;
        if denial_settings.fallback_to_interactive
            && approval_mode == ToolApprovalMode::Deny
            && self.denial_counts.exceeded(
                denial_settings.max_consecutive_denials,
                denial_settings.max_total_denials,
            )
        {
            // Thresholds exceeded: fall back to interactive
            approval_mode = ToolApprovalMode::Prompt;
        }

        let mut approved = true;
        let mut deny_message: Option<String> = None;
        if approval_mode == ToolApprovalMode::Prompt {
            if let Some(handler) = self.interaction.clone() {
                let reason = if is_bash_tool(&tool_name) {
                    bash_command_from_args(&args).map(|cmd| format!("bash 命令: {cmd}"))
                } else if tool_name == "skill" {
                    skill_name_from_args(&args).map(|name| format!("skill: {name}"))
                } else {
                    None
                };
                let response = handler
                    .request_tool_approval(ToolApprovalRequest {
                        tool_name: tool_name.clone(),
                        arguments: args.clone(),
                        reason,
                        auto_decision: Some(approval_mode),
                        decision_reason: None,
                    })
                    .await?;
                match response.decision {
                    ToolApprovalDecision::AllowOnce => {
                        self.denial_counts.record_approval();
                    }
                    ToolApprovalDecision::AllowSession => {
                        self.tool_approvals
                            .write()
                            .await
                            .insert(approval_key.clone());
                        self.denial_counts.record_approval();
                    }
                    ToolApprovalDecision::AllowWorkspace => {
                        self.tool_approvals
                            .write()
                            .await
                            .insert(approval_key.clone());
                        self.store.insert_tool_approval(&approval_key).await?;
                        self.denial_counts.record_approval();
                    }
                    ToolApprovalDecision::Deny => {
                        approved = false;
                        deny_message = Some("工具调用被拒绝".to_string());
                        self.denial_counts.record_denial();
                    }
                }
            } else {
                approved = false;
                deny_message = Some("需要用户授权，但当前无交互处理器".to_string());
                self.denial_counts.record_denial();
            }
        } else if approval_mode == ToolApprovalMode::Deny {
            approved = false;
            deny_message = Some("工具调用被策略拒绝".to_string());
            self.denial_counts.record_denial();
        }

        // Emit PermissionDenied event if denied
        if !approved {
            self.emit(
                events,
                AgentEvent::PermissionDenied {
                    tool_name: tool_name.clone(),
                    reason: deny_message.clone().unwrap_or_default(),
                    permission_reason: Some(format!("Mode: {permission_mode}")),
                },
            );
        }

        self.emit(
            events,
            AgentEvent::ToolCallStarted {
                tool_call_id: tool_call_external_id.clone(),
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
                    tool_call_id: Some(tool_call_id.clone()),
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
                    tool_call_id: tool_call_id.clone(),
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
                let mut ok = output
                    .metadata
                    .get("ok")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
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
                    if let Some(updated_ok) = output.get("ok").and_then(|v| v.as_bool()) {
                        ok = updated_ok;
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
                        tool_call_id: Some(tool_call_id.clone()),
                        tool_calls: None,
                        created_at: Utc::now().timestamp(),
                    })
                    .await;
                let skill_hooks = self.load_skill_hooks(session_id).await?;
                let event_name = if ok {
                    HookEvent::PostToolUse
                } else {
                    HookEvent::PostToolUseFailure
                };
                let _ = self
                    .hooks
                    .apply_event(
                        event_name,
                        session_id,
                        serde_json::json!({
                            "tool_name": tool_name.clone(),
                            "tool_input": args,
                            "tool_output": output_content.clone(),
                            "ok": ok,
                        }),
                        &skill_hooks,
                    )
                    .await;
                self.emit(
                    events,
                    AgentEvent::ToolCallFinished {
                        tool_call_id: tool_call_id.clone(),
                        name: tool_name,
                        output: output_content,
                        ok,
                    },
                );
            }
            Err(err) => {
                let error_msg = format!("工具执行错误: {err}");
                let _ = self
                    .store
                    .record_tool_output(&tool_call_id, &error_msg)
                    .await;
                let _ = self
                    .append_message_with_hooks(Message {
                        id: Uuid::new_v4().to_string(),
                        session_id: session_id.to_string(),
                        role: MessageRole::Tool,
                        content: error_msg.clone(),
                        summary: false,
                        tool_call_id: Some(tool_call_id.clone()),
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
                            "tool_input": args,
                            "tool_output": error_msg.clone(),
                            "ok": false,
                        }),
                        &skill_hooks,
                    )
                    .await;
                self.emit(
                    events,
                    AgentEvent::ToolCallFinished {
                        tool_call_id: tool_call_id.clone(),
                        name: tool_name,
                        output: error_msg,
                        ok: false,
                    },
                );
            }
        }
        Ok(())
    }
}

/// A batch of tool calls to execute together.
enum ToolBatch {
    /// Read-only tools that can run concurrently.
    Parallel(Vec<ToolCall>),
    /// A single write/destructive tool that must run serially.
    Serial(Vec<ToolCall>),
}

/// Partition tool calls into batches: consecutive read-only calls form a parallel batch,
/// write calls each become a serial batch.
fn partition_tool_calls(calls: Vec<ToolCall>, registry: &ToolRegistry) -> Vec<ToolBatch> {
    let mut batches: Vec<ToolBatch> = Vec::new();
    let mut current_read_only: Vec<ToolCall> = Vec::new();

    for call in calls {
        if registry.is_read_only(&call.name) {
            current_read_only.push(call);
        } else {
            // Flush accumulated read-only calls as a parallel batch
            if !current_read_only.is_empty() {
                batches.push(ToolBatch::Parallel(std::mem::take(&mut current_read_only)));
            }
            // Write tool gets its own serial batch
            batches.push(ToolBatch::Serial(vec![call]));
        }
    }
    // Flush any remaining read-only calls
    if !current_read_only.is_empty() {
        batches.push(ToolBatch::Parallel(current_read_only));
    }

    batches
}

fn is_bash_tool(name: &str) -> bool {
    matches!(name, "bash" | "shell")
}

/// Check if a tool performs write or execute operations (not read-only).
fn is_write_or_execute_tool(name: &str) -> bool {
    matches!(
        name,
        "write" | "edit" | "apply_patch" | "patch" | "bash" | "shell" | "todowrite"
    )
}

fn bash_command_from_args(args: &JsonValue) -> Option<&str> {
    args.get("command").and_then(|v| v.as_str())
}

fn skill_name_from_args(args: &JsonValue) -> Option<&str> {
    args.get("name").and_then(|v| v.as_str())
}

fn approval_key(tool_name: &str, args: &JsonValue) -> String {
    if is_bash_tool(tool_name) {
        if let Some(command) = bash_command_from_args(args) {
            return format!("{tool_name}:{command}");
        }
    } else if tool_name == "skill" {
        if let Some(name) = skill_name_from_args(args) {
            return format!("{tool_name}:{name}");
        }
    }
    tool_name.to_string()
}

fn rewrite_skill_alias_call(
    tool_name: &str,
    args: &mut JsonValue,
    skill_names: &HashSet<String>,
) -> Option<String> {
    if !skill_names.contains(tool_name) {
        return None;
    }
    match args {
        JsonValue::Object(obj) => {
            obj.insert("name".to_string(), JsonValue::String(tool_name.to_string()));
        }
        _ => {
            *args = serde_json::json!({ "name": tool_name });
        }
    }
    Some("skill".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_key_uses_skill_name() {
        let key = approval_key("skill", &serde_json::json!({ "name": "deploy-prod" }));
        assert_eq!(key, "skill:deploy-prod");
    }

    #[test]
    fn rewrite_skill_alias_call_rewrites_object_args() {
        let mut args = serde_json::json!({ "command": "create_presentation" });
        let names = HashSet::from(["pptx".to_string()]);
        let mapped = rewrite_skill_alias_call("pptx", &mut args, &names);
        assert_eq!(mapped.as_deref(), Some("skill"));
        assert_eq!(args.get("name").and_then(|v| v.as_str()), Some("pptx"));
    }

    #[test]
    fn rewrite_skill_alias_call_wraps_non_object_args() {
        let mut args = serde_json::json!("whatever");
        let names = HashSet::from(["pptx".to_string()]);
        let mapped = rewrite_skill_alias_call("pptx", &mut args, &names);
        assert_eq!(mapped.as_deref(), Some("skill"));
        assert_eq!(args, serde_json::json!({ "name": "pptx" }));
    }

    #[test]
    fn rewrite_skill_alias_call_ignores_non_skill_name() {
        let mut args = serde_json::json!({ "x": 1 });
        let names = HashSet::from(["browser-automation".to_string()]);
        let mapped = rewrite_skill_alias_call("pptx", &mut args, &names);
        assert!(mapped.is_none());
        assert_eq!(args, serde_json::json!({ "x": 1 }));
    }
}
