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
//!
//! and it does the housekeeping every front-end used to do for itself, or
//! forget:
//!
//! - **Reaping.** When the audio thread swaps or removes a plugin it hands the
//!   old one back over a channel so that it is dropped here, on the owner
//!   thread. Dropping a plugin can take milliseconds and free memory, which the
//!   audio thread must not do. If nobody reads the channel it fills up (16
//!   slots), and from then on the audio thread drops them itself.
//! - **Device hot-plug.** New MIDI inputs are looked for once a second.
//!
//! The fields are public while the front-ends still edit the model themselves;
//! they close as those edits become intents (`docs/web-ui.md` step 6).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};

use crate::app::model::SessionModel;
use crate::audio::MidiEvent;
use crate::held_notes::HeldNotes;
use crate::midi_monitor::MidiMonitor;
use crate::piano_filter::PianoFilter;
use crate::plugin::chain::{GraphCommand, MidiLearnedNotification, PatternNotification};
use crate::plugin::{Plugin, Runtime};

/// How often `poll` looks for newly connected devices.
const DEVICE_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// The tempo of a session none of whose patterns has been recorded.
const DEFAULT_BPM: f32 = 120.0;

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
        } = engine;
        Controller {
            global_bpm: initial_bpm(&model),
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
            return_rx,
            devices: None,
            last_device_poll: now,
        }
    }

    /// Have `poll` look for newly connected devices through `watcher`.
    pub fn watch_devices(&mut self, watcher: Box<dyn DeviceWatcher>) {
        self.devices = Some(watcher);
    }

    /// Housekeeping, to be called on every iteration of the front-end's loop.
    pub fn poll(&mut self) {
        self.poll_at(Instant::now());
    }

    fn poll_at(&mut self, now: Instant) {
        self.reap();
        if now.duration_since(self.last_device_poll) >= DEVICE_POLL_INTERVAL {
            self.last_device_poll = now;
            if let Some(devices) = &mut self.devices {
                devices.poll_new_devices();
            }
        }
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
    use crate::app::testutil::{self, DropProbe, StubEngine, load};
    use crate::plugin::chain::{AudioGraph, GraphCommand};

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
}
