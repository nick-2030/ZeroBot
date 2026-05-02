use std::path::PathBuf;
use crate::error::{ZeroBotError, ZeroBotResult};

/// 文件系统邮箱 IPC
pub struct Mailbox {
    dir: PathBuf,
}

impl Mailbox {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn mailbox_path(&self, agent_name: &str, team_name: &str) -> PathBuf {
        self.dir.join(format!("{}_{}.jsonl", team_name, agent_name))
    }

    /// 发送消息到 teammate 的邮箱
    pub fn send(&self, agent_name: &str, team_name: &str, message: &str) -> ZeroBotResult<()> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| ZeroBotError::Swarm(format!("创建邮箱目录失败: {}", e)))?;

        let path = self.mailbox_path(agent_name, team_name);
        let entry = serde_json::json!({
            "message": message,
            "timestamp": chrono::Utc::now().to_rfc3339(),
        });

        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| ZeroBotError::Swarm(format!("打开邮箱文件失败: {}", e)))?;

        writeln!(file, "{}", entry)
            .map_err(|e| ZeroBotError::Swarm(format!("写入邮箱失败: {}", e)))?;

        Ok(())
    }

    /// 读取并清空自己的邮箱
    pub fn drain(&self, agent_name: &str, team_name: &str) -> ZeroBotResult<Vec<String>> {
        let path = self.mailbox_path(agent_name, team_name);
        if !path.exists() {
            return Ok(Vec::new());
        }

        let content = std::fs::read_to_string(&path)
            .map_err(|e| ZeroBotError::Swarm(format!("读取邮箱失败: {}", e)))?;

        let messages: Vec<String> = content
            .lines()
            .filter_map(|line| {
                serde_json::from_str::<serde_json::Value>(line)
                    .ok()
                    .and_then(|v| v["message"].as_str().map(|s| s.to_string()))
            })
            .collect();

        // 清空邮箱
        std::fs::remove_file(&path)
            .map_err(|e| ZeroBotError::Swarm(format!("清空邮箱失败: {}", e)))?;

        Ok(messages)
    }
}
