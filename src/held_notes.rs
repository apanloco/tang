//! Lock-free shared state of "which notes are currently held".
//!
//! Both the hardware MIDI thread (in `midi.rs`) and the virtual piano
//! (in `piano.rs`) update this bitset on note-on / note-off. The TUI's
//! Piano tab reads it each frame to highlight active keys.

use std::sync::atomic::{AtomicU64, Ordering};

/// 128-bit atomic bitset, one bit per MIDI note number.
pub struct HeldNotes {
    bits: [AtomicU64; 2],
}

impl HeldNotes {
    pub const fn new() -> Self {
        Self {
            bits: [AtomicU64::new(0), AtomicU64::new(0)],
        }
    }

    pub fn note_on(&self, note: u8) {
        if note >= 128 {
            return;
        }
        let word = (note / 64) as usize;
        let bit = note % 64;
        self.bits[word].fetch_or(1u64 << bit, Ordering::Relaxed);
    }

    pub fn note_off(&self, note: u8) {
        if note >= 128 {
            return;
        }
        let word = (note / 64) as usize;
        let bit = note % 64;
        self.bits[word].fetch_and(!(1u64 << bit), Ordering::Relaxed);
    }

    pub fn is_held(&self, note: u8) -> bool {
        if note >= 128 {
            return false;
        }
        let word = (note / 64) as usize;
        let bit = note % 64;
        self.bits[word].load(Ordering::Relaxed) & (1u64 << bit) != 0
    }

    /// Currently-held note numbers, sorted ascending.
    pub fn held(&self) -> Vec<u8> {
        let lo = self.bits[0].load(Ordering::Relaxed);
        let hi = self.bits[1].load(Ordering::Relaxed);
        let mut out = Vec::new();
        for n in 0..64u8 {
            if lo & (1u64 << n) != 0 {
                out.push(n);
            }
        }
        for n in 0..64u8 {
            if hi & (1u64 << n) != 0 {
                out.push(64 + n);
            }
        }
        out
    }

    #[cfg(test)]
    pub fn count(&self) -> usize {
        let lo = self.bits[0].load(Ordering::Relaxed);
        let hi = self.bits[1].load(Ordering::Relaxed);
        (lo.count_ones() + hi.count_ones()) as usize
    }
}

impl Default for HeldNotes {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_off() {
        let h = HeldNotes::new();
        assert!(!h.is_held(60));
        h.note_on(60);
        assert!(h.is_held(60));
        h.note_on(72);
        assert_eq!(h.held(), vec![60, 72]);
        h.note_off(60);
        assert!(!h.is_held(60));
        assert_eq!(h.held(), vec![72]);
    }

    #[test]
    fn high_range() {
        let h = HeldNotes::new();
        h.note_on(100);
        h.note_on(127);
        assert!(h.is_held(100));
        assert!(h.is_held(127));
        assert_eq!(h.count(), 2);
    }

    #[test]
    fn out_of_range() {
        let h = HeldNotes::new();
        h.note_on(200);
        assert_eq!(h.count(), 0);
    }
}
