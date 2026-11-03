//! The names of the things in a session: what the model, the engine and the front-ends call a node and a cable by.
//!
//! They are here and not in the model so that the engine can hold them (P2 of `docs/patch-graph.md` §11.6) without reaching into a module that imports the engine. The model
//! hands them out (`app::model::NodeIds`) and re-exports them, so that nothing that names `app::model::NodeId` changes.

/// Names a node of the session (an instrument, an effect, a group, a bus effect
/// or a modulator) for as long as it exists. The engine and the tree address
/// nodes by position, which shifts when others come and go; a `NodeId` does not.
/// Allocated by the model, never reused during a run, not saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
pub struct NodeId(pub u32);

impl NodeId {
    /// The master mix, where every ungrouped lane and every group ends up. It is a
    /// node of the graph a client gets but not of the model: nothing is allocated
    /// with this id, and `SessionModel::locate` does not find it.
    pub const MASTER: NodeId = NodeId(u32::MAX);
}

/// Names a modulation edge, one target of one modulator, for as long as it
/// exists: the cable a client draws from the modulator to what it drives. Edges
/// come and go as targets are bound and removed, and a target is addressed by its
/// position in its modulator, which shifts; an `EdgeId` does not. Allocated by the
/// model, never reused during a run, not saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
pub struct EdgeId(pub u32);

/// The settings of a modulator that a cable can drive: how fast an LFO or a step sequencer goes, and the stages of an envelope. (The depth of a cable is the cable's, `Driven::Depth`;
/// the rest of what a modulator has is a person's to set.)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModSetting {
    Rate,
    Attack,
    Decay,
    Sustain,
    Release,
}

impl ModSetting {
    /// Whether the knob of the setting runs over decades (an LFO's rate, an envelope's attack, decay and release), so that a cable moves it a fraction of the knob's travel, in ratios, and
    /// not a fraction of the range in seconds or hertz.
    pub fn is_turned_on_a_log_scale(self) -> bool {
        matches!(self, ModSetting::Rate | ModSetting::Attack | ModSetting::Decay | ModSetting::Release)
    }
}

/// A value that the host keeps for a node, outside the plugin's own parameter list, and that a cable can drive (step 97): what the intents call a `Gain`, less the difference between a lane's
/// volume and a group's, which the kind of node says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModGain {
    /// The output volume of a lane (applied before its effects) or of a group (applied to the sum of its members): 0 to 4, where 1 is unity.
    Volume,
    /// The dry/wet mix of an effect, a lane's or a bus's: 0 is dry and 1 is wet.
    Mix,
    /// The stereo balance of a group: -1 is left, 0 the middle and 1 right.
    Pan,
}

impl ModGain {
    /// The lowest and the highest value, as `Gain::range` says them.
    pub fn range(self) -> (f32, f32) {
        match self {
            ModGain::Volume => (0.0, 4.0),
            ModGain::Mix => (0.0, 1.0),
            ModGain::Pan => (-1.0, 1.0),
        }
    }

    /// What the value is called, in the tree and in the file (`gain = "mix"`).
    pub fn name(self) -> &'static str {
        match self {
            ModGain::Volume => "volume",
            ModGain::Mix => "mix",
            ModGain::Pan => "pan",
        }
    }

    /// What the value is called in a list that has the plugins' own parameters in it, where a lane's `volume` would be the oscillator's own: the words of the terminal's `v` and `x` (an output
    /// volume, a dry/wet mix) and a group's pan.
    pub fn label(self) -> &'static str {
        match self {
            ModGain::Volume => "output volume",
            ModGain::Mix => "dry/wet mix",
            ModGain::Pan => "pan",
        }
    }

    /// The gain that a word names, exactly as `name` writes it.
    pub fn parse(text: &str) -> Option<ModGain> {
        [ModGain::Volume, ModGain::Mix, ModGain::Pan].into_iter().find(|gain| gain.name() == text)
    }
}

/// What a cable drives, named so that it can be found again once other cables and modulators have come and gone and the places in the racks have moved: a plugin's parameter (by the
/// node of the plugin and the parameter's own index), a modulator's rate or a stage of its envelope (by the node of the modulator), the depth of a cable (by its edge), or a value that
/// the host keeps for a node (`ModGain`: the volume of a lane or a group, the mix of an effect, the pan of a group: by the node it is of). The model keeps it for each cable, and the engine
/// too (P5b-1 of `docs/patch-graph.md` §11.6): neither names a place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Driven {
    Param { plugin: NodeId, index: u32 },
    Setting { modulator: NodeId, field: ModSetting },
    Depth(EdgeId),
    Gain { node: NodeId, gain: ModGain },
}
