//! The controller: the one owner of a running session.
//!
//! Whichever thread runs a front-end (the TUI loop, the `play` loop, later a
//! headless server) owns a `Controller` and calls `poll` every iteration. It is
//! a plain struct, no threads inside; see `docs/web-ui.md` §3.1. It holds
//!
//! - **the session**: the model of what the loader put into the engine, the
//!   file it came from, whether it has changed since, and the tempo;
//! - **the engine's handles** (`EngineHandles`): the command channel to the
//!   audio thread, the plugin runtime, the notification receivers and the state
//!   shared with the note senders;
//! - **the plugin catalog**, scanned on a background thread (`app::catalog`);
//!
//! and it does the housekeeping every front-end used to do for itself, or
//! forget:
//!
//! - **Notifications.** What the audio thread reports (a pattern recording that
//!   has finished, a control a MIDI-learn modulator has captured) is folded
//!   into the model, and what the catalog scan finds into the catalog.
//!   `poll` returns what changed, so that a front-end redraws only what it
//!   shows.
//! - **Reaping.** When the audio thread swaps or removes a plugin it hands the
//!   old one back over a channel so that it is dropped here, on the owner
//!   thread. Dropping a plugin can take milliseconds and free memory, which the
//!   audio thread must not do. If nobody reads the channel it fills up (16
//!   slots), and from then on the audio thread drops them itself.
//! - **Device hot-plug.** New MIDI inputs are looked for once a second.
//!
//! The fields are public while the front-ends still edit the model themselves;
//! they close as those edits become intents (`docs/web-ui.md` step 6).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};

use crate::app::catalog::Catalog;
use crate::app::load::fresh_slot;
use crate::app::model::{
    GroupNode, InstrumentNode, ModSourceSlot, ModTargetSlot, ModulatorSlot, PatternState, PluginSlot, SessionModel,
    mod_source_slot_to_graph,
};
use crate::app::save::to_save;
use crate::audio::MidiEvent;
use crate::held_notes::HeldNotes;
use crate::midi_monitor::MidiMonitor;
use crate::piano_filter::{PianoFilter, PianoMode};
use crate::plugin::chain::{
    GraphCommand, MidiLearnedNotification, ModTarget, ModTargetKind, ModulatorLocation, PatternNotification,
};
use crate::plugin::{self, Plugin, Runtime};
use crate::session::{self, PianoConfig};
use crate::telemetry::Telemetry;

/// How often `poll` looks for newly connected devices.
const DEVICE_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// The tempo of a session none of whose patterns has been recorded.
const DEFAULT_BPM: f32 = 120.0;

/// What a `poll` changed, for the front-end to redraw from.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Polled {
    /// The model changed: a recording finished or a control was captured.
    pub model_changed: bool,
    /// Plugins joined the catalog.
    pub catalog_changed: bool,
}

/// Something that notices devices being plugged in: the MIDI manager.
pub trait DeviceWatcher {
    /// Open whatever has been connected since the last call.
    fn poll_new_devices(&mut self);
}

impl DeviceWatcher for crate::midi::MidiManager {
    fn poll_new_devices(&mut self) {
        crate::midi::MidiManager::poll_new_devices(self)
    }
}

/// The channels to and from the running engine, and the state it shares with
/// the note senders: everything a `Controller` needs besides the session.
pub struct EngineHandles {
    /// Commands for the audio thread.
    pub cmd_tx: Sender<GraphCommand>,
    /// Notes for the audio thread, from the MIDI devices and the virtual piano.
    pub midi_tx: Sender<MidiEvent>,
    /// Plugins the audio thread has taken out of the graph.
    pub return_rx: Receiver<Box<dyn Plugin>>,
    /// Pattern recordings the audio thread has finished.
    pub pattern_rx: Receiver<PatternNotification>,
    /// Controls the audio thread has captured for a MIDI-learn modulator.
    pub learn_rx: Receiver<MidiLearnedNotification>,
    /// What plugins are loaded with (the shared LV2 world).
    pub runtime: Runtime,
    pub sample_rate: f32,
    pub max_block_size: usize,
    /// The notes currently held, written by every note source.
    pub held: Arc<HeldNotes>,
    /// The Piano tab's scale and Highlight / Locked mode, read by every note
    /// source.
    pub piano_filter: Arc<PianoFilter>,
    /// Incoming MIDI, tallied for `--debug`.
    pub midi_monitor: Arc<MidiMonitor>,
    /// What the audio thread publishes about itself: meters, for now.
    pub telemetry: Arc<Telemetry>,
}

/// A modulation target to bind (`Controller::add_target`): the parameter it
/// drives, or the modulator field, and how strongly.
pub struct NewTarget {
    pub kind: ModTargetKind,
    /// The chain slot the parameter is on (0 = the instrument, 1.. = its effects);
    /// the model keeps it to label the target with its plugin.
    pub slot: usize,
    pub param_name: String,
    pub depth: f32,
    /// The user's set value of the parameter, where its modulation is centred.
    pub base_value: f32,
    pub param_min: f32,
    pub param_max: f32,
}

pub struct Controller {
    /// The instrument lanes and the submix groups.
    pub model: SessionModel,
    /// Where `Ctrl+S` writes.
    pub session_path: Option<PathBuf>,
    /// The session differs from what was last saved.
    pub dirty: bool,
    /// The tempo patterns are recorded and played at.
    pub global_bpm: f32,
    /// How many intents have been applied (`Controller::apply`).
    pub rev: u64,
    /// The installed plugins, for the selector popups. Empty until
    /// `catalog.scan()` is called: sessions that never open a selector (`play`)
    /// do not pay for it.
    pub catalog: Catalog,

    pub cmd_tx: Sender<GraphCommand>,
    pub midi_tx: Sender<MidiEvent>,
    pub pattern_rx: Receiver<PatternNotification>,
    pub learn_rx: Receiver<MidiLearnedNotification>,
    pub runtime: Runtime,
    pub sample_rate: f32,
    pub max_block_size: usize,
    pub held: Arc<HeldNotes>,
    pub piano_filter: Arc<PianoFilter>,
    pub midi_monitor: Arc<MidiMonitor>,
    pub telemetry: Arc<Telemetry>,

    /// Plugins the audio thread has taken out of the graph.
    return_rx: Receiver<Box<dyn Plugin>>,
    devices: Option<Box<dyn DeviceWatcher>>,
    last_device_poll: Instant,
}

/// The tempo a session starts at: that of the first lane with a recorded
/// pattern, else `DEFAULT_BPM`.
fn initial_bpm(model: &SessionModel) -> f32 {
    model
        .instruments
        .iter()
        .find_map(|lane| lane.pattern.as_ref())
        .map_or(DEFAULT_BPM, |pattern| pattern.bpm)
}

impl Controller {
    /// A controller for a freshly loaded session: nothing has changed yet.
    pub fn new(model: SessionModel, engine: EngineHandles, session_path: Option<PathBuf>) -> Self {
        Self::starting_at(model, engine, session_path, Instant::now())
    }

    fn starting_at(
        model: SessionModel,
        engine: EngineHandles,
        session_path: Option<PathBuf>,
        now: Instant,
    ) -> Self {
        let EngineHandles {
            cmd_tx,
            midi_tx,
            return_rx,
            pattern_rx,
            learn_rx,
            runtime,
            sample_rate,
            max_block_size,
            held,
            piano_filter,
            midi_monitor,
            telemetry,
        } = engine;
        Controller {
            global_bpm: initial_bpm(&model),
            rev: 0,
            catalog: Catalog::default(),
            model,
            session_path,
            dirty: false,
            cmd_tx,
            midi_tx,
            pattern_rx,
            learn_rx,
            runtime,
            sample_rate,
            max_block_size,
            held,
            piano_filter,
            midi_monitor,
            telemetry,
            return_rx,
            devices: None,
            last_device_poll: now,
        }
    }

    /// Load `source` (a plugin path or id) with the model slot that describes it,
    /// every parameter at its default, in a node of its own. The plugin is for the
    /// audio thread; None, after logging why, if it does not load. (An
    /// instrument takes its lane's node instead, in `set_instrument`.)
    pub fn load_plugin(&mut self, source: &str) -> Option<(Box<dyn Plugin>, PluginSlot)> {
        let loaded = match plugin::load(source, self.sample_rate, self.max_block_size, &self.runtime) {
            Ok(p) => p,
            Err(e) => {
                log::error!("Failed to load plugin '{source}': {e}");
                return None;
            }
        };
        let slot = fresh_slot(loaded.as_ref(), source, self.model.alloc_node());
        Some((loaded, slot))
    }

    /// Make `loaded`/`slot` the instrument plugin of lane `inst`, on the audio
    /// thread and in the model. The plugin sits in the lane's node.
    pub fn set_instrument(&mut self, inst: usize, loaded: Box<dyn Plugin>, mut slot: PluginSlot) {
        let inst_buf = (0..loaded.audio_output_count()).map(|_| Vec::new()).collect();
        let _ = self.cmd_tx.send(GraphCommand::SwapInstrument {
            inst,
            instrument: loaded,
            inst_buf,
            remapper: None,
        });
        if let Some(lane) = self.model.instruments.get_mut(inst) {
            slot.node = lane.node;
            lane.instrument = Some(slot);
        }
    }

    /// Add an empty instrument lane at the end, full range and in no group: the
    /// audio thread gets it with a meter, the model a node of its own for it.
    /// Returns its index. (The lane makes no sound until an instrument is put in.)
    pub fn add_lane(&mut self) -> usize {
        let inst = self.model.instruments.len();
        let node = self.model.alloc_node();
        let _ = self.cmd_tx.send(GraphCommand::AddInstrument { range: None });
        let meter = self.model.slots.meter();
        if let Some(slots) = meter {
            let _ = self.cmd_tx.send(GraphCommand::SetLaneMeter { inst, slots });
        }
        self.model.instruments.push(InstrumentNode {
            node,
            range: None,
            transpose: 0,
            volume: 1.0,
            instrument: None,
            effects: Vec::new(),
            modulators: Vec::new(),
            pattern: None,
            pitch_bend_range: 2.0,
            remap: Default::default(),
            group: None,
            meter,
        });
        inst
    }

    /// Add an empty submix group at the end, with a meter: the audio thread gets
    /// it and the model a node of its own for it. Returns its index.
    pub fn add_group(&mut self) -> usize {
        let group = self.model.groups.len();
        let node = self.model.alloc_node();
        let _ = self.cmd_tx.send(GraphCommand::AddGroup);
        let meter = self.model.slots.meter();
        if let Some(slots) = meter {
            let _ = self.cmd_tx.send(GraphCommand::SetGroupMeter { group, slots });
        }
        self.model.groups.push(GroupNode {
            node,
            name: None,
            volume: 1.0,
            pan: 0.0,
            effects: Vec::new(),
            modulators: Vec::new(),
            meter,
        });
        group
    }

    /// Add a modulator with `source` at the end of the rack at `at` (a lane's or a
    /// group's): the audio thread gets it, the model a node of its own for it.
    /// Returns its index in the rack, or `None`, sending nothing, if there is no
    /// such rack.
    pub fn add_modulator(&mut self, at: ModulatorLocation, source: ModSourceSlot) -> Option<usize> {
        let index = self.model.modulators_mut(at)?.len();
        let graph_source = mod_source_slot_to_graph(&source);
        let _ = self.cmd_tx.send(match at {
            ModulatorLocation::Lane(inst) => GraphCommand::InsertModulator { inst, index, source: graph_source },
            ModulatorLocation::Group(group) => {
                GraphCommand::InsertGroupModulator { group, index, source: graph_source }
            }
        });
        // Its output is published from now on.
        let output = self.model.slots.alloc();
        if let Some(slot) = output {
            let _ = self.cmd_tx.send(GraphCommand::SetModulatorOutput { location: at, index, slot });
        }
        let node = self.model.alloc_node();
        self.model.modulators_mut(at)?.push(ModulatorSlot { node, source, targets: Vec::new(), output });
        Some(index)
    }

    /// Bind `new` as a target of modulator `index` in the rack at `at`, on the audio
    /// thread and in the model. False, sending nothing, if there is no such
    /// modulator.
    pub fn add_target(&mut self, at: ModulatorLocation, index: usize, new: NewTarget) -> bool {
        if self.model.modulators_mut(at).and_then(|rack| rack.get(index)).is_none() {
            return false;
        }
        // A target on a plugin's parameter publishes the value that parameter is set to.
        let effective = if new.kind.has_effective_value() { self.model.slots.alloc() } else { None };
        let target = ModTarget {
            kind: new.kind.clone(),
            depth: new.depth,
            base_value: new.base_value,
            param_min: new.param_min,
            param_max: new.param_max,
            effective,
        };
        let Some(modulator) = self.model.modulators_mut(at).and_then(|rack| rack.get_mut(index)) else {
            return false;
        };
        modulator.targets.push(ModTargetSlot {
            slot: new.slot,
            param_name: new.param_name,
            kind: new.kind,
            depth: new.depth,
            param_min: new.param_min,
            param_max: new.param_max,
            effective,
        });
        let _ = self.cmd_tx.send(match at {
            ModulatorLocation::Lane(inst) => GraphCommand::AddModTarget { inst, mod_index: index, target },
            ModulatorLocation::Group(group) => GraphCommand::AddGroupModTarget { group, mod_index: index, target },
        });
        true
    }

    /// Save the session to the file it came from.
    pub fn save(&mut self) -> anyhow::Result<()> {
        let Some(path) = self.session_path.clone() else {
            anyhow::bail!("No session path — cannot save");
        };
        self.save_to(&path)
    }

    /// Save the session as `name` in the directory of its file (the working
    /// directory if it has none), with `.toml` appended unless `name` already
    /// ends in it, and from then on save to that file. Returns the path written.
    /// When it fails nothing changes: the session keeps its file and its mark.
    pub fn save_as(&mut self, name: &str) -> anyhow::Result<PathBuf> {
        let path = self.save_as_path(name)?;
        self.save_to(&path)?;
        self.session_path = Some(path.clone());
        Ok(path)
    }

    fn save_as_path(&self, name: &str) -> anyhow::Result<PathBuf> {
        let mut name = name.trim().to_string();
        anyhow::ensure!(!name.is_empty(), "A file name is needed");
        if !name.ends_with(".toml") {
            name.push_str(".toml");
        }
        let dir = self.session_path.as_deref().and_then(Path::parent).unwrap_or(Path::new("."));
        Ok(dir.join(name))
    }

    /// Write the session, with the Piano tab's scale and mode, to `path`. Does
    /// not touch `session_path`: callers decide what a failed save means for it.
    fn save_to(&mut self, path: &Path) -> anyhow::Result<()> {
        let (instruments, groups) = to_save(&self.model.instruments, &self.model.groups);
        let piano = PianoConfig {
            scale: Some(self.piano_filter.scale().short()),
            locked: matches!(self.piano_filter.mode(), PianoMode::Locked),
        };
        session::save(path, &instruments, &groups, Some(&piano))?;
        self.dirty = false;
        log::info!("Session saved to {}", path.display());
        Ok(())
    }

    /// Have `poll` look for newly connected devices through `watcher`.
    pub fn watch_devices(&mut self, watcher: Box<dyn DeviceWatcher>) {
        self.devices = Some(watcher);
    }

    /// Housekeeping, to be called on every iteration of the front-end's loop.
    pub fn poll(&mut self) -> Polled {
        self.poll_at(Instant::now())
    }

    fn poll_at(&mut self, now: Instant) -> Polled {
        self.reap();
        if now.duration_since(self.last_device_poll) >= DEVICE_POLL_INTERVAL {
            self.last_device_poll = now;
            if let Some(devices) = &mut self.devices {
                devices.poll_new_devices();
            }
        }
        Polled {
            catalog_changed: self.catalog.poll(),
            model_changed: self.fold_in_notifications(),
        }
    }

    /// Fold what the audio thread has reported into the model. True if the
    /// model changed.
    fn fold_in_notifications(&mut self) -> bool {
        let mut changed = false;
        while let Ok(recording) = self.pattern_rx.try_recv() {
            changed |= self.finish_recording(recording);
        }
        while let Ok(capture) = self.learn_rx.try_recv() {
            changed |= self.bind_capture(capture);
        }
        changed
    }

    /// A pattern recording has finished: it becomes the lane's pattern, at the
    /// tempo it was recorded at. One for a lane that is gone changes nothing.
    fn finish_recording(&mut self, recording: PatternNotification) -> bool {
        let Some(lane) = self.model.instruments.get_mut(recording.inst) else {
            return false;
        };
        // Re-recording replaces the events but keeps the transpose mode.
        let in_key = lane.pattern.as_ref().is_some_and(|p| p.in_key);
        lane.pattern = Some(PatternState {
            bpm: self.global_bpm,
            length_beats: recording.length_beats,
            looping: recording.looping,
            base_note: recording.base_note,
            events: recording.events,
            enabled: recording.enabled,
            recording: false,
            in_key,
        });
        self.dirty = true;
        true
    }

    /// A MIDI-learn modulator has captured a control: bind it. One for a
    /// modulator that is gone, or no longer listening, changes nothing.
    fn bind_capture(&mut self, capture: MidiLearnedNotification) -> bool {
        let Some(modulator) = self
            .model
            .modulators_mut(capture.location)
            .and_then(|rack| rack.get_mut(capture.mod_index))
        else {
            return false;
        };
        let ModSourceSlot::MidiLearn { source, learning } = &mut modulator.source else {
            return false;
        };
        *source = Some(capture.source);
        *learning = false;
        self.dirty = true;
        true
    }

    /// Drop the plugins the audio thread has handed back.
    fn reap(&self) {
        while self.return_rx.try_recv().is_ok() {}
    }

    /// Close the devices, then drop what is left in the return channel, and the
    /// plugin runtime after that. Call it once the audio stream has stopped, so
    /// that nothing is still calling into a plugin.
    pub fn shutdown(mut self) {
        self.devices = None;
        self.reap();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;
    use std::sync::{Arc, Mutex};
    use std::thread;

    use super::*;
    use crate::app::testutil::{self, DropProbe, StubEngine, load, recording};
    use crate::plugin::chain::{AudioGraph, GraphCommand, MidiBindSource, ModulatorLocation};

    fn controller() -> (StubEngine, Controller) {
        let (handles, engine) = testutil::stub_engine();
        (engine, Controller::new(SessionModel::default(), handles, None))
    }

    #[test]
    fn poll_drops_returned_plugins_on_the_calling_thread() {
        let (engine, mut ctl) = controller();
        let (probe, dropped_on) = DropProbe::recording();
        // The "audio thread" hands a plugin back and goes away.
        let tx = engine.return_tx.clone();
        let audio_thread = thread::spawn(move || tx.send(Box::new(probe) as Box<dyn Plugin>).unwrap());
        audio_thread.join().unwrap();
        let before = *dropped_on.lock().unwrap();
        assert!(before.is_none(), "nothing is dropped until someone reaps");

        ctl.poll();
        let after = *dropped_on.lock().unwrap();
        assert_eq!(after, Some(thread::current().id()));
    }

    #[test]
    fn poll_reaps_what_the_audio_graph_hands_back() {
        // The real producer: swapping an instrument returns the old one.
        let (engine, mut ctl) = controller();
        let mut graph = AudioGraph::new(2, engine.cmd_rx.clone(), engine.return_tx.clone());
        let swap = |plugin: Box<dyn Plugin>| GraphCommand::SwapInstrument {
            inst: 0,
            instrument: plugin,
            inst_buf: vec![Vec::new(), Vec::new()],
            remapper: None,
        };
        ctl.cmd_tx.send(GraphCommand::AddInstrument { range: None }).unwrap();
        let osc = |n: usize| (0..n).map(|_| crate::plugin::load("builtin:osc", 48_000.0, 256, &Default::default()).unwrap());
        for plugin in osc(3) {
            ctl.cmd_tx.send(swap(plugin)).unwrap();
        }
        graph.drain_commands();
        assert_eq!(engine.return_tx.len(), 2, "the second and third swap each hand one back");

        ctl.poll();
        assert_eq!(engine.return_tx.len(), 0);
    }

    #[test]
    fn devices_are_polled_once_a_second() {
        struct Watcher(Rc<Cell<usize>>);
        impl DeviceWatcher for Watcher {
            fn poll_new_devices(&mut self) {
                self.0.set(self.0.get() + 1);
            }
        }
        let t0 = Instant::now();
        let (handles, _engine) = testutil::stub_engine();
        let mut ctl = Controller::starting_at(SessionModel::default(), handles, None, t0);
        let polls = Rc::new(Cell::new(0));
        ctl.watch_devices(Box::new(Watcher(polls.clone())));

        let at = |ms: u64| t0 + Duration::from_millis(ms);
        for (ms, expected) in [(100, 0), (999, 0), (1000, 1), (1500, 1), (1999, 1), (2000, 2), (5000, 3)] {
            ctl.poll_at(at(ms));
            assert_eq!(polls.get(), expected, "after {ms} ms");
        }
    }

    #[test]
    fn polling_without_a_device_watcher_is_fine() {
        let (_engine, mut ctl) = controller();
        ctl.poll_at(Instant::now() + Duration::from_secs(10));
    }

    #[test]
    fn shutdown_closes_the_devices_before_it_drops_what_was_handed_back() {
        struct Watcher(Arc<Mutex<Vec<&'static str>>>);
        impl DeviceWatcher for Watcher {
            fn poll_new_devices(&mut self) {}
        }
        impl Drop for Watcher {
            fn drop(&mut self) {
                self.0.lock().unwrap_or_else(|e| e.into_inner()).push("devices closed");
            }
        }
        let log = Arc::new(Mutex::new(Vec::new()));
        let (engine, mut ctl) = controller();
        ctl.watch_devices(Box::new(Watcher(log.clone())));
        let probe_log = log.clone();
        let probe = DropProbe::new(move |_| probe_log.lock().unwrap_or_else(|e| e.into_inner()).push("plugin dropped"));
        engine.return_tx.send(Box::new(probe)).unwrap();

        ctl.shutdown();
        let order = log.lock().unwrap().clone();
        assert_eq!(order, ["devices closed", "plugin dropped"]);
    }

    fn tempo_of(session: &str) -> f32 {
        let (handles, _engine) = testutil::stub_engine();
        Controller::new(load(session).model, handles, None).global_bpm
    }

    #[test]
    fn the_tempo_starts_at_the_first_recorded_patterns() {
        let lane = |bpm: &str| {
            format!(
                "[[instrument]]\nplugin = \"builtin:osc\"\n[instrument.pattern]\nbpm = {bpm}\nlength_beats = 4.0\nenabled = true\n"
            )
        };
        let plain = "[[instrument]]\nplugin = \"builtin:osc\"\n";
        // The first lane that has a pattern decides, not the first lane.
        assert_eq!(tempo_of(&format!("{plain}{}{}", lane("90.0"), lane("140.0"))), 90.0);
        assert_eq!(tempo_of(&lane("140.0")), 140.0);
    }

    #[test]
    fn the_tempo_is_120_without_a_recorded_pattern() {
        assert_eq!(tempo_of("[[instrument]]\nplugin = \"builtin:osc\"\n"), 120.0);
        assert_eq!(tempo_of(""), 120.0);
    }

    #[test]
    fn a_new_controller_has_nothing_to_save() {
        let (handles, _engine) = testutil::stub_engine();
        let ctl = Controller::new(load(testutil::GROUPS).model, handles, Some(PathBuf::from("song.toml")));
        assert!(!ctl.dirty);
        assert_eq!(ctl.session_path, Some(PathBuf::from("song.toml")));
        assert!(!ctl.model.instruments.is_empty(), "the model is the one it was given");
    }

    fn controller_for(session: &str) -> (StubEngine, Controller) {
        let (handles, engine) = testutil::stub_engine();
        (engine, Controller::new(load(session).model, handles, None))
    }

    const TWO_LANES: &str = "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument]]\nplugin = \"builtin:osc\"\n";

    const CHANGED_MODEL: Polled = Polled { model_changed: true, catalog_changed: false };

    #[test]
    fn polling_with_nothing_to_report_changes_nothing() {
        let (_engine, mut ctl) = controller_for(TWO_LANES);
        assert_eq!(ctl.poll(), Polled::default());
        assert!(!ctl.dirty);
    }

    #[test]
    fn a_finished_recording_becomes_the_lanes_pattern() {
        let (engine, mut ctl) = controller_for(TWO_LANES);
        // It carries the tempo the session has when the recording ends, and the
        // player's loop setting.
        ctl.global_bpm = 95.0;
        engine.pattern_tx.send(PatternNotification { looping: false, ..recording(1) }).unwrap();
        assert_eq!(ctl.poll(), CHANGED_MODEL);

        let pattern = ctl.model.instruments[1].pattern.as_ref().expect("lane 1 has a pattern now");
        assert_eq!((pattern.bpm, pattern.length_beats, pattern.looping, pattern.enabled), (95.0, 2.0, false, true));
        assert_eq!((pattern.base_note, pattern.in_key, pattern.recording), (Some(60), false, false));
        assert_eq!(pattern.events, [(0, 0x90, 60, 100), (24_000, 0x80, 60, 0)]);
        assert!(ctl.model.instruments[0].pattern.is_none());
        assert!(ctl.dirty, "the recording is part of the session");
        assert_eq!(ctl.poll(), Polled::default(), "it is reported once");
    }

    #[test]
    fn re_recording_replaces_the_events_but_keeps_the_transpose_mode() {
        let (engine, mut ctl) = controller_for(
            r#"
[[instrument]]
plugin = "builtin:osc"
[instrument.pattern]
bpm = 120.0
length_beats = 4.0
base_note = "C4"
enabled = false
looping = false
transpose = "in_key"
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
"#,
        );
        engine.pattern_tx.send(recording(0)).unwrap();
        assert_eq!(ctl.poll(), CHANGED_MODEL);

        let pattern = ctl.model.instruments[0].pattern.as_ref().unwrap();
        assert!(pattern.in_key, "the transpose mode is the user's, not the recording's");
        assert_eq!((pattern.length_beats, pattern.looping, pattern.enabled), (2.0, true, true));
        assert_eq!(pattern.events, [(0, 0x90, 60, 100), (24_000, 0x80, 60, 0)]);
    }

    #[test]
    fn a_recording_with_no_notes_leaves_a_disabled_pattern() {
        // The user pressed `r` and played nothing: the player keeps an empty,
        // disabled pattern, and so does the model.
        let (engine, mut ctl) = controller_for(TWO_LANES);
        engine
            .pattern_tx
            .send(PatternNotification { base_note: None, enabled: false, events: vec![], ..recording(1) })
            .unwrap();
        assert_eq!(ctl.poll(), CHANGED_MODEL);

        let pattern = ctl.model.instruments[1].pattern.as_ref().expect("the lane still gets a pattern");
        assert!(!pattern.enabled);
        assert!(pattern.events.is_empty());
        assert_eq!(pattern.base_note, None);
    }

    #[test]
    fn a_recording_for_a_lane_that_is_gone_changes_nothing() {
        let (engine, mut ctl) = controller_for(TWO_LANES);
        engine.pattern_tx.send(recording(7)).unwrap();
        assert_eq!(ctl.poll(), Polled::default());
        assert!(ctl.model.instruments.iter().all(|lane| lane.pattern.is_none()));
        assert!(!ctl.dirty, "nothing changed");
    }

    #[test]
    fn recordings_are_folded_in_in_the_order_they_were_sent() {
        let (engine, mut ctl) = controller_for(TWO_LANES);
        engine.pattern_tx.send(PatternNotification { length_beats: 2.0, ..recording(0) }).unwrap();
        engine.pattern_tx.send(PatternNotification { length_beats: 3.0, ..recording(0) }).unwrap();
        engine.pattern_tx.send(recording(1)).unwrap();
        engine.pattern_tx.send(recording(7)).unwrap(); // changes nothing, and must not hide the rest
        assert_eq!(ctl.poll(), CHANGED_MODEL);
        assert_eq!(ctl.model.instruments[0].pattern.as_ref().unwrap().length_beats, 3.0, "the later one wins");
        assert!(ctl.model.instruments[1].pattern.is_some(), "all of them are drained in one poll");
    }

    /// Lane 0 has an armed MIDI-learn modulator (index 0) and an LFO (index 1);
    /// its group has an armed one too.
    const ARMED: &str = r#"
[[group]]
[[group.modulator]]
type = "midi"
[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.modulator]]
type = "midi"
[[instrument.modulator]]
type = "lfo"
"#;

    fn capture(location: ModulatorLocation, mod_index: usize, source: MidiBindSource) -> MidiLearnedNotification {
        MidiLearnedNotification { location, mod_index, source }
    }

    #[test]
    fn a_captured_control_binds_the_armed_lane_modulator() {
        for source in [MidiBindSource::Cc(74), MidiBindSource::PitchBend, MidiBindSource::Aftertouch] {
            let (engine, mut ctl) = controller_for(ARMED);
            assert!(matches!(ctl.model.instruments[0].modulators[0].source, ModSourceSlot::MidiLearn { source: None, learning: true }));
            engine.learn_tx.send(capture(ModulatorLocation::Lane(0), 0, source)).unwrap();
            assert_eq!(ctl.poll(), CHANGED_MODEL, "{source:?}");

            let bound = &ctl.model.instruments[0].modulators[0].source;
            assert!(
                matches!(bound, ModSourceSlot::MidiLearn { source: Some(s), learning: false } if *s == source),
                "{source:?}: {bound:?}"
            );
            assert!(ctl.dirty, "{source:?}: the binding is part of the saved session");
            assert!(matches!(ctl.model.groups[0].modulators[0].source, ModSourceSlot::MidiLearn { source: None, learning: true }), "the group's is untouched");
        }
    }

    #[test]
    fn a_captured_control_binds_the_armed_group_modulator() {
        let (engine, mut ctl) = controller_for(ARMED);
        engine.learn_tx.send(capture(ModulatorLocation::Group(0), 0, MidiBindSource::Cc(1))).unwrap();
        assert_eq!(ctl.poll(), CHANGED_MODEL);
        assert!(matches!(
            ctl.model.groups[0].modulators[0].source,
            ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Cc(1)), learning: false }
        ));
        assert!(matches!(ctl.model.instruments[0].modulators[0].source, ModSourceSlot::MidiLearn { source: None, learning: true }), "the lane's is untouched");
        assert!(ctl.dirty);
    }

    #[test]
    fn a_capture_with_no_modulator_to_take_it_changes_nothing() {
        let (engine, mut ctl) = controller_for(ARMED);
        for (location, mod_index) in [
            (ModulatorLocation::Lane(0), 1), // the LFO is not listening
            (ModulatorLocation::Lane(0), 9), // no such modulator
            (ModulatorLocation::Lane(7), 0), // no such lane
            (ModulatorLocation::Group(0), 9),
            (ModulatorLocation::Group(7), 0),
        ] {
            engine.learn_tx.send(capture(location, mod_index, MidiBindSource::PitchBend)).unwrap();
        }
        assert_eq!(ctl.poll(), Polled::default());
        assert!(!ctl.dirty, "nothing changed");
        assert!(matches!(ctl.model.instruments[0].modulators[1].source, ModSourceSlot::Lfo { .. }));
        assert!(matches!(ctl.model.instruments[0].modulators[0].source, ModSourceSlot::MidiLearn { source: None, learning: true }));
    }

    #[test]
    fn captures_are_folded_in_in_the_order_they_were_sent() {
        let (engine, mut ctl) = controller_for(ARMED);
        // The model mirrors the audio thread, which may learn a modulator again:
        // the last word on lane 0's modulator is Cc(2). The group's comes after.
        engine.learn_tx.send(capture(ModulatorLocation::Lane(0), 0, MidiBindSource::Cc(1))).unwrap();
        engine.learn_tx.send(capture(ModulatorLocation::Lane(0), 0, MidiBindSource::Cc(2))).unwrap();
        engine.learn_tx.send(capture(ModulatorLocation::Group(0), 0, MidiBindSource::Cc(3))).unwrap();
        assert_eq!(ctl.poll(), CHANGED_MODEL);
        assert!(matches!(
            ctl.model.instruments[0].modulators[0].source,
            ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Cc(2)), learning: false }
        ));
        assert!(
            matches!(
                ctl.model.groups[0].modulators[0].source,
                ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Cc(3)), learning: false }
            ),
            "all of them are drained in one poll"
        );
    }

    #[test]
    fn a_recording_and_a_capture_in_one_poll_are_both_reported() {
        let (engine, mut ctl) = controller_for(ARMED);
        engine.pattern_tx.send(recording(0)).unwrap();
        engine.learn_tx.send(capture(ModulatorLocation::Lane(0), 0, MidiBindSource::Cc(7))).unwrap();
        assert_eq!(ctl.poll(), CHANGED_MODEL);
        assert!(ctl.model.instruments[0].pattern.is_some());
        assert!(matches!(ctl.model.instruments[0].modulators[0].source, ModSourceSlot::MidiLearn { source: Some(_), .. }));

        // Either one alone is enough, whichever order they are drained in.
        engine.pattern_tx.send(recording(0)).unwrap();
        engine.learn_tx.send(capture(ModulatorLocation::Lane(7), 0, MidiBindSource::Cc(7))).unwrap();
        assert_eq!(ctl.poll(), CHANGED_MODEL, "the capture that finds nothing does not undo the recording");
        engine.pattern_tx.send(recording(7)).unwrap();
        engine.learn_tx.send(capture(ModulatorLocation::Group(0), 0, MidiBindSource::Cc(8))).unwrap();
        assert_eq!(ctl.poll(), CHANGED_MODEL, "the recording that finds nothing does not undo the capture");
    }

    fn controller_with_file(path: &Path) -> Controller {
        let (handles, _engine) = testutil::stub_engine();
        Controller::new(load(testutil::GROUPS).model, handles, Some(path.to_path_buf()))
    }

    #[test]
    fn save_writes_the_session_and_the_piano_settings_to_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("song.toml");
        let mut ctl = controller_with_file(&path);
        ctl.piano_filter.set_scale(crate::scale::ScaleSetting::parse("D dorian").unwrap());
        ctl.piano_filter.set_mode(PianoMode::Locked);
        ctl.dirty = true;
        ctl.save().unwrap();

        assert!(!ctl.dirty);
        let text = std::fs::read_to_string(&path).unwrap();
        let piano = session::parse(&text).unwrap().piano.expect("the piano settings are saved");
        assert_eq!((piano.scale.as_deref(), piano.locked), (Some("D dorian"), true));
        testutil::assert_same_model("the saved file", &ctl.model, &load(&text).model);
    }

    #[test]
    fn save_needs_a_file_and_keeps_the_mark_when_it_fails() {
        let (handles, _engine) = testutil::stub_engine();
        let mut ctl = Controller::new(SessionModel::default(), handles, None);
        ctl.dirty = true;
        assert!(ctl.save().is_err());
        assert!(ctl.dirty);

        // A file where the directory should be: the write fails.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("blocker"), "").unwrap();
        let mut ctl = controller_with_file(&dir.path().join("blocker").join("song.toml"));
        ctl.dirty = true;
        assert!(ctl.save().is_err());
        assert!(ctl.dirty, "a failed save has not saved anything");
    }

    #[test]
    fn save_as_writes_beside_the_current_file_and_switches_to_it() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("song.toml");
        let mut ctl = controller_with_file(&old);
        ctl.dirty = true;
        let new = ctl.save_as("copy").unwrap();

        assert_eq!(new, dir.path().join("copy.toml"));
        assert!(new.exists());
        assert!(!old.exists(), "the old file is not written");
        assert_eq!(ctl.session_path, Some(new));
        assert!(!ctl.dirty);
    }

    #[test]
    fn save_as_appends_toml_once_and_trims_the_name() {
        let ctl = controller_with_file(Path::new("/songs/old.toml"));
        for (name, expected) in [
            ("mix", "/songs/mix.toml"),
            ("mix.toml", "/songs/mix.toml"),
            ("  mix.toml  ", "/songs/mix.toml"),
            ("a.b", "/songs/a.b.toml"),
        ] {
            assert_eq!(ctl.save_as_path(name).unwrap(), PathBuf::from(expected), "{name:?}");
        }
    }

    #[test]
    fn save_as_without_a_current_file_means_the_working_directory() {
        let (handles, _engine) = testutil::stub_engine();
        let ctl = Controller::new(SessionModel::default(), handles, None);
        assert_eq!(ctl.save_as_path("mix").unwrap(), Path::new(".").join("mix.toml"));
        let ctl = controller_with_file(Path::new("old.toml"));
        assert_eq!(ctl.save_as_path("mix").unwrap(), Path::new("").join("mix.toml"), "a bare file name has no directory");
    }

    #[test]
    fn save_as_needs_a_name() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("song.toml");
        let mut ctl = controller_with_file(&old);
        ctl.dirty = true;
        for name in ["", "   "] {
            assert!(ctl.save_as(name).is_err(), "{name:?}");
        }
        assert_eq!(ctl.session_path, Some(old));
        assert!(ctl.dirty);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_failed_save_as_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("blocker"), "").unwrap();
        let old = dir.path().join("song.toml");
        let mut ctl = controller_with_file(&old);
        ctl.dirty = true;
        assert!(ctl.save_as("blocker/x").is_err());
        assert_eq!(ctl.session_path, Some(old), "the session keeps its file");
        assert!(ctl.dirty);
    }

    #[test]
    fn load_plugin_gives_the_plugin_and_the_slot_that_describes_it() {
        let (mut handles, _engine) = testutil::stub_engine();
        handles.sample_rate = 44_100.0;
        let mut ctl = Controller::new(SessionModel::default(), handles, None);
        let (plugin, slot) = ctl.load_plugin("builtin:osc").expect("a built-in loads");
        assert_eq!(plugin.sample_rate(), 44_100.0, "it is loaded at the session's sample rate");
        assert_eq!((slot.name.as_str(), slot.id.as_str(), slot.format.as_str()), ("Oscillator", "builtin:osc", "Built-in"));
        assert!(slot.is_instrument);
        assert_eq!(slot.params.len(), plugin.parameters().len());
    }

    #[test]
    fn each_loaded_plugin_gets_a_node_of_its_own() {
        let (_engine, mut ctl) = controller_for(TWO_LANES);
        let (_, first) = ctl.load_plugin("builtin:filter").unwrap();
        let (_, second) = ctl.load_plugin("builtin:filter").unwrap();
        assert_ne!(first.node, second.node);
        assert!(ctl.model.nodes().all(|(id, _)| id != first.node && id != second.node), "nor does any node in the session have them");
    }

    #[test]
    fn a_plugin_that_does_not_load_is_none() {
        let (_engine, mut ctl) = controller_for(TWO_LANES);
        assert!(ctl.load_plugin("builtin:no-such-plugin").is_none());
        assert!(ctl.load_plugin("./no-such-plugin.clap").is_none());
    }

    #[test]
    fn set_instrument_swaps_it_into_the_audio_thread_and_the_model() {
        let (engine, mut ctl) = controller_for(TWO_LANES);
        let (plugin, slot) = ctl.load_plugin("builtin:filter").unwrap();
        ctl.set_instrument(1, plugin, slot);

        assert_eq!(ctl.model.instruments[1].instrument.as_ref().unwrap().name, "Filter");
        assert_eq!(ctl.model.instruments[0].instrument.as_ref().unwrap().name, "Oscillator", "the other lane is untouched");
        let lane = &ctl.model.instruments[1];
        assert_eq!(lane.instrument.as_ref().unwrap().node, lane.node, "the plugin sits in the lane's node");
        ctl.model.check_ids().unwrap();
        let sent: Vec<String> = testutil::described(engine.cmd_rx.try_iter());
        assert_eq!(sent, ["SwapInstrument inst=1 plugin=Filter bufs=2 remap=false"]);
    }

    #[test]
    fn set_instrument_for_a_lane_that_is_gone_changes_no_lane() {
        let (_engine, mut ctl) = controller_for(TWO_LANES);
        let (plugin, slot) = ctl.load_plugin("builtin:filter").unwrap();
        ctl.set_instrument(9, plugin, slot);
        assert!(ctl.model.instruments.iter().all(|lane| lane.instrument.as_ref().unwrap().name == "Oscillator"));
    }

    #[test]
    fn plugins_the_scan_finds_are_reported_once() {
        let (handles, _engine) = testutil::stub_engine();
        let mut ctl = Controller::new(SessionModel::default(), handles, None);
        let (scan, batches) = crossbeam_channel::unbounded();
        ctl.catalog.follow(batches);
        let found = |name: &str| crate::plugin::PluginInfo {
            name: name.to_string(),
            id: format!("builtin:{name}"),
            is_instrument: false,
            param_count: 0,
            preset_count: 0,
            path: String::new(),
            scan_ms: 0,
        };
        scan.send(vec![found("Delay")]).unwrap();
        assert_eq!(ctl.poll(), Polled { model_changed: false, catalog_changed: true });
        assert_eq!(ctl.catalog.plugins().len(), 1);
        assert_eq!(ctl.poll(), Polled::default());
        assert!(!ctl.dirty, "the catalog is not part of the session");
    }

    // -- modulators and their targets --

    /// A group (one member) with a lane LFO and a group LFO, and a bare lane.
    const WITH_MODULATORS: &str = r#"
[[group]]
[[group.modulator]]
rate = 0.5

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.modulator]]
rate = 2.0

[[instrument]]
plugin = "builtin:osc"
"#;

    fn sent(engine: &StubEngine) -> Vec<String> {
        testutil::described(engine.cmd_rx.try_iter())
    }

    fn lfo_source() -> ModSourceSlot {
        ModSourceSlot::Lfo { waveform: crate::plugin::chain::LfoWaveform::Sine, rate: 1.0 }
    }

    fn cutoff_target(depth: f32) -> NewTarget {
        NewTarget {
            kind: crate::plugin::chain::ModTargetKind::PluginParam { slot: 1, param_index: 0 },
            slot: 1,
            param_name: "cutoff".to_string(),
            depth,
            base_value: 0.35,
            param_min: 0.0,
            param_max: 1.0,
        }
    }

    #[test]
    fn add_modulator_puts_one_at_the_end_of_a_lanes_rack_and_tells_the_audio_thread() {
        let (engine, mut ctl) = controller_for(WITH_MODULATORS);
        sent(&engine);
        assert_eq!(ctl.add_modulator(ModulatorLocation::Lane(0), lfo_source()), Some(1));
        assert_eq!(
            sent(&engine),
            ["InsertModulator inst=0 index=1 source=Lfo { waveform: Sine, rate: 1.0, phase: 0.0 }"]
        );
        let rack = &ctl.model.instruments[0].modulators;
        assert_eq!(rack.len(), 2);
        assert!(matches!(rack[1].source, ModSourceSlot::Lfo { rate, .. } if rate == 1.0));
        assert!(rack[1].targets.is_empty());
        assert_ne!(rack[1].node, rack[0].node, "a node of its own");
        ctl.model.check_ids().unwrap();
        assert!(ctl.model.instruments[1].modulators.is_empty(), "the other lane's rack is untouched");
    }

    #[test]
    fn add_modulator_in_a_groups_rack_and_of_each_source() {
        let (engine, mut ctl) = controller_for(WITH_MODULATORS);
        sent(&engine);
        let envelope = ModSourceSlot::Envelope { attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 };
        assert_eq!(ctl.add_modulator(ModulatorLocation::Group(0), envelope), Some(1));
        let learn = ModSourceSlot::MidiLearn { source: None, learning: true };
        assert_eq!(ctl.add_modulator(ModulatorLocation::Group(0), learn), Some(2));
        assert_eq!(
            sent(&engine),
            [
                "InsertGroupModulator group=0 index=1 source=Envelope { attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5, state: Idle, level: 0.0, notes_held: 0 }",
                "InsertGroupModulator group=0 index=2 source=MidiLearn { source: None, value: 0.0, learning: true }",
            ]
        );
        assert_eq!(ctl.model.groups[0].modulators.len(), 3);
        assert_eq!(ctl.model.instruments[0].modulators.len(), 1, "the lane's rack is untouched");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn add_modulator_for_a_rack_that_is_not_there_does_nothing() {
        let (engine, mut ctl) = controller_for(WITH_MODULATORS);
        sent(&engine);
        let slots_before = ctl.model.slots.high_water();
        assert_eq!(ctl.add_modulator(ModulatorLocation::Lane(9), lfo_source()), None);
        assert_eq!(ctl.add_modulator(ModulatorLocation::Group(9), lfo_source()), None);
        assert!(sent(&engine).is_empty());
        assert_eq!(ctl.model.slots.high_water(), slots_before, "no slot is used up for nothing");
    }

    #[test]
    fn add_target_binds_a_parameter_to_a_lane_modulator() {
        let (engine, mut ctl) = controller_for(WITH_MODULATORS);
        sent(&engine);
        assert!(ctl.add_target(ModulatorLocation::Lane(0), 0, cutoff_target(0.5)));
        assert_eq!(
            sent(&engine),
            ["AddModTarget inst=0 mod=0 kind=PluginParam { slot: 1, param_index: 0 } depth=0.500 base=0.350 range=[0.000, 1.000]"]
        );
        let target = &ctl.model.instruments[0].modulators[0].targets[0];
        assert_eq!((target.slot, target.param_name.as_str(), target.depth), (1, "cutoff", 0.5));
        assert_eq!((target.param_min, target.param_max), (0.0, 1.0));
        assert!(matches!(target.kind, crate::plugin::chain::ModTargetKind::PluginParam { slot: 1, param_index: 0 }));
        assert!(ctl.add_target(ModulatorLocation::Lane(0), 0, cutoff_target(-0.25)));
        assert_eq!(ctl.model.instruments[0].modulators[0].targets.len(), 2, "after the first");
    }

    #[test]
    fn add_target_binds_a_parameter_to_a_group_modulator() {
        let (engine, mut ctl) = controller_for(WITH_MODULATORS);
        sent(&engine);
        let target = NewTarget {
            kind: crate::plugin::chain::ModTargetKind::GroupMember { member: 0, slot: 1, param_index: 0 },
            ..cutoff_target(0.4)
        };
        assert!(ctl.add_target(ModulatorLocation::Group(0), 0, target));
        assert_eq!(
            sent(&engine),
            ["AddGroupModTarget group=0 mod=0 kind=GroupMember { member: 0, slot: 1, param_index: 0 } depth=0.400 base=0.350 range=[0.000, 1.000]"]
        );
        assert_eq!(ctl.model.groups[0].modulators[0].targets.len(), 1);
        assert!(ctl.model.instruments[0].modulators[0].targets.is_empty());
    }

    #[test]
    fn add_target_to_a_modulator_that_is_not_there_does_nothing() {
        let (engine, mut ctl) = controller_for(WITH_MODULATORS);
        sent(&engine);
        let slots_before = ctl.model.slots.high_water();
        assert!(!ctl.add_target(ModulatorLocation::Lane(0), 5, cutoff_target(0.5)));
        assert!(!ctl.add_target(ModulatorLocation::Lane(9), 0, cutoff_target(0.5)));
        assert!(!ctl.add_target(ModulatorLocation::Group(0), 3, cutoff_target(0.5)));
        assert!(sent(&engine).is_empty());
        assert_eq!(ctl.model.slots.high_water(), slots_before, "no slot is used up for nothing");
        assert!(ctl.model.instruments[0].modulators[0].targets.is_empty());
    }

    #[test]
    fn modulators_and_targets_do_not_mark_the_session_changed() {
        // The front-end says when an edit is a change: these only do the work.
        let (_engine, mut ctl) = controller_for(WITH_MODULATORS);
        ctl.add_modulator(ModulatorLocation::Lane(0), lfo_source());
        ctl.add_target(ModulatorLocation::Lane(0), 1, cutoff_target(0.5));
        assert!(!ctl.dirty);
    }

    #[test]
    fn add_modulator_gives_the_modulator_an_output_slot_and_tells_the_audio_thread_right_after() {
        let (engine, mut ctl) = controller_for(WITH_MODULATORS);
        engine.drain();
        ctl.add_modulator(ModulatorLocation::Lane(0), lfo_source());
        ctl.add_modulator(ModulatorLocation::Group(0), lfo_source());
        let lane = ctl.model.instruments[0].modulators[1].output.unwrap().index();
        let group = ctl.model.groups[0].modulators[1].output.unwrap().index();
        assert_ne!(lane, group);
        let (commands, telemetry) = engine.drain();
        assert_eq!(commands.len(), 2, "the slot commands are not among the rest: {commands:?}");
        assert_eq!(
            telemetry,
            [
                format!("SetModulatorOutput Lane(0) index=1 slot={lane}"),
                format!("SetModulatorOutput Group(0) index=1 slot={group}"),
            ]
        );
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_parameter_target_gets_an_effective_value_slot_and_a_modulator_field_target_none() {
        let (engine, mut ctl) = controller_for(WITH_MODULATORS);
        engine.drain();
        assert!(ctl.add_target(ModulatorLocation::Lane(0), 0, cutoff_target(0.5)));
        assert!(ctl.add_target(ModulatorLocation::Group(0), 0, NewTarget {
            kind: crate::plugin::chain::ModTargetKind::GroupBus { effect_index: 0, param_index: 0 },
            ..cutoff_target(0.5)
        }));
        let cross = NewTarget {
            kind: crate::plugin::chain::ModTargetKind::ModulatorRate { mod_index: 0 },
            slot: 0,
            param_name: "Mod 0 rate".to_string(),
            ..cutoff_target(0.5)
        };
        assert!(ctl.add_target(ModulatorLocation::Lane(0), 0, cross));
        let lane = &ctl.model.instruments[0].modulators[0].targets;
        let group = &ctl.model.groups[0].modulators[0].targets;
        assert!(lane[0].effective.is_some() && group[0].effective.is_some());
        assert_ne!(lane[0].effective, group[0].effective);
        assert!(lane[1].effective.is_none(), "a modulator's own field has no effective value");
        ctl.model.check_ids().unwrap();
        let (_, telemetry) = engine.drain();
        assert!(telemetry.is_empty(), "targets carry their slot, they are not assigned one");
    }

    #[test]
    fn a_target_bound_later_carries_its_slot_to_the_real_engine_too() {
        use crate::plugin::chain::LfoWaveform;
        let (handles, engine) = testutil::stub_engine();
        let model = testutil::load_onto(WITH_MODULATORS, &handles.cmd_tx);
        let mut graph = AudioGraph::new(2, engine.cmd_rx.clone(), engine.return_tx.clone());
        graph.set_telemetry(handles.telemetry.clone());
        let mut ctl = Controller::new(model, handles, None);
        // The lane's LFO (a 2 Hz sine) gets a target on the filter's cutoff,
        // centred on 0.35 and 0.5 deep.
        assert!(ctl.add_target(ModulatorLocation::Lane(0), 0, cutoff_target(0.5)));
        let mut out = vec![vec![0.0f32; 64]; 2];
        graph.process(&[], &mut out).unwrap();

        let lfo = &ctl.model.instruments[0].modulators[0];
        let published = ctl.telemetry.load(lfo.targets[0].effective.unwrap());
        let output = LfoWaveform::Sine.eval(2.0 * 64.0 / 48_000.0);
        assert!((published - (0.35 + 0.5 * output)).abs() < 1e-5, "{published} for an LFO output of {output}");
        assert_eq!(ctl.telemetry.load(lfo.output.unwrap()), output);
    }

    #[test]
    fn the_slots_the_model_holds_are_the_cells_the_real_engine_publishes_into() {
        use crate::plugin::chain::LfoWaveform;
        // A group LFO and a lane LFO, each a square at rate 0 (always +1), driving
        // the filter's `cutoff` (the member's, through the group; the session sets
        // it to 0.5) and `resonance` (left at its default).
        let session = r#"
[[group]]
[[group.modulator]]
waveform = "square"
rate = 0.0
[[group.modulator.target]]
member = 0
slot = 1
param = "cutoff"
depth = -0.2

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.effect]]
plugin = "builtin:filter"
[instrument.effect.params]
cutoff = 0.5
[[instrument.modulator]]
waveform = "square"
rate = 0.0
[[instrument.modulator.target]]
slot = 1
param = "resonance"
depth = 0.1
"#;
        let (handles, engine) = testutil::stub_engine();
        let model = testutil::load_onto(session, &handles.cmd_tx);
        let mut graph = AudioGraph::new(2, engine.cmd_rx.clone(), engine.return_tx.clone());
        graph.set_telemetry(handles.telemetry.clone());
        let ctl = Controller::new(model, handles, None);
        let mut out = vec![vec![0.0f32; 64]; 2];
        graph.process(&[], &mut out).unwrap();

        // What the filter says about itself: the parameters' defaults and ranges.
        let filter = crate::plugin::builtin::load("builtin:filter", 48_000.0, 256).unwrap();
        let param = |name: &str| filter.parameters().into_iter().find(|p| p.name == name).unwrap();
        let driven = |name: &str, base: f32, depth: f32| {
            let p = param(name);
            (base + depth * (p.max - p.min)).clamp(p.min, p.max)
        };

        let t = &ctl.telemetry;
        let group_lfo = &ctl.model.groups[0].modulators[0];
        let lane_lfo = &ctl.model.instruments[0].modulators[0];
        assert_eq!(t.load(group_lfo.output.unwrap()), LfoWaveform::Square.eval(0.0), "the group LFO's output");
        assert_eq!(t.load(lane_lfo.output.unwrap()), 1.0, "the lane LFO's output");
        let cutoff = t.load(group_lfo.targets[0].effective.unwrap());
        assert!(
            (cutoff - driven("cutoff", 0.5, -0.2)).abs() < 1e-6,
            "member 0's cutoff, through the group, centred on the 0.5 the session sets: {cutoff}"
        );
        let resonance = t.load(lane_lfo.targets[0].effective.unwrap());
        assert!(
            (resonance - driven("resonance", param("resonance").default, 0.1)).abs() < 1e-6,
            "the lane's resonance, centred on its default: {resonance}"
        );
        assert_ne!(cutoff, resonance);
        ctl.model.check_ids().unwrap();
        ctl.shutdown();
    }

    // -- lanes and groups --

    #[test]
    fn add_lane_adds_an_empty_full_range_lane_with_a_meter_and_tells_the_audio_thread() {
        let (engine, mut ctl) = controller_for(TWO_LANES);
        engine.drain();
        assert_eq!(ctl.add_lane(), 2);
        let lane = &ctl.model.instruments[2];
        assert!(lane.instrument.is_none() && lane.effects.is_empty() && lane.modulators.is_empty());
        assert_eq!((lane.range, lane.volume, lane.transpose, lane.group), (None, 1.0, 0, None));
        assert_eq!(lane.pitch_bend_range, 2.0);
        assert!(lane.pattern.is_none());
        let meter = lane.meter.expect("a meter");
        let (commands, telemetry) = engine.drain();
        assert_eq!(commands, ["AddInstrument range=None"]);
        assert_eq!(telemetry, [format!("SetLaneMeter inst=2 peak={} clips={}", meter.peak.index(), meter.clips.index())]);
        assert_ne!(lane.node, ctl.model.instruments[1].node, "a node of its own");
        ctl.model.check_ids().unwrap();
        assert_eq!(ctl.add_lane(), 3);
        assert_ne!(ctl.model.instruments[3].meter, ctl.model.instruments[2].meter, "a meter of its own");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn add_group_adds_an_empty_group_with_a_meter_and_tells_the_audio_thread() {
        let (engine, mut ctl) = controller_for(WITH_MODULATORS);
        engine.drain();
        assert_eq!(ctl.add_group(), 1);
        let group = &ctl.model.groups[1];
        assert!(group.effects.is_empty() && group.modulators.is_empty());
        assert_eq!((group.name.as_deref(), group.volume, group.pan), (None, 1.0, 0.0));
        let meter = group.meter.expect("a meter");
        let (commands, telemetry) = engine.drain();
        assert_eq!(commands, ["AddGroup"]);
        assert_eq!(telemetry, [format!("SetGroupMeter group=1 peak={} clips={}", meter.peak.index(), meter.clips.index())]);
        assert_ne!(group.node, ctl.model.groups[0].node);
        assert_ne!(Some(meter), ctl.model.groups[0].meter);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_lane_and_a_group_the_controller_adds_are_measured_by_the_real_engine() {
        let (handles, engine) = testutil::stub_engine();
        let model = testutil::load_onto(TWO_LANES, &handles.cmd_tx);
        let mut graph = AudioGraph::new(2, engine.cmd_rx.clone(), engine.return_tx.clone());
        graph.set_telemetry(handles.telemetry.clone());
        let mut ctl = Controller::new(model, handles, None);
        let lane = ctl.add_lane();
        let (plugin, slot) = ctl.load_plugin("builtin:osc").unwrap();
        ctl.set_instrument(lane, plugin, slot);
        let group = ctl.add_group();
        let _ = ctl.cmd_tx.send(GraphCommand::SetLaneGroup { inst: lane, group: Some(group) });
        ctl.model.instruments[lane].group = Some(group);
        let mut out = vec![vec![0.0f32; 64]; 2];
        // A note, a few buffers of it.
        graph.process(&[(0, [0x90, 60, 127])], &mut out).unwrap();
        for _ in 0..4 {
            graph.process(&[], &mut out).unwrap();
        }
        let peak = |meter: Option<crate::telemetry::MeterSlots>| ctl.telemetry.load(meter.unwrap().peak);
        let new_lane = peak(ctl.model.instruments[lane].meter);
        assert!(new_lane > 0.01, "the new lane's own output: {new_lane}");
        assert_eq!(peak(ctl.model.groups[group].meter), new_lane, "the group holds only that lane");
        assert!(peak(ctl.model.instruments[0].meter) > 0.01, "and the first lane, sounding too: {}", peak(ctl.model.instruments[0].meter));
        ctl.model.check_ids().unwrap();
        ctl.shutdown();
    }
}
