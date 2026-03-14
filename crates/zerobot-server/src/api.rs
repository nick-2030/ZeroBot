use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::sse::{Event, KeepAlive, Sse},
    Json, Router,
};
use tokio_stream::{wrappers::BroadcastStream, StreamExt};
use uuid::Uuid;

use zerobot_core::{ContextSummary, Role, SessionId, Task, TaskStatus, ToolDefinition, SettingsBundle};

use crate::{agent::Supervisor, context::compress_messages, events::ServerEvent, llm, state::AppState};

#[derive(serde::Deserialize)]
struct CreateSessionRequest {
    title: Option<String>,
}

#[derive(serde::Serialize)]
struct CreateSessionResponse {
    session_id: String,
}

#[derive(serde::Deserialize)]
struct MessageRequest {
    content: String,
}

#[derive(serde::Deserialize)]
struct ForkRequest {
    title: Option<String>,
}

#[derive(serde::Deserialize)]
struct RollbackRequest {
    message_id: String,
}

#[derive(serde::Deserialize)]
struct LoadFileRequest {
    path: String,
}

#[derive(serde::Deserialize)]
struct CreateTaskRequest {
    name: String,
    cron: Option<String>,
    payload: serde_json::Value,
}

#[derive(serde::Serialize)]
struct PermissionResponse {
    allow_bash: bool,
    allow_write: bool,
    allow_edit: bool,
    allow_delete: bool,
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/sessions", axum::routing::get(list_sessions).post(create_session))
        .route("/sessions/:id", axum::routing::get(get_session))
        .route("/sessions/:id/messages", axum::routing::post(send_message))
        .route("/sessions/:id/events", axum::routing::get(stream_events))
        .route("/sessions/:id/fork", axum::routing::post(fork_session))
        .route("/sessions/:id/rollback", axum::routing::post(rollback_session))
        .route("/sessions/:id/context/compress", axum::routing::post(compress_context))
        .route("/sessions/:id/context/load_file", axum::routing::post(load_context_file))
        .route("/tools", axum::routing::get(list_tools))
        .route("/plugins", axum::routing::get(list_plugins))
        .route("/plugins/reload", axum::routing::post(reload_plugins))
        .route("/mcp/tools", axum::routing::get(list_mcp_tools))
        .route("/tasks", axum::routing::post(create_task))
        .route("/tasks/:id", axum::routing::get(get_task))
        .route("/permissions", axum::routing::get(get_permissions))
        .route("/settings", axum::routing::get(get_settings))
        .route("/llm/test", axum::routing::post(test_llm))
        .route("/openapi.json", axum::routing::get(openapi))
}

pub async fn health() -> &'static str {
    "ok"
}

async fn create_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<CreateSessionRequest>,
) -> Result<Json<CreateSessionResponse>, StatusCode> {
    require_api_key(&state, &headers)?;
    let title = payload.title.unwrap_or_else(|| "New Session".to_string());
    let session = state.store.create_session(title, None).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(CreateSessionResponse {
        session_id: session.id.0.to_string(),
    }))
}

async fn list_sessions(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<zerobot_core::Session>>, StatusCode> {
    require_api_key(&state, &headers)?;
    let sessions = state.store.list_sessions().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(sessions))
}

async fn get_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<zerobot_core::SessionState>, StatusCode> {
    require_api_key(&state, &headers)?;
    let session_id = parse_session_id(&id)?;
    let state_data = state.store.get_state(&session_id).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    state_data.map(Json).ok_or(StatusCode::NOT_FOUND)
}

async fn send_message(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<MessageRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    require_api_key(&state, &headers)?;
    let session_id = parse_session_id(&id)?;
    let _ = state
        .store
        .add_message(&session_id, Role::User, payload.content.clone())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let supervisor = Supervisor::new(state.tools.clone());
    let history = state
        .store
        .list_messages(&session_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let outputs = supervisor
        .handle_user_message(&state.settings.active, &history, &payload.content)
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    for output in outputs {
        match output {
            crate::agent::AgentOutput::Assistant(text) => {
                let msg = state
                    .store
                    .add_message(&session_id, Role::Assistant, text.clone())
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                state
                    .events
                    .publish(&session_id, ServerEvent {
                        event_type: "token".to_string(),
                        data: serde_json::json!({"content": msg.content}),
                    })
                    .await;
            }
            crate::agent::AgentOutput::Tool(result) => {
                state
                    .store
                    .add_message(&session_id, Role::Tool, result.output.to_string())
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                state
                    .events
                    .publish(&session_id, ServerEvent {
                        event_type: "tool_result".to_string(),
                        data: serde_json::json!({"name": result.name, "output": result.output}),
                    })
                    .await;
            }
        }
    }

    Ok(Json(serde_json::json!({"ok": true})))
}

async fn stream_events(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Sse<impl tokio_stream::Stream<Item = Result<Event, axum::Error>>>, StatusCode> {
    require_api_key(&state, &headers)?;
    let session_id = parse_session_id(&id)?;
    let receiver = state.events.subscribe(&session_id).await;
    let stream = BroadcastStream::new(receiver).filter_map(|event| match event {
        Ok(ev) => Some(Ok(Event::default().event(ev.event_type).data(ev.data.to_string()))),
        Err(_) => None,
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

async fn fork_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<ForkRequest>,
) -> Result<Json<CreateSessionResponse>, StatusCode> {
    require_api_key(&state, &headers)?;
    let session_id = parse_session_id(&id)?;
    let title = payload.title.unwrap_or_else(|| "Forked Session".to_string());
    let session = state.store.fork_session(&session_id, title).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(CreateSessionResponse { session_id: session.id.0.to_string() }))
}

async fn rollback_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<RollbackRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    require_api_key(&state, &headers)?;
    let session_id = parse_session_id(&id)?;
    let message_id = Uuid::parse_str(&payload.message_id).map_err(|_| StatusCode::BAD_REQUEST)?;
    state.store.rollback_to_message(&session_id, message_id).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({"ok": true})))
}

async fn compress_context(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<ContextSummary>, StatusCode> {
    require_api_key(&state, &headers)?;
    let session_id = parse_session_id(&id)?;
    let messages = state.store.list_messages(&session_id).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let contents: Vec<String> = messages.into_iter().map(|m| m.content).collect();
    let summary = compress_messages(&contents);
    let summary_json = serde_json::to_value(&summary).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    state.store.upsert_summary(&session_id, &summary_json).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(summary))
}

async fn load_context_file(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<LoadFileRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    require_api_key(&state, &headers)?;
    let session_id = parse_session_id(&id)?;
    let result = state.tools.execute("read", &serde_json::json!({"path": payload.path}));
    if result.is_error {
        return Err(StatusCode::BAD_REQUEST);
    }
    let content = result.output.get("content").and_then(|v| v.as_str()).unwrap_or("");
    state
        .store
        .add_message(&session_id, Role::System, format!("[FILE]\n{}", content))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({"ok": true})))
}

async fn list_tools(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<ToolDefinition>>, StatusCode> {
    require_api_key(&state, &headers)?;
    let mut tools = state.tools.list().to_vec();
    tools.extend(state.mcp.list());
    for plugin in state.plugins.list() {
        for tool in plugin.tools {
            tools.push(ToolDefinition {
                name: tool.name,
                description: tool.description,
                input_schema: tool.input_schema,
            });
        }
    }
    Ok(Json(tools))
}

async fn list_plugins(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<zerobot_core::PluginManifest>>, StatusCode> {
    require_api_key(&state, &headers)?;
    Ok(Json(state.plugins.list()))
}

async fn reload_plugins(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    require_api_key(&state, &headers)?;
    state.plugins.load_from_dir(state.data_dir.join("plugins")).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({"ok": true})))
}

async fn list_mcp_tools(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<ToolDefinition>>, StatusCode> {
    require_api_key(&state, &headers)?;
    Ok(Json(state.mcp.list()))
}

async fn create_task(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<CreateTaskRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    require_api_key(&state, &headers)?;
    let task = Task {
        id: Uuid::new_v4(),
        session_id: None,
        name: payload.name,
        cron: payload.cron,
        status: TaskStatus::Pending,
        payload: payload.payload,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    state.store.create_task(task).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({"ok": true})))
}

async fn get_task(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Task>, StatusCode> {
    require_api_key(&state, &headers)?;
    let task_id = Uuid::parse_str(&id).map_err(|_| StatusCode::BAD_REQUEST)?;
    let task = state.store.get_task(task_id).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    task.map(Json).ok_or(StatusCode::NOT_FOUND)
}

async fn get_permissions(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<PermissionResponse>, StatusCode> {
    require_api_key(&state, &headers)?;
    Ok(Json(PermissionResponse {
        allow_bash: state.config.allow_bash,
        allow_write: state.config.allow_write,
        allow_edit: state.config.allow_edit,
        allow_delete: state.config.allow_delete,
    }))
}

async fn get_settings(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<SettingsBundle>, StatusCode> {
    require_api_key(&state, &headers)?;
    Ok(Json(state.settings.as_ref().clone()))
}

async fn test_llm(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<llm::LlmTestRequest>,
) -> Result<Json<llm::LlmTestResponse>, StatusCode> {
    require_api_key(&state, &headers)?;
    let result = llm::test_llm(&state.settings.active, payload)
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(Json(result))
}

async fn openapi(
    State(_state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let _ = headers;
    Ok(Json(serde_json::json!({"openapi":"3.0.0","info":{"title":"zerobot","version":"0.1.0"}})))
}

fn parse_session_id(id: &str) -> Result<SessionId, StatusCode> {
    let uuid = Uuid::parse_str(id).map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(SessionId(uuid))
}

fn require_api_key(state: &AppState, headers: &HeaderMap) -> Result<(), StatusCode> {
    let api_key = headers
        .get("x-zerobot-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if api_key == state.config.api_key {
        Ok(())
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}
