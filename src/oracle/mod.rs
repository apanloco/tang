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
use self::edits::{Counts, Edit};
use crate::piano_filter::PianoFilter;
use crate::plugin::chain::{AudioGraph, GraphCommand, MidiLearnedNotification, PatternNotification};
use crate::plugin::{Plugin, Runtime};
use crate::session;
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

/// The same `AudioGraph` run by the loop that the plan replaced (`AudioGraph::process_legacy`), which is what `agree` holds the plan to, on every platform and with no file to be
/// out of date: the lanes and groups, the commands and the sums are the same, and only the thing that runs them is not.
pub fn legacy(wiring: Wiring) -> Box<dyn Engine> {
    struct Legacy(AudioGraph);

    impl Engine for Legacy {
        fn num_channels(&self) -> usize {
            self.0.num_channels()
        }

        fn process(&mut self, midi_events: &[MidiEvent], out: &mut [Vec<f32>]) -> anyhow::Result<()> {
            self.0.process_legacy(midi_events, out)
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

/// The scenarios that edit the cables of a session while it plays.
pub fn edited() -> Vec<Scenario> {
    vec![Scenario {
        id: format!("edited:cables@{BLOCK}"),
        session: CABLED.to_string(),
        block: BLOCK,
        rate: RATE,
        frames: SESSION_FRAMES,
        script: performance(),
        edits: CABLE_EDITS.to_vec(),
    }]
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
            self.midi_tx.send((frame.saturating_sub(start), bytes)).expect("room for the notes of a block");
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
        let differences: Vec<String> = always_on().iter().filter(|scenario| !scenario.has_cables()).filter_map(|scenario| agree(scenario, &legacy, &engine)).collect();
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
            assert_eq!(r.edits, Counts { applied: CABLE_EDITS.len(), skipped: 0, refused: 0 }, "{}", r.id);
            assert!(r.fingerprint.peak > 0.01, "{} is heard", r.id);
        }
        // The first cable of the scenario (lane 1's instrument into the reverb on the bus) is heard: without the edits the render is another, and it is the same until the block
        // that has the edit (the reverb puts out its tail only, and a tail takes a while to begin: it is not at the first frame of the cable).
        let scenario = edited().remove(0);
        let drawn = render(&scenario, &engine);
        let plain = render(&Scenario { edits: Vec::new(), ..scenario }, &engine);
        let first = drawn.samples.iter().zip(&plain.samples).position(|(x, y)| x.to_bits() != y.to_bits()).expect("the cable is heard");
        assert!(first / 2 >= 8_000 - 8_000 % BLOCK, "nothing is different before the block that has the edit: the first frame that differs is {}", first / 2);
    }

    #[test]
    fn the_scenarios_that_have_cables_are_those_that_the_loop_the_plan_replaced_is_not_held_to() {
        let with: Vec<String> = always_on().iter().filter(|scenario| scenario.has_cables()).map(|scenario| scenario.id.clone()).collect();
        assert_eq!(with, ["session:patched@256", "session:sends@256", "edited:cables@256"]);
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
        let differences: Vec<String> = whole().iter().filter(|scenario| !scenario.has_cables()).filter_map(|scenario| agree(scenario, &legacy, &engine)).collect();
        assert!(differences.is_empty(), "{} of the {} renders differ:\n{}", differences.len(), whole().len(), differences.join("\n"));
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
