use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::{routing::get, Router};
use tower_http::cors::{Any, CorsLayer};
use tracing_subscriber::EnvFilter;

mod api;
mod agent;
mod context;
mod events;
mod mcp;
mod plugins;
mod state;
mod storage;
mod tasks;
mod tools;

use crate::state::AppState;
use zerobot_core::ServerConfig;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let config = load_config();
    let data_dir = PathBuf::from(&config.data_dir);
    std::fs::create_dir_all(&data_dir)?;

    let store = storage::SqliteStore::new(data_dir.join("zerobot.db"))?;
    store.init()?;

    let tool_registry = tools::ToolRegistry::new(&config, data_dir.clone())?;

    let app_state = Arc::new(AppState {
        config: config.clone(),
        data_dir: data_dir.clone(),
        store,
        tools: tool_registry,
        events: events::EventBus::default().into(),
        mcp: mcp::McpRegistry::default().into(),
        plugins: plugins::PluginRegistry::default().into(),
        tasks: tasks::TaskScheduler::default().into(),
    });

    app_state.plugins.load_from_dir(app_state.data_dir.join("plugins"))?;
    app_state.mcp.load_from_file(app_state.data_dir.join("mcp.yaml"))?;

    let router = Router::new()
        .route("/health", get(api::health))
        .nest("/v1", api::router())
        .layer(CorsLayer::new().allow_origin(Any).allow_methods(Any))
        .with_state(app_state);

    let addr: SocketAddr = config.bind_addr.parse()?;
    tracing::info!("zerobot server listening on {}", addr);
    axum::Server::bind(&addr)
        .serve(router.into_make_service())
        .await?;
    Ok(())
}

fn load_config() -> ServerConfig {
    let path = std::env::var("ZEROBOT_CONFIG").ok();
    if let Some(path) = path {
        if let Ok(contents) = std::fs::read_to_string(path) {
            if let Ok(cfg) = serde_yaml::from_str::<ServerConfig>(&contents) {
                return cfg;
            }
        }
    }
    ServerConfig::default()
}
