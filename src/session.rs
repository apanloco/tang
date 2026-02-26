use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::plugin::Plugin;

#[derive(Deserialize, Debug, Clone)]
pub struct RemapTarget {
    pub note: String,
    pub detune: f64,
}

// ---------------------------------------------------------------------------
// New keyboard/split config
// ---------------------------------------------------------------------------

/// Top-level session config: one or more keyboards, each with splits.
pub struct SessionConfig {
    pub keyboards: Vec<KeyboardConfig>,
}

pub struct KeyboardConfig {
    pub name: Option<String>,
    pub splits: Vec<SplitConfig>,
}

pub struct SplitConfig {
    pub range: Option<(u8, u8)>,
    pub transpose: i8,
    pub instrument: Option<PluginConfig>,
    pub effects: Vec<EffectConfig>,
    pub pattern: Option<PatternConfig>,
}

/// Parsed pattern config for a split.
pub struct PatternConfig {
    pub bpm: f32,
    pub length_beats: f32,
    pub looping: bool,
    pub base_note: Option<u8>,
    pub events: Vec<(u64, u8, u8, u8)>, // (frame, status, note, velocity)
    pub enabled: bool,
}

#[derive(Deserialize)]
pub struct ModulatorConfig {
    #[serde(default = "default_mod_type", rename = "type")]
    pub mod_type: String,
    #[serde(default = "default_waveform")]
    pub waveform: String,
    #[serde(default = "default_rate")]
    pub rate: f64,
    #[serde(default = "default_attack")]
    pub attack: f64,
    #[serde(default = "default_decay")]
    pub decay: f64,
    #[serde(default = "default_sustain")]
    pub sustain: f64,
    #[serde(default = "default_release")]
    pub release: f64,
    #[serde(default, rename = "target")]
    pub targets: Vec<ModTargetConfig>,
}

#[derive(Deserialize)]
pub struct ModTargetConfig {
    /// Plugin parameter name (mutually exclusive with mod_* fields).
    #[serde(default)]
    pub param: Option<String>,
    /// Target a sibling modulator's LFO rate (by mod index).
    #[serde(default)]
    pub mod_rate: Option<usize>,
    /// Target a sibling modulator's target depth as [mod_index, target_index].
    #[serde(default)]
    pub mod_depth: Option<Vec<usize>>,
    /// Target a sibling modulator's envelope attack (by mod index).
    #[serde(default)]
    pub mod_attack: Option<usize>,
    /// Target a sibling modulator's envelope decay (by mod index).
    #[serde(default)]
    pub mod_decay: Option<usize>,
    /// Target a sibling modulator's envelope sustain (by mod index).
    #[serde(default)]
    pub mod_sustain: Option<usize>,
    /// Target a sibling modulator's envelope release (by mod index).
    #[serde(default)]
    pub mod_release: Option<usize>,
    #[serde(default = "default_depth")]
    pub depth: f64,
}

#[derive(Deserialize)]
pub struct PluginConfig {
    pub plugin: String,
    pub preset: Option<String>,
    #[serde(default = "default_volume")]
    pub volume: f64,
    #[serde(default = "default_pitch_bend_range")]
    pub pitch_bend_range: f64,
    #[serde(default)]
    pub remap: HashMap<String, RemapTarget>,
    #[serde(default)]
    pub params: HashMap<String, f64>,
    #[serde(default, rename = "modulator")]
    pub modulators: Vec<ModulatorConfig>,
}

fn default_volume() -> f64 {
    1.0
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
    #[serde(default, rename = "modulator")]
    pub modulators: Vec<ModulatorConfig>,
}

fn default_mix() -> f64 {
    1.0
}

// ---------------------------------------------------------------------------
// TOML deserialization helpers (intermediate structs)
// ---------------------------------------------------------------------------

/// New format: [[keyboard]] with nested [[keyboard.split]]
#[derive(Deserialize)]
struct NewSessionRaw {
    #[serde(default, rename = "keyboard")]
    keyboards: Vec<KeyboardRaw>,
}

#[derive(Deserialize)]
struct KeyboardRaw {
    name: Option<String>,
    #[serde(default, rename = "split")]
    splits: Vec<SplitRaw>,
}

#[derive(Deserialize)]
struct SplitRaw {
    range: Option<String>,
    #[serde(default)]
    transpose: i8,
    instrument: Option<PluginConfig>,
    #[serde(default, rename = "effect")]
    effects: Vec<EffectConfig>,
    pattern: Option<PatternRaw>,
}

#[derive(Deserialize)]
struct PatternRaw {
    #[serde(default = "default_pattern_bpm")]
    bpm: f64,
    #[serde(default = "default_pattern_length")]
    length_beats: f64,
    #[serde(default = "default_true")]
    looping: bool,
    #[serde(default)]
    base_note: Option<String>,
    #[serde(default)]
    events: Vec<PatternEventRaw>,
    #[serde(default)]
    enabled: bool,
}

fn default_true() -> bool { true }

#[derive(Deserialize)]
struct PatternEventRaw {
    frame: u64,
    status: String, // "on" or "off"
    note: String,   // e.g. "C4"
    #[serde(default)]
    velocity: u8,
}

fn default_pattern_bpm() -> f64 {
    120.0
}

fn default_pattern_length() -> f64 {
    4.0
}

fn default_mod_type() -> String {
    "lfo".into()
}

fn default_waveform() -> String {
    "sine".into()
}

fn default_rate() -> f64 {
    1.0
}

fn default_depth() -> f64 {
    0.5
}

fn default_attack() -> f64 {
    0.01
}

fn default_decay() -> f64 {
    0.3
}

fn default_sustain() -> f64 {
    0.7
}

fn default_release() -> f64 {
    0.5
}

/// Legacy format: [instrument] + [[effect]]
#[derive(Deserialize)]
struct LegacySessionRaw {
    instrument: PluginConfig,
    #[serde(default, rename = "effect")]
    effects: Vec<EffectConfig>,
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

pub fn load(path: &str) -> anyhow::Result<SessionConfig> {
    let content = std::fs::read_to_string(path)?;

    // Try new format first (has [[keyboard]])
    if let Ok(raw) = toml::from_str::<NewSessionRaw>(&content) {
        if !raw.keyboards.is_empty() {
            let mut keyboards = Vec::new();
            for kb in raw.keyboards {
                let mut splits = Vec::new();
                for sp in kb.splits {
                    let range = sp.range.as_deref().map(parse_range).transpose()?;
                    let pattern = sp.pattern.map(parse_pattern_raw).transpose()?;
                    splits.push(SplitConfig {
                        range,
                        transpose: sp.transpose,
                        instrument: sp.instrument,
                        effects: sp.effects,
                        pattern,
                    });
                }
                keyboards.push(KeyboardConfig {
                    name: kb.name,
                    splits,
                });
            }
            return Ok(SessionConfig { keyboards });
        }
    }

    // Fall back to legacy format ([instrument] + [[effect]])
    let legacy: LegacySessionRaw = toml::from_str(&content)?;
    Ok(SessionConfig {
        keyboards: vec![KeyboardConfig {
            name: None,
            splits: vec![SplitConfig {
                range: None,
                transpose: 0,
                instrument: Some(legacy.instrument),
                effects: legacy.effects,
                pattern: None,
            }],
        }],
    })
}

/// Parse a note range string like "C0-B3" into (low, high) MIDI note numbers (inclusive).
pub fn parse_range(s: &str) -> anyhow::Result<(u8, u8)> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 2 {
        anyhow::bail!("invalid range format '{}', expected 'NOTE-NOTE' (e.g. 'C0-B3')", s);
    }
    let low = parse_note_name(parts[0])?;
    let high = parse_note_name(parts[1])?;
    if low > high {
        anyhow::bail!("range '{}' has low ({}) > high ({})", s, low, high);
    }
    Ok((low, high))
}

/// Parse a raw pattern from TOML into a PatternConfig.
fn parse_pattern_raw(raw: PatternRaw) -> anyhow::Result<PatternConfig> {
    let base_note = raw
        .base_note
        .as_deref()
        .map(parse_note_name)
        .transpose()?;
    let mut events = Vec::with_capacity(raw.events.len());
    for ev in &raw.events {
        let note = parse_note_name(&ev.note)?;
        let status = match ev.status.as_str() {
            "on" => 0x90,
            "off" => 0x80,
            other => anyhow::bail!("invalid pattern event status '{other}', expected 'on' or 'off'"),
        };
        events.push((ev.frame, status, note, ev.velocity));
    }
    Ok(PatternConfig {
        bpm: raw.bpm as f32,
        length_beats: raw.length_beats as f32,
        looping: raw.looping,
        base_note,
        events,
        enabled: raw.enabled,
    })
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

// ---------------------------------------------------------------------------
// Saving
// ---------------------------------------------------------------------------

/// Data needed to serialize one keyboard for saving.
pub struct SaveKeyboard {
    pub name: String,
    pub splits: Vec<SaveSplit>,
}

/// Data needed to serialize one split for saving.
pub struct SaveSplit {
    pub range: Option<(u8, u8)>,
    pub transpose: i8,
    pub instrument: Option<SaveInstrument>,
    pub effects: Vec<SaveEffect>,
    pub pattern: Option<SavePattern>,
}

/// Data needed to serialize a pattern for saving.
pub struct SavePattern {
    pub bpm: f32,
    pub length_beats: f32,
    pub looping: bool,
    pub base_note: Option<u8>,
    pub events: Vec<(u64, u8, u8, u8)>, // (frame, status, note, velocity)
    pub enabled: bool,
}

/// Data needed to serialize a modulator for saving.
pub enum SaveModSource {
    Lfo { waveform: String, rate: f32 },
    Envelope { attack: f32, decay: f32, sustain: f32, release: f32 },
}

pub struct SaveModulator {
    pub source: SaveModSource,
    pub targets: Vec<SaveModTarget>,
}

/// Data needed to serialize a modulation target for saving.
pub struct SaveModTarget {
    pub kind: crate::plugin::chain::ModTargetKind,
    pub label: String,
    pub depth: f32,
}

/// Data needed to serialize an instrument slot for saving.
pub struct SaveInstrument {
    pub plugin: String,
    pub volume: f32,
    pub params: Vec<(String, f32)>,
    pub modulators: Vec<SaveModulator>,
}

/// Data needed to serialize an effect slot for saving.
pub struct SaveEffect {
    pub plugin: String,
    pub mix: f32,
    pub params: Vec<(String, f32)>,
    pub modulators: Vec<SaveModulator>,
}

#[derive(Serialize)]
struct SessionOut {
    #[serde(rename = "keyboard")]
    keyboards: Vec<KeyboardOut>,
}

#[derive(Serialize)]
struct KeyboardOut {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(rename = "split")]
    splits: Vec<SplitOut>,
}

#[derive(Serialize)]
struct SplitOut {
    #[serde(skip_serializing_if = "Option::is_none")]
    range: Option<String>,
    #[serde(skip_serializing_if = "is_zero_i8")]
    transpose: i8,
    #[serde(skip_serializing_if = "Option::is_none")]
    instrument: Option<InstrumentOut>,
    #[serde(skip_serializing_if = "Vec::is_empty", rename = "effect")]
    effects: Vec<EffectOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pattern: Option<PatternOut>,
}

#[derive(Serialize)]
struct PatternOut {
    bpm: f64,
    length_beats: f64,
    #[serde(skip_serializing_if = "is_true")]
    looping: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    base_note: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    events: Vec<PatternEventOut>,
    enabled: bool,
}

fn is_true(v: &bool) -> bool { *v }
fn is_zero_i8(v: &i8) -> bool { *v == 0 }

#[derive(Serialize)]
struct PatternEventOut {
    frame: u64,
    status: String,
    note: String,
    velocity: u8,
}

#[derive(Serialize)]
struct ModulatorOut {
    #[serde(rename = "type", skip_serializing_if = "is_lfo_type")]
    mod_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    waveform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    attack: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    decay: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sustain: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    release: Option<f64>,
    #[serde(skip_serializing_if = "Vec::is_empty", rename = "target")]
    targets: Vec<ModTargetOut>,
}

fn is_lfo_type(s: &String) -> bool {
    s == "lfo"
}

#[derive(Serialize)]
struct ModTargetOut {
    #[serde(skip_serializing_if = "Option::is_none")]
    param: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mod_rate: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mod_depth: Option<Vec<usize>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mod_attack: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mod_decay: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mod_sustain: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mod_release: Option<usize>,
    depth: f64,
}

#[derive(Serialize)]
struct InstrumentOut {
    plugin: String,
    #[serde(skip_serializing_if = "is_default_volume_f32")]
    volume: f32,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    params: HashMap<String, f64>,
    #[serde(skip_serializing_if = "Vec::is_empty", rename = "modulator")]
    modulators: Vec<ModulatorOut>,
}

#[derive(Serialize)]
struct EffectOut {
    plugin: String,
    #[serde(skip_serializing_if = "is_default_mix_f32")]
    mix: f32,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    params: HashMap<String, f64>,
    #[serde(skip_serializing_if = "Vec::is_empty", rename = "modulator")]
    modulators: Vec<ModulatorOut>,
}

fn save_mod_target_to_out(t: &SaveModTarget) -> ModTargetOut {
    use crate::plugin::chain::ModTargetKind;
    let mut out = ModTargetOut {
        param: None,
        mod_rate: None,
        mod_depth: None,
        mod_attack: None,
        mod_decay: None,
        mod_sustain: None,
        mod_release: None,
        depth: t.depth as f64,
    };
    match &t.kind {
        ModTargetKind::PluginParam { .. } => {
            out.param = Some(t.label.clone());
        }
        ModTargetKind::ModulatorRate { mod_index } => {
            out.mod_rate = Some(*mod_index);
        }
        ModTargetKind::ModulatorDepth { mod_index, target_index } => {
            out.mod_depth = Some(vec![*mod_index, *target_index]);
        }
        ModTargetKind::ModulatorAttack { mod_index } => {
            out.mod_attack = Some(*mod_index);
        }
        ModTargetKind::ModulatorDecay { mod_index } => {
            out.mod_decay = Some(*mod_index);
        }
        ModTargetKind::ModulatorSustain { mod_index } => {
            out.mod_sustain = Some(*mod_index);
        }
        ModTargetKind::ModulatorRelease { mod_index } => {
            out.mod_release = Some(*mod_index);
        }
    }
    out
}

fn is_default_volume_f32(v: &f32) -> bool {
    (*v - 1.0).abs() < f32::EPSILON
}

fn is_default_mix_f32(v: &f32) -> bool {
    (*v - 1.0).abs() < f32::EPSILON
}

/// Format a MIDI note number as a note name (e.g. 60 → "C4").
fn note_name(note: u8) -> String {
    crate::note_name(note)
}

/// Save the current session state to a TOML file.
pub fn save(path: &Path, keyboards: &[SaveKeyboard]) -> anyhow::Result<()> {
    let session = SessionOut {
        keyboards: keyboards
            .iter()
            .map(|kb| KeyboardOut {
                name: Some(kb.name.clone()),
                splits: kb
                    .splits
                    .iter()
                    .map(|sp| {
                        let mods_to_out = |mods: &[SaveModulator]| -> Vec<ModulatorOut> {
                            mods.iter()
                                .map(|m| {
                                    let targets: Vec<ModTargetOut> = m
                                        .targets
                                        .iter()
                                        .map(save_mod_target_to_out)
                                        .collect();
                                    match &m.source {
                                        SaveModSource::Lfo { waveform, rate } => ModulatorOut {
                                            mod_type: "lfo".into(),
                                            waveform: Some(waveform.clone()),
                                            rate: Some(*rate as f64),
                                            attack: None,
                                            decay: None,
                                            sustain: None,
                                            release: None,
                                            targets,
                                        },
                                        SaveModSource::Envelope { attack, decay, sustain, release } => ModulatorOut {
                                            mod_type: "envelope".into(),
                                            waveform: None,
                                            rate: None,
                                            attack: Some(*attack as f64),
                                            decay: Some(*decay as f64),
                                            sustain: Some(*sustain as f64),
                                            release: Some(*release as f64),
                                            targets,
                                        },
                                    }
                                })
                                .collect()
                        };
                        SplitOut {
                            range: sp
                                .range
                                .map(|(lo, hi)| format!("{}-{}", note_name(lo), note_name(hi))),
                            transpose: sp.transpose,
                            instrument: sp.instrument.as_ref().map(|inst| {
                                let params: HashMap<String, f64> = inst
                                    .params
                                    .iter()
                                    .map(|(k, v)| (k.clone(), *v as f64))
                                    .collect();
                                InstrumentOut {
                                    plugin: inst.plugin.clone(),
                                    volume: inst.volume,
                                    params,
                                    modulators: mods_to_out(&inst.modulators),
                                }
                            }),
                            effects: sp
                                .effects
                                .iter()
                                .map(|fx| {
                                    let params: HashMap<String, f64> = fx
                                        .params
                                        .iter()
                                        .map(|(k, v)| (k.clone(), *v as f64))
                                        .collect();
                                    EffectOut {
                                        plugin: fx.plugin.clone(),
                                        mix: fx.mix,
                                        params,
                                        modulators: mods_to_out(&fx.modulators),
                                    }
                                })
                                .collect(),
                            pattern: sp.pattern.as_ref().map(|p| {
                                PatternOut {
                                    bpm: p.bpm as f64,
                                    length_beats: p.length_beats as f64,
                                    looping: p.looping,
                                    base_note: p.base_note.map(note_name),
                                    events: p.events.iter().map(|&(frame, status, note, vel)| {
                                        PatternEventOut {
                                            frame,
                                            status: if status == 0x90 { "on".into() } else { "off".into() },
                                            note: note_name(note),
                                            velocity: vel,
                                        }
                                    }).collect(),
                                    enabled: p.enabled,
                                }
                            }),
                        }
                    })
                    .collect(),
            })
            .collect(),
    };

    let content = toml::to_string_pretty(&session)?;
    std::fs::write(path, content)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_range_valid() {
        assert_eq!(parse_range("C0-B3").unwrap(), (12, 59));
    }

    #[test]
    fn parse_range_single_octave() {
        assert_eq!(parse_range("C4-B4").unwrap(), (60, 71));
    }

    #[test]
    fn parse_range_same_note() {
        assert_eq!(parse_range("C4-C4").unwrap(), (60, 60));
    }

    #[test]
    fn parse_range_with_accidentals() {
        assert_eq!(parse_range("C#2-Bb5").unwrap(), (37, 82));
    }

    #[test]
    fn parse_range_invalid_low_gt_high() {
        assert!(parse_range("C4-C3").is_err());
    }

    #[test]
    fn parse_range_invalid_format() {
        assert!(parse_range("C4").is_err());
        assert!(parse_range("C4-B3-C5").is_err());
    }

    #[test]
    fn load_legacy_format() {
        let toml = r#"
[instrument]
plugin = "builtin:sine"

[[effect]]
plugin = "builtin:sine"
mix = 0.5
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.toml");
        std::fs::write(&path, toml).unwrap();

        let config = load(path.to_str().unwrap()).unwrap();
        assert_eq!(config.keyboards.len(), 1);
        assert_eq!(config.keyboards[0].splits.len(), 1);
        assert!(config.keyboards[0].splits[0].range.is_none());
        assert_eq!(config.keyboards[0].splits[0].instrument.as_ref().unwrap().plugin, "builtin:sine");
        assert_eq!(config.keyboards[0].splits[0].effects.len(), 1);
    }

    #[test]
    fn load_new_format() {
        let toml = r#"
[[keyboard]]
name = "Main"

[[keyboard.split]]
range = "C0-B3"

[keyboard.split.instrument]
plugin = "builtin:sine"

[[keyboard.split]]
range = "C4-C8"

[keyboard.split.instrument]
plugin = "builtin:sine"
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.toml");
        std::fs::write(&path, toml).unwrap();

        let config = load(path.to_str().unwrap()).unwrap();
        assert_eq!(config.keyboards.len(), 1);
        assert_eq!(config.keyboards[0].name, Some("Main".to_string()));
        assert_eq!(config.keyboards[0].splits.len(), 2);
        assert_eq!(config.keyboards[0].splits[0].range, Some((12, 59)));
        assert_eq!(config.keyboards[0].splits[1].range, Some((60, 108)));
    }

    #[test]
    fn save_and_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("saved.toml");

        let keyboards = vec![SaveKeyboard {
            name: "Main".into(),
            splits: vec![
                SaveSplit {
                    range: Some((12, 59)), // C0-B3
                    transpose: 0,
                    instrument: Some(SaveInstrument {
                        plugin: "builtin:sine".into(),
                        volume: 0.8,
                        params: vec![("cutoff".into(), 0.75)],
                        modulators: vec![],
                    }),
                    effects: vec![SaveEffect {
                        plugin: "builtin:sine".into(),
                        mix: 0.5,
                        params: vec![],
                        modulators: vec![],
                    }],
                    pattern: None,
                },
                SaveSplit {
                    range: None,
                    transpose: 0,
                    instrument: Some(SaveInstrument {
                        plugin: "builtin:sine".into(),
                        volume: 1.0,
                        params: vec![],
                        modulators: vec![],
                    }),
                    effects: vec![],
                    pattern: None,
                },
            ],
        }];

        save(&path, &keyboards).unwrap();

        // Reload and verify
        let config = load(path.to_str().unwrap()).unwrap();
        assert_eq!(config.keyboards.len(), 1);
        assert_eq!(config.keyboards[0].name, Some("Main".to_string()));
        assert_eq!(config.keyboards[0].splits.len(), 2);
        assert_eq!(config.keyboards[0].splits[0].range, Some((12, 59)));
        let inst = config.keyboards[0].splits[0].instrument.as_ref().unwrap();
        assert_eq!(inst.plugin, "builtin:sine");
        assert!((inst.volume - 0.8).abs() < 0.01);
        assert_eq!(config.keyboards[0].splits[0].effects.len(), 1);
        assert!((config.keyboards[0].splits[0].effects[0].mix - 0.5).abs() < 0.01);
        assert!(config.keyboards[0].splits[1].range.is_none());
    }

    #[test]
    fn save_and_reload_with_modulators() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mod_test.toml");

        let keyboards = vec![SaveKeyboard {
            name: "Main".into(),
            splits: vec![SaveSplit {
                range: None,
                transpose: 0,
                instrument: Some(SaveInstrument {
                    plugin: "builtin:sine".into(),
                    volume: 1.0,
                    params: vec![],
                    modulators: vec![SaveModulator {
                        source: SaveModSource::Lfo {
                            waveform: "sine".into(),
                            rate: 2.5,
                        },
                        targets: vec![SaveModTarget {
                            kind: crate::plugin::chain::ModTargetKind::PluginParam { param_index: 0 },
                            label: "cutoff".into(),
                            depth: 0.75,
                        }],
                    }],
                }),
                effects: vec![],
                pattern: None,
            }],
        }];

        save(&path, &keyboards).unwrap();

        let config = load(path.to_str().unwrap()).unwrap();
        assert_eq!(config.keyboards.len(), 1);
        let inst = config.keyboards[0].splits[0].instrument.as_ref().unwrap();
        assert_eq!(inst.modulators.len(), 1);
        let m = &inst.modulators[0];
        assert_eq!(m.waveform, "sine");
        assert!((m.rate - 2.5).abs() < 0.01);
        assert_eq!(m.targets.len(), 1);
        assert_eq!(m.targets[0].param.as_deref(), Some("cutoff"));
        assert!((m.targets[0].depth - 0.75).abs() < 0.01);
    }

    #[test]
    fn load_session_with_modulators() {
        let toml = r#"
[[keyboard]]
name = "Test"

[[keyboard.split]]

[keyboard.split.instrument]
plugin = "builtin:sine"

[[keyboard.split.instrument.modulator]]
waveform = "triangle"
rate = 0.5

[[keyboard.split.instrument.modulator.target]]
param = "frequency"
depth = 0.3
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_mod.toml");
        std::fs::write(&path, toml).unwrap();

        let config = load(path.to_str().unwrap()).unwrap();
        let inst = config.keyboards[0].splits[0].instrument.as_ref().unwrap();
        assert_eq!(inst.modulators.len(), 1);
        let m = &inst.modulators[0];
        assert_eq!(m.waveform, "triangle");
        assert!((m.rate - 0.5).abs() < 0.01);
        assert_eq!(m.targets.len(), 1);
        assert_eq!(m.targets[0].param.as_deref(), Some("frequency"));
        assert!((m.targets[0].depth - 0.3).abs() < 0.01);
    }
}
