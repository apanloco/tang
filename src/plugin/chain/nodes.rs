//! The lane, the group and the output mix as nodes of a plan (P1 of `docs/patch-graph.md` §11.6).
//!
//! `AudioGraph` still has its lanes and groups in vectors and its commands name places, so the plan is made of what is there: a node for each lane, each group and the output
//! mix, a cable from each lane to its group (or to the mix if it has none) and from each group to the mix, and ranks that are the places (the lanes in order, then the groups,
//! then the mix), so that the order of the plan, and so the order of every sum, is the order that the loop it replaced had: a group's bus is the lanes added in the order of their
//! places, and the mix is the lanes that have no group and then the groups. The nodes run the code that was in the loop, as it was.
//!
//! The plan is made again when the wiring is not the one it was made for. That is found by reading the wiring into a `Shape` before every block and comparing it with the one the
//! plan was made for (a few comparisons of keys: a knob that is turned compiles nothing), so a lane that a test pushes into the vector is found as well as one that a command
//! puts there.

use anyhow::Result;
use crossbeam_channel::Sender;
use kernel::{Io, Key, NodeSpec, Nodes, Patch, Plan, Pool, Port, PortSpec};

use super::*;

/// What a node of the plan is called: the key of a lane or a group (`Ordered`), or the mix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum NodeId {
    Lane(Key),
    Group(Key),
    Master,
}

/// What every node is told of the block besides its buffers.
pub(super) struct BlockCx<'a> {
    /// The notes and controllers that came in, with their frames in this block.
    pub midi: &'a [(u64, [u8; 3])],
    /// Where the output mix goes.
    pub audio_out: &'a mut [Vec<f32>],
    pub num_channels: usize,
    pub scale: ScaleSetting,
    pub bpm: f32,
    /// Where what is measured is published, and how far a meter's peak falls over this block, if anything is measured.
    pub telemetry: Option<&'a Telemetry>,
    pub fall: Option<f32>,
    /// Where a control that a MIDI-learn modulator has captured is told.
    pub learn_tx: Option<&'a Sender<MidiLearnedNotification>>,
}

/// The output mix: what is added up from the lanes that have no group and the groups is copied to the output and measured.
pub(super) struct Master {
    /// Measures the output; publishes into `telemetry::MASTER`.
    pub(super) meter: Meter,
}

impl Master {
    pub(super) fn new() -> Self {
        Master { meter: Meter::new(telemetry::MASTER) }
    }

    fn run_node(&mut self, io: &mut Io, cx: &mut BlockCx<'_>) -> Result<()> {
        let mix = &io.inputs()[0];
        for (ch, out) in cx.audio_out.iter_mut().enumerate() {
            if ch < mix.len() {
                let copy_len = out.len().min(mix[ch].len());
                out[..copy_len].copy_from_slice(&mix[ch][..copy_len]);
            }
        }
        // What reaches the output, measured and kept for a scope.
        if let (Some(telemetry), Some(fall)) = (cx.telemetry, cx.fall) {
            self.meter.update(telemetry, mix, fall);
            telemetry.scope.push(mix);
        }
        Ok(())
    }
}

impl InstrumentLane {
    /// The lane as a node: it makes its output in the buffer of its port (which is zero), is let in or out by its mute and solo, is measured and is read for a control that
    /// was learned. `position` is where it is among the lanes, which is what a notification of a learned control says. What the plan then does is add the output to the bus of
    /// the lane's group or to the mix.
    fn run_node(&mut self, io: &mut Io, cx: &mut BlockCx<'_>, position: usize) -> Result<()> {
        let out = &mut io.outputs_mut()[0];
        self.process(cx.midi, out, cx.num_channels, cx.scale, cx.bpm, cx.telemetry)?;
        // What the lane is heard at: its own mute and solo (its group's mute is the bus's). The meter is after it, as it is after the volume.
        fade(out, cx.num_channels, self.heard_at, self.heard_goal);
        self.heard_at = self.heard_goal;
        if let (Some(telemetry), Some(fall)) = (cx.telemetry, cx.fall) {
            publish_outputs(&self.modulators, telemetry);
            if let Some(meter) = self.meter.as_mut() {
                meter.update(telemetry, out, fall);
            }
        }
        // A lane MidiLearn modulator may have just captured a control.
        AudioGraph::drain_learned(cx.learn_tx, &mut self.modulators, |_| ModulatorLocation::Lane(position));
        Ok(())
    }
}

impl Group {
    /// The group as a node: its input is the sum of its members, the bus, which it works on in place (volume, effects, pan) and hands on as its output, let in or out by its
    /// mute and the solos, and measured.
    fn run_node(&mut self, io: &mut Io, cx: &mut BlockCx<'_>) -> Result<()> {
        let frames = io.frames();
        let bus = &mut io.inputs_mut()[0];
        self.finish(bus, cx.num_channels, frames, cx.telemetry)?;
        // What the bus is heard at: a group that is not muted is heard when nothing is soloed, when it is, and when a lane in it is.
        fade(bus, cx.num_channels, self.heard_at, self.heard_goal);
        self.heard_at = self.heard_goal;
        if let (Some(telemetry), Some(fall), Some(meter)) = (cx.telemetry, cx.fall, self.meter.as_mut()) {
            meter.update(telemetry, bus, fall);
        }
        io.forward(0, 0);
        Ok(())
    }
}

/// The nodes of the plan, by name: what `Plan::run` asks for a block of.
struct Store<'a> {
    lanes: &'a mut Ordered<InstrumentLane>,
    groups: &'a mut Ordered<Group>,
    master: &'a mut Master,
}

impl<'a, 'cx> Nodes<NodeId, BlockCx<'cx>> for Store<'a> {
    fn process(&mut self, id: NodeId, io: &mut Io, cx: &mut BlockCx<'cx>) -> Result<()> {
        match id {
            NodeId::Lane(key) => match self.lanes.get_by_key_mut(key) {
                Some((position, lane)) => lane.run_node(io, cx, position),
                // A lane that is gone makes silence: the plan is made again before the next block.
                None => Ok(()),
            },
            NodeId::Group(key) => match self.groups.get_by_key_mut(key) {
                Some((_, group)) => group.run_node(io, cx),
                None => Ok(()),
            },
            NodeId::Master => self.master.run_node(io, cx),
        }
    }
}

/// How the lanes and groups are wired, as far as the plan is concerned: for each lane its key and its group's (none if it has none, or if the group it names is not there),
/// and the keys of the groups, in the order of their places.
#[derive(Default, PartialEq)]
struct Shape {
    channels: usize,
    lanes: Vec<(Key, Option<Key>)>,
    groups: Vec<Key>,
}

impl Shape {
    /// Make this the same as `other`, using the room it has.
    fn copy_from(&mut self, other: &Shape) {
        self.channels = other.channels;
        self.lanes.clone_from(&other.lanes);
        self.groups.clone_from(&other.groups);
    }

    /// Read the wiring of `lanes` and `groups` into this, keeping the room it has.
    fn read(&mut self, lanes: &Ordered<InstrumentLane>, groups: &Ordered<Group>, channels: usize) {
        self.channels = channels;
        self.lanes.clear();
        self.lanes.extend(lanes.iter().zip(lanes.keys()).map(|(lane, key)| (*key, lane.group.and_then(|g| groups.key_at(g)))));
        self.groups.clear();
        self.groups.extend_from_slice(groups.keys());
    }
}

/// The plan that the lanes, groups and mix are run by, the buffers it runs over, and what it was made for.
pub(super) struct Schedule {
    plan: Plan<NodeId>,
    pool: Pool,
    /// The wiring that `plan` was made for, and room to read the wiring that there is now into.
    shape: Shape,
    now: Shape,
    /// The patch that is made again and compiled when the wiring changes, kept so that its room is.
    patch: Patch<NodeId>,
    /// How many frames of a block the pool has room for.
    room: usize,
    /// How many times a plan has been made, for the tests to see that a block that finds the wiring as it was makes none.
    #[cfg(test)]
    compiles: usize,
}

impl Schedule {
    pub(super) fn new() -> Self {
        let plan = kernel::compile(&Patch::<NodeId>::new()).expect("a patch with nothing in it compiles");
        Schedule {
            plan,
            pool: Pool::new(),
            shape: Shape::default(),
            now: Shape::default(),
            patch: Patch::new(),
            room: 0,
            #[cfg(test)]
            compiles: 0,
        }
    }

    /// Make the plan the one for the lanes and groups as they are now (when they are not as they were, which asks the allocator for memory, as a command that inserts a lane
    /// does), and give the pool room for a block of `frames`.
    pub(super) fn refresh(&mut self, lanes: &Ordered<InstrumentLane>, groups: &Ordered<Group>, channels: usize, frames: usize) {
        self.now.read(lanes, groups, channels);
        if self.now != self.shape {
            self.compile();
            // Copied, not swapped: both keep their room, so that reading the wiring of the next block asks for none.
            self.shape.copy_from(&self.now);
        }
        if frames > self.room {
            self.pool.reserve(frames);
            self.room = frames;
        }
    }

    /// The plan for `now`: the mix, a node for each lane and each group, and the cables.
    fn compile(&mut self) {
        #[cfg(test)]
        {
            self.compiles += 1;
        }
        let audio = PortSpec::audio(self.now.channels as u16);
        let lanes = self.now.lanes.len() as u32;
        self.patch.clear();
        self.patch.add_node(NodeId::Master, NodeSpec::new(vec![audio], vec![], u32::MAX));
        for (at, (key, _)) in self.now.lanes.iter().enumerate() {
            self.patch.add_node(NodeId::Lane(*key), NodeSpec::new(vec![], vec![audio], at as u32));
        }
        for (at, key) in self.now.groups.iter().enumerate() {
            self.patch.add_node(NodeId::Group(*key), NodeSpec::new(vec![audio], vec![audio], lanes + at as u32));
        }
        for (key, group) in &self.now.lanes {
            let to = group.map_or(NodeId::Master, NodeId::Group);
            self.patch.connect(Port::new(NodeId::Lane(*key), 0), Port::new(to, 0));
        }
        for key in &self.now.groups {
            self.patch.connect(Port::new(NodeId::Group(*key), 0), Port::new(NodeId::Master, 0));
        }
        match kernel::compile(&self.patch) {
            Ok(plan) => {
                self.pool.fit(&plan);
                self.plan = plan;
            }
            Err(error) => {
                // The wiring is made of nodes and cables that go one way, so this is not a thing that happens: the plan that there was is kept.
                log::error!("the plan of the lanes and groups does not compile: {error}");
                debug_assert!(false, "the plan of the lanes and groups does not compile: {error}");
            }
        }
    }

    /// Run one block: the lanes, the groups and the mix.
    pub(super) fn run(
        &mut self,
        lanes: &mut Ordered<InstrumentLane>,
        groups: &mut Ordered<Group>,
        master: &mut Master,
        frames: usize,
        cx: &mut BlockCx<'_>,
    ) -> Result<()> {
        self.plan.run(&mut self.pool, frames, &mut Store { lanes, groups, master }, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alloc_guard::allocations_in;

    fn lanes(n: usize) -> Ordered<InstrumentLane> {
        let mut lanes = Ordered::new();
        for _ in 0..n {
            lanes.push(InstrumentLane::new(2, super::test_node()));
        }
        lanes
    }

    fn groups(n: usize) -> Ordered<Group> {
        let mut groups = Ordered::new();
        for _ in 0..n {
            groups.push(Group::new(2, super::test_node()));
        }
        groups
    }

    /// What a node is called, by where it is: `lane 1`, `group 0`, `mix`.
    fn name(id: NodeId, lanes: &Ordered<InstrumentLane>, groups: &Ordered<Group>) -> String {
        match id {
            NodeId::Lane(key) => format!("lane {}", lanes.position_of(key).expect("a lane that is there")),
            NodeId::Group(key) => format!("group {}", groups.position_of(key).expect("a group that is there")),
            NodeId::Master => "mix".to_string(),
        }
    }

    /// The nodes of the plan in the order they run.
    fn order(schedule: &Schedule, lanes: &Ordered<InstrumentLane>, groups: &Ordered<Group>) -> Vec<String> {
        schedule.plan.order().map(|id| name(id, lanes, groups)).collect()
    }

    /// Every feed of the plan, `(from, to)`: the node that makes it, and the node whose input it is added to (the first op after it that reads the buffer it goes into).
    fn feeds(schedule: &Schedule, lanes: &Ordered<InstrumentLane>, groups: &Ordered<Group>) -> Vec<(String, String)> {
        let ops = schedule.plan.ops();
        let mut all = Vec::new();
        for (at, op) in ops.iter().enumerate() {
            for feed in &op.feeds {
                let to = ops[at + 1..].iter().find(|later| later.inputs.contains(&feed.into)).expect("a node that reads the sum");
                all.push((name(op.node, lanes, groups), name(to.node, lanes, groups)));
            }
        }
        all
    }

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[test]
    fn an_engine_with_nothing_in_it_has_a_plan_of_the_mix() {
        let mut schedule = Schedule::new();
        let (lanes, groups) = (lanes(0), groups(0));
        schedule.refresh(&lanes, &groups, 2, 64);
        assert_eq!(order(&schedule, &lanes, &groups), ["mix"]);
        assert!(feeds(&schedule, &lanes, &groups).is_empty());
    }

    #[test]
    fn the_plan_runs_the_lanes_in_order_then_the_groups_then_the_mix() {
        let mut schedule = Schedule::new();
        let (lanes, groups) = (lanes(3), groups(2));
        schedule.refresh(&lanes, &groups, 2, 64);
        assert_eq!(order(&schedule, &lanes, &groups), ["lane 0", "lane 1", "lane 2", "group 0", "group 1", "mix"]);
    }

    #[test]
    fn a_lane_feeds_its_group_or_the_mix_and_a_group_feeds_the_mix() {
        let mut schedule = Schedule::new();
        let (mut lanes, groups) = (lanes(3), groups(2));
        lanes[0].group = Some(1);
        lanes[2].group = Some(0);
        schedule.refresh(&lanes, &groups, 2, 64);
        assert_eq!(
            feeds(&schedule, &lanes, &groups),
            pairs(&[("lane 0", "group 1"), ("lane 1", "mix"), ("lane 2", "group 0"), ("group 0", "mix"), ("group 1", "mix")])
        );
    }

    #[test]
    fn a_lane_that_names_a_group_that_is_not_there_feeds_the_mix() {
        let mut schedule = Schedule::new();
        let (mut lanes, groups) = (lanes(2), groups(1));
        lanes[0].group = Some(5);
        lanes[1].group = Some(0);
        schedule.refresh(&lanes, &groups, 2, 64);
        assert_eq!(feeds(&schedule, &lanes, &groups), pairs(&[("lane 0", "mix"), ("lane 1", "group 0"), ("group 0", "mix")]));
    }

    #[test]
    fn the_plan_is_made_again_when_the_wiring_is_not_what_it_was_made_for_and_not_otherwise() {
        let mut schedule = Schedule::new();
        let (mut lanes, mut groups) = (lanes(3), groups(1));
        let refresh = |schedule: &mut Schedule, lanes: &Ordered<InstrumentLane>, groups: &Ordered<Group>, channels, frames| {
            schedule.refresh(lanes, groups, channels, frames);
            schedule.compiles
        };
        assert_eq!(refresh(&mut schedule, &lanes, &groups, 2, 64), 1, "the first block");
        assert_eq!(refresh(&mut schedule, &lanes, &groups, 2, 64), 1, "the same");
        assert_eq!(refresh(&mut schedule, &lanes, &groups, 2, 1024), 1, "a longer block is room, not a plan");
        lanes[1].solo = true;
        lanes[1].volume = 0.5;
        lanes[1].muted = true;
        assert_eq!(refresh(&mut schedule, &lanes, &groups, 2, 64), 1, "mute, solo and volume are not wiring");
        lanes[0].group = Some(0);
        assert_eq!(refresh(&mut schedule, &lanes, &groups, 2, 64), 2, "a lane in a group");
        lanes[0].group = Some(0);
        assert_eq!(refresh(&mut schedule, &lanes, &groups, 2, 64), 2, "the same group");
        lanes.swap(0, 2);
        assert_eq!(refresh(&mut schedule, &lanes, &groups, 2, 64), 3, "two lanes that change places");
        lanes.push(InstrumentLane::new(2, super::test_node()));
        assert_eq!(refresh(&mut schedule, &lanes, &groups, 2, 64), 4, "a lane that comes");
        lanes.remove(3);
        assert_eq!(refresh(&mut schedule, &lanes, &groups, 2, 64), 5, "a lane that goes");
        groups.push(Group::new(2, super::test_node()));
        assert_eq!(refresh(&mut schedule, &lanes, &groups, 2, 64), 6, "a group that comes");
        groups.remove(1);
        assert_eq!(refresh(&mut schedule, &lanes, &groups, 2, 64), 7, "a group that goes");
        assert_eq!(refresh(&mut schedule, &lanes, &groups, 1, 64), 8, "buffers of another width");
    }

    #[test]
    fn lanes_that_swap_places_are_run_in_their_new_places_and_their_groups_go_with_them() {
        let mut schedule = Schedule::new();
        let (mut lanes, groups) = (lanes(3), groups(2));
        lanes[0].group = Some(1);
        lanes[1].group = Some(0);
        schedule.refresh(&lanes, &groups, 2, 64);
        let before = feeds(&schedule, &lanes, &groups);
        assert_eq!(before[..3], pairs(&[("lane 0", "group 1"), ("lane 1", "group 0"), ("lane 2", "mix")])[..]);
        lanes.swap(0, 1);
        schedule.refresh(&lanes, &groups, 2, 64);
        let after = feeds(&schedule, &lanes, &groups);
        assert_eq!(after[..3], pairs(&[("lane 0", "group 0"), ("lane 1", "group 1"), ("lane 2", "mix")])[..]);
        assert_eq!(schedule.plan.order().take(3).collect::<Vec<_>>(), lanes.keys().iter().map(|key| NodeId::Lane(*key)).collect::<Vec<_>>());
    }

    #[test]
    fn what_a_group_is_given_is_the_sum_of_its_members_and_the_mix_the_sum_of_what_is_in_no_group_and_the_groups() {
        // Two lanes in a group and one in none: the mix is the lane in no group first and then the group, as the loop that the plan replaced added them.
        let mut schedule = Schedule::new();
        let (mut lanes, groups) = (lanes(3), groups(1));
        lanes[0].group = Some(0);
        lanes[2].group = Some(0);
        schedule.refresh(&lanes, &groups, 2, 64);
        let ops = schedule.plan.ops();
        let mix = ops.last().unwrap();
        assert_eq!(mix.node, NodeId::Master);
        let into_mix: Vec<String> = ops.iter().filter(|op| op.feeds.iter().any(|feed| feed.into == mix.inputs[0])).map(|op| name(op.node, &lanes, &groups)).collect();
        assert_eq!(into_mix, ["lane 1", "group 0"], "in the order they run");
        let group = ops.iter().find(|op| matches!(op.node, NodeId::Group(_))).unwrap();
        let into_group: Vec<String> = ops.iter().filter(|op| op.feeds.iter().any(|feed| feed.into == group.inputs[0] && !matches!(op.node, NodeId::Master))).map(|op| name(op.node, &lanes, &groups)).collect();
        assert_eq!(into_group, ["lane 0", "lane 2"]);
    }

    #[test]
    fn reading_the_wiring_of_the_next_block_asks_the_allocator_for_nothing_not_even_right_after_a_plan_was_made() {
        let mut schedule = Schedule::new();
        let (mut lanes, groups) = (lanes(4), groups(2));
        lanes[1].group = Some(1);
        schedule.refresh(&lanes, &groups, 2, 256);
        let (_, asked) = allocations_in(|| {
            for _ in 0..5 {
                schedule.refresh(&lanes, &groups, 2, 256);
            }
        });
        assert_eq!(asked, 0);
        assert_eq!(schedule.compiles, 1);
    }

    #[test]
    fn the_first_block_after_the_plan_was_refreshed_for_it_asks_the_allocator_for_nothing() {
        let mut schedule = Schedule::new();
        let (mut lanes, mut groups) = (lanes(2), groups(1));
        lanes[0].group = Some(0);
        schedule.refresh(&lanes, &groups, 2, 128);
        let mut master = Master::new();
        let mut out = vec![vec![0.0f32; 128]; 2];
        let mut cx = BlockCx { midi: &[], audio_out: &mut out, num_channels: 2, scale: Default::default(), bpm: 120.0, telemetry: None, fall: None, learn_tx: None };
        let (result, asked) = allocations_in(|| schedule.run(&mut lanes, &mut groups, &mut master, 128, &mut cx));
        result.unwrap();
        assert_eq!(asked, 0, "refresh made the room, as the pool of the kernel has it made before a block");
    }

    #[test]
    fn a_lane_that_was_taken_out_since_the_plan_was_made_makes_silence_and_the_block_runs() {
        let mut schedule = Schedule::new();
        let (mut lanes, mut groups) = (lanes(2), groups(1));
        schedule.refresh(&lanes, &groups, 2, 8);
        lanes.remove(0);
        let mut master = Master::new();
        let mut out = vec![vec![1.0f32; 8]; 2];
        let mut cx = BlockCx { midi: &[], audio_out: &mut out, num_channels: 2, scale: Default::default(), bpm: 120.0, telemetry: None, fall: None, learn_tx: None };
        schedule.run(&mut lanes, &mut groups, &mut master, 8, &mut cx).unwrap();
        assert!(out.iter().flatten().all(|&s| s == 0.0), "the lane that is left has no instrument, and the one that went is silence");
    }
}
