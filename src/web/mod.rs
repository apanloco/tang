//! The web runtime (feature `web`): the part of Tang that a browser talks to; see
//! `docs/web-ui.md` §3.2, §7 and §8.
//!
//! The controller, the audio thread and the MIDI threads stay synchronous and
//! never see a socket. The runtime lives on threads of its own (an async server),
//! and the two sides meet in a few places: requests go to the owner of the
//! controller over a channel, what the controller applies and the telemetry come
//! back through the hub to every client's bounded queue, and notes go straight to
//! the audio thread through `NoteInput`.

pub mod connection;
pub mod hub;
pub mod security;
