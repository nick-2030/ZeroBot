use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub bind_addr: String,
    pub data_dir: String,
    pub api_key: String,
    pub allow_bash: bool,
    pub allow_write: bool,
    pub allow_delete: bool,
    pub allow_edit: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:9080".to_string(),
            data_dir: "./data".to_string(),
            api_key: "dev-key".to_string(),
            allow_bash: false,
            allow_write: true,
            allow_delete: false,
            allow_edit: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientConfig {
    pub server_url: String,
    pub api_key: String,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            server_url: "http://127.0.0.1:9080".to_string(),
            api_key: "dev-key".to_string(),
        }
    }
}
