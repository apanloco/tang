//! Pattern boxes in the controller (P4h of `docs/patch-graph.md` §11.6): putting one in the session and taking one out, what the audio thread is told of one (the box with its notes, its settings, its transport) and what it
//! tells of one (what it recorded, what it is doing).
//!
//! A box is a source of notes with a number of its own, its **slot** (`Source::pattern(slot)`): the lowest of the engine's sixteen that was free when the box was made, kept for as long as the box is in the session, so that what
//! a lane has sounding from a box is never another box's. The lanes that hear it, and the sources that it records, are worked out from the cables of notes (`SessionModel::lane_sources`, `box_inputs`) and told to the audio thread
//! when they are not what it was told last (`sync_lane_sources`, which the end of every intent that changes the structure calls). The notes of the model are what `patternbox::sanitize` says, which is what the audio thread's box keeps, so
//! that the two never differ; the audio thread says what a pass of recording made (`BoxNotification::Recorded`: the model's notes become those, the session is unsaved, and a step of the history is kept) and what the box is doing
//! (`Status`: the model says it, and nothing else changes), and nothing is guessed in between.

use crate::app::apply::{Done, invalid, pattern_title};
use crate::app::controller::Controller;
use crate::app::graph::PatternNote;
use crate::app::history::{Key, Step};
use crate::app::intent::{Applied, Intent, IntentError, PatternBoxAction, PatternBoxField};
use crate::app::journal::Inverse;
use crate::app::model::{NodeId, NodePath, PatternBox, SourceKind};
use crate::patternbox::{self, BoxNote, BoxNotification, BoxPlan, BoxPlayer, Settings, Status};
use crate::plugin::chain::{BoxAction, GraphCommand};
use crate::source::{MAX_BOXES, Source, SourceSet};

impl Controller {
    /// Put a pattern box at the end of the session's boxes, under a node of its own, and tell the audio thread of it (`Intent::AddPattern`): empty, 4 beats, called "Pattern N". Refused, with nothing changed and no id used,
    /// when the session has `MAX_BOXES`.
    pub(super) fn add_pattern(&mut self) -> Result<NodeId, IntentError> {
        let Some(slot) = self.model.free_pattern_slot() else {
            return Err(invalid(format!("a session has at most {MAX_BOXES} pattern boxes")));
        };
        let node = self.model.alloc_node();
        let name = self.model.next_pattern_name();
        self.model.patterns.push(PatternBox::new(node, slot, name));
        self.send_box(self.model.patterns.len() - 1);
        Ok(node)
    }

    /// Take the pattern box at `index` out of the session, after the cables of notes that start or end at it (each written down, as a lane's are), and tell the audio thread: the lanes that heard it let go of what it had sounding. The
    /// lanes that it fed hear what their other cables bring, or every source if they have none (`sync_lane_sources`, which `conclude` calls). False, changing nothing, if there is no such box.
    pub(super) fn remove_pattern(&mut self, index: usize) -> bool {
        let Some(node) = self.model.patterns.get(index).map(|pattern| pattern.node) else { return false };
        self.forget_the_history_unless_an_intent_asks();
        self.take_out_event_cables_at(&[node]);
        let pattern = self.model.patterns.remove(index);
        let _ = self.cmd_tx.send(GraphCommand::RemoveBox { node });
        self.sent_boxes.retain(|(told, _)| *told != node);
        self.sent_plans.retain(|(told, _)| *told != node);
        self.took_out(Inverse::Pattern { index, pattern });
        true
    }

    /// Put a pattern box back among the boxes at `index`, as what undo takes out comes back: under the node it had, with its notes and its settings, at the slot it had if that is free and the lowest that is if it is not (a box that
    /// was made since is not undone first only when the session was changed without the history). A session that has `MAX_BOXES` already is not given another: the box is left out, and logged.
    pub(super) fn put_back_pattern(&mut self, index: usize, mut pattern: PatternBox) {
        if self.model.patterns.iter().any(|there| there.slot == pattern.slot) {
            let Some(slot) = self.model.free_pattern_slot() else {
                log::warn!("{:?} was not put back: the session has {MAX_BOXES} pattern boxes", pattern.node);
                return;
            };
            pattern.slot = slot;
        }
        // (What the audio thread says of it is what it says of a box that is new to it.)
        pattern.status = Status::Stopped;
        let place = self.model.insert_pattern_at(index, pattern);
        self.send_box(place);
    }

    /// The sources that a source of notes of the model stands for: the keys are the keys' source, and a device is the open ports that its name stands for (`DeviceWatcher::sources_of`), none while it is not plugged in.
    pub(super) fn sources_of_kind(&self, kind: &SourceKind) -> SourceSet {
        match kind {
            SourceKind::Keys => SourceSet::NONE.with(Source::KEYS),
            SourceKind::Device(name) => self.devices.as_ref().map_or(SourceSet::NONE, |devices| devices.sources_of(name)),
        }
    }

    /// What the audio thread's box is to be set to for the box at `index`, as the model and the open devices say.
    pub(crate) fn box_settings(&self, index: usize) -> Settings {
        let pattern = &self.model.patterns[index];
        Settings {
            looping: pattern.looping,
            clock: pattern.sync,
            overdub: pattern.overdub,
            click: pattern.click,
            inputs: self.model.box_inputs(index, |kind| self.sources_of_kind(kind)),
        }
    }

    /// Tell the audio thread of the box at `index`, which is new to it: the box, with its notes and its settings.
    fn send_box(&mut self, index: usize) {
        let settings = self.box_settings(index);
        let pattern = &self.model.patterns[index];
        let mut player = BoxPlayer::new(pattern.source());
        player.set_plan(BoxPlan::new(pattern.length_beats, &pattern.notes));
        player.set_settings(settings);
        let node = pattern.node;
        let _ = self.cmd_tx.send(GraphCommand::AddBox { node, player: Box::new(player) });
        self.sent_boxes.retain(|(told, _)| *told != node);
        self.sent_boxes.push((node, settings));
    }

    /// How many plans the audio thread has been given for the box `node`: the one that it was made with (a box is made with one, as the loader's are: a box that has no count has had that one), and each that `send_box_plan` has sent
    /// since. The count of a box that goes is taken out with it (`remove_pattern`), so that a box that comes back, which is a new box to the audio thread, has the one again.
    fn plans_sent(&self, node: NodeId) -> u32 {
        self.sent_plans.iter().find(|(told, _)| *told == node).map_or(1, |(_, plans)| *plans)
    }

    /// Tell the audio thread the notes and the length of the box at `index`: it goes on from where it is.
    fn send_box_plan(&mut self, index: usize) {
        let pattern = &self.model.patterns[index];
        let (node, plan) = (pattern.node, BoxPlan::new(pattern.length_beats, &pattern.notes));
        let _ = self.cmd_tx.send(GraphCommand::SetBoxPlan { node, plan: Box::new(plan) });
        let plans = self.plans_sent(node) + 1;
        self.sent_plans.retain(|(told, _)| *told != node);
        self.sent_plans.push((node, plans));
    }

    /// Tell the audio thread what each box is set to where it is not what it was told last (`sent_boxes`): the settings are told whole, with the sources the box records, which the cables and the devices that are open say. A
    /// box that was not told anything (one that a loaded session has) is told.
    pub(super) fn sync_box_settings(&mut self) {
        let mut now = Vec::with_capacity(self.model.patterns.len());
        for index in 0..self.model.patterns.len() {
            let node = self.model.patterns[index].node;
            let settings = self.box_settings(index);
            let told = self.sent_boxes.iter().find(|(told, _)| *told == node).map(|(_, settings)| *settings);
            if told != Some(settings) {
                let _ = self.cmd_tx.send(GraphCommand::SetBoxSettings { node, settings });
            }
            now.push((node, settings));
        }
        self.sent_boxes = now;
    }

    /// The place of the pattern box whose node is `node`. Refused, in words, for a node that is another kind, and not found for one that is not there.
    fn pattern_index(&self, node: NodeId) -> Result<usize, IntentError> {
        match self.model.locate(node) {
            Some(NodePath::Pattern(index)) => Ok(index),
            Some(_) => Err(invalid("that node is not a pattern box")),
            None => Err(IntentError::NotFound(node)),
        }
    }

    /// `Intent::AddPattern`.
    pub(super) fn add_pattern_intent(&mut self) -> Result<Done, IntentError> {
        self.add_pattern()?;
        Ok(Done::Restructured)
    }

    /// `Intent::SetPatternBoxField`: the setting is the model's, and what the audio thread has of it is told (the length as a new plan, the others as the box's settings, which the end of the intent sends).
    pub(super) fn set_pattern_box_field_intent(&mut self, node: NodeId, field: PatternBoxField) -> Result<Done, IntentError> {
        let index = self.pattern_index(node)?;
        match field {
            PatternBoxField::Name { name } => self.model.patterns[index].name = pattern_title(&name)?,
            PatternBoxField::LengthBeats { beats } => {
                if !beats.is_finite() {
                    return Err(invalid(format!("{beats} is not a length in beats")));
                }
                let length = BoxPlan::clamp_length(beats);
                // (A length that the box has is not told again: the audio thread lets go of what a new plan has sounding, which a drag that ends where it began should not do.)
                if self.model.patterns[index].length_beats != length {
                    self.model.patterns[index].length_beats = length;
                    self.send_box_plan(index);
                }
            }
            PatternBoxField::Looping { on } => self.model.patterns[index].looping = on,
            PatternBoxField::Sync { sync } => self.model.patterns[index].sync = sync.into(),
            PatternBoxField::Overdub { on } => self.model.patterns[index].overdub = on,
            PatternBoxField::Click { on } => self.model.patterns[index].click = on,
        }
        Ok(Done::Restructured)
    }

    /// `Intent::ClearPatternBox`.
    pub(super) fn clear_pattern_box_intent(&mut self, node: NodeId) -> Result<Done, IntentError> {
        let index = self.pattern_index(node)?;
        if !self.model.patterns[index].notes.is_empty() {
            self.model.patterns[index].notes.clear();
            self.send_box_plan(index);
        }
        Ok(Done::Restructured)
    }

    /// `Intent::SetPatternBoxNotes`: the notes as a box holds them.
    pub(super) fn set_pattern_box_notes_intent(&mut self, node: NodeId, notes: Vec<PatternNote>) -> Result<Done, IntentError> {
        let index = self.pattern_index(node)?;
        let notes = patternbox::sanitize(&notes.into_iter().map(BoxNote::from).collect::<Vec<_>>());
        // (Notes that the box has are not told again, for the reason that a length is not.)
        if self.model.patterns[index].notes != notes {
            self.model.patterns[index].notes = notes;
            self.send_box_plan(index);
        }
        Ok(Done::Restructured)
    }

    /// `Intent::PatternBoxTransport`: the audio thread is given the box as the model has it (its settings, with the sources it records) and then told to go; what it does comes back as its status and notes. Nothing of the model changes, so
    /// there is no step of the history, the session is not marked, and the clients are told only that a revision is used.
    pub(super) fn pattern_box_transport_intent(&mut self, node: NodeId, action: PatternBoxAction) -> Result<Applied, IntentError> {
        self.pattern_index(node)?;
        self.sync_box_settings();
        let action = match action {
            PatternBoxAction::Record => BoxAction::Record,
            PatternBoxAction::FinishRecording => BoxAction::FinishRecording,
            PatternBoxAction::Play => BoxAction::Play,
            PatternBoxAction::Stop => BoxAction::Stop,
        };
        let _ = self.cmd_tx.send(GraphCommand::BoxTransport { node, action });
        self.rev += 1;
        let applied = Applied::of(self.rev, Vec::new());
        self.publish(&applied);
        Ok(applied)
    }

    /// Fold what the audio thread has said of the boxes into the model (`poll`): what a box is doing, and the notes of a pass that is over. True if the model changed.
    pub(super) fn fold_in_box_notifications(&mut self) -> bool {
        let mut changed = false;
        while let Ok(notification) = self.box_rx.try_recv() {
            changed |= match notification {
                BoxNotification::Status { node, status } => self.box_status(node, status),
                BoxNotification::Recorded { node, notes, plans } => self.box_recorded(node, notes, plans),
            };
        }
        changed
    }

    /// What the audio thread says the box `node` is doing. That is all that changes (the session is not marked); one for a box that is gone changes nothing.
    fn box_status(&mut self, node: NodeId, status: Status) -> bool {
        match self.model.pattern_mut(node) {
            Some(pattern) if pattern.status != status => {
                pattern.status = status;
                true
            }
            _ => false,
        }
    }

    /// A pass of recording is over: the notes that the box has are the model's, the session is unsaved and the history has a step ("record into Pattern 1") that undo takes back to the notes it had. One for a box that is gone, or
    /// that has these notes already, changes nothing. Nor does one that was made on a plan that is not the last that was sent (`plans`: how many the box had been given when the pass ended): the notes were changed since, and the
    /// box takes the change at the start of its next block, so the model's notes are the ones that the box is left with, and the notes of that pass are lost to it.
    fn box_recorded(&mut self, node: NodeId, notes: Vec<BoxNote>, plans: u32) -> bool {
        if plans != self.plans_sent(node) {
            return false;
        }
        let notes = patternbox::sanitize(&notes);
        let Some(pattern) = self.model.pattern_mut(node) else { return false };
        if pattern.notes == notes {
            return false;
        }
        let before = std::mem::replace(&mut pattern.notes, notes.clone());
        let name = pattern.name.clone();
        self.dirty = true;
        let wire = |notes: &[BoxNote]| notes.iter().copied().map(PatternNote::from).collect::<Vec<_>>();
        let step = Step::new(format!("record into {name}"), Intent::SetPatternBoxNotes { node, notes: wire(&before) }, Intent::SetPatternBoxNotes { node, notes: wire(&notes) }, None::<Key>, (self.clock)());
        self.history.record(step);
        true
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use super::*;
    use crate::app::graph::{Endpoint, NodeKind, PatternStatus, PatternSync, Port, SourceInfo};
    use crate::app::history::HistoryState;
    use crate::app::model::{EdgeId, EventCable};
    use crate::app::testutil::{self, PATTERNS, Ports, StubEngine, load};
    use crate::patternbox::Clock;

    const TWO_LANES: &str = "[[instrument]]\nplugin = \"builtin:osc\"\n\n[[instrument]]\nplugin = \"builtin:osc\"\n";

    /// A clock that a test turns.
    #[derive(Clone)]
    struct TestClock(Arc<Mutex<Instant>>);

    impl TestClock {
        fn advance(&self, by: Duration) {
            *self.0.lock().unwrap() += by;
        }
    }

    /// A controller over `session` and a stub engine that has room for the commands of a long test, with a clock that the test turns.
    fn controller(session: &str) -> (StubEngine, Controller, TestClock) {
        let (handles, engine) = testutil::stub_engine_of(4_096);
        let mut ctl = Controller::new(load(session).model, handles, None);
        ctl.catalog.add(crate::plugin::builtin::enumerate_plugins());
        let clock = TestClock(Arc::new(Mutex::new(Instant::now())));
        let shared = clock.clone();
        ctl.clock = Box::new(move || *shared.0.lock().unwrap());
        (engine, ctl, clock)
    }

    /// What the controller has told the audio thread of the boxes and of what the lanes hear since the last look, a line for each command, in order.
    fn told(engine: &StubEngine) -> Vec<String> {
        const WORDS: [&str; 7] = ["AddBox", "RemoveBox", "SetBoxPlan", "SetBoxSettings", "BoxTransport", "SetLaneSources", "ReleaseSource"];
        engine.drain().0.into_iter().filter(|command| WORDS.iter().any(|word| command.starts_with(word))).collect()
    }

    fn note(start: f32, length: f32, note: u8, velocity: u8) -> BoxNote {
        BoxNote { start, length, note, velocity }
    }

    fn out(node: NodeId) -> Endpoint {
        Endpoint { node, port: Port::EventsOut }
    }

    fn into(node: NodeId) -> Endpoint {
        Endpoint { node, port: Port::EventsIn }
    }

    fn draw(ctl: &mut Controller, from: NodeId, to: NodeId) -> Result<Applied, IntentError> {
        ctl.apply(Intent::AddCable { from: out(from), to: into(to), gain: None })
    }

    fn box_node(ctl: &Controller, index: usize) -> NodeId {
        ctl.model.patterns[index].node
    }

    fn lane_node(ctl: &Controller, index: usize) -> NodeId {
        ctl.model.instruments[index].node
    }

    fn undo_label(ctl: &Controller) -> Option<String> {
        ctl.history_state().undo
    }

    /// What `describe` says of the settings of a box (the sources it records: `all` or the numbers).
    fn settings_tail(looping: bool, clock: &str, overdub: bool, click: bool, inputs: &str) -> String {
        format!("looping={looping} clock={clock} overdub={overdub} click={click} inputs={inputs}")
    }

    /// What `describe` says of the command that sets a box.
    fn settings_line(node: NodeId, looping: bool, clock: &str, overdub: bool, click: bool, inputs: &str) -> String {
        format!("SetBoxSettings node={} {}", node.0, settings_tail(looping, clock, overdub, click, inputs))
    }

    /// The settings of a box that is new.
    fn new_box_settings() -> String {
        settings_tail(true, "own", false, true, "all")
    }

    /// What `describe` says of the command that puts a box in the graph: its source, the length of its loop, how many notes it has and what it is set to.
    fn add_line(node: NodeId, source: usize, length: &str, notes: usize, tail: &str) -> String {
        format!("AddBox node={} source={source} length={length} notes={notes} {tail}", node.0)
    }

    // -- a session that has boxes --

    #[test]
    fn a_loaded_session_has_its_boxes_at_the_slots_in_the_order_of_the_file_and_the_controller_tells_the_audio_thread_what_each_records_and_what_each_lane_hears() {
        let (engine, ctl, _) = controller(PATTERNS);
        let (bass, plain) = (&ctl.model.patterns[0], &ctl.model.patterns[1]);
        assert_eq!((bass.name.as_str(), bass.slot, bass.length_beats, bass.looping, bass.sync, bass.overdub, bass.click), ("Bass", 0, 8.0, true, Clock::Own, true, true));
        assert_eq!(bass.notes, [note(0.0, 1.0, 36, 100), note(2.0, 0.5, 43, 90)], "C2 and G2");
        assert_eq!((plain.name.as_str(), plain.slot, plain.length_beats, plain.notes.len(), plain.overdub), ("Pattern", 1, 4.0, 0, false), "a box that the file says nothing of is a new box, and has the name of one that is not counted");
        assert_eq!((bass.status, plain.status), (Status::Stopped, Status::Stopped));
        // The lanes hear the box that a cable from it ends at, and the first box records the keys, which its cable comes from, and the second every input.
        assert_eq!(
            told(&engine),
            [
                "SetLaneSources inst=0 sources=[48]".to_string(),
                "SetLaneSources inst=1 sources=[49]".to_string(),
                settings_line(bass.node, true, "own", true, true, "[0]"),
                settings_line(plain.node, true, "own", false, true, "all"),
            ]
        );
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_session_with_no_box_is_told_nothing_of_boxes_whatever_is_done_to_it() {
        let (engine, mut ctl, _) = controller(TWO_LANES);
        assert_eq!(told(&engine), Vec::<String>::new());
        ctl.apply(Intent::AddSource { source: SourceInfo::Keys }).unwrap();
        let (keys, lane) = (ctl.model.sources[0].node, lane_node(&ctl, 0));
        draw(&mut ctl, keys, lane).unwrap();
        ctl.apply(Intent::Undo).unwrap();
        assert!(told(&engine).iter().all(|command| !command.contains("Box")), "no box is told of");
    }

    // -- adding a box --

    #[test]
    fn a_box_is_added_at_the_end_with_the_lowest_free_slot_and_name_and_the_audio_thread_gets_it_with_nothing_in_it() {
        let (engine, mut ctl, _) = controller(TWO_LANES);
        assert_eq!(ctl.apply(Intent::AddPattern), Ok(Applied::restructured(1)));
        let first = ctl.model.patterns[0].clone();
        assert_eq!(
            (first.name.as_str(), first.slot, first.length_beats, first.looping, first.sync, first.overdub, first.click, first.notes.len(), first.status),
            ("Pattern 1", 0, 4.0, true, Clock::Own, false, true, 0, Status::Stopped)
        );
        assert_eq!(ctl.model.locate(first.node), Some(NodePath::Pattern(0)));
        assert!(ctl.dirty, "the box is in the file when the session is saved");
        assert_eq!(told(&engine), [add_line(first.node, 48, "4", 0, &new_box_settings())], "and no lane hears it until a cable says so");
        assert_eq!(ctl.apply(Intent::AddPattern), Ok(Applied::restructured(2)));
        let second = ctl.model.patterns[1].clone();
        assert_eq!((second.name.as_str(), second.slot), ("Pattern 2", 1));
        assert_eq!(told(&engine), [add_line(second.node, 49, "4", 0, &new_box_settings())]);
        assert_ne!(first.node, second.node);
        ctl.model.check_ids().unwrap();
        // A box that is gone leaves its slot and its name to the next: the lowest that is free.
        ctl.apply(Intent::RemoveNode { node: first.node }).unwrap();
        told(&engine);
        ctl.apply(Intent::AddPattern).unwrap();
        let third = &ctl.model.patterns[1];
        assert_eq!((third.name.as_str(), third.slot), ("Pattern 1", 0), "the slot and the name of the box that went");
        assert_eq!(told(&engine), [add_line(third.node, 48, "4", 0, &new_box_settings())]);
    }

    #[test]
    fn a_name_that_a_box_has_is_not_given_to_the_next_even_when_it_is_not_the_lowest_number() {
        let (_engine, mut ctl, _) = controller(TWO_LANES);
        ctl.apply(Intent::AddPattern).unwrap();
        ctl.apply(Intent::AddPattern).unwrap();
        let first = box_node(&ctl, 0);
        ctl.apply(Intent::SetPatternBoxField { node: first, field: PatternBoxField::Name { name: "Pattern 3".to_string() } }).unwrap();
        ctl.apply(Intent::AddPattern).unwrap();
        let names: Vec<&str> = ctl.model.patterns.iter().map(|pattern| pattern.name.as_str()).collect();
        assert_eq!(names, ["Pattern 3", "Pattern 2", "Pattern 1"], "the third is the lowest number that no box is called");
        ctl.apply(Intent::AddPattern).unwrap();
        assert_eq!(ctl.model.patterns[3].name, "Pattern 4");
    }

    #[test]
    fn a_session_has_sixteen_boxes_at_the_most_and_the_next_is_refused_with_nothing_changed_nothing_told_and_no_id_used() {
        let (engine, mut ctl, _) = controller(TWO_LANES);
        for n in 1..=crate::source::MAX_BOXES {
            ctl.apply(Intent::AddPattern).unwrap_or_else(|why| panic!("box {n}: {why}"));
        }
        let slots: Vec<u8> = ctl.model.patterns.iter().map(|pattern| pattern.slot).collect();
        assert_eq!(slots, (0..16).collect::<Vec<u8>>());
        assert_eq!(ctl.model.patterns[15].name, "Pattern 16");
        ctl.model.check_ids().unwrap();
        told(&engine);
        let (rev, nodes) = (ctl.rev, ctl.model.nodes().count());
        let why = IntentError::Invalid("a session has at most 16 pattern boxes".to_string());
        assert_eq!(ctl.apply(Intent::AddPattern), Err(why));
        assert_eq!((ctl.rev, ctl.model.nodes().count(), ctl.model.patterns.len()), (rev, nodes, 16));
        assert_eq!(told(&engine), Vec::<String>::new());
        // No id was used by it: the box that is added when one has gone has the next id after the ones there were.
        let top = ctl.model.nodes().map(|(node, _)| node.0).max().unwrap();
        let last = box_node(&ctl, 15);
        ctl.apply(Intent::RemoveNode { node: last }).unwrap();
        ctl.apply(Intent::AddPattern).unwrap();
        assert_eq!(ctl.model.patterns[15].node, NodeId(top + 1));
        assert_eq!(ctl.model.patterns[15].slot, 15, "the slot that was free");
    }

    #[test]
    fn adding_a_box_is_a_step_of_the_history_that_undo_takes_out_and_redo_puts_back_under_the_same_node_and_slot() {
        let (engine, mut ctl, _) = controller(TWO_LANES);
        ctl.apply(Intent::AddPattern).unwrap();
        let (node, slot) = (box_node(&ctl, 0), ctl.model.patterns[0].slot);
        assert_eq!(undo_label(&ctl).as_deref(), Some("add a pattern"));
        told(&engine);
        ctl.apply(Intent::Undo).unwrap();
        assert!(ctl.model.patterns.is_empty());
        assert_eq!(told(&engine), [format!("RemoveBox node={}", node.0)]);
        assert_eq!(ctl.history_state(), HistoryState { undo: None, redo: Some("add a pattern".to_string()) });
        ctl.apply(Intent::Redo).unwrap();
        assert_eq!((box_node(&ctl, 0), ctl.model.patterns[0].slot, ctl.model.patterns.len()), (node, slot, 1));
        assert_eq!(told(&engine), [add_line(node, 48, "4", 0, &new_box_settings())]);
        ctl.model.check_ids().unwrap();
    }

    // -- taking a box out --

    #[test]
    fn a_box_that_is_taken_out_takes_its_cables_both_ways_and_the_lanes_that_heard_only_it_hear_every_source_again() {
        let (engine, mut ctl, _) = controller(PATTERNS);
        told(&engine);
        let bass = box_node(&ctl, 0);
        assert_eq!(ctl.model.event_cables.len(), 3);
        ctl.apply(Intent::RemoveNode { node: bass }).unwrap();
        assert_eq!(ctl.model.patterns.len(), 1);
        assert_eq!(ctl.model.event_cables.len(), 1, "the cable from the keys into the box and the cable from the box to the lane are gone, and the second box's stays");
        assert_eq!(told(&engine), [format!("RemoveBox node={}", bass.0), "SetLaneSources inst=0 sources=all".to_string()]);
        assert_eq!(undo_label(&ctl).as_deref(), Some("remove Bass"));
        assert!(ctl.model.locate(bass).is_none());
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn undo_puts_a_box_back_with_its_cables_notes_and_settings_under_its_node_at_its_place_and_the_audio_thread_is_told_of_it_again() {
        let (engine, mut ctl, _) = controller(PATTERNS);
        let (bass, keys_cable) = (box_node(&ctl, 0), ctl.model.event_cables[0]);
        let before = (ctl.model.patterns.clone(), ctl.model.event_cables.clone());
        ctl.apply(Intent::RemoveNode { node: bass }).unwrap();
        told(&engine);
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!((ctl.model.patterns.clone(), ctl.model.event_cables.clone()), before, "all of it, as it was");
        assert_eq!(ctl.model.event_cables[0], keys_cable);
        assert_eq!(
            told(&engine),
            [
                add_line(bass, 48, "8", 2, &settings_tail(true, "own", true, true, "all")),
                "SetLaneSources inst=0 sources=[48]".to_string(),
                settings_line(bass, true, "own", true, true, "[0]"),
            ],
            "the box with its notes, what the lane hears, and then what the box records, which is known when its cables are back"
        );
        ctl.model.check_ids().unwrap();
        // Redo takes it out again, as the first time.
        ctl.apply(Intent::Redo).unwrap();
        assert_eq!(ctl.model.patterns.len(), 1);
        assert_eq!(told(&engine), [format!("RemoveBox node={}", bass.0), "SetLaneSources inst=0 sources=all".to_string()]);
    }

    #[test]
    fn a_box_that_is_put_back_when_its_slot_was_taken_meanwhile_has_the_lowest_slot_that_is_free_and_the_lane_hears_that_one() {
        let (engine, mut ctl, _) = controller(PATTERNS);
        let bass = box_node(&ctl, 0);
        ctl.apply(Intent::RemoveNode { node: bass }).unwrap();
        // (A box that is made without the history, as a session that is changed behind its back: it takes the slot that is free.)
        ctl.add_pattern().unwrap();
        assert_eq!(ctl.model.patterns.iter().map(|pattern| pattern.slot).collect::<Vec<_>>(), [1, 0]);
        told(&engine);
        ctl.apply(Intent::Undo).unwrap();
        let back = ctl.model.pattern(bass).expect("the box is back");
        assert_eq!(back.slot, 2, "the lowest that is free");
        assert_eq!(
            told(&engine),
            [add_line(bass, 50, "8", 2, &settings_tail(true, "own", true, true, "all")), "SetLaneSources inst=0 sources=[50]".to_string(), settings_line(bass, true, "own", true, true, "[0]")],
            "the box under the source it has now, and the lane told that one"
        );
        assert_eq!(ctl.model.lane_sources(0, |_| SourceSet::NONE), SourceSet::NONE.with(Source::pattern(2)), "the lane hears the box where it is now");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_box_that_cannot_be_put_back_because_the_session_has_sixteen_is_left_out_and_so_are_the_cables_that_were_between_it_and_the_rest() {
        let (_engine, mut ctl, _) = controller(PATTERNS);
        let bass = box_node(&ctl, 0);
        ctl.apply(Intent::RemoveNode { node: bass }).unwrap();
        // (One more than there is room for: a box that is let in past the limit is a failure of this test, and not a loop that has no end.)
        for _ in 0..=crate::source::MAX_BOXES {
            if ctl.add_pattern().is_err() {
                break;
            }
        }
        assert_eq!(ctl.model.patterns.len(), crate::source::MAX_BOXES);
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!(ctl.model.patterns.len(), crate::source::MAX_BOXES);
        assert!(ctl.model.pattern(bass).is_none());
        assert_eq!(ctl.model.event_cables.len(), 1, "only the second box's cable: those of the box that is not back are not");
        ctl.model.check_ids().unwrap();
    }

    // -- the settings of a box --

    fn set_field(ctl: &mut Controller, node: NodeId, field: PatternBoxField) -> Result<Applied, IntentError> {
        ctl.apply(Intent::SetPatternBoxField { node, field })
    }

    #[test]
    fn every_setting_of_a_box_is_the_models_and_is_told_to_the_audio_thread_as_the_box_is_set_to_and_is_a_step_in_the_words_of_the_setting_that_undo_puts_back() {
        let (engine, mut ctl, _) = controller(PATTERNS);
        let bass = box_node(&ctl, 0);
        told(&engine);
        let changes = [
            (PatternBoxField::Looping { on: false }, settings_line(bass, false, "own", true, true, "[0]"), "turn looping off for Bass"),
            (PatternBoxField::Sync { sync: PatternSync::Host }, settings_line(bass, false, "host", true, true, "[0]"), "let Bass go by the host's beat"),
            (PatternBoxField::Overdub { on: false }, settings_line(bass, false, "host", false, true, "[0]"), "turn overdub off for Bass"),
            (PatternBoxField::Click { on: false }, settings_line(bass, false, "host", false, false, "[0]"), "turn the click off for Bass"),
            (PatternBoxField::Sync { sync: PatternSync::Own }, settings_line(bass, false, "own", false, false, "[0]"), "let Bass go by its own clock"),
            (PatternBoxField::Looping { on: true }, settings_line(bass, true, "own", false, false, "[0]"), "turn looping on for Bass"),
        ];
        for (field, line, label) in &changes {
            let rev = ctl.rev;
            assert_eq!(set_field(&mut ctl, bass, field.clone()), Ok(Applied::restructured(rev + 1)), "{field:?}");
            assert_eq!(told(&engine), std::slice::from_ref(line), "{field:?}");
            assert_eq!(undo_label(&ctl).as_deref(), Some(*label), "{field:?}");
            assert!(ctl.dirty);
        }
        let pattern = &ctl.model.patterns[0];
        assert_eq!((pattern.looping, pattern.sync, pattern.overdub, pattern.click), (true, Clock::Own, false, false));
        // Undo puts each back, last first, and the audio thread is told each time.
        let mut before = vec![
            settings_line(bass, false, "own", false, false, "[0]"),
            settings_line(bass, false, "host", false, false, "[0]"),
            settings_line(bass, false, "host", false, true, "[0]"),
            settings_line(bass, false, "host", true, true, "[0]"),
            settings_line(bass, false, "own", true, true, "[0]"),
            settings_line(bass, true, "own", true, true, "[0]"),
        ];
        while let Some(line) = before.first().cloned() {
            ctl.apply(Intent::Undo).unwrap();
            assert_eq!(told(&engine), [line]);
            before.remove(0);
        }
        let pattern = &ctl.model.patterns[0];
        assert_eq!((pattern.looping, pattern.sync, pattern.overdub, pattern.click), (true, Clock::Own, true, true), "the box as the file had it");
        assert_eq!(undo_label(&ctl), None);
    }

    #[test]
    fn a_setting_that_is_what_it_is_asked_to_be_changes_nothing_keeps_no_step_and_tells_the_audio_thread_nothing() {
        let (engine, mut ctl, _) = controller(PATTERNS);
        let bass = box_node(&ctl, 0);
        told(&engine);
        for field in [
            PatternBoxField::Looping { on: true },
            PatternBoxField::Sync { sync: PatternSync::Own },
            PatternBoxField::Overdub { on: true },
            PatternBoxField::Click { on: true },
            PatternBoxField::LengthBeats { beats: 8.0 },
            PatternBoxField::LengthBeats { beats: 8.0000001 },
            PatternBoxField::Name { name: "Bass".to_string() },
            PatternBoxField::Name { name: "  Bass \t".to_string() },
        ] {
            set_field(&mut ctl, bass, field.clone()).unwrap_or_else(|why| panic!("{field:?}: {why}"));
            assert_eq!(undo_label(&ctl), None, "{field:?}");
            assert_eq!(told(&engine), Vec::<String>::new(), "{field:?}");
        }
    }

    #[test]
    fn a_box_is_renamed_by_the_rule_of_a_group_and_a_name_of_nothing_is_the_name_of_a_box_that_is_not_called() {
        let (_engine, mut ctl, _) = controller(PATTERNS);
        let bass = box_node(&ctl, 0);
        set_field(&mut ctl, bass, PatternBoxField::Name { name: "  Lead line \t".to_string() }).unwrap();
        assert_eq!(ctl.model.patterns[0].name, "Lead line");
        assert_eq!(undo_label(&ctl).as_deref(), Some("rename Bass"));
        set_field(&mut ctl, bass, PatternBoxField::Name { name: " \t ".to_string() }).unwrap();
        assert_eq!(ctl.model.patterns[0].name, "Pattern");
        let longest = "x".repeat(crate::app::model::MAX_NAME);
        set_field(&mut ctl, bass, PatternBoxField::Name { name: longest.clone() }).unwrap();
        assert_eq!(ctl.model.patterns[0].name, longest);
        let rev = ctl.rev;
        assert_eq!(set_field(&mut ctl, bass, PatternBoxField::Name { name: format!("{longest}x") }), Err(IntentError::Invalid("a name is at most 64 characters".to_string())));
        assert_eq!(set_field(&mut ctl, bass, PatternBoxField::Name { name: "A\nB".to_string() }), Err(IntentError::Invalid("a name has no control characters".to_string())));
        assert_eq!((ctl.rev, ctl.model.patterns[0].name.as_str()), (rev, longest.as_str()));
    }

    #[test]
    fn the_length_of_a_box_is_held_to_the_lengths_a_box_can_have_and_told_to_the_audio_thread_as_a_new_plan_with_the_notes_it_has() {
        let (engine, mut ctl, _) = controller(PATTERNS);
        let bass = box_node(&ctl, 0);
        told(&engine);
        for (asked, length) in [(16.0, 16.0), (1000.0, 256.0), (0.01, 0.25), (0.0, 0.25), (-3.0, 0.25), (256.0, 256.0), (0.25, 0.25), (3.5, 3.5)] {
            let before = ctl.model.patterns[0].length_beats;
            set_field(&mut ctl, bass, PatternBoxField::LengthBeats { beats: asked }).unwrap();
            assert_eq!(ctl.model.patterns[0].length_beats, length, "{asked}");
            let expected = if length == before { Vec::new() } else { vec![format!("SetBoxPlan node={} length={length} notes=2", bass.0)] };
            assert_eq!(told(&engine), expected, "{asked}");
        }
        let rev = ctl.rev;
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(set_field(&mut ctl, bass, PatternBoxField::LengthBeats { beats: bad }), Err(IntentError::Invalid(format!("{bad} is not a length in beats"))));
        }
        assert_eq!((ctl.rev, ctl.model.patterns[0].length_beats, told(&engine)), (rev, 3.5, Vec::new()));
        // They came at one instant, so they are one step: undo puts back the length that the box had before the first, and the audio thread is told it.
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!(told(&engine), [format!("SetBoxPlan node={} length=8 notes=2", bass.0)]);
        assert_eq!(ctl.model.patterns[0].length_beats, 8.0);
    }

    #[test]
    fn what_is_asked_of_a_node_that_is_no_box_or_is_not_there_is_refused_for_every_intent_of_a_box_and_changes_nothing() {
        let (engine, mut ctl, _) = controller(PATTERNS);
        let lane = lane_node(&ctl, 0);
        told(&engine);
        let rev = ctl.rev;
        for (node, error) in [(lane, IntentError::Invalid("that node is not a pattern box".to_string())), (NodeId(900), IntentError::NotFound(NodeId(900)))] {
            let asks = [
                Intent::SetPatternBoxField { node, field: PatternBoxField::Looping { on: false } },
                Intent::PatternBoxTransport { node, action: PatternBoxAction::Record },
                Intent::ClearPatternBox { node },
                Intent::SetPatternBoxNotes { node, notes: vec![] },
            ];
            for ask in asks {
                assert_eq!(ctl.apply(ask.clone()), Err(error.clone()), "{ask:?}");
            }
        }
        assert_eq!((ctl.rev, ctl.dirty, told(&engine)), (rev, false, Vec::new()));
    }

    #[test]
    fn typing_a_name_and_dragging_a_length_are_one_step_each_if_they_come_soon_after_each_other_and_undo_goes_back_to_where_they_began() {
        let (engine, mut ctl, clock) = controller(PATTERNS);
        let bass = box_node(&ctl, 0);
        for name in ["B", "Ba", "Bas"] {
            set_field(&mut ctl, bass, PatternBoxField::Name { name: name.to_string() }).unwrap();
            clock.advance(Duration::from_millis(300));
        }
        for beats in [9.0, 10.0, 12.0] {
            set_field(&mut ctl, bass, PatternBoxField::LengthBeats { beats }).unwrap();
            clock.advance(Duration::from_millis(300));
        }
        assert_eq!((ctl.model.patterns[0].name.as_str(), ctl.model.patterns[0].length_beats), ("Bas", 12.0));
        told(&engine);
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!((ctl.model.patterns[0].name.as_str(), ctl.model.patterns[0].length_beats), ("Bas", 8.0), "the drag was one step");
        assert_eq!(told(&engine), [format!("SetBoxPlan node={} length=8 notes=2", bass.0)]);
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!(ctl.model.patterns[0].name, "Bass", "and so was the typing");
        assert_eq!(undo_label(&ctl), None);
        // A change that comes a second after the last is a step of its own.
        ctl.apply(Intent::Redo).unwrap();
        ctl.apply(Intent::Redo).unwrap();
        clock.advance(Duration::from_secs(5));
        set_field(&mut ctl, bass, PatternBoxField::Name { name: "Bassline".to_string() }).unwrap();
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!(ctl.model.patterns[0].name, "Bas");
    }

    // -- the notes of a box --

    fn wire(notes: &[BoxNote]) -> Vec<PatternNote> {
        notes.iter().copied().map(PatternNote::from).collect()
    }

    #[test]
    fn the_notes_that_a_client_sets_are_the_notes_as_a_box_holds_them_and_the_audio_thread_gets_them_as_a_plan_and_undo_gives_back_the_notes_there_were() {
        let (engine, mut ctl, _) = controller(PATTERNS);
        let bass = box_node(&ctl, 0);
        let first = ctl.model.patterns[0].notes.clone();
        told(&engine);
        let asked = vec![
            PatternNote { start: 3.0, length: 1.0, note: 64, velocity: 100 },
            PatternNote { start: 1.0, length: -2.0, note: 60, velocity: 0 },
            PatternNote { start: f32::NAN, length: 1.0, note: 62, velocity: 100 },
            PatternNote { start: 2.0, length: 1.0, note: 200, velocity: 100 },
        ];
        assert_eq!(ctl.apply(Intent::SetPatternBoxNotes { node: bass, notes: asked }), Ok(Applied::restructured(1)));
        assert_eq!(ctl.model.patterns[0].notes, [note(1.0, 0.0, 60, 1), note(3.0, 1.0, 64, 100)], "what is not a note is out, the rest is in range and in order");
        assert_eq!(told(&engine), [format!("SetBoxPlan node={} length=8 notes=2", bass.0)]);
        assert_eq!(undo_label(&ctl).as_deref(), Some("edit Bass"));
        assert!(ctl.dirty);
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!(ctl.model.patterns[0].notes, first);
        assert_eq!(told(&engine), [format!("SetBoxPlan node={} length=8 notes=2", bass.0)]);
        ctl.apply(Intent::Redo).unwrap();
        assert_eq!(ctl.model.patterns[0].notes, [note(1.0, 0.0, 60, 1), note(3.0, 1.0, 64, 100)]);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn the_notes_that_a_box_has_are_no_change_and_are_not_told_again_whatever_order_they_come_in() {
        let (engine, mut ctl, _) = controller(PATTERNS);
        let bass = box_node(&ctl, 0);
        told(&engine);
        let mut reversed = wire(&ctl.model.patterns[0].notes);
        reversed.reverse();
        ctl.apply(Intent::SetPatternBoxNotes { node: bass, notes: reversed }).unwrap();
        assert_eq!((undo_label(&ctl), told(&engine)), (None, Vec::new()));
    }

    #[test]
    fn a_box_holds_four_thousand_and_ninety_six_notes_at_the_most_the_first_of_those_that_are_sent() {
        let (engine, mut ctl, _) = controller(PATTERNS);
        let bass = box_node(&ctl, 0);
        let many: Vec<PatternNote> = (0..crate::patternbox::MAX_NOTES + 5).map(|n| PatternNote { start: n as f32 * 0.01, length: 0.005, note: (n % 100) as u8, velocity: 100 }).collect();
        told(&engine);
        ctl.apply(Intent::SetPatternBoxNotes { node: bass, notes: many }).unwrap();
        assert_eq!(ctl.model.patterns[0].notes.len(), crate::patternbox::MAX_NOTES);
        assert_eq!(told(&engine), [format!("SetBoxPlan node={} length=8 notes=4096", bass.0)]);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn dragging_a_note_is_one_step_whatever_the_number_of_changes_and_undo_gives_back_the_notes_it_began_with() {
        let (_engine, mut ctl, clock) = controller(PATTERNS);
        let bass = box_node(&ctl, 0);
        let first = ctl.model.patterns[0].notes.clone();
        for start in [0.1, 0.2, 0.3] {
            let mut notes = wire(&first);
            notes[0].start = start;
            ctl.apply(Intent::SetPatternBoxNotes { node: bass, notes }).unwrap();
            clock.advance(Duration::from_millis(200));
        }
        assert_eq!(ctl.model.patterns[0].notes[0].start, 0.3);
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!(ctl.model.patterns[0].notes, first);
        assert_eq!(undo_label(&ctl), None);
    }

    #[test]
    fn clearing_a_box_takes_its_notes_out_in_one_step_that_undo_gives_back_and_a_box_with_none_is_not_cleared_again() {
        let (engine, mut ctl, _) = controller(PATTERNS);
        let (bass, plain) = (box_node(&ctl, 0), box_node(&ctl, 1));
        let first = ctl.model.patterns[0].notes.clone();
        told(&engine);
        assert_eq!(ctl.apply(Intent::ClearPatternBox { node: bass }), Ok(Applied::restructured(1)));
        assert!(ctl.model.patterns[0].notes.is_empty());
        assert_eq!(told(&engine), [format!("SetBoxPlan node={} length=8 notes=0", bass.0)]);
        assert_eq!(undo_label(&ctl).as_deref(), Some("clear Bass"));
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!(ctl.model.patterns[0].notes, first);
        assert_eq!(told(&engine), [format!("SetBoxPlan node={} length=8 notes=2", bass.0)]);
        // The second box has none: nothing is told and no step is kept.
        ctl.apply(Intent::Redo).unwrap();
        ctl.apply(Intent::Undo).unwrap();
        told(&engine);
        let steps = ctl.history_state();
        ctl.apply(Intent::ClearPatternBox { node: plain }).unwrap();
        assert_eq!((ctl.history_state(), told(&engine)), (steps, Vec::new()));
    }

    // -- recording and playing --

    #[test]
    fn the_transport_of_a_box_is_told_to_the_audio_thread_and_changes_nothing_of_the_session_and_keeps_no_step_and_the_status_is_what_the_audio_thread_says() {
        let (engine, mut ctl, _) = controller(PATTERNS);
        let bass = box_node(&ctl, 0);
        told(&engine);
        for (n, (action, word)) in [(PatternBoxAction::Record, "Record"), (PatternBoxAction::FinishRecording, "FinishRecording"), (PatternBoxAction::Play, "Play"), (PatternBoxAction::Stop, "Stop")].into_iter().enumerate() {
            let applied = ctl.apply(Intent::PatternBoxTransport { node: bass, action }).unwrap();
            assert_eq!(applied, Applied::of(n as u64 + 1, Vec::new()), "a revision is used, and no event");
            assert_eq!(told(&engine), [format!("BoxTransport node={} {word}", bass.0)]);
        }
        assert!(!ctl.dirty, "the session is not changed");
        assert_eq!(ctl.history_state(), HistoryState::default());
        assert_eq!(ctl.model.patterns[0].status, Status::Stopped, "nothing is guessed: the audio thread says what the box does");
    }

    #[test]
    fn a_box_is_told_what_it_records_before_it_is_told_to_record_when_a_device_it_is_cabled_from_has_been_plugged_in_since_the_last_look() {
        let (engine, mut ctl, _) = controller(PATTERNS);
        let ports = Ports::default();
        ctl.watch_devices(Box::new(ports.clone()));
        let (device, plain) = (ctl.model.sources[1].node, box_node(&ctl, 1));
        draw(&mut ctl, device, plain).unwrap();
        told(&engine);
        // The first look at the ports has not been made, and the port is open.
        ports.plug_in("Launchkey Mini MK4 37 MIDI Out", Source::device(0));
        ctl.apply(Intent::PatternBoxTransport { node: plain, action: PatternBoxAction::Record }).unwrap();
        assert_eq!(told(&engine), [settings_line(plain, true, "own", false, true, "[1]"), format!("BoxTransport node={} Record", plain.0)]);
    }

    #[test]
    fn what_a_box_says_it_is_doing_is_the_models_status_and_nothing_else_changes_and_it_is_told_to_the_clients_in_the_graph() {
        let (engine, mut ctl, _) = controller(PATTERNS);
        let bass = box_node(&ctl, 0);
        let status = |ctl: &Controller| -> PatternStatus {
            match ctl.model.graph().nodes.iter().find(|node| node.id == bass).map(|node| &node.kind) {
                Some(NodeKind::Pattern { status, .. }) => *status,
                other => panic!("not a box: {other:?}"),
            }
        };
        assert_eq!(status(&ctl), PatternStatus::Stopped);
        for (said, shown) in [
            (Status::CountingIn, PatternStatus::CountingIn),
            (Status::Recording, PatternStatus::Recording),
            (Status::Waiting, PatternStatus::Waiting),
            (Status::Playing, PatternStatus::Playing),
            (Status::Stopped, PatternStatus::Stopped),
        ] {
            engine.box_tx.send(BoxNotification::Status { node: bass, status: said }).unwrap();
            assert!(ctl.poll_at(Instant::now()).model_changed, "{said:?}");
            assert_eq!((ctl.model.patterns[0].status, status(&ctl)), (said, shown));
            assert!(!ctl.dirty && ctl.history_state() == HistoryState::default(), "a status is not a change of the session: {said:?}");
        }
        // What it says again changes nothing, and neither does what is said of a box that is not there.
        engine.box_tx.send(BoxNotification::Status { node: bass, status: Status::Stopped }).unwrap();
        engine.box_tx.send(BoxNotification::Status { node: NodeId(900), status: Status::Playing }).unwrap();
        engine.box_tx.send(BoxNotification::Status { node: lane_node(&ctl, 0), status: Status::Playing }).unwrap();
        assert!(!ctl.poll_at(Instant::now()).model_changed);
    }

    #[test]
    fn what_a_box_recorded_are_its_notes_the_session_is_unsaved_and_the_history_has_a_step_that_undo_takes_back_and_redo_does_again() {
        let (engine, mut ctl, _) = controller(PATTERNS);
        let bass = box_node(&ctl, 0);
        let first = ctl.model.patterns[0].notes.clone();
        let recorded = vec![note(1.0, 0.5, 60, 100), note(2.0, 0.25, 64, 80)];
        told(&engine);
        engine.box_tx.send(BoxNotification::Recorded { node: bass, notes: recorded.clone(), plans: 1 }).unwrap();
        assert!(ctl.poll_at(Instant::now()).model_changed);
        assert_eq!(ctl.model.patterns[0].notes, recorded);
        assert!(ctl.dirty);
        assert_eq!(undo_label(&ctl).as_deref(), Some("record into Bass"));
        assert_eq!(told(&engine), Vec::<String>::new(), "the audio thread has these notes: it is where they came from");
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!(ctl.model.patterns[0].notes, first);
        assert_eq!(told(&engine), [format!("SetBoxPlan node={} length=8 notes=2", bass.0)]);
        ctl.apply(Intent::Redo).unwrap();
        assert_eq!(ctl.model.patterns[0].notes, recorded);
        assert_eq!(ctl.history_state().undo.as_deref(), Some("record into Bass"));
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn what_a_box_recorded_is_held_as_a_box_holds_notes_and_the_name_in_the_step_is_the_name_the_box_has() {
        let (engine, mut ctl, _) = controller(PATTERNS);
        let bass = box_node(&ctl, 0);
        set_field(&mut ctl, bass, PatternBoxField::Name { name: "Lead".to_string() }).unwrap();
        let said = vec![note(2.0, 1.0, 64, 0), note(1.0, 1.0, 60, 200), note(f32::NAN, 1.0, 61, 100)];
        engine.box_tx.send(BoxNotification::Recorded { node: bass, notes: said, plans: 1 }).unwrap();
        ctl.poll_at(Instant::now());
        assert_eq!(ctl.model.patterns[0].notes, [note(1.0, 1.0, 60, 127), note(2.0, 1.0, 64, 1)]);
        assert_eq!(undo_label(&ctl).as_deref(), Some("record into Lead"));
    }

    #[test]
    fn what_a_box_recorded_that_it_has_already_or_of_a_box_that_is_gone_changes_nothing_and_keeps_no_step() {
        let (engine, mut ctl, _) = controller(PATTERNS);
        let bass = box_node(&ctl, 0);
        let same = ctl.model.patterns[0].notes.clone();
        engine.box_tx.send(BoxNotification::Recorded { node: bass, notes: same, plans: 1 }).unwrap();
        engine.box_tx.send(BoxNotification::Recorded { node: NodeId(900), notes: vec![note(0.0, 1.0, 60, 100)], plans: 1 }).unwrap();
        engine.box_tx.send(BoxNotification::Recorded { node: lane_node(&ctl, 0), notes: vec![note(0.0, 1.0, 60, 100)], plans: 1 }).unwrap();
        assert!(!ctl.poll_at(Instant::now()).model_changed);
        assert!(!ctl.dirty);
        assert_eq!(undo_label(&ctl), None);
    }

    #[test]
    fn a_recording_that_ended_before_the_audio_thread_took_the_last_change_of_the_notes_is_lost_to_the_box_and_is_not_the_models_notes_and_one_made_on_the_last_plan_is() {
        let (engine, mut ctl, _) = controller(PATTERNS);
        let bass = box_node(&ctl, 0);
        // The box is made with one plan, and two changes of its notes are told to it: it has had three when it has taken both.
        ctl.apply(Intent::ClearPatternBox { node: bass }).unwrap();
        ctl.apply(Intent::SetPatternBoxNotes { node: bass, notes: vec![PatternNote { start: 1.0, length: 1.0, note: 62, velocity: 100 }] }).unwrap();
        let (kept, steps) = (ctl.model.patterns[0].notes.clone(), ctl.history_state());
        assert_eq!(kept, [note(1.0, 1.0, 62, 100)]);
        told(&engine);
        // A pass that ended when the box had had the first plan, or the second, took notes that the changes since have replaced: the box is left with the model's, and the model keeps them.
        for plans in [1, 2] {
            engine.box_tx.send(BoxNotification::Recorded { node: bass, notes: vec![note(0.0, 1.0, 72, 100)], plans }).unwrap();
            assert!(!ctl.poll_at(Instant::now()).model_changed, "made on plan {plans}");
            assert_eq!((ctl.model.patterns[0].notes.clone(), ctl.history_state()), (kept.clone(), steps.clone()), "made on plan {plans}");
        }
        assert_eq!(told(&engine), Vec::<String>::new(), "nothing is told: the audio thread has what the model has");
        // One that ended when it had had all three is the box's notes.
        engine.box_tx.send(BoxNotification::Recorded { node: bass, notes: vec![note(0.0, 1.0, 72, 100)], plans: 3 }).unwrap();
        assert!(ctl.poll_at(Instant::now()).model_changed);
        assert_eq!(ctl.model.patterns[0].notes, [note(0.0, 1.0, 72, 100)]);
        assert_eq!(undo_label(&ctl).as_deref(), Some("record into Bass"));
        // A plan that is told after that is the fourth.
        ctl.apply(Intent::ClearPatternBox { node: bass }).unwrap();
        engine.box_tx.send(BoxNotification::Recorded { node: bass, notes: vec![note(0.0, 1.0, 74, 100)], plans: 3 }).unwrap();
        assert!(!ctl.poll_at(Instant::now()).model_changed, "made on the third, and the fourth is told");
        engine.box_tx.send(BoxNotification::Recorded { node: bass, notes: vec![note(0.0, 1.0, 74, 100)], plans: 4 }).unwrap();
        assert!(ctl.poll_at(Instant::now()).model_changed);
    }

    #[test]
    fn a_change_of_the_length_is_a_plan_and_a_change_that_is_no_change_is_none_and_a_box_that_is_put_back_has_the_one_plan_again() {
        let (engine, mut ctl, _) = controller(PATTERNS);
        let bass = box_node(&ctl, 0);
        let recorded = |plans: u32| BoxNotification::Recorded { node: bass, notes: vec![note(0.0, 1.0, 72, 100)], plans };
        // A length that is told is the second plan; one that the box has already is not told, and is no plan.
        set_field(&mut ctl, bass, PatternBoxField::LengthBeats { beats: 8.0 }).unwrap();
        engine.box_tx.send(recorded(2)).unwrap();
        assert!(!ctl.poll_at(Instant::now()).model_changed, "no plan was told: the box has had one");
        engine.box_tx.send(recorded(1)).unwrap();
        assert!(ctl.poll_at(Instant::now()).model_changed);
        // The settings are not plans.
        set_field(&mut ctl, bass, PatternBoxField::Overdub { on: false }).unwrap();
        ctl.apply(Intent::ClearPatternBox { node: bass }).unwrap();
        ctl.apply(Intent::ClearPatternBox { node: bass }).unwrap();
        set_field(&mut ctl, bass, PatternBoxField::LengthBeats { beats: 5.0 }).unwrap();
        assert_eq!(ctl.plans_sent(bass), 3, "the notes that were cleared and the length; the second clear changed nothing");
        // A box that goes and comes back by undo is a new box to the audio thread, and has the one plan.
        ctl.apply(Intent::RemoveNode { node: bass }).unwrap();
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!(ctl.plans_sent(bass), 1);
        engine.box_tx.send(BoxNotification::Recorded { node: bass, notes: vec![note(3.0, 1.0, 70, 100)], plans: 1 }).unwrap();
        assert!(ctl.poll_at(Instant::now()).model_changed);
        assert_eq!(ctl.model.patterns[0].notes, [note(3.0, 1.0, 70, 100)]);
    }

    #[test]
    fn a_recording_that_is_made_after_another_step_is_the_next_step_and_one_that_was_undone_is_forgotten_by_it() {
        let (engine, mut ctl, _) = controller(PATTERNS);
        let bass = box_node(&ctl, 0);
        set_field(&mut ctl, bass, PatternBoxField::Looping { on: false }).unwrap();
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!(ctl.history_state().redo.as_deref(), Some("turn looping off for Bass"));
        engine.box_tx.send(BoxNotification::Recorded { node: bass, notes: vec![note(0.5, 1.0, 62, 100)], plans: 1 }).unwrap();
        ctl.poll_at(Instant::now());
        assert_eq!(ctl.history_state(), HistoryState { undo: Some("record into Bass".to_string()), redo: None });
    }

    // -- the cables of a box --

    const BOXES_AND_LANES: &str = "[keys]\n\n[[device]]\nname = \"Launchkey\"\n\n[[pattern]]\n\n[[pattern]]\n\n[[instrument]]\nplugin = \"builtin:osc\"\n\n[[instrument]]\nplugin = \"builtin:osc\"\n";

    #[test]
    fn a_cable_of_notes_into_a_box_makes_it_record_the_source_it_comes_from_and_a_cable_from_a_box_makes_the_lane_hear_that_box_and_no_source() {
        let (engine, mut ctl, _) = controller(BOXES_AND_LANES);
        let (keys, device) = (ctl.model.sources[0].node, ctl.model.sources[1].node);
        let (first, second, lane0, lane1) = (box_node(&ctl, 0), box_node(&ctl, 1), lane_node(&ctl, 0), lane_node(&ctl, 1));
        assert_eq!(told(&engine), [settings_line(first, true, "own", false, true, "all"), settings_line(second, true, "own", false, true, "all")], "a loaded box is told what it records, every input until a cable says another");
        assert_eq!(draw(&mut ctl, keys, first), Ok(Applied::restructured(1)));
        assert_eq!(told(&engine), [settings_line(first, true, "own", false, true, "[0]")]);
        assert_eq!(undo_label(&ctl).as_deref(), Some("draw a cable of notes"));
        assert_eq!(draw(&mut ctl, first, lane0), Ok(Applied::restructured(2)));
        assert_eq!(told(&engine), ["SetLaneSources inst=0 sources=[48]".to_string()], "the lane hears the box and no key");
        draw(&mut ctl, device, first).unwrap();
        assert_eq!(told(&engine), Vec::<String>::new(), "a device that is not plugged in is no source, either way");
        draw(&mut ctl, keys, lane1).unwrap();
        assert_eq!(told(&engine), ["SetLaneSources inst=1 sources=[0]".to_string()]);
        draw(&mut ctl, second, lane1).unwrap();
        assert_eq!(told(&engine), ["SetLaneSources inst=1 sources=[0, 49]".to_string()], "the keys and the second box");
        assert_eq!(ctl.model.event_cables.len(), 5);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_cable_of_notes_to_or_from_a_box_that_cannot_be_drawn_is_refused_with_the_reason_and_changes_nothing_and_uses_no_id() {
        let (engine, mut ctl, _) = controller(BOXES_AND_LANES);
        let (keys, first, second, lane) = (ctl.model.sources[0].node, box_node(&ctl, 0), box_node(&ctl, 1), lane_node(&ctl, 0));
        draw(&mut ctl, keys, first).unwrap();
        told(&engine);
        let (rev, cables) = (ctl.rev, ctl.model.event_cables.clone());
        let audio_in = Endpoint { node: lane, port: Port::AudioIn };
        let audio_out = Endpoint { node: first, port: Port::AudioOut };
        for (from, to, gain, why) in [
            (out(first), into(second), None, "a pattern box cannot send its notes to another box yet: it sends them to a lane"),
            (out(first), into(first), None, "a pattern box cannot send its notes to another box yet"),
            (out(lane), into(first), None, "only a source of notes has an output of events"),
            (out(keys), into(first), None, "there is a cable between those two already"),
            (out(keys), into(second), Some(0.5), "a cable of notes has no level"),
            (out(first), into(lane), Some(1.0), "a cable of notes has no level"),
            (out(first), audio_in, None, "ends at an input of events"),
            (audio_out, into(lane), None, "starts at an output of events"),
            (out(first), into(NodeId::MASTER), None, "the mix has no input of events"),
            (out(first), into(keys), None, "a cable of notes ends at a lane"),
            (out(NodeId(900)), into(first), None, "no such node"),
        ] {
            let error = ctl.apply(Intent::AddCable { from, to, gain }).unwrap_err();
            assert!(error.to_string().contains(why) || matches!(error, IntentError::NotFound(_)) && why == "no such node", "{from:?} to {to:?}: {error}");
        }
        assert_eq!((ctl.rev, ctl.model.event_cables.clone(), told(&engine)), (rev, cables, Vec::new()));
        draw(&mut ctl, first, lane).unwrap();
        assert_eq!(ctl.model.event_cables.last().unwrap().id, EdgeId(1), "no id was used by the refusals");
        assert_eq!(ctl.model.event_cables.last().copied(), Some(EventCable::new(EdgeId(1), first, lane)));
    }

    #[test]
    fn a_box_sends_no_notes_to_a_lane_whose_instrument_takes_none() {
        let (engine, mut ctl, _) = controller("[keys]\n[[pattern]]\n[[instrument]]\nplugin = \"builtin:input\"\nroute = \"none\"\n");
        let (first, input) = (box_node(&ctl, 0), lane_node(&ctl, 0));
        told(&engine);
        assert_eq!(draw(&mut ctl, first, input), Err(IntentError::Invalid("that instrument takes no notes: an audio input has no input of events".to_string())));
        assert!(ctl.model.event_cables.is_empty());
    }

    #[test]
    fn taking_out_a_cable_of_notes_into_a_box_makes_it_record_every_input_again_and_one_from_it_makes_the_lane_hear_every_source_and_undo_gives_each_back() {
        let (engine, mut ctl, _) = controller(PATTERNS);
        let bass = box_node(&ctl, 0);
        told(&engine);
        let (into_box, out_of_box) = (ctl.model.event_cables[0].id, ctl.model.event_cables[1].id);
        ctl.apply(Intent::RemoveEdge { edge: into_box }).unwrap();
        assert_eq!(told(&engine), [settings_line(bass, true, "own", true, true, "all")]);
        assert_eq!(undo_label(&ctl).as_deref(), Some("remove a cable of notes"));
        ctl.apply(Intent::RemoveEdge { edge: out_of_box }).unwrap();
        assert_eq!(told(&engine), ["SetLaneSources inst=0 sources=all".to_string()]);
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!(told(&engine), ["SetLaneSources inst=0 sources=[48]".to_string()]);
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!(told(&engine), [settings_line(bass, true, "own", true, true, "[0]")]);
        assert_eq!(ctl.model.event_cables.iter().map(|cable| cable.id).collect::<Vec<_>>(), [EdgeId(0), EdgeId(1), EdgeId(2)], "under the ids they had");
    }

    #[test]
    fn a_lane_that_hears_a_box_is_taken_out_with_its_cable_and_comes_back_hearing_the_box_and_the_box_goes_on() {
        let (engine, mut ctl, _) = controller(PATTERNS);
        let (bass, lane) = (box_node(&ctl, 0), lane_node(&ctl, 0));
        told(&engine);
        ctl.apply(Intent::RemoveNode { node: lane }).unwrap();
        assert_eq!(ctl.model.event_cables.len(), 2, "the box's cable from the keys and the second box's cable to the second lane");
        assert!(ctl.model.pattern(bass).is_some());
        assert_eq!(told(&engine), Vec::<String>::new(), "no box was taken out, and what the other lane hears is what it was told");
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!(ctl.model.event_cables.len(), 3);
        assert_eq!(told(&engine), ["SetLaneSources inst=0 sources=[48]".to_string()]);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_lane_that_is_added_to_a_session_that_has_boxes_hears_every_input_and_no_box_until_a_cable_says_so() {
        let (engine, mut ctl, _) = controller(PATTERNS);
        told(&engine);
        ctl.apply(Intent::AddInstrument { plugin: "builtin:osc".to_string() }).unwrap();
        assert_eq!(told(&engine), ["SetLaneSources inst=2 sources=all".to_string()]);
        assert_eq!(ctl.lane_sources(2), SourceSet::ALL);
        assert!(!ctl.lane_sources(2).contains(Source::pattern(0)) && !ctl.lane_sources(2).contains(Source::pattern(1)), "a box is not an input");
        let (plain, lane) = (box_node(&ctl, 1), lane_node(&ctl, 2));
        draw(&mut ctl, plain, lane).unwrap();
        assert_eq!(told(&engine), ["SetLaneSources inst=2 sources=[49]".to_string()]);
    }

    #[test]
    fn what_a_box_records_follows_the_devices_that_it_is_cabled_from_as_they_are_plugged_in_and_out() {
        let (engine, mut ctl, _) = controller(PATTERNS);
        told(&engine);
        let ports = Ports::default();
        ctl.watch_devices(Box::new(ports.clone()));
        let (device, bass) = (ctl.model.sources[1].node, box_node(&ctl, 0));
        draw(&mut ctl, device, bass).unwrap();
        assert_eq!(told(&engine), Vec::<String>::new(), "the keys, and a device that is not plugged in");
        let t0 = Instant::now() + Duration::from_secs(60);
        ports.plug_in("Launchkey Mini MK4 37 MIDI Out", Source::device(0));
        ctl.poll_at(t0 + Duration::from_secs(1));
        assert_eq!(told(&engine), [settings_line(bass, true, "own", true, true, "[0, 1]")], "the keys, and the port that the device's name stands for");
        ports.plug_in("Launchkey Mini MK4 37 DAW Out", Source::device(1));
        ctl.poll_at(t0 + Duration::from_secs(2));
        assert_eq!(told(&engine), [settings_line(bass, true, "own", true, true, "[0, 1, 2]")]);
        ports.unplug("Launchkey Mini MK4 37 MIDI Out");
        ctl.poll_at(t0 + Duration::from_secs(3));
        assert_eq!(told(&engine), ["ReleaseSource source=1".to_string(), settings_line(bass, true, "own", true, true, "[0, 2]")]);
        // A look at the ports that finds nothing new tells nothing.
        ctl.poll_at(t0 + Duration::from_secs(4));
        assert_eq!(told(&engine), Vec::<String>::new());
    }

    // -- what the mutants showed, and what a box is refused --

    #[test]
    fn a_box_that_is_taken_out_by_something_that_is_no_intent_forgets_the_history_as_any_removal_does_and_a_box_that_is_not_there_is_not_taken_out() {
        let (engine, mut ctl, _) = controller(PATTERNS);
        let bass = box_node(&ctl, 0);
        set_field(&mut ctl, bass, PatternBoxField::Looping { on: false }).unwrap();
        assert!(undo_label(&ctl).is_some());
        told(&engine);
        assert!(!ctl.remove_pattern(9), "there is no box 9");
        assert_eq!((ctl.model.patterns.len(), undo_label(&ctl).is_some(), told(&engine)), (2, true, Vec::new()), "nothing was done");
        assert!(ctl.remove_pattern(0));
        assert_eq!(ctl.history_state(), HistoryState::default(), "what was done before is not undone over a change that was no step");
        assert_eq!(told(&engine)[0], format!("RemoveBox node={}", bass.0));
    }

    #[test]
    fn a_box_that_is_put_back_keeps_its_slot_when_that_is_free_though_a_lower_slot_is_free_too() {
        let (engine, mut ctl, _) = controller(BOXES_AND_LANES);
        ctl.apply(Intent::AddPattern).unwrap();
        let (first, third) = (box_node(&ctl, 0), box_node(&ctl, 2));
        assert_eq!(ctl.model.patterns[2].slot, 2);
        ctl.apply(Intent::RemoveNode { node: first }).unwrap();
        ctl.apply(Intent::RemoveNode { node: third }).unwrap();
        told(&engine);
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!(ctl.model.pattern(third).map(|pattern| pattern.slot), Some(2), "its own, which is free, and not the lowest that is");
        assert_eq!(told(&engine), [add_line(third, 50, "4", 0, &new_box_settings())]);
    }

    #[test]
    fn a_box_comes_back_at_its_place_among_the_boxes_and_stopped_whatever_it_was_doing_when_it_went() {
        let (engine, mut ctl, _) = controller(BOXES_AND_LANES);
        ctl.apply(Intent::AddPattern).unwrap();
        let (first, second, third) = (box_node(&ctl, 0), box_node(&ctl, 1), box_node(&ctl, 2));
        engine.box_tx.send(BoxNotification::Status { node: second, status: Status::Playing }).unwrap();
        ctl.poll_at(Instant::now());
        assert_eq!(ctl.model.patterns[1].status, Status::Playing);
        ctl.apply(Intent::RemoveNode { node: second }).unwrap();
        ctl.apply(Intent::Undo).unwrap();
        let order: Vec<NodeId> = ctl.model.patterns.iter().map(|pattern| pattern.node).collect();
        assert_eq!(order, [first, second, third], "at its place");
        assert_eq!(ctl.model.patterns[1].status, Status::Stopped, "a box that comes back is a new box to the audio thread, and is stopped");
    }

    #[test]
    fn a_setting_of_one_box_is_not_merged_with_the_same_setting_of_another_box_that_comes_soon_after() {
        let (_engine, mut ctl, _) = controller(PATTERNS);
        let (bass, plain) = (box_node(&ctl, 0), box_node(&ctl, 1));
        set_field(&mut ctl, bass, PatternBoxField::Looping { on: false }).unwrap();
        set_field(&mut ctl, plain, PatternBoxField::Looping { on: false }).unwrap();
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!((ctl.model.patterns[0].looping, ctl.model.patterns[1].looping), (false, true), "only the second box's change is undone");
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!((ctl.model.patterns[0].looping, ctl.model.patterns[1].looping), (true, true));
        assert_eq!(undo_label(&ctl), None);
    }

    #[test]
    fn a_length_that_a_box_has_after_it_is_held_is_no_change_though_the_length_that_was_asked_for_is_another() {
        let (engine, mut ctl, clock) = controller(PATTERNS);
        let bass = box_node(&ctl, 0);
        set_field(&mut ctl, bass, PatternBoxField::LengthBeats { beats: 1000.0 }).unwrap();
        assert_eq!(ctl.model.patterns[0].length_beats, 256.0);
        clock.advance(Duration::from_secs(5));
        told(&engine);
        let steps = ctl.history_state();
        set_field(&mut ctl, bass, PatternBoxField::LengthBeats { beats: 300.0 }).unwrap();
        assert_eq!((ctl.history_state(), told(&engine)), (steps, Vec::new()), "256 is what 300 comes to: no step, and nothing told");
        // (A step that is made has the label of the one before it, so the labels do not tell: the one Undo that there is takes the change of 1000 back, to the length that the box had.)
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!(ctl.model.patterns[0].length_beats, 8.0, "the one step that there is: the 1000 that came to 256");
        assert_eq!(undo_label(&ctl), None);
    }

    #[test]
    fn undo_is_refused_when_the_node_that_it_would_put_back_is_there_already() {
        let (_engine, mut ctl, _) = controller(PATTERNS);
        let bass = box_node(&ctl, 0);
        ctl.apply(Intent::RemoveNode { node: bass }).unwrap();
        // (A box that is made behind the history's back under the same node: a session that is changed without it.)
        ctl.model.patterns.push(PatternBox::new(bass, 5, "Again".to_string()));
        let error = ctl.apply(Intent::Undo).unwrap_err();
        assert_eq!(error, IntentError::Invalid(format!("node {} is there already", bass.0)));
        assert_eq!(ctl.model.patterns.len(), 2, "nothing was put back over it");
    }

    #[test]
    fn a_box_is_placed_like_any_node_and_its_place_is_in_the_file_and_stays_through_undo() {
        let (_engine, mut ctl, _) = controller(PATTERNS);
        let plain = box_node(&ctl, 1);
        assert_eq!(ctl.model.ui.get(&plain), None);
        let place = crate::session::NodeUi { x: 120.0, y: 80.5 };
        ctl.apply(Intent::SetNodeUi { node: plain, ui: Some(place) }).unwrap();
        assert_eq!(ctl.model.ui.get(&plain), Some(&place));
        let (instruments, groups, cables, sources) = crate::app::save::to_save(&ctl.model);
        let saved = crate::session::to_toml(&instruments, &groups, &cables, &sources, None, ctl.model.master_ui(), ctl.model.bpm_to_save()).unwrap();
        assert!(saved.contains("[[pattern]]\nname = \"Pattern\"\nlength_beats = 4.0\nlooping = true\nsync = \"own\"\nui = { x = 120, y = 80.5 }\n"), "{saved}");
        ctl.apply(Intent::RemoveNode { node: plain }).unwrap();
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!(ctl.model.ui.get(&plain), Some(&place), "the place of a box that comes back is where it was");
    }

    #[test]
    fn every_intent_that_names_a_node_and_is_not_for_a_box_is_refused_when_it_names_a_box_and_changes_nothing_and_tells_the_audio_thread_nothing() {
        use crate::app::graph::NoteRange;
        use crate::app::intent::{Gain, ModulatorField, ModulatorKind, PatternField};
        let (engine, mut ctl, _) = controller(PATTERNS);
        let (bass, plain) = (box_node(&ctl, 0), box_node(&ctl, 1));
        told(&engine);
        let (rev, patterns, cables) = (ctl.rev, ctl.model.patterns.clone(), ctl.model.event_cables.clone());
        let audio_in = Endpoint { node: bass, port: Port::AudioIn };
        let audio_out = Endpoint { node: bass, port: Port::AudioOut };
        let rate = Endpoint { node: bass, port: Port::Rate };
        let asks = vec![
            Intent::SetParam { node: bass, param: 0, value: 0.5 },
            Intent::SetGain { node: bass, gain: Gain::Volume, value: 0.5 },
            Intent::SetGain { node: bass, gain: Gain::Mix, value: 0.5 },
            Intent::SetGain { node: bass, gain: Gain::GroupVolume, value: 0.5 },
            Intent::LoadPreset { node: bass, preset: "x".to_string() },
            Intent::SetModulatorField { node: bass, field: ModulatorField::Rate, value: 1.0 },
            Intent::AddFreeModulator { kind: ModulatorKind::Lfo, hears: Some(bass) },
            Intent::AddModEdge { modulator: bass, to: audio_in },
            Intent::Modulate { to: rate, kind: ModulatorKind::Lfo, depth: None },
            Intent::AddCable { from: audio_out, to: Endpoint { node: NodeId::MASTER, port: Port::AudioIn }, gain: None },
            Intent::AddCable { from: Endpoint { node: lane_node(&ctl, 0), port: Port::AudioOut }, to: audio_in, gain: None },
            Intent::AddEffect { parent: bass, plugin: "builtin:reverb".to_string() },
            Intent::ReplaceInstrument { node: bass, plugin: "builtin:osc".to_string() },
            Intent::ClearInstrument { node: bass },
            Intent::SetRange { node: bass, range: Some(NoteRange { low: 0, high: 60 }) },
            Intent::SetMute { node: bass, muted: true },
            Intent::SetSolo { node: bass, solo: true },
            Intent::SetRoute { node: bass, route: crate::session::Route::None },
            Intent::SetModulatorEar { node: bass, ear: None },
            Intent::SetFeed { node: bass, feed: crate::session::Feed::None },
            Intent::SetRecording { node: bass, recording: true },
            Intent::ClearPattern { node: bass },
            Intent::SetPatternField { node: bass, field: PatternField::Length, value: 2.0 },
            Intent::SetPinned { node: bass, params: vec![0] },
            Intent::SetSteps { node: bass, values: vec![0.0] },
            Intent::AddGroup { members: vec![bass] },
            Intent::SetLaneGroup { node: bass, group: None },
            Intent::RenameGroup { node: bass, name: "x".to_string() },
            Intent::RenameMacro { node: bass, name: "x".to_string() },
            Intent::ExposeMacro { node: bass, expose: true },
            Intent::CollapseIntoModule { nodes: vec![bass, plain], name: "x".to_string(), ui: None },
            Intent::UngroupModule { node: bass },
            Intent::MoveEffect { node: bass, index: 0 },
            Intent::SaveModule { node: bass, name: "x".to_string() },
            Intent::OpenEditor { node: bass },
        ];
        for ask in asks {
            let error = ctl.apply(ask.clone()).expect_err(&format!("{ask:?} is refused"));
            assert!(!error.to_string().is_empty(), "{ask:?}");
        }
        assert_eq!((ctl.rev, ctl.model.patterns.clone(), ctl.model.event_cables.clone(), ctl.dirty), (rev, patterns, cables, false));
        assert_eq!(told(&engine), Vec::<String>::new());
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_transport_is_told_to_the_subscribers_as_a_revision_with_no_event() {
        let (_engine, mut ctl, _) = controller(PATTERNS);
        let heard: Arc<Mutex<Vec<Applied>>> = Arc::default();
        let log = heard.clone();
        ctl.subscribe(Box::new(move |applied: &Applied| log.lock().unwrap().push(applied.clone())));
        let bass = box_node(&ctl, 0);
        let applied = ctl.apply(Intent::PatternBoxTransport { node: bass, action: PatternBoxAction::Play }).unwrap();
        assert_eq!(*heard.lock().unwrap(), std::slice::from_ref(&applied), "the clients are told that a revision is used, and nothing else");
        assert!(applied.events.is_empty());
    }

    #[test]
    fn a_change_that_a_notification_made_is_a_change_though_the_notifications_after_it_made_none() {
        let (engine, mut ctl, _) = controller(PATTERNS);
        let bass = box_node(&ctl, 0);
        engine.box_tx.send(BoxNotification::Status { node: bass, status: Status::Recording }).unwrap();
        engine.box_tx.send(BoxNotification::Status { node: NodeId(900), status: Status::Playing }).unwrap();
        engine.box_tx.send(BoxNotification::Recorded { node: NodeId(900), notes: Vec::new(), plans: 1 }).unwrap();
        assert!(ctl.poll_at(Instant::now()).model_changed);
        assert_eq!(ctl.model.patterns[0].status, Status::Recording);
    }

    // -- the web client's fixture --

    /// The sessions that the fixture of what pattern boxes come to is made of, and the intents tried on each (each on a fresh controller): "boxes" is `PATTERNS` (the keys and a device, two boxes, two lanes and the cables of notes
    /// between them: from the keys to the first box, from the first box to the first lane and from the second box to the second lane) with an ask of every kind that a box takes and of every kind that it refuses; "bare" has
    /// no source and no box; "full" has the sixteen boxes that a session can have.
    fn pattern_box_fixture_cases() -> Vec<(&'static str, String, Vec<Intent>)> {
        use crate::app::graph::{NoteRange, PatternNote};
        use crate::app::intent::{Gain, ModulatorField, ModulatorKind, PatternField};
        let (_engine, base, _) = controller(PATTERNS);
        let (bass, plain) = (box_node(&base, 0), box_node(&base, 1));
        let (keys, device) = (base.model.sources[0].node, base.model.sources[1].node);
        let (lane0, lane1) = (lane_node(&base, 0), lane_node(&base, 1));
        let cables: Vec<EdgeId> = base.model.event_cables.iter().map(|cable| cable.id).collect();
        let (master, ghost) = (NodeId::MASTER, NodeId(900));
        let out = |node: NodeId| Endpoint { node, port: Port::EventsOut };
        let into = |node: NodeId| Endpoint { node, port: Port::EventsIn };
        let cable = |from: Endpoint, to: Endpoint| Intent::AddCable { from, to, gain: None };
        let field = |node: NodeId, field: PatternBoxField| Intent::SetPatternBoxField { node, field };
        let notes = |node: NodeId, notes: Vec<PatternNote>| Intent::SetPatternBoxNotes { node, notes };
        let note = |start: f32, length: f32, note: u8, velocity: u8| PatternNote { start, length, note, velocity };
        let transport = |node: NodeId, action: PatternBoxAction| Intent::PatternBoxTransport { node, action };
        let audio_in = Endpoint { node: bass, port: Port::AudioIn };
        let audio_out = Endpoint { node: bass, port: Port::AudioOut };
        let rate = Endpoint { node: bass, port: Port::Rate };
        let mut boxes = vec![Intent::AddPattern];
        // The settings: each is set to what it is not and to what it is, to what is too long or too short and to what is no name, on a box that has it and on one that has not, and on what is no box.
        let (longest, too_long) = ("x".repeat(crate::app::model::MAX_NAME), "x".repeat(crate::app::model::MAX_NAME + 1));
        for (node, name) in [(bass, "Lead"), (bass, "  Lead  "), (bass, ""), (bass, "Bass"), (plain, "Pad"), (bass, longest.as_str()), (bass, too_long.as_str()), (bass, "a\u{1}b")] {
            boxes.push(field(node, PatternBoxField::Name { name: name.to_string() }));
        }
        for beats in [2.5, 8.0, 1000.0, 0.0, -3.0, 0.25, 256.0, 0.1] {
            boxes.push(field(bass, PatternBoxField::LengthBeats { beats }));
        }
        boxes.push(field(plain, PatternBoxField::LengthBeats { beats: 4.0 }));
        boxes.push(field(plain, PatternBoxField::LengthBeats { beats: 6.0 }));
        for on in [false, true] {
            boxes.extend([
                field(bass, PatternBoxField::Looping { on }),
                field(plain, PatternBoxField::Looping { on }),
                field(bass, PatternBoxField::Overdub { on }),
                field(plain, PatternBoxField::Overdub { on }),
                field(bass, PatternBoxField::Click { on }),
                field(plain, PatternBoxField::Click { on }),
            ]);
        }
        for sync in [PatternSync::Host, PatternSync::Own] {
            boxes.extend([field(bass, PatternBoxField::Sync { sync }), field(plain, PatternBoxField::Sync { sync })]);
        }
        for node in [lane0, keys, device, master, ghost] {
            boxes.extend([field(node, PatternBoxField::Looping { on: false }), field(node, PatternBoxField::Name { name: "x".to_string() })]);
        }
        // The notes: cleared, the same, as a recording would leave them (unsorted, a velocity that is none, a pitch that is no pitch, a length below nothing) and set on the box that has none.
        boxes.extend([
            Intent::ClearPatternBox { node: bass },
            Intent::ClearPatternBox { node: plain },
            Intent::ClearPatternBox { node: lane0 },
            Intent::ClearPatternBox { node: ghost },
            notes(bass, Vec::new()),
            notes(bass, vec![note(0.0, 1.0, 36, 100), note(2.0, 0.5, 43, 90)]),
            notes(bass, vec![note(2.0, 1.0, 60, 0), note(0.0, 1.0, 200, 100), note(1.0, 0.5, 62, 90), note(1.0, 0.25, 61, 255), note(-1.0, -2.0, 64, 100)]),
            notes(plain, vec![note(0.5, 0.5, 72, 127)]),
            notes(plain, vec![note(0.5, 0.5, 72, 127), note(0.5, 0.5, 72, 127), note(0.0, 0.0, 12, 1)]),
            notes(keys, Vec::new()),
            notes(ghost, Vec::new()),
        ]);
        // The transport: what a box is told to do changes nothing that the controller keeps; what is no box is refused.
        for action in [PatternBoxAction::Record, PatternBoxAction::FinishRecording, PatternBoxAction::Play, PatternBoxAction::Stop] {
            boxes.extend([transport(bass, action), transport(plain, action), transport(lane0, action), transport(ghost, action)]);
        }
        // The cables of notes: into a box from a source, from a box to a lane, and every way that is not.
        boxes.extend([
            cable(out(keys), into(plain)),
            cable(out(device), into(bass)),
            cable(out(device), into(plain)),
            cable(out(keys), into(bass)),
            cable(out(bass), into(lane1)),
            cable(out(plain), into(lane0)),
            cable(out(bass), into(lane0)),
            cable(out(plain), into(lane1)),
            cable(out(bass), into(plain)),
            cable(out(plain), into(bass)),
            cable(out(bass), into(bass)),
            cable(out(lane0), into(bass)),
            cable(out(lane0), into(lane1)),
            cable(out(keys), into(master)),
            cable(out(bass), into(master)),
            cable(out(master), into(bass)),
            cable(out(keys), into(lane0)),
            cable(out(keys), into(ghost)),
            cable(out(ghost), into(bass)),
            cable(out(ghost), into(lane0)),
            cable(Endpoint { node: keys, port: Port::AudioOut }, into(bass)),
            cable(out(keys), Endpoint { node: bass, port: Port::AudioIn }),
            cable(Endpoint { node: lane0, port: Port::AudioOut }, Endpoint { node: bass, port: Port::AudioIn }),
            Intent::AddCable { from: out(keys), to: into(bass), gain: Some(0.5) },
            Intent::AddCable { from: out(keys), to: into(plain), gain: Some(0.5) },
        ]);
        // Cables taken out, and nodes taken out with the cables that start or end at them: a lane that no cable ends at hears every source again, and a box with none records every source.
        for edge in &cables {
            boxes.push(Intent::RemoveEdge { edge: *edge });
        }
        boxes.push(Intent::RemoveEdge { edge: EdgeId(900) });
        for node in [bass, plain, keys, device, lane0, lane1, ghost, master] {
            boxes.push(Intent::RemoveNode { node });
        }
        // Moving a box, which is a node like another.
        boxes.extend([
            Intent::SetNodeUi { node: bass, ui: Some(crate::session::NodeUi { x: 12.3, y: 45.6 }) },
            Intent::SetNodeUi { node: plain, ui: Some(crate::session::NodeUi { x: 1.0, y: 2.0 }) },
            Intent::SetNodeUi { node: bass, ui: None },
            Intent::SetNodeUi { node: plain, ui: None },
        ]);
        // What a box is not: every kind of ask that names a node and is not for a box is refused when it names one (and a module is not made of one).
        let asks = vec![
            Intent::SetParam { node: bass, param: 0, value: 0.5 },
            Intent::SetGain { node: bass, gain: Gain::Volume, value: 0.5 },
            Intent::SetGain { node: bass, gain: Gain::Mix, value: 0.5 },
            Intent::SetGain { node: bass, gain: Gain::GroupVolume, value: 0.5 },
            Intent::LoadPreset { node: bass, preset: "x".to_string() },
            Intent::SetModulatorField { node: bass, field: ModulatorField::Rate, value: 1.0 },
            Intent::AddFreeModulator { kind: ModulatorKind::Lfo, hears: Some(bass) },
            Intent::AddModEdge { modulator: bass, to: audio_in },
            Intent::Modulate { to: rate, kind: ModulatorKind::Lfo, depth: None },
            Intent::AddCable { from: audio_out, to: Endpoint { node: master, port: Port::AudioIn }, gain: None },
            Intent::AddCable { from: Endpoint { node: lane0, port: Port::AudioOut }, to: audio_in, gain: None },
            Intent::AddEffect { parent: bass, plugin: "builtin:reverb".to_string() },
            Intent::ReplaceInstrument { node: bass, plugin: "builtin:osc".to_string() },
            Intent::ClearInstrument { node: bass },
            Intent::SetRange { node: bass, range: Some(NoteRange { low: 0, high: 60 }) },
            Intent::SetMute { node: bass, muted: true },
            Intent::SetSolo { node: bass, solo: true },
            Intent::SetRoute { node: bass, route: crate::session::Route::None },
            Intent::SetModulatorEar { node: bass, ear: None },
            Intent::SetFeed { node: bass, feed: crate::session::Feed::None },
            Intent::SetRecording { node: bass, recording: true },
            Intent::ClearPattern { node: bass },
            Intent::SetPatternField { node: bass, field: PatternField::Length, value: 2.0 },
            Intent::SetPinned { node: bass, params: vec![0] },
            Intent::SetSteps { node: bass, values: vec![0.0] },
            Intent::AddGroup { members: vec![bass] },
            Intent::SetLaneGroup { node: bass, group: None },
            Intent::RenameGroup { node: bass, name: "x".to_string() },
            Intent::RenameMacro { node: bass, name: "x".to_string() },
            Intent::ExposeMacro { node: bass, expose: true },
            Intent::CollapseIntoModule { nodes: vec![bass, plain], name: "x".to_string(), ui: None },
            Intent::CollapseIntoModule { nodes: vec![lane0, bass], name: "x".to_string(), ui: None },
            Intent::UngroupModule { node: bass },
            Intent::MoveEffect { node: bass, index: 0 },
            Intent::OpenEditor { node: bass },
        ];
        boxes.extend(asks);
        // A session that has no source and no box: the first box is called "Pattern 1", and there is nothing for a cable of notes to start at or to end at but a lane.
        let (_engine, bare_base, _) = controller(TWO_LANES);
        let bare = vec![
            Intent::AddPattern,
            cable(out(lane_node(&bare_base, 0)), into(lane_node(&bare_base, 1))),
            Intent::SetPatternBoxField { node: lane_node(&bare_base, 0), field: PatternBoxField::Looping { on: false } },
            transport(lane_node(&bare_base, 0), PatternBoxAction::Play),
        ];
        let full = "[[pattern]]\n\n".repeat(crate::source::MAX_BOXES) + "[[instrument]]\nplugin = \"builtin:osc\"\n";
        vec![("boxes", PATTERNS.to_string(), boxes), ("bare", TWO_LANES.to_string(), bare), ("full", full, vec![Intent::AddPattern])]
    }

    /// The sessions of `pattern_box_fixture_cases` and what the controller makes of each intent tried on them, as the web client's tests read it (`web/src/client/fixtures/patternBoxes.json`): they pin the mock engine's to the
    /// controller's, intent by intent. A refusal is the controller's words; the rest is the events the intent made (stamped), whether it changed the structure and what the session comes to after it (`testutil::shape_of`: the boxes are in it, with
    /// their lengths, settings and notes, but not what they are doing, which is the audio thread's and which a controller with no engine does not hear of). `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` rewrites it.
    #[test]
    fn the_web_clients_fixture_of_what_pattern_boxes_come_to_is_current() {
        let mut cases = Vec::new();
        for (name, session, intents) in pattern_box_fixture_cases() {
            let (_engine, base, _) = controller(&session);
            let mut snapshot = testutil::wire(&base.snapshot());
            // The layout's fingerprint comes from the hasher, which the standard library does not promise to keep the same.
            snapshot["telemetry"]["rev"] = serde_json::json!(1);
            let mut rows = Vec::new();
            for intent in intents {
                let (_engine, mut fresh, _) = controller(&session);
                let what = testutil::wire(&intent);
                rows.push(match fresh.apply(intent.clone()) {
                    Err(error) => serde_json::json!({ "intent": what, "error": error.to_string() }),
                    Ok(applied) => serde_json::json!({
                        "intent": what,
                        "events": testutil::wire(&applied.stamped()),
                        "restructured": applied.restructured,
                        "after": testutil::shape_of(&fresh.snapshot()),
                    }),
                });
            }
            cases.push(serde_json::json!({ "name": name, "snapshot": snapshot, "rows": rows }));
        }
        let fresh = serde_json::to_string_pretty(&serde_json::json!({ "cases": cases })).unwrap() + "\n";
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/src/client/fixtures/patternBoxes.json");
        if std::env::var_os("TANG_UPDATE_TYPES").is_some() {
            std::fs::write(&path, &fresh).unwrap();
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            committed == fresh,
            "{} is not what pattern boxes come to now: run `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` and commit the result",
            path.display()
        );
    }
}
