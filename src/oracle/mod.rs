//! The oracle (P0 of `docs/patch-graph.md` §11.6): what the engine does, written down, so that a new engine can be held to it.
//!
//! A *scenario* is a session, a script of notes and controllers, and a block size. It is rendered the way the program renders: the session is loaded by the loader, a controller
//! is made around it, and each block goes through the audio callback's own `Renderer` (the notes that came in, the denormals flushed, the engine behind the `Engine` trait, the
//! channels interleaved) and then lets the controller see what the engine reported. The render is boiled down to a *fingerprint*: a hash of every sample and of what the engine
//! published to its telemetry each block, and a few numbers (the peak, the rms, the rms of each eighth) that say how much changed when a hash does.
//!
//! The corpus is `sessions/` (the example sessions as they were when the oracle was made, a copy so that a person who edits an example does not break it, and a few that
//! the generator does not draw), and the sessions that `generate` makes from a seed (300 of them), the last at three block sizes (64, 256 and 1024 frames).
//!
//! What is held, and by what:
//!
//! - **The engine is deterministic**: a scenario rendered twice is the same bits (`a_scenario_renders_the_same_bits_twice`). A hash needs it, and it found the oscillator's hash
//!   map of voices, which summed a chord in an order that changed from run to run.
//! - **It is what it was**: the fingerprints of the corpus are in files here: `fingerprints.txt` for every platform, held to `TOLERANCE` (the transcendental functions of
//!   another platform's libm differ in the last bit, and `a_last_bit_of_difference_in_the_rate_stays_within_the_tolerance` measures what that comes to), and `exact/<os>-<arch>.txt`
//!   for the platform it was made on, held to the bit. `TANG_UPDATE_ORACLE=1 cargo test oracle` writes the always-on part of them again, and
//!   `TANG_UPDATE_ORACLE=1 cargo test --release oracle -- --ignored` the whole of them; a change that is meant to alter what the engine renders says so in its commit, and does that.
//! - **Two engines agree**: `agree` renders a scenario with two engines and compares the bits, which is how the compiled graph of P1 is held to the graph, on every platform,
//!   with no file to be out of date.

mod bench;
pub mod edits;
pub mod generate;
mod wav;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};

use crate::alloc_guard::allocations_in;
use crate::app::controller::Controller;
use crate::app::load::load_session;
use crate::app::testutil;
use crate::audio::{Engine, MidiEvent, Renderer};
use crate::source::Events;
use self::edits::{Counts, Edit};
use crate::piano_filter::PianoFilter;
use crate::plugin::chain::{AudioGraph, GraphCommand, MidiLearnedNotification, PatternNotification};
use crate::plugin::{Plugin, Runtime};
use crate::session::{self, Feed, Route};
use crate::telemetry::{self, Telemetry};

/// What an engine is built from: the ends of the channels that the controller talks to it by, and the cells it publishes in. `main` wires an `AudioGraph` with the same.
pub struct Wiring {
    pub channels: usize,
    pub cmd_rx: Receiver<GraphCommand>,
    pub return_tx: Sender<Box<dyn Plugin>>,
    pub pattern_tx: Sender<PatternNotification>,
    pub learn_tx: Sender<MidiLearnedNotification>,
    pub telemetry: Arc<Telemetry>,
    pub piano_filter: Arc<PianoFilter>,
}

/// An `AudioGraph` wired as `main` wires one.
fn graph(wiring: Wiring) -> AudioGraph {
    let mut graph = AudioGraph::new(wiring.channels, wiring.cmd_rx, wiring.return_tx);
    graph.set_pattern_tx(wiring.pattern_tx);
    graph.set_learn_tx(wiring.learn_tx);
    graph.set_piano_filter(wiring.piano_filter);
    graph.set_telemetry(wiring.telemetry);
    graph
}

/// The engine as the program has it: an `AudioGraph`, whose lanes and groups are run by the plan that the kernel makes of them (P1 of `docs/patch-graph.md` §11.6).
pub fn engine(wiring: Wiring) -> Box<dyn Engine> {
    Box::new(graph(wiring))
}

/// The engine without the latency compensation (P3f): the paths that are added are not put in step by what the plugins say of their latency. The loop that the plan replaced has none,
/// so `agree` holds the plan to it with this, and the compensation is held apart: where nothing has latency the two engines are one (`has_latency`).
pub fn engine_uncompensated(wiring: Wiring) -> Box<dyn Engine> {
    Box::new(graph(wiring).without_latency_compensation())
}

/// The same `AudioGraph` run by the loop that the plan replaced (`AudioGraph::process_legacy`), which is what `agree` holds the plan to, on every platform and with no file to be
/// out of date: the lanes and groups, the commands and the sums are the same, and only the thing that runs them is not.
pub fn legacy(wiring: Wiring) -> Box<dyn Engine> {
    struct Legacy(AudioGraph);

    impl Engine for Legacy {
        fn num_channels(&self) -> usize {
            self.0.num_channels()
        }

        fn process(&mut self, midi_events: Events<'_>, out: &mut [Vec<f32>]) -> anyhow::Result<()> {
            self.0.process_legacy_events(midi_events, out)
        }

        fn facts(&self) -> Option<crate::plugin::chain::Facts> {
            Engine::facts(&self.0)
        }
    }

    Box::new(Legacy(graph(wiring)))
}

/// The sample rate of the corpus.
const RATE: f32 = 48_000.0;
/// The block that the always-on corpus is rendered at (macOS's default).
pub const BLOCK: usize = 256;
/// The block sizes of the whole corpus: a small one, the usual and the one that Linux starts with.
pub const BLOCKS: [usize; 3] = [64, BLOCK, 1024];
/// How many generated sessions there are in the whole corpus, and how many of them the tests that run every time render.
pub const SEEDS: u64 = 300;
pub const SEEDS_ALWAYS_ON: u64 = 24;
/// How long a generated session sounds and a session of `sessions/` does (frames).
const GENERATED_FRAMES: u64 = 36_000;
const SESSION_FRAMES: u64 = 48_000;

/// A session, what is played into it and how it is rendered.
#[derive(Clone)]
pub struct Scenario {
    /// What the scenario is called in the fixtures: `session:arp@256`, `generated:17@64`.
    pub id: String,
    pub session: String,
    /// Frames in a block.
    pub block: usize,
    pub rate: f32,
    /// How long it sounds, in frames (whole blocks are rendered: the last block may go past it).
    pub frames: u64,
    /// The notes and controllers, `(frame, bytes)` in the order they are played. A frame of the render, so that the same script means the same notes at every block size.
    pub script: Vec<(u64, [u8; 3])>,
    /// What a front-end asks of the controller while it plays, `(frame, edit)` in the order they are made: each is made before the first block that has its frame in it.
    pub edits: Vec<(u64, Edit)>,
}

impl Scenario {
    fn name(&self) -> &str {
        self.id.split('@').next().unwrap_or(&self.id)
    }

    /// The scenario at another block size.
    pub fn at_block(&self, block: usize) -> Scenario {
        Scenario { id: format!("{}@{block}", self.name()), block, ..self.clone() }
    }

    /// The scenario at another sample rate (not a corpus member: it has no fixtures).
    pub fn at_rate(&self, rate: f32) -> Scenario {
        Scenario { rate, ..self.clone() }
    }

    /// Whether it has audio cables, in its file or drawn by its edits. The loop that the plan replaced (`AudioGraph::process_legacy`) is from before there were any and does not
    /// draw them, so a scenario that has some is not held to it.
    pub fn has_cables(&self) -> bool {
        self.session.contains("[[cable]]") || self.edits.iter().any(|(_, edit)| matches!(edit, Edit::AddCable { .. }))
    }

    /// Whether a lane or a group has its way out cut, in its file or by its edits: the old loop has no such thing either (every lane goes to its group's bus or the mix).
    pub fn has_cut_routes(&self) -> bool {
        self.session.contains("route = \"none\"")
            || self.edits.iter().any(|(_, edit)| matches!(edit, Edit::Route { route: Route::None, .. } | Edit::RouteGroup { route: Route::None, .. }))
    }

    /// Whether an effect has the link into it cut, in its file or by its edits (step 86): the old loop has no such thing either (every effect is fed by the stage before it).
    pub fn has_cut_feeds(&self) -> bool {
        self.session.contains("feed = \"none\"") || self.edits.iter().any(|(_, edit)| matches!(edit, Edit::Feed { feed: Feed::None, .. }))
    }

    /// Whether it has a plugin that is late (the built-in drive, in its file or put in by an edit). Where the paths that are added are not in step by the plugins' latencies, the engine is
    /// what the loop that the plan replaced is (P3f: the plan puts them in step, and the loop does not, so `agree` holds the two with the compensation off); where nothing is late the
    /// compensation changes nothing, bit for bit, and a scenario that has nothing late is held to that.
    pub fn has_latency(&self) -> bool {
        self.session.contains("builtin:drive")
            || self.edits.iter().any(|(_, edit)| matches!(edit, Edit::AddEffect { plugin, .. } | Edit::AddBusEffect { plugin, .. } if *plugin == "builtin:drive"))
    }

    /// Whether the loop that the plan replaced can run it: it has no audio cables, no way out that is cut and no link of a chain that is cut, which that loop is from before.
    pub fn is_in_the_old_loop(&self) -> bool {
        !self.has_cables() && !self.has_cut_routes() && !self.has_cut_feeds()
    }
}

/// What a session of `sessions/` is played with: a chord (C3, E3 and G3) from a tenth of a second, the pitch wheel up a quarter of its range and back, the pedal down while the
/// keys go up and a key (C4) is hit, the pedal up, all sound off, and the key let go of. So that the pedal, the wheel, the release of the envelope and all sound off are each heard.
fn performance() -> Vec<(u64, [u8; 3])> {
    let mut events = Vec::new();
    for key in [48, 52, 55] {
        events.push((4_800, [0x90, key, 100]));
    }
    events.push((12_000, [0xE0, 0x00, 0x50]));
    events.push((18_000, [0xE0, 0x00, 0x40]));
    events.push((20_000, [0xB0, 64, 127]));
    for key in [48, 52, 55] {
        events.push((24_000, [0x80, key, 0]));
    }
    events.push((30_000, [0x90, 60, 70]));
    events.push((38_000, [0xB0, 64, 0]));
    events.push((42_000, [0xB0, 120, 0]));
    events.push((44_000, [0x80, 60, 0]));
    events
}

/// The sessions of `sessions/`, by file name.
pub fn sessions() -> Vec<Scenario> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/oracle/sessions");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|entry| entry.expect("a directory entry").path())
        .filter(|path| path.extension().is_some_and(|e| e == "toml"))
        .collect();
    files.sort();
    files
        .iter()
        .map(|path| Scenario {
            id: format!("session:{}@{BLOCK}", path.file_stem().and_then(|s| s.to_str()).expect("a file name")),
            session: std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
            block: BLOCK,
            rate: RATE,
            frames: SESSION_FRAMES,
            script: performance(),
            edits: Vec::new(),
        })
        .collect()
}

/// A session to draw cables in while it plays: a group with a reverb on its bus and a filter on lane 0, which is in it, and a delay on lane 1, which is in none. The places of its
/// cable edits (`Edit::AddCable`): the outputs are lane 0's instrument and its filter, lane 1's instrument and its delay, the group, and the reverb on its bus; the inputs are the
/// filter, the delay, the group's bus, the reverb, and the mix.
const CABLED: &str = r#"
[[group]]
[[group.effect]]
plugin = "builtin:reverb"
mix = 1.0

[[instrument]]
plugin = "builtin:osc"
group = 0
volume = 0.3
[instrument.params]
waveform = 3.0
[[instrument.effect]]
plugin = "builtin:filter"

[[instrument]]
plugin = "builtin:osc"
volume = 0.3
transpose = 7
[[instrument.effect]]
plugin = "builtin:delay"
mix = 0.4
"#;

/// What is done to the cables of `CABLED` while it plays: a cable is drawn and its level turned, its lane is muted and unmuted (the send goes with the mute and comes back), a
/// second cable is drawn, the first is taken out and undone, the lane that has them is taken out, put back by Undo, taken out again by Redo and put back again. Each is made
/// (`Counts` says so: none is skipped or refused).
const CABLE_EDITS: [(u64, Edit); 11] = [
    (8_000, Edit::AddCable { from: 2, to: 3, gain: 0.5 }),
    (12_000, Edit::CableGain { cable: 0, gain: 1.0 }),
    (16_000, Edit::Mute { lane: 1, muted: true }),
    (20_000, Edit::Mute { lane: 1, muted: false }),
    (22_000, Edit::AddCable { from: 4, to: 1, gain: 0.3 }),
    (26_000, Edit::RemoveCable { cable: 0 }),
    (28_000, Edit::Undo),
    (30_000, Edit::RemoveLane { lane: 1 }),
    (34_000, Edit::Undo),
    (38_000, Edit::Redo),
    (40_000, Edit::Undo),
];

/// What is done to the way out of the lanes and the group of `CABLED` while it plays: lane 1 is sent to the reverb of the group and its way out is cut (it is heard through the reverb
/// and the cable only), the group's way out is cut too and the reverb sent to the mix, lane 1's way out is given back and Undo cuts it again, the lane is taken out with its cut way
/// out and its cables, put back by Undo, taken out again by Redo and put back, and the way out of the group is given back. Each is made (`Counts` says so: none is skipped or
/// refused).
const ROUTE_EDITS: [(u64, Edit); 11] = [
    (8_000, Edit::AddCable { from: 2, to: 3, gain: 0.5 }),
    (12_000, Edit::Route { lane: 1, route: Route::None }),
    (16_000, Edit::RouteGroup { group: 0, route: Route::None }),
    (20_000, Edit::AddCable { from: 5, to: 4, gain: 0.7 }),
    (24_000, Edit::Route { lane: 1, route: Route::Out }),
    (26_000, Edit::Undo),
    (28_000, Edit::RemoveLane { lane: 1 }),
    (32_000, Edit::Undo),
    (36_000, Edit::Redo),
    (38_000, Edit::Undo),
    (40_000, Edit::RouteGroup { group: 0, route: Route::Out }),
];

/// What is soloed in `CABLED` while it plays, with cables that carry: lane 1, which is in no group, is sent to the reverb on the group's bus, and lane 0, which is in the group, to lane 1's delay.
/// Lane 1 is soloed (it is heard, and the bus is heard for its send), lane 0 too (all are heard), lane 1 is not any more (it is heard for what lane 0 sends it, and not for itself), it is
/// muted (nothing is heard for it), the group is soloed, lane 1 is unmuted, lane 0 is taken out while it is soloed, put back by Undo, and the solos are taken off. Each is made.
const SOLO_EDITS: [(u64, Edit); 12] = [
    (6_000, Edit::AddCable { from: 2, to: 3, gain: 0.5 }),
    (8_000, Edit::AddCable { from: 0, to: 1, gain: 0.7 }),
    (12_000, Edit::Solo { lane: 1, solo: true }),
    (16_000, Edit::Solo { lane: 0, solo: true }),
    (20_000, Edit::Solo { lane: 1, solo: false }),
    (24_000, Edit::Mute { lane: 1, muted: true }),
    (26_000, Edit::SoloGroup { group: 0, solo: true }),
    (28_000, Edit::Mute { lane: 1, muted: false }),
    (30_000, Edit::RemoveLane { lane: 0 }),
    (34_000, Edit::Undo),
    (38_000, Edit::Solo { lane: 0, solo: false }),
    (40_000, Edit::SoloGroup { group: 0, solo: false }),
];

/// What is done to the latency of `CABLED` while it plays (P3f): a drive, which is late, is put in lane 0 (lane 1, which is added to it at the mix, is delayed to match), its mix is
/// turned down (its dry sound is as late as it is), a drive is put on the bus of the group too (the delay of lane 1 is longer), a cable from lane 0's drive to lane 1's delay is drawn
/// (what is added to lane 1's own sound at the delay is delayed to match) and taken out by Undo, so is the drive on the bus, the drive of lane 0 is taken out (nothing is late, and
/// nothing is delayed), put back by Undo and taken out again by Redo, and put back. Each is made (`Counts` says so: none is skipped or refused).
const LATENCY_EDITS: [(u64, Edit); 11] = [
    (6_000, Edit::AddEffect { lane: 0, plugin: "builtin:drive" }),
    (10_000, Edit::Mix { lane: 0, effect: 1, value: 0.5 }),
    (14_000, Edit::AddBusEffect { group: 0, plugin: "builtin:drive" }),
    (18_000, Edit::AddCable { from: 2, to: 2, gain: 0.5 }),
    (22_000, Edit::Undo),
    (24_000, Edit::Undo),
    (28_000, Edit::RemoveEffect { lane: 0, effect: 1 }),
    (32_000, Edit::Undo),
    (36_000, Edit::Redo),
    (40_000, Edit::Undo),
    (44_000, Edit::Mix { lane: 0, effect: 1, value: 1.0 }),
];

/// What is done to the sources of notes of `CABLED` while it plays (P4b), with `performance_with_a_late_key`: the keys are put in as a source, and a device that is never plugged in;
/// the keys are wired to lane 0 (which heard them), the device to lane 1, which then hears nothing and lets go of the chord that it holds and of the wheel; that cable is taken out
/// (lane 1 hears every source again, and the pedal that goes down at 20 000 is its); the device is wired to lane 0 too and the keys' cable of lane 0 is taken out, while the pedal
/// holds the chord (lane 0 hears the device only, and lets go of what the pedal holds); the keys and then the device are wired to lane 1 (it hears the keys, as it did, and a key is hit
/// at 30 000); the device is taken out with its cables, both lanes', and put back by Undo; lane 0 is taken out with its cable of notes and put back by Undo, and is told what it hears;
/// its cable is taken out, so that it hears every source and plays the key that is hit at 40 500; and the keys are taken out at 41 000, while that key and the one hit at 30 000 are
/// held, with the cable of lane 1 that starts at them (lane 1 is left with the device and lets go of both). Each is made (`Counts` says so: none is skipped or refused). (Lane 0 is on a
/// bus whose reverb is all wet, so that it is heard through the reverb's tail, and a change of what it plays is heard after the reverb's first echoes.)
const SOURCE_EDITS: [(u64, Edit); 15] = [
    (6_000, Edit::AddSource { device: None }),
    (8_000, Edit::AddSource { device: Some("Seaboard") }),
    (12_000, Edit::NotesCable { source: 0, lane: 0 }),
    (16_000, Edit::NotesCable { source: 1, lane: 1 }),
    (18_000, Edit::RemoveNotesCable { cable: 1 }),
    (26_000, Edit::NotesCable { source: 1, lane: 0 }),
    (27_000, Edit::RemoveNotesCable { cable: 0 }),
    (30_000, Edit::NotesCable { source: 0, lane: 1 }),
    (31_000, Edit::NotesCable { source: 1, lane: 1 }),
    (32_000, Edit::RemoveSource { source: 1 }),
    (34_000, Edit::Undo),
    (36_000, Edit::RemoveLane { lane: 0 }),
    (38_000, Edit::Undo),
    (40_000, Edit::RemoveNotesCable { cable: 0 }),
    (41_000, Edit::RemoveSource { source: 0 }),
];

/// What is done to the instruments of `CABLED` while it plays (the effects of a lane that has none run): lane 0's instrument is sent to lane 1's delay by a cable; lane 1's instrument is taken
/// out while the chord is held (its delay goes on, and is heard on what the cable sends it: the lane has no sound of its own); lane 0's instrument is taken out too (the cable sends nothing,
/// the filter and the reverb ring out); Undo puts lane 0's instrument back, the very plugin, still holding the chord, and then lane 1's; the cable is taken out by Undo and drawn again by Redo, lane 1's
/// instrument is taken out again by Redo, and lane 1, which has none, is taken out with its delay and its cable and put back whole by Undo, and its instrument by the one after it. Each is made
/// (`Counts` says so: none is skipped or refused). (It has a cable: the loop that the plan replaced, which stops the effects of a lane that has no instrument, is not held to it.)
const INSTRUMENT_EDITS: [(u64, Edit); 11] = [
    (8_000, Edit::AddCable { from: 0, to: 1, gain: 0.7 }),
    (12_000, Edit::ClearInstrument { lane: 1 }),
    (16_000, Edit::ClearInstrument { lane: 0 }),
    (20_000, Edit::Undo),
    (24_000, Edit::Undo),
    (28_000, Edit::Undo),
    (32_000, Edit::Redo),
    (34_000, Edit::Redo),
    (36_000, Edit::RemoveLane { lane: 1 }),
    (40_000, Edit::Undo),
    (42_000, Edit::Undo),
];

/// What is done to the links of the chains of `CABLED` while it plays (step 86): the feed of lane 0's filter is cut (lane 0 is silent, and so is the group's bus: the filter is given nothing), a
/// cable from lane 0's instrument to the filter is drawn (it is heard through the filter by the cable alone), the feed of the reverb on the bus is cut (the sum of the bus does not reach it)
/// and a cable from lane 1's delay brings sound in, the feed of lane 1's delay is cut too, lane 0's filter is given its feed back (the chain and the cable both go in) and Undo cuts it again,
/// the filter is taken out while its feed is cut and put back by Undo (cut still), lane 0 is taken out with it and put back by Undo, a filter is added to lane 1 and the delay, which is not fed,
/// is moved after it (its cut goes with it) and given its feed back. Each is made (`Counts` says so: none is skipped or refused). The places of the feeds are the effects of the session
/// in order: lane 0's filter, lane 1's delay and the reverb on the bus (and, once it is added, lane 1's filter before the delay and the reverb).
const FEED_EDITS: [(u64, Edit); 14] = [
    (6_000, Edit::Feed { effect: 0, feed: Feed::None }),
    (10_000, Edit::AddCable { from: 0, to: 0, gain: 0.5 }),
    (14_000, Edit::Feed { effect: 2, feed: Feed::None }),
    (18_000, Edit::AddCable { from: 3, to: 3, gain: 0.6 }),
    (22_000, Edit::Feed { effect: 1, feed: Feed::None }),
    (26_000, Edit::Feed { effect: 0, feed: Feed::Chain }),
    (28_000, Edit::Undo),
    (30_000, Edit::RemoveEffect { lane: 0, effect: 0 }),
    (34_000, Edit::Undo),
    (36_000, Edit::RemoveLane { lane: 0 }),
    (38_000, Edit::Undo),
    (40_000, Edit::AddEffect { lane: 1, plugin: "builtin:filter" }),
    (42_000, Edit::MoveEffect { lane: 1, effect: 0, to: 1 }),
    (46_000, Edit::Feed { effect: 2, feed: Feed::Chain }),
];

/// What the keys play into `edited:sources`: `performance`, and a key (E4) that is hit and let go of after the pedal is up, which a lane that has been given the keys again plays.
fn performance_with_a_late_key() -> Vec<(u64, [u8; 3])> {
    let mut events = performance();
    events.push((40_500, [0x90, 64, 80]));
    events.push((41_500, [0x80, 64, 0]));
    events.sort_by_key(|(frame, _)| *frame);
    events
}

/// The scenarios that edit the cables, the ways out, the solos, the latency, the sources of notes, the instruments and the links of the chains of a session while it plays.
pub fn edited() -> Vec<Scenario> {
    let scenario = |name: &str, edits: &[(u64, Edit)]| Scenario {
        id: format!("edited:{name}@{BLOCK}"),
        session: CABLED.to_string(),
        block: BLOCK,
        rate: RATE,
        frames: SESSION_FRAMES,
        script: performance(),
        edits: edits.to_vec(),
    };
    let sources = Scenario { script: performance_with_a_late_key(), ..scenario("sources", &SOURCE_EDITS) };
    vec![scenario("cables", &CABLE_EDITS), scenario("routes", &ROUTE_EDITS), scenario("solos", &SOLO_EDITS), scenario("latency", &LATENCY_EDITS), sources, scenario("instruments", &INSTRUMENT_EDITS), scenario("feeds", &FEED_EDITS)]
}

/// The sessions that the seeds in `seeds` stand for, with their scripts.
pub fn generated(seeds: std::ops::Range<u64>) -> Vec<Scenario> {
    seeds
        .map(|seed| Scenario {
            id: format!("generated:{seed}@{BLOCK}"),
            session: generate::session(seed),
            block: BLOCK,
            rate: RATE,
            frames: GENERATED_FRAMES,
            script: generate::script(seed, GENERATED_FRAMES),
            edits: generate::edits(seed, GENERATED_FRAMES),
        })
        .collect()
}

/// The scenarios that the tests that run every time render: every session of `sessions/`, the ones that edit cables and the first generated ones, at the usual block.
pub fn always_on() -> Vec<Scenario> {
    let mut scenarios = sessions();
    scenarios.extend(edited());
    scenarios.extend(generated(0..SEEDS_ALWAYS_ON));
    scenarios
}

/// The whole corpus: every session of `sessions/`, the ones that edit cables and every generated one, at each block size.
pub fn whole() -> Vec<Scenario> {
    let mut base = sessions();
    base.extend(edited());
    base.extend(generated(0..SEEDS));
    base.iter().flat_map(|scenario| BLOCKS.iter().map(|&block| scenario.at_block(block))).collect()
}

/// What a scenario rendered: the samples (interleaved), a hash of what the engine had published to its telemetry at the end of each block, and how many times the engine
/// asked for memory in each.
pub struct Render {
    pub samples: Vec<f32>,
    pub telemetry: Vec<u64>,
    pub allocations: Vec<usize>,
    /// What became of the scenario's edits.
    pub edits: Counts,
    /// The blocks that edits were made before, whose commands the engine took at their start.
    pub edit_blocks: Vec<usize>,
}

/// FNV-1a over the bytes that are fed to it: a hash of bits, the same everywhere.
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Fnv(0xCBF2_9CE4_8422_2325)
    }

    fn feed(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 ^= u64::from(byte);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01B3);
        }
    }
}

/// A scenario set up to be rendered a block at a time, the way the program renders: the session is loaded onto the engine's channel by the loader, a controller is made around it,
/// and each block takes the notes that the script has in it, goes through the callback's `Renderer`, and (`poll`) lets the controller see what the engine reported (a pattern that
/// was recorded, a control that was learned).
pub struct Rig {
    /// What a front-end has: it is applied to between blocks, as a browser's requests are.
    pub ctl: Controller,
    renderer: Renderer,
    midi_tx: Sender<MidiEvent>,
    telemetry: Arc<Telemetry>,
    script: Vec<(u64, [u8; 3])>,
    /// The first note of the script that has not been sent yet.
    next: usize,
    edits: Vec<(u64, Edit)>,
    /// The first edit that has not been made yet.
    next_edit: usize,
    /// What became of the edits that have been.
    pub made: Counts,
    block: usize,
    /// How many blocks have been rendered.
    done: usize,
    data: Vec<f32>,
}

/// A block as it went: the samples (interleaved), how long the engine took (the callback's `Renderer`, nothing else) and how many times it asked for memory.
pub struct Block<'a> {
    pub samples: &'a [f32],
    pub took: Duration,
    pub allocations: usize,
}

impl Rig {
    pub fn new(scenario: &Scenario, make: &dyn Fn(Wiring) -> Box<dyn Engine>) -> Rig {
        let (mut handles, stub) = testutil::stub_engine_of(8192);
        handles.sample_rate = scenario.rate;
        handles.max_block_size = scenario.block;
        // The channels that the engine reports on are the program's own, bounded, and not the stub's unbounded ones (a send on an unbounded channel allocates when it needs room).
        let (pattern_tx, pattern_rx) = crossbeam_channel::bounded(64);
        let (learn_tx, learn_rx) = crossbeam_channel::bounded(64);
        handles.pattern_rx = pattern_rx;
        handles.learn_rx = learn_rx;
        let config = session::parse(&scenario.session).unwrap_or_else(|e| panic!("{}: the session does not parse: {e:#}", scenario.id));
        let model = load_session(&config, Path::new("."), scenario.rate, scenario.block, &Runtime::default(), &handles.cmd_tx, &mut |_| {})
            .unwrap_or_else(|e| panic!("{}: the session does not load: {e:#}", scenario.id));
        let telemetry = handles.telemetry.clone();
        let midi_tx = handles.midi_tx.clone();
        let wiring = Wiring {
            channels: 2,
            cmd_rx: stub.cmd_rx.clone(),
            return_tx: stub.return_tx.clone(),
            pattern_tx,
            learn_tx,
            telemetry: telemetry.clone(),
            piano_filter: handles.piano_filter.clone(),
        };
        let mut ctl = Controller::new(model, handles, None);
        // (What a scan finds, and what an effect or an instrument that is asked for is looked up in.)
        ctl.catalog.add(crate::plugin::builtin::enumerate_plugins());
        let renderer = Renderer::new(make(wiring), stub.midi_rx.clone(), scenario.block);
        Rig {
            ctl,
            renderer,
            midi_tx,
            telemetry,
            script: scenario.script.clone(),
            next: 0,
            edits: scenario.edits.clone(),
            next_edit: 0,
            made: Counts::default(),
            block: scenario.block,
            done: 0,
            data: vec![0.0; scenario.block * 2],
        }
    }

    /// Make the edits that have their frame in the next block, play what the script has in it, at its place in it, and render the block.
    pub fn block(&mut self) -> Block<'_> {
        let (start, end) = ((self.done * self.block) as u64, ((self.done + 1) * self.block) as u64);
        while self.next_edit < self.edits.len() && self.edits[self.next_edit].0 < end {
            let outcome = self.edits[self.next_edit].1.make(&mut self.ctl);
            self.made.add(outcome);
            self.next_edit += 1;
        }
        while self.next < self.script.len() && self.script[self.next].0 < end {
            let (frame, bytes) = self.script[self.next];
            self.midi_tx.send(MidiEvent::keys(frame.saturating_sub(start), bytes)).expect("room for the notes of a block");
            self.next += 1;
        }
        let cb = self.done as u64;
        let before = Instant::now();
        let ((), allocations) = allocations_in(|| self.renderer.render(&mut self.data, cb));
        let took = before.elapsed();
        self.done += 1;
        Block { samples: &self.data, took, allocations }
    }

    /// Let the controller see what the engine reported.
    pub fn poll(&mut self) {
        self.ctl.poll();
    }

    /// The engine has the lanes and the groups that the model has, and calls each, and each effect of each, by the id that the model gave it: what the edits that were made have
    /// done to the one they have done to the other (an engine that says nothing of its lanes is not held to it).
    fn assert_the_engine_has_the_nodes_of_the_model(&self, what: &str) {
        if let Some(facts) = self.renderer.engine().facts() {
            assert_eq!(facts.lanes, testutil::model_lanes(&self.ctl.model), "{what}: the lanes of the engine are not those of the model");
            assert_eq!(facts.groups, testutil::model_groups(&self.ctl.model), "{what}: the groups of the engine are not those of the model");
            assert_eq!(facts.cables, testutil::model_audio_cables(&self.ctl.model), "{what}: the audio cables of the engine are not those of the model");
        }
    }

    /// A hash of every cell of the telemetry, as the engine left them.
    pub fn telemetry_hash(&self) -> u64 {
        let mut hash = Fnv::new();
        for index in 0..telemetry::CAPACITY {
            hash.feed(&self.telemetry.load_index(index).to_bits().to_le_bytes());
        }
        hash.0
    }
}

/// Render `scenario` with the engine that `make` builds, all of it.
pub fn render(scenario: &Scenario, make: &dyn Fn(Wiring) -> Box<dyn Engine>) -> Render {
    let mut rig = Rig::new(scenario, make);
    let blocks = scenario.frames.div_ceil(scenario.block as u64) as usize;
    let mut samples = Vec::with_capacity(blocks * scenario.block * 2);
    let mut allocations = Vec::with_capacity(blocks);
    let mut slots = Vec::with_capacity(blocks);
    let mut edit_blocks = Vec::new();
    for b in 0..blocks {
        let before = rig.made.total();
        let block = rig.block();
        samples.extend_from_slice(block.samples);
        allocations.push(block.allocations);
        if rig.made.total() > before {
            edit_blocks.push(b);
        }
        rig.assert_the_engine_has_the_nodes_of_the_model(&format!("{}, block {b}", scenario.id));
        slots.push(rig.telemetry_hash());
        rig.poll();
    }
    Render { samples, telemetry: slots, allocations, edits: rig.made, edit_blocks }
}

/// How many pieces the rms of a render is kept in.
pub const WINDOWS: usize = 8;

/// What a render comes to: the hashes say whether it is the same, the numbers how far it is when it is not.
#[derive(Clone, Debug, PartialEq)]
pub struct Fingerprint {
    pub hash: u64,
    pub telemetry: u64,
    pub peak: f32,
    pub rms: f32,
    pub windows: [f32; WINDOWS],
}

pub fn fingerprint(render: &Render) -> Fingerprint {
    let mut hash = Fnv::new();
    for sample in &render.samples {
        hash.feed(&sample.to_bits().to_le_bytes());
    }
    let mut telemetry = Fnv::new();
    for block in &render.telemetry {
        telemetry.feed(&block.to_le_bytes());
    }
    let peak = render.samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    let rms_of = |part: &[f32]| (part.iter().map(|&s| f64::from(s) * f64::from(s)).sum::<f64>() / part.len().max(1) as f64).sqrt() as f32;
    let mut windows = [0.0f32; WINDOWS];
    let size = render.samples.len().div_ceil(WINDOWS).max(1);
    for (window, part) in windows.iter_mut().zip(render.samples.chunks(size)) {
        *window = rms_of(part);
    }
    Fingerprint { hash: hash.0, telemetry: telemetry.0, peak, rms: rms_of(&render.samples), windows }
}

/// The two renders of one scenario by two engines: `None` if they are the same bits, else where they first differ.
pub fn agree(scenario: &Scenario, a: &dyn Fn(Wiring) -> Box<dyn Engine>, b: &dyn Fn(Wiring) -> Box<dyn Engine>) -> Option<String> {
    let (one, other) = (render(scenario, a), render(scenario, b));
    if one.samples.len() != other.samples.len() {
        return Some(format!("{}: {} samples against {}", scenario.id, one.samples.len(), other.samples.len()));
    }
    if let Some(at) = one.samples.iter().zip(&other.samples).position(|(x, y)| x.to_bits() != y.to_bits()) {
        let frame = at / 2;
        return Some(format!(
            "{}: the samples differ at frame {frame} (block {}, channel {}): {} against {}",
            scenario.id,
            frame / scenario.block,
            at % 2,
            one.samples[at],
            other.samples[at]
        ));
    }
    if let Some(block) = one.telemetry.iter().zip(&other.telemetry).position(|(x, y)| x != y) {
        return Some(format!("{}: the telemetry differs from block {block}", scenario.id));
    }
    None
}

/// A scenario, and what it rendered boiled down.
pub struct Rendered {
    pub id: String,
    pub block: usize,
    pub fingerprint: Fingerprint,
    /// How many times the engine asked for memory after its first block (which takes the commands that the session was loaded with).
    pub late_allocations: usize,
    pub edits: Counts,
}

pub fn render_all(scenarios: &[Scenario], make: &dyn Fn(Wiring) -> Box<dyn Engine>) -> Vec<Rendered> {
    scenarios
        .iter()
        .map(|scenario| {
            let rendered = render(scenario, make);
            assert!(rendered.samples.iter().all(|s| s.is_finite()), "{}: a sample is not a number", scenario.id);
            let late_allocations = rendered.allocations.iter().enumerate().filter(|&(b, _)| b > 0 && !rendered.edit_blocks.contains(&b)).map(|(_, &n)| n).sum();
            Rendered { id: scenario.id.clone(), block: scenario.block, fingerprint: fingerprint(&rendered), late_allocations, edits: rendered.edits }
        })
        .collect()
}

// -- The fixtures --

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/oracle")
}

/// The platform that the exact hashes are for: another platform's libm may differ in the last bit of a note's frequency or of a filter's coefficient.
fn exact_key() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

fn read_table(path: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(path)
        .map(|text| {
            text.lines()
                .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
                .filter_map(|line| line.split_once(' ').map(|(id, rest)| (id.to_string(), rest.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

fn write_table(path: &Path, header: &str, table: &BTreeMap<String, String>) {
    let mut text = String::from(header);
    for (id, rest) in table {
        text.push_str(&format!("{id} {rest}\n"));
    }
    std::fs::create_dir_all(path.parent().expect("a directory")).expect("the fixture directory");
    std::fs::write(path, text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
}

fn exact_line(f: &Fingerprint) -> String {
    format!("{:016x} {:016x}", f.hash, f.telemetry)
}

fn tolerant_line(f: &Fingerprint) -> String {
    let mut line = format!("{:.6e} {:.6e}", f.peak, f.rms);
    for window in f.windows {
        line.push_str(&format!(" {window:.6e}"));
    }
    line
}

/// How far a number of a fingerprint may be from the one stored, relative to the larger, on a platform that has no exact hashes (and an absolute floor under it, for a render that
/// is nearly silent). It is wider than the last bit of a platform's libm makes it, by a measure (`a_last_bit_of_difference_in_the_rate_stays_within_the_tolerance`), and narrow
/// enough for a changed law to be seen: a filter's coefficient, a modulator's range or an effect's feedback changes these by percents.
pub const TOLERANCE: f32 = 1e-2;
const FLOOR: f32 = 2e-5;

/// The numbers of a fingerprint, in the order that `tolerant_line` writes them, with their names.
fn numbers(f: &Fingerprint) -> Vec<(String, f32)> {
    let mut all = vec![("peak".to_string(), f.peak), ("rms".to_string(), f.rms)];
    all.extend(f.windows.iter().enumerate().map(|(i, &w)| (format!("rms of the {}th eighth", i + 1), w)));
    all
}

/// How far a fingerprint is from the numbers that a line stores: the largest relative distance, and which number it is of.
fn distance(stored: &str, f: &Fingerprint) -> Result<(f32, String), String> {
    let then: Vec<f32> = stored.split(' ').map(|n| n.parse().unwrap_or(f32::NAN)).collect();
    let now = numbers(f);
    if then.len() != now.len() {
        return Err(format!("{} numbers stored, {} now", then.len(), now.len()));
    }
    let mut worst = (0.0f32, String::new());
    for (then, (name, now)) in then.iter().zip(&now) {
        let relative = (then - now).abs() / (then.abs().max(now.abs()) + FLOOR / TOLERANCE);
        if relative > worst.0 || relative.is_nan() {
            worst = (relative, format!("{name} was {then:e} and is {now:e}"));
        }
    }
    Ok(worst)
}

fn tolerates(stored: &str, f: &Fingerprint) -> Result<(), String> {
    let (relative, what) = distance(stored, f)?;
    if relative > TOLERANCE || relative.is_nan() { Err(what) } else { Ok(()) }
}

const TOLERANT_HEADER: &str = "# What the engine renders, boiled down: peak, rms, and the rms of each eighth of the render, for the corpus at the block of 256 frames. Held to a\n# relative TOLERANCE of each number, on every platform. Written by `TANG_UPDATE_ORACLE=1 cargo test oracle` (src/oracle/mod.rs).\n";
const EXACT_HEADER: &str = "# The hash of every sample and of the telemetry of every block, for one platform (the file's name). Held to the bit. Written by\n# `TANG_UPDATE_ORACLE=1 cargo test oracle` (src/oracle/mod.rs).\n";

fn updating() -> bool {
    std::env::var("TANG_UPDATE_ORACLE").is_ok_and(|v| v == "1")
}

/// Hold the renders to the fixtures, or write the fixtures when `TANG_UPDATE_ORACLE=1` (with `whole`, the files are written from these renders alone; else what is in the
/// files for other scenarios is kept).
pub fn check_against_the_fixtures(renders: &[Rendered], whole: bool) {
    let tolerant_path = fixture_dir().join("fingerprints.txt");
    let exact_path = fixture_dir().join("exact").join(format!("{}.txt", exact_key()));
    let (mut tolerant, mut exact) = (read_table(&tolerant_path), read_table(&exact_path));
    if updating() {
        if whole {
            tolerant.clear();
            exact.clear();
        }
        for r in renders {
            exact.insert(r.id.clone(), exact_line(&r.fingerprint));
            if r.block == BLOCK {
                tolerant.insert(r.id.clone(), tolerant_line(&r.fingerprint));
            }
        }
        write_table(&tolerant_path, TOLERANT_HEADER, &tolerant);
        write_table(&exact_path, EXACT_HEADER, &exact);
        return;
    }
    let mut wrong = Vec::new();
    let mut held_exactly = 0;
    for r in renders {
        let f = &r.fingerprint;
        if let Some(stored) = exact.get(&r.id) {
            held_exactly += 1;
            if *stored != exact_line(f) {
                let by_numbers = tolerant.get(&r.id).map_or_else(
                    || "no numbers stored".to_string(),
                    |t| match tolerates(t, f) {
                        Ok(()) => "the numbers are within the tolerance".to_string(),
                        Err(why) => why,
                    },
                );
                wrong.push(format!("{}: the hashes are not what they were ({by_numbers})", r.id));
            }
        } else if r.block == BLOCK {
            match tolerant.get(&r.id) {
                Some(stored) => {
                    if let Err(why) = tolerates(stored, f) {
                        wrong.push(format!("{}: {why}", r.id));
                    }
                }
                None => wrong.push(format!("{}: nothing is stored for it (TANG_UPDATE_ORACLE=1 writes it)", r.id)),
            }
        }
    }
    eprintln!("oracle: {} renders, {held_exactly} held to the bit on {}", renders.len(), exact_key());
    assert!(wrong.is_empty(), "what the engine renders is not what it rendered:\n{}", wrong.join("\n"));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The always-on corpus, rendered once for the tests that look at it.
    fn always_on_renders() -> &'static [Rendered] {
        static RENDERS: OnceLock<Vec<Rendered>> = OnceLock::new();
        RENDERS.get_or_init(|| render_all(&always_on(), &engine))
    }

    #[test]
    fn a_scenario_renders_the_same_bits_twice() {
        for scenario in always_on() {
            if let Some(difference) = agree(&scenario, &engine, &engine) {
                panic!("the engine is not deterministic: {difference}");
            }
        }
    }

    #[test]
    fn the_renders_are_what_they_were() {
        check_against_the_fixtures(always_on_renders(), false);
    }

    #[test]
    fn the_plan_gives_what_the_loop_it_replaced_gave_bit_for_bit() {
        let differences: Vec<String> = always_on().iter().filter(|scenario| scenario.is_in_the_old_loop()).filter_map(|scenario| agree(scenario, &legacy, &engine_uncompensated)).collect();
        assert!(differences.is_empty(), "the engine on the plan is not the engine of the loop:\n{}", differences.join("\n"));
    }

    #[test]
    fn no_block_after_the_first_asks_the_allocator_for_memory() {
        let late: Vec<String> = always_on_renders().iter().filter(|r| r.late_allocations > 0).map(|r| format!("{}: {}", r.id, r.late_allocations)).collect();
        assert!(late.is_empty(), "the audio thread asked for memory in these (the first block takes the commands of the load, and is let be):\n{}", late.join("\n"));
    }

    #[test]
    fn the_edits_of_the_generated_sessions_mostly_apply() {
        let mut counts = Counts::default();
        for r in always_on_renders().iter().filter(|r| r.id.starts_with("generated:")) {
            counts.applied += r.edits.applied;
            counts.skipped += r.edits.skipped;
            counts.refused += r.edits.refused;
        }
        eprintln!("oracle: the edits of the first {SEEDS_ALWAYS_ON} generated sessions: {counts:?}");
        assert!(counts.total() >= 4 * SEEDS_ALWAYS_ON as usize, "{counts:?}");
        assert!(counts.applied * 10 >= counts.total() * 7, "most of the edits are made: {counts:?}");
        assert!(counts.refused * 20 <= counts.total(), "the controller refuses few of them: {counts:?}");
    }

    #[test]
    fn the_edits_of_the_cable_scenarios_are_all_made_and_a_cable_is_heard_when_it_is_drawn() {
        for r in always_on_renders().iter().filter(|r| r.id.starts_with("edited:")) {
            let made = edited().iter().find(|scenario| scenario.id == r.id).map(|scenario| scenario.edits.len()).expect("a scenario of the corpus");
            assert_eq!(r.edits, Counts { applied: made, skipped: 0, refused: 0 }, "{}", r.id);
            assert!(r.fingerprint.peak > 0.01, "{} is heard", r.id);
        }
        // The first cable of the scenario (lane 1's instrument into the reverb on the bus) is heard: without the edits the render is another, and it is the same until the block
        // that has the edit (the reverb puts out its tail only, and a tail takes a while to begin: it is not at the first frame of the cable).
        let scenario = edited().remove(0);
        assert!(scenario.id.starts_with("edited:cables"));
        let drawn = render(&scenario, &engine);
        let plain = render(&Scenario { edits: Vec::new(), ..scenario }, &engine);
        let first = drawn.samples.iter().zip(&plain.samples).position(|(x, y)| x.to_bits() != y.to_bits()).expect("the cable is heard");
        assert!(first / 2 >= 8_000 - 8_000 % BLOCK, "nothing is different before the block that has the edit: the first frame that differs is {}", first / 2);
    }

    #[test]
    fn the_scenarios_that_have_cables_are_those_that_the_loop_the_plan_replaced_is_not_held_to() {
        let with: Vec<String> = always_on().iter().filter(|scenario| scenario.has_cables()).map(|scenario| scenario.id.clone()).collect();
        assert_eq!(
            with,
            [
                "session:patched@256",
                "session:routed@256",
                "session:sends@256",
                "session:unfed@256",
                "edited:cables@256",
                "edited:routes@256",
                "edited:solos@256",
                "edited:latency@256",
                "edited:instruments@256",
                "edited:feeds@256"
            ]
        );
    }

    #[test]
    fn the_scenarios_that_cut_the_link_into_an_effect_are_those_that_the_loop_the_plan_replaced_is_not_held_to_either() {
        let cut: Vec<String> = always_on().iter().filter(|scenario| scenario.has_cut_feeds()).map(|scenario| scenario.id.clone()).collect();
        assert_eq!(cut, ["session:unfed@256", "edited:feeds@256"]);
        // (A cut that is given back is not a cut: only the edits that cut count, and a file that says `chain` has none.)
        let given_back = Scenario { session: "[[instrument]]\nplugin = \"builtin:osc\"\n".to_string(), edits: vec![(1_000, Edit::Feed { effect: 0, feed: Feed::Chain })], ..sessions().remove(0) };
        assert!(!given_back.has_cut_feeds() && given_back.is_in_the_old_loop());
        assert!(Scenario { session: "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\nfeed = \"none\"\n".to_string(), ..given_back.clone() }.has_cut_feeds());
        assert!(Scenario { edits: vec![(1_000, Edit::Feed { effect: 0, feed: Feed::None })], ..given_back }.has_cut_feeds());
    }

    #[test]
    fn a_link_of_a_chain_that_is_cut_is_heard_in_the_feeds_scenario_and_the_unfed_session_is_heard_through_its_cables() {
        // Lane 0's filter is not fed (the first edit): its sound goes nowhere but by the cable that is drawn after, so the render is another from the block that has the edit on, and the
        // same before it.
        let scenario = edited().into_iter().find(|scenario| scenario.id.starts_with("edited:feeds")).expect("a scenario of the corpus");
        let cut = render(&scenario, &engine);
        let without = Scenario { edits: scenario.edits.iter().filter(|(_, edit)| !matches!(edit, Edit::Feed { .. })).cloned().collect(), ..scenario.clone() };
        let plain = render(&without, &engine);
        let first = cut.samples.iter().zip(&plain.samples).position(|(x, y)| x.to_bits() != y.to_bits()).expect("the cut is heard");
        assert!(first / 2 >= 6_000 - 6_000 % BLOCK, "nothing is different before the block that has the first edit: the first frame that differs is {}", first / 2);
        assert!(fingerprint(&cut).peak > 0.01 && fingerprint(&plain).peak > 0.01, "both are heard");
        // The session that cuts the links (`unfed.toml`) is heard, and its lane 0 is heard through the cables and in no other way: with the cable that carries its delay to the reverb taken
        // out, what is left is lane 1's sound alone, and the render is another.
        let session = sessions().into_iter().find(|scenario| scenario.id.starts_with("session:unfed")).expect("a session of the corpus");
        let with = render(&session, &engine);
        let without = Scenario { session: session.session.replace("[[cable]]\nfrom = \"instrument[0].effect[1]\"\nto = \"group[0].effect[0]\"\ngain = 0.8\n", ""), ..session };
        let alone = render(&without, &engine);
        assert_ne!(with.samples.iter().map(|s| s.to_bits()).collect::<Vec<_>>(), alone.samples.iter().map(|s| s.to_bits()).collect::<Vec<_>>(), "lane 0 is heard through its reverb");
        assert!(fingerprint(&with).peak > 0.01 && fingerprint(&alone).peak > 0.01, "both are heard");
    }

    #[test]
    fn the_scenarios_that_cut_a_way_out_are_those_that_the_loop_the_plan_replaced_is_not_held_to_either() {
        let cut: Vec<String> = always_on().iter().filter(|scenario| scenario.has_cut_routes()).map(|scenario| scenario.id.clone()).collect();
        assert_eq!(cut, ["session:routed@256", "edited:routes@256"]);
        // (A scenario that has no cable and no cut is what the loop can run: there are many of them, and every generated session is one.)
        assert!(always_on().iter().filter(|scenario| scenario.is_in_the_old_loop()).count() > 20);
        assert!(always_on().iter().filter(|scenario| scenario.id.starts_with("generated:")).all(|scenario| scenario.is_in_the_old_loop()));
    }

    #[test]
    fn where_nothing_has_latency_the_engine_is_what_it_is_without_the_compensation_bit_for_bit() {
        let scenarios: Vec<Scenario> = always_on().into_iter().filter(|scenario| !scenario.has_latency()).collect();
        assert!(scenarios.len() > 15, "{} scenarios have nothing that is late", scenarios.len());
        let differences: Vec<String> = scenarios.iter().filter_map(|scenario| agree(scenario, &engine_uncompensated, &engine)).collect();
        assert!(differences.is_empty(), "the compensation changes what nothing makes late:\n{}", differences.join("\n"));
    }

    #[test]
    fn the_scenarios_that_have_something_that_is_late_are_the_sessions_with_a_drive_and_the_scenarios_that_put_one_in() {
        let late: Vec<String> = always_on().iter().filter(|scenario| scenario.has_latency() && !scenario.id.starts_with("generated:")).map(|scenario| scenario.id.clone()).collect();
        assert_eq!(late, ["session:cross@256", "session:fold@256", "session:patched@256", "edited:latency@256"]);
    }

    #[test]
    fn the_compensation_is_heard_where_what_is_late_is_added_to_what_is_not() {
        // The scenario that edits the latency (a drive put in lane 0, which is added to lane 1 at the mix) renders another sound than the engine without the compensation does.
        let scenario = edited().into_iter().find(|scenario| scenario.id.starts_with("edited:latency")).expect("a scenario of the corpus");
        assert!(agree(&scenario, &engine_uncompensated, &engine).is_some(), "the compensation is heard");
    }

    #[test]
    fn a_way_out_that_is_cut_is_heard_not_in_the_mix_but_through_its_cables_in_the_routed_session() {
        // The session that cuts the ways out (`routed.toml`) is heard, and the lanes that have no way out are in it only through their cables: without the cable of lane 1 to the mix
        // (half of itself), what is rendered is another, and it is as loud as the lanes that have their way out and the cables of the others make it.
        let scenario = sessions().into_iter().find(|scenario| scenario.id.starts_with("session:routed")).expect("a session of the corpus");
        let with = render(&scenario, &engine);
        assert!(fingerprint(&with).peak > 0.01, "it is heard");
        let without = Scenario { session: scenario.session.replace("[[cable]]\nfrom = \"instrument[1]\"\nto = \"master\"\ngain = 0.5\n", ""), ..scenario };
        let alone = render(&without, &engine);
        assert_ne!(with.samples.iter().map(|s| s.to_bits()).collect::<Vec<_>>(), alone.samples.iter().map(|s| s.to_bits()).collect::<Vec<_>>(), "lane 1 is heard through its cable and in no other way");
    }

    #[test]
    fn a_lane_that_is_soloed_is_heard_through_the_cables_that_it_sends_in_the_solo_scenario_and_what_is_sent_to_a_lane_is_heard_when_another_is_soloed() {
        // Lane 1 is soloed from frame 12 000 to 16 000 and sends to the reverb on the group's bus: the bus is heard for it, so that the tail is in the mix, which without the cable is the
        // lane alone. (The scenario is the same up to the block that has the first solo: the cables are drawn before it.)
        let scenario = edited().into_iter().find(|scenario| scenario.id.starts_with("edited:solos")).expect("a scenario of the corpus");
        let with = render(&scenario, &engine);
        let without = render(&Scenario { edits: scenario.edits.iter().filter(|(_, edit)| !matches!(edit, Edit::AddCable { .. })).cloned().collect(), ..scenario.clone() }, &engine);
        let window = |render: &Render, from: usize, to: usize| -> f32 {
            let samples = &render.samples[2 * from..2 * to];
            (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
        };
        // The solo has been on for a while (12 000 to 16 000: the bus's tail swells in over a few thousand frames).
        let (heard, alone) = (window(&with, 14_000, 16_000), window(&without, 14_000, 16_000));
        assert!(heard > 1.05 * alone, "the reverb that lane 1 is sent to is heard for it: {heard} and {alone}");
        // Lane 1 is not soloed from 20 000 and lane 0 is: lane 1 is heard for what lane 0 sends it, which without the cable is nothing.
        let (heard, alone) = (window(&with, 21_000, 23_500), window(&without, 21_000, 23_500));
        assert!(heard > 1.05 * alone, "lane 1 is heard for what lane 0 sends it: {heard} and {alone}");
        // (Measured: an rms of 0.0700 against 0.0575 in the first window, 1.22 times as much, and 0.0399 against 0.0238 in the second, 1.68 times; the test asks for 1.05.)
    }

    #[test]
    fn what_a_cable_sends_to_the_effects_of_a_lane_that_has_no_instrument_is_heard_in_the_instruments_scenario() {
        // Lane 0's chord goes by the cable (drawn at 8 000) into lane 1's delay, and lane 1's instrument is taken out at 12 000: from 12 288 on lane 1 has no sound of its own, and its delay
        // runs all the same and puts out what the cable sends it (the part of it that the mix leaves dry, at once). Without the cable the lane is silent there.
        let scenario = edited().into_iter().find(|scenario| scenario.id.starts_with("edited:instruments")).expect("a scenario of the corpus");
        assert!(matches!(scenario.edits[0].1, Edit::AddCable { .. }) && matches!(scenario.edits[1].1, Edit::ClearInstrument { lane: 1 }));
        let until_16_000 = |edits: &[(u64, Edit)]| render(&Scenario { edits: edits.to_vec(), frames: 16_128, ..scenario.clone() }, &engine);
        let with = until_16_000(&scenario.edits[..2]);
        let without = until_16_000(&scenario.edits[1..2]);
        // (Lane 0 is in both: what is different is what lane 1 puts out.)
        let difference = |from: usize, to: usize| -> f32 {
            let (a, b) = (&with.samples[2 * from..2 * to], &without.samples[2 * from..2 * to]);
            (a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum::<f32>() / a.len() as f32).sqrt()
        };
        let after = difference(12_288, 16_000);
        assert!(after > 0.01, "lane 1, which has no instrument, is heard through its delay for what the cable sends it: {after}");
        // (Measured: an rms of 0.0328, and exactly 0 where the effects of a lane that has no instrument do not run; the test asks for 0.01.)
    }

    #[test]
    fn an_edit_is_made_before_the_block_that_has_its_frame_in_it() {
        // A held note and a mute at frame 5000: the first block that is not as it would have been is the one that frame is in, at every block size.
        for block in BLOCKS {
            let plain = Scenario {
                id: format!("edit@{block}"),
                session: "[[instrument]]\nplugin = \"builtin:osc\"\n".to_string(),
                block,
                rate: RATE,
                frames: 12_000,
                script: vec![(0, [0x90, 60, 100])],
                edits: Vec::new(),
            };
            let muted = Scenario { edits: vec![(5_000, Edit::Mute { lane: 0, muted: true })], ..plain.clone() };
            let (a, b) = (render(&plain, &engine), render(&muted, &engine));
            let first = a.samples.iter().zip(&b.samples).position(|(x, y)| x.to_bits() != y.to_bits()).expect("the mute is heard");
            assert_eq!(first / 2 / block, 5_000 / block, "block {block}: the first frame that differs is {}", first / 2);
            assert_eq!(first / 2 % block, 0, "the mute is made at the start of its block, and is let go of over the block");
            assert_eq!(b.edits, Counts { applied: 1, skipped: 0, refused: 0 });
        }
    }

    #[test]
    fn every_generated_session_loads() {
        for seed in 0..SEEDS {
            let text = generate::session(seed);
            if let Err(e) = testutil::try_load(&text) {
                panic!("the session of seed {seed} does not load: {e:#}\n{text}");
            }
        }
    }

    /// What a generated session may have, as the text a person would write it, and in how many of the `SEEDS` it is at least (about two fifths of what the generator makes of it).
    const COVERED: [(&str, usize); 50] = [
        ("waveform = \"sine\"", 25),
        ("waveform = \"triangle\"", 25),
        ("waveform = \"saw\"", 30),
        ("waveform = \"square\"", 25),
        ("waveform = \"random\"", 25),
        ("waveform = \"smooth\"", 25),
        ("\nsync = ", 60),
        ("retrigger = true", 50),
        ("retrigger = false", 20),
        ("fade = ", 45),
        ("type = \"envelope\"", 65),
        ("type = \"macro\"", 65),
        ("type = \"steps\"", 65),
        ("glide = ", 30),
        ("type = \"midi\"\n[[", 10),
        ("midi_cc", 25),
        ("midi_pitch_bend", 15),
        ("midi_aftertouch", 10),
        ("midi_velocity", 12),
        ("midi_key", 10),
        ("\nmod_rate = ", 55),
        ("\nmod_attack = ", 8),
        ("\nmod_decay = ", 8),
        ("\nmod_sustain = ", 6),
        ("\nmod_release = ", 8),
        ("\nmember = ", 40),
        ("\nbus = ", 40),
        ("[[group]]", 75),
        ("\ngroup = ", 60),
        ("[[group.modulator]]", 55),
        ("[[group.effect]]", 65),
        ("mute = true", 25),
        ("solo = true", 12),
        ("remap = ", 25),
        ("[instrument.pattern]", 35),
        ("in_key", 15),
        ("looping = false", 10),
        ("\nrange = ", 45),
        ("\ntranspose = ", 75),
        ("[[instrument.effect]]", 100),
        ("[[instrument.modulator]]", 100),
        ("plugin = \"builtin:filter\"", 85),
        ("plugin = \"builtin:delay\"", 85),
        ("plugin = \"builtin:reverb\"", 85),
        ("plugin = \"builtin:drive\"", 80),
        ("slot = 1", 80),
        ("slot = 2", 60),
        ("slot = 3", 25),
        ("depth = -", 100),
        ("\nlength = ", 35),
    ];

    #[test]
    fn the_generated_sessions_have_what_the_engine_has() {
        let texts: Vec<String> = (0..SEEDS).map(generate::session).collect();
        let short: Vec<String> = COVERED
            .iter()
            .filter_map(|&(feature, at_least)| {
                let count = texts.iter().filter(|text| text.contains(feature)).count();
                (count < at_least).then(|| format!("{feature:?} is in {count} sessions, and should be in {at_least}"))
            })
            .collect();
        assert!(short.is_empty(), "the generator does not draw enough of:\n{}", short.join("\n"));
    }

    #[test]
    fn the_sessions_are_heard_and_most_generated_ones_are() {
        let renders = always_on_renders();
        let heard = |r: &Rendered| r.fingerprint.peak > 0.01;
        let silent: Vec<&str> = renders.iter().filter(|r| r.id.starts_with("session:") && !heard(r)).map(|r| r.id.as_str()).collect();
        assert!(silent.is_empty(), "these sessions of sessions/ are silent: {silent:?}");
        let generated: Vec<&Rendered> = renders.iter().filter(|r| r.id.starts_with("generated:")).collect();
        let sounding = generated.iter().filter(|r| heard(r)).count();
        assert!(sounding * 10 >= generated.len() * 7, "{sounding} of {} generated renders are heard", generated.len());
    }

    /// The first float above 48 kHz: the sample rate that a platform whose libm differs in the last bit would make of it.
    fn a_bit_above(rate: f32) -> f32 {
        f32::from_bits(rate.to_bits() + 1)
    }

    #[test]
    fn a_last_bit_of_difference_in_the_rate_stays_within_the_tolerance() {
        // The rate is what every phase step, every time constant and every coefficient is made from, so a last bit of it moves them all by a last bit, as another libm's
        // `powf` and `exp` do. (Measured: the largest of these distances is printed, and the tolerance has room over it.)
        let mut worst = (0.0f32, String::new());
        for scenario in sessions().iter().chain(generated(0..12).iter()) {
            let stored = fingerprint(&render(scenario, &engine));
            let moved = fingerprint(&render(&scenario.at_rate(a_bit_above(RATE)), &engine));
            let line = tolerant_line(&stored);
            let (relative, what) = distance(&line, &moved).expect("the same numbers");
            if relative > worst.0 {
                worst = (relative, format!("{}: {what}", scenario.id));
            }
        }
        eprintln!("oracle: a last bit of the rate moves a number by at most {:.2e} (tolerance {TOLERANCE:e}): {}", worst.0, worst.1);
        assert!(worst.0 * 3.0 < TOLERANCE, "a last bit of the rate moves a number by {:.2e}: {}", worst.0, worst.1);
    }

    #[test]
    #[ignore = "the whole corpus at three block sizes: cargo test --release oracle -- --ignored"]
    fn the_whole_corpus_is_the_same_under_the_plan_and_under_the_loop_it_replaced() {
        let differences: Vec<String> = whole().iter().filter(|scenario| !scenario.has_cables()).filter_map(|scenario| agree(scenario, &legacy, &engine_uncompensated)).collect();
        assert!(differences.is_empty(), "{} of the {} renders differ:\n{}", differences.len(), whole().len(), differences.join("\n"));
    }

    #[test]
    #[ignore = "the whole corpus at three block sizes: cargo test --release oracle -- --ignored"]
    fn the_whole_corpus_is_what_it_is_without_the_compensation_where_nothing_has_latency() {
        let scenarios: Vec<Scenario> = whole().into_iter().filter(|scenario| !scenario.has_latency()).collect();
        let differences: Vec<String> = scenarios.iter().filter_map(|scenario| agree(scenario, &engine_uncompensated, &engine)).collect();
        assert!(differences.is_empty(), "{} of the {} renders that have nothing late differ:\n{}", differences.len(), scenarios.len(), differences.join("\n"));
    }

    #[test]
    fn the_sources_scenario_is_heard_as_the_session_is_until_a_lane_is_wired_to_a_device_that_is_not_there_and_is_quieter_after_it_has_let_go_of_its_chord() {
        let scenario = edited().into_iter().find(|scenario| scenario.id.starts_with("edited:sources")).expect("a scenario of the corpus");
        let with = render(&scenario, &engine);
        let plain = render(&Scenario { edits: Vec::new(), ..scenario }, &engine);
        // The sources, and the cable of the keys to the lane that heard them, change nothing that is heard. The cable of lane 1 to the device (frame 16 000, made before the block that has it)
        // does, from the first frame of that block: the lane lets go of the chord that it holds.
        let first = with.samples.iter().zip(&plain.samples).position(|(x, y)| x.to_bits() != y.to_bits()).expect("the cable is heard");
        assert_eq!(first / 2, 16_000 - 16_000 % BLOCK, "the first frame that differs");
        let window = |render: &Render, from: usize, to: usize| -> f32 {
            let samples = &render.samples[2 * from..2 * to];
            (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
        };
        // (The lane that hears nothing is the one with the transposed saw and the delay: what is left is lane 0.)
        let (heard, alone) = (window(&plain, 16_500, 21_500), window(&with, 16_500, 21_500));
        assert!(alone < 0.75 * heard, "lane 1 hears nothing from 16 000 to 22 000: {alone} against {heard}");
    }

    #[test]
    fn a_lane_that_is_left_with_a_device_that_is_not_there_lets_go_of_what_it_holds_when_a_cable_of_notes_is_taken_out_and_when_a_source_is() {
        let scenario = edited().into_iter().find(|scenario| scenario.id.starts_with("edited:sources")).expect("a scenario of the corpus");
        let with = render(&scenario, &engine);
        let without = |frame: u64| render(&Scenario { edits: scenario.edits.iter().filter(|(at, _)| *at != frame).cloned().collect(), ..scenario.clone() }, &engine);
        let first_difference = |other: &Render| with.samples.iter().zip(&other.samples).position(|(x, y)| x.to_bits() != y.to_bits()).map(|at| at / 2);
        // At 27 000 the keys' cable of lane 0 is taken out, and it is left with the device, while the pedal holds its chord. Lane 0 is on a bus whose reverb is all wet, so that it is heard
        // through the reverb's tail, which has no sound for the first few thousand frames.
        let let_go = first_difference(&without(27_000)).expect("what lane 0 held is let go of");
        assert!((27_000..32_000).contains(&let_go), "the first frame that differs is {let_go}");
        // At 41 000 the keys are taken out, with the cable of lane 1 from them, and it is left with the device while two keys are held: it lets go of both, in the block that has the edit.
        assert_eq!(first_difference(&without(41_000)), Some(41_000 - 41_000 % BLOCK), "the first frame that differs");
    }

    // -- who hears whom (P4b: cables of notes in a session, through the real engine) --

    /// Two oscillators, one hard to the left and one hard to the right, so that the channel that sounds says which lane does; the keys go to the first, and a device (that is plugged in when
    /// a test says it is) to the second. With `wired` false there is no cable of notes at all.
    fn two_lanes(wired: bool) -> Scenario {
        let cables = if wired { "[[cable]]\nfrom = \"keys\"\nto = \"instrument[0]\"\n[[cable]]\nfrom = \"device[0]\"\nto = \"instrument[1]\"\n" } else { "" };
        Scenario {
            id: "wired".to_string(),
            session: format!("[[device]]\nname = \"Launchkey\"\n[[instrument]]\nplugin = \"builtin:osc\"\n[instrument.params]\npan = -1.0\n[[instrument]]\nplugin = \"builtin:osc\"\n[instrument.params]\npan = 1.0\n{cables}"),
            block: 256,
            rate: 48_000.0,
            frames: 0,
            script: Vec::new(),
            edits: Vec::new(),
        }
    }

    /// Ports that are open, by name, with the source each has.
    struct Plugged(Vec<(&'static str, crate::source::Source)>);

    impl crate::app::controller::DeviceWatcher for Plugged {
        fn poll_new_devices(&mut self) -> crate::midi::DeviceChanges {
            crate::midi::DeviceChanges::default()
        }

        fn sources_of(&self, name: &str) -> crate::source::SourceSet {
            self.0.iter().filter(|(open, _)| open.contains(name)).fold(crate::source::SourceSet::NONE, |set, (_, source)| set.with(*source))
        }

        fn open_names(&self) -> Vec<String> {
            self.0.iter().map(|(name, _)| (*name).to_string()).collect()
        }
    }

    /// How little sound a lane that is not played makes, in the sum of the squares of a few blocks: the pan of an oscillator is smoothed, and what is left of it after four blocks is a few
    /// thousandths (a lane that is played makes tens).
    const SILENT: f64 = 0.01;

    /// A rig that has run a few blocks of silence: the pan of an oscillator is smoothed over about 5 ms from the middle, so that a note that begins at once has some sound on the other side.
    fn settled(scenario: &Scenario) -> Rig {
        let mut rig = Rig::new(scenario, &engine);
        assert_eq!(play(&mut rig, &[], 4), (0.0, 0.0), "nobody plays");
        rig
    }

    /// The sound of the left side and of the right side (the sum of the squares) for `blocks` blocks, after `events` are sent at the start of the first.
    fn play(rig: &mut Rig, events: &[MidiEvent], blocks: usize) -> (f64, f64) {
        for event in events {
            rig.midi_tx.send(*event).unwrap();
        }
        let mut sound = (0.0, 0.0);
        for _ in 0..blocks {
            for frame in rig.block().samples.chunks(2) {
                sound = (sound.0 + f64::from(frame[0]).powi(2), sound.1 + f64::from(frame[1]).powi(2));
            }
        }
        sound
    }

    #[test]
    fn a_lane_that_a_session_wires_to_the_keys_is_the_only_lane_that_the_keys_play() {
        let mut rig = settled(&two_lanes(true));
        let (left, right) = play(&mut rig, &[MidiEvent::keys(0, [0x90, 60, 100])], 8);
        assert!(left > 1.0, "the lane that the keys are wired to sounds: {left}");
        assert!(right < SILENT, "and the lane that a device is wired to, which is not plugged in, does not: {right}");
    }

    #[test]
    fn a_lane_wired_to_a_device_is_played_by_the_device_once_it_is_plugged_in_and_not_by_the_keys() {
        let launchkey = crate::source::Source::device(0);
        let plugged = || Box::new(Plugged(vec![("Launchkey Mini MK4 37 MIDI Out", launchkey)]));
        // The device plays the lane that it is wired to, and not the lane of the keys.
        let mut rig = settled(&two_lanes(true));
        rig.ctl.watch_devices(plugged());
        let (left, right) = play(&mut rig, &[MidiEvent::from_source(launchkey, 0, [0x90, 64, 100])], 8);
        assert!(left < SILENT, "the lane that the keys are wired to does not hear a device: {left}");
        assert!(right > 1.0, "the lane that the device is wired to does: {right}");
        // The keys play theirs, and not the lane of the device that is plugged in.
        let mut rig = settled(&two_lanes(true));
        rig.ctl.watch_devices(plugged());
        let (left, right) = play(&mut rig, &[MidiEvent::keys(0, [0x90, 60, 100])], 8);
        assert!(left > 1.0, "the keys play their lane: {left}");
        assert!(right < SILENT, "and not the device's: {right}");
    }

    #[test]
    fn a_session_with_no_cable_of_notes_is_played_by_the_keys_on_every_lane() {
        let mut rig = settled(&two_lanes(false));
        let (left, right) = play(&mut rig, &[MidiEvent::keys(0, [0x90, 60, 100])], 8);
        assert!(left > 1.0 && right > 1.0, "both lanes hear the keys: {left} {right}");
    }

    #[test]
    fn a_lane_that_is_wired_to_a_device_while_a_key_is_held_lets_go_of_the_key_and_hears_the_next_key_when_the_cable_is_taken_out() {
        use crate::app::graph::{Endpoint, Port};
        use crate::app::intent::Intent;
        let mut rig = settled(&two_lanes(false));
        let key = |note: u8| MidiEvent::keys(0, [0x90, note, 100]);
        let (left, right) = play(&mut rig, &[key(60)], 8);
        assert!(left > 1.0 && right > 1.0, "both lanes play the key: {left} {right}");
        // The second lane is wired to the device, which is not plugged in: it hears nothing from now on, and lets go of the key that it was playing, which is still held.
        let (device, second) = (rig.ctl.model.sources[0].node, rig.ctl.model.instruments[1].node);
        rig.ctl.apply(Intent::AddCable { from: Endpoint { node: device, port: Port::EventsOut }, to: Endpoint { node: second, port: Port::EventsIn }, gain: None }).unwrap();
        play(&mut rig, &[], 8);
        let (left, right) = play(&mut rig, &[], 4);
        assert!(left > 1.0, "the first lane plays the key still: {left}");
        assert!(right < SILENT, "and the second has let go of it: {right}");
        // The cable is taken out: the second lane hears every source again. The key that it let go of is not played again, and the next one is.
        let cable = rig.ctl.model.event_cables[0].id;
        rig.ctl.apply(Intent::RemoveEdge { edge: cable }).unwrap();
        let (_, right) = play(&mut rig, &[], 4);
        assert!(right < SILENT, "the key that it let go of is not played again: {right}");
        let (_, right) = play(&mut rig, &[key(64)], 8);
        assert!(right > 1.0, "a key that is hit now is played: {right}");
    }

    #[test]
    #[ignore = "the whole corpus at three block sizes: cargo test --release oracle -- --ignored"]
    fn the_whole_corpus_is_what_it_was_and_the_same_twice() {
        let scenarios = whole();
        let renders = render_all(&scenarios, &engine);
        check_against_the_fixtures(&renders, true);
        let late: Vec<String> = renders.iter().filter(|r| r.late_allocations > 0).map(|r| format!("{}: {}", r.id, r.late_allocations)).collect();
        assert!(late.is_empty(), "the audio thread asked for memory in these:\n{}", late.join("\n"));
        let twice: Vec<String> = scenarios.iter().filter_map(|scenario| agree(scenario, &engine, &engine)).collect();
        assert!(twice.is_empty(), "the engine is not deterministic:\n{}", twice.join("\n"));
        let generated: Vec<&Rendered> = renders.iter().filter(|r| r.id.starts_with("generated:")).collect();
        let sounding = generated.iter().filter(|r| r.fingerprint.peak > 0.01).count();
        let clipping = generated.iter().filter(|r| r.fingerprint.peak > 1.0).count();
        let mut edits = Counts::default();
        for r in generated.iter().filter(|r| r.block == BLOCK) {
            edits.applied += r.edits.applied;
            edits.skipped += r.edits.skipped;
            edits.refused += r.edits.refused;
        }
        eprintln!("oracle: {} renders; of the generated, {sounding} of {} are heard and {clipping} clip; their edits: {edits:?}", renders.len(), generated.len());
        assert!(edits.applied * 10 >= edits.total() * 7 && edits.refused * 20 <= edits.total(), "{edits:?}");
        assert!(sounding * 10 >= generated.len() * 8, "{sounding} of {} generated renders are heard", generated.len());
        assert!(clipping > 0, "none of the generated renders clips");
    }
}
