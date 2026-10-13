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

use std::sync::atomic::{AtomicU32, Ordering};

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
    #[cfg_attr(not(test), expect(dead_code, reason = "the layout a client gets (`app::layout`) names the slots; the web runtime sends it in step 10"))]
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

/// The cells: written by the audio thread, read by anyone.
pub struct Telemetry {
    cells: Box<[AtomicU32]>,
}

impl Telemetry {
    pub fn new() -> Self {
        Telemetry { cells: (0..CAPACITY).map(|_| AtomicU32::new(0)).collect() }
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
    #[cfg_attr(not(test), expect(dead_code, reason = "the web runtime (step 10) reads the values for its telemetry frames"))]
    pub fn load_index(&self, index: u32) -> f32 {
        self.cells.get(index as usize).map_or(0.0, |cell| f32::from_bits(cell.load(Ordering::Relaxed)))
    }

    /// How many events the slot numbered `index` has counted.
    #[cfg_attr(not(test), expect(dead_code, reason = "the web runtime (step 10) reads the values for its telemetry frames"))]
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
