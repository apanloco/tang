use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::plugin::Plugin;

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct RemapTarget {
    pub note: String,
    pub detune: f64,
}

/// Where a front-end draws a node, in the pixels of its canvas. The engine and the TUI do not use it, and it is kept when
/// a session is saved: `ui = { x = 80, y = 120 }` on the node's table, the master's in a top-level `[ui]` table.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
pub struct NodeUi {
    pub x: f32,
    pub y: f32,
}

impl NodeUi {
    /// How far from the origin a node may be drawn, either way: a canvas has to be one a person can come back from.
    pub const LIMIT: f64 = 100_000.0;
    /// A place is kept to this many parts of a pixel (a tenth): it is read by people, in the file, and a drag is not
    /// that exact.
    pub const PARTS: f64 = 10.0;

    /// The place as it is kept: to a tenth of a pixel (half away from zero), no further than `LIMIT`, and never `-0`.
    /// `None` if a number of it is not one (NaN, infinite). Done in 64 bits, as a client's page does it, so that what
    /// the page says it is is what it is.
    pub fn tidy(self) -> Option<NodeUi> {
        if !self.x.is_finite() || !self.y.is_finite() {
            return None;
        }
        let tidy = |v: f32| ((f64::from(v).clamp(-Self::LIMIT, Self::LIMIT) * Self::PARTS).round() / Self::PARTS + 0.0) as f32;
        Some(NodeUi { x: tidy(self.x), y: tidy(self.y) })
    }
}

/// A node's `ui`, read leniently: a layout is not worth refusing a session for, so one that is not a pair of numbers
/// is left out (and logged) and the rest of the session loads.
fn lenient_ui<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<NodeUi>, D::Error> {
    let value = toml::Value::deserialize(deserializer)?;
    Ok(node_ui_of(value))
}

fn node_ui_of(value: toml::Value) -> Option<NodeUi> {
    match value.try_into::<NodeUi>() {
        Ok(ui) => Some(ui),
        Err(error) => {
            log::warn!("ignoring a position that is not {{ x, y }} numbers: {error}");
            None
        }
    }
}

/// A `pinned` list, read leniently like a place: which parameters a front-end shows as knobs is not worth refusing a session for,
/// so what is not a list is left out (and logged), and so is whatever in a list is not a name.
fn lenient_names<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    Ok(match toml::Value::deserialize(deserializer)? {
        toml::Value::Array(items) => items
            .into_iter()
            .filter_map(|item| match item {
                toml::Value::String(name) => Some(name),
                other => {
                    log::warn!("ignoring a pinned parameter that is not a name: {other}");
                    None
                }
            })
            .collect(),
        other => {
            log::warn!("ignoring a `pinned` that is not a list of names: {other}");
            Vec::new()
        }
    })
}

/// The top-level `[ui]` table, read leniently like a node's.
fn lenient_table<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<toml::Table>, D::Error> {
    Ok(match toml::Value::deserialize(deserializer)? {
        toml::Value::Table(table) => Some(table),
        _ => {
            log::warn!("ignoring a [ui] that is not a table");
            None
        }
    })
}

// ---------------------------------------------------------------------------
// Instrument-centric config
// ---------------------------------------------------------------------------

/// Top-level session config: a flat list of instruments, optional submix
/// groups, and an optional Piano-tab tonality (root + scale).
pub struct SessionConfig {
    pub instruments: Vec<InstrumentSlotConfig>,
    pub groups: Vec<GroupConfig>,
    pub piano: Option<PianoConfig>,
    /// Where the master mix is drawn (`[ui] master = { x, y }`).
    pub master_ui: Option<NodeUi>,
}

/// A submix group: members are the instruments whose `group` index points here.
pub struct GroupConfig {
    pub name: Option<String>,
    pub volume: f32,
    pub pan: f32,
    pub effects: Vec<EffectConfig>,
    /// Group-scoped modulators. Their targets address member instruments
    /// (`member` + optional `slot`), bus effects (`bus`), or sibling
    /// group modulators (`mod_*`).
    pub modulators: Vec<ModulatorConfig>,
    /// Where the group is drawn.
    pub ui: Option<NodeUi>,
}

/// Optional `[piano]` section of the session TOML.
#[derive(Deserialize, Serialize, Debug, Clone, Default)]
pub struct PianoConfig {
    /// Scale spec like "C major" or "F# dorian".
    #[serde(default)]
    pub scale: Option<String>,
    /// If true, off-scale notes are dropped at the sender (Locked mode).
    #[serde(default)]
    pub locked: bool,
}

/// One instrument slot: an optional plugin with range, effects, and pattern.
pub struct InstrumentSlotConfig {
    pub range: Option<(u8, u8)>,
    pub transpose: i8,
    pub instrument: Option<PluginConfig>,
    pub effects: Vec<EffectConfig>,
    /// The lane's modulators (their targets carry the chain slot they land on). They belong to the lane, not
    /// to its instrument plugin: a lane whose plugin was cleared has its effects and these still.
    pub modulators: Vec<ModulatorConfig>,
    pub pattern: Option<PatternConfig>,
    /// Submix group membership: index into the session's groups, or None.
    pub group: Option<usize>,
    /// Where the lane is drawn.
    pub ui: Option<NodeUi>,
}

/// Parsed pattern config for an instrument slot.
pub struct PatternConfig {
    pub bpm: f32,
    pub length_beats: f32,
    pub looping: bool,
    pub base_note: Option<u8>,
    pub events: Vec<(u64, u8, u8, u8)>, // (frame, status, note, velocity)
    pub enabled: bool,
    /// Transpose playback by scale degrees of the piano scale instead of
    /// by semitones.
    pub in_key: bool,
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
    /// For `type = "midi"`: the bound CC number (mutually exclusive with
    /// `midi_pitch_bend` / `midi_aftertouch`). Absent = unbound (armed for
    /// learning on load).
    #[serde(default)]
    pub midi_cc: Option<u8>,
    /// For `type = "midi"`: bound to the pitch-bend wheel.
    #[serde(default)]
    pub midi_pitch_bend: bool,
    /// For `type = "midi"`: bound to key pressure / aftertouch (channel
    /// pressure or poly aftertouch).
    #[serde(default)]
    pub midi_aftertouch: bool,
    #[serde(default, rename = "target")]
    pub targets: Vec<ModTargetConfig>,
    /// Where the modulator is drawn.
    #[serde(default, deserialize_with = "lenient_ui")]
    pub ui: Option<NodeUi>,
}

#[derive(Deserialize)]
pub struct ModTargetConfig {
    /// Plugin parameter name (mutually exclusive with mod_* fields).
    #[serde(default)]
    pub param: Option<String>,
    /// Chain slot a `param` target lives on: 0 = instrument (default),
    /// 1..N = effects in order.
    #[serde(default)]
    pub slot: usize,
    /// (Group modulators) member ordinal a `param` target lands on. Present
    /// only for group-scoped modulator targets that drive a member instrument.
    #[serde(default)]
    pub member: Option<usize>,
    /// (Group modulators) bus-effect index a `param` target lands on. Present
    /// only for group-scoped modulator targets that drive a bus effect.
    #[serde(default)]
    pub bus: Option<usize>,
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
    /// The parameters a front-end shows as knobs on the instrument's node, by name, in order (`pinned = [...]`). The engine and
    /// the TUI do not use them, and they are kept when a session is saved.
    #[serde(default, deserialize_with = "lenient_names")]
    pub pinned: Vec<String>,
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
    /// The parameters a front-end shows as knobs on the effect's node, by name, in order.
    #[serde(default, deserialize_with = "lenient_names")]
    pub pinned: Vec<String>,
    /// Where the effect is drawn.
    #[serde(default, deserialize_with = "lenient_ui")]
    pub ui: Option<NodeUi>,
}

fn default_mix() -> f64 {
    1.0
}

// ---------------------------------------------------------------------------
// TOML deserialization helpers (intermediate structs)
// ---------------------------------------------------------------------------

/// New flat format: [[instrument]] at top level.
/// Instrument plugin fields are directly on the [[instrument]] table.
#[derive(Deserialize)]
struct SessionRaw {
    #[serde(default, rename = "instrument")]
    instruments: Vec<InstrumentSlotRaw>,
    #[serde(default, rename = "group")]
    groups: Vec<GroupRaw>,
    #[serde(default)]
    piano: Option<PianoConfig>,
    #[serde(default, deserialize_with = "lenient_table")]
    ui: Option<toml::Table>,
}

#[derive(Deserialize)]
struct GroupRaw {
    #[serde(default)]
    name: Option<String>,
    #[serde(default = "default_volume")]
    volume: f64,
    #[serde(default)]
    pan: f64,
    #[serde(default, rename = "effect")]
    effects: Vec<EffectConfig>,
    #[serde(default, rename = "modulator")]
    modulators: Vec<ModulatorConfig>,
    #[serde(default, deserialize_with = "lenient_ui")]
    ui: Option<NodeUi>,
}

#[derive(Deserialize)]
struct InstrumentSlotRaw {
    // Instrument plugin fields (flat on the table).
    plugin: Option<String>,
    #[serde(default)]
    preset: Option<String>,
    #[serde(default = "default_volume")]
    volume: f64,
    #[serde(default = "default_pitch_bend_range")]
    pitch_bend_range: f64,
    #[serde(default)]
    remap: HashMap<String, RemapTarget>,
    #[serde(default)]
    params: HashMap<String, f64>,
    #[serde(default, deserialize_with = "lenient_names")]
    pinned: Vec<String>,
    #[serde(default, rename = "modulator")]
    modulators: Vec<ModulatorConfig>,
    // Slot-level fields.
    #[serde(default)]
    range: Option<String>,
    #[serde(default)]
    transpose: i8,
    #[serde(default)]
    group: Option<usize>,
    #[serde(default, rename = "effect")]
    effects: Vec<EffectConfig>,
    #[serde(default)]
    pattern: Option<PatternRaw>,
    #[serde(default, deserialize_with = "lenient_ui")]
    ui: Option<NodeUi>,
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
    /// "chromatic" (default) or "in_key".
    #[serde(default)]
    transpose: Option<String>,
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

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

pub fn load(path: &str) -> anyhow::Result<SessionConfig> {
    let content = std::fs::read_to_string(path)?;
    parse(&content)
}

/// Refuse a `nan` or an `inf` anywhere in a session file (TOML has both), and a number too
/// big for the 32-bit floats the model and the audio thread hold (`1e39` is finite to TOML's
/// 64 bits and `inf` once narrowed): as a volume, a rate or a depth it would be in the model
/// and in the audio thread, and a snapshot with one in it is not even JSON. The error says
/// where it is.
fn reject_non_finite(table: &toml::Table) -> anyhow::Result<()> {
    fn walk(value: &toml::Value, path: &str) -> anyhow::Result<()> {
        match value {
            toml::Value::Float(number) if !number.is_finite() => anyhow::bail!("{path} is not a finite number ({number})"),
            toml::Value::Float(number) if !(*number as f32).is_finite() => {
                anyhow::bail!("{path} is too big for the 32-bit numbers the audio engine holds ({number})")
            }
            toml::Value::Array(items) => items.iter().enumerate().try_for_each(|(i, item)| walk(item, &format!("{path}[{i}]"))),
            toml::Value::Table(table) => table.iter().try_for_each(|(key, item)| walk(item, &format!("{path}.{key}"))),
            _ => Ok(()),
        }
    }
    table.iter().try_for_each(|(key, value)| walk(value, key))
}

/// Parse session TOML text (the body of `load`, usable without a file).
pub fn parse(content: &str) -> anyhow::Result<SessionConfig> {
    reject_non_finite(&content.parse::<toml::Table>()?)?;
    let raw: SessionRaw = toml::from_str(content)?;
    let groups: Vec<GroupConfig> = raw
        .groups
        .into_iter()
        .map(|g| GroupConfig {
            name: g.name,
            volume: g.volume as f32,
            pan: (g.pan as f32).clamp(-1.0, 1.0),
            effects: g.effects,
            modulators: g.modulators,
            ui: g.ui,
        })
        .collect();
    let mut instruments = Vec::new();
    for slot in raw.instruments {
        let range = slot.range.as_deref().map(parse_range).transpose()?;
        let pattern = slot.pattern.map(parse_pattern_raw).transpose()?;
        let instrument = slot.plugin.map(|plugin| PluginConfig {
            plugin,
            preset: slot.preset,
            volume: slot.volume,
            pitch_bend_range: slot.pitch_bend_range,
            remap: slot.remap,
            params: slot.params,
            pinned: slot.pinned,
        });
        // Drop out-of-range group references defensively.
        let group = slot.group.filter(|&g| g < groups.len());
        instruments.push(InstrumentSlotConfig {
            range,
            transpose: slot.transpose,
            instrument,
            effects: slot.effects,
            modulators: slot.modulators,
            pattern,
            group,
            ui: slot.ui,
        });
    }
    let master_ui = raw.ui.and_then(|mut table| table.remove("master")).and_then(node_ui_of);
    Ok(SessionConfig { instruments, groups, piano: raw.piano, master_ui })
}

/// Parse a note range like "C0-B3" into (low, high) MIDI note numbers
/// (inclusive). Either bound may be omitted for an open-ended range: "C4-"
/// means C4 and up (to MIDI 127), "-C4" means up to and including C4 (from
/// MIDI 0). Octave -1 note names are accepted (e.g. "C-1-B3").
pub fn parse_range(s: &str) -> anyhow::Result<(u8, u8)> {
    let (low_str, high_str) = split_range_bounds(s.trim()).ok_or_else(|| {
        anyhow::anyhow!(
            "invalid range format '{}', expected 'NOTE-NOTE' (e.g. 'C0-B3'); \
             open-ended 'C4-' or '-C4' are also allowed",
            s
        )
    })?;
    let low_str = low_str.trim();
    let high_str = high_str.trim();
    if low_str.is_empty() && high_str.is_empty() {
        anyhow::bail!("invalid range '{}': at least one bound is required", s);
    }
    let low = if low_str.is_empty() { 0 } else { parse_note_name(low_str)? };
    let high = if high_str.is_empty() { 127 } else { parse_note_name(high_str)? };
    if low > high {
        anyhow::bail!("range '{}' has low ({}) > high ({})", s, low, high);
    }
    Ok((low, high))
}

/// Split a range string at its bounds separator '-'. A '-' right after the
/// low bound's note letter (or accidental) that is followed by a digit is the
/// negative-octave sign, not the separator (e.g. "C-1-B3" → ("C-1", "B3")).
fn split_range_bounds(s: &str) -> Option<(&str, &str)> {
    let b = s.as_bytes();
    if b.is_empty() {
        return None;
    }
    if b[0] == b'-' {
        return Some(("", &s[1..]));
    }
    let mut i = 1; // note letter
    if matches!(b.get(i), Some(b'#' | b'b')) {
        i += 1; // accidental
    }
    if b.get(i) == Some(&b'-') && b.get(i + 1).is_some_and(u8::is_ascii_digit) {
        i += 1; // negative-octave sign
    }
    while b.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1; // octave digits
    }
    while b.get(i).is_some_and(u8::is_ascii_whitespace) {
        i += 1; // whitespace before separator
    }
    if b.get(i) == Some(&b'-') {
        Some((&s[..i], &s[i + 1..]))
    } else {
        None
    }
}

/// Format a note range as a string `parse_range` accepts, preserving
/// open-ended forms: (0, hi) → "-B3", (lo, 127) → "C4-", else "C0-B3".
pub fn format_range((low, high): (u8, u8)) -> String {
    match (low, high) {
        (0, hi) => format!("-{}", note_name(hi)),
        (lo, 127) => format!("{}-", note_name(lo)),
        (lo, hi) => format!("{}-{}", note_name(lo), note_name(hi)),
    }
}

/// Parse a raw pattern from TOML into a PatternConfig.
fn parse_pattern_raw(raw: PatternRaw) -> anyhow::Result<PatternConfig> {
    let base_note = raw
        .base_note
        .as_deref()
        .map(parse_note_name)
        .transpose()?;
    let in_key = match raw.transpose.as_deref() {
        None | Some("chromatic") => false,
        Some("in_key") | Some("in-key") | Some("in key") => true,
        Some(other) => anyhow::bail!(
            "invalid pattern transpose '{other}', expected 'chromatic' or 'in_key'"
        ),
    };
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
        in_key,
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
/// Accepts either the preset's display `name` or its `id`.
pub fn apply_preset(plugin: &mut Box<dyn Plugin>, preset_ref: &str) {
    let presets = plugin.presets();
    let found = presets
        .iter()
        .find(|p| p.name == *preset_ref)
        .or_else(|| presets.iter().find(|p| p.id == *preset_ref));
    match found {
        Some(preset_info) => {
            let id = preset_info.id.clone();
            match plugin.load_preset(&id) {
                Ok(()) => log::info!("Loaded preset '{}' on {}", preset_info.name, plugin.name()),
                Err(e) => log::warn!(
                    "Failed to load preset '{}' on {}: {}",
                    preset_info.name,
                    plugin.name(),
                    e
                ),
            }
        }
        None => {
            log::warn!(
                "Preset '{}' not found for {} (available: {})",
                preset_ref,
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

/// Data needed to serialize one instrument slot for saving.
pub struct SaveInstrumentSlot {
    pub range: Option<(u8, u8)>,
    pub transpose: i8,
    /// Lane-scoped modulators (targets carry their chain slot).
    pub modulators: Vec<SaveModulator>,
    pub instrument: Option<SaveInstrument>,
    pub effects: Vec<SaveEffect>,
    pub pattern: Option<SavePattern>,
    /// Submix group membership: index into the saved groups, or None.
    pub group: Option<usize>,
    /// Where the lane is drawn.
    pub ui: Option<NodeUi>,
}

/// Data needed to serialize a submix group for saving.
pub struct SaveGroup {
    pub name: Option<String>,
    pub volume: f32,
    pub pan: f32,
    pub effects: Vec<SaveEffect>,
    /// Group-scoped modulators (targets address members / bus effects).
    pub modulators: Vec<SaveModulator>,
    /// Where the group is drawn.
    pub ui: Option<NodeUi>,
}

/// Data needed to serialize a pattern for saving.
pub struct SavePattern {
    pub bpm: f32,
    pub length_beats: f32,
    pub looping: bool,
    pub base_note: Option<u8>,
    pub events: Vec<(u64, u8, u8, u8)>, // (frame, status, note, velocity)
    pub enabled: bool,
    pub in_key: bool,
}

/// Data needed to serialize a modulator for saving.
pub enum SaveModSource {
    Lfo { waveform: String, rate: f32 },
    Envelope { attack: f32, decay: f32, sustain: f32, release: f32 },
    /// A MIDI-learned control. `cc = Some(n)` = CC n, else `pitch_bend` /
    /// `aftertouch` select the wheel or key pressure. All unset = unbound
    /// (still learning).
    MidiLearn { cc: Option<u8>, pitch_bend: bool, aftertouch: bool },
}

pub struct SaveModulator {
    pub source: SaveModSource,
    pub targets: Vec<SaveModTarget>,
    /// Where the modulator is drawn.
    pub ui: Option<NodeUi>,
}

/// Data needed to serialize a modulation target for saving.
pub struct SaveModTarget {
    pub kind: crate::plugin::chain::ModTargetKind,
    pub label: String,
    pub depth: f32,
    /// Chain slot the target lives on (0 = instrument, 1..N = effect).
    pub slot: usize,
}

/// Data needed to serialize an instrument plugin for saving.
pub struct SaveInstrument {
    pub plugin: String,
    pub volume: f32,
    pub preset: Option<String>,
    pub params: Vec<(String, f32)>,
    /// The parameters a front-end shows as knobs on the node, by name, in order.
    pub pinned: Vec<String>,
    pub pitch_bend_range: f64,
    pub remap: HashMap<String, RemapTarget>,
}

/// Data needed to serialize an effect slot for saving.
pub struct SaveEffect {
    pub plugin: String,
    pub mix: f32,
    pub preset: Option<String>,
    pub params: Vec<(String, f32)>,
    /// The parameters a front-end shows as knobs on the node, by name, in order.
    pub pinned: Vec<String>,
    /// Where the effect is drawn.
    pub ui: Option<NodeUi>,
}

/// Serialization output: flat [[instrument]] format.
#[derive(Serialize)]
struct SessionOut {
    #[serde(rename = "instrument")]
    instruments: Vec<InstrumentSlotOut>,
    #[serde(skip_serializing_if = "Vec::is_empty", rename = "group")]
    groups: Vec<GroupOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    piano: Option<PianoOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ui: Option<UiOut>,
}

/// The top-level `[ui]` table: where the master mix is drawn.
#[derive(Serialize)]
struct UiOut {
    master: NodeUiOut,
}

/// A position as it is written: `x` and `y`, a whole number without a point.
#[derive(Serialize)]
struct NodeUiOut {
    #[serde(serialize_with = "coordinate")]
    x: f32,
    #[serde(serialize_with = "coordinate")]
    y: f32,
}

impl From<NodeUi> for NodeUiOut {
    fn from(ui: NodeUi) -> Self {
        NodeUiOut { x: ui.x, y: ui.y }
    }
}

/// A coordinate the way a person writes it: a whole number without a point, anything else as it was typed
/// (an f32 widened to f64 would print `0.10000000149011612`).
fn coordinate<S: serde::Serializer>(v: &f32, serializer: S) -> Result<S::Ok, S::Error> {
    if v.fract() == 0.0 && v.abs() < 1e9 {
        serializer.serialize_i64(*v as i64)
    } else {
        serializer.serialize_f64(clean_f64(*v))
    }
}

#[derive(Serialize)]
struct GroupOut {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "is_default_volume_f32", serialize_with = "f32_clean")]
    volume: f32,
    #[serde(skip_serializing_if = "is_default_pan_f32", serialize_with = "f32_clean")]
    pan: f32,
    #[serde(skip_serializing_if = "Vec::is_empty", rename = "effect")]
    effects: Vec<EffectOut>,
    #[serde(skip_serializing_if = "Vec::is_empty", rename = "modulator")]
    modulators: Vec<ModulatorOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ui: Option<NodeUiOut>,
}

fn is_default_volume_f32(v: &f32) -> bool {
    (*v - 1.0).abs() < f32::EPSILON
}

/// Write an f32 the way it was typed: widening 0.8_f32 to f64 would print
/// `0.800000011920929`.
fn f32_clean<S: serde::Serializer>(v: &f32, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_f64(clean_f64(*v))
}

fn f32_clean_opt<S: serde::Serializer>(v: &Option<f32>, serializer: S) -> Result<S::Ok, S::Error> {
    match v {
        Some(v) => serializer.serialize_f64(clean_f64(*v)),
        None => serializer.serialize_none(),
    }
}

fn is_default_pan_f32(v: &f32) -> bool {
    v.abs() < f32::EPSILON
}

#[derive(Serialize)]
struct PianoOut {
    #[serde(skip_serializing_if = "Option::is_none")]
    scale: Option<String>,
    /// Only written when on — highlight mode is the default.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    locked: bool,
}

/// One [[instrument]] table in the output. Plugin fields are at the top level.
#[derive(Serialize)]
struct InstrumentSlotOut {
    // Instrument plugin fields (top-level).
    #[serde(skip_serializing_if = "Option::is_none")]
    plugin: Option<String>,
    #[serde(skip_serializing_if = "is_default_volume_f32_opt", serialize_with = "f32_clean_opt")]
    volume: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    preset: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pinned: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    params: BTreeMap<String, f64>,
    #[serde(skip_serializing_if = "is_default_pitch_bend_range_opt")]
    pitch_bend_range: Option<f64>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    remap: BTreeMap<String, RemapTarget>,
    #[serde(skip_serializing_if = "Vec::is_empty", rename = "modulator")]
    modulators: Vec<ModulatorOut>,
    // Slot-level fields.
    #[serde(skip_serializing_if = "Option::is_none")]
    range: Option<String>,
    #[serde(skip_serializing_if = "is_zero_i8")]
    transpose: i8,
    #[serde(skip_serializing_if = "Option::is_none")]
    group: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty", rename = "effect")]
    effects: Vec<EffectOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pattern: Option<PatternOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ui: Option<NodeUiOut>,
}

#[derive(Serialize)]
struct PatternOut {
    bpm: f64,
    length_beats: f64,
    #[serde(skip_serializing_if = "is_true")]
    looping: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    base_note: Option<String>,
    /// Only written when "in_key" — chromatic is the default.
    #[serde(skip_serializing_if = "Option::is_none")]
    transpose: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    midi_cc: Option<u8>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    midi_pitch_bend: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    midi_aftertouch: bool,
    #[serde(skip_serializing_if = "Vec::is_empty", rename = "target")]
    targets: Vec<ModTargetOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ui: Option<NodeUiOut>,
}

fn is_lfo_type(s: &String) -> bool {
    s == "lfo"
}

#[derive(Serialize)]
struct ModTargetOut {
    #[serde(skip_serializing_if = "Option::is_none")]
    param: Option<String>,
    /// Chain slot for a plugin-param target (omitted when 0 = instrument).
    #[serde(skip_serializing_if = "Option::is_none")]
    slot: Option<usize>,
    /// (Group modulators) member ordinal a `param` target lands on.
    #[serde(skip_serializing_if = "Option::is_none")]
    member: Option<usize>,
    /// (Group modulators) bus-effect index a `param` target lands on.
    #[serde(skip_serializing_if = "Option::is_none")]
    bus: Option<usize>,
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
struct EffectOut {
    plugin: String,
    #[serde(skip_serializing_if = "is_default_mix_f32", serialize_with = "f32_clean")]
    mix: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    preset: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pinned: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    params: BTreeMap<String, f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ui: Option<NodeUiOut>,
}

fn save_mod_target_to_out(t: &SaveModTarget) -> ModTargetOut {
    use crate::plugin::chain::ModTargetKind;
    let mut out = ModTargetOut {
        param: None,
        slot: None,
        member: None,
        bus: None,
        mod_rate: None,
        mod_depth: None,
        mod_attack: None,
        mod_decay: None,
        mod_sustain: None,
        mod_release: None,
        depth: clean_f64(t.depth),
    };
    match &t.kind {
        ModTargetKind::PluginParam { .. } => {
            out.param = Some(t.label.clone());
            // 0 (instrument) is the default and omitted.
            if t.slot != 0 {
                out.slot = Some(t.slot);
            }
        }
        ModTargetKind::GroupMember { member, slot, .. } => {
            out.param = Some(t.label.clone());
            out.member = Some(*member);
            // 0 (member's instrument) is the default and omitted.
            if *slot != 0 {
                out.slot = Some(*slot);
            }
        }
        ModTargetKind::GroupBus { effect_index, .. } => {
            out.param = Some(t.label.clone());
            out.bus = Some(*effect_index);
        }
        ModTargetKind::GroupMemberMod { member, mod_index, field } => {
            use crate::plugin::chain::CrossModField;
            // Serialized as `member` (discriminator) + the matching mod_* field
            // carrying the member modulator's index.
            out.member = Some(*member);
            match field {
                CrossModField::Rate => out.mod_rate = Some(*mod_index),
                CrossModField::Attack => out.mod_attack = Some(*mod_index),
                CrossModField::Decay => out.mod_decay = Some(*mod_index),
                CrossModField::Sustain => out.mod_sustain = Some(*mod_index),
                CrossModField::Release => out.mod_release = Some(*mod_index),
                CrossModField::Depth(ti) => out.mod_depth = Some(vec![*mod_index, *ti]),
            }
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

fn is_default_volume_f32_opt(v: &Option<f32>) -> bool {
    match v {
        Some(v) => (*v - 1.0).abs() < f32::EPSILON,
        None => true,
    }
}

fn is_default_pitch_bend_range_opt(v: &Option<f64>) -> bool {
    match v {
        Some(v) => (*v - default_pitch_bend_range()).abs() < f64::EPSILON,
        None => true,
    }
}

/// Widen f32 to f64 without exposing the f32 binary mantissa as decimal noise.
/// e.g. 0.55_f32 → 0.55_f64 (not 0.550000011920929).
fn clean_f64(v: f32) -> f64 {
    v.to_string().parse::<f64>().unwrap_or(v as f64)
}

fn is_default_mix_f32(v: &f32) -> bool {
    (*v - 1.0).abs() < f32::EPSILON
}

/// Format a MIDI note number as a note name (e.g. 60 → "C4").
fn note_name(note: u8) -> String {
    crate::note_name(note)
}

fn mods_to_out(mods: &[SaveModulator]) -> Vec<ModulatorOut> {
    mods.iter()
        .map(|m| {
            let targets: Vec<ModTargetOut> = m
                .targets
                .iter()
                .map(save_mod_target_to_out)
                .collect();
            let ui = m.ui.map(NodeUiOut::from);
            match &m.source {
                SaveModSource::Lfo { waveform, rate } => ModulatorOut {
                    mod_type: "lfo".into(),
                    waveform: Some(waveform.clone()),
                    rate: Some(clean_f64(*rate)),
                    attack: None,
                    decay: None,
                    sustain: None,
                    release: None,
                    midi_cc: None,
                    midi_pitch_bend: false,
                    midi_aftertouch: false,
                    targets,
                    ui,
                },
                SaveModSource::Envelope { attack, decay, sustain, release } => ModulatorOut {
                    mod_type: "envelope".into(),
                    waveform: None,
                    rate: None,
                    attack: Some(clean_f64(*attack)),
                    decay: Some(clean_f64(*decay)),
                    sustain: Some(clean_f64(*sustain)),
                    release: Some(clean_f64(*release)),
                    midi_cc: None,
                    midi_pitch_bend: false,
                    midi_aftertouch: false,
                    targets,
                    ui,
                },
                SaveModSource::MidiLearn { cc, pitch_bend, aftertouch } => ModulatorOut {
                    mod_type: "midi".into(),
                    waveform: None,
                    rate: None,
                    attack: None,
                    decay: None,
                    sustain: None,
                    release: None,
                    midi_cc: *cc,
                    midi_pitch_bend: *pitch_bend,
                    midi_aftertouch: *aftertouch,
                    targets,
                    ui,
                },
            }
        })
        .collect()
}

fn effect_to_out(fx: &SaveEffect) -> EffectOut {
    EffectOut {
        plugin: fx.plugin.clone(),
        mix: fx.mix,
        preset: fx.preset.clone(),
        pinned: fx.pinned.clone(),
        params: fx.params.iter().map(|(k, v)| (k.clone(), clean_f64(*v))).collect(),
        ui: fx.ui.map(NodeUiOut::from),
    }
}

/// The session as TOML text, which is what `save` writes to the file.
pub fn to_toml(
    instruments: &[SaveInstrumentSlot],
    groups: &[SaveGroup],
    piano: Option<&PianoConfig>,
    master_ui: Option<NodeUi>,
) -> anyhow::Result<String> {
    let session = SessionOut {
        piano: piano.map(|p| PianoOut { scale: p.scale.clone(), locked: p.locked }),
        ui: master_ui.map(|ui| UiOut { master: ui.into() }),
        groups: groups
            .iter()
            .map(|g| GroupOut {
                name: g.name.clone(),
                volume: g.volume,
                pan: g.pan,
                effects: g.effects.iter().map(effect_to_out).collect(),
                modulators: mods_to_out(&g.modulators),
                ui: g.ui.map(NodeUiOut::from),
            })
            .collect(),
        instruments: instruments
            .iter()
            .map(|slot| {
                let (plugin, volume, preset, params, pitch_bend_range, remap) = match &slot.instrument {
                    Some(inst) => {
                        let params: BTreeMap<String, f64> = inst
                            .params
                            .iter()
                            .map(|(k, v)| (k.clone(), clean_f64(*v)))
                            .collect();
                        let remap: BTreeMap<String, RemapTarget> = inst
                            .remap
                            .iter()
                            .map(|(k, v)| {
                                (
                                    k.clone(),
                                    RemapTarget {
                                        note: v.note.clone(),
                                        detune: clean_f64(v.detune as f32),
                                    },
                                )
                            })
                            .collect();
                        (
                            Some(inst.plugin.clone()),
                            Some(inst.volume),
                            inst.preset.clone(),
                            params,
                            Some(inst.pitch_bend_range),
                            remap,
                        )
                    }
                    None => (None, None, None, BTreeMap::new(), None, BTreeMap::new()),
                };
                InstrumentSlotOut {
                    plugin,
                    volume,
                    preset,
                    pinned: slot.instrument.as_ref().map(|inst| inst.pinned.clone()).unwrap_or_default(),
                    params,
                    pitch_bend_range,
                    remap,
                    // Lane modulators serialize at the instrument level.
                    modulators: mods_to_out(&slot.modulators),
                    range: slot.range.map(format_range),
                    transpose: slot.transpose,
                    group: slot.group,
                    effects: slot.effects.iter().map(effect_to_out).collect(),
                    pattern: slot.pattern.as_ref().map(|p| {
                        PatternOut {
                            bpm: clean_f64(p.bpm),
                            length_beats: clean_f64(p.length_beats),
                            looping: p.looping,
                            base_note: p.base_note.map(note_name),
                            transpose: p.in_key.then(|| "in_key".to_string()),
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
                    ui: slot.ui.map(NodeUiOut::from),
                }
            })
            .collect(),
    };

    // Not `to_string_pretty`: it writes arrays one element per line.
    let content = toml::to_string(&session)?;
    inline_remap_entries(&content)
}

/// The key of the master mix in the top-level `[ui]` table.
const MASTER_KEY: &str = "master";

/// Rewrite every node's `ui` table as an inline one, `ui = { x = 80, y = 120 }` on the node's own table rather than a
/// table of its own below it, and the master's in the top-level `[ui]` as `master = { x, y }`.
fn inline_ui_tables(table: &mut toml_edit::Table, root: bool) {
    use toml_edit::{InlineTable, Item, Value};

    fn inlined(table: &toml_edit::Table) -> Item {
        let mut inline = InlineTable::new();
        for (key, value) in table.iter() {
            if let Some(value) = value.as_value() {
                inline.insert(key, value.clone());
            }
        }
        inline.fmt();
        Item::Value(Value::InlineTable(inline))
    }

    let keys: Vec<String> = table.iter().map(|(key, _)| key.to_string()).collect();
    for key in keys {
        let Some(item) = table.get_mut(&key) else { continue };
        match item {
            // (The master's position is one level down, in the table of the session's own settings.)
            Item::Table(child) if key == "ui" && root => {
                if let Some(Item::Table(position)) = child.get(MASTER_KEY) {
                    let inline = inlined(position);
                    child.insert(MASTER_KEY, inline);
                    // (A key that was a table's header is not written as `key = value` until it is told to be.)
                    if let Some(mut key) = child.key_mut(MASTER_KEY) {
                        key.fmt();
                    }
                }
            }
            Item::Table(child) if key == "ui" => {
                *item = inlined(child);
                if let Some(mut key) = table.key_mut(&key) {
                    key.fmt();
                }
            }
            // (Every node is an element of an array of tables, the instruments and what is under them.)
            Item::ArrayOfTables(array) => {
                for child in array.iter_mut() {
                    inline_ui_tables(child, false);
                }
            }
            _ => {}
        }
    }
}

/// Where the session given as `given` on the command line is, as a path that keeps pointing there. A
/// relative one would not: plugins that are loaded or scanned can change the working directory of the
/// whole process (some do), and the session would be saved to wherever that led.
pub fn absolute_path(given: &str) -> PathBuf {
    std::path::absolute(given).unwrap_or_else(|_| PathBuf::from(given))
}

/// The names that Windows keeps for devices: a file cannot be called one of them there, with any extension after it.
const WINDOWS_DEVICE_NAMES: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// The most bytes a file name has on the file systems that matter (ext4, APFS, NTFS count 255 of one thing or another).
const MAX_FILE_NAME_BYTES: usize = 255;

/// The file that a session is saved as when it is given the name `given`, if that is a plain file name; or the words for what is wrong with it. The
/// spaces round it go, `.toml` is added unless it ends in it, and what is left is a name and not a place: no `/` or `\` (whichever system it is
/// written for, so a folder cannot be got into a name), none of the characters that a file cannot have on one system or another (`:*?"<>|`, and the
/// controls), nothing before the extension that is not a name, none of the names that Windows keeps for itself, and at most 255 bytes. It is the one
/// rule for every front-end that lets a person name a file: a name that a page sends is a name and not a path (the design, §8), and `dir.join(name)` would
/// let an absolute one replace the directory.
pub fn plain_file_name(given: &str) -> Result<String, String> {
    let name = given.trim();
    if name.contains(['/', '\\']) {
        return Err("a file name has no folder in it: `/` and `\\` are not allowed".to_string());
    }
    if name.chars().any(|c| c.is_control() || ":*?\"<>|".contains(c)) {
        return Err("a file name cannot have `:`, `*`, `?`, `\"`, `<`, `>`, `|` or a control character".to_string());
    }
    let file = if name.ends_with(".toml") { name.to_string() } else { format!("{name}.toml") };
    // (Nothing before the extension, which is what no name at all comes to as well.)
    if file == ".toml" {
        return Err("a file name is needed".to_string());
    }
    let stem = file.split('.').next().unwrap_or_default();
    if WINDOWS_DEVICE_NAMES.contains(&stem.to_ascii_lowercase().as_str()) {
        return Err(format!("{stem} is a name that Windows keeps for itself"));
    }
    if file.len() > MAX_FILE_NAME_BYTES {
        return Err("a file name is at most 255 bytes long".to_string());
    }
    Ok(file)
}

/// Write the session to `path`, creating its directory first (a new session's
/// default home is `~/.config/tang/sessions/`). A file that is there is written over.
pub fn save(
    path: &Path,
    instruments: &[SaveInstrumentSlot],
    groups: &[SaveGroup],
    piano: Option<&PianoConfig>,
    master_ui: Option<NodeUi>,
) -> anyhow::Result<()> {
    write_session(path, instruments, groups, piano, master_ui, false)
}

/// As `save`, for a file that is not there yet: the write itself fails, with an `io::Error` of kind `AlreadyExists` in the error, if the path is taken
/// (by a file, a folder or a link, one that leads nowhere included, which it does not follow). Nothing looked at before the write can be out of date by the
/// time of it (finding f128).
pub fn save_new(
    path: &Path,
    instruments: &[SaveInstrumentSlot],
    groups: &[SaveGroup],
    piano: Option<&PianoConfig>,
    master_ui: Option<NodeUi>,
) -> anyhow::Result<()> {
    write_session(path, instruments, groups, piano, master_ui, true)
}

fn write_session(
    path: &Path,
    instruments: &[SaveInstrumentSlot],
    groups: &[SaveGroup],
    piano: Option<&PianoConfig>,
    master_ui: Option<NodeUi>,
    create_new: bool,
) -> anyhow::Result<()> {
    let content = to_toml(instruments, groups, piano, master_ui)?;
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow::anyhow!("failed to create directory {}: {e}", parent.display()))?;
        }
    }
    if create_new {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path)?;
        std::io::Write::write_all(&mut file, content.as_bytes())?;
    } else {
        std::fs::write(path, content)?;
    }
    Ok(())
}

/// Walk every `[[instrument]]` and rewrite each `remap` child so its entries
/// are inline tables — `"F#2" = { note = "F2", detune = 1.0 }` rather than the
/// default toml-rs block-table form. Non-instrument tables are untouched.
fn inline_remap_entries(input: &str) -> anyhow::Result<String> {
    use toml_edit::{DocumentMut, InlineTable, Item, Value};

    let mut doc: DocumentMut = input.parse()?;
    inline_ui_tables(doc.as_table_mut(), true);
    if let Some(Item::ArrayOfTables(arr)) = doc.get_mut("instrument") {
        for inst in arr.iter_mut() {
            if let Some(Item::Table(remap)) = inst.get_mut("remap") {
                // Drain block-table entries into inline tables paired with their MIDI pitch.
                let keys: Vec<String> = remap.iter().map(|(k, _)| k.to_string()).collect();
                let mut entries: Vec<(u8, String, InlineTable)> = Vec::new();
                for key in keys {
                    let Some(Item::Table(entry)) = remap.remove(&key) else { continue };
                    let mut inline = InlineTable::new();
                    for (k, v) in entry.iter() {
                        if let Some(val) = v.as_value() {
                            inline.insert(k, val.clone());
                        }
                    }
                    inline.fmt();
                    let pitch = parse_note_name(&key).unwrap_or(0);
                    entries.push((pitch, key, inline));
                }
                // Re-insert in pitch order so the file reads musically.
                entries.sort_by_key(|(pitch, _, _)| *pitch);
                for (_, key, inline) in entries {
                    remap.insert(&key, Item::Value(Value::InlineTable(inline)));
                }
            }
        }
    }
    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_given_by_a_relative_path_is_known_by_an_absolute_one() {
        let here = std::env::current_dir().unwrap();
        for given in ["jam.toml", "./jam.toml", "sub/jam.toml", "../jam.toml"] {
            let path = absolute_path(given);
            assert!(path.is_absolute(), "{given}: {path:?}");
            assert_eq!(path.file_name(), Path::new(given).file_name(), "{given}: it is the same file");
            assert!(path.starts_with(&here), "{given}: relative to where the program was started: {path:?}");
        }
        assert_eq!(absolute_path("sub/jam.toml"), here.join("sub/jam.toml"));
        // One that is absolute already is left as it is.
        let given = here.join("jam.toml");
        assert_eq!(absolute_path(given.to_str().unwrap()), given);
    }

    /// The words of the refusals of a file name (what `plain_file_name` says), as the page's tests have them.
    const NEEDED: &str = "a file name is needed";
    const FOLDER: &str = "a file name has no folder in it: `/` and `\\` are not allowed";
    const CHARACTERS: &str = "a file name cannot have `:`, `*`, `?`, `\"`, `<`, `>`, `|` or a control character";
    const TOO_LONG: &str = "a file name is at most 255 bytes long";

    #[test]
    fn a_plain_file_name_gets_toml_once_and_loses_the_spaces_round_it() {
        for (given, file) in [
            ("mix", "mix.toml"),
            ("mix.toml", "mix.toml"),
            ("  mix.toml  ", "mix.toml"),
            ("a.b", "a.b.toml"),
            ("my sound", "my sound.toml"),
            (".hidden", ".hidden.toml"),
            ("Ünïcode ♪", "Ünïcode ♪.toml"),
            ("MIX.TOML", "MIX.TOML.toml"),
            ("a..b", "a..b.toml"),
            ("..", "...toml"),
            ("console", "console.toml"),
            ("com0", "com0.toml"),
            ("nul2", "nul2.toml"),
        ] {
            assert_eq!(plain_file_name(given).as_deref(), Ok(file), "{given:?}");
        }
    }

    #[test]
    fn a_name_that_has_nothing_before_the_extension_is_a_name_that_is_needed() {
        for given in ["", "   ", "\t\n", ".toml", "  .toml ", " \u{a0} "] {
            assert_eq!(plain_file_name(given), Err(NEEDED.to_string()), "{given:?}");
        }
    }

    #[test]
    fn a_name_with_a_folder_in_it_is_refused_whatever_system_it_is_written_for() {
        for given in ["a/b", "/etc/passwd", "../x", "..\\x", "C:\\x", "sub/", "/", "a\\b", "./x", "x/../y"] {
            assert_eq!(plain_file_name(given), Err(FOLDER.to_string()), "{given:?}");
        }
    }

    #[test]
    fn a_name_with_a_character_that_a_file_cannot_have_on_every_system_is_refused() {
        for given in ["c:x", "a*b", "a?b", "a\"b", "a<b", "a>b", "a|b", "a\0b", "a\nb", "a\tb", "a\u{7f}b", "a\u{85}b", "\u{1b}[31m"] {
            assert_eq!(plain_file_name(given), Err(CHARACTERS.to_string()), "{given:?}");
        }
    }

    #[test]
    fn a_name_that_windows_keeps_for_itself_is_refused_whatever_follows_its_first_dot_and_in_any_case() {
        for (given, stem) in [("con", "con"), ("CON", "CON"), ("Nul.txt", "Nul"), ("prn", "prn"), ("aux.toml", "aux"), ("com1", "com1"), ("COM9", "COM9"), ("lpt1", "lpt1"), ("Lpt9.x.y", "Lpt9")] {
            assert_eq!(plain_file_name(given), Err(format!("{stem} is a name that Windows keeps for itself")), "{given:?}");
        }
    }

    #[test]
    fn a_name_is_at_most_as_long_as_a_file_system_takes_with_its_extension_counted() {
        let longest = "a".repeat(250);
        assert_eq!(plain_file_name(&longest), Ok(format!("{longest}.toml")));
        assert_eq!(plain_file_name(&format!("{longest}a")), Err(TOO_LONG.to_string()));
        // (Bytes, not characters: 'é' is two.)
        assert!(plain_file_name(&"é".repeat(125)).is_ok());
        assert_eq!(plain_file_name(&"é".repeat(126)), Err(TOO_LONG.to_string()));
        // The extension is not added twice, so a name that has it is counted with it.
        assert_eq!(plain_file_name(&format!("{longest}.toml")).map(|name| name.len()), Ok(255));
    }

    #[test]
    fn a_plain_file_name_is_one_part_of_a_path_that_is_not_a_prefix_or_a_way_up() {
        use std::path::{Component, Path};
        // What is let through is what `Path` says is one ordinary component: it joins to a directory and stays in it.
        for given in ["mix", "a.b", ".hidden", "..", "a..b", "my sound", "x.toml"] {
            let file = plain_file_name(given).unwrap();
            let parts: Vec<Component> = Path::new(&file).components().collect();
            assert!(matches!(parts.as_slice(), [Component::Normal(_)]), "{given:?} -> {file:?}: {parts:?}");
            assert_eq!(Path::new("/songs").join(&file).parent(), Some(Path::new("/songs")), "{given:?}");
        }
    }

    /// What each of some names comes to, as the web client's mock engine reads it (`web/src/client/fixtures/filenames.json`): the file it is saved as, or
    /// the words for what is wrong with it. They pin the mock's rule to this one. `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` rewrites it.
    #[test]
    fn the_web_clients_fixture_of_file_names_is_current() {
        let mut given: Vec<String> = [
            "mix", "mix.toml", "  mix.toml  ", "a.b", "my sound", ".hidden", "Ünïcode ♪", "MIX.TOML", "a..b", "..", "console", "com0", "nul2", "", "   ", "\t\n", ".toml", "  .toml ", "a/b", "/etc/passwd", "../x",
            "..\\x", "C:\\x", "sub/", "/", "a\\b", "./x", "x/../y", "c:x", "a*b", "a?b", "a\"b", "a<b", "a>b", "a|b", "a\0b", "a\nb", "a\tb", "a\u{7f}b", "a\u{85}b", "con", "CON", "Nul.txt", "prn", "aux.toml", "com1",
            "COM9", "lpt1", "Lpt9.x.y", "\u{85}x", "\u{feff}x", " \u{3000}x\u{2003} ", "x\u{a0}", "toml", "atoml",
            // (The ends of the controls, which the page's rule has to write out: the last of the first block and the ends of the second, and what is next to each.)
            "a\u{1f}b", "a\u{20}b", "a\u{7e}b", "a\u{80}b", "a\u{9e}b", "a\u{9f}b", "a\u{a0}b",
        ]
        .iter()
        .map(|name| name.to_string())
        .collect();
        given.extend([250, 251].map(|count| "a".repeat(count)));
        given.extend([125, 126].map(|count| "é".repeat(count)));
        given.push(format!("{}.toml", "a".repeat(250)));
        let names: Vec<serde_json::Value> = given
            .iter()
            .map(|given| match plain_file_name(given) {
                Ok(file) => serde_json::json!({ "given": given, "file": file }),
                Err(error) => serde_json::json!({ "given": given, "error": error }),
            })
            .collect();
        let fresh = serde_json::to_string_pretty(&serde_json::json!({ "names": names })).unwrap() + "\n";

        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/src/client/fixtures/filenames.json");
        if std::env::var_os("TANG_UPDATE_TYPES").is_some() {
            std::fs::write(&path, &fresh).unwrap();
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            committed == fresh,
            "{} is not what a file name comes to now: run `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` and commit the result",
            path.display()
        );
    }

    #[test]
    fn a_number_that_is_not_a_number_is_refused_wherever_it_is_and_the_error_says_where() {
        // TOML has `nan` and `inf`. As a volume, a rate or a depth they would be in the
        // model and in the audio thread, and a snapshot with a NaN in it is not even JSON.
        let osc = "[[instrument]]\nplugin = \"builtin:osc\"\n";
        for (text, place) in [
            (format!("{osc}volume = nan\n"), "instrument[0].volume"),
            (format!("bpm = inf\n{osc}"), "bpm"),
            (format!("{osc}[instrument.params]\ncutoff = -inf\n"), "instrument[0].params.cutoff"),
            (format!("{osc}[[instrument.effect]]\nplugin = \"builtin:reverb\"\nmix = nan\n"), "instrument[0].effect[0].mix"),
            (format!("{osc}[[instrument.modulator]]\nrate = nan\n"), "instrument[0].modulator[0].rate"),
            (format!("[[group]]\npan = nan\n{osc}"), "group[0].pan"),
            (format!("{osc}[instrument.pattern]\nbpm = nan\n"), "instrument[0].pattern.bpm"),
        ] {
            let error = parse(&text).err().unwrap_or_else(|| panic!("accepted:\n{text}")).to_string();
            assert!(error.contains(place) && error.contains("is not a finite number"), "{error}\nfor:\n{text}");
        }
    }

    #[test]
    fn a_number_too_big_for_the_audio_thread_is_refused_like_one_that_is_not_a_number() {
        // TOML's floats are 64 bits and the model's are 32: 1e39 is finite to the one and `inf` to the other, and
        // it would be in the model as that, and in a snapshot as `null`.
        let osc = "[[instrument]]\nplugin = \"builtin:osc\"\n";
        for (text, place) in [
            (format!("{osc}volume = 1e39\n"), "instrument[0].volume"),
            (format!("{osc}[[instrument.modulator]]\nrate = 1e39\n"), "instrument[0].modulator[0].rate"),
            (format!("{osc}[[instrument.modulator]]\n[[instrument.modulator.target]]\nparam = \"detune\"\ndepth = -1e39\n"), "instrument[0].modulator[0].target[0].depth"),
            (format!("[[group]]\nvolume = 4e38\n{osc}"), "group[0].volume"),
        ] {
            let error = parse(&text).err().unwrap_or_else(|| panic!("accepted:\n{text}")).to_string();
            assert!(error.contains(place) && error.contains("32-bit"), "{error}\nfor:\n{text}");
        }
        // The largest the audio thread holds is as good as any other number, and so is a very small one.
        assert!(parse(&format!("{osc}volume = 3.4e38\n")).is_ok());
        assert!(parse(&format!("{osc}volume = 1e-50\n")).is_ok());
    }

    #[test]
    fn a_word_that_looks_like_a_number_that_is_not_one_is_only_a_word() {
        let config = parse("[[instrument]]\nplugin = \"builtin:osc\"\npreset = \"nan\"\nvolume = 1e2\n").unwrap();
        assert_eq!(config.instruments[0].instrument.as_ref().map(|i| (i.preset.as_deref(), i.volume)), Some((Some("nan"), 100.0)));
    }

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
    fn parse_range_open_high() {
        // "C4-" means C4 and up, to the top of the MIDI range.
        assert_eq!(parse_range("C4-").unwrap(), (60, 127));
    }

    #[test]
    fn parse_range_open_low() {
        // "-B3" means everything up to and including B3.
        assert_eq!(parse_range("-B3").unwrap(), (0, 59));
    }

    #[test]
    fn parse_range_invalid_format() {
        assert!(parse_range("C4").is_err());
        assert!(parse_range("C4-B3-C5").is_err());
        // Both bounds omitted is not a range.
        assert!(parse_range("-").is_err());
    }

    #[test]
    fn parse_range_negative_octave() {
        // A '-' that is part of an octave -1 note name is not the separator.
        assert_eq!(parse_range("C-1-B3").unwrap(), (0, 59));
        assert_eq!(parse_range("D-1-B-1").unwrap(), (2, 11));
        assert_eq!(parse_range("B-1-").unwrap(), (11, 127));
        assert_eq!(parse_range("-C-1").unwrap(), (0, 0));
    }

    #[test]
    fn format_range_round_trips() {
        // Open-ended forms are preserved on save.
        assert_eq!(format_range((0, 59)), "-B3");
        assert_eq!(format_range((60, 127)), "C4-");
        assert_eq!(format_range((12, 59)), "C0-B3");
        // Bounds in octave -1 survive a save/load cycle.
        for range in [(0, 59), (2, 11), (60, 127), (0, 127), (12, 59)] {
            assert_eq!(parse_range(&format_range(range)).unwrap(), range);
        }
    }

    #[test]
    fn load_new_format() {
        let toml = r#"
[[instrument]]
plugin = "builtin:sine"
range = "C0-B3"

[[instrument]]
plugin = "builtin:sine"
range = "C4-C8"
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.toml");
        std::fs::write(&path, toml).unwrap();

        let config = load(path.to_str().unwrap()).unwrap();
        assert_eq!(config.instruments.len(), 2);
        assert_eq!(config.instruments[0].range, Some((12, 59)));
        assert_eq!(config.instruments[1].range, Some((60, 108)));
    }

    #[test]
    fn save_and_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("saved.toml");

        let instruments = vec![
            SaveInstrumentSlot {
                range: Some((12, 59)), // C0-B3
                transpose: 0,
                group: None,
                modulators: vec![],
                instrument: Some(SaveInstrument {
                    pinned: vec![],
                    plugin: "builtin:sine".into(),
                    volume: 0.8,
                    preset: None,
                    params: vec![("cutoff".into(), 0.75)],
                    pitch_bend_range: 2.0,
                    remap: HashMap::new(),
                }),
                effects: vec![SaveEffect {
                    pinned: vec![],
                    plugin: "builtin:sine".into(),
                    mix: 0.5,
                    preset: None,
                    params: vec![],
                    ui: None,
                }],
                pattern: None,
                ui: None,
            },
            SaveInstrumentSlot {
                range: None,
                transpose: 0,
                group: None,
                modulators: vec![],
                instrument: Some(SaveInstrument {
                    pinned: vec![],
                    plugin: "builtin:sine".into(),
                    volume: 1.0,
                    preset: None,
                    params: vec![],
                    pitch_bend_range: 2.0,
                    remap: HashMap::new(),
                }),
                effects: vec![],
                pattern: None,
                ui: None,
            },
        ];

        save(&path, &instruments, &[], None, None).unwrap();

        // Reload and verify
        let config = load(path.to_str().unwrap()).unwrap();
        assert_eq!(config.instruments.len(), 2);
        assert_eq!(config.instruments[0].range, Some((12, 59)));
        let inst = config.instruments[0].instrument.as_ref().unwrap();
        assert_eq!(inst.plugin, "builtin:sine");
        assert!((inst.volume - 0.8).abs() < 0.01);
        assert_eq!(config.instruments[0].effects.len(), 1);
        assert!((config.instruments[0].effects[0].mix - 0.5).abs() < 0.01);
        assert!(config.instruments[1].range.is_none());
    }

    #[test]
    fn save_and_reload_with_preset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preset_test.toml");

        let instruments = vec![SaveInstrumentSlot {
            range: None,
            transpose: 0,
            group: None,
            modulators: vec![],
            instrument: Some(SaveInstrument {
                pinned: vec![],
                plugin: "builtin:sine".into(),
                volume: 1.0,
                preset: Some("Lead".into()),
                params: vec![],
                pitch_bend_range: 2.0,
                remap: HashMap::new(),
            }),
            effects: vec![SaveEffect {
                pinned: vec![],
                plugin: "builtin:reverb".into(),
                mix: 0.4,
                preset: Some("Arcadia Dream Hall".into()),
                params: vec![],
                ui: None,
            }],
            pattern: None,
            ui: None,
        }];

        save(&path, &instruments, &[], None, None).unwrap();

        let config = load(path.to_str().unwrap()).unwrap();
        assert_eq!(config.instruments.len(), 1);
        let inst = config.instruments[0].instrument.as_ref().unwrap();
        assert_eq!(inst.preset.as_deref(), Some("Lead"));
        assert_eq!(config.instruments[0].effects.len(), 1);
        assert_eq!(
            config.instruments[0].effects[0].preset.as_deref(),
            Some("Arcadia Dream Hall")
        );
    }

    #[test]
    fn save_and_reload_with_modulators() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mod_test.toml");

        let instruments = vec![SaveInstrumentSlot {
            range: None,
            transpose: 0,
            group: None,
            modulators: vec![SaveModulator {
                source: SaveModSource::Lfo {
                    waveform: "sine".into(),
                    rate: 2.5,
                },
                targets: vec![SaveModTarget {
                    kind: crate::plugin::chain::ModTargetKind::PluginParam { slot: 0, param_index: 0 },
                    label: "cutoff".into(),
                    depth: 0.75,
                    slot: 0,
                }],
                ui: None,
            }],
            instrument: Some(SaveInstrument {
                pinned: vec![],
                plugin: "builtin:sine".into(),
                volume: 1.0,
                preset: None,
                params: vec![],
                pitch_bend_range: 2.0,
                remap: HashMap::new(),
            }),
            effects: vec![],
            pattern: None,
            ui: None,
        }];

        save(&path, &instruments, &[], None, None).unwrap();

        let config = load(path.to_str().unwrap()).unwrap();
        assert_eq!(config.instruments.len(), 1);
        let inst = &config.instruments[0];
        assert_eq!(inst.modulators.len(), 1);
        let m = &inst.modulators[0];
        assert_eq!(m.waveform, "sine");
        assert!((m.rate - 2.5).abs() < 0.01);
        assert_eq!(m.targets.len(), 1);
        assert_eq!(m.targets[0].param.as_deref(), Some("cutoff"));
        assert!((m.targets[0].depth - 0.75).abs() < 0.01);
        // Slot defaults to instrument (0).
        assert_eq!(m.targets[0].slot, 0);
    }

    #[test]
    fn save_and_reload_midi_learn_modulator() {
        // A MIDI-learned modulator (bound to CC 74) must round-trip as
        // `type = "midi"` + `midi_cc = 74`.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("midi_learn.toml");

        let instruments = vec![SaveInstrumentSlot {
            range: None,
            transpose: 0,
            group: None,
            modulators: vec![SaveModulator {
                source: SaveModSource::MidiLearn { cc: Some(74), pitch_bend: false, aftertouch: false },
                targets: vec![SaveModTarget {
                    kind: crate::plugin::chain::ModTargetKind::PluginParam { slot: 0, param_index: 0 },
                    label: "cutoff".into(),
                    depth: 1.0,
                    slot: 0,
                }],
                ui: None,
            }],
            instrument: Some(SaveInstrument {
                pinned: vec![],
                plugin: "builtin:filter".into(),
                volume: 1.0,
                preset: None,
                params: vec![],
                pitch_bend_range: 2.0,
                remap: HashMap::new(),
            }),
            effects: vec![],
            pattern: None,
            ui: None,
        }];

        save(&path, &instruments, &[], None, None).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("type = \"midi\""), "expected midi modulator:\n{raw}");
        assert!(raw.contains("midi_cc = 74"), "expected bound CC:\n{raw}");

        let config = load(path.to_str().unwrap()).unwrap();
        let inst = &config.instruments[0];
        assert_eq!(inst.modulators.len(), 1);
        let m = &inst.modulators[0];
        assert_eq!(m.mod_type, "midi");
        assert_eq!(m.midi_cc, Some(74));
        assert!(!m.midi_pitch_bend);
        assert!(!m.midi_aftertouch);
        assert_eq!(m.targets[0].param.as_deref(), Some("cutoff"));
    }

    #[test]
    fn save_and_reload_aftertouch_modulator() {
        // A MIDI-learned modulator bound to key pressure must round-trip as
        // `type = "midi"` + `midi_aftertouch = true`.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("aftertouch.toml");

        let instruments = vec![SaveInstrumentSlot {
            range: None,
            transpose: 0,
            group: None,
            modulators: vec![SaveModulator {
                source: SaveModSource::MidiLearn { cc: None, pitch_bend: false, aftertouch: true },
                targets: vec![SaveModTarget {
                    kind: crate::plugin::chain::ModTargetKind::PluginParam { slot: 0, param_index: 0 },
                    label: "cutoff".into(),
                    depth: 1.0,
                    slot: 0,
                }],
                ui: None,
            }],
            instrument: Some(SaveInstrument {
                pinned: vec![],
                plugin: "builtin:filter".into(),
                volume: 1.0,
                preset: None,
                params: vec![],
                pitch_bend_range: 2.0,
                remap: HashMap::new(),
            }),
            effects: vec![],
            pattern: None,
            ui: None,
        }];

        save(&path, &instruments, &[], None, None).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("midi_aftertouch = true"), "expected aftertouch binding:\n{raw}");

        let config = load(path.to_str().unwrap()).unwrap();
        let m = &config.instruments[0].modulators[0];
        assert_eq!(m.mod_type, "midi");
        assert!(m.midi_aftertouch);
        assert!(m.midi_cc.is_none());
        assert!(!m.midi_pitch_bend);
    }

    #[test]
    fn save_and_reload_cross_slot_modulator() {
        // A lane modulator targeting an effect (slot 1) must round-trip the slot.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cross_slot.toml");

        let instruments = vec![SaveInstrumentSlot {
            range: None,
            transpose: 0,
            group: None,
            modulators: vec![SaveModulator {
                source: SaveModSource::Lfo { waveform: "sine".into(), rate: 1.0 },
                targets: vec![SaveModTarget {
                    kind: crate::plugin::chain::ModTargetKind::PluginParam { slot: 1, param_index: 0 },
                    label: "cutoff".into(),
                    depth: 0.5,
                    slot: 1,
                }],
                ui: None,
            }],
            instrument: Some(SaveInstrument {
                pinned: vec![],
                plugin: "builtin:osc".into(),
                volume: 1.0,
                preset: None,
                params: vec![],
                pitch_bend_range: 2.0,
                remap: HashMap::new(),
            }),
            effects: vec![SaveEffect {
                pinned: vec![],
                plugin: "builtin:filter".into(),
                mix: 1.0,
                preset: None,
                params: vec![],
                ui: None,
            }],
            pattern: None,
            ui: None,
        }];
        save(&path, &instruments, &[], None, None).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("slot = 1"), "saved target should record its slot:\n{raw}");

        let config = load(path.to_str().unwrap()).unwrap();
        let inst = &config.instruments[0];
        assert_eq!(inst.modulators.len(), 1);
        assert_eq!(inst.modulators[0].targets[0].slot, 1);
        assert_eq!(inst.modulators[0].targets[0].param.as_deref(), Some("cutoff"));
    }

    #[test]
    fn save_and_reload_group() {
        // A group with a bus effect + volume, and two member instruments,
        // must round-trip (membership index + group fields).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("group.toml");

        let member = |group: Option<usize>| SaveInstrumentSlot {
            range: None,
            transpose: 0,
            group,
            modulators: vec![],
            instrument: Some(SaveInstrument {
                pinned: vec![],
                plugin: "builtin:osc".into(),
                volume: 1.0,
                preset: None,
                params: vec![],
                pitch_bend_range: 2.0,
                remap: HashMap::new(),
            }),
            effects: vec![],
            pattern: None,
            ui: None,
        };
        let instruments = vec![member(Some(0)), member(Some(0)), member(None)];
        let groups = vec![SaveGroup {
            name: Some("Pad".into()),
            volume: 0.5,
            pan: -0.5,
            effects: vec![SaveEffect {
                pinned: vec![],
                plugin: "builtin:reverb".into(),
                mix: 1.0,
                preset: None,
                params: vec![],
                ui: None,
            }],
            modulators: vec![],
            ui: None,
        }];
        save(&path, &instruments, &groups, None, None).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("[[group]]"), "expected a [[group]] section:\n{raw}");
        assert!(raw.contains("group = 0"), "members should record group index:\n{raw}");
        assert!(raw.contains("pan = -0.5"), "group pan should serialize:\n{raw}");

        let config = load(path.to_str().unwrap()).unwrap();
        assert_eq!(config.groups.len(), 1);
        assert_eq!(config.groups[0].name.as_deref(), Some("Pad"));
        assert!((config.groups[0].volume - 0.5).abs() < 1e-6);
        assert!((config.groups[0].pan + 0.5).abs() < 1e-6);
        assert_eq!(config.groups[0].effects.len(), 1);
        assert_eq!(config.instruments[0].group, Some(0));
        assert_eq!(config.instruments[1].group, Some(0));
        assert_eq!(config.instruments[2].group, None);
    }

    #[test]
    fn save_and_reload_group_modulator() {
        // A group-scoped modulator with a member target and a bus target must
        // round-trip through `[[group.modulator]]` (member/bus + param name).
        use crate::plugin::chain::ModTargetKind;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("group_mod.toml");

        let member = SaveInstrumentSlot {
            range: None,
            transpose: 0,
            group: Some(0),
            modulators: vec![],
            instrument: Some(SaveInstrument {
                pinned: vec![],
                plugin: "builtin:osc".into(),
                volume: 1.0,
                preset: None,
                params: vec![],
                pitch_bend_range: 2.0,
                remap: HashMap::new(),
            }),
            effects: vec![],
            pattern: None,
            ui: None,
        };
        let groups = vec![SaveGroup {
            name: None,
            volume: 1.0,
            pan: 0.0,
            effects: vec![SaveEffect {
                pinned: vec![],
                plugin: "builtin:filter".into(),
                mix: 1.0,
                preset: None,
                params: vec![],
                ui: None,
            }],
            modulators: vec![SaveModulator {
                source: SaveModSource::Lfo { waveform: "sine".into(), rate: 2.0 },
                targets: vec![
                    SaveModTarget {
                        kind: ModTargetKind::GroupMember { member: 0, slot: 0, param_index: 0 },
                        label: "cutoff".into(),
                        depth: 0.5,
                        slot: 0,
                    },
                    SaveModTarget {
                        kind: ModTargetKind::GroupBus { effect_index: 0, param_index: 0 },
                        label: "cutoff".into(),
                        depth: 0.25,
                        slot: 0,
                    },
                    // Group→member-modulator cross-mod: member 0's modulator 0 rate.
                    SaveModTarget {
                        kind: ModTargetKind::GroupMemberMod {
                            member: 0,
                            mod_index: 0,
                            field: crate::plugin::chain::CrossModField::Rate,
                        },
                        label: "M0 Mod0 rate".into(),
                        depth: 0.3,
                        slot: 0,
                    },
                ],
                ui: None,
            }],
            ui: None,
        }];
        save(&path, &[member], &groups, None, None).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("[[group.modulator]]"), "expected group modulator:\n{raw}");
        assert!(raw.contains("member = 0"), "member target should record member:\n{raw}");
        assert!(raw.contains("bus = 0"), "bus target should record bus index:\n{raw}");

        let config = load(path.to_str().unwrap()).unwrap();
        assert_eq!(config.groups[0].modulators.len(), 1);
        let m = &config.groups[0].modulators[0];
        assert_eq!(m.targets.len(), 3);
        assert_eq!(m.targets[0].member, Some(0));
        assert_eq!(m.targets[0].param.as_deref(), Some("cutoff"));
        assert_eq!(m.targets[1].bus, Some(0));
        assert_eq!(m.targets[1].param.as_deref(), Some("cutoff"));
        // The member-modulator target: member set, mod_rate carries the index,
        // and no param name.
        assert_eq!(m.targets[2].member, Some(0));
        assert_eq!(m.targets[2].mod_rate, Some(0));
        assert_eq!(m.targets[2].param, None);
    }

    #[test]
    fn save_and_reload_with_remap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("remap_test.toml");

        let mut remap = HashMap::new();
        // Insert out of pitch order on purpose; saved file should still be in pitch order.
        remap.insert(
            "G#6".into(),
            RemapTarget { note: "G6".into(), detune: 1.0 },
        );
        remap.insert(
            "F#2".into(),
            RemapTarget { note: "F2".into(), detune: 1.0 },
        );
        remap.insert(
            "C#6".into(),
            RemapTarget { note: "C6".into(), detune: 1.0 },
        );

        let instruments = vec![SaveInstrumentSlot {
            range: None,
            transpose: 0,
            group: None,
            modulators: vec![],
            instrument: Some(SaveInstrument {
                pinned: vec![],
                plugin: "builtin:sine".into(),
                volume: 1.0,
                preset: None,
                params: vec![],
                pitch_bend_range: 2.0,
                remap,
            }),
            effects: vec![],
            pattern: None,
            ui: None,
        }];

        save(&path, &instruments, &[], None, None).unwrap();

        // Output should use inline tables and stable alphabetical ordering.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            raw.contains(r#""F#2" = { note = "F2", detune = 1.0 }"#)
                && !raw.contains(r#"[instrument.remap."F#2"]"#),
            "expected inline table for F#2, got:\n{raw}"
        );
        // Saved file should be ordered by MIDI pitch: F#2 < C#6 < G#6.
        let f_pos = raw.find("F#2").unwrap();
        let c_pos = raw.find("C#6").unwrap();
        let g_pos = raw.find("G#6").unwrap();
        assert!(
            f_pos < c_pos && c_pos < g_pos,
            "remap entries not in pitch order:\n{raw}"
        );

        let config = load(path.to_str().unwrap()).unwrap();
        let inst = config.instruments[0].instrument.as_ref().unwrap();
        assert_eq!(inst.remap.len(), 3);
        assert_eq!(inst.remap["F#2"].note, "F2");
        assert!((inst.remap["F#2"].detune - 1.0).abs() < f64::EPSILON);
        assert_eq!(inst.remap["G#6"].note, "G6");
    }

    /// A session with a position on every kind of node, as a person or the page writes them: whole numbers or decimals.
    const PLACED: &str = r#"
[ui]
master = { x = 1200, y = 300.5 }

[[group]]
name = "Pad"
ui = { x = 800, y = 40 }

[[group.effect]]
plugin = "builtin:reverb"
ui = { x = 1100, y = 40 }

[[group.modulator]]
ui = { x = 800, y = -120 }

[[instrument]]
plugin = "builtin:osc"
group = 0
ui = { x = 0, y = 0 }

[[instrument.effect]]
plugin = "builtin:filter"
ui = { x = 376, y = 0.25 }

[[instrument.modulator]]
type = "envelope"
ui = { x = 376, y = -200 }

[[instrument]]
plugin = "builtin:osc"
"#;

    #[test]
    fn a_position_is_read_on_every_kind_of_node_and_a_node_without_one_has_none() {
        let config = parse(PLACED).unwrap();
        let at = |x: f32, y: f32| Some(NodeUi { x, y });
        assert_eq!(config.master_ui, at(1200.0, 300.5));
        assert_eq!(config.groups[0].ui, at(800.0, 40.0));
        assert_eq!(config.groups[0].effects[0].ui, at(1100.0, 40.0));
        assert_eq!(config.groups[0].modulators[0].ui, at(800.0, -120.0));
        assert_eq!(config.instruments[0].ui, at(0.0, 0.0));
        assert_eq!(config.instruments[0].effects[0].ui, at(376.0, 0.25));
        assert_eq!(config.instruments[0].modulators[0].ui, at(376.0, -200.0));
        assert_eq!(config.instruments[1].ui, None);
        // (A session from before there were positions has none.)
        let old = parse("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n").unwrap();
        assert_eq!((old.master_ui, old.instruments[0].ui, old.instruments[0].effects[0].ui), (None, None, None));
    }

    #[test]
    fn a_position_that_is_not_a_pair_of_numbers_is_ignored_and_the_rest_of_the_session_loads() {
        let osc = "[[instrument]]\nplugin = \"builtin:osc\"\n";
        for text in ["ui = { x = 1 }", "ui = 5", "ui = { x = \"a\", y = 2 }", "ui = []", "ui = {}"] {
            let config = parse(&format!("{osc}{text}\n")).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!((config.instruments.len(), config.instruments[0].ui), (1, None), "{text}");
        }
        let config = parse(&format!("{osc}[[instrument.effect]]\nplugin = \"builtin:filter\"\nui = \"here\"\n[ui]\nmaster = 3\n")).unwrap();
        assert_eq!((config.instruments[0].effects[0].ui, config.master_ui), (None, None));
        // (Numbers that are not numbers are refused wherever they are, as every number is.)
        assert!(parse(&format!("{osc}ui = {{ x = nan, y = 1 }}\n")).is_err());
    }

    /// An effect with nothing set, for the tests of what is written for one thing.
    fn bare_effect(plugin: &str) -> SaveEffect {
        SaveEffect { plugin: plugin.into(), mix: 1.0, preset: None, params: vec![], pinned: vec![], ui: None }
    }

    /// A lane with an effect and a modulator, each with this position, for the tests of what is written.
    fn placed_lane(ui: Option<NodeUi>) -> SaveInstrumentSlot {
        SaveInstrumentSlot {
            range: None,
            transpose: 0,
            group: None,
            modulators: vec![SaveModulator { source: SaveModSource::Lfo { waveform: "sine".into(), rate: 1.0 }, targets: vec![], ui }],
            instrument: Some(SaveInstrument { pinned: vec![], plugin: "builtin:osc".into(), volume: 1.0, preset: None, params: vec![], pitch_bend_range: 2.0, remap: HashMap::new() }),
            effects: vec![SaveEffect { pinned: vec![], plugin: "builtin:filter".into(), mix: 1.0, preset: None, params: vec![], ui }],
            pattern: None,
            ui,
        }
    }

    #[test]
    fn positions_are_written_on_their_nodes_as_inline_tables_the_master_in_a_table_of_its_own_and_read_back() {
        let at = |x: f32, y: f32| Some(NodeUi { x, y });
        let group = SaveGroup {
            name: None,
            volume: 1.0,
            pan: 0.0,
            effects: vec![SaveEffect { pinned: vec![], plugin: "builtin:reverb".into(), mix: 1.0, preset: None, params: vec![], ui: at(1100.0, 40.0) }],
            modulators: vec![SaveModulator { source: SaveModSource::Lfo { waveform: "sine".into(), rate: 1.0 }, targets: vec![], ui: at(800.0, -120.5) }],
            ui: at(800.0, 40.0),
        };
        let text = to_toml(&[placed_lane(at(80.0, 120.0))], &[group], None, at(1200.0, 300.0)).unwrap();
        // Whole numbers are written without a point, and it is one line for a node, in the table of the node.
        assert!(text.contains("ui = { x = 80, y = 120 }"), "{text}");
        assert_eq!(text.matches("ui = { x = 80, y = 120 }").count(), 3, "the lane, its effect and its modulator: {text}");
        assert!(text.contains("ui = { x = 800, y = -120.5 }"), "{text}");
        assert!(text.contains("[ui]\nmaster = { x = 1200, y = 300 }"), "the master has a table of its own: {text}");
        // And what is written is read back, where it was.
        let config = parse(&text).unwrap();
        assert_eq!(config.master_ui, at(1200.0, 300.0));
        assert_eq!(config.groups[0].ui, at(800.0, 40.0));
        assert_eq!(config.groups[0].effects[0].ui, at(1100.0, 40.0));
        assert_eq!(config.groups[0].modulators[0].ui, at(800.0, -120.5));
        assert_eq!(config.instruments[0].ui, at(80.0, 120.0));
        assert_eq!(config.instruments[0].effects[0].ui, at(80.0, 120.0));
        assert_eq!(config.instruments[0].modulators[0].ui, at(80.0, 120.0));
    }

    #[test]
    fn a_pinned_list_is_read_on_an_instrument_an_effect_and_a_bus_effect_and_a_node_without_one_has_none() {
        let config = parse(crate::app::testutil::PINNED).unwrap();
        fn names(list: &[String]) -> Vec<&str> {
            list.iter().map(String::as_str).collect()
        }
        assert_eq!(names(&config.instruments[0].instrument.as_ref().unwrap().pinned), ["sustain", "detune", "waveform"]);
        assert_eq!(names(&config.instruments[0].effects[0].pinned), ["resonance", "no such parameter", "resonance", "cutoff"]);
        assert_eq!(names(&config.groups[0].effects[0].pinned), ["drive", "cutoff"]);
        assert!(config.instruments[1].instrument.as_ref().unwrap().pinned.is_empty());
        // (A session from before there were pins has none.)
        let old = parse("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n").unwrap();
        assert!(old.instruments[0].instrument.as_ref().unwrap().pinned.is_empty() && old.instruments[0].effects[0].pinned.is_empty());
    }

    #[test]
    fn a_pinned_list_that_is_not_a_list_of_names_is_ignored_where_it_is_wrong_and_the_rest_of_the_session_loads() {
        let osc = "[[instrument]]\nplugin = \"builtin:osc\"\n";
        let cases: [(&str, &[&str]); 7] = [
            ("pinned = 5", &[]),
            ("pinned = \"cutoff\"", &[]),
            ("pinned = {}", &[]),
            ("pinned = [1, 2]", &[]),
            ("pinned = [\"a\", 2, \"b\"]", &["a", "b"]),
            ("pinned = [[\"a\"]]", &[]),
            ("pinned = []", &[]),
        ];
        for (text, expected) in cases {
            let config = parse(&format!("{osc}{text}\n")).unwrap_or_else(|e| panic!("{text}: {e}"));
            let pinned: Vec<&str> = config.instruments[0].instrument.as_ref().unwrap().pinned.iter().map(String::as_str).collect();
            assert_eq!((config.instruments.len(), pinned), (1, expected.to_vec()), "{text}");
        }
        let config = parse(&format!("{osc}[[instrument.effect]]\nplugin = \"builtin:filter\"\npinned = false\n")).unwrap();
        assert_eq!((config.instruments.len(), config.instruments[0].effects[0].pinned.len()), (1, 0));
    }

    #[test]
    fn pinned_parameters_are_written_by_name_on_the_plugin_they_are_for_and_read_back_and_none_are_written_for_none() {
        let mut lane = placed_lane(None);
        lane.instrument.as_mut().unwrap().pinned = vec!["sustain".into(), "detune".into()];
        lane.effects[0].pinned = vec!["cutoff".into()];
        let group = SaveGroup {
            name: None,
            volume: 1.0,
            pan: 0.0,
            effects: vec![SaveEffect { pinned: vec!["mix".into(), "width".into()], ..bare_effect("builtin:reverb") }],
            modulators: vec![],
            ui: None,
        };
        let text = to_toml(&[lane], &[group], None, None).unwrap();
        for line in [r#"pinned = ["sustain", "detune"]"#, r#"pinned = ["cutoff"]"#, r#"pinned = ["mix", "width"]"#] {
            assert_eq!(text.lines().filter(|l| *l == line).count(), 1, "{line} in\n{text}");
        }
        assert_eq!(text.lines().filter(|l| l.starts_with("pinned")).count(), 3, "{text}");
        // And what is written is read back, on the same plugins and in the same order.
        let config = parse(&text).unwrap();
        assert_eq!(config.instruments[0].instrument.as_ref().unwrap().pinned, ["sustain", "detune"]);
        assert_eq!(config.instruments[0].effects[0].pinned, ["cutoff"]);
        assert_eq!(config.groups[0].effects[0].pinned, ["mix", "width"]);
        // With none pinned there is no line: a file is as it always was.
        let plain = to_toml(&[placed_lane(None)], &[], None, None).unwrap();
        assert!(!plain.contains("pinned"), "{plain}");
    }

    #[test]
    fn a_place_is_kept_to_a_tenth_of_a_pixel_half_away_from_zero_and_within_the_limits_of_the_canvas() {
        let tidy = |x: f32, y: f32| NodeUi { x, y }.tidy().map(|ui| (ui.x, ui.y));
        assert_eq!(tidy(376.382, -40.04), Some((376.4, -40.0)));
        assert_eq!(tidy(0.25, -0.25), Some((0.3, -0.3)));
        assert_eq!(tidy(0.05, -0.05), Some((0.1, -0.1)));
        assert_eq!(tidy(80.0, 120.0), Some((80.0, 120.0)), "a place that is tidy is as it was");
        assert_eq!(tidy(1.0e9, -1.0e9), Some((NodeUi::LIMIT as f32, -(NodeUi::LIMIT as f32))));
        assert_eq!(tidy(f32::MAX, f32::MIN), Some((100_000.0, -100_000.0)));
        // (No `-0`: it would be written, to a file and to a client, as it is.)
        let (x, y) = tidy(-0.04, -0.0).unwrap();
        assert!(x.is_sign_positive() && y.is_sign_positive(), "{x} {y}");
        // Numbers that are not numbers are not a place.
        for (x, y) in [(f32::NAN, 1.0), (1.0, f32::NAN), (f32::INFINITY, 1.0), (1.0, f32::NEG_INFINITY)] {
            assert_eq!(tidy(x, y), None, "{x} {y}");
        }
    }

    #[test]
    fn a_node_without_a_position_is_written_as_it_always_was() {
        let text = to_toml(&[placed_lane(None)], &[], None, None).unwrap();
        assert!(!text.lines().any(|line| line.starts_with("ui ") || line == "[ui]"), "{text}");
        // (The same session with positions has the same lines besides the positions.)
        let placed = to_toml(&[placed_lane(Some(NodeUi { x: 1.0, y: 2.0 }))], &[], None, None).unwrap();
        let without: Vec<&str> = placed.lines().filter(|line| !line.starts_with("ui = ")).collect();
        assert_eq!(without.join("\n") + "\n", text);
    }

    #[test]
    fn a_position_with_decimals_is_written_the_way_it_was_typed() {
        // (Not `0.10000000149011612`: an f32 is written as the shortest number that is it.)
        let text = to_toml(&[placed_lane(Some(NodeUi { x: 12.3, y: -0.1 }))], &[], None, None).unwrap();
        assert!(text.contains("ui = { x = 12.3, y = -0.1 }"), "{text}");
    }

    #[test]
    fn save_and_reload_with_piano_scale() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("piano.toml");

        let instruments = vec![SaveInstrumentSlot {
            range: None,
            transpose: 0,
            group: None,
            modulators: vec![],
            instrument: Some(SaveInstrument {
                pinned: vec![],
                plugin: "builtin:sine".into(),
                volume: 1.0,
                preset: None,
                params: vec![],
                pitch_bend_range: 2.0,
                remap: HashMap::new(),
            }),
            effects: vec![],
            pattern: None,
            ui: None,
        }];
        let piano = PianoConfig { scale: Some("F# dorian".into()), locked: false };
        save(&path, &instruments, &[], Some(&piano), None).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("[piano]"));
        assert!(raw.contains("scale = \"F# dorian\""));

        let config = load(path.to_str().unwrap()).unwrap();
        let piano_cfg = config.piano.expect("piano section");
        assert_eq!(piano_cfg.scale.as_deref(), Some("F# dorian"));
    }

    #[test]
    fn save_creates_the_sessions_directory() {
        // A new session's home (~/.config/tang/sessions/) does not exist before the first save.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tang").join("sessions").join("session-1.toml");
        let lane = SaveInstrumentSlot {
            range: None,
            transpose: 0,
            group: None,
            modulators: vec![],
            instrument: None,
            effects: vec![],
            pattern: None,
            ui: None,
        };
        save(&path, &[lane], &[], None, None).unwrap();
        assert_eq!(load(path.to_str().unwrap()).unwrap().instruments.len(), 1);
    }

    /// A session of one lane, for the tests of what a write does to the file system.
    fn one_lane() -> Vec<SaveInstrumentSlot> {
        vec![SaveInstrumentSlot {
            range: None,
            transpose: 0,
            group: None,
            modulators: vec![],
            instrument: Some(SaveInstrument {
                pinned: vec![],
                plugin: "builtin:sine".into(),
                volume: 1.0,
                preset: None,
                params: vec![],
                pitch_bend_range: 2.0,
                remap: HashMap::new(),
            }),
            effects: vec![],
            pattern: None,
            ui: None,
        }]
    }

    /// Whether an error that a write gave is the file system saying that the path is taken.
    fn path_is_taken(error: &anyhow::Error) -> bool {
        error.downcast_ref::<std::io::Error>().is_some_and(|error| error.kind() == std::io::ErrorKind::AlreadyExists)
    }

    #[test]
    fn a_session_saved_as_a_new_file_is_what_a_save_writes_and_is_made_where_the_directory_is_not_yet() {
        let dir = tempfile::tempdir().unwrap();
        save_new(&dir.path().join("new.toml"), &one_lane(), &[], None, None).unwrap();
        save(&dir.path().join("same.toml"), &one_lane(), &[], None, None).unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("new.toml")).unwrap(), std::fs::read_to_string(dir.path().join("same.toml")).unwrap());
        // (A new session's home is a directory that may not be there yet.)
        let nested = dir.path().join("sessions").join("new.toml");
        save_new(&nested, &one_lane(), &[], None, None).unwrap();
        assert_eq!(load(nested.to_str().unwrap()).unwrap().instruments.len(), 1);
    }

    #[test]
    fn a_session_saved_as_a_new_file_does_not_write_over_a_file_that_is_there_and_the_write_is_what_finds_it() {
        // (Finding f128: a look at the path and then a write could be out of date by the time of the write; the write itself refuses a path that is taken.)
        let dir = tempfile::tempdir().unwrap();
        let taken = dir.path().join("taken.toml");
        std::fs::write(&taken, "another session").unwrap();
        let error = save_new(&taken, &one_lane(), &[], None, None).expect_err("the file is there");
        assert!(path_is_taken(&error), "{error:#}");
        assert_eq!(std::fs::read_to_string(&taken).unwrap(), "another session", "what was there is as it was");

        // A folder is a path that is taken.
        std::fs::create_dir(dir.path().join("folder.toml")).unwrap();
        assert!(path_is_taken(&save_new(&dir.path().join("folder.toml"), &one_lane(), &[], None, None).expect_err("a folder is there")));

        // A save, which is for the session's own file, writes over it.
        save(&taken, &one_lane(), &[], None, None).unwrap();
        assert!(std::fs::read_to_string(&taken).unwrap().contains("builtin:sine"));
    }

    #[cfg(unix)]
    #[test]
    fn a_session_saved_as_a_new_file_does_not_follow_a_link_that_is_there() {
        let dir = tempfile::tempdir().unwrap();
        // A link to nowhere, and one to a file that is there: the write would follow either (and make the first one's file, or write over the second's).
        std::os::unix::fs::symlink(dir.path().join("nowhere"), dir.path().join("dangling.toml")).unwrap();
        std::fs::write(dir.path().join("precious.toml"), "keep me").unwrap();
        std::os::unix::fs::symlink(dir.path().join("precious.toml"), dir.path().join("link.toml")).unwrap();
        for link in ["dangling.toml", "link.toml"] {
            let error = save_new(&dir.path().join(link), &one_lane(), &[], None, None).expect_err("a link is a path that is taken");
            assert!(path_is_taken(&error), "{link}: {error:#}");
        }
        assert!(!dir.path().join("nowhere").exists(), "what the link leads to was not made");
        assert_eq!(std::fs::read_to_string(dir.path().join("precious.toml")).unwrap(), "keep me");
    }

    #[test]
    fn save_and_reload_piano_lock() {
        // The Piano tab's lock toggle is session state: `[piano] locked = true` is
        // read at startup, so a save has to write it back.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("locked.toml");
        let instruments = || {
            vec![SaveInstrumentSlot {
                range: None,
                transpose: 0,
                group: None,
                modulators: vec![],
                instrument: Some(SaveInstrument {
                    pinned: vec![],
                    plugin: "builtin:sine".into(),
                    volume: 1.0,
                    preset: None,
                    params: vec![],
                    pitch_bend_range: 2.0,
                    remap: HashMap::new(),
                }),
                effects: vec![],
                pattern: None,
                ui: None,
            }]
        };

        let locked = PianoConfig { scale: Some("C major".into()), locked: true };
        save(&path, &instruments(), &[], Some(&locked), None).unwrap();
        let config = load(path.to_str().unwrap()).unwrap();
        assert!(config.piano.expect("piano section").locked);

        // Highlight mode is the default and stays out of the file.
        let open = PianoConfig { scale: Some("C major".into()), locked: false };
        save(&path, &instruments(), &[], Some(&open), None).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("locked"), "{raw}");
        assert!(!load(path.to_str().unwrap()).unwrap().piano.expect("piano section").locked);
    }

    #[test]
    fn save_and_reload_pattern_transpose_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pattern.toml");

        let instruments = vec![SaveInstrumentSlot {
            range: None,
            transpose: 0,
            group: None,
            modulators: vec![],
            instrument: Some(SaveInstrument {
                pinned: vec![],
                plugin: "builtin:sine".into(),
                volume: 1.0,
                preset: None,
                params: vec![],
                pitch_bend_range: 2.0,
                remap: HashMap::new(),
            }),
            effects: vec![],
            pattern: Some(SavePattern {
                bpm: 120.0,
                length_beats: 4.0,
                looping: true,
                base_note: Some(60),
                events: vec![(0, 0x90, 60, 100), (24000, 0x80, 60, 0)],
                enabled: true,
                in_key: true,
            }),
            ui: None,
        }];
        save(&path, &instruments, &[], None, None).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("transpose = \"in_key\""));

        let config = load(path.to_str().unwrap()).unwrap();
        let pattern = config.instruments[0].pattern.as_ref().expect("pattern");
        assert!(pattern.in_key);

        // Chromatic (default) round-trips without writing the field.
        let mut instruments = instruments;
        instruments[0].pattern.as_mut().unwrap().in_key = false;
        save(&path, &instruments, &[], None, None).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("transpose"));
        let config = load(path.to_str().unwrap()).unwrap();
        assert!(!config.instruments[0].pattern.as_ref().unwrap().in_key);
    }

    #[test]
    fn pattern_transpose_rejects_unknown_value() {
        let toml = r#"
[[instrument]]
plugin = "builtin:sine"

[instrument.pattern]
transpose = "dorian"
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad_transpose.toml");
        std::fs::write(&path, toml).unwrap();
        let err = match load(path.to_str().unwrap()) {
            Err(e) => e,
            Ok(_) => panic!("expected load to fail on unknown transpose value"),
        };
        assert!(err.to_string().contains("transpose"));
    }

    #[test]
    fn load_session_with_modulators() {
        let toml = r#"
[[instrument]]
plugin = "builtin:sine"

[[instrument.modulator]]
waveform = "triangle"
rate = 0.5

[[instrument.modulator.target]]
param = "frequency"
depth = 0.3
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_mod.toml");
        std::fs::write(&path, toml).unwrap();

        let config = load(path.to_str().unwrap()).unwrap();
        let inst = &config.instruments[0];
        assert_eq!(inst.modulators.len(), 1);
        let m = &inst.modulators[0];
        assert_eq!(m.waveform, "triangle");
        assert!((m.rate - 0.5).abs() < 0.01);
        assert_eq!(m.targets.len(), 1);
        assert_eq!(m.targets[0].param.as_deref(), Some("frequency"));
        assert!((m.targets[0].depth - 0.3).abs() < 0.01);
    }
}
