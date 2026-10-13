//! The web runtime (feature `web`): the part of Tang that a browser talks to; see
//! `docs/web-ui.md` §3.2, §7 and §8.
//!
//! The controller, the audio thread and the MIDI threads stay synchronous and
//! never see a socket. The runtime lives on threads of its own (an async server),
//! and the two sides meet in a few places: requests go to the owner of the
//! controller over a channel, what the controller applies and the telemetry come
//! back through the hub to every client's bounded queue, and notes go straight to
//! the audio thread through `NoteInput`.

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use crate::app::layout::TelemetryLayout;

pub mod connection;
pub mod hub;
pub mod owner;
pub mod pusher;
pub mod runtime;
pub mod security;
pub mod server;
#[cfg(test)]
mod testclient;

/// What the owner of the controller shares with every connection and the telemetry
/// pusher: things they only read.
pub struct Shared {
    /// The session's revision, for `Hello`.
    pub rev: AtomicU64,
    /// What the telemetry slots mean now, for the frames.
    layout: Mutex<Arc<TelemetryLayout>>,
}

impl Shared {
    #[cfg_attr(not(test), expect(dead_code, reason = "the server (later in step 10) makes it"))]
    pub fn new(rev: u64, layout: TelemetryLayout) -> Shared {
        Shared { rev: AtomicU64::new(rev), layout: Mutex::new(Arc::new(layout)) }
    }

    pub fn layout(&self) -> Arc<TelemetryLayout> {
        self.layout.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
    }

    pub fn set_layout(&self, layout: TelemetryLayout) {
        *self.layout.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Arc::new(layout);
    }
}
