//! The executor: a plan run over the buffers of a pool.
//!
//! A block is: for each op of the plan, zero the buffers where a value begins, hand the node the buffers of its ports, and add each of its outputs into the sums that the
//! cables from it end at (`Feed`). Nothing in it allocates once the pool is fitted to the plan and has room for the block (`Pool::reserve`): the buffers are moved to the
//! node and back (a move of a vector's three words, not a copy), and the sums are loops over slices.

use std::mem;

use anyhow::{Result, bail};

use crate::arena::{Arena, Key};
use crate::compile::Plan;
use crate::patch::Id;

/// A block of audio: the channels, each a vector of frames.
pub type ChanBuf = Vec<Vec<f32>>;

/// What a node is given for one block: the sum at each of its input ports (which it may use as scratch: it is not read again), and a buffer for each output port, zeroed.
pub struct Io {
    frames: usize,
    inputs: Vec<ChanBuf>,
    outputs: Vec<ChanBuf>,
}

impl Io {
    /// An `Io` of the buffers given, for the tests of nodes and for the reference interpreter. The executor makes its own.
    pub fn from_buffers(frames: usize, inputs: Vec<ChanBuf>, outputs: Vec<ChanBuf>) -> Self {
        Io { frames, inputs, outputs }
    }

    /// How many frames each channel of each buffer has this block.
    pub fn frames(&self) -> usize {
        self.frames
    }

    pub fn inputs(&self) -> &[ChanBuf] {
        &self.inputs
    }

    pub fn inputs_mut(&mut self) -> &mut [ChanBuf] {
        &mut self.inputs
    }

    pub fn outputs(&self) -> &[ChanBuf] {
        &self.outputs
    }

    pub fn outputs_mut(&mut self) -> &mut [ChanBuf] {
        &mut self.outputs
    }

    /// Both at once: a node that reads an input and writes an output needs the two borrowed together.
    pub fn split(&mut self) -> (&mut [ChanBuf], &mut [ChanBuf]) {
        (&mut self.inputs, &mut self.outputs)
    }

    /// Make an output the buffer that an input has, with what it holds: a node that works in place does so on its input and hands it on, and nothing is copied. The input is
    /// left with what the output had, which it is not to be read for. (The two must have the same number of channels.)
    pub fn forward(&mut self, input: usize, output: usize) {
        mem::swap(&mut self.inputs[input], &mut self.outputs[output]);
    }

    /// What the node made, when the `Io` was made by `from_buffers`.
    pub fn into_outputs(self) -> Vec<ChanBuf> {
        self.outputs
    }
}

/// The nodes of a plan, as the executor sees them: it names one by its id and asks for a block. A node that is not there is a node that makes silence (its outputs are zero),
/// and the implementation says so by doing nothing.
pub trait Nodes<I, C> {
    fn process(&mut self, id: I, io: &mut Io, cx: &mut C) -> Result<()>;
}

/// A node, for the nodes that live in an [`Arena`]: `C` is what every node is given besides its buffers, which is whatever the program needs to say about the block
/// (the notes that came in, the tempo, where to publish what is measured).
pub trait Node<C>: Send {
    fn process(&mut self, io: &mut Io, cx: &mut C) -> Result<()>;
}

impl<N: Node<C>, C> Nodes<Key, C> for Arena<N> {
    fn process(&mut self, id: Key, io: &mut Io, cx: &mut C) -> Result<()> {
        match self.get_mut(id) {
            Some(node) => node.process(io, cx),
            None => Ok(()),
        }
    }
}

/// The buffers that a plan runs over. A pool is fitted to a plan with `fit` and given room for blocks with `reserve`, both on a thread that may allocate; `run` then
/// allocates nothing.
pub struct Pool {
    buffers: Vec<ChanBuf>,
    io: Io,
    /// The frames that every channel has room for.
    reserved: usize,
}

impl Default for Pool {
    fn default() -> Self {
        Self::new()
    }
}

impl Pool {
    pub fn new() -> Self {
        Pool { buffers: Vec::new(), io: Io::from_buffers(0, Vec::new(), Vec::new()), reserved: 0 }
    }

    /// Give every channel of every buffer room for blocks of `frames` (and the buffers that `fit` makes later: the room is never taken back).
    pub fn reserve(&mut self, frames: usize) {
        self.reserved = self.reserved.max(frames);
        for channel in self.buffers.iter_mut().flatten() {
            channel.reserve(self.reserved.saturating_sub(channel.len()));
        }
    }

    /// Make the buffers a plan wants, keeping those there are (and their room): as many buffers as the plan has, each of the channels it says.
    pub fn fit<I: Id>(&mut self, plan: &Plan<I>) {
        self.buffers.resize_with(plan.buffers.len(), Vec::new);
        for (buffer, &channels) in self.buffers.iter_mut().zip(&plan.buffers) {
            buffer.resize_with(usize::from(channels), Vec::new);
            for channel in buffer.iter_mut() {
                channel.reserve(self.reserved.saturating_sub(channel.len()));
            }
        }
        // The scratch that the ops' buffers are moved into: between ops it is empty, so that this is room for the most that any op has.
        let (inputs, outputs) = plan.ops.iter().fold((0, 0), |(i, o), op| (i.max(op.inputs.len()), o.max(op.outputs.len())));
        self.io.inputs.reserve(inputs);
        self.io.outputs.reserve(outputs);
    }
}

/// Zero a buffer, at the length of this block.
fn zero(buffer: &mut ChanBuf, frames: usize) {
    for channel in buffer.iter_mut() {
        channel.clear();
        channel.resize(frames, 0.0);
    }
}

/// Add the buffer `from` into the buffer `into` (two buffers, not one), sample by sample.
fn add(buffers: &mut [ChanBuf], from: usize, into: usize) {
    let (source, sum) = if from < into {
        let (low, high) = buffers.split_at_mut(into);
        (&low[from], &mut high[0])
    } else {
        let (low, high) = buffers.split_at_mut(from);
        (&high[0], &mut low[into])
    };
    for (sum, source) in sum.iter_mut().zip(source) {
        for (x, y) in sum.iter_mut().zip(source) {
            *x += *y;
        }
    }
}

impl<I: Id> Plan<I> {
    /// Run one block of `frames` frames: each op in turn, as described at the top of this file. A node that fails stops the block, with the error, and the buffers are where
    /// they belong when it does, so that the next block runs whole.
    pub fn run<C>(&self, pool: &mut Pool, frames: usize, nodes: &mut impl Nodes<I, C>, cx: &mut C) -> Result<()> {
        // A pool with more buffers than the plan has is fine (the plan names its own); one with fewer, or with buffers of other widths, is not fitted to it.
        if pool.buffers.len() < self.buffers.len() || self.buffers.iter().zip(&pool.buffers).any(|(&channels, buffer)| buffer.len() != usize::from(channels)) {
            bail!("the pool is not fitted to the plan: the plan has buffers of {:?} channels", self.buffers);
        }
        pool.io.frames = frames;
        for op in &self.ops {
            for &buffer in &op.zero {
                zero(&mut pool.buffers[buffer as usize], frames);
            }
            pool.io.inputs.clear();
            pool.io.outputs.clear();
            for &buffer in &op.inputs {
                pool.io.inputs.push(mem::take(&mut pool.buffers[buffer as usize]));
            }
            for &buffer in &op.outputs {
                pool.io.outputs.push(mem::take(&mut pool.buffers[buffer as usize]));
            }
            let result = nodes.process(op.node, &mut pool.io, cx);
            // The buffers go back whether the node worked or not.
            for (&buffer, taken) in op.inputs.iter().zip(pool.io.inputs.drain(..)) {
                pool.buffers[buffer as usize] = taken;
            }
            for (&buffer, taken) in op.outputs.iter().zip(pool.io.outputs.drain(..)) {
                pool.buffers[buffer as usize] = taken;
            }
            result?;
            for feed in &op.feeds {
                add(&mut pool.buffers, op.outputs[feed.output as usize] as usize, feed.into as usize);
            }
        }
        Ok(())
    }
}

/// An arena of nodes with the plan that runs them, for the crate's own tests and benchmark and for anything that has only one kind of node.
pub struct Kernel<N> {
    arena: Arena<N>,
    plan: Plan<Key>,
    pool: Pool,
}

impl<N> Default for Kernel<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<N> Kernel<N> {
    pub fn new() -> Self {
        Kernel { arena: Arena::new(), plan: Plan { ops: Vec::new(), buffers: Vec::new() }, pool: Pool::new() }
    }

    pub fn arena(&self) -> &Arena<N> {
        &self.arena
    }

    pub fn arena_mut(&mut self) -> &mut Arena<N> {
        &mut self.arena
    }

    pub fn plan(&self) -> &Plan<Key> {
        &self.plan
    }

    /// Give the pool room for blocks of `frames`.
    pub fn reserve(&mut self, frames: usize) {
        self.pool.reserve(frames);
    }

    /// Run by another plan from the next block on, and hand back the one that was run, for whoever may drop it. The state of the nodes is in the arena and is not touched.
    pub fn install(&mut self, plan: Plan<Key>) -> Plan<Key> {
        self.pool.fit(&plan);
        mem::replace(&mut self.plan, plan)
    }

    pub fn run<C>(&mut self, frames: usize, cx: &mut C) -> Result<()>
    where
        N: Node<C>,
    {
        self.plan.run(&mut self.pool, frames, &mut self.arena, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::compile;
    use crate::patch::{NodeSpec, Patch, Port, PortSpec};
    use crate::reference::run_reference;
    use crate::testnodes::{Store, TestNode};
    use crate::testutil::{Rng, random_patch};

    const ST: PortSpec = PortSpec::audio(2);

    fn spec(inputs: usize, outputs: usize, rank: u32) -> NodeSpec {
        NodeSpec::new(vec![ST; inputs], vec![ST; outputs], rank)
    }

    /// `spec`, with inputs that are `single`.
    fn spec_single(inputs: usize, outputs: usize, rank: u32) -> NodeSpec {
        NodeSpec::new(vec![PortSpec::single(2); inputs], vec![ST; outputs], rank)
    }

    /// Nodes that are a function.
    struct Fns<F>(F);

    impl<F: FnMut(u32, &mut Io) -> Result<()>> Nodes<u32, ()> for Fns<F> {
        fn process(&mut self, id: u32, io: &mut Io, _: &mut ()) -> Result<()> {
            (self.0)(id, io)
        }
    }

    fn fitted(plan: &Plan<u32>) -> Pool {
        let mut pool = Pool::new();
        pool.fit(plan);
        pool
    }

    fn run(plan: &Plan<u32>, pool: &mut Pool, frames: usize, f: impl FnMut(u32, &mut Io) -> Result<()>) -> Result<()> {
        plan.run(pool, frames, &mut Fns(f), &mut ())
    }

    fn fill(buffer: &mut ChanBuf, value: f32) {
        for channel in buffer {
            channel.fill(value);
        }
    }

    /// A source of `value` in every sample, a sink, and a cable between them: `(plan, pool)`.
    fn source_and_sink() -> (Plan<u32>, Pool) {
        let mut patch = Patch::new();
        patch.add_node(1, spec(0, 1, 0));
        patch.add_node(2, spec(1, 0, 1));
        patch.connect(Port::new(1, 0), Port::new(2, 0));
        let plan = compile(&patch).unwrap();
        let pool = fitted(&plan);
        (plan, pool)
    }

    #[test]
    fn what_a_node_makes_is_what_the_node_that_it_has_a_cable_to_is_given() {
        let (plan, mut pool) = source_and_sink();
        let mut seen = Vec::new();
        run(&plan, &mut pool, 4, |id, io| {
            if id == 1 {
                for (c, channel) in io.outputs_mut()[0].iter_mut().enumerate() {
                    for (i, x) in channel.iter_mut().enumerate() {
                        *x = (i + 1) as f32 * (c + 1) as f32;
                    }
                }
            } else {
                seen = io.inputs()[0].clone();
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, vec![vec![1.0, 2.0, 3.0, 4.0], vec![2.0, 4.0, 6.0, 8.0]]);
    }

    /// Three sources of `values` (their ranks as given) into one sink: what the sink is given in its first sample.
    fn sum_of(values: [(f32, u32); 3]) -> f32 {
        let mut patch = Patch::new();
        for (at, (_, rank)) in values.iter().enumerate() {
            patch.add_node(at as u32, spec(0, 1, *rank));
        }
        patch.add_node(9, spec(1, 0, 99));
        for at in 0..3 {
            patch.connect(Port::new(at, 0), Port::new(9, 0));
        }
        let plan = compile(&patch).unwrap();
        let mut pool = fitted(&plan);
        let mut sum = f32::NAN;
        run(&plan, &mut pool, 2, |id, io| {
            if id == 9 {
                sum = io.inputs()[0][0][0];
            } else {
                fill(&mut io.outputs_mut()[0], values[id as usize].0);
            }
            Ok(())
        })
        .unwrap();
        sum
    }

    #[test]
    fn an_input_is_a_sum_of_the_cables_in_the_order_of_the_plan() {
        // 1e8 + 1 is 1e8 in 32 bits, so the order shows: (1e8 + 1) - 1e8 is 0, and (1e8 - 1e8) + 1 is 1.
        assert_eq!(sum_of([(1e8, 0), (1.0, 1), (-1e8, 2)]), 0.0);
        assert_eq!(sum_of([(1e8, 0), (-1e8, 1), (1.0, 2)]), 1.0);
        assert_eq!(sum_of([(1.0, 0), (1e8, 1), (-1e8, 2)]), 0.0);
    }

    #[test]
    fn a_sum_begins_at_positive_zero_so_a_negative_zero_through_one_cable_arrives_as_positive_zero() {
        let (plan, mut pool) = source_and_sink();
        let mut seen = Vec::new();
        run(&plan, &mut pool, 3, |id, io| {
            if id == 1 {
                fill(&mut io.outputs_mut()[0], -0.0);
            } else {
                seen = io.inputs()[0].iter().flatten().map(|x| x.to_bits()).collect();
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, vec![0u32; 6], "+0.0, not -0.0 (0x80000000)");
    }

    #[test]
    fn a_negative_zero_through_a_single_input_arrives_as_it_is_and_through_a_sum_as_a_positive_zero() {
        let sink_sees = |sink: NodeSpec| {
            let mut patch = Patch::new();
            patch.add_node(1, spec(0, 1, 0));
            patch.add_node(2, sink);
            patch.connect(Port::new(1, 0), Port::new(2, 0));
            let plan = compile(&patch).unwrap();
            let mut pool = fitted(&plan);
            let mut seen = Vec::new();
            run(&plan, &mut pool, 3, |id, io| {
                if id == 1 {
                    fill(&mut io.outputs_mut()[0], -0.0);
                } else {
                    seen = io.inputs()[0].iter().flatten().map(|x| x.to_bits()).collect();
                }
                Ok(())
            })
            .unwrap();
            seen
        };
        assert_eq!(sink_sees(NodeSpec::new(vec![PortSpec::single(2)], vec![], 1)), vec![0x8000_0000u32; 6], "-0.0, which is what it was");
        assert_eq!(sink_sees(spec(1, 0, 1)), vec![0u32; 6], "+0.0, as any sum is");
    }

    #[test]
    fn a_node_that_works_in_place_on_a_single_input_is_the_only_one_that_sees_the_change_and_the_chain_runs_on_block_after_block() {
        // 1 → 2 (single, in place, doubles) → 3 (single, in place, adds one) → 4 (a sum); 5 is fed by 1 as well, so 1's output is read by two and node 2's input is a sum of one that
        // node 2 may spoil without the node 5 seeing it.
        let mut patch = Patch::new();
        patch.add_node(1, spec(0, 1, 0));
        patch.add_node(2, spec_single(1, 1, 1));
        patch.add_node(3, spec_single(1, 1, 2));
        patch.add_node(4, spec(1, 0, 3));
        patch.add_node(5, spec(1, 0, 4));
        patch.connect(Port::new(1, 0), Port::new(2, 0));
        patch.connect(Port::new(1, 0), Port::new(5, 0));
        patch.connect(Port::new(2, 0), Port::new(3, 0));
        patch.connect(Port::new(3, 0), Port::new(4, 0));
        let plan = compile(&patch).unwrap();
        let mut pool = fitted(&plan);
        let (mut at_4, mut at_5) = (0.0, 0.0);
        for block in 0..3 {
            run(&plan, &mut pool, 2, |id, io| {
                match id {
                    1 => fill(&mut io.outputs_mut()[0], 3.0 + block as f32),
                    2 | 3 => {
                        for channel in &mut io.inputs_mut()[0] {
                            for x in channel {
                                *x = if id == 2 { *x * 2.0 } else { *x + 1.0 };
                            }
                        }
                        io.forward(0, 0);
                    }
                    4 => at_4 = io.inputs()[0][0][0],
                    _ => at_5 = io.inputs()[0][0][0],
                }
                Ok(())
            })
            .unwrap();
            assert_eq!((at_4, at_5), ((3.0 + block as f32) * 2.0 + 1.0, 3.0 + block as f32), "block {block}");
        }
    }

    #[test]
    fn a_single_input_with_no_cable_is_silence_even_in_a_buffer_that_another_node_filled() {
        let mut patch = Patch::new();
        patch.add_node(1, spec(0, 1, 0));
        patch.add_node(2, spec_single(1, 0, 1));
        let plan = compile(&patch).unwrap();
        assert_eq!(plan.ops()[0].outputs[0], plan.ops()[1].inputs[0], "the premise: the two share a buffer, though no cable joins them");
        let mut pool = fitted(&plan);
        let mut seen = Vec::new();
        for _ in 0..2 {
            run(&plan, &mut pool, 5, |id, io| {
                if id == 1 {
                    fill(&mut io.outputs_mut()[0], 7.0);
                } else {
                    seen = io.inputs()[0].iter().flatten().copied().collect();
                }
                Ok(())
            })
            .unwrap();
            assert_eq!(seen, vec![0.0; 10]);
        }
    }

    #[test]
    fn an_input_that_has_no_cable_is_silence_even_in_a_buffer_that_another_node_filled() {
        let mut patch = Patch::new();
        patch.add_node(1, spec(0, 1, 0));
        patch.add_node(2, spec(1, 0, 1));
        let plan = compile(&patch).unwrap();
        assert_eq!(plan.ops()[0].outputs[0], plan.ops()[1].inputs[0], "the premise: the two share a buffer");
        let mut pool = fitted(&plan);
        let mut seen = Vec::new();
        for _ in 0..2 {
            run(&plan, &mut pool, 5, |id, io| {
                if id == 1 {
                    fill(&mut io.outputs_mut()[0], 7.0);
                } else {
                    seen = io.inputs()[0].iter().flatten().copied().collect();
                }
                Ok(())
            })
            .unwrap();
            assert_eq!(seen, vec![0.0; 10]);
        }
    }

    #[test]
    fn an_output_is_zero_when_its_node_begins_even_in_a_buffer_that_another_node_filled() {
        let mut patch = Patch::new();
        patch.add_node(1, spec(0, 1, 0));
        patch.add_node(2, spec(0, 1, 1));
        let plan = compile(&patch).unwrap();
        assert_eq!(plan.ops()[0].outputs[0], plan.ops()[1].outputs[0], "the premise: the two share a buffer");
        let mut pool = fitted(&plan);
        let mut at_entry = Vec::new();
        run(&plan, &mut pool, 4, |id, io| {
            if id == 2 {
                at_entry = io.outputs()[0].iter().flatten().copied().collect();
            }
            fill(&mut io.outputs_mut()[0], 5.0);
            Ok(())
        })
        .unwrap();
        assert_eq!(at_entry, vec![0.0; 8]);
    }

    #[test]
    fn a_node_that_works_in_place_hands_its_input_on_with_forward() {
        let mut patch = Patch::new();
        patch.add_node(1, spec(0, 1, 0));
        patch.add_node(2, spec(1, 1, 1));
        patch.add_node(3, spec(1, 0, 2));
        patch.connect(Port::new(1, 0), Port::new(2, 0));
        patch.connect(Port::new(2, 0), Port::new(3, 0));
        let plan = compile(&patch).unwrap();
        let mut pool = fitted(&plan);
        let mut seen = Vec::new();
        for block in 0..3 {
            run(&plan, &mut pool, 3, |id, io| {
                match id {
                    1 => fill(&mut io.outputs_mut()[0], 3.0 + block as f32),
                    2 => {
                        for channel in &mut io.inputs_mut()[0] {
                            for x in channel {
                                *x *= 2.0;
                            }
                        }
                        io.forward(0, 0);
                    }
                    _ => seen = io.inputs()[0].iter().flatten().copied().collect(),
                }
                Ok(())
            })
            .unwrap();
            assert_eq!(seen, vec![(3.0 + block as f32) * 2.0; 6], "block {block}");
        }
    }

    #[test]
    fn a_node_that_fails_stops_the_block_with_the_error_and_the_next_block_runs_whole() {
        let (plan, mut pool) = source_and_sink();
        let mut seen = 0.0;
        for (block, expected) in [Ok(4.0), Err("boom"), Ok(6.0)].into_iter().enumerate() {
            let result = run(&plan, &mut pool, 2, |id, io| {
                if id == 1 {
                    fill(&mut io.outputs_mut()[0], 4.0 + block as f32);
                    Ok(())
                } else if block == 1 {
                    anyhow::bail!("boom")
                } else {
                    seen = io.inputs()[0][1][1];
                    Ok(())
                }
            });
            match (result, expected) {
                (Ok(()), Ok(value)) => assert_eq!(seen, value, "block {block}"),
                (Err(error), Err(message)) => assert_eq!(error.to_string(), message),
                (result, expected) => panic!("block {block}: {result:?} and {expected:?}"),
            }
            assert_eq!(pool.buffers.len(), plan.buffers().len(), "the buffers are all where they were");
            assert!(pool.buffers.iter().zip(plan.buffers()).all(|(buffer, &channels)| buffer.len() == usize::from(channels)));
        }
    }

    #[test]
    fn a_block_of_another_length_is_run_with_buffers_of_that_length_and_a_block_of_none_is_run() {
        let (plan, mut pool) = source_and_sink();
        for frames in [4, 7, 0, 1, 64] {
            let mut lengths = Vec::new();
            run(&plan, &mut pool, frames, |id, io| {
                assert_eq!(io.frames(), frames);
                let buffer = if id == 1 { &io.outputs()[0] } else { &io.inputs()[0] };
                lengths.extend(buffer.iter().map(Vec::len));
                Ok(())
            })
            .unwrap();
            assert_eq!(lengths, vec![frames; 4], "frames {frames}");
        }
    }

    #[test]
    fn the_nodes_are_run_in_the_order_of_the_plan() {
        let mut patch = Patch::new();
        for (id, rank) in [(4, 1), (2, 0), (7, 5)] {
            patch.add_node(id, spec(0, 0, rank));
        }
        let plan = compile(&patch).unwrap();
        let mut pool = fitted(&plan);
        let mut ran = Vec::new();
        run(&plan, &mut pool, 1, |id, _| {
            ran.push(id);
            Ok(())
        })
        .unwrap();
        assert_eq!(ran, [2, 4, 7]);
    }

    #[test]
    fn a_pool_has_the_buffers_of_the_plan_it_is_fitted_to_with_the_room_it_was_given_and_keeps_what_it_has() {
        let mut patch = Patch::new();
        patch.add_node(1, NodeSpec::new(vec![], vec![PortSpec::audio(1)], 0));
        patch.add_node(2, NodeSpec::new(vec![PortSpec::audio(1)], vec![], 1));
        patch.connect(Port::new(1, 0), Port::new(2, 0));
        let small = compile(&patch).unwrap();
        patch.add_node(3, spec(0, 1, 2));
        patch.add_node(4, spec(1, 0, 3));
        patch.connect(Port::new(3, 0), Port::new(4, 0));
        let large = compile(&patch).unwrap();
        assert!(large.buffers().len() > small.buffers().len());
        let mut pool = Pool::new();
        pool.reserve(256);
        pool.fit(&small);
        assert_eq!(pool.buffers.iter().map(Vec::len).collect::<Vec<_>>(), small.buffers().iter().map(|&c| usize::from(c)).collect::<Vec<_>>());
        assert!(pool.buffers.iter().flatten().all(|channel| channel.capacity() >= 256));
        let kept = pool.buffers[0][0].as_ptr();
        pool.reserve(8);
        pool.fit(&large);
        assert_eq!(pool.buffers.iter().map(Vec::len).collect::<Vec<_>>(), large.buffers().iter().map(|&c| usize::from(c)).collect::<Vec<_>>());
        assert!(pool.buffers.iter().flatten().all(|channel| channel.capacity() >= 256), "the new buffers have room too");
        assert_eq!(pool.buffers[0][0].as_ptr(), kept, "the buffer that was there is the one that is there");
        pool.fit(&small);
        assert_eq!(pool.buffers.len(), small.buffers().len());
        assert_eq!(pool.buffers[0][0].as_ptr(), kept);
    }

    #[test]
    fn room_that_is_reserved_after_the_pool_is_fitted_is_made_in_the_buffers_that_are_there() {
        let (plan, _) = source_and_sink();
        let mut pool = Pool::new();
        pool.fit(&plan);
        assert!(pool.buffers.iter().flatten().all(|channel| channel.capacity() == 0), "the premise: nothing was reserved");
        pool.reserve(128);
        assert!(pool.buffers.iter().flatten().all(|channel| channel.capacity() >= 128));
    }

    #[test]
    fn a_pool_that_is_not_fitted_to_the_plan_is_an_error_and_not_a_panic() {
        let (plan, _) = source_and_sink();
        let error = run(&plan, &mut Pool::new(), 4, |_, _| Ok(())).unwrap_err();
        assert!(error.to_string().contains("fitted"), "{error}");
    }

    #[test]
    fn a_pool_with_more_buffers_than_the_plan_has_runs_it_and_one_with_buffers_of_other_widths_does_not() {
        let (plan, _) = source_and_sink();
        // Fitted to a plan of twice the buffers (the same ones first): runs.
        let mut bigger = Patch::new();
        // Two sources that run before the two sinks: three values are alive at once, where the plan of one source and one sink has two.
        for (id, ins, outs, rank) in [(1, 0, 1, 0), (3, 0, 1, 1), (2, 1, 0, 2), (4, 1, 0, 3)] {
            bigger.add_node(id, spec(ins, outs, rank));
        }
        bigger.connect(Port::new(1, 0), Port::new(2, 0));
        bigger.connect(Port::new(3, 0), Port::new(4, 0));
        let mut pool = fitted(&compile(&bigger).unwrap());
        assert!(pool.buffers.len() > plan.buffers().len());
        run(&plan, &mut pool, 4, |_, _| Ok(())).unwrap();
        // Fitted to a plan of mono buffers: the same number of buffers, and not the ones this plan names.
        let mut mono = Patch::new();
        mono.add_node(1, NodeSpec::new(vec![], vec![PortSpec::audio(1)], 0));
        mono.add_node(2, NodeSpec::new(vec![PortSpec::audio(1)], vec![], 1));
        mono.connect(Port::new(1, 0), Port::new(2, 0));
        let mut pool = fitted(&compile(&mono).unwrap());
        assert_eq!(pool.buffers.len(), plan.buffers().len());
        let error = run(&plan, &mut pool, 4, |_, _| Ok(())).unwrap_err();
        assert!(error.to_string().contains("fitted"), "{error}");
    }

    // ---- the executor against the reference interpreter ------------------------------------------------------------------------------------------------

    const BLOCKS: [usize; 6] = [16, 3, 32, 1, 16, 7];

    #[test]
    fn a_random_plan_gives_what_the_reference_interpreter_gives_block_after_block_bit_for_bit() {
        for seed in 0..400 {
            let patch = random_patch(seed);
            let plan = compile(&patch).unwrap();
            let order: Vec<u32> = plan.order().collect();
            let mut pool = fitted(&plan);
            let mut kernel = Store::for_patch(&patch, &mut Rng::new(seed ^ 0xF0F0), 0.4);
            let mut reference = kernel.clone();
            for (block, &frames) in BLOCKS.iter().enumerate() {
                plan.run(&mut pool, frames, &mut kernel, &mut ()).unwrap();
                run_reference(&patch, &order, frames, &mut reference, &mut ()).unwrap();
                assert_eq!(kernel.states(), reference.states(), "seed {seed}, block {block}");
            }
        }
    }

    #[test]
    fn a_node_that_fails_in_a_random_plan_stops_the_block_where_the_reference_stops_it_and_the_next_blocks_agree() {
        for seed in 0..200 {
            let patch = random_patch(seed);
            let plan = compile(&patch).unwrap();
            let order: Vec<u32> = plan.order().collect();
            let mut pool = fitted(&plan);
            let mut rng = Rng::new(seed ^ 0xFA11);
            let mut kernel = Store::for_patch(&patch, &mut rng, 0.4);
            kernel.0.get_mut(&order[rng.below(order.len())]).unwrap().fails_at = Some(2);
            let mut reference = kernel.clone();
            for (block, &frames) in BLOCKS.iter().enumerate() {
                let a = plan.run(&mut pool, frames, &mut kernel, &mut ());
                let b = run_reference(&patch, &order, frames, &mut reference, &mut ());
                assert_eq!(a.is_err(), b.is_err(), "seed {seed}, block {block}");
                assert_eq!(a.is_err(), block == 2, "seed {seed}, block {block}");
                assert_eq!(kernel.states(), reference.states(), "seed {seed}, block {block}");
            }
        }
    }

    /// The nodes of a store in an arena, and the patch made over their keys: the keys by id.
    fn into_arena(patch: &Patch<u32>, store: &Store) -> (Arena<TestNode>, Patch<Key>, std::collections::HashMap<u32, Key>) {
        let mut arena = Arena::new();
        let mut keys = std::collections::HashMap::new();
        let mut keyed = Patch::new();
        for (id, spec) in patch.nodes() {
            let key = arena.insert(store.0[id].clone());
            keys.insert(*id, key);
            keyed.add_node(key, spec.clone());
        }
        for c in patch.cables() {
            keyed.connect(Port::new(keys[&c.from.node], c.from.port), Port::new(keys[&c.to.node], c.to.port));
        }
        (arena, keyed, keys)
    }

    #[test]
    fn the_state_of_nodes_in_an_arena_goes_on_through_a_swap_of_plans() {
        for seed in 0..150 {
            let patch = random_patch(seed);
            let store = Store::for_patch(&patch, &mut Rng::new(seed ^ 0xA2E4), 0.4);
            let plan = compile(&patch).unwrap();
            let order: Vec<u32> = plan.order().collect();
            let mut reference = store.clone();

            let (arena, keyed, keys) = into_arena(&patch, &store);
            let mut kernel = Kernel::new();
            *kernel.arena_mut() = arena;
            kernel.reserve(32);
            assert!(kernel.install(compile(&keyed).unwrap()).ops().is_empty(), "there was no plan");

            // The second plan has one node more, with no cables, put in first among those of its rank: the others are laid out as they were, or not, and run in the same order.
            let mut bigger = Patch::new();
            let extra = kernel.arena_mut().insert(TestNode::new(&spec(0, 0, 0), false));
            bigger.add_node(extra, spec(0, 0, 0));
            for (key_spec, _) in keyed.nodes().iter().map(|n| (n, ())) {
                bigger.add_node(key_spec.0, key_spec.1.clone());
            }
            for c in keyed.cables() {
                bigger.connect(c.from, c.to);
            }
            let bigger = compile(&bigger).unwrap();

            for (block, &frames) in BLOCKS.iter().enumerate() {
                if block == 3 {
                    let old = kernel.install(bigger.clone());
                    assert_eq!(old.ops().len(), keyed.nodes().len(), "seed {seed}: the plan that was run is handed back");
                }
                kernel.run(frames, &mut ()).unwrap();
                run_reference(&patch, &order, frames, &mut reference, &mut ()).unwrap();
                let ours: Vec<_> = reference.states().iter().map(|(id, _)| (*id, kernel.arena()[keys[id]].state())).collect();
                assert_eq!(ours, reference.states(), "seed {seed}, block {block}");
            }
        }
    }

    /// A source of ones and a sink that writes down the first sample it is given.
    enum Probe {
        Source,
        Sink(Vec<f32>),
    }

    impl Node<()> for Probe {
        fn process(&mut self, io: &mut Io, _: &mut ()) -> Result<()> {
            match self {
                Probe::Source => fill(&mut io.outputs_mut()[0], 1.0),
                Probe::Sink(seen) => seen.push(io.inputs()[0][0][0]),
            }
            Ok(())
        }
    }

    #[test]
    fn a_node_whose_key_is_gone_makes_silence_and_the_others_run() {
        let mut kernel = Kernel::new();
        let source = kernel.arena_mut().insert(Probe::Source);
        let sink = kernel.arena_mut().insert(Probe::Sink(Vec::new()));
        let mut patch = Patch::new();
        patch.add_node(source, spec(0, 1, 0));
        patch.add_node(sink, spec(1, 0, 1));
        patch.connect(Port::new(source, 0), Port::new(sink, 0));
        kernel.install(compile(&patch).unwrap());
        kernel.run(8, &mut ()).unwrap();
        assert!(kernel.arena_mut().remove(source).is_some());
        kernel.run(8, &mut ()).unwrap();
        let Probe::Sink(seen) = &kernel.arena()[sink] else { panic!("the sink is a sink") };
        assert_eq!(seen, &[1.0, 0.0], "the second block gave the sink silence, and it ran");
    }
}
