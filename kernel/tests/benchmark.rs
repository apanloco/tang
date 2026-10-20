//! What the graph itself costs: the executor over nodes that do nothing, so that what is measured is the bookkeeping of a block (the zeroing of buffers where values begin, the
//! move of buffers to a node and back, the call, the feeds that add an output into a sum) and nothing of any node's work. `docs/patch-graph.md` §11.3 says a thousand nodes should
//! be hundreds of microseconds of a block of 5.3 ms; this is the measure of it.
//!
//! `cargo test --release -p kernel --test benchmark -- --ignored --nocapture --test-threads=1`

use std::time::Instant;

use anyhow::Result;
use kernel::{Io, NodeSpec, Nodes, Patch, Pool, Port, PortSpec, compile};

const ST: PortSpec = PortSpec::audio(2);

/// A node that does nothing.
struct Nothing;

impl Nodes<usize, ()> for Nothing {
    fn process(&mut self, _: usize, _: &mut Io, _: &mut ()) -> Result<()> {
        Ok(())
    }
}

fn chain(n: usize) -> Patch<usize> {
    let mut patch = Patch::new();
    for at in 0..n {
        patch.add_node(at, NodeSpec::new(if at == 0 { vec![] } else { vec![ST] }, if at == n - 1 { vec![] } else { vec![ST] }, 0));
        if at > 0 {
            patch.connect(Port::new(at - 1, 0), Port::new(at, 0));
        }
    }
    patch
}

fn fan_in(n: usize) -> Patch<usize> {
    let mut patch = Patch::new();
    patch.add_node(n, NodeSpec::new(vec![ST], vec![], u32::MAX));
    for at in 0..n {
        patch.add_node(at, NodeSpec::new(vec![], vec![ST], at as u32));
        patch.connect(Port::new(at, 0), Port::new(n, 0));
    }
    patch
}

/// Lanes of `chain` nodes each, into two groups and the master, as `no_allocation.rs` has it.
fn session(lanes: usize, chain: usize) -> Patch<usize> {
    let mut patch = Patch::new();
    let (group_a, group_b, master) = (1_000_000, 1_000_001, 1_000_002);
    patch.add_node(master, NodeSpec::new(vec![ST], vec![], u32::MAX));
    patch.add_node(group_a, NodeSpec::new(vec![ST], vec![ST], lanes as u32));
    patch.add_node(group_b, NodeSpec::new(vec![ST], vec![ST], lanes as u32 + 1));
    for lane in 0..lanes {
        let first = lane * 100;
        patch.add_node(first, NodeSpec::new(vec![], vec![ST], lane as u32));
        for at in 1..chain {
            patch.add_node(first + at, NodeSpec::new(vec![ST], vec![ST], lane as u32));
            patch.connect(Port::new(first + at - 1, 0), Port::new(first + at, 0));
        }
        let to = if lane % 2 == 0 { group_a } else { group_b };
        patch.connect(Port::new(first + chain - 1, 0), Port::new(to, 0));
    }
    patch.connect(Port::new(group_a, 0), Port::new(master, 0));
    patch.connect(Port::new(group_b, 0), Port::new(master, 0));
    patch
}

fn median_ns(patch: &Patch<usize>, frames: usize) -> (f64, usize, usize) {
    let plan = compile(patch).unwrap();
    let mut pool = Pool::new();
    pool.reserve(1024);
    pool.fit(&plan);
    for _ in 0..200 {
        plan.run(&mut pool, frames, &mut Nothing, &mut ()).unwrap();
    }
    let mut took: Vec<f64> = (0..2000)
        .map(|_| {
            let start = Instant::now();
            plan.run(&mut pool, frames, &mut Nothing, &mut ()).unwrap();
            start.elapsed().as_nanos() as f64
        })
        .collect();
    took.sort_by(|a, b| a.total_cmp(b));
    (took[took.len() / 2], plan.ops().len(), plan.buffers().len())
}

#[test]
#[ignore = "a benchmark: cargo test --release -p kernel --test benchmark -- --ignored --nocapture --test-threads=1"]
fn what_the_graph_costs_over_nodes_that_do_nothing() {
    let shapes: Vec<(&str, Patch<usize>)> = vec![
        ("chain of 1000", chain(1000)),
        ("fan-in of 1000", fan_in(1000)),
        ("session: 8 lanes of 4, 2 groups", session(8, 4)),
        ("session: 64 lanes of 4, 2 groups", session(64, 4)),
    ];
    eprintln!("{:<36} {:>6} {:>8} {:>6} {:>12} {:>10} {:>10}", "shape", "nodes", "buffers", "frames", "µs a block", "ns a node", "of budget");
    for (name, patch) in &shapes {
        for frames in [0, 64, 256, 1024] {
            let (ns, nodes, buffers) = median_ns(patch, frames);
            let budget_ns = frames.max(1) as f64 / 48_000.0 * 1e9;
            let budget = if frames == 0 { "-".to_string() } else { format!("{:.2}%", 100.0 * ns / budget_ns) };
            eprintln!("{name:<36} {nodes:>6} {buffers:>8} {frames:>6} {:>12.1} {:>10.0} {budget:>10}", ns / 1e3, ns / nodes as f64);
        }
    }
}
