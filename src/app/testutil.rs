//! Test helpers shared by the app and TUI tests.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread::{self, ThreadId};

use crossbeam_channel::{Receiver, Sender};

use crate::app::controller::EngineHandles;
use crate::app::load::{Progress, load_session};
use crate::app::model::{NodeId, SessionModel};
use crate::audio::MidiEvent;
use crate::held_notes::HeldNotes;
use crate::midi_monitor::MidiMonitor;
use crate::piano_filter::{PianoFilter, PianoMode};
use crate::plugin::chain::{GraphCommand, GroupFacts, LaneFacts, MidiLearnedNotification, ModulatorLocation, PatternNotification};
use crate::scale::ScaleSetting;
use crate::telemetry::Telemetry;
use crate::{plugin, session};

/// How `describe` names a command that addresses a rack of modulators: the command's name for a lane's rack or for a group's, and the place of the lane or the group
/// (`InsertModulator inst=0`, `InsertGroupModulator group=1`), which is what the tests that read it have always said.
fn rack_word(location: ModulatorLocation, lane: &str, group: &str) -> String {
    match location {
        ModulatorLocation::Lane(inst) => format!("{lane} inst={inst}"),
        ModulatorLocation::Group(of) => format!("{group} group={of}"),
    }
}

/// One line per command, for asserting on what a front-end or the loader sent
/// to the audio thread. `GraphCommand` carries boxed plugins, so it has no
/// `Debug`; this prints each command's variant and its scalar fields (plugins by
/// name). Commands nobody asserts on yet show as `<other>`, which still changes
/// an expected list when one shows up unexpectedly.
pub fn describe(cmd: &GraphCommand) -> String {
    use GraphCommand as C;
    match cmd {
        C::AddGroup { .. } => "AddGroup".to_string(),
        C::RemoveGroup { group, keep } => format!("RemoveGroup group={group}{}", if keep.is_some() { " keep" } else { "" }),
        C::InsertGroup { index, group, members } => format!("InsertGroup index={index} plugins={} members={members:?}", group.plugin_count()),
        C::SetGroupVolume { group, value } => format!("SetGroupVolume group={group} value={value:.3}"),
        C::SetGroupPan { group, value } => format!("SetGroupPan group={group} value={value:.3}"),
        C::SetGroupMute { group, muted } => format!("SetGroupMute group={group} muted={muted}"),
        C::SetGroupRoute { group, route } => format!("SetGroupRoute group={group} route={}", route.name()),
        C::SetLaneRoute { inst, route } => format!("SetLaneRoute inst={inst} route={}", route.name()),
        C::SetEffectFeed { node, fed } => format!("SetEffectFeed node={} fed={fed}", node.0),
        C::SetGroupSolo { group, solo } => format!("SetGroupSolo group={group} solo={solo}"),
        C::SetGroupMix { group, index, value } => {
            format!("SetGroupMix group={group} fx={index} value={value:.3}")
        }
        C::InsertGroupEffect { group, index, effect, mix, .. } => {
            format!("InsertGroupEffect group={group} index={index} plugin={} mix={mix:.3}", effect.name())
        }
        C::RemoveGroupEffect { group, index, keep } => format!("RemoveGroupEffect group={group} index={index}{}", if keep.is_some() { " keep" } else { "" }),
        C::ReorderGroupEffect { group, from, to } => {
            format!("ReorderGroupEffect group={group} from={from} to={to}")
        }
        C::SetGroupParameter { group, index, param_index, value } => {
            format!("SetGroupParameter group={group} fx={index} param={param_index} value={value:.3}")
        }
        C::AddInstrument { range, .. } => format!("AddInstrument range={range:?}"),
        C::RemoveInstrument { inst, keep } => format!("RemoveInstrument inst={inst}{}", if keep.is_some() { " keep" } else { "" }),
        C::InsertInstrument { index, lane } => format!("InsertInstrument index={index} plugins={}", lane.plugin_count()),
        C::ClearInstrument { inst, keep } => format!("ClearInstrument inst={inst}{}", if keep.is_some() { " keep" } else { "" }),
        C::SwapInstruments { inst_a, inst_b } => format!("SwapInstruments a={inst_a} b={inst_b}"),
        C::SetLaneGroup { inst, group } => format!("SetLaneGroup inst={inst} group={group:?}"),
        C::SetInstrumentRange { inst, range } => format!("SetInstrumentRange inst={inst} range={range:?}"),
        C::SwapInstrument { inst, instrument, inst_buf, remapper, keep } => format!(
            "SwapInstrument inst={inst} plugin={} bufs={} remap={}{}",
            instrument.name(),
            inst_buf.len(),
            remapper.is_some(),
            if keep.is_some() { " keep" } else { "" }
        ),
        C::SetVolume { inst, value } => format!("SetVolume inst={inst} value={value:.3}"),
        C::SetMute { inst, muted } => format!("SetMute inst={inst} muted={muted}"),
        C::SetSolo { inst, solo } => format!("SetSolo inst={inst} solo={solo}"),
        C::SetTranspose { inst, semitones } => format!("SetTranspose inst={inst} semitones={semitones}"),
        C::SetParameter { inst, slot, param_index, value } => {
            format!("SetParameter inst={inst} slot={slot} param={param_index} value={value:.3}")
        }
        C::SetMix { inst, slot, value } => format!("SetMix inst={inst} slot={slot} value={value:.3}"),
        C::LoadPreset { inst, slot, preset_id } => format!("LoadPreset inst={inst} slot={slot} id={preset_id}"),
        C::InsertEffect { inst, index, effect, mix, .. } => {
            format!("InsertEffect inst={inst} index={index} plugin={} mix={mix:.3}", effect.name())
        }
        C::RemoveEffect { inst, index, keep } => format!("RemoveEffect inst={inst} index={index}{}", if keep.is_some() { " keep" } else { "" }),
        C::ReorderEffect { inst, from, to } => format!("ReorderEffect inst={inst} from={from} to={to}"),
        C::InsertModulator { location, index, source, .. } => {
            format!("{} index={index} source={source:?}", rack_word(*location, "InsertModulator", "InsertGroupModulator"))
        }
        C::RemoveModulator { location, index } => format!("{} index={index}", rack_word(*location, "RemoveModulator", "RemoveGroupModulator")),
        C::AddModTarget { location, mod_index, target } => format!(
            "{} mod={mod_index} aim={:?} depth={:.3} base={:.3} range=[{:.3}, {:.3}]",
            rack_word(*location, "AddModTarget", "AddGroupModTarget"),
            target.aim,
            target.depth,
            target.base_value,
            target.param_min,
            target.param_max
        ),
        C::InsertModTarget { location, mod_index, target_index, target } => format!(
            "{} mod={mod_index} target={target_index} aim={:?} depth={:.3} base={:.3} range=[{:.3}, {:.3}]",
            rack_word(*location, "InsertModTarget", "InsertGroupModTarget"),
            target.aim,
            target.depth,
            target.base_value,
            target.param_min,
            target.param_max
        ),
        C::RemoveModTarget { location, mod_index, target_index } => {
            format!("{} mod={mod_index} target={target_index}", rack_word(*location, "RemoveModTarget", "RemoveGroupModTarget"))
        }
        C::SetModulatorOutput { location, index, slot } => {
            format!("SetModulatorOutput {location:?} index={index} slot={}", slot.index())
        }
        C::SetModulatorPosition { location, index, slot } => {
            format!("SetModulatorPosition {location:?} index={index} slot={}", slot.index())
        }
        C::SetModulatorKey { location, index, retrigger, fade } => {
            format!("SetModulatorKey {location:?} index={index} retrigger={retrigger} fade={fade:.3}")
        }
        C::SetModulatorSync { location, index, beats } => {
            format!("SetModulatorSync {location:?} index={index} beats={beats:.3}")
        }
        C::SetModulatorSteps { location, index, values, length, glide, retrigger } => {
            format!("SetModulatorSteps {location:?} index={index} values={values:?} length={length} glide={glide:.3} retrigger={retrigger}")
        }
        C::SetLaneMeter { inst, slots } => {
            format!("SetLaneMeter inst={inst} peak={} clips={}", slots.peak.index(), slots.clips.index())
        }
        C::SetGroupMeter { group, slots } => {
            format!("SetGroupMeter group={group} peak={} clips={}", slots.peak.index(), slots.clips.index())
        }
        C::SetModulatorRate { location, mod_index, rate } => {
            format!("{} mod={mod_index} rate={rate:.3}", rack_word(*location, "SetModulatorRate", "SetGroupModulatorRate"))
        }
        C::SetModTargetDepth { location, mod_index, target_index, depth } => {
            format!("{} mod={mod_index} target={target_index} depth={depth:.3}", rack_word(*location, "SetModTargetDepth", "SetGroupModTargetDepth"))
        }
        C::SetModulatorSource { location, mod_index, source } => {
            format!("{} mod={mod_index} source={source:?}", rack_word(*location, "SetModulatorSource", "SetGroupModulatorSource"))
        }
        C::SetModulatorWaveform { location, mod_index, waveform } => {
            format!("{} mod={mod_index} waveform={waveform:?}", rack_word(*location, "SetModulatorWaveform", "SetGroupModulatorWaveform"))
        }
        C::SetModulatorEnvelopeParam { location, mod_index, attack, decay, sustain, release } => format!(
            "{} mod={mod_index} attack={attack:.3} decay={decay:.3} sustain={sustain:.3} release={release:.3}",
            rack_word(*location, "SetModulatorEnvelopeParam", "SetGroupModulatorEnvelopeParam")
        ),

        C::SetPattern { inst, pattern, base_note, in_key } => format!(
            "SetPattern inst={inst} events={} length_samples={} base_note={base_note:?} in_key={in_key}",
            pattern.events.len(),
            pattern.length_samples
        ),
        C::SetGlobalBpm { bpm } => format!("SetGlobalBpm bpm={bpm:.1}"),
        C::SetPatternLength { inst, beats } => format!("SetPatternLength inst={inst} beats={beats:.1}"),
        C::SetPatternEnabled { inst, enabled } => format!("SetPatternEnabled inst={inst} enabled={enabled}"),
        C::SetPatternLooping { inst, looping } => format!("SetPatternLooping inst={inst} looping={looping}"),
        C::SetPatternInKey { inst, in_key } => format!("SetPatternInKey inst={inst} in_key={in_key}"),
        C::SetPatternRecording { inst, recording } => format!("SetPatternRecording inst={inst} recording={recording}"),
        C::ClearPattern { inst } => format!("ClearPattern inst={inst}"),
        C::AddCable { id, from, to, gain } => format!("AddCable id={} from={from:?} to={to:?} gain={gain:.3}", id.0),
        C::RemoveCable { id } => format!("RemoveCable id={}", id.0),
        C::SetCableGain { id, gain } => format!("SetCableGain id={} gain={gain:.3}", id.0),
        C::SetLaneSources { inst, sources } => {
            let heard = if *sources == crate::source::SourceSet::ALL { "all".to_string() } else { format!("{:?}", sources.iter().map(|source| source.number()).collect::<Vec<_>>()) };
            format!("SetLaneSources inst={inst} sources={heard}")
        }
        C::ReleaseSource { source } => format!("ReleaseSource source={}", source.number()),
        C::SetProbe { target, ring } => format!("SetProbe ring={ring} {target:?}"),
        C::ClearProbes => "ClearProbes".to_string(),
        _ => "<other>".to_string(),
    }
}

/// A command that only assigns a telemetry slot to a node a command before it
/// created. They say nothing about the session, and a test that lists what was
/// sent would have to repeat one after every modulator, so `described` leaves
/// them out, and `StubEngine::drain` and `Run::telemetry` have them on their own.
fn is_telemetry(cmd: &GraphCommand) -> bool {
    matches!(
        cmd,
        GraphCommand::SetModulatorOutput { .. } | GraphCommand::SetModulatorPosition { .. } | GraphCommand::SetLaneMeter { .. } | GraphCommand::SetGroupMeter { .. }
    )
}

/// What `describe` says about each of `commands`, except those that only assign
/// telemetry slots (see `is_telemetry`).
pub fn described(commands: impl IntoIterator<Item = GraphCommand>) -> Vec<String> {
    commands.into_iter().inspect(answer).filter(|c| !is_telemetry(c)).map(|c| describe(&c)).collect()
}

/// What the audio thread would do for a command that asks something of it, for a test that has none: a lane or a group that is taken out is handed to the channel that asks
/// for it, with no plugins in it (one that gave up its plugins, which the controller loads again to put it back).
fn answer(command: &GraphCommand) {
    match command {
        GraphCommand::RemoveInstrument { keep: Some(keep), .. } => {
            let _ = keep.try_send(crate::plugin::chain::KeptLane::empty());
        }
        GraphCommand::RemoveGroup { keep: Some(keep), .. } => {
            let _ = keep.try_send(crate::plugin::chain::KeptGroup::empty());
        }
        _ => {}
    }
}

/// What a load did: the model it returned, the commands it sent (one line
/// each, see `described`), those that assigned telemetry slots, and the progress
/// it reported.
#[derive(Debug)]
pub struct Run {
    pub model: SessionModel,
    pub commands: Vec<String>,
    pub telemetry: Vec<String>,
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
    let (slot_commands, others): (Vec<GraphCommand>, Vec<GraphCommand>) = rx.try_iter().partition(is_telemetry);
    let telemetry = slot_commands.iter().map(describe).collect();
    let commands = others.iter().map(describe).collect();
    model.check_ids().map_err(|e| anyhow::anyhow!("the loader's node ids are wrong: {e}"))?;
    Ok(Run { model, commands, telemetry, progress })
}

/// Load `text` the way the program does, sending the commands down `cmd_tx`
/// (to a real graph, say) and returning the model.
pub fn load_onto(text: &str, cmd_tx: &Sender<GraphCommand>) -> SessionModel {
    let config = session::parse(text).expect("the session parses");
    load_session(&config, Path::new("."), 48_000.0, 256, &plugin::Runtime::default(), cmd_tx, &mut |_| {})
        .expect("the session loads")
}

pub fn load(text: &str) -> Run {
    try_load(text).unwrap_or_else(|e| panic!("load failed: {e:#}"))
}

/// The audio thread's side of the channels a controller is built around.
pub struct StubEngine {
    /// What the controller has sent the audio thread.
    pub cmd_rx: Receiver<GraphCommand>,
    /// The notes it would play.
    pub midi_rx: Receiver<MidiEvent>,
    /// Where the audio thread hands back plugins it has taken out of the graph.
    pub return_tx: Sender<Box<dyn plugin::Plugin>>,
    /// Where it reports finished pattern recordings.
    pub pattern_tx: Sender<PatternNotification>,
    /// Where it reports captured MIDI-learn controls.
    pub learn_tx: Sender<MidiLearnedNotification>,
}

impl StubEngine {
    /// Everything the controller has sent since the last call, as text: the
    /// commands, and, separately, the ones that only assign telemetry slots.
    pub fn drain(&self) -> (Vec<String>, Vec<String>) {
        let (slot_commands, others): (Vec<GraphCommand>, Vec<GraphCommand>) = self.cmd_rx.try_iter().inspect(answer).partition(is_telemetry);
        (others.iter().map(describe).collect(), slot_commands.iter().map(describe).collect())
    }
}

/// A finished two-beat recording of one note, as the audio thread reports it.
pub fn recording(inst: usize) -> PatternNotification {
    PatternNotification {
        inst,
        base_note: Some(60),
        length_beats: 2.0,
        looping: true,
        enabled: true,
        events: vec![(0, 0x90, 60, 100), (24_000, 0x80, 60, 0)],
    }
}

/// Handles for a `Controller` over stub channels (48 kHz, 256-frame blocks, the
/// default scale), and the other ends of those channels.
pub fn stub_engine() -> (EngineHandles, StubEngine) {
    stub_engine_of(64)
}

/// `stub_engine`, with room for `capacity` commands that nobody has taken yet (a session
/// loaded onto a real graph from one thread needs room for all of its commands).
pub fn stub_engine_of(capacity: usize) -> (EngineHandles, StubEngine) {
    let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(capacity);
    let (midi_tx, midi_rx) = crossbeam_channel::bounded(1024);
    let (return_tx, return_rx) = crossbeam_channel::bounded(16);
    let (pattern_tx, pattern_rx) = crossbeam_channel::unbounded();
    let (learn_tx, learn_rx) = crossbeam_channel::unbounded();
    let handles = EngineHandles {
        cmd_tx,
        midi_tx,
        return_rx,
        pattern_rx,
        learn_rx,
        runtime: plugin::Runtime::default(),
        sample_rate: 48_000.0,
        max_block_size: 256,
        held: Arc::new(HeldNotes::new()),
        piano_filter: Arc::new(PianoFilter::new(ScaleSetting::default(), PianoMode::Highlight)),
        midi_monitor: Arc::new(MidiMonitor::new(false)),
        telemetry: Arc::new(Telemetry::new()),
        // (Nobody runs the audio thread that hands a plugin back, unless a test does it by hand: it is not waited for.)
        recovery_wait: std::time::Duration::ZERO,
    };
    (handles, StubEngine { cmd_rx, midi_rx, return_tx, pattern_tx, learn_tx })
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

/// Every kind of node has a place on the canvas, the master too; the second lane has none.
pub const PLACED: &str = r#"
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

/// Parameters pinned on the nodes of a plugin: on a lane's instrument, on its effect and on a bus effect. A name that the plugin has no
/// parameter of, and a name twice, are no pins; the second lane has none.
pub const PINNED: &str = r#"
[[group]]
[[group.effect]]
plugin = "builtin:filter"
pinned = ["drive", "cutoff"]

[[instrument]]
plugin = "builtin:osc"
group = 0
pinned = ["sustain", "detune", "waveform"]

[[instrument.effect]]
plugin = "builtin:filter"
pinned = ["resonance", "no such parameter", "resonance", "cutoff"]

[[instrument]]
plugin = "builtin:osc"
"#;

/// A group whose members have modulators of every kind with cables of their own (an envelope with two), and a rack of three of the
/// group's own; a lane in no group and a lane of a second group, each with an LFO that the first group does not drive: for the lists
/// of what a modulator can drive of the others'.
pub const CROSS_MOD_RACKS: &str = r#"
[[group]]
[[group.effect]]
plugin = "builtin:reverb"

[[group.modulator]]
rate = 0.5
[[group.modulator.target]]
member = 0
param = "detune"
depth = 0.4

[[group.modulator]]
type = "envelope"
attack = 0.2
[[group.modulator.target]]
bus = 0
param = "damping"
depth = 0.25

[[group.modulator]]
type = "midi"
midi_cc = 74
[[group.modulator.target]]
member = 1
param = "volume"
depth = 1.0

[[group]]

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.modulator]]
rate = 3.0
[[instrument.modulator.target]]
param = "detune"
depth = 0.1
[[instrument.modulator]]
type = "envelope"
sustain = 0.4
[[instrument.modulator.target]]
param = "volume"
depth = 0.2
[[instrument.modulator.target]]
param = "detune"
depth = 0.7
[[instrument.modulator]]
type = "midi"
midi_cc = 1
[[instrument.modulator.target]]
param = "pan"
depth = 0.3

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.modulator]]
rate = 1.0

[[instrument]]
plugin = "builtin:osc"
[[instrument.modulator]]
rate = 2.0

[[instrument]]
plugin = "builtin:osc"
group = 1
[[instrument.modulator]]
rate = 4.0
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
[[instrument.modulator]]
type = "midi"
midi_velocity = true
[[instrument.modulator.target]]
param = "volume"
[[instrument.modulator]]
type = "midi"
midi_key = true
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

/// A lane with three modulators: the first drives three parameters, the second the depth of the
/// first's first and third cable, the third the depth of the second's first.
pub const CABLES_THAT_DRIVE_CABLES: &str = r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.modulator]]
rate = 1.0
[[instrument.modulator.target]]
param = "detune"
depth = 0.1
[[instrument.modulator.target]]
param = "volume"
depth = 0.2
[[instrument.modulator.target]]
param = "pan"
depth = 0.3
[[instrument.modulator]]
rate = 2.0
[[instrument.modulator.target]]
mod_depth = [0, 0]
depth = 0.4
[[instrument.modulator.target]]
mod_depth = [0, 2]
depth = 0.5
[[instrument.modulator]]
rate = 3.0
[[instrument.modulator.target]]
mod_depth = [1, 0]
depth = 0.6
"#;

/// Two modulators whose first cables drive each other's depth.
pub const CABLES_THAT_DRIVE_EACH_OTHER: &str = r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.modulator]]
rate = 1.0
[[instrument.modulator.target]]
mod_depth = [1, 0]
depth = 0.1
[[instrument.modulator.target]]
param = "detune"
depth = 0.2
[[instrument.modulator]]
rate = 2.0
[[instrument.modulator.target]]
mod_depth = [0, 0]
depth = 0.3
"#;

/// Four lanes in two groups and none, each told apart by what it has: the first in group 0, the second in no
/// group, the third in group 1, the fourth in group 0, and the groups' modulators drive the members.
pub const LANES_IN_GROUPS: &str = r#"
[[group]]
[[group.modulator]]
rate = 1.0
[[group.modulator.target]]
member = 0
param = "detune"
depth = 0.1
[[group.modulator.target]]
member = 1
param = "pan"
depth = 0.2
[[group.modulator.target]]
member = 1
mod_rate = 0
depth = 0.3

[[group]]
[[group.modulator]]
rate = 2.0
[[group.modulator.target]]
member = 0
param = "volume"
depth = 0.4

[[instrument]]
plugin = "builtin:osc"
group = 0
volume = 0.5
range = "C0-B3"
[[instrument.modulator]]
rate = 3.0

[[instrument]]
plugin = "builtin:osc"
volume = 0.6
transpose = 7
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.modulator]]
rate = 4.0
[[instrument.modulator]]
rate = 5.0

[[instrument]]
plugin = "builtin:osc"
group = 1
volume = 0.7

[[instrument]]
plugin = "builtin:osc"
group = 0
volume = 0.8
[[instrument.effect]]
plugin = "builtin:reverb"
[[instrument.modulator]]
rate = 6.0
"#;

/// A session to draw audio cables in (P3c of `docs/patch-graph.md` §11.6): group 0 has a reverb on its bus; lane 0, in the group, has a filter and then a delay; lane 1,
/// in no group, has a drive.
pub const SENDS: &str = r#"
[[group]]
[[group.effect]]
plugin = "builtin:reverb"

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.effect]]
plugin = "builtin:delay"

[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:drive"
"#;

/// The nodes of `SENDS`: the two lanes, the three effects of the lanes (lane 0's filter and delay, lane 1's drive), the group and its bus effect.
#[derive(Clone, Copy, Debug)]
pub struct Sends {
    pub lane: [NodeId; 2],
    pub fx: [NodeId; 3],
    pub group: NodeId,
    pub bus: NodeId,
}

impl Sends {
    /// The nodes of a model that was loaded from `SENDS`.
    pub fn of(model: &SessionModel) -> Sends {
        Sends {
            lane: [model.instruments[0].node, model.instruments[1].node],
            fx: [model.instruments[0].effects[0].node, model.instruments[0].effects[1].node, model.instruments[1].effects[0].node],
            group: model.groups[0].node,
            bus: model.groups[0].effects[0].node,
        }
    }
}

/// A session to route notes in (P4b of `docs/patch-graph.md` §11.6): the keys (placed) and two devices (the first placed), and three lanes: lane 0 hears the keys only, lane 1 hears both
/// devices, and lane 2 has no cable and hears every source. The cables of notes have ids 0, 1 and 2, in the order of the file, and the audio cable after them has 3.
pub const SOURCES: &str = r#"
[keys]
ui = { x = 0, y = 40 }

[[device]]
name = "Launchkey Mini MK4 37"
ui = { x = 0, y = 300.5 }

[[device]]
name = "Seaboard"

[[instrument]]
plugin = "builtin:osc"

[[instrument]]
plugin = "builtin:osc"

[[instrument]]
plugin = "builtin:osc"

[[cable]]
from = "keys"
to = "instrument[0]"

[[cable]]
from = "device[0]"
to = "instrument[1]"

[[cable]]
from = "device[1]"
to = "instrument[1]"

[[cable]]
from = "instrument[2]"
to = "master"
"#;

/// A session with sources of notes among everything that a cable of notes could be drawn to or from by mistake (P4c: the fixture of what sources and cables of notes come to,
/// `web/src/client/fixtures/sources.json`): the keys (placed) and a device, a group ("Pad") with a reverb on its bus, lane 0 in it with a filter and an LFO that drives the filter, a plain lane (1)
/// and an empty one (2), a cable of notes from the keys to lane 1 and an audio cable from lane 1 to the filter of lane 0 (the two kinds of cable have ids of one series, in the order of the file).
pub const NOTES: &str = r#"
[keys]
ui = { x = 0, y = 40 }

[[device]]
name = "Launchkey Mini MK4 37"

[[group]]
name = "Pad"
[[group.effect]]
plugin = "builtin:reverb"

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.modulator]]
rate = 1.0
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = 0.5

[[instrument]]
plugin = "builtin:osc"

[[instrument]]

[[cable]]
from = "keys"
to = "instrument[1]"

[[cable]]
from = "instrument[1]"
to = "instrument[0].effect[0]"
gain = 0.5
"#;

/// The nodes of `SOURCES`: the keys, the two devices and the three lanes.
#[derive(Clone, Copy, Debug)]
pub struct Routed {
    pub keys: NodeId,
    pub devices: [NodeId; 2],
    pub lane: [NodeId; 3],
}

impl Routed {
    /// The nodes of a model that was loaded from `SOURCES`.
    pub fn of(model: &SessionModel) -> Routed {
        Routed {
            keys: model.sources[0].node,
            devices: [model.sources[1].node, model.sources[2].node],
            lane: [model.instruments[0].node, model.instruments[1].node, model.instruments[2].node],
        }
    }
}

/// A session with audio cables that can make the audio go round when a lane changes group or an effect moves, and an empty lane, a lane's modulator and a bus of three effects (P3d: the
/// fixture of what audio cables come to, `web/src/client/fixtures/audioCables.json`). Group 0 ("Pad") has a filter, a reverb and a delay on its bus, and lane 0 in it; lane 0 has a filter,
/// a delay and an LFO that drives the filter; lane 1, in no group, has a drive; lane 2 is empty. The audio cables, in the order of the file (their ids come after the LFO's cable's, 0):
/// 1 from lane 1's instrument to lane 0's filter; 2 from lane 0's filter to its delay, beside the link that the chain has; 3 from the group's output to lane 1's drive (lane 1 in the group
/// would feed itself); 4 from the bus filter to the bus reverb, beside the link that the bus has; 5 from lane 0's delay to the group's bus; 6 from the bus reverb to the mix.
pub const PATCHED: &str = r#"
[[group]]
name = "Pad"
[[group.effect]]
plugin = "builtin:filter"
[[group.effect]]
plugin = "builtin:reverb"
[[group.effect]]
plugin = "builtin:delay"

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.effect]]
plugin = "builtin:delay"
[[instrument.modulator]]
rate = 1.0
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = 0.5

[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:drive"

[[instrument]]

[[cable]]
from = "instrument[1]"
to = "instrument[0].effect[0]"
gain = 0.5

[[cable]]
from = "instrument[0].effect[0]"
to = "instrument[0].effect[1]"
gain = 0.75

[[cable]]
from = "group[0]"
to = "instrument[1].effect[0]"
gain = 1.5

[[cable]]
from = "group[0].effect[0]"
to = "group[0].effect[1]"

[[cable]]
from = "instrument[0].effect[1]"
to = "group[0]"

[[cable]]
from = "group[0].effect[1]"
to = "master"
gain = 0.5
"#;

/// What taking things out of the chains has to keep right, with a cable of every kind that can end at a
/// plugin: a group (a filter and a reverb on its bus) of two lanes, and a lane in none. The first lane has a
/// filter and a reverb of its own, and a modulator that drives both (and its own detune), and another that
/// drives the depth of two of its cables; the group's first modulator drives the first member's filter and
/// reverb, the second member's detune, both bus effects and the first member's modulator, and its second
/// the depth of two of its own cables.
pub const CHAINS: &str = r#"
[[group]]
name = "Pad"

[[group.effect]]
plugin = "builtin:filter"
[[group.effect]]
plugin = "builtin:reverb"

[[group.modulator]]
rate = 0.5
[[group.modulator.target]]
member = 0
slot = 1
param = "cutoff"
depth = 0.4
[[group.modulator.target]]
member = 0
slot = 2
param = "damping"
depth = 0.3
[[group.modulator.target]]
member = 1
param = "detune"
depth = 0.2
[[group.modulator.target]]
bus = 0
param = "cutoff"
depth = 0.1
[[group.modulator.target]]
bus = 1
param = "damping"
depth = 0.5
[[group.modulator.target]]
member = 0
mod_rate = 0
depth = 0.25

[[group.modulator]]
rate = 0.7
[[group.modulator.target]]
mod_depth = [0, 1]
depth = 0.3
[[group.modulator.target]]
mod_depth = [0, 4]
depth = 0.35

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.effect]]
plugin = "builtin:reverb"
[[instrument.modulator]]
rate = 2.0
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = 0.1
[[instrument.modulator.target]]
slot = 2
param = "damping"
depth = 0.2
[[instrument.modulator.target]]
param = "detune"
depth = 0.3
[[instrument.modulator]]
rate = 4.0
[[instrument.modulator.target]]
mod_depth = [0, 0]
depth = 0.4
[[instrument.modulator.target]]
mod_depth = [0, 1]
depth = 0.45

[[instrument]]
plugin = "builtin:osc"
group = 0

[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.modulator]]
rate = 1.0
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = 0.2
"#;

/// Chains long enough to be moved about in: a group with four bus effects (two filters and two reverbs) and a lane in it that has four
/// effects of its own, then a lane in no group with three. The group's modulator has a cable to each of the eight plugins of the group's
/// and its member's chains, the member's modulator a cable to each of its four, and the other lane's a cable to each of its three.
pub const LONG_CHAINS: &str = r#"
[[group]]
[[group.effect]]
plugin = "builtin:filter"
[[group.effect]]
plugin = "builtin:reverb"
[[group.effect]]
plugin = "builtin:filter"
[[group.effect]]
plugin = "builtin:reverb"
[[group.modulator]]
rate = 1.0
[[group.modulator.target]]
bus = 0
param = "cutoff"
depth = 0.1
[[group.modulator.target]]
bus = 1
param = "damping"
depth = 0.2
[[group.modulator.target]]
bus = 2
param = "cutoff"
depth = 0.3
[[group.modulator.target]]
bus = 3
param = "damping"
depth = 0.4
[[group.modulator.target]]
member = 0
slot = 1
param = "cutoff"
depth = 0.1
[[group.modulator.target]]
member = 0
slot = 2
param = "damping"
depth = 0.2
[[group.modulator.target]]
member = 0
slot = 3
param = "cutoff"
depth = 0.3
[[group.modulator.target]]
member = 0
slot = 4
param = "damping"
depth = 0.4

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.effect]]
plugin = "builtin:reverb"
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.effect]]
plugin = "builtin:reverb"
[[instrument.modulator]]
rate = 2.0
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = 0.1
[[instrument.modulator.target]]
slot = 2
param = "damping"
depth = 0.2
[[instrument.modulator.target]]
slot = 3
param = "cutoff"
depth = 0.3
[[instrument.modulator.target]]
slot = 4
param = "damping"
depth = 0.4

[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.effect]]
plugin = "builtin:reverb"
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.modulator]]
rate = 3.0
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = 0.1
[[instrument.modulator.target]]
slot = 2
param = "damping"
depth = 0.2
[[instrument.modulator.target]]
slot = 3
param = "cutoff"
depth = 0.3
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

/// A plugin that does nothing but call `on_drop` with the thread that dropped it,
/// for tests of where and when plugins get dropped.
pub struct DropProbe {
    on_drop: Option<Box<dyn FnOnce(ThreadId) + Send>>,
}

impl DropProbe {
    pub fn new(on_drop: impl FnOnce(ThreadId) + Send + 'static) -> Self {
        DropProbe { on_drop: Some(Box::new(on_drop)) }
    }

    /// A probe, and the slot that will hold the id of the thread that dropped it.
    pub fn recording() -> (Self, Arc<Mutex<Option<ThreadId>>>) {
        let slot = Arc::new(Mutex::new(None));
        let probe_slot = slot.clone();
        // (Tolerates a poisoned lock: this runs in `drop`, possibly while a test is unwinding.)
        (Self::new(move |thread| *probe_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(thread)), slot)
    }
}

impl Drop for DropProbe {
    fn drop(&mut self) {
        if let Some(on_drop) = self.on_drop.take() {
            on_drop(thread::current().id());
        }
    }
}

/// An effect with a single audio output, which the chains (stereo) have no place for.
pub struct MonoEffect;

impl plugin::Plugin for MonoEffect {
    fn name(&self) -> &str {
        "MonoEffect"
    }
    fn is_instrument(&self) -> bool {
        false
    }
    fn sample_rate(&self) -> f32 {
        48_000.0
    }
    fn audio_output_count(&self) -> usize {
        1
    }
    fn audio_input_count(&self) -> usize {
        1
    }
    fn process(&mut self, _: &[(u64, [u8; 3])], _: &[&[f32]], _: &mut [&mut [f32]]) -> anyhow::Result<()> {
        Ok(())
    }
    fn parameters(&self) -> Vec<plugin::ParameterInfo> {
        Vec::new()
    }
    fn get_parameter(&mut self, _: u32) -> Option<f32> {
        None
    }
    fn set_parameter(&mut self, _: u32, _: f32) -> anyhow::Result<()> {
        Ok(())
    }
    fn presets(&self) -> Vec<plugin::Preset> {
        Vec::new()
    }
    fn load_preset(&mut self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

/// An instrument with this many audio outputs, no parameters and no sound: none is a MIDI utility, which has nothing to
/// put in a lane to be heard; one is a mono instrument, which the lanes (stereo) play on the left.
pub struct FakeInstrument(pub usize);

impl plugin::Plugin for FakeInstrument {
    fn name(&self) -> &str {
        "FakeInstrument"
    }
    fn is_instrument(&self) -> bool {
        true
    }
    fn sample_rate(&self) -> f32 {
        48_000.0
    }
    fn audio_output_count(&self) -> usize {
        self.0
    }
    fn audio_input_count(&self) -> usize {
        0
    }
    fn process(&mut self, _: &[(u64, [u8; 3])], _: &[&[f32]], _: &mut [&mut [f32]]) -> anyhow::Result<()> {
        Ok(())
    }
    fn parameters(&self) -> Vec<plugin::ParameterInfo> {
        Vec::new()
    }
    fn get_parameter(&mut self, _: u32) -> Option<f32> {
        None
    }
    fn set_parameter(&mut self, _: u32, _: f32) -> anyhow::Result<()> {
        Ok(())
    }
    fn presets(&self) -> Vec<plugin::Preset> {
        Vec::new()
    }
    fn load_preset(&mut self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

impl plugin::Plugin for DropProbe {
    fn name(&self) -> &str {
        "DropProbe"
    }
    fn is_instrument(&self) -> bool {
        true
    }
    fn sample_rate(&self) -> f32 {
        48_000.0
    }
    fn audio_output_count(&self) -> usize {
        2
    }
    fn audio_input_count(&self) -> usize {
        0
    }
    fn process(&mut self, _: &[(u64, [u8; 3])], _: &[&[f32]], _: &mut [&mut [f32]]) -> anyhow::Result<()> {
        Ok(())
    }
    fn parameters(&self) -> Vec<plugin::ParameterInfo> {
        Vec::new()
    }
    fn get_parameter(&mut self, _: u32) -> Option<f32> {
        None
    }
    fn set_parameter(&mut self, _: u32, _: f32) -> anyhow::Result<()> {
        Ok(())
    }
    fn presets(&self) -> Vec<plugin::Preset> {
        Vec::new()
    }
    fn load_preset(&mut self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

/// What a session comes to, for the fixtures of what the intents of groups and the history do: the nodes (their id, their parent and their name), the
/// audio path, the modulation cables, the audio cables that have been drawn between two places (`sends`, a list only when there are some), the lanes and groups whose way out is cut
/// (`routes`, likewise), the sources of notes (`sources`: what each is, the keys or a device by its name), the cables of notes (`notes`) and the links from each source to each lane that hears
/// every source (`hears`), each a list only when there is something in it, and the order of the kinds of edge (`order`, for a session that has a source), and the two lists of the layout of
/// the telemetry (without its fingerprint), each entry as a line of text the web client's tests make of its own copy by the same rules (`shapeOf` in
/// `web/src/client/fixtures.ts`).
pub fn shape_of(snapshot: &crate::app::snapshot::Snapshot) -> serde_json::Value {
    use serde_json::Value;
    let wire: Value = self::wire(snapshot);
    let text = |value: &Value| value.as_str().map(str::to_string).unwrap_or_else(|| value.to_string());
    // A port: its kind, then its index or the cable it is about.
    let port = |port: &Value| {
        let index = port.get("index").map(|i| i.to_string()).unwrap_or_default();
        let edge = port.get("edge").map(|e| format!("#{e}")).unwrap_or_default();
        format!("{}{index}{edge}", text(&port["type"]))
    };
    // A number as a script writes it: `1` where the wire says `1.0` (which no script reads back as anything but 1).
    let number = |value: &Value| value.as_f64().map_or_else(|| value.to_string(), |n| n.to_string());
    let list = |values: &Value| values.as_array().unwrap().clone();
    let nodes: Vec<String> = list(&wire["graph"]["nodes"])
        .iter()
        .map(|n| format!("{} {} {}", n["id"], if n["parent"].is_null() { "-".to_string() } else { n["parent"].to_string() }, text(&n["name"])))
        .collect();
    let edges = list(&wire["graph"]["edges"]);
    let audio: Vec<String> = edges
        .iter()
        .filter(|e| matches!(text(&e["kind"]).as_str(), "audio" | "member"))
        .map(|e| format!("{}>{} {}", e["from"]["node"], e["to"]["node"], text(&e["kind"])))
        .collect();
    let cables: Vec<String> = edges
        .iter()
        .filter(|e| text(&e["kind"]) == "modulation")
        .map(|e| format!("#{} {}>{} {} @{}", e["id"], e["from"]["node"], e["to"]["node"], port(&e["to"]["port"]), number(&e["amount"])))
        .collect();
    // The audio cables that have been drawn between two places (sends): a line each, and no list at all for a session that has none, so that the fixtures of the sessions that have none are as they were.
    let sends: Vec<String> = edges
        .iter()
        .filter(|e| text(&e["kind"]) == "cable")
        .map(|e| format!("#{} {}>{} @{}", e["id"], e["from"]["node"], e["to"]["node"], number(&e["gain"])))
        .collect();
    let slots = |entries: &Value| -> Vec<String> {
        list(entries).iter().map(|e| format!("{} {} {} {}", e["slot"], e["node"], port(&e["port"]), text(&e["kind"]))).collect()
    };
    let mut shape = serde_json::json!({
        "nodes": nodes,
        "audio": audio,
        "cables": cables,
        "meters": slots(&wire["telemetry"]["meters"]),
        "modulation": slots(&wire["telemetry"]["modulation"]),
    });
    if !sends.is_empty() {
        shape["sends"] = serde_json::json!(sends);
    }
    // The lanes and groups whose way out is cut: a line each, and no list at all when none is, so that the fixtures of the sessions that cut none are as they were.
    let routes: Vec<String> = list(&wire["graph"]["nodes"])
        .iter()
        .filter(|n| matches!(text(&n["kind"]).as_str(), "instrument" | "group") && text(&n["route"]) == "none")
        .map(|n| format!("{} none", n["id"]))
        .collect();
    if !routes.is_empty() {
        shape["routes"] = serde_json::json!(routes);
    }
    // The effects whose feed is cut (the link into them from the stage before is not there): a line each, and no list at all when none is, so that the fixtures of the sessions that cut none are as they were.
    let feeds: Vec<String> = list(&wire["graph"]["nodes"])
        .iter()
        .filter(|n| text(&n["kind"]) == "effect" && text(&n["feed"]) == "none")
        .map(|n| format!("{} none", n["id"]))
        .collect();
    if !feeds.is_empty() {
        shape["feeds"] = serde_json::json!(feeds);
    }
    // The sources of notes, the cables of notes that have been drawn and the links of the lanes that have none: a line each, and no list at all for a session that has none, so that the fixtures of the
    // sessions that have no source are as they were.
    let sources: Vec<String> = list(&wire["graph"]["nodes"])
        .iter()
        .filter(|n| text(&n["kind"]) == "source")
        .map(|n| match text(&n["source"]["type"]).as_str() {
            "keys" => format!("{} keys", n["id"]),
            _ => format!("{} device {}", n["id"], text(&n["source"]["name"])),
        })
        .collect();
    let notes: Vec<String> = edges.iter().filter(|e| text(&e["kind"]) == "events").map(|e| format!("#{} {}>{}", e["id"], e["from"]["node"], e["to"]["node"])).collect();
    let hears: Vec<String> = edges.iter().filter(|e| text(&e["kind"]) == "hears").map(|e| format!("{}>{}", e["from"]["node"], e["to"]["node"])).collect();
    // The order that the kinds of edge come in (a run of edges of one kind is one entry, `hears×4`), for a session that has a source: the graph lists the links of the audio path, those of the lanes that
    // hear every source, the audio cables, the cables of notes and then those of modulation.
    let order = if sources.is_empty() && notes.is_empty() && hears.is_empty() {
        Vec::new()
    } else {
        let mut runs: Vec<(String, usize)> = Vec::new();
        for edge in &edges {
            let kind = text(&edge["kind"]);
            match runs.last_mut() {
                Some((last, count)) if *last == kind => *count += 1,
                _ => runs.push((kind, 1)),
            }
        }
        runs.into_iter().map(|(kind, count)| format!("{kind}×{count}")).collect()
    };
    for (name, lines) in [("sources", sources), ("notes", notes), ("hears", hears), ("order", order)] {
        if !lines.is_empty() {
            shape[name] = serde_json::json!(lines);
        }
    }
    shape
}

/// A value as the wire says it: what its JSON says, with the 32-bit numbers in the fewest digits that tell them. (`serde_json::to_value` writes
/// them as the 64-bit numbers they widen to, 0.3 as 0.30000001192092896, which no client is sent: the fixtures the web client's tests read
/// are written from this.)
pub fn wire<T: serde::Serialize>(value: &T) -> serde_json::Value {
    serde_json::from_str(&serde_json::to_string(value).unwrap()).unwrap()
}

/// What each lane is as the model has it, in the terms `AudioGraph::lanes` uses.
pub fn model_lanes(model: &SessionModel) -> Vec<LaneFacts> {
    model
        .instruments
        .iter()
        .enumerate()
        .map(|(i, lane)| LaneFacts {
            node: lane.node,
            effect_nodes: lane.effects.iter().map(|effect| effect.node).collect(),
            cut_feeds: lane.effects.iter().filter(|effect| !effect.fed).map(|effect| effect.node).collect(),
            group: lane.group,
            range: lane.range,
            transpose: lane.transpose,
            volume: lane.volume,
            muted: lane.muted,
            solo: lane.solo,
            has_instrument: lane.instrument.is_some(),
            effects: lane.effects.len(),
            modulators: lane.modulators.len(),
            pattern_index: i,
        })
        .collect()
}

/// What each group is as the model has it, in the terms `AudioGraph::groups` uses.
/// What the model says the audio cables are, as the engine's `AudioGraph::audio_cables` says it: in the order of the ids.
pub fn model_audio_cables(model: &SessionModel) -> Vec<crate::plugin::chain::CableFacts> {
    model.cables.iter().map(|cable| crate::plugin::chain::CableFacts { id: cable.id, from: cable.from, to: cable.to, gain: cable.gain }).collect()
}

/// Every modulator's cables as the model has them, in the terms and the order of `AudioGraph::cables`: the lanes' racks and then the groups', each modulator with its node and its
/// cables' ids, what they drive and their depths.
pub fn model_racks(model: &SessionModel) -> crate::plugin::chain::RackCables {
    use crate::plugin::chain::ModulatorLocation;
    let racks = |at: fn(usize) -> ModulatorLocation, i: usize, rack: &[crate::app::model::ModulatorSlot]| -> Vec<_> {
        rack.iter()
            .enumerate()
            .map(|(m, modulator)| {
                let cables = modulator.targets.iter().enumerate().map(|(t, cable)| {
                    assert!(model.kind_of_target(at(i), m, t).is_some(), "the cable {:?} of modulator {m} of {:?} drives nothing that is in reach", cable.edge, at(i));
                    (cable.edge, cable.aim, cable.depth)
                });
                (at(i), m, modulator.node, cables.collect())
            })
            .collect()
    };
    let lanes = model.instruments.iter().enumerate().flat_map(|(i, lane)| racks(ModulatorLocation::Lane, i, &lane.modulators));
    let groups = model.groups.iter().enumerate().flat_map(|(i, group)| racks(ModulatorLocation::Group, i, &group.modulators));
    lanes.chain(groups).collect()
}

/// How the audio thread's racks name what each cable of the modulator at `mod_index` in the rack at `at` drives (`SessionModel::kind_of_target`), for a test that says it.
pub fn kinds_at(model: &SessionModel, at: crate::plugin::chain::ModulatorLocation, mod_index: usize) -> Vec<crate::plugin::chain::ModTargetKind> {
    let modulator = &model.modulators(at).expect("the rack is there")[mod_index];
    (0..modulator.targets.len()).map(|t| model.kind_of_target(at, mod_index, t).expect("a cable drives something that is in reach")).collect()
}

/// The slot of the chain that a kind of cable drives a plugin in (0 for the instrument and for anything else that is not in a chain): the slot that the file writes, and the tree
/// labels a target with its plugin by.
pub fn slot_of(kind: &crate::plugin::chain::ModTargetKind) -> usize {
    use crate::plugin::chain::ModTargetKind as K;
    match kind {
        K::PluginParam { slot, .. } | K::GroupMember { slot, .. } => *slot,
        _ => 0,
    }
}

/// A command as `describe` prints it, written with the place that its cable was drawn at (`kind=PluginParam { slot: 1, param_index: 0 }`: how the tests said what a cable drives before the
/// audio thread named it by a node, and how the file and the terminal's lists still say it, `ModTargetKind`), turned into what the audio thread is told (`aim=Param { plugin: NodeId(7),
/// index: 0 }`) by the node that the model has at that place. A test says where a cable goes by the place it chose, and is held to the node that is there; a text that has no `kind=`
/// is as it was.
pub fn told(model: &SessionModel, text: &str) -> String {
    use crate::app::graph::Port;
    use crate::app::model::{Driven, ModSetting};
    use crate::plugin::chain::{CrossModField, ModTargetKind as K, ModulatorLocation};
    let Some(start) = text.find("kind=") else { return text.to_string() };
    let (head, rest) = (&text[..start], &text[start + "kind=".len()..]);
    let number_after = |text: &str, key: &str| -> Option<usize> {
        let from = text.find(key)? + key.len();
        let digits: String = text[from..].chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    };
    let at = match number_after(head, "group=") {
        Some(group) => ModulatorLocation::Group(group),
        None => ModulatorLocation::Lane(number_after(head, "inst=").expect("a command that names a lane or a group")),
    };
    let name_end = rest.find(' ').expect("a kind has a name and fields");
    let open = name_end + 1;
    assert!(rest[open..].starts_with('{'), "a kind has fields: {text}");
    let close = open + rest[open..].find('}').unwrap_or_else(|| panic!("the fields of a kind end: {text}"));
    let fields: Vec<(&str, &str)> = rest[open + 1..close].split(',').map(|f| f.split_once(':').map(|(k, v)| (k.trim(), v.trim())).expect("a field has a name and a value")).collect();
    let field = |key: &str| fields.iter().find(|(k, _)| *k == key).unwrap_or_else(|| panic!("the kind has no field {key}: {text}")).1;
    let n = |key: &str| -> usize { field(key).parse().unwrap_or_else(|_| panic!("{key} is a number: {text}")) };
    let cross = |value: &str| match value {
        "Rate" => CrossModField::Rate,
        "Attack" => CrossModField::Attack,
        "Decay" => CrossModField::Decay,
        "Sustain" => CrossModField::Sustain,
        "Release" => CrossModField::Release,
        depth => CrossModField::Depth(depth.strip_prefix("Depth(").and_then(|d| d.strip_suffix(')')).and_then(|d| d.parse().ok()).unwrap_or_else(|| panic!("a field of a cross-mod cable: {depth}"))),
    };
    let kind = match &rest[..name_end] {
        "PluginParam" => K::PluginParam { slot: n("slot"), param_index: n("param_index") as u32 },
        "GroupMember" => K::GroupMember { member: n("member"), slot: n("slot"), param_index: n("param_index") as u32 },
        "GroupBus" => K::GroupBus { effect_index: n("effect_index"), param_index: n("param_index") as u32 },
        "GroupMemberMod" => K::GroupMemberMod { member: n("member"), mod_index: n("mod_index"), field: cross(field("field")) },
        "ModulatorRate" => K::ModulatorRate { mod_index: n("mod_index") },
        "ModulatorDepth" => K::ModulatorDepth { mod_index: n("mod_index"), target_index: n("target_index") },
        "ModulatorAttack" => K::ModulatorAttack { mod_index: n("mod_index") },
        "ModulatorDecay" => K::ModulatorDecay { mod_index: n("mod_index") },
        "ModulatorSustain" => K::ModulatorSustain { mod_index: n("mod_index") },
        "ModulatorRelease" => K::ModulatorRelease { mod_index: n("mod_index") },
        other => panic!("{other} is not a kind of cable: {text}"),
    };
    let endpoint = model.endpoint_of(at, &kind).unwrap_or_else(|| panic!("{kind:?} is not a place of the rack at {at:?}: {text}"));
    let setting = |field| Driven::Setting { modulator: endpoint.node, field };
    let aim = match endpoint.port {
        Port::Param { index } => Driven::Param { plugin: endpoint.node, index },
        Port::Rate => setting(ModSetting::Rate),
        Port::Attack => setting(ModSetting::Attack),
        Port::Decay => setting(ModSetting::Decay),
        Port::Sustain => setting(ModSetting::Sustain),
        Port::Release => setting(ModSetting::Release),
        Port::Depth { edge } => Driven::Depth(edge),
        port => panic!("{port:?} is not what a cable drives"),
    };
    format!("{head}aim={aim:?}{}", &rest[close + 1..])
}

/// `told` for each of `texts`: the commands that a test expects, in order.
pub fn told_all(model: &SessionModel, texts: &[&str]) -> Vec<String> {
    texts.iter().map(|text| told(model, text)).collect()
}

pub fn model_groups(model: &SessionModel) -> Vec<GroupFacts> {
    model
        .groups
        .iter()
        .map(|group| GroupFacts {
            node: group.node,
            effect_nodes: group.effects.iter().map(|effect| effect.node).collect(),
            cut_feeds: group.effects.iter().filter(|effect| !effect.fed).map(|effect| effect.node).collect(),
            effects: group.effects.iter().map(|effect| effect.name.clone()).collect(),
            modulators: group.modulators.len(),
            volume: group.volume,
            pan: group.pan,
            muted: group.muted,
            solo: group.solo,
        })
        .collect()
}

