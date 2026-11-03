//! The patch: what a plan is made from. Nodes with audio ports, and cables from the output of one to the input of another. It is data, kept on the main thread, and
//! `compile` is what checks it.

use std::fmt::Debug;
use std::hash::Hash;

/// What a node is called. Anything that is copied, compared and hashed will do: a key of an arena in the program, a number in the tests.
pub trait Id: Copy + Eq + Hash + Debug {}

impl<T: Copy + Eq + Hash + Debug> Id for T {}

/// A port carries a block of `channels` channels of audio. (Events and control are ports of other kinds, which come with P4 and P5 of `docs/patch-graph.md` §11.6.)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortSpec {
    pub channels: u16,
    /// Only an input has a use for it: see [`PortSpec::single`].
    pub single: bool,
}

impl PortSpec {
    /// A port whose input is a sum: it begins at +0.0 and the cables that end at it are added to it in plan order, a single cable too (`0.0 + -0.0` is `+0.0`).
    pub const fn audio(channels: u16) -> Self {
        PortSpec { channels, single: false }
    }

    /// An input that is *its cable's buffer*, when it has exactly one cable and that cable is the only one that leaves its output: nothing is zeroed, added or copied, and the
    /// node reads (and may work in place on) what the node before it made, bit for bit. With no cable it is silence, and with two or more it is a sum, as any input is.
    pub const fn single(channels: u16) -> Self {
        PortSpec { channels, single: true }
    }
}

/// What a node has: its ports, and its *rank*, which is what decides the order of nodes that nothing else orders (the lowest rank goes first; equal ranks go in the order
/// the nodes were put in the patch). A rank is the layout of a template, such as the position of a lane in the session, and it is what makes a render reproducible.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct NodeSpec {
    pub inputs: Vec<PortSpec>,
    pub outputs: Vec<PortSpec>,
    pub rank: u32,
}

impl NodeSpec {
    pub fn new(inputs: impl Into<Vec<PortSpec>>, outputs: impl Into<Vec<PortSpec>>, rank: u32) -> Self {
        NodeSpec { inputs: inputs.into(), outputs: outputs.into(), rank }
    }
}

/// A port of a node: an input port of the node that a cable ends at, an output port of the one it starts at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Port<I> {
    pub node: I,
    pub port: u16,
}

impl<I> Port<I> {
    pub const fn new(node: I, port: u16) -> Self {
        Port { node, port }
    }
}

/// From an output port to an input port. An input that several cables end at is the sum of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cable<I> {
    pub from: Port<I>,
    pub to: Port<I>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Patch<I> {
    nodes: Vec<(I, NodeSpec)>,
    cables: Vec<Cable<I>>,
}

impl<I> Default for Patch<I> {
    fn default() -> Self {
        Patch { nodes: Vec::new(), cables: Vec::new() }
    }
}

impl<I: Id> Patch<I> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Put a node in. The order they are put in is the order of nodes of one rank.
    pub fn add_node(&mut self, id: I, spec: NodeSpec) {
        self.nodes.push((id, spec));
    }

    /// Draw a cable from an output port to an input port. The order the cables are drawn in is the order that a node's feeds into its inputs are made in.
    pub fn connect(&mut self, from: Port<I>, to: Port<I>) {
        self.cables.push(Cable { from, to });
    }

    pub fn nodes(&self) -> &[(I, NodeSpec)] {
        &self.nodes
    }

    pub fn cables(&self) -> &[Cable<I>] {
        &self.cables
    }

    /// Empty the patch and keep the room it had, so that a patch that is made again does not ask the allocator for it.
    pub fn clear(&mut self) {
        self.nodes.clear();
        self.cables.clear();
    }
}
