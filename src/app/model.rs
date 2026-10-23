//! The session model: the main thread's mirror of what the audio graph holds.
//!
//! One `InstrumentNode` per lane (instrument slot, effects, lane modulators,
//! pattern, group membership) and one `GroupNode` per submix group (bus effects,
//! group modulators). Front-ends edit it alongside the audio graph: every change
//! is applied to both, the model is what gets saved, and the helpers at the end
//! of this file keep the model's indices in step with the audio thread's own
//! fix-ups (`shift_slots_after_insert`, `fixup_group_member_after_remove`, ...
//! in `plugin::chain`) when lanes, effects, modulators or members come and go.
//!
//! Nothing here knows about the terminal: the TUI imports it today, and the
//! controller and the web projection planned in `docs/web-ui.md` build on it.

use crate::app::intent::{Intent, IntentError, ModulatorField};
use crate::plugin;
use crate::plugin::chain::{CableFrom, CableProblem, CableTo, CrossModField, GroupWiring, LaneWiring, ModTargetKind, ModulatorLocation, group_target_member_mut, member_moves};
use crate::session::{NodeUi, Route};
use crate::source::SourceSet;
use crate::telemetry::{MeterSlots, Slot, SlotAllocator};
use std::collections::{BTreeMap, VecDeque};

pub use crate::ids::{EdgeId, NodeId};

/// The ids that were handed out for something: the nodes and the cables, each in the order they were made.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Made {
    pub nodes: Vec<NodeId>,
    pub edges: Vec<EdgeId>,
}

/// Hands out `NodeId`s and `EdgeId`s, each one once. The two are series of their
/// own.
///
/// One thing is handed out twice: what undo puts back (a node that was taken out, or made, and is made again) has the ids it had, so
/// that every step of the history that names it goes on naming it. `reuse` says which ids come next, before any new one.
#[derive(Debug, Default)]
pub struct NodeIds {
    next: u32,
    next_edge: u32,
    /// The ids to hand out before new ones, in order.
    put_back: VecDeque<NodeId>,
    put_back_edges: VecDeque<EdgeId>,
    /// Every id handed out since `begin_log`, if it was begun.
    log: Option<Made>,
}

impl NodeIds {
    pub fn alloc(&mut self) -> NodeId {
        let id = self.put_back.pop_front().unwrap_or_else(|| {
            let id = NodeId(self.next);
            self.next += 1;
            id
        });
        if let Some(log) = &mut self.log {
            log.nodes.push(id);
        }
        id
    }

    pub fn alloc_edge(&mut self) -> EdgeId {
        let id = self.put_back_edges.pop_front().unwrap_or_else(|| {
            let id = EdgeId(self.next_edge);
            self.next_edge += 1;
            id
        });
        if let Some(log) = &mut self.log {
            log.edges.push(id);
        }
        id
    }

    /// The ids that come next, in this order, in place of new ones: what undo puts back is under the ids it had.
    pub fn reuse(&mut self, nodes: &[NodeId], edges: &[EdgeId]) {
        self.put_back = nodes.iter().copied().collect();
        self.put_back_edges = edges.iter().copied().collect();
    }

    /// What was not used of the ids that were to come next is not to be: new ids go on.
    pub fn drop_ids_to_put_back(&mut self) {
        self.put_back.clear();
        self.put_back_edges.clear();
    }

    /// Write down every id that is handed out, from now until `take_log`.
    pub fn begin_log(&mut self) {
        self.log = Some(Made::default());
    }

    /// The ids handed out since `begin_log`, in order, and nothing is written down after this until the next `begin_log`.
    pub fn take_log(&mut self) -> Made {
        self.log.take().unwrap_or_default()
    }
}

/// Where a node sits in the model, in the positional terms the audio thread's
/// commands use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodePath {
    /// A lane, with the instrument plugin in it.
    Instrument(usize),
    Effect { inst: usize, index: usize },
    Group(usize),
    GroupEffect { group: usize, index: usize },
    Modulator { at: ModulatorLocation, index: usize },
    /// A source of notes: the keys, or a MIDI device.
    Source(usize),
}

/// What a cable drives, named so that it can be found again once other cables and modulators have gone
/// and the places in the racks have moved: a plugin's parameter (by the node of the plugin), a
/// modulator's rate or a stage of its envelope (by the node of the modulator), or the depth of a cable
/// (by its edge).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Driven {
    Param { plugin: NodeId, index: u32 },
    Setting { modulator: NodeId, field: ModulatorField },
    Depth(EdgeId),
}

/// The modulator, in the same rack, and the field of it that a cable of `kind` drives, if it drives one.
fn sibling_field(kind: &ModTargetKind) -> Option<(usize, CrossModField)> {
    match *kind {
        ModTargetKind::ModulatorRate { mod_index } => Some((mod_index, CrossModField::Rate)),
        ModTargetKind::ModulatorAttack { mod_index } => Some((mod_index, CrossModField::Attack)),
        ModTargetKind::ModulatorDecay { mod_index } => Some((mod_index, CrossModField::Decay)),
        ModTargetKind::ModulatorSustain { mod_index } => Some((mod_index, CrossModField::Sustain)),
        ModTargetKind::ModulatorRelease { mod_index } => Some((mod_index, CrossModField::Release)),
        ModTargetKind::ModulatorDepth { mod_index, target_index } => Some((mod_index, CrossModField::Depth(target_index))),
        ModTargetKind::PluginParam { .. }
        | ModTargetKind::GroupMember { .. }
        | ModTargetKind::GroupBus { .. }
        | ModTargetKind::GroupMemberMod { .. } => None,
    }
}

/// The tempo of a session none of whose patterns has been recorded, in beats a minute: the one the audio thread begins at as well.
pub const DEFAULT_BPM: f32 = crate::plugin::chain::DEFAULT_BPM;
/// The slowest and the fastest tempo there can be, in beats a minute: a tempo that a session file has is held to them, as is one that is set.
pub const BPM_MIN: f32 = 20.0;
pub const BPM_MAX: f32 = 300.0;

/// What a front-end edits: the instrument lanes and the submix groups.
#[derive(Debug)]
pub struct SessionModel {
    pub instruments: Vec<InstrumentNode>,
    pub groups: Vec<GroupNode>,
    pub ids: NodeIds,
    /// Hands out the telemetry slots the nodes publish into.
    pub slots: SlotAllocator,
    /// Where a front-end draws the nodes it has been told a place for, by node: a side table, since the engine and the
    /// TUI have no use for it and a node does not need one. The master is under `NodeId::MASTER`. An id is never
    /// reused, so what is left here of a node that has gone is never read: `graph()` and the save look up the nodes
    /// there are.
    pub ui: BTreeMap<NodeId, NodeUi>,
    /// The tempo that patterns are recorded at, in beats a minute: that of the first recorded pattern of a session that is loaded
    /// (`DEFAULT_BPM` if it has none).
    pub bpm: f32,
    /// The audio cables that have been drawn (P3c of `docs/patch-graph.md` §11.6), in the order of their ids: sends from one place of the audio to another, besides the
    /// way that a lane and a group are wired by themselves.
    pub cables: Vec<AudioCable>,
    /// The sources of notes that a person put in the session (P4b of `docs/patch-graph.md` §11.6): the keys, and the MIDI devices that it names. None until one is added, and then a lane
    /// that no event cable ends at hears every source (the program is as it was), and one that has some hears the sources they start at.
    pub sources: Vec<SourceNode>,
    /// The event cables that have been drawn, in the order of their ids: the notes of a source go to a lane.
    pub event_cables: Vec<EventCable>,
}

impl Default for SessionModel {
    fn default() -> Self {
        SessionModel {
            instruments: Vec::new(),
            groups: Vec::new(),
            ids: NodeIds::default(),
            slots: SlotAllocator::default(),
            ui: BTreeMap::new(),
            bpm: DEFAULT_BPM,
            cables: Vec::new(),
            sources: Vec::new(),
            event_cables: Vec::new(),
        }
    }
}

/// A source of notes that a session has (P4b of `docs/patch-graph.md` §11.6): a node of the graph with an output of events, which event cables start at. Its id is from the
/// series that the other nodes' are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceNode {
    pub node: NodeId,
    pub kind: SourceKind,
}

/// What a source of notes is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceKind {
    /// The computer's keys, the page's piano and keys, and every browser's: they are one source (`Source::KEYS`).
    Keys,
    /// A MIDI device, by the name of its port as the system gives it, or as much of it as tells it ("Launchkey" is both ports of a Launchkey Mini): the sources of the open ports that
    /// have the name (`DeviceWatcher::sources_of`).
    Device(String),
}

impl SourceKind {
    /// What the node of the source is called.
    pub fn display_name(&self) -> String {
        match self {
            SourceKind::Keys => "Keys".to_string(),
            SourceKind::Device(name) => name.clone(),
        }
    }
}

/// An event cable that a session has: the notes of a source go to a lane, which then hears that source (and the others that cables bring it) and no other. Its id is from the series
/// that the other cables' are, so that no two edges of the graph a client gets have one id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventCable {
    pub id: EdgeId,
    /// The node of the source it starts at.
    pub from: NodeId,
    /// The node of the lane it ends at (the lane's instrument is the lane's own node).
    pub to: NodeId,
}

/// An audio cable that a session has: a send from one place of the audio to another (P3c of `docs/patch-graph.md` §11.6). Its id is from the series that the modulation
/// cables' are, so that no two edges of the graph a client gets have one id.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioCable {
    pub id: EdgeId,
    pub from: CableFrom,
    pub to: CableTo,
    /// The send level, `0..=CABLE_GAIN_MAX`: 1 is what comes out of `from`, as it is.
    pub gain: f32,
}

/// The highest send level an intent can set (+6 dB).
pub const CABLE_GAIN_MAX: f32 = 2.0;

/// The level that a cable is drawn at when nothing says another: what comes out of its start, as it is.
pub const NEW_CABLE_GAIN: f32 = 1.0;

/// The tempo a session comes to when its file says none: that of the first of its lanes that has a recorded pattern, else `DEFAULT_BPM`, held
/// to the tempos there can be.
pub fn tempo_of_patterns(lanes: &[InstrumentNode]) -> f32 {
    lanes.iter().find_map(|lane| lane.pattern.as_ref()).map_or(DEFAULT_BPM, |pattern| pattern.bpm.clamp(BPM_MIN, BPM_MAX))
}

impl SessionModel {
    /// The wiring of the lanes and the groups, as the plan of the audio thread is made of it.
    pub fn wiring(&self) -> (Vec<LaneWiring>, Vec<GroupWiring>) {
        (
            self.instruments
                .iter()
                .map(|lane| LaneWiring {
                    node: lane.node,
                    group: lane.group,
                    effects: lane.effects.iter().map(|effect| effect.node).collect(),
                    cut: lane.effects.iter().filter(|effect| !effect.fed).map(|effect| effect.node).collect(),
                    route: lane.route,
                })
                .collect(),
            self.groups
                .iter()
                .map(|group| GroupWiring {
                    node: group.node,
                    effects: group.effects.iter().map(|effect| effect.node).collect(),
                    cut: group.effects.iter().filter(|effect| !effect.fed).map(|effect| effect.node).collect(),
                    route: group.route,
                })
                .collect(),
        )
    }

    /// Why a cable from `from` to `to` cannot be drawn: an end that is not there, or a loop (the audio would go round). None if it can. (The plan is compiled on the side with it, as
    /// the audio thread does when it is sent one, so that what is refused here is what it would refuse, and the controller can say why.)
    pub fn cable_problem(&self, from: CableFrom, to: CableTo) -> Option<CableProblem> {
        let (lanes, groups) = self.wiring();
        let others = self.cables.iter().map(|cable| (cable.id, cable.from, cable.to));
        // (An id that no cable has: the cable is not there yet.)
        crate::plugin::chain::cable_problem(&lanes, &groups, others, (EdgeId(u32::MAX), from, to), crate::plugin::chain::CHAIN_CHANNELS)
    }

    /// Why the audio would go round if lane `inst` were in group `group` (or in none), with the cables there are: the reason, or None if it would not (or if the lane or
    /// the group is not there, which is for the caller to say). Nothing is changed.
    pub fn loop_if_lane_goes_to(&self, inst: usize, group: Option<usize>) -> Option<String> {
        let (mut lanes, groups) = self.wiring();
        lanes.get_mut(inst)?.group = group.filter(|group| *group < groups.len());
        self.wiring_loop(&lanes, &groups)
    }

    /// Why the audio would go round if the lane or the group at `node` had the way out `route`, with the cables there are: the reason, or None if it would not (or if `node` is
    /// neither). Nothing is changed. Only a way out that is `Out` can close a loop: a link that is not there is on no path.
    pub fn loop_if_route_is(&self, node: NodePath, route: Route) -> Option<String> {
        let (mut lanes, mut groups) = self.wiring();
        match node {
            NodePath::Instrument(inst) => lanes.get_mut(inst)?.route = route,
            NodePath::Group(group) => groups.get_mut(group)?.route = route,
            _ => return None,
        }
        self.wiring_loop(&lanes, &groups)
    }

    /// Why the audio would go round if the link into the effect `node` (of a lane's chain or of a group's bus) from the stage before it were there (`fed`) or not, with the cables there are:
    /// the reason, or None if it would not (or if `node` is no effect). Nothing is changed. Only a link that is there can close a loop: one that is not is on no path.
    pub fn loop_if_feed_is(&self, node: NodeId, fed: bool) -> Option<String> {
        let (mut lanes, mut groups) = self.wiring();
        let cut = match self.locate(node)? {
            NodePath::Effect { inst, .. } => &mut lanes.get_mut(inst)?.cut,
            NodePath::GroupEffect { group, .. } => &mut groups.get_mut(group)?.cut,
            _ => return None,
        };
        cut.retain(|other| *other != node);
        if !fed {
            cut.push(node);
        }
        self.wiring_loop(&lanes, &groups)
    }

    /// Why the audio would go round if the effect `node` (of a lane's chain or of a group's bus) were at place `index` of its chain, the effects between moving one place the
    /// other way, with the cables there are: the reason, or None if it would not (or if the node is no effect, or the chain has no such place). Nothing is changed.
    pub fn loop_if_effect_goes_to(&self, node: NodeId, index: usize) -> Option<String> {
        let (mut lanes, mut groups) = self.wiring();
        let chain = match self.locate(node)? {
            NodePath::Effect { inst, .. } => &mut lanes.get_mut(inst)?.effects,
            NodePath::GroupEffect { group, .. } => &mut groups.get_mut(group)?.effects,
            _ => return None,
        };
        let from = chain.iter().position(|effect| *effect == node)?;
        if index >= chain.len() {
            return None;
        }
        let effect = chain.remove(from);
        chain.insert(index, effect);
        self.wiring_loop(&lanes, &groups)
    }

    /// Why the wiring `lanes` and `groups`, with the cables there are, does not compile: the audio goes round in it.
    fn wiring_loop(&self, lanes: &[LaneWiring], groups: &[GroupWiring]) -> Option<String> {
        let cables = self.cables.iter().map(|cable| (cable.id, cable.from, cable.to));
        crate::plugin::chain::wiring_loop(lanes, groups, cables, crate::plugin::chain::CHAIN_CHANNELS)
    }

    /// The audio cable `id`, if there is one.
    pub fn audio_cable(&self, id: EdgeId) -> Option<&AudioCable> {
        self.cables.binary_search_by_key(&id, |cable| cable.id).ok().map(|at| &self.cables[at])
    }

    /// The audio cable `id`, to change.
    pub fn audio_cable_mut(&mut self, id: EdgeId) -> Option<&mut AudioCable> {
        self.cables.binary_search_by_key(&id, |cable| cable.id).ok().map(|at| &mut self.cables[at])
    }

    /// Put a cable among the cables, in the order of the ids (one that is there under the id is replaced): the place it went to.
    pub fn insert_audio_cable(&mut self, cable: AudioCable) -> usize {
        match self.cables.binary_search_by_key(&cable.id, |other| other.id) {
            Ok(at) => {
                self.cables[at] = cable;
                at
            }
            Err(at) => {
                self.cables.insert(at, cable);
                at
            }
        }
    }

    /// Put an event cable among the event cables, in the order of the ids (one that is there under the id is replaced): the place it went to.
    pub fn insert_event_cable(&mut self, cable: EventCable) -> usize {
        match self.event_cables.binary_search_by_key(&cable.id, |other| other.id) {
            Ok(at) => {
                self.event_cables[at] = cable;
                at
            }
            Err(at) => {
                self.event_cables.insert(at, cable);
                at
            }
        }
    }

    /// The source of notes whose node is `node`, if there is one.
    pub fn source(&self, node: NodeId) -> Option<&SourceNode> {
        self.sources.iter().find(|source| source.node == node)
    }

    /// Put a source of notes among the sources at `index` (at the end for one past it): the place it went to.
    pub fn insert_source_at(&mut self, index: usize, source: SourceNode) -> usize {
        let index = index.min(self.sources.len());
        self.sources.insert(index, source);
        index
    }

    /// The event cable `id`, if there is one.
    pub fn event_cable(&self, id: EdgeId) -> Option<&EventCable> {
        self.event_cables.binary_search_by_key(&id, |cable| cable.id).ok().map(|at| &self.event_cables[at])
    }

    /// The ends that a cable from the port `from` to the port `to` of the graph has, if it is a cable of notes: the node of the source that it starts at (`EventsOut`) and the node of the
    /// lane that it ends at (`EventsIn`; a lane's instrument is the lane's own node). The reason, if they are not that: a node that is not there is not found, and a port or a node of
    /// another kind is invalid.
    pub fn event_cable_ends(&self, from: crate::app::graph::Endpoint, to: crate::app::graph::Endpoint) -> Result<(NodeId, NodeId), IntentError> {
        use crate::app::graph::Port;
        let invalid = |why: &str| IntentError::Invalid(why.to_string());
        if from.port != Port::EventsOut {
            return Err(invalid("a cable of notes starts at an output of events"));
        }
        if to.port != Port::EventsIn {
            return Err(invalid("a cable of notes ends at an input of events"));
        }
        if from.node == NodeId::MASTER {
            return Err(invalid("the mix has no output of events"));
        }
        if self.source(from.node).is_none() {
            return Err(match self.locate(from.node) {
                Some(_) => invalid("only a source of notes has an output of events"),
                None => IntentError::NotFound(from.node),
            });
        }
        if to.node == NodeId::MASTER {
            return Err(invalid("the mix has no input of events"));
        }
        match self.locate(to.node).ok_or(IntentError::NotFound(to.node))? {
            NodePath::Instrument(_) => Ok((from.node, to.node)),
            _ => Err(invalid("a cable of notes ends at a lane: it is the lanes that have an input of events")),
        }
    }

    /// The sources that lane `inst` hears (P4b of `docs/patch-graph.md` §11.6): every source if no event cable ends at it, and otherwise the sources of the nodes that the cables that end at it start at,
    /// as `resolve` says them (the keys are one source, a device is the sources of the open ports that its name stands for, and none while it is not plugged in: a lane that has a cable from it
    /// and no other does not hear the others in the meantime). A lane that is not there hears every source, as it will when it is.
    pub fn lane_sources(&self, inst: usize, mut resolve: impl FnMut(&SourceKind) -> SourceSet) -> SourceSet {
        let Some(lane) = self.instruments.get(inst) else { return SourceSet::ALL };
        let mut cables = self.event_cables.iter().filter(|cable| cable.to == lane.node).peekable();
        if cables.peek().is_none() {
            return SourceSet::ALL;
        }
        cables.filter_map(|cable| self.source(cable.from)).fold(SourceSet::NONE, |heard, source| heard.or(resolve(&source.kind)))
    }

    /// Take the event cable `id` out: what it was. None if there is no such cable.
    pub fn remove_event_cable(&mut self, id: EdgeId) -> Option<EventCable> {
        let at = self.event_cables.binary_search_by_key(&id, |cable| cable.id).ok()?;
        Some(self.event_cables.remove(at))
    }

    /// The event cables that start or end at any of `nodes` (a source, a lane), in the order of their ids: what goes with them when they are taken out.
    pub fn event_cables_at(&self, nodes: &[NodeId]) -> Vec<EdgeId> {
        self.event_cables.iter().filter(|cable| nodes.contains(&cable.from) || nodes.contains(&cable.to)).map(|cable| cable.id).collect()
    }

    /// Take the audio cable `id` out: the place it had and what it was. None if there is no such cable.
    pub fn remove_audio_cable(&mut self, id: EdgeId) -> Option<(usize, AudioCable)> {
        let at = self.cables.binary_search_by_key(&id, |cable| cable.id).ok()?;
        Some((at, self.cables.remove(at)))
    }

    /// The audio cables that start or end at any of `nodes` (a lane, an effect of a lane or of a bus, a group), in the order of their ids: what goes with them when they are taken out.
    pub fn audio_cables_at(&self, nodes: &[NodeId]) -> Vec<EdgeId> {
        let starts = |cable: &AudioCable| match cable.from {
            CableFrom::Instrument(id) | CableFrom::Effect(id) | CableFrom::Group(id) => nodes.contains(&id),
        };
        let ends = |cable: &AudioCable| match cable.to {
            CableTo::Effect(id) | CableTo::GroupBus(id) => nodes.contains(&id),
            CableTo::Master => false,
        };
        self.cables.iter().filter(|cable| starts(cable) || ends(cable)).map(|cable| cable.id).collect()
    }

    /// The nodes that have the audio of lane `inst`: its instrument's, which is the lane's, and each of its effects.
    pub fn audio_nodes_of_lane(&self, inst: usize) -> Vec<NodeId> {
        self.instruments.get(inst).map_or_else(Vec::new, |lane| std::iter::once(lane.node).chain(lane.effects.iter().map(|effect| effect.node)).collect())
    }

    /// The nodes that have the audio of group `group`: its own, which is its bus and its output, and each of its bus effects.
    pub fn audio_nodes_of_group(&self, group: usize) -> Vec<NodeId> {
        self.groups.get(group).map_or_else(Vec::new, |bus| std::iter::once(bus.node).chain(bus.effects.iter().map(|effect| effect.node)).collect())
    }

    /// Whether a cable, of any kind, has the id: the modulation cables, the audio cables and the event cables are one series.
    pub fn edge_in_use(&self, id: EdgeId) -> bool {
        self.locate_edge(id).is_some() || self.audio_cable(id).is_some() || self.event_cables.iter().any(|cable| cable.id == id)
    }

    /// The ends that a cable from the port `from` to the port `to` of the graph has: an instrument's, an effect's or a group's `AudioOut` to an effect's, a group's or the master's
    /// `AudioIn`. The reason, if they are not that: a node that is not there is not found, and a port or a node of another kind is invalid.
    pub fn cable_ends(&self, from: crate::app::graph::Endpoint, to: crate::app::graph::Endpoint) -> Result<(CableFrom, CableTo), IntentError> {
        use crate::app::graph::Port;
        let invalid = |why: &str| IntentError::Invalid(why.to_string());
        if from.port != Port::AudioOut {
            return Err(invalid("an audio cable starts at an output of audio"));
        }
        if to.port != Port::AudioIn {
            return Err(invalid("an audio cable ends at an input of audio"));
        }
        let start = if from.node == NodeId::MASTER {
            return Err(invalid("the mix has no output of audio"));
        } else {
            match self.locate(from.node).ok_or(IntentError::NotFound(from.node))? {
                NodePath::Instrument(_) => CableFrom::Instrument(from.node),
                NodePath::Effect { .. } | NodePath::GroupEffect { .. } => CableFrom::Effect(from.node),
                NodePath::Group(_) => CableFrom::Group(from.node),
                NodePath::Modulator { .. } => return Err(invalid("a modulator has no output of audio")),
                NodePath::Source(_) => return Err(invalid("a source of notes has no output of audio: it has an output of events")),
            }
        };
        let end = if to.node == NodeId::MASTER {
            CableTo::Master
        } else {
            match self.locate(to.node).ok_or(IntentError::NotFound(to.node))? {
                NodePath::Effect { .. } | NodePath::GroupEffect { .. } => CableTo::Effect(to.node),
                NodePath::Group(_) => CableTo::GroupBus(to.node),
                NodePath::Instrument(_) => return Err(invalid("a lane has no input of audio: a cable ends at an effect, a group or the mix")),
                NodePath::Modulator { .. } => return Err(invalid("a modulator has no input of audio")),
                NodePath::Source(_) => return Err(invalid("a source of notes has no input of audio")),
            }
        };
        Ok((start, end))
    }

    /// The `bpm` that a save writes: none when a load would come to this tempo anyway (`tempo_of_patterns`), so that the files of sessions that never
    /// set one are as they were.
    pub fn bpm_to_save(&self) -> Option<f32> {
        (self.bpm != tempo_of_patterns(&self.instruments)).then_some(self.bpm)
    }

    /// Where the master mix is drawn, if it has been placed.
    pub fn master_ui(&self) -> Option<NodeUi> {
        self.ui.get(&NodeId::MASTER).copied()
    }

    /// A `NodeId` no other node has had.
    pub fn alloc_node(&mut self) -> NodeId {
        self.ids.alloc()
    }

    /// An `EdgeId` no other modulation edge has had.
    pub fn alloc_edge(&mut self) -> EdgeId {
        self.ids.alloc_edge()
    }

    /// Every node and where it sits: the groups with their bus effects and
    /// modulators, then the lanes with their effects and modulators. (A lane's
    /// instrument plugin shares the lane's id.)
    pub fn nodes(&self) -> impl Iterator<Item = (NodeId, NodePath)> + '_ {
        let groups = self.groups.iter().enumerate().flat_map(|(group, g)| {
            std::iter::once((g.node, NodePath::Group(group)))
                .chain(g.effects.iter().enumerate().map(move |(index, fx)| {
                    (fx.node, NodePath::GroupEffect { group, index })
                }))
                .chain(g.modulators.iter().enumerate().map(move |(index, m)| {
                    (m.node, NodePath::Modulator { at: ModulatorLocation::Group(group), index })
                }))
        });
        let lanes = self.instruments.iter().enumerate().flat_map(|(inst, lane)| {
            std::iter::once((lane.node, NodePath::Instrument(inst)))
                .chain(lane.effects.iter().enumerate().map(move |(index, fx)| {
                    (fx.node, NodePath::Effect { inst, index })
                }))
                .chain(lane.modulators.iter().enumerate().map(move |(index, m)| {
                    (m.node, NodePath::Modulator { at: ModulatorLocation::Lane(inst), index })
                }))
        });
        let sources = self.sources.iter().enumerate().map(|(index, source)| (source.node, NodePath::Source(index)));
        groups.chain(lanes).chain(sources)
    }

    /// Where the node `id` sits, if it is still in the session.
    pub fn locate(&self, id: NodeId) -> Option<NodePath> {
        self.nodes().find(|(node, _)| *node == id).map(|(_, path)| path)
    }

    /// Where the modulation cable `id` is: the modulator's rack, the modulator's
    /// position in it and the target's in the modulator. None if there is no such
    /// cable (any more).
    pub fn locate_edge(&self, id: EdgeId) -> Option<(ModulatorLocation, usize, usize)> {
        let groups = self.groups.iter().enumerate().flat_map(|(group, g)| {
            g.modulators.iter().enumerate().flat_map(move |(index, m)| {
                m.targets.iter().enumerate().map(move |(target, t)| (t.edge, ModulatorLocation::Group(group), index, target))
            })
        });
        let lanes = self.instruments.iter().enumerate().flat_map(|(inst, lane)| {
            lane.modulators.iter().enumerate().flat_map(move |(index, m)| {
                m.targets.iter().enumerate().map(move |(target, t)| (t.edge, ModulatorLocation::Lane(inst), index, target))
            })
        });
        groups.chain(lanes).find(|(edge, ..)| *edge == id).map(|(_, at, index, target)| (at, index, target))
    }

    /// Check that every node has an id of its own, that ids were allocated here,
    /// that a lane's instrument plugin carries the lane's id, and that the
    /// telemetry slots are in order: every lane and group has a meter (two slots),
    /// every modulator an output slot and every target an effective-value slot
    /// (what it drives is a parameter or a setting of a modulator), all different,
    /// allocated here, and none the engine's own.
    #[cfg(test)]
    pub fn check_ids(&self) -> Result<(), String> {
        let mut seen = std::collections::HashSet::new();
        for (id, path) in self.nodes() {
            if id.0 >= self.ids.next {
                return Err(format!("{path:?} has {id:?}, which was never allocated"));
            }
            if !seen.insert(id) {
                return Err(format!("{path:?} has {id:?}, which another node has too"));
            }
        }
        for (inst, lane) in self.instruments.iter().enumerate() {
            if let Some(slot) = &lane.instrument {
                if slot.node != lane.node {
                    return Err(format!("lane {inst} is {:?}, its instrument {:?}", lane.node, slot.node));
                }
            }
        }

        let racks = self
            .groups
            .iter()
            .enumerate()
            .map(|(g, group)| (format!("group {g}"), &group.modulators))
            .chain(self.instruments.iter().enumerate().map(|(i, lane)| (format!("lane {i}"), &lane.modulators)));
        let mut edges = std::collections::HashSet::new();
        let mut taken = std::collections::HashSet::new();
        let mut claim = |slot: Option<Slot>, whose: String| -> Result<(), String> {
            let Some(slot) = slot else { return Err(format!("{whose} has no telemetry slot")) };
            if slot == crate::telemetry::MASTER.peak || slot == crate::telemetry::MASTER.clips {
                return Err(format!("{whose} has {slot:?}, which is the master meter's"));
            }
            if slot.index() >= self.slots.high_water() {
                return Err(format!("{whose} has {slot:?}, which was never allocated"));
            }
            if !taken.insert(slot) {
                return Err(format!("{whose} has {slot:?}, which another node has too"));
            }
            Ok(())
        };
        for (g, group) in self.groups.iter().enumerate() {
            let meter = group.meter;
            claim(meter.map(|m| m.peak), format!("the meter of group {g}"))?;
            claim(meter.map(|m| m.clips), format!("the meter of group {g}"))?;
        }
        for (i, lane) in self.instruments.iter().enumerate() {
            let meter = lane.meter;
            claim(meter.map(|m| m.peak), format!("the meter of lane {i}"))?;
            claim(meter.map(|m| m.clips), format!("the meter of lane {i}"))?;
        }
        for (rack, modulators) in racks {
            for (m, modulator) in modulators.iter().enumerate() {
                claim(modulator.output, format!("modulator {m} of {rack}"))?;
                if matches!(modulator.source, ModSourceSlot::Steps { .. }) {
                    claim(modulator.position, format!("the position of modulator {m} of {rack}"))?;
                } else if modulator.position.is_some() {
                    return Err(format!("modulator {m} of {rack} is not a step sequencer and has a position"));
                }
                for (t, target) in modulator.targets.iter().enumerate() {
                    let whose = format!("target {t} of modulator {m} of {rack}");
                    if target.edge.0 >= self.ids.next_edge {
                        return Err(format!("{whose} has {:?}, which was never allocated", target.edge));
                    }
                    if !edges.insert(target.edge) {
                        return Err(format!("{whose} has {:?}, which another edge has too", target.edge));
                    }
                    claim(target.effective, whose)?;
                }
            }
        }
        // The audio cables: their ids are from the series that the modulation cables have, and each is at nodes that are there, and none makes the audio go round.
        let (lanes, groups) = self.wiring();
        let drawn = || self.cables.iter().map(|cable| (cable.id, cable.from, cable.to));
        for cable in &self.cables {
            let whose = format!("audio cable {:?}", cable.id);
            if cable.id.0 >= self.ids.next_edge {
                return Err(format!("{whose} was never allocated"));
            }
            if !edges.insert(cable.id) {
                return Err(format!("{whose} has an id which another edge has too"));
            }
            if let Some(problem) = crate::plugin::chain::cable_problem(&lanes, &groups, drawn(), (cable.id, cable.from, cable.to), crate::plugin::chain::CHAIN_CHANNELS) {
                return Err(format!("{whose}: {problem}"));
            }
        }
        // The sources: the keys once at most, and a device once for a name.
        if self.sources.iter().filter(|source| source.kind == SourceKind::Keys).count() > 1 {
            return Err("the keys are a source more than once".to_string());
        }
        for (at, source) in self.sources.iter().enumerate() {
            if let SourceKind::Device(name) = &source.kind {
                if self.sources[..at].iter().any(|earlier| earlier.kind == source.kind) {
                    return Err(format!("the device {name:?} is a source more than once"));
                }
            }
        }
        // The event cables: ids from the same series, each from a source to a lane, none twice between the same two.
        for (at, cable) in self.event_cables.iter().enumerate() {
            let whose = format!("event cable {:?}", cable.id);
            if cable.id.0 >= self.ids.next_edge {
                return Err(format!("{whose} was never allocated"));
            }
            if !edges.insert(cable.id) {
                return Err(format!("{whose} has an id which another edge has too"));
            }
            if !self.sources.iter().any(|source| source.node == cable.from) {
                return Err(format!("{whose} starts at {:?}, which is not a source", cable.from));
            }
            if !self.instruments.iter().any(|lane| lane.node == cable.to) {
                return Err(format!("{whose} ends at {:?}, which is not a lane", cable.to));
            }
            if self.event_cables[..at].iter().any(|earlier| (earlier.from, earlier.to) == (cable.from, cable.to)) {
                return Err(format!("{whose} is between two nodes that another event cable is between"));
            }
            if at > 0 && self.event_cables[at - 1].id >= cable.id {
                return Err(format!("{whose} is not after the one before it"));
            }
        }
        Ok(())
    }

    /// The modulator rack at `at`: a lane's or a group's, if it exists.
    pub fn modulators_mut(&mut self, at: ModulatorLocation) -> Option<&mut Vec<ModulatorSlot>> {
        match at {
            ModulatorLocation::Lane(inst) => self.instruments.get_mut(inst).map(|n| &mut n.modulators),
            ModulatorLocation::Group(group) => self.groups.get_mut(group).map(|g| &mut g.modulators),
        }
    }

    /// The modulator rack at `at`: a lane's or a group's, if it exists.
    pub fn modulators(&self, at: ModulatorLocation) -> Option<&Vec<ModulatorSlot>> {
        match at {
            ModulatorLocation::Lane(inst) => self.instruments.get(inst).map(|n| &n.modulators),
            ModulatorLocation::Group(group) => self.groups.get(group).map(|g| &g.modulators),
        }
    }

    /// Take the cable `edge` out of the session, and with it every cable that drives the
    /// depth of a cable that goes: those, and the ones that drive theirs in turn. (A cable
    /// that is left driving the depth of one that is gone would, being addressed by its place,
    /// drive the next one's.) The cables of the same modulator, and of the modulators that
    /// drive their depths, are told the places their neighbours have after, as the audio
    /// thread does it (`RemoveModTarget`).
    ///
    /// The removals, in the order they were made: the rack, the modulator in it, and the
    /// cable's place in the modulator *as it was when its turn came*, so that the audio thread
    /// can be sent the same commands in the same order, and the cable itself with what it was
    /// centred on, so that it can be put back (`insert_target_at`, in the reverse order). A cable
    /// goes after the cables that drive its depth. `None`, and nothing changed, if there is no such cable.
    pub fn remove_edge(&mut self, edge: EdgeId) -> Option<Vec<RemovedCable>> {
        let going = self.cascade_of(edge)?;
        let mut removed = Vec::new();
        for gone in going.into_iter().rev() {
            let (at, mod_index, target_index) = self.locate_edge(gone)?;
            removed.push(self.remove_target_at(at, mod_index, target_index)?);
        }
        Some(removed)
    }

    /// What a cable of `kind` from the modulator at `mod_index` of the rack at `at` is centred on: what the user has set what it drives to
    /// (a plugin's parameter, a modulator's rate or stage, the depth of a cable). `None` if what it drives is not there.
    pub fn centre_of(&self, at: ModulatorLocation, mod_index: usize, kind: &ModTargetKind) -> Option<f32> {
        if let Some(target) = self.param_targets(at).into_iter().find(|target| target.kind == *kind) {
            return Some(target.base_value);
        }
        self.field_targets(at, mod_index).into_iter().find(|target| target.kind == *kind).map(|target| target.base_value)
    }

    /// The cables that go when cable `edge` is taken out: it, and then, in turn, those that drive the
    /// depth of one that goes (which may be one that is there already: they can drive each other's).
    /// `None` if there is no such cable.
    pub fn cascade_of(&self, edge: EdgeId) -> Option<Vec<EdgeId>> {
        self.locate_edge(edge)?;
        let mut going = vec![edge];
        let mut next = 0;
        while let Some(&gone) = going.get(next) {
            next += 1;
            for driver in self.depth_cables_of(gone) {
                if !going.contains(&driver) {
                    going.push(driver);
                }
            }
        }
        Some(going)
    }

    /// What the cable of kind `kind`, from the modulator at `source` in the rack at `at`, drives. Nothing
    /// if it names a plugin, a modulator or a cable that is not there, or a modulator that is its own (a
    /// cable like that does nothing).
    pub fn driven_by(&self, at: ModulatorLocation, source: usize, kind: &ModTargetKind) -> Option<Driven> {
        use ModTargetKind as K;
        // A field of the modulator at `index` of `rack`, as a cable of `field` drives it.
        let field_of = |rack: &[ModulatorSlot], index: usize, field: CrossModField| -> Option<Driven> {
            let modulator = rack.get(index)?;
            let setting = |field| Driven::Setting { modulator: modulator.node, field };
            Some(match field {
                CrossModField::Rate => setting(ModulatorField::Rate),
                CrossModField::Attack => setting(ModulatorField::Attack),
                CrossModField::Decay => setting(ModulatorField::Decay),
                CrossModField::Sustain => setting(ModulatorField::Sustain),
                CrossModField::Release => setting(ModulatorField::Release),
                CrossModField::Depth(target) => Driven::Depth(modulator.targets.get(target)?.edge),
            })
        };
        match (at, kind) {
            (ModulatorLocation::Lane(inst), &K::PluginParam { slot, param_index }) => {
                Some(Driven::Param { plugin: self.instruments.get(inst)?.plugin_node(slot)?, index: param_index })
            }
            (ModulatorLocation::Group(group), &K::GroupMember { member, slot, param_index }) => {
                Some(Driven::Param { plugin: self.group_member(group, member)?.plugin_node(slot)?, index: param_index })
            }
            (ModulatorLocation::Group(group), &K::GroupBus { effect_index, param_index }) => {
                Some(Driven::Param { plugin: self.groups.get(group)?.effects.get(effect_index)?.node, index: param_index })
            }
            (ModulatorLocation::Group(group), &K::GroupMemberMod { member, mod_index, field }) => {
                field_of(&self.group_member(group, member)?.modulators, mod_index, field)
            }
            (_, kind) => {
                let (index, field) = sibling_field(kind)?;
                if index == source {
                    return None;
                }
                field_of(self.modulators(at)?, index, field)
            }
        }
    }

    /// What the cable `edge` drives.
    pub fn driven_by_edge(&self, edge: EdgeId) -> Option<Driven> {
        let (at, source, target) = self.locate_edge(edge)?;
        let kind = &self.modulators(at)?.get(source)?.targets.get(target)?.kind;
        self.driven_by(at, source, kind)
    }

    /// Whether any cable in the session drives `driven`.
    pub fn is_driven(&self, driven: Driven) -> bool {
        let lanes = self.instruments.iter().enumerate().map(|(i, lane)| (ModulatorLocation::Lane(i), &lane.modulators));
        let groups = self.groups.iter().enumerate().map(|(i, group)| (ModulatorLocation::Group(i), &group.modulators));
        lanes.chain(groups).any(|(at, rack)| {
            rack.iter().enumerate().any(|(source, m)| m.targets.iter().any(|t| self.driven_by(at, source, &t.kind) == Some(driven)))
        })
    }

    /// The lane that is member `member` of group `group`: the `member`th of the lanes in it.
    fn group_member(&self, group: usize, member: usize) -> Option<&InstrumentNode> {
        self.instruments.iter().filter(|lane| lane.group == Some(group)).nth(member)
    }

    /// Put the lanes at `a` and `b` in each other's places, each with everything it has (its
    /// plugins and effects, modulators, volume, range, group and pattern: the tree moves whole
    /// nodes), as the audio thread's `SwapInstruments` does. A group's cables address their members
    /// by the place of the lane among the group's lanes, so they are told the places the lanes have
    /// now (`member_moves`). False, and nothing changed, if either is not there or they are one.
    pub fn swap_lanes(&mut self, a: usize, b: usize) -> bool {
        let count = self.instruments.len();
        if a == b || a >= count || b >= count {
            return false;
        }
        let groups: Vec<Option<usize>> = self.instruments.iter().map(|lane| lane.group).collect();
        let moves = member_moves(&groups, a, b);
        self.instruments.swap(a, b);
        for (index, group) in self.groups.iter_mut().enumerate() {
            let of_group: Vec<(usize, usize)> = moves.iter().filter(|(g, ..)| *g == index).map(|&(_, old, new)| (old, new)).collect();
            if of_group.is_empty() {
                continue;
            }
            for target in group.modulators.iter_mut().flat_map(|m| &mut m.targets) {
                if let Some(member) = group_target_member_mut(&mut target.kind) {
                    if let Some(&(_, new)) = of_group.iter().find(|(old, _)| old == member) {
                        *member = new;
                    }
                }
            }
        }
        true
    }

    /// The cables of other modulators that would be left pointing at nothing if modulator `index`
    /// of the rack at `at` were taken out: those of its siblings (and, for a lane's, of the group
    /// the lane is a member of) that drive its rate, its stages or the depth of its cables. (Its
    /// own cables go with it. The cables that drive the depth of any of these go with *them*:
    /// `remove_edge`.)
    pub fn cables_into(&self, at: ModulatorLocation, index: usize) -> Vec<EdgeId> {
        let rack = match at {
            ModulatorLocation::Lane(inst) => self.instruments.get(inst).map(|lane| &lane.modulators),
            ModulatorLocation::Group(group) => self.groups.get(group).map(|g| &g.modulators),
        };
        let Some(rack) = rack else { return Vec::new() };
        let siblings = rack.iter().enumerate().filter(|(i, _)| *i != index).flat_map(|(_, m)| &m.targets);
        let mut found: Vec<EdgeId> = siblings.filter(|t| t.kind.sibling_modulator() == Some(index)).map(|t| t.edge).collect();
        if let ModulatorLocation::Lane(inst) = at {
            if let Some((group, member)) = lane_member_ordinal(&self.instruments, inst) {
                let group_cables = self.groups.get(group).into_iter().flat_map(|g| &g.modulators).flat_map(|m| &m.targets);
                found.extend(
                    group_cables
                        .filter(|t| matches!(t.kind, ModTargetKind::GroupMemberMod { member: of, mod_index, .. } if of == member && mod_index == index))
                        .map(|t| t.edge),
                );
            }
        }
        found
    }

    /// The cables that drive a parameter of the plugin in node `plugin`, an instrument, an effect or a bus
    /// effect: a lane's own, and a group's to its members' plugins and to its bus effects. In the order of the
    /// racks (the lanes', then the groups'), each modulator's by place.
    pub fn cables_into_plugin(&self, plugin: NodeId) -> Vec<EdgeId> {
        let lanes = self.instruments.iter().enumerate().map(|(i, lane)| (ModulatorLocation::Lane(i), &lane.modulators));
        let groups = self.groups.iter().enumerate().map(|(i, group)| (ModulatorLocation::Group(i), &group.modulators));
        let mut found = Vec::new();
        for (at, rack) in lanes.chain(groups) {
            for (source, modulator) in rack.iter().enumerate() {
                for target in &modulator.targets {
                    if matches!(self.driven_by(at, source, &target.kind), Some(Driven::Param { plugin: driven, .. }) if driven == plugin) {
                        found.push(target.edge);
                    }
                }
            }
        }
        found
    }

    /// The cables that drive the depth of cable `edge`: in the rack of its modulator, and, if
    /// the modulator is a lane's and the lane is a member of a group, in the group's.
    fn depth_cables_of(&self, edge: EdgeId) -> Vec<EdgeId> {
        let Some((at, mod_index, target)) = self.locate_edge(edge) else { return Vec::new() };
        let rack = match at {
            ModulatorLocation::Lane(inst) => self.instruments.get(inst).map(|lane| &lane.modulators),
            ModulatorLocation::Group(group) => self.groups.get(group).map(|g| &g.modulators),
        };
        let drivers = |rack: Option<&Vec<ModulatorSlot>>, drives: ModTargetKind| -> Vec<EdgeId> {
            rack.into_iter().flatten().flat_map(|m| &m.targets).filter(|t| t.kind == drives).map(|t| t.edge).collect()
        };
        let mut found = drivers(rack, ModTargetKind::ModulatorDepth { mod_index, target_index: target });
        if let ModulatorLocation::Lane(inst) = at {
            if let Some((group, member)) = lane_member_ordinal(&self.instruments, inst) {
                let field = CrossModField::Depth(target);
                found.extend(drivers(self.groups.get(group).map(|g| &g.modulators), ModTargetKind::GroupMemberMod { member, mod_index, field }));
            }
        }
        found
    }

    /// Take cable `target` out of modulator `mod_index` in the rack at `at`, and tell the cables
    /// that drive the depths of its later ones (`fixup_depth_cables_after_remove`). The cable, and what it was
    /// centred on; `None`, and nothing changed, if there is no such cable.
    fn remove_target_at(&mut self, at: ModulatorLocation, mod_index: usize, target: usize) -> Option<RemovedCable> {
        // (What it drives is read while the cable is there: another cable that drives its depth is taken out first.)
        let kind = self.modulators(at)?.get(mod_index)?.targets.get(target)?.kind.clone();
        let centre = self.centre_of(at, mod_index, &kind);
        let still_driven = self.drives_depth_of(at, mod_index, target);
        let rack = self.modulators_mut(at)?;
        let slot = rack[mod_index].targets.remove(target);
        fixup_depth_cables_after_remove(rack, mod_index, target);
        if let ModulatorLocation::Lane(inst) = at {
            if let Some((group, member)) = lane_member_ordinal(&self.instruments, inst) {
                if let Some(g) = self.groups.get_mut(group) {
                    fixup_member_depth_cables_after_remove(&mut g.modulators, member, mod_index, target);
                }
            }
        }
        let base_value = centre.unwrap_or(slot.param_min);
        Some(RemovedCable { at, mod_index, target_index: target, slot, base_value, still_driven })
    }

    /// Whether a cable that is there drives the depth of the cable at place `target` of the modulator at `mod_index` of the rack at `at`. (The
    /// cables that drive a cable go before it, unless they drive each other's: a group's cables to a member's cable always go first, since a
    /// lane's cables cannot reach the group's.)
    fn drives_depth_of(&self, at: ModulatorLocation, mod_index: usize, target: usize) -> bool {
        let drives_it = |kind: &ModTargetKind| matches!(kind, ModTargetKind::ModulatorDepth { mod_index: of, target_index } if *of == mod_index && *target_index == target);
        self.modulators(at).into_iter().flatten().flat_map(|m| &m.targets).any(|t| drives_it(&t.kind))
    }

    /// Put a cable into modulator `mod_index` of the rack at `at`, at place `target_index` (the last, for a place past the end): what
    /// `remove_edge` took out, put back (the cables of the removal in the reverse order, each at the place it had when its turn came). The
    /// cables of the rack that drive the depth of a later cable of the modulator, and the group's when the lane is a member, are told the
    /// place that cable has now, before the cable goes in. The place it went to; `None`, and nothing changed, if there is no such modulator.
    pub fn insert_target_at(&mut self, at: ModulatorLocation, mod_index: usize, target_index: usize, slot: ModTargetSlot) -> Option<usize> {
        let rack = self.modulators_mut(at)?;
        let place = target_index.min(rack.get(mod_index)?.targets.len());
        shift_depth_cables_after_insert(rack, mod_index, place);
        rack[mod_index].targets.insert(place, slot);
        if let ModulatorLocation::Lane(inst) = at {
            if let Some((group, member)) = lane_member_ordinal(&self.instruments, inst) {
                if let Some(g) = self.groups.get_mut(group) {
                    shift_member_depth_cables_after_insert(&mut g.modulators, member, mod_index, place);
                }
            }
        }
        Some(place)
    }

    /// Take the modulator at `index` out of the rack at `at`, with its own cables, as the audio thread's `RemoveModulator` and `RemoveGroupModulator`
    /// do: the cables of its siblings that address it are dropped, and those that address a later modulator are told the place it has now
    /// (`fixup_cross_mod_after_remove`); for a lane that is a member of a group, so are the group's cables to the lane's modulators
    /// (`fixup_group_member_mod_after_remove`). (The controller takes out the cables that address it first, so none is dropped there.) `None`, and
    /// nothing changed, if there is no such modulator.
    pub fn remove_modulator_at(&mut self, at: ModulatorLocation, index: usize) -> Option<ModulatorSlot> {
        let member_of = self.member_of_rack(at);
        let rack = self.modulators_mut(at)?;
        if index >= rack.len() {
            return None;
        }
        let removed = rack.remove(index);
        fixup_cross_mod_after_remove(rack, index);
        if let Some((group, member)) = member_of {
            if let Some(g) = self.groups.get_mut(group) {
                fixup_group_member_mod_after_remove(&mut g.modulators, member, index);
            }
        }
        Some(removed)
    }

    /// Put a modulator, with its own cables, into the rack at `at` at place `index` (the last, for a place past the end): what `remove_modulator_at`
    /// took out, put back. If it goes in before others, the cables of its siblings that address a modulator at that place or after it, and the group's to
    /// the lane's modulators when the lane is a member, are told the place that modulator has now, before it goes in (its own cables say the places that
    /// there are by then); one that goes in at the end moves nothing. The place it went to; `None`, and nothing changed, if there is no such rack.
    pub fn insert_modulator_at(&mut self, at: ModulatorLocation, index: usize, modulator: ModulatorSlot) -> Option<usize> {
        let member_of = self.member_of_rack(at);
        let rack = self.modulators_mut(at)?;
        let place = index.min(rack.len());
        // (One that goes in at the end moves nothing: the audio thread is loaded a modulator at a time, with cables to ones that come later, and does the same.)
        let in_front = place < rack.len();
        if in_front {
            shift_cross_mod_after_insert(rack, place);
        }
        rack.insert(place, modulator);
        if let Some((group, member)) = member_of.filter(|_| in_front) {
            if let Some(g) = self.groups.get_mut(group) {
                shift_group_member_mod_after_insert(&mut g.modulators, member, place);
            }
        }
        Some(place)
    }

    /// The group, and the lane's place among its members, when the rack at `at` is the rack of a lane that is a member of a group.
    fn member_of_rack(&self, at: ModulatorLocation) -> Option<(usize, usize)> {
        match at {
            ModulatorLocation::Lane(inst) => lane_member_ordinal(&self.instruments, inst),
            ModulatorLocation::Group(_) => None,
        }
    }

    /// The plugin parameters a modulator in the rack at `at` can drive, in the order a
    /// person is offered them. A lane's modulator drives its lane's chain: the instrument,
    /// then each effect. A group's drives its members' chains, member by member in lane
    /// order, and then its own bus effects. Nothing else is a parameter a modulator can
    /// drive (the fields of other modulators are another kind of target), and a rack that
    /// is not there drives nothing.
    pub fn param_targets(&self, at: ModulatorLocation) -> Vec<ParamTarget> {
        let mut targets = Vec::new();
        match at {
            ModulatorLocation::Lane(inst) => {
                if let Some(lane) = self.instruments.get(inst) {
                    push_chain(&mut targets, lane, |slot, param_index| ModTargetKind::PluginParam { slot, param_index });
                }
            }
            ModulatorLocation::Group(group) => {
                let Some(bus) = self.groups.get(group) else { return targets };
                for (member, lane) in self.instruments.iter().filter(|lane| lane.group == Some(group)).enumerate() {
                    push_chain(&mut targets, lane, |slot, param_index| ModTargetKind::GroupMember { member, slot, param_index });
                }
                for (effect_index, effect) in bus.effects.iter().enumerate() {
                    push_params(&mut targets, effect, 0, |param_index| ModTargetKind::GroupBus { effect_index, param_index });
                }
            }
        }
        targets
    }

    /// The fields of other modulators that the modulator at `mod_index` of the rack at `at` can drive: a rate, the stages of an
    /// envelope and the depths of cables (a cable that pulls a parameter, or a field, pulls it by a depth another modulator can
    /// drive), in the order a person is offered them. A lane's modulator drives those of the others in its own rack, a group's
    /// those of its members' modulators (member by member in lane order, each modulator's settings and then its cables) and then
    /// those of the others in its own rack. Never its own (a modulator does not drive itself), and a rack that is not there
    /// drives nothing. (An index that is not in the rack leaves none out.)
    pub fn field_targets(&self, at: ModulatorLocation, mod_index: usize) -> Vec<FieldTarget> {
        let mut targets = Vec::new();
        match at {
            ModulatorLocation::Lane(inst) => {
                if let Some(lane) = self.instruments.get(inst) {
                    push_siblings(&mut targets, &lane.modulators, mod_index);
                }
            }
            ModulatorLocation::Group(group) => {
                let Some(bus) = self.groups.get(group) else { return targets };
                for (member, lane) in self.instruments.iter().filter(|lane| lane.group == Some(group)).enumerate() {
                    for (index, modulator) in lane.modulators.iter().enumerate() {
                        let owner = format!("M{member} Mod{index}");
                        push_fields(&mut targets, modulator, owner, |field| ModTargetKind::GroupMemberMod { member, mod_index: index, field });
                    }
                }
                push_siblings(&mut targets, &bus.modulators, mod_index);
            }
        }
        targets
    }

    /// The modulators whose settings the modulator at `mod_index` of the rack at `at` can be drawn to, in the order of
    /// `field_targets`: the others of its own rack, and for a group's those of its members first. A modulator in reach may have
    /// nothing to drive (a MIDI control that has no cable has no setting and no depth), and so is not in `field_targets`.
    pub fn modulators_in_reach(&self, at: ModulatorLocation, mod_index: usize) -> Vec<NodeId> {
        let others = |rack: &[ModulatorSlot]| -> Vec<NodeId> {
            rack.iter().enumerate().filter(|(index, _)| *index != mod_index).map(|(_, modulator)| modulator.node).collect()
        };
        match at {
            ModulatorLocation::Lane(inst) => self.instruments.get(inst).map(|lane| others(&lane.modulators)).unwrap_or_default(),
            ModulatorLocation::Group(group) => {
                let Some(bus) = self.groups.get(group) else { return Vec::new() };
                let members = self.instruments.iter().filter(|lane| lane.group == Some(group));
                let mut reach: Vec<NodeId> = members.flat_map(|lane| lane.modulators.iter().map(|modulator| modulator.node)).collect();
                reach.extend(others(&bus.modulators));
                reach
            }
        }
    }

    /// Take the effect at `index` out of lane `inst`'s chain, the way the audio
    /// thread's `RemoveEffect` does: the modulator targets on it go with it, and
    /// the ones on later plugins of the chain move down a slot so that they keep
    /// pointing at the same plugins. Those are the lane's own modulators and, if
    /// the lane is a group member, the group's. `None` (and no change) if the lane
    /// has no such effect.
    pub fn remove_effect(&mut self, inst: usize, index: usize) -> Option<PluginSlot> {
        let member_of = lane_member_ordinal(&self.instruments, inst);
        let lane = self.instruments.get_mut(inst)?;
        if index >= lane.effects.len() {
            return None;
        }
        let removed = lane.effects.remove(index);
        fixup_slots_after_remove(&mut lane.modulators, index);
        if let Some((group, member)) = member_of {
            if let Some(g) = self.groups.get_mut(group) {
                fixup_member_slots_after_remove(&mut g.modulators, member, index);
            }
        }
        Some(removed)
    }

    /// Put a plugin into lane `inst`'s chain at `index` (the last place, for one past the end): what `remove_effect` took out, put back, the way the audio
    /// thread's `InsertEffect` does it. The targets on the effects at that place and after it, the lane's own modulators' and, if the lane is a member of a
    /// group, the group's, are told the slot they have now, before it goes in, so that they keep pointing at the same plugins. The place it went to; `None`,
    /// and nothing changed, if there is no such lane.
    pub fn insert_effect_at(&mut self, inst: usize, index: usize, effect: PluginSlot) -> Option<usize> {
        let member_of = lane_member_ordinal(&self.instruments, inst);
        let lane = self.instruments.get_mut(inst)?;
        let place = index.min(lane.effects.len());
        shift_slots_after_insert(&mut lane.modulators, place);
        lane.effects.insert(place, effect);
        if let Some((group, member)) = member_of {
            if let Some(g) = self.groups.get_mut(group) {
                shift_member_slots_after_insert(&mut g.modulators, member, place);
            }
        }
        Some(place)
    }

    /// Put a plugin into group `group`'s bus at `index` (the last place, for one past the end): what `remove_bus_effect` took out, put back, the way the
    /// audio thread's `InsertGroupEffect` does it: the group's cables to the bus effects at that place and after it are told the place they have now, before
    /// it goes in. The place it went to; `None`, and nothing changed, if there is no such group.
    pub fn insert_bus_effect_at(&mut self, group: usize, index: usize, effect: PluginSlot) -> Option<usize> {
        let bus = self.groups.get_mut(group)?;
        let place = index.min(bus.effects.len());
        shift_group_bus_after_insert(&mut bus.modulators, place);
        bus.effects.insert(place, effect);
        Some(place)
    }

    /// The cables of the group that lane `inst` is a member of that go to the lane: to its plugins, and to
    /// its modulators' settings and the depths of their cables. (The lane's own modulators' cables, which go
    /// with it, are not these.) None if the lane is in no group.
    pub fn group_cables_into_lane(&self, inst: usize) -> Vec<EdgeId> {
        let Some((group, member)) = lane_member_ordinal(&self.instruments, inst) else { return Vec::new() };
        let cables = self.groups.get(group).into_iter().flat_map(|g| &g.modulators).flat_map(|m| &m.targets);
        cables
            .filter(|t| matches!(t.kind, ModTargetKind::GroupMember { member: of, .. } | ModTargetKind::GroupMemberMod { member: of, .. } if of == member))
            .map(|t| t.edge)
            .collect()
    }

    /// Take lane `inst` out, the way the audio thread's `RemoveInstrument` does, with everything it has: its
    /// plugins, modulators and pattern. If it was a member of a group, the group's cables to it go with it, and
    /// the ones to the members after it are told their new ordinals. `None` (and no change) if there is no
    /// such lane.
    pub fn remove_lane(&mut self, inst: usize) -> Option<InstrumentNode> {
        let member_of = lane_member_ordinal(&self.instruments, inst);
        if inst >= self.instruments.len() {
            return None;
        }
        let lane = self.instruments.remove(inst);
        if let Some((group, ordinal)) = member_of {
            if let Some(g) = self.groups.get_mut(group) {
                fixup_group_member_after_remove(&mut g.modulators, ordinal);
            }
        }
        Some(lane)
    }

    /// Put a lane back among the lanes at `index` (the last place, for one past the end): what `remove_lane` took out, put back, the way the audio thread's
    /// `InsertInstrument` does it. If it is a member of a group, the cables of the group to the members from its place on are told the ordinals they have now
    /// before it goes in (they were told the others' when it went); a group that is not there is no group. The place it went to.
    pub fn insert_lane_at(&mut self, index: usize, mut lane: InstrumentNode) -> usize {
        if lane.group.is_some_and(|group| group >= self.groups.len()) {
            lane.group = None;
        }
        let place = index.min(self.instruments.len());
        self.instruments.insert(place, lane);
        if let Some((group, ordinal)) = lane_member_ordinal(&self.instruments, place) {
            if let Some(bus) = self.groups.get_mut(group) {
                shift_group_member_after_insert(&mut bus.modulators, ordinal);
            }
        }
        place
    }

    /// Take group `group` out, with its bus effects and its modulators, the way the audio thread's `RemoveGroup`
    /// does: its member lanes are in no group, and the lanes of the groups after it have the index that is one
    /// less. `None` (and no change) if there is no such group.
    pub fn remove_group(&mut self, group: usize) -> Option<GroupNode> {
        if group >= self.groups.len() {
            return None;
        }
        let removed = self.groups.remove(group);
        for lane in &mut self.instruments {
            match lane.group {
                Some(of) if of == group => lane.group = None,
                Some(of) if of > group => lane.group = Some(of - 1),
                _ => {}
            }
        }
        Some(removed)
    }

    /// Put a group back among the groups at `index` (the last place, for one past the end): what `remove_group` took out, put back, the way the audio thread's
    /// `InsertGroup` does it. The lanes that are in the groups from that place on are told the group they are in now (one more, which the removal told them the other
    /// way), and the lanes in `members` (places among the lanes: one that is no lane is nobody's) are its members again. The cables of the group are not here: the
    /// controller takes them out before the group goes and puts them back after it. The place it went to.
    pub fn insert_group_at(&mut self, index: usize, group: GroupNode, members: &[usize]) -> usize {
        let place = index.min(self.groups.len());
        for lane in &mut self.instruments {
            if let Some(of) = lane.group.as_mut().filter(|of| **of >= place) {
                *of += 1;
            }
        }
        self.groups.insert(place, group);
        for &inst in members {
            if let Some(lane) = self.instruments.get_mut(inst) {
                lane.group = Some(place);
            }
        }
        place
    }

    /// Put lane `inst` in group `group`, or in none, the way the audio thread's `SetLaneGroup` does: the group
    /// it leaves drops its modulators' targets on the lane and moves the ones on its later members down an
    /// ordinal, and the group it joins moves the ones on the members from the lane's place on up one. False
    /// (and no change) if there is no such lane or no such group.
    pub fn set_lane_group(&mut self, inst: usize, group: Option<usize>) -> bool {
        if inst >= self.instruments.len() || group.is_some_and(|g| g >= self.groups.len()) {
            return false;
        }
        let old = lane_member_ordinal(&self.instruments, inst);
        self.instruments[inst].group = group;
        let new = lane_member_ordinal(&self.instruments, inst);
        if old.map(|(g, _)| g) != new.map(|(g, _)| g) {
            if let Some((leaves, ordinal)) = old {
                if let Some(g) = self.groups.get_mut(leaves) {
                    fixup_group_member_after_remove(&mut g.modulators, ordinal);
                }
            }
            if let Some((joins, ordinal)) = new {
                if let Some(g) = self.groups.get_mut(joins) {
                    shift_group_member_after_insert(&mut g.modulators, ordinal);
                }
            }
        }
        true
    }

    /// Take the bus effect at `index` out of group `group`, the way the audio thread's `RemoveGroupEffect`
    /// does: the group's modulator targets on it go with it, and the ones on later bus effects move down an
    /// index so that they keep pointing at the same plugins. `None` (and no change) if the group has no such
    /// effect.
    pub fn remove_bus_effect(&mut self, group: usize, index: usize) -> Option<PluginSlot> {
        let bus = self.groups.get_mut(group)?;
        if index >= bus.effects.len() {
            return None;
        }
        let removed = bus.effects.remove(index);
        fixup_group_bus_after_remove(&mut bus.modulators, index);
        Some(removed)
    }

    /// Move the effect at `from` to `to` in lane `inst`'s chain, the way the audio
    /// thread's `ReorderEffect` does: the modulator targets of the lane and of its
    /// group follow their plugins to the new slots. False (and no change) if either
    /// index is outside the chain or they are the same.
    pub fn move_effect(&mut self, inst: usize, from: usize, to: usize) -> bool {
        let member_of = lane_member_ordinal(&self.instruments, inst);
        let Some(lane) = self.instruments.get_mut(inst) else { return false };
        if from >= lane.effects.len() || to >= lane.effects.len() || from == to {
            return false;
        }
        let effect = lane.effects.remove(from);
        lane.effects.insert(to, effect);
        remap_slots_after_reorder(&mut lane.modulators, from, to);
        if let Some((group, member)) = member_of {
            if let Some(g) = self.groups.get_mut(group) {
                remap_member_slots_after_reorder(&mut g.modulators, member, from, to);
            }
        }
        true
    }

    /// Move the effect at `from` to `to` in group `group`'s bus, the way the audio thread's
    /// `ReorderGroupEffect` does: the group's modulator targets on the bus effects follow their plugins
    /// to the new places (a member's chain is another one: nothing in it moves). False (and no change)
    /// if either index is outside the bus, they are the same, or the group is not there.
    pub fn move_bus_effect(&mut self, group: usize, from: usize, to: usize) -> bool {
        let Some(bus) = self.groups.get_mut(group) else { return false };
        if from >= bus.effects.len() || to >= bus.effects.len() || from == to {
            return false;
        }
        let effect = bus.effects.remove(from);
        bus.effects.insert(to, effect);
        remap_group_bus_after_reorder(&mut bus.modulators, from, to);
        true
    }
}

/// Default dry/wet mix for a newly added effect. Half-wet suits blend/parallel
/// effects (reverb, delay), the common DAW default for sends.
pub const DEFAULT_EFFECT_MIX: f32 = 0.5;

/// Default dry/wet mix for a freshly added effect, by plugin. Filters and
/// other tone-shaping inserts must be fully wet — at 50% the dry path passes
/// the very frequencies the filter removes, so e.g. a highpass barely changes
/// the sound — and so must a drive, which is an insert too (what it does to
/// the sound is the point, and half of it is a parallel distortion that is a
/// choice, not a default). A drive's latency (the oversampling it makes its
/// shapes with) is no reason now: the host makes the dry sound as late as the
/// wet one, so a drive that is blended is in step.
/// Blend effects (reverb) keep the half-wet default.
pub fn default_effect_mix(id: &str) -> f32 {
    if id == "builtin:filter" || id == "builtin:drive" {
        1.0
    } else {
        DEFAULT_EFFECT_MIX
    }
}

// ---------------------------------------------------------------------------
// Plugin slot — main-thread mirror of what the audio thread has
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct PluginSlot {
    /// The node the plugin sits in: its own for an effect or a bus effect, the
    /// lane's for an instrument.
    pub node: NodeId,
    pub name: String,
    pub format: String,
    #[allow(dead_code)]
    pub id: String,
    #[allow(dead_code)]
    pub is_instrument: bool,
    pub params: Vec<ParamSlot>,
    /// All presets the plugin reports, cached at load time. Used by the
    /// preset selector popup.
    pub presets: Vec<plugin::Preset>,
    /// Name of the currently-loaded preset (if any). Tracked for save
    /// round-trip and display.
    pub current_preset: Option<String>,
    /// Effect dry/wet mix (1.0 = full wet). Only meaningful for effect slots;
    /// instrument slots leave this at 1.0 and don't read it.
    pub mix: f32,
    /// Whether the link into an effect from the stage before it in its chain is there (step 86): `true` for the way a chain is wired by itself, `false` once it is cut (`SetFeed`), when
    /// the effect is fed by the audio cables that end at it and by nothing else. Only meaningful for an effect or a bus effect; an instrument has nothing before it and leaves this `true`.
    /// A session file keeps it as `feed = "none"`; the engine has it by the effect's node (`GraphCommand::SetEffectFeed`), so it goes where its effect goes.
    pub fed: bool,
    /// The parameters (`ParamSlot::index`) a front-end shows as knobs on the plugin's node, in the order they were pinned; none pinned
    /// is the first few (`docs/web-ui.md` §4.4). The engine and the TUI do not use it; a session file keeps it as `pinned`, by name.
    pub pinned: Vec<u32>,
}

impl PluginSlot {
    /// The names of the pinned parameters, in order, as a session file keeps them.
    pub fn pinned_names(&self) -> Vec<String> {
        self.pinned.iter().filter_map(|index| self.params.iter().find(|p| p.index == *index)).map(|p| p.name.clone()).collect()
    }

    /// Pin the parameters of these names, in this order, as a session file says them. A name the plugin has no parameter of (or one
    /// that is no control, a separator) is not a pin, and nor is a name that comes twice (the first place counts): each is logged.
    pub fn pin_names(&mut self, names: &[String]) {
        self.pinned.clear();
        for name in names {
            match self.params.iter().find(|p| p.name == *name && !matches!(p.kind, ParamKind::Separator)) {
                Some(param) if self.pinned.contains(&param.index) => log::warn!("{}: '{name}' is pinned twice", self.name),
                Some(param) => self.pinned.push(param.index),
                None => log::warn!("{}: no parameter '{name}' to pin", self.name),
            }
        }
    }
}

#[derive(Debug, Default)]
pub enum ParamKind {
    #[default]
    Float,
    Enum(Vec<String>),
    Separator,
}

#[derive(Debug, Default)]
pub struct ParamSlot {
    pub name: String,
    pub index: u32,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub value: f32,
    pub kind: ParamKind,
    /// What the value is measured in, for showing it (`ParameterInfo::unit`).
    #[cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "read by the graph a client gets (`app::graph`), which only the web runtime sends"))]
    pub unit: Option<String>,
    /// The sections the plugin files the parameter under, outermost first
    /// (`ParameterInfo::group`).
    #[cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "read by the graph a client gets (`app::graph`), which only the web runtime sends"))]
    pub group: Vec<String>,
    /// How many steps there are between `min` and `max`; 0 for a continuous
    /// parameter (`ParameterInfo::step_count`).
    #[cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "read by the graph a client gets (`app::graph`), which only the web runtime sends"))]
    pub step_count: u32,
}

// ---------------------------------------------------------------------------
// Lanes and groups
// ---------------------------------------------------------------------------

/// Main-thread mirror of pattern state for an instrument.
#[derive(Debug)]
pub struct PatternState {
    pub bpm: f32,
    pub length_beats: f32,
    pub looping: bool,
    pub base_note: Option<u8>,
    pub events: Vec<(u64, u8, u8, u8)>, // (frame, status, note, velocity)
    pub enabled: bool,
    pub recording: bool,
    /// Transpose playback by scale degrees (in-key) instead of semitones.
    pub in_key: bool,
}

impl PatternState {
    /// A pattern that has nothing recorded: what a lane that has none is given when its recording starts or one of its settings is set, at the tempo `bpm`.
    pub fn empty(bpm: f32) -> Self {
        PatternState { bpm, length_beats: 4.0, looping: true, base_note: None, events: Vec::new(), enabled: false, recording: false, in_key: false }
    }

    /// What a front-end is told of it: its settings and how many notes it has (the notes themselves are not sent).
    pub fn info(&self) -> crate::app::graph::PatternInfo {
        crate::app::graph::PatternInfo {
            recording: self.recording,
            enabled: self.enabled,
            looping: self.looping,
            in_key: self.in_key,
            length_beats: self.length_beats,
            bpm: self.bpm,
            notes: self.events.iter().filter(|event| event.1 == 0x90).count() as u32,
        }
    }
}

#[derive(Debug)]
pub struct InstrumentNode {
    pub node: NodeId,
    pub range: Option<(u8, u8)>,
    pub transpose: i8,
    /// Host-side output gain, applied before effects (1.0 = unity).
    pub volume: f32,
    /// Muted: never heard (a mute wins over a solo); the lane's notes, modulators and pattern go on.
    pub muted: bool,
    /// Soloed: while anything is, only what is soloed is heard (and the group a soloed lane is in).
    pub solo: bool,
    /// Where the lane's end goes: to the bus of its group or to the mix (`Route::Out`), or nowhere but its audio cables (`Route::None`).
    pub route: Route,
    pub instrument: Option<PluginSlot>,
    pub effects: Vec<PluginSlot>,
    /// Lane-scoped modulators. Each target addresses a plugin in this chain by
    /// slot (0 = instrument, 1..N = effects).
    pub modulators: Vec<ModulatorSlot>,
    pub pattern: Option<PatternState>,
    /// Carried through from session config; not editable in the UI.
    pub pitch_bend_range: f64,
    pub remap: std::collections::HashMap<String, crate::session::RemapTarget>,
    /// Submix group membership: index into the session's groups, or None.
    pub group: Option<usize>,
    /// Where the audio thread publishes the lane's output level (telemetry);
    /// `None` if the slots had run out.
    #[cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "read by the telemetry layout a client gets (`app::layout`), which only the web runtime sends"))]
    pub meter: Option<MeterSlots>,
}

impl InstrumentNode {
    /// The lane has no note remap any more, nor the pitch bend range that went with it: the audio thread drops the remapper
    /// when the instrument it was for goes (a swap, a clear), and the model says what the audio thread has.
    pub fn forget_remap(&mut self) {
        self.remap.clear();
        self.pitch_bend_range = 2.0;
    }

    /// The node of the plugin in `slot` of the lane's chain (0 is the instrument, 1.. the effects), if there is one.
    pub fn plugin_node(&self, slot: usize) -> Option<NodeId> {
        match slot {
            0 => self.instrument.as_ref().map(|plugin| plugin.node),
            effect => self.effects.get(effect - 1).map(|plugin| plugin.node),
        }
    }
}

/// A submix group: its members are the instruments whose `group` points here;
/// it sums them and runs its own effect chain + volume.
#[derive(Debug)]
pub struct GroupNode {
    pub node: NodeId,
    pub name: Option<String>,
    pub volume: f32,
    /// Stereo balance pan for the group bus (-1 left … 0 center … +1 right).
    pub pan: f32,
    /// Muted: the bus is never heard (a mute wins over a solo); the members and the bus effects go on.
    pub muted: bool,
    /// Soloed: while anything is, only what is soloed is heard, and every member of this group.
    pub solo: bool,
    /// Where the group's end goes: to the mix (`Route::Out`), or nowhere but its audio cables (`Route::None`).
    pub route: Route,
    pub effects: Vec<PluginSlot>,
    /// Group-scoped modulators. Each target addresses a member instrument
    /// (`GroupMember`), a bus effect (`GroupBus`), or a sibling group modulator.
    pub modulators: Vec<ModulatorSlot>,
    /// Where the audio thread publishes the bus's output level (telemetry);
    /// `None` if the slots had run out.
    #[cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "read by the telemetry layout a client gets (`app::layout`), which only the web runtime sends"))]
    pub meter: Option<MeterSlots>,
}

impl GroupNode {
    /// What the group is called: its name, or "Group N" counting from one by its
    /// position `index` among the session's groups.
    pub fn display_name(&self, index: usize) -> String {
        self.name.clone().unwrap_or_else(|| format!("Group {}", index + 1))
    }
}

#[derive(Debug)]
pub enum ModSourceSlot {
    /// An LFO: its waveform and rate, whether a note starts it again at the start of its wave, how many seconds it takes to come in after a note (0 for none) and the note length of the tempo
    /// that a cycle of it is instead of its rate, as a place in `SYNC_DIVISIONS` from 1 (0 for none: it goes at its rate, which is kept for when it does).
    Lfo {
        waveform: crate::plugin::chain::LfoWaveform,
        rate: f32,
        retrigger: bool,
        fade: f32,
        sync: usize,
    },
    Envelope {
        attack: f32,
        decay: f32,
        sustain: f32,
        release: f32,
    },
    /// A MIDI-learned control (CC or pitch bend). `source` is None while
    /// `learning` (armed for the next incoming control).
    MidiLearn {
        source: Option<crate::plugin::chain::MidiBindSource>,
        learning: bool,
    },
    /// A number that the person sets, 0 to 1: a knob that drives what has a cable from it.
    Macro {
        value: f32,
    },
    /// A step sequencer: the values it goes through, -1 to 1 (the first `length` of them are in use, the others are kept for when it is longer), how many
    /// steps it goes through a second, how much of each step is spent gliding to the next one's value, whether a note starts it again at the first step, and the note length of the tempo
    /// that a step of it is instead of its rate (a place in `SYNC_DIVISIONS` from 1, or 0 for none).
    Steps {
        values: [f32; crate::plugin::chain::MAX_STEPS],
        length: usize,
        rate: f32,
        glide: f32,
        retrigger: bool,
        sync: usize,
    },
}

impl ModSourceSlot {
    /// What a modulator added without further ado starts as: a 1 Hz sine.
    pub fn new_lfo() -> Self {
        ModSourceSlot::Lfo { waveform: crate::plugin::chain::LfoWaveform::Sine, rate: 1.0, retrigger: false, fade: 0.0, sync: 0 }
    }

    /// An envelope as the modulate popup makes it: 10 ms attack, 0.3 s decay,
    /// sustain 0.7, 0.5 s release.
    pub fn new_envelope() -> Self {
        ModSourceSlot::Envelope { attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 }
    }

    /// A control that is waiting for the next one that comes in to bind to.
    pub fn new_midi_learn() -> Self {
        ModSourceSlot::MidiLearn { source: None, learning: true }
    }

    /// A control that follows the notes the lane plays (`MidiBindSource::Velocity` or `Key`): bound when it is made, since no controller is moved for it.
    pub fn new_note_control(source: crate::plugin::chain::MidiBindSource) -> Self {
        ModSourceSlot::MidiLearn { source: Some(source), learning: false }
    }

    /// A macro at rest: it drives nothing until it is turned.
    pub fn new_macro() -> Self {
        ModSourceSlot::Macro { value: 0.0 }
    }

    /// A step sequencer as the modulate popup makes it: all sixteen steps in use, a staircase that climbs from the bottom in eighths (so that it shows at once
    /// that it goes round), eight steps a second (sixteenths at the 120 beats a minute that a session begins at), no glide, and started again by every note.
    pub fn new_steps() -> Self {
        ModSourceSlot::Steps {
            values: ModSourceSlot::staircase(),
            length: crate::plugin::chain::MAX_STEPS,
            rate: 8.0,
            glide: 0.0,
            retrigger: true,
            sync: 0,
        }
    }

    /// The steps of a step sequencer that is new: a staircase that climbs from -1 in eighths.
    pub fn staircase() -> [f32; crate::plugin::chain::MAX_STEPS] {
        std::array::from_fn(|i| i as f32 / 8.0 - 1.0)
    }

    /// The values of a step sequencer, if it is one.
    pub fn steps(&self) -> Option<[f32; crate::plugin::chain::MAX_STEPS]> {
        match self {
            ModSourceSlot::Steps { values, .. } => Some(*values),
            _ => None,
        }
    }

    /// What the modulator's node is called for a client: its kind, or what a control
    /// is bound to. Not its settings, which a client is told when they change and
    /// which a name would have to follow (`label` has them, for the TUI).
    pub fn node_name(&self) -> String {
        match self {
            ModSourceSlot::Lfo { .. } => "LFO".to_string(),
            ModSourceSlot::Macro { .. } => "Macro".to_string(),
            ModSourceSlot::Steps { .. } => "Steps".to_string(),
            other => other.label(),
        }
    }

    /// A short label for the source (used in the tree and param pane).
    pub fn label(&self) -> String {
        use crate::plugin::chain::MidiBindSource;
        match self {
            ModSourceSlot::Lfo { waveform, rate, sync, .. } if *sync == 0 => format!("LFO {:.1}Hz {}", rate, waveform.name()),
            ModSourceSlot::Lfo { waveform, sync, .. } => format!("LFO {} {}", sync_name(*sync), waveform.name()),
            ModSourceSlot::Envelope { .. } => "ADSR".to_string(),
            ModSourceSlot::MidiLearn { source, learning } => match source {
                Some(MidiBindSource::Cc(n)) => format!("CC {n}"),
                Some(MidiBindSource::PitchBend) => "Pitch Bend".to_string(),
                Some(MidiBindSource::Aftertouch) => "Aftertouch".to_string(),
                Some(MidiBindSource::Velocity) => "Velocity".to_string(),
                Some(MidiBindSource::Key) => "Key".to_string(),
                None if *learning => "MIDI (learning…)".to_string(),
                None => "MIDI (unbound)".to_string(),
            },
            ModSourceSlot::Macro { value } => format!("Macro {value:.2}"),
            ModSourceSlot::Steps { length, rate, sync, .. } if *sync == 0 => format!("Steps {length} {rate:.1}Hz"),
            ModSourceSlot::Steps { length, sync, .. } => format!("Steps {length} {}", sync_name(*sync)),
        }
    }
}

#[derive(Debug)]
pub struct ModulatorSlot {
    pub node: NodeId,
    pub source: ModSourceSlot,
    pub targets: Vec<ModTargetSlot>,
    /// Where the audio thread publishes the modulator's output (telemetry);
    /// `None` if the slots had run out.
    pub output: Option<Slot>,
    /// Where the audio thread publishes where a step sequencer is (telemetry): the whole part of the number is the step it is at, the fraction how far
    /// into it. `None` for any other modulator, and for a step sequencer if the slots had run out.
    pub position: Option<Slot>,
}

#[derive(Debug)]
pub struct ModTargetSlot {
    /// The edge this target is, for as long as it exists.
    pub edge: EdgeId,
    /// Slot the target lives on: 0 = instrument, 1..N = effects. Used to label
    /// the target with its plugin and to resolve cross-mod vs plugin params.
    pub slot: usize,
    pub param_name: String,
    pub kind: crate::plugin::chain::ModTargetKind,
    pub depth: f32,
    #[allow(dead_code)]
    pub param_min: f32,
    #[allow(dead_code)]
    pub param_max: f32,
    /// Where the audio thread publishes the effective value of what the target
    /// drives (telemetry): a plugin's parameter, or a setting of a modulator or the
    /// depth of one of its cables; `None` only if the slots had run out.
    pub effective: Option<Slot>,
}

/// A cable that was taken out of the session (`SessionModel::remove_edge`): where it was, the cable itself, and what it was centred on. What puts it
/// back is `SessionModel::insert_target_at` and a command to the audio thread (`Controller::undo`).
#[derive(Debug)]
pub struct RemovedCable {
    /// The rack it was in.
    pub at: ModulatorLocation,
    /// The modulator in the rack, and the cable's place in it as it was when its turn came.
    pub mod_index: usize,
    pub target_index: usize,
    /// The cable: its id, the slot it publishes to, and all it says.
    pub slot: ModTargetSlot,
    /// What the audio thread had the cable centred on, which is what the user had set what it drove to (`SessionModel::centre_of`).
    pub base_value: f32,
    /// A cable that drives the depth of this one was still there when it went: they drive each other's depths, one of the two goes first
    /// and the other is left driving a place with no cable at it for the moment. Putting them back one at a time cannot tell that place
    /// from the place of the cable that was after it (both are the same number once the cable has gone), so such a removal is not one that
    /// can be put back.
    pub still_driven: bool,
}

/// A plugin parameter a modulator can drive (`SessionModel::param_targets`), with what it
/// takes to bind it (`Controller::add_target`) and to say what it is.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamTarget {
    /// The node of the plugin the parameter is on (a lane's own, for its instrument).
    pub node: NodeId,
    /// The plugin's own index for the parameter.
    pub param: u32,
    /// How the modulator's rack addresses it.
    pub kind: ModTargetKind,
    /// The plugin's slot in its chain: 0 for an instrument (and for a bus effect), 1.. for
    /// the effects of a lane. A target is labelled with its plugin by it.
    pub slot: usize,
    pub plugin_name: String,
    pub param_name: String,
    /// What the user has set the parameter to, where its modulation is centred.
    pub base_value: f32,
    pub param_min: f32,
    pub param_max: f32,
}

/// A field of a modulator that another modulator can drive (`SessionModel::field_targets`): its rate, a stage of its envelope or the
/// depth of one of its cables, with what it takes to bind it (`Controller::add_target`) and to say what it is.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldTarget {
    /// The node of the modulator whose field it is.
    pub node: NodeId,
    /// Which field it is.
    pub field: CrossModField,
    /// The cable, when the field is the depth of one (`Port::Depth` names it by this).
    pub edge: Option<EdgeId>,
    /// How the driving modulator's rack addresses it.
    pub kind: ModTargetKind,
    /// Whose field it is, as a person is told: `Mod 1` for the modulator at index 1 of the rack, `M0 Mod1` for the one at index 1
    /// of the first member of a group.
    pub owner: String,
    /// What the field is called: `rate`, `attack`, ..., or, for a cable's depth, `detune depth`.
    pub field_name: String,
    /// What the target is labelled with once it is bound: the owner and the name.
    pub label: String,
    /// What the field is set to now, where its modulation is centred.
    pub base_value: f32,
    pub param_min: f32,
    pub param_max: f32,
}

/// The fields of the modulators of `rack` but the one at `except`, which a modulator of the same rack addresses as its siblings'.
fn push_siblings(targets: &mut Vec<FieldTarget>, rack: &[ModulatorSlot], except: usize) {
    for (index, modulator) in rack.iter().enumerate().filter(|(index, _)| *index != except) {
        push_fields(targets, modulator, format!("Mod {index}"), |field| sibling_kind(index, field));
    }
}

/// The fields of `modulator`: the settings its source has, in the order of the pane (the rate; the four stages), and then the depth
/// of each of its cables. `kind` says how the rack that drives them addresses a field.
fn push_fields(targets: &mut Vec<FieldTarget>, modulator: &ModulatorSlot, owner: String, kind: impl Fn(CrossModField) -> ModTargetKind) {
    let mut push = |field: CrossModField, name: String, edge: Option<EdgeId>, base_value: f32, (param_min, param_max): (f32, f32)| {
        let (node, kind, label, owner) = (modulator.node, kind(field), format!("{owner} {name}"), owner.clone());
        targets.push(FieldTarget { node, field, edge, kind, owner, field_name: name, label, base_value, param_min, param_max });
    };
    const SETTINGS: [(CrossModField, ModulatorField, &str); 5] = [
        (CrossModField::Rate, ModulatorField::Rate, "rate"),
        (CrossModField::Attack, ModulatorField::Attack, "attack"),
        (CrossModField::Decay, ModulatorField::Decay, "decay"),
        (CrossModField::Sustain, ModulatorField::Sustain, "sustain"),
        (CrossModField::Release, ModulatorField::Release, "release"),
    ];
    for (field, setting, name) in SETTINGS {
        if let Some(value) = modulator.source.setting(setting) {
            push(field, name.to_string(), None, value, setting.range());
        }
    }
    for (index, cable) in modulator.targets.iter().enumerate() {
        push(CrossModField::Depth(index), format!("{} depth", cable.param_name), Some(cable.edge), cable.depth, (DEPTH_MIN, DEPTH_MAX));
    }
}

/// How a rack addresses the field `field` of its own modulator at `mod_index`: the other way round of `sibling_field`.
fn sibling_kind(mod_index: usize, field: CrossModField) -> ModTargetKind {
    match field {
        CrossModField::Rate => ModTargetKind::ModulatorRate { mod_index },
        CrossModField::Attack => ModTargetKind::ModulatorAttack { mod_index },
        CrossModField::Decay => ModTargetKind::ModulatorDecay { mod_index },
        CrossModField::Sustain => ModTargetKind::ModulatorSustain { mod_index },
        CrossModField::Release => ModTargetKind::ModulatorRelease { mod_index },
        CrossModField::Depth(target_index) => ModTargetKind::ModulatorDepth { mod_index, target_index },
    }
}

/// The parameters of every plugin in `lane`'s chain, the instrument first. `kind` makes the
/// kind of target of a parameter from its plugin's slot and its own index.
fn push_chain(targets: &mut Vec<ParamTarget>, lane: &InstrumentNode, kind: impl Fn(usize, u32) -> ModTargetKind) {
    for (slot, plugin) in std::iter::once(lane.instrument.as_ref()).chain(lane.effects.iter().map(Some)).enumerate() {
        if let Some(plugin) = plugin {
            push_params(targets, plugin, slot, |param_index| kind(slot, param_index));
        }
    }
}

/// The parameters of `plugin`, which is in chain slot `slot` (a separator is a row of a pane,
/// not a parameter, and is left out).
fn push_params(targets: &mut Vec<ParamTarget>, plugin: &PluginSlot, slot: usize, kind: impl Fn(u32) -> ModTargetKind) {
    for row in plugin.params.iter().filter(|row| !matches!(row.kind, ParamKind::Separator)) {
        targets.push(ParamTarget {
            node: plugin.node,
            param: row.index,
            kind: kind(row.index),
            slot,
            plugin_name: plugin.name.clone(),
            param_name: row.name.clone(),
            base_value: row.value,
            param_min: row.min,
            param_max: row.max,
        });
    }
}

// ---------------------------------------------------------------------------
// Plugin ids
// ---------------------------------------------------------------------------

pub fn format_from_id(id: &str) -> String {
    if id.starts_with("builtin:") {
        "Built-in".into()
    } else if id.starts_with("lv2:") || id.starts_with("http://") || id.starts_with("urn:") {
        "LV2".into()
    } else if id.starts_with("clap:") || id.contains('.') && !id.contains('/') {
        "CLAP".into()
    } else if id.starts_with("vst3:") {
        "VST3".into()
    } else if id.ends_with(".lv2") {
        "LV2".into()
    } else if id.ends_with(".clap") {
        "CLAP".into()
    } else if id.ends_with(".vst3") {
        "VST3".into()
    } else {
        "?".into()
    }
}

// ---------------------------------------------------------------------------
// Modulator rows: how a modulator's source and targets map to parameter rows
// ---------------------------------------------------------------------------

/// Depth range for a modulator target. Negative depth **inverts** the
/// modulation direction — e.g. a control that lowers the parameter instead of
/// raising it, an LFO wobbling in anti-phase, or an envelope that dips below
/// the base. The audio-side math is a plain multiply, so this is only the
/// editor's bound; sessions already round-trip negative depth.
pub const DEPTH_MIN: f32 = -1.0;
pub const DEPTH_MAX: f32 = 1.0;
/// The range of the value of a step of a step sequencer: below and above what the user set, by as much as the depth of the cable says.
pub const STEP_MIN: f32 = -1.0;
pub const STEP_MAX: f32 = 1.0;
/// The longest an LFO's fade can be, in seconds.
pub const MAX_FADE: f32 = 5.0;
/// The note lengths that a cycle of an LFO, or a step of a step sequencer, can be tied to the tempo by (`ModulatorField::Sync`): each with the name that a session file and the screens give it, and
/// how many beats of the tempo it is, a beat being a quarter note and a bar four beats (a session has no time signature: the four beats of a pattern are a bar). They go from the slowest note
/// to the fastest, and a note's dotted length (half as long again), its own and its triplet (two thirds of it) are together, as a plugin lists them, so that a step along the list goes to the
/// next note and not always to a faster one. The place of one in this list, counted from 1, is what a modulator's `sync` is; 0 is none, and the modulator goes at its own rate.
pub const SYNC_DIVISIONS: [(&str, f32); 16] = [
    ("4 bars", 16.0),
    ("2 bars", 8.0),
    ("1 bar", 4.0),
    ("1/2 dotted", 3.0),
    ("1/2", 2.0),
    ("1/2 triplet", 4.0 / 3.0),
    ("1/4 dotted", 1.5),
    ("1/4", 1.0),
    ("1/4 triplet", 2.0 / 3.0),
    ("1/8 dotted", 0.75),
    ("1/8", 0.5),
    ("1/8 triplet", 1.0 / 3.0),
    ("1/16 dotted", 0.375),
    ("1/16", 0.25),
    ("1/16 triplet", 1.0 / 6.0),
    ("1/32", 0.125),
];

/// What a modulator that does not follow the tempo (a `sync` of 0) is called.
pub const FREE: &str = "free";

/// The name of a `sync`: that of its note length, or `free` for none (and for a place the list has not).
pub fn sync_name(sync: usize) -> &'static str {
    sync.checked_sub(1).and_then(|place| SYNC_DIVISIONS.get(place)).map_or(FREE, |&(name, _)| name)
}

/// How many beats of the tempo a cycle or a step is for a `sync`: 0 for none (and for a place the list has not).
pub fn sync_beats(sync: usize) -> f32 {
    sync.checked_sub(1).and_then(|place| SYNC_DIVISIONS.get(place)).map_or(0.0, |&(_, beats)| beats)
}

/// The `sync` that `name` says, whatever its case and the space round it and in it: `free` (or `off`, `none`, or nothing at all) is 0, and a note length is its place in `SYNC_DIVISIONS`.
/// `None` for any other.
pub fn sync_from_name(name: &str) -> Option<usize> {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    if matches!(name.as_str(), "" | FREE | "off" | "none") {
        return Some(0);
    }
    SYNC_DIVISIONS.iter().position(|&(known, _)| known == name).map(|place| place + 1)
}

/// How deep a cable pulls when it is first drawn: half way, which a person can hear and turn either way.
pub const NEW_CABLE_DEPTH: f32 = 0.5;
/// How many parameters of a plugin can be pinned to its node: a node that shows more is taller than a canvas has room for, and a plugin of
/// hundreds has its other parameters in the panel.
pub const MAX_PINNED: usize = 64;
/// How long a group's name can be, in characters: a node shows it in a header, and a name that fills a screen is no name.
pub const MAX_NAME: usize = 64;

/// What a row of a modulator's parameter pane is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModulatorRow {
    /// The kind of source, which an LFO and an envelope switch between.
    Type,
    /// A MIDI control's source: shown, not edited.
    Info,
    /// An LFO's waveform, stepped through the four.
    Waveform,
    /// A number of the source: an LFO's rate, a stage of an envelope.
    Setting(ModulatorField),
    /// The "Targets" separator.
    Separator,
    /// The switch for whether a note starts a step sequencer or an LFO again: a choice of two.
    Retrigger,
    /// Whether the rate follows the tempo, and by what note length: a choice of `free` and the note lengths of `SYNC_DIVISIONS`.
    Sync,
    /// The value of the step sequencer's step with this index.
    Step(usize),
    /// The depth of the target with this index.
    Depth(usize),
}

/// The rows of the pane of a step sequencer before its targets': its settings and its choices, its sixteen steps, and the separator.
const STEPS_ROWS: [ModulatorRow; 5 + crate::plugin::chain::MAX_STEPS + 1] = {
    use ModulatorField as F;
    use ModulatorRow as R;
    let mut rows = [R::Separator; 5 + crate::plugin::chain::MAX_STEPS + 1];
    rows[0] = R::Setting(F::Rate);
    rows[1] = R::Sync;
    rows[2] = R::Setting(F::Glide);
    rows[3] = R::Setting(F::Length);
    rows[4] = R::Retrigger;
    let mut step = 0;
    while step < crate::plugin::chain::MAX_STEPS {
        rows[5 + step] = R::Step(step);
        step += 1;
    }
    rows
};

impl ModSourceSlot {
    /// The rows of the pane before the targets' (which follow the separator).
    fn rows(&self) -> &'static [ModulatorRow] {
        use ModulatorField as F;
        use ModulatorRow as R;
        match self {
            ModSourceSlot::Lfo { .. } => &[R::Type, R::Waveform, R::Setting(F::Rate), R::Sync, R::Retrigger, R::Setting(F::Fade), R::Separator],
            ModSourceSlot::Envelope { .. } => {
                &[R::Type, R::Setting(F::Attack), R::Setting(F::Decay), R::Setting(F::Sustain), R::Setting(F::Release), R::Separator]
            }
            ModSourceSlot::MidiLearn { .. } => &[R::Info, R::Separator],
            ModSourceSlot::Macro { .. } => &[R::Setting(F::Value), R::Separator],
            ModSourceSlot::Steps { .. } => &STEPS_ROWS,
        }
    }

    /// The value of a setting, if the source has it (a waveform is its index).
    pub fn setting(&self, field: ModulatorField) -> Option<f32> {
        match (self, field) {
            (ModSourceSlot::Lfo { rate, .. }, ModulatorField::Rate) => Some(*rate),
            (ModSourceSlot::Lfo { waveform, .. }, ModulatorField::Waveform) => Some(waveform.to_index() as f32),
            (ModSourceSlot::Lfo { retrigger, .. }, ModulatorField::Retrigger) => Some(if *retrigger { 1.0 } else { 0.0 }),
            (ModSourceSlot::Lfo { fade, .. }, ModulatorField::Fade) => Some(*fade),
            (ModSourceSlot::Lfo { sync, .. } | ModSourceSlot::Steps { sync, .. }, ModulatorField::Sync) => Some(*sync as f32),
            (ModSourceSlot::Envelope { attack, .. }, ModulatorField::Attack) => Some(*attack),
            (ModSourceSlot::Envelope { decay, .. }, ModulatorField::Decay) => Some(*decay),
            (ModSourceSlot::Envelope { sustain, .. }, ModulatorField::Sustain) => Some(*sustain),
            (ModSourceSlot::Envelope { release, .. }, ModulatorField::Release) => Some(*release),
            (ModSourceSlot::Macro { value }, ModulatorField::Value) => Some(*value),
            (ModSourceSlot::Steps { rate, .. }, ModulatorField::Rate) => Some(*rate),
            (ModSourceSlot::Steps { glide, .. }, ModulatorField::Glide) => Some(*glide),
            (ModSourceSlot::Steps { length, .. }, ModulatorField::Length) => Some(*length as f32),
            (ModSourceSlot::Steps { retrigger, .. }, ModulatorField::Retrigger) => Some(if *retrigger { 1.0 } else { 0.0 }),
            _ => None,
        }
    }
}

/// Number of param-pane rows for a modulator: the rows of its source, the "Targets"
/// separator among them, and one row a target. Scope-independent (lane or group).
pub fn modulator_param_len(m: &ModulatorSlot) -> usize {
    m.source.rows().len() + m.targets.len()
}

/// What row `row` of the pane of `m` is; `None` past the last. Every other question
/// about a row (its range, whether it is an enum, what it is bound to by MIDI learn,
/// which intent it makes) is answered from this.
pub fn modulator_row(m: &ModulatorSlot, row: usize) -> Option<ModulatorRow> {
    let fixed = m.source.rows();
    match fixed.get(row) {
        Some(&found) => Some(found),
        None => {
            let target = row - fixed.len();
            (target < m.targets.len()).then_some(ModulatorRow::Depth(target))
        }
    }
}

/// (min, max) for a modulator pseudo-param at row `pa`, or None for enum /
/// separator rows. Scope-independent.
pub fn modulator_param_range(m: &ModulatorSlot, pa: usize) -> Option<(f32, f32)> {
    match modulator_row(m, pa)? {
        ModulatorRow::Setting(field) => Some(field.range()),
        ModulatorRow::Step(_) => Some((STEP_MIN, STEP_MAX)),
        ModulatorRow::Depth(_) => Some((DEPTH_MIN, DEPTH_MAX)),
        ModulatorRow::Type | ModulatorRow::Info | ModulatorRow::Waveform | ModulatorRow::Retrigger | ModulatorRow::Sync | ModulatorRow::Separator => None,
    }
}

/// True if a modulator pseudo-param at row `pa` is an enum (Type / Waveform / Retrigger / Sync).
pub fn modulator_param_is_enum(m: &ModulatorSlot, pa: usize) -> bool {
    matches!(modulator_row(m, pa), Some(ModulatorRow::Type | ModulatorRow::Waveform | ModulatorRow::Retrigger | ModulatorRow::Sync))
}

impl ModulatorSlot {
    /// The telemetry slots of a modulator that is made with `source`: the one it puts its output in and, for a step sequencer, the one that says where it is, in that
    /// order. Each is `None` if the allocator had run out of them: the modulator works all the same, and a page has nothing to draw from.
    pub fn alloc_slots(slots: &mut SlotAllocator, source: &ModSourceSlot) -> (Option<Slot>, Option<Slot>) {
        let output = slots.alloc();
        let position = if matches!(source, ModSourceSlot::Steps { .. }) { slots.alloc() } else { None };
        (output, position)
    }

    /// The intent that makes row `row` what `f` makes of what it is now: a setting of the
    /// source (an LFO's rate, a stage of an envelope) or the depth of a target. `None` for
    /// a row that is not a number (the type, the waveform, a separator, a control's source).
    pub fn row_intent(&self, row: usize, f: impl FnOnce(f32) -> f32) -> Option<Intent> {
        match modulator_row(self, row)? {
            ModulatorRow::Setting(field) => self.field_intent(field, f),
            ModulatorRow::Retrigger => self.field_intent(ModulatorField::Retrigger, f),
            ModulatorRow::Sync => self.field_intent(ModulatorField::Sync, f),
            ModulatorRow::Step(step) => {
                let mut values = self.source.steps()?;
                let value = values.get_mut(step)?;
                *value = f(*value);
                Some(Intent::SetSteps { node: self.node, values: values.to_vec() })
            }
            ModulatorRow::Depth(target) => {
                let target = self.targets.get(target)?;
                Some(Intent::SetEdgeDepth { edge: target.edge, depth: f(target.depth) })
            }
            ModulatorRow::Type | ModulatorRow::Info | ModulatorRow::Waveform | ModulatorRow::Separator => None,
        }
    }

    /// The intent that sets `field` to what `f` makes of it now, if the source has it.
    fn field_intent(&self, field: ModulatorField, f: impl FnOnce(f32) -> f32) -> Option<Intent> {
        Some(Intent::SetModulatorField { node: self.node, field, value: f(self.source.setting(field)?) })
    }
}

/// The commands that tell the audio thread where the modulator at `index` of the rack at `location` puts its output and, for a step sequencer, where it is: one for each
/// slot it has, the output's first.
pub fn modulator_slot_commands(location: ModulatorLocation, index: usize, output: Option<Slot>, position: Option<Slot>) -> Vec<plugin::chain::GraphCommand> {
    use plugin::chain::GraphCommand;
    let mut commands = Vec::new();
    if let Some(slot) = output {
        commands.push(GraphCommand::SetModulatorOutput { location, index, slot });
    }
    if let Some(slot) = position {
        commands.push(GraphCommand::SetModulatorPosition { location, index, slot });
    }
    commands
}

/// Map a modulator's param-pane row `pa` to a cross-mod target on that
/// modulator (index `mod_index`): its rate/ADSR field or one of its target
/// depths. Returns `(kind, min, max, base, label)`, or None for enum/separator
/// rows. Used by MIDI-learn to bind a control to a modulator's own parameter.
pub fn modulator_row_to_cross_mod(
    m: &ModulatorSlot,
    mod_index: usize,
    pa: usize,
) -> Option<(crate::plugin::chain::ModTargetKind, f32, f32, f32, String)> {
    use crate::plugin::chain::ModTargetKind as K;
    match modulator_row(m, pa)? {
        ModulatorRow::Setting(field) => {
            let (kind, name) = match field {
                ModulatorField::Rate => (K::ModulatorRate { mod_index }, "rate"),
                ModulatorField::Attack => (K::ModulatorAttack { mod_index }, "attack"),
                ModulatorField::Decay => (K::ModulatorDecay { mod_index }, "decay"),
                ModulatorField::Sustain => (K::ModulatorSustain { mod_index }, "sustain"),
                ModulatorField::Release => (K::ModulatorRelease { mod_index }, "release"),
                // (It has a row of its own, `ModulatorRow::Waveform`, which is not a number for a control to move.)
                ModulatorField::Waveform => return None,
                // (A macro's value is turned by a person: a cable that drives it is not built. Nor is a step sequencer's glide, length or switch.)
                ModulatorField::Value | ModulatorField::Glide | ModulatorField::Length | ModulatorField::Retrigger | ModulatorField::Fade | ModulatorField::Sync => return None,
            };
            let (min, max) = field.range();
            Some((kind, min, max, m.source.setting(field)?, format!("Mod {mod_index} {name}")))
        }
        ModulatorRow::Depth(target) => {
            let depth = m.targets.get(target)?.depth;
            Some((K::ModulatorDepth { mod_index, target_index: target }, DEPTH_MIN, DEPTH_MAX, depth, format!("Mod {mod_index} depth {target}")))
        }
        ModulatorRow::Type | ModulatorRow::Info | ModulatorRow::Waveform | ModulatorRow::Retrigger | ModulatorRow::Sync | ModulatorRow::Step(_) | ModulatorRow::Separator => None,
    }
}

// ---------------------------------------------------------------------------
// Index fix-ups and audio-graph conversion
// ---------------------------------------------------------------------------

/// After removing a modulator at `removed_index` from the session model,
/// clean up cross-mod targets in siblings: remove targets pointing at the
/// removed index, and decrement indices > removed_index.
pub fn fixup_cross_mod_after_remove(modulators: &mut [ModulatorSlot], removed_index: usize) {
    use crate::plugin::chain::ModTargetKind;
    for m in modulators.iter_mut() {
        m.targets.retain(|t| {
            let idx = match &t.kind {
                ModTargetKind::PluginParam { .. }
                | ModTargetKind::GroupMember { .. }
                | ModTargetKind::GroupBus { .. }
                | ModTargetKind::GroupMemberMod { .. } => None,
                ModTargetKind::ModulatorRate { mod_index }
                | ModTargetKind::ModulatorAttack { mod_index }
                | ModTargetKind::ModulatorDecay { mod_index }
                | ModTargetKind::ModulatorSustain { mod_index }
                | ModTargetKind::ModulatorRelease { mod_index }
                | ModTargetKind::ModulatorDepth { mod_index, .. } => Some(*mod_index),
            };
            idx != Some(removed_index)
        });
        for t in &mut m.targets {
            let idx = match &mut t.kind {
                ModTargetKind::PluginParam { .. }
                | ModTargetKind::GroupMember { .. }
                | ModTargetKind::GroupBus { .. }
                | ModTargetKind::GroupMemberMod { .. } => continue,
                ModTargetKind::ModulatorRate { mod_index }
                | ModTargetKind::ModulatorAttack { mod_index }
                | ModTargetKind::ModulatorDecay { mod_index }
                | ModTargetKind::ModulatorSustain { mod_index }
                | ModTargetKind::ModulatorRelease { mod_index }
                | ModTargetKind::ModulatorDepth { mod_index, .. } => mod_index,
            };
            if *idx > removed_index {
                *idx -= 1;
            }
        }
    }
}

/// Mirror of the audio thread's `shift_group_bus_after_insert`: after a bus effect is put in at `at`, bump the `GroupBus` targets that pointed at that
/// effect or a later one, so they keep pointing at the same effects (the inverse of `fixup_group_bus_after_remove`).
pub fn shift_group_bus_after_insert(modulators: &mut [ModulatorSlot], at: usize) {
    use crate::plugin::chain::ModTargetKind;
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::GroupBus { effect_index, .. } = &mut t.kind {
                if *effect_index >= at {
                    *effect_index += 1;
                }
            }
        }
    }
}

/// Mirror of the audio-thread `fixup_group_bus_after_remove` for the session model:
/// after a group bus effect at `removed` is deleted, drop GroupBus targets on
/// it and shift higher bus indices down.
pub fn fixup_group_bus_after_remove(modulators: &mut [ModulatorSlot], removed: usize) {
    use crate::plugin::chain::ModTargetKind;
    for m in modulators.iter_mut() {
        m.targets.retain(|t| {
            !matches!(t.kind, ModTargetKind::GroupBus { effect_index, .. } if effect_index == removed)
        });
        for t in &mut m.targets {
            if let ModTargetKind::GroupBus { effect_index, .. } = &mut t.kind {
                if *effect_index > removed {
                    *effect_index -= 1;
                }
            }
        }
    }
}

/// Session-model mirror of the audio thread's `fixup_depth_cables_after_remove`: after the cable
/// at `removed` is taken out of the modulator at `mod_index`, the cables of the modulators in
/// the rack that drive the depth of a later one of its cables are told its new place.
fn fixup_depth_cables_after_remove(modulators: &mut [ModulatorSlot], mod_index: usize, removed: usize) {
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::ModulatorDepth { mod_index: of, target_index } = &mut t.kind {
                if *of == mod_index && *target_index > removed {
                    *target_index -= 1;
                }
            }
        }
    }
}

/// The same for a group's cables to the depth of a cable of member `member`'s modulator
/// `mod_index` (`fixup_member_depth_cables_after_remove` of the audio thread).
fn fixup_member_depth_cables_after_remove(modulators: &mut [ModulatorSlot], member: usize, mod_index: usize, removed: usize) {
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::GroupMemberMod { member: of, mod_index: at, field: CrossModField::Depth(target) } = &mut t.kind {
                if *of == member && *at == mod_index && *target > removed {
                    *target -= 1;
                }
            }
        }
    }
}

/// The inverse of `fixup_cross_mod_after_remove`, mirror of the audio thread's `shift_cross_mod_after_insert`: after a modulator goes into a rack at
/// `inserted`, the cross-mod cables of its siblings that address a modulator at that place or after it are told the place that modulator has now.
fn shift_cross_mod_after_insert(modulators: &mut [ModulatorSlot], inserted: usize) {
    use crate::plugin::chain::ModTargetKind;
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            let idx = match &mut t.kind {
                ModTargetKind::PluginParam { .. }
                | ModTargetKind::GroupMember { .. }
                | ModTargetKind::GroupBus { .. }
                | ModTargetKind::GroupMemberMod { .. } => continue,
                ModTargetKind::ModulatorRate { mod_index }
                | ModTargetKind::ModulatorAttack { mod_index }
                | ModTargetKind::ModulatorDecay { mod_index }
                | ModTargetKind::ModulatorSustain { mod_index }
                | ModTargetKind::ModulatorRelease { mod_index }
                | ModTargetKind::ModulatorDepth { mod_index, .. } => mod_index,
            };
            if *idx >= inserted {
                *idx += 1;
            }
        }
    }
}

/// The inverse of `fixup_group_member_mod_after_remove`, mirror of the audio thread's `shift_group_member_mod_after_insert`: after member `member`'s lane
/// modulator goes in at `inserted`, the group's cables to that member's modulators at that place or after it are told the place they have now.
fn shift_group_member_mod_after_insert(modulators: &mut [ModulatorSlot], member: usize, inserted: usize) {
    use crate::plugin::chain::ModTargetKind;
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::GroupMemberMod { member: tm, mod_index, .. } = &mut t.kind {
                if *tm == member && *mod_index >= inserted {
                    *mod_index += 1;
                }
            }
        }
    }
}

/// The inverse of `fixup_depth_cables_after_remove`, mirror of the audio thread's `shift_depth_cables_after_insert`: after a cable goes into the
/// modulator at `mod_index` at place `inserted`, the cables of the modulators in the rack that drive the depth of a cable of it at that place or
/// after it are told the place that cable has now. (Before the cable goes in: its own kind is what it says, and is not moved with the others.)
fn shift_depth_cables_after_insert(modulators: &mut [ModulatorSlot], mod_index: usize, inserted: usize) {
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::ModulatorDepth { mod_index: of, target_index } = &mut t.kind {
                if *of == mod_index && *target_index >= inserted {
                    *target_index += 1;
                }
            }
        }
    }
}

/// The same for a group's cables to the depth of a cable of member `member`'s modulator `mod_index` (`shift_member_depth_cables_after_insert`
/// of the audio thread).
fn shift_member_depth_cables_after_insert(modulators: &mut [ModulatorSlot], member: usize, mod_index: usize, inserted: usize) {
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::GroupMemberMod { member: of, mod_index: at, field: CrossModField::Depth(target) } = &mut t.kind {
                if *of == member && *at == mod_index && *target >= inserted {
                    *target += 1;
                }
            }
        }
    }
}

/// Session-model mirror of `fixup_group_member_after_remove`: after the member at
/// ordinal `removed` leaves a group, drop GroupMember targets on it and shift
/// higher ordinals down.
pub fn fixup_group_member_after_remove(modulators: &mut [ModulatorSlot], removed: usize) {
    use crate::plugin::chain::ModTargetKind;
    for m in modulators.iter_mut() {
        m.targets.retain(|t| match &t.kind {
            ModTargetKind::GroupMember { member, .. }
            | ModTargetKind::GroupMemberMod { member, .. } => *member != removed,
            _ => true,
        });
        for t in &mut m.targets {
            let member = match &mut t.kind {
                ModTargetKind::GroupMember { member, .. }
                | ModTargetKind::GroupMemberMod { member, .. } => member,
                _ => continue,
            };
            if *member > removed {
                *member -= 1;
            }
        }
    }
}

/// Session-model mirror of `shift_group_member_after_insert`: after a member joins
/// a group at ordinal `inserted`, bump group-target member ordinals at/after it.
pub fn shift_group_member_after_insert(modulators: &mut [ModulatorSlot], inserted: usize) {
    use crate::plugin::chain::ModTargetKind;
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            let member = match &mut t.kind {
                ModTargetKind::GroupMember { member, .. }
                | ModTargetKind::GroupMemberMod { member, .. } => member,
                _ => continue,
            };
            if *member >= inserted {
                *member += 1;
            }
        }
    }
}

/// Session-model mirror of `fixup_group_member_mod_after_remove`: after member
/// `member`'s lane modulator at `removed` is deleted, drop GroupMemberMod
/// targets on it and shift that member's higher mod-indices down.
pub fn fixup_group_member_mod_after_remove(
    modulators: &mut [ModulatorSlot],
    member: usize,
    removed: usize,
) {
    use crate::plugin::chain::ModTargetKind;
    for m in modulators.iter_mut() {
        m.targets.retain(|t| {
            !matches!(t.kind, ModTargetKind::GroupMemberMod { member: tm, mod_index, .. } if tm == member && mod_index == removed)
        });
        for t in &mut m.targets {
            if let ModTargetKind::GroupMemberMod { member: tm, mod_index, .. } = &mut t.kind {
                if *tm == member && *mod_index > removed {
                    *mod_index -= 1;
                }
            }
        }
    }
}

/// Where index `i` lands when the element at `from` moves to `to` and the
/// elements in between slide over by one.
fn remap_index(i: usize, from: usize, to: usize) -> usize {
    if i == from {
        to
    } else if from < to && i > from && i <= to {
        i - 1
    } else if from > to && i >= to && i < from {
        i + 1
    } else {
        i
    }
}

/// Session-model mirror of the audio thread's `fixup_slots_after_remove`: after
/// the effect at index `removed` is deleted from a lane's chain, drop the
/// plugin-param targets that pointed at it and shift higher slots down one, so
/// every remaining target still points at the same plugin. Slot 0 (the
/// instrument) never moves.
pub fn fixup_slots_after_remove(modulators: &mut [ModulatorSlot], removed: usize) {
    use crate::plugin::chain::ModTargetKind;
    let removed_slot = removed + 1;
    for m in modulators.iter_mut() {
        m.targets.retain(|t| {
            !matches!(&t.kind, ModTargetKind::PluginParam { slot, .. } if *slot == removed_slot)
        });
        for t in &mut m.targets {
            if let ModTargetKind::PluginParam { slot, .. } = &mut t.kind {
                if *slot > removed_slot {
                    *slot -= 1;
                    t.slot = *slot;
                }
            }
        }
    }
}

/// Session-model mirror of the audio thread's `shift_slots_after_insert`: after an effect is put in a lane's chain at effect index `at`, bump the
/// plugin-param targets at the slots of that effect and the ones after it (a slot is an effect index and one more), so every target still points at the same
/// plugin (the inverse of `fixup_slots_after_remove`). Slot 0, the instrument, never moves.
pub fn shift_slots_after_insert(modulators: &mut [ModulatorSlot], at: usize) {
    use crate::plugin::chain::ModTargetKind;
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::PluginParam { slot, .. } = &mut t.kind {
                if *slot > at {
                    *slot += 1;
                    t.slot = *slot;
                }
            }
        }
    }
}

/// Session-model mirror of `remap_slots_after_reorder`: after the effect at
/// `from` moves to `to`, remap plugin-param target slots through the same
/// permutation so they follow their plugins.
pub fn remap_slots_after_reorder(modulators: &mut [ModulatorSlot], from: usize, to: usize) {
    use crate::plugin::chain::ModTargetKind;
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::PluginParam { slot, .. } = &mut t.kind {
                if *slot >= 1 {
                    *slot = remap_index(*slot - 1, from, to) + 1;
                    t.slot = *slot;
                }
            }
        }
    }
}

/// Session-model mirror of `fixup_member_slots_after_remove`: after the effect at
/// index `removed` is deleted from member `member`'s lane, drop the group targets
/// that pointed at it and shift that member's higher slots down one, so every
/// remaining target still points at the same plugin. Other members' targets and
/// slot 0 (the instrument) never move.
pub fn fixup_member_slots_after_remove(modulators: &mut [ModulatorSlot], member: usize, removed: usize) {
    use crate::plugin::chain::ModTargetKind;
    let removed_slot = removed + 1;
    for m in modulators.iter_mut() {
        m.targets.retain(|t| {
            !matches!(&t.kind, ModTargetKind::GroupMember { member: tm, slot, .. }
                if *tm == member && *slot == removed_slot)
        });
        for t in &mut m.targets {
            if let ModTargetKind::GroupMember { member: tm, slot, .. } = &mut t.kind {
                if *tm == member && *slot > removed_slot {
                    *slot -= 1;
                    t.slot = *slot;
                }
            }
        }
    }
}

/// Session-model mirror of the audio thread's `shift_member_slots_after_insert`: after an effect is put in member `member`'s chain at effect index `at`, bump
/// that member's group targets at the slots of that effect and the ones after it (the inverse of `fixup_member_slots_after_remove`). Other members' targets
/// and slot 0, the instrument, never move.
pub fn shift_member_slots_after_insert(modulators: &mut [ModulatorSlot], member: usize, at: usize) {
    use crate::plugin::chain::ModTargetKind;
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::GroupMember { member: tm, slot, .. } = &mut t.kind {
                if *tm == member && *slot > at {
                    *slot += 1;
                    t.slot = *slot;
                }
            }
        }
    }
}

/// Session-model mirror of `remap_member_slots_after_reorder`: after the effect at
/// `from` moves to `to` in member `member`'s lane, remap that member's group
/// target slots through the same permutation so they follow their plugins.
pub fn remap_member_slots_after_reorder(modulators: &mut [ModulatorSlot], member: usize, from: usize, to: usize) {
    use crate::plugin::chain::ModTargetKind;
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::GroupMember { member: tm, slot, .. } = &mut t.kind {
                if *tm == member && *slot >= 1 {
                    *slot = remap_index(*slot - 1, from, to) + 1;
                    t.slot = *slot;
                }
            }
        }
    }
}

/// Session-model mirror of `remap_group_bus_after_reorder`: after bus effect
/// `from` moves to `to`, remap `GroupBus` effect indices through the same
/// permutation so targets follow their effects.
pub fn remap_group_bus_after_reorder(modulators: &mut [ModulatorSlot], from: usize, to: usize) {
    use crate::plugin::chain::ModTargetKind;
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::GroupBus { effect_index, .. } = &mut t.kind {
                *effect_index = remap_index(*effect_index, from, to);
            }
        }
    }
}

/// The (group, member-ordinal) of instrument `inst`, or None if ungrouped.
pub fn lane_member_ordinal(instruments: &[InstrumentNode], inst: usize) -> Option<(usize, usize)> {
    let group = instruments.get(inst)?.group?;
    let ordinal = instruments[..inst]
        .iter()
        .filter(|n| n.group == Some(group))
        .count();
    Some((group, ordinal))
}

/// Convert a `ModSourceSlot` to the audio-thread `ModSource` carried by `GraphCommand`s.
pub fn mod_source_slot_to_graph(slot: &ModSourceSlot) -> crate::plugin::chain::ModSource {
    match slot {
        ModSourceSlot::Lfo { waveform, rate, retrigger, fade, sync } => {
            crate::plugin::chain::ModSource::lfo(*waveform, *rate).with_key(*retrigger, *fade).with_sync(sync_beats(*sync))
        }
        ModSourceSlot::Envelope { attack, decay, sustain, release } => crate::plugin::chain::ModSource::Envelope {
            attack: *attack,
            decay: *decay,
            sustain: *sustain,
            release: *release,
            state: crate::plugin::chain::EnvState::Idle,
            level: 0.0,
            notes_held: 0,
        },
        ModSourceSlot::MidiLearn { source, learning } => crate::plugin::chain::ModSource::MidiLearn {
            source: *source,
            // (Where the control is before it sends anything: the wheel's centre is not 0.)
            value: source.map_or(0.0, crate::plugin::chain::MidiBindSource::rest),
            learning: *learning,
        },
        ModSourceSlot::Macro { value } => crate::plugin::chain::ModSource::Macro { value: *value },
        ModSourceSlot::Steps { values, length, rate, glide, retrigger, sync } => crate::plugin::chain::ModSource::Steps {
            values: *values,
            length: *length,
            rate: *rate,
            glide: *glide,
            retrigger: *retrigger,
            sync: sync_beats(*sync),
            position: 0.0,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::{LONG_CHAINS, SENDS, Sends, load};
    use crate::plugin::chain::{CrossModField, LfoWaveform, MidiBindSource, ModSource, ModTargetKind as K};

    #[test]
    fn every_node_is_found_by_its_id() {
        use crate::app::testutil::GROUPS;
        let model = load(GROUPS).model;
        model.check_ids().unwrap();

        let nodes: Vec<(NodeId, NodePath)> = model.nodes().collect();
        let expected = model.groups.len()
            + model.groups.iter().map(|g| g.effects.len() + g.modulators.len()).sum::<usize>()
            + model.instruments.len()
            + model.instruments.iter().map(|l| l.effects.len() + l.modulators.len()).sum::<usize>();
        assert_eq!(nodes.len(), expected, "one node per group, lane, effect, bus effect and modulator");
        for (id, path) in &nodes {
            assert_eq!(model.locate(*id), Some(*path), "{id:?}");
        }
        let has = |f: fn(&NodePath) -> bool| nodes.iter().any(|(_, p)| f(p));
        assert!(has(|p| matches!(p, NodePath::Instrument(_))));
        assert!(has(|p| matches!(p, NodePath::Effect { .. })));
        assert!(has(|p| matches!(p, NodePath::Group(_))));
        assert!(has(|p| matches!(p, NodePath::GroupEffect { .. })));
        assert!(has(|p| matches!(p, NodePath::Modulator { at: ModulatorLocation::Lane(_), .. })));
        assert!(has(|p| matches!(p, NodePath::Modulator { at: ModulatorLocation::Group(_), .. })));

        assert_eq!(model.locate(NodeId(9_999)), None, "an id nothing has");
    }

    #[test]
    fn a_lanes_instrument_sits_in_the_lanes_node() {
        let model = load("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n").model;
        let lane = &model.instruments[0];
        assert_eq!(lane.instrument.as_ref().unwrap().node, lane.node);
        assert_ne!(lane.effects[0].node, lane.node, "an effect is a node of its own");
    }

    #[test]
    fn ids_are_handed_out_once() {
        let mut ids = NodeIds::default();
        let a = ids.alloc();
        let b = ids.alloc();
        assert_ne!(a, b);
        let mut model = SessionModel::default();
        let first = model.alloc_node();
        assert_ne!(model.alloc_node(), first);
    }

    #[test]
    fn a_new_model_is_at_120_beats_a_minute() {
        assert_eq!(SessionModel::default().bpm, 120.0);
    }

    #[test]
    fn the_tempo_is_saved_only_when_a_load_would_not_come_to_it() {
        let pattern = "[[instrument]]\nplugin = \"builtin:osc\"\n[instrument.pattern]\nbpm = 90.0\nlength_beats = 4.0\nenabled = true\n";
        let mut plain = load("[[instrument]]\nplugin = \"builtin:osc\"\n").model;
        assert_eq!(plain.bpm_to_save(), None, "120 is what a session with no pattern is at");
        plain.bpm = 100.0;
        assert_eq!(plain.bpm_to_save(), Some(100.0));

        let mut patterned = load(pattern).model;
        assert_eq!(patterned.bpm_to_save(), None, "90 is the first pattern's");
        patterned.bpm = 120.0;
        assert_eq!(patterned.bpm_to_save(), Some(120.0), "(a 120 that a load would not come to is written)");
        patterned.bpm = 90.0;
        patterned.instruments[0].pattern = None;
        assert_eq!(patterned.bpm_to_save(), Some(90.0), "without the pattern a load would be at 120");
    }

    #[test]
    fn the_tempo_of_patterns_is_that_of_the_first_and_held_to_the_tempos_there_can_be() {
        let lane = |bpm: &str| format!("[[instrument]]\nplugin = \"builtin:osc\"\n[instrument.pattern]\nbpm = {bpm}\nlength_beats = 4.0\nenabled = true\n");
        let tempo = |text: &str| tempo_of_patterns(&load(text).model.instruments);
        assert_eq!(tempo(&format!("[[instrument]]\nplugin = \"builtin:osc\"\n{}{}", lane("140.0"), lane("90.0"))), 140.0, "the first lane that has a pattern decides");
        assert_eq!(tempo(&lane("5.0")), 20.0);
        assert_eq!(tempo(&lane("1000.0")), 300.0);
        assert_eq!(tempo("[[instrument]]\nplugin = \"builtin:osc\"\n"), 120.0);
    }

    #[test]
    fn a_group_is_called_by_its_name_or_else_by_its_number_counting_from_one() {
        let mut model = load("[[group]]\nname = \"Pad\"\n[[group]]\n[[group]]\nname = \"\"\n").model;
        let names: Vec<String> = model.groups.iter().enumerate().map(|(g, group)| group.display_name(g)).collect();
        assert_eq!(names, ["Pad", "Group 2", "Group 3"]);
        model.groups[0].name = None;
        assert_eq!(model.groups[0].display_name(0), "Group 1");
        assert_eq!(model.groups[0].display_name(6), "Group 7", "it is the index it is asked about");
    }

    #[test]
    fn edge_ids_are_a_series_of_their_own() {
        let mut model = SessionModel::default();
        let (n0, e0, n1, e1) = (model.alloc_node(), model.alloc_edge(), model.alloc_node(), model.alloc_edge());
        assert_eq!((n0, n1), (NodeId(0), NodeId(1)), "edges do not use up node ids");
        assert_eq!((e0, e1), (EdgeId(0), EdgeId(1)));
    }

    #[test]
    fn ids_that_are_to_be_put_back_come_first_in_order_and_new_ones_go_on_where_they_were() {
        let mut ids = NodeIds::default();
        let (a, b, c) = (ids.alloc(), ids.alloc(), ids.alloc());
        let (x, y) = (ids.alloc_edge(), ids.alloc_edge());
        // What was taken out comes back under its own ids, in the order it is made; what is made after it is new.
        ids.reuse(&[b, a], &[y]);
        assert_eq!((ids.alloc(), ids.alloc()), (b, a));
        assert_eq!(ids.alloc_edge(), y);
        assert_eq!(ids.alloc(), NodeId(c.0 + 1), "the next new id is the one after the last there was");
        assert_eq!(ids.alloc_edge(), EdgeId(y.0 + 1));
        assert_eq!(x, EdgeId(0));
    }

    #[test]
    fn ids_to_put_back_that_are_not_used_can_be_dropped_and_are_not_given_to_what_is_made_next() {
        let mut ids = NodeIds::default();
        let a = ids.alloc();
        ids.reuse(&[a], &[]);
        ids.drop_ids_to_put_back();
        assert_ne!(ids.alloc(), a, "an id that nothing asked for is not given again");
        ids.reuse(&[], &[EdgeId(7)]);
        ids.drop_ids_to_put_back();
        assert_ne!(ids.alloc_edge(), EdgeId(7), "an edge's id is dropped as a node's is");
    }

    #[test]
    fn edges_that_are_put_back_come_in_the_order_they_were_said() {
        let mut ids = NodeIds::default();
        let (x, y) = (ids.alloc_edge(), ids.alloc_edge());
        ids.reuse(&[], &[y, x]);
        assert_eq!((ids.alloc_edge(), ids.alloc_edge()), (y, x));
    }

    #[test]
    fn a_log_that_is_begun_again_starts_empty() {
        let mut ids = NodeIds::default();
        ids.begin_log();
        ids.alloc();
        ids.begin_log();
        assert_eq!(ids.take_log(), Made::default());
    }

    #[test]
    fn what_is_to_be_put_back_is_what_was_last_said_and_not_what_was_said_before() {
        let mut ids = NodeIds::default();
        let (a, b) = (ids.alloc(), ids.alloc());
        let (x, y) = (ids.alloc_edge(), ids.alloc_edge());
        ids.reuse(&[a], &[x]);
        ids.reuse(&[b], &[y]);
        assert_eq!((ids.alloc(), ids.alloc_edge()), (b, y));
        assert_eq!((ids.alloc(), ids.alloc_edge()), (NodeId(b.0 + 1), EdgeId(y.0 + 1)), "and what was said first is not left to come after it");
    }

    #[test]
    fn the_ids_handed_out_since_the_log_was_begun_are_told_once_in_order() {
        let mut ids = NodeIds::default();
        ids.alloc(); // (before the log: not in it)
        ids.begin_log();
        let (n, e, m) = (ids.alloc(), ids.alloc_edge(), ids.alloc());
        assert_eq!(ids.take_log(), Made { nodes: vec![n, m], edges: vec![e] });
        ids.alloc();
        assert_eq!(ids.take_log(), Made::default(), "the log ended with the take: nothing handed out since is in it");
    }

    #[test]
    fn an_id_that_is_put_back_is_in_the_log_as_any_is_and_in_its_turn() {
        let mut ids = NodeIds::default();
        let a = ids.alloc();
        ids.begin_log();
        ids.reuse(&[a], &[]);
        ids.alloc();
        ids.alloc();
        assert_eq!(ids.take_log().nodes, [a, NodeId(a.0 + 1)]);
    }

    #[test]
    fn check_ids_notices_edge_ids_that_are_shared_or_strangers() {
        use crate::app::testutil::GROUPS;
        let fresh = || load(GROUPS).model;

        let mut model = fresh();
        let first = model.groups[0].modulators[0].targets[0].edge;
        model.groups[0].modulators[0].targets[1].edge = first;
        let error = model.check_ids().unwrap_err();
        assert!(error.contains("another edge has too"), "two targets with one edge: {error}");

        // A lane's target and a group's.
        let mut model = load(
            r#"
[[group]]
[[group.modulator]]
[[group.modulator.target]]
member = 0
param = "detune"
[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.modulator]]
[[instrument.modulator.target]]
param = "detune"
"#,
        )
        .model;
        assert!(model.check_ids().is_ok());
        model.instruments[0].modulators[0].targets[0].edge = model.groups[0].modulators[0].targets[0].edge;
        let error = model.check_ids().unwrap_err();
        assert!(error.contains("another edge has too"), "a lane's target and a group's: {error}");

        let mut model = fresh();
        model.groups[0].modulators[0].targets[0].edge = EdgeId(9_999);
        let error = model.check_ids().unwrap_err();
        assert!(error.contains("never allocated"), "an edge nothing handed out: {error}");
    }

    #[test]
    fn check_ids_notices_audio_cables_that_are_shared_strangers_or_at_what_is_not_there_or_make_the_audio_go_round() {
        let with_cables = || {
            let (mut model, n) = the_sends();
            model.ids.alloc_edge();
            model.ids.alloc_edge();
            model.insert_audio_cable(cable(0, CableFrom::Instrument(n.lane[1]), CableTo::Effect(n.fx[0])));
            model.insert_audio_cable(cable(1, CableFrom::Effect(n.fx[0]), CableTo::Master));
            (model, n)
        };
        let (model, _) = with_cables();
        model.check_ids().unwrap();
        // An id that nothing handed out.
        let (mut model, n) = with_cables();
        model.insert_audio_cable(cable(9_999, CableFrom::Instrument(n.lane[0]), CableTo::Master));
        assert!(model.check_ids().unwrap_err().contains("never allocated"));
        // An id that a modulation cable has.
        let mut model = load(crate::app::testutil::GROUPS).model;
        let taken = model.groups[0].modulators[0].targets[0].edge;
        let node = model.instruments[0].node;
        model.insert_audio_cable(cable(taken.0, CableFrom::Instrument(node), CableTo::Master));
        assert!(model.check_ids().unwrap_err().contains("another edge has too"));
        // An end that is not there.
        let (mut model, _) = with_cables();
        model.cables[0].from = CableFrom::Instrument(NodeId(900));
        assert!(model.check_ids().unwrap_err().contains("what the cable starts at is not in the wiring"));
        let (mut model, _) = with_cables();
        model.cables[1].to = CableTo::GroupBus(NodeId(901));
        assert!(model.check_ids().unwrap_err().contains("what the cable ends at is not in the wiring"));
        // A loop (only a model that was changed behind the controller's back has one).
        let (mut model, n) = with_cables();
        model.cables[1] = cable(1, CableFrom::Effect(n.fx[0]), CableTo::Effect(n.fx[0]));
        assert!(model.check_ids().unwrap_err().contains("go round"));
    }

    #[test]
    fn check_ids_notices_duplicates_strangers_and_a_mismatched_instrument() {
        let fresh = || load("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n").model;

        let mut model = fresh();
        assert!(model.check_ids().is_ok());

        model.instruments[0].effects[0].node = model.instruments[0].node;
        assert!(model.check_ids().unwrap_err().contains("another node has"), "a duplicate");

        let mut model = fresh();
        model.instruments[0].effects[0].node = NodeId(500);
        assert!(model.check_ids().unwrap_err().contains("never allocated"), "a stranger");

        let mut model = fresh();
        let other = model.alloc_node();
        model.instruments[0].instrument.as_mut().unwrap().node = other;
        assert!(model.check_ids().unwrap_err().contains("its instrument"), "an instrument in another node");
    }

    #[test]
    fn check_ids_notices_telemetry_slots_that_are_missing_shared_reserved_or_strangers() {
        use crate::app::testutil::GROUPS;
        use crate::telemetry::MASTER;
        let fresh = || load(GROUPS).model;
        assert!(fresh().check_ids().is_ok());

        let mut model = fresh();
        model.groups[0].modulators[0].output = None;
        assert!(model.check_ids().unwrap_err().contains("has no telemetry slot"), "a modulator without one");

        let mut model = fresh();
        model.instruments[1].modulators[0].output = None;
        let error = model.check_ids().unwrap_err();
        assert!(error.contains("has no telemetry slot") && error.contains("lane 1"), "a lane's modulator without one: {error}");

        let mut model = fresh();
        model.groups[0].modulators[0].targets[0].effective = None;
        assert!(model.check_ids().unwrap_err().contains("has no telemetry slot"), "a parameter target without one");

        let mut model = fresh();
        model.instruments[0].meter = None;
        assert!(model.check_ids().unwrap_err().contains("the meter of lane 0"), "a lane without a meter");

        let mut model = fresh();
        model.groups[0].meter = None;
        assert!(model.check_ids().unwrap_err().contains("the meter of group 0"), "a group without a meter");

        let mut model = fresh();
        let meter = model.groups[0].meter.unwrap();
        model.groups[0].meter = Some(crate::telemetry::MeterSlots { peak: meter.peak, clips: meter.peak });
        assert!(model.check_ids().unwrap_err().contains("another node has too"), "a meter whose two slots are one");

        let mut model = fresh();
        model.instruments[2].meter = model.instruments[1].meter;
        assert!(model.check_ids().unwrap_err().contains("another node has too"), "two lanes with one meter");

        let mut model = fresh();
        model.instruments[0].meter = Some(MASTER);
        assert!(model.check_ids().unwrap_err().contains("master meter's"), "a lane with the master meter's");

        // Each of a meter's two slots is looked at, not only the first.
        let mut model = fresh();
        let own = model.instruments[0].meter.unwrap();
        model.instruments[0].meter = Some(crate::telemetry::MeterSlots { peak: own.peak, clips: MASTER.clips });
        assert!(model.check_ids().unwrap_err().contains("master meter's"), "a lane's clip counter that is the master's");
        let mut model = fresh();
        let own = model.groups[0].meter.unwrap();
        model.groups[0].meter = Some(crate::telemetry::MeterSlots { peak: own.peak, clips: MASTER.clips });
        assert!(model.check_ids().unwrap_err().contains("master meter's"), "a group's clip counter that is the master's");

        let mut model = fresh();
        model.groups[0].modulators[1].output = model.groups[0].modulators[0].output;
        assert!(model.check_ids().unwrap_err().contains("another node has too"), "two modulators with one");

        let mut model = fresh();
        model.groups[0].modulators[0].targets[1].effective = model.groups[0].modulators[0].targets[0].effective;
        assert!(model.check_ids().unwrap_err().contains("another node has too"), "two targets with one");

        let mut model = fresh();
        model.groups[0].modulators[0].output = Some(MASTER.clips);
        assert!(model.check_ids().unwrap_err().contains("master meter's"));

        let mut model = fresh();
        let high = model.slots.alloc().unwrap(); // allocated here...
        model.instruments[1].modulators[0].output = Some(high);
        assert!(model.check_ids().is_ok(), "...so it is fine");
        let mut model = fresh();
        model.slots = SlotAllocator::default(); // as if nothing had been allocated: every slot is a stranger
        assert!(model.check_ids().unwrap_err().contains("never allocated"));

        // A cable to a modulator's own setting has a slot like any other (f132): one without is noticed, and one it shares is.
        let mut model = fresh();
        model.groups[0].modulators[0].targets[3].effective = None;
        assert!(model.check_ids().unwrap_err().contains("has no telemetry slot"), "a cross-mod target without one");
        let mut model = fresh();
        model.groups[0].modulators[1].targets[0].effective = model.groups[0].modulators[0].targets[3].effective;
        assert!(model.check_ids().unwrap_err().contains("another node has too"), "two cross-mod targets with one");
    }

    #[test]
    fn a_modulator_rack_is_found_by_where_it_lives() {
        let mut model = load(
            r#"
[[group]]
[[group.modulator]]
type = "lfo"
[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.modulator]]
type = "lfo"
[[instrument.modulator]]
type = "envelope"
"#,
        )
        .model;
        assert_eq!(model.modulators_mut(ModulatorLocation::Lane(0)).map(|m| m.len()), Some(2));
        assert_eq!(model.modulators_mut(ModulatorLocation::Group(0)).map(|m| m.len()), Some(1));
        assert!(model.modulators_mut(ModulatorLocation::Lane(1)).is_none());
        assert!(model.modulators_mut(ModulatorLocation::Group(1)).is_none());

        // It is the rack itself, not a copy.
        model.modulators_mut(ModulatorLocation::Group(0)).unwrap().pop();
        assert_eq!((model.instruments[0].modulators.len(), model.groups[0].modulators.len()), (2, 0));
    }

    // -- sources of notes and the cables from them (P4b of docs/patch-graph.md §11.6) --

    fn the_sources() -> (SessionModel, crate::app::testutil::Routed) {
        let model = load(crate::app::testutil::SOURCES).model;
        let nodes = crate::app::testutil::Routed::of(&model);
        (model, nodes)
    }

    #[test]
    fn the_sources_are_nodes_that_come_after_the_groups_and_the_lanes_and_are_found_by_their_ids() {
        let (model, n) = the_sources();
        let nodes: Vec<(NodeId, NodePath)> = model.nodes().collect();
        assert_eq!(
            nodes,
            [
                (n.lane[0], NodePath::Instrument(0)),
                (n.lane[1], NodePath::Instrument(1)),
                (n.lane[2], NodePath::Instrument(2)),
                (n.keys, NodePath::Source(0)),
                (n.devices[0], NodePath::Source(1)),
                (n.devices[1], NodePath::Source(2)),
            ]
        );
        assert_eq!(model.locate(n.devices[0]), Some(NodePath::Source(1)));
        assert_eq!(model.locate(NodeId(900)), None);
    }

    #[test]
    fn the_cables_of_notes_have_ids_of_the_series_that_the_other_cables_have() {
        let (mut model, n) = the_sources();
        assert!(model.edge_in_use(EdgeId(0)) && model.edge_in_use(EdgeId(2)), "the cables of notes");
        assert!(model.edge_in_use(EdgeId(3)), "and the audio cable after them");
        assert!(!model.edge_in_use(EdgeId(4)));
        // The id that an event cable has is not handed out again.
        assert_eq!(model.alloc_edge(), EdgeId(4));
        // (A cable that goes out of the model frees nothing: ids are not reused but for what undo puts back.)
        model.event_cables.retain(|cable| cable.to != n.lane[1]);
        assert!(!model.edge_in_use(EdgeId(1)));
    }

    #[test]
    fn an_event_cable_goes_in_among_the_event_cables_in_the_order_of_the_ids_and_one_that_is_there_under_the_id_is_replaced() {
        let (mut model, n) = the_sources();
        model.ids.reuse(&[], &[EdgeId(10), EdgeId(11)]);
        let first = model.alloc_edge();
        let second = model.alloc_edge();
        assert_eq!((first, second), (EdgeId(10), EdgeId(11)));
        assert_eq!(model.insert_event_cable(EventCable { id: second, from: n.keys, to: n.lane[1] }), 3, "at the end: the id is the highest");
        assert_eq!(model.insert_event_cable(EventCable { id: EdgeId(5), from: n.keys, to: n.lane[2] }), 3, "in the middle, before the one that has a higher id");
        assert_eq!(model.event_cables.iter().map(|cable| cable.id.0).collect::<Vec<_>>(), [0, 1, 2, 5, 11]);
        let place = model.insert_event_cable(EventCable { id: EdgeId(5), from: n.devices[0], to: n.lane[2] });
        assert_eq!(place, 3);
        assert_eq!(model.event_cables.len(), 5, "replaced");
        assert_eq!((model.event_cables[3].from, model.event_cables[3].to), (n.devices[0], n.lane[2]));
    }

    #[test]
    fn check_ids_notices_sources_and_cables_of_notes_that_are_not_what_they_should_be() {
        let (model, _) = the_sources();
        model.check_ids().unwrap();
        // The keys twice, and a device by one name twice.
        let (mut model, _) = the_sources();
        let node = model.alloc_node();
        model.sources.push(SourceNode { node, kind: SourceKind::Keys });
        assert!(model.check_ids().unwrap_err().contains("the keys are a source more than once"));
        let (mut model, _) = the_sources();
        let node = model.alloc_node();
        model.sources.push(SourceNode { node, kind: SourceKind::Device("Seaboard".to_string()) });
        assert!(model.check_ids().unwrap_err().contains("\"Seaboard\" is a source more than once"));
        // A node that is not a source's id: a stranger, or another node's.
        let (mut model, n) = the_sources();
        model.sources[0].node = n.lane[0];
        assert!(model.check_ids().unwrap_err().contains("another node has too"));
        // An id that nothing handed out, and one that an audio cable has.
        let (mut model, n) = the_sources();
        model.insert_event_cable(EventCable { id: EdgeId(9_999), from: n.keys, to: n.lane[2] });
        assert!(model.check_ids().unwrap_err().contains("never allocated"));
        let (mut model, n) = the_sources();
        model.insert_event_cable(EventCable { id: EdgeId(3), from: n.keys, to: n.lane[2] });
        assert!(model.check_ids().unwrap_err().contains("another edge has too"));
        // A start that is not a source (a lane, a stranger) and an end that is not a lane (a source, a stranger).
        let (mut model, n) = the_sources();
        model.event_cables[0].from = n.lane[2];
        assert!(model.check_ids().unwrap_err().contains("which is not a source"));
        let (mut model, _) = the_sources();
        model.event_cables[0].from = NodeId(900);
        assert!(model.check_ids().unwrap_err().contains("which is not a source"));
        let (mut model, n) = the_sources();
        model.event_cables[0].to = n.keys;
        assert!(model.check_ids().unwrap_err().contains("which is not a lane"));
        let (mut model, _) = the_sources();
        model.event_cables[0].to = NodeId(900);
        assert!(model.check_ids().unwrap_err().contains("which is not a lane"));
        // Two between the same nodes, and out of the order of their ids.
        let (mut model, n) = the_sources();
        model.event_cables[1] = EventCable { id: EdgeId(1), from: n.keys, to: n.lane[0] };
        assert!(model.check_ids().unwrap_err().contains("between two nodes that another event cable is between"));
        let (mut model, _) = the_sources();
        model.event_cables.swap(1, 2);
        assert!(model.check_ids().unwrap_err().contains("is not after the one before it"));
    }

    /// What the open devices stand for in these tests: the keys are the keys; the first device is two ports, sources 1 and 2; the second is not plugged in.
    fn open_ports(kind: &SourceKind) -> SourceSet {
        match kind {
            SourceKind::Keys => SourceSet::NONE.with(crate::source::Source::KEYS),
            SourceKind::Device(name) if name.starts_with("Launchkey") => SourceSet::NONE.with(crate::source::Source::device(0)).with(crate::source::Source::device(1)),
            SourceKind::Device(_) => SourceSet::NONE,
        }
    }

    fn numbers(sources: SourceSet) -> Vec<u8> {
        sources.iter().map(|source| source.number()).collect()
    }

    #[test]
    fn a_lane_that_no_cable_of_notes_ends_at_hears_every_source_and_one_that_has_some_hears_the_sources_they_start_at() {
        let (model, _) = the_sources();
        assert_eq!(numbers(model.lane_sources(0, open_ports)), [0], "the keys");
        assert_eq!(numbers(model.lane_sources(1, open_ports)), [1, 2], "both ports of the first device, and the second device that is not plugged in adds none");
        assert_eq!(model.lane_sources(2, open_ports), SourceSet::ALL, "no cable of notes ends at it");
        assert_eq!(model.lane_sources(9, open_ports), SourceSet::ALL, "a lane that is not there hears what a lane does until it is told otherwise");
    }

    #[test]
    fn a_lane_whose_cables_start_at_devices_that_are_not_plugged_in_hears_nothing_and_not_every_source() {
        let (model, _) = the_sources();
        let nobody = |_: &SourceKind| SourceSet::NONE;
        assert!(model.lane_sources(0, nobody).is_empty());
        assert!(model.lane_sources(1, nobody).is_empty());
        assert_eq!(model.lane_sources(2, nobody), SourceSet::ALL, "a lane with no cable is not one of them");
    }

    #[test]
    fn each_cable_of_a_lane_is_asked_about_its_source_once_and_a_cable_from_what_is_no_source_counts_for_nothing() {
        let (mut model, n) = the_sources();
        let mut asked = Vec::new();
        model.lane_sources(1, |kind| {
            asked.push(kind.clone());
            SourceSet::NONE
        });
        assert_eq!(asked, [SourceKind::Device("Launchkey Mini MK4 37".to_string()), SourceKind::Device("Seaboard".to_string())], "in the order of the cables");
        // (A cable from a node that is no source: only a model that was changed behind the controller's back has one.)
        model.event_cables[0].from = n.lane[2];
        assert!(model.lane_sources(0, open_ports).is_empty(), "it hears nothing: it has a cable, and what the cable starts at is no source");
    }

    #[test]
    fn an_event_cable_is_taken_out_by_its_id_and_the_cables_at_a_node_are_found_in_the_order_of_their_ids() {
        let (mut model, n) = the_sources();
        assert_eq!(model.event_cables_at(&[n.lane[1]]), [EdgeId(1), EdgeId(2)], "what ends at the lane");
        assert_eq!(model.event_cables_at(&[n.keys]), [EdgeId(0)], "what starts at the source");
        assert_eq!(model.event_cables_at(&[n.devices[1], n.lane[0]]), [EdgeId(0), EdgeId(2)], "at either of the nodes, once each, in the order of the ids");
        assert!(model.event_cables_at(&[n.lane[2], NodeId(900)]).is_empty());
        let taken = model.remove_event_cable(EdgeId(1)).unwrap();
        assert_eq!((taken.id, taken.from, taken.to), (EdgeId(1), n.devices[0], n.lane[1]));
        assert_eq!(model.event_cables.iter().map(|cable| cable.id.0).collect::<Vec<_>>(), [0, 2]);
        assert_eq!(model.remove_event_cable(EdgeId(1)), None, "it is gone");
        assert_eq!(model.remove_event_cable(EdgeId(3)), None, "an audio cable's is not one of these");
        assert_eq!(model.source(n.devices[1]).map(|source| source.kind.clone()), Some(SourceKind::Device("Seaboard".to_string())));
        assert!(model.source(n.lane[0]).is_none(), "a lane is no source");
    }

    #[test]
    fn an_audio_cable_cannot_start_or_end_at_a_source_of_notes() {
        let (model, n) = the_sources();
        let audio = |node: NodeId, port| crate::app::graph::Endpoint { node, port };
        use crate::app::graph::Port;
        let refused = |from, to| model.cable_ends(from, to).unwrap_err();
        assert!(matches!(refused(audio(n.keys, Port::AudioOut), audio(NodeId::MASTER, Port::AudioIn)), IntentError::Invalid(why) if why.contains("no output of audio")));
        assert!(matches!(refused(audio(n.lane[0], Port::AudioOut), audio(n.devices[0], Port::AudioIn)), IntentError::Invalid(why) if why.contains("no input of audio")));
    }

    /// The keys, and one of every other kind of node that a cable of notes could be drawn from or to by mistake: a lane with an effect and an LFO, in a group that has a bus effect.
    const NODES_A_CABLE_OF_NOTES_COULD_BE_DRAWN_TO: &str = r#"
[keys]

[[group]]
[[group.effect]]
plugin = "builtin:reverb"

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.modulator]]
type = "lfo"
"#;

    #[test]
    fn a_cable_of_notes_goes_from_the_output_of_events_of_a_source_to_the_input_of_events_of_a_lane_and_nothing_else_is_one() {
        use crate::app::graph::{Endpoint, Port};
        let model = load(NODES_A_CABLE_OF_NOTES_COULD_BE_DRAWN_TO).model;
        let (keys, lane, effect) = (model.sources[0].node, model.instruments[0].node, model.instruments[0].effects[0].node);
        let (group, bus_effect, modulator) = (model.groups[0].node, model.groups[0].effects[0].node, model.instruments[0].modulators[0].node);
        let (from, to) = (|node| Endpoint { node, port: Port::EventsOut }, |node| Endpoint { node, port: Port::EventsIn });
        assert_eq!(model.event_cable_ends(from(keys), to(lane)), Ok((keys, lane)));

        let invalid = |why: &str| Err(IntentError::Invalid(why.to_string()));
        let (no_output, no_input) = ("a cable of notes starts at an output of events", "a cable of notes ends at an input of events");
        let (only_sources, only_lanes) = ("only a source of notes has an output of events", "a cable of notes ends at a lane: it is the lanes that have an input of events");
        // The ports are looked at first, whatever the nodes are, and then the start before the end.
        let audio_out = Endpoint { node: keys, port: Port::AudioOut };
        let audio_in = Endpoint { node: lane, port: Port::AudioIn };
        assert_eq!(model.event_cable_ends(audio_out, to(lane)), invalid(no_output));
        assert_eq!(model.event_cable_ends(to(keys), to(lane)), invalid(no_output), "an input of events is not an output");
        assert_eq!(model.event_cable_ends(from(keys), audio_in), invalid(no_input));
        assert_eq!(model.event_cable_ends(from(keys), from(lane)), invalid(no_input), "an output of events is not an input");
        assert_eq!(model.event_cable_ends(audio_out, audio_in), invalid(no_output), "the start first");
        assert_eq!(model.event_cable_ends(from(NodeId(900)), audio_in), invalid(no_input), "the ports before the nodes");
        // The start: the mix has none, a node that is not there is not found, and only a source has one.
        assert_eq!(model.event_cable_ends(from(NodeId::MASTER), to(lane)), invalid("the mix has no output of events"));
        assert_eq!(model.event_cable_ends(from(NodeId(900)), to(lane)), Err(IntentError::NotFound(NodeId(900))));
        for start in [lane, effect, group, bus_effect, modulator] {
            assert_eq!(model.event_cable_ends(from(start), to(lane)), invalid(only_sources), "from {start:?}");
        }
        // The end: the mix has none, a node that is not there is not found, and only a lane has one.
        assert_eq!(model.event_cable_ends(from(keys), to(NodeId::MASTER)), invalid("the mix has no input of events"));
        assert_eq!(model.event_cable_ends(from(keys), to(NodeId(900))), Err(IntentError::NotFound(NodeId(900))));
        for end in [keys, effect, group, bus_effect, modulator] {
            assert_eq!(model.event_cable_ends(from(keys), to(end)), invalid(only_lanes), "to {end:?}");
        }
        assert_eq!(model.event_cable_ends(from(effect), to(effect)), invalid(only_sources), "the start is looked at before the end");
    }

    #[test]
    fn a_source_goes_in_among_the_sources_at_its_place_and_at_the_end_for_a_place_past_it() {
        let (mut model, n) = the_sources();
        let device = |node: u32| SourceNode { node: NodeId(node), kind: SourceKind::Device(format!("D{node}")) };
        assert_eq!(model.insert_source_at(1, device(100)), 1);
        assert_eq!(model.insert_source_at(99, device(101)), 4, "a place past the end is the end");
        assert_eq!(model.sources.iter().map(|source| source.node).collect::<Vec<_>>(), [n.keys, NodeId(100), n.devices[0], n.devices[1], NodeId(101)]);
    }

    #[test]
    fn an_event_cable_is_found_by_its_id_among_the_event_cables_and_an_audio_cables_id_is_not_one_of_them() {
        let (model, n) = the_sources();
        assert_eq!(model.event_cable(EdgeId(1)), Some(&EventCable { id: EdgeId(1), from: n.devices[0], to: n.lane[1] }));
        assert_eq!(model.event_cable(EdgeId(3)), None, "the audio cable's");
        assert_eq!(model.event_cable(EdgeId(9)), None, "none at all");
    }

    // -- audio cables (P3c of docs/patch-graph.md §11.6) --

    fn the_sends() -> (SessionModel, Sends) {
        let model = load(SENDS).model;
        let nodes = Sends::of(&model);
        (model, nodes)
    }

    fn cable(id: u32, from: CableFrom, to: CableTo) -> AudioCable {
        AudioCable { id: EdgeId(id), from, to, gain: 1.0 }
    }

    #[test]
    fn the_wiring_is_the_lanes_with_their_groups_and_effects_and_the_groups_with_their_bus_effects() {
        let (model, n) = the_sends();
        let (lanes, groups) = model.wiring();
        assert_eq!(lanes.iter().map(|lane| (lane.node, lane.group, lane.effects.clone())).collect::<Vec<_>>(), [(n.lane[0], Some(0), vec![n.fx[0], n.fx[1]]), (n.lane[1], None, vec![n.fx[2]])]);
        assert_eq!(groups.iter().map(|group| (group.node, group.effects.clone())).collect::<Vec<_>>(), [(n.group, vec![n.bus])]);
    }

    #[test]
    fn a_cable_is_drawn_from_an_instrument_an_effect_or_a_group_to_an_effect_a_bus_or_the_mix() {
        let (model, n) = the_sends();
        let drawn = [
            (CableFrom::Instrument(n.lane[1]), CableTo::Effect(n.fx[0])),
            (CableFrom::Instrument(n.lane[0]), CableTo::Effect(n.fx[2])),
            (CableFrom::Effect(n.fx[2]), CableTo::Effect(n.bus)),
            (CableFrom::Effect(n.bus), CableTo::Effect(n.fx[2])),
            (CableFrom::Effect(n.fx[0]), CableTo::GroupBus(n.group)),
            (CableFrom::Group(n.group), CableTo::Effect(n.fx[2])),
            (CableFrom::Instrument(n.lane[0]), CableTo::Master),
            (CableFrom::Effect(n.fx[1]), CableTo::Master),
            (CableFrom::Instrument(n.lane[0]), CableTo::Effect(n.fx[1])),
        ];
        for (from, to) in drawn {
            assert_eq!(model.cable_problem(from, to), None, "{from:?} to {to:?}");
        }
    }

    #[test]
    fn a_cable_is_not_drawn_from_or_to_what_is_not_there_or_is_not_what_it_says() {
        let (model, n) = the_sends();
        assert_eq!(model.cable_problem(CableFrom::Instrument(NodeId(900)), CableTo::Master), Some(CableProblem::NoStart));
        assert_eq!(model.cable_problem(CableFrom::Instrument(n.group), CableTo::Master), Some(CableProblem::NoStart), "a group is no instrument");
        assert_eq!(model.cable_problem(CableFrom::Instrument(n.fx[0]), CableTo::Master), Some(CableProblem::NoStart), "an effect is no instrument");
        assert_eq!(model.cable_problem(CableFrom::Group(n.lane[0]), CableTo::Master), Some(CableProblem::NoStart), "a lane is no group");
        assert_eq!(model.cable_problem(CableFrom::Effect(n.lane[0]), CableTo::Master), Some(CableProblem::NoStart), "a lane is no effect");
        assert_eq!(model.cable_problem(CableFrom::Instrument(n.lane[0]), CableTo::Effect(NodeId(901))), Some(CableProblem::NoEnd));
        assert_eq!(model.cable_problem(CableFrom::Instrument(n.lane[0]), CableTo::Effect(n.lane[1])), Some(CableProblem::NoEnd), "a lane has no input");
        assert_eq!(model.cable_problem(CableFrom::Instrument(n.lane[0]), CableTo::GroupBus(n.lane[1])), Some(CableProblem::NoEnd), "a lane is no group");
        assert_eq!(model.cable_problem(CableFrom::Instrument(n.lane[0]), CableTo::GroupBus(n.bus)), Some(CableProblem::NoEnd), "an effect is no group");
    }

    #[test]
    fn a_cable_that_would_make_the_audio_go_round_is_not_drawn() {
        let (mut model, n) = the_sends();
        let loops = |model: &SessionModel, from: CableFrom, to: CableTo| matches!(model.cable_problem(from, to), Some(CableProblem::Loop(_)));
        // Back along a chain, and into itself.
        assert!(loops(&model, CableFrom::Effect(n.fx[1]), CableTo::Effect(n.fx[0])));
        assert!(loops(&model, CableFrom::Effect(n.fx[0]), CableTo::Effect(n.fx[0])));
        // A group's output into the chain of one of its members, and the bus into a member.
        assert!(loops(&model, CableFrom::Group(n.group), CableTo::Effect(n.fx[0])));
        assert!(loops(&model, CableFrom::Effect(n.bus), CableTo::Effect(n.fx[1])));
        assert!(!loops(&model, CableFrom::Group(n.group), CableTo::Effect(n.fx[2])), "the lane that is in no group is not one of its members");
        // Two cables that are no loop by themselves and are one together.
        model.insert_audio_cable(cable(1, CableFrom::Effect(n.fx[2]), CableTo::Effect(n.fx[0])));
        assert!(loops(&model, CableFrom::Effect(n.fx[1]), CableTo::Effect(n.fx[2])), "lane 1's effect feeds lane 0's, and lane 0's last effect would feed lane 1's");
        assert!(!loops(&model, CableFrom::Effect(n.fx[1]), CableTo::Master));
    }

    #[test]
    fn the_cables_are_kept_in_the_order_of_their_ids_and_are_taken_out_by_id_with_the_place_they_had() {
        let (mut model, n) = the_sends();
        let (a, b, c) = (cable(5, CableFrom::Instrument(n.lane[0]), CableTo::Master), cable(3, CableFrom::Instrument(n.lane[1]), CableTo::Master), cable(9, CableFrom::Effect(n.fx[0]), CableTo::Master));
        assert_eq!([model.insert_audio_cable(a), model.insert_audio_cable(b), model.insert_audio_cable(c)], [0, 0, 2]);
        assert_eq!(model.cables.iter().map(|cable| cable.id.0).collect::<Vec<_>>(), [3, 5, 9]);
        assert_eq!(model.audio_cable(EdgeId(5)), Some(&a));
        assert_eq!(model.audio_cable(EdgeId(4)), None);
        assert_eq!(model.remove_audio_cable(EdgeId(5)), Some((1, a)));
        assert_eq!(model.remove_audio_cable(EdgeId(5)), None, "it is not there any more");
        assert_eq!(model.insert_audio_cable(a), 1, "it goes back where it was");
        assert_eq!(model.cables, [b, a, c]);
        // A cable that is put in under the id of one that is there takes its place, and does not go beside it: no two cables have one id.
        let again = AudioCable { gain: 0.5, ..a };
        assert_eq!(model.insert_audio_cable(again), 1);
        assert_eq!(model.cables, [b, again, c]);
    }

    #[test]
    fn the_cables_at_a_node_are_those_that_start_or_end_there() {
        let (mut model, n) = the_sends();
        for (id, from, to) in [
            (1, CableFrom::Instrument(n.lane[1]), CableTo::Effect(n.fx[0])),
            (2, CableFrom::Effect(n.fx[0]), CableTo::Master),
            (3, CableFrom::Group(n.group), CableTo::Effect(n.fx[2])),
            (4, CableFrom::Instrument(n.lane[0]), CableTo::GroupBus(n.group)),
            (5, CableFrom::Effect(n.bus), CableTo::Master),
            (6, CableFrom::Effect(n.fx[2]), CableTo::Effect(n.bus)),
        ] {
            model.insert_audio_cable(cable(id, from, to));
        }
        let at = |node: NodeId| model.audio_cables_at(&[node]).iter().map(|edge| edge.0).collect::<Vec<_>>();
        assert_eq!(at(n.lane[0]), [4], "a lane: what its instrument sends");
        assert_eq!(at(n.lane[1]), [1]);
        assert_eq!(at(n.fx[0]), [1, 2], "an effect: what it sends and what it takes");
        assert_eq!(at(n.fx[1]), Vec::<u32>::new());
        assert_eq!(at(n.fx[2]), [3, 6]);
        assert_eq!(at(n.group), [3, 4], "a group: what it sends and what its bus takes");
        assert_eq!(at(n.bus), [5, 6]);
        assert_eq!(at(NodeId::MASTER), Vec::<u32>::new(), "the mix is no node that goes");
    }

    #[test]
    fn a_lane_is_not_put_in_a_group_when_a_cable_from_the_group_leads_back_into_the_lane() {
        let (mut model, n) = the_sends();
        // Lane 1 is in no group. A cable from the group's output into lane 1's effect is no loop, until the lane is a member of the group.
        model.insert_audio_cable(cable(1, CableFrom::Group(n.group), CableTo::Effect(n.fx[2])));
        assert!(model.loop_if_lane_goes_to(1, Some(0)).is_some(), "its own output would feed the group that feeds it");
        assert_eq!(model.loop_if_lane_goes_to(1, None), None, "the lane stays out of the group");
        assert_eq!(model.loop_if_lane_goes_to(0, None), None, "lane 0 leaves the group");
        assert_eq!(model.loop_if_lane_goes_to(0, Some(0)), None, "lane 0 is in it already");
        // The same through a bus effect, and through a cable that leaves the lane's chain.
        model.remove_audio_cable(EdgeId(1));
        model.insert_audio_cable(cable(2, CableFrom::Effect(n.bus), CableTo::Effect(n.fx[2])));
        assert!(model.loop_if_lane_goes_to(1, Some(0)).is_some());
        model.remove_audio_cable(EdgeId(2));
        model.insert_audio_cable(cable(3, CableFrom::Instrument(n.lane[1]), CableTo::Effect(n.bus)));
        assert_eq!(model.loop_if_lane_goes_to(1, Some(0)), None, "a cable from the lane into an effect of the bus is no loop: the lane is upstream of both");
        // A lane or a group that is not there changes nothing.
        assert_eq!(model.loop_if_lane_goes_to(9, Some(0)), None);
        assert_eq!(model.loop_if_lane_goes_to(1, Some(9)), None);
    }

    #[test]
    fn an_effect_is_not_moved_to_a_place_where_the_audio_would_go_round() {
        let (mut model, n) = the_sends();
        // Lane 0's chain is fx0 and then fx1. fx0 feeds lane 1's effect, which feeds fx1: no loop while fx0 is first.
        model.insert_audio_cable(cable(1, CableFrom::Effect(n.fx[0]), CableTo::Effect(n.fx[2])));
        model.insert_audio_cable(cable(2, CableFrom::Effect(n.fx[2]), CableTo::Effect(n.fx[1])));
        assert!(model.loop_if_effect_goes_to(n.fx[1], 0).is_some(), "fx1 would feed fx0, which feeds fx1 by the cables");
        assert!(model.loop_if_effect_goes_to(n.fx[0], 1).is_some(), "the same two effects, the same swap");
        assert_eq!(model.loop_if_effect_goes_to(n.fx[0], 0), None, "to where it is");
        // Without the second cable the swap is fine.
        model.remove_audio_cable(EdgeId(2));
        assert_eq!(model.loop_if_effect_goes_to(n.fx[1], 0), None);
        // A bus effect is moved on its bus, and what is not an effect, or is not there, has no place to go to.
        assert_eq!(model.loop_if_effect_goes_to(n.bus, 0), None);
        assert_eq!(model.loop_if_effect_goes_to(n.lane[0], 0), None);
        assert_eq!(model.loop_if_effect_goes_to(NodeId(900), 0), None);
        assert_eq!(model.loop_if_effect_goes_to(n.fx[1], 7), None, "a place the chain has not");
    }

    #[test]
    fn an_effect_of_a_bus_is_not_moved_to_a_place_where_the_audio_would_go_round() {
        let mut model = load(
            r#"
[[group]]
[[group.effect]]
plugin = "builtin:filter"
[[group.effect]]
plugin = "builtin:delay"

[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:drive"
"#,
        )
        .model;
        let (first, second, lane_fx) = (model.groups[0].effects[0].node, model.groups[0].effects[1].node, model.instruments[0].effects[0].node);
        model.insert_audio_cable(cable(1, CableFrom::Effect(first), CableTo::Effect(lane_fx)));
        model.insert_audio_cable(cable(2, CableFrom::Effect(lane_fx), CableTo::Effect(second)));
        assert!(model.loop_if_effect_goes_to(second, 0).is_some());
        assert!(model.loop_if_effect_goes_to(first, 1).is_some());
    }

    #[test]
    fn a_link_into_an_effect_is_not_given_back_where_the_audio_would_go_round_and_cutting_one_is_no_loop() {
        let (mut model, n) = the_sends();
        // The link into lane 0's delay and the one into the group's bus effect are cut, and a cable goes from the delay back to the filter before it and one from the bus effect into the delay
        // (lane 0 is in the group): each is a loop only through the link that is cut.
        model.instruments[0].effects[1].fed = false;
        model.groups[0].effects[0].fed = false;
        model.insert_audio_cable(cable(1, CableFrom::Effect(n.fx[1]), CableTo::Effect(n.fx[0])));
        model.insert_audio_cable(cable(2, CableFrom::Effect(n.bus), CableTo::Effect(n.fx[1])));
        assert!(model.loop_if_feed_is(n.fx[1], true).is_some(), "given back, the delay's link would close the loop through the cable to the filter");
        assert!(model.loop_if_feed_is(n.bus, true).is_some(), "given back, the bus effect's link would close the loop through the cable into lane 0's delay");
        // Cutting a link that is cut is no loop (it is looked at as cut, and not as if the link were there).
        assert_eq!(model.loop_if_feed_is(n.fx[1], false), None);
        assert_eq!(model.loop_if_feed_is(n.bus, false), None);
        // The links that are there are as they are, and the wiring has the cuts that the model says, a lane's and a bus's: neither cable is a loop.
        assert_eq!(model.loop_if_feed_is(n.fx[0], true), None);
        assert_eq!(model.loop_if_feed_is(n.fx[2], true), None);
        // What is no effect, and what is not there, has no link to give back.
        for node in [n.lane[0], n.group, NodeId::MASTER, NodeId(900)] {
            assert_eq!(model.loop_if_feed_is(node, true), None, "{node:?}");
        }
    }

    #[test]
    fn what_goes_with_a_lane_or_a_group_is_the_cables_at_its_node_and_at_each_of_its_effects_once() {
        let (mut model, n) = the_sends();
        for (id, from, to) in [
            (1, CableFrom::Instrument(n.lane[1]), CableTo::Effect(n.fx[0])),
            (2, CableFrom::Effect(n.fx[0]), CableTo::Effect(n.fx[1])),
            (3, CableFrom::Group(n.group), CableTo::Effect(n.fx[2])),
            (4, CableFrom::Instrument(n.lane[0]), CableTo::GroupBus(n.group)),
            (5, CableFrom::Effect(n.bus), CableTo::Master),
            (6, CableFrom::Effect(n.fx[2]), CableTo::Effect(n.bus)),
        ] {
            model.insert_audio_cable(cable(id, from, to));
        }
        assert_eq!(model.audio_nodes_of_lane(0), [n.lane[0], n.fx[0], n.fx[1]]);
        assert_eq!(model.audio_nodes_of_lane(1), [n.lane[1], n.fx[2]]);
        assert_eq!(model.audio_nodes_of_group(0), [n.group, n.bus]);
        assert_eq!((model.audio_nodes_of_lane(9), model.audio_nodes_of_group(9)), (vec![], vec![]));
        let ids = |nodes: Vec<NodeId>| model.audio_cables_at(&nodes).iter().map(|edge| edge.0).collect::<Vec<_>>();
        // (Cable 2 is between two effects of lane 0, and is one cable and not two.)
        assert_eq!(ids(model.audio_nodes_of_lane(0)), [1, 2, 4]);
        assert_eq!(ids(model.audio_nodes_of_lane(1)), [1, 3, 6]);
        assert_eq!(ids(model.audio_nodes_of_group(0)), [3, 4, 5, 6]);
        assert_eq!(ids(vec![]), Vec::<u32>::new());
    }

    #[test]
    fn a_cable_of_either_kind_has_its_id_in_use_and_no_other_does() {
        let (mut model, n) = the_sends();
        model.insert_audio_cable(cable(7, CableFrom::Instrument(n.lane[0]), CableTo::Master));
        assert!(model.edge_in_use(EdgeId(7)));
        assert!(!model.edge_in_use(EdgeId(8)));
        let modulated = load(crate::app::testutil::GROUPS).model;
        let modulation = modulated.groups[0].modulators[0].targets[0].edge;
        assert!(modulated.edge_in_use(modulation), "a modulation cable's id is in use");
        assert!(!modulated.edge_in_use(EdgeId(9_999)));
    }

    #[test]
    fn the_ends_of_a_cable_are_found_from_the_ports_of_the_graph() {
        use crate::app::graph::{Endpoint, Port};
        let (model, n) = the_sends();
        let (out, into) = (|node| Endpoint { node, port: Port::AudioOut }, |node| Endpoint { node, port: Port::AudioIn });
        assert_eq!(model.cable_ends(out(n.lane[1]), into(n.fx[0])), Ok((CableFrom::Instrument(n.lane[1]), CableTo::Effect(n.fx[0]))));
        assert_eq!(model.cable_ends(out(n.fx[0]), into(n.bus)), Ok((CableFrom::Effect(n.fx[0]), CableTo::Effect(n.bus))));
        assert_eq!(model.cable_ends(out(n.group), into(n.group)), Ok((CableFrom::Group(n.group), CableTo::GroupBus(n.group))));
        assert_eq!(model.cable_ends(out(n.bus), into(NodeId::MASTER)), Ok((CableFrom::Effect(n.bus), CableTo::Master)));
        // The wrong port, a node that cannot have that port, and a node that is not there.
        assert!(matches!(model.cable_ends(into(n.lane[1]), into(n.fx[0])), Err(IntentError::Invalid(_))));
        assert!(matches!(model.cable_ends(out(n.lane[1]), out(n.fx[0])), Err(IntentError::Invalid(_))));
        assert!(matches!(model.cable_ends(out(n.lane[1]), into(n.lane[0])), Err(IntentError::Invalid(_))), "a lane has no input");
        assert!(matches!(model.cable_ends(out(NodeId::MASTER), into(n.fx[0])), Err(IntentError::Invalid(_))), "the mix has no output");
        assert_eq!(model.cable_ends(out(NodeId(900)), into(n.fx[0])), Err(IntentError::NotFound(NodeId(900))));
        assert_eq!(model.cable_ends(out(n.lane[0]), into(NodeId(901))), Err(IntentError::NotFound(NodeId(901))));
        assert!(matches!(model.cable_ends(out(n.lane[0]), Endpoint { node: n.fx[0], port: Port::Rate }), Err(IntentError::Invalid(_))));
    }

    #[test]
    fn the_cables_into_a_plugin_are_the_lanes_and_the_groups_that_drive_a_parameter_of_it() {
        use crate::app::testutil::CHAINS;
        let model = load(CHAINS).model;
        let cable = |at: ModulatorLocation, modulator: usize, target: usize| model.modulators(at).unwrap()[modulator].targets[target].edge;
        let lane = |i: usize, m: usize, t: usize| cable(ModulatorLocation::Lane(i), m, t);
        let group = |m: usize, t: usize| cable(ModulatorLocation::Group(0), m, t);
        let (first, second, third) = (&model.instruments[0], &model.instruments[1], &model.instruments[2]);
        // The first lane's filter: its own modulator's cable to the cutoff, and the group's to it as the first member's.
        assert_eq!(model.cables_into_plugin(first.effects[0].node), [lane(0, 0, 0), group(0, 0)]);
        assert_eq!(model.cables_into_plugin(first.effects[1].node), [lane(0, 0, 1), group(0, 1)]);
        // An instrument, which has the lane's node: its detune from its own modulator and, for the second member, from the group.
        assert_eq!(model.cables_into_plugin(first.node), [lane(0, 0, 2)]);
        assert_eq!(model.cables_into_plugin(second.node), [group(0, 2)]);
        // The bus effects have the group's cables to them, and the third lane's filter its own.
        assert_eq!(model.cables_into_plugin(model.groups[0].effects[0].node), [group(0, 3)]);
        assert_eq!(model.cables_into_plugin(model.groups[0].effects[1].node), [group(0, 4)]);
        assert_eq!(model.cables_into_plugin(third.effects[0].node), [lane(2, 0, 0)]);
        // What drives only a modulator's settings or the depth of a cable is not a cable into a plugin, and a
        // node that is no plugin (or is not there) has none.
        assert!(model.cables_into_plugin(third.node).is_empty());
        assert!(model.cables_into_plugin(model.groups[0].node).is_empty());
        assert!(model.cables_into_plugin(first.modulators[1].node).is_empty());
        assert!(model.cables_into_plugin(NodeId(9999)).is_empty());
    }

    /// What a rack's modulators can drive, one run per plugin: its node and the kind of target, with the
    /// parameter's own index left out.
    fn plugins_driven(model: &SessionModel, at: ModulatorLocation) -> Vec<(NodeId, K)> {
        let mut runs: Vec<(NodeId, K)> = Vec::new();
        for target in model.param_targets(at) {
            let shape = match target.kind {
                K::PluginParam { slot, .. } => K::PluginParam { slot, param_index: 0 },
                K::GroupMember { member, slot, .. } => K::GroupMember { member, slot, param_index: 0 },
                K::GroupBus { effect_index, .. } => K::GroupBus { effect_index, param_index: 0 },
                other => panic!("{other:?} is not a parameter"),
            };
            if runs.last() != Some(&(target.node, shape.clone())) {
                runs.push((target.node, shape));
            }
        }
        runs
    }

    #[test]
    fn a_lane_modulator_can_drive_every_parameter_of_its_lanes_chain_as_the_plugins_have_them() {
        use crate::app::testutil::GROUPS;
        let mut model = load(GROUPS).model;
        // (A value the user has set is not the parameter's default.)
        model.instruments[1].instrument.as_mut().unwrap().params[1].value = 0.77;
        let lane = &model.instruments[1];
        let (osc, filter) = (lane.instrument.as_ref().unwrap(), &lane.effects[0]);
        let targets = model.param_targets(ModulatorLocation::Lane(1));

        let expected: Vec<_> =
            osc.params.iter().map(|row| (osc, 0, row)).chain(filter.params.iter().map(|row| (filter, 1, row))).collect();
        assert_eq!(targets.len(), expected.len());
        for (target, (plugin, slot, row)) in targets.iter().zip(expected) {
            assert_eq!((target.node, target.slot, target.param), (plugin.node, slot, row.index));
            assert_eq!(target.kind, K::PluginParam { slot, param_index: row.index });
            assert_eq!((target.plugin_name.as_str(), target.param_name.as_str()), (plugin.name.as_str(), row.name.as_str()));
            assert_eq!((target.base_value, target.param_min, target.param_max), (row.value, row.min, row.max));
        }
        assert_eq!(plugins_driven(&model, ModulatorLocation::Lane(1)), [(lane.node, K::PluginParam { slot: 0, param_index: 0 }), (filter.node, K::PluginParam { slot: 1, param_index: 0 })]);
    }

    #[test]
    fn a_group_modulator_can_drive_its_members_chains_in_lane_order_and_then_its_bus_effects() {
        use crate::app::testutil::GROUPS;
        let model = load(GROUPS).model;
        let (first, second) = (&model.instruments[0], &model.instruments[1]);
        // (Lane 2 is in no group: none of it is the group's to drive.)
        assert_eq!(
            plugins_driven(&model, ModulatorLocation::Group(0)),
            [
                (first.node, K::GroupMember { member: 0, slot: 0, param_index: 0 }),
                (second.node, K::GroupMember { member: 1, slot: 0, param_index: 0 }),
                (second.effects[0].node, K::GroupMember { member: 1, slot: 1, param_index: 0 }),
                (model.groups[0].effects[0].node, K::GroupBus { effect_index: 0, param_index: 0 }),
            ]
        );
        // Each parameter of the bus reverb, by its own index, with the plugin's name.
        let reverb = &model.groups[0].effects[0];
        let bus: Vec<_> = model.param_targets(ModulatorLocation::Group(0)).into_iter().filter(|t| matches!(t.kind, K::GroupBus { .. })).collect();
        assert_eq!(bus.iter().map(|t| t.param).collect::<Vec<_>>(), reverb.params.iter().map(|row| row.index).collect::<Vec<_>>());
        assert!(bus.iter().all(|t| t.slot == 0 && t.plugin_name == reverb.name));
    }

    #[test]
    fn a_group_modulator_counts_members_among_its_own_lanes_and_bus_effects_by_their_place() {
        let mut model = load(
            r#"
[[group]]
[[group.effect]]
plugin = "builtin:reverb"
[[group.effect]]
plugin = "builtin:filter"
[[group]]
[[instrument]]
plugin = "builtin:osc"
[[instrument]]
plugin = "builtin:osc"
group = 1
[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument]]
plugin = "builtin:osc"
group = 1
"#,
        )
        .model;
        let lane = |i: usize| model.instruments[i].node;
        let bus = |i: usize| model.groups[0].effects[i].node;
        // The first group: the one lane in it (the third, but its first member) and its two bus effects.
        assert_eq!(
            plugins_driven(&model, ModulatorLocation::Group(0)),
            [
                (lane(2), K::GroupMember { member: 0, slot: 0, param_index: 0 }),
                (bus(0), K::GroupBus { effect_index: 0, param_index: 0 }),
                (bus(1), K::GroupBus { effect_index: 1, param_index: 0 }),
            ]
        );
        // The second: its two lanes, and none of the first group's.
        assert_eq!(
            plugins_driven(&model, ModulatorLocation::Group(1)),
            [(lane(1), K::GroupMember { member: 0, slot: 0, param_index: 0 }), (lane(3), K::GroupMember { member: 1, slot: 0, param_index: 0 })]
        );
        // A lane that says it is in a group that is not there makes no members of one.
        model.instruments[0].group = Some(7);
        assert!(model.param_targets(ModulatorLocation::Group(7)).is_empty());
    }

    // -- What a modulator can drive of the others' --

    /// What a list of field targets says, one row each: whose, what it is called, how the rack addresses it, its range and where it is centred.
    fn field_rows(targets: &[FieldTarget]) -> Vec<(String, String, K, f32, f32, f32)> {
        targets.iter().map(|t| (t.owner.clone(), t.field_name.clone(), t.kind.clone(), t.param_min, t.param_max, t.base_value)).collect()
    }

    #[test]
    fn a_lane_modulator_can_drive_the_settings_of_the_others_in_its_rack_and_the_depths_of_their_cables_and_not_its_own() {
        use crate::app::testutil::CROSS_MOD_RACKS;
        let model = load(CROSS_MOD_RACKS).model;
        let rack = &model.instruments[0].modulators;
        let mut expected = vec![];
        let row = |owner: &str, name: &str, kind: K, min: f32, max: f32, base: f32| (owner.to_string(), name.to_string(), kind, min, max, base);
        // The envelope (1) has four stages, which an LFO does not; the MIDI control (2) has no settings, only the depth of its cable.
        expected.push(row("Mod 1", "attack", K::ModulatorAttack { mod_index: 1 }, 0.001, 10.0, 0.01));
        expected.push(row("Mod 1", "decay", K::ModulatorDecay { mod_index: 1 }, 0.001, 10.0, 0.3));
        expected.push(row("Mod 1", "sustain", K::ModulatorSustain { mod_index: 1 }, 0.0, 1.0, 0.4));
        expected.push(row("Mod 1", "release", K::ModulatorRelease { mod_index: 1 }, 0.001, 10.0, 0.5));
        expected.push(row("Mod 1", "volume depth", K::ModulatorDepth { mod_index: 1, target_index: 0 }, -1.0, 1.0, 0.2));
        expected.push(row("Mod 1", "detune depth", K::ModulatorDepth { mod_index: 1, target_index: 1 }, -1.0, 1.0, 0.7));
        expected.push(row("Mod 2", "pan depth", K::ModulatorDepth { mod_index: 2, target_index: 0 }, -1.0, 1.0, 0.3));
        let targets = model.field_targets(ModulatorLocation::Lane(0), 0);
        assert_eq!(field_rows(&targets), expected);
        // Each is the field of the modulator it says, by its node; a depth names its cable.
        let nodes: Vec<NodeId> = targets.iter().map(|t| t.node).collect();
        assert_eq!(nodes, [rack[1].node, rack[1].node, rack[1].node, rack[1].node, rack[1].node, rack[1].node, rack[2].node]);
        let fields: Vec<(CrossModField, Option<EdgeId>)> = targets.iter().map(|t| (t.field, t.edge)).collect();
        assert_eq!(
            fields,
            [
                (CrossModField::Attack, None),
                (CrossModField::Decay, None),
                (CrossModField::Sustain, None),
                (CrossModField::Release, None),
                (CrossModField::Depth(0), Some(rack[1].targets[0].edge)),
                (CrossModField::Depth(1), Some(rack[1].targets[1].edge)),
                (CrossModField::Depth(0), Some(rack[2].targets[0].edge)),
            ]
        );
        // And what it is called when it is bound: the owner and the name.
        assert_eq!(targets[0].label, "Mod 1 attack");
        assert_eq!(targets[4].label, "Mod 1 volume depth");
        assert_eq!(targets[5].label, "Mod 1 detune depth");
    }

    #[test]
    fn what_a_lane_modulator_can_drive_depends_on_which_it_is() {
        use crate::app::testutil::CROSS_MOD_RACKS;
        let model = load(CROSS_MOD_RACKS).model;
        // The envelope (1) can drive the LFO's rate and the depth of its cable, and the MIDI control's cable.
        let rows = field_rows(&model.field_targets(ModulatorLocation::Lane(0), 1));
        let names: Vec<(&str, &str)> = rows.iter().map(|r| (r.0.as_str(), r.1.as_str())).collect();
        assert_eq!(names, [("Mod 0", "rate"), ("Mod 0", "detune depth"), ("Mod 2", "pan depth")]);
        assert_eq!(rows[0].2, K::ModulatorRate { mod_index: 0 });
        // The MIDI control (2) can drive all of the others'.
        let names: Vec<String> = field_rows(&model.field_targets(ModulatorLocation::Lane(0), 2)).iter().map(|r| format!("{} {}", r.0, r.1)).collect();
        assert_eq!(
            names,
            ["Mod 0 rate", "Mod 0 detune depth", "Mod 1 attack", "Mod 1 decay", "Mod 1 sustain", "Mod 1 release", "Mod 1 volume depth", "Mod 1 detune depth"]
        );
        // A lane with one modulator has nothing to drive of the others'.
        assert!(model.field_targets(ModulatorLocation::Lane(1), 0).is_empty());
    }

    #[test]
    fn a_group_modulator_can_drive_the_settings_of_its_members_modulators_and_then_those_of_the_others_in_its_rack() {
        use crate::app::testutil::CROSS_MOD_RACKS;
        let model = load(CROSS_MOD_RACKS).model;
        let targets = model.field_targets(ModulatorLocation::Group(0), 0);
        let names: Vec<String> = field_rows(&targets).iter().map(|r| format!("{} {}", r.0, r.1)).collect();
        assert_eq!(
            names,
            [
                "M0 Mod0 rate",
                "M0 Mod0 detune depth",
                "M0 Mod1 attack",
                "M0 Mod1 decay",
                "M0 Mod1 sustain",
                "M0 Mod1 release",
                "M0 Mod1 volume depth",
                "M0 Mod1 detune depth",
                "M0 Mod2 pan depth",
                "M1 Mod0 rate",
                "Mod 1 attack",
                "Mod 1 decay",
                "Mod 1 sustain",
                "Mod 1 release",
                "Mod 1 damping depth",
                "Mod 2 volume depth",
            ]
        );
        // A member's modulator is addressed by the member's place in the group and its own in the lane's rack; the group's
        // own are addressed as a lane's are.
        assert_eq!(targets[0].kind, K::GroupMemberMod { member: 0, mod_index: 0, field: CrossModField::Rate });
        assert_eq!(targets[9].kind, K::GroupMemberMod { member: 1, mod_index: 0, field: CrossModField::Rate });
        assert_eq!(targets[10].kind, K::ModulatorAttack { mod_index: 1 });
        // A depth is of the cable at its place in the modulator.
        assert_eq!(targets[6].kind, K::GroupMemberMod { member: 0, mod_index: 1, field: CrossModField::Depth(0) });
        assert_eq!(targets[7].kind, K::GroupMemberMod { member: 0, mod_index: 1, field: CrossModField::Depth(1) });
        // The node of each is the modulator that has the field: the lane's own for a member's, the group's for the rest.
        assert_eq!(targets[0].node, model.instruments[0].modulators[0].node);
        assert_eq!(targets[9].node, model.instruments[1].modulators[0].node);
        assert_eq!(targets[10].node, model.groups[0].modulators[1].node);
        // The depth of a member's cable and of the group's own name the cable.
        assert_eq!(targets[1].edge, Some(model.instruments[0].modulators[0].targets[0].edge));
        assert_eq!(targets[14].edge, Some(model.groups[0].modulators[1].targets[0].edge));
        // The lanes that are in no group, or in another, are not the group's: the second group's own member is its first.
        let second: Vec<String> = field_rows(&model.field_targets(ModulatorLocation::Group(1), 0)).iter().map(|r| format!("{} {}", r.0, r.1)).collect();
        assert_eq!(second, ["M0 Mod0 rate"]);
        assert_eq!(model.field_targets(ModulatorLocation::Group(1), 0)[0].node, model.instruments[3].modulators[0].node);
    }

    #[test]
    fn the_modulators_in_a_modulators_reach_are_the_others_of_its_rack_and_for_a_group_its_members() {
        use crate::app::testutil::CROSS_MOD_RACKS;
        let model = load(CROSS_MOD_RACKS).model;
        let lane = |inst: usize, index: usize| model.instruments[inst].modulators[index].node;
        let group = |index: usize| model.groups[0].modulators[index].node;
        // A lane's modulator reaches the others of its lane, whatever kind they are (a MIDI control with no cable has no field to
        // drive, and is in reach all the same).
        assert_eq!(model.modulators_in_reach(ModulatorLocation::Lane(0), 0), [lane(0, 1), lane(0, 2)]);
        assert_eq!(model.modulators_in_reach(ModulatorLocation::Lane(0), 2), [lane(0, 0), lane(0, 1)]);
        assert!(model.modulators_in_reach(ModulatorLocation::Lane(1), 0).is_empty(), "the lane has the one");
        // A group's reaches its members' modulators, member by member, and then the others of its own rack.
        assert_eq!(
            model.modulators_in_reach(ModulatorLocation::Group(0), 0),
            [lane(0, 0), lane(0, 1), lane(0, 2), lane(1, 0), group(1), group(2)]
        );
        assert_eq!(model.modulators_in_reach(ModulatorLocation::Group(0), 2), [lane(0, 0), lane(0, 1), lane(0, 2), lane(1, 0), group(0), group(1)]);
        // The lane in no group and the lane of the other group are not the first group's.
        assert_eq!(model.modulators_in_reach(ModulatorLocation::Group(1), 0), [lane(3, 0)]);
        // A rack that is not there reaches none, and every modulator that has a field to drive is in reach.
        assert!(model.modulators_in_reach(ModulatorLocation::Lane(9), 0).is_empty());
        assert!(model.modulators_in_reach(ModulatorLocation::Group(9), 0).is_empty());
        for (at, index) in [(ModulatorLocation::Lane(0), 0), (ModulatorLocation::Lane(0), 2), (ModulatorLocation::Group(0), 1), (ModulatorLocation::Group(1), 0)] {
            let reach = model.modulators_in_reach(at, index);
            assert!(model.field_targets(at, index).iter().all(|t| reach.contains(&t.node)), "{at:?} {index}");
        }
    }

    #[test]
    fn a_rack_that_is_not_there_drives_no_settings_and_an_index_that_is_not_in_it_leaves_none_out() {
        use crate::app::testutil::CROSS_MOD_RACKS;
        let model = load(CROSS_MOD_RACKS).model;
        assert!(model.field_targets(ModulatorLocation::Lane(9), 0).is_empty());
        assert!(model.field_targets(ModulatorLocation::Group(9), 0).is_empty());
        // (A modulator that is not there is none of them: the others are all there is.)
        let all = model.field_targets(ModulatorLocation::Lane(0), 99);
        let each = [0, 1, 2].map(|i| model.field_targets(ModulatorLocation::Lane(0), i).len());
        assert!(all.len() > each[0] && all.len() > each[1] && all.len() > each[2]);
    }

    // -- Taking a cable out --

    use crate::app::testutil::{CABLES_THAT_DRIVE_CABLES, CABLES_THAT_DRIVE_EACH_OTHER};

    /// Where each cable that `remove_edge` took out was.
    fn places(removed: &[RemovedCable]) -> Vec<(ModulatorLocation, usize, usize)> {
        removed.iter().map(|cable| (cable.at, cable.mod_index, cable.target_index)).collect()
    }

    /// What the modulators of a rack drive: each cable's kind and depth.
    fn cables(rack: &[ModulatorSlot]) -> Vec<Vec<(K, f32)>> {
        rack.iter().map(|m| m.targets.iter().map(|t| (t.kind.clone(), t.depth)).collect()).collect()
    }

    fn depth_of(mod_index: usize, target_index: usize) -> K {
        K::ModulatorDepth { mod_index, target_index }
    }

    /// The edge of cable `target` of modulator `index` in lane `inst`.
    fn edge_of(model: &SessionModel, inst: usize, index: usize, target: usize) -> EdgeId {
        model.instruments[inst].modulators[index].targets[target].edge
    }

    #[test]
    fn a_cable_that_nothing_drives_is_taken_out_alone_and_the_depth_cables_of_the_later_ones_follow_them() {
        let mut model = load(CABLES_THAT_DRIVE_CABLES).model;
        let (volume, pan) = (edge_of(&model, 0, 0, 1), edge_of(&model, 0, 0, 2));
        let removals = model.remove_edge(volume).unwrap();
        assert_eq!(places(&removals), [(ModulatorLocation::Lane(0), 0, 1)]);
        let rack = &model.instruments[0].modulators;
        assert_eq!(rack[0].targets.len(), 2);
        assert_eq!(rack[0].targets[1].edge, pan, "the cable that was third is the second, and is the same cable");
        // The second modulator drove the first's third cable's depth: it is the second now.
        assert_eq!(cables(rack)[1], [(depth_of(0, 0), 0.4), (depth_of(0, 1), 0.5)]);
        assert_eq!(cables(rack)[2], [(depth_of(1, 0), 0.6)]);
        model.check_ids().unwrap();
    }

    #[test]
    fn the_cables_that_drive_the_depth_of_a_cable_go_with_it_and_those_that_drive_theirs() {
        let mut model = load(CABLES_THAT_DRIVE_CABLES).model;
        let (detune, pan) = (edge_of(&model, 0, 0, 0), edge_of(&model, 0, 0, 2));
        let survivor = edge_of(&model, 0, 1, 1);
        let removals = model.remove_edge(detune).unwrap();
        // The third modulator's cable drove the depth of the second's first, which drove the depth of this one: the
        // deepest goes first, and each is told the place it has at its turn.
        assert_eq!(places(&removals), [(ModulatorLocation::Lane(0), 2, 0), (ModulatorLocation::Lane(0), 1, 0), (ModulatorLocation::Lane(0), 0, 0)]);
        let rack = &model.instruments[0].modulators;
        assert_eq!(rack[0].targets.iter().map(|t| t.depth).collect::<Vec<_>>(), [0.2, 0.3], "volume and pan are left, in their order");
        assert_eq!(rack[0].targets.len(), 2);
        assert_eq!(rack[0].targets[1].edge, pan);
        // What is left of the second modulator is the cable that drove the first's third, now its second.
        assert_eq!(cables(rack)[1], [(depth_of(0, 1), 0.5)]);
        assert_eq!(rack[1].targets[0].edge, survivor);
        assert!(rack[2].targets.is_empty());
        model.check_ids().unwrap();
    }

    #[test]
    fn the_cables_of_a_group_that_drive_the_depth_of_a_members_cable_go_with_it() {
        use crate::app::testutil::CROSS_MOD;
        use crate::plugin::chain::CrossModField as F;
        let mut model = load(CROSS_MOD).model;
        // The member's first modulator drives one parameter. The member's third drives the depth of that cable, and
        // so does the group's second.
        let detune = edge_of(&model, 0, 0, 0);
        let group_cables_before = model.groups[0].modulators[1].targets.len();
        let removals = model.remove_edge(detune).unwrap();
        assert_eq!(
            places(&removals),
            [(ModulatorLocation::Group(0), 1, 7), (ModulatorLocation::Lane(0), 2, 1), (ModulatorLocation::Lane(0), 0, 0)],
            "the group's cable, then the lane's, then the cable"
        );
        assert!(model.instruments[0].modulators[0].targets.is_empty());
        let third = &model.instruments[0].modulators[2];
        assert!(third.targets.iter().all(|t| !matches!(t.kind, K::ModulatorDepth { .. })), "{:?}", cables(std::slice::from_ref(third)));
        assert_eq!(third.targets.len(), 5);
        let group = &model.groups[0].modulators[1];
        assert_eq!(group.targets.len(), group_cables_before - 1);
        assert!(group.targets.iter().all(|t| !matches!(t.kind, K::GroupMemberMod { field: F::Depth(_), .. })));
        model.check_ids().unwrap();
    }

    #[test]
    fn a_members_cable_that_is_taken_out_moves_the_groups_cables_to_the_depths_of_its_later_ones_and_no_others() {
        use crate::plugin::chain::CrossModField as F;
        let cables = |count: usize| {
            (0..count).map(|i| format!("[[instrument.modulator.target]]\nparam = \"detune\"\ndepth = 0.{}\n", i + 1)).collect::<String>()
        };
        // Two members. The first has two modulators, the second one, each with three cables. The group's modulator
        // drives the depth of the first member's first modulator's first and third cables, of its second modulator's
        // third, and of the second member's modulator's third.
        let mut model = load(&format!(
            r#"
[[group]]
[[group.modulator]]
rate = 1.0
[[group.modulator.target]]
member = 0
mod_depth = [0, 0]
depth = 0.1
[[group.modulator.target]]
member = 0
mod_depth = [0, 2]
depth = 0.2
[[group.modulator.target]]
member = 0
mod_depth = [1, 2]
depth = 0.3
[[group.modulator.target]]
member = 1
mod_depth = [0, 2]
depth = 0.4

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.modulator]]
rate = 1.0
{}[[instrument.modulator]]
rate = 2.0
{}
[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.modulator]]
rate = 3.0
{}"#,
            cables(3),
            cables(3),
            cables(3)
        ))
        .model;
        let drive = |member, mod_index, depth| K::GroupMemberMod { member, mod_index, field: F::Depth(depth) };
        let in_the_group = |model: &SessionModel| model.groups[0].modulators[0].targets.iter().map(|t| t.kind.clone()).collect::<Vec<_>>();
        assert_eq!(in_the_group(&model), [drive(0, 0, 0), drive(0, 0, 2), drive(0, 1, 2), drive(1, 0, 2)]);

        // The first member's first modulator loses its second cable, which no cable of the group drives the depth of:
        // its third is its second now, and so is the group's cable that drove that one's depth. The cables that drive
        // the depths of the first member's other modulator's, and of the other member's, are where they were.
        let second = edge_of(&model, 0, 0, 1);
        assert_eq!(places(&model.remove_edge(second).unwrap()), [(ModulatorLocation::Lane(0), 0, 1)]);
        assert_eq!(in_the_group(&model), [drive(0, 0, 0), drive(0, 0, 1), drive(0, 1, 2), drive(1, 0, 2)]);

        // The second member's modulator loses its first cable: its third is its second, and its group cable follows.
        let first_of_second_member = edge_of(&model, 1, 0, 0);
        model.remove_edge(first_of_second_member).unwrap();
        assert_eq!(in_the_group(&model), [drive(0, 0, 0), drive(0, 0, 1), drive(0, 1, 2), drive(1, 0, 1)]);
        model.check_ids().unwrap();
    }

    #[test]
    fn a_cable_of_a_group_modulator_is_taken_out_with_the_cables_that_drive_its_depth() {
        let mut model = load(
            r#"
[[group]]
[[group.effect]]
plugin = "builtin:reverb"
[[group.modulator]]
rate = 1.0
[[group.modulator.target]]
bus = 0
param = "size"
depth = 0.1
[[group.modulator.target]]
bus = 0
param = "damping"
depth = 0.2
[[group.modulator]]
rate = 2.0
[[group.modulator.target]]
mod_depth = [0, 0]
depth = 0.3
[[group.modulator.target]]
mod_depth = [0, 1]
depth = 0.4
"#,
        )
        .model;
        let size = model.groups[0].modulators[0].targets[0].edge;
        let removals = model.remove_edge(size).unwrap();
        assert_eq!(places(&removals), [(ModulatorLocation::Group(0), 1, 0), (ModulatorLocation::Group(0), 0, 0)]);
        let rack = &model.groups[0].modulators;
        assert_eq!(rack[0].targets.len(), 1, "damping is left");
        assert_eq!(cables(rack)[1], [(depth_of(0, 0), 0.4)], "and what drove its depth, which is its first now");
        model.check_ids().unwrap();
    }

    // -- Putting a cable back --

    /// Everything that the racks of the model say about their cables, by modulator (the lanes', then the groups'): each cable as it is
    /// written down, its id, place in the chain, kind, depth, range and slot.
    fn racks(model: &SessionModel) -> Vec<Vec<String>> {
        let lanes = model.instruments.iter().flat_map(|lane| lane.modulators.iter());
        let groups = model.groups.iter().flat_map(|group| group.modulators.iter());
        lanes.chain(groups).map(|modulator| modulator.targets.iter().map(|target| format!("{target:?}")).collect()).collect()
    }

    fn every_cable(model: &SessionModel) -> Vec<EdgeId> {
        let lanes = model.instruments.iter().flat_map(|lane| lane.modulators.iter());
        let groups = model.groups.iter().flat_map(|group| group.modulators.iter());
        lanes.chain(groups).flat_map(|modulator| modulator.targets.iter().map(|target| target.edge)).collect()
    }

    #[test]
    fn every_cable_of_every_session_is_put_back_where_it_was_with_the_cables_that_went_with_it() {
        use crate::app::testutil::{CROSS_MOD, CROSS_MOD_RACKS, LANES_IN_GROUPS};
        let sessions = [CABLES_THAT_DRIVE_CABLES, CABLES_THAT_DRIVE_EACH_OTHER, CROSS_MOD, CROSS_MOD_RACKS, LANES_IN_GROUPS];
        let (mut tried, mut cycles) = (0, 0);
        for (n, session) in sessions.into_iter().enumerate() {
            for edge in every_cable(&load(session).model) {
                // (A session of its own for each cable: what goes and is not put back would leave the next without its cable.)
                let mut model = load(session).model;
                let before = racks(&model);
                let removed = model.remove_edge(edge).unwrap();
                // (Cables that drive each other's depths are not put back: see `RemovedCable::still_driven`.)
                if removed.iter().any(|cable| cable.still_driven) {
                    cycles += 1;
                    continue;
                }
                for cable in removed.into_iter().rev() {
                    assert!(model.insert_target_at(cable.at, cable.mod_index, cable.target_index, cable.slot).is_some());
                }
                assert_eq!(racks(&model), before, "session {n}: {edge:?} taken out and put back");
                model.check_ids().unwrap();
                tried += 1;
            }
        }
        assert!(tried > 30, "{tried} cables is not enough to say much");
        assert_eq!(cycles, 2, "the two cables that drive each other's depths are the only ones that are not put back");
    }

    #[test]
    fn a_cable_whose_driver_is_still_there_when_it_goes_says_so_and_only_when_it_is() {
        let mut model = load(CABLES_THAT_DRIVE_EACH_OTHER).model;
        // Each of the first cables drives the depth of the other: one goes first, with the other still driving its place.
        let first = edge_of(&model, 0, 0, 0);
        let removed = model.remove_edge(first).unwrap();
        assert_eq!(removed.iter().map(|cable| cable.still_driven).collect::<Vec<_>>(), [true, false]);
        // A chain of cables that drive each other's depths one way round has none that is left driving: the driver goes first.
        let mut model = load(CABLES_THAT_DRIVE_CABLES).model;
        let detune = edge_of(&model, 0, 0, 0);
        let removed = model.remove_edge(detune).unwrap();
        assert_eq!(removed.iter().map(|cable| cable.still_driven).collect::<Vec<_>>(), [false, false, false]);
    }

    #[test]
    fn a_cable_of_a_group_that_drives_the_depth_of_a_members_cable_is_taken_out_first_so_that_it_is_not_left_driving_it() {
        use crate::app::testutil::CROSS_MOD;
        let mut model = load(CROSS_MOD).model;
        let detune = edge_of(&model, 0, 0, 0);
        let removed = model.remove_edge(detune).unwrap();
        assert_eq!(places(&removed), [(ModulatorLocation::Group(0), 1, 7), (ModulatorLocation::Lane(0), 2, 1), (ModulatorLocation::Lane(0), 0, 0)]);
        assert!(removed.iter().all(|cable| !cable.still_driven));
    }

    #[test]
    fn a_cable_that_is_taken_out_says_what_it_was_centred_on() {
        let mut model = load(CABLES_THAT_DRIVE_CABLES).model;
        let osc = model.instruments[0].instrument.as_ref().unwrap();
        let detune = osc.params.iter().find(|row| row.name == "detune").unwrap().value;
        let first = edge_of(&model, 0, 0, 0);
        let removed = model.remove_edge(first).unwrap();
        // The deepest goes first: a cable that drives the depth of the one that drives the depth of this one is centred on that depth.
        assert_eq!(removed.iter().map(|cable| cable.slot.depth).collect::<Vec<_>>(), [0.6, 0.4, 0.1]);
        assert_eq!(removed.iter().map(|cable| cable.base_value).collect::<Vec<_>>(), [0.4, 0.1, detune]);
        assert_eq!(removed.iter().map(|cable| cable.slot.edge).collect::<Vec<_>>().last(), Some(&first));
    }

    #[test]
    fn what_a_cable_is_centred_on_is_what_the_user_set_what_it_drives_to_and_nothing_for_what_is_not_there() {
        use crate::app::testutil::CROSS_MOD;
        let model = load(CROSS_MOD).model;
        // A group's cable to a member's parameter, to a bus parameter and to a field of a member's modulator; a lane's to a sibling's rate.
        let mut found = 0;
        for (at, rack) in [(ModulatorLocation::Group(0), &model.groups[0].modulators), (ModulatorLocation::Lane(0), &model.instruments[0].modulators)] {
            for (index, modulator) in rack.iter().enumerate() {
                for target in &modulator.targets {
                    // (The session has a cable to the depth of a cable that its modulator does not have: it drives nothing.)
                    if model.driven_by(at, index, &target.kind).is_some() {
                        let centre = model.centre_of(at, index, &target.kind);
                        assert!(centre.is_some_and(f32::is_finite), "{at:?} {index}: {:?} drives something that is there", target.kind);
                        found += 1;
                    } else {
                        assert_eq!(model.centre_of(at, index, &target.kind), None, "{at:?} {index}: {:?}", target.kind);
                    }
                }
            }
        }
        assert!(found > 10, "{found}");
        let gone = K::PluginParam { slot: 9, param_index: 0 };
        assert_eq!(model.centre_of(ModulatorLocation::Lane(0), 0, &gone), None);
        assert_eq!(model.centre_of(ModulatorLocation::Lane(7), 0, &K::PluginParam { slot: 0, param_index: 0 }), None);
    }

    #[test]
    fn a_cable_that_drove_nothing_when_it_went_is_put_back_centred_on_the_bottom_of_its_range() {
        use crate::app::testutil::CROSS_MOD;
        let mut model = load(CROSS_MOD).model;
        // The group's second modulator has a cable to the depth of a cable that the first has not: it drives nothing.
        let nothing = model.groups[0].modulators[1].targets.iter().find(|t| t.kind == depth_of(0, 0)).unwrap().edge;
        let removed = model.remove_edge(nothing).unwrap();
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].base_value, removed[0].slot.param_min);
    }

    /// A cable to the instrument's parameter 5, told apart by its id.
    fn spare_cable(edge: u32) -> ModTargetSlot {
        ModTargetSlot {
            edge: EdgeId(edge),
            slot: 0,
            param_name: "spare".to_string(),
            kind: K::PluginParam { slot: 0, param_index: 5 },
            depth: 0.7,
            param_min: 0.0,
            param_max: 1.0,
            effective: None,
        }
    }

    #[test]
    fn a_cable_is_put_in_at_the_place_it_is_given_or_the_last_and_not_at_all_where_there_is_no_modulator() {
        let mut model = load(CABLES_THAT_DRIVE_CABLES).model;
        assert_eq!(model.insert_target_at(ModulatorLocation::Lane(0), 0, 1, spare_cable(900)), Some(1));
        let edges = |model: &SessionModel| model.instruments[0].modulators[0].targets.iter().map(|t| t.edge.0).collect::<Vec<_>>();
        assert_eq!(edges(&model)[1], 900);
        assert_eq!(edges(&model).len(), 4);
        // The second modulator drove the depth of the first's third cable, which is the fourth now: it is told.
        assert_eq!(cables(&model.instruments[0].modulators)[1], [(depth_of(0, 0), 0.4), (depth_of(0, 3), 0.5)]);
        assert_eq!(model.insert_target_at(ModulatorLocation::Lane(0), 0, 99, spare_cable(901)), Some(4), "past the end is the last place");
        assert_eq!(edges(&model).last(), Some(&901));
        assert_eq!(cables(&model.instruments[0].modulators)[1], [(depth_of(0, 0), 0.4), (depth_of(0, 3), 0.5)], "and moves nothing");
        for at in [ModulatorLocation::Lane(0), ModulatorLocation::Lane(5), ModulatorLocation::Group(0)] {
            let index = if at == ModulatorLocation::Lane(0) { 7 } else { 0 };
            assert_eq!(model.insert_target_at(at, index, 0, spare_cable(902)), None, "{at:?} {index}");
        }
        assert_eq!(edges(&model).len(), 5);
    }

    #[test]
    fn a_cable_that_goes_in_keeps_the_kind_it_has_though_it_names_a_place_of_the_modulator_it_goes_into() {
        let mut model = load(CABLES_THAT_DRIVE_CABLES).model;
        let mut cable = spare_cable(900);
        cable.kind = depth_of(0, 2);
        assert_eq!(model.insert_target_at(ModulatorLocation::Lane(0), 0, 0, cable), Some(0));
        let rack = &model.instruments[0].modulators;
        assert_eq!(rack[0].targets[0].kind, depth_of(0, 2), "it is not told a place it was not at: the others are");
        assert_eq!(cables(rack)[1], [(depth_of(0, 1), 0.4), (depth_of(0, 3), 0.5)]);
    }

    #[test]
    fn a_cable_in_a_members_modulator_that_goes_in_leaves_the_groups_cables_to_the_cables_of_the_other_members_alone() {
        use crate::plugin::chain::CrossModField as F;
        // Two members, each with a modulator of three cables; the group drives the depth of the third cable of each.
        let mut model = load(
            r#"
[[group]]
[[group.modulator]]
rate = 1.0
[[group.modulator.target]]
member = 0
mod_depth = [0, 2]
depth = 0.1
[[group.modulator.target]]
member = 1
mod_depth = [0, 2]
depth = 0.2

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.modulator]]
rate = 1.0
[[instrument.modulator.target]]
param = "detune"
[[instrument.modulator.target]]
param = "volume"
[[instrument.modulator.target]]
param = "pan"

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.modulator]]
rate = 1.0
[[instrument.modulator.target]]
param = "detune"
[[instrument.modulator.target]]
param = "volume"
[[instrument.modulator.target]]
param = "pan"
"#,
        )
        .model;
        let drive = |member, depth| K::GroupMemberMod { member, mod_index: 0, field: F::Depth(depth) };
        let in_the_group = |model: &SessionModel| model.groups[0].modulators[0].targets.iter().map(|t| t.kind.clone()).collect::<Vec<_>>();
        assert_eq!(in_the_group(&model), [drive(0, 2), drive(1, 2)]);
        // The first member's modulator gets a cable at its first place: the group's cable to its third is to its fourth, and the other member's is where it was.
        assert_eq!(model.insert_target_at(ModulatorLocation::Lane(0), 0, 0, spare_cable(900)), Some(0));
        assert_eq!(in_the_group(&model), [drive(0, 3), drive(1, 2)]);
    }

    #[test]
    fn a_cable_in_a_members_modulator_that_goes_in_tells_the_groups_cables_to_the_depths_of_the_later_ones() {
        use crate::app::testutil::CROSS_MOD;
        use crate::plugin::chain::CrossModField as F;
        let mut model = load(CROSS_MOD).model;
        let drive = |member, mod_index, depth| K::GroupMemberMod { member, mod_index, field: F::Depth(depth) };
        let in_the_group = |model: &SessionModel| model.groups[0].modulators[1].targets.iter().map(|t| t.kind.clone()).collect::<Vec<_>>();
        let before = in_the_group(&model);
        assert!(before.contains(&drive(0, 0, 0)), "the group drives the depth of the member's first modulator's first cable");
        // The member (lane 0) has a cable more in its first modulator, at its first place: the group's cable is to the second now.
        assert_eq!(model.insert_target_at(ModulatorLocation::Lane(0), 0, 0, spare_cable(900)), Some(0));
        let after = in_the_group(&model);
        assert!(after.contains(&drive(0, 0, 1)) && !after.contains(&drive(0, 0, 0)), "{after:?}");
        // A cable of another modulator of the member is another's: the group's cables to the first's are where they were.
        assert_eq!(model.insert_target_at(ModulatorLocation::Lane(0), 2, 0, spare_cable(901)), Some(0));
        assert_eq!(in_the_group(&model), after);
    }

    // -- Putting an effect back --

    /// What every chain has and what drives it, written out: each lane's effects, group and modulators (with the slots their cables say), then each group's.
    fn chains_and_cables_of(model: &SessionModel) -> String {
        let lanes = model.instruments.iter().map(|lane| format!("{:?} | {:?} | {:?}", lane.effects, lane.modulators, lane.group));
        let groups = model.groups.iter().map(|group| format!("{:?} | {:?}", group.effects, group.modulators));
        lanes.chain(groups).collect::<Vec<_>>().join("\n")
    }

    /// The nodes of the effects of the model, a lane's and a bus's, in the order it lists them.
    fn effect_nodes(model: &SessionModel) -> Vec<NodeId> {
        model.nodes().filter(|(_, path)| matches!(path, NodePath::Effect { .. } | NodePath::GroupEffect { .. })).map(|(id, _)| id).collect()
    }

    /// Take the effect in `node` out as the controller does (the cables that drive it first, each with those that drive their depths, and then the
    /// effect) and put everything back in the reverse order. False, and not put back, if cables that drive each other's depths went.
    fn effect_out_and_back(model: &mut SessionModel, node: NodeId) -> bool {
        let mut cables = Vec::new();
        for edge in model.cables_into_plugin(node) {
            // (One that an earlier cascade took with it is not there any more.)
            cables.extend(model.remove_edge(edge).into_iter().flatten());
        }
        let path = model.locate(node).unwrap();
        let effect = match path {
            NodePath::Effect { inst, index } => model.remove_effect(inst, index).unwrap(),
            NodePath::GroupEffect { group, index } => model.remove_bus_effect(group, index).unwrap(),
            _ => panic!("{path:?} is no effect"),
        };
        if cables.iter().any(|cable| cable.still_driven) {
            return false;
        }
        match path {
            NodePath::Effect { inst, index } => assert_eq!(model.insert_effect_at(inst, index, effect), Some(index)),
            NodePath::GroupEffect { group, index } => assert_eq!(model.insert_bus_effect_at(group, index, effect), Some(index)),
            _ => unreachable!(),
        }
        for cable in cables.into_iter().rev() {
            assert!(model.insert_target_at(cable.at, cable.mod_index, cable.target_index, cable.slot).is_some());
        }
        true
    }

    #[test]
    fn every_effect_of_every_session_is_put_back_where_it_was_with_the_cables_that_drove_it_and_the_ones_that_were_told_their_slots() {
        use crate::app::testutil::{CHAINS, CROSS_MOD_RACKS, GROUPS, LANES_IN_GROUPS};
        let (mut tried, mut cycles) = (0, 0);
        for (name, session) in [("LONG_CHAINS", LONG_CHAINS), ("CHAINS", CHAINS), ("GROUPS", GROUPS), ("LANES_IN_GROUPS", LANES_IN_GROUPS), ("CROSS_MOD_RACKS", CROSS_MOD_RACKS)] {
            let count = effect_nodes(&load(session).model).len();
            for n in 0..count {
                // (A session of its own for each: what goes and is not put back would leave the next without it.)
                let mut model = load(session).model;
                let (node, before) = (effect_nodes(&model)[n], chains_and_cables_of(&model));
                if !effect_out_and_back(&mut model, node) {
                    cycles += 1;
                    continue;
                }
                assert_eq!(chains_and_cables_of(&model), before, "effect {n} of {name} taken out and put back");
                model.check_ids().unwrap();
                tried += 1;
            }
        }
        assert!(tried > 20, "{tried} effects is not enough to say much");
        assert_eq!(cycles, 0);
    }

    /// An effect to put in, taken from a lane that is not in a group (a spare: its node is the one it had).
    fn spare_effect(model: &mut SessionModel) -> PluginSlot {
        model.remove_effect(1, 0).expect("lane 1 has an effect")
    }

    #[test]
    fn an_effect_is_put_in_a_lanes_chain_at_its_place_and_the_cables_to_it_and_to_the_effects_after_it_are_told_their_slots() {
        let mut model = load(LONG_CHAINS).model;
        // Lane 0 is member 0 of the group: its own modulator has a cable to each effect (slots 1 to 4), and so has the group's (member 0, slots 1 to 4).
        let own = |model: &SessionModel| model.instruments[0].modulators[0].targets.iter().map(|t| (t.slot, t.kind.clone())).collect::<Vec<_>>();
        let groups = |model: &SessionModel| -> Vec<(usize, usize)> {
            model.groups[0].modulators[0].targets.iter().filter_map(|t| match t.kind { K::GroupMember { member, slot, .. } => Some((member, slot)), _ => None }).collect()
        };
        let slot_of = |kind: &K| match kind { K::PluginParam { slot, .. } => *slot, _ => usize::MAX };
        assert_eq!(own(&model).iter().map(|(label, kind)| (*label, slot_of(kind))).collect::<Vec<_>>(), [(1, 1), (2, 2), (3, 3), (4, 4)]);
        let spare = spare_effect(&mut model);
        assert_eq!(model.insert_effect_at(0, 1, spare), Some(1));
        assert_eq!(model.instruments[0].effects.len(), 5);
        // What drove the first is as it was; what drove the second and the ones after drive the third and the ones after, by the slot the cable says and
        // the slot its kind says.
        assert_eq!(own(&model).iter().map(|(label, kind)| (*label, slot_of(kind))).collect::<Vec<_>>(), [(1, 1), (3, 3), (4, 4), (5, 5)]);
        assert_eq!(groups(&model), [(0, 1), (0, 3), (0, 4), (0, 5)], "the group's cables to the member too");
    }

    #[test]
    fn an_effect_that_is_put_in_at_the_end_or_past_it_is_the_last_and_moves_no_cable_and_one_in_a_lane_that_is_not_there_is_not_put_anywhere() {
        for index in [4, 5, 99] {
            let mut model = load(LONG_CHAINS).model;
            let before = chains_and_cables_of(&model);
            let spare = spare_effect(&mut model);
            let before_spare = chains_and_cables_of(&model);
            assert_ne!(before_spare, before, "the spare is out");
            assert_eq!(model.insert_effect_at(0, index, spare), Some(4), "put at {index}");
            assert_eq!(model.instruments[0].effects.len(), 5);
            let cables: Vec<_> = model.instruments[0].modulators[0].targets.iter().map(|t| t.slot).collect();
            assert_eq!(cables, [1, 2, 3, 4], "put at {index}: nothing moved");
        }
        let mut model = load(LONG_CHAINS).model;
        let spare = spare_effect(&mut model);
        let before = chains_and_cables_of(&model);
        assert_eq!(model.insert_effect_at(7, 0, spare), None);
        assert_eq!(chains_and_cables_of(&model), before);
    }

    #[test]
    fn an_effect_that_is_put_in_the_chain_of_a_lane_tells_the_group_only_when_the_lane_is_a_member_and_only_the_cables_to_that_member() {
        let mut model = load(LONG_CHAINS).model;
        let group_slots = |model: &SessionModel| -> Vec<(usize, usize)> {
            model.groups[0].modulators[0].targets.iter().filter_map(|t| match t.kind { K::GroupMember { member, slot, .. } => Some((member, slot)), _ => None }).collect()
        };
        assert_eq!(group_slots(&model), [(0, 1), (0, 2), (0, 3), (0, 4)]);
        // Lane 1 is in no group: what goes into its chain moves nothing of the group's.
        let spare = spare_effect(&mut model);
        assert_eq!(model.insert_effect_at(1, 0, spare), Some(0));
        assert_eq!(group_slots(&model), [(0, 1), (0, 2), (0, 3), (0, 4)]);
        // A cable to a plugin of another member (the model does not mind that this session has none) is not what member 0's chain moves.
        let last = model.groups[0].modulators[0].targets.iter().position(|t| matches!(t.kind, K::GroupMember { slot: 4, .. })).unwrap();
        model.groups[0].modulators[0].targets[last].kind = K::GroupMember { member: 1, slot: 2, param_index: 0 };
        let spare = model.remove_effect(1, 0).unwrap();
        assert_eq!(model.insert_effect_at(0, 0, spare), Some(0));
        assert_eq!(group_slots(&model), [(0, 2), (0, 3), (0, 4), (1, 2)]);
    }

    #[test]
    fn an_effect_is_put_in_a_groups_bus_at_its_place_and_the_cables_to_the_bus_effects_from_that_place_on_are_told_their_places() {
        let mut model = load(LONG_CHAINS).model;
        let bus = |model: &SessionModel| -> Vec<usize> {
            model.groups[0].modulators[0].targets.iter().filter_map(|t| match t.kind { K::GroupBus { effect_index, .. } => Some(effect_index), _ => None }).collect()
        };
        let members = |model: &SessionModel| -> Vec<usize> {
            model.groups[0].modulators[0].targets.iter().filter_map(|t| match t.kind { K::GroupMember { slot, .. } => Some(slot), _ => None }).collect()
        };
        assert_eq!(bus(&model), [0, 1, 2, 3]);
        let (members_before, spare) = (members(&model), model.remove_bus_effect(0, 3).unwrap());
        assert_eq!(bus(&model), [0, 1, 2], "the cable to it went with it");
        assert_eq!(model.insert_bus_effect_at(0, 1, spare), Some(1));
        assert_eq!(bus(&model), [0, 2, 3], "what drove the second and the third drives the third and the fourth");
        assert_eq!(members(&model), members_before, "the cables to the members' plugins are not the bus's");
        // At the end, past it, and for a group that is not there.
        let spare = model.remove_bus_effect(0, 0).unwrap();
        assert_eq!(bus(&model), [1, 2]);
        assert_eq!(model.insert_bus_effect_at(0, 99, spare), Some(3));
        assert_eq!(bus(&model), [1, 2], "one that goes in at the end moves nothing");
        let spare = model.remove_bus_effect(0, 3).unwrap();
        assert_eq!(model.insert_bus_effect_at(4, 0, spare), None);
    }

    // -- Putting a lane back --

    /// Take lane `inst` out as the controller does (the cables of its group to it first, each with those that drive their depths, and then the lane) and put everything back in the
    /// reverse order, the lane at `index`. False, and not put back, if cables that drive each other's depths went.
    fn lane_out_and_back(model: &mut SessionModel, inst: usize, index: usize) -> bool {
        let mut cables = Vec::new();
        for edge in model.group_cables_into_lane(inst) {
            // (One that an earlier cascade took with it is not there any more.)
            cables.extend(model.remove_edge(edge).into_iter().flatten());
        }
        let lane = model.remove_lane(inst).unwrap();
        if cables.iter().any(|cable| cable.still_driven) {
            return false;
        }
        assert_eq!(model.insert_lane_at(index, lane), index);
        for cable in cables.into_iter().rev() {
            assert!(model.insert_target_at(cable.at, cable.mod_index, cable.target_index, cable.slot).is_some());
        }
        true
    }

    #[test]
    fn every_lane_of_every_session_is_put_back_where_it_was_with_the_cables_of_its_group_and_the_ones_that_were_told_their_ordinals() {
        use crate::app::testutil::{assert_same_model, CHAINS, CROSS_MOD_RACKS, GROUPS, LANES_IN_GROUPS};
        let (mut tried, mut cycles) = (0, 0);
        for (name, session) in [("LONG_CHAINS", LONG_CHAINS), ("CHAINS", CHAINS), ("GROUPS", GROUPS), ("LANES_IN_GROUPS", LANES_IN_GROUPS), ("CROSS_MOD_RACKS", CROSS_MOD_RACKS)] {
            let before = load(session).model;
            for inst in 0..before.instruments.len() {
                // (A session of its own for each: what goes and is not put back would leave the next without it.)
                let mut model = load(session).model;
                if !lane_out_and_back(&mut model, inst, inst) {
                    cycles += 1;
                    continue;
                }
                assert_same_model(&format!("lane {inst} of {name} taken out and put back"), &load(session).model, &model);
                model.check_ids().unwrap();
                tried += 1;
            }
        }
        assert!(tried > 10, "{tried} lanes is not enough to say much");
        assert_eq!(cycles, 0);
    }

    /// The member each of group 0's first modulator's cables addresses (to a plugin of the member or to a modulator of it), in order.
    fn members_driven(model: &SessionModel) -> Vec<usize> {
        model.groups[0].modulators[0].targets.iter().filter_map(|t| match t.kind { K::GroupMember { member, .. } | K::GroupMemberMod { member, .. } => Some(member), _ => None }).collect()
    }

    #[test]
    fn a_member_that_is_put_back_tells_the_cables_of_its_group_to_the_members_from_its_place_on_the_ordinals_they_have_then() {
        use crate::app::testutil::LANES_IN_GROUPS;
        // Group 0 has lanes 0 and 3 (members 0 and 1), and its cables are to member 0 (one) and member 1 (two).
        let mut model = load(LANES_IN_GROUPS).model;
        assert_eq!(members_driven(&model), [0, 1, 1]);
        let mut cables = Vec::new();
        for edge in model.group_cables_into_lane(0) {
            cables.extend(model.remove_edge(edge).into_iter().flatten());
        }
        let lane = model.remove_lane(0).unwrap();
        assert_eq!(members_driven(&model), [0, 0], "the member after it is the first now");
        assert_eq!(model.insert_lane_at(0, lane), 0);
        assert_eq!(members_driven(&model), [1, 1], "and the second again, before the cables of the first member come back");
        for cable in cables.into_iter().rev() {
            model.insert_target_at(cable.at, cable.mod_index, cable.target_index, cable.slot).unwrap();
        }
        assert_eq!(members_driven(&model), [0, 1, 1]);
        // The second member (lane 3) goes with its two cables, and the first member's is not moved, going or coming.
        let mut cables = Vec::new();
        for edge in model.group_cables_into_lane(3) {
            cables.extend(model.remove_edge(edge).into_iter().flatten());
        }
        let lane = model.remove_lane(3).unwrap();
        assert_eq!(members_driven(&model), [0]);
        assert_eq!(model.insert_lane_at(3, lane), 3);
        assert_eq!(members_driven(&model), [0]);
    }

    #[test]
    fn a_lane_that_is_put_back_past_the_end_is_the_last_and_one_for_a_group_that_is_not_there_is_in_no_group() {
        use crate::app::testutil::LANES_IN_GROUPS;
        let mut model = load(LANES_IN_GROUPS).model;
        let nodes = |model: &SessionModel| model.instruments.iter().map(|lane| lane.node).collect::<Vec<_>>();
        let before = nodes(&model);
        let mut cables = Vec::new();
        for edge in model.group_cables_into_lane(0) {
            cables.extend(model.remove_edge(edge).into_iter().flatten());
        }
        let lane = model.remove_lane(0).unwrap();
        // (Past the end is the last: it is the last member of group 0 then, and the cables of the member before it are not moved.)
        assert_eq!(model.insert_lane_at(99, lane), 3);
        assert_eq!(nodes(&model), [before[1], before[2], before[3], before[0]]);
        assert_eq!(members_driven(&model), [0, 0]);
        assert_eq!(model.instruments[3].group, Some(0));
        // A group that is not there: the lane that was in it is in none (here the session's two groups are both gone).
        let lane = model.remove_lane(3).unwrap();
        model.remove_group(1).unwrap();
        model.remove_group(0).unwrap();
        assert_eq!(model.insert_lane_at(0, lane), 0);
        assert_eq!(model.instruments[0].group, None);
    }

    // -- Putting a group back --

    /// Take group `group` out as the controller does (every cable of its modulators first, each with those that drive their depths, and then the group) and put everything back in
    /// the reverse order: the group at `index`, in which the lanes that were its members are put again. False, and not put back, if cables that drive each other's depths went.
    fn group_out_and_back(model: &mut SessionModel, group: usize, index: usize) -> bool {
        let members: Vec<usize> = model.instruments.iter().enumerate().filter(|(_, lane)| lane.group == Some(group)).map(|(inst, _)| inst).collect();
        let edges: Vec<EdgeId> = model.groups[group].modulators.iter().flat_map(|m| m.targets.iter().map(|t| t.edge)).collect();
        let mut cables = Vec::new();
        for edge in edges {
            // (One that an earlier cascade took with it is not there any more.)
            cables.extend(model.remove_edge(edge).into_iter().flatten());
        }
        let removed = model.remove_group(group).unwrap();
        if cables.iter().any(|cable| cable.still_driven) {
            return false;
        }
        assert_eq!(model.insert_group_at(index, removed, &members), index);
        for cable in cables.into_iter().rev() {
            assert!(model.insert_target_at(cable.at, cable.mod_index, cable.target_index, cable.slot).is_some());
        }
        true
    }

    #[test]
    fn every_group_of_every_session_is_put_back_where_it_was_with_its_members_and_its_cables_and_the_lanes_of_the_later_groups_told_their_groups() {
        use crate::app::testutil::{assert_same_model, CHAINS, CROSS_MOD_RACKS, GROUPS, LANES_IN_GROUPS};
        let (mut tried, mut cycles) = (0, 0);
        for (name, session) in [("LONG_CHAINS", LONG_CHAINS), ("CHAINS", CHAINS), ("GROUPS", GROUPS), ("LANES_IN_GROUPS", LANES_IN_GROUPS), ("CROSS_MOD_RACKS", CROSS_MOD_RACKS)] {
            for group in 0..load(session).model.groups.len() {
                // (A session of its own for each: what goes and is not put back would leave the next without it.)
                let mut model = load(session).model;
                if !group_out_and_back(&mut model, group, group) {
                    cycles += 1;
                    continue;
                }
                assert_same_model(&format!("group {group} of {name} taken out and put back"), &load(session).model, &model);
                model.check_ids().unwrap();
                tried += 1;
            }
        }
        assert!(tried > 5, "{tried} groups is not enough to say much");
        assert_eq!(cycles, 0);
    }

    #[test]
    fn the_lanes_are_told_the_groups_they_are_in_while_a_group_is_out_and_when_it_is_back() {
        use crate::app::testutil::LANES_IN_GROUPS;
        // Lanes 0 and 3 are in group 0, lane 2 is in group 1, and lane 1 is in none.
        let mut model = load(LANES_IN_GROUPS).model;
        let groups_of = |model: &SessionModel| model.instruments.iter().map(|lane| lane.group).collect::<Vec<_>>();
        assert_eq!(groups_of(&model), [Some(0), None, Some(1), Some(0)]);
        for cable in model.groups[0].modulators.iter().flat_map(|m| m.targets.iter().map(|t| t.edge)).collect::<Vec<_>>() {
            model.remove_edge(cable);
        }
        let removed = model.remove_group(0).unwrap();
        assert_eq!(groups_of(&model), [None, None, Some(0), None], "its members are in none, and the lane of the group after it is in the one before");
        // It comes back first: the lane of the group after it is in the one after it again, and its members are in it again.
        assert_eq!(model.insert_group_at(0, removed, &[0, 3]), 0);
        assert_eq!(groups_of(&model), [Some(0), None, Some(1), Some(0)]);
        assert_eq!(model.groups.len(), 2);
    }

    #[test]
    fn a_group_that_is_put_back_past_the_end_is_the_last_and_a_member_that_is_no_lane_is_nobodys() {
        use crate::app::testutil::LANES_IN_GROUPS;
        let mut model = load(LANES_IN_GROUPS).model;
        let nodes = |model: &SessionModel| model.groups.iter().map(|group| group.node).collect::<Vec<_>>();
        let before = nodes(&model);
        for cable in model.groups[0].modulators.iter().flat_map(|m| m.targets.iter().map(|t| t.edge)).collect::<Vec<_>>() {
            model.remove_edge(cable);
        }
        let removed = model.remove_group(0).unwrap();
        // (Put at a place past the end it is the last, group 1 of two; lane 9 is no lane.)
        assert_eq!(model.insert_group_at(99, removed, &[0, 3, 9]), 1);
        assert_eq!(nodes(&model), [before[1], before[0]]);
        assert_eq!(model.instruments.iter().map(|lane| lane.group).collect::<Vec<_>>(), [Some(1), None, Some(0), Some(1)]);
    }

    // -- Putting a modulator back --

    /// Every modulator of the model as it is written down (its node, source, cables and the slot it publishes to): the lanes', then the groups'.
    fn modulators_of(model: &SessionModel) -> Vec<String> {
        let lanes = model.instruments.iter().flat_map(|lane| lane.modulators.iter());
        let groups = model.groups.iter().flat_map(|group| group.modulators.iter());
        lanes.chain(groups).map(|modulator| format!("{modulator:?}")).collect()
    }

    /// Take modulator `index` of the rack at `at` out as the controller does (the cables that address it first, each with the cables that drive its
    /// depth, and then the modulator with its own cables) and put everything back in the reverse order. False, and not put back, if cables that drive
    /// each other's depths went (`RemovedCable::still_driven`).
    fn modulator_out_and_back(model: &mut SessionModel, at: ModulatorLocation, index: usize) -> bool {
        let mut cables = Vec::new();
        for edge in model.cables_into(at, index) {
            // (One that an earlier cascade took with it is not there any more.)
            cables.extend(model.remove_edge(edge).into_iter().flatten());
        }
        let modulator = model.remove_modulator_at(at, index).unwrap();
        if cables.iter().any(|cable| cable.still_driven) {
            return false;
        }
        assert_eq!(model.insert_modulator_at(at, index, modulator), Some(index));
        for cable in cables.into_iter().rev() {
            assert!(model.insert_target_at(cable.at, cable.mod_index, cable.target_index, cable.slot).is_some());
        }
        true
    }

    #[test]
    fn every_modulator_of_every_session_is_put_back_where_it_was_with_what_drove_it_and_what_it_drove() {
        use crate::app::testutil::{CROSS_MOD, CROSS_MOD_RACKS, LANES_IN_GROUPS};
        let sessions = [CABLES_THAT_DRIVE_CABLES, CABLES_THAT_DRIVE_EACH_OTHER, CROSS_MOD, CROSS_MOD_RACKS, LANES_IN_GROUPS];
        let (mut tried, mut cycles) = (0, 0);
        for (n, session) in sessions.into_iter().enumerate() {
            let model = load(session).model;
            let lanes = model.instruments.iter().enumerate().map(|(i, lane)| (ModulatorLocation::Lane(i), lane.modulators.len()));
            let groups = model.groups.iter().enumerate().map(|(i, group)| (ModulatorLocation::Group(i), group.modulators.len()));
            for (at, count) in lanes.chain(groups).collect::<Vec<_>>() {
                for index in 0..count {
                    // (A session of its own for each: what goes and is not put back would leave the next without it.)
                    let mut model = load(session).model;
                    let before = modulators_of(&model);
                    if !modulator_out_and_back(&mut model, at, index) {
                        cycles += 1;
                        continue;
                    }
                    assert_eq!(modulators_of(&model), before, "session {n}: modulator {index} of {at:?} taken out and put back");
                    model.check_ids().unwrap();
                    tried += 1;
                }
            }
        }
        assert!(tried > 20, "{tried} modulators is not enough to say much");
        assert_eq!(cycles, 2, "the two modulators whose cables drive each other's depths are the only ones that are not put back");
    }

    #[test]
    fn a_modulator_that_is_taken_out_is_handed_back_and_the_cables_that_address_the_others_are_told_their_places() {
        let mut model = load(CABLES_THAT_DRIVE_CABLES).model;
        let first = model.instruments[0].modulators[0].node;
        // (Nothing took out the cables that address it: the second modulator's two cables to the depths of the first's go with it.)
        let removed = model.remove_modulator_at(ModulatorLocation::Lane(0), 0).unwrap();
        assert_eq!((removed.node, removed.targets.len()), (first, 3));
        let rack = &model.instruments[0].modulators;
        assert_eq!(rack.len(), 2);
        assert_eq!(cables(rack)[0], [], "the second modulator drove only the first's cables, and is the first now");
        assert_eq!(cables(rack)[1], [(depth_of(0, 0), 0.6)], "the third drove the second's first cable: it is the first modulator's now");
        assert!(model.remove_modulator_at(ModulatorLocation::Lane(0), 2).is_none(), "it has two now");
        assert!(model.remove_modulator_at(ModulatorLocation::Lane(0), 5).is_none());
        assert!(model.remove_modulator_at(ModulatorLocation::Lane(3), 0).is_none());
        assert!(model.remove_modulator_at(ModulatorLocation::Group(0), 0).is_none());
    }

    #[test]
    fn the_check_wants_a_position_slot_for_a_step_sequencer_and_for_no_other_modulator() {
        let session = "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\ntype = \"steps\"\n[[instrument.modulator]]\n";
        let mut model = crate::app::testutil::load(session).model;
        model.check_ids().unwrap();
        let (output, position) = (model.instruments[0].modulators[0].output, model.instruments[0].modulators[0].position);
        assert!(position.is_some());

        model.instruments[0].modulators[0].position = None;
        assert_eq!(model.check_ids().unwrap_err(), "the position of modulator 0 of lane 0 has no telemetry slot");

        model.instruments[0].modulators[0].position = position;
        model.instruments[0].modulators[1].position = position;
        assert_eq!(model.check_ids().unwrap_err(), "modulator 1 of lane 0 is not a step sequencer and has a position");

        model.instruments[0].modulators[1].position = None;
        model.instruments[0].modulators[0].position = output;
        assert!(model.check_ids().unwrap_err().ends_with("which another node has too"), "a position is not its modulator's output");
    }

    /// A modulator to put in, told apart by its node, with one cable to the rate of the modulator at index 0.
    fn spare_modulator(node: u32) -> ModulatorSlot {
        let mut cable = spare_cable(900 + node);
        cable.kind = K::ModulatorRate { mod_index: 0 };
        ModulatorSlot { node: NodeId(node), source: ModSourceSlot::Lfo { waveform: crate::plugin::chain::LfoWaveform::Sine, rate: 9.0, retrigger: false, fade: 0.0, sync: 0 }, targets: vec![cable], output: None, position: None }
    }

    #[test]
    fn a_modulator_is_put_in_at_the_place_it_is_given_or_the_last_and_not_at_all_where_there_is_no_rack() {
        let mut model = load(CABLES_THAT_DRIVE_CABLES).model;
        assert_eq!(model.insert_modulator_at(ModulatorLocation::Lane(0), 1, spare_modulator(800)), Some(1));
        let nodes = |model: &SessionModel| model.instruments[0].modulators.iter().map(|m| m.node.0).collect::<Vec<_>>();
        assert_eq!(nodes(&model)[1], 800);
        assert_eq!(nodes(&model).len(), 4);
        // The old second is the third now, and what drove its cables' depths (the old third's) addresses it there.
        assert_eq!(cables(&model.instruments[0].modulators)[3], [(depth_of(2, 0), 0.6)]);
        // What drove the first modulator still does.
        assert_eq!(cables(&model.instruments[0].modulators)[2], [(depth_of(0, 0), 0.4), (depth_of(0, 2), 0.5)]);
        assert_eq!(model.insert_modulator_at(ModulatorLocation::Lane(0), 99, spare_modulator(801)), Some(4), "past the end is the last place");
        assert_eq!(nodes(&model).last(), Some(&801));
        assert_eq!(cables(&model.instruments[0].modulators)[3], [(depth_of(2, 0), 0.6)], "and moves nothing");
        for at in [ModulatorLocation::Lane(5), ModulatorLocation::Group(0)] {
            assert_eq!(model.insert_modulator_at(at, 0, spare_modulator(802)), None, "{at:?}");
        }
        assert_eq!(nodes(&model).len(), 5);
    }

    #[test]
    fn a_modulator_that_goes_in_at_the_end_moves_nothing_whatever_the_cables_address_as_the_audio_thread_does() {
        let mut model = load(CABLES_THAT_DRIVE_CABLES).model;
        // (A cable to a modulator that is not there: what is filled a modulator at a time has, and the audio thread is loaded that way.)
        model.instruments[0].modulators[0].targets[0].kind = K::ModulatorRate { mod_index: 3 };
        assert_eq!(model.insert_modulator_at(ModulatorLocation::Lane(0), 3, spare_modulator(800)), Some(3));
        assert_eq!(model.instruments[0].modulators[0].targets[0].kind, K::ModulatorRate { mod_index: 3 }, "it addresses the one that came");
        // One that goes in before the one it addresses moves it.
        assert_eq!(model.insert_modulator_at(ModulatorLocation::Lane(0), 3, spare_modulator(801)), Some(3));
        assert_eq!(model.instruments[0].modulators[0].targets[0].kind, K::ModulatorRate { mod_index: 4 });
        // The same goes for the cables of a group to the modulators of a member that is not there yet.
        let mut model = load(crate::app::testutil::CROSS_MOD).model;
        let count = model.instruments[0].modulators.len();
        let to_come = K::GroupMemberMod { member: 0, mod_index: count, field: CrossModField::Rate };
        model.groups[0].modulators[1].targets[0].kind = to_come.clone();
        assert_eq!(model.insert_modulator_at(ModulatorLocation::Lane(0), count, spare_modulator(802)), Some(count));
        assert_eq!(model.groups[0].modulators[1].targets[0].kind, to_come);
        assert_eq!(model.insert_modulator_at(ModulatorLocation::Lane(0), 0, spare_modulator(803)), Some(0));
        assert_eq!(model.groups[0].modulators[1].targets[0].kind, K::GroupMemberMod { member: 0, mod_index: count + 1, field: CrossModField::Rate });
    }

    #[test]
    fn a_modulator_that_goes_in_keeps_the_cables_it_has_though_they_address_places_of_its_rack() {
        let mut model = load(CABLES_THAT_DRIVE_CABLES).model;
        // Its cable is to the rate of the modulator at index 0: the one that is at index 0 once it has gone in is its own sibling, and the cable is not told.
        assert_eq!(model.insert_modulator_at(ModulatorLocation::Lane(0), 0, spare_modulator(800)), Some(0));
        assert_eq!(model.instruments[0].modulators[0].targets[0].kind, K::ModulatorRate { mod_index: 0 }, "it is not told a place it was not at");
        assert_eq!(cables(&model.instruments[0].modulators)[2], [(depth_of(1, 0), 0.4), (depth_of(1, 2), 0.5)], "the others are");
    }

    #[test]
    fn a_members_modulator_that_goes_in_tells_the_groups_cables_to_the_modulators_of_that_member_after_it_and_no_others() {
        use crate::app::testutil::CROSS_MOD;
        use crate::plugin::chain::CrossModField as F;
        let mut model = load(CROSS_MOD).model;
        let drive = |mod_index, field| K::GroupMemberMod { member: 0, mod_index, field };
        let in_the_group = |model: &SessionModel| model.groups[0].modulators[1].targets.iter().map(|t| t.kind.clone()).collect::<Vec<_>>();
        // A cable to a modulator of another member (this session has one member: the model does not mind) is not what the first member's modulators move.
        let other_members = K::GroupMemberMod { member: 1, mod_index: 1, field: F::Rate };
        model.groups[0].modulators[1].targets[1].kind = other_members.clone();
        let before = in_the_group(&model);
        assert!(before.contains(&drive(0, F::Rate)) && before.contains(&drive(1, F::Attack)) && before.contains(&drive(0, F::Depth(0))), "{before:?}");
        // The member (lane 0) gets a modulator at its second place: the one that was the second is the third.
        assert_eq!(model.insert_modulator_at(ModulatorLocation::Lane(0), 1, spare_modulator(800)), Some(1));
        let after = in_the_group(&model);
        assert!(after.contains(&drive(0, F::Rate)) && after.contains(&drive(0, F::Depth(0))), "what drove the first is as it was: {after:?}");
        assert!(after.contains(&drive(2, F::Attack)) && !after.contains(&drive(1, F::Attack)), "what drove the second drives the third: {after:?}");
        assert!(after.contains(&other_members), "what drove another member's second modulator still does: {after:?}");
        // A group's modulator that goes in tells the cables of the group's own modulators to their siblings: the LFO drove the attack of the envelope before it,
        // which is the second modulator now, and the LFO is the third.
        assert_eq!(model.insert_modulator_at(ModulatorLocation::Group(0), 0, spare_modulator(801)), Some(0));
        assert_eq!(model.groups[0].modulators[2].targets[0].kind, K::ModulatorAttack { mod_index: 1 });
    }

    #[test]
    fn cables_that_drive_each_others_depths_are_all_taken_out_and_it_ends() {
        let mut model = load(CABLES_THAT_DRIVE_EACH_OTHER).model;
        let first = edge_of(&model, 0, 0, 0);
        let removals = model.remove_edge(first).unwrap();
        // Each drives the depth of the other: the second goes first, and then the cable itself. What was after it is
        // the modulator's first cable now, and drove nothing of the others.
        assert_eq!(places(&removals), [(ModulatorLocation::Lane(0), 1, 0), (ModulatorLocation::Lane(0), 0, 0)]);
        assert!(model.instruments[0].modulators[1].targets.is_empty());
        assert_eq!(model.instruments[0].modulators[0].targets.len(), 1, "the parameter it drives is left");
    }

    #[test]
    fn a_cable_that_is_not_there_is_not_taken_out_and_nothing_changes() {
        let mut model = load(CABLES_THAT_DRIVE_CABLES).model;
        let before = cables(&model.instruments[0].modulators);
        assert!(model.remove_edge(EdgeId(999)).is_none());
        let gone = edge_of(&model, 0, 0, 1);
        assert!(model.remove_edge(gone).is_some());
        assert!(model.remove_edge(gone).is_none(), "it is not there the second time");
        assert_ne!(cables(&model.instruments[0].modulators), before, "(the first time did change it)");
    }

    // -- Lanes change places --

    /// The member each cable of the first group's first modulator drives, in order (a plugin's parameter or a
    /// modulator's rate), and the group of each lane.
    fn members_and_groups(model: &SessionModel) -> (Vec<usize>, Vec<Option<usize>>) {
        let members = model.groups[0].modulators[0]
            .targets
            .iter()
            .map(|t| match t.kind {
                K::GroupMember { member, .. } | K::GroupMemberMod { member, .. } => member,
                ref other => panic!("{other:?}"),
            })
            .collect();
        (members, model.instruments.iter().map(|lane| lane.group).collect())
    }

    #[test]
    fn two_lanes_that_swap_places_each_take_what_it_has_and_the_groups_cables_follow_the_members() {
        use crate::app::testutil::LANES_IN_GROUPS;
        let mut model = load(LANES_IN_GROUPS).model;
        // Group 0's members are lanes 0 and 3 (members 0 and 1); its modulator drives parameters of both, and the
        // rate of the second's first modulator.
        assert_eq!(members_and_groups(&model), (vec![0, 1, 1], vec![Some(0), None, Some(1), Some(0)]));
        let (ids, volumes): (Vec<NodeId>, Vec<f32>) = model.instruments.iter().map(|lane| (lane.node, lane.volume)).unzip();

        // Two members change places: so do the members the cables drive.
        assert!(model.swap_lanes(0, 3));
        assert_eq!(members_and_groups(&model), (vec![1, 0, 0], vec![Some(0), None, Some(1), Some(0)]));
        assert_eq!(model.instruments.iter().map(|lane| lane.node).collect::<Vec<_>>(), [ids[3], ids[1], ids[2], ids[0]], "each lane is its own node, and moved");
        assert_eq!(model.instruments.iter().map(|lane| lane.volume).collect::<Vec<_>>(), [volumes[3], volumes[1], volumes[2], volumes[0]]);
        assert_eq!(model.instruments[3].range, Some((12, 59)), "the range of the lane went with it");
        assert_eq!(model.instruments[0].effects.len(), 1);
        assert_eq!(model.locate(ids[0]), Some(NodePath::Instrument(3)), "the node is found where the lane is");
        model.check_ids().unwrap();

        // A member and a lane in no group: the member's place among the members does not change, the lane's group goes with it.
        assert!(model.swap_lanes(0, 1));
        assert_eq!(members_and_groups(&model), (vec![1, 0, 0], vec![None, Some(0), Some(1), Some(0)]));
        // A member goes past the other group's lane and then past the other member: they change places again.
        assert!(model.swap_lanes(1, 3));
        assert_eq!(members_and_groups(&model), (vec![0, 1, 1], vec![None, Some(0), Some(1), Some(0)]));
        model.check_ids().unwrap();
    }

    #[test]
    fn lanes_that_are_not_there_or_are_one_do_not_swap_and_nothing_changes() {
        use crate::app::testutil::LANES_IN_GROUPS;
        let mut model = load(LANES_IN_GROUPS).model;
        let before = format!("{:?}", model.instruments.iter().map(|lane| (lane.node, lane.group)).collect::<Vec<_>>());
        for (a, b) in [(1, 1), (0, 4), (7, 0), (9, 9)] {
            assert!(!model.swap_lanes(a, b), "{a} {b}");
        }
        assert_eq!(format!("{:?}", model.instruments.iter().map(|lane| (lane.node, lane.group)).collect::<Vec<_>>()), before);
        assert_eq!(members_and_groups(&model).0, [0, 1, 1]);
    }

    #[test]
    fn a_member_with_no_plugin_is_still_a_member_and_keeps_its_place() {
        let mut model = load("[[group]]\n[[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\n[[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\n").model;
        model.instruments[0].instrument = None;
        let second = model.instruments[1].node;
        assert_eq!(plugins_driven(&model, ModulatorLocation::Group(0)), [(second, K::GroupMember { member: 1, slot: 0, param_index: 0 })]);
    }

    #[test]
    fn what_is_not_there_drives_nothing() {
        let mut model = load("[[instrument]]\nplugin = \"builtin:osc\"\n").model;
        assert!(model.param_targets(ModulatorLocation::Lane(1)).is_empty(), "no such lane");
        assert!(model.param_targets(ModulatorLocation::Group(0)).is_empty(), "no such group");
        model.instruments[0].instrument = None;
        assert!(model.param_targets(ModulatorLocation::Lane(0)).is_empty(), "an empty lane has no chain");
        // A lane that says it is in a group that is not there does not make one.
        model.instruments[0].instrument = load("[[instrument]]\nplugin = \"builtin:osc\"\n").model.instruments.remove(0).instrument;
        model.instruments[0].group = Some(3);
        assert!(model.param_targets(ModulatorLocation::Group(3)).is_empty(), "no such group, whatever the lanes say");
    }

    #[test]
    fn a_separator_is_not_a_parameter_to_drive() {
        let mut model = load("[[instrument]]\nplugin = \"builtin:osc\"\n").model;
        let plugin = model.instruments[0].instrument.as_mut().unwrap();
        let all = plugin.params.len();
        plugin.params[0].kind = ParamKind::Separator;
        assert_eq!(model.param_targets(ModulatorLocation::Lane(0)).len(), all - 1);
    }

    fn target(kind: K) -> ModTargetSlot {
        ModTargetSlot {
            edge: EdgeId(0),
            slot: 0,
            param_name: "p".to_string(),
            kind,
            depth: 0.5,
            param_min: 0.0,
            param_max: 1.0,
            effective: None,
        }
    }

    fn lfo(targets: Vec<ModTargetSlot>) -> ModulatorSlot {
        ModulatorSlot {
            node: NodeId(0),
            source: ModSourceSlot::Lfo { waveform: LfoWaveform::Sine, rate: 2.0, retrigger: false, fade: 0.0, sync: 0 },
            targets,
            output: None,
            position: None,
        }
    }

    fn adsr(targets: Vec<ModTargetSlot>) -> ModulatorSlot {
        ModulatorSlot {
            node: NodeId(0),
            source: ModSourceSlot::Envelope { attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 },
            targets,
            output: None,
            position: None,
        }
    }

    fn midi(targets: Vec<ModTargetSlot>) -> ModulatorSlot {
        ModulatorSlot {
            node: NodeId(0),
            source: ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Cc(74)), learning: false },
            targets,
            output: None,
            position: None,
        }
    }

    /// A target list as readable text (`ModTargetKind` has no `PartialEq`).
    fn kinds(m: &ModulatorSlot) -> Vec<String> {
        m.targets.iter().map(|t| format!("{:?}", t.kind)).collect()
    }

    /// One target of every kind that is *not* in the index space under test, to
    /// prove the fix-ups leave them alone.
    fn bystanders() -> Vec<ModTargetSlot> {
        vec![
            target(K::PluginParam { slot: 1, param_index: 7 }),
            target(K::GroupMember { member: 3, slot: 0, param_index: 1 }),
            target(K::GroupBus { effect_index: 2, param_index: 0 }),
            target(K::GroupMemberMod { member: 0, mod_index: 0, field: CrossModField::Rate }),
        ]
    }

    #[test]
    fn cross_mod_fixup_drops_targets_on_the_removed_modulator_and_shifts_higher_ones() {
        // The list *after* modulator 0 was removed: every sibling target that
        // pointed at index 0 is gone, indices above 0 move down one.
        let mut targets = vec![
            target(K::ModulatorRate { mod_index: 0 }),
            target(K::ModulatorAttack { mod_index: 2 }),
            target(K::ModulatorDecay { mod_index: 1 }),
            target(K::ModulatorSustain { mod_index: 3 }),
            target(K::ModulatorRelease { mod_index: 0 }),
            target(K::ModulatorDepth { mod_index: 2, target_index: 4 }),
        ];
        targets.extend(bystanders());
        let mut mods = vec![lfo(targets), adsr(vec![target(K::ModulatorRate { mod_index: 1 })])];
        fixup_cross_mod_after_remove(&mut mods, 0);
        assert_eq!(
            kinds(&mods[0]),
            [
                "ModulatorAttack { mod_index: 1 }",
                "ModulatorDecay { mod_index: 0 }",
                "ModulatorSustain { mod_index: 2 }",
                "ModulatorDepth { mod_index: 1, target_index: 4 }",
                "PluginParam { slot: 1, param_index: 7 }",
                "GroupMember { member: 3, slot: 0, param_index: 1 }",
                "GroupBus { effect_index: 2, param_index: 0 }",
                "GroupMemberMod { member: 0, mod_index: 0, field: Rate }",
            ]
        );
        assert_eq!(kinds(&mods[1]), ["ModulatorRate { mod_index: 0 }"], "other racks' targets shift too");
    }

    #[test]
    fn cross_mod_fixup_removing_the_last_modulator_leaves_lower_indices_alone() {
        let mut mods = vec![lfo(vec![
            target(K::ModulatorRate { mod_index: 0 }),
            target(K::ModulatorRate { mod_index: 1 }),
            target(K::ModulatorRate { mod_index: 2 }),
        ])];
        fixup_cross_mod_after_remove(&mut mods, 2);
        assert_eq!(kinds(&mods[0]), ["ModulatorRate { mod_index: 0 }", "ModulatorRate { mod_index: 1 }"]);
    }

    #[test]
    fn group_bus_fixup_drops_targets_on_the_removed_effect_and_shifts_higher_ones() {
        let mut targets = vec![
            target(K::GroupBus { effect_index: 0, param_index: 1 }),
            target(K::GroupBus { effect_index: 1, param_index: 2 }),
            target(K::GroupBus { effect_index: 2, param_index: 3 }),
            target(K::PluginParam { slot: 2, param_index: 7 }),
            target(K::GroupMember { member: 1, slot: 2, param_index: 4 }),
        ];
        targets.push(target(K::ModulatorRate { mod_index: 2 }));
        let mut mods = vec![lfo(targets)];
        fixup_group_bus_after_remove(&mut mods, 1);
        assert_eq!(
            kinds(&mods[0]),
            [
                "GroupBus { effect_index: 0, param_index: 1 }",
                "GroupBus { effect_index: 1, param_index: 3 }",
                "PluginParam { slot: 2, param_index: 7 }",
                "GroupMember { member: 1, slot: 2, param_index: 4 }",
                "ModulatorRate { mod_index: 2 }",
            ]
        );
    }

    #[test]
    fn group_member_removal_fixup_drops_targets_on_the_member_and_shifts_higher_ordinals() {
        let mut targets = vec![
            target(K::GroupMember { member: 0, slot: 0, param_index: 1 }),
            target(K::GroupMember { member: 1, slot: 0, param_index: 2 }),
            target(K::GroupMember { member: 2, slot: 1, param_index: 3 }),
            target(K::GroupMemberMod { member: 1, mod_index: 0, field: CrossModField::Rate }),
            target(K::GroupMemberMod { member: 2, mod_index: 4, field: CrossModField::Depth(1) }),
        ];
        targets.push(target(K::GroupBus { effect_index: 1, param_index: 0 }));
        targets.push(target(K::ModulatorRate { mod_index: 1 }));
        let mut mods = vec![lfo(targets)];
        fixup_group_member_after_remove(&mut mods, 1);
        assert_eq!(
            kinds(&mods[0]),
            [
                "GroupMember { member: 0, slot: 0, param_index: 1 }",
                "GroupMember { member: 1, slot: 1, param_index: 3 }",
                "GroupMemberMod { member: 1, mod_index: 4, field: Depth(1) }",
                "GroupBus { effect_index: 1, param_index: 0 }",
                "ModulatorRate { mod_index: 1 }",
            ]
        );
    }

    #[test]
    fn group_member_insert_shift_bumps_ordinals_at_and_after_the_new_member() {
        let mut targets = vec![
            target(K::GroupMember { member: 0, slot: 0, param_index: 1 }),
            target(K::GroupMember { member: 1, slot: 0, param_index: 2 }),
            target(K::GroupMember { member: 2, slot: 1, param_index: 3 }),
            target(K::GroupMemberMod { member: 0, mod_index: 0, field: CrossModField::Rate }),
            target(K::GroupMemberMod { member: 1, mod_index: 4, field: CrossModField::Release }),
        ];
        targets.push(target(K::GroupBus { effect_index: 1, param_index: 0 }));
        targets.push(target(K::ModulatorRate { mod_index: 1 }));
        let mut mods = vec![lfo(targets)];
        shift_group_member_after_insert(&mut mods, 1);
        assert_eq!(
            kinds(&mods[0]),
            [
                "GroupMember { member: 0, slot: 0, param_index: 1 }",
                "GroupMember { member: 2, slot: 0, param_index: 2 }",
                "GroupMember { member: 3, slot: 1, param_index: 3 }",
                "GroupMemberMod { member: 0, mod_index: 0, field: Rate }",
                "GroupMemberMod { member: 2, mod_index: 4, field: Release }",
                "GroupBus { effect_index: 1, param_index: 0 }",
                "ModulatorRate { mod_index: 1 }",
            ]
        );
        // Joining at the very front shifts everyone; joining after the last member shifts no one.
        shift_group_member_after_insert(&mut mods, 0);
        assert!(kinds(&mods[0])[0].starts_with("GroupMember { member: 1,"));
        let before = kinds(&mods[0]);
        shift_group_member_after_insert(&mut mods, 99);
        assert_eq!(kinds(&mods[0]), before);
    }

    #[test]
    fn group_member_mod_fixup_only_touches_that_members_modulators() {
        let mut mods = vec![lfo(vec![
            target(K::GroupMemberMod { member: 1, mod_index: 0, field: CrossModField::Rate }),
            target(K::GroupMemberMod { member: 1, mod_index: 1, field: CrossModField::Attack }),
            target(K::GroupMemberMod { member: 1, mod_index: 2, field: CrossModField::Depth(0) }),
            target(K::GroupMemberMod { member: 0, mod_index: 1, field: CrossModField::Rate }),
            target(K::GroupMemberMod { member: 0, mod_index: 3, field: CrossModField::Release }),
            target(K::GroupMember { member: 1, slot: 0, param_index: 5 }),
            target(K::ModulatorRate { mod_index: 1 }),
        ])];
        fixup_group_member_mod_after_remove(&mut mods, 1, 1);
        // Member 1's modulator 1 is gone and its modulator 2 became 1; member 0's
        // modulators (same index, and a higher one) and every other kind stay put.
        assert_eq!(
            kinds(&mods[0]),
            [
                "GroupMemberMod { member: 1, mod_index: 0, field: Rate }",
                "GroupMemberMod { member: 1, mod_index: 1, field: Depth(0) }",
                "GroupMemberMod { member: 0, mod_index: 1, field: Rate }",
                "GroupMemberMod { member: 0, mod_index: 3, field: Release }",
                "GroupMember { member: 1, slot: 0, param_index: 5 }",
                "ModulatorRate { mod_index: 1 }",
            ]
        );
    }

    /// A lane modulator target on chain `slot` (0 = instrument, 1.. = effects).
    fn plugin_target(slot: usize, param_index: u32) -> ModTargetSlot {
        ModTargetSlot { slot, ..target(K::PluginParam { slot, param_index }) }
    }

    /// Each target's own `slot` field and its kind, as the tree and the save code see them.
    fn slots(m: &ModulatorSlot) -> Vec<(usize, String)> {
        m.targets.iter().map(|t| (t.slot, format!("{:?}", t.kind))).collect()
    }

    #[test]
    fn slot_fixup_after_effect_removal_drops_that_slot_and_shifts_later_ones() {
        // Chain: instrument, then effects 0, 1, 2 at slots 1, 2, 3. Effect 0 is removed.
        let mut mods = vec![lfo(vec![
            plugin_target(0, 1),
            plugin_target(1, 5),
            plugin_target(2, 6),
            plugin_target(3, 7),
            target(K::ModulatorRate { mod_index: 1 }),
            target(K::GroupMember { member: 0, slot: 2, param_index: 4 }),
        ])];
        fixup_slots_after_remove(&mut mods, 0);
        assert_eq!(
            slots(&mods[0]),
            [
                (0, "PluginParam { slot: 0, param_index: 1 }".to_string()),
                (1, "PluginParam { slot: 1, param_index: 6 }".to_string()),
                (2, "PluginParam { slot: 2, param_index: 7 }".to_string()),
                (0, "ModulatorRate { mod_index: 1 }".to_string()),
                (0, "GroupMember { member: 0, slot: 2, param_index: 4 }".to_string()),
            ]
        );
        // Removing the last effect only drops its own targets.
        fixup_slots_after_remove(&mut mods, 1);
        assert_eq!(
            slots(&mods[0]).iter().map(|(slot, _)| *slot).collect::<Vec<_>>(),
            [0, 1, 0, 0],
        );
    }

    #[test]
    fn slot_remap_after_reorder_follows_the_plugins() {
        // Effects A, B, C at slots 1, 2, 3, each with one target; A moves to the end.
        let mut mods = vec![lfo(vec![
            plugin_target(0, 9),
            plugin_target(1, 10),
            plugin_target(2, 11),
            plugin_target(3, 12),
            target(K::ModulatorRate { mod_index: 0 }),
        ])];
        remap_slots_after_reorder(&mut mods, 0, 2);
        let slot_of = |m: &ModulatorSlot| m.targets.iter().map(|t| t.slot).collect::<Vec<_>>();
        assert_eq!(slot_of(&mods[0]), [0, 3, 1, 2, 0], "A -> 3, B -> 1, C -> 2");
        // The kind agrees with the target's own slot, and the parameters are unchanged.
        assert_eq!(
            kinds(&mods[0])[1..4],
            [
                "PluginParam { slot: 3, param_index: 10 }",
                "PluginParam { slot: 1, param_index: 11 }",
                "PluginParam { slot: 2, param_index: 12 }",
            ]
        );
        // Moving it back restores the original slots.
        remap_slots_after_reorder(&mut mods, 2, 0);
        assert_eq!(slot_of(&mods[0]), [0, 1, 2, 3, 0]);
        // Moving an effect onto itself changes nothing.
        remap_slots_after_reorder(&mut mods, 1, 1);
        assert_eq!(slot_of(&mods[0]), [0, 1, 2, 3, 0]);
    }

    /// A group target on member `member`'s chain `slot`, as the target selector makes it.
    fn member_target(member: usize, slot: usize, param_index: u32) -> ModTargetSlot {
        ModTargetSlot { slot, ..target(K::GroupMember { member, slot, param_index }) }
    }

    #[test]
    fn member_slot_fixup_after_effect_removal_touches_only_that_members_slots() {
        // Member 0 has effects at slots 1..=3, member 1 one at slot 1. Effect 0 of member 0 is removed.
        let mut mods = vec![lfo(vec![
            member_target(0, 0, 1),
            member_target(0, 1, 5),
            member_target(0, 2, 6),
            member_target(0, 3, 7),
            member_target(1, 1, 8),
            member_target(1, 2, 9),
            plugin_target(2, 4),
            target(K::GroupBus { effect_index: 2, param_index: 3 }),
        ])];
        fixup_member_slots_after_remove(&mut mods, 0, 0);
        assert_eq!(
            slots(&mods[0]),
            [
                (0, "GroupMember { member: 0, slot: 0, param_index: 1 }".to_string()),
                (1, "GroupMember { member: 0, slot: 1, param_index: 6 }".to_string()),
                (2, "GroupMember { member: 0, slot: 2, param_index: 7 }".to_string()),
                (1, "GroupMember { member: 1, slot: 1, param_index: 8 }".to_string()),
                (2, "GroupMember { member: 1, slot: 2, param_index: 9 }".to_string()),
                (2, "PluginParam { slot: 2, param_index: 4 }".to_string()),
                (0, "GroupBus { effect_index: 2, param_index: 3 }".to_string()),
            ],
            "the removed slot's target is gone, later slots of that member move down, nothing else does"
        );
        // Removing what is now the last effect of member 0 only drops its own targets.
        fixup_member_slots_after_remove(&mut mods, 0, 1);
        assert_eq!(
            kinds(&mods[0]),
            [
                "GroupMember { member: 0, slot: 0, param_index: 1 }",
                "GroupMember { member: 0, slot: 1, param_index: 6 }",
                "GroupMember { member: 1, slot: 1, param_index: 8 }",
                "GroupMember { member: 1, slot: 2, param_index: 9 }",
                "PluginParam { slot: 2, param_index: 4 }",
                "GroupBus { effect_index: 2, param_index: 3 }",
            ]
        );
        // The other racks' targets move too.
        let mut racks = vec![lfo(vec![member_target(0, 2, 1)]), adsr(vec![member_target(0, 3, 2)])];
        fixup_member_slots_after_remove(&mut racks, 0, 0);
        assert_eq!((racks[0].targets[0].slot, racks[1].targets[0].slot), (1, 2));
    }

    #[test]
    fn member_slot_remap_after_reorder_follows_the_plugins_of_that_member() {
        // Member 0's effects A, B, C at slots 1, 2, 3, each with a target; A moves to the end.
        let mut mods = vec![lfo(vec![
            member_target(0, 0, 9),
            member_target(0, 1, 10),
            member_target(0, 2, 11),
            member_target(0, 3, 12),
            member_target(1, 1, 13),
            member_target(1, 2, 14),
            plugin_target(1, 15),
        ])];
        remap_member_slots_after_reorder(&mut mods, 0, 0, 2);
        let slot_of = |m: &ModulatorSlot| m.targets.iter().map(|t| t.slot).collect::<Vec<_>>();
        assert_eq!(slot_of(&mods[0]), [0, 3, 1, 2, 1, 2, 1], "A -> 3, B -> 1, C -> 2; member 1 and the lane's own stay");
        // The kind agrees with the target's own slot, and the parameters are unchanged.
        assert_eq!(
            kinds(&mods[0])[1..4],
            [
                "GroupMember { member: 0, slot: 3, param_index: 10 }",
                "GroupMember { member: 0, slot: 1, param_index: 11 }",
                "GroupMember { member: 0, slot: 2, param_index: 12 }",
            ]
        );
        // Moving it back restores the original slots; moving an effect onto itself changes nothing.
        remap_member_slots_after_reorder(&mut mods, 0, 2, 0);
        assert_eq!(slot_of(&mods[0]), [0, 1, 2, 3, 1, 2, 1]);
        remap_member_slots_after_reorder(&mut mods, 0, 1, 1);
        assert_eq!(slot_of(&mods[0]), [0, 1, 2, 3, 1, 2, 1]);
        // The other way: the last effect moves to the front.
        remap_member_slots_after_reorder(&mut mods, 0, 2, 0);
        assert_eq!(slot_of(&mods[0]), [0, 2, 3, 1, 1, 2, 1], "C -> 1, A -> 2, B -> 3");
    }

    /// An ungrouped lane with a filter and a reverb, then a group member (lane 1,
    /// member 0) with the same, driven by a lane LFO and by a group LFO that each
    /// have a target on both: told apart by parameter index, `cutoff` (0) is the
    /// filter's and `damping` (2) the reverb's.
    const MEMBER_WITH_EFFECTS: &str = r#"
[[group]]
[[group.modulator]]
rate = 0.5
[[group.modulator.target]]
member = 0
slot = 1
param = "cutoff"
depth = 0.4
[[group.modulator.target]]
member = 0
slot = 2
param = "damping"
depth = 0.3

[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.effect]]
plugin = "builtin:reverb"

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.effect]]
plugin = "builtin:reverb"
[[instrument.modulator]]
rate = 1.0
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = 0.5
[[instrument.modulator.target]]
slot = 2
param = "damping"
depth = 0.5
"#;

    fn lane_and_group_slots(model: &SessionModel) -> (Vec<usize>, Vec<usize>) {
        let of = |m: &ModulatorSlot| m.targets.iter().map(|t| t.slot).collect::<Vec<_>>();
        (of(&model.instruments[1].modulators[0]), of(&model.groups[0].modulators[0]))
    }

    #[test]
    fn removing_an_effect_moves_the_targets_of_its_lane_and_of_its_group() {
        let mut model = load(MEMBER_WITH_EFFECTS).model;
        assert_eq!(lane_and_group_slots(&model), ([1, 2].to_vec(), [1, 2].to_vec()));
        let removed = model.remove_effect(1, 0).expect("the filter");
        assert_eq!(removed.name, "Filter");
        assert_eq!(model.instruments[1].effects.len(), 1);
        assert_eq!(model.instruments[1].effects[0].name, "Reverb");
        // The reverb's `damping` is in slot 1 now, in both racks, and the filter's targets are gone.
        assert_eq!(lane_and_group_slots(&model), ([1].to_vec(), [1].to_vec()));
        assert_eq!(kinds(&model.instruments[1].modulators[0]), ["PluginParam { slot: 1, param_index: 2 }"]);
        assert_eq!(kinds(&model.groups[0].modulators[0]), ["GroupMember { member: 0, slot: 1, param_index: 2 }"]);
    }

    #[test]
    fn removing_an_effect_of_an_ungrouped_lane_leaves_the_groups_targets() {
        let mut model = load(MEMBER_WITH_EFFECTS).model;
        assert_eq!(model.remove_effect(0, 0).expect("the ungrouped lane's filter").name, "Filter");
        assert_eq!(model.instruments[0].effects.len(), 1);
        assert_eq!(lane_and_group_slots(&model), ([1, 2].to_vec(), [1, 2].to_vec()));
    }

    #[test]
    fn removing_an_effect_that_is_not_there_changes_nothing() {
        let mut model = load(MEMBER_WITH_EFFECTS).model;
        assert!(model.remove_effect(1, 2).is_none(), "the chain has two effects");
        assert!(model.remove_effect(1, 9).is_none());
        assert!(model.remove_effect(0, 2).is_none(), "the ungrouped lane has two");
        assert!(model.remove_effect(9, 0).is_none(), "there is no lane 9");
        assert_eq!(model.instruments[1].effects.len(), 2);
        assert_eq!(model.instruments[0].effects.len(), 2);
        assert_eq!(lane_and_group_slots(&model), ([1, 2].to_vec(), [1, 2].to_vec()));
    }

    #[test]
    fn the_cables_into_a_lane_are_those_of_its_group_to_the_lane_and_its_modulators() {
        use crate::app::testutil::{CHAINS, CROSS_MOD, LANES_IN_GROUPS};
        let model = load(CHAINS).model;
        let group = |m: usize, t: usize| model.groups[0].modulators[m].targets[t].edge;
        // The first member: the group's cables to its filter, its reverb and its first modulator's rate (not to the
        // bus effects, nor the second member's detune, nor the cables that drive the depth of the group's own).
        assert_eq!(model.group_cables_into_lane(0), [group(0, 0), group(0, 1), group(0, 5)]);
        assert_eq!(model.group_cables_into_lane(1), [group(0, 2)]);
        assert!(model.group_cables_into_lane(2).is_empty(), "a lane in no group has none");
        assert!(model.group_cables_into_lane(9).is_empty());
        // A cable to the depth of a cable of a member's modulator is one into the member.
        let model = load(CROSS_MOD).model;
        assert_eq!(model.group_cables_into_lane(0).len(), 3, "the rate, the attack and the depth of the first modulator");
        // The members are counted in their group, whatever lanes are between them.
        let model = load(LANES_IN_GROUPS).model;
        assert_eq!(model.group_cables_into_lane(0).len(), 1); // group 0's member 0: its detune
        assert_eq!(model.group_cables_into_lane(3).len(), 2); // group 0's member 1: its pan and the rate of its modulator
        assert_eq!(model.group_cables_into_lane(2).len(), 1); // group 1's only member: its volume (not group 0's cables to member 0)
    }

    #[test]
    fn removing_a_lane_takes_it_with_everything_it_has_and_moves_the_members_after_it_down() {
        use crate::app::testutil::CHAINS;
        let mut model = load(CHAINS).model;
        let first = model.instruments[0].node;
        let removed = model.remove_lane(0).expect("the first lane is there");
        assert_eq!((removed.node, removed.effects.len(), removed.modulators.len()), (first, 2, 2));
        assert_eq!(model.instruments.len(), 2);
        // The second member is the first now: the group's cable to its detune says member 0. (The cables to the
        // removed member went with it, which `Controller::remove_lane` makes sure of before: here they are dropped
        // the way the audio thread drops them.)
        let kinds: Vec<K> = model.groups[0].modulators[0].targets.iter().map(|t| t.kind.clone()).collect();
        assert!(kinds.contains(&K::GroupMember { member: 0, slot: 0, param_index: 1 }), "{kinds:?}");
        assert!(!kinds.iter().any(|k| matches!(k, K::GroupMember { member: 1, .. })), "{kinds:?}");
        assert!(model.remove_lane(2).is_none() && model.remove_lane(9).is_none(), "no lane 2 now");
        assert_eq!(model.instruments.len(), 2);
    }

    #[test]
    fn removing_a_group_leaves_its_members_in_no_group_and_numbers_the_later_groups_again() {
        use crate::app::testutil::LANES_IN_GROUPS;
        let mut model = load(LANES_IN_GROUPS).model;
        let groups: Vec<_> = model.instruments.iter().map(|lane| lane.group).collect();
        assert_eq!(groups, [Some(0), None, Some(1), Some(0)]);
        let first = model.groups[0].node;
        let removed = model.remove_group(0).expect("the first group is there");
        assert_eq!((removed.node, removed.modulators.len()), (first, 1));
        assert_eq!(model.groups.len(), 1);
        let groups: Vec<_> = model.instruments.iter().map(|lane| lane.group).collect();
        assert_eq!(groups, [None, None, Some(0), None], "its members are in none, and the second group is the first now");
        assert!(model.remove_group(1).is_none() && model.remove_group(9).is_none());
        assert_eq!(model.groups.len(), 1);
        // The last one: nothing is left.
        assert!(model.remove_group(0).is_some());
        assert!(model.groups.is_empty() && model.instruments.iter().all(|lane| lane.group.is_none()));
    }

    #[test]
    fn removing_a_bus_effect_drops_the_groups_targets_on_it_and_moves_the_later_ones_down() {
        use crate::app::testutil::CHAINS;
        let mut model = load(CHAINS).model;
        let bus_cables = |model: &SessionModel| -> Vec<K> { model.groups[0].modulators[0].targets.iter().map(|t| t.kind.clone()).filter(|kind| matches!(kind, K::GroupBus { .. })).collect() };
        assert_eq!(bus_cables(&model), [K::GroupBus { effect_index: 0, param_index: 0 }, K::GroupBus { effect_index: 1, param_index: 2 }]);
        let filter = model.groups[0].effects[0].node;
        let removed = model.remove_bus_effect(0, 0).expect("the filter is there");
        assert_eq!(removed.node, filter);
        assert_eq!(model.groups[0].effects.len(), 1);
        // The cable to the filter is gone, and the one to the reverb has its place now.
        assert_eq!(bus_cables(&model), [K::GroupBus { effect_index: 0, param_index: 2 }]);
        // The reverb is the only one left, and the last: nothing moves after it.
        assert!(model.remove_bus_effect(0, 0).is_some());
        assert!(bus_cables(&model).is_empty());
    }

    #[test]
    fn removing_a_bus_effect_that_is_not_there_changes_nothing() {
        use crate::app::testutil::CHAINS;
        let mut model = load(CHAINS).model;
        assert!(model.remove_bus_effect(0, 2).is_none(), "the bus has two effects");
        assert!(model.remove_bus_effect(1, 0).is_none(), "there is no group 1");
        assert_eq!(model.groups[0].effects.len(), 2);
        assert_eq!(model.groups[0].modulators[0].targets.len(), 6);
    }

    #[test]
    fn moving_an_effect_moves_the_targets_of_its_lane_and_of_its_group_with_it() {
        let mut model = load(MEMBER_WITH_EFFECTS).model;
        assert!(model.move_effect(1, 0, 1));
        let names: Vec<&str> = model.instruments[1].effects.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Reverb", "Filter"]);
        // `cutoff` followed the filter to slot 2, `damping` the reverb to slot 1.
        assert_eq!(lane_and_group_slots(&model), ([2, 1].to_vec(), [2, 1].to_vec()));
        assert_eq!(
            kinds(&model.groups[0].modulators[0]),
            ["GroupMember { member: 0, slot: 2, param_index: 0 }", "GroupMember { member: 0, slot: 1, param_index: 2 }"]
        );

        // Back again, the other way round.
        assert!(model.move_effect(1, 1, 0));
        let names: Vec<&str> = model.instruments[1].effects.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Filter", "Reverb"]);
        assert_eq!(lane_and_group_slots(&model), ([1, 2].to_vec(), [1, 2].to_vec()));
    }

    #[test]
    fn moving_an_effect_of_an_ungrouped_lane_leaves_the_groups_targets() {
        let mut model = load(MEMBER_WITH_EFFECTS).model;
        assert!(model.move_effect(0, 0, 1));
        let names: Vec<&str> = model.instruments[0].effects.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Reverb", "Filter"]);
        assert_eq!(lane_and_group_slots(&model), ([1, 2].to_vec(), [1, 2].to_vec()));
    }

    #[test]
    fn moving_an_effect_nowhere_changes_nothing() {
        let mut model = load(MEMBER_WITH_EFFECTS).model;
        assert!(!model.move_effect(1, 0, 0), "onto itself");
        assert!(!model.move_effect(1, 0, 2), "past the end");
        assert!(!model.move_effect(1, 2, 0), "from past the end");
        assert!(!model.move_effect(9, 0, 1), "no such lane");
        let names: Vec<&str> = model.instruments[1].effects.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Filter", "Reverb"]);
        assert_eq!(lane_and_group_slots(&model), ([1, 2].to_vec(), [1, 2].to_vec()));
    }

    #[test]
    fn moving_a_bus_effect_moves_the_groups_targets_on_it_with_it() {
        use crate::app::testutil::CHAINS;
        let mut model = load(CHAINS).model;
        let bus_cables = |model: &SessionModel| -> Vec<K> { model.groups[0].modulators[0].targets.iter().map(|t| t.kind.clone()).filter(|kind| matches!(kind, K::GroupBus { .. })).collect() };
        let (filter, reverb) = (model.groups[0].effects[0].node, model.groups[0].effects[1].node);
        assert!(model.move_bus_effect(0, 0, 1));
        let names: Vec<&str> = model.groups[0].effects.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Reverb", "Filter"]);
        assert_eq!(model.locate(filter), Some(NodePath::GroupEffect { group: 0, index: 1 }), "the node is the same, in its new place");
        assert_eq!(model.locate(reverb), Some(NodePath::GroupEffect { group: 0, index: 0 }));
        // The cable to the filter's cutoff says effect 1 now, and the one to the reverb's damping says effect 0: in the order the cables were in.
        assert_eq!(bus_cables(&model), [K::GroupBus { effect_index: 1, param_index: 0 }, K::GroupBus { effect_index: 0, param_index: 2 }]);
        // The cables to the members' plugins are what they were: the bus is not a member's chain.
        let members: Vec<K> = model.groups[0].modulators[0].targets.iter().map(|t| t.kind.clone()).filter(|kind| matches!(kind, K::GroupMember { .. })).collect();
        assert_eq!(
            members,
            [K::GroupMember { member: 0, slot: 1, param_index: 0 }, K::GroupMember { member: 0, slot: 2, param_index: 2 }, K::GroupMember { member: 1, slot: 0, param_index: 1 }]
        );
        // Back again, the other way round.
        assert!(model.move_bus_effect(0, 1, 0));
        let names: Vec<&str> = model.groups[0].effects.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Filter", "Reverb"]);
        assert_eq!(bus_cables(&model), [K::GroupBus { effect_index: 0, param_index: 0 }, K::GroupBus { effect_index: 1, param_index: 2 }]);
    }

    #[test]
    fn moving_a_bus_effect_nowhere_changes_nothing() {
        use crate::app::testutil::CHAINS;
        let mut model = load(CHAINS).model;
        assert!(!model.move_bus_effect(0, 0, 0), "onto itself");
        assert!(!model.move_bus_effect(0, 0, 2), "past the end");
        assert!(!model.move_bus_effect(0, 2, 0), "from past the end");
        assert!(!model.move_bus_effect(1, 0, 1), "no such group");
        let names: Vec<&str> = model.groups[0].effects.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Filter", "Reverb"]);
        assert_eq!(model.groups[0].modulators[0].targets.len(), 6);
    }

    /// What every cable of `LONG_CHAINS` drives (the group's, then the lane's), as the model says: the node of the plugin and the
    /// parameter's index.
    fn what_the_cables_of_long_chains_drive(model: &SessionModel) -> Vec<Option<Driven>> {
        let mut driven = Vec::new();
        for (at, rack) in [(ModulatorLocation::Group(0), &model.groups[0].modulators), (ModulatorLocation::Lane(0), &model.instruments[0].modulators)] {
            for (source, modulator) in rack.iter().enumerate() {
                driven.extend(modulator.targets.iter().map(|t| model.driven_by(at, source, &t.kind)));
            }
        }
        driven
    }

    #[test]
    fn an_effect_that_is_moved_anywhere_in_its_chain_leaves_every_cable_driving_the_plugin_it_drove() {
        let all_moves = || (0..4).flat_map(|from| (0..4).filter(move |to| *to != from).map(move |to| (from, to)));
        for (on_a_lane, from, to) in all_moves().map(|(f, t)| (true, f, t)).chain(all_moves().map(|(f, t)| (false, f, t))) {
            let chain = if on_a_lane { "the lane's chain" } else { "the bus" };
            let what = format!("{from} to {to} in {chain}");
            let mut model = load(LONG_CHAINS).model;
            let before = what_the_cables_of_long_chains_drive(&model);
            assert_eq!(before.len(), 12);
            assert!(before.iter().all(Option::is_some), "every cable drives something to begin with");
            let nodes = |model: &SessionModel| -> Vec<NodeId> {
                let chain = if on_a_lane { &model.instruments[0].effects } else { &model.groups[0].effects };
                chain.iter().map(|effect| effect.node).collect()
            };
            let mut expected = nodes(&model);
            let moved = expected.remove(from);
            expected.insert(to, moved);

            let did = if on_a_lane { model.move_effect(0, from, to) } else { model.move_bus_effect(0, from, to) };
            assert!(did, "{what}");
            assert_eq!(nodes(&model), expected, "the effects, {what}");
            assert_eq!(what_the_cables_of_long_chains_drive(&model), before, "what the cables drive, {what}");
            model.check_ids().unwrap_or_else(|e| panic!("{what}: {e}"));
        }
    }

    #[test]
    fn bus_remap_after_reorder_follows_the_effects() {
        let mut mods = vec![lfo(vec![
            target(K::GroupBus { effect_index: 0, param_index: 1 }),
            target(K::GroupBus { effect_index: 1, param_index: 2 }),
            target(K::GroupBus { effect_index: 2, param_index: 3 }),
            target(K::PluginParam { slot: 2, param_index: 4 }),
            target(K::GroupMember { member: 1, slot: 2, param_index: 5 }),
        ])];
        remap_group_bus_after_reorder(&mut mods, 2, 0);
        assert_eq!(
            kinds(&mods[0]),
            [
                "GroupBus { effect_index: 1, param_index: 1 }",
                "GroupBus { effect_index: 2, param_index: 2 }",
                "GroupBus { effect_index: 0, param_index: 3 }",
                "PluginParam { slot: 2, param_index: 4 }",
                "GroupMember { member: 1, slot: 2, param_index: 5 }",
            ]
        );
    }

    #[test]
    fn remap_index_agrees_with_actually_moving_an_element() {
        for n in 1..=6usize {
            for from in 0..n {
                for to in 0..n {
                    let mut chain: Vec<usize> = (0..n).collect();
                    let moved = chain.remove(from);
                    chain.insert(to, moved);
                    for original in 0..n {
                        assert_eq!(
                            chain[remap_index(original, from, to)],
                            original,
                            "n={n} from={from} to={to}: element {original} should land where the move put it"
                        );
                    }
                }
            }
        }
    }

    fn node(group: Option<usize>) -> InstrumentNode {
        InstrumentNode {
            node: NodeId(0),
            range: None,
            transpose: 0,
            volume: 1.0,
            muted: false,
            solo: false,
            route: Route::Out,
            instrument: None,
            effects: vec![],
            modulators: vec![],
            pattern: None,
            pitch_bend_range: 2.0,
            remap: Default::default(),
            group,
            meter: None,
        }
    }

    #[test]
    fn lane_member_ordinal_counts_earlier_members_of_the_same_group() {
        let lanes = [node(None), node(Some(0)), node(Some(1)), node(Some(0)), node(None), node(Some(0))];
        let ordinals: Vec<_> = (0..lanes.len()).map(|i| lane_member_ordinal(&lanes, i)).collect();
        assert_eq!(
            ordinals,
            [None, Some((0, 0)), Some((1, 0)), Some((0, 1)), None, Some((0, 2))]
        );
        assert_eq!(lane_member_ordinal(&lanes, 99), None, "out of range is ungrouped");
    }

    #[test]
    fn mod_source_slot_converts_to_a_fresh_audio_source() {
        match mod_source_slot_to_graph(&ModSourceSlot::Lfo { waveform: LfoWaveform::Saw, rate: 3.5, retrigger: false, fade: 0.0, sync: 0 }) {
            ModSource::Lfo { waveform, rate, phase, retrigger, fade, sync, age } => {
                assert_eq!((waveform, rate, phase, retrigger, fade, sync, age), (LfoWaveform::Saw, 3.5, 0.0, false, 0.0, 0.0, f32::INFINITY));
            }
            other => panic!("{other:?}"),
        }
        // The key goes with it: a note that starts it again and a fade that is as long as it was set to; it rests at full until a note.
        match mod_source_slot_to_graph(&ModSourceSlot::Lfo { waveform: LfoWaveform::Sine, rate: 1.0, retrigger: true, fade: 0.5, sync: 0 }) {
            ModSource::Lfo { retrigger, fade, age, .. } => assert_eq!((retrigger, fade, age), (true, 0.5, f32::INFINITY)),
            other => panic!("{other:?}"),
        }
        let env = ModSourceSlot::Envelope { attack: 0.1, decay: 0.2, sustain: 0.3, release: 0.4 };
        match mod_source_slot_to_graph(&env) {
            ModSource::Envelope { attack, decay, sustain, release, state, level, notes_held } => {
                assert_eq!((attack, decay, sustain, release), (0.1, 0.2, 0.3, 0.4));
                assert_eq!((state, level, notes_held), (crate::plugin::chain::EnvState::Idle, 0.0, 0));
            }
            other => panic!("{other:?}"),
        }
        let learned = ModSourceSlot::MidiLearn { source: Some(MidiBindSource::PitchBend), learning: false };
        match mod_source_slot_to_graph(&learned) {
            ModSource::MidiLearn { source, value, learning } => {
                assert_eq!((source, value, learning), (Some(MidiBindSource::PitchBend), 0.5, false));
            }
            other => panic!("{other:?}"),
        }
        let armed = ModSourceSlot::MidiLearn { source: None, learning: true };
        assert!(matches!(
            mod_source_slot_to_graph(&armed),
            ModSource::MidiLearn { source: None, learning: true, .. }
        ));
    }

    #[test]
    fn a_bound_control_goes_to_the_audio_thread_resting_where_its_knob_or_wheel_does() {
        // (The wheel sends nothing until it is moved, and its centre is where it is: half of the range of its 14 bits.)
        // (A key rests at middle C, which is where it adds nothing: note 60 of 127.)
        for (source, rest) in [
            (MidiBindSource::Cc(74), 0.0),
            (MidiBindSource::PitchBend, 0.5),
            (MidiBindSource::Aftertouch, 0.0),
            (MidiBindSource::Velocity, 0.0),
            (MidiBindSource::Key, 60.0 / 127.0),
        ] {
            match mod_source_slot_to_graph(&ModSourceSlot::MidiLearn { source: Some(source), learning: false }) {
                ModSource::MidiLearn { value, .. } => assert_eq!(value, rest, "{source:?}"),
                other => panic!("{other:?}"),
            }
        }
        // A control that is waiting to learn is bound to nothing, and rests at 0 (the base shows through whatever its value).
        match mod_source_slot_to_graph(&ModSourceSlot::new_midi_learn()) {
            ModSource::MidiLearn { value, .. } => assert_eq!(value, 0.0),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_macro_goes_to_the_audio_thread_as_a_macro_of_its_value() {
        assert!(matches!(mod_source_slot_to_graph(&ModSourceSlot::Macro { value: 0.4 }), ModSource::Macro { value } if value == 0.4));
        assert!(matches!(mod_source_slot_to_graph(&ModSourceSlot::new_macro()), ModSource::Macro { value } if value == 0.0));
    }

    #[test]
    fn a_new_macro_is_at_rest() {
        assert!(matches!(ModSourceSlot::new_macro(), ModSourceSlot::Macro { value } if value == 0.0));
    }

    #[test]
    fn a_macro_is_labelled_with_its_value_and_its_node_is_called_a_macro_whatever_the_value() {
        assert_eq!(ModSourceSlot::Macro { value: 0.5 }.label(), "Macro 0.50");
        assert_eq!(ModSourceSlot::Macro { value: 0.0 }.label(), "Macro 0.00");
        assert_eq!(ModSourceSlot::Macro { value: 1.0 }.label(), "Macro 1.00");
        // (A node's name is its kind: a client is told the settings when they change, and a name would have to follow them.)
        assert_eq!(ModSourceSlot::Macro { value: 0.5 }.node_name(), "Macro");
        assert_eq!(ModSourceSlot::Macro { value: 0.9 }.node_name(), "Macro");
    }

    #[test]
    fn new_modulators_start_as_a_sine_of_one_hertz_or_the_default_envelope() {
        use crate::plugin::chain::LfoWaveform;
        assert!(matches!(
            ModSourceSlot::new_lfo(),
            ModSourceSlot::Lfo { waveform: LfoWaveform::Sine, rate, .. } if rate == 1.0
        ));
        assert!(matches!(
            ModSourceSlot::new_envelope(),
            ModSourceSlot::Envelope { attack, decay, sustain, release }
                if (attack, decay, sustain, release) == (0.01, 0.3, 0.7, 0.5)
        ));
    }

    #[test]
    fn the_note_lengths_go_from_the_slowest_note_to_the_fastest_and_each_has_a_name_of_its_own_and_the_beats_it_is() {
        // The notes (the lengths that are neither dotted nor triplets) halve all the way: four bars to a thirty-second.
        let notes: Vec<f32> = SYNC_DIVISIONS.iter().filter(|(name, _)| !name.ends_with("dotted") && !name.ends_with("triplet")).map(|&(_, beats)| beats).collect();
        assert_eq!(notes, [16.0, 8.0, 4.0, 2.0, 1.0, 0.5, 0.25, 0.125]);
        // A dotted length is half as long again as its note, and a triplet two thirds of it.
        for &(name, beats) in &SYNC_DIVISIONS {
            for (suffix, times) in [(" dotted", 1.5), (" triplet", 2.0 / 3.0)] {
                if let Some(note) = name.strip_suffix(suffix) {
                    let &(_, of_the_note) = SYNC_DIVISIONS.iter().find(|&&(other, _)| other == note).unwrap_or_else(|| panic!("{name} has no note {note}"));
                    assert!((beats - of_the_note * times).abs() < 1e-6, "{name}: {beats} of {of_the_note}");
                }
            }
        }
        let mut names: Vec<&str> = SYNC_DIVISIONS.iter().map(|&(name, _)| name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), SYNC_DIVISIONS.len(), "no two are called the same");
        // A sync is a place in the list from 1, and 0 is none. A bar is four beats, a dotted note is a half as long again and a triplet two thirds of it.
        assert_eq!([sync_name(0), sync_name(1), sync_name(8), sync_name(16)], ["free", "4 bars", "1/4", "1/32"]);
        assert_eq!([sync_beats(0), sync_beats(3), sync_beats(8), sync_beats(7), sync_beats(11), sync_beats(16)], [0.0, 4.0, 1.0, 1.5, 0.5, 0.125]);
        assert!((sync_beats(9) - 2.0 / 3.0).abs() < 1e-6 && (sync_beats(15) - 1.0 / 6.0).abs() < 1e-6, "a triplet of quarters and of sixteenths");
        // A place that the list has not is none.
        for beyond in [17, 99, usize::MAX] {
            assert_eq!((sync_name(beyond), sync_beats(beyond)), ("free", 0.0), "{beyond}");
        }
    }

    #[test]
    fn a_note_length_is_found_by_its_name_in_any_case_and_none_by_free_and_what_is_neither_is_not_found() {
        for place in 1..=SYNC_DIVISIONS.len() {
            assert_eq!(sync_from_name(sync_name(place)), Some(place), "{}", sync_name(place));
        }
        for (written, place) in [("1/8", 11), ("  1/8   Triplet ", 12), ("4 BARS", 1), ("1/16 Dotted", 13), ("free", 0), ("Off", 0), ("NONE", 0), ("", 0), ("  ", 0)] {
            assert_eq!(sync_from_name(written), Some(place), "{written:?}");
        }
        for unknown in ["1/7", "1/8t", "1/8.", "8 bars", "1 bars", "triplet", "1/8 triplets", "0"] {
            assert_eq!(sync_from_name(unknown), None, "{unknown:?}");
        }
    }

    /// What every note length comes to at some tempos, as the web client's tests read it (`web/src/client/fixtures/sync.json`): they pin the page's `rateAt` to the
    /// engine's `rate_at_tempo`. A row is a place in the list, its name, a tempo and the rate (cycles or steps a second) that the engine goes at then.
    /// `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` rewrites it.
    #[test]
    fn the_web_clients_fixture_of_what_a_note_length_comes_to_is_current() {
        #[derive(serde::Serialize)]
        struct Row {
            place: usize,
            name: &'static str,
            bpm: f32,
            rate: f32,
        }
        let mut rows = Vec::new();
        for place in 1..=SYNC_DIVISIONS.len() {
            for bpm in [BPM_MIN, 90.0, DEFAULT_BPM, 133.0, BPM_MAX] {
                rows.push(Row { place, name: sync_name(place), bpm, rate: crate::plugin::chain::rate_at_tempo(bpm, sync_beats(place)) });
            }
        }
        // (The numbers are written as the wire writes them: the 32 bits, in the fewest digits.)
        let fresh = serde_json::to_string_pretty(&serde_json::json!({ "rows": crate::app::testutil::wire(&rows) })).unwrap() + "\n";
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/src/client/fixtures/sync.json");
        if std::env::var_os("TANG_UPDATE_TYPES").is_some() {
            std::fs::write(&path, &fresh).unwrap();
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            committed == fresh,
            "{} is not what the note lengths come to now: run `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` and commit the result",
            path.display()
        );
    }

    #[test]
    fn what_a_modulator_that_follows_the_tempo_goes_at_is_what_its_note_length_and_the_tempo_make() {
        let at = |place: usize, bpm: f32| crate::plugin::chain::rate_at_tempo(bpm, sync_beats(place));
        let near = |got: f32, want: f32| (got - want).abs() < 1e-4;
        // A sixteenth note at 120 beats a minute is eight a second, a quarter note two, a bar a half of one; the tempo scales all of them.
        assert!(near(at(14, 120.0), 8.0));
        assert!(near(at(8, 120.0), 2.0));
        assert!(near(at(3, 120.0), 0.5));
        assert!(near(at(8, 90.0), 1.5));
        assert!(near(at(14, 60.0), 4.0));
        assert!(near(at(12, 120.0), 6.0), "a triplet of eighths: three in the time of two");
        assert!(near(at(10, 120.0), 2.0 * 4.0 / 3.0), "a dotted eighth: two in the time of three");
    }

    #[test]
    fn an_lfo_and_a_step_sequencer_that_follow_the_tempo_are_labelled_by_the_note_length_and_not_the_rate_they_keep() {
        let lfo = |sync| ModSourceSlot::Lfo { waveform: LfoWaveform::Sine, rate: 4.0, retrigger: false, fade: 0.0, sync };
        assert_eq!(lfo(0).label(), "LFO 4.0Hz sine");
        assert_eq!(lfo(11).label(), "LFO 1/8 sine");
        assert_eq!(lfo(12).label(), "LFO 1/8 triplet sine");
        let mut steps = ModSourceSlot::new_steps();
        assert_eq!(steps.label(), "Steps 16 8.0Hz");
        if let ModSourceSlot::Steps { sync, .. } = &mut steps {
            *sync = 14;
        }
        assert_eq!(steps.label(), "Steps 16 1/16");
    }

    #[test]
    fn an_lfo_and_a_step_sequencer_go_to_the_engine_with_the_beats_of_their_note_length_and_none_for_none() {
        let beats = |source: &ModSourceSlot| match mod_source_slot_to_graph(source) {
            ModSource::Lfo { sync, .. } | ModSource::Steps { sync, .. } => sync,
            other => panic!("{other:?}"),
        };
        let lfo = |sync| ModSourceSlot::Lfo { waveform: LfoWaveform::Saw, rate: 3.5, retrigger: false, fade: 0.0, sync };
        assert_eq!([0, 3, 11, 16].map(|sync| beats(&lfo(sync))), [0.0, 4.0, 0.5, 0.125]);
        let mut steps = ModSourceSlot::new_steps();
        assert_eq!(beats(&steps), 0.0);
        if let ModSourceSlot::Steps { sync, .. } = &mut steps {
            *sync = 14;
        }
        assert_eq!(beats(&steps), 0.25);
        // (The rate is sent as it is, whether or not it is followed: the engine keeps it for when the modulator is free again.)
        assert!(matches!(mod_source_slot_to_graph(&lfo(11)), ModSource::Lfo { rate, .. } if rate == 3.5));
    }

    #[test]
    fn source_labels_are_what_the_tree_shows() {
        let label = |s: ModSourceSlot| s.label();
        assert_eq!(label(ModSourceSlot::Lfo { waveform: LfoWaveform::Triangle, rate: 0.25, retrigger: false, fade: 0.0, sync: 0 }), "LFO 0.2Hz triangle");
        assert_eq!(label(ModSourceSlot::Lfo { waveform: LfoWaveform::Sine, rate: 12.0, retrigger: false, fade: 0.0, sync: 0 }), "LFO 12.0Hz sine");
        assert_eq!(label(ModSourceSlot::Lfo { waveform: LfoWaveform::Random, rate: 4.0, retrigger: false, fade: 0.0, sync: 0 }), "LFO 4.0Hz random");
        assert_eq!(label(ModSourceSlot::Lfo { waveform: LfoWaveform::Smooth, rate: 0.5, retrigger: false, fade: 0.0, sync: 0 }), "LFO 0.5Hz smooth");
        assert_eq!(label(ModSourceSlot::Envelope { attack: 0.0, decay: 0.0, sustain: 0.0, release: 0.0 }), "ADSR");
        assert_eq!(label(ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Cc(74)), learning: false }), "CC 74");
        assert_eq!(label(ModSourceSlot::MidiLearn { source: Some(MidiBindSource::PitchBend), learning: false }), "Pitch Bend");
        assert_eq!(label(ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Aftertouch), learning: false }), "Aftertouch");
        assert_eq!(label(ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Velocity), learning: false }), "Velocity");
        assert_eq!(label(ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Key), learning: false }), "Key");
        assert_eq!(label(ModSourceSlot::MidiLearn { source: None, learning: true }), "MIDI (learning…)");
        assert_eq!(label(ModSourceSlot::MidiLearn { source: None, learning: false }), "MIDI (unbound)");
    }

    /// Row layouts of the modulator parameter pane, per source type:
    /// `(is_enum, range)` for each row.
    #[test]
    fn modulator_rows_per_source_type() {
        let depth = Some((DEPTH_MIN, DEPTH_MAX));
        let two = || vec![target(K::PluginParam { slot: 0, param_index: 0 }), target(K::PluginParam { slot: 0, param_index: 1 })];

        let m = lfo(two());
        assert_eq!(modulator_param_len(&m), 6 + 1 + 2);
        let rows: Vec<_> = (0..modulator_param_len(&m))
            .map(|r| (modulator_param_is_enum(&m, r), modulator_param_range(&m, r)))
            .collect();
        assert_eq!(
            rows,
            [
                (true, None),                   // Type
                (true, None),                   // Waveform
                (false, Some((0.01, 50.0))),    // Rate
                (true, None),                   // Sync
                (true, None),                   // Retrigger
                (false, Some((0.0, 5.0))),      // Fade
                (false, None),                  // "Targets" separator
                (false, depth),
                (false, depth),
            ]
        );

        let m = adsr(two());
        assert_eq!(modulator_param_len(&m), 5 + 1 + 2);
        let rows: Vec<_> = (0..modulator_param_len(&m))
            .map(|r| (modulator_param_is_enum(&m, r), modulator_param_range(&m, r)))
            .collect();
        assert_eq!(
            rows,
            [
                (true, None),                    // Type
                (false, Some((0.001, 10.0))),    // Attack
                (false, Some((0.001, 10.0))),    // Decay
                (false, Some((0.0, 1.0))),       // Sustain
                (false, Some((0.001, 10.0))),    // Release
                (false, None),                   // "Targets" separator
                (false, depth),
                (false, depth),
            ]
        );

        let m = midi(two());
        assert_eq!(modulator_param_len(&m), 1 + 1 + 2);
        let rows: Vec<_> = (0..modulator_param_len(&m))
            .map(|r| (modulator_param_is_enum(&m, r), modulator_param_range(&m, r)))
            .collect();
        assert_eq!(
            rows,
            [
                (false, None), // read-only source info
                (false, None), // "Targets" separator
                (false, depth),
                (false, depth),
            ]
        );

        // Past the last row there is nothing to edit.
        assert_eq!(modulator_param_range(&m, modulator_param_len(&m)), None);
    }

    fn knob(targets: Vec<ModTargetSlot>) -> ModulatorSlot {
        ModulatorSlot { node: NodeId(7), source: ModSourceSlot::Macro { value: 0.25 }, targets, output: None, position: None }
    }

    #[test]
    fn the_pane_of_a_macro_has_its_value_and_then_its_targets_and_no_type_to_turn() {
        use ModulatorField as F;
        use ModulatorRow::*;
        let two = || vec![target(K::PluginParam { slot: 0, param_index: 0 }), target(K::PluginParam { slot: 0, param_index: 1 })];
        let m = knob(two());
        let rows: Vec<_> = (0..=modulator_param_len(&m)).map(|r| modulator_row(&m, r)).collect();
        assert_eq!(rows, [Some(Setting(F::Value)), Some(Separator), Some(Depth(0)), Some(Depth(1)), None]);
        assert_eq!(modulator_param_range(&m, 0), Some((0.0, 1.0)));
        assert!(!modulator_param_is_enum(&m, 0), "a number, not a choice");
        assert_eq!(modulator_param_len(&knob(vec![])), 2);
        // The value is the macro's setting, and it has no other: no rate, no stage.
        assert_eq!(m.source.setting(F::Value), Some(0.25));
        for field in [F::Rate, F::Waveform, F::Attack, F::Decay, F::Sustain, F::Release] {
            assert_eq!(m.source.setting(field), None, "{field:?}");
        }
        // An LFO and an envelope have no value.
        assert_eq!(lfo(vec![]).source.setting(F::Value), None);
        assert_eq!(adsr(vec![]).source.setting(F::Value), None);
        // The row makes the intent that sets it.
        assert_eq!(m.row_intent(0, |value| value * 2.0), Some(Intent::SetModulatorField { node: NodeId(7), field: F::Value, value: 0.5 }));
        assert_eq!(m.row_intent(1, |value| value), None, "the separator");
    }

    /// An LFO that a note starts again and that comes in over three quarters of a second.
    fn keyed_lfo(targets: Vec<ModTargetSlot>) -> ModulatorSlot {
        ModulatorSlot {
            node: NodeId(7),
            source: ModSourceSlot::Lfo { waveform: LfoWaveform::Sine, rate: 2.0, retrigger: true, fade: 0.75, sync: 0 },
            targets,
            output: None,
            position: None,
        }
    }

    #[test]
    fn the_pane_of_an_lfo_has_its_waveform_its_rate_its_key_and_then_its_targets() {
        use ModulatorField as F;
        use ModulatorRow::*;
        let m = keyed_lfo(vec![target(K::PluginParam { slot: 0, param_index: 0 })]);
        let rows: Vec<_> = (0..=modulator_param_len(&m)).map(|r| modulator_row(&m, r)).collect();
        assert_eq!(rows, [Some(Type), Some(Waveform), Some(Setting(F::Rate)), Some(Sync), Some(Retrigger), Some(Setting(F::Fade)), Some(Separator), Some(Depth(0)), None]);
        let enums: Vec<usize> = (0..modulator_param_len(&m)).filter(|&r| modulator_param_is_enum(&m, r)).collect();
        assert_eq!(enums, [0, 1, 3, 4], "the type, the waveform, the note length to follow the tempo by and the retrigger are choices");
    }

    #[test]
    fn an_lfo_says_the_value_of_its_key_settings_and_a_row_makes_the_intent_that_sets_it() {
        use ModulatorField as F;
        let m = keyed_lfo(vec![]);
        assert_eq!([F::Retrigger, F::Fade].map(|field| m.source.setting(field)), [Some(1.0), Some(0.75)]);
        assert_eq!(ModSourceSlot::new_lfo().setting(F::Retrigger), Some(0.0), "a new one runs free");
        assert_eq!(ModSourceSlot::new_lfo().setting(F::Fade), Some(0.0), "and does not fade in");
        let set = |field, value| Some(Intent::SetModulatorField { node: NodeId(7), field, value });
        assert_eq!(m.row_intent(4, |on| 1.0 - on), set(F::Retrigger, 0.0), "the switch is turned over by the closure");
        assert_eq!(m.row_intent(5, |fade| fade + 0.25), set(F::Fade, 1.0));
        assert_eq!(m.row_intent(6, |value| value), None, "the separator");
        // The settings that are not an LFO's: an envelope, a control and a macro have neither, and a step sequencer has a retrigger of its own but no fade.
        for other in [adsr(vec![]), midi(vec![]), knob(vec![])] {
            for field in [F::Retrigger, F::Fade] {
                assert_eq!(other.source.setting(field), None, "{field:?}");
            }
        }
        assert_eq!(sequencer(vec![]).source.setting(F::Fade), None);
        assert_eq!(sequencer(vec![]).source.setting(F::Retrigger), Some(1.0));
    }

    #[test]
    fn the_key_of_an_lfo_is_set_by_a_person_and_no_cable_drives_it() {
        let m = keyed_lfo(vec![target(K::PluginParam { slot: 0, param_index: 0 })]);
        assert!(modulator_row_to_cross_mod(&m, 3, 2).is_some(), "the rate is");
        for row in [0, 1, 3, 4, 5, 6] {
            assert_eq!(modulator_row_to_cross_mod(&m, 3, row), None, "row {row}: the type, the waveform, the sync, the key and the separator");
        }
        let (kind, ..) = modulator_row_to_cross_mod(&m, 3, 7).unwrap();
        assert!(matches!(kind, K::ModulatorDepth { mod_index: 3, target_index: 0 }), "{kind:?}");
    }

    fn sequencer(targets: Vec<ModTargetSlot>) -> ModulatorSlot {
        ModulatorSlot { node: NodeId(7), source: ModSourceSlot::new_steps(), targets, output: None, position: None }
    }

    #[test]
    fn the_pane_of_a_step_sequencer_has_its_settings_its_sixteen_steps_and_then_its_targets() {
        use ModulatorField as F;
        use ModulatorRow::*;
        let two = || vec![target(K::PluginParam { slot: 0, param_index: 0 }), target(K::PluginParam { slot: 0, param_index: 1 })];
        let m = sequencer(two());
        assert_eq!(modulator_param_len(&m), 5 + 16 + 1 + 2);
        let rows: Vec<_> = (0..=modulator_param_len(&m)).map(|r| modulator_row(&m, r)).collect();
        let mut expected = vec![Some(Setting(F::Rate)), Some(Sync), Some(Setting(F::Glide)), Some(Setting(F::Length)), Some(Retrigger)];
        expected.extend((0..16).map(|i| Some(Step(i))));
        expected.extend([Some(Separator), Some(Depth(0)), Some(Depth(1)), None]);
        assert_eq!(rows, expected);
        // What a row can be turned over: the rate, the glide and the length by their fields, a step by -1 to 1, and the two choices and the separator by nothing.
        assert_eq!(modulator_param_range(&m, 0), Some(F::Rate.range()));
        assert_eq!(modulator_param_range(&m, 1), None);
        assert_eq!(modulator_param_range(&m, 2), Some((0.0, 1.0)));
        assert_eq!(modulator_param_range(&m, 3), Some((1.0, 16.0)));
        assert_eq!(modulator_param_range(&m, 4), None);
        assert_eq!(modulator_param_range(&m, 5), Some((-1.0, 1.0)));
        assert_eq!(modulator_param_range(&m, 20), Some((-1.0, 1.0)));
        assert_eq!(modulator_param_range(&m, 21), None, "the separator");
        assert_eq!(modulator_param_range(&m, 22), Some((DEPTH_MIN, DEPTH_MAX)));
        let enums: Vec<usize> = (0..modulator_param_len(&m)).filter(|&r| modulator_param_is_enum(&m, r)).collect();
        assert_eq!(enums, [1, 4], "the note length and the retrigger are choices, and no type is to be turned (it is not an LFO or an envelope)");
    }

    #[test]
    fn a_step_sequencer_says_the_value_of_its_settings_and_a_row_makes_the_intent_that_sets_it() {
        use ModulatorField as F;
        let m = sequencer(vec![]);
        assert_eq!(
            [F::Rate, F::Glide, F::Length, F::Retrigger, F::Sync].map(|field| m.source.setting(field)),
            [Some(8.0), Some(0.0), Some(16.0), Some(1.0), Some(0.0)],
            "(a new one is not tied to the tempo)"
        );
        for field in [F::Waveform, F::Attack, F::Decay, F::Sustain, F::Release, F::Value] {
            assert_eq!(m.source.setting(field), None, "{field:?}");
        }
        // The other kinds have none of its settings (an LFO has a switch of the same name, which is the key's: a note starts it again).
        for other in [lfo(vec![]), adsr(vec![]), midi(vec![]), knob(vec![])] {
            for field in [F::Glide, F::Length] {
                assert_eq!(other.source.setting(field), None, "{field:?}");
            }
        }
        for other in [adsr(vec![]), midi(vec![]), knob(vec![])] {
            assert_eq!(other.source.setting(F::Retrigger), None);
        }
        let set = |field, value| Some(Intent::SetModulatorField { node: NodeId(7), field, value });
        assert_eq!(m.row_intent(0, |rate| rate * 2.0), set(F::Rate, 16.0));
        assert_eq!(m.row_intent(1, |sync| sync + 14.0), set(F::Sync, 14.0), "a place in the list of note lengths");
        assert_eq!(m.row_intent(2, |glide| glide + 0.5), set(F::Glide, 0.5));
        assert_eq!(m.row_intent(3, |length| length - 1.0), set(F::Length, 15.0));
        assert_eq!(m.row_intent(4, |on| 1.0 - on), set(F::Retrigger, 0.0), "the switch is turned over by the closure");
        // A step is one of the values, and the intent has all of them: the one that is turned and the others as they are.
        let steps = |i: usize, value: f32| {
            let ModSourceSlot::Steps { mut values, .. } = ModSourceSlot::new_steps() else { unreachable!() };
            values[i] = value;
            Some(Intent::SetSteps { node: NodeId(7), values: values.to_vec() })
        };
        assert_eq!(m.row_intent(5 + 3, |value| value + 0.25), steps(3, -0.625 + 0.25));
        assert_eq!(m.row_intent(5 + 15, |_| -0.5), steps(15, -0.5), "a value that was typed");
        assert_eq!(m.row_intent(5 + 16, |value| value), None, "the separator");
    }

    #[test]
    fn the_rate_of_a_step_sequencer_is_a_setting_that_a_cable_can_drive_and_its_other_rows_are_not() {
        let m = sequencer(vec![target(K::PluginParam { slot: 0, param_index: 0 })]);
        let (kind, min, max, base, label) = modulator_row_to_cross_mod(&m, 3, 0).unwrap();
        assert!(matches!(kind, K::ModulatorRate { mod_index: 3 }), "{kind:?}");
        assert_eq!((min, max, base, label.as_str()), (0.01, 50.0, 8.0, "Mod 3 rate"));
        for row in 1..=20 {
            assert_eq!(modulator_row_to_cross_mod(&m, 3, row), None, "row {row}: the note length, the glide, the length, the switch and the steps are set by a person");
        }
        assert_eq!(modulator_row_to_cross_mod(&m, 3, 21), None, "the separator");
        let (kind, ..) = modulator_row_to_cross_mod(&m, 3, 22).unwrap();
        assert!(matches!(kind, K::ModulatorDepth { mod_index: 3, target_index: 0 }), "{kind:?}");
    }

    #[test]
    fn a_step_sequencer_is_named_and_labelled_by_what_it_goes_through_and_how_fast() {
        let source = ModSourceSlot::new_steps();
        assert_eq!(source.node_name(), "Steps");
        assert_eq!(source.label(), "Steps 16 8.0Hz");
        let mut shorter = ModSourceSlot::new_steps();
        if let ModSourceSlot::Steps { length, rate, .. } = &mut shorter {
            *length = 5;
            *rate = 0.25;
        }
        assert_eq!(shorter.label(), "Steps 5 0.2Hz");
    }

    #[test]
    fn a_step_sequencer_goes_to_the_engine_with_all_that_it_has_and_at_its_first_step() {
        let mut values = [0.0; 16];
        values[..3].copy_from_slice(&[0.5, -0.25, 1.0]);
        let source = ModSourceSlot::Steps { values, length: 3, rate: 6.5, glide: 0.25, retrigger: false, sync: 0 };
        match mod_source_slot_to_graph(&source) {
            ModSource::Steps { values: sent, length, rate, glide, retrigger, sync, position } => {
                assert_eq!((sent, length, rate, glide, retrigger, sync, position), (values, 3, 6.5, 0.25, false, 0.0, 0.0));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_value_of_a_macro_is_no_setting_that_a_cable_can_drive_and_the_depths_of_its_cables_are() {
        let m = knob(vec![target(K::PluginParam { slot: 0, param_index: 0 })]);
        assert_eq!(modulator_row_to_cross_mod(&m, 3, 0), None, "the value is turned by a person");
        assert_eq!(modulator_row_to_cross_mod(&m, 3, 1), None, "the separator");
        let (kind, min, max, base, label) = modulator_row_to_cross_mod(&m, 3, 2).unwrap();
        assert!(matches!(kind, K::ModulatorDepth { mod_index: 3, target_index: 0 }), "{kind:?}");
        assert_eq!((min, max, base, label.as_str()), (DEPTH_MIN, DEPTH_MAX, 0.5, "Mod 3 depth 0"));
    }

    #[test]
    fn a_row_of_the_pane_is_what_the_layout_of_its_source_says() {
        use ModulatorField as F;
        use ModulatorRow::*;
        let two = || vec![target(K::PluginParam { slot: 0, param_index: 0 }), target(K::PluginParam { slot: 0, param_index: 1 })];
        // (One row past the end too: there is nothing there.)
        let rows = |m: &ModulatorSlot| (0..=modulator_param_len(m)).map(|r| modulator_row(m, r)).collect::<Vec<_>>();
        assert_eq!(
            rows(&lfo(two())),
            [Some(Type), Some(Waveform), Some(Setting(F::Rate)), Some(Sync), Some(Retrigger), Some(Setting(F::Fade)), Some(Separator), Some(Depth(0)), Some(Depth(1)), None]
        );
        assert_eq!(
            rows(&adsr(two())),
            [
                Some(Type),
                Some(Setting(F::Attack)),
                Some(Setting(F::Decay)),
                Some(Setting(F::Sustain)),
                Some(Setting(F::Release)),
                Some(Separator),
                Some(Depth(0)),
                Some(Depth(1)),
                None
            ]
        );
        assert_eq!(rows(&midi(two())), [Some(Info), Some(Separator), Some(Depth(0)), Some(Depth(1)), None]);
        assert_eq!(rows(&lfo(vec![])), [Some(Type), Some(Waveform), Some(Setting(F::Rate)), Some(Sync), Some(Retrigger), Some(Setting(F::Fade)), Some(Separator), None]);
    }

    #[test]
    fn the_rows_that_are_numbers_have_the_range_the_controller_clamps_to() {
        use ModulatorField as F;
        let two = || vec![target(K::PluginParam { slot: 0, param_index: 0 })];
        for (m, field, row) in [
            (lfo(two()), F::Rate, 2),
            (adsr(two()), F::Attack, 1),
            (adsr(two()), F::Decay, 2),
            (adsr(two()), F::Sustain, 3),
            (adsr(two()), F::Release, 4),
        ] {
            assert_eq!(modulator_param_range(&m, row), Some(field.range()), "{field:?}");
        }
    }

    #[test]
    fn a_source_says_the_value_of_a_setting_it_has_and_nothing_of_one_it_has_not() {
        use ModulatorField as F;
        let triangle = ModSourceSlot::Lfo { waveform: LfoWaveform::Triangle, rate: 2.5, retrigger: false, fade: 0.0, sync: 0 };
        let envelope = ModSourceSlot::Envelope { attack: 0.1, decay: 0.2, sustain: 0.3, release: 0.4 };
        let control = ModSourceSlot::MidiLearn { source: None, learning: true };
        let all = [F::Rate, F::Waveform, F::Attack, F::Decay, F::Sustain, F::Release];
        let of = |source: &ModSourceSlot| all.map(|field| source.setting(field));
        assert_eq!(of(&triangle), [Some(2.5), Some(1.0), None, None, None, None]);
        assert_eq!(of(&envelope), [None, None, Some(0.1), Some(0.2), Some(0.3), Some(0.4)]);
        assert_eq!(of(&control), [None; 6]);
    }

    #[test]
    fn the_intent_of_a_row_sets_the_setting_or_the_depth_it_stands_for() {
        use ModulatorField as F;
        let with_edges = |m: ModulatorSlot, node: u32| ModulatorSlot {
            node: NodeId(node),
            targets: m.targets.into_iter().enumerate().map(|(i, t)| ModTargetSlot { edge: EdgeId(50 + i as u32), ..t }).collect(),
            ..m
        };
        let two = || vec![target(K::PluginParam { slot: 0, param_index: 0 }), target(K::PluginParam { slot: 0, param_index: 1 })];
        let intents = |m: &ModulatorSlot| (0..=modulator_param_len(m)).map(|r| m.row_intent(r, |v| v + 1.0)).collect::<Vec<_>>();
        let set = |node: u32, field, value| Some(Intent::SetModulatorField { node: NodeId(node), field, value });
        let depth = |edge: u32, depth| Some(Intent::SetEdgeDepth { edge: EdgeId(edge), depth });

        // A value is what the closure makes of the one the row has now (the controller clamps it).
        assert_eq!(
            intents(&with_edges(lfo(two()), 7)),
            [None, None, set(7, F::Rate, 3.0), set(7, F::Sync, 1.0), set(7, F::Retrigger, 1.0), set(7, F::Fade, 1.0), None, depth(50, 1.5), depth(51, 1.5), None]
        );
        assert_eq!(
            intents(&with_edges(adsr(two()), 8)),
            [
                None,
                set(8, F::Attack, 1.01),
                set(8, F::Decay, 1.3),
                set(8, F::Sustain, 1.7),
                set(8, F::Release, 1.5),
                None,
                depth(50, 1.5),
                depth(51, 1.5),
                None
            ]
        );
        assert_eq!(intents(&with_edges(midi(two()), 9)), [None, None, depth(50, 1.5), depth(51, 1.5), None]);
        // And the closure may ignore what the row has: a value that was typed.
        assert_eq!(with_edges(lfo(two()), 7).row_intent(2, |_| 12.5), set(7, F::Rate, 12.5));
    }

    #[test]
    fn cross_mod_rows_agree_with_the_row_ranges() {
        // The two tables (`modulator_param_range` for editing, `modulator_row_to_cross_mod`
        // for MIDI-learn / `m`) are written separately; they must not drift apart.
        let two = || vec![target(K::PluginParam { slot: 0, param_index: 0 }), target(K::PluginParam { slot: 0, param_index: 1 })];
        for m in [lfo(two()), adsr(two()), midi(two())] {
            for row in 0..=modulator_param_len(&m) {
                let range = modulator_param_range(&m, row);
                let cross = modulator_row_to_cross_mod(&m, 3, row);
                match (range, cross) {
                    (None, None) => {}
                    (Some((lo, hi)), Some((_, min, max, _, _))) => assert_eq!((lo, hi), (min, max), "row {row}"),
                    // (An LFO's fade is a number that a person turns and that no cable drives, as a step sequencer's glide is: it has a range and no cross-mod.)
                    (Some(_), None) if matches!(modulator_row(&m, row), Some(ModulatorRow::Setting(ModulatorField::Fade))) => {}
                    (range, cross) => panic!("row {row}: range {range:?} vs cross-mod {:?}", cross.map(|c| c.4)),
                }
            }
        }
    }

    #[test]
    fn cross_mod_rows_name_the_field_and_carry_its_current_value() {
        let two = vec![
            ModTargetSlot { depth: 0.3, ..target(K::PluginParam { slot: 0, param_index: 0 }) },
            ModTargetSlot { depth: -0.6, ..target(K::PluginParam { slot: 0, param_index: 1 }) },
        ];
        let m = lfo(two);
        let (kind, _, _, base, label) = modulator_row_to_cross_mod(&m, 3, 2).unwrap();
        assert_eq!((format!("{kind:?}"), base, label.as_str()), ("ModulatorRate { mod_index: 3 }".to_string(), 2.0, "Mod 3 rate"));
        let (kind, _, _, base, label) = modulator_row_to_cross_mod(&m, 3, 8).unwrap();
        assert_eq!(
            (format!("{kind:?}"), base, label.as_str()),
            ("ModulatorDepth { mod_index: 3, target_index: 1 }".to_string(), -0.6, "Mod 3 depth 1")
        );
        // Type, waveform, the note length, the key's switch and fade and the separator are not bindable.
        for row in [0, 1, 3, 4, 5, 6] {
            assert!(modulator_row_to_cross_mod(&m, 3, row).is_none(), "row {row}");
        }

        let m = adsr(vec![]);
        let names: Vec<_> = (0..modulator_param_len(&m))
            .filter_map(|r| modulator_row_to_cross_mod(&m, 0, r))
            .map(|(kind, _, _, base, label)| (format!("{kind:?}"), base, label))
            .collect();
        assert_eq!(
            names,
            [
                ("ModulatorAttack { mod_index: 0 }".to_string(), 0.01, "Mod 0 attack".to_string()),
                ("ModulatorDecay { mod_index: 0 }".to_string(), 0.3, "Mod 0 decay".to_string()),
                ("ModulatorSustain { mod_index: 0 }".to_string(), 0.7, "Mod 0 sustain".to_string()),
                ("ModulatorRelease { mod_index: 0 }".to_string(), 0.5, "Mod 0 release".to_string()),
            ]
        );
    }

    #[test]
    fn filters_default_fully_wet_everything_else_half_wet() {
        assert_eq!(default_effect_mix("builtin:filter"), 1.0);
        assert_eq!(default_effect_mix("builtin:drive"), 1.0);
        assert_eq!(default_effect_mix("builtin:reverb"), DEFAULT_EFFECT_MIX);
        assert_eq!(default_effect_mix("lv2:http://calf.sourceforge.net/plugins/Reverb"), 0.5);
    }

    #[test]
    fn plugin_format_comes_from_the_id() {
        for (id, format) in [
            ("builtin:osc", "Built-in"),
            ("lv2:http://tytel.org/helm", "LV2"),
            ("http://tytel.org/helm", "LV2"),
            ("urn:example:plugin", "LV2"),
            ("clap:com.u-he.diva", "CLAP"),
            ("com.u-he.diva", "CLAP"),
            ("vst3:Pianoteq 9", "VST3"),
            ("./Reverb.lv2", "LV2"),
            ("./Synth.clap", "CLAP"),
            ("/Library/Audio/Plug-Ins/VST3/Pianoteq 9.vst3", "VST3"),
            ("mystery", "?"),
        ] {
            assert_eq!(format_from_id(id), format, "{id}");
        }
    }
}
