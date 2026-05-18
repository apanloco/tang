//! Shared runtime state for the Piano tab — the current scale and the
//! "locked / highlight" mode. Lives behind an `Arc` and is updated from the
//! TUI; consulted from the hardware MIDI thread and the virtual piano so they
//! can drop off-scale notes when locked.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use crate::scale::ScaleSetting;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PianoMode {
    /// Play anything. The Piano-tab visualization paints off-scale held notes
    /// red as a "wrong note" hint, but they still sound.
    Highlight,
    /// Drop off-scale note-ons at the sender. The notes never reach the audio
    /// thread, and they don't appear in the held visualization.
    Locked,
}

impl PianoMode {
    pub fn label(self) -> &'static str {
        match self {
            PianoMode::Highlight => "Highlight",
            PianoMode::Locked => "Locked",
        }
    }
}

pub struct PianoFilter {
    root: AtomicU8,
    scale_idx: AtomicU8,
    locked: AtomicBool,
}

impl PianoFilter {
    pub fn new(scale: ScaleSetting, mode: PianoMode) -> Self {
        Self {
            root: AtomicU8::new(scale.root),
            scale_idx: AtomicU8::new(scale.scale_idx as u8),
            locked: AtomicBool::new(matches!(mode, PianoMode::Locked)),
        }
    }

    pub fn scale(&self) -> ScaleSetting {
        ScaleSetting {
            root: self.root.load(Ordering::Relaxed),
            scale_idx: self.scale_idx.load(Ordering::Relaxed) as usize,
        }
    }

    pub fn set_scale(&self, scale: ScaleSetting) {
        self.root.store(scale.root, Ordering::Relaxed);
        self.scale_idx.store(scale.scale_idx as u8, Ordering::Relaxed);
    }

    pub fn mode(&self) -> PianoMode {
        if self.locked.load(Ordering::Relaxed) {
            PianoMode::Locked
        } else {
            PianoMode::Highlight
        }
    }

    pub fn set_mode(&self, mode: PianoMode) {
        self.locked
            .store(matches!(mode, PianoMode::Locked), Ordering::Relaxed);
    }

    /// True if this note-on should be dropped (locked mode + off-scale).
    /// Always returns false for note-off events — those need to pass through
    /// to clean up state from notes that may have been started before lock.
    pub fn block_note_on(&self, note: u8) -> bool {
        self.locked.load(Ordering::Relaxed) && !self.scale().contains(note)
    }
}
