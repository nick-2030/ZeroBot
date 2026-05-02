use crate::agent::Agent;
use crate::agents::AgentManager;
use crate::config::{McpLocalProtocol, McpServerConfig, Settings};
use crate::error::{ZeroBotError, ZeroBotResult};
use crate::events::AgentEvent;
use crate::hooks::HookManager;
use crate::interaction::{
    InteractionHandler, ToolApprovalDecision, ToolApprovalRequest, ToolApprovalResponse,
    UserInputRequest, UserInputResponse,
};
use crate::plugin::PluginManager;
use crate::provider::{AnthropicProvider, OpenAIProvider, Provider, ProviderFactory, TokenUsage};
use crate::session::{create_session_with_hooks, Message, MessageRole, SessionKind, SessionStore};
use crate::tool::{SubagentTool, ToolRegistry};
use agent_client_protocol as acp;
use agent_client_protocol::Client as _;
use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use serde_json::Value as JsonValue;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Mutex, RwLock};
use tracing::warn;
use uuid::Uuid;

const PERMISSION_ALLOW_ONCE: &str = "allow_once";
const PERMISSION_ALLOW_SESSION: &str = "allow_session";
const PERMISSION_REJECT: &str = "reject";
const CONFIG_ID_MODEL: &str = "model";
const CONFIG_ID_MODE: &str = "mode";

#[derive(Clone)]
pub struct AcpServerConfig {
    pub settings: Settings,
    pub cwd: PathBuf,
    pub store: Arc<dyn SessionStore>,
    pub base_hooks: HookManager,
    pub plugins: Option<Arc<PluginManager>>,
    pub tool_approvals: Arc<RwLock<HashSet<String>>>,
    pub default_provider: String,
    pub default_model: String,
}

#[derive(Clone)]
struct SessionState {
    id: String,
    cwd: PathBuf,
    provider_id: String,
    model: String,
    mode_id: Option<String>,
    mcp_servers: Vec<acp::McpServer>,
    prompt_abort: Option<tokio::task::AbortHandle>,
}

impl SessionState {
    fn new(
        id: impl Into<String>,
        cwd: PathBuf,
        provider_id: impl Into<String>,
        model: impl Into<String>,
        mode_id: Option<String>,
        mcp_servers: Vec<acp::McpServer>,
    ) -> Self {
        Self {
            id: id.into(),
            cwd,
            provider_id: provider_id.into(),
            model: model.into(),
            mode_id,
            mcp_servers,
            prompt_abort: None,
        }
    }

    fn model_id(&self) -> String {
        format!("{}/{}", self.provider_id, self.model)
    }
}

struct PermissionBridgeRequest {
    request: ToolApprovalRequest,
    respond_to: oneshot::Sender<ZeroBotResult<ToolApprovalResponse>>,
}

#[derive(Clone)]
struct AcpInteractionHandler {
    tx: mpsc::UnboundedSender<PermissionBridgeRequest>,
}

#[async_trait]
impl InteractionHandler for AcpInteractionHandler {
    async fn request_user_input(
        &self,
        _request: UserInputRequest,
    ) -> ZeroBotResult<UserInputResponse> {
        Err(ZeroBotError::Agent(
            "ACP 模式暂不支持 request_user_input".to_string(),
        ))
    }

    async fn request_tool_approval(
        &self,
        request: ToolApprovalRequest,
    ) -> ZeroBotResult<ToolApprovalResponse> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(PermissionBridgeRequest {
                request,
                respond_to: tx,
            })
            .map_err(|_| ZeroBotError::Agent("ACP 权限桥接已关闭".to_string()))?;
        rx.await
            .map_err(|_| ZeroBotError::Agent("ACP 权限桥接响应失败".to_string()))?
    }
}

pub struct AcpRuntime {
    config: AcpServerConfig,
    sessions: Mutex<HashMap<String, SessionState>>,
    connection: RefCell<Option<Rc<acp::AgentSideConnection>>>,
}

impl AcpRuntime {
    pub fn new(config: AcpServerConfig) -> Self {
        Self {
            config,
            sessions: Mutex::new(HashMap::new()),
            connection: RefCell::new(None),
        }
    }

    fn set_connection(&self, connection: Rc<acp::AgentSideConnection>) {
        *self.connection.borrow_mut() = Some(connection);
    }

    fn connection(&self) -> Option<Rc<acp::AgentSideConnection>> {
        self.connection.borrow().as_ref().cloned()
    }

    async fn send_update(&self, session_id: &str, update: acp::SessionUpdate) {
        let Some(conn) = self.connection() else {
            return;
        };
        if let Err(err) = conn
            .session_notification(acp::SessionNotification::new(
                session_id.to_string(),
                update,
            ))
            .await
        {
            warn!("failed to send ACP session update: {err}");
        }
    }

    async fn request_permission(
        &self,
        session_id: &str,
        request: ToolApprovalRequest,
    ) -> ZeroBotResult<ToolApprovalResponse> {
        let Some(conn) = self.connection() else {
            return Ok(ToolApprovalResponse {
                decision: ToolApprovalDecision::Deny,
            });
        };

        let tool_call_id = format!(
            "approval-{}",
            Uuid::new_v5(
                &Uuid::NAMESPACE_OID,
                format!("{}:{}", request.tool_name, request.arguments).as_bytes(),
            )
        );
        let title = request
            .reason
            .clone()
            .unwrap_or_else(|| format!("批准工具调用: {}", request.tool_name));
        let tool_call = acp::ToolCallUpdate::new(
            tool_call_id,
            acp::ToolCallUpdateFields::new()
                .title(title)
                .kind(to_tool_kind(&request.tool_name))
                .status(acp::ToolCallStatus::Pending)
                .raw_input(request.arguments.clone()),
        );

        let options = vec![
            acp::PermissionOption::new(
                PERMISSION_ALLOW_ONCE,
                "Allow once",
                acp::PermissionOptionKind::AllowOnce,
            ),
            acp::PermissionOption::new(
                PERMISSION_ALLOW_SESSION,
                "Allow session",
                acp::PermissionOptionKind::AllowAlways,
            ),
            acp::PermissionOption::new(
                PERMISSION_REJECT,
                "Reject",
                acp::PermissionOptionKind::RejectOnce,
            ),
        ];

        let response = conn
            .request_permission(acp::RequestPermissionRequest::new(
                session_id.to_string(),
                tool_call,
                options,
            ))
            .await
            .map_err(to_zerobot_permission_error)?;

        let decision = match response.outcome {
            acp::RequestPermissionOutcome::Cancelled => ToolApprovalDecision::Deny,
            acp::RequestPermissionOutcome::Selected(selected) => {
                match selected.option_id.0.as_ref() {
                    PERMISSION_ALLOW_ONCE => ToolApprovalDecision::AllowOnce,
                    PERMISSION_ALLOW_SESSION => ToolApprovalDecision::AllowSession,
                    _ => ToolApprovalDecision::Deny,
                }
            }
            _ => ToolApprovalDecision::Deny,
        };
        Ok(ToolApprovalResponse { decision })
    }

    async fn upsert_session(&self, state: SessionState) {
        self.sessions.lock().await.insert(state.id.clone(), state);
    }

    async fn session_state(&self, session_id: &str) -> Option<SessionState> {
        self.sessions.lock().await.get(session_id).cloned()
    }

    async fn require_session_state(&self, session_id: &str) -> acp::Result<SessionState> {
        self.session_state(session_id).await.ok_or_else(|| {
            acp::Error::invalid_params().data(serde_json::json!(format!("未知会话: {session_id}")))
        })
    }

    async fn set_prompt_abort(
        &self,
        session_id: &str,
        abort: Option<tokio::task::AbortHandle>,
    ) -> acp::Result<()> {
        let mut sessions = self.sessions.lock().await;
        let state = sessions.get_mut(session_id).ok_or_else(|| {
            acp::Error::invalid_params().data(serde_json::json!(format!("未知会话: {session_id}")))
        })?;
        state.prompt_abort = abort;
        Ok(())
    }

    async fn current_mode_state(&self, state: &mut SessionState) -> Option<acp::SessionModeState> {
        let modes = discover_modes(&state.cwd);
        if modes.is_empty() {
            return None;
        }
        let current = if let Some(existing) = state.mode_id.clone() {
            if modes.iter().any(|mode| mode.id.0.as_ref() == existing) {
                existing
            } else {
                default_mode_id(&modes)
            }
        } else {
            default_mode_id(&modes)
        };
        state.mode_id = Some(current.clone());
        Some(acp::SessionModeState::new(current, modes))
    }

    fn current_model_state(&self, state: &SessionState) -> acp::SessionModelState {
        let mut available = available_models_from_settings(&self.config.settings);
        if !available
            .iter()
            .any(|info| info.model_id.0.as_ref() == state.model_id())
        {
            available.push(acp::ModelInfo::new(state.model_id(), state.model_id()));
        }
        acp::SessionModelState::new(state.model_id(), available)
    }

    fn current_config_options(&self, state: &SessionState) -> Vec<acp::SessionConfigOption> {
        let model_options = available_models_from_settings(&self.config.settings)
            .into_iter()
            .map(|info| acp::SessionConfigSelectOption::new(info.model_id.0.to_string(), info.name))
            .collect::<Vec<_>>();

        let mut config = vec![acp::SessionConfigOption::select(
            CONFIG_ID_MODEL,
            "Model",
            state.model_id(),
            model_options,
        )
        .category(acp::SessionConfigOptionCategory::Model)];

        let mode_options = discover_modes(&state.cwd)
            .into_iter()
            .map(|mode| {
                acp::SessionConfigSelectOption::new(mode.id.0.to_string(), mode.name)
                    .description(mode.description)
            })
            .collect::<Vec<_>>();

        if !mode_options.is_empty() {
            config.push(
                acp::SessionConfigOption::select(
                    CONFIG_ID_MODE,
                    "Mode",
                    state
                        .mode_id
                        .clone()
                        .unwrap_or_else(|| mode_options[0].value.0.to_string()),
                    mode_options,
                )
                .category(acp::SessionConfigOptionCategory::Mode),
            );
        }

        config
    }

    async fn replay_history(&self, session_id: &str) -> acp::Result<()> {
        let history = self
            .config
            .store
            .list_messages(session_id)
            .await
            .map_err(to_acp_internal_error)?;

        let mut tool_names: HashMap<String, String> = HashMap::new();

        for message in history {
            match message.role {
                MessageRole::User => {
                    if !message.content.is_empty() {
                        self.send_update(
                            session_id,
                            acp::SessionUpdate::UserMessageChunk(acp::ContentChunk::new(
                                acp::ContentBlock::Text(acp::TextContent::new(message.content)),
                            )),
                        )
                        .await;
                    }
                }
                MessageRole::Assistant => {
                    if !message.content.is_empty() {
                        self.send_update(
                            session_id,
                            acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(
                                acp::ContentBlock::Text(acp::TextContent::new(message.content)),
                            )),
                        )
                        .await;
                    }

                    if let Some(calls) = message.tool_calls {
                        for call in calls {
                            tool_names.insert(call.id.clone(), call.name.clone());
                            self.send_update(
                                session_id,
                                acp::SessionUpdate::ToolCall(
                                    acp::ToolCall::new(call.id.clone(), call.name.clone())
                                        .kind(to_tool_kind(&call.name))
                                        .status(acp::ToolCallStatus::Pending)
                                        .raw_input(call.arguments),
                                ),
                            )
                            .await;
                        }
                    }
                }
                MessageRole::Tool => {
                    if let Some(tool_call_id) = message.tool_call_id {
                        let title = tool_names
                            .get(&tool_call_id)
                            .cloned()
                            .unwrap_or_else(|| "tool".to_string());
                        self.send_update(
                            session_id,
                            acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
                                tool_call_id,
                                acp::ToolCallUpdateFields::new()
                                    .title(title)
                                    .status(acp::ToolCallStatus::Completed)
                                    .content(vec![acp::ToolCallContent::from(
                                        acp::ContentBlock::Text(acp::TextContent::new(
                                            message.content,
                                        )),
                                    )]),
                            )),
                        )
                        .await;
                    }
                }
                MessageRole::System => {}
            }
        }

        Ok(())
    }

    async fn apply_mode_settings(
        &self,
        base: &Settings,
        cwd: &Path,
        mode_id: Option<&str>,
    ) -> ZeroBotResult<(Settings, HookManager, Option<String>)> {
        let Some(mode_id) = mode_id else {
            return Ok((base.clone(), HookManager::load(base, cwd, None)?, None));
        };

        let manager = AgentManager::new(cwd);
        let def = manager.load(mode_id)?;

        let mut settings = base.clone();
        let mut system_prompt = String::new();
        if !def.description.trim().is_empty() {
            system_prompt.push_str(&format!("模式描述：{}", def.description.trim()));
        }
        if !def.body.trim().is_empty() {
            if !system_prompt.is_empty() {
                system_prompt.push_str("\n\n");
            }
            system_prompt.push_str(def.body.trim());
        }
        if !system_prompt.trim().is_empty() {
            settings.agent.system_prompt = Some(system_prompt);
        }
        if let Some(enabled_tools) = def.tools.clone() {
            settings.tools.enabled = enabled_tools;
        }
        let hooks = HookManager::load(&settings, cwd, Some(def.hooks.clone()))?;

        Ok((settings, hooks, Some(def.name)))
    }

    fn build_provider(
        &self,
        settings: &Settings,
        provider_id: &str,
    ) -> ZeroBotResult<Box<dyn Provider>> {
        let info = settings.providers.get(provider_id);
        let (kind, api_key, base_url) = if let Some(info) = info {
            (
                info.kind.clone(),
                resolve_api_key(info.api_key.clone(), info.api_key_env.clone(), provider_id),
                info.base_url.clone(),
            )
        } else {
            (
                provider_id.to_string(),
                resolve_api_key(None, None, provider_id),
                None,
            )
        };

        match kind.as_str() {
            "openai" => Ok(Box::new(OpenAIProvider::new(api_key, base_url))),
            "anthropic" => Ok(Box::new(AnthropicProvider::new(api_key, base_url))),
            _ => Err(ZeroBotError::Provider(format!(
                "不支持的提供商类型: {kind}"
            ))),
        }
    }

    async fn build_turn_tools(
        &self,
        turn_settings: &Settings,
        cwd: &Path,
        model: &str,
        interaction: Option<Arc<dyn InteractionHandler>>,
        provider_id: &str,
        hooks: &HookManager,
    ) -> ZeroBotResult<ToolRegistry> {
        let mut tools = ToolRegistry::with_builtin_async(
            turn_settings,
            cwd,
            Some(self.config.store.clone()),
            self.config.plugins.clone(),
        )
        .await?;

        let settings_for_factory = turn_settings.clone();
        let provider_id = provider_id.to_string();
        let provider_factory: ProviderFactory = Arc::new(move || {
            let info = settings_for_factory.providers.get(&provider_id);
            let (kind, api_key, base_url) = if let Some(info) = info {
                (
                    info.kind.clone(),
                    resolve_api_key(info.api_key.clone(), info.api_key_env.clone(), &provider_id),
                    info.base_url.clone(),
                )
            } else {
                (
                    provider_id.clone(),
                    resolve_api_key(None, None, &provider_id),
                    None,
                )
            };
            match kind.as_str() {
                "openai" => {
                    Ok(Box::new(OpenAIProvider::new(api_key, base_url)) as Box<dyn Provider>)
                }
                "anthropic" => {
                    Ok(Box::new(AnthropicProvider::new(api_key, base_url)) as Box<dyn Provider>)
                }
                _ => Err(ZeroBotError::Provider(format!(
                    "不支持的提供商类型: {kind}"
                ))),
            }
        });

        let subagent_tools = tools.clone();
        tools.register(SubagentTool::new(
            turn_settings.clone(),
            self.config.store.clone(),
            subagent_tools,
            cwd.to_path_buf(),
            provider_factory,
            model.to_string(),
            hooks.clone(),
            interaction,
            self.config.tool_approvals.clone(),
        ));

        Ok(tools)
    }

    fn merge_mcp_servers(
        &self,
        base: &[McpServerConfig],
        incoming: &[acp::McpServer],
    ) -> Vec<McpServerConfig> {
        let mut merged: HashMap<String, McpServerConfig> = HashMap::new();
        for item in base {
            merged.insert(item.name().to_string(), item.clone());
        }
        for item in incoming {
            if let Some(converted) = convert_mcp_server(item) {
                merged.insert(converted.name().to_string(), converted);
            }
        }
        merged.into_values().collect()
    }

    async fn prompt_inner(&self, args: acp::PromptRequest) -> acp::Result<acp::PromptResponse> {
        let session_id = args.session_id.0.to_string();

        let mut state = self.require_session_state(&session_id).await?;
        if state.prompt_abort.is_some() {
            return Err(
                acp::Error::invalid_params().data(serde_json::json!("会话中已有进行中的 prompt"))
            );
        }

        let input_text = prompt_blocks_to_text(&args.prompt);

        let mut turn_settings = self.config.settings.clone();
        turn_settings.mcp.enabled = turn_settings.mcp.enabled || !state.mcp_servers.is_empty();
        if turn_settings.mcp.enabled {
            turn_settings.mcp.servers =
                self.merge_mcp_servers(&turn_settings.mcp.servers, &state.mcp_servers);
        }

        let (turn_settings, hooks, resolved_mode) = self
            .apply_mode_settings(&turn_settings, &state.cwd, state.mode_id.as_deref())
            .await
            .map_err(to_acp_internal_error)?;

        if resolved_mode.is_some() && resolved_mode != state.mode_id {
            state.mode_id = resolved_mode;
            self.upsert_session(state.clone()).await;
        }

        let provider = self
            .build_provider(&turn_settings, &state.provider_id)
            .map_err(to_acp_internal_error)?;

        let (approval_tx, mut approval_rx) = mpsc::unbounded_channel();
        let interaction_handler: Arc<dyn InteractionHandler> =
            Arc::new(AcpInteractionHandler { tx: approval_tx });

        let tools = self
            .build_turn_tools(
                &turn_settings,
                &state.cwd,
                &state.model,
                Some(interaction_handler.clone()),
                &state.provider_id,
                &hooks,
            )
            .await
            .map_err(to_acp_internal_error)?;

        let agent = Agent::new(
            provider,
            state.model.clone(),
            turn_settings,
            self.config.store.clone(),
            tools,
            state.cwd.clone(),
            hooks,
            Some(interaction_handler),
            self.config.plugins.clone(),
            self.config.tool_approvals.clone(),
            None,
            None,
        );

        let (event_tx, mut event_rx) = mpsc::unbounded_channel();

        let session_id_for_run = session_id.clone();
        let input_for_run = input_text.clone();
        let run_handle = tokio::spawn(async move {
            agent
                .run_turn(&session_id_for_run, &input_for_run, Some(event_tx))
                .await
        });

        let abort = run_handle.abort_handle();
        self.set_prompt_abort(&session_id, Some(abort)).await?;

        let mut run_handle = run_handle;
        let mut latest_usage: Option<acp::Usage> = None;
        let mut last_context_limit: Option<u64> = None;

        let run_result = loop {
            tokio::select! {
                maybe_request = approval_rx.recv() => {
                    if let Some(request) = maybe_request {
                        let response = self.request_permission(&session_id, request.request).await;
                        let _ = request.respond_to.send(response);
                    }
                }
                maybe_event = event_rx.recv() => {
                    if let Some(event) = maybe_event {
                        match event {
                            AgentEvent::UserMessage { content } => {
                                self.send_update(
                                    &session_id,
                                    acp::SessionUpdate::UserMessageChunk(acp::ContentChunk::new(
                                        acp::ContentBlock::Text(acp::TextContent::new(content)),
                                    )),
                                ).await;
                            }
                            AgentEvent::AssistantDelta { content } | AgentEvent::AssistantMessage { content } => {
                                if !content.is_empty() {
                                    self.send_update(
                                        &session_id,
                                        acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(
                                            acp::ContentBlock::Text(acp::TextContent::new(content)),
                                        )),
                                    ).await;
                                }
                            }
                            AgentEvent::ToolCallStarted {
                                tool_call_id,
                                name,
                                input,
                            } => {
                                self.send_update(
                                    &session_id,
                                    acp::SessionUpdate::ToolCall(
                                        acp::ToolCall::new(tool_call_id, name.clone())
                                            .kind(to_tool_kind(&name))
                                            .status(acp::ToolCallStatus::Pending)
                                            .raw_input(parse_json_string(&input))
                                            .locations(locations_from_tool_io(&name, &input)),
                                    ),
                                ).await;
                            }
                            AgentEvent::ToolCallFinished {
                                tool_call_id,
                                name,
                                output,
                                ok,
                            } => {
                                self.send_update(
                                    &session_id,
                                    acp::SessionUpdate::ToolCallUpdate(
                                        acp::ToolCallUpdate::new(
                                            tool_call_id,
                                            acp::ToolCallUpdateFields::new()
                                                .kind(to_tool_kind(&name))
                                                .title(name)
                                                .status(if ok {
                                                    acp::ToolCallStatus::Completed
                                                } else {
                                                    acp::ToolCallStatus::Failed
                                                })
                                                .raw_output(parse_json_string(&output))
                                                .content(vec![acp::ToolCallContent::from(
                                                    acp::ContentBlock::Text(acp::TextContent::new(output)),
                                                )]),
                                        ),
                                    ),
                                ).await;
                            }
                            AgentEvent::ContextUsage { used, limit } => {
                                last_context_limit = limit.map(u64::from);
                                let size = limit.map(u64::from).unwrap_or_else(|| (used as u64).max(1));
                                self.send_update(
                                    &session_id,
                                    acp::SessionUpdate::UsageUpdate(acp::UsageUpdate::new(used as u64, size)),
                                ).await;
                            }
                            AgentEvent::Usage { usage } => {
                                let prompt_usage = usage_to_acp_usage(&usage);
                                latest_usage = Some(prompt_usage.clone());
                                let used = prompt_usage.total_tokens;
                                let size = last_context_limit.unwrap_or(used.max(1));
                                self.send_update(
                                    &session_id,
                                    acp::SessionUpdate::UsageUpdate(acp::UsageUpdate::new(used, size)),
                                ).await;
                            }
                            AgentEvent::Error { message } => {
                                self.send_update(
                                    &session_id,
                                    acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(
                                        acp::ContentBlock::Text(acp::TextContent::new(format!(
                                            "[error] {message}"
                                        ))),
                                    )),
                                ).await;
                            }
                            AgentEvent::Done
                            | AgentEvent::SessionStarted { .. }
                            | AgentEvent::SessionResumed { .. }
                            | AgentEvent::PluginWarning { .. }
                            | AgentEvent::ToolBatchStarted { .. }
                            | AgentEvent::SessionCost { .. } => {}
                        }
                    }
                }
                result = &mut run_handle => {
                    break result;
                }
            }
        };

        self.set_prompt_abort(&session_id, None).await?;

        match run_result {
            Err(join_err) if join_err.is_cancelled() => {
                let mut response = acp::PromptResponse::new(acp::StopReason::Cancelled);
                if let Some(usage) = latest_usage {
                    response = response.usage(usage);
                }
                Ok(response)
            }
            Err(join_err) => Err(to_acp_internal_error(join_err)),
            Ok(Err(err)) => Err(to_acp_internal_error(err)),
            Ok(Ok(_)) => {
                let mut response = acp::PromptResponse::new(acp::StopReason::EndTurn);
                if let Some(usage) = latest_usage {
                    response = response.usage(usage);
                }
                Ok(response)
            }
        }
    }
}

#[async_trait(?Send)]
impl acp::Agent for AcpRuntime {
    async fn initialize(
        &self,
        _args: acp::InitializeRequest,
    ) -> acp::Result<acp::InitializeResponse> {
        let capabilities = acp::AgentCapabilities::new()
            .load_session(true)
            .prompt_capabilities(
                acp::PromptCapabilities::new()
                    .image(true)
                    .embedded_context(true),
            )
            .mcp_capabilities(acp::McpCapabilities::new().http(true).sse(true))
            .session_capabilities(
                acp::SessionCapabilities::new()
                    .list(acp::SessionListCapabilities::new())
                    .fork(acp::SessionForkCapabilities::new())
                    .resume(acp::SessionResumeCapabilities::new()),
            );

        Ok(acp::InitializeResponse::new(acp::ProtocolVersion::LATEST)
            .agent_capabilities(capabilities)
            .agent_info(acp::Implementation::new(
                "ZeroBot",
                env!("CARGO_PKG_VERSION"),
            )))
    }

    async fn authenticate(
        &self,
        _args: acp::AuthenticateRequest,
    ) -> acp::Result<acp::AuthenticateResponse> {
        Ok(acp::AuthenticateResponse::new())
    }

    async fn new_session(
        &self,
        args: acp::NewSessionRequest,
    ) -> acp::Result<acp::NewSessionResponse> {
        let session = create_session_with_hooks(
            self.config.store.as_ref(),
            &self.config.base_hooks,
            "ACP 会话".to_string(),
            None,
            SessionKind::Main,
        )
        .await
        .map_err(to_acp_internal_error)?;

        let mut state = SessionState::new(
            session.id.clone(),
            args.cwd,
            self.config.default_provider.clone(),
            self.config.default_model.clone(),
            None,
            args.mcp_servers,
        );
        let modes = self.current_mode_state(&mut state).await;
        let models = self.current_model_state(&state);
        let config_options = self.current_config_options(&state);
        self.upsert_session(state).await;

        Ok(acp::NewSessionResponse::new(session.id)
            .modes(modes)
            .models(models)
            .config_options(config_options))
    }

    async fn load_session(
        &self,
        args: acp::LoadSessionRequest,
    ) -> acp::Result<acp::LoadSessionResponse> {
        self.config
            .store
            .get_session(&args.session_id.0)
            .await
            .map_err(to_acp_internal_error)?
            .ok_or_else(|| {
                acp::Error::invalid_params().data(serde_json::json!(format!(
                    "会话不存在: {}",
                    args.session_id.0
                )))
            })?;

        let existing = self.session_state(&args.session_id.0).await;
        let mut state = SessionState::new(
            args.session_id.0.to_string(),
            args.cwd,
            existing
                .as_ref()
                .map(|s| s.provider_id.clone())
                .unwrap_or_else(|| self.config.default_provider.clone()),
            existing
                .as_ref()
                .map(|s| s.model.clone())
                .unwrap_or_else(|| self.config.default_model.clone()),
            existing.as_ref().and_then(|s| s.mode_id.clone()),
            args.mcp_servers,
        );

        let modes = self.current_mode_state(&mut state).await;
        let models = self.current_model_state(&state);
        let config_options = self.current_config_options(&state);
        self.upsert_session(state.clone()).await;

        self.replay_history(&state.id).await?;

        Ok(acp::LoadSessionResponse::new()
            .modes(modes)
            .models(models)
            .config_options(config_options))
    }

    async fn prompt(&self, args: acp::PromptRequest) -> acp::Result<acp::PromptResponse> {
        self.prompt_inner(args).await
    }

    async fn cancel(&self, args: acp::CancelNotification) -> acp::Result<()> {
        let mut sessions = self.sessions.lock().await;
        if let Some(state) = sessions.get_mut(&args.session_id.0.to_string()) {
            if let Some(abort) = state.prompt_abort.take() {
                abort.abort();
            }
        }
        Ok(())
    }

    async fn set_session_mode(
        &self,
        args: acp::SetSessionModeRequest,
    ) -> acp::Result<acp::SetSessionModeResponse> {
        let mut sessions = self.sessions.lock().await;
        let state = sessions
            .get_mut(args.session_id.0.as_ref())
            .ok_or_else(|| {
                acp::Error::invalid_params().data(serde_json::json!(format!(
                    "未知会话: {}",
                    args.session_id.0
                )))
            })?;

        let available = discover_modes(&state.cwd);
        let target = args.mode_id.0.as_ref();
        if !available.iter().any(|mode| mode.id.0.as_ref() == target) {
            return Err(
                acp::Error::invalid_params().data(serde_json::json!(format!("未知模式: {target}")))
            );
        }

        state.mode_id = Some(target.to_string());
        let session_id = state.id.clone();
        drop(sessions);

        self.send_update(
            &session_id,
            acp::SessionUpdate::CurrentModeUpdate(acp::CurrentModeUpdate::new(target.to_string())),
        )
        .await;

        if let Some(updated) = self.session_state(&session_id).await {
            self.send_update(
                &session_id,
                acp::SessionUpdate::ConfigOptionUpdate(acp::ConfigOptionUpdate::new(
                    self.current_config_options(&updated),
                )),
            )
            .await;
        }

        Ok(acp::SetSessionModeResponse::new())
    }

    async fn set_session_model(
        &self,
        args: acp::SetSessionModelRequest,
    ) -> acp::Result<acp::SetSessionModelResponse> {
        let session_id = args.session_id.0.to_string();
        let (provider_id, model) = {
            let state = self.require_session_state(&session_id).await?;
            parse_model_id(args.model_id.0.as_ref(), &state.provider_id)
        };

        {
            let mut sessions = self.sessions.lock().await;
            let state = sessions.get_mut(session_id.as_str()).ok_or_else(|| {
                acp::Error::invalid_params()
                    .data(serde_json::json!(format!("未知会话: {session_id}")))
            })?;
            state.provider_id = provider_id;
            state.model = model;
        }

        if let Some(updated) = self.session_state(&session_id).await {
            self.send_update(
                &session_id,
                acp::SessionUpdate::ConfigOptionUpdate(acp::ConfigOptionUpdate::new(
                    self.current_config_options(&updated),
                )),
            )
            .await;
        }

        Ok(acp::SetSessionModelResponse::new())
    }

    async fn set_session_config_option(
        &self,
        args: acp::SetSessionConfigOptionRequest,
    ) -> acp::Result<acp::SetSessionConfigOptionResponse> {
        let session_id = args.session_id.0.to_string();
        let config_id = args.config_id.0.to_string();
        let value = args
            .value
            .as_value_id()
            .map(|id| id.0.to_string())
            .ok_or_else(|| {
                acp::Error::invalid_params()
                    .data(serde_json::json!("当前仅支持 value_id 类型的配置值"))
            })?;

        match config_id.as_str() {
            CONFIG_ID_MODEL => {
                let (provider_id, model) = {
                    let state = self.require_session_state(&session_id).await?;
                    parse_model_id(&value, &state.provider_id)
                };
                let mut sessions = self.sessions.lock().await;
                let state = sessions.get_mut(session_id.as_str()).ok_or_else(|| {
                    acp::Error::invalid_params()
                        .data(serde_json::json!(format!("未知会话: {session_id}")))
                })?;
                state.provider_id = provider_id;
                state.model = model;
            }
            CONFIG_ID_MODE => {
                let mut sessions = self.sessions.lock().await;
                let state = sessions.get_mut(session_id.as_str()).ok_or_else(|| {
                    acp::Error::invalid_params()
                        .data(serde_json::json!(format!("未知会话: {session_id}")))
                })?;
                let available = discover_modes(&state.cwd);
                if !available.iter().any(|mode| mode.id.0.as_ref() == value) {
                    return Err(acp::Error::invalid_params()
                        .data(serde_json::json!(format!("未知模式: {value}"))));
                }
                state.mode_id = Some(value.clone());
            }
            _ => {
                return Err(acp::Error::invalid_params()
                    .data(serde_json::json!(format!("未知配置项: {config_id}"))))
            }
        }

        let updated = self.require_session_state(&session_id).await?;
        Ok(acp::SetSessionConfigOptionResponse::new(
            self.current_config_options(&updated),
        ))
    }

    async fn list_sessions(
        &self,
        args: acp::ListSessionsRequest,
    ) -> acp::Result<acp::ListSessionsResponse> {
        let sessions = self
            .config
            .store
            .list_sessions()
            .await
            .map_err(to_acp_internal_error)?;
        let states = self.sessions.lock().await;

        let filtered = sessions
            .into_iter()
            .filter_map(|session| {
                let known_cwd = states
                    .get(&session.id)
                    .map(|state| state.cwd.clone())
                    .unwrap_or_else(|| self.config.cwd.clone());
                if let Some(expected) = &args.cwd {
                    if &known_cwd != expected {
                        return None;
                    }
                }
                let updated_at = Utc
                    .timestamp_opt(session.updated_at, 0)
                    .single()
                    .map(|dt| dt.to_rfc3339());
                Some(
                    acp::SessionInfo::new(session.id, known_cwd)
                        .title(session.title)
                        .updated_at(updated_at),
                )
            })
            .collect::<Vec<_>>();

        Ok(acp::ListSessionsResponse::new(filtered))
    }

    async fn fork_session(
        &self,
        args: acp::ForkSessionRequest,
    ) -> acp::Result<acp::ForkSessionResponse> {
        let source_id = args.session_id.0.to_string();
        self.config
            .store
            .get_session(&source_id)
            .await
            .map_err(to_acp_internal_error)?
            .ok_or_else(|| {
                acp::Error::invalid_params()
                    .data(serde_json::json!(format!("会话不存在: {source_id}")))
            })?;

        let new_session = create_session_with_hooks(
            self.config.store.as_ref(),
            &self.config.base_hooks,
            format!("fork:{source_id}"),
            Some(source_id.clone()),
            SessionKind::Main,
        )
        .await
        .map_err(to_acp_internal_error)?;

        let messages = self
            .config
            .store
            .list_messages(&source_id)
            .await
            .map_err(to_acp_internal_error)?;
        for message in messages {
            let copied = Message {
                id: Uuid::new_v4().to_string(),
                session_id: new_session.id.clone(),
                role: message.role,
                content: message.content,
                summary: message.summary,
                tool_call_id: message.tool_call_id,
                tool_calls: message.tool_calls,
                created_at: message.created_at,
            };
            self.config
                .store
                .append_message(copied)
                .await
                .map_err(to_acp_internal_error)?;
        }

        let existing = self.session_state(&source_id).await;
        let mut state = SessionState::new(
            new_session.id.clone(),
            args.cwd,
            existing
                .as_ref()
                .map(|s| s.provider_id.clone())
                .unwrap_or_else(|| self.config.default_provider.clone()),
            existing
                .as_ref()
                .map(|s| s.model.clone())
                .unwrap_or_else(|| self.config.default_model.clone()),
            existing.as_ref().and_then(|s| s.mode_id.clone()),
            args.mcp_servers,
        );

        let modes = self.current_mode_state(&mut state).await;
        let models = self.current_model_state(&state);
        let config_options = self.current_config_options(&state);
        self.upsert_session(state.clone()).await;
        self.replay_history(&state.id).await?;

        Ok(acp::ForkSessionResponse::new(new_session.id)
            .modes(modes)
            .models(models)
            .config_options(config_options))
    }

    async fn resume_session(
        &self,
        args: acp::ResumeSessionRequest,
    ) -> acp::Result<acp::ResumeSessionResponse> {
        let session_id = args.session_id.0.to_string();
        self.config
            .store
            .get_session(&session_id)
            .await
            .map_err(to_acp_internal_error)?
            .ok_or_else(|| {
                acp::Error::invalid_params()
                    .data(serde_json::json!(format!("会话不存在: {session_id}")))
            })?;

        let existing = self.session_state(&session_id).await;
        let mut state = SessionState::new(
            session_id,
            args.cwd,
            existing
                .as_ref()
                .map(|s| s.provider_id.clone())
                .unwrap_or_else(|| self.config.default_provider.clone()),
            existing
                .as_ref()
                .map(|s| s.model.clone())
                .unwrap_or_else(|| self.config.default_model.clone()),
            existing.as_ref().and_then(|s| s.mode_id.clone()),
            args.mcp_servers,
        );
        let modes = self.current_mode_state(&mut state).await;
        let models = self.current_model_state(&state);
        let config_options = self.current_config_options(&state);
        self.upsert_session(state).await;

        Ok(acp::ResumeSessionResponse::new()
            .modes(modes)
            .models(models)
            .config_options(config_options))
    }
}

pub async fn run_stdio(config: AcpServerConfig) -> ZeroBotResult<()> {
    let runtime = Rc::new(AcpRuntime::new(config));
    let stdin = futures::io::AllowStdIo::new(io::stdin());
    let stdout = futures::io::AllowStdIo::new(io::stdout());

    let runtime_for_conn = runtime.clone();
    let (connection, io_task) =
        acp::AgentSideConnection::new(runtime_for_conn, stdout, stdin, |fut| {
            tokio::task::spawn_local(fut);
        });
    runtime.set_connection(Rc::new(connection));

    io_task.await.map_err(to_zerobot_acp_error)
}

fn discover_modes(cwd: &Path) -> Vec<acp::SessionMode> {
    let manager = AgentManager::new(cwd);
    let mut defs = match manager.discover() {
        Ok(defs) => defs,
        Err(_) => return Vec::new(),
    };
    defs.sort_by(|a, b| a.name.cmp(&b.name));
    defs.into_iter()
        .filter(|def| !def.name.trim().is_empty())
        .map(|def| {
            let id = def.name.clone();
            acp::SessionMode::new(id, def.name).description(def.description)
        })
        .collect()
}

fn default_mode_id(modes: &[acp::SessionMode]) -> String {
    if modes.iter().any(|mode| mode.id.0.as_ref() == "execute") {
        "execute".to_string()
    } else {
        modes[0].id.0.to_string()
    }
}

fn available_models_from_settings(settings: &Settings) -> Vec<acp::ModelInfo> {
    let mut items = Vec::new();
    for (provider_id, info) in &settings.providers {
        if let Some(model) = &info.model {
            let model_id = format!("{provider_id}/{model}");
            items.push(acp::ModelInfo::new(model_id.clone(), model_id));
        }
    }
    items.sort_by(|a, b| a.name.cmp(&b.name));
    items.dedup_by(|a, b| a.model_id.0 == b.model_id.0);
    items
}

fn parse_model_id(value: &str, fallback_provider: &str) -> (String, String) {
    let raw = value.trim();
    if raw.is_empty() {
        return (fallback_provider.to_string(), String::new());
    }
    if let Some((provider, model)) = raw.split_once('/') {
        (provider.to_string(), model.to_string())
    } else {
        (fallback_provider.to_string(), raw.to_string())
    }
}

fn parse_json_string(raw: &str) -> JsonValue {
    serde_json::from_str(raw).unwrap_or_else(|_| JsonValue::String(raw.to_string()))
}

fn to_tool_kind(tool_name: &str) -> acp::ToolKind {
    match tool_name.to_lowercase().as_str() {
        "read" => acp::ToolKind::Read,
        "write" | "edit" | "apply_patch" | "patch" => acp::ToolKind::Edit,
        "glob" | "grep" | "search" => acp::ToolKind::Search,
        "bash" | "shell" => acp::ToolKind::Execute,
        "webfetch" | "fetch" => acp::ToolKind::Fetch,
        _ => acp::ToolKind::Other,
    }
}

fn locations_from_tool_io(tool_name: &str, input: &str) -> Vec<acp::ToolCallLocation> {
    let parsed = parse_json_string(input);
    let value = match parsed {
        JsonValue::Object(map) => JsonValue::Object(map),
        _ => return Vec::new(),
    };

    let pick = |key: &str| {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    };

    match tool_name.to_lowercase().as_str() {
        "read" | "write" | "edit" | "apply_patch" | "patch" => pick("path")
            .or_else(|| pick("filePath"))
            .or_else(|| pick("filepath"))
            .map(|path| vec![acp::ToolCallLocation::new(path)])
            .unwrap_or_default(),
        "glob" | "grep" | "list" => pick("path")
            .map(|path| vec![acp::ToolCallLocation::new(path)])
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn prompt_blocks_to_text(blocks: &[acp::ContentBlock]) -> String {
    let mut out = Vec::new();

    for block in blocks {
        match block {
            acp::ContentBlock::Text(text) => {
                if !text.text.trim().is_empty() {
                    out.push(text.text.clone());
                }
            }
            acp::ContentBlock::ResourceLink(link) => {
                out.push(format!(
                    "[resource_link] name={} uri={} mime={}",
                    link.name,
                    link.uri,
                    link.mime_type.clone().unwrap_or_default(),
                ));
            }
            acp::ContentBlock::Resource(resource) => match &resource.resource {
                acp::EmbeddedResourceResource::TextResourceContents(text) => {
                    if !text.text.trim().is_empty() {
                        out.push(text.text.clone());
                    } else {
                        out.push(format!("[resource_text] uri={}", text.uri));
                    }
                }
                acp::EmbeddedResourceResource::BlobResourceContents(blob) => {
                    out.push(format!(
                        "[resource_blob] uri={} mime={} bytes(base64)={}",
                        blob.uri,
                        blob.mime_type.clone().unwrap_or_default(),
                        blob.blob.len(),
                    ));
                }
                _ => {}
            },
            acp::ContentBlock::Image(image) => {
                out.push(format!(
                    "[image] mime={} uri={} bytes(base64)={}",
                    image.mime_type,
                    image.uri.clone().unwrap_or_default(),
                    image.data.len(),
                ));
            }
            acp::ContentBlock::Audio(audio) => {
                out.push(format!(
                    "[audio] mime={} bytes(base64)={}",
                    audio.mime_type,
                    audio.data.len(),
                ));
            }
            _ => {}
        }
    }

    out.join("\n\n")
}

fn convert_mcp_server(server: &acp::McpServer) -> Option<McpServerConfig> {
    match server {
        acp::McpServer::Http(http) => Some(McpServerConfig::Remote {
            name: http.name.clone(),
            url: http.url.clone(),
            headers: http
                .headers
                .iter()
                .map(|header| (header.name.clone(), header.value.clone()))
                .collect(),
            timeout_ms: None,
            enabled: Some(true),
        }),
        acp::McpServer::Sse(sse) => Some(McpServerConfig::Remote {
            name: sse.name.clone(),
            url: sse.url.clone(),
            headers: sse
                .headers
                .iter()
                .map(|header| (header.name.clone(), header.value.clone()))
                .collect(),
            timeout_ms: None,
            enabled: Some(true),
        }),
        acp::McpServer::Stdio(local) => {
            let mut command = vec![local.command.to_string_lossy().to_string()];
            command.extend(local.args.clone());
            Some(McpServerConfig::Local {
                name: local.name.clone(),
                command,
                env: local
                    .env
                    .iter()
                    .map(|item| (item.name.clone(), item.value.clone()))
                    .collect(),
                protocol: Some(McpLocalProtocol::ContentLength),
                timeout_ms: None,
                enabled: Some(true),
            })
        }
        _ => None,
    }
}

fn usage_to_acp_usage(usage: &TokenUsage) -> acp::Usage {
    let input = u64::from(usage.input_tokens.unwrap_or(0));
    let output = u64::from(usage.output_tokens.unwrap_or(0));
    let total = u64::from(usage.total_tokens.unwrap_or((input + output) as u32));
    acp::Usage::new(total, input, output)
}

fn to_acp_internal_error(err: impl std::fmt::Display) -> acp::Error {
    acp::Error::internal_error().data(serde_json::json!(err.to_string()))
}

fn to_zerobot_acp_error(err: acp::Error) -> ZeroBotError {
    ZeroBotError::Agent(format!("ACP 错误: {}", err.message))
}

fn to_zerobot_permission_error(err: acp::Error) -> ZeroBotError {
    ZeroBotError::Agent(format!("ACP permission 错误: {}", err.message))
}

fn resolve_api_key(
    api_key: Option<String>,
    api_key_env: Option<String>,
    provider_id: &str,
) -> String {
    if let Some(key) = api_key {
        return key;
    }
    if let Some(env) = api_key_env {
        if let Ok(value) = std::env::var(env) {
            return value;
        }
    }

    let env_name = match provider_id {
        "anthropic" => "ANTHROPIC_API_KEY",
        _ => "OPENAI_API_KEY",
    };
    std::env::var(env_name).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProviderSettings;
    use crate::session::SqliteSessionStore;
    use tempfile::TempDir;

    async fn build_runtime(tmp: &TempDir) -> AcpRuntime {
        let mut settings = Settings::default();
        settings.providers.insert(
            "openai".to_string(),
            ProviderSettings {
                kind: "openai".to_string(),
                model: Some("gpt-4o-mini".to_string()),
                ..ProviderSettings::default()
            },
        );
        settings.providers.insert(
            "anthropic".to_string(),
            ProviderSettings {
                kind: "anthropic".to_string(),
                model: Some("claude-3-7-sonnet-latest".to_string()),
                ..ProviderSettings::default()
            },
        );
        settings.default_provider = Some("openai".to_string());
        settings.default_model = Some("gpt-4o-mini".to_string());

        let db_path = tmp.path().join("sessions.db");
        let store = Arc::new(
            SqliteSessionStore::new(db_path)
                .await
                .expect("create sqlite store"),
        );
        store.init().await.expect("init sqlite store");
        let hooks = HookManager::load(&settings, tmp.path(), None).expect("load hooks");

        AcpRuntime::new(AcpServerConfig {
            settings,
            cwd: tmp.path().to_path_buf(),
            store,
            base_hooks: hooks,
            plugins: None,
            tool_approvals: Arc::new(RwLock::new(HashSet::new())),
            default_provider: "openai".to_string(),
            default_model: "gpt-4o-mini".to_string(),
        })
    }

    #[test]
    fn parse_model_id_prefers_explicit_provider() {
        let (provider, model) = parse_model_id("openai/gpt-5", "anthropic");
        assert_eq!(provider, "openai");
        assert_eq!(model, "gpt-5");
    }

    #[test]
    fn parse_model_id_falls_back_to_current_provider() {
        let (provider, model) = parse_model_id("gpt-5", "openai");
        assert_eq!(provider, "openai");
        assert_eq!(model, "gpt-5");
    }

    #[test]
    fn prompt_blocks_to_text_degrades_multimodal() {
        let blocks = vec![
            acp::ContentBlock::Text(acp::TextContent::new("hello")),
            acp::ContentBlock::Image(
                acp::ImageContent::new("AAAA", "image/png").uri("file:///tmp/a.png"),
            ),
            acp::ContentBlock::ResourceLink(acp::ResourceLink::new("ctx", "file:///tmp/ctx.txt")),
        ];
        let out = prompt_blocks_to_text(&blocks);
        assert!(out.contains("hello"));
        assert!(out.contains("[image]"));
        assert!(out.contains("[resource_link]"));
    }

    #[test]
    fn convert_mcp_http_to_remote_config() {
        let server = acp::McpServer::Http(
            acp::McpServerHttp::new("remote-a", "https://example.com/mcp")
                .headers(vec![acp::HttpHeader::new("Authorization", "Bearer token")]),
        );
        let converted = convert_mcp_server(&server).expect("converted");
        match converted {
            McpServerConfig::Remote {
                name, url, headers, ..
            } => {
                assert_eq!(name, "remote-a");
                assert_eq!(url, "https://example.com/mcp");
                assert_eq!(
                    headers.get("Authorization").map(String::as_str),
                    Some("Bearer token")
                );
            }
            _ => panic!("unexpected config variant"),
        }
    }

    #[test]
    fn locations_from_tool_io_extracts_file_path() {
        let locations = locations_from_tool_io("read", r#"{"path":"src/main.rs"}"#);
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].path, PathBuf::from("src/main.rs"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn initialize_reports_capabilities() {
        let tmp = TempDir::new().expect("tmp dir");
        let runtime = build_runtime(&tmp).await;
        let response = acp::Agent::initialize(
            &runtime,
            acp::InitializeRequest::new(acp::ProtocolVersion::LATEST),
        )
        .await
        .expect("initialize");

        assert_eq!(response.protocol_version, acp::ProtocolVersion::LATEST);
        assert!(response.agent_capabilities.load_session);
        assert!(response
            .agent_capabilities
            .session_capabilities
            .list
            .is_some());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn session_lifecycle_methods_update_state() {
        let tmp = TempDir::new().expect("tmp dir");
        let runtime = build_runtime(&tmp).await;
        let cwd = tmp.path().to_path_buf();

        let new_response =
            acp::Agent::new_session(&runtime, acp::NewSessionRequest::new(cwd.clone()))
                .await
                .expect("new session");
        let session_id = new_response.session_id.0.to_string();
        assert!(new_response.models.is_some());
        assert!(new_response.modes.is_some());
        assert!(new_response.config_options.is_some());

        runtime
            .config
            .store
            .append_message(Message {
                id: Uuid::new_v4().to_string(),
                session_id: session_id.clone(),
                role: MessageRole::User,
                content: "hello".to_string(),
                summary: false,
                tool_call_id: None,
                tool_calls: None,
                created_at: Utc::now().timestamp(),
            })
            .await
            .expect("append message");

        let loaded = acp::Agent::load_session(
            &runtime,
            acp::LoadSessionRequest::new(session_id.clone(), cwd.clone()),
        )
        .await
        .expect("load session");
        assert!(loaded.models.is_some());
        assert!(loaded.modes.is_some());

        acp::Agent::set_session_model(
            &runtime,
            acp::SetSessionModelRequest::new(session_id.clone(), "anthropic/claude-3-7-sonnet"),
        )
        .await
        .expect("set model");
        let state = runtime
            .session_state(&session_id)
            .await
            .expect("session state exists");
        assert_eq!(state.provider_id, "anthropic");
        assert_eq!(state.model, "claude-3-7-sonnet");

        acp::Agent::set_session_mode(
            &runtime,
            acp::SetSessionModeRequest::new(session_id.clone(), "review"),
        )
        .await
        .expect("set mode");
        let state = runtime
            .session_state(&session_id)
            .await
            .expect("session state exists");
        assert_eq!(state.mode_id.as_deref(), Some("review"));

        acp::Agent::set_session_config_option(
            &runtime,
            acp::SetSessionConfigOptionRequest::new(
                session_id.clone(),
                CONFIG_ID_MODEL,
                "openai/gpt-4.1-mini",
            ),
        )
        .await
        .expect("set model config");
        let state = runtime
            .session_state(&session_id)
            .await
            .expect("session state exists");
        assert_eq!(state.provider_id, "openai");
        assert_eq!(state.model, "gpt-4.1-mini");

        let listed =
            acp::Agent::list_sessions(&runtime, acp::ListSessionsRequest::new().cwd(cwd.clone()))
                .await
                .expect("list sessions");
        assert!(listed
            .sessions
            .iter()
            .any(|s| s.session_id.0.as_ref() == session_id));

        let forked = acp::Agent::fork_session(
            &runtime,
            acp::ForkSessionRequest::new(session_id.clone(), cwd.clone()),
        )
        .await
        .expect("fork session");
        assert_ne!(forked.session_id.0.as_ref(), session_id);

        let resumed =
            acp::Agent::resume_session(&runtime, acp::ResumeSessionRequest::new(session_id, cwd))
                .await
                .expect("resume session");
        assert!(resumed.models.is_some());
        assert!(resumed.modes.is_some());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancel_aborts_inflight_prompt_handle() {
        let tmp = TempDir::new().expect("tmp dir");
        let runtime = build_runtime(&tmp).await;
        let session_id = "cancel-session".to_string();
        runtime
            .upsert_session(SessionState::new(
                session_id.clone(),
                tmp.path().to_path_buf(),
                "openai",
                "gpt-4o-mini",
                None,
                Vec::new(),
            ))
            .await;

        let sleeper = tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        });
        runtime
            .set_prompt_abort(&session_id, Some(sleeper.abort_handle()))
            .await
            .expect("set abort handle");

        acp::Agent::cancel(&runtime, acp::CancelNotification::new(session_id))
            .await
            .expect("cancel request");
        let join = sleeper.await;
        assert!(matches!(join, Err(err) if err.is_cancelled()));
    }
}
