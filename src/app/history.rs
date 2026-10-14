//! The history of what a person did, and the way back (`docs/web-ui.md` §5, D35).
//!
//! Every change that can be undone leaves a `Step`: the intents that put it back and the intents that do it again, in the
//! words a button has for it. The controller keeps the steps in a `History`, and `Undo` and `Redo` are intents like the others:
//! they apply what a step says, so that the audio thread, the model, the TUI and every browser take them as any other change.
//!
//! A change that goes on (a knob dragged turns it at every frame) is one step, not a hundred: a change of what the last step
//! was of, soon enough after it, is merged into it, and the step goes back to before the first.

use std::time::{Duration, Instant};

use crate::app::controller::Controller;
use crate::app::graph::NoteRange;
use crate::app::intent::{Applied, Gain, Intent, IntentError, ModulatorField};
use crate::app::model::{EdgeId, ModSourceSlot, NodeId, NodePath, PluginSlot};
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
}

impl Step {
    /// A step of one change: `undo` puts back what it found, `redo` is the change.
    pub fn new(label: impl Into<String>, undo: Intent, redo: Intent, key: Option<Key>, at: Instant) -> Step {
        Step { label: label.into(), undo: vec![undo], redo: vec![redo], key, last: at }
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

    /// Put back what the last step that can be undone did. Refused if there is none, or if what it names is not there any more
    /// (a node taken out since: the step is dropped, and the one before it is the next to be undone).
    pub fn undo(&mut self) -> Result<Applied, IntentError> {
        let Some(step) = self.history.take_undo() else {
            return Err(IntentError::Invalid("there is nothing to undo".to_string()));
        };
        let applied = self.carry_out_all(&step.undo)?;
        self.history.kept_undone(step);
        Ok(applied)
    }

    /// Do again what the last step that was undone did.
    pub fn redo(&mut self) -> Result<Applied, IntentError> {
        let Some(step) = self.history.take_redo() else {
            return Err(IntentError::Invalid("there is nothing to redo".to_string()));
        };
        let applied = self.carry_out_all(&step.redo)?;
        self.history.kept_redone(step);
        Ok(applied)
    }

    /// Carry out `intents` one after the other, each a revision of its own, and say what all of them did.
    fn carry_out_all(&mut self, intents: &[Intent]) -> Result<Applied, IntentError> {
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
            // Nothing puts these back yet.
            Intent::LoadPreset { .. }
            | Intent::AddGroup { .. }
            | Intent::SetLaneGroup { .. }
            | Intent::RemoveEdge { .. }
            | Intent::AddModulator { .. }
            | Intent::AddModEdge { .. }
            | Intent::RemoveNode { .. }
            | Intent::AddEffect { .. }
            | Intent::AddInstrument { .. }
            | Intent::ReplaceInstrument { .. } => Plan::Barrier,
        }
    }

    /// An intent has been carried out: its step is kept, or the history is forgotten.
    pub(super) fn keep_in_history(&mut self, plan: Plan) {
        match plan {
            Plan::Step(step) => self.history.record(step),
            Plan::Barrier => {
                self.history.clear();
            }
            Plan::Nothing => {}
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
    use crate::app::graph::NoteRange;
    use crate::app::intent::{Applied, Event, IntentError};
    use crate::app::model::{ModSourceSlot, NodePath};
    use crate::app::testutil::{self, StubEngine, described, load};
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

    /// A group with a bus filter, a lane in it with a reverb and a filter, an LFO whose cable goes to its detune and an envelope, and a lane of its own.
    const SESSION: &str = r#"
[[group]]
name = "Pad"
[[group.effect]]
plugin = "builtin:filter"

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

[[instrument]]
plugin = "builtin:osc"
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
        let (handles, engine) = testutil::stub_engine();
        let mut ctl = Controller::new(load(SESSION).model, handles, None);
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
        use crate::app::intent::ModulatorKind;
        let (_engine, mut ctl, clock) = controller();
        set_detune(&mut ctl, 1.0);
        clock.advance(Duration::from_secs(5));
        set_detune(&mut ctl, 2.0);
        ctl.undo().unwrap();
        assert_eq!((undo_label(&ctl).is_some(), redo_label(&ctl).is_some()), (true, true));
        // A modulator is added: nothing puts that back yet, and what was before it cannot be undone past it.
        ctl.apply(Intent::AddModulator { parent: lane(&ctl), kind: ModulatorKind::Lfo }).unwrap();
        assert_eq!(ctl.history_state(), HistoryState { undo: None, redo: None });
        assert!(ctl.undo().is_err() && ctl.redo().is_err());
    }

    #[test]
    fn every_intent_that_cannot_be_undone_yet_empties_the_history() {
        use crate::app::intent::ModulatorKind;
        let (_engine, mut ctl, clock) = controller();
        ctl.catalog.add(crate::plugin::builtin::enumerate_plugins());
        let lfo = ctl.model.instruments[0].modulators[0].node;
        let edge = ctl.model.instruments[0].modulators[0].targets[0].edge;
        let cutoff_node = node(&ctl, NodePath::GroupEffect { group: 0, index: 0 });
        let intents = [
            ("add a group", Intent::AddGroup { members: vec![] }),
            ("put a lane in a group", Intent::SetLaneGroup { node: node(&ctl, NodePath::Instrument(1)), group: Some(node(&ctl, NodePath::Group(0))) }),
            ("take a cable out", Intent::RemoveEdge { edge }),
            ("add a modulator", Intent::AddModulator { parent: lane(&ctl), kind: ModulatorKind::Envelope }),
            ("take a node out", Intent::RemoveNode { node: lfo }),
            ("add an effect", Intent::AddEffect { parent: lane(&ctl), plugin: "builtin:filter".to_string() }),
            ("add an instrument", Intent::AddInstrument { plugin: "builtin:osc".to_string() }),
            ("replace an instrument", Intent::ReplaceInstrument { node: node(&ctl, NodePath::Instrument(1)), plugin: "builtin:osc".to_string() }),
        ];
        let _ = cutoff_node;
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
}
