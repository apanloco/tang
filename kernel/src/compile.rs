//! `compile`: from a patch to a plan.
//!
//! **The order** is the topological order that takes, at every step, the node with the least `(rank, insertion)` of those that are ready. It is a function of the patch and
//! of nothing else (not of a hash, not of an address), so a patch is compiled to the same plan every time.
//!
//! **The buffers.** Every node output is a value that lives for the op that makes it: the plan feeds it into the inputs it has cables to at once, after the op. Every input
//! is a value that is a sum: it begins at +0.0 and the feeds are added to it in plan order, from the first feed to the op of the node (so a node with one cable has a sum
//! of one, and `0.0 + -0.0` is `+0.0`, as the engine's mix is). Each value lives from where it begins to the op that reads it, and values that are not alive together share a
//! buffer: the least number of buffers that fits, the lowest free one first. A buffer is zeroed where a value begins in it.
//!
//! **A single input** (`PortSpec::single`) that has one cable, which is the only cable that leaves its output, is not a sum: it is the output's own buffer. The output then lives
//! to the op that reads it and the cable has no feed, so that nothing is zeroed, added or copied and the bits are the output's own (a negative zero stays one), which is what
//! the stages of a chain want.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use crate::patch::{Id, Patch};

/// What is wrong with a patch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompileError<I> {
    /// Two nodes have one id.
    DuplicateNode(I),
    /// The cable (by its place in the patch) starts or ends at a node that is not in it.
    UnknownNode { cable: usize, node: I },
    /// The cable starts at an output or ends at an input that the node has not.
    NoSuchPort { cable: usize, node: I, port: u16, input: bool },
    /// The two ends of the cable carry different numbers of channels.
    ChannelMismatch { cable: usize, from: u16, to: u16 },
    /// Cables that go round: the nodes that could not be put in an order, which are those in a cycle and those that wait for one, in the order they were put in.
    Cycle(Vec<I>),
}

impl<I: std::fmt::Debug> std::fmt::Display for CompileError<I> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompileError::DuplicateNode(id) => write!(f, "two nodes are called {id:?}"),
            CompileError::UnknownNode { cable, node } => write!(f, "cable {cable} is at {node:?}, which is not in the patch"),
            CompileError::NoSuchPort { cable, node, port, input } => {
                write!(f, "cable {cable} is at {} {port} of {node:?}, which has none", if *input { "input" } else { "output" })
            }
            CompileError::ChannelMismatch { cable, from, to } => write!(f, "cable {cable} joins {from} channels to {to}"),
            CompileError::Cycle(nodes) => write!(f, "the cables go round: {nodes:?} cannot be put in an order"),
        }
    }
}

impl<I: std::fmt::Debug> std::error::Error for CompileError<I> {}

/// Add what an output has, once the op that made it is done, to the sum that an input of a later node is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Feed {
    /// The output port of the op's node.
    pub output: u32,
    /// The buffer of the sum.
    pub into: u32,
}

/// One node's turn in a block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Op<I> {
    pub node: I,
    /// The buffer of the sum at each input port of the node.
    pub inputs: Vec<u32>,
    /// The buffer of each output port.
    pub outputs: Vec<u32>,
    /// The buffers to zero before the op: those where a value begins.
    pub zero: Vec<u32>,
    /// After the op, in the order of the cables.
    pub feeds: Vec<Feed>,
}

/// What `compile` makes: immutable, and nothing in it allocates when it runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan<I> {
    pub(crate) ops: Vec<Op<I>>,
    /// The number of channels of each buffer.
    pub(crate) buffers: Vec<u16>,
}

impl<I: Id> Plan<I> {
    pub fn ops(&self) -> &[Op<I>] {
        &self.ops
    }

    /// The channels of each buffer, by buffer.
    pub fn buffers(&self) -> &[u16] {
        &self.buffers
    }

    /// The nodes, in the order they run.
    pub fn order(&self) -> impl Iterator<Item = I> + '_ {
        self.ops.iter().map(|op| op.node)
    }
}

/// A value that has a buffer: an output of an op, or the sum at an input of one.
struct Value {
    /// The first op it is alive at, and the last: where it is first written, and where it is read.
    start: usize,
    end: usize,
    channels: u16,
}

pub fn compile<I: Id>(patch: &Patch<I>) -> Result<Plan<I>, CompileError<I>> {
    let nodes = patch.nodes();
    let cables = patch.cables();

    // What is wrong with the patch, in the order of the nodes and then of the cables.
    let mut at_of: HashMap<I, usize> = HashMap::with_capacity(nodes.len());
    for (at, (id, _)) in nodes.iter().enumerate() {
        if at_of.insert(*id, at).is_some() {
            return Err(CompileError::DuplicateNode(*id));
        }
    }
    let mut from_at = Vec::with_capacity(cables.len());
    let mut to_at = Vec::with_capacity(cables.len());
    for (cable_at, cable) in cables.iter().enumerate() {
        let find = |node: I| at_of.get(&node).copied().ok_or(CompileError::UnknownNode { cable: cable_at, node });
        let (from, to) = (find(cable.from.node)?, find(cable.to.node)?);
        let out = nodes[from].1.outputs.get(usize::from(cable.from.port)).ok_or(CompileError::NoSuchPort { cable: cable_at, node: cable.from.node, port: cable.from.port, input: false })?;
        let inp = nodes[to].1.inputs.get(usize::from(cable.to.port)).ok_or(CompileError::NoSuchPort { cable: cable_at, node: cable.to.node, port: cable.to.port, input: true })?;
        if out.channels != inp.channels {
            return Err(CompileError::ChannelMismatch { cable: cable_at, from: out.channels, to: inp.channels });
        }
        from_at.push(from);
        to_at.push(to);
    }

    // The order: of the nodes that are ready, the least `(rank, insertion)` goes first.
    let mut waiting = vec![0usize; nodes.len()];
    let mut leaving: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    for cable_at in 0..cables.len() {
        waiting[to_at[cable_at]] += 1;
        leaving[from_at[cable_at]].push(cable_at);
    }
    let mut ready: BinaryHeap<Reverse<(u32, usize)>> = (0..nodes.len()).filter(|&at| waiting[at] == 0).map(|at| Reverse((nodes[at].1.rank, at))).collect();
    let mut order: Vec<usize> = Vec::with_capacity(nodes.len());
    while let Some(Reverse((_, at))) = ready.pop() {
        order.push(at);
        for &cable_at in &leaving[at] {
            let to = to_at[cable_at];
            waiting[to] -= 1;
            if waiting[to] == 0 {
                ready.push(Reverse((nodes[to].1.rank, to)));
            }
        }
    }
    if order.len() < nodes.len() {
        let mut placed = vec![false; nodes.len()];
        for &at in &order {
            placed[at] = true;
        }
        return Err(CompileError::Cycle(nodes.iter().enumerate().filter(|(at, _)| !placed[*at]).map(|(_, (id, _))| *id).collect()));
    }
    let mut place = vec![0usize; nodes.len()];
    for (op_at, &at) in order.iter().enumerate() {
        place[at] = op_at;
    }

    // The values, in the order of the ops: for each, the sums at its inputs and then its outputs. A sum begins at the first op that feeds it, or at its own op if none does.
    let mut values: Vec<Value> = Vec::new();
    let mut sum_of: Vec<Vec<usize>> = Vec::with_capacity(order.len());
    let mut output_of: Vec<Vec<usize>> = Vec::with_capacity(order.len());
    for (op_at, &at) in order.iter().enumerate() {
        let spec = &nodes[at].1;
        sum_of.push(
            spec.inputs
                .iter()
                .map(|port| {
                    values.push(Value { start: op_at, end: op_at, channels: port.channels });
                    values.len() - 1
                })
                .collect(),
        );
        output_of.push(
            spec.outputs
                .iter()
                .map(|port| {
                    values.push(Value { start: op_at, end: op_at, channels: port.channels });
                    values.len() - 1
                })
                .collect(),
        );
    }
    for cable_at in 0..cables.len() {
        let sum = sum_of[place[to_at[cable_at]]][usize::from(cables[cable_at].to.port)];
        values[sum].start = values[sum].start.min(place[from_at[cable_at]]);
    }

    // The cables that are an input itself: to an input that is `single` and has no other cable, from an output that has no other cable. The input is the output's value, which
    // then lives to the op that reads it.
    let mut into_count: HashMap<(usize, u16), usize> = HashMap::new();
    let mut out_count: HashMap<(usize, u16), usize> = HashMap::new();
    for cable_at in 0..cables.len() {
        *into_count.entry((to_at[cable_at], cables[cable_at].to.port)).or_default() += 1;
        *out_count.entry((from_at[cable_at], cables[cable_at].from.port)).or_default() += 1;
    }
    let is_alias: Vec<bool> = (0..cables.len())
        .map(|cable_at| {
            let cable = &cables[cable_at];
            nodes[to_at[cable_at]].1.inputs[usize::from(cable.to.port)].single && into_count[&(to_at[cable_at], cable.to.port)] == 1 && out_count[&(from_at[cable_at], cable.from.port)] == 1
        })
        .collect();
    let mut alias_of: Vec<Option<usize>> = vec![None; values.len()];
    for cable_at in (0..cables.len()).filter(|&cable_at| is_alias[cable_at]) {
        let sum = sum_of[place[to_at[cable_at]]][usize::from(cables[cable_at].to.port)];
        let output = output_of[place[from_at[cable_at]]][usize::from(cables[cable_at].from.port)];
        alias_of[sum] = Some(output);
        values[output].end = values[output].end.max(values[sum].end);
    }

    // The buffers: values in the order they begin (those that begin together in the order made above), each into the lowest buffer of its width that is free. An input that is an
    // output has the output's.
    let mut by_start: Vec<usize> = (0..values.len()).filter(|&value| alias_of[value].is_none()).collect();
    by_start.sort_by_key(|&value| values[value].start);
    let mut buffers: Vec<(u16, Option<usize>)> = Vec::new(); // channels, and the last op that the value in it is alive at
    let mut buffer_of = vec![0u32; values.len()];
    for &value in &by_start {
        let Value { start, end, channels } = values[value];
        let free = buffers.iter().position(|&(width, until)| width == channels && until.is_none_or(|until| until < start));
        let buffer = free.unwrap_or_else(|| {
            buffers.push((channels, None));
            buffers.len() - 1
        });
        buffers[buffer].1 = Some(end);
        buffer_of[value] = buffer as u32;
    }
    for value in 0..values.len() {
        if let Some(output) = alias_of[value] {
            buffer_of[value] = buffer_of[output];
        }
    }

    // The ops.
    let mut zero_at: Vec<Vec<u32>> = vec![Vec::new(); order.len()];
    for (value, v) in values.iter().enumerate() {
        if alias_of[value].is_none() {
            zero_at[v.start].push(buffer_of[value]);
        }
    }
    let mut ops = Vec::with_capacity(order.len());
    for (op_at, &at) in order.iter().enumerate() {
        let feeds = leaving[at]
            .iter()
            .filter(|&&cable_at| !is_alias[cable_at])
            .map(|&cable_at| Feed { output: u32::from(cables[cable_at].from.port), into: buffer_of[sum_of[place[to_at[cable_at]]][usize::from(cables[cable_at].to.port)]] })
            .collect();
        ops.push(Op {
            node: nodes[at].0,
            inputs: sum_of[op_at].iter().map(|&value| buffer_of[value]).collect(),
            outputs: output_of[op_at].iter().map(|&value| buffer_of[value]).collect(),
            zero: std::mem::take(&mut zero_at[op_at]),
            feeds,
        });
    }
    Ok(Plan { ops, buffers: buffers.iter().map(|&(channels, _)| channels).collect() })
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use super::*;
    use crate::patch::{NodeSpec, Port, PortSpec};
    use crate::testutil::{Rng, random_patch};

    const ST: PortSpec = PortSpec::audio(2);
    const SINGLE: PortSpec = PortSpec::single(2);

    /// A node with `inputs` and `outputs` stereo ports.
    fn spec(inputs: usize, outputs: usize, rank: u32) -> NodeSpec {
        NodeSpec::new(vec![ST; inputs], vec![ST; outputs], rank)
    }

    /// A node with `inputs` stereo inputs that are `single`, and `outputs` stereo outputs.
    fn spec_single(inputs: usize, outputs: usize, rank: u32) -> NodeSpec {
        NodeSpec::new(vec![SINGLE; inputs], vec![ST; outputs], rank)
    }

    fn cable(patch: &mut Patch<u32>, from: (u32, u16), to: (u32, u16)) {
        patch.connect(Port::new(from.0, from.1), Port::new(to.0, to.1));
    }

    fn order(patch: &Patch<u32>) -> Vec<u32> {
        compile(patch).unwrap().order().collect()
    }

    // ---- the order -------------------------------------------------------------------------------------------------------------------------------------------

    #[test]
    fn an_empty_patch_is_an_empty_plan() {
        let plan = compile(&Patch::<u32>::new()).unwrap();
        assert!(plan.ops().is_empty() && plan.buffers().is_empty());
    }

    #[test]
    fn a_chain_runs_from_its_source_to_its_sink_whatever_order_it_was_put_in() {
        let mut patch = Patch::new();
        for (id, ins, outs) in [(3, 1, 0), (1, 0, 1), (2, 1, 1)] {
            patch.add_node(id, spec(ins, outs, 0));
        }
        cable(&mut patch, (1, 0), (2, 0));
        cable(&mut patch, (2, 0), (3, 0));
        assert_eq!(order(&patch), [1, 2, 3]);
    }

    #[test]
    fn of_the_nodes_that_are_ready_the_one_of_the_least_rank_runs_first() {
        let mut patch = Patch::new();
        for (id, rank) in [(10, 3), (11, 1), (12, 2)] {
            patch.add_node(id, spec(0, 1, rank));
        }
        assert_eq!(order(&patch), [11, 12, 10]);
    }

    #[test]
    fn nodes_of_one_rank_run_in_the_order_they_were_put_in() {
        let mut patch = Patch::new();
        for id in [7, 3, 9, 1] {
            patch.add_node(id, spec(0, 1, 5));
        }
        assert_eq!(order(&patch), [7, 3, 9, 1]);
    }

    #[test]
    fn a_node_waits_for_the_nodes_it_has_a_cable_from_whatever_its_rank() {
        let mut patch = Patch::new();
        patch.add_node(1, spec(1, 0, 0));
        patch.add_node(2, spec(0, 1, 9));
        cable(&mut patch, (2, 0), (1, 0));
        assert_eq!(order(&patch), [2, 1]);
    }

    #[test]
    fn at_every_step_the_node_of_the_least_rank_of_those_that_are_ready_runs() {
        // X (rank 5) has a cable to A (rank 1); B (rank 2) has none. B is ready first, then X, and A is not ready until X has run.
        let mut patch = Patch::new();
        patch.add_node(100, spec(1, 0, 1)); // A
        patch.add_node(101, spec(0, 1, 5)); // X
        patch.add_node(102, spec(0, 0, 2)); // B
        cable(&mut patch, (101, 0), (100, 0));
        assert_eq!(order(&patch), [102, 101, 100]);
    }

    #[test]
    fn lanes_then_groups_then_the_master_is_the_order_that_ranks_by_position_make() {
        // The shape of today's session: lanes (ranks 0..4) feed their group or the master, groups (ranks 4 and 5) feed the master.
        let mut patch = Patch::new();
        let (master, group_a, group_b) = (900, 800, 801);
        patch.add_node(master, spec(1, 0, u32::MAX));
        patch.add_node(group_a, spec(1, 1, 4));
        patch.add_node(group_b, spec(1, 1, 5));
        for lane in 0..4u32 {
            patch.add_node(lane, spec(0, 1, lane));
        }
        cable(&mut patch, (0, 0), (group_a, 0));
        cable(&mut patch, (1, 0), (master, 0));
        cable(&mut patch, (2, 0), (group_b, 0));
        cable(&mut patch, (3, 0), (group_a, 0));
        cable(&mut patch, (group_a, 0), (master, 0));
        cable(&mut patch, (group_b, 0), (master, 0));
        assert_eq!(order(&patch), [0, 1, 2, 3, group_a, group_b, master]);
    }

    #[test]
    fn a_cycle_is_an_error_that_names_the_nodes_that_could_not_be_put_in_an_order() {
        // 1 feeds 2, 2 feeds 3, 3 feeds 2 (a cycle), 3 also feeds 4, which waits for it; 5 is free.
        let mut patch = Patch::new();
        for (id, ins, outs) in [(1, 0, 1), (2, 2, 1), (3, 1, 2), (4, 1, 0), (5, 0, 1)] {
            patch.add_node(id, spec(ins, outs, 0));
        }
        cable(&mut patch, (1, 0), (2, 0));
        cable(&mut patch, (2, 0), (3, 0));
        cable(&mut patch, (3, 0), (2, 1));
        cable(&mut patch, (3, 1), (4, 0));
        assert_eq!(compile(&patch), Err(CompileError::Cycle(vec![2, 3, 4])));
    }

    #[test]
    fn a_cable_from_a_node_to_itself_is_a_cycle() {
        let mut patch = Patch::new();
        patch.add_node(1, spec(1, 1, 0));
        cable(&mut patch, (1, 0), (1, 0));
        assert_eq!(compile(&patch), Err(CompileError::Cycle(vec![1])));
    }

    // ---- what is refused ---------------------------------------------------------------------------------------------------------------------------------

    #[test]
    fn two_nodes_of_one_id_are_refused() {
        let mut patch = Patch::new();
        patch.add_node(1, spec(0, 1, 0));
        patch.add_node(2, spec(1, 0, 0));
        patch.add_node(1, spec(1, 0, 0));
        assert_eq!(compile(&patch), Err(CompileError::DuplicateNode(1)));
    }

    #[test]
    fn a_cable_at_a_node_that_is_not_in_the_patch_is_refused_at_either_end() {
        let mut patch = Patch::new();
        patch.add_node(1, spec(1, 1, 0));
        cable(&mut patch, (1, 0), (7, 0));
        assert_eq!(compile(&patch), Err(CompileError::UnknownNode { cable: 0, node: 7 }));
        let mut patch = Patch::new();
        patch.add_node(1, spec(1, 1, 0));
        cable(&mut patch, (1, 0), (1, 0));
        cable(&mut patch, (8, 0), (1, 0));
        assert_eq!(compile(&patch), Err(CompileError::UnknownNode { cable: 1, node: 8 }));
    }

    #[test]
    fn a_cable_at_a_port_that_is_not_there_is_refused_and_says_whether_it_was_an_input_or_an_output() {
        let mut patch = Patch::new();
        patch.add_node(1, spec(0, 1, 0));
        patch.add_node(2, spec(1, 0, 0));
        cable(&mut patch, (1, 1), (2, 0));
        assert_eq!(compile(&patch), Err(CompileError::NoSuchPort { cable: 0, node: 1, port: 1, input: false }));
        let mut patch = Patch::new();
        patch.add_node(1, spec(0, 1, 0));
        patch.add_node(2, spec(1, 0, 0));
        cable(&mut patch, (1, 0), (2, 1));
        assert_eq!(compile(&patch), Err(CompileError::NoSuchPort { cable: 0, node: 2, port: 1, input: true }));
        // An input is not an output: node 2 has an input 0 and no output 0.
        let mut patch = Patch::new();
        patch.add_node(1, spec(0, 1, 0));
        patch.add_node(2, spec(1, 0, 0));
        cable(&mut patch, (2, 0), (1, 0));
        assert_eq!(compile(&patch), Err(CompileError::NoSuchPort { cable: 0, node: 2, port: 0, input: false }));
    }

    #[test]
    fn a_cable_between_ports_of_different_channels_is_refused() {
        let mut patch = Patch::new();
        patch.add_node(1, NodeSpec::new(vec![], vec![PortSpec::audio(1)], 0));
        patch.add_node(2, NodeSpec::new(vec![PortSpec::audio(2)], vec![], 0));
        cable(&mut patch, (1, 0), (2, 0));
        assert_eq!(compile(&patch), Err(CompileError::ChannelMismatch { cable: 0, from: 1, to: 2 }));
    }

    #[test]
    fn the_first_thing_that_is_wrong_in_the_order_of_the_cables_is_the_one_that_is_said() {
        let mut patch = Patch::new();
        patch.add_node(1, spec(1, 1, 0));
        cable(&mut patch, (1, 0), (1, 0)); // a cycle, which is found after the cables have been looked at
        cable(&mut patch, (1, 5), (1, 0));
        assert_eq!(compile(&patch), Err(CompileError::NoSuchPort { cable: 1, node: 1, port: 5, input: false }));
    }

    // ---- the plan ----------------------------------------------------------------------------------------------------------------------------------------

    #[test]
    fn an_op_has_a_buffer_for_each_port_of_its_node_and_a_feed_for_each_cable_that_leaves_it() {
        let mut patch = Patch::new();
        patch.add_node(1, spec(0, 2, 0));
        patch.add_node(2, spec(2, 1, 0));
        patch.add_node(3, spec(1, 0, 0));
        cable(&mut patch, (1, 0), (2, 0));
        cable(&mut patch, (1, 1), (2, 1));
        cable(&mut patch, (1, 1), (3, 0));
        cable(&mut patch, (2, 0), (3, 0));
        let plan = compile(&patch).unwrap();
        let shape: Vec<_> = plan.ops().iter().map(|op| (op.node, op.inputs.len(), op.outputs.len(), op.feeds.len())).collect();
        assert_eq!(shape, [(1, 0, 2, 3), (2, 2, 1, 1), (3, 1, 0, 0)]);
        assert!(plan.buffers().iter().all(|&channels| channels == 2));
    }

    #[test]
    fn the_feeds_of_an_op_are_in_the_order_the_cables_were_drawn() {
        // Two outputs of one node go to one input: the sum is (0 + b) + a if the cable from output 1 was drawn first, and the plan says so.
        let mut patch = Patch::new();
        patch.add_node(1, spec(0, 2, 0));
        patch.add_node(2, spec(1, 0, 0));
        cable(&mut patch, (1, 1), (2, 0));
        cable(&mut patch, (1, 0), (2, 0));
        let plan = compile(&patch).unwrap();
        let outputs: Vec<u32> = plan.ops()[0].feeds.iter().map(|feed| feed.output).collect();
        assert_eq!(outputs, [1, 0]);
        assert_eq!(plan.ops()[0].feeds[0].into, plan.ops()[0].feeds[1].into, "into one sum");
        assert_eq!(plan.ops()[0].feeds[0].into, plan.ops()[1].inputs[0]);
    }

    #[test]
    fn two_cables_between_the_same_ports_are_two_feeds() {
        let mut patch = Patch::new();
        patch.add_node(1, spec(0, 1, 0));
        patch.add_node(2, spec(1, 0, 0));
        cable(&mut patch, (1, 0), (2, 0));
        cable(&mut patch, (1, 0), (2, 0));
        assert_eq!(compile(&patch).unwrap().ops()[0].feeds.len(), 2);
    }

    #[test]
    fn an_output_with_cables_to_several_inputs_is_fed_to_each() {
        let mut patch = Patch::new();
        patch.add_node(1, spec(0, 1, 0));
        patch.add_node(2, spec(1, 0, 1));
        patch.add_node(3, spec(1, 0, 2));
        cable(&mut patch, (1, 0), (2, 0));
        cable(&mut patch, (1, 0), (3, 0));
        let plan = compile(&patch).unwrap();
        let feeds = &plan.ops()[0].feeds;
        assert_eq!(feeds.len(), 2);
        assert_eq!((feeds[0].into, feeds[1].into), (plan.ops()[1].inputs[0], plan.ops()[2].inputs[0]));
        assert_ne!(feeds[0].into, feeds[1].into);
    }

    #[test]
    fn a_chain_is_laid_out_in_three_buffers_the_lowest_free_one_first() {
        let mut patch = Patch::new();
        for (id, ins, outs) in [(1, 0, 1), (2, 1, 1), (3, 1, 1), (4, 1, 0)] {
            patch.add_node(id, spec(ins, outs, 0));
        }
        cable(&mut patch, (1, 0), (2, 0));
        cable(&mut patch, (2, 0), (3, 0));
        cable(&mut patch, (3, 0), (4, 0));
        let plan = compile(&patch).unwrap();
        let op = |node, inputs: &[u32], outputs: &[u32], zero: &[u32], feeds: &[(u32, u32)]| Op {
            node,
            inputs: inputs.to_vec(),
            outputs: outputs.to_vec(),
            zero: zero.to_vec(),
            feeds: feeds.iter().map(|&(output, into)| Feed { output, into }).collect(),
        };
        assert_eq!(
            plan.ops(),
            [op(1, &[], &[0], &[0, 1], &[(0, 1)]), op(2, &[1], &[0], &[0, 2], &[(0, 2)]), op(3, &[2], &[0], &[0, 1], &[(0, 1)]), op(4, &[1], &[], &[], &[])]
        );
        assert_eq!(plan.buffers(), [2, 2, 2]);
    }

    #[test]
    fn lanes_that_feed_the_master_share_the_buffer_that_they_put_their_output_in() {
        // Five lanes, each fed into the master as soon as it has run: one buffer for every lane's output, and one for the sum at the master.
        let mut patch = Patch::new();
        patch.add_node(100, spec(1, 0, u32::MAX));
        for lane in 0..5 {
            patch.add_node(lane, spec(0, 1, lane));
            cable(&mut patch, (lane, 0), (100, 0));
        }
        let plan = compile(&patch).unwrap();
        assert_eq!(plan.buffers().len(), 2);
        let lane_buffers: HashSet<u32> = plan.ops()[..5].iter().map(|op| op.outputs[0]).collect();
        assert_eq!(lane_buffers.len(), 1);
    }

    #[test]
    fn an_input_with_no_cable_has_a_buffer_that_is_zeroed_just_before_its_node() {
        let mut patch = Patch::new();
        patch.add_node(1, spec(0, 1, 0));
        patch.add_node(2, spec(1, 1, 1)); // fed by 1
        patch.add_node(3, spec(1, 0, 2)); // nothing is cabled to it
        cable(&mut patch, (1, 0), (2, 0));
        let plan = compile(&patch).unwrap();
        let op = &plan.ops()[2];
        assert_eq!(op.zero.iter().filter(|&&buffer| buffer == op.inputs[0]).count(), 1, "once, where its value begins, which is its own op");
    }

    #[test]
    fn a_sum_is_zeroed_before_the_first_op_that_feeds_it_and_not_before_the_op_that_reads_it() {
        let mut patch = Patch::new();
        patch.add_node(1, spec(0, 1, 0));
        patch.add_node(2, spec(0, 1, 1));
        patch.add_node(3, spec(1, 0, 2));
        cable(&mut patch, (1, 0), (3, 0));
        cable(&mut patch, (2, 0), (3, 0));
        let plan = compile(&patch).unwrap();
        let sum = plan.ops()[2].inputs[0];
        assert!(plan.ops()[0].zero.contains(&sum), "before the first feed");
        assert!(!plan.ops()[1].zero.contains(&sum), "and once");
        assert!(!plan.ops()[2].zero.contains(&sum));
        assert!(plan.ops()[0].zero.contains(&plan.ops()[0].outputs[0]), "an output is zeroed before its op");
    }

    // ---- single inputs -----------------------------------------------------------------------------------------------------------------------------------

    #[test]
    fn a_single_input_with_one_cable_is_the_buffer_of_the_output_that_the_cable_comes_from_and_is_neither_zeroed_nor_fed() {
        let mut patch = Patch::new();
        patch.add_node(1, spec(0, 1, 0));
        patch.add_node(2, spec_single(1, 1, 1));
        patch.add_node(3, spec(1, 0, 2));
        cable(&mut patch, (1, 0), (2, 0));
        cable(&mut patch, (2, 0), (3, 0));
        let plan = compile(&patch).unwrap();
        let ops = plan.ops();
        assert_eq!(ops[1].inputs[0], ops[0].outputs[0], "the one buffer");
        assert!(ops[0].feeds.is_empty(), "nothing is fed to it");
        let zeroed: Vec<u32> = ops.iter().flat_map(|op| op.zero.iter().copied()).collect();
        assert_eq!(zeroed.iter().filter(|&&buffer| buffer == ops[0].outputs[0]).count(), 1, "zeroed once, where the output begins");
        assert_eq!(ops[1].feeds.len(), 1, "the node after it has a sum, which is fed as ever");
    }

    #[test]
    fn a_chain_of_single_inputs_is_laid_out_in_two_buffers_and_the_sum_at_its_end() {
        let mut patch = Patch::new();
        patch.add_node(1, spec(0, 1, 0));
        patch.add_node(2, spec_single(1, 1, 0));
        patch.add_node(3, spec_single(1, 1, 0));
        patch.add_node(4, spec(1, 0, 0));
        cable(&mut patch, (1, 0), (2, 0));
        cable(&mut patch, (2, 0), (3, 0));
        cable(&mut patch, (3, 0), (4, 0));
        let plan = compile(&patch).unwrap();
        let op = |node, inputs: &[u32], outputs: &[u32], zero: &[u32], feeds: &[(u32, u32)]| Op {
            node,
            inputs: inputs.to_vec(),
            outputs: outputs.to_vec(),
            zero: zero.to_vec(),
            feeds: feeds.iter().map(|&(output, into)| Feed { output, into }).collect(),
        };
        // Each output lives to the node that reads it, so two of them are alive at any op, and the sum at the end is a third.
        assert_eq!(
            plan.ops(),
            [op(1, &[], &[0], &[0], &[]), op(2, &[0], &[1], &[1], &[]), op(3, &[1], &[0], &[0, 2], &[(0, 2)]), op(4, &[2], &[], &[], &[])]
        );
        assert_eq!(plan.buffers(), [2, 2, 2]);
    }

    #[test]
    fn a_single_input_with_two_cables_is_a_sum_like_any_other() {
        let mut patch = Patch::new();
        patch.add_node(1, spec(0, 1, 0));
        patch.add_node(2, spec(0, 1, 1));
        patch.add_node(3, spec_single(2, 0, 2));
        cable(&mut patch, (1, 0), (3, 0));
        cable(&mut patch, (2, 0), (3, 0));
        let plan = compile(&patch).unwrap();
        let ops = plan.ops();
        let sum = ops[2].inputs[0];
        assert_ne!(sum, ops[0].outputs[0]);
        assert_ne!(sum, ops[1].outputs[0]);
        assert_eq!((ops[0].feeds.len(), ops[1].feeds.len()), (1, 1), "each is fed to it");
        assert!(ops[0].zero.contains(&sum) && !ops[1].zero.contains(&sum), "zeroed before the first feed, and once");
    }

    #[test]
    fn a_single_input_with_no_cable_is_a_buffer_that_is_zeroed_before_its_node() {
        let mut patch = Patch::new();
        patch.add_node(1, spec(0, 1, 0));
        patch.add_node(2, spec_single(1, 0, 1));
        let plan = compile(&patch).unwrap();
        let op = &plan.ops()[1];
        assert_eq!(op.zero.iter().filter(|&&buffer| buffer == op.inputs[0]).count(), 1, "once, where its value begins, which is its own op");
    }

    #[test]
    fn a_single_input_whose_cable_leaves_an_output_that_has_other_cables_is_a_sum_of_one() {
        let mut patch = Patch::new();
        patch.add_node(1, spec(0, 1, 0));
        patch.add_node(2, spec_single(1, 0, 1));
        patch.add_node(3, spec(1, 0, 2));
        cable(&mut patch, (1, 0), (2, 0));
        cable(&mut patch, (1, 0), (3, 0));
        let plan = compile(&patch).unwrap();
        let ops = plan.ops();
        assert_ne!(ops[1].inputs[0], ops[0].outputs[0], "not the output's buffer: the other node reads it too, and this one may work in place");
        assert_eq!(ops[0].feeds.len(), 2, "both are fed");
    }

    #[test]
    fn two_single_inputs_are_not_the_one_buffer_of_the_output_that_both_have_a_cable_from() {
        let mut patch = Patch::new();
        patch.add_node(1, spec(0, 1, 0));
        patch.add_node(2, spec_single(1, 0, 1));
        patch.add_node(3, spec_single(1, 0, 2));
        cable(&mut patch, (1, 0), (2, 0));
        cable(&mut patch, (1, 0), (3, 0));
        let plan = compile(&patch).unwrap();
        let ops = plan.ops();
        assert_ne!(ops[1].inputs[0], ops[2].inputs[0]);
        assert!(ops[1].inputs[0] != ops[0].outputs[0] && ops[2].inputs[0] != ops[0].outputs[0]);
        assert_eq!(ops[0].feeds.len(), 2);
    }

    #[test]
    fn the_buffer_of_an_output_that_a_single_input_is_stays_taken_until_that_node_has_run() {
        // 1 goes to 2 (single), and the nodes 3 and 4 run between them (the ranks say so): the order is 1, 3, 4, 2, 5, and neither 3 nor 4 takes the buffer that 1 made its output in.
        let mut patch = Patch::new();
        patch.add_node(1, spec(0, 1, 0));
        patch.add_node(3, spec(0, 1, 1));
        patch.add_node(4, spec(1, 1, 2));
        patch.add_node(2, spec_single(1, 1, 3));
        patch.add_node(5, spec(2, 0, 4));
        cable(&mut patch, (1, 0), (2, 0));
        cable(&mut patch, (3, 0), (4, 0));
        cable(&mut patch, (4, 0), (5, 0));
        cable(&mut patch, (2, 0), (5, 1));
        let plan = compile(&patch).unwrap();
        assert_eq!(plan.order().collect::<Vec<_>>(), [1, 3, 4, 2, 5]);
        let held = plan.ops()[0].outputs[0];
        assert_eq!(plan.ops()[3].inputs[0], held, "node 2 reads it");
        for between in &plan.ops()[1..3] {
            assert!(!between.inputs.contains(&held) && !between.outputs.contains(&held), "node {} does not have it", between.node);
        }
    }

    // ---- properties over seeded random patches --------------------------------------------------------------------------------------------------------

    /// What lives where, worked out from the patch and the order of the plan alone (not from what the plan says): (start, end, channels) of every output and every sum,
    /// by the op that has it and the port. An input that is `single`, has one cable, and has the only cable that leaves its output is not a sum: it is the output, which then
    /// lives to the op that reads it (`alias` says which output an input is: by the op and the input, the op and the output).
    struct Lives {
        outputs: HashMap<(usize, usize), (usize, usize, u16)>,
        sums: HashMap<(usize, usize), (usize, usize, u16)>,
        alias: HashMap<(usize, usize), (usize, usize)>,
    }

    /// Whether each cable of the patch is the whole of an input that is `single` and of an output (see `Lives`).
    fn aliased(patch: &Patch<u32>) -> Vec<bool> {
        let single = |c: &crate::patch::Cable<u32>| patch.nodes().iter().find(|(id, _)| *id == c.to.node).unwrap().1.inputs[usize::from(c.to.port)].single;
        patch
            .cables()
            .iter()
            .map(|c| single(c) && patch.cables().iter().filter(|o| o.to == c.to).count() == 1 && patch.cables().iter().filter(|o| o.from == c.from).count() == 1)
            .collect()
    }

    fn lives(patch: &Patch<u32>, order: &[u32]) -> Lives {
        let place: HashMap<u32, usize> = order.iter().enumerate().map(|(at, id)| (*id, at)).collect();
        let spec_of: HashMap<u32, &NodeSpec> = patch.nodes().iter().map(|(id, spec)| (*id, spec)).collect();
        let aliased = aliased(patch);
        let mut outputs = HashMap::new();
        let mut sums = HashMap::new();
        let mut alias = HashMap::new();
        for (at, id) in order.iter().enumerate() {
            for (m, port) in spec_of[id].outputs.iter().enumerate() {
                outputs.insert((at, m), (at, at, port.channels));
            }
            for (k, port) in spec_of[id].inputs.iter().enumerate() {
                if let Some(cable) = patch.cables().iter().enumerate().find(|(c, cable)| aliased[*c] && cable.to.node == *id && usize::from(cable.to.port) == k).map(|(_, cable)| cable) {
                    alias.insert((at, k), (place[&cable.from.node], usize::from(cable.from.port)));
                    continue;
                }
                let first = patch.cables().iter().filter(|c| c.to.node == *id && usize::from(c.to.port) == k).map(|c| place[&c.from.node]).min().unwrap_or(at);
                sums.insert((at, k), (first, at, port.channels));
            }
        }
        for (&(reader, _), &(maker, m)) in &alias {
            outputs.get_mut(&(maker, m)).unwrap().1 = reader;
        }
        Lives { outputs, sums, alias }
    }

    #[test]
    fn the_order_of_a_random_patch_is_one_that_every_cable_agrees_with_and_that_takes_the_least_ready_node_every_time() {
        for seed in 0..300 {
            let patch = random_patch(seed);
            let order = order(&patch);
            assert_eq!(order.len(), patch.nodes().len(), "seed {seed}");
            let place: HashMap<u32, usize> = order.iter().enumerate().map(|(at, id)| (*id, at)).collect();
            for c in patch.cables() {
                assert!(place[&c.from.node] < place[&c.to.node], "seed {seed}: {c:?}");
            }
            // The least `(rank, insertion)` of the nodes that were ready (every cable into them from a node already run) was taken at each step.
            let insertion: HashMap<u32, usize> = patch.nodes().iter().enumerate().map(|(at, (id, _))| (*id, at)).collect();
            let rank: HashMap<u32, u32> = patch.nodes().iter().map(|(id, spec)| (*id, spec.rank)).collect();
            let mut done: HashSet<u32> = HashSet::new();
            for id in &order {
                let ready = |candidate: &u32| !done.contains(candidate) && patch.cables().iter().filter(|c| c.to.node == *candidate).all(|c| done.contains(&c.from.node));
                let least = patch.nodes().iter().map(|(candidate, _)| *candidate).filter(ready).min_by_key(|candidate| (rank[candidate], insertion[candidate])).unwrap();
                assert_eq!(*id, least, "seed {seed}");
                done.insert(*id);
            }
        }
    }

    #[test]
    fn a_patch_gives_the_same_plan_each_time_it_is_compiled_and_one_made_of_the_same_things_the_same() {
        for seed in 0..100 {
            let patch = random_patch(seed);
            assert_eq!(compile(&patch), compile(&patch), "seed {seed}");
            assert_eq!(compile(&patch), compile(&patch.clone()), "seed {seed}");
        }
    }

    #[test]
    fn nodes_that_are_given_other_ids_are_run_in_the_same_order_with_the_same_buffers() {
        for seed in 0..100 {
            let patch = random_patch(seed);
            let mut renamed = Patch::new();
            let new_id = |id: u32| id.wrapping_mul(2_654_435_761).wrapping_add(17);
            for (id, spec) in patch.nodes() {
                renamed.add_node(new_id(*id), spec.clone());
            }
            for c in patch.cables() {
                renamed.connect(Port::new(new_id(c.from.node), c.from.port), Port::new(new_id(c.to.node), c.to.port));
            }
            let (a, b) = (compile(&patch).unwrap(), compile(&renamed).unwrap());
            assert_eq!(a.order().map(new_id).collect::<Vec<_>>(), b.order().collect::<Vec<_>>(), "seed {seed}");
            let strip = |plan: &Plan<u32>| plan.ops().iter().map(|op| (op.inputs.clone(), op.outputs.clone(), op.zero.clone(), op.feeds.clone())).collect::<Vec<_>>();
            assert_eq!((strip(&a), a.buffers()), (strip(&b), b.buffers()), "seed {seed}");
        }
    }

    #[test]
    fn values_that_are_alive_together_never_share_a_buffer() {
        for seed in 0..300 {
            let patch = random_patch(seed);
            let plan = compile(&patch).unwrap();
            let order: Vec<u32> = plan.order().collect();
            let lives = lives(&patch, &order);
            // Every value, with the buffer the plan gives it.
            let mut values: Vec<(usize, usize, u16, u32, String)> = Vec::new();
            for (at, op) in plan.ops().iter().enumerate() {
                for (m, &buffer) in op.outputs.iter().enumerate() {
                    let (start, end, channels) = lives.outputs[&(at, m)];
                    values.push((start, end, channels, buffer, format!("output {m} of op {at}")));
                }
                for (k, &buffer) in op.inputs.iter().enumerate() {
                    if let Some(&(maker, m)) = lives.alias.get(&(at, k)) {
                        assert_eq!(buffer, plan.ops()[maker].outputs[m], "seed {seed}: input {k} of op {at} is output {m} of op {maker}");
                        continue;
                    }
                    let (start, end, channels) = lives.sums[&(at, k)];
                    values.push((start, end, channels, buffer, format!("sum {k} of op {at}")));
                }
            }
            for (i, a) in values.iter().enumerate() {
                assert_eq!(plan.buffers()[a.3 as usize], a.2, "seed {seed}: {}", a.4);
                for b in &values[i + 1..] {
                    if a.3 == b.3 {
                        assert!(a.1 < b.0 || b.1 < a.0, "seed {seed}: {} [{}, {}] and {} [{}, {}] share buffer {}", a.4, a.0, a.1, b.4, b.0, b.1, a.3);
                    }
                }
            }
        }
    }

    #[test]
    fn the_buffers_are_the_fewest_that_fit_of_each_number_of_channels() {
        for seed in 0..5000 {
            let patch = random_patch(seed);
            let plan = compile(&patch).unwrap();
            let order: Vec<u32> = plan.order().collect();
            let lives = lives(&patch, &order);
            let all: Vec<(usize, usize, u16)> = lives.outputs.values().chain(lives.sums.values()).copied().collect();
            for channels in [1u16, 2] {
                // The most values of this width alive at one op: no fewer buffers can hold them.
                let most = (0..order.len()).map(|at| all.iter().filter(|(start, end, c)| *c == channels && *start <= at && at <= *end).count()).max().unwrap_or(0);
                let have = plan.buffers().iter().filter(|&&c| c == channels).count();
                assert_eq!(have, most, "seed {seed}, {channels} channels");
            }
        }
    }

    #[test]
    fn a_buffer_is_zeroed_once_for_every_value_that_begins_in_it_and_at_the_op_where_it_begins() {
        for seed in 0..300 {
            let patch = random_patch(seed);
            let plan = compile(&patch).unwrap();
            let order: Vec<u32> = plan.order().collect();
            let lives = lives(&patch, &order);
            let mut expected: Vec<Vec<u32>> = vec![Vec::new(); order.len()];
            for (at, op) in plan.ops().iter().enumerate() {
                for m in 0..op.outputs.len() {
                    expected[lives.outputs[&(at, m)].0].push(op.outputs[m]);
                }
                for k in 0..op.inputs.len() {
                    if let Some(sum) = lives.sums.get(&(at, k)) {
                        expected[sum.0].push(op.inputs[k]);
                    }
                }
            }
            for (at, op) in plan.ops().iter().enumerate() {
                let mut have = op.zero.clone();
                have.sort_unstable();
                expected[at].sort_unstable();
                assert_eq!(have, expected[at], "seed {seed}, op {at}");
            }
        }
    }

    #[test]
    fn a_feed_is_made_for_every_cable_into_the_buffer_of_the_sum_that_the_cable_ends_at_and_for_none_that_is_an_input_itself() {
        for seed in 0..200 {
            let patch = random_patch(seed);
            let plan = compile(&patch).unwrap();
            let place: HashMap<u32, usize> = plan.order().enumerate().map(|(at, id)| (id, at)).collect();
            let aliased = aliased(&patch);
            let mut expected: Vec<(usize, u32, u32)> = patch
                .cables()
                .iter()
                .enumerate()
                .filter(|(at, _)| !aliased[*at])
                .map(|(_, c)| (place[&c.from.node], u32::from(c.from.port), plan.ops()[place[&c.to.node]].inputs[usize::from(c.to.port)]))
                .collect();
            let mut have: Vec<(usize, u32, u32)> = plan.ops().iter().enumerate().flat_map(|(at, op)| op.feeds.iter().map(move |f| (at, f.output, f.into))).collect();
            expected.sort_unstable();
            have.sort_unstable();
            assert_eq!(have, expected, "seed {seed}");
        }
    }

    #[test]
    fn a_patch_with_a_cycle_that_is_put_in_a_random_patch_is_refused_and_the_cycle_is_among_what_is_named() {
        for seed in 0..100 {
            let mut patch = random_patch(seed);
            // A node with a cable to an earlier one in the order, and a cable that runs the other way along the same ports' order, closes a cycle: take the last node in the
            // order that has an input, and make a cable from it to the first node of the order that has an output... only if there is a path, so use a self-cable instead.
            let mut rng = Rng::new(seed ^ 0xC1C1E);
            let with_both: Vec<(u32, NodeSpec)> = patch.nodes().iter().filter(|(_, s)| !s.inputs.is_empty() && !s.outputs.is_empty()).cloned().collect();
            if with_both.is_empty() {
                continue;
            }
            let (id, spec) = &with_both[rng.below(with_both.len())];
            let (out, inp) = (0..spec.outputs.len() as u16).flat_map(|o| (0..spec.inputs.len() as u16).map(move |i| (o, i))).find(|&(o, i)| spec.outputs[o as usize].channels == spec.inputs[i as usize].channels).unzip();
            let (Some(out), Some(inp)) = (out, inp) else { continue };
            patch.connect(Port::new(*id, out), Port::new(*id, inp));
            match compile(&patch) {
                Err(CompileError::Cycle(nodes)) => assert!(nodes.contains(id), "seed {seed}"),
                other => panic!("seed {seed}: {other:?}"),
            }
        }
    }
}
