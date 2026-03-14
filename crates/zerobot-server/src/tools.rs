use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use regex::Regex;
use serde_json::Value;

use zerobot_core::{builtin_tools, ToolDefinition, ToolPolicy, ToolResult};

pub struct ToolRegistry {
    tools: Vec<ToolDefinition>,
    policy: ToolPolicy,
    root: PathBuf,
    data_dir: PathBuf,
}

impl ToolRegistry {
    pub fn new(config: &zerobot_core::ServerConfig, data_dir: PathBuf) -> anyhow::Result<Arc<Self>> {
        let mut tools = builtin_tools();
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Arc::new(Self {
            tools,
            policy: ToolPolicy {
                allow_bash: config.allow_bash,
                allow_write: config.allow_write,
                allow_edit: config.allow_edit,
                allow_delete: config.allow_delete,
            },
            root: std::env::current_dir()?,
            data_dir,
        }))
    }

    pub fn list(&self) -> &[ToolDefinition] {
        &self.tools
    }

    pub fn execute(&self, name: &str, args: &Value) -> ToolResult {
        if !self.policy.is_allowed(name) {
            return ToolResult {
                name: name.to_string(),
                output: serde_json::json!({"error":"tool not allowed"}),
                is_error: true,
            };
        }
        match name {
            "read" => self.read_file(args),
            "write" => self.write_file(args),
            "edit" => self.edit_file(args),
            "glob" => self.glob_files(args),
            "find" => self.find_files(args),
            "grep" => self.grep_files(args),
            "delete" => self.delete_file(args),
            "bash" => self.bash(args),
            "todo_write" => self.todo_write(args),
            "todo_update" => self.todo_write(args),
            _ => ToolResult {
                name: name.to_string(),
                output: serde_json::json!({"error":"unknown tool"}),
                is_error: true,
            },
        }
    }

    fn safe_path(&self, path: &str) -> anyhow::Result<PathBuf> {
        let raw = Path::new(path);
        if raw.is_absolute() {
            anyhow::bail!("absolute paths not allowed");
        }
        if raw.components().any(|c| matches!(c, Component::ParentDir)) {
            anyhow::bail!("parent paths not allowed");
        }
        Ok(self.root.join(raw))
    }

    fn read_file(&self, args: &Value) -> ToolResult {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
        match self.safe_path(path).and_then(|p| std::fs::read_to_string(p).map_err(|e| e.into())) {
            Ok(content) => ToolResult {
                name: "read".to_string(),
                output: serde_json::json!({"content": content}),
                is_error: false,
            },
            Err(err) => ToolResult {
                name: "read".to_string(),
                output: serde_json::json!({"error": err.to_string()}),
                is_error: true,
            },
        }
    }

    fn write_file(&self, args: &Value) -> ToolResult {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
        match self.safe_path(path).and_then(|p| std::fs::write(p, content).map_err(|e| e.into())) {
            Ok(_) => ToolResult {
                name: "write".to_string(),
                output: serde_json::json!({"ok": true}),
                is_error: false,
            },
            Err(err) => ToolResult {
                name: "write".to_string(),
                output: serde_json::json!({"error": err.to_string()}),
                is_error: true,
            },
        }
    }

    fn edit_file(&self, args: &Value) -> ToolResult {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let old = args.get("old").and_then(|v| v.as_str()).unwrap_or("");
        let new = args.get("new").and_then(|v| v.as_str()).unwrap_or("");
        let result = self.safe_path(path).and_then(|p| {
            let content = std::fs::read_to_string(&p)?;
            let updated = content.replace(old, new);
            std::fs::write(&p, updated)?;
            Ok(())
        });
        match result {
            Ok(_) => ToolResult {
                name: "edit".to_string(),
                output: serde_json::json!({"ok": true}),
                is_error: false,
            },
            Err(err) => ToolResult {
                name: "edit".to_string(),
                output: serde_json::json!({"error": err.to_string()}),
                is_error: true,
            },
        }
    }

    fn glob_files(&self, args: &Value) -> ToolResult {
        let pattern = args.get("pattern").and_then(|v| v.as_str()).unwrap_or("**/*");
        let mut results = Vec::new();
        if let Ok(paths) = glob::glob(&self.root.join(pattern).to_string_lossy()) {
            for entry in paths.flatten() {
                if let Some(rel) = entry.strip_prefix(&self.root).ok() {
                    results.push(rel.to_string_lossy().to_string());
                }
            }
        }
        ToolResult {
            name: "glob".to_string(),
            output: serde_json::json!({"paths": results}),
            is_error: false,
        }
    }

    fn find_files(&self, args: &Value) -> ToolResult {
        let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let mut results = Vec::new();
        for entry in walkdir::WalkDir::new(&self.root) {
            if let Ok(entry) = entry {
                if entry.file_name().to_string_lossy() == name {
                    if let Some(rel) = entry.path().strip_prefix(&self.root).ok() {
                        results.push(rel.to_string_lossy().to_string());
                    }
                }
            }
        }
        ToolResult {
            name: "find".to_string(),
            output: serde_json::json!({"paths": results}),
            is_error: false,
        }
    }

    fn grep_files(&self, args: &Value) -> ToolResult {
        let pattern = args.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
        let glob_pattern = args.get("glob").and_then(|v| v.as_str());
        let regex = Regex::new(pattern).ok();
        let mut matches = Vec::new();
        let paths: Vec<PathBuf> = if let Some(glob_pattern) = glob_pattern {
            glob::glob(&self.root.join(glob_pattern).to_string_lossy())
                .ok()
                .into_iter()
                .flat_map(|paths| paths.flatten())
                .collect()
        } else {
            walkdir::WalkDir::new(&self.root)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|e| e.path().is_file())
                .map(|e| e.path().to_path_buf())
                .collect()
        };
        if let Some(regex) = regex {
            for path in paths {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    for (idx, line) in content.lines().enumerate() {
                        if regex.is_match(line) {
                            if let Some(rel) = path.strip_prefix(&self.root).ok() {
                                matches.push(serde_json::json!({
                                    "path": rel.to_string_lossy(),
                                    "line": idx + 1,
                                    "text": line,
                                }));
                            }
                        }
                    }
                }
            }
        }
        ToolResult {
            name: "grep".to_string(),
            output: serde_json::json!({"matches": matches}),
            is_error: false,
        }
    }

    fn delete_file(&self, args: &Value) -> ToolResult {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
        match self.safe_path(path).and_then(|p| std::fs::remove_file(p).map_err(|e| e.into())) {
            Ok(_) => ToolResult {
                name: "delete".to_string(),
                output: serde_json::json!({"ok": true}),
                is_error: false,
            },
            Err(err) => ToolResult {
                name: "delete".to_string(),
                output: serde_json::json!({"error": err.to_string()}),
                is_error: true,
            },
        }
    }

    fn bash(&self, args: &Value) -> ToolResult {
        let cmd = args.get("cmd").and_then(|v| v.as_str()).unwrap_or("");
        let result = std::process::Command::new("sh")
            .arg("-lc")
            .arg(cmd)
            .current_dir(&self.root)
            .output();
        match result {
            Ok(output) => ToolResult {
                name: "bash".to_string(),
                output: serde_json::json!({
                    "status": output.status.code().unwrap_or(-1),
                    "stdout": String::from_utf8_lossy(&output.stdout),
                    "stderr": String::from_utf8_lossy(&output.stderr),
                }),
                is_error: !output.status.success(),
            },
            Err(err) => ToolResult {
                name: "bash".to_string(),
                output: serde_json::json!({"error": err.to_string()}),
                is_error: true,
            },
        }
    }

    fn todo_write(&self, args: &Value) -> ToolResult {
        let items = args.get("items").cloned().unwrap_or_else(|| serde_json::json!([]));
        let path = self.data_dir.join("todos.json");
        let result = std::fs::write(&path, serde_json::to_string_pretty(&items).unwrap_or_default());
        match result {
            Ok(_) => ToolResult {
                name: "todo".to_string(),
                output: serde_json::json!({"path": path.to_string_lossy()}),
                is_error: false,
            },
            Err(err) => ToolResult {
                name: "todo".to_string(),
                output: serde_json::json!({"error": err.to_string()}),
                is_error: true,
            },
        }
    }
}
