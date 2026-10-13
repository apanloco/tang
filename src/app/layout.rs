//! What each slot of the telemetry means, for a client that reads them; see
//! `docs/web-ui.md` §6.
//!
//! The audio thread publishes numbers into slots (`telemetry`); the model knows
//! whose slot is whose (`InstrumentNode::meter`, `ModulatorSlot::output`,
//! `ModTargetSlot::effective`, ...). `SessionModel::telemetry_layout` turns that
//! into the list a client is sent, in two channel groups: the `meters` (a peak and
//! a clip count for the master, each group and each lane) and the `modulation`
//! (what each modulator puts out, and the value each modulated parameter is set
//! to). A telemetry frame carries the current values of one group, in the order of
//! its list, so the list is sent once and again only when it changes. Its `rev`
//! says which list a frame belongs to: a fingerprint of the entries, so that it
//! changes exactly when they do, whoever changed them.
//!
//! A parameter that several targets drive has several slots with one value
//! between them (see `docs/web-ui.md` §6); the layout names one, preferring the
//! lane's modulators to the group's, because a lane's modulator is applied after
//! the group's and wins.

#![cfg_attr(not(test), expect(dead_code, reason = "the snapshot a client gets is built from it; the web runtime (step 10) sends that"))]

use std::collections::HashSet;
use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};

use crate::app::graph::{Endpoint, Port};
use crate::app::model::{NodeId, SessionModel};
use crate::plugin::chain::ModulatorLocation;
use crate::telemetry::{self, MeterSlots, Slot};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
pub struct TelemetryLayout {
    /// A fingerprint of the entries: it differs when they do.
    pub rev: u32,
    /// The peaks and the clip counts, of the master, then each group, then each lane.
    pub meters: Vec<LayoutEntry>,
    /// What each modulator puts out, then what each modulated parameter is set to.
    pub modulation: Vec<LayoutEntry>,
}

/// One slot: where it is, and what it holds, at which port of which node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
pub struct LayoutEntry {
    pub slot: u32,
    pub node: NodeId,
    pub port: Port,
    pub kind: SlotKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum SlotKind {
    /// The loudest sample lately at the port, as a float (1.0 is full scale).
    Peak,
    /// How many buffers have clipped at the port; only ever goes up.
    Clips,
    /// What a modulator put out this buffer: an LFO -1 to 1, an envelope or a MIDI
    /// control 0 to 1.
    ModOutput,
    /// The value a parameter was set to this buffer, modulation included.
    Effective,
}

impl TelemetryLayout {
    fn new(meters: Vec<LayoutEntry>, modulation: Vec<LayoutEntry>) -> Self {
        // Fixed keys: the same entries give the same number every time in a run,
        // which is all a client needs to tell whether its list is current.
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (&meters, &modulation).hash(&mut hasher);
        TelemetryLayout { rev: hasher.finish() as u32, meters, modulation }
    }
}

impl SessionModel {
    /// The meaning of every telemetry slot the model has handed out and that still
    /// has something to publish.
    pub fn telemetry_layout(&self) -> TelemetryLayout {
        let mut meters = Vec::new();
        let mut meter = |slots: Option<MeterSlots>, node: NodeId, port: Port| {
            if let Some(slots) = slots {
                meters.push(entry(slots.peak, node, port, SlotKind::Peak));
                meters.push(entry(slots.clips, node, port, SlotKind::Clips));
            }
        };
        meter(Some(telemetry::MASTER), NodeId::MASTER, Port::AudioIn);
        for group in &self.groups {
            meter(group.meter, group.node, Port::AudioOut);
        }
        for lane in &self.instruments {
            meter(lane.meter, lane.node, Port::AudioOut);
        }

        let mut modulation = Vec::new();
        let groups = self.groups.iter().enumerate().map(|(g, group)| (ModulatorLocation::Group(g), &group.modulators));
        let lanes = self.instruments.iter().enumerate().map(|(i, lane)| (ModulatorLocation::Lane(i), &lane.modulators));
        for (_, rack) in groups.clone().chain(lanes.clone()) {
            for modulator in rack {
                if let Some(slot) = modulator.output {
                    modulation.push(entry(slot, modulator.node, Port::ModOut, SlotKind::ModOutput));
                }
            }
        }
        // The lanes' targets first: of two slots for one parameter, theirs holds the final value.
        let mut named: HashSet<(NodeId, Port)> = HashSet::new();
        for (at, rack) in lanes.chain(groups) {
            for modulator in rack {
                for target in &modulator.targets {
                    let Some(slot) = target.effective else { continue };
                    let Some(Endpoint { node, port }) = self.aim(at, rack, &target.kind) else { continue };
                    if named.insert((node, port)) {
                        modulation.push(entry(slot, node, port, SlotKind::Effective));
                    }
                }
            }
        }
        TelemetryLayout::new(meters, modulation)
    }
}

fn entry(slot: Slot, node: NodeId, port: Port, kind: SlotKind) -> LayoutEntry {
    LayoutEntry { slot: slot.index(), node, port, kind }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::app::testutil::{GROUPS, load};

    fn entries(list: &[LayoutEntry]) -> Vec<(u32, NodeId, Port, SlotKind)> {
        list.iter().map(|e| (e.slot, e.node, e.port, e.kind)).collect()
    }

    #[test]
    fn a_session_with_nothing_in_it_has_the_masters_meter() {
        let layout = SessionModel::default().telemetry_layout();
        assert_eq!(
            entries(&layout.meters),
            [
                (0, NodeId::MASTER, Port::AudioIn, SlotKind::Peak),
                (1, NodeId::MASTER, Port::AudioIn, SlotKind::Clips),
            ]
        );
        assert!(layout.modulation.is_empty());
    }

    #[test]
    fn every_group_and_lane_has_its_meter_after_the_masters() {
        let model = load(GROUPS).model;
        let layout = model.telemetry_layout();
        let mut expected = vec![
            (0, NodeId::MASTER, Port::AudioIn, SlotKind::Peak),
            (1, NodeId::MASTER, Port::AudioIn, SlotKind::Clips),
        ];
        let meter = |slots: Option<MeterSlots>, node: NodeId| {
            let slots = slots.unwrap();
            [
                (slots.peak.index(), node, Port::AudioOut, SlotKind::Peak),
                (slots.clips.index(), node, Port::AudioOut, SlotKind::Clips),
            ]
        };
        expected.extend(meter(model.groups[0].meter, model.groups[0].node));
        for lane in &model.instruments {
            expected.extend(meter(lane.meter, lane.node));
        }
        assert_eq!(entries(&layout.meters), expected);
        assert_eq!(expected.len(), 2 + 2 + 3 * 2, "the master, one group and three lanes");
    }

    #[test]
    fn a_meter_that_has_no_slots_is_left_out() {
        let mut model = load(GROUPS).model;
        model.groups[0].meter = None;
        model.instruments[1].meter = None;
        let layout = model.telemetry_layout();
        assert_eq!(layout.meters.len(), 2 + 2 * 2);
        let nodes: Vec<NodeId> = layout.meters.iter().map(|e| e.node).collect();
        assert!(!nodes.contains(&model.groups[0].node) && !nodes.contains(&model.instruments[1].node));
    }

    #[test]
    fn each_modulator_has_its_output_and_each_modulated_parameter_its_value() {
        let model = load(GROUPS).model;
        let layout = model.telemetry_layout();
        let group = &model.groups[0];
        let (lane0, lane1) = (model.instruments[0].node, model.instruments[1].node);
        let filter = model.instruments[1].effects[0].node;
        let bus = group.effects[0].node;
        let slot = |s: Option<Slot>| s.unwrap().index();
        let (lfo, env) = (&group.modulators[0], &group.modulators[1]);
        let lane_lfo = &model.instruments[1].modulators[0];
        assert_eq!(
            entries(&layout.modulation),
            [
                // The outputs: the group's modulators, then the lanes'.
                (slot(lfo.output), lfo.node, Port::ModOut, SlotKind::ModOutput),
                (slot(env.output), env.node, Port::ModOut, SlotKind::ModOutput),
                (slot(lane_lfo.output), lane_lfo.node, Port::ModOut, SlotKind::ModOutput),
                // The values of the parameters the group's LFO drives: member 0's detune, member 1's
                // filter cutoff, the bus effect's damping. Its other targets drive modulators.
                (slot(lfo.targets[0].effective), lane0, Port::Param { index: 1 }, SlotKind::Effective),
                (slot(lfo.targets[1].effective), filter, Port::Param { index: 0 }, SlotKind::Effective),
                (slot(lfo.targets[2].effective), bus, Port::Param { index: 2 }, SlotKind::Effective),
            ]
        );
        assert_ne!(lane0, lane1);
    }

    #[test]
    fn a_parameter_several_targets_drive_is_named_once_by_the_lanes_slot() {
        let model = load(
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
[[instrument.modulator.target]]
param = "volume"
[[instrument.modulator]]
[[instrument.modulator.target]]
param = "detune"
"#,
        )
        .model;
        let lane = &model.instruments[0];
        let layout = model.telemetry_layout();
        let effective: Vec<(u32, Port)> = layout
            .modulation
            .iter()
            .filter(|e| e.kind == SlotKind::Effective)
            .map(|e| (e.slot, e.port))
            .collect();
        assert_eq!(
            effective,
            [
                (lane.modulators[0].targets[0].effective.unwrap().index(), Port::Param { index: 1 }),
                (lane.modulators[0].targets[1].effective.unwrap().index(), Port::Param { index: 2 }),
            ],
            "detune once, by the first lane target (the group's and the second lane modulator's are the same parameter), then volume"
        );
    }

    #[test]
    fn a_target_that_drives_a_modulator_or_has_no_slot_or_no_aim_has_no_entry() {
        let mut model = load(GROUPS).model;
        let before = model.telemetry_layout().modulation.len();
        // No slot (they ran out): the target is left out, its parameter is not named.
        model.groups[0].modulators[0].targets[0].effective = None;
        assert_eq!(model.telemetry_layout().modulation.len(), before - 1);
        // A target whose aim is gone.
        let target = &mut model.groups[0].modulators[0].targets[1];
        target.kind = crate::plugin::chain::ModTargetKind::GroupBus { effect_index: 9, param_index: 0 };
        assert_eq!(model.telemetry_layout().modulation.len(), before - 2);
        // And a modulator with no output slot.
        model.groups[0].modulators[1].output = None;
        assert_eq!(model.telemetry_layout().modulation.len(), before - 3);
    }

    #[test]
    fn the_rev_is_the_same_for_the_same_layout_and_differs_when_it_does() {
        let model = load(GROUPS).model;
        let layout = model.telemetry_layout();
        assert_eq!(layout.rev, model.telemetry_layout().rev);

        // A value changing is not a change of layout.
        let mut edited = load(GROUPS).model;
        edited.instruments[0].volume = 0.25;
        edited.groups[0].volume = 0.5;
        assert_eq!(edited.telemetry_layout(), layout);

        // A slot being somebody else's, or a node being someone else's, is.
        let mut other = load(GROUPS).model;
        other.groups[0].modulators[0].output = other.groups[0].modulators[1].output;
        assert_ne!(other.telemetry_layout().rev, layout.rev, "a modulator's slot");
        let mut other = load(GROUPS).model;
        other.groups[0].meter = None;
        assert_ne!(other.telemetry_layout().rev, layout.rev, "a meter gone");
        let mut other = load(GROUPS).model;
        other.instruments[0].node = NodeId(9_000);
        assert_ne!(other.telemetry_layout().rev, layout.rev, "a node's id");
        let mut other = load(GROUPS).model;
        other.groups[0].modulators[0].targets[0].kind = crate::plugin::chain::ModTargetKind::GroupMember { member: 0, slot: 0, param_index: 5 };
        assert_ne!(other.telemetry_layout().rev, layout.rev, "a parameter");
        let swapped = TelemetryLayout::new(layout.modulation.clone(), layout.meters.clone());
        assert_ne!(swapped.rev, layout.rev, "which list an entry is in");
    }

    #[test]
    fn a_target_added_later_changes_the_layout() {
        use crate::app::controller::{Controller, NewTarget};
        use crate::app::testutil::stub_engine;
        use crate::plugin::chain::ModTargetKind;
        let model = load("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\n").model;
        let (handles, _engine) = stub_engine();
        let mut ctl = Controller::new(model, handles, None);
        let before = ctl.model.telemetry_layout();
        let detune = NewTarget {
            kind: ModTargetKind::PluginParam { slot: 0, param_index: 1 },
            slot: 0,
            param_name: "detune".to_string(),
            depth: 0.5,
            base_value: 0.0,
            param_min: -24.0,
            param_max: 24.0,
        };
        assert!(ctl.add_target(ModulatorLocation::Lane(0), 0, detune));
        let after = ctl.model.telemetry_layout();
        assert_ne!(after.rev, before.rev);
        assert_eq!(after.modulation.len(), before.modulation.len() + 1);
        let added = after.modulation.last().unwrap();
        assert_eq!((added.kind, added.node, added.port), (SlotKind::Effective, ctl.model.instruments[0].node, Port::Param { index: 1 }));
        assert_eq!(Some(added.slot), ctl.model.instruments[0].modulators[0].targets[0].effective.map(|s| s.index()));
    }

    #[test]
    fn the_layout_is_camel_case_json() {
        let layout = TelemetryLayout::new(
            vec![LayoutEntry { slot: 0, node: NodeId::MASTER, port: Port::AudioIn, kind: SlotKind::Peak }],
            vec![
                LayoutEntry { slot: 5, node: NodeId(3), port: Port::ModOut, kind: SlotKind::ModOutput },
                LayoutEntry { slot: 6, node: NodeId(4), port: Port::Param { index: 2 }, kind: SlotKind::Effective },
            ],
        );
        let json = serde_json::to_value(&layout).unwrap();
        assert_eq!(json["rev"], json!(layout.rev));
        assert_eq!(
            json["meters"],
            json!([{"slot": 0, "node": u32::MAX, "port": {"type": "audioIn"}, "kind": "peak"}])
        );
        assert_eq!(
            json["modulation"],
            json!([
                {"slot": 5, "node": 3, "port": {"type": "modOut"}, "kind": "modOutput"},
                {"slot": 6, "node": 4, "port": {"type": "param", "index": 2}, "kind": "effective"},
            ])
        );
        assert_eq!(serde_json::from_value::<TelemetryLayout>(json).unwrap(), layout);
    }
}
