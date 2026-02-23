use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::plugin::Plugin;

#[derive(Deserialize, Debug, Clone)]
pub struct RemapTarget {
    pub note: String,
    pub detune: f64,
}

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
    #[serde(default = "default_pitch_bend_range")]
    pub pitch_bend_range: f64,
    #[serde(default)]
    pub remap: HashMap<String, RemapTarget>,
    #[serde(default)]
    pub params: HashMap<String, f64>,
}

fn default_pitch_bend_range() -> f64 {
    2.0
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
    session_dir
        .join(plugin_source)
        .to_string_lossy()
        .to_string()
}

/// Parse a note name like "C4", "G#3", "Bb5" into a MIDI note number.
/// C4 = 60, A0 = 21. Formula: (octave + 1) * 12 + semitone.
pub fn parse_note_name(name: &str) -> anyhow::Result<u8> {
    let bytes = name.as_bytes();
    if bytes.is_empty() {
        anyhow::bail!("empty note name");
    }

    let letter = bytes[0].to_ascii_uppercase();
    let semitone_base = match letter {
        b'C' => 0,
        b'D' => 2,
        b'E' => 4,
        b'F' => 5,
        b'G' => 7,
        b'A' => 9,
        b'B' => 11,
        _ => anyhow::bail!("invalid note letter '{}'", bytes[0] as char),
    };

    let (accidental, rest) = if bytes.len() > 1 && bytes[1] == b'#' {
        (1i8, &name[2..])
    } else if bytes.len() > 1 && bytes[1] == b'b' {
        (-1i8, &name[2..])
    } else {
        (0i8, &name[1..])
    };

    let octave: i8 = rest
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid octave in note name '{name}'"))?;

    let note = (octave as i16 + 1) * 12 + semitone_base as i16 + accidental as i16;
    if !(0..=127).contains(&note) {
        anyhow::bail!("note '{name}' is out of MIDI range (0-127)");
    }
    Ok(note as u8)
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
