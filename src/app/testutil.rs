//! Test helpers shared by the app and TUI tests.

use std::path::Path;

use crate::app::load::{Progress, load_session};
use crate::app::model::SessionModel;
use crate::plugin::chain::GraphCommand;
use crate::{plugin, session};

/// One line per command, for asserting on what a front-end or the loader sent
/// to the audio thread. `GraphCommand` carries boxed plugins, so it has no
/// `Debug`; this prints each command's variant and its scalar fields (plugins by
/// name). Commands nobody asserts on yet show as `<other>`, which still changes
/// an expected list when one shows up unexpectedly.
pub fn describe(cmd: &GraphCommand) -> String {
    use GraphCommand as C;
    match cmd {
        C::AddGroup => "AddGroup".to_string(),
        C::RemoveGroup { group } => format!("RemoveGroup group={group}"),
        C::SetGroupVolume { group, value } => format!("SetGroupVolume group={group} value={value:.3}"),
        C::SetGroupPan { group, value } => format!("SetGroupPan group={group} value={value:.3}"),
        C::SetGroupMix { group, index, value } => {
            format!("SetGroupMix group={group} fx={index} value={value:.3}")
        }
        C::InsertGroupEffect { group, index, effect, mix } => {
            format!("InsertGroupEffect group={group} index={index} plugin={} mix={mix:.3}", effect.name())
        }
        C::RemoveGroupEffect { group, index } => format!("RemoveGroupEffect group={group} index={index}"),
        C::ReorderGroupEffect { group, from, to } => {
            format!("ReorderGroupEffect group={group} from={from} to={to}")
        }
        C::SetGroupParameter { group, index, param_index, value } => {
            format!("SetGroupParameter group={group} fx={index} param={param_index} value={value:.3}")
        }
        C::InsertGroupModulator { group, index, source } => {
            format!("InsertGroupModulator group={group} index={index} source={source:?}")
        }
        C::RemoveGroupModulator { group, index } => {
            format!("RemoveGroupModulator group={group} index={index}")
        }
        C::AddGroupModTarget { group, mod_index, target } => format!(
            "AddGroupModTarget group={group} mod={mod_index} kind={:?} depth={:.3} base={:.3} range=[{:.3}, {:.3}]",
            target.kind, target.depth, target.base_value, target.param_min, target.param_max
        ),
        C::SetGroupModTargetDepth { group, mod_index, target_index, depth } => {
            format!("SetGroupModTargetDepth group={group} mod={mod_index} target={target_index} depth={depth:.3}")
        }

        C::AddInstrument { range } => format!("AddInstrument range={range:?}"),
        C::RemoveInstrument { inst } => format!("RemoveInstrument inst={inst}"),
        C::ClearInstrument { inst } => format!("ClearInstrument inst={inst}"),
        C::SetLaneGroup { inst, group } => format!("SetLaneGroup inst={inst} group={group:?}"),
        C::SetInstrumentRange { inst, range } => format!("SetInstrumentRange inst={inst} range={range:?}"),
        C::SwapInstrument { inst, instrument, inst_buf, remapper } => format!(
            "SwapInstrument inst={inst} plugin={} bufs={} remap={}",
            instrument.name(),
            inst_buf.len(),
            remapper.is_some()
        ),
        C::SetVolume { inst, value } => format!("SetVolume inst={inst} value={value:.3}"),
        C::SetTranspose { inst, semitones } => format!("SetTranspose inst={inst} semitones={semitones}"),
        C::SetParameter { inst, slot, param_index, value } => {
            format!("SetParameter inst={inst} slot={slot} param={param_index} value={value:.3}")
        }
        C::SetMix { inst, slot, value } => format!("SetMix inst={inst} slot={slot} value={value:.3}"),
        C::InsertEffect { inst, index, effect, mix } => {
            format!("InsertEffect inst={inst} index={index} plugin={} mix={mix:.3}", effect.name())
        }
        C::RemoveEffect { inst, index } => format!("RemoveEffect inst={inst} index={index}"),
        C::ReorderEffect { inst, from, to } => format!("ReorderEffect inst={inst} from={from} to={to}"),
        C::InsertModulator { inst, index, source } => {
            format!("InsertModulator inst={inst} index={index} source={source:?}")
        }
        C::RemoveModulator { inst, index } => format!("RemoveModulator inst={inst} index={index}"),
        C::AddModTarget { inst, mod_index, target } => format!(
            "AddModTarget inst={inst} mod={mod_index} kind={:?} depth={:.3} base={:.3} range=[{:.3}, {:.3}]",
            target.kind, target.depth, target.base_value, target.param_min, target.param_max
        ),
        C::SetModulatorRate { inst, mod_index, rate } => {
            format!("SetModulatorRate inst={inst} mod={mod_index} rate={rate:.3}")
        }
        C::SetModTargetDepth { inst, mod_index, target_index, depth } => {
            format!("SetModTargetDepth inst={inst} mod={mod_index} target={target_index} depth={depth:.3}")
        }

        C::SetPattern { inst, pattern, base_note, in_key } => format!(
            "SetPattern inst={inst} events={} length_samples={} base_note={base_note:?} in_key={in_key}",
            pattern.events.len(),
            pattern.length_samples
        ),
        C::SetGlobalBpm { bpm } => format!("SetGlobalBpm bpm={bpm:.1}"),
        C::SetPatternLength { inst, beats } => format!("SetPatternLength inst={inst} beats={beats:.1}"),
        C::SetPatternEnabled { inst, enabled } => format!("SetPatternEnabled inst={inst} enabled={enabled}"),
        C::SetPatternLooping { inst, looping } => format!("SetPatternLooping inst={inst} looping={looping}"),
        _ => "<other>".to_string(),
    }
}

/// What a load did: the model it returned, the commands it sent (one line
/// each, see `describe`) and the progress it reported.
#[derive(Debug)]
pub struct Run {
    pub model: SessionModel,
    pub commands: Vec<String>,
    pub progress: Vec<String>,
}

pub fn try_load(text: &str) -> anyhow::Result<Run> {
    let config = session::parse(text)?;
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut progress = Vec::new();
    let model = load_session(
        &config,
        Path::new("."),
        48_000.0,
        256,
        &plugin::Runtime::default(),
        &tx,
        &mut |p| {
            progress.push(match p {
                Progress::Loading(name) => format!("loading {name}"),
                Progress::Loaded { done } => format!("loaded {done}"),
            })
        },
    )?;
    let commands = rx.try_iter().map(|c| describe(&c)).collect();
    Ok(Run { model, commands, progress })
}

pub fn load(text: &str) -> Run {
    try_load(text).unwrap_or_else(|e| panic!("load failed: {e:#}"))
}

pub const GROUPS: &str = r#"
[[group]]
name = "Pad"
volume = 0.8
pan = -0.3

[[group.effect]]
plugin = "builtin:reverb"
mix = 0.6
[group.effect.params]
size = 0.5
bogus = 1.0

[[group.modulator]]
type = "lfo"
waveform = "triangle"
rate = 0.5
[[group.modulator.target]]
member = 0
param = "detune"
depth = 0.4
[[group.modulator.target]]
member = 1
slot = 1
param = "cutoff"
depth = -0.3
[[group.modulator.target]]
bus = 0
param = "damping"
depth = 0.25
[[group.modulator.target]]
member = 1
mod_rate = 0
depth = 0.2
[[group.modulator.target]]
param = "nope"
depth = 0.1
[[group.modulator.target]]
bus = 3
param = "damping"
depth = 0.1

[[group.modulator]]
type = "envelope"
attack = 0.02
decay = 0.4
sustain = 0.5
release = 1.0
[[group.modulator.target]]
mod_rate = 0
depth = 0.3
[[group.modulator.target]]
mod_depth = [0, 2]
depth = 0.1

[[instrument]]
plugin = "builtin:osc"
group = 0
range = "C0-B3"
[instrument.params]
waveform = 1.0

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.effect]]
plugin = "builtin:filter"
mix = 1.0
[[instrument.modulator]]
type = "lfo"
rate = 3.0

[[instrument]]
plugin = "builtin:osc"
"#;

pub const LEGACY: &str = r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.modulator]]
rate = 2.0
[[instrument.modulator.target]]
param = "detune"
depth = 0.1

[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.effect.modulator]]
rate = 3.0
[[instrument.effect.modulator.target]]
param = "cutoff"
slot = 7
depth = 0.2
[[instrument.effect.modulator]]
rate = 4.0
[[instrument.effect.modulator.target]]
mod_rate = 0
depth = 0.3
"#;

pub const MIDI: &str = r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.modulator]]
type = "midi"
midi_cc = 74
[[instrument.modulator.target]]
param = "detune"
[[instrument.modulator]]
type = "midi"
[[instrument.modulator]]
type = "midi"
midi_pitch_bend = true
[[instrument.modulator]]
type = "midi"
midi_aftertouch = true
"#;

pub const SETTINGS: &str = r#"
[[instrument]]
plugin = "builtin:osc"
range = "C2-B5"
transpose = -12
volume = 0.5
pitch_bend_range = 4.0
[instrument.params]
waveform = 2.0
bogus = 1.0
[instrument.remap]
"G#4" = { note = "G4", detune = 1.0 }
[instrument.pattern]
bpm = 90.0
length_beats = 2.0
base_note = "C4"
enabled = true
looping = false
transpose = "in_key"
[[instrument.pattern.events]]
frame = 0
status = "on"
note = "C4"
velocity = 100
[[instrument.pattern.events]]
frame = 24000
status = "off"
note = "C4"
velocity = 0

[[instrument.effect]]
plugin = "builtin:reverb"
preset = "Arcadia Dream Hall"
mix = 0.4
[instrument.effect.params]
size = 0.9
bogus = 1.0
[[instrument.effect]]
plugin = "builtin:filter"
[instrument.effect.params]
cutoff = 0.25

[[instrument]]
"#;

/// A recorded pattern the session switched off.
pub const DISABLED_PATTERN: &str = r#"
[[instrument]]
plugin = "builtin:osc"
[instrument.pattern]
bpm = 100.0
length_beats = 4.0
base_note = "C4"
enabled = false
[[instrument.pattern.events]]
frame = 0
status = "on"
note = "C4"
velocity = 100
[[instrument.pattern.events]]
frame = 12000
status = "off"
note = "C4"
velocity = 0
"#;

/// Cross-modulation of every kind: a group envelope and LFO that drive each
/// other's fields and a member's modulators, and a lane whose LFO drives its
/// siblings' rate, depth and ADSR stages.
pub const CROSS_MOD: &str = r#"
[[group]]
name = "Cross"

[[group.modulator]]
type = "envelope"
attack = 0.05
decay = 0.2
sustain = 0.6
release = 0.9

[[group.modulator]]
rate = 0.7
[[group.modulator.target]]
mod_attack = 0
depth = 0.1
[[group.modulator.target]]
mod_decay = 0
depth = 0.2
[[group.modulator.target]]
mod_sustain = 0
depth = 0.3
[[group.modulator.target]]
mod_release = 0
depth = 0.4
[[group.modulator.target]]
mod_depth = [0, 0]
depth = 0.5
[[group.modulator.target]]
member = 0
mod_rate = 0
depth = 0.15
[[group.modulator.target]]
member = 0
mod_attack = 1
depth = 0.25
[[group.modulator.target]]
member = 0
mod_depth = [0, 0]
depth = 0.35

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.modulator]]
rate = 2.0
[[instrument.modulator.target]]
param = "detune"
depth = 0.3
[[instrument.modulator]]
type = "envelope"
[[instrument.modulator]]
rate = 0.1
[[instrument.modulator.target]]
mod_rate = 0
depth = 0.1
[[instrument.modulator.target]]
mod_depth = [0, 0]
depth = 0.2
[[instrument.modulator.target]]
mod_attack = 1
depth = 0.3
[[instrument.modulator.target]]
mod_decay = 1
depth = 0.4
[[instrument.modulator.target]]
mod_sustain = 1
depth = 0.5
[[instrument.modulator.target]]
mod_release = 1
depth = 0.6
"#;

/// Panic, showing where, if two models differ anywhere. They are compared by their
/// `Debug` dumps, so a field added to the model later is covered without touching this.
pub fn assert_same_model(what: &str, a: &SessionModel, b: &SessionModel) {
    let (a, b) = (format!("{a:#?}"), format!("{b:#?}"));
    if a == b {
        return;
    }
    let (a, b): (Vec<&str>, Vec<&str>) = (a.lines().collect(), b.lines().collect());
    let at = a.iter().zip(&b).position(|(x, y)| x != y).unwrap_or(a.len().min(b.len()));
    let around = |lines: &[&str]| lines[at.saturating_sub(8)..(at + 3).min(lines.len())].join("\n");
    panic!("{what}: the models differ at line {at}\n--- first ---\n{}\n--- second ---\n{}", around(&a), around(&b));
}
