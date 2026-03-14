use std::path::{Path, PathBuf};

use zerobot_core::{SettingsBundle, SettingsScope, SettingsSource, ZeroSettings};

pub fn load_settings(project_root: &Path) -> SettingsBundle {
    let mut bundle = SettingsBundle::default();

    let mut sources: Vec<(SettingsScope, PathBuf)> = Vec::new();

    if let Some(user_path) = user_settings_path() {
        sources.push((SettingsScope::User, user_path));
    }

    let project_settings = project_root.join(".zero").join("settings.yaml");
    sources.push((SettingsScope::Project, project_settings));

    let local_settings = project_root.join(".zero").join("settings.local.yaml");
    sources.push((SettingsScope::Local, local_settings));

    if let Some(managed_path) = managed_settings_path(project_root) {
        sources.push((SettingsScope::Managed, managed_path));
    }

    for (scope, path) in sources {
        if !path.exists() {
            continue;
        }
        match read_settings(&path) {
            Ok(settings) => {
                bundle.active.merge(settings);
                bundle.sources.push(SettingsSource {
                    scope,
                    path: path.to_string_lossy().to_string(),
                });
            }
            Err(err) => {
                tracing::warn!("failed to read settings {:?}: {}", path, err);
            }
        }
    }

    bundle
}

fn read_settings(path: &Path) -> anyhow::Result<ZeroSettings> {
    let content = std::fs::read_to_string(path)?;
    let settings: ZeroSettings = serde_yaml::from_str(&content)?;
    Ok(settings)
}

fn user_settings_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".zero").join("settings.yaml"))
}

fn managed_settings_path(project_root: &Path) -> Option<PathBuf> {
    if let Ok(path) = std::env::var("ZEROBOT_MANAGED_SETTINGS") {
        return Some(PathBuf::from(path));
    }
    let project_managed = project_root.join(".zero").join("managed-settings.yaml");
    if project_managed.exists() {
        return Some(project_managed);
    }
    None
}
