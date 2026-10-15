//! What the audio thread publishes about itself, for any thread to read; see
//! `docs/web-ui.md` §6.
//!
//! A `Telemetry` is a fixed set of 32-bit cells, preallocated, that the audio
//! thread stores into with relaxed atomics and that readers (the TUI, later the
//! web runtime) load from: no allocation, no lock, and a reader that falls behind
//! just sees the latest value. What a cell means is decided by whoever gave the
//! engine its `Slot`; the main thread hands slots out with a `SlotAllocator`, the
//! engine never invents one.
//!
//! A value is a float (a modulator's output, a peak) or a count (how many buffers
//! clipped), stored as its bits; a counter is the way to publish an event that
//! more than one reader must see, because nobody has to reset it.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// How many slots a session gets. A modulator takes one, each of its targets one,
/// a meter two: a session with hundreds of nodes is far below this, and 16 KiB is
/// nothing.
pub const CAPACITY: u32 = 4096;

/// One cell of the `Telemetry`, handed out by a `SlotAllocator` (or reserved).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Slot(u32);

impl Slot {
    /// The slot numbered `index`, for tests that publish into the slots a layout names
    /// (the web runtime's, which is what reads layouts).
    #[cfg(all(test, feature = "web"))]
    pub fn for_test(index: u32) -> Slot {
        Slot(index)
    }

    /// The cell's number, for describing it to a client.
    #[cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "the layout a client gets (`app::layout`) names the slots; only the web runtime sends it"))]
    pub fn index(self) -> u32 {
        self.0
    }
}

/// The two slots of a meter: its peak, and how many buffers have clipped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeterSlots {
    /// The loudest sample lately, as a float: the highest absolute sample of the
    /// last buffers, falling off over time (see `Meter`).
    pub peak: Slot,
    /// A count: how many buffers had a sample beyond full scale (above 1.0 in
    /// absolute value). Only ever goes up, so any number of readers can watch it
    /// change.
    pub clips: Slot,
}

/// The meter of the output mix, the first two slots.
pub const MASTER: MeterSlots = MeterSlots { peak: Slot(0), clips: Slot(1) };

/// Slots before this are reserved for what exists once per engine (`MASTER`).
const RESERVED: u32 = 2;

/// The cells, and the last frames of the output: written by the audio thread, read by anyone.
pub struct Telemetry {
    cells: Box<[AtomicU32]>,
    /// The output mix as it was a moment ago, for a display of the waveform.
    pub scope: ScopeRing,
}

impl Telemetry {
    pub fn new() -> Self {
        Telemetry { cells: (0..CAPACITY).map(|_| AtomicU32::new(0)).collect(), scope: ScopeRing::new() }
    }

    /// Publish a float.
    pub fn store(&self, slot: Slot, value: f32) {
        if let Some(cell) = self.cells.get(slot.0 as usize) {
            cell.store(value.to_bits(), Ordering::Relaxed);
        }
    }

    /// The float last published in `slot` (0.0 before anything was).
    #[cfg_attr(not(test), expect(dead_code, reason = "the web runtime (step 10) reads the values for its telemetry frames"))]
    pub fn load(&self, slot: Slot) -> f32 {
        self.cells.get(slot.0 as usize).map_or(0.0, |cell| f32::from_bits(cell.load(Ordering::Relaxed)))
    }

    /// The float last published in the slot numbered `index`, as a layout names it;
    /// 0.0 for a number that is not a slot.
    #[cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "only the web runtime reads the values for its telemetry frames"))]
    pub fn load_index(&self, index: u32) -> f32 {
        self.cells.get(index as usize).map_or(0.0, |cell| f32::from_bits(cell.load(Ordering::Relaxed)))
    }

    /// How many events the slot numbered `index` has counted.
    #[cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "only the web runtime reads the values for its telemetry frames"))]
    pub fn count_index(&self, index: u32) -> u32 {
        self.cells.get(index as usize).map_or(0, |cell| cell.load(Ordering::Relaxed))
    }

    /// Count one more event in `slot`.
    pub fn bump(&self, slot: Slot) {
        if let Some(cell) = self.cells.get(slot.0 as usize) {
            cell.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// How many events `slot` has counted (wrapping, in the unlikely long run).
    pub fn count(&self, slot: Slot) -> u32 {
        self.cells.get(slot.0 as usize).map_or(0, |cell| cell.load(Ordering::Relaxed))
    }
}

/// How many frames of the output the scope keeps (a frame is one sample of each side): a power of two, 170 ms at 48 kHz.
pub const SCOPE_FRAMES: usize = 8192;

/// The most frames a reader can take at once: a quarter of the ring, so that what it reads is not written over while it reads.
#[cfg_attr(not(test), expect(dead_code, reason = "the web runtime's scope frames and the terminal's Scope tab read the ring (the next commits)"))]
pub const SCOPE_READ_MAX: usize = SCOPE_FRAMES / 4;

/// How many frames the writer stores before it says that it has: a quarter of the ring too.
const SCOPE_CHUNK: usize = SCOPE_FRAMES / 4;

/// How many frames may be stored while a reader reads, and what it has read still be what was there: the ring less what it reads and less what the
/// writer may have half stored.
const SCOPE_SAFE_ADVANCE: u64 = (SCOPE_FRAMES - SCOPE_READ_MAX - SCOPE_CHUNK) as u64;

/// How many times a reader that was overtaken by the writer reads again before it takes what it has.
const SCOPE_TRIES: usize = 3;

/// The last frames of the output mix, for a display of the waveform (the page's scope, the terminal's): a ring that the audio thread stores into
/// and any thread copies from, with no lock and no allocation, and a reader that falls behind just sees the latest frames. Each sample is a cell
/// of its own (a float cannot tear), and a count of the frames stored in all, which the writer updates after the frames and a reader reads
/// before and after its copy, says whether the copy is one run of frames that were stored together: a copy that the writer overtook is made again.
pub struct ScopeRing {
    /// The samples, a frame's left side and its right side next to each other, as bits.
    cells: Box<[AtomicU32]>,
    /// How many frames have been stored in all (a `u64` does not wrap in practice).
    written: AtomicU64,
}

impl ScopeRing {
    pub fn new() -> Self {
        ScopeRing { cells: (0..2 * SCOPE_FRAMES).map(|_| AtomicU32::new(0)).collect(), written: AtomicU64::new(0) }
    }

    /// Store a buffer of the output, one `Vec` for each channel: the first is the left side and the second the right (a mix of one channel is
    /// both sides; the others are not kept). Called by the audio thread only.
    pub fn push(&self, channels: &[Vec<f32>]) {
        let Some(left) = channels.first() else { return };
        let right = channels.get(1).unwrap_or(left);
        let frames = left.len().min(right.len());
        let mut written = self.written.load(Ordering::Relaxed);
        for start in (0..frames).step_by(SCOPE_CHUNK) {
            let end = (start + SCOPE_CHUNK).min(frames);
            for (i, (&l, &r)) in left[start..end].iter().zip(&right[start..end]).enumerate() {
                let cell = ((written as usize + i) & (SCOPE_FRAMES - 1)) * 2;
                self.cells[cell].store(l.to_bits(), Ordering::Relaxed);
                self.cells[cell + 1].store(r.to_bits(), Ordering::Relaxed);
            }
            written += (end - start) as u64;
            self.written.store(written, Ordering::Release);
        }
    }

    /// Copy the latest frames into `left` and `right` (the most that `SCOPE_READ_MAX` allows, as many as the shorter of them holds; what is
    /// past that is left as it was), the latest last; before the first frames came, silence. Returns how many frames have been stored in all,
    /// so that a reader can tell whether it has seen these before.
    #[cfg_attr(not(test), expect(dead_code, reason = "the web runtime's scope frames and the terminal's Scope tab read the ring (the next commits)"))]
    pub fn latest(&self, left: &mut [f32], right: &mut [f32]) -> u64 {
        self.latest_checked(left, right, || {})
    }

    /// `latest`, with `between` called after each copy and before the look at whether the writer overtook it (a test makes the writer do its part there).
    fn latest_checked(&self, left: &mut [f32], right: &mut [f32], mut between: impl FnMut()) -> u64 {
        let wanted = left.len().min(right.len()).min(SCOPE_READ_MAX);
        let (left, right) = (&mut left[..wanted], &mut right[..wanted]);
        let mut written = self.written.load(Ordering::Acquire);
        for attempt in 1..=SCOPE_TRIES {
            for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
                // (Before the first frames there is nothing, and the sides get silence.)
                let frame = written as i64 - wanted as i64 + i as i64;
                (*l, *r) = if frame < 0 {
                    (0.0, 0.0)
                } else {
                    let cell = (frame as usize & (SCOPE_FRAMES - 1)) * 2;
                    (f32::from_bits(self.cells[cell].load(Ordering::Relaxed)), f32::from_bits(self.cells[cell + 1].load(Ordering::Relaxed)))
                };
            }
            between();
            let now = self.written.load(Ordering::Acquire);
            if now - written <= SCOPE_SAFE_ADVANCE || attempt == SCOPE_TRIES {
                break;
            }
            written = now;
        }
        written
    }
}

/// How fast a meter's peak falls when the signal is quieter: 20 dB in half a
/// second. A reader looking 30 times a second then sees a peak at most about
/// 1.3 dB under the loudest sample since its last look (a meter that only
/// published each buffer's own peak would miss most of them, and short ones are
/// the clips that matter).
const PEAK_FALL_DB_PER_SECOND: f32 = 40.0;

/// A peak below this is silence: it is published as 0 rather than left to
/// dwindle through ever smaller numbers.
const SILENCE: f32 = 1e-6;

/// The factor a peak is multiplied by over a buffer that lasts `seconds`.
pub fn peak_fall(seconds: f32) -> f32 {
    10f32.powf(-PEAK_FALL_DB_PER_SECOND * seconds / 20.0)
}

/// What the audio thread keeps for one meter between buffers, and where it
/// publishes it.
pub struct Meter {
    slots: MeterSlots,
    /// The peak so far, falling off buffer by buffer.
    peak: f32,
}

impl Meter {
    pub fn new(slots: MeterSlots) -> Self {
        Meter { slots, peak: 0.0 }
    }

    /// Account for a buffer that has just been produced: publish the peak and
    /// count a clip if any sample went beyond full scale. `fall` is
    /// `peak_fall` of the buffer's length.
    pub fn update(&mut self, telemetry: &Telemetry, channels: &[Vec<f32>], fall: f32) {
        // `max` takes the other operand when one is NaN, so a NaN sample does not
        // poison the peak.
        let loudest = channels.iter().flatten().fold(0.0f32, |loudest, sample| loudest.max(sample.abs()));
        self.peak = loudest.max(self.peak * fall);
        if self.peak < SILENCE {
            self.peak = 0.0;
        }
        telemetry.store(self.slots.peak, self.peak);
        if loudest > 1.0 {
            telemetry.bump(self.slots.clips);
        }
    }
}

/// How the main thread hands out slots. Slots are never taken back or reused: a
/// session would have to add and remove thousands of modulators to run out, and
/// when it does the item just has no telemetry (the audio is unaffected).
#[derive(Debug)]
pub struct SlotAllocator {
    next: u32,
}

impl Default for SlotAllocator {
    fn default() -> Self {
        SlotAllocator { next: RESERVED }
    }
}

impl SlotAllocator {
    /// A slot nobody has, or `None` when they are all taken.
    pub fn alloc(&mut self) -> Option<Slot> {
        if self.next >= CAPACITY {
            log::warn!("out of telemetry slots; what is added from now on has none");
            return None;
        }
        let slot = Slot(self.next);
        self.next += 1;
        Some(slot)
    }

    /// The two slots of a meter, both or neither.
    pub fn meter(&mut self) -> Option<MeterSlots> {
        if self.next + 1 >= CAPACITY {
            // Not enough left for a pair: leave the last one rather than half a meter.
            log::warn!("out of telemetry slots; what is added from now on has none");
            return None;
        }
        Some(MeterSlots { peak: self.alloc()?, clips: self.alloc()? })
    }

    /// Every slot handed out so far, the reserved ones included, are below this.
    #[cfg(test)]
    pub fn high_water(&self) -> u32 {
        self.next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_float_comes_back_as_published() {
        let t = Telemetry::new();
        let mut slots = SlotAllocator::default();
        let (a, b) = (slots.alloc().unwrap(), slots.alloc().unwrap());
        assert_eq!(t.load(a), 0.0, "nothing published yet");
        t.store(a, 0.25);
        t.store(b, -1.5);
        assert_eq!((t.load(a), t.load(b)), (0.25, -1.5));
        t.store(a, f32::INFINITY);
        assert_eq!(t.load(a), f32::INFINITY, "the bits are kept as they are");
        assert_eq!(t.load(b), -1.5, "a slot only changes when it is stored to");
    }

    #[test]
    fn a_slot_can_be_read_by_its_number() {
        let t = Telemetry::new();
        let mut slots = SlotAllocator::default();
        let (a, b) = (slots.alloc().unwrap(), slots.alloc().unwrap());
        t.store(a, 0.75);
        t.bump(b);
        t.bump(b);
        assert_eq!((t.load_index(a.index()), t.count_index(b.index())), (0.75, 2));
        assert_eq!((t.load_index(b.index()), t.count_index(a.index())), (f32::from_bits(2), 0.75f32.to_bits()), "the same cells, whichever way they are read");
        assert_eq!((t.load_index(CAPACITY), t.count_index(CAPACITY), t.load_index(u32::MAX)), (0.0, 0, 0.0), "no such slot");
    }

    #[test]
    fn a_counter_goes_up_by_one_and_leaves_the_others_alone() {
        let t = Telemetry::new();
        let mut slots = SlotAllocator::default();
        let (a, b) = (slots.alloc().unwrap(), slots.alloc().unwrap());
        assert_eq!(t.count(a), 0);
        t.bump(a);
        t.bump(a);
        t.bump(a);
        assert_eq!((t.count(a), t.count(b)), (3, 0));
    }

    #[test]
    fn a_slot_out_of_range_reads_zero_and_ignores_writes() {
        let t = Telemetry::new();
        let stray = Slot(CAPACITY);
        t.store(stray, 1.0);
        t.bump(stray);
        assert_eq!((t.load(stray), t.count(stray)), (0.0, 0));
        let last = Slot(CAPACITY - 1);
        t.store(last, 2.0);
        assert_eq!(t.load(last), 2.0, "the last cell is in range");
    }

    #[test]
    fn the_master_meter_is_reserved_and_never_handed_out() {
        assert_ne!(MASTER.peak, MASTER.clips);
        let mut slots = SlotAllocator::default();
        let first = slots.alloc().unwrap();
        assert!(first.index() > MASTER.peak.index() && first.index() > MASTER.clips.index());
    }

    #[test]
    fn slots_are_handed_out_once_each_until_they_run_out() {
        let mut slots = SlotAllocator::default();
        let mut seen = std::collections::HashSet::new();
        while let Some(slot) = slots.alloc() {
            assert!(seen.insert(slot), "{slot:?} was handed out twice");
            assert!(slot.index() < CAPACITY);
        }
        assert_eq!(seen.len() as u32, CAPACITY - RESERVED);
        assert!(slots.alloc().is_none(), "and stays out");
        assert!(slots.meter().is_none());
    }

    #[test]
    fn a_meter_gets_two_slots_of_its_own_or_none() {
        let mut slots = SlotAllocator::default();
        let a = slots.meter().unwrap();
        let b = slots.meter().unwrap();
        let all = [a.peak, a.clips, b.peak, b.clips];
        let unique: std::collections::HashSet<_> = all.iter().collect();
        assert_eq!(unique.len(), 4);

        // With one slot left there is no meter, and that slot is still there.
        let mut slots = SlotAllocator::default();
        while slots.high_water() < CAPACITY - 1 {
            slots.alloc().unwrap();
        }
        assert!(slots.meter().is_none(), "one slot is not a meter");
        assert!(slots.alloc().is_some(), "and it was not used up trying");
        assert!(slots.alloc().is_none());
    }

    // -- Meter --

    fn meter() -> (Telemetry, Meter) {
        let slots = SlotAllocator::default().meter().unwrap();
        (Telemetry::new(), Meter::new(slots))
    }

    fn peak_and_clips(t: &Telemetry, m: &Meter) -> (f32, u32) {
        (t.load(m.slots.peak), t.count(m.slots.clips))
    }

    #[test]
    fn a_meter_publishes_the_loudest_sample_of_any_channel_ignoring_sign() {
        let (t, mut m) = meter();
        m.update(&t, &[vec![0.1, -0.7, 0.2], vec![0.3, 0.4, -0.5]], 1.0);
        assert_eq!(peak_and_clips(&t, &m), (0.7, 0));
        m.update(&t, &[vec![0.1, 0.2], vec![0.1, -0.9]], 1.0);
        assert_eq!(peak_and_clips(&t, &m), (0.9, 0), "the other channel is loudest now");
    }

    #[test]
    fn a_peak_falls_by_the_given_factor_over_quieter_buffers_and_never_below_them() {
        let (t, mut m) = meter();
        m.update(&t, &[vec![0.8]], 0.5);
        m.update(&t, &[vec![0.0]], 0.5);
        assert_eq!(t.load(m.slots.peak), 0.4);
        m.update(&t, &[vec![0.0]], 0.5);
        assert_eq!(t.load(m.slots.peak), 0.2);
        m.update(&t, &[vec![0.3]], 0.5);
        assert_eq!(t.load(m.slots.peak), 0.3, "a louder buffer takes over at once");
        m.update(&t, &[vec![0.1]], 0.5);
        assert_eq!(t.load(m.slots.peak), 0.15, "a quieter one only slows the fall");
    }

    #[test]
    fn silence_is_published_as_exactly_zero() {
        let (t, mut m) = meter();
        m.update(&t, &[vec![1e-5]], 0.5);
        assert!(t.load(m.slots.peak) > 0.0);
        for _ in 0..8 {
            m.update(&t, &[vec![0.0]], 0.5);
        }
        assert_eq!(t.load(m.slots.peak), 0.0, "1e-5 halved eight times is under the floor");
    }

    #[test]
    fn a_buffer_with_a_sample_beyond_full_scale_is_one_clip() {
        let (t, mut m) = meter();
        m.update(&t, &[vec![1.0, -1.0, 0.5]], 1.0);
        assert_eq!(peak_and_clips(&t, &m), (1.0, 0), "full scale is not a clip");
        m.update(&t, &[vec![1.5, 2.0, -3.0], vec![0.0]], 1.0);
        assert_eq!(peak_and_clips(&t, &m), (3.0, 1), "however many samples clip, it is one buffer");
        m.update(&t, &[vec![-1.2]], 1.0);
        assert_eq!(peak_and_clips(&t, &m).1, 2, "a negative one clips too");
        m.update(&t, &[vec![0.5]], 1.0);
        assert_eq!(t.count(m.slots.clips), 2, "a quiet buffer after a clip is not one, although the peak still holds");
        assert!(t.load(m.slots.peak) > 1.0);
    }

    #[test]
    fn a_nan_sample_does_not_poison_the_peak() {
        let (t, mut m) = meter();
        m.update(&t, &[vec![f32::NAN, 0.5]], 1.0);
        assert_eq!(peak_and_clips(&t, &m), (0.5, 0));
        m.update(&t, &[vec![f32::INFINITY]], 1.0);
        assert_eq!(peak_and_clips(&t, &m), (f32::INFINITY, 1), "infinity is beyond full scale");
    }

    #[test]
    fn two_meters_do_not_share_slots() {
        let mut slots = SlotAllocator::default();
        let t = Telemetry::new();
        let mut a = Meter::new(slots.meter().unwrap());
        let mut b = Meter::new(slots.meter().unwrap());
        a.update(&t, &[vec![2.0]], 1.0);
        b.update(&t, &[vec![0.25]], 1.0);
        assert_eq!((t.load(a.slots.peak), t.count(a.slots.clips)), (2.0, 1));
        assert_eq!((t.load(b.slots.peak), t.count(b.slots.clips)), (0.25, 0));
    }

    #[test]
    fn the_fall_is_twenty_db_in_half_a_second() {
        let over_half_a_second = peak_fall(0.5);
        assert!((over_half_a_second - 0.1).abs() < 1e-6, "{over_half_a_second}");
        assert_eq!(peak_fall(0.0), 1.0, "no time, no fall");
        // Many short buffers add up to the same fall as one long one.
        let steps: f32 = (0..100).map(|_| peak_fall(0.005)).product();
        assert!((steps - over_half_a_second).abs() < 1e-4, "{steps}");
    }

    // -- Scope ring --

    /// `n` frames of a ramp from `from`: the left side counts up and the right side is its negative, so that a mix-up of the sides shows.
    fn ramp(from: usize, n: usize) -> Vec<Vec<f32>> {
        vec![(from..from + n).map(|i| i as f32).collect(), (from..from + n).map(|i| -(i as f32)).collect()]
    }

    /// The latest `n` frames, from sides that were 9 before: what each side has, and how many frames were written in all.
    fn latest(ring: &ScopeRing, n: usize) -> (Vec<f32>, Vec<f32>, u64) {
        let (mut left, mut right) = (vec![9.0; n], vec![9.0; n]);
        let written = ring.latest(&mut left, &mut right);
        (left, right, written)
    }

    fn counting(from: usize, to: usize) -> Vec<f32> {
        (from..to).map(|i| i as f32).collect()
    }

    #[test]
    fn a_scope_that_nothing_was_pushed_to_reads_silence_and_no_frames() {
        let ring = ScopeRing::new();
        assert_eq!(latest(&ring, 16), (vec![0.0; 16], vec![0.0; 16], 0));
    }

    #[test]
    fn the_latest_frames_are_what_was_pushed_in_the_order_it_was_pushed_each_side_its_own() {
        let ring = ScopeRing::new();
        ring.push(&ramp(0, 100));
        ring.push(&ramp(100, 50));
        let (left, right, written) = latest(&ring, 120);
        assert_eq!(left, counting(30, 150));
        assert_eq!(right, counting(30, 150).iter().map(|s| -s).collect::<Vec<_>>());
        assert_eq!(written, 150);
    }

    #[test]
    fn asking_for_more_frames_than_there_are_puts_silence_before_them() {
        let ring = ScopeRing::new();
        ring.push(&ramp(1, 3));
        let (left, right, written) = latest(&ring, 5);
        assert_eq!(left, [0.0, 0.0, 1.0, 2.0, 3.0]);
        assert_eq!(right, [0.0, 0.0, -1.0, -2.0, -3.0]);
        assert_eq!(written, 3);
    }

    #[test]
    fn after_the_ring_has_gone_round_the_latest_frames_are_the_last_ones_pushed() {
        let ring = ScopeRing::new();
        let total = 3 * SCOPE_FRAMES + 1234;
        for from in (0..total).step_by(500) {
            ring.push(&ramp(from, 500.min(total - from)));
        }
        let (left, right, written) = latest(&ring, SCOPE_READ_MAX);
        assert_eq!(written, total as u64);
        assert_eq!(left, counting(total - SCOPE_READ_MAX, total));
        assert!(left.iter().zip(&right).all(|(l, r)| *r == -*l));
    }

    #[test]
    fn a_block_longer_than_the_ring_leaves_its_last_frames() {
        let ring = ScopeRing::new();
        let total = 3 * SCOPE_FRAMES + 7;
        ring.push(&ramp(0, total));
        let (left, _, written) = latest(&ring, 100);
        assert_eq!((left, written), (counting(total - 100, total), total as u64));
    }

    #[test]
    fn a_mix_of_one_channel_is_both_sides_and_a_mix_of_none_pushes_nothing() {
        let ring = ScopeRing::new();
        ring.push(&[vec![0.25, -0.5]]);
        assert_eq!(latest(&ring, 2), (vec![0.25, -0.5], vec![0.25, -0.5], 2));
        ring.push(&[]);
        assert_eq!(latest(&ring, 2).2, 2);
    }

    #[test]
    fn a_read_asks_for_no_more_than_the_ring_can_give_at_once() {
        let ring = ScopeRing::new();
        ring.push(&ramp(0, SCOPE_FRAMES));
        let (left, _, written) = latest(&ring, SCOPE_READ_MAX + 10);
        assert_eq!(written, SCOPE_FRAMES as u64);
        assert_eq!(left[..SCOPE_READ_MAX], counting(SCOPE_FRAMES - SCOPE_READ_MAX, SCOPE_FRAMES)[..]);
        assert_eq!(left[SCOPE_READ_MAX..], [9.0; 10], "what is past the most it gives is left as it was");
    }

    #[test]
    fn a_copy_that_the_writer_overtook_is_made_again_from_the_frames_that_are_there_now() {
        let ring = ScopeRing::new();
        ring.push(&ramp(0, 3000));
        let (mut left, mut right) = (vec![0.0; 1000], vec![0.0; 1000]);
        let mut pushed = false;
        let written = ring.latest_checked(&mut left, &mut right, || {
            // While it copies, more goes in than the ring has room for beside what it reads.
            if !std::mem::replace(&mut pushed, true) {
                ring.push(&ramp(3000, 5000));
            }
        });
        assert_eq!(written, 8000);
        assert_eq!(left, counting(7000, 8000));
    }

    #[test]
    fn a_copy_that_the_writer_did_not_overtake_stands_as_of_the_frames_it_copied() {
        let ring = ScopeRing::new();
        ring.push(&ramp(0, 3000));
        let (mut left, mut right) = (vec![0.0; 1000], vec![0.0; 1000]);
        let mut looks = 0;
        let written = ring.latest_checked(&mut left, &mut right, || {
            // A buffer goes in while it copies: nothing it copied is touched.
            looks += 1;
            if looks == 1 {
                ring.push(&ramp(3000, 1000));
            }
        });
        assert_eq!((written, looks), (3000, 1));
        assert_eq!(left, counting(2000, 3000));
    }

    #[test]
    fn a_reader_that_is_overtaken_every_time_gives_up_after_three_tries() {
        let ring = ScopeRing::new();
        ring.push(&ramp(0, 3000));
        let (mut left, mut right) = (vec![0.0; 1000], vec![0.0; 1000]);
        let mut looks = 0;
        let mut at = 3000;
        let written = ring.latest_checked(&mut left, &mut right, || {
            looks += 1;
            ring.push(&ramp(at, 5000));
            at += 5000;
        });
        assert_eq!(looks, 3);
        assert_eq!(written, 13_000, "what the third copy began at");
    }

    #[test]
    fn a_reader_that_looks_while_the_writer_pushes_sees_a_run_of_frames_that_were_pushed() {
        use std::sync::Arc;
        use std::time::Duration;
        let ring = Arc::new(ScopeRing::new());
        let writer = {
            let ring = ring.clone();
            std::thread::spawn(move || {
                // About fifty times faster than the sound card, which is more than a real one does to a reader.
                for block in 0..2000 {
                    ring.push(&ramp(block * 256, 256));
                    std::thread::sleep(Duration::from_micros(20));
                }
            })
        };
        let mut looks = 0;
        while !writer.is_finished() {
            let (left, right, written) = latest(&ring, SCOPE_READ_MAX);
            if written >= SCOPE_READ_MAX as u64 {
                let end = written as usize;
                assert_eq!(left, counting(end - SCOPE_READ_MAX, end), "frames up to {written}");
                assert!(left.iter().zip(&right).all(|(l, r)| *r == -*l));
                looks += 1;
            }
        }
        writer.join().unwrap();
        assert!(looks > 10, "the reader looked {looks} times while the writer ran");
    }

    #[test]
    fn a_reader_on_another_thread_sees_what_the_writer_stores() {
        use std::sync::Arc;
        let t = Arc::new(Telemetry::new());
        let slot = SlotAllocator::default().alloc().unwrap();
        let reader = {
            let t = t.clone();
            std::thread::spawn(move || {
                // Spin until the writer's last value shows up; only values it
                // stored may ever be seen.
                let started = std::time::Instant::now();
                loop {
                    let v = t.load(slot);
                    assert!(v == 0.0 || (1.0..=1000.0).contains(&v), "{v}");
                    if v == 1000.0 {
                        return;
                    }
                    assert!(started.elapsed() < std::time::Duration::from_secs(5), "the last value never showed up");
                }
            })
        };
        for i in 1..=1000 {
            t.store(slot, i as f32);
        }
        reader.join().unwrap();
    }
}
