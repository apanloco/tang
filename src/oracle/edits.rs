//! Scripted edits (P0 of `docs/patch-graph.md` §11.6): what a front-end asks of the controller while the session plays, applied between blocks.
//!
//! A session file says what the engine is at the start; a person changes it as it sounds, and that is where an engine that swaps its plan, fades a mute or hands a node back
//! for an Undo can go wrong in ways that no file shows (a fade only happens when a mute *changes*). An `Edit` is such a request in terms that mean something in every session:
//! the lane is a place among the lanes (wrapped round the lanes there are, so that an edit aims at something whatever has been taken out), a parameter a place among the
//! plugin's. `resolve` turns it into the `Intent` that a front-end would send, from the model as it is when the edit is made; one with nothing to aim at is skipped, one that the
//! controller refuses is counted as refused (as a front-end's request would be). The intents go through `Controller::apply`, the way the TUI's and the browser's do, so they are
//! in the history and `Undo` and `Redo` are among them.

use crate::app::controller::Controller;
use crate::app::graph::{Endpoint, NoteRange, Port, SourceInfo};
use crate::app::intent::{Gain, Intent, ModulatorKind};
use crate::app::model::{NodeId, ParamKind, PluginSlot};
use crate::session::{Feed, Route};

/// A request that a front-end makes.
#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    /// A parameter of the plugin at `slot` of a lane (0 is the instrument, 1 and up the effects), to `fraction` of its range; `param` is a place among the plugin's parameters
    /// that are not separators.
    Param { lane: usize, slot: usize, param: usize, fraction: f32 },
    Volume { lane: usize, value: f32 },
    Mix { lane: usize, effect: usize, value: f32 },
    GroupVolume { group: usize, value: f32 },
    GroupPan { group: usize, value: f32 },
    Mute { lane: usize, muted: bool },
    Solo { lane: usize, solo: bool },
    MuteGroup { group: usize, muted: bool },
    SoloGroup { group: usize, solo: bool },
    AddEffect { lane: usize, plugin: &'static str },
    /// An effect that stands by itself (step 87): a lane of its own at the end, with no instrument, and the effect in it.
    AddFreeEffect { plugin: &'static str },
    AddBusEffect { group: usize, plugin: &'static str },
    /// The effect at a place of a chain, put at another place of it.
    MoveEffect { lane: usize, effect: usize, to: usize },
    RemoveEffect { lane: usize, effect: usize },
    AddModulator { lane: usize, kind: ModulatorKind },
    /// A modulator of `kind` for a parameter, and a cable to it (the `~` menu of a knob).
    Modulate { lane: usize, slot: usize, param: usize, kind: ModulatorKind },
    RemoveModulator { lane: usize, index: usize },
    Range { lane: usize, range: Option<(u8, u8)> },
    /// A lane put in a group (a place among the groups), or out of its group.
    LaneGroup { lane: usize, group: Option<usize> },
    AddGroup { lanes: Vec<usize> },
    AddLane { plugin: &'static str },
    RemoveLane { lane: usize },
    /// The instrument of a lane taken out, the lane staying with its effects (which go on running) and the rest of what it has.
    ClearInstrument { lane: usize },
    RemoveGroup { group: usize },
    /// A preset of the plugin at `slot` of a lane, by its place among the plugin's.
    Preset { lane: usize, slot: usize, preset: usize },
    Bpm { bpm: f32 },
    /// An audio cable at `gain`, from a place among the outputs of the session (each lane's instrument and effects, then each group and its bus effects) to a place among its
    /// inputs (each lane's effects, each group's bus and its bus effects, and the mix last), both wrapped round the ones there are.
    AddCable { from: usize, to: usize, gain: f32 },
    /// The level of a cable, by its place among the cables that there are.
    CableGain { cable: usize, gain: f32 },
    /// A cable, by its place among the cables that there are, taken out.
    RemoveCable { cable: usize },
    /// The way out of a lane cut or given back.
    Route { lane: usize, route: Route },
    /// The way out of a group (a place among the groups) cut or given back.
    RouteGroup { group: usize, route: Route },
    /// The link into an effect cut or given back: a place among the effects of the session (each lane's effects, then each group's bus effects), wrapped round the ones there are.
    Feed { effect: usize, feed: Feed },
    /// The keys (none) or a device, by its name, put in the session as a source of notes.
    AddSource { device: Option<&'static str> },
    /// A cable of notes from a place among the sources to a place among the lanes, both wrapped round the ones there are.
    NotesCable { source: usize, lane: usize },
    /// A cable of notes, by its place among the cables of notes that there are, taken out.
    RemoveNotesCable { cable: usize },
    /// A source, by its place among the sources that there are, taken out with the cables of notes that start at it.
    RemoveSource { source: usize },
    Undo,
    Redo,
}

/// What became of an edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The controller carried it out.
    Applied,
    /// There was nothing for it to aim at (no effect to move, no group to put a lane in).
    Skipped,
    /// The controller refused it.
    Refused,
}

/// How many edits ended each way.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub applied: usize,
    pub skipped: usize,
    pub refused: usize,
}

impl Counts {
    pub fn add(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Applied => self.applied += 1,
            Outcome::Skipped => self.skipped += 1,
            Outcome::Refused => self.refused += 1,
        }
    }

    pub fn total(&self) -> usize {
        self.applied + self.skipped + self.refused
    }
}

/// The plugin at `slot` of a lane's chain, the places wrapped round the chain (the instrument, if it has one, and the effects).
fn plugin_at(lane: &crate::app::model::InstrumentNode, slot: usize) -> Option<&PluginSlot> {
    let places = usize::from(lane.instrument.is_some()) + lane.effects.len();
    if places == 0 {
        return None;
    }
    let slot = slot % places;
    if lane.instrument.is_some() {
        if slot == 0 { lane.instrument.as_ref() } else { lane.effects.get(slot - 1) }
    } else {
        lane.effects.get(slot)
    }
}

/// A parameter of a plugin that is not a separator, by its place among those.
fn param_at(plugin: &PluginSlot, place: usize) -> Option<&crate::app::model::ParamSlot> {
    let params: Vec<&crate::app::model::ParamSlot> = plugin.params.iter().filter(|p| !matches!(p.kind, ParamKind::Separator)).collect();
    (!params.is_empty()).then(|| params[place % params.len()])
}

/// The outputs of audio that a cable can start at, in the order of the model: each lane's instrument and then its effects, then each group and then its bus effects.
fn cable_starts(model: &crate::app::model::SessionModel) -> Vec<Endpoint> {
    let out = |node| Endpoint { node, port: Port::AudioOut };
    let lanes = model.instruments.iter().flat_map(|lane| std::iter::once(out(lane.node)).chain(lane.effects.iter().map(|effect| out(effect.node))));
    let groups = model.groups.iter().flat_map(|group| std::iter::once(out(group.node)).chain(group.effects.iter().map(|effect| out(effect.node))));
    lanes.chain(groups).collect()
}

/// The inputs of audio that a cable can end at, in the order of the model: each lane's effects, each group's bus and then its bus effects, and the mix.
fn cable_ends(model: &crate::app::model::SessionModel) -> Vec<Endpoint> {
    let into = |node| Endpoint { node, port: Port::AudioIn };
    let lanes = model.instruments.iter().flat_map(|lane| lane.effects.iter().map(|effect| into(effect.node)));
    let groups = model.groups.iter().flat_map(|group| std::iter::once(into(group.node)).chain(group.effects.iter().map(|effect| into(effect.node))));
    lanes.chain(groups).chain(std::iter::once(into(NodeId::MASTER))).collect()
}

impl Edit {
    /// The intent that a front-end would send for this edit, from the model as it is, or `None` if there is nothing for it to aim at.
    pub fn resolve(&self, ctl: &Controller) -> Option<Intent> {
        let model = &ctl.model;
        let lane = |n: usize| (!model.instruments.is_empty()).then(|| &model.instruments[n % model.instruments.len()]);
        let group = |n: usize| (!model.groups.is_empty()).then(|| &model.groups[n % model.groups.len()]);
        Some(match self {
            Edit::Param { lane: l, slot, param, fraction } => {
                let plugin = plugin_at(lane(*l)?, *slot)?;
                let p = param_at(plugin, *param)?;
                Intent::SetParam { node: plugin.node, param: p.index, value: p.min + fraction * (p.max - p.min) }
            }
            Edit::Volume { lane: l, value } => Intent::SetGain { node: lane(*l)?.node, gain: Gain::Volume, value: *value },
            Edit::Mix { lane: l, effect, value } => {
                let effects = &lane(*l)?.effects;
                if effects.is_empty() {
                    return None;
                }
                Intent::SetGain { node: effects[effect % effects.len()].node, gain: Gain::Mix, value: *value }
            }
            Edit::GroupVolume { group: g, value } => Intent::SetGain { node: group(*g)?.node, gain: Gain::GroupVolume, value: *value },
            Edit::GroupPan { group: g, value } => Intent::SetGain { node: group(*g)?.node, gain: Gain::GroupPan, value: *value },
            Edit::Mute { lane: l, muted } => Intent::SetMute { node: lane(*l)?.node, muted: *muted },
            Edit::Solo { lane: l, solo } => Intent::SetSolo { node: lane(*l)?.node, solo: *solo },
            Edit::MuteGroup { group: g, muted } => Intent::SetMute { node: group(*g)?.node, muted: *muted },
            Edit::SoloGroup { group: g, solo } => Intent::SetSolo { node: group(*g)?.node, solo: *solo },
            Edit::AddEffect { lane: l, plugin } => Intent::AddEffect { parent: lane(*l)?.node, plugin: (*plugin).to_string() },
            Edit::AddFreeEffect { plugin } => Intent::AddFreeEffect { plugin: (*plugin).to_string() },
            Edit::AddBusEffect { group: g, plugin } => Intent::AddEffect { parent: group(*g)?.node, plugin: (*plugin).to_string() },
            Edit::MoveEffect { lane: l, effect, to } => {
                let effects = &lane(*l)?.effects;
                if effects.len() < 2 {
                    return None;
                }
                let (at, mut place) = (effect % effects.len(), to % effects.len());
                if place == at {
                    place = (place + 1) % effects.len();
                }
                Intent::MoveEffect { node: effects[at].node, index: place as u32 }
            }
            Edit::RemoveEffect { lane: l, effect } => {
                let effects = &lane(*l)?.effects;
                if effects.is_empty() {
                    return None;
                }
                Intent::RemoveNode { node: effects[effect % effects.len()].node }
            }
            Edit::AddModulator { lane: l, kind } => Intent::AddModulator { parent: lane(*l)?.node, kind: *kind },
            Edit::Modulate { lane: l, slot, param, kind } => {
                let plugin = plugin_at(lane(*l)?, *slot)?;
                let p = param_at(plugin, *param)?;
                Intent::Modulate { to: Endpoint { node: plugin.node, port: Port::Param { index: p.index } }, kind: *kind, depth: None }
            }
            Edit::RemoveModulator { lane: l, index } => {
                let modulators = &lane(*l)?.modulators;
                if modulators.is_empty() {
                    return None;
                }
                Intent::RemoveNode { node: modulators[index % modulators.len()].node }
            }
            Edit::Range { lane: l, range } => Intent::SetRange { node: lane(*l)?.node, range: range.map(|(low, high)| NoteRange { low, high }) },
            Edit::LaneGroup { lane: l, group: g } => {
                Intent::SetLaneGroup { node: lane(*l)?.node, group: match g { Some(g) => Some(group(*g)?.node), None => None } }
            }
            Edit::AddGroup { lanes } => {
                let members = lanes.iter().map(|&l| lane(l).map(|lane| lane.node)).collect::<Option<Vec<_>>>()?;
                Intent::AddGroup { members }
            }
            Edit::AddLane { plugin } => Intent::AddInstrument { plugin: (*plugin).to_string() },
            Edit::RemoveLane { lane: l } => Intent::RemoveNode { node: lane(*l)?.node },
            Edit::ClearInstrument { lane: l } => {
                let lane = lane(*l)?;
                lane.instrument.as_ref()?;
                Intent::ClearInstrument { node: lane.node }
            }
            Edit::RemoveGroup { group: g } => Intent::RemoveNode { node: group(*g)?.node },
            Edit::Preset { lane: l, slot, preset } => {
                let plugin = plugin_at(lane(*l)?, *slot)?;
                if plugin.presets.is_empty() {
                    return None;
                }
                Intent::LoadPreset { node: plugin.node, preset: plugin.presets[preset % plugin.presets.len()].id.clone() }
            }
            Edit::Bpm { bpm } => Intent::SetBpm { bpm: *bpm },
            Edit::AddCable { from, to, gain } => {
                let (starts, ends) = (cable_starts(model), cable_ends(model));
                Intent::AddCable { from: starts[from % starts.len()], to: ends[to % ends.len()], gain: Some(*gain) }
            }
            Edit::CableGain { cable, gain } => {
                let cables = &model.cables;
                if cables.is_empty() {
                    return None;
                }
                Intent::SetCableGain { edge: cables[cable % cables.len()].id, gain: *gain }
            }
            Edit::RemoveCable { cable } => {
                let cables = &model.cables;
                if cables.is_empty() {
                    return None;
                }
                Intent::RemoveEdge { edge: cables[cable % cables.len()].id }
            }
            Edit::Route { lane: l, route } => Intent::SetRoute { node: lane(*l)?.node, route: *route },
            Edit::RouteGroup { group: g, route } => Intent::SetRoute { node: group(*g)?.node, route: *route },
            Edit::Feed { effect, feed } => {
                let effects: Vec<NodeId> = model.instruments.iter().flat_map(|lane| lane.effects.iter()).chain(model.groups.iter().flat_map(|group| group.effects.iter())).map(|effect| effect.node).collect();
                if effects.is_empty() {
                    return None;
                }
                Intent::SetFeed { node: effects[effect % effects.len()], feed: *feed }
            }
            Edit::AddSource { device } => Intent::AddSource { source: device.map_or(SourceInfo::Keys, |name| SourceInfo::Device { name: name.to_string() }) },
            Edit::NotesCable { source, lane: l } => {
                let sources = &model.sources;
                if sources.is_empty() {
                    return None;
                }
                Intent::AddCable {
                    from: Endpoint { node: sources[source % sources.len()].node, port: Port::EventsOut },
                    to: Endpoint { node: lane(*l)?.node, port: Port::EventsIn },
                    gain: None,
                }
            }
            Edit::RemoveNotesCable { cable } => {
                let cables = &model.event_cables;
                if cables.is_empty() {
                    return None;
                }
                Intent::RemoveEdge { edge: cables[cable % cables.len()].id }
            }
            Edit::RemoveSource { source } => {
                let sources = &model.sources;
                if sources.is_empty() {
                    return None;
                }
                Intent::RemoveNode { node: sources[source % sources.len()].node }
            }
            Edit::Undo => Intent::Undo,
            Edit::Redo => Intent::Redo,
        })
    }

    /// Make the edit through the controller.
    pub fn make(&self, ctl: &mut Controller) -> Outcome {
        match self.resolve(ctl) {
            None => Outcome::Skipped,
            Some(intent) => match ctl.apply(intent) {
                Ok(_) => Outcome::Applied,
                Err(_) => Outcome::Refused,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil;
    use crate::plugin::builtin;

    const TWO_LANES: &str = r#"
[[group]]
name = "Bus"

[[instrument]]
plugin = "builtin:osc"
group = 0

[[instrument.effect]]
plugin = "builtin:reverb"
mix = 0.5

[[instrument.effect]]
plugin = "builtin:filter"

[[instrument]]
plugin = "builtin:osc"
"#;

    fn controller(session: &str) -> Controller {
        let (handles, _engine) = testutil::stub_engine_of(4096);
        let model = testutil::load(session).model;
        let mut ctl = Controller::new(model, handles, None);
        ctl.catalog.add(builtin::enumerate_plugins());
        ctl
    }

    #[test]
    fn a_lane_is_a_place_among_the_lanes_and_wraps_round_them() {
        let ctl = controller(TWO_LANES);
        let second = ctl.model.instruments[1].node;
        assert_eq!(Edit::Mute { lane: 1, muted: true }.resolve(&ctl), Some(Intent::SetMute { node: second, muted: true }));
        assert_eq!(Edit::Mute { lane: 5, muted: true }.resolve(&ctl), Some(Intent::SetMute { node: second, muted: true }), "5 is 1 of 2");
        let bus = ctl.model.groups[0].node;
        assert_eq!(Edit::SoloGroup { group: 3, solo: true }.resolve(&ctl), Some(Intent::SetSolo { node: bus, solo: true }));
    }

    #[test]
    fn a_way_out_is_the_way_out_of_a_place_among_the_lanes_or_the_groups_and_wraps_round_them() {
        let mut ctl = controller(TWO_LANES);
        let (first, second, bus) = (ctl.model.instruments[0].node, ctl.model.instruments[1].node, ctl.model.groups[0].node);
        assert_eq!(Edit::Route { lane: 1, route: Route::None }.resolve(&ctl), Some(Intent::SetRoute { node: second, route: Route::None }));
        assert_eq!(Edit::Route { lane: 4, route: Route::Out }.resolve(&ctl), Some(Intent::SetRoute { node: first, route: Route::Out }), "4 is 0 of 2");
        assert_eq!(Edit::RouteGroup { group: 2, route: Route::None }.resolve(&ctl), Some(Intent::SetRoute { node: bus, route: Route::None }), "2 is 0 of 1");
        assert_eq!(Edit::Route { lane: 0, route: Route::None }.make(&mut ctl), Outcome::Applied);
        assert_eq!(Edit::RouteGroup { group: 0, route: Route::None }.make(&mut ctl), Outcome::Applied);
        assert_eq!((ctl.model.instruments[0].route, ctl.model.groups[0].route), (Route::None, Route::None));
        // A session with no group has nothing for a group's way out to aim at.
        let mut none = controller("[[instrument]]\nplugin = \"builtin:osc\"\n");
        assert_eq!(Edit::RouteGroup { group: 0, route: Route::None }.make(&mut none), Outcome::Skipped);
    }

    #[test]
    fn a_feed_is_the_feed_of_a_place_among_the_effects_of_the_lanes_and_then_of_the_buses_and_wraps_round_them() {
        let mut ctl = controller("[[group]]\n[[group.effect]]\nplugin = \"builtin:reverb\"\n[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n[[instrument.effect]]\nplugin = \"builtin:delay\"\n[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:drive\"\n");
        let (filter, delay, drive, reverb) = (ctl.model.instruments[0].effects[0].node, ctl.model.instruments[0].effects[1].node, ctl.model.instruments[1].effects[0].node, ctl.model.groups[0].effects[0].node);
        // The effects: lane 0's filter and delay, lane 1's drive, and then the reverb on the bus.
        let at = |effect: usize, feed: Feed| Edit::Feed { effect, feed }.resolve(&ctl);
        assert_eq!(at(0, Feed::None), Some(Intent::SetFeed { node: filter, feed: Feed::None }));
        assert_eq!(at(1, Feed::Chain), Some(Intent::SetFeed { node: delay, feed: Feed::Chain }));
        assert_eq!(at(2, Feed::None), Some(Intent::SetFeed { node: drive, feed: Feed::None }));
        assert_eq!(at(3, Feed::None), Some(Intent::SetFeed { node: reverb, feed: Feed::None }));
        assert_eq!(at(4, Feed::None), Some(Intent::SetFeed { node: filter, feed: Feed::None }), "4 is 0 of 4");
        assert_eq!(at(5, Feed::None), Some(Intent::SetFeed { node: delay, feed: Feed::None }), "5 is 1 of 4");
        assert_eq!(Edit::Feed { effect: 1, feed: Feed::None }.make(&mut ctl), Outcome::Applied);
        assert_eq!(ctl.model.instruments[0].effects.iter().map(|effect| effect.fed).collect::<Vec<_>>(), [true, false]);
        // A session with no effect has nothing for a feed to aim at.
        let mut none = controller("[[instrument]]\nplugin = \"builtin:osc\"\n");
        assert_eq!(Edit::Feed { effect: 0, feed: Feed::None }.make(&mut none), Outcome::Skipped);
    }

    #[test]
    fn an_effect_that_stands_by_itself_is_added_by_the_intent_whatever_the_session_has_and_is_a_lane_of_its_own_with_no_instrument() {
        let mut ctl = controller(TWO_LANES);
        assert_eq!(Edit::AddFreeEffect { plugin: "builtin:delay" }.resolve(&ctl), Some(Intent::AddFreeEffect { plugin: "builtin:delay".to_string() }));
        assert_eq!(Edit::AddFreeEffect { plugin: "builtin:delay" }.make(&mut ctl), Outcome::Applied);
        let lane = ctl.model.instruments.last().unwrap();
        assert_eq!(ctl.model.instruments.len(), 3);
        assert!(lane.instrument.is_none());
        assert_eq!(lane.effects.iter().map(|effect| effect.name.as_str()).collect::<Vec<_>>(), ["Delay"]);
        // A plugin that is an instrument is refused, as a front-end's request would be.
        assert_eq!(Edit::AddFreeEffect { plugin: "builtin:osc" }.make(&mut ctl), Outcome::Refused);
        assert_eq!(ctl.model.instruments.len(), 3);
        // (The new lane is a place among the lanes, like any: an edit that aims at one aims at it when it is the last.)
        let holder = ctl.model.instruments[2].node;
        assert_eq!(Edit::Mute { lane: 2, muted: true }.resolve(&ctl), Some(Intent::SetMute { node: holder, muted: true }));
    }

    #[test]
    fn a_parameter_is_a_fraction_of_the_range_of_one_of_the_plugins_that_is_not_a_separator() {
        let ctl = controller(TWO_LANES);
        let filter = &ctl.model.instruments[0].effects[1];
        let cutoff = filter.params.iter().find(|p| p.name == "cutoff").unwrap();
        let places: Vec<&str> = filter.params.iter().filter(|p| !matches!(p.kind, ParamKind::Separator)).map(|p| p.name.as_str()).collect();
        let at = places.iter().position(|&n| n == "cutoff").unwrap();
        // (Slot 2 of lane 0 is the filter: the instrument, then the reverb, then the filter.)
        let edit = Edit::Param { lane: 0, slot: 2, param: at + places.len(), fraction: 0.25 };
        assert_eq!(
            edit.resolve(&ctl),
            Some(Intent::SetParam { node: filter.node, param: cutoff.index, value: cutoff.min + 0.25 * (cutoff.max - cutoff.min) }),
            "a place that is past the end wraps round"
        );
    }

    #[test]
    fn an_edit_with_nothing_to_aim_at_is_skipped() {
        let mut ctl = controller(TWO_LANES);
        // The second lane has no effect and no modulator.
        for edit in [
            Edit::Mix { lane: 1, effect: 0, value: 0.5 },
            Edit::RemoveEffect { lane: 1, effect: 0 },
            Edit::MoveEffect { lane: 1, effect: 0, to: 1 },
            Edit::RemoveModulator { lane: 1, index: 0 },
        ] {
            assert_eq!(edit.make(&mut ctl), Outcome::Skipped, "{edit:?}");
        }
        // A lane that has no instrument has none to take out: the first edit takes it out, and the second finds nothing.
        assert_eq!(Edit::ClearInstrument { lane: 1 }.make(&mut ctl), Outcome::Applied);
        assert_eq!(Edit::ClearInstrument { lane: 1 }.make(&mut ctl), Outcome::Skipped, "the lane has none now");
        let mut none = controller("[[instrument]]\nplugin = \"builtin:osc\"\n");
        assert_eq!(Edit::GroupVolume { group: 0, value: 0.5 }.make(&mut none), Outcome::Skipped, "no group");
        assert_eq!(Edit::LaneGroup { lane: 0, group: Some(0) }.make(&mut none), Outcome::Skipped, "no group to put a lane in");
        assert_eq!(Edit::MoveEffect { lane: 0, effect: 0, to: 1 }.make(&mut none), Outcome::Skipped, "no chain to move in");
    }

    #[test]
    fn an_effect_is_moved_to_a_place_that_is_not_the_one_it_is_at() {
        let ctl = controller(TWO_LANES);
        let reverb = ctl.model.instruments[0].effects[0].node;
        // The place 0 is where the reverb is: the next one is taken, as a front-end's drag would not be a request at all.
        assert_eq!(Edit::MoveEffect { lane: 0, effect: 0, to: 0 }.resolve(&ctl), Some(Intent::MoveEffect { node: reverb, index: 1 }));
        assert_eq!(Edit::MoveEffect { lane: 0, effect: 0, to: 3 }.resolve(&ctl), Some(Intent::MoveEffect { node: reverb, index: 1 }), "3 is 1 of 2");
    }

    #[test]
    fn an_edit_that_the_controller_refuses_is_counted_as_refused_and_one_it_carries_out_as_applied() {
        let mut ctl = controller("[[instrument]]\nplugin = \"builtin:osc\"\n");
        assert_eq!(Edit::Undo.make(&mut ctl), Outcome::Refused, "there is nothing to undo");
        assert_eq!(Edit::RemoveLane { lane: 0 }.make(&mut ctl), Outcome::Refused, "a session keeps its last lane");
        assert_eq!(Edit::Volume { lane: 0, value: 0.5 }.make(&mut ctl), Outcome::Applied);
        assert_eq!(Edit::Undo.make(&mut ctl), Outcome::Applied, "now there is");
        let mut counts = Counts::default();
        for outcome in [Outcome::Applied, Outcome::Skipped, Outcome::Refused, Outcome::Applied] {
            counts.add(outcome);
        }
        assert_eq!((counts.applied, counts.skipped, counts.refused, counts.total()), (2, 1, 1, 4));
    }

    #[test]
    fn a_cable_goes_from_a_place_among_the_outputs_to_a_place_among_the_inputs_both_wrapped_round() {
        let ctl = controller(TWO_LANES);
        let (lane0, lane1, bus) = (ctl.model.instruments[0].node, ctl.model.instruments[1].node, ctl.model.groups[0].node);
        let (reverb, filter) = (ctl.model.instruments[0].effects[0].node, ctl.model.instruments[0].effects[1].node);
        let out = |node| Endpoint { node, port: Port::AudioOut };
        let into = |node| Endpoint { node, port: Port::AudioIn };
        // The outputs: lane 0's instrument, its reverb and its filter, lane 1's instrument, then the group. The inputs: the reverb and the filter, the group's bus, the mix.
        let at = |from: usize, to: usize| Edit::AddCable { from, to, gain: 0.5 }.resolve(&ctl);
        assert_eq!(at(0, 0), Some(Intent::AddCable { from: out(lane0), to: into(reverb), gain: Some(0.5) }));
        assert_eq!(at(2, 1), Some(Intent::AddCable { from: out(filter), to: into(filter), gain: Some(0.5) }));
        assert_eq!(at(3, 2), Some(Intent::AddCable { from: out(lane1), to: into(bus), gain: Some(0.5) }));
        assert_eq!(at(4, 3), Some(Intent::AddCable { from: out(bus), to: into(NodeId::MASTER), gain: Some(0.5) }));
        assert_eq!(at(5, 4), Some(Intent::AddCable { from: out(lane0), to: into(reverb), gain: Some(0.5) }), "5 is 0 of 5 outputs, and 4 is 0 of 4 inputs");
    }

    #[test]
    fn the_level_and_the_removal_of_a_cable_name_it_by_its_place_among_the_cables_and_skip_when_there_is_none() {
        let mut ctl = controller(TWO_LANES);
        assert_eq!(Edit::CableGain { cable: 0, gain: 0.5 }.make(&mut ctl), Outcome::Skipped);
        assert_eq!(Edit::RemoveCable { cable: 0 }.make(&mut ctl), Outcome::Skipped);
        assert_eq!(Edit::AddCable { from: 3, to: 3, gain: 0.9 }.make(&mut ctl), Outcome::Applied, "the second lane's instrument to the mix: a send at a level that is not the way out's, which has none");
        assert_eq!(Edit::AddCable { from: 0, to: 3, gain: 0.5 }.make(&mut ctl), Outcome::Applied);
        let (first, second) = (ctl.model.cables[0].id, ctl.model.cables[1].id);
        assert_eq!(Edit::CableGain { cable: 1, gain: 1.5 }.resolve(&ctl), Some(Intent::SetCableGain { edge: second, gain: 1.5 }));
        assert_eq!(Edit::CableGain { cable: 2, gain: 1.5 }.resolve(&ctl), Some(Intent::SetCableGain { edge: first, gain: 1.5 }), "2 is 0 of 2");
        assert_eq!(Edit::RemoveCable { cable: 3 }.resolve(&ctl), Some(Intent::RemoveEdge { edge: second }));
        assert_eq!(Edit::RemoveCable { cable: 0 }.make(&mut ctl), Outcome::Applied);
        assert_eq!(ctl.model.cables.iter().map(|cable| cable.id).collect::<Vec<_>>(), [second]);
    }

    #[test]
    fn a_cable_that_would_make_the_audio_go_round_or_that_is_there_is_refused_as_a_front_ends_request_would_be() {
        let mut ctl = controller(TWO_LANES);
        // The filter into the reverb, which is before it: a loop. The same cable twice.
        assert_eq!(Edit::AddCable { from: 2, to: 0, gain: 1.0 }.make(&mut ctl), Outcome::Refused);
        assert_eq!(Edit::AddCable { from: 3, to: 3, gain: 0.9 }.make(&mut ctl), Outcome::Applied);
        assert_eq!(Edit::AddCable { from: 3, to: 3, gain: 0.9 }.make(&mut ctl), Outcome::Refused);
        assert_eq!(ctl.model.cables.len(), 1);
        // A cable between the two ends of a link that is there (lane 1's instrument is its whole chain, and its way out goes to the mix) is refused as a cable between the two is.
        assert_eq!(Edit::AddCable { from: 3, to: 3, gain: 1.0 }.make(&mut ctl), Outcome::Refused);
    }

    #[test]
    fn a_source_is_the_keys_or_a_device_and_a_cable_of_notes_goes_from_a_place_among_the_sources_to_a_place_among_the_lanes_both_wrapped_round() {
        let mut ctl = controller(TWO_LANES);
        let (lane0, lane1) = (ctl.model.instruments[0].node, ctl.model.instruments[1].node);
        assert_eq!(Edit::AddSource { device: None }.resolve(&ctl), Some(Intent::AddSource { source: SourceInfo::Keys }));
        assert_eq!(Edit::AddSource { device: Some("Seaboard") }.resolve(&ctl), Some(Intent::AddSource { source: SourceInfo::Device { name: "Seaboard".to_string() } }));
        // There is nothing to aim at while there is no source or no cable of notes.
        for edit in [Edit::NotesCable { source: 0, lane: 0 }, Edit::RemoveNotesCable { cable: 0 }, Edit::RemoveSource { source: 0 }] {
            assert_eq!(edit.make(&mut ctl), Outcome::Skipped, "{edit:?}");
        }
        assert_eq!(Edit::AddSource { device: None }.make(&mut ctl), Outcome::Applied);
        assert_eq!(Edit::AddSource { device: Some("Seaboard") }.make(&mut ctl), Outcome::Applied);
        let (keys, seaboard) = (ctl.model.sources[0].node, ctl.model.sources[1].node);
        let out = |node| Endpoint { node, port: Port::EventsOut };
        let into = |node| Endpoint { node, port: Port::EventsIn };
        let draw = |source: usize, lane: usize| Edit::NotesCable { source, lane }.resolve(&ctl);
        assert_eq!(draw(0, 0), Some(Intent::AddCable { from: out(keys), to: into(lane0), gain: None }));
        assert_eq!(draw(1, 1), Some(Intent::AddCable { from: out(seaboard), to: into(lane1), gain: None }));
        assert_eq!(draw(3, 4), Some(Intent::AddCable { from: out(seaboard), to: into(lane0), gain: None }), "3 is 1 of 2 sources, and 4 is 0 of 2 lanes");
        assert_eq!(draw(2, 0), Some(Intent::AddCable { from: out(keys), to: into(lane0), gain: None }), "2 is 0 of 2 sources: a place wraps round, and is not held to the last");
        // The same cable twice is refused, as a front-end's request would be.
        assert_eq!(Edit::NotesCable { source: 0, lane: 0 }.make(&mut ctl), Outcome::Applied);
        assert_eq!(Edit::NotesCable { source: 0, lane: 0 }.make(&mut ctl), Outcome::Refused);
        assert_eq!(Edit::NotesCable { source: 1, lane: 1 }.make(&mut ctl), Outcome::Applied);
        let (first, second) = (ctl.model.event_cables[0].id, ctl.model.event_cables[1].id);
        assert_eq!(Edit::RemoveNotesCable { cable: 1 }.resolve(&ctl), Some(Intent::RemoveEdge { edge: second }));
        assert_eq!(Edit::RemoveNotesCable { cable: 2 }.resolve(&ctl), Some(Intent::RemoveEdge { edge: first }), "2 is 0 of 2");
        assert_eq!(Edit::RemoveSource { source: 3 }.resolve(&ctl), Some(Intent::RemoveNode { node: seaboard }), "3 is 1 of 2");
        assert_eq!(Edit::RemoveSource { source: 2 }.resolve(&ctl), Some(Intent::RemoveNode { node: keys }), "2 is 0 of 2: a place wraps round, and is not held to the last");
        // A source is taken out with its cable.
        assert_eq!(Edit::RemoveSource { source: 1 }.make(&mut ctl), Outcome::Applied);
        assert_eq!(ctl.model.event_cables.iter().map(|cable| cable.id).collect::<Vec<_>>(), [first]);
        assert_eq!(Edit::RemoveNotesCable { cable: 0 }.make(&mut ctl), Outcome::Applied);
        assert!(ctl.model.event_cables.is_empty());
    }

    #[test]
    fn every_kind_of_edit_is_made_into_the_intent_it_names() {
        let ctl = controller(TWO_LANES);
        let (lane0, lane1, bus) = (ctl.model.instruments[0].node, ctl.model.instruments[1].node, ctl.model.groups[0].node);
        let (reverb, filter) = (ctl.model.instruments[0].effects[0].node, ctl.model.instruments[0].effects[1].node);
        let expect = |edit: Edit, intent: Intent| assert_eq!(edit.resolve(&ctl), Some(intent), "{edit:?}");
        expect(Edit::Volume { lane: 0, value: 0.7 }, Intent::SetGain { node: lane0, gain: Gain::Volume, value: 0.7 });
        expect(Edit::Mix { lane: 0, effect: 1, value: 0.2 }, Intent::SetGain { node: filter, gain: Gain::Mix, value: 0.2 });
        expect(Edit::GroupVolume { group: 0, value: 0.6 }, Intent::SetGain { node: bus, gain: Gain::GroupVolume, value: 0.6 });
        expect(Edit::GroupPan { group: 0, value: -0.3 }, Intent::SetGain { node: bus, gain: Gain::GroupPan, value: -0.3 });
        expect(Edit::Solo { lane: 1, solo: true }, Intent::SetSolo { node: lane1, solo: true });
        expect(Edit::MuteGroup { group: 0, muted: true }, Intent::SetMute { node: bus, muted: true });
        expect(Edit::AddEffect { lane: 1, plugin: "builtin:delay" }, Intent::AddEffect { parent: lane1, plugin: "builtin:delay".to_string() });
        expect(Edit::AddBusEffect { group: 0, plugin: "builtin:drive" }, Intent::AddEffect { parent: bus, plugin: "builtin:drive".to_string() });
        expect(Edit::RemoveEffect { lane: 0, effect: 0 }, Intent::RemoveNode { node: reverb });
        expect(Edit::AddModulator { lane: 0, kind: ModulatorKind::Steps }, Intent::AddModulator { parent: lane0, kind: ModulatorKind::Steps });
        expect(Edit::Range { lane: 1, range: Some((36, 71)) }, Intent::SetRange { node: lane1, range: Some(NoteRange { low: 36, high: 71 }) });
        expect(Edit::Range { lane: 1, range: None }, Intent::SetRange { node: lane1, range: None });
        expect(Edit::LaneGroup { lane: 1, group: Some(0) }, Intent::SetLaneGroup { node: lane1, group: Some(bus) });
        expect(Edit::LaneGroup { lane: 0, group: None }, Intent::SetLaneGroup { node: lane0, group: None });
        expect(Edit::AddGroup { lanes: vec![0, 1] }, Intent::AddGroup { members: vec![lane0, lane1] });
        expect(Edit::AddLane { plugin: "builtin:osc" }, Intent::AddInstrument { plugin: "builtin:osc".to_string() });
        expect(Edit::RemoveLane { lane: 1 }, Intent::RemoveNode { node: lane1 });
        expect(Edit::ClearInstrument { lane: 1 }, Intent::ClearInstrument { node: lane1 });
        expect(Edit::RemoveGroup { group: 0 }, Intent::RemoveNode { node: bus });
        expect(Edit::Bpm { bpm: 99.0 }, Intent::SetBpm { bpm: 99.0 });
        expect(Edit::Undo, Intent::Undo);
        expect(Edit::Redo, Intent::Redo);
        let reverb_presets = &ctl.model.instruments[0].effects[0].presets;
        assert!(!reverb_presets.is_empty());
        expect(Edit::Preset { lane: 0, slot: 1, preset: 1 }, Intent::LoadPreset { node: reverb, preset: reverb_presets[1].id.clone() });
        let volume = ctl.model.instruments[0].instrument.as_ref().unwrap().params.iter().find(|p| p.name == "volume").unwrap().index;
        let places: Vec<&str> = ctl.model.instruments[0].instrument.as_ref().unwrap().params.iter().filter(|p| !matches!(p.kind, ParamKind::Separator)).map(|p| p.name.as_str()).collect();
        let at = places.iter().position(|&n| n == "volume").unwrap();
        expect(
            Edit::Modulate { lane: 0, slot: 0, param: at, kind: ModulatorKind::Lfo },
            Intent::Modulate { to: Endpoint { node: lane0, port: Port::Param { index: volume } }, kind: ModulatorKind::Lfo, depth: None },
        );
    }
}
