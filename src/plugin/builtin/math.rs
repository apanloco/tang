//! Math: an effect that makes a new sound of two. **A** is what its chain and the cables at its first input give it, **B** is what the cables at its second input bring, or, while
//! nothing is patched there, a carrier of its own (`pitch` and `wave`), so that a module with one cable already sounds like something. Twelve ways to do it (`mode`), each a few
//! lines of arithmetic on the two sounds, sample by sample, channel by channel:
//!
//! | mode | what it makes |
//! |------|---------------|
//! | `ring` | A × B: the sum and the difference of the pitches, and neither pitch |
//! | `am` | A × (1 − `amount` + `amount` × (B + 1) / 2): a tremolo that becomes a ring as `amount` goes up |
//! | `fold` | A + 3 × `amount` × B, folded back at ±1: a wavefolder that B moves |
//! | `warp` | A, read at a delay that B moves (0 to `amount` × 10 ms): vibrato when B is slow, FM when B is a tone |
//! | `compare` | whether A is above B, as +1 or −1 (`amount` is how hard: soft is a difference that is limited): pulse width modulation when A is a wave and B is slow |
//! | `min`, `max` | the lower and the higher of the two |
//! | `xor`, `and`, `or` | the two taken as numbers of `bits` bits, and the logic of their bits: the buzz of a digital circuit |
//! | `hold` | A, held at each place where B goes up through zero (a sample and hold clocked by B; `amount` glides from step to step) |
//! | `follow` | A, turned down by how loud B is (`amount` is how far: a sidechain, a ducker) |
//!
//! The modes that make an offset (`fold`, `compare`, `min`, `max` and the logic) have it taken out by a high-pass at 15 Hz; the others put out exactly what the arithmetic says. Nothing
//! here is band-limited: a ring of two bright sounds, a hard comparison and a logic of bits make partials above the Nyquist frequency that fold down into the band (as the oscillator's
//! waveforms do), which is some of what these sound like. `warp` has no latency (its delay begins at nothing), and nothing allocates once the effect is made.

use std::f32::consts::TAU;

use crate::plugin::{ParameterInfo, Plugin, Preset, SideInput, UsedCondition, UsedWhen};

pub const NAME: &str = "Math";
pub const ID: &str = "builtin:math";
pub const PARAM_COUNT: usize = 8;

/// What the two inputs are called.
const SIDE: SideInput = SideInput { main: "A", side: "B" };

/// The places of the parameters.
const MODE: usize = 0;
const AMOUNT: usize = 1;
const BITS: usize = 2;
const PITCH: usize = 3;
const WAVE: usize = 4;
const ATTACK: usize = 5;
const RELEASE: usize = 6;
const LEVEL: usize = 7;

/// The names of the modes, in the order of the `mode` parameter.
const MODE_NAMES: [&str; 12] = ["ring", "am", "fold", "warp", "compare", "min", "max", "xor", "and", "or", "hold", "follow"];
/// The names of the waves of the carrier, in the order of the `wave` parameter.
const WAVE_NAMES: [&str; 4] = ["sine", "triangle", "saw", "square"];
/// The names of the choices of `bits`: the value is the place in the list, 0 is one bit.
const BITS_NAMES: [&str; 12] = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12"];

struct ParamSpec {
    name: &'static str,
    min: f32,
    max: f32,
    default: f32,
}

const PARAMS: &[ParamSpec] = &[
    ParamSpec { name: "mode",    min: 0.0,   max: 11.0,  default: 0.0 },
    ParamSpec { name: "amount",  min: 0.0,   max: 1.0,   default: 0.5 },
    ParamSpec { name: "bits",    min: 0.0,   max: 11.0,  default: 5.0 },
    ParamSpec { name: "pitch",   min: -48.0, max: 120.0, default: 57.0 },
    ParamSpec { name: "wave",    min: 0.0,   max: 3.0,   default: 0.0 },
    ParamSpec { name: "attack",  min: 0.0005, max: 0.2,  default: 0.005 },
    ParamSpec { name: "release", min: 0.005, max: 1.5,   default: 0.12 },
    ParamSpec { name: "level",   min: 0.0,   max: 1.0,   default: 0.7 },
];

pub const PRESETS: &[(&str, &str, [f32; PARAM_COUNT])] = &[
    // (id, display_name, [mode, amount, bits, pitch, wave, attack, release, level])
    ("ring-bell",  "Ring Bell",  [0.0,  1.0, 5.0,  64.0, 0.0, 0.005, 0.12, 0.70]),
    ("tremolo",    "Tremolo",    [1.0,  0.8, 5.0,  -5.0, 0.0, 0.005, 0.12, 0.90]),
    ("pump",       "Pump",       [11.0, 0.9, 5.0, -36.0, 0.0, 0.005, 0.20, 0.90]),
    ("xor-growl",  "Xor Growl",  [7.0,  0.5, 5.0,  40.0, 2.0, 0.005, 0.12, 0.55]),
    ("square-up",  "Square Up",  [4.0,  0.9, 5.0, -24.0, 1.0, 0.005, 0.12, 0.60]),
    ("fold-sweep", "Fold Sweep", [2.0,  0.6, 5.0, -12.0, 0.0, 0.005, 0.12, 0.70]),
    ("decimator",  "Decimator",  [10.0, 0.1, 5.0, 100.0, 2.0, 0.005, 0.12, 0.70]),
    ("vibrato",    "Vibrato",    [3.0,  0.25, 5.0, -5.0, 0.0, 0.005, 0.12, 0.90]),
    ("warp-metal", "Warp Metal", [3.0,  1.0, 5.0,  60.0, 0.0, 0.005, 0.12, 0.70]),
];

/// How fast `amount`, `level` and `pitch` follow what they are set to, and how fast the sound goes from the carrier to the cable's (and back) when a cable is patched or taken out, in
/// seconds: a step in any of them is a click.
const SMOOTHING_SEC: f32 = 0.005;

/// The longest delay `warp` makes, in seconds, at an `amount` of 1.
const WARP_MAX_SEC: f32 = 0.010;

/// The longest that `hold` glides from one held value to the next, in seconds, at an `amount` of 1.
const HOLD_GLIDE_SEC: f32 = 0.005;

/// How hard `compare` is at its softest and at its hardest: the gain into the `tanh` that makes the comparison, `SOFT_GAIN` for an `amount` of 0 and `SOFT_GAIN * (1 + HARD_BOOST)` at 1.
const SOFT_GAIN: f32 = 2.0;
const HARD_BOOST: f32 = 400.0;

/// The corner of the high-pass that takes the offset out of the modes that make one.
const DC_BLOCK_HZ: f32 = 15.0;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Mode {
    Ring,
    Am,
    Fold,
    Warp,
    Compare,
    Min,
    Max,
    Xor,
    And,
    Or,
    Hold,
    Follow,
}

impl Mode {
    fn from_param(value: f32) -> Self {
        match value.round() as i32 {
            1 => Mode::Am,
            2 => Mode::Fold,
            3 => Mode::Warp,
            4 => Mode::Compare,
            5 => Mode::Min,
            6 => Mode::Max,
            7 => Mode::Xor,
            8 => Mode::And,
            9 => Mode::Or,
            10 => Mode::Hold,
            11 => Mode::Follow,
            _ => Mode::Ring,
        }
    }

    /// Whether the mode makes an offset that is to be taken out.
    fn has_offset(self) -> bool {
        matches!(self, Mode::Fold | Mode::Compare | Mode::Min | Mode::Max | Mode::Xor | Mode::And | Mode::Or)
    }
}

/// A line that is back at the start: `x` folded back at 1 and at −1, as a triangle with a period of 4 (what the drive's fold does).
fn fold(x: f32) -> f32 {
    let t = (x + 1.0).rem_euclid(4.0) - 1.0;
    if t <= 1.0 {
        t
    } else {
        2.0 - t
    }
}

/// How many levels a sample has at `bits` bits, less one: the largest number.
fn top_code(bits: u32) -> u32 {
    (1u32 << bits) - 1
}

/// A sample (−1 to 1) as a number of `bits` bits, from 0 at −1 to the largest at 1 (offset binary).
fn code_of(x: f32, bits: u32) -> u32 {
    let top = top_code(bits);
    ((x.clamp(-1.0, 1.0) + 1.0) * 0.5 * top as f32 + 0.5) as u32
}

/// A number of `bits` bits as a sample, the inverse of `code_of`.
fn sample_of(code: u32, bits: u32) -> f32 {
    code as f32 / top_code(bits) as f32 * 2.0 - 1.0
}

/// What the modes that make a sound of the two samples alone make of `a` and `b` (the others need to remember something), before the offset is taken out and the level is put on:
/// `amount` is 0 to 1 and `bits` is the number of bits of the logic, 1 to 12.
fn combine(mode: Mode, a: f32, b: f32, amount: f32, bits: u32) -> f32 {
    match mode {
        Mode::Ring => a * b,
        Mode::Am => a * (1.0 - amount + amount * (b + 1.0) * 0.5),
        Mode::Fold => fold(a + 3.0 * amount * b),
        Mode::Compare => {
            let gain = SOFT_GAIN * (1.0 + HARD_BOOST * amount.powi(4));
            (gain * (a - b)).tanh()
        }
        Mode::Min => a.min(b),
        Mode::Max => a.max(b),
        Mode::Xor => sample_of(code_of(a, bits) ^ code_of(b, bits), bits),
        Mode::And => sample_of(code_of(a, bits) & code_of(b, bits), bits),
        Mode::Or => sample_of(code_of(a, bits) | code_of(b, bits), bits),
        // (These remember something, and are made by the effect.)
        Mode::Warp | Mode::Hold | Mode::Follow => a,
    }
}

/// The frequency of a note number (69 is A4, 440 Hz), in Hz.
fn hz_of(note: f32) -> f64 {
    440.0 * ((f64::from(note) - 69.0) / 12.0).exp2()
}

/// One sample of the carrier's wave at `phase` (0 to 1): a sine and a triangle that begin at 0 and go up, a saw that begins at −1 and goes up, a square that begins at +1.
fn wave_at(wave: u32, phase: f64) -> f32 {
    let p = phase as f32;
    match wave {
        1 => {
            if p < 0.25 {
                4.0 * p
            } else if p < 0.75 {
                2.0 - 4.0 * p
            } else {
                4.0 * p - 4.0
            }
        }
        2 => 2.0 * p - 1.0,
        3 => {
            if p < 0.5 {
                1.0
            } else {
                -1.0
            }
        }
        _ => (TAU * p).sin(),
    }
}

/// When a parameter has an effect: `amount` in the modes that read it, `bits` in the logic modes, `attack` and `release` in `follow`, and `pitch` and `wave` while nothing is patched at B, which is
/// when the carrier is what B is. The others, `mode` and `level`, always do. What `render` reads, in words: `a_parameter_that_says_it_has_no_effect_has_none_and_one_that_says_it_has_one_has_one`
/// holds this to it, state by state.
fn used_when(index: usize) -> Option<UsedWhen> {
    let in_modes = |places: &[usize]| UsedWhen {
        condition: UsedCondition::Choice { param: MODE as u32, values: places.iter().map(|&place| place as u32).collect() },
        why: format!("only in the {} {}", if places.len() == 1 { "mode" } else { "modes" }, places.iter().map(|&place| MODE_NAMES[place]).collect::<Vec<_>>().join(", ")),
    };
    match index {
        AMOUNT => Some(in_modes(&[1, 2, 3, 4, 10, 11])),
        BITS => Some(in_modes(&[7, 8, 9])),
        ATTACK | RELEASE => Some(in_modes(&[11])),
        PITCH | WAVE => Some(UsedWhen { condition: UsedCondition::SideUnpatched, why: format!("only while nothing is patched at {}", SIDE.side) }),
        _ => None,
    }
}

/// A sample that is a number: what is not (a plugin before it put out a NaN) would stay in the line and the filters for ever.
#[inline]
fn finite(x: f32) -> f32 {
    if x.is_finite() {
        x
    } else {
        0.0
    }
}

/// What each side of the sound has of its own: the high-pass that takes the offset out, what `hold` holds and where its glide is, and where B was the sample before (for the
/// place where it goes up through zero).
#[derive(Default)]
struct Side {
    dc_in: f32,
    dc_out: f32,
    held: f32,
    glided: f32,
    last_b: f32,
}

pub struct Math {
    sample_rate: f32,
    mode: f32,
    amount: f32,
    bits: f32,
    pitch: f32,
    wave: f32,
    attack: f32,
    release: f32,
    level: f32,
    /// Whether it has run since it was made or given a preset: it starts at what is set and does not glide to it from where it was.
    started: bool,
    /// `amount`, `level` and `pitch` as they are now: they follow what they are set to (see `SMOOTHING_SEC`).
    amount_now: f32,
    level_now: f32,
    pitch_now: f32,
    /// How much of B is the cables' now, 0 (the carrier) to 1: it follows whether a cable is patched, so that patching one or taking it out is not a click.
    patched_now: f32,
    /// Where the carrier is in its wave, 0 to 1. It goes on whether it is heard or not, so that it is where it should be when it is.
    phase: f64,
    /// How loud B has been lately, for `follow`.
    envelope: f32,
    sides: [Side; 2],
    /// The last sounds that went in, for `warp`: a line for each channel, as long as the longest delay (a power of two), and where the newest is.
    line: [Vec<f32>; 2],
    at: usize,
}

impl Math {
    pub fn new(sample_rate: f32) -> Self {
        let frames = (WARP_MAX_SEC * sample_rate).ceil() as usize + 4;
        let size = frames.next_power_of_two().max(8);
        Self {
            sample_rate,
            mode: PARAMS[MODE].default,
            amount: PARAMS[AMOUNT].default,
            bits: PARAMS[BITS].default,
            pitch: PARAMS[PITCH].default,
            wave: PARAMS[WAVE].default,
            attack: PARAMS[ATTACK].default,
            release: PARAMS[RELEASE].default,
            level: PARAMS[LEVEL].default,
            started: false,
            amount_now: PARAMS[AMOUNT].default,
            level_now: PARAMS[LEVEL].default,
            pitch_now: PARAMS[PITCH].default,
            patched_now: 0.0,
            phase: 0.0,
            envelope: 0.0,
            sides: [Side::default(), Side::default()],
            line: [vec![0.0; size], vec![0.0; size]],
            at: 0,
        }
    }

    /// One block: `audio_in` is A, `side` is B if a cable is patched to it, and the carrier's sound if it is not.
    fn render(&mut self, audio_in: &[&[f32]], side: Option<&[&[f32]]>, audio_out: &mut [&mut [f32]]) {
        let frames = audio_out.first().map_or(0, |out| out.len());
        let patched = if side.is_some() { 1.0 } else { 0.0 };
        if !self.started {
            self.amount_now = self.amount;
            self.level_now = self.level;
            self.pitch_now = self.pitch;
            self.patched_now = patched;
            self.started = true;
        }
        let mode = Mode::from_param(self.mode);
        let bits = self.bits.round().clamp(0.0, 11.0) as u32 + 1;
        let wave = self.wave.round().clamp(0.0, 3.0) as u32;
        let smoothing = 1.0 - (-1.0 / (SMOOTHING_SEC * self.sample_rate)).exp();
        let dc_pole = (-TAU * DC_BLOCK_HZ / self.sample_rate).exp();
        let attack = 1.0 - (-1.0 / (self.attack.max(1e-5) * self.sample_rate)).exp();
        let release = 1.0 - (-1.0 / (self.release.max(1e-5) * self.sample_rate)).exp();
        let mask = self.line[0].len() - 1;
        let warp_max = WARP_MAX_SEC * self.sample_rate;
        let glide_frames = HOLD_GLIDE_SEC * self.sample_rate;
        for frame in 0..frames {
            self.amount_now += (self.amount - self.amount_now) * smoothing;
            self.level_now += (self.level - self.level_now) * smoothing;
            self.pitch_now += (self.pitch - self.pitch_now) * smoothing;
            self.patched_now += (patched - self.patched_now) * smoothing;
            let carrier = wave_at(wave, self.phase);
            self.phase += hz_of(self.pitch_now) / f64::from(self.sample_rate);
            self.phase -= self.phase.floor();
            // B on each side: the cable's sound and the carrier, as much of each as `patched_now` says (a cable that has one channel is on both sides).
            let mut b = [0.0f32; 2];
            for (channel, b) in b.iter_mut().enumerate() {
                let cable = side.and_then(|side| side.get(channel).or(side.first())).and_then(|buffer| buffer.get(frame)).copied().map_or(0.0, finite);
                *b = cable * self.patched_now + carrier * (1.0 - self.patched_now);
            }
            if mode == Mode::Follow {
                let loudness = b[0].abs().max(b[1].abs()).min(4.0);
                let coefficient = if loudness > self.envelope { attack } else { release };
                self.envelope += (loudness - self.envelope) * coefficient;
            }
            for (channel, out) in audio_out.iter_mut().enumerate().take(2) {
                let a = audio_in.get(channel).or(audio_in.first()).and_then(|input| input.get(frame)).copied().map_or(0.0, finite);
                let state = &mut self.sides[channel];
                let mut y = match mode {
                    Mode::Warp => {
                        self.line[channel][self.at] = a;
                        let delay = (self.amount_now * warp_max * (b[channel].clamp(-1.0, 1.0) + 1.0) * 0.5).clamp(0.0, warp_max);
                        let whole = delay.floor();
                        let fraction = delay - whole;
                        let newer = (self.at + mask + 1 - whole as usize) & mask;
                        let older = (newer + mask) & mask;
                        self.line[channel][newer] * (1.0 - fraction) + self.line[channel][older] * fraction
                    }
                    Mode::Hold => {
                        if state.last_b < 0.0 && b[channel] >= 0.0 {
                            state.held = a;
                        }
                        state.last_b = b[channel];
                        let glide = self.amount_now * glide_frames;
                        if glide < 1.0 {
                            state.glided = state.held;
                        } else {
                            state.glided += (state.held - state.glided) / glide;
                        }
                        state.glided
                    }
                    Mode::Follow => a * (1.0 - self.amount_now * self.envelope.min(1.0)),
                    _ => combine(mode, a, b[channel], self.amount_now, bits),
                };
                // (`warp` writes the line for a side that it is not in as well, below, so that it is full when the mode is chosen.)
                if mode != Mode::Warp {
                    self.line[channel][self.at] = a;
                }
                if mode.has_offset() {
                    let blocked = y - state.dc_in + dc_pole * state.dc_out;
                    state.dc_in = y;
                    state.dc_out = blocked;
                    y = blocked;
                }
                out[frame] = y * self.level_now;
            }
            self.at = (self.at + 1) & mask;
        }
    }
}

impl Plugin for Math {
    fn name(&self) -> &str {
        NAME
    }

    fn is_instrument(&self) -> bool {
        false
    }

    fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    fn audio_input_count(&self) -> usize {
        2
    }

    fn audio_output_count(&self) -> usize {
        2
    }

    fn side_input(&self) -> Option<SideInput> {
        Some(SIDE)
    }

    fn process(&mut self, _midi_events: &[(u64, [u8; 3])], audio_in: &[&[f32]], audio_out: &mut [&mut [f32]]) -> anyhow::Result<()> {
        self.render(audio_in, None, audio_out);
        Ok(())
    }

    fn process_side(&mut self, _midi_events: &[(u64, [u8; 3])], audio_in: &[&[f32]], side: Option<&[&[f32]]>, audio_out: &mut [&mut [f32]]) -> anyhow::Result<()> {
        self.render(audio_in, side, audio_out);
        Ok(())
    }

    fn parameters(&self) -> Vec<ParameterInfo> {
        PARAMS
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let labels: Option<&[&str]> = match i {
                    MODE => Some(&MODE_NAMES),
                    BITS => Some(&BITS_NAMES),
                    WAVE => Some(&WAVE_NAMES),
                    _ => None,
                };
                ParameterInfo {
                    index: i as u32,
                    name: p.name.into(),
                    min: p.min,
                    max: p.max,
                    default: p.default,
                    labels: labels.map(|names| names.iter().map(|name| name.to_string()).collect()),
                    unit: matches!(i, ATTACK | RELEASE).then(|| "s".to_string()),
                    group: match i {
                        PITCH | WAVE => vec!["Carrier".to_string()],
                        BITS => vec!["Logic".to_string()],
                        ATTACK | RELEASE => vec!["Follower".to_string()],
                        _ => Vec::new(),
                    },
                    step_count: labels.map_or(0, |names| (names.len() - 1) as u32),
                    used_when: used_when(i),
                }
            })
            .collect()
    }

    fn get_parameter(&mut self, index: u32) -> Option<f32> {
        match index as usize {
            MODE => Some(self.mode),
            AMOUNT => Some(self.amount),
            BITS => Some(self.bits),
            PITCH => Some(self.pitch),
            WAVE => Some(self.wave),
            ATTACK => Some(self.attack),
            RELEASE => Some(self.release),
            LEVEL => Some(self.level),
            _ => None,
        }
    }

    fn set_parameter(&mut self, index: u32, value: f32) -> anyhow::Result<()> {
        let spec = PARAMS.get(index as usize).ok_or_else(|| anyhow::anyhow!("no parameter with index {index}"))?;
        anyhow::ensure!(value.is_finite(), "{} is not a number: {value}", spec.name);
        let v = value.clamp(spec.min, spec.max);
        match index as usize {
            MODE => self.mode = v.round(),
            AMOUNT => self.amount = v,
            BITS => self.bits = v.round(),
            PITCH => self.pitch = v,
            WAVE => self.wave = v.round(),
            ATTACK => self.attack = v,
            RELEASE => self.release = v,
            LEVEL => self.level = v,
            _ => unreachable!(),
        }
        Ok(())
    }

    fn presets(&self) -> Vec<Preset> {
        PRESETS.iter().map(|(id, name, _)| Preset { name: (*name).into(), id: (*id).into() }).collect()
    }

    fn load_preset(&mut self, id: &str) -> anyhow::Result<()> {
        let (_, _, values) = PRESETS.iter().find(|(pid, _, _)| *pid == id).ok_or_else(|| anyhow::anyhow!("no preset with id {id:?}"))?;
        [self.mode, self.amount, self.bits, self.pitch, self.wave, self.attack, self.release, self.level] = *values;
        // A fresh sound: the settings are the preset's from the first sample.
        self.started = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RATE: f32 = 48_000.0;

    /// A math effect with the parameters set by name.
    fn math_with(settings: &[(usize, f32)]) -> Math {
        let mut math = Math::new(SAMPLE_RATE);
        for &(index, value) in settings {
            math.set_parameter(index as u32, value).unwrap();
        }
        math
    }

    /// `frames` of a sine of `hz` and `amplitude`.
    fn sine(hz: f32, amplitude: f32, frames: usize) -> Vec<f32> {
        (0..frames).map(|n| amplitude * (TAU * hz * n as f32 / SAMPLE_RATE).sin()).collect()
    }

    /// What the effect puts out on the left for `a` on both sides and `b` on both sides (none: nothing is patched), in blocks of 256 frames, as the audio thread has them.
    fn run(math: &mut Math, a: &[f32], b: Option<&[f32]>) -> Vec<f32> {
        run_stereo(math, a, a, b, b).0
    }

    fn run_stereo(math: &mut Math, a_left: &[f32], a_right: &[f32], b_left: Option<&[f32]>, b_right: Option<&[f32]>) -> (Vec<f32>, Vec<f32>) {
        let frames = a_left.len();
        let (mut out_l, mut out_r) = (Vec::with_capacity(frames), Vec::with_capacity(frames));
        let mut at = 0;
        while at < frames {
            let end = (at + 256).min(frames);
            let (mut block_l, mut block_r) = (vec![0.0f32; end - at], vec![0.0f32; end - at]);
            let side: Option<Vec<&[f32]>> = match (b_left, b_right) {
                (Some(l), Some(r)) => Some(vec![&l[at..end], &r[at..end]]),
                _ => None,
            };
            math.process_side(&[], &[&a_left[at..end], &a_right[at..end]], side.as_deref(), &mut [&mut block_l[..], &mut block_r[..]]).unwrap();
            out_l.extend_from_slice(&block_l);
            out_r.extend_from_slice(&block_r);
            at = end;
        }
        (out_l, out_r)
    }

    /// The magnitude of the part of `signal` that is a sine of `hz` (Goertzel): the amplitude of that sine if there is one.
    fn magnitude(signal: &[f32], hz: f32) -> f32 {
        let w = TAU * hz / SAMPLE_RATE;
        let (mut s1, mut s2) = (0.0f64, 0.0f64);
        for &x in signal {
            let s0 = f64::from(x) + 2.0 * f64::from(w).cos() * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        let power = s1 * s1 + s2 * s2 - 2.0 * f64::from(w).cos() * s1 * s2;
        (power.sqrt() * 2.0 / signal.len() as f64) as f32
    }

    #[test]
    fn it_is_an_effect_with_two_inputs_and_a_side_input() {
        let m = Math::new(SAMPLE_RATE);
        assert_eq!((m.name(), m.is_instrument(), m.audio_input_count(), m.audio_output_count(), m.latency()), ("Math", false, 2, 2, 0));
        assert_eq!(m.side_input(), Some(SideInput { main: "A", side: "B" }));
        assert_eq!(m.parameters().len(), PARAM_COUNT);
    }

    #[test]
    fn its_parameters_say_their_names_ranges_labels_and_steps() {
        let m = Math::new(SAMPLE_RATE);
        let params = m.parameters();
        let names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["mode", "amount", "bits", "pitch", "wave", "attack", "release", "level"]);
        for p in &params {
            assert!(p.min <= p.default && p.default <= p.max, "{}: the default is in the range", p.name);
        }
        assert_eq!(params[MODE].labels.as_ref().unwrap().len(), 12);
        assert_eq!((params[MODE].min, params[MODE].max, params[MODE].step_count), (0.0, 11.0, 11));
        assert_eq!(params[BITS].labels.as_ref().unwrap().first().map(String::as_str), Some("1"));
        assert_eq!((params[BITS].min, params[BITS].max, params[BITS].step_count), (0.0, 11.0, 11));
        assert_eq!(params[WAVE].labels.as_ref().unwrap(), &["sine", "triangle", "saw", "square"]);
        assert!(params[AMOUNT].labels.is_none() && params[PITCH].labels.is_none());
    }

    #[test]
    fn a_ring_of_two_sines_has_their_sum_and_their_difference_and_neither_of_them() {
        let a = sine(1000.0, 1.0, 48_000);
        let b = sine(300.0, 1.0, 48_000);
        let out = run(&mut math_with(&[(MODE, 0.0), (LEVEL, 1.0)]), &a, Some(&b));
        // sin(x) sin(y) = (cos(x - y) - cos(x + y)) / 2: two partials of a half.
        assert!((magnitude(&out, 700.0) - 0.5).abs() < 0.01, "the difference: {}", magnitude(&out, 700.0));
        assert!((magnitude(&out, 1300.0) - 0.5).abs() < 0.01, "the sum: {}", magnitude(&out, 1300.0));
        assert!(magnitude(&out, 1000.0) < 0.01 && magnitude(&out, 300.0) < 0.01, "neither of the two");
    }

    #[test]
    fn a_ring_is_exactly_the_product_and_the_level_is_a_gain_on_it() {
        let a = sine(440.0, 0.8, 2048);
        let b = sine(660.0, 0.6, 2048);
        let out = run(&mut math_with(&[(MODE, 0.0), (LEVEL, 0.5)]), &a, Some(&b));
        for n in 0..a.len() {
            assert!((out[n] - a[n] * b[n] * 0.5).abs() < 1e-6, "frame {n}");
        }
    }

    #[test]
    fn with_no_cable_at_b_the_carrier_is_b_and_it_has_the_pitch_that_is_set() {
        // A constant 1, a ring: what comes out is the carrier itself, a sine of A4 at a note number of 69.
        let a = vec![1.0f32; 48_000];
        let out = run(&mut math_with(&[(MODE, 0.0), (PITCH, 69.0), (WAVE, 0.0), (LEVEL, 1.0)]), &a, None);
        assert!((magnitude(&out, 440.0) - 1.0).abs() < 0.01, "{}", magnitude(&out, 440.0));
        assert!(magnitude(&out, 880.0) < 0.01);
        // An octave up is twice the frequency.
        let out = run(&mut math_with(&[(MODE, 0.0), (PITCH, 81.0), (LEVEL, 1.0)]), &a, None);
        assert!((magnitude(&out, 880.0) - 1.0).abs() < 0.01 && magnitude(&out, 440.0) < 0.01);
    }

    #[test]
    fn the_carriers_four_waves_are_what_they_are_named_for() {
        let a = vec![1.0f32; 48_000];
        let wave = |wave: f32| run(&mut math_with(&[(MODE, 0.0), (PITCH, 69.0), (WAVE, wave), (LEVEL, 1.0)]), &a, None);
        // A triangle has the odd partials only, falling by the square of the number: the third is a ninth of the first.
        let triangle = wave(1.0);
        assert!((magnitude(&triangle, 440.0) - 8.0 / (std::f32::consts::PI * std::f32::consts::PI)).abs() < 0.01);
        assert!((magnitude(&triangle, 1320.0) / magnitude(&triangle, 440.0) - 1.0 / 9.0).abs() < 0.01);
        // A saw has every partial, falling by their number; a square has the odd ones, falling by their number.
        let saw = wave(2.0);
        assert!((magnitude(&saw, 880.0) / magnitude(&saw, 440.0) - 0.5).abs() < 0.02, "{}", magnitude(&saw, 880.0) / magnitude(&saw, 440.0));
        let square = wave(3.0);
        assert!(magnitude(&square, 880.0) < 0.01);
        assert!((magnitude(&square, 1320.0) / magnitude(&square, 440.0) - 1.0 / 3.0).abs() < 0.02);
        // They all stay within -1 to 1, and the saw and the square begin where they say.
        for (name, signal) in [("triangle", &triangle), ("saw", &saw), ("square", &square)] {
            assert!(signal.iter().all(|x| x.abs() <= 1.0 + 1e-6), "{name}");
        }
        assert!(saw[0] < -0.99 && square[0] > 0.99);
    }

    #[test]
    fn a_cable_at_b_is_used_and_the_carrier_is_not_heard() {
        // A constant 1 and a cable of a sine of 300 Hz: the sound is the cable's, and there is nothing of the carrier's A4.
        let a = vec![1.0f32; 48_000];
        let b = sine(300.0, 1.0, 48_000);
        let out = run(&mut math_with(&[(MODE, 0.0), (PITCH, 69.0), (LEVEL, 1.0)]), &a, Some(&b));
        assert!((magnitude(&out, 300.0) - 1.0).abs() < 0.01);
        assert!(magnitude(&out, 440.0) < 0.01, "{}", magnitude(&out, 440.0));
    }

    #[test]
    fn a_cable_that_has_one_channel_is_b_on_both_sides() {
        let a = sine(500.0, 1.0, 1024);
        let b = sine(200.0, 1.0, 1024);
        let mut m = math_with(&[(MODE, 0.0), (LEVEL, 1.0)]);
        let (mut left, mut right) = (vec![0.0f32; 1024], vec![0.0f32; 1024]);
        m.process_side(&[], &[&a, &a], Some(&[&b]), &mut [&mut left[..], &mut right[..]]).unwrap();
        assert_eq!(left, right);
        assert!((left[100] - a[100] * b[100]).abs() < 1e-6);
    }

    #[test]
    fn each_side_has_its_own_a_and_its_own_b() {
        let a_left = sine(500.0, 1.0, 1024);
        let a_right = sine(700.0, 0.5, 1024);
        let b_left = sine(200.0, 1.0, 1024);
        let b_right = sine(300.0, 0.25, 1024);
        let (left, right) = run_stereo(&mut math_with(&[(MODE, 0.0), (LEVEL, 1.0)]), &a_left, &a_right, Some(&b_left), Some(&b_right));
        for n in 0..1024 {
            assert!((left[n] - a_left[n] * b_left[n]).abs() < 1e-6 && (right[n] - a_right[n] * b_right[n]).abs() < 1e-6, "frame {n}");
        }
    }

    #[test]
    fn an_am_is_the_sound_at_nothing_and_a_ring_with_an_offset_at_the_top() {
        let a = sine(440.0, 0.8, 1024);
        let b = sine(5.0, 1.0, 1024);
        // At 0 it is A as it is; at 1 it is A times B moved to 0..1 (a half of 1 + B); in between, a blend of the two gains.
        let out = run(&mut math_with(&[(MODE, 1.0), (AMOUNT, 0.0), (LEVEL, 1.0)]), &a, Some(&b));
        for n in 0..a.len() {
            assert!((out[n] - a[n]).abs() < 1e-6, "{n}");
        }
        let out = run(&mut math_with(&[(MODE, 1.0), (AMOUNT, 1.0), (LEVEL, 1.0)]), &a, Some(&b));
        for n in 0..a.len() {
            assert!((out[n] - a[n] * (1.0 + b[n]) * 0.5).abs() < 1e-6, "{n}");
        }
        let out = run(&mut math_with(&[(MODE, 1.0), (AMOUNT, 0.5), (LEVEL, 1.0)]), &a, Some(&b));
        for n in 0..a.len() {
            assert!((out[n] - a[n] * (0.5 + 0.25 * (1.0 + b[n]))).abs() < 1e-6, "{n}");
        }
    }

    #[test]
    fn the_stateless_modes_are_the_arithmetic_that_the_table_says() {
        // fold: a line that goes up to 1 and comes back, and again at -1.
        assert_eq!(combine(Mode::Fold, 0.5, 0.0, 1.0, 8), 0.5, "it is the line inside ±1");
        assert!(combine(Mode::Fold, 0.5, 0.5, 1.0, 8).abs() < 1e-6, "0.5 + 1.5 = 2 is folded to 0");
        assert!((combine(Mode::Fold, 0.0, 1.0, 0.5, 8) - 0.5).abs() < 1e-6, "0 + 3 * 0.5 = 1.5 is folded to 0.5");
        assert!((combine(Mode::Fold, 0.0, 1.0, 1.0, 8) - (-1.0)).abs() < 1e-6, "0 + 3 = 3 is folded to -1");
        // min and max.
        assert_eq!((combine(Mode::Min, 0.25, -0.5, 0.0, 8), combine(Mode::Max, 0.25, -0.5, 0.0, 8)), (-0.5, 0.25));
        // compare: whether A is above B, as +1 or -1, hard at the top and a limited difference at the bottom.
        assert!(combine(Mode::Compare, 0.2, 0.1, 1.0, 8) > 0.999 && combine(Mode::Compare, 0.1, 0.2, 1.0, 8) < -0.999);
        assert!((combine(Mode::Compare, 0.02, 0.0, 0.0, 8) - (0.04f32).tanh()).abs() < 1e-6, "soft: tanh of twice the difference");
        assert!((combine(Mode::Compare, 0.01, 0.0, 0.5, 8) - (0.52f32).tanh()).abs() < 1e-6, "half of the knob is its fourth power, so that the gain is 2 * (1 + 400 / 16) = 52 and not 2 * (1 + 400 / 4)");
        // The logic, at 8 bits: 0.0 is the code 128 (of 255) and 0.5 is 191.
        let (a, b) = (0.0, 0.5);
        assert_eq!(code_of(a, 8), 128);
        assert_eq!(code_of(b, 8), 191);
        assert_eq!(combine(Mode::Xor, a, b, 0.0, 8), sample_of(128 ^ 191, 8));
        assert_eq!(combine(Mode::And, a, b, 0.0, 8), sample_of(128 & 191, 8));
        assert_eq!(combine(Mode::Or, a, b, 0.0, 8), sample_of(128 | 191, 8));
        // The ends are the ends, at any number of bits: -1 is 0 and 1 is the largest.
        for bits in 1..=12u32 {
            assert_eq!(code_of(-1.0, bits), 0);
            assert_eq!(code_of(1.0, bits), top_code(bits));
            assert_eq!(sample_of(0, bits), -1.0);
            assert_eq!(sample_of(top_code(bits), bits), 1.0);
        }
        // At one bit a sample is a bit: the sign.
        assert_eq!(combine(Mode::Xor, 0.3, -0.3, 0.0, 1), 1.0);
        assert_eq!(combine(Mode::Xor, 0.3, 0.3, 0.0, 1), -1.0);
    }

    #[test]
    fn a_warp_delays_a_by_what_b_says_from_nothing_to_ten_milliseconds() {
        // An impulse in A. B at -1 is no delay at all, and B at +1 at an amount of 1 is ten milliseconds, 480 frames.
        let mut a = vec![0.0f32; 2048];
        a[100] = 1.0;
        for (b, delay) in [(-1.0f32, 0usize), (1.0, 480)] {
            let b = vec![b; a.len()];
            let out = run(&mut math_with(&[(MODE, 3.0), (AMOUNT, 1.0), (LEVEL, 1.0)]), &a, Some(&b));
            let peak = out.iter().enumerate().max_by(|x, y| x.1.abs().total_cmp(&y.1.abs())).unwrap().0;
            assert_eq!(peak, 100 + delay, "B at {}", b[0]);
            assert!((out[peak] - 1.0).abs() < 1e-6);
        }
        // B at 0 is half of it, and the amount is how much of the ten milliseconds.
        let b = vec![0.0f32; a.len()];
        let out = run(&mut math_with(&[(MODE, 3.0), (AMOUNT, 0.5), (LEVEL, 1.0)]), &a, Some(&b));
        assert!((out[100 + 120] - 1.0).abs() < 1e-6, "a quarter of 480 frames");
    }

    #[test]
    fn a_warp_between_two_frames_is_a_blend_of_the_two() {
        let mut a = vec![0.0f32; 1024];
        a[100] = 1.0;
        // The delay is 480 * (B + 1) / 2 at an amount of 1: B + 1 = 1 / 480 is half a frame, and it is read as half of the frame it is at and half of the one before.
        let b = vec![-1.0 + 1.0 / 480.0; a.len()];
        let out = run(&mut math_with(&[(MODE, 3.0), (AMOUNT, 1.0), (LEVEL, 1.0)]), &a, Some(&b));
        assert!((out[100] - 0.5).abs() < 1e-4 && (out[101] - 0.5).abs() < 1e-4, "{} {}", out[100], out[101]);
    }

    #[test]
    fn a_hold_keeps_what_a_was_where_b_went_up_through_zero() {
        // B is a square that goes up through zero every 100 frames; A is a ramp, its frame number.
        let a: Vec<f32> = (0..1000).map(|n| n as f32 / 1000.0).collect();
        let b: Vec<f32> = (0..1000).map(|n| if (n / 50) % 2 == 0 { -1.0 } else { 1.0 }).collect();
        let out = run(&mut math_with(&[(MODE, 10.0), (AMOUNT, 0.0), (LEVEL, 1.0)]), &a, Some(&b));
        // It goes up at frames 50, 150, 250 ...: from there A at that frame is held until the next.
        assert_eq!(out[10], 0.0, "nothing has been held before the first place");
        for place in [50usize, 150, 250, 850] {
            for (n, sample) in out.iter().enumerate().skip(place).take(100) {
                assert_eq!(*sample, a[place], "frame {n}");
            }
        }
    }

    #[test]
    fn a_hold_glides_from_one_value_to_the_next_by_the_amount() {
        let a: Vec<f32> = (0..2000).map(|n| if n < 100 { 0.0 } else { 1.0 }).collect();
        let b: Vec<f32> = (0..2000).map(|n| if (n / 100) % 2 == 0 { -1.0 } else { 1.0 }).collect();
        let hard = run(&mut math_with(&[(MODE, 10.0), (AMOUNT, 0.0), (LEVEL, 1.0)]), &a, Some(&b));
        let soft = run(&mut math_with(&[(MODE, 10.0), (AMOUNT, 1.0), (LEVEL, 1.0)]), &a, Some(&b));
        // B first goes up at frame 100, where A is 1: hard jumps to 1 at once, and soft takes its five milliseconds (240 frames) to go most of the way.
        assert_eq!(hard[100], 1.0);
        assert!(soft[100] < 0.1 && soft[100 + 240] > 0.5 && soft[100 + 240] < 0.8, "{} {}", soft[100], soft[340]);
        assert!(soft[199] > soft[150] && soft[150] > soft[110], "and rising");
    }

    #[test]
    fn a_follow_turns_a_down_by_how_loud_b_has_been() {
        let a = vec![0.5f32; 48_000];
        let quiet = vec![0.0f32; 48_000];
        let loud = vec![1.0f32; 48_000];
        let out = run(&mut math_with(&[(MODE, 11.0), (AMOUNT, 1.0), (LEVEL, 1.0)]), &a, Some(&quiet));
        assert!(out.iter().all(|x| (*x - 0.5).abs() < 1e-6), "a B that is silent turns nothing down");
        let out = run(&mut math_with(&[(MODE, 11.0), (AMOUNT, 1.0), (LEVEL, 1.0)]), &a, Some(&loud));
        assert!(out[47_999].abs() < 1e-3, "a B that is at 1 for a second turns A down to nothing: {}", out[47_999]);
        assert!(out[0] > 0.3, "and not at once: it takes the attack: {}", out[0]);
        let out = run(&mut math_with(&[(MODE, 11.0), (AMOUNT, 0.5), (LEVEL, 1.0)]), &a, Some(&loud));
        assert!((out[47_999] - 0.25).abs() < 1e-3, "an amount of a half turns it down by a half: {}", out[47_999]);
    }

    #[test]
    fn a_follow_turns_a_down_to_nothing_and_not_past_it_for_a_b_that_is_louder_than_full_scale() {
        // A cable can be at a level of 2, so B can be: the follower is held to 1 where it turns A down, and A is not turned over.
        let a = vec![0.5f32; 48_000];
        let louder = vec![3.0f32; 48_000];
        let out = run(&mut math_with(&[(MODE, 11.0), (AMOUNT, 1.0), (LEVEL, 1.0)]), &a, Some(&louder));
        assert!(out.iter().all(|x| *x >= 0.0), "A is never turned over: the lowest is {}", out.iter().copied().fold(f32::MAX, f32::min));
        assert!(out[47_999].abs() < 1e-3, "and it is turned down to nothing: {}", out[47_999]);
    }

    #[test]
    fn a_follow_hears_the_louder_of_the_two_sides_of_b_and_ducks_both_sides_of_a() {
        let a = vec![0.5f32; 48_000];
        let (quiet, loud) = (vec![0.0f32; 48_000], vec![1.0f32; 48_000]);
        for (left, right) in [(&quiet, &loud), (&loud, &quiet)] {
            let (out_l, out_r) = run_stereo(&mut math_with(&[(MODE, 11.0), (AMOUNT, 1.0), (LEVEL, 1.0)]), &a, &a, Some(left), Some(right));
            assert!(out_l[47_999].abs() < 1e-3 && out_r[47_999].abs() < 1e-3, "B is loud on one side only: {} and {}", out_l[47_999], out_r[47_999]);
        }
    }

    #[test]
    fn a_follow_lets_go_over_the_release_when_b_goes_quiet() {
        let a = vec![1.0f32; 24_000];
        let mut b = vec![1.0f32; 12_000];
        b.extend(vec![0.0f32; 12_000]);
        let out = run(&mut math_with(&[(MODE, 11.0), (AMOUNT, 1.0), (ATTACK, 0.001), (RELEASE, 0.1), (LEVEL, 1.0)]), &a, Some(&b));
        assert!(out[11_999] < 0.01, "ducked");
        // After a release of 0.1 s (4800 frames) the follower has fallen to 1/e of where it was, so A is back to 1 - 1/e.
        assert!((out[12_000 + 4800] - (1.0 - (-1.0f32).exp())).abs() < 0.02, "{}", out[16_800]);
        // (Twelve thousand frames more, which is two and a half releases: it has fallen to 8% of where it was.)
        assert!(out[23_999] > 0.9, "and in the end it is nearly all the way back: {}", out[23_999]);
    }

    #[test]
    fn the_modes_that_make_an_offset_have_it_taken_out() {
        // A comparison of a sine that is mostly above B has a mean that is not 0: the high-pass takes it out. Without it, the mean is the one of the comparison.
        let a = sine(220.0, 1.0, 48_000);
        let b = vec![-0.5f32; 48_000];
        let out = run(&mut math_with(&[(MODE, 4.0), (AMOUNT, 1.0), (LEVEL, 1.0)]), &a, Some(&b));
        let mean = out[24_000..].iter().sum::<f32>() / 24_000.0;
        assert!(mean.abs() < 0.01, "the mean is {mean}");
        let raw_mean = a.iter().map(|&x| combine(Mode::Compare, x, -0.5, 1.0, 6)).sum::<f32>() / 48_000.0;
        assert!(raw_mean > 0.3, "the comparison's own mean is {raw_mean}");
        // A ring has no offset to take out and puts out the product itself (the test above: exact to a millionth).
        assert!(!Mode::Ring.has_offset() && !Mode::Am.has_offset() && !Mode::Warp.has_offset() && !Mode::Hold.has_offset() && !Mode::Follow.has_offset());
        assert!(Mode::Fold.has_offset() && Mode::Compare.has_offset() && Mode::Min.has_offset() && Mode::Max.has_offset() && Mode::Xor.has_offset() && Mode::And.has_offset() && Mode::Or.has_offset());
    }

    #[test]
    fn a_cable_that_is_patched_or_taken_out_is_not_a_click() {
        // A is a constant half and the mode is a ring, so the sound is B scaled: with a cable of silence it is none, and with none patched it is the carrier. Going from one to the other
        // glides over a few milliseconds.
        let a = vec![0.5f32; 4096];
        let silence = vec![0.0f32; 4096];
        let mut m = math_with(&[(MODE, 0.0), (PITCH, 57.0), (LEVEL, 1.0)]);
        let patched = run(&mut m, &a, Some(&silence));
        assert!(patched.iter().all(|x| *x == 0.0));
        let after = run(&mut m, &a, None);
        // (The step from the last frame of the block before to the first of this one is one of them: it is where a gain that jumps does it.)
        let steps = |before: &[f32], block: &[f32]| before.iter().chain(block).copied().collect::<Vec<f32>>().windows(2).map(|pair| (pair[1] - pair[0]).abs()).fold(0.0f32, f32::max);
        // The carrier's own steepest slope is 0.5 * 2 pi * 220 / 48000 = 0.0144 a frame; a gain that went from 0 to 1 at once would jump by half the carrier, 0.49 where it is (a sine at
        // 0.77 of its cycle), at the first frame.
        let biggest_step = steps(&patched[4095..], &after);
        assert!(biggest_step < 0.03, "{biggest_step}");
        assert!(after[4000].abs() > 0.0 || after[4001].abs() > 0.0, "and it gets there");
        // The other way: the sound goes from the carrier's to the cable's, here a constant 1.
        let ones = vec![1.0f32; 4096];
        let into = run(&mut m, &a, Some(&ones));
        assert!((into[4095] - 0.5).abs() < 0.001, "the end of the glide is the cable's: {}", into[4095]);
        let biggest_step = steps(&after[4095..], &into);
        assert!(biggest_step < 0.03, "{biggest_step}");
    }

    #[test]
    fn it_starts_at_what_is_set_and_does_not_glide_to_it() {
        let a = vec![1.0f32; 64];
        let b = vec![1.0f32; 64];
        let out = run(&mut math_with(&[(MODE, 0.0), (LEVEL, 0.25)]), &a, Some(&b));
        assert!((out[0] - 0.25).abs() < 1e-6, "the first frame is at the level that is set: {}", out[0]);
        let out = run(&mut math_with(&[(MODE, 1.0), (AMOUNT, 1.0), (LEVEL, 1.0)]), &a, Some(&b));
        assert!((out[0] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_level_that_is_turned_is_gone_to_and_not_jumped_to() {
        let a = vec![1.0f32; 2048];
        let b = vec![1.0f32; 2048];
        let mut m = math_with(&[(MODE, 0.0), (LEVEL, 1.0)]);
        let first = run(&mut m, &a, Some(&b));
        assert!((first[0] - 1.0).abs() < 1e-6);
        m.set_parameter(LEVEL as u32, 0.0).unwrap();
        let out = run(&mut m, &a, Some(&b));
        assert!(out[0] > 0.99, "the first frame is almost where it was: {}", out[0]);
        assert!(out[2047] < 0.01, "and it gets there: {}", out[2047]);
        let biggest_step = out.windows(2).map(|pair| (pair[1] - pair[0]).abs()).fold(0.0f32, f32::max);
        assert!(biggest_step < 0.01, "{biggest_step}");
    }

    #[test]
    fn what_is_not_a_number_is_not_put_in() {
        let mut a = sine(440.0, 1.0, 512);
        a[10] = f32::NAN;
        a[20] = f32::INFINITY;
        let mut b = sine(300.0, 1.0, 512);
        b[30] = f32::NAN;
        for mode in 0..12 {
            let mut m = math_with(&[(MODE, mode as f32), (LEVEL, 1.0)]);
            let out = run(&mut m, &a, Some(&b));
            assert!(out.iter().all(|x| x.is_finite()), "mode {mode}");
            let out = run(&mut m, &a, Some(&b));
            assert!(out.iter().all(|x| x.is_finite()), "mode {mode}, the next block");
        }
    }

    #[test]
    fn a_parameter_is_held_to_its_range_and_a_choice_is_a_whole_number() {
        let mut m = Math::new(SAMPLE_RATE);
        m.set_parameter(MODE as u32, 3.6).unwrap();
        assert_eq!(m.get_parameter(MODE as u32), Some(4.0));
        m.set_parameter(MODE as u32, 99.0).unwrap();
        assert_eq!(m.get_parameter(MODE as u32), Some(11.0));
        m.set_parameter(PITCH as u32, 500.0).unwrap();
        assert_eq!(m.get_parameter(PITCH as u32), Some(120.0));
        m.set_parameter(AMOUNT as u32, -1.0).unwrap();
        assert_eq!(m.get_parameter(AMOUNT as u32), Some(0.0));
        assert!(m.set_parameter(AMOUNT as u32, f32::NAN).is_err());
        assert!(m.set_parameter(99, 0.0).is_err());
        assert_eq!(m.get_parameter(99), None);
    }

    #[test]
    fn each_preset_sets_all_the_parameters_and_loads_by_its_id() {
        let mut m = Math::new(SAMPLE_RATE);
        assert_eq!(m.presets().len(), PRESETS.len());
        for (id, name, values) in PRESETS {
            assert_eq!(m.presets().iter().find(|preset| preset.id == *id).map(|preset| preset.name.as_str()), Some(*name));
            m.load_preset(id).unwrap();
            for (index, value) in values.iter().enumerate() {
                assert_eq!(m.get_parameter(index as u32), Some(*value), "{id}: {}", PARAMS[index].name);
                assert!((PARAMS[index].min..=PARAMS[index].max).contains(value), "{id}: {} is in its range", PARAMS[index].name);
            }
            let a = sine(220.0, 0.5, 4096);
            let out = run(&mut m, &a, None);
            assert!(out.iter().all(|x| x.is_finite() && x.abs() <= 1.5), "{id}");
        }
        assert!(m.load_preset("no such preset").is_err());
    }

    #[test]
    fn a_preset_is_what_it_says_from_the_first_sample() {
        let mut m = Math::new(SAMPLE_RATE);
        // It has run at a level of nothing; the preset's level (0.7) is there from its first sample, and does not glide up from nothing.
        m.set_parameter(LEVEL as u32, 0.0).unwrap();
        run(&mut m, &sine(220.0, 0.5, 1024), None);
        m.load_preset("ring-bell").unwrap();
        let phase = m.phase;
        let out = run(&mut m, &[1.0; 8], None);
        // The ring bell's carrier is E4, a sine, and the sound is A (1) times it, at 0.7.
        for (n, sample) in out.iter().enumerate() {
            let carrier = (TAU as f64 * (phase + n as f64 * 329.6256 / 48_000.0)).sin() as f32;
            assert!((sample - 0.7 * carrier).abs() < 1e-3, "frame {n}: {sample}");
        }
    }

    #[test]
    fn the_warp_line_has_a_power_of_two_frames_and_enough_for_ten_milliseconds_at_any_rate() {
        for rate in [22_050.0f32, 44_100.0, 48_000.0, 96_000.0, 192_000.0] {
            let m = Math::new(rate);
            let size = m.line[0].len();
            assert!(size.is_power_of_two() && size as f32 > WARP_MAX_SEC * rate + 2.0, "{rate}: {size}");
        }
    }

    #[test]
    fn every_mode_makes_a_sound_of_two_and_changing_the_mode_while_it_runs_is_safe() {
        let a = sine(330.0, 0.8, 1024);
        let b = sine(110.0, 0.8, 1024);
        let mut m = Math::new(SAMPLE_RATE);
        for round in 0..3 {
            for mode in 0..12 {
                m.set_parameter(MODE as u32, mode as f32).unwrap();
                let out = run(&mut m, &a, Some(&b));
                assert!(out.iter().all(|x| x.is_finite() && x.abs() <= 1.5), "round {round}, mode {mode}");
            }
        }
    }

    #[test]
    fn its_parameters_say_when_they_have_an_effect() {
        let params = Math::new(SAMPLE_RATE).parameters();
        let choice = |values: &[u32]| UsedCondition::Choice { param: MODE as u32, values: values.to_vec() };
        let said = |name: &str| params.iter().find(|p| p.name == name).unwrap().used_when.clone().map(|used| (used.condition, used.why));
        assert_eq!(said("mode"), None);
        assert_eq!(said("level"), None);
        assert_eq!(said("amount"), Some((choice(&[1, 2, 3, 4, 10, 11]), "only in the modes am, fold, warp, compare, hold, follow".to_string())));
        assert_eq!(said("bits"), Some((choice(&[7, 8, 9]), "only in the modes xor, and, or".to_string())));
        for name in ["attack", "release"] {
            assert_eq!(said(name), Some((choice(&[11]), "only in the mode follow".to_string())), "{name}");
        }
        for name in ["pitch", "wave"] {
            assert_eq!(said(name), Some((UsedCondition::SideUnpatched, "only while nothing is patched at B".to_string())), "{name}");
        }
        // The modes that the rules name are the ones that the names say.
        for (name, modes) in [("amount", &["am", "fold", "warp", "compare", "hold", "follow"][..]), ("bits", &["xor", "and", "or"]), ("attack", &["follow"])] {
            let Some(UsedWhen { condition: UsedCondition::Choice { param, values }, .. }) = params.iter().find(|p| p.name == name).unwrap().used_when.clone() else { panic!("{name} has a rule") };
            assert_eq!(param, MODE as u32);
            assert_eq!(values.iter().map(|&place| MODE_NAMES[place as usize]).collect::<Vec<_>>(), modes, "{name}");
        }
    }

    #[test]
    fn a_parameter_that_says_it_has_no_effect_has_none_and_one_that_says_it_has_one_has_one() {
        // In every mode, with a cable at B and with none, each parameter is set to two values and what the effect puts out is compared. Where the parameter says that it has no effect the two are
        // the same bits (nothing that runs reads it); where it says it has one they are not the same sound. So that the rules that a front-end dims a knob by are what the arithmetic does.
        let sound = |n: usize, hz: f32, amplitude: f32| amplitude * (TAU * hz * n as f32 / SAMPLE_RATE).sin();
        let a: Vec<f32> = (0..9_600).map(|n| sound(n, 220.0, 0.6) + sound(n, 1_450.0, 0.3)).collect();
        // B is loud for the first tenth of a second and quiet after it, so that the follower rises and falls, and it goes up through zero often, for `hold`.
        let b: Vec<f32> = (0..9_600).map(|n| sound(n, 330.0, if n < 4_800 { 0.9 } else { 0.1 })).collect();
        let params = Math::new(SAMPLE_RATE).parameters();
        let settings = [(AMOUNT, 0.2, 0.9), (BITS, 2.0, 9.0), (PITCH, 48.0, 60.0), (WAVE, 0.0, 2.0), (ATTACK, 0.001, 0.1), (RELEASE, 0.01, 1.0), (LEVEL, 0.3, 0.9)];
        for (mode, mode_name) in MODE_NAMES.iter().enumerate() {
            for patched in [false, true] {
                for (index, low, high) in settings {
                    let play = |value: f32| run(&mut math_with(&[(MODE, mode as f32), (index, value)]), &a, patched.then_some(&b[..]));
                    let (first, second) = (play(low), play(high));
                    let used = params[index].used_when.as_ref().is_none_or(|used| used.condition.holds(|param| (param == MODE as u32).then_some(mode as f32), patched));
                    let worst = first.iter().zip(&second).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
                    let what = format!("{} in {mode_name}, with {}", PARAMS[index].name, if patched { "a cable at B" } else { "nothing at B" });
                    if used {
                        assert!(worst > 1e-3, "{what}: it says it has an effect and the two sounds differ by {worst}");
                    } else {
                        assert!(first == second, "{what}: it says it has none and the two sounds differ by {worst}");
                    }
                }
            }
        }
    }

    #[test]
    fn it_makes_no_sound_of_its_own_when_a_is_silent_in_the_modes_that_multiply_or_hold() {
        let silence = vec![0.0f32; 2048];
        let b = sine(300.0, 1.0, 2048);
        for mode in [0.0f32, 1.0, 3.0, 10.0, 11.0] {
            let out = run(&mut math_with(&[(MODE, mode), (LEVEL, 1.0)]), &silence, Some(&b));
            assert!(out.iter().all(|x| *x == 0.0), "mode {mode}");
        }
    }
}
