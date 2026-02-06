use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::plugin::Plugin;

#[derive(Deserialize)]
pub struct SessionConfig {
    pub instrument: PluginConfig,
    #[serde(default, rename = "effect")]
    pub effects: Vec<EffectConfig>,
}

#[derive(Deserialize)]
pub struct PluginConfig {
    pub plugin: String,
    pub preset: Option<String>,
    #[serde(default)]
    pub params: HashMap<String, f64>,
}

#[derive(Deserialize)]
pub struct EffectConfig {
    pub plugin: String,
    pub preset: Option<String>,
    #[serde(default = "default_mix")]
    pub mix: f64,
    #[serde(default)]
    pub params: HashMap<String, f64>,
}

fn default_mix() -> f64 {
    1.0
}

pub fn load(path: &str) -> anyhow::Result<SessionConfig> {
    let content = std::fs::read_to_string(path)?;
    Ok(toml::from_str(&content)?)
}

/// Resolve a plugin path relative to the session file's directory.
pub fn resolve_plugin_path(plugin_source: &str, session_dir: &Path) -> String {
    // URI-style references (lv2:..., clap:...) pass through as-is
    if plugin_source.contains(':') {
        return plugin_source.to_string();
    }
    // Absolute paths pass through
    let p = Path::new(plugin_source);
    if p.is_absolute() {
        return plugin_source.to_string();
    }
    // Relative paths are resolved against the session file's directory
    session_dir.join(plugin_source).to_string_lossy().to_string()
}

/// Apply a preset to a loaded plugin (no parameter overrides).
pub fn apply_preset(plugin: &mut Box<dyn Plugin>, preset_name: &str) {
    let presets = plugin.presets();
    match presets.iter().find(|p| p.name == *preset_name) {
        Some(preset_info) => {
            let id = preset_info.id.clone();
            match plugin.load_preset(&id) {
                Ok(()) => log::info!("Loaded preset '{}' on {}", preset_name, plugin.name()),
                Err(e) => log::warn!(
                    "Failed to load preset '{}' on {}: {}",
                    preset_name,
                    plugin.name(),
                    e
                ),
            }
        }
        None => {
            log::warn!(
                "Preset '{}' not found for {} (available: {})",
                preset_name,
                plugin.name(),
                presets
                    .iter()
                    .map(|p| p.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
}
