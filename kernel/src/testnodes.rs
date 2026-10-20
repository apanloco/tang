//! Nodes for the tests: they make numbers that are not round (so that a different order of a sum is a different number), have memory from one block to the next (so that
//! state that is lost or moved is seen), and write what they were given and what they made into hashes of the bits (so that a sample that is off by a bit, or a zero
//! with the wrong sign, is seen).

use std::collections::HashMap;

use anyhow::{Result, bail};

use crate::exec::{Io, Node, Nodes};
use crate::patch::{NodeSpec, Patch};
use crate::testutil::Rng;

const BASIS: u64 = 0xcbf2_9ce4_8422_2325;

fn fnv(hash: u64, bits: u32) -> u64 {
    (hash ^ u64::from(bits)).wrapping_mul(0x0000_0100_0000_01b3)
}

/// What a node holds, as bits: its two hashes, how many blocks it has had, and its memory.
pub type State = (u64, u64, u32, Vec<Vec<u32>>);

/// What a test node is.
#[derive(Clone, Debug)]
pub struct TestNode {
    inputs: usize,
    /// Work in place and hand the input on, for output 0 (needs input 0 and output 0 of one width).
    forwards: bool,
    /// Make output 0 exactly what input 0 is, bit for bit and zeros of either sign included (a node with one input and one output of one width): what a node that is no more than
    /// a cable does, which is how the sign of a zero is followed through a chain.
    copies: bool,
    /// Fail at this block (counting from 0), after having looked at its inputs.
    pub fails_at: Option<u32>,
    /// A hash of the bits of every sample it was given, and of every one it made.
    pub seen: u64,
    pub made: u64,
    pub blocks: u32,
    /// The last frame of each channel of each output: memory from one block to the next.
    last: Vec<Vec<f32>>,
}

impl TestNode {
    pub fn new(spec: &NodeSpec, forwards: bool) -> Self {
        let can = spec.inputs.first().zip(spec.outputs.first()).is_some_and(|(i, o)| i.channels == o.channels);
        TestNode { inputs: spec.inputs.len(), forwards: forwards && can, copies: false, fails_at: None, seen: BASIS, made: BASIS, blocks: 0, last: Vec::new() }
    }

    /// A node that makes what it is given, for a node of one input and one output of one width (any other is as it was).
    pub fn copying(mut self, spec: &NodeSpec) -> Self {
        self.copies = spec.inputs.len() == 1 && spec.outputs.len() == 1 && spec.inputs[0].channels == spec.outputs[0].channels && !self.forwards;
        self
    }

    /// Everything that is the node's, as bits: equal when two nodes have been through the same.
    pub fn state(&self) -> State {
        (self.seen, self.made, self.blocks, self.last.iter().map(|o| o.iter().map(|x| x.to_bits()).collect()).collect())
    }

    /// What a node with no input makes: a pattern of numbers that are not round, with exact zeros of both signs.
    fn source(block: u32, output: usize, channel: usize, frame: usize) -> f32 {
        match (block as usize * 31 + output * 17 + channel * 7 + frame * 3) % 23 {
            0 => -0.0,
            1 => 0.0,
            k => k as f32 * 0.1 - 1.1,
        }
    }
}

impl Node<()> for TestNode {
    fn process(&mut self, io: &mut Io, _: &mut ()) -> Result<()> {
        let block = self.blocks;
        for buffer in io.inputs() {
            for channel in buffer {
                for &x in channel {
                    self.seen = fnv(self.seen, x.to_bits());
                }
            }
        }
        if self.fails_at == Some(block) {
            self.blocks += 1;
            bail!("a test node that fails at block {block}");
        }
        let (inputs, outputs) = io.split();
        self.last.resize(outputs.len(), Vec::new());
        for (m, output) in outputs.iter_mut().enumerate() {
            self.last[m].resize(output.len(), 0.0);
            for (c, channel) in output.iter_mut().enumerate() {
                let echo = self.last[m][c];
                for (i, y) in channel.iter_mut().enumerate() {
                    if self.copies {
                        *y = inputs[0][c][i];
                        continue;
                    }
                    let mut v = if self.inputs == 0 { Self::source(block, m, c, i) } else { 0.0 };
                    for (k, input) in inputs.iter().enumerate() {
                        v += input[c % input.len()][i] * (0.5 + 0.25 * ((m + k) % 3) as f32);
                    }
                    // A source's negative zero is made as it is (so that it can be followed: a sum or a product would turn it into a positive one).
                    *y = if self.inputs == 0 && v.to_bits() == (-0.0f32).to_bits() { v } else { v + 0.125 * echo };
                }
                self.last[m][c] = channel.last().copied().unwrap_or(echo);
            }
        }
        if self.forwards {
            // Output 0 is made again from input 0, in place, and the input's buffer is handed on in place of the output's.
            let (inputs, outputs) = io.split();
            for (c, channel) in inputs[0].iter_mut().enumerate() {
                for (i, x) in channel.iter_mut().enumerate() {
                    *x = *x * 0.75 + outputs[0][c][i] * 0.0625;
                }
            }
            io.forward(0, 0);
            self.last[0] = io.outputs()[0].iter().map(|c| c.last().copied().unwrap_or(0.0)).collect();
        }
        for buffer in io.outputs() {
            for channel in buffer {
                for &x in channel {
                    self.made = fnv(self.made, x.to_bits());
                }
            }
        }
        self.blocks += 1;
        Ok(())
    }
}

/// Test nodes by id, for plans whose ids are numbers.
#[derive(Clone, Debug, Default)]
pub struct Store(pub HashMap<u32, TestNode>);

impl Nodes<u32, ()> for Store {
    fn process(&mut self, id: u32, io: &mut Io, cx: &mut ()) -> Result<()> {
        match self.0.get_mut(&id) {
            Some(node) => node.process(io, cx),
            None => Ok(()),
        }
    }
}

impl Store {
    /// A node for each node of the patch; `forwards` is drawn for each from `rng` with the chance given.
    pub fn for_patch(patch: &Patch<u32>, rng: &mut Rng, forwards: f64) -> Self {
        // (A node is a copy of its input, if it has one input and one output of one width, for one in four of the ids: drawn from the id, so that the generator's draws are as they were.)
        Store(
            patch
                .nodes()
                .iter()
                .map(|(id, spec)| {
                    let node = TestNode::new(spec, rng.chance(forwards));
                    (*id, if id.wrapping_mul(2_654_435_761) >> 9 & 3 == 0 { node.copying(spec) } else { node })
                })
                .collect(),
        )
    }

    /// Everything that the nodes hold, by id, as bits.
    pub fn states(&self) -> Vec<(u32, State)> {
        let mut all: Vec<_> = self.0.iter().map(|(id, node)| (*id, node.state())).collect();
        all.sort_by_key(|(id, _)| *id);
        all
    }
}
