//! Latency compensation (P3f of `docs/patch-graph.md` §11.6): finding how late what each node makes is, and putting the cables that are added at one input in step.
//!
//! A node that makes its output later than its input (a plugin with a look-ahead, an oversampling filter) has a *latency*, in frames. Where the cables of two paths are added at an
//! input and the paths do not have the same latency, the one that is early is out of step with the other, and the sum is a comb filter: the dry sound and the sound that has been
//! through the plugin do not line up. [`align`] works out, for every cable, how many frames it has to be delayed for what arrives at an input to be in step, and [`with_delays`] makes
//! the patch that has those delays in it, as nodes (a [`DelayLine`](crate::DelayLine) is what each is made of), so that `compile` and the executor need to know nothing of latency.
//!
//! The rule is the usual one: what arrives at a node is as late as the latest of the cables that end at it (all its input ports together: a sidechain is in step with the main
//! input), and what it makes is that much later by its own latency. The cable that is the latest is not delayed; the others are by the difference, so that nothing is made later
//! than it has to be, and the latency of the patch is that of its longest path.

use std::collections::HashMap;

use crate::compile::{CompileError, Layout, layout};
use crate::patch::{Cable, Id, NodeSpec, Patch, PortSpec};

/// What [`align`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alignment<I> {
    /// For each cable of the patch, by its place: the frames it is to be delayed by.
    delays: Vec<u32>,
    /// For each node of the patch, in the order it was put in: its id, how late what arrives at it is (once the cables are in step) and how late what it makes is.
    nodes: Vec<(I, u32, u32)>,
}

impl<I: Id> Alignment<I> {
    /// For each cable of the patch, by its place: how many frames it is to be delayed by, so that what arrives at an input is in step. A cable that is the latest at its input, and
    /// every cable that is the only one at its input, has none.
    pub fn delays(&self) -> &[u32] {
        &self.delays
    }

    /// Whether any cable is to be delayed: a patch that has no use for compensation is made no different by it.
    pub fn is_needed(&self) -> bool {
        self.delays.iter().any(|&frames| frames > 0)
    }

    /// How late what arrives at the inputs of a node is, and how late what it makes is (that and its own latency); none for a node that is not in the patch.
    pub fn node(&self, id: I) -> Option<(u32, u32)> {
        self.nodes.iter().find(|(node, _, _)| *node == id).map(|&(_, arrives, makes)| (arrives, makes))
    }

    /// How late the latest of what any node makes is: the latency of the patch.
    pub fn latency(&self) -> u32 {
        self.nodes.iter().map(|&(_, _, makes)| makes).max().unwrap_or(0)
    }
}

/// Find, for the patch, how many frames each cable is to be delayed by: `latency` says how many frames a node makes its output later than its input (0 for a node that has none).
/// A patch is refused for what `compile` refuses it for.
pub fn align<I: Id>(patch: &Patch<I>, latency: impl Fn(I) -> u32) -> Result<Alignment<I>, CompileError<I>> {
    let nodes = patch.nodes();
    let Layout { from_at, to_at, leaving, order } = layout(patch)?;
    let mut arrives = vec![0u32; nodes.len()];
    let mut makes = vec![0u32; nodes.len()];
    for &at in &order {
        // Every cable that ends at the node comes from a node that is earlier in the order, so what arrives is known.
        makes[at] = arrives[at].saturating_add(latency(nodes[at].0));
        for &cable_at in &leaving[at] {
            let to = to_at[cable_at];
            arrives[to] = arrives[to].max(makes[at]);
        }
    }
    let delays = (0..patch.cables().len()).map(|cable_at| arrives[to_at[cable_at]] - makes[from_at[cable_at]]).collect();
    Ok(Alignment { delays, nodes: nodes.iter().enumerate().map(|(at, (id, _))| (*id, arrives[at], makes[at])).collect() })
}

/// The patch with the delays of `alignment` in it: each cable that is to be delayed is two, to a node that delays what it is given and from it. The new nodes are put in after the
/// others, have the rank of the node that the cable starts at (so that what is added at an input is added in the order it was), a `single` input and an output of the cable's width, and
/// are named by `id_of`, which is given the number of the delay (0 for the first, by the order of the cables), the cable and the frames. The cables go in the order they were in, a
/// delayed one as its two. A patch that has no delays is made again as it was.
pub fn with_delays<I: Id>(patch: &Patch<I>, alignment: &Alignment<I>, mut id_of: impl FnMut(usize, &Cable<I>, u32) -> I) -> Patch<I> {
    assert_eq!(alignment.delays().len(), patch.cables().len(), "the alignment is of another patch");
    let spec_of: HashMap<I, &NodeSpec> = patch.nodes().iter().map(|(id, spec)| (*id, spec)).collect();
    let mut made = Patch::new();
    for (id, spec) in patch.nodes() {
        made.add_node(*id, spec.clone());
    }
    let mut number = 0;
    for (cable, &frames) in patch.cables().iter().zip(alignment.delays()) {
        if frames == 0 {
            made.connect(cable.from, cable.to);
            continue;
        }
        let start = spec_of[&cable.from.node];
        let width = start.outputs[usize::from(cable.from.port)].channels;
        let delay = id_of(number, cable, frames);
        number += 1;
        made.add_node(delay, NodeSpec::new(vec![PortSpec::single(width)], vec![PortSpec::audio(width)], start.rank));
        made.connect(cable.from, crate::patch::Port::new(delay, 0));
        made.connect(crate::patch::Port::new(delay, 0), cable.to);
    }
    made
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use anyhow::Result;

    use super::*;
    use crate::compile::compile;
    use crate::delay::DelayLine;
    use crate::exec::{Io, Node, Nodes, Pool};
    use crate::patch::{Port, PortSpec};
    use crate::testutil::Rng;

    const MONO: PortSpec = PortSpec::audio(1);

    /// A node with `inputs` and `outputs` mono ports, of a rank.
    fn spec(inputs: usize, outputs: usize, rank: u32) -> NodeSpec {
        NodeSpec::new(vec![MONO; inputs], vec![MONO; outputs], rank)
    }

    fn cable(patch: &mut Patch<u32>, from: (u32, u16), to: (u32, u16)) {
        patch.connect(Port::new(from.0, from.1), Port::new(to.0, to.1));
    }

    fn latencies(list: &[(u32, u32)]) -> impl Fn(u32) -> u32 {
        let map: HashMap<u32, u32> = list.iter().copied().collect();
        move |id| map.get(&id).copied().unwrap_or(0)
    }

    // ---- what each cable is delayed by -----------------------------------------------------------------------------------------------------------------------

    #[test]
    fn a_patch_with_no_latency_has_nothing_to_delay_and_every_node_is_on_time() {
        let mut patch = Patch::new();
        for (id, ins, outs) in [(1, 0, 1), (2, 1, 1), (3, 2, 0)] {
            patch.add_node(id, spec(ins, outs, 0));
        }
        cable(&mut patch, (1, 0), (2, 0));
        cable(&mut patch, (1, 0), (3, 0));
        cable(&mut patch, (2, 0), (3, 1));
        let found = align(&patch, |_| 0).unwrap();
        assert_eq!(found.delays(), [0, 0, 0]);
        assert!(!found.is_needed());
        assert_eq!((found.node(1), found.node(2), found.node(3), found.latency()), (Some((0, 0)), Some((0, 0)), Some((0, 0)), 0));
    }

    #[test]
    fn a_chain_adds_the_latencies_of_its_nodes_and_delays_nothing_since_it_has_one_cable_at_each_input() {
        let mut patch = Patch::new();
        for (id, ins, outs) in [(1, 0, 1), (2, 1, 1), (3, 1, 1), (4, 1, 0)] {
            patch.add_node(id, spec(ins, outs, 0));
        }
        cable(&mut patch, (1, 0), (2, 0));
        cable(&mut patch, (2, 0), (3, 0));
        cable(&mut patch, (3, 0), (4, 0));
        let found = align(&patch, latencies(&[(2, 23), (3, 5), (4, 7)])).unwrap();
        assert_eq!(found.delays(), [0, 0, 0]);
        assert_eq!((found.node(1), found.node(2), found.node(3), found.node(4)), (Some((0, 0)), Some((0, 23)), Some((23, 28)), Some((28, 35))));
        assert_eq!(found.latency(), 35);
    }

    #[test]
    fn the_path_with_the_least_latency_is_delayed_by_the_difference_where_two_paths_are_added() {
        // 1 feeds 3 straight and through 2, which has 23 frames of latency; 3 adds them.
        let mut patch = Patch::new();
        for (id, ins, outs) in [(1, 0, 1), (2, 1, 1), (3, 2, 0)] {
            patch.add_node(id, spec(ins, outs, 0));
        }
        cable(&mut patch, (1, 0), (3, 0)); // cable 0: the dry path
        cable(&mut patch, (1, 0), (2, 0)); // cable 1
        cable(&mut patch, (2, 0), (3, 1)); // cable 2: the path through the plugin
        let found = align(&patch, latencies(&[(2, 23)])).unwrap();
        assert_eq!(found.delays(), [23, 0, 0]);
        assert!(found.is_needed());
        assert_eq!(found.node(3), Some((23, 23)));
        assert_eq!(found.latency(), 23);
    }

    #[test]
    fn a_difference_of_one_frame_is_a_delay() {
        let mut patch = Patch::new();
        for (id, ins, outs) in [(1, 0, 1), (2, 1, 1), (3, 2, 0)] {
            patch.add_node(id, spec(ins, outs, 0));
        }
        cable(&mut patch, (1, 0), (3, 0));
        cable(&mut patch, (1, 0), (2, 0));
        cable(&mut patch, (2, 0), (3, 1));
        let found = align(&patch, latencies(&[(2, 1)])).unwrap();
        assert_eq!(found.delays(), [1, 0, 0]);
        assert!(found.is_needed());
    }

    #[test]
    fn of_three_paths_the_latest_is_not_delayed_and_the_others_are_by_what_they_lack() {
        let mut patch = Patch::new();
        for (id, ins, outs) in [(1, 0, 1), (2, 0, 1), (3, 0, 1), (4, 3, 0)] {
            patch.add_node(id, spec(ins, outs, 0));
        }
        cable(&mut patch, (1, 0), (4, 0));
        cable(&mut patch, (2, 0), (4, 1));
        cable(&mut patch, (3, 0), (4, 2));
        let found = align(&patch, latencies(&[(1, 5), (2, 40), (3, 12)])).unwrap();
        assert_eq!(found.delays(), [35, 0, 28]);
        assert_eq!(found.node(4), Some((40, 40)));
    }

    #[test]
    fn every_input_port_of_a_node_is_in_step_with_the_others_so_that_a_sidechain_is_too() {
        let mut patch = Patch::new();
        patch.add_node(1, spec(0, 1, 0)); // main
        patch.add_node(2, spec(0, 1, 0)); // sidechain, 10 frames late
        patch.add_node(3, spec(2, 1, 0)); // a compressor
        cable(&mut patch, (1, 0), (3, 0));
        cable(&mut patch, (2, 0), (3, 1));
        let found = align(&patch, latencies(&[(2, 10), (3, 4)])).unwrap();
        assert_eq!(found.delays(), [10, 0]);
        assert_eq!(found.node(3), Some((10, 14)));
    }

    #[test]
    fn two_cables_between_the_same_ports_have_the_same_delay() {
        let mut patch = Patch::new();
        patch.add_node(1, spec(0, 1, 0));
        patch.add_node(2, spec(0, 1, 0));
        patch.add_node(3, spec(1, 0, 0));
        cable(&mut patch, (1, 0), (3, 0));
        cable(&mut patch, (1, 0), (3, 0));
        cable(&mut patch, (2, 0), (3, 0));
        let found = align(&patch, latencies(&[(2, 9)])).unwrap();
        assert_eq!(found.delays(), [9, 9, 0]);
    }

    #[test]
    fn a_node_that_nothing_arrives_at_is_on_time_and_a_source_that_has_latency_makes_late() {
        let mut patch = Patch::new();
        patch.add_node(1, spec(0, 1, 0));
        patch.add_node(2, spec(1, 1, 0)); // no cable at its input
        let found = align(&patch, latencies(&[(1, 6), (2, 3)])).unwrap();
        assert_eq!((found.node(1), found.node(2)), (Some((0, 6)), Some((0, 3))));
        assert_eq!(found.latency(), 6);
        assert_eq!(found.node(99), None);
    }

    #[test]
    fn the_latency_of_the_patch_is_the_longest_path_and_not_the_sum_of_every_node() {
        // Two branches in parallel, 20 and 30 frames, and a node after them with 1.
        let mut patch = Patch::new();
        for (id, ins, outs) in [(1, 0, 2), (2, 1, 1), (3, 1, 1), (4, 2, 1)] {
            patch.add_node(id, spec(ins, outs, 0));
        }
        cable(&mut patch, (1, 0), (2, 0));
        cable(&mut patch, (1, 1), (3, 0));
        cable(&mut patch, (2, 0), (4, 0));
        cable(&mut patch, (3, 0), (4, 1));
        let found = align(&patch, latencies(&[(2, 20), (3, 30), (4, 1)])).unwrap();
        assert_eq!(found.latency(), 31);
        assert_eq!(found.delays(), [0, 0, 10, 0]);
    }

    #[test]
    fn the_numbers_are_the_graphs_and_not_the_order_the_nodes_were_put_in_or_their_ranks() {
        let build = |order: [u32; 4], ranks: [u32; 4]| {
            let mut patch = Patch::new();
            for &id in &order {
                let (ins, outs) = [(0, 1), (1, 1), (2, 1), (1, 0)][id as usize - 1];
                patch.add_node(id, spec(ins, outs, ranks[id as usize - 1]));
            }
            cable(&mut patch, (1, 0), (2, 0));
            cable(&mut patch, (1, 0), (3, 0));
            cable(&mut patch, (2, 0), (3, 1));
            cable(&mut patch, (3, 0), (4, 0));
            patch
        };
        let (a, b) = (build([1, 2, 3, 4], [0, 0, 0, 0]), build([4, 3, 2, 1], [3, 1, 2, 0]));
        let (fa, fb) = (align(&a, latencies(&[(2, 7), (3, 2)])).unwrap(), align(&b, latencies(&[(2, 7), (3, 2)])).unwrap());
        assert_eq!(fa.delays(), fb.delays());
        assert!((1..=4).all(|id| fa.node(id) == fb.node(id)));
        assert_eq!(fa.delays(), [0, 7, 0, 0], "the cable from 1 to 3, which 2 (7 frames late) is added to");
    }

    #[test]
    fn a_latency_that_is_as_big_as_a_number_can_be_does_not_overflow() {
        let mut patch = Patch::new();
        for (id, ins, outs) in [(1, 0, 1), (2, 1, 1), (3, 1, 0)] {
            patch.add_node(id, spec(ins, outs, 0));
        }
        cable(&mut patch, (1, 0), (2, 0));
        cable(&mut patch, (2, 0), (3, 0));
        let found = align(&patch, latencies(&[(1, u32::MAX), (2, u32::MAX), (3, 5)])).unwrap();
        assert_eq!(found.latency(), u32::MAX);
        assert_eq!(found.delays(), [0, 0]);
        // (A sum that wrapped would make node 2 and node 3 early, and no node as late as the one before it.)
        assert_eq!((found.node(1), found.node(2), found.node(3)), (Some((0, u32::MAX)), Some((u32::MAX, u32::MAX)), Some((u32::MAX, u32::MAX))));
    }

    #[test]
    fn a_patch_that_compile_refuses_is_refused_for_the_same_reason() {
        let mut cycle = Patch::new();
        cycle.add_node(1, spec(1, 1, 0));
        cycle.add_node(2, spec(1, 1, 0));
        cable(&mut cycle, (1, 0), (2, 0));
        cable(&mut cycle, (2, 0), (1, 0));
        assert_eq!(align(&cycle, |_| 0).unwrap_err(), compile(&cycle).unwrap_err());
        let mut lost = Patch::new();
        lost.add_node(1, spec(0, 1, 0));
        cable(&mut lost, (1, 0), (9, 0));
        assert_eq!(align(&lost, |_| 0).unwrap_err(), CompileError::UnknownNode { cable: 0, node: 9 });
        let mut twice = Patch::new();
        twice.add_node(1, spec(0, 1, 0));
        twice.add_node(1, spec(0, 1, 0));
        assert_eq!(align(&twice, |_| 0).unwrap_err(), CompileError::DuplicateNode(1));
    }

    // ---- the patch with the delays in it ---------------------------------------------------------------------------------------------------------------------

    /// The dry path past a plugin of 23 frames, as `the_path_with_the_least…` has it, with the ranks 4 (node 1) and 9 (node 2).
    fn dry_and_wet() -> Patch<u32> {
        let mut patch = Patch::new();
        patch.add_node(1, spec(0, 1, 4));
        patch.add_node(2, spec(1, 1, 9));
        patch.add_node(3, spec(2, 0, 20));
        cable(&mut patch, (1, 0), (3, 0));
        cable(&mut patch, (1, 0), (2, 0));
        cable(&mut patch, (2, 0), (3, 1));
        patch
    }

    #[test]
    fn a_cable_that_is_delayed_is_two_through_a_node_that_has_the_rank_of_where_it_starts() {
        let patch = dry_and_wet();
        let found = align(&patch, latencies(&[(2, 23)])).unwrap();
        let mut asked = Vec::new();
        let made = with_delays(&patch, &found, |number, cable, frames| {
            asked.push((number, cable.from, cable.to, frames));
            100 + number as u32
        });
        assert_eq!(asked, [(0, Port::new(1, 0), Port::new(3, 0), 23)]);
        assert_eq!(made.nodes().len(), 4);
        assert_eq!(made.nodes()[3], (100, NodeSpec::new(vec![PortSpec::single(1)], vec![MONO], 4)), "the rank of the node the cable starts at, a single input, an output as wide as the cable");
        let cables: Vec<_> = made.cables().iter().map(|c| ((c.from.node, c.from.port), (c.to.node, c.to.port))).collect();
        assert_eq!(cables, [((1, 0), (100, 0)), ((100, 0), (3, 0)), ((1, 0), (2, 0)), ((2, 0), (3, 1))], "in the order of the cables, a delayed one as its two");
        assert!(compile(&made).is_ok());
    }

    #[test]
    fn the_nodes_that_come_in_are_numbered_by_the_order_of_the_cables_and_are_as_wide_as_the_output_their_cables_start_at() {
        // 4 has a stereo input that 1 (stereo, straight) and 2 (stereo, 8 frames late) are added at, and a mono one that the second output of 3 (a stereo one and a mono one) is added at:
        // of the two delays that are asked for, in the order of the cables, the first is as wide as the stereo cable and the second as the mono one.
        let mut patch = Patch::new();
        patch.add_node(1, NodeSpec::new(vec![], vec![PortSpec::audio(2)], 0));
        patch.add_node(2, NodeSpec::new(vec![PortSpec::audio(2)], vec![PortSpec::audio(2)], 0));
        patch.add_node(3, NodeSpec::new(vec![], vec![PortSpec::audio(2), PortSpec::audio(1)], 0));
        patch.add_node(4, NodeSpec::new(vec![PortSpec::audio(2), PortSpec::audio(1)], vec![], 0));
        patch.connect(Port::new(1, 0), Port::new(4, 0)); // 0: the stereo dry cable, 8 frames short
        patch.connect(Port::new(1, 0), Port::new(2, 0)); // 1
        patch.connect(Port::new(2, 0), Port::new(4, 0)); // 2
        patch.connect(Port::new(3, 1), Port::new(4, 1)); // 3: mono, and 8 frames short of the stereo input (every input of a node is in step)
        let found = align(&patch, latencies(&[(2, 8)])).unwrap();
        assert_eq!(found.delays(), [8, 0, 0, 8]);
        let mut asked = Vec::new();
        let made = with_delays(&patch, &found, |number, cable, frames| {
            asked.push((number, cable.from.node, frames));
            50 + number as u32
        });
        assert_eq!(asked, [(0, 1, 8), (1, 3, 8)]);
        let width_of = |id: u32| made.nodes().iter().find(|(node, _)| *node == id).map(|(_, spec)| (spec.inputs[0].channels, spec.outputs[0].channels, spec.inputs[0].single));
        assert_eq!((width_of(50), width_of(51)), (Some((2, 2, true)), Some((1, 1, true))));
        assert!(compile(&made).is_ok());
    }

    #[test]
    fn a_patch_that_has_no_delays_is_made_again_as_it_was() {
        let patch = dry_and_wet();
        let found = align(&patch, |_| 0).unwrap();
        assert_eq!(with_delays(&patch, &found, |_, _, _| unreachable!("no delay is asked for")), patch);
    }

    #[test]
    #[should_panic(expected = "another patch")]
    fn an_alignment_of_another_patch_is_not_used() {
        let patch = dry_and_wet();
        let found = align(&patch, latencies(&[(2, 23)])).unwrap();
        let mut other = Patch::new();
        other.add_node(1, spec(0, 0, 0));
        let _ = with_delays(&other, &found, |_, _, _| 0);
    }

    // ---- what the executor does with them: impulses --------------------------------------------------------------------------------------------------------

    /// What the simulation's nodes are: a node that has latency puts what it is given out again late, by a line of its own; a source (a node with no input ports) makes an impulse at
    /// the first frame of the first block; and a node that was put in by `with_delays` is a line.
    enum Sim {
        Node { line: DelayLine, source: bool, seen: Vec<(u64, f32)> },
        Delay(DelayLine),
    }

    impl Node<u64> for Sim {
        fn process(&mut self, io: &mut Io, frame: &mut u64) -> Result<()> {
            let frames = io.frames();
            match self {
                Sim::Delay(line) => {
                    io.forward(0, 0);
                    line.run(&mut io.outputs_mut()[0]);
                }
                Sim::Node { line, source, seen } => {
                    let mut signal: Vec<f32> = vec![0.0; frames];
                    for input in io.inputs() {
                        for (sum, x) in signal.iter_mut().zip(&input[0]) {
                            *sum += *x;
                        }
                    }
                    seen.extend(signal.iter().enumerate().filter(|(_, x)| **x != 0.0).map(|(i, x)| (*frame + i as u64, *x)));
                    if *source && *frame == 0 {
                        signal[0] += 1.0;
                    }
                    let mut block = vec![signal];
                    line.run(&mut block);
                    for output in io.outputs_mut() {
                        output[0].copy_from_slice(&block[0]);
                    }
                }
            }
            Ok(())
        }
    }

    struct SimStore(HashMap<u32, Sim>);

    impl Nodes<u32, u64> for SimStore {
        fn process(&mut self, id: u32, io: &mut Io, frame: &mut u64) -> Result<()> {
            self.0.get_mut(&id).expect("a node of the simulation").process(io, frame)
        }
    }

    /// A random mono patch: 3 to 12 nodes (a quarter of them sources, with no input ports), a cable from a node to a later one in a hidden order now and then (some twice), and for
    /// every node a latency of 0 to 40 frames, 0 for about a third.
    fn simulated(seed: u64) -> (Patch<u32>, HashMap<u32, u32>) {
        let mut rng = Rng::new(seed ^ 0x1A7E);
        let n = 3 + rng.below(10);
        let mut patch = Patch::new();
        let mut kinds = Vec::new();
        for id in 0..n {
            let inputs = if rng.chance(0.25) { 0 } else { 1 + rng.below(2) };
            let outputs = 1 + rng.below(2);
            kinds.push((inputs, outputs));
            patch.add_node(id as u32, spec(inputs, outputs, rng.below(3) as u32));
        }
        for a in 0..n {
            for b in a + 1..n {
                if kinds[b].0 > 0 && rng.chance(0.35) {
                    let (from, to) = ((a as u32, rng.below(kinds[a].1) as u16), (b as u32, rng.below(kinds[b].0) as u16));
                    cable(&mut patch, from, to);
                    if rng.chance(0.1) {
                        cable(&mut patch, from, to);
                    }
                }
            }
        }
        let latency = (0..n).map(|id| (id as u32, if rng.chance(0.35) { 0 } else { rng.below(41) as u32 })).collect();
        (patch, latency)
    }

    /// When the impulses arrive at the input of each node that has input ports, and how many there are, worked out by recursion over the cables and not by `align`. What arrives at a
    /// node is as late as the latest of the cables that end at it (each as late as the node it starts at is, and that node's own latency: a source makes its impulse at its latency),
    /// whether or not that cable carries an impulse, and each cable carries as many impulses as the node it starts at makes (a source one, any other what arrives at it).
    fn expected(patch: &Patch<u32>, latency: &HashMap<u32, u32>) -> HashMap<u32, (u32, f32)> {
        fn into(node: u32, patch: &Patch<u32>, latency: &HashMap<u32, u32>, memo: &mut HashMap<u32, (u32, f32)>) -> (u32, f32) {
            if let Some(&known) = memo.get(&node) {
                return known;
            }
            let (mut late, mut count) = (0u32, 0.0f32);
            for c in patch.cables().iter().filter(|c| c.to.node == node) {
                let from = c.from.node;
                let source = patch.nodes().iter().find(|(id, _)| *id == from).unwrap().1.inputs.is_empty();
                let (from_late, from_count) = if source { (0, 1.0) } else { into(from, patch, latency, memo) };
                late = late.max(from_late + latency[&from]);
                count += from_count;
            }
            memo.insert(node, (late, count));
            (late, count)
        }
        let mut memo = HashMap::new();
        patch.nodes().iter().filter(|(_, spec)| !spec.inputs.is_empty()).map(|(id, _)| (*id, into(*id, patch, latency, &mut memo))).collect()
    }

    /// Run a patch (which has the delays, if it is to) over `total` frames in blocks of `block`, and say, for each node that has input ports, the frames at which something arrived
    /// at it and how much.
    fn arrivals(patch: &Patch<u32>, latency: &HashMap<u32, u32>, delays: &HashMap<u32, u32>, block: usize, total: u64) -> HashMap<u32, Vec<(u64, f32)>> {
        let plan = compile(patch).expect("the patch compiles");
        let mut nodes = HashMap::new();
        for (id, spec) in patch.nodes() {
            nodes.insert(
                *id,
                match delays.get(id) {
                    Some(&frames) => Sim::Delay(DelayLine::new(frames as usize, 1)),
                    None => Sim::Node { line: DelayLine::new(latency[id] as usize, 1), source: spec.inputs.is_empty(), seen: Vec::new() },
                },
            );
        }
        let mut store = SimStore(nodes);
        let mut pool = Pool::new();
        pool.reserve(block);
        pool.fit(&plan);
        let mut frame = 0u64;
        while frame < total {
            plan.run(&mut pool, block, &mut store, &mut frame).expect("the block runs");
            frame += block as u64;
        }
        store.0.into_iter().filter_map(|(id, node)| match node {
            Sim::Node { seen, .. } if patch.nodes().iter().any(|(n, s)| *n == id && !s.inputs.is_empty()) => Some((id, seen)),
            _ => None,
        }).collect()
    }

    #[test]
    fn once_the_delays_are_in_every_impulse_that_arrives_at_a_node_arrives_together_at_the_time_the_longest_path_says() {
        let mut compensated_patches = 0;
        for seed in 0..300 {
            let (patch, latency) = simulated(seed);
            let found = align(&patch, |id| latency[&id]).unwrap();
            let mut delays = HashMap::new();
            let made = with_delays(&patch, &found, |number, _, frames| {
                let id = 1000 + number as u32;
                delays.insert(id, frames);
                id
            });
            let want = expected(&patch, &latency);
            for block in [1usize, 7, 64, 1000] {
                let got = arrivals(&made, &latency, &delays, block, 1_400);
                for (node, (late, count)) in &want {
                    let seen = &got[node];
                    if *count == 0.0 {
                        assert!(seen.is_empty(), "seed {seed}, block {block}: node {node} is not reached by an impulse, and something arrived: {seen:?}");
                    } else {
                        assert_eq!(seen, &[(u64::from(*late), *count)], "seed {seed}, block {block}: node {node}");
                    }
                    assert_eq!(found.node(*node).map(|(arrives, _)| arrives), Some(*late), "seed {seed}: what align says arrives at node {node} is the latest path");
                }
            }
            compensated_patches += usize::from(found.is_needed());
        }
        assert!(compensated_patches > 150, "the patches drawn need compensation: {compensated_patches} of 300");
    }

    #[test]
    fn without_the_delays_the_impulses_of_the_same_patches_arrive_apart() {
        // The control of the test above: the same patches, run as they are drawn, have nodes that impulses arrive at at more than one time, and `align` says that such a patch
        // needs delays.
        let mut apart = 0;
        for seed in 0..300 {
            let (patch, latency) = simulated(seed);
            let got = arrivals(&patch, &latency, &HashMap::new(), 64, 1_400);
            if got.values().any(|seen| seen.iter().map(|(frame, _)| *frame).collect::<HashSet<_>>().len() > 1) {
                apart += 1;
                assert!(align(&patch, |id| latency[&id]).unwrap().is_needed(), "seed {seed}: impulses arrive apart, and align finds nothing to delay");
            }
        }
        assert!(apart > 60, "{apart} of 300");
    }

    #[test]
    fn the_latency_of_the_patch_is_the_frame_that_the_latest_impulse_arrives_at_a_node_that_ends_it() {
        let mut checked = 0;
        for seed in 0..300 {
            let (patch, latency) = simulated(seed);
            let found = align(&patch, |id| latency[&id]).unwrap();
            let last = (0..patch.nodes().len() as u32).map(|id| found.node(id).unwrap().1).max().unwrap();
            assert_eq!(found.latency(), last, "seed {seed}");
            checked += 1;
        }
        assert_eq!(checked, 300);
    }
}
