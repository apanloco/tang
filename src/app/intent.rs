//! What a front-end can ask of the controller, and what comes back; see
//! `docs/web-ui.md` §5.
//!
//! An `Intent` names the node it concerns by `NodeId`, so it stays valid while
//! other nodes come and go. `Controller::apply` carries it out on the model and
//! the audio thread and answers with the `Event`s that describe what changed, or
//! says why it could not. This is the first family; the rest of §5 follows.
//!
//! Intents and events are also what the browser sends and receives, as JSON: an
//! object tagged with its camel-case name, as in
//! `{"type":"setParam","node":3,"param":2,"value":0.5}`; a gain is a string
//! (`"groupPan"`); a node is a bare number. The TypeScript is generated from the
//! same types (`app::protocol`).

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::app::graph::{Endpoint, NoteRange};
use crate::app::model::{EdgeId, ModSourceSlot, NodeId};
use crate::session::NodeUi;

/// What a front-end asks of the controller, naming the node it concerns.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Intent {
    /// Set a parameter of the plugin in an instrument, effect or bus-effect node.
    /// `param` is the plugin's own index for it. The controller clamps `value` to
    /// the parameter's range and rounds an enum parameter to a whole value.
    SetParam { node: NodeId, param: u32, value: f32 },
    /// Set a host-side gain: an instrument's volume, a group's volume or pan, or
    /// the dry/wet mix of an effect or bus effect. `gain` says which, and the node
    /// has to be one it applies to. The controller clamps `value` to the gain's
    /// range.
    SetGain { node: NodeId, gain: Gain, value: f32 },
    /// Load one of the plugin's presets, by its id, into an instrument or an
    /// effect node. The parameters take the preset's values.
    LoadPreset { node: NodeId, preset: String },
    /// Set one setting of a modulator (lane or group): an LFO's rate or waveform, an
    /// envelope's stage. `value` is clamped to the setting's range, and a waveform is
    /// rounded to a whole value, an index (see `ModulatorField`). A modulator has no
    /// setting that is not its kind's: an LFO has no attack.
    SetModulatorField { node: NodeId, field: ModulatorField, value: f32 },
    /// Set how deep a modulation cable pulls, -1 to 1 (a negative depth pulls the
    /// other way). The controller clamps it.
    SetEdgeDepth { edge: EdgeId, depth: f32 },
    /// Take a modulation cable out: the modulator stays, no longer driving what the cable led
    /// to. The cables that drive the depth of this one go with it (and theirs, in turn), since
    /// what they drove is gone.
    RemoveEdge { edge: EdgeId },
    /// Add a modulator to a lane or to a group, after the ones it has: `parent` is the node of the
    /// lane (its instrument's, which an empty lane has too) or of the group; any other node is
    /// refused. It drives nothing yet.
    AddModulator { parent: NodeId, kind: ModulatorKind },
    /// Draw a modulation cable from the modulator `modulator` to the port `to` of a node: a plugin's parameter,
    /// or a setting of another modulator. A parameter has to be one that modulator reaches: a lane's modulator
    /// reaches the plugins in its lane (the instrument and its effects), a group's reaches those of the lanes
    /// that are its members and its own bus effects. A setting (`rate` of an LFO, `attack`, `decay`, `sustain` or
    /// `release` of an envelope) or the `depth` of one of the cables of a modulator has to be that of a modulator
    /// in reach: a lane's modulator reaches the others of its lane, a group's its members' modulators and the
    /// others of its own, and none reaches itself. The cable starts at half depth (`NEW_CABLE_DEPTH`;
    /// `SetEdgeDepth` changes it). Refused, with nothing changed, for a port that is none of these (an output, an
    /// audio port), a node or a plugin that is not there or out of reach, a node that is no modulator when the port
    /// is a modulator's, a setting its modulator does not have, a depth that is not one of its cables, and a
    /// modulator that drives that port already.
    AddModEdge { modulator: NodeId, to: Endpoint },
    /// Put a plugin at the end of the effect chain of a lane or of a group's bus: `parent` is the node of the lane
    /// (its instrument's, which an empty lane has too) or of the group, and `plugin` the `id` of an entry of the
    /// catalog the server told (`ServerMessage::Catalog`): it is loaded by the server, which loads only what its own
    /// catalog names, and not an instrument. It starts at the mix that suits it (a filter fully wet, the rest half
    /// wet). Refused, with nothing changed, for a parent that is not a lane or a group, a plugin the catalog does
    /// not name, an instrument, one that does not load, and one whose outputs are not the chain's channels.
    AddEffect { parent: NodeId, plugin: String },
    /// Add a lane at the end with an instrument in it: `plugin` is the `id` of an entry of the catalog the server told, which has
    /// to be an instrument with an audio output; it is loaded by the server, which loads only what its own catalog names. The lane
    /// is full range, in no group, and has a meter of its own (and the master's meter hears it). Refused, with nothing changed and
    /// no lane made, for a plugin the catalog does not name, an effect, one that does not load, and one with no audio output.
    AddInstrument { plugin: String },
    /// Put an instrument in a lane, replacing the one it has (an empty lane is filled): `node` is the node of the lane, which the
    /// instrument keeps, and `plugin` is as for `AddInstrument`. The lane keeps its effects, modulators, range, volume and group.
    /// The cables that drove the old plugin's parameters (a lane's and a group's, with what drives their depths) are taken out,
    /// since the new plugin has parameters of its own, and a note remap goes with the old plugin. Refused, with nothing changed,
    /// for a node that is not a lane, and for what `AddInstrument` refuses.
    ReplaceInstrument { node: NodeId, plugin: String },
    /// Set the keys a lane plays: `node` is the node of the lane (its instrument's, which an empty lane has too), and
    /// `range` the notes it takes, both ends included, or `None` for every note. A lane that is given a range
    /// plays no note outside it (a held note that was counted under the old range is let go). The ends are notes,
    /// 0 to 127, and the low one is not above the high one. Refused, with nothing changed, for a node that is not
    /// a lane and for a range that is not a range of notes.
    SetRange { node: NodeId, range: Option<NoteRange> },
    /// Set the scale of the piano (the Piano tab's): `scale` is a root and a kind of scale as the session file
    /// says it, `C major` or `F# dorian`, where the root may be a sharp or a flat, and the name of the kind
    /// short or long, in any case. The piano colours the notes of the scale, and drops the others when it is
    /// locked (`SetLocked`). The scale is saved with the session. Refused, with nothing changed, for text that is
    /// not a scale.
    SetScale { scale: String },
    /// Lock the piano to its scale, so that a note that is not in it is dropped where it is sent (from the page,
    /// a MIDI keyboard or the computer's keys), or let every note through, with the ones outside the scale only
    /// coloured. It is saved with the session.
    SetLocked { locked: bool },
    /// Say where a front-end draws a node, in the pixels of its canvas, or `None` to give the node back to the
    /// front-end's own layout. `node` is any node of the session, the master (`NodeId::MASTER`) included. A place is
    /// kept to a tenth of a pixel and to 100 000 either way, which the controller does for the client; the engine has
    /// no use for it, and the TUI does not show it, but it is saved with the session (the node's `ui` in the file) and
    /// told to every client. Refused, with nothing changed, for a node that is not there and for numbers that are not
    /// numbers.
    SetNodeUi { node: NodeId, ui: Option<NodeUi> },
    /// Say which parameters of a plugin a front-end shows as knobs on its node: `node` is the node of an instrument (a lane's) or of an
    /// effect or a bus effect, and `params` the plugin's own indices of the parameters (`Param::index`), in the order the front-end shows
    /// them; none gives the node back to the front-end's own choice (the page shows the first few). It replaces the list there was. The
    /// engine has no use for it, and the TUI does not show it, but it is saved with the session (the plugin's `pinned`, by name) and told
    /// to every client. It belongs to the plugin: an effect that is moved takes it along, and an instrument that is replaced has none.
    /// Refused, with nothing changed, for a node that has no plugin, a parameter the plugin does not have, a separator, a parameter
    /// listed twice and more than `MAX_PINNED` of them.
    SetPinned { node: NodeId, params: Vec<u32> },
    /// Add a group, a submix bus, at the end: it has no effects and no modulators, full volume and a centred pan, and a meter of
    /// its own. `members` are lanes (each the node of a lane, its instrument's, which an empty lane has too) that are put in it
    /// at once, as `SetLaneGroup` does for each (a lane in another group leaves it); none makes an empty group. Refused, with
    /// nothing changed and no group made, for a member that is not a lane.
    AddGroup { members: Vec<NodeId> },
    /// Put a lane in a group, or take it out of the one it is in (`None`): `node` is the node of the lane (its instrument's, which an
    /// empty lane has too) and `group` the node of a group. A lane is in one group at most: one that is in another leaves it. What it
    /// leaves is no longer driven by it: the cables of the group it leaves to the lane's plugins and its modulators are taken out (with
    /// the cables that drive their depths) and what they drove has the value that was set again, and the cables to the members after
    /// the lane keep driving the same plugins. A lane put in the group it is in is as it was. Refused, with nothing changed, for a
    /// node that is not a lane and for a group that is not a group.
    SetLaneGroup { node: NodeId, group: Option<NodeId> },
    /// Name a group: `name` is what it is called, without the spaces around it; a name of nothing gives the group back the name it has
    /// by default, "Group" and its number, counting from one. A name is at most `MAX_NAME` characters and has no control characters.
    /// The engine has no use for it, but it is saved with the session and told to every client. Refused, with nothing changed, for a
    /// node that is not a group and a name that is not one.
    RenameGroup { node: NodeId, name: String },
    /// Take a node out of the session. A modulator can be, with its own cables and the cables of the others
    /// that drove its settings (and what drove the depth of any of those), and so can an effect or a bus
    /// effect, with the cables that drive its parameters (and what drove their depths): what they were
    /// driving goes with it, and the audio path closes up. A lane and a group are refused so far, as is any
    /// other node.
    RemoveNode { node: NodeId },
    /// Move an effect to another place in its chain: `node` is the node of an effect in a lane's chain or on a group's bus, and `index`
    /// the place it is to have among the effects of that chain, from 0 (the first, the nearest the instrument or the group) to one less
    /// than the chain has. The effects between its old place and its new one move one place the other way, and the audio path goes
    /// through them in the new order. The cables that drive the effect's parameters, and those that drive the other plugins of the lane
    /// or of the bus, stay on the plugins they drove (the controller tells the audio thread the places they have now). Refused, with
    /// nothing changed, for a node that is not an effect (a lane, a group, a modulator), a place the chain has not, and the place the
    /// effect is at.
    MoveEffect { node: NodeId, index: u32 },
    /// Write the session to the file it came from. A client does not name a file: it is the
    /// session's own. Refused (`IntentError::Io`) if the session has no file or it cannot be written,
    /// and then nothing changes.
    Save,
}

/// What a new modulator is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum ModulatorKind {
    /// A sine at 1 Hz.
    Lfo,
    /// An envelope of 10 ms attack, 0.3 s decay, 0.7 sustain and 0.5 s release, opened by the notes
    /// its lane (or its group's members) plays.
    Envelope,
    /// A hardware control, waiting for the next one that comes in to bind to.
    MidiLearn,
}

impl ModulatorKind {
    /// What the modulator is to begin with.
    pub fn source(self) -> ModSourceSlot {
        match self {
            ModulatorKind::Lfo => ModSourceSlot::new_lfo(),
            ModulatorKind::Envelope => ModSourceSlot::new_envelope(),
            ModulatorKind::MidiLearn => ModSourceSlot::new_midi_learn(),
        }
    }
}

/// A setting of a modulator, by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum ModulatorField {
    /// An LFO's rate, in Hz: 0.01 to 50.
    Rate,
    /// An LFO's waveform, as an index: 0 sine, 1 triangle, 2 saw, 3 square.
    Waveform,
    /// An envelope's attack time, in seconds: 0.001 to 10.
    Attack,
    /// An envelope's decay time, in seconds: 0.001 to 10.
    Decay,
    /// An envelope's sustain level: 0 to 1.
    Sustain,
    /// An envelope's release time, in seconds: 0.001 to 10.
    Release,
}

impl ModulatorField {
    /// The lowest and the highest value.
    pub fn range(self) -> (f32, f32) {
        match self {
            ModulatorField::Rate => (0.01, 50.0),
            ModulatorField::Waveform => (0.0, 3.0),
            ModulatorField::Attack | ModulatorField::Decay | ModulatorField::Release => (0.001, 10.0),
            ModulatorField::Sustain => (0.0, 1.0),
        }
    }
}

/// A host-side gain, outside the plugin's own parameter list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum Gain {
    /// An instrument's output volume, applied before its effects: 0 to 4, 1 is
    /// unity.
    Volume,
    /// An effect's, or a bus effect's, dry/wet mix: 0 (dry) to 1 (wet).
    Mix,
    /// A group's output volume: 0 to 4.
    GroupVolume,
    /// A group's stereo balance: -1 (left) to 1 (right).
    GroupPan,
}

impl Gain {
    /// The lowest and the highest value.
    pub fn range(self) -> (f32, f32) {
        match self {
            Gain::Volume | Gain::GroupVolume => (0.0, 4.0),
            Gain::Mix => (0.0, 1.0),
            Gain::GroupPan => (-1.0, 1.0),
        }
    }
}

/// What happened, in terms a front-end can apply to its own copy of the session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Event {
    /// A parameter now has `value` (after clamping and rounding).
    ParamSet { node: NodeId, param: u32, value: f32 },
    /// A gain now has `value` (after clamping).
    GainSet { node: NodeId, gain: Gain, value: f32 },
    /// A preset is loaded, by name; these are the parameters it set, by the
    /// plugin's own index, with their new values.
    PresetLoaded { node: NodeId, preset: String, values: Vec<ParamValue> },
    /// A lane now takes the notes of `range`, or every note if it is `None`.
    RangeSet { node: NodeId, range: Option<NoteRange> },
    /// The piano's scale is now this: `scale` as the session file says it (`F# dorian`), and what a client
    /// needs to draw it without knowing any scale: its `root` (a pitch class, 0 C to 11 B), the short name of
    /// its `kind` (`dorian`, `harm minor`) and the pitch classes of its `notes`, lowest first.
    ScaleSet { scale: String, root: u8, kind: String, notes: Vec<u8> },
    /// The piano is locked to its scale, or it is not.
    LockedSet { locked: bool },
    /// A node is now drawn at `ui` (after rounding and clamping), or wherever the front-end lays it out if it is `None`.
    NodeUiSet { node: NodeId, ui: Option<NodeUi> },
    /// The parameters now pinned to the node's plugin, in order.
    PinnedSet { node: NodeId, params: Vec<u32> },
    /// A group is now called `name`: what it was given, or "Group" and its number if it was given none.
    GroupRenamed { node: NodeId, name: String },
    /// A modulator's setting now has `value` (after clamping, and rounding a waveform).
    ModulatorFieldSet { node: NodeId, field: ModulatorField, value: f32 },
    /// A cable now pulls `depth` (after clamping).
    EdgeDepthSet { edge: EdgeId, depth: f32 },
    /// The session is written: it has nothing unsaved now. `file` is the name of the file it was
    /// written to (not its path), if the session has one.
    Saved { file: Option<String> },
}

/// A parameter, by the plugin's own index, and the value it has.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
pub struct ParamValue {
    pub param: u32,
    pub value: f32,
}

/// An event as it goes out to the clients: the revision of the session it brought
/// about, then the event itself, in one object
/// (`{"rev":9,"type":"paramSet","node":3,...}`). A client that sees a gap in the
/// revisions has missed something and asks for a snapshot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "only the web runtime sends events"))]
pub struct StampedEvent {
    pub rev: u64,
    #[serde(flatten)]
    pub event: Event,
}

/// An intent that was carried out.
#[derive(Clone, Debug, PartialEq)]
pub struct Applied {
    /// The session's revision after the intent: one more than before, every time.
    pub rev: u64,
    pub events: Vec<Event>,
    /// The intent added or took out a node or a cable, which no event describes: a front-end
    /// that draws the structure has to draw it again, and a client is given a snapshot (the one
    /// projection of the graph is the controller's: a client that derived the audio path, the
    /// order of the nodes, from events would be a second one). It follows at once, at the revision
    /// after this one.
    pub restructured: bool,
}

impl Applied {
    /// An intent that changed values, as the `events` tell.
    pub fn of(rev: u64, events: Vec<Event>) -> Applied {
        Applied { rev, events, restructured: false }
    }

    /// An intent that changed the structure of the session.
    pub fn restructured(rev: u64) -> Applied {
        Applied { rev, events: Vec::new(), restructured: true }
    }

    /// The events, each stamped with the revision this intent brought about.
    #[cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "only the web runtime sends events"))]
    pub fn stamped(&self) -> Vec<StampedEvent> {
        self.events.iter().map(|event| StampedEvent { rev: self.rev, event: event.clone() }).collect()
    }
}

/// An intent that was not carried out. Nothing changed.
#[derive(Clone, Debug, PartialEq)]
pub enum IntentError {
    /// No node has that id: it was removed, or never existed.
    NotFound(NodeId),
    /// No modulation cable has that id: it was removed, or never existed.
    EdgeNotFound(EdgeId),
    /// The intent does not make sense for what it names.
    Invalid(String),
    /// A file could not be written: the reason is in the words.
    Io(String),
}

impl fmt::Display for IntentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IntentError::NotFound(node) => write!(f, "there is no node {}", node.0),
            IntentError::EdgeNotFound(edge) => write!(f, "there is no cable {}", edge.0),
            IntentError::Invalid(why) | IntentError::Io(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for IntentError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::graph::{NoteRange, Port};
    use crate::session::NodeUi;
    use serde_json::{Value, json};

    fn wire<T: serde::Serialize>(value: &T) -> Value {
        serde_json::to_value(value).unwrap()
    }

    /// Every intent, and what it looks like as JSON. The values are exact in a
    /// float, so that they read the same as f32 and as f64.
    fn intents() -> Vec<(Intent, Value)> {
        vec![
            (
                Intent::SetParam { node: NodeId(3), param: 2, value: 0.5 },
                json!({"type": "setParam", "node": 3, "param": 2, "value": 0.5}),
            ),
            (
                Intent::SetGain { node: NodeId(4), gain: Gain::GroupPan, value: -0.25 },
                json!({"type": "setGain", "node": 4, "gain": "groupPan", "value": -0.25}),
            ),
            (
                Intent::LoadPreset { node: NodeId(5), preset: "cathedral".to_string() },
                json!({"type": "loadPreset", "node": 5, "preset": "cathedral"}),
            ),
            (Intent::RemoveEdge { edge: EdgeId(6) }, json!({"type": "removeEdge", "edge": 6})),
            (
                Intent::AddModulator { parent: NodeId(2), kind: ModulatorKind::MidiLearn },
                json!({"type": "addModulator", "parent": 2, "kind": "midiLearn"}),
            ),
            (Intent::RemoveNode { node: NodeId(8) }, json!({"type": "removeNode", "node": 8})),
            (Intent::MoveEffect { node: NodeId(7), index: 0 }, json!({"type": "moveEffect", "node": 7, "index": 0})),
            (
                Intent::AddEffect { parent: NodeId(2), plugin: "builtin:filter".to_string() },
                json!({"type": "addEffect", "parent": 2, "plugin": "builtin:filter"}),
            ),
            (
                Intent::AddInstrument { plugin: "builtin:osc".to_string() },
                json!({"type": "addInstrument", "plugin": "builtin:osc"}),
            ),
            (
                Intent::ReplaceInstrument { node: NodeId(2), plugin: "builtin:osc".to_string() },
                json!({"type": "replaceInstrument", "node": 2, "plugin": "builtin:osc"}),
            ),
            (Intent::Save, json!({"type": "save"})),
            (
                Intent::SetRange { node: NodeId(2), range: Some(NoteRange { low: 36, high: 84 }) },
                json!({"type": "setRange", "node": 2, "range": {"low": 36, "high": 84}}),
            ),
            (Intent::SetRange { node: NodeId(2), range: None }, json!({"type": "setRange", "node": 2, "range": null})),
            (Intent::SetScale { scale: "F# dorian".to_string() }, json!({"type": "setScale", "scale": "F# dorian"})),
            (Intent::SetLocked { locked: true }, json!({"type": "setLocked", "locked": true})),
            (
                Intent::SetNodeUi { node: NodeId(2), ui: Some(NodeUi { x: 80.0, y: -12.5 }) },
                json!({"type": "setNodeUi", "node": 2, "ui": {"x": 80.0, "y": -12.5}}),
            ),
            (Intent::SetNodeUi { node: NodeId(2), ui: None }, json!({"type": "setNodeUi", "node": 2, "ui": null})),
            (Intent::SetPinned { node: NodeId(2), params: vec![3, 1] }, json!({"type": "setPinned", "node": 2, "params": [3, 1]})),
            (Intent::SetPinned { node: NodeId(2), params: vec![] }, json!({"type": "setPinned", "node": 2, "params": []})),
            (Intent::AddGroup { members: vec![NodeId(2), NodeId(5)] }, json!({"type": "addGroup", "members": [2, 5]})),
            (Intent::AddGroup { members: vec![] }, json!({"type": "addGroup", "members": []})),
            (
                Intent::SetLaneGroup { node: NodeId(2), group: Some(NodeId(7)) },
                json!({"type": "setLaneGroup", "node": 2, "group": 7}),
            ),
            (Intent::SetLaneGroup { node: NodeId(2), group: None }, json!({"type": "setLaneGroup", "node": 2, "group": null})),
            (
                Intent::RenameGroup { node: NodeId(7), name: "Pad".to_string() },
                json!({"type": "renameGroup", "node": 7, "name": "Pad"}),
            ),
            (
                Intent::AddModEdge { modulator: NodeId(4), to: Endpoint { node: NodeId(1), port: Port::Param { index: 3 } } },
                json!({"type": "addModEdge", "modulator": 4, "to": {"node": 1, "port": {"type": "param", "index": 3}}}),
            ),
        ]
    }

    fn events() -> Vec<(Event, Value)> {
        vec![
            (
                Event::ParamSet { node: NodeId(3), param: 2, value: 0.5 },
                json!({"type": "paramSet", "node": 3, "param": 2, "value": 0.5}),
            ),
            (
                Event::GainSet { node: NodeId(4), gain: Gain::Volume, value: 2.0 },
                json!({"type": "gainSet", "node": 4, "gain": "volume", "value": 2.0}),
            ),
            (
                Event::PresetLoaded {
                    node: NodeId(5),
                    preset: "Cathedral".to_string(),
                    values: vec![ParamValue { param: 0, value: 0.5 }, ParamValue { param: 3, value: 0.25 }],
                },
                json!({
                    "type": "presetLoaded",
                    "node": 5,
                    "preset": "Cathedral",
                    "values": [{"param": 0, "value": 0.5}, {"param": 3, "value": 0.25}],
                }),
            ),
            (
                Event::RangeSet { node: NodeId(2), range: Some(NoteRange { low: 36, high: 84 }) },
                json!({"type": "rangeSet", "node": 2, "range": {"low": 36, "high": 84}}),
            ),
            (Event::RangeSet { node: NodeId(2), range: None }, json!({"type": "rangeSet", "node": 2, "range": null})),
            (
                Event::ScaleSet { scale: "F# dorian".to_string(), root: 6, kind: "dorian".to_string(), notes: vec![1, 3, 4, 6, 8, 9, 11] },
                json!({"type": "scaleSet", "scale": "F# dorian", "root": 6, "kind": "dorian", "notes": [1, 3, 4, 6, 8, 9, 11]}),
            ),
            (Event::LockedSet { locked: false }, json!({"type": "lockedSet", "locked": false})),
            (
                Event::NodeUiSet { node: NodeId(2), ui: Some(NodeUi { x: 80.0, y: -12.5 }) },
                json!({"type": "nodeUiSet", "node": 2, "ui": {"x": 80.0, "y": -12.5}}),
            ),
            (Event::NodeUiSet { node: NodeId(2), ui: None }, json!({"type": "nodeUiSet", "node": 2, "ui": null})),
            (Event::PinnedSet { node: NodeId(2), params: vec![3, 1] }, json!({"type": "pinnedSet", "node": 2, "params": [3, 1]})),
            (Event::PinnedSet { node: NodeId(2), params: vec![] }, json!({"type": "pinnedSet", "node": 2, "params": []})),
            (
                Event::GroupRenamed { node: NodeId(7), name: "Pad".to_string() },
                json!({"type": "groupRenamed", "node": 7, "name": "Pad"}),
            ),
            (Event::Saved { file: Some("song.toml".to_string()) }, json!({"type": "saved", "file": "song.toml"})),
            (Event::Saved { file: None }, json!({"type": "saved", "file": null})),
        ]
    }

    #[test]
    fn an_intent_is_an_object_tagged_with_its_camel_case_name() {
        for (intent, json) in intents() {
            assert_eq!(wire(&intent), json, "{intent:?}");
        }
    }

    #[test]
    fn an_intent_comes_back_from_json_as_it_went() {
        for (intent, json) in intents() {
            assert_eq!(serde_json::from_value::<Intent>(json.clone()).unwrap(), intent, "{json}");
        }
    }

    #[test]
    fn every_gain_has_its_own_name() {
        let names: Vec<Value> = [Gain::Volume, Gain::Mix, Gain::GroupVolume, Gain::GroupPan].iter().map(wire).collect();
        assert_eq!(names, [json!("volume"), json!("mix"), json!("groupVolume"), json!("groupPan")]);
        for name in names {
            assert!(serde_json::from_value::<Gain>(name).is_ok());
        }
    }

    #[test]
    fn every_kind_of_modulator_has_its_own_name() {
        let names: Vec<Value> = [ModulatorKind::Lfo, ModulatorKind::Envelope, ModulatorKind::MidiLearn].iter().map(wire).collect();
        assert_eq!(names, [json!("lfo"), json!("envelope"), json!("midiLearn")]);
        for name in names {
            assert!(serde_json::from_value::<ModulatorKind>(name).is_ok());
        }
    }

    #[test]
    fn what_a_client_sends_that_is_not_an_intent_is_an_error() {
        let bad = [
            ("no type", json!({"node": 3, "param": 2, "value": 0.5})),
            ("an unknown type", json!({"type": "explode", "node": 3})),
            ("a Rust name instead of the wire's", json!({"type": "SetParam", "node": 3, "param": 2, "value": 0.5})),
            ("a missing field", json!({"type": "setParam", "node": 3, "param": 2})),
            ("a value that is not a number (what NaN becomes)", json!({"type": "setParam", "node": 3, "param": 2, "value": null})),
            ("a negative node", json!({"type": "setParam", "node": -1, "param": 2, "value": 0.5})),
            ("a gain that does not exist", json!({"type": "setGain", "node": 3, "gain": "loud", "value": 1.0})),
            ("a node that is a string", json!({"type": "loadPreset", "node": "3", "preset": "x"})),
            ("a kind of modulator that does not exist", json!({"type": "addModulator", "parent": 3, "kind": "noise"})),
            ("a kind spelled the Rust way", json!({"type": "addModulator", "parent": 3, "kind": "MidiLearn"})),
            ("no kind", json!({"type": "addModulator", "parent": 3})),
            ("a scale that is a number", json!({"type": "setScale", "scale": 3})),
            ("no scale", json!({"type": "setScale"})),
            ("a lock that is not a boolean", json!({"type": "setLocked", "locked": "yes"})),
            ("no lock", json!({"type": "setLocked"})),
            ("a place with no y", json!({"type": "setNodeUi", "node": 3, "ui": {"x": 1.0}})),
            ("a place that is a number", json!({"type": "setNodeUi", "node": 3, "ui": 5})),
            ("a place whose x is a string", json!({"type": "setNodeUi", "node": 3, "ui": {"x": "1", "y": 2.0}})),
            ("a place whose x is null (what NaN becomes)", json!({"type": "setNodeUi", "node": 3, "ui": {"x": null, "y": 2.0}})),
            ("pinned parameters with no list", json!({"type": "setPinned", "node": 3})),
            ("pinned parameters that are names", json!({"type": "setPinned", "node": 3, "params": ["cutoff"]})),
            ("a pinned parameter that is negative", json!({"type": "setPinned", "node": 3, "params": [-1]})),
            ("a pinned parameter that is not whole", json!({"type": "setPinned", "node": 3, "params": [1.5]})),
            ("a list of pinned parameters that is a number", json!({"type": "setPinned", "node": 3, "params": 4})),
            ("a group with no members", json!({"type": "addGroup"})),
            ("members that are not nodes", json!({"type": "addGroup", "members": ["a"]})),
            ("a lane's group that is a string", json!({"type": "setLaneGroup", "node": 3, "group": "7"})),
            ("a name that is a number", json!({"type": "renameGroup", "node": 3, "name": 5})),
            ("no name", json!({"type": "renameGroup", "node": 3})),
        ];
        for (what, json) in bad {
            assert!(serde_json::from_value::<Intent>(json).is_err(), "{what}");
        }
    }

    #[test]
    fn an_event_is_an_object_tagged_with_its_camel_case_name() {
        for (event, json) in events() {
            assert_eq!(wire(&event), json, "{event:?}");
            assert_eq!(serde_json::from_value::<Event>(json).unwrap(), event);
        }
    }

    #[test]
    fn the_events_of_an_applied_intent_go_out_stamped_with_its_revision() {
        let applied = Applied::of(
            9,
            vec![
                Event::ParamSet { node: NodeId(3), param: 2, value: 0.5 },
                Event::GainSet { node: NodeId(4), gain: Gain::Mix, value: 1.0 },
            ],
        );
        let stamped = applied.stamped();
        assert_eq!(
            wire(&stamped),
            json!([
                {"rev": 9, "type": "paramSet", "node": 3, "param": 2, "value": 0.5},
                {"rev": 9, "type": "gainSet", "node": 4, "gain": "mix", "value": 1.0},
            ])
        );
        let back: Vec<StampedEvent> = serde_json::from_value(wire(&stamped)).unwrap();
        assert_eq!(back, stamped);
        assert_eq!(back[1], StampedEvent { rev: 9, event: applied.events[1].clone() });
    }

    #[test]
    fn an_applied_intent_without_events_goes_out_as_nothing() {
        assert_eq!(wire(&Applied::of(1, vec![]).stamped()), json!([]));
        // (A change of structure is told as a snapshot, not as events.)
        assert_eq!(wire(&Applied::restructured(2).stamped()), json!([]));
    }
}
