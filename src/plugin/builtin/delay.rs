//! A stereo delay: echoes of the input that repeat by a feedback, which can cross from one side to the other (a ping-pong) and be darkened
//! at every pass (a tone). It puts out the echoes only, as the reverb puts out its tail only: the host's `mix` is the blend with the dry
//! sound, so that what is heard first of a note is the note.

use std::f32::consts::TAU;

use super::soft_clip;
use crate::plugin::{ParameterInfo, Plugin, Preset};

pub const NAME: &str = "Delay";
pub const ID: &str = "builtin:delay";
pub const PARAM_COUNT: usize = 4;

/// The longest time the delay can be set to, in seconds: the top of the `time` parameter.
const MAX_TIME_SEC: f32 = 2.0;

/// How the delay, in samples, follows `time`: a change is gone through by a one-pole of this time constant, and no faster than `MAX_SLOPE`
/// samples of delay for each sample that goes by (a delay that grows by one sample a sample is a read head that stands still, and half
/// of that is a read head at half the speed, an octave down). So a jump of the knob is a sweep in the pitch of the echoes, as on a tape
/// delay, and never a click or a held sample, and a slow modulation of the time (a vibrato, a chorus) is followed as it is.
const TIME_SMOOTHING_SEC: f64 = 0.01;
const MAX_SLOPE: f64 = 0.5;

/// How fast `feedback` and `pingpong` follow what they are set to, in seconds: a step in either would be a step in every echo.
const GAIN_SMOOTHING_SEC: f32 = 0.005;

/// The cutoff of the tone at its lowest, and at its highest, which is no filter; the scale between them is exponential, as a cutoff is.
const TONE_LOW_HZ: f32 = 500.0;
const TONE_HIGH_HZ: f32 = 20_000.0;
/// A tone at or above this is no filter: the echoes are as they were put in.
const TONE_OPEN: f32 = 0.999;

struct ParamSpec {
    name: &'static str,
    min: f32,
    max: f32,
    default: f32,
    unit: Option<&'static str>,
}

const PARAMS: &[ParamSpec] = &[
    ParamSpec { name: "time",     min: 0.001, max: MAX_TIME_SEC, default: 0.375, unit: Some("s") },
    ParamSpec { name: "feedback", min: 0.0,   max: 0.95,         default: 0.45,  unit: None },
    ParamSpec { name: "tone",     min: 0.0,   max: 1.0,          default: 0.7,   unit: None },
    ParamSpec { name: "pingpong", min: 0.0,   max: 1.0,          default: 0.0,   unit: None },
];

pub const PRESETS: &[(&str, &str, [f32; PARAM_COUNT])] = &[
    // (id, display_name, [time, feedback, tone, pingpong])
    ("slapback", "Slapback",  [0.09, 0.12, 0.85, 0.0]),
    ("tape",     "Tape Echo", [0.38, 0.50, 0.45, 0.0]),
    ("pingpong", "Ping-Pong", [0.30, 0.55, 0.70, 1.0]),
    ("dub",      "Dub Echo",  [0.45, 0.78, 0.30, 0.35]),
];

pub struct Delay {
    sample_rate: f32,
    /// The echoes of the left side and of the right: rings that hold the last `lines[0].len()` samples of what was put in and fed back.
    lines: [Vec<f32>; 2],
    /// Where the next sample is written.
    write: usize,
    /// The delay, in samples, that the echoes are read at now: it follows `time` (see `TIME_SMOOTHING_SEC`). It is a 64-bit number: a one-pole
    /// in 32 bits stops short of what it follows, by as much as the steps of its number are, over its coefficient (0.1 of a sample at 4800
    /// samples), and then every echo is a fraction of a sample from where it was asked for.
    delay: f64,
    /// The feedback and the ping-pong as they are now: they follow their settings (see `GAIN_SMOOTHING_SEC`).
    feedback_now: f32,
    pingpong_now: f32,
    /// The tone, a one-pole low-pass in the feedback of each side.
    tone_state: [f32; 2],
    /// Whether the delay has run since it was made or given a preset. It starts at the time that is set and does not sweep to it from where
    /// it was.
    started: bool,
    time: f32,
    feedback: f32,
    tone: f32,
    pingpong: f32,
}

impl Delay {
    pub fn new(sample_rate: f32) -> Self {
        // (Room for the longest time, and what the read of a fraction of a sample looks at past it.)
        let length = (MAX_TIME_SEC * sample_rate).ceil() as usize + 4;
        Self {
            sample_rate,
            lines: [vec![0.0; length], vec![0.0; length]],
            write: 0,
            delay: f64::from(PARAMS[0].default * sample_rate),
            feedback_now: PARAMS[1].default,
            pingpong_now: PARAMS[3].default,
            tone_state: [0.0; 2],
            started: false,
            time: PARAMS[0].default,
            feedback: PARAMS[1].default,
            tone: PARAMS[2].default,
            pingpong: PARAMS[3].default,
        }
    }
}

/// The sample that was put in `delay` samples ago (a fraction of one is read between two), `write` being where the next is put.
#[inline]
fn read(line: &[f32], write: usize, delay: f32) -> f32 {
    let len = line.len();
    let d = delay.clamp(1.0, (len - 2) as f32);
    let whole = d as usize;
    let fraction = d - whole as f32;
    let near = line[(write + len - whole) % len];
    let far = line[(write + len - whole - 1) % len];
    near + (far - near) * fraction
}

/// The coefficient of the one-pole low-pass that darkens the feedback for `tone` (0 dark, 1 open), or none when it is open.
fn tone_coefficient(tone: f32, sample_rate: f32) -> Option<f32> {
    if tone >= TONE_OPEN {
        return None;
    }
    let hz = (TONE_LOW_HZ * (TONE_HIGH_HZ / TONE_LOW_HZ).powf(tone)).min(sample_rate * 0.45);
    Some(1.0 - (-TAU * hz / sample_rate).exp())
}

/// A sample that is a number: what is not (a plugin before it put out a NaN) would stay in the echoes for ever.
#[inline]
fn finite(x: f32) -> f32 {
    if x.is_finite() {
        x
    } else {
        0.0
    }
}

impl Plugin for Delay {
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

    fn process(
        &mut self,
        _midi_events: &[(u64, [u8; 3])],
        audio_in: &[&[f32]],
        audio_out: &mut [&mut [f32]],
    ) -> anyhow::Result<()> {
        let frames = audio_out.first().map_or(0, |out| out.len());
        let in_l = audio_in.first().copied();
        let in_r = audio_in.get(1).copied().or(in_l);
        let sample_rate = self.sample_rate;
        let target = f64::from((self.time * sample_rate).clamp(1.0, (self.lines[0].len() - 2) as f32));
        if !self.started {
            self.delay = target;
            self.feedback_now = self.feedback;
            self.pingpong_now = self.pingpong;
            self.started = true;
        }
        let time_coef = 1.0 - (-1.0 / (TIME_SMOOTHING_SEC * f64::from(sample_rate))).exp();
        let gain_coef = 1.0 - (-1.0 / (GAIN_SMOOTHING_SEC * sample_rate)).exp();
        let tone_coef = tone_coefficient(self.tone, sample_rate);

        for frame in 0..frames {
            self.delay += ((target - self.delay) * time_coef).clamp(-MAX_SLOPE, MAX_SLOPE);
            self.feedback_now += (self.feedback - self.feedback_now) * gain_coef;
            self.pingpong_now += (self.pingpong - self.pingpong_now) * gain_coef;

            let delay = self.delay as f32;
            let echo = [read(&self.lines[0], self.write, delay), read(&self.lines[1], self.write, delay)];

            // What is fed back: the echoes, darkened by the tone, and crossed to the other side as far as the ping-pong says.
            let fed = match tone_coef {
                None => echo,
                Some(coef) => {
                    for (state, &x) in self.tone_state.iter_mut().zip(echo.iter()) {
                        *state += coef * (x - *state);
                    }
                    self.tone_state
                }
            };
            let pingpong = self.pingpong_now;
            let back_l = (1.0 - pingpong) * fed[0] + pingpong * fed[1];
            let back_r = (1.0 - pingpong) * fed[1] + pingpong * fed[0];

            // What is put in: each side by itself, and as the ping-pong goes up the right side's goes over to the left as the sum of the two,
            // so that a sound that is the same on both sides starts on the left and goes from there.
            let l = finite(in_l.and_then(|s| s.get(frame)).copied().unwrap_or(0.0));
            let r = finite(in_r.and_then(|s| s.get(frame)).copied().unwrap_or(0.0));
            let mid = 0.5 * (l + r);
            let put_l = (1.0 - pingpong) * l + pingpong * mid;
            let put_r = (1.0 - pingpong) * r;

            // (The soft clip keeps a feedback near 1 from building up past full scale.)
            self.lines[0][self.write] = soft_clip(put_l + self.feedback_now * back_l);
            self.lines[1][self.write] = soft_clip(put_r + self.feedback_now * back_r);
            self.write = (self.write + 1) % self.lines[0].len();

            audio_out[0][frame] = echo[0];
            if let Some(out) = audio_out.get_mut(1) {
                out[frame] = echo[1];
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
                unit: p.unit.map(String::from),
                ..Default::default()
            })
            .collect()
    }

    fn get_parameter(&mut self, index: u32) -> Option<f32> {
        match index {
            0 => Some(self.time),
            1 => Some(self.feedback),
            2 => Some(self.tone),
            3 => Some(self.pingpong),
            _ => None,
        }
    }

    fn set_parameter(&mut self, index: u32, value: f32) -> anyhow::Result<()> {
        let spec = PARAMS.get(index as usize).ok_or_else(|| anyhow::anyhow!("no parameter with index {index}"))?;
        anyhow::ensure!(value.is_finite(), "{} is not a number: {value}", spec.name);
        let v = value.clamp(spec.min, spec.max);
        match index {
            0 => self.time = v,
            1 => self.feedback = v,
            2 => self.tone = v,
            3 => self.pingpong = v,
            _ => unreachable!(),
        }
        Ok(())
    }

    fn presets(&self) -> Vec<Preset> {
        PRESETS.iter().map(|(id, name, _)| Preset { name: (*name).into(), id: (*id).into() }).collect()
    }

    fn load_preset(&mut self, id: &str) -> anyhow::Result<()> {
        let (_, _, values) = PRESETS.iter().find(|(pid, _, _)| *pid == id).ok_or_else(|| anyhow::anyhow!("no preset with id {id:?}"))?;
        [self.time, self.feedback, self.tone, self.pingpong] = *values;
        // A fresh sound: what was echoing is gone, and the time is the preset's from the first sample.
        for line in self.lines.iter_mut() {
            line.fill(0.0);
        }
        self.tone_state = [0.0; 2];
        self.started = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RATE: f32 = 48_000.0;

    /// A delay that has `time` (seconds), `feedback`, `tone` and `pingpong` set, and has not run yet.
    fn delay_with(time: f32, feedback: f32, tone: f32, pingpong: f32) -> Delay {
        let mut delay = Delay::new(SAMPLE_RATE);
        for (index, value) in [time, feedback, tone, pingpong].into_iter().enumerate() {
            delay.set_parameter(index as u32, value).unwrap();
        }
        delay
    }

    /// What the delay puts out on the left and on the right for `left` and `right` put in, in blocks of 256 frames.
    fn run(delay: &mut Delay, left: &[f32], right: &[f32]) -> (Vec<f32>, Vec<f32>) {
        assert_eq!(left.len(), right.len());
        let (mut out_l, mut out_r) = (Vec::new(), Vec::new());
        for (l, r) in left.chunks(256).zip(right.chunks(256)) {
            let (mut block_l, mut block_r) = (vec![0.0; l.len()], vec![0.0; l.len()]);
            let mut out: [&mut [f32]; 2] = [&mut block_l, &mut block_r];
            delay.process(&[], &[l, r], &mut out).unwrap();
            out_l.extend_from_slice(&block_l);
            out_r.extend_from_slice(&block_r);
        }
        (out_l, out_r)
    }

    /// `frames` of silence with `value` in the first.
    fn impulse(value: f32, frames: usize) -> Vec<f32> {
        let mut signal = vec![0.0; frames];
        signal[0] = value;
        signal
    }

    /// The frames of a signal that are not silent, as (frame, value).
    fn heard(signal: &[f32]) -> Vec<(usize, f32)> {
        signal.iter().copied().enumerate().filter(|(_, x)| x.abs() > 1e-4).collect()
    }

    /// 0.0125 s is 600 samples.
    const SIX_HUNDRED: f32 = 0.0125;

    #[test]
    fn an_impulse_comes_back_after_the_time_and_again_after_every_time_by_the_feedback() {
        // (The time is set before the first sample is run: the first echo is at that time, and the delay has not swept to it from the default.)
        let mut delay = delay_with(SIX_HUNDRED, 0.5, 1.0, 0.0);
        let (l, r) = run(&mut delay, &impulse(0.5, 2400), &impulse(0.5, 2400));
        // The echo of the frame is read before the frame's own sample is written, so that the sound put in at frame 0 is heard at frame 600,
        // and nothing of it before: the delay puts out echoes and not the sound.
        for side in [&l, &r] {
            let echoes = heard(side);
            assert_eq!(echoes.iter().map(|e| e.0).collect::<Vec<_>>(), [600, 1200, 1800]);
            for ((_, got), expected) in echoes.iter().zip([0.5, 0.25, 0.125]) {
                assert!((got - expected).abs() < 1e-4, "{echoes:?}");
            }
        }
    }

    #[test]
    fn without_feedback_there_is_one_echo() {
        let mut delay = delay_with(SIX_HUNDRED, 0.0, 1.0, 0.0);
        let (l, _) = run(&mut delay, &impulse(0.5, 2400), &impulse(0.5, 2400));
        let echoes = heard(&l);
        assert_eq!(echoes.len(), 1, "{echoes:?}");
        assert_eq!(echoes[0].0, 600);
    }

    #[test]
    fn a_sound_on_the_left_stays_on_the_left_without_a_ping_pong() {
        let mut delay = delay_with(SIX_HUNDRED, 0.5, 1.0, 0.0);
        let (l, r) = run(&mut delay, &impulse(0.5, 2400), &[0.0; 2400]);
        assert_eq!(heard(&l).iter().map(|e| e.0).collect::<Vec<_>>(), [600, 1200, 1800]);
        assert!(heard(&r).is_empty(), "{:?}", heard(&r));
    }

    #[test]
    fn a_ping_pong_sends_the_echoes_from_one_side_to_the_other_and_starts_on_the_left() {
        let mut delay = delay_with(SIX_HUNDRED, 0.5, 1.0, 1.0);
        // (The same sound on both sides: a mono sound.)
        let (l, r) = run(&mut delay, &impulse(0.5, 2400), &impulse(0.5, 2400));
        let (left, right) = (heard(&l), heard(&r));
        assert_eq!(left.iter().map(|e| e.0).collect::<Vec<_>>(), [600, 1800], "{left:?}");
        assert_eq!(right.iter().map(|e| e.0).collect::<Vec<_>>(), [1200], "{right:?}");
        assert!((left[0].1 - 0.5).abs() < 1e-4 && (right[0].1 - 0.25).abs() < 1e-4 && (left[1].1 - 0.125).abs() < 1e-4);

        // A sound on the right alone goes in as the sum of the two sides (half of it) on the left, where it starts, and not on the right.
        let mut delay = delay_with(SIX_HUNDRED, 0.5, 1.0, 1.0);
        let (l, r) = run(&mut delay, &[0.0; 2400], &impulse(0.5, 2400));
        let (left, right) = (heard(&l), heard(&r));
        assert_eq!(left.iter().map(|e| e.0).collect::<Vec<_>>(), [600, 1800], "{left:?}");
        assert_eq!(right.iter().map(|e| e.0).collect::<Vec<_>>(), [1200], "{right:?}");
        assert!((left[0].1 - 0.25).abs() < 1e-4, "{left:?}");
    }

    #[test]
    fn a_time_that_is_not_a_whole_number_of_samples_puts_the_echo_between_two() {
        // 600.5 samples.
        let mut delay = delay_with(600.5 / SAMPLE_RATE, 0.0, 1.0, 0.0);
        let (l, _) = run(&mut delay, &impulse(0.5, 1200), &impulse(0.5, 1200));
        let echoes = heard(&l);
        assert_eq!(echoes.iter().map(|e| e.0).collect::<Vec<_>>(), [600, 601], "{echoes:?}");
        assert!(echoes.iter().all(|e| (e.1 - 0.25).abs() < 1e-3), "{echoes:?}");
    }

    #[test]
    fn the_longest_time_is_two_seconds_and_the_shortest_a_thousandth_of_one() {
        let mut delay = delay_with(2.0, 0.0, 1.0, 0.0);
        let (l, _) = run(&mut delay, &impulse(0.5, 97_000), &impulse(0.5, 97_000));
        assert_eq!(heard(&l).iter().map(|e| e.0).collect::<Vec<_>>(), [96_000]);
        let mut delay = delay_with(0.001, 0.0, 1.0, 0.0);
        let (l, _) = run(&mut delay, &impulse(0.5, 200), &impulse(0.5, 200));
        assert_eq!(heard(&l).iter().map(|e| e.0).collect::<Vec<_>>(), [48]);
    }

    #[test]
    fn a_change_of_the_feedback_or_of_the_ping_pong_is_gone_through_and_does_not_step_the_echoes() {
        // The change is made 24,037 frames in, where the sound is not at a zero (a step in the gain is a step in the echoes by as much as the
        // sound is then).
        const CHANGE: usize = 24_037;
        let sine = |k: usize| 0.4 * (TAU * 440.0 * k as f32 / SAMPLE_RATE).sin();
        assert!(sine(CHANGE).abs() > 0.3 && sine(CHANGE - 600).abs() > 0.3);
        let steepest = |signal: &[f32]| signal.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
        for (parameter, from, to) in [(1, 0.0, 0.9), (3, 0.0, 1.0), (3, 1.0, 0.0)] {
            let mut delay = delay_with(SIX_HUNDRED, if parameter == 1 { from } else { 0.5 }, 1.0, if parameter == 3 { from } else { 0.0 });
            // A sound on the right alone, whose echoes are on the right while the ping-pong is 0 and on the left when it is 1.
            let right: Vec<f32> = (0..CHANGE).map(sine).collect();
            let (l, r) = run(&mut delay, &[0.0; CHANGE], &right);
            delay.set_parameter(parameter, to).unwrap();
            let right: Vec<f32> = (CHANGE..CHANGE + 24_000).map(sine).collect();
            let (l2, r2) = run(&mut delay, &[0.0; 24_000], &right);
            // (A sine of this pitch and level moves by at most 0.0230 a sample, and a gain going from one value to the other in 5 ms by the
            // difference over 240 samples of the sound, which is under 0.0015 a sample.)
            for (what, out) in [("left", [l, l2].concat()), ("right", [r, r2].concat())] {
                assert!(steepest(&out) < 0.05, "parameter {parameter} from {from} to {to}, {what}: {}", steepest(&out));
            }
            // It is where it was sent within 50 ms.
            let now = if parameter == 1 { delay.feedback_now } else { delay.pingpong_now };
            assert!((now - to).abs() < 1e-3, "parameter {parameter}: {now} for {to}");
        }
    }

    /// The energy of a signal between two frames.
    fn energy(signal: &[f32], from: usize, to: usize) -> f32 {
        signal[from..to].iter().map(|x| x * x).sum()
    }

    #[test]
    fn a_dark_tone_takes_the_highs_out_of_the_repeats_and_leaves_the_first_echo_alone() {
        // A burst at the highest frequency there is, 64 samples of it.
        let burst: Vec<f32> = (0..2400).map(|k| if k < 64 { if k % 2 == 0 { 0.5 } else { -0.5 } } else { 0.0 }).collect();
        let (open, open_right) = run(&mut delay_with(SIX_HUNDRED, 0.9, 1.0, 0.0), &burst, &burst);
        let (dark, dark_right) = run(&mut delay_with(SIX_HUNDRED, 0.9, 0.0, 0.0), &burst, &burst);
        // (The same sound on both sides is the same echoes on both, darkened alike.)
        assert!(open == open_right && dark == dark_right);
        // The first echo is the sound as it was put in, whatever the tone; the second has been round the loop once.
        let (first_open, first_dark) = (energy(&open, 600, 900), energy(&dark, 600, 900));
        assert!(first_open > 10.0 && (first_open - first_dark).abs() < 1e-3 * first_open, "{first_open} {first_dark}");
        let (second_open, second_dark) = (energy(&open, 1200, 1500), energy(&dark, 1200, 1500));
        assert!(second_open > 0.7 * first_open, "an open tone keeps the highs: {second_open}");
        assert!(second_dark < 0.01 * second_open, "a dark tone takes them out: {second_dark} of {second_open}");
    }

    #[test]
    fn a_change_of_the_time_sweeps_the_pitch_of_the_echoes_within_an_octave_and_neither_clicks_nor_holds_a_sample() {
        let mut played = 0;
        let mut play = |delay: &mut Delay, frames: usize| -> Vec<f32> {
            let input: Vec<f32> = (played..played + frames).map(|k| 0.4 * (TAU * 440.0 * k as f32 / SAMPLE_RATE).sin()).collect();
            played += frames;
            run(delay, &input, &input).0
        };
        let crossings = |signal: &[f32]| signal.windows(2).filter(|w| (w[0] < 0.0) != (w[1] < 0.0)).count();
        let steepest = |signal: &[f32]| signal.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);

        let mut delay = delay_with(0.1, 0.0, 1.0, 0.0);
        // A sine of this pitch and level moves by at most 0.0230 from one sample to the next.
        let steady = play(&mut delay, 48_000);
        assert!(steepest(&steady[6000..]) < 0.0235, "{}", steepest(&steady[6000..]));

        // Up to 0.3 s: the delay grows by half a sample for each that goes by, so the echoes are read at half the speed, an octave down (220 Hz),
        // and not held (no crossing of the middle) or stepped.
        delay.set_parameter(0, 0.3).unwrap();
        let down = play(&mut delay, 48_000);
        let middle = crossings(&down[2000..16_000]);
        assert!((110..=145).contains(&middle), "{middle} crossings in 14,000 frames (128 at 220 Hz)");
        assert!(steepest(&down) < 0.0235, "{}", steepest(&down));

        // Back to 0.1 s: the delay shrinks by half a sample for each that goes by, so they are read at one and a half times the speed (660 Hz), a
        // fifth up, which moves a sample from the next by at most 0.0346.
        delay.set_parameter(0, 0.1).unwrap();
        let up = play(&mut delay, 48_000);
        let middle = crossings(&up[2000..16_000]);
        assert!((360..=410).contains(&middle), "{middle} crossings in 14,000 frames (385 at 660 Hz)");
        assert!(steepest(&up[2000..16_000]) > 0.03 && steepest(&up) < 0.036, "{} {}", steepest(&up[2000..16_000]), steepest(&up));

        // And it ends where it was sent: an impulse put in after the sweeps (and after what the delay held of the sine has gone by) is heard
        // 0.1 s later.
        run(&mut delay, &[0.0; 12_000], &[0.0; 12_000]);
        let (settled, _) = run(&mut delay, &impulse(0.5, 12_000), &impulse(0.5, 12_000));
        let echoes = heard(&settled);
        assert_eq!(echoes.len(), 1, "{echoes:?}");
        assert!(echoes[0].0 == 4800 && (echoes[0].1 - 0.5).abs() < 1e-3, "{echoes:?}");
    }

    #[test]
    fn a_sound_that_is_not_a_number_is_not_kept_in_the_echoes() {
        let mut delay = delay_with(SIX_HUNDRED, 0.5, 0.7, 0.3);
        let mut left = impulse(0.5, 96_000);
        left[100] = f32::NAN;
        left[200] = f32::INFINITY;
        left[201] = f32::NEG_INFINITY;
        let right = left.clone();
        let (l, r) = run(&mut delay, &left, &right);
        assert!(l.iter().chain(r.iter()).all(|x| x.is_finite()));
        // Two seconds later it is silent: the echoes of the good sound died away, and there are none of the bad.
        assert!(l[48_000..].iter().chain(r[48_000..].iter()).all(|x| x.abs() < 1e-6));
        assert!((l[600] - 0.5).abs() < 0.2, "the sound that was a number is heard: {}", l[600]);
    }

    #[test]
    fn the_echoes_stay_within_full_scale_at_the_most_feedback_for_a_loud_sound() {
        let mut delay = delay_with(SIX_HUNDRED, 0.95, 1.0, 0.0);
        // (240 Hz is four times in a delay of 0.0125 s: the echoes add up round the loop, as loud as it lets them.)
        let loud: Vec<f32> = (0..144_000).map(|k| (TAU * 240.0 * k as f32 / SAMPLE_RATE).sin()).collect();
        let (l, r) = run(&mut delay, &loud, &loud);
        let peak = l.iter().chain(r.iter()).fold(0.0f32, |m, x| m.max(x.abs()));
        // (Driven into the soft clip, or the bound is not what is tested.)
        assert!(peak > 0.9 && peak <= 1.0 + 1e-6, "{peak}");
    }

    /// `process` runs on the audio thread, where a trip to the allocator can stall the callback: once the delay is made, running it makes none.
    #[test]
    fn running_does_not_allocate() {
        let mut delay = delay_with(0.2, 0.6, 0.5, 0.5);
        let input = vec![0.3f32; 256];
        let (mut out_l, mut out_r) = (vec![0.0f32; 256], vec![0.0f32; 256]);
        let ((), allocations) = crate::alloc_guard::allocations_in(|| {
            for block in 0..400 {
                // (A change of the time on the way, which a modulator makes at every block.)
                delay.set_parameter(0, 0.2 + 0.05 * (block % 7) as f32).unwrap();
                let mut out: [&mut [f32]; 2] = [&mut out_l, &mut out_r];
                delay.process(&[], &[&input, &input], &mut out).unwrap();
            }
        });
        assert_eq!(allocations, 0);
    }

    #[test]
    fn the_parameters_say_what_they_are_and_are_held_to_it() {
        let mut delay = Delay::new(SAMPLE_RATE);
        let parameters = delay.parameters();
        assert_eq!(parameters.len(), PARAM_COUNT);
        let described: Vec<(&str, Option<&str>, f32, f32, f32)> = parameters.iter().map(|p| (p.name.as_str(), p.unit.as_deref(), p.min, p.max, p.default)).collect();
        assert_eq!(
            described,
            [
                ("time", Some("s"), 0.001, 2.0, 0.375),
                ("feedback", None, 0.0, 0.95, 0.45),
                ("tone", None, 0.0, 1.0, 0.7),
                ("pingpong", None, 0.0, 1.0, 0.0),
            ]
        );
        assert!(parameters.iter().all(|p| p.labels.is_none() && p.group.is_empty() && p.step_count == 0));
        for (index, p) in parameters.iter().enumerate() {
            assert_eq!(delay.get_parameter(index as u32), Some(p.default), "{}", p.name);
            delay.set_parameter(index as u32, p.max + 1000.0).unwrap();
            assert_eq!(delay.get_parameter(index as u32), Some(p.max), "{} is held to its top", p.name);
            delay.set_parameter(index as u32, p.min - 1000.0).unwrap();
            assert_eq!(delay.get_parameter(index as u32), Some(p.min), "{} is held to its bottom", p.name);
            delay.set_parameter(index as u32, 0.5 * (p.min + p.max)).unwrap();
            assert_eq!(delay.get_parameter(index as u32), Some(0.5 * (p.min + p.max)), "{}", p.name);
        }
    }

    #[test]
    fn a_value_that_is_not_a_number_and_a_parameter_that_is_not_there_are_refused() {
        let mut delay = Delay::new(SAMPLE_RATE);
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(delay.set_parameter(0, bad).is_err());
            assert_eq!(delay.get_parameter(0), Some(0.375), "a refused value changes nothing");
        }
        assert!(delay.set_parameter(4, 0.5).is_err());
        assert_eq!(delay.get_parameter(4), None);
    }

    #[test]
    fn a_preset_sets_the_parameters_and_the_echoes_that_were_there_are_gone() {
        let mut delay = delay_with(SIX_HUNDRED, 0.5, 1.0, 0.0);
        // An impulse that is still on its way when the preset is loaded.
        run(&mut delay, &impulse(0.5, 256), &impulse(0.5, 256));
        for (id, name, values) in PRESETS {
            delay.load_preset(id).unwrap();
            let got: Vec<f32> = (0..PARAM_COUNT as u32).map(|i| delay.get_parameter(i).unwrap()).collect();
            assert_eq!(got, values, "{name}");
            let (l, r) = run(&mut delay, &[0.0; 48_000], &[0.0; 48_000]);
            assert!(heard(&l).is_empty() && heard(&r).is_empty(), "{name}: nothing is left of what was echoing");
        }
        // What the tone holds goes with the rest: a dark tone has the first echo in it (601 frames in, one frame after it was read) when the
        // preset comes, and it is not fed back into the echoes of the preset.
        let mut delay = delay_with(SIX_HUNDRED, 0.5, 0.5, 0.0);
        run(&mut delay, &impulse(0.5, 601), &impulse(0.5, 601));
        assert!(delay.tone_state.iter().all(|&state| state > 0.1), "{:?}", delay.tone_state);
        delay.load_preset("slapback").unwrap();
        let (l, r) = run(&mut delay, &[0.0; 12_000], &[0.0; 12_000]);
        assert!(heard(&l).is_empty() && heard(&r).is_empty(), "{:?} {:?}", heard(&l), heard(&r));

        // The time of a preset is heard from the first sample: the delay does not sweep to it from where it was (0.0125 s, and the slapback's is 0.09).
        let mut delay = delay_with(SIX_HUNDRED, 0.5, 1.0, 0.0);
        run(&mut delay, &impulse(0.5, 256), &impulse(0.5, 256));
        delay.load_preset("slapback").unwrap();
        let (l, _) = run(&mut delay, &impulse(0.5, 6000), &impulse(0.5, 6000));
        let echoes = heard(&l);
        assert!(echoes.len() == 1 && echoes[0].0 == 4320 && (echoes[0].1 - 0.5).abs() < 1e-3, "{echoes:?}");
        assert_eq!(delay.presets().iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["slapback", "tape", "pingpong", "dub"]);
        assert!(delay.load_preset("no such preset").is_err());
    }
}
