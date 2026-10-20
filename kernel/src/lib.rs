//! The real-time kernel of Tang's patch graph (`docs/patch-graph.md` §11).
//!
//! It knows nothing about lanes, groups, plugins or MIDI: it is an [`Arena`] that owns the state of nodes and hands a node back with its state when it is taken out, a
//! [`compile`] from a [`Patch`] (nodes with ports, and cables) to an immutable [`Plan`], and the executor that runs a plan over buffers that were made before the block
//! (`Plan::run`). What it promises is in the rules of §11.3: the work of a block is bounded by the size of the plan, nothing on the audio thread allocates or waits, the
//! order is deterministic, and a sum is made in a fixed order, so that a render is the same bits whatever the platform's scheduling does.

mod arena;
mod compile;
mod patch;

pub use arena::{Arena, Key};
pub use compile::{CompileError, Feed, Op, Plan, compile};
pub use patch::{Cable, Id, NodeSpec, Patch, Port, PortSpec};

#[cfg(test)]
mod testutil;
