//! What a front-end can ask of the controller, and what comes back; see
//! `docs/web-ui.md` §5.
//!
//! An `Intent` names the node it concerns by `NodeId`, so it stays valid while
//! other nodes come and go. `Controller::apply` carries it out on the model and
//! the audio thread and answers with the `Event`s that describe what changed, or
//! says why it could not. This is the first family; the rest of §5 follows.

use std::fmt;

use crate::app::model::NodeId;

#[derive(Clone, Debug, PartialEq)]
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
}

/// A host-side gain, outside the plugin's own parameter list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A parameter now has `value` (after clamping and rounding).
    ParamSet { node: NodeId, param: u32, value: f32 },
    /// A gain now has `value` (after clamping).
    GainSet { node: NodeId, gain: Gain, value: f32 },
}

/// An intent that was carried out.
#[derive(Clone, Debug, PartialEq)]
pub struct Applied {
    /// The session's revision after the intent: one more than before, every time.
    pub rev: u64,
    pub events: Vec<Event>,
}

/// An intent that was not carried out. Nothing changed.
#[derive(Clone, Debug, PartialEq)]
pub enum IntentError {
    /// No node has that id: it was removed, or never existed.
    NotFound(NodeId),
    /// The intent does not make sense for what it names.
    Invalid(String),
}

impl fmt::Display for IntentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IntentError::NotFound(node) => write!(f, "there is no node {}", node.0),
            IntentError::Invalid(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for IntentError {}
