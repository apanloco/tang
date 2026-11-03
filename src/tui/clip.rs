//! The CLIP light in the status bar: on for a while after the master meter last
//! counted a clipped buffer.
//!
//! The engine only counts (`telemetry::MASTER.clips`, up by one for every buffer
//! with a sample beyond full scale); it is up to each reader to decide what that
//! looks like. This one lights red when the count has moved since it last looked,
//! and stays lit for `HOLD` after the last move, because a clip is over in a few
//! milliseconds and a frame is drawn every hundred.

use std::time::{Duration, Instant};

/// How long the light stays on after the last clipped buffer.
pub const HOLD: Duration = Duration::from_secs(2);

pub struct ClipLight {
    /// The count at the last look.
    seen: u32,
    /// When the count last moved.
    last_clip: Option<Instant>,
    lit: bool,
}

impl ClipLight {
    /// A light that starts from `count`, what the meter has counted so far:
    /// clips from before it was watching do not light it.
    pub fn new(count: u32) -> Self {
        ClipLight { seen: count, last_clip: None, lit: false }
    }

    /// Look at the meter's clip count at `now`.
    pub fn update(&mut self, count: u32, now: Instant) {
        // Not `>`: the count wraps.
        if count != self.seen {
            self.seen = count;
            self.last_clip = Some(now);
        }
        self.lit = self.last_clip.is_some_and(|at| now.saturating_duration_since(at) < HOLD);
    }

    /// Whether the light is on, as of the last `update`.
    pub fn is_lit(&self) -> bool {
        self.lit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn after(start: Instant, millis: u64) -> Instant {
        start + Duration::from_millis(millis)
    }

    #[test]
    fn it_is_dark_until_the_count_moves() {
        let t0 = Instant::now();
        let mut light = ClipLight::new(0);
        assert!(!light.is_lit(), "before any look");
        light.update(0, t0);
        light.update(0, after(t0, 5_000));
        assert!(!light.is_lit());
    }

    #[test]
    fn clips_from_before_it_started_watching_do_not_light_it() {
        let t0 = Instant::now();
        let mut light = ClipLight::new(41);
        light.update(41, t0);
        assert!(!light.is_lit());
        light.update(42, after(t0, 100));
        assert!(light.is_lit());
    }

    #[test]
    fn it_stays_lit_for_two_seconds_after_the_count_moved() {
        let t0 = Instant::now();
        let mut light = ClipLight::new(0);
        light.update(1, t0);
        assert!(light.is_lit());
        light.update(1, after(t0, 1_999));
        assert!(light.is_lit(), "just under the hold");
        light.update(1, after(t0, 2_000));
        assert!(!light.is_lit(), "the hold is over");
        light.update(1, after(t0, 9_000));
        assert!(!light.is_lit(), "and it stays dark");
    }

    #[test]
    fn another_clip_starts_the_hold_over() {
        let t0 = Instant::now();
        let mut light = ClipLight::new(0);
        light.update(1, t0);
        light.update(2, after(t0, 1_500));
        light.update(2, after(t0, 3_000));
        assert!(light.is_lit(), "1.5 s after the second clip");
        light.update(2, after(t0, 3_500));
        assert!(!light.is_lit(), "2 s after the second clip");
    }

    #[test]
    fn a_count_that_wraps_around_is_still_a_clip() {
        let t0 = Instant::now();
        let mut light = ClipLight::new(u32::MAX);
        light.update(0, t0);
        assert!(light.is_lit());
    }

    #[test]
    fn a_look_before_the_last_clip_does_not_panic_or_go_dark() {
        let t0 = Instant::now();
        let mut light = ClipLight::new(0);
        light.update(1, after(t0, 1_000));
        light.update(1, t0);
        assert!(light.is_lit(), "time that goes backwards counts as no time");
    }
}
