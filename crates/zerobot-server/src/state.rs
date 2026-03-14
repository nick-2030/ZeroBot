use std::path::PathBuf;
use std::sync::Arc;

use zerobot_core::ServerConfig;

use crate::events::EventBus;
use crate::mcp::McpRegistry;
use crate::plugins::PluginRegistry;
use crate::storage::SqliteStore;
use crate::tasks::TaskScheduler;
use crate::tools::ToolRegistry;

#[derive(Clone)]
pub struct AppState {
    pub config: ServerConfig,
    pub data_dir: PathBuf,
    pub store: Arc<SqliteStore>,
    pub tools: Arc<ToolRegistry>,
    pub settings: Arc<zerobot_core::SettingsBundle>,
    pub events: Arc<EventBus>,
    pub mcp: Arc<McpRegistry>,
    pub plugins: Arc<PluginRegistry>,
    pub tasks: Arc<TaskScheduler>,
}
