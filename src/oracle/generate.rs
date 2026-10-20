//! Sessions and scripts of notes made from a seed, so that the oracle has a few hundred of them to render (see `super`).
//!
//! A session is text, the same text a person would write, so that it goes through the loader that the program uses; and it is made from the built-in plugins only (their parameter
//! lists are read from the plugins, so a parameter that is added is drawn from too), since nothing else is on every machine. The numbers come from a small generator of our own
//! (xorshift64*, started from the seed through splitmix64), not from a library: the same seed makes the same session on every platform and in every version, which is what makes
//! a session's id a name for it.

use std::fmt::Write;

use crate::plugin::{ParameterInfo, builtin};

/// A generator of numbers that is the same everywhere (xorshift64*).
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        // splitmix64, once, so that the seeds 0, 1, 2 ... do not start the generator at nearly the same state.
        let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        Rng((z ^ (z >> 31)) | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A whole number from 0 up to but not including `n` (`n` is more than 0).
    pub fn below(&mut self, n: usize) -> usize {
        ((self.next_u64() >> 33) % n as u64) as usize
    }

    /// A number from 0 up to but not including 1.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    pub fn between(&mut self, low: f64, high: f64) -> f64 {
        low + self.unit() * (high - low)
    }

    pub fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

const EFFECTS: [&str; 4] = ["builtin:filter", "builtin:delay", "builtin:reverb", "builtin:drive"];
const WAVEFORMS: [&str; 6] = ["sine", "triangle", "saw", "square", "random", "smooth"];
const SYNCS: [&str; 7] = ["1/4", "1/8", "1/16", "1/8 triplet", "1/2 dotted", "1/4 triplet", "1 bar"];
const RANGES: [&str; 5] = ["C2-B4", "C4-", "-B3", "C3-C5", "E2-G#6"];
const NOTE_NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

/// The name a session gives a key (C4 is middle C, 60).
fn note_name(note: u8) -> String {
    format!("{}{}", NOTE_NAMES[usize::from(note % 12)], i32::from(note / 12) - 1)
}

/// A number as a session says it: three places, which parse back to the number that was meant.
fn num(x: f64) -> String {
    format!("{x:.3}")
}

fn parameters_of(plugin: &str) -> Vec<ParameterInfo> {
    builtin::load(plugin, 48_000.0, 256).expect("a built-in plugin").parameters()
}

/// A value for a parameter: a place in its list for a choice, else a number in its range.
fn value_for(rng: &mut Rng, parameter: &ParameterInfo) -> f64 {
    match &parameter.labels {
        Some(labels) => rng.below(labels.len()) as f64,
        None => f64::from(parameter.min) + rng.unit() * f64::from(parameter.max - parameter.min),
    }
}

/// A table of `count` settings of the plugin's parameters that the session would write as `params = { ... }`.
fn params_table(rng: &mut Rng, plugin: &str, most: usize) -> String {
    let parameters = parameters_of(plugin);
    let count = rng.below(most + 1);
    let mut chosen: Vec<usize> = Vec::new();
    for _ in 0..count {
        let at = rng.below(parameters.len());
        if !chosen.contains(&at) {
            chosen.push(at);
        }
    }
    let entries: Vec<String> = chosen.iter().map(|&at| format!("\"{}\" = {}", parameters[at].name, num(value_for(rng, &parameters[at])))).collect();
    format!("{{ {} }}", entries.join(", "))
}

/// What a modulator is, so that a cable between modulators can ask for what the other has.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Lfo,
    Envelope,
    Macro,
    Steps,
    Midi,
}

/// Where a modulator's cables may land.
enum Rack<'a> {
    /// A lane's: the plugins of its chain (the instrument first), by slot.
    Lane { chain: &'a [&'static str] },
    /// A group's: how many lanes are members, and the bus effects.
    Group { members: usize, bus: &'a [&'static str] },
}

fn modulator(rng: &mut Rng, rack: &Rack, kinds: &[Kind], index: usize, at: &str, out: &mut String) {
    let kind = kinds[index];
    writeln!(out, "[[{at}.modulator]]").unwrap();
    match kind {
        Kind::Lfo => {
            writeln!(out, "type = \"lfo\"\nwaveform = \"{}\"", rng.pick(&WAVEFORMS)).unwrap();
            writeln!(out, "rate = {}", num(rng.between(0.1, 12.0))).unwrap();
            if rng.chance(0.3) {
                writeln!(out, "sync = \"{}\"", rng.pick(&SYNCS)).unwrap();
            }
            if rng.chance(0.4) {
                writeln!(out, "retrigger = true").unwrap();
            }
            if rng.chance(0.3) {
                writeln!(out, "fade = {}", num(rng.between(0.0, 2.0))).unwrap();
            }
        }
        Kind::Envelope => {
            writeln!(out, "type = \"envelope\"").unwrap();
            writeln!(out, "attack = {}\ndecay = {}\nsustain = {}\nrelease = {}", num(rng.between(0.001, 0.3)), num(rng.between(0.01, 0.6)), num(rng.unit()), num(rng.between(0.01, 0.8))).unwrap();
        }
        Kind::Macro => {
            writeln!(out, "type = \"macro\"\nvalue = {}", num(rng.unit())).unwrap();
        }
        Kind::Steps => {
            writeln!(out, "type = \"steps\"").unwrap();
            let count = 2 + rng.below(15);
            let values: Vec<String> = (0..count).map(|_| num(rng.between(-1.0, 1.0))).collect();
            writeln!(out, "steps = [{}]", values.join(", ")).unwrap();
            if rng.chance(0.5) {
                writeln!(out, "length = {}", 1 + rng.below(count)).unwrap();
            }
            if rng.chance(0.4) {
                writeln!(out, "sync = \"{}\"", rng.pick(&SYNCS)).unwrap();
            } else {
                writeln!(out, "rate = {}", num(rng.between(1.0, 16.0))).unwrap();
            }
            if rng.chance(0.4) {
                writeln!(out, "glide = {}", num(rng.unit())).unwrap();
            }
            if rng.chance(0.3) {
                writeln!(out, "retrigger = false").unwrap();
            }
        }
        Kind::Midi => {
            writeln!(out, "type = \"midi\"").unwrap();
            match rng.below(7) {
                0 | 1 => writeln!(out, "midi_cc = {}", rng.pick(&[1, 7, 11, 74])).unwrap(),
                2 => writeln!(out, "midi_pitch_bend = true").unwrap(),
                3 => writeln!(out, "midi_aftertouch = true").unwrap(),
                4 => writeln!(out, "midi_velocity = true").unwrap(),
                5 => writeln!(out, "midi_key = true").unwrap(),
                // Unbound: it learns the first control that comes.
                _ => {}
            }
        }
    }
    let cables = 1 + rng.below(3);
    for _ in 0..cables {
        let depth = num(if rng.chance(0.25) { -rng.between(0.05, 1.0) } else { rng.between(0.05, 1.0) });
        let sibling = (0..kinds.len()).filter(|&other| other != index).collect::<Vec<_>>();
        if !sibling.is_empty() && rng.chance(0.2) {
            let other = *rng.pick(&sibling);
            let field = match kinds[other] {
                Kind::Lfo | Kind::Steps => "mod_rate",
                Kind::Envelope => *rng.pick(&["mod_attack", "mod_decay", "mod_sustain", "mod_release"]),
                Kind::Macro | Kind::Midi => continue,
            };
            writeln!(out, "[[{at}.modulator.target]]\n{field} = {other}\ndepth = {depth}").unwrap();
            continue;
        }
        match rack {
            Rack::Lane { chain } => {
                let slot = rng.below(chain.len());
                let parameters = parameters_of(chain[slot]);
                let parameter = &parameters[rng.below(parameters.len())];
                writeln!(out, "[[{at}.modulator.target]]\nparam = \"{}\"\nslot = {slot}\ndepth = {depth}", parameter.name).unwrap();
            }
            Rack::Group { members, bus } => {
                if !bus.is_empty() && (*members == 0 || rng.chance(0.4)) {
                    let effect = rng.below(bus.len());
                    let parameters = parameters_of(bus[effect]);
                    let parameter = &parameters[rng.below(parameters.len())];
                    writeln!(out, "[[{at}.modulator.target]]\nbus = {effect}\nparam = \"{}\"\ndepth = {depth}", parameter.name).unwrap();
                } else if *members > 0 {
                    let parameters = parameters_of("builtin:osc");
                    let parameter = &parameters[rng.below(parameters.len())];
                    writeln!(out, "[[{at}.modulator.target]]\nmember = {}\nparam = \"{}\"\ndepth = {depth}", rng.below(*members), parameter.name).unwrap();
                }
            }
        }
    }
}

fn effects(rng: &mut Rng, at: &str, out: &mut String) -> Vec<&'static str> {
    let mut chain = Vec::new();
    for _ in 0..rng.below(4) {
        let plugin = *rng.pick(&EFFECTS);
        writeln!(out, "[[{at}.effect]]\nplugin = \"{plugin}\"\nmix = {}\nparams = {}", num(rng.between(0.2, 1.0)), params_table(rng, plugin, 4)).unwrap();
        chain.push(plugin);
    }
    chain
}

fn modulators(rng: &mut Rng, rack: Rack, at: &str, most: usize, out: &mut String) {
    let count = rng.below(most + 1);
    let kinds: Vec<Kind> = (0..count).map(|_| *rng.pick(&[Kind::Lfo, Kind::Lfo, Kind::Envelope, Kind::Macro, Kind::Steps, Kind::Midi])).collect();
    for index in 0..count {
        modulator(rng, &rack, &kinds, index, at, out);
    }
}

fn pattern(rng: &mut Rng, out: &mut String) {
    let bpm = rng.between(70.0, 160.0);
    let length = 1 + rng.below(4);
    let frames = (length as f64 * 60.0 / bpm * 48_000.0) as u64;
    writeln!(out, "[instrument.pattern]\nbpm = {}\nlength_beats = {length}.0\nenabled = true\nlooping = {}", num(bpm), rng.chance(0.7)).unwrap();
    writeln!(out, "transpose = \"{}\"", if rng.chance(0.5) { "in_key" } else { "chromatic" }).unwrap();
    let base = 48 + rng.below(24) as u8;
    writeln!(out, "base_note = \"{}\"", note_name(base)).unwrap();
    let mut at = 0;
    for _ in 0..(1 + rng.below(4)) {
        let note = base + [0, 3, 4, 7, 10, 12][rng.below(6)];
        let len = frames / 6 + rng.below((frames / 6) as usize + 1) as u64;
        if at + len >= frames {
            break;
        }
        writeln!(out, "[[instrument.pattern.events]]\nframe = {at}\nstatus = \"on\"\nnote = \"{}\"\nvelocity = {}", note_name(note), 40 + rng.below(88)).unwrap();
        writeln!(out, "[[instrument.pattern.events]]\nframe = {}\nstatus = \"off\"\nnote = \"{}\"\nvelocity = 0", at + len, note_name(note)).unwrap();
        at += len + frames / 24;
    }
}

fn lane(rng: &mut Rng, group: Option<usize>, out: &mut String) {
    writeln!(out, "[[instrument]]\nplugin = \"builtin:osc\"\nvolume = {}", num(rng.between(0.3, 1.0))).unwrap();
    if rng.chance(0.25) {
        writeln!(out, "range = \"{}\"", rng.pick(&RANGES)).unwrap();
    }
    if rng.chance(0.25) {
        writeln!(out, "transpose = {}", rng.below(25) as i32 - 12).unwrap();
    }
    if let Some(group) = group {
        writeln!(out, "group = {group}").unwrap();
    }
    // (Rare, since a solo silences every lane but its own, and a session in which the others are never heard is not worth many seeds.)
    if rng.chance(0.06) {
        writeln!(out, "mute = true").unwrap();
    }
    if rng.chance(0.04) {
        writeln!(out, "solo = true").unwrap();
    }
    writeln!(out, "params = {}", params_table(rng, "builtin:osc", 6)).unwrap();
    if rng.chance(0.12) {
        writeln!(out, "pitch_bend_range = 2.0\nremap = {{ \"C4\" = {{ note = \"D4\", detune = {} }}, \"E4\" = {{ note = \"F4\", detune = -0.5 }} }}", num(rng.between(-1.5, 1.5))).unwrap();
    }
    let mut chain = vec!["builtin:osc"];
    chain.extend(effects(rng, "instrument", out));
    modulators(rng, Rack::Lane { chain: &chain }, "instrument", 3, out);
    if rng.chance(0.15) {
        pattern(rng, out);
    }
}

fn group(rng: &mut Rng, members: usize, out: &mut String) {
    writeln!(out, "[[group]]\nvolume = {}\npan = {}", num(rng.between(0.4, 1.0)), num(rng.between(-0.6, 0.6))).unwrap();
    if rng.chance(0.05) {
        writeln!(out, "mute = true").unwrap();
    }
    if rng.chance(0.04) {
        writeln!(out, "solo = true").unwrap();
    }
    let bus = effects(rng, "group", out);
    modulators(rng, Rack::Group { members, bus: &bus }, "group", 2, out);
}

/// The text of the session that `seed` stands for.
pub fn session(seed: u64) -> String {
    let mut rng = Rng::new(seed);
    let mut out = String::new();
    if rng.chance(0.7) {
        writeln!(out, "bpm = {:.1}", rng.between(60.0, 180.0)).unwrap();
    }
    let groups = *rng.pick(&[0, 0, 1, 1, 2]);
    let lanes = 1 + rng.below(4);
    let membership: Vec<Option<usize>> = (0..lanes).map(|_| if groups > 0 && rng.chance(0.6) { Some(rng.below(groups)) } else { None }).collect();
    for g in 0..groups {
        let members = membership.iter().filter(|&&m| m == Some(g)).count();
        group(&mut rng, members, &mut out);
    }
    for &g in &membership {
        lane(&mut rng, g, &mut out);
    }
    out
}

/// The notes and controllers that go with it: `(frame, bytes)` in the order they are played, over `frames`.
pub fn script(seed: u64, frames: u64) -> Vec<(u64, [u8; 3])> {
    // (A seed of its own for the notes, so that the same session can be played again by changing one number.)
    let mut rng = Rng::new(seed ^ 0x5CA1_AB1E);
    let mut events: Vec<(u64, [u8; 3])> = Vec::new();
    // The notes, as (start, end, key, channel), for what is drawn after them.
    let mut notes: Vec<(u64, u64, u8, u8)> = Vec::new();
    for _ in 0..(2 + rng.below(4)) {
        let start = rng.below((frames * 55 / 100) as usize) as u64;
        let end = (start + 2_000 + rng.below((frames / 3) as usize) as u64).min(frames - 1);
        let key = 36 + rng.below(49) as u8;
        let channel = if rng.chance(0.25) { 1 + rng.below(3) as u8 } else { 0 };
        if channel != 0 {
            // A note of an MPE controller: its own wheel, set before it begins, and a pressure that moves while it is held.
            let raw = 8192 + rng.below(4096) as u16 - 2048;
            events.push((start, [0xE0 | channel, (raw & 0x7F) as u8, (raw >> 7) as u8]));
        }
        events.push((start, [0x90 | channel, key, 30 + rng.below(98) as u8]));
        if channel != 0 {
            events.push((start + (end - start) / 2, [0xD0 | channel, 20 + rng.below(100) as u8, 0]));
        }
        events.push((end, [0x80 | channel, key, 0]));
        notes.push((start, end, key, channel));
    }
    for _ in 0..rng.below(6) {
        let frame = rng.below(frames as usize) as u64;
        let controller = *rng.pick(&[1u8, 7, 11, 74]);
        events.push((frame, [0xB0, controller, rng.below(128) as u8]));
    }
    if rng.chance(0.4) {
        let raw = rng.below(16384) as u16;
        events.push((rng.below(frames as usize) as u64, [0xE0, (raw & 0x7F) as u8, (raw >> 7) as u8]));
    }
    if rng.chance(0.3) {
        events.push((rng.below(frames as usize) as u64, [0xA0, 36 + rng.below(49) as u8, rng.below(128) as u8]));
    }
    if rng.chance(0.3) {
        events.push((frames / 10, [0xB0, 64, 127]));
        events.push((frames * 7 / 10, [0xB0, 64, 0]));
    }
    if rng.chance(0.15) {
        events.push((frames * 9 / 10, [0xB0, 123, 0]));
    }
    // (Drawn last, so that the scripts that were made before them are the scripts they were.)
    // A key that is hit again while it sounds, and all sound off.
    if rng.chance(0.3) {
        let (start, end, key, channel) = notes[rng.below(notes.len())];
        events.push((start + (end - start) / 3, [0x90 | channel, key, 20 + rng.below(100) as u8]));
    }
    if rng.chance(0.12) {
        events.push((frames * 8 / 10, [0xB0, 120, 0]));
    }
    events.sort_by_key(|&(frame, _)| frame);
    events
}
