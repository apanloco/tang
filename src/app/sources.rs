//! What each lane hears (P4b of `docs/patch-graph.md` §11.6): the controller works the set of sources of every lane out of the cables of notes and tells the audio thread
//! (`GraphCommand::SetLaneSources`), when it is not what the audio thread was told.
//!
//! A lane that no cable of notes ends at hears every source; one that has some hears the sources that they start at: the keys are the keys' source, and a device is the sources
//! of the open MIDI ports that its name stands for (`DeviceWatcher::sources_of`), so that a device that is not plugged in is not heard, and does not leave the lane to the others.

use crate::app::controller::Controller;
use crate::app::model::{NodeId, SourceKind};
use crate::plugin::chain::GraphCommand;
use crate::source::{Source, SourceSet};

impl Controller {
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
    use crate::app::intent::Intent;
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
}
