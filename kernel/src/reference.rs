//! The reference interpreter: slow, and obviously right. It has what a plan has not: a buffer of its own for every output that is kept for the whole block, and a sum
//! made at an input from those, in the order that the sum is defined to be made in (the producers by their place in the order, and a producer's cables in the order they were
//! drawn), starting from +0.0. No buffer is shared, nothing is fed on the way, and nothing is zeroed that was not made zero. A plan that is run by the executor must give
//! what this gives, bit for bit: that is what finds the bugs of colouring, zeroing and feeding.

use std::collections::HashMap;

use anyhow::Result;

use crate::exec::{ChanBuf, Io, Nodes};
use crate::patch::{Id, Patch};

/// Run one block of `frames` frames of `patch`, the nodes in `order` (which `Plan::order` gives: the order is `compile`'s, and what it is held to is tested apart).
pub fn run_reference<I: Id, C>(patch: &Patch<I>, order: &[I], frames: usize, nodes: &mut impl Nodes<I, C>, cx: &mut C) -> Result<()> {
    let place: HashMap<I, usize> = order.iter().enumerate().map(|(at, id)| (*id, at)).collect();
    let spec_of: HashMap<I, &crate::patch::NodeSpec> = patch.nodes().iter().map(|(id, spec)| (*id, spec)).collect();
    let mut made: HashMap<I, Vec<ChanBuf>> = HashMap::new();
    for &id in order {
        let spec = spec_of[&id];
        let mut inputs: Vec<ChanBuf> = Vec::new();
        for (k, port) in spec.inputs.iter().enumerate() {
            let mut sum: ChanBuf = vec![vec![0.0; frames]; usize::from(port.channels)];
            let mut feeding: Vec<(usize, usize)> = patch
                .cables()
                .iter()
                .enumerate()
                .filter(|(_, c)| c.to.node == id && usize::from(c.to.port) == k)
                .map(|(at, c)| (place[&c.from.node], at))
                .collect();
            feeding.sort_unstable();
            for (_, at) in feeding {
                let cable = &patch.cables()[at];
                let source = &made[&cable.from.node][usize::from(cable.from.port)];
                for (s, o) in sum.iter_mut().zip(source) {
                    for (x, y) in s.iter_mut().zip(o) {
                        *x += *y;
                    }
                }
            }
            inputs.push(sum);
        }
        let outputs: Vec<ChanBuf> = spec.outputs.iter().map(|port| vec![vec![0.0; frames]; usize::from(port.channels)]).collect();
        let mut io = Io::from_buffers(frames, inputs, outputs);
        nodes.process(id, &mut io, cx)?;
        made.insert(id, io.into_outputs());
    }
    Ok(())
}
