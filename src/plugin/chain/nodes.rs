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
//! **Latency** (P3f): a plugin says how late what it makes is (`Plugin::latency`), and where cables are added at an input, the plan has a delay on those that are early, so that the paths
//! are in step: `kernel::align` finds how much each cable is to be delayed by, from the latencies of the nodes (an instrument's, in its lane's head, and an effect's), and `kernel::with_delays`
//! puts a node on each (`Stage::Delay`, whose line is kept by the schedule under the two nodes it joins, so that it goes on through a plan that is made again). The dry part of an effect that
//! is blended with its output by its `mix` is put in step with the output the same way (a line of the effect's own, kept under its id). A session in which nothing has latency has no
//! delays, and is what it was, bit for bit.
//!
//! A node of the plan is named by the id that the model gave it (`NodeId`, which the creating commands carry: a lane's, a group's, an effect's) and what part of it that is, and the
//! mix by `Stage::Master`: the plan says the things that the model says. The places of the stages are made when the plan is (a sorted list that a block looks a node up in),
//! since the vectors are still addressed by place.
//!
//! The plan is made again when the wiring is not the one it was made for. That is found by reading the wiring into a `Shape` before every block and comparing it with the one the
//! plan was made for (a few comparisons of ids: a knob that is turned compiles nothing), so a lane that a test pushes into the vector is found as well as one that a command
//! puts there.

use anyhow::Result;
use kernel::{DelayLine, Io, NodeSpec, Nodes, Patch, Plan, Pool, Port, PortSpec};

use super::*;

/// What every node is told of the block besides its buffers.
pub(super) struct BlockCx<'a> {
    /// The notes and controllers that came in, with their frames in this block and the sources they came from.
    pub midi: Events<'a>,
    /// Where the output mix goes.
    pub audio_out: &'a mut [Vec<f32>],
    pub num_channels: usize,
    pub scale: ScaleSetting,
    /// Where what is measured is published, and how far a meter's peak falls over this block, if anything is measured.
    pub telemetry: Option<&'a Telemetry>,
    pub fall: Option<f32>,
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
    /// An audio cable that a person drew (its id): the node that its level is made by.
    Cable(EdgeId),
    /// A delay that the plan has put on a cable (its number among the plan's delays), so that the paths that are added where the cable ends are in step.
    Delay(u32),
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

/// An effect of a chain as a node: it makes its output of what its input is, and the output is blended with the input by `mix` (the sound that was put in is the dry part). An effect
/// that has latency makes its output late, so the dry part goes through `dry`, a line of as many frames, to be as late (P3f): the line is run whatever the `mix` is, so that it has
/// what was put in when the `mix` is turned down. An effect that has a second input has a second port in the plan (step 95): `side_patched` says whether a cable ends there, and the
/// plugin is given what the second port has if it does, and told that nothing is patched if not (it may have a sound of its own in its place).
fn run_effect(effect: &mut dyn Plugin, mix: f32, dry: Option<&mut DelayLine>, side_patched: bool, io: &mut Io, num_channels: usize) -> Result<()> {
    let frames = io.frames();
    let (inputs, outputs) = io.split();
    let (first, rest) = inputs.split_at_mut(1);
    let (input, output) = (&mut first[0], &mut outputs[0]);
    {
        let mut in_s = [const { MaybeUninit::uninit() }; MAX_CHANNELS];
        let mut out_s = [const { MaybeUninit::uninit() }; MAX_CHANNELS];
        let in_refs = shared_slices(input, &mut in_s);
        let out_refs = mut_slices(output, &mut out_s);
        match rest.first() {
            Some(side) => {
                let mut side_s = [const { MaybeUninit::uninit() }; MAX_CHANNELS];
                let side_refs = shared_slices(side, &mut side_s);
                effect.process_side(&[], in_refs, side_patched.then_some(side_refs), out_refs)?;
            }
            None => effect.process(&[], in_refs, out_refs)?,
        }
    }
    // (The input is the node's own to use as scratch once the plugin has read it.)
    if let Some(line) = dry {
        line.run(input);
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
    /// The head of the lane as a node: the notes are filtered by the range, remapped, played by the pattern and transposed, the
    /// instrument renders (into the buffer of the output, or into the lane's own, `inst_buf`, if it has more outputs than the chain has channels, and the first of them go on), and
    /// the volume and the tap after it are applied. A lane with no instrument makes silence (the buffer of the output is zero).
    fn run_head(&mut self, io: &mut Io, cx: &mut BlockCx<'_>) -> Result<()> {
        let num_channels = cx.num_channels;
        let out = &mut io.outputs_mut()[0];
        let frames = out.first().map_or(0, |buf| buf.len());

        // Filter MIDI by range, remap, let the pattern play, transpose (unless a modulator that hears the lane had it done ahead of the head: `prepare_events`).
        if !self.prepared {
            self.prepare_events(cx.midi, frames, cx.scale);
        }
        self.prepared = false;
        let effective_events: &[(u64, [u8; 3])] = &self.heard;

        let Some(instrument) = self.instrument.as_mut() else {
            self.head_at = self.head_goal;
            return Ok(());
        };
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
        // The head gate: a lane that cables end in puts none of its own sound into its chain while something else is soloed, so that what it puts out is what it was sent (every other
        // lane has the gain 1, which costs nothing). The cables that start at the instrument have the same gate by what the lane is heard for, and are 0 when this is. The gate is one ramp
        // over the buffer, as the others are, except where a ramp would let sound through that the solo does not let in: a gate that goes down while the lane's tail comes up (a cable drawn
        // into a lane that is not heard) or goes up while the tail goes down (a cable taken out of a lane that is let go of) lets through the product of the two, a bump of the lane's own sound.
        // So a lane that was not heard when the last buffer ended has the gate at its goal at once (nothing was heard to step), and one that is not to be heard when this one ends keeps it
        // where it was (the buffer after has it at its goal).
        let from = if self.heard_at == 0.0 { self.head_goal } else { self.head_at };
        let to = if self.heard_goal == 0.0 { from } else { self.head_goal };
        fade(out, num_channels, from, to);
        self.head_at = to;
        Ok(())
    }

    /// An effect of the lane's chain as a node (its place in the chain is `index`). A lane that has no instrument runs its effects all the same: what comes to the first of them is the
    /// silence that the head made, with what the cables that end at it or at an effect after it bring, so that the effects that a lane keeps when its instrument is taken out go on working
    /// (a reverb's tail rings out, and what another lane sends into one of them is heard through it).
    fn run_fx(&mut self, index: usize, dry: Option<&mut DelayLine>, side_patched: bool, io: &mut Io, cx: &mut BlockCx<'_>) -> Result<()> {
        let (Some(effect), Some(&mix)) = (self.effects.get_mut(index), self.mix_values.get(index)) else { return Ok(()) };
        run_effect(effect.as_mut(), mix as f32, dry, side_patched, io, cx.num_channels)?;
        store_taps(&self.taps, cx.telemetry, TapStage::Effect(index), &io.outputs()[0]);
        Ok(())
    }

    /// The tail of the lane as a node: the metronome's click goes on top of what the chain made, the lane is let in or out by its mute and solo (its group's mute is the bus's), and it
    /// is measured. What the plan then does is add the lane's sound to the bus of its group or to the mix.
    fn run_tail(&mut self, io: &mut Io, cx: &mut BlockCx<'_>) -> Result<()> {
        let frames = io.frames();
        let out = &mut io.inputs_mut()[0];
        self.pattern.render_metronome(out, frames);
        fade(out, cx.num_channels, self.heard_at, self.heard_goal);
        self.heard_at = self.heard_goal;
        if let (Some(telemetry), Some(fall), Some(meter)) = (cx.telemetry, cx.fall, self.meter.as_mut()) {
            meter.update(telemetry, out, fall);
        }
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
    fn run_fx(&mut self, index: usize, dry: Option<&mut DelayLine>, side_patched: bool, io: &mut Io, cx: &mut BlockCx<'_>) -> Result<()> {
        let (Some(effect), Some(&mix)) = (self.effects.get_mut(index), self.mix_values.get(index)) else { return Ok(()) };
        run_effect(effect.as_mut(), mix as f32, dry, side_patched, io, cx.num_channels)?;
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

/// An audio cable that has been drawn, as the engine has it (P3b of `docs/patch-graph.md` §11.6): where it starts and ends, the level that was set, and where its level is.
pub(super) struct Cable {
    id: EdgeId,
    from: CableFrom,
    to: CableTo,
    /// The level that was set.
    gain: f32,
    /// The level it was at when the last buffer ended: 0 for a cable that has just been drawn, so that it comes in over a buffer.
    at: f32,
    /// The level it is to be at when this buffer ends: `gain` times what its start is heard at, and 0 for a cable that has been told to go.
    goal: f32,
    /// Told to go: it goes down to nothing over one buffer, and is then taken out.
    leaving: bool,
    /// The lane or group that the cable starts at and the one that it ends at (none for the mix), found by the plan's places at the start of each block (`Schedule::set_gates`).
    from_owner: Option<Owner>,
    to_owner: Option<Owner>,
}

impl Cable {
    pub(super) fn new(id: EdgeId, from: CableFrom, to: CableTo, gain: f32) -> Cable {
        Cable { id, from, to, gain, at: 0.0, goal: 0.0, leaving: false, from_owner: None, to_owner: None }
    }

    /// The cable as a node: what it is given is let through at the level that it is going to, from the level it was at, over the buffer (so that it comes, goes and is turned
    /// without a click, as a mute is: `fade`), and handed on.
    fn run_node(&mut self, io: &mut Io, num_channels: usize) -> Result<()> {
        fade(&mut io.inputs_mut()[0], num_channels, self.at, self.goal);
        self.at = self.goal;
        io.forward(0, 0);
        Ok(())
    }
}

/// A delay that the plan has put on a cable (P3f), as the schedule keeps it: the nodes that the cable starts and ends at name it, so that it is the same delay, with what it holds, in
/// the next plan that has a cable between them.
struct Link {
    from: Stage,
    to: Stage,
    line: DelayLine,
}

impl Link {
    /// The delay as a node: what is given is handed on in place, through the line.
    fn run_node(&mut self, io: &mut Io) -> Result<()> {
        io.forward(0, 0);
        self.line.run(&mut io.outputs_mut()[0]);
        Ok(())
    }
}

/// Where a node of the plan is kept: a lane or a group at a place of its vector, an effect at a place of the chain of one, a cable or a delay at a place of the schedule's lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Place {
    Lane(usize),
    LaneFx(usize, usize),
    Group(usize),
    GroupFx(usize, usize),
    Cable(usize),
    Delay(usize),
}

/// The most that a plugin may say its latency is, in frames (2.7 seconds at 48 kHz): a plugin that says more is held to it, since a delay is a line of that many frames for each channel
/// that is made when the plan is, and a number that is not meant (a plugin that says what it was not asked) would make one of gigabytes.
const MAX_LATENCY: u32 = 1 << 17;

/// What the host takes a plugin's latency to be: what it says, held to `MAX_LATENCY`.
fn latency_of(plugin: &dyn Plugin) -> u32 {
    plugin.latency().min(MAX_LATENCY)
}

/// The latency of every node of the plan that has some, by stage (sorted, so that it is looked up by a search): an instrument's in the head of its lane, and an effect's.
fn latencies_of(lanes: &[InstrumentLane], groups: &[Group]) -> Vec<(Stage, u32)> {
    let mut all = Vec::new();
    for lane in lanes {
        let instrument = lane.instrument.as_deref().map_or(0, latency_of);
        if instrument > 0 {
            all.push((Stage::Head(lane.node), instrument));
        }
        for (index, effect) in lane.effect_nodes.iter().enumerate() {
            let frames = lane.effects.get(index).map_or(0, |plugin| latency_of(plugin.as_ref()));
            if frames > 0 {
                all.push((Stage::Fx(*effect), frames));
            }
        }
    }
    for group in groups {
        for (index, effect) in group.effect_nodes.iter().enumerate() {
            let frames = group.effects.get(index).map_or(0, |plugin| latency_of(plugin.as_ref()));
            if frames > 0 {
                all.push((Stage::Fx(*effect), frames));
            }
        }
    }
    all.sort_unstable_by_key(|&(stage, _)| stage);
    all
}

/// The line that is the dry sound of the effect `id`, if it has one.
fn dry_line(dry: &mut [(NodeId, DelayLine)], id: NodeId) -> Option<&mut DelayLine> {
    dry.binary_search_by_key(&id, |&(node, _)| node).ok().map(|at| &mut dry[at].1)
}

/// The nodes of the plan, by name: what `Plan::run` asks for a block of.
struct Store<'a> {
    lanes: &'a mut [InstrumentLane],
    groups: &'a mut [Group],
    master: &'a mut Master,
    cables: &'a mut [Cable],
    delays: &'a mut [Link],
    /// The dry sound of the effects that have latency, by the effect's id.
    dry: &'a mut [(NodeId, DelayLine)],
    /// The effects that a cable ends at the second input of (sorted), as the plan was made: the others that have a second input have none patched there.
    sided: &'a [NodeId],
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
                Some(lane) if lane.node == id => lane.run_tail(io, cx),
                _ => Ok(()),
            },
            (Stage::Fx(id), Place::LaneFx(at, index)) => match self.lanes.get_mut(at) {
                Some(lane) if lane.effect_nodes.get(index) == Some(&id) => lane.run_fx(index, dry_line(self.dry, id), self.sided.binary_search(&id).is_ok(), io, cx),
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
                Some(group) if group.effect_nodes.get(index) == Some(&id) => group.run_fx(index, dry_line(self.dry, id), self.sided.binary_search(&id).is_ok(), io, cx),
                _ => Ok(()),
            },
            (Stage::Cable(id), Place::Cable(at)) => match self.cables.get_mut(at) {
                Some(cable) if cable.id == id => cable.run_node(io, cx.num_channels),
                _ => Ok(()),
            },
            (Stage::Delay(number), Place::Delay(at)) => match self.delays.get_mut(at) {
                Some(link) if at == number as usize => link.run_node(io),
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
const CABLE: u64 = 5 << 32;
const FROM_INSTRUMENT: u64 = 6 << 32;
const FROM_EFFECT: u64 = 7 << 32;
const FROM_GROUP: u64 = 8 << 32;
const TO_EFFECT: u64 = 9 << 32;
const TO_BUS: u64 = 10 << 32;
const TO_MASTER: u64 = 11 << 32;
const CUT: u64 = 12 << 32;
/// The latency, in frames, of the instrument of the lane that came before it in the list, and of the effect that did.
const HEAD_LATENCY: u64 = 13 << 32;
const FX_LATENCY: u64 = 14 << 32;
/// The effect that came before it in the list has its feed cut (step 86: the plan has no link into it from the stage before it). An effect that is fed has no entry, so that a session
/// that cuts none has the shape it had.
const CUT_FEED: u64 = 15 << 32;
/// The effect that came before it in the list has a second input (step 95: the plan has a second port for it). An effect that has none has no entry, so that a session that has no
/// effect with one has the shape it had.
const SIDE: u64 = 16 << 32;
const TO_EFFECT_SIDE: u64 = 17 << 32;
const NO_GROUP: u64 = u32::MAX as u64;

impl Shape {
    /// Make this the same as `other`, using the room it has.
    fn copy_from(&mut self, other: &Shape) {
        self.channels = other.channels;
        self.list.clone_from(&other.list);
    }

    /// Read the wiring of `lanes`, `groups` and `cables` into this, keeping the room it has.
    fn read(&mut self, lanes: &[InstrumentLane], groups: &[Group], cables: &[Cable], channels: usize, compensates: bool) {
        self.channels = channels;
        self.list.clear();
        for lane in lanes {
            self.list.push(LANE | u64::from(lane.node.0));
            self.list.push(GROUP_OF | lane.group.and_then(|g| groups.get(g)).map_or(NO_GROUP, |group| u64::from(group.node.0)));
            // (What has no latency has no entry, so that a session that has none has the shape it had, and neither has anything where the latency is not made up for.)
            if let Some(frames) = lane.instrument.as_deref().map(latency_of).filter(|&frames| compensates && frames > 0) {
                self.list.push(HEAD_LATENCY | u64::from(frames));
            }
            if lane.route == Route::None {
                self.list.push(CUT);
            }
            for (index, effect) in lane.effect_nodes.iter().enumerate() {
                self.list.push(EFFECT | u64::from(effect.0));
                if lane.effects.get(index).is_some_and(|plugin| plugin.side_input().is_some()) {
                    self.list.push(SIDE);
                }
                if lane.cut_feeds.contains(effect) {
                    self.list.push(CUT_FEED);
                }
                if let Some(frames) = lane.effects.get(index).map(|plugin| latency_of(plugin.as_ref())).filter(|&frames| compensates && frames > 0) {
                    self.list.push(FX_LATENCY | u64::from(frames));
                }
            }
        }
        for group in groups {
            self.list.push(GROUP | u64::from(group.node.0));
            if group.route == Route::None {
                self.list.push(CUT);
            }
            for (index, effect) in group.effect_nodes.iter().enumerate() {
                self.list.push(EFFECT | u64::from(effect.0));
                if group.effects.get(index).is_some_and(|plugin| plugin.side_input().is_some()) {
                    self.list.push(SIDE);
                }
                if group.cut_feeds.contains(effect) {
                    self.list.push(CUT_FEED);
                }
                if let Some(frames) = group.effects.get(index).map(|plugin| latency_of(plugin.as_ref())).filter(|&frames| compensates && frames > 0) {
                    self.list.push(FX_LATENCY | u64::from(frames));
                }
            }
        }
        for cable in cables {
            self.list.push(CABLE | u64::from(cable.id.0));
            self.list.push(match cable.from {
                CableFrom::Instrument(id) => FROM_INSTRUMENT | u64::from(id.0),
                CableFrom::Effect(id) => FROM_EFFECT | u64::from(id.0),
                CableFrom::Group(id) => FROM_GROUP | u64::from(id.0),
            });
            self.list.push(match cable.to {
                CableTo::Effect(id) => TO_EFFECT | u64::from(id.0),
                CableTo::EffectSide(id) => TO_EFFECT_SIDE | u64::from(id.0),
                CableTo::GroupBus(id) => TO_BUS | u64::from(id.0),
                CableTo::Master => TO_MASTER,
            });
        }
    }
}

impl CableFrom {
    /// The node of the plan that the cable starts at (its output 0).
    fn stage(self) -> Stage {
        match self {
            CableFrom::Instrument(id) => Stage::Head(id),
            CableFrom::Effect(id) => Stage::Fx(id),
            CableFrom::Group(id) => Stage::Tail(id),
        }
    }
}

impl CableTo {
    /// The node of the plan that the cable ends at.
    fn stage(self) -> Stage {
        match self {
            CableTo::Effect(id) | CableTo::EffectSide(id) => Stage::Fx(id),
            CableTo::GroupBus(id) => Stage::Head(id),
            CableTo::Master => Stage::Master,
        }
    }

    /// The input port of that node that it ends at: the first, except for the second input of an effect that has one.
    fn port(self) -> u16 {
        match self {
            CableTo::EffectSide(_) => 1,
            CableTo::Effect(_) | CableTo::GroupBus(_) | CableTo::Master => 0,
        }
    }
}

/// What the plan needs to know of a lane: its id, the place of the group that it is in, and the ids of its effects. The engine and the model both make it of what they have, the
/// model to find out, before it asks the engine, whether a cable can be drawn.
#[derive(Clone, Debug)]
pub(crate) struct LaneWiring {
    pub node: NodeId,
    pub group: Option<usize>,
    pub effects: Vec<NodeId>,
    /// The effects (of `effects`) that the stage before them does not feed: the plan has no link into them from it (`Feed::None`), only their cables.
    pub cut: Vec<NodeId>,
    /// The effects (of `effects`) that have a second input (`Plugin::side_input`): their nodes have a second port, which cables can end at.
    pub sides: Vec<NodeId>,
    /// Whether the lane's end goes anywhere: to the bus of its group, or to the mix (`Route::Out`), or nowhere but its cables (`Route::None`).
    pub route: Route,
}

/// What the plan needs to know of a group: its id and the ids of its bus effects.
#[derive(Clone, Debug)]
pub(crate) struct GroupWiring {
    pub node: NodeId,
    pub effects: Vec<NodeId>,
    /// The bus effects that the stage before them does not feed (see `LaneWiring::cut`).
    pub cut: Vec<NodeId>,
    /// The bus effects that have a second input (see `LaneWiring::sides`).
    pub sides: Vec<NodeId>,
    /// Whether the group's end goes to the mix (`Route::Out`) or nowhere but its cables (`Route::None`).
    pub route: Route,
}

/// The wiring of the lanes and the groups as the engine has them.
fn wiring_of(lanes: &[InstrumentLane], groups: &[Group]) -> (Vec<LaneWiring>, Vec<GroupWiring>) {
    let sides = |nodes: &[NodeId], plugins: &[Box<dyn Plugin>]| -> Vec<NodeId> { nodes.iter().zip(plugins).filter(|(_, plugin)| plugin.side_input().is_some()).map(|(node, _)| *node).collect() };
    (
        lanes
            .iter()
            .map(|lane| LaneWiring { node: lane.node, group: lane.group, effects: lane.effect_nodes.clone(), cut: lane.cut_feeds.clone(), sides: sides(&lane.effect_nodes, &lane.effects), route: lane.route })
            .collect(),
        groups
            .iter()
            .map(|group| GroupWiring { node: group.node, effects: group.effect_nodes.clone(), cut: group.cut_feeds.clone(), sides: sides(&group.effect_nodes, &group.effects), route: group.route })
            .collect(),
    )
}

/// The rank of the node that a cable starts at: its owner's, a lane's place or the place of a group after the lanes. None if what `from` names is not in the wiring (or is not what
/// it says: the id of a group is no instrument).
fn start_rank(from: CableFrom, lanes: &[LaneWiring], groups: &[GroupWiring]) -> Option<u32> {
    let lane_count = lanes.len() as u32;
    let in_a_group = |at: usize| lane_count + at as u32;
    match from {
        CableFrom::Instrument(id) => lanes.iter().position(|lane| lane.node == id).map(|at| at as u32),
        CableFrom::Group(id) => groups.iter().position(|group| group.node == id).map(in_a_group),
        CableFrom::Effect(id) => lanes
            .iter()
            .position(|lane| lane.effects.contains(&id))
            .map(|at| at as u32)
            .or_else(|| groups.iter().position(|group| group.effects.contains(&id)).map(in_a_group)),
    }
}

/// Whether what a cable ends at is in the wiring.
fn end_exists(to: CableTo, lanes: &[LaneWiring], groups: &[GroupWiring]) -> bool {
    match to {
        CableTo::Effect(id) => lanes.iter().any(|lane| lane.effects.contains(&id)) || groups.iter().any(|group| group.effects.contains(&id)),
        CableTo::EffectSide(id) => lanes.iter().any(|lane| lane.sides.contains(&id)) || groups.iter().any(|group| group.sides.contains(&id)),
        CableTo::GroupBus(id) => groups.iter().any(|group| group.node == id),
        CableTo::Master => true,
    }
}

/// A lane or a group, by its place: what a cable starts at and ends at is one of them, and the law of mute and solo is worked out for them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Owner {
    Lane(usize),
    Group(usize),
}

/// The lane or the group that the node `stage` of the plan belongs to, found by the place that the plan has for it. None for a node that is not at its place (it makes silence).
fn owner_of(stage: Stage, places: &[(Stage, Place)], lanes: &[InstrumentLane], groups: &[Group]) -> Option<Owner> {
    let found = places.binary_search_by_key(&stage, |&(stage, _)| stage).ok().map(|found| places[found].1)?;
    match (stage, found) {
        (Stage::Head(id) | Stage::Tail(id), Place::Lane(at)) => lanes.get(at).filter(|lane| lane.node == id).map(|_| Owner::Lane(at)),
        (Stage::Fx(id), Place::LaneFx(at, index)) => lanes.get(at).filter(|lane| lane.effect_nodes.get(index) == Some(&id)).map(|_| Owner::Lane(at)),
        (Stage::Head(id) | Stage::Tail(id), Place::Group(at)) => groups.get(at).filter(|group| group.node == id).map(|_| Owner::Group(at)),
        (Stage::Fx(id), Place::GroupFx(at, index)) => groups.get(at).filter(|group| group.effect_nodes.get(index) == Some(&id)).map(|_| Owner::Group(at)),
        _ => None,
    }
}

/// Make `patch` the wiring of the lanes, the groups and the cables, which are an id, a start and an end (see `Schedule::compile`). A cable with an end that is not in the wiring is
/// left out.
fn build_patch(patch: &mut Patch<Stage>, lanes: &[LaneWiring], groups: &[GroupWiring], cables: impl Iterator<Item = (EdgeId, CableFrom, CableTo)> + Clone, channels: usize) {
    let audio = PortSpec::audio(channels as u16);
    let chain = PortSpec::single(channels as u16);
    let lane_count = lanes.len() as u32;
    patch.clear();
    patch.add_node(Stage::Master, NodeSpec::new(vec![audio], vec![], u32::MAX));
    for (at, lane) in lanes.iter().enumerate() {
        let rank = at as u32;
        patch.add_node(Stage::Head(lane.node), NodeSpec::new(vec![], vec![audio], rank));
        for effect in &lane.effects {
            // (An effect that has a second input has a second port: what the cables at it bring, which is silence while there are none.)
            let inputs = if lane.sides.contains(effect) { vec![chain, chain] } else { vec![chain] };
            patch.add_node(Stage::Fx(*effect), NodeSpec::new(inputs, vec![audio], rank));
        }
        patch.add_node(Stage::Tail(lane.node), NodeSpec::new(vec![chain], vec![audio], rank));
    }
    for (at, group) in groups.iter().enumerate() {
        let rank = lane_count + at as u32;
        // The head of a group takes the sum of its members, so it is an input that is a sum.
        patch.add_node(Stage::Head(group.node), NodeSpec::new(vec![audio], vec![audio], rank));
        for effect in &group.effects {
            let inputs = if group.sides.contains(effect) { vec![chain, chain] } else { vec![chain] };
            patch.add_node(Stage::Fx(*effect), NodeSpec::new(inputs, vec![audio], rank));
        }
        patch.add_node(Stage::Tail(group.node), NodeSpec::new(vec![chain], vec![audio], rank));
    }
    for lane in lanes {
        let mut from = Stage::Head(lane.node);
        for effect in &lane.effects {
            // (An effect whose feed is cut has no link into it from the stage before: what it is given is what its cables carry, and nothing else. The chain goes on from it all the same.)
            if !lane.cut.contains(effect) {
                patch.connect(Port::new(from, 0), Port::new(Stage::Fx(*effect), 0));
            }
            from = Stage::Fx(*effect);
        }
        patch.connect(Port::new(from, 0), Port::new(Stage::Tail(lane.node), 0));
        // (A lane whose way out is cut has no link out of its tail: what it makes goes to its cables, from the nodes that they start at, and nowhere else.)
        if lane.route == Route::Out {
            let to = lane.group.and_then(|g| groups.get(g)).map_or(Stage::Master, |group| Stage::Head(group.node));
            patch.connect(Port::new(Stage::Tail(lane.node), 0), Port::new(to, 0));
        }
    }
    for group in groups {
        let mut from = Stage::Head(group.node);
        for effect in &group.effects {
            if !group.cut.contains(effect) {
                patch.connect(Port::new(from, 0), Port::new(Stage::Fx(*effect), 0));
            }
            from = Stage::Fx(*effect);
        }
        patch.connect(Port::new(from, 0), Port::new(Stage::Tail(group.node), 0));
        if group.route == Route::Out {
            patch.connect(Port::new(Stage::Tail(group.node), 0), Port::new(Stage::Master, 0));
        }
    }
    let cables = cables.filter_map(|(id, from, to)| {
        let rank = start_rank(from, lanes, groups).filter(|_| end_exists(to, lanes, groups))?;
        Some((id, from, to, rank))
    });
    for (id, _, _, rank) in cables.clone() {
        patch.add_node(Stage::Cable(id), NodeSpec::new(vec![chain], vec![audio], rank));
    }
    for (id, from, to, _) in cables {
        patch.connect(Port::new(from.stage(), 0), Port::new(Stage::Cable(id), 0));
        patch.connect(Port::new(Stage::Cable(id), 0), Port::new(to.stage(), to.port()));
    }
}

/// Why a cable cannot be drawn into a wiring.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CableProblem {
    /// What it starts at is not in the wiring, or is not what it says.
    NoStart,
    /// What it ends at is not in the wiring.
    NoEnd,
    /// The audio would go round: the wiring with it does not compile, and the reason is the kernel's.
    Loop(String),
}

impl std::fmt::Display for CableProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CableProblem::NoStart => f.write_str("what the cable starts at is not in the wiring"),
            CableProblem::NoEnd => f.write_str("what the cable ends at is not in the wiring"),
            CableProblem::Loop(reason) => write!(f, "the audio would go round ({reason})"),
        }
    }
}

/// Whether `cable` (an id, a start and an end) can be drawn into the wiring of `lanes` and `groups` that has the cables `others` already: both its ends are in it, and the audio
/// goes one way through it with the cable (the wiring is compiled, on the side). The engine asks it of a cable that it is sent, and the model before it sends one.
pub(crate) fn cable_problem(
    lanes: &[LaneWiring],
    groups: &[GroupWiring],
    others: impl Iterator<Item = (EdgeId, CableFrom, CableTo)> + Clone,
    cable: (EdgeId, CableFrom, CableTo),
    channels: usize,
) -> Option<CableProblem> {
    if start_rank(cable.1, lanes, groups).is_none() {
        return Some(CableProblem::NoStart);
    }
    if !end_exists(cable.2, lanes, groups) {
        return Some(CableProblem::NoEnd);
    }
    wiring_loop(lanes, groups, others.filter(|other| other.0 != cable.0).chain(std::iter::once(cable)), channels).map(CableProblem::Loop)
}

/// Why the audio would go round in the wiring of `lanes` and `groups` with `cables` (compiled on the side), or None if it goes one way through it. A cable with an end that is not
/// in the wiring is left out of it, as the plan leaves it out. The model asks it of a wiring that a change of the lanes' groups or of the order of a chain would make, before it makes it.
pub(crate) fn wiring_loop(lanes: &[LaneWiring], groups: &[GroupWiring], cables: impl Iterator<Item = (EdgeId, CableFrom, CableTo)> + Clone, channels: usize) -> Option<String> {
    let mut scratch = Patch::new();
    build_patch(&mut scratch, lanes, groups, cables, channels);
    kernel::compile(&scratch).err().map(|error| error.to_string())
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
    /// The audio cables that have been drawn, by id (P3b).
    cables: Vec<Cable>,
    /// The delays that the plan has put on cables so that the paths that are added are in step (P3f), in the order of their numbers (`Stage::Delay`).
    delays: Vec<Link>,
    /// The dry sound of each effect that has latency, by the effect's id (sorted): the line that makes it as late as the effect's own sound is.
    dry: Vec<(NodeId, DelayLine)>,
    /// The effects that a cable ends at the second input of, by id (sorted), as the plan was made (a cable that is drawn or taken out makes the plan again): an effect that has a second
    /// input and is not here has nothing patched there, and is told so.
    sided: Vec<NodeId>,
    /// How late the latest of what the lanes and groups make is, in frames, when the plan was made: the latency of the session.
    latency: u32,
    /// Whether the paths that are added are put in step by the latencies of the plugins. It is always so in the program; the loop that the plan replaced does not do it, so the oracle
    /// holds the plan to it with this off.
    compensates: bool,
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
            cables: Vec::new(),
            delays: Vec::new(),
            dry: Vec::new(),
            sided: Vec::new(),
            latency: 0,
            compensates: true,
            room: 0,
            #[cfg(test)]
            compiles: 0,
        }
    }

    /// Make the plan the one for the lanes and groups as they are now (when they are not as they were, which asks the allocator for memory, as a command that inserts a lane
    /// does), and give the pool room for a block of `frames`.
    pub(super) fn refresh(&mut self, lanes: &[InstrumentLane], groups: &[Group], channels: usize, frames: usize) {
        // A cable that was told to go, and has gone down to nothing, is taken out.
        self.cables.retain(|cable| !(cable.leaving && cable.at == 0.0));
        self.now.read(lanes, groups, &self.cables, channels, self.compensates);
        if self.now != self.shape {
            self.compile(lanes, groups, channels);
            // The cables that had an end that has gone are gone, so the wiring is read again: what the plan was made for.
            self.now.read(lanes, groups, &self.cables, channels, self.compensates);
            // Copied, not swapped: both keep their room, so that reading the wiring of the next block asks for none.
            self.shape.copy_from(&self.now);
        }
        if frames > self.room {
            self.pool.reserve(frames);
            self.room = frames;
        }
    }

    /// The plan for the lanes and groups: the mix, a row of nodes for each lane and each group, a node for each audio cable that has both its ends, and the cables.
    fn compile(&mut self, lanes: &[InstrumentLane], groups: &[Group], channels: usize) {
        #[cfg(test)]
        {
            self.compiles += 1;
        }
        let (lane_wiring, group_wiring) = wiring_of(lanes, groups);
        build_patch(&mut self.patch, &lane_wiring, &group_wiring, self.cables.iter().map(|cable| (cable.id, cable.from, cable.to)), channels);
        // The latency of the nodes, and where cables are added whose paths are not as late as each other, the delay that puts them in step.
        let latencies = if self.compensates { latencies_of(lanes, groups) } else { Vec::new() };
        let (mut latency, mut wanted) = (0, Vec::new());
        if !latencies.is_empty() {
            let of = |stage: Stage| latencies.binary_search_by_key(&stage, |&(node, _)| node).map_or(0, |at| latencies[at].1);
            // (The patch that `build_patch` made compiles, or the plan below says so: an error here is that one's.)
            if let Ok(alignment) = kernel::align(&self.patch, of) {
                latency = alignment.latency();
                if alignment.is_needed() {
                    self.patch = kernel::with_delays(&self.patch, &alignment, |number, cable, frames| {
                        wanted.push((cable.from.node, cable.to.node, frames));
                        Stage::Delay(number as u32)
                    });
                }
            }
        }
        match kernel::compile(&self.patch) {
            Ok(plan) => {
                self.pool.fit(&plan);
                self.plan = plan;
                self.latency = latency;
                // The delays: those between the same two nodes as before are the same lines, with what they hold (a line that is another length keeps the newest of it), the others are new.
                let mut before = std::mem::take(&mut self.delays);
                self.delays = wanted
                    .into_iter()
                    .map(|(from, to, frames)| match before.iter().position(|link| link.from == from && link.to == to) {
                        Some(at) => {
                            let mut link = before.swap_remove(at);
                            link.line.resize(frames as usize);
                            link
                        }
                        None => Link { from, to, line: DelayLine::new(frames as usize, channels) },
                    })
                    .collect();
                // The dry sound of an effect that has latency, in the same way, by the effect's id.
                let mut before = std::mem::take(&mut self.dry);
                for &(stage, frames) in &latencies {
                    if let Stage::Fx(id) = stage {
                        let line = match before.iter().position(|(node, _)| *node == id) {
                            Some(at) => {
                                let mut line = before.swap_remove(at).1;
                                line.resize(frames as usize);
                                line
                            }
                            None => DelayLine::new(frames as usize, channels),
                        };
                        self.dry.push((id, line));
                    }
                }
                // (They are in the order of `latencies`, which is sorted by stage, so that the effects are by id, as `dry_line` looks them up.)
                debug_assert!(self.dry.windows(2).all(|pair| pair[0].0 < pair[1].0), "the dry lines are by the id of the effect");
                // A cable that has an end that is not in the wiring (a lane, a group or an effect that went) goes with it.
                let patch = &self.patch;
                self.cables.retain(|cable| patch.nodes().iter().any(|(stage, _)| *stage == Stage::Cable(cable.id)));
                // The effects that have a cable at their second input (a cable that has been told to go is there until it has gone down).
                self.sided.clear();
                self.sided.extend(self.cables.iter().filter_map(|cable| if let CableTo::EffectSide(id) = cable.to { Some(id) } else { None }));
                self.sided.sort_unstable();
                self.sided.dedup();
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
                self.places.extend(self.cables.iter().enumerate().map(|(at, cable)| (Stage::Cable(cable.id), Place::Cable(at))));
                self.places.extend((0..self.delays.len()).map(|at| (Stage::Delay(at as u32), Place::Delay(at))));
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

    /// Why the audio would go round in the wiring of `lanes` and `groups` as they are, with the cables there are: what a way out that is given back is checked by, after it is
    /// set (the cables were drawn into a wiring that had its way out cut, and the link that is made again may close a loop with them). None if it goes one way through it.
    pub(super) fn route_problem(&self, lanes: &[InstrumentLane], groups: &[Group], channels: usize) -> Option<String> {
        let (lane_wiring, group_wiring) = wiring_of(lanes, groups);
        wiring_loop(&lane_wiring, &group_wiring, self.cables.iter().map(|cable| (cable.id, cable.from, cable.to)), channels)
    }

    /// Draw a cable (see `GraphCommand::AddCable`): it is put among the cables by its id, so that the plan is the same whatever the order the cables were drawn in, if both its
    /// ends are in the wiring and the audio goes one way through the wiring with it (made on the side: a cable that would make it go round is not drawn, and nothing changes). A
    /// cable that is there under the id is drawn again at the new ends and the new level, from where it was.
    pub(super) fn add_cable(&mut self, cable: Cable, lanes: &[InstrumentLane], groups: &[Group], channels: usize) {
        if !cable.gain.is_finite() {
            log::warn!("cable {}: its level is not a number; not drawn", cable.id.0);
            return;
        }
        let (lane_wiring, group_wiring) = wiring_of(lanes, groups);
        let others = self.cables.iter().map(|other| (other.id, other.from, other.to));
        if let Some(problem) = cable_problem(&lane_wiring, &group_wiring, others, (cable.id, cable.from, cable.to), channels) {
            log::warn!("cable {}: {problem}; not drawn", cable.id.0);
            return;
        }
        match self.cables.binary_search_by_key(&cable.id, |other| other.id) {
            Ok(at) => {
                let drawn = &mut self.cables[at];
                (drawn.from, drawn.to, drawn.gain, drawn.leaving) = (cable.from, cable.to, cable.gain, false);
            }
            Err(at) => self.cables.insert(at, cable),
        }
    }

    /// Take a cable out (see `GraphCommand::RemoveCable`): one that is heard goes down to nothing over a buffer and is taken out when the next one begins; one that is not heard
    /// goes at once.
    pub(super) fn remove_cable(&mut self, id: EdgeId) {
        if let Ok(at) = self.cables.binary_search_by_key(&id, |cable| cable.id) {
            if self.cables[at].at == 0.0 {
                self.cables.remove(at);
            } else {
                self.cables[at].leaving = true;
            }
        }
    }

    /// Set the level of a cable (see `GraphCommand::SetCableGain`): what is not a number leaves it as it was.
    pub(super) fn set_cable_gain(&mut self, id: EdgeId, gain: f32) {
        if let (Ok(at), true) = (self.cables.binary_search_by_key(&id, |cable| cable.id), gain.is_finite()) {
            self.cables[at].gain = gain;
        }
    }

    /// Whether a cable that was told to go has gone down to nothing: it is to leave the plan now (`refresh` takes it out), at the end of the block it went down in.
    pub(super) fn has_a_cable_that_is_gone(&self) -> bool {
        self.cables.iter().any(|cable| cable.leaving && cable.at == 0.0)
    }

    /// What the lanes, the groups and the cables are heard at in this block, from the mutes and solos and from what the cables send: the law of mute and solo for audio that cables
    /// carry (P3c-2b of `docs/patch-graph.md` §11.6, D78 of `docs/web-ui.md`). After `refresh`, which has the places that the owners of the cables' ends are found by.
    ///
    /// A lane or a group is heard *for what it is* (`own`) when it is not muted and nothing is soloed, or it is, or (a lane) its group is, or (a group) a lane in it is. It is heard
    /// *for what is sent to it* (`live`) when it is not muted and a cable ends in it (at an effect of its chain or its bus, or at the bus of a group) from something that is heard
    /// (the instrument of a lane that is heard for what it is, an effect of a lane or a group that is heard either way, or the output of a group that is), or, for a group, when a lane in
    /// it that goes to its bus is heard for what is sent to it. What is heard either way is let through (`heard_goal`); a cable carries what its start puts out, at the level it was
    /// set (an effect's or a group's by what its owner is heard at, an instrument's by what the lane is heard for), and a lane that cables end in puts none of its own sound into
    /// its chain while it is not let in by the solo (`head_goal`): what it puts out is what it was sent. Where nothing is soloed, or no cable ends in a lane, this is what the
    /// mutes and solos always made.
    pub(super) fn set_gates(&mut self, lanes: &mut [InstrumentLane], groups: &mut [Group]) {
        let any_solo = lanes.iter().any(|lane| lane.solo) || groups.iter().any(|group| group.solo);
        // Let in by the solo: a group that is soloed or has a lane in it that is, a lane that is soloed or is in a group that is.
        for (gi, group) in groups.iter_mut().enumerate() {
            let member_solo = lanes.iter().any(|lane| lane.solo && lane.group == Some(gi));
            group.solo_heard = !any_solo || group.solo || member_solo;
            group.live = false;
        }
        for lane in lanes.iter_mut() {
            let group_solo = lane.group.and_then(|g| groups.get(g)).is_some_and(|g| g.solo);
            lane.solo_heard = !any_solo || lane.solo || group_solo;
            lane.live = false;
            lane.receives = false;
        }
        // Whose each cable's ends are, and which lanes cables end in.
        for cable in &mut self.cables {
            // (The mix is no lane or group, and has no place.)
            cable.from_owner = owner_of(cable.from.stage(), &self.places, lanes, groups);
            cable.to_owner = owner_of(cable.to.stage(), &self.places, lanes, groups);
            if let (false, Some(Owner::Lane(at))) = (cable.leaving, cable.to_owner) {
                lanes[at].receives = true;
            }
        }
        // What is sent to a lane or a group from something that is heard lets it in. A cable's sender being heard depends on whether the sender was let in by cables, and these are
        // cables that go one way (a loop is never drawn), so that going through them again until nothing changes is at most as many times as there are cables.
        for _ in 0..=self.cables.len() {
            let mut changed = false;
            for cable in &self.cables {
                if cable.leaving || cable.gain <= 0.0 {
                    continue;
                }
                let sent = match (cable.from, cable.from_owner) {
                    (CableFrom::Instrument(_), Some(Owner::Lane(at))) => lanes[at].own(),
                    (_, Some(Owner::Lane(at))) => lanes[at].out(),
                    (_, Some(Owner::Group(at))) => groups[at].out(),
                    (_, None) => false,
                };
                match cable.to_owner {
                    Some(Owner::Lane(at)) if sent && !lanes[at].muted && !lanes[at].live => {
                        lanes[at].live = true;
                        changed = true;
                    }
                    Some(Owner::Group(at)) if sent && !groups[at].muted && !groups[at].live => {
                        groups[at].live = true;
                        changed = true;
                    }
                    _ => {}
                }
            }
            // A lane that is heard for what is sent to it puts it into the bus of its group, which is heard for that.
            for lane in lanes.iter() {
                if let (true, Route::Out, Some(group)) = (lane.live, lane.route, lane.group.and_then(|g| groups.get_mut(g))) {
                    if !group.muted && !group.live {
                        group.live = true;
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        for lane in lanes.iter_mut() {
            lane.heard_goal = if lane.out() { 1.0 } else { 0.0 };
            lane.head_goal = if lane.receives && !lane.solo_heard { 0.0 } else { 1.0 };
        }
        for group in groups.iter_mut() {
            group.heard_goal = if group.out() { 1.0 } else { 0.0 };
        }
        for cable in &mut self.cables {
            // (The output of a group has been let in or out already: a cable from it is not gated again.)
            let gate = match (cable.from, cable.from_owner) {
                (CableFrom::Group(_), _) => 1.0,
                (CableFrom::Instrument(_), Some(Owner::Lane(at))) => f32::from(lanes[at].own()),
                (_, Some(Owner::Lane(at))) => f32::from(lanes[at].out()),
                (_, Some(Owner::Group(at))) => f32::from(groups[at].out()),
                (_, None) => 0.0,
            };
            cable.goal = if cable.leaving { 0.0 } else { cable.gain * gate };
        }
    }

    /// The ids of the cables that there are, in order.
    #[cfg(test)]
    pub(super) fn cable_ids(&self) -> Vec<EdgeId> {
        self.cables.iter().map(|cable| cable.id).collect()
    }

    /// How many times the plan has been made, for a test to see which block makes it.
    #[cfg(test)]
    pub(super) fn plans_made(&self) -> usize {
        self.compiles
    }

    /// The cables that are drawn, in order: those that have not been told to go.
    #[cfg(test)]
    pub(super) fn cable_facts(&self) -> Vec<super::CableFacts> {
        self.cables.iter().filter(|cable| !cable.leaving).map(|cable| super::CableFacts { id: cable.id, from: cable.from, to: cable.to, gain: cable.gain }).collect()
    }

    /// The nodes of the plan in the order they run.
    #[cfg(test)]
    pub(super) fn stages(&self) -> Vec<Stage> {
        self.plan.order().collect()
    }

    /// How late the latest of what the lanes and groups make is, in frames: the latency of the session, as the plan was made.
    #[cfg(test)]
    pub(super) fn latency(&self) -> u32 {
        self.latency
    }

    /// Stop putting the paths that are added in step by the latencies of the plugins: what the loop that the plan replaced did, for the oracle to hold the plan to.
    #[cfg(test)]
    pub(super) fn without_compensation(&mut self) {
        self.compensates = false;
    }

    /// The delays that the plan has put on cables: the nodes each joins and the frames.
    #[cfg(test)]
    fn delay_frames(&self) -> Vec<(Stage, Stage, usize)> {
        self.delays.iter().map(|link| (link.from, link.to, link.line.frames())).collect()
    }

    /// The frames of the line that is the dry sound of an effect, if it has one.
    #[cfg(test)]
    fn dry_frames(&self, effect: NodeId) -> Option<usize> {
        self.dry.iter().find(|(id, _)| *id == effect).map(|(_, line)| line.frames())
    }

    /// Run one block: the lanes, the groups and the mix.
    pub(super) fn run(&mut self, lanes: &mut [InstrumentLane], groups: &mut [Group], master: &mut Master, frames: usize, cx: &mut BlockCx<'_>) -> Result<()> {
        self.plan.run(&mut self.pool, frames, &mut Store { lanes, groups, master, cables: &mut self.cables, delays: &mut self.delays, dry: &mut self.dry, sided: &self.sided, places: &self.places }, cx)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

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
            Stage::Cable(id) => format!("cable {}", id.0),
            Stage::Delay(number) => format!("delay {number}"),
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
        let mut cx = BlockCx { midi: Events::keys(midi), audio_out: &mut out, num_channels: 2, scale: Default::default(), telemetry: None, fall: None };
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

    /// A cable of level 1 from `from` to `to`, drawn into the schedule as the engine does it.
    fn draw(schedule: &mut Schedule, id: u32, from: CableFrom, to: CableTo, lanes: &[InstrumentLane], groups: &[Group]) {
        schedule.add_cable(Cable::new(EdgeId(id), from, to, 1.0), lanes, groups, 2);
    }

    #[test]
    fn a_cable_is_a_node_of_the_plan_that_runs_between_its_ends_and_the_chain_link_at_its_start_and_at_its_end_is_fed() {
        let mut schedule = Schedule::new();
        let (mut lanes, groups) = (lanes(2), groups(0));
        with_effects(&mut lanes[0], 1);
        with_effects(&mut lanes[1], 1);
        let (sender, fx) = (lanes[1].node, lanes[0].effect_nodes[0]);
        draw(&mut schedule, 7, CableFrom::Instrument(sender), CableTo::Effect(fx), &lanes, &groups);
        schedule.refresh(&lanes, &groups, 2, 64);
        let order = order(&schedule, &lanes, &groups);
        let at = |name: &str| order.iter().position(|node| node == name).unwrap_or_else(|| panic!("{name} is not in {order:?}"));
        assert!(at("lane 1 head") < at("cable 7") && at("cable 7") < at("lane 0 fx 0"), "{order:?}");
        // The head of the sender has two cables leaving it now, and the effect two entering it: each is a sum that is fed, where a chain link of one cable is the buffer itself.
        let feeds = feeds(&schedule, &lanes, &groups);
        for (from, to) in [("lane 1 head", "cable 7"), ("cable 7", "lane 0 fx 0"), ("lane 1 head", "lane 1 fx 0"), ("lane 0 head", "lane 0 fx 0")] {
            assert!(feeds.contains(&(from.to_string(), to.to_string())), "{from} to {to} is not fed in {feeds:?}");
        }
        assert!(!feeds.contains(&("lane 0 fx 0".to_string(), "lane 0 tail".to_string())), "a link that nothing else joins is still the buffer itself");
    }

    #[test]
    fn a_cable_has_the_rank_of_the_node_that_owns_its_start() {
        let mut schedule = Schedule::new();
        let (mut lanes, groups) = (lanes(3), groups(2));
        with_effects(&mut lanes[1], 1);
        let mut groups = groups;
        with_bus_effects(&mut groups[1], 1);
        let (lane_effect, bus_effect) = (lanes[1].effect_nodes[0], groups[1].effect_nodes[0]);
        draw(&mut schedule, 1, CableFrom::Instrument(lanes[2].node), CableTo::Master, &lanes, &groups);
        draw(&mut schedule, 2, CableFrom::Effect(lane_effect), CableTo::Master, &lanes, &groups);
        draw(&mut schedule, 3, CableFrom::Effect(bus_effect), CableTo::Master, &lanes, &groups);
        draw(&mut schedule, 4, CableFrom::Group(groups[0].node), CableTo::GroupBus(groups[1].node), &lanes, &groups);
        schedule.refresh(&lanes, &groups, 2, 64);
        let rank = |id: u32| schedule.patch.nodes().iter().find(|(stage, _)| *stage == Stage::Cable(EdgeId(id))).map(|(_, spec)| spec.rank).expect("a cable node");
        // The lanes are 0 to 2 and the groups 3 and 4: the instrument of lane 2, an effect of lane 1, an effect of the bus of group 1, the output of group 0.
        assert_eq!([rank(1), rank(2), rank(3), rank(4)], [2, 1, 4, 3]);
    }

    #[test]
    fn a_cable_that_comes_or_goes_makes_the_plan_again_and_a_level_that_is_set_does_not() {
        let mut schedule = Schedule::new();
        let (mut lanes, mut groups) = (lanes(2), groups(0));
        with_effects(&mut lanes[0], 1);
        schedule.refresh(&lanes, &groups, 2, 64);
        let made = schedule.compiles;
        let (sender, fx) = (lanes[1].node, lanes[0].effect_nodes[0]);
        draw(&mut schedule, 7, CableFrom::Instrument(sender), CableTo::Effect(fx), &lanes, &groups);
        schedule.refresh(&lanes, &groups, 2, 64);
        assert_eq!(schedule.compiles, made + 1, "a cable that is drawn");
        schedule.refresh(&lanes, &groups, 2, 64);
        schedule.set_cable_gain(EdgeId(7), 0.5);
        schedule.set_gates(&mut lanes, &mut groups);
        schedule.refresh(&lanes, &groups, 2, 64);
        assert_eq!(schedule.compiles, made + 1, "nothing is made again for a block that has the same wiring, and not for a level");
        // A buffer in which it has a level, and then it is told to go: it is there for the buffer in which it goes down, and gone from the next.
        run(&mut schedule, &mut lanes, &mut groups, 64);
        schedule.remove_cable(EdgeId(7));
        schedule.refresh(&lanes, &groups, 2, 64);
        assert_eq!(schedule.cable_ids(), [EdgeId(7)]);
        assert!(schedule.cable_facts().is_empty(), "it is in the plan, for the buffer it goes down in, and it is not a cable that the engine has");
        assert_eq!(schedule.compiles, made + 1);
        schedule.set_gates(&mut lanes, &mut groups);
        run(&mut schedule, &mut lanes, &mut groups, 64);
        schedule.refresh(&lanes, &groups, 2, 64);
        assert!(schedule.cable_ids().is_empty());
        assert_eq!(schedule.compiles, made + 2, "the cable that has gone");
        // One that was never heard goes at once.
        draw(&mut schedule, 8, CableFrom::Instrument(sender), CableTo::Effect(fx), &lanes, &groups);
        schedule.remove_cable(EdgeId(8));
        assert!(schedule.cable_ids().is_empty());
    }

    #[test]
    fn a_cable_is_not_drawn_to_or_from_a_node_that_is_not_there_and_goes_with_the_lane_or_the_effect_that_it_ends_at() {
        let mut schedule = Schedule::new();
        let (mut lanes, groups) = (lanes(2), groups(1));
        with_effects(&mut lanes[0], 1);
        let (sender, fx) = (lanes[1].node, lanes[0].effect_nodes[0]);
        draw(&mut schedule, 1, CableFrom::Instrument(NodeId(70_000)), CableTo::Master, &lanes, &groups);
        draw(&mut schedule, 2, CableFrom::Instrument(sender), CableTo::Effect(NodeId(70_001)), &lanes, &groups);
        draw(&mut schedule, 3, CableFrom::Instrument(groups[0].node), CableTo::Master, &lanes, &groups);
        draw(&mut schedule, 4, CableFrom::Group(sender), CableTo::Master, &lanes, &groups);
        draw(&mut schedule, 5, CableFrom::Instrument(sender), CableTo::GroupBus(lanes[0].node), &lanes, &groups);
        draw(&mut schedule, 6, CableFrom::Effect(sender), CableTo::Master, &lanes, &groups);
        assert!(schedule.cable_ids().is_empty(), "no end that is not there, and no id of a lane for a group or of a group for an instrument");
        draw(&mut schedule, 7, CableFrom::Instrument(sender), CableTo::Effect(fx), &lanes, &groups);
        schedule.refresh(&lanes, &groups, 2, 64);
        assert_eq!(schedule.cable_ids(), [EdgeId(7)]);
        // The lane that it starts at goes (the plan is made again for the lanes that are there): and the cable goes.
        let (mut lanes, groups) = (lanes, groups);
        lanes.remove(1);
        schedule.refresh(&lanes, &groups, 2, 64);
        assert!(schedule.cable_ids().is_empty());
        // The wiring that the plan was made for is the one without it: the next block makes no plan again.
        let made = schedule.compiles;
        schedule.refresh(&lanes, &groups, 2, 64);
        assert_eq!(schedule.compiles, made);
    }

    #[test]
    fn a_cable_that_is_drawn_again_at_other_ends_is_in_a_plan_that_has_them() {
        let mut schedule = Schedule::new();
        let (mut lanes, groups) = (lanes(2), groups(0));
        with_effects(&mut lanes[0], 1);
        let (sender, fx) = (lanes[1].node, lanes[0].effect_nodes[0]);
        draw(&mut schedule, 7, CableFrom::Instrument(sender), CableTo::Effect(fx), &lanes, &groups);
        schedule.refresh(&lanes, &groups, 2, 64);
        assert!(feeds(&schedule, &lanes, &groups).contains(&("cable 7".to_string(), "lane 0 fx 0".to_string())));
        let made = schedule.compiles;
        draw(&mut schedule, 7, CableFrom::Instrument(sender), CableTo::Master, &lanes, &groups);
        schedule.refresh(&lanes, &groups, 2, 64);
        assert_eq!(schedule.cable_ids(), [EdgeId(7)]);
        assert_eq!(schedule.compiles, made + 1, "other ends are another wiring");
        assert!(feeds(&schedule, &lanes, &groups).contains(&("cable 7".to_string(), "mix".to_string())));
        // The same ends again: nothing is made again.
        draw(&mut schedule, 7, CableFrom::Instrument(sender), CableTo::Master, &lanes, &groups);
        schedule.refresh(&lanes, &groups, 2, 64);
        assert_eq!(schedule.compiles, made + 1);
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
    fn a_lane_or_a_group_whose_way_out_is_cut_feeds_nothing_and_the_others_feed_as_they_did() {
        let mut schedule = Schedule::new();
        let (mut lanes, mut groups) = (lanes(4), groups(2));
        lanes[1].group = Some(0);
        lanes[2].group = Some(1);
        lanes[3].group = Some(1);
        lanes[0].route = Route::None; // would feed the mix
        lanes[1].route = Route::None; // would feed group 0
        groups[1].route = Route::None; // would feed the mix
        schedule.refresh(&lanes, &groups, 2, 64);
        assert_eq!(feeds(&schedule, &lanes, &groups), pairs(&[("lane 2 tail", "group 1 head"), ("lane 3 tail", "group 1 head"), ("group 0 tail", "mix")]));
        // The nodes are all there: the tail of a lane that goes nowhere still runs (it is measured, and it is what its mute is made at).
        let order = order(&schedule, &lanes, &groups);
        for node in ["lane 0 tail", "lane 1 tail", "group 1 tail"] {
            assert!(order.iter().any(|name| name == node), "{node} is in {order:?}");
        }
        // Given back, they feed as they did.
        lanes[0].route = Route::Out;
        lanes[1].route = Route::Out;
        groups[1].route = Route::Out;
        schedule.refresh(&lanes, &groups, 2, 64);
        assert_eq!(
            feeds(&schedule, &lanes, &groups),
            pairs(&[("lane 0 tail", "mix"), ("lane 1 tail", "group 0 head"), ("lane 2 tail", "group 1 head"), ("lane 3 tail", "group 1 head"), ("group 0 tail", "mix"), ("group 1 tail", "mix")])
        );
    }

    #[test]
    fn the_plan_is_made_again_when_a_way_out_is_cut_or_given_back_and_not_when_it_is_set_to_what_it_is() {
        let mut schedule = Schedule::new();
        let (mut lanes, mut groups) = (lanes(2), groups(1));
        schedule.refresh(&lanes, &groups, 2, 64);
        let made = schedule.compiles;
        lanes[0].route = Route::Out;
        groups[0].route = Route::Out;
        schedule.refresh(&lanes, &groups, 2, 64);
        assert_eq!(schedule.compiles, made, "the way out that it has");
        lanes[0].route = Route::None;
        schedule.refresh(&lanes, &groups, 2, 64);
        assert_eq!(schedule.compiles, made + 1, "a lane's way out that is cut");
        lanes[0].route = Route::None;
        schedule.refresh(&lanes, &groups, 2, 64);
        assert_eq!(schedule.compiles, made + 1, "cut again");
        groups[0].route = Route::None;
        schedule.refresh(&lanes, &groups, 2, 64);
        assert_eq!(schedule.compiles, made + 2, "a group's");
        lanes[0].route = Route::Out;
        schedule.refresh(&lanes, &groups, 2, 64);
        assert_eq!(schedule.compiles, made + 3, "given back");
    }

    /// Every link that the chains and the ways out make (no audio cables), `(from, to)`, in the order they are drawn: what `build_patch` connects.
    fn links(lanes: &[InstrumentLane], groups: &[Group]) -> Vec<(String, String)> {
        let (lane_wiring, group_wiring) = wiring_of(lanes, groups);
        let mut patch = Patch::new();
        build_patch(&mut patch, &lane_wiring, &group_wiring, std::iter::empty(), 2);
        patch.cables().iter().map(|cable| (name(cable.from.node, lanes, groups), name(cable.to.node, lanes, groups))).collect()
    }

    #[test]
    fn an_effect_whose_feed_is_cut_has_no_link_into_it_and_the_chain_goes_on_from_it() {
        let (mut lanes, mut groups) = (lanes(1), groups(1));
        with_effects(&mut lanes[0], 3);
        with_bus_effects(&mut groups[0], 2);
        lanes[0].group = Some(0);
        let all = pairs(&[
            ("lane 0 head", "lane 0 fx 0"),
            ("lane 0 fx 0", "lane 0 fx 1"),
            ("lane 0 fx 1", "lane 0 fx 2"),
            ("lane 0 fx 2", "lane 0 tail"),
            ("lane 0 tail", "group 0 head"),
            ("group 0 head", "group 0 fx 0"),
            ("group 0 fx 0", "group 0 fx 1"),
            ("group 0 fx 1", "group 0 tail"),
            ("group 0 tail", "mix"),
        ]);
        assert_eq!(links(&lanes, &groups), all);
        // The middle effect of the lane is not fed by the first: the link into it is gone, and the one out of it is not.
        let cut = lanes[0].effect_nodes[1];
        lanes[0].cut_feeds.push(cut);
        let without = |gone: &[(&str, &str)]| all.iter().filter(|link| !gone.iter().any(|(a, b)| (link.0.as_str(), link.1.as_str()) == (*a, *b))).cloned().collect::<Vec<_>>();
        assert_eq!(links(&lanes, &groups), without(&[("lane 0 fx 0", "lane 0 fx 1")]));
        // The first effect has the instrument before it, and a bus effect the sum of the bus.
        let (first, bus) = (lanes[0].effect_nodes[0], groups[0].effect_nodes[0]);
        lanes[0].cut_feeds.push(first);
        groups[0].cut_feeds.push(bus);
        assert_eq!(links(&lanes, &groups), without(&[("lane 0 fx 0", "lane 0 fx 1"), ("lane 0 head", "lane 0 fx 0"), ("group 0 head", "group 0 fx 0")]));
        // The link out of the end of a chain (into the tail, and the tail's way out) is not an effect's feed: cutting the last effect's feed leaves them.
        lanes[0].cut_feeds = vec![lanes[0].effect_nodes[2]];
        groups[0].cut_feeds.clear();
        assert_eq!(links(&lanes, &groups), without(&[("lane 0 fx 1", "lane 0 fx 2")]));
    }

    #[test]
    fn the_plan_is_made_again_when_a_feed_is_cut_or_given_back_and_not_when_it_is_set_to_what_it_is() {
        let mut schedule = Schedule::new();
        let (mut lanes, mut groups) = (lanes(1), groups(1));
        with_effects(&mut lanes[0], 2);
        with_bus_effects(&mut groups[0], 1);
        schedule.refresh(&lanes, &groups, 2, 64);
        let made = schedule.compiles;
        let (fx, bus) = (lanes[0].effect_nodes[1], groups[0].effect_nodes[0]);
        lanes[0].cut_feeds.clear();
        schedule.refresh(&lanes, &groups, 2, 64);
        assert_eq!(schedule.compiles, made, "the feeds that there are");
        lanes[0].cut_feeds.push(fx);
        schedule.refresh(&lanes, &groups, 2, 64);
        assert_eq!(schedule.compiles, made + 1, "a lane's effect whose feed is cut");
        schedule.refresh(&lanes, &groups, 2, 64);
        assert_eq!(schedule.compiles, made + 1, "the same");
        groups[0].cut_feeds.push(bus);
        schedule.refresh(&lanes, &groups, 2, 64);
        assert_eq!(schedule.compiles, made + 2, "a bus effect's");
        lanes[0].cut_feeds.clear();
        schedule.refresh(&lanes, &groups, 2, 64);
        assert_eq!(schedule.compiles, made + 3, "given back");
        // A cut that is on another effect is another wiring.
        let first = lanes[0].effect_nodes[0];
        lanes[0].cut_feeds.push(first);
        schedule.refresh(&lanes, &groups, 2, 64);
        assert_eq!(schedule.compiles, made + 4, "the first effect's feed cut, and the bus effect's as well");
        groups[0].cut_feeds.clear();
        schedule.refresh(&lanes, &groups, 2, 64);
        assert_eq!(schedule.compiles, made + 5, "the bus effect's given back");
        lanes[0].cut_feeds = vec![fx];
        schedule.refresh(&lanes, &groups, 2, 64);
        assert_eq!(schedule.compiles, made + 6, "the cut on the second effect and not the first is another wiring");
    }

    #[test]
    fn what_a_chain_link_that_is_cut_would_have_closed_a_loop_with_is_not_one() {
        // An effect that sends back to the effect before it in its own chain is a loop; with the link between them cut it is not.
        let mut schedule = Schedule::new();
        let (mut lanes, groups) = (lanes(1), groups(1));
        with_effects(&mut lanes[0], 2);
        let (first, second) = (lanes[0].effect_nodes[0], lanes[0].effect_nodes[1]);
        draw(&mut schedule, 1, CableFrom::Effect(second), CableTo::Effect(first), &lanes, &groups);
        assert!(schedule.cable_ids().is_empty(), "the loop is not drawn");
        lanes[0].cut_feeds.push(second);
        draw(&mut schedule, 1, CableFrom::Effect(second), CableTo::Effect(first), &lanes, &groups);
        assert_eq!(schedule.cable_ids(), [EdgeId(1)], "no loop: the second is not fed by the first");
        schedule.refresh(&lanes, &groups, 2, 64);
        // (The second effect has nothing before it, so it runs first of the chain, and the first, which only the cable feeds, runs after it, with its output going nowhere.)
        assert_eq!(order(&schedule, &lanes, &groups), ["lane 0 head", "lane 0 fx 1", "lane 0 tail", "cable 1", "lane 0 fx 0", "group 0 head", "group 0 tail", "mix"]);
    }

    #[test]
    fn what_a_lane_that_has_its_way_out_cut_would_have_closed_a_loop_with_is_not_one() {
        // A group's output sent to an effect of a lane that is in the group is a loop (the lane feeds the group, which feeds the lane); with the lane's way out cut it is not.
        let mut schedule = Schedule::new();
        let (mut lanes, groups) = (lanes(1), groups(1));
        with_effects(&mut lanes[0], 1);
        lanes[0].group = Some(0);
        let (group, fx) = (groups[0].node, lanes[0].effect_nodes[0]);
        draw(&mut schedule, 1, CableFrom::Group(group), CableTo::Effect(fx), &lanes, &groups);
        assert!(schedule.cable_ids().is_empty(), "the loop is not drawn");
        lanes[0].route = Route::None;
        draw(&mut schedule, 1, CableFrom::Group(group), CableTo::Effect(fx), &lanes, &groups);
        assert_eq!(schedule.cable_ids(), [EdgeId(1)], "no loop: the lane does not feed the group");
        schedule.refresh(&lanes, &groups, 2, 64);
        assert_eq!(order(&schedule, &lanes, &groups), ["lane 0 head", "group 0 head", "group 0 tail", "cable 1", "lane 0 fx 0", "lane 0 tail", "mix"]);
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
        let mut cx = BlockCx { midi: Events::keys(&[]), audio_out: &mut out, num_channels: 2, scale: Default::default(), telemetry: None, fall: None };
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

    // ---- latency (P3f of `docs/patch-graph.md` §11.6) ----------------------------------------------------------------------------------------------------------

    /// What the plugins of these tests have in common: no parameters and no presets, at 48 kHz.
    macro_rules! plain_plugin {
        () => {
            fn sample_rate(&self) -> f32 {
                48_000.0
            }
            fn parameters(&self) -> Vec<crate::plugin::ParameterInfo> {
                Vec::new()
            }
            fn get_parameter(&mut self, _: u32) -> Option<f32> {
                None
            }
            fn set_parameter(&mut self, index: u32, _: f32) -> anyhow::Result<()> {
                anyhow::bail!("no parameter {index}")
            }
            fn presets(&self) -> Vec<crate::plugin::Preset> {
                Vec::new()
            }
            fn load_preset(&mut self, id: &str) -> anyhow::Result<()> {
                anyhow::bail!("no preset {id}")
            }
        };
    }

    /// A plugin that is no more than a delay of `frames` frames, and says so, as a plugin with a look-ahead is to the host. It is a queue of its own, apart from the line that the host
    /// makes the paths late with, so that what the host does is held to a delay that was made another way. What it says can be changed (`said`) without changing what it does.
    struct Lagging {
        kept: Vec<VecDeque<f32>>,
        said: Arc<AtomicU32>,
    }

    impl Lagging {
        fn new(frames: usize) -> (Self, Arc<AtomicU32>) {
            let said = Arc::new(AtomicU32::new(frames as u32));
            (Lagging { kept: vec![VecDeque::from(vec![0.0; frames]); 2], said: said.clone() }, said)
        }
    }

    impl Plugin for Lagging {
        fn name(&self) -> &str {
            "Lagging"
        }
        fn is_instrument(&self) -> bool {
            false
        }
        fn audio_output_count(&self) -> usize {
            2
        }
        fn audio_input_count(&self) -> usize {
            2
        }
        fn process(&mut self, _: &[(u64, [u8; 3])], audio_in: &[&[f32]], audio_out: &mut [&mut [f32]]) -> anyhow::Result<()> {
            for ((out, input), kept) in audio_out.iter_mut().zip(audio_in).zip(&mut self.kept) {
                for (o, &i) in out.iter_mut().zip(input.iter()) {
                    kept.push_back(i);
                    *o = kept.pop_front().unwrap_or(0.0);
                }
            }
            Ok(())
        }
        fn latency(&self) -> u32 {
            self.said.load(Ordering::Relaxed)
        }
        plain_plugin!();
    }

    /// An instrument that puts out one frame of `level` on every channel `lag` frames after each note on, and says that it is late by that: a click, so that where a sound arrives is a
    /// frame number.
    struct Click {
        level: f32,
        lag: usize,
        pending: Vec<usize>,
        /// What it says its latency is: `lag` to begin with.
        said: Arc<AtomicU32>,
    }

    impl Plugin for Click {
        fn name(&self) -> &str {
            "Click"
        }
        fn is_instrument(&self) -> bool {
            true
        }
        fn audio_output_count(&self) -> usize {
            2
        }
        fn audio_input_count(&self) -> usize {
            0
        }
        fn process(&mut self, midi: &[(u64, [u8; 3])], _: &[&[f32]], audio_out: &mut [&mut [f32]]) -> anyhow::Result<()> {
            let frames = audio_out.first().map_or(0, |out| out.len());
            for &(frame, bytes) in midi {
                if bytes[0] & 0xF0 == 0x90 && bytes[2] > 0 {
                    self.pending.push(frame as usize + self.lag);
                }
            }
            self.pending.retain_mut(|at| {
                if *at < frames {
                    for channel in audio_out.iter_mut() {
                        channel[*at] = self.level;
                    }
                    false
                } else {
                    *at -= frames;
                    true
                }
            });
            Ok(())
        }
        fn latency(&self) -> u32 {
            self.said.load(Ordering::Relaxed)
        }
        plain_plugin!();
    }

    /// A lane whose instrument is a click of `level` that is `lag` frames late, and what it says its latency is.
    fn lane_of_click_saying(level: f32, lag: usize) -> (InstrumentLane, Arc<AtomicU32>) {
        let mut lane = InstrumentLane::new(super::test_node());
        lane.inst_buf = vec![Vec::new(), Vec::new()];
        let said = Arc::new(AtomicU32::new(lag as u32));
        // (The room for the clicks that are waiting is made here: the instrument of a test is held to asking for none while it runs, too.)
        lane.instrument = Some(Box::new(Click { level, lag, pending: Vec::with_capacity(64), said: said.clone() }));
        (lane, said)
    }

    /// A lane whose instrument is a click of `level` that is `lag` frames late.
    fn lane_of_click(level: f32, lag: usize) -> InstrumentLane {
        lane_of_click_saying(level, lag).0
    }

    /// Put an effect that is late by `frames` last in a lane's chain, blended in by `mix`: its id, and what it says its latency is.
    fn push_lagging(lane: &mut InstrumentLane, frames: usize, mix: f64) -> (NodeId, Arc<AtomicU32>) {
        let (plugin, said) = Lagging::new(frames);
        let id = super::test_node();
        lane.effects.push(Box::new(plugin));
        lane.mix_values.push(mix);
        lane.effect_nodes.push(id);
        (id, said)
    }

    /// The same for the bus of a group.
    fn push_bus_lagging(group: &mut Group, frames: usize, mix: f64) -> (NodeId, Arc<AtomicU32>) {
        let (plugin, said) = Lagging::new(frames);
        let id = super::test_node();
        group.effects.push(Box::new(plugin));
        group.mix_values.push(mix);
        group.effect_nodes.push(id);
        (id, said)
    }

    /// A note on at a frame of the whole render.
    fn note_at(frame: u64) -> (u64, [u8; 3]) {
        (frame, [0x90, 60, 100])
    }

    /// The frames `from` to `to` of the mix (its left side), in blocks of `block`, each made as `AudioGraph::process` makes one (the plan, the gates, the lanes and the groups), with the notes
    /// that are given by their frame in the whole render. A cable has come in over its first block, so a note that is to be heard through one is played after that.
    fn render(schedule: &mut Schedule, lanes: &mut [InstrumentLane], groups: &mut [Group], notes: &[(u64, [u8; 3])], block: usize, from: usize, to: usize) -> Vec<f32> {
        let mut all = Vec::new();
        let mut at = from;
        while at < to {
            let frames = block.min(to - at);
            schedule.refresh(lanes, groups, 2, frames);
            schedule.set_gates(lanes, groups);
            let here: Vec<(u64, [u8; 3])> = notes.iter().filter(|&&(frame, _)| frame >= at as u64 && frame < (at + frames) as u64).map(|&(frame, bytes)| (frame - at as u64, bytes)).collect();
            all.extend_from_slice(&run_with(schedule, lanes, groups, &here, frames)[0]);
            at += frames;
        }
        all
    }

    /// The frames at which the output is not silence, and what it is there (the first frame of `out` is the frame `from`).
    fn arrivals(out: &[f32], from: usize) -> Vec<(usize, f32)> {
        out.iter().enumerate().filter(|(_, x)| **x != 0.0).map(|(i, x)| (from + i, *x)).collect()
    }

    /// Block lengths that are shorter than the delays of these tests, about as long and longer, and one frame.
    const BLOCKS: [usize; 5] = [1, 7, 64, 256, 1000];

    #[test]
    fn a_session_where_nothing_has_latency_has_no_delay_and_no_latency() {
        let mut schedule = Schedule::new();
        let (mut lanes, mut groups) = (vec![lane_of_click(1.0, 0), lane_of_click(0.5, 0)], groups(1));
        lanes[0].group = Some(0);
        push_lagging(&mut lanes[0], 0, 0.5);
        let out = render(&mut schedule, &mut lanes, &mut groups, &[note_at(1000)], 256, 0, 1400);
        assert_eq!(arrivals(&out, 0), [(1000, 1.5)]);
        assert_eq!((schedule.delay_frames(), schedule.latency(), schedule.dry_frames(lanes[0].effect_nodes[0])), (vec![], 0, None));
        assert!(schedule.stages().iter().all(|stage| !matches!(stage, Stage::Delay(_))));
    }

    #[test]
    fn a_lane_that_has_no_latency_is_delayed_by_the_latency_of_the_one_it_is_added_to_at_the_mix() {
        for block in BLOCKS {
            let mut schedule = Schedule::new();
            let (mut lanes, mut groups) = (vec![lane_of_click(1.0, 0), lane_of_click(0.5, 0)], groups(0));
            push_lagging(&mut lanes[0], 23, 1.0);
            let out = render(&mut schedule, &mut lanes, &mut groups, &[note_at(1000)], block, 0, 1400);
            assert_eq!(arrivals(&out, 0), [(1023, 1.5)], "blocks of {block}");
            assert_eq!(schedule.delay_frames(), [(Stage::Tail(lanes[1].node), Stage::Master, 23)], "blocks of {block}");
            assert_eq!(schedule.latency(), 23);
        }
    }

    #[test]
    fn the_lanes_of_a_group_are_in_step_at_its_bus_and_what_goes_round_it_is_as_late_as_the_group_is_at_the_mix() {
        // Lane 0 (late by 23) and lane 1 are added at the bus of group 0, whose own effect is late by 7; lane 2 goes to the mix, where it is added to the group.
        for block in BLOCKS {
            let mut schedule = Schedule::new();
            let (mut lanes, mut groups) = (vec![lane_of_click(1.0, 0), lane_of_click(0.5, 0), lane_of_click(0.25, 0)], groups(1));
            lanes[0].group = Some(0);
            lanes[1].group = Some(0);
            push_lagging(&mut lanes[0], 23, 1.0);
            push_bus_lagging(&mut groups[0], 7, 1.0);
            let out = render(&mut schedule, &mut lanes, &mut groups, &[note_at(1000)], block, 0, 1400);
            assert_eq!(arrivals(&out, 0), [(1030, 1.75)], "blocks of {block}");
            let mut delays = schedule.delay_frames();
            delays.sort_by_key(|&(from, _, _)| from);
            let mut want = vec![(Stage::Tail(lanes[1].node), Stage::Head(groups[0].node), 23), (Stage::Tail(lanes[2].node), Stage::Master, 30)];
            want.sort_by_key(|&(from, _, _)| from);
            assert_eq!(delays, want, "blocks of {block}");
            assert_eq!(schedule.latency(), 30);
        }
    }

    #[test]
    fn the_paths_that_are_added_where_an_audio_cable_ends_are_in_step() {
        // Lane 0's chain is an effect that is late by 23 and one that is not; lane 1's is one that is late by 10, and a cable goes from it to the second effect of lane 0, where it is added to
        // what comes from the first. The cable (and lane 1's own way to the mix) is delayed by the 13 that it lacks.
        for block in BLOCKS {
            let mut schedule = Schedule::new();
            let (mut lanes, mut groups) = (vec![lane_of_click(1.0, 0), lane_of_click(0.5, 0)], groups(0));
            push_lagging(&mut lanes[0], 23, 1.0);
            let (second, _) = push_lagging(&mut lanes[0], 0, 1.0);
            let (late_by_ten, _) = push_lagging(&mut lanes[1], 10, 1.0);
            schedule.add_cable(Cable::new(EdgeId(1), CableFrom::Effect(late_by_ten), CableTo::Effect(second), 1.0), &lanes, &groups, 2);
            let out = render(&mut schedule, &mut lanes, &mut groups, &[note_at(1000)], block, 0, 1400);
            assert_eq!(arrivals(&out, 0), [(1023, 2.0)], "blocks of {block}: lane 0's click and lane 1's, which reaches the mix by its cable and by its own way, and each is heard at its own level");
            assert_eq!(schedule.delay_frames(), [(Stage::Tail(lanes[1].node), Stage::Master, 13), (Stage::Cable(EdgeId(1)), Stage::Fx(second), 13)], "blocks of {block}");
        }
    }

    #[test]
    fn what_an_instrument_that_is_late_makes_is_late_in_its_lane_and_the_other_lanes_wait_for_it() {
        let mut schedule = Schedule::new();
        let (mut lanes, mut groups) = (vec![lane_of_click(1.0, 12), lane_of_click(0.5, 0)], groups(0));
        let out = render(&mut schedule, &mut lanes, &mut groups, &[note_at(1000)], 64, 0, 1400);
        assert_eq!(arrivals(&out, 0), [(1012, 1.5)]);
        assert_eq!(schedule.delay_frames(), [(Stage::Tail(lanes[1].node), Stage::Master, 12)]);
        assert_eq!(schedule.latency(), 12);
    }

    #[test]
    fn the_dry_part_of_an_effect_that_is_late_is_as_late_as_the_rest_of_it() {
        // Half of the sound is the effect's (late by 23) and half is what was put in: they are heard together, and none of it 23 frames before.
        for block in BLOCKS {
            let mut schedule = Schedule::new();
            let (mut lanes, mut groups) = (vec![lane_of_click(1.0, 0)], groups(0));
            let (effect, _) = push_lagging(&mut lanes[0], 23, 0.5);
            let out = render(&mut schedule, &mut lanes, &mut groups, &[note_at(1000)], block, 0, 1400);
            assert_eq!(arrivals(&out, 0), [(1023, 1.0)], "blocks of {block}");
            assert_eq!((schedule.dry_frames(effect), schedule.delay_frames(), schedule.latency()), (Some(23), vec![], 23));
        }
    }

    #[test]
    fn the_dry_part_of_a_bus_effect_that_is_late_is_as_late_as_the_rest_of_it() {
        let mut schedule = Schedule::new();
        let (mut lanes, mut groups) = (vec![lane_of_click(1.0, 0)], groups(1));
        lanes[0].group = Some(0);
        let (effect, _) = push_bus_lagging(&mut groups[0], 23, 0.25);
        let out = render(&mut schedule, &mut lanes, &mut groups, &[note_at(1000)], 64, 0, 1400);
        assert_eq!(arrivals(&out, 0), [(1023, 1.0)]);
        assert_eq!(schedule.dry_frames(effect), Some(23));
    }

    #[test]
    fn the_dry_line_has_what_was_put_in_while_the_mix_was_all_wet_so_that_the_mix_can_be_turned_down() {
        for block in [1usize, 7, 64] {
            let mut schedule = Schedule::new();
            let (mut lanes, mut groups) = (vec![lane_of_click(1.0, 0)], groups(0));
            push_lagging(&mut lanes[0], 23, 1.0);
            // The click is at 1000 and the effect's own is at 1023; the mix is turned down at 1010, in between.
            let mut out = render(&mut schedule, &mut lanes, &mut groups, &[note_at(1000)], block, 0, 1010);
            lanes[0].mix_values[0] = 0.5;
            out.extend(render(&mut schedule, &mut lanes, &mut groups, &[], block, 1010, 1400));
            assert_eq!(arrivals(&out, 0), [(1023, 1.0)], "blocks of {block}");
        }
    }

    #[test]
    fn a_plan_that_is_made_again_keeps_what_the_delays_hold() {
        // The click of lane 1 is in the delay that is between it and the mix when an effect is put in its chain, which makes the plan again.
        for block in [1usize, 7, 64] {
            let mut schedule = Schedule::new();
            let (mut lanes, mut groups) = (vec![lane_of_click(1.0, 0), lane_of_click(0.5, 0)], groups(0));
            push_lagging(&mut lanes[0], 23, 1.0);
            let mut out = render(&mut schedule, &mut lanes, &mut groups, &[note_at(1000)], block, 0, 1005);
            let plans = schedule.plans_made();
            push_lagging(&mut lanes[1], 0, 1.0);
            out.extend(render(&mut schedule, &mut lanes, &mut groups, &[], block, 1005, 1400));
            assert_eq!(schedule.plans_made(), plans + 1, "the plan was made again");
            assert_eq!(arrivals(&out, 0), [(1023, 1.5)], "blocks of {block}");
        }
    }

    #[test]
    fn the_dry_line_of_an_effect_goes_through_a_plan_that_is_made_again() {
        let mut schedule = Schedule::new();
        let (mut lanes, mut groups) = (vec![lane_of_click(1.0, 0), lane_of_click(0.5, 0)], groups(0));
        push_lagging(&mut lanes[0], 23, 0.5);
        let mut out = render(&mut schedule, &mut lanes, &mut groups, &[note_at(1000)], 7, 0, 1005);
        push_lagging(&mut lanes[1], 0, 1.0);
        out.extend(render(&mut schedule, &mut lanes, &mut groups, &[], 7, 1005, 1400));
        // Lane 0: the dry click and the effect's are the same frame; lane 1 is delayed to meet them.
        assert_eq!(arrivals(&out, 0), [(1023, 1.5)]);
    }

    #[test]
    fn a_latency_that_changes_makes_the_plan_again_and_the_delays_are_what_it_says_now() {
        let mut schedule = Schedule::new();
        let (mut lanes, mut groups) = (vec![lane_of_click(1.0, 0), lane_of_click(0.5, 0)], groups(0));
        let (effect, said) = push_lagging(&mut lanes[0], 23, 0.5);
        render(&mut schedule, &mut lanes, &mut groups, &[], 64, 0, 128);
        let plans = schedule.plans_made();
        assert_eq!((schedule.latency(), schedule.dry_frames(effect), schedule.delay_frames()), (23, Some(23), vec![(Stage::Tail(lanes[1].node), Stage::Master, 23)]));
        render(&mut schedule, &mut lanes, &mut groups, &[], 64, 128, 256);
        assert_eq!(schedule.plans_made(), plans, "what it says is what it said");
        said.store(40, Ordering::Relaxed);
        render(&mut schedule, &mut lanes, &mut groups, &[], 64, 256, 320);
        assert_eq!(schedule.plans_made(), plans + 1);
        assert_eq!((schedule.latency(), schedule.dry_frames(effect), schedule.delay_frames()), (40, Some(40), vec![(Stage::Tail(lanes[1].node), Stage::Master, 40)]));
        said.store(0, Ordering::Relaxed);
        render(&mut schedule, &mut lanes, &mut groups, &[], 64, 320, 384);
        assert_eq!((schedule.latency(), schedule.dry_frames(effect), schedule.delay_frames()), (0, None, vec![]));
    }

    #[test]
    fn an_effect_that_is_taken_out_takes_its_delays_with_it() {
        let mut schedule = Schedule::new();
        let (mut lanes, mut groups) = (vec![lane_of_click(1.0, 0), lane_of_click(0.5, 0)], groups(0));
        let (effect, _) = push_lagging(&mut lanes[0], 23, 0.5);
        render(&mut schedule, &mut lanes, &mut groups, &[], 64, 0, 128);
        assert_eq!(schedule.delay_frames().len(), 1);
        lanes[0].effects.pop();
        lanes[0].mix_values.pop();
        lanes[0].effect_nodes.pop();
        render(&mut schedule, &mut lanes, &mut groups, &[], 64, 128, 256);
        assert_eq!((schedule.latency(), schedule.dry_frames(effect), schedule.delay_frames()), (0, None, vec![]));
    }

    #[test]
    fn a_latency_that_is_absurd_is_held_to_the_most_a_delay_may_have() {
        let mut schedule = Schedule::new();
        let (mut lanes, mut groups) = (vec![lane_of_click(1.0, 0), lane_of_click(0.5, 0)], groups(0));
        let (effect, said) = push_lagging(&mut lanes[0], 0, 0.5);
        said.store(u32::MAX, Ordering::Relaxed);
        render(&mut schedule, &mut lanes, &mut groups, &[], 64, 0, 128);
        assert_eq!((schedule.latency(), schedule.dry_frames(effect)), (MAX_LATENCY, Some(MAX_LATENCY as usize)));
        assert_eq!(schedule.delay_frames(), [(Stage::Tail(lanes[1].node), Stage::Master, MAX_LATENCY as usize)]);
    }

    #[test]
    fn a_block_with_delays_and_dry_lines_in_the_plan_asks_the_allocator_for_nothing() {
        let mut schedule = Schedule::new();
        // Lanes 0 and 1 are in the group, whose bus is late by 7, and lane 2 goes to the mix: a delay for lane 1 at the bus and one for lane 2 at the mix.
        let (mut lanes, mut groups) = (vec![lane_of_click(1.0, 0), lane_of_click(0.5, 0), lane_of_click(0.25, 0)], groups(1));
        lanes[0].group = Some(0);
        lanes[1].group = Some(0);
        push_lagging(&mut lanes[0], 23, 0.5);
        push_bus_lagging(&mut groups[0], 7, 0.5);
        // These blocks make the plan, its delays and its dry lines, and may ask for memory, as the first block of a session does.
        render(&mut schedule, &mut lanes, &mut groups, &[], 256, 0, 512);
        assert!(schedule.delay_frames().len() == 2 && schedule.dry_frames(lanes[0].effect_nodes[0]).is_some());
        let mut master = Master::new();
        let mut out = vec![vec![0.0f32; 256]; 2];
        let mut cx = BlockCx { midi: Events::keys(&NOTE_ON), audio_out: &mut out, num_channels: 2, scale: Default::default(), telemetry: None, fall: None };
        let (_, asked) = allocations_in(|| {
            for _ in 0..8 {
                schedule.refresh(&lanes, &groups, 2, 256);
                schedule.set_gates(&mut lanes, &mut groups);
                schedule.run(&mut lanes, &mut groups, &mut master, 256, &mut cx).unwrap();
            }
        });
        assert_eq!(asked, 0);
        assert!(loudest(&out) > 0.0, "the blocks were heard");
    }

    #[test]
    fn the_latency_of_an_instrument_and_of_effects_in_other_lanes_are_each_found_for_their_own_node() {
        // Lane 0's instrument is late by 12, lane 1's effect by 5, lane 2's by 3 and its instrument by 7: lane 0 is 12, lane 1 is 5 and lane 2 is 10 late at the mix, and the
        // click that each makes arrives at the latest of them.
        let mut schedule = Schedule::new();
        let mut lanes = vec![lane_of_click(1.0, 12), lane_of_click(0.5, 0), lane_of_click(0.25, 7)];
        let mut groups = groups(0);
        push_lagging(&mut lanes[1], 5, 1.0);
        push_lagging(&mut lanes[2], 3, 1.0);
        let out = render(&mut schedule, &mut lanes, &mut groups, &[note_at(1000)], 64, 0, 1400);
        assert_eq!(arrivals(&out, 0), [(1012, 1.75)]);
        let mut delays = schedule.delay_frames();
        delays.sort_by_key(|&(from, _, _)| from);
        let mut want = vec![(Stage::Tail(lanes[1].node), Stage::Master, 7), (Stage::Tail(lanes[2].node), Stage::Master, 2)];
        want.sort_by_key(|&(from, _, _)| from);
        assert_eq!(delays, want);
        assert_eq!(schedule.latency(), 12);
    }

    #[test]
    fn the_latency_that_an_instrument_says_changes_the_plan_when_it_changes() {
        let mut schedule = Schedule::new();
        let (first, said) = lane_of_click_saying(1.0, 0);
        let (mut lanes, mut groups) = (vec![first, lane_of_click(0.5, 0)], groups(0));
        render(&mut schedule, &mut lanes, &mut groups, &[], 64, 0, 128);
        let plans = schedule.plans_made();
        assert_eq!((schedule.latency(), schedule.delay_frames()), (0, vec![]));
        said.store(30, Ordering::Relaxed);
        render(&mut schedule, &mut lanes, &mut groups, &[], 64, 128, 192);
        assert_eq!(schedule.plans_made(), plans + 1);
        assert_eq!((schedule.latency(), schedule.delay_frames()), (30, vec![(Stage::Tail(lanes[1].node), Stage::Master, 30)]));
    }

    #[test]
    fn the_latency_that_a_bus_effect_says_changes_the_plan_when_it_changes() {
        let mut schedule = Schedule::new();
        let (mut lanes, mut groups) = (vec![lane_of_click(1.0, 0), lane_of_click(0.5, 0)], groups(1));
        lanes[0].group = Some(0);
        let (effect, said) = push_bus_lagging(&mut groups[0], 9, 0.5);
        render(&mut schedule, &mut lanes, &mut groups, &[], 64, 0, 128);
        let plans = schedule.plans_made();
        assert_eq!((schedule.latency(), schedule.dry_frames(effect), schedule.delay_frames()), (9, Some(9), vec![(Stage::Tail(lanes[1].node), Stage::Master, 9)]));
        said.store(21, Ordering::Relaxed);
        render(&mut schedule, &mut lanes, &mut groups, &[], 64, 128, 192);
        assert_eq!(schedule.plans_made(), plans + 1);
        assert_eq!((schedule.latency(), schedule.dry_frames(effect), schedule.delay_frames()), (21, Some(21), vec![(Stage::Tail(lanes[1].node), Stage::Master, 21)]));
    }

    #[test]
    fn delays_that_end_at_one_node_each_keep_their_own_line_when_the_plan_is_made_again() {
        // Lane 0 is late by 40, lane 1 by 10 and lane 2 by nothing, all at the mix: lane 1 is delayed by 30 and lane 2 by 40, and the plan is made again 5 frames after the click, with
        // the click of each in its line. Each arrives with the latest, and a line that went to the other cable would be too short or too long for the click that is in it.
        for block in [1usize, 7, 64] {
            let mut schedule = Schedule::new();
            let (mut lanes, mut groups) = (vec![lane_of_click(1.0, 0), lane_of_click(0.5, 0), lane_of_click(0.25, 0)], groups(0));
            push_lagging(&mut lanes[0], 40, 1.0);
            push_lagging(&mut lanes[1], 10, 1.0);
            let mut out = render(&mut schedule, &mut lanes, &mut groups, &[note_at(1000)], block, 0, 1005);
            let plans = schedule.plans_made();
            push_lagging(&mut lanes[2], 0, 1.0);
            out.extend(render(&mut schedule, &mut lanes, &mut groups, &[], block, 1005, 1400));
            assert_eq!(schedule.plans_made(), plans + 1, "the plan was made again");
            assert_eq!(arrivals(&out, 0), [(1040, 1.75)], "blocks of {block}");
        }
    }

    #[test]
    fn a_schedule_that_does_not_compensate_has_no_delay_and_makes_no_plan_for_a_latency_that_changes() {
        let mut schedule = Schedule::new();
        schedule.without_compensation();
        let (mut lanes, mut groups) = (vec![lane_of_click(1.0, 0), lane_of_click(0.5, 0)], groups(0));
        let (effect, said) = push_lagging(&mut lanes[0], 23, 0.5);
        let out = render(&mut schedule, &mut lanes, &mut groups, &[note_at(1000)], 64, 0, 1400);
        assert_eq!(arrivals(&out, 0), [(1000, 1.0), (1023, 0.5)], "the effect's own sound is late and the sound that was put in is not: nothing is made up for");
        assert_eq!((schedule.latency(), schedule.delay_frames(), schedule.dry_frames(effect)), (0, vec![], None));
        let plans = schedule.plans_made();
        said.store(40, Ordering::Relaxed);
        render(&mut schedule, &mut lanes, &mut groups, &[], 64, 1400, 1464);
        assert_eq!(schedule.plans_made(), plans, "a latency that changes is nothing to a schedule that does not make it up");
    }

    #[test]
    fn delays_that_end_at_one_node_keep_their_own_line_when_the_lanes_change_places() {
        // The same lanes as above, and the plan is made again by two of them swapping places, which puts the delays in the other order: each keeps the line with its click in it.
        for block in [1usize, 7, 64] {
            let mut schedule = Schedule::new();
            let (mut lanes, mut groups) = (vec![lane_of_click(1.0, 0), lane_of_click(0.5, 0), lane_of_click(0.25, 0)], groups(0));
            push_lagging(&mut lanes[0], 40, 1.0);
            push_lagging(&mut lanes[1], 10, 1.0);
            let mut out = render(&mut schedule, &mut lanes, &mut groups, &[note_at(1000)], block, 0, 1005);
            let plans = schedule.plans_made();
            lanes.swap(1, 2);
            out.extend(render(&mut schedule, &mut lanes, &mut groups, &[], block, 1005, 1400));
            assert_eq!(schedule.plans_made(), plans + 1, "the plan was made again");
            assert_eq!(arrivals(&out, 0), [(1040, 1.75)], "blocks of {block}");
        }
    }

    #[test]
    fn a_dry_line_stays_with_its_effect_when_an_effect_before_it_is_taken_out() {
        // Two effects that are late have a dry line each, half of their mix; the first is taken out 5 frames after the click, and the second's dry sound, which has the click in it, is
        // still its own (the first's dry line, which has another click in it, is not its dry line any more).
        for block in [1usize, 7, 64] {
            let mut schedule = Schedule::new();
            let (mut lanes, mut groups) = (vec![lane_of_click(1.0, 0), lane_of_click(0.5, 0)], groups(0));
            push_lagging(&mut lanes[0], 23, 0.5);
            let (second, _) = push_lagging(&mut lanes[1], 10, 0.5);
            let mut out = render(&mut schedule, &mut lanes, &mut groups, &[note_at(1000)], block, 0, 1005);
            lanes[0].effects.pop();
            lanes[0].mix_values.pop();
            lanes[0].effect_nodes.pop();
            out.extend(render(&mut schedule, &mut lanes, &mut groups, &[], block, 1005, 1400));
            assert_eq!(schedule.dry_frames(second), Some(10));
            assert_eq!(arrivals(&out, 0), [(1010, 0.5)], "blocks of {block}: half of lane 1's click as it was put in and half of it from its effect, 10 frames later, and nothing of lane 0's");
        }
    }

    #[test]
    fn a_schedule_that_does_not_compensate_makes_no_plan_for_a_latency_that_an_instrument_or_a_bus_effect_says() {
        let mut schedule = Schedule::new();
        schedule.without_compensation();
        let (first, instrument_said) = lane_of_click_saying(1.0, 0);
        let (mut lanes, mut groups) = (vec![first, lane_of_click(0.5, 0)], groups(1));
        lanes[0].group = Some(0);
        let (bus_effect, bus_said) = push_bus_lagging(&mut groups[0], 9, 0.5);
        render(&mut schedule, &mut lanes, &mut groups, &[], 64, 0, 128);
        let plans = schedule.plans_made();
        instrument_said.store(30, Ordering::Relaxed);
        bus_said.store(40, Ordering::Relaxed);
        render(&mut schedule, &mut lanes, &mut groups, &[], 64, 128, 192);
        assert_eq!(schedule.plans_made(), plans);
        assert_eq!((schedule.latency(), schedule.delay_frames(), schedule.dry_frames(bus_effect)), (0, vec![], None));
    }

    /// Set the parameter of a plugin that has `name`.
    fn set_named(plugin: &mut dyn Plugin, name: &str, value: f32) {
        let index = plugin.parameters().iter().position(|p| p.name == name).unwrap_or_else(|| panic!("a parameter called {name}"));
        plugin.set_parameter(index as u32, value).unwrap();
    }

    /// The oscillator's waveforms, by their place in its list.
    const SINE: f32 = 0.0;
    const SAW: f32 = 3.0;

    /// A lane with an oscillator of the waveform and volume given, and, if `drive` is a mix, a drive that lets a quiet sound through as it is (no drive, none of the tone filter, the
    /// full level), blended in by that mix.
    fn lane_of_oscillator(waveform: f32, volume: f32, drive: Option<f64>) -> InstrumentLane {
        let mut lane = InstrumentLane::new(super::test_node());
        let mut osc = crate::plugin::load("builtin:osc", 48_000.0, 256, &Default::default()).unwrap();
        set_named(osc.as_mut(), "waveform", waveform);
        set_named(osc.as_mut(), "volume", volume);
        lane.inst_buf = (0..osc.audio_output_count()).map(|_| Vec::new()).collect();
        lane.instrument = Some(osc);
        if let Some(mix) = drive {
            let mut plugin = crate::plugin::load("builtin:drive", 48_000.0, 256, &Default::default()).unwrap();
            set_named(plugin.as_mut(), "drive", 0.0);
            set_named(plugin.as_mut(), "tone", 1.0);
            set_named(plugin.as_mut(), "level", 1.0);
            lane.effects.push(plugin);
            lane.mix_values.push(mix);
            lane.effect_nodes.push(super::test_node());
        }
        lane
    }

    #[test]
    fn two_lanes_one_through_the_drive_are_in_step_at_the_mix_as_the_drive_says() {
        // The drive says 23 frames. What the two lanes make together is what the first makes alone and what the second makes alone, 23 frames later: the sound that has no drive waits.
        let notes = [note_at(100)];
        for block in [64usize, 256, 1000] {
            let (mut a, mut b) = (Schedule::new(), Schedule::new());
            let alone_a = render(&mut a, &mut [lane_of_oscillator(SINE, 0.5, Some(1.0))], &mut [], &notes, block, 0, 4800);
            let alone_b = render(&mut b, &mut [lane_of_oscillator(SAW, 0.25, None)], &mut [], &notes, block, 0, 4800);
            let mut together = Schedule::new();
            let mut lanes = [lane_of_oscillator(SINE, 0.5, Some(1.0)), lane_of_oscillator(SAW, 0.25, None)];
            let both = render(&mut together, &mut lanes, &mut [], &notes, block, 0, 4800);
            let want: Vec<f32> = (0..4800).map(|n| alone_a[n] + if n >= 23 { alone_b[n - 23] } else { 0.0 }).collect();
            assert!(loudest(std::slice::from_ref(&both)) > 0.05, "something is heard");
            assert_eq!(both, want, "blocks of {block}");
            assert_eq!(together.latency(), 23);
        }
    }

    #[test]
    fn a_drive_that_is_blended_with_the_sound_that_went_in_does_not_make_a_comb_of_the_two() {
        // A quiet sine through a drive that does nothing to it but make it late, at half the mix: it is the sine, 23 frames late. (With the dry sound as it went in, the blend is
        // 0.5 x(t) + 0.5 x(t - 23): 39% of the sine's height off the sine that is 23 frames late, at this pitch, and a few percent of the sine at 1 kHz.)
        let notes = [note_at(100)];
        let mut plain = Schedule::new();
        let input = render(&mut plain, &mut [lane_of_oscillator(SINE, 0.15, None)], &mut [], &notes, 256, 0, 4800);
        let mut blended = Schedule::new();
        let out = render(&mut blended, &mut [lane_of_oscillator(SINE, 0.15, Some(0.5))], &mut [], &notes, 256, 0, 4800);
        let worst = (1000..4800).map(|n| (out[n] - input[n - 23]).abs()).fold(0.0f32, f32::max);
        assert!(loudest(&[input[1000..].to_vec()]) > 0.03, "a sine of 0.04 or so is heard");
        assert!(worst < 0.0001, "{worst} from the sine that went in, 23 frames late");
        let comb = (1000..4800).map(|n| (0.5 * input[n] + 0.5 * input[n - 23] - input[n - 23]).abs()).fold(0.0f32, f32::max);
        assert!(comb > 0.01, "the blend that the host used to make is {comb} from it, so that this test would see it");
    }

    // ---- a second input on an effect (step 95, the small half of P3g) ----------------------------------------------------------------------------------------

    /// An effect with a second input, as the host sees one: it puts out its first input and ten times its second, and where nothing is patched at the second it puts out a quarter more, so
    /// that what the host told it can be heard.
    struct Sided;

    impl Plugin for Sided {
        fn name(&self) -> &str {
            "Sided"
        }
        fn is_instrument(&self) -> bool {
            false
        }
        fn audio_output_count(&self) -> usize {
            2
        }
        fn audio_input_count(&self) -> usize {
            2
        }
        fn side_input(&self) -> Option<crate::plugin::SideInput> {
            Some(crate::plugin::SideInput { main: "A", side: "B" })
        }
        fn process(&mut self, midi: &[(u64, [u8; 3])], audio_in: &[&[f32]], audio_out: &mut [&mut [f32]]) -> anyhow::Result<()> {
            self.process_side(midi, audio_in, None, audio_out)
        }
        fn process_side(&mut self, _: &[(u64, [u8; 3])], audio_in: &[&[f32]], side: Option<&[&[f32]]>, audio_out: &mut [&mut [f32]]) -> anyhow::Result<()> {
            for (channel, (out, input)) in audio_out.iter_mut().zip(audio_in).enumerate() {
                for (frame, (o, &i)) in out.iter_mut().zip(input.iter()).enumerate() {
                    *o = i + match side {
                        Some(side) => 10.0 * side[channel][frame],
                        None => 0.25,
                    };
                }
            }
            Ok(())
        }
        plain_plugin!();
    }

    /// Put an effect that has a second input last in a lane's chain, at a mix of 1: its id.
    fn push_sided(lane: &mut InstrumentLane) -> NodeId {
        let id = super::test_node();
        lane.effects.push(Box::new(Sided));
        lane.mix_values.push(1.0);
        lane.effect_nodes.push(id);
        id
    }

    /// The same for the bus of a group.
    fn push_bus_sided(group: &mut Group) -> NodeId {
        let id = super::test_node();
        group.effects.push(Box::new(Sided));
        group.mix_values.push(1.0);
        group.effect_nodes.push(id);
        id
    }

    /// How many input ports the node of an effect has in the plan.
    fn input_ports(schedule: &Schedule, effect: NodeId) -> usize {
        schedule.patch.nodes().iter().find(|(stage, _)| *stage == Stage::Fx(effect)).map(|(_, spec)| spec.inputs.len()).expect("the effect is in the plan")
    }

    /// A lane that sends what it plays and nothing else: a click of `level`, with its way out cut, so that the mix has it only through its cables.
    fn sender(level: f32) -> InstrumentLane {
        let mut lane = lane_of_click(level, 0);
        lane.route = Route::None;
        lane
    }

    /// A lane that has no instrument and has an effect with a second input: what a module that stands by itself is, in the plan.
    fn receiver() -> (InstrumentLane, NodeId) {
        let mut lane = InstrumentLane::new(super::test_node());
        let fx = push_sided(&mut lane);
        (lane, fx)
    }

    #[test]
    fn an_effect_that_has_a_second_input_has_a_second_port_in_the_plan_and_the_others_have_the_one() {
        let mut schedule = Schedule::new();
        let (mut lanes, mut groups) = (lanes(1), groups(1));
        with_effects(&mut lanes[0], 1);
        let sided = push_sided(&mut lanes[0]);
        with_bus_effects(&mut groups[0], 1);
        let bus_sided = push_bus_sided(&mut groups[0]);
        schedule.refresh(&lanes, &groups, 2, 64);
        assert_eq!([lanes[0].effect_nodes[0], sided, groups[0].effect_nodes[0], bus_sided].map(|effect| input_ports(&schedule, effect)), [1, 2, 1, 2]);
        // (The second port has nothing in it that anything feeds: the plan is what it was, for the rest.)
        assert!(feeds(&schedule, &lanes, &groups).iter().all(|(_, to)| !to.contains("fx 1") || to == "lane 0 fx 1"), "{:?}", feeds(&schedule, &lanes, &groups));
    }

    #[test]
    fn a_cable_that_ends_at_the_second_input_is_what_the_plugin_is_given_there_and_is_added_to_nothing_else() {
        let mut schedule = Schedule::new();
        let (mut lanes, groups) = (vec![], groups(0));
        let (receiving, fx) = receiver();
        lanes.push(receiving);
        lanes.push(sender(0.5));
        let from = CableFrom::Instrument(lanes[1].node);
        draw(&mut schedule, 3, from, CableTo::EffectSide(fx), &lanes, &groups);
        assert_eq!(schedule.cable_ids(), [EdgeId(3)], "the cable is drawn");
        // The cable comes in over its first block; a note in the second is heard through it, ten times, at the frame it is played at, and nothing else is heard.
        let out = render(&mut schedule, &mut lanes, &mut [], &[note_at(70)], 64, 0, 128);
        assert_eq!(arrivals(&out, 0), [(70, 5.0)], "the click, ten times, from the second input: and nothing from the first, and no quarter, since something is patched");
    }

    #[test]
    fn an_effect_that_has_nothing_patched_at_its_second_input_is_told_so_and_a_cable_at_the_first_is_not_a_cable_at_the_second() {
        let mut schedule = Schedule::new();
        let (mut lanes, groups) = (vec![], groups(0));
        let (receiving, fx) = receiver();
        lanes.push(receiving);
        lanes.push(sender(0.5));
        // With no cable: the quarter, at every frame (the plugin was told that nothing is patched, and the port is silence).
        let out = render(&mut schedule, &mut lanes, &mut [], &[note_at(70)], 64, 0, 128);
        assert!(out.iter().all(|x| *x == 0.25), "{:?}", &out[60..80]);
        // A cable at the first input: the click is added to what the chain gives, and the second input has nothing, so the plugin says so.
        let from = CableFrom::Instrument(lanes[1].node);
        draw(&mut schedule, 4, from, CableTo::Effect(fx), &lanes, &groups);
        let out = render(&mut schedule, &mut lanes, &mut [], &[note_at(200)], 64, 128, 256);
        assert_eq!(out[200 - 128], 0.5 + 0.25, "the click through the first input, and the quarter of an input with no cable");
        assert_eq!(out[199 - 128], 0.25);
    }

    #[test]
    fn a_cable_at_each_input_gives_the_plugin_both() {
        let mut schedule = Schedule::new();
        let (mut lanes, groups) = (vec![], groups(0));
        let (receiving, fx) = receiver();
        lanes.push(receiving);
        lanes.push(sender(0.5));
        lanes.push(sender(0.25));
        let (a, b) = (CableFrom::Instrument(lanes[1].node), CableFrom::Instrument(lanes[2].node));
        draw(&mut schedule, 1, a, CableTo::Effect(fx), &lanes, &groups);
        draw(&mut schedule, 2, b, CableTo::EffectSide(fx), &lanes, &groups);
        let out = render(&mut schedule, &mut lanes, &mut [], &[note_at(70)], 64, 0, 128);
        assert_eq!(arrivals(&out, 0), [(70, 0.5 + 10.0 * 0.25)]);
    }

    #[test]
    fn two_cables_at_the_second_input_are_added_there() {
        let mut schedule = Schedule::new();
        let (mut lanes, groups) = (vec![], groups(0));
        let (receiving, fx) = receiver();
        lanes.push(receiving);
        lanes.push(sender(0.5));
        lanes.push(sender(0.25));
        for (id, lane) in [(1, 1), (2, 2)] {
            let from = CableFrom::Instrument(lanes[lane].node);
            draw(&mut schedule, id, from, CableTo::EffectSide(fx), &lanes, &groups);
        }
        let out = render(&mut schedule, &mut lanes, &mut [], &[note_at(70)], 64, 0, 128);
        assert_eq!(arrivals(&out, 0), [(70, 10.0 * (0.5 + 0.25))]);
    }

    #[test]
    fn every_effect_that_a_cable_ends_at_the_second_input_of_is_given_it_whatever_the_order_the_cables_were_drawn_in() {
        // What is patched is found by the effect's id, so the list of them is kept in the order of the ids and not of the cables: here the first cable drawn goes to the later effect.
        let mut schedule = Schedule::new();
        let (mut lanes, groups) = (vec![], groups(0));
        let ((first, first_fx), (second, second_fx)) = (receiver(), receiver());
        assert!(first_fx < second_fx);
        lanes.push(first);
        lanes.push(second);
        lanes.push(sender(0.5));
        let from = CableFrom::Instrument(lanes[2].node);
        draw(&mut schedule, 1, from, CableTo::EffectSide(second_fx), &lanes, &groups);
        draw(&mut schedule, 2, from, CableTo::EffectSide(first_fx), &lanes, &groups);
        let out = render(&mut schedule, &mut lanes, &mut [], &[note_at(70)], 64, 0, 128);
        assert_eq!(arrivals(&out, 0), [(70, 10.0)], "the click, ten times, through each, and nothing else: an effect that was told that nothing is patched would add its quarter at every frame");
    }

    #[test]
    fn the_second_input_of_a_bus_effect_takes_a_cable_as_a_lanes_does() {
        let mut schedule = Schedule::new();
        let (mut lanes, mut groups) = (vec![], groups(1));
        let bus_fx = push_bus_sided(&mut groups[0]);
        lanes.push(sender(0.5));
        let from = CableFrom::Instrument(lanes[0].node);
        draw(&mut schedule, 1, from, CableTo::EffectSide(bus_fx), &lanes, &groups);
        let out = render(&mut schedule, &mut lanes, &mut groups, &[note_at(70)], 64, 0, 128);
        assert_eq!(arrivals(&out, 0), [(70, 5.0)]);
    }

    #[test]
    fn a_cable_at_a_second_input_that_an_effect_has_not_is_not_drawn() {
        let mut schedule = Schedule::new();
        let (mut lanes, groups) = (lanes(2), groups(0));
        with_effects(&mut lanes[0], 1);
        let (filter, from) = (lanes[0].effect_nodes[0], CableFrom::Instrument(lanes[1].node));
        draw(&mut schedule, 1, from, CableTo::EffectSide(filter), &lanes, &groups);
        assert!(schedule.cable_ids().is_empty(), "a filter has one input");
        draw(&mut schedule, 2, from, CableTo::EffectSide(NodeId(9_999)), &lanes, &groups);
        assert!(schedule.cable_ids().is_empty(), "and what is not there has none");
        draw(&mut schedule, 3, from, CableTo::Effect(filter), &lanes, &groups);
        assert_eq!(schedule.cable_ids(), [EdgeId(3)], "the first input takes one, as it did");
        // What it is told is that the end is not there: a cable at a port that is not there does not compile either, which would say that the audio goes round.
        let (lane_wiring, group_wiring) = wiring_of(&lanes, &groups);
        let problem = |to| cable_problem(&lane_wiring, &group_wiring, std::iter::empty(), (EdgeId(4), from, to), 2);
        assert!(matches!(problem(CableTo::EffectSide(filter)), Some(CableProblem::NoEnd)));
        assert!(matches!(problem(CableTo::EffectSide(NodeId(9_999))), Some(CableProblem::NoEnd)));
        assert!(problem(CableTo::Effect(filter)).is_none());
    }

    #[test]
    fn a_cable_from_an_effect_to_its_own_second_input_would_make_the_audio_go_round_and_is_not_drawn() {
        let mut schedule = Schedule::new();
        let (mut lanes, groups) = (vec![], groups(0));
        let (receiving, fx) = receiver();
        lanes.push(receiving);
        draw(&mut schedule, 1, CableFrom::Effect(fx), CableTo::EffectSide(fx), &lanes, &groups);
        assert!(schedule.cable_ids().is_empty());
        let (lane_wiring, group_wiring) = wiring_of(&lanes, &groups);
        assert!(matches!(cable_problem(&lane_wiring, &group_wiring, std::iter::empty(), (EdgeId(1), CableFrom::Effect(fx), CableTo::EffectSide(fx)), 2), Some(CableProblem::Loop(_))));
    }

    #[test]
    fn a_cable_that_comes_or_goes_at_a_second_input_makes_the_plan_again_and_its_level_does_not() {
        let mut schedule = Schedule::new();
        let (mut lanes, groups) = (vec![], groups(0));
        let (receiving, fx) = receiver();
        lanes.push(receiving);
        lanes.push(sender(0.5));
        schedule.refresh(&lanes, &groups, 2, 64);
        let plans = schedule.plans_made();
        let from = CableFrom::Instrument(lanes[1].node);
        draw(&mut schedule, 1, from, CableTo::EffectSide(fx), &lanes, &groups);
        schedule.refresh(&lanes, &groups, 2, 64);
        assert_eq!(schedule.plans_made(), plans + 1, "drawn");
        schedule.set_cable_gain(EdgeId(1), 0.5);
        schedule.refresh(&lanes, &groups, 2, 64);
        assert_eq!(schedule.plans_made(), plans + 1, "a level is a value, and is no plan");
    }

    #[test]
    fn a_cable_that_is_taken_out_goes_down_over_a_block_and_then_the_plugin_is_told_that_nothing_is_patched() {
        let mut schedule = Schedule::new();
        let (mut lanes, groups) = (vec![], groups(0));
        let (receiving, fx) = receiver();
        lanes.push(receiving);
        lanes.push(sender(0.5));
        let from = CableFrom::Instrument(lanes[1].node);
        draw(&mut schedule, 1, from, CableTo::EffectSide(fx), &lanes, &groups);
        render(&mut schedule, &mut lanes, &mut [], &[], 64, 0, 128);
        schedule.remove_cable(EdgeId(1));
        // The block it goes down in has it, a ramp to nothing (no quarter: something is patched); the blocks after it have none.
        let during = render(&mut schedule, &mut lanes, &mut [], &[], 64, 128, 192);
        assert!(during.iter().all(|x| *x == 0.0), "the plugin is still told that there is a cable, and it carries silence");
        let after = render(&mut schedule, &mut lanes, &mut [], &[], 64, 192, 256);
        assert!(after.iter().all(|x| *x == 0.25), "and then it is told that there is none: {:?}", &after[..4]);
    }

    #[test]
    fn what_is_sent_to_the_second_input_of_an_effect_lets_the_lane_it_is_in_be_heard_for_it_while_something_is_soloed() {
        // The sender is soloed, and the receiver is not: it is heard for what its second input is sent, as it is for its first, and not otherwise.
        let mut schedule = Schedule::new();
        let (mut lanes, groups) = (vec![], groups(0));
        let (receiving, fx) = receiver();
        lanes.push(receiving);
        lanes.push(sender(0.5));
        lanes[1].solo = true;
        let from = CableFrom::Instrument(lanes[1].node);
        draw(&mut schedule, 1, from, CableTo::EffectSide(fx), &lanes, &groups);
        let out = render(&mut schedule, &mut lanes, &mut [], &[note_at(70)], 64, 0, 128);
        assert_eq!(arrivals(&out, 0), [(70, 5.0)], "heard for the cable");
        let mut alone = Schedule::new();
        let mut lanes = vec![receiver().0, sender(0.5)];
        lanes[1].solo = true;
        let out = render(&mut alone, &mut lanes, &mut [], &[note_at(70)], 64, 0, 192);
        // (The first block lets the lane go over its length, as a mute does: what is left is silence.)
        assert!(out[64..].iter().all(|x| *x == 0.0), "with no cable the receiver is not heard: nothing is soloed in it");
    }

    #[test]
    fn a_muted_sender_sends_nothing_to_the_second_input_and_the_plugin_is_still_told_that_a_cable_is_patched() {
        let mut schedule = Schedule::new();
        let (mut lanes, groups) = (vec![], groups(0));
        let (receiving, fx) = receiver();
        lanes.push(receiving);
        lanes.push(sender(0.5));
        lanes[1].muted = true;
        let from = CableFrom::Instrument(lanes[1].node);
        draw(&mut schedule, 1, from, CableTo::EffectSide(fx), &lanes, &groups);
        let out = render(&mut schedule, &mut lanes, &mut [], &[note_at(70)], 64, 0, 128);
        assert!(out.iter().all(|x| *x == 0.0), "silence at the second input is not no cable: no quarter");
    }

    #[test]
    fn the_paths_that_end_at_the_two_inputs_of_an_effect_are_in_step() {
        // The receiver has a click in its own lane (no latency), which reaches the effect's first input; the sender's click goes through an effect that is 10 frames late and ends at the
        // second input. The two arrive together, 10 frames after the note: the first path waits for the second, as a sidechain is in step with the main input.
        let mut schedule = Schedule::new();
        let groups = groups(0);
        let mut receiving = lane_of_click(0.5, 0);
        let fx = push_sided(&mut receiving);
        let mut sending = lane_of_click(0.25, 0);
        sending.route = Route::None;
        push_lagging(&mut sending, 10, 1.0);
        let late = sending.effect_nodes[0];
        let mut lanes = vec![receiving, sending];
        draw(&mut schedule, 1, CableFrom::Effect(late), CableTo::EffectSide(fx), &lanes, &groups);
        let out = render(&mut schedule, &mut lanes, &mut [], &[note_at(70)], 64, 0, 256);
        assert_eq!(arrivals(&out, 0).iter().map(|&(frame, _)| frame).collect::<Vec<_>>(), [80], "one arrival, 10 frames late");
        assert_eq!(out[80], 0.5 + 10.0 * 0.25, "both of them");
    }

    #[test]
    fn a_block_with_a_second_input_and_a_cable_at_it_asks_the_allocator_for_nothing() {
        let mut schedule = Schedule::new();
        let (mut lanes, mut groups) = (vec![], groups(0));
        let (receiving, fx) = receiver();
        lanes.push(receiving);
        lanes.push(sender(0.5));
        let from = CableFrom::Instrument(lanes[1].node);
        draw(&mut schedule, 1, from, CableTo::EffectSide(fx), &lanes, &groups);
        render(&mut schedule, &mut lanes, &mut [], &[], 64, 0, 128);
        let mut master = Master::new();
        let mut out = vec![vec![0.0f32; 64]; 2];
        let mut cx = BlockCx { midi: Events::keys(&NOTE_ON), audio_out: &mut out, num_channels: 2, scale: Default::default(), telemetry: None, fall: None };
        let (_, asked) = allocations_in(|| {
            for _ in 0..8 {
                schedule.refresh(&lanes, &groups, 2, 64);
                schedule.set_gates(&mut lanes, &mut groups);
                schedule.run(&mut lanes, &mut groups, &mut master, 64, &mut cx).unwrap();
            }
        });
        assert_eq!(asked, 0);
        assert!(loudest(&out) > 0.0, "the blocks were heard");
    }

    #[test]
    fn the_shape_has_the_effects_that_have_a_second_input_and_the_cables_that_end_there_and_a_session_with_none_has_the_shape_it_had() {
        let (mut lanes, groups) = (lanes(1), groups(0));
        with_effects(&mut lanes[0], 2);
        let mut plain = Shape::default();
        plain.read(&lanes, &groups, &[], 2, true);
        assert!(!plain.list.iter().any(|entry| entry >> 32 == SIDE >> 32 || entry >> 32 == TO_EFFECT_SIDE >> 32));
        let mut sided = Shape::default();
        let fx = push_sided(&mut lanes[0]);
        sided.read(&lanes, &groups, &[], 2, true);
        assert!(sided.list.contains(&SIDE) && sided != plain);
        let cable = Cable::new(EdgeId(5), CableFrom::Instrument(lanes[0].node), CableTo::EffectSide(fx), 1.0);
        let mut with_cable = Shape::default();
        with_cable.read(&lanes, &groups, std::slice::from_ref(&cable), 2, true);
        assert!(with_cable.list.contains(&(TO_EFFECT_SIDE | u64::from(fx.0))));
        let mut at_first = Shape::default();
        let first = Cable::new(EdgeId(5), CableFrom::Instrument(lanes[0].node), CableTo::Effect(fx), 1.0);
        at_first.read(&lanes, &groups, std::slice::from_ref(&first), 2, true);
        assert!(at_first != with_cable, "a cable at the first input is not a cable at the second");
    }

    #[test]
    fn the_shape_has_a_bus_effect_that_has_a_second_input_as_it_has_a_lanes() {
        let (lanes, mut groups) = (lanes(1), groups(1));
        let mut plain = Shape::default();
        plain.read(&lanes, &groups, &[], 2, true);
        push_bus_sided(&mut groups[0]);
        let mut sided = Shape::default();
        sided.read(&lanes, &groups, &[], 2, true);
        assert!(sided.list.contains(&SIDE) && sided != plain);
    }
}
