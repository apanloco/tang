//! A drive: a waveshaper that bends the sound, so that a clean one gains harmonics as the drive is turned up, from the warmth of a rounded
//! peak to a fuzz that is a square, or a wave that folds back on itself. Four shapes (`type`), a gain into the shape (`drive`), a low-pass
//! after it (`tone`) and the level that comes out (`level`).
//!
//! The shape is made at twice the sample rate (a half-band filter up and another down), since a shape that is bent by a hard corner makes
//! harmonics far above the Nyquist frequency, and in a sampled signal they fold down into the band as notes that are not in tune with
//! anything. The price is a delay of `LATENCY` frames, which the host's dry/wet mix does not make up for: a drive is put in fully wet.

use std::f32::consts::TAU;

use crate::plugin::{ParameterInfo, Plugin, Preset};

pub const NAME: &str = "Drive";
pub const ID: &str = "builtin:drive";
pub const PARAM_COUNT: usize = 4;

/// How many of the half-band's taps the oversampling filters have on each side of the middle one that matter (the taps at odd distances from
/// the middle are the only ones that are not 0), and so how long they are: `4 * HALF - 1` taps.
const HALF: usize = 12;
/// How many frames the sound is late by, for the two filters that it goes through, up and down. (Nothing in the program needs the number: the host has no way to be told of it,
/// and the tests line the sound up by it.)
#[cfg(test)]
const LATENCY: usize = 2 * HALF - 1;

/// The shape of the window that the half-band is cut out of an ideal one with (Kaiser's `beta`): about 70 decibels of the images and the aliases gone.
const KAISER_BETA: f64 = 7.0;

/// How many sounds each side keeps of what went in and of what the shape made of it: more than the `2 * HALF` that the filters look back over, a power of two.
const RING: usize = 32;
const MASK: usize = RING - 1;

/// The names of the four shapes, in the order of the `type` parameter.
const TYPE_NAMES: [&str; 4] = ["soft", "hard", "tube", "fold"];

/// The gain into the shape at the top of `drive`, in decibels (the knob is linear in decibels, so that a modulator moves the gain by the ear's steps).
const DRIVE_MAX_DB: f32 = 36.0;

/// How fast `drive` and `level` follow what they are set to, in seconds: a step in the gain into a shape is a click.
const GAIN_SMOOTHING_SEC: f32 = 0.005;

/// The cutoff of the tone at its lowest, and at its highest, which is no filter; the scale between them is exponential, as a cutoff is.
const TONE_LOW_HZ: f32 = 500.0;
const TONE_HIGH_HZ: f32 = 20_000.0;
/// A tone at or above this is no filter.
const TONE_OPEN: f32 = 0.999;

/// How far the tube's curve is moved off the middle, which is what makes its two sides different (and makes the even harmonics), and the corner
/// of the high-pass that takes out the offset that makes.
const TUBE_BIAS: f32 = 0.3;
const DC_BLOCK_HZ: f32 = 15.0;

struct ParamSpec {
    name: &'static str,
    min: f32,
    max: f32,
    default: f32,
}

const PARAMS: &[ParamSpec] = &[
    ParamSpec { name: "type",  min: 0.0, max: 3.0, default: 0.0 },
    ParamSpec { name: "drive", min: 0.0, max: 1.0, default: 0.35 },
    ParamSpec { name: "tone",  min: 0.0, max: 1.0, default: 1.0 },
    ParamSpec { name: "level", min: 0.0, max: 1.0, default: 0.5 },
];

pub const PRESETS: &[(&str, &str, [f32; PARAM_COUNT])] = &[
    // (id, display_name, [type, drive, tone, level])
    ("warm",   "Warm",       [0.0, 0.30, 0.75, 0.48]),
    ("crunch", "Crunch",     [2.0, 0.50, 0.85, 0.29]),
    ("fuzz",   "Fuzz",       [1.0, 0.85, 0.60, 0.25]),
    ("fold",   "Wavefolder", [3.0, 0.55, 1.00, 0.50]),
];

/// What each side of the sound has of its own: the last sounds that went in (after the gain) and the last that the shape made of what the filter up put out, in its
/// two phases, with the place of the newest; the tone's low-pass; and the high-pass that takes the tube's offset out.
#[derive(Default)]
struct Channel {
    input: [f32; RING],
    even: [f32; RING],
    odd: [f32; RING],
    at: usize,
    tone: f32,
    dc_in: f32,
    dc_out: f32,
}

pub struct Drive {
    sample_rate: f32,
    /// The half-band filter's taps at even places (see `half_band_taps`), made once.
    taps: [f32; 2 * HALF],
    channels: [Channel; 2],
    /// Whether the drive has run since it was made or given a preset. It starts at the gain and the level that are set and does not glide to them from where it was.
    started: bool,
    /// The knob of the gain as it is now, and the gain it makes: they follow `drive` (see `GAIN_SMOOTHING_SEC`) by the knob, which is linear in decibels, so that the gain
    /// glides by a ratio a sample and a step of the knob is not a step of the gain. The level follows its setting too.
    drive_now: f32,
    gain_now: f32,
    level_now: f32,
    kind: f32,
    drive: f32,
    tone: f32,
    level: f32,
}

impl Drive {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            taps: half_band_taps(),
            channels: [Channel::default(), Channel::default()],
            started: false,
            drive_now: PARAMS[1].default,
            gain_now: gain_for(PARAMS[1].default),
            level_now: PARAMS[3].default,
            kind: PARAMS[0].default,
            drive: PARAMS[1].default,
            tone: PARAMS[2].default,
            level: PARAMS[3].default,
        }
    }
}

/// The gain into the shape that the knob `drive` (0 to 1) makes: 0 to `DRIVE_MAX_DB` decibels.
fn gain_for(drive: f32) -> f32 {
    10.0f32.powf(DRIVE_MAX_DB * drive / 20.0)
}

/// What the shape is given at the most: the gain of a sound that is hot makes a number that is large, and a fold of an infinite one is not a number.
const SHAPE_LIMIT: f32 = 1000.0;

#[derive(Copy, Clone, PartialEq, Debug)]
enum Kind {
    Soft,
    Hard,
    Tube,
    Fold,
}

impl Kind {
    fn from_param(value: f32) -> Self {
        match value.round() as i32 {
            1 => Kind::Hard,
            2 => Kind::Tube,
            3 => Kind::Fold,
            _ => Kind::Soft,
        }
    }
}

/// The shape: what comes out for `x` that goes in. All four have a slope of 1 at the middle (so that no drive is no change), and three are odd (the two sides are
/// alike, which makes the odd harmonics only): the hyperbolic tangent, a line that stops at 1, a line that turns back at 1 and at -1 (a triangle, folded) and the
/// tangent again with its middle moved off to one side, which makes the two sides different, the even harmonics and an offset (the high-pass after it takes that out).
fn shape(kind: Kind, x: f32) -> f32 {
    match kind {
        Kind::Soft => x.tanh(),
        Kind::Hard => x.clamp(-1.0, 1.0),
        Kind::Tube => ((x + TUBE_BIAS).tanh() - TUBE_BIAS.tanh()) / (1.0 - TUBE_BIAS.tanh().powi(2)),
        Kind::Fold => {
            // A line that goes on past 1 as a line that comes back, and the same past -1: a triangle of a period of 4, in [-1, 3) from -1.
            let t = (x + 1.0).rem_euclid(4.0) - 1.0;
            if t <= 1.0 {
                t
            } else {
                2.0 - t
            }
        }
    }
}

/// The coefficient of the one-pole low-pass of the tone (0 dark, 1 open), or none when it is open.
fn tone_coefficient(tone: f32, sample_rate: f32) -> Option<f32> {
    if tone >= TONE_OPEN {
        return None;
    }
    let hz = (TONE_LOW_HZ * (TONE_HIGH_HZ / TONE_LOW_HZ).powf(tone)).min(sample_rate * 0.45);
    Some(1.0 - (-TAU * hz / sample_rate).exp())
}

/// A sample that is a number: what is not (a plugin before it put out a NaN) would stay in the filters for ever.
#[inline]
fn finite(x: f32) -> f32 {
    if x.is_finite() {
        x
    } else {
        0.0
    }
}

/// The modified Bessel function of the first kind and order 0 (the series), which the Kaiser window is made of.
fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term) = (1.0, 1.0);
    for k in 1..60 {
        term *= (x / (2.0 * k as f64)).powi(2);
        sum += term;
    }
    sum
}

/// The taps of the half-band low-pass of `4 * HALF - 1` taps (a quarter of the rate that it runs at is where it is a half) that are not 0: all but the middle one, which
/// is a half, are at odd distances from the middle, so that the filter has a middle and `2 * HALF` taps that are not, and the pair of them that are at the same distance
/// on both sides of it are the same. They are the ideal ones, `sin(pi d / 2) / (pi d)`, under a Kaiser window. Whatever the window, the filter adds to its own mirror image
/// round a quarter of the rate to 1 (the middle is a half and the taps are odd about it), which is why the sound that comes out of a pair of them in line is the sound that went in.
fn half_band_taps() -> [f32; 2 * HALF] {
    let reach = (2 * HALF - 1) as f64;
    std::array::from_fn(|j| {
        let distance = 2.0 * j as f64 - reach;
        let ideal = (std::f64::consts::FRAC_PI_2 * distance).sin() / (std::f64::consts::PI * distance);
        let window = bessel_i0(KAISER_BETA * (1.0 - (distance / reach).powi(2)).max(0.0).sqrt()) / bessel_i0(KAISER_BETA);
        (ideal * window) as f32
    })
}

impl Plugin for Drive {
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

    fn process(&mut self, _midi_events: &[(u64, [u8; 3])], audio_in: &[&[f32]], audio_out: &mut [&mut [f32]]) -> anyhow::Result<()> {
        let frames = audio_out.first().map_or(0, |out| out.len());
        if !self.started {
            self.drive_now = self.drive;
            self.gain_now = gain_for(self.drive);
            self.level_now = self.level;
            self.started = true;
        }
        let kind = Kind::from_param(self.kind);
        let smoothing = 1.0 - (-1.0 / (GAIN_SMOOTHING_SEC * self.sample_rate)).exp();
        let tone_coef = tone_coefficient(self.tone, self.sample_rate);
        let dc_pole = (-TAU * DC_BLOCK_HZ / self.sample_rate).exp();
        for frame in 0..frames {
            self.drive_now += (self.drive - self.drive_now) * smoothing;
            self.gain_now = gain_for(self.drive_now);
            self.level_now += (self.level - self.level_now) * smoothing;
            for (channel, out) in audio_out.iter_mut().enumerate().take(2) {
                let input = audio_in.get(channel).or(audio_in.first()).and_then(|input| input.get(frame)).copied().unwrap_or(0.0);
                let state = &mut self.channels[channel];
                // The sound at twice the rate: what the filter makes of the sound at the places that are between two of its samples, and the sound itself, delayed, at
                // those that are not; the shape is made of both, and the filter down takes the one half of them again.
                state.at = (state.at + 1) & MASK;
                state.input[state.at] = (finite(input) * self.gain_now).clamp(-SHAPE_LIMIT, SHAPE_LIMIT);
                let mut between = 0.0;
                for (j, tap) in self.taps.iter().enumerate() {
                    between += tap * state.input[(state.at + RING - j) & MASK];
                }
                let on = state.input[(state.at + RING - (HALF - 1)) & MASK];
                state.even[state.at] = shape(kind, 2.0 * between);
                state.odd[state.at] = shape(kind, on);
                let mut y = 0.5 * state.odd[(state.at + RING - HALF) & MASK];
                for (j, tap) in self.taps.iter().enumerate() {
                    y += tap * state.even[(state.at + RING - j) & MASK];
                }
                if kind == Kind::Tube {
                    let blocked = y - state.dc_in + dc_pole * state.dc_out;
                    state.dc_in = y;
                    state.dc_out = blocked;
                    y = blocked;
                }
                if let Some(coef) = tone_coef {
                    state.tone += coef * (y - state.tone);
                    y = state.tone;
                }
                out[frame] = y * self.level_now;
            }
        }
        Ok(())
    }

    fn parameters(&self) -> Vec<ParameterInfo> {
        PARAMS
            .iter()
            .enumerate()
            .map(|(i, p)| ParameterInfo {
                index: i as u32,
                name: p.name.into(),
                min: p.min,
                max: p.max,
                default: p.default,
                labels: (i == 0).then(|| TYPE_NAMES.iter().map(|name| name.to_string()).collect()),
                step_count: if i == 0 { (TYPE_NAMES.len() - 1) as u32 } else { 0 },
                ..Default::default()
            })
            .collect()
    }

    fn get_parameter(&mut self, index: u32) -> Option<f32> {
        match index {
            0 => Some(self.kind),
            1 => Some(self.drive),
            2 => Some(self.tone),
            3 => Some(self.level),
            _ => None,
        }
    }

    fn set_parameter(&mut self, index: u32, value: f32) -> anyhow::Result<()> {
        let spec = PARAMS.get(index as usize).ok_or_else(|| anyhow::anyhow!("no parameter with index {index}"))?;
        anyhow::ensure!(value.is_finite(), "{} is not a number: {value}", spec.name);
        let v = value.clamp(spec.min, spec.max);
        match index {
            0 => self.kind = v.round(),
            1 => self.drive = v,
            2 => self.tone = v,
            3 => self.level = v,
            _ => unreachable!(),
        }
        Ok(())
    }

    fn presets(&self) -> Vec<Preset> {
        PRESETS.iter().map(|(id, name, _)| Preset { name: (*name).into(), id: (*id).into() }).collect()
    }

    fn load_preset(&mut self, id: &str) -> anyhow::Result<()> {
        let (_, _, values) = PRESETS.iter().find(|(pid, _, _)| *pid == id).ok_or_else(|| anyhow::anyhow!("no preset with id {id:?}"))?;
        [self.kind, self.drive, self.tone, self.level] = *values;
        // A fresh sound: the gain and the level are the preset's from the first sample.
        self.started = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RATE: f32 = 48_000.0;

    /// The parameters by their places.
    const TYPE: u32 = 0;
    const DRIVE: u32 = 1;
    const LEVEL: u32 = 3;
    const SOFT: f32 = 0.0;
    const HARD: f32 = 1.0;
    const TUBE: f32 = 2.0;
    const FOLD: f32 = 3.0;

    /// The `drive` that makes a gain of `gain` into the shape.
    fn drive_for_gain(gain: f32) -> f32 {
        20.0 * gain.log10() / DRIVE_MAX_DB
    }

    /// A drive with the four parameters set.
    fn drive_with(kind: f32, drive: f32, tone: f32, level: f32) -> Drive {
        let mut d = Drive::new(SAMPLE_RATE);
        for (index, value) in [kind, drive, tone, level].into_iter().enumerate() {
            d.set_parameter(index as u32, value).unwrap();
        }
        d
    }

    /// `frames` of a sine of `hz` and `amplitude`.
    fn sine(hz: f32, amplitude: f32, frames: usize) -> Vec<f32> {
        (0..frames).map(|n| amplitude * (TAU * hz * n as f32 / SAMPLE_RATE).sin()).collect()
    }

    /// What the drive puts out on the left and on the right for `left` and `right` put in, in blocks of 256 frames, as the audio thread has them.
    fn run(drive: &mut Drive, left: &[f32], right: &[f32]) -> (Vec<f32>, Vec<f32>) {
        assert_eq!(left.len(), right.len());
        let (mut out_l, mut out_r) = (Vec::with_capacity(left.len()), Vec::with_capacity(left.len()));
        for (l, r) in left.chunks(256).zip(right.chunks(256)) {
            let (mut block_l, mut block_r) = (vec![0.0f32; l.len()], vec![0.0f32; l.len()]);
            drive.process(&[], &[l, r], &mut [&mut block_l[..], &mut block_r[..]]).unwrap();
            out_l.extend_from_slice(&block_l);
            out_r.extend_from_slice(&block_r);
        }
        (out_l, out_r)
    }

    /// The left side of a sine of `hz` and `amplitude`, `frames` long, put through a drive.
    fn through(drive: &mut Drive, hz: f32, amplitude: f32, frames: usize) -> Vec<f32> {
        let input = sine(hz, amplitude, frames);
        run(drive, &input, &input).0
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

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    #[test]
    fn it_is_an_effect_with_two_inputs_and_two_outputs() {
        let d = Drive::new(SAMPLE_RATE);
        assert_eq!((d.name(), d.is_instrument(), d.audio_input_count(), d.audio_output_count()), ("Drive", false, 2, 2));
        assert_eq!(d.parameters().len(), PARAM_COUNT);
    }

    #[test]
    fn its_parameters_say_their_names_ranges_labels_and_steps() {
        let d = Drive::new(SAMPLE_RATE);
        let described: Vec<(String, f32, f32, f32, u32, Option<String>)> =
            d.parameters().iter().map(|p| (p.name.clone(), p.min, p.max, p.default, p.step_count, p.labels.as_ref().map(|l| l.join(" ")))).collect();
        assert_eq!(
            described,
            [
                ("type".to_string(), 0.0, 3.0, 0.0, 3, Some("soft hard tube fold".to_string())),
                ("drive".to_string(), 0.0, 1.0, 0.35, 0, None),
                ("tone".to_string(), 0.0, 1.0, 1.0, 0, None),
                ("level".to_string(), 0.0, 1.0, 0.5, 0, None),
            ]
        );
    }

    #[test]
    fn parameters_round_trip_are_held_to_their_ranges_and_a_type_is_a_whole_number() {
        let mut d = Drive::new(SAMPLE_RATE);
        for (index, p) in d.parameters().iter().enumerate() {
            assert_eq!(d.get_parameter(index as u32), Some(p.default), "{}", p.name);
            d.set_parameter(index as u32, p.max + 100.0).unwrap();
            assert_eq!(d.get_parameter(index as u32), Some(p.max), "{} held to its top", p.name);
            d.set_parameter(index as u32, p.min - 100.0).unwrap();
            assert_eq!(d.get_parameter(index as u32), Some(p.min), "{} held to its bottom", p.name);
        }
        for (set, got) in [(1.4, 1.0), (1.6, 2.0), (2.5, 3.0), (-0.4, 0.0)] {
            d.set_parameter(TYPE, set).unwrap();
            assert_eq!(d.get_parameter(TYPE), Some(got), "{set}");
        }
        assert!(d.set_parameter(DRIVE, f32::NAN).is_err() && d.set_parameter(DRIVE, f32::INFINITY).is_err());
        assert!(d.set_parameter(4, 0.0).is_err() && d.get_parameter(4).is_none());
    }

    #[test]
    fn a_quiet_sound_with_no_drive_comes_out_as_it_went_in_late_by_the_latency_and_at_the_level() {
        // A gain of 1 into a shape that has a slope of 1 at the middle: a sine of 0.05 (the shape bends it by 0.1%) is the same sine, LATENCY frames later.
        for kind in [SOFT, HARD, TUBE, FOLD] {
            let mut d = drive_with(kind, 0.0, 1.0, 1.0);
            let input = sine(1000.0, 0.05, 4800);
            let out = run(&mut d, &input, &input).0;
            let worst = (1000..4800).map(|n| (out[n] - input[n - LATENCY]).abs()).fold(0.0f32, f32::max);
            // (The tube's curve is not straight at the middle: it bends a sound of 0.05 by a thousandth, and by twice as much with its offset.)
            let limit = if kind == TUBE { 0.002 } else { 0.001 };
            assert!(worst < limit, "type {kind}: {worst} from the sound that went in");
        }
        let mut d = drive_with(SOFT, 0.0, 1.0, 0.5);
        let out = through(&mut d, 1000.0, 0.05, 4800);
        assert!((magnitude(&out[1000..], 1000.0) - 0.025).abs() < 0.0005, "half the level");
    }

    #[test]
    fn the_gain_into_the_shape_is_the_drive_in_decibels_up_to_36() {
        // A quiet sine through the hard shape is the sine times the gain while it stays under 1.
        for (drive, gain) in [(0.0f32, 1.0f32), (0.25, 10.0f32.powf(9.0 / 20.0)), (0.5, 10.0f32.powf(18.0 / 20.0)), (1.0, 10.0f32.powf(36.0 / 20.0))] {
            let mut d = drive_with(HARD, drive, 1.0, 1.0);
            let out = through(&mut d, 1000.0, 0.005, 9600);
            let got = magnitude(&out[2400..], 1000.0);
            assert!((got / (0.005 * gain) - 1.0).abs() < 0.02, "drive {drive}: {got} for {}", 0.005 * gain);
        }
    }

    #[test]
    fn the_hard_shape_clips_a_sine_to_the_level_and_the_soft_one_rounds_it_off_under_the_level() {
        let mut hard = drive_with(HARD, 1.0, 1.0, 0.5);
        let out = through(&mut hard, 1000.0, 0.5, 9600);
        let peak = out[2400..].iter().fold(0.0f32, |m, x| m.max(x.abs()));
        // (A square that is made of a band of frequencies overshoots by about a tenth of its step: the ringing of a filter, 13% of the level here.)
        assert!(peak > 0.49 && peak < 0.6, "{peak}");
        // The flat tops: most frames of a sine that is gained by 60 are at the level.
        let flat = out[2400..].iter().filter(|x| x.abs() > 0.45).count() as f32 / out[2400..].len() as f32;
        assert!(flat > 0.85, "{flat} of the frames are on a flat top");
        let mut soft = drive_with(SOFT, 0.6, 1.0, 1.0);
        let out = through(&mut soft, 1000.0, 0.5, 9600);
        let peak = out[2400..].iter().fold(0.0f32, |m, x| m.max(x.abs()));
        // (Round, and not a square: its corners are rounded and the filter has little to ring at. Its peak is the level to a hair.)
        assert!(peak > 0.95 && peak < 1.02, "{peak}");
    }

    #[test]
    fn the_symmetric_shapes_make_the_odd_harmonics_only_and_the_tube_makes_the_even_ones_too() {
        for kind in [SOFT, HARD, FOLD] {
            let mut d = drive_with(kind, drive_for_gain(4.0), 1.0, 1.0);
            let out = through(&mut d, 1000.0, 0.5, 14_400);
            let settled = &out[2400..];
            let (first, second, third) = (magnitude(settled, 1000.0), magnitude(settled, 2000.0), magnitude(settled, 3000.0));
            assert!(third > 0.03 * first, "type {kind}: the third harmonic is {} of the first", third / first);
            assert!(second < 0.002 * first, "type {kind}: the second is {} of the first", second / first);
        }
        let mut tube = drive_with(TUBE, drive_for_gain(4.0), 1.0, 1.0);
        let out = through(&mut tube, 1000.0, 0.5, 14_400);
        let settled = &out[2400..];
        assert!(magnitude(settled, 2000.0) > 0.05 * magnitude(settled, 1000.0), "the tube has a second harmonic");
        // ... and the offset that the bent curve makes is taken out.
        let mean = settled.iter().sum::<f32>() / settled.len() as f32;
        assert!(mean.abs() < 0.002, "{mean} is left of the offset");
    }

    #[test]
    fn the_fold_goes_back_on_itself_where_the_hard_shape_stops() {
        // A sine of 1000 Hz whose peak is 2.5 at the shape: 0 up to 1, back down to 0 at 2 and on to 0.5, and the same on the way down and under: six zero crossings in a cycle.
        let mut d = drive_with(FOLD, drive_for_gain(5.0), 1.0, 1.0);
        let out = through(&mut d, 1000.0, 0.5, 14_400);
        let window = &out[2400..2400 + 480];
        let crossings = window.windows(2).filter(|w| (w[0] < 0.0) != (w[1] < 0.0)).count();
        assert_eq!(crossings, 60, "ten cycles of six");
        let peak = window.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        assert!(peak > 0.85 && peak < 1.1, "and no higher than the level (a fold is a triangle, and a band of frequencies rounds its corners off): {peak}");
    }

    #[test]
    fn the_level_is_the_gain_out_and_none_is_silence() {
        let input = sine(1000.0, 0.25, 9600);
        let (mut half, mut full, mut none) = (drive_with(SOFT, 0.35, 1.0, 0.5), drive_with(SOFT, 0.35, 1.0, 1.0), drive_with(SOFT, 0.35, 1.0, 0.0));
        let (half, full, none) = (run(&mut half, &input, &input).0, run(&mut full, &input, &input).0, run(&mut none, &input, &input).0);
        assert!((rms(&half[2400..]) / rms(&full[2400..]) - 0.5).abs() < 0.01);
        assert!(none.iter().all(|x| x.abs() < 1e-6), "level 0 is silence");
    }

    #[test]
    fn the_tone_is_a_low_pass_that_the_top_of_the_knob_takes_away() {
        // Through the soft shape at no drive (a straight line for sines this quiet).
        let level_at = |tone: f32, hz: f32| {
            let mut d = drive_with(SOFT, 0.0, tone, 1.0);
            magnitude(&through(&mut d, hz, 0.02, 9600)[2400..], hz)
        };
        let open = (level_at(1.0, 1000.0), level_at(1.0, 10_000.0));
        assert!((open.1 / open.0 - 1.0).abs() < 0.06, "open, 10 kHz is as loud as 1 kHz: {}", open.1 / open.0);
        // (The filter of the lowest tone is a one-pole at 500 Hz: a tenth of what an open one lets through at 10 kHz, and a half at 1 kHz.)
        let dark = (level_at(0.0, 1000.0) / open.0, level_at(0.0, 10_000.0) / open.1);
        assert!(dark.1 < 0.08 && dark.0 > 0.35 && dark.0 < 0.55, "dark, 1 kHz is {} of the open sound and 10 kHz is {}", dark.0, dark.1);
        let middle = level_at(0.5, 10_000.0) / open.1;
        assert!(middle > dark.1 && middle < 0.5, "and in between: {middle}");
    }

    #[test]
    fn the_gain_is_gone_to_over_a_few_milliseconds_and_not_jumped_to() {
        let mut d = drive_with(HARD, 0.0, 1.0, 1.0);
        let input = sine(1000.0, 0.01, 4800);
        run(&mut d, &input, &input);
        let before = d.gain_now;
        d.set_parameter(DRIVE, 1.0).unwrap();
        run(&mut d, &input[..1], &input[..1]);
        assert!(d.gain_now > before && d.gain_now < 1.2 * before, "a frame in, the gain is {} for {before}", d.gain_now);
        run(&mut d, &input, &input);
        let target = 10.0f32.powf(DRIVE_MAX_DB / 20.0);
        assert!((d.gain_now / target - 1.0).abs() < 0.01, "and a tenth of a second on it is there: {} for {target}", d.gain_now);
    }

    #[test]
    fn the_level_is_gone_to_and_not_jumped_to() {
        let mut d = drive_with(SOFT, 0.35, 1.0, 0.0);
        let input = sine(1000.0, 0.25, 4800);
        run(&mut d, &input, &input);
        d.set_parameter(LEVEL, 1.0).unwrap();
        run(&mut d, &input[..1], &input[..1]);
        assert!(d.level_now > 0.0 && d.level_now < 0.01, "a frame in, the level is {}", d.level_now);
        run(&mut d, &input, &input);
        assert!((d.level_now - 1.0).abs() < 0.01, "and a tenth of a second on it is {}", d.level_now);
    }

    #[test]
    fn a_drive_that_is_set_before_it_runs_and_one_that_is_given_a_preset_start_at_what_is_set() {
        let mut d = drive_with(HARD, 1.0, 1.0, 0.8);
        run(&mut d, &[0.0], &[0.0]);
        assert_eq!((d.gain_now, d.level_now), (gain_for(1.0), 0.8), "the gain and the level are not on their way from the defaults");
        d.load_preset("fuzz").unwrap();
        run(&mut d, &[0.0], &[0.0]);
        let fuzz = PRESETS.iter().find(|preset| preset.0 == "fuzz").unwrap().2;
        assert_eq!((d.gain_now, d.level_now), (gain_for(fuzz[1]), fuzz[3]), "and a preset is the sound it makes from its first sample");
    }

    #[test]
    fn silence_is_silence_and_a_sound_that_is_not_a_number_is_silence() {
        for kind in [SOFT, HARD, TUBE, FOLD] {
            let mut d = drive_with(kind, 1.0, 0.5, 1.0);
            let zeros = vec![0.0f32; 4800];
            let out = run(&mut d, &zeros, &zeros).0;
            assert!(out.iter().all(|x| *x == 0.0), "type {kind}");
            let mut bad = vec![0.0f32; 4800];
            bad[100] = f32::NAN;
            bad[200] = f32::INFINITY;
            let out = run(&mut d, &bad, &bad).0;
            assert!(out.iter().all(|x| x.is_finite() && x.abs() < 1e-6), "type {kind}: nothing in the numbers that were not numbers");
            // A number that is huge is one: it is bent like any other (a fold of 1e30 gained by sixty is not a number, unless it is held to what a fold can be given).
            let mut huge = vec![0.0f32; 4800];
            huge[300] = 1e30;
            huge[301] = -3e38;
            let out = run(&mut d, &huge, &huge).0;
            assert!(out.iter().all(|x| x.is_finite() && x.abs() < 2.0), "type {kind}: {:?}", out.iter().cloned().fold(0.0f32, |m, x| if x.is_finite() { m.max(x.abs()) } else { f32::NAN }));
        }
    }

    #[test]
    fn the_two_sides_are_shaped_alike_and_a_single_input_is_both() {
        let mut d = drive_with(TUBE, 0.6, 0.8, 0.7);
        let left = sine(440.0, 0.3, 4800);
        let right = sine(660.0, 0.2, 4800);
        let (out_l, out_r) = run(&mut d, &left, &right);
        let mut alone = drive_with(TUBE, 0.6, 0.8, 0.7);
        assert_eq!(run(&mut alone, &left, &left).0, out_l, "the left is the left's whatever is on the right");
        let mut again = drive_with(TUBE, 0.6, 0.8, 0.7);
        assert_eq!(run(&mut again, &right, &right).0, out_r);
        // One input channel and one output: the sound that a mono chain has.
        let mut mono = drive_with(TUBE, 0.6, 0.8, 0.7);
        let mut one = vec![0.0f32; 4800];
        mono.process(&[], &[&left[..]], &mut [&mut one[..]]).unwrap();
        assert_eq!(one, out_l);
        // ... and one input with two outputs is the same sound on both.
        let mut dual = drive_with(TUBE, 0.6, 0.8, 0.7);
        let (mut a, mut b) = (vec![0.0f32; 4800], vec![0.0f32; 4800]);
        dual.process(&[], &[&left[..]], &mut [&mut a[..], &mut b[..]]).unwrap();
        assert_eq!((a, b), (out_l.clone(), out_l));
    }

    /// The numbers of a drive are made on the audio thread, which must not make a trip to the allocator.
    #[test]
    fn a_drive_does_not_allocate() {
        let mut d = drive_with(FOLD, 0.8, 0.5, 0.5);
        let input = sine(220.0, 0.3, 256);
        let mut out_l = vec![0.0f32; 256];
        let mut out_r = vec![0.0f32; 256];
        let ((), allocations) = crate::alloc_guard::allocations_in(|| {
            for _ in 0..8 {
                d.process(&[], &[&input[..], &input[..]], &mut [&mut out_l[..], &mut out_r[..]]).unwrap();
            }
        });
        assert_eq!(allocations, 0);
    }

    // ---------------------------------------------------------------------------------------------------------------------------------
    // The oversampling
    // ---------------------------------------------------------------------------------------------------------------------------------

    /// An FFT of `re` (and `im`), in place; the length is a power of two.
    fn fft(re: &mut [f64], im: &mut [f64]) {
        let n = re.len();
        let mut j = 0;
        for i in 1..n {
            let mut bit = n >> 1;
            while j & bit != 0 {
                j ^= bit;
                bit >>= 1;
            }
            j ^= bit;
            if i < j {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            let angle = -std::f64::consts::TAU / len as f64;
            for start in (0..n).step_by(len) {
                for k in 0..len / 2 {
                    let (cos, sin) = ((angle * k as f64).cos(), (angle * k as f64).sin());
                    let (a, b) = (start + k, start + k + len / 2);
                    let (tr, ti) = (re[b] * cos - im[b] * sin, re[b] * sin + im[b] * cos);
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
            }
            len <<= 1;
        }
    }

    /// How much of the power of `signal` (a sine of `hz` that was shaped) is not in the harmonics of `hz` that are under the Nyquist frequency: the notes that
    /// are in tune with nothing, which the harmonics above it fold down into. A Hann window, and the bins within `SPREAD` of a harmonic count as the harmonic.
    fn aliased_share(signal: &[f32], hz: f32) -> f32 {
        const SPREAD: i64 = 4;
        let n = signal.len();
        let (mut re, mut im): (Vec<f64>, Vec<f64>) = (signal.iter().enumerate().map(|(i, &x)| f64::from(x) * (0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos())).collect(), vec![0.0; n]);
        fft(&mut re, &mut im);
        let bin_hz = f64::from(SAMPLE_RATE) / n as f64;
        let harmonics: Vec<i64> = (1..).map(|k| (f64::from(hz) * f64::from(k) / bin_hz).round() as i64).take_while(|&bin| (bin as usize) < n / 2).collect();
        let (mut total, mut aliased) = (0.0f64, 0.0f64);
        for bin in 1..n / 2 {
            let power = re[bin] * re[bin] + im[bin] * im[bin];
            total += power;
            if !harmonics.iter().any(|&h| (h - bin as i64).abs() <= SPREAD) {
                aliased += power;
            }
        }
        (aliased / total) as f32
    }

    #[test]
    fn what_the_shapes_make_above_the_nyquist_frequency_is_mostly_not_folded_down_into_the_band() {
        // A sine of 3137 Hz (no harmonic of it is another's alias) through each shape, at the top of the knob (36 dB) and at a half of it (18 dB). The share of the power that
        // is in no harmonic is what folded. With no oversampling the hard shape's share at the top is 0.032 (-15 dB: measured, and the first red of this test); these are
        // what it is with it, and the limits are a little above. The corners of a hard shape and of a fold are the worst there is, since their harmonics go on for ever (a
        // sound that is a square has a hundredth of its power above the eighteenth harmonic), so that a doubled rate still folds a good part of them at the top of the knob.
        for (kind, drive, limit) in [
            (HARD, 1.0, 0.0035),
            (SOFT, 1.0, 0.0017),
            (TUBE, 1.0, 0.0017),
            (HARD, 0.5, 0.00006),
            (SOFT, 0.5, 0.00003),
            (TUBE, 0.5, 0.00003),
            (FOLD, 0.5, 0.0005),
        ] {
            let mut d = drive_with(kind, drive, 1.0, 1.0);
            let out = through(&mut d, 3137.0, 0.3, 4096 + 32_768);
            let share = aliased_share(&out[4096..], 3137.0);
            assert!(share < limit, "type {kind}: {share} of the power is aliased");
        }
    }

    #[test]
    fn the_half_band_filters_are_flat_in_the_band_and_the_two_images_of_it_add_up_to_one() {
        let taps = half_band_taps();
        // The response of the whole filter (taps at even distances from the middle are 0, the middle is a half) at a frequency, in units of the output rate.
        let response = |f: f64| -> f64 {
            let c = (2 * HALF - 1) as f64;
            let (mut re, mut im) = (0.5 * (std::f64::consts::TAU * f * c).cos(), 0.5 * (std::f64::consts::TAU * f * c).sin());
            for (j, &t) in taps.iter().enumerate() {
                let (re_part, im_part) = ((std::f64::consts::TAU * f * (2 * j) as f64).cos(), (std::f64::consts::TAU * f * (2 * j) as f64).sin());
                re += f64::from(t) * re_part;
                im += f64::from(t) * im_part;
            }
            (re * re + im * im).sqrt()
        };
        let db = |x: f64| 20.0 * x.log10();
        assert!(db(response(0.0)).abs() < 0.01, "DC: {} dB", db(response(0.0)));
        for hz in [1000.0f64, 5000.0, 10_000.0, 16_000.0] {
            assert!(db(response(hz / 96_000.0)).abs() < 0.1, "{hz} Hz: {} dB", db(response(hz / 96_000.0)));
        }
        assert!(db(response(19_000.0 / 96_000.0)) > -1.5, "19 kHz: {} dB", db(response(19_000.0 / 96_000.0)));
        assert!((db(response(0.25)) + 6.02).abs() < 0.05, "a half at the Nyquist frequency of the input: {} dB", db(response(0.25)));
        // The stopband: a sound at 28 kHz at the doubled rate is 45 decibels down and every one past 30 kHz 75 (what a shape makes harmonics of, up there, that would fold
        // down into the band at the rate of the input).
        assert!(db(response(28_000.0 / 96_000.0)) < -45.0, "28 kHz: {} dB", db(response(28_000.0 / 96_000.0)));
        for hz in [30_000.0f64, 32_000.0, 36_000.0, 40_000.0, 48_000.0] {
            assert!(db(response(hz / 96_000.0)) < -75.0, "{hz} Hz: {} dB", db(response(hz / 96_000.0)));
        }
    }

    #[test]
    fn the_shapes_are_what_their_names_say() {
        // Soft: the hyperbolic tangent. Hard: a line that stops at 1. Fold: a line that turns back at 1 and goes on to -1 and back. All three have a slope of 1 at the middle and are odd.
        assert!((shape(Kind::Soft, 0.1) - 0.1f32.tanh()).abs() < 1e-7 && (shape(Kind::Soft, 3.0) - 3.0f32.tanh()).abs() < 1e-7);
        assert_eq!((shape(Kind::Hard, 0.3), shape(Kind::Hard, 2.0), shape(Kind::Hard, -7.0)), (0.3, 1.0, -1.0));
        for (x, folded) in [(0.5f32, 0.5f32), (1.0, 1.0), (1.5, 0.5), (2.0, 0.0), (2.5, -0.5), (3.0, -1.0), (3.5, -0.5), (4.0, 0.0), (4.5, 0.5), (5.0, 1.0), (101.0, 1.0)] {
            assert!((shape(Kind::Fold, x) - folded).abs() < 1e-5, "fold({x}) is {}", shape(Kind::Fold, x));
            assert!((shape(Kind::Fold, -x) + folded).abs() < 1e-5, "and it is odd");
        }
        for kind in [Kind::Soft, Kind::Hard, Kind::Tube, Kind::Fold] {
            let slope = (shape(kind, 1e-3) - shape(kind, -1e-3)) / 2e-3;
            assert!((slope - 1.0).abs() < 2e-3, "the slope at the middle is {slope}");
            assert_eq!(shape(kind, 0.0), 0.0);
        }
        // The tube is not odd: its two sides are not alike.
        assert!((shape(Kind::Tube, 1.0) + shape(Kind::Tube, -1.0)).abs() > 0.2);
    }

    // ---------------------------------------------------------------------------------------------------------------------------------
    // The presets
    // ---------------------------------------------------------------------------------------------------------------------------------

    #[test]
    fn the_presets_are_listed_by_name_and_set_every_parameter() {
        let mut d = Drive::new(SAMPLE_RATE);
        let listed: Vec<(String, String)> = d.presets().into_iter().map(|p| (p.id, p.name)).collect();
        assert_eq!(
            listed,
            [("warm", "Warm"), ("crunch", "Crunch"), ("fuzz", "Fuzz"), ("fold", "Wavefolder")].map(|(id, name)| (id.to_string(), name.to_string()))
        );
        for (id, _, values) in PRESETS {
            d.load_preset(id).unwrap();
            let got: Vec<f32> = (0..PARAM_COUNT as u32).map(|i| d.get_parameter(i).unwrap()).collect();
            assert_eq!(got, values.to_vec(), "{id}");
        }
        assert!(d.load_preset("no such preset").is_err());
    }

    #[test]
    fn the_presets_are_as_loud_as_each_other_and_a_little_louder_than_the_sound_that_goes_in() {
        // A sine of 0.25 (what a voice of the oscillator is), at a pitch of a bass note and at one of a lead: all four come out 1.32 to 1.42 times as loud as it went in (2.4 to 3 dB over it,
        // which is what a drive does), so that a preset that is chosen after another does not jump in level.
        for (hz, amplitude) in [(110.0f32, 0.25f32), (880.0, 0.25)] {
            let input = sine(hz, amplitude, 14_400);
            let ratios: Vec<f32> = PRESETS
                .iter()
                .map(|(id, _, _)| {
                    let mut d = Drive::new(SAMPLE_RATE);
                    d.load_preset(id).unwrap();
                    let out = run(&mut d, &input, &input).0;
                    rms(&out[4800..]) / rms(&input[4800..])
                })
                .collect();
            let (lowest, highest) = (ratios.iter().cloned().fold(f32::MAX, f32::min), ratios.iter().cloned().fold(0.0f32, f32::max));
            assert!(lowest > 1.2 && highest < 1.6, "at {hz} Hz: {ratios:?}");
            assert!(highest / lowest < 1.1, "at {hz} Hz: {ratios:?} are not as loud as each other");
        }
    }
}
