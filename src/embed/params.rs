//! The parameters that Tang has for a host (step 117): a bank of `SLOTS` of them that follow the macros of the session, the first macro being the first parameter, so that a DAW can automate and modulate what a person builds.
//!
//! A host wants a list of parameters that does not change while a plugin is on, and a session's macros come and go, so the list is fixed: every slot is a parameter whether a macro is in it or not, and what a slot is called and
//! which macro it is follows the session (`Bank::sync`, which asks the host to read the list again when it changes). The three threads that meet here share `HostParams` and never wait for one another:
//!
//! - the host's audio thread (`process`) writes what the host sets (`host_sets`, `host_modulates`) and reads what the page changed (`changes_for_host`),
//! - the host's main thread reads what the host asks for (`value`, `info`) and what it is to read again (`take_rescan`),
//! - the controller's thread (`Bank`) takes what the host set into the session as the change of a macro that a knob makes, with no step of the history, and tells the host what the page changed.
//!
//! What goes from one thread to another is a number with a sequence number (`Cell`), one for each slot and each way: the writer puts the newest there and the reader sees that it is a new one by the sequence, so that nothing is queued, a value is
//! never lost to a full queue and the newest is the one that is taken (a host that sends a hundred values while the controller is busy is taken once).

use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crossbeam_channel::{Receiver, Sender};

use crate::app::controller::Controller;
use crate::ids::NodeId;

/// How many parameters the bank has: the host is told of this many, whatever the session has, and a macro beyond the last is not a parameter.
pub const SLOTS: usize = 16;

/// The values of the parameters of the bank run from 0 to 1, as a macro's does.
pub const MIN: f64 = 0.0;
pub const MAX: f64 = 1.0;

/// What the host is to read again (`HostParams::take_rescan`): the values of the parameters, and what they are called (and whether a macro is in the slot). How a value is written as text does not change with the session, so there is no flag for it.
pub mod rescan {
    pub const VALUES: u8 = 1;
    pub const INFO: u8 = 2;
}

/// A number and how many times it has been put there: the sequence is the high half of the word and the number (32-bit float) the low half, so that one load gives both and one store gives both.
#[derive(Default)]
struct Cell(AtomicU64);

impl Cell {
    fn pack(sequence: u32, value: f32) -> u64 {
        (u64::from(sequence) << 32) | u64::from(value.to_bits())
    }

    /// The sequence and the number as they are now (the sequence of a cell that has not been written is 0).
    fn load(&self) -> (u32, f32) {
        let word = self.0.load(Ordering::Acquire);
        ((word >> 32) as u32, f32::from_bits(word as u32))
    }

    /// Put `value` there as the newest, with the next sequence number. It is a loop that does not wait for anyone and takes nothing from the allocator, which the audio thread may call.
    fn store(&self, value: f32) {
        let mut word = self.0.load(Ordering::Acquire);
        loop {
            let next = Self::pack(((word >> 32) as u32).wrapping_add(1), value);
            match self.0.compare_exchange_weak(word, next, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return,
                Err(seen) => word = seen,
            }
        }
    }
}

/// What the host is told of a slot (`HostParams::info`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SlotInfo {
    /// Whether a macro of the session is in the slot.
    pub present: bool,
    /// What a person called the macro (nothing if it has no name of its own, or none is in the slot).
    pub name: String,
}

/// The bank of parameters that the host's threads and the controller's thread share (see the module). One for each instance of the plugin.
pub struct HostParams {
    /// The value that the host set each parameter to last.
    from_host: [Cell; SLOTS],
    /// How much the host's modulation adds to each parameter now (in the parameter's own units: a macro goes from 0 to 1).
    modulation: [Cell; SLOTS],
    /// The value that the page, or anything else than the host, changed each macro to last.
    to_host: [Cell; SLOTS],
    /// The tempo that the host said last, in beats a minute (step 118); its sequence is 0 until it has said one.
    tempo: Cell,
    /// The value of each macro as the controller's thread has it (the bits of an `f32`): what the host is told when it asks.
    now: [AtomicU32; SLOTS],
    info: Mutex<[SlotInfo; SLOTS]>,
    /// What the host is to read again, as `rescan` says.
    rescan: AtomicU8,
    /// Wakes the controller's thread when the host has set something: one token at most is waiting, which is all that it takes to look.
    bell: Sender<()>,
    ring: Receiver<()>,
}

impl HostParams {
    pub fn new() -> Arc<HostParams> {
        let (bell, ring) = crossbeam_channel::bounded(1);
        Arc::new(HostParams {
            from_host: Default::default(),
            modulation: Default::default(),
            to_host: Default::default(),
            tempo: Cell::default(),
            now: Default::default(),
            info: Mutex::new(std::array::from_fn(|_| SlotInfo::default())),
            rescan: AtomicU8::new(0),
            bell,
            ring,
        })
    }

    /// The host sets the parameter in `slot` to `value` (held to 0 to 1; a number that is not finite and a slot that is not there are left out). It is for the audio thread, and for the main thread when the host sets values with no block.
    pub fn host_sets(&self, slot: usize, value: f64) {
        if let (Some(cell), true) = (self.from_host.get(slot), value.is_finite()) {
            cell.store(value.clamp(MIN, MAX) as f32);
            let _ = self.bell.try_send(());
        }
    }

    /// The host's modulation of the parameter in `slot` is now `amount` (held to -1 to 1, which is as far as a value of 0 to 1 can be taken; a number that is not finite is left out).
    pub fn host_modulates(&self, slot: usize, amount: f64) {
        if let (Some(cell), true) = (self.modulation.get(slot), amount.is_finite()) {
            cell.store(amount.clamp(-1.0, 1.0) as f32);
            let _ = self.bell.try_send(());
        }
    }

    /// The host says its tempo, in beats a minute (a number that is not finite and one that is not above 0 are left out): it is for the audio thread, in every block that has a tempo, and it writes and rings the bell only when the tempo
    /// is not what it was, so that a host that holds one tempo costs nothing. The session takes it (`Bank::sync`: the tempo is the host's from then on); a bank that is made later, for another session, takes the one that is here.
    pub fn host_says_tempo(&self, bpm: f64) {
        if bpm.is_finite() && bpm > 0.0 {
            let bpm = bpm as f32;
            // (A tempo above 0 is never the 0 that a cell that has been written to by no one holds, so that the first tempo is always a change.)
            if self.tempo.load().1 != bpm {
                self.tempo.store(bpm);
                let _ = self.bell.try_send(());
            }
        }
    }

    /// For what the audio thread has said to the host so far: the sequence of each slot now (an audio thread that is made now says what comes after).
    pub fn said(&self) -> [u32; SLOTS] {
        std::array::from_fn(|slot| self.to_host[slot].load().0)
    }

    /// The slots whose value the page (or anything but the host) has changed since `said` (which this brings up to date), with the value each has now: for the audio thread, which tells the host. At most one for a slot, the newest.
    pub fn changes_for_host(&self, said: &mut [u32; SLOTS], mut each: impl FnMut(usize, f32)) {
        for (slot, cell) in self.to_host.iter().enumerate() {
            let (sequence, value) = cell.load();
            if sequence != said[slot] {
                said[slot] = sequence;
                each(slot, value);
            }
        }
    }

    /// The value of the parameter in `slot` as the host is to be told when it asks (0 for a slot that is not there).
    pub fn value(&self, slot: usize) -> f64 {
        self.now.get(slot).map_or(0.0, |bits| f64::from(f32::from_bits(bits.load(Ordering::Acquire))))
    }

    /// What the host is told of the parameter in `slot`.
    pub fn info(&self, slot: usize) -> SlotInfo {
        let info = self.info.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        info.get(slot).cloned().unwrap_or_default()
    }

    /// What the host is to read again since the last time that this was asked (`rescan`'s bits), or 0 if nothing: for the host's main thread, which was called back for it.
    pub fn take_rescan(&self) -> u8 {
        self.rescan.swap(0, Ordering::AcqRel)
    }

    /// What the controller's thread waits on along with what else it waits for: a token comes when the host has set a value or a modulation.
    pub fn ring(&self) -> Receiver<()> {
        self.ring.clone()
    }
}

/// The controller's end of the bank: it takes what the host set into the session, tells the host what changed in the session, and keeps the bank's list of slots as the session has it. It lives on the controller's thread, which calls `sync` when
/// the host has set something, when a person's request has been served and on every tick.
pub(super) struct Bank {
    params: Arc<HostParams>,
    /// Wakes the host's main thread, which reads what is to be read again (a plugin: `request_callback`, which may be called from any thread).
    wake: Arc<dyn Fn() + Send + Sync>,
    /// The sequence of the last value of each slot that was taken (what the cell had when the bank was made is taken: it is the old session's).
    taken_value: [u32; SLOTS],
    /// Who is in each slot: the macro's node.
    nodes: Vec<NodeId>,
    /// The value that the host has for each slot, as far as the bank knows: what it set, or what it was told. None for a slot whose macro has just come (the host reads its value again).
    told: [Option<f32>; SLOTS],
    /// How much the host's modulation adds to each slot now, and whether the engine is playing a macro as another value than the one it was set to because of it.
    amount: [f32; SLOTS],
    modulated: [bool; SLOTS],
}

impl Bank {
    pub(super) fn new(params: Arc<HostParams>, wake: Arc<dyn Fn() + Send + Sync>) -> Bank {
        let taken_value = std::array::from_fn(|slot| params.from_host[slot].load().0);
        Bank { params, wake, taken_value, nodes: Vec::new(), told: [None; SLOTS], amount: [0.0; SLOTS], modulated: [false; SLOTS] }
    }

    /// What the controller's thread waits on for the bank: a token comes when the host has set a value or a modulation.
    pub(super) fn ring(&self) -> Receiver<()> {
        self.params.ring()
    }

    /// Bring the session and the host together: the slots as the session has its macros (and the host asked to read them again if they are not what it was told), what the host set taken into the session (a value as the change of the macro
    /// that a knob makes, a modulation as what the engine plays it as, with the value that was set kept), and what the page changed told to the host.
    pub(super) fn sync(&mut self, ctl: &mut Controller) {
        // The host's tempo is the session's from the first that it says: it is put back where a person's edit, or anything else, has moved it.
        let (sequence, tempo) = self.params.tempo.load();
        if sequence != 0 {
            let _ = ctl.set_bpm_from_host(tempo);
        }

        let macros = ctl.macros(SLOTS);
        let mut rescan = 0;

        // What the host is told of each slot, against what it was told: a name that is another one is read again, and a macro that came to a slot or went from it is a value that the host has to read as well.
        {
            let mut published = self.params.info.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            for (slot, told) in published.iter_mut().enumerate() {
                let now = macros.get(slot).map_or_else(SlotInfo::default, |each| SlotInfo { present: true, name: each.name.clone() });
                if *told != now {
                    rescan |= rescan::INFO;
                    if told.present != now.present {
                        rescan |= rescan::VALUES;
                    }
                    *told = now;
                }
            }
        }
        // Who is in each slot: a slot whose macro is another one has a value that the host does not know (it reads it again), and the engine plays it as the value that it has.
        for slot in 0..SLOTS {
            if self.nodes.get(slot) != macros.get(slot).map(|each| each.node).as_ref() {
                self.told[slot] = None;
                self.modulated[slot] = false;
                rescan |= rescan::VALUES;
            }
        }
        self.nodes = macros.iter().map(|each| each.node).collect();

        for slot in 0..SLOTS {
            // What the host set is taken whether a macro is in the slot or not: a value that it set for a slot with no macro is not a value for the macro that comes to it, which the host reads when it comes.
            let (sequence, value) = self.params.from_host[slot].load();
            let host_set = sequence != self.taken_value[slot];
            self.taken_value[slot] = sequence;
            // (What the host's modulation adds is a state and not an event: it is the amount that is compared, and no value that comes back to what it was is a change.)
            let amount = self.params.modulation[slot].load().1;
            let mut changed = amount != self.amount[slot];
            self.amount[slot] = amount;
            let Some(each) = macros.get(slot) else {
                self.params.now[slot].store(0f32.to_bits(), Ordering::Release);
                continue;
            };

            let mut base = each.value;
            if host_set {
                // (A macro that cannot be set is not in the session any more: the next look finds the slots as they are.)
                if ctl.set_macro_from_host(each.node, value).is_ok() {
                    base = value.clamp(MIN as f32, MAX as f32);
                    changed = true;
                }
                self.told[slot] = Some(base);
            } else if self.told[slot] != Some(base) {
                // The macro was changed by something that is not the host (a knob on the page, Undo, a session that was opened): the host is told, unless it is to read the value again.
                if self.told[slot].is_some() {
                    self.params.to_host[slot].store(base);
                }
                self.told[slot] = Some(base);
                changed = true;
            }
            self.params.now[slot].store(base.to_bits(), Ordering::Release);

            // The engine plays the value that was set and what the host's modulation adds to it; what the page and the file show is the value alone.
            if changed && (self.amount[slot] != 0.0 || self.modulated[slot]) {
                // (`set_macro_in_engine` holds what is played to 0 to 1.)
                let _ = ctl.set_macro_in_engine(each.node, base + self.amount[slot]);
                self.modulated[slot] = self.amount[slot] != 0.0;
            }
        }

        if rescan != 0 {
            self.params.rescan.fetch_or(rescan, Ordering::AcqRel);
            (self.wake)();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;
    use crate::app::controller::Controller;
    use crate::app::intent::{Applied, Event, Intent, ModulatorField, ModulatorKind};
    use crate::app::subscribers::EventSink;
    use crate::app::testutil::{self, StubEngine, described, load};

    const SESSION: &str = "[[instrument]]\nplugin = \"builtin:osc\"\n";

    /// A controller of `session` with a bank over its macros, a counter of how many times the host's main thread has been woken, and what the engine was sent.
    fn rig(session: &str) -> (StubEngine, Controller, Arc<HostParams>, Bank, Arc<AtomicUsize>) {
        let (handles, engine) = testutil::stub_engine();
        let ctl = Controller::new(load(session).model, handles, None);
        let params = HostParams::new();
        let woken = Arc::new(AtomicUsize::new(0));
        let counter = woken.clone();
        let bank = Bank::new(params.clone(), Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        (engine, ctl, params, bank, woken)
    }

    /// A controller with `count` macros made one after another (the first is called `names[0]` and so on, as far as there are names), settled with a first `sync` that the test does not look at.
    fn with_macros(names: &[&str], count: usize) -> (StubEngine, Controller, Arc<HostParams>, Bank, Arc<AtomicUsize>) {
        let (engine, mut ctl, params, mut bank, woken) = rig(SESSION);
        for index in 0..count {
            ctl.apply(Intent::AddFreeModulator { kind: ModulatorKind::Macro, hears: None }).unwrap();
            if let Some(name) = names.get(index) {
                let node = ctl.macros(SLOTS)[index].node;
                ctl.apply(Intent::RenameMacro { node, name: (*name).to_string() }).unwrap();
            }
        }
        bank.sync(&mut ctl);
        params.take_rescan();
        woken.store(0, Ordering::SeqCst);
        engine.cmd_rx.try_iter().for_each(drop);
        (engine, ctl, params, bank, woken)
    }

    fn sent(engine: &StubEngine) -> Vec<String> {
        described(engine.cmd_rx.try_iter())
    }

    /// A sink that keeps what it hears.
    fn listener(ctl: &mut Controller) -> Arc<Mutex<Vec<Applied>>> {
        let heard = Arc::new(Mutex::new(Vec::new()));
        let sink = heard.clone();
        let sink: Box<dyn EventSink> = Box::new(move |applied: &Applied| sink.lock().unwrap().push(applied.clone()));
        ctl.subscribe(sink);
        heard
    }

    fn changes(params: &HostParams, said: &mut [u32; SLOTS]) -> Vec<(usize, f32)> {
        let mut told = Vec::new();
        params.changes_for_host(said, |slot, value| told.push((slot, value)));
        told
    }

    #[test]
    fn a_cell_gives_the_newest_with_a_sequence_that_goes_up_each_time_and_a_cell_nobody_wrote_has_sequence_nothing() {
        let cell = Cell::default();
        assert_eq!(cell.load(), (0, 0.0));
        cell.store(0.25);
        cell.store(0.75);
        assert_eq!(cell.load(), (2, 0.75), "the newest, and two stores");
        cell.store(-0.5);
        assert_eq!(cell.load(), (3, -0.5), "a sign is kept");
        cell.0.store(Cell::pack(u32::MAX, 1.0), Ordering::SeqCst);
        cell.store(0.5);
        assert_eq!(cell.load(), (0, 0.5), "the sequence wraps and is not a panic");
    }

    #[test]
    fn what_the_host_sets_is_held_to_its_range_and_what_cannot_be_a_value_or_has_no_slot_is_left_out() {
        let params = HostParams::new();
        params.host_sets(2, 1.5);
        params.host_sets(3, -4.0);
        params.host_sets(4, f64::NAN);
        params.host_sets(5, f64::INFINITY);
        params.host_sets(SLOTS, 0.5);
        params.host_sets(usize::MAX, 0.5);
        assert_eq!(params.from_host[2].load(), (1, 1.0), "held to 1");
        assert_eq!(params.from_host[3].load(), (1, 0.0), "and to 0");
        assert_eq!(params.from_host[4].load(), (0, 0.0), "not a number: nothing was set");
        assert_eq!(params.from_host[5].load(), (0, 0.0));
        params.host_modulates(1, 7.0);
        params.host_modulates(2, -7.0);
        params.host_modulates(3, f64::NAN);
        params.host_modulates(SLOTS, 0.5);
        assert_eq!(params.modulation[1].load(), (1, 1.0));
        assert_eq!(params.modulation[2].load(), (1, -1.0), "a modulation can take a value down");
        assert_eq!(params.modulation[3].load(), (0, 0.0));
    }

    #[test]
    fn the_bell_rings_once_for_a_value_that_the_host_set_and_once_for_a_modulation_and_for_nothing_else() {
        let params = HostParams::new();
        let ring = params.ring();
        params.host_sets(0, 0.5);
        assert_eq!(ring.len(), 1, "a value rings it");
        ring.try_recv().unwrap();
        params.host_modulates(0, 0.5);
        assert_eq!(ring.len(), 1, "and so does a modulation");
        ring.try_recv().unwrap();
        for _ in 0..10 {
            params.host_sets(1, 0.2);
            params.host_modulates(1, 0.1);
        }
        assert_eq!(ring.len(), 1, "one token for all of it: the controller's thread looks once");
        ring.try_recv().unwrap();
        params.host_sets(0, f64::NAN);
        params.host_modulates(0, f64::NAN);
        params.host_sets(SLOTS, 0.5);
        params.host_modulates(SLOTS, 0.5);
        assert_eq!(ring.len(), 0, "what is not set rings nothing");
    }

    #[test]
    fn the_hosts_tempo_rings_the_bell_when_it_changes_and_not_when_it_does_not_and_a_tempo_that_is_not_one_is_left_out() {
        let params = HostParams::new();
        let ring = params.ring();
        params.host_says_tempo(120.0);
        assert_eq!((ring.len(), params.tempo.load()), (1, (1, 120.0)), "the first tempo rings it");
        ring.try_recv().unwrap();
        params.host_says_tempo(120.0);
        assert_eq!((ring.len(), params.tempo.load().0), (0, 1), "a host that holds one tempo says nothing in every block");
        params.host_says_tempo(121.5);
        assert_eq!((ring.len(), params.tempo.load()), (1, (2, 121.5)));
        ring.try_recv().unwrap();
        for not_a_tempo in [f64::NAN, f64::INFINITY, 0.0, -90.0] {
            params.host_says_tempo(not_a_tempo);
        }
        assert_eq!((ring.len(), params.tempo.load()), (0, (2, 121.5)), "what is not a tempo is not said");
    }

    #[test]
    fn the_hosts_tempo_is_the_sessions_from_the_first_that_it_says_and_a_tempo_that_something_else_sets_does_not_last() {
        let (engine, mut ctl, params, mut bank, _woken) = rig(SESSION);
        bank.sync(&mut ctl);
        assert!(!ctl.host_tempo, "a host that has said no tempo leaves the session its own");
        let (history, dirty) = (ctl.history_state(), ctl.dirty);
        params.host_says_tempo(93.0);
        engine.cmd_rx.try_iter().for_each(drop);
        bank.sync(&mut ctl);
        assert_eq!((ctl.model.bpm, ctl.host_tempo), (93.0, true));
        assert_eq!((ctl.history_state(), ctl.dirty), (history, dirty), "no step of the history, and no mark on the session");
        assert!(sent(&engine).is_empty(), "and nothing for the engine, which has it from the host's block");
        // A person types another tempo on the page: it is put back at the next look.
        ctl.apply(Intent::SetBpm { bpm: 140.0 }).unwrap();
        assert_eq!(ctl.model.bpm, 140.0);
        bank.sync(&mut ctl);
        assert_eq!(ctl.model.bpm, 93.0, "the host's tempo is the session's");
        // The host's tempo moves, as it does in a ramp: the session goes with it.
        params.host_says_tempo(94.0);
        bank.sync(&mut ctl);
        assert_eq!(ctl.model.bpm, 94.0);
    }

    #[test]
    fn a_bank_that_is_made_for_a_new_session_takes_the_tempo_that_the_host_said_before_it_was_made() {
        let (_engine, mut ctl, _unused, _bank, _woken) = rig(SESSION);
        let params = HostParams::new();
        params.host_says_tempo(101.0);
        let mut bank = Bank::new(params, Arc::new(|| {}));
        bank.sync(&mut ctl);
        assert_eq!((ctl.model.bpm, ctl.host_tempo), (101.0, true), "the host says a tempo only when it changes: a session that comes later is given the one that it has");
    }

    #[test]
    fn a_session_that_has_no_macro_has_no_slot_in_use_and_asks_the_host_to_read_nothing() {
        let (_engine, mut ctl, params, mut bank, woken) = rig(SESSION);
        bank.sync(&mut ctl);
        assert_eq!(params.take_rescan(), 0, "no slot has a macro in it, and none had one");
        assert_eq!(woken.load(Ordering::SeqCst), 0, "so the host's main thread is not woken");
        assert!((0..SLOTS).all(|slot| params.info(slot) == SlotInfo::default() && params.value(slot) == 0.0));
    }

    #[test]
    fn the_macros_are_the_slots_in_the_order_of_the_session_and_the_host_is_asked_to_read_all_of_it_once() {
        let (_engine, mut ctl, params, mut bank, woken) = rig(SESSION);
        for name in ["Brightness", ""] {
            ctl.apply(Intent::AddFreeModulator { kind: ModulatorKind::Macro, hears: None }).unwrap();
            if !name.is_empty() {
                let node = ctl.macros(SLOTS)[0].node;
                ctl.apply(Intent::RenameMacro { node, name: name.to_string() }).unwrap();
            }
        }
        bank.sync(&mut ctl);
        assert_eq!(params.info(0), SlotInfo { present: true, name: "Brightness".to_string() });
        assert_eq!(params.info(1), SlotInfo { present: true, name: String::new() }, "a macro that has no name is in its slot as it is");
        assert_eq!(params.info(2), SlotInfo::default(), "and no more of them");
        assert_eq!(params.take_rescan(), rescan::VALUES | rescan::INFO, "everything is new to the host");
        assert_eq!(woken.load(Ordering::SeqCst), 1);
        bank.sync(&mut ctl);
        assert_eq!((params.take_rescan(), woken.load(Ordering::SeqCst)), (0, 1), "a look that finds nothing new asks for nothing");
    }

    #[test]
    fn a_name_that_changes_is_read_again_by_the_host_and_its_values_are_not() {
        let (_engine, mut ctl, params, mut bank, woken) = with_macros(&["Brightness"], 2);
        let node = ctl.macros(SLOTS)[1].node;
        ctl.apply(Intent::RenameMacro { node, name: "Wet".to_string() }).unwrap();
        bank.sync(&mut ctl);
        assert_eq!(params.info(1).name, "Wet");
        assert_eq!(params.take_rescan(), rescan::INFO, "what it is called, and nothing of what it is set to");
        assert_eq!(woken.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_macro_that_is_taken_out_moves_the_ones_after_it_up_a_slot_and_the_host_reads_it_all_again() {
        let (_engine, mut ctl, params, mut bank, _woken) = with_macros(&["A", "B", "C"], 3);
        let (first, last) = (ctl.macros(SLOTS)[0].node, ctl.macros(SLOTS)[2].node);
        ctl.apply(Intent::SetModulatorField { node: last, field: ModulatorField::Value, value: 0.9 }).unwrap();
        bank.sync(&mut ctl);
        assert_eq!(params.value(2), f64::from(0.9f32), "the last macro is turned up");
        params.take_rescan();
        ctl.apply(Intent::RemoveNode { node: first }).unwrap();
        bank.sync(&mut ctl);
        let names: Vec<String> = (0..4).map(|slot| params.info(slot).name).collect();
        assert_eq!(names, ["B", "C", "", ""]);
        assert!(params.info(1).present && !params.info(2).present);
        assert_eq!((params.value(1), params.value(2)), (f64::from(0.9f32), 0.0), "the macro that was turned up is in the second slot now, and the third has no value");
        assert_eq!(params.take_rescan(), rescan::VALUES | rescan::INFO);
    }

    #[test]
    fn a_macro_that_takes_the_place_of_another_in_a_slot_is_read_by_the_host_as_a_value_and_is_not_told_as_a_change() {
        let (engine, mut ctl, params, mut bank, _woken) = with_macros(&[], 2);
        let (first, second) = (ctl.macros(SLOTS)[0].node, ctl.macros(SLOTS)[1].node);
        ctl.apply(Intent::SetModulatorField { node: first, field: ModulatorField::Value, value: 0.2 }).unwrap();
        ctl.apply(Intent::SetModulatorField { node: second, field: ModulatorField::Value, value: 0.7 }).unwrap();
        bank.sync(&mut ctl);
        params.host_modulates(0, 0.3);
        bank.sync(&mut ctl);
        // The first macro goes and another comes, and the host ends its modulation in the same moment: both slots have a macro and both have no name, but the first slot's macro is the second one now.
        ctl.apply(Intent::RemoveNode { node: first }).unwrap();
        ctl.apply(Intent::AddFreeModulator { kind: ModulatorKind::Macro, hears: None }).unwrap();
        params.host_modulates(0, 0.0);
        sent(&engine);
        params.take_rescan();
        let mut said = params.said();
        bank.sync(&mut ctl);
        assert_eq!(params.take_rescan(), rescan::VALUES, "nothing is called anything else, so it is the values that the host reads");
        assert_eq!((params.value(0), params.value(1)), (f64::from(0.7f32), 0.0), "and the first slot has the value of the macro that is there now");
        assert!(changes(&params, &mut said).is_empty(), "which is not told as a change that a knob made");
        assert!(sent(&engine).is_empty(), "and the engine, which has not played this macro as modulated, is told nothing");
    }

    #[test]
    fn what_the_host_is_to_read_again_is_kept_until_it_reads_it_whatever_is_asked_for_in_the_meantime() {
        let (_engine, mut ctl, params, mut bank, _woken) = rig(SESSION);
        ctl.apply(Intent::AddFreeModulator { kind: ModulatorKind::Macro, hears: None }).unwrap();
        bank.sync(&mut ctl);
        let node = ctl.macros(SLOTS)[0].node;
        ctl.apply(Intent::RenameMacro { node, name: "Wet".to_string() }).unwrap();
        bank.sync(&mut ctl);
        assert_eq!(params.take_rescan(), rescan::VALUES | rescan::INFO, "a macro came, and then it was called something: the host reads the values it asked for first, and the names");
    }

    #[test]
    fn a_macro_beyond_the_last_slot_is_no_parameter() {
        let (_engine, ctl, params, _bank, _woken) = with_macros(&[], SLOTS + 3);
        assert_eq!(ctl.macros(usize::MAX).len(), SLOTS + 3, "the session has them all");
        assert_eq!(ctl.macros(SLOTS).len(), SLOTS);
        assert!((0..SLOTS).all(|slot| params.info(slot).present));
        assert_eq!(params.info(SLOTS), SlotInfo::default(), "and the bank has no slot past its end");
    }

    #[test]
    fn a_value_that_the_host_sets_is_a_knob_turned_in_the_model_the_engine_and_every_client_and_a_step_of_nothing() {
        let (engine, mut ctl, params, mut bank, _woken) = with_macros(&["Brightness", "Wet"], 2);
        let heard = listener(&mut ctl);
        let (history, rev, dirty) = (ctl.history_state(), ctl.rev, ctl.dirty);
        let second = ctl.macros(SLOTS)[1].node;
        params.host_sets(1, 0.8);
        bank.sync(&mut ctl);
        assert_eq!(ctl.macros(SLOTS)[1].value, 0.8, "the model has it");
        assert_eq!(ctl.macros(SLOTS)[0].value, 0.0, "and the other macro is as it was");
        assert_eq!(sent(&engine).len(), 1, "one command for the engine: a macro of the new value");
        let heard = heard.lock().unwrap();
        assert_eq!(heard.len(), 1, "every client is told once");
        assert_eq!(heard[0].events, vec![Event::ModulatorFieldSet { node: second, field: ModulatorField::Value, value: 0.8 }]);
        assert_eq!(ctl.rev, rev + 1, "a revision is used, as for any change that clients are told of");
        assert_eq!(ctl.history_state(), history, "nothing is a step of the history: an automation lane would fill it");
        assert_eq!(ctl.dirty, dirty, "and playing a project does not mark the session as changed");
        let mut said = [0; SLOTS];
        assert!(changes(&params, &mut said).is_empty(), "the host is not told its own value back");
        bank.sync(&mut ctl);
        assert!(changes(&params, &mut said).is_empty() && sent(&engine).is_empty(), "and a look that finds nothing new does nothing");
    }

    #[test]
    fn the_newest_of_many_values_that_the_host_sent_is_the_one_that_is_taken_and_it_is_taken_once() {
        let (engine, mut ctl, params, mut bank, _woken) = with_macros(&[], 1);
        for step in 1..=50 {
            params.host_sets(0, f64::from(step) / 100.0);
        }
        bank.sync(&mut ctl);
        assert_eq!(ctl.macros(SLOTS)[0].value, 0.5);
        assert_eq!(sent(&engine).len(), 1, "one command for fifty values");
    }

    #[test]
    fn a_value_for_a_slot_that_has_no_macro_is_taken_by_nothing_and_kept_by_no_one() {
        let (engine, mut ctl, params, mut bank, _woken) = with_macros(&[], 1);
        params.host_sets(5, 0.9);
        bank.sync(&mut ctl);
        assert!(sent(&engine).is_empty());
        assert_eq!(params.value(5), 0.0);
        // (A macro that comes to the slot is what it was made as, and not what the host set it to while the slot was empty.)
        for _ in 0..5 {
            ctl.apply(Intent::AddFreeModulator { kind: ModulatorKind::Macro, hears: None }).unwrap();
        }
        bank.sync(&mut ctl);
        assert_eq!(ctl.macros(SLOTS)[5].value, 0.0);
        assert_eq!(params.value(5), 0.0);
    }

    #[test]
    fn what_the_page_changes_is_told_to_the_host_once_and_a_macro_that_the_host_set_is_not_told_back() {
        let (_engine, mut ctl, params, mut bank, _woken) = with_macros(&[], 2);
        let mut said = params.said();
        let (first, second) = (ctl.macros(SLOTS)[0].node, ctl.macros(SLOTS)[1].node);
        ctl.apply(Intent::SetModulatorField { node: second, field: ModulatorField::Value, value: 0.4 }).unwrap();
        bank.sync(&mut ctl);
        assert_eq!(changes(&params, &mut said), vec![(1, 0.4)], "the slot of the macro that was turned, and its value");
        assert_eq!(params.value(1), f64::from(0.4f32), "and the host that asks gets it");
        bank.sync(&mut ctl);
        assert!(changes(&params, &mut said).is_empty(), "once");
        params.host_sets(0, 0.9);
        bank.sync(&mut ctl);
        assert!(changes(&params, &mut said).is_empty(), "the host set the first macro: it knows");
        ctl.apply(Intent::SetModulatorField { node: first, field: ModulatorField::Value, value: 0.1 }).unwrap();
        ctl.apply(Intent::SetModulatorField { node: first, field: ModulatorField::Value, value: 0.2 }).unwrap();
        bank.sync(&mut ctl);
        assert_eq!(changes(&params, &mut said), vec![(0, 0.2)], "a knob that went through several values is told as the last of them");
    }

    #[test]
    fn undo_that_puts_a_macro_back_is_told_to_the_host_as_a_change_of_the_page_is() {
        let (_engine, mut ctl, params, mut bank, _woken) = with_macros(&[], 1);
        let node = ctl.macros(SLOTS)[0].node;
        ctl.apply(Intent::SetModulatorField { node, field: ModulatorField::Value, value: 0.6 }).unwrap();
        bank.sync(&mut ctl);
        let mut said = params.said();
        ctl.apply(Intent::Undo).unwrap();
        bank.sync(&mut ctl);
        assert_eq!(changes(&params, &mut said), vec![(0, 0.0)]);
    }

    #[test]
    fn a_macro_that_comes_to_a_slot_is_read_by_the_host_and_is_not_told_as_a_change() {
        let (_engine, mut ctl, params, mut bank, _woken) = with_macros(&[], 0);
        let mut said = params.said();
        ctl.apply(Intent::AddFreeModulator { kind: ModulatorKind::Macro, hears: None }).unwrap();
        let node = ctl.macros(SLOTS)[0].node;
        ctl.apply(Intent::SetModulatorField { node, field: ModulatorField::Value, value: 0.3 }).unwrap();
        bank.sync(&mut ctl);
        assert_eq!(params.take_rescan() & rescan::VALUES, rescan::VALUES, "the host reads the value, which is how it is not recorded as automation");
        assert!(changes(&params, &mut said).is_empty());
        assert_eq!(params.value(0), f64::from(0.3f32));
    }

    #[test]
    fn the_hosts_modulation_is_added_to_what_the_engine_plays_and_the_value_that_was_set_is_what_is_shown_and_kept() {
        let (engine, mut ctl, params, mut bank, _woken) = with_macros(&[], 1);
        let node = ctl.macros(SLOTS)[0].node;
        params.host_sets(0, 0.5);
        bank.sync(&mut ctl);
        sent(&engine);
        params.host_modulates(0, 0.25);
        bank.sync(&mut ctl);
        let commands = sent(&engine);
        assert_eq!(commands.len(), 1, "{commands:?}");
        assert!(commands[0].contains("Macro { value: 0.75 }"), "the engine plays the two together: {}", commands[0]);
        bank.sync(&mut ctl);
        assert!(sent(&engine).is_empty(), "and while the modulation goes on and nothing changes, nothing is said again");
        assert_eq!(ctl.macros(SLOTS)[0].value, 0.5, "the session has the value that was set");
        assert_eq!(params.value(0), 0.5, "and so does the host that asks");
        // The value that was set moves under the modulation: the engine plays the sum of the new one.
        params.host_sets(0, 0.6);
        bank.sync(&mut ctl);
        let commands = sent(&engine);
        assert!(commands.last().unwrap().contains("Macro { value: 0.85"), "{commands:?}");
        // The page turns it too: the same.
        ctl.apply(Intent::SetModulatorField { node, field: ModulatorField::Value, value: 0.9 }).unwrap();
        bank.sync(&mut ctl);
        assert!(sent(&engine).last().unwrap().contains("Macro { value: 1.0 }"), "held to 1 at the top");
        // The modulation ends: the engine plays the value that was set again.
        params.host_modulates(0, 0.0);
        bank.sync(&mut ctl);
        assert!(sent(&engine).last().unwrap().contains("Macro { value: 0.9"), "the value, as it was set");
        bank.sync(&mut ctl);
        assert!(sent(&engine).is_empty(), "and nothing more is said while nothing changes");
        // A value that is set after the modulation has ended is one command, the value: the engine is not played the sum of a modulation that is over.
        params.host_sets(0, 0.3);
        bank.sync(&mut ctl);
        assert_eq!(sent(&engine).len(), 1);
    }

    #[test]
    fn a_modulation_that_takes_a_value_below_nothing_plays_nothing() {
        let (engine, mut ctl, params, mut bank, _woken) = with_macros(&[], 1);
        params.host_sets(0, 0.1);
        bank.sync(&mut ctl);
        sent(&engine);
        params.host_modulates(0, -0.5);
        bank.sync(&mut ctl);
        assert!(sent(&engine).last().unwrap().contains("Macro { value: 0.0 }"));
    }

    #[test]
    fn a_bank_that_is_made_for_a_new_session_does_not_take_what_the_host_set_for_the_old_one() {
        let (_engine, mut ctl, _unused, _bank, _woken) = rig(SESSION);
        let params = HostParams::new();
        params.host_sets(0, 0.9);
        let mut bank = Bank::new(params.clone(), Arc::new(|| {}));
        ctl.apply(Intent::AddFreeModulator { kind: ModulatorKind::Macro, hears: None }).unwrap();
        bank.sync(&mut ctl);
        assert_eq!(ctl.macros(SLOTS)[0].value, 0.0, "the new session's macro is as it is");
    }

    #[test]
    fn a_new_session_that_has_no_macro_has_the_host_read_that_the_old_ones_are_gone() {
        let (_engine, mut ctl, params, mut bank, _woken) = with_macros(&["Brightness"], 1);
        assert!(params.info(0).present);
        let (_engine2, mut fresh, _p, _b, _w) = rig(SESSION);
        let mut next = Bank::new(params.clone(), Arc::new(|| {}));
        next.sync(&mut fresh);
        assert_eq!(params.info(0), SlotInfo::default(), "the names of the old session are not the new one's");
        assert_eq!(params.take_rescan(), rescan::VALUES | rescan::INFO);
        let _ = (&mut ctl, &mut bank);
    }
}
