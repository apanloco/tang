//! The executor asks the allocator for nothing while it runs a block: the buffers are made before (`Pool::fit` and `Pool::reserve`, on a thread that may allocate) and moved
//! to the nodes and back, not copied, and a sum is a loop over slices. The rule of §11.3 of `docs/patch-graph.md` that the audio thread allocates nothing, for the part of it
//! that is the kernel's.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use anyhow::Result;
use kernel::{DelayLine, Io, Kernel, Node, NodeSpec, Nodes, Patch, Plan, Pool, Port, PortSpec, align, compile, with_delays};

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static COUNT: Cell<usize> = const { Cell::new(0) };
}

/// The system allocator, counting the calls that the thread makes while it is told to.
struct Counting;

fn count() {
    if COUNTING.with(Cell::get) {
        COUNT.with(|count| count.set(count.get() + 1));
    }
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The result of `f`, and how many times it asked the allocator for memory.
fn allocations_in<R>(f: impl FnOnce() -> R) -> (R, usize) {
    COUNT.with(|count| count.set(0));
    COUNTING.with(|counting| counting.set(true));
    let result = f();
    COUNTING.with(|counting| counting.set(false));
    (result, COUNT.with(Cell::get))
}

const ST: PortSpec = PortSpec::audio(2);
const SINGLE: PortSpec = PortSpec::single(2);

/// The shape of a session: `lanes` sources, each through two effects (whose inputs are `single`: each is the buffer of the node before it), into one of two groups (or, for the odd
/// ones out, straight to the master), and the groups into the master.
fn session(lanes: usize) -> Patch<usize> {
    let mut patch = Patch::new();
    let (group_a, group_b, master) = (10_000, 10_001, 10_002);
    patch.add_node(master, NodeSpec::new(vec![ST], vec![], u32::MAX));
    patch.add_node(group_a, NodeSpec::new(vec![ST], vec![ST], lanes as u32));
    patch.add_node(group_b, NodeSpec::new(vec![ST], vec![ST], lanes as u32 + 1));
    for lane in 0..lanes {
        let (fx1, fx2) = (lane + 1_000, lane + 2_000);
        patch.add_node(lane, NodeSpec::new(vec![], vec![ST], lane as u32));
        patch.add_node(fx1, NodeSpec::new(vec![SINGLE], vec![ST], lane as u32));
        patch.add_node(fx2, NodeSpec::new(vec![SINGLE], vec![ST], lane as u32));
        patch.connect(Port::new(lane, 0), Port::new(fx1, 0));
        patch.connect(Port::new(fx1, 0), Port::new(fx2, 0));
        let to = match lane % 3 {
            0 => group_a,
            1 => group_b,
            _ => master,
        };
        patch.connect(Port::new(fx2, 0), Port::new(to, 0));
    }
    patch.connect(Port::new(group_a, 0), Port::new(master, 0));
    patch.connect(Port::new(group_b, 0), Port::new(master, 0));
    patch
}

/// Nodes that do some arithmetic on what they have: a source makes a ramp, the others add up their inputs into their outputs, or work in place when they can.
struct Arithmetic;

fn work(io: &mut Io, block: usize) {
    let frames = io.frames();
    let (inputs, outputs) = io.split();
    for (m, output) in outputs.iter_mut().enumerate() {
        for (c, channel) in output.iter_mut().enumerate() {
            for (i, y) in channel.iter_mut().enumerate() {
                let mut v = if inputs.is_empty() { ((block + i + c + m) % 17) as f32 * 0.1 } else { 0.0 };
                for input in inputs.iter() {
                    v += input[c][i] * 0.5;
                }
                *y = v;
            }
        }
    }
    let _ = frames;
}

impl Nodes<usize, usize> for Arithmetic {
    fn process(&mut self, _: usize, io: &mut Io, block: &mut usize) -> Result<()> {
        work(io, *block);
        Ok(())
    }
}

/// A block's worth of lengths that are not all the same, none over what was reserved.
const FRAMES: [usize; 7] = [512, 64, 256, 1, 480, 0, 512];

fn run_blocks(plan: &Plan<usize>, pool: &mut Pool, blocks: usize) -> usize {
    let mut block = 0;
    for n in 0..blocks {
        block = n;
        plan.run(pool, FRAMES[n % FRAMES.len()], &mut Arithmetic, &mut block).unwrap();
    }
    block
}

#[test]
fn the_counter_counts_what_a_vector_asks_for() {
    let (_, count) = allocations_in(|| std::hint::black_box(vec![1u8, 2, 3]));
    assert!(count >= 1, "{count}");
    let (_, count) = allocations_in(|| 1 + 1);
    assert_eq!(count, 0);
}

#[test]
fn a_block_asks_the_allocator_for_nothing() {
    let plan = compile(&session(8)).unwrap();
    let mut pool = Pool::new();
    pool.reserve(512);
    pool.fit(&plan);
    run_blocks(&plan, &mut pool, 4);
    let (_, count) = allocations_in(|| run_blocks(&plan, &mut pool, 70));
    assert_eq!(count, 0, "the plan has {} ops and {} buffers", plan.ops().len(), plan.buffers().len());
}

#[test]
fn the_very_first_block_asks_for_nothing_when_the_pool_was_fitted_and_given_room() {
    let plan = compile(&session(8)).unwrap();
    let mut pool = Pool::new();
    pool.reserve(512);
    pool.fit(&plan);
    let (_, count) = allocations_in(|| run_blocks(&plan, &mut pool, 14));
    assert_eq!(count, 0, "no warm-up: the room was made before");
    // A pool that is not given room has to make it, and the test sees that: it would say so if the room were not made.
    let mut bare = Pool::new();
    bare.fit(&plan);
    let (_, count) = allocations_in(|| run_blocks(&plan, &mut bare, 1));
    assert!(count > 0, "a pool with no room asks for it when the first block comes");
}

#[test]
fn the_blocks_after_a_plan_is_swapped_in_ask_for_nothing_either() {
    let small = compile(&session(3)).unwrap();
    let large = compile(&session(24)).unwrap();
    let mut pool = Pool::new();
    pool.reserve(512);
    pool.fit(&small);
    run_blocks(&small, &mut pool, 3);
    // The swap is made on a thread that may allocate: the pool is fitted to the plan that comes.
    pool.fit(&large);
    let (_, count) = allocations_in(|| run_blocks(&large, &mut pool, 70));
    assert_eq!(count, 0);
    pool.fit(&small);
    let (_, count) = allocations_in(|| run_blocks(&small, &mut pool, 70));
    assert_eq!(count, 0, "and back to the small one");
}

struct Ramp(f32);

impl Node<usize> for Ramp {
    fn process(&mut self, io: &mut Io, block: &mut usize) -> Result<()> {
        work(io, *block);
        self.0 += 1.0;
        Ok(())
    }
}

#[test]
fn a_kernel_over_an_arena_asks_for_nothing_in_a_block() {
    let mut kernel = Kernel::new();
    let mut patch = Patch::new();
    let keys: Vec<_> = (0..12).map(|_| kernel.arena_mut().insert(Ramp(0.0))).collect();
    for (at, key) in keys.iter().enumerate() {
        patch.add_node(*key, NodeSpec::new(if at == 0 { vec![] } else { vec![ST] }, if at == keys.len() - 1 { vec![] } else { vec![ST] }, 0));
        if at > 0 {
            patch.connect(Port::new(keys[at - 1], 0), Port::new(*key, 0));
        }
    }
    kernel.reserve(256);
    kernel.install(compile(&patch).unwrap());
    let mut block = 0;
    let (_, count) = allocations_in(|| kernel.run(256, &mut block).unwrap());
    assert_eq!(count, 0, "the first block, too");
    let (_, count) = allocations_in(|| {
        for n in 0..50 {
            block = n;
            kernel.run([256, 17, 100][n % 3], &mut block).unwrap();
        }
    });
    assert_eq!(count, 0);
}

/// The nodes of a plan that has delays in it: the delays are lines that the block goes through in place, and the others do some arithmetic.
struct WithLines(std::collections::HashMap<usize, DelayLine>);

impl Nodes<usize, usize> for WithLines {
    fn process(&mut self, id: usize, io: &mut Io, block: &mut usize) -> Result<()> {
        match self.0.get_mut(&id) {
            Some(line) => {
                io.forward(0, 0);
                line.run(&mut io.outputs_mut()[0]);
            }
            None => work(io, *block),
        }
        Ok(())
    }
}

#[test]
fn a_plan_with_delays_in_it_asks_the_allocator_for_nothing_in_a_block() {
    // Lane 0's first effect has 23 frames of latency and lane 1's second has 5: the other lanes of their groups, and the lanes that go to the master, are late by what they lack.
    let patch = session(8);
    let latency = |id: usize| match id {
        1_000 => 23,
        2_001 => 5,
        _ => 0,
    };
    let alignment = align(&patch, latency).unwrap();
    assert!(alignment.is_needed());
    let mut lines = std::collections::HashMap::new();
    let made = with_delays(&patch, &alignment, |number, _, frames| {
        lines.insert(50_000 + number, DelayLine::new(frames as usize, 2));
        50_000 + number
    });
    assert!(lines.len() >= 6, "{} delays", lines.len());
    let plan = compile(&made).unwrap();
    let mut pool = Pool::new();
    pool.reserve(512);
    pool.fit(&plan);
    let mut nodes = WithLines(lines);
    let mut block = 0;
    for n in 0..4 {
        block = n;
        plan.run(&mut pool, FRAMES[n % FRAMES.len()], &mut nodes, &mut block).unwrap();
    }
    let (_, count) = allocations_in(|| {
        for n in 0..70 {
            block = n;
            plan.run(&mut pool, FRAMES[n % FRAMES.len()], &mut nodes, &mut block).unwrap();
        }
    });
    assert_eq!(count, 0, "the plan has {} ops", plan.ops().len());
}

#[test]
fn a_line_that_is_made_the_length_it_is_asks_the_allocator_for_nothing_and_a_longer_one_does() {
    let mut line = DelayLine::new(480, 2);
    let (_, count) = allocations_in(|| line.resize(480));
    assert_eq!(count, 0, "the plan is made again with the same delays, and a line of them is made the length it is each time");
    let (_, count) = allocations_in(|| line.resize(960));
    assert!(count > 0, "a longer line is made on a thread that may allocate: the test sees that it asks");
}
