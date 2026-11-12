//! Note splitters in the controller (P4i of `docs/patch-graph.md` §11.6): putting one in the session and taking one out, and what the audio thread is told of one (the splitter, with the note that it picks and the sources that it hears).
//!
//! A splitter has two sources of notes, the pick's and the rest's, with numbers of their own: its **slot** (`Source::splitter(slot, Output)`) is the lowest of the engine's sixteen that was free when the splitter was made, kept for as
//! long as it is in the session, so that what a lane has sounding from one of its outputs is never another splitter's. The lanes that hear an output, and the sources that a splitter hears, are worked out from the cables of notes
//! (`SessionModel::lane_sources`, `splitter_inputs`) and told to the audio thread when they are not what it was told last (`sync_lane_sources`, which the end of every intent that changes the structure calls). What a splitter does
//! with the notes that come in is the audio thread's (`notesplit`) and nothing of it is the model's: the model has the splitter's name, its pick and its cables, which is what is saved.

use crate::app::apply::{Done, invalid, splitter_title};
use crate::app::controller::Controller;
use crate::app::intent::{IntentError, SplitterField};
use crate::app::journal::Inverse;
use crate::app::model::{NodeId, NodePath, NoteSplitter};
use crate::notesplit::{Settings, Splitter};
use crate::plugin::chain::GraphCommand;
use crate::source::MAX_SPLITTERS;

impl Controller {
    /// Put a note splitter at the end of the session's splitters, under a node of its own, and tell the audio thread of it (`Intent::AddSplitter`): picking the lowest note, called "Splitter N". Refused, with nothing changed and no id used,
    /// when the session has `MAX_SPLITTERS`.
    pub(super) fn add_splitter(&mut self) -> Result<NodeId, IntentError> {
        let Some(slot) = self.model.free_splitter_slot() else {
            return Err(invalid(format!("a session has at most {MAX_SPLITTERS} note splitters")));
        };
        let node = self.model.alloc_node();
        let name = self.model.next_splitter_name();
        self.model.splitters.push(NoteSplitter::new(node, slot, name));
        self.send_splitter(self.model.splitters.len() - 1);
        Ok(node)
    }

    /// Take the note splitter at `index` out of the session, after the cables of notes that start or end at it (each written down, as a box's are), and tell the audio thread: the lanes that heard it let go of what it had sounding. The lanes
    /// that it fed hear what their other cables bring, or every source if they have none (`sync_lane_sources`, which `conclude` calls). False, changing nothing, if there is no such splitter.
    pub(super) fn remove_splitter(&mut self, index: usize) -> bool {
        let Some(node) = self.model.splitters.get(index).map(|splitter| splitter.node) else { return false };
        self.forget_the_history_unless_an_intent_asks();
        self.take_out_event_cables_at(&[node]);
        let splitter = self.model.splitters.remove(index);
        let _ = self.cmd_tx.send(GraphCommand::RemoveSplitter { node });
        self.sent_splitters.retain(|(told, _)| *told != node);
        self.took_out(Inverse::Splitter { index, splitter });
        true
    }

    /// Put a note splitter back among the splitters at `index`, as what undo takes out comes back: under the node it had, with its name and its pick, at the slot it had if that is free and the lowest that is if it is not (a splitter that was
    /// made since is not undone first only when the session was changed without the history). A session that has `MAX_SPLITTERS` already is not given another: the splitter is left out, and logged.
    pub(super) fn put_back_splitter(&mut self, index: usize, mut splitter: NoteSplitter) {
        if self.model.splitters.iter().any(|there| there.slot == splitter.slot) {
            let Some(slot) = self.model.free_splitter_slot() else {
                log::warn!("{:?} was not put back: the session has {MAX_SPLITTERS} note splitters", splitter.node);
                return;
            };
            splitter.slot = slot;
        }
        let place = self.model.insert_splitter_at(index, splitter);
        self.send_splitter(place);
    }

    /// What the audio thread's splitter is to be set to for the splitter at `index`, as the model and the open devices say: the note it picks, and the sources it hears (every input if no cable ends at it).
    pub(crate) fn splitter_settings(&self, index: usize) -> Settings {
        Settings { pick: self.model.splitters[index].pick, inputs: self.model.splitter_inputs(index, |kind| self.sources_of_kind(kind)) }
    }

    /// Tell the audio thread of the splitter at `index`, which is new to it: the splitter, with its settings.
    fn send_splitter(&mut self, index: usize) {
        let settings = self.splitter_settings(index);
        let splitter = &self.model.splitters[index];
        let node = splitter.node;
        let mut made = Splitter::new(splitter.slot as usize);
        made.set_settings(settings);
        let _ = self.cmd_tx.send(GraphCommand::AddSplitter { node, splitter: Box::new(made) });
        self.sent_splitters.retain(|(told, _)| *told != node);
        self.sent_splitters.push((node, settings));
    }

    /// Tell the audio thread what each splitter is set to where it is not what it was told last (`sent_splitters`): the settings are told whole, with the sources the splitter hears, which the cables and the devices that are open say. A
    /// splitter that was not told anything (one that a loaded session has) is told.
    pub(super) fn sync_splitter_settings(&mut self) {
        let mut now = Vec::with_capacity(self.model.splitters.len());
        for index in 0..self.model.splitters.len() {
            let node = self.model.splitters[index].node;
            let settings = self.splitter_settings(index);
            let told = self.sent_splitters.iter().find(|(told, _)| *told == node).map(|(_, settings)| *settings);
            if told != Some(settings) {
                let _ = self.cmd_tx.send(GraphCommand::SetSplitterSettings { node, settings });
            }
            now.push((node, settings));
        }
        self.sent_splitters = now;
    }

    /// The place of the note splitter whose node is `node`. Refused, in words, for a node that is another kind, and not found for one that is not there.
    fn splitter_index(&self, node: NodeId) -> Result<usize, IntentError> {
        match self.model.locate(node) {
            Some(NodePath::Splitter(index)) => Ok(index),
            Some(_) => Err(invalid("that node is not a note splitter")),
            None => Err(IntentError::NotFound(node)),
        }
    }

    /// `Intent::AddSplitter`.
    pub(super) fn add_splitter_intent(&mut self) -> Result<Done, IntentError> {
        self.add_splitter()?;
        Ok(Done::Restructured)
    }

    /// `Intent::SetSplitterField`: the setting is the model's, and what the audio thread has of it is told (the pick, as the splitter's settings, which the end of the intent sends).
    pub(super) fn set_splitter_field_intent(&mut self, node: NodeId, field: SplitterField) -> Result<Done, IntentError> {
        let index = self.splitter_index(node)?;
        match field {
            SplitterField::Name { name } => self.model.splitters[index].name = splitter_title(&name)?,
            SplitterField::Pick { pick } => self.model.splitters[index].pick = pick.into(),
        }
        Ok(Done::Restructured)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use super::*;
    use crate::app::graph::{Endpoint, NodeKind, Port, SourceInfo, SplitterPick};
    use crate::app::history::HistoryState;
    use crate::app::intent::{Applied, Intent};
    use crate::app::model::EdgeId;
    use crate::app::testutil::{self, Ports, SPLITTERS, StubEngine, load};
    use crate::notesplit::Pick;
    use crate::source::{Output, Source, SourceSet};

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

    /// What the controller has told the audio thread of the splitters and of what the lanes hear since the last look, a line for each command, in order.
    fn told(engine: &StubEngine) -> Vec<String> {
        const WORDS: [&str; 5] = ["AddSplitter", "RemoveSplitter", "SetSplitterSettings", "SetLaneSources", "ReleaseSource"];
        engine.drain().0.into_iter().filter(|command| WORDS.iter().any(|word| command.starts_with(word))).collect()
    }

    fn out(node: NodeId) -> Endpoint {
        Endpoint { node, port: Port::EventsOut }
    }

    fn out_rest(node: NodeId) -> Endpoint {
        Endpoint { node, port: Port::EventsOutRest }
    }

    fn into(node: NodeId) -> Endpoint {
        Endpoint { node, port: Port::EventsIn }
    }

    fn draw(ctl: &mut Controller, from: NodeId, to: NodeId) -> Result<Applied, IntentError> {
        ctl.apply(Intent::AddCable { from: out(from), to: into(to), gain: None })
    }

    fn draw_rest(ctl: &mut Controller, from: NodeId, to: NodeId) -> Result<Applied, IntentError> {
        ctl.apply(Intent::AddCable { from: out_rest(from), to: into(to), gain: None })
    }

    fn splitter_node(ctl: &Controller, index: usize) -> NodeId {
        ctl.model.splitters[index].node
    }

    fn lane_node(ctl: &Controller, index: usize) -> NodeId {
        ctl.model.instruments[index].node
    }

    fn undo_label(ctl: &Controller) -> Option<String> {
        ctl.history_state().undo
    }

    /// What `describe` says of the settings of a splitter (the note it picks, and the sources it hears: `all` or the numbers).
    fn settings_tail(pick: &str, inputs: &str) -> String {
        format!("pick={pick} inputs={inputs}")
    }

    /// What `describe` says of the command that sets a splitter.
    fn settings_line(node: NodeId, pick: &str, inputs: &str) -> String {
        format!("SetSplitterSettings node={} {}", node.0, settings_tail(pick, inputs))
    }

    /// What `describe` says of the command that puts a splitter in the graph: the numbers of its two outputs, and what it is set to.
    fn add_line(node: NodeId, first_output: usize, tail: &str) -> String {
        format!("AddSplitter node={} sources={first_output}/{} {tail}", node.0, first_output + 1)
    }

    fn set_field(ctl: &mut Controller, node: NodeId, field: SplitterField) -> Result<Applied, IntentError> {
        ctl.apply(Intent::SetSplitterField { node, field })
    }

    // -- a session that has splitters --

    #[test]
    fn a_loaded_session_has_its_splitters_at_the_slots_in_the_order_of_the_file_and_the_controller_tells_the_audio_thread_what_each_picks_and_hears_and_what_each_lane_hears() {
        let (engine, ctl, _) = controller(SPLITTERS);
        let (first, second) = (&ctl.model.splitters[0], &ctl.model.splitters[1]);
        assert_eq!((first.name.as_str(), first.slot, first.pick), ("Bass line", 0, Pick::Lowest));
        assert_eq!((second.name.as_str(), second.slot, second.pick), ("Splitter", 1, Pick::Highest), "a splitter that the file says little of has the name of one that is not called");
        assert_eq!(ctl.model.ui.get(&second.node).map(|ui| (ui.x, ui.y)), Some((300.0, 40.0)));
        // The first splitter hears the keys and the second the box and a device that is not plugged in; the lanes hear the outputs that cables from them end at: the pick (64) and the rest (65) of the first, the pick (66) of the second.
        assert_eq!(
            told(&engine),
            [
                "SetLaneSources inst=0 sources=[64]".to_string(),
                "SetLaneSources inst=1 sources=[65]".to_string(),
                "SetLaneSources inst=2 sources=[66]".to_string(),
                settings_line(first.node, "lowest", "[0]"),
                settings_line(second.node, "highest", "[48]"),
            ]
        );
        ctl.model.check_ids().unwrap();
        assert_eq!(ctl.model.locate(first.node), Some(NodePath::Splitter(0)));
        assert_eq!(ctl.model.locate(second.node), Some(NodePath::Splitter(1)));
    }

    #[test]
    fn a_session_with_no_splitter_is_told_nothing_of_splitters_whatever_is_done_to_it() {
        let (engine, mut ctl, _) = controller(TWO_LANES);
        assert_eq!(told(&engine), Vec::<String>::new());
        ctl.apply(Intent::AddSource { source: SourceInfo::Keys }).unwrap();
        let (keys, lane) = (ctl.model.sources[0].node, lane_node(&ctl, 0));
        draw(&mut ctl, keys, lane).unwrap();
        ctl.apply(Intent::AddPattern).unwrap();
        ctl.apply(Intent::Undo).unwrap();
        ctl.apply(Intent::Undo).unwrap();
        assert!(told(&engine).iter().all(|command| !command.contains("Splitter")), "no splitter is told of");
    }

    // -- adding a splitter --

    #[test]
    fn a_splitter_is_added_at_the_end_with_the_lowest_free_slot_and_name_picking_the_lowest_note_and_the_audio_thread_gets_it_hearing_every_input() {
        let (engine, mut ctl, _) = controller(TWO_LANES);
        assert_eq!(ctl.apply(Intent::AddSplitter), Ok(Applied::restructured(1)));
        let first = ctl.model.splitters[0].clone();
        assert_eq!((first.name.as_str(), first.slot, first.pick), ("Splitter 1", 0, Pick::Lowest));
        assert_eq!(ctl.model.locate(first.node), Some(NodePath::Splitter(0)));
        assert!(ctl.dirty, "the splitter is in the file when the session is saved");
        assert_eq!(told(&engine), [add_line(first.node, 64, &settings_tail("lowest", "all"))], "and no lane hears it until a cable says so");
        assert_eq!(ctl.apply(Intent::AddSplitter), Ok(Applied::restructured(2)));
        let second = ctl.model.splitters[1].clone();
        assert_eq!((second.name.as_str(), second.slot), ("Splitter 2", 1));
        assert_eq!(told(&engine), [add_line(second.node, 66, &settings_tail("lowest", "all"))]);
        assert_ne!(first.node, second.node);
        ctl.model.check_ids().unwrap();
        // A splitter that is gone leaves its slot and its name to the next: the lowest that is free.
        ctl.apply(Intent::RemoveNode { node: first.node }).unwrap();
        told(&engine);
        ctl.apply(Intent::AddSplitter).unwrap();
        let third = &ctl.model.splitters[1];
        assert_eq!((third.name.as_str(), third.slot), ("Splitter 1", 0), "the slot and the name of the splitter that went");
        assert_eq!(told(&engine), [add_line(third.node, 64, &settings_tail("lowest", "all"))]);
    }

    #[test]
    fn a_name_that_a_splitter_has_is_not_given_to_the_next_even_when_it_is_not_the_lowest_number() {
        let (_engine, mut ctl, _) = controller(TWO_LANES);
        ctl.apply(Intent::AddSplitter).unwrap();
        ctl.apply(Intent::AddSplitter).unwrap();
        let first = splitter_node(&ctl, 0);
        set_field(&mut ctl, first, SplitterField::Name { name: "Splitter 3".to_string() }).unwrap();
        ctl.apply(Intent::AddSplitter).unwrap();
        let names: Vec<&str> = ctl.model.splitters.iter().map(|splitter| splitter.name.as_str()).collect();
        assert_eq!(names, ["Splitter 3", "Splitter 2", "Splitter 1"], "the third is the lowest number that no splitter is called");
        ctl.apply(Intent::AddSplitter).unwrap();
        assert_eq!(ctl.model.splitters[3].name, "Splitter 4");
    }

    #[test]
    fn a_session_has_sixteen_splitters_at_the_most_and_the_next_is_refused_with_nothing_changed_nothing_told_and_no_id_used() {
        let (engine, mut ctl, _) = controller(TWO_LANES);
        for n in 1..=crate::source::MAX_SPLITTERS {
            ctl.apply(Intent::AddSplitter).unwrap_or_else(|why| panic!("splitter {n}: {why}"));
        }
        let slots: Vec<u8> = ctl.model.splitters.iter().map(|splitter| splitter.slot).collect();
        assert_eq!(slots, (0..16).collect::<Vec<u8>>());
        assert_eq!(ctl.model.splitters[15].name, "Splitter 16");
        ctl.model.check_ids().unwrap();
        told(&engine);
        let (rev, nodes) = (ctl.rev, ctl.model.nodes().count());
        let why = IntentError::Invalid("a session has at most 16 note splitters".to_string());
        assert_eq!(ctl.apply(Intent::AddSplitter), Err(why));
        assert_eq!((ctl.rev, ctl.model.nodes().count(), ctl.model.splitters.len()), (rev, nodes, 16));
        assert_eq!(told(&engine), Vec::<String>::new());
        // No id was used by it: the splitter that is added when one has gone has the next id after the ones there were.
        let top = ctl.model.nodes().map(|(node, _)| node.0).max().unwrap();
        let last = splitter_node(&ctl, 15);
        ctl.apply(Intent::RemoveNode { node: last }).unwrap();
        ctl.apply(Intent::AddSplitter).unwrap();
        assert_eq!(ctl.model.splitters[15].node, NodeId(top + 1));
        assert_eq!(ctl.model.splitters[15].slot, 15, "the slot that was free");
    }

    #[test]
    fn adding_a_splitter_is_a_step_of_the_history_that_undo_takes_out_and_redo_puts_back_under_the_same_node_and_slot() {
        let (engine, mut ctl, _) = controller(TWO_LANES);
        ctl.apply(Intent::AddSplitter).unwrap();
        let (node, slot) = (splitter_node(&ctl, 0), ctl.model.splitters[0].slot);
        assert_eq!(undo_label(&ctl).as_deref(), Some("add a splitter"));
        told(&engine);
        ctl.apply(Intent::Undo).unwrap();
        assert!(ctl.model.splitters.is_empty());
        assert_eq!(told(&engine), [format!("RemoveSplitter node={}", node.0)]);
        assert_eq!(ctl.history_state(), HistoryState { undo: None, redo: Some("add a splitter".to_string()) });
        ctl.apply(Intent::Redo).unwrap();
        assert_eq!((splitter_node(&ctl, 0), ctl.model.splitters[0].slot, ctl.model.splitters.len()), (node, slot, 1));
        assert_eq!(told(&engine), [add_line(node, 64, &settings_tail("lowest", "all"))]);
        ctl.model.check_ids().unwrap();
    }

    // -- taking a splitter out --

    #[test]
    fn a_splitter_that_is_taken_out_takes_its_cables_both_ways_and_the_lanes_that_heard_only_it_hear_every_source_again() {
        let (engine, mut ctl, _) = controller(SPLITTERS);
        told(&engine);
        let first = splitter_node(&ctl, 0);
        assert_eq!(ctl.model.event_cables.len(), 6);
        ctl.apply(Intent::RemoveNode { node: first }).unwrap();
        assert_eq!(ctl.model.splitters.len(), 1);
        assert_eq!(ctl.model.event_cables.len(), 3, "the cable from the keys into the splitter and the two cables from its outputs are gone, and the second splitter's stay");
        assert_eq!(told(&engine), [format!("RemoveSplitter node={}", first.0), "SetLaneSources inst=0 sources=all".to_string(), "SetLaneSources inst=1 sources=all".to_string()]);
        assert_eq!(undo_label(&ctl).as_deref(), Some("remove Bass line"));
        assert!(ctl.model.locate(first).is_none());
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn undo_puts_a_splitter_back_with_its_cables_name_and_pick_under_its_node_at_its_place_and_the_audio_thread_is_told_of_it_again() {
        let (engine, mut ctl, _) = controller(SPLITTERS);
        let first = splitter_node(&ctl, 0);
        let before = (ctl.model.splitters.clone(), ctl.model.event_cables.clone());
        assert_eq!(before.1[4].output, Output::Rest, "the cable from the second output is in the fixture");
        ctl.apply(Intent::RemoveNode { node: first }).unwrap();
        told(&engine);
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!((ctl.model.splitters.clone(), ctl.model.event_cables.clone()), before, "all of it, as it was: the cable from the rest comes back from the rest, and not from the pick");
        assert_eq!(
            told(&engine),
            [
                add_line(first, 64, &settings_tail("lowest", "all")),
                "SetLaneSources inst=0 sources=[64]".to_string(),
                "SetLaneSources inst=1 sources=[65]".to_string(),
                settings_line(first, "lowest", "[0]"),
            ],
            "the splitter, what the lanes hear, and then what the splitter hears, which is known when its cables are back"
        );
        ctl.model.check_ids().unwrap();
        // Redo takes it out again, as the first time.
        ctl.apply(Intent::Redo).unwrap();
        assert_eq!(ctl.model.splitters.len(), 1);
        assert_eq!(told(&engine), [format!("RemoveSplitter node={}", first.0), "SetLaneSources inst=0 sources=all".to_string(), "SetLaneSources inst=1 sources=all".to_string()]);
    }

    #[test]
    fn a_splitter_that_is_put_back_when_its_slot_was_taken_meanwhile_has_the_lowest_slot_that_is_free_and_the_lanes_hear_that_one() {
        let (engine, mut ctl, _) = controller(SPLITTERS);
        let first = splitter_node(&ctl, 0);
        ctl.apply(Intent::RemoveNode { node: first }).unwrap();
        // (A splitter that is made without the history, as a session that is changed behind its back: it takes the slot that is free.)
        ctl.add_splitter().unwrap();
        assert_eq!(ctl.model.splitters.iter().map(|splitter| splitter.slot).collect::<Vec<_>>(), [1, 0]);
        told(&engine);
        ctl.apply(Intent::Undo).unwrap();
        let back = ctl.model.splitter(first).expect("the splitter is back");
        assert_eq!(back.slot, 2, "the lowest that is free");
        assert_eq!(
            told(&engine),
            [
                add_line(first, 68, &settings_tail("lowest", "all")),
                "SetLaneSources inst=0 sources=[68]".to_string(),
                "SetLaneSources inst=1 sources=[69]".to_string(),
                settings_line(first, "lowest", "[0]"),
            ],
            "the splitter under the sources it has now, and the lanes told those: the pick and the rest of the third splitter"
        );
        assert_eq!(ctl.model.lane_sources(0, |_| SourceSet::NONE), SourceSet::NONE.with(Source::splitter(2, Output::Pick)), "the lane hears the pick where it is now");
        assert_eq!(ctl.model.lane_sources(1, |_| SourceSet::NONE), SourceSet::NONE.with(Source::splitter(2, Output::Rest)));
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_splitter_that_picks_the_highest_note_is_put_back_picking_it_and_is_told_so_before_its_cables_are_back() {
        let (engine, mut ctl, _) = controller(SPLITTERS);
        let second = splitter_node(&ctl, 1);
        ctl.apply(Intent::RemoveNode { node: second }).unwrap();
        told(&engine);
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!(ctl.model.splitter(second).map(|splitter| splitter.pick), Some(Pick::Highest));
        // (The splitter is made with what the model says it picks, and what it hears is every input until its cables, which come back after it, are: the engine is told that when they are.)
        assert_eq!(
            told(&engine),
            [
                add_line(second, 66, &settings_tail("highest", "all")),
                "SetLaneSources inst=2 sources=[66]".to_string(),
                settings_line(second, "highest", "[48]"),
            ]
        );
        assert_eq!(ctl.splitter_settings(1).pick, Pick::Highest);
    }

    #[test]
    fn a_splitter_that_cannot_be_put_back_because_the_session_has_sixteen_is_left_out_and_so_are_the_cables_that_were_between_it_and_the_rest() {
        let (_engine, mut ctl, _) = controller(SPLITTERS);
        let first = splitter_node(&ctl, 0);
        ctl.apply(Intent::RemoveNode { node: first }).unwrap();
        // (One more than there is room for: a splitter that is let in past the limit is a failure of this test, and not a loop that has no end.)
        for _ in 0..=crate::source::MAX_SPLITTERS {
            if ctl.add_splitter().is_err() {
                break;
            }
        }
        assert_eq!(ctl.model.splitters.len(), crate::source::MAX_SPLITTERS);
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!(ctl.model.splitters.len(), crate::source::MAX_SPLITTERS);
        assert!(ctl.model.splitter(first).is_none());
        assert_eq!(ctl.model.event_cables.len(), 3, "only the second splitter's cables: those of the splitter that is not back are not");
        ctl.model.check_ids().unwrap();
    }

    // -- the settings of a splitter --

    #[test]
    fn every_setting_of_a_splitter_is_the_models_and_the_pick_is_told_to_the_audio_thread_and_each_is_a_step_in_the_words_of_the_setting_that_undo_puts_back() {
        let (engine, mut ctl, clock) = controller(SPLITTERS);
        let first = splitter_node(&ctl, 0);
        told(&engine);
        let rev = ctl.rev;
        assert_eq!(set_field(&mut ctl, first, SplitterField::Pick { pick: SplitterPick::Highest }), Ok(Applied::restructured(rev + 1)));
        assert_eq!(told(&engine), [settings_line(first, "highest", "[0]")], "the audio thread is told the whole of what the splitter is set to");
        assert_eq!(undo_label(&ctl).as_deref(), Some("let Bass line pick the highest note"));
        assert_eq!(ctl.model.splitters[0].pick, Pick::Highest);
        assert!(ctl.dirty);
        // (A change that comes after a pause is a step of its own; one that comes at once would be merged with this.)
        clock.advance(Duration::from_secs(30));
        set_field(&mut ctl, first, SplitterField::Pick { pick: SplitterPick::Lowest }).unwrap();
        assert_eq!(told(&engine), [settings_line(first, "lowest", "[0]")]);
        assert_eq!(undo_label(&ctl).as_deref(), Some("let Bass line pick the lowest note"));
        // Undo puts each back, last first, and the audio thread is told each time.
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!((ctl.model.splitters[0].pick, told(&engine)), (Pick::Highest, vec![settings_line(first, "highest", "[0]")]));
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!((ctl.model.splitters[0].pick, told(&engine)), (Pick::Lowest, vec![settings_line(first, "lowest", "[0]")]), "the splitter as the file had it");
        assert_eq!(undo_label(&ctl), None);
    }

    #[test]
    fn a_setting_that_is_what_it_is_asked_to_be_changes_nothing_keeps_no_step_and_tells_the_audio_thread_nothing() {
        let (engine, mut ctl, _) = controller(SPLITTERS);
        let first = splitter_node(&ctl, 0);
        told(&engine);
        for field in [SplitterField::Pick { pick: SplitterPick::Lowest }, SplitterField::Name { name: "Bass line".to_string() }, SplitterField::Name { name: "  Bass line \t".to_string() }] {
            set_field(&mut ctl, first, field.clone()).unwrap_or_else(|why| panic!("{field:?}: {why}"));
            assert_eq!(undo_label(&ctl), None, "{field:?}");
            assert_eq!(told(&engine), Vec::<String>::new(), "{field:?}");
        }
    }

    #[test]
    fn a_splitter_is_renamed_by_the_rule_of_a_group_and_a_name_of_nothing_is_the_name_of_a_splitter_that_is_not_called() {
        let (engine, mut ctl, _) = controller(SPLITTERS);
        let first = splitter_node(&ctl, 0);
        told(&engine);
        set_field(&mut ctl, first, SplitterField::Name { name: "  Chord bass \t".to_string() }).unwrap();
        assert_eq!(ctl.model.splitters[0].name, "Chord bass");
        assert_eq!(undo_label(&ctl).as_deref(), Some("rename Bass line"));
        assert_eq!(told(&engine), Vec::<String>::new(), "the audio thread has no use for a name");
        set_field(&mut ctl, first, SplitterField::Name { name: " \t ".to_string() }).unwrap();
        assert_eq!(ctl.model.splitters[0].name, "Splitter");
        let longest = "x".repeat(crate::app::model::MAX_NAME);
        set_field(&mut ctl, first, SplitterField::Name { name: longest.clone() }).unwrap();
        assert_eq!(ctl.model.splitters[0].name, longest);
        let rev = ctl.rev;
        assert_eq!(set_field(&mut ctl, first, SplitterField::Name { name: format!("{longest}x") }), Err(IntentError::Invalid("a name is at most 64 characters".to_string())));
        assert_eq!(set_field(&mut ctl, first, SplitterField::Name { name: "A\nB".to_string() }), Err(IntentError::Invalid("a name has no control characters".to_string())));
        assert_eq!((ctl.rev, ctl.model.splitters[0].name.as_str()), (rev, longest.as_str()));
    }

    #[test]
    fn what_is_asked_of_a_node_that_is_no_splitter_or_is_not_there_is_refused_for_every_intent_of_a_splitter_and_changes_nothing() {
        let (engine, mut ctl, _) = controller(SPLITTERS);
        let lane = lane_node(&ctl, 0);
        told(&engine);
        let rev = ctl.rev;
        for (node, error) in [(lane, IntentError::Invalid("that node is not a note splitter".to_string())), (NodeId(900), IntentError::NotFound(NodeId(900)))] {
            let ask = Intent::SetSplitterField { node, field: SplitterField::Pick { pick: SplitterPick::Highest } };
            assert_eq!(ctl.apply(ask.clone()), Err(error.clone()), "{ask:?}");
            let ask = Intent::SetSplitterField { node, field: SplitterField::Name { name: "x".to_string() } };
            assert_eq!(ctl.apply(ask.clone()), Err(error), "{ask:?}");
        }
        // (And a pattern box is not a splitter, nor a splitter a pattern box.)
        let wrong = IntentError::Invalid("that node is not a pattern box".to_string());
        let first = splitter_node(&ctl, 0);
        assert_eq!(ctl.apply(Intent::ClearPatternBox { node: first }), Err(wrong));
        assert_eq!((ctl.rev, ctl.dirty, told(&engine)), (rev, false, Vec::new()));
    }

    #[test]
    fn typing_a_name_is_one_step_if_the_changes_come_soon_after_each_other_and_a_pick_and_a_name_are_two() {
        let (engine, mut ctl, clock) = controller(SPLITTERS);
        let first = splitter_node(&ctl, 0);
        for name in ["B", "Ba", "Bas"] {
            set_field(&mut ctl, first, SplitterField::Name { name: name.to_string() }).unwrap();
        }
        set_field(&mut ctl, first, SplitterField::Pick { pick: SplitterPick::Highest }).unwrap();
        told(&engine);
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!((ctl.model.splitters[0].name.as_str(), ctl.model.splitters[0].pick), ("Bas", Pick::Lowest), "the pick is a step of its own");
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!(ctl.model.splitters[0].name, "Bass line", "the three that came soon after each other are one step, back to where they began");
        assert_eq!(undo_label(&ctl), None);
        // A change that comes after a pause is a step of its own.
        set_field(&mut ctl, first, SplitterField::Name { name: "One".to_string() }).unwrap();
        clock.advance(Duration::from_secs(30));
        set_field(&mut ctl, first, SplitterField::Name { name: "Two".to_string() }).unwrap();
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!(ctl.model.splitters[0].name, "One");
    }

    #[test]
    fn a_setting_of_one_splitter_is_not_merged_with_the_same_setting_of_another_splitter_that_comes_soon_after() {
        let (_engine, mut ctl, _) = controller(SPLITTERS);
        let (first, second) = (splitter_node(&ctl, 0), splitter_node(&ctl, 1));
        set_field(&mut ctl, first, SplitterField::Pick { pick: SplitterPick::Highest }).unwrap();
        set_field(&mut ctl, second, SplitterField::Pick { pick: SplitterPick::Lowest }).unwrap();
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!((ctl.model.splitters[0].pick, ctl.model.splitters[1].pick), (Pick::Highest, Pick::Highest), "only the second splitter's change is undone");
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!((ctl.model.splitters[0].pick, ctl.model.splitters[1].pick), (Pick::Lowest, Pick::Highest));
        assert_eq!(undo_label(&ctl), None);
    }

    // -- the cables of notes of a splitter --

    #[test]
    fn a_cable_from_the_pick_makes_the_lane_hear_the_pick_and_no_source_and_one_from_the_rest_makes_it_hear_the_rest() {
        let (engine, mut ctl, _) = controller("[keys]\n[[splitter]]\n[[splitter]]\n[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument]]\nplugin = \"builtin:osc\"\n");
        let (first, second, lane0, lane1) = (splitter_node(&ctl, 0), splitter_node(&ctl, 1), lane_node(&ctl, 0), lane_node(&ctl, 1));
        assert_eq!(told(&engine), [settings_line(first, "lowest", "all"), settings_line(second, "lowest", "all")], "a loaded splitter is told what it hears, every input until a cable says another");
        assert_eq!(draw(&mut ctl, first, lane0), Ok(Applied::restructured(1)));
        assert_eq!(told(&engine), ["SetLaneSources inst=0 sources=[64]".to_string()], "the pick of the first splitter, and no key");
        assert_eq!(undo_label(&ctl).as_deref(), Some("draw a cable of notes"));
        assert_eq!(draw_rest(&mut ctl, first, lane1), Ok(Applied::restructured(2)));
        assert_eq!(told(&engine), ["SetLaneSources inst=1 sources=[65]".to_string()], "the rest of it");
        draw_rest(&mut ctl, second, lane0).unwrap();
        assert_eq!(told(&engine), ["SetLaneSources inst=0 sources=[64, 67]".to_string()], "the pick of the first splitter and the rest of the second");
        assert_eq!(ctl.model.event_cables.iter().map(|cable| cable.output).collect::<Vec<_>>(), [Output::Pick, Output::Rest, Output::Rest]);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_lane_can_hear_both_outputs_of_a_splitter_one_cable_each_and_a_cable_is_not_drawn_twice_between_the_same_two_ports() {
        let (engine, mut ctl, _) = controller("[keys]\n[[splitter]]\n[[instrument]]\nplugin = \"builtin:osc\"\n");
        let (keys, splitter, lane) = (ctl.model.sources[0].node, splitter_node(&ctl, 0), lane_node(&ctl, 0));
        told(&engine);
        draw(&mut ctl, splitter, lane).unwrap();
        draw_rest(&mut ctl, splitter, lane).unwrap();
        draw(&mut ctl, keys, lane).unwrap();
        assert_eq!(told(&engine), ["SetLaneSources inst=0 sources=[64]".to_string(), "SetLaneSources inst=0 sources=[64, 65]".to_string(), "SetLaneSources inst=0 sources=[0, 64, 65]".to_string()]);
        let (rev, cables) = (ctl.rev, ctl.model.event_cables.clone());
        let again = IntentError::Invalid("there is a cable between those two already".to_string());
        assert_eq!(draw(&mut ctl, splitter, lane), Err(again.clone()));
        assert_eq!(draw_rest(&mut ctl, splitter, lane), Err(again));
        assert_eq!((ctl.rev, ctl.model.event_cables.clone(), told(&engine)), (rev, cables, Vec::new()));
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_cable_of_notes_into_a_splitter_makes_it_hear_the_sources_it_comes_from_and_a_box_makes_it_hear_the_box_and_no_input() {
        let (engine, mut ctl, _) = controller("[keys]\n[[device]]\nname = \"Launchkey\"\n[[pattern]]\n[[splitter]]\n[[instrument]]\nplugin = \"builtin:osc\"\n");
        let (keys, device, pattern, splitter) = (ctl.model.sources[0].node, ctl.model.sources[1].node, ctl.model.patterns[0].node, splitter_node(&ctl, 0));
        told(&engine);
        assert_eq!(draw(&mut ctl, keys, splitter), Ok(Applied::restructured(1)));
        assert_eq!(told(&engine), [settings_line(splitter, "lowest", "[0]")]);
        draw(&mut ctl, device, splitter).unwrap();
        assert_eq!(told(&engine), Vec::<String>::new(), "a device that is not plugged in is no source, either way");
        draw(&mut ctl, pattern, splitter).unwrap();
        assert_eq!(told(&engine), [settings_line(splitter, "lowest", "[0, 48]")], "the keys and the box");
        assert_eq!(ctl.model.splitter_inputs(0, |_| SourceSet::NONE), SourceSet::NONE.with(Source::pattern(0)));
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_splitter_that_has_a_box_and_no_other_cable_hears_the_box_and_no_key() {
        let (engine, mut ctl, _) = controller("[keys]\n[[pattern]]\n[[splitter]]\n[[instrument]]\nplugin = \"builtin:osc\"\n");
        let (pattern, splitter) = (ctl.model.patterns[0].node, splitter_node(&ctl, 0));
        told(&engine);
        draw(&mut ctl, pattern, splitter).unwrap();
        assert_eq!(told(&engine), [settings_line(splitter, "lowest", "[48]")]);
        assert!(!ctl.splitter_settings(0).inputs.contains(Source::KEYS));
    }

    #[test]
    fn a_cable_of_notes_to_or_from_a_splitter_that_cannot_be_drawn_is_refused_with_the_reason_and_changes_nothing_and_uses_no_id() {
        let (engine, mut ctl, _) = controller(SPLITTERS);
        let (keys, first, second, lane, pattern) = (ctl.model.sources[0].node, splitter_node(&ctl, 0), splitter_node(&ctl, 1), lane_node(&ctl, 0), ctl.model.patterns[0].node);
        told(&engine);
        let (rev, cables) = (ctl.rev, ctl.model.event_cables.clone());
        let audio_in = Endpoint { node: lane, port: Port::AudioIn };
        let audio_out = Endpoint { node: first, port: Port::AudioOut };
        for (from, to, gain, why) in [
            (out(first), into(second), None, "a note splitter cannot take the notes of another splitter"),
            (out_rest(first), into(second), None, "a note splitter cannot take the notes of another splitter"),
            (out(first), into(first), None, "a note splitter cannot take the notes of another splitter"),
            (out(first), into(pattern), None, "a note splitter cannot send its notes to a pattern box yet"),
            (out_rest(first), into(pattern), None, "a note splitter cannot send its notes to a pattern box yet"),
            (out_rest(keys), into(first), None, "only a note splitter has a second output of events"),
            (out_rest(pattern), into(first), None, "only a note splitter has a second output of events"),
            (out_rest(lane), into(first), None, "only a source of notes has an output of events"),
            (out(lane), into(first), None, "only a source of notes has an output of events"),
            (out(keys), into(first), None, "there is a cable between those two already"),
            (out(first), into(lane), None, "there is a cable between those two already"),
            (out_rest(first), into(ctl_lane(&ctl, 1)), None, "there is a cable between those two already"),
            (out(keys), into(second), Some(0.5), "a cable of notes has no level"),
            (out(first), into(lane), Some(1.0), "a cable of notes has no level"),
            (out_rest(first), into(lane), Some(1.0), "a cable of notes has no level"),
            (out(first), audio_in, None, "ends at an input of events"),
            (out_rest(first), audio_in, None, "ends at an input of events"),
            (audio_out, into(lane), None, "starts at an output of events"),
            (out(first), into(NodeId::MASTER), None, "the mix has no input of events"),
            (out_rest(first), into(keys), None, "a cable of notes ends at a lane"),
            (out_rest(NodeId(900)), into(first), None, "no such node"),
            (out(keys), into(NodeId(900)), None, "no such node"),
        ] {
            let error = ctl.apply(Intent::AddCable { from, to, gain }).unwrap_err();
            assert!(error.to_string().contains(why) || matches!(error, IntentError::NotFound(_)) && why == "no such node", "{from:?} to {to:?}: {error}");
        }
        assert_eq!((ctl.rev, ctl.model.event_cables.clone(), told(&engine)), (rev, cables, Vec::new()));
        ctl.model.check_ids().unwrap();
        draw_rest(&mut ctl, second, lane).unwrap();
        assert_eq!(ctl.model.event_cables.last().unwrap().id, EdgeId(6), "no id was used by the refusals");
        assert_eq!((ctl.model.event_cables.last().unwrap().from, ctl.model.event_cables.last().unwrap().output), (second, Output::Rest));
    }

    /// The node of lane `index`, for a table that is made before the controller is borrowed again.
    fn ctl_lane(ctl: &Controller, index: usize) -> NodeId {
        lane_node(ctl, index)
    }

    #[test]
    fn a_splitter_sends_no_notes_to_a_lane_whose_instrument_takes_none() {
        let (engine, mut ctl, _) = controller("[keys]\n[[splitter]]\n[[instrument]]\nplugin = \"builtin:input\"\nroute = \"none\"\n");
        let (splitter, input) = (splitter_node(&ctl, 0), lane_node(&ctl, 0));
        told(&engine);
        let error = IntentError::Invalid("that instrument takes no notes: an audio input has no input of events".to_string());
        assert_eq!(draw(&mut ctl, splitter, input), Err(error.clone()));
        assert_eq!(draw_rest(&mut ctl, splitter, input), Err(error));
        assert!(ctl.model.event_cables.is_empty());
    }

    #[test]
    fn taking_a_cable_of_notes_out_makes_the_splitter_hear_every_input_again_and_the_lane_every_source_and_undo_gives_each_back_from_the_output_it_came_from() {
        let (engine, mut ctl, _) = controller(SPLITTERS);
        let first = splitter_node(&ctl, 0);
        told(&engine);
        // The cables in the file: 0 keys to the first splitter, 3 its pick to lane 0, 4 its rest to lane 1.
        let (into_splitter, pick_cable, rest_cable) = (ctl.model.event_cables[0].id, ctl.model.event_cables[3].id, ctl.model.event_cables[4].id);
        assert_eq!((into_splitter, pick_cable, rest_cable), (EdgeId(0), EdgeId(3), EdgeId(4)));
        ctl.apply(Intent::RemoveEdge { edge: into_splitter }).unwrap();
        assert_eq!(told(&engine), [settings_line(first, "lowest", "all")]);
        assert_eq!(undo_label(&ctl).as_deref(), Some("remove a cable of notes"));
        ctl.apply(Intent::RemoveEdge { edge: rest_cable }).unwrap();
        assert_eq!(told(&engine), ["SetLaneSources inst=1 sources=all".to_string()]);
        ctl.apply(Intent::RemoveEdge { edge: pick_cable }).unwrap();
        assert_eq!(told(&engine), ["SetLaneSources inst=0 sources=all".to_string()]);
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!(told(&engine), ["SetLaneSources inst=0 sources=[64]".to_string()]);
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!(told(&engine), ["SetLaneSources inst=1 sources=[65]".to_string()], "the lane hears the rest again, and not the pick");
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!(told(&engine), [settings_line(first, "lowest", "[0]")]);
        assert_eq!(ctl.model.event_cables.iter().map(|cable| (cable.id.0, cable.output)).collect::<Vec<_>>(), [(0, Output::Pick), (1, Output::Pick), (2, Output::Pick), (3, Output::Pick), (4, Output::Rest), (5, Output::Pick)], "under the ids they had, each at the output it started at");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_lane_that_hears_a_splitter_is_taken_out_with_its_cable_and_comes_back_hearing_the_splitter_and_the_splitter_goes_on() {
        let (engine, mut ctl, _) = controller(SPLITTERS);
        let (first, lane) = (splitter_node(&ctl, 0), lane_node(&ctl, 1));
        told(&engine);
        ctl.apply(Intent::RemoveNode { node: lane }).unwrap();
        assert_eq!(ctl.model.event_cables.len(), 5, "the cable from the splitter's rest to the lane is gone");
        assert!(ctl.model.splitter(first).is_some());
        assert_eq!(told(&engine).iter().filter(|line| line.contains("Splitter")).count(), 0, "no splitter was taken out, and what it hears is what it was told");
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!(ctl.model.event_cables.len(), 6);
        assert!(told(&engine).contains(&"SetLaneSources inst=1 sources=[65]".to_string()));
        assert_eq!(ctl.model.event_cables[4].output, Output::Rest);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_lane_that_is_added_to_a_session_that_has_splitters_hears_every_input_and_no_splitter_until_a_cable_says_so() {
        let (engine, mut ctl, _) = controller(SPLITTERS);
        told(&engine);
        ctl.apply(Intent::AddInstrument { plugin: "builtin:osc".to_string() }).unwrap();
        assert_eq!(told(&engine), ["SetLaneSources inst=3 sources=all".to_string()]);
        let hears = ctl.lane_sources(3);
        assert_eq!(hears, SourceSet::ALL);
        assert!(!hears.contains(Source::splitter(0, Output::Pick)) && !hears.contains(Source::splitter(0, Output::Rest)), "an output of a splitter is not an input");
        let (second, lane) = (splitter_node(&ctl, 1), lane_node(&ctl, 3));
        draw_rest(&mut ctl, second, lane).unwrap();
        assert_eq!(told(&engine), ["SetLaneSources inst=3 sources=[67]".to_string()]);
    }

    #[test]
    fn what_a_splitter_hears_follows_the_devices_that_it_is_cabled_from_as_they_are_plugged_in_and_out() {
        let (engine, mut ctl, _) = controller(SPLITTERS);
        told(&engine);
        let ports = Ports::default();
        ctl.watch_devices(Box::new(ports.clone()));
        let second = splitter_node(&ctl, 1);
        assert_eq!(told(&engine), Vec::<String>::new(), "the box, and a device that is not plugged in");
        let t0 = Instant::now() + Duration::from_secs(60);
        ports.plug_in("Launchkey Mini MK4 37 MIDI Out", Source::device(0));
        ctl.poll_at(t0 + Duration::from_secs(1));
        assert_eq!(told(&engine), [settings_line(second, "highest", "[1, 48]")], "the port that the device's name stands for, and the box");
        ports.plug_in("Launchkey Mini MK4 37 DAW Out", Source::device(1));
        ctl.poll_at(t0 + Duration::from_secs(2));
        assert_eq!(told(&engine), [settings_line(second, "highest", "[1, 2, 48]")]);
        ports.unplug("Launchkey Mini MK4 37 MIDI Out");
        ctl.poll_at(t0 + Duration::from_secs(3));
        assert_eq!(told(&engine), ["ReleaseSource source=1".to_string(), settings_line(second, "highest", "[2, 48]")], "what the gone port held is let go of, by the splitter too (the engine does that for every splitter), and it hears the port that is left");
        // A look at the ports that finds nothing new tells nothing.
        ctl.poll_at(t0 + Duration::from_secs(4));
        assert_eq!(told(&engine), Vec::<String>::new());
    }

    // -- what the history and the model keep --

    #[test]
    fn a_splitter_that_is_taken_out_by_something_that_is_no_intent_forgets_the_history_as_any_removal_does_and_a_splitter_that_is_not_there_is_not_taken_out() {
        let (engine, mut ctl, _) = controller(SPLITTERS);
        let first = splitter_node(&ctl, 0);
        set_field(&mut ctl, first, SplitterField::Pick { pick: SplitterPick::Highest }).unwrap();
        assert!(undo_label(&ctl).is_some());
        told(&engine);
        assert!(!ctl.remove_splitter(9), "there is no splitter 9");
        assert_eq!((ctl.model.splitters.len(), undo_label(&ctl).is_some(), told(&engine)), (2, true, Vec::new()), "nothing was done");
        assert!(ctl.remove_splitter(0));
        assert_eq!(ctl.history_state(), HistoryState::default(), "what was done before is not undone over a change that was no step");
        assert_eq!(told(&engine)[0], format!("RemoveSplitter node={}", first.0));
        assert!(ctl.sent_splitters.iter().all(|(told, _)| *told != first), "what the audio thread was told of a splitter that is gone is not kept");
    }

    #[test]
    fn a_splitter_that_is_put_back_keeps_its_slot_when_that_is_free_though_a_lower_slot_is_free_too() {
        let (engine, mut ctl, _) = controller(SPLITTERS);
        ctl.apply(Intent::AddSplitter).unwrap();
        let (first, third) = (splitter_node(&ctl, 0), splitter_node(&ctl, 2));
        assert_eq!(ctl.model.splitters[2].slot, 2);
        ctl.apply(Intent::RemoveNode { node: first }).unwrap();
        ctl.apply(Intent::RemoveNode { node: third }).unwrap();
        told(&engine);
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!(ctl.model.splitter(third).map(|splitter| splitter.slot), Some(2), "its own, which is free, and not the lowest that is");
        assert_eq!(told(&engine), [add_line(third, 68, &settings_tail("lowest", "all"))]);
    }

    #[test]
    fn undo_is_refused_when_the_node_that_it_would_put_back_is_there_already() {
        let (_engine, mut ctl, _) = controller(SPLITTERS);
        let first = splitter_node(&ctl, 0);
        ctl.apply(Intent::RemoveNode { node: first }).unwrap();
        // (A splitter that is made behind the history's back under the same node: a session that is changed without it.)
        ctl.model.splitters.push(NoteSplitter::new(first, 5, "Again".to_string()));
        let error = ctl.apply(Intent::Undo).unwrap_err();
        assert_eq!(error, IntentError::Invalid(format!("node {} is there already", first.0)));
        assert_eq!(ctl.model.splitters.len(), 2, "nothing was put back over it");
    }

    #[test]
    fn a_splitter_is_placed_like_any_node_and_its_place_is_in_the_file_and_stays_through_undo() {
        let (_engine, mut ctl, _) = controller(SPLITTERS);
        let first = splitter_node(&ctl, 0);
        assert_eq!(ctl.model.ui.get(&first), None);
        let place = crate::session::NodeUi { x: 120.0, y: 80.5 };
        ctl.apply(Intent::SetNodeUi { node: first, ui: Some(place) }).unwrap();
        assert_eq!(ctl.model.ui.get(&first), Some(&place));
        let (instruments, groups, cables, sources) = crate::app::save::to_save(&ctl.model);
        let saved = crate::session::to_toml(&instruments, &groups, &cables, &sources, None, ctl.model.master_ui(), ctl.model.bpm_to_save()).unwrap();
        assert!(saved.contains("[[splitter]]\nname = \"Bass line\"\nui = { x = 120, y = 80.5 }\n"), "{saved}");
        ctl.apply(Intent::RemoveNode { node: first }).unwrap();
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!(ctl.model.ui.get(&first), Some(&place), "the place of a splitter that comes back is where it was");
    }

    #[test]
    fn a_session_that_is_saved_and_loaded_again_has_every_splitter_and_cable_of_notes_it_had_each_at_the_output_it_started_at() {
        let (_engine, mut ctl, _) = controller(SPLITTERS);
        let text = ctl.session_text().unwrap();
        for line in ["from = \"splitter[0]\"", "from = \"splitter[0].rest\"", "from = \"splitter[1]\"", "to = \"splitter[0]\"", "to = \"splitter[1]\"", "pick = \"highest\""] {
            assert!(text.contains(line), "{line} is written:\n{text}");
        }
        assert!(!text.contains("pick = \"lowest\""), "the pick that a new splitter has is not written:\n{text}");
        let again = load(&text).model;
        assert_eq!(again.splitters, ctl.model.splitters);
        let shape = |model: &crate::app::model::SessionModel| model.event_cables.iter().map(|cable| (cable.id.0, model.locate(cable.from), cable.output, model.locate(cable.to))).collect::<Vec<_>>();
        assert_eq!(shape(&again), shape(&ctl.model));
        assert_eq!(again.splitters[0].name, "Bass line");
        assert_eq!(again.splitters[1].pick, Pick::Highest);
        // A node that is added, and the two outputs that it can start cables at, are as they were saved a second time.
        assert_eq!(crate::app::controller::Controller::new(again, testutil::stub_engine_of(4_096).0, None).session_text().unwrap(), text);
    }

    #[test]
    fn every_intent_that_names_a_node_and_is_not_for_a_splitter_is_refused_when_it_names_a_splitter_and_changes_nothing_and_tells_the_audio_thread_nothing() {
        use crate::app::graph::NoteRange;
        use crate::app::intent::{Gain, ModulatorField, ModulatorKind, PatternBoxField, PatternField};
        let (engine, mut ctl, _) = controller(SPLITTERS);
        let (first, second) = (splitter_node(&ctl, 0), splitter_node(&ctl, 1));
        told(&engine);
        let (rev, splitters, cables) = (ctl.rev, ctl.model.splitters.clone(), ctl.model.event_cables.clone());
        let audio_in = Endpoint { node: first, port: Port::AudioIn };
        let audio_out = Endpoint { node: first, port: Port::AudioOut };
        let rate = Endpoint { node: first, port: Port::Rate };
        let asks = vec![
            Intent::SetParam { node: first, param: 0, value: 0.5 },
            Intent::SetGain { node: first, gain: Gain::Volume, value: 0.5 },
            Intent::SetGain { node: first, gain: Gain::Mix, value: 0.5 },
            Intent::SetGain { node: first, gain: Gain::GroupVolume, value: 0.5 },
            Intent::LoadPreset { node: first, preset: "x".to_string() },
            Intent::SetModulatorField { node: first, field: ModulatorField::Rate, value: 1.0 },
            Intent::AddFreeModulator { kind: ModulatorKind::Lfo, hears: Some(first) },
            Intent::AddModEdge { modulator: first, to: audio_in },
            Intent::Modulate { to: rate, kind: ModulatorKind::Lfo, depth: None },
            Intent::AddCable { from: audio_out, to: Endpoint { node: NodeId::MASTER, port: Port::AudioIn }, gain: None },
            Intent::AddCable { from: Endpoint { node: lane_node(&ctl, 0), port: Port::AudioOut }, to: audio_in, gain: None },
            Intent::AddEffect { parent: first, plugin: "builtin:reverb".to_string() },
            Intent::ReplaceInstrument { node: first, plugin: "builtin:osc".to_string() },
            Intent::ClearInstrument { node: first },
            Intent::SetRange { node: first, range: Some(NoteRange { low: 0, high: 60 }) },
            Intent::SetMute { node: first, muted: true },
            Intent::SetSolo { node: first, solo: true },
            Intent::SetRoute { node: first, route: crate::session::Route::None },
            Intent::SetModulatorEar { node: first, ear: None },
            Intent::SetFeed { node: first, feed: crate::session::Feed::None },
            Intent::SetRecording { node: first, recording: true },
            Intent::ClearPattern { node: first },
            Intent::SetPatternField { node: first, field: PatternField::Length, value: 2.0 },
            Intent::SetPatternBoxField { node: first, field: PatternBoxField::Looping { on: false } },
            Intent::ClearPatternBox { node: first },
            Intent::SetPatternBoxNotes { node: first, notes: vec![] },
            Intent::PatternBoxTransport { node: first, action: crate::app::intent::PatternBoxAction::Record },
            Intent::SetPinned { node: first, params: vec![0] },
            Intent::SetSteps { node: first, values: vec![0.0] },
            Intent::AddGroup { members: vec![first] },
            Intent::SetLaneGroup { node: first, group: None },
            Intent::RenameGroup { node: first, name: "x".to_string() },
            Intent::RenameMacro { node: first, name: "x".to_string() },
            Intent::ExposeMacro { node: first, expose: true },
            Intent::CollapseIntoModule { nodes: vec![first, second], name: "x".to_string(), ui: None },
            Intent::UngroupModule { node: first },
            Intent::MoveEffect { node: first, index: 0 },
            Intent::SaveModule { node: first, name: "x".to_string() },
            Intent::OpenEditor { node: first },
        ];
        for ask in asks {
            let error = ctl.apply(ask.clone()).expect_err(&format!("{ask:?} is refused"));
            assert!(!error.to_string().is_empty(), "{ask:?}");
        }
        assert_eq!((ctl.rev, ctl.model.splitters.clone(), ctl.model.event_cables.clone(), ctl.dirty), (rev, splitters, cables, false));
        assert_eq!(told(&engine), Vec::<String>::new());
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn the_example_of_a_chord_split_into_its_bass_and_the_rest_loads_and_the_lanes_hear_the_outputs_it_cables_them_from() {
        let (engine, ctl, _) = controller(include_str!("../../examples/note-splitter.toml"));
        let splitter = &ctl.model.splitters[0];
        assert_eq!((splitter.name.as_str(), splitter.slot, splitter.pick), ("Bass and chord", 0, Pick::Lowest));
        assert_eq!(ctl.model.event_cables.iter().map(|cable| cable.output).collect::<Vec<_>>(), [Output::Pick, Output::Pick, Output::Rest], "the keys into the splitter, its pick to the bass and its rest to the pad");
        assert_eq!(
            told(&engine),
            ["SetLaneSources inst=0 sources=[64]".to_string(), "SetLaneSources inst=1 sources=[65]".to_string(), settings_line(splitter.node, "lowest", "[0]")],
            "the bass hears the pick, the pad the rest, and the splitter the keys"
        );
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn the_graph_a_client_gets_has_a_node_for_each_splitter_after_the_boxes_and_an_edge_for_each_cable_at_the_port_it_starts_at() {
        let (_engine, ctl, _) = controller(SPLITTERS);
        let graph = ctl.model.graph();
        let kinds: Vec<(&str, &NodeKind)> = graph.nodes.iter().filter(|node| matches!(node.kind, NodeKind::Splitter { .. })).map(|node| (node.name.as_str(), &node.kind)).collect();
        assert_eq!(kinds, [("Bass line", &NodeKind::Splitter { pick: SplitterPick::Lowest }), ("Splitter", &NodeKind::Splitter { pick: SplitterPick::Highest })]);
        let rest_edges = graph.edges.iter().filter(|edge| edge.from.port == Port::EventsOutRest).count();
        assert_eq!(rest_edges, 1, "the one cable from a second output");
    }
}
