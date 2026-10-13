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

use crate::app::model::{EdgeId, NodeId};

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
    /// A modulator's setting now has `value` (after clamping, and rounding a waveform).
    ModulatorFieldSet { node: NodeId, field: ModulatorField, value: f32 },
    /// A cable now pulls `depth` (after clamping).
    EdgeDepthSet { edge: EdgeId, depth: f32 },
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
}

impl Applied {
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
}

impl fmt::Display for IntentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IntentError::NotFound(node) => write!(f, "there is no node {}", node.0),
            IntentError::EdgeNotFound(edge) => write!(f, "there is no cable {}", edge.0),
            IntentError::Invalid(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for IntentError {}

#[cfg(test)]
mod tests {
    use super::*;
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
        let applied = Applied {
            rev: 9,
            events: vec![
                Event::ParamSet { node: NodeId(3), param: 2, value: 0.5 },
                Event::GainSet { node: NodeId(4), gain: Gain::Mix, value: 1.0 },
            ],
        };
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
        assert_eq!(wire(&Applied { rev: 1, events: vec![] }.stamped()), json!([]));
    }
}
