//! The limiter on the output mix (step 102): a look-ahead peak limiter, so that what is sent to the sound card does not go over a ceiling, and a mix that is too hot is brought down
//! and not clipped.
//!
//! **How it works.** Every frame the loudest sample of either channel says what gain would bring it down to the ceiling (`wanted`: 1 for a frame that is under it). The channels
//! are linked, so that the stereo image does not move when one side is limited. The sound is held back by `window - 1` frames (the look-ahead, 2 ms), and each frame that comes out
//! has the gain that was worked out for it a window later: the *least* gain that was wanted over the frames from it to the look-ahead after it (a sliding minimum, so that the gain is
//! already down when a peak arrives), which goes up again no faster than the release lets it (an exponential), and is then *averaged* over `window` frames (so that it goes down in a ramp
//! the length of the look-ahead and not in one frame, which would click). Every gain that is in the average is at most what the frame itself wanted, so the frame comes out at the ceiling or
//! under it: the clamp at the ceiling at the end is for the last bit of rounding only.
//!
//! A sample that is not a number is not put in (it would stay in the gain for ever): it is a silence to the limiter. Nothing allocates once the limiter is made (the lines are made for the
//! longest look-ahead, `MAX_WINDOW`, whatever the rate is), and only `+ - * /`, `min` and `max` run for each frame, so that the same frames in come out as the same bits on any platform and in any
//! blocks (`the_result_is_the_same_bits_whatever_the_blocks_it_is_given_in`).

use super::CHAIN_CHANNELS;

/// How far ahead the limiter looks, in seconds: the time it has to bring the gain down before a peak, and how late the sound that it limits is.
pub(super) const LOOKAHEAD_SEC: f32 = 0.002;

/// How fast the gain comes back up after it has been brought down, as the time constant of an exponential, in seconds.
pub(super) const RELEASE_SEC: f32 = 0.08;

/// The longest window there is room for, in frames (a power of two, for the ring of the sliding minimum): 2 ms at up to 256 kHz.
const MAX_WINDOW: usize = 512;

/// The shortest window: a look-ahead of one frame, which is no look-ahead (the ramp is two frames long).
const MIN_WINDOW: usize = 2;

/// The sample rate that a limiter starts at, until it is told another.
const DEFAULT_RATE: f32 = 48_000.0;

pub(super) struct Limiter {
    /// How many frames the gain is averaged over, and the look-ahead is one frame less: how late the sound comes out.
    window: usize,
    /// How much of the way up to 1 the gain goes each frame: `1 - exp(-1 / (RELEASE_SEC * rate))`. The gain is kept in 64 bits: a one-pole in 32 stops short of what it follows by the
    /// step of its number over twice its coefficient (a ten-thousandth here), so that the gain would never be back at 1 and a sound under the ceiling would never come out as it went in.
    release: f64,
    /// The sound that is held back, a ring of `window` frames for each channel (made for `MAX_WINDOW`).
    delay: [Vec<f32>; CHAIN_CHANNELS],
    /// The gain after the release for each of the last `window` frames (a ring, made for `MAX_WINDOW`) and the sum of them, which the average is made from.
    smooth: Vec<f64>,
    sum: f64,
    /// The gain after the release at the frame before.
    last: f64,
    /// Where the frame that comes in is written in the rings: the frame number, brought round at the window.
    at: usize,
    /// The sliding minimum of what is wanted: the gains that can still be the least of a window, from the lowest, with the number of the frame that each was wanted at, as a ring of
    /// `MAX_WINDOW` places that starts at `head` and has `len` in it.
    wanted: Vec<f32>,
    wanted_at: Vec<u64>,
    head: usize,
    len: usize,
    /// How many frames have come in.
    frame: u64,
}

impl Limiter {
    pub(super) fn new() -> Self {
        let mut limiter = Limiter {
            window: MIN_WINDOW,
            release: 0.0,
            delay: std::array::from_fn(|_| vec![0.0; MAX_WINDOW]),
            smooth: vec![1.0; MAX_WINDOW],
            sum: 0.0,
            last: 1.0,
            at: 0,
            wanted: vec![1.0; MAX_WINDOW],
            wanted_at: vec![0; MAX_WINDOW],
            head: 0,
            len: 0,
            frame: 0,
        };
        limiter.configure(DEFAULT_RATE);
        limiter
    }

    /// Set the look-ahead and the release for a sample rate. A window of another length starts the limiter again (what it holds back is let go of, and the gain is 1): this is
    /// said once, when the session is loaded, and not while a note sounds. A rate that is not a rate leaves it as it was.
    pub(super) fn configure(&mut self, rate: f32) {
        if !(rate.is_finite() && rate > 0.0) {
            return;
        }
        let window = ((LOOKAHEAD_SEC * rate).round() as usize).clamp(MIN_WINDOW, MAX_WINDOW);
        self.release = 1.0 - (-1.0 / (f64::from(RELEASE_SEC) * f64::from(rate))).exp();
        if window != self.window {
            self.window = window;
            self.reset();
        }
    }

    /// Let go of everything the limiter holds: no sound is held back, and the gain is 1.
    fn reset(&mut self) {
        for line in &mut self.delay {
            line.fill(0.0);
        }
        self.smooth.fill(1.0);
        self.sum = self.window as f64;
        self.last = 1.0;
        self.at = 0;
        self.head = 0;
        self.len = 0;
        self.frame = 0;
    }

    /// How many frames late the sound comes out.
    #[cfg(test)]
    pub(super) fn latency(&self) -> usize {
        self.window - 1
    }

    /// Take in one frame, a sample for each channel, and give the frame that is due: `window - 1` frames older, with the gain that it has been given (the frame has gone through at the same
    /// time as the frame that came in, so that it is never over `ceiling`, a linear level above 0), and that gain.
    #[inline]
    pub(super) fn step(&mut self, frame: [f32; CHAIN_CHANNELS], ceiling: f32) -> ([f32; CHAIN_CHANNELS], f32) {
        let frame = frame.map(|sample| if sample.is_finite() { sample } else { 0.0 });
        let peak = frame.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()));
        let wanted = if peak > ceiling { ceiling / peak } else { 1.0 };

        // The least gain that was wanted in the last `window` frames, this one included: the ones that are too old to be in them are let go of first, and then those that are not
        // lower than this one, which can never be the least again while it is in the window.
        let window = self.window as u64;
        while self.len > 0 && self.wanted_at[self.head] + window <= self.frame {
            self.head = (self.head + 1) & (MAX_WINDOW - 1);
            self.len -= 1;
        }
        while self.len > 0 && self.wanted[(self.head + self.len - 1) & (MAX_WINDOW - 1)] >= wanted {
            self.len -= 1;
        }
        let back = (self.head + self.len) & (MAX_WINDOW - 1);
        self.wanted[back] = wanted;
        self.wanted_at[back] = self.frame;
        self.len += 1;
        let least = self.wanted[self.head];

        // The gain comes down at once to what the window wants, and goes up by the release.
        let held = f64::from(least).min(self.last + self.release * (1.0 - self.last));
        self.last = held;

        // The average of the gains of the last `window` frames: the one that is replaced is the one that was `window` frames ago.
        self.sum += held - self.smooth[self.at];
        self.smooth[self.at] = held;
        let gain = (self.sum / self.window as f64) as f32;

        // The frame that is due was put in `window - 1` frames ago, which is the place after the one that this frame goes to.
        let due = if self.at + 1 == self.window { 0 } else { self.at + 1 };
        let mut out = [0.0; CHAIN_CHANNELS];
        for (channel, out) in out.iter_mut().enumerate() {
            let old = self.delay[channel][due];
            self.delay[channel][self.at] = frame[channel];
            // (Only the last bit of rounding can take the sound over the ceiling here.)
            *out = (old * gain).clamp(-ceiling, ceiling);
        }

        self.at += 1;
        if self.at == self.window {
            self.at = 0;
            // The sum of the ring is worked out again once a window, so that what it has of rounding does not build up: it is the frames' own that decide it, and not the blocks'.
            self.sum = self.smooth[..self.window].iter().sum();
        }
        self.frame += 1;
        (out, gain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 48_000.0;

    /// A linear level for a number of decibels below full scale.
    fn linear(db: f32) -> f32 {
        10f32.powf(db / 20.0)
    }

    /// A small generator of numbers from -1 to 1 that does not change from run to run.
    struct Noise(u64);

    impl Noise {
        fn next(&mut self) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 40) as f32 / (1u64 << 23) as f32 - 1.0
        }
    }

    /// Run `input` (frames of both channels) through a limiter in blocks of `block` frames, and give the sound that comes out and the gains.
    fn run(limiter: &mut Limiter, input: &[[f32; CHAIN_CHANNELS]], ceiling: f32, block: usize) -> (Vec<[f32; CHAIN_CHANNELS]>, Vec<f32>) {
        let (mut out, mut gains) = (Vec::new(), Vec::new());
        for chunk in input.chunks(block) {
            for &frame in chunk {
                let (sound, gain) = limiter.step(frame, ceiling);
                out.push(sound);
                gains.push(gain);
            }
        }
        (out, gains)
    }

    fn sine(frames: usize, hz: f32, amplitude: f32) -> Vec<[f32; CHAIN_CHANNELS]> {
        (0..frames)
            .map(|n| {
                let sample = amplitude * (2.0 * std::f32::consts::PI * hz * n as f32 / RATE).sin();
                [sample, sample]
            })
            .collect()
    }

    #[test]
    fn a_sound_that_is_under_the_ceiling_comes_out_as_it_went_in_late_by_the_look_ahead() {
        let mut limiter = Limiter::new();
        let ceiling = linear(-1.0);
        let input = sine(5_000, 440.0, 0.5);
        let (out, gains) = run(&mut limiter, &input, ceiling, 256);
        let late = limiter.latency();
        assert_eq!(late, 95, "2 ms at 48 kHz is 96 frames of window and one less of look-ahead");
        assert!(out[..late].iter().all(|frame| *frame == [0.0, 0.0]), "what comes out first is the silence that was held back");
        for n in late..input.len() {
            assert_eq!(out[n], input[n - late], "frame {n}: the bits that went in");
        }
        assert!(gains.iter().all(|&gain| gain == 1.0), "the gain is exactly 1 where nothing is over the ceiling");
    }

    #[test]
    fn nothing_that_comes_out_is_over_the_ceiling_whatever_goes_in() {
        for (ceiling_db, block) in [(-1.0, 256), (-6.0, 64), (-12.0, 1024), (0.0, 7)] {
            let ceiling = linear(ceiling_db);
            let mut limiter = Limiter::new();
            let mut noise = Noise(0x9e37_79b9_7f4a_7c15);
            // Loud noise, a sine that is 12 dB over, a burst that is 40 dB over and a stretch of silence.
            let mut input: Vec<[f32; CHAIN_CHANNELS]> = (0..6_000).map(|_| [noise.next() * 4.0, noise.next() * 4.0]).collect();
            input.extend(sine(6_000, 220.0, 4.0));
            input.extend((0..300).map(|_| [noise.next() * 100.0, noise.next() * 100.0]));
            input.extend(std::iter::repeat_n([0.0, 0.0], 3_000));
            let (out, _) = run(&mut limiter, &input, ceiling, block);
            let loudest = out.iter().flatten().fold(0.0f32, |loudest, sample| loudest.max(sample.abs()));
            assert!(loudest <= ceiling, "a ceiling of {ceiling_db} dB: the loudest sample is {loudest}");
            assert!(loudest > ceiling * 0.9, "and it is brought to it and no lower ({loudest} against {ceiling})");
        }
    }

    #[test]
    fn a_peak_is_brought_down_by_a_ramp_that_ends_at_the_peak_and_is_as_long_as_the_look_ahead() {
        let mut limiter = Limiter::new();
        let ceiling = 0.5;
        let late = limiter.latency();
        // A single sample four times the ceiling in silence.
        let mut input = vec![[0.0, 0.0]; 1_000];
        input[400] = [2.0, 0.0];
        let (out, gains) = run(&mut limiter, &input, ceiling, 256);
        // The gain that the peak has is the one that brings it to the ceiling (a quarter), and it arrives at the peak: it is 1 the frame before the ramp starts.
        let at_peak = 400 + late;
        assert!((gains[at_peak] - 0.25).abs() < 1e-6, "{}", gains[at_peak]);
        assert!((out[at_peak][0] - ceiling).abs() < 1e-6, "the peak comes out at the ceiling: {}", out[at_peak][0]);
        assert_eq!(gains[at_peak - late - 1], 1.0, "nothing of it is heard before the ramp");
        // In between the gain goes down in equal steps: a ramp, and not a step.
        let step = (1.0 - 0.25) / (late as f32 + 1.0);
        for j in 0..=late {
            let want = 1.0 - step * (j as f32 + 1.0);
            let n = at_peak - late + j;
            assert!((gains[n] - want).abs() < 1e-5, "frame {n}: {} against {want}", gains[n]);
        }
        // And after the peak it comes up again by the release, and not at once.
        assert!(gains[at_peak + 1] < 0.3, "{}", gains[at_peak + 1]);
        assert!(gains[at_peak + 480] < 0.9 && gains[at_peak + 480] > 0.3, "ten milliseconds on: {}", gains[at_peak + 480]);
    }

    /// What the limiter makes of what each frame asks for (`wanted`), worked out as its description says it, in 64 bits and with no rings: the least wanted over the last `window` frames, held down by the
    /// release, and averaged over the last `window` of those (before the start, 1 all the way back).
    fn reference(wanted: &[f32], window: usize, release: f64) -> Vec<f64> {
        let mut held = Vec::with_capacity(wanted.len());
        let mut last = 1.0f64;
        for n in 0..wanted.len() {
            let least = wanted[(n + 1).saturating_sub(window)..=n].iter().fold(f32::INFINITY, |least, &w| least.min(w));
            last = f64::from(least).min(last + release * (1.0 - last));
            held.push(last);
        }
        (0..wanted.len()).map(|n| (0..window).map(|k| n.checked_sub(k).map_or(1.0, |at| held[at])).sum::<f64>() / window as f64).collect()
    }

    #[test]
    fn the_gain_is_what_its_description_says_frame_by_frame_in_the_ramp_the_hold_and_the_release() {
        let ceiling = 0.5f32;
        let mut input = vec![[0.0f32, 0.0]; 12_000];
        // A frame that is four times the ceiling, another that is twice it 40 frames on (the first is still in the window: the ramps run into one another), a burst of 300 frames that is three
        // times over on one side and a quiet tone on the other, and then silence for the release.
        input[400] = [2.0, 0.0];
        input[440] = [0.0, -1.0];
        for (n, frame) in input.iter_mut().enumerate().take(2_300).skip(2_000) {
            *frame = [0.3, 1.5 * ((n - 2_000) as f32 / 40.0).sin().abs() + 0.2];
        }
        let mut limiter = Limiter::new();
        let (window, release) = (limiter.window, limiter.release);
        let (_, gains) = run(&mut limiter, &input, ceiling, 256);
        let wanted: Vec<f32> = input
            .iter()
            .map(|frame| {
                let peak = frame[0].abs().max(frame[1].abs());
                if peak > ceiling { ceiling / peak } else { 1.0 }
            })
            .collect();
        let want = reference(&wanted, window, release);
        assert!(want.iter().any(|&gain| gain < 0.3) && want[11_999] > 0.9, "the stretch has a deep ramp and a release that has begun");
        for (n, (&got, &expected)) in gains.iter().zip(&want).enumerate() {
            assert!((f64::from(got) - expected).abs() < 3e-7, "frame {n}: {got} against {expected}");
        }
        assert_eq!(gains.len(), input.len());
    }

    #[test]
    fn a_side_that_is_louder_than_the_other_limits_both_by_the_same_gain_whichever_side_it_is() {
        let ceiling = 0.5f32;
        for loud in [0usize, 1] {
            let quiet = 1 - loud;
            let mut limiter = Limiter::new();
            // A quiet level on the one side all through and, in the middle of it, a stretch that is four times the ceiling on the other.
            let mut input = vec![[0.0f32; CHAIN_CHANNELS]; 4_000];
            for (n, frame) in input.iter_mut().enumerate() {
                frame[quiet] = 0.2;
                if (1_000..1_400).contains(&n) {
                    frame[loud] = 2.0;
                }
            }
            let (out, gains) = run(&mut limiter, &input, ceiling, 256);
            let late = limiter.latency();
            let loudest = out.iter().map(|frame| frame[loud].abs()).fold(0.0f32, f32::max);
            assert!(loudest <= ceiling && loudest > ceiling * 0.99, "side {loud}: the loud side is held at the ceiling ({loudest})");
            assert!(gains[1_200 + late] < 0.3, "side {loud}: {}", gains[1_200 + late]);
            // The other side is turned down by the very gain (it is 0.2 of the sound that is held back, whatever that was).
            for (n, (frame, &gain)) in out.iter().zip(&gains).enumerate().skip(late) {
                assert!((frame[quiet] - 0.2 * gain).abs() < 1e-6, "side {loud}, frame {n}: {} against {}", frame[quiet], 0.2 * gain);
            }
        }
    }

    #[test]
    fn a_sum_that_has_drifted_is_worked_out_again_at_the_end_of_the_window_and_the_gain_is_exactly_1_again() {
        let mut limiter = Limiter::new();
        let ceiling = linear(-1.0);
        let quiet = vec![[0.1f32, 0.1]; 1_000];
        run(&mut limiter, &quiet, ceiling, 256);
        // (What rounding does over hours, done at once: the sum is a thousandth too much.)
        limiter.sum += 0.001;
        let (_, gains) = run(&mut limiter, &quiet, ceiling, 256);
        assert!(gains[0] > 1.0, "the gain is wrong for what is left of the window");
        assert!(gains[limiter.window..].iter().all(|&gain| gain == 1.0), "and is exactly 1 from the end of it");
    }

    #[test]
    fn the_gain_comes_back_up_over_the_release_to_1() {
        let mut limiter = Limiter::new();
        let ceiling = linear(-1.0);
        let mut input = sine(2_000, 100.0, 4.0);
        input.extend(sine(96_000, 100.0, 0.25));
        let (_, gains) = run(&mut limiter, &input, ceiling, 256);
        let after = |ms: usize| gains[2_000 + ms * 48];
        assert!(after(0) < 0.3, "{}", after(0));
        // The time constant is 80 ms: 63 % of the way back after one, and all but a thousandth of it after seven.
        let down = after(0);
        let back = (after(80) - down) / (1.0 - down);
        assert!((0.55..0.70).contains(&back), "{back} of the way after 80 ms");
        assert!(after(560) > 0.999, "{}", after(560));
        assert_eq!(*gains.last().unwrap(), 1.0, "and it stays at 1 once it is there");
    }

    #[test]
    fn a_steady_sine_that_is_over_the_ceiling_is_turned_down_and_not_distorted() {
        let mut limiter = Limiter::new();
        let ceiling = linear(-1.0);
        let input = sine(48_000, 100.0, 1.0);
        let (out, gains) = run(&mut limiter, &input, ceiling, 256);
        let late = limiter.latency();
        // After the gain has settled the sound is the sine at a level: the gain moves by well under a percent within a cycle.
        let settled = 24_000..48_000;
        let (low, high) = settled.clone().fold((f32::MAX, f32::MIN), |(low, high), n| (low.min(gains[n]), high.max(gains[n])));
        assert!((high - low) / high < 0.01, "the gain between {low} and {high}");
        assert!((high - ceiling).abs() < 0.01, "{high} against {ceiling}");
        let worst = settled.map(|n| (out[n][0] - input[n - late][0] * ceiling).abs()).fold(0.0f32, f32::max);
        assert!(worst < 0.01, "the sound is within a hundredth of the sine turned down: {worst}");
    }

    #[test]
    fn the_two_channels_have_the_same_gain_so_that_the_stereo_image_stays_where_it_is() {
        let mut limiter = Limiter::new();
        let ceiling = linear(-1.0);
        let mut noise = Noise(7);
        // The left side is 12 dB over the ceiling and the right is 20 dB under it.
        let input: Vec<[f32; CHAIN_CHANNELS]> = (0..8_000).map(|_| [noise.next() * 4.0, noise.next() * 0.1]).collect();
        let (out, gains) = run(&mut limiter, &input, ceiling, 256);
        let late = limiter.latency();
        for n in late + 100..input.len() {
            let want = input[n - late][1] * gains[n];
            assert!((out[n][1] - want).abs() < 1e-7, "frame {n}: the right side has the gain that the left side made: {} against {want}", out[n][1]);
        }
        assert!(gains.iter().any(|&gain| gain < 0.5), "and the left side did bring it down");
    }

    #[test]
    fn a_sound_that_is_not_a_number_is_a_silence_and_does_not_stay_in_the_gain() {
        let mut limiter = Limiter::new();
        let ceiling = linear(-1.0);
        let mut input = sine(3_000, 440.0, 0.5);
        input[1_000] = [f32::NAN, 0.25];
        input[1_001] = [f32::INFINITY, f32::NEG_INFINITY];
        input[1_002] = [0.25, f32::NAN];
        let (out, gains) = run(&mut limiter, &input, ceiling, 128);
        assert!(out.iter().flatten().all(|sample| sample.is_finite() && sample.abs() <= ceiling), "nothing that is not a number comes out, and nothing is over the ceiling");
        assert!(gains.iter().all(|gain| gain.is_finite() && *gain == 1.0), "and the gain is not moved by it: they are silences, which are under the ceiling");
        let late = limiter.latency();
        assert_eq!(out[1_000 + late], [0.0, 0.25], "the side that is a number is let through, and the other is a silence");
    }

    #[test]
    fn the_result_is_the_same_bits_whatever_the_blocks_it_is_given_in() {
        let ceiling = linear(-3.0);
        let mut noise = Noise(42);
        let mut input: Vec<[f32; CHAIN_CHANNELS]> = (0..20_000).map(|_| [noise.next() * 2.0, noise.next() * 2.0]).collect();
        input.extend(sine(20_000, 55.0, 3.0));
        let mut first: Option<(Vec<[f32; CHAIN_CHANNELS]>, Vec<f32>)> = None;
        for block in [1, 7, 64, 256, 1000, 40_000] {
            let mut limiter = Limiter::new();
            let got = run(&mut limiter, &input, ceiling, block);
            match &first {
                None => first = Some(got),
                Some(first) => assert!(*first == got, "blocks of {block} frames give other bits"),
            }
        }
    }

    #[test]
    fn the_window_is_two_milliseconds_at_any_rate_and_the_release_is_what_the_rate_makes_of_it() {
        let mut limiter = Limiter::new();
        for (rate, window) in [(44_100.0, 88), (48_000.0, 96), (96_000.0, 192), (192_000.0, 384), (384_000.0, MAX_WINDOW), (8_000.0, 16), (100.0, MIN_WINDOW)] {
            limiter.configure(rate);
            assert_eq!((limiter.window, limiter.latency()), (window, window - 1), "at {rate} Hz");
            let seconds_to_63 = -1.0 / (1.0 - limiter.release).ln() / f64::from(rate);
            assert!((seconds_to_63 - f64::from(RELEASE_SEC)).abs() / f64::from(RELEASE_SEC) < 1e-3, "at {rate} Hz the release is {seconds_to_63} s");
        }
        // A rate that is not one leaves it as it was, and the same rate again does not start it over.
        limiter.configure(48_000.0);
        let ceiling = linear(-1.0);
        let (_, gain) = limiter.step([4.0, 4.0], ceiling);
        assert!(gain < 1.0);
        let before = (limiter.window, limiter.release, limiter.sum);
        for rate in [f32::NAN, 0.0, -1.0, f32::INFINITY] {
            limiter.configure(rate);
            assert_eq!((limiter.window, limiter.release, limiter.sum), before, "a rate of {rate}");
        }
        limiter.configure(48_000.0);
        assert_eq!(limiter.sum, before.2, "the same rate again leaves what it holds");
    }

    #[test]
    fn a_ceiling_that_is_moved_while_it_runs_is_the_ceiling_from_then_on() {
        let mut limiter = Limiter::new();
        let input = sine(20_000, 100.0, 1.0);
        let (first, _) = run(&mut limiter, &input, linear(-1.0), 256);
        let (second, _) = run(&mut limiter, &input, linear(-12.0), 256);
        let loudest = |frames: &[[f32; CHAIN_CHANNELS]]| frames.iter().flatten().fold(0.0f32, |loudest, sample| loudest.max(sample.abs()));
        assert!(loudest(&first) <= linear(-1.0));
        assert!(loudest(&second) <= linear(-12.0), "{}", loudest(&second));
        assert!(loudest(&second[10_000..]) > linear(-12.0) * 0.95, "and it is not lower than it has to be");
    }

    #[test]
    fn the_sum_of_the_gains_is_what_the_ring_has_in_it_after_a_long_run() {
        let mut limiter = Limiter::new();
        let mut noise = Noise(99);
        let ceiling = linear(-1.0);
        for _ in 0..2_000_000 {
            limiter.step([noise.next() * 3.0, noise.next() * 3.0], ceiling);
        }
        let exact: f64 = limiter.smooth[..limiter.window].iter().sum();
        assert!((limiter.sum - exact).abs() < 1e-9, "{} against {exact}", limiter.sum);
    }
}
