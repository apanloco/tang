//! The application layer between the front-ends (the TUI today, a web server
//! later) and the audio engine; see `docs/web-ui.md`.
//!
//! It starts with the session model and grows, step by step, into the
//! controller that applies intents to both the model and the engine.

pub mod apply;
pub mod catalog;
pub mod controller;
pub mod intent;
pub mod load;
pub mod model;
pub mod protocol;
pub mod save;
#[cfg(test)]
pub mod testutil;
