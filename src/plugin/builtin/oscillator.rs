use std::collections::HashMap;
use std::f32::consts::{PI, TAU};

use crate::plugin::{ParameterInfo, Plugin, Preset};

/// Selectable waveform names for the `waveform` enum parameter. Saw is
/// appended (index 3) so the existing sine/triangle/square indices — and the
/// numeric `waveform` values saved in sessions — stay stable, and so is the
/// noise (index 4).
pub const WAVEFORMS: [&str; 5] = ["sine", "triangle", "square", "saw", "noise"];

#[derive(Copy, Clone)]
pub enum Waveform {
    Sine,
    Triangle,
    Square,
    Saw,
    /// White noise: a number from -1 to 1 for every sample, with no cycle (a voice makes them: `Voice::noise`).
    Noise,
}

impl Waveform {
    /// Map the `waveform` parameter value (0–4, rounded) to a waveform.
    fn from_param(v: f32) -> Self {
        match v.round() as i32 {
            1 => Waveform::Triangle,
            2 => Waveform::Square,
            3 => Waveform::Saw,
            4 => Waveform::Noise,
            _ => Waveform::Sine,
        }
    }

    /// The `waveform` parameter value for this waveform.
    fn to_param(self) -> f32 {
        match self {
            Waveform::Sine => 0.0,
            Waveform::Triangle => 1.0,
            Waveform::Square => 2.0,
            Waveform::Saw => 3.0,
            Waveform::Noise => 4.0,
        }
    }

    /// Render one sample for a phase in [0.0, 1.0). `pulse` is how much of the cycle a square is high (the other waveforms have no use for it). The noise has no
    /// cycle to be at a phase of, and a voice makes its numbers (`Voice::noise`): this gives silence for it.
    fn sample(self, phase: f32, pulse: f32) -> f32 {
        match self {
            Waveform::Sine => (2.0 * PI * phase).sin(),
            // Triangle in [-1, 1]: rises 0→1 over [0, 0.25), falls 1→-1 over
            // [0.25, 0.75), rises -1→0 over [0.75, 1.0).
            Waveform::Triangle => 4.0 * (phase - (phase + 0.5).floor()).abs() - 1.0,
            // Naïve square (no anti-aliasing): +1 for the first `pulse` of the cycle, -1 for the rest, with the offset that a pulse that is not
            // a half has taken out (its mean is 2 * pulse - 1) and scaled so that its higher side is at most 1: a half is the square it was.
            Waveform::Square => {
                let offset = 2.0 * pulse - 1.0;
                let scale = (2.0 - 2.0 * pulse).max(2.0 * pulse);
                (if phase < pulse { 1.0 } else { -1.0 } - offset) / scale
            }
            // Naïve rising sawtooth (no anti-aliasing): -1 → +1 across the cycle.
            Waveform::Saw => 2.0 * phase - 1.0,
            Waveform::Noise => 0.0,
        }
    }
}

/// A simple polyphonic oscillator, useful for testing audio/MIDI without
/// external plugins. Each voice has its own ADSR amplitude envelope, so
/// notes attack and release independently.
pub struct Oscillator {
    sample_rate: f32,
    voices: HashMap<u8, Voice>,
    /// Detune in semitones, applied to all voices. Modulator-friendly.
    detune: f32,
    /// Output volume target (0.0–1.0). Modulator-friendly.
    volume: f32,
    /// Smoothed volume actually applied, ramping toward `volume` to avoid
    /// zipper noise when the parameter is tweaked or modulated at block rate.
    volume_smoothed: f32,
    /// Stereo pan target, -1.0 = hard left, 0.0 = center, +1.0 = hard right.
    /// Modulator-friendly.
    pan: f32,
    /// Smoothed pan actually applied (same anti-zipper ramp as `volume`).
    pan_smoothed: f32,
    waveform: Waveform,
    /// How many oscillators sound for each note (a unison, 1 to `MAX_UNISON`), how far the outer ones are from the pitch of the note in
    /// semitones, and how far across the stereo field they are put (0 is all in the middle, 1 puts the outer ones in a channel each).
    unison: usize,
    spread: f32,
    width: f32,
    /// How much of the cycle a square is high.
    pulse: f32,
    /// The FM: how far the phase of the wave is swung, 0 to 1 of `FM_MAX_CYCLES` (and the value that it is smoothed to, as the volume is, so that a modulator that
    /// moves it a block at a time does not click), and the place in `FM_RATIOS` of how fast the sine that swings it goes.
    fm: f32,
    fm_smoothed: f32,
    fm_ratio: usize,
    /// How much of the level of a note is its velocity (0 is none: every note is as loud as the next, as it has always been, and 1 is all of it), and the place in `BEND_RANGES`
    /// of how far the pitch wheel bends a note.
    velocity: f32,
    bend_range: usize,
    /// Where each MIDI channel's pitch wheel is, -1 to 1 (the master channel's, the first, bends every note; one of the others, the notes that began on that channel: MPE), and
    /// whether the sustain pedal is down.
    bend: [f32; 16],
    pedal: bool,
    /// The level of the sine an octave below the note, 0 to 1 (and the value that it is smoothed to, as the volume is), how many times as fast as the note the synced oscillator goes round, 1 to 8, and how far the pitches wander, 0 to 1.
    sub: f32,
    sub_smoothed: f32,
    sync: f32,
    drift: f32,
    /// Per-voice envelope: attack/decay/release in seconds, sustain 0.0–1.0.
    attack: f32,
    decay: f32,
    sustain: f32,
    release: f32,
    /// How many voices have begun (a note that is hit again while it sounds goes on with its voice): the numbers of a noise are made from it, so that each note has its own.
    voices_begun: u32,
}

#[derive(Copy, Clone)]
enum VoiceState {
    Attack,
    Decay,
    Sustain,
    Release,
}

struct Voice {
    /// The place in its cycle of each of the oscillators of the note (the first `Oscillator::unison` of them are in use).
    phases: [f32; MAX_UNISON],
    /// The place in its cycle of the sine that swings the phase of each of them (the FM): it begins at 0 with the note, and goes round whether or not the FM is on.
    fm_phases: [f32; MAX_UNISON],
    /// Where each of them is in the cycle of the oscillator that it is synced to (hard sync: the master's `phases` is the note's own cycle, and this starts again with it); the sound is read here. With a
    /// sync of one it is the master's place, to the bit.
    sync_phases: [f32; MAX_UNISON],
    /// The place in its cycle of the sine an octave below the note (the sub).
    sub_phase: f32,
    /// How far each oscillator has wandered from the pitch of the note, -1 to 1 (the drift), the place it is going to, and how long it has been going there; and the numbers that it picks the places from.
    drift: [f32; MAX_UNISON],
    drift_target: [f32; MAX_UNISON],
    drift_clock: f32,
    drift_rng: u32,
    /// What each oscillator's frequency is multiplied by for its drift (1 when there is none), made once a block.
    drift_ratio: [f32; MAX_UNISON],
    /// The MIDI channel that the note began on (the wheel of that channel bends it, as well as the master channel's), how hard it was hit, 0 to 1, and the part of that it sounds
    /// at now (it goes to it over a few milliseconds when the key is hit again while it sounds), whether the key is down (a key that is up under the pedal is held by the pedal),
    /// and whether it is let go of fast (all sound off).
    channel: u8,
    velocity: f32,
    velocity_now: f32,
    held: bool,
    fast: bool,
    /// Where the random numbers of each oscillator of the note are (xorshift32, never 0): they are the voice's, so that what it sounds like does not depend on which of the
    /// voices of a chord is summed first. Only the noise uses them.
    noise: [u32; MAX_UNISON],
    amp: f32,
    state: VoiceState,
}

impl Voice {
    /// A voice that begins its envelope, with its oscillators spread over the cycle (the first at its beginning, which is where a note
    /// has always begun) so that the oscillators of a unison do not start as one. It is the `begun`th voice, which its noise is made from: the numbers of two voices are
    /// not alike, and a run that has the same notes has the same numbers.
    fn new(begun: u32) -> Self {
        Self {
            phases: std::array::from_fn(|u| (u as f32 * 0.618_034).fract()),
            fm_phases: [0.0; MAX_UNISON],
            sync_phases: std::array::from_fn(|u| (u as f32 * 0.618_034).fract()),
            sub_phase: 0.0,
            drift: [0.0; MAX_UNISON],
            drift_target: [0.0; MAX_UNISON],
            // (So that the first block that finds the voice there gives it its places to go to.)
            drift_clock: DRIFT_HOLD_SEC,
            drift_rng: noise_seed(begun, MAX_UNISON),
            drift_ratio: [1.0; MAX_UNISON],
            channel: 0,
            velocity: 1.0,
            velocity_now: 1.0,
            held: true,
            fast: false,
            noise: std::array::from_fn(|u| noise_seed(begun, u)),
            amp: 0.0,
            state: VoiceState::Attack,
        }
    }

    /// The next place for an oscillator of the note to drift to, from -1 up to (not including) 1 (the voice's own numbers, as the noise's are).
    fn drift_place(&mut self) -> f32 {
        next_unit(&mut self.drift_rng)
    }

    /// The next number of the noise of oscillator `u` of the note, from -1 up to (not including) 1, all equally likely.
    fn noise(&mut self, u: usize) -> f32 {
        next_unit(&mut self.noise[u])
    }
}

/// The next number of a stream (xorshift32: `state` is where it is, and is never 0), from -1 up to (not including) 1, all equally likely.
fn next_unit(state: &mut u32) -> f32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    // (The top 24 bits are all that a 32-bit float holds exactly.)
    (x >> 8) as f32 * (2.0 / 16_777_216.0) - 1.0
}

/// Where the numbers of oscillator `u` of the `begun`th voice start: a hash of the two (Chris Wellons's "lowbias32"), which is never 0 (a state of 0 would stay there).
fn noise_seed(begun: u32, u: usize) -> u32 {
    let mut x = begun.wrapping_mul(MAX_UNISON as u32).wrapping_add(u as u32).wrapping_add(0x9E37_79B9);
    x ^= x >> 16;
    x = x.wrapping_mul(0x7FEB_352D);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846C_A68B);
    x ^= x >> 16;
    if x == 0 { 0x2545_F491 } else { x }
}

// waveform, detune, volume, attack, decay, sustain, release, pan, unison, spread, width, pulse, fm, fm_ratio, sub, sync, drift, velocity, bend
pub const PARAM_COUNT: usize = 19;

/// What the pitch wheel's full deflection is, in semitones each way, for each place in the `bend` list, and the place that the oscillator starts at (two semitones: what a
/// keyboard's wheel is by default). The list is a choice and not a number so that the useful ranges (the 48 that an MPE controller's notes bend by) are a step apart.
const BEND_RANGES: [f32; 9] = [0.0, 1.0, 2.0, 3.0, 5.0, 7.0, 12.0, 24.0, 48.0];
const BEND_DEFAULT: usize = 2;

/// How long a note takes to be let go of by all sound off, whatever the release is set to.
const FAST_RELEASE_SEC: f32 = 0.005;

/// The hard sync's `sync`: how many times as fast as the note the oscillator that is synced to it goes round, from one (which is no sync at all) to eight.
const SYNC_MIN: f32 = 1.0;
const SYNC_MAX: f32 = 8.0;
/// The drift: how far it takes the pitch of an oscillator at the top, in cents each way, how long each one takes to go to the place that it has been given (a time constant) and how long it goes on before it is given
/// another.
const DRIFT_CENTS: f32 = 12.0;
const DRIFT_TAU_SEC: f32 = 0.4;
const DRIFT_HOLD_SEC: f32 = 0.8;

/// What the frequency of the sine that swings the phase of a wave (the FM) is, as a multiple of the wave's own, for each place in the `fm_ratio` list: the whole numbers, which make
/// harmonic sounds, and the family of 1.4 (1.4, 2.8, 5.6, and 3.5 between), whose partials are not harmonics: bells and metal. The labels are the numbers.
pub const FM_RATIOS: [f32; 16] = [0.5, 1.0, 1.4, 1.5, 2.0, 2.5, 2.8, 3.0, 3.5, 4.0, 5.0, 5.6, 6.0, 7.0, 8.0, 12.0];
/// The place in the list of the ratio of one, which is where `fm_ratio` starts.
const FM_DEFAULT_RATIO: usize = 1;
/// How far the phase of the wave is swung at the top of `fm`, in cycles each way: an index of 10 radians, where a sine has partials of more than a hundredth of its level out to the fourteenth on each side.
const FM_MAX_CYCLES: f32 = 10.0 / TAU;

/// The most oscillators that sound for a note.
const MAX_UNISON: usize = 7;
/// The `spread` parameter's top, in semitones.
const SPREAD_MAX: f32 = 1.0;
/// The pulse width's lowest and highest: a pulse that is narrower is a click.
const PULSE_MIN: f32 = 0.05;
const PULSE_MAX: f32 = 0.95;

/// The keys of a keyboard: one voice for each is the most there can be.
const KEYS: usize = 128;

const PAN_MIN: f32 = -1.0;
const PAN_MAX: f32 = 1.0;

// ±2 octaves. Wide enough that a modulator (e.g. a learned pitch bend) can
// produce a dramatic pitch sweep; use the target's depth to dial the amount
// down for subtle detuning.
const DETUNE_MIN: f32 = -24.0;
const DETUNE_MAX: f32 = 24.0;

// Envelope defaults preserve the oscillator's original behavior: ramps just
// long enough to avoid clicks on note start/stop, full sustain.
const DEFAULT_ATTACK_SEC: f32 = 0.004;
const DEFAULT_DECAY_SEC: f32 = 0.1;
const DEFAULT_SUSTAIN: f32 = 1.0;
const DEFAULT_RELEASE_SEC: f32 = 0.012;
/// Envelope times are floored to this when applied, so even a 0 setting
/// keeps the click-guard ramps.
const MIN_RAMP_SEC: f32 = 0.002;
/// Maximum envelope time in seconds (matches the modulator envelope UI).
const ADSR_TIME_MAX: f32 = 10.0;

/// What the unison's spread and width start at: they do nothing until a unison is of more than one oscillator.
const DEFAULT_SPREAD: f32 = 0.15;
const DEFAULT_WIDTH: f32 = 0.5;

// One-pole smoothing time constant for the volume parameter.
const VOLUME_SMOOTH_SEC: f32 = 0.005;

impl Oscillator {
    pub fn new(sample_rate: f32, waveform: Waveform) -> Self {
        Self {
            sample_rate,
            // (Room for every key, the most that can sound: a note-on on the audio thread must not allocate.)
            voices: HashMap::with_capacity(KEYS),
            detune: 0.0,
            volume: 1.0,
            volume_smoothed: 1.0,
            pan: 0.0,
            pan_smoothed: 0.0,
            waveform,
            unison: 1,
            spread: DEFAULT_SPREAD,
            width: DEFAULT_WIDTH,
            pulse: 0.5,
            fm: 0.0,
            fm_smoothed: 0.0,
            fm_ratio: FM_DEFAULT_RATIO,
            velocity: 0.0,
            bend_range: BEND_DEFAULT,
            bend: [0.0; 16],
            pedal: false,
            sub: 0.0,
            sub_smoothed: 0.0,
            sync: SYNC_MIN,
            drift: 0.0,
            attack: DEFAULT_ATTACK_SEC,
            decay: DEFAULT_DECAY_SEC,
            sustain: DEFAULT_SUSTAIN,
            release: DEFAULT_RELEASE_SEC,
            voices_begun: 0,
        }
    }

    fn note_to_freq(note: u8, detune_semitones: f32) -> f32 {
        440.0 * 2.0_f32.powf((note as f32 - 69.0 + detune_semitones) / 12.0)
    }
}

impl Plugin for Oscillator {
    fn name(&self) -> &str {
        "Oscillator"
    }

    fn is_instrument(&self) -> bool {
        true
    }

    fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    fn audio_output_count(&self) -> usize {
        2
    }

    fn audio_input_count(&self) -> usize {
        0
    }

    fn process(
        &mut self,
        midi_events: &[(u64, [u8; 3])],
        _audio_in: &[&[f32]],
        audio_out: &mut [&mut [f32]],
    ) -> anyhow::Result<()> {
        let block_size = audio_out[0].len();

        // Clear output buffers
        for ch in audio_out.iter_mut() {
            for s in ch.iter_mut() {
                *s = 0.0;
            }
        }

        let mut event_idx = 0;
        // Per-frame envelope increments. Attack ramps 0→1 over `attack` sec,
        // decay 1→sustain over `decay` sec, release falls at a 1→0-over-
        // `release`-sec rate. Times are floored to keep ramps click-free.
        let attack_inc = 1.0 / (self.attack.max(MIN_RAMP_SEC) * self.sample_rate);
        let decay_inc =
            (1.0 - self.sustain) / (self.decay.max(MIN_RAMP_SEC) * self.sample_rate);
        let release_inc = 1.0 / (self.release.max(MIN_RAMP_SEC) * self.sample_rate);
        let fast_release_inc = 1.0 / (FAST_RELEASE_SEC * self.sample_rate);
        let volume_alpha = 1.0 - (-1.0 / (VOLUME_SMOOTH_SEC * self.sample_rate)).exp();

        // The oscillators of a unison, spread evenly from -1 to 1 (a single one is in the middle): each one's pitch is that many `spread`s from
        // the note's, each is put that many `width`s across the channels (with the same power wherever it is, which is the level of one in the
        // middle at both channels, so that the width does not change how loud the sum is: not by a balance, which would take it down towards
        // the sides; and none on a single channel), and the sum is brought down by the root of how many there are, which keeps the level of what
        // is not in step.
        let voices = self.unison.clamp(1, MAX_UNISON);
        let mut ratios = [1.0f32; MAX_UNISON];
        let mut gains = [(1.0f32, 1.0f32); MAX_UNISON];
        let width = if audio_out.len() > 1 { self.width } else { 0.0 };
        let norm = 1.0 / (voices as f32).sqrt();
        for u in 0..voices {
            let place = if voices > 1 { 2.0 * u as f32 / (voices - 1) as f32 - 1.0 } else { 0.0 };
            ratios[u] = 2.0f32.powf(self.spread * place / 12.0);
            let across = width * place;
            gains[u] = if across == 0.0 {
                (norm, norm)
            } else {
                let angle = (across + 1.0) * std::f32::consts::FRAC_PI_4;
                (std::f32::consts::SQRT_2 * angle.cos() * norm, std::f32::consts::SQRT_2 * angle.sin() * norm)
            };
        }
        let pulse = self.pulse;
        let fm_ratio = FM_RATIOS[self.fm_ratio.min(FM_RATIOS.len() - 1)];
        let sync = self.sync;

        // The drift, once a block: each oscillator of each voice goes to the place it was given, which is a number of its own voice's, and is given another every so often; what that comes to is
        // a number to multiply its frequency by (1 for no drift, so that a sound with none is the sound there always was).
        let block_seconds = block_size as f32 / self.sample_rate;
        let drift_alpha = 1.0 - (-block_seconds / DRIFT_TAU_SEC).exp();
        let drift_amount = self.drift * DRIFT_CENTS / 1200.0;
        for voice in self.voices.values_mut() {
            if self.drift > 0.0 {
                voice.drift_clock += block_seconds;
                if voice.drift_clock >= DRIFT_HOLD_SEC {
                    voice.drift_clock -= DRIFT_HOLD_SEC;
                    for u in 0..MAX_UNISON {
                        voice.drift_target[u] = voice.drift_place();
                    }
                }
                for u in 0..MAX_UNISON {
                    voice.drift[u] += (voice.drift_target[u] - voice.drift[u]) * drift_alpha;
                    voice.drift_ratio[u] = (drift_amount * voice.drift[u]).exp2();
                }
            } else {
                voice.drift_ratio = [1.0; MAX_UNISON];
            }
        }

        for frame in 0..block_size {
            // The index is gone to as the volume is, and a swing that has gone to nothing is nothing, so that a sound with no FM is the sound that there always was.
            self.fm_smoothed += (self.fm - self.fm_smoothed) * volume_alpha;
            if self.fm == 0.0 && self.fm_smoothed < 1e-5 {
                self.fm_smoothed = 0.0;
            }
            let swing = self.fm_smoothed * FM_MAX_CYCLES;
            self.sub_smoothed += (self.sub - self.sub_smoothed) * volume_alpha;
            if self.sub == 0.0 && self.sub_smoothed < 1e-5 {
                self.sub_smoothed = 0.0;
            }
            let sub_level = self.sub_smoothed;

            // Process MIDI events at this frame
            while event_idx < midi_events.len() && midi_events[event_idx].0 as usize <= frame {
                let [status, note, velocity] = midi_events[event_idx].1;
                let msg_type = status & 0xF0;
                let channel = status & 0x0F;
                match msg_type {
                    0x90 if velocity > 0 => {
                        // Note-on: attack from current amp (0 if new voice, or
                        // the partially-released level on retrigger — avoids a
                        // click when re-hitting a still-decaying note). The key is
                        // down, on this channel and as hard as it was hit.
                        let hit = f32::from(velocity) / 127.0;
                        self.voices
                            .entry(note)
                            .and_modify(|v| {
                                v.state = VoiceState::Attack;
                                v.held = true;
                                v.fast = false;
                                v.channel = channel;
                                v.velocity = hit;
                            })
                            .or_insert_with(|| {
                                self.voices_begun = self.voices_begun.wrapping_add(1);
                                let mut voice = Voice::new(self.voices_begun);
                                voice.channel = channel;
                                voice.velocity = hit;
                                voice.velocity_now = hit;
                                voice
                            });
                    }
                    0x80 | 0x90 => {
                        // The key is up; under the pedal the note goes on sounding until the pedal goes up.
                        if let Some(v) = self.voices.get_mut(&note) {
                            v.held = false;
                            if !self.pedal {
                                v.state = VoiceState::Release;
                            }
                        }
                    }
                    // A controller: `note` is its number and `velocity` its value.
                    0xB0 => match note {
                        // The sustain pedal, down from 64 up. Going up lets go of the notes that only it was holding.
                        64 => {
                            let down = velocity >= 64;
                            if self.pedal && !down {
                                for v in self.voices.values_mut().filter(|v| !v.held) {
                                    v.state = VoiceState::Release;
                                }
                            }
                            self.pedal = down;
                        }
                        // All notes off (123) and all sound off (120): every note is let go of, the pedal's included; the first with the envelope's release, the second quickly.
                        123 | 120 => {
                            for v in self.voices.values_mut() {
                                v.held = false;
                                v.state = VoiceState::Release;
                                v.fast |= note == 120;
                            }
                        }
                        _ => {}
                    },
                    // The pitch wheel of a channel: `note` is the low 7 bits of a number from 0 to 16383 and `velocity` the high 7; the middle of it, 8192, is no bend.
                    0xE0 => {
                        let raw = i32::from(note) | (i32::from(velocity) << 7);
                        self.bend[usize::from(channel)] = ((raw - 8192) as f32 / 8192.0).clamp(-1.0, 1.0);
                    }
                    _ => {}
                }
                event_idx += 1;
            }

            // Render all active voices with fixed per-voice gain.
            // Using a fixed gain avoids amplitude discontinuities when
            // voices are added/removed (dividing by voice count causes
            // existing voices to suddenly change volume).
            const VOICE_GAIN: f32 = 0.25;
            let (mut sample_l, mut sample_r) = (0.0_f32, 0.0_f32);
            for (&note, voice) in self.voices.iter_mut() {
                match voice.state {
                    VoiceState::Attack => {
                        voice.amp += attack_inc;
                        if voice.amp >= 1.0 {
                            voice.amp = 1.0;
                            voice.state = VoiceState::Decay;
                        }
                    }
                    VoiceState::Decay => {
                        voice.amp -= decay_inc;
                        if voice.amp <= self.sustain {
                            voice.amp = self.sustain;
                            voice.state = VoiceState::Sustain;
                        }
                    }
                    VoiceState::Sustain => {
                        // Track the sustain parameter (it may be modulated).
                        voice.amp = self.sustain;
                    }
                    VoiceState::Release => {
                        voice.amp -= if voice.fast { fast_release_inc } else { release_inc };
                        if voice.amp < 0.0 {
                            voice.amp = 0.0;
                        }
                    }
                }

                // The wheels that bend this note: the master channel's, and its own channel's if it is not the master.
                let wheel = self.bend[0] + if voice.channel > 0 { self.bend[usize::from(voice.channel)] } else { 0.0 };
                let freq = Self::note_to_freq(note, self.detune + wheel * BEND_RANGES[self.bend_range.min(BEND_RANGES.len() - 1)]);
                // How loud it sounds: the envelope, and as much of the velocity as is asked for (none of it is 1 exactly).
                voice.velocity_now += (voice.velocity - voice.velocity_now) * volume_alpha;
                let level = voice.amp * ((1.0 - self.velocity) + self.velocity * voice.velocity_now);
                for u in 0..voices {
                    // (The sound is read where the synced oscillator is, which with a sync of one is where the note's cycle is.)
                    let read = voice.sync_phases[u];
                    let raw = match self.waveform {
                        Waveform::Noise => voice.noise(u),
                        waveform if swing > 0.0 => {
                            // The FM: the wave is read where its phase is swung to by a sine of its own pitch's ratio.
                            let phase = read + swing * (TAU * voice.fm_phases[u]).sin();
                            waveform.sample(phase - phase.floor(), pulse)
                        }
                        waveform => waveform.sample(read, pulse),
                    };
                    let heard = raw * level * VOICE_GAIN;
                    sample_l += heard * gains[u].0;
                    sample_r += heard * gains[u].1;
                    let step = freq * ratios[u] * voice.drift_ratio[u] / self.sample_rate;
                    voice.phases[u] += step;
                    voice.sync_phases[u] += step * sync;
                    if voice.phases[u] >= 1.0 {
                        // The note's cycle is over, and the synced oscillator starts again as far into its own as the note is past its end.
                        voice.phases[u] -= 1.0;
                        let started = voice.phases[u] * sync;
                        voice.sync_phases[u] = started - started.floor();
                    } else if voice.sync_phases[u] >= 1.0 {
                        voice.sync_phases[u] -= voice.sync_phases[u].floor();
                    }
                    voice.fm_phases[u] += step * fm_ratio;
                    voice.fm_phases[u] -= voice.fm_phases[u].floor();
                }
                // The sub: a sine an octave below the note, in the middle, which is its own and not one of the unison's (so that the unison does not bring it down).
                if sub_level > 0.0 {
                    let sub_heard = sub_level * (TAU * voice.sub_phase).sin() * level * VOICE_GAIN;
                    sample_l += sub_heard;
                    sample_r += sub_heard;
                }
                voice.sub_phase += freq * 0.5 / self.sample_rate;
                voice.sub_phase -= voice.sub_phase.floor();
            }

            // Drop fully-released voices.
            self.voices
                .retain(|_, v| !(matches!(v.state, VoiceState::Release) && v.amp <= 0.0));

            self.volume_smoothed += (self.volume - self.volume_smoothed) * volume_alpha;
            self.pan_smoothed += (self.pan - self.pan_smoothed) * volume_alpha;
            let (left, right) = (sample_l * self.volume_smoothed, sample_r * self.volume_smoothed);

            // Balance-style pan: center leaves both channels at full (matching
            // the old mono-to-both behavior), panning attenuates the far channel.
            let p = self.pan_smoothed;
            let (left_gain, right_gain) = if p >= 0.0 {
                (1.0 - p, 1.0)
            } else {
                (1.0, 1.0 + p)
            };
            if audio_out.len() > 1 {
                audio_out[0][frame] = left * left_gain;
                audio_out[1][frame] = right * right_gain;
            } else {
                // Mono output: pan collapses to full level, and the voices of a unison have no width (the two sums are the same).
                audio_out[0][frame] = left;
            }
        }

        Ok(())
    }

    fn parameters(&self) -> Vec<ParameterInfo> {
        vec![
            ParameterInfo {
                index: 0,
                name: "waveform".into(),
                min: 0.0,
                max: (WAVEFORMS.len() - 1) as f32,
                default: 0.0,
                labels: Some(WAVEFORMS.iter().map(|s| s.to_string()).collect()),
                step_count: (WAVEFORMS.len() - 1) as u32,
                ..Default::default()
            },
            ParameterInfo {
                index: 1,
                name: "detune".into(),
                min: DETUNE_MIN,
                max: DETUNE_MAX,
                default: 0.0,
                unit: Some("st".into()),
                ..Default::default()
            },
            ParameterInfo {
                index: 2,
                name: "volume".into(),
                min: 0.0,
                max: 1.0,
                default: 1.0,
                ..Default::default()
            },
            ParameterInfo {
                index: 3,
                name: "attack".into(),
                min: 0.0,
                max: ADSR_TIME_MAX,
                default: DEFAULT_ATTACK_SEC,
                unit: Some("s".into()),
                group: vec!["Envelope".into()],
                ..Default::default()
            },
            ParameterInfo {
                index: 4,
                name: "decay".into(),
                min: 0.0,
                max: ADSR_TIME_MAX,
                default: DEFAULT_DECAY_SEC,
                unit: Some("s".into()),
                group: vec!["Envelope".into()],
                ..Default::default()
            },
            ParameterInfo {
                index: 5,
                name: "sustain".into(),
                min: 0.0,
                max: 1.0,
                default: DEFAULT_SUSTAIN,
                group: vec!["Envelope".into()],
                ..Default::default()
            },
            ParameterInfo {
                index: 6,
                name: "release".into(),
                min: 0.0,
                max: ADSR_TIME_MAX,
                default: DEFAULT_RELEASE_SEC,
                unit: Some("s".into()),
                group: vec!["Envelope".into()],
                ..Default::default()
            },
            ParameterInfo {
                index: 7,
                name: "pan".into(),
                min: PAN_MIN,
                max: PAN_MAX,
                default: 0.0,
                ..Default::default()
            },
            ParameterInfo {
                index: 8,
                name: "unison".into(),
                min: 0.0,
                max: (MAX_UNISON - 1) as f32,
                default: 0.0,
                // (Named by how many, and the value is the place in the list: 0 is one oscillator.)
                labels: Some((1..=MAX_UNISON).map(|n| n.to_string()).collect()),
                step_count: (MAX_UNISON - 1) as u32,
                ..Default::default()
            },
            ParameterInfo {
                index: 9,
                name: "spread".into(),
                min: 0.0,
                max: SPREAD_MAX,
                default: DEFAULT_SPREAD,
                unit: Some("st".into()),
                ..Default::default()
            },
            ParameterInfo {
                index: 10,
                name: "width".into(),
                min: 0.0,
                max: 1.0,
                default: DEFAULT_WIDTH,
                ..Default::default()
            },
            ParameterInfo {
                index: 11,
                name: "pulse".into(),
                min: PULSE_MIN,
                max: PULSE_MAX,
                default: 0.5,
                ..Default::default()
            },
            ParameterInfo {
                index: 12,
                name: "fm".into(),
                min: 0.0,
                max: 1.0,
                default: 0.0,
                group: vec!["FM".into()],
                ..Default::default()
            },
            ParameterInfo {
                index: 13,
                name: "fm_ratio".into(),
                min: 0.0,
                max: (FM_RATIOS.len() - 1) as f32,
                default: FM_DEFAULT_RATIO as f32,
                // (Named by the number, and the value is the place in the list.)
                labels: Some(FM_RATIOS.iter().map(|ratio| format!("×{ratio}")).collect()),
                step_count: (FM_RATIOS.len() - 1) as u32,
                group: vec!["FM".into()],
                ..Default::default()
            },
            ParameterInfo {
                index: 14,
                name: "sub".into(),
                min: 0.0,
                max: 1.0,
                default: 0.0,
                group: vec!["Voice".into()],
                ..Default::default()
            },
            ParameterInfo {
                index: 15,
                name: "sync".into(),
                min: SYNC_MIN,
                max: SYNC_MAX,
                default: SYNC_MIN,
                group: vec!["Voice".into()],
                ..Default::default()
            },
            ParameterInfo {
                index: 16,
                name: "drift".into(),
                min: 0.0,
                max: 1.0,
                default: 0.0,
                group: vec!["Voice".into()],
                ..Default::default()
            },
            ParameterInfo {
                index: 17,
                name: "velocity".into(),
                min: 0.0,
                max: 1.0,
                default: 0.0,
                group: vec!["Performance".into()],
                ..Default::default()
            },
            ParameterInfo {
                index: 18,
                name: "bend".into(),
                min: 0.0,
                max: (BEND_RANGES.len() - 1) as f32,
                default: BEND_DEFAULT as f32,
                labels: Some(BEND_RANGES.iter().map(|range| if *range == 0.0 { "off".to_string() } else { format!("±{range}") }).collect()),
                step_count: (BEND_RANGES.len() - 1) as u32,
                group: vec!["Performance".into()],
                ..Default::default()
            },
        ]
    }

    fn get_parameter(&mut self, index: u32) -> Option<f32> {
        match index {
            0 => Some(self.waveform.to_param()),
            1 => Some(self.detune),
            2 => Some(self.volume),
            3 => Some(self.attack),
            4 => Some(self.decay),
            5 => Some(self.sustain),
            6 => Some(self.release),
            7 => Some(self.pan),
            8 => Some((self.unison - 1) as f32),
            9 => Some(self.spread),
            10 => Some(self.width),
            11 => Some(self.pulse),
            12 => Some(self.fm),
            13 => Some(self.fm_ratio as f32),
            14 => Some(self.sub),
            15 => Some(self.sync),
            16 => Some(self.drift),
            17 => Some(self.velocity),
            18 => Some(self.bend_range as f32),
            _ => None,
        }
    }

    fn set_parameter(&mut self, index: u32, value: f32) -> anyhow::Result<()> {
        match index {
            0 => {
                self.waveform = Waveform::from_param(value);
                Ok(())
            }
            1 => {
                self.detune = value.clamp(DETUNE_MIN, DETUNE_MAX);
                Ok(())
            }
            2 => {
                self.volume = value.clamp(0.0, 1.0);
                Ok(())
            }
            3 => {
                self.attack = value.clamp(0.0, ADSR_TIME_MAX);
                Ok(())
            }
            4 => {
                self.decay = value.clamp(0.0, ADSR_TIME_MAX);
                Ok(())
            }
            5 => {
                self.sustain = value.clamp(0.0, 1.0);
                Ok(())
            }
            6 => {
                self.release = value.clamp(0.0, ADSR_TIME_MAX);
                Ok(())
            }
            7 => {
                self.pan = value.clamp(PAN_MIN, PAN_MAX);
                Ok(())
            }
            8 => {
                self.unison = value.round().clamp(0.0, (MAX_UNISON - 1) as f32) as usize + 1;
                Ok(())
            }
            9 => {
                self.spread = value.clamp(0.0, SPREAD_MAX);
                Ok(())
            }
            10 => {
                self.width = value.clamp(0.0, 1.0);
                Ok(())
            }
            11 => {
                self.pulse = value.clamp(PULSE_MIN, PULSE_MAX);
                Ok(())
            }
            12 => {
                self.fm = value.clamp(0.0, 1.0);
                Ok(())
            }
            13 => {
                // (A place in the list: the nearest whole one, held to it.)
                self.fm_ratio = (value.round() as i64).clamp(0, FM_RATIOS.len() as i64 - 1) as usize;
                Ok(())
            }
            14 => {
                self.sub = value.clamp(0.0, 1.0);
                Ok(())
            }
            15 => {
                self.sync = value.clamp(SYNC_MIN, SYNC_MAX);
                Ok(())
            }
            16 => {
                self.drift = value.clamp(0.0, 1.0);
                Ok(())
            }
            17 => {
                self.velocity = value.clamp(0.0, 1.0);
                Ok(())
            }
            18 => {
                self.bend_range = (value.round() as i64).clamp(0, BEND_RANGES.len() as i64 - 1) as usize;
                Ok(())
            }
            _ => anyhow::bail!("no parameter with index {index}"),
        }
    }

    fn presets(&self) -> Vec<Preset> {
        Vec::new()
    }

    fn load_preset(&mut self, id: &str) -> anyhow::Result<()> {
        anyhow::bail!("no preset with id {id:?}")
    }
}

#[cfg(test)]
mod tests {
    use std::f32::consts::TAU;

    use super::*;

    const SAMPLE_RATE: f32 = 48000.0;

    /// Render `frames` of audio with the given MIDI events, returning the
    /// left channel.
    fn render(osc: &mut Oscillator, events: &[(u64, [u8; 3])], frames: usize) -> Vec<f32> {
        let mut left = vec![0.0f32; frames];
        let mut right = vec![0.0f32; frames];
        let mut out: Vec<&mut [f32]> = vec![&mut left, &mut right];
        osc.process(events, &[], &mut out).unwrap();
        left
    }

    /// Peak amplitude of the last quarter of the buffer — past the attack
    /// ramp and volume smoothing, so the level has settled.
    fn settled_peak(samples: &[f32]) -> f32 {
        samples[samples.len() * 3 / 4..]
            .iter()
            .fold(0.0f32, |m, &s| m.max(s.abs()))
    }

    /// `process` runs on the audio thread, where a trip to the allocator can stall the callback: once the oscillator is made, notes
    /// that start, sound and end make none, however many of them there are at once. (Every key of the keyboard at once is the most
    /// there can be, and played twice, so that notes start again where the first ones were.)
    #[test]
    fn notes_that_start_sound_and_end_do_not_allocate() {
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Saw);
        let mut left = vec![0.0f32; 256];
        let mut right = vec![0.0f32; 256];
        let on: Vec<(u64, [u8; 3])> = (0..128u8).map(|note| (0, [0x90, note, 100])).collect();
        let off: Vec<(u64, [u8; 3])> = (0..128u8).map(|note| (0, [0x80, note, 0])).collect();
        let mut block = |osc: &mut Oscillator, events: &[(u64, [u8; 3])]| {
            let mut out: [&mut [f32]; 2] = [&mut left, &mut right];
            osc.process(events, &[], &mut out).unwrap();
        };
        let ((), allocations) = crate::alloc_guard::allocations_in(|| {
            for _ in 0..2 {
                block(&mut osc, &on);
                block(&mut osc, &[]);
                block(&mut osc, &off);
                // The release ends within a few blocks and the voices go.
                for _ in 0..8 {
                    block(&mut osc, &[]);
                }
            }
        });
        assert!(osc.voices.is_empty(), "the notes have ended");
        assert_eq!(allocations, 0);
    }

    #[test]
    fn parameters_match_param_count() {
        let osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        assert_eq!(osc.parameters().len(), PARAM_COUNT);
    }

    #[test]
    fn parameters_in_named_order() {
        // The save/load path resolves params by name, but assert the order
        // so the index-based get/set arms stay aligned with parameters().
        let osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        let params = osc.parameters();
        let names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            ["waveform", "detune", "volume", "attack", "decay", "sustain", "release", "pan", "unison", "spread", "width", "pulse", "fm", "fm_ratio", "sub", "sync", "drift", "velocity", "bend"]
        );
        // The waveform parameter is an enum, and so is the unison (a choice of how many).
        assert!(params[0].labels.is_some() && params[8].labels.is_some());
    }

    #[test]
    fn waveform_param_switches_output() {
        let frames = (SAMPLE_RATE * 0.1) as usize;
        let rms = |s: &[f32]| {
            let tail = &s[s.len() * 3 / 4..];
            (tail.iter().map(|x| x * x).sum::<f32>() / tail.len() as f32).sqrt()
        };
        let mut sine = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        let sine_rms = rms(&render(&mut sine, &NOTE_ON, frames));

        // Same oscillator, switched to square via the parameter.
        let mut sq = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        sq.set_parameter(0, 2.0).unwrap();
        let sq_rms = rms(&render(&mut sq, &NOTE_ON, frames));

        // A square's RMS is ~√2× a sine of the same peak — clearly different.
        assert!(
            sq_rms > sine_rms * 1.2,
            "square should have more energy than sine: square {sq_rms}, sine {sine_rms}"
        );
        assert_eq!(sq.get_parameter(0), Some(2.0));
    }

    #[test]
    fn saw_waveform_selectable() {
        // Saw is index 3 (appended after square) and round-trips.
        assert_eq!(WAVEFORMS[3], "saw");
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        osc.set_parameter(0, 3.0).unwrap();
        assert_eq!(osc.get_parameter(0), Some(3.0));

        // It renders a real signal that swings both positive and negative
        // (a rising ramp), scaled by the per-voice gain (~0.25 peak).
        let frames = (SAMPLE_RATE * 0.1) as usize;
        let out = render(&mut osc, &NOTE_ON, frames);
        let tail = &out[out.len() * 3 / 4..];
        let max = tail.iter().cloned().fold(f32::MIN, f32::max);
        let min = tail.iter().cloned().fold(f32::MAX, f32::min);
        assert!(max > 0.15 && min < -0.15, "saw should swing across the range: [{min}, {max}]");
    }

    #[test]
    fn volume_round_trips_and_clamps() {
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        assert_eq!(osc.get_parameter(2), Some(1.0));
        osc.set_parameter(2, 0.7).unwrap();
        assert_eq!(osc.get_parameter(2), Some(0.7));
        osc.set_parameter(2, 2.5).unwrap();
        assert_eq!(osc.get_parameter(2), Some(1.0));
        osc.set_parameter(2, -1.0).unwrap();
        assert_eq!(osc.get_parameter(2), Some(0.0));
    }

    #[test]
    fn volume_scales_output() {
        let frames = (SAMPLE_RATE * 0.1) as usize;
        let note_on = [(0u64, [0x90, 60, 100])];

        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        let full = settled_peak(&render(&mut osc, &note_on, frames));

        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        osc.set_parameter(2, 0.5).unwrap();
        let half = settled_peak(&render(&mut osc, &note_on, frames));

        assert!(full > 0.2, "expected audible output, got peak {full}");
        let ratio = half / full;
        assert!((ratio - 0.5).abs() < 0.01, "expected ~0.5 ratio, got {ratio}");
    }

    #[test]
    fn volume_zero_silences_output() {
        let frames = (SAMPLE_RATE * 0.1) as usize;
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        osc.set_parameter(2, 0.0).unwrap();
        let peak = settled_peak(&render(&mut osc, &[(0, [0x90, 60, 100])], frames));
        assert!(peak < 1e-3, "expected silence, got peak {peak}");
    }

    #[test]
    fn pan_round_trips_and_clamps() {
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        assert_eq!(osc.get_parameter(7), Some(0.0)); // default center
        osc.set_parameter(7, -0.5).unwrap();
        assert_eq!(osc.get_parameter(7), Some(-0.5));
        osc.set_parameter(7, 5.0).unwrap();
        assert_eq!(osc.get_parameter(7), Some(1.0)); // clamped to hard right
        osc.set_parameter(7, -5.0).unwrap();
        assert_eq!(osc.get_parameter(7), Some(-1.0)); // clamped to hard left
    }

    #[test]
    fn detune_round_trips_and_clamps_to_two_octaves() {
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        assert_eq!(osc.get_parameter(1), Some(0.0)); // default: no detune
        osc.set_parameter(1, 12.0).unwrap();
        assert_eq!(osc.get_parameter(1), Some(12.0)); // an octave up
        osc.set_parameter(1, 100.0).unwrap();
        assert_eq!(osc.get_parameter(1), Some(24.0)); // clamped to +2 octaves
        osc.set_parameter(1, -100.0).unwrap();
        assert_eq!(osc.get_parameter(1), Some(-24.0)); // clamped to −2 octaves
    }

    #[test]
    fn pan_balances_channels() {
        let frames = (SAMPLE_RATE * 0.1) as usize;
        // Settled peak of each channel (past the attack + pan-smoothing ramps).
        let render_lr = |osc: &mut Oscillator| -> (f32, f32) {
            let mut l = vec![0.0f32; frames];
            let mut r = vec![0.0f32; frames];
            let mut out: Vec<&mut [f32]> = vec![&mut l, &mut r];
            osc.process(&NOTE_ON, &[], &mut out).unwrap();
            (settled_peak(&l), settled_peak(&r))
        };

        // Center: both channels equal (matches the old mono-to-both output).
        let mut center = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        let (cl, cr) = render_lr(&mut center);
        assert!(cl > 0.2 && (cl - cr).abs() < 1e-3, "center should be equal: L {cl}, R {cr}");

        // Hard right: left silent, right stays at full (center level).
        let mut right = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        right.set_parameter(7, 1.0).unwrap();
        let (rl, rr) = render_lr(&mut right);
        assert!(rl < 1e-2, "hard right should silence the left channel: {rl}");
        assert!((rr - cr).abs() < 0.02, "hard right keeps the right at full: {rr} vs {cr}");

        // Hard left: mirror image.
        let mut left = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        left.set_parameter(7, -1.0).unwrap();
        let (ll, lr) = render_lr(&mut left);
        assert!(lr < 1e-2, "hard left should silence the right channel: {lr}");
        assert!((ll - cl).abs() < 0.02, "hard left keeps the left at full: {ll} vs {cl}");
    }

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0f32, |m, &s| m.max(s.abs()))
    }

    const NOTE_ON: [(u64, [u8; 3]); 1] = [(0, [0x90, 60, 100])];
    const NOTE_OFF: [(u64, [u8; 3]); 1] = [(0, [0x80, 60, 0])];

    #[test]
    fn adsr_defaults_match_legacy_ramps() {
        // Default envelope = the original click-guard ramps: full level
        // within ~10ms of note-on, silent within ~25ms of note-off.
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        let on = render(&mut osc, &NOTE_ON, (SAMPLE_RATE * 0.05) as usize);
        assert!(settled_peak(&on) > 0.2, "expected full level after note-on");

        let off = render(&mut osc, &NOTE_OFF, (SAMPLE_RATE * 0.05) as usize);
        assert!(
            settled_peak(&off) < 1e-3,
            "expected silence shortly after note-off, got {}",
            settled_peak(&off)
        );
    }

    #[test]
    fn adsr_attack_ramps_slowly() {
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        osc.set_parameter(3, 0.2).unwrap(); // 200ms attack
        let early = render(&mut osc, &NOTE_ON, (SAMPLE_RATE * 0.02) as usize);
        let later = render(&mut osc, &[], (SAMPLE_RATE * 0.3) as usize);
        let early_peak = peak(&early);
        let full_peak = settled_peak(&later);
        assert!(full_peak > 0.2, "expected full level eventually, got {full_peak}");
        assert!(
            early_peak < 0.15 * full_peak,
            "20ms into a 200ms attack should still be quiet: early {early_peak}, full {full_peak}"
        );
    }

    #[test]
    fn adsr_release_rings_after_note_off() {
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        osc.set_parameter(6, 0.5).unwrap(); // 500ms release
        let on = render(&mut osc, &NOTE_ON, (SAMPLE_RATE * 0.05) as usize);
        let full_peak = settled_peak(&on);

        // 200ms after note-off the voice must still be sounding (linear
        // release from 1.0 puts it at ~0.6 of full level).
        let tail = render(&mut osc, &NOTE_OFF, (SAMPLE_RATE * 0.2) as usize);
        let tail_end = settled_peak(&tail);
        assert!(
            tail_end > 0.3 * full_peak,
            "release tail should still ring at 200ms: tail {tail_end}, full {full_peak}"
        );

        // Well past the 500ms release it must be silent.
        let done = render(&mut osc, &[], (SAMPLE_RATE * 0.5) as usize);
        assert!(
            settled_peak(&done) < 1e-3,
            "expected silence after release completes, got {}",
            settled_peak(&done)
        );
    }

    #[test]
    fn adsr_sustain_sets_held_level() {
        let frames = (SAMPLE_RATE * 0.2) as usize;

        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        let full = settled_peak(&render(&mut osc, &NOTE_ON, frames));

        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        osc.set_parameter(4, 0.02).unwrap(); // quick decay
        osc.set_parameter(5, 0.5).unwrap(); // sustain at half
        let half = settled_peak(&render(&mut osc, &NOTE_ON, frames));

        let ratio = half / full;
        assert!(
            (ratio - 0.5).abs() < 0.02,
            "expected sustain at ~0.5 of full level, got ratio {ratio}"
        );
    }

    // What the oscillator rendered before it had a unison and a pulse width, for a chord of three notes of which one is let go of a block in and
    // two are at the end (a note-on at frames 0, 100 and 200, then let go of at 1074, 3072 and 3092): the left channel's sum and its sum weighted
    // by the frame, and ten of its samples. The new parameters start at what changes nothing, so that a session that was made before them sounds as it did.
    // (A chord is summed in the order of a hash map, which is not the same from one run to the next, so the numbers are compared to a millionth.)
    const BEFORE: [(Waveform, f64, f64, [f32; 10]); 4] = [
        (Waveform::Sine, 0.34626, -1277.736, [0.0, 0.0000892, 0.0002673, -0.1238491, 0.0672553, -0.1482764, 0.0137012, -0.422_891, -0.3707744, 0.0]),
        (Waveform::Triangle, 1.80225, 969.254, [-0.0013021, -0.0025474, -0.0037359, -0.0283506, -0.3617573, 0.291_47, 0.0944284, 0.0905470, -0.0759250, 0.0]),
        (Waveform::Square, 1.12672, -1763.233, [0.0013021, 0.0026042, 0.0039062, -0.1302082, 0.25, -0.25, -0.0646708, -0.5, -0.4878473, 0.0]),
        (Waveform::Saw, -0.49097, 1103.494, [-0.0013021, -0.0025758, -0.0038211, 0.1040787, -0.2404989, 0.0677222, -0.0890651, 0.2047265, 0.2818861, 0.0]),
    ];

    /// The chord of `BEFORE`, rendered in four blocks of 1024 frames: both channels.
    fn chord(osc: &mut Oscillator) -> (Vec<f32>, Vec<f32>) {
        let (mut left, mut right) = (vec![0.0f32; 4096], vec![0.0f32; 4096]);
        let blocks: [Vec<(u64, [u8; 3])>; 4] = [
            vec![(0, [0x90, 60, 100]), (100, [0x90, 64, 100]), (200, [0x90, 67, 90])],
            vec![(50, [0x80, 64, 0])],
            vec![],
            vec![(10, [0x80, 60, 0]), (20, [0x80, 67, 0])],
        ];
        for (block, events) in blocks.iter().enumerate() {
            let span = block * 1024..(block + 1) * 1024;
            let mut out: [&mut [f32]; 2] = [&mut left[span.clone()], &mut right[span]];
            osc.process(events, &[], &mut out).unwrap();
        }
        (left, right)
    }

    #[test]
    fn the_oscillator_renders_what_it_did_before_it_had_a_unison_and_a_pulse_width() {
        for (waveform, sum, weighted, picks) in BEFORE {
            let (left, right) = chord(&mut Oscillator::new(SAMPLE_RATE, waveform));
            let name = waveform.to_param();
            assert_eq!(left, right, "waveform {name}: a centred sound is the same on both sides");
            assert!((left.iter().map(|&x| f64::from(x)).sum::<f64>() - sum).abs() < 1e-3, "waveform {name}: the sum");
            assert!((left.iter().enumerate().map(|(i, &x)| f64::from(x) * (i as f64 + 1.0)).sum::<f64>() - weighted).abs() < 0.05, "waveform {name}: the weighted sum");
            for (&frame, expected) in [0usize, 1, 2, 150, 700, 1023, 1500, 2500, 3100, 4000].iter().zip(picks) {
                assert!((left[frame] - expected).abs() < 1e-5, "waveform {name}, frame {frame}: {} for {expected}", left[frame]);
            }
        }
    }

    /// A note that is 480 Hz, so that a cycle is a hundred frames: the detune that makes the A of 440 Hz that is.
    fn hundred_frame_cycle(waveform: Waveform) -> Oscillator {
        let mut osc = Oscillator::new(SAMPLE_RATE, waveform);
        osc.set_parameter(1, 12.0 * (480.0f32 / 440.0).log2()).unwrap();
        osc
    }

    #[test]
    fn the_pulse_width_is_how_much_of_the_cycle_of_a_square_is_high_and_it_has_no_offset() {
        for (width, high, low) in [(0.5, 0.25, -0.25), (0.25, 0.25, -0.25 / 3.0), (0.1, 0.25, -0.25 / 9.0), (0.75, 0.25 / 3.0, -0.25), (0.9, 0.25 / 9.0, -0.25)] {
            let mut osc = hundred_frame_cycle(Waveform::Square);
            osc.set_parameter(11, width).unwrap();
            let left = render(&mut osc, &[(0, [0x90, 69, 100])], 6000);
            // A hundred cycles, past the attack: the share of frames that are high, the mean (none) and the two levels.
            let settled = &left[2000..5000];
            let share = settled.iter().filter(|&&x| x > 0.0).count() as f32 / settled.len() as f32;
            let mean = settled.iter().sum::<f32>() / settled.len() as f32;
            let top = settled.iter().fold(f32::MIN, |m, &x| m.max(x));
            let bottom = settled.iter().fold(f32::MAX, |m, &x| m.min(x));
            assert!((share - width).abs() < 0.02, "pulse {width}: {share} of the cycle is high");
            assert!(mean.abs() < 0.003, "pulse {width}: the mean is {mean}, and a pulse has none");
            assert!((top - high).abs() < 1e-3 && (bottom - low).abs() < 1e-3, "pulse {width}: {bottom} to {top}");
        }
    }

    #[test]
    fn the_pulse_width_is_the_square_s_alone() {
        for waveform in [Waveform::Sine, Waveform::Triangle, Waveform::Saw] {
            let mut narrow = Oscillator::new(SAMPLE_RATE, waveform);
            narrow.set_parameter(11, 0.1).unwrap();
            let mut plain = Oscillator::new(SAMPLE_RATE, waveform);
            let events = [(0u64, [0x90u8, 60, 100])];
            assert_eq!(render(&mut narrow, &events, 2000), render(&mut plain, &events, 2000), "waveform {}", waveform.to_param());
        }
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
    fn a_unison_of_three_is_three_voices_a_spread_apart_each_a_third_of_the_power() {
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        osc.set_parameter(8, 2.0).unwrap(); // three voices
        osc.set_parameter(9, 1.0).unwrap(); // a semitone each way
        osc.set_parameter(10, 0.0).unwrap();
        let left = render(&mut osc, &[(0, [0x90, 69, 100])], 24_000);
        let settled = &left[4000..];
        // Each voice is a sine of 0.25 (the voice's gain) over the root of 3, whole cycles or not.
        let each = 0.25 / 3.0f32.sqrt();
        for (name, hz) in [("below", 440.0 * 2.0f32.powf(-1.0 / 12.0)), ("in the middle", 440.0), ("above", 440.0 * 2.0f32.powf(1.0 / 12.0))] {
            let heard = magnitude(settled, hz);
            assert!((heard - each).abs() < 0.1 * each, "the voice {name} ({hz} Hz) is {heard} and should be {each}");
        }
        // And nothing between them: half a semitone up is not a voice.
        assert!(magnitude(settled, 440.0 * 2.0f32.powf(0.5 / 12.0)) < 0.2 * each);
    }

    #[test]
    fn seven_is_the_most_voices_of_a_unison_and_every_one_of_them_sounds() {
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        osc.set_parameter(8, 6.0).unwrap();
        osc.set_parameter(9, 1.0).unwrap();
        osc.set_parameter(10, 0.0).unwrap();
        let left = render(&mut osc, &[(0, [0x90, 69, 100])], 48_000);
        let each = 0.25 / 7.0f32.sqrt();
        // Seven voices from a semitone below to a semitone above, a third of a semitone apart: the one in the middle is the note.
        for place in -3..=3 {
            let hz = 440.0 * 2.0f32.powf(place as f32 / 3.0 / 12.0);
            let heard = magnitude(&left[8000..], hz);
            assert!((heard - each).abs() < 0.15 * each, "voice {place} ({hz} Hz) is {heard} and should be {each}");
        }
    }

    #[test]
    fn the_lowest_voice_of_a_unison_is_on_the_left_and_the_highest_on_the_right() {
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        osc.set_parameter(8, 1.0).unwrap(); // two voices
        osc.set_parameter(9, 1.0).unwrap();
        osc.set_parameter(10, 1.0).unwrap();
        let (mut left, mut right) = (vec![0.0f32; 24_000], vec![0.0f32; 24_000]);
        let mut out: [&mut [f32]; 2] = [&mut left, &mut right];
        osc.process(&[(0, [0x90, 69, 100])], &[], &mut out).unwrap();
        let (below, above) = (440.0 * 2.0f32.powf(-1.0 / 12.0), 440.0 * 2.0f32.powf(1.0 / 12.0));
        let each = 0.25 * std::f32::consts::SQRT_2 / 2.0f32.sqrt();
        // (Each is all in its channel, at full width, and at the level of one voice: the root of two for the width's power and the root of two
        // of the voices cancel.)
        assert!((magnitude(&left[4000..], below) - each).abs() < 0.1 * each && magnitude(&left[4000..], above) < 0.05 * each);
        assert!((magnitude(&right[4000..], above) - each).abs() < 0.1 * each && magnitude(&right[4000..], below) < 0.05 * each);
    }

    #[test]
    fn a_unison_of_one_has_no_spread_and_no_width() {
        let mut plain = Oscillator::new(SAMPLE_RATE, Waveform::Saw);
        let mut spread = Oscillator::new(SAMPLE_RATE, Waveform::Saw);
        spread.set_parameter(9, 1.0).unwrap();
        spread.set_parameter(10, 1.0).unwrap();
        let (left, right) = chord(&mut plain);
        let (spread_left, spread_right) = chord(&mut spread);
        // (A chord is summed in the order of a hash map, which is not the same from one oscillator to the next: the sums are the same to a millionth.)
        assert!(left.iter().zip(&spread_left).chain(right.iter().zip(&spread_right)).all(|(a, b)| (a - b).abs() < 1e-6));
    }

    #[test]
    fn a_unison_keeps_about_the_level_of_one_voice() {
        let rms = |signal: &[f32]| (signal.iter().map(|x| x * x).sum::<f32>() / signal.len() as f32).sqrt();
        let mut one = Oscillator::new(SAMPLE_RATE, Waveform::Saw);
        let single = rms(&render(&mut one, &[(0, [0x90, 60, 100])], 48_000)[8000..]);
        for voices in [2.0, 4.0, 6.0] {
            let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Saw);
            osc.set_parameter(8, voices).unwrap();
            osc.set_parameter(9, 0.3).unwrap();
            let level = rms(&render(&mut osc, &[(0, [0x90, 60, 100])], 48_000)[8000..]);
            // (Voices that are a fraction of a semitone apart are in step for a good part of a second, so the sum is a little over the root: one
            // that was not brought down would be 2.6 times the level of one at seven, and one that were brought down by the number, 0.38.)
            assert!(level > 0.8 * single && level < 1.25 * single, "{} voices: {level} for {single}", voices + 1.0);
        }
    }

    #[test]
    fn the_width_puts_the_voices_of_a_unison_across_the_channels() {
        let render_both = |width: f32| {
            let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Saw);
            osc.set_parameter(8, 2.0).unwrap();
            osc.set_parameter(9, 0.3).unwrap();
            osc.set_parameter(10, width).unwrap();
            let (mut left, mut right) = (vec![0.0f32; 12_000], vec![0.0f32; 12_000]);
            let mut out: [&mut [f32]; 2] = [&mut left, &mut right];
            osc.process(&[(0, [0x90, 60, 100])], &[], &mut out).unwrap();
            (left, right)
        };
        let (left, right) = render_both(0.0);
        assert_eq!(left, right, "no width: every voice in the middle");
        // With the whole width the first voice is on the left alone and the last on the right alone, the middle one on both.
        let (left, right) = render_both(1.0);
        assert_ne!(left, right);
        let correlation = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>() / (a.iter().map(|x| x * x).sum::<f32>() * b.iter().map(|x| x * x).sum::<f32>()).sqrt();
        assert!(correlation(&left[4000..], &right[4000..]) < 0.9, "{}", correlation(&left[4000..], &right[4000..]));
    }

    #[test]
    fn the_width_does_not_change_how_loud_a_channel_is() {
        // A balance would take the voices that are put at the side out of the channel at the other side; the width is of the same power wherever
        // the voices are, so that a channel has the level of one voice, with no width and with all of it.
        let rms_of_left = |width: f32| {
            let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Saw);
            osc.set_parameter(8, 4.0).unwrap();
            osc.set_parameter(9, 0.5).unwrap();
            osc.set_parameter(10, width).unwrap();
            let left = render(&mut osc, &[(0, [0x90, 60, 100])], 48_000);
            (left[8000..].iter().map(|x| x * x).sum::<f32>() / 40_000.0).sqrt()
        };
        let ratio = rms_of_left(1.0) / rms_of_left(0.0);
        assert!(ratio > 0.9 && ratio < 1.1, "{ratio}");
    }

    #[test]
    fn a_unison_has_the_voices_of_a_note_apart_in_phase_so_that_they_do_not_start_as_one() {
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Saw);
        osc.set_parameter(8, 2.0).unwrap();
        osc.set_parameter(9, 0.0).unwrap();
        // With no spread the three voices have the same pitch, and if they began together they would be one voice three times as loud.
        let mut single = Oscillator::new(SAMPLE_RATE, Waveform::Saw);
        let with = render(&mut osc, &[(0, [0x90, 60, 100])], 12_000);
        let without = render(&mut single, &[(0, [0x90, 60, 100])], 12_000);
        let peak = |signal: &[f32]| signal[4000..].iter().fold(0.0f32, |m, &x| m.max(x.abs()));
        assert!(peak(&with) < 0.8 * 3.0f32.sqrt() * peak(&without), "{} against {}", peak(&with), peak(&without));
    }

    #[test]
    fn a_unison_does_not_allocate_and_a_mono_output_is_the_sum_of_the_voices() {
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Saw);
        osc.set_parameter(8, 6.0).unwrap();
        osc.set_parameter(9, 0.5).unwrap();
        osc.set_parameter(10, 1.0).unwrap();
        let (mut left, mut right) = (vec![0.0f32; 256], vec![0.0f32; 256]);
        let on: Vec<(u64, [u8; 3])> = (0..128u8).map(|note| (0, [0x90, note, 100])).collect();
        let off: Vec<(u64, [u8; 3])> = (0..128u8).map(|note| (0, [0x80, note, 0])).collect();
        let ((), allocations) = crate::alloc_guard::allocations_in(|| {
            for events in [&on[..], &[], &off[..], &[]] {
                let mut out: [&mut [f32]; 2] = [&mut left, &mut right];
                osc.process(events, &[], &mut out).unwrap();
            }
        });
        assert_eq!(allocations, 0);
        // On one channel there is nothing to put the voices across: it is all of them, with no width.
        let mut mono = Oscillator::new(SAMPLE_RATE, Waveform::Saw);
        mono.set_parameter(8, 2.0).unwrap();
        mono.set_parameter(9, 0.3).unwrap();
        mono.set_parameter(10, 1.0).unwrap();
        let mut only = vec![0.0f32; 2000];
        mono.process(&[(0, [0x90, 60, 100])], &[], &mut [&mut only[..]]).unwrap();
        let mut centred = Oscillator::new(SAMPLE_RATE, Waveform::Saw);
        centred.set_parameter(8, 2.0).unwrap();
        centred.set_parameter(9, 0.3).unwrap();
        centred.set_parameter(10, 0.0).unwrap();
        assert_eq!(only, render(&mut centred, &[(0, [0x90, 60, 100])], 2000));
    }

    // ---------------------------------------------------------------------------------------------------------------------------------
    // The noise: the fifth waveform
    // ---------------------------------------------------------------------------------------------------------------------------------

    /// An oscillator whose waveform is the noise, the fifth value of the `waveform` parameter.
    fn noise() -> Oscillator {
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        osc.set_parameter(0, 4.0).unwrap();
        osc
    }

    /// What is left of `samples` when the first quarter, where the note starts and the volume settles, has gone.
    fn settled(samples: &[f32]) -> Vec<f64> {
        samples[samples.len() / 4..].iter().map(|&x| f64::from(x)).collect()
    }

    fn mean(x: &[f64]) -> f64 {
        x.iter().sum::<f64>() / x.len() as f64
    }

    fn rms(x: &[f64]) -> f64 {
        (x.iter().map(|v| v * v).sum::<f64>() / x.len() as f64).sqrt()
    }

    /// How much of `x` is in `x` again `lag` samples on, about its mean: 0 for numbers that have nothing to do with each other, 1 for a signal that repeats.
    fn autocorrelation(x: &[f64], lag: usize) -> f64 {
        let m = mean(x);
        let top: f64 = (0..x.len() - lag).map(|i| (x[i] - m) * (x[i + lag] - m)).sum();
        let bottom: f64 = x.iter().map(|v| (v - m) * (v - m)).sum();
        top / bottom
    }

    #[test]
    fn the_noise_is_the_fifth_waveform_and_the_four_before_it_are_where_they_were() {
        assert_eq!(WAVEFORMS.to_vec(), ["sine", "triangle", "square", "saw", "noise"]);
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        let waveform = osc.parameters().remove(0);
        assert_eq!((waveform.min, waveform.max, waveform.step_count, waveform.labels.map(|labels| labels.len())), (0.0, 4.0, 4, Some(5)));
        // (A session file has the number: saw is 3, as it always was, and the noise 4.)
        for (index, name) in WAVEFORMS.iter().enumerate() {
            osc.set_parameter(0, index as f32).unwrap();
            assert_eq!(osc.get_parameter(0), Some(index as f32), "{name}");
        }
    }

    #[test]
    fn the_noise_has_numbers_that_have_nothing_to_do_with_each_other_and_the_level_of_a_saw() {
        let frames = SAMPLE_RATE as usize / 2;
        let out = settled(&render(&mut noise(), &NOTE_ON, frames));
        // A voice is a quarter of the numbers, from -1 to 1: they are as far from their mean as a saw's are (a third of the square of the range).
        let level = 0.25 / 3.0f64.sqrt();
        assert!(mean(&out).abs() < 0.01, "no lean to one side: {}", mean(&out));
        assert!((rms(&out) / level - 1.0).abs() < 0.03, "as loud as a saw (rms {}, a saw's {level})", rms(&out));
        assert!(out.iter().all(|x| x.abs() <= 0.25), "no number is out of the range of a voice");
        let saw = settled(&render(&mut Oscillator::new(SAMPLE_RATE, Waveform::Saw), &NOTE_ON, frames));
        assert!((rms(&out) / rms(&saw) - 1.0).abs() < 0.03, "{} and a saw's {}", rms(&out), rms(&saw));
        // Nothing in a number says what the one before it was: no lag has more than chance in it, which is a hundredth or so for these numbers.
        for lag in 1..=64 {
            assert!(autocorrelation(&out, lag).abs() < 0.04, "lag {lag}: {}", autocorrelation(&out, lag));
        }
    }

    #[test]
    fn the_noise_is_as_loud_in_the_bass_as_it_is_in_the_treble() {
        let out = settled(&render(&mut noise(), &NOTE_ON, SAMPLE_RATE as usize));
        let signal: Vec<f32> = out.iter().map(|&x| x as f32).collect();
        // The power of forty sines of each band that are not harmonics of each other (a number of the Goertzel's is as likely to be far from the average as near it, so the
        // average of forty has to be taken).
        let band = |from: f32, to: f32| -> f64 {
            let powers: Vec<f64> = (0..40).map(|i| f64::from(magnitude(&signal, from + (to - from) * (i as f32 + 0.37) / 40.0)).powi(2)).collect();
            mean(&powers)
        };
        let (low, high) = (band(100.0, 1_000.0), band(10_000.0, 20_000.0));
        assert!(low / high > 0.6 && low / high < 1.6, "as much power in the low band as in the high one: {low} and {high}");
    }

    #[test]
    fn the_noise_has_no_pitch_and_the_note_does_not_change_it() {
        for note in [24u8, 60, 108] {
            let out = settled(&render(&mut noise(), &[(0, [0x90, note, 100])], SAMPLE_RATE as usize / 2));
            assert!((rms(&out) / (0.25 / 3.0f64.sqrt()) - 1.0).abs() < 0.03, "note {note}: {}", rms(&out));
            // Nothing comes round again at the period of the note.
            let period = (SAMPLE_RATE / Oscillator::note_to_freq(note, 0.0)).round() as usize;
            assert!(autocorrelation(&out, period).abs() < 0.04, "note {note}, period {period}: {}", autocorrelation(&out, period));
        }
    }

    /// `process` is summed in the order of a hash map, which is not the same from one oscillator to the next, so what makes the noise of a voice must not be shared by the
    /// voices. (The notes begin at frames of their own, so that the voices are at different places of their attacks, which makes the numbers of one voice louder than another's:
    /// numbers that were dealt out in the order of the map would not be the same sum.)
    #[test]
    fn the_noise_of_a_chord_is_the_same_whatever_the_order_that_its_voices_are_summed_in() {
        for notes in [[48u8, 52, 55, 59, 62, 65, 69], [30, 41, 53, 64, 70, 82, 95], [60, 61, 62, 63, 64, 65, 66]] {
            let events: Vec<(u64, [u8; 3])> = notes.iter().zip([0u64, 37, 91, 160, 250, 333, 400]).map(|(&note, frame)| (frame, [0x90, note, 100])).collect();
            for unison in [0.0, 2.0] {
                let chord = || {
                    let mut osc = noise();
                    osc.set_parameter(8, unison).unwrap();
                    let (mut left, mut right) = (vec![0.0f32; 4096], vec![0.0f32; 4096]);
                    osc.process(&events, &[], &mut [&mut left[..], &mut right[..]]).unwrap();
                    (left, right)
                };
                let ((first_left, first_right), (second_left, second_right)) = (chord(), chord());
                assert!(first_left.iter().zip(&second_left).all(|(a, b)| (a - b).abs() < 1e-5), "{notes:?}, unison {unison}");
                assert!(first_right.iter().zip(&second_right).all(|(a, b)| (a - b).abs() < 1e-5), "{notes:?}, unison {unison}");
            }
            // (And it is a chord of noises, not one voice's: the notes are not the same numbers.)
            let (chord, one) = (render(&mut noise(), &events, 4096), render(&mut noise(), &events[..1], 4096));
            assert!(chord.iter().zip(&one).any(|(a, b)| (a - b).abs() > 0.01), "{notes:?}");
        }
    }

    #[test]
    fn a_key_that_is_hit_again_has_noise_of_its_own() {
        let mut osc = noise();
        let hit = |osc: &mut Oscillator| {
            let mut out = render(osc, &[(0, [0x90, 60, 100]), (1500, [0x80, 60, 0])], 2048);
            // (The release ends and the voice goes, so that the next hit is a new voice.)
            render(osc, &[], 2048);
            out.drain(..400);
            out.truncate(1000);
            out.iter().map(|&x| f64::from(x)).collect::<Vec<f64>>()
        };
        let (first, second) = (hit(&mut osc), hit(&mut osc));
        assert!(osc.voices.is_empty());
        let m = (mean(&first), mean(&second));
        let together: f64 = first.iter().zip(&second).map(|(a, b)| (a - m.0) * (b - m.1)).sum::<f64>() / (rms(&first) * rms(&second) * first.len() as f64);
        assert!(together.abs() < 0.1, "the two hits are alike by {together}");
    }

    #[test]
    fn a_unison_of_noise_has_the_level_of_one_voice_and_every_voice_has_noise_of_its_own() {
        let (mut left, mut right) = (vec![0.0f32; 24_000], vec![0.0f32; 24_000]);
        let mut seven = noise();
        seven.set_parameter(8, 6.0).unwrap();
        seven.set_parameter(10, 1.0).unwrap();
        seven.process(&NOTE_ON, &[], &mut [&mut left[..], &mut right[..]]).unwrap();
        let (left, right) = (settled(&left), settled(&right));
        let one = settled(&render(&mut noise(), &NOTE_ON, 24_000));
        assert!((rms(&left) / rms(&one) - 1.0).abs() < 0.06, "the level of a voice: {} and {}", rms(&left), rms(&one));
        // The lowest voice is on the left and the highest on the right, and the ones between are across: the two sides are not the same numbers, though they are of the same sum.
        let m = (mean(&left), mean(&right));
        let across: f64 = left.iter().zip(&right).map(|(a, b)| (a - m.0) * (b - m.1)).sum::<f64>() / (rms(&left) * rms(&right) * left.len() as f64);
        assert!(across.abs() < 0.9, "the two sides are not one signal: {across}");
        // (And a single voice is in the middle, the same numbers on both sides, as a waveform is.)
        let (mut a, mut b) = (vec![0.0f32; 4096], vec![0.0f32; 4096]);
        noise().process(&NOTE_ON, &[], &mut [&mut a[..], &mut b[..]]).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn the_envelope_shapes_the_noise_as_it_shapes_a_wave() {
        // Attack: a long one is quiet at the start. Release: the noise goes down to nothing after the note.
        let mut slow = noise();
        slow.set_parameter(3, 0.5).unwrap();
        let out = render(&mut slow, &NOTE_ON, SAMPLE_RATE as usize);
        let early = rms(&out[..480].iter().map(|&x| f64::from(x)).collect::<Vec<_>>());
        let late = rms(&out[out.len() * 3 / 4..].iter().map(|&x| f64::from(x)).collect::<Vec<_>>());
        assert!(early < late * 0.05, "the first 10 ms of a half second's attack: {early}, and later {late}");
        let mut osc = noise();
        osc.set_parameter(6, 0.1).unwrap();
        let held = render(&mut osc, &NOTE_ON, 4800);
        assert!(held[4000..].iter().any(|x| x.abs() > 0.05));
        let released = render(&mut osc, &NOTE_OFF, 9600);
        assert!(released[..100].iter().any(|x| x.abs() > 0.05), "it does not stop at once");
        assert!(released[8000..].iter().all(|&x| x == 0.0), "and it is gone after the release");
    }

    /// The hash that the numbers of the noise start from is 0 for one input only, and a state of 0 would stay there: that seed (the 234,361,646th voice, oscillator 5)
    /// is not 0, and no other is.
    #[test]
    fn the_seed_of_a_noise_is_never_the_state_that_stays_at_zero() {
        assert_eq!(noise_seed(234_361_646, 5), 0x2545_F491);
        assert!((0..2000u32).all(|begun| (0..MAX_UNISON).all(|u| noise_seed(begun, u) != 0)));
        // (And each voice, and each oscillator of a voice, starts from a state of its own.)
        let seeds: std::collections::HashSet<u32> = (0..2000u32).flat_map(|begun| (0..MAX_UNISON).map(move |u| noise_seed(begun, u))).collect();
        assert_eq!(seeds.len(), 2000 * MAX_UNISON);
    }

    /// The numbers of a noise are made as the notes are played, on the audio thread, which must not make a trip to the allocator.
    #[test]
    fn the_notes_of_a_noise_that_start_sound_and_end_do_not_allocate() {
        let mut osc = noise();
        osc.set_parameter(8, 6.0).unwrap();
        let mut left = vec![0.0f32; 256];
        let mut right = vec![0.0f32; 256];
        let on: Vec<(u64, [u8; 3])> = (0..128u8).map(|note| (0, [0x90, note, 100])).collect();
        let off: Vec<(u64, [u8; 3])> = (0..128u8).map(|note| (0, [0x80, note, 0])).collect();
        let mut block = |osc: &mut Oscillator, events: &[(u64, [u8; 3])]| {
            let mut out: [&mut [f32]; 2] = [&mut left, &mut right];
            osc.process(events, &[], &mut out).unwrap();
        };
        let ((), allocations) = crate::alloc_guard::allocations_in(|| {
            for _ in 0..2 {
                block(&mut osc, &on);
                block(&mut osc, &[]);
                block(&mut osc, &off);
                for _ in 0..8 {
                    block(&mut osc, &[]);
                }
            }
        });
        assert!(osc.voices.is_empty(), "the notes have ended");
        assert_eq!(allocations, 0);
    }

    // ---------------------------------------------------------------------------------------------------------------------------------
    // FM: the phase of a wave swung by a sine
    // ---------------------------------------------------------------------------------------------------------------------------------

    /// The parameters of the FM: how far the phase is swung (`fm`) and how fast the sine that swings it goes (a choice, `fm_ratio`).
    const FM: u32 = 12;
    const FM_RATIO: u32 = 13;

    /// The Bessel function of the first kind and order `n` at `x`, by its series. The partials of a sine that another sine swings are sines whose amplitudes are these, at the index `x`.
    fn bessel(n: i32, x: f64) -> f64 {
        let n = n.unsigned_abs() as usize;
        let factorial = |k: usize| (1..=k).map(|i| i as f64).product::<f64>();
        (0..30)
            .map(|m| (if m % 2 == 0 { 1.0 } else { -1.0 }) / (factorial(m) * factorial(m + n)) * (x / 2.0).powi(2 * m as i32 + n as i32))
            .sum()
    }

    /// The place in the list of the `fm_ratio` choices of the one called `label`.
    fn fm_ratio_named(osc: &Oscillator, label: &str) -> f32 {
        let labels = osc.parameters()[FM_RATIO as usize].labels.clone().expect("the ratio is a choice");
        labels.iter().position(|l| l == label).unwrap_or_else(|| panic!("no ratio called {label} in {labels:?}")) as f32
    }

    /// A sine of 480 Hz that a sine of the ratio called `label` times its own frequency swings with the index `index` (radians): 4800 frames of it, a whole number of its cycles, past the
    /// attack and the smoothing of the volume and of the index.
    fn fm_sine(index: f32, label: &str) -> Vec<f32> {
        let mut osc = hundred_frame_cycle(Waveform::Sine);
        osc.set_parameter(FM, index / 10.0).unwrap();
        let ratio = fm_ratio_named(&osc, label);
        osc.set_parameter(FM_RATIO, ratio).unwrap();
        render(&mut osc, &[(0, [0x90, 69, 100])], 7200)[2400..].to_vec()
    }

    /// The ratios whose partials fall on no other partial's frequency up to the third on either side (`1 + k * ratio` is a different number of carriers for every `k`, and never 1 but for `k` of 0).
    const SEPARATE_RATIOS: [&str; 13] = ["×1.4", "×1.5", "×2.5", "×2.8", "×3", "×3.5", "×4", "×5", "×5.6", "×6", "×7", "×8", "×12"];

    #[test]
    fn a_sine_that_is_swung_by_another_has_the_partials_of_the_bessel_functions_at_the_index() {
        // Each partial of sin(w t + b sin(r w t)) is a sine of (1 + k r) w, the negative frequencies folded over, with the amplitude J_k(b) (and the voice's 0.25).
        for label in SEPARATE_RATIOS {
            let ratio: f32 = label.trim_start_matches('×').parse().unwrap();
            for index in [0.5f32, 1.0, 2.0, 3.0] {
                let out = fm_sine(index, label);
                for k in -2..=2 {
                    let hz = (480.0 * (1.0 + k as f32 * ratio)).abs();
                    let expected = 0.25 * bessel(k, f64::from(index)).abs() as f32;
                    let heard = magnitude(&out, hz);
                    assert!((heard - expected).abs() < 0.004 + 0.03 * expected, "{label}, index {index}, partial {k} ({hz} Hz): {heard} for {expected}");
                }
            }
        }
    }

    #[test]
    fn a_sine_that_is_swung_is_as_loud_as_one_that_is_not_whatever_the_index() {
        for label in ["×1.5", "×3"] {
            for index in [1.0f32, 3.0, 8.0] {
                let out: Vec<f64> = fm_sine(index, label).iter().map(|&x| f64::from(x)).collect();
                assert!((rms(&out) / (0.25 / 2.0f64.sqrt()) - 1.0).abs() < 0.03, "{label}, index {index}: {}", rms(&out));
            }
        }
    }

    #[test]
    fn the_fm_parameters_say_what_they_are_and_the_ratios_are_where_they_are() {
        let osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        let parameters = osc.parameters();
        let (fm, ratio) = (&parameters[FM as usize], &parameters[FM_RATIO as usize]);
        assert_eq!((fm.name.as_str(), fm.min, fm.max, fm.default, fm.step_count, fm.group.clone()), ("fm", 0.0, 1.0, 0.0, 0, vec!["FM".to_string()]));
        assert_eq!((ratio.name.as_str(), ratio.min, ratio.max, ratio.default, ratio.step_count, ratio.group.clone()), ("fm_ratio", 0.0, 15.0, 1.0, 15, vec!["FM".to_string()]));
        assert_eq!(
            ratio.labels.as_ref().map(|labels| labels.join(" ")).as_deref(),
            Some("×0.5 ×1 ×1.4 ×1.5 ×2 ×2.5 ×2.8 ×3 ×3.5 ×4 ×5 ×5.6 ×6 ×7 ×8 ×12"),
            "the harmonic ratios, and the family of 1.4 that bells and metal are made of"
        );
        assert_eq!(fm_ratio_named(&osc, "×1"), 1.0, "the default is a ratio of one");
    }

    #[test]
    fn the_fm_ratio_is_a_whole_place_in_its_list_that_is_held_to_it() {
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        for (set, got) in [(2.4, 2.0), (2.6, 3.0), (-3.0, 0.0), (99.0, 15.0), (15.0, 15.0)] {
            osc.set_parameter(FM_RATIO, set).unwrap();
            assert_eq!(osc.get_parameter(FM_RATIO), Some(got), "{set}");
        }
        osc.set_parameter(FM, 7.0).unwrap();
        assert_eq!(osc.get_parameter(FM), Some(1.0), "held to its top");
        osc.set_parameter(FM, -1.0).unwrap();
        assert_eq!(osc.get_parameter(FM), Some(0.0), "and to its bottom");
        assert!(osc.set_parameter(19, 0.0).is_err() && osc.get_parameter(19).is_none(), "no parameter after them");
    }

    #[test]
    fn with_no_fm_whatever_the_ratio_the_oscillator_renders_what_it_did_before_it_had_one() {
        for (waveform, ..) in BEFORE {
            let (plain_left, plain_right) = chord(&mut Oscillator::new(SAMPLE_RATE, waveform));
            for ratio in 0..16 {
                let mut osc = Oscillator::new(SAMPLE_RATE, waveform);
                osc.set_parameter(FM_RATIO, ratio as f32).unwrap();
                let (left, right) = chord(&mut osc);
                // (A chord is summed in the order of a hash map, so the numbers are compared to a millionth.)
                let same = |a: &[f32], b: &[f32]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-6);
                assert!(same(&left, &plain_left) && same(&right, &plain_right), "waveform {}, ratio {ratio}", waveform.to_param());
            }
        }
    }

    #[test]
    fn fm_changes_a_waveform_that_has_a_phase_and_keeps_it_in_its_range() {
        for waveform in [Waveform::Triangle, Waveform::Square, Waveform::Saw] {
            let name = waveform.to_param();
            let plain = render(&mut hundred_frame_cycle(waveform), &[(0, [0x90, 69, 100])], 4800);
            let mut swung = hundred_frame_cycle(waveform);
            swung.set_parameter(FM, 0.5).unwrap();
            swung.set_parameter(FM_RATIO, fm_ratio_named(&swung, "×2.5")).unwrap();
            let swung = render(&mut swung, &[(0, [0x90, 69, 100])], 4800);
            let furthest = plain[2400..].iter().zip(&swung[2400..]).fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
            assert!(furthest > 0.05, "waveform {name}: swung by {furthest} at the most");
            assert!(swung.iter().all(|x| x.abs() <= 0.25 + 1e-6), "waveform {name} stays in the range of a voice");
        }
    }

    #[test]
    fn the_noise_has_no_phase_to_swing() {
        let (plain, _) = chord(&mut noise());
        let mut swung = noise();
        swung.set_parameter(FM, 1.0).unwrap();
        let (left, _) = chord(&mut swung);
        assert!(left.iter().zip(&plain).all(|(a, b)| (a - b).abs() < 1e-6));
    }

    #[test]
    fn the_index_is_gone_to_and_not_jumped_to() {
        // A steady sine, and fm turned all the way up between two blocks: the next block begins where the sine is, and the swing grows over some milliseconds (the volume's 5 ms).
        let note = [(0, [0x90, 69, 100])];
        let blocks = |fm: f32| {
            let mut osc = hundred_frame_cycle(Waveform::Sine);
            osc.set_parameter(FM_RATIO, fm_ratio_named(&osc, "×3")).unwrap();
            render(&mut osc, &note, 1024);
            osc.set_parameter(FM, fm).unwrap();
            render(&mut osc, &[], 1024)
        };
        let (steady, turned) = (blocks(0.0), blocks(1.0));
        let first = steady[..4].iter().zip(&turned[..4]).fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
        assert!(first < 0.05, "four frames in, the sound is {first} from the steady one (a jump to the whole index is a click)");
        let later = steady[1000..].iter().zip(&turned[1000..]).fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
        assert!(later > 0.1, "and it is swung by the end of the block: {later}");
    }

    #[test]
    fn each_oscillator_of_a_unison_is_swung_at_its_own_pitch() {
        // Two voices a semitone each way from the A of 440 Hz, none across: each is a sine whose sine goes three times as fast as itself, so its first partial is four of its own frequencies.
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        osc.set_parameter(8, 1.0).unwrap();
        osc.set_parameter(9, 1.0).unwrap();
        osc.set_parameter(10, 0.0).unwrap();
        osc.set_parameter(FM, 0.1).unwrap();
        osc.set_parameter(FM_RATIO, fm_ratio_named(&osc, "×3")).unwrap();
        let out = render(&mut osc, &[(0, [0x90, 69, 100])], 24_000);
        let settled = &out[4000..];
        let each = 0.25 / 2.0f32.sqrt();
        let carriers = [440.0 * 2.0f32.powf(-1.0 / 12.0), 440.0 * 2.0f32.powf(1.0 / 12.0)];
        for carrier in carriers {
            assert!((magnitude(settled, carrier) - each * bessel(0, 1.0) as f32).abs() < 0.1 * each, "the carrier of {carrier} Hz");
            assert!((magnitude(settled, 4.0 * carrier) - each * bessel(1, 1.0) as f32).abs() < 0.1 * each, "its first partial, at {} Hz", 4.0 * carrier);
        }
    }

    #[test]
    fn a_note_begins_with_the_sine_that_swings_it_at_its_beginning_and_it_stays_in_one_cycle() {
        // The first note sounds and ends, with fm all the way up, and a second one begins: its first frame is the wave's, whatever the index is (the swing is the sine's 0 at that frame).
        let mut osc = hundred_frame_cycle(Waveform::Saw);
        osc.set_parameter(FM, 1.0).unwrap();
        render(&mut osc, &[(0, [0x90, 69, 100]), (1000, [0x80, 69, 0])], 4096);
        assert!(osc.voices.is_empty());
        let mut plain = hundred_frame_cycle(Waveform::Saw);
        let first = render(&mut osc, &[(0, [0x90, 69, 100])], 8);
        let reference = render(&mut plain, &[(0, [0x90, 69, 100])], 8);
        assert!((first[0] - reference[0]).abs() < 1e-6, "{} and {}", first[0], reference[0]);
        // And what goes round is kept to one cycle: after a long note the sines' places are all in 0 to 1.
        render(&mut osc, &[], 48_000);
        assert!(osc.voices.values().all(|voice| voice.fm_phases.iter().all(|place| (0.0..1.0).contains(place))));
    }

    #[test]
    fn a_swing_that_has_gone_back_to_nothing_is_nothing_and_not_a_number_that_never_gets_there() {
        let note = [(0, [0x90, 69, 100])];
        let mut osc = hundred_frame_cycle(Waveform::Sine);
        render(&mut osc, &note, 1024);
        osc.set_parameter(FM, 1.0).unwrap();
        render(&mut osc, &[], 4096);
        assert!(osc.fm_smoothed > 0.9, "gone to: {}", osc.fm_smoothed);
        osc.set_parameter(FM, 0.0).unwrap();
        // (A second of it is two hundred time constants: a number that only approached 0 would be 1e-87 by now, and denormal on the way.)
        render(&mut osc, &[], 48_000);
        assert_eq!(osc.fm_smoothed, 0.0);
        let mut plain = hundred_frame_cycle(Waveform::Sine);
        render(&mut plain, &note, 1024 + 4096 + 48_000);
        let (swung, steady) = (render(&mut osc, &[], 512), render(&mut plain, &[], 512));
        assert!(swung.iter().zip(&steady).all(|(a, b)| (a - b).abs() < 1e-4), "the sound is the sine's again");
    }

    #[test]
    fn the_index_is_gone_down_from_and_not_dropped_from() {
        // fm all the way up and steady, and turned to nothing between two blocks: the next block begins in the swung sound, and it is the sine's by the end.
        let note = [(0, [0x90, 69, 100])];
        let blocks = |fm: f32| {
            let mut osc = hundred_frame_cycle(Waveform::Sine);
            osc.set_parameter(FM_RATIO, fm_ratio_named(&osc, "×3")).unwrap();
            osc.set_parameter(FM, 1.0).unwrap();
            render(&mut osc, &note, 4096);
            osc.set_parameter(FM, fm).unwrap();
            render(&mut osc, &[], 1024)
        };
        let (kept, turned) = (blocks(1.0), blocks(0.0));
        let first = kept[..4].iter().zip(&turned[..4]).fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
        assert!(first < 0.05, "four frames in, the sound is {first} from the one that is still swung (a drop to no index is a click)");
        let later = kept[1000..].iter().zip(&turned[1000..]).fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
        assert!(later > 0.1, "and it is not swung by the end of the block: {later}");
    }

    /// The numbers of a swung sound are made on the audio thread, which must not make a trip to the allocator.
    #[test]
    fn notes_that_are_swung_do_not_allocate() {
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        osc.set_parameter(FM, 0.5).unwrap();
        osc.set_parameter(8, 6.0).unwrap();
        let mut left = vec![0.0f32; 256];
        let mut right = vec![0.0f32; 256];
        let on: Vec<(u64, [u8; 3])> = (0..128u8).map(|note| (0, [0x90, note, 100])).collect();
        let off: Vec<(u64, [u8; 3])> = (0..128u8).map(|note| (0, [0x80, note, 0])).collect();
        let mut block = |osc: &mut Oscillator, events: &[(u64, [u8; 3])]| {
            let mut out: [&mut [f32]; 2] = [&mut left, &mut right];
            osc.process(events, &[], &mut out).unwrap();
        };
        let ((), allocations) = crate::alloc_guard::allocations_in(|| {
            for _ in 0..2 {
                block(&mut osc, &on);
                block(&mut osc, &[]);
                block(&mut osc, &off);
                for _ in 0..8 {
                    block(&mut osc, &[]);
                }
            }
        });
        assert!(osc.voices.is_empty(), "the notes have ended");
        assert_eq!(allocations, 0);
    }

    // ---------------------------------------------------------------------------------------------------------------------------------
    // The voice: a sub oscillator, hard sync and drift
    // ---------------------------------------------------------------------------------------------------------------------------------

    /// The parameters of the voice that come after the FM's.
    const SUB: u32 = 14;
    const SYNC: u32 = 15;
    const DRIFT: u32 = 16;

    /// A sine of 480 Hz (a hundred frames to a cycle) whose `sub` is `sub`: the left and right sides of 7200 frames, and the 4800 of each past the attack and the smoothing.
    fn with_sub(sub: f32, edit: impl FnOnce(&mut Oscillator)) -> (Vec<f32>, Vec<f32>) {
        let mut osc = hundred_frame_cycle(Waveform::Sine);
        osc.set_parameter(SUB, sub).unwrap();
        edit(&mut osc);
        let (mut left, mut right) = (vec![0.0f32; 7200], vec![0.0f32; 7200]);
        osc.process(&[(0, [0x90, 69, 100])], &[], &mut [&mut left[..], &mut right[..]]).unwrap();
        (left[2400..].to_vec(), right[2400..].to_vec())
    }

    #[test]
    fn the_sub_is_a_sine_an_octave_below_the_note_at_the_level_that_is_set() {
        // The note is 480 Hz and its voice is a sine of 0.25: the sub is a sine of 240 Hz of `sub` times that.
        for sub in [0.0f32, 0.25, 0.5, 1.0] {
            let (left, _) = with_sub(sub, |_| {});
            assert!((magnitude(&left, 240.0) - 0.25 * sub).abs() < 0.01, "sub {sub}: {} Hz is {}", 240.0, magnitude(&left, 240.0));
            assert!((magnitude(&left, 480.0) - 0.25).abs() < 0.01, "the note itself is as it was: {}", magnitude(&left, 480.0));
        }
    }

    #[test]
    fn the_sub_goes_where_the_note_goes_and_not_where_a_unison_spreads_it() {
        // The detune moves it with the note (an octave above the 480 Hz makes the note 960 Hz and the sub 480). A unison of seven brings the oscillators down by the root of how many there are, which is not what the sub is.
        let (left, _) = with_sub(1.0, |osc| {
            let detune = osc.get_parameter(1).unwrap();
            osc.set_parameter(1, detune + 12.0).unwrap();
        });
        assert!((magnitude(&left, 480.0) - 0.25).abs() < 0.015 && (magnitude(&left, 960.0) - 0.25).abs() < 0.015, "{} and {}", magnitude(&left, 480.0), magnitude(&left, 960.0));
        let (left, right) = with_sub(1.0, |osc| {
            osc.set_parameter(8, 6.0).unwrap();
            osc.set_parameter(9, 0.0).unwrap();
            osc.set_parameter(10, 1.0).unwrap();
        });
        assert!((magnitude(&left, 240.0) - 0.25).abs() < 0.01, "not brought down by the unison: {}", magnitude(&left, 240.0));
        assert!((magnitude(&left, 240.0) - magnitude(&right, 240.0)).abs() < 1e-3, "in the middle, whatever the width");
    }

    #[test]
    fn the_sub_does_not_drift_with_the_oscillators() {
        // A second of a note whose oscillator drifts all the way: the sub is where one that does not drift is, to a hundredth of a cycle (a drift of 12 cents is more than half a cycle of 240 Hz in the second).
        let mut osc = hundred_frame_cycle(Waveform::Sine);
        osc.set_parameter(SUB, 1.0).unwrap();
        osc.set_parameter(DRIFT, 1.0).unwrap();
        render_in_blocks(&mut osc, &[(0, [0x90, 69, 100])], 1);
        let (phase, expected) = (osc.voices[&69].sub_phase, (187.0 * 256.0 * 240.0 / 48_000.0f32).fract());
        let apart = (phase - expected).abs();
        assert!(apart.min(1.0 - apart) < 0.01, "the sub is at {phase} of its cycle, and one that does not drift is at {expected}");
    }

    #[test]
    fn the_sub_is_gone_to_and_not_jumped_to() {
        let note = [(0, [0x90, 69, 100])];
        let blocks = |sub: f32| {
            let mut osc = hundred_frame_cycle(Waveform::Sine);
            render(&mut osc, &note, 1024);
            osc.set_parameter(SUB, sub).unwrap();
            render(&mut osc, &[], 1024)
        };
        let (steady, raised) = (blocks(0.0), blocks(1.0));
        let first = steady[..4].iter().zip(&raised[..4]).fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
        assert!(first < 0.02, "four frames in, the sound is {first} from the one with no sub (a jump to the whole sub is a click)");
        assert!(steady[1000..].iter().zip(&raised[1000..]).fold(0.0f32, |m, (a, b)| m.max((a - b).abs())) > 0.1, "and the sub is there by the end of the block");
    }

    /// The hundred frames of the cycle of the 480 Hz note that start at the frame `from`, where the sound goes up through zero (the frames after a negative one that are not).
    fn rising_crossings(signal: &[f32], from: usize) -> usize {
        (from..from + 100).filter(|&n| signal[n - 1] < 0.0 && signal[n] >= 0.0).count()
    }

    #[test]
    fn a_saw_that_is_synced_goes_through_as_many_ramps_in_a_cycle_of_the_note_as_the_ratio_says() {
        // The note's cycle is a hundred frames. The saw is made to start again at its beginning at every one of them, and goes round `sync` times as fast between: 3.3 is three ramps and a third of one that is cut,
        // which does not reach 0, 3.7 is three and most of a fourth, which does. (With no sync, a saw of 2.2 times the pitch would have two ramps in some cycles and three in others.)
        for (sync, ramps) in [(1.0f32, 1usize), (2.2, 2), (3.3, 3), (3.7, 4), (5.6, 6), (8.0, 8)] {
            let mut osc = hundred_frame_cycle(Waveform::Saw);
            osc.set_parameter(SYNC, sync).unwrap();
            let out = render(&mut osc, &[(0, [0x90, 69, 100])], 6000);
            for cycle in 10..30 {
                assert_eq!(rising_crossings(&out, cycle * 100), ramps, "sync {sync}, cycle {cycle}");
            }
        }
    }

    #[test]
    fn a_sync_of_one_is_no_sync_whatever_the_waveform() {
        for (waveform, ..) in BEFORE {
            let (plain_left, plain_right) = chord(&mut Oscillator::new(SAMPLE_RATE, waveform));
            let mut osc = Oscillator::new(SAMPLE_RATE, waveform);
            osc.set_parameter(SYNC, 1.0).unwrap();
            let (left, right) = chord(&mut osc);
            let same = |a: &[f32], b: &[f32]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-6);
            assert!(same(&left, &plain_left) && same(&right, &plain_right), "waveform {}", waveform.to_param());
        }
    }

    #[test]
    fn the_noise_has_no_cycle_to_sync() {
        let (plain, _) = chord(&mut noise());
        let mut synced = noise();
        synced.set_parameter(SYNC, 5.0).unwrap();
        let (left, _) = chord(&mut synced);
        assert!(left.iter().zip(&plain).all(|(a, b)| (a - b).abs() < 1e-6));
    }

    /// `seconds` of the oscillator in blocks of 256 frames, as the audio thread has them (the drift moves once a block), the left side, with the events in the first.
    fn render_in_blocks(osc: &mut Oscillator, events: &[(u64, [u8; 3])], seconds: usize) -> Vec<f32> {
        let mut all = Vec::with_capacity(seconds * 48_000);
        for block in 0..seconds * 48_000 / 256 {
            let (mut left, mut right) = (vec![0.0f32; 256], vec![0.0f32; 256]);
            osc.process(if block == 0 { events } else { &[] }, &[], &mut [&mut left[..], &mut right[..]]).unwrap();
            all.extend_from_slice(&left);
        }
        all
    }

    /// How far from 440 Hz, in cents, the pitch of a sine is that goes up through zero at the same places in `window` (a number of cycles: the crossings are found to a fraction of a frame).
    fn cents_from_440(window: &[f32]) -> f32 {
        let crossings: Vec<f32> = window.windows(2).enumerate().filter(|(_, w)| w[0] < 0.0 && w[1] >= 0.0).map(|(i, w)| i as f32 + (-w[0]) / (w[1] - w[0])).collect();
        let heard = (crossings.len() - 1) as f32 * 48_000.0 / (crossings[crossings.len() - 1] - crossings[0]);
        1200.0 * (heard / 440.0).log2()
    }

    /// The pitch of a 440 Hz sine, in cents, in each of the windows of 0.1 s that follow the first second.
    fn drifting_cents(drift: f32, seconds: usize) -> Vec<f32> {
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        osc.set_parameter(DRIFT, drift).unwrap();
        let out = render_in_blocks(&mut osc, &[(0, [0x90, 69, 100])], seconds);
        (10..seconds * 10 - 1).map(|n| cents_from_440(&out[n * 4800..(n + 1) * 4800])).collect()
    }

    #[test]
    fn a_drift_wanders_the_pitch_a_few_cents_and_none_is_none() {
        let still = drifting_cents(0.0, 20);
        assert!(still.iter().all(|c| c.abs() < 0.2), "no drift, no wander: {still:?}");
        let wandering = drifting_cents(1.0, 20);
        let (top, bottom) = (wandering.iter().cloned().fold(f32::MIN, f32::max), wandering.iter().cloned().fold(f32::MAX, f32::min));
        assert!(top - bottom > 15.0, "it wanders: from {bottom} to {top} cents");
        assert!(top < 13.0 && bottom > -13.0, "and not far: from {bottom} to {top} cents");
        assert!(top.max(-bottom) > 9.0, "but to most of the twelve cents that the top of the knob is: from {bottom} to {top} cents");
        // A half of the drift is half as far.
        let half = drifting_cents(0.5, 20);
        let spread = |x: &[f32]| x.iter().fold(0.0f32, |m, c| m.max(c.abs()));
        assert!(spread(&half) < 0.75 * spread(&wandering) && spread(&half) > 0.25 * spread(&wandering), "{} and {}", spread(&half), spread(&wandering));
    }

    #[test]
    fn the_same_notes_wander_the_same_way_in_two_runs() {
        let (first, second) = (drifting_cents(1.0, 8), drifting_cents(1.0, 8));
        assert!(first.iter().zip(&second).all(|(a, b)| (a - b).abs() < 0.05), "{first:?} and {second:?}");
    }

    #[test]
    fn the_notes_of_a_chord_and_the_oscillators_of_a_unison_each_wander_on_their_own() {
        // Two keys at once, each a unison of four: eight places that the pitches are on, block after block for four seconds, no two of which keep together (two can be at one place for a moment, by chance).
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        osc.set_parameter(DRIFT, 1.0).unwrap();
        osc.set_parameter(8, 3.0).unwrap();
        let chord = [(0, [0x90, 69, 100]), (0, [0x90, 81, 100])];
        let mut traces: Vec<Vec<f32>> = vec![Vec::new(); 8];
        for block in 0..4 * 48_000 / 256 {
            let (mut left, mut right) = (vec![0.0f32; 256], vec![0.0f32; 256]);
            osc.process(if block == 0 { &chord[..] } else { &[] }, &[], &mut [&mut left[..], &mut right[..]]).unwrap();
            for (n, note) in [69u8, 81].iter().enumerate() {
                for u in 0..4 {
                    traces[n * 4 + u].push(osc.voices[note].drift[u]);
                }
            }
        }
        for (i, a) in traces.iter().enumerate() {
            assert!(a.iter().any(|d| d.abs() > 0.2), "trace {i} wanders");
            for (j, b) in traces.iter().enumerate().skip(i + 1) {
                let apart = a.iter().zip(b).fold(0.0f32, |m, (x, y)| m.max((x - y).abs()));
                assert!(apart > 0.3, "traces {i} and {j} are never more than {apart} apart");
            }
        }
    }

    #[test]
    fn a_sub_that_is_turned_off_goes_to_nothing_and_not_to_a_number_that_never_gets_there() {
        let note = [(0, [0x90, 69, 100])];
        let mut osc = hundred_frame_cycle(Waveform::Sine);
        osc.set_parameter(SUB, 1.0).unwrap();
        render(&mut osc, &note, 4096);
        assert!(osc.sub_smoothed > 0.9, "gone to: {}", osc.sub_smoothed);
        osc.set_parameter(SUB, 0.0).unwrap();
        // (A second of it is two hundred time constants: a number that only approached 0 would be 1e-87 by now, and denormal on the way.)
        render(&mut osc, &[], 48_000);
        assert_eq!(osc.sub_smoothed, 0.0);
        let mut plain = hundred_frame_cycle(Waveform::Sine);
        render(&mut plain, &note, 4096 + 48_000);
        let (heard, steady) = (render(&mut osc, &[], 512), render(&mut plain, &[], 512));
        assert!(heard.iter().zip(&steady).all(|(a, b)| (a - b).abs() < 1e-4), "the sound is the one that has no sub");
    }

    #[test]
    fn the_synced_oscillator_is_as_far_into_its_cycle_as_the_note_is_into_its_own_times_the_ratio() {
        // A note of 440 Hz is 109.09 frames to a cycle, so that the cycle ends between two frames: the synced oscillator begins again as far on as the note is past its end, and not at 0.
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Saw);
        osc.set_parameter(SYNC, 3.3).unwrap();
        let mut worst = 0.0f32;
        let mut wraps = 0;
        let mut before = 0.0f32;
        for frame in 0..3000 {
            let (mut left, mut right) = ([0.0f32], [0.0f32]);
            osc.process(if frame == 0 { &[(0, [0x90, 69, 100])][..] } else { &[] }, &[], &mut [&mut left[..], &mut right[..]]).unwrap();
            let voice = &osc.voices[&69];
            let (master, slave) = (voice.phases[0], voice.sync_phases[0]);
            let expected = (master * 3.3).fract();
            let apart = (slave - expected).abs();
            worst = worst.max(apart.min(1.0 - apart));
            if master < before {
                wraps += 1;
            }
            before = master;
        }
        assert!(wraps >= 27, "{wraps} cycles of the note");
        assert!(worst < 1e-3, "the synced oscillator is {worst} of a cycle from where the note's is");
    }

    #[test]
    fn the_drift_is_given_a_new_place_every_eight_tenths_of_a_second_and_goes_to_it_slowly() {
        // Ten seconds in blocks of 256 frames: a new place to go to is given at the second block (the voice is not there at the first, which is the one that the note begins in) and then every 0.8 s, 13 in all,
        // and the pitch goes to it with a time constant of 0.4 s (it is more than half of the way there a half second after, and no block moves it by more than the 0.03 of the whole run that the time constant allows).
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        osc.set_parameter(DRIFT, 1.0).unwrap();
        let (mut places, mut last_place, mut last_state) = (0, 0.0f32, 0.0f32);
        let (mut steepest, mut asked) = (0.0f32, Vec::new());
        for block in 0..10 * 48_000 / 256 {
            let (mut left, mut right) = (vec![0.0f32; 256], vec![0.0f32; 256]);
            osc.process(if block == 0 { &[(0, [0x90, 69, 100])][..] } else { &[] }, &[], &mut [&mut left[..], &mut right[..]]).unwrap();
            let voice = &osc.voices[&69];
            let (place, state) = (voice.drift_target[0], voice.drift[0]);
            if place != last_place {
                places += 1;
                last_place = place;
                asked.push((block, (place - state).abs()));
            }
            steepest = steepest.max((state - last_state).abs());
            last_state = state;
            if let Some(&(at, from)) = asked.last() {
                if block == at + 94 && from > 0.5 {
                    assert!((place - state).abs() < 0.5 * from, "a half second after, {} of {from} is left", (place - state).abs());
                }
            }
        }
        assert_eq!(places, 13);
        assert!(steepest < 0.03, "{steepest} in a block");
    }

    #[test]
    fn a_drift_that_is_turned_to_none_leaves_the_pitch_of_the_note() {
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        osc.set_parameter(DRIFT, 1.0).unwrap();
        let wandering = render_in_blocks(&mut osc, &[(0, [0x90, 69, 100])], 3);
        assert!(cents_from_440(&wandering[wandering.len() - 4800..]).abs() > 1.0, "it was away from the note");
        osc.set_parameter(DRIFT, 0.0).unwrap();
        let after = render_in_blocks(&mut osc, &[], 1);
        let cents = cents_from_440(&after[after.len() - 4800..]);
        assert!(cents.abs() < 0.2, "{cents} cents from the pitch of the note");
    }

    #[test]
    fn a_synced_saw_that_is_swung_a_little_goes_through_the_ramps_of_the_sync_all_the_same() {
        let mut osc = hundred_frame_cycle(Waveform::Saw);
        osc.set_parameter(SYNC, 3.3).unwrap();
        osc.set_parameter(12, 0.02).unwrap();
        let out = render(&mut osc, &[(0, [0x90, 69, 100])], 6000);
        for cycle in 10..30 {
            assert_eq!(rising_crossings(&out, cycle * 100), 3, "cycle {cycle}");
        }
    }

    #[test]
    fn the_phase_of_the_sub_is_kept_within_a_cycle() {
        let mut osc = hundred_frame_cycle(Waveform::Sine);
        osc.set_parameter(SUB, 1.0).unwrap();
        render(&mut osc, &[(0, [0x90, 69, 100])], 48_000);
        let phase = osc.voices[&69].sub_phase;
        assert!((0.0..1.0).contains(&phase), "{phase}");
    }

    /// The numbers of a voice with a sub, a sync and a drift are made on the audio thread, which must not make a trip to the allocator.
    #[test]
    fn notes_with_a_sub_a_sync_and_a_drift_do_not_allocate() {
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Saw);
        osc.set_parameter(SUB, 0.5).unwrap();
        osc.set_parameter(SYNC, 3.0).unwrap();
        osc.set_parameter(DRIFT, 1.0).unwrap();
        osc.set_parameter(8, 6.0).unwrap();
        let mut left = vec![0.0f32; 256];
        let mut right = vec![0.0f32; 256];
        let on: Vec<(u64, [u8; 3])> = (0..128u8).map(|note| (0, [0x90, note, 100])).collect();
        let off: Vec<(u64, [u8; 3])> = (0..128u8).map(|note| (0, [0x80, note, 0])).collect();
        let mut block = |osc: &mut Oscillator, events: &[(u64, [u8; 3])]| {
            let mut out: [&mut [f32]; 2] = [&mut left, &mut right];
            osc.process(events, &[], &mut out).unwrap();
        };
        let ((), allocations) = crate::alloc_guard::allocations_in(|| {
            for _ in 0..2 {
                block(&mut osc, &on);
                block(&mut osc, &[]);
                block(&mut osc, &off);
                for _ in 0..8 {
                    block(&mut osc, &[]);
                }
            }
        });
        assert!(osc.voices.is_empty(), "the notes have ended");
        assert_eq!(allocations, 0);
    }

    // ---------------------------------------------------------------------------------------------------------------------------------
    // The keyboard: velocity, the pitch wheel, the sustain pedal and all notes off
    // ---------------------------------------------------------------------------------------------------------------------------------

    /// The parameters of the keyboard, by their places.
    const VELOCITY: u32 = 17;
    const BEND: u32 = 18;

    fn note_on(note: u8, velocity: u8, channel: u8) -> (u64, [u8; 3]) {
        (0, [0x90 | channel, note, velocity])
    }

    fn note_off(note: u8) -> (u64, [u8; 3]) {
        (0, [0x80, note, 0])
    }

    /// The pitch wheel of a channel at `raw` (0 to 16383, 8192 the middle).
    fn wheel(raw: u16, channel: u8) -> (u64, [u8; 3]) {
        (0, [0xE0 | channel, (raw & 0x7F) as u8, (raw >> 7) as u8])
    }

    fn pedal(value: u8) -> (u64, [u8; 3]) {
        (0, [0xB0, 64, value])
    }

    /// The loudest of the last `n` frames of `x`.
    fn loudest_of_the_last(x: &[f32], n: usize) -> f32 {
        x[x.len() - n..].iter().fold(0.0f32, |m, v| m.max(v.abs()))
    }

    /// The amplitude of the sine of 480 Hz that the key 69 makes when it is hit at `velocity` (it is 0.25 at full level), with a velocity of `sensitivity`.
    fn level_of(sensitivity: f32, velocity: u8) -> f32 {
        let mut osc = hundred_frame_cycle(Waveform::Sine);
        osc.set_parameter(VELOCITY, sensitivity).unwrap();
        let out = render(&mut osc, &[note_on(69, velocity, 0)], 7200);
        magnitude(&out[2400..], 480.0)
    }

    #[test]
    fn velocity_does_nothing_at_its_default() {
        let (soft, loud) = (level_of(0.0, 20), level_of(0.0, 127));
        assert!((soft - 0.25).abs() < 0.005 && (loud - 0.25).abs() < 0.005, "{soft} and {loud}");
        // ... and bit for bit: two notes of different velocities are the same sound.
        let (mut a, mut b) = (hundred_frame_cycle(Waveform::Saw), hundred_frame_cycle(Waveform::Saw));
        assert_eq!(render(&mut a, &[note_on(69, 20, 0)], 2400), render(&mut b, &[note_on(69, 127, 0)], 2400));
    }

    #[test]
    fn a_velocity_of_all_makes_the_level_the_velocity_over_127() {
        for velocity in [1u8, 32, 64, 100, 127] {
            let got = level_of(1.0, velocity);
            let wanted = 0.25 * f32::from(velocity) / 127.0;
            assert!((got - wanted).abs() < 0.003, "velocity {velocity}: {got} for {wanted}");
        }
    }

    #[test]
    fn half_a_velocity_is_half_way_between_none_of_it_and_all_of_it() {
        let wanted = 0.25 * (0.5 + 0.5 * 64.0 / 127.0);
        assert!((level_of(0.5, 64) - wanted).abs() < 0.003, "{} for {wanted}", level_of(0.5, 64));
    }

    #[test]
    fn the_sub_is_as_loud_as_the_velocity_says_too() {
        let mut osc = hundred_frame_cycle(Waveform::Sine);
        osc.set_parameter(VELOCITY, 1.0).unwrap();
        osc.set_parameter(SUB, 1.0).unwrap();
        let out = render(&mut osc, &[note_on(69, 64, 0)], 7200);
        let wanted = 0.25 * 64.0 / 127.0;
        assert!((magnitude(&out[2400..], 480.0) - wanted).abs() < 0.003 && (magnitude(&out[2400..], 240.0) - wanted).abs() < 0.003);
    }

    #[test]
    fn a_key_that_is_hit_again_goes_to_the_new_velocity_and_does_not_jump_to_it() {
        let mut osc = hundred_frame_cycle(Waveform::Sine);
        osc.set_parameter(VELOCITY, 1.0).unwrap();
        render(&mut osc, &[note_on(69, 127, 0)], 4800);
        let again = render(&mut osc, &[note_on(69, 32, 0)], 4800);
        // The first cycle of it is still well over the level of the new velocity (0.0625: a jump to it would be that at once) ...
        assert!(again[..100].iter().fold(0.0f32, |m, v| m.max(v.abs())) > 0.15);
        // ... and it has gone to it after the smoothing of the volume (5 ms is 240 frames; 2400 are ten time constants).
        assert!((magnitude(&again[2400..], 480.0) - 0.25 * 32.0 / 127.0).abs() < 0.004);
    }

    /// How far from the pitch of the key 69 (440 Hz, a sine) the sound is after `events`, in cents, with the wheel's range at the place `range` of the list.
    fn bent_cents(range: f32, events: &[(u64, [u8; 3])]) -> f32 {
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        osc.set_parameter(BEND, range).unwrap();
        let out = render(&mut osc, events, 9600);
        cents_from_440(&out[4800..])
    }

    #[test]
    fn the_wheel_bends_every_note_by_the_range_and_the_middle_bends_none() {
        let key = note_on(69, 100, 0);
        // A range of twelve semitones (the seventh place): the top of the wheel is all but the last step of it, and the bottom all of it.
        assert!((bent_cents(6.0, &[key, wheel(16383, 0)]) - 1199.85).abs() < 1.0, "{}", bent_cents(6.0, &[key, wheel(16383, 0)]));
        assert!((bent_cents(6.0, &[key, wheel(0, 0)]) + 1200.0).abs() < 1.0);
        assert!(bent_cents(6.0, &[key, wheel(8192, 0)]).abs() < 0.5);
        assert!(bent_cents(6.0, &[key]).abs() < 0.5);
        // No range is no bend, and the range that it starts at is two semitones.
        assert!(bent_cents(0.0, &[key, wheel(16383, 0)]).abs() < 0.5);
        assert!((bent_cents(2.0, &[key, wheel(16383, 0)]) - 199.98).abs() < 0.5);
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        assert_eq!(osc.get_parameter(BEND), Some(2.0));
        let out = render(&mut osc, &[key, wheel(16383, 0)], 9600);
        assert!((cents_from_440(&out[4800..]) - 199.98).abs() < 0.5, "a wheel bends a sound that has not been told a range by two semitones");
    }

    #[test]
    fn the_wheel_and_the_detune_add_up() {
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        osc.set_parameter(1, 1.0).unwrap();
        let out = render(&mut osc, &[note_on(69, 100, 0), wheel(16383, 0)], 9600);
        assert!((cents_from_440(&out[4800..]) - 299.98).abs() < 0.6, "{}", cents_from_440(&out[4800..]));
    }

    #[test]
    fn a_wheel_message_is_heard_at_its_frame_of_the_block() {
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        osc.set_parameter(BEND, 6.0).unwrap();
        let out = render(&mut osc, &[note_on(69, 100, 0), (4800, [0xE0, 0x7F, 0x7F])], 9600);
        assert!(cents_from_440(&out[1000..4700]).abs() < 1.0, "before the message");
        assert!((cents_from_440(&out[5200..9600]) - 1199.85).abs() < 1.5, "after it");
    }

    #[test]
    fn a_member_channel_bends_the_notes_that_began_on_it_and_the_master_channel_bends_all_of_them() {
        // Two keys an octave apart (440 Hz on the master channel and 880 Hz on the second), a range of twelve semitones.
        let heard = |wheel_message: (u64, [u8; 3])| {
            let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
            osc.set_parameter(BEND, 6.0).unwrap();
            let out = render(&mut osc, &[note_on(69, 100, 0), note_on(81, 100, 1), wheel_message], 9600);
            let settled = out[4800..].to_vec();
            [440.0, 880.0, 1760.0].map(|hz| magnitude(&settled, hz))
        };
        // The wheel of the second channel at the top: the key on the master channel stays, the one on the second goes up an octave.
        let on_the_member = heard(wheel(16383, 1));
        assert!(on_the_member[0] > 0.2 && on_the_member[1] < 0.02 && on_the_member[2] > 0.2, "{on_the_member:?}");
        // The wheel of the master channel: both go up an octave.
        let on_the_master = heard(wheel(16383, 0));
        assert!(on_the_master[0] < 0.02 && on_the_master[1] > 0.2 && on_the_master[2] > 0.2, "{on_the_master:?}");
    }

    #[test]
    fn a_note_that_is_let_go_under_the_pedal_sounds_until_the_pedal_goes_up() {
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        let block = render(&mut osc, &[pedal(127), note_on(69, 100, 0), (2400, [0x80, 69, 0])], 9600);
        assert!(loudest_of_the_last(&block, 480) > 0.2, "still sounding a tenth of a second after the key went up");
        let block = render(&mut osc, &[pedal(0)], 4800);
        assert!(loudest_of_the_last(&block, 480) < 1e-4 && osc.voices.is_empty(), "and let go of when the pedal goes up");
    }

    #[test]
    fn a_note_that_was_let_go_before_the_pedal_went_down_is_not_held_by_it() {
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        // The key goes up at 2400 and the pedal down a hundred frames after, while the note dies away: it goes on dying.
        let block = render(&mut osc, &[note_on(69, 100, 0), (2400, [0x80, 69, 0]), (2500, [0xB0, 64, 127])], 4800);
        assert!(loudest_of_the_last(&block, 480) < 1e-4 && osc.voices.is_empty());
    }

    #[test]
    fn a_pedal_that_goes_up_does_not_let_go_of_a_key_that_is_down() {
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        render(&mut osc, &[pedal(127), note_on(69, 100, 0)], 4800);
        let block = render(&mut osc, &[pedal(0)], 4800);
        assert!(loudest_of_the_last(&block, 480) > 0.2, "the key is down");
        let block = render(&mut osc, &[note_off(69)], 4800);
        assert!(loudest_of_the_last(&block, 480) < 1e-4 && osc.voices.is_empty(), "and it is let go of with the key");
    }

    #[test]
    fn a_key_that_is_hit_again_under_the_pedal_is_a_key_that_is_down_again() {
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        // Hit, let go (held by the pedal) and hit again: it is a key that is down, and the pedal's going up does not let go of it.
        let block = render(&mut osc, &[pedal(127), note_on(69, 100, 0), (1200, [0x80, 69, 0]), (2400, [0x90, 69, 100])], 4800);
        assert!(loudest_of_the_last(&block, 480) > 0.2);
        let block = render(&mut osc, &[pedal(0)], 4800);
        assert!(loudest_of_the_last(&block, 480) > 0.2, "the pedal goes up and the key is down");
        // Down again with the pedal, let go: held by the pedal, and let go of with it.
        let block = render(&mut osc, &[pedal(127), note_off(69)], 4800);
        assert!(loudest_of_the_last(&block, 480) > 0.2, "let go under the pedal");
        let block = render(&mut osc, &[pedal(0)], 4800);
        assert!(loudest_of_the_last(&block, 480) < 1e-4 && osc.voices.is_empty());
    }

    #[test]
    fn a_pedal_is_down_from_64_up() {
        for (value, held) in [(63u8, false), (64, true), (127, true), (0, false)] {
            let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
            let block = render(&mut osc, &[pedal(value), note_on(69, 100, 0), (2400, [0x80, 69, 0])], 9600);
            assert_eq!(loudest_of_the_last(&block, 480) > 0.2, held, "a pedal at {value}");
        }
    }

    #[test]
    fn all_notes_off_lets_go_of_every_note_under_the_pedal_too_and_all_sound_off_is_quicker() {
        let keys = [pedal(127), note_on(60, 100, 0), note_on(64, 100, 0), note_on(67, 100, 0)];
        let loudest = |x: &[f32]| x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        // The notes are let go of with the release of the envelope (12 ms): well on their way 5 ms after, and gone after a tenth of a second.
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        render(&mut osc, &keys, 4800);
        let block = render(&mut osc, &[(0, [0xB0, 123, 0])], 4800);
        assert!(loudest(&block[200..300]) > 0.05, "the release of the envelope is not over at 4 to 6 ms: {}", loudest(&block[200..300]));
        assert!(loudest_of_the_last(&block, 480) < 1e-4 && osc.voices.is_empty());
        // All sound off is a release of 5 ms, whatever the envelope's: nothing is left after 300 frames.
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        render(&mut osc, &keys, 4800);
        let block = render(&mut osc, &[(0, [0xB0, 120, 0])], 4800);
        assert!(loudest(&block[300..]) < 1e-3, "{}", loudest(&block[300..]));
        assert!(osc.voices.is_empty());
    }

    #[test]
    fn a_key_that_is_hit_again_after_all_sound_off_is_let_go_of_by_the_envelope_again() {
        // All sound off, and the same key hit again 2 ms later, while the note is on its way down: it begins again, and when the key goes up (at 2400) it is let go of by the
        // release of the envelope (12 ms, 576 frames): half of it is left 6 ms (288 frames) after, where the quick release of all sound off would have left nothing.
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        render(&mut osc, &[note_on(69, 100, 0)], 4800);
        let block = render(&mut osc, &[(0, [0xB0, 120, 0]), (100, [0x90, 69, 100]), (2400, [0x80, 69, 0])], 4800);
        assert!(block[2400 + 280..2400 + 296].iter().fold(0.0f32, |m, v| m.max(v.abs())) > 0.05, "the release is the envelope's");
    }

    /// The numbers of a keyboard are made on the audio thread, which must not make a trip to the allocator.
    #[test]
    fn notes_with_velocity_a_wheel_a_pedal_and_all_notes_off_do_not_allocate() {
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Saw);
        osc.set_parameter(VELOCITY, 1.0).unwrap();
        osc.set_parameter(BEND, 8.0).unwrap();
        let mut left = vec![0.0f32; 256];
        let mut right = vec![0.0f32; 256];
        let on: Vec<(u64, [u8; 3])> = (0..128u8).map(|note| (0, [0x90 | (note % 16), note, 1 + note])).collect();
        let off: Vec<(u64, [u8; 3])> = (0..128u8).map(|note| (0, [0x80, note, 0])).collect();
        let controls: Vec<(u64, [u8; 3])> = vec![pedal(127), wheel(12000, 0), wheel(4000, 3), (0, [0xB0, 74, 99]), pedal(0), (0, [0xB0, 123, 0]), (0, [0xB0, 120, 0])];
        let mut block = |osc: &mut Oscillator, events: &[(u64, [u8; 3])]| {
            let mut out: [&mut [f32]; 2] = [&mut left, &mut right];
            osc.process(events, &[], &mut out).unwrap();
        };
        let ((), allocations) = crate::alloc_guard::allocations_in(|| {
            for _ in 0..2 {
                block(&mut osc, &on);
                block(&mut osc, &controls);
                block(&mut osc, &on);
                block(&mut osc, &off);
                for _ in 0..8 {
                    block(&mut osc, &[]);
                }
            }
        });
        assert!(osc.voices.is_empty(), "the notes have ended");
        assert_eq!(allocations, 0);
    }

    /// What a parameter says of itself: name, unit, lowest, highest, default and steps.
    type Described<'a> = (&'a str, Option<&'a str>, f32, f32, f32, u32);

    #[test]
    fn the_new_parameters_round_trip_and_are_held_to_their_ranges() {
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        let parameters = osc.parameters();
        let described: Vec<Described> = parameters[8..].iter().map(|p| (p.name.as_str(), p.unit.as_deref(), p.min, p.max, p.default, p.step_count)).collect();
        assert_eq!(
            described,
            [("unison", None, 0.0, 6.0, 0.0, 6), ("spread", Some("st"), 0.0, 1.0, 0.15, 0), ("width", None, 0.0, 1.0, 0.5, 0), ("pulse", None, 0.05, 0.95, 0.5, 0), ("fm", None, 0.0, 1.0, 0.0, 0), ("fm_ratio", None, 0.0, 15.0, 1.0, 15), ("sub", None, 0.0, 1.0, 0.0, 0), ("sync", None, 1.0, 8.0, 1.0, 0), ("drift", None, 0.0, 1.0, 0.0, 0), ("velocity", None, 0.0, 1.0, 0.0, 0), ("bend", None, 0.0, 8.0, 2.0, 8)]
        );
        // (A unison is a choice of one to seven voices, named by how many; the bend a choice of how far the wheel goes.)
        assert_eq!(parameters[8].labels.as_deref().map(|l| l.join(" ")), Some("1 2 3 4 5 6 7".to_string()));
        assert_eq!(parameters[18].labels.as_deref().map(|l| l.join(" ")), Some("off ±1 ±2 ±3 ±5 ±7 ±12 ±24 ±48".to_string()));
        for (index, p) in parameters.iter().enumerate().skip(8) {
            assert_eq!(osc.get_parameter(index as u32), Some(p.default), "{}", p.name);
            osc.set_parameter(index as u32, p.max + 100.0).unwrap();
            assert_eq!(osc.get_parameter(index as u32), Some(p.max), "{} held to its top", p.name);
            osc.set_parameter(index as u32, p.min - 100.0).unwrap();
            assert_eq!(osc.get_parameter(index as u32), Some(p.min), "{} held to its bottom", p.name);
        }
        assert!(osc.set_parameter(19, 0.0).is_err() && osc.get_parameter(19).is_none());
        // A unison is a whole number of voices, and a bend a place in its list: a value between two is the nearer.
        for (set, got) in [(2.4, 2.0), (2.6, 3.0), (0.4, 0.0), (5.6, 6.0)] {
            osc.set_parameter(8, set).unwrap();
            assert_eq!(osc.get_parameter(8), Some(got), "{set}");
            osc.set_parameter(18, set).unwrap();
            assert_eq!(osc.get_parameter(18), Some(got), "bend {set}");
        }
    }
}
