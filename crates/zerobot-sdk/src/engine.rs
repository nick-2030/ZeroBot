use crate::abort::AbortHandle;
use crate::message::{self, SDKMessage};
use crate::result::QueryResult;
use futures::stream::{self, Stream};
use std::collections::HashSet;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, RwLock};
use zerobot_core::agent::Agent;
use zerobot_core::config::Settings;
use zerobot_core::events::AgentEvent;
use zerobot_core::hooks::HookManager;
use zerobot_core::plugin::PluginManager;
use zerobot_core::session::SessionStore;
use zerobot_core::tool::{SubagentTool, ToolRegistry};
use zerobot_core::ZeroBotError;

/// The query engine owns the provider factory, tool registry, and settings.
/// It can execute multiple queries against different sessions without
/// re-initializing everything from scratch each time.
pub(crate) struct QueryEngine {
    pub(crate) settings: Settings,
    pub(crate) store: Arc<dyn SessionStore>,
    pub(crate) tools: ToolRegistry,
    pub(crate) hooks: HookManager,
    pub(crate) cwd: PathBuf,
    pub(crate) model: String,
    pub(crate) plugins: Option<Arc<PluginManager>>,
    pub(crate) tool_approvals: Arc<RwLock<HashSet<String>>>,
    pub(crate) max_turns: Option<usize>,
    pub(crate) max_budget_usd: Option<f64>,
}

impl QueryEngine {
    /// Build a provider and register subagent tools, returning a ready Agent.
    /// This is the shared setup that was previously duplicated in `run()` and `run_stream()`.
    pub(crate) fn build_agent(&self) -> Result<Agent, ZeroBotError> {
        let provider_factory = {
            let settings = self.settings.clone();
            Arc::new(move || {
                crate::helpers::build_provider(&settings, None)
                    .map_err(|err| ZeroBotError::Provider(err.to_string()))
            })
        };
        let provider = (provider_factory)()?;

        let mut tools = self.tools.clone();
        let subagent_tools = tools.clone();
        tools.register(SubagentTool::new(
            self.settings.clone(),
            self.store.clone(),
            subagent_tools,
            self.cwd.clone(),
            provider_factory.clone(),
            self.model.clone(),
            self.hooks.clone(),
            None,
            self.tool_approvals.clone(),
        ));

        Ok(Agent::new(
            provider,
            self.model.clone(),
            self.settings.clone(),
            self.store.clone(),
            tools,
            self.cwd.clone(),
            self.hooks.clone(),
            None,
            self.plugins.clone(),
            self.tool_approvals.clone(),
            None,
            None,
        ))
    }

    /// Execute a query and return the structured result.
    pub async fn query(
        &self,
        session_id: &str,
        input: &str,
        abort: Option<&AbortHandle>,
    ) -> crate::SdkResult<QueryResult> {
        let start = Instant::now();

        if let Some(handle) = abort {
            if handle.is_aborted() {
                return Ok(QueryResult {
                    response: String::new(),
                    session_id: session_id.to_string(),
                    turns: 0,
                    duration_ms: 0,
                    usage: Default::default(),
                    cost_usd: None,
                    is_error: true,
                    error: Some("aborted before execution".to_string()),
                });
            }
        }

        let agent = self.build_agent()?;
        let result = agent.run_turn(session_id, input, None).await;
        let duration_ms = start.elapsed().as_millis() as u64;

        match result {
            Ok(response) => Ok(QueryResult {
                response,
                session_id: session_id.to_string(),
                turns: 1,
                duration_ms,
                usage: Default::default(),
                cost_usd: None,
                is_error: false,
                error: None,
            }),
            Err(err) => Ok(QueryResult {
                response: String::new(),
                session_id: session_id.to_string(),
                turns: 1,
                duration_ms,
                usage: Default::default(),
                cost_usd: None,
                is_error: true,
                error: Some(err.to_string()),
            }),
        }
    }

    /// Execute a query and return a stream of typed `SDKMessage`s.
    pub fn query_stream(
        &self,
        session_id: &str,
        input: &str,
        abort: Option<AbortHandle>,
    ) -> crate::SdkResult<Pin<Box<dyn Stream<Item = SDKMessage> + Send>>> {
        let agent = self.build_agent()?;
        let session_id = session_id.to_string();
        let input = input.to_string();

        let (tx, rx) = mpsc::unbounded_channel::<AgentEvent>();
        let start = Instant::now();

        let sid = session_id.clone();
        tokio::spawn(async move {
            let _ = agent.run_turn(&session_id, &input, Some(tx)).await;
        });
        let abort_clone = abort.clone();
        let mapped = stream::unfold(
            (rx, sid, start, abort_clone, false),
            |(mut rx, sid, start, abort, mut saw_done)| async move {
                loop {
                    if let Some(ref handle) = abort {
                        if handle.is_aborted() {
                            let msg =
                                SDKMessage::Result(message::ResultMessage::Error(
                                    message::ErrorResult {
                                        session_id: sid.clone(),
                                        uuid: uuid::Uuid::new_v4().to_string(),
                                        error: "aborted".to_string(),
                                        duration_ms: start.elapsed().as_millis()
                                            as u64,
                                        turns: 0,
                                        is_error: true,
                                    },
                                ));
                            return Some((msg, (rx, sid, start, abort, true)));
                        }
                    }

                    match rx.recv().await {
                        Some(event) => {
                            if matches!(event, AgentEvent::Done) {
                                saw_done = true;
                            }
                            if let Some(msg) =
                                SDKMessage::from_agent_event(&event, &sid)
                            {
                                return Some((
                                    msg,
                                    (rx, sid, start, abort, saw_done),
                                ));
                            }
                            continue;
                        }
                        None => {
                            if !saw_done {
                                let msg = SDKMessage::Result(
                                    message::ResultMessage::Success(
                                        message::SuccessResult {
                                            session_id: sid.clone(),
                                            uuid: uuid::Uuid::new_v4()
                                                .to_string(),
                                            result: String::new(),
                                            duration_ms: start
                                                .elapsed()
                                                .as_millis()
                                                as u64,
                                            total_cost_usd: None,
                                            usage: Default::default(),
                                            turns: 0,
                                            is_error: false,
                                        },
                                    ),
                                );
                                return Some((
                                    msg,
                                    (rx, sid, start, abort, true),
                                ));
                            }
                            return None;
                        }
                    }
                }
            },
        );

        Ok(Box::pin(mapped))
    }
}
