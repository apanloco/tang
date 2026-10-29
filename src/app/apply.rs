//! `Controller::apply`: carrying out an `Intent`.
//!
//! Each intent is resolved from its `NodeId` to a position in the model, checked,
//! applied to the model and sent to the audio thread, and answered with the event
//! that describes it. The checks come first: an intent that fails changes
//! nothing, and does not count as a revision.

use crate::app::controller::{Controller, FileMode, NewTarget, depth_command, envelope_command, key_command, macro_command, master_command, rate_command, steps_command, sync_command};
use crate::app::graph::{Endpoint, NoteRange, Port, SourceInfo};
use crate::app::intent::{Applied, Event, Gain, Intent, IntentError, ModulatorField, ModulatorKind, ParamValue, PatternField};
use crate::app::journal::{Inverse, Journal, RowBefore};
use crate::app::model::{
    BPM_MAX, BPM_MIN, CABLE_GAIN_MAX, DEPTH_MAX, DEPTH_MIN, EdgeId, FieldTarget, LinkOf, MAX_NAME, MAX_PINNED, ModSourceSlot, NEW_CABLE_DEPTH, NEW_CABLE_GAIN, NodeId, NodePath, ParamKind, ParamSlot,
    PatternState, PluginSlot, STEP_MAX, STEP_MIN, sync_beats,
};
use crate::piano_filter::PianoMode;
use crate::plugin::chain::{GraphCommand, LfoWaveform, MAX_STEPS, ModulatorLocation};
use crate::scale::ScaleSetting;
use crate::session::NodeUi;
use crate::session::{Feed, Route};
use crate::{plugin, session};

/// What an intent did, for `apply` to tell.
pub(super) enum Done {
    /// It changed a value: the event says which.
    Event(Event),
    /// It added or took out nodes or cables.
    Restructured,
}

/// Where the audio thread finds the plugin an intent concerns.
pub(super) enum PluginAt {
    /// A lane's instrument (slot 0) or one of its effects (slot 1..).
    Lane { inst: usize, slot: usize },
    /// One of a group's bus effects.
    Bus { group: usize, index: usize },
}

impl Controller {
    /// Carry out `intent`, and keep what puts it back in the history (what it is, `app::history`).
    pub fn apply(&mut self, intent: Intent) -> Result<Applied, IntentError> {
        // (Saving is the one intent that leaves the session with nothing unsaved.)
        match intent {
            Intent::Save => return self.save_intent(),
            Intent::SaveAs { name } => return self.save_as_intent(&name),
            // (What they do is carried out as any intent is, and is no step of the history.)
            Intent::Undo => return self.undo(),
            Intent::Redo => return self.redo(),
            _ => {}
        }
        let intent = self.link_or_cable(intent)?;
        let plan = self.plan_history(&intent);
        let applied = self.carry_out(intent)?;
        self.keep_in_history(plan);
        Ok(applied)
    }

    /// An audio cable asked for between the two ends of a link of the audio path that a lane or a group makes by itself (step 88, P5.0 of `docs/patch-graph.md` §11.6: the page draws the links as
    /// cables) is that link: given back if it is cut, as `SetRoute` or `SetFeed` give it back (so that it is a step of the history in their words), and refused if it is there, in the words of a
    /// cable between the two that is there already. A cable asked for at a level that is not the link's (a link has none: it is the plain level, 1), one whose ends the controller does not take
    /// (its own words say so) and one between the two of which there is a cable already are judged as cables, as they always were.
    fn link_or_cable(&self, intent: Intent) -> Result<Intent, IntentError> {
        let (from, to, gain) = match &intent {
            Intent::AddCable { from, to, gain } => (*from, *to, *gain),
            _ => return Ok(intent),
        };
        if gain.is_some_and(|gain| gain != NEW_CABLE_GAIN) {
            return Ok(intent);
        }
        let Ok((start, end)) = self.model.cable_ends(from, to) else { return Ok(intent) };
        if self.model.cables.iter().any(|cable| (cable.from, cable.to) == (start, end)) {
            return Ok(intent);
        }
        let Some(link) = self.model.template_link(start, end) else { return Ok(intent) };
        match (link.present, link.of) {
            (true, _) => Err(invalid("there is a cable between those two already")),
            (false, LinkOf::WayOut(node)) => Ok(Intent::SetRoute { node, route: Route::Out }),
            (false, LinkOf::Feed(node)) => Ok(Intent::SetFeed { node, feed: Feed::Chain }),
        }
    }

    /// Carry out `intent` and tell the subscribers, with nothing kept in the history: what `apply`, `undo` and `redo` are made of.
    /// The ids it hands out are written down (`made_last`): an add that is undone is done again under them.
    pub(super) fn carry_out(&mut self, intent: Intent) -> Result<Applied, IntentError> {
        self.model.ids.begin_log();
        // What it takes out is written down, as the ids it hands out are: a removal that is undone puts it back.
        self.journal = Some(Journal::default());
        let carried = self.carry_out_intent(intent);
        self.made_last = self.model.ids.take_log();
        self.taken_last = self.journal.take().unwrap_or_default();
        carried
    }

    fn carry_out_intent(&mut self, intent: Intent) -> Result<Applied, IntentError> {
        let done = match intent {
            Intent::SetParam { node, param, value } => Done::Event(self.set_param(node, param, value)?),
            Intent::SetGain { node, gain, value } => Done::Event(self.set_gain(node, gain, value)?),
            Intent::SetLimiter { on } => Done::Event(self.set_limiter(on)),
            Intent::LoadPreset { node, preset } => Done::Event(self.load_preset(node, &preset)?),
            Intent::SetModulatorField { node, field, value } => Done::Event(self.set_modulator_field(node, field, value)?),
            Intent::SetSteps { node, values } => Done::Event(self.set_steps(node, values)?),
            Intent::SetEdgeDepth { edge, depth } => Done::Event(self.set_edge_depth(edge, depth)?),
            Intent::SetCableGain { edge, gain } => Done::Event(self.set_cable_gain(edge, gain)?),
            Intent::SetRange { node, range } => Done::Event(self.set_range(node, range)?),
            Intent::SetMute { node, muted } => Done::Event(self.set_mute(node, muted)?),
            Intent::SetSolo { node, solo } => Done::Event(self.set_solo(node, solo)?),
            Intent::SetRoute { node, route } => self.set_route_intent(node, route)?,
            Intent::SetModulatorEar { node, ear } => self.set_modulator_ear_intent(node, ear)?,
            Intent::SetFeed { node, feed } => self.set_feed_intent(node, feed)?,
            Intent::SetScale { scale } => Done::Event(self.set_scale(&scale)?),
            Intent::SetLocked { locked } => Done::Event(self.set_locked(locked)),
            Intent::SetBpm { bpm } => Done::Event(self.set_bpm(bpm)?),
            Intent::SetRecording { node, recording } => Done::Event(self.set_recording(node, recording)?),
            Intent::ClearPattern { node } => Done::Event(self.clear_pattern(node)?),
            Intent::SetPatternField { node, field, value } => Done::Event(self.set_pattern_field(node, field, value)?),
            Intent::SetNodeUi { node, ui } => Done::Event(self.set_node_ui(node, ui)?),
            Intent::SetPinned { node, params } => Done::Event(self.set_pinned(node, params)?),
            Intent::RenameGroup { node, name } => Done::Event(self.rename_group(node, &name)?),
            Intent::AddGroup { members } => self.add_group_intent(&members)?,
            Intent::SetLaneGroup { node, group } => self.set_lane_group_intent(node, group)?,
            Intent::RemoveEdge { edge } => self.remove_edge_intent(edge)?,
            Intent::AddFreeModulator { kind, hears } => self.add_free_modulator_intent(kind, hears)?,
            Intent::AddModEdge { modulator, to } => self.add_mod_edge_intent(modulator, to)?,
            Intent::Modulate { to, kind, depth } => self.modulate_intent(to, kind, depth)?,
            Intent::AddCable { from, to, gain } => self.add_cable_intent(from, to, gain)?,
            Intent::AddSource { source } => self.add_source_intent(source)?,
            Intent::RemoveNode { node } => self.remove_node_intent(node)?,
            Intent::MoveEffect { node, index } => self.move_effect_intent(node, index)?,
            Intent::AddEffect { parent, plugin } => self.add_effect_intent(parent, &plugin)?,
            Intent::AddFreeEffect { plugin } => self.add_free_effect_intent(&plugin)?,
            Intent::AddInstrument { plugin } => self.add_instrument_intent(&plugin)?,
            Intent::ReplaceInstrument { node, plugin } => self.replace_instrument_intent(node, &plugin)?,
            Intent::ClearInstrument { node } => self.clear_instrument_intent(node)?,
            Intent::Save | Intent::SaveAs { .. } | Intent::Undo | Intent::Redo => unreachable!("done above"),
        };
        Ok(self.conclude(done))
    }

    /// What was done is told: the session has changes that are not saved and a revision more, the subscribers hear of it, and when it changed the
    /// structure they are given the new one as a snapshot, at once, whole.
    pub(super) fn conclude(&mut self, done: Done) -> Applied {
        self.dirty = true;
        self.rev += 1;
        let applied = match done {
            Done::Event(event) => Applied::of(self.rev, vec![event]),
            Done::Restructured => Applied::restructured(self.rev),
        };
        self.publish(&applied);
        if applied.restructured {
            // (The lanes that are there, and the cables of notes and the sources, are what a lane hears.)
            self.sync_lane_sources();
            #[cfg(any(test, feature = "web"))]
            self.resync();
            // (The nodes that are watched may be somewhere else, or gone, or back.)
            self.reconcile_probes();
        }
        applied
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

    /// Write the session as `name` beside its file, and have that file be its own from then on (`Intent::SaveAs`). A refusal changes nothing: the name is
    /// looked at first, and a file that is there is refused by the write that would make it (f128).
    fn save_as_intent(&mut self, name: &str) -> Result<Applied, IntentError> {
        let path = self.save_as_path(name)?;
        // The session's own file is written over (that is a save). Any other is only created, so that it is the write that finds a file there, a link that
        // leads nowhere included, and nothing that a look at the path had found can be out of date by the time of the write.
        let mode = if self.session_path.as_deref() == Some(path.as_path()) { FileMode::Replace } else { FileMode::CreateNew };
        self.save_to(&path, mode).map_err(|error| {
            if error.downcast_ref::<std::io::Error>().is_some_and(|error| error.kind() == std::io::ErrorKind::AlreadyExists) {
                let file = path.file_name().map(|file| file.to_string_lossy().into_owned()).unwrap_or_default();
                invalid(format!("a file called {file} is there already"))
            } else {
                IntentError::Io(error.to_string())
            }
        })?;
        self.session_path = Some(path);
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
        // (The output mix is a node of the graph and not of the model: `locate` has nothing for it.)
        if node == NodeId::MASTER {
            return self.set_master_gain(gain, value);
        }
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

    /// A gain of the output mix (step 102): its volume, or the ceiling of its limiter. Held to its range as every gain is, and the audio thread is told the whole of the master (`master_command`).
    fn set_master_gain(&mut self, gain: Gain, value: f32) -> Result<Event, IntentError> {
        if !value.is_finite() {
            return Err(invalid(format!("{value} is not a value for {gain:?}")));
        }
        let (min, max) = gain.range();
        let value = value.max(min).min(max);
        match gain {
            Gain::MasterVolume => self.model.master.volume = value,
            Gain::LimiterCeiling => self.model.master.ceiling = value,
            _ => return Err(invalid(format!("{gain:?} does not apply to that node"))),
        }
        self.tell_master();
        Ok(Event::GainSet { node: NodeId::MASTER, gain, value })
    }

    /// Put the limiter on the output mix in the sound, or take it out (step 102).
    fn set_limiter(&mut self, on: bool) -> Event {
        self.model.master.limiter = on;
        self.tell_master();
        Event::LimiterSet { on }
    }

    /// Tell the audio thread what the output mix is: its volume, whether its limiter is in the sound and its ceiling, which it is told whole whichever of them changed.
    pub(super) fn tell_master(&self) {
        let _ = self.cmd_tx.send(master_command(&self.model.master, self.sample_rate));
    }

    /// Where a front-end draws a node: in the model only (the session file keeps it; the audio thread has no use for it). The
    /// master is a node of the graph and not of the model, but has a place like the others.
    fn set_node_ui(&mut self, node: NodeId, ui: Option<NodeUi>) -> Result<Event, IntentError> {
        if node != NodeId::MASTER && self.model.locate(node).is_none() {
            return Err(IntentError::NotFound(node));
        }
        let ui = match ui {
            None => None,
            Some(place) => Some(place.tidy().ok_or_else(|| invalid(format!("({}, {}) is not a place", place.x, place.y)))?),
        };
        match ui {
            Some(ui) => {
                self.model.ui.insert(node, ui);
            }
            None => {
                self.model.ui.remove(&node);
            }
        }
        Ok(Event::NodeUiSet { node, ui })
    }

    /// Which parameters a front-end shows as knobs on a plugin's node: in the model only (the session file keeps them; the audio thread
    /// has no use for them). The list is looked at whole before anything is changed.
    fn set_pinned(&mut self, node: NodeId, params: Vec<u32>) -> Result<Event, IntentError> {
        let at = self.locate_plugin(node)?;
        let plugin = self.plugin_at(&at).ok_or_else(|| invalid("that instrument has no plugin"))?;
        if params.len() > MAX_PINNED {
            return Err(invalid(format!("at most {MAX_PINNED} parameters can be pinned to a node")));
        }
        for (i, param) in params.iter().enumerate() {
            let row = plugin.params.iter().find(|p| p.index == *param).ok_or_else(|| invalid(format!("the plugin has no parameter {param}")))?;
            if matches!(row.kind, ParamKind::Separator) {
                return Err(invalid(format!("{} is a separator, not a control", row.name)));
            }
            if params[..i].contains(param) {
                return Err(invalid(format!("parameter {param} is listed twice")));
            }
        }
        plugin.pinned = params.clone();
        Ok(Event::PinnedSet { node, params })
    }

    /// What a group is called: in the model only (the session file keeps it; the audio thread has no use for it). The event says the
    /// name the group has now, which is the default one for a name of nothing.
    fn rename_group(&mut self, node: NodeId, name: &str) -> Result<Event, IntentError> {
        let NodePath::Group(group) = self.model.locate(node).ok_or(IntentError::NotFound(node))? else {
            return Err(invalid("only a group has a name to change"));
        };
        let name = name.trim();
        if name.chars().count() > MAX_NAME {
            return Err(invalid(format!("a name is at most {MAX_NAME} characters")));
        }
        if name.chars().any(char::is_control) {
            return Err(invalid("a name has no control characters"));
        }
        self.model.groups[group].name = if name.is_empty() { None } else { Some(name.to_string()) };
        Ok(Event::GroupRenamed { node, name: self.model.groups[group].display_name(group) })
    }

    /// A group at the end, with the lanes in `members` put in it. Every member is looked at before anything is made.
    fn add_group_intent(&mut self, members: &[NodeId]) -> Result<Done, IntentError> {
        let lanes = members.iter().map(|node| self.lane_at(*node)).collect::<Result<Vec<usize>, IntentError>>()?;
        let group = self.add_group();
        for inst in lanes {
            self.set_lane_group(inst, Some(group));
        }
        Ok(Done::Restructured)
    }

    /// A lane put in a group, or taken out of its group.
    fn set_lane_group_intent(&mut self, node: NodeId, group: Option<NodeId>) -> Result<Done, IntentError> {
        let inst = self.lane_at(node)?;
        let group = match group {
            None => None,
            Some(group) => match self.model.locate(group).ok_or(IntentError::NotFound(group))? {
                NodePath::Group(index) => Some(index),
                _ => return Err(invalid("that node is not a group")),
            },
        };
        refuse_a_loop(self.model.loop_if_lane_goes_to(inst, group), "putting that lane in that group")?;
        self.set_lane_group(inst, group);
        Ok(Done::Restructured)
    }

    /// The place of the lane whose node is `node`: its instrument's, which a lane with none has too.
    fn lane_at(&self, node: NodeId) -> Result<usize, IntentError> {
        match self.model.locate(node).ok_or(IntentError::NotFound(node))? {
            NodePath::Instrument(inst) => Ok(inst),
            _ => Err(invalid("only a lane can be put in a group")),
        }
    }

    /// The keys of a lane: in the model and, as `SetInstrumentRange`, in the audio thread (which lets go of the notes it
    /// counted under the old range).
    fn set_range(&mut self, node: NodeId, range: Option<NoteRange>) -> Result<Event, IntentError> {
        let NodePath::Instrument(inst) = self.model.locate(node).ok_or(IntentError::NotFound(node))? else {
            return Err(invalid("a key range belongs to a lane"));
        };
        if let Some(NoteRange { low, high }) = range {
            for note in [low, high] {
                if note > 127 {
                    return Err(invalid(format!("{note} is not a note: the notes are 0 to 127")));
                }
            }
            if low > high {
                return Err(invalid(format!("the range goes from {} down to {}: its low note has to be the lower", crate::note_name(low), crate::note_name(high))));
            }
        }
        let range = range.map(|r| (r.low, r.high));
        self.model.instruments[inst].range = range;
        let _ = self.cmd_tx.send(GraphCommand::SetInstrumentRange { inst, range });
        Ok(Event::RangeSet { node, range: range.map(|(low, high)| NoteRange { low, high }) })
    }

    /// The lane or the group `node` is, for what a mute or a solo is of: a lane's place or a group's, and a refusal for any other node (the master is a node of
    /// the graph and not of the model: it is not found, as for every intent).
    fn lane_or_group(&self, node: NodeId, what: &str) -> Result<NodePath, IntentError> {
        match self.model.locate(node).ok_or(IntentError::NotFound(node))? {
            path @ (NodePath::Instrument(_) | NodePath::Group(_)) => Ok(path),
            _ => Err(invalid(format!("only a lane or a group can be {what}"))),
        }
    }

    /// Mute or unmute a lane or a group, in the model and in the audio thread (which lets it go, or brings it back, over a buffer).
    fn set_mute(&mut self, node: NodeId, muted: bool) -> Result<Event, IntentError> {
        match self.lane_or_group(node, "muted")? {
            NodePath::Instrument(inst) => {
                self.model.instruments[inst].muted = muted;
                let _ = self.cmd_tx.send(GraphCommand::SetMute { inst, muted });
            }
            NodePath::Group(group) => {
                self.model.groups[group].muted = muted;
                let _ = self.cmd_tx.send(GraphCommand::SetGroupMute { group, muted });
            }
            _ => unreachable!("`lane_or_group` says a lane or a group"),
        }
        Ok(Event::MuteSet { node, muted })
    }

    /// Solo or unsolo a lane or a group, in the model and in the audio thread.
    fn set_solo(&mut self, node: NodeId, solo: bool) -> Result<Event, IntentError> {
        match self.lane_or_group(node, "soloed")? {
            NodePath::Instrument(inst) => {
                self.model.instruments[inst].solo = solo;
                let _ = self.cmd_tx.send(GraphCommand::SetSolo { inst, solo });
            }
            NodePath::Group(group) => {
                self.model.groups[group].solo = solo;
                let _ = self.cmd_tx.send(GraphCommand::SetGroupSolo { group, solo });
            }
            _ => unreachable!("`lane_or_group` says a lane or a group"),
        }
        Ok(Event::SoloSet { node, solo })
    }

    /// Cut or give back the way out of a lane or a group, in the model and in the audio thread, which makes its plan again (`Intent::SetRoute`). A way out that is given back is
    /// refused where the audio would go round with the cables there are (the lane's end would feed its group's bus, which a cable may feed the lane from); cutting one makes no loop.
    fn set_route_intent(&mut self, node: NodeId, route: Route) -> Result<Done, IntentError> {
        let path = match self.model.locate(node).ok_or(IntentError::NotFound(node))? {
            path @ (NodePath::Instrument(_) | NodePath::Group(_)) => path,
            _ => return Err(invalid("only a lane or a group has a way out")),
        };
        if route == Route::Out {
            let what = if matches!(path, NodePath::Instrument(_)) { "giving that lane its way out" } else { "giving that group its way out" };
            refuse_a_loop(self.model.loop_if_route_is(path, route), what)?;
        }
        match path {
            NodePath::Instrument(inst) => {
                self.model.instruments[inst].route = route;
                let _ = self.cmd_tx.send(GraphCommand::SetLaneRoute { inst, route });
            }
            NodePath::Group(group) => {
                self.model.groups[group].route = route;
                let _ = self.cmd_tx.send(GraphCommand::SetGroupRoute { group, route });
            }
            _ => unreachable!("a lane or a group, found above"),
        }
        Ok(Done::Restructured)
    }

    /// Set whose notes a modulator of the session's own rack hears, in the model and in the audio thread (`Intent::SetModulatorEar`): the notes that a lane's instrument is given, those of
    /// the members of a group, or every event. Nothing else changes (the cables, the settings and the place of the modulator stay), and it is told as a snapshot, since the graph says whom
    /// a modulator hears.
    fn set_modulator_ear_intent(&mut self, node: NodeId, ear: Option<NodeId>) -> Result<Done, IntentError> {
        let NodePath::Modulator { at: at @ ModulatorLocation::Free, index } = self.model.locate(node).ok_or(IntentError::NotFound(node))? else {
            return Err(invalid("only a modulator has an ear"));
        };
        self.check_ear(ear)?;
        let slot = self.model.free_modulators.get(index).ok_or(IntentError::NotFound(node))?;
        // What it hears already (a lane that went hears every note, as far as anyone can tell) changes nothing and tells the audio thread nothing: an ear that names a lane that went is kept, so that
        // Undo of the lane's removal gives it back, and is not cleared by a request for what is so.
        if self.model.ear_of(slot) == ear {
            return Ok(Done::Restructured);
        }
        self.model.free_modulators[index].ear = ear;
        let _ = self.cmd_tx.send(GraphCommand::SetModulatorEar { location: at, index, ear });
        Ok(Done::Restructured)
    }

    /// Cut or give back the link into an effect from the stage before it in its chain, in the model and in the audio thread, which makes its plan again (`Intent::SetFeed`). A feed that
    /// is given back is refused where the audio would go round with the cables there are (a cable may have been drawn from the effect to a stage before it, which the link had kept
    /// from being drawn); cutting one makes no loop. The audio thread is told what the intent says, whether or not it is what the effect has.
    fn set_feed_intent(&mut self, node: NodeId, feed: Feed) -> Result<Done, IntentError> {
        let path = match self.model.locate(node).ok_or(IntentError::NotFound(node))? {
            path @ (NodePath::Effect { .. } | NodePath::GroupEffect { .. }) => path,
            _ => return Err(invalid("only an effect has a feed")),
        };
        if feed.is_fed() {
            refuse_a_loop(self.model.loop_if_feed_is(node, true), "giving that effect its input back")?;
        }
        let slot = match path {
            NodePath::Effect { inst, index } => &mut self.model.instruments[inst].effects[index],
            NodePath::GroupEffect { group, index } => &mut self.model.groups[group].effects[index],
            _ => unreachable!("an effect, found above"),
        };
        slot.fed = feed.is_fed();
        let _ = self.cmd_tx.send(GraphCommand::SetEffectFeed { node, fed: feed.is_fed() });
        Ok(Done::Restructured)
    }

    /// The scale of the piano, which the threads that send notes and the Piano tab read: it is nothing of the audio thread's.
    fn set_scale(&mut self, scale: &str) -> Result<Event, IntentError> {
        let setting = ScaleSetting::parse(scale)
            .ok_or_else(|| invalid(format!("{scale:?} is not a scale: a root and a kind of scale, like C major or F# dorian")))?;
        self.piano_filter.set_scale(setting);
        Ok(Event::ScaleSet { scale: setting.short(), root: setting.root, kind: setting.kind().to_string(), notes: setting.pitch_classes() })
    }

    /// Whether the piano drops the notes that are not in its scale.
    fn set_locked(&mut self, locked: bool) -> Event {
        self.piano_filter.set_mode(if locked { PianoMode::Locked } else { PianoMode::Highlight });
        Event::LockedSet { locked }
    }

    /// The tempo that patterns are recorded at: the model's, and the audio thread's (the next recording is counted in it). The patterns that have been
    /// recorded say the tempo they were recorded at, and keep it.
    fn set_bpm(&mut self, bpm: f32) -> Result<Event, IntentError> {
        if !bpm.is_finite() {
            return Err(invalid(format!("{bpm} is not a tempo")));
        }
        let bpm = bpm.clamp(BPM_MIN, BPM_MAX);
        self.model.bpm = bpm;
        let _ = self.cmd_tx.send(GraphCommand::SetGlobalBpm { bpm });
        Ok(Event::BpmSet { bpm })
    }

    /// The position of the lane `node`, whose pattern an intent is about.
    fn lane_of_a_pattern(&self, node: NodeId) -> Result<usize, IntentError> {
        match self.model.locate(node).ok_or(IntentError::NotFound(node))? {
            NodePath::Instrument(inst) => Ok(inst),
            _ => Err(invalid("a pattern belongs to a lane")),
        }
    }

    /// Start a recording on a lane, or stop the one that is going on (`Intent::SetRecording`). Starting is what the terminal's `r` does: a lane that has no pattern is
    /// given an empty one, the audio thread is told the tempo and the length first, since the count-in and the recording are counted in them, and then to record; a
    /// pattern that is there keeps its notes and settings until the audio thread reports the new one (`Controller::poll`). What is started while it is going on, or stopped
    /// while it is not, is told as it is, and nothing is sent: a second start would begin the count-in again.
    fn set_recording(&mut self, node: NodeId, recording: bool) -> Result<Event, IntentError> {
        let inst = self.lane_of_a_pattern(node)?;
        let bpm = self.model.bpm;
        let lane = &mut self.model.instruments[inst];
        let going = lane.pattern.as_ref().is_some_and(|pattern| pattern.recording);
        if recording && !going {
            let pattern = lane.pattern.get_or_insert_with(|| PatternState::empty(bpm));
            pattern.recording = true;
            let beats = pattern.length_beats;
            let _ = self.cmd_tx.send(GraphCommand::SetGlobalBpm { bpm });
            let _ = self.cmd_tx.send(GraphCommand::SetPatternLength { inst, beats });
            let _ = self.cmd_tx.send(GraphCommand::SetPatternRecording { inst, recording: true });
        } else if !recording && going {
            if let Some(pattern) = lane.pattern.as_mut() {
                pattern.recording = false;
            }
            let _ = self.cmd_tx.send(GraphCommand::SetPatternRecording { inst, recording: false });
        }
        Ok(Event::PatternSet { node, pattern: lane.pattern.as_ref().map(PatternState::info) })
    }

    /// Take the pattern of a lane out (`Intent::ClearPattern`): the audio thread forgets it, and a recording that was going on ends.
    fn clear_pattern(&mut self, node: NodeId) -> Result<Event, IntentError> {
        let inst = self.lane_of_a_pattern(node)?;
        if self.model.instruments[inst].pattern.take().is_none() {
            return Err(invalid("that lane has no pattern"));
        }
        let _ = self.cmd_tx.send(GraphCommand::ClearPattern { inst });
        Ok(Event::PatternSet { node, pattern: None })
    }

    /// A setting of the pattern of a lane (`Intent::SetPatternField`), held to its range, a switch rounded; a lane that has no pattern is given an empty one with it, so
    /// that the length of its first recording can be chosen. Each setting is told to the audio thread by its own command.
    fn set_pattern_field(&mut self, node: NodeId, field: PatternField, value: f32) -> Result<Event, IntentError> {
        if !value.is_finite() {
            return Err(invalid(format!("{value} is not a value for {field:?}")));
        }
        let inst = self.lane_of_a_pattern(node)?;
        let (min, max) = field.range();
        let mut value = value.clamp(min, max);
        if field.is_switch() {
            value = value.round();
        }
        let bpm = self.model.bpm;
        let pattern = self.model.instruments[inst].pattern.get_or_insert_with(|| PatternState::empty(bpm));
        let command = match field {
            PatternField::Length => {
                pattern.length_beats = value;
                GraphCommand::SetPatternLength { inst, beats: value }
            }
            PatternField::Enabled => {
                pattern.enabled = value != 0.0;
                GraphCommand::SetPatternEnabled { inst, enabled: pattern.enabled }
            }
            PatternField::Looping => {
                pattern.looping = value != 0.0;
                GraphCommand::SetPatternLooping { inst, looping: pattern.looping }
            }
            PatternField::Transpose => {
                pattern.in_key = value != 0.0;
                GraphCommand::SetPatternInKey { inst, in_key: pattern.in_key }
            }
        };
        let told = pattern.info();
        let _ = self.cmd_tx.send(command);
        Ok(Event::PatternSet { node, pattern: Some(told) })
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
        if field.is_whole() {
            value = value.round();
        }
        let modulator = self
            .model
            .modulators_mut(at)
            .and_then(|rack| rack.get_mut(index))
            .ok_or(IntentError::NotFound(node))?;
        let command = match (&mut modulator.source, field) {
            (ModSourceSlot::Lfo { rate, .. } | ModSourceSlot::Steps { rate, .. }, ModulatorField::Rate) => {
                *rate = value;
                rate_command(at, index, value)
            }
            // (A step sequencer's other settings go to the engine together with its steps, whichever of them was changed: the model has them all.)
            (
                ModSourceSlot::Steps { values, length, glide, retrigger, .. },
                ModulatorField::Glide | ModulatorField::Length | ModulatorField::Retrigger,
            ) => {
                match field {
                    ModulatorField::Glide => *glide = value,
                    ModulatorField::Length => *length = value as usize,
                    _ => *retrigger = value != 0.0,
                }
                steps_command(at, index, values, *length, *glide, *retrigger)
            }
            // (A note length to follow the tempo by goes to the engine as the beats it is, and its own rate stays where it was: the engine goes by the one or the other.)
            (ModSourceSlot::Lfo { sync, .. } | ModSourceSlot::Steps { sync, .. }, ModulatorField::Sync) => {
                *sync = value as usize;
                sync_command(at, index, sync_beats(*sync))
            }
            // (An LFO's key goes to the engine as the two together, whichever of them was changed: the model has both.)
            (ModSourceSlot::Lfo { retrigger, fade, .. }, ModulatorField::Retrigger | ModulatorField::Fade) => {
                match field {
                    ModulatorField::Fade => *fade = value,
                    _ => *retrigger = value != 0.0,
                }
                key_command(at, index, *retrigger, *fade)
            }
            (ModSourceSlot::Lfo { waveform, .. }, ModulatorField::Waveform) => {
                *waveform = LfoWaveform::ALL[value as usize];
                let waveform = *waveform;
                GraphCommand::SetModulatorWaveform { location: at, mod_index: index, waveform }
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
            // (A macro has nothing but its value, so the engine is given a macro of the new one in place of the old.)
            (ModSourceSlot::Macro { value: set }, ModulatorField::Value) => {
                *set = value;
                macro_command(at, index, value)
            }
            _ => return Err(invalid(format!("{field:?} does not apply to that modulator"))),
        };
        let _ = self.cmd_tx.send(command);
        Ok(Event::ModulatorFieldSet { node, field, value })
    }

    /// Give a step sequencer all of its steps (`Intent::SetSteps`): each held to `STEP_MIN` to `STEP_MAX`, and what is told is what it has.
    fn set_steps(&mut self, node: NodeId, values: Vec<f32>) -> Result<Event, IntentError> {
        let path = self.model.locate(node).ok_or(IntentError::NotFound(node))?;
        let NodePath::Modulator { at, index } = path else {
            return Err(invalid("that node is not a modulator"));
        };
        if values.len() != MAX_STEPS {
            return Err(invalid(format!("a step sequencer has {MAX_STEPS} steps, and {} were given", values.len())));
        }
        if let Some(bad) = values.iter().find(|value| !value.is_finite()) {
            return Err(invalid(format!("{bad} is not a value for a step")));
        }
        let modulator = self
            .model
            .modulators_mut(at)
            .and_then(|rack| rack.get_mut(index))
            .ok_or(IntentError::NotFound(node))?;
        let ModSourceSlot::Steps { values: steps, length, glide, retrigger, .. } = &mut modulator.source else {
            return Err(invalid("that modulator is not a step sequencer"));
        };
        for (step, value) in steps.iter_mut().zip(&values) {
            *step = value.clamp(STEP_MIN, STEP_MAX);
        }
        let told = steps.to_vec();
        let _ = self.cmd_tx.send(steps_command(at, index, steps, *length, *glide, *retrigger));
        Ok(Event::StepsSet { node, values: told })
    }

    fn set_edge_depth(&mut self, edge: EdgeId, depth: f32) -> Result<Event, IntentError> {
        if self.model.audio_cable(edge).is_some() {
            return Err(invalid("an audio cable has a level, and no depth"));
        }
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

    /// The level of an audio cable, in the model and in the audio thread (which goes to it over a buffer).
    fn set_cable_gain(&mut self, edge: EdgeId, gain: f32) -> Result<Event, IntentError> {
        if self.model.audio_cable(edge).is_none() {
            return Err(if self.model.locate_edge(edge).is_some() { invalid("a modulation cable has a depth, and no level") } else { IntentError::EdgeNotFound(edge) });
        }
        if !gain.is_finite() {
            return Err(invalid(format!("{gain} is not a value for the level of a cable")));
        }
        let gain = gain.clamp(0.0, CABLE_GAIN_MAX);
        if let Some(cable) = self.model.audio_cable_mut(edge) {
            cable.gain = gain;
        }
        let _ = self.cmd_tx.send(GraphCommand::SetCableGain { id: edge, gain });
        Ok(Event::CableGainSet { edge, gain })
    }

    /// An audio cable from the output `from` to the input `to` of the graph (`Intent::AddCable`): the ports say what the ends are (`SessionModel::cable_ends`), and
    /// everything that can be refused is looked at before anything is made.
    fn add_cable_intent(&mut self, from: Endpoint, to: Endpoint, gain: Option<f32>) -> Result<Done, IntentError> {
        // A cable that has a port of events at either end is a cable of notes, and is judged as one: from a source to a lane, and with no level.
        if from.port == Port::EventsOut || to.port == Port::EventsIn {
            let (source, lane) = self.model.event_cable_ends(from, to)?;
            if gain.is_some() {
                return Err(invalid("a cable of notes has no level: it carries notes, which are heard or not"));
            }
            self.add_event_cable(source, lane)?;
            return Ok(Done::Restructured);
        }
        let (start, end) = self.model.cable_ends(from, to)?;
        self.add_audio_cable(start, end, gain.unwrap_or(NEW_CABLE_GAIN))?;
        Ok(Done::Restructured)
    }

    /// The keys or a device put in the session as a source of notes (`Intent::AddSource`).
    fn add_source_intent(&mut self, source: SourceInfo) -> Result<Done, IntentError> {
        self.add_source(source.into())?;
        Ok(Done::Restructured)
    }

    fn remove_edge_intent(&mut self, edge: EdgeId) -> Result<Done, IntentError> {
        if self.remove_edge(edge) {
            Ok(Done::Restructured)
        } else {
            Err(IntentError::EdgeNotFound(edge))
        }
    }

    /// A modulator that is in no lane and no group (`Intent::AddFreeModulator`): it goes at the end of the session's own rack, hearing the lane or the group that `hears` names, or every note.
    fn add_free_modulator_intent(&mut self, kind: ModulatorKind, hears: Option<NodeId>) -> Result<Done, IntentError> {
        self.check_ear(hears)?;
        self.add_modulator_hearing(ModulatorLocation::Free, kind.source(), hears).expect("the session's own rack is always there");
        Ok(Done::Restructured)
    }

    /// Whether `ear` can be heard: none can (every note), and so can a lane and a group, and no other node.
    fn check_ear(&self, ear: Option<NodeId>) -> Result<(), IntentError> {
        let Some(heard) = ear else { return Ok(()) };
        match self.model.locate(heard).ok_or(IntentError::NotFound(heard))? {
            NodePath::Instrument(_) | NodePath::Group(_) => Ok(()),
            _ => Err(invalid("only a lane or a group can be heard")),
        }
    }

    fn add_mod_edge_intent(&mut self, modulator: NodeId, to: Endpoint) -> Result<Done, IntentError> {
        let NodePath::Modulator { at, index } = self.model.locate(modulator).ok_or(IntentError::NotFound(modulator))? else {
            return Err(invalid("a cable starts at a modulator"));
        };
        match to.port {
            Port::Param { index: param } => self.add_param_cable(at, index, to.node, param)?,
            Port::Volume | Port::Mix | Port::Pan => {
                let cable = self.gain_cable(at, index, to.node, to.port)?;
                self.add_target(at, index, cable);
            }
            Port::Rate | Port::Attack | Port::Decay | Port::Sustain | Port::Release | Port::Depth { .. } => self.add_field_cable(at, index, &to)?,
            Port::ModOut | Port::AudioIn | Port::AudioInSide | Port::AudioOut | Port::EventsIn | Port::EventsOut => {
                return Err(invalid("a cable can only end at a plugin's parameter, at a volume, a mix or a pan, or at a modulator's rate, stage or depth"));
            }
        }
        Ok(Done::Restructured)
    }

    /// A modulator of `kind` made for what `to` is, with a cable from it to `to`, as one thing (`Intent::Modulate`). Everything that can be refused is
    /// looked at before anything is made, as the cable of a modulator that is not there yet: the place it will have is the end of its rack, which is
    /// where the rules of reach are told the same of it as of any other (a modulator is never in reach of itself).
    fn modulate_intent(&mut self, to: Endpoint, kind: ModulatorKind, depth: Option<f32>) -> Result<Done, IntentError> {
        if let Some(depth) = depth {
            if !depth.is_finite() {
                return Err(invalid(format!("{depth} is not a value for the depth of a cable")));
            }
        }
        // The new modulator is the session's own and hears what its target is of: the lane of a plugin of a lane, the group of a bus effect, what a free modulator hears for a setting of it.
        let (at, ear) = match self.model.locate(to.node).ok_or(IntentError::NotFound(to.node))? {
            NodePath::Instrument(inst) | NodePath::Effect { inst, .. } => (ModulatorLocation::Free, Some(self.model.instruments[inst].node)),
            NodePath::GroupEffect { group, .. } => (ModulatorLocation::Free, Some(self.model.groups[group].node)),
            NodePath::Modulator { at: ModulatorLocation::Free, index } => (ModulatorLocation::Free, self.model.free_modulators.get(index).and_then(|slot| self.model.ear_of(slot))),
            // (A group has no plugin: what a cable can drive in it is its volume and its pan, and the modulator hears the group.)
            NodePath::Group(group) if to.port.gain().is_some() => (ModulatorLocation::Free, Some(self.model.groups[group].node)),
            NodePath::Group(_) | NodePath::Source(_) => return Err(invalid("a modulator drives a parameter of a plugin, a volume, a mix or a pan, or a setting of a modulator, and that is none of them")),
        };
        let index = self.model.modulators(at).map_or(0, |rack| rack.len());
        let cable = match to.port {
            Port::Param { index: param } => self.param_cable(at, index, to.node, param)?,
            Port::Volume | Port::Mix | Port::Pan => self.gain_cable(at, index, to.node, to.port)?,
            Port::Rate | Port::Attack | Port::Decay | Port::Sustain | Port::Release | Port::Depth { .. } => self.field_cable(at, index, &to)?,
            Port::ModOut | Port::AudioIn | Port::AudioInSide | Port::AudioOut | Port::EventsIn | Port::EventsOut => {
                return Err(invalid("a cable can only end at a plugin's parameter, at a volume, a mix or a pan, or at a modulator's rate, stage or depth"));
            }
        };
        let made = self.add_modulator_hearing(at, kind.source(), ear).ok_or(IntentError::NotFound(to.node))?;
        debug_assert_eq!(made, index, "a modulator goes at the end of its rack");
        self.add_target(at, index, cable);
        if let Some(depth) = depth {
            let edge = self.model.modulators(at).and_then(|rack| rack.get(index)).and_then(|modulator| modulator.targets.last()).map(|target| target.edge);
            self.set_edge_depth(edge.ok_or(IntentError::NotFound(to.node))?, depth)?;
        }
        Ok(Done::Restructured)
    }

    /// A cable from the modulator at `index` of the rack at `at` to the parameter `param` of the plugin in `node`.
    fn add_param_cable(&mut self, at: ModulatorLocation, index: usize, node: NodeId, param: u32) -> Result<(), IntentError> {
        let cable = self.param_cable(at, index, node, param)?;
        self.add_target(at, index, cable);
        Ok(())
    }

    /// What a cable from the modulator at `index` of the rack at `at` to the parameter `param` of the plugin in `node` is, if it can be drawn (the
    /// modulator need not be there: an `index` at the end of the rack is where a new one would go). Nothing is changed.
    fn param_cable(&mut self, at: ModulatorLocation, index: usize, node: NodeId, param: u32) -> Result<NewTarget, IntentError> {
        // The plugin has to be there, and in reach of the modulator (`param_targets`: the rule is the model's).
        let plugin = self.locate_plugin(node)?;
        if self.plugin_at(&plugin).is_none() {
            return Err(invalid("that instrument has no plugin"));
        }
        let reachable = self.model.param_targets(at);
        if !reachable.iter().any(|t| t.node == node) {
            return Err(invalid("a modulator of this session cannot reach that plugin"));
        }
        let target = reachable
            .into_iter()
            .find(|t| t.node == node && t.param == param)
            .ok_or_else(|| invalid(format!("the plugin has no parameter {param}")))?;
        let drives_it_already = self.model.modulators_mut(at).and_then(|rack| rack.get(index)).is_some_and(|m| m.targets.iter().any(|t| t.aim == target.aim()));
        if drives_it_already {
            return Err(invalid("that modulator drives that parameter already"));
        }
        Ok(NewTarget::to_param(&target, NEW_CABLE_DEPTH))
    }

    /// A cable from the modulator at `index` of the rack at `at` to a setting of another modulator (its rate, a stage of its
    /// envelope) or to the depth of one of that modulator's cables: which are in reach, and which a modulator has, is the model's
    /// (`modulators_in_reach`, `field_targets`). A modulator does not drive itself.
    fn add_field_cable(&mut self, at: ModulatorLocation, index: usize, to: &Endpoint) -> Result<(), IntentError> {
        let cable = self.field_cable(at, index, to)?;
        self.add_target(at, index, cable);
        Ok(())
    }

    /// What a cable from the modulator at `index` of the rack at `at` to the host-side value that `port` is (`Port::Volume`, `Mix` or `Pan`) of the node `node` is, if it can be drawn (the
    /// modulator need not be there, as for `param_cable`). The node has to be one that has such a value (`SessionModel::gain_targets`: the volume of a lane that has an instrument and of a
    /// group, the mix of an effect, the pan of a group). Nothing is changed.
    fn gain_cable(&mut self, at: ModulatorLocation, index: usize, node: NodeId, port: Port) -> Result<NewTarget, IntentError> {
        let gain = port.gain().ok_or_else(|| invalid("that is not a volume, a mix or a pan"))?;
        self.model.locate(node).ok_or(IntentError::NotFound(node))?;
        let target = self
            .model
            .gain_targets(at)
            .into_iter()
            .find(|t| t.node == node && t.gain == gain)
            .ok_or_else(|| invalid(format!("that node has no {}", gain.name())))?;
        let drives_it_already = self.model.modulators_mut(at).and_then(|rack| rack.get(index)).is_some_and(|m| m.targets.iter().any(|t| t.aim == target.aim()));
        if drives_it_already {
            return Err(invalid(format!("that modulator drives that {} already", gain.name())));
        }
        Ok(NewTarget::to_gain(&target, NEW_CABLE_DEPTH))
    }

    /// What a cable from the modulator at `index` of the rack at `at` to `to` (a setting of a modulator, or the depth of one of its cables) is, if it
    /// can be drawn (the modulator need not be there, as for `param_cable`). Nothing is changed.
    fn field_cable(&mut self, at: ModulatorLocation, index: usize, to: &Endpoint) -> Result<NewTarget, IntentError> {
        match self.model.locate(to.node).ok_or(IntentError::NotFound(to.node))? {
            NodePath::Modulator { at: there, index: place } if (there, place) == (at, index) => {
                return Err(invalid("a modulator cannot drive its own settings"));
            }
            NodePath::Modulator { .. } => {}
            _ => return Err(invalid("a cable ends at the settings of a modulator, and that is not one")),
        }
        if !self.model.modulators_in_reach(at, index).contains(&to.node) {
            return Err(invalid("a modulator of this session cannot reach that modulator"));
        }
        let Some(target) = self.model.field_targets(at, index).into_iter().find(|t| t.node == to.node && t.port() == to.port) else {
            return Err(match to.port {
                Port::Depth { edge } => IntentError::EdgeNotFound(edge),
                Port::Rate => invalid("that modulator has no rate"),
                Port::Attack => invalid("that modulator has no attack"),
                Port::Decay => invalid("that modulator has no decay"),
                Port::Sustain => invalid("that modulator has no sustain"),
                Port::Release => invalid("that modulator has no release"),
                Port::Param { .. } | Port::Volume | Port::Mix | Port::Pan | Port::ModOut | Port::AudioIn | Port::AudioInSide | Port::AudioOut | Port::EventsIn | Port::EventsOut => invalid("that is not a setting of a modulator"),
            });
        };
        let drives_it_already = self.model.modulators_mut(at).and_then(|rack| rack.get(index)).is_some_and(|m| m.targets.iter().any(|t| t.aim == target.aim()));
        if drives_it_already {
            return Err(invalid("that modulator drives that setting already"));
        }
        let aim = target.aim();
        let FieldTarget { label, base_value, param_min, param_max, .. } = target;
        Ok(NewTarget { aim, param_name: label, depth: NEW_CABLE_DEPTH, base_value, param_min, param_max })
    }

    fn add_effect_intent(&mut self, parent: NodeId, plugin: &str) -> Result<Done, IntentError> {
        match self.model.locate(parent).ok_or(IntentError::NotFound(parent))? {
            NodePath::Instrument(inst) => self.add_effect(inst, plugin)?,
            NodePath::Group(group) => self.add_bus_effect(group, plugin)?,
            _ => return Err(invalid("an effect goes in a lane or in a group")),
        };
        Ok(Done::Restructured)
    }

    fn add_free_effect_intent(&mut self, plugin: &str) -> Result<Done, IntentError> {
        self.add_free_effect(plugin)?;
        Ok(Done::Restructured)
    }

    fn add_instrument_intent(&mut self, plugin: &str) -> Result<Done, IntentError> {
        self.add_instrument_lane(plugin)?;
        Ok(Done::Restructured)
    }

    fn replace_instrument_intent(&mut self, node: NodeId, plugin: &str) -> Result<Done, IntentError> {
        match self.model.locate(node).ok_or(IntentError::NotFound(node))? {
            NodePath::Instrument(inst) => self.put_instrument(inst, plugin)?,
            _ => return Err(invalid("an instrument goes in a lane")),
        }
        Ok(Done::Restructured)
    }

    fn clear_instrument_intent(&mut self, node: NodeId) -> Result<Done, IntentError> {
        match self.model.locate(node).ok_or(IntentError::NotFound(node))? {
            NodePath::Instrument(inst) => {
                if !self.clear_instrument(inst) {
                    return Err(invalid("the lane has no instrument to take out"));
                }
            }
            _ => return Err(invalid("an instrument is taken out of a lane")),
        }
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
            // (A lane goes with everything it has and the cables of its group to it, a group with its bus and its modulators and every cable of
            // it, which the controller does: what the cables drove in a lane that stays has the value that was set. A session keeps one lane.)
            NodePath::Instrument(inst) => {
                if !self.remove_lane(inst) {
                    return Err(invalid("a session keeps its last lane"));
                }
            }
            NodePath::Group(group) => {
                self.remove_group(group);
            }
            // (A source goes with the cables of notes that start at it, which the controller does: the lanes that heard it hear what their other cables bring, or every source.)
            NodePath::Source(index) => {
                self.remove_source(index);
            }
        }
        Ok(Done::Restructured)
    }

    fn move_effect_intent(&mut self, node: NodeId, index: u32) -> Result<Done, IntentError> {
        match self.model.locate(node).ok_or(IntentError::NotFound(node))? {
            NodePath::Effect { inst, index: from } => {
                let to = place_to_move_to(from, self.model.instruments[inst].effects.len(), index)?;
                refuse_a_loop(self.model.loop_if_effect_goes_to(node, to), "moving that effect there")?;
                self.move_effect(inst, from, to);
            }
            NodePath::GroupEffect { group, index: from } => {
                let to = place_to_move_to(from, self.model.groups[group].effects.len(), index)?;
                refuse_a_loop(self.model.loop_if_effect_goes_to(node, to), "moving that effect there")?;
                self.move_bus_effect(group, from, to);
            }
            NodePath::Instrument(_) | NodePath::Group(_) | NodePath::Modulator { .. } | NodePath::Source(_) => return Err(invalid("only an effect can be moved")),
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
        // What the plugin has now, for undo to give back when an intent is carried out: the preset it has, and the values that the preset changes or that the user set over
        // the ones it had (a preset that is loaded again would put its own over them), each with what it is compared with when the file is saved.
        if self.journal.is_some() {
            let before = self.plugin_at(&at).map(|plugin| Inverse::Preset { node, preset: plugin.current_preset.clone(), rows: rows_a_preset_changes(&plugin.params, values.as_deref()) });
            if let Some(before) = before {
                self.took_out(before);
            }
        }
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
    pub(super) fn locate_plugin(&self, node: NodeId) -> Result<PluginAt, IntentError> {
        match self.model.locate(node).ok_or(IntentError::NotFound(node))? {
            NodePath::Instrument(inst) => Ok(PluginAt::Lane { inst, slot: 0 }),
            NodePath::Effect { inst, index } => Ok(PluginAt::Lane { inst, slot: index + 1 }),
            NodePath::GroupEffect { group, index } => Ok(PluginAt::Bus { group, index }),
            NodePath::Group(_) | NodePath::Modulator { .. } | NodePath::Source(_) => Err(invalid("that node has no plugin")),
        }
    }

    /// The model's slot for the plugin at `at`.
    pub(super) fn plugin_at(&mut self, at: &PluginAt) -> Option<&mut PluginSlot> {
        match *at {
            PluginAt::Lane { inst, slot: 0 } => self.model.instruments[inst].instrument.as_mut(),
            PluginAt::Lane { inst, slot } => self.model.instruments[inst].effects.get_mut(slot - 1),
            PluginAt::Bus { group, index } => self.model.groups[group].effects.get_mut(index),
        }
    }
}

/// The rows that a preset that is loaded changes, or that the user has set over what they had: the ones that undo gives back. `values` is what the plugin has for its parameters
/// once the preset is loaded (none if it could not be read, and then it is the user's values only). A row that has the value that the preset gives it changes only if it is the user's
/// (its baseline is another), and the user's rows are written down whatever the preset says of them, since the preset that is loaded again would put its own over them.
fn rows_a_preset_changes(rows: &[ParamSlot], values: Option<&[(u32, f32)]>) -> Vec<RowBefore> {
    rows.iter()
        .filter(|row| {
            let changed = values.and_then(|values| values.iter().find(|(index, _)| *index == row.index)).is_some_and(|&(_, value)| row.value != value);
            changed || row.value != row.default
        })
        .map(|row| RowBefore { index: row.index, value: row.value, default: row.default })
        .collect()
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

/// The place `index` in a chain of `places` effects, for the effect that is at `from` to be moved to: refused if the chain has
/// no such place, or if the effect is at it.
fn place_to_move_to(from: usize, places: usize, index: u32) -> Result<usize, IntentError> {
    let to = index as usize;
    if to >= places {
        return Err(invalid(format!("the chain has no place {index}")));
    }
    if to == from {
        return Err(invalid("that effect is at that place already"));
    }
    Ok(to)
}

fn invalid(why: impl Into<String>) -> IntentError {
    IntentError::Invalid(why.into())
}

/// Refused, in words, if `reason` says that the audio would go round with the cables there are when `what` is done (`SessionModel::loop_if_lane_goes_to`,
/// `loop_if_effect_goes_to`): the reason is the kernel's, which names its nodes and is for the log.
fn refuse_a_loop(reason: Option<String>, what: &str) -> Result<(), IntentError> {
    match reason {
        None => Ok(()),
        Some(reason) => {
            log::debug!("{what} would make the audio go round: {reason}");
            Err(invalid(format!("{what} would make the audio go round, by the cables there are")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The modulators of the session's own that hear the lane `lane`: what the lane's rack was before step 96.
    #[allow(dead_code)]
    fn hearing_lane(model: &crate::app::model::SessionModel, lane: usize) -> Vec<&crate::app::model::ModulatorSlot> {
        let node = model.instruments[lane].node;
        model.free_modulators.iter().filter(|m| m.ear == Some(node)).collect()
    }

    /// The modulators of the session's own that hear the group `group`: what the group's rack was before step 96.
    #[allow(dead_code)]
    fn hearing_group(model: &crate::app::model::SessionModel, group: usize) -> Vec<&crate::app::model::ModulatorSlot> {
        let node = model.groups[group].node;
        model.free_modulators.iter().filter(|m| m.ear == Some(node)).collect()
    }

    use crate::app::graph::{Graph, NodeKind};
    use crate::app::model::{Driven, MasterNode};
    use crate::app::testutil::{self, SENDS, SIDED, Sends, Sided, StubEngine, described, load};
    use crate::plugin::chain::{CableFrom, CableTo, ModTargetKind, ModulatorLocation};
    use crate::session::{Feed, NodeUi, Route};

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

    /// `controller()` with the modulators that its session has (the group's LFO, which is one of the session's own once the session is loaded) taken out of the model: a test of the session's own
    /// rack that counts from an empty one starts here.
    fn controller_without_modulators() -> (StubEngine, Controller) {
        let (handles, engine) = testutil::stub_engine();
        let mut model = load(SESSION).model;
        model.free_modulators.clear();
        (engine, Controller::new(model, handles, None))
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
        let (waveform, _) = param(&ctl, NodePath::Instrument(0), "waveform"); // 0 ..= 4
        for (asked, got) in [(1.4, 1.0), (1.6, 2.0), (2.5, 3.0), (3.6, 4.0), (99.0, 4.0), (-1.0, 0.0), (0.4, 0.0)] {
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
        let lfo = node(&ctl, NodePath::Modulator { at: ModulatorLocation::Free, index: 0 });
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

    /// What the audio thread was told of the output mix since the last look (the commands that `sent` leaves out).
    fn master_sent(engine: &StubEngine) -> Vec<String> {
        engine.cmd_rx.try_iter().filter(|command| matches!(command, GraphCommand::SetMaster { .. })).map(|command| testutil::describe(&command)).collect()
    }

    #[test]
    fn the_masters_volume_and_ceiling_are_gains_of_the_master_and_tell_the_audio_thread_the_whole_of_it() {
        let (engine, mut ctl) = controller();
        let applied = gain(&mut ctl, NodeId::MASTER, Gain::MasterVolume, 0.5).unwrap();
        assert_eq!(applied, Applied::of(1, vec![Event::GainSet { node: NodeId::MASTER, gain: Gain::MasterVolume, value: 0.5 }]));
        let applied = gain(&mut ctl, NodeId::MASTER, Gain::LimiterCeiling, -6.0).unwrap();
        assert_eq!(applied, Applied::of(2, vec![Event::GainSet { node: NodeId::MASTER, gain: Gain::LimiterCeiling, value: -6.0 }]));
        assert_eq!(ctl.model.master, MasterNode { volume: 0.5, limiter: true, ceiling: -6.0 });
        assert_eq!(master_sent(&engine), ["SetMaster volume=0.500 limiter=true ceiling=-1.0 rate=48000", "SetMaster volume=0.500 limiter=true ceiling=-6.0 rate=48000"]);
        assert!(ctl.dirty);
    }

    #[test]
    fn the_masters_gains_are_held_to_their_ranges_and_a_value_that_is_not_a_number_is_refused_with_nothing_changed() {
        let (engine, mut ctl) = controller();
        let cases = [
            (Gain::MasterVolume, [(9.0, 2.0), (-1.0, 0.0), (2.0, 2.0), (0.0, 0.0)]),
            (Gain::LimiterCeiling, [(5.0, 0.0), (-99.0, -24.0), (-24.0, -24.0), (0.0, 0.0)]),
        ];
        for (kind, values) in cases {
            for (asked, got) in values {
                let applied = gain(&mut ctl, NodeId::MASTER, kind, asked).unwrap();
                assert_eq!(applied.events, [Event::GainSet { node: NodeId::MASTER, gain: kind, value: got }], "{kind:?} {asked}");
            }
        }
        assert_eq!((Gain::MasterVolume.range(), Gain::LimiterCeiling.range()), ((0.0, 2.0), (-24.0, 0.0)));
        master_sent(&engine);
        let rev = ctl.rev;
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            for kind in [Gain::MasterVolume, Gain::LimiterCeiling] {
                assert!(matches!(gain(&mut ctl, NodeId::MASTER, kind, value), Err(IntentError::Invalid(_))), "{kind:?} {value}");
            }
        }
        assert_eq!((ctl.rev, ctl.model.master), (rev, MasterNode { volume: 0.0, limiter: true, ceiling: 0.0 }), "nothing changed, and no revision");
        assert!(master_sent(&engine).is_empty());
    }

    #[test]
    fn a_gain_that_is_not_the_masters_is_refused_for_the_master_and_the_masters_for_every_other_node() {
        let (engine, mut ctl) = controller();
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        let group = node(&ctl, NodePath::Group(0));
        let lfo = node(&ctl, NodePath::Modulator { at: ModulatorLocation::Free, index: 0 });
        let wrong = |g: Gain| IntentError::Invalid(format!("{g:?} does not apply to that node"));
        let mut cases = vec![];
        for kind in [Gain::Volume, Gain::Mix, Gain::GroupVolume, Gain::GroupPan] {
            cases.push((NodeId::MASTER, kind));
        }
        for node in [lane(&ctl), reverb, group, lfo] {
            for kind in [Gain::MasterVolume, Gain::LimiterCeiling] {
                cases.push((node, kind));
            }
        }
        let before = ctl.rev;
        for (node, kind) in cases {
            assert_eq!(gain(&mut ctl, node, kind, 0.5).unwrap_err(), wrong(kind), "{kind:?} of {node:?}");
        }
        assert_eq!(ctl.rev, before);
        assert!(sent(&engine).is_empty() && master_sent(&engine).is_empty());
        assert_eq!(ctl.model.master, MasterNode::default());
    }

    #[test]
    fn the_limiter_is_put_in_the_sound_and_taken_out_by_an_intent_that_tells_the_audio_thread_the_whole_master() {
        let (engine, mut ctl) = controller();
        assert!(ctl.model.master.limiter, "it is in the sound unless a session says it is not");
        let applied = ctl.apply(Intent::SetLimiter { on: false }).unwrap();
        assert_eq!(applied, Applied::of(1, vec![Event::LimiterSet { on: false }]));
        assert!(!ctl.model.master.limiter);
        // What is as it is asked to be is told as it is, and the audio thread is told again.
        let applied = ctl.apply(Intent::SetLimiter { on: false }).unwrap();
        assert_eq!(applied.events, [Event::LimiterSet { on: false }]);
        ctl.apply(Intent::SetLimiter { on: true }).unwrap();
        assert!(ctl.model.master.limiter);
        assert_eq!(
            master_sent(&engine),
            [
                "SetMaster volume=1.000 limiter=false ceiling=-1.0 rate=48000",
                "SetMaster volume=1.000 limiter=false ceiling=-1.0 rate=48000",
                "SetMaster volume=1.000 limiter=true ceiling=-1.0 rate=48000",
            ]
        );
        assert!(ctl.dirty);
    }

    #[test]
    fn the_masters_values_are_told_at_the_sample_rate_that_the_controller_has() {
        let (mut handles, engine) = testutil::stub_engine();
        handles.sample_rate = 44_100.0;
        let mut ctl = Controller::new(load(SESSION).model, handles, None);
        ctl.apply(Intent::SetLimiter { on: false }).unwrap();
        assert_eq!(master_sent(&engine), ["SetMaster volume=1.000 limiter=false ceiling=-1.0 rate=44100"]);
    }

    #[test]
    fn set_gain_refuses_a_gain_that_does_not_apply_to_the_node() {
        let (engine, mut ctl) = controller();
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        let bus = node(&ctl, NodePath::GroupEffect { group: 0, index: 0 });
        let group = node(&ctl, NodePath::Group(0));
        let lfo = node(&ctl, NodePath::Modulator { at: ModulatorLocation::Free, index: 0 });
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

    fn range(ctl: &mut Controller, node: NodeId, low: u8, high: u8) -> Result<Applied, IntentError> {
        ctl.apply(Intent::SetRange { node, range: Some(NoteRange { low, high }) })
    }

    #[test]
    fn set_range_sets_a_lanes_key_range_in_the_model_and_the_engine_and_none_is_every_note() {
        let (engine, mut ctl) = controller();
        let instrument = lane(&ctl);
        let applied = range(&mut ctl, instrument, 40, 90).unwrap();
        assert_eq!(applied, Applied::of(1, vec![Event::RangeSet { node: instrument, range: Some(NoteRange { low: 40, high: 90 }) }]));
        assert_eq!(ctl.model.instruments[0].range, Some((40, 90)));
        assert_eq!(sent(&engine), ["SetInstrumentRange inst=0 range=Some((40, 90))"]);
        assert!(ctl.dirty);

        let applied = ctl.apply(Intent::SetRange { node: instrument, range: None }).unwrap();
        assert_eq!(applied, Applied::of(2, vec![Event::RangeSet { node: instrument, range: None }]));
        assert_eq!(ctl.model.instruments[0].range, None);
        assert_eq!(sent(&engine), ["SetInstrumentRange inst=0 range=None"]);
    }

    #[test]
    fn set_range_takes_a_single_note_and_the_whole_keyboard() {
        let (engine, mut ctl) = controller();
        let instrument = lane(&ctl);
        range(&mut ctl, instrument, 60, 60).unwrap();
        range(&mut ctl, instrument, 0, 127).unwrap();
        assert_eq!(sent(&engine), ["SetInstrumentRange inst=0 range=Some((60, 60))", "SetInstrumentRange inst=0 range=Some((0, 127))"]);
        assert_eq!(ctl.model.instruments[0].range, Some((0, 127)));
    }

    #[test]
    fn set_range_is_for_a_lane_that_has_no_instrument_too_and_changes_only_the_lane_it_names() {
        let (engine, mut ctl) = controller();
        let empty = node(&ctl, NodePath::Instrument(1));
        range(&mut ctl, empty, 0, 59).unwrap();
        assert_eq!(sent(&engine), ["SetInstrumentRange inst=1 range=Some((0, 59))"]);
        assert_eq!((ctl.model.instruments[0].range, ctl.model.instruments[1].range), (None, Some((0, 59))));
    }

    #[test]
    fn set_range_refuses_what_is_not_a_range_of_notes_and_what_is_not_a_lane() {
        let (engine, mut ctl) = controller();
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        let bus = node(&ctl, NodePath::GroupEffect { group: 0, index: 0 });
        let group = node(&ctl, NodePath::Group(0));
        let lfo = node(&ctl, NodePath::Modulator { at: ModulatorLocation::Free, index: 0 });
        let ghost = NodeId(9_999);
        for node in [reverb, bus, group, lfo] {
            assert_eq!(range(&mut ctl, node, 40, 90), Err(invalid("a key range belongs to a lane")), "{node:?}");
        }
        assert_eq!(range(&mut ctl, ghost, 40, 90), Err(IntentError::NotFound(ghost)));

        let instrument = lane(&ctl);
        assert_eq!(range(&mut ctl, instrument, 90, 40), Err(invalid("the range goes from F#6 down to E2: its low note has to be the lower")));
        assert_eq!(range(&mut ctl, instrument, 40, 128), Err(invalid("128 is not a note: the notes are 0 to 127")));
        assert_eq!(range(&mut ctl, instrument, 200, 255), Err(invalid("200 is not a note: the notes are 0 to 127")));

        assert_eq!(sent(&engine), Vec::<String>::new());
        assert_eq!((ctl.rev, ctl.dirty), (0, false));
        assert_eq!(ctl.model.instruments[0].range, None);
    }

    #[test]
    fn a_range_that_was_set_is_what_a_session_saves() {
        let (_engine, mut ctl) = controller();
        let instrument = lane(&ctl);
        range(&mut ctl, instrument, 40, 90).unwrap();
        let saved = |ctl: &Controller| crate::session::to_toml(&crate::app::save::to_save(&ctl.model).0, &[], &[], &Default::default(), None, None, None).unwrap();
        assert!(saved(&ctl).contains("range = \"E2-F#6\""), "{}", saved(&ctl));
        ctl.apply(Intent::SetRange { node: instrument, range: None }).unwrap();
        assert!(!saved(&ctl).contains("range ="), "{}", saved(&ctl));
    }

    #[test]
    fn set_mute_mutes_a_lane_and_a_group_in_the_model_and_the_engine_and_tells_it() {
        let (engine, mut ctl) = controller();
        let (instrument, empty, group) = (lane(&ctl), node(&ctl, NodePath::Instrument(1)), node(&ctl, NodePath::Group(0)));
        let applied = ctl.apply(Intent::SetMute { node: instrument, muted: true }).unwrap();
        assert_eq!(applied, Applied::of(1, vec![Event::MuteSet { node: instrument, muted: true }]));
        assert_eq!((ctl.model.instruments[0].muted, ctl.model.instruments[0].solo), (true, false));
        assert_eq!(sent(&engine), ["SetMute inst=0 muted=true"]);
        assert!(ctl.dirty);

        let applied = ctl.apply(Intent::SetMute { node: group, muted: true }).unwrap();
        assert_eq!(applied, Applied::of(2, vec![Event::MuteSet { node: group, muted: true }]));
        assert_eq!((ctl.model.groups[0].muted, ctl.model.groups[0].solo), (true, false));
        assert_eq!(sent(&engine), ["SetGroupMute group=0 muted=true"]);

        // A lane with no instrument can be muted, and it is that lane alone.
        ctl.apply(Intent::SetMute { node: empty, muted: true }).unwrap();
        assert_eq!(sent(&engine), ["SetMute inst=1 muted=true"]);
        assert_eq!(ctl.model.instruments.iter().map(|lane| lane.muted).collect::<Vec<_>>(), [true, true]);

        ctl.apply(Intent::SetMute { node: instrument, muted: false }).unwrap();
        ctl.apply(Intent::SetMute { node: group, muted: false }).unwrap();
        assert_eq!(sent(&engine), ["SetMute inst=0 muted=false", "SetGroupMute group=0 muted=false"]);
        assert_eq!((ctl.model.instruments[0].muted, ctl.model.groups[0].muted), (false, false));
    }

    #[test]
    fn set_solo_solos_a_lane_and_a_group_in_the_model_and_the_engine_and_tells_it() {
        let (engine, mut ctl) = controller();
        let (instrument, group) = (lane(&ctl), node(&ctl, NodePath::Group(0)));
        let applied = ctl.apply(Intent::SetSolo { node: instrument, solo: true }).unwrap();
        assert_eq!(applied, Applied::of(1, vec![Event::SoloSet { node: instrument, solo: true }]));
        assert_eq!((ctl.model.instruments[0].muted, ctl.model.instruments[0].solo), (false, true));
        assert_eq!(sent(&engine), ["SetSolo inst=0 solo=true"]);
        assert!(ctl.dirty);

        let applied = ctl.apply(Intent::SetSolo { node: group, solo: true }).unwrap();
        assert_eq!(applied, Applied::of(2, vec![Event::SoloSet { node: group, solo: true }]));
        assert_eq!((ctl.model.groups[0].muted, ctl.model.groups[0].solo), (false, true));
        assert_eq!(sent(&engine), ["SetGroupSolo group=0 solo=true"]);

        ctl.apply(Intent::SetSolo { node: instrument, solo: false }).unwrap();
        ctl.apply(Intent::SetSolo { node: group, solo: false }).unwrap();
        assert_eq!(sent(&engine), ["SetSolo inst=0 solo=false", "SetGroupSolo group=0 solo=false"]);
        assert_eq!((ctl.model.instruments[0].solo, ctl.model.groups[0].solo), (false, false));
    }

    #[test]
    fn set_mute_and_set_solo_refuse_what_is_neither_a_lane_nor_a_group() {
        let (engine, mut ctl) = controller();
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        let bus = node(&ctl, NodePath::GroupEffect { group: 0, index: 0 });
        let lfo = node(&ctl, NodePath::Modulator { at: ModulatorLocation::Free, index: 0 });
        let ghost = NodeId(9_999);
        for node in [reverb, bus, lfo] {
            assert_eq!(ctl.apply(Intent::SetMute { node, muted: true }), Err(invalid("only a lane or a group can be muted")), "{node:?}");
            assert_eq!(ctl.apply(Intent::SetSolo { node, solo: true }), Err(invalid("only a lane or a group can be soloed")), "{node:?}");
        }
        // (The master is a node of the graph and not of the model, so it is not found, as it is for every intent.)
        for node in [ghost, NodeId::MASTER] {
            assert_eq!(ctl.apply(Intent::SetMute { node, muted: true }), Err(IntentError::NotFound(node)));
            assert_eq!(ctl.apply(Intent::SetSolo { node, solo: true }), Err(IntentError::NotFound(node)));
        }
        assert_eq!(sent(&engine), Vec::<String>::new());
        assert_eq!((ctl.rev, ctl.dirty), (0, false));
    }

    #[test]
    fn what_is_muted_or_soloed_is_what_a_session_saves() {
        let (_engine, mut ctl) = controller();
        let (instrument, group) = (lane(&ctl), node(&ctl, NodePath::Group(0)));
        ctl.apply(Intent::SetMute { node: instrument, muted: true }).unwrap();
        ctl.apply(Intent::SetSolo { node: group, solo: true }).unwrap();
        let saved = |ctl: &Controller| {
            let (instruments, groups, cables, sources) = crate::app::save::to_save(&ctl.model);
            crate::session::to_toml(&instruments, &groups, &cables, &sources, None, None, None).unwrap()
        };
        let text = saved(&ctl);
        assert_eq!((text.matches("mute = true").count(), text.matches("solo = true").count()), (1, 1), "{text}");
        ctl.apply(Intent::SetMute { node: instrument, muted: false }).unwrap();
        ctl.apply(Intent::SetSolo { node: group, solo: false }).unwrap();
        let text = saved(&ctl);
        assert!(!text.contains("mute") && !text.contains("solo"), "{text}");
    }

    #[test]
    fn set_route_cuts_the_way_out_of_a_lane_and_a_group_in_the_model_and_the_engine_and_says_the_structure_changed() {
        let (engine, mut ctl) = controller();
        let (instrument, empty, group) = (lane(&ctl), node(&ctl, NodePath::Instrument(1)), node(&ctl, NodePath::Group(0)));
        let applied = ctl.apply(Intent::SetRoute { node: instrument, route: Route::None }).unwrap();
        assert_eq!(applied, Applied::restructured(1));
        assert!(applied.events.is_empty(), "nothing a client could apply: the links of the graph change, so it is told by a snapshot");
        assert_eq!(ctl.model.instruments.iter().map(|lane| lane.route).collect::<Vec<_>>(), [Route::None, Route::Out]);
        assert_eq!(sent(&engine), ["SetLaneRoute inst=0 route=none"]);
        assert!(ctl.dirty);

        let applied = ctl.apply(Intent::SetRoute { node: group, route: Route::None }).unwrap();
        assert_eq!(applied, Applied::restructured(2));
        assert_eq!(ctl.model.groups[0].route, Route::None);
        assert_eq!(sent(&engine), ["SetGroupRoute group=0 route=none"]);

        // A lane with no instrument has a way out, and it is that lane's alone.
        ctl.apply(Intent::SetRoute { node: empty, route: Route::None }).unwrap();
        assert_eq!(sent(&engine), ["SetLaneRoute inst=1 route=none"]);

        ctl.apply(Intent::SetRoute { node: instrument, route: Route::Out }).unwrap();
        ctl.apply(Intent::SetRoute { node: group, route: Route::Out }).unwrap();
        assert_eq!(sent(&engine), ["SetLaneRoute inst=0 route=out", "SetGroupRoute group=0 route=out"]);
        assert_eq!((ctl.model.instruments[0].route, ctl.model.groups[0].route), (Route::Out, Route::Out));
    }

    #[test]
    fn set_route_refuses_what_is_neither_a_lane_nor_a_group() {
        let (engine, mut ctl) = controller();
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        let bus = node(&ctl, NodePath::GroupEffect { group: 0, index: 0 });
        let lfo = node(&ctl, NodePath::Modulator { at: ModulatorLocation::Free, index: 0 });
        for node in [reverb, bus, lfo] {
            assert_eq!(ctl.apply(Intent::SetRoute { node, route: Route::None }), Err(invalid("only a lane or a group has a way out")), "{node:?}");
        }
        // (The master is a node of the graph and not of the model, so it is not found, as it is for every intent.)
        for node in [NodeId(9_999), NodeId::MASTER] {
            assert_eq!(ctl.apply(Intent::SetRoute { node, route: Route::None }), Err(IntentError::NotFound(node)));
        }
        assert_eq!(sent(&engine), Vec::<String>::new());
        assert_eq!((ctl.rev, ctl.dirty), (0, false));
    }

    #[test]
    fn a_way_out_is_not_given_back_when_the_audio_would_go_round_and_nothing_changes() {
        let (engine, mut ctl, n) = sends();
        // The group's output goes to the drive of lane 1, which is in no group. With the lane's way out cut it can be put in the group: it feeds nothing.
        draw(&mut ctl, out(n.group), into(n.fx[2]), None).unwrap();
        ctl.apply(Intent::SetRoute { node: n.lane[1], route: Route::None }).unwrap();
        ctl.apply(Intent::SetLaneGroup { node: n.lane[1], group: Some(n.group) }).unwrap();
        sent(&engine);
        let rev = ctl.rev;
        // Given back, it would feed the group that feeds it.
        let error = ctl.apply(Intent::SetRoute { node: n.lane[1], route: Route::Out }).unwrap_err();
        assert!(error.to_string().contains("go round"), "{error}");
        assert!(sent(&engine).is_empty());
        assert_eq!((ctl.rev, ctl.model.instruments[1].route), (rev, Route::None));
        // A lane whose way out may be given back is given it, and cutting one is not looked at.
        ctl.apply(Intent::SetRoute { node: n.lane[0], route: Route::None }).unwrap();
        ctl.apply(Intent::SetRoute { node: n.lane[0], route: Route::Out }).unwrap();
        assert_eq!(ctl.model.instruments[0].route, Route::Out);
    }

    #[test]
    fn a_cable_is_not_drawn_that_would_make_the_audio_go_round_unless_the_way_out_that_it_goes_round_by_is_cut() {
        let (engine, mut ctl, n) = sends();
        // Lane 0 is in the group: the group's output into lane 0's delay is a loop (the lane feeds the group, which feeds the lane).
        let error = draw(&mut ctl, out(n.group), into(n.fx[1]), None).unwrap_err();
        assert!(error.to_string().contains("go round"), "{error}");
        ctl.apply(Intent::SetRoute { node: n.lane[0], route: Route::None }).unwrap();
        sent(&engine);
        draw(&mut ctl, out(n.group), into(n.fx[1]), None).unwrap();
        assert_eq!(sent(&engine).len(), 1);
        assert_eq!(ctl.model.cables.len(), 1);
    }

    #[test]
    fn a_group_is_given_back_its_way_out_whatever_the_ways_out_of_the_lanes_are() {
        let (_engine, mut ctl, n) = sends();
        // Lane 0 is in the group and its way out is cut, so that the group's output can go into its delay: given back, the lane would make a loop. The group's way out has nothing to do with it
        // (and a lane that has the group's place among the lanes is not the group).
        ctl.apply(Intent::SetRoute { node: n.lane[0], route: Route::None }).unwrap();
        draw(&mut ctl, out(n.group), into(n.fx[1]), None).unwrap();
        ctl.apply(Intent::SetRoute { node: n.group, route: Route::None }).unwrap();
        ctl.apply(Intent::SetRoute { node: n.group, route: Route::Out }).unwrap();
        assert_eq!((ctl.model.groups[0].route, ctl.model.instruments[0].route), (Route::Out, Route::None));
        // The lane's own is still refused.
        let error = ctl.apply(Intent::SetRoute { node: n.lane[0], route: Route::Out }).unwrap_err();
        assert!(error.to_string().contains("go round"), "{error}");
    }

    #[test]
    fn what_has_its_way_out_cut_is_what_a_session_saves() {
        let (_engine, mut ctl) = controller();
        let (instrument, group) = (lane(&ctl), node(&ctl, NodePath::Group(0)));
        ctl.apply(Intent::SetRoute { node: instrument, route: Route::None }).unwrap();
        ctl.apply(Intent::SetRoute { node: group, route: Route::None }).unwrap();
        let saved = |ctl: &Controller| {
            let (instruments, groups, cables, sources) = crate::app::save::to_save(&ctl.model);
            crate::session::to_toml(&instruments, &groups, &cables, &sources, None, None, None).unwrap()
        };
        let text = saved(&ctl);
        assert_eq!(text.matches("route = \"none\"").count(), 2, "{text}");
        let again = load(&text).model;
        assert_eq!((again.instruments[0].route, again.groups[0].route), (Route::None, Route::None));
        ctl.apply(Intent::SetRoute { node: instrument, route: Route::Out }).unwrap();
        ctl.apply(Intent::SetRoute { node: group, route: Route::Out }).unwrap();
        assert!(!saved(&ctl).contains("route"));
    }

    #[test]
    fn set_feed_cuts_the_link_into_an_effect_and_a_bus_effect_in_the_model_and_the_engine_and_says_the_structure_changed() {
        let (engine, mut ctl, n) = sends();
        let applied = ctl.apply(Intent::SetFeed { node: n.fx[0], feed: Feed::None }).unwrap();
        assert_eq!(applied, Applied::restructured(1));
        assert!(applied.events.is_empty(), "nothing a client could apply: the links of the graph change, so it is told by a snapshot");
        let fed = |ctl: &Controller| (ctl.model.instruments[0].effects.iter().map(|fx| fx.fed).collect::<Vec<_>>(), ctl.model.instruments[1].effects[0].fed, ctl.model.groups[0].effects[0].fed);
        assert_eq!(fed(&ctl), (vec![false, true], true, true));
        assert_eq!(sent(&engine), [format!("SetEffectFeed node={} fed=false", n.fx[0].0)]);
        assert!(ctl.dirty);

        let applied = ctl.apply(Intent::SetFeed { node: n.bus, feed: Feed::None }).unwrap();
        assert_eq!(applied, Applied::restructured(2));
        assert_eq!(fed(&ctl), (vec![false, true], true, false));
        assert_eq!(sent(&engine), [format!("SetEffectFeed node={} fed=false", n.bus.0)]);

        // The last effect of a lane that has none before it in another lane is its own: the drive of lane 1 is fed by lane 1's instrument.
        ctl.apply(Intent::SetFeed { node: n.fx[2], feed: Feed::None }).unwrap();
        assert_eq!(fed(&ctl), (vec![false, true], false, false));
        sent(&engine);

        ctl.apply(Intent::SetFeed { node: n.fx[0], feed: Feed::Chain }).unwrap();
        ctl.apply(Intent::SetFeed { node: n.bus, feed: Feed::Chain }).unwrap();
        assert_eq!(sent(&engine), [format!("SetEffectFeed node={} fed=true", n.fx[0].0), format!("SetEffectFeed node={} fed=true", n.bus.0)]);
        assert_eq!(fed(&ctl), (vec![true, true], false, true));
    }

    #[test]
    fn set_feed_refuses_what_is_not_an_effect() {
        let (engine, mut ctl, n) = sends();
        for node in [n.lane[0], n.lane[1], n.group] {
            assert_eq!(ctl.apply(Intent::SetFeed { node, feed: Feed::None }), Err(invalid("only an effect has a feed")), "{node:?}");
        }
        // (The master is a node of the graph and not of the model, so it is not found, as it is for every intent.)
        for node in [NodeId(9_999), NodeId::MASTER] {
            assert_eq!(ctl.apply(Intent::SetFeed { node, feed: Feed::None }), Err(IntentError::NotFound(node)));
        }
        assert_eq!(sent(&engine), Vec::<String>::new());
        assert_eq!((ctl.rev, ctl.dirty), (0, false));
    }

    #[test]
    fn a_feed_is_not_given_back_when_the_audio_would_go_round_and_nothing_changes() {
        let (engine, mut ctl, n) = sends();
        // Lane 0's delay sends back to its filter, which is before it: a loop through the link between them, so that the link is cut first.
        assert!(draw(&mut ctl, out(n.fx[1]), into(n.fx[0]), None).unwrap_err().to_string().contains("go round"));
        ctl.apply(Intent::SetFeed { node: n.fx[1], feed: Feed::None }).unwrap();
        draw(&mut ctl, out(n.fx[1]), into(n.fx[0]), None).unwrap();
        sent(&engine);
        let rev = ctl.rev;
        // Given back, the link would close the loop.
        let error = ctl.apply(Intent::SetFeed { node: n.fx[1], feed: Feed::Chain }).unwrap_err();
        assert!(error.to_string().contains("go round"), "{error}");
        assert!(sent(&engine).is_empty());
        assert_eq!((ctl.rev, ctl.model.instruments[0].effects[1].fed), (rev, false));
        // Cutting a link is not looked at, and a link that no cable goes round is given back.
        ctl.apply(Intent::SetFeed { node: n.fx[0], feed: Feed::None }).unwrap();
        ctl.apply(Intent::SetFeed { node: n.fx[0], feed: Feed::Chain }).unwrap();
        assert!(ctl.model.instruments[0].effects[0].fed);
    }

    #[test]
    fn the_link_into_a_bus_effect_is_not_given_back_where_the_audio_would_go_round_either() {
        let (engine, mut ctl, n) = sends();
        // Lane 0 is in the group: what the bus effect puts out, sent into lane 0's delay, goes round through the link into the bus effect.
        assert!(draw(&mut ctl, out(n.bus), into(n.fx[1]), None).unwrap_err().to_string().contains("go round"));
        ctl.apply(Intent::SetFeed { node: n.bus, feed: Feed::None }).unwrap();
        draw(&mut ctl, out(n.bus), into(n.fx[1]), None).unwrap();
        sent(&engine);
        let rev = ctl.rev;
        let error = ctl.apply(Intent::SetFeed { node: n.bus, feed: Feed::Chain }).unwrap_err();
        assert!(error.to_string().contains("go round"), "{error}");
        assert!(sent(&engine).is_empty());
        assert_eq!((ctl.rev, ctl.model.groups[0].effects[0].fed), (rev, false));
    }

    #[test]
    fn a_cable_that_would_make_the_audio_go_round_through_a_chain_link_is_drawn_once_the_link_is_cut() {
        let (engine, mut ctl, n) = sends();
        assert!(draw(&mut ctl, out(n.fx[1]), into(n.fx[0]), None).is_err());
        ctl.apply(Intent::SetFeed { node: n.fx[1], feed: Feed::None }).unwrap();
        sent(&engine);
        draw(&mut ctl, out(n.fx[1]), into(n.fx[0]), None).unwrap();
        assert_eq!(ctl.model.cables.len(), 1);
        assert_eq!(sent(&engine).len(), 1);
    }

    #[test]
    fn a_feed_that_is_set_to_what_it_is_changes_nothing_that_is_heard() {
        let (engine, mut ctl, n) = sends();
        ctl.apply(Intent::SetFeed { node: n.fx[0], feed: Feed::Chain }).unwrap();
        assert!(ctl.model.instruments[0].effects[0].fed);
        assert_eq!(sent(&engine), [format!("SetEffectFeed node={} fed=true", n.fx[0].0)], "(the audio thread takes it as nothing to change)");
    }

    #[test]
    fn what_has_its_feed_cut_is_what_a_session_saves() {
        let (_engine, mut ctl, n) = sends();
        ctl.apply(Intent::SetFeed { node: n.fx[1], feed: Feed::None }).unwrap();
        ctl.apply(Intent::SetFeed { node: n.bus, feed: Feed::None }).unwrap();
        let saved = |ctl: &Controller| {
            let (instruments, groups, cables, sources) = crate::app::save::to_save(&ctl.model);
            crate::session::to_toml(&instruments, &groups, &cables, &sources, None, None, None).unwrap()
        };
        let text = saved(&ctl);
        assert_eq!(text.matches("feed = \"none\"").count(), 2, "{text}");
        let again = load(&text).model;
        assert_eq!(again.instruments[0].effects.iter().map(|fx| fx.fed).collect::<Vec<_>>(), [true, false]);
        assert_eq!((again.instruments[1].effects[0].fed, again.groups[0].effects[0].fed), (true, false));
        ctl.apply(Intent::SetFeed { node: n.fx[1], feed: Feed::Chain }).unwrap();
        ctl.apply(Intent::SetFeed { node: n.bus, feed: Feed::Chain }).unwrap();
        assert!(!saved(&ctl).contains("feed"));
    }

    fn place(ctl: &mut Controller, node: NodeId, x: f32, y: f32) -> Result<Applied, IntentError> {
        ctl.apply(Intent::SetNodeUi { node, ui: Some(NodeUi { x, y }) })
    }

    #[test]
    fn set_node_ui_keeps_where_a_node_is_drawn_and_tells_it_and_the_engine_hears_nothing_of_it() {
        let (engine, mut ctl) = controller();
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        let applied = place(&mut ctl, reverb, 376.0, -40.0).unwrap();
        assert_eq!(applied, Applied::of(1, vec![Event::NodeUiSet { node: reverb, ui: Some(NodeUi { x: 376.0, y: -40.0 }) }]));
        assert_eq!(ctl.model.ui.get(&reverb), Some(&NodeUi { x: 376.0, y: -40.0 }));
        assert_eq!(sent(&engine), Vec::<String>::new(), "the audio thread has no use for where a node is drawn");
        assert!(ctl.dirty, "the place is in the file when the session is saved");
        // It is moved by saying another.
        place(&mut ctl, reverb, 0.0, 0.0).unwrap();
        assert_eq!(ctl.model.ui.get(&reverb), Some(&NodeUi { x: 0.0, y: 0.0 }));
        assert_eq!(ctl.rev, 2);
    }

    #[test]
    fn set_node_ui_is_for_every_kind_of_node_the_master_too() {
        let (_engine, mut ctl) = controller();
        let nodes = [
            lane(&ctl),
            node(&ctl, NodePath::Instrument(1)),
            node(&ctl, NodePath::Effect { inst: 0, index: 0 }),
            node(&ctl, NodePath::Group(0)),
            node(&ctl, NodePath::GroupEffect { group: 0, index: 0 }),
            node(&ctl, NodePath::Modulator { at: ModulatorLocation::Free, index: 0 }),
            NodeId::MASTER,
        ];
        for (i, node) in nodes.into_iter().enumerate() {
            let at = NodeUi { x: i as f32 * 10.0, y: -(i as f32) };
            let applied = place(&mut ctl, node, at.x, at.y).unwrap_or_else(|e| panic!("{node:?}: {e}"));
            assert_eq!(applied.events, [Event::NodeUiSet { node, ui: Some(at) }]);
            assert_eq!(ctl.model.ui.get(&node), Some(&at), "{node:?}");
        }
        assert_eq!(ctl.model.ui.len(), 7);
        assert_eq!(ctl.model.master_ui(), Some(NodeUi { x: 60.0, y: -6.0 }));
    }

    #[test]
    fn set_node_ui_with_no_place_gives_the_node_back_to_the_layout() {
        let (_engine, mut ctl) = controller();
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        place(&mut ctl, reverb, 5.0, 6.0).unwrap();
        place(&mut ctl, NodeId::MASTER, 7.0, 8.0).unwrap();
        let applied = ctl.apply(Intent::SetNodeUi { node: reverb, ui: None }).unwrap();
        assert_eq!(applied, Applied::of(3, vec![Event::NodeUiSet { node: reverb, ui: None }]));
        assert_eq!(ctl.model.ui.get(&reverb), None);
        let applied = ctl.apply(Intent::SetNodeUi { node: NodeId::MASTER, ui: None }).unwrap();
        assert_eq!(applied.events, [Event::NodeUiSet { node: NodeId::MASTER, ui: None }]);
        assert!(ctl.model.ui.is_empty());
        // (A node with no place has no place to give back, and it is told all the same: the page may be behind.)
        assert!(ctl.apply(Intent::SetNodeUi { node: reverb, ui: None }).is_ok());
    }

    #[test]
    fn set_node_ui_rounds_a_place_to_a_tenth_and_keeps_it_on_the_canvas() {
        let (_engine, mut ctl) = controller();
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        let told = |applied: Applied| match applied.events.as_slice() {
            [Event::NodeUiSet { ui: Some(ui), .. }] => (ui.x, ui.y),
            other => panic!("{other:?}"),
        };
        // (To a tenth: a file is read by people, and a drag on a screen is not that exact. Away from zero at the half.)
        assert_eq!(told(place(&mut ctl, reverb, 376.382, -40.04).unwrap()), (376.4, -40.0));
        assert_eq!(told(place(&mut ctl, reverb, 0.25, -0.25).unwrap()), (0.3, -0.3));
        assert_eq!(told(place(&mut ctl, reverb, 1.0e9, -1.0e9).unwrap()), (100_000.0, -100_000.0));
        assert_eq!(told(place(&mut ctl, reverb, f32::MAX, f32::MIN).unwrap()), (100_000.0, -100_000.0));
        // (No `-0`: it is written to the file and to the wire as it is.)
        let (x, y) = told(place(&mut ctl, reverb, -0.04, -0.0).unwrap());
        assert!(x.is_sign_positive() && y.is_sign_positive(), "{x} {y}");
        assert_eq!(ctl.model.ui.get(&reverb), Some(&NodeUi { x: 0.0, y: 0.0 }));
    }

    #[test]
    fn set_node_ui_refuses_a_node_that_is_not_there_and_a_place_that_is_not_one() {
        let (engine, mut ctl) = controller();
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        let ghost = NodeId(9_999);
        assert_eq!(place(&mut ctl, ghost, 1.0, 2.0), Err(IntentError::NotFound(ghost)));
        assert_eq!(ctl.apply(Intent::SetNodeUi { node: ghost, ui: None }), Err(IntentError::NotFound(ghost)));
        for (x, y) in [(f32::NAN, 1.0), (1.0, f32::NAN), (f32::INFINITY, 1.0), (1.0, f32::NEG_INFINITY)] {
            assert_eq!(place(&mut ctl, reverb, x, y), Err(invalid(format!("({x}, {y}) is not a place"))), "{x} {y}");
        }
        assert_eq!(sent(&engine), Vec::<String>::new());
        assert_eq!((ctl.rev, ctl.dirty), (0, false));
        assert!(ctl.model.ui.is_empty());
    }

    #[test]
    fn a_place_that_was_set_is_what_a_session_saves_and_is_read_back_where_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("song.toml");
        let (handles, _engine) = testutil::stub_engine();
        let mut ctl = Controller::new(load(SESSION).model, handles, Some(path.clone()));
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        place(&mut ctl, reverb, 376.4, -40.0).unwrap();
        place(&mut ctl, NodeId::MASTER, 1200.0, 300.0).unwrap();
        ctl.apply(Intent::Save).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("ui = { x = 376.4, y = -40 }"), "{text}");
        assert!(text.contains("[ui]\nmaster = { x = 1200, y = 300 }"), "{text}");
        let again = load(&text).model;
        assert_eq!(again.ui.get(&again.instruments[0].effects[0].node), Some(&NodeUi { x: 376.4, y: -40.0 }));
        assert_eq!(again.master_ui(), Some(NodeUi { x: 1200.0, y: 300.0 }));
        // And a place that is given back is not in the file.
        ctl.apply(Intent::SetNodeUi { node: reverb, ui: None }).unwrap();
        ctl.apply(Intent::Save).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("y = -40"), "{text}");
    }

    fn scale_set(scale: &str, root: u8, kind: &str, notes: &[u8]) -> Event {
        Event::ScaleSet { scale: scale.to_string(), root, kind: kind.to_string(), notes: notes.to_vec() }
    }

    #[test]
    fn set_scale_sets_the_scale_of_the_piano_and_tells_what_a_client_draws_of_it() {
        let (engine, mut ctl) = controller();
        let applied = ctl.apply(Intent::SetScale { scale: "F# dorian".to_string() }).unwrap();
        assert_eq!(applied, Applied::of(1, vec![scale_set("F# dorian", 6, "dorian", &[1, 3, 4, 6, 8, 9, 11])]));
        assert_eq!(ctl.piano_filter.scale().short(), "F# dorian");
        assert!(ctl.dirty, "the scale is saved with the session");
        assert_eq!(sent(&engine), Vec::<String>::new(), "the audio thread has no part in it: the notes are sorted out where they are sent");
    }

    #[test]
    fn set_scale_takes_the_names_a_session_file_does_and_tells_the_form_it_saves() {
        let (_engine, mut ctl) = controller();
        let cases = [
            ("Bb Natural Minor", "A# minor", 10, "minor", vec![0, 1, 3, 5, 6, 8, 10]),
            ("db MAJOR", "C# major", 1, "major", vec![0, 1, 3, 5, 6, 8, 10]),
            ("  e  blues ", "E blues", 4, "blues", vec![2, 4, 7, 9, 10, 11]),
            ("G# maj pent", "G# maj pent", 8, "maj pent", vec![0, 3, 5, 8, 10]),
        ];
        for (asked, scale, root, kind, notes) in cases {
            let applied = ctl.apply(Intent::SetScale { scale: asked.to_string() }).unwrap();
            assert_eq!(applied.events, [scale_set(scale, root, kind, &notes)], "{asked}");
            assert_eq!(ctl.piano_filter.scale().short(), scale, "{asked}");
        }
    }

    #[test]
    fn set_scale_refuses_what_is_not_a_scale_and_changes_nothing() {
        let (engine, mut ctl) = controller();
        for text in ["", "banana", "C", "C wibble", "H major", "major", "Cé", "日本語"] {
            assert_eq!(
                ctl.apply(Intent::SetScale { scale: text.to_string() }),
                Err(invalid(format!("{text:?} is not a scale: a root and a kind of scale, like C major or F# dorian"))),
                "{text:?}"
            );
        }
        assert_eq!(sent(&engine), Vec::<String>::new());
        assert_eq!((ctl.rev, ctl.dirty), (0, false), "a refusal is not a revision");
        assert_eq!(ctl.piano_filter.scale().short(), "C major");
    }

    #[test]
    fn set_locked_locks_the_piano_to_its_scale_or_lets_every_note_through() {
        use crate::piano_filter::PianoMode;
        let (engine, mut ctl) = controller();
        // C major to begin with, and not locked: C#4 (61) is off the scale, and goes through.
        assert_eq!(ctl.piano_filter.mode(), PianoMode::Highlight);
        assert!(!ctl.piano_filter.block_note_on(61));

        let applied = ctl.apply(Intent::SetLocked { locked: true }).unwrap();
        assert_eq!(applied, Applied::of(1, vec![Event::LockedSet { locked: true }]));
        assert_eq!(ctl.piano_filter.mode(), PianoMode::Locked);
        assert!(ctl.piano_filter.block_note_on(61), "locked, a note that is not on the scale is dropped where it is sent");
        assert!(!ctl.piano_filter.block_note_on(60), "and one that is on it is not");
        assert!(ctl.dirty, "the lock is saved with the session");

        // What it is locked to is the scale that is set: C# major has C# (61), and not D (62).
        ctl.apply(Intent::SetScale { scale: "C# major".to_string() }).unwrap();
        assert!(!ctl.piano_filter.block_note_on(61));
        assert!(ctl.piano_filter.block_note_on(62));

        let applied = ctl.apply(Intent::SetLocked { locked: false }).unwrap();
        assert_eq!(applied, Applied::of(3, vec![Event::LockedSet { locked: false }]));
        assert_eq!(ctl.piano_filter.mode(), PianoMode::Highlight);
        assert!(!ctl.piano_filter.block_note_on(60));
        assert_eq!(sent(&engine), Vec::<String>::new());
    }

    #[test]
    fn a_lock_that_is_set_again_is_a_revision_and_tells_what_it_is() {
        let (_engine, mut ctl) = controller();
        for revision in 1..=2 {
            let applied = ctl.apply(Intent::SetLocked { locked: true }).unwrap();
            assert_eq!(applied, Applied::of(revision, vec![Event::LockedSet { locked: true }]));
        }
    }

    #[test]
    fn set_bpm_sets_the_tempo_in_the_model_and_in_the_audio_thread_and_marks_the_session_changed() {
        let (engine, mut ctl) = controller();
        sent(&engine);
        let applied = ctl.apply(Intent::SetBpm { bpm: 96.5 }).unwrap();
        assert_eq!(applied, Applied::of(1, vec![Event::BpmSet { bpm: 96.5 }]));
        assert_eq!(ctl.model.bpm, 96.5);
        assert_eq!(sent(&engine), ["SetGlobalBpm bpm=96.5"]);
        assert!(ctl.dirty, "the tempo is saved with the session");
    }

    #[test]
    fn set_bpm_holds_the_tempo_to_the_tempos_there_can_be_and_tells_what_it_is() {
        let (engine, mut ctl) = controller();
        sent(&engine);
        for (asked, is) in [(5.0, 20.0), (19.9, 20.0), (20.0, 20.0), (133.3, 133.3), (300.0, 300.0), (1000.0, 300.0), (-1.0, 20.0)] {
            let applied = ctl.apply(Intent::SetBpm { bpm: asked }).unwrap();
            assert_eq!(applied.events, vec![Event::BpmSet { bpm: is }], "{asked}");
            assert_eq!(ctl.model.bpm, is, "{asked}");
            assert_eq!(sent(&engine), [format!("SetGlobalBpm bpm={is:?}")], "{asked}");
        }
    }

    #[test]
    fn set_bpm_refuses_what_is_not_a_number_and_changes_nothing() {
        let (engine, mut ctl) = controller();
        sent(&engine);
        for (bpm, words) in [(f32::NAN, "NaN is not a tempo"), (f32::INFINITY, "inf is not a tempo"), (f32::NEG_INFINITY, "-inf is not a tempo")] {
            match ctl.apply(Intent::SetBpm { bpm }) {
                Err(IntentError::Invalid(said)) => assert_eq!(said, words),
                other => panic!("{bpm} is refused as invalid, and not {other:?}"),
            }
        }
        assert_eq!(ctl.model.bpm, 120.0);
        assert_eq!(sent(&engine), Vec::<String>::new());
        assert_eq!((ctl.rev, ctl.dirty), (0, false), "a refusal is not a revision");
    }

    #[test]
    fn a_tempo_that_is_set_again_is_a_revision_and_tells_what_it_is() {
        let (_engine, mut ctl) = controller();
        for revision in 1..=2 {
            let applied = ctl.apply(Intent::SetBpm { bpm: 90.0 }).unwrap();
            assert_eq!(applied, Applied::of(revision, vec![Event::BpmSet { bpm: 90.0 }]));
        }
    }

    #[test]
    fn a_tempo_leaves_the_tempo_that_a_pattern_was_recorded_at() {
        let (handles, _engine) = testutil::stub_engine();
        let session = "[[instrument]]\nplugin = \"builtin:osc\"\n[instrument.pattern]\nbpm = 120.0\nlength_beats = 4.0\nenabled = true\n";
        let mut ctl = Controller::new(load(session).model, handles, None);
        ctl.apply(Intent::SetBpm { bpm: 60.0 }).unwrap();
        assert_eq!(ctl.model.instruments[0].pattern.as_ref().unwrap().bpm, 120.0);
    }

    #[test]
    fn the_tempo_that_was_set_is_what_a_session_saves() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("song.toml");
        let (_engine, mut ctl) = controller_with_file(&path);
        ctl.apply(Intent::SetBpm { bpm: 87.5 }).unwrap();
        ctl.apply(Intent::Save).unwrap();
        assert_eq!(session::parse(&std::fs::read_to_string(&path).unwrap()).unwrap().bpm, Some(87.5));
    }

    /// A lane with no pattern, and a lane with one that was recorded at 90: two beats, one note, in key, looping and enabled.
    const WITH_A_PATTERN: &str = r#"
bpm = 120.0

[[instrument]]
plugin = "builtin:osc"

[[instrument]]
plugin = "builtin:osc"
[instrument.pattern]
bpm = 90.0
length_beats = 2.0
base_note = "C4"
enabled = true
transpose = "in_key"
[[instrument.pattern.events]]
frame = 0
status = "on"
note = "C4"
velocity = 100
[[instrument.pattern.events]]
frame = 24000
status = "off"
note = "C4"
velocity = 0
"#;

    fn with_patterns() -> (StubEngine, Controller) {
        let (handles, engine) = testutil::stub_engine();
        (engine, Controller::new(load(WITH_A_PATTERN).model, handles, None))
    }

    fn lane_at(ctl: &Controller, inst: usize) -> NodeId {
        node(ctl, NodePath::Instrument(inst))
    }

    /// What a pattern is told as: the settings and the notes it has.
    fn info(recording: bool, enabled: bool, looping: bool, in_key: bool, length_beats: f32, bpm: f32, notes: u32) -> crate::app::graph::PatternInfo {
        crate::app::graph::PatternInfo { recording, enabled, looping, in_key, length_beats, bpm, notes }
    }

    fn pattern_set(node: NodeId, pattern: Option<crate::app::graph::PatternInfo>) -> Event {
        Event::PatternSet { node, pattern }
    }

    #[test]
    fn a_recording_that_starts_on_a_lane_with_no_pattern_gives_it_an_empty_one_and_is_counted_in_the_tempo_as_the_terminal_does() {
        let (engine, mut ctl) = with_patterns();
        let lane = lane_at(&ctl, 0);
        sent(&engine);
        let applied = ctl.apply(Intent::SetRecording { node: lane, recording: true }).unwrap();
        assert_eq!(applied, Applied::of(1, vec![pattern_set(lane, Some(info(true, false, true, false, 4.0, 120.0, 0)))]));
        assert_eq!(sent(&engine), ["SetGlobalBpm bpm=120.0", "SetPatternLength inst=0 beats=4.0", "SetPatternRecording inst=0 recording=true"]);
        assert!(ctl.model.instruments[0].pattern.as_ref().unwrap().recording);
        assert!(ctl.dirty, "the pattern is saved with the session");
    }

    #[test]
    fn a_recording_that_starts_on_a_lane_that_has_a_pattern_keeps_its_notes_and_its_settings_until_the_new_ones_come() {
        let (engine, mut ctl) = with_patterns();
        let lane = lane_at(&ctl, 1);
        sent(&engine);
        let applied = ctl.apply(Intent::SetRecording { node: lane, recording: true }).unwrap();
        // (Recorded at 90, which it says still; the tempo of the next one is the session's, 120.)
        assert_eq!(applied.events, vec![pattern_set(lane, Some(info(true, true, true, true, 2.0, 90.0, 1)))]);
        assert_eq!(sent(&engine), ["SetGlobalBpm bpm=120.0", "SetPatternLength inst=1 beats=2.0", "SetPatternRecording inst=1 recording=true"]);
        assert_eq!(ctl.model.instruments[1].pattern.as_ref().unwrap().events.len(), 2);
    }

    #[test]
    fn a_recording_that_is_started_again_or_stopped_when_there_is_none_is_told_as_it_is_and_changes_nothing() {
        let (engine, mut ctl) = with_patterns();
        let lane = lane_at(&ctl, 0);
        ctl.apply(Intent::SetRecording { node: lane, recording: true }).unwrap();
        sent(&engine);
        let again = ctl.apply(Intent::SetRecording { node: lane, recording: true }).unwrap();
        assert_eq!(again, Applied::of(2, vec![pattern_set(lane, Some(info(true, false, true, false, 4.0, 120.0, 0)))]));
        assert_eq!(sent(&engine), Vec::<String>::new(), "the count-in is not begun again");
        ctl.apply(Intent::SetRecording { node: lane, recording: false }).unwrap();
        sent(&engine);
        let again = ctl.apply(Intent::SetRecording { node: lane, recording: false }).unwrap();
        assert_eq!(again.events, vec![pattern_set(lane, Some(info(false, false, true, false, 4.0, 120.0, 0)))]);
        assert_eq!(sent(&engine), Vec::<String>::new(), "there is nothing to stop");
        let other = lane_at(&ctl, 1);
        let stopped = ctl.apply(Intent::SetRecording { node: other, recording: false }).unwrap();
        assert_eq!(stopped.events, vec![pattern_set(other, Some(info(false, true, true, true, 2.0, 90.0, 1)))]);
        assert_eq!(sent(&engine), Vec::<String>::new());
    }

    #[test]
    fn a_recording_is_stopped_at_once_and_the_pattern_that_is_made_comes_from_the_audio_thread() {
        let (engine, mut ctl) = with_patterns();
        let lane = lane_at(&ctl, 0);
        ctl.apply(Intent::SetRecording { node: lane, recording: true }).unwrap();
        sent(&engine);
        let applied = ctl.apply(Intent::SetRecording { node: lane, recording: false }).unwrap();
        assert_eq!(applied.events, vec![pattern_set(lane, Some(info(false, false, true, false, 4.0, 120.0, 0)))]);
        assert_eq!(sent(&engine), ["SetPatternRecording inst=0 recording=false"]);
        assert!(!ctl.model.instruments[0].pattern.as_ref().unwrap().recording);
        // The audio thread reports what it recorded: the controller puts it in the lane.
        engine.pattern_tx.send(testutil::recording(0)).unwrap();
        ctl.poll();
        let pattern = ctl.model.instruments[0].pattern.as_ref().unwrap();
        assert_eq!((pattern.events.len(), pattern.recording, pattern.enabled), (2, false, true));
        let snapshot = ctl.snapshot();
        let kind = &snapshot.graph.nodes.iter().find(|n| n.id == lane).unwrap().kind;
        assert!(matches!(kind, NodeKind::Instrument { pattern: Some(p), .. } if p.notes == 1 && !p.recording), "{kind:?}");
    }

    #[test]
    fn a_recording_is_refused_for_what_is_not_a_lane_and_changes_nothing() {
        let (engine, mut ctl) = with_patterns();
        sent(&engine);
        // A node that is not there.
        assert!(matches!(ctl.apply(Intent::SetRecording { node: NodeId(9_999), recording: true }), Err(IntentError::NotFound(_))));
        // The master, which is a node of the graph and not of the model.
        let master = ctl.apply(Intent::SetRecording { node: NodeId::MASTER, recording: true });
        assert!(matches!(master, Err(IntentError::NotFound(_))), "{master:?}");
        assert_eq!(sent(&engine), Vec::<String>::new());
        assert_eq!((ctl.rev, ctl.dirty), (0, false));
        assert!(ctl.model.instruments[0].pattern.is_none());
    }

    #[test]
    fn a_pattern_is_not_asked_of_a_node_that_is_there_and_is_no_lane_an_effect_a_group_a_modulator() {
        let (handles, engine) = testutil::stub_engine();
        let mut ctl = Controller::new(load(THE_PATTERNS_SCENE).model, handles, None);
        let (group, bus, reverb, lane_lfo) = (
            node(&ctl, NodePath::Group(0)),
            node(&ctl, NodePath::GroupEffect { group: 0, index: 0 }),
            node(&ctl, NodePath::Effect { inst: 0, index: 0 }),
            ctl.model.free_modulators[0].node,
        );
        let group_lfo = hearing_group(&ctl.model, 0)[0].node;
        sent(&engine);
        for node in [group, bus, reverb, lane_lfo, group_lfo] {
            for intent in [
                Intent::SetRecording { node, recording: true },
                Intent::ClearPattern { node },
                Intent::SetPatternField { node, field: PatternField::Length, value: 2.0 },
            ] {
                let refused = ctl.apply(intent.clone());
                assert_eq!(refused, Err(invalid("a pattern belongs to a lane")), "{intent:?}");
            }
        }
        assert_eq!(sent(&engine), Vec::<String>::new());
        assert_eq!((ctl.rev, ctl.dirty), (0, false));
        assert!(ctl.model.instruments.iter().all(|lane| lane.pattern.as_ref().is_none_or(|pattern| pattern.length_beats == 2.0 && !pattern.recording)));
    }

    #[test]
    fn a_pattern_is_taken_out_with_the_audio_threads_and_the_lane_has_none() {
        let (engine, mut ctl) = with_patterns();
        let lane = lane_at(&ctl, 1);
        sent(&engine);
        let applied = ctl.apply(Intent::ClearPattern { node: lane }).unwrap();
        assert_eq!(applied, Applied::of(1, vec![pattern_set(lane, None)]));
        assert!(ctl.model.instruments[1].pattern.is_none());
        assert_eq!(sent(&engine), ["ClearPattern inst=1"]);
        assert!(ctl.dirty);
        // (There is none now to take out.)
        assert!(matches!(ctl.apply(Intent::ClearPattern { node: lane }), Err(IntentError::Invalid(_))));
        assert_eq!(sent(&engine), Vec::<String>::new());
        assert_eq!(ctl.rev, 1, "a refusal is no revision");
    }

    #[test]
    fn a_pattern_that_is_being_recorded_is_taken_out_too_and_the_recording_ends() {
        let (engine, mut ctl) = with_patterns();
        let lane = lane_at(&ctl, 1);
        ctl.apply(Intent::SetRecording { node: lane, recording: true }).unwrap();
        sent(&engine);
        ctl.apply(Intent::ClearPattern { node: lane }).unwrap();
        assert!(ctl.model.instruments[1].pattern.is_none());
        assert_eq!(sent(&engine), ["ClearPattern inst=1"]);
    }

    #[test]
    fn a_pattern_is_not_taken_out_of_what_is_not_a_lane() {
        let (_engine, mut ctl) = with_patterns();
        assert!(matches!(ctl.apply(Intent::ClearPattern { node: NodeId(9_999) }), Err(IntentError::NotFound(_))));
        let master = ctl.apply(Intent::ClearPattern { node: NodeId::MASTER });
        assert!(matches!(master, Err(IntentError::NotFound(_)) | Err(IntentError::Invalid(_))), "{master:?}");
        assert_eq!((ctl.rev, ctl.dirty), (0, false));
    }

    #[test]
    fn the_settings_of_a_pattern_are_set_in_the_model_and_the_audio_thread_each_by_its_own_command() {
        let (engine, mut ctl) = with_patterns();
        let lane = lane_at(&ctl, 1);
        sent(&engine);
        let set = |ctl: &mut Controller, field: PatternField, value: f32| ctl.apply(Intent::SetPatternField { node: lane, field, value }).unwrap();
        assert_eq!(set(&mut ctl, PatternField::Length, 8.0), Applied::of(1, vec![pattern_set(lane, Some(info(false, true, true, true, 8.0, 90.0, 1)))]));
        assert_eq!(sent(&engine), ["SetPatternLength inst=1 beats=8.0"]);
        assert_eq!(set(&mut ctl, PatternField::Enabled, 0.0).events, vec![pattern_set(lane, Some(info(false, false, true, true, 8.0, 90.0, 1)))]);
        assert_eq!(sent(&engine), ["SetPatternEnabled inst=1 enabled=false"]);
        assert_eq!(set(&mut ctl, PatternField::Looping, 0.0).events, vec![pattern_set(lane, Some(info(false, false, false, true, 8.0, 90.0, 1)))]);
        assert_eq!(sent(&engine), ["SetPatternLooping inst=1 looping=false"]);
        assert_eq!(set(&mut ctl, PatternField::Transpose, 0.0).events, vec![pattern_set(lane, Some(info(false, false, false, false, 8.0, 90.0, 1)))]);
        assert_eq!(sent(&engine), ["SetPatternInKey inst=1 in_key=false"]);
        let pattern = ctl.model.instruments[1].pattern.as_ref().unwrap();
        assert_eq!((pattern.length_beats, pattern.enabled, pattern.looping, pattern.in_key), (8.0, false, false, false));
        assert_eq!(pattern.events.len(), 2, "the notes are as they were");
        assert!(ctl.dirty);
    }

    #[test]
    fn a_setting_of_a_pattern_is_held_to_its_range_and_a_switch_is_rounded() {
        let (engine, mut ctl) = with_patterns();
        let lane = lane_at(&ctl, 1);
        sent(&engine);
        for (field, asked, is) in [
            (PatternField::Length, 0.0, 1.0),
            (PatternField::Length, 1.0, 1.0),
            (PatternField::Length, 2.5, 2.5),
            (PatternField::Length, 32.0, 32.0),
            (PatternField::Length, 100.0, 32.0),
            (PatternField::Enabled, 0.4, 0.0),
            (PatternField::Enabled, 0.6, 1.0),
            (PatternField::Looping, -3.0, 0.0),
            (PatternField::Looping, 7.0, 1.0),
            (PatternField::Transpose, 0.5, 1.0),
        ] {
            let applied = ctl.apply(Intent::SetPatternField { node: lane, field, value: asked }).unwrap();
            let Some(Event::PatternSet { pattern: Some(told), .. }) = applied.events.first() else { panic!("{applied:?}") };
            let now = match field {
                PatternField::Length => told.length_beats,
                PatternField::Enabled => f32::from(u8::from(told.enabled)),
                PatternField::Looping => f32::from(u8::from(told.looping)),
                PatternField::Transpose => f32::from(u8::from(told.in_key)),
            };
            assert_eq!(now, is, "{field:?} {asked}");
        }
        sent(&engine);
    }

    #[test]
    fn a_setting_of_a_pattern_that_is_not_a_number_is_refused_and_changes_nothing() {
        let (engine, mut ctl) = with_patterns();
        let lane = lane_at(&ctl, 1);
        sent(&engine);
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let refused = ctl.apply(Intent::SetPatternField { node: lane, field: PatternField::Length, value });
            assert_eq!(refused, Err(invalid(format!("{value} is not a value for Length"))));
            // (The number is judged before the node it is for: a node that is not a lane, and one that is not there, are refused for the number.)
            for field in [PatternField::Enabled, PatternField::Looping, PatternField::Transpose] {
                for node in [NodeId(9_999), NodeId::MASTER] {
                    let refused = ctl.apply(Intent::SetPatternField { node, field, value });
                    assert_eq!(refused, Err(invalid(format!("{value} is not a value for {field:?}"))), "{node:?}");
                }
            }
        }
        assert_eq!(ctl.model.instruments[1].pattern.as_ref().unwrap().length_beats, 2.0);
        assert_eq!(sent(&engine), Vec::<String>::new());
        assert_eq!((ctl.rev, ctl.dirty), (0, false));
    }

    #[test]
    fn a_setting_given_to_a_lane_that_has_no_pattern_makes_an_empty_one_so_that_the_length_of_its_first_recording_can_be_chosen() {
        let (engine, mut ctl) = with_patterns();
        let lane = lane_at(&ctl, 0);
        sent(&engine);
        let applied = ctl.apply(Intent::SetPatternField { node: lane, field: PatternField::Length, value: 8.0 }).unwrap();
        assert_eq!(applied.events, vec![pattern_set(lane, Some(info(false, false, true, false, 8.0, 120.0, 0)))]);
        assert_eq!(sent(&engine), ["SetPatternLength inst=0 beats=8.0"]);
        // The recording that follows is counted in it.
        sent(&engine);
        ctl.apply(Intent::SetRecording { node: lane, recording: true }).unwrap();
        assert_eq!(sent(&engine), ["SetGlobalBpm bpm=120.0", "SetPatternLength inst=0 beats=8.0", "SetPatternRecording inst=0 recording=true"]);
    }

    #[test]
    fn a_setting_is_not_given_to_what_is_not_a_lane() {
        let (_engine, mut ctl) = with_patterns();
        let refused = ctl.apply(Intent::SetPatternField { node: NodeId(9_999), field: PatternField::Length, value: 2.0 });
        assert!(matches!(refused, Err(IntentError::NotFound(_))), "{refused:?}");
        let master = ctl.apply(Intent::SetPatternField { node: NodeId::MASTER, field: PatternField::Length, value: 2.0 });
        assert!(matches!(master, Err(IntentError::NotFound(_)) | Err(IntentError::Invalid(_))), "{master:?}");
        assert_eq!((ctl.rev, ctl.dirty), (0, false));
    }

    #[test]
    fn what_is_done_to_a_pattern_is_no_step_of_the_history_and_does_not_forget_it() {
        let (_engine, mut ctl) = with_patterns();
        let lane = lane_at(&ctl, 1);
        let osc = lane_at(&ctl, 0);
        gain(&mut ctl, osc, Gain::Volume, 0.5).unwrap();
        ctl.apply(Intent::SetPatternField { node: lane, field: PatternField::Length, value: 8.0 }).unwrap();
        ctl.apply(Intent::SetRecording { node: lane, recording: true }).unwrap();
        ctl.apply(Intent::ClearPattern { node: lane }).unwrap();
        // The step that was there is the one that is undone, and the pattern is what it was.
        ctl.undo().unwrap();
        assert_eq!(ctl.model.instruments[0].volume, 1.0);
        assert!(ctl.model.instruments[1].pattern.is_none(), "(a pattern is not put back by undo)");
    }

    /// A group with a bus filter and an LFO, a member of it with a reverb and an LFO of its own, a lane with a pattern that was recorded at 90 (two beats, one note, in key,
    /// looping and enabled) and a lane with no instrument, at a tempo of 100: every kind of node that a pattern can be asked of, and is refused for.
    const THE_PATTERNS_SCENE: &str = r#"
bpm = 100.0

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
[[instrument.modulator]]
type = "lfo"

[[instrument]]
plugin = "builtin:osc"
[instrument.pattern]
bpm = 90.0
length_beats = 2.0
base_note = "C4"
enabled = true
transpose = "in_key"
[[instrument.pattern.events]]
frame = 0
status = "on"
note = "C4"
velocity = 100
[[instrument.pattern.events]]
frame = 24000
status = "off"
note = "C4"
velocity = 0

[[instrument]]
"#;

    /// `THE_PATTERNS_SCENE`, and what the controller makes of intents about patterns, as the web client's tests read it (`web/src/client/fixtures/patterns.json`): they pin the
    /// mock engine's patterns to the controller's. Each script is a list of intents asked of one controller in turn, with the event each is answered with or the words it is
    /// refused with: every kind of intent asked of every node of the session (the master and one that is not there too), with the values that are held to the range, rounded and
    /// taken as they are, and then runs on each lane that go on from one intent to the next (a recording that is started twice, a pattern that is taken out twice, a setting that
    /// makes the pattern a lane had not). `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` rewrites it.
    #[test]
    fn the_web_clients_fixture_of_what_a_pattern_comes_to_is_current() {
        let scene = || {
            let (handles, engine) = testutil::stub_engine();
            (engine, Controller::new(load(THE_PATTERNS_SCENE).model, handles, None))
        };
        let (_engine, ctl) = scene();
        let before = ctl.snapshot();
        let mut nodes: Vec<NodeId> = ctl.model.nodes().map(|(id, _)| id).collect();
        let lanes: Vec<NodeId> = (0..ctl.model.instruments.len()).map(|inst| lane_at(&ctl, inst)).collect();
        nodes.extend([NodeId::MASTER, NodeId(9_999)]);
        let length = [0.0, 0.4, 1.0, 2.5, 8.0, 32.0, 100.0, -3.0];
        let switch = [0.0, 0.4, 0.6, 1.0, 7.0, -3.0];

        let mut scripts: Vec<Vec<Intent>> = Vec::new();
        for &node in &nodes {
            scripts.push(vec![Intent::SetRecording { node, recording: true }]);
            scripts.push(vec![Intent::SetRecording { node, recording: false }]);
            scripts.push(vec![Intent::ClearPattern { node }]);
            for (field, values) in [(PatternField::Length, &length[..]), (PatternField::Enabled, &switch[..]), (PatternField::Looping, &switch[..]), (PatternField::Transpose, &switch[..])] {
                scripts.extend(values.iter().map(|&value| vec![Intent::SetPatternField { node, field, value }]));
            }
        }
        for &node in &lanes {
            let record = |recording| Intent::SetRecording { node, recording };
            let field = |field, value| Intent::SetPatternField { node, field, value };
            let clear = Intent::ClearPattern { node };
            scripts.push(vec![record(true), record(true), record(false), record(false)]);
            scripts.push(vec![field(PatternField::Length, 8.0), record(true), record(false), clear.clone(), clear.clone()]);
            scripts.push(vec![
                field(PatternField::Enabled, 1.0),
                field(PatternField::Looping, 0.0),
                field(PatternField::Transpose, 1.0),
                field(PatternField::Length, 3.5),
                clear.clone(),
                field(PatternField::Length, 2.0),
            ]);
            scripts.push(vec![clear.clone(), record(false), record(true), clear.clone(), record(true)]);
        }

        let scripts: Vec<serde_json::Value> = scripts
            .into_iter()
            .map(|script| {
                let (_engine, mut ctl) = scene();
                let steps: Vec<serde_json::Value> = script
                    .into_iter()
                    .map(|intent| match ctl.apply(intent.clone()) {
                        Err(error) => serde_json::json!({ "intent": testutil::wire(&intent), "error": error.to_string() }),
                        Ok(applied) => {
                            assert_eq!(applied.events.len(), 1, "a pattern's intent is told as one event: {intent:?}");
                            serde_json::json!({ "intent": testutil::wire(&intent), "event": testutil::wire(&applied.events[0]) })
                        }
                    })
                    .collect();
                serde_json::json!(steps)
            })
            .collect();
        assert!(scripts.len() > 200, "the fixture is not empty");

        let wire = serde_json::to_string(&before).unwrap();
        let mut snapshot: serde_json::Value = serde_json::from_str(&wire).unwrap();
        // The layout's fingerprint comes from the hasher, which the standard library does not promise to keep the same.
        snapshot["telemetry"]["rev"] = serde_json::json!(1);
        let fixture = serde_json::json!({ "snapshot": snapshot, "scripts": scripts });
        let fresh = serde_json::to_string_pretty(&fixture).unwrap() + "\n";

        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/src/client/fixtures/patterns.json");
        if std::env::var_os("TANG_UPDATE_TYPES").is_some() {
            std::fs::write(&path, &fresh).unwrap();
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            committed == fresh,
            "{} is not what a pattern comes to now: run `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` and commit the result",
            path.display()
        );
    }

    #[test]
    fn the_scale_and_the_lock_that_were_set_are_what_a_session_saves() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("song.toml");
        let (_engine, mut ctl) = controller_with_file(&path);
        ctl.apply(Intent::SetScale { scale: "Bb dorian".to_string() }).unwrap();
        ctl.apply(Intent::SetLocked { locked: true }).unwrap();
        ctl.apply(Intent::Save).unwrap();
        let piano = session::parse(&std::fs::read_to_string(&path).unwrap()).unwrap().piano.expect("the piano settings are saved");
        assert_eq!((piano.scale.as_deref(), piano.locked), (Some("A# dorian"), true));
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
        ctl.model.free_modulators[index].node
    }

    fn group_mod(ctl: &Controller, index: usize) -> NodeId {
        hearing_group(&ctl.model, 0)[index].node
    }

    fn field(ctl: &mut Controller, node: NodeId, field: ModulatorField, value: f32) -> Result<Applied, IntentError> {
        ctl.apply(Intent::SetModulatorField { node, field, value })
    }

    use crate::app::model::ModSourceSlot;
    use crate::plugin::chain::{LfoWaveform, MidiBindSource};

    fn lane_lfo(ctl: &Controller, index: usize) -> (LfoWaveform, f32) {
        match &ctl.model.free_modulators[index].source {
            ModSourceSlot::Lfo { waveform, rate, .. } => (*waveform, *rate),
            other => panic!("{other:?} is not an LFO"),
        }
    }

    fn lane_envelope(ctl: &Controller, index: usize) -> [f32; 4] {
        match &ctl.model.free_modulators[index].source {
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
        assert_eq!(sent(&engine), ["SetFreeModulatorRate mod=0 rate=5.000", "SetFreeModulatorWaveform mod=0 waveform=Triangle"]);
        assert!(ctl.dirty);
        assert_eq!(ctl.rev, 2);
    }

    #[test]
    fn set_modulator_field_sets_a_lane_lfo_to_a_random_waveform_in_the_model_and_the_engine() {
        let (engine, mut ctl) = modulated();
        sent(&engine);
        let lfo = lane_mod(&ctl, 0);
        field(&mut ctl, lfo, ModulatorField::Waveform, 4.0).unwrap();
        assert_eq!(lane_lfo(&ctl, 0).0, LfoWaveform::Random);
        field(&mut ctl, lfo, ModulatorField::Waveform, 5.0).unwrap();
        assert_eq!(lane_lfo(&ctl, 0).0, LfoWaveform::Smooth);
        assert_eq!(sent(&engine), ["SetFreeModulatorWaveform mod=0 waveform=Random", "SetFreeModulatorWaveform mod=0 waveform=Smooth"]);
    }

    /// Whether a note starts an LFO of the lane again, and how many seconds it takes to come in.
    fn lane_key(ctl: &Controller, index: usize) -> (bool, f32) {
        match &ctl.model.free_modulators[index].source {
            ModSourceSlot::Lfo { retrigger, fade, .. } => (*retrigger, *fade),
            other => panic!("{other:?} is not an LFO"),
        }
    }

    #[test]
    fn set_modulator_field_sets_whether_a_note_starts_an_lfo_again_and_how_it_fades_in_and_gives_the_engine_both_each_time() {
        let (engine, mut ctl) = modulated();
        sent(&engine);
        let lfo = lane_mod(&ctl, 0);
        let applied = field(&mut ctl, lfo, ModulatorField::Retrigger, 1.0).unwrap();
        assert_eq!(applied, Applied::of(1, vec![Event::ModulatorFieldSet { node: lfo, field: ModulatorField::Retrigger, value: 1.0 }]));
        field(&mut ctl, lfo, ModulatorField::Fade, 0.75).unwrap();
        field(&mut ctl, lfo, ModulatorField::Retrigger, 0.0).unwrap();
        assert_eq!(lane_key(&ctl, 0), (false, 0.75));
        assert_eq!(
            sent(&engine),
            [
                "SetModulatorKey Free index=0 retrigger=true fade=0.000",
                "SetModulatorKey Free index=0 retrigger=true fade=0.750",
                "SetModulatorKey Free index=0 retrigger=false fade=0.750",
            ],
            "one command that has both, whichever was changed"
        );
        assert!(ctl.dirty);
        assert_eq!(ctl.rev, 3);
    }

    #[test]
    fn set_modulator_field_reaches_the_key_of_the_lfo_that_hears_a_group_by_the_same_command_and_leaves_the_lanes_alone() {
        let (engine, mut ctl) = modulated();
        sent(&engine);
        let group = group_mod(&ctl, 0);
        field(&mut ctl, group, ModulatorField::Fade, 2.0).unwrap();
        field(&mut ctl, group, ModulatorField::Retrigger, 1.0).unwrap();
        assert_eq!(
            sent(&engine),
            ["SetModulatorKey Free index=4 retrigger=false fade=2.000", "SetModulatorKey Free index=4 retrigger=true fade=2.000"]
        );
        assert_eq!(lane_key(&ctl, 0), (false, 0.0), "the lane's is another modulator");
    }

    #[test]
    fn the_key_of_an_lfo_is_held_to_its_range_and_the_switch_is_rounded() {
        let (engine, mut ctl) = modulated();
        let lfo = lane_mod(&ctl, 0);
        let told = |applied: Applied| match applied.events[0] {
            Event::ModulatorFieldSet { value, .. } => value,
            ref other => panic!("{other:?}"),
        };
        for (setting, asked, got) in [
            (ModulatorField::Fade, 9.0, 5.0),
            (ModulatorField::Fade, -1.0, 0.0),
            (ModulatorField::Fade, 0.3, 0.3),
            (ModulatorField::Retrigger, 0.4, 0.0),
            (ModulatorField::Retrigger, 0.6, 1.0),
            (ModulatorField::Retrigger, 7.0, 1.0),
        ] {
            assert_eq!(told(field(&mut ctl, lfo, setting, asked).unwrap()), got, "{setting:?} {asked}");
        }
        sent(&engine);
        let rev = ctl.rev;
        for setting in [ModulatorField::Fade, ModulatorField::Retrigger] {
            for bad in [f32::NAN, f32::INFINITY] {
                assert!(matches!(field(&mut ctl, lfo, setting, bad), Err(IntentError::Invalid(_))), "{setting:?} {bad}");
            }
        }
        assert_eq!((ctl.rev, sent(&engine)), (rev, Vec::<String>::new()), "a refusal changes nothing");
    }

    #[test]
    fn an_envelope_has_no_key_and_a_modulator_that_is_not_an_lfo_has_no_fade() {
        let (engine, mut ctl) = modulated();
        let envelope = lane_mod(&ctl, 1);
        sent(&engine);
        for setting in [ModulatorField::Retrigger, ModulatorField::Fade] {
            assert!(matches!(field(&mut ctl, envelope, setting, 0.5), Err(IntentError::Invalid(_))), "an envelope: {setting:?}");
        }
        let (_engine, mut sequencers) = sequencers();
        let sequencer = lane_mod(&sequencers, 0);
        assert!(matches!(field(&mut sequencers, sequencer, ModulatorField::Fade, 0.5), Err(IntentError::Invalid(_))), "a step sequencer has no fade");
        let (_engine, mut knobs) = macros();
        let knob = lane_mod(&knobs, 0);
        assert!(matches!(field(&mut knobs, knob, ModulatorField::Fade, 0.5), Err(IntentError::Invalid(_))), "a macro");
        assert_eq!((ctl.rev, sent(&engine)), (0, Vec::<String>::new()), "nothing changed");
    }

    /// The note length an LFO of the lane follows the tempo by (a place in `SYNC_DIVISIONS`, 0 for none) and its own rate.
    fn lane_sync(ctl: &Controller, index: usize) -> (usize, f32) {
        match &ctl.model.free_modulators[index].source {
            ModSourceSlot::Lfo { sync, rate, .. } => (*sync, *rate),
            other => panic!("{other:?} is not an LFO"),
        }
    }

    #[test]
    fn set_modulator_field_sets_the_note_length_an_lfo_follows_the_tempo_by_and_gives_the_engine_its_beats_and_keeps_the_rate() {
        let (engine, mut ctl) = modulated();
        sent(&engine);
        let lfo = lane_mod(&ctl, 0);
        let (_, rate) = lane_sync(&ctl, 0);
        // 11 is an eighth note, 14 a sixteenth, 6 a triplet of halves and 0 none.
        let applied = field(&mut ctl, lfo, ModulatorField::Sync, 11.0).unwrap();
        assert_eq!(applied, Applied::of(1, vec![Event::ModulatorFieldSet { node: lfo, field: ModulatorField::Sync, value: 11.0 }]));
        assert_eq!(lane_sync(&ctl, 0), (11, rate));
        field(&mut ctl, lfo, ModulatorField::Sync, 14.0).unwrap();
        field(&mut ctl, lfo, ModulatorField::Sync, 6.0).unwrap();
        field(&mut ctl, lfo, ModulatorField::Sync, 0.0).unwrap();
        assert_eq!(lane_sync(&ctl, 0), (0, rate), "its own rate is what it was all through");
        assert_eq!(
            sent(&engine),
            [
                "SetModulatorSync Free index=0 beats=0.500",
                "SetModulatorSync Free index=0 beats=0.250",
                "SetModulatorSync Free index=0 beats=1.333",
                "SetModulatorSync Free index=0 beats=0.000",
            ],
            "the beats of the note length, and nothing of the rate"
        );
        assert!(ctl.dirty);
        assert_eq!(ctl.rev, 4);
    }

    #[test]
    fn set_modulator_field_reaches_the_sync_of_the_lfo_that_hears_a_group_by_the_same_command_and_leaves_the_lanes_alone() {
        let (engine, mut ctl) = modulated();
        sent(&engine);
        let group = group_mod(&ctl, 0);
        field(&mut ctl, group, ModulatorField::Sync, 8.0).unwrap();
        assert_eq!(sent(&engine), ["SetModulatorSync Free index=4 beats=1.000"]);
        assert_eq!(lane_sync(&ctl, 0).0, 0, "the lane's is another modulator");
    }

    #[test]
    fn a_sync_is_held_to_the_list_and_rounded_to_a_place_in_it() {
        let (engine, mut ctl) = modulated();
        let lfo = lane_mod(&ctl, 0);
        let told = |applied: Applied| match applied.events[0] {
            Event::ModulatorFieldSet { value, .. } => value,
            ref other => panic!("{other:?}"),
        };
        for (asked, got) in [(99.0, 16.0), (-3.0, 0.0), (2.4, 2.0), (2.6, 3.0), (16.0, 16.0)] {
            assert_eq!(told(field(&mut ctl, lfo, ModulatorField::Sync, asked).unwrap()), got, "{asked}");
        }
        assert_eq!(lane_sync(&ctl, 0).0, 16);
        sent(&engine);
        let rev = ctl.rev;
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(matches!(field(&mut ctl, lfo, ModulatorField::Sync, bad), Err(IntentError::Invalid(_))), "{bad}");
        }
        assert_eq!((ctl.rev, sent(&engine), lane_sync(&ctl, 0).0), (rev, Vec::<String>::new(), 16), "a refusal changes nothing");
    }

    #[test]
    fn an_envelope_a_midi_control_and_a_macro_do_not_follow_the_tempo_and_have_no_sync_to_set() {
        let (engine, mut ctl) = modulated();
        let (envelope, control) = (lane_mod(&ctl, 1), lane_mod(&ctl, 2));
        sent(&engine);
        for (name, node) in [("an envelope", envelope), ("a MIDI control", control)] {
            assert!(matches!(field(&mut ctl, node, ModulatorField::Sync, 4.0), Err(IntentError::Invalid(_))), "{name}");
        }
        let (_engine, mut knobs) = macros();
        let knob = lane_mod(&knobs, 0);
        assert!(matches!(field(&mut knobs, knob, ModulatorField::Sync, 4.0), Err(IntentError::Invalid(_))), "a macro");
        assert_eq!((ctl.rev, sent(&engine)), (0, Vec::<String>::new()), "nothing changed");
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
                "SetFreeModulatorEnvelopeParam mod=1 attack=0.500 decay=0.300 sustain=0.600 release=0.500",
                "SetFreeModulatorEnvelopeParam mod=1 attack=0.500 decay=1.500 sustain=0.600 release=0.500",
                "SetFreeModulatorEnvelopeParam mod=1 attack=0.500 decay=1.500 sustain=0.250 release=0.500",
                "SetFreeModulatorEnvelopeParam mod=1 attack=0.500 decay=1.500 sustain=0.250 release=2.000",
            ]
        );
    }

    /// A group with a macro at 0.5 and a lane in it with a macro at 0.25.
    const MACROS: &str = r#"
[[group]]
[[group.modulator]]
type = "macro"
value = 0.5

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.modulator]]
type = "macro"
value = 0.25
"#;

    fn macros() -> (StubEngine, Controller) {
        let (handles, engine) = testutil::stub_engine();
        (engine, Controller::new(load(MACROS).model, handles, None))
    }

    fn lane_macro(ctl: &Controller) -> f32 {
        match &ctl.model.free_modulators[0].source {
            ModSourceSlot::Macro { value } => *value,
            other => panic!("{other:?} is not a macro"),
        }
    }

    #[test]
    fn set_modulator_field_value_sets_a_lane_macro_in_the_model_and_gives_the_engine_a_macro_of_that_value() {
        let (engine, mut ctl) = macros();
        sent(&engine);
        let knob = lane_mod(&ctl, 0);
        let applied = field(&mut ctl, knob, ModulatorField::Value, 0.75).unwrap();
        assert_eq!(applied, Applied::of(1, vec![Event::ModulatorFieldSet { node: knob, field: ModulatorField::Value, value: 0.75 }]));
        assert_eq!(lane_macro(&ctl), 0.75);
        assert_eq!(sent(&engine), ["SetFreeModulatorSource mod=0 source=Macro { value: 0.75 }"]);
        assert!(ctl.dirty);
        assert_eq!(ctl.rev, 1);
    }

    #[test]
    fn the_value_of_a_macro_is_held_between_nothing_and_one_and_a_number_that_is_not_one_is_refused() {
        let (engine, mut ctl) = macros();
        let knob = lane_mod(&ctl, 0);
        let told = |applied: Applied| match applied.events[0] {
            Event::ModulatorFieldSet { value, .. } => value,
            ref other => panic!("{other:?}"),
        };
        for (asked, got) in [(1.5, 1.0), (-0.5, 0.0), (0.0, 0.0), (1.0, 1.0), (0.3, 0.3)] {
            assert_eq!(told(field(&mut ctl, knob, ModulatorField::Value, asked).unwrap()), got, "{asked}");
            assert_eq!(lane_macro(&ctl), got, "{asked}");
        }
        sent(&engine);
        let rev = ctl.rev;
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(matches!(field(&mut ctl, knob, ModulatorField::Value, bad), Err(IntentError::Invalid(_))), "{bad}");
        }
        assert_eq!((lane_macro(&ctl), ctl.rev), (0.3, rev), "a refusal changes nothing");
        assert_eq!(sent(&engine), Vec::<String>::new());
    }

    /// A group with a step sequencer of three steps (at 3.5 a second) and a lane in it with one that is as a new one is.
    const SEQUENCERS: &str = r#"
[[group]]
[[group.modulator]]
type = "steps"
steps = [0.5, -0.25, 1.0]
rate = 3.5

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.modulator]]
type = "steps"
"#;

    fn sequencers() -> (StubEngine, Controller) {
        let (handles, engine) = testutil::stub_engine();
        (engine, Controller::new(load(SEQUENCERS).model, handles, None))
    }

    /// What a step sequencer has, in the model.
    fn steps_in(ctl: &Controller, at: ModulatorLocation) -> ([f32; 16], usize, f32, f32, bool) {
        steps_at(ctl, at, 0)
    }

    /// What the step sequencer at place `index` of the session's own rack has, in the model.
    fn steps_at(ctl: &Controller, at: ModulatorLocation, index: usize) -> ([f32; 16], usize, f32, f32, bool) {
        let rack = ctl.model.modulators(at).expect("the rack is there");
        match &rack[index].source {
            ModSourceSlot::Steps { values, length, rate, glide, retrigger, .. } => (*values, *length, *rate, *glide, *retrigger),
            other => panic!("{other:?} is not a step sequencer"),
        }
    }

    #[test]
    fn set_modulator_field_sets_the_note_length_a_step_sequencer_follows_the_tempo_by_and_gives_the_engine_the_beats_and_not_its_steps() {
        let (engine, mut ctl) = sequencers();
        sent(&engine);
        let lane = lane_mod(&ctl, 0);
        let rate = steps_in(&ctl, ModulatorLocation::Free).2;
        field(&mut ctl, lane, ModulatorField::Sync, 14.0).unwrap();
        match &ctl.model.free_modulators[0].source {
            ModSourceSlot::Steps { sync, rate: kept, .. } => assert_eq!((*sync, *kept), (14, rate)),
            other => panic!("{other:?}"),
        }
        assert_eq!(sent(&engine), ["SetModulatorSync Free index=0 beats=0.250"]);
        let group = group_mod(&ctl, 0);
        field(&mut ctl, group, ModulatorField::Sync, 11.0).unwrap();
        assert_eq!(sent(&engine), ["SetModulatorSync Free index=1 beats=0.500"]);
    }

    /// What `sent` says of the command that gives the engine the steps of a step sequencer.
    fn steps_line(index: usize, values: [f32; 16], length: usize, glide: f32, retrigger: bool) -> String {
        format!("SetModulatorSteps Free index={index} values={values:?} length={length} glide={glide:.3} retrigger={retrigger}")
    }

    #[test]
    fn set_modulator_field_sets_what_a_step_sequencer_goes_by_in_the_model_and_gives_the_engine_all_of_it_each_time() {
        let (engine, mut ctl) = sequencers();
        sent(&engine);
        let lane = lane_mod(&ctl, 0);
        let ModSourceSlot::Steps { values: staircase, .. } = ModSourceSlot::new_steps() else { unreachable!() };
        let applied = field(&mut ctl, lane, ModulatorField::Glide, 0.25).unwrap();
        assert_eq!(applied, Applied::of(1, vec![Event::ModulatorFieldSet { node: lane, field: ModulatorField::Glide, value: 0.25 }]));
        field(&mut ctl, lane, ModulatorField::Length, 5.0).unwrap();
        field(&mut ctl, lane, ModulatorField::Retrigger, 0.0).unwrap();
        field(&mut ctl, lane, ModulatorField::Rate, 12.5).unwrap();
        assert_eq!(steps_in(&ctl, ModulatorLocation::Free), (staircase, 5, 12.5, 0.25, false));
        assert_eq!(
            sent(&engine),
            [
                steps_line(0, staircase, 16, 0.25, true),
                steps_line(0, staircase, 5, 0.25, true),
                steps_line(0, staircase, 5, 0.25, false),
                "SetFreeModulatorRate mod=0 rate=12.500".to_string(),
            ],
            "the rate is an LFO's command, and the rest are one that has all of the others"
        );
        assert!(ctl.dirty);
        assert_eq!(ctl.rev, 4);
    }

    #[test]
    fn the_settings_of_a_step_sequencer_are_held_to_their_ranges_and_the_whole_ones_are_rounded() {
        let (engine, mut ctl) = sequencers();
        let lane = lane_mod(&ctl, 0);
        let told = |applied: Applied| match applied.events[0] {
            Event::ModulatorFieldSet { value, .. } => value,
            ref other => panic!("{other:?}"),
        };
        for (setting, asked, got) in [
            (ModulatorField::Glide, 2.0, 1.0),
            (ModulatorField::Glide, -1.0, 0.0),
            (ModulatorField::Glide, 0.3, 0.3),
            (ModulatorField::Length, 0.0, 1.0),
            (ModulatorField::Length, 99.0, 16.0),
            (ModulatorField::Length, 4.4, 4.0),
            (ModulatorField::Length, 4.6, 5.0),
            (ModulatorField::Retrigger, 0.4, 0.0),
            (ModulatorField::Retrigger, 0.6, 1.0),
            (ModulatorField::Retrigger, 7.0, 1.0),
            (ModulatorField::Rate, 1.0e9, 50.0),
            (ModulatorField::Rate, -3.0, 0.01),
        ] {
            assert_eq!(told(field(&mut ctl, lane, setting, asked).unwrap()), got, "{setting:?} {asked}");
        }
        sent(&engine);
        let rev = ctl.rev;
        for setting in [ModulatorField::Glide, ModulatorField::Length, ModulatorField::Retrigger, ModulatorField::Rate] {
            for bad in [f32::NAN, f32::INFINITY] {
                assert!(matches!(field(&mut ctl, lane, setting, bad), Err(IntentError::Invalid(_))), "{setting:?} {bad}");
            }
        }
        assert_eq!((ctl.rev, sent(&engine)), (rev, Vec::<String>::new()), "a refusal changes nothing");
    }

    #[test]
    fn a_step_sequencer_has_no_waveform_or_stage_or_value_and_the_others_have_no_glide_or_length_and_an_envelope_and_a_macro_no_switch() {
        let (engine, mut ctl) = sequencers();
        let lane = lane_mod(&ctl, 0);
        sent(&engine);
        for setting in [ModulatorField::Waveform, ModulatorField::Attack, ModulatorField::Decay, ModulatorField::Sustain, ModulatorField::Release, ModulatorField::Value] {
            assert!(matches!(field(&mut ctl, lane, setting, 0.5), Err(IntentError::Invalid(_))), "{setting:?}");
        }
        // (An LFO has the switch, which is its key's, and neither the glide nor the length.)
        for (name, mut other, modulator, refused) in [
            ("an LFO", modulated(), 0, vec![ModulatorField::Glide, ModulatorField::Length]),
            ("an envelope", modulated(), 1, vec![ModulatorField::Glide, ModulatorField::Length, ModulatorField::Retrigger]),
        ] {
            let node = lane_mod(&other.1, modulator);
            for setting in refused {
                assert!(matches!(field(&mut other.1, node, setting, 0.5), Err(IntentError::Invalid(_))), "{name}: {setting:?}");
            }
        }
        let (_engine, mut knobs) = macros();
        let knob = lane_mod(&knobs, 0);
        for setting in [ModulatorField::Glide, ModulatorField::Length, ModulatorField::Retrigger] {
            assert!(matches!(field(&mut knobs, knob, setting, 0.5), Err(IntentError::Invalid(_))), "a macro: {setting:?}");
        }
        assert_eq!((ctl.rev, sent(&engine)), (0, Vec::<String>::new()), "nothing changed");
    }

    /// The sixteen values of a step sequencer that a test asks for: 0.1, 0.2, ... and then back down.
    fn some_steps() -> Vec<f32> {
        (0..16).map(|i| if i < 8 { (i + 1) as f32 / 10.0 } else { (15 - i) as f32 / 10.0 }).collect()
    }

    #[test]
    fn set_steps_gives_a_step_sequencer_all_of_its_values_and_the_engine_with_them_and_tells_them() {
        let (engine, mut ctl) = sequencers();
        sent(&engine);
        let lane = lane_mod(&ctl, 0);
        let values = some_steps();
        let applied = ctl.apply(Intent::SetSteps { node: lane, values: values.clone() }).unwrap();
        assert_eq!(applied, Applied::of(1, vec![Event::StepsSet { node: lane, values: values.clone() }]));
        let all: [f32; 16] = values.clone().try_into().unwrap();
        assert_eq!(steps_in(&ctl, ModulatorLocation::Free), (all, 16, 8.0, 0.0, true), "the rest of what it has is as it was");
        assert_eq!(sent(&engine), [steps_line(0, all, 16, 0.0, true)]);
        assert!(ctl.dirty);
        // The one that hears the group is another modulator, with its own length and glide.
        let group = group_mod(&ctl, 0);
        ctl.apply(Intent::SetSteps { node: group, values: values.clone() }).unwrap();
        assert_eq!(steps_at(&ctl, ModulatorLocation::Free, 1), (all, 3, 3.5, 0.0, true));
        assert_eq!(sent(&engine), [steps_line(1, all, 3, 0.0, true)]);
        assert_eq!(steps_in(&ctl, ModulatorLocation::Free).0, all, "the first one keeps what it was given");
    }

    #[test]
    fn the_values_of_a_step_sequencer_are_held_to_minus_one_to_one_and_what_is_told_is_what_it_has() {
        let (_engine, mut ctl) = sequencers();
        let lane = lane_mod(&ctl, 0);
        let mut values = vec![0.0; 16];
        values[0] = 3.0;
        values[1] = -7.5;
        values[2] = 0.5;
        let applied = ctl.apply(Intent::SetSteps { node: lane, values }).unwrap();
        let Event::StepsSet { values: told, .. } = &applied.events[0] else { panic!("{:?}", applied.events) };
        assert_eq!(told[..4], [1.0, -1.0, 0.5, 0.0]);
        assert_eq!(steps_in(&ctl, ModulatorLocation::Free).0[..4], [1.0, -1.0, 0.5, 0.0]);
    }

    #[test]
    fn set_steps_is_refused_for_what_is_not_a_step_sequencer_a_list_of_the_wrong_length_and_a_value_that_is_not_a_number() {
        let (engine, mut ctl) = sequencers();
        let (_other, mut lfos) = modulated();
        let lane = lane_mod(&ctl, 0);
        sent(&engine);
        let lane_node = ctl.model.instruments[0].node;
        let lfo = lane_mod(&lfos, 0);
        let rev = ctl.rev;
        for (what, node, values) in [
            ("a list that is short", lane, vec![0.0; 15]),
            ("a list that is long", lane, vec![0.0; 17]),
            ("none", lane, vec![]),
            ("a value that is not a number", lane, { let mut v = vec![0.0; 16]; v[7] = f32::NAN; v }),
            ("a value that is not finite", lane, { let mut v = vec![0.0; 16]; v[7] = f32::INFINITY; v }),
            ("a lane", lane_node, vec![0.0; 16]),
            ("a node that is not there", NodeId(9999), vec![0.0; 16]),
        ] {
            let result = ctl.apply(Intent::SetSteps { node, values });
            assert!(matches!(result, Err(IntentError::Invalid(_)) | Err(IntentError::NotFound(_))), "{what}: {result:?}");
        }
        assert!(matches!(lfos.apply(Intent::SetSteps { node: lfo, values: vec![0.0; 16] }), Err(IntentError::Invalid(_))), "an LFO");
        assert!(matches!(ctl.apply(Intent::SetSteps { node: NodeId(9999), values: vec![0.0; 16] }), Err(IntentError::NotFound(_))));
        assert_eq!((ctl.rev, sent(&engine)), (rev, Vec::<String>::new()), "nothing changed, and nothing was sent");
        assert_eq!(steps_in(&ctl, ModulatorLocation::Free).0, ModSourceSlot::staircase());
    }

    #[test]
    fn a_macro_has_no_rate_and_an_lfo_has_no_value() {
        let (engine, mut ctl) = macros();
        let knob = lane_mod(&ctl, 0);
        let group_knob = group_mod(&ctl, 0);
        sent(&engine);
        for setting in [ModulatorField::Rate, ModulatorField::Waveform, ModulatorField::Attack, ModulatorField::Sustain] {
            assert!(matches!(field(&mut ctl, knob, setting, 0.5), Err(IntentError::Invalid(_))), "{setting:?}");
            assert!(matches!(field(&mut ctl, group_knob, setting, 0.5), Err(IntentError::Invalid(_))), "{setting:?}");
        }
        let (_engine, mut ctl_with_lfo) = modulated();
        let lfo = lane_mod(&ctl_with_lfo, 0);
        let envelope = lane_mod(&ctl_with_lfo, 1);
        for modulator in [lfo, envelope, group_mod(&ctl_with_lfo, 0)] {
            assert!(matches!(field(&mut ctl_with_lfo, modulator, ModulatorField::Value, 0.5), Err(IntentError::Invalid(_))), "{modulator:?}");
        }
        assert_eq!((lane_macro(&ctl), ctl.rev), (0.25, 0), "nothing changed");
        assert_eq!(sent(&engine), Vec::<String>::new());
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
            (lfo, ModulatorField::Waveform, 4.4, 4.0),
            (lfo, ModulatorField::Waveform, 9.0, 5.0),
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
        let before_group = hearing_group(&ctl.model, 0).iter().map(|m| format!("{:?}", m.source)).collect::<Vec<_>>();
        let lfo = lane_mod(&ctl, 0);
        field(&mut ctl, lfo, ModulatorField::Rate, 9.0).unwrap();
        assert_eq!(lane_lfo(&ctl, 3), before_other_lfo, "another lane modulator");
        assert_eq!(hearing_group(&ctl.model, 0).iter().map(|m| format!("{:?}", m.source)).collect::<Vec<_>>(), before_group, "the group's");
        assert_eq!(lane_envelope(&ctl, 1), [0.02, 0.3, 0.6, 0.5]);
        assert_eq!(ctl.model.free_modulators[0].targets.len(), 2, "its targets");
    }

    fn lane_edge(ctl: &Controller, modulator: usize, target: usize) -> EdgeId {
        ctl.model.free_modulators[modulator].targets[target].edge
    }

    fn depth(ctl: &Controller, modulator: usize, target: usize) -> f32 {
        ctl.model.free_modulators[modulator].targets[target].depth
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
            ["SetFreeModTargetDepth mod=0 target=0 depth=0.750", "SetFreeModTargetDepth mod=0 target=1 depth=-0.500"]
        );
        assert!(ctl.dirty);
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
        let before = (depth(&ctl, 0, 1), hearing_group(&ctl.model, 0)[0].targets[0].depth);
        let edge = lane_edge(&ctl, 0, 0);
        ctl.apply(Intent::SetEdgeDepth { edge, depth: 0.9 }).unwrap();
        assert_eq!((depth(&ctl, 0, 1), hearing_group(&ctl.model, 0)[0].targets[0].depth), before);
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
        assert_eq!(sent(&engine), ["RemoveFreeModTarget mod=0 target=0", "SetParameter inst=0 slot=1 param=0 value=1.000"]);
        let rack = &ctl.model.free_modulators;
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
        assert_eq!(sent(&engine), ["RemoveFreeModTarget mod=0 target=1", "SetFreeModulatorRate mod=3 rate=1.000"]);
        assert_eq!(ctl.model.free_modulators[0].targets.len(), 1);
        // The cable of the modulator that hears the group, to its bus filter's cutoff (the lane has four modulators, so that the group's are the fifth and the sixth).
        let bus = hearing_group(&ctl.model, 0)[0].targets[0].edge;
        ctl.apply(Intent::RemoveEdge { edge: bus }).unwrap();
        assert_eq!(sent(&engine), ["RemoveFreeModTarget mod=4 target=0", "SetGroupParameter group=0 fx=0 param=0 value=1.000"]);
        assert!(hearing_group(&ctl.model, 0)[0].targets.is_empty());
        assert_eq!(hearing_group(&ctl.model, 0).len(), 2, "its modulator stays");
        assert_eq!(ctl.rev, 2);
    }

    #[test]
    fn remove_edge_leaves_the_modulator_when_its_last_cable_goes() {
        let (_engine, mut ctl) = modulated();
        let (first, second) = (lane_edge(&ctl, 0, 0), lane_edge(&ctl, 0, 1));
        let modulator = lane_mod(&ctl, 0);
        ctl.apply(Intent::RemoveEdge { edge: first }).unwrap();
        ctl.apply(Intent::RemoveEdge { edge: second }).unwrap();
        assert!(ctl.model.free_modulators[0].targets.is_empty());
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

    // -- audio cables (P3c of docs/patch-graph.md §11.6) --

    fn sends() -> (StubEngine, Controller, Sends) {
        let (handles, engine) = testutil::stub_engine();
        let model = load(SENDS).model;
        let nodes = Sends::of(&model);
        (engine, Controller::new(model, handles, None), nodes)
    }

    fn out(node: NodeId) -> Endpoint {
        Endpoint { node, port: Port::AudioOut }
    }

    fn into(node: NodeId) -> Endpoint {
        Endpoint { node, port: Port::AudioIn }
    }

    fn draw(ctl: &mut Controller, from: Endpoint, to: Endpoint, gain: Option<f32>) -> Result<Applied, IntentError> {
        ctl.apply(Intent::AddCable { from, to, gain })
    }

    #[test]
    fn add_cable_draws_a_send_in_the_model_and_the_engine_and_says_the_structure_changed() {
        let (engine, mut ctl, n) = sends();
        let applied = draw(&mut ctl, out(n.lane[1]), into(n.fx[0]), Some(0.5)).unwrap();
        assert_eq!(applied, Applied::restructured(1));
        assert!(applied.events.is_empty(), "nothing a client could apply: it is told by a snapshot");
        let cable = ctl.model.cables[0];
        assert_eq!((cable.from, cable.to, cable.gain), (CableFrom::Instrument(n.lane[1]), CableTo::Effect(n.fx[0]), 0.5));
        assert_eq!(sent(&engine), [format!("AddCable id={} from=Instrument({:?}) to=Effect({:?}) gain=0.500", cable.id.0, n.lane[1], n.fx[0])]);
        assert!(ctl.dirty);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn add_cable_has_a_start_and_an_end_of_every_kind() {
        let (engine, mut ctl, n) = sends();
        let drawn = [
            (out(n.lane[1]), into(n.fx[0]), CableFrom::Instrument(n.lane[1]), CableTo::Effect(n.fx[0])),
            (out(n.fx[2]), into(n.bus), CableFrom::Effect(n.fx[2]), CableTo::Effect(n.bus)),
            (out(n.fx[1]), into(NodeId::MASTER), CableFrom::Effect(n.fx[1]), CableTo::Master),
            (out(n.fx[0]), into(n.group), CableFrom::Effect(n.fx[0]), CableTo::GroupBus(n.group)),
            (out(n.group), into(NodeId::MASTER), CableFrom::Group(n.group), CableTo::Master),
            (out(n.lane[0]), into(NodeId::MASTER), CableFrom::Instrument(n.lane[0]), CableTo::Master),
        ];
        for (from, to, start, end) in drawn {
            draw(&mut ctl, from, to, None).unwrap();
            let cable = ctl.model.cables.last().unwrap();
            assert_eq!((cable.from, cable.to), (start, end), "{from:?} to {to:?}");
        }
        assert_eq!(ctl.model.cables.len(), 6);
        assert_eq!(sent(&engine).len(), 6);
    }

    #[test]
    fn a_cable_starts_at_unity_and_its_level_is_held_to_nothing_and_twice_that() {
        let (_engine, mut ctl, n) = sends();
        draw(&mut ctl, out(n.lane[0]), into(NodeId::MASTER), None).unwrap();
        draw(&mut ctl, out(n.lane[1]), into(NodeId::MASTER), Some(9.0)).unwrap();
        draw(&mut ctl, out(n.fx[0]), into(NodeId::MASTER), Some(-1.0)).unwrap();
        assert_eq!(ctl.model.cables.iter().map(|cable| cable.gain).collect::<Vec<_>>(), [1.0, 2.0, 0.0]);
    }

    #[test]
    fn a_cable_that_is_refused_changes_nothing_and_uses_no_id() {
        let (engine, mut ctl, n) = sends();
        let refused = [
            (into(n.lane[0]), into(n.fx[0]), None, "starts at an output"),
            (out(n.lane[0]), out(n.fx[0]), None, "ends at an input"),
            (Endpoint { node: n.lane[0], port: Port::Param { index: 0 } }, into(n.fx[0]), None, "starts at an output"),
            (out(n.lane[0]), Endpoint { node: n.fx[0], port: Port::Rate }, None, "ends at an input"),
            (out(NodeId::MASTER), into(n.fx[0]), None, "the mix has no output"),
            (out(n.lane[0]), into(n.lane[1]), None, "a lane has no input"),
            (out(n.lane[0]), into(n.fx[0]), Some(f32::NAN), "not a value"),
            (out(n.fx[1]), into(n.fx[0]), None, "go round"),
            (out(n.fx[0]), into(n.fx[0]), None, "go round"),
            (out(n.group), into(n.fx[0]), None, "go round"),
        ];
        for (from, to, gain, why) in refused {
            let error = draw(&mut ctl, from, to, gain).unwrap_err();
            assert!(error.to_string().contains(why), "{from:?} to {to:?}: {error}");
        }
        assert_eq!(draw(&mut ctl, out(NodeId(900)), into(n.fx[0]), None), Err(IntentError::NotFound(NodeId(900))));
        assert_eq!(draw(&mut ctl, out(n.lane[0]), into(NodeId(901)), None), Err(IntentError::NotFound(NodeId(901))));
        assert!(sent(&engine).is_empty(), "nothing was sent");
        assert_eq!((ctl.rev, ctl.dirty, ctl.model.cables.len()), (0, false, 0));
        // No id was used up by any of them: the first cable that is drawn has the first id.
        draw(&mut ctl, out(n.lane[0]), into(n.fx[2]), None).unwrap();
        assert_eq!(ctl.model.cables[0].id, EdgeId(0));
    }

    // -- a second input on an effect (step 95) --

    fn sided() -> (StubEngine, Controller, Sided) {
        let (handles, engine) = testutil::stub_engine();
        let model = load(SIDED).model;
        let nodes = Sided::of(&model);
        let mut ctl = Controller::new(model, handles, None);
        ctl.catalog.add(plugin::builtin::enumerate_plugins());
        (engine, ctl, nodes)
    }

    fn side(node: NodeId) -> Endpoint {
        Endpoint { node, port: Port::AudioInSide }
    }

    #[test]
    fn a_cable_is_drawn_to_the_second_input_of_an_effect_that_has_one_and_the_graph_says_which_input() {
        let (engine, mut ctl, n) = sided();
        sent(&engine);
        let applied = draw(&mut ctl, out(n.lane[0]), side(n.bus_math), Some(0.5)).unwrap();
        assert_eq!(applied, Applied::restructured(1));
        let cable = *ctl.model.cables.last().unwrap();
        assert_eq!((cable.from, cable.to, cable.gain), (CableFrom::Instrument(n.lane[0]), CableTo::EffectSide(n.bus_math), 0.5));
        assert_eq!(sent(&engine), [format!("AddCable id={} from=Instrument({:?}) to=EffectSide({:?}) gain=0.500", cable.id.0, n.lane[0], n.bus_math)]);
        ctl.model.check_ids().unwrap();
        // The graph has the cable from the output to the second port, and the nodes say that they have one (and what the two are called) or that they have not.
        let graph = ctl.model.graph();
        let edge = graph.edges.iter().find(|edge| matches!(edge.kind, crate::app::graph::EdgeKind::Cable { id, .. } if id == cable.id)).expect("the cable is an edge");
        assert_eq!((edge.from, edge.to), (out(n.lane[0]), side(n.bus_math)));
        let sides = |node: NodeId| match &graph.nodes.iter().find(|candidate| candidate.id == node).unwrap().kind {
            NodeKind::Effect { side, .. } => side.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(sides(n.math), Some(crate::app::graph::SideInfo { main: "A".to_string(), side: "B".to_string() }));
        assert_eq!(sides(n.bus_math), sides(n.math));
        assert_eq!(sides(n.filter), None);
        // The cable from the file is the same: at the second input of lane 0's math effect.
        assert!(graph.edges.iter().any(|edge| (edge.from, edge.to) == (out(n.lane[1]), side(n.math))));
    }

    #[test]
    fn the_second_input_takes_a_cable_beside_the_one_at_the_first_and_a_cable_between_the_same_two_places_only_once() {
        let (engine, mut ctl, n) = sided();
        sent(&engine);
        // The file has lane 1's instrument at the second input; the first input of the same effect is another end.
        draw(&mut ctl, out(n.lane[1]), into(n.math), None).unwrap();
        assert_eq!(ctl.model.cables.len(), 2);
        let error = draw(&mut ctl, out(n.lane[1]), side(n.math), None).unwrap_err();
        assert!(error.to_string().contains("already"), "{error}");
        // Another sender is another cable.
        draw(&mut ctl, out(n.filter), side(n.math), None).unwrap();
        assert_eq!(ctl.model.cables.len(), 3);
    }

    #[test]
    fn the_second_input_is_refused_where_the_effect_has_none_and_for_what_is_no_effect_and_nothing_is_changed() {
        let (engine, mut ctl, n) = sided();
        sent(&engine);
        let refused = [
            (out(n.lane[0]), side(n.filter), "that effect has no second input"),
            (out(n.lane[0]), side(NodeId::MASTER), "the mix has no second input"),
            (out(n.lane[0]), side(n.group), "a group has one input"),
            (out(n.lane[1]), side(n.lane[0]), "a lane has no input of audio"),
            (side(n.math), into(NodeId::MASTER), "starts at an output"),
            (out(n.math), side(n.math), "go round"),
            (out(n.group), side(n.bus_math), "go round"),
        ];
        let ids = ctl.model.cables.iter().map(|cable| cable.id).collect::<Vec<_>>();
        for (from, to, why) in refused {
            let error = draw(&mut ctl, from, to, None).unwrap_err();
            assert!(error.to_string().contains(why), "{from:?} to {to:?}: {error}");
        }
        assert_eq!(draw(&mut ctl, out(n.lane[0]), side(NodeId(901)), None), Err(IntentError::NotFound(NodeId(901))));
        assert!(sent(&engine).is_empty(), "nothing was sent");
        assert_eq!(ctl.model.cables.iter().map(|cable| cable.id).collect::<Vec<_>>(), ids);
        // A cable of modulation does not end at an input of audio, either.
        let modulator = ctl.apply(Intent::AddFreeModulator { kind: ModulatorKind::Lfo, hears: None }).unwrap();
        let _ = modulator;
        let lfo = ctl.model.free_modulators[0].node;
        let error = ctl.apply(Intent::AddModEdge { modulator: lfo, to: side(n.math) }).unwrap_err();
        assert!(error.to_string().contains("a cable can only end at a plugin's parameter"), "{error}");
    }

    #[test]
    fn the_level_of_a_cable_at_the_second_input_is_set_and_the_cable_is_taken_out_as_any_cable_is() {
        let (engine, mut ctl, n) = sided();
        let edge = ctl.model.cables[0].id;
        sent(&engine);
        ctl.apply(Intent::SetCableGain { edge, gain: 0.25 }).unwrap();
        assert_eq!(ctl.model.cables[0].gain, 0.25);
        assert_eq!(sent(&engine), [format!("SetCableGain id={} gain=0.250", edge.0)]);
        ctl.apply(Intent::RemoveEdge { edge }).unwrap();
        assert!(ctl.model.cables.is_empty());
        assert_eq!(sent(&engine), [format!("RemoveCable id={}", edge.0)]);
        let _ = n;
    }

    #[test]
    fn a_cable_at_the_second_input_is_a_step_of_the_history_that_undo_takes_out_and_redo_draws_again_under_its_id_and_level() {
        let (engine, mut ctl, n) = sided();
        sent(&engine);
        draw(&mut ctl, out(n.lane[0]), side(n.bus_math), Some(0.75)).unwrap();
        let made = *ctl.model.cables.last().unwrap();
        assert_eq!(ctl.history_state().undo.as_deref(), Some("draw an audio cable"));
        ctl.undo().unwrap();
        assert_eq!(ctl.model.cables.len(), 1, "only the file's is left");
        ctl.redo().unwrap();
        assert_eq!(*ctl.model.cables.last().unwrap(), made, "the same cable, under its id and at its level, at the same end");
        assert_eq!(made.to, CableTo::EffectSide(n.bus_math));
        sent(&engine);
        // Taking it out is a step too, and undo puts it back whole.
        ctl.apply(Intent::RemoveEdge { edge: made.id }).unwrap();
        assert_eq!(ctl.history_state().undo.as_deref(), Some("remove an audio cable"));
        ctl.undo().unwrap();
        assert_eq!(*ctl.model.cables.last().unwrap(), made);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn an_effect_that_has_a_second_input_takes_the_cables_at_it_out_with_it_and_undo_gives_them_back_whole() {
        let (engine, mut ctl, n) = sided();
        let file_cable = ctl.model.cables[0];
        assert_eq!(file_cable.to, CableTo::EffectSide(n.math));
        sent(&engine);
        ctl.apply(Intent::RemoveNode { node: n.math }).unwrap();
        assert!(ctl.model.cables.is_empty(), "the cable at its second input went with it");
        assert!(sent(&engine).iter().any(|command| command.starts_with("RemoveCable")), "{:?}", sent(&engine));
        ctl.undo().unwrap();
        assert_eq!(ctl.model.cables, [file_cable], "back, under its id, at the second input of the same effect");
        assert_eq!(ctl.model.plugin_slot(n.math).unwrap().side, Some(crate::plugin::SideInput { main: "A", side: "B" }), "and the effect has its second input");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_math_effect_that_is_added_has_a_second_input_and_one_that_stands_by_itself_takes_a_cable_there_at_once() {
        let (engine, mut ctl, n) = sided();
        sent(&engine);
        ctl.apply(Intent::AddFreeEffect { plugin: "builtin:math".to_string() }).unwrap();
        let lane = ctl.model.instruments.last().unwrap();
        let free = lane.effects[0].node;
        assert_eq!(lane.effects[0].side, Some(crate::plugin::SideInput { main: "A", side: "B" }));
        draw(&mut ctl, out(n.lane[0]), into(free), None).unwrap();
        draw(&mut ctl, out(n.lane[1]), side(free), None).unwrap();
        assert_eq!(ctl.model.cables.last().unwrap().to, CableTo::EffectSide(free));
    }

    #[test]
    fn a_cable_is_drawn_to_the_second_input_of_a_math_effect_that_an_undo_loads_again() {
        // An effect that undo has the plugin of again (the history has let go of it) is loaded from its id: it has its second input.
        let (engine, mut ctl, n) = sided();
        sent(&engine);
        ctl.apply(Intent::RemoveNode { node: n.bus_math }).unwrap();
        ctl.undo().unwrap();
        assert_eq!(ctl.model.plugin_slot(n.bus_math).unwrap().side, Some(crate::plugin::SideInput { main: "A", side: "B" }));
        draw(&mut ctl, out(n.lane[0]), side(n.bus_math), None).unwrap();
    }

    #[test]
    fn a_cable_that_is_drawn_between_the_ends_of_a_link_gives_it_back_and_one_at_the_second_input_is_never_a_link() {
        let (_engine, mut ctl, n) = sided();
        // The filter feeds the math effect, which is the link between them: a cable at the second input from the filter is a cable of its own, beside the link.
        assert!(ctl.model.template_link(CableFrom::Effect(n.filter), CableTo::EffectSide(n.math)).is_none());
        draw(&mut ctl, out(n.filter), side(n.math), None).unwrap();
        assert_eq!(ctl.model.cables.last().unwrap().to, CableTo::EffectSide(n.math));
        assert!(ctl.model.template_link(CableFrom::Effect(n.filter), CableTo::Effect(n.math)).is_some_and(|link| link.present), "and the link is there as it was");
    }

    #[test]
    fn a_cable_that_is_there_is_not_drawn_again_between_the_same_two_ports() {
        let (engine, mut ctl, n) = sends();
        draw(&mut ctl, out(n.lane[0]), into(n.fx[2]), None).unwrap();
        sent(&engine);
        let error = draw(&mut ctl, out(n.lane[0]), into(n.fx[2]), Some(0.5)).unwrap_err();
        assert!(error.to_string().contains("already"), "{error}");
        assert!(sent(&engine).is_empty());
        assert_eq!(ctl.model.cables.len(), 1);
        // Another end, or another start, is another cable.
        draw(&mut ctl, out(n.lane[0]), into(NodeId::MASTER), None).unwrap();
        draw(&mut ctl, out(n.lane[1]), into(n.fx[1]), None).unwrap();
        assert_eq!(ctl.model.cables.len(), 3);
    }

    #[test]
    fn a_cable_drawn_between_the_ends_of_a_link_that_is_cut_gives_the_link_back_and_makes_no_cable() {
        let (engine, mut ctl, n) = sends();
        // Lane 1 is in no group: its drive's way out goes to the mix, and its instrument feeds the drive. The group's bus effect is fed by the group.
        ctl.apply(Intent::SetRoute { node: n.lane[1], route: Route::None }).unwrap();
        ctl.apply(Intent::SetFeed { node: n.fx[2], feed: Feed::None }).unwrap();
        ctl.apply(Intent::SetFeed { node: n.bus, feed: Feed::None }).unwrap();
        ctl.apply(Intent::SetRoute { node: n.group, route: Route::None }).unwrap();
        sent(&engine);
        let rev = ctl.rev;

        // The end of lane 1's chain to the mix: its way out.
        let applied = draw(&mut ctl, out(n.fx[2]), into(NodeId::MASTER), None).unwrap();
        assert_eq!(applied, Applied::restructured(rev + 1));
        assert!(applied.events.is_empty(), "told by a snapshot, as `SetRoute` is");
        assert_eq!(sent(&engine), ["SetLaneRoute inst=1 route=out"]);
        // The instrument of lane 1 to its drive: the drive's feed.
        draw(&mut ctl, out(n.lane[1]), into(n.fx[2]), Some(1.0)).unwrap();
        assert_eq!(sent(&engine), [format!("SetEffectFeed node={} fed=true", n.fx[2].0)]);
        // The group to its bus effect (a cable that starts at the group, and would go round if it were one: it is the link into the effect), and the end of its chain to the mix.
        draw(&mut ctl, out(n.group), into(n.bus), None).unwrap();
        draw(&mut ctl, out(n.bus), into(NodeId::MASTER), None).unwrap();
        assert_eq!(sent(&engine), [format!("SetEffectFeed node={} fed=true", n.bus.0), "SetGroupRoute group=0 route=out".to_string()]);
        assert_eq!(ctl.model.template_links().iter().filter(|link| !link.present).count(), 0, "every link is there");
        assert!(ctl.model.cables.is_empty(), "none of them made a cable: they are the links");
        assert_eq!(ctl.rev, rev + 4);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_cable_drawn_between_the_ends_of_a_link_that_is_there_is_refused_as_one_between_the_two_is() {
        let (engine, mut ctl, n) = sends();
        let links = [
            (out(n.lane[0]), into(n.fx[0])),
            (out(n.fx[0]), into(n.fx[1])),
            (out(n.fx[1]), into(n.group)),
            (out(n.lane[1]), into(n.fx[2])),
            (out(n.fx[2]), into(NodeId::MASTER)),
            (out(n.group), into(n.bus)),
            (out(n.bus), into(NodeId::MASTER)),
        ];
        for (from, to) in links {
            for gain in [None, Some(1.0)] {
                assert_eq!(draw(&mut ctl, from, to, gain), Err(invalid("there is a cable between those two already")), "{from:?} to {to:?}");
            }
        }
        assert!(sent(&engine).is_empty());
        assert_eq!((ctl.rev, ctl.dirty, ctl.model.cables.len()), (0, false, 0), "a refusal is nothing");
    }

    #[test]
    fn a_cable_at_another_level_than_the_links_is_a_cable_beside_the_link() {
        let (engine, mut ctl, n) = sends();
        // A link has no level but the plain one. Asked at another, it is a send, and the link is as it was.
        draw(&mut ctl, out(n.fx[2]), into(NodeId::MASTER), Some(0.5)).unwrap();
        assert_eq!(ctl.model.cables.len(), 1);
        assert_eq!((ctl.model.cables[0].from, ctl.model.cables[0].to, ctl.model.cables[0].gain), (CableFrom::Effect(n.fx[2]), CableTo::Master, 0.5));
        assert_eq!(ctl.model.instruments[1].route, Route::Out);
        assert!(sent(&engine)[0].starts_with("AddCable"));
        // And a cable that is there between the two makes the link no part of it: the cable's own refusal.
        ctl.apply(Intent::SetRoute { node: n.lane[1], route: Route::None }).unwrap();
        sent(&engine);
        let error = draw(&mut ctl, out(n.fx[2]), into(NodeId::MASTER), None).unwrap_err();
        assert!(error.to_string().contains("already"), "{error}");
        assert_eq!(ctl.model.instruments[1].route, Route::None, "the link is not given back under a cable that is there");
        assert!(sent(&engine).is_empty());
    }

    #[test]
    fn a_link_that_a_cable_gives_back_is_refused_where_the_audio_would_go_round_as_the_intents_are() {
        let (engine, mut ctl, n) = sends();
        // The group's output goes to the drive of lane 1; with the way out of lane 1 cut, the lane can be in the group and feeds nothing.
        draw(&mut ctl, out(n.group), into(n.fx[2]), None).unwrap();
        ctl.apply(Intent::SetRoute { node: n.lane[1], route: Route::None }).unwrap();
        ctl.apply(Intent::SetLaneGroup { node: n.lane[1], group: Some(n.group) }).unwrap();
        sent(&engine);
        let rev = ctl.rev;
        // A cable from the end of the lane's chain to its group's bus is its way out, given back: it would feed the group that feeds it.
        let error = draw(&mut ctl, out(n.fx[2]), into(n.group), None).unwrap_err();
        assert!(error.to_string().contains("go round"), "{error}");
        assert!(sent(&engine).is_empty());
        assert_eq!((ctl.rev, ctl.model.instruments[1].route, ctl.model.cables.len()), (rev, Route::None, 1));
    }

    #[test]
    fn an_end_that_the_controller_does_not_take_is_judged_as_a_cable_in_the_cables_words_whether_or_not_a_link_joins_the_nodes() {
        let (_engine, mut ctl, n) = sends();
        ctl.apply(Intent::SetRoute { node: n.lane[1], route: Route::None }).unwrap();
        // (An end that the controller does not take is judged as a cable, in the cable's words, whether or not a link joins the nodes.)
        assert_eq!(draw(&mut ctl, out(n.fx[2]), into(NodeId(901)), None), Err(IntentError::NotFound(NodeId(901))));
        assert_eq!(draw(&mut ctl, out(n.fx[2]), into(n.lane[1]), None), Err(invalid("a lane has no input of audio: a cable ends at an effect, a group or the mix")));
        assert_eq!(ctl.model.instruments[1].route, Route::None);
    }

    fn level(ctl: &Controller, edge: EdgeId) -> f32 {
        ctl.model.audio_cable(edge).unwrap().gain
    }

    #[test]
    fn set_cable_gain_sets_the_level_in_the_model_and_the_engine_and_tells_it() {
        let (engine, mut ctl, n) = sends();
        draw(&mut ctl, out(n.lane[0]), into(n.fx[2]), None).unwrap();
        let edge = ctl.model.cables[0].id;
        sent(&engine);
        let applied = ctl.apply(Intent::SetCableGain { edge, gain: 0.25 }).unwrap();
        assert_eq!(applied, Applied::of(2, vec![Event::CableGainSet { edge, gain: 0.25 }]));
        assert_eq!(level(&ctl, edge), 0.25);
        assert_eq!(sent(&engine), [format!("SetCableGain id={} gain=0.250", edge.0)]);
    }

    #[test]
    fn set_cable_gain_clamps_to_nothing_and_twice_that_and_refuses_what_is_not_a_cable_or_a_number() {
        let (engine, mut ctl, n) = sends();
        draw(&mut ctl, out(n.lane[0]), into(n.fx[2]), None).unwrap();
        let edge = ctl.model.cables[0].id;
        let gain_of = |applied: Applied| match applied.events[0] {
            Event::CableGainSet { gain, .. } => gain,
            ref other => panic!("{other:?}"),
        };
        assert_eq!(gain_of(ctl.apply(Intent::SetCableGain { edge, gain: 9.0 }).unwrap()), 2.0);
        assert_eq!(gain_of(ctl.apply(Intent::SetCableGain { edge, gain: -9.0 }).unwrap()), 0.0);
        sent(&engine);
        let rev = ctl.rev;
        for gain in [f32::NAN, f32::INFINITY] {
            let error = ctl.apply(Intent::SetCableGain { edge, gain }).unwrap_err();
            assert!(error.to_string().contains("not a value"), "{gain}: {error}");
        }
        assert_eq!(ctl.apply(Intent::SetCableGain { edge: EdgeId(9_999), gain: 0.5 }).unwrap_err(), IntentError::EdgeNotFound(EdgeId(9_999)));
        assert!(sent(&engine).is_empty());
        assert_eq!((ctl.rev, level(&ctl, edge)), (rev, 0.0));
    }

    #[test]
    fn a_modulation_cable_has_a_depth_and_an_audio_cable_a_level_and_neither_takes_the_others_intent() {
        let (_engine, mut ctl) = modulated();
        let (depth_edge, had) = (lane_edge(&ctl, 0, 0), depth(&ctl, 0, 0));
        let error = ctl.apply(Intent::SetCableGain { edge: depth_edge, gain: 0.5 }).unwrap_err();
        assert!(error.to_string().contains("depth"), "{error}");
        assert_eq!(depth(&ctl, 0, 0), had, "what it had");
        let (_engine, mut ctl, n) = sends();
        draw(&mut ctl, out(n.lane[0]), into(n.fx[2]), None).unwrap();
        let edge = ctl.model.cables[0].id;
        let error = ctl.apply(Intent::SetEdgeDepth { edge, depth: 0.5 }).unwrap_err();
        assert!(error.to_string().contains("level"), "{error}");
        assert_eq!(level(&ctl, edge), 1.0);
    }

    #[test]
    fn remove_edge_takes_an_audio_cable_out_of_the_model_and_the_engine_and_gives_nothing_back() {
        let (engine, mut ctl, n) = sends();
        draw(&mut ctl, out(n.lane[0]), into(n.fx[2]), None).unwrap();
        draw(&mut ctl, out(n.lane[1]), into(NodeId::MASTER), Some(0.5)).unwrap();
        let (first, second) = (ctl.model.cables[0].id, ctl.model.cables[1].id);
        sent(&engine);
        let applied = ctl.apply(Intent::RemoveEdge { edge: first }).unwrap();
        assert_eq!(applied, Applied::restructured(3));
        assert_eq!(sent(&engine), [format!("RemoveCable id={}", first.0)]);
        assert_eq!(ctl.model.cables.iter().map(|cable| cable.id).collect::<Vec<_>>(), [second], "the other is as it was");
        assert_eq!(level(&ctl, second), 0.5);
        // It is not there the second time.
        assert_eq!(ctl.apply(Intent::RemoveEdge { edge: first }).unwrap_err(), IntentError::EdgeNotFound(first));
        assert!(sent(&engine).is_empty());
        assert_eq!(ctl.rev, 3, "a refusal is not a revision");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_lane_is_not_put_in_a_group_that_would_make_the_audio_go_round_and_nothing_changes() {
        let (engine, mut ctl, n) = sends();
        draw(&mut ctl, out(n.group), into(n.fx[2]), None).unwrap();
        sent(&engine);
        let rev = ctl.rev;
        let error = ctl.apply(Intent::SetLaneGroup { node: n.lane[1], group: Some(n.group) }).unwrap_err();
        assert!(error.to_string().contains("go round"), "{error}");
        assert!(sent(&engine).is_empty());
        assert_eq!((ctl.rev, ctl.model.instruments[1].group), (rev, None));
        // A lane that may go in does, and taking one out is not looked at.
        ctl.apply(Intent::SetLaneGroup { node: n.lane[0], group: None }).unwrap();
        assert_eq!(ctl.model.instruments[0].group, None);
    }

    #[test]
    fn an_effect_is_not_moved_to_where_the_audio_would_go_round_and_nothing_changes() {
        let (engine, mut ctl, n) = sends();
        draw(&mut ctl, out(n.fx[0]), into(n.fx[2]), None).unwrap();
        draw(&mut ctl, out(n.fx[2]), into(n.fx[1]), None).unwrap();
        sent(&engine);
        let rev = ctl.rev;
        let error = ctl.apply(Intent::MoveEffect { node: n.fx[1], index: 0 }).unwrap_err();
        assert!(error.to_string().contains("go round"), "{error}");
        assert!(sent(&engine).is_empty());
        assert_eq!((ctl.rev, ctl.model.instruments[0].effects[0].node), (rev, n.fx[0]));
    }

    #[test]
    fn a_bus_effect_is_not_moved_to_where_the_audio_would_go_round_and_nothing_changes() {
        let (engine, mut ctl) = chains();
        let (filter, reverb) = (ctl.model.groups[0].effects[0].node, ctl.model.groups[0].effects[1].node);
        // The filter sends into the reverb that is after it, which is no loop; with the filter after the reverb the audio would go round. (At a level that is not the link's: the link between the
        // two is there, and a cable at its level would be the link.)
        draw(&mut ctl, out(filter), into(reverb), Some(0.5)).unwrap();
        sent(&engine);
        let rev = ctl.rev;
        let error = ctl.apply(Intent::MoveEffect { node: filter, index: 1 }).unwrap_err();
        assert!(error.to_string().contains("go round"), "{error}");
        assert!(sent(&engine).is_empty());
        assert_eq!((ctl.rev, ctl.model.groups[0].effects[0].node), (rev, filter));
    }

    /// `testutil::PATCHED` (a bus of two effects, three lanes, a modulator and six audio cables, some of which make a loop of what a lane or an effect is put at) over the stub engine.
    fn patched() -> (StubEngine, Controller) {
        let (handles, engine) = testutil::stub_engine();
        (engine, Controller::new(load(testutil::PATCHED).model, handles, None))
    }

    /// The session of `patched()` and what the controller makes of the intents about audio cables, as the web client's tests read it
    /// (`web/src/client/fixtures/audioCables.json`): they pin the mock engine's to the controller's, intent by intent. Every start is tried with every end (the master, a node that
    /// is not there and a modulator among them), the ports that are not audio's, levels, the level and the depth of each kind of cable, the cables taken out, every node that has
    /// cables taken out, every lane put in a group and out of it and every effect moved to every place of its chain (the cables there are make some of those go round). A refusal is
    /// the controller's words; a cable that is drawn and a level that is set give the audio cables that the session has after it (`sends`: nothing else changes, and the test of the
    /// client says so), and the rest what the session comes to (`testutil::shape_of`). `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` rewrites it.
    #[test]
    fn the_web_clients_fixture_of_what_audio_cables_come_to_is_current() {
        let (_engine, ctl) = patched();
        let before = ctl.snapshot();
        let m = &ctl.model;
        let lanes = [m.instruments[0].node, m.instruments[1].node, m.instruments[2].node];
        let fx = [m.instruments[0].effects[0].node, m.instruments[0].effects[1].node, m.instruments[1].effects[0].node];
        let (group, bus) = (m.groups[0].node, [m.groups[0].effects[0].node, m.groups[0].effects[1].node, m.groups[0].effects[2].node]);
        let lfo = hearing_lane(m, 0)[0].node;
        let modulation = hearing_lane(m, 0)[0].targets[0].edge;
        let cables: Vec<EdgeId> = m.cables.iter().map(|cable| cable.id).collect();
        assert_eq!(cables.len(), 6);
        let ghost = NodeId(9_999);
        let everything = [lanes[0], lanes[1], lanes[2], fx[0], fx[1], fx[2], group, bus[0], bus[1], bus[2], lfo, NodeId::MASTER, ghost];

        let mut intents: Vec<Intent> = Vec::new();
        for from in everything {
            for to in everything {
                intents.push(Intent::AddCable { from: out(from), to: into(to), gain: None });
            }
        }
        // A start that is not an output of audio and an end that is not an input of audio.
        for port in [Port::AudioIn, Port::ModOut, Port::Param { index: 0 }, Port::Rate] {
            intents.push(Intent::AddCable { from: Endpoint { node: lanes[1], port }, to: into(fx[1]), gain: None });
        }
        for port in [Port::AudioOut, Port::ModOut, Port::Param { index: 0 }, Port::Depth { edge: modulation }] {
            intents.push(Intent::AddCable { from: out(lanes[1]), to: Endpoint { node: fx[1], port }, gain: None });
        }
        // Levels: held to nothing and to twice.
        for gain in [0.0, 0.25, 1.0, 2.0, 2.5, 9.0, -1.0] {
            intents.push(Intent::AddCable { from: out(lanes[1]), to: into(fx[1]), gain: Some(gain) });
        }
        // The level of each cable, of a cable of modulation and of one that is not there; the depth of the first two audio cables and of the cable of modulation.
        for edge in cables.iter().copied().chain([modulation, EdgeId(9_999)]) {
            for gain in [0.0, 0.25, 2.0, 9.0, -3.0] {
                intents.push(Intent::SetCableGain { edge, gain });
            }
        }
        for edge in [cables[0], cables[1], modulation] {
            intents.push(Intent::SetEdgeDepth { edge, depth: 0.5 });
        }
        for edge in cables.iter().copied().chain([modulation, EdgeId(9_999)]) {
            intents.push(Intent::RemoveEdge { edge });
        }
        // Every node taken out (the cables at it go).
        for node in everything {
            intents.push(Intent::RemoveNode { node });
        }
        // Every lane in the group and out of it; every effect to every place of its chain.
        for lane in lanes {
            for group in [None, Some(group)] {
                intents.push(Intent::SetLaneGroup { node: lane, group });
            }
        }
        for (node, places) in [(fx[0], 2), (fx[1], 2), (fx[2], 1), (bus[0], 3), (bus[1], 3), (bus[2], 3)] {
            for index in 0..places {
                intents.push(Intent::MoveEffect { node, index });
            }
        }

        let mut rows = Vec::new();
        for intent in intents {
            let (_engine, mut fresh) = patched();
            let what = testutil::wire(&intent);
            let row = match fresh.apply(intent.clone()) {
                Err(error) => serde_json::json!({ "intent": what, "error": error.to_string() }),
                Ok(applied) => {
                    let after = testutil::shape_of(&fresh.snapshot());
                    match intent {
                        // (A cable that is drawn changes the structure, and a level is an event: nothing but the audio cables is not what it was.)
                        Intent::AddCable { .. } => {
                            assert!(applied.restructured, "{intent:?}");
                            serde_json::json!({ "intent": what, "sends": after.get("sends").cloned().unwrap_or(serde_json::json!([])) })
                        }
                        Intent::SetCableGain { .. } => {
                            assert!(matches!(applied.events.as_slice(), [Event::CableGainSet { .. }]), "{intent:?}");
                            serde_json::json!({ "intent": what, "sends": after.get("sends").cloned().unwrap_or(serde_json::json!([])) })
                        }
                        _ => {
                            // (The depth of a cable of modulation is the one thing here that is an event and no change of structure.)
                            assert_eq!(applied.restructured, !matches!(intent, Intent::SetEdgeDepth { .. }), "{intent:?}");
                            serde_json::json!({ "intent": what, "after": after })
                        }
                    }
                }
            };
            rows.push(row);
        }
        // The same session with every link of the audio path cut (the way out of every lane and group, the input of every effect and bus effect), and every start tried with every end again, each on a
        // session of its own (step 88): a cable between the two ends of a link that is cut gives the link back, where the audio would not go round (and not by the rule of a cable: the group's own link into
        // its first bus effect would go round as a cable), and a cable between the ends of one that is there, now cut, is one of the six that are drawn. What is kept of a session that took one is its links and
        // what is cut, the nodes' own words that say it (`audio`, `routes`, `feeds`) and the audio cables there are after it (`sends`).
        let cut = || {
            let (engine, mut ctl) = patched();
            for node in [lanes[0], lanes[1], lanes[2], group] {
                ctl.apply(Intent::SetRoute { node, route: Route::None }).unwrap();
            }
            for node in [fx[0], fx[1], fx[2], bus[0], bus[1], bus[2]] {
                ctl.apply(Intent::SetFeed { node, feed: Feed::None }).unwrap();
            }
            (engine, ctl)
        };
        let mut cut_rows = Vec::new();
        for from in everything {
            for to in everything {
                let (_engine, mut fresh) = cut();
                let intent = Intent::AddCable { from: out(from), to: into(to), gain: None };
                let what = testutil::wire(&intent);
                cut_rows.push(match fresh.apply(intent) {
                    Err(error) => serde_json::json!({ "intent": what, "error": error.to_string() }),
                    Ok(applied) => {
                        assert!(applied.restructured);
                        let after = testutil::shape_of(&fresh.snapshot());
                        let kept = |key: &str| after.get(key).cloned().unwrap_or(serde_json::json!([]));
                        serde_json::json!({ "intent": what, "links": { "audio": kept("audio"), "routes": kept("routes"), "feeds": kept("feeds"), "sends": kept("sends") } })
                    }
                });
            }
        }
        let (_engine, cut_ctl) = cut();
        let mut cut_snapshot = testutil::wire(&cut_ctl.snapshot());
        cut_snapshot["telemetry"]["rev"] = serde_json::json!(1);

        let mut snapshot = testutil::wire(&before);
        // The layout's fingerprint comes from the hasher, which the standard library does not promise to keep the same.
        snapshot["telemetry"]["rev"] = serde_json::json!(1);
        let fixture = serde_json::json!({ "snapshot": snapshot, "rows": rows, "cutSnapshot": cut_snapshot, "cutRows": cut_rows });
        let fresh = serde_json::to_string_pretty(&fixture).unwrap() + "\n";

        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/src/client/fixtures/audioCables.json");
        if std::env::var_os("TANG_UPDATE_TYPES").is_some() {
            std::fs::write(&path, &fresh).unwrap();
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            committed == fresh,
            "{} is not what audio cables come to now: run `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` and commit the result",
            path.display()
        );
    }

    /// `testutil::SIDED` with a free LFO and two cables more at the second input of lane 0's math effect (one from the group's output, one from the filter before it), over the stub engine:
    /// the session of the fixture of the second input, in which moving the effect before the filter and putting lane 0 in the group are loops.
    fn sided_and_wired() -> (StubEngine, Controller, Sided, NodeId) {
        let (engine, mut ctl, n) = sided();
        ctl.apply(Intent::AddFreeModulator { kind: ModulatorKind::Lfo, hears: None }).unwrap();
        let lfo = ctl.model.free_modulators[0].node;
        draw(&mut ctl, out(n.group), side(n.math), None).unwrap();
        draw(&mut ctl, out(n.filter), side(n.math), None).unwrap();
        (engine, ctl, n, lfo)
    }

    /// The session of `sided_and_wired()` and what the controller makes of the intents about the second input of an effect, as the web client's tests read it
    /// (`web/src/client/fixtures/sideCables.json`): they pin the mock engine's to the controller's, intent by intent. Every start is tried with both inputs of every end (the master, a node that is
    /// not there and a modulator among them), the ports that are not audio's, levels, the level of each cable and the cables taken out, every node taken out (the cables at its inputs go), every lane
    /// put in the group and out of it and every effect moved to every place of its chain (the cables at the second input make some of those go round), and a math effect, a lane, a group and effects added. A refusal is the controller's
    /// words; a cable that is drawn and a level that is set give the audio cables that the session has after it (`sends`, with the word `side` after the nodes of a cable at a second input), and
    /// the rest what the session comes to (`testutil::shape_of`). `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` rewrites it.
    #[test]
    fn the_web_clients_fixture_of_what_a_second_input_comes_to_is_current() {
        let (_engine, ctl, n, lfo) = sided_and_wired();
        let before = ctl.snapshot();
        let cables: Vec<EdgeId> = ctl.model.cables.iter().map(|cable| cable.id).collect();
        assert_eq!(cables.len(), 3);
        let ghost = NodeId(9_999);
        let everything = [n.lane[0], n.lane[1], n.filter, n.math, n.group, n.bus_math, lfo, NodeId::MASTER, ghost];

        let mut intents: Vec<Intent> = Vec::new();
        for from in everything {
            for to in everything {
                intents.push(Intent::AddCable { from: out(from), to: into(to), gain: None });
                intents.push(Intent::AddCable { from: out(from), to: side(to), gain: None });
            }
        }
        // A start that is not an output of audio, an end that is not an input of audio, and a second input as a start.
        for port in [Port::AudioIn, Port::AudioInSide, Port::ModOut, Port::Param { index: 0 }] {
            intents.push(Intent::AddCable { from: Endpoint { node: n.lane[1], port }, to: side(n.bus_math), gain: None });
        }
        for port in [Port::AudioOut, Port::ModOut, Port::Param { index: 0 }, Port::Rate] {
            intents.push(Intent::AddCable { from: out(n.lane[1]), to: Endpoint { node: n.bus_math, port }, gain: None });
        }
        // Levels at a second input: held to nothing and to twice.
        for gain in [0.0, 0.25, 1.0, 2.0, 2.5, 9.0, -1.0] {
            intents.push(Intent::AddCable { from: out(n.lane[0]), to: side(n.bus_math), gain: Some(gain) });
        }
        for edge in cables.iter().copied() {
            for gain in [0.0, 0.25, 2.0, 9.0, -3.0] {
                intents.push(Intent::SetCableGain { edge, gain });
            }
            intents.push(Intent::RemoveEdge { edge });
        }
        for node in everything {
            intents.push(Intent::RemoveNode { node });
        }
        for lane in n.lane {
            for group in [None, Some(n.group)] {
                intents.push(Intent::SetLaneGroup { node: lane, group });
            }
        }
        for (node, places) in [(n.filter, 2), (n.math, 2), (n.bus_math, 1)] {
            for index in 0..places {
                intents.push(Intent::MoveEffect { node, index });
            }
        }
        // A math effect that stands by itself: a lane with the effect in it, whose second input is another end. And a lane and a group, which the graph lists before the free modulator (a lane after the other
        // lanes, a group before them) and the page's mock engine once listed after it.
        intents.push(Intent::AddFreeEffect { plugin: "builtin:math".to_string() });
        intents.push(Intent::AddInstrument { plugin: "builtin:osc".to_string() });
        intents.push(Intent::AddGroup { members: vec![n.lane[0]] });
        intents.push(Intent::AddEffect { parent: n.lane[1], plugin: "builtin:math".to_string() });
        intents.push(Intent::AddEffect { parent: n.group, plugin: "builtin:filter".to_string() });

        let links_before = testutil::shape_of(&before).get("audio").cloned();
        let mut rows = Vec::new();
        for intent in intents {
            let (_engine, mut fresh, _, _) = sided_and_wired();
            let what = testutil::wire(&intent);
            let row = match fresh.apply(intent.clone()) {
                Err(error) => serde_json::json!({ "intent": what, "error": error.to_string() }),
                Ok(applied) => {
                    let after = testutil::shape_of(&fresh.snapshot());
                    match intent {
                        // (A cable that is drawn between the two ends of a link that is cut gives the link back, which changes the links of the audio path: then it is what the session comes to that is kept.)
                        Intent::AddCable { .. } if after.get("audio") == links_before.as_ref() => {
                            assert!(applied.restructured, "{intent:?}");
                            serde_json::json!({ "intent": what, "sends": after.get("sends").cloned().unwrap_or(serde_json::json!([])) })
                        }
                        Intent::SetCableGain { .. } => {
                            assert!(matches!(applied.events.as_slice(), [Event::CableGainSet { .. }]), "{intent:?}");
                            serde_json::json!({ "intent": what, "sends": after.get("sends").cloned().unwrap_or(serde_json::json!([])) })
                        }
                        _ => {
                            assert!(applied.restructured, "{intent:?}");
                            serde_json::json!({ "intent": what, "after": after })
                        }
                    }
                }
            };
            rows.push(row);
        }

        let mut snapshot = testutil::wire(&before);
        // The layout's fingerprint comes from the hasher, which the standard library does not promise to keep the same.
        snapshot["telemetry"]["rev"] = serde_json::json!(1);
        let fixture = serde_json::json!({ "snapshot": snapshot, "rows": rows });
        let fresh = serde_json::to_string_pretty(&fixture).unwrap() + "\n";

        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/src/client/fixtures/sideCables.json");
        if std::env::var_os("TANG_UPDATE_TYPES").is_some() {
            std::fs::write(&path, &fresh).unwrap();
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            committed == fresh,
            "{} is not what a second input comes to now: run `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` and commit the result",
            path.display()
        );
    }

    /// When each parameter of the math effects of `sided_and_wired()` has an effect, as the web client's tests read it (`web/src/client/fixtures/uses.json`): they pin the page's `unusedBecause` to the model's
    /// `PluginSlot::unused_because`. The graph that the page reads is the snapshot of `sideCables.json` (the same session): lane 0's math effect has three cables at its second input and the group's has none.
    /// A row is one of the two effects with its cables as they are or taken out, set to one of its twelve modes, and for each parameter why it has no effect then, or null if it has one.
    /// `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` rewrites it.
    #[test]
    fn the_web_clients_fixture_of_when_a_parameter_has_an_effect_is_current() {
        let (_engine, mut ctl, n, _lfo) = sided_and_wired();
        let cables: Vec<EdgeId> = ctl.model.cables.iter().map(|cable| cable.id).collect();
        assert_eq!(cables.len(), 3);
        let mut rows = Vec::new();
        for taken_out in [false, true] {
            if taken_out {
                for edge in &cables {
                    ctl.apply(Intent::RemoveEdge { edge: *edge }).unwrap();
                }
            }
            for node in [n.math, n.bus_math] {
                for mode in 0..12 {
                    ctl.apply(Intent::SetParam { node, param: 0, value: mode as f32 }).unwrap();
                    let slot = if node == n.math { &ctl.model.instruments[0].effects[1] } else { &ctl.model.groups[0].effects[0] };
                    assert_eq!(slot.node, node);
                    let unused: serde_json::Map<String, serde_json::Value> =
                        slot.params.iter().map(|p| (p.name.clone(), slot.unused_because(p, &ctl.model.cables).map_or(serde_json::Value::Null, |why| serde_json::json!(why)))).collect();
                    rows.push(serde_json::json!({ "node": testutil::wire(&node), "cables": if taken_out { "taken out" } else { "as they were" }, "mode": mode, "unused": unused }));
                }
            }
        }
        let fresh = serde_json::to_string_pretty(&serde_json::json!({ "graph": "the snapshot of sideCables.json", "rows": rows })).unwrap() + "\n";
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/src/client/fixtures/uses.json");
        if std::env::var_os("TANG_UPDATE_TYPES").is_some() {
            std::fs::write(&path, &fresh).unwrap();
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            committed == fresh,
            "{} is not when a parameter has an effect now: run `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` and commit the result",
            path.display()
        );
    }

    fn add(ctl: &mut Controller, parent: NodeId, kind: ModulatorKind) -> Result<Applied, IntentError> {
        ctl.apply(Intent::AddFreeModulator { kind, hears: Some(parent) })
    }

    #[test]
    fn add_modulator_makes_each_kind_what_it_says_and_an_empty_lane_takes_one_too() {
        let (engine, mut ctl) = controller_without_modulators();
        sent(&engine);
        let empty = node(&ctl, NodePath::Instrument(1));
        for kind in [ModulatorKind::Lfo, ModulatorKind::Envelope, ModulatorKind::MidiLearn, ModulatorKind::Macro, ModulatorKind::Velocity, ModulatorKind::Key] {
            add(&mut ctl, empty, kind).unwrap();
        }
        let rack = &hearing_lane(&ctl.model, 1);
        assert!(matches!(rack[0].source, ModSourceSlot::Lfo { waveform: LfoWaveform::Sine, rate, .. } if rate == 1.0));
        assert!(matches!(
            rack[1].source,
            ModSourceSlot::Envelope { attack, decay, sustain, release } if (attack, decay, sustain, release) == (0.01, 0.3, 0.7, 0.5)
        ));
        assert!(matches!(rack[2].source, ModSourceSlot::MidiLearn { source: None, learning: true }), "armed for the next control");
        assert!(matches!(rack[3].source, ModSourceSlot::Macro { value } if value == 0.0), "at rest: it moves nothing until it is turned");
        assert!(matches!(rack[4].source, ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Velocity), learning: false }), "bound at once: no controller is moved for it");
        assert!(matches!(rack[5].source, ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Key), learning: false }));
        let sources = [
            "Lfo { waveform: Sine, rate: 1.0, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.0, age: inf }".to_string(),
            "Envelope { attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5, state: Idle, level: 0.0, notes_held: 0 }".to_string(),
            "MidiLearn { source: None, value: 0.0, learning: true }".to_string(),
            "Macro { value: 0.0 }".to_string(),
            "MidiLearn { source: Some(Velocity), value: 0.0, learning: false }".to_string(),
            // (A key rests at middle C, where it adds nothing.)
            format!("MidiLearn {{ source: Some(Key), value: {:?}, learning: false }}", 60.0f32 / 127.0),
        ];
        // Each is put in the session's own rack and told the lane that it hears, which is the empty lane's.
        let told: Vec<String> = sources
            .iter()
            .enumerate()
            .flat_map(|(index, source)| [format!("InsertFreeModulator index={index} source={source}"), format!("SetModulatorEar Free index={index} ear={}", empty.0)])
            .collect();
        assert_eq!(sent(&engine), told);
        assert_eq!(ctl.rev, 6);
        ctl.model.check_ids().unwrap();
    }

    // -- Free modulators (AddFreeModulator) --

    /// A free modulator of `kind` added, its node.
    fn add_free(ctl: &mut Controller, kind: ModulatorKind) -> NodeId {
        ctl.apply(Intent::AddFreeModulator { kind, hears: None }).unwrap();
        ctl.model.free_modulators.last().expect("it is in the session's own rack").node
    }

    /// The nodes of `controller()`'s plugins: the oscillator of lane 0, its reverb, and the group's filter, with a parameter of each to draw a cable to.
    fn free_targets(ctl: &Controller) -> [(NodeId, u32, &'static str); 3] {
        let (detune, _) = param(ctl, NodePath::Instrument(0), "detune");
        let (damping, _) = param(ctl, NodePath::Effect { inst: 0, index: 0 }, "damping");
        let (cutoff, _) = param(ctl, NodePath::GroupEffect { group: 0, index: 0 }, "cutoff");
        [
            (node(ctl, NodePath::Instrument(0)), detune, "detune"),
            (node(ctl, NodePath::Effect { inst: 0, index: 0 }), damping, "damping"),
            (node(ctl, NodePath::GroupEffect { group: 0, index: 0 }), cutoff, "cutoff"),
        ]
    }

    #[test]
    fn add_free_modulator_puts_one_at_the_end_of_the_sessions_own_rack_in_the_model_and_the_engine() {
        let (engine, mut ctl) = controller_without_modulators();
        sent(&engine);
        let applied = ctl.apply(Intent::AddFreeModulator { kind: ModulatorKind::Lfo, hears: None }).unwrap();
        assert_eq!(applied, Applied::restructured(1));
        assert!(applied.events.is_empty(), "nothing a client could apply: it is told by a snapshot");
        assert_eq!(sent(&engine), ["InsertFreeModulator index=0 source=Lfo { waveform: Sine, rate: 1.0, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.0, age: inf }"]);
        ctl.apply(Intent::AddFreeModulator { kind: ModulatorKind::Envelope, hears: None }).unwrap();
        assert_eq!(
            sent(&engine),
            ["InsertFreeModulator index=1 source=Envelope { attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5, state: Idle, level: 0.0, notes_held: 0 }"]
        );
        let rack = &ctl.model.free_modulators;
        assert_eq!(rack.len(), 2);
        assert!(matches!(rack[0].source, ModSourceSlot::Lfo { waveform: LfoWaveform::Sine, rate, .. } if rate == 1.0));
        assert!(rack[0].targets.is_empty(), "it drives nothing yet");
        assert_eq!(ctl.model.locate(rack[1].node), Some(NodePath::Modulator { at: ModulatorLocation::Free, index: 1 }));
        assert_ne!(rack[0].node, rack[1].node, "a node of its own each");
        // (They hear every note: nothing that hears a lane or a group has one more.)
        assert!(hearing_group(&ctl.model, 0).is_empty() && hearing_lane(&ctl.model, 0).is_empty());
        assert_eq!(ctl.rev, 2, "a revision for each");
        assert!(ctl.dirty);
        ctl.model.check_ids().unwrap();
    }

    // -- What a modulator that is in no lane hears (SetModulatorEar, step 93) --

    #[test]
    fn set_modulator_ear_makes_a_free_modulator_hear_a_lane_a_group_or_every_event_in_the_model_and_the_engine() {
        let (engine, mut ctl) = controller_without_modulators();
        let envelope = add_free(&mut ctl, ModulatorKind::Envelope);
        let (lane0, empty, group) = (node(&ctl, NodePath::Instrument(0)), node(&ctl, NodePath::Instrument(1)), node(&ctl, NodePath::Group(0)));
        assert_eq!(ctl.model.free_modulators[0].ear, None, "every event to begin with");
        sent(&engine);
        let applied = ctl.apply(Intent::SetModulatorEar { node: envelope, ear: Some(lane0) }).unwrap();
        assert_eq!(applied, Applied::restructured(2));
        assert!(applied.events.is_empty(), "nothing a client could apply: it is told by a snapshot");
        assert_eq!(ctl.model.free_modulators[0].ear, Some(lane0));
        assert_eq!(sent(&engine), [format!("SetModulatorEar Free index=0 ear={}", lane0.0)]);
        // A group, and a lane with no instrument (its node is the one an instrument would have), and every event again.
        for (ear, text) in [(Some(group), group.0.to_string()), (Some(empty), empty.0.to_string()), (None, "none".to_string())] {
            ctl.apply(Intent::SetModulatorEar { node: envelope, ear }).unwrap();
            assert_eq!(ctl.model.free_modulators[0].ear, ear);
            assert_eq!(sent(&engine), [format!("SetModulatorEar Free index=0 ear={text}")]);
        }
        assert_eq!(ctl.rev, 5, "a revision for each");
        assert!(ctl.dirty);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn set_modulator_ear_is_refused_for_what_is_not_a_modulator_of_the_sessions_own_and_for_an_ear_that_is_not_a_lane_or_a_group() {
        let (engine, mut ctl) = controller();
        let envelope = add_free(&mut ctl, ModulatorKind::Envelope);
        let (lane0, effect) = (node(&ctl, NodePath::Instrument(0)), node(&ctl, NodePath::Effect { inst: 0, index: 0 }));
        let group_lfo = hearing_group(&ctl.model, 0)[0].node;
        sent(&engine);
        let rev = ctl.rev;
        let not_a_modulator = invalid("only a modulator has an ear");
        let not_heard = invalid("only a lane or a group can be heard");
        for (node, ear, refused) in [
            (lane0, Some(lane0), &not_a_modulator),
            (effect, None, &not_a_modulator),
            (envelope, Some(effect), &not_heard),
            (envelope, Some(envelope), &not_heard),
            (envelope, Some(group_lfo), &not_heard),
        ] {
            assert_eq!(ctl.apply(Intent::SetModulatorEar { node, ear }), Err(refused.clone()), "{node:?} hears {ear:?}");
        }
        // A node that is not there (the master has no such node to the intents either).
        for (node, ear, missing) in [(NodeId(9999), None, NodeId(9999)), (NodeId::MASTER, None, NodeId::MASTER), (envelope, Some(NodeId(9999)), NodeId(9999))] {
            assert_eq!(ctl.apply(Intent::SetModulatorEar { node, ear }), Err(IntentError::NotFound(missing)));
        }
        assert_eq!(ctl.rev, rev, "no revision");
        assert!(sent(&engine).is_empty(), "nothing sent");
        assert_eq!(ctl.model.free_modulators[1].ear, None, "the envelope hears every note still (the group's LFO, which the session has, is the first)");
    }

    #[test]
    fn the_graph_says_whom_a_modulator_hears_and_a_lane_that_is_gone_is_every_event_in_it_as_in_the_audio_thread_and_comes_back_with_undo() {
        let (engine, mut ctl) = controller_without_modulators();
        let envelope = add_free(&mut ctl, ModulatorKind::Envelope);
        let empty = node(&ctl, NodePath::Instrument(1));
        let hears = |ctl: &Controller| match &ctl.model.graph().nodes.iter().find(|n| n.id == envelope).unwrap().kind {
            NodeKind::Modulator { hears, .. } => *hears,
            other => panic!("{other:?}"),
        };
        assert_eq!(hears(&ctl), None);
        ctl.apply(Intent::SetModulatorEar { node: envelope, ear: Some(empty) }).unwrap();
        assert_eq!(hears(&ctl), Some(empty));
        sent(&engine);
        ctl.apply(Intent::RemoveNode { node: empty }).unwrap();
        assert_eq!(ctl.model.free_modulators[0].ear, Some(empty), "the model keeps it: the lane comes back under its id");
        assert_eq!(hears(&ctl), None, "and the graph says what the audio thread does: every event, for a lane that is gone");
        assert!(!sent(&engine).iter().any(|c| c.starts_with("SetModulatorEar")), "nothing is told: the audio thread finds no lane by that node");
        ctl.undo().unwrap();
        assert_eq!(hears(&ctl), Some(empty), "and it hears the lane that came back");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_request_for_what_a_modulator_hears_already_changes_nothing_and_does_not_forget_a_lane_that_is_gone() {
        let (engine, mut ctl) = controller_without_modulators();
        let envelope = add_free(&mut ctl, ModulatorKind::Envelope);
        let empty = node(&ctl, NodePath::Instrument(1));
        sent(&engine);
        ctl.apply(Intent::SetModulatorEar { node: envelope, ear: None }).unwrap();
        assert!(sent(&engine).is_empty(), "every note already: the audio thread is told nothing");
        ctl.apply(Intent::SetModulatorEar { node: envelope, ear: Some(empty) }).unwrap();
        assert_eq!(sent(&engine).len(), 1);
        ctl.apply(Intent::SetModulatorEar { node: envelope, ear: Some(empty) }).unwrap();
        assert!(sent(&engine).is_empty(), "that lane already: the audio thread is told nothing");
        // The lane goes: every note is what the modulator hears, as far as anyone can tell, and a request for it is a request for what is so: the model keeps the lane, which Undo brings back.
        ctl.apply(Intent::RemoveNode { node: empty }).unwrap();
        sent(&engine);
        ctl.apply(Intent::SetModulatorEar { node: envelope, ear: None }).unwrap();
        assert!(sent(&engine).is_empty());
        assert_eq!(ctl.model.free_modulators[0].ear, Some(empty));
        assert_eq!(ctl.history_state().undo.as_deref(), Some("remove an empty lane"), "and it is no step");
        ctl.undo().unwrap();
        assert_eq!(ctl.model.free_modulators[0].ear, Some(empty), "the lane that comes back is heard again");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_modulator_that_is_made_to_hear_another_while_the_lane_it_heard_is_gone_is_put_back_hearing_every_note_and_not_the_lane_that_is_not_there() {
        let (_engine, mut ctl) = controller_without_modulators();
        let envelope = add_free(&mut ctl, ModulatorKind::Envelope);
        let (empty, group) = (node(&ctl, NodePath::Instrument(1)), node(&ctl, NodePath::Group(0)));
        ctl.apply(Intent::SetModulatorEar { node: envelope, ear: Some(empty) }).unwrap();
        ctl.apply(Intent::RemoveNode { node: empty }).unwrap();
        ctl.apply(Intent::SetModulatorEar { node: envelope, ear: Some(group) }).unwrap();
        assert_eq!(ctl.model.free_modulators[0].ear, Some(group));
        ctl.undo().expect("a step that names a lane that is not there could not be undone");
        assert_eq!(ctl.model.free_modulators[0].ear, None, "what it heard: every note");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn add_free_modulator_makes_each_kind_what_it_says() {
        let (_engine, mut ctl) = controller_without_modulators();
        for kind in [ModulatorKind::Lfo, ModulatorKind::Envelope, ModulatorKind::MidiLearn, ModulatorKind::Macro, ModulatorKind::Velocity, ModulatorKind::Key, ModulatorKind::Steps] {
            add_free(&mut ctl, kind);
        }
        let rack = &ctl.model.free_modulators;
        assert!(matches!(rack[0].source, ModSourceSlot::Lfo { .. }));
        assert!(matches!(rack[1].source, ModSourceSlot::Envelope { .. }));
        assert!(matches!(rack[2].source, ModSourceSlot::MidiLearn { source: None, learning: true }), "armed for the next control");
        assert!(matches!(rack[3].source, ModSourceSlot::Macro { value } if value == 0.0));
        assert!(matches!(rack[4].source, ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Velocity), learning: false }));
        assert!(matches!(rack[5].source, ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Key), learning: false }));
        assert!(rack[6].source.steps().is_some(), "a step sequencer");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_free_modulator_draws_a_cable_to_a_parameter_of_any_plugin_of_the_session_and_the_engine_is_told_by_the_node() {
        let (engine, mut ctl) = controller_without_modulators();
        let lfo = add_free(&mut ctl, ModulatorKind::Lfo);
        sent(&engine);
        let plugins = free_targets(&ctl);
        for (n, (plugin, index, name)) in plugins.into_iter().enumerate() {
            let applied = ctl.apply(cable_to(lfo, plugin, index)).unwrap();
            assert_eq!(applied, Applied::restructured(ctl.rev));
            let commands = sent(&engine);
            assert_eq!(commands.len(), 1, "{commands:?}");
            let aim = format!("AddFreeModTarget mod=0 aim=Param {{ plugin: NodeId({}), index: {index} }} depth=0.500", plugin.0);
            assert!(commands[0].starts_with(&aim), "{} should start with {aim}", commands[0]);
            let cable = &ctl.model.free_modulators[0].targets[n];
            assert_eq!((cable.param_name.as_str(), cable.depth), (name, 0.5));
            // The place the model names it by, for the file and the lists: the plugin's node.
            assert_eq!(ctl.model.kind_of_target(ModulatorLocation::Free, 0, n), Some(ModTargetKind::FreeParam { plugin, param_index: index }));
        }
        assert_eq!(ctl.model.free_modulators[0].targets.len(), 3);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_free_modulator_does_not_reach_what_is_no_plugin_or_has_none_and_a_refusal_changes_nothing() {
        let (engine, mut ctl) = controller_without_modulators();
        let lfo = add_free(&mut ctl, ModulatorKind::Lfo);
        let (group, empty_lane) = (node(&ctl, NodePath::Group(0)), node(&ctl, NodePath::Instrument(1)));
        let [(osc, detune, _), ..] = free_targets(&ctl);
        ctl.apply(cable_to(lfo, osc, detune)).unwrap();
        sent(&engine);
        let (rev, slots) = (ctl.rev, ctl.model.slots.high_water());
        for (what, intent) in [
            ("a group has no parameter", cable_to(lfo, group, 0)),
            ("a lane that has no instrument has none", cable_to(lfo, empty_lane, 0)),
            ("a parameter that the plugin has not", cable_to(lfo, osc, 9_999)),
            ("a node that is not there", cable_to(lfo, NodeId(9_999), 0)),
            ("a parameter that it drives already", cable_to(lfo, osc, detune)),
            ("itself", Intent::AddModEdge { modulator: lfo, to: Endpoint { node: lfo, port: Port::Rate } }),
        ] {
            assert!(ctl.apply(intent).is_err(), "{what}");
        }
        assert!(sent(&engine).is_empty(), "nothing is sent for what is refused");
        assert_eq!((ctl.rev, ctl.model.slots.high_water()), (rev, slots), "no revision and no slot is used");
        assert_eq!(ctl.model.free_modulators[0].targets.len(), 1);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_free_modulator_draws_cables_to_the_settings_of_another_free_modulator_and_the_depths_of_its_cables() {
        let (engine, mut ctl) = controller_without_modulators();
        let lfo = add_free(&mut ctl, ModulatorKind::Lfo);
        let envelope = add_free(&mut ctl, ModulatorKind::Envelope);
        let [(osc, detune, _), ..] = free_targets(&ctl);
        // (The envelope's cable to the oscillator, whose depth the LFO can then drive.)
        ctl.apply(cable_to(envelope, osc, detune)).unwrap();
        let cable = ctl.model.free_modulators[1].targets[0].edge;
        sent(&engine);
        ctl.apply(Intent::AddModEdge { modulator: lfo, to: Endpoint { node: envelope, port: Port::Attack } }).unwrap();
        ctl.apply(Intent::AddModEdge { modulator: lfo, to: Endpoint { node: envelope, port: Port::Depth { edge: cable } } }).unwrap();
        let commands = sent(&engine);
        assert_eq!(commands.len(), 2, "{commands:?}");
        assert!(commands[0].starts_with(&format!("AddFreeModTarget mod=0 aim=Setting {{ modulator: NodeId({}), field: Attack }}", envelope.0)), "{commands:?}");
        assert!(commands[1].starts_with(&format!("AddFreeModTarget mod=0 aim=Depth(EdgeId({})) ", cable.0)), "{commands:?}");
        assert_eq!(ctl.model.kind_of_target(ModulatorLocation::Free, 0, 0), Some(ModTargetKind::ModulatorAttack { mod_index: 1 }));
        assert_eq!(ctl.model.kind_of_target(ModulatorLocation::Free, 0, 1), Some(ModTargetKind::ModulatorDepth { mod_index: 1, target_index: 0 }));
        // The envelope has no rate, and the LFO has no attack.
        assert!(ctl.apply(Intent::AddModEdge { modulator: lfo, to: Endpoint { node: envelope, port: Port::Rate } }).is_err());
        assert!(ctl.apply(Intent::AddModEdge { modulator: envelope, to: Endpoint { node: lfo, port: Port::Attack } }).is_err());
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn modulate_for_a_setting_of_a_free_modulator_makes_a_free_sibling_that_drives_it() {
        let (engine, mut ctl) = controller_without_modulators();
        let lfo = add_free(&mut ctl, ModulatorKind::Lfo);
        sent(&engine);
        let applied = modulate(&mut ctl, lfo, Port::Rate, ModulatorKind::Envelope, None).unwrap();
        assert!(applied.restructured);
        assert_eq!(ctl.model.free_modulators.len(), 2, "the new one goes in the session's own rack");
        assert!(matches!(ctl.model.free_modulators[1].source, ModSourceSlot::Envelope { .. }));
        assert_eq!(ctl.model.free_modulators[1].targets.len(), 1);
        assert_eq!(ctl.model.kind_of_target(ModulatorLocation::Free, 1, 0), Some(ModTargetKind::ModulatorRate { mod_index: 0 }));
        let commands = sent(&engine);
        assert!(commands[0].starts_with("InsertFreeModulator index=1 source=Envelope"), "{commands:?}");
        assert!(commands[1].starts_with(&format!("AddFreeModTarget mod=1 aim=Setting {{ modulator: NodeId({}), field: Rate }}", lfo.0)), "{commands:?}");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn modulate_for_a_setting_of_a_free_modulator_makes_a_sibling_that_hears_what_it_hears() {
        let (engine, mut ctl) = controller();
        let envelope = add_free(&mut ctl, ModulatorKind::Envelope);
        let (lane0, group) = (node(&ctl, NodePath::Instrument(0)), node(&ctl, NodePath::Group(0)));
        // (A modulator that hears every note has a sibling that does, and one that hears a lane or a group has one that hears the same: the sibling is in the rack that the modulator is in.)
        for ear in [None, Some(lane0), Some(group)] {
            ctl.apply(Intent::SetModulatorEar { node: envelope, ear }).unwrap();
            sent(&engine);
            modulate(&mut ctl, envelope, Port::Attack, ModulatorKind::Envelope, None).unwrap();
            let sibling = ctl.model.free_modulators.last().unwrap();
            assert_eq!(sibling.ear, ear, "{ear:?}");
            let commands = sent(&engine);
            let told = commands.iter().find(|c| c.starts_with("SetModulatorEar")).cloned();
            assert_eq!(told, ear.map(|heard| format!("SetModulatorEar Free index={} ear={}", ctl.model.free_modulators.len() - 1, heard.0)), "{commands:?}");
        }
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn add_free_modulator_can_make_one_that_hears_a_lane_or_a_group_and_the_audio_thread_is_told_with_the_modulator() {
        let (engine, mut ctl) = controller_without_modulators();
        let (lane0, empty, group) = (node(&ctl, NodePath::Instrument(0)), node(&ctl, NodePath::Instrument(1)), node(&ctl, NodePath::Group(0)));
        sent(&engine);
        for (index, heard) in [lane0, empty, group].into_iter().enumerate() {
            ctl.apply(Intent::AddFreeModulator { kind: ModulatorKind::Envelope, hears: Some(heard) }).unwrap();
            assert_eq!(ctl.model.free_modulators[index].ear, Some(heard));
            let commands = sent(&engine);
            assert!(commands[0].starts_with(&format!("InsertFreeModulator index={index} source=Envelope")), "{commands:?}");
            assert_eq!(commands.iter().filter(|c| c.starts_with("SetModulatorEar")).collect::<Vec<_>>(), [&format!("SetModulatorEar Free index={index} ear={}", heard.0)], "{commands:?}");
        }
        // One that hears nothing in particular tells the audio thread nothing of an ear.
        ctl.apply(Intent::AddFreeModulator { kind: ModulatorKind::Lfo, hears: None }).unwrap();
        assert_eq!(ctl.model.free_modulators[3].ear, None);
        assert!(sent(&engine).iter().all(|c| !c.starts_with("SetModulatorEar")));
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn add_free_modulator_refuses_an_ear_that_is_not_a_lane_or_a_group_and_changes_nothing_not_even_the_ids_that_the_next_one_gets() {
        let (engine, mut ctl) = controller();
        let (effect, group_lfo) = (node(&ctl, NodePath::Effect { inst: 0, index: 0 }), hearing_group(&ctl.model, 0)[0].node);
        let rack = ctl.model.free_modulators.len();
        sent(&engine);
        for (ear, refused) in [
            (effect, invalid("only a lane or a group can be heard")),
            (group_lfo, invalid("only a lane or a group can be heard")),
            (NodeId(9_999), IntentError::NotFound(NodeId(9_999))),
            (NodeId::MASTER, IntentError::NotFound(NodeId::MASTER)),
        ] {
            assert_eq!(ctl.apply(Intent::AddFreeModulator { kind: ModulatorKind::Lfo, hears: Some(ear) }), Err(refused), "{ear:?}");
        }
        assert_eq!(ctl.model.free_modulators.len(), rack);
        assert_eq!(ctl.rev, 0, "no revision");
        assert!(sent(&engine).is_empty());
        // The next one has the node and the slot that it would have had if nothing had been refused.
        let (_other_engine, mut other) = controller();
        let made = |ctl: &mut Controller| {
            let lane0 = node(ctl, NodePath::Instrument(0));
            ctl.apply(Intent::AddFreeModulator { kind: ModulatorKind::Lfo, hears: Some(lane0) }).unwrap();
            ctl.model.free_modulators.last().map(|m| (m.node, m.output))
        };
        assert_eq!(made(&mut ctl), made(&mut other));
    }

    #[test]
    fn a_free_modulator_that_is_taken_out_gives_what_it_drove_the_value_the_user_set_and_the_cables_that_drove_its_settings_go_first() {
        let (engine, mut ctl) = controller_without_modulators();
        let lfo = add_free(&mut ctl, ModulatorKind::Lfo);
        let envelope = add_free(&mut ctl, ModulatorKind::Envelope);
        let [(osc, detune, _), ..] = free_targets(&ctl);
        ctl.apply(Intent::SetParam { node: osc, param: detune, value: 3.0 }).unwrap();
        ctl.apply(cable_to(lfo, osc, detune)).unwrap();
        ctl.apply(Intent::AddModEdge { modulator: envelope, to: Endpoint { node: lfo, port: Port::Rate } }).unwrap();
        sent(&engine);
        ctl.apply(Intent::RemoveNode { node: lfo }).unwrap();
        // The envelope's cable to the LFO's rate goes first, then the LFO, whose cable's parameter has the value that the user set again.
        assert_eq!(
            sent(&engine),
            [
                "RemoveFreeModTarget mod=1 target=0".to_string(),
                "RemoveFreeModulator index=0".to_string(),
                format!("SetParameter inst=0 slot=0 param={detune} value=3.000"),
            ]
        );
        assert_eq!(ctl.model.free_modulators.len(), 1);
        assert_eq!(ctl.model.free_modulators[0].node, envelope);
        assert!(ctl.model.free_modulators[0].targets.is_empty());
        assert_eq!(ctl.model.locate(envelope), Some(NodePath::Modulator { at: ModulatorLocation::Free, index: 0 }), "the one after it moves down a place");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn the_cables_of_a_free_modulator_to_an_effect_that_goes_go_first_and_nothing_is_given_back() {
        let (engine, mut ctl) = controller_without_modulators();
        let lfo = add_free(&mut ctl, ModulatorKind::Lfo);
        let [(osc, detune, _), (reverb, damping, _), (filter, cutoff, _)] = free_targets(&ctl);
        for (plugin, index) in [(osc, detune), (reverb, damping), (filter, cutoff)] {
            ctl.apply(cable_to(lfo, plugin, index)).unwrap();
        }
        sent(&engine);
        ctl.apply(Intent::RemoveNode { node: reverb }).unwrap();
        let commands = sent(&engine);
        assert_eq!(commands.first().map(String::as_str), Some("RemoveFreeModTarget mod=0 target=1"), "{commands:?}");
        assert!(commands[1].starts_with("RemoveEffect inst=0 index=0"), "{commands:?}");
        assert_eq!(commands.len(), 2, "what it drove goes with the plugin: {commands:?}");
        let aims: Vec<_> = ctl.model.free_modulators[0].targets.iter().map(|t| t.aim).collect();
        assert_eq!(aims.len(), 2);
        assert!(aims.iter().all(|aim| !matches!(aim, Driven::Param { plugin, .. } if *plugin == reverb)));
        // The bus's filter too.
        ctl.apply(Intent::RemoveNode { node: filter }).unwrap();
        assert_eq!(ctl.model.free_modulators[0].targets.len(), 1);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn the_cables_of_a_free_modulator_to_the_plugins_of_a_lane_that_goes_go_with_it_and_the_cables_to_the_others_stay() {
        let (engine, mut ctl) = controller_without_modulators();
        let lfo = add_free(&mut ctl, ModulatorKind::Lfo);
        let [(osc, detune, _), (reverb, damping, _), (filter, cutoff, _)] = free_targets(&ctl);
        for (plugin, index) in [(osc, detune), (reverb, damping), (filter, cutoff)] {
            ctl.apply(cable_to(lfo, plugin, index)).unwrap();
        }
        sent(&engine);
        ctl.apply(Intent::RemoveNode { node: osc }).unwrap();
        let commands = sent(&engine);
        assert_eq!(commands.len(), 3, "{commands:?}");
        assert_eq!(&commands[..2], ["RemoveFreeModTarget mod=0 target=0", "RemoveFreeModTarget mod=0 target=0"], "the two cables to the lane's plugins, the first of what is left each time");
        assert!(commands[2].starts_with("RemoveInstrument inst=0"), "{commands:?}");
        let aims: Vec<_> = ctl.model.free_modulators[0].targets.iter().map(|t| t.aim).collect();
        assert_eq!(aims.len(), 1);
        assert!(matches!(aims[0], Driven::Param { plugin, .. } if plugin == filter), "the bus's stays");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn the_cables_of_a_free_modulator_to_the_bus_effects_of_a_group_that_goes_go_with_it() {
        let (engine, mut ctl) = controller_without_modulators();
        let lfo = add_free(&mut ctl, ModulatorKind::Lfo);
        let [(osc, detune, _), _, (filter, cutoff, _)] = free_targets(&ctl);
        ctl.apply(cable_to(lfo, osc, detune)).unwrap();
        ctl.apply(cable_to(lfo, filter, cutoff)).unwrap();
        sent(&engine);
        let group = node(&ctl, NodePath::Group(0));
        ctl.apply(Intent::RemoveNode { node: group }).unwrap();
        let commands = sent(&engine);
        assert_eq!(commands.first().map(String::as_str), Some("RemoveFreeModTarget mod=0 target=1"), "{commands:?}");
        assert!(commands.iter().any(|c| c.starts_with("RemoveGroup group=0")), "{commands:?}");
        assert_eq!(ctl.model.free_modulators[0].targets.len(), 1, "the lane's plugin stays, and so does the cable to it");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_free_modulators_cable_to_an_instrument_that_is_replaced_or_taken_out_of_its_lane_goes_first() {
        let (engine, mut ctl) = controller_without_modulators();
        let lfo = add_free(&mut ctl, ModulatorKind::Lfo);
        let [(osc, detune, _), ..] = free_targets(&ctl);
        ctl.apply(cable_to(lfo, osc, detune)).unwrap();
        sent(&engine);
        ctl.apply(Intent::ClearInstrument { node: osc }).unwrap();
        let commands = sent(&engine);
        assert_eq!(commands.first().map(String::as_str), Some("RemoveFreeModTarget mod=0 target=0"), "{commands:?}");
        assert!(ctl.model.free_modulators[0].targets.is_empty());
        ctl.model.check_ids().unwrap();
    }

    // -- Modulate --

    fn modulate(ctl: &mut Controller, node: NodeId, port: Port, kind: ModulatorKind, depth: Option<f32>) -> Result<Applied, IntentError> {
        ctl.apply(Intent::Modulate { to: Endpoint { node, port }, kind, depth })
    }

    #[test]
    fn modulate_makes_a_modulator_of_the_sessions_own_that_hears_the_lane_and_a_cable_from_it_to_the_parameter_as_one_intent() {
        let (engine, mut ctl) = controller_without_modulators();
        sent(&engine);
        let (detune, _) = param(&ctl, NodePath::Instrument(0), "detune");
        let osc = lane(&ctl);
        let applied = modulate(&mut ctl, osc, Port::Param { index: detune }, ModulatorKind::Lfo, None).unwrap();
        // One intent is one revision, and the structure is told whole.
        assert_eq!((applied.rev, applied.restructured, ctl.rev), (1, true, 1));
        // (Step 94: the modulator is in no lane. The new one hears the lane that the knob is in.)
        let rack = &ctl.model.free_modulators;
        assert_eq!(rack.len(), 1);
        assert!(matches!(rack[0].source, ModSourceSlot::Lfo { .. }));
        assert_eq!(rack[0].ear, Some(osc), "it hears the notes of the lane that the plugin is in");
        assert_eq!(rack[0].targets.len(), 1);
        assert_eq!(ctl.model.kind_of_target(ModulatorLocation::Free, 0, 0), Some(ModTargetKind::FreeParam { plugin: osc, param_index: detune }));
        assert_eq!(rack[0].targets[0].depth, NEW_CABLE_DEPTH);
        let commands = sent(&engine);
        assert_eq!(commands.len(), 3, "{commands:?}");
        assert!(commands[0].starts_with("InsertFreeModulator index=0 source=Lfo"), "{commands:?}");
        assert_eq!(commands[1], format!("SetModulatorEar Free index=0 ear={}", osc.0), "the ear goes with the modulator");
        assert!(commands[2].starts_with("AddFreeModTarget mod=0 "), "{commands:?}");
        assert!(ctl.dirty);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn modulate_gives_the_cable_the_depth_that_is_asked_for_clamped_as_a_depth_that_is_set_is() {
        let (engine, mut ctl) = controller_without_modulators();
        let (detune, _) = param(&ctl, NodePath::Instrument(0), "detune");
        let (volume, _) = param(&ctl, NodePath::Instrument(0), "volume");
        let (pan, _) = param(&ctl, NodePath::Instrument(0), "pan");
        let osc = lane(&ctl);
        sent(&engine);
        for (index, (param, asked, got)) in [(detune, 1.0, 1.0), (volume, -0.25, -0.25), (pan, 5.0, 1.0)].into_iter().enumerate() {
            modulate(&mut ctl, osc, Port::Param { index: param }, ModulatorKind::MidiLearn, Some(asked)).unwrap();
            assert_eq!(ctl.model.free_modulators[index].targets[0].depth, got, "{asked}");
        }
        // (The depth is set after the cable is drawn, as it is when it is set by itself.)
        let commands = sent(&engine);
        let at = commands.iter().position(|c| c.starts_with("AddFreeModTarget mod=0")).expect("the first cable");
        assert_eq!(commands[at + 1], "SetFreeModTargetDepth mod=0 target=0 depth=1.000", "{commands:?}");
        assert!(commands.contains(&"SetFreeModTargetDepth mod=1 target=0 depth=-0.250".to_string()), "{commands:?}");
        assert!(commands.contains(&"SetFreeModTargetDepth mod=2 target=0 depth=1.000".to_string()), "{commands:?}");
        assert_eq!(ctl.rev, 3, "a revision for each");
    }

    #[test]
    fn modulate_makes_each_kind_of_modulator_and_a_control_is_made_armed_to_learn() {
        let (_engine, mut ctl) = controller_without_modulators();
        let (detune, _) = param(&ctl, NodePath::Instrument(0), "detune");
        let (volume, _) = param(&ctl, NodePath::Instrument(0), "volume");
        let (pan, _) = param(&ctl, NodePath::Instrument(0), "pan");
        let (attack, _) = param(&ctl, NodePath::Instrument(0), "attack");
        let osc = lane(&ctl);
        let (sustain, _) = param(&ctl, NodePath::Instrument(0), "sustain");
        let (release, _) = param(&ctl, NodePath::Instrument(0), "release");
        for (param, kind) in [
            (detune, ModulatorKind::Lfo),
            (volume, ModulatorKind::Envelope),
            (pan, ModulatorKind::MidiLearn),
            (attack, ModulatorKind::Macro),
            (sustain, ModulatorKind::Velocity),
            (release, ModulatorKind::Key),
        ] {
            modulate(&mut ctl, osc, Port::Param { index: param }, kind, None).unwrap();
        }
        let rack = &ctl.model.free_modulators;
        assert!(rack.iter().all(|m| m.ear == Some(osc)), "each hears the lane");
        assert!(matches!(rack[0].source, ModSourceSlot::Lfo { waveform: LfoWaveform::Sine, rate, .. } if rate == 1.0));
        assert!(matches!(rack[1].source, ModSourceSlot::Envelope { attack, decay, sustain, release } if (attack, decay, sustain, release) == (0.01, 0.3, 0.7, 0.5)));
        assert!(matches!(rack[2].source, ModSourceSlot::MidiLearn { source: None, learning: true }), "armed for the next control");
        assert!(matches!(rack[3].source, ModSourceSlot::Macro { value } if value == 0.0), "at rest, so that the cable moves nothing until the macro is turned");
        assert!(matches!(rack[4].source, ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Velocity), learning: false }), "{:?}", rack[4].source);
        assert!(matches!(rack[5].source, ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Key), learning: false }), "{:?}", rack[5].source);
        assert_eq!(rack.iter().map(|m| m.targets.len()).collect::<Vec<_>>(), [1, 1, 1, 1, 1, 1]);
        assert_eq!(rack[4].targets[0].depth, NEW_CABLE_DEPTH, "a cable starts at half depth, as an LFO's does: only a control that is learned starts at the whole of it");
        assert_eq!(rack[5].targets[0].depth, NEW_CABLE_DEPTH);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn modulate_an_effects_parameter_makes_a_modulator_that_hears_its_lane_and_a_bus_effects_one_that_hears_its_group() {
        let (engine, mut ctl) = controller_without_modulators();
        sent(&engine);
        let reverb = NodePath::Effect { inst: 0, index: 0 };
        let (damping, _) = param(&ctl, reverb, "damping");
        let reverb_node = node(&ctl, reverb);
        modulate(&mut ctl, reverb_node, Port::Param { index: damping }, ModulatorKind::Lfo, None).unwrap();
        // The reverb is the second plugin of the lane; the new modulator hears the lane (though the lane is a member of the group), and nothing hears the group.
        let (lane_node, group_node) = (node(&ctl, NodePath::Instrument(0)), node(&ctl, NodePath::Group(0)));
        assert_eq!(ctl.model.free_modulators.len(), 1);
        assert_eq!(ctl.model.free_modulators[0].ear, Some(lane_node));
        assert_eq!(ctl.model.kind_of_target(ModulatorLocation::Free, 0, 0), Some(ModTargetKind::FreeParam { plugin: reverb_node, param_index: damping }));
        assert!(hearing_group(&ctl.model, 0).is_empty(), "nothing hears the group");

        let bus = NodePath::GroupEffect { group: 0, index: 0 };
        let (cutoff, _) = param(&ctl, bus, "cutoff");
        let bus_node = node(&ctl, bus);
        sent(&engine);
        modulate(&mut ctl, bus_node, Port::Param { index: cutoff }, ModulatorKind::Envelope, None).unwrap();
        assert_eq!(ctl.model.free_modulators.len(), 2);
        assert_eq!(ctl.model.free_modulators[1].ear, Some(group_node), "a bus effect's modulator hears the group");
        assert_eq!(ctl.model.kind_of_target(ModulatorLocation::Free, 1, 0), Some(ModTargetKind::FreeParam { plugin: bus_node, param_index: cutoff }));
        assert_eq!(hearing_group(&ctl.model, 0).len(), 1, "it is the one that hears the group");
        let commands = sent(&engine);
        assert!(commands[0].starts_with("InsertFreeModulator index=1"), "{commands:?}");
        assert_eq!(commands[1], format!("SetModulatorEar Free index=1 ear={}", group_node.0), "{commands:?}");
        assert!(commands[2].starts_with("AddFreeModTarget mod=1 "), "{commands:?}");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn modulate_a_modulators_own_setting_makes_a_sibling_that_hears_what_it_hears_and_drives_it() {
        let (engine, mut ctl) = modulated();
        let (lane_lfo, group_lfo) = (lane_mod(&ctl, 0), group_mod(&ctl, 0));
        let (lane0, group) = (node(&ctl, NodePath::Instrument(0)), node(&ctl, NodePath::Group(0)));
        let cable = ctl.model.free_modulators[0].targets[0].edge;
        sent(&engine);
        // The rate of the lane's LFO, the rate of the group's, and the depth of the lane's first cable: each is driven by a sibling.
        modulate(&mut ctl, lane_lfo, Port::Rate, ModulatorKind::Lfo, None).unwrap();
        modulate(&mut ctl, group_lfo, Port::Rate, ModulatorKind::Envelope, None).unwrap();
        modulate(&mut ctl, lane_lfo, Port::Depth { edge: cable }, ModulatorKind::Lfo, None).unwrap();
        let rack = &ctl.model.free_modulators;
        assert_eq!(rack.len(), 9, "three new ones, after the six the session had (the lane's four and the group's two)");
        assert_eq!(ctl.model.kind_of_target(ModulatorLocation::Free, 6, 0), Some(ModTargetKind::ModulatorRate { mod_index: 0 }));
        assert_eq!(ctl.model.kind_of_target(ModulatorLocation::Free, 7, 0), Some(ModTargetKind::ModulatorRate { mod_index: 4 }));
        assert_eq!(ctl.model.kind_of_target(ModulatorLocation::Free, 8, 0), Some(ModTargetKind::ModulatorDepth { mod_index: 0, target_index: 0 }));
        // Each hears what the one that it drives hears: two the lane, one the group.
        assert_eq!(rack[6..].iter().map(|m| m.ear).collect::<Vec<_>>(), [Some(lane0), Some(group), Some(lane0)]);
        let commands = sent(&engine);
        assert!(commands[0].starts_with("InsertFreeModulator index=6"), "{commands:?}");
        assert_eq!(commands[1], format!("SetModulatorEar Free index=6 ear={}", lane0.0), "{commands:?}");
        assert!(commands[2].starts_with(&testutil::told(&ctl.model, "AddFreeModTarget mod=6 kind=ModulatorRate { mod_index: 0 }")), "{commands:?}");
        assert!(commands[3].starts_with("InsertFreeModulator index=7"), "{commands:?}");
        assert_eq!(commands[4], format!("SetModulatorEar Free index=7 ear={}", group.0), "{commands:?}");
        assert!(commands[5].starts_with(&testutil::told(&ctl.model, "AddFreeModTarget mod=7 kind=ModulatorRate { mod_index: 4 }")), "{commands:?}");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn modulate_makes_a_modulator_that_hears_the_lane_the_plugin_is_in_whichever_lane_that_is() {
        let (engine, mut ctl) = reaching();
        sent(&engine);
        let racks = |ctl: &Controller| (0..ctl.model.instruments.len()).map(|lane| hearing_lane(&ctl.model, lane).len()).collect::<Vec<_>>();
        let groups = |ctl: &Controller| (0..ctl.model.groups.len()).map(|group| hearing_group(&ctl.model, group).len()).collect::<Vec<_>>();
        let (before, groups_before, free_before) = (racks(&ctl), groups(&ctl), ctl.model.free_modulators.len());
        let mut expected = before.clone();
        // The filter of the lane that has none of a group's: the new modulator hears that lane.
        let filter = NodePath::Effect { inst: 3, index: 0 };
        let (cutoff, _) = param(&ctl, filter, "cutoff");
        let filter_node = node(&ctl, filter);
        modulate(&mut ctl, filter_node, Port::Param { index: cutoff }, ModulatorKind::Lfo, None).unwrap();
        expected[3] += 1;
        assert_eq!(racks(&ctl), expected, "one more hears that lane, and no other lane has one more");
        assert_eq!(ctl.model.free_modulators.len(), free_before + 1);
        let made = ctl.model.free_modulators.last().unwrap();
        assert_eq!(made.ear, Some(node(&ctl, NodePath::Instrument(3))));
        assert_eq!(ctl.model.kind_of_target(ModulatorLocation::Free, free_before, 0), Some(ModTargetKind::FreeParam { plugin: filter_node, param_index: cutoff }));
        assert!(sent(&engine)[0].starts_with(&format!("InsertFreeModulator index={free_before}")));
        // The instrument of a lane that is a member of a group: it hears the lane, and nothing hears the group more.
        let (detune, _) = param(&ctl, NodePath::Instrument(2), "detune");
        let osc = node(&ctl, NodePath::Instrument(2));
        modulate(&mut ctl, osc, Port::Param { index: detune }, ModulatorKind::Envelope, None).unwrap();
        expected[2] += 1;
        assert_eq!(racks(&ctl), expected);
        assert_eq!(ctl.model.free_modulators.last().unwrap().ear, Some(osc));
        assert_eq!(ctl.model.kind_of_target(ModulatorLocation::Free, free_before + 1, 0), Some(ModTargetKind::FreeParam { plugin: osc, param_index: detune }));
        assert_eq!(groups(&ctl), groups_before);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn modulate_refuses_what_it_cannot_do_and_changes_nothing_not_even_the_ids_that_the_next_one_gets() {
        let (engine, mut ctl) = modulated();
        let (lane_lfo, envelope, group) = (lane_mod(&ctl, 0), lane_mod(&ctl, 1), node(&ctl, NodePath::Group(0)));
        let lane = node(&ctl, NodePath::Instrument(0));
        let (detune, _) = param(&ctl, NodePath::Instrument(0), "detune");
        sent(&engine);
        let racks = |ctl: &Controller| (ctl.model.free_modulators.len(), hearing_group(&ctl.model, 0).len(), ctl.model.free_modulators.len());
        let before = racks(&ctl);
        let refused: Vec<(&str, NodeId, Port, Option<f32>, IntentError)> = vec![
            ("a node that is not there", NodeId(9_999), Port::Param { index: 0 }, None, IntentError::NotFound(NodeId(9_999))),
            ("a group, which has no parameter", group, Port::Param { index: 0 }, None, invalid("a modulator drives a parameter of a plugin, a volume, a mix or a pan, or a setting of a modulator, and that is none of them")),
            ("a parameter the plugin has not", lane, Port::Param { index: 999 }, None, invalid("the plugin has no parameter 999")),
            ("a setting that the modulator has not", envelope, Port::Rate, None, invalid("that modulator has no rate")),
            ("the depth of a cable that is not there", lane_lfo, Port::Depth { edge: EdgeId(9_999) }, None, IntentError::EdgeNotFound(EdgeId(9_999))),
            ("a port that no cable ends at", lane, Port::ModOut, None, invalid("a cable can only end at a plugin's parameter, at a volume, a mix or a pan, or at a modulator's rate, stage or depth")),
            ("a depth that is no number", lane, Port::Param { index: detune }, Some(f32::NAN), invalid("NaN is not a value for the depth of a cable")),
        ];
        for (what, to, port, depth, error) in refused {
            assert_eq!(modulate(&mut ctl, to, port, ModulatorKind::Lfo, depth), Err(error), "{what}");
            assert_eq!(racks(&ctl), before, "{what}");
            assert_eq!(ctl.rev, 0, "{what}");
            assert!(!ctl.dirty, "{what}");
            assert!(sent(&engine).is_empty(), "{what}");
        }
        ctl.model.check_ids().unwrap();
        // The next modulator and cable have the ids that they would have had if nothing had been refused.
        modulate(&mut ctl, lane, Port::Param { index: detune }, ModulatorKind::Lfo, None).unwrap();
        let (_other_engine, mut other) = modulated();
        let other_lane = node(&other, NodePath::Instrument(0));
        modulate(&mut other, other_lane, Port::Param { index: detune }, ModulatorKind::Lfo, None).unwrap();
        let ids = |ctl: &Controller| ctl.model.free_modulators.last().map(|m| (m.node, m.targets[0].edge, m.targets[0].effective, m.output));
        assert_eq!(ids(&ctl), ids(&other));
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
        ctl.add_modulator(ModulatorLocation::Free, ModSourceSlot::new_lfo()).unwrap();
        (engine, ctl)
    }

    /// `reaching()` and two modulators more, an LFO and an envelope that hear every note, the LFO with a cable to the first lane's detune and the envelope with one to its filter's resonance (so that each has a cable whose
    /// depth the other can drive, and neither drives a setting or the first parameter of a plugin, which the fixture's cables are tried to: the page's `reachFrom` and `fieldsOf` do not look at what a modulator drives already, as the
    /// controller does): every kind of rack in one session, for what the page's rules say of a
    /// modulator that has no parent (`web/src/client/fixtures/freeReach.json`).
    fn reaching_with_free() -> (StubEngine, Controller) {
        let (engine, mut ctl) = reaching();
        ctl.apply(Intent::AddFreeModulator { kind: ModulatorKind::Lfo, hears: None }).unwrap();
        ctl.apply(Intent::AddFreeModulator { kind: ModulatorKind::Envelope, hears: None }).unwrap();
        let made = ctl.model.free_modulators.len();
        let (lfo, envelope) = (ctl.model.free_modulators[made - 2].node, ctl.model.free_modulators[made - 1].node);
        let (osc, (detune, _)) = (ctl.model.instruments[0].node, param(&ctl, NodePath::Instrument(0), "detune"));
        ctl.apply(cable_to(lfo, osc, detune)).unwrap();
        let (filter, (resonance, _)) = (ctl.model.instruments[0].effects[0].node, param(&ctl, NodePath::Effect { inst: 0, index: 0 }, "resonance"));
        ctl.apply(cable_to(envelope, filter, resonance)).unwrap();
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
        testutil::told(&ctl.model, &format!("{prefix} kind={kind} depth=0.500 base={value:.3} range=[{min:.3}, {max:.3}]"))
    }

    #[test]
    fn add_mod_edge_draws_a_cable_from_a_lane_modulator_to_its_instrument_and_to_its_effects() {
        let (engine, mut ctl) = reaching();
        sent(&engine);
        let lfo = ctl.model.free_modulators[0].node;
        let (osc, filter) = (ctl.model.instruments[0].node, ctl.model.instruments[0].effects[0].node);
        let (detune, _) = param(&ctl, NodePath::Instrument(0), "detune");
        let (cutoff, _) = param(&ctl, NodePath::Effect { inst: 0, index: 0 }, "cutoff");

        let applied = ctl.apply(cable_to(lfo, osc, detune)).unwrap();
        assert_eq!(applied, Applied::restructured(1));
        assert!(applied.events.is_empty(), "nothing a client could apply: it is told by a snapshot");
        assert_eq!(
            sent(&engine),
            [command("AddFreeModTarget mod=0", &format!("LaneParam {{ lane: 0, slot: 0, param_index: {detune} }}"), &ctl, NodePath::Instrument(0), "detune")]
        );
        ctl.apply(cable_to(lfo, filter, cutoff)).unwrap();
        assert_eq!(
            sent(&engine),
            [command("AddFreeModTarget mod=0", &format!("LaneParam {{ lane: 0, slot: 1, param_index: {cutoff} }}"), &ctl, NodePath::Effect { inst: 0, index: 0 }, "cutoff")]
        );

        let targets = &ctl.model.free_modulators[0].targets;
        assert_eq!(targets.len(), 2);
        assert_eq!((targets[0].param_name.as_str(), targets[0].depth), ("detune", 0.5));
        assert_eq!(ctl.model.kind_of_target(ModulatorLocation::Free, 0, 0), Some(ModTargetKind::FreeParam { plugin: osc, param_index: detune }));
        assert_eq!((targets[0].param_min, targets[0].param_max), (-24.0, 24.0));
        assert_eq!((targets[1].param_name.as_str(), targets[1].depth), ("cutoff", 0.5));
        assert_eq!(ctl.model.kind_of_target(ModulatorLocation::Free, 0, 1), Some(ModTargetKind::FreeParam { plugin: filter, param_index: cutoff }));
        assert_ne!(targets[0].edge, targets[1].edge, "a cable of its own each");
        assert!(ctl.dirty);
        ctl.model.check_ids().unwrap();
        assert!(hearing_group(&ctl.model, 0)[0].targets.is_empty(), "the group's modulator is untouched");
    }

    /// Where a client may draw a cable from a modulator, as it can tell from the graph alone (`reachFrom` in `web/src/client/reach.ts` is the same rule): every plugin of the session, an instrument's
    /// (of a lane that has one) and an effect's, a lane's or a bus's. Every modulator is the session's own since step 96, and none has a parent.
    fn reach_by_parent(graph: &Graph, modulator: NodeId) -> Vec<NodeId> {
        assert!(graph.nodes.iter().find(|n| n.id == modulator).unwrap().parent.is_none(), "a modulator is in no lane and no group");
        let mut every: Vec<NodeId> = graph.nodes.iter().filter(|n| matches!(n.kind, NodeKind::Effect { .. } | NodeKind::Instrument { plugin: Some(_), .. })).map(|n| n.id).collect();
        every.sort();
        every
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
                    NodePath::Group(_) | NodePath::Modulator { .. } | NodePath::Source(_) => return None,
                };
                Some((id, slot.map_or(0, |plugin| plugin.params[0].index)))
            })
            .collect()
    }

    /// The plugins the controller lets `modulator` draw a cable to: a cable to each is tried on a session of its own.
    fn reach_by_controller(modulator: NodeId, plugins: &[(NodeId, u32)]) -> Vec<NodeId> {
        reach_by_controller_on(reaching, modulator, plugins)
    }

    /// The plugins the controller lets `modulator` draw a cable to, in the session that `make` makes (each cable is tried in a session of its own).
    fn reach_by_controller_on(make: fn() -> (StubEngine, Controller), modulator: NodeId, plugins: &[(NodeId, u32)]) -> Vec<NodeId> {
        let mut reached: Vec<NodeId> = plugins
            .iter()
            .filter(|&&(node, param)| {
                let (_engine, mut fresh) = make();
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
        // Every modulator reaches every plugin there is: the nine that have parameters (the lane with no plugin has nothing to draw a cable to).
        assert_eq!(reached, [9, 9, 9, 9, 9, 9]);
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
    fn add_mod_edge_reaches_what_a_client_can_tell_from_the_graph_for_a_modulator_that_is_in_no_lane_and_no_group_too() {
        let (_engine, ctl) = reaching_with_free();
        let graph = ctl.model.graph();
        let plugins = plugins_to_try(&ctl);
        let modulators = modulators_of(&ctl);
        assert_eq!(modulators.len(), 8, "six that the session has and two that were made");
        let mut reached = Vec::new();
        for modulator in modulators {
            let by_controller = reach_by_controller_on(reaching_with_free, modulator, &plugins);
            assert_eq!(by_controller, reach_by_parent(&graph, modulator), "{modulator:?}");
            reached.push(by_controller.len());
        }
        // Every one of them reaches every plugin there is (the lane that has none has nothing to draw a cable to).
        assert_eq!(reached, [9, 9, 9, 9, 9, 9, 9, 9]);
    }

    /// The session of `reaching_with_free()`, and what the controller lets each of its modulators draw a cable to: the plugins (`reach`), the settings and cable depths of other modulators (`fields`) and the volumes, mixes and pans that the host keeps (`gains`), as the
    /// web client's tests read it (`web/src/client/fixtures/freeReach.json`): they pin the rules the client draws its valid targets by (`reachFrom`, `fieldsOf`) to the controller's for a modulator that is in no lane
    /// and no group, which reaches every plugin and the other modulators like it. `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` rewrites it.
    #[test]
    fn the_web_clients_fixture_of_what_a_modulator_that_is_in_no_lane_reaches_is_current() {
        let (_engine, ctl) = reaching_with_free();
        let plugins = plugins_to_try(&ctl);
        let to_try = every_field_to_try(&ctl);
        let modulators = modulators_of(&ctl);
        let reach: std::collections::BTreeMap<String, Vec<NodeId>> =
            modulators.iter().map(|&modulator| (modulator.0.to_string(), reach_by_controller_on(reaching_with_free, modulator, &plugins))).collect();
        let fields: std::collections::BTreeMap<String, Vec<serde_json::Value>> = modulators
            .iter()
            .map(|&modulator| {
                let reached = fields_reached_by_the_controller_on(reaching_with_free, modulator, &to_try);
                (modulator.0.to_string(), reached.into_iter().map(|(node, port)| serde_json::json!({ "node": node, "port": port })).collect())
            })
            .collect();
        // The volume, the mix and the pan that the host keeps (step 97): each of the three is tried at every node there is, and the controller takes the ones that the node has.
        let gains_to_try: Vec<(NodeId, Port)> = ctl.model.graph().nodes.iter().flat_map(|node| [Port::Volume, Port::Mix, Port::Pan].map(move |port| (node.id, port))).collect();
        let gains: std::collections::BTreeMap<String, Vec<serde_json::Value>> = modulators
            .iter()
            .map(|&modulator| {
                let reached = fields_reached_by_the_controller_on(reaching_with_free, modulator, &gains_to_try);
                (modulator.0.to_string(), reached.into_iter().map(|(node, port)| serde_json::json!({ "node": node, "port": port })).collect())
            })
            .collect();
        // Through the text the wire carries, so that the numbers read as they do there.
        let wire = serde_json::to_string(&ctl.snapshot()).unwrap();
        let mut snapshot: serde_json::Value = serde_json::from_str(&wire).unwrap();
        // The layout's fingerprint comes from the hasher, which the standard library does not promise to keep the same.
        snapshot["telemetry"]["rev"] = serde_json::json!(1);
        let fixture = serde_json::json!({ "snapshot": snapshot, "reach": reach, "fields": fields, "gains": gains });
        let fresh = serde_json::to_string_pretty(&fixture).unwrap() + "\n";

        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/src/client/fixtures/freeReach.json");
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
        let lfo = ctl.model.free_modulators[0].node;
        let group_lfo = hearing_group(&ctl.model, 0)[0].node;
        let (osc, effect, group) = (ctl.model.instruments[0].node, ctl.model.instruments[0].effects[0].node, ctl.model.groups[0].node);
        let empty_lane = ctl.model.instruments[4].node;
        let (detune, _) = param(&ctl, NodePath::Instrument(0), "detune");
        let ghost = NodeId(9_999);
        let heading = ctl.model.instruments[0].instrument.as_ref().unwrap().params[1].index;
        ctl.model.instruments[0].instrument.as_mut().unwrap().params[1].kind = ParamKind::Separator;

        let not_a_parameter = invalid("a cable can only end at a plugin's parameter, at a volume, a mix or a pan, or at a modulator's rate, stage or depth");
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
            ("a parameter the plugin lacks", cable_to(lfo, osc, 999), invalid("the plugin has no parameter 999")),
            ("a heading, which is not a parameter", cable_to(lfo, osc, heading), invalid(format!("the plugin has no parameter {heading}"))),
        ];
        for (what, intent, error) in cases {
            assert_eq!(ctl.apply(intent), Err(error), "{what}");
        }
        // The ports that are neither a parameter nor a modulator's setting.
        for port in [Port::ModOut, Port::AudioIn, Port::AudioOut] {
            let intent = Intent::AddModEdge { modulator: lfo, to: Endpoint { node: osc, port } };
            assert_eq!(ctl.apply(intent), Err(not_a_parameter.clone()), "{port:?}");
        }
        // A setting of a node that is not a modulator (a plugin's node is not).
        for port in [Port::Rate, Port::Attack, Port::Sustain, Port::Depth { edge: EdgeId(0) }] {
            let intent = Intent::AddModEdge { modulator: lfo, to: Endpoint { node: osc, port } };
            assert_eq!(ctl.apply(intent), Err(invalid("a cable ends at the settings of a modulator, and that is not one")), "{port:?}");
        }

        assert!(sent(&engine).is_empty(), "nothing reached the audio thread");
        assert_eq!((ctl.rev, ctl.dirty), (0, false), "a refusal is not a revision");
        assert_eq!(ctl.model.slots.high_water(), slots, "no telemetry slot is used up for nothing");
        assert!(ctl.model.free_modulators.iter().all(|m| m.targets.is_empty()));
        // And no cable id was used up: the first cable that is drawn is the first there is.
        let (cutoff, _) = param(&ctl, NodePath::Effect { inst: 0, index: 0 }, "cutoff");
        ctl.apply(cable_to(lfo, effect, cutoff)).unwrap();
        assert_eq!(ctl.model.free_modulators[0].targets[0].edge, EdgeId(0));
    }

    #[test]
    fn add_mod_edge_refuses_a_modulator_that_drives_the_parameter_already_but_not_another_one() {
        let (engine, mut ctl) = reaching();
        let lfo = ctl.model.free_modulators[0].node;
        let osc = ctl.model.instruments[0].node;
        let (detune, _) = param(&ctl, NodePath::Instrument(0), "detune");
        let (volume, _) = param(&ctl, NodePath::Instrument(0), "volume");
        ctl.apply(cable_to(lfo, osc, detune)).unwrap();
        sent(&engine);
        let rev = ctl.rev;
        assert_eq!(ctl.apply(cable_to(lfo, osc, detune)), Err(invalid("that modulator drives that parameter already")));
        assert!(sent(&engine).is_empty());
        assert_eq!(ctl.rev, rev, "a refusal is not a revision");
        assert_eq!(ctl.model.free_modulators[0].targets.len(), 1);

        // Another parameter of the same plugin is another cable; another modulator, of the lane or of the group, may drive the same one.
        ctl.apply(cable_to(lfo, osc, volume)).unwrap();
        let parent = lane(&ctl);
        add(&mut ctl, parent, ModulatorKind::Envelope).unwrap();
        let envelope = ctl.model.free_modulators[1].node;
        ctl.apply(cable_to(envelope, osc, detune)).unwrap();
        let group_lfo = hearing_group(&ctl.model, 0)[0].node;
        ctl.apply(cable_to(group_lfo, osc, detune)).unwrap();
        assert_eq!(ctl.model.free_modulators[0].targets.len(), 2);
        assert_eq!(ctl.model.free_modulators[1].targets.len(), 1);
        assert_eq!(hearing_group(&ctl.model, 0)[0].targets.len(), 1);
        ctl.model.check_ids().unwrap();
    }

    // -- Cables from one modulator to the settings of another --

    /// `CROSS_MOD_RACKS` over the stub engine: a group of three modulators, whose first member has three modulators of its own (the
    /// second an envelope with two cables), a second member with an LFO, a lane in no group and a lane in a second group.
    fn racks() -> (StubEngine, Controller) {
        let (handles, engine) = testutil::stub_engine();
        (engine, Controller::new(load(testutil::CROSS_MOD_RACKS).model, handles, None))
    }

    fn rack_mod(ctl: &Controller, inst: usize, index: usize) -> NodeId {
        hearing_lane(&ctl.model, inst)[index].node
    }

    fn in_group(ctl: &Controller, group: usize, index: usize) -> NodeId {
        hearing_group(&ctl.model, group)[index].node
    }

    fn cable_to_field(modulator: NodeId, node: NodeId, port: Port) -> Intent {
        Intent::AddModEdge { modulator, to: Endpoint { node, port } }
    }

    #[test]
    fn add_mod_edge_draws_a_cable_from_a_lane_modulator_to_the_settings_of_a_sibling_and_to_the_depth_of_a_cable_of_its() {
        let (engine, mut ctl) = racks();
        sent(&engine);
        let (lfo, envelope) = (rack_mod(&ctl, 0, 0), rack_mod(&ctl, 0, 1));

        let applied = ctl.apply(cable_to_field(lfo, envelope, Port::Attack)).unwrap();
        assert_eq!(applied, Applied::restructured(1));
        assert!(applied.events.is_empty(), "nothing a client could apply: it is told by a snapshot");
        assert_eq!(sent(&engine), [testutil::told(&ctl.model, "AddFreeModTarget mod=0 kind=ModulatorAttack { mod_index: 1 } depth=0.500 base=0.010 range=[0.001, 10.000]")]);
        for (port, text) in [
            (Port::Decay, "kind=ModulatorDecay { mod_index: 1 } depth=0.500 base=0.300 range=[0.001, 10.000]"),
            (Port::Sustain, "kind=ModulatorSustain { mod_index: 1 } depth=0.500 base=0.400 range=[0.000, 1.000]"),
            (Port::Release, "kind=ModulatorRelease { mod_index: 1 } depth=0.500 base=0.500 range=[0.001, 10.000]"),
        ] {
            ctl.apply(cable_to_field(lfo, envelope, port)).unwrap_or_else(|e| panic!("{port:?}: {e}"));
            assert_eq!(sent(&engine), [testutil::told(&ctl.model, &format!("AddFreeModTarget mod=0 {text}"))]);
        }
        // The depth of the envelope's second cable, which is named by the cable.
        let edge = ctl.model.free_modulators[1].targets[1].edge;
        ctl.apply(cable_to_field(lfo, envelope, Port::Depth { edge })).unwrap();
        assert_eq!(
            sent(&engine),
            [testutil::told(&ctl.model, "AddFreeModTarget mod=0 kind=ModulatorDepth { mod_index: 1, target_index: 1 } depth=0.500 base=0.700 range=[-1.000, 1.000]")]
        );
        // And the rate of the sibling that is an LFO, from the envelope: the other way.
        ctl.apply(cable_to_field(envelope, lfo, Port::Rate)).unwrap();
        assert_eq!(sent(&engine), [testutil::told(&ctl.model, "AddFreeModTarget mod=1 kind=ModulatorRate { mod_index: 0 } depth=0.500 base=3.000 range=[0.010, 50.000]")]);

        // The model has them, as the TUI's selector binds them: the label says whose, the depth is the new cable's.
        let targets = &ctl.model.free_modulators[0].targets;
        assert_eq!(
            targets.iter().map(|t| (t.param_name.as_str(), t.depth)).collect::<Vec<_>>(),
            [("detune", 0.1), ("Mod 1 attack", 0.5), ("Mod 1 decay", 0.5), ("Mod 1 sustain", 0.5), ("Mod 1 release", 0.5), ("Mod 1 detune depth", 0.5)]
        );
        assert_eq!(
            targets.iter().skip(1).map(|t| (t.param_min, t.param_max)).collect::<Vec<_>>(),
            [(0.001, 10.0), (0.001, 10.0), (0.0, 1.0), (0.001, 10.0), (-1.0, 1.0)]
        );
        // (Each takes a slot for the value that its setting is set to, which the engine publishes: f132.)
        let slots: Vec<_> = targets.iter().map(|t| t.effective).collect();
        assert!(slots.iter().all(Option::is_some), "every cable has an effective-value slot: {slots:?}");
        assert_eq!(slots.iter().collect::<std::collections::HashSet<_>>().len(), slots.len(), "and each its own");
        // The graph draws each as a cable from the modulator's output to the port of the setting.
        let graph = ctl.model.graph();
        let cable = |from: NodeId, to: NodeId, port: Port| {
            graph.edges.iter().any(|e| e.from.node == from && e.to == Endpoint { node: to, port } && matches!(e.kind, crate::app::graph::EdgeKind::Modulation { .. }))
        };
        assert!(cable(lfo, envelope, Port::Attack) && cable(lfo, envelope, Port::Release) && cable(lfo, envelope, Port::Depth { edge }) && cable(envelope, lfo, Port::Rate));
        assert!(ctl.dirty);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn add_mod_edge_draws_cables_between_two_modulators_both_ways_as_the_tui_does() {
        let (_engine, mut ctl) = racks();
        let (lfo, envelope) = (rack_mod(&ctl, 0, 0), rack_mod(&ctl, 0, 1));
        ctl.apply(cable_to_field(lfo, envelope, Port::Sustain)).unwrap();
        ctl.apply(cable_to_field(envelope, lfo, Port::Rate)).unwrap();
        assert_eq!(ctl.model.free_modulators[0].targets.len(), 2);
        assert_eq!(ctl.model.free_modulators[1].targets.len(), 3);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn add_mod_edge_refuses_the_settings_it_cannot_reach_and_changes_nothing() {
        let (engine, mut ctl) = racks();
        sent(&engine);
        let slots = ctl.model.slots.high_water();
        let (lfo, envelope, midi) = (rack_mod(&ctl, 0, 0), rack_mod(&ctl, 0, 1), rack_mod(&ctl, 0, 2));
        let (group_lfo, group_envelope) = (in_group(&ctl, 0, 0), in_group(&ctl, 0, 1));
        let (lane, group) = (ctl.model.instruments[0].node, ctl.model.groups[0].node);
        let ghost = NodeId(9_999);
        let own_cable = ctl.model.free_modulators[0].targets[0].edge;
        let envelope_cable = ctl.model.free_modulators[1].targets[0].edge;
        let other_cable = hearing_lane(&ctl.model, 2)[0].targets.first().map(|t| t.edge);
        assert!(other_cable.is_none(), "(the lane in no group has no cable)");

        let own = invalid("a modulator cannot drive its own settings");
        let not_a_modulator = invalid("a cable ends at the settings of a modulator, and that is not one");
        let cases = [
            ("its own rate", cable_to_field(lfo, lfo, Port::Rate), own.clone()),
            ("the depth of its own cable", cable_to_field(lfo, lfo, Port::Depth { edge: own_cable }), own.clone()),
            ("a group modulator's own stage", cable_to_field(group_envelope, group_envelope, Port::Attack), own),
            ("a node that is not there", cable_to_field(lfo, ghost, Port::Rate), IntentError::NotFound(ghost)),
            ("the master", cable_to_field(lfo, NodeId::MASTER, Port::Rate), IntentError::NotFound(NodeId::MASTER)),
            ("a lane", cable_to_field(lfo, lane, Port::Rate), not_a_modulator.clone()),
            ("a group", cable_to_field(group_lfo, group, Port::Rate), not_a_modulator),
            ("an attack an LFO has not", cable_to_field(envelope, lfo, Port::Attack), invalid("that modulator has no attack")),
            ("a rate an envelope has not", cable_to_field(lfo, envelope, Port::Rate), invalid("that modulator has no rate")),
            ("a setting of a MIDI control", cable_to_field(lfo, midi, Port::Rate), invalid("that modulator has no rate")),
            ("a sustain of a MIDI control", cable_to_field(lfo, midi, Port::Sustain), invalid("that modulator has no sustain")),
            ("the depth of a cable that is not there", cable_to_field(lfo, envelope, Port::Depth { edge: EdgeId(9_999) }), IntentError::EdgeNotFound(EdgeId(9_999))),
            ("the depth of another modulator's cable", cable_to_field(lfo, envelope, Port::Depth { edge: own_cable }), IntentError::EdgeNotFound(own_cable)),
            ("the depth of the MIDI control's cable, named as the envelope's", cable_to_field(lfo, midi, Port::Depth { edge: envelope_cable }), IntentError::EdgeNotFound(envelope_cable)),
        ];
        for (what, intent, error) in cases {
            assert_eq!(ctl.apply(intent), Err(error), "{what}");
        }
        // The ports that are neither a parameter nor a setting.
        for port in [Port::ModOut, Port::AudioIn, Port::AudioOut] {
            let intent = cable_to_field(lfo, envelope, port);
            assert_eq!(ctl.apply(intent), Err(invalid("a cable can only end at a plugin's parameter, at a volume, a mix or a pan, or at a modulator's rate, stage or depth")), "{port:?}");
        }

        assert!(sent(&engine).is_empty(), "nothing reached the audio thread");
        assert_eq!((ctl.rev, ctl.dirty), (0, false), "a refusal is not a revision");
        assert_eq!(ctl.model.slots.high_water(), slots, "no telemetry slot is used up for nothing");
        assert_eq!(ctl.model.free_modulators[0].targets.len(), 1, "nothing was bound");
    }

    /// The settings and the cable depths the controller lets `modulator` draw a cable to, on the session of `racks()`: a cable to each
    /// is tried on a session of its own, and the ones that are drawn are (the node of the modulator that has it, and the port).
    fn fields_reached_by_the_controller(modulator: NodeId, ports: &[(NodeId, Port)]) -> Vec<(NodeId, Port)> {
        fields_reached_by_the_controller_on(racks, modulator, ports)
    }

    /// The same on the session that `make` makes.
    fn fields_reached_by_the_controller_on(make: fn() -> (StubEngine, Controller), modulator: NodeId, ports: &[(NodeId, Port)]) -> Vec<(NodeId, Port)> {
        ports
            .iter()
            .copied()
            .filter(|&(node, port)| {
                let (_engine, mut fresh) = make();
                fresh.apply(cable_to_field(modulator, node, port)).is_ok()
            })
            .collect()
    }

    /// Every setting and every cable depth of every modulator of a session (that of `racks()`, or of `reaching_with_free()`), that a cable could be tried to.
    fn every_field_to_try(ctl: &Controller) -> Vec<(NodeId, Port)> {
        let mut ports = Vec::new();
        for (node, path) in ctl.model.nodes() {
            let NodePath::Modulator { at, index } = path else { continue };
            let modulator = &ctl.model.modulators(at).expect("the rack is there")[index];
            ports.extend([Port::Rate, Port::Attack, Port::Decay, Port::Sustain, Port::Release].map(|port| (node, port)));
            ports.extend(modulator.targets.iter().map(|t| (node, Port::Depth { edge: t.edge })));
        }
        ports
    }

    /// The session of `racks()`, and which settings and cable depths the controller lets each of its modulators draw a cable to, as the
    /// web client's tests read it (`web/src/client/fixtures/fields.json`): they pin the rule the client draws its valid targets by
    /// (`fieldsOf`) to the controller's. `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` rewrites it.
    #[test]
    fn the_web_clients_fixture_of_what_a_modulator_reaches_of_the_others_is_current() {
        let (_engine, ctl) = racks();
        let to_try = every_field_to_try(&ctl);
        let modulators: Vec<NodeId> = ctl.model.nodes().filter(|(_, path)| matches!(path, NodePath::Modulator { .. })).map(|(id, _)| id).collect();
        assert_eq!(modulators.len(), 9, "three in the group, three and one in the first group's lanes, one in each of the others");
        let fields: std::collections::BTreeMap<String, Vec<serde_json::Value>> = modulators
            .into_iter()
            .map(|modulator| {
                let reached = fields_reached_by_the_controller(modulator, &to_try);
                let wire = reached.into_iter().map(|(node, port)| serde_json::json!({ "node": node, "port": port })).collect();
                (modulator.0.to_string(), wire)
            })
            .collect();
        let wire = serde_json::to_string(&ctl.snapshot()).unwrap();
        let mut snapshot: serde_json::Value = serde_json::from_str(&wire).unwrap();
        // The layout's fingerprint comes from the hasher, which the standard library does not promise to keep the same.
        snapshot["telemetry"]["rev"] = serde_json::json!(1);
        let fixture = serde_json::json!({ "snapshot": snapshot, "fields": fields });
        let fresh = serde_json::to_string_pretty(&fixture).unwrap() + "\n";

        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/src/client/fixtures/fields.json");
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

    /// `LONG_CHAINS` (a bus of four effects, a grouped lane with four and a lane in no group with three) over the stub engine.
    fn long_chains() -> (StubEngine, Controller) {
        let (handles, engine) = testutil::stub_engine();
        (engine, Controller::new(load(testutil::LONG_CHAINS).model, handles, None))
    }

    /// The session of `long_chains()`, and what the controller makes of asking it to move each of its nodes to each of some places, as
    /// the web client's tests read it (`web/src/client/fixtures/moves.json`): they pin the mock engine's moves to the controller's, node
    /// by node and place by place. A move that is done gives the order of the nodes of the graph afterwards and the links of the audio
    /// path (the node each leaves, the node it goes to, and its kind: `audio` or `member`), in the order of the graph's edges; the cables
    /// and the layout of the telemetry are asserted to be what they were. One that is refused gives the controller's words.
    /// `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` rewrites it.
    #[test]
    fn the_web_clients_fixture_of_what_moving_an_effect_comes_to_is_current() {
        use crate::app::graph::EdgeKind;
        let (_engine, ctl) = long_chains();
        let before = ctl.snapshot();
        let cables = |snapshot: &crate::app::snapshot::Snapshot| -> Vec<crate::app::graph::Edge> {
            snapshot.graph.edges.iter().filter(|e| matches!(e.kind, EdgeKind::Modulation { .. })).cloned().collect()
        };
        // Every node of the session, the master, and one that is not there; an effect at each place of a chain and past it, the others at one.
        let mut nodes: Vec<(NodeId, bool)> = ctl.model.nodes().map(|(id, path)| (id, matches!(path, NodePath::Effect { .. } | NodePath::GroupEffect { .. }))).collect();
        nodes.extend([(NodeId::MASTER, false), (NodeId(9_999), false)]);
        let mut moves = Vec::new();
        for (node, is_effect) in nodes {
            let places: &[u32] = if is_effect { &[0, 1, 2, 3, 4, 99] } else { &[0] };
            for &index in places {
                let (_engine, mut fresh) = long_chains();
                let result = match fresh.apply(Intent::MoveEffect { node, index }) {
                    Err(error) => serde_json::json!({ "node": node, "index": index, "error": error.to_string() }),
                    Ok(_) => {
                        let after = fresh.snapshot();
                        assert_eq!(cables(&after), cables(&before), "a move leaves the cables as they were: {node:?} to {index}");
                        assert_eq!(after.telemetry, before.telemetry, "and the layout of the telemetry: {node:?} to {index}");
                        let audio: Vec<_> = after
                            .graph
                            .edges
                            .iter()
                            .filter(|e| !matches!(e.kind, EdgeKind::Modulation { .. }))
                            .map(|e| serde_json::json!([e.from.node, e.to.node, if matches!(e.kind, EdgeKind::Member) { "member" } else { "audio" }]))
                            .collect();
                        let order: Vec<_> = after.graph.nodes.iter().map(|n| n.id).collect();
                        serde_json::json!({ "node": node, "index": index, "nodes": order, "audio": audio })
                    }
                };
                moves.push(result);
            }
        }
        let wire = serde_json::to_string(&before).unwrap();
        let mut snapshot: serde_json::Value = serde_json::from_str(&wire).unwrap();
        // The layout's fingerprint comes from the hasher, which the standard library does not promise to keep the same.
        snapshot["telemetry"]["rev"] = serde_json::json!(1);
        let fixture = serde_json::json!({ "snapshot": snapshot, "moves": moves });
        let fresh = serde_json::to_string_pretty(&fixture).unwrap() + "\n";

        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/src/client/fixtures/moves.json");
        if std::env::var_os("TANG_UPDATE_TYPES").is_some() {
            std::fs::write(&path, &fresh).unwrap();
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            committed == fresh,
            "{} is not what moving an effect comes to now: run `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` and commit the result",
            path.display()
        );
    }

    #[test]
    fn add_mod_edge_refuses_a_cable_to_a_setting_that_the_modulator_drives_already() {
        let (engine, mut ctl) = racks();
        let (lfo, envelope) = (rack_mod(&ctl, 0, 0), rack_mod(&ctl, 0, 1));
        ctl.apply(cable_to_field(lfo, envelope, Port::Release)).unwrap();
        sent(&engine);
        let rev = ctl.rev;
        assert_eq!(ctl.apply(cable_to_field(lfo, envelope, Port::Release)), Err(invalid("that modulator drives that setting already")));
        assert!(sent(&engine).is_empty());
        assert_eq!(ctl.rev, rev, "a refusal is not a revision");
        // Another setting of the same modulator, or the same setting from another modulator, is another cable.
        ctl.apply(cable_to_field(lfo, envelope, Port::Attack)).unwrap();
        let midi = rack_mod(&ctl, 0, 2);
        ctl.apply(cable_to_field(midi, envelope, Port::Release)).unwrap();
        let group_lfo = in_group(&ctl, 0, 0);
        let edge = ctl.model.free_modulators[1].targets[0].edge;
        ctl.apply(cable_to_field(group_lfo, envelope, Port::Depth { edge })).unwrap();
        assert_eq!(ctl.apply(cable_to_field(group_lfo, envelope, Port::Depth { edge })), Err(invalid("that modulator drives that setting already")));
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_cable_to_a_setting_is_taken_out_and_gives_the_setting_back_what_the_user_set() {
        let (engine, mut ctl) = racks();
        let (lfo, envelope) = (rack_mod(&ctl, 0, 0), rack_mod(&ctl, 0, 1));
        ctl.apply(cable_to_field(lfo, envelope, Port::Sustain)).unwrap();
        let edge = ctl.model.free_modulators[0].targets[1].edge;
        sent(&engine);
        let applied = ctl.apply(Intent::SetEdgeDepth { edge, depth: -0.25 }).unwrap();
        assert_eq!(applied.events, [Event::EdgeDepthSet { edge, depth: -0.25 }]);
        assert_eq!(sent(&engine), ["SetFreeModTargetDepth mod=0 target=1 depth=-0.250"]);
        ctl.apply(Intent::RemoveEdge { edge }).unwrap();
        let commands = sent(&engine);
        assert_eq!(
            commands,
            ["RemoveFreeModTarget mod=0 target=1", "SetFreeModulatorEnvelopeParam mod=1 attack=0.010 decay=0.300 sustain=0.400 release=0.500"],
            "and the envelope has the stages the user set"
        );
        assert_eq!(ctl.model.free_modulators[0].targets.len(), 1);
    }

    #[test]
    fn a_cable_that_was_drawn_is_turned_and_taken_out_by_the_intents_that_name_a_cable() {
        let (engine, mut ctl) = reaching();
        let lfo = ctl.model.free_modulators[0].node;
        let osc = ctl.model.instruments[0].node;
        let (detune, _) = param(&ctl, NodePath::Instrument(0), "detune");
        ctl.apply(cable_to(lfo, osc, detune)).unwrap();
        let edge = ctl.model.free_modulators[0].targets[0].edge;
        sent(&engine);

        let applied = ctl.apply(Intent::SetEdgeDepth { edge, depth: -0.25 }).unwrap();
        assert_eq!(applied.events, [Event::EdgeDepthSet { edge, depth: -0.25 }]);
        assert_eq!(sent(&engine), ["SetFreeModTargetDepth mod=0 target=0 depth=-0.250"]);
        ctl.apply(Intent::RemoveEdge { edge }).unwrap();
        assert_eq!(sent(&engine), ["RemoveFreeModTarget mod=0 target=0", "SetParameter inst=0 slot=0 param=1 value=0.000"], "and the detune is the one the user set");
        assert!(ctl.model.free_modulators[0].targets.is_empty());
        // Drawn again, it is another cable.
        ctl.apply(cable_to(lfo, osc, detune)).unwrap();
        assert_ne!(ctl.model.free_modulators[0].targets[0].edge, edge);
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
            ["RemoveFreeModulator index=0", "SetParameter inst=0 slot=1 param=0 value=1.000", "SetFreeModulatorRate mod=2 rate=1.000"]
        );
        let rack = &ctl.model.free_modulators;
        let (group_lfo, group_envelope) = (group_mod(&ctl, 0), group_mod(&ctl, 1));
        assert_eq!(rack.iter().map(|m| m.node).collect::<Vec<_>>(), [envelope, learn, last, group_lfo, group_envelope], "the others stay, in their order");
        assert!(rack[..3].iter().all(|m| m.targets.is_empty()));
        assert!(ctl.model.locate(first).is_none(), "its node is gone");
        assert_eq!(ctl.model.locate(last), Some(NodePath::Modulator { at: ModulatorLocation::Free, index: 2 }), "and the later ones are a place up");
        assert_eq!(hearing_group(&ctl.model, 0).len(), 2, "the group's rack is untouched");
        assert!(ctl.dirty);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn remove_node_first_takes_out_the_cables_of_the_others_that_drive_the_modulator() {
        let (engine, mut ctl) = modulated();
        sent(&engine);
        // The first modulator's second cable drives the rate of the fourth.
        let (first, fourth) = (lane_mod(&ctl, 0), lane_mod(&ctl, 3));
        assert_eq!(ctl.model.free_modulators[0].targets.len(), 2);
        remove(&mut ctl, fourth).unwrap();
        assert_eq!(sent(&engine), ["RemoveFreeModTarget mod=0 target=1", "RemoveFreeModulator index=3"]);
        let rack = &ctl.model.free_modulators;
        assert_eq!(rack.len(), 5, "the lane's other three and the group's two");
        assert_eq!(rack[0].node, first);
        assert_eq!(rack[0].targets.len(), 1, "the cable to the cutoff is as it was");
        assert_eq!(rack[0].targets[0].depth, 0.4);
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
        let added = ctl.model.free_modulators.last().unwrap().node;
        assert!(added != gone && ctl.model.nodes().all(|(id, _)| id != gone));
    }

    #[test]
    fn remove_node_refuses_what_is_not_there_the_master_and_the_last_lane_and_changes_nothing() {
        let (engine, mut ctl) = modulated();
        sent(&engine);
        let lane_node = lane(&ctl);
        let ghost = NodeId(9_999);
        let cases = [
            ("a node that is not there", ghost, IntentError::NotFound(ghost)),
            ("the master, which the model does not have", NodeId::MASTER, IntentError::NotFound(NodeId::MASTER)),
            ("the only lane there is", lane_node, invalid("a session keeps its last lane")),
        ];
        for (what, node, error) in cases {
            assert_eq!(remove(&mut ctl, node), Err(error), "{what}");
        }
        assert!(sent(&engine).is_empty(), "nothing reached the audio thread");
        assert_eq!((ctl.rev, ctl.dirty), (0, false), "a refusal is not a revision");
        assert_eq!(ctl.model.free_modulators.len(), 6);
        assert_eq!(hearing_group(&ctl.model, 0).len(), 2);
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
                "RemoveFreeModTarget mod=1 target=0",
                "RemoveFreeModTarget mod=0 target=0",
                "RemoveFreeModTarget mod=3 target=0",
                "RemoveEffect inst=0 index=0 keep",
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
        assert_eq!(sent(&engine), ["RemoveFreeModTarget mod=3 target=3", "RemoveGroupEffect group=0 index=0 keep"]);
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

    fn move_to(ctl: &mut Controller, node: NodeId, index: u32) -> Result<Applied, IntentError> {
        ctl.apply(Intent::MoveEffect { node, index })
    }

    #[test]
    fn move_effect_puts_an_effect_at_a_place_of_its_lanes_chain_and_says_the_structure_changed() {
        use crate::plugin::chain::ModTargetKind as K;
        let (engine, mut ctl) = chains();
        sent(&engine);
        // Lane 0's filter, which two modulators drive, goes after the reverb.
        let (filter, reverb) = (ctl.model.instruments[0].effects[0].node, ctl.model.instruments[0].effects[1].node);
        let applied = move_to(&mut ctl, filter, 1).unwrap();
        assert_eq!(applied, Applied::restructured(1));
        assert!(applied.events.is_empty(), "nothing a client could apply: it is told by a snapshot");
        assert_eq!(sent(&engine), ["ReorderEffect inst=0 from=0 to=1"]);
        let chain: Vec<_> = ctl.model.instruments[0].effects.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(chain, ["Reverb", "Filter"]);
        assert!(ctl.dirty);
        // The nodes are the ones they were, in other places.
        assert_eq!(ctl.model.locate(filter), Some(NodePath::Effect { inst: 0, index: 1 }));
        assert_eq!(ctl.model.locate(reverb), Some(NodePath::Effect { inst: 0, index: 0 }));
        // The lane's first modulator drove the filter's cutoff, the reverb's damping and the oscillator's detune: it drives the same plugins in their new places.
        let kinds = testutil::kinds_at(&ctl.model, ModulatorLocation::Free, 0);
        let osc = ctl.model.instruments[0].node;
        assert_eq!(kinds, [K::FreeParam { plugin: filter, param_index: 0 }, K::FreeParam { plugin: reverb, param_index: 2 }, K::FreeParam { plugin: osc, param_index: 1 }]);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn move_effect_puts_a_bus_effect_at_a_place_of_its_groups_bus_by_the_groups_command() {
        use crate::plugin::chain::ModTargetKind as K;
        let (engine, mut ctl) = chains();
        sent(&engine);
        // The group's reverb (1) goes before its filter (0).
        let reverb = ctl.model.groups[0].effects[1].node;
        let applied = move_to(&mut ctl, reverb, 0).unwrap();
        assert_eq!(applied, Applied::restructured(1));
        assert!(applied.events.is_empty());
        assert_eq!(sent(&engine), ["ReorderGroupEffect group=0 from=1 to=0"]);
        let bus: Vec<_> = ctl.model.groups[0].effects.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(bus, ["Reverb", "Filter"]);
        assert_eq!(ctl.model.locate(reverb), Some(NodePath::GroupEffect { group: 0, index: 0 }));
        // The cables to the bus follow the plugins (each is on the plugin that it drove, which is another place of the bus now).
        let group_modulator = ctl.model.free_modulators.iter().position(|m| m.ear == Some(ctl.model.groups[0].node)).expect("a modulator that hears the group");
        let bus_cables: Vec<_> = testutil::kinds_at(&ctl.model, ModulatorLocation::Free, group_modulator).into_iter().filter(|k| matches!(k, K::FreeParam { plugin, .. } if ctl.model.groups[0].effects.iter().any(|e| e.node == *plugin))).collect();
        let (filter, reverb_node) = (ctl.model.groups[0].effects[1].node, reverb);
        assert_eq!(bus_cables, [K::FreeParam { plugin: filter, param_index: 0 }, K::FreeParam { plugin: reverb_node, param_index: 2 }]);
        assert!(ctl.dirty);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn an_effect_that_is_moved_and_moved_back_leaves_the_graph_as_it_was() {
        let (_engine, mut ctl) = chains();
        let before = ctl.model.graph();
        let (filter, reverb) = (ctl.model.instruments[0].effects[0].node, ctl.model.instruments[0].effects[1].node);
        move_to(&mut ctl, filter, 1).unwrap();
        let swapped = ctl.model.graph();
        assert_ne!(swapped, before);
        move_to(&mut ctl, filter, 0).unwrap();
        assert_eq!(ctl.model.graph(), before, "the same effect, back where it was");
        // Of two effects, moving the other one the other way makes the same chain.
        move_to(&mut ctl, reverb, 0).unwrap();
        assert_eq!(ctl.model.graph(), swapped);
        assert_eq!(ctl.rev, 3);
        // And the same on the bus.
        let (bus_filter, bus_reverb) = (ctl.model.groups[0].effects[0].node, ctl.model.groups[0].effects[1].node);
        let before = ctl.model.graph();
        move_to(&mut ctl, bus_filter, 1).unwrap();
        let swapped = ctl.model.graph();
        move_to(&mut ctl, bus_filter, 0).unwrap();
        assert_eq!(ctl.model.graph(), before);
        move_to(&mut ctl, bus_reverb, 0).unwrap();
        assert_eq!(ctl.model.graph(), swapped);
    }

    #[test]
    fn move_effect_refuses_what_is_no_effect_a_place_the_chain_has_not_and_the_place_the_effect_is_at_and_changes_nothing() {
        let (engine, mut ctl) = chains();
        sent(&engine);
        let (lane_node, group) = (ctl.model.instruments[0].node, ctl.model.groups[0].node);
        let (filter, bus_filter, lone) = (ctl.model.instruments[0].effects[0].node, ctl.model.groups[0].effects[0].node, ctl.model.instruments[2].effects[0].node);
        let lfo = ctl.model.free_modulators[0].node;
        let ghost = NodeId(9_999);
        let cases: [(&str, NodeId, u32, IntentError); 13] = [
            ("a lane", lane_node, 0, invalid("only an effect can be moved")),
            ("a group", group, 0, invalid("only an effect can be moved")),
            ("a modulator", lfo, 0, invalid("only an effect can be moved")),
            ("a node that is not there", ghost, 0, IntentError::NotFound(ghost)),
            ("the master, which the model does not have", NodeId::MASTER, 0, IntentError::NotFound(NodeId::MASTER)),
            ("a place past the end of the chain", filter, 2, invalid("the chain has no place 2")),
            ("a place far past the end of the chain", filter, u32::MAX, invalid(format!("the chain has no place {}", u32::MAX))),
            ("the place the effect is at", filter, 0, invalid("that effect is at that place already")),
            ("the only effect of a chain, where it is", lone, 0, invalid("that effect is at that place already")),
            ("the only effect of a chain, after it", lone, 1, invalid("the chain has no place 1")),
            ("a place past the end of the bus", bus_filter, 2, invalid("the chain has no place 2")),
            ("the place the bus effect is at", bus_filter, 0, invalid("that effect is at that place already")),
            ("a place past the end of the bus, far", bus_filter, u32::MAX, invalid(format!("the chain has no place {}", u32::MAX))),
        ];
        for (what, node, index, error) in cases {
            assert_eq!(move_to(&mut ctl, node, index), Err(error), "{what}");
        }
        assert!(sent(&engine).is_empty(), "nothing reached the audio thread");
        assert_eq!((ctl.rev, ctl.dirty), (0, false), "a refusal is not a revision");
        let chains: Vec<Vec<_>> = ctl.model.instruments.iter().map(|lane| lane.effects.iter().map(|e| e.name.as_str()).collect()).collect();
        assert_eq!(chains, [vec!["Filter", "Reverb"], vec![], vec!["Filter"]]);
        let bus: Vec<_> = ctl.model.groups[0].effects.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(bus, ["Filter", "Reverb"]);
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

    /// The files that a directory has, by name.
    fn files_in(dir: &std::path::Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    fn save_as(name: &str) -> Intent {
        Intent::SaveAs { name: name.to_string() }
    }

    #[test]
    fn save_as_writes_beside_the_sessions_file_and_the_session_is_that_file_from_then_on() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("song.toml");
        let (engine, mut ctl) = controller_with_file(&old);
        let osc = lane(&ctl);
        ctl.apply(Intent::SetGain { node: osc, gain: Gain::Volume, value: 0.25 }).unwrap();
        sent(&engine);
        let applied = ctl.apply(save_as("copy")).unwrap();
        // It is told as a save is, with the name the session has now, and is a revision.
        assert_eq!(applied, Applied::of(2, vec![Event::Saved { file: Some("copy.toml".to_string()) }]));
        assert_eq!(ctl.rev, 2);
        assert!(!ctl.dirty, "nothing is unsaved now");
        assert_eq!(ctl.session_path.as_deref(), Some(dir.path().join("copy.toml").as_path()));
        assert_eq!(files_in(dir.path()), ["copy.toml"], "the file it had is not written");
        assert!(sent(&engine).is_empty(), "the audio thread has nothing to do with it");
        let text = std::fs::read_to_string(dir.path().join("copy.toml")).unwrap();
        testutil::assert_same_model("the copy", &ctl.model, &load(&text).model);

        // A save after it is to the new file.
        ctl.apply(Intent::SetGain { node: osc, gain: Gain::Volume, value: 0.5 }).unwrap();
        ctl.apply(Intent::Save).unwrap();
        assert!(std::fs::read_to_string(dir.path().join("copy.toml")).unwrap().contains("volume = 0.5"));
        assert_eq!(files_in(dir.path()), ["copy.toml"]);
    }

    #[test]
    fn save_as_writes_the_tempo_as_a_save_does() {
        let dir = tempfile::tempdir().unwrap();
        let (_engine, mut ctl) = controller_with_file(&dir.path().join("song.toml"));
        ctl.model.bpm = 87.5;
        ctl.apply(save_as("copy")).unwrap();
        let text = std::fs::read_to_string(dir.path().join("copy.toml")).unwrap();
        assert_eq!(load(&text).model.bpm, 87.5);
    }

    #[test]
    fn save_as_the_name_the_session_has_is_a_save_though_the_file_is_there() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("song.toml");
        let (_engine, mut ctl) = controller_with_file(&path);
        ctl.apply(Intent::Save).unwrap();
        let osc = lane(&ctl);
        ctl.apply(Intent::SetGain { node: osc, gain: Gain::Volume, value: 0.25 }).unwrap();
        // (With the extension, without it, and with spaces round it: it is the same name.)
        for given in ["song", "song.toml", "  song  "] {
            ctl.apply(Intent::SetGain { node: osc, gain: Gain::Volume, value: 0.25 }).unwrap();
            let applied = ctl.apply(save_as(given)).unwrap_or_else(|why| panic!("{given:?}: {why}"));
            assert_eq!(applied.events, [Event::Saved { file: Some("song.toml".to_string()) }]);
            assert!(!ctl.dirty);
        }
        assert_eq!(files_in(dir.path()), ["song.toml"]);
        assert!(std::fs::read_to_string(&path).unwrap().contains("volume = 0.25"));
    }

    #[test]
    fn save_as_refuses_another_file_that_is_there_and_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("song.toml");
        let (_engine, mut ctl) = controller_with_file(&path);
        std::fs::write(dir.path().join("other.toml"), "another session").unwrap();
        std::fs::create_dir(dir.path().join("folder.toml")).unwrap();
        let osc = lane(&ctl);
        ctl.apply(Intent::SetGain { node: osc, gain: Gain::Volume, value: 0.25 }).unwrap();
        let before = (ctl.rev, ctl.dirty);
        for (given, file) in [("other", "other.toml"), ("other.toml", "other.toml"), ("folder", "folder.toml")] {
            assert_eq!(ctl.apply(save_as(given)), Err(IntentError::Invalid(format!("a file called {file} is there already"))), "{given:?}");
        }
        // (A link that leads nowhere is a file that is there, as far as a name goes: the write would follow it.)
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.path().join("nowhere"), dir.path().join("link.toml")).unwrap();
            assert_eq!(ctl.apply(save_as("link")), Err(IntentError::Invalid("a file called link.toml is there already".to_string())));
            assert!(!dir.path().join("nowhere").exists(), "what the link leads to was not written");
        }
        assert_eq!((ctl.rev, ctl.dirty), before, "a refusal is not a revision, and it saved nothing");
        assert_eq!(ctl.session_path.as_deref(), Some(path.as_path()));
        assert_eq!(std::fs::read_to_string(dir.path().join("other.toml")).unwrap(), "another session", "what was there is as it was");
        assert!(!path.exists(), "and the session's own file was not written either");
    }

    #[test]
    fn save_as_refuses_what_is_no_plain_file_name_in_the_words_of_the_rule_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (_engine, mut ctl) = controller_with_file(&dir.path().join("song.toml"));
        let before = (ctl.rev, ctl.dirty);
        for given in ["", "   ", ".toml", "../escape", "/etc/passwd", "a/b", "..\\escape", "C:\\x", "c:x", "a*b", "a\0b", "con", "Nul.txt", &"a".repeat(251)] {
            let why = crate::session::plain_file_name(given).unwrap_err();
            assert_eq!(ctl.apply(save_as(given)), Err(IntentError::Invalid(why)), "{given:?}");
        }
        assert_eq!((ctl.rev, ctl.dirty), before);
        assert!(files_in(dir.path()).is_empty(), "nothing was written, here or beside it");
        assert!(!dir.path().parent().unwrap().join("escape.toml").exists());
    }

    #[test]
    fn save_as_that_cannot_be_written_is_an_io_error_and_the_session_keeps_its_file_and_its_mark() {
        // A file where the directory should be: the write fails.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("blocker"), "").unwrap();
        let path = dir.path().join("blocker").join("song.toml");
        let (engine, mut ctl) = controller_with_file(&path);
        let osc = lane(&ctl);
        ctl.apply(Intent::SetGain { node: osc, gain: Gain::Volume, value: 0.25 }).unwrap();
        sent(&engine);
        assert!(matches!(ctl.apply(save_as("copy")), Err(IntentError::Io(_))));
        assert_eq!((ctl.rev, ctl.dirty), (1, true));
        assert_eq!(ctl.session_path.as_deref(), Some(path.as_path()), "it is the session's file still");
    }

    #[test]
    fn save_as_is_no_step_of_the_history_and_does_not_forget_it() {
        let dir = tempfile::tempdir().unwrap();
        let (_engine, mut ctl) = controller_with_file(&dir.path().join("song.toml"));
        let osc = lane(&ctl);
        ctl.apply(Intent::SetGain { node: osc, gain: Gain::Volume, value: 0.25 }).unwrap();
        let history = ctl.history_state();
        assert!(history.undo.is_some());
        // (It is no step and no barrier, by what the history would make of it as well as by what `apply` does.)
        assert!(matches!(ctl.plan_history(&save_as("copy")), crate::app::history::Plan::Nothing));
        ctl.apply(save_as("copy")).unwrap();
        assert_eq!(ctl.history_state(), history);
        // What was done before it is put back, and is not saved: the file keeps what it had.
        ctl.apply(Intent::Undo).unwrap();
        assert!(ctl.dirty);
        assert!(std::fs::read_to_string(dir.path().join("copy.toml")).unwrap().contains("volume = 0.25"));
    }

    #[test]
    fn the_file_a_session_is_saved_as_is_beside_its_own_or_where_the_program_was_started() {
        let (_engine, mut ctl) = controller_with_file(std::path::Path::new("/songs/old.toml"));
        for (given, written) in [("mix", "/songs/mix.toml"), ("mix.toml", "/songs/mix.toml"), ("  mix.toml  ", "/songs/mix.toml"), ("a.b", "/songs/a.b.toml")] {
            assert_eq!(ctl.save_as_path(given), Ok(std::path::PathBuf::from(written)), "{given:?}");
        }
        assert_eq!(ctl.save_as_path("../mix"), Err(IntentError::Invalid(crate::session::plain_file_name("../mix").unwrap_err())));
        // A session that has no file is saved where the program was started, and one whose file is a bare name where that name is.
        ctl.session_path = None;
        assert_eq!(ctl.save_as_path("mix"), Ok(std::path::Path::new(".").join("mix.toml")));
        ctl.session_path = Some(std::path::PathBuf::from("old.toml"));
        assert_eq!(ctl.save_as_path("mix"), Ok(std::path::Path::new("").join("mix.toml")), "a bare file name has no directory");
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

    // -- SetPinned --

    fn pin(ctl: &mut Controller, node: NodeId, params: &[u32]) -> Result<Applied, IntentError> {
        ctl.apply(Intent::SetPinned { node, params: params.to_vec() })
    }

    /// The plugin slot a plugin node of the model has.
    fn slot_of(ctl: &Controller, path: NodePath) -> &PluginSlot {
        match path {
            NodePath::Instrument(i) => ctl.model.instruments[i].instrument.as_ref().unwrap(),
            NodePath::Effect { inst, index } => &ctl.model.instruments[inst].effects[index],
            NodePath::GroupEffect { group, index } => &ctl.model.groups[group].effects[index],
            _ => panic!("{path:?} has no plugin"),
        }
    }

    fn pinned_of(ctl: &Controller, path: NodePath) -> Vec<u32> {
        slot_of(ctl, path).pinned.clone()
    }

    #[test]
    fn set_pinned_keeps_which_parameters_a_node_shows_in_the_order_given_and_the_engine_hears_nothing_of_it() {
        let (engine, mut ctl) = controller();
        let osc = lane(&ctl);
        let (detune, _) = param(&ctl, NodePath::Instrument(0), "detune");
        let (sustain, _) = param(&ctl, NodePath::Instrument(0), "sustain");
        let applied = pin(&mut ctl, osc, &[sustain, detune]).unwrap();
        assert_eq!(applied, Applied::of(1, vec![Event::PinnedSet { node: osc, params: vec![sustain, detune] }]));
        assert_eq!(pinned_of(&ctl, NodePath::Instrument(0)), [sustain, detune]);
        assert_eq!(sent(&engine), Vec::<String>::new(), "the audio thread has no use for which knobs a node has");
        assert!(ctl.dirty, "it is in the file when the session is saved");
        // Saying another list replaces it, and none gives the node back to the front-end's own choice.
        pin(&mut ctl, osc, &[detune]).unwrap();
        assert_eq!(pinned_of(&ctl, NodePath::Instrument(0)), [detune]);
        let applied = pin(&mut ctl, osc, &[]).unwrap();
        assert_eq!(applied, Applied::of(3, vec![Event::PinnedSet { node: osc, params: vec![] }]));
        assert!(pinned_of(&ctl, NodePath::Instrument(0)).is_empty());
        // (A list that is the one there is is told all the same: a page may be behind.)
        assert!(pin(&mut ctl, osc, &[]).is_ok());
    }

    #[test]
    fn set_pinned_is_for_an_instrument_an_effect_and_a_bus_effect_and_changes_only_the_plugin_it_names() {
        let (_engine, mut ctl) = controller();
        let paths = [NodePath::Instrument(0), NodePath::Effect { inst: 0, index: 0 }, NodePath::GroupEffect { group: 0, index: 0 }];
        for (i, path) in paths.into_iter().enumerate() {
            let node = node(&ctl, path);
            let indices: Vec<u32> = slot_of(&ctl, path).params.iter().take(2).map(|p| p.index).rev().collect();
            let applied = pin(&mut ctl, node, &indices).unwrap_or_else(|e| panic!("{path:?}: {e}"));
            assert_eq!(applied.events, [Event::PinnedSet { node, params: indices.clone() }]);
            assert_eq!(pinned_of(&ctl, path), indices, "{path:?}");
            // (Those after it have nothing yet, and those before it have what they had.)
            for (j, other) in paths.into_iter().enumerate() {
                assert_eq!(pinned_of(&ctl, other).len(), if j <= i { 2 } else { 0 }, "{path:?} then {other:?}");
            }
        }
    }

    #[test]
    fn set_pinned_refuses_what_it_cannot_do_and_changes_nothing() {
        let (engine, mut ctl) = controller();
        let osc = lane(&ctl);
        let (detune, _) = param(&ctl, NodePath::Instrument(0), "detune");
        let group = node(&ctl, NodePath::Group(0));
        let lfo = node(&ctl, NodePath::Modulator { at: ModulatorLocation::Free, index: 0 });
        let empty_lane = node(&ctl, NodePath::Instrument(1));
        let ghost = NodeId(9_999);
        // A separator is a row of the TUI's parameter pane: the model has it, and a client has no such parameter.
        ctl.model.instruments[0].instrument.as_mut().unwrap().params.push(ParamSlot {
            name: "Envelope".into(),
            index: 99,
            kind: ParamKind::Separator,
            ..Default::default()
        });
        let cases = [
            ("a node that is not there", ghost, vec![detune], IntentError::NotFound(ghost)),
            ("a group", group, vec![0], invalid("that node has no plugin")),
            ("a modulator", lfo, vec![0], invalid("that node has no plugin")),
            ("a lane without an instrument", empty_lane, vec![0], invalid("that instrument has no plugin")),
            ("a parameter the plugin lacks", osc, vec![detune, 999], invalid("the plugin has no parameter 999")),
            ("a separator", osc, vec![99], invalid("Envelope is a separator, not a control")),
            ("a parameter twice", osc, vec![detune, detune], invalid(format!("parameter {detune} is listed twice"))),
        ];
        for (what, node, params, error) in cases {
            assert_eq!(pin(&mut ctl, node, &params), Err(error), "{what}");
        }
        assert_eq!(sent(&engine), Vec::<String>::new(), "nothing reached the audio thread");
        assert_eq!(ctl.rev, 0, "a refusal is not a revision");
        assert!(!ctl.dirty);
        assert!(pinned_of(&ctl, NodePath::Instrument(0)).is_empty());
        // And a refusal leaves what was pinned as it was.
        pin(&mut ctl, osc, &[detune]).unwrap();
        assert!(pin(&mut ctl, osc, &[detune, 999]).is_err());
        assert_eq!(pinned_of(&ctl, NodePath::Instrument(0)), [detune]);
    }

    #[test]
    fn set_pinned_takes_as_many_parameters_as_a_node_may_have_and_no_more() {
        let (_engine, mut ctl) = controller();
        let osc = lane(&ctl);
        // (No plugin of the built-in ones has so many parameters: the model's rows are made up, as a plugin of hundreds has them.)
        ctl.model.instruments[0].instrument.as_mut().unwrap().params =
            (0..70).map(|i| ParamSlot { name: format!("p{i}"), index: i, ..Default::default() }).collect();
        let all: Vec<u32> = (0..70).collect();
        pin(&mut ctl, osc, &all[..MAX_PINNED]).unwrap();
        assert_eq!(pinned_of(&ctl, NodePath::Instrument(0)).len(), MAX_PINNED);
        let refused = pin(&mut ctl, osc, &all[..=MAX_PINNED]);
        assert_eq!(refused, Err(invalid(format!("at most {MAX_PINNED} parameters can be pinned to a node"))));
        assert_eq!(pinned_of(&ctl, NodePath::Instrument(0)), all[..MAX_PINNED], "as it was");
        // (Counted before anything else is looked at: a list that is too long is refused for being long, not for what is in it.)
        assert_eq!(pin(&mut ctl, osc, &vec![999; MAX_PINNED + 1]), Err(invalid(format!("at most {MAX_PINNED} parameters can be pinned to a node"))));
    }

    #[test]
    fn what_is_pinned_is_what_a_session_saves() {
        let (_engine, mut ctl) = controller();
        let osc = lane(&ctl);
        let (detune, _) = param(&ctl, NodePath::Instrument(0), "detune");
        let (sustain, _) = param(&ctl, NodePath::Instrument(0), "sustain");
        let saved = |ctl: &Controller| {
            let (instruments, groups, cables, sources) = crate::app::save::to_save(&ctl.model);
            crate::session::to_toml(&instruments, &groups, &cables, &sources, None, None, None).unwrap()
        };
        assert!(!saved(&ctl).contains("pinned"), "{}", saved(&ctl));
        pin(&mut ctl, osc, &[sustain, detune]).unwrap();
        assert!(saved(&ctl).contains("pinned = [\"sustain\", \"detune\"]"), "{}", saved(&ctl));
        pin(&mut ctl, osc, &[]).unwrap();
        assert!(!saved(&ctl).contains("pinned"), "{}", saved(&ctl));
    }

    #[test]
    fn an_instrument_that_is_replaced_has_nothing_pinned_and_a_preset_that_is_loaded_keeps_the_pins() {
        let (_engine, mut ctl) = controller_with_builtins();
        let osc = lane(&ctl);
        let (detune, _) = param(&ctl, NodePath::Instrument(0), "detune");
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        let (size, _) = param(&ctl, NodePath::Effect { inst: 0, index: 0 }, "size");
        pin(&mut ctl, osc, &[detune]).unwrap();
        pin(&mut ctl, reverb, &[size]).unwrap();
        // A preset sets values and leaves the parameters as they are.
        ctl.apply(Intent::LoadPreset { node: reverb, preset: "cathedral".to_string() }).unwrap();
        assert_eq!(pinned_of(&ctl, NodePath::Effect { inst: 0, index: 0 }), [size]);
        // A new plugin has parameters of its own, and the pins were for the old one's.
        replace_instrument(&mut ctl, osc, "builtin:osc").unwrap();
        assert!(pinned_of(&ctl, NodePath::Instrument(0)).is_empty());
        assert_eq!(pinned_of(&ctl, NodePath::Effect { inst: 0, index: 0 }), [size], "the effects of the lane are as they were");
    }

    #[test]
    fn an_effect_that_is_moved_along_its_chain_takes_its_pins_with_it() {
        let (_engine, mut ctl) = long_chains();
        let bus: Vec<NodeId> = ctl.model.groups[0].effects.iter().map(|fx| fx.node).collect();
        let cutoff = ctl.model.groups[0].effects[0].params.iter().find(|p| p.name == "cutoff").unwrap().index;
        pin(&mut ctl, bus[0], &[cutoff]).unwrap();
        ctl.apply(Intent::MoveEffect { node: bus[0], index: 2 }).unwrap();
        let after: Vec<(NodeId, Vec<u32>)> = ctl.model.groups[0].effects.iter().map(|fx| (fx.node, fx.pinned.clone())).collect();
        assert_eq!(after[2], (bus[0], vec![cutoff]), "it is at its new place, with its pin");
        assert!(after.iter().filter(|(node, _)| *node != bus[0]).all(|(_, pins)| pins.is_empty()), "{after:?}");
    }

    /// The session of `controller()`, and what the controller makes of pinning some parameters to each of its nodes, as the web client's
    /// tests read it (`web/src/client/fixtures/pins.json`): they pin the mock engine's pins to the controller's, node by node and list by
    /// list. A list that is taken gives the parameters pinned to the node's plugin afterwards, one that is refused gives the controller's
    /// words. `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` rewrites it.
    #[test]
    fn the_web_clients_fixture_of_what_pinning_comes_to_is_current() {
        let (_engine, ctl) = controller();
        let before = ctl.snapshot();
        let plugin_of = |snapshot: &crate::app::snapshot::Snapshot, node: NodeId| -> Option<crate::app::graph::Plugin> {
            match &snapshot.graph.nodes.iter().find(|n| n.id == node)?.kind {
                NodeKind::Instrument { plugin, .. } => plugin.clone(),
                NodeKind::Effect { plugin, .. } => Some(plugin.clone()),
                _ => None,
            }
        };
        // Every node of the session, the master, and one that is not there; each with lists made of its own first parameters (the first two
        // numbers for a node that has none), one that is as long as a node may have and one that is longer.
        let mut nodes: Vec<NodeId> = ctl.model.nodes().map(|(id, _)| id).collect();
        nodes.extend([NodeId::MASTER, NodeId(9_999)]);
        let mut pins = Vec::new();
        for node in nodes {
            let own: Vec<u32> = plugin_of(&before, node).map(|p| p.params.iter().take(2).map(|param| param.index).collect()).unwrap_or_default();
            let (a, b) = (own.first().copied().unwrap_or(0), own.get(1).copied().unwrap_or(1));
            let lists: Vec<Vec<u32>> =
                vec![vec![], vec![a], vec![a, b], vec![b, a], vec![a, a], vec![999], vec![a, 999], (0..MAX_PINNED as u32).collect(), (0..=MAX_PINNED as u32).collect()];
            for params in lists {
                let (_engine, mut fresh) = controller();
                let row = match fresh.apply(Intent::SetPinned { node, params: params.clone() }) {
                    Err(error) => serde_json::json!({ "node": node, "params": params, "error": error.to_string() }),
                    Ok(_) => {
                        let pinned = plugin_of(&fresh.snapshot(), node).map(|p| p.pinned).unwrap();
                        assert_eq!(pinned, params, "what the node has pinned is what was asked");
                        serde_json::json!({ "node": node, "params": params, "pinned": pinned })
                    }
                };
                pins.push(row);
            }
        }
        let wire = serde_json::to_string(&before).unwrap();
        let mut snapshot: serde_json::Value = serde_json::from_str(&wire).unwrap();
        // The layout's fingerprint comes from the hasher, which the standard library does not promise to keep the same.
        snapshot["telemetry"]["rev"] = serde_json::json!(1);
        let fixture = serde_json::json!({ "snapshot": snapshot, "pins": pins });
        let fresh = serde_json::to_string_pretty(&fixture).unwrap() + "\n";

        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/src/client/fixtures/pins.json");
        if std::env::var_os("TANG_UPDATE_TYPES").is_some() {
            std::fs::write(&path, &fresh).unwrap();
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            committed == fresh,
            "{} is not what pinning comes to now: run `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` and commit the result",
            path.display()
        );
    }

    // -- AddGroup, SetLaneGroup and RenameGroup --

    /// `LANES_IN_GROUPS` (two groups, each with a modulator that drives its members, and four lanes: the first and the
    /// last in group 0, the third in group 1, the second in none) over the stub engine.
    fn lanes_in_groups() -> (StubEngine, Controller) {
        let (handles, engine) = testutil::stub_engine();
        (engine, Controller::new(load(testutil::LANES_IN_GROUPS).model, handles, None))
    }

    /// `LANES_IN_GROUPS` with one more modulator in the first group: an envelope whose cable drives the depth of the group LFO's cable to its
    /// second member's pan, so that a lane that leaves the group takes with it a cable that no plugin or modulator of the lane is the end of;
    /// and a third group, empty and with a name of its own, after the two that are called by their places.
    fn groups_scene() -> (StubEngine, Controller) {
        let session = testutil::LANES_IN_GROUPS.replacen(
            "\n[[group]]\n[[group.modulator]]\nrate = 2.0\n",
            "\n[[group.modulator]]\ntype = \"envelope\"\n[[group.modulator.target]]\nmod_depth = [0, 1]\ndepth = 0.5\n\n[[group]]\n[[group.modulator]]\nrate = 2.0\n",
            1,
        ) + "\n[[group]]\nname = \"Lead\"\n";
        assert_ne!(session, testutil::LANES_IN_GROUPS, "the envelope is in the session");
        let (handles, engine) = testutil::stub_engine();
        (engine, Controller::new(load(&session).model, handles, None))
    }

    fn lane_node(ctl: &Controller, inst: usize) -> NodeId {
        ctl.model.instruments[inst].node
    }

    fn group_node(ctl: &Controller, group: usize) -> NodeId {
        ctl.model.groups[group].node
    }

    fn groups_of_lanes(ctl: &Controller) -> Vec<Option<usize>> {
        ctl.model.instruments.iter().map(|lane| lane.group).collect()
    }

    fn set_lane_group(ctl: &mut Controller, node: NodeId, group: Option<NodeId>) -> Result<Applied, IntentError> {
        ctl.apply(Intent::SetLaneGroup { node, group })
    }

    #[test]
    fn add_group_adds_an_empty_group_at_the_end_in_the_model_and_the_engine_and_says_the_structure_changed() {
        let (engine, mut ctl) = lanes_in_groups();
        sent(&engine);
        let applied = ctl.apply(Intent::AddGroup { members: vec![] }).unwrap();
        assert_eq!(applied, Applied::restructured(1));
        assert!(applied.events.is_empty(), "nothing a client could apply: it is told by a snapshot");
        assert_eq!(sent(&engine), ["AddGroup"]);
        assert_eq!(ctl.model.groups.len(), 3);
        let group = &ctl.model.groups[2];
        assert_eq!((group.name.as_deref(), group.volume, group.pan), (None, 1.0, 0.0));
        assert!(group.effects.is_empty() && group.meter.is_some());
        assert!(hearing_group(&ctl.model, 2).is_empty());
        assert_eq!(groups_of_lanes(&ctl), [Some(0), None, Some(1), Some(0)], "no lane moved");
        assert!(ctl.dirty);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn add_group_with_lanes_puts_them_in_the_new_group_and_the_cables_of_what_hears_the_old_group_stay() {
        let (engine, mut ctl) = lanes_in_groups();
        // Lane 3 is member 1 of group 0, which drives its pan and the rate of its modulator; lane 1 is in no group.
        let members = vec![lane_node(&ctl, 1), lane_node(&ctl, 3)];
        sent(&engine);
        let applied = ctl.apply(Intent::AddGroup { members }).unwrap();
        assert_eq!(applied, Applied::restructured(1));
        let commands = sent(&engine);
        assert_eq!(commands[0], "AddGroup", "the group is there before a lane is put in it: {commands:?}");
        assert!(commands.contains(&"SetLaneGroup inst=1 group=Some(2)".to_string()), "{commands:?}");
        assert!(commands.contains(&"SetLaneGroup inst=3 group=Some(2)".to_string()), "{commands:?}");
        assert_eq!(groups_of_lanes(&ctl), [Some(0), Some(2), Some(1), Some(2)]);
        // (A cable names the plugin that it drives by its node, and not by the place of a lane among the members of a group, so that it drives it wherever the lane goes.)
        let group_0_cables: Vec<_> = hearing_group(&ctl.model, 0)[0].targets.iter().map(|t| t.param_name.clone()).collect();
        assert_eq!(group_0_cables, ["detune", "pan", "Mod 3 rate"], "the cables to lane 3 are as they were, and so is the one to lane 0");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn add_group_with_something_that_is_not_a_lane_makes_no_group() {
        let (engine, mut ctl) = lanes_in_groups();
        sent(&engine);
        let ghost = NodeId(9_999);
        let (group, effect, lane1) = (group_node(&ctl, 0), ctl.model.instruments[3].effects[0].node, lane_node(&ctl, 1));
        let cases = [
            ("a node that is not there", vec![lane1, ghost], IntentError::NotFound(ghost)),
            ("a group", vec![group], invalid("only a lane can be put in a group")),
            ("an effect", vec![lane1, effect], invalid("only a lane can be put in a group")),
            ("the master", vec![NodeId::MASTER], IntentError::NotFound(NodeId::MASTER)),
        ];
        for (what, members, error) in cases {
            assert_eq!(ctl.apply(Intent::AddGroup { members }), Err(error), "{what}");
        }
        assert!(sent(&engine).is_empty(), "nothing reached the audio thread");
        assert_eq!((ctl.rev, ctl.dirty), (0, false), "a refusal is not a revision");
        assert_eq!(ctl.model.groups.len(), 2, "and no group was made for the lane that was fine");
        assert_eq!(groups_of_lanes(&ctl), [Some(0), None, Some(1), Some(0)]);
    }

    #[test]
    fn set_lane_group_puts_a_lane_in_a_group_and_takes_it_out_and_says_the_structure_changed() {
        let (engine, mut ctl) = lanes_in_groups();
        sent(&engine);
        let (lane1, group1) = (lane_node(&ctl, 1), group_node(&ctl, 1));
        let applied = set_lane_group(&mut ctl, lane1, Some(group1)).unwrap();
        assert_eq!(applied, Applied::restructured(1));
        assert!(applied.events.is_empty(), "nothing a client could apply: it is told by a snapshot");
        assert_eq!(sent(&engine), ["SetLaneGroup inst=1 group=Some(1)"]);
        assert_eq!(groups_of_lanes(&ctl), [Some(0), Some(1), Some(1), Some(0)]);
        assert!(ctl.dirty);
        // And out again.
        set_lane_group(&mut ctl, lane1, None).unwrap();
        assert_eq!(sent(&engine), ["SetLaneGroup inst=1 group=None"]);
        assert_eq!(groups_of_lanes(&ctl), [Some(0), None, Some(1), Some(0)]);
        assert_eq!(ctl.rev, 2);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn set_lane_group_leaves_the_cables_of_the_modulators_that_hear_the_group_where_they_are() {
        let (engine, mut ctl) = lanes_in_groups();
        sent(&engine);
        // Group 0's LFO drives the detune of lane 0 (and the pan and the modulator of lane 3): a cable names its plugin by node, so that it drives it wherever the lane goes.
        let before: Vec<_> = hearing_group(&ctl.model, 0)[0].targets.iter().map(|t| (t.edge, t.aim)).collect();
        let lane0 = lane_node(&ctl, 0);
        set_lane_group(&mut ctl, lane0, None).unwrap();
        assert_eq!(sent(&engine), ["SetLaneGroup inst=0 group=None"], "no cable is taken out, and nothing is given back");
        let after: Vec<_> = hearing_group(&ctl.model, 0)[0].targets.iter().map(|t| (t.edge, t.aim)).collect();
        assert_eq!(after, before);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn set_lane_group_refuses_what_is_not_a_lane_what_is_not_a_group_and_what_is_not_there_and_changes_nothing() {
        let (engine, mut ctl) = lanes_in_groups();
        sent(&engine);
        let ghost = NodeId(9_999);
        let (lane0, lane1) = (lane_node(&ctl, 0), lane_node(&ctl, 1));
        let (group0, group1) = (group_node(&ctl, 0), group_node(&ctl, 1));
        let effect = ctl.model.instruments[3].effects[0].node;
        let lfo = hearing_group(&ctl.model, 0)[0].node;
        let cases = [
            ("a group for the lane", set_lane_group(&mut ctl, group1, Some(group0)), invalid("only a lane can be put in a group")),
            ("an effect for the lane", set_lane_group(&mut ctl, effect, None), invalid("only a lane can be put in a group")),
            ("a modulator for the lane", set_lane_group(&mut ctl, lfo, Some(group1)), invalid("only a lane can be put in a group")),
            ("a lane that is not there", set_lane_group(&mut ctl, ghost, Some(group1)), IntentError::NotFound(ghost)),
            ("a lane for the group", set_lane_group(&mut ctl, lane1, Some(lane0)), invalid("that node is not a group")),
            ("an effect for the group", set_lane_group(&mut ctl, lane1, Some(effect)), invalid("that node is not a group")),
            ("a group that is not there", set_lane_group(&mut ctl, lane1, Some(ghost)), IntentError::NotFound(ghost)),
            ("the master for the group", set_lane_group(&mut ctl, lane1, Some(NodeId::MASTER)), IntentError::NotFound(NodeId::MASTER)),
        ];
        for (what, result, error) in cases {
            assert_eq!(result, Err(error), "{what}");
        }
        assert!(sent(&engine).is_empty(), "nothing reached the audio thread");
        assert_eq!((ctl.rev, ctl.dirty), (0, false), "a refusal is not a revision");
        assert_eq!(groups_of_lanes(&ctl), [Some(0), None, Some(1), Some(0)]);
    }

    #[test]
    fn rename_group_names_a_group_in_the_model_alone_and_says_the_name_it_has_now() {
        let (engine, mut ctl) = lanes_in_groups();
        sent(&engine);
        let group = group_node(&ctl, 1);
        let applied = ctl.apply(Intent::RenameGroup { node: group, name: "Strings".to_string() }).unwrap();
        assert_eq!(applied, Applied::of(1, vec![Event::GroupRenamed { node: group, name: "Strings".to_string() }]));
        assert_eq!(ctl.model.groups[1].name.as_deref(), Some("Strings"));
        assert!(sent(&engine).is_empty(), "the audio thread has no use for a name");
        assert!(ctl.dirty);
        let name = |ctl: &Controller| ctl.model.graph().nodes.iter().find(|n| n.id == group).map(|n| n.name.clone());
        assert_eq!(name(&ctl).as_deref(), Some("Strings"));
        // Spaces around a name are not part of it, and a name of nothing is no name: the group is called what it is called by default.
        let applied = ctl.apply(Intent::RenameGroup { node: group, name: "  Pad two  ".to_string() }).unwrap();
        assert_eq!(applied.events, [Event::GroupRenamed { node: group, name: "Pad two".to_string() }]);
        assert_eq!(ctl.model.groups[1].name.as_deref(), Some("Pad two"));
        for blank in ["", "   "] {
            let applied = ctl.apply(Intent::RenameGroup { node: group, name: blank.to_string() }).unwrap();
            assert_eq!(applied.events, [Event::GroupRenamed { node: group, name: "Group 2".to_string() }], "{blank:?}");
            assert_eq!(ctl.model.groups[1].name, None);
        }
        assert_eq!(name(&ctl).as_deref(), Some("Group 2"));
    }

    #[test]
    fn rename_group_refuses_what_is_not_a_group_a_name_too_long_and_one_with_a_control_character() {
        let (engine, mut ctl) = lanes_in_groups();
        sent(&engine);
        let (group, lane0) = (group_node(&ctl, 0), lane_node(&ctl, 0));
        let ghost = NodeId(9_999);
        let rename = |ctl: &mut Controller, node: NodeId, name: &str| ctl.apply(Intent::RenameGroup { node, name: name.to_string() });
        assert_eq!(rename(&mut ctl, lane0, "x"), Err(invalid("only a group has a name to change")));
        assert_eq!(rename(&mut ctl, ghost, "x"), Err(IntentError::NotFound(ghost)));
        assert_eq!(rename(&mut ctl, NodeId::MASTER, "x"), Err(IntentError::NotFound(NodeId::MASTER)));
        assert_eq!(rename(&mut ctl, group, &"x".repeat(65)), Err(invalid("a name is at most 64 characters")));
        assert_eq!(rename(&mut ctl, group, "two\nlines"), Err(invalid("a name has no control characters")));
        assert_eq!(rename(&mut ctl, group, "tab\there"), Err(invalid("a name has no control characters")));
        assert_eq!((ctl.rev, ctl.dirty), (0, false), "a refusal is not a revision");
        assert_eq!(ctl.model.groups[0].name, None);
        // A name of 64 characters is fine, and so is one that is not ASCII (it is the characters that are counted, not the bytes).
        assert!(rename(&mut ctl, group, &"x".repeat(64)).is_ok());
        assert!(rename(&mut ctl, group, &"å".repeat(64)).is_ok());
        assert!(rename(&mut ctl, group, &"å".repeat(65)).is_err());
    }

    #[test]
    fn remove_node_takes_a_group_out_with_its_effects_and_its_members_are_in_no_group_and_the_modulators_that_heard_it_stay() {
        let (engine, mut ctl) = groups_scene();
        let (group, lfo, envelope) = (group_node(&ctl, 0), hearing_group(&ctl.model, 0)[0].node, hearing_group(&ctl.model, 0)[1].node);
        sent(&engine);
        let applied = remove(&mut ctl, group).unwrap();
        assert_eq!(applied, Applied::restructured(1));
        assert!(applied.events.is_empty(), "nothing a client could apply: it is told by a snapshot");
        // The group goes with its bus effects (the scene has no cable to one). The modulators that heard it stay, with their cables, which name the plugins of the members that stay by node, and hear every note.
        assert_eq!(sent(&engine), ["RemoveGroup group=0 keep"]);
        assert_eq!(ctl.model.groups.len(), 2, "the other two are the first and the second now");
        assert_eq!(ctl.model.groups[1].name.as_deref(), Some("Lead"));
        assert_eq!(groups_of_lanes(&ctl), [None, None, Some(0), None], "its members are in no group, and the lane of the group after it has its index");
        assert_eq!(ctl.model.locate(group), None, "the node is gone");
        for stays in [lfo, envelope] {
            let slot = ctl.model.free_modulators.iter().find(|m| m.node == stays).expect("the modulator stays");
            assert_eq!(slot.ear, Some(group), "the model keeps the group that it heard, which Undo brings back");
            assert_eq!(ctl.model.ear_of(slot), None, "and the audio thread has it hear every note");
        }
        assert!(!ctl.model.free_modulators.iter().find(|m| m.node == lfo).unwrap().targets.is_empty(), "with its cables");
        assert!(ctl.dirty);
        ctl.model.check_ids().unwrap();
        // And its node is not handed out again.
        ctl.apply(Intent::AddGroup { members: vec![] }).unwrap();
        assert!(ctl.model.nodes().all(|(id, _)| id != group));
    }

    #[test]
    fn remove_node_takes_a_lane_out_with_its_plugins_and_the_cables_to_them_and_the_modulators_that_heard_it_stay() {
        let (engine, mut ctl) = groups_scene();
        // Lane 3 is the second member of the first group, which drives its pan and the rate of its modulator, and the depth of the first
        // of those by a cable of an envelope; it has a reverb and a modulator.
        let lane3 = lane_node(&ctl, 3);
        let (effect, modulator) = (ctl.model.instruments[3].effects[0].node, hearing_lane(&ctl.model, 3)[0].node);
        sent(&engine);
        let applied = remove(&mut ctl, lane3).unwrap();
        assert_eq!(applied, Applied::restructured(1));
        assert!(applied.events.is_empty(), "nothing a client could apply: it is told by a snapshot");
        assert_eq!(
            sent(&engine),
            [
                "RemoveFreeModTarget mod=5 target=0",
                "RemoveFreeModTarget mod=4 target=1",
                "RemoveInstrument inst=3 keep",
            ]
        );
        assert_eq!(ctl.model.instruments.len(), 3);
        assert_eq!(groups_of_lanes(&ctl), [Some(0), None, Some(1)]);
        for gone in [lane3, effect] {
            assert_eq!(ctl.model.locate(gone), None, "the node is gone");
        }
        // The modulator that heard the lane stays, hearing every note (the model keeps the lane's node, which Undo brings back).
        let slot = ctl.model.free_modulators.iter().find(|m| m.node == modulator).expect("the modulator stays");
        assert_eq!((slot.ear, ctl.model.ear_of(slot)), (Some(lane3), None));
        let left: Vec<_> = hearing_group(&ctl.model, 0).iter().flat_map(|m| m.targets.iter().map(|t| t.param_name.clone())).collect();
        assert_eq!(left, ["detune", "Mod 3 rate"], "the group's modulators keep the cable to its first member and the one to the modulator that stays");
        assert!(ctl.dirty);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn remove_node_takes_a_lane_that_has_no_instrument_out_too() {
        let (engine, mut ctl) = controller();
        // (`SESSION` has a lane in the group and one with nothing in it.)
        let empty = ctl.model.instruments[1].node;
        sent(&engine);
        assert_eq!(remove(&mut ctl, empty), Ok(Applied::restructured(1)));
        assert_eq!(sent(&engine), ["RemoveInstrument inst=1 keep"]);
        assert_eq!(ctl.model.instruments.len(), 1);
        assert_eq!(ctl.model.locate(empty), None);
        ctl.model.check_ids().unwrap();
    }

    /// The session of `groups_scene()` and what the controller makes of the intents that add a group, put a lane in one, name one and take
    /// a lane or a group out, as the web client's tests read it (`web/src/client/fixtures/groups.json`): they pin the mock engine's to the
    /// controller's, intent by intent. One that is taken gives what the session comes to (`testutil::shape_of`; for a name, the name the group has), one that is refused the
    /// controller's words. `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` rewrites it.
    #[test]
    fn the_web_clients_fixture_of_what_the_intents_of_groups_come_to_is_current() {
        let (_engine, ctl) = groups_scene();
        let before = ctl.snapshot();
        let lanes: Vec<NodeId> = (0..4).map(|i| lane_node(&ctl, i)).collect();
        let (g0, g1, g2) = (group_node(&ctl, 0), group_node(&ctl, 1), group_node(&ctl, 2));
        let effect = ctl.model.instruments[3].effects[0].node;
        let lane_modulator = hearing_lane(&ctl.model, 1)[0].node;
        let ghost = NodeId(9_999);

        let mut intents: Vec<Intent> = Vec::new();
        // A group added: empty, with each lane, with lanes that are in a group, with one twice, and with what is no lane.
        for members in [vec![], vec![lanes[0]], vec![lanes[1]], vec![lanes[3]], vec![lanes[0], lanes[3]], vec![lanes[1], lanes[2]], vec![lanes[2], lanes[2]]]
            .into_iter()
            .chain([vec![ghost], vec![lanes[1], ghost], vec![g0], vec![effect], vec![NodeId::MASTER], vec![lanes[0], g0]])
        {
            intents.push(Intent::AddGroup { members });
        }
        // Each lane in each group and in none, then what is no lane and what is no group.
        for lane in &lanes {
            for group in [None, Some(g0), Some(g1)] {
                intents.push(Intent::SetLaneGroup { node: *lane, group });
            }
        }
        // (And the third group, a named one: a lane in it, and one that goes from a group to it.)
        for lane in [lanes[1], lanes[0]] {
            intents.push(Intent::SetLaneGroup { node: lane, group: Some(g2) });
        }
        for node in [g0, effect, lane_modulator, NodeId::MASTER, ghost] {
            intents.push(Intent::SetLaneGroup { node, group: Some(g1) });
        }
        for group in [lanes[0], effect, NodeId::MASTER, ghost] {
            intents.push(Intent::SetLaneGroup { node: lanes[1], group: Some(group) });
        }
        // Names: good ones, ones with spaces round them, none, ones at the limit and one past it (the characters, not the bytes), and ones
        // with control characters; for each group, then what is no group.
        let long = |c: &str, n: usize| c.repeat(n);
        let names = [
            "Strings".to_string(),
            "  Pad two  ".to_string(),
            String::new(),
            "   ".to_string(),
            long("x", 64),
            long("x", 65),
            long("å", 64),
            long("å", 65),
            long("😀", 64),
            long("😀", 65),
            "two\nlines".to_string(),
            "tab\there".to_string(),
            "\u{7}".to_string(),
            "\u{85}Next line\u{85}".to_string(),
        ];
        for group in [g0, g1] {
            for name in &names {
                intents.push(Intent::RenameGroup { node: group, name: name.clone() });
            }
        }
        // (The third group, which has a name: given another, and given none, which is the default name for its place.)
        for name in ["Strings", ""] {
            intents.push(Intent::RenameGroup { node: g2, name: name.to_string() });
        }
        for node in [lanes[0], effect, NodeId::MASTER, ghost] {
            intents.push(Intent::RenameGroup { node, name: "Strings".to_string() });
        }
        // Each lane and each group taken out, and what is neither (the master, a node that is not there).
        for node in lanes.iter().copied().chain([g0, g1, g2, NodeId::MASTER, ghost]) {
            intents.push(Intent::RemoveNode { node });
        }

        let mut rows = Vec::new();
        for intent in intents {
            let (_engine, mut fresh) = groups_scene();
            let row = match fresh.apply(intent.clone()) {
                Err(error) => serde_json::json!({ "intent": intent, "error": error.to_string() }),
                Ok(applied) => match applied.events.as_slice() {
                    [Event::GroupRenamed { name, .. }] => serde_json::json!({ "intent": intent, "name": name }),
                    [] => {
                        assert!(applied.restructured, "{intent:?}");
                        serde_json::json!({ "intent": intent, "after": testutil::shape_of(&fresh.snapshot()) })
                    }
                    other => panic!("{intent:?}: {other:?}"),
                },
            };
            rows.push(row);
        }
        let wire = serde_json::to_string(&before).unwrap();
        let mut snapshot: serde_json::Value = serde_json::from_str(&wire).unwrap();
        // The layout's fingerprint comes from the hasher, which the standard library does not promise to keep the same.
        snapshot["telemetry"]["rev"] = serde_json::json!(1);
        // The one lane of the tracer scene, which a session keeps: what the controller says to taking it out.
        let (handles, _engine) = testutil::stub_engine();
        let mut tracer = Controller::new(load(include_str!("../../web/src/client/fixtures/tracer.toml")).model, handles, None);
        let only = tracer.model.instruments[0].node;
        let last_lane = match tracer.apply(Intent::RemoveNode { node: only }) {
            Err(error) => serde_json::json!({ "node": only, "error": error.to_string() }),
            Ok(_) => panic!("the only lane of a session was taken out"),
        };
        let fixture = serde_json::json!({ "snapshot": snapshot, "rows": rows, "lastLane": last_lane });
        let fresh = serde_json::to_string_pretty(&fixture).unwrap() + "\n";

        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/src/client/fixtures/groups.json");
        if std::env::var_os("TANG_UPDATE_TYPES").is_some() {
            std::fs::write(&path, &fresh).unwrap();
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            committed == fresh,
            "{} is not what the intents of groups come to now: run `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` and commit the result",
            path.display()
        );
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
        let (group_lfo, lane0) = (hearing_group(&ctl.model, 0)[0].node, lane(&ctl));
        add_effect(&mut ctl, lane0, "builtin:filter").unwrap();
        sent(&engine);
        let filter = ctl.model.instruments[0].effects[1].node;
        let (cutoff, _) = param(&ctl, NodePath::Effect { inst: 0, index: 1 }, "cutoff");
        // A modulator reaches every plugin: the new filter is the lane's slot 2 (the instrument, the reverb).
        ctl.apply(cable_to(group_lfo, filter, cutoff)).unwrap();
        assert_eq!(
            sent(&engine),
            [command("AddFreeModTarget mod=0", &format!("LaneParam {{ lane: 0, slot: 2, param_index: {cutoff} }}"), &ctl, NodePath::Effect { inst: 0, index: 1 }, "cutoff")]
        );
    }

    #[test]
    fn add_effect_refuses_what_it_cannot_do_and_changes_nothing() {
        let (engine, mut ctl) = controller_with_builtins();
        sent(&engine);
        let untouched = load(SESSION).model;
        let (lane0, reverb) = (lane(&ctl), node(&ctl, NodePath::Effect { inst: 0, index: 0 }));
        let bus = node(&ctl, NodePath::GroupEffect { group: 0, index: 0 });
        let modulator = node(&ctl, NodePath::Modulator { at: ModulatorLocation::Free, index: 0 });
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

    // -- AddFreeEffect --

    fn add_free_effect(ctl: &mut Controller, plugin: &str) -> Result<Applied, IntentError> {
        ctl.apply(Intent::AddFreeEffect { plugin: plugin.to_string() })
    }

    #[test]
    fn add_free_effect_adds_a_lane_of_its_own_that_holds_the_effect_in_the_model_and_the_engine() {
        let (engine, mut ctl) = controller_with_builtins();
        sent(&engine);
        let applied = add_free_effect(&mut ctl, "builtin:filter").unwrap();
        assert_eq!(applied, Applied::restructured(1));
        assert!(applied.events.is_empty(), "nothing a client could apply: it is told by a snapshot");
        assert_eq!(sent(&engine), ["AddInstrument range=None", "SetLaneRoute inst=2 route=none", "InsertEffect inst=2 index=0 plugin=Filter mix=1.000"]);
        assert_eq!(ctl.model.instruments.len(), 3);
        let lane = &ctl.model.instruments[2];
        assert!(lane.instrument.is_none());
        assert_eq!((lane.range, lane.group, lane.volume, lane.route), (None, None, 1.0, Route::None), "a bare module: it has no cable out, and sends where cables are drawn from it");
        let chain: Vec<_> = lane.effects.iter().map(|e| (e.name.as_str(), e.mix, e.fed)).collect();
        assert_eq!(chain, [("Filter", 1.0, true)], "fed by the chain, as every effect is to begin with");
        assert_eq!(lane.effects[0].node.0, lane.node.0 + 1, "the lane's node, and then the effect's");
        assert!(ctl.dirty);
        ctl.model.check_ids().unwrap();
        // It is an effect like any other: a node that a client names, whose parameters the intents reach.
        let fx = ctl.model.instruments[2].effects[0].node;
        assert_eq!(ctl.model.locate(fx), Some(NodePath::Effect { inst: 2, index: 0 }));
        let (cutoff, _) = param(&ctl, NodePath::Effect { inst: 2, index: 0 }, "cutoff");
        ctl.apply(Intent::SetParam { node: fx, param: cutoff, value: 0.25 }).unwrap();
        assert_eq!(sent(&engine), ["SetParameter inst=2 slot=1 param=0 value=0.250"]);
    }

    #[test]
    fn the_graph_after_adding_a_free_effect_has_an_empty_lane_and_the_effect_after_it_and_no_link_out_of_them() {
        use crate::app::graph::EdgeKind;
        use crate::app::layout::SlotKind;
        let (_engine, mut ctl) = controller_with_builtins();
        let before = ctl.model.graph();
        add_free_effect(&mut ctl, "builtin:filter").unwrap();
        let (lane, fx) = (ctl.model.instruments[2].node, ctl.model.instruments[2].effects[0].node);
        let graph = ctl.model.graph();
        let head = graph.nodes.iter().find(|n| n.id == lane).unwrap();
        assert_eq!((head.parent, head.name.as_str()), (None, "(empty)"));
        assert!(matches!(&head.kind, NodeKind::Instrument { plugin: None, range: None, .. }), "{head:?}");
        let filter = graph.nodes.iter().find(|n| n.id == fx).unwrap();
        assert_eq!((filter.parent, filter.name.as_str()), (Some(lane), "Filter"));
        // The audio goes from the lane to its effect, after the links that the lanes there were have, and the effect is a bare module: no link goes out of it, to the master or anywhere (step 88: it
        // sends where cables are drawn from it).
        let audio = |graph: &crate::app::graph::Graph| -> Vec<(NodeId, NodeId)> {
            graph.edges.iter().filter(|e| !matches!(e.kind, EdgeKind::Modulation { .. })).map(|e| (e.from.node, e.to.node)).collect()
        };
        let (before_audio, audio) = (audio(&before), audio(&graph));
        assert_eq!(audio.len(), before_audio.len() + 1);
        assert_eq!(audio[audio.len() - 1..], [(lane, fx)]);
        assert!(matches!(&head.kind, NodeKind::Instrument { route: Route::None, .. }), "{head:?}");
        // The lane has a meter of its own in the layout, after the others.
        let meters = ctl.model.telemetry_layout().meters;
        let mine: Vec<_> = meters.iter().filter(|m| m.node == lane).map(|m| (m.port, m.kind)).collect();
        assert_eq!(mine, [(Port::AudioOut, SlotKind::Peak), (Port::AudioOut, SlotKind::Clips)]);
        assert_eq!(meters[meters.len() - 2].node, lane, "the last two entries are the new lane's");
    }

    #[test]
    fn a_cable_can_be_drawn_into_a_free_effect_and_out_of_it_as_soon_as_it_has_been_added() {
        let (engine, mut ctl) = controller_with_builtins();
        let lane0 = lane(&ctl);
        add_free_effect(&mut ctl, "builtin:filter").unwrap();
        let (holder, fx) = (ctl.model.instruments[2].node, ctl.model.instruments[2].effects[0].node);
        sent(&engine);
        // The instrument of lane 0 into the filter, and the filter to the mix: a free effect is fed by cables.
        draw(&mut ctl, out(lane0), into(fx), None).unwrap();
        draw(&mut ctl, out(fx), into(NodeId::MASTER), Some(0.5)).unwrap();
        assert_eq!(sent(&engine).len(), 2);
        assert_eq!(ctl.model.cables.iter().map(|c| (c.from, c.to)).collect::<Vec<_>>(), [(CableFrom::Instrument(lane0), CableTo::Effect(fx)), (CableFrom::Effect(fx), CableTo::Master)]);
        // With the lane's way out cut, the filter is heard through its cable only: the lane is the effect's holder, and its way out is the effect's.
        ctl.apply(Intent::SetRoute { node: holder, route: Route::None }).unwrap();
        assert_eq!(ctl.model.instruments[2].route, Route::None);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn add_free_effect_refuses_what_it_cannot_do_and_changes_nothing() {
        let (engine, mut ctl) = controller_with_builtins();
        sent(&engine);
        let untouched = load(SESSION).model;
        let cases = [
            ("an instrument", "builtin:osc", invalid("Oscillator is an instrument, not an effect")),
            ("a path", "./examples/plugin.clap", invalid("./examples/plugin.clap is not a plugin that is installed")),
            ("an id the catalog does not have", "clap:com.example.nothing", invalid("clap:com.example.nothing is not a plugin that is installed")),
        ];
        for (what, plugin, error) in cases {
            assert_eq!(add_free_effect(&mut ctl, plugin), Err(error), "{what}");
        }
        assert!(sent(&engine).is_empty(), "nothing reached the audio thread, and no lane was made");
        assert_eq!((ctl.rev, ctl.dirty), (0, false), "a refusal is not a revision");
        testutil::assert_same_model("after the refusals", &ctl.model, &untouched);
    }

    // -- the rows that a preset overwrites --

    #[test]
    fn the_rows_a_preset_changes_are_those_it_gives_another_value_or_baseline_and_those_the_user_set_over_what_they_had() {
        // (index, value, default) as the model has the rows before the preset is loaded.
        let row = |index: u32, value: f32, default: f32| ParamSlot { index, value, default, ..Default::default() };
        let rows = [
            row(0, 0.5, 0.5),  // the preset gives it 0.5 too: as it will be
            row(1, 0.5, 0.5),  // the preset gives it 0.7: changed
            row(2, 0.3, 0.7),  // set by the user over 0.7, and the preset gives it 0.3: the baseline is what changes
            row(3, 0.9, 0.5),  // set by the user, and the preset gives it 0.9: the baseline is what changes too
            row(4, 0.2, 0.5),  // set by the user, and the preset has nothing for it: it would be loaded over
            row(5, 0.4, 0.4),  // nothing set, and the preset has nothing for it: as it will be
            row(6, 0.1, 0.1),  // the preset gives it 0.8: changed (the preset's values are not in the order of the rows)
        ];
        let values = [(6, 0.8), (3, 0.9), (2, 0.3), (1, 0.7), (0, 0.5)];
        let indexes = |changed: Vec<RowBefore>| changed.iter().map(|before| (before.index, before.value, before.default)).collect::<Vec<_>>();
        assert_eq!(indexes(rows_a_preset_changes(&rows, Some(&values))), [(1, 0.5, 0.5), (2, 0.3, 0.7), (3, 0.9, 0.5), (4, 0.2, 0.5), (6, 0.1, 0.1)]);
        // A preset whose values could not be read leaves the user's.
        assert_eq!(indexes(rows_a_preset_changes(&rows, None)), [(2, 0.3, 0.7), (3, 0.9, 0.5), (4, 0.2, 0.5)]);
    }

    // -- AddInstrument and ReplaceInstrument --

    fn add_instrument(ctl: &mut Controller, plugin: &str) -> Result<Applied, IntentError> {
        ctl.apply(Intent::AddInstrument { plugin: plugin.to_string() })
    }

    fn replace_instrument(ctl: &mut Controller, node: NodeId, plugin: &str) -> Result<Applied, IntentError> {
        ctl.apply(Intent::ReplaceInstrument { node, plugin: plugin.to_string() })
    }

    #[test]
    fn add_instrument_adds_a_lane_at_the_end_with_the_instrument_in_the_model_and_the_engine() {
        let (engine, mut ctl) = controller_with_builtins();
        sent(&engine);
        let applied = add_instrument(&mut ctl, "builtin:osc").unwrap();
        assert_eq!(applied, Applied::restructured(1));
        assert!(applied.events.is_empty(), "nothing a client could apply: it is told by a snapshot");
        assert_eq!(sent(&engine), ["AddInstrument range=None", "SwapInstrument inst=2 plugin=Oscillator bufs=2 remap=false"]);
        assert_eq!(ctl.model.instruments.len(), 3);
        let lane = &ctl.model.instruments[2];
        assert_eq!(lane.instrument.as_ref().map(|p| (p.node, p.name.as_str())), Some((lane.node, "Oscillator")));
        assert_eq!((lane.range, lane.group, lane.volume), (None, None, 1.0));
        assert!(ctl.dirty);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn an_audio_input_is_added_in_a_lane_whose_way_out_is_cut_so_that_it_is_heard_where_a_cable_is_drawn_from_it_and_nowhere_before() {
        use crate::app::graph::EdgeKind;
        use crate::session::Route;
        let (engine, mut ctl) = controller_with_builtins();
        sent(&engine);
        add_instrument(&mut ctl, "builtin:input").unwrap();
        // The way out is cut before the instrument is in: one that was there for a moment would be heard, and a microphone that is heard through the speakers it listens to howls.
        assert_eq!(sent(&engine), ["AddInstrument range=None", "SetLaneRoute inst=2 route=none", "SwapInstrument inst=2 plugin=Audio Input bufs=2 remap=false"]);
        let (node, route, name) = {
            let lane = &ctl.model.instruments[2];
            (lane.node, lane.route, lane.instrument.as_ref().map(|p| p.name.clone()))
        };
        assert_eq!((route, name.as_deref()), (Route::None, Some("Audio Input")));
        let graph = ctl.model.graph();
        assert!(!graph.edges.iter().any(|e| !matches!(e.kind, EdgeKind::Modulation { .. }) && e.from.node == node), "no link leaves it: it sends where a cable is drawn");
        // An instrument that is heard as it is has its way out, and the input's cut is the lane's: the next lane has its own.
        add_instrument(&mut ctl, "builtin:osc").unwrap();
        assert_eq!(ctl.model.instruments[3].route, Route::Out);
        // Undo takes the lane out and redo makes it again, bare as it was; a cable from it to the mix gives the way out back as any lane's does (it is a cable of the lane's instrument).
        ctl.apply(Intent::Undo).unwrap();
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!(ctl.model.instruments.len(), 2);
        ctl.apply(Intent::Redo).unwrap();
        assert_eq!((ctl.model.instruments.len(), ctl.model.instruments[2].route), (3, Route::None));
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn an_audio_input_that_replaces_the_instrument_of_a_lane_leaves_the_way_out_as_it_was() {
        use crate::session::Route;
        let (_engine, mut ctl) = controller_with_builtins();
        let lane0 = lane(&ctl);
        assert_eq!(ctl.model.instruments[0].route, Route::Out);
        replace_instrument(&mut ctl, lane0, "builtin:input").unwrap();
        assert_eq!((ctl.model.instruments[0].route, ctl.model.instruments[0].instrument.as_ref().map(|p| p.name.as_str())), (Route::Out, Some("Audio Input")));
    }

    #[test]
    fn the_graph_says_of_an_audio_input_that_it_takes_no_notes_and_of_the_oscillator_that_it_does_and_the_wire_has_a_word_of_it_for_the_input_only() {
        let (_engine, mut ctl) = controller_with_builtins();
        add_instrument(&mut ctl, "builtin:input").unwrap();
        add_instrument(&mut ctl, "builtin:osc").unwrap();
        let graph = ctl.model.graph();
        let plugin_of = |inst: usize| match &graph.nodes.iter().find(|node| node.id == ctl.model.instruments[inst].node).unwrap().kind {
            NodeKind::Instrument { plugin: Some(plugin), .. } => plugin.clone(),
            other => panic!("{other:?}"),
        };
        let (input, oscillator) = (plugin_of(2), plugin_of(3));
        assert_eq!((input.id.as_str(), input.takes_notes, oscillator.id.as_str(), oscillator.takes_notes), ("builtin:input", false, "builtin:osc", true));
        // On the wire the input says it, and a plugin that takes notes says nothing (a client reads an absent word as true, and the fixtures of every other plugin are as they were).
        assert_eq!(serde_json::to_value(&input).unwrap()["takesNotes"], serde_json::json!(false));
        assert!(serde_json::to_value(&oscillator).unwrap().get("takesNotes").is_none());
        let read: crate::app::graph::Plugin = serde_json::from_value(serde_json::to_value(&oscillator).unwrap()).unwrap();
        assert!(read.takes_notes, "and what is read without the word takes notes");
    }

    #[test]
    fn the_graph_after_adding_an_instrument_has_its_node_its_audio_path_to_the_master_and_its_meter() {
        use crate::app::graph::EdgeKind;
        use crate::app::layout::SlotKind;
        let (_engine, mut ctl) = controller_with_builtins();
        let before = ctl.model.graph();
        add_instrument(&mut ctl, "builtin:osc").unwrap();
        let added = ctl.model.instruments[2].node;
        let graph = ctl.model.graph();
        // The node comes after the lanes there were and before the modulators of the session's own, and the master is last.
        let ids: Vec<_> = graph.nodes.iter().map(|n| n.id).collect();
        let before_ids: Vec<_> = before.nodes.iter().map(|n| n.id).collect();
        let first_modulator = ctl.model.free_modulators.first().map(|m| m.node);
        let at = before_ids.iter().position(|id| Some(*id) == first_modulator).unwrap_or(before_ids.len() - 1);
        let mut expected = before_ids.clone();
        expected.insert(at, added);
        assert_eq!(ids, expected);
        assert_eq!(ids.last(), Some(&NodeId::MASTER));
        let node = graph.nodes.iter().find(|n| n.id == added).unwrap();
        assert_eq!((node.parent, node.name.as_str()), (None, "Oscillator"));
        assert!(
            matches!(&node.kind, NodeKind::Instrument { plugin: Some(p), range: None, transpose: 0, volume, .. } if p.id == "builtin:osc" && *volume == 1.0),
            "{node:?}"
        );
        // Its audio goes to the master, after the links the others have.
        let audio: Vec<_> = graph.edges.iter().filter(|e| !matches!(e.kind, EdgeKind::Modulation { .. })).map(|e| (e.from.node, e.to.node)).collect();
        assert_eq!(audio.last(), Some(&(added, NodeId::MASTER)));
        assert_eq!(audio.len(), before.edges.iter().filter(|e| !matches!(e.kind, EdgeKind::Modulation { .. })).count() + 1);
        // Its meter is in the layout, after the others.
        let meters = ctl.model.telemetry_layout().meters;
        let mine: Vec<_> = meters.iter().filter(|m| m.node == added).map(|m| (m.port, m.kind)).collect();
        assert_eq!(mine, [(Port::AudioOut, SlotKind::Peak), (Port::AudioOut, SlotKind::Clips)]);
        assert_eq!(meters[meters.len() - 2].node, added, "the last two entries are the new lane's");
    }

    #[test]
    fn a_lane_that_was_added_takes_a_modulator_and_a_cable_to_its_instrument_at_once() {
        let (engine, mut ctl) = controller_with_builtins();
        add_instrument(&mut ctl, "builtin:osc").unwrap();
        let added = ctl.model.instruments[2].node;
        ctl.apply(Intent::AddFreeModulator { kind: ModulatorKind::Lfo, hears: Some(added) }).unwrap();
        let lfo = hearing_lane(&ctl.model, 2)[0].node;
        let (detune, _) = param(&ctl, NodePath::Instrument(2), "detune");
        sent(&engine);
        ctl.apply(cable_to(lfo, added, detune)).unwrap();
        assert_eq!(
            sent(&engine),
            [command("AddFreeModTarget mod=1", &format!("LaneParam {{ lane: 2, slot: 0, param_index: {detune} }}"), &ctl, NodePath::Instrument(2), "detune")]
        );
    }

    #[test]
    fn replace_instrument_puts_the_instrument_in_the_lane_under_the_same_node_and_takes_the_cables_to_the_old_one_out() {
        let (engine, mut ctl) = controller_with_builtins();
        let (lane0, group_lfo) = (lane(&ctl), hearing_group(&ctl.model, 0)[0].node);
        let (detune, _) = param(&ctl, NodePath::Instrument(0), "detune");
        ctl.apply(cable_to(group_lfo, lane0, detune)).unwrap();
        ctl.apply(Intent::SetParam { node: lane0, param: detune, value: 3.0 }).unwrap();
        sent(&engine);
        let rev = ctl.rev;

        let applied = replace_instrument(&mut ctl, lane0, "builtin:osc").unwrap();
        assert_eq!(applied, Applied::restructured(rev + 1));
        assert!(applied.events.is_empty(), "nothing a client could apply: it is told by a snapshot");
        // The group's cable to the old plugin goes first: it names the parameter by an index, and the new plugin has its own.
        assert_eq!(sent(&engine), ["RemoveFreeModTarget mod=0 target=0", "SwapInstrument inst=0 plugin=Oscillator bufs=2 remap=false keep"]);
        assert!(hearing_group(&ctl.model, 0)[0].targets.is_empty());
        assert_eq!(lane(&ctl), lane0, "the lane keeps its node");
        let lane = &ctl.model.instruments[0];
        assert_eq!((lane.group, lane.effects.len()), (Some(0), 1), "and what else it has");
        assert_eq!(param(&ctl, NodePath::Instrument(0), "detune").1, 0.0, "the parameters are the new plugin's own");
        // A client sees it in the graph: the same node, with no cable into it.
        let graph = ctl.model.graph();
        assert!(graph.nodes.iter().any(|n| n.id == lane0 && matches!(&n.kind, NodeKind::Instrument { plugin: Some(_), .. })));
        assert!(!graph.edges.iter().any(|e| matches!(e.kind, crate::app::graph::EdgeKind::Modulation { .. })), "no cable is left");
        assert!(ctl.dirty);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn replace_instrument_fills_a_lane_that_has_none() {
        let (engine, mut ctl) = controller_with_builtins();
        sent(&engine);
        let empty = node(&ctl, NodePath::Instrument(1));
        assert!(ctl.model.instruments[1].instrument.is_none());
        replace_instrument(&mut ctl, empty, "builtin:osc").unwrap();
        assert_eq!(sent(&engine), ["SwapInstrument inst=1 plugin=Oscillator bufs=2 remap=false"]);
        assert_eq!(ctl.model.instruments[1].instrument.as_ref().map(|p| (p.node, p.name.as_str())), Some((empty, "Oscillator")));
        let graph = ctl.model.graph();
        assert_eq!(graph.nodes.iter().find(|n| n.id == empty).map(|n| n.name.as_str()), Some("Oscillator"), "the lane has a name now");
    }

    fn clear_instrument(ctl: &mut Controller, node: NodeId) -> Result<Applied, IntentError> {
        ctl.apply(Intent::ClearInstrument { node })
    }

    #[test]
    fn clear_instrument_takes_the_instrument_out_and_the_cables_to_it_and_leaves_the_lane_what_else_it_has() {
        use crate::app::graph::EdgeKind;
        let (engine, mut ctl) = controller_with_builtins();
        let (lane0, group_lfo) = (lane(&ctl), hearing_group(&ctl.model, 0)[0].node);
        let (detune, _) = param(&ctl, NodePath::Instrument(0), "detune");
        ctl.apply(cable_to(group_lfo, lane0, detune)).unwrap();
        sent(&engine);
        let rev = ctl.rev;

        let applied = clear_instrument(&mut ctl, lane0).unwrap();
        assert_eq!(applied, Applied::restructured(rev + 1));
        assert!(applied.events.is_empty(), "nothing a client could apply: it is told by a snapshot");
        // The group's cable to the plugin goes first: it names a parameter of a plugin that is not there. The audio thread is asked to hand the instrument to a channel of its own.
        assert_eq!(sent(&engine), ["RemoveFreeModTarget mod=0 target=0", "ClearInstrument inst=0 keep"]);
        assert!(hearing_group(&ctl.model, 0)[0].targets.is_empty());
        let lane = &ctl.model.instruments[0];
        assert!(lane.instrument.is_none());
        assert_eq!(lane.node, lane0, "the lane keeps its node");
        assert_eq!((lane.group, lane.effects.len()), (Some(0), 1), "and what else it has");
        // A client sees it in the graph: the same node, an empty lane called so, no cable into it, and its effect still after it in the audio path.
        let graph = ctl.model.graph();
        let node = graph.nodes.iter().find(|n| n.id == lane0).unwrap();
        assert_eq!(node.name, "(empty)");
        assert!(matches!(&node.kind, NodeKind::Instrument { plugin: None, .. }), "{node:?}");
        assert!(!graph.edges.iter().any(|e| matches!(e.kind, EdgeKind::Modulation { .. })), "no cable is left");
        assert!(graph.edges.iter().any(|e| e.kind == EdgeKind::Audio && e.from.node == lane0), "the audio path goes on from the lane to its effect");
        assert!(ctl.dirty);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn clear_instrument_refuses_a_lane_that_has_none_and_what_is_no_lane_and_changes_nothing() {
        let (engine, mut ctl) = controller_with_builtins();
        let (lane0, group_lfo) = (lane(&ctl), hearing_group(&ctl.model, 0)[0].node);
        let (detune, _) = param(&ctl, NodePath::Instrument(0), "detune");
        ctl.apply(cable_to(group_lfo, lane0, detune)).unwrap();
        let (_other_engine, mut untouched) = controller_with_builtins();
        untouched.apply(cable_to(group_lfo, lane0, detune)).unwrap();
        sent(&engine);
        let empty = node(&ctl, NodePath::Instrument(1));
        assert!(ctl.model.instruments[1].instrument.is_none());
        for (node, error) in [
            (empty, invalid("the lane has no instrument to take out")),
            (node(&ctl, NodePath::Effect { inst: 0, index: 0 }), invalid("an instrument is taken out of a lane")),
            (node(&ctl, NodePath::GroupEffect { group: 0, index: 0 }), invalid("an instrument is taken out of a lane")),
            (node(&ctl, NodePath::Group(0)), invalid("an instrument is taken out of a lane")),
            (node(&ctl, NodePath::Modulator { at: ModulatorLocation::Free, index: 0 }), invalid("an instrument is taken out of a lane")),
            (NodeId(9_999), IntentError::NotFound(NodeId(9_999))),
        ] {
            assert_eq!(clear_instrument(&mut ctl, node).unwrap_err(), error);
        }
        assert!(sent(&engine).is_empty(), "nothing was sent");
        assert_eq!(ctl.model.graph(), untouched.model.graph(), "and nothing changed, the cable to the instrument among it");
        assert!(ctl.model.instruments[0].instrument.is_some());
    }

    #[test]
    fn the_instrument_intents_refuse_what_they_cannot_do_and_change_nothing() {
        let (engine, mut ctl) = controller_with_builtins();
        // A cable into the instrument that a refused replace must not take out.
        let (lane0, group_lfo) = (lane(&ctl), hearing_group(&ctl.model, 0)[0].node);
        let (detune, _) = param(&ctl, NodePath::Instrument(0), "detune");
        ctl.apply(cable_to(group_lfo, lane0, detune)).unwrap();
        let (_other_engine, mut untouched) = controller_with_builtins();
        untouched.apply(cable_to(group_lfo, lane0, detune)).unwrap();
        sent(&engine);

        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        let bus = node(&ctl, NodePath::GroupEffect { group: 0, index: 0 });
        let group = node(&ctl, NodePath::Group(0));
        let modulator = node(&ctl, NodePath::Modulator { at: ModulatorLocation::Free, index: 0 });
        let ghost = NodeId(9_999);
        let not_a_lane = invalid("an instrument goes in a lane");
        let not_an_instrument = invalid("Filter is an effect, not an instrument");
        let not_installed = |what: &str| invalid(format!("{what} is not a plugin that is installed"));
        let replaces = [
            ("an effect", reverb, "builtin:osc", not_a_lane.clone()),
            ("a bus effect", bus, "builtin:osc", not_a_lane.clone()),
            ("a group", group, "builtin:osc", not_a_lane.clone()),
            ("a modulator", modulator, "builtin:osc", not_a_lane),
            ("a node that is not there", ghost, "builtin:osc", IntentError::NotFound(ghost)),
            ("the master, which the model does not have", NodeId::MASTER, "builtin:osc", IntentError::NotFound(NodeId::MASTER)),
            ("an effect for the instrument", lane0, "builtin:filter", not_an_instrument.clone()),
            ("a path", lane0, "./examples/plugin.clap", not_installed("./examples/plugin.clap")),
            ("an id the catalog does not have", lane0, "clap:com.example.nothing", not_installed("clap:com.example.nothing")),
        ];
        for (what, node, plugin, error) in replaces {
            assert_eq!(replace_instrument(&mut ctl, node, plugin), Err(error), "replace: {what}");
        }
        let adds = [
            ("an effect", "builtin:filter", not_an_instrument),
            ("a path", "./examples/plugin.clap", not_installed("./examples/plugin.clap")),
            ("an id the catalog does not have", "clap:com.example.nothing", not_installed("clap:com.example.nothing")),
        ];
        for (what, plugin, error) in adds {
            assert_eq!(add_instrument(&mut ctl, plugin), Err(error), "add: {what}");
        }
        assert!(sent(&engine).is_empty(), "nothing reached the audio thread: no lane, no cable taken out");
        assert_eq!((ctl.rev, ctl.dirty), (1, true), "a refusal is not a revision (the cable was the one)");
        testutil::assert_same_model("after the refusals", &ctl.model, &untouched.model);
    }

    /// What the controller says each preset of each built-in plugin does, for the web client's mock engine to say the same: the values the
    /// plugin has for its parameters after the preset, by the preset's id, as `PresetLoaded` carries them: the file
    /// `web/src/client/fixtures/builtins.presets.json`, which this test keeps current.
    /// `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` rewrites it.
    #[test]
    fn the_web_clients_fixture_of_the_builtin_presets_is_current() {
        let mut plugins = serde_json::Map::new();
        for info in plugin::builtin::enumerate_plugins().into_iter().filter(|p| p.preset_count > 0) {
            let (_engine, mut ctl) = controller_with_builtins();
            let lane0 = lane(&ctl);
            let node = if info.is_instrument {
                replace_instrument(&mut ctl, lane0, &info.id).unwrap();
                lane0
            } else {
                add_effect(&mut ctl, lane0, &info.id).unwrap();
                ctl.model.instruments[0].effects.last().unwrap().node
            };
            let ids: Vec<String> = match ctl.model.locate(node).unwrap() {
                NodePath::Instrument(inst) => ctl.model.instruments[inst].instrument.as_ref().unwrap().presets.iter().map(|p| p.id.clone()).collect(),
                NodePath::Effect { inst, index } => ctl.model.instruments[inst].effects[index].presets.iter().map(|p| p.id.clone()).collect(),
                other => panic!("{other:?} has no plugin"),
            };
            assert_eq!(ids.len(), info.preset_count, "{}", info.id);
            let mut by_preset = serde_json::Map::new();
            for preset in ids {
                let applied = ctl.apply(Intent::LoadPreset { node, preset: preset.clone() }).unwrap();
                let [Event::PresetLoaded { values, .. }] = applied.events.as_slice() else { panic!("one PresetLoaded: {:?}", applied.events) };
                assert!(!values.is_empty(), "{} {preset}: the controller reports values", info.id);
                by_preset.insert(preset, testutil::wire(values));
            }
            plugins.insert(info.id.clone(), serde_json::Value::Object(by_preset));
        }
        let fresh = serde_json::to_string_pretty(&plugins).unwrap() + "\n";

        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/src/client/fixtures/builtins.presets.json");
        if std::env::var_os("TANG_UPDATE_TYPES").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &fresh).unwrap();
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            committed == fresh,
            "{} is not what the built-in presets do now: run `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` and commit the result",
            path.display()
        );
    }

    /// What the web client's mock engine makes of an instrument it is asked to add or to put in a lane, for each built-in
    /// instrument: the node the controller makes for it, without the ids (the mock hands out its own): the file
    /// `web/src/client/fixtures/builtins.instruments.json`, which this test keeps current.
    /// `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` rewrites it.
    #[test]
    fn the_web_clients_fixture_of_the_builtin_instruments_is_current() {
        let mut instruments = serde_json::Map::new();
        for info in plugin::builtin::enumerate_plugins().into_iter().filter(|p| p.is_instrument) {
            let (_engine, mut ctl) = controller_with_builtins();
            add_instrument(&mut ctl, &info.id).unwrap();
            let added = ctl.model.instruments.last().unwrap().node;
            let mut json = testutil::wire(ctl.model.graph().nodes.iter().find(|n| n.id == added).unwrap());
            let object = json.as_object_mut().unwrap();
            object.remove("id");
            object.remove("parent");
            instruments.insert(info.id.clone(), json);
        }
        let fresh = serde_json::to_string_pretty(&instruments).unwrap() + "\n";

        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/src/client/fixtures/builtins.instruments.json");
        if std::env::var_os("TANG_UPDATE_TYPES").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &fresh).unwrap();
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            committed == fresh,
            "{} is not what the built-in instruments are now: run `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` and commit the result",
            path.display()
        );
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
            let mut json = testutil::wire(ctl.model.graph().nodes.iter().find(|n| n.id == added).unwrap());
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

    // -- What happens to the modulators of a lane and a group that the program has moved to the session's own (step 94: they are nodes of their own that hear the lane or the group, and drive the plugins by their nodes) --

    /// A group with the first lane in it and an LFO that drives that lane's `detune`, an LFO of the lane that drives its `volume`, and a second lane that is in no group.
    const A_GROUP_AND_A_LANE_WITH_MODULATORS: &str = r#"
[[group]]
[[group.modulator]]
[[group.modulator.target]]
member = 0
param = "detune"
depth = 0.5

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.modulator]]
[[instrument.modulator.target]]
param = "volume"
depth = 0.2

[[instrument]]
plugin = "builtin:osc"
"#;

    /// `A_GROUP_AND_A_LANE_WITH_MODULATORS` as the program loads it, with the modulators moved: the lane's LFO first, the group's after it.
    fn a_moved_session() -> (StubEngine, Controller) {
        let (handles, engine) = testutil::stub_engine();
        (engine, Controller::new(load(A_GROUP_AND_A_LANE_WITH_MODULATORS).model, handles, None))
    }

    /// What each of the session's own modulators hears and how many cables it has: `(ear, cables)`.
    fn the_modulators_of(ctl: &Controller) -> Vec<(Option<NodeId>, usize)> {
        ctl.model.free_modulators.iter().map(|m| (m.ear, m.targets.len())).collect()
    }

    #[test]
    fn a_session_that_is_moved_has_the_lanes_lfo_and_the_groups_as_modulators_of_the_sessions_own_that_hear_them() {
        let (_engine, ctl) = a_moved_session();
        let (lane, group) = (ctl.model.instruments[0].node, ctl.model.groups[0].node);
        assert_eq!(the_modulators_of(&ctl), vec![(Some(lane), 1), (Some(group), 1)]);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn taking_a_group_out_leaves_the_modulator_that_heard_it_and_its_cable_to_the_member_and_undo_gives_the_ear_back() {
        let (engine, mut ctl) = a_moved_session();
        let (lane, group) = (ctl.model.instruments[0].node, ctl.model.groups[0].node);
        let modulator = ctl.model.free_modulators[1].node;
        sent(&engine);
        ctl.apply(Intent::RemoveNode { node: group }).unwrap();
        let told = sent(&engine);
        // The modulator stays, with its cable to the member (which stays, in no group): cables go by node, and a group has none of its own to take with it. It hears every note now, as the audio thread has it (its ear names a node
        // that went), and the model keeps the node, so that Undo gives the group back to it.
        assert_eq!(the_modulators_of(&ctl), vec![(Some(lane), 1), (Some(group), 1)]);
        assert_eq!(ctl.model.ear_of(&ctl.model.free_modulators[1]), None);
        assert!(told.iter().all(|c| !c.contains("ModTarget")), "none of its cables is taken out: {told:?}");
        assert!(ctl.model.graph().edges.iter().any(|e| matches!(e.kind, crate::app::graph::EdgeKind::Modulation { .. }) && e.from.node == modulator), "and the graph has it");
        ctl.undo().unwrap();
        assert_eq!(ctl.model.ear_of(&ctl.model.free_modulators[1]), Some(group), "it hears the group that came back");
        assert_eq!(the_modulators_of(&ctl), vec![(Some(lane), 1), (Some(group), 1)]);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_lane_that_leaves_its_group_keeps_the_cables_of_the_modulators_that_hear_the_group() {
        let (engine, mut ctl) = a_moved_session();
        let (lane, group) = (ctl.model.instruments[0].node, ctl.model.groups[0].node);
        sent(&engine);
        ctl.apply(Intent::SetLaneGroup { node: lane, group: None }).unwrap();
        // (A group's modulator was the group's, and its cables to a member went with the member when it left: it is a modulator of the session's own now, which drives the plugin it was drawn to wherever the lane is.)
        assert_eq!(the_modulators_of(&ctl), vec![(Some(lane), 1), (Some(group), 1)]);
        assert!(sent(&engine).iter().all(|c| !c.contains("ModTarget")), "no cable is taken out");
        ctl.undo().unwrap();
        assert_eq!(the_modulators_of(&ctl), vec![(Some(lane), 1), (Some(group), 1)]);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn taking_a_lane_out_leaves_the_modulators_that_heard_it_with_no_cable_to_its_plugins_and_undo_gives_it_all_back() {
        let (engine, mut ctl) = a_moved_session();
        let (lane, group) = (ctl.model.instruments[0].node, ctl.model.groups[0].node);
        ctl.apply(Intent::RemoveNode { node: lane }).unwrap();
        // (The audio thread hands the lane back when it is asked.)
        sent(&engine);
        // Both stay (the lane's own, and the group's): each drove a plugin of the lane, so that the cables go with it, and what the lane's hears is every note then.
        assert_eq!(the_modulators_of(&ctl), vec![(Some(lane), 0), (Some(group), 0)]);
        assert_eq!(ctl.model.ear_of(&ctl.model.free_modulators[0]), None);
        assert_eq!(ctl.model.ear_of(&ctl.model.free_modulators[1]), Some(group));
        ctl.undo().unwrap();
        assert_eq!(the_modulators_of(&ctl), vec![(Some(lane), 1), (Some(group), 1)]);
        assert_eq!(ctl.model.ear_of(&ctl.model.free_modulators[0]), Some(lane), "the lane that came back is heard again");
        ctl.model.check_ids().unwrap();
    }

    // -- The host-side values: a lane's volume, an effect's mix, a group's volume and pan (step 97) --

    use crate::app::model::ModGain;

    /// The nodes of `GAINS` that have a host-side value, by what they are.
    struct GainNodes {
        group: NodeId,
        lane: NodeId,
        filter: NodeId,
        reverb: NodeId,
        empty_lane: NodeId,
        delay: NodeId,
    }

    /// `GAINS` with an LFO that hears every note.
    fn gains_scene() -> (StubEngine, Controller, GainNodes) {
        let (handles, engine) = testutil::stub_engine();
        let mut ctl = Controller::new(load(testutil::GAINS).model, handles, None);
        ctl.apply(Intent::AddFreeModulator { kind: ModulatorKind::Lfo, hears: None }).unwrap();
        let nodes = GainNodes {
            group: ctl.model.groups[0].node,
            lane: ctl.model.instruments[0].node,
            filter: ctl.model.instruments[0].effects[0].node,
            reverb: ctl.model.groups[0].effects[0].node,
            empty_lane: ctl.model.instruments[1].node,
            delay: ctl.model.instruments[1].effects[0].node,
        };
        sent(&engine);
        (engine, ctl, nodes)
    }

    fn cable_to_port(modulator: NodeId, node: NodeId, port: Port) -> Intent {
        Intent::AddModEdge { modulator, to: Endpoint { node, port } }
    }

    #[test]
    fn a_cable_can_be_drawn_to_every_host_side_value_there_is_and_the_audio_thread_is_told_what_it_is_centred_on() {
        let (engine, mut ctl, n) = gains_scene();
        let lfo = ctl.model.free_modulators[0].node;
        let cases = [
            (n.lane, Port::Volume, ModGain::Volume, 0.7, (0.0, 4.0)),
            (n.filter, Port::Mix, ModGain::Mix, 0.9, (0.0, 1.0)),
            (n.group, Port::Volume, ModGain::Volume, 0.8, (0.0, 4.0)),
            (n.group, Port::Pan, ModGain::Pan, -0.25, (-1.0, 1.0)),
            (n.reverb, Port::Mix, ModGain::Mix, 0.6, (0.0, 1.0)),
            (n.delay, Port::Mix, ModGain::Mix, 0.4, (0.0, 1.0)),
        ];
        for (node, port, gain, value, (min, max)) in cases {
            ctl.apply(cable_to_port(lfo, node, port)).unwrap();
            let aim = Driven::Gain { node, gain };
            assert_eq!(sent(&engine), [format!("AddFreeModTarget mod=0 aim={aim:?} depth=0.500 base={value:.3} range=[{min:.3}, {max:.3}]")], "{aim:?}");
            let cable = ctl.model.free_modulators[0].targets.last().unwrap();
            assert_eq!((cable.aim, cable.param_name.as_str(), cable.depth, (cable.param_min, cable.param_max)), (aim, gain.name(), 0.5, (min, max)));
            assert_eq!(ctl.model.centre_of(aim), Some(value), "the user's value is what it is centred on");
            assert_eq!(ctl.model.kind_of(ModulatorLocation::Free, 0, aim), Some(ModTargetKind::Gain { node, gain }));
        }
        assert_eq!(ctl.model.free_modulators[0].targets.len(), 6);
        assert!(ctl.dirty);
        ctl.model.check_ids().unwrap();
        // The graph has each cable as an edge from the modulator to the port of the node, and a client is told the telemetry slot of each by that port.
        let graph = ctl.model.graph();
        let ends: Vec<(NodeId, Port)> = graph.edges.iter().filter(|e| matches!(e.kind, crate::app::graph::EdgeKind::Modulation { .. })).map(|e| (e.to.node, e.to.port)).collect();
        assert_eq!(ends, cases.map(|(node, port, ..)| (node, port)));
    }

    #[test]
    fn a_cable_to_a_host_side_value_that_the_node_has_not_is_refused_and_changes_nothing_not_even_the_ids() {
        let (engine, mut ctl, n) = gains_scene();
        let lfo = ctl.model.free_modulators[0].node;
        let refused = [
            ("the volume of a lane that has no instrument", n.empty_lane, Port::Volume, invalid("that node has no volume")),
            ("the pan of a lane", n.lane, Port::Pan, invalid("that node has no pan")),
            ("the mix of a lane", n.lane, Port::Mix, invalid("that node has no mix")),
            ("the volume of an effect", n.filter, Port::Volume, invalid("that node has no volume")),
            ("the pan of an effect", n.filter, Port::Pan, invalid("that node has no pan")),
            ("the mix of a group", n.group, Port::Mix, invalid("that node has no mix")),
            ("the volume of a node that is not there", NodeId(9_999), Port::Volume, IntentError::NotFound(NodeId(9_999))),
        ];
        let rev = ctl.rev;
        for (what, node, port, error) in refused {
            assert_eq!(ctl.apply(cable_to_port(lfo, node, port)), Err(error), "{what}");
            assert_eq!(ctl.rev, rev, "{what}");
            assert!(sent(&engine).is_empty(), "{what}");
        }
        assert!(ctl.model.free_modulators[0].targets.is_empty());
        ctl.apply(cable_to_port(lfo, n.filter, Port::Mix)).unwrap();
        // A modulator drives a value once; another may drive it as well, and the two add up.
        assert_eq!(ctl.apply(cable_to_port(lfo, n.filter, Port::Mix)), Err(invalid("that modulator drives that mix already")));
        ctl.apply(Intent::AddFreeModulator { kind: ModulatorKind::Envelope, hears: None }).unwrap();
        let envelope = ctl.model.free_modulators[1].node;
        ctl.apply(cable_to_port(envelope, n.filter, Port::Mix)).unwrap();
        // The cables of the refusals used no id: this one has the id the next would have had.
        let ids: Vec<EdgeId> = ctl.model.free_modulators.iter().flat_map(|m| m.targets.iter().map(|t| t.edge)).collect();
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[1].0, ids[0].0 + 1);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn modulate_makes_a_modulator_for_a_host_side_value_that_hears_the_lane_or_the_group_it_belongs_to() {
        let (engine, mut ctl, n) = gains_scene();
        for (node, port, gain, hears) in [
            (n.lane, Port::Volume, ModGain::Volume, n.lane),
            (n.filter, Port::Mix, ModGain::Mix, n.lane),
            (n.delay, Port::Mix, ModGain::Mix, n.empty_lane),
            (n.reverb, Port::Mix, ModGain::Mix, n.group),
            (n.group, Port::Volume, ModGain::Volume, n.group),
            (n.group, Port::Pan, ModGain::Pan, n.group),
        ] {
            sent(&engine);
            modulate(&mut ctl, node, port, ModulatorKind::Lfo, None).unwrap();
            let made = ctl.model.free_modulators.last().unwrap();
            assert_eq!(made.ear, Some(hears), "{gain:?} of {node:?}");
            assert_eq!(made.targets.iter().map(|t| (t.aim, t.depth)).collect::<Vec<_>>(), [(Driven::Gain { node, gain }, 0.5)]);
            let told = sent(&engine);
            assert!(told.first().unwrap().starts_with("InsertFreeModulator"), "{told:?}: the modulator first");
            assert!(told.last().unwrap().contains(&format!("aim={:?}", Driven::Gain { node, gain })), "{told:?}: its cable last");
            // One step of the history: the modulator and its cable, in the words of the button.
            assert_eq!(ctl.history_state().undo.as_deref(), Some(&*format!("modulate {}", gain.name())));
        }
        ctl.model.check_ids().unwrap();
        // The modulator is undone with its cable, and made again under its ids.
        let count = ctl.model.free_modulators.len();
        ctl.undo().unwrap();
        assert_eq!(ctl.model.free_modulators.len(), count - 1);
        ctl.redo().unwrap();
        assert_eq!(ctl.model.free_modulators.len(), count);
        assert_eq!(ctl.model.free_modulators.last().unwrap().targets[0].aim, Driven::Gain { node: n.group, gain: ModGain::Pan });
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn taking_a_cable_to_a_host_side_value_out_gives_the_user_s_value_back_unless_another_cable_drives_it() {
        let (engine, mut ctl, n) = gains_scene();
        let lfo = ctl.model.free_modulators[0].node;
        for (node, port) in [(n.lane, Port::Volume), (n.filter, Port::Mix), (n.reverb, Port::Mix), (n.group, Port::Volume), (n.group, Port::Pan)] {
            ctl.apply(cable_to_port(lfo, node, port)).unwrap();
        }
        let edges: Vec<EdgeId> = ctl.model.free_modulators[0].targets.iter().map(|t| t.edge).collect();
        sent(&engine);
        for (edge, back) in edges.iter().zip(["SetVolume inst=0 value=0.700", "SetMix inst=0 slot=1 value=0.900", "SetGroupMix group=0 fx=0 value=0.600", "SetGroupVolume group=0 value=0.800", "SetGroupPan group=0 value=-0.250"]) {
            ctl.apply(Intent::RemoveEdge { edge: *edge }).unwrap();
            let told = sent(&engine);
            assert_eq!(told.len(), 2, "{told:?}");
            assert!(told[0].starts_with("RemoveFreeModTarget"), "{told:?}");
            assert_eq!(told[1], back);
        }
        // A cable that another still drives the value with leaves the value to it.
        ctl.apply(Intent::AddFreeModulator { kind: ModulatorKind::Envelope, hears: None }).unwrap();
        let envelope = ctl.model.free_modulators[1].node;
        ctl.apply(cable_to_port(lfo, n.filter, Port::Mix)).unwrap();
        ctl.apply(cable_to_port(envelope, n.filter, Port::Mix)).unwrap();
        let first = ctl.model.free_modulators[0].targets[0].edge;
        sent(&engine);
        ctl.apply(Intent::RemoveEdge { edge: first }).unwrap();
        let told = sent(&engine);
        assert_eq!(told.len(), 1, "{told:?}: the envelope still drives the mix, and the value is not given back");
        // The user's own change is what the cables are centred on, and what is given back.
        ctl.apply(Intent::SetGain { node: n.filter, gain: Gain::Mix, value: 0.5 }).unwrap();
        assert_eq!(ctl.model.centre_of(Driven::Gain { node: n.filter, gain: ModGain::Mix }), Some(0.5));
        ctl.model.check_ids().unwrap();
    }
}
