use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub lib_path: String,
    pub lib_name: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            lib_path: dirs::document_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("KiCad")
                .join("libraries")
                .to_string_lossy()
                .to_string(),
            lib_name: "local_lib".to_string(),
        }
    }
}

fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("jlcpcb-kicad")
        .join("settings.json")
}

fn history_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("jlcpcb-kicad")
        .join("history.json")
}

pub fn load_history() -> Vec<String> {
    std::fs::read_to_string(history_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_history(history: &[String]) {
    let path = history_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(history) {
        let _ = std::fs::write(&path, json);
    }
}

pub fn load() -> Settings {
    let path = config_path();
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(settings: &Settings) -> Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(settings)?;
    std::fs::write(&path, json)?;
    Ok(())
}
