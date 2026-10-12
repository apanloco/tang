//! Test helpers shared by the app and TUI tests.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread::{self, ThreadId};

use crossbeam_channel::{Receiver, Sender};

use crate::app::controller::EngineHandles;
use crate::app::load::{Progress, load_session};
use crate::app::model::SessionModel;
use crate::audio::MidiEvent;
use crate::held_notes::HeldNotes;
use crate::midi_monitor::MidiMonitor;
use crate::piano_filter::{PianoFilter, PianoMode};
use crate::plugin::chain::{GraphCommand, MidiLearnedNotification, PatternNotification};
use crate::scale::ScaleSetting;
use crate::telemetry::Telemetry;
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
        C::LoadPreset { inst, slot, preset_id } => format!("LoadPreset inst={inst} slot={slot} id={preset_id}"),
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
        C::SetModulatorOutput { location, index, slot } => {
            format!("SetModulatorOutput {location:?} index={index} slot={}", slot.index())
        }
        C::SetLaneMeter { inst, slots } => {
            format!("SetLaneMeter inst={inst} peak={} clips={}", slots.peak.index(), slots.clips.index())
        }
        C::SetGroupMeter { group, slots } => {
            format!("SetGroupMeter group={group} peak={} clips={}", slots.peak.index(), slots.clips.index())
        }
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

/// A command that only assigns a telemetry slot to a node a command before it
/// created. They say nothing about the session, and a test that lists what was
/// sent would have to repeat one after every modulator, so `described` leaves
/// them out, and `StubEngine::drain` and `Run::telemetry` have them on their own.
fn is_telemetry(cmd: &GraphCommand) -> bool {
    matches!(
        cmd,
        GraphCommand::SetModulatorOutput { .. } | GraphCommand::SetLaneMeter { .. } | GraphCommand::SetGroupMeter { .. }
    )
}

/// What `describe` says about each of `commands`, except those that only assign
/// telemetry slots (see `is_telemetry`).
pub fn described(commands: impl IntoIterator<Item = GraphCommand>) -> Vec<String> {
    commands.into_iter().filter(|c| !is_telemetry(c)).map(|c| describe(&c)).collect()
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
        let (slot_commands, others): (Vec<GraphCommand>, Vec<GraphCommand>) = self.cmd_rx.try_iter().partition(is_telemetry);
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
    let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(64);
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
