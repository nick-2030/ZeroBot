use std::collections::{HashMap, HashSet};

use crate::error::{ZeroBotError, ZeroBotResult};

/// 工具集定义
#[derive(Debug, Clone)]
pub struct ToolsetDefinition {
    pub name: String,
    pub description: String,
    pub tools: Vec<String>,
    pub includes: Vec<String>,
}

/// 工具集注册表
pub struct ToolsetRegistry {
    toolsets: HashMap<String, ToolsetDefinition>,
}

impl ToolsetRegistry {
    pub fn new() -> Self {
        Self {
            toolsets: HashMap::new(),
        }
    }

    /// 注册工具集
    pub fn register(&mut self, def: ToolsetDefinition) {
        self.toolsets.insert(def.name.clone(), def);
    }

    /// 解析工具集，展开 includes 递归，返回去重后的工具名列表
    pub fn resolve(&self, name: &str) -> ZeroBotResult<Vec<String>> {
        let mut result = HashSet::new();
        let mut visited = HashSet::new();
        self.resolve_recursive(name, &mut result, &mut visited)?;
        Ok(result.into_iter().collect())
    }

    /// 解析多个工具集，合并去重
    pub fn resolve_many(&self, names: &[String]) -> ZeroBotResult<Vec<String>> {
        let mut result = HashSet::new();
        let mut visited = HashSet::new();
        for name in names {
            self.resolve_recursive(name, &mut result, &mut visited)?;
        }
        Ok(result.into_iter().collect())
    }

    fn resolve_recursive(
        &self,
        name: &str,
        result: &mut HashSet<String>,
        visited: &mut HashSet<String>,
    ) -> ZeroBotResult<()> {
        if visited.contains(name) {
            return Ok(()); // 防止循环引用
        }
        visited.insert(name.to_string());

        let def = self.toolsets.get(name).ok_or_else(|| {
            ZeroBotError::Config(format!("未知工具集: {}", name))
        })?;

        // 先解析 includes
        for include in &def.includes {
            self.resolve_recursive(include, result, visited)?;
        }

        // 再添加自己的工具
        for tool in &def.tools {
            result.insert(tool.clone());
        }

        Ok(())
    }

    /// 列出所有已注册的工具集
    pub fn list(&self) -> Vec<&ToolsetDefinition> {
        self.toolsets.values().collect()
    }
}

impl Default for ToolsetRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// 创建内置工具集注册表
pub fn builtin_toolsets() -> ToolsetRegistry {
    let mut registry = ToolsetRegistry::new();

    registry.register(ToolsetDefinition {
        name: "filesystem".to_string(),
        description: "文件读写操作".to_string(),
        tools: vec![
            "read".to_string(),
            "write".to_string(),
            "edit".to_string(),
            "apply_patch".to_string(),
            "patch".to_string(),
            "glob".to_string(),
            "grep".to_string(),
        ],
        includes: vec![],
    });

    registry.register(ToolsetDefinition {
        name: "shell".to_string(),
        description: "Shell 命令执行".to_string(),
        tools: vec!["bash".to_string(), "shell".to_string()],
        includes: vec![],
    });

    registry.register(ToolsetDefinition {
        name: "code".to_string(),
        description: "代码分析和修改".to_string(),
        tools: vec!["bash".to_string()],
        includes: vec!["filesystem".to_string()],
    });

    registry.register(ToolsetDefinition {
        name: "web".to_string(),
        description: "网络搜索和抓取".to_string(),
        tools: vec!["web_search".to_string(), "web_fetch".to_string()],
        includes: vec![],
    });

    registry.register(ToolsetDefinition {
        name: "task".to_string(),
        description: "任务管理".to_string(),
        tools: vec!["todo_read".to_string(), "todo_write".to_string()],
        includes: vec![],
    });

    registry.register(ToolsetDefinition {
        name: "agent".to_string(),
        description: "多智能体调度".to_string(),
        tools: vec!["agent".to_string(), "send_message".to_string()],
        includes: vec![],
    });

    registry.register(ToolsetDefinition {
        name: "kanban".to_string(),
        description: "看板任务协调".to_string(),
        tools: vec![
            "kanban_create".to_string(),
            "kanban_show".to_string(),
            "kanban_complete".to_string(),
            "kanban_block".to_string(),
            "kanban_comment".to_string(),
        ],
        includes: vec![],
    });

    registry.register(ToolsetDefinition {
        name: "swarm".to_string(),
        description: "团队协作".to_string(),
        tools: vec![
            "spawn_teammate".to_string(),
            "send_teammate_message".to_string(),
            "list_teammates".to_string(),
        ],
        includes: vec![],
    });

    registry
}
