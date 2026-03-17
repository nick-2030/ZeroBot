use crate::config::Settings;
use glob::glob;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use tokio::time::{timeout, Duration};

const FILES: [&str; 3] = ["AGENTS.md", "CLAUDE.md", "CONTEXT.md"];
const URL_TIMEOUT_SECS: u64 = 5;

#[derive(Debug, Clone)]
pub struct Instruction {
    pub source: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct InstructionSources {
    pub files: Vec<PathBuf>,
    pub urls: Vec<String>,
}

pub fn system_sources(settings: &Settings, cwd: &Path) -> InstructionSources {
    let root = find_workspace_root(cwd);
    let mut files = Vec::new();
    let mut urls = Vec::new();
    let mut seen = HashSet::new();

    if let Some(path) = find_project_instruction(cwd, &root) {
        push_unique(&path, &mut files, &mut seen);
    }

    for path in global_instruction_paths() {
        if path.exists() {
            push_unique(&path, &mut files, &mut seen);
        }
    }

    let (config_files, config_urls) = config_instruction_sources(settings, cwd);
    for path in config_files {
        push_unique(&path, &mut files, &mut seen);
    }
    urls.extend(config_urls);

    InstructionSources { files, urls }
}

pub fn load_file_instructions(paths: &[PathBuf]) -> Vec<Instruction> {
    let mut results = Vec::new();
    for path in paths {
        let content = std::fs::read_to_string(path).unwrap_or_default();
        let trimmed = content.trim();
        if trimmed.is_empty() {
            continue;
        }
        let source = path.display().to_string();
        results.push(Instruction {
            source: source.clone(),
            content: format!("Instructions from: {}\n{}", source, trimmed),
        });
    }
    results
}

pub async fn fetch_url_instructions(urls: &[String]) -> Vec<Instruction> {
    let mut results = Vec::new();
    if urls.is_empty() {
        return results;
    }
    let client = reqwest::Client::new();
    for url in urls {
        if let Some(cached) = url_cache_get(url) {
            results.push(Instruction {
                source: url.clone(),
                content: format!("Instructions from: {}\n{}", url, cached),
            });
            continue;
        }
        let fetched = timeout(Duration::from_secs(URL_TIMEOUT_SECS), client.get(url).send()).await;
        let Ok(Ok(resp)) = fetched else { continue };
        if !resp.status().is_success() {
            continue;
        }
        let Ok(text) = resp.text().await else { continue };
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        url_cache_put(url, trimmed.to_string());
        results.push(Instruction {
            source: url.clone(),
            content: format!("Instructions from: {}\n{}", url, trimmed),
        });
    }
    results
}

pub fn resolve_nearby_instructions(session_id: &str, filepath: &Path) -> Vec<Instruction> {
    let base = filepath.parent().unwrap_or(filepath);
    let root = find_workspace_root(base);
    let mut results = Vec::new();
    let mut current = filepath.parent().map(|p| p.to_path_buf());
    while let Some(dir) = current {
        if let Some(path) = find_instruction_in_dir(&dir) {
            if mark_loaded(session_id, &path) {
                let content = std::fs::read_to_string(&path).unwrap_or_default();
                let trimmed = content.trim();
                if !trimmed.is_empty() {
                    let source = path.display().to_string();
                    results.push(Instruction {
                        source: source.clone(),
                        content: format!("Instructions from: {}\n{}", source, trimmed),
                    });
                }
            }
        }
        if dir == root {
            break;
        }
        current = dir.parent().map(|p| p.to_path_buf());
    }
    results
}

fn config_instruction_sources(settings: &Settings, cwd: &Path) -> (Vec<PathBuf>, Vec<String>) {
    let mut files = Vec::new();
    let mut urls = Vec::new();
    for item in &settings.instructions {
        let raw = item.trim();
        if raw.is_empty() {
            continue;
        }
        if is_url(raw) {
            urls.push(raw.to_string());
            continue;
        }
        let expanded = expand_home(raw);
        let path = if Path::new(&expanded).is_absolute() {
            PathBuf::from(&expanded)
        } else {
            cwd.join(&expanded)
        };
        if looks_like_glob(&expanded) {
            if let Ok(entries) = glob(path.to_string_lossy().as_ref()) {
                for entry in entries.flatten() {
                    files.push(entry);
                }
            }
        } else if path.exists() {
            files.push(path);
        }
    }
    (files, urls)
}

fn global_instruction_paths() -> Vec<PathBuf> {
    let mut result = Vec::new();
    if let Some(home) = home_dir() {
        result.push(home.join(".zerobot").join("AGENTS.md"));
        result.push(home.join(".claude").join("CLAUDE.md"));
    }
    result
}

fn find_project_instruction(cwd: &Path, root: &Path) -> Option<PathBuf> {
    let mut current = cwd.to_path_buf();
    loop {
        if let Some(found) = find_instruction_in_dir(&current) {
            return Some(found);
        }
        if current == *root {
            break;
        }
        let parent = current.parent()?.to_path_buf();
        if parent == current {
            break;
        }
        current = parent;
    }
    None
}

fn find_instruction_in_dir(dir: &Path) -> Option<PathBuf> {
    for name in FILES {
        let candidate = dir.join(name);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn find_workspace_root(start: &Path) -> PathBuf {
    let mut current = start.to_path_buf();
    loop {
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
    start.to_path_buf()
}

fn is_url(raw: &str) -> bool {
    raw.starts_with("http://") || raw.starts_with("https://")
}

fn looks_like_glob(raw: &str) -> bool {
    raw.contains('*') || raw.contains('?') || raw.contains('[')
}

fn expand_home(raw: &str) -> String {
    if let Some(stripped) = raw.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(stripped).to_string_lossy().to_string();
        }
    }
    raw.to_string()
}

fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

fn push_unique(path: &Path, out: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>) {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if seen.insert(canonical.clone()) {
        out.push(canonical);
    }
}

fn loaded_cache() -> &'static Mutex<HashMap<String, HashSet<PathBuf>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, HashSet<PathBuf>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn mark_loaded(session_id: &str, path: &Path) -> bool {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut guard = loaded_cache().lock().expect("loaded cache lock");
    let entry = guard
        .entry(session_id.to_string())
        .or_insert_with(HashSet::new);
    entry.insert(canonical)
}

fn url_cache() -> &'static Mutex<HashMap<String, String>> {
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn url_cache_get(url: &str) -> Option<String> {
    let guard = url_cache().lock().expect("url cache lock");
    guard.get(url).cloned()
}

fn url_cache_put(url: &str, content: String) {
    let mut guard = url_cache().lock().expect("url cache lock");
    guard.insert(url.to_string(), content);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Settings;
    use tempfile::tempdir;

    #[test]
    fn resolves_project_instruction() {
        let dir = tempdir().expect("tmpdir");
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("AGENTS.md"), "project instructions").unwrap();
        let child = root.join("src");
        std::fs::create_dir_all(&child).unwrap();

        let settings = Settings::default();
        let sources = system_sources(&settings, &child);
        assert!(sources.files.iter().any(|p| p.ends_with("AGENTS.md")));
    }

    #[test]
    fn resolve_nearby_instructions_once_per_session() {
        let dir = tempdir().expect("tmpdir");
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("AGENTS.md"), "project instructions").unwrap();
        let file = root.join("src").join("lib.rs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "fn main() {}").unwrap();

        let first = resolve_nearby_instructions("s1", &file);
        let second = resolve_nearby_instructions("s1", &file);
        assert_eq!(first.len(), 1);
        assert!(second.is_empty());
    }
}
