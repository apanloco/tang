use std::f32::consts::{PI, TAU};

use crate::plugin::{ParameterInfo, Plugin, Preset};

pub const NAME: &str = "Filter";
pub const ID: &str = "builtin:filter";
pub const PARAM_COUNT: usize = 4;

/// Filter types selectable via the `type` parameter (rounded to nearest).
/// 0–3 are clean 12 dB/oct state-variable modes; 4–6 are nonlinear analog
/// models: `ladder` = Moog/Mother-32-style 4-pole transistor ladder
/// (24 dB/oct, warm), `acid` = TB-303-style ladder (18 dB/oct tap,
/// high-passed feedback squelch), `ms20` = Korg MS-20-style Sallen-Key
/// (12 dB/oct, hard-clipped feedback, aggressive).
const TYPE_NAMES: [&str; 7] = [
    "lowpass", "highpass", "bandpass", "notch", "ladder", "acid", "ms20",
];

const MODE_LADDER: usize = 4;
const MODE_ACID: usize = 5;
const MODE_MS20: usize = 6;

const CUTOFF_MIN_HZ: f32 = 20.0;
const CUTOFF_MAX_HZ: f32 = 20_000.0;
const Q_MIN: f32 = 0.5;
const Q_MAX: f32 = 10.0;
const Q_DEFAULT: f32 = 0.707;
const DRIVE_MIN: f32 = 1.0;
const DRIVE_MAX: f32 = 10.0;

/// Feedback amounts at full resonance. The ladder self-oscillates at 4.0,
/// the Sallen-Key at 2.0 — full resonance sits just past the edge.
const LADDER_K_MAX: f32 = 4.2;
const ACID_K_MAX: f32 = 4.4;
const MS20_K_MAX: f32 = 2.05;

/// Cutoff tuning multipliers for the analog modes. A cascade of N one-pole
/// stages has its composite -3 dB point well below the per-stage corner, so
/// to make the `cutoff` knob mark the -3 dB point (consistent with the clean
/// SVF modes and the standard filter convention) the per-stage corner is
/// raised by these factors. Measured at the default resonance so low-Q
/// brightness matches across all filter types. (The resonant peak then sits
/// somewhat above the knob at high resonance — inherent to multipole filters.)
const LADDER_TUNE: f32 = 2.00; // 4-pole
const ACID_TUNE: f32 = 1.75; // 3-pole tap (with feedback high-pass)
const MS20_TUNE: f32 = 1.50; // 2-pole

/// Corner of the high-pass in the acid feedback path (the 303 squelch:
/// keeps the resonance feedback from muddying the sub-bass). Low enough
/// that resonant basslines down in their home register keep their squelch.
const ACID_FB_HP_HZ: f32 = 30.0;

/// One-pole smoothing time constant for cutoff/resonance/drive, against
/// zipper noise when dragged or modulated at block rate.
const SMOOTH_SEC: f32 = 0.005;

/// A stereo multimode filter. Types 0–3 are a trapezoidal state-variable
/// filter (Simper SVF) — clean, stable under cutoff modulation. Types 4–6
/// are nonlinear zero-delay-style analog models run at 2× internal
/// oversampling, with `drive` setting the input gain into the saturation.
///
/// `cutoff` is normalized 0–1 and mapped exponentially to 20 Hz–20 kHz
/// (three decades), so sweeps and LFO/envelope modulation move musically
/// (equal param distance = equal pitch distance) instead of spending the
/// whole range in the top octaves.
pub struct Filter {
    sample_rate: f32,
    /// Filter type parameter (0–6, rounded to nearest on use).
    mode: f32,
    /// Normalized cutoff target (0–1, exponential to Hz).
    cutoff: f32,
    /// Resonance (Q) target. Analog models map it onto their feedback range.
    resonance: f32,
    /// Input gain into the analog models' saturation (ignored by 0–3).
    drive: f32,
    cutoff_smoothed: f32,
    q_smoothed: f32,
    drive_smoothed: f32,
    /// SVF integrator state per channel: [ic1eq, ic2eq].
    svf: [[f32; 2]; 2],
    /// Analog model state per channel: 4 ladder/Sallen-Key stages plus the
    /// acid feedback high-pass state.
    analog: [AnalogState; 2],
}

#[derive(Clone, Copy, Default)]
struct AnalogState {
    s: [f32; 4],
    fb_lp: f32,
}

impl Filter {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            mode: 0.0,
            cutoff: 1.0,
            resonance: Q_DEFAULT,
            drive: DRIVE_MIN,
            cutoff_smoothed: 1.0,
            q_smoothed: Q_DEFAULT,
            drive_smoothed: DRIVE_MIN,
            svf: [[0.0; 2]; 2],
            analog: [AnalogState::default(); 2],
        }
    }

    fn cutoff_hz(normalized: f32) -> f32 {
        CUTOFF_MIN_HZ * (CUTOFF_MAX_HZ / CUTOFF_MIN_HZ).powf(normalized.clamp(0.0, 1.0))
    }

    fn reset_state(&mut self) {
        self.svf = [[0.0; 2]; 2];
        self.analog = [AnalogState::default(); 2];
    }

    /// Map the Q parameter onto an analog model's feedback range.
    fn feedback(&self, k_max: f32) -> f32 {
        k_max * (self.q_smoothed - Q_MIN) / (Q_MAX - Q_MIN)
    }
}

impl Plugin for Filter {
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
        let block_size = audio_out[0].len();

        let in_l = audio_in.first().copied();
        let in_r = audio_in.get(1).copied().or(in_l);

        let mode = (self.mode.round() as usize).min(TYPE_NAMES.len() - 1);
        let alpha = 1.0 - (-1.0 / (SMOOTH_SEC * self.sample_rate)).exp();
        // Keep the SVF tan() well away from the Nyquist pole.
        let max_fc = 0.45 * self.sample_rate;
        // Analog models run two half-steps per sample.
        let sr2 = 2.0 * self.sample_rate;
        // The analog per-stage corner is raised (tuning multipliers) so the
        // composite -3 dB lands on the knob; clamp against the internal
        // (oversampled) Nyquist so tan()/the integrators stay stable.
        let max_fc_analog = 0.45 * sr2;
        let g_fb_hp = 1.0 - (-TAU * ACID_FB_HP_HZ / sr2).exp();

        for frame in 0..block_size {
            self.cutoff_smoothed += (self.cutoff - self.cutoff_smoothed) * alpha;
            self.q_smoothed += (self.resonance - self.q_smoothed) * alpha;
            self.drive_smoothed += (self.drive - self.drive_smoothed) * alpha;

            let fc = Self::cutoff_hz(self.cutoff_smoothed).min(max_fc);
            let drive = self.drive_smoothed;

            match mode {
                MODE_LADDER | MODE_ACID => {
                    // Zero-delay-feedback (TPT) transistor ladder. `tan`
                    // prewarping tunes each one-pole stage exactly to fc, and
                    // the feedback is resolved instantaneously (no one-sample
                    // delay) so the resonant peak — and self-oscillation —
                    // land on fc across the whole range. The single tanh on
                    // the resonant feedback bounds self-oscillation and is
                    // ≈linear for small signals, so the tuning no longer
                    // wanders with input level.
                    let tune = if mode == MODE_LADDER { LADDER_TUNE } else { ACID_TUNE };
                    let fc_a = (fc * tune).min(max_fc_analog);
                    let big_g = {
                        let g = (PI * fc_a / sr2).tan();
                        g / (1.0 + g)
                    };
                    let one_minus_g = 1.0 - big_g;
                    let g2 = big_g * big_g;
                    let g3 = g2 * big_g;
                    let g4 = g3 * big_g;
                    let k = if mode == MODE_LADDER {
                        self.feedback(LADDER_K_MAX)
                    } else {
                        self.feedback(ACID_K_MAX)
                    };
                    // Input gain compensation counters resonance bass loss.
                    let makeup = 1.0 + 0.5 * k;

                    for ch in 0..audio_out.len().min(2) {
                        let x = match ch {
                            0 => in_l.map(|s| s[frame]).unwrap_or(0.0),
                            _ => in_r.map(|s| s[frame]).unwrap_or(0.0),
                        };
                        let st = &mut self.analog[ch];
                        let mut out = 0.0;
                        // Two half-steps per sample (2× oversampling) to push
                        // saturation aliasing above the audible band.
                        for _ in 0..2 {
                            let xin = (drive * x).tanh();
                            // Resolve the 4-pole output from current states.
                            let s_sum = g3 * one_minus_g * st.s[0]
                                + g2 * one_minus_g * st.s[1]
                                + big_g * one_minus_g * st.s[2]
                                + one_minus_g * st.s[3];
                            let y_est = (g4 * xin + s_sum) / (1.0 + k * g4);
                            // Feedback signal: for acid, high-pass it (the 303
                            // squelch — resonance rides on top, low end stays
                            // put); for the ladder, the full output.
                            let fb = if mode == MODE_ACID {
                                st.fb_lp += g_fb_hp * (y_est - st.fb_lp);
                                y_est - st.fb_lp
                            } else {
                                y_est
                            };
                            // Resonant feedback into stage 1, tanh-bounded.
                            let u = xin - (k * fb).tanh();
                            // Run the four TPT one-poles forward.
                            let mut inp = u;
                            let mut stage3 = 0.0;
                            for (i, z) in st.s.iter_mut().enumerate() {
                                let v = (inp - *z) * big_g;
                                let y = v + *z;
                                *z = y + v; // z += 2v
                                inp = y;
                                if i == 2 {
                                    stage3 = y; // 18 dB/oct tap for acid
                                }
                            }
                            // 24 dB tap (ladder) or 18 dB tap (acid).
                            out = if mode == MODE_LADDER { inp } else { stage3 };
                        }
                        audio_out[ch][frame] = out * makeup;
                    }
                }
                MODE_MS20 => {
                    // MS-20-style Sallen-Key 2-pole: a bandpass-derived
                    // resonant feedback (the difference of the two pole
                    // states), hard-clipped by a diode pair for the
                    // characteristic bite, summed into the input.
                    let fc_a = (fc * MS20_TUNE).min(max_fc_analog);
                    let g = 1.0 - (-TAU * fc_a / sr2).exp();
                    let k = self.feedback(MS20_K_MAX);

                    for ch in 0..audio_out.len().min(2) {
                        let x = match ch {
                            0 => in_l.map(|s| s[frame]).unwrap_or(0.0),
                            _ => in_r.map(|s| s[frame]).unwrap_or(0.0),
                        };
                        let st = &mut self.analog[ch];
                        for _ in 0..2 {
                            let bp = st.s[0] - st.s[1];
                            let fb = (k * bp).clamp(-1.0, 1.0);
                            let u = (drive * x + fb).tanh();
                            st.s[0] += g * (u - st.s[0]);
                            st.s[1] += g * (st.s[0] - st.s[1]);
                        }
                        audio_out[ch][frame] = st.s[1];
                    }
                }
                _ => {
                    // Clean SVF modes.
                    let g = (PI * fc / self.sample_rate).tan();
                    let k = 1.0 / self.q_smoothed;
                    let a1 = 1.0 / (1.0 + g * (g + k));

                    for ch in 0..audio_out.len().min(2) {
                        let x = match ch {
                            0 => in_l.map(|s| s[frame]).unwrap_or(0.0),
                            _ => in_r.map(|s| s[frame]).unwrap_or(0.0),
                        };
                        let [ic1, ic2] = &mut self.svf[ch];

                        let v1 = a1 * (*ic1 + g * (x - *ic2)); // bandpass (raw)
                        let v2 = *ic2 + g * v1; // lowpass
                        *ic1 = 2.0 * v1 - *ic1;
                        *ic2 = 2.0 * v2 - *ic2;

                        let y = match mode {
                            0 => v2,              // lowpass
                            1 => x - k * v1 - v2, // highpass
                            2 => k * v1,          // bandpass (unity peak at fc)
                            _ => x - k * v1,      // notch
                        };
                        audio_out[ch][frame] = y;
                    }
                }
            }
        }

        Ok(())
    }

    fn parameters(&self) -> Vec<ParameterInfo> {
        vec![
            ParameterInfo {
                index: 0,
                name: "cutoff".into(),
                min: 0.0,
                max: 1.0,
                default: 1.0,
                ..Default::default()
            },
            ParameterInfo {
                index: 1,
                name: "resonance".into(),
                min: Q_MIN,
                max: Q_MAX,
                default: Q_DEFAULT,
                ..Default::default()
            },
            ParameterInfo {
                index: 2,
                name: "type".into(),
                min: 0.0,
                max: (TYPE_NAMES.len() - 1) as f32,
                default: 0.0,
                labels: Some(TYPE_NAMES.iter().map(|s| s.to_string()).collect()),
            },
            ParameterInfo {
                index: 3,
                name: "drive".into(),
                min: DRIVE_MIN,
                max: DRIVE_MAX,
                default: DRIVE_MIN,
                ..Default::default()
            },
        ]
    }

    fn get_parameter(&mut self, index: u32) -> Option<f32> {
        match index {
            0 => Some(self.cutoff),
            1 => Some(self.resonance),
            2 => Some(self.mode),
            3 => Some(self.drive),
            _ => None,
        }
    }

    fn set_parameter(&mut self, index: u32, value: f32) -> anyhow::Result<()> {
        match index {
            0 => {
                self.cutoff = value.clamp(0.0, 1.0);
                Ok(())
            }
            1 => {
                self.resonance = value.clamp(Q_MIN, Q_MAX);
                Ok(())
            }
            2 => {
                let old_mode = self.mode.round();
                self.mode = value.clamp(0.0, (TYPE_NAMES.len() - 1) as f32);
                // Discard stale energy when the topology changes.
                if self.mode.round() != old_mode {
                    self.reset_state();
                }
                Ok(())
            }
            3 => {
                self.drive = value.clamp(DRIVE_MIN, DRIVE_MAX);
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
    use super::*;

    const SAMPLE_RATE: f32 = 48000.0;

    /// Gain of the filter at `freq` for a sine of amplitude `amp`: process
    /// 0.2s, compare RMS of the last half of output vs input (past smoothing
    /// and transients). Small amplitudes keep the analog models near-linear.
    fn gain_at_amp(filter: &mut Filter, freq: f32, amp: f32) -> f32 {
        let frames = (SAMPLE_RATE * 0.2) as usize;
        let input: Vec<f32> = (0..frames)
            .map(|n| amp * (TAU * freq * n as f32 / SAMPLE_RATE).sin())
            .collect();
        let mut out_l = vec![0.0f32; frames];
        let mut out_r = vec![0.0f32; frames];
        {
            let ins: Vec<&[f32]> = vec![&input, &input];
            let mut outs: Vec<&mut [f32]> = vec![&mut out_l, &mut out_r];
            filter.process(&[], &ins, &mut outs).unwrap();
        }
        let rms = |s: &[f32]| {
            let tail = &s[s.len() / 2..];
            (tail.iter().map(|x| x * x).sum::<f32>() / tail.len() as f32).sqrt()
        };
        rms(&out_l) / rms(&input)
    }

    fn gain_at(filter: &mut Filter, freq: f32) -> f32 {
        gain_at_amp(filter, freq, 1.0)
    }

    /// Filter with cutoff at 0.5 → 20 * 1000^0.5 ≈ 632 Hz.
    fn filter_at_mid_cutoff(mode: f32) -> Filter {
        let mut f = Filter::new(SAMPLE_RATE);
        f.set_parameter(0, 0.5).unwrap();
        f.set_parameter(2, mode).unwrap();
        // Skip the smoothing ramp so the measurement window is settled.
        f.cutoff_smoothed = 0.5;
        f
    }

    // --- Goertzel-based spectral analysis -------------------------------
    //
    // RMS gain conflates the fundamental with harmonic distortion, which is
    // misleading for the saturating analog modes. These helpers extract the
    // amplitude at a single frequency so we can separate the true magnitude
    // response from distortion and aliasing.

    /// Run a sine of amplitude `amp` at `freq` through the filter for `secs`
    /// and return the settled second-half of the (left) output.
    fn run_sine(filter: &mut Filter, freq: f32, amp: f32, secs: f32) -> Vec<f32> {
        let frames = (SAMPLE_RATE * secs) as usize;
        let input: Vec<f32> = (0..frames)
            .map(|n| amp * (TAU * freq * n as f32 / SAMPLE_RATE).sin())
            .collect();
        let mut out_l = vec![0.0f32; frames];
        let mut out_r = vec![0.0f32; frames];
        {
            let ins: Vec<&[f32]> = vec![&input, &input];
            let mut outs: Vec<&mut [f32]> = vec![&mut out_l, &mut out_r];
            filter.process(&[], &ins, &mut outs).unwrap();
        }
        out_l.split_off(frames / 2)
    }

    /// Hann-windowed single-bin amplitude at `freq` (relative units; the
    /// window scale cancels when taking ratios of two such measurements).
    fn goertzel(samples: &[f32], freq: f32) -> f32 {
        let n = samples.len();
        let omega = TAU * freq / SAMPLE_RATE;
        let coeff = 2.0 * omega.cos();
        let (mut s1, mut s2) = (0.0f32, 0.0f32);
        for (i, &x) in samples.iter().enumerate() {
            let w = 0.5 - 0.5 * (TAU * i as f32 / (n as f32 - 1.0)).cos();
            let s = x * w + coeff * s1 - s2;
            s2 = s1;
            s1 = s;
        }
        let real = s1 - s2 * omega.cos();
        let imag = s2 * omega.sin();
        (real * real + imag * imag).sqrt()
    }

    /// Magnitude response (fundamental only) at `freq`, in dB.
    fn mag_db(make: impl Fn() -> Filter, freq: f32, amp: f32) -> f32 {
        let out = run_sine(&mut make(), freq, amp, 0.4);
        let inp: Vec<f32> = (0..out.len())
            .map(|n| amp * (TAU * freq * n as f32 / SAMPLE_RATE).sin())
            .collect();
        let g = goertzel(&out, freq) / goertzel(&inp, freq);
        20.0 * g.max(1e-9).log10()
    }

    /// Total harmonic distortion (%) at `freq`: harmonic energy / fundamental.
    fn thd_pct(make: impl Fn() -> Filter, freq: f32, amp: f32) -> f32 {
        let out = run_sine(&mut make(), freq, amp, 0.4);
        let fund = goertzel(&out, freq);
        let mut harm_sq = 0.0;
        let mut h = 2.0;
        while h * freq < SAMPLE_RATE * 0.45 {
            let m = goertzel(&out, h * freq);
            harm_sq += m * m;
            h += 1.0;
        }
        100.0 * harm_sq.sqrt() / fund.max(1e-9)
    }

    /// Find the frequency of maximum gain (resonant peak) by log-scanning,
    /// at a small amplitude to stay near-linear.
    fn measured_peak_hz(make: impl Fn() -> Filter) -> f32 {
        let mut best_f = 20.0;
        let mut best_g = 0.0;
        let mut f = 20.0;
        while f < 18_000.0 {
            let g = gain_at_amp(&mut make(), f, 0.02);
            if g > best_g {
                best_g = g;
                best_f = f;
            }
            f *= 1.02;
        }
        best_f
    }

    /// Build an analog-mode filter settled at a normalized cutoff and Q.
    fn analog(mode: f32, norm: f32, q: f32) -> Filter {
        let mut f = Filter::new(SAMPLE_RATE);
        f.set_parameter(2, mode).unwrap();
        f.set_parameter(0, norm).unwrap();
        f.set_parameter(1, q).unwrap();
        f.cutoff_smoothed = norm;
        f.q_smoothed = q;
        f
    }

    #[test]
    #[ignore]
    fn diag_cutoff_consistency() {
        // -3 dB point vs knob for every mode at default resonance. The goal:
        // all modes land near 1.0 so `cutoff` means the same thing.
        for (mode, name) in [
            (0.0, "lowpass"), (4.0, "ladder"), (5.0, "acid"), (6.0, "ms20"),
        ] {
            print!("{name:8}:");
            for norm in [0.3, 0.5, 0.7] {
                let fc = Filter::cutoff_hz(norm);
                // Scan for the -3 dB crossing (normalized to passband).
                let g0 = mag_db(|| analog(mode, norm, 0.707), 30.0, 0.03);
                let mut prev_f = 30.0;
                let mut prev = 0.0;
                let mut f = 35.0;
                let mut m3 = 0.0;
                while f < 20_000.0 {
                    let d = mag_db(|| analog(mode, norm, 0.707), f, 0.03) - g0;
                    if d <= -3.0 && prev > -3.0 {
                        let t = (-3.0 - prev) / (d - prev);
                        m3 = prev_f * (f / prev_f).powf(t);
                        break;
                    }
                    prev_f = f;
                    prev = d;
                    f *= 1.02;
                }
                print!("  {norm:.1}->{:.2}", m3 / fc);
            }
            println!();
        }
    }

    #[test]
    #[ignore]
    fn diag_full_report() {
        let norm = 0.5; // knob ≈ 632 Hz
        let fc = Filter::cutoff_hz(norm);
        let freqs = [50.0, 100.0, 200.0, 316.0, 447.0, 632.0, 894.0, 1265.0,
            1790.0, 2530.0, 3580.0, 5060.0, 7160.0, 10120.0];

        for (mode, name) in [(4.0, "LADDER"), (5.0, "ACID")] {
            println!("\n######## {name}  (knob {fc:.0} Hz) ########");
            for q in [0.707, 4.0, 9.0] {
                println!("\n--- magnitude response, Q={q} (fundamental, dB) ---");
                for f in freqs {
                    let db = mag_db(|| analog(mode, norm, q), f, 0.05);
                    let bar = "#".repeat(((db + 48.0).max(0.0) / 2.0) as usize);
                    println!("  {f:6.0} Hz: {db:7.1} dB {bar}");
                }
            }

            println!("\n--- stopband slope (dB/oct) at Q=0.707, octaves above fc ---");
            for (lo, hi) in [(2.0, 4.0), (4.0, 8.0), (8.0, 16.0)] {
                let a = mag_db(|| analog(mode, norm, 0.707), fc * lo, 0.05);
                let b = mag_db(|| analog(mode, norm, 0.707), fc * hi, 0.05);
                println!("  {lo:.0}fc->{hi:.0}fc: {:.1} dB/oct", b - a);
            }

            println!("\n--- peak height & THD at cutoff vs Q (amp 0.1) ---");
            for q in [0.707, 2.0, 4.0, 6.0, 8.0, 10.0] {
                let pk = mag_db(|| analog(mode, norm, q), fc, 0.1);
                let thd = thd_pct(|| analog(mode, norm, q), fc, 0.1);
                println!("  Q={q:5.2}: peak {pk:6.1} dB, THD {thd:5.1}%");
            }

            println!("\n--- THD vs drive at cutoff (Q=4, amp 0.3) ---");
            for d in [1.0, 2.0, 4.0, 7.0, 10.0] {
                let thd = thd_pct(
                    || {
                        let mut f = analog(mode, norm, 4.0);
                        f.set_parameter(3, d).unwrap();
                        f.drive_smoothed = d;
                        f
                    },
                    fc,
                    0.3,
                );
                println!("  drive {d:4.1}: THD {thd:5.1}%");
            }

            // Realistic: low fundamental, cutoff ~1 octave above it, so the
            // saturation harmonics land in the passband (not the stopband).
            println!("\n--- THD vs drive, f0=110 Hz, cutoff ~1.2 kHz (Q=4, amp 0.5) ---");
            let cutoff_norm = 0.593; // ≈ 1.2 kHz
            for d in [1.0, 2.0, 4.0, 7.0, 10.0] {
                let thd = thd_pct(
                    || {
                        let mut f = analog(mode, cutoff_norm, 4.0);
                        f.set_parameter(3, d).unwrap();
                        f.drive_smoothed = d;
                        f
                    },
                    110.0,
                    0.5,
                );
                println!("  drive {d:4.1}: THD {thd:5.1}%");
            }
        }

        // Loudness staging: a band-limited sawtooth through the filter at a
        // fixed setting, broadband output RMS as resonance and drive rise.
        // A musical filter shouldn't lurch in level when you turn these up.
        println!("\n######## LOUDNESS (sawtooth in, broadband out RMS) ########");
        let saw = |frames: usize| -> Vec<f32> {
            // Sum of harmonics of 110 Hz up to ~8 kHz, amplitude ~0.3.
            (0..frames)
                .map(|n| {
                    let t = n as f32 / SAMPLE_RATE;
                    let mut s = 0.0;
                    let mut h = 1.0;
                    while h * 110.0 < 8000.0 {
                        s += (TAU * 110.0 * h * t).sin() / h;
                        h += 1.0;
                    }
                    0.3 * s
                })
                .collect()
        };
        let out_rms = |f: &mut Filter, input: &[f32]| -> f32 {
            let mut out_l = vec![0.0f32; input.len()];
            let mut out_r = vec![0.0f32; input.len()];
            {
                let ins: Vec<&[f32]> = vec![input, input];
                let mut outs: Vec<&mut [f32]> = vec![&mut out_l, &mut out_r];
                f.process(&[], &ins, &mut outs).unwrap();
            }
            let tail = &out_l[out_l.len() / 2..];
            (tail.iter().map(|x| x * x).sum::<f32>() / tail.len() as f32).sqrt()
        };
        let input = saw((SAMPLE_RATE * 0.3) as usize);
        let in_rms = {
            let t = &input[input.len() / 2..];
            (t.iter().map(|x| x * x).sum::<f32>() / t.len() as f32).sqrt()
        };
        println!("  input rms {in_rms:.3}");
        println!("  ladder, knob 632 Hz, out rms vs Q:");
        for q in [0.707, 2.0, 4.0, 6.0, 8.0, 10.0] {
            let r = out_rms(&mut analog(4.0, 0.5, q), &input);
            println!("    Q={q:5.2}: {r:.3} ({:+.1} dB)", 20.0 * (r / in_rms).log10());
        }
        println!("  ladder, Q=4, out rms vs drive:");
        for d in [1.0, 2.0, 4.0, 7.0, 10.0] {
            let mut f = analog(4.0, 0.5, 4.0);
            f.set_parameter(3, d).unwrap();
            f.drive_smoothed = d;
            let r = out_rms(&mut f, &input);
            println!("    drive {d:4.1}: {r:.3} ({:+.1} dB)", 20.0 * (r / in_rms).log10());
        }

        // Self-oscillation: kick with an impulse on silence, measure the
        // tail's dominant frequency, amplitude, and purity.
        println!("\n######## SELF-OSCILLATION (ladder, Q=10) ########");
        for norm in [0.3, 0.5, 0.7] {
            let fc = Filter::cutoff_hz(norm);
            let frames = (SAMPLE_RATE * 1.0) as usize;
            let mut input = vec![0.0f32; frames];
            input[0] = 1.0;
            let mut out_l = vec![0.0f32; frames];
            let mut out_r = vec![0.0f32; frames];
            {
                let mut f = analog(4.0, norm, 10.0);
                let ins: Vec<&[f32]> = vec![&input, &input];
                let mut outs: Vec<&mut [f32]> = vec![&mut out_l, &mut out_r];
                f.process(&[], &ins, &mut outs).unwrap();
            }
            let tail = &out_l[out_l.len() * 3 / 4..];
            let rms = (tail.iter().map(|x| x * x).sum::<f32>() / tail.len() as f32).sqrt();
            // Dominant frequency: scan bins near fc.
            let mut best_f = 0.0;
            let mut best_m = 0.0;
            let mut ff = fc * 0.5;
            while ff < fc * 2.0 {
                let m = goertzel(tail, ff);
                if m > best_m {
                    best_m = m;
                    best_f = ff;
                }
                ff *= 1.01;
            }
            println!(
                "  knob {fc:6.0} Hz: osc {best_f:6.0} Hz (ratio {:.2}), tail rms {rms:.3}",
                best_f / fc
            );
        }

        // Aliasing: a 2.4 kHz tone, high cutoff, full resonance + drive.
        // Alias products land at non-harmonic frequencies; sum their energy.
        println!("\n######## ALIASING (ladder, fc≈7 kHz, Q=10, drive 8) ########");
        {
            let f0 = 2400.0;
            let out = run_sine(
                &mut {
                    let mut f = analog(4.0, 0.85, 10.0);
                    f.set_parameter(3, 8.0).unwrap();
                    f.drive_smoothed = 8.0;
                    f
                },
                f0,
                0.5,
                0.5,
            );
            let fund = goertzel(&out, f0);
            // Non-harmonic probe frequencies (won't coincide with n*f0).
            let mut alias = 0.0;
            for probe in [370.0, 910.0, 1450.0, 1990.0, 3100.0, 4300.0] {
                let m = goertzel(&out, probe);
                alias += m * m;
            }
            println!(
                "  fundamental {:.3}, inharmonic energy {:.4} ({:.1}% of fund)",
                fund,
                alias.sqrt(),
                100.0 * alias.sqrt() / fund.max(1e-9)
            );
        }
    }

    /// Ratio of the measured -3 dB point to the knob frequency, at default
    /// resonance, using fundamental-only (Goertzel) magnitudes.
    fn minus3db_ratio(mode: f32, norm: f32) -> f32 {
        let fc = Filter::cutoff_hz(norm);
        let g0 = mag_db(|| analog(mode, norm, Q_DEFAULT), 30.0, 0.03);
        let mut prev_f = 30.0;
        let mut prev = 0.0;
        let mut f = 35.0;
        while f < 20_000.0 {
            let d = mag_db(|| analog(mode, norm, Q_DEFAULT), f, 0.03) - g0;
            if d <= -3.0 && prev > -3.0 {
                let t = (-3.0 - prev) / (d - prev);
                return prev_f * (f / prev_f).powf(t) / fc;
            }
            prev_f = f;
            prev = d;
            f *= 1.02;
        }
        f / fc
    }

    #[test]
    fn cutoff_marks_minus3db_across_modes() {
        // The consistency fix: `cutoff` marks the -3 dB point for every type
        // (not the per-stage corner), so switching `type` at the same knob
        // keeps the same brightness instead of jumping ~an octave darker.
        for mode in [0.0, 4.0, 5.0, 6.0] {
            for norm in [0.35, 0.55] {
                let ratio = minus3db_ratio(mode, norm);
                assert!(
                    (0.82..1.22).contains(&ratio),
                    "mode {mode} should mark the -3 dB point at the knob: ratio {ratio:.2}"
                );
            }
        }
    }

    #[test]
    fn ladder_resonant_peak_tracks_knob() {
        // `cutoff` marks the -3 dB point, so the resonant peak sits above the
        // knob — but it must track *proportionally* (a constant ratio), so
        // sweeping the knob sweeps the peak musically across the range.
        let ratios: Vec<f32> = [0.3, 0.45, 0.6, 0.75]
            .iter()
            .map(|&norm| measured_peak_hz(|| analog(4.0, norm, 9.0)) / Filter::cutoff_hz(norm))
            .collect();
        let min = ratios.iter().copied().fold(f32::MAX, f32::min);
        let max = ratios.iter().copied().fold(0.0f32, f32::max);
        assert!(
            max / min < 1.15,
            "ladder peak should track the knob proportionally: ratios {ratios:?}"
        );
        assert!(min > 1.0, "peak sits above the -3 dB knob: ratios {ratios:?}");
    }

    #[test]
    fn acid_keeps_resonance_in_the_bass() {
        // The 303 lives down low. The feedback high-pass must not kill the
        // resonance there: at a low knob with high Q, the resonant peak
        // should tower over the passband.
        for norm in [0.2, 0.3, 0.45] {
            let peak_f = measured_peak_hz(|| analog(5.0, norm, 9.0));
            let peak_g = mag_db(|| analog(5.0, norm, 9.0), peak_f, 0.03);
            let pass_g = mag_db(|| analog(5.0, norm, 9.0), 35.0, 0.03);
            assert!(
                peak_g - pass_g > 10.0,
                "acid should keep a strong bass resonance: knob norm {norm}, peak {peak_g:.1} dB vs passband {pass_g:.1} dB"
            );
        }
    }

    /// Composite -3 dB point of an analog mode at a given input amplitude,
    /// normalized by passband gain so makeup/drive don't bias the crossing.
    fn analog_cutoff_at_amp(mode: f32, norm: f32, amp: f32) -> f32 {
        let g0 = gain_at_amp(&mut analog(mode, norm, Q_DEFAULT), 20.0, amp);
        let mut prev_f = 20.0;
        let mut prev_g = 1.0;
        let mut f = 25.0;
        while f < 20_000.0 {
            let g = gain_at_amp(&mut analog(mode, norm, Q_DEFAULT), f, amp) / g0;
            if g <= std::f32::consts::FRAC_1_SQRT_2 && prev_g > std::f32::consts::FRAC_1_SQRT_2 {
                let t = (std::f32::consts::FRAC_1_SQRT_2 - prev_g) / (g - prev_g);
                return prev_f * (f / prev_f).powf(t);
            }
            prev_f = f;
            prev_g = g;
            f *= 1.03;
        }
        f
    }

    #[test]
    fn ladder_cutoff_is_level_independent() {
        // The headline fix: the per-stage tanh cascade used to drag the
        // cutoff down as input level rose (it got darker the harder you
        // played). The ZDF core holds the cutoff steady regardless of level.
        let quiet = analog_cutoff_at_amp(4.0, 0.5, 0.02);
        let loud = analog_cutoff_at_amp(4.0, 0.5, 1.0);
        let ratio = loud / quiet;
        assert!(
            (0.9..1.1).contains(&ratio),
            "ladder cutoff should not move with level: quiet {quiet:.0} Hz, loud {loud:.0} Hz, ratio {ratio:.2}"
        );
    }

    #[test]
    fn parameters_match_param_count() {
        let f = Filter::new(SAMPLE_RATE);
        assert_eq!(f.parameters().len(), PARAM_COUNT);
    }

    #[test]
    fn type_param_has_labels() {
        let f = Filter::new(SAMPLE_RATE);
        let params = f.parameters();
        let labels = params[2].labels.as_ref().expect("type should have labels");
        assert_eq!(labels.len(), TYPE_NAMES.len());
        assert_eq!(params[2].max, (TYPE_NAMES.len() - 1) as f32);
        assert!(params[0].labels.is_none());
    }

    #[test]
    fn parameters_clamp_and_roundtrip() {
        let mut f = Filter::new(SAMPLE_RATE);
        f.set_parameter(0, 0.3).unwrap();
        assert_eq!(f.get_parameter(0), Some(0.3));
        f.set_parameter(0, 7.0).unwrap();
        assert_eq!(f.get_parameter(0), Some(1.0));
        f.set_parameter(1, 100.0).unwrap();
        assert_eq!(f.get_parameter(1), Some(Q_MAX));
        f.set_parameter(2, 99.0).unwrap();
        assert_eq!(f.get_parameter(2), Some(6.0));
        f.set_parameter(3, 99.0).unwrap();
        assert_eq!(f.get_parameter(3), Some(DRIVE_MAX));
        assert!(f.set_parameter(4, 1.0).is_err());
    }

    #[test]
    fn lowpass_passes_low_cuts_high() {
        // Cutoff ≈ 632 Hz: 100 Hz passes, 6.3 kHz (a decade up) is ~-40 dB.
        let g_low = gain_at(&mut filter_at_mid_cutoff(0.0), 100.0);
        let g_high = gain_at(&mut filter_at_mid_cutoff(0.0), 6320.0);
        assert!(g_low > 0.9, "lowpass should pass 100 Hz, gain {g_low}");
        assert!(g_high < 0.05, "lowpass should cut 6.3 kHz, gain {g_high}");
    }

    #[test]
    fn highpass_cuts_low_passes_high() {
        let g_low = gain_at(&mut filter_at_mid_cutoff(1.0), 100.0);
        let g_high = gain_at(&mut filter_at_mid_cutoff(1.0), 6320.0);
        assert!(g_low < 0.06, "highpass should cut 100 Hz, gain {g_low}");
        assert!(g_high > 0.9, "highpass should pass 6.3 kHz, gain {g_high}");
    }

    #[test]
    fn highpass_sweeps_across_cutoff() {
        // Moving the cutoff must audibly change a fixed 500 Hz tone: low
        // cutoff passes it, high cutoff blocks it. (Confirms the highpass
        // responds to the knob — the DSP, independent of chain dry/wet mix.)
        let probe = 500.0;
        let open = {
            let mut f = Filter::new(SAMPLE_RATE); // highpass, cutoff low
            f.set_parameter(2, 1.0).unwrap();
            f.set_parameter(0, 0.1).unwrap();
            f.cutoff_smoothed = 0.1;
            gain_at_amp(&mut f, probe, 0.1)
        };
        let closed = {
            let mut f = Filter::new(SAMPLE_RATE); // highpass, cutoff high
            f.set_parameter(2, 1.0).unwrap();
            f.set_parameter(0, 0.9).unwrap();
            f.cutoff_smoothed = 0.9;
            gain_at_amp(&mut f, probe, 0.1)
        };
        assert!(open > 0.9, "low cutoff should pass 500 Hz, gain {open}");
        assert!(closed < 0.1, "high cutoff should block 500 Hz, gain {closed}");
    }

    #[test]
    fn bandpass_peaks_at_cutoff() {
        let g_center = gain_at(&mut filter_at_mid_cutoff(2.0), 632.0);
        let g_low = gain_at(&mut filter_at_mid_cutoff(2.0), 100.0);
        let g_high = gain_at(&mut filter_at_mid_cutoff(2.0), 4000.0);
        assert!(
            (g_center - 1.0).abs() < 0.15,
            "bandpass should be ~unity at center, gain {g_center}"
        );
        assert!(g_low < 0.3, "bandpass should attenuate below, gain {g_low}");
        assert!(g_high < 0.3, "bandpass should attenuate above, gain {g_high}");
    }

    #[test]
    fn notch_kills_center_passes_ends() {
        let g_center = gain_at(&mut filter_at_mid_cutoff(3.0), 632.0);
        let g_low = gain_at(&mut filter_at_mid_cutoff(3.0), 50.0);
        assert!(g_center < 0.05, "notch should kill the center, gain {g_center}");
        assert!(g_low > 0.9, "notch should pass far frequencies, gain {g_low}");
    }

    #[test]
    fn open_lowpass_is_transparent() {
        // Default state: lowpass fully open — audio passes ~unchanged.
        let mut f = Filter::new(SAMPLE_RATE);
        let g = gain_at(&mut f, 1000.0);
        assert!((g - 1.0).abs() < 0.05, "open lowpass should be ~unity, gain {g}");
    }

    #[test]
    fn resonance_boosts_cutoff_region() {
        let mut f = filter_at_mid_cutoff(0.0);
        f.set_parameter(1, 8.0).unwrap();
        f.q_smoothed = 8.0;
        let g = gain_at(&mut f, 632.0);
        assert!(g > 2.0, "high Q should boost at cutoff, gain {g}");
    }

    // --- Analog models ---

    #[test]
    fn ladder_is_steep_and_passes_lows() {
        // 24 dB/oct: a decade above cutoff is ~-80 dB. Small amplitude keeps
        // the tanh stages near-linear.
        let g_low = gain_at_amp(&mut filter_at_mid_cutoff(4.0), 100.0, 0.2);
        let g_high = gain_at_amp(&mut filter_at_mid_cutoff(4.0), 6320.0, 0.2);
        assert!(
            (0.5..1.6).contains(&g_low),
            "ladder should pass lows near unity, gain {g_low}"
        );
        assert!(g_high < 0.01, "ladder should cut hard above, gain {g_high}");
        assert!(
            g_high < gain_at(&mut filter_at_mid_cutoff(0.0), 6320.0),
            "ladder rolloff should be steeper than the 12 dB SVF"
        );
    }

    #[test]
    fn acid_is_a_lowpass_with_squelch_tap() {
        let g_low = gain_at_amp(&mut filter_at_mid_cutoff(5.0), 100.0, 0.2);
        let g_high = gain_at_amp(&mut filter_at_mid_cutoff(5.0), 6320.0, 0.2);
        assert!(
            (0.5..1.8).contains(&g_low),
            "acid should pass lows near unity, gain {g_low}"
        );
        assert!(g_high < 0.03, "acid should cut above (18 dB/oct), gain {g_high}");
    }

    #[test]
    fn ms20_resonance_screams() {
        // Measure at the actual resonant peak (which sits above the -3 dB knob).
        let peak_f = measured_peak_hz(|| analog(6.0, 0.5, Q_MAX));
        let g = gain_at_amp(&mut analog(6.0, 0.5, Q_MAX), peak_f, 0.1);
        assert!(g > 2.0, "ms20 at full resonance should boost hard, gain {g} at {peak_f:.0} Hz");
        // And it still filters well above the peak.
        let g_high = gain_at_amp(&mut analog(6.0, 0.5, Q_DEFAULT), 8000.0, 0.2);
        assert!(g_high < 0.3, "ms20 should attenuate well above cutoff, gain {g_high}");
    }

    #[test]
    fn ladder_self_oscillates_at_full_resonance() {
        let rms_tail = |f: &mut Filter| {
            // Kick with a short impulse, then run on silence.
            let frames = (SAMPLE_RATE * 0.5) as usize;
            let mut input = vec![0.0f32; frames];
            input[0] = 1.0;
            let mut out_l = vec![0.0f32; frames];
            let mut out_r = vec![0.0f32; frames];
            let ins: Vec<&[f32]> = vec![&input, &input];
            let mut outs: Vec<&mut [f32]> = vec![&mut out_l, &mut out_r];
            f.process(&[], &ins, &mut outs).unwrap();
            let tail = &out_l[out_l.len() * 3 / 4..];
            (tail.iter().map(|x| x * x).sum::<f32>() / tail.len() as f32).sqrt()
        };

        let mut hot = filter_at_mid_cutoff(4.0);
        hot.set_parameter(1, Q_MAX).unwrap();
        hot.q_smoothed = Q_MAX;
        let ringing = rms_tail(&mut hot);
        assert!(
            ringing > 0.05,
            "ladder at full resonance should self-oscillate, rms {ringing}"
        );

        let mut cold = filter_at_mid_cutoff(4.0);
        let silent = rms_tail(&mut cold);
        assert!(
            silent < 1e-3,
            "ladder at low resonance should decay to silence, rms {silent}"
        );
    }

    #[test]
    fn drive_saturates_and_stays_bounded() {
        // Drive saturates the input (adds harmonics) rather than auto-
        // attenuating. The contract: even at heavy drive, full resonance,
        // and a loud input the output stays finite and bounded — the tanh
        // input stage and tanh-bounded feedback can't blow up.
        let frames = (SAMPLE_RATE * 0.1) as usize;
        let input: Vec<f32> = (0..frames)
            .map(|n| 0.8 * (TAU * 200.0 * n as f32 / SAMPLE_RATE).sin())
            .collect();
        let mut out_l = vec![0.0f32; frames];
        let mut out_r = vec![0.0f32; frames];
        let ins: Vec<&[f32]> = vec![&input, &input];
        let mut outs: Vec<&mut [f32]> = vec![&mut out_l, &mut out_r];
        let mut f = filter_at_mid_cutoff(4.0);
        f.set_parameter(1, Q_MAX).unwrap();
        f.set_parameter(3, DRIVE_MAX).unwrap();
        f.process(&[], &ins, &mut outs).unwrap();
        let peak = out_l.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!(peak.is_finite(), "drive output must be finite");
        assert!(peak < 8.0, "analog output should stay bounded, peak {peak}");

        // Driving harder adds energy (harmonics) rather than removing it.
        let mut clean = filter_at_mid_cutoff(4.0);
        let g_clean = gain_at_amp(&mut clean, 200.0, 0.5);
        let mut hot = filter_at_mid_cutoff(4.0);
        hot.set_parameter(3, DRIVE_MAX).unwrap();
        hot.drive_smoothed = DRIVE_MAX;
        let g_hot = gain_at_amp(&mut hot, 200.0, 0.5);
        assert!(
            g_hot > g_clean && g_hot.is_finite(),
            "drive should add level/harmonics, hot {g_hot} vs clean {g_clean}"
        );
    }

    #[test]
    fn switching_type_resets_state() {
        let mut f = filter_at_mid_cutoff(4.0);
        f.set_parameter(1, Q_MAX).unwrap();
        f.q_smoothed = Q_MAX;
        // Get it ringing, then switch type — the ring must not carry over.
        let frames = (SAMPLE_RATE * 0.2) as usize;
        let mut input = vec![0.0f32; frames];
        input[0] = 1.0;
        let mut out_l = vec![0.0f32; frames];
        let mut out_r = vec![0.0f32; frames];
        {
            let ins: Vec<&[f32]> = vec![&input, &input];
            let mut outs: Vec<&mut [f32]> = vec![&mut out_l, &mut out_r];
            f.process(&[], &ins, &mut outs).unwrap();
        }
        f.set_parameter(2, 0.0).unwrap(); // switch to clean lowpass
        let silence = vec![0.0f32; frames];
        let mut out_l2 = vec![0.0f32; frames];
        let mut out_r2 = vec![0.0f32; frames];
        let ins: Vec<&[f32]> = vec![&silence, &silence];
        let mut outs: Vec<&mut [f32]> = vec![&mut out_l2, &mut out_r2];
        f.process(&[], &ins, &mut outs).unwrap();
        let peak = out_l2.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!(peak < 1e-4, "state should reset on type switch, peak {peak}");
    }
}
