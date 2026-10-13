//! `Controller::apply`: carrying out an `Intent`.
//!
//! Each intent is resolved from its `NodeId` to a position in the model, checked,
//! applied to the model and sent to the audio thread, and answered with the event
//! that describes it. The checks come first: an intent that fails changes
//! nothing, and does not count as a revision.

use crate::app::controller::{Controller, NewTarget, depth_command, envelope_command, rate_command};
use crate::app::graph::{Endpoint, Port};
use crate::app::intent::{Applied, Event, Gain, Intent, IntentError, ModulatorField, ModulatorKind, ParamValue};
use crate::app::model::{DEPTH_MAX, DEPTH_MIN, EdgeId, ModSourceSlot, NEW_CABLE_DEPTH, NodeId, NodePath, ParamKind, ParamSlot, PluginSlot};
use crate::plugin::chain::{GraphCommand, LfoWaveform, ModulatorLocation};
use crate::{plugin, session};

/// What an intent did, for `apply` to tell.
enum Done {
    /// It changed a value: the event says which.
    Event(Event),
    /// It added or took out nodes or cables.
    Restructured,
}

/// Where the audio thread finds the plugin an intent concerns.
enum PluginAt {
    /// A lane's instrument (slot 0) or one of its effects (slot 1..).
    Lane { inst: usize, slot: usize },
    /// One of a group's bus effects.
    Bus { group: usize, index: usize },
}

impl Controller {
    pub fn apply(&mut self, intent: Intent) -> Result<Applied, IntentError> {
        // (Saving is the one intent that leaves the session with nothing unsaved.)
        if matches!(intent, Intent::Save) {
            return self.save_intent();
        }
        let done = match intent {
            Intent::SetParam { node, param, value } => Done::Event(self.set_param(node, param, value)?),
            Intent::SetGain { node, gain, value } => Done::Event(self.set_gain(node, gain, value)?),
            Intent::LoadPreset { node, preset } => Done::Event(self.load_preset(node, &preset)?),
            Intent::SetModulatorField { node, field, value } => Done::Event(self.set_modulator_field(node, field, value)?),
            Intent::SetEdgeDepth { edge, depth } => Done::Event(self.set_edge_depth(edge, depth)?),
            Intent::RemoveEdge { edge } => self.remove_edge_intent(edge)?,
            Intent::AddModulator { parent, kind } => self.add_modulator_intent(parent, kind)?,
            Intent::AddModEdge { modulator, to } => self.add_mod_edge_intent(modulator, to)?,
            Intent::RemoveNode { node } => self.remove_node_intent(node)?,
            Intent::AddEffect { parent, plugin } => self.add_effect_intent(parent, &plugin)?,
            Intent::Save => unreachable!("saving is done above"),
        };
        self.dirty = true;
        self.rev += 1;
        let applied = match done {
            Done::Event(event) => Applied::of(self.rev, vec![event]),
            Done::Restructured => Applied::restructured(self.rev),
        };
        self.publish(&applied);
        // The subscribers are told the new structure as a snapshot, at once, whole.
        #[cfg(any(test, feature = "web"))]
        if applied.restructured {
            self.resync();
        }
        Ok(applied)
    }

    /// Write the session to its own file. Refused, with nothing changed (and so no revision), if it has none or
    /// it cannot be written.
    fn save_intent(&mut self) -> Result<Applied, IntentError> {
        self.save().map_err(|e| IntentError::Io(e.to_string()))?;
        self.rev += 1;
        let applied = Applied::of(self.rev, vec![Event::Saved { file: self.session_file_name() }]);
        self.publish(&applied);
        Ok(applied)
    }

    fn set_param(&mut self, node: NodeId, param: u32, value: f32) -> Result<Event, IntentError> {
        let at = self.locate_plugin(node)?;
        let plugin = self.plugin_at(&at).ok_or_else(|| invalid("that instrument has no plugin"))?;
        let row = plugin
            .params
            .iter_mut()
            .find(|p| p.index == param)
            .ok_or_else(|| invalid(format!("the plugin has no parameter {param}")))?;
        if !value.is_finite() {
            return Err(invalid(format!("{value} is not a value for {}", row.name)));
        }
        let mut value = value.max(row.min).min(row.max);
        if matches!(row.kind, ParamKind::Enum(_)) {
            value = value.round();
        }
        row.value = value;
        let command = match at {
            PluginAt::Lane { inst, slot } => GraphCommand::SetParameter { inst, slot, param_index: param, value },
            PluginAt::Bus { group, index } => GraphCommand::SetGroupParameter { group, index, param_index: param, value },
        };
        let _ = self.cmd_tx.send(command);
        Ok(Event::ParamSet { node, param, value })
    }

    fn set_gain(&mut self, node: NodeId, gain: Gain, value: f32) -> Result<Event, IntentError> {
        let path = self.model.locate(node).ok_or(IntentError::NotFound(node))?;
        if !value.is_finite() {
            return Err(invalid(format!("{value} is not a value for {gain:?}")));
        }
        let (min, max) = gain.range();
        let value = value.max(min).min(max);
        let command = match (gain, path) {
            (Gain::Volume, NodePath::Instrument(inst)) => {
                self.model.instruments[inst].volume = value;
                GraphCommand::SetVolume { inst, value }
            }
            (Gain::Mix, NodePath::Effect { inst, index }) => {
                self.model.instruments[inst].effects[index].mix = value;
                GraphCommand::SetMix { inst, slot: index + 1, value }
            }
            (Gain::Mix, NodePath::GroupEffect { group, index }) => {
                self.model.groups[group].effects[index].mix = value;
                GraphCommand::SetGroupMix { group, index, value }
            }
            (Gain::GroupVolume, NodePath::Group(group)) => {
                self.model.groups[group].volume = value;
                GraphCommand::SetGroupVolume { group, value }
            }
            (Gain::GroupPan, NodePath::Group(group)) => {
                self.model.groups[group].pan = value;
                GraphCommand::SetGroupPan { group, value }
            }
            _ => return Err(invalid(format!("{gain:?} does not apply to that node"))),
        };
        let _ = self.cmd_tx.send(command);
        Ok(Event::GainSet { node, gain, value })
    }

    fn set_modulator_field(&mut self, node: NodeId, field: ModulatorField, value: f32) -> Result<Event, IntentError> {
        let path = self.model.locate(node).ok_or(IntentError::NotFound(node))?;
        let NodePath::Modulator { at, index } = path else {
            return Err(invalid("that node is not a modulator"));
        };
        if !value.is_finite() {
            return Err(invalid(format!("{value} is not a value for {field:?}")));
        }
        let (min, max) = field.range();
        let mut value = value.max(min).min(max);
        if field == ModulatorField::Waveform {
            value = value.round();
        }
        let modulator = self
            .model
            .modulators_mut(at)
            .and_then(|rack| rack.get_mut(index))
            .ok_or(IntentError::NotFound(node))?;
        let command = match (&mut modulator.source, field) {
            (ModSourceSlot::Lfo { rate, .. }, ModulatorField::Rate) => {
                *rate = value;
                rate_command(at, index, value)
            }
            (ModSourceSlot::Lfo { waveform, .. }, ModulatorField::Waveform) => {
                *waveform = LfoWaveform::ALL[value as usize];
                let waveform = *waveform;
                match at {
                    ModulatorLocation::Lane(inst) => GraphCommand::SetModulatorWaveform { inst, mod_index: index, waveform },
                    ModulatorLocation::Group(group) => GraphCommand::SetGroupModulatorWaveform { group, mod_index: index, waveform },
                }
            }
            (
                ModSourceSlot::Envelope { attack, decay, sustain, release },
                ModulatorField::Attack | ModulatorField::Decay | ModulatorField::Sustain | ModulatorField::Release,
            ) => {
                match field {
                    ModulatorField::Attack => *attack = value,
                    ModulatorField::Decay => *decay = value,
                    ModulatorField::Sustain => *sustain = value,
                    _ => *release = value,
                }
                envelope_command(at, index, (*attack, *decay, *sustain, *release))
            }
            _ => return Err(invalid(format!("{field:?} does not apply to that modulator"))),
        };
        let _ = self.cmd_tx.send(command);
        Ok(Event::ModulatorFieldSet { node, field, value })
    }

    fn set_edge_depth(&mut self, edge: EdgeId, depth: f32) -> Result<Event, IntentError> {
        let (at, mod_index, target_index) = self.model.locate_edge(edge).ok_or(IntentError::EdgeNotFound(edge))?;
        if !depth.is_finite() {
            return Err(invalid(format!("{depth} is not a value for the depth of a cable")));
        }
        let depth = depth.clamp(DEPTH_MIN, DEPTH_MAX);
        let target = self
            .model
            .modulators_mut(at)
            .and_then(|rack| rack.get_mut(mod_index))
            .and_then(|modulator| modulator.targets.get_mut(target_index))
            .ok_or(IntentError::EdgeNotFound(edge))?;
        target.depth = depth;
        let _ = self.cmd_tx.send(depth_command(at, mod_index, target_index, depth));
        Ok(Event::EdgeDepthSet { edge, depth })
    }

    fn remove_edge_intent(&mut self, edge: EdgeId) -> Result<Done, IntentError> {
        if self.remove_edge(edge) {
            Ok(Done::Restructured)
        } else {
            Err(IntentError::EdgeNotFound(edge))
        }
    }

    fn add_modulator_intent(&mut self, parent: NodeId, kind: ModulatorKind) -> Result<Done, IntentError> {
        let at = match self.model.locate(parent).ok_or(IntentError::NotFound(parent))? {
            NodePath::Instrument(inst) => ModulatorLocation::Lane(inst),
            NodePath::Group(group) => ModulatorLocation::Group(group),
            _ => return Err(invalid("a modulator goes in a lane or in a group")),
        };
        self.add_modulator(at, kind.source()).ok_or(IntentError::NotFound(parent))?;
        Ok(Done::Restructured)
    }

    fn add_mod_edge_intent(&mut self, modulator: NodeId, to: Endpoint) -> Result<Done, IntentError> {
        let NodePath::Modulator { at, index } = self.model.locate(modulator).ok_or(IntentError::NotFound(modulator))? else {
            return Err(invalid("a cable starts at a modulator"));
        };
        let Port::Param { index: param } = to.port else {
            return Err(invalid("a cable can only end at a plugin's parameter so far"));
        };
        // The plugin has to be there, and in reach of the modulator (`param_targets`: the rule is the model's).
        let plugin = self.locate_plugin(to.node)?;
        if self.plugin_at(&plugin).is_none() {
            return Err(invalid("that instrument has no plugin"));
        }
        let reachable = self.model.param_targets(at);
        if !reachable.iter().any(|t| t.node == to.node) {
            let owner = match at {
                ModulatorLocation::Lane(_) => "lane",
                ModulatorLocation::Group(_) => "group",
            };
            return Err(invalid(format!("a modulator of this {owner} cannot reach that plugin")));
        }
        let target = reachable
            .into_iter()
            .find(|t| t.node == to.node && t.param == param)
            .ok_or_else(|| invalid(format!("the plugin has no parameter {param}")))?;
        let drives_it_already = self.model.modulators_mut(at).and_then(|rack| rack.get(index)).is_some_and(|m| m.targets.iter().any(|t| t.kind == target.kind));
        if drives_it_already {
            return Err(invalid("that modulator drives that parameter already"));
        }
        self.add_target(at, index, NewTarget::to_param(&target, NEW_CABLE_DEPTH));
        Ok(Done::Restructured)
    }

    fn add_effect_intent(&mut self, parent: NodeId, plugin: &str) -> Result<Done, IntentError> {
        match self.model.locate(parent).ok_or(IntentError::NotFound(parent))? {
            NodePath::Instrument(inst) => self.add_effect(inst, plugin)?,
            NodePath::Group(group) => self.add_bus_effect(group, plugin)?,
            _ => return Err(invalid("an effect goes in a lane or in a group")),
        };
        Ok(Done::Restructured)
    }

    fn remove_node_intent(&mut self, node: NodeId) -> Result<Done, IntentError> {
        match self.model.locate(node).ok_or(IntentError::NotFound(node))? {
            NodePath::Modulator { at, index } => {
                self.remove_modulator(at, index);
            }
            // (The cables that drive the plugin go first, with the ones that drive their depths; the controller does it.)
            NodePath::Effect { inst, index } => {
                self.remove_effect(inst, index);
            }
            NodePath::GroupEffect { group, index } => {
                self.remove_bus_effect(group, index);
            }
            NodePath::Instrument(_) | NodePath::Group(_) => return Err(invalid("only a modulator or an effect can be removed so far")),
        }
        Ok(Done::Restructured)
    }

    fn load_preset(&mut self, node: NodeId, preset_id: &str) -> Result<Event, IntentError> {
        let at = self.locate_plugin(node)?;
        let PluginAt::Lane { inst, slot } = at else {
            return Err(invalid("a bus effect has no presets to load"));
        };
        let plugin = self.plugin_at(&at).ok_or_else(|| invalid("that instrument has no plugin"))?;
        let preset = plugin
            .presets
            .iter()
            .find(|p| p.id == preset_id)
            .cloned()
            .ok_or_else(|| invalid(format!("the plugin has no preset {preset_id}")))?;
        let source = plugin.id.clone();

        // The audio thread first: that is what changes the sound.
        let _ = self.cmd_tx.send(GraphCommand::LoadPreset { inst, slot, preset_id: preset.id.clone() });

        let values = self.values_after_preset(&source, &preset.name);
        let mut mirrored = Vec::new();
        if let Some(plugin) = self.plugin_at(&at) {
            plugin.current_preset = Some(preset.name.clone());
            if let Some(values) = values {
                mirrored = mirror_values(&mut plugin.params, &values);
            }
        }
        Ok(Event::PresetLoaded { node, preset: preset.name, values: mirrored })
    }

    /// What a plugin's parameters are once `preset` is loaded, by the plugin's own
    /// index, read from a second instance: the live one is on the audio thread, and
    /// loading twice is acceptable at preset-switch time. None, after a warning, if
    /// the plugin does not load again.
    fn values_after_preset(&self, source: &str, preset: &str) -> Option<Vec<(u32, f32)>> {
        match plugin::load(source, self.sample_rate, self.max_block_size, &self.runtime) {
            Ok(mut second) => {
                session::apply_preset(&mut second, preset);
                let params = second.parameters();
                Some(params.iter().map(|p| (p.index, second.get_parameter(p.index).unwrap_or(p.default))).collect())
            }
            Err(e) => {
                log::warn!("Failed to mirror preset values for {source}: {e}");
                None
            }
        }
    }

    /// Where the plugin in node `node` is, if it is a node with a plugin.
    fn locate_plugin(&self, node: NodeId) -> Result<PluginAt, IntentError> {
        match self.model.locate(node).ok_or(IntentError::NotFound(node))? {
            NodePath::Instrument(inst) => Ok(PluginAt::Lane { inst, slot: 0 }),
            NodePath::Effect { inst, index } => Ok(PluginAt::Lane { inst, slot: index + 1 }),
            NodePath::GroupEffect { group, index } => Ok(PluginAt::Bus { group, index }),
            NodePath::Group(_) | NodePath::Modulator { .. } => Err(invalid("that node has no plugin")),
        }
    }

    /// The model's slot for the plugin at `at`.
    fn plugin_at(&mut self, at: &PluginAt) -> Option<&mut PluginSlot> {
        match *at {
            PluginAt::Lane { inst, slot: 0 } => self.model.instruments[inst].instrument.as_mut(),
            PluginAt::Lane { inst, slot } => self.model.instruments[inst].effects.get_mut(slot - 1),
            PluginAt::Bus { group, index } => self.model.groups[group].effects.get_mut(index),
        }
    }
}

/// Give each parameter row the value its plugin reports for it, as the value and as
/// the baseline, so that a preset's values are not saved as the user's edits. Rows
/// are matched to `values` by the plugin's own parameter index; a row with no value
/// keeps what it has. Returns what was set.
fn mirror_values(rows: &mut [ParamSlot], values: &[(u32, f32)]) -> Vec<ParamValue> {
    let mut set = Vec::new();
    for row in rows {
        if let Some(&(_, value)) = values.iter().find(|(index, _)| *index == row.index) {
            row.value = value;
            row.default = value;
            set.push(ParamValue { param: row.index, value });
        }
    }
    set
}

fn invalid(why: impl Into<String>) -> IntentError {
    IntentError::Invalid(why.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::graph::{Graph, NodeKind};
    use crate::app::testutil::{self, StubEngine, described, load};
    use crate::plugin::chain::{ModTargetKind, ModulatorLocation};

    /// A group with a bus filter and an LFO, a member with a reverb, and a lane
    /// with no instrument plugin.
    const SESSION: &str = r#"
[[group]]
[[group.effect]]
plugin = "builtin:filter"
[[group.modulator]]
type = "lfo"

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.effect]]
plugin = "builtin:reverb"

[[instrument]]
"#;

    fn controller() -> (StubEngine, Controller) {
        let (handles, engine) = testutil::stub_engine();
        (engine, Controller::new(load(SESSION).model, handles, None))
    }

    fn sent(engine: &StubEngine) -> Vec<String> {
        described(engine.cmd_rx.try_iter())
    }

    fn node(ctl: &Controller, path: NodePath) -> NodeId {
        ctl.model.nodes().find(|(_, p)| *p == path).map(|(id, _)| id).unwrap()
    }

    fn lane(ctl: &Controller) -> NodeId {
        node(ctl, NodePath::Instrument(0))
    }

    fn param(ctl: &Controller, path: NodePath, name: &str) -> (u32, f32) {
        let (_, p) = ctl.model.nodes().find(|(_, p)| *p == path).unwrap();
        let slot = match p {
            NodePath::Instrument(i) => ctl.model.instruments[i].instrument.as_ref().unwrap(),
            NodePath::Effect { inst, index } => &ctl.model.instruments[inst].effects[index],
            NodePath::GroupEffect { group, index } => &ctl.model.groups[group].effects[index],
            _ => panic!("{p:?} has no plugin"),
        };
        let row = slot.params.iter().find(|r| r.name == name).unwrap();
        (row.index, row.value)
    }

    #[test]
    fn set_param_sets_an_instruments_parameter_in_the_model_and_the_engine() {
        let (engine, mut ctl) = controller();
        let (detune, before) = param(&ctl, NodePath::Instrument(0), "detune");
        assert_eq!(before, 0.0);
        let applied = ctl.apply(Intent::SetParam { node: lane(&ctl), param: detune, value: 3.0 }).unwrap();

        assert_eq!(applied, Applied::of(1, vec![Event::ParamSet { node: lane(&ctl), param: detune, value: 3.0 }]));
        assert_eq!(param(&ctl, NodePath::Instrument(0), "detune").1, 3.0);
        assert_eq!(sent(&engine), [format!("SetParameter inst=0 slot=0 param={detune} value=3.000")]);
        assert!(ctl.dirty);
    }

    #[test]
    fn set_param_reaches_an_effect_by_its_slot_and_a_bus_effect_by_its_group() {
        let (engine, mut ctl) = controller();
        let reverb = NodePath::Effect { inst: 0, index: 0 };
        let (damping, _) = param(&ctl, reverb, "damping");
        ctl.apply(Intent::SetParam { node: node(&ctl, reverb), param: damping, value: 0.25 }).unwrap();
        assert_eq!(sent(&engine), [format!("SetParameter inst=0 slot=1 param={damping} value=0.250")]);
        assert_eq!(param(&ctl, reverb, "damping").1, 0.25);

        let bus = NodePath::GroupEffect { group: 0, index: 0 };
        let (cutoff, _) = param(&ctl, bus, "cutoff");
        ctl.apply(Intent::SetParam { node: node(&ctl, bus), param: cutoff, value: 0.5 }).unwrap();
        assert_eq!(sent(&engine), [format!("SetGroupParameter group=0 fx=0 param={cutoff} value=0.500")]);
        assert_eq!(param(&ctl, bus, "cutoff").1, 0.5);
    }

    #[test]
    fn set_param_only_changes_the_parameter_it_names() {
        let (_engine, mut ctl) = controller();
        let before: Vec<f32> = ctl.model.instruments[0].instrument.as_ref().unwrap().params.iter().map(|p| p.value).collect();
        let (detune, _) = param(&ctl, NodePath::Instrument(0), "detune");
        ctl.apply(Intent::SetParam { node: lane(&ctl), param: detune, value: 3.0 }).unwrap();
        let after: Vec<f32> = ctl.model.instruments[0].instrument.as_ref().unwrap().params.iter().map(|p| p.value).collect();
        let changed: Vec<usize> = (0..before.len()).filter(|&i| before[i] != after[i]).collect();
        assert_eq!(changed, [detune as usize]);
    }

    #[test]
    fn set_param_clamps_to_the_parameters_range() {
        let (engine, mut ctl) = controller();
        let (detune, _) = param(&ctl, NodePath::Instrument(0), "detune"); // -24 ..= 24
        for (asked, got) in [(99.0, 24.0), (-99.0, -24.0), (-24.0, -24.0), (24.0, 24.0), (0.5, 0.5)] {
            let applied = ctl.apply(Intent::SetParam { node: lane(&ctl), param: detune, value: asked }).unwrap();
            assert_eq!(applied.events, [Event::ParamSet { node: lane(&ctl), param: detune, value: got }], "{asked}");
            assert_eq!(param(&ctl, NodePath::Instrument(0), "detune").1, got);
        }
        assert_eq!(sent(&engine).len(), 5, "a command for each, moved or not");
    }

    #[test]
    fn set_param_rounds_an_enum_parameter_to_a_whole_value() {
        let (engine, mut ctl) = controller();
        let (waveform, _) = param(&ctl, NodePath::Instrument(0), "waveform"); // 0 ..= 3
        for (asked, got) in [(1.4, 1.0), (1.6, 2.0), (2.5, 3.0), (99.0, 3.0), (-1.0, 0.0), (0.4, 0.0)] {
            let applied = ctl.apply(Intent::SetParam { node: lane(&ctl), param: waveform, value: asked }).unwrap();
            assert_eq!(applied.events, [Event::ParamSet { node: lane(&ctl), param: waveform, value: got }], "{asked}");
        }
        assert_eq!(sent(&engine).last().unwrap(), &format!("SetParameter inst=0 slot=0 param={waveform} value=0.000"));
    }

    #[test]
    fn set_param_names_the_plugins_own_parameter_index() {
        let (engine, mut ctl) = controller();
        // A plugin that hides parameters numbers them differently from the rows.
        ctl.model.instruments[0].instrument.as_mut().unwrap().params[1].index = 42;
        let id = lane(&ctl);
        ctl.apply(Intent::SetParam { node: id, param: 42, value: 3.0 }).unwrap();
        assert_eq!(sent(&engine), ["SetParameter inst=0 slot=0 param=42 value=3.000"]);
        assert_eq!(ctl.model.instruments[0].instrument.as_ref().unwrap().params[1].value, 3.0);
        assert!(matches!(
            ctl.apply(Intent::SetParam { node: id, param: 1, value: 3.0 }),
            Err(IntentError::Invalid(_))
        ), "1 is a row, not a parameter, now");
    }

    #[test]
    fn every_applied_intent_is_a_revision() {
        let (_engine, mut ctl) = controller();
        let (detune, _) = param(&ctl, NodePath::Instrument(0), "detune");
        assert_eq!(ctl.rev, 0);
        for expected in 1..=3 {
            let applied = ctl.apply(Intent::SetParam { node: lane(&ctl), param: detune, value: expected as f32 }).unwrap();
            assert_eq!(applied.rev, expected);
        }
        assert_eq!(ctl.rev, 3);
    }

    #[test]
    fn set_param_refuses_what_it_cannot_do_and_changes_nothing() {
        let (engine, mut ctl) = controller();
        let (detune, _) = param(&ctl, NodePath::Instrument(0), "detune");
        let good = lane(&ctl);
        let group = node(&ctl, NodePath::Group(0));
        let lfo = node(&ctl, NodePath::Modulator { at: ModulatorLocation::Group(0), index: 0 });
        let empty_lane = node(&ctl, NodePath::Instrument(1));
        let ghost = NodeId(9_999);

        let cases = [
            ("a node that is not there", Intent::SetParam { node: ghost, param: detune, value: 1.0 }, IntentError::NotFound(ghost)),
            ("a group", Intent::SetParam { node: group, param: 0, value: 1.0 }, invalid("that node has no plugin")),
            ("a modulator", Intent::SetParam { node: lfo, param: 0, value: 1.0 }, invalid("that node has no plugin")),
            ("a lane without an instrument", Intent::SetParam { node: empty_lane, param: 0, value: 1.0 }, invalid("that instrument has no plugin")),
            ("a parameter the plugin lacks", Intent::SetParam { node: good, param: 999, value: 1.0 }, invalid("the plugin has no parameter 999")),
        ];
        for (what, intent, error) in cases {
            assert_eq!(ctl.apply(intent), Err(error), "{what}");
        }
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let refused = ctl.apply(Intent::SetParam { node: good, param: detune, value });
            assert!(matches!(refused, Err(IntentError::Invalid(_))), "{value}");
        }

        assert_eq!(sent(&engine), Vec::<String>::new(), "nothing reached the audio thread");
        assert_eq!(ctl.rev, 0, "a refusal is not a revision");
        assert!(!ctl.dirty);
        assert_eq!(param(&ctl, NodePath::Instrument(0), "detune").1, 0.0);
    }

    #[test]
    fn a_node_that_was_removed_is_not_found() {
        let (engine, mut ctl) = controller();
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        let (damping, _) = param(&ctl, NodePath::Effect { inst: 0, index: 0 }, "damping");
        ctl.model.instruments[0].effects.remove(0);
        assert_eq!(
            ctl.apply(Intent::SetParam { node: reverb, param: damping, value: 0.5 }),
            Err(IntentError::NotFound(reverb))
        );
        assert_eq!(sent(&engine), Vec::<String>::new());
    }

    fn gain(ctl: &mut Controller, node: NodeId, gain: Gain, value: f32) -> Result<Applied, IntentError> {
        ctl.apply(Intent::SetGain { node, gain, value })
    }

    #[test]
    fn set_gain_sets_each_kind_of_gain_in_the_model_and_the_engine() {
        let (engine, mut ctl) = controller();
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        let bus = node(&ctl, NodePath::GroupEffect { group: 0, index: 0 });
        let group = node(&ctl, NodePath::Group(0));

        let instrument = lane(&ctl);
        let applied = gain(&mut ctl, instrument, Gain::Volume, 0.5).unwrap();
        assert_eq!(applied, Applied::of(1, vec![Event::GainSet { node: instrument, gain: Gain::Volume, value: 0.5 }]));
        gain(&mut ctl, reverb, Gain::Mix, 0.25).unwrap();
        gain(&mut ctl, bus, Gain::Mix, 0.75).unwrap();
        gain(&mut ctl, group, Gain::GroupVolume, 2.0).unwrap();
        gain(&mut ctl, group, Gain::GroupPan, -0.5).unwrap();

        assert_eq!(
            sent(&engine),
            [
                "SetVolume inst=0 value=0.500",
                "SetMix inst=0 slot=1 value=0.250",
                "SetGroupMix group=0 fx=0 value=0.750",
                "SetGroupVolume group=0 value=2.000",
                "SetGroupPan group=0 value=-0.500",
            ]
        );
        assert_eq!(ctl.model.instruments[0].volume, 0.5);
        assert_eq!(ctl.model.instruments[0].effects[0].mix, 0.25);
        assert_eq!(ctl.model.groups[0].effects[0].mix, 0.75);
        assert_eq!((ctl.model.groups[0].volume, ctl.model.groups[0].pan), (2.0, -0.5));
        assert_eq!(ctl.rev, 5);
        assert!(ctl.dirty);
    }

    #[test]
    fn set_gain_clamps_to_each_gains_range() {
        let (_engine, mut ctl) = controller();
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        let group = node(&ctl, NodePath::Group(0));
        let cases = [
            (lane(&ctl), Gain::Volume, [(9.0, 4.0), (-1.0, 0.0), (4.0, 4.0), (0.0, 0.0)]),
            (reverb, Gain::Mix, [(2.0, 1.0), (-0.5, 0.0), (1.0, 1.0), (0.0, 0.0)]),
            (group, Gain::GroupVolume, [(9.0, 4.0), (-1.0, 0.0), (4.0, 4.0), (0.0, 0.0)]),
            (group, Gain::GroupPan, [(3.0, 1.0), (-3.0, -1.0), (1.0, 1.0), (-1.0, -1.0)]),
        ];
        for (node, kind, values) in cases {
            for (asked, got) in values {
                let applied = gain(&mut ctl, node, kind, asked).unwrap();
                assert_eq!(applied.events, [Event::GainSet { node, gain: kind, value: got }], "{kind:?} {asked}");
            }
        }
        assert_eq!(Gain::Volume.range(), (0.0, 4.0));
        assert_eq!(Gain::Mix.range(), (0.0, 1.0));
        assert_eq!(Gain::GroupVolume.range(), (0.0, 4.0));
        assert_eq!(Gain::GroupPan.range(), (-1.0, 1.0));
    }

    #[test]
    fn set_gain_refuses_a_gain_that_does_not_apply_to_the_node() {
        let (engine, mut ctl) = controller();
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        let bus = node(&ctl, NodePath::GroupEffect { group: 0, index: 0 });
        let group = node(&ctl, NodePath::Group(0));
        let lfo = node(&ctl, NodePath::Modulator { at: ModulatorLocation::Group(0), index: 0 });
        let ghost = NodeId(9_999);
        let wrong = |g: Gain| IntentError::Invalid(format!("{g:?} does not apply to that node"));

        let cases = [
            (lane(&ctl), Gain::Mix, wrong(Gain::Mix)),
            (lane(&ctl), Gain::GroupVolume, wrong(Gain::GroupVolume)),
            (lane(&ctl), Gain::GroupPan, wrong(Gain::GroupPan)),
            (reverb, Gain::Volume, wrong(Gain::Volume)),
            (reverb, Gain::GroupPan, wrong(Gain::GroupPan)),
            (bus, Gain::Volume, wrong(Gain::Volume)),
            (group, Gain::Volume, wrong(Gain::Volume)),
            (group, Gain::Mix, wrong(Gain::Mix)),
            (lfo, Gain::Volume, wrong(Gain::Volume)),
            (lfo, Gain::Mix, wrong(Gain::Mix)),
            (ghost, Gain::Volume, IntentError::NotFound(ghost)),
        ];
        for (node, kind, error) in cases {
            assert_eq!(gain(&mut ctl, node, kind, 0.5), Err(error), "{node:?} {kind:?}");
        }
        let instrument = lane(&ctl);
        for value in [f32::NAN, f32::INFINITY] {
            assert!(matches!(gain(&mut ctl, instrument, Gain::Volume, value), Err(IntentError::Invalid(_))), "{value}");
        }
        assert_eq!(sent(&engine), Vec::<String>::new());
        assert_eq!((ctl.rev, ctl.dirty), (0, false));
        assert_eq!(ctl.model.instruments[0].volume, 1.0);
    }

    #[test]
    fn set_gain_changes_only_the_node_it_names() {
        let (_engine, mut ctl) = controller();
        let before = (ctl.model.instruments[0].effects[0].mix, ctl.model.groups[0].effects[0].mix, ctl.model.groups[0].volume);
        let instrument = lane(&ctl);
        gain(&mut ctl, instrument, Gain::Volume, 0.5).unwrap();
        assert_eq!(
            (ctl.model.instruments[0].effects[0].mix, ctl.model.groups[0].effects[0].mix, ctl.model.groups[0].volume),
            before
        );
        assert_eq!(ctl.model.instruments[1].volume, 1.0, "the other lane");
    }

    /// What a reverb reports for each parameter, by its own index, once `preset`
    /// is loaded into it: what the model has to mirror.
    fn reverb_values_after(preset: &str) -> Vec<ParamValue> {
        let mut reverb = crate::plugin::load("builtin:reverb", 48_000.0, 256, &Default::default()).unwrap();
        crate::session::apply_preset(&mut reverb, preset);
        let indexes: Vec<u32> = reverb.parameters().iter().map(|p| p.index).collect();
        indexes.into_iter().map(|i| ParamValue { param: i, value: reverb.get_parameter(i).unwrap() }).collect()
    }

    #[test]
    fn load_preset_loads_it_on_the_audio_thread_and_mirrors_its_values_in_the_model() {
        let (engine, mut ctl) = controller();
        let reverb_at = NodePath::Effect { inst: 0, index: 0 };
        let reverb = node(&ctl, reverb_at);
        let before: Vec<f32> = ctl.model.instruments[0].effects[0].params.iter().map(|p| p.value).collect();

        let applied = ctl.apply(Intent::LoadPreset { node: reverb, preset: "cathedral".to_string() }).unwrap();
        let expected = reverb_values_after("Cathedral");
        assert_eq!(
            applied,
            Applied::of(1, vec![Event::PresetLoaded { node: reverb, preset: "Cathedral".to_string(), values: expected.clone() }])
        );
        assert_eq!(sent(&engine), ["LoadPreset inst=0 slot=1 id=cathedral"]);
        assert!(ctl.dirty);

        let slot = &ctl.model.instruments[0].effects[0];
        assert_eq!(slot.current_preset.as_deref(), Some("Cathedral"));
        for row in &slot.params {
            let want = expected.iter().find(|v| v.param == row.index).unwrap().value;
            assert_eq!((row.value, row.default), (want, want), "{}: the preset's value, and the baseline", row.name);
        }
        let after: Vec<f32> = slot.params.iter().map(|p| p.value).collect();
        assert_ne!(before, after, "the preset changed something");
    }

    #[test]
    fn load_preset_leaves_the_rest_of_the_session_alone() {
        let (_engine, mut ctl) = controller();
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        let osc: Vec<f32> = ctl.model.instruments[0].instrument.as_ref().unwrap().params.iter().map(|p| p.value).collect();
        let bus: Vec<f32> = ctl.model.groups[0].effects[0].params.iter().map(|p| p.value).collect();
        ctl.apply(Intent::LoadPreset { node: reverb, preset: "plate".to_string() }).unwrap();
        let osc_after: Vec<f32> = ctl.model.instruments[0].instrument.as_ref().unwrap().params.iter().map(|p| p.value).collect();
        let bus_after: Vec<f32> = ctl.model.groups[0].effects[0].params.iter().map(|p| p.value).collect();
        assert_eq!((osc, bus), (osc_after, bus_after));
        assert_eq!(ctl.model.instruments[0].instrument.as_ref().unwrap().current_preset, None);
    }

    #[test]
    fn load_preset_refuses_what_it_cannot_do_and_changes_nothing() {
        let (engine, mut ctl) = controller();
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        let ghost = NodeId(9_999);
        let cases = [
            ("an id the plugin lacks", Intent::LoadPreset { node: reverb, preset: "no-such".to_string() }, invalid("the plugin has no preset no-such")),
            ("a name, not an id", Intent::LoadPreset { node: reverb, preset: "Cathedral".to_string() }, invalid("the plugin has no preset Cathedral")),
            ("an instrument without presets", Intent::LoadPreset { node: lane(&ctl), preset: "cathedral".to_string() }, invalid("the plugin has no preset cathedral")),
            ("a bus effect", Intent::LoadPreset { node: node(&ctl, NodePath::GroupEffect { group: 0, index: 0 }), preset: "x".to_string() }, invalid("a bus effect has no presets to load")),
            ("a group", Intent::LoadPreset { node: node(&ctl, NodePath::Group(0)), preset: "x".to_string() }, invalid("that node has no plugin")),
            ("a lane without an instrument", Intent::LoadPreset { node: node(&ctl, NodePath::Instrument(1)), preset: "x".to_string() }, invalid("that instrument has no plugin")),
            ("a node that is not there", Intent::LoadPreset { node: ghost, preset: "cathedral".to_string() }, IntentError::NotFound(ghost)),
        ];
        for (what, intent, error) in cases {
            assert_eq!(ctl.apply(intent), Err(error), "{what}");
        }
        assert_eq!(sent(&engine), Vec::<String>::new());
        assert_eq!((ctl.rev, ctl.dirty), (0, false));
        assert_eq!(ctl.model.instruments[0].effects[0].current_preset, None);
    }

    #[test]
    fn mirror_values_matches_rows_to_values_by_the_plugins_own_index() {
        let row = |index: u32, value: f32| ParamSlot {
            name: format!("p{index}"),
            index,
            min: 0.0,
            max: 1.0,
            default: value,
            value,
            kind: ParamKind::Float,
            ..Default::default()
        };
        // A plugin with hidden parameters numbers the rows it shows 5, 9 and 12;
        // it also reports a hidden 7, and nothing for 12.
        let mut rows = vec![row(5, 0.5), row(9, 0.5), row(12, 0.5)];
        let set = mirror_values(&mut rows, &[(5, 0.1), (7, 0.7), (9, 0.9)]);
        assert_eq!(set, [ParamValue { param: 5, value: 0.1 }, ParamValue { param: 9, value: 0.9 }]);
        let now: Vec<(f32, f32)> = rows.iter().map(|r| (r.value, r.default)).collect();
        assert_eq!(now, [(0.1, 0.1), (0.9, 0.9), (0.5, 0.5)]);
    }

    #[test]
    fn errors_read_as_sentences() {
        assert_eq!(IntentError::NotFound(NodeId(7)).to_string(), "there is no node 7");
        assert_eq!(IntentError::Invalid("no good".to_string()).to_string(), "no good");
    }

    // -- Modulator settings and the depths of cables --

    use crate::app::intent::ModulatorField;
    use crate::app::model::EdgeId;

    /// A group with a bus filter and two modulators (an LFO that drives the filter, an
    /// envelope); a lane with an LFO (a cutoff target and a cross-mod target on the
    /// second LFO's rate), an envelope, a second LFO and a MIDI-learn control.
    const MODULATORS: &str = r#"
[[group]]
[[group.effect]]
plugin = "builtin:filter"
[[group.modulator]]
type = "lfo"
rate = 2.0
[[group.modulator.target]]
bus = 0
param = "cutoff"
depth = 0.5
[[group.modulator]]
type = "envelope"

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.modulator]]
type = "lfo"
waveform = "saw"
rate = 3.0
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = 0.4
[[instrument.modulator.target]]
mod_rate = 3
depth = -0.25
[[instrument.modulator]]
type = "envelope"
attack = 0.02
decay = 0.3
sustain = 0.6
release = 0.5
[[instrument.modulator]]
type = "midi"
midi_cc = 74
[[instrument.modulator]]
type = "lfo"
rate = 1.0
"#;

    fn modulated() -> (StubEngine, Controller) {
        let (handles, engine) = testutil::stub_engine();
        (engine, Controller::new(load(MODULATORS).model, handles, None))
    }

    fn lane_mod(ctl: &Controller, index: usize) -> NodeId {
        ctl.model.instruments[0].modulators[index].node
    }

    fn group_mod(ctl: &Controller, index: usize) -> NodeId {
        ctl.model.groups[0].modulators[index].node
    }

    fn field(ctl: &mut Controller, node: NodeId, field: ModulatorField, value: f32) -> Result<Applied, IntentError> {
        ctl.apply(Intent::SetModulatorField { node, field, value })
    }

    use crate::app::model::ModSourceSlot;
    use crate::plugin::chain::LfoWaveform;

    fn lane_lfo(ctl: &Controller, index: usize) -> (LfoWaveform, f32) {
        match &ctl.model.instruments[0].modulators[index].source {
            ModSourceSlot::Lfo { waveform, rate } => (*waveform, *rate),
            other => panic!("{other:?} is not an LFO"),
        }
    }

    fn lane_envelope(ctl: &Controller, index: usize) -> [f32; 4] {
        match &ctl.model.instruments[0].modulators[index].source {
            ModSourceSlot::Envelope { attack, decay, sustain, release } => [*attack, *decay, *sustain, *release],
            other => panic!("{other:?} is not an envelope"),
        }
    }

    #[test]
    fn set_modulator_field_sets_a_lane_lfos_rate_and_waveform_in_the_model_and_the_engine() {
        let (engine, mut ctl) = modulated();
        sent(&engine);
        let lfo = lane_mod(&ctl, 0);
        let applied = field(&mut ctl, lfo, ModulatorField::Rate, 5.0).unwrap();
        assert_eq!(applied, Applied::of(1, vec![Event::ModulatorFieldSet { node: lfo, field: ModulatorField::Rate, value: 5.0 }]));
        let applied = field(&mut ctl, lfo, ModulatorField::Waveform, 1.0).unwrap();
        assert_eq!(applied.events, [Event::ModulatorFieldSet { node: lfo, field: ModulatorField::Waveform, value: 1.0 }]);
        assert_eq!(lane_lfo(&ctl, 0), (LfoWaveform::Triangle, 5.0));
        assert_eq!(sent(&engine), ["SetModulatorRate inst=0 mod=0 rate=5.000", "SetModulatorWaveform inst=0 mod=0 waveform=Triangle"]);
        assert!(ctl.dirty);
        assert_eq!(ctl.rev, 2);
    }

    #[test]
    fn set_modulator_field_sets_an_envelope_stage_and_sends_all_four() {
        let (engine, mut ctl) = modulated();
        sent(&engine);
        let envelope = lane_mod(&ctl, 1);
        field(&mut ctl, envelope, ModulatorField::Attack, 0.5).unwrap();
        field(&mut ctl, envelope, ModulatorField::Decay, 1.5).unwrap();
        field(&mut ctl, envelope, ModulatorField::Sustain, 0.25).unwrap();
        field(&mut ctl, envelope, ModulatorField::Release, 2.0).unwrap();
        assert_eq!(lane_envelope(&ctl, 1), [0.5, 1.5, 0.25, 2.0]);
        assert_eq!(
            sent(&engine),
            [
                "SetModulatorEnvelopeParam inst=0 mod=1 attack=0.500 decay=0.300 sustain=0.600 release=0.500",
                "SetModulatorEnvelopeParam inst=0 mod=1 attack=0.500 decay=1.500 sustain=0.600 release=0.500",
                "SetModulatorEnvelopeParam inst=0 mod=1 attack=0.500 decay=1.500 sustain=0.250 release=0.500",
                "SetModulatorEnvelopeParam inst=0 mod=1 attack=0.500 decay=1.500 sustain=0.250 release=2.000",
            ]
        );
    }

    #[test]
    fn set_modulator_field_reaches_a_groups_modulators_by_the_groups_commands() {
        let (engine, mut ctl) = modulated();
        sent(&engine);
        let lfo = group_mod(&ctl, 0);
        let envelope = group_mod(&ctl, 1);
        field(&mut ctl, lfo, ModulatorField::Rate, 7.5).unwrap();
        field(&mut ctl, lfo, ModulatorField::Waveform, 3.0).unwrap();
        field(&mut ctl, envelope, ModulatorField::Sustain, 0.1).unwrap();
        assert_eq!(
            sent(&engine),
            [
                "SetGroupModulatorRate group=0 mod=0 rate=7.500",
                "SetGroupModulatorWaveform group=0 mod=0 waveform=Square",
                "SetGroupModulatorEnvelopeParam group=0 mod=1 attack=0.010 decay=0.300 sustain=0.100 release=0.500",
            ]
        );
        assert!(matches!(ctl.model.groups[0].modulators[0].source, ModSourceSlot::Lfo { waveform: LfoWaveform::Square, rate } if rate == 7.5));
    }

    #[test]
    fn set_modulator_field_clamps_to_each_fields_range_and_rounds_the_waveform() {
        let (_engine, mut ctl) = modulated();
        let lfo = lane_mod(&ctl, 0);
        let envelope = lane_mod(&ctl, 1);
        let value_of = |applied: Applied| match applied.events[0] {
            Event::ModulatorFieldSet { value, .. } => value,
            ref other => panic!("{other:?}"),
        };
        let cases = [
            (lfo, ModulatorField::Rate, 1.0e9, 50.0),
            (lfo, ModulatorField::Rate, -3.0, 0.01),
            (envelope, ModulatorField::Attack, 0.0, 0.001),
            (envelope, ModulatorField::Decay, 99.0, 10.0),
            (envelope, ModulatorField::Sustain, 2.0, 1.0),
            (envelope, ModulatorField::Sustain, -2.0, 0.0),
            (envelope, ModulatorField::Release, 0.0, 0.001),
            (lfo, ModulatorField::Waveform, 2.6, 3.0),
            (lfo, ModulatorField::Waveform, 1.4, 1.0),
            (lfo, ModulatorField::Waveform, 9.0, 3.0),
            (lfo, ModulatorField::Waveform, -4.0, 0.0),
        ];
        for (node, which, asked, expected) in cases {
            assert_eq!(value_of(field(&mut ctl, node, which, asked).unwrap()), expected, "{which:?} {asked}");
        }
        assert_eq!(lane_lfo(&ctl, 0).1, 0.01);
        assert_eq!(lane_lfo(&ctl, 0).0, LfoWaveform::Sine);
    }

    #[test]
    fn set_modulator_field_refuses_what_it_cannot_do_and_changes_nothing() {
        let (engine, mut ctl) = modulated();
        sent(&engine);
        let lfo = lane_mod(&ctl, 0);
        let envelope = lane_mod(&ctl, 1);
        let learn = lane_mod(&ctl, 2);
        let instrument = ctl.model.instruments[0].node;
        let effect = ctl.model.instruments[0].effects[0].node;
        let group = ctl.model.groups[0].node;
        let ghost = NodeId(9_999);
        let refused = [
            (lfo, ModulatorField::Attack, 0.1, "does not apply"),
            (lfo, ModulatorField::Sustain, 0.1, "does not apply"),
            (envelope, ModulatorField::Rate, 1.0, "does not apply"),
            (envelope, ModulatorField::Waveform, 1.0, "does not apply"),
            (learn, ModulatorField::Rate, 1.0, "does not apply"),
            (learn, ModulatorField::Attack, 1.0, "does not apply"),
            (instrument, ModulatorField::Rate, 1.0, "not a modulator"),
            (effect, ModulatorField::Rate, 1.0, "not a modulator"),
            (group, ModulatorField::Rate, 1.0, "not a modulator"),
            (lfo, ModulatorField::Rate, f32::NAN, "not a value"),
            (lfo, ModulatorField::Rate, f32::INFINITY, "not a value"),
        ];
        for (node, which, value, why) in refused {
            let error = field(&mut ctl, node, which, value).unwrap_err().to_string();
            assert!(error.contains(why), "{which:?} {value}: {error}");
        }
        assert_eq!(field(&mut ctl, ghost, ModulatorField::Rate, 1.0), Err(IntentError::NotFound(ghost)));
        assert!(sent(&engine).is_empty(), "nothing was sent");
        assert_eq!((ctl.rev, ctl.dirty), (0, false));
        assert_eq!(lane_lfo(&ctl, 0), (LfoWaveform::Saw, 3.0));
        assert_eq!(lane_envelope(&ctl, 1), [0.02, 0.3, 0.6, 0.5]);
    }

    #[test]
    fn set_modulator_field_changes_only_the_modulator_it_names() {
        let (_engine, mut ctl) = modulated();
        let before_other_lfo = lane_lfo(&ctl, 3);
        let before_group = ctl.model.groups[0].modulators.iter().map(|m| format!("{:?}", m.source)).collect::<Vec<_>>();
        let lfo = lane_mod(&ctl, 0);
        field(&mut ctl, lfo, ModulatorField::Rate, 9.0).unwrap();
        assert_eq!(lane_lfo(&ctl, 3), before_other_lfo, "another lane modulator");
        assert_eq!(ctl.model.groups[0].modulators.iter().map(|m| format!("{:?}", m.source)).collect::<Vec<_>>(), before_group, "the group's");
        assert_eq!(lane_envelope(&ctl, 1), [0.02, 0.3, 0.6, 0.5]);
        assert_eq!(ctl.model.instruments[0].modulators[0].targets.len(), 2, "its targets");
    }

    fn lane_edge(ctl: &Controller, modulator: usize, target: usize) -> EdgeId {
        ctl.model.instruments[0].modulators[modulator].targets[target].edge
    }

    fn depth(ctl: &Controller, modulator: usize, target: usize) -> f32 {
        ctl.model.instruments[0].modulators[modulator].targets[target].depth
    }

    #[test]
    fn set_edge_depth_sets_the_depth_of_a_cable_in_the_model_and_the_engine() {
        let (engine, mut ctl) = modulated();
        sent(&engine);
        let cutoff = lane_edge(&ctl, 0, 0);
        let rate = lane_edge(&ctl, 0, 1);
        let applied = ctl.apply(Intent::SetEdgeDepth { edge: cutoff, depth: 0.75 }).unwrap();
        assert_eq!(applied, Applied::of(1, vec![Event::EdgeDepthSet { edge: cutoff, depth: 0.75 }]));
        // The second one drives a modulator's own rate, and a cable can pull the other way.
        ctl.apply(Intent::SetEdgeDepth { edge: rate, depth: -0.5 }).unwrap();
        assert_eq!((depth(&ctl, 0, 0), depth(&ctl, 0, 1)), (0.75, -0.5));
        assert_eq!(
            sent(&engine),
            ["SetModTargetDepth inst=0 mod=0 target=0 depth=0.750", "SetModTargetDepth inst=0 mod=0 target=1 depth=-0.500"]
        );
        assert!(ctl.dirty);
    }

    #[test]
    fn set_edge_depth_reaches_a_group_modulators_cable_by_the_groups_command() {
        let (engine, mut ctl) = modulated();
        sent(&engine);
        let edge = ctl.model.groups[0].modulators[0].targets[0].edge;
        ctl.apply(Intent::SetEdgeDepth { edge, depth: 0.1 }).unwrap();
        assert_eq!(sent(&engine), ["SetGroupModTargetDepth group=0 mod=0 target=0 depth=0.100"]);
        assert_eq!(ctl.model.groups[0].modulators[0].targets[0].depth, 0.1);
    }

    #[test]
    fn set_edge_depth_clamps_to_minus_one_and_one_and_refuses_what_is_not_a_cable_or_a_number() {
        let (engine, mut ctl) = modulated();
        let edge = lane_edge(&ctl, 0, 0);
        let depth_of = |applied: Applied| match applied.events[0] {
            Event::EdgeDepthSet { depth, .. } => depth,
            ref other => panic!("{other:?}"),
        };
        assert_eq!(depth_of(ctl.apply(Intent::SetEdgeDepth { edge, depth: 5.0 }).unwrap()), 1.0);
        assert_eq!(depth_of(ctl.apply(Intent::SetEdgeDepth { edge, depth: -5.0 }).unwrap()), -1.0);
        sent(&engine);
        let rev = ctl.rev;
        let ghost = EdgeId(9_999);
        let error = ctl.apply(Intent::SetEdgeDepth { edge: ghost, depth: 0.5 }).unwrap_err();
        assert!(error.to_string().contains("no cable 9999"), "{error}");
        let error = ctl.apply(Intent::SetEdgeDepth { edge, depth: f32::NAN }).unwrap_err();
        assert!(error.to_string().contains("not a value"), "{error}");
        assert!(sent(&engine).is_empty());
        assert_eq!(ctl.rev, rev);
        assert_eq!(depth(&ctl, 0, 0), -1.0);
    }

    #[test]
    fn set_edge_depth_changes_only_the_cable_it_names() {
        let (_engine, mut ctl) = modulated();
        let before = (depth(&ctl, 0, 1), ctl.model.groups[0].modulators[0].targets[0].depth);
        let edge = lane_edge(&ctl, 0, 0);
        ctl.apply(Intent::SetEdgeDepth { edge, depth: 0.9 }).unwrap();
        assert_eq!((depth(&ctl, 0, 1), ctl.model.groups[0].modulators[0].targets[0].depth), before);
    }

    #[test]
    fn remove_edge_takes_a_cable_out_of_the_model_and_the_engine_and_says_the_structure_changed() {
        let (engine, mut ctl) = modulated();
        sent(&engine);
        let (cutoff, rate) = (lane_edge(&ctl, 0, 0), lane_edge(&ctl, 0, 1));
        let applied = ctl.apply(Intent::RemoveEdge { edge: cutoff }).unwrap();
        assert_eq!(applied, Applied::restructured(1));
        assert!(applied.events.is_empty(), "nothing a client could apply: it is told by a snapshot");
        // (The filter's cutoff, which the cable drove, is given the value the user set.)
        assert_eq!(sent(&engine), ["RemoveModTarget inst=0 mod=0 target=0", "SetParameter inst=0 slot=1 param=0 value=1.000"]);
        let rack = &ctl.model.instruments[0].modulators;
        assert_eq!(rack[0].targets.iter().map(|t| t.edge).collect::<Vec<_>>(), [rate], "the other cable is as it was");
        assert!(ctl.dirty);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn remove_edge_takes_out_a_cable_that_drives_a_modulators_own_rate_and_one_of_a_group() {
        let (engine, mut ctl) = modulated();
        sent(&engine);
        // The lane's first modulator's second cable drives the rate of the lane's fourth modulator.
        let rate = lane_edge(&ctl, 0, 1);
        ctl.apply(Intent::RemoveEdge { edge: rate }).unwrap();
        // (And the rate is the one the user set.)
        assert_eq!(sent(&engine), ["RemoveModTarget inst=0 mod=0 target=1", "SetModulatorRate inst=0 mod=3 rate=1.000"]);
        assert_eq!(ctl.model.instruments[0].modulators[0].targets.len(), 1);
        // The group modulator's cable to its bus filter's cutoff: by the group's command.
        let bus = ctl.model.groups[0].modulators[0].targets[0].edge;
        ctl.apply(Intent::RemoveEdge { edge: bus }).unwrap();
        assert_eq!(sent(&engine), ["RemoveGroupModTarget group=0 mod=0 target=0", "SetGroupParameter group=0 fx=0 param=0 value=1.000"]);
        assert!(ctl.model.groups[0].modulators[0].targets.is_empty());
        assert_eq!(ctl.model.groups[0].modulators.len(), 2, "its modulator stays");
        assert_eq!(ctl.rev, 2);
    }

    #[test]
    fn remove_edge_leaves_the_modulator_when_its_last_cable_goes() {
        let (_engine, mut ctl) = modulated();
        let (first, second) = (lane_edge(&ctl, 0, 0), lane_edge(&ctl, 0, 1));
        let modulator = lane_mod(&ctl, 0);
        ctl.apply(Intent::RemoveEdge { edge: first }).unwrap();
        ctl.apply(Intent::RemoveEdge { edge: second }).unwrap();
        assert!(ctl.model.instruments[0].modulators[0].targets.is_empty());
        assert_eq!(lane_mod(&ctl, 0), modulator, "and is the same one");
    }

    #[test]
    fn remove_edge_refuses_a_cable_that_is_not_there_and_changes_nothing() {
        let (engine, mut ctl) = modulated();
        sent(&engine);
        let edge = lane_edge(&ctl, 0, 0);
        let error = ctl.apply(Intent::RemoveEdge { edge: EdgeId(9_999) }).unwrap_err();
        assert_eq!(error, IntentError::EdgeNotFound(EdgeId(9_999)));
        assert!(error.to_string().contains("no cable 9999"), "{error}");
        assert!(sent(&engine).is_empty());
        assert_eq!((ctl.rev, ctl.dirty), (0, false));
        ctl.apply(Intent::RemoveEdge { edge }).unwrap();
        // A cable that was removed is not there the second time.
        sent(&engine);
        assert_eq!(ctl.apply(Intent::RemoveEdge { edge }).unwrap_err(), IntentError::EdgeNotFound(edge));
        assert!(sent(&engine).is_empty());
        assert_eq!(ctl.rev, 1, "a refusal is not a revision");
    }

    fn add(ctl: &mut Controller, parent: NodeId, kind: ModulatorKind) -> Result<Applied, IntentError> {
        ctl.apply(Intent::AddModulator { parent, kind })
    }

    #[test]
    fn add_modulator_puts_an_lfo_at_the_end_of_a_lanes_rack_in_the_model_and_the_engine() {
        let (engine, mut ctl) = controller();
        sent(&engine);
        let parent = lane(&ctl);
        let applied = add(&mut ctl, parent, ModulatorKind::Lfo).unwrap();
        assert_eq!(applied, Applied::restructured(1));
        assert!(applied.events.is_empty(), "nothing a client could apply: it is told by a snapshot");
        assert_eq!(sent(&engine), ["InsertModulator inst=0 index=0 source=Lfo { waveform: Sine, rate: 1.0, phase: 0.0 }"]);
        let rack = &ctl.model.instruments[0].modulators;
        assert_eq!(rack.len(), 1);
        assert!(matches!(rack[0].source, ModSourceSlot::Lfo { waveform: LfoWaveform::Sine, rate } if rate == 1.0));
        assert!(rack[0].targets.is_empty(), "it drives nothing yet");
        assert_eq!(ctl.model.locate(rack[0].node), Some(NodePath::Modulator { at: ModulatorLocation::Lane(0), index: 0 }));
        assert!(ctl.dirty);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn add_modulator_adds_after_the_modulators_a_rack_has_and_leaves_them_as_they_were() {
        let (engine, mut ctl) = modulated();
        sent(&engine);
        let before: Vec<(NodeId, String, usize)> =
            ctl.model.instruments[0].modulators.iter().map(|m| (m.node, format!("{:?}", m.source), m.targets.len())).collect();
        let parent = lane(&ctl);
        add(&mut ctl, parent, ModulatorKind::Envelope).unwrap();
        let rack = &ctl.model.instruments[0].modulators;
        assert_eq!(rack.len(), before.len() + 1);
        let still: Vec<(NodeId, String, usize)> = rack.iter().take(before.len()).map(|m| (m.node, format!("{:?}", m.source), m.targets.len())).collect();
        assert_eq!(still, before);
        assert!(!before.iter().any(|(node, ..)| *node == rack[before.len()].node), "a node of its own");
        assert_eq!(
            sent(&engine),
            [format!(
                "InsertModulator inst=0 index={} source=Envelope {{ attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5, state: Idle, level: 0.0, notes_held: 0 }}",
                before.len()
            )]
        );
        assert_eq!(ctl.model.groups[0].modulators.len(), 2, "the group's rack is untouched");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn add_modulator_to_a_group_goes_in_the_groups_rack_by_the_groups_command() {
        let (engine, mut ctl) = controller();
        sent(&engine);
        let group = node(&ctl, NodePath::Group(0));
        add(&mut ctl, group, ModulatorKind::Envelope).unwrap();
        assert_eq!(
            sent(&engine),
            ["InsertGroupModulator group=0 index=1 source=Envelope { attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5, state: Idle, level: 0.0, notes_held: 0 }"]
        );
        let rack = &ctl.model.groups[0].modulators;
        assert_eq!(rack.len(), 2);
        assert_eq!(ctl.model.locate(rack[1].node), Some(NodePath::Modulator { at: ModulatorLocation::Group(0), index: 1 }));
        assert!(ctl.model.instruments[0].modulators.is_empty(), "the member's rack is untouched");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn add_modulator_makes_each_kind_what_it_says_and_an_empty_lane_takes_one_too() {
        let (engine, mut ctl) = controller();
        sent(&engine);
        let empty = node(&ctl, NodePath::Instrument(1));
        for kind in [ModulatorKind::Lfo, ModulatorKind::Envelope, ModulatorKind::MidiLearn] {
            add(&mut ctl, empty, kind).unwrap();
        }
        let rack = &ctl.model.instruments[1].modulators;
        assert!(matches!(rack[0].source, ModSourceSlot::Lfo { waveform: LfoWaveform::Sine, rate } if rate == 1.0));
        assert!(matches!(
            rack[1].source,
            ModSourceSlot::Envelope { attack, decay, sustain, release } if (attack, decay, sustain, release) == (0.01, 0.3, 0.7, 0.5)
        ));
        assert!(matches!(rack[2].source, ModSourceSlot::MidiLearn { source: None, learning: true }), "armed for the next control");
        assert_eq!(
            sent(&engine),
            [
                "InsertModulator inst=1 index=0 source=Lfo { waveform: Sine, rate: 1.0, phase: 0.0 }".to_string(),
                "InsertModulator inst=1 index=1 source=Envelope { attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5, state: Idle, level: 0.0, notes_held: 0 }".to_string(),
                "InsertModulator inst=1 index=2 source=MidiLearn { source: None, value: 0.0, learning: true }".to_string(),
            ]
        );
        assert_eq!(ctl.rev, 3);
        ctl.model.check_ids().unwrap();
    }

    fn remove(ctl: &mut Controller, node: NodeId) -> Result<Applied, IntentError> {
        ctl.apply(Intent::RemoveNode { node })
    }

    /// Two groups (each with a bus effect and a modulator), three lanes in them (the first has an effect and a
    /// modulator, the second a modulator, the third an effect), a lane of its own with an effect and a modulator,
    /// and a lane with nothing in it.
    const REACH: &str = r#"
[[group]]
[[group.effect]]
plugin = "builtin:filter"
[[group.modulator]]
type = "lfo"

[[group]]
[[group.effect]]
plugin = "builtin:reverb"
[[group.modulator]]
type = "envelope"

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.modulator]]
type = "lfo"

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.modulator]]
type = "envelope"

[[instrument]]
plugin = "builtin:osc"
group = 1
[[instrument.effect]]
plugin = "builtin:reverb"

[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.modulator]]
type = "midi"

[[instrument]]
"#;

    /// `REACH`, and an LFO in the lane that has nothing in it (a session file cannot say so: its modulators are
    /// those of the instrument plugin; a lane the TUI cleared can have them).
    fn reaching() -> (StubEngine, Controller) {
        let (handles, engine) = testutil::stub_engine();
        let mut ctl = Controller::new(load(REACH).model, handles, None);
        ctl.add_modulator(ModulatorLocation::Lane(4), ModSourceSlot::new_lfo()).unwrap();
        (engine, ctl)
    }

    fn cable_to(modulator: NodeId, node: NodeId, param: u32) -> Intent {
        Intent::AddModEdge { modulator, to: Endpoint { node, port: Port::Param { index: param } } }
    }

    /// The lowest and the highest value of the parameter `name` of the plugin at `path`.
    fn bounds(ctl: &Controller, path: NodePath, name: &str) -> (f32, f32) {
        let (_, p) = ctl.model.nodes().find(|(_, p)| *p == path).unwrap();
        let slot = match p {
            NodePath::Instrument(i) => ctl.model.instruments[i].instrument.as_ref().unwrap(),
            NodePath::Effect { inst, index } => &ctl.model.instruments[inst].effects[index],
            NodePath::GroupEffect { group, index } => &ctl.model.groups[group].effects[index],
            _ => panic!("{p:?} has no plugin"),
        };
        let row = slot.params.iter().find(|r| r.name == name).unwrap();
        (row.min, row.max)
    }

    /// What the audio thread is told of a cable to a parameter: how it is addressed, and the depth, base value and range it starts from.
    fn command(prefix: &str, kind: &str, ctl: &Controller, path: NodePath, name: &str) -> String {
        let (_, value) = param(ctl, path, name);
        let (min, max) = bounds(ctl, path, name);
        format!("{prefix} kind={kind} depth=0.500 base={value:.3} range=[{min:.3}, {max:.3}]")
    }

    #[test]
    fn add_mod_edge_draws_a_cable_from_a_lane_modulator_to_its_instrument_and_to_its_effects() {
        let (engine, mut ctl) = reaching();
        sent(&engine);
        let lfo = ctl.model.instruments[0].modulators[0].node;
        let (osc, filter) = (ctl.model.instruments[0].node, ctl.model.instruments[0].effects[0].node);
        let (detune, _) = param(&ctl, NodePath::Instrument(0), "detune");
        let (cutoff, _) = param(&ctl, NodePath::Effect { inst: 0, index: 0 }, "cutoff");

        let applied = ctl.apply(cable_to(lfo, osc, detune)).unwrap();
        assert_eq!(applied, Applied::restructured(1));
        assert!(applied.events.is_empty(), "nothing a client could apply: it is told by a snapshot");
        assert_eq!(
            sent(&engine),
            [command("AddModTarget inst=0 mod=0", &format!("PluginParam {{ slot: 0, param_index: {detune} }}"), &ctl, NodePath::Instrument(0), "detune")]
        );
        ctl.apply(cable_to(lfo, filter, cutoff)).unwrap();
        assert_eq!(
            sent(&engine),
            [command("AddModTarget inst=0 mod=0", &format!("PluginParam {{ slot: 1, param_index: {cutoff} }}"), &ctl, NodePath::Effect { inst: 0, index: 0 }, "cutoff")]
        );

        let targets = &ctl.model.instruments[0].modulators[0].targets;
        assert_eq!(targets.len(), 2);
        assert_eq!((targets[0].slot, targets[0].param_name.as_str(), targets[0].depth), (0, "detune", 0.5));
        assert!(matches!(targets[0].kind, ModTargetKind::PluginParam { slot: 0, param_index } if param_index == detune));
        assert_eq!((targets[0].param_min, targets[0].param_max), (-24.0, 24.0));
        assert_eq!((targets[1].slot, targets[1].param_name.as_str(), targets[1].depth), (1, "cutoff", 0.5));
        assert!(matches!(targets[1].kind, ModTargetKind::PluginParam { slot: 1, param_index } if param_index == cutoff));
        assert_ne!(targets[0].edge, targets[1].edge, "a cable of its own each");
        assert!(ctl.dirty);
        ctl.model.check_ids().unwrap();
        assert!(ctl.model.groups[0].modulators[0].targets.is_empty(), "the group's modulator is untouched");
    }

    #[test]
    fn add_mod_edge_draws_a_cable_from_a_group_modulator_to_its_members_and_its_bus_effects() {
        let (engine, mut ctl) = reaching();
        sent(&engine);
        let group_lfo = ctl.model.groups[0].modulators[0].node;
        // Lanes 0 and 1 are the members of group 0, in that order: the second one's instrument is member 1.
        let (second, first_filter, bus) = (ctl.model.instruments[1].node, ctl.model.instruments[0].effects[0].node, ctl.model.groups[0].effects[0].node);
        let (detune, _) = param(&ctl, NodePath::Instrument(1), "detune");
        let (cutoff, _) = param(&ctl, NodePath::Effect { inst: 0, index: 0 }, "cutoff");
        let (bus_cutoff, _) = param(&ctl, NodePath::GroupEffect { group: 0, index: 0 }, "cutoff");

        ctl.apply(cable_to(group_lfo, second, detune)).unwrap();
        ctl.apply(cable_to(group_lfo, first_filter, cutoff)).unwrap();
        ctl.apply(cable_to(group_lfo, bus, bus_cutoff)).unwrap();
        assert_eq!(
            sent(&engine),
            [
                command("AddGroupModTarget group=0 mod=0", &format!("GroupMember {{ member: 1, slot: 0, param_index: {detune} }}"), &ctl, NodePath::Instrument(1), "detune"),
                command("AddGroupModTarget group=0 mod=0", &format!("GroupMember {{ member: 0, slot: 1, param_index: {cutoff} }}"), &ctl, NodePath::Effect { inst: 0, index: 0 }, "cutoff"),
                command("AddGroupModTarget group=0 mod=0", &format!("GroupBus {{ effect_index: 0, param_index: {bus_cutoff} }}"), &ctl, NodePath::GroupEffect { group: 0, index: 0 }, "cutoff"),
            ]
        );
        let targets = &ctl.model.groups[0].modulators[0].targets;
        assert_eq!(targets.iter().map(|t| (t.slot, t.param_name.as_str(), t.depth)).collect::<Vec<_>>(), [(0, "detune", 0.5), (1, "cutoff", 0.5), (0, "cutoff", 0.5)]);
        assert!(ctl.model.instruments[0].modulators[0].targets.is_empty(), "the lane's own modulator is untouched");
        ctl.model.check_ids().unwrap();
    }

    /// Where a client may draw a cable from a modulator, as it can tell from the graph alone, by the `parent` of
    /// each node (`reachFrom` in `web/src/app/patch/reach.ts` is the same rule): a lane's modulator reaches the
    /// plugins in its lane, a group's the plugins of the lanes that are its members and its own bus effects.
    fn reach_by_parent(graph: &Graph, modulator: NodeId) -> Vec<NodeId> {
        let node = |id: NodeId| graph.nodes.iter().find(|n| n.id == id).unwrap();
        let children = |id: NodeId| graph.nodes.iter().filter(move |n| n.parent == Some(id));
        let is_effect = |n: &&crate::app::graph::Node| matches!(n.kind, NodeKind::Effect { .. });
        // The chain of a lane: its instrument, if it has one (the lane's own node), and the effects whose parent it is.
        let chain = |lane: &crate::app::graph::Node| -> Vec<NodeId> {
            let instrument = matches!(lane.kind, NodeKind::Instrument { plugin: Some(_), .. }).then_some(lane.id);
            instrument.into_iter().chain(children(lane.id).filter(is_effect).map(|n| n.id)).collect()
        };
        let owner = node(node(modulator).parent.expect("a modulator sits in a lane or a group"));
        let mut found: Vec<NodeId> = match owner.kind {
            NodeKind::Group { .. } => {
                let bus = children(owner.id).filter(is_effect).map(|n| n.id);
                let members = children(owner.id).filter(|n| matches!(n.kind, NodeKind::Instrument { .. })).flat_map(chain);
                members.chain(bus).collect()
            }
            _ => chain(owner),
        };
        found.sort();
        found
    }

    /// Every plugin of the session with a parameter to try (the lane that has no plugin, which a cable cannot end at, is among them with 0).
    fn plugins_to_try(ctl: &Controller) -> Vec<(NodeId, u32)> {
        ctl.model
            .nodes()
            .filter_map(|(id, path)| {
                let slot = match path {
                    NodePath::Instrument(i) => ctl.model.instruments[i].instrument.as_ref(),
                    NodePath::Effect { inst, index } => ctl.model.instruments[inst].effects.get(index),
                    NodePath::GroupEffect { group, index } => ctl.model.groups[group].effects.get(index),
                    NodePath::Group(_) | NodePath::Modulator { .. } => return None,
                };
                Some((id, slot.map_or(0, |plugin| plugin.params[0].index)))
            })
            .collect()
    }

    /// The plugins the controller lets `modulator` draw a cable to: a cable to each is tried on a session of its own.
    fn reach_by_controller(modulator: NodeId, plugins: &[(NodeId, u32)]) -> Vec<NodeId> {
        let mut reached: Vec<NodeId> = plugins
            .iter()
            .filter(|&&(node, param)| {
                let (_engine, mut fresh) = reaching();
                fresh.apply(cable_to(modulator, node, param)).is_ok()
            })
            .map(|&(node, _)| node)
            .collect();
        reached.sort();
        reached
    }

    fn modulators_of(ctl: &Controller) -> Vec<NodeId> {
        ctl.model.nodes().filter(|(_, path)| matches!(path, NodePath::Modulator { .. })).map(|(id, _)| id).collect()
    }

    #[test]
    fn add_mod_edge_reaches_what_a_client_can_tell_from_the_graph_that_a_modulator_reaches() {
        let (_engine, ctl) = reaching();
        let graph = ctl.model.graph();
        let plugins = plugins_to_try(&ctl);
        assert_eq!(plugins.len(), 10, "four lanes with a plugin and one without, three lane effects and two bus effects");
        let modulators = modulators_of(&ctl);
        assert_eq!(modulators.len(), 6);

        let mut reached = Vec::new();
        for modulator in modulators {
            let by_controller = reach_by_controller(modulator, &plugins);
            assert_eq!(by_controller, reach_by_parent(&graph, modulator), "{modulator:?}");
            reached.push(by_controller.len());
        }
        // Group 0's LFO: lane 0's two plugins, lane 1's, the bus filter. Group 1's envelope: lane 2's two, the bus reverb.
        // Lane 0's LFO: its two; lane 1's envelope: one; lane 3's control: two; the empty lane's LFO: none.
        assert_eq!(reached, [4, 3, 2, 1, 2, 0]);
    }

    /// The session of `reaching()`, and what the controller lets each of its modulators draw a cable to, as the web
    /// client's tests read it (`web/src/client/fixtures/reach.json`): they pin the rule the client draws its valid
    /// targets by (`reachFrom`) to the controller's. `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture`
    /// rewrites it.
    #[test]
    fn the_web_clients_fixture_of_what_a_modulator_reaches_is_current() {
        let (_engine, ctl) = reaching();
        let plugins = plugins_to_try(&ctl);
        let reach: std::collections::BTreeMap<String, Vec<NodeId>> =
            modulators_of(&ctl).into_iter().map(|modulator| (modulator.0.to_string(), reach_by_controller(modulator, &plugins))).collect();
        // Through the text the wire carries, so that the numbers read as they do there.
        let wire = serde_json::to_string(&ctl.snapshot()).unwrap();
        let mut snapshot: serde_json::Value = serde_json::from_str(&wire).unwrap();
        // The layout's fingerprint comes from the hasher, which the standard library does not promise to keep the same.
        snapshot["telemetry"]["rev"] = serde_json::json!(1);
        let fixture = serde_json::json!({ "snapshot": snapshot, "reach": reach });
        let fresh = serde_json::to_string_pretty(&fixture).unwrap() + "\n";

        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/src/client/fixtures/reach.json");
        if std::env::var_os("TANG_UPDATE_TYPES").is_some() {
            std::fs::write(&path, &fresh).unwrap();
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            committed == fresh,
            "{} is not what the session reaches now: run `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` and commit the result",
            path.display()
        );
    }

    #[test]
    fn add_mod_edge_refuses_what_it_cannot_do_and_changes_nothing() {
        let (engine, mut ctl) = reaching();
        sent(&engine);
        let slots = ctl.model.slots.high_water();
        let lfo = ctl.model.instruments[0].modulators[0].node;
        let group_lfo = ctl.model.groups[0].modulators[0].node;
        let (osc, effect, group) = (ctl.model.instruments[0].node, ctl.model.instruments[0].effects[0].node, ctl.model.groups[0].node);
        let lane_of_its_own = ctl.model.instruments[3].node;
        let empty_lane = ctl.model.instruments[4].node;
        let other_bus = ctl.model.groups[1].effects[0].node;
        let (detune, _) = param(&ctl, NodePath::Instrument(0), "detune");
        let ghost = NodeId(9_999);
        let heading = ctl.model.instruments[0].instrument.as_ref().unwrap().params[1].index;
        ctl.model.instruments[0].instrument.as_mut().unwrap().params[1].kind = ParamKind::Separator;

        let not_a_parameter = invalid("a cable can only end at a plugin's parameter so far");
        let from_a_modulator = invalid("a cable starts at a modulator");
        let cases = [
            ("a modulator that is not there", cable_to(ghost, osc, detune), IntentError::NotFound(ghost)),
            ("the master as the modulator", cable_to(NodeId::MASTER, osc, detune), IntentError::NotFound(NodeId::MASTER)),
            ("a lane as the modulator", cable_to(osc, osc, detune), from_a_modulator.clone()),
            ("an effect as the modulator", cable_to(effect, osc, detune), from_a_modulator.clone()),
            ("a group as the modulator", cable_to(group, osc, detune), from_a_modulator),
            ("a plugin that is not there", cable_to(lfo, ghost, 0), IntentError::NotFound(ghost)),
            ("the master as the plugin", cable_to(lfo, NodeId::MASTER, 0), IntentError::NotFound(NodeId::MASTER)),
            ("a group as the plugin", cable_to(lfo, group, 0), invalid("that node has no plugin")),
            ("a modulator as the plugin", cable_to(lfo, group_lfo, 0), invalid("that node has no plugin")),
            ("a lane with no instrument", cable_to(lfo, empty_lane, 0), invalid("that instrument has no plugin")),
            ("a plugin of another lane", cable_to(lfo, lane_of_its_own, detune), invalid("a modulator of this lane cannot reach that plugin")),
            ("a bus effect, from a lane's modulator", cable_to(lfo, ctl.model.groups[0].effects[0].node, 0), invalid("a modulator of this lane cannot reach that plugin")),
            ("a plugin of another group", cable_to(group_lfo, other_bus, 0), invalid("a modulator of this group cannot reach that plugin")),
            ("a lane outside the group", cable_to(group_lfo, lane_of_its_own, detune), invalid("a modulator of this group cannot reach that plugin")),
            ("a parameter the plugin lacks", cable_to(lfo, osc, 999), invalid("the plugin has no parameter 999")),
            ("a heading, which is not a parameter", cable_to(lfo, osc, heading), invalid(format!("the plugin has no parameter {heading}"))),
        ];
        for (what, intent, error) in cases {
            assert_eq!(ctl.apply(intent), Err(error), "{what}");
        }
        // The ports that are not parameters (the cables that drive a modulator's own settings are not drawn by intent yet).
        let others = [Port::ModOut, Port::AudioIn, Port::AudioOut, Port::Rate, Port::Attack, Port::Sustain, Port::Depth { edge: EdgeId(0) }];
        for port in others {
            let intent = Intent::AddModEdge { modulator: lfo, to: Endpoint { node: osc, port } };
            assert_eq!(ctl.apply(intent), Err(not_a_parameter.clone()), "{port:?}");
        }

        assert!(sent(&engine).is_empty(), "nothing reached the audio thread");
        assert_eq!((ctl.rev, ctl.dirty), (0, false), "a refusal is not a revision");
        assert_eq!(ctl.model.slots.high_water(), slots, "no telemetry slot is used up for nothing");
        assert!(ctl.model.groups.iter().flat_map(|g| &g.modulators).chain(ctl.model.instruments.iter().flat_map(|l| &l.modulators)).all(|m| m.targets.is_empty()));
        // And no cable id was used up: the first cable that is drawn is the first there is.
        let (cutoff, _) = param(&ctl, NodePath::Effect { inst: 0, index: 0 }, "cutoff");
        ctl.apply(cable_to(lfo, effect, cutoff)).unwrap();
        assert_eq!(ctl.model.instruments[0].modulators[0].targets[0].edge, EdgeId(0));
    }

    #[test]
    fn add_mod_edge_refuses_a_modulator_that_drives_the_parameter_already_but_not_another_one() {
        let (engine, mut ctl) = reaching();
        let lfo = ctl.model.instruments[0].modulators[0].node;
        let osc = ctl.model.instruments[0].node;
        let (detune, _) = param(&ctl, NodePath::Instrument(0), "detune");
        let (volume, _) = param(&ctl, NodePath::Instrument(0), "volume");
        ctl.apply(cable_to(lfo, osc, detune)).unwrap();
        sent(&engine);
        let rev = ctl.rev;
        assert_eq!(ctl.apply(cable_to(lfo, osc, detune)), Err(invalid("that modulator drives that parameter already")));
        assert!(sent(&engine).is_empty());
        assert_eq!(ctl.rev, rev, "a refusal is not a revision");
        assert_eq!(ctl.model.instruments[0].modulators[0].targets.len(), 1);

        // Another parameter of the same plugin is another cable; another modulator, of the lane or of the group, may drive the same one.
        ctl.apply(cable_to(lfo, osc, volume)).unwrap();
        let parent = lane(&ctl);
        add(&mut ctl, parent, ModulatorKind::Envelope).unwrap();
        let envelope = ctl.model.instruments[0].modulators[1].node;
        ctl.apply(cable_to(envelope, osc, detune)).unwrap();
        let group_lfo = ctl.model.groups[0].modulators[0].node;
        ctl.apply(cable_to(group_lfo, osc, detune)).unwrap();
        assert_eq!(ctl.model.instruments[0].modulators[0].targets.len(), 2);
        assert_eq!(ctl.model.instruments[0].modulators[1].targets.len(), 1);
        assert_eq!(ctl.model.groups[0].modulators[0].targets.len(), 1);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_cable_that_was_drawn_is_turned_and_taken_out_by_the_intents_that_name_a_cable() {
        let (engine, mut ctl) = reaching();
        let lfo = ctl.model.instruments[0].modulators[0].node;
        let osc = ctl.model.instruments[0].node;
        let (detune, _) = param(&ctl, NodePath::Instrument(0), "detune");
        ctl.apply(cable_to(lfo, osc, detune)).unwrap();
        let edge = ctl.model.instruments[0].modulators[0].targets[0].edge;
        sent(&engine);

        let applied = ctl.apply(Intent::SetEdgeDepth { edge, depth: -0.25 }).unwrap();
        assert_eq!(applied.events, [Event::EdgeDepthSet { edge, depth: -0.25 }]);
        assert_eq!(sent(&engine), ["SetModTargetDepth inst=0 mod=0 target=0 depth=-0.250"]);
        ctl.apply(Intent::RemoveEdge { edge }).unwrap();
        assert_eq!(sent(&engine), ["RemoveModTarget inst=0 mod=0 target=0", "SetParameter inst=0 slot=0 param=1 value=0.000"], "and the detune is the one the user set");
        assert!(ctl.model.instruments[0].modulators[0].targets.is_empty());
        // Drawn again, it is another cable.
        ctl.apply(cable_to(lfo, osc, detune)).unwrap();
        assert_ne!(ctl.model.instruments[0].modulators[0].targets[0].edge, edge);
    }

    #[test]
    fn remove_node_takes_a_lane_modulator_out_with_its_cables_and_says_the_structure_changed() {
        let (engine, mut ctl) = modulated();
        sent(&engine);
        let (first, envelope, learn, last) = (lane_mod(&ctl, 0), lane_mod(&ctl, 1), lane_mod(&ctl, 2), lane_mod(&ctl, 3));
        let applied = remove(&mut ctl, first).unwrap();
        assert_eq!(applied, Applied::restructured(1));
        assert!(applied.events.is_empty(), "nothing a client could apply: it is told by a snapshot");
        // (Its own cables, the cutoff's and the other LFO's rate, go with it; the audio thread drops it, and what they
        // drove has the values the user set: the cutoff, and the rate of what is the third modulator now.)
        assert_eq!(
            sent(&engine),
            ["RemoveModulator inst=0 index=0", "SetParameter inst=0 slot=1 param=0 value=1.000", "SetModulatorRate inst=0 mod=2 rate=1.000"]
        );
        let rack = &ctl.model.instruments[0].modulators;
        assert_eq!(rack.iter().map(|m| m.node).collect::<Vec<_>>(), [envelope, learn, last], "the others stay, in their order");
        assert!(rack.iter().all(|m| m.targets.is_empty()));
        assert!(ctl.model.locate(first).is_none(), "its node is gone");
        assert_eq!(ctl.model.locate(last), Some(NodePath::Modulator { at: ModulatorLocation::Lane(0), index: 2 }), "and the later ones are a place up");
        assert_eq!(ctl.model.groups[0].modulators.len(), 2, "the group's rack is untouched");
        assert!(ctl.dirty);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn remove_node_first_takes_out_the_cables_of_the_others_that_drive_the_modulator() {
        let (engine, mut ctl) = modulated();
        sent(&engine);
        // The first modulator's second cable drives the rate of the fourth.
        let (first, fourth) = (lane_mod(&ctl, 0), lane_mod(&ctl, 3));
        assert_eq!(ctl.model.instruments[0].modulators[0].targets.len(), 2);
        remove(&mut ctl, fourth).unwrap();
        assert_eq!(sent(&engine), ["RemoveModTarget inst=0 mod=0 target=1", "RemoveModulator inst=0 index=3"]);
        let rack = &ctl.model.instruments[0].modulators;
        assert_eq!(rack.len(), 3);
        assert_eq!(rack[0].node, first);
        assert_eq!(rack[0].targets.len(), 1, "the cable to the cutoff is as it was");
        assert_eq!(rack[0].targets[0].depth, 0.4);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn remove_node_takes_a_group_modulator_out_by_the_groups_command() {
        let (engine, mut ctl) = modulated();
        sent(&engine);
        let (lfo, envelope) = (group_mod(&ctl, 0), group_mod(&ctl, 1));
        remove(&mut ctl, lfo).unwrap();
        // (What its cable drove, the bus filter's cutoff, has the value the user set.)
        assert_eq!(sent(&engine), ["RemoveGroupModulator group=0 index=0", "SetGroupParameter group=0 fx=0 param=0 value=1.000"]);
        assert_eq!(ctl.model.groups[0].modulators.iter().map(|m| m.node).collect::<Vec<_>>(), [envelope]);
        assert_eq!(ctl.model.instruments[0].modulators.len(), 4, "the lane's rack is untouched");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn remove_node_leaves_the_nodes_it_has_not_been_asked_for_with_the_ids_they_had() {
        let (_engine, mut ctl) = modulated();
        let before: Vec<NodeId> = ctl.model.nodes().map(|(id, _)| id).collect();
        let gone = lane_mod(&ctl, 1);
        remove(&mut ctl, gone).unwrap();
        let after: Vec<NodeId> = ctl.model.nodes().map(|(id, _)| id).collect();
        assert_eq!(after, before.into_iter().filter(|id| *id != gone).collect::<Vec<_>>());
        // And its id is not handed out again.
        let parent = lane(&ctl);
        add(&mut ctl, parent, ModulatorKind::Lfo).unwrap();
        let added = ctl.model.instruments[0].modulators.last().unwrap().node;
        assert!(added != gone && ctl.model.nodes().all(|(id, _)| id != gone));
    }

    #[test]
    fn remove_node_refuses_a_lane_and_a_group_and_what_is_not_there_and_changes_nothing() {
        let (engine, mut ctl) = modulated();
        sent(&engine);
        let lane_node = lane(&ctl);
        let group = ctl.model.groups[0].node;
        let ghost = NodeId(9_999);
        let cases = [
            ("a lane", lane_node, invalid("only a modulator or an effect can be removed so far")),
            ("a group", group, invalid("only a modulator or an effect can be removed so far")),
            ("a node that is not there", ghost, IntentError::NotFound(ghost)),
            ("the master, which the model does not have", NodeId::MASTER, IntentError::NotFound(NodeId::MASTER)),
        ];
        for (what, node, error) in cases {
            assert_eq!(remove(&mut ctl, node), Err(error), "{what}");
        }
        assert!(sent(&engine).is_empty(), "nothing reached the audio thread");
        assert_eq!((ctl.rev, ctl.dirty), (0, false), "a refusal is not a revision");
        assert_eq!(ctl.model.instruments[0].modulators.len(), 4);
        assert_eq!(ctl.model.groups[0].modulators.len(), 2);
        assert_eq!(ctl.model.instruments[0].effects.len(), 1);
        assert_eq!(ctl.model.groups[0].effects.len(), 1);
        assert_eq!((ctl.model.instruments.len(), ctl.model.groups.len()), (1, 1));
    }

    /// `CHAINS` (cables of every kind to the effects of a lane and of a bus) over the stub engine.
    fn chains() -> (StubEngine, Controller) {
        let (handles, engine) = testutil::stub_engine();
        (engine, Controller::new(load(testutil::CHAINS).model, handles, None))
    }

    #[test]
    fn remove_node_takes_an_effect_out_of_a_lanes_chain_with_the_cables_to_it_and_says_the_structure_changed() {
        let (engine, mut ctl) = chains();
        sent(&engine);
        // Lane 0's filter, which two modulators drive, one by a cable whose depth another drives, and the group's.
        let filter = ctl.model.instruments[0].effects[0].node;
        let applied = remove(&mut ctl, filter).unwrap();
        assert_eq!(applied, Applied::restructured(1));
        assert!(applied.events.is_empty(), "nothing a client could apply: it is told by a snapshot");
        assert_eq!(
            sent(&engine),
            [
                "RemoveModTarget inst=0 mod=1 target=0",
                "RemoveModTarget inst=0 mod=0 target=0",
                "RemoveGroupModTarget group=0 mod=0 target=0",
                "RemoveEffect inst=0 index=0",
            ]
        );
        let chain: Vec<_> = ctl.model.instruments[0].effects.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(chain, ["Reverb"]);
        assert!(ctl.dirty);
        assert_eq!(ctl.model.locate(filter), None, "the node is gone");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn remove_node_takes_a_bus_effect_out_by_the_groups_command() {
        let (engine, mut ctl) = chains();
        sent(&engine);
        let bus = ctl.model.groups[0].effects[0].node;
        assert_eq!(remove(&mut ctl, bus), Ok(Applied::restructured(1)));
        assert_eq!(sent(&engine), ["RemoveGroupModTarget group=0 mod=0 target=3", "RemoveGroupEffect group=0 index=0"]);
        let buses: Vec<_> = ctl.model.groups[0].effects.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(buses, ["Reverb"]);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn the_audio_path_closes_up_where_an_effect_was_taken_out() {
        use crate::app::graph::EdgeKind;
        let (_engine, mut ctl) = chains();
        let (lane0, group) = (ctl.model.instruments[0].node, ctl.model.groups[0].node);
        let (filter, reverb) = (ctl.model.instruments[0].effects[0].node, ctl.model.instruments[0].effects[1].node);
        let path = |ctl: &Controller| -> Vec<(NodeId, NodeId, bool)> {
            ctl.model
                .graph()
                .edges
                .iter()
                .filter(|e| !matches!(e.kind, EdgeKind::Modulation { .. }) && [lane0, filter, reverb].contains(&e.from.node))
                .map(|e| (e.from.node, e.to.node, matches!(e.kind, EdgeKind::Member)))
                .collect()
        };
        assert_eq!(path(&ctl), [(lane0, filter, false), (filter, reverb, false), (reverb, group, true)]);
        // The one in the middle of the chain: the lane goes into the reverb.
        remove(&mut ctl, filter).unwrap();
        assert_eq!(path(&ctl), [(lane0, reverb, false), (reverb, group, true)]);
        // The last one: the lane goes into its group, by the link that a member's chain ends in.
        remove(&mut ctl, reverb).unwrap();
        assert_eq!(path(&ctl), [(lane0, group, true)]);
    }

    #[test]
    fn an_effect_that_is_added_and_taken_out_leaves_the_graph_as_it_was() {
        let (_engine, mut ctl) = chains();
        ctl.catalog.add(plugin::builtin::enumerate_plugins());
        let before = ctl.model.graph();
        let (lane0, group) = (ctl.model.instruments[0].node, ctl.model.groups[0].node);
        for parent in [lane0, group] {
            add_effect(&mut ctl, parent, "builtin:reverb").unwrap();
            let added = ctl.model.nodes().filter(|(id, _)| !before.nodes.iter().any(|n| n.id == *id)).map(|(id, _)| id).last().unwrap();
            assert_eq!(remove(&mut ctl, added), Ok(Applied::restructured(ctl.rev)));
            assert_eq!(ctl.model.graph(), before, "after adding a reverb to {parent:?} and taking it out");
        }
    }

    #[test]
    fn remove_node_does_not_take_an_effect_out_twice() {
        let (engine, mut ctl) = chains();
        let filter = ctl.model.instruments[0].effects[0].node;
        remove(&mut ctl, filter).unwrap();
        sent(&engine);
        assert_eq!(remove(&mut ctl, filter), Err(IntentError::NotFound(filter)));
        assert!(sent(&engine).is_empty());
        assert_eq!(ctl.rev, 1, "a refusal is not a revision");
    }

    fn controller_with_file(path: &std::path::Path) -> (StubEngine, Controller) {
        let (handles, engine) = testutil::stub_engine();
        (engine, Controller::new(load(SESSION).model, handles, Some(path.to_path_buf())))
    }

    #[test]
    fn save_writes_the_sessions_own_file_and_says_so_as_a_revision() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("song.toml");
        let (engine, mut ctl) = controller_with_file(&path);
        let osc = lane(&ctl);
        ctl.apply(Intent::SetGain { node: osc, gain: Gain::Volume, value: 0.25 }).unwrap();
        assert!(ctl.dirty);
        sent(&engine);
        let applied = ctl.apply(Intent::Save).unwrap();
        assert_eq!(applied, Applied::of(2, vec![Event::Saved { file: Some("song.toml".to_string()) }]));
        assert_eq!(ctl.rev, 2);
        assert!(!ctl.dirty, "nothing is unsaved now");
        assert!(sent(&engine).is_empty(), "the audio thread has nothing to do with it");
        // The file is the session as it is, volume and all.
        let text = std::fs::read_to_string(&path).unwrap();
        testutil::assert_same_model("the saved file", &ctl.model, &load(&text).model);
        assert!(text.contains("volume = 0.25"), "{text}");
    }

    #[test]
    fn save_that_cannot_be_done_says_why_and_is_no_revision_and_leaves_the_session_unsaved() {
        // No file to write to.
        let (engine, mut ctl) = controller();
        let osc = lane(&ctl);
        ctl.apply(Intent::SetGain { node: osc, gain: Gain::Volume, value: 0.25 }).unwrap();
        sent(&engine);
        match ctl.apply(Intent::Save) {
            Err(IntentError::Io(why)) => assert!(why.contains("path"), "{why}"),
            other => panic!("{other:?}"),
        }
        assert_eq!((ctl.rev, ctl.dirty), (1, true), "a refusal is not a revision, and it saved nothing");

        // A file where the directory should be: the write fails.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("blocker"), "").unwrap();
        let (engine, mut ctl) = controller_with_file(&dir.path().join("blocker").join("song.toml"));
        let osc = lane(&ctl);
        ctl.apply(Intent::SetGain { node: osc, gain: Gain::Volume, value: 0.25 }).unwrap();
        sent(&engine);
        assert!(matches!(ctl.apply(Intent::Save), Err(IntentError::Io(_))));
        assert_eq!((ctl.rev, ctl.dirty), (1, true));
    }

    #[test]
    fn remove_node_does_not_take_the_modulator_out_twice() {
        let (engine, mut ctl) = modulated();
        let node = lane_mod(&ctl, 2);
        remove(&mut ctl, node).unwrap();
        sent(&engine);
        assert_eq!(remove(&mut ctl, node), Err(IntentError::NotFound(node)));
        assert!(sent(&engine).is_empty());
        assert_eq!(ctl.rev, 1, "a refusal is not a revision");
    }

    #[test]
    fn add_modulator_refuses_a_parent_that_has_no_rack_and_changes_nothing() {
        let (engine, mut ctl) = controller();
        sent(&engine);
        let slots = ctl.model.slots.high_water();
        let effect = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        let bus = node(&ctl, NodePath::GroupEffect { group: 0, index: 0 });
        let modulator = node(&ctl, NodePath::Modulator { at: ModulatorLocation::Group(0), index: 0 });
        let ghost = NodeId(9_999);
        let cases = [
            ("an effect", effect, invalid("a modulator goes in a lane or in a group")),
            ("a bus effect", bus, invalid("a modulator goes in a lane or in a group")),
            ("a modulator", modulator, invalid("a modulator goes in a lane or in a group")),
            ("a node that is not there", ghost, IntentError::NotFound(ghost)),
            ("the master, which the model does not have", NodeId::MASTER, IntentError::NotFound(NodeId::MASTER)),
        ];
        for (what, parent, error) in cases {
            assert_eq!(add(&mut ctl, parent, ModulatorKind::Lfo), Err(error), "{what}");
        }
        assert!(sent(&engine).is_empty(), "nothing reached the audio thread");
        assert_eq!((ctl.rev, ctl.dirty), (0, false), "a refusal is not a revision");
        assert_eq!(ctl.model.slots.high_water(), slots, "no telemetry slot is used up for nothing");
        assert!(ctl.model.instruments[0].modulators.is_empty());
        assert_eq!(ctl.model.groups[0].modulators.len(), 1);
    }

    // -- AddEffect --

    /// `controller()`, with the built-in plugins in its catalog, as a scan finds them.
    fn controller_with_builtins() -> (StubEngine, Controller) {
        let (engine, mut ctl) = controller();
        ctl.catalog.add(plugin::builtin::enumerate_plugins());
        (engine, ctl)
    }

    fn add_effect(ctl: &mut Controller, parent: NodeId, plugin: &str) -> Result<Applied, IntentError> {
        ctl.apply(Intent::AddEffect { parent, plugin: plugin.to_string() })
    }

    #[test]
    fn add_effect_puts_an_effect_at_the_end_of_a_lanes_chain_in_the_model_and_the_engine() {
        let (engine, mut ctl) = controller_with_builtins();
        sent(&engine);
        let lane0 = lane(&ctl);
        let applied = add_effect(&mut ctl, lane0, "builtin:filter").unwrap();
        assert_eq!(applied, Applied::restructured(1));
        assert!(applied.events.is_empty(), "nothing a client could apply: it is told by a snapshot");
        assert_eq!(sent(&engine), ["InsertEffect inst=0 index=1 plugin=Filter mix=1.000"]);
        let chain: Vec<_> = ctl.model.instruments[0].effects.iter().map(|e| (e.name.as_str(), e.mix)).collect();
        // (The reverb the session has is at the mix its file gives it, which is fully wet when it gives none.)
        assert_eq!(chain, [("Reverb", 1.0), ("Filter", 1.0)]);
        assert!(ctl.dirty);
        ctl.model.check_ids().unwrap();
        // It is a node a client can name, in the lane, whose parameters the intents reach.
        let added = ctl.model.instruments[0].effects[1].node;
        assert_eq!(ctl.model.locate(added), Some(NodePath::Effect { inst: 0, index: 1 }));
        let (cutoff, _) = param(&ctl, NodePath::Effect { inst: 0, index: 1 }, "cutoff");
        ctl.apply(Intent::SetParam { node: added, param: cutoff, value: 0.25 }).unwrap();
        assert_eq!(sent(&engine), ["SetParameter inst=0 slot=2 param=0 value=0.250"]);
    }

    #[test]
    fn add_effect_to_a_group_goes_on_its_bus_by_the_groups_command() {
        let (engine, mut ctl) = controller_with_builtins();
        sent(&engine);
        let group = node(&ctl, NodePath::Group(0));
        assert_eq!(add_effect(&mut ctl, group, "builtin:reverb"), Ok(Applied::restructured(1)));
        assert_eq!(sent(&engine), ["InsertGroupEffect group=0 index=1 plugin=Reverb mix=0.500"]);
        let bus: Vec<_> = ctl.model.groups[0].effects.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(bus, ["Filter", "Reverb"]);
        assert_eq!(ctl.model.instruments[0].effects.len(), 1, "the member's chain is as it was");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn add_effect_to_a_lane_that_has_no_instrument_takes_one_too() {
        let (engine, mut ctl) = controller_with_builtins();
        sent(&engine);
        let empty = node(&ctl, NodePath::Instrument(1));
        assert!(ctl.model.instruments[1].instrument.is_none());
        add_effect(&mut ctl, empty, "builtin:filter").unwrap();
        assert_eq!(sent(&engine), ["InsertEffect inst=1 index=0 plugin=Filter mix=1.000"]);
    }

    #[test]
    fn the_graph_after_adding_an_effect_has_its_node_and_the_audio_path_through_it() {
        use crate::app::graph::EdgeKind;
        let (_engine, mut ctl) = controller_with_builtins();
        let (lane0, group, reverb) = (lane(&ctl), node(&ctl, NodePath::Group(0)), node(&ctl, NodePath::Effect { inst: 0, index: 0 }));
        add_effect(&mut ctl, lane0, "builtin:filter").unwrap();
        let graph = ctl.model.graph();
        let filter = ctl.model.instruments[0].effects[1].node;
        // The node is the lane's, after the effects the lane has and before its modulators.
        let strip: Vec<_> = graph.nodes.iter().filter(|n| n.parent == Some(lane0) || n.id == lane0).map(|n| (n.id, n.name.as_str())).collect();
        assert_eq!(strip, [(lane0, "Oscillator"), (reverb, "Reverb"), (filter, "Filter")]);
        // The audio path goes through it: the lane, the reverb, the filter, and then the group the lane is a member of.
        let path: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| !matches!(e.kind, EdgeKind::Modulation { .. }) && (e.from.node == lane0 || e.from.node == reverb || e.from.node == filter))
            .map(|e| (e.from.node, e.to.node, matches!(e.kind, EdgeKind::Member)))
            .collect();
        assert_eq!(path, [(lane0, reverb, false), (reverb, filter, false), (filter, group, true)]);
    }

    #[test]
    fn a_cable_can_be_drawn_to_an_effect_as_soon_as_it_has_been_added() {
        let (engine, mut ctl) = controller_with_builtins();
        let (group_lfo, lane0) = (ctl.model.groups[0].modulators[0].node, lane(&ctl));
        add_effect(&mut ctl, lane0, "builtin:filter").unwrap();
        sent(&engine);
        let filter = ctl.model.instruments[0].effects[1].node;
        let (cutoff, _) = param(&ctl, NodePath::Effect { inst: 0, index: 1 }, "cutoff");
        // The group's modulator reaches the plugins of its members: the new filter is the member's slot 2 (the instrument, the reverb).
        ctl.apply(cable_to(group_lfo, filter, cutoff)).unwrap();
        assert_eq!(
            sent(&engine),
            [command("AddGroupModTarget group=0 mod=0", &format!("GroupMember {{ member: 0, slot: 2, param_index: {cutoff} }}"), &ctl, NodePath::Effect { inst: 0, index: 1 }, "cutoff")]
        );
    }

    #[test]
    fn add_effect_refuses_what_it_cannot_do_and_changes_nothing() {
        let (engine, mut ctl) = controller_with_builtins();
        sent(&engine);
        let untouched = load(SESSION).model;
        let (lane0, reverb) = (lane(&ctl), node(&ctl, NodePath::Effect { inst: 0, index: 0 }));
        let bus = node(&ctl, NodePath::GroupEffect { group: 0, index: 0 });
        let modulator = node(&ctl, NodePath::Modulator { at: ModulatorLocation::Group(0), index: 0 });
        let ghost = NodeId(9_999);
        let not_a_chain = invalid("an effect goes in a lane or in a group");
        let cases = [
            ("an effect", reverb, "builtin:filter", not_a_chain.clone()),
            ("a bus effect", bus, "builtin:filter", not_a_chain.clone()),
            ("a modulator", modulator, "builtin:filter", not_a_chain),
            ("a node that is not there", ghost, "builtin:filter", IntentError::NotFound(ghost)),
            ("the master, which the model does not have", NodeId::MASTER, "builtin:filter", IntentError::NotFound(NodeId::MASTER)),
            ("an instrument", lane0, "builtin:osc", invalid("Oscillator is an instrument, not an effect")),
            ("a path", lane0, "./examples/plugin.clap", invalid("./examples/plugin.clap is not a plugin that is installed")),
            ("an id the catalog does not have", lane0, "clap:com.example.nothing", invalid("clap:com.example.nothing is not a plugin that is installed")),
        ];
        for (what, parent, plugin, error) in cases {
            assert_eq!(add_effect(&mut ctl, parent, plugin), Err(error), "{what}");
        }
        assert!(sent(&engine).is_empty(), "nothing reached the audio thread");
        assert_eq!((ctl.rev, ctl.dirty), (0, false), "a refusal is not a revision");
        testutil::assert_same_model("after the refusals", &ctl.model, &untouched);
    }

    /// What the web client's mock engine makes of an effect it is asked to add, for each built-in effect: the
    /// node the controller makes for it, without the ids (the mock hands out its own): the file
    /// `web/src/client/fixtures/builtins.effects.json`, which this test keeps current.
    /// `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` rewrites it.
    #[test]
    fn the_web_clients_fixture_of_the_builtin_effects_is_current() {
        let mut effects = serde_json::Map::new();
        for info in plugin::builtin::enumerate_plugins().into_iter().filter(|p| !p.is_instrument) {
            let (_engine, mut ctl) = controller_with_builtins();
            let lane0 = lane(&ctl);
            add_effect(&mut ctl, lane0, &info.id).unwrap();
            let added = ctl.model.instruments[0].effects.last().unwrap().node;
            let mut json = serde_json::to_value(ctl.model.graph().nodes.iter().find(|n| n.id == added).unwrap()).unwrap();
            let object = json.as_object_mut().unwrap();
            object.remove("id");
            object.remove("parent");
            effects.insert(info.id.clone(), json);
        }
        let fresh = serde_json::to_string_pretty(&effects).unwrap() + "\n";

        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/src/client/fixtures/builtins.effects.json");
        if std::env::var_os("TANG_UPDATE_TYPES").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &fresh).unwrap();
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            committed == fresh,
            "{} is not what the built-in effects are now: run `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` and commit the result",
            path.display()
        );
    }
}
