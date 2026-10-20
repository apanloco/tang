//! The lane, the group and the output mix as nodes of a plan (P1 to P3a of `docs/patch-graph.md` §11.6).
//!
//! `AudioGraph` still has its lanes and groups in vectors and its commands name places, so the plan is made of what is there. A lane is a row of nodes, a *head* (the events through
//! range, remap, pattern and transpose, the modulators, the instrument and its volume), a node for each *effect* of its chain, and a *tail* (the metronome's click, the fade of a
//! mute or a solo, the meter, the learned controls); a group is the same (its head takes the sum of its members and has the volume, the tail has the pan, the fade and the meter);
//! and there is the output mix. The stages of a chain are `single` inputs, so each reads the buffer that the stage before made and nothing is added or copied between them. A
//! lane's tail is a cable to its group's head, or to the mix if it has no group, and a group's tail is a cable to the mix. Ranks are the places (the lanes in order, then the
//! groups, then the mix; all the nodes of a lane have the rank of the lane), so that the order of the plan, and so the order of every sum, is the order that the loop it replaced
//! had: a group's bus is the lanes added in the order of their places, and the mix is the lanes that have no group and then the groups. The nodes run the code that was in the loop,
//! cut where it was cut by what it did (`InstrumentLane::process` and `Group::finish` are still there, in test builds, as the specification that the oracle holds the nodes to).
//!
//! A node of the plan is named by the id that the model gave it (`NodeId`, which the creating commands carry: a lane's, a group's, an effect's) and what part of it that is, and the
//! mix by `Stage::Master`: the plan says the things that the model says. The places of the stages are made when the plan is (a sorted list that a block looks a node up in),
//! since the vectors are still addressed by place.
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

/// What a node of the plan is: a part of a lane or of a group, named by the id of the lane or the group (or of the effect), or the output mix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) enum Stage {
    /// The start of a lane (its id): the events, the modulators, the instrument and the volume. Or of a group (its id): the sum of its members, and the group's volume.
    Head(NodeId),
    /// An effect (its id) of a lane's chain or of a group's bus.
    Fx(NodeId),
    /// The end of a lane (its id): the metronome's click, the fade of a mute and a solo, the meter, the learned controls. Or of a group (its id): the pan, the fade, the meter.
    Tail(NodeId),
    /// The output mix.
    Master,
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

/// An effect of a chain as a node: it makes its output of what its input is, and the output is blended with the input by `mix` (the sound that was put in is the dry part).
fn run_effect(effect: &mut dyn Plugin, mix: f32, io: &mut Io, num_channels: usize) -> Result<()> {
    let frames = io.frames();
    let (inputs, outputs) = io.split();
    let (input, output) = (&inputs[0], &mut outputs[0]);
    {
        let mut in_s = [const { MaybeUninit::uninit() }; MAX_CHANNELS];
        let mut out_s = [const { MaybeUninit::uninit() }; MAX_CHANNELS];
        let in_refs = shared_slices(input, &mut in_s);
        let out_refs = mut_slices(output, &mut out_s);
        effect.process(&[], in_refs, out_refs)?;
    }
    if mix < 1.0 {
        let dry = 1.0 - mix;
        for ch in 0..num_channels {
            for i in 0..frames {
                output[ch][i] = input[ch][i] * dry + output[ch][i] * mix;
            }
        }
    }
    Ok(())
}

impl InstrumentLane {
    /// The head of the lane as a node: the notes are filtered by the range, remapped, played by the pattern and transposed, the modulators are ticked and applied to the chain, the
    /// instrument renders (into the buffer of the output, or into the lane's own, `inst_buf`, if it has more outputs than the chain has channels, and the first of them go on), and
    /// the volume and the tap after it are applied. A lane with no instrument makes silence (the buffer of the output is zero).
    fn run_head(&mut self, io: &mut Io, cx: &mut BlockCx<'_>) -> Result<()> {
        let num_channels = cx.num_channels;
        let out = &mut io.outputs_mut()[0];
        let frames = out.first().map_or(0, |buf| buf.len());

        // Filter MIDI by range, remap, let the pattern play, transpose.
        self.filter_midi(cx.midi);
        let effective_events: &[(u64, [u8; 3])] = if let Some(ref remapper) = self.remapper {
            remapper.remap_events(&self.filtered_midi, &mut self.remapped_events);
            &self.remapped_events
        } else {
            &self.filtered_midi
        };
        let effective_events = self.pattern.process(effective_events, frames, cx.scale);
        let effective_events = if self.transpose != 0 {
            self.transposed_events.clear();
            for &(frame, bytes) in effective_events {
                let status_type = bytes[0] & 0xF0;
                if matches!(status_type, 0x80 | 0x90) {
                    let note = bytes[1] as i16 + self.transpose as i16;
                    if (0..=127).contains(&note) {
                        self.transposed_events.push((frame, [bytes[0], note as u8, bytes[2]]));
                    }
                    // Drop notes that fall outside 0-127
                } else {
                    self.transposed_events.push((frame, bytes));
                }
            }
            self.transposed_events.as_slice()
        } else {
            effective_events
        };

        // Modulators (block-rate: once per buffer, before the chain renders): tick all, cross-mod, route each target to its slot's plugin (the instrument or an effect). Applying
        // them up front is the same as per slot, since each plugin reads its parameters when it processes, later in this buffer.
        if frames > 0 {
            for m in &mut self.modulators {
                m.tick(frames, effective_events, cx.bpm);
            }
            apply_cross_mod(&mut self.modulators, &mut self.cross_mod_scratch, cx.telemetry);
            apply_modulators_to_chain(&self.modulators, &mut self.instrument, &mut self.effects, &mut self.drive_scratch, cx.telemetry);
        }

        let Some(instrument) = self.instrument.as_mut() else { return Ok(()) };
        let outputs = self.inst_buf.len();
        if outputs <= num_channels {
            // The instrument is given a buffer for each of its outputs, which are the first of the output's: the others stay zero.
            let mut storage = [const { MaybeUninit::uninit() }; MAX_CHANNELS];
            let refs = mut_slices(&mut out[..outputs], &mut storage);
            instrument.process(effective_events, &[], refs)?;
            if (self.volume - 1.0).abs() >= f32::EPSILON {
                for channel in out.iter_mut().take(outputs) {
                    for sample in channel.iter_mut() {
                        *sample *= self.volume;
                    }
                }
            }
        } else {
            // More outputs than the chain has channels (Pianoteq has sixteen): the first stereo pair goes on and the rest are dropped.
            for buf in self.inst_buf.iter_mut() {
                buf.resize(frames, 0.0);
                buf.fill(0.0);
            }
            {
                let mut storage = [const { MaybeUninit::uninit() }; MAX_CHANNELS];
                let refs = mut_slices(&mut self.inst_buf, &mut storage);
                instrument.process(effective_events, &[], refs)?;
            }
            if (self.volume - 1.0).abs() >= f32::EPSILON {
                for channel in self.inst_buf.iter_mut().take(num_channels) {
                    for sample in channel.iter_mut() {
                        *sample *= self.volume;
                    }
                }
            }
            for (channel, buf) in out.iter_mut().zip(&self.inst_buf) {
                channel.copy_from_slice(buf);
            }
        }
        store_taps(&self.taps, cx.telemetry, TapStage::Output, out);
        Ok(())
    }

    /// An effect of the lane's chain as a node (its place in the chain is `index`). The effects of a lane that has no instrument do not run, and what they make is silence.
    fn run_fx(&mut self, index: usize, io: &mut Io, cx: &mut BlockCx<'_>) -> Result<()> {
        if self.instrument.is_none() {
            return Ok(());
        }
        let (Some(effect), Some(&mix)) = (self.effects.get_mut(index), self.mix_values.get(index)) else { return Ok(()) };
        run_effect(effect.as_mut(), mix as f32, io, cx.num_channels)?;
        store_taps(&self.taps, cx.telemetry, TapStage::Effect(index), &io.outputs()[0]);
        Ok(())
    }

    /// The tail of the lane as a node: the metronome's click goes on top of what the chain made, the lane is let in or out by its mute and solo (its group's mute is the bus's), it
    /// is measured, and it is read for a control that was learned. `position` is where the lane is among the lanes, which is what a notification of a learned control says. What
    /// the plan then does is add the lane's sound to the bus of its group or to the mix.
    fn run_tail(&mut self, io: &mut Io, cx: &mut BlockCx<'_>, position: usize) -> Result<()> {
        let frames = io.frames();
        let out = &mut io.inputs_mut()[0];
        self.pattern.render_metronome(out, frames);
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
        io.forward(0, 0);
        Ok(())
    }
}

impl Group {
    /// The head of the group as a node: its input is the sum of its members, the bus, which the group's volume is applied to in place.
    fn run_head(&mut self, io: &mut Io, cx: &mut BlockCx<'_>) -> Result<()> {
        let bus = &mut io.inputs_mut()[0];
        if (self.volume - 1.0).abs() >= f32::EPSILON {
            for channel in bus.iter_mut().take(cx.num_channels) {
                for sample in channel.iter_mut() {
                    *sample *= self.volume;
                }
            }
        }
        io.forward(0, 0);
        Ok(())
    }

    /// An effect of the group's bus as a node (its place in the chain is `index`).
    fn run_fx(&mut self, index: usize, io: &mut Io, cx: &mut BlockCx<'_>) -> Result<()> {
        let (Some(effect), Some(&mix)) = (self.effects.get_mut(index), self.mix_values.get(index)) else { return Ok(()) };
        run_effect(effect.as_mut(), mix as f32, io, cx.num_channels)?;
        store_taps(&self.taps, cx.telemetry, TapStage::Effect(index), &io.outputs()[0]);
        Ok(())
    }

    /// The tail of the group as a node: the pan, the tap after it, the group is let in or out by its mute and the solos, and it is measured. What a bus is heard at: a group that
    /// is not muted is heard when nothing is soloed, when it is, and when a lane in it is.
    fn run_tail(&mut self, io: &mut Io, cx: &mut BlockCx<'_>) -> Result<()> {
        let bus = &mut io.inputs_mut()[0];
        self.apply_pan(bus, cx.num_channels);
        store_taps(&self.taps, cx.telemetry, TapStage::Output, bus);
        fade(bus, cx.num_channels, self.heard_at, self.heard_goal);
        self.heard_at = self.heard_goal;
        if let (Some(telemetry), Some(fall), Some(meter)) = (cx.telemetry, cx.fall, self.meter.as_mut()) {
            meter.update(telemetry, bus, fall);
        }
        io.forward(0, 0);
        Ok(())
    }
}

/// Where a node of the plan is kept: a lane or a group at a place of its vector, or an effect at a place of the chain of one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Place {
    Lane(usize),
    LaneFx(usize, usize),
    Group(usize),
    GroupFx(usize, usize),
}

/// The nodes of the plan, by name: what `Plan::run` asks for a block of.
struct Store<'a> {
    lanes: &'a mut [InstrumentLane],
    groups: &'a mut [Group],
    master: &'a mut Master,
    /// Where each node was when the plan was made, in the order of the stages.
    places: &'a [(Stage, Place)],
}

impl<'a, 'cx> Nodes<Stage, BlockCx<'cx>> for Store<'a> {
    fn process(&mut self, stage: Stage, io: &mut Io, cx: &mut BlockCx<'cx>) -> Result<()> {
        if stage == Stage::Master {
            return self.master.run_node(io, cx);
        }
        let Ok(found) = self.places.binary_search_by_key(&stage, |&(stage, _)| stage) else { return Ok(()) };
        // A node that is not at its place any more (the lane that was there is gone: the plan is made again before the next block) makes silence, and is not taken for the one
        // that is at its place now.
        match (stage, self.places[found].1) {
            (Stage::Head(id), Place::Lane(at)) => match self.lanes.get_mut(at) {
                Some(lane) if lane.node == id => lane.run_head(io, cx),
                _ => Ok(()),
            },
            (Stage::Tail(id), Place::Lane(at)) => match self.lanes.get_mut(at) {
                Some(lane) if lane.node == id => lane.run_tail(io, cx, at),
                _ => Ok(()),
            },
            (Stage::Fx(id), Place::LaneFx(at, index)) => match self.lanes.get_mut(at) {
                Some(lane) if lane.effect_nodes.get(index) == Some(&id) => lane.run_fx(index, io, cx),
                _ => Ok(()),
            },
            (Stage::Head(id), Place::Group(at)) => match self.groups.get_mut(at) {
                Some(group) if group.node == id => group.run_head(io, cx),
                _ => Ok(()),
            },
            (Stage::Tail(id), Place::Group(at)) => match self.groups.get_mut(at) {
                Some(group) if group.node == id => group.run_tail(io, cx),
                _ => Ok(()),
            },
            (Stage::Fx(id), Place::GroupFx(at, index)) => match self.groups.get_mut(at) {
                Some(group) if group.effect_nodes.get(index) == Some(&id) => group.run_fx(index, io, cx),
                _ => Ok(()),
            },
            _ => Ok(()),
        }
    }
}

/// How the lanes and groups are wired, as far as the plan is concerned, as a list of numbers: for each lane its id, the id of its group (none if it has none, or if the group it
/// names is not there) and the ids of its effects, and for each group its id and the ids of its bus effects, in the order of their places. Two shapes that are the same list are
/// the same wiring, and the list is read into room that is kept, so that reading it asks the allocator for nothing.
#[derive(Default, PartialEq)]
struct Shape {
    channels: usize,
    list: Vec<u64>,
}

const LANE: u64 = 1 << 32;
const GROUP_OF: u64 = 2 << 32;
const GROUP: u64 = 3 << 32;
const EFFECT: u64 = 4 << 32;
const NO_GROUP: u64 = u32::MAX as u64;

impl Shape {
    /// Make this the same as `other`, using the room it has.
    fn copy_from(&mut self, other: &Shape) {
        self.channels = other.channels;
        self.list.clone_from(&other.list);
    }

    /// Read the wiring of `lanes` and `groups` into this, keeping the room it has.
    fn read(&mut self, lanes: &[InstrumentLane], groups: &[Group], channels: usize) {
        self.channels = channels;
        self.list.clear();
        for lane in lanes {
            self.list.push(LANE | u64::from(lane.node.0));
            self.list.push(GROUP_OF | lane.group.and_then(|g| groups.get(g)).map_or(NO_GROUP, |group| u64::from(group.node.0)));
            self.list.extend(lane.effect_nodes.iter().map(|effect| EFFECT | u64::from(effect.0)));
        }
        for group in groups {
            self.list.push(GROUP | u64::from(group.node.0));
            self.list.extend(group.effect_nodes.iter().map(|effect| EFFECT | u64::from(effect.0)));
        }
    }
}

/// The plan that the lanes, groups and mix are run by, the buffers it runs over, and what it was made for.
pub(super) struct Schedule {
    plan: Plan<Stage>,
    pool: Pool,
    /// Where the plan's nodes are kept, by stage: made with the plan.
    places: Vec<(Stage, Place)>,
    /// The wiring that `plan` was made for, and room to read the wiring that there is now into.
    shape: Shape,
    now: Shape,
    /// The patch that is made again and compiled when the wiring changes, kept so that its room is.
    patch: Patch<Stage>,
    /// How many frames of a block the pool has room for.
    room: usize,
    /// How many times a plan has been made, for the tests to see that a block that finds the wiring as it was makes none.
    #[cfg(test)]
    compiles: usize,
}

impl Schedule {
    pub(super) fn new() -> Self {
        let plan = kernel::compile(&Patch::<Stage>::new()).expect("a patch with nothing in it compiles");
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
            self.compile(lanes, groups, channels);
            // Copied, not swapped: both keep their room, so that reading the wiring of the next block asks for none.
            self.shape.copy_from(&self.now);
        }
        if frames > self.room {
            self.pool.reserve(frames);
            self.room = frames;
        }
    }

    /// The plan for the lanes and groups: the mix, a row of nodes for each lane and each group, and the cables.
    fn compile(&mut self, lanes: &[InstrumentLane], groups: &[Group], channels: usize) {
        #[cfg(test)]
        {
            self.compiles += 1;
        }
        let audio = PortSpec::audio(channels as u16);
        let chain = PortSpec::single(channels as u16);
        let lane_count = lanes.len() as u32;
        self.patch.clear();
        self.patch.add_node(Stage::Master, NodeSpec::new(vec![audio], vec![], u32::MAX));
        for (at, lane) in lanes.iter().enumerate() {
            let rank = at as u32;
            self.patch.add_node(Stage::Head(lane.node), NodeSpec::new(vec![], vec![audio], rank));
            for effect in &lane.effect_nodes {
                self.patch.add_node(Stage::Fx(*effect), NodeSpec::new(vec![chain], vec![audio], rank));
            }
            self.patch.add_node(Stage::Tail(lane.node), NodeSpec::new(vec![chain], vec![audio], rank));
        }
        for (at, group) in groups.iter().enumerate() {
            let rank = lane_count + at as u32;
            // The head of a group takes the sum of its members, so it is an input that is a sum.
            self.patch.add_node(Stage::Head(group.node), NodeSpec::new(vec![audio], vec![audio], rank));
            for effect in &group.effect_nodes {
                self.patch.add_node(Stage::Fx(*effect), NodeSpec::new(vec![chain], vec![audio], rank));
            }
            self.patch.add_node(Stage::Tail(group.node), NodeSpec::new(vec![chain], vec![audio], rank));
        }
        for lane in lanes {
            let mut from = Stage::Head(lane.node);
            for effect in &lane.effect_nodes {
                self.patch.connect(Port::new(from, 0), Port::new(Stage::Fx(*effect), 0));
                from = Stage::Fx(*effect);
            }
            self.patch.connect(Port::new(from, 0), Port::new(Stage::Tail(lane.node), 0));
            let to = lane.group.and_then(|g| groups.get(g)).map_or(Stage::Master, |group| Stage::Head(group.node));
            self.patch.connect(Port::new(Stage::Tail(lane.node), 0), Port::new(to, 0));
        }
        for group in groups {
            let mut from = Stage::Head(group.node);
            for effect in &group.effect_nodes {
                self.patch.connect(Port::new(from, 0), Port::new(Stage::Fx(*effect), 0));
                from = Stage::Fx(*effect);
            }
            self.patch.connect(Port::new(from, 0), Port::new(Stage::Tail(group.node), 0));
            self.patch.connect(Port::new(Stage::Tail(group.node), 0), Port::new(Stage::Master, 0));
        }
        match kernel::compile(&self.patch) {
            Ok(plan) => {
                self.pool.fit(&plan);
                self.plan = plan;
                self.places.clear();
                for (at, lane) in lanes.iter().enumerate() {
                    self.places.push((Stage::Head(lane.node), Place::Lane(at)));
                    self.places.push((Stage::Tail(lane.node), Place::Lane(at)));
                    self.places.extend(lane.effect_nodes.iter().enumerate().map(|(index, effect)| (Stage::Fx(*effect), Place::LaneFx(at, index))));
                }
                for (at, group) in groups.iter().enumerate() {
                    self.places.push((Stage::Head(group.node), Place::Group(at)));
                    self.places.push((Stage::Tail(group.node), Place::Group(at)));
                    self.places.extend(group.effect_nodes.iter().enumerate().map(|(index, effect)| (Stage::Fx(*effect), Place::GroupFx(at, index))));
                }
                self.places.sort_unstable_by_key(|&(stage, _)| stage);
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
        (0..n).map(|_| InstrumentLane::new(super::test_node())).collect()
    }

    fn groups(n: usize) -> Vec<Group> {
        (0..n).map(|_| Group::new(super::test_node())).collect()
    }

    fn effect() -> Box<dyn Plugin> {
        crate::plugin::load("builtin:filter", 48_000.0, 256, &Default::default()).unwrap()
    }

    /// Give a lane `n` effects, each with an id of its own.
    fn with_effects(lane: &mut InstrumentLane, n: usize) {
        for _ in 0..n {
            lane.effects.push(effect());
            lane.mix_values.push(1.0);
            lane.effect_nodes.push(super::test_node());
        }
    }

    /// Give a group a bus of `n` effects, each with an id of its own.
    fn with_bus_effects(group: &mut Group, n: usize) {
        for _ in 0..n {
            group.effects.push(effect());
            group.mix_values.push(1.0);
            group.effect_nodes.push(super::test_node());
        }
    }

    /// What a node is called, by where it is: `lane 1 head`, `lane 1 fx 0`, `lane 1 tail`, `group 0 head`, `group 0 fx 1`, `group 0 tail`, `mix`.
    fn name(stage: Stage, lanes: &[InstrumentLane], groups: &[Group]) -> String {
        let owner = |id: NodeId| match (lanes.iter().position(|lane| lane.node == id), groups.iter().position(|group| group.node == id)) {
            (Some(at), None) => format!("lane {at}"),
            (None, Some(at)) => format!("group {at}"),
            other => panic!("a node that is one lane or one group, and not {other:?}"),
        };
        match stage {
            Stage::Master => "mix".to_string(),
            Stage::Head(id) => format!("{} head", owner(id)),
            Stage::Tail(id) => format!("{} tail", owner(id)),
            Stage::Fx(id) => {
                let in_lane = lanes.iter().enumerate().find_map(|(at, lane)| lane.effect_nodes.iter().position(|effect| *effect == id).map(|index| format!("lane {at} fx {index}")));
                let in_group = groups.iter().enumerate().find_map(|(at, group)| group.effect_nodes.iter().position(|effect| *effect == id).map(|index| format!("group {at} fx {index}")));
                in_lane.or(in_group).expect("an effect that is in a chain")
            }
        }
    }

    /// The nodes of the plan in the order they run.
    fn order(schedule: &Schedule, lanes: &[InstrumentLane], groups: &[Group]) -> Vec<String> {
        schedule.plan.order().map(|stage| name(stage, lanes, groups)).collect()
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

    /// A block of `frames` frames run through the plan as it is, with the notes given and no telemetry: what the mix has.
    fn run_with(schedule: &mut Schedule, lanes: &mut [InstrumentLane], groups: &mut [Group], midi: &[(u64, [u8; 3])], frames: usize) -> Vec<Vec<f32>> {
        let mut master = Master::new();
        let mut out = vec![vec![1.0f32; frames]; 2];
        let mut cx = BlockCx { midi, audio_out: &mut out, num_channels: 2, scale: Default::default(), bpm: 120.0, telemetry: None, fall: None, learn_tx: None };
        schedule.run(lanes, groups, &mut master, frames, &mut cx).unwrap();
        out
    }

    fn run(schedule: &mut Schedule, lanes: &mut [InstrumentLane], groups: &mut [Group], frames: usize) -> Vec<Vec<f32>> {
        run_with(schedule, lanes, groups, &[], frames)
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
        assert_eq!(
            order(&schedule, &lanes, &groups),
            ["lane 0 head", "lane 0 tail", "lane 1 head", "lane 1 tail", "lane 2 head", "lane 2 tail", "group 0 head", "group 0 tail", "group 1 head", "group 1 tail", "mix"]
        );
    }

    #[test]
    fn a_lane_is_a_head_its_effects_in_the_order_of_its_chain_and_a_tail_and_a_group_is_the_same_with_its_bus() {
        let mut schedule = Schedule::new();
        let (mut lanes, mut groups) = (lanes(2), groups(1));
        with_effects(&mut lanes[0], 2);
        with_effects(&mut lanes[1], 1);
        with_bus_effects(&mut groups[0], 3);
        lanes[1].group = Some(0);
        schedule.refresh(&lanes, &groups, 2, 64);
        assert_eq!(
            order(&schedule, &lanes, &groups),
            [
                "lane 0 head",
                "lane 0 fx 0",
                "lane 0 fx 1",
                "lane 0 tail",
                "lane 1 head",
                "lane 1 fx 0",
                "lane 1 tail",
                "group 0 head",
                "group 0 fx 0",
                "group 0 fx 1",
                "group 0 fx 2",
                "group 0 tail",
                "mix"
            ]
        );
    }

    #[test]
    fn the_stages_of_a_chain_read_the_buffer_that_the_stage_before_made_and_nothing_is_added_between_them() {
        let mut schedule = Schedule::new();
        let (mut lanes, mut groups) = (lanes(1), groups(1));
        with_effects(&mut lanes[0], 2);
        with_bus_effects(&mut groups[0], 1);
        lanes[0].group = Some(0);
        schedule.refresh(&lanes, &groups, 2, 64);
        let ops = schedule.plan.ops();
        let at = |stage: &str| order(&schedule, &lanes, &groups).iter().position(|name| name == stage).unwrap();
        // The lane's chain: head → fx 0 → fx 1 → tail, each the buffer of the one before; and the group's, after its head, which takes a sum.
        for (before, after) in [("lane 0 head", "lane 0 fx 0"), ("lane 0 fx 0", "lane 0 fx 1"), ("lane 0 fx 1", "lane 0 tail"), ("group 0 head", "group 0 fx 0"), ("group 0 fx 0", "group 0 tail")] {
            let (before, after) = (&ops[at(before)], &ops[at(after)]);
            assert_eq!(after.inputs[0], before.outputs[0], "the buffer of the stage before");
            assert!(before.feeds.is_empty(), "nothing is fed to it");
        }
        // What joins the chains is a sum, fed: the lane's tail to the group's head, and the group's tail to the mix.
        assert_eq!(feeds(&schedule, &lanes, &groups), pairs(&[("lane 0 tail", "group 0 head"), ("group 0 tail", "mix")]));
    }

    #[test]
    fn every_node_of_a_lane_has_the_rank_of_its_place_and_every_node_of_a_group_the_rank_of_its_place_after_the_lanes() {
        let mut schedule = Schedule::new();
        let (mut lanes, mut groups) = (lanes(3), groups(2));
        with_effects(&mut lanes[1], 2);
        with_bus_effects(&mut groups[1], 1);
        schedule.refresh(&lanes, &groups, 2, 64);
        let rank = |stage: Stage| schedule.patch.nodes().iter().find(|(id, _)| *id == stage).map(|(_, spec)| spec.rank).expect("a node of the plan");
        for (at, lane) in lanes.iter().enumerate() {
            let stages = [Stage::Head(lane.node), Stage::Tail(lane.node)].into_iter().chain(lane.effect_nodes.iter().map(|effect| Stage::Fx(*effect)));
            for stage in stages {
                assert_eq!(rank(stage), at as u32, "a node of lane {at}");
            }
        }
        for (at, group) in groups.iter().enumerate() {
            let stages = [Stage::Head(group.node), Stage::Tail(group.node)].into_iter().chain(group.effect_nodes.iter().map(|effect| Stage::Fx(*effect)));
            for stage in stages {
                assert_eq!(rank(stage), 3 + at as u32, "a node of group {at}");
            }
        }
        assert_eq!(rank(Stage::Master), u32::MAX);
    }

    #[test]
    fn the_plan_names_its_nodes_by_the_ids_that_the_model_gave_them_and_the_mix_by_the_master() {
        let mut schedule = Schedule::new();
        let (mut lanes, mut groups) = (lanes(2), groups(1));
        with_effects(&mut lanes[1], 2);
        with_bus_effects(&mut groups[0], 1);
        schedule.refresh(&lanes, &groups, 2, 64);
        let (lane_a, lane_b, group) = (lanes[0].node, lanes[1].node, groups[0].node);
        let (e0, e1, bus) = (lanes[1].effect_nodes[0], lanes[1].effect_nodes[1], groups[0].effect_nodes[0]);
        let expected = [
            Stage::Head(lane_a),
            Stage::Tail(lane_a),
            Stage::Head(lane_b),
            Stage::Fx(e0),
            Stage::Fx(e1),
            Stage::Tail(lane_b),
            Stage::Head(group),
            Stage::Fx(bus),
            Stage::Tail(group),
            Stage::Master,
        ];
        assert_eq!(schedule.plan.order().collect::<Vec<_>>(), expected);
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
            pairs(&[("lane 0 tail", "group 1 head"), ("lane 1 tail", "mix"), ("lane 2 tail", "group 0 head"), ("group 0 tail", "mix"), ("group 1 tail", "mix")])
        );
    }

    #[test]
    fn a_lane_that_names_a_group_that_is_not_there_feeds_the_mix() {
        let mut schedule = Schedule::new();
        let (mut lanes, groups) = (lanes(2), groups(1));
        lanes[0].group = Some(5);
        lanes[1].group = Some(0);
        schedule.refresh(&lanes, &groups, 2, 64);
        assert_eq!(feeds(&schedule, &lanes, &groups), pairs(&[("lane 0 tail", "mix"), ("lane 1 tail", "group 0 head"), ("group 0 tail", "mix")]));
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
        lanes.push(InstrumentLane::new(super::test_node()));
        assert_eq!(refresh(&mut schedule, &lanes, &groups, 2, 64), 4, "a lane that comes");
        lanes.remove(3);
        assert_eq!(refresh(&mut schedule, &lanes, &groups, 2, 64), 5, "a lane that goes");
        groups.push(Group::new(super::test_node()));
        assert_eq!(refresh(&mut schedule, &lanes, &groups, 2, 64), 6, "a group that comes");
        groups.remove(1);
        assert_eq!(refresh(&mut schedule, &lanes, &groups, 2, 64), 7, "a group that goes");
        assert_eq!(refresh(&mut schedule, &lanes, &groups, 1, 64), 8, "buffers of another width");
        lanes[1] = InstrumentLane::new(super::test_node());
        assert_eq!(refresh(&mut schedule, &lanes, &groups, 1, 64), 9, "a lane that is another at the same place");
        groups[0] = Group::new(super::test_node());
        assert_eq!(refresh(&mut schedule, &lanes, &groups, 1, 64), 10, "a group that is another at the same place");
        with_effects(&mut lanes[1], 2);
        assert_eq!(refresh(&mut schedule, &lanes, &groups, 1, 64), 11, "effects that come to a lane");
        lanes[1].mix_values[0] = 0.3;
        assert_eq!(refresh(&mut schedule, &lanes, &groups, 1, 64), 11, "a mix is not wiring");
        lanes[1].effect_nodes.swap(0, 1);
        lanes[1].effects.swap(0, 1);
        assert_eq!(refresh(&mut schedule, &lanes, &groups, 1, 64), 12, "two effects that change places");
        lanes[1].effect_nodes[0] = super::test_node();
        assert_eq!(refresh(&mut schedule, &lanes, &groups, 1, 64), 13, "an effect that is another at the same place");
        lanes[1].effects.remove(1);
        lanes[1].mix_values.remove(1);
        lanes[1].effect_nodes.remove(1);
        assert_eq!(refresh(&mut schedule, &lanes, &groups, 1, 64), 14, "an effect that goes");
        with_bus_effects(&mut groups[0], 1);
        assert_eq!(refresh(&mut schedule, &lanes, &groups, 1, 64), 15, "an effect that comes to a bus");
        groups[0].effect_nodes[0] = super::test_node();
        assert_eq!(refresh(&mut schedule, &lanes, &groups, 1, 64), 16, "a bus effect that is another at the same place");
    }

    #[test]
    fn lanes_that_swap_places_are_run_in_their_new_places_and_their_groups_go_with_them() {
        let mut schedule = Schedule::new();
        let (mut lanes, groups) = (lanes(3), groups(2));
        lanes[0].group = Some(1);
        lanes[1].group = Some(0);
        schedule.refresh(&lanes, &groups, 2, 64);
        let before = feeds(&schedule, &lanes, &groups);
        assert_eq!(before[..3], pairs(&[("lane 0 tail", "group 1 head"), ("lane 1 tail", "group 0 head"), ("lane 2 tail", "mix")])[..]);
        lanes.swap(0, 1);
        schedule.refresh(&lanes, &groups, 2, 64);
        let after = feeds(&schedule, &lanes, &groups);
        assert_eq!(after[..3], pairs(&[("lane 0 tail", "group 0 head"), ("lane 1 tail", "group 1 head"), ("lane 2 tail", "mix")])[..]);
        let heads: Vec<Stage> = schedule.plan.order().filter(|stage| matches!(stage, Stage::Head(id) if lanes.iter().any(|lane| lane.node == *id))).collect();
        assert_eq!(heads, lanes.iter().map(|lane| Stage::Head(lane.node)).collect::<Vec<_>>());
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
        assert_eq!(mix.node, Stage::Master);
        let into_mix: Vec<String> = ops.iter().filter(|op| op.feeds.iter().any(|feed| feed.into == mix.inputs[0])).map(|op| name(op.node, &lanes, &groups)).collect();
        assert_eq!(into_mix, ["lane 1 tail", "group 0 tail"], "in the order they run");
        let group = ops.iter().find(|op| op.node == Stage::Head(groups[0].node)).unwrap();
        let into_group: Vec<String> = ops.iter().filter(|op| op.feeds.iter().any(|feed| feed.into == group.inputs[0])).map(|op| name(op.node, &lanes, &groups)).collect();
        assert_eq!(into_group, ["lane 0 tail", "lane 2 tail"]);
    }

    #[test]
    fn reading_the_wiring_of_the_next_block_asks_the_allocator_for_nothing_not_even_right_after_a_plan_was_made() {
        let mut schedule = Schedule::new();
        let (mut lanes, mut groups) = (lanes(4), groups(2));
        lanes[1].group = Some(1);
        with_effects(&mut lanes[0], 3);
        with_effects(&mut lanes[2], 1);
        with_bus_effects(&mut groups[1], 2);
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
        with_effects(&mut lanes[0], 2);
        with_bus_effects(&mut groups[0], 1);
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
        // `run_tail` leaves the gain that the lane is heard at where it was asked to go: the lane that ran is the one that has it.
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

    /// A lane with the built-in oscillator and `n` effects of the built-in filter (open, so that the sound goes through), which a note on is heard from.
    fn sounding_lane(n: usize) -> InstrumentLane {
        let mut lane = InstrumentLane::new(super::test_node());
        let osc = crate::plugin::load("builtin:osc", 48_000.0, 256, &Default::default()).unwrap();
        lane.inst_buf = (0..osc.audio_output_count()).map(|_| Vec::new()).collect();
        lane.instrument = Some(osc);
        with_effects(&mut lane, n);
        lane
    }

    const NOTE_ON: [(u64, [u8; 3]); 1] = [(0, [0x90, 60, 100])];

    fn loudest(out: &[Vec<f32>]) -> f32 {
        out.iter().flatten().fold(0.0f32, |most, sample| most.max(sample.abs()))
    }

    #[test]
    fn a_note_is_heard_through_the_effects_of_the_chain_and_the_plan_is_what_makes_it_so() {
        let mut schedule = Schedule::new();
        let (mut lanes, mut groups) = (vec![sounding_lane(2)], groups(0));
        schedule.refresh(&lanes, &groups, 2, 256);
        assert!(loudest(&run_with(&mut schedule, &mut lanes, &mut groups, &NOTE_ON, 256)) > 0.01);
    }

    #[test]
    fn an_effect_that_was_taken_out_or_is_another_since_the_plan_was_made_is_not_run_in_the_place_of_the_one_that_was_there() {
        let mut schedule = Schedule::new();
        let (mut lanes, mut groups) = (vec![sounding_lane(1)], groups(0));
        schedule.refresh(&lanes, &groups, 2, 256);
        // Another effect at its place, with an id of its own: the stage that the plan has is silence (nothing is made for what comes after it).
        lanes[0].effect_nodes[0] = super::test_node();
        assert_eq!(loudest(&run_with(&mut schedule, &mut lanes, &mut groups, &NOTE_ON, 256)), 0.0);
        // Made again for what is there, it is heard.
        schedule.refresh(&lanes, &groups, 2, 256);
        assert!(loudest(&run_with(&mut schedule, &mut lanes, &mut groups, &NOTE_ON, 256)) > 0.01);
    }

    #[test]
    fn a_bus_effect_that_is_another_since_the_plan_was_made_is_not_run_in_the_place_of_the_one_that_was_there() {
        let mut schedule = Schedule::new();
        let (mut lanes, mut groups) = (vec![sounding_lane(0)], groups(1));
        lanes[0].group = Some(0);
        with_bus_effects(&mut groups[0], 1);
        schedule.refresh(&lanes, &groups, 2, 256);
        assert!(loudest(&run_with(&mut schedule, &mut lanes, &mut groups, &NOTE_ON, 256)) > 0.01, "the premise: it is heard through the bus's effect");
        groups[0].effect_nodes[0] = super::test_node();
        assert_eq!(loudest(&run_with(&mut schedule, &mut lanes, &mut groups, &NOTE_ON, 256)), 0.0);
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
        with_effects(&mut lanes[2], 2);
        with_bus_effects(&mut groups[0], 1);
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

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "the plan of the lanes and groups does not compile")]
    fn two_effects_with_one_id_are_a_plan_that_does_not_compile() {
        let mut schedule = Schedule::new();
        let (mut lanes, groups) = (lanes(1), groups(0));
        with_effects(&mut lanes[0], 2);
        lanes[0].effect_nodes[1] = lanes[0].effect_nodes[0];
        schedule.refresh(&lanes, &groups, 2, 8);
    }
}
