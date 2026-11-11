//! Pattern boxes (P4h of `docs/patch-graph.md` §11.6): a node that stores notes in beats, records them from the inputs, and plays them as a source of its own.
//!
//! This is the logic of one box as plain code: where it is in its loop, which notes begin and end in a block (with their frames), what a recording makes of the notes that arrive, and the click that counts a recording in.
//! The engine (`plugin::chain`) owns the boxes, gives each the block's events and merges what it makes into them with its source; the controller keeps the model of the same box.
//!
//! **Time.** A box has a clock in beats. On its **own clock** it runs from Play (from 0) and goes by the graph's tempo; a recording begins at -`COUNT_IN_BEATS` and its pass is the beats from 0 to the box's length. On the
//! **host's clock** its clock is the host's beat while the host plays (and it is let go of while the host does not, and when the host jumps). In both, a place in the loop is the clock modulo the box's length, so a box
//! goes by the tempo whatever the tempo was when its notes were recorded: the notes are in beats.
//!
//! **No allocation once it runs.** Everything a block needs is made when the box is (`BoxPlayer::new`, `BoxPlan::new`): the audio thread does not ask for memory in `process`, not even when a pass ends and the notes it took
//! become the box's.

use crate::ids::NodeId;
use crate::source::{Source, SourceSet};

/// How many beats of the metronome a recording is counted in with (the lane recorder's: `plugin::chain::COUNT_IN_BEATS`).
pub const COUNT_IN_BEATS: f64 = 4.0;
/// The shortest and the longest loop a box has, in beats.
pub const MIN_LENGTH_BEATS: f32 = 0.25;
pub const MAX_LENGTH_BEATS: f32 = 256.0;
/// How many notes a box holds: a recording that has more drops the rest.
pub const MAX_NOTES: usize = 4096;
/// The shortest a recorded note is, in beats: a key that came up in the instant it went down in.
const MIN_NOTE_BEATS: f64 = 1.0 / 64.0;
/// How many events a box makes for one block at the most, and how many it hears: room is made once, so that the audio thread asks for none.
const MAX_EVENTS_PER_BLOCK: usize = 4096;
const MAX_HEARD_PER_BLOCK: usize = 512;
/// How far a host's beat may be from where the last block ended before it is a jump (a loop, a seek), in beats.
const JUMP_BEATS: f64 = 0.1;
/// How many times a block is cut at the most (a count-in is over, the loop begins, a pass ends: a few).
const MAX_SEGMENTS: usize = 16;
/// A beat that is this near a boundary is on it.
const EPS: f64 = 1e-9;

/// The metronome's click: as the lane recorder's (`plugin::chain`), a sine that dies away.
const CLICK_DURATION_SECS: f32 = 0.025;
const CLICK_FREQ: f32 = 1000.0;
const CLICK_DOWNBEAT_FREQ: f32 = 1500.0;
const CLICK_VOLUME: f32 = 0.3;

/// A note of a box: where it begins and how long it is, in beats, its pitch and its velocity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoxNote {
    pub start: f32,
    pub length: f32,
    pub note: u8,
    pub velocity: u8,
}

/// Whose beat a box goes by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Clock {
    /// Its own: from Play to Stop.
    #[default]
    Own,
    /// The host's: its beat modulo the box's length while the host plays.
    Host,
}

/// What a box is set to, apart from its notes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    /// Whether it plays again from the start at the end of its length (the own clock; the host's clock goes round with the host's beat).
    pub looping: bool,
    pub clock: Clock,
    /// Whether a recording keeps the notes that are there and goes on pass after pass until it is finished, and not takes them out and makes one pass.
    pub overdub: bool,
    /// Whether the metronome clicks while a recording is counted in and made (the own clock: a host has its own metronome).
    pub click: bool,
    /// The sources it records.
    pub inputs: SourceSet,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings { looping: true, clock: Clock::Own, overdub: false, click: true, inputs: SourceSet::ALL }
    }
}

/// What a box is doing, for whoever shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Stopped,
    Playing,
    /// Record was pressed on the own clock: the metronome counts.
    CountingIn,
    /// Record was pressed on the host's clock: it waits for the loop's start.
    Waiting,
    /// A pass of recording.
    Recording,
}

/// What the engine knows of the block that a box is run for.
#[derive(Clone, Copy, Debug)]
pub struct Block {
    pub frames: usize,
    pub sample_rate: f32,
    /// The tempo the graph goes by, in beats a minute.
    pub bpm: f32,
    /// Where the host is, in beats, at the start of the block, while it plays.
    pub host_beat: Option<f64>,
}

impl Block {
    /// How many beats the block is: 0 if the tempo or the rate is not a number above 0.
    fn beats(&self) -> f64 {
        let beats = self.frames as f64 * f64::from(self.bpm) / 60.0 / f64::from(self.sample_rate);
        if beats.is_finite() && beats > 0.0 { beats } else { 0.0 }
    }
}

/// A block as the box lays it on its clock: where it begins, how many beats it is and how many frames.
#[derive(Clone, Copy)]
struct Window {
    c0: f64,
    beats: f64,
    frames: usize,
}

impl Window {
    /// The clock at a frame of the block.
    fn clock_at(&self, frame: u64) -> f64 {
        self.c0 + frame.min(self.frames as u64 - 1) as f64 / self.frames as f64 * self.beats
    }

    /// The frame of the block that a clock is at: the first frame for a clock before the block, the last for one after it.
    fn frame_of(&self, clock: f64) -> u64 {
        (((clock - self.c0) / self.beats * self.frames as f64).floor().max(0.0) as u64).min(self.frames as u64 - 1)
    }
}

/// A note on or a note off that a box makes for a block, at a frame of it, as the bytes that a lane is given (channel 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoxEvent {
    pub frame: u64,
    pub bytes: [u8; 3],
}

/// A tick of the metronome: the frame of the block that it begins at, and whether it is the first beat of a bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tick {
    pub frame: usize,
    pub downbeat: bool,
}

/// What a box tells the controller.
#[derive(Debug, PartialEq)]
pub enum BoxNotification {
    /// A pass of recording is over: these are the notes that the box has now, which the model keeps and the file saves.
    Recorded { node: NodeId, notes: Vec<BoxNote> },
    /// What the box is doing has changed: a count-in is over, a recording has ended, a box that plays once has played it.
    Status { node: NodeId, status: Status },
}

/// One thing to do at a beat of the loop.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Step {
    beat: f64,
    on: bool,
    note: u8,
    velocity: u8,
}

/// A box's notes and length, made ready to be played: its steps, in the order of the loop. It is made on the thread that is not the audio thread and moved in; it has the room for all the notes that a box holds, so that
/// the notes that a recording takes become the box's without asking for memory.
#[derive(Debug, PartialEq)]
pub struct BoxPlan {
    length: f32,
    notes: Vec<BoxNote>,
    steps: Vec<Step>,
}

impl BoxPlan {
    /// The plan of these notes in a loop this long (clamped to `MIN_LENGTH_BEATS` and `MAX_LENGTH_BEATS`; a length that is no number is the shortest). What is not a note is left out: a pitch above 127, a number
    /// that is not finite; a velocity is brought into 1 to 127, a start below 0 to 0, a length below 0 to 0. A note that begins past the loop's end, or is no longer than 0, is kept and is not played (a longer loop plays
    /// it); one that goes past the end ends there; and a note is ended by the next of its pitch (two that overlap are an off and then the next on, and not two ons and an off between them).
    pub fn new(length_beats: f32, notes: &[BoxNote]) -> BoxPlan {
        let mut plan = BoxPlan { length: Self::clamp_length(length_beats), notes: Vec::with_capacity(MAX_NOTES), steps: Vec::with_capacity(2 * MAX_NOTES) };
        plan.notes.extend(notes.iter().filter_map(Self::tidy).take(MAX_NOTES));
        plan.compile();
        plan
    }

    fn clamp_length(length_beats: f32) -> f32 {
        if length_beats.is_finite() { length_beats.clamp(MIN_LENGTH_BEATS, MAX_LENGTH_BEATS) } else { MIN_LENGTH_BEATS }
    }

    /// A note as the box holds it, or none if it is not one.
    fn tidy(note: &BoxNote) -> Option<BoxNote> {
        (note.start.is_finite() && note.length.is_finite() && note.note <= 127).then(|| BoxNote {
            start: note.start.max(0.0),
            length: note.length.max(0.0),
            note: note.note,
            velocity: note.velocity.clamp(1, 127),
        })
    }

    /// Sort the notes and make the steps of the loop from them. No memory is asked for (the room was made with the plan).
    fn compile(&mut self) {
        // Every field is in the order, so that notes that differ in nothing but the order they came in are one plan whatever the sort does with them.
        self.notes.sort_unstable_by(|a, b| a.start.total_cmp(&b.start).then(a.note.cmp(&b.note)).then(a.length.total_cmp(&b.length)).then(a.velocity.cmp(&b.velocity)));
        self.steps.clear();
        let length = f64::from(self.length);
        // The start of the next note of each pitch, going backwards through the notes: the next one ends this one.
        let mut next_start = [f64::INFINITY; 128];
        for note in self.notes.iter().rev() {
            // A note with no length is not played, and ends none (one that begins past the loop's end is cut to nothing below).
            if note.length <= 0.0 {
                continue;
            }
            let start = f64::from(note.start);
            let slot = note.note as usize;
            let end = (start + f64::from(note.length)).min(length).min(next_start[slot]);
            next_start[slot] = start;
            if end <= start {
                continue;
            }
            self.steps.push(Step { beat: start, on: true, note: note.note, velocity: note.velocity });
            self.steps.push(Step { beat: end, on: false, note: note.note, velocity: 0 });
        }
        // The order of the loop: by beat, and at one beat what ends before what begins (a note that ends where the next of its pitch begins).
        self.steps.sort_unstable_by(|a, b| a.beat.total_cmp(&b.beat).then(a.on.cmp(&b.on)).then(a.note.cmp(&b.note)));
    }

    /// The loop's length in beats.
    pub fn length(&self) -> f32 {
        self.length
    }

    /// The notes, in the order of their starts.
    pub fn notes(&self) -> &[BoxNote] {
        &self.notes
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    /// The own clock is counting the beats before the pass (the clock is below 0).
    CountIn,
    /// The host's clock: waiting for the loop's start.
    WaitLoop,
    /// A pass: the clock where it began.
    Pass { start: f64 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum State {
    Stopped,
    Playing,
    Recording(Phase),
}

/// What a pass has taken so far.
struct Pass {
    /// For each pitch the beat of the pass and the velocity of a note that is held.
    held: [Option<(f64, u8)>; 128],
    notes: Vec<BoxNote>,
}

impl Pass {
    fn new() -> Pass {
        Pass { held: [None; 128], notes: Vec::with_capacity(MAX_NOTES) }
    }

    fn clear(&mut self) {
        self.held = [None; 128];
        self.notes.clear();
    }
}

/// A note that was heard: the clock it came at, whether it began, its pitch and its velocity.
type Heard = (f64, bool, u8, u8);

/// One pattern box in the engine: it plays its notes, and records the notes that it hears.
pub struct BoxPlayer {
    source: Source,
    plan: BoxPlan,
    settings: Settings,
    state: State,
    /// The own clock: where it is at the start of the next block (0 while it is stopped). The host's clock: where the last block ended, to see a jump.
    clock: f64,
    /// The pitches that are on.
    sounding: [bool; 128],
    /// What is on is to be let go of at the start of the next block (a plan was put in, or the box was stopped).
    release_next: bool,
    /// A recording that goes on is to end when its pass does.
    finishing: bool,
    pass: Pass,
    heard: Vec<Heard>,
    out: Vec<BoxEvent>,
    ticks: Vec<Tick>,
    /// A pass ended and the engine has not been asked for the notes yet.
    finished: bool,
}

impl BoxPlayer {
    /// A box with no notes, 4 beats long, stopped. It is made where memory may be asked for, and moved to the audio thread.
    pub fn new(source: Source) -> BoxPlayer {
        BoxPlayer {
            source,
            plan: BoxPlan::new(4.0, &[]),
            settings: Settings::default(),
            state: State::Stopped,
            clock: 0.0,
            sounding: [false; 128],
            release_next: false,
            finishing: false,
            pass: Pass::new(),
            heard: Vec::with_capacity(MAX_HEARD_PER_BLOCK),
            out: Vec::with_capacity(MAX_EVENTS_PER_BLOCK),
            ticks: Vec::with_capacity(64),
            finished: false,
        }
    }

    /// The source its notes are from.
    pub fn source(&self) -> Source {
        self.source
    }

    /// What it is doing.
    pub fn status(&self) -> Status {
        match self.state {
            State::Stopped => Status::Stopped,
            State::Playing => Status::Playing,
            State::Recording(Phase::CountIn) => Status::CountingIn,
            State::Recording(Phase::WaitLoop) => Status::Waiting,
            State::Recording(Phase::Pass { .. }) => Status::Recording,
        }
    }

    /// The notes it has, in the order of their starts.
    pub fn notes(&self) -> &[BoxNote] {
        self.plan.notes()
    }

    /// Put these notes and this length in (a plan that was made elsewhere). What is on is let go of at the start of the next block, and a pass that is going on goes on.
    pub fn set_plan(&mut self, plan: BoxPlan) {
        self.plan = plan;
        self.release_next = true;
    }

    /// What it is set to. A change of clock stops it.
    pub fn set_settings(&mut self, settings: Settings) {
        if settings.clock != self.settings.clock {
            self.stop();
        }
        self.settings = settings;
    }

    /// Play from the start, on the own clock, or along with the host, if it is stopped (a box that plays, or records, goes on).
    pub fn play(&mut self) {
        if self.state == State::Stopped {
            self.state = State::Playing;
        }
    }

    /// Stop, and let go of what is on. A pass that is going on is lost, and the notes that the box had before it are as they were.
    pub fn stop(&mut self) {
        self.state = State::Stopped;
        self.clock = 0.0;
        self.release_next = true;
    }

    /// Record: from the beginning of a count-in (the own clock), or from the next start of the loop (the host's). Pressed while it records, it begins again.
    pub fn record(&mut self) {
        // (What is on is let go of by the count-in or the wait, which play nothing.)
        self.finishing = false;
        match self.settings.clock {
            Clock::Own => {
                self.clock = -COUNT_IN_BEATS;
                self.state = State::Recording(Phase::CountIn);
            }
            Clock::Host => {
                self.clock = 0.0;
                self.state = State::Recording(Phase::WaitLoop);
            }
        }
    }

    /// Finish recording: a count-in or a wait is given up (the box stops), and a pass that is going on is the last one.
    pub fn finish_recording(&mut self) {
        match self.state {
            State::Recording(Phase::Pass { .. }) => self.finishing = true,
            State::Recording(_) => self.stop(),
            _ => {}
        }
    }

    /// What it made for the block that `process` ran for: note ons and offs by frame, in order.
    pub fn events(&self) -> &[BoxEvent] {
        &self.out
    }

    /// The ticks of the metronome in that block.
    pub fn ticks(&self) -> &[Tick] {
        &self.ticks
    }

    /// Whether a pass of recording ended in the block that `process` ran for, once: the notes that it made are the box's (`notes`).
    pub fn take_finished(&mut self) -> bool {
        std::mem::take(&mut self.finished)
    }

    /// Run for one block: `input` is every event of the block with its source and frame, of which the box takes the note ons and offs of the sources it records, while it records.
    pub fn process(&mut self, block: &Block, input: impl Iterator<Item = (Source, u64, [u8; 3])>) {
        self.out.clear();
        self.ticks.clear();
        self.heard.clear();
        if self.release_next {
            self.release(0);
            self.release_next = false;
        }
        let beats = block.beats();
        // (A block of no frames is no beats. A stopped box that went on would make nothing: this is only the quick way out.)
        if beats == 0.0 || self.state == State::Stopped {
            return;
        }

        // Where the block begins on the box's clock.
        let c0 = match self.settings.clock {
            Clock::Own => self.clock,
            Clock::Host => match self.follow(block.host_beat) {
                Some(beat) => beat,
                None => return,
            },
        };
        let c1 = c0 + beats;
        let window = Window { c0, beats, frames: block.frames };
        self.listen(&window, input);

        // The block in segments, cut where the box changes what it is doing (a count-in is over, the loop begins, a pass ends).
        let mut t = c0;
        for _ in 0..MAX_SEGMENTS {
            let end = self.next_change(t).min(c1);
            let moved = end > t + EPS;
            if moved {
                self.segment(t, end, &window);
                t = end;
            }
            let changed = self.change_at(t);
            if !moved && !changed {
                break;
            }
        }
        self.clock = c1;
        if self.state == State::Stopped {
            // A box that plays once, and has played it, is at its start again (its offs, at the loop's end, were made by the last segment).
            self.clock = 0.0;
        }
    }

    /// The host's clock: where the block begins, or none if the host does not play. A box that the host left (it stopped, or went somewhere else) lets go of what it has on, and a pass that it was making is lost.
    fn follow(&mut self, host_beat: Option<f64>) -> Option<f64> {
        let Some(beat) = host_beat.filter(|beat| beat.is_finite()) else {
            self.lose_the_place();
            return None;
        };
        // The host is not where the last block ended: it jumped (a loop, a seek). (A box that has not run yet has nothing to lose.)
        if (beat - self.clock).abs() > JUMP_BEATS {
            self.lose_the_place();
        }
        Some(beat)
    }

    /// Let go of what is on, and begin the pass that was going on again, at the next start of the loop.
    fn lose_the_place(&mut self) {
        self.release(0);
        if matches!(self.state, State::Recording(Phase::Pass { .. })) {
            self.state = State::Recording(Phase::WaitLoop);
        }
    }

    /// What is heard in the block, if a recording is going on, with the clock that it came at.
    fn listen(&mut self, window: &Window, input: impl Iterator<Item = (Source, u64, [u8; 3])>) {
        if !matches!(self.state, State::Recording(_)) {
            return;
        }
        for (source, frame, bytes) in input {
            if self.heard.len() == MAX_HEARD_PER_BLOCK {
                break;
            }
            if !self.settings.inputs.contains(source) || bytes[1] > 127 {
                continue;
            }
            let on = match bytes[0] & 0xF0 {
                0x90 if bytes[2] > 0 => true,
                0x90 | 0x80 => false,
                _ => continue,
            };
            self.heard.push((window.clock_at(frame), on, bytes[1], bytes[2]));
        }
    }

    /// The clock at which the box next changes what it is doing, from `t`: infinity if it does not.
    fn next_change(&self, t: f64) -> f64 {
        let length = f64::from(self.plan.length);
        match self.state {
            State::Recording(Phase::CountIn) => 0.0,
            State::Recording(Phase::WaitLoop) => ((t - EPS) / length).ceil() * length,
            State::Recording(Phase::Pass { start }) => start + length,
            State::Playing if self.settings.clock == Clock::Own && !self.settings.looping => length,
            _ => f64::INFINITY,
        }
    }

    /// The change that is due at the clock `t`, made if there is one: whether there was.
    fn change_at(&mut self, t: f64) -> bool {
        let length = f64::from(self.plan.length);
        match self.state {
            State::Recording(Phase::CountIn) if t >= -EPS => {
                self.pass.clear();
                self.state = State::Recording(Phase::Pass { start: 0.0 });
                true
            }
            State::Recording(Phase::WaitLoop) => {
                let start = (t / length).round() * length;
                if (t - start).abs() < EPS {
                    self.pass.clear();
                    self.state = State::Recording(Phase::Pass { start });
                    true
                } else {
                    false
                }
            }
            State::Recording(Phase::Pass { start }) if t >= start + length - EPS => {
                self.finish_pass();
                if self.settings.overdub && !self.finishing {
                    self.state = State::Recording(Phase::Pass { start: start + length });
                } else {
                    self.state = State::Playing;
                }
                true
            }
            State::Playing if self.settings.clock == Clock::Own && !self.settings.looping && t >= length - EPS => {
                self.state = State::Stopped;
                true
            }
            _ => false,
        }
    }

    /// What the box does from the clock `a` to `b` (in the block that `window` is): it ticks, takes what it hears into the pass, and plays what it has, unless a recording is about to replace it.
    fn segment(&mut self, a: f64, b: f64, window: &Window) {
        // The metronome ticks on the whole beats of a count-in and of a pass (not on the host's clock: the host has one).
        let ticking = self.settings.click && self.settings.clock == Clock::Own && matches!(self.state, State::Recording(Phase::CountIn | Phase::Pass { .. }));
        if ticking {
            let mut beat = (a - EPS).ceil();
            while beat < b - EPS {
                if self.ticks.len() < self.ticks.capacity() {
                    self.ticks.push(Tick { frame: window.frame_of(beat) as usize, downbeat: beat.rem_euclid(COUNT_IN_BEATS) < EPS });
                }
                beat += 1.0;
            }
        }

        // What it hears is taken into the pass.
        if let State::Recording(Phase::Pass { start }) = self.state {
            for i in 0..self.heard.len() {
                let (clock, on, note, velocity) = self.heard[i];
                if clock >= a - EPS && clock < b - EPS {
                    self.hear(clock - start, on, note, velocity);
                }
            }
        }

        // What it plays: nothing in a count-in and a wait, and nothing while a recording replaces what it has (a recording that adds to it is played over).
        let plays = match self.state {
            State::Playing => true,
            State::Recording(Phase::Pass { .. }) => self.settings.overdub,
            State::Recording(_) | State::Stopped => false,
        };
        if !plays {
            self.release(window.frame_of(a));
            return;
        }
        self.play_steps(a, b, window);
    }

    /// Make the note ons and offs of the loop from the clock `a` to `b`: in each cycle of the loop that the stretch is in, the steps whose beats are in it, an off at the loop's end being in the stretch that reaches it.
    fn play_steps(&mut self, a: f64, b: f64, window: &Window) {
        let length = f64::from(self.plan.length);
        let lo = a.max(0.0);
        if lo >= b {
            return;
        }
        let mut cycle = (lo / length).floor();
        while cycle * length < b - EPS {
            let base = cycle * length;
            let from = lo.max(base) - base;
            let to = b.min(base + length) - base;
            let reaches_end = b >= base + length - EPS;
            for i in 0..self.plan.steps.len() {
                let step = self.plan.steps[i];
                let in_stretch = step.beat >= from - EPS && (reaches_end || step.beat < to - EPS);
                if !in_stretch {
                    continue;
                }
                if self.out.len() >= MAX_EVENTS_PER_BLOCK {
                    return;
                }
                let frame = window.frame_of(base + step.beat);
                let slot = step.note as usize;
                if step.on {
                    if self.sounding[slot] {
                        self.out.push(BoxEvent { frame, bytes: [0x80, step.note, 0] });
                    }
                    self.sounding[slot] = true;
                    self.out.push(BoxEvent { frame, bytes: [0x90, step.note, step.velocity] });
                } else if self.sounding[slot] {
                    self.sounding[slot] = false;
                    self.out.push(BoxEvent { frame, bytes: [0x80, step.note, 0] });
                }
            }
            cycle += 1.0;
        }
    }

    /// A note that was heard `pos` beats into the pass.
    fn hear(&mut self, pos: f64, on: bool, note: u8, velocity: u8) {
        let slot = note as usize;
        if on {
            // A key that goes down again before it came up ends the note that it held.
            if let Some((began, v)) = self.pass.held[slot].take() {
                self.push_note(began, (pos - began).max(MIN_NOTE_BEATS), note, v);
            }
            self.pass.held[slot] = Some((pos, velocity));
        } else if let Some((began, v)) = self.pass.held[slot].take() {
            self.push_note(began, (pos - began).max(MIN_NOTE_BEATS), note, v);
        }
    }

    fn push_note(&mut self, start: f64, length: f64, note: u8, velocity: u8) {
        if self.pass.notes.len() < MAX_NOTES {
            self.pass.notes.push(BoxNote { start: start as f32, length: length as f32, note, velocity });
        }
    }

    /// The pass is over: what is still held ends at the loop's end, and what was taken is the box's notes (all of them, or added to those that it had).
    fn finish_pass(&mut self) {
        let length = f64::from(self.plan.length);
        for note in 0..128usize {
            if let Some((began, velocity)) = self.pass.held[note].take() {
                self.push_note(began, (length - began).max(MIN_NOTE_BEATS), note as u8, velocity);
            }
        }
        if !self.settings.overdub {
            self.plan.notes.clear();
        }
        let room = MAX_NOTES.saturating_sub(self.plan.notes.len());
        for note in self.pass.notes.drain(..).take(room) {
            if let Some(note) = BoxPlan::tidy(&note) {
                self.plan.notes.push(note);
            }
        }
        self.plan.compile();
        self.finished = true;
    }

    /// Let go of every pitch that is on, at `frame`.
    fn release(&mut self, frame: u64) {
        for note in 0..128usize {
            if self.sounding[note] {
                self.sounding[note] = false;
                if self.out.len() < MAX_EVENTS_PER_BLOCK {
                    self.out.push(BoxEvent { frame, bytes: [0x80, note as u8, 0] });
                }
            }
        }
    }
}

/// The metronome's sound: a click that begins at each tick, which is added to the audio that it is given (the output, since a box has no lane).
#[derive(Default)]
pub struct Click {
    phase: f32,
    remaining: u32,
    downbeat: bool,
}

impl Click {
    /// Whether no click is sounding: a block that has no tick has nothing to add.
    pub fn is_idle(&self) -> bool {
        self.remaining == 0
    }

    /// Add the clicks of one block to `out`: `ticks` are in the order of their frames. A click that is not over at the end of the block goes on in the next.
    pub fn render(&mut self, ticks: &[Tick], sample_rate: f32, out: &mut [Vec<f32>]) {
        if ticks.is_empty() && self.remaining == 0 {
            return;
        }
        let duration = (CLICK_DURATION_SECS * sample_rate) as u32;
        let frames = out.first().map_or(0, Vec::len);
        let mut next = 0;
        for i in 0..frames {
            while next < ticks.len() && ticks[next].frame <= i {
                self.remaining = duration;
                self.phase = 0.0;
                self.downbeat = ticks[next].downbeat;
                next += 1;
            }
            if self.remaining == 0 {
                continue;
            }
            let freq = if self.downbeat { CLICK_DOWNBEAT_FREQ } else { CLICK_FREQ };
            self.phase = (self.phase + freq / sample_rate) % 1.0;
            let t = 1.0 - self.remaining as f32 / duration as f32;
            let sample = (self.phase * std::f32::consts::TAU).sin() * (-t * 8.0).exp() * CLICK_VOLUME;
            for channel in out.iter_mut() {
                channel[i] += sample;
            }
            self.remaining -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alloc_guard::allocations_in;

    const SR: f32 = 48_000.0;
    const BPM: f32 = 120.0;
    /// A beat in frames, at 120 beats a minute and 48 kHz.
    const BEAT: u64 = 24_000;
    const KEYS: Source = Source::KEYS;

    fn note(start: f32, length: f32, note: u8) -> BoxNote {
        BoxNote { start, length, note, velocity: 100 }
    }

    fn on(note: u8, velocity: u8) -> [u8; 3] {
        [0x90, note, velocity]
    }

    fn off(note: u8) -> [u8; 3] {
        [0x80, note, 0]
    }

    /// A box run block by block, with what it did kept by the frame since the start.
    struct Rig {
        b: BoxPlayer,
        block: usize,
        at: u64,
        bpm: f32,
        /// Where the host is at the start of the next block (it goes on by the blocks that are run), or not playing.
        host: Option<f64>,
        /// What is heard, by the frame since the start.
        input: Vec<(Source, u64, [u8; 3])>,
        events: Vec<(u64, [u8; 3])>,
        ticks: Vec<(u64, bool)>,
        finished: Vec<Vec<BoxNote>>,
    }

    impl Rig {
        fn new(block: usize, length: f32, notes: &[BoxNote]) -> Rig {
            let mut b = BoxPlayer::new(Source::pattern(0));
            b.set_plan(BoxPlan::new(length, notes));
            Rig { b, block, at: 0, bpm: BPM, host: None, input: Vec::new(), events: Vec::new(), ticks: Vec::new(), finished: Vec::new() }
        }

        fn with(mut self, settings: Settings) -> Rig {
            self.b.set_settings(settings);
            self
        }

        /// A box on the host's clock, with the host at beat `host`.
        fn hosted(block: usize, length: f32, notes: &[BoxNote], host: Option<f64>) -> Rig {
            let mut rig = Rig::new(block, length, notes).with(Settings { clock: Clock::Host, ..Settings::default() });
            rig.host = host;
            rig
        }

        /// Run blocks to cover `frames` frames.
        fn run(&mut self, frames: u64) {
            let end = self.at + frames;
            while self.at < end {
                let (start, len) = (self.at, self.block as u64);
                let heard: Vec<(Source, u64, [u8; 3])> = self.input.iter().filter(|(_, frame, _)| *frame >= start && *frame < start + len).map(|&(source, frame, bytes)| (source, frame - start, bytes)).collect();
                let block = Block { frames: self.block, sample_rate: SR, bpm: self.bpm, host_beat: self.host };
                self.b.process(&block, heard.into_iter());
                for event in self.b.events() {
                    self.events.push((start + event.frame, event.bytes));
                }
                for tick in self.b.ticks() {
                    self.ticks.push((start + tick.frame as u64, tick.downbeat));
                }
                if self.b.take_finished() {
                    self.finished.push(self.b.notes().to_vec());
                }
                self.at += len;
                if let Some(host) = self.host.as_mut() {
                    *host += self.block as f64 * f64::from(self.bpm) / 60.0 / f64::from(SR);
                }
            }
        }

        fn beats(&mut self, beats: f64) {
            self.run((beats * BEAT as f64).round() as u64);
        }
    }

    /// The pitches that are on after these events, having seen that no note began while it was on or ended while it was off, and that the frames never go back.
    fn on_after(events: &[(u64, [u8; 3])]) -> Vec<u8> {
        let mut on = [false; 128];
        let mut last = 0;
        for &(frame, bytes) in events {
            assert!(frame >= last, "the events are in order of their frames: {frame} after {last}");
            last = frame;
            let n = bytes[1] as usize;
            match bytes[0] {
                0x90 => {
                    assert!(!on[n], "note {n} began at {frame} while it was on");
                    on[n] = true;
                }
                0x80 => {
                    assert!(on[n], "note {n} ended at {frame} while it was off");
                    on[n] = false;
                }
                other => panic!("an event that is no note: {other:#x}"),
            }
        }
        (0..128u8).filter(|&n| on[n as usize]).collect()
    }

    fn steps_of(plan: &BoxPlan) -> Vec<(f64, bool, u8)> {
        plan.steps.iter().map(|step| (step.beat, step.on, step.note)).collect()
    }

    // ---- the plan --------------------------------------------------------------------------------------------------------------------------------

    #[test]
    fn a_plan_has_an_on_and_an_off_for_each_note_in_the_order_of_the_loop() {
        let plan = BoxPlan::new(4.0, &[note(2.0, 1.0, 64), note(0.0, 1.5, 60)]);
        assert_eq!(plan.length(), 4.0);
        assert_eq!(plan.notes(), &[note(0.0, 1.5, 60), note(2.0, 1.0, 64)], "the notes are in the order of their starts");
        assert_eq!(steps_of(&plan), [(0.0, true, 60), (1.5, false, 60), (2.0, true, 64), (3.0, false, 64)]);
    }

    #[test]
    fn a_note_that_goes_past_the_end_ends_there_and_one_that_begins_past_it_or_has_no_length_is_kept_and_not_played() {
        let plan = BoxPlan::new(4.0, &[note(3.0, 2.0, 60), note(4.0, 1.0, 62), note(5.0, 1.0, 64), note(1.0, 0.0, 65)]);
        assert_eq!(plan.notes().len(), 4, "all of them are the box's: a longer loop plays them");
        assert_eq!(steps_of(&plan), [(3.0, true, 60), (4.0, false, 60)]);
    }

    #[test]
    fn a_note_with_no_length_is_not_played_and_ends_no_other_note() {
        let plan = BoxPlan::new(4.0, &[note(0.0, 3.0, 60), note(1.0, 0.0, 60), note(5.0, 1.0, 60)]);
        assert_eq!(steps_of(&plan), [(0.0, true, 60), (3.0, false, 60)], "neither the one with no length nor the one past the end of the loop ends the first");
    }

    #[test]
    fn a_note_is_ended_by_the_next_of_its_pitch_and_an_off_comes_before_an_on_at_one_beat() {
        // 60 from 0 for 3 beats is cut at 1 by the 60 that begins there, which ends at 2 where the third begins.
        let plan = BoxPlan::new(4.0, &[note(0.0, 3.0, 60), note(1.0, 1.0, 60), note(2.0, 1.0, 60)]);
        assert_eq!(steps_of(&plan), [(0.0, true, 60), (1.0, false, 60), (1.0, true, 60), (2.0, false, 60), (2.0, true, 60), (3.0, false, 60)]);
    }

    #[test]
    fn two_notes_of_one_pitch_at_one_beat_are_one_note_the_longer() {
        let plan = BoxPlan::new(4.0, &[note(1.0, 2.0, 60), note(1.0, 1.0, 60)]);
        assert_eq!(steps_of(&plan), [(1.0, true, 60), (3.0, false, 60)]);
    }

    #[test]
    fn what_is_not_a_note_is_left_out_and_the_rest_is_brought_into_range() {
        let odd = [
            BoxNote { start: -1.0, length: 2.0, note: 60, velocity: 0 },
            BoxNote { start: 1.0, length: 1.0, note: 128, velocity: 100 },
            BoxNote { start: f32::NAN, length: 1.0, note: 61, velocity: 100 },
            BoxNote { start: 1.0, length: f32::INFINITY, note: 62, velocity: 100 },
            BoxNote { start: 1.0, length: -3.0, note: 63, velocity: 200 },
        ];
        let plan = BoxPlan::new(4.0, &odd);
        assert_eq!(plan.notes(), &[BoxNote { start: 0.0, length: 2.0, note: 60, velocity: 1 }, BoxNote { start: 1.0, length: 0.0, note: 63, velocity: 127 }]);
        assert_eq!(steps_of(&plan), [(0.0, true, 60), (2.0, false, 60)]);
    }

    #[test]
    fn the_length_is_kept_between_the_shortest_and_the_longest_loop() {
        assert_eq!(BoxPlan::new(0.0, &[]).length(), MIN_LENGTH_BEATS);
        assert_eq!(BoxPlan::new(-4.0, &[]).length(), MIN_LENGTH_BEATS);
        assert_eq!(BoxPlan::new(f32::NAN, &[]).length(), MIN_LENGTH_BEATS);
        assert_eq!(BoxPlan::new(f32::INFINITY, &[]).length(), MIN_LENGTH_BEATS);
        assert_eq!(BoxPlan::new(1000.0, &[]).length(), MAX_LENGTH_BEATS);
        assert_eq!(BoxPlan::new(6.5, &[]).length(), 6.5);
    }

    #[test]
    fn a_plan_has_room_for_all_the_notes_that_a_box_holds_and_holds_no_more() {
        let plan = BoxPlan::new(4.0, &[]);
        assert!(plan.notes.capacity() >= MAX_NOTES && plan.steps.capacity() >= 2 * MAX_NOTES);
        let many: Vec<BoxNote> = (0..MAX_NOTES + 10).map(|i| note(i as f32 * 0.01, 0.005, (i % 100) as u8)).collect();
        let plan = BoxPlan::new(256.0, &many);
        assert_eq!(plan.notes().len(), MAX_NOTES);
    }

    // ---- playing ---------------------------------------------------------------------------------------------------------------------------------

    #[test]
    fn a_box_that_plays_makes_the_ons_and_offs_of_its_notes_at_their_frames() {
        let mut r = Rig::new(1000, 4.0, &[note(1.0, 1.0, 60), note(2.5, 0.5, 64)]);
        r.b.play();
        assert_eq!(r.b.status(), Status::Playing);
        r.beats(4.0);
        assert_eq!(r.events, [(BEAT, on(60, 100)), (2 * BEAT, off(60)), (60_000, on(64, 100)), (72_000, off(64))]);
    }

    #[test]
    fn a_box_that_loops_plays_again_from_the_start_at_the_end_of_its_length() {
        let mut r = Rig::new(1000, 4.0, &[note(1.0, 1.0, 60)]);
        r.b.play();
        r.beats(12.0);
        let cycle = 4 * BEAT;
        assert_eq!(r.events, [(BEAT, on(60, 100)), (2 * BEAT, off(60)), (cycle + BEAT, on(60, 100)), (cycle + 2 * BEAT, off(60)), (2 * cycle + BEAT, on(60, 100)), (2 * cycle + 2 * BEAT, off(60))]);
        assert_eq!(r.b.status(), Status::Playing);
    }

    #[test]
    fn a_note_that_ends_where_the_loop_does_is_let_go_of_at_its_end_before_the_next_cycle_begins() {
        let mut r = Rig::new(1000, 2.0, &[note(0.0, 0.5, 60), note(1.5, 0.5, 60)]);
        r.b.play();
        r.beats(4.0);
        // The off at the very end of a cycle is in the last frame of the block that reaches it, and the first note of the next cycle is in the first frame of the one after.
        assert_eq!(r.events, [(0, on(60, 100)), (12_000, off(60)), (36_000, on(60, 100)), (47_999, off(60)), (48_000, on(60, 100)), (60_000, off(60)), (84_000, on(60, 100)), (95_999, off(60))]);
        assert!(on_after(&r.events).is_empty());
    }

    #[test]
    fn what_a_box_plays_does_not_depend_on_the_size_of_the_block() {
        let notes = [note(0.0, 0.5, 60), note(0.333, 0.25, 64), note(1.75, 1.0, 67), note(3.9, 0.1, 72), note(2.0, 4.0, 48)];
        // Twelve beats, and a block that does not divide them runs a little past: what is compared is what comes before the last hundred frames, where the box begins a fourth loop.
        let play = |block: usize| {
            let mut r = Rig::new(block, 4.0, &notes);
            r.b.play();
            r.beats(12.0);
            on_after(&r.events);
            r.events.into_iter().filter(|(frame, _)| *frame < 12 * BEAT - 100).collect::<Vec<_>>()
        };
        let reference = play(1000);
        assert_eq!(reference.len(), 3 * 10 - 2, "an on and an off for each of five notes in each of three cycles, less the offs at the very end of the last (the long note is cut at the loop's end)");
        for block in [1, 7, 100, 480, 1024, 4096, 6000, 24_000] {
            let events = play(block);
            assert_eq!(events.len(), reference.len(), "block {block}: {events:?}");
            for (got, want) in events.iter().zip(&reference) {
                assert_eq!(got.1, want.1, "block {block}");
                assert!(got.0.abs_diff(want.0) <= 1, "block {block}: {got:?} is not {want:?}");
            }
        }
    }

    #[test]
    fn a_box_that_does_not_loop_plays_once_and_stops() {
        let mut r = Rig::new(1000, 4.0, &[note(1.0, 1.0, 60), note(3.5, 0.5, 64)]).with(Settings { looping: false, ..Settings::default() });
        r.b.play();
        r.beats(12.0);
        assert_eq!(r.events, [(BEAT, on(60, 100)), (2 * BEAT, off(60)), (84_000, on(64, 100)), (95_999, off(64))]);
        assert_eq!(r.b.status(), Status::Stopped);
        // And plays once more from the start when it is told.
        r.b.play();
        r.beats(2.0);
        assert_eq!(r.events.len(), 5);
        assert_eq!(r.events[4], (12 * BEAT + BEAT, on(60, 100)));
    }

    #[test]
    fn a_box_goes_by_the_tempo_it_is_given_now() {
        let mut slow = Rig::new(1000, 4.0, &[note(1.0, 1.0, 60)]);
        slow.bpm = 60.0;
        slow.b.play();
        slow.run(4 * 48_000);
        assert_eq!(slow.events, [(48_000, on(60, 100)), (96_000, off(60))], "a beat is 48000 frames at 60 beats a minute");

        // The tempo changes after the first beat (24000 frames at 120): the beats after it are 12000 frames.
        let mut r = Rig::new(1000, 4.0, &[note(2.0, 1.0, 60)]);
        r.b.play();
        r.beats(1.0);
        r.bpm = 240.0;
        r.run(3 * 12_000);
        assert_eq!(r.events, [(BEAT + 12_000, on(60, 100)), (BEAT + 24_000, off(60))]);
    }

    #[test]
    fn a_stopped_box_makes_nothing_and_stopping_lets_go_of_what_is_on() {
        let mut r = Rig::new(1000, 4.0, &[note(1.0, 2.0, 60)]);
        r.beats(2.0);
        assert!(r.events.is_empty() && r.b.status() == Status::Stopped);
        let began = r.at;
        r.b.play();
        r.run(30_000);
        assert_eq!(r.events, [(began + BEAT, on(60, 100))]);
        r.b.stop();
        r.run(5000);
        assert_eq!(r.events, [(began + BEAT, on(60, 100)), (began + 30_000, off(60))], "it lets go in the first block after it was stopped");
        r.beats(8.0);
        assert_eq!(r.events.len(), 2);
        // Play begins again from the start, and not from where it was stopped.
        let again = r.at;
        r.b.play();
        r.beats(2.0);
        assert_eq!(r.events[2], (again + BEAT, on(60, 100)));
    }

    #[test]
    fn a_new_plan_lets_go_of_what_is_on_and_is_played_from_where_the_box_is() {
        let mut r = Rig::new(1000, 4.0, &[note(0.0, 3.0, 60)]);
        r.b.play();
        r.beats(1.0);
        r.b.set_plan(BoxPlan::new(4.0, &[note(2.0, 1.0, 64)]));
        r.beats(3.0);
        assert_eq!(r.events, [(0, on(60, 100)), (BEAT, off(60)), (2 * BEAT, on(64, 100)), (3 * BEAT, off(64))]);
    }

    #[test]
    fn a_block_of_no_frames_or_a_tempo_that_is_not_one_makes_nothing_and_moves_nothing() {
        let mut b = BoxPlayer::new(Source::pattern(0));
        b.set_plan(BoxPlan::new(4.0, &[note(0.0, 1.0, 60)]));
        b.play();
        for (frames, sample_rate, bpm) in [(0, SR, BPM), (1000, SR, 0.0), (1000, SR, -120.0), (1000, SR, f32::NAN), (1000, SR, f32::INFINITY), (1000, 0.0, BPM), (1000, f32::NAN, BPM)] {
            b.process(&Block { frames, sample_rate, bpm, host_beat: None }, std::iter::empty());
            assert!(b.events().is_empty(), "frames {frames}, rate {sample_rate}, tempo {bpm}");
        }
        // The first real block is still the beginning of the loop.
        b.process(&Block { frames: 1000, sample_rate: SR, bpm: BPM, host_beat: None }, std::iter::empty());
        assert_eq!(b.events(), [BoxEvent { frame: 0, bytes: on(60, 100) }]);
    }

    #[test]
    fn a_host_beat_that_is_no_number_is_a_host_that_does_not_play() {
        let mut b = BoxPlayer::new(Source::pattern(0));
        b.set_plan(BoxPlan::new(4.0, &[note(0.0, 1.0, 60)]));
        b.set_settings(Settings { clock: Clock::Host, ..Settings::default() });
        b.play();
        for host_beat in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            b.process(&Block { frames: 1000, sample_rate: SR, bpm: BPM, host_beat: Some(host_beat) }, std::iter::empty());
            assert!(b.events().is_empty(), "{host_beat}");
        }
        b.process(&Block { frames: 1000, sample_rate: SR, bpm: BPM, host_beat: Some(8.0) }, std::iter::empty());
        assert_eq!(b.events(), [BoxEvent { frame: 0, bytes: on(60, 100) }]);
        // A note that is on is let go of by a host beat that is none, as by a host that stopped.
        b.process(&Block { frames: 1000, sample_rate: SR, bpm: BPM, host_beat: Some(f64::NAN) }, std::iter::empty());
        assert_eq!(b.events(), [BoxEvent { frame: 0, bytes: off(60) }]);
    }

    #[test]
    fn what_a_box_plays_never_leaves_a_note_on_whatever_the_notes_and_the_blocks() {
        // A small generator of notes, so that the same ones come every run.
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for case in 0..40 {
            let length = [1.0, 2.0, 3.0, 4.0, 7.5][case % 5];
            let notes: Vec<BoxNote> = (0..(next() % 30) as usize)
                .map(|_| BoxNote { start: (next() % 400) as f32 / 100.0, length: (next() % 300) as f32 / 100.0, note: 60 + (next() % 5) as u8, velocity: 1 + (next() % 127) as u8 })
                .collect();
            let plan = BoxPlan::new(length, &notes);
            let played = plan.steps.len() / 2;
            for block in [64, 333, 1000, 1024, 5000] {
                let mut r = Rig::new(block, length, &notes);
                r.b.play();
                // Whole cycles, and the block that reaches the end of the last.
                r.run((3.0 * f64::from(length) * BEAT as f64) as u64 + block as u64);
                let ons = r.events.iter().filter(|(_, bytes)| bytes[0] == 0x90).count();
                let offs = r.events.iter().filter(|(_, bytes)| bytes[0] == 0x80).count();
                assert!(ons >= 3 * played && ons <= 4 * played, "case {case}, block {block}: {ons} ons of {played} notes in three cycles and a block");
                assert!(offs <= ons && offs + played >= ons, "case {case}, block {block}: {offs} offs for {ons} ons");
                // Whatever is on is let go of by stopping, and nothing was on twice.
                let still_on = on_after(&r.events);
                r.b.stop();
                r.run(block as u64);
                let events_after = r.events.clone();
                assert!(on_after(&events_after).is_empty(), "case {case}, block {block}: {still_on:?} was on and is on");
            }
        }
    }

    // ---- recording on the box's own clock ----------------------------------------------------------------------------------------------------

    #[test]
    fn a_recording_is_counted_in_takes_the_keys_of_one_pass_and_plays_what_it_took_and_not_what_it_had() {
        let pass = 4 * BEAT;
        let mut r = Rig::new(1000, 4.0, &[note(0.5, 0.5, 72)]);
        r.input = vec![(KEYS, pass + BEAT, on(60, 90)), (KEYS, pass + 5 * BEAT / 2, off(60))];
        r.b.record();
        assert_eq!(r.b.status(), Status::CountingIn);
        r.beats(2.0);
        assert_eq!(r.b.status(), Status::CountingIn);
        r.beats(3.0);
        assert_eq!(r.b.status(), Status::Recording, "the pass began after four beats");
        r.beats(3.0);
        assert_eq!(r.b.status(), Status::Playing, "the pass is as long as the box, and one is all that an overwriting recording takes");
        assert_eq!(r.finished, [vec![BoxNote { start: 1.0, length: 1.5, note: 60, velocity: 90 }]], "the note that it had is gone");
        assert!(r.events.is_empty(), "nothing is played while it counts in and records: not what it had, and not what it takes");
        assert_eq!(r.ticks, [(0, true), (BEAT, false), (2 * BEAT, false), (3 * BEAT, false), (pass, true), (pass + BEAT, false), (pass + 2 * BEAT, false), (pass + 3 * BEAT, false)]);

        r.beats(4.0);
        assert_eq!(r.events, [(8 * BEAT + BEAT, on(60, 90)), (8 * BEAT + 5 * BEAT / 2, off(60))], "and it plays what it took, from the start of the loop that the pass ended");
        assert_eq!(r.ticks.len(), 8, "the metronome is over");
    }

    #[test]
    fn keys_held_in_the_count_in_are_not_recorded_and_a_key_held_at_the_end_ends_at_the_loops_end() {
        let pass = 4 * BEAT;
        let mut r = Rig::new(1000, 4.0, &[]);
        r.input = vec![(KEYS, 2 * BEAT, on(60, 90)), (KEYS, 3 * BEAT, off(60)), (KEYS, 3 * BEAT + 1000, on(61, 90)), (KEYS, pass + 1000, off(61)), (KEYS, pass + 7 * BEAT / 2, on(62, 70))];
        r.b.record();
        r.beats(8.0);
        assert_eq!(r.finished, [vec![BoxNote { start: 3.5, length: 0.5, note: 62, velocity: 70 }]], "the key that came up in the pass had gone down before it began: no note");
    }

    #[test]
    fn the_notes_of_a_pass_are_in_beats_whatever_the_size_of_the_block() {
        for block in [480, 1000, 1024, 4800, 24_000] {
            let pass = 4 * BEAT;
            let mut r = Rig::new(block, 4.0, &[]);
            r.input = vec![(KEYS, pass + BEAT + 5000, on(60, 90)), (KEYS, pass + 2 * BEAT + 7000, off(60))];
            r.b.record();
            r.beats(8.0);
            let [taken] = &r.finished[..] else { panic!("block {block}: {:?}", r.finished) };
            assert_eq!(taken.len(), 1);
            assert!((taken[0].start - 1.208_333).abs() < 1e-4 && (taken[0].length - 1.083_333).abs() < 1e-4, "block {block}: {:?}", taken[0]);
        }
    }

    #[test]
    fn a_recording_that_overdubs_plays_what_the_box_has_over_the_count_in_goes_on_pass_after_pass_and_ends_when_it_is_finished() {
        let (p1, p2) = (4 * BEAT, 8 * BEAT);
        let mut r = Rig::new(1000, 4.0, &[note(0.5, 0.5, 72)]).with(Settings { overdub: true, ..Settings::default() });
        r.input = vec![(KEYS, p1 + BEAT, on(62, 80)), (KEYS, p1 + 2 * BEAT, off(62)), (KEYS, p2 + 2 * BEAT, on(64, 70)), (KEYS, p2 + 5 * BEAT / 2, off(64))];
        r.b.record();
        r.beats(8.0);
        assert_eq!(r.finished, [vec![note(0.5, 0.5, 72), BoxNote { start: 1.0, length: 1.0, note: 62, velocity: 80 }]], "the notes it had, and the notes of the pass");
        assert_eq!(r.b.status(), Status::Recording, "and it goes on with another pass");
        assert_eq!(r.events, [(p1 + 12_000, on(72, 100)), (p1 + 24_000, off(72))], "what it had is played over the first pass; what the pass takes is not yet");

        r.beats(2.0);
        r.b.finish_recording();
        assert_eq!(r.b.status(), Status::Recording, "the pass that is going on is the last");
        r.beats(2.0);
        assert_eq!(r.b.status(), Status::Playing);
        assert_eq!(r.finished.len(), 2);
        assert_eq!(r.finished[1], [note(0.5, 0.5, 72), BoxNote { start: 1.0, length: 1.0, note: 62, velocity: 80 }, BoxNote { start: 2.0, length: 0.5, note: 64, velocity: 70 }]);
        assert_eq!(&r.events[2..], [(p2 + 12_000, on(72, 100)), (p2 + 24_000, off(72)), (p2 + 24_000, on(62, 80)), (p2 + 48_000, off(62))], "the second pass plays what the first made");

        r.beats(4.0);
        assert_eq!(r.events.len(), 6 + 6, "and it plays the three notes after it");
        assert!(on_after(&r.events).is_empty());
    }

    #[test]
    fn finishing_a_recording_that_is_counting_in_gives_it_up_and_changes_nothing() {
        let mut r = Rig::new(1000, 4.0, &[note(0.0, 1.0, 60)]);
        r.input = vec![(KEYS, 5 * BEAT, on(62, 90))];
        r.b.record();
        r.beats(2.0);
        r.b.finish_recording();
        assert_eq!(r.b.status(), Status::Stopped);
        r.beats(10.0);
        assert!(r.finished.is_empty() && r.events.is_empty());
        assert_eq!(r.b.notes(), [note(0.0, 1.0, 60)]);
    }

    #[test]
    fn stopping_loses_a_pass_that_is_going_on_and_the_box_has_the_notes_it_had() {
        let mut r = Rig::new(1000, 4.0, &[note(0.0, 1.0, 72)]);
        r.input = vec![(KEYS, 5 * BEAT, on(60, 90)), (KEYS, 6 * BEAT, off(60))];
        r.b.record();
        r.beats(6.5);
        assert_eq!(r.b.status(), Status::Recording);
        r.b.stop();
        r.beats(8.0);
        assert!(r.finished.is_empty());
        assert_eq!(r.b.notes(), [note(0.0, 1.0, 72)]);
        assert!(r.events.is_empty());
    }

    #[test]
    fn record_pressed_while_it_records_begins_again_and_one_pressed_while_it_plays_lets_go_of_what_sounds() {
        let mut r = Rig::new(1000, 4.0, &[note(1.0, 2.0, 60)]);
        r.b.play();
        r.run(30_000);
        assert_eq!(r.events, [(BEAT, on(60, 100))]);
        let began = r.at;
        r.b.record();
        r.run(2000);
        assert_eq!(r.events, [(BEAT, on(60, 100)), (30_000, off(60))], "what it was playing ends");
        assert_eq!(r.b.status(), Status::CountingIn);

        // A pass is going on, with a key in it; record again, and only the keys of the new pass are taken.
        let pass_1 = began + 4 * BEAT;
        r.input = vec![(KEYS, pass_1 + BEAT, on(60, 90)), (KEYS, pass_1 + 2 * BEAT, off(60))];
        r.beats(6.0);
        assert_eq!(r.b.status(), Status::Recording);
        r.b.record();
        assert_eq!(r.b.status(), Status::CountingIn);
        let pass_2 = r.at + 4 * BEAT;
        r.input.extend([(KEYS, pass_2 + 2 * BEAT, on(62, 70)), (KEYS, pass_2 + 3 * BEAT, off(62))]);
        r.beats(8.0);
        assert_eq!(r.finished, [vec![BoxNote { start: 2.0, length: 1.0, note: 62, velocity: 70 }]]);
    }

    #[test]
    fn only_the_note_ons_and_offs_of_the_sources_that_a_box_records_are_taken() {
        let device = Source::device(0);
        let pass = 4 * BEAT;
        let mut r = Rig::new(1000, 4.0, &[]).with(Settings { inputs: SourceSet::NONE.with(device), ..Settings::default() });
        r.input = vec![
            (KEYS, pass + BEAT, on(60, 90)),
            (KEYS, pass + 2 * BEAT, off(60)),
            (device, pass + BEAT, on(62, 100)),
            (device, pass + BEAT + 1000, [0xB0, 1, 64]),
            (device, pass + BEAT + 2000, [0xE0, 0, 64]),
            (device, pass + BEAT + 3000, [0xD0, 50, 0]),
            (Source::pattern(1), pass + BEAT, on(64, 100)),
            (Source::BROADCAST, pass + BEAT, on(65, 100)),
            (device, pass + 2 * BEAT, [0x90, 62, 0]),
            (device, pass + BEAT, [0x90, 200, 100]),
            (device, pass + 2 * BEAT, [0x80, 200, 0]),
        ];
        r.b.record();
        r.beats(8.0);
        assert_eq!(r.finished, [vec![BoxNote { start: 1.0, length: 1.0, note: 62, velocity: 100 }]], "the device's note, ended by a note on with no velocity; no other source, no controller");
    }

    #[test]
    fn a_key_that_goes_down_again_ends_the_note_it_held_and_one_that_comes_up_at_once_is_the_shortest_note() {
        let pass = 4 * BEAT;
        let mut r = Rig::new(1000, 4.0, &[]);
        r.input = vec![
            (KEYS, pass + 12_000, on(66, 70)),
            (KEYS, pass + 12_000, on(66, 75)),
            (KEYS, pass + 24_000, off(66)),
            (KEYS, pass + BEAT, on(60, 80)),
            (KEYS, pass + 2 * BEAT, on(60, 90)),
            (KEYS, pass + 3 * BEAT, off(60)),
            (KEYS, pass + 3 * BEAT + 12_000, on(64, 100)),
            (KEYS, pass + 3 * BEAT + 12_000, off(64)),
        ];
        r.b.record();
        r.beats(8.0);
        assert_eq!(
            r.finished,
            [vec![
                BoxNote { start: 0.5, length: 1.0 / 64.0, note: 66, velocity: 70 },
                BoxNote { start: 0.5, length: 0.5, note: 66, velocity: 75 },
                BoxNote { start: 1.0, length: 1.0, note: 60, velocity: 80 },
                BoxNote { start: 2.0, length: 1.0, note: 60, velocity: 90 },
                BoxNote { start: 3.5, length: 1.0 / 64.0, note: 64, velocity: 100 },
            ]]
        );
    }

    #[test]
    fn a_box_that_plays_hears_nothing() {
        let mut r = Rig::new(1000, 4.0, &[]);
        r.input = vec![(KEYS, BEAT, on(60, 90)), (KEYS, 2 * BEAT, off(60))];
        r.b.play();
        r.beats(8.0);
        assert!(r.finished.is_empty() && r.b.notes().is_empty());
    }

    #[test]
    fn play_pressed_while_it_records_changes_nothing() {
        let mut r = Rig::new(1000, 4.0, &[]);
        r.b.record();
        r.beats(1.0);
        r.b.play();
        assert_eq!(r.b.status(), Status::CountingIn);
    }

    #[test]
    fn the_ticks_mark_the_bars_of_four_beats_and_there_are_none_when_the_click_is_off_or_the_host_has_the_beat() {
        let mut r = Rig::new(1000, 8.0, &[]);
        r.b.record();
        r.beats(12.0);
        let downbeats: Vec<u64> = r.ticks.iter().filter(|(_, down)| *down).map(|(frame, _)| frame / BEAT).collect();
        assert_eq!(downbeats, [0, 4, 8], "the count-in, the pass's first beat and its fifth");
        assert_eq!(r.ticks.len(), 12);

        let mut quiet = Rig::new(1000, 4.0, &[]).with(Settings { click: false, ..Settings::default() });
        quiet.b.record();
        quiet.beats(8.0);
        assert!(quiet.ticks.is_empty() && quiet.b.status() == Status::Playing);

        let mut hosted = Rig::hosted(1000, 4.0, &[], Some(0.0));
        hosted.b.record();
        hosted.beats(12.0);
        assert!(hosted.ticks.is_empty(), "the host has its own metronome");
    }

    #[test]
    fn a_change_of_clock_stops_the_box_and_a_change_of_anything_else_does_not() {
        let mut r = Rig::new(1000, 4.0, &[note(0.0, 1.0, 60)]);
        r.b.play();
        r.beats(0.5);
        r.b.set_settings(Settings { looping: false, ..Settings::default() });
        assert_eq!(r.b.status(), Status::Playing);
        r.b.set_settings(Settings { clock: Clock::Host, ..Settings::default() });
        assert_eq!(r.b.status(), Status::Stopped);
        r.beats(0.1);
        assert_eq!(r.events.last().map(|(_, bytes)| *bytes), Some(off(60)), "it let go of what it had on");
    }

    #[test]
    fn a_block_with_more_events_than_a_box_takes_gives_it_the_first_of_them() {
        let mut b = BoxPlayer::new(Source::pattern(0));
        b.set_plan(BoxPlan::new(1.0, &[]));
        b.set_settings(Settings { click: false, ..Settings::default() });
        b.record();
        let block = Block { frames: 1000, sample_rate: SR, bpm: BPM, host_beat: None };
        // The count-in is 96 blocks of this size and the pass 24. In the first of the pass, 300 keys go down and up (600 events) at pitches that come round.
        for _ in 0..96 {
            b.process(&block, std::iter::empty());
        }
        let storm: Vec<(Source, u64, [u8; 3])> = (0..300u64).flat_map(|i| [(KEYS, i, on((i % 100) as u8, 90)), (KEYS, i, off((i % 100) as u8))]).collect();
        b.process(&block, storm.iter().copied());
        for _ in 0..23 {
            b.process(&block, std::iter::empty());
        }
        assert!(b.take_finished());
        assert_eq!(b.notes().len(), MAX_HEARD_PER_BLOCK / 2, "the first 512 of the 600 events are 256 notes");
    }

    #[test]
    fn a_pass_that_takes_more_notes_than_a_box_holds_keeps_the_first_and_asks_for_no_memory() {
        let mut b = BoxPlayer::new(Source::pattern(0));
        b.set_plan(BoxPlan::new(256.0, &[]));
        b.set_settings(Settings { click: false, ..Settings::default() });
        // Blocks of a second (two beats): the count-in is two of them and the pass 128. In each of the first 20 of the pass, 250 keys go down and up.
        let block = Block { frames: 48_000, sample_rate: SR, bpm: BPM, host_beat: None };
        let keys: Vec<(Source, u64, [u8; 3])> = (0..250u64).flat_map(|i| [(KEYS, i * 100, on((i % 100) as u8, 90)), (KEYS, i * 100 + 50, off((i % 100) as u8))]).collect();
        let ((), allocations) = allocations_in(|| {
            b.record();
            for nth in 0..2 + 128 {
                let in_the_pass = (2..2 + 20).contains(&nth);
                b.process(&block, if in_the_pass { &keys[..] } else { &[] }.iter().copied());
            }
        });
        assert_eq!(allocations, 0);
        assert!(b.take_finished());
        assert_eq!(b.notes().len(), MAX_NOTES, "5000 keys, and a box holds {MAX_NOTES}");

        // A recording that adds to a box that is full has no room for more.
        b.set_settings(Settings { click: false, overdub: true, ..Settings::default() });
        let ((), allocations) = allocations_in(|| {
            b.record();
            for nth in 0..2 + 128 {
                let in_the_pass = nth == 2;
                if nth == 3 {
                    b.finish_recording();
                }
                b.process(&block, if in_the_pass { &keys[..] } else { &[] }.iter().copied());
            }
        });
        assert_eq!(allocations, 0);
        assert_eq!(b.notes().len(), MAX_NOTES);
    }

    #[test]
    fn a_recording_that_was_told_to_finish_and_is_begun_again_goes_on_pass_after_pass() {
        let mut r = Rig::new(1000, 4.0, &[]).with(Settings { overdub: true, click: false, ..Settings::default() });
        r.b.record();
        r.beats(5.0);
        r.b.finish_recording();
        r.b.record();
        r.beats(10.0);
        assert_eq!(r.b.status(), Status::Recording, "a count-in, a pass and two beats of another: the new recording is not the last pass that the old one was told to be");
    }

    #[test]
    fn a_note_that_begins_while_the_same_pitch_is_still_on_lets_go_of_it_first() {
        let mut b = BoxPlayer::new(Source::pattern(0));
        b.set_plan(BoxPlan::new(4.0, &[note(0.0, 1.0, 60)]));
        b.play();
        // As if the end of the last note had never been made (a block that was full), and what was on were not let go of with the plan.
        b.release_next = false;
        b.sounding[60] = true;
        b.process(&Block { frames: 1000, sample_rate: SR, bpm: BPM, host_beat: None }, std::iter::empty());
        assert_eq!(b.events(), [BoxEvent { frame: 0, bytes: off(60) }, BoxEvent { frame: 0, bytes: on(60, 100) }], "no lane is given two ons for a note");
    }

    #[test]
    fn a_block_that_holds_a_change_goes_on_with_what_comes_after_it() {
        // Blocks of 1024 frames: the count-in (96,000 frames) ends 768 frames into a block, and the pass (the next 96,000) 512 into another.
        let pass = 4 * BEAT;
        let mut r = Rig::new(1024, 4.0, &[]);
        // (The first two keys come in the block that the count-in ends in, before it ends, and the last two after the end of the pass, in the block that it ends in: they are not the pass's.)
        r.input = vec![
            (KEYS, pass - 200, on(62, 60)),
            (KEYS, pass - 100, off(62)),
            (KEYS, pass + 100, on(60, 90)),
            (KEYS, pass + 2000, off(60)),
            (KEYS, 8 * BEAT + 50, on(64, 80)),
            (KEYS, 8 * BEAT + 60, off(64)),
        ];
        r.b.record();
        r.beats(8.0);
        let [taken] = &r.finished[..] else { panic!("{:?}", r.finished) };
        assert_eq!(taken.len(), 1, "{taken:?}");
        assert!((f64::from(taken[0].start) - 100.0 / 24_000.0).abs() < 1e-5 && (f64::from(taken[0].length) - 1900.0 / 24_000.0).abs() < 1e-5, "the key that comes in the block that the count-in ends in: {:?}", taken[0]);
        assert!(r.ticks.iter().any(|&(frame, down)| down && frame.abs_diff(pass) <= 1), "the first beat of the pass is ticked in the block that the count-in ends in: {:?}", r.ticks);
        // And the box plays what it took from the frame that the loop begins again at, in the block that the pass ends in.
        assert_eq!(r.events.len(), 1, "{:?}", r.events);
        assert!(r.events[0].0.abs_diff(8 * BEAT + 100) <= 1 && r.events[0].1 == on(60, 90), "{:?}", r.events);
    }

    #[test]
    fn a_box_that_does_not_loop_stops_at_the_end_of_its_length_in_the_middle_of_a_block() {
        let mut r = Rig::new(1024, 4.0, &[note(0.0, 1.0, 60)]).with(Settings { looping: false, ..Settings::default() });
        r.b.play();
        r.beats(4.5);
        assert_eq!(r.events.len(), 2, "its note, once, and not the first of another loop in the block that the end of this one is in: {:?}", r.events);
        assert_eq!(r.b.status(), Status::Stopped, "stopped at the end of its length, and not a beat after it");
    }

    #[test]
    fn a_block_with_no_beats_changes_nothing_not_even_a_wait_that_is_at_the_start_of_its_loop() {
        let mut b = BoxPlayer::new(Source::pattern(0));
        b.set_settings(Settings { clock: Clock::Host, ..Settings::default() });
        b.record();
        let at_the_loop = |bpm: f32, frames: usize| Block { frames, sample_rate: SR, bpm, host_beat: Some(8.0) };
        b.process(&at_the_loop(0.0, 1000), std::iter::empty());
        assert_eq!(b.status(), Status::Waiting, "a tempo that is none: no time passes, and the loop's start is not reached");
        b.process(&at_the_loop(BPM, 0), [(KEYS, 0, on(60, 90))].into_iter());
        assert_eq!(b.status(), Status::Waiting, "a block of no frames, with an event in it");
        b.process(&at_the_loop(BPM, 1000), std::iter::empty());
        assert_eq!(b.status(), Status::Recording);
    }

    // ---- the host's clock --------------------------------------------------------------------------------------------------------------------

    #[test]
    fn a_box_on_the_hosts_clock_plays_where_the_hosts_beat_is_and_not_while_the_host_is_stopped() {
        let mut r = Rig::hosted(1000, 4.0, &[note(1.0, 1.0, 60)], None);
        r.b.play();
        r.beats(2.0);
        assert!(r.events.is_empty(), "the host is not playing");
        // Beat 8.5 is half a beat into a loop: the note begins half a beat later, and ends a beat after that.
        r.host = Some(8.5);
        r.beats(2.0);
        let begin = 2 * BEAT;
        assert_eq!(r.events, [(begin + BEAT / 2, on(60, 100)), (begin + 3 * BEAT / 2, off(60))]);
    }

    #[test]
    fn a_box_on_the_hosts_clock_lets_go_when_the_host_stops_and_when_it_jumps() {
        let mut r = Rig::hosted(1000, 4.0, &[note(0.0, 3.0, 60)], Some(8.0));
        r.b.play();
        r.beats(1.0);
        assert_eq!(r.events, [(0, on(60, 100))]);
        r.host = None;
        r.beats(0.1);
        assert_eq!(r.events, [(0, on(60, 100)), (BEAT, off(60))], "the host stopped");

        r.host = Some(16.0);
        r.beats(1.0);
        assert_eq!(r.events.len(), 3, "the host plays again, and the note begins where the loop does");
        // A jump: the host's beat is not where the last block ended. What was on is let go of, and the note begins where the loop does (beat 100 is a loop's start).
        let at_the_jump = r.at;
        r.host = Some(100.0);
        r.beats(0.5);
        let events = r.events.clone();
        assert_eq!(&events[3..], [(at_the_jump, off(60)), (at_the_jump, on(60, 100))]);
        assert_eq!(on_after(&events), [60]);
    }

    #[test]
    fn the_hosts_clock_goes_round_with_its_beat_and_ignores_loop_off() {
        let mut r = Rig::hosted(1000, 2.0, &[note(0.0, 0.5, 60)], Some(0.0)).with(Settings { clock: Clock::Host, looping: false, ..Settings::default() });
        r.host = Some(0.0);
        r.b.play();
        r.beats(6.0);
        assert_eq!(r.events.iter().filter(|(_, bytes)| bytes[0] == 0x90).count(), 3, "once in each loop of the host's beat");
        assert_eq!(r.b.status(), Status::Playing);
    }

    #[test]
    fn a_recording_on_the_hosts_clock_waits_for_the_start_of_the_loop_and_takes_one_pass() {
        // The host is at beat 5 and the loop is 4 beats: the loop begins at beat 8, three beats on, and the pass is beats 8 to 12.
        let mut r = Rig::hosted(1000, 4.0, &[note(0.0, 1.0, 72)], Some(5.0));
        r.input = vec![(KEYS, 4 * BEAT + BEAT / 2, on(60, 90)), (KEYS, 5 * BEAT, off(60)), (KEYS, BEAT, on(61, 90)), (KEYS, 2 * BEAT, off(61))];
        r.b.record();
        assert_eq!(r.b.status(), Status::Waiting);
        r.beats(2.0);
        assert_eq!(r.b.status(), Status::Waiting, "the host is at beat 7");
        r.beats(2.0);
        assert_eq!(r.b.status(), Status::Recording, "and at 9: the pass began at 8");
        r.beats(3.0);
        assert_eq!(r.b.status(), Status::Playing, "the pass ended at 12");
        assert_eq!(r.finished, [vec![BoxNote { start: 1.5, length: 0.5, note: 60, velocity: 90 }]], "the key at host beat 9.5; the one in the wait is not taken");
        assert!(r.ticks.is_empty());
    }

    #[test]
    fn a_recording_on_the_hosts_clock_begins_at_the_start_of_the_loop_wherever_in_a_block_that_is() {
        // The host is at beat 5.01 and the loop begins at beat 8, 2.99 beats on: frame 71,760, in the block of 1,000 frames that begins at 71,000.
        let mut r = Rig::hosted(1000, 4.0, &[], Some(5.01));
        r.input = vec![(KEYS, 71_760 + 6000, on(60, 90)), (KEYS, 71_760 + 12_000, off(60))];
        r.b.record();
        r.beats(2.99 + 4.0);
        assert_eq!(r.finished, [vec![BoxNote { start: 0.25, length: 0.25, note: 60, velocity: 90 }]], "the key at a quarter of the way into the loop, and not a block after it began");

        // A host that is a hair below the start of a loop when a block begins is at it.
        let mut r = Rig::hosted(1000, 4.0, &[], Some(8.0 - 1e-12));
        r.input = vec![(KEYS, 6000, on(62, 70)), (KEYS, 12_000, off(62))];
        r.b.record();
        r.beats(4.0);
        assert_eq!(r.finished, [vec![BoxNote { start: 0.25, length: 0.25, note: 62, velocity: 70 }]]);
    }

    #[test]
    fn a_pass_on_the_hosts_clock_is_lost_when_the_host_stops_and_the_next_loop_is_waited_for() {
        let mut r = Rig::hosted(1000, 4.0, &[], Some(8.0));
        r.input = vec![(KEYS, BEAT, on(60, 90)), (KEYS, 2 * BEAT, off(60)), (KEYS, 8 * BEAT + BEAT, on(62, 70)), (KEYS, 8 * BEAT + 2 * BEAT, off(62))];
        r.b.record();
        r.beats(2.5);
        assert_eq!(r.b.status(), Status::Recording);
        r.host = None;
        r.beats(0.1);
        assert_eq!(r.b.status(), Status::Waiting, "the pass is lost");
        // The host plays again from the start of a loop, and the pass that begins takes the key that comes in it.
        r.host = Some(16.0);
        r.at = 8 * BEAT;
        r.beats(4.0);
        assert_eq!(r.finished, [vec![BoxNote { start: 1.0, length: 1.0, note: 62, velocity: 70 }]]);
    }

    #[test]
    fn a_pass_on_the_hosts_clock_is_lost_when_the_host_jumps() {
        let mut r = Rig::hosted(1000, 4.0, &[], Some(8.0));
        r.b.record();
        r.beats(2.0);
        assert_eq!(r.b.status(), Status::Recording);
        r.host = Some(4.0);
        r.beats(0.1);
        assert_eq!(r.b.status(), Status::Recording, "the host went back to the start of a loop, and the pass begins there");
        let mut other = Rig::hosted(1000, 4.0, &[], Some(8.0));
        other.b.record();
        other.beats(2.0);
        other.host = Some(5.0);
        other.beats(0.1);
        assert_eq!(other.b.status(), Status::Waiting, "the host went into the middle of a loop");
    }

    #[test]
    fn a_box_on_the_hosts_clock_plays_nothing_before_the_hosts_beat_0() {
        let mut r = Rig::hosted(1000, 4.0, &[note(3.5, 0.25, 60), note(0.5, 0.25, 64)], Some(-1.0));
        r.b.play();
        r.beats(1.0);
        assert!(r.events.is_empty(), "the host counts in: beat -0.5 is not the end of a loop that has not begun");
        r.beats(1.0);
        assert_eq!(r.events, [(BEAT + BEAT / 2, on(64, 100)), (BEAT + 3 * BEAT / 4, off(64))]);
    }

    #[test]
    fn a_box_on_the_hosts_clock_that_comes_in_the_middle_of_a_note_makes_no_end_for_a_note_that_it_did_not_begin() {
        let mut r = Rig::hosted(1000, 4.0, &[note(1.0, 1.0, 60)], Some(9.5));
        r.b.play();
        r.beats(1.0);
        assert!(r.events.is_empty(), "the host is a half beat into the note, whose end is not one for a note that nothing began");
    }

    // ---- the click -----------------------------------------------------------------------------------------------------------------------------

    fn crossings(samples: &[f32]) -> usize {
        samples.windows(2).filter(|pair| (pair[0] < 0.0) != (pair[1] < 0.0)).count()
    }

    #[test]
    fn a_click_is_a_short_sine_that_dies_away_and_the_downbeat_is_higher() {
        let mut click = Click::default();
        let mut out = vec![vec![0.0f32; 4800]; 2];
        click.render(&[Tick { frame: 100, downbeat: false }], SR, &mut out);
        assert!(out[0][..100].iter().all(|&s| s == 0.0), "nothing before the tick");
        assert!(out[0][100].abs() > 0.01, "it begins at its frame");
        assert!(out[0][100..1300].iter().any(|&s| s.abs() > 0.1));
        assert!(out[0][1250..1300].iter().all(|&s| s.abs() < 0.01), "it dies away");
        assert!(out[0][1300..].iter().all(|&s| s == 0.0), "25 ms of it, and then nothing");
        assert!(out[0].iter().all(|&s| s.abs() <= CLICK_VOLUME + 1e-6));
        assert_eq!(out[0], out[1], "every channel");
        let normal = crossings(&out[0][100..1300]);

        let mut down = vec![vec![0.0f32; 4800]; 1];
        Click::default().render(&[Tick { frame: 100, downbeat: true }], SR, &mut down);
        let high = crossings(&down[0][100..1300]);
        assert!((48..=52).contains(&normal), "1000 Hz for 25 ms is 25 cycles: {normal}");
        assert!((73..=77).contains(&high), "1500 Hz is 37.5: {high}");
    }

    #[test]
    fn a_click_that_is_not_over_at_the_end_of_the_block_goes_on_in_the_next_and_is_added_to_what_is_there() {
        let tick = Tick { frame: 4700, downbeat: false };
        let mut whole = vec![vec![0.0f32; 9600]];
        Click::default().render(&[tick], SR, &mut whole);

        let mut click = Click::default();
        let mut first = vec![vec![0.0f32; 4800]];
        let mut second = vec![vec![0.5f32; 4800]];
        click.render(&[tick], SR, &mut first);
        click.render(&[], SR, &mut second);
        assert_eq!(first[0], whole[0][..4800]);
        let carried: Vec<f32> = second[0].iter().map(|s| s - 0.5).collect();
        for (got, want) in carried.iter().zip(&whole[0][4800..]) {
            assert!((got - want).abs() < 1e-6);
        }
        assert!(second[0][1100..].iter().all(|&s| s == 0.5), "and ends where the whole one does");

        let mut quiet = vec![vec![0.25f32; 100]];
        click.render(&[], SR, &mut quiet);
        assert!(quiet[0].iter().all(|&s| s == 0.25), "nothing to click: nothing added");
    }

    // ---- what the audio thread may do ------------------------------------------------------------------------------------------------------

    #[test]
    fn a_box_asks_the_allocator_for_nothing_while_it_plays_records_and_ends_a_pass() {
        let mut b = BoxPlayer::new(Source::pattern(0));
        b.set_plan(BoxPlan::new(2.0, &[note(0.0, 0.5, 60), note(1.0, 0.5, 64)]));
        b.set_settings(Settings { overdub: true, ..Settings::default() });
        let heard = [(KEYS, 10u64, on(62, 100)), (KEYS, 500, off(62)), (KEYS, 700, on(66, 90))];
        let block = Block { frames: 1000, sample_rate: SR, bpm: BPM, host_beat: None };
        let ((), allocations) = allocations_in(|| {
            b.play();
            for _ in 0..100 {
                b.process(&block, heard.iter().copied());
            }
            b.record();
            // A count-in, passes that end (each makes the notes of the box what it took and what it had), and the end of the recording.
            for i in 0..2000 {
                b.process(&block, heard.iter().copied());
                b.take_finished();
                if i == 1500 {
                    b.finish_recording();
                }
            }
            b.stop();
            b.process(&block, std::iter::empty());
        });
        assert_eq!(allocations, 0);
        assert_eq!(b.status(), Status::Stopped);
        assert!(!b.notes().is_empty());
    }
}
