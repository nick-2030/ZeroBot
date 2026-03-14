use std::path::PathBuf;
use std::sync::Mutex;

use zerobot_core::PluginManifest;

#[derive(Default)]
pub struct PluginRegistry {
    plugins: Mutex<Vec<PluginManifest>>,
}

impl PluginRegistry {
    pub fn load_from_dir(&self, path: PathBuf) -> anyhow::Result<()> {
        if !path.exists() {
            return Ok(());
        }
        let mut manifests = Vec::new();
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("yaml") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(manifest) = serde_yaml::from_str::<PluginManifest>(&content) {
                        manifests.push(manifest);
                    }
                }
            }
        }
        let mut guard = self.plugins.lock().unwrap();
        *guard = manifests;
        Ok(())
    }

    pub fn list(&self) -> Vec<PluginManifest> {
        self.plugins.lock().unwrap().clone()
    }
}
