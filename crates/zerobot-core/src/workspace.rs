use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const WORKSPACE_MARKER: &str = ".zerobot";

pub fn resolve_workspace_root(cwd: &Path) -> PathBuf {
    let mut current = cwd.to_path_buf();
    loop {
        if current.join(WORKSPACE_MARKER).exists() {
            return current;
        }
        if current.join(".git").exists() {
            return current;
        }
        let parent = match current.parent() {
            Some(p) => p.to_path_buf(),
            None => break,
        };
        if parent == current {
            break;
        }
        current = parent;
    }
    cwd.to_path_buf()
}

pub fn workspace_key(root: &Path) -> String {
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let bytes = canonical.to_string_lossy().as_bytes().to_vec();
    Uuid::new_v5(&Uuid::NAMESPACE_URL, &bytes).to_string()
}

pub fn resolve_session_db_path(root: &Path) -> PathBuf {
    let key = workspace_key(root);
    let path = workspace_state_dir()
        .join("workspaces")
        .join(&key)
        .join("zerobot.db");
    let _ = ensure_workspace_mapping(root, &key);
    path
}

fn workspace_state_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".zerobot").join("state")
}

fn workspace_index_path() -> PathBuf {
    workspace_state_dir().join("workspaces").join("index.yaml")
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct WorkspaceIndex {
    #[serde(default)]
    workspaces: Vec<WorkspaceEntry>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct WorkspaceEntry {
    uuid: String,
    path: String,
}

fn ensure_workspace_mapping(root: &Path, key: &str) -> std::io::Result<()> {
    let index_path = workspace_index_path();
    if let Some(parent) = index_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let path_str = canonical.to_string_lossy().to_string();

    let mut index = match std::fs::read_to_string(&index_path) {
        Ok(raw) => serde_yaml::from_str::<WorkspaceIndex>(&raw).unwrap_or_default(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => WorkspaceIndex::default(),
        Err(err) => return Err(err),
    };

    let mut changed = false;
    let mut matched = false;
    for entry in &mut index.workspaces {
        if entry.uuid == key || entry.path == path_str {
            matched = true;
            if entry.uuid != key {
                entry.uuid = key.to_string();
                changed = true;
            }
            if entry.path != path_str {
                entry.path = path_str.clone();
                changed = true;
            }
        }
    }
    if !matched {
        index.workspaces.push(WorkspaceEntry {
            uuid: key.to_string(),
            path: path_str,
        });
        changed = true;
    }
    if changed {
        let serialized = serde_yaml::to_string(&index).unwrap_or_default();
        write_atomic(&index_path, serialized.as_bytes())?;
    }

    Ok(())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use tempfile::TempDir;

    #[test]
    fn prefers_zerobot_over_git() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let nested = root.join("nested");
        std::fs::create_dir_all(nested.join(".zerobot")).unwrap();
        let resolved = resolve_workspace_root(&nested);
        assert_eq!(resolved, nested);
    }

    #[test]
    fn falls_back_to_git_root() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let nested = root.join("nested/child");
        std::fs::create_dir_all(&nested).unwrap();
        let resolved = resolve_workspace_root(&nested);
        assert_eq!(resolved, root);
    }

    #[test]
    fn falls_back_to_cwd_when_no_markers() {
        let dir = TempDir::new().unwrap();
        let nested = dir.path().join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        let resolved = resolve_workspace_root(&nested);
        assert_eq!(resolved, nested);
    }

    #[test]
    fn resolves_workspace_db_path() {
        let home_dir = TempDir::new().unwrap();
        let prev_home = env::var("HOME").ok();
        env::set_var("HOME", home_dir.path());
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let path = resolve_session_db_path(root);
        assert!(path.to_string_lossy().contains("workspaces"));
        assert!(path.to_string_lossy().contains("zerobot.db"));
        let index = workspace_index_path();
        assert!(index.exists());
        let raw = std::fs::read_to_string(index).unwrap_or_default();
        let parsed: WorkspaceIndex = serde_yaml::from_str(&raw).unwrap_or_default();
        assert!(parsed
            .workspaces
            .iter()
            .any(|entry| entry.uuid == workspace_key(root)));
        if let Some(prev) = prev_home {
            env::set_var("HOME", prev);
        } else {
            env::remove_var("HOME");
        }
    }
}
