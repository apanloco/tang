//! The controller: the one owner of a running session's lifecycle.
//!
//! Whichever thread runs a front-end (the TUI loop, the `play` loop, later a
//! headless server) owns a `Controller` and calls `poll` every iteration. It is
//! a plain struct, no threads inside; see `docs/web-ui.md` §3.1. It starts with
//! the housekeeping every front-end used to do for itself, or forget:
//!
//! - **Reaping.** When the audio thread swaps or removes a plugin it hands the
//!   old one back over a channel so that it is dropped here, on the owner
//!   thread. Dropping a plugin can take milliseconds and free memory, which the
//!   audio thread must not do. If nobody reads the channel it fills up (16
//!   slots), and from then on the audio thread drops them itself.
//! - **Device hot-plug.** New MIDI inputs are looked for once a second.

use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;

use crate::plugin::Plugin;

/// How often `poll` looks for newly connected devices.
const DEVICE_POLL_INTERVAL: Duration = Duration::from_secs(1);

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

pub struct Controller {
    /// Plugins the audio thread has taken out of the graph.
    return_rx: Receiver<Box<dyn Plugin>>,
    devices: Option<Box<dyn DeviceWatcher>>,
    last_device_poll: Instant,
}

impl Controller {
    pub fn new(return_rx: Receiver<Box<dyn Plugin>>) -> Self {
        Self::starting_at(return_rx, Instant::now())
    }

    fn starting_at(return_rx: Receiver<Box<dyn Plugin>>, now: Instant) -> Self {
        Controller { return_rx, devices: None, last_device_poll: now }
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

    /// Close the devices, then drop what is left in the return channel. Call it
    /// once the audio stream has stopped, so that nothing is still calling into
    /// a plugin.
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
    use crate::app::testutil::DropProbe;
    use crate::plugin::chain::{AudioGraph, GraphCommand};

    fn channel() -> (crossbeam_channel::Sender<Box<dyn Plugin>>, Controller) {
        let (tx, rx) = crossbeam_channel::bounded(16);
        (tx, Controller::new(rx))
    }

    #[test]
    fn poll_drops_returned_plugins_on_the_calling_thread() {
        let (tx, mut ctl) = channel();
        let (probe, dropped_on) = DropProbe::recording();
        // The "audio thread" hands a plugin back and goes away.
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
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(64);
        let (return_tx, return_rx) = crossbeam_channel::bounded(16);
        let mut graph = AudioGraph::new(2, cmd_rx, return_tx.clone());
        let mut ctl = Controller::new(return_rx);
        let swap = |plugin: Box<dyn Plugin>| GraphCommand::SwapInstrument {
            inst: 0,
            instrument: plugin,
            inst_buf: vec![Vec::new(), Vec::new()],
            remapper: None,
        };
        cmd_tx.send(GraphCommand::AddInstrument { range: None }).unwrap();
        let osc = |n: usize| (0..n).map(|_| crate::plugin::load("builtin:osc", 48_000.0, 256, &Default::default()).unwrap());
        for plugin in osc(3) {
            cmd_tx.send(swap(plugin)).unwrap();
        }
        graph.drain_commands();
        assert_eq!(return_tx.len(), 2, "the second and third swap each hand one back");

        ctl.poll();
        assert_eq!(return_tx.len(), 0);
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
        let (_tx, rx) = crossbeam_channel::bounded(16);
        let mut ctl = Controller::starting_at(rx, t0);
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
        let (_tx, mut ctl) = channel();
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
        let (tx, mut ctl) = channel();
        ctl.watch_devices(Box::new(Watcher(log.clone())));
        let probe_log = log.clone();
        let probe = DropProbe::new(move |_| probe_log.lock().unwrap_or_else(|e| e.into_inner()).push("plugin dropped"));
        tx.send(Box::new(probe)).unwrap();

        ctl.shutdown();
        let order = log.lock().unwrap().clone();
        assert_eq!(order, ["devices closed", "plugin dropped"]);
    }
}
