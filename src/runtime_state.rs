use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuntimeState {
    pub default_provider: Option<String>,
}

pub fn load_runtime_state() -> Option<RuntimeState> {
    let path = runtime_state_path()?;
    let contents = std::fs::read_to_string(path).ok()?;
    toml::from_str(&contents).ok()
}

pub fn save_runtime_state(state: &RuntimeState) -> Result<(), String> {
    let path =
        runtime_state_path().ok_or_else(|| "Failed to resolve XDG state path".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create state dir {}: {}", parent.display(), e))?;
    }
    let contents =
        toml::to_string_pretty(state).map_err(|e| format!("Failed to serialize state: {}", e))?;
    std::fs::write(&path, contents)
        .map_err(|e| format!("Failed to write state file {}: {}", path.display(), e))?;
    Ok(())
}

pub fn runtime_state_path() -> Option<PathBuf> {
    let state_home = crate::paths::state_dir().ok()?;
    Some(state_home.join("eavs").join("state.toml"))
}
