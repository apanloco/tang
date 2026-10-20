//! The lane, the group and the output mix as nodes of a plan (P1 and P2 of `docs/patch-graph.md` §11.6).
//!
//! `AudioGraph` still has its lanes and groups in vectors and its commands name places, so the plan is made of what is there: a node for each lane, each group and the output
//! mix, a cable from each lane to its group (or to the mix if it has none) and from each group to the mix, and ranks that are the places (the lanes in order, then the groups,
//! then the mix), so that the order of the plan, and so the order of every sum, is the order that the loop it replaced had: a group's bus is the lanes added in the order of their
//! places, and the mix is the lanes that have no group and then the groups. The nodes run the code that was in the loop, as it was.
//!
//! A node of the plan is named by the id that the model gave it (`NodeId`, which the creating commands carry: a lane's, a group's), and the mix by `NodeId::MASTER`: the plan says
//! the things that the model says. The places of the ids are made when the plan is (a sorted list that a block looks a node up in), since the vectors are still addressed by place.
//!
//! The plan is made again when the wiring is not the one it was made for. That is found by reading the wiring into a `Shape` before every block and comparing it with the one the
//! plan was made for (a few comparisons of ids: a knob that is turned compiles nothing), so a lane that a test pushes into the vector is found as well as one that a command
//! puts there.

use anyhow::Result;
use crossbeam_channel::Sender;
use kernel::{Io, NodeSpec, Nodes, Patch, Plan, Pool, Port, PortSpec};

use super::*;

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

/// Where a node of the plan is kept: a lane or a group, at a place of its vector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Place {
    Lane(usize),
    Group(usize),
}

/// The nodes of the plan, by name: what `Plan::run` asks for a block of.
struct Store<'a> {
    lanes: &'a mut [InstrumentLane],
    groups: &'a mut [Group],
    master: &'a mut Master,
    /// Where each node was when the plan was made, in the order of the ids.
    places: &'a [(NodeId, Place)],
}

impl<'a, 'cx> Nodes<NodeId, BlockCx<'cx>> for Store<'a> {
    fn process(&mut self, id: NodeId, io: &mut Io, cx: &mut BlockCx<'cx>) -> Result<()> {
        if id == NodeId::MASTER {
            return self.master.run_node(io, cx);
        }
        let Ok(found) = self.places.binary_search_by_key(&id, |&(id, _)| id) else { return Ok(()) };
        // A node that is not at its place any more (the lane that was there is gone: the plan is made again before the next block) makes silence, and is not taken for the one
        // that is at its place now.
        match self.places[found].1 {
            Place::Lane(at) => match self.lanes.get_mut(at) {
                Some(lane) if lane.node == id => lane.run_node(io, cx, at),
                _ => Ok(()),
            },
            Place::Group(at) => match self.groups.get_mut(at) {
                Some(group) if group.node == id => group.run_node(io, cx),
                _ => Ok(()),
            },
        }
    }
}

/// How the lanes and groups are wired, as far as the plan is concerned: for each lane its id and its group's (none if it has none, or if the group it names is not there), and the
/// ids of the groups, in the order of their places.
#[derive(Default, PartialEq)]
struct Shape {
    channels: usize,
    lanes: Vec<(NodeId, Option<NodeId>)>,
    groups: Vec<NodeId>,
}

impl Shape {
    /// Make this the same as `other`, using the room it has.
    fn copy_from(&mut self, other: &Shape) {
        self.channels = other.channels;
        self.lanes.clone_from(&other.lanes);
        self.groups.clone_from(&other.groups);
    }

    /// Read the wiring of `lanes` and `groups` into this, keeping the room it has.
    fn read(&mut self, lanes: &[InstrumentLane], groups: &[Group], channels: usize) {
        self.channels = channels;
        self.lanes.clear();
        self.lanes.extend(lanes.iter().map(|lane| (lane.node, lane.group.and_then(|g| groups.get(g)).map(|group| group.node))));
        self.groups.clear();
        self.groups.extend(groups.iter().map(|group| group.node));
    }
}

/// The plan that the lanes, groups and mix are run by, the buffers it runs over, and what it was made for.
pub(super) struct Schedule {
    plan: Plan<NodeId>,
    pool: Pool,
    /// Where the plan's nodes are kept, by id: made with the plan.
    places: Vec<(NodeId, Place)>,
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
            places: Vec::new(),
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
    pub(super) fn refresh(&mut self, lanes: &[InstrumentLane], groups: &[Group], channels: usize, frames: usize) {
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
        self.patch.add_node(NodeId::MASTER, NodeSpec::new(vec![audio], vec![], u32::MAX));
        for (at, (id, _)) in self.now.lanes.iter().enumerate() {
            self.patch.add_node(*id, NodeSpec::new(vec![], vec![audio], at as u32));
        }
        for (at, id) in self.now.groups.iter().enumerate() {
            self.patch.add_node(*id, NodeSpec::new(vec![audio], vec![audio], lanes + at as u32));
        }
        for (id, group) in &self.now.lanes {
            let to = group.unwrap_or(NodeId::MASTER);
            self.patch.connect(Port::new(*id, 0), Port::new(to, 0));
        }
        for id in &self.now.groups {
            self.patch.connect(Port::new(*id, 0), Port::new(NodeId::MASTER, 0));
        }
        match kernel::compile(&self.patch) {
            Ok(plan) => {
                self.pool.fit(&plan);
                self.plan = plan;
                self.places.clear();
                self.places.extend(self.now.lanes.iter().enumerate().map(|(at, (id, _))| (*id, Place::Lane(at))));
                self.places.extend(self.now.groups.iter().enumerate().map(|(at, id)| (*id, Place::Group(at))));
                self.places.sort_unstable_by_key(|&(id, _)| id);
            }
            Err(error) => {
                // The wiring is made of nodes and cables that go one way, and every node has an id of its own (the model gives each one, and the controller sends it once), so this is
                // not a thing that happens: the plan that there was is kept.
                log::error!("the plan of the lanes and groups does not compile: {error}");
                debug_assert!(false, "the plan of the lanes and groups does not compile: {error}");
            }
        }
    }

    /// Run one block: the lanes, the groups and the mix.
    pub(super) fn run(&mut self, lanes: &mut [InstrumentLane], groups: &mut [Group], master: &mut Master, frames: usize, cx: &mut BlockCx<'_>) -> Result<()> {
        self.plan.run(&mut self.pool, frames, &mut Store { lanes, groups, master, places: &self.places }, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alloc_guard::allocations_in;

    fn lanes(n: usize) -> Vec<InstrumentLane> {
        (0..n).map(|_| InstrumentLane::new(2, super::test_node())).collect()
    }

    fn groups(n: usize) -> Vec<Group> {
        (0..n).map(|_| Group::new(2, super::test_node())).collect()
    }

    /// What a node is called, by where it is: `lane 1`, `group 0`, `mix`.
    fn name(id: NodeId, lanes: &[InstrumentLane], groups: &[Group]) -> String {
        if id == NodeId::MASTER {
            return "mix".to_string();
        }
        match (lanes.iter().position(|lane| lane.node == id), groups.iter().position(|group| group.node == id)) {
            (Some(at), None) => format!("lane {at}"),
            (None, Some(at)) => format!("group {at}"),
            other => panic!("a node that is one lane or one group, and not {other:?}"),
        }
    }

    /// The nodes of the plan in the order they run.
    fn order(schedule: &Schedule, lanes: &[InstrumentLane], groups: &[Group]) -> Vec<String> {
        schedule.plan.order().map(|id| name(id, lanes, groups)).collect()
    }

    /// Every feed of the plan, `(from, to)`: the node that makes it, and the node whose input it is added to (the first op after it that reads the buffer it goes into).
    fn feeds(schedule: &Schedule, lanes: &[InstrumentLane], groups: &[Group]) -> Vec<(String, String)> {
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

    /// A block of `frames` frames run through the plan as it is, with no notes and no telemetry.
    fn run(schedule: &mut Schedule, lanes: &mut [InstrumentLane], groups: &mut [Group], frames: usize) -> Vec<Vec<f32>> {
        let mut master = Master::new();
        let mut out = vec![vec![1.0f32; frames]; 2];
        let mut cx = BlockCx { midi: &[], audio_out: &mut out, num_channels: 2, scale: Default::default(), bpm: 120.0, telemetry: None, fall: None, learn_tx: None };
        schedule.run(lanes, groups, &mut master, frames, &mut cx).unwrap();
        out
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
    fn the_plan_names_its_nodes_by_the_ids_that_the_model_gave_them_and_the_mix_by_the_master() {
        let mut schedule = Schedule::new();
        let (lanes, groups) = (lanes(3), groups(2));
        schedule.refresh(&lanes, &groups, 2, 64);
        let ids: Vec<NodeId> = lanes.iter().map(|lane| lane.node).chain(groups.iter().map(|group| group.node)).chain([NodeId::MASTER]).collect();
        assert_eq!(schedule.plan.order().collect::<Vec<_>>(), ids);
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
        let refresh = |schedule: &mut Schedule, lanes: &[InstrumentLane], groups: &[Group], channels, frames| {
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
        lanes[1] = InstrumentLane::new(2, super::test_node());
        assert_eq!(refresh(&mut schedule, &lanes, &groups, 1, 64), 9, "a lane that is another at the same place");
        groups[0] = Group::new(2, super::test_node());
        assert_eq!(refresh(&mut schedule, &lanes, &groups, 1, 64), 10, "a group that is another at the same place");
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
        assert_eq!(schedule.plan.order().take(3).collect::<Vec<_>>(), lanes.iter().map(|lane| lane.node).collect::<Vec<_>>());
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
        assert_eq!(mix.node, NodeId::MASTER);
        let into_mix: Vec<String> = ops.iter().filter(|op| op.feeds.iter().any(|feed| feed.into == mix.inputs[0])).map(|op| name(op.node, &lanes, &groups)).collect();
        assert_eq!(into_mix, ["lane 1", "group 0"], "in the order they run");
        let group = ops.iter().find(|op| op.node == groups[0].node).unwrap();
        let into_group: Vec<String> = ops.iter().filter(|op| op.feeds.iter().any(|feed| feed.into == group.inputs[0] && op.node != NodeId::MASTER)).map(|op| name(op.node, &lanes, &groups)).collect();
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
        let out = run(&mut schedule, &mut lanes, &mut groups, 8);
        assert!(out.iter().flatten().all(|&s| s == 0.0), "the lane that is left has no instrument, and the one that went is silence");
    }

    #[test]
    fn a_lane_is_run_as_the_lane_it_is_and_not_as_the_one_that_was_at_its_place_when_the_plan_was_made() {
        // `run_node` leaves the gain that the lane is heard at where it was asked to go: the lane that ran is the one that has it.
        let mut schedule = Schedule::new();
        let (mut lanes, mut groups) = (lanes(2), groups(0));
        lanes[0].heard_goal = 0.25;
        lanes[1].heard_goal = 0.5;
        schedule.refresh(&lanes, &groups, 2, 8);
        lanes.remove(0);
        run(&mut schedule, &mut lanes, &mut groups, 8);
        assert_eq!(lanes[0].heard_at, 1.0, "the plan was made for a lane at that place that is gone, and this is another: it did not run, as the one that went");
        // Made again for the lane that is left, it runs.
        schedule.refresh(&lanes, &groups, 2, 8);
        run(&mut schedule, &mut lanes, &mut groups, 8);
        assert_eq!(lanes[0].heard_at, 0.5);
    }

    #[test]
    fn a_group_is_run_as_the_group_it_is_and_not_as_the_one_that_was_at_its_place_when_the_plan_was_made() {
        let mut schedule = Schedule::new();
        let (mut lanes, mut groups) = (lanes(0), groups(2));
        groups[0].heard_goal = 0.25;
        groups[1].heard_goal = 0.5;
        schedule.refresh(&lanes, &groups, 2, 8);
        groups.remove(0);
        run(&mut schedule, &mut lanes, &mut groups, 8);
        assert_eq!(groups[0].heard_at, 1.0, "the plan was made for a group at that place that is gone, and this is another: it did not run, as the one that went");
        schedule.refresh(&lanes, &groups, 2, 8);
        run(&mut schedule, &mut lanes, &mut groups, 8);
        assert_eq!(groups[0].heard_at, 0.5);
    }

    #[test]
    fn each_node_is_found_by_its_id_whatever_its_place_and_whatever_the_order_of_the_ids() {
        // Ids that go the other way from the places, and groups among them: every lane and every group runs once, at the place it is at.
        let mut schedule = Schedule::new();
        let (mut lanes, mut groups) = (lanes(4), groups(2));
        for (at, lane) in lanes.iter_mut().enumerate() {
            lane.node = NodeId(90 - 10 * at as u32);
            lane.heard_goal = 0.1 * (at + 1) as f32;
        }
        for (at, group) in groups.iter_mut().enumerate() {
            group.node = NodeId(45 - 20 * at as u32);
            group.heard_goal = 0.1 * (at + 6) as f32;
        }
        lanes[1].group = Some(0);
        lanes[3].group = Some(1);
        schedule.refresh(&lanes, &groups, 2, 8);
        run(&mut schedule, &mut lanes, &mut groups, 8);
        assert_eq!(lanes.iter().map(|lane| lane.heard_at).collect::<Vec<_>>(), [0.1, 0.2, 0.3, 0.4]);
        assert_eq!(groups.iter().map(|group| group.heard_at).collect::<Vec<_>>(), [0.6, 0.7]);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "the plan of the lanes and groups does not compile")]
    fn two_nodes_with_one_id_are_a_plan_that_does_not_compile() {
        let mut schedule = Schedule::new();
        let (mut lanes, groups) = (lanes(2), groups(0));
        lanes[1].node = lanes[0].node;
        schedule.refresh(&lanes, &groups, 2, 8);
    }
}
