use std::path::PathBuf;
use std::sync::Mutex;

use zerobot_core::ToolDefinition;

#[derive(Default)]
pub struct McpRegistry {
    tools: Mutex<Vec<ToolDefinition>>,
}

impl McpRegistry {
    pub fn load_from_file(&self, path: PathBuf) -> anyhow::Result<()> {
        if !path.exists() {
            return Ok(());
        }
        let content = std::fs::read_to_string(path)?;
        let tools: Vec<ToolDefinition> = serde_yaml::from_str(&content).unwrap_or_default();
        let mut guard = self.tools.lock().unwrap();
        *guard = tools;
        Ok(())
    }

    pub fn list(&self) -> Vec<ToolDefinition> {
        self.tools.lock().unwrap().clone()
    }
}
