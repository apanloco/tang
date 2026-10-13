//! The session as a graph: what a front-end draws; see `docs/web-ui.md` §4.2.
//!
//! The engine is lane-shaped (instrument lanes with effect chains, submix groups,
//! modulators that drive parameters), the wire model is graph-shaped: nodes with
//! ports, and edges between the ports. `SessionModel::graph` projects the one into
//! the other, on demand. Nothing here is stored: a node is a lane, a group, an
//! effect or a modulator, with the `NodeId` the model gave it, and an edge is
//! either a link of the audio path (derived) or a modulation target, which has the
//! `EdgeId` the model gave it.
//!
//! ```text
//!   Instrument ──audio──▶ Effect ──audio──▶ Effect ──member──▶ Group ──audio──▶ Effect ──audio──▶ Master
//!   Modulator ──modulation(amount)──▶ a Param port of a plugin node, or a field of another Modulator
//! ```
//!
//! A node's `parent` is the node it sits in: a lane's effects and modulators sit in
//! the lane, a group's bus effects and modulators in the group, and a lane in the
//! group it is a member of. The master mix is a node of its own, `NodeId::MASTER`.
//!
//! The types are the wire format: JSON with camel-case names, and a tag (`kind`,
//! `type`) on each variant; the TypeScript is generated from them
//! (`app::protocol`).

#![cfg_attr(not(test), expect(dead_code, reason = "the snapshot a client gets is built from it; the web runtime (step 10) sends that"))]

use serde::{Deserialize, Serialize};

use crate::app::model::{EdgeId, ModSourceSlot, ModulatorSlot, NodeId, ParamKind, ParamSlot, PluginSlot, SessionModel};
use crate::plugin::chain::{CrossModField, LfoWaveform, MidiBindSource, ModTargetKind, ModulatorLocation};

/// A session as a front-end draws it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

/// One node of the graph. What it is, and what it carries, is in `kind`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
pub struct Node {
    pub id: NodeId,
    /// The node this one sits in, if any.
    pub parent: Option<NodeId>,
    /// What to call it: the plugin's name, the group's name, the modulator's label.
    pub name: String,
    #[serde(flatten)]
    pub kind: NodeKind,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum NodeKind {
    /// An instrument lane: the instrument plugin (null while the lane is empty),
    /// the key range it answers to, and its host-side output volume.
    Instrument { plugin: Option<Plugin>, range: Option<NoteRange>, transpose: i8, volume: f32 },
    /// An effect in a lane's chain or in a group's bus chain, with its dry/wet mix.
    Effect { plugin: Plugin, mix: f32 },
    /// A modulator, lane-scoped or group-scoped. What it drives are its modulation
    /// edges.
    Modulator { source: ModulatorSource },
    /// A submix group: its volume and stereo balance.
    Group { volume: f32, pan: f32 },
    /// The master mix.
    Master,
}

/// A range of MIDI notes, both ends included.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
pub struct NoteRange {
    pub low: u8,
    pub high: u8,
}

/// The plugin in an instrument or effect node.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Plugin {
    /// What the plugin was loaded from (`builtin:osc`, a path, a URI).
    pub id: String,
    /// `Built-in`, `LV2`, `CLAP` or `VST3`.
    pub format: String,
    pub is_instrument: bool,
    pub params: Vec<Param>,
    pub presets: Vec<Preset>,
    /// The name of the preset last loaded, if any.
    pub current_preset: Option<String>,
}

/// A parameter of a plugin, with the value the user set (modulation is not in it:
/// that is telemetry).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Param {
    /// The plugin's own index for it, which the intents and the ports name.
    pub index: u32,
    pub name: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub value: f32,
    /// The names of the values of an enum parameter (the value is an index into
    /// them); null for a number.
    pub labels: Option<Vec<String>>,
    pub unit: Option<String>,
    pub group: Vec<String>,
    pub step_count: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
pub struct Preset {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ModulatorSource {
    Lfo { waveform: Waveform, rate: f32 },
    Envelope { attack: f32, decay: f32, sustain: f32, release: f32 },
    /// A hardware control. `control` is null until one is bound; `learning` is
    /// true while it waits for the next control to come in.
    MidiLearn { control: Option<MidiControl>, learning: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum Waveform {
    Sine,
    Triangle,
    Saw,
    Square,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum MidiControl {
    Cc { number: u8 },
    PitchBend,
    Aftertouch,
}

/// A connection from a port of one node to a port of another.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
pub struct Edge {
    pub from: Endpoint,
    pub to: Endpoint,
    #[serde(flatten)]
    pub kind: EdgeKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum EdgeKind {
    /// A link of the audio path: instrument to its first effect, effect to effect,
    /// the end of a chain to the master. Derived; there is nothing to edit.
    Audio,
    /// A lane feeding the group it is a member of. Derived.
    Member,
    /// A modulator driving a parameter or a modulator field: a modulation target.
    /// `amount` is its depth, -1 to 1.
    Modulation { id: EdgeId, amount: f32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
pub struct Endpoint {
    pub node: NodeId,
    pub port: Port,
}

/// Where on a node an edge attaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Port {
    /// What a node takes in (an effect, a group, the master).
    AudioIn,
    /// What a node puts out (an instrument, an effect, a group).
    AudioOut,
    /// A modulator's output.
    ModOut,
    /// A parameter of the plugin in a node, by the plugin's own index.
    Param { index: u32 },
    /// A LFO's rate.
    Rate,
    /// An envelope's attack, decay, sustain and release.
    Attack,
    Decay,
    Sustain,
    Release,
    /// The depth of a modulation edge that starts at this modulator.
    Depth { edge: EdgeId },
}

impl From<LfoWaveform> for Waveform {
    fn from(waveform: LfoWaveform) -> Self {
        match waveform {
            LfoWaveform::Sine => Waveform::Sine,
            LfoWaveform::Triangle => Waveform::Triangle,
            LfoWaveform::Saw => Waveform::Saw,
            LfoWaveform::Square => Waveform::Square,
        }
    }
}

impl From<MidiBindSource> for MidiControl {
    fn from(source: MidiBindSource) -> Self {
        match source {
            MidiBindSource::Cc(number) => MidiControl::Cc { number },
            MidiBindSource::PitchBend => MidiControl::PitchBend,
            MidiBindSource::Aftertouch => MidiControl::Aftertouch,
        }
    }
}

impl SessionModel {
    /// The session as a graph: groups first (each followed by its bus effects and
    /// modulators), then lanes (each followed by its effects and modulators), then
    /// the master. The edges are the audio path, then the modulation targets in the
    /// order of the nodes that own them.
    pub fn graph(&self) -> Graph {
        let mut nodes = Vec::new();
        let mut edges = Vec::new();

        for (g, group) in self.groups.iter().enumerate() {
            nodes.push(Node {
                id: group.node,
                parent: None,
                name: group.display_name(g),
                kind: NodeKind::Group { volume: group.volume, pan: group.pan },
            });
            nodes.extend(group.effects.iter().map(|fx| effect_node(fx, group.node)));
            nodes.extend(group.modulators.iter().map(|m| modulator_node(m, group.node)));
            // Out of the group, through its bus effects, to the master.
            audio_chain(&mut edges, group.node, group.effects.iter().map(|fx| fx.node), NodeId::MASTER, EdgeKind::Audio);
        }

        for lane in &self.instruments {
            // A lane whose group is gone (the model keeps them in step, but a front-end
            // must not crash if it did not) goes to the master like an ungrouped one.
            let group = lane.group.and_then(|g| self.groups.get(g));
            nodes.push(Node {
                id: lane.node,
                parent: group.map(|g| g.node),
                name: lane.instrument.as_ref().map_or_else(|| "(empty)".to_string(), |plugin| plugin.name.clone()),
                kind: NodeKind::Instrument {
                    plugin: lane.instrument.as_ref().map(plugin),
                    range: lane.range.map(|(low, high)| NoteRange { low, high }),
                    transpose: lane.transpose,
                    volume: lane.volume,
                },
            });
            nodes.extend(lane.effects.iter().map(|fx| effect_node(fx, lane.node)));
            nodes.extend(lane.modulators.iter().map(|m| modulator_node(m, lane.node)));
            // Out of the instrument, through the lane's effects, to its group or the master.
            let (sink, last) = match group {
                Some(group) => (group.node, EdgeKind::Member),
                None => (NodeId::MASTER, EdgeKind::Audio),
            };
            audio_chain(&mut edges, lane.node, lane.effects.iter().map(|fx| fx.node), sink, last);
        }

        nodes.push(Node { id: NodeId::MASTER, parent: None, name: "Master".to_string(), kind: NodeKind::Master });
        self.modulation_edges(&mut edges);
        Graph { nodes, edges }
    }

    /// One edge for each modulation target that still has something to drive: the
    /// group modulators', then the lanes'.
    fn modulation_edges(&self, edges: &mut Vec<Edge>) {
        let groups = self.groups.iter().enumerate().map(|(g, group)| (ModulatorLocation::Group(g), &group.modulators));
        let lanes = self.instruments.iter().enumerate().map(|(i, lane)| (ModulatorLocation::Lane(i), &lane.modulators));
        for (at, rack) in groups.chain(lanes) {
            for modulator in rack {
                for target in &modulator.targets {
                    if let Some(to) = self.aim(at, rack, &target.kind) {
                        edges.push(Edge {
                            from: Endpoint { node: modulator.node, port: Port::ModOut },
                            to,
                            kind: EdgeKind::Modulation { id: target.edge, amount: target.depth },
                        });
                    }
                }
            }
        }
    }

    /// Where a target of a modulator in the rack `rack` at `at` points, as a port;
    /// `None` if what it points at is not there (or is not something that rack can
    /// drive).
    fn aim(&self, at: ModulatorLocation, rack: &[ModulatorSlot], kind: &ModTargetKind) -> Option<Endpoint> {
        use ModTargetKind as K;
        let param = |node: NodeId, index: u32| Endpoint { node, port: Port::Param { index } };
        let sibling = |mod_index: usize, port: Port| rack.get(mod_index).map(|m| Endpoint { node: m.node, port });
        match (at, kind) {
            (ModulatorLocation::Lane(inst), K::PluginParam { slot, param_index }) => {
                Some(param(self.chain_node(inst, *slot)?, *param_index))
            }
            (ModulatorLocation::Group(group), K::GroupMember { member, slot, param_index }) => {
                Some(param(self.chain_node(self.member_lane(group, *member)?, *slot)?, *param_index))
            }
            (ModulatorLocation::Group(group), K::GroupBus { effect_index, param_index }) => {
                Some(param(self.groups.get(group)?.effects.get(*effect_index)?.node, *param_index))
            }
            (ModulatorLocation::Group(group), K::GroupMemberMod { member, mod_index, field }) => {
                let lane = self.instruments.get(self.member_lane(group, *member)?)?;
                let modulator = lane.modulators.get(*mod_index)?;
                let port = match field {
                    CrossModField::Rate => Port::Rate,
                    CrossModField::Attack => Port::Attack,
                    CrossModField::Decay => Port::Decay,
                    CrossModField::Sustain => Port::Sustain,
                    CrossModField::Release => Port::Release,
                    CrossModField::Depth(target) => Port::Depth { edge: modulator.targets.get(*target)?.edge },
                };
                Some(Endpoint { node: modulator.node, port })
            }
            (_, K::ModulatorRate { mod_index }) => sibling(*mod_index, Port::Rate),
            (_, K::ModulatorAttack { mod_index }) => sibling(*mod_index, Port::Attack),
            (_, K::ModulatorDecay { mod_index }) => sibling(*mod_index, Port::Decay),
            (_, K::ModulatorSustain { mod_index }) => sibling(*mod_index, Port::Sustain),
            (_, K::ModulatorRelease { mod_index }) => sibling(*mod_index, Port::Release),
            (_, K::ModulatorDepth { mod_index, target_index }) => {
                let modulator = rack.get(*mod_index)?;
                Some(Endpoint { node: modulator.node, port: Port::Depth { edge: modulator.targets.get(*target_index)?.edge } })
            }
            _ => None,
        }
    }

    /// The node of the plugin in chain slot `slot` of lane `inst`: 0 is the
    /// instrument (the lane's own node), 1.. are its effects.
    fn chain_node(&self, inst: usize, slot: usize) -> Option<NodeId> {
        let lane = self.instruments.get(inst)?;
        match slot {
            0 => Some(lane.node),
            slot => lane.effects.get(slot - 1).map(|fx| fx.node),
        }
    }

    /// The lane that is the `member`-th (counting from 0, in lane order) of group
    /// `group`'s members: how a group modulator names a member.
    fn member_lane(&self, group: usize, member: usize) -> Option<usize> {
        self.instruments.iter().enumerate().filter(|(_, lane)| lane.group == Some(group)).nth(member).map(|(inst, _)| inst)
    }
}

/// The edges of a serial audio chain: out of `first`, in and out of each of
/// `through`, into `sink`. The last link, the one into `sink`, is of kind `last`.
fn audio_chain(edges: &mut Vec<Edge>, first: NodeId, through: impl Iterator<Item = NodeId>, sink: NodeId, last: EdgeKind) {
    let mut from = first;
    for node in through {
        edges.push(Edge {
            from: Endpoint { node: from, port: Port::AudioOut },
            to: Endpoint { node, port: Port::AudioIn },
            kind: EdgeKind::Audio,
        });
        from = node;
    }
    edges.push(Edge {
        from: Endpoint { node: from, port: Port::AudioOut },
        to: Endpoint { node: sink, port: Port::AudioIn },
        kind: last,
    });
}

fn effect_node(slot: &PluginSlot, parent: NodeId) -> Node {
    Node { id: slot.node, parent: Some(parent), name: slot.name.clone(), kind: NodeKind::Effect { plugin: plugin(slot), mix: slot.mix } }
}

fn modulator_node(modulator: &ModulatorSlot, parent: NodeId) -> Node {
    let source = match &modulator.source {
        ModSourceSlot::Lfo { waveform, rate } => ModulatorSource::Lfo { waveform: (*waveform).into(), rate: *rate },
        ModSourceSlot::Envelope { attack, decay, sustain, release } => {
            ModulatorSource::Envelope { attack: *attack, decay: *decay, sustain: *sustain, release: *release }
        }
        ModSourceSlot::MidiLearn { source, learning } => {
            ModulatorSource::MidiLearn { control: source.map(MidiControl::from), learning: *learning }
        }
    };
    Node { id: modulator.node, parent: Some(parent), name: modulator.source.label(), kind: NodeKind::Modulator { source } }
}

fn plugin(slot: &PluginSlot) -> Plugin {
    Plugin {
        id: slot.id.clone(),
        format: slot.format.clone(),
        is_instrument: slot.is_instrument,
        params: slot.params.iter().filter_map(param).collect(),
        presets: slot.presets.iter().map(|p| Preset { id: p.id.clone(), name: p.name.clone() }).collect(),
        current_preset: slot.current_preset.clone(),
    }
}

/// A plugin's parameter. A separator is a row of the TUI's parameter pane, not a
/// parameter, and has none.
fn param(slot: &ParamSlot) -> Option<Param> {
    let labels = match &slot.kind {
        ParamKind::Float => None,
        ParamKind::Enum(labels) => Some(labels.clone()),
        ParamKind::Separator => return None,
    };
    Some(Param {
        index: slot.index,
        name: slot.name.clone(),
        min: slot.min,
        max: slot.max,
        default: slot.default,
        value: slot.value,
        labels,
        unit: slot.unit.clone(),
        group: slot.group.clone(),
        step_count: slot.step_count,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::app::model::ModTargetSlot;
    use crate::app::testutil::{DISABLED_PATTERN, GROUPS, LEGACY, MIDI, SETTINGS, load};

    const OSC: &str = "[[instrument]]\nplugin = \"builtin:osc\"\n";

    fn out(node: NodeId) -> Endpoint {
        Endpoint { node, port: Port::AudioOut }
    }

    fn into(node: NodeId) -> Endpoint {
        Endpoint { node, port: Port::AudioIn }
    }

    fn at(node: NodeId, port: Port) -> Endpoint {
        Endpoint { node, port }
    }

    fn param(node: NodeId, index: u32) -> Endpoint {
        at(node, Port::Param { index })
    }

    fn audio(from: Endpoint, to: Endpoint) -> Edge {
        Edge { from, to, kind: EdgeKind::Audio }
    }

    fn member(from: Endpoint, to: Endpoint) -> Edge {
        Edge { from, to, kind: EdgeKind::Member }
    }

    fn modulation(modulator: NodeId, to: Endpoint, id: EdgeId, amount: f32) -> Edge {
        Edge { from: at(modulator, Port::ModOut), to, kind: EdgeKind::Modulation { id, amount } }
    }

    fn node(graph: &Graph, id: NodeId) -> &Node {
        graph.nodes.iter().find(|n| n.id == id).unwrap_or_else(|| panic!("no node {id:?} in {graph:#?}"))
    }

    fn plugin_of(node: &Node) -> &Plugin {
        match &node.kind {
            NodeKind::Instrument { plugin: Some(plugin), .. } | NodeKind::Effect { plugin, .. } => plugin,
            other => panic!("{other:?} has no plugin"),
        }
    }

    fn the_param<'a>(plugin: &'a Plugin, name: &str) -> &'a Param {
        plugin.params.iter().find(|p| p.name == name).unwrap_or_else(|| panic!("no parameter {name}"))
    }

    // -- Nodes --

    #[test]
    fn a_lone_instrument_is_a_node_that_feeds_the_master() {
        let model = load(OSC).model;
        let lane = model.instruments[0].node;
        let graph = model.graph();
        let ids: Vec<NodeId> = graph.nodes.iter().map(|n| n.id).collect();
        assert_eq!(ids, [lane, NodeId::MASTER]);
        assert_eq!(graph.edges, [audio(out(lane), into(NodeId::MASTER))]);
        assert_eq!(graph.nodes[1], Node { id: NodeId::MASTER, parent: None, name: "Master".to_string(), kind: NodeKind::Master });

        let osc = &graph.nodes[0];
        assert_eq!((osc.parent, osc.name.as_str()), (None, "Oscillator"));
        let NodeKind::Instrument { plugin: Some(plugin), range, transpose, volume } = &osc.kind else {
            panic!("{:?}", osc.kind)
        };
        assert_eq!((*range, *transpose, *volume), (None, 0, 1.0));
        assert_eq!((plugin.id.as_str(), plugin.format.as_str(), plugin.is_instrument), ("builtin:osc", "Built-in", true));
        assert_eq!((plugin.presets.len(), plugin.current_preset.as_deref()), (0, None));
        let names: Vec<&str> = plugin.params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["waveform", "detune", "volume", "attack", "decay", "sustain", "release", "pan"]);
    }

    #[test]
    fn an_instruments_settings_are_on_its_node() {
        let model = load(SETTINGS).model;
        let graph = model.graph();
        let NodeKind::Instrument { plugin: Some(plugin), range, transpose, volume } = &graph.nodes[0].kind else {
            panic!("{:?}", graph.nodes[0].kind)
        };
        assert_eq!((*range, *transpose, *volume), (Some(NoteRange { low: 36, high: 83 }), -12, 0.5));
        let waveform = the_param(plugin, "waveform");
        assert_eq!(waveform.value, 2.0, "the session's override, not the default");
        assert_eq!(waveform.default, 0.0);
        assert_eq!(waveform.labels.as_deref(), Some(&["sine", "triangle", "square", "saw"].map(String::from)[..]));
        assert_eq!(waveform.step_count, 3);
    }

    #[test]
    fn a_parameter_says_what_the_plugin_says_about_it() {
        let model = load("[[instrument]]\nplugin = \"builtin:osc\"\n[instrument.params]\nattack = 0.25\n").model;
        let graph = model.graph();
        let attack = the_param(plugin_of(&graph.nodes[0]), "attack").clone();
        assert_eq!(
            attack,
            Param {
                index: 3,
                name: "attack".to_string(),
                min: 0.0,
                max: 10.0,
                default: 0.004,
                value: 0.25,
                labels: None,
                unit: Some("s".to_string()),
                group: vec!["Envelope".to_string()],
                step_count: 0,
            }
        );
    }

    #[test]
    fn a_lanes_effects_follow_it_in_a_chain_and_sit_in_it() {
        let model = load(
            "[[instrument]]\nplugin = \"builtin:osc\"\n\
             [[instrument.effect]]\nplugin = \"builtin:filter\"\nmix = 1.0\n\
             [[instrument.effect]]\nplugin = \"builtin:reverb\"\nmix = 0.25\n",
        )
        .model;
        let lane = &model.instruments[0];
        let (osc, filter, reverb) = (lane.node, lane.effects[0].node, lane.effects[1].node);
        let graph = model.graph();
        let ids: Vec<NodeId> = graph.nodes.iter().map(|n| n.id).collect();
        assert_eq!(ids, [osc, filter, reverb, NodeId::MASTER]);
        assert_eq!(
            graph.edges,
            [audio(out(osc), into(filter)), audio(out(filter), into(reverb)), audio(out(reverb), into(NodeId::MASTER))]
        );
        assert_eq!((node(&graph, filter).parent, node(&graph, reverb).parent), (Some(osc), Some(osc)));
        assert_eq!((node(&graph, filter).name.as_str(), node(&graph, reverb).name.as_str()), ("Filter", "Reverb"));
        let NodeKind::Effect { plugin, mix } = &node(&graph, reverb).kind else { panic!() };
        assert_eq!((plugin.id.as_str(), *mix, plugin.is_instrument), ("builtin:reverb", 0.25, false));
        assert!(!plugin.presets.is_empty(), "the reverb's presets come along");
    }

    #[test]
    fn an_empty_lane_is_an_instrument_node_without_a_plugin() {
        let mut model = load(OSC).model;
        model.instruments[0].instrument = None;
        let graph = model.graph();
        assert!(matches!(&graph.nodes[0].kind, NodeKind::Instrument { plugin: None, .. }), "{:?}", graph.nodes[0].kind);
        assert_eq!(graph.nodes[0].name, "(empty)");
        assert_eq!(graph.edges, [audio(out(model.instruments[0].node), into(NodeId::MASTER))]);
    }

    // -- Groups --

    const GROUPED: &str = r#"
[[group]]
name = "Pad"
volume = 0.8
pan = -0.25
[[group.effect]]
plugin = "builtin:reverb"
mix = 0.6

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.effect]]
plugin = "builtin:filter"
mix = 1.0

[[instrument]]
plugin = "builtin:osc"
"#;

    #[test]
    fn a_group_is_a_node_its_lanes_feed_and_its_bus_effects_follow() {
        let model = load(GROUPED).model;
        let pad = model.groups[0].node;
        let bus = model.groups[0].effects[0].node;
        let (lane0, filter, lane1) = (model.instruments[0].node, model.instruments[0].effects[0].node, model.instruments[1].node);
        let graph = model.graph();

        let ids: Vec<NodeId> = graph.nodes.iter().map(|n| n.id).collect();
        assert_eq!(ids, [pad, bus, lane0, filter, lane1, NodeId::MASTER], "groups first, then lanes, then the master");
        assert_eq!(
            graph.edges,
            [
                audio(out(pad), into(bus)),
                audio(out(bus), into(NodeId::MASTER)),
                audio(out(lane0), into(filter)),
                member(out(filter), into(pad)),
                audio(out(lane1), into(NodeId::MASTER)),
            ]
        );

        let parents: Vec<(NodeId, Option<NodeId>)> = graph.nodes.iter().map(|n| (n.id, n.parent)).collect();
        assert_eq!(
            parents,
            [(pad, None), (bus, Some(pad)), (lane0, Some(pad)), (filter, Some(lane0)), (lane1, None), (NodeId::MASTER, None)]
        );
        assert_eq!(node(&graph, pad).name, "Pad");
        assert_eq!(node(&graph, pad).kind, NodeKind::Group { volume: 0.8, pan: -0.25 });
        assert!(matches!(&node(&graph, bus).kind, NodeKind::Effect { mix, .. } if *mix == 0.6));
    }

    #[test]
    fn a_group_with_no_bus_effects_feeds_the_master_directly() {
        let model = load("[[group]]\n[[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\n").model;
        let (group, lane) = (model.groups[0].node, model.instruments[0].node);
        let graph = model.graph();
        assert_eq!(graph.edges, [audio(out(group), into(NodeId::MASTER)), member(out(lane), into(group))]);
        assert_eq!(node(&graph, group).name, "Group 1", "unnamed, so by its number");
    }

    #[test]
    fn a_group_nobody_is_in_is_still_a_node() {
        let model = load("[[group]]\nname = \"Empty\"\n").model;
        let graph = model.graph();
        let group = model.groups[0].node;
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.edges, [audio(out(group), into(NodeId::MASTER))]);
    }

    #[test]
    fn a_lane_whose_group_is_gone_goes_to_the_master() {
        let mut model = load("[[group]]\n[[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\n").model;
        model.instruments[0].group = Some(7);
        let graph = model.graph();
        let lane = model.instruments[0].node;
        assert!(graph.edges.contains(&audio(out(lane), into(NodeId::MASTER))), "{:?}", graph.edges);
        assert_eq!(node(&graph, lane).parent, None);
    }

    // -- Modulators --

    const MODULATED: &str = r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:filter"

[[instrument.modulator]]
type = "lfo"
waveform = "saw"
rate = 2.0
[[instrument.modulator.target]]
param = "detune"
depth = 0.1
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = -0.3

[[instrument.modulator]]
type = "envelope"
attack = 0.02
decay = 0.4
sustain = 0.5
release = 1.0
[[instrument.modulator.target]]
mod_rate = 0
depth = 0.5
[[instrument.modulator.target]]
mod_depth = [0, 1]
depth = 0.2
[[instrument.modulator.target]]
mod_sustain = 1
depth = 0.1
"#;

    #[test]
    fn a_modulator_is_a_node_in_its_lane_with_an_edge_for_each_target() {
        let model = load(MODULATED).model;
        let lane = &model.instruments[0];
        let (osc, filter) = (lane.node, lane.effects[0].node);
        let (lfo, env) = (lane.modulators[0].node, lane.modulators[1].node);
        let lfo_edges: Vec<EdgeId> = lane.modulators[0].targets.iter().map(|t| t.edge).collect();
        let env_edges: Vec<EdgeId> = lane.modulators[1].targets.iter().map(|t| t.edge).collect();
        let graph = model.graph();

        let ids: Vec<NodeId> = graph.nodes.iter().map(|n| n.id).collect();
        assert_eq!(ids, [osc, filter, lfo, env, NodeId::MASTER]);
        assert_eq!((node(&graph, lfo).parent, node(&graph, env).parent), (Some(osc), Some(osc)));
        assert_eq!(node(&graph, lfo).name, "LFO 2.0Hz saw");
        assert_eq!(node(&graph, env).name, "ADSR");
        assert_eq!(
            node(&graph, lfo).kind,
            NodeKind::Modulator { source: ModulatorSource::Lfo { waveform: Waveform::Saw, rate: 2.0 } }
        );
        assert_eq!(
            node(&graph, env).kind,
            NodeKind::Modulator { source: ModulatorSource::Envelope { attack: 0.02, decay: 0.4, sustain: 0.5, release: 1.0 } }
        );

        let modulation_edges: Vec<&Edge> = graph.edges.iter().filter(|e| matches!(e.kind, EdgeKind::Modulation { .. })).collect();
        let expected = [
            // The oscillator's detune is its parameter 1, the filter's cutoff its parameter 0.
            modulation(lfo, param(osc, 1), lfo_edges[0], 0.1),
            modulation(lfo, param(filter, 0), lfo_edges[1], -0.3),
            // The envelope drives the LFO's rate and the depth of the LFO's second edge,
            // and (`mod_sustain = 1` is the envelope itself) its own sustain.
            modulation(env, at(lfo, Port::Rate), env_edges[0], 0.5),
            modulation(env, at(lfo, Port::Depth { edge: lfo_edges[1] }), env_edges[1], 0.2),
            modulation(env, at(env, Port::Sustain), env_edges[2], 0.1),
        ];
        assert_eq!(modulation_edges, expected.iter().collect::<Vec<_>>());
    }

    #[test]
    fn the_audio_edges_come_before_the_modulation_edges() {
        let model = load(MODULATED).model;
        let graph = model.graph();
        let first_modulation = graph.edges.iter().position(|e| matches!(e.kind, EdgeKind::Modulation { .. })).unwrap();
        assert!(graph.edges[..first_modulation].iter().all(|e| e.kind == EdgeKind::Audio));
        assert!(graph.edges[first_modulation..].iter().all(|e| matches!(e.kind, EdgeKind::Modulation { .. })));
    }

    #[test]
    fn every_kind_of_modulator_source_is_described() {
        let model = load(MIDI).model;
        let graph = model.graph();
        let lane = &model.instruments[0];
        let sources: Vec<&ModulatorSource> = lane
            .modulators
            .iter()
            .map(|m| match &node(&graph, m.node).kind {
                NodeKind::Modulator { source } => source,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            sources,
            [
                &ModulatorSource::MidiLearn { control: Some(MidiControl::Cc { number: 74 }), learning: false },
                &ModulatorSource::MidiLearn { control: None, learning: true },
                &ModulatorSource::MidiLearn { control: Some(MidiControl::PitchBend), learning: false },
                &ModulatorSource::MidiLearn { control: Some(MidiControl::Aftertouch), learning: false },
            ]
        );
        let names: Vec<&str> = lane.modulators.iter().map(|m| node(&graph, m.node).name.as_str()).collect();
        assert_eq!(names, ["CC 74", "MIDI (learning…)", "Pitch Bend", "Aftertouch"]);
    }

    #[test]
    fn every_waveform_is_named() {
        for (name, waveform) in [("sine", Waveform::Sine), ("triangle", Waveform::Triangle), ("saw", Waveform::Saw), ("square", Waveform::Square)] {
            let model = load(&format!("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\nwaveform = \"{name}\"\n")).model;
            let graph = model.graph();
            let m = model.instruments[0].modulators[0].node;
            assert_eq!(
                node(&graph, m).kind,
                NodeKind::Modulator { source: ModulatorSource::Lfo { waveform, rate: 1.0 } },
                "{name}"
            );
        }
    }

    #[test]
    fn a_group_modulator_reaches_into_its_members_and_its_bus() {
        let model = load(GROUPS).model;
        let group = &model.groups[0];
        let (pad, bus) = (group.node, group.effects[0].node);
        let (lane0, lane1) = (model.instruments[0].node, model.instruments[1].node);
        let filter = model.instruments[1].effects[0].node;
        let lane1_lfo = model.instruments[1].modulators[0].node;
        let (lfo, env) = (group.modulators[0].node, group.modulators[1].node);
        let lfo_edges: Vec<EdgeId> = group.modulators[0].targets.iter().map(|t| t.edge).collect();
        let env_edges: Vec<EdgeId> = group.modulators[1].targets.iter().map(|t| t.edge).collect();
        let graph = model.graph();

        assert_eq!((node(&graph, lfo).parent, node(&graph, env).parent), (Some(pad), Some(pad)));
        let edges: Vec<&Edge> = graph.edges.iter().filter(|e| matches!(e.kind, EdgeKind::Modulation { .. })).collect();
        let expected = [
            // Member 0's detune (the oscillator's parameter 1).
            modulation(lfo, param(lane0, 1), lfo_edges[0], 0.4),
            // Member 1's effect in slot 1: the filter's cutoff, parameter 0.
            modulation(lfo, param(filter, 0), lfo_edges[1], -0.3),
            // The bus effect's damping: the reverb's parameter 2.
            modulation(lfo, param(bus, 2), lfo_edges[2], 0.25),
            // Member 1's own LFO's rate.
            modulation(lfo, at(lane1_lfo, Port::Rate), lfo_edges[3], 0.2),
            // The second group modulator drives the first one's rate, and the depth of its edge 2.
            modulation(env, at(lfo, Port::Rate), env_edges[0], 0.3),
            modulation(env, at(lfo, Port::Depth { edge: lfo_edges[2] }), env_edges[1], 0.1),
        ];
        assert_eq!(edges, expected.iter().collect::<Vec<_>>());
        assert_ne!(lane0, lane1);
    }

    #[test]
    fn a_target_whose_aim_is_gone_has_no_edge() {
        use crate::plugin::chain::ModTargetKind as K;
        let mut model = load(MODULATED).model;
        let lfo = &mut model.instruments[0].modulators[0];
        let real = lfo.targets.len();
        let template = |kind: K| crate::app::model::ModTargetSlot {
            edge: EdgeId(1_000),
            slot: 0,
            param_name: "x".to_string(),
            kind,
            depth: 0.5,
            param_min: 0.0,
            param_max: 1.0,
            effective: None,
        };
        for (n, kind) in [
            K::PluginParam { slot: 9, param_index: 0 },
            K::ModulatorRate { mod_index: 9 },
            K::ModulatorDepth { mod_index: 0, target_index: 99 },
            K::ModulatorAttack { mod_index: 9 },
            K::GroupBus { effect_index: 0, param_index: 0 },
            K::GroupMember { member: 0, slot: 0, param_index: 0 },
        ]
        .into_iter()
        .enumerate()
        {
            lfo.targets.push(ModTargetSlot { edge: EdgeId(1_000 + n as u32), ..template(kind) });
        }
        let graph = model.graph();
        let edges = graph.edges.iter().filter(|e| matches!(e.kind, EdgeKind::Modulation { .. })).count();
        assert_eq!(edges, 5, "only the {real} real targets of the LFO and the envelope's three");
    }

    #[test]
    fn a_group_target_whose_aim_is_gone_has_no_edge() {
        use crate::plugin::chain::{CrossModField, ModTargetKind as K};
        let mut model = load(GROUPS).model;
        let before = model.graph().edges.len();
        let rack = &mut model.groups[0].modulators[0];
        for (n, kind) in [
            K::GroupMember { member: 9, slot: 0, param_index: 0 },
            K::GroupMember { member: 0, slot: 9, param_index: 0 },
            K::GroupBus { effect_index: 9, param_index: 0 },
            K::GroupMemberMod { member: 9, mod_index: 0, field: CrossModField::Rate },
            K::GroupMemberMod { member: 1, mod_index: 9, field: CrossModField::Rate },
            K::GroupMemberMod { member: 1, mod_index: 0, field: CrossModField::Depth(9) },
            K::ModulatorRate { mod_index: 9 },
            K::PluginParam { slot: 0, param_index: 0 },
        ]
        .into_iter()
        .enumerate()
        {
            rack.targets.push(ModTargetSlot {
                edge: EdgeId(2_000 + n as u32),
                slot: 0,
                param_name: "x".to_string(),
                kind,
                depth: 0.5,
                param_min: 0.0,
                param_max: 1.0,
                effective: None,
            });
        }
        assert_eq!(model.graph().edges.len(), before);
    }

    /// The modulation edges of `graph` as `(from, port, amount)`: the modulator
    /// they start at, the port they end at, and their amount.
    fn modulation_of(graph: &Graph) -> Vec<(NodeId, Endpoint, f32)> {
        graph
            .edges
            .iter()
            .filter_map(|e| match e.kind {
                EdgeKind::Modulation { amount, .. } => Some((e.from.node, e.to, amount)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_modulator_can_drive_every_field_of_another() {
        let model = load(
            r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.modulator]]
type = "lfo"
[[instrument.modulator.target]]
param = "detune"
[[instrument.modulator.target]]
param = "volume"
[[instrument.modulator]]
type = "envelope"
[[instrument.modulator]]
type = "lfo"
[[instrument.modulator.target]]
mod_rate = 0
depth = 0.1
[[instrument.modulator.target]]
mod_attack = 1
depth = 0.2
[[instrument.modulator.target]]
mod_decay = 1
depth = 0.3
[[instrument.modulator.target]]
mod_sustain = 1
depth = 0.4
[[instrument.modulator.target]]
mod_release = 1
depth = 0.5
[[instrument.modulator.target]]
mod_depth = [0, 1]
depth = 0.6
"#,
        )
        .model;
        let lane = &model.instruments[0];
        let (lfo, env, driver) = (lane.modulators[0].node, lane.modulators[1].node, lane.modulators[2].node);
        let second_edge = lane.modulators[0].targets[1].edge;
        let graph = model.graph();
        let got = modulation_of(&graph);
        let driven = |port: Port, node: NodeId| at(node, port);
        assert_eq!(
            got[2..],
            [
                (driver, driven(Port::Rate, lfo), 0.1),
                (driver, driven(Port::Attack, env), 0.2),
                (driver, driven(Port::Decay, env), 0.3),
                (driver, driven(Port::Sustain, env), 0.4),
                (driver, driven(Port::Release, env), 0.5),
                (driver, driven(Port::Depth { edge: second_edge }, lfo), 0.6),
            ]
        );
        assert_closed(&graph, "every field");
    }

    #[test]
    fn a_group_modulator_can_drive_every_field_of_a_members_modulator() {
        let model = load(
            r#"
[[group]]
[[group.modulator]]
type = "lfo"
[[group.modulator.target]]
member = 0
mod_rate = 0
depth = 0.1
[[group.modulator.target]]
member = 0
mod_attack = 1
depth = 0.2
[[group.modulator.target]]
member = 0
mod_decay = 1
depth = 0.3
[[group.modulator.target]]
member = 0
mod_sustain = 1
depth = 0.4
[[group.modulator.target]]
member = 0
mod_release = 1
depth = 0.5
[[group.modulator.target]]
member = 0
mod_depth = [0, 1]
depth = 0.6

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.modulator]]
type = "lfo"
[[instrument.modulator.target]]
param = "detune"
[[instrument.modulator.target]]
param = "volume"
[[instrument.modulator]]
type = "envelope"
"#,
        )
        .model;
        let lane = &model.instruments[0];
        let (lfo, env) = (lane.modulators[0].node, lane.modulators[1].node);
        let second_edge = lane.modulators[0].targets[1].edge;
        let driver = model.groups[0].modulators[0].node;
        let graph = model.graph();
        assert_eq!(
            modulation_of(&graph)[..6],
            [
                (driver, at(lfo, Port::Rate), 0.1),
                (driver, at(env, Port::Attack), 0.2),
                (driver, at(env, Port::Decay), 0.3),
                (driver, at(env, Port::Sustain), 0.4),
                (driver, at(env, Port::Release), 0.5),
                (driver, at(lfo, Port::Depth { edge: second_edge }), 0.6),
            ]
        );
        assert_closed(&graph, "every member field");
    }

    #[test]
    fn a_group_modulators_members_are_counted_within_its_own_group() {
        let model = load(
            r#"
[[group]]
[[group.modulator]]
[[group.modulator.target]]
member = 0
param = "detune"
depth = 0.1
[[group]]
[[group.modulator]]
[[group.modulator.target]]
member = 0
param = "detune"
depth = 0.2
[[group.modulator.target]]
member = 1
param = "detune"
depth = 0.3

[[instrument]]
plugin = "builtin:osc"
group = 1
[[instrument]]
plugin = "builtin:osc"
[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument]]
plugin = "builtin:osc"
group = 1
"#,
        )
        .model;
        let lanes: Vec<NodeId> = model.instruments.iter().map(|l| l.node).collect();
        let (first, second) = (model.groups[0].modulators[0].node, model.groups[1].modulators[0].node);
        let graph = model.graph();
        // Group 0's member 0 is lane 2; group 1's members are lanes 0 and 3, and lane 1 is nobody's.
        assert_eq!(
            modulation_of(&graph),
            [(first, param(lanes[2], 1), 0.1), (second, param(lanes[0], 1), 0.2), (second, param(lanes[3], 1), 0.3)]
        );
    }

    #[test]
    fn a_group_modulator_can_drive_any_of_the_bus_effects() {
        let model = load(
            r#"
[[group]]
[[group.effect]]
plugin = "builtin:reverb"
[[group.effect]]
plugin = "builtin:filter"
[[group.modulator]]
[[group.modulator.target]]
bus = 1
param = "cutoff"
depth = 0.1
[[group.modulator.target]]
bus = 0
param = "damping"
depth = 0.2
"#,
        )
        .model;
        let (reverb, filter) = (model.groups[0].effects[0].node, model.groups[0].effects[1].node);
        let driver = model.groups[0].modulators[0].node;
        assert_eq!(
            modulation_of(&model.graph()),
            [(driver, param(filter, 0), 0.1), (driver, param(reverb, 2), 0.2)]
        );
    }

    #[test]
    fn a_target_of_a_missing_member_is_not_taken_for_another() {
        let model = load(
            r#"
[[group]]
[[group.modulator]]
[[group.modulator.target]]
member = 1
param = "detune"
depth = 0.1
[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument]]
plugin = "builtin:osc"
"#,
        )
        .model;
        // The loader may keep the target; it points at a member that is not there, and a
        // lane that is not in the group is not that member.
        assert_eq!(modulation_of(&model.graph()), []);
    }

    #[test]
    fn the_group_modulators_edges_come_before_the_lane_modulators() {
        let model = load(
            r#"
[[group]]
[[group.modulator]]
[[group.modulator.target]]
member = 0
param = "detune"
depth = 0.1
[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.modulator]]
[[instrument.modulator.target]]
param = "detune"
depth = 0.2
"#,
        )
        .model;
        let amounts: Vec<f32> = modulation_of(&model.graph()).iter().map(|(_, _, amount)| *amount).collect();
        assert_eq!(amounts, [0.1, 0.2]);
    }

    #[test]
    fn a_row_that_is_not_a_parameter_is_left_out_and_a_preset_is_described() {
        use crate::app::model::{ParamKind, ParamSlot};
        let mut model = load("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:reverb\"\n").model;
        let reverb = &mut model.instruments[0].effects[0];
        reverb.params.push(ParamSlot { name: "rule".to_string(), index: 99, kind: ParamKind::Separator, ..Default::default() });
        reverb.current_preset = Some("Cathedral".to_string());
        let id = reverb.node;
        let graph = model.graph();
        let plugin = plugin_of(node(&graph, id));
        assert!(plugin.params.iter().all(|p| p.index != 99), "the separator is not a parameter");
        assert_eq!(plugin.params.len(), 10, "and the ten real ones are");
        assert_eq!(plugin.current_preset.as_deref(), Some("Cathedral"));
        let presets: Vec<(&str, &str)> = plugin.presets.iter().map(|p| (p.id.as_str(), p.name.as_str())).collect();
        assert_eq!(presets[..2], [("arcadia", "Arcadia Dream Hall"), ("cathedral", "Cathedral")]);
    }

    // -- The whole graph --

    /// What has to hold of any graph: ids are unique, every edge joins ports that
    /// exist, and every parent is a node.
    fn assert_closed(graph: &Graph, what: &str) {
        let mut ids = std::collections::HashSet::new();
        for n in &graph.nodes {
            assert!(ids.insert(n.id), "{what}: {:?} twice", n.id);
        }
        assert_eq!(graph.nodes.iter().filter(|n| matches!(n.kind, NodeKind::Master)).count(), 1, "{what}: one master");
        for n in &graph.nodes {
            if let Some(parent) = n.parent {
                assert!(ids.contains(&parent), "{what}: the parent of {:?}", n.id);
            }
        }
        let modulation_ids: std::collections::HashSet<EdgeId> = graph
            .edges
            .iter()
            .filter_map(|e| match e.kind {
                EdgeKind::Modulation { id, .. } => Some(id),
                _ => None,
            })
            .collect();
        let count = graph.edges.iter().filter(|e| matches!(e.kind, EdgeKind::Modulation { .. })).count();
        assert_eq!(modulation_ids.len(), count, "{what}: modulation edge ids are unique");
        for e in &graph.edges {
            for end in [e.from, e.to] {
                let n = graph.nodes.iter().find(|n| n.id == end.node).unwrap_or_else(|| panic!("{what}: {e:?} has no node"));
                match (end.port, &n.kind) {
                    (Port::AudioOut, NodeKind::Instrument { .. } | NodeKind::Effect { .. } | NodeKind::Group { .. }) => {}
                    (Port::AudioIn, NodeKind::Effect { .. } | NodeKind::Group { .. } | NodeKind::Master) => {}
                    (Port::ModOut, NodeKind::Modulator { .. }) => {}
                    (Port::Param { index }, NodeKind::Instrument { plugin: Some(p), .. } | NodeKind::Effect { plugin: p, .. }) => {
                        assert!(p.params.iter().any(|q| q.index == index), "{what}: {e:?} names a parameter {index} the plugin lacks");
                    }
                    (Port::Rate | Port::Attack | Port::Decay | Port::Sustain | Port::Release, NodeKind::Modulator { .. }) => {}
                    (Port::Depth { edge }, NodeKind::Modulator { .. }) => {
                        assert!(modulation_ids.contains(&edge), "{what}: {e:?} names an edge that is not there");
                    }
                    (port, kind) => panic!("{what}: {port:?} on {kind:?}"),
                }
            }
            match e.kind {
                EdgeKind::Modulation { .. } => assert_eq!(e.from.port, Port::ModOut, "{what}"),
                EdgeKind::Audio => {
                    assert_eq!((e.from.port, e.to.port), (Port::AudioOut, Port::AudioIn), "{what}");
                }
                EdgeKind::Member => {
                    assert!(matches!(node(graph, e.to.node).kind, NodeKind::Group { .. }), "{what}: members go to groups");
                    assert_eq!((e.from.port, e.to.port), (Port::AudioOut, Port::AudioIn), "{what}");
                }
            }
        }
    }

    #[test]
    fn every_scene_projects_into_a_closed_graph() {
        for (what, scene) in [
            ("OSC", OSC),
            ("GROUPED", GROUPED),
            ("MODULATED", MODULATED),
            ("GROUPS", GROUPS),
            ("LEGACY", LEGACY),
            ("MIDI", MIDI),
            ("SETTINGS", SETTINGS),
            ("DISABLED_PATTERN", DISABLED_PATTERN),
            ("split", include_str!("../../examples/split.toml")),
        ] {
            let model = load(scene).model;
            assert_closed(&model.graph(), what);
        }
    }

    #[test]
    fn every_node_of_the_model_is_a_node_of_the_graph() {
        for scene in [GROUPED, MODULATED, GROUPS, LEGACY] {
            let model = load(scene).model;
            let graph = model.graph();
            let in_model: std::collections::HashSet<NodeId> = model.nodes().map(|(id, _)| id).collect();
            let in_graph: std::collections::HashSet<NodeId> =
                graph.nodes.iter().map(|n| n.id).filter(|id| *id != NodeId::MASTER).collect();
            assert_eq!(in_graph, in_model);
            assert_eq!(graph.nodes.len(), in_model.len() + 1, "and once each, with the master");
        }
    }

    #[test]
    fn the_master_is_not_a_node_of_the_model() {
        let model = load(GROUPS).model;
        assert_eq!(model.locate(NodeId::MASTER), None);
        assert!(model.nodes().all(|(id, _)| id != NodeId::MASTER));
    }

    #[test]
    fn a_session_with_nothing_in_it_is_just_the_master() {
        let graph = SessionModel::default().graph();
        assert_eq!(graph.nodes.len(), 1);
        assert_eq!(graph.nodes[0].id, NodeId::MASTER);
        assert!(graph.edges.is_empty());
    }

    // -- The wire --

    #[test]
    fn nodes_edges_and_ports_are_tagged_objects_in_camel_case() {
        let node = Node {
            id: NodeId(3),
            parent: Some(NodeId(1)),
            name: "Hall".to_string(),
            kind: NodeKind::Effect {
                plugin: Plugin {
                    id: "builtin:reverb".to_string(),
                    format: "Built-in".to_string(),
                    is_instrument: false,
                    params: vec![Param {
                        index: 2,
                        name: "decay".to_string(),
                        min: 0.0,
                        max: 1.0,
                        default: 0.5,
                        value: 0.75,
                        labels: None,
                        unit: Some("s".to_string()),
                        group: vec!["Tail".to_string()],
                        step_count: 0,
                    }],
                    presets: vec![Preset { id: "room".to_string(), name: "Small Room".to_string() }],
                    current_preset: None,
                },
                mix: 0.5,
            },
        };
        assert_eq!(
            serde_json::to_value(&node).unwrap(),
            json!({
                "id": 3,
                "parent": 1,
                "name": "Hall",
                "kind": "effect",
                "plugin": {
                    "id": "builtin:reverb",
                    "format": "Built-in",
                    "isInstrument": false,
                    "params": [{
                        "index": 2, "name": "decay", "min": 0.0, "max": 1.0, "default": 0.5, "value": 0.75,
                        "labels": null, "unit": "s", "group": ["Tail"], "stepCount": 0
                    }],
                    "presets": [{"id": "room", "name": "Small Room"}],
                    "currentPreset": null,
                },
                "mix": 0.5,
            })
        );

        let edge = modulation(NodeId(7), at(NodeId(3), Port::Depth { edge: EdgeId(4) }), EdgeId(9), 0.5);
        assert_eq!(
            serde_json::to_value(&edge).unwrap(),
            json!({
                "from": {"node": 7, "port": {"type": "modOut"}},
                "to": {"node": 3, "port": {"type": "depth", "edge": 4}},
                "kind": "modulation",
                "id": 9,
                "amount": 0.5,
            })
        );
        assert_eq!(
            serde_json::to_value(audio(out(NodeId(1)), into(NodeId(2)))).unwrap(),
            json!({"from": {"node": 1, "port": {"type": "audioOut"}}, "to": {"node": 2, "port": {"type": "audioIn"}}, "kind": "audio"})
        );
        assert_eq!(
            serde_json::to_value(member(out(NodeId(1)), into(NodeId(2)))).unwrap()["kind"],
            json!("member")
        );
        assert_eq!(serde_json::to_value(Port::Param { index: 5 }).unwrap(), json!({"type": "param", "index": 5}));
        for (port, name) in [
            (Port::Rate, "rate"),
            (Port::Attack, "attack"),
            (Port::Decay, "decay"),
            (Port::Sustain, "sustain"),
            (Port::Release, "release"),
        ] {
            assert_eq!(serde_json::to_value(port).unwrap(), json!({ "type": name }));
        }
    }

    #[test]
    fn modulator_sources_and_controls_are_tagged_objects() {
        assert_eq!(
            serde_json::to_value(ModulatorSource::Lfo { waveform: Waveform::Saw, rate: 2.0 }).unwrap(),
            json!({"type": "lfo", "waveform": "saw", "rate": 2.0})
        );
        assert_eq!(
            serde_json::to_value(ModulatorSource::Envelope { attack: 0.5, decay: 0.25, sustain: 1.0, release: 2.0 }).unwrap(),
            json!({"type": "envelope", "attack": 0.5, "decay": 0.25, "sustain": 1.0, "release": 2.0})
        );
        assert_eq!(
            serde_json::to_value(ModulatorSource::MidiLearn { control: Some(MidiControl::Cc { number: 74 }), learning: false }).unwrap(),
            json!({"type": "midiLearn", "control": {"type": "cc", "number": 74}, "learning": false})
        );
        assert_eq!(serde_json::to_value(MidiControl::PitchBend).unwrap(), json!({"type": "pitchBend"}));
        assert_eq!(
            serde_json::to_value(NodeKind::Instrument { plugin: None, range: Some(NoteRange { low: 36, high: 83 }), transpose: -12, volume: 0.5 })
                .unwrap(),
            json!({"kind": "instrument", "plugin": null, "range": {"low": 36, "high": 83}, "transpose": -12, "volume": 0.5})
        );
        assert_eq!(serde_json::to_value(NodeKind::Master).unwrap(), json!({"kind": "master"}));
        assert_eq!(serde_json::to_value(NodeKind::Group { volume: 1.0, pan: 0.0 }).unwrap(), json!({"kind": "group", "volume": 1.0, "pan": 0.0}));
    }

    #[test]
    fn a_graph_comes_back_from_json_as_it_went() {
        for scene in [GROUPED, MODULATED, GROUPS, MIDI] {
            let graph = load(scene).model.graph();
            let json = serde_json::to_string(&graph).unwrap();
            assert_eq!(serde_json::from_str::<Graph>(&json).unwrap(), graph);
        }
    }
}
