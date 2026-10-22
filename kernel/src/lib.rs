//! The real-time kernel of Tang's patch graph (`docs/patch-graph.md` §11).
//!
//! It knows nothing about lanes, groups, plugins or MIDI: it is an [`Arena`] that owns the state of nodes and hands a node back with its state when it is taken out, a
//! [`compile`] from a [`Patch`] (nodes with ports, and cables) to an immutable [`Plan`], and the executor that runs a plan over buffers that were made before the block
//! (`Plan::run`), and what puts the paths that are added in step where plugins are late (`align` finds the delay of each cable from the latencies of the nodes, `with_delays` makes the patch that has the
//! delays in it as nodes, `DelayLine` is what they are made of). What it promises is in the rules of §11.3: the work of a block is bounded by the size of the plan, nothing on the audio thread allocates or waits, the
//! order is deterministic, and a sum is made in a fixed order, so that a render is the same bits whatever the platform's scheduling does.

mod arena;
mod compile;
mod delay;
mod exec;
mod latency;
mod patch;

pub use arena::{Arena, Key};
pub use compile::{CompileError, Feed, Op, Plan, compile};
pub use delay::DelayLine;
pub use exec::{ChanBuf, Io, Kernel, Node, Nodes, Pool};
pub use latency::{Alignment, align, with_delays};
pub use patch::{Cable, Id, NodeSpec, Patch, Port, PortSpec};

#[cfg(test)]
mod reference;
#[cfg(test)]
mod testnodes;
#[cfg(test)]
mod testutil;
