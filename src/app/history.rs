//! The history of what a person did, and the way back (`docs/web-ui.md` §5, D35).
//!
//! Every change that can be undone leaves a `Step`: the intents that put it back and the intents that do it again, in the
//! words a button has for it. The controller keeps the steps in a `History`, and `Undo` and `Redo` are intents like the others:
//! they apply what a step says, so that the audio thread, the model, the TUI and every browser take them as any other change.
//!
//! A change that goes on (a knob dragged turns it at every frame) is one step, not a hundred: a change of what the last step
//! was of, soon enough after it, is merged into it, and the step goes back to before the first.
//!
//! What is added is taken out by undo and put back by redo **under the ids it had** (`Made`, `NodeIds::reuse`): every step that
//! names the node or the cable goes on naming it, whatever is undone and redone around it.

use std::time::{Duration, Instant};

use crate::app::controller::Controller;
use crate::app::graph::NoteRange;
use crate::app::intent::{Applied, Gain, Intent, IntentError, ModulatorField};
use crate::app::model::{EdgeId, Made, ModSourceSlot, NodeId, NodePath, PluginSlot};
use crate::piano_filter::PianoMode;

/// How long after one change of a thing the next change of it may come and still be the same step.
pub const SAME_STEP: Duration = Duration::from_millis(1000);

/// How many steps are kept. The oldest go.
pub const MAX_STEPS: usize = 200;

/// What a change is of, for a change that goes on to be told from the rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// A parameter of a plugin: its node and its own index.
    Param(NodeId, u32),
    /// A gain of a node.
    Gain(NodeId, Gain),
    /// A setting of a modulator.
    Field(NodeId, ModulatorField),
    /// The depth of a cable.
    Depth(EdgeId),
}

/// One thing that was done, and the way back.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    /// What it was, for a button: "set cutoff".
    pub label: String,
    /// The intents that put it back, in the order they are applied.
    pub undo: Vec<Intent>,
    /// The intents that do it again, in the order they are applied.
    pub redo: Vec<Intent>,
    /// What it was of, if it is a change that goes on.
    key: Option<Key>,
    /// When it was last changed.
    last: Instant,
    /// The ids that `undo` makes, in the order it makes them (none, for most).
    undo_made: Made,
    /// The ids that `redo` makes, in the order it makes them: what was added is added again under the ids it had.
    redo_made: Made,
}

impl Step {
    /// A step of one change: `undo` puts back what it found, `redo` is the change.
    pub fn new(label: impl Into<String>, undo: Intent, redo: Intent, key: Option<Key>, at: Instant) -> Step {
        Step { label: label.into(), undo: vec![undo], redo: vec![redo], key, last: at, undo_made: Made::default(), redo_made: Made::default() }
    }

    /// A step of an add: `redo` is the intent, which made `made` the first time, and `undo` takes it out.
    fn added(label: impl Into<String>, undo: Intent, redo: Intent, made: Made, at: Instant) -> Step {
        Step { redo_made: made, ..Step::new(label, undo, redo, None, at) }
    }
}

/// What the buttons need to say: what Undo and Redo would do, or nothing if they would not.
#[cfg(any(test, feature = "web"))]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HistoryState {
    pub undo: Option<String>,
    pub redo: Option<String>,
}

/// The steps that can be undone, the last on top, and the steps that were undone and can be done again.
#[derive(Debug, Default)]
pub struct History {
    undo: Vec<Step>,
    redo: Vec<Step>,
}

impl History {
    /// Something was done. What could have been redone cannot be any more. If it is another change of what the last step
    /// was of, soon after it, the two are one step: it puts back what the first found and does what the last did.
    pub fn record(&mut self, step: Step) {
        self.redo.clear();
        if let (Some(top), Some(key)) = (self.undo.last_mut(), step.key) {
            if top.key == Some(key) && step.last.saturating_duration_since(top.last) <= SAME_STEP {
                top.redo = step.redo;
                top.last = step.last;
                return;
            }
        }
        self.undo.push(step);
        if self.undo.len() > MAX_STEPS {
            self.undo.remove(0);
        }
    }

    /// The step to undo, taken off (it is gone if it cannot be put back; `kept_undone` keeps it to be redone if it was).
    pub fn take_undo(&mut self) -> Option<Step> {
        self.undo.pop()
    }

    /// The step that was undone, to be redone.
    pub fn kept_undone(&mut self, step: Step) {
        self.redo.push(step);
    }

    /// The step to redo, taken off.
    pub fn take_redo(&mut self) -> Option<Step> {
        self.redo.pop()
    }

    /// The step that was redone, to be undone again: as it was, and not merged into the one before it.
    pub fn kept_redone(&mut self, step: Step) {
        self.undo.push(step);
    }

    /// Forget everything: something was done that cannot be undone, and what came before cannot be undone past it. True if there
    /// was something to forget.
    pub fn clear(&mut self) -> bool {
        let had = !self.undo.is_empty() || !self.redo.is_empty();
        self.undo.clear();
        self.redo.clear();
        had
    }

    /// How many steps can be undone.
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    /// The steps that were recorded after the first `mark` become one step, `label`: it goes back through them in the reverse order and does
    /// them again in the order they were done, the nodes and the cables that they make under the ids they made them under. Nothing is joined
    /// unless there are two or more of them.
    pub fn join_above(&mut self, mark: usize, label: &str) {
        if self.undo.len() < mark + 2 {
            return;
        }
        let steps = self.undo.split_off(mark);
        let Some(last) = steps.last().map(|step| step.last) else { return };
        let mut joined = Step { label: label.to_string(), undo: Vec::new(), redo: Vec::new(), key: None, last, undo_made: Made::default(), redo_made: Made::default() };
        for step in &steps {
            joined.redo.extend(step.redo.iter().cloned());
            joined.redo_made.nodes.extend(&step.redo_made.nodes);
            joined.redo_made.edges.extend(&step.redo_made.edges);
        }
        for step in steps.iter().rev() {
            joined.undo.extend(step.undo.iter().cloned());
            joined.undo_made.nodes.extend(&step.undo_made.nodes);
            joined.undo_made.edges.extend(&step.undo_made.edges);
        }
        self.undo.push(joined);
    }

    /// What Undo and Redo would do.
    #[cfg(any(test, feature = "web"))]
    pub fn state(&self) -> HistoryState {
        HistoryState { undo: self.undo.last().map(|step| step.label.clone()), redo: self.redo.last().map(|step| step.label.clone()) }
    }

    #[cfg(test)]
    fn undoable(&self) -> usize {
        self.undo.len()
    }

    #[cfg(test)]
    fn redoable(&self) -> usize {
        self.redo.len()
    }
}

/// What a change is to the history.
pub(super) enum Plan {
    /// It can be undone: this is the step.
    Step(Step),
    /// It adds something, and is undone by taking that out: what it made is known once it is carried out, and the step is made then.
    Added { label: String, intent: Intent, at: Instant },
    /// It cannot be undone yet, and what came before cannot be undone past it.
    Barrier,
    /// It is no change of the session.
    Nothing,
}

impl Controller {
    /// What Undo and Redo would do.
    #[cfg(any(test, feature = "web"))]
    pub fn history_state(&self) -> HistoryState {
        self.history.state()
    }

    /// Where the history is now, for `join_steps_since`: how many steps can be undone.
    pub fn history_mark(&self) -> usize {
        self.history.undo_len()
    }

    /// Make one step, `label`, of the steps that were recorded since `mark`, so that one thing a person asks for that takes several intents (a new
    /// modulator and a cable to it) is one thing to undo: it goes back through them in the reverse order and does them again in the order they were
    /// done. A step that was merged into one before the mark stays in that one, and what cannot be undone has forgotten the history as it does
    /// anywhere (there is nothing to join then). One step, or none, is left as it is.
    pub fn join_steps_since(&mut self, mark: usize, label: &str) {
        self.history.join_above(mark, label);
    }

    /// Put back what the last step that can be undone did. Refused if there is none, or if what it names is not there any more
    /// (a node taken out since: the step is dropped, and the one before it is the next to be undone).
    pub fn undo(&mut self) -> Result<Applied, IntentError> {
        let Some(step) = self.history.take_undo() else {
            return Err(IntentError::Invalid("there is nothing to undo".to_string()));
        };
        let applied = self.carry_out_all(&step.undo, &step.undo_made)?;
        self.history.kept_undone(step);
        Ok(applied)
    }

    /// Do again what the last step that was undone did.
    pub fn redo(&mut self) -> Result<Applied, IntentError> {
        let Some(step) = self.history.take_redo() else {
            return Err(IntentError::Invalid("there is nothing to redo".to_string()));
        };
        let applied = self.carry_out_all(&step.redo, &step.redo_made)?;
        self.history.kept_redone(step);
        Ok(applied)
    }

    /// Carry out `intents` one after the other, each a revision of its own, and say what all of them did. What they make is made under
    /// the ids in `made`, in the order they were handed out the first time: what comes back has the ids it had.
    fn carry_out_all(&mut self, intents: &[Intent], made: &Made) -> Result<Applied, IntentError> {
        self.put_ids_back(made)?;
        let carried = self.carry_out_each(intents);
        // (What the intents did not make is not kept for what is made next.)
        self.model.ids.drop_ids_to_put_back();
        carried
    }

    /// The ids in `made` are the ones to come next. Refused if one of them is in use: a node or a cable cannot have two.
    fn put_ids_back(&mut self, made: &Made) -> Result<(), IntentError> {
        if let Some(node) = made.nodes.iter().find(|node| self.model.locate(**node).is_some()) {
            return Err(IntentError::Invalid(format!("node {} is there already", node.0)));
        }
        if let Some(edge) = made.edges.iter().find(|edge| self.model.locate_edge(**edge).is_some()) {
            return Err(IntentError::Invalid(format!("cable {} is there already", edge.0)));
        }
        self.model.ids.reuse(&made.nodes, &made.edges);
        Ok(())
    }

    fn carry_out_each(&mut self, intents: &[Intent]) -> Result<Applied, IntentError> {
        let mut all: Option<Applied> = None;
        for intent in intents {
            let applied = self.carry_out(intent.clone())?;
            all = Some(match all {
                None => applied,
                Some(mut so_far) => {
                    so_far.rev = applied.rev;
                    so_far.events.extend(applied.events);
                    so_far.restructured |= applied.restructured;
                    so_far
                }
            });
        }
        all.ok_or_else(|| IntentError::Invalid("the step has nothing to do".to_string()))
    }

    /// What `intent` is to the history, read from the session before it is applied: the step that puts back what it will change
    /// (so it is the value it will overwrite that is read), or that it cannot be undone.
    pub(super) fn plan_history(&self, intent: &Intent) -> Plan {
        let now = (self.clock)();
        let step = |label: String, undo: Intent, key: Option<Key>| Plan::Step(Step::new(label, undo, intent.clone(), key, now));
        let added = |label: String| Plan::Added { label, intent: intent.clone(), at: now };
        match intent {
            // (Done before `plan_history` is asked, by `apply`: they are no steps of the history.)
            Intent::Save | Intent::Undo | Intent::Redo => Plan::Nothing,
            Intent::SetParam { node, param, .. } => match self.param_row(*node, *param) {
                Some((name, old)) => step(format!("set {name}"), Intent::SetParam { node: *node, param: *param, value: old }, Some(Key::Param(*node, *param))),
                None => Plan::Nothing,
            },
            Intent::SetGain { node, gain, .. } => match self.gain_value(*node, *gain) {
                Some(old) => step(gain.label().to_string(), Intent::SetGain { node: *node, gain: *gain, value: old }, Some(Key::Gain(*node, *gain))),
                None => Plan::Nothing,
            },
            Intent::SetModulatorField { node, field, .. } => match self.field_value(*node, *field) {
                Some(old) => step(format!("set {}", field.label()), Intent::SetModulatorField { node: *node, field: *field, value: old }, Some(Key::Field(*node, *field))),
                None => Plan::Nothing,
            },
            Intent::SetEdgeDepth { edge, .. } => match self.edge_depth(*edge) {
                Some(old) => step("set the depth of a cable".to_string(), Intent::SetEdgeDepth { edge: *edge, depth: old }, Some(Key::Depth(*edge))),
                None => Plan::Nothing,
            },
            Intent::SetRange { node, .. } => match self.model.locate(*node) {
                Some(NodePath::Instrument(inst)) => {
                    let old = self.model.instruments[inst].range.map(|(low, high)| NoteRange { low, high });
                    step("set the keys".to_string(), Intent::SetRange { node: *node, range: old }, None)
                }
                _ => Plan::Nothing,
            },
            Intent::SetNodeUi { node, .. } => {
                let old = self.model.ui.get(node).copied();
                step("move a node".to_string(), Intent::SetNodeUi { node: *node, ui: old }, None)
            }
            Intent::SetPinned { node, .. } => match self.plugin_slot(*node) {
                Some(plugin) => step("choose the knobs".to_string(), Intent::SetPinned { node: *node, params: plugin.pinned.clone() }, None),
                None => Plan::Nothing,
            },
            Intent::RenameGroup { node, .. } => match self.model.locate(*node) {
                Some(NodePath::Group(group)) => {
                    // (None is the name a group has by default, which a name of nothing gives back.)
                    let old = self.model.groups[group].name.clone().unwrap_or_default();
                    step("rename a group".to_string(), Intent::RenameGroup { node: *node, name: old }, None)
                }
                _ => Plan::Nothing,
            },
            Intent::SetScale { .. } => step("set the scale".to_string(), Intent::SetScale { scale: self.piano_filter.scale().short() }, None),
            Intent::SetLocked { locked } => {
                let label = if *locked { "lock the piano to its scale" } else { "let every note of the piano through" };
                step(label.to_string(), Intent::SetLocked { locked: self.piano_filter.mode() == PianoMode::Locked }, None)
            }
            Intent::MoveEffect { node, .. } => match self.model.locate(*node) {
                Some(NodePath::Effect { index, .. } | NodePath::GroupEffect { index, .. }) => {
                    step("move an effect".to_string(), Intent::MoveEffect { node: *node, index: index as u32 }, None)
                }
                _ => Plan::Nothing,
            },
            // (The catalog names the plugin, or the intent is refused.)
            Intent::AddEffect { plugin, .. } => self.catalog_name(plugin).map_or(Plan::Nothing, |name| added(format!("add {name}"))),
            Intent::AddInstrument { plugin } => self.catalog_name(plugin).map_or(Plan::Nothing, |name| added(format!("add {name} as an instrument"))),
            Intent::AddModulator { kind, .. } => added(format!("add {}", kind.label())),
            Intent::AddModEdge { .. } => added("draw a cable".to_string()),
            Intent::AddGroup { members } => {
                // (A member that is no lane is refused with the rest, so it need not be looked at.)
                let mut lanes = members.iter().filter_map(|member| self.lane_place(*member));
                // A lane that leaves its group is no longer driven by it, and the cables that did are gone: a step does not bring them back.
                if lanes.any(|inst| self.model.instruments[inst].group.is_some()) { Plan::Barrier } else { added("add a group".to_string()) }
            }
            Intent::SetLaneGroup { node, group } => {
                // (A node that is no lane, and a group that is no group, are refused: no step is kept for them.)
                let Some(inst) = self.lane_place(*node) else { return Plan::Nothing };
                let old = self.model.instruments[inst].group.map(|index| self.model.groups[index].node);
                if old == *group {
                    Plan::Nothing
                } else if !self.model.group_cables_into_lane(inst).is_empty() {
                    // The cables of the group it leaves are taken out, and a step does not bring them back.
                    Plan::Barrier
                } else {
                    let label = if group.is_some() { "put a lane in a group" } else { "take a lane out of its group" };
                    step(label.to_string(), Intent::SetLaneGroup { node: *node, group: old }, None)
                }
            }
            // Nothing puts these back yet.
            Intent::LoadPreset { .. } | Intent::RemoveEdge { .. } | Intent::RemoveNode { .. } | Intent::ReplaceInstrument { .. } => Plan::Barrier,
        }
    }

    /// An intent has been carried out: its step is kept, or the history is forgotten.
    pub(super) fn keep_in_history(&mut self, plan: Plan) {
        match plan {
            Plan::Step(step) => self.history.record(step),
            Plan::Added { label, intent, at } => {
                let made = self.made_last.clone();
                match self.takes_out(&intent, &made) {
                    Some(undo) => self.history.record(Step::added(label, undo, intent, made, at)),
                    // (It made something that cannot be named, or what it made is not there: nothing puts it back.)
                    None => {
                        self.history.clear();
                    }
                }
            }
            Plan::Barrier => {
                self.history.clear();
            }
            Plan::Nothing => {}
        }
    }

    /// The intent that takes out what `added`, an add that is carried out, made: the one node, or the one cable.
    fn takes_out(&self, added: &Intent, made: &Made) -> Option<Intent> {
        match (added, made.nodes.as_slice(), made.edges.as_slice()) {
            (Intent::AddModEdge { .. }, [], [edge]) if self.model.locate_edge(*edge).is_some() => Some(Intent::RemoveEdge { edge: *edge }),
            (Intent::AddModEdge { .. }, ..) => None,
            (_, [node], []) if self.model.locate(*node).is_some() => Some(Intent::RemoveNode { node: *node }),
            _ => None,
        }
    }

    /// The name the catalog has for the plugin `source`.
    fn catalog_name(&self, source: &str) -> Option<String> {
        self.catalog.plugins().iter().find(|plugin| plugin.id == source).map(|plugin| plugin.name.clone())
    }

    /// The place of the lane whose node is `node`.
    fn lane_place(&self, node: NodeId) -> Option<usize> {
        match self.model.locate(node)? {
            NodePath::Instrument(inst) => Some(inst),
            _ => None,
        }
    }

    /// The name and the value of parameter `param` of the plugin in `node`.
    fn param_row(&self, node: NodeId, param: u32) -> Option<(String, f32)> {
        let row = self.plugin_slot(node)?.params.iter().find(|row| row.index == param)?;
        Some((row.name.clone(), row.value))
    }

    /// The plugin in `node`: a lane's instrument, an effect of a lane's chain or of a group's bus.
    fn plugin_slot(&self, node: NodeId) -> Option<&PluginSlot> {
        match self.model.locate(node)? {
            NodePath::Instrument(inst) => self.model.instruments[inst].instrument.as_ref(),
            NodePath::Effect { inst, index } => self.model.instruments[inst].effects.get(index),
            NodePath::GroupEffect { group, index } => self.model.groups[group].effects.get(index),
            NodePath::Group(_) | NodePath::Modulator { .. } => None,
        }
    }

    /// The host-side gain `gain` of `node`, if it has it.
    fn gain_value(&self, node: NodeId, gain: Gain) -> Option<f32> {
        match (gain, self.model.locate(node)?) {
            (Gain::Volume, NodePath::Instrument(inst)) => Some(self.model.instruments[inst].volume),
            (Gain::Mix, NodePath::Effect { inst, index }) => self.model.instruments[inst].effects.get(index).map(|effect| effect.mix),
            (Gain::Mix, NodePath::GroupEffect { group, index }) => self.model.groups[group].effects.get(index).map(|effect| effect.mix),
            (Gain::GroupVolume, NodePath::Group(group)) => Some(self.model.groups[group].volume),
            (Gain::GroupPan, NodePath::Group(group)) => Some(self.model.groups[group].pan),
            _ => None,
        }
    }

    /// The value of a setting of the modulator in `node`, if its kind has it.
    fn field_value(&self, node: NodeId, field: ModulatorField) -> Option<f32> {
        let NodePath::Modulator { at, index } = self.model.locate(node)? else { return None };
        match (&self.model.modulators(at)?.get(index)?.source, field) {
            (ModSourceSlot::Lfo { rate, .. }, ModulatorField::Rate) => Some(*rate),
            (ModSourceSlot::Lfo { waveform, .. }, ModulatorField::Waveform) => Some(waveform.to_index() as f32),
            (ModSourceSlot::Envelope { attack, .. }, ModulatorField::Attack) => Some(*attack),
            (ModSourceSlot::Envelope { decay, .. }, ModulatorField::Decay) => Some(*decay),
            (ModSourceSlot::Envelope { sustain, .. }, ModulatorField::Sustain) => Some(*sustain),
            (ModSourceSlot::Envelope { release, .. }, ModulatorField::Release) => Some(*release),
            _ => None,
        }
    }

    /// The depth of the cable `edge`.
    fn edge_depth(&self, edge: EdgeId) -> Option<f32> {
        let (at, index, target) = self.model.locate_edge(edge)?;
        Some(self.model.modulators(at)?.get(index)?.targets.get(target)?.depth)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use crate::app::controller::Controller;
    use crate::app::graph::{Endpoint, NoteRange, Port};
    use crate::app::intent::{Applied, Event, IntentError, ModulatorKind};
    use crate::app::model::{Made, ModSourceSlot, NodePath, SessionModel};
    use crate::app::testutil::{self, StubEngine, described, load, wire};
    use crate::session::NodeUi;

    const T0: fn() -> Instant = Instant::now;

    /// A step that sets the lock to `redo` and puts it back to `undo`: what a step is made of does not matter to the history.
    fn step(label: &str, undo: bool, redo: bool, key: Option<Key>, at: Instant) -> Step {
        Step::new(label, Intent::SetLocked { locked: undo }, Intent::SetLocked { locked: redo }, key, at)
    }

    fn knob(n: u32) -> Option<Key> {
        Some(Key::Param(NodeId(1), n))
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn a_new_history_has_nothing_to_undo_or_redo() {
        let mut history = History::default();
        assert_eq!(history.state(), HistoryState { undo: None, redo: None });
        assert_eq!(history.take_undo(), None);
        assert_eq!(history.take_redo(), None);
    }

    #[test]
    fn a_step_that_is_recorded_is_what_undo_would_undo_and_the_last_of_them_comes_first() {
        let start = T0();
        let mut history = History::default();
        history.record(step("set cutoff", false, true, None, start));
        history.record(step("set mix", true, false, None, start + ms(5000)));
        assert_eq!(history.state(), HistoryState { undo: Some("set mix".to_string()), redo: None });
        let last = history.take_undo().unwrap();
        assert_eq!((last.label.as_str(), last.undo, last.redo), ("set mix", vec![Intent::SetLocked { locked: true }], vec![Intent::SetLocked { locked: false }]));
        assert_eq!(history.state().undo.as_deref(), Some("set cutoff"));
    }

    #[test]
    fn a_step_that_was_undone_can_be_redone_and_is_then_to_be_undone_again() {
        let start = T0();
        let mut history = History::default();
        history.record(step("set cutoff", false, true, None, start));
        let undone = history.take_undo().unwrap();
        history.kept_undone(undone);
        assert_eq!(history.state(), HistoryState { undo: None, redo: Some("set cutoff".to_string()) });
        let redone = history.take_redo().unwrap();
        history.kept_redone(redone);
        assert_eq!(history.state(), HistoryState { undo: Some("set cutoff".to_string()), redo: None });
    }

    #[test]
    fn a_step_that_is_recorded_after_an_undo_ends_what_could_be_redone() {
        let start = T0();
        let mut history = History::default();
        history.record(step("set cutoff", false, true, None, start));
        history.record(step("set mix", false, true, None, start + ms(5000)));
        let undone = history.take_undo().unwrap();
        history.kept_undone(undone);
        assert_eq!(history.state().redo.as_deref(), Some("set mix"));
        history.record(step("set volume", false, true, None, start + ms(9000)));
        assert_eq!(history.state(), HistoryState { undo: Some("set volume".to_string()), redo: None });
    }

    #[test]
    fn changes_of_one_thing_that_follow_each_other_closely_are_one_step_that_goes_back_to_before_the_first() {
        let start = T0();
        let mut history = History::default();
        // A knob dragged: a change at every frame, in the order they came.
        history.record(Step::new("set cutoff", Intent::SetLocked { locked: false }, Intent::SetLocked { locked: true }, knob(3), start));
        history.record(Step::new("set cutoff", Intent::SetLocked { locked: true }, Intent::SetLocked { locked: false }, knob(3), start + ms(16)));
        history.record(Step::new("set cutoff", Intent::SetLocked { locked: false }, Intent::SetLocked { locked: true }, knob(3), start + ms(32)));
        let only = history.take_undo().unwrap();
        // It puts back what the first change found, and does again what the last did.
        assert_eq!((only.undo, only.redo), (vec![Intent::SetLocked { locked: false }], vec![Intent::SetLocked { locked: true }]));
        assert_eq!(history.take_undo(), None, "one step and no more");
    }

    #[test]
    fn a_drag_that_goes_on_is_one_step_for_as_long_as_no_change_is_a_second_after_the_one_before() {
        let start = T0();
        let mut history = History::default();
        for frame in 0..10 {
            // Every 900 ms for nine seconds: each is soon enough after the one before, though the first is far from the last.
            history.record(step("set cutoff", false, true, knob(3), start + ms(900 * frame)));
        }
        assert!(history.take_undo().is_some());
        assert_eq!(history.take_undo(), None);
    }

    #[test]
    fn changes_of_one_thing_a_second_apart_or_more_are_steps_of_their_own() {
        let start = T0();
        let mut history = History::default();
        history.record(step("set cutoff", false, true, knob(3), start));
        // Exactly a second after the last is still the same step: the window is closed at the end it has.
        history.record(step("set cutoff", true, false, knob(3), start + ms(1000)));
        assert_eq!(history.undoable(), 1);
        history.record(step("set cutoff", false, true, knob(3), start + ms(2001)));
        assert_eq!(history.undoable(), 2);
    }

    #[test]
    fn changes_of_other_things_are_steps_of_their_own_and_so_is_a_change_after_a_step_that_is_not_its_own() {
        let start = T0();
        let mut history = History::default();
        history.record(step("set cutoff", false, true, knob(3), start));
        history.record(step("set resonance", false, true, knob(4), start + ms(10)));
        // Back to the first knob: the last step is the second's, so this is a step of its own.
        history.record(step("set cutoff", false, true, knob(3), start + ms(20)));
        assert_eq!(history.undoable(), 3);
        // A change that has no key is never merged, whatever came before it.
        history.record(step("rename a group", false, true, None, start + ms(30)));
        history.record(step("rename a group", false, true, None, start + ms(40)));
        assert_eq!(history.undoable(), 5);
    }

    #[test]
    fn a_change_after_an_undo_is_a_step_of_its_own_and_ends_the_redo() {
        let start = T0();
        let mut history = History::default();
        history.record(step("set cutoff", false, true, knob(3), start));
        let undone = history.take_undo().unwrap();
        history.kept_undone(undone);
        history.record(step("set cutoff", false, true, knob(3), start + ms(10)));
        // The redo that could have been is gone, and the change is a step of its own (it is not merged into what was undone).
        assert_eq!((history.undoable(), history.redoable()), (1, 0));
    }

    #[test]
    fn clearing_the_history_leaves_nothing_to_undo_or_redo() {
        let start = T0();
        let mut history = History::default();
        history.record(step("set cutoff", false, true, None, start));
        history.record(step("set mix", false, true, None, start + ms(5000)));
        let undone = history.take_undo().unwrap();
        history.kept_undone(undone);
        assert!(history.clear(), "there was something");
        assert_eq!(history.state(), HistoryState { undo: None, redo: None });
        assert!(!history.clear(), "and now there is not");
    }

    #[test]
    fn clearing_says_there_was_something_when_there_was_only_something_to_redo() {
        let mut history = History::default();
        history.record(step("set cutoff", false, true, None, Instant::now()));
        let undone = history.take_undo().unwrap();
        history.kept_undone(undone);
        assert_eq!((history.undoable(), history.redoable()), (0, 1));
        assert!(history.clear());
        assert_eq!(history.redoable(), 0);
    }

    #[test]
    fn the_oldest_steps_go_when_there_are_more_than_it_keeps() {
        let start = T0();
        let mut history = History::default();
        for n in 0..(MAX_STEPS as u64 + 5) {
            history.record(step(&format!("step {n}"), false, true, None, start + ms(5000 * n)));
        }
        assert_eq!(history.undoable(), MAX_STEPS);
        assert_eq!(history.state().undo.as_deref(), Some(format!("step {}", MAX_STEPS + 4).as_str()));
        // The oldest that is left is the sixth.
        let mut oldest = None;
        while let Some(step) = history.take_undo() {
            oldest = Some(step.label);
        }
        assert_eq!(oldest.as_deref(), Some("step 5"));
    }

    // -- the controller keeps the history --

    /// A group with a bus filter, another with nothing and no name, a lane in the first with a reverb and a filter, an LFO whose cable goes to its detune and an envelope, and a lane of its own.
    const SESSION: &str = r#"
[[group]]
name = "Pad"
[[group.effect]]
plugin = "builtin:filter"

[[group]]

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.effect]]
plugin = "builtin:reverb"
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.modulator]]
type = "lfo"
rate = 2.0
[[instrument.modulator.target]]
param = "detune"
depth = 0.5
[[instrument.modulator]]
type = "envelope"
attack = 0.1
decay = 0.2
sustain = 0.3
release = 0.4
[[instrument.modulator.target]]
param = "volume"
depth = 0.25

[[instrument]]
plugin = "builtin:osc"
[[instrument.modulator]]
type = "lfo"
rate = 3.0
"#;

    /// A clock that a test turns.
    #[derive(Clone)]
    struct Clock(Arc<Mutex<Instant>>);

    impl Clock {
        fn advance(&self, by: Duration) {
            *self.0.lock().unwrap() += by;
        }
    }

    fn controller() -> (StubEngine, Controller, Clock) {
        controller_over(load(SESSION).model)
    }

    /// A controller over `model` and a stub engine, with a clock that the test turns.
    fn controller_over(model: SessionModel) -> (StubEngine, Controller, Clock) {
        let (handles, engine) = testutil::stub_engine();
        let mut ctl = Controller::new(model, handles, None);
        let clock = Clock(Arc::new(Mutex::new(Instant::now())));
        let shared = clock.clone();
        ctl.clock = Box::new(move || *shared.0.lock().unwrap());
        (engine, ctl, clock)
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

    /// The index and value of a parameter of the plugin at `path`.
    fn param(ctl: &Controller, path: NodePath, name: &str) -> (u32, f32) {
        let slot = match path {
            NodePath::Instrument(i) => ctl.model.instruments[i].instrument.as_ref().unwrap(),
            NodePath::Effect { inst, index } => &ctl.model.instruments[inst].effects[index],
            NodePath::GroupEffect { group, index } => &ctl.model.groups[group].effects[index],
            _ => panic!("{path:?} has no plugin"),
        };
        let row = slot.params.iter().find(|r| r.name == name).unwrap();
        (row.index, row.value)
    }

    fn detune(ctl: &Controller) -> (u32, f32) {
        param(ctl, NodePath::Instrument(0), "detune")
    }

    fn set_detune(ctl: &mut Controller, value: f32) -> Applied {
        let (index, _) = detune(ctl);
        ctl.apply(Intent::SetParam { node: lane(ctl), param: index, value }).unwrap()
    }

    fn undo_label(ctl: &Controller) -> Option<String> {
        ctl.history_state().undo
    }

    fn redo_label(ctl: &Controller) -> Option<String> {
        ctl.history_state().redo
    }

    #[test]
    fn a_parameter_that_was_set_is_put_back_in_the_model_and_the_engine_and_set_again_by_redo() {
        let (engine, mut ctl, _clock) = controller();
        let (detune_index, _) = detune(&ctl);
        set_detune(&mut ctl, 3.0);
        sent(&engine);
        assert_eq!((undo_label(&ctl), redo_label(&ctl)), (Some("set detune".to_string()), None));

        let undone = ctl.undo().unwrap();
        // It is an intent like the others: a revision, the event, the engine's command.
        assert_eq!(undone, Applied::of(2, vec![Event::ParamSet { node: lane(&ctl), param: detune_index, value: 0.0 }]));
        assert_eq!(detune(&ctl).1, 0.0);
        assert_eq!(sent(&engine), [format!("SetParameter inst=0 slot=0 param={detune_index} value=0.000")]);
        assert_eq!((undo_label(&ctl), redo_label(&ctl)), (None, Some("set detune".to_string())));

        let redone = ctl.redo().unwrap();
        assert_eq!(redone, Applied::of(3, vec![Event::ParamSet { node: lane(&ctl), param: detune_index, value: 3.0 }]));
        assert_eq!(detune(&ctl).1, 3.0);
        assert_eq!(sent(&engine), [format!("SetParameter inst=0 slot=0 param={detune_index} value=3.000")]);
        assert_eq!((undo_label(&ctl), redo_label(&ctl)), (Some("set detune".to_string()), None));
        assert!(ctl.dirty);
    }

    #[test]
    fn what_a_parameter_was_set_to_after_clamping_is_what_redo_sets_and_what_it_had_is_what_undo_puts_back() {
        let (_engine, mut ctl, clock) = controller();
        set_detune(&mut ctl, 99.0);
        assert_eq!(detune(&ctl).1, 24.0);
        clock.advance(Duration::from_secs(5));
        set_detune(&mut ctl, -99.0);
        ctl.undo().unwrap();
        // Back to what it had, which was the clamped value, and not to what was asked.
        assert_eq!(detune(&ctl).1, 24.0);
        ctl.undo().unwrap();
        assert_eq!(detune(&ctl).1, 0.0);
        ctl.redo().unwrap();
        ctl.redo().unwrap();
        assert_eq!(detune(&ctl).1, -24.0);
    }

    #[test]
    fn a_drag_is_one_step_that_goes_back_to_before_it() {
        let (_engine, mut ctl, clock) = controller();
        for (frame, value) in [1.0, 2.0, 3.0, 4.0].into_iter().enumerate() {
            set_detune(&mut ctl, value);
            clock.advance(Duration::from_millis(16 + frame as u64));
        }
        ctl.undo().unwrap();
        assert_eq!(detune(&ctl).1, 0.0);
        assert!(ctl.undo().is_err(), "that was all of it");
        ctl.redo().unwrap();
        assert_eq!(detune(&ctl).1, 4.0);
    }

    #[test]
    fn a_change_a_while_after_the_last_is_a_step_of_its_own() {
        let (_engine, mut ctl, clock) = controller();
        set_detune(&mut ctl, 1.0);
        clock.advance(Duration::from_millis(1500));
        set_detune(&mut ctl, 2.0);
        ctl.undo().unwrap();
        assert_eq!(detune(&ctl).1, 1.0);
        ctl.undo().unwrap();
        assert_eq!(detune(&ctl).1, 0.0);
    }

    #[test]
    fn changes_of_two_parameters_are_two_steps_in_the_order_they_were_made() {
        let (_engine, mut ctl, _clock) = controller();
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        let (damping, _) = param(&ctl, NodePath::Effect { inst: 0, index: 0 }, "damping");
        let before = param(&ctl, NodePath::Effect { inst: 0, index: 0 }, "damping").1;
        set_detune(&mut ctl, 3.0);
        ctl.apply(Intent::SetParam { node: reverb, param: damping, value: 0.9 }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("set damping"));
        ctl.undo().unwrap();
        assert_eq!(param(&ctl, NodePath::Effect { inst: 0, index: 0 }, "damping").1, before);
        assert_eq!(detune(&ctl).1, 3.0, "the other is as it was");
        ctl.undo().unwrap();
        assert_eq!(detune(&ctl).1, 0.0);
    }

    #[test]
    fn a_parameter_of_a_bus_effect_is_put_back_by_the_groups_command() {
        let (engine, mut ctl, _clock) = controller();
        let bus = NodePath::GroupEffect { group: 0, index: 0 };
        let (cutoff, before) = param(&ctl, bus, "cutoff");
        ctl.apply(Intent::SetParam { node: node(&ctl, bus), param: cutoff, value: 0.5 }).unwrap();
        sent(&engine);
        ctl.undo().unwrap();
        assert_eq!(param(&ctl, bus, "cutoff").1, before);
        assert_eq!(sent(&engine), [format!("SetGroupParameter group=0 fx=0 param={cutoff} value={before:.3}")]);
    }

    #[test]
    fn the_gains_are_put_back_each_by_its_own_command() {
        let (engine, mut ctl, clock) = controller();
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        let group = node(&ctl, NodePath::Group(0));
        let before = (ctl.model.instruments[0].volume, ctl.model.instruments[0].effects[0].mix, ctl.model.groups[0].volume, ctl.model.groups[0].pan);
        for (node, gain, value) in [(lane(&ctl), Gain::Volume, 2.5), (reverb, Gain::Mix, 0.125), (group, Gain::GroupVolume, 0.5), (group, Gain::GroupPan, -0.5)] {
            ctl.apply(Intent::SetGain { node, gain, value }).unwrap();
            clock.advance(Duration::from_secs(5));
        }
        sent(&engine);
        assert_eq!(undo_label(&ctl).as_deref(), Some("set pan"));
        for _ in 0..4 {
            ctl.undo().unwrap();
        }
        assert_eq!((ctl.model.instruments[0].volume, ctl.model.instruments[0].effects[0].mix, ctl.model.groups[0].volume, ctl.model.groups[0].pan), before);
        assert_eq!(
            sent(&engine),
            [
                format!("SetGroupPan group=0 value={:.3}", before.3),
                format!("SetGroupVolume group=0 value={:.3}", before.2),
                format!("SetMix inst=0 slot=1 value={:.3}", before.1),
                format!("SetVolume inst=0 value={:.3}", before.0),
            ]
        );
        for _ in 0..4 {
            ctl.redo().unwrap();
        }
        assert_eq!((ctl.model.instruments[0].volume, ctl.model.instruments[0].effects[0].mix, ctl.model.groups[0].volume, ctl.model.groups[0].pan), (2.5, 0.125, 0.5, -0.5));
    }

    #[test]
    fn the_settings_of_a_modulator_and_the_depth_of_a_cable_are_put_back() {
        let (engine, mut ctl, clock) = controller();
        let lfo = ctl.model.instruments[0].modulators[0].node;
        let edge = ctl.model.instruments[0].modulators[0].targets[0].edge;
        for (field, value) in [(ModulatorField::Rate, 9.0), (ModulatorField::Waveform, 1.0), (ModulatorField::Waveform, 2.0)] {
            ctl.apply(Intent::SetModulatorField { node: lfo, field, value }).unwrap();
            clock.advance(Duration::from_secs(5));
        }
        ctl.apply(Intent::SetEdgeDepth { edge, depth: -0.75 }).unwrap();
        sent(&engine);
        assert_eq!(undo_label(&ctl).as_deref(), Some("set the depth of a cable"));
        for _ in 0..4 {
            ctl.undo().unwrap();
        }
        let ModSourceSlot::Lfo { waveform, rate } = ctl.model.instruments[0].modulators[0].source else { panic!("an LFO") };
        assert_eq!((format!("{waveform:?}"), rate, ctl.model.instruments[0].modulators[0].targets[0].depth), ("Sine".to_string(), 2.0, 0.5));
        assert_eq!(
            sent(&engine),
            [
                "SetModTargetDepth inst=0 mod=0 target=0 depth=0.500",
                "SetModulatorWaveform inst=0 mod=0 waveform=Triangle",
                "SetModulatorWaveform inst=0 mod=0 waveform=Sine",
                "SetModulatorRate inst=0 mod=0 rate=2.000"
            ]
        );
        for _ in 0..4 {
            ctl.redo().unwrap();
        }
        let ModSourceSlot::Lfo { waveform, rate } = ctl.model.instruments[0].modulators[0].source else { panic!("an LFO") };
        assert_eq!((format!("{waveform:?}"), rate, ctl.model.instruments[0].modulators[0].targets[0].depth), ("Saw".to_string(), 9.0, -0.75));
    }

    #[test]
    fn the_stages_of_an_envelope_are_each_put_back_to_what_they_were() {
        let (_engine, mut ctl, clock) = controller();
        let envelope = ctl.model.instruments[0].modulators[1].node;
        let stages = [(ModulatorField::Attack, 0.1, 1.5), (ModulatorField::Decay, 0.2, 1.6), (ModulatorField::Sustain, 0.3, 0.9), (ModulatorField::Release, 0.4, 1.7)];
        for (field, _, value) in stages {
            ctl.apply(Intent::SetModulatorField { node: envelope, field, value }).unwrap();
            clock.advance(Duration::from_secs(5));
        }
        assert_eq!(undo_label(&ctl).as_deref(), Some("set release"));
        for (field, was, _) in stages.into_iter().rev() {
            let applied = ctl.undo().unwrap();
            assert_eq!(applied.events, [Event::ModulatorFieldSet { node: envelope, field, value: was }]);
        }
    }

    #[test]
    fn two_settings_of_one_modulator_that_are_changed_in_a_moment_are_two_steps() {
        let (_engine, mut ctl, _clock) = controller();
        let lfo = ctl.model.instruments[0].modulators[0].node;
        ctl.apply(Intent::SetModulatorField { node: lfo, field: ModulatorField::Rate, value: 9.0 }).unwrap();
        ctl.apply(Intent::SetModulatorField { node: lfo, field: ModulatorField::Waveform, value: 3.0 }).unwrap();
        ctl.undo().unwrap();
        let ModSourceSlot::Lfo { waveform, rate } = ctl.model.instruments[0].modulators[0].source else { panic!("an LFO") };
        assert_eq!((format!("{waveform:?}"), rate), ("Sine".to_string(), 9.0));
    }

    #[test]
    fn the_depth_of_a_cable_that_is_dragged_is_one_step() {
        let (_engine, mut ctl, _clock) = controller();
        let edge = ctl.model.instruments[0].modulators[0].targets[0].edge;
        for depth in [0.6, 0.7, 0.8, 0.9] {
            ctl.apply(Intent::SetEdgeDepth { edge, depth }).unwrap();
        }
        ctl.undo().unwrap();
        assert_eq!(ctl.model.instruments[0].modulators[0].targets[0].depth, 0.5);
        assert!(ctl.undo().is_err());
    }

    #[test]
    fn two_parameters_of_one_plugin_that_are_turned_one_after_the_other_are_two_steps() {
        let (_engine, mut ctl, _clock) = controller();
        let (volume, _) = param(&ctl, NodePath::Instrument(0), "volume");
        set_detune(&mut ctl, 3.0);
        ctl.apply(Intent::SetParam { node: lane(&ctl), param: volume, value: 0.5 }).unwrap();
        ctl.undo().unwrap();
        assert_eq!((detune(&ctl).1, param(&ctl, NodePath::Instrument(0), "volume").1), (3.0, 1.0));
        ctl.undo().unwrap();
        assert_eq!(detune(&ctl).1, 0.0);
    }

    #[test]
    fn the_volume_and_the_pan_of_a_group_are_two_steps_for_all_they_are_changed_in_a_moment() {
        let (_engine, mut ctl, _clock) = controller();
        let group = node(&ctl, NodePath::Group(0));
        ctl.apply(Intent::SetGain { node: group, gain: Gain::GroupVolume, value: 0.5 }).unwrap();
        ctl.apply(Intent::SetGain { node: group, gain: Gain::GroupPan, value: 0.25 }).unwrap();
        ctl.undo().unwrap();
        assert_eq!((ctl.model.groups[0].volume, ctl.model.groups[0].pan), (0.5, 0.0));
        ctl.undo().unwrap();
        assert_eq!(ctl.model.groups[0].volume, 1.0);
    }

    #[test]
    fn a_scale_is_put_back_to_the_one_it_was_and_the_lock_has_its_words_for_both_ways() {
        let (_engine, mut ctl, clock) = controller();
        ctl.apply(Intent::SetScale { scale: "F# dorian".to_string() }).unwrap();
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::SetScale { scale: "A minor".to_string() }).unwrap();
        ctl.undo().unwrap();
        assert_eq!(ctl.piano_filter.scale().short(), "F# dorian");
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::SetLocked { locked: true }).unwrap();
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::SetLocked { locked: false }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("let every note of the piano through"));
    }

    #[test]
    fn a_step_that_changes_the_structure_says_so_when_it_is_undone_among_others() {
        let (_engine, mut ctl, _clock) = controller();
        let (detune_index, _) = detune(&ctl);
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        let mut step = Step::new("do two things", Intent::SetParam { node: lane(&ctl), param: detune_index, value: 1.0 }, Intent::SetParam { node: lane(&ctl), param: detune_index, value: 2.0 }, None, Instant::now());
        step.undo.push(Intent::MoveEffect { node: reverb, index: 1 });
        ctl.history.record(step);
        let undone = ctl.undo().unwrap();
        assert!(undone.restructured, "the second of them did");
        assert_eq!(undone.events.len(), 1, "and the first did not");
    }

    #[test]
    fn the_keys_of_a_lane_are_put_back_and_every_note_is_no_range() {
        let (engine, mut ctl, clock) = controller();
        ctl.apply(Intent::SetRange { node: lane(&ctl), range: Some(NoteRange { low: 36, high: 60 }) }).unwrap();
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::SetRange { node: lane(&ctl), range: None }).unwrap();
        sent(&engine);
        ctl.undo().unwrap();
        assert_eq!(ctl.model.instruments[0].range, Some((36, 60)));
        ctl.undo().unwrap();
        assert_eq!(ctl.model.instruments[0].range, None);
        assert_eq!(sent(&engine), ["SetInstrumentRange inst=0 range=Some((36, 60))", "SetInstrumentRange inst=0 range=None"]);
        ctl.redo().unwrap();
        assert_eq!(ctl.model.instruments[0].range, Some((36, 60)));
    }

    #[test]
    fn the_place_of_a_node_is_put_back_and_a_node_that_had_none_has_none_again() {
        let (_engine, mut ctl, clock) = controller();
        let node = lane(&ctl);
        ctl.apply(Intent::SetNodeUi { node, ui: Some(NodeUi { x: 10.0, y: 20.0 }) }).unwrap();
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::SetNodeUi { node, ui: Some(NodeUi { x: 30.0, y: 40.0 }) }).unwrap();
        ctl.undo().unwrap();
        assert_eq!(ctl.model.ui.get(&node), Some(&NodeUi { x: 10.0, y: 20.0 }));
        let applied = ctl.undo().unwrap();
        assert_eq!(applied.events, [Event::NodeUiSet { node, ui: None }]);
        assert_eq!(ctl.model.ui.get(&node), None);
        ctl.redo().unwrap();
        ctl.redo().unwrap();
        assert_eq!(ctl.model.ui.get(&node), Some(&NodeUi { x: 30.0, y: 40.0 }));
    }

    #[test]
    fn the_places_of_nodes_that_are_put_one_after_the_other_are_a_step_each() {
        // (Arranging the nodes again is a request for each of them; that is a step each, and not a step that goes back
        // to the place of the first only.)
        let (_engine, mut ctl, _clock) = controller();
        let (a, b) = (lane(&ctl), node(&ctl, NodePath::Group(0)));
        ctl.apply(Intent::SetNodeUi { node: a, ui: Some(NodeUi { x: 1.0, y: 1.0 }) }).unwrap();
        ctl.apply(Intent::SetNodeUi { node: b, ui: Some(NodeUi { x: 2.0, y: 2.0 }) }).unwrap();
        ctl.undo().unwrap();
        assert_eq!((ctl.model.ui.contains_key(&a), ctl.model.ui.contains_key(&b)), (true, false));
    }

    #[test]
    fn the_knobs_a_node_shows_are_put_back() {
        let (_engine, mut ctl, clock) = controller();
        let (detune_index, _) = detune(&ctl);
        ctl.apply(Intent::SetPinned { node: lane(&ctl), params: vec![detune_index] }).unwrap();
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::SetPinned { node: lane(&ctl), params: vec![] }).unwrap();
        ctl.undo().unwrap();
        assert_eq!(ctl.model.instruments[0].instrument.as_ref().unwrap().pinned, [detune_index]);
        ctl.undo().unwrap();
        assert!(ctl.model.instruments[0].instrument.as_ref().unwrap().pinned.is_empty());
        ctl.redo().unwrap();
        assert_eq!(ctl.model.instruments[0].instrument.as_ref().unwrap().pinned, [detune_index]);
    }

    #[test]
    fn the_name_of_a_group_is_put_back_and_a_group_that_had_none_has_its_default_name_again() {
        let (_engine, mut ctl, clock) = controller();
        let group = node(&ctl, NodePath::Group(0));
        ctl.apply(Intent::RenameGroup { node: group, name: "Strings".to_string() }).unwrap();
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::RenameGroup { node: group, name: String::new() }).unwrap();
        assert_eq!(ctl.model.groups[0].name, None);
        let applied = ctl.undo().unwrap();
        assert_eq!(applied.events, [Event::GroupRenamed { node: group, name: "Strings".to_string() }]);
        let applied = ctl.undo().unwrap();
        assert_eq!(applied.events, [Event::GroupRenamed { node: group, name: "Pad".to_string() }]);
        assert_eq!(ctl.model.groups[0].name.as_deref(), Some("Pad"));
    }

    #[test]
    fn a_group_that_had_no_name_is_given_none_again_and_not_the_name_it_was_called_by() {
        let (_engine, mut ctl, _clock) = controller();
        ctl.model.groups[0].name = None;
        let group = node(&ctl, NodePath::Group(0));
        ctl.apply(Intent::RenameGroup { node: group, name: "Strings".to_string() }).unwrap();
        let applied = ctl.undo().unwrap();
        assert_eq!(ctl.model.groups[0].name, None);
        assert_eq!(applied.events, [Event::GroupRenamed { node: group, name: "Group 1".to_string() }]);
    }

    #[test]
    fn the_scale_and_the_lock_of_the_piano_are_put_back() {
        let (_engine, mut ctl, clock) = controller();
        let before = ctl.piano_filter.scale().short();
        ctl.apply(Intent::SetScale { scale: "F# dorian".to_string() }).unwrap();
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::SetLocked { locked: true }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("lock the piano to its scale"));
        ctl.undo().unwrap();
        assert_eq!(ctl.piano_filter.mode(), crate::piano_filter::PianoMode::Highlight);
        let applied = ctl.undo().unwrap();
        assert_eq!(ctl.piano_filter.scale().short(), before);
        assert!(matches!(applied.events.as_slice(), [Event::ScaleSet { .. }]));
        ctl.redo().unwrap();
        ctl.redo().unwrap();
        assert_eq!((ctl.piano_filter.scale().short(), ctl.piano_filter.mode()), ("F# dorian".to_string(), crate::piano_filter::PianoMode::Locked));
    }

    #[test]
    fn an_effect_that_was_moved_is_put_back_in_its_place_and_the_structure_is_said_to_have_changed() {
        let (engine, mut ctl, _clock) = controller();
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        ctl.apply(Intent::MoveEffect { node: reverb, index: 1 }).unwrap();
        sent(&engine);
        assert_eq!(undo_label(&ctl).as_deref(), Some("move an effect"));
        let undone = ctl.undo().unwrap();
        assert!(undone.restructured && undone.events.is_empty());
        assert_eq!(ctl.model.locate(reverb), Some(NodePath::Effect { inst: 0, index: 0 }));
        assert_eq!(sent(&engine), ["ReorderEffect inst=0 from=1 to=0"]);
        let redone = ctl.redo().unwrap();
        assert!(redone.restructured);
        assert_eq!(ctl.model.locate(reverb), Some(NodePath::Effect { inst: 0, index: 1 }));
    }

    #[test]
    fn a_bus_effect_that_was_moved_is_put_back_by_the_groups_command() {
        let (handles, engine) = testutil::stub_engine();
        let session = SESSION.replace("[[group.effect]]\nplugin = \"builtin:filter\"", "[[group.effect]]\nplugin = \"builtin:filter\"\n[[group.effect]]\nplugin = \"builtin:reverb\"");
        let mut ctl = Controller::new(load(&session).model, handles, None);
        let reverb = node(&ctl, NodePath::GroupEffect { group: 0, index: 1 });
        ctl.apply(Intent::MoveEffect { node: reverb, index: 0 }).unwrap();
        sent(&engine);
        ctl.undo().unwrap();
        assert_eq!(ctl.model.locate(reverb), Some(NodePath::GroupEffect { group: 0, index: 1 }));
        assert_eq!(sent(&engine), ["ReorderGroupEffect group=0 from=0 to=1"]);
    }

    // -- what cannot be undone, and what cannot be undone any more --

    #[test]
    fn the_intents_undo_and_redo_do_what_the_methods_do_and_are_no_steps_of_the_history_themselves() {
        let (engine, mut ctl, _clock) = controller();
        let (detune_index, _) = detune(&ctl);
        set_detune(&mut ctl, 3.0);
        sent(&engine);
        let undone = ctl.apply(Intent::Undo).unwrap();
        assert_eq!(undone, Applied::of(2, vec![Event::ParamSet { node: lane(&ctl), param: detune_index, value: 0.0 }]));
        assert_eq!(sent(&engine), [format!("SetParameter inst=0 slot=0 param={detune_index} value=0.000")]);
        assert_eq!((undo_label(&ctl), redo_label(&ctl)), (None, Some("set detune".to_string())));
        let redone = ctl.apply(Intent::Redo).unwrap();
        assert_eq!(redone, Applied::of(3, vec![Event::ParamSet { node: lane(&ctl), param: detune_index, value: 3.0 }]));
        assert_eq!((undo_label(&ctl), redo_label(&ctl)), (Some("set detune".to_string()), None));
        // Nothing more to redo, and what is refused is no revision.
        assert_eq!(ctl.apply(Intent::Redo), Err(IntentError::Invalid("there is nothing to redo".to_string())));
        assert_eq!(ctl.rev, 3);
        // The undo of the undo is the redo: it is the same step, which went back and forth, and not a new one.
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!(detune(&ctl).1, 0.0);
        assert!(ctl.apply(Intent::Undo).is_err(), "there was only the one step");
    }

    #[test]
    fn nothing_to_undo_and_nothing_to_redo_are_refused_in_words_and_cost_no_revision() {
        let (engine, mut ctl, _clock) = controller();
        assert_eq!(ctl.undo(), Err(IntentError::Invalid("there is nothing to undo".to_string())));
        assert_eq!(ctl.redo(), Err(IntentError::Invalid("there is nothing to redo".to_string())));
        assert_eq!((ctl.rev, ctl.dirty), (0, false));
        assert!(sent(&engine).is_empty());
    }

    #[test]
    fn a_change_after_an_undo_ends_what_could_be_redone() {
        let (_engine, mut ctl, clock) = controller();
        set_detune(&mut ctl, 1.0);
        clock.advance(Duration::from_secs(5));
        ctl.undo().unwrap();
        assert_eq!(redo_label(&ctl).as_deref(), Some("set detune"));
        let (reverb_damping, _) = param(&ctl, NodePath::Effect { inst: 0, index: 0 }, "damping");
        ctl.apply(Intent::SetParam { node: node(&ctl, NodePath::Effect { inst: 0, index: 0 }), param: reverb_damping, value: 0.1 }).unwrap();
        assert_eq!(redo_label(&ctl), None);
        assert_eq!(ctl.redo(), Err(IntentError::Invalid("there is nothing to redo".to_string())));
    }

    #[test]
    fn what_cannot_be_undone_yet_empties_the_history_so_that_undo_never_does_something_else() {
        let (_engine, mut ctl, clock) = controller();
        set_detune(&mut ctl, 1.0);
        clock.advance(Duration::from_secs(5));
        set_detune(&mut ctl, 2.0);
        ctl.undo().unwrap();
        assert_eq!((undo_label(&ctl).is_some(), redo_label(&ctl).is_some()), (true, true));
        // A cable is taken out: nothing puts that back yet, and what was before it cannot be undone past it.
        let edge = ctl.model.instruments[0].modulators[0].targets[0].edge;
        ctl.apply(Intent::RemoveEdge { edge }).unwrap();
        assert_eq!(ctl.history_state(), HistoryState { undo: None, redo: None });
        assert!(ctl.undo().is_err() && ctl.redo().is_err());
    }

    /// The session of `SESSION` with the built-in plugins in the catalog, so that an effect and an instrument can be added.
    fn controller_with_plugins() -> (StubEngine, Controller, Clock) {
        let (engine, mut ctl, clock) = controller();
        ctl.catalog.add(crate::plugin::builtin::enumerate_plugins());
        (engine, ctl, clock)
    }

    #[test]
    fn an_effect_that_was_added_is_taken_out_by_undo_and_put_back_by_redo_under_the_id_it_had() {
        let (engine, mut ctl, _clock) = controller_with_plugins();
        let lane0 = lane(&ctl);
        let before = ctl.model.instruments[0].effects.len();
        sent(&engine);
        ctl.apply(Intent::AddEffect { parent: lane0, plugin: "builtin:reverb".to_string() }).unwrap();
        let added = ctl.model.instruments[0].effects.last().unwrap().node;
        assert_eq!(ctl.last_made().nodes, [added]);
        assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl)), (Some("add Reverb"), None));
        assert_eq!(sent(&engine), ["InsertEffect inst=0 index=2 plugin=Reverb mix=0.500"]);

        let undone = ctl.undo().unwrap();
        assert!(undone.restructured);
        assert_eq!(ctl.model.instruments[0].effects.len(), before);
        assert_eq!(ctl.model.locate(added), None);
        assert_eq!(sent(&engine), ["RemoveEffect inst=0 index=2"]);
        assert_eq!((undo_label(&ctl), redo_label(&ctl).as_deref()), (None, Some("add Reverb")));

        let redone = ctl.redo().unwrap();
        assert!(redone.restructured);
        assert_eq!(ctl.model.instruments[0].effects.last().unwrap().node, added, "the effect that comes back has the id it had");
        assert_eq!(sent(&engine), ["InsertEffect inst=0 index=2 plugin=Reverb mix=0.500"]);
        assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl)), (Some("add Reverb"), None));
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn an_effect_on_a_groups_bus_is_added_and_taken_out_the_same_way() {
        let (engine, mut ctl, _clock) = controller_with_plugins();
        let g0 = node(&ctl, NodePath::Group(0));
        let before = ctl.model.groups[0].effects.len();
        sent(&engine);
        ctl.apply(Intent::AddEffect { parent: g0, plugin: "builtin:reverb".to_string() }).unwrap();
        let added = ctl.model.groups[0].effects.last().unwrap().node;
        assert_eq!(undo_label(&ctl).as_deref(), Some("add Reverb"));
        ctl.undo().unwrap();
        assert_eq!((ctl.model.groups[0].effects.len(), ctl.model.locate(added)), (before, None));
        ctl.redo().unwrap();
        assert_eq!(ctl.model.groups[0].effects.last().unwrap().node, added);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn what_was_done_to_an_effect_that_was_added_is_put_back_on_the_same_effect_whatever_came_and_went_in_between() {
        let (engine, mut ctl, clock) = controller_with_plugins();
        let lane0 = lane(&ctl);
        ctl.apply(Intent::AddEffect { parent: lane0, plugin: "builtin:reverb".to_string() }).unwrap();
        let added = ctl.model.instruments[0].effects.last().unwrap().node;
        ctl.apply(Intent::SetGain { node: added, gain: Gain::Mix, value: 0.25 }).unwrap();
        clock.advance(Duration::from_secs(5));
        let (damping, _) = param(&ctl, NodePath::Effect { inst: 0, index: 2 }, "damping");
        ctl.apply(Intent::SetParam { node: added, param: damping, value: 0.75 }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("set damping"));

        // All three go back, the effect with them, and all three come again, on the effect that comes back.
        for _ in 0..3 {
            ctl.undo().unwrap();
        }
        assert_eq!(ctl.model.locate(added), None);
        sent(&engine);
        for _ in 0..3 {
            ctl.redo().unwrap();
        }
        let effect = &ctl.model.instruments[0].effects[2];
        assert_eq!(effect.node, added);
        assert_eq!(effect.mix, 0.25);
        assert_eq!(effect.params.iter().find(|row| row.index == damping).unwrap().value, 0.75);
        assert_eq!(
            sent(&engine),
            [
                "InsertEffect inst=0 index=2 plugin=Reverb mix=0.500".to_string(),
                "SetMix inst=0 slot=3 value=0.250".to_string(),
                format!("SetParameter inst=0 slot=3 param={damping} value=0.750"),
            ]
        );
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_modulator_and_a_cable_that_were_added_are_taken_out_by_undo_and_come_back_under_their_ids_by_redo() {
        let (_engine, mut ctl, _clock) = controller_with_plugins();
        let (lane0, filter) = (lane(&ctl), node(&ctl, NodePath::Effect { inst: 0, index: 1 }));
        let (resonance, set) = param(&ctl, NodePath::Effect { inst: 0, index: 1 }, "resonance");
        ctl.apply(Intent::AddModulator { parent: lane0, kind: ModulatorKind::Lfo }).unwrap();
        let lfo = ctl.last_made().nodes[0];
        assert_eq!(undo_label(&ctl).as_deref(), Some("add an LFO"));
        ctl.apply(Intent::AddModEdge { modulator: lfo, to: Endpoint { node: filter, port: Port::Param { index: resonance } } }).unwrap();
        let cable = ctl.last_made().edges[0];
        assert_eq!(undo_label(&ctl).as_deref(), Some("draw a cable"));

        ctl.undo().unwrap();
        assert!(ctl.model.locate_edge(cable).is_none());
        assert_eq!(param(&ctl, NodePath::Effect { inst: 0, index: 1 }, "resonance").1, set, "what the cable drove has the value that was set");
        assert_eq!(undo_label(&ctl).as_deref(), Some("add an LFO"));
        ctl.undo().unwrap();
        assert!(ctl.model.locate(lfo).is_none());
        assert_eq!(undo_label(&ctl), None);

        ctl.redo().unwrap();
        assert!(ctl.model.locate(lfo).is_some(), "the modulator comes back under its id");
        ctl.redo().unwrap();
        let (at, index, target) = ctl.model.locate_edge(cable).expect("the cable comes back under its id");
        assert_eq!(ctl.model.modulators(at).unwrap()[index].node, lfo);
        assert_eq!(ctl.model.modulators(at).unwrap()[index].targets[target].edge, cable);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn an_envelope_and_a_midi_control_have_their_own_words() {
        let (_engine, mut ctl, clock) = controller_with_plugins();
        let lane0 = lane(&ctl);
        ctl.apply(Intent::AddModulator { parent: lane0, kind: ModulatorKind::Envelope }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("add an envelope"));
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::AddModulator { parent: lane0, kind: ModulatorKind::MidiLearn }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("add a MIDI control"));
    }

    #[test]
    fn an_instrument_that_was_added_is_a_lane_that_undo_takes_out_and_redo_puts_back_under_its_id_and_what_was_done_to_it_is_done_again() {
        let (engine, mut ctl, clock) = controller_with_plugins();
        let lanes = ctl.model.instruments.len();
        set_detune(&mut ctl, 1.0);
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::AddInstrument { plugin: "builtin:osc".to_string() }).unwrap();
        let added = ctl.model.instruments.last().unwrap().node;
        assert_eq!(undo_label(&ctl).as_deref(), Some("add Oscillator as an instrument"));
        ctl.apply(Intent::SetNodeUi { node: added, ui: Some(NodeUi { x: 5.0, y: 6.0 }) }).unwrap();
        sent(&engine);

        ctl.undo().unwrap();
        ctl.undo().unwrap();
        assert_eq!(ctl.model.instruments.len(), lanes);
        assert_eq!(ctl.model.locate(added), None);
        assert_eq!(sent(&engine), ["RemoveInstrument inst=2"]);
        assert_eq!(undo_label(&ctl).as_deref(), Some("set detune"), "what was done before the add is still to be undone");
        ctl.redo().unwrap();
        // (The step that moved the lane finds it under the id it had, or it would be refused and dropped.)
        ctl.redo().unwrap();
        assert_eq!(ctl.model.instruments.last().unwrap().node, added, "the lane comes back under its id");
        assert_eq!(ctl.model.ui.get(&added).copied(), Some(NodeUi { x: 5.0, y: 6.0 }));
        ctl.model.check_ids().unwrap();
        for _ in 0..3 {
            ctl.undo().unwrap();
        }
        assert_eq!(detune(&ctl).1, 0.0);
    }

    #[test]
    fn a_group_that_was_added_with_lanes_that_were_in_none_is_taken_out_by_undo_and_comes_back_with_them() {
        let (_engine, mut ctl, clock) = controller_with_plugins();
        let lane1 = node(&ctl, NodePath::Instrument(1));
        assert_eq!(ctl.model.instruments[1].group, None);
        ctl.apply(Intent::AddGroup { members: vec![lane1] }).unwrap();
        let group = ctl.model.groups.last().unwrap().node;
        assert_eq!(ctl.model.instruments[1].group, Some(2));
        assert_eq!(undo_label(&ctl).as_deref(), Some("add a group"));
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::RenameGroup { node: group, name: "Keys".to_string() }).unwrap();

        ctl.undo().unwrap();
        ctl.undo().unwrap();
        assert_eq!((ctl.model.groups.len(), ctl.model.instruments[1].group), (2, None), "the group is gone and its lane is in none");
        ctl.redo().unwrap();
        ctl.redo().unwrap();
        assert_eq!(ctl.model.groups.last().unwrap().node, group, "the group comes back under its id");
        assert_eq!(ctl.model.groups.last().unwrap().name.as_deref(), Some("Keys"));
        assert_eq!(ctl.model.instruments[1].group, Some(2));
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_session_that_is_built_by_adds_goes_back_through_each_of_the_structures_it_had_and_forward_through_them_again() {
        let (_engine, mut ctl, _clock) = controller_with_plugins();
        let (lane0, lane1) = (lane(&ctl), node(&ctl, NodePath::Instrument(1)));
        let (g0, g1) = (node(&ctl, NodePath::Group(0)), node(&ctl, NodePath::Group(1)));
        let (filter, bus) = (node(&ctl, NodePath::Effect { inst: 0, index: 1 }), node(&ctl, NodePath::GroupEffect { group: 0, index: 0 }));
        let (resonance, _) = param(&ctl, NodePath::Effect { inst: 0, index: 1 }, "resonance");
        let (cutoff, _) = param(&ctl, NodePath::GroupEffect { group: 0, index: 0 }, "cutoff");
        let (detune, _) = detune(&ctl);
        let to = |node: NodeId, index: u32| Endpoint { node, port: Port::Param { index } };
        // What each add made (the node, or the cable), as it is told, so that a later add can name it.
        let made = |ctl: &mut Controller, intent: Intent| -> Made {
            ctl.apply(intent).unwrap();
            ctl.last_made().clone()
        };

        let mut structures = vec![(ctl.model.graph(), ctl.model.telemetry_layout())];
        let mut record = |ctl: &Controller| structures.push((ctl.model.graph(), ctl.model.telemetry_layout()));
        let fx = made(&mut ctl, Intent::AddEffect { parent: lane0, plugin: "builtin:reverb".to_string() }).nodes[0];
        record(&ctl);
        made(&mut ctl, Intent::AddEffect { parent: g0, plugin: "builtin:filter".to_string() });
        record(&ctl);
        let lfo = made(&mut ctl, Intent::AddModulator { parent: lane0, kind: ModulatorKind::Lfo }).nodes[0];
        record(&ctl);
        made(&mut ctl, Intent::AddModEdge { modulator: lfo, to: to(filter, resonance) });
        record(&ctl);
        let envelope = made(&mut ctl, Intent::AddModulator { parent: lane0, kind: ModulatorKind::Envelope }).nodes[0];
        record(&ctl);
        made(&mut ctl, Intent::AddModEdge { modulator: envelope, to: Endpoint { node: lfo, port: Port::Rate } });
        record(&ctl);
        made(&mut ctl, Intent::AddModEdge { modulator: envelope, to: to(fx, 1) });
        record(&ctl);
        let group_lfo = made(&mut ctl, Intent::AddModulator { parent: g0, kind: ModulatorKind::Lfo }).nodes[0];
        record(&ctl);
        made(&mut ctl, Intent::AddModEdge { modulator: group_lfo, to: to(lane0, detune) });
        record(&ctl);
        made(&mut ctl, Intent::AddModEdge { modulator: group_lfo, to: to(bus, cutoff) });
        record(&ctl);
        let lane2 = made(&mut ctl, Intent::AddInstrument { plugin: "builtin:osc".to_string() }).nodes[0];
        record(&ctl);
        made(&mut ctl, Intent::AddGroup { members: vec![lane1, lane2] });
        record(&ctl);
        made(&mut ctl, Intent::SetLaneGroup { node: lane1, group: Some(g1) });
        record(&ctl);
        made(&mut ctl, Intent::AddModulator { parent: lane2, kind: ModulatorKind::MidiLearn });
        record(&ctl);
        let steps = structures.len() - 1;
        assert_eq!(steps, 14);

        // Back through each structure: the nodes, their places, their ids and the cables are what they were, and so are the entries of the layout of the telemetry.
        for step in (0..steps).rev() {
            ctl.undo().unwrap();
            assert_eq!(ctl.model.graph(), structures[step].0, "undone to the structure of {step} adds");
            assert_eq!(ctl.model.telemetry_layout().meters.len(), structures[step].1.meters.len(), "{step}");
            ctl.model.check_ids().unwrap();
        }
        assert_eq!(ctl.history_state().undo, None);
        // And forward again: the graph is what it was, whole, with every id the same (the slots of the telemetry are new ones).
        for (step, (graph, _)) in structures.iter().enumerate().skip(1) {
            ctl.redo().unwrap();
            assert_eq!(&ctl.model.graph(), graph, "done again to the structure of {step} adds");
            ctl.model.check_ids().unwrap();
        }
        assert_eq!(ctl.history_state().redo, None);
    }

    #[test]
    fn an_empty_group_is_added_and_taken_out_too() {
        let (_engine, mut ctl, _clock) = controller_with_plugins();
        ctl.apply(Intent::AddGroup { members: vec![] }).unwrap();
        assert_eq!(ctl.model.groups.len(), 3);
        ctl.undo().unwrap();
        assert_eq!(ctl.model.groups.len(), 2);
    }

    #[test]
    fn a_group_that_took_a_lane_out_of_another_group_cannot_be_taken_out_again_and_empties_the_history() {
        let (_engine, mut ctl, clock) = controller_with_plugins();
        let (lane0, lane1) = (lane(&ctl), node(&ctl, NodePath::Instrument(1)));
        assert_eq!((ctl.model.instruments[0].group, ctl.model.instruments[1].group), (Some(0), None));
        // One lane that is in a group is enough, whatever the others are and wherever it is in the list.
        for members in [vec![lane0], vec![lane0, lane1], vec![lane1, lane0]] {
            set_detune(&mut ctl, 1.0);
            clock.advance(Duration::from_secs(5));
            assert!(undo_label(&ctl).is_some());
            ctl.apply(Intent::AddGroup { members: members.clone() }).unwrap();
            assert_eq!(ctl.history_state(), HistoryState { undo: None, redo: None }, "{members:?}");
        }
    }

    #[test]
    fn a_lane_put_in_a_group_is_taken_out_by_undo_and_one_that_was_taken_out_is_put_back() {
        let (engine, mut ctl, clock) = controller_with_plugins();
        let (lane1, g1) = (node(&ctl, NodePath::Instrument(1)), node(&ctl, NodePath::Group(1)));
        sent(&engine);
        ctl.apply(Intent::SetLaneGroup { node: lane1, group: Some(g1) }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("put a lane in a group"));
        assert_eq!(ctl.model.instruments[1].group, Some(1));
        ctl.undo().unwrap();
        assert_eq!(ctl.model.instruments[1].group, None);
        ctl.redo().unwrap();
        assert_eq!(ctl.model.instruments[1].group, Some(1));

        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::SetLaneGroup { node: lane1, group: None }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("take a lane out of its group"));
        ctl.undo().unwrap();
        assert_eq!(ctl.model.instruments[1].group, Some(1));
        ctl.undo().unwrap();
        assert_eq!(ctl.model.instruments[1].group, None);
    }

    #[test]
    fn a_lane_that_moves_from_one_group_to_another_goes_back_to_the_first() {
        let (_engine, mut ctl, _clock) = controller_with_plugins();
        let (lane0, g1) = (lane(&ctl), node(&ctl, NodePath::Group(1)));
        assert_eq!(ctl.model.instruments[0].group, Some(0));
        ctl.apply(Intent::SetLaneGroup { node: lane0, group: Some(g1) }).unwrap();
        assert_eq!(ctl.model.instruments[0].group, Some(1));
        ctl.undo().unwrap();
        assert_eq!(ctl.model.instruments[0].group, Some(0));
    }

    #[test]
    fn a_lane_put_in_the_group_it_is_in_is_no_step_and_leaves_the_history_as_it_was() {
        let (_engine, mut ctl, _clock) = controller_with_plugins();
        let (lane0, g0) = (lane(&ctl), node(&ctl, NodePath::Group(0)));
        set_detune(&mut ctl, 1.0);
        let before = ctl.history_state();
        ctl.apply(Intent::SetLaneGroup { node: lane0, group: Some(g0) }).unwrap();
        assert_eq!(ctl.history_state(), before);
        // (Nor is a lane that is in none taken out of its group.)
        let lane1 = node(&ctl, NodePath::Instrument(1));
        ctl.apply(Intent::SetLaneGroup { node: lane1, group: None }).unwrap();
        assert_eq!(ctl.history_state(), before);
    }

    #[test]
    fn a_lane_that_leaves_a_group_that_has_cables_to_it_cannot_be_put_back_and_empties_the_history() {
        let (_engine, mut ctl, clock) = controller_over(load(testutil::LANES_IN_GROUPS).model);
        let member = ctl.model.instruments.iter().position(|lane| lane.group.is_some()).unwrap();
        assert!(!ctl.model.group_cables_into_lane(member).is_empty(), "the group drives the lane");
        let (lane_node, param_node) = (ctl.model.instruments[member].node, ctl.model.instruments[member].node);
        // Something to undo first.
        let (index, _) = param(&ctl, NodePath::Instrument(member), "detune");
        ctl.apply(Intent::SetParam { node: param_node, param: index, value: 3.0 }).unwrap();
        clock.advance(Duration::from_secs(5));
        assert!(undo_label(&ctl).is_some());
        ctl.apply(Intent::SetLaneGroup { node: lane_node, group: None }).unwrap();
        assert_eq!(ctl.history_state(), HistoryState { undo: None, redo: None }, "the cables that went are not brought back by a step");
    }

    #[test]
    fn what_is_refused_when_it_is_added_leaves_no_step_and_what_came_before_stays() {
        let (_engine, mut ctl, _clock) = controller_with_plugins();
        let lane0 = lane(&ctl);
        set_detune(&mut ctl, 1.0);
        let before = ctl.history_state();
        assert!(ctl.apply(Intent::AddEffect { parent: lane0, plugin: "builtin:nothing".to_string() }).is_err());
        assert!(ctl.apply(Intent::AddInstrument { plugin: "builtin:reverb".to_string() }).is_err());
        assert!(ctl.apply(Intent::AddGroup { members: vec![NodeId(9_999)] }).is_err());
        assert!(ctl.apply(Intent::SetLaneGroup { node: lane0, group: Some(NodeId(9_999)) }).is_err());
        assert_eq!(ctl.history_state(), before);
        // (And a refusal leaves no id of its own to put back: the next effect is the first one that was made.)
        ctl.apply(Intent::AddEffect { parent: lane0, plugin: "builtin:reverb".to_string() }).unwrap();
        assert_eq!(ctl.last_made().nodes.len(), 1);
    }

    #[test]
    fn a_step_that_adds_what_is_there_already_is_refused_and_dropped_and_nothing_has_two_ids() {
        let (_engine, mut ctl, _clock) = controller_with_plugins();
        let lane0 = lane(&ctl);
        ctl.apply(Intent::AddEffect { parent: lane0, plugin: "builtin:reverb".to_string() }).unwrap();
        ctl.undo().unwrap();
        // Two of the same step to redo (as nothing the program does makes it): the first puts the effect back, the second finds it there.
        let step = ctl.history.take_redo().unwrap();
        ctl.history.kept_undone(step.clone());
        ctl.history.kept_undone(step);
        ctl.redo().unwrap();
        let effects = ctl.model.instruments[0].effects.len();
        let refused = ctl.redo().unwrap_err();
        assert!(refused.to_string().contains("is there already"), "{refused}");
        assert_eq!(ctl.model.instruments[0].effects.len(), effects, "it was not added twice");
        ctl.model.check_ids().unwrap();
        // The step is dropped: it is not to be redone again, and what was done before it is still to be undone.
        assert_eq!(redo_label(&ctl), None);
    }

    #[test]
    fn a_step_that_draws_a_cable_that_is_there_already_is_refused_and_no_cable_has_two_ids() {
        let (_engine, mut ctl, _clock) = controller_with_plugins();
        let (lfo, filter) = (ctl.model.instruments[0].modulators[0].node, node(&ctl, NodePath::Effect { inst: 0, index: 1 }));
        let (resonance, _) = param(&ctl, NodePath::Effect { inst: 0, index: 1 }, "resonance");
        ctl.apply(Intent::AddModEdge { modulator: lfo, to: Endpoint { node: filter, port: Port::Param { index: resonance } } }).unwrap();
        ctl.undo().unwrap();
        let step = ctl.history.take_redo().unwrap();
        ctl.history.kept_undone(step.clone());
        ctl.history.kept_undone(step);
        ctl.redo().unwrap();
        let cables = ctl.model.instruments[0].modulators[0].targets.len();
        let refused = ctl.redo().unwrap_err();
        assert!(refused.to_string().contains("cable") && refused.to_string().contains("is there already"), "{refused}");
        assert_eq!(ctl.model.instruments[0].modulators[0].targets.len(), cables);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_step_that_adds_what_the_tui_has_taken_out_is_refused_with_the_reason_and_dropped() {
        let (_engine, mut ctl, clock) = controller_with_plugins();
        let lane0 = lane(&ctl);
        set_detune(&mut ctl, 1.0);
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::AddEffect { parent: lane0, plugin: "builtin:reverb".to_string() }).unwrap();
        let added = ctl.model.instruments[0].effects.last().unwrap().node;
        // Something takes it out by the controller's method and not by the intent, which leaves no step of the history.
        let index = ctl.model.instruments[0].effects.len() - 1;
        assert!(ctl.remove_effect(0, index));
        let refused = ctl.undo().unwrap_err();
        assert_eq!(refused, IntentError::NotFound(added));
        assert_eq!(undo_label(&ctl).as_deref(), Some("set detune"), "the step is dropped and the one before it is next");
    }

    #[test]
    fn an_id_that_an_undone_add_had_is_not_given_to_another_node() {
        let (_engine, mut ctl, _clock) = controller_with_plugins();
        let lane0 = lane(&ctl);
        ctl.apply(Intent::AddEffect { parent: lane0, plugin: "builtin:reverb".to_string() }).unwrap();
        let first = ctl.last_made().nodes[0];
        ctl.undo().unwrap();
        // Something else is added instead of redoing: it is a new node, and what could be redone is gone.
        ctl.apply(Intent::AddEffect { parent: lane0, plugin: "builtin:filter".to_string() }).unwrap();
        let second = ctl.last_made().nodes[0];
        assert_ne!(first, second);
        assert_eq!(redo_label(&ctl), None);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn every_intent_that_cannot_be_undone_yet_empties_the_history() {
        let (_engine, mut ctl, clock) = controller();
        ctl.catalog.add(crate::plugin::builtin::enumerate_plugins());
        let lfo = ctl.model.instruments[0].modulators[0].node;
        let edge = ctl.model.instruments[0].modulators[0].targets[0].edge;
        let intents = [
            ("take a cable out", Intent::RemoveEdge { edge }),
            ("take a node out", Intent::RemoveNode { node: lfo }),
            ("replace an instrument", Intent::ReplaceInstrument { node: node(&ctl, NodePath::Instrument(1)), plugin: "builtin:osc".to_string() }),
            // A group made of a lane that is in one: the cables of the group that has it go, and a step would not bring them back.
            ("add a group of a lane that is in another", Intent::AddGroup { members: vec![lane(&ctl)] }),
        ];
        for (what, intent) in intents {
            set_detune(&mut ctl, 1.0);
            assert!(undo_label(&ctl).is_some(), "{what}: there was something to undo");
            ctl.apply(intent).unwrap_or_else(|e| panic!("{what}: {e}"));
            assert_eq!(ctl.history_state(), HistoryState { undo: None, redo: None }, "{what}");
            clock.advance(Duration::from_secs(5));
        }
    }

    #[test]
    fn a_preset_that_is_loaded_empties_the_history_too() {
        let (_engine, mut ctl, _clock) = controller();
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        let preset = ctl.model.instruments[0].effects[0].presets[0].id.clone();
        set_detune(&mut ctl, 1.0);
        ctl.apply(Intent::LoadPreset { node: reverb, preset }).unwrap();
        assert_eq!(ctl.history_state(), HistoryState { undo: None, redo: None });
    }

    #[test]
    fn an_intent_that_is_refused_changes_nothing_in_the_history() {
        let (_engine, mut ctl, _clock) = controller();
        set_detune(&mut ctl, 1.0);
        let before = ctl.history_state();
        assert!(ctl.apply(Intent::SetParam { node: lane(&ctl), param: 9999, value: 1.0 }).is_err());
        assert!(ctl.apply(Intent::RemoveNode { node: NodeId(424242) }).is_err());
        assert!(ctl.apply(Intent::AddGroup { members: vec![NodeId(424242)] }).is_err());
        assert_eq!(ctl.history_state(), before);
    }

    #[test]
    fn saving_is_no_change_and_leaves_the_history_as_it_was() {
        let dir = std::env::temp_dir().join(format!("tang-history-save-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (handles, _engine) = testutil::stub_engine();
        let mut ctl = Controller::new(load(SESSION).model, handles, Some(dir.join("session.toml")));
        set_detune(&mut ctl, 1.0);
        let before = ctl.history_state();
        ctl.apply(Intent::Save).unwrap();
        assert_eq!(ctl.history_state(), before);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn putting_another_instrument_in_a_lane_empties_the_history_whoever_asks_for_it() {
        // (The TUI's `i` calls the controller's method and not the intent; the parameters of the new plugin are not the old one's.)
        let (_engine, mut ctl, _clock) = controller();
        ctl.catalog.add(crate::plugin::builtin::enumerate_plugins());
        set_detune(&mut ctl, 1.0);
        ctl.put_instrument(0, "builtin:osc").unwrap();
        assert_eq!(ctl.history_state(), HistoryState { undo: None, redo: None });
    }

    /// Run `f` and make one step of what it did, as a front-end does.
    fn as_one_step<T>(ctl: &mut Controller, label: &str, f: impl FnOnce(&mut Controller) -> T) -> T {
        let mark = ctl.history_mark();
        let answer = f(ctl);
        ctl.join_steps_since(mark, label);
        answer
    }

    /// A modulator for lane 0 and a cable from it to the filter's resonance: two intents, made one step by the caller.
    fn modulate_the_filter(ctl: &mut Controller) {
        use crate::app::graph::{Endpoint, Port};
        use crate::app::intent::ModulatorKind;
        let (lane0, filter) = (lane(ctl), node(ctl, NodePath::Effect { inst: 0, index: 1 }));
        let (resonance, _) = param(ctl, NodePath::Effect { inst: 0, index: 1 }, "resonance");
        ctl.apply(Intent::AddModulator { parent: lane0, kind: ModulatorKind::Lfo }).unwrap();
        let lfo = ctl.last_made().nodes[0];
        ctl.apply(Intent::AddModEdge { modulator: lfo, to: Endpoint { node: filter, port: Port::Param { index: resonance } } }).unwrap();
    }

    #[test]
    fn what_is_done_as_one_step_is_undone_and_redone_at_once_each_in_its_order_and_under_its_ids() {
        let (_engine, mut ctl, clock) = controller_with_plugins();
        set_detune(&mut ctl, 1.0);
        clock.advance(Duration::from_secs(5));
        let before = ctl.model.graph();
        as_one_step(&mut ctl, "modulate the resonance", modulate_the_filter);
        let after = ctl.model.graph();
        assert_ne!(before, after);
        assert_eq!(undo_label(&ctl).as_deref(), Some("modulate the resonance"));

        // One undo takes out the cable and then the modulator, and what came before is next.
        ctl.undo().unwrap();
        assert_eq!(ctl.model.graph(), before);
        assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl).as_deref()), (Some("set detune"), Some("modulate the resonance")));
        // One redo puts the modulator back and then the cable to it, under the ids they had.
        ctl.redo().unwrap();
        assert_eq!(ctl.model.graph(), after);
        ctl.model.check_ids().unwrap();
        ctl.undo().unwrap();
        ctl.undo().unwrap();
        assert_eq!(detune(&ctl).1, 0.0, "and what was done before it is put back after");
    }

    #[test]
    fn what_is_done_as_one_step_leaves_one_step_even_when_it_made_three_and_hands_back_what_it_returns() {
        let (_engine, mut ctl, _clock) = controller_with_plugins();
        let answer = as_one_step(&mut ctl, "three things", |ctl| {
            set_detune(ctl, 1.0);
            ctl.apply(Intent::SetLocked { locked: true }).unwrap();
            modulate_the_filter(ctl);
            42
        });
        assert_eq!(answer, 42);
        assert_eq!(undo_label(&ctl).as_deref(), Some("three things"));
        ctl.undo().unwrap();
        assert_eq!(undo_label(&ctl), None, "all of them were in it");
        assert_eq!(detune(&ctl).1, 0.0);
        assert_eq!(ctl.piano_filter.mode(), crate::piano_filter::PianoMode::Highlight);
        assert!(ctl.model.instruments[0].modulators.len() == 2);
    }

    #[test]
    fn what_is_done_as_one_step_that_made_one_step_or_none_is_left_as_it_is() {
        let (_engine, mut ctl, _clock) = controller_with_plugins();
        as_one_step(&mut ctl, "nothing", |_| {});
        assert_eq!(ctl.history_state(), HistoryState { undo: None, redo: None });
        as_one_step(&mut ctl, "one thing", |ctl| {
            set_detune(ctl, 1.0);
        });
        assert_eq!(undo_label(&ctl).as_deref(), Some("set detune"), "one step keeps its own words");
    }

    #[test]
    fn what_is_done_as_one_step_leaves_the_steps_before_it_alone_and_a_barrier_in_it_forgets_all() {
        let (_engine, mut ctl, clock) = controller_with_plugins();
        set_detune(&mut ctl, 1.0);
        clock.advance(Duration::from_secs(5));
        as_one_step(&mut ctl, "modulate, and take out", |ctl| {
            modulate_the_filter(ctl);
            let edge = ctl.model.instruments[0].modulators.last().unwrap().targets[0].edge;
            ctl.apply(Intent::RemoveEdge { edge }).unwrap();
        });
        assert_eq!(ctl.history_state(), HistoryState { undo: None, redo: None }, "the barrier forgot what came before it, and what was done as one step with it");
    }

    #[test]
    fn a_step_that_was_made_of_several_is_never_merged_with_what_is_done_after_it() {
        let (_engine, mut ctl, _clock) = controller();
        as_one_step(&mut ctl, "two things", |ctl| {
            set_detune(ctl, 1.0);
            ctl.apply(Intent::SetLocked { locked: true }).unwrap();
        });
        // The same knob again, soon after: a step of its own, and not part of the step that has the first change of it among others.
        set_detune(&mut ctl, 2.0);
        assert_eq!(undo_label(&ctl).as_deref(), Some("set detune"));
        ctl.undo().unwrap();
        assert_eq!(detune(&ctl).1, 1.0);
        assert_eq!(undo_label(&ctl).as_deref(), Some("two things"));
    }

    #[test]
    fn what_was_merged_into_an_older_step_stays_in_it_when_the_rest_is_made_one_step() {
        let (_engine, mut ctl, _clock) = controller_with_plugins();
        set_detune(&mut ctl, 1.0);
        as_one_step(&mut ctl, "modulate", |ctl| {
            set_detune(ctl, 2.0); // (the same knob a moment later: merged into the step that is there, which is not part of this one)
            modulate_the_filter(ctl);
        });
        assert_eq!(undo_label(&ctl).as_deref(), Some("modulate"));
        ctl.undo().unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("set detune"));
        assert_eq!(detune(&ctl).1, 2.0, "the second turn of the knob is in the step it was merged into");
    }

    #[test]
    fn emptying_a_lane_of_its_instrument_forgets_the_history_as_putting_another_in_does_and_a_lane_with_none_changes_nothing() {
        // (The TUI's `d` calls the controller's method: there is no intent for it, and what was set on the plugin is addressed by its parameters' places.)
        let (_engine, mut ctl, _clock) = controller();
        set_detune(&mut ctl, 1.0);
        assert!(ctl.clear_instrument(0));
        assert_eq!(ctl.history_state(), HistoryState { undo: None, redo: None });
        ctl.apply(Intent::SetLocked { locked: true }).unwrap();
        let before = ctl.history_state();
        assert!(!ctl.clear_instrument(0), "the lane has none now");
        assert_eq!(ctl.history_state(), before, "and the history is as it was");
    }

    #[test]
    fn a_step_whose_node_has_gone_is_refused_with_the_reason_and_is_dropped_and_the_ones_before_it_stay() {
        let (_engine, mut ctl, clock) = controller();
        set_detune(&mut ctl, 1.0);
        clock.advance(Duration::from_secs(5));
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        let (damping, _) = param(&ctl, NodePath::Effect { inst: 0, index: 0 }, "damping");
        ctl.apply(Intent::SetParam { node: reverb, param: damping, value: 0.9 }).unwrap();
        // The TUI takes the reverb out by the controller's method, which is no intent: the history is not told.
        assert!(ctl.remove_effect(0, 0));
        assert_eq!(undo_label(&ctl).as_deref(), Some("set damping"));
        assert_eq!(ctl.undo(), Err(IntentError::NotFound(reverb)));
        // The step is gone, and the one before it is what Undo does now.
        assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl)), (Some("set detune"), None));
        ctl.undo().unwrap();
        assert_eq!(detune(&ctl).1, 0.0);
    }

    #[test]
    fn a_step_of_several_intents_applies_them_in_order_each_a_revision_and_says_what_all_of_them_did() {
        let (engine, mut ctl, _clock) = controller();
        let (detune_index, _) = detune(&ctl);
        let group = node(&ctl, NodePath::Group(0));
        let mut step = Step::new(
            "do two things",
            Intent::SetParam { node: lane(&ctl), param: detune_index, value: 1.0 },
            Intent::SetParam { node: lane(&ctl), param: detune_index, value: 2.0 },
            None,
            Instant::now(),
        );
        step.undo.push(Intent::RenameGroup { node: group, name: "First".to_string() });
        step.redo.push(Intent::RenameGroup { node: group, name: "Second".to_string() });
        ctl.history.record(step);
        let undone = ctl.undo().unwrap();
        assert_eq!(undone.rev, 2, "a revision for each");
        assert_eq!(
            undone.events,
            [Event::ParamSet { node: lane(&ctl), param: detune_index, value: 1.0 }, Event::GroupRenamed { node: group, name: "First".to_string() }]
        );
        assert_eq!(sent(&engine).len(), 1);
        let redone = ctl.redo().unwrap();
        assert_eq!((redone.rev, redone.events.len()), (4, 2));
        assert_eq!((detune(&ctl).1, ctl.model.groups[0].name.as_deref()), (2.0, Some("Second")));
    }

    #[test]
    fn a_step_that_fails_half_way_is_dropped_and_what_was_done_stays() {
        let (_engine, mut ctl, _clock) = controller();
        let (detune_index, _) = detune(&ctl);
        let mut step = Step::new(
            "do two things",
            Intent::SetParam { node: lane(&ctl), param: detune_index, value: 1.0 },
            Intent::SetParam { node: lane(&ctl), param: detune_index, value: 2.0 },
            None,
            Instant::now(),
        );
        step.undo.push(Intent::RenameGroup { node: NodeId(424242), name: "Nobody".to_string() });
        ctl.history.record(step);
        assert_eq!(ctl.undo(), Err(IntentError::NotFound(NodeId(424242))));
        assert_eq!(detune(&ctl).1, 1.0, "the first of them was done");
        assert_eq!(ctl.history_state(), HistoryState { undo: None, redo: None });
    }

    #[test]
    fn what_undo_and_redo_do_is_told_to_the_subscribers_as_any_intent_is() {
        let (_engine, mut ctl, _clock) = controller();
        let heard = Arc::new(Mutex::new(Vec::<Applied>::new()));
        let sink = heard.clone();
        ctl.subscribe(Box::new(move |applied: &Applied| sink.lock().unwrap().push(applied.clone())));
        set_detune(&mut ctl, 1.0);
        ctl.undo().unwrap();
        ctl.redo().unwrap();
        let heard = heard.lock().unwrap();
        assert_eq!(heard.iter().map(|a| a.rev).collect::<Vec<_>>(), [1, 2, 3]);
        let values: Vec<f32> = heard.iter().map(|a| match a.events[..] {
            [Event::ParamSet { value, .. }] => value,
            _ => panic!("{a:?}"),
        }).collect();
        assert_eq!(values, [1.0, 0.0, 1.0]);
    }

    #[test]
    fn what_an_add_made_is_what_takes_it_out_and_it_has_to_be_the_one_thing_and_be_there() {
        let (_engine, ctl, _clock) = controller_with_plugins();
        let lane0 = lane(&ctl);
        let (lfo, edge) = (ctl.model.instruments[0].modulators[0].node, ctl.model.instruments[0].modulators[0].targets[0].edge);
        let add_effect = Intent::AddEffect { parent: lane0, plugin: "builtin:reverb".to_string() };
        let add_cable = Intent::AddModEdge { modulator: lfo, to: Endpoint { node: lane0, port: Port::Param { index: 1 } } };
        let nodes = |nodes: &[NodeId]| Made { nodes: nodes.to_vec(), edges: vec![] };
        // One node that is there is taken out; none, two, and one that is not there are not.
        assert_eq!(ctl.takes_out(&add_effect, &nodes(&[lfo])), Some(Intent::RemoveNode { node: lfo }));
        assert_eq!(ctl.takes_out(&add_effect, &nodes(&[])), None);
        assert_eq!(ctl.takes_out(&add_effect, &nodes(&[lfo, lane0])), None);
        assert_eq!(ctl.takes_out(&add_effect, &nodes(&[NodeId(9_999)])), None);
        // A cable is taken out by its id, and only a cable that is there, and only when it is all that was made.
        let cable = |edges: &[EdgeId]| Made { nodes: vec![], edges: edges.to_vec() };
        assert_eq!(ctl.takes_out(&add_cable, &cable(&[edge])), Some(Intent::RemoveEdge { edge }));
        assert_eq!(ctl.takes_out(&add_cable, &cable(&[])), None);
        assert_eq!(ctl.takes_out(&add_cable, &cable(&[edge, edge])), None);
        assert_eq!(ctl.takes_out(&add_cable, &cable(&[EdgeId(9_999)])), None);
        assert_eq!(ctl.takes_out(&add_cable, &Made { nodes: vec![lfo], edges: vec![edge] }), None);
        assert_eq!(ctl.takes_out(&add_cable, &nodes(&[lfo])), None);
        // Nodes are not what an add of a node makes when it has made a cable.
        assert_eq!(ctl.takes_out(&add_effect, &cable(&[edge])), None);
        assert_eq!(ctl.takes_out(&add_effect, &Made { nodes: vec![lfo], edges: vec![edge] }), None);
    }

    #[test]
    fn an_add_that_made_nothing_that_can_be_named_leaves_no_step_and_empties_the_history() {
        let (_engine, mut ctl, _clock) = controller_with_plugins();
        set_detune(&mut ctl, 1.0);
        assert!(undo_label(&ctl).is_some());
        ctl.made_last = Made::default();
        let intent = Intent::AddEffect { parent: lane(&ctl), plugin: "builtin:reverb".to_string() };
        ctl.keep_in_history(Plan::Added { label: "add Reverb".to_string(), intent, at: Instant::now() });
        assert_eq!(ctl.history_state(), HistoryState { undo: None, redo: None }, "what came before cannot be undone past what cannot be put back");
    }

    #[test]
    fn ids_that_a_step_did_not_use_are_not_kept_for_what_is_made_next() {
        let (_engine, mut ctl, _clock) = controller_with_plugins();
        let lane0 = lane(&ctl);
        ctl.apply(Intent::AddEffect { parent: lane0, plugin: "builtin:reverb".to_string() }).unwrap();
        let first = ctl.last_made().nodes.clone();
        ctl.undo().unwrap();
        // The step does something that makes nothing, though it says it makes a node (as a step that was made for something else would).
        let mut step = ctl.history.take_redo().unwrap();
        step.redo = vec![Intent::SetLocked { locked: true }];
        ctl.history.kept_undone(step);
        ctl.redo().unwrap();
        ctl.apply(Intent::AddEffect { parent: lane0, plugin: "builtin:filter".to_string() }).unwrap();
        assert_ne!(ctl.last_made().nodes, first, "the id that was left over is not given to the next node");
        ctl.model.check_ids().unwrap();
    }

    // -- what the web client's tests read --

    /// What a page that is open is to the controller: a subscriber, which a change of structure is told to with a snapshot of a revision of its
    /// own (`Controller::resync`), so that the revisions that come after it are the ones a page is told.
    struct Page;

    impl crate::app::subscribers::EventSink for Page {
        fn applied(&mut self, _: &Applied) {}
        fn resync(&mut self, _: &crate::app::snapshot::Snapshot) {}
    }

    /// A step of a script: an intent, or time going by (in milliseconds).
    enum Op {
        Do(Intent),
        Wait(u64),
    }

    /// The session of `SESSION` and what the controller says to scripts of intents and waits, as the web client's tests read it
    /// (`web/src/client/fixtures/history.json`): they pin the mock engine's history to the controller's, op by op. Each op is
    /// answered by the events it made (stamped, in the order they came), whether it changed the structure, what Undo and Redo would
    /// do after it, or the controller's words for a refusal. `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` rewrites it.
    #[test]
    fn the_web_clients_fixture_of_what_a_history_comes_to_is_current() {
        let (_engine, base, _clock) = controller();
        let (lane0, lane1) = (node(&base, NodePath::Instrument(0)), node(&base, NodePath::Instrument(1)));
        let (reverb, bus) = (node(&base, NodePath::Effect { inst: 0, index: 0 }), node(&base, NodePath::GroupEffect { group: 0, index: 0 }));
        let (g0, g1) = (node(&base, NodePath::Group(0)), node(&base, NodePath::Group(1)));
        let (lfo, envelope) = (base.model.instruments[0].modulators[0].node, base.model.instruments[0].modulators[1].node);
        let other_lfo = base.model.instruments[1].modulators[0].node;
        let edge = base.model.instruments[0].modulators[0].targets[0].edge;
        let other_edge = base.model.instruments[0].modulators[1].targets[0].edge;
        let (detune, _) = param(&base, NodePath::Instrument(0), "detune");
        let (volume, _) = param(&base, NodePath::Instrument(0), "volume");
        // The same index in three nodes: the knob of one is not the knob of another.
        let (filter_fx, filter_bus) = (node(&base, NodePath::Effect { inst: 0, index: 1 }), bus);
        let (resonance, _) = param(&base, NodePath::Effect { inst: 0, index: 1 }, "resonance");
        let (bus_resonance, _) = param(&base, NodePath::GroupEffect { group: 0, index: 0 }, "resonance");
        assert_eq!((detune, resonance, bus_resonance), (1, 1, 1));
        let ghost = NodeId(9_999);
        // The ids that are handed out go on from where the session's end: what an add makes first has the next one.
        let first_node = base.model.nodes().map(|(id, _)| id.0).max().unwrap() + 1;
        let cables = base.model.instruments.iter().flat_map(|lane| &lane.modulators).chain(base.model.groups.iter().flat_map(|group| &group.modulators));
        let first_edge = cables.flat_map(|modulator| &modulator.targets).map(|target| target.edge.0).max().unwrap() + 1;
        let fresh = |n: u32| NodeId(first_node + n);
        let fresh_edge = |n: u32| EdgeId(first_edge + n);

        let set = |param: u32, value: f32| Op::Do(Intent::SetParam { node: lane0, param, value });
        let gain = |node: NodeId, gain: Gain, value: f32| Op::Do(Intent::SetGain { node, gain, value });
        let field = |node: NodeId, field: ModulatorField, value: f32| Op::Do(Intent::SetModulatorField { node, field, value });
        let name = |node: NodeId, name: &str| Op::Do(Intent::RenameGroup { node, name: name.to_string() });
        let undo = || Op::Do(Intent::Undo);
        let redo = || Op::Do(Intent::Redo);
        // A change is a step of its own when a second has gone by since the last of its kind.
        let pause = || Op::Wait(5000);
        let n_of = |n: usize, op: fn() -> Op| (0..n).map(|_| op()).collect::<Vec<Op>>();
        let add_effect = |parent: NodeId, plugin: &str| Op::Do(Intent::AddEffect { parent, plugin: plugin.to_string() });
        let add_instrument = |plugin: &str| Op::Do(Intent::AddInstrument { plugin: plugin.to_string() });
        let add_modulator = |parent: NodeId, kind: ModulatorKind| Op::Do(Intent::AddModulator { parent, kind });
        let add_group = |members: Vec<NodeId>| Op::Do(Intent::AddGroup { members });
        let cable = |modulator: NodeId, node: NodeId, index: u32| Op::Do(Intent::AddModEdge { modulator, to: Endpoint { node, port: Port::Param { index } } });
        let put_in = |node: NodeId, group: Option<NodeId>| Op::Do(Intent::SetLaneGroup { node, group });

        let mut scripts: Vec<(&str, Vec<Op>)> = vec![
            ("a parameter that was set is put back and set again", vec![set(detune, 3.0), undo(), redo(), undo(), undo(), redo(), redo()]),
            (
                "a drag is one step that goes back to before it",
                vec![set(detune, 1.0), Op::Wait(16), set(detune, 2.0), Op::Wait(16), set(detune, 3.0), Op::Wait(16), set(detune, 4.0), undo(), undo(), redo()],
            ),
            ("a pause of more than a second makes a step of its own", vec![set(detune, 1.0), Op::Wait(1500), set(detune, 2.0), undo(), undo(), undo()]),
            ("a pause of exactly a second does not", vec![set(detune, 1.0), Op::Wait(1000), set(detune, 2.0), undo(), undo()]),
            ("another thing is a step of its own, and so is going back to the first", vec![set(detune, 1.0), set(volume, 0.5), set(detune, 2.0), undo(), undo(), undo()]),
            ("a change after an undo ends what could be redone", vec![set(detune, 1.0), pause(), undo(), set(volume, 0.5), redo(), undo(), undo()]),
            ("an intent that is refused changes nothing in the history", vec![set(detune, 1.0), set(9_999, 1.0), gain(g0, Gain::Mix, 0.5), field(lane0, ModulatorField::Rate, 1.0), Op::Do(Intent::RemoveNode { node: ghost }), undo()]),
            ("with nothing done there is nothing to undo or redo", vec![undo(), redo()]),
            ("what cannot be undone empties the history, what could be redone too", vec![set(detune, 1.0), pause(), set(volume, 0.5), undo(), Op::Do(Intent::RemoveEdge { edge }), undo(), redo()]),
            ("taking a node out is one of them", vec![set(detune, 1.0), Op::Do(Intent::RemoveNode { node: envelope }), undo()]),
        ];
        let mut gains = vec![gain(lane0, Gain::Volume, 2.5), pause(), gain(reverb, Gain::Mix, 0.125), pause(), gain(bus, Gain::Mix, 0.75), pause(), gain(g0, Gain::GroupVolume, 0.5), pause(), gain(g0, Gain::GroupPan, -0.5)];
        gains.extend(n_of(5, undo));
        gains.extend(n_of(5, redo));
        scripts.push(("the gains are put back each by its own words", gains));
        let mut modulators = vec![
            field(lfo, ModulatorField::Rate, 9.0),
            pause(),
            field(lfo, ModulatorField::Waveform, 1.0),
            pause(),
            field(lfo, ModulatorField::Waveform, 2.0),
            pause(),
            field(envelope, ModulatorField::Attack, 1.5),
            pause(),
            field(envelope, ModulatorField::Sustain, 0.9),
            pause(),
            Op::Do(Intent::SetEdgeDepth { edge, depth: -0.75 }),
            Op::Do(Intent::SetEdgeDepth { edge, depth: -0.5 }),
        ];
        modulators.extend(n_of(6, undo));
        modulators.extend(n_of(6, redo));
        scripts.push(("the settings of modulators and the depth of a cable are put back", modulators));
        let mut keys = vec![
            Op::Do(Intent::SetRange { node: lane0, range: Some(NoteRange { low: 36, high: 60 }) }),
            pause(),
            Op::Do(Intent::SetRange { node: lane0, range: None }),
            pause(),
            Op::Do(Intent::SetNodeUi { node: lane0, ui: Some(NodeUi { x: 10.0, y: 20.0 }) }),
            Op::Do(Intent::SetNodeUi { node: lane1, ui: Some(NodeUi { x: 30.0, y: 40.0 }) }),
            Op::Do(Intent::SetNodeUi { node: lane0, ui: None }),
            Op::Do(Intent::SetNodeUi { node: lane0, ui: Some(NodeUi { x: 50.0, y: 60.0 }) }),
            Op::Do(Intent::SetNodeUi { node: lane0, ui: Some(NodeUi { x: 70.0, y: 80.0 }) }),
            pause(),
            Op::Do(Intent::SetPinned { node: lane0, params: vec![detune] }),
        ];
        keys.extend(n_of(8, undo));
        keys.extend(n_of(8, redo));
        scripts.push(("the keys, the places of nodes and the knobs are put back", keys));
        let mut names = vec![name(g0, "Strings"), name(g1, "Keys"), name(g1, ""), name(g0, "  ")];
        names.extend(n_of(4, undo));
        names.extend(n_of(4, redo));
        scripts.push(("the name of a group is put back, and a group that had none has its default again", names));
        let mut piano = vec![
            Op::Do(Intent::SetScale { scale: "F# dorian".to_string() }),
            pause(),
            Op::Do(Intent::SetScale { scale: "not a scale".to_string() }),
            Op::Do(Intent::SetLocked { locked: true }),
            pause(),
            Op::Do(Intent::SetLocked { locked: false }),
        ];
        piano.extend(n_of(3, undo));
        piano.extend(n_of(3, redo));
        scripts.push(("the scale and the lock of the piano are put back", piano));
        scripts.push(("an effect that was moved is put back, and the structure is said to have changed", vec![Op::Do(Intent::MoveEffect { node: reverb, index: 1 }), undo(), redo(), undo()]));

        // Each thing is a step of its own, though no time goes by: what a change is of is the node it is for and what it is for.
        let mut knobs = vec![
            set(detune, 1.0),
            Op::Do(Intent::SetParam { node: filter_fx, param: resonance, value: 2.0 }),
            Op::Do(Intent::SetParam { node: filter_bus, param: bus_resonance, value: 3.0 }),
            Op::Do(Intent::SetParam { node: filter_bus, param: bus_resonance, value: 4.0 }),
        ];
        knobs.extend(n_of(3, undo));
        knobs.extend(n_of(3, redo));
        scripts.push(("the same knob of another node is a step of its own, though no time has gone by", knobs));
        let mut gains_together = vec![
            gain(lane0, Gain::Volume, 2.0),
            gain(lane1, Gain::Volume, 3.0),
            gain(reverb, Gain::Mix, 0.25),
            gain(bus, Gain::Mix, 0.75),
            gain(g0, Gain::GroupVolume, 0.5),
            gain(g0, Gain::GroupPan, 0.5),
            gain(g0, Gain::GroupPan, 0.25),
        ];
        gains_together.extend(n_of(6, undo));
        gains_together.extend(n_of(6, redo));
        scripts.push(("a gain is a step for each node and each kind of gain, and the same one again is the same step", gains_together));
        let mut settings = vec![
            field(lfo, ModulatorField::Rate, 5.0),
            field(other_lfo, ModulatorField::Rate, 6.0),
            field(lfo, ModulatorField::Waveform, 2.0),
            field(envelope, ModulatorField::Attack, 1.0),
            field(envelope, ModulatorField::Decay, 1.0),
            field(envelope, ModulatorField::Attack, 2.0),
            field(envelope, ModulatorField::Attack, 3.0),
        ];
        settings.extend(n_of(6, undo));
        settings.extend(n_of(6, redo));
        scripts.push(("a setting of a modulator is a step for each modulator and each setting, and the same one again is the same step", settings));
        let mut cables = vec![
            Op::Do(Intent::SetEdgeDepth { edge, depth: -0.25 }),
            Op::Do(Intent::SetEdgeDepth { edge: other_edge, depth: 0.75 }),
            Op::Do(Intent::SetEdgeDepth { edge, depth: 0.125 }),
            Op::Do(Intent::SetEdgeDepth { edge, depth: 0.5 }),
        ];
        cables.extend(n_of(3, undo));
        cables.extend(n_of(3, redo));
        scripts.push(("the depth of a cable is a step for each cable, and the same one again is the same step", cables));
        // What is added is taken out by undo and put back by redo under the id it had, so that what was done to it goes on naming it.
        scripts.push(("an effect that is added is taken out by undo and put back by redo, and so is what was done to it", vec![
            add_effect(lane0, "builtin:reverb"),
            gain(fresh(0), Gain::Mix, 0.25),
            undo(),
            undo(),
            undo(),
            redo(),
            redo(),
            redo(),
            undo(),
        ]));
        scripts.push(("an effect on the bus of a group is added and taken out the same way", vec![add_effect(g0, "builtin:filter"), undo(), redo(), undo()]));
        scripts.push((
            "a modulator and its cables are taken out one at a time and put back under their ids",
            vec![
                add_modulator(lane0, ModulatorKind::Lfo),
                cable(fresh(0), filter_fx, resonance),
                Op::Do(Intent::SetEdgeDepth { edge: fresh_edge(0), depth: -0.25 }),
                add_modulator(lane0, ModulatorKind::Envelope),
                Op::Do(Intent::AddModEdge { modulator: fresh(1), to: Endpoint { node: fresh(0), port: Port::Rate } }),
                undo(),
                undo(),
                undo(),
                undo(),
                undo(),
                redo(),
                redo(),
                redo(),
                redo(),
                redo(),
            ],
        ));
        scripts.push((
            "a MIDI control, and a modulator of a group with cables to its bus and to a member",
            vec![
                add_modulator(lane1, ModulatorKind::MidiLearn),
                add_modulator(g0, ModulatorKind::Lfo),
                cable(fresh(1), bus, bus_resonance),
                cable(fresh(1), lane0, detune),
                undo(),
                undo(),
                undo(),
                undo(),
                redo(),
                redo(),
                redo(),
                redo(),
            ],
        ));
        scripts.push((
            "an instrument is added and taken out as a lane, and put back with its id",
            vec![
                add_instrument("builtin:osc"),
                Op::Do(Intent::SetNodeUi { node: fresh(0), ui: Some(NodeUi { x: 5.0, y: 6.0 }) }),
                undo(),
                undo(),
                undo(),
                redo(),
                redo(),
                redo(),
            ],
        ));
        scripts.push((
            "a group is added with a lane that was in none, and taken out with the lane in none again",
            vec![add_group(vec![lane1]), name(fresh(0), "Keys"), undo(), undo(), redo(), redo(), add_group(vec![]), undo(), undo(), undo()],
        ));
        scripts.push((
            "a lane is put in a group, taken out of it, and moved from one to another, and each goes back",
            vec![
                put_in(lane1, Some(g1)),
                put_in(lane1, None),
                put_in(lane1, Some(g0)),
                put_in(lane0, Some(g1)),
                put_in(lane0, Some(g1)),
                undo(),
                undo(),
                undo(),
                undo(),
                undo(),
                redo(),
                redo(),
                redo(),
                redo(),
                redo(),
            ],
        ));
        scripts.push((
            "a lane that leaves a group whose modulator drives it cannot be put back, though one that is not driven can",
            vec![
                add_modulator(g0, ModulatorKind::Lfo),
                cable(fresh(0), lane0, detune),
                cable(fresh(0), bus, bus_resonance),
                put_in(lane1, Some(g0)),
                undo(),
                redo(),
                put_in(lane1, None),
                undo(),
                put_in(lane0, Some(g1)),
                undo(),
                redo(),
            ],
        ));
        scripts.push((
            "a lane that leaves a group that drives it for none is as much a barrier",
            vec![add_modulator(g0, ModulatorKind::Lfo), cable(fresh(0), lane0, detune), put_in(lane0, None), undo(), redo()],
        ));
        scripts.push((
            "a group made of a lane that is in another is a barrier, whatever else is in the list",
            vec![set(detune, 1.0), pause(), set(detune, 2.0), add_group(vec![lane1, lane0]), undo(), set(detune, 3.0), add_group(vec![lane0, lane1]), undo()],
        ));
        scripts.push((
            "an add that is refused leaves the history as it was",
            vec![
                set(detune, 1.0),
                add_effect(lane0, "builtin:nothing"),
                add_effect(ghost, "builtin:reverb"),
                add_effect(lfo, "builtin:reverb"),
                add_effect(lane0, "builtin:osc"),
                add_instrument("builtin:nothing"),
                add_instrument("builtin:reverb"),
                add_modulator(ghost, ModulatorKind::Lfo),
                add_modulator(reverb, ModulatorKind::Lfo),
                cable(ghost, lane0, detune),
                cable(lfo, ghost, detune),
                cable(lfo, lane0, 9_999),
                cable(lfo, lane0, detune),
                cable(lfo, lane1, detune),
                add_group(vec![ghost]),
                add_group(vec![lane1, reverb]),
                put_in(ghost, None),
                put_in(lane0, Some(ghost)),
                put_in(lane0, Some(lane1)),
                undo(),
                undo(),
            ],
        ));
        scripts.push((
            "what is added after an undo is a new node, and what could have been redone is gone",
            vec![add_effect(lane0, "builtin:reverb"), undo(), add_effect(lane0, "builtin:filter"), gain(fresh(1), Gain::Mix, 0.5), redo(), undo(), undo(), undo()],
        ));
        let preset = base.model.instruments[0].effects[0].presets[0].id.clone();
        let barriers = [
            // A group made of a lane that is in another: the lane leaves it, and the cables of the group to the lane would go.
            Intent::AddGroup { members: vec![lane0] },
            Intent::RemoveEdge { edge },
            Intent::RemoveNode { node: envelope },
            Intent::ReplaceInstrument { node: lane1, plugin: "builtin:osc".to_string() },
            Intent::LoadPreset { node: reverb, preset },
        ];
        let mut forgotten = Vec::new();
        for barrier in barriers {
            forgotten.extend([set(detune, 1.0), pause(), set(detune, 2.0), undo(), Op::Do(barrier), undo(), redo()]);
        }
        scripts.push(("each of what cannot be undone yet empties the history, and what could be redone", forgotten));

        let mut written = Vec::new();
        for (what, ops) in &scripts {
            let (_engine, mut ctl, clock) = controller();
            ctl.catalog.add(crate::plugin::builtin::enumerate_plugins());
            ctl.subscribe(Box::new(Page));
            let mut rows = Vec::new();
            for op in ops {
                match op {
                    Op::Wait(ms) => {
                        clock.advance(Duration::from_millis(*ms));
                        rows.push(serde_json::json!({ "wait": ms }));
                    }
                    Op::Do(intent) => {
                        let state = ctl.apply(intent.clone());
                        let history = ctl.history_state();
                        let history = serde_json::json!({ "undo": history.undo, "redo": history.redo });
                        rows.push(match state {
                            Err(error) => serde_json::json!({ "intent": wire(intent), "error": error.to_string(), "history": history }),
                            Ok(applied) => {
                                let mut row = serde_json::json!({ "intent": wire(intent), "events": wire(&applied.stamped()), "restructured": applied.restructured, "history": history });
                                // What the session comes to, when the structure changed: the nodes and the cables, with their ids, and the layout of the telemetry.
                                if applied.restructured {
                                    row["shape"] = testutil::shape_of(&ctl.snapshot());
                                }
                                row
                            }
                        });
                    }
                }
            }
            written.push(serde_json::json!({ "name": what, "rows": rows }));
        }
        let mut snapshot = wire(&base.snapshot());
        // The layout's fingerprint comes from the hasher, which the standard library does not promise to keep the same.
        snapshot["telemetry"]["rev"] = serde_json::json!(1);
        let fixture = serde_json::json!({ "snapshot": snapshot, "scripts": written });
        let fresh = serde_json::to_string_pretty(&fixture).unwrap() + "\n";

        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/src/client/fixtures/history.json");
        if std::env::var_os("TANG_UPDATE_TYPES").is_some() {
            std::fs::write(&path, &fresh).unwrap();
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            committed == fresh,
            "{} is not what a history comes to now: run `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` and commit the result",
            path.display()
        );
    }
}
