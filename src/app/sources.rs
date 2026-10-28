//! What each lane hears (P4b of `docs/patch-graph.md` §11.6): the controller works the set of sources of every lane out of the cables of notes and tells the audio thread
//! (`GraphCommand::SetLaneSources`), when it is not what the audio thread was told.
//!
//! A lane that no cable of notes ends at hears every source; one that has some hears the sources that they start at: the keys are the keys' source, and a device is the sources
//! of the open MIDI ports that its name stands for (`DeviceWatcher::sources_of`), so that a device that is not plugged in is not heard, and does not leave the lane to the others.

use crate::app::controller::Controller;
use crate::app::intent::IntentError;
use crate::app::journal::Inverse;
use crate::app::model::{EdgeId, EventCable, NodeId, SourceKind, SourceNode};
use crate::plugin::chain::GraphCommand;
use crate::session;
use crate::source::{Source, SourceSet};

impl Controller {
    /// Put a source of notes at the end of the session's sources, under a node of its own (`Intent::AddSource`). It changes nothing that is heard: a lane that no cable of notes ends at hears every source.
    /// Refused, with nothing changed and no id used, for the keys when they are a source already, for a device whose name is not one (`session::device_name`) and for a device of a name that is a source already.
    pub(super) fn add_source(&mut self, kind: SourceKind) -> Result<NodeId, IntentError> {
        let kind = match kind {
            SourceKind::Keys => SourceKind::Keys,
            SourceKind::Device(name) => SourceKind::Device(session::device_name(&name).map_err(|why| IntentError::Invalid(format!("a device's name {why}")))?),
        };
        if self.model.sources.iter().any(|source| source.kind == kind) {
            return Err(IntentError::Invalid(match kind {
                SourceKind::Keys => "the keys are a source already".to_string(),
                SourceKind::Device(name) => format!("{name} is a source already"),
            }));
        }
        let node = self.model.alloc_node();
        self.model.sources.push(SourceNode { node, kind });
        Ok(node)
    }

    /// Draw a cable of notes from the source `from` to the lane `to` (both by node), under an id of the series that the other cables have (`Intent::AddCable`): the lane then hears the sources that
    /// its cables start at and no others. Refused, with nothing changed and no id used, for a cable between the two that is there already. (The ends are `SessionModel::event_cable_ends`'s to say.)
    pub(super) fn add_event_cable(&mut self, from: NodeId, to: NodeId) -> Result<EdgeId, IntentError> {
        if self.model.event_cables.iter().any(|cable| (cable.from, cable.to) == (from, to)) {
            return Err(IntentError::Invalid("there is a cable between those two already".to_string()));
        }
        let id = self.model.alloc_edge();
        self.model.insert_event_cable(EventCable { id, from, to });
        Ok(id)
    }

    /// Take the source of notes at `index` out of the session, after the cables of notes that start at it (each written down, as a lane's are): the lanes that heard it hear what their other cables bring,
    /// or every source if they have none (`sync_lane_sources`, which `conclude` calls). False, changing nothing, if there is no such source.
    pub(super) fn remove_source(&mut self, index: usize) -> bool {
        let Some(node) = self.model.sources.get(index).map(|source| source.node) else { return false };
        self.forget_the_history_unless_an_intent_asks();
        self.take_out_event_cables_at(&[node]);
        let source = self.model.sources.remove(index);
        self.took_out(Inverse::Source { index, source });
        true
    }

    /// What lane `inst` hears, as the model's cables and the devices that are open say (every source if no cable of notes ends at the lane).
    pub(super) fn lane_sources(&self, inst: usize) -> SourceSet {
        self.model.lane_sources(inst, |kind| match kind {
            SourceKind::Keys => SourceSet::NONE.with(Source::KEYS),
            SourceKind::Device(name) => self.devices.as_ref().map_or(SourceSet::NONE, |devices| devices.sources_of(name)),
        })
    }

    /// Tell the audio thread what each lane hears where it is not what it was told last (`sent_sources`). A lane that was not told anything before is told if the session has a cable of
    /// notes: it may be one that came back by undo, with what it heard when it went, which is not what it hears now (a lane that is new hears every source, as it has it).
    pub(super) fn sync_lane_sources(&mut self) {
        let routed = !self.model.event_cables.is_empty();
        let mut now: Vec<(NodeId, SourceSet)> = Vec::with_capacity(self.model.instruments.len());
        for inst in 0..self.model.instruments.len() {
            let (node, sources) = (self.model.instruments[inst].node, self.lane_sources(inst));
            let told = self.sent_sources.iter().find(|(told, _)| *told == node).map(|(_, sources)| *sources);
            if told.map_or(routed, |told| told != sources) {
                let _ = self.cmd_tx.send(GraphCommand::SetLaneSources { inst, sources });
            }
            now.push((node, sources));
        }
        self.sent_sources = now;
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::app::controller::DeviceWatcher;
    use crate::app::graph::{Edge, EdgeKind, Endpoint, Port, SourceInfo};
    use crate::app::intent::{Applied, Event, Intent};
    use crate::app::model::NodePath;
    use crate::session::NodeUi;
    use crate::app::testutil::{self, GROUPS, SENDS, SOURCES, StubEngine, load};
    use crate::midi::DeviceChanges;

    /// MIDI ports that a test plugs in and out, by name, each with the source it has; what a look at them finds (`looks`) is told by the test.
    #[derive(Clone, Default)]
    struct Ports {
        open: Rc<RefCell<Vec<(String, Source)>>>,
        looks: Rc<RefCell<VecDeque<DeviceChanges>>>,
    }

    impl Ports {
        fn plug_in(&self, name: &str, source: Source) {
            self.open.borrow_mut().push((name.to_string(), source));
            self.looks.borrow_mut().push_back(DeviceChanges { came: vec![source], gone: vec![] });
        }

        fn unplug(&self, name: &str) {
            let at = self.open.borrow().iter().position(|(open, _)| open == name).expect("a port that is open");
            let (_, source) = self.open.borrow_mut().remove(at);
            self.looks.borrow_mut().push_back(DeviceChanges { came: vec![], gone: vec![source] });
        }
    }

    impl DeviceWatcher for Ports {
        fn poll_new_devices(&mut self) -> DeviceChanges {
            self.looks.borrow_mut().pop_front().unwrap_or_default()
        }

        fn sources_of(&self, name: &str) -> SourceSet {
            self.open.borrow().iter().filter(|(open, _)| open.contains(name)).fold(SourceSet::NONE, |set, (_, source)| set.with(*source))
        }

        fn open_names(&self) -> Vec<String> {
            self.open.borrow().iter().map(|(name, _)| name.clone()).collect()
        }
    }

    fn controller(session: &str) -> (StubEngine, Controller) {
        let (handles, engine) = testutil::stub_engine();
        let mut ctl = Controller::new(load(session).model, handles, None);
        ctl.catalog.add(crate::plugin::builtin::enumerate_plugins());
        (engine, ctl)
    }

    fn lane_sources_told(engine: &StubEngine) -> Vec<String> {
        engine.drain().0.into_iter().filter(|command| command.starts_with("SetLaneSources") || command.starts_with("ReleaseSource")).collect()
    }

    #[test]
    fn a_session_with_no_source_is_told_nothing_of_what_its_lanes_hear_whatever_is_done_to_it() {
        for session in [SENDS, GROUPS, "[[instrument]]\nplugin = \"builtin:osc\"\n"] {
            let (engine, mut ctl) = controller(session);
            assert_eq!(lane_sources_told(&engine), Vec::<String>::new(), "when it is made");
            ctl.apply(Intent::AddInstrument { plugin: "builtin:osc".to_string() }).unwrap_or_else(|_| panic!("a lane is added"));
            let node = ctl.model.instruments[0].node;
            ctl.apply(Intent::SetGain { node, gain: crate::app::intent::Gain::Volume, value: 0.5 }).unwrap();
            assert_eq!(lane_sources_told(&engine), Vec::<String>::new(), "after a lane is added and a volume is set");
        }
    }

    #[test]
    fn a_lane_that_has_cables_of_notes_is_told_what_it_hears_when_the_controller_is_made_and_a_lane_that_has_none_is_not() {
        let (engine, _ctl) = controller(SOURCES);
        // Lane 0 is wired to the keys, lane 1 to two devices that are not plugged in (it hears nothing until they are), and lane 2 has no cable and hears every source, as it did.
        assert_eq!(lane_sources_told(&engine), ["SetLaneSources inst=0 sources=[0]", "SetLaneSources inst=1 sources=[]"]);
    }

    #[test]
    fn a_lane_wired_to_a_device_hears_the_ports_that_are_open_for_it_when_the_devices_are_watched_and_as_they_come_and_go() {
        let (engine, mut ctl) = controller(SOURCES);
        lane_sources_told(&engine);
        let ports = Ports::default();
        ports.plug_in("Launchkey Mini MK4 37 MIDI Out", Source::device(0));
        ports.plug_in("Launchkey Mini MK4 37 DAW Out", Source::device(1));
        ports.looks.borrow_mut().clear();
        ctl.watch_devices(Box::new(ports.clone()));
        assert_eq!(lane_sources_told(&engine), ["SetLaneSources inst=1 sources=[1, 2]"], "both ports of the first device; the second is not plugged in, and lane 0 is as it was");

        // The second device is plugged in: the lane hears it as well.
        let t0 = Instant::now() + Duration::from_secs(60);
        ports.plug_in("Seaboard RISE", Source::device(2));
        ctl.poll_at(t0 + Duration::from_secs(1));
        assert_eq!(lane_sources_told(&engine), ["SetLaneSources inst=1 sources=[1, 2, 3]"]);

        // The first device is unplugged, a port at a time: every lane lets go of what the port held, and the lane does not hear it any more (and the other device goes on).
        ports.unplug("Launchkey Mini MK4 37 MIDI Out");
        ctl.poll_at(t0 + Duration::from_secs(2));
        assert_eq!(lane_sources_told(&engine), ["ReleaseSource source=1", "SetLaneSources inst=1 sources=[2, 3]"]);
        ports.unplug("Launchkey Mini MK4 37 DAW Out");
        ctl.poll_at(t0 + Duration::from_secs(3));
        assert_eq!(lane_sources_told(&engine), ["ReleaseSource source=2", "SetLaneSources inst=1 sources=[3]"]);

        // Plugged in again, the first port is heard again, under the source it had.
        ports.plug_in("Launchkey Mini MK4 37 MIDI Out", Source::device(0));
        ctl.poll_at(t0 + Duration::from_secs(4));
        assert_eq!(lane_sources_told(&engine), ["SetLaneSources inst=1 sources=[1, 3]"]);
    }

    #[test]
    fn the_controller_says_which_inputs_are_open_as_the_watcher_has_them_and_none_without_a_watcher() {
        let (_engine, mut ctl) = controller(SOURCES);
        assert_eq!(ctl.inputs(), Vec::<String>::new(), "no one watches the devices: none is open as far as it knows");
        let ports = Ports::default();
        ports.plug_in("Launchkey Mini MK4 37 MIDI Out", Source::device(0));
        ports.plug_in("Seaboard RISE", Source::device(1));
        ctl.watch_devices(Box::new(ports.clone()));
        assert_eq!(ctl.inputs(), ["Launchkey Mini MK4 37 MIDI Out", "Seaboard RISE"], "in the order they were opened");
        ports.unplug("Launchkey Mini MK4 37 MIDI Out");
        assert_eq!(ctl.inputs(), ["Seaboard RISE"], "the watcher's list as it is when it is asked, and not as it was when the watching began");
        ports.plug_in("Launchkey Mini MK4 37 MIDI Out", Source::device(0));
        assert_eq!(ctl.inputs(), ["Seaboard RISE", "Launchkey Mini MK4 37 MIDI Out"], "a device that came back is the last one opened");
    }

    #[test]
    fn a_lane_that_hears_what_it_was_told_it_hears_is_not_told_again() {
        let (engine, mut ctl) = controller(SOURCES);
        lane_sources_told(&engine);
        ctl.sync_lane_sources();
        assert_eq!(lane_sources_told(&engine), Vec::<String>::new());
    }

    #[test]
    fn a_lane_that_loses_its_last_cable_of_notes_hears_every_source_again_and_the_one_that_has_one_left_hears_what_it_comes_from() {
        let (engine, mut ctl) = controller(SOURCES);
        lane_sources_told(&engine);
        // Lane 1 has two cables, from the two devices. One is taken out: it hears the other (that is not plugged in: nothing); and then the other: every source.
        let (first, second) = (ctl.model.event_cables[1].id, ctl.model.event_cables[2].id);
        ctl.model.remove_event_cable(first).unwrap();
        ctl.sync_lane_sources();
        assert_eq!(lane_sources_told(&engine), Vec::<String>::new(), "a device that is not plugged in is no source, either way");
        ctl.model.remove_event_cable(second).unwrap();
        ctl.sync_lane_sources();
        assert_eq!(lane_sources_told(&engine), ["SetLaneSources inst=1 sources=all"]);
        // And the keys' cable too: lane 0 hears every source. With no cable left, and the sources there are no more, no lane is told anything again.
        let keys = ctl.model.event_cables[0].id;
        ctl.model.remove_event_cable(keys).unwrap();
        ctl.model.sources.clear();
        ctl.sync_lane_sources();
        assert_eq!(lane_sources_told(&engine), ["SetLaneSources inst=0 sources=all"]);
        ctl.sync_lane_sources();
        assert_eq!(lane_sources_told(&engine), Vec::<String>::new());
    }

    #[test]
    fn a_lane_that_is_added_to_a_session_that_has_sources_is_told_what_it_hears() {
        let (engine, mut ctl) = controller(SOURCES);
        lane_sources_told(&engine);
        ctl.apply(Intent::AddInstrument { plugin: "builtin:osc".to_string() }).unwrap();
        assert_eq!(lane_sources_told(&engine), ["SetLaneSources inst=3 sources=all"], "(it hears every source, as the audio thread has it: a lane that came back by undo would not)");
    }

    // -- the intents that put sources and cables of notes in the session --

    const TWO_LANES: &str = "[[instrument]]\nplugin = \"builtin:osc\"\n\n[[instrument]]\nplugin = \"builtin:osc\"\n";

    fn device(name: &str) -> SourceInfo {
        SourceInfo::Device { name: name.to_string() }
    }

    fn out(node: NodeId) -> Endpoint {
        Endpoint { node, port: Port::EventsOut }
    }

    fn into(node: NodeId) -> Endpoint {
        Endpoint { node, port: Port::EventsIn }
    }

    /// Draw a cable of notes from the source `from` to the lane `to`, as a client asks.
    fn draw(ctl: &mut Controller, from: NodeId, to: NodeId) -> Result<Applied, IntentError> {
        ctl.apply(Intent::AddCable { from: out(from), to: into(to), gain: None })
    }

    #[test]
    fn the_keys_are_added_as_a_node_at_the_end_of_the_sources_and_the_audio_thread_is_told_nothing() {
        let (engine, mut ctl) = controller(TWO_LANES);
        let applied = ctl.apply(Intent::AddSource { source: SourceInfo::Keys }).unwrap();
        assert_eq!(applied, Applied::restructured(1));
        let keys = ctl.model.sources[0].node;
        assert_eq!(ctl.model.sources, [SourceNode { node: keys, kind: SourceKind::Keys }]);
        assert_eq!(ctl.model.locate(keys), Some(NodePath::Source(0)));
        assert!(ctl.dirty, "the node is in the file when the session is saved");
        assert_eq!(lane_sources_told(&engine), Vec::<String>::new(), "no lane has a cable of notes, so every lane hears every source, as before");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_device_is_added_after_the_sources_there_are_by_its_name_without_the_spaces_round_it_and_the_case_is_the_names() {
        let (_engine, mut ctl) = controller(SOURCES);
        ctl.apply(Intent::AddSource { source: device("  Roland A-49 \t") }).unwrap();
        ctl.apply(Intent::AddSource { source: device("seaboard") }).unwrap();
        let kinds: Vec<SourceKind> = ctl.model.sources.iter().map(|source| source.kind.clone()).collect();
        assert_eq!(
            kinds,
            [
                SourceKind::Keys,
                SourceKind::Device("Launchkey Mini MK4 37".to_string()),
                SourceKind::Device("Seaboard".to_string()),
                SourceKind::Device("Roland A-49".to_string()),
                SourceKind::Device("seaboard".to_string()),
            ]
        );
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_source_that_is_there_already_or_has_no_name_is_refused_with_the_reason_and_changes_nothing_and_uses_no_id() {
        let (engine, mut ctl) = controller(&format!("[keys]\n\n[[device]]\nname = \"Seaboard\"\n\n{TWO_LANES}"));
        let long = "x".repeat(session::MAX_DEVICE_NAME + 1);
        let blank = "a device's name is blank: a device is called by its name, or as much of it as tells it";
        for (source, why) in [
            (SourceInfo::Keys, "the keys are a source already"),
            (device("Seaboard"), "Seaboard is a source already"),
            (device(" Seaboard\t"), "Seaboard is a source already"),
            (device(""), blank),
            (device(" \t "), blank),
            (device(&long), "a device's name is longer than 256 characters"),
            (device("A\nB"), "a device's name has a control character in it"),
        ] {
            assert_eq!(ctl.apply(Intent::AddSource { source: source.clone() }), Err(IntentError::Invalid(why.to_string())), "{source:?}");
        }
        assert_eq!((ctl.rev, ctl.dirty, ctl.model.sources.len()), (0, false, 2));
        assert_eq!(lane_sources_told(&engine), Vec::<String>::new());
        // No id was used up by any of them: the next source has the next id after the ones there are.
        let top = ctl.model.nodes().map(|(node, _)| node.0).max().unwrap();
        ctl.apply(Intent::AddSource { source: device("Roland") }).unwrap();
        assert_eq!(ctl.model.sources[2].node, NodeId(top + 1));
    }

    #[test]
    fn a_cable_of_notes_makes_the_lane_hear_the_source_it_starts_at_and_no_other_and_the_other_lanes_go_on_hearing_every_source() {
        let (engine, mut ctl) = controller(TWO_LANES);
        ctl.apply(Intent::AddSource { source: SourceInfo::Keys }).unwrap();
        let (keys, first) = (ctl.model.sources[0].node, ctl.model.instruments[0].node);
        assert_eq!(lane_sources_told(&engine), Vec::<String>::new());
        let applied = draw(&mut ctl, keys, first).unwrap();
        assert_eq!(applied, Applied::restructured(2));
        assert_eq!(ctl.model.event_cables, [EventCable { id: EdgeId(0), from: keys, to: first }]);
        assert_eq!(lane_sources_told(&engine), ["SetLaneSources inst=0 sources=[0]"], "the first lane only: the second has no cable");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_second_cable_of_notes_adds_the_open_ports_of_the_device_it_starts_at_to_what_the_lane_hears() {
        let (engine, mut ctl) = controller(TWO_LANES);
        ctl.apply(Intent::AddSource { source: SourceInfo::Keys }).unwrap();
        ctl.apply(Intent::AddSource { source: device("Launchkey") }).unwrap();
        let ports = Ports::default();
        ports.plug_in("Launchkey Mini MK4 37 MIDI Out", Source::device(0));
        ports.plug_in("Launchkey Mini MK4 37 DAW Out", Source::device(1));
        ports.looks.borrow_mut().clear();
        ctl.watch_devices(Box::new(ports.clone()));
        let (keys, launchkey, lane) = (ctl.model.sources[0].node, ctl.model.sources[1].node, ctl.model.instruments[0].node);
        lane_sources_told(&engine);
        draw(&mut ctl, keys, lane).unwrap();
        assert_eq!(lane_sources_told(&engine), ["SetLaneSources inst=0 sources=[0]"]);
        draw(&mut ctl, launchkey, lane).unwrap();
        assert_eq!(lane_sources_told(&engine), ["SetLaneSources inst=0 sources=[0, 1, 2]"], "the keys, and both ports that the name stands for");
    }

    #[test]
    fn a_cable_of_notes_that_cannot_be_drawn_is_refused_with_the_reason_and_changes_nothing_and_uses_no_id() {
        let (engine, mut ctl) = controller(&format!("[keys]\n\n{TWO_LANES}"));
        let (keys, lane) = (ctl.model.sources[0].node, ctl.model.instruments[0].node);
        let audio_in = Endpoint { node: lane, port: Port::AudioIn };
        let audio_out = Endpoint { node: keys, port: Port::AudioOut };
        for (from, to, gain, why) in [
            (out(keys), into(lane), Some(1.0), "a cable of notes has no level"),
            (out(keys), into(lane), Some(f32::NAN), "a cable of notes has no level"),
            (out(keys), audio_in, None, "ends at an input of events"),
            (audio_out, into(lane), None, "starts at an output of events"),
            (out(lane), into(lane), None, "only a source of notes has an output of events"),
            (out(lane), into(lane), Some(1.0), "only a source of notes has an output of events"),
            (out(keys), into(keys), None, "ends at a lane"),
            (out(NodeId::MASTER), into(lane), None, "the mix has no output of events"),
        ] {
            let error = ctl.apply(Intent::AddCable { from, to, gain }).unwrap_err();
            assert!(error.to_string().contains(why), "{from:?} to {to:?}: {error}");
        }
        assert_eq!(ctl.apply(Intent::AddCable { from: out(NodeId(900)), to: into(lane), gain: None }), Err(IntentError::NotFound(NodeId(900))));
        assert_eq!(ctl.apply(Intent::AddCable { from: out(keys), to: into(NodeId(901)), gain: None }), Err(IntentError::NotFound(NodeId(901))));
        assert_eq!(lane_sources_told(&engine), Vec::<String>::new());
        assert_eq!((ctl.rev, ctl.dirty, ctl.model.event_cables.len()), (0, false, 0));
        // No id was used up by any of them: the first cable that is drawn has the first id.
        draw(&mut ctl, keys, lane).unwrap();
        assert_eq!(ctl.model.event_cables[0].id, EdgeId(0));
    }

    #[test]
    fn a_cable_of_notes_is_not_drawn_again_between_the_same_two_nodes_and_another_start_or_end_is_another_cable() {
        let (engine, mut ctl) = controller(&format!("[keys]\n\n[[device]]\nname = \"Seaboard\"\n\n{TWO_LANES}"));
        let (keys, seaboard) = (ctl.model.sources[0].node, ctl.model.sources[1].node);
        let (first, second) = (ctl.model.instruments[0].node, ctl.model.instruments[1].node);
        draw(&mut ctl, keys, first).unwrap();
        lane_sources_told(&engine);
        assert_eq!(draw(&mut ctl, keys, first), Err(IntentError::Invalid("there is a cable between those two already".to_string())));
        assert_eq!(lane_sources_told(&engine), Vec::<String>::new());
        assert_eq!(ctl.model.event_cables.len(), 1);
        draw(&mut ctl, keys, second).unwrap();
        draw(&mut ctl, seaboard, first).unwrap();
        assert_eq!(ctl.model.event_cables.iter().map(|cable| cable.id.0).collect::<Vec<_>>(), [0, 1, 2], "the refusal used no id up");
    }

    #[test]
    fn taking_a_cable_of_notes_out_leaves_the_lane_the_sources_of_its_other_cables_and_every_source_when_it_was_the_last() {
        let (engine, mut ctl) = controller(SOURCES);
        lane_sources_told(&engine);
        // Lane 0 has one cable, from the keys. Lane 1 has two, from the two devices, neither of them plugged in.
        let applied = ctl.apply(Intent::RemoveEdge { edge: EdgeId(0) }).unwrap();
        assert_eq!(applied, Applied::restructured(1));
        assert_eq!(lane_sources_told(&engine), ["SetLaneSources inst=0 sources=all"]);
        ctl.apply(Intent::RemoveEdge { edge: EdgeId(1) }).unwrap();
        assert_eq!(lane_sources_told(&engine), Vec::<String>::new(), "it has the other device's cable, which is not plugged in either: it hears nothing, as it did");
        ctl.apply(Intent::RemoveEdge { edge: EdgeId(2) }).unwrap();
        assert_eq!(lane_sources_told(&engine), ["SetLaneSources inst=1 sources=all"]);
        assert!(ctl.model.event_cables.is_empty());
        assert_eq!((ctl.model.sources.len(), ctl.model.instruments.len()), (3, 3), "the nodes it was between are as they were");
        assert_eq!(ctl.apply(Intent::RemoveEdge { edge: EdgeId(2) }), Err(IntentError::EdgeNotFound(EdgeId(2))), "it is gone");
    }

    #[test]
    fn a_source_that_is_taken_out_takes_the_cables_that_start_at_it_and_a_lane_that_heard_only_it_hears_every_source() {
        let (engine, mut ctl) = controller(SOURCES);
        lane_sources_told(&engine);
        let (keys, seaboard) = (ctl.model.sources[0].node, ctl.model.sources[2].node);
        let applied = ctl.apply(Intent::RemoveNode { node: keys }).unwrap();
        assert_eq!(applied, Applied::restructured(1));
        assert_eq!(ctl.model.locate(keys), None);
        assert_eq!(ctl.model.event_cables.iter().map(|cable| cable.id.0).collect::<Vec<_>>(), [1, 2], "the keys' cable went with it");
        assert_eq!(lane_sources_told(&engine), ["SetLaneSources inst=0 sources=all"]);
        // The second device: lane 1 has the first device's cable still, which is not plugged in, so it hears nothing, as it did.
        ctl.apply(Intent::RemoveNode { node: seaboard }).unwrap();
        assert_eq!(ctl.model.event_cables.iter().map(|cable| cable.id.0).collect::<Vec<_>>(), [1]);
        assert_eq!(lane_sources_told(&engine), Vec::<String>::new());
        let launchkey = ctl.model.sources[0].node;
        ctl.apply(Intent::RemoveNode { node: launchkey }).unwrap();
        assert!(ctl.model.sources.is_empty() && ctl.model.event_cables.is_empty());
        assert_eq!(lane_sources_told(&engine), ["SetLaneSources inst=1 sources=all"]);
        assert_eq!(ctl.model.instruments.len(), 3, "a source is no lane");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_source_is_placed_by_the_intent_that_places_nodes_and_the_place_is_in_the_file_and_stays_through_a_removal_that_undo_puts_back() {
        let (_engine, mut ctl) = controller(&format!("[keys]\n\n{TWO_LANES}"));
        let keys = ctl.model.sources[0].node;
        let place = NodeUi { x: 12.5, y: -40.0 };
        let applied = ctl.apply(Intent::SetNodeUi { node: keys, ui: Some(place) }).unwrap();
        assert_eq!(applied, Applied::of(1, vec![Event::NodeUiSet { node: keys, ui: Some(place) }]));
        assert_eq!(ctl.model.ui.get(&keys), Some(&place));
        let (instruments, groups, cables, sources) = crate::app::save::to_save(&ctl.model);
        let text = session::to_toml(&instruments, &groups, &cables, &sources, None, None, None).unwrap();
        assert!(text.starts_with("[keys]\nui = { x = 12.5, y = -40 }\n"), "{text}");
        ctl.apply(Intent::RemoveNode { node: keys }).unwrap();
        ctl.undo().unwrap();
        assert_eq!(ctl.model.ui.get(&keys), Some(&place), "where it was");
    }

    #[test]
    fn the_graph_a_client_gets_has_the_cables_of_notes_that_were_drawn_and_what_the_lanes_that_have_none_hear() {
        let (_engine, mut ctl) = controller(TWO_LANES);
        ctl.apply(Intent::AddSource { source: SourceInfo::Keys }).unwrap();
        let (keys, first, second) = (ctl.model.sources[0].node, ctl.model.instruments[0].node, ctl.model.instruments[1].node);
        let edges = |ctl: &Controller| -> Vec<Edge> { ctl.snapshot().graph.edges.into_iter().filter(|edge| matches!(edge.kind, EdgeKind::Events { .. } | EdgeKind::Hears)).collect() };
        let hears = |lane| Edge { from: out(keys), to: into(lane), kind: EdgeKind::Hears };
        assert_eq!(edges(&ctl), [hears(first), hears(second)], "no cable: both lanes hear the keys");
        draw(&mut ctl, keys, first).unwrap();
        let drawn = Edge { from: out(keys), to: into(first), kind: EdgeKind::Events { id: EdgeId(0) } };
        assert_eq!(edges(&ctl), [hears(second), drawn], "the first lane hears the keys by its cable, and the second by having none");
    }
    /// The sessions of the fixture of what sources and cables of notes come to, each with the intents that are tried on it (one on each fresh controller): the session of `testutil::NOTES`, with
    /// every start tried with every end, the ports that are not events', levels, the names of devices, the cables and the nodes taken out, and every intent that names a node tried with the keys and with
    /// the device as the node; and a session that has no source, for them to be added to.
    fn sources_fixture_cases() -> Vec<(&'static str, &'static str, Vec<Intent>)> {
        use crate::app::intent::{Gain, ModulatorField, ModulatorKind, PatternField};
        let (_engine, ctl) = controller(testutil::NOTES);
        let m = &ctl.model;
        let (keys, launchkey) = (m.sources[0].node, m.sources[1].node);
        let lanes = [m.instruments[0].node, m.instruments[1].node, m.instruments[2].node];
        let fx = m.instruments[0].effects[0].node;
        let (group, bus) = (m.groups[0].node, m.groups[0].effects[0].node);
        let lfo = m.free_modulators[0].node;
        let modulation = m.free_modulators[0].targets[0].edge;
        let (notes_cable, audio_cable) = (m.event_cables[0].id, m.cables[0].id);
        let ghost = NodeId(9_999);
        let everything = [keys, launchkey, lanes[0], lanes[1], lanes[2], fx, group, bus, lfo, NodeId::MASTER, ghost];

        let mut wired: Vec<Intent> = Vec::new();
        // The names of devices that are sources, and those that are not: the spaces round a name, the case, nothing, a name that is long (counted in characters) and one with a control character.
        let long = "x".repeat(session::MAX_DEVICE_NAME);
        let wide = "é".repeat(session::MAX_DEVICE_NAME);
        wired.push(Intent::AddSource { source: SourceInfo::Keys });
        for name in [
            "Seaboard", " Seaboard\t", "\u{a0}Seaboard\u{2003}", "Launchkey Mini MK4 37", "  Launchkey Mini MK4 37 ", "launchkey mini mk4 37", "Launchkey", "Roland A-49 (2)", "", " ", " \t\n ", &long,
            &format!("{long}x"), &wide, &format!("{wide}é"), "A\nB", "A\u{7f}B", "A\u{85}B", "A\u{0}B", "A\tB", "A B", "\u{85}A\u{85}", "Ünïcode Keys",
        ] {
            wired.push(Intent::AddSource { source: device(name) });
        }
        // Every start with every end.
        for from in everything {
            for to in everything {
                wired.push(Intent::AddCable { from: out(from), to: into(to), gain: None });
            }
        }
        // A start that is not an output of events and an end that is not an input of events, and the other way round.
        for port in [Port::AudioOut, Port::AudioIn, Port::ModOut, Port::Param { index: 0 }, Port::Rate, Port::EventsIn] {
            wired.push(Intent::AddCable { from: Endpoint { node: keys, port }, to: into(lanes[0]), gain: None });
        }
        for port in [Port::EventsOut, Port::AudioIn, Port::AudioOut, Port::ModOut, Port::Param { index: 0 }, Port::Depth { edge: modulation }] {
            wired.push(Intent::AddCable { from: out(keys), to: Endpoint { node: lanes[0], port }, gain: None });
        }
        // The audio ports of a source (it has none), and an audio cable between what has them.
        for (from, to) in [(keys, fx), (fx, keys), (launchkey, NodeId::MASTER), (lanes[1], launchkey), (lanes[1], bus), (keys, keys)] {
            wired.push(Intent::AddCable { from: Endpoint { node: from, port: Port::AudioOut }, to: Endpoint { node: to, port: Port::AudioIn }, gain: None });
        }
        // A cable of notes has no level, whatever it is; what else is wrong with it is said first.
        // (A level that is not a number is no row: JSON has none, and the intent a client sends would not be it.)
        for gain in [0.0, 1.0, 2.5, -1.0] {
            wired.push(Intent::AddCable { from: out(keys), to: into(lanes[0]), gain: Some(gain) });
        }
        wired.push(Intent::AddCable { from: out(keys), to: into(keys), gain: Some(1.0) });
        wired.push(Intent::AddCable { from: out(lanes[0]), to: into(lanes[0]), gain: Some(1.0) });
        wired.push(Intent::AddCable { from: out(keys), to: into(lanes[1]), gain: Some(1.0) });
        // The level and the depth of each kind of cable, and a cable that is not there.
        for edge in [notes_cable, audio_cable, modulation, EdgeId(9_999)] {
            wired.push(Intent::SetCableGain { edge, gain: 0.5 });
            wired.push(Intent::SetEdgeDepth { edge, depth: 0.5 });
        }
        // Each cable taken out, and each node.
        for edge in [notes_cable, audio_cable, modulation, EdgeId(9_999)] {
            wired.push(Intent::RemoveEdge { edge });
        }
        for node in everything {
            wired.push(Intent::RemoveNode { node });
        }
        // Lanes that come, and go into groups, in a session that has sources.
        wired.push(Intent::AddInstrument { plugin: "builtin:osc".to_string() });
        wired.push(Intent::AddGroup { members: vec![lanes[1], lanes[2]] });
        wired.push(Intent::AddGroup { members: vec![keys] });
        wired.push(Intent::AddGroup { members: vec![] });
        for lane in lanes {
            wired.push(Intent::SetLaneGroup { node: lane, group: None });
            wired.push(Intent::SetLaneGroup { node: lane, group: Some(group) });
        }
        // Every intent that names a node, with a source as the node.
        for node in [keys, launchkey] {
            let place = NodeUi { x: 12.5, y: -40.0 };
            let ends = |port| Endpoint { node, port };
            wired.extend([
                Intent::SetParam { node, param: 0, value: 0.5 },
                Intent::SetGain { node, gain: Gain::Volume, value: 0.5 },
                Intent::SetGain { node, gain: Gain::Mix, value: 0.5 },
                Intent::SetGain { node, gain: Gain::GroupVolume, value: 0.5 },
                Intent::SetGain { node, gain: Gain::GroupPan, value: 0.5 },
                Intent::LoadPreset { node, preset: "a preset".to_string() },
                Intent::SetModulatorField { node, field: ModulatorField::Rate, value: 1.0 },
                Intent::SetSteps { node, values: vec![0.0; 16] },
                Intent::SetRange { node, range: None },
                Intent::SetMute { node, muted: true },
                Intent::SetSolo { node, solo: true },
                Intent::SetRoute { node, route: session::Route::None },
                Intent::SetRecording { node, recording: true },
                Intent::ClearPattern { node },
                Intent::SetPatternField { node, field: PatternField::Length, value: 4.0 },
                Intent::SetNodeUi { node, ui: Some(place) },
                Intent::SetNodeUi { node, ui: None },
                Intent::SetPinned { node, params: vec![] },
                Intent::SetPinned { node, params: vec![0] },
                Intent::RenameGroup { node, name: "A name".to_string() },
                Intent::AddEffect { parent: node, plugin: "builtin:reverb".to_string() },
                Intent::AddFreeModulator { kind: ModulatorKind::Lfo, hears: Some(node) },
                Intent::AddModEdge { modulator: node, to: Endpoint { node: lanes[0], port: Port::Param { index: 0 } } },
                Intent::AddModEdge { modulator: lfo, to: ends(Port::Param { index: 0 }) },
                Intent::AddModEdge { modulator: lfo, to: ends(Port::Rate) },
                Intent::Modulate { to: ends(Port::Param { index: 0 }), kind: ModulatorKind::Lfo, depth: None },
                Intent::Modulate { to: ends(Port::Rate), kind: ModulatorKind::Lfo, depth: None },
                Intent::ReplaceInstrument { node, plugin: "builtin:osc".to_string() },
                Intent::MoveEffect { node, index: 0 },
                Intent::AddGroup { members: vec![node] },
            ]);
        }

        let bare = vec![
            Intent::AddSource { source: SourceInfo::Keys },
            Intent::AddSource { source: device("Seaboard") },
            Intent::AddSource { source: device("  Seaboard  ") },
            Intent::AddSource { source: device("") },
            Intent::AddCable { from: out(NodeId(0)), to: into(NodeId(1)), gain: None },
            Intent::AddCable { from: out(NodeId(9_999)), to: into(NodeId(1)), gain: None },
            Intent::AddInstrument { plugin: "builtin:osc".to_string() },
            Intent::AddGroup { members: vec![NodeId(0)] },
            Intent::RemoveNode { node: NodeId(0) },
            Intent::SetNodeUi { node: NodeId(0), ui: Some(NodeUi { x: 1.0, y: 2.0 }) },
        ];
        vec![("wired", testutil::NOTES, wired), ("bare", TWO_LANES, bare)]
    }

    /// The sessions of `sources_fixture_cases` and what the controller makes of each intent tried on them, as the web client's tests read it (`web/src/client/fixtures/sources.json`): they pin the mock
    /// engine's to the controller's, intent by intent. A refusal is the controller's words; the rest is the events the intent made (stamped), whether it changed the structure and what the session
    /// comes to after it (`testutil::shape_of`: the sources, the cables of notes and what each lane that has none hears are in it). `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` rewrites it.
    #[test]
    fn the_web_clients_fixture_of_what_sources_and_cables_of_notes_come_to_is_current() {
        let mut cases = Vec::new();
        for (name, session, intents) in sources_fixture_cases() {
            let (_engine, base) = controller(session);
            let mut snapshot = testutil::wire(&base.snapshot());
            // The layout's fingerprint comes from the hasher, which the standard library does not promise to keep the same.
            snapshot["telemetry"]["rev"] = serde_json::json!(1);
            let mut rows = Vec::new();
            for intent in intents {
                let (_engine, mut fresh) = controller(session);
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
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/src/client/fixtures/sources.json");
        if std::env::var_os("TANG_UPDATE_TYPES").is_some() {
            std::fs::write(&path, &fresh).unwrap();
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            committed == fresh,
            "{} is not what sources and cables of notes come to now: run `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` and commit the result",
            path.display()
        );
    }
}
