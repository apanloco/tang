//! The owner's side of the web runtime; see `docs/web-ui.md` §3.1 and §3.2.
//!
//! The thread that owns the `Controller` (the headless loop of `tang serve`, later
//! the TUI loop) holds an `Owner` and, on every turn, calls `serve` to answer the
//! requests the connections have queued and `refresh` to keep what they read up to
//! date. Everything that touches the controller happens here, on the owner's thread,
//! so nothing guards the model with a lock.
//!
//! **The cut between a snapshot and the events after it** is made here: a client's
//! `Subscribe` is answered by marking it subscribed and queueing the snapshot in one
//! step, and events are only ever produced on this thread (inside `apply`), so the
//! client gets the snapshot of revision *n* and then exactly the events of revisions
//! after *n*, without a gap or a repeat.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use crossbeam_channel::Receiver;

use crate::app::controller::Controller;
use crate::app::history::HistoryState;
use crate::app::intent::Applied;
use crate::app::probes::Watcher;
use crate::app::protocol::{ServerMessage, WireError};
use crate::web::Shared;
use crate::web::connection::OwnerRequest;
use crate::web::hub::{ClientId, Hub, HubSink, Outgoing};

pub struct Owner {
    hub: Arc<Hub>,
    shared: Arc<Shared>,
    requests: Receiver<OwnerRequest>,
    /// What the clients were last told of the plugin catalog: its revision, and whether the scan was still going.
    catalog_told: (u64, bool),
    /// What the clients were last told Undo and Redo would do.
    history_told: HistoryState,
    /// What the clients were last told of the MIDI inputs that are open.
    inputs_told: Vec<String>,
}

impl Owner {
    /// An owner for `ctl`: from now on what the controller applies is told to the
    /// hub's subscribed clients, whichever front-end asked for it, and so is a
    /// snapshot when the session changes in a way no intent describes.
    pub fn new(ctl: &mut Controller, hub: Arc<Hub>, shared: Arc<Shared>, requests: Receiver<OwnerRequest>) -> Owner {
        ctl.subscribe(Box::new(HubSink { hub: hub.clone(), shared: shared.clone() }));
        let catalog_told = (ctl.catalog.rev(), ctl.catalog.is_scanning());
        let history_told = ctl.history_state();
        let inputs_told = ctl.inputs();
        Owner { hub, shared, requests, catalog_told, history_told, inputs_told }
    }

    /// Answer every request that is waiting, in the order they came. How many.
    pub fn serve(&mut self, ctl: &mut Controller) -> usize {
        self.serve_with(ctl, |_| {})
    }

    /// `serve`, handing each intent that was applied to `applied`: the front-end that
    /// shows the session (the TUI) has to react to what a browser changed.
    pub fn serve_with(&mut self, ctl: &mut Controller, mut applied: impl FnMut(Applied)) -> usize {
        let mut handled = 0;
        while let Ok(request) = self.requests.try_recv() {
            if let Some(intent) = self.handle(ctl, request) {
                applied(intent);
            }
            handled += 1;
        }
        handled
    }

    /// How many clients are connected.
    pub fn clients(&self) -> usize {
        self.hub.client_count()
    }

    /// The requests the connections make, for a loop that waits for them along with
    /// other things (a clone of the receiver can be selected on).
    pub fn requests(&self) -> &Receiver<OwnerRequest> {
        &self.requests
    }

    /// Answer one request. The intent it applied, if it applied one.
    pub fn handle(&mut self, ctl: &mut Controller, request: OwnerRequest) -> Option<Applied> {
        let applied = match request {
            OwnerRequest::Subscribe { client } => {
                // Subscribed and sent the snapshot in one step: no event can come between. The
                // catalog, what can be undone and the MIDI inputs that are open are not part of the session, and follow it.
                if self.hub.subscribe(client) {
                    self.send(client, &ServerMessage::Snapshot { snapshot: ctl.snapshot() });
                    self.send(client, &Self::catalog(ctl));
                    self.send(client, &Self::history(ctl));
                    self.send(client, &Self::inputs(ctl));
                }
                None
            }
            OwnerRequest::Apply { client, req_id, intent } => {
                // The events go out to the subscribed clients from inside `apply`, so the
                // answer follows them.
                let (reply, applied) = match ctl.apply(intent) {
                    Ok(applied) => (ServerMessage::Ack { req_id, rev: applied.rev }, Some(applied)),
                    Err(e) => (ServerMessage::Rejected { req_id, error: WireError::from(&e) }, None),
                };
                // (What can be undone changes with an intent, and is told after its events and before its answer.)
                self.tell_history(ctl);
                self.send(client, &reply);
                applied
            }
            OwnerRequest::Probes { client, nodes } => {
                // (Not an intent: it changes nothing in the session. The controller keeps the engine's taps where the nodes are, and the answer is which of them have a ring.)
                let watched = ctl.set_probes(Watcher(client.number()), &nodes);
                self.send(client, &ServerMessage::Probes { nodes: watched });
                None
            }
        };
        self.shared.rev.store(ctl.rev, Ordering::Relaxed);
        applied
    }

    /// The plugins that can be added, as a client is told of them.
    fn catalog(ctl: &Controller) -> ServerMessage {
        ServerMessage::Catalog { rev: ctl.catalog.rev(), scanning: ctl.catalog.is_scanning(), entries: ctl.catalog.entries() }
    }

    /// The MIDI inputs that are open, as a client is told of them.
    fn inputs(ctl: &Controller) -> ServerMessage {
        ServerMessage::Inputs { names: ctl.inputs() }
    }

    /// What Undo and Redo would do, as a client is told of it.
    fn history(ctl: &Controller) -> ServerMessage {
        let HistoryState { undo, redo } = ctl.history_state();
        ServerMessage::History { undo, redo }
    }

    /// Tell the subscribed clients what Undo and Redo would do, if it is not what they were told.
    fn tell_history(&mut self, ctl: &Controller) {
        let now = ctl.history_state();
        if now != self.history_told {
            self.history_told = now;
            self.hub.broadcast(&Self::text(&Self::history(ctl)));
        }
    }

    /// Let go of the probes of the clients that have gone (a connection that closes says nothing to the owner): call it on every turn that `refresh` is called on.
    pub fn refresh_probes(&mut self, ctl: &mut Controller) {
        let connected = self.hub.connected();
        // (The terminal is no connection of the hub's: it watches what it watches until it says otherwise.)
        ctl.keep_probes_of(|watcher| watcher == Watcher::TERMINAL || connected.contains(&watcher.0));
    }

    /// Keep what the connections read current: the session's revision, and the
    /// meaning of the telemetry slots, which the clients are told when it changes,
    /// the plugin catalog, as the scan finds more, and the MIDI inputs that are open, as devices
    /// come and go. Call it regularly (a few times a second): the model can change without an
    /// intent, under the TUI's hands.
    pub fn refresh(&mut self, ctl: &Controller) {
        self.shared.rev.store(ctl.rev, Ordering::Relaxed);
        let catalog = (ctl.catalog.rev(), ctl.catalog.is_scanning());
        if catalog != self.catalog_told {
            self.catalog_told = catalog;
            self.hub.broadcast(&Self::text(&Self::catalog(ctl)));
        }
        // (What the TUI does through the controller changes what can be undone too, with no request.)
        self.tell_history(ctl);
        // (A device that is plugged in or pulled out is found by the controller's look at the ports, and what can be made a source follows it.)
        let inputs = ctl.inputs();
        if inputs != self.inputs_told {
            self.hub.broadcast(&Self::text(&ServerMessage::Inputs { names: inputs.clone() }));
            self.inputs_told = inputs;
        }
        let layout = ctl.model.telemetry_layout();
        // (A snapshot carries the layout, and sets it in `shared` when it is sent: then
        // there is nothing to tell here.)
        if layout.rev != self.shared.layout().rev {
            self.shared.set_layout(layout.clone());
            self.hub.broadcast(&Self::text(&ServerMessage::Layout { layout }));
        }
    }

    fn send(&self, client: ClientId, message: &ServerMessage) {
        self.hub.send(client, Self::text(message));
    }

    fn text(message: &ServerMessage) -> Outgoing {
        Outgoing::Text(serde_json::to_string(message).expect("a message is serializable").into())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use tokio::sync::mpsc::Receiver as ClientQueue;

    use super::*;
    use crate::app::controller::DeviceWatcher;
    use crate::app::intent::{Gain, Intent};
    use crate::app::model::NodeId;
    use crate::app::testutil::{self, StubEngine, load};

    struct Rig {
        ctl: Controller,
        _engine: StubEngine,
        owner: Owner,
        hub: Arc<Hub>,
        shared: Arc<Shared>,
        tx: crossbeam_channel::Sender<OwnerRequest>,
    }

    const SESSION: &str = "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument]]\nplugin = \"builtin:osc\"\n";

    fn rig_with(session: &str, queue: usize) -> Rig {
        let (handles, engine) = testutil::stub_engine();
        let mut ctl = Controller::new(load(session).model, handles, None);
        let hub = Arc::new(Hub::new(queue));
        let shared = Arc::new(Shared::new(ctl.rev, ctl.model.telemetry_layout()));
        let (tx, rx) = crossbeam_channel::unbounded();
        let owner = Owner::new(&mut ctl, hub.clone(), shared.clone(), rx);
        Rig { ctl, _engine: engine, owner, hub, shared, tx }
    }

    fn rig() -> Rig {
        rig_with(SESSION, 64)
    }

    fn said(queue: &mut ClientQueue<Outgoing>) -> Vec<Value> {
        let mut said = Vec::new();
        while let Ok(message) = queue.try_recv() {
            match message {
                Outgoing::Text(text) => said.push(serde_json::from_str(&text).unwrap()),
                other => panic!("{other:?}"),
            }
        }
        said
    }

    fn volume(rig: &Rig, value: f32) -> Intent {
        Intent::SetGain { node: rig.ctl.model.instruments[0].node, gain: Gain::Volume, value }
    }

    fn types(messages: &[Value]) -> Vec<&str> {
        messages.iter().map(|m| m["type"].as_str().unwrap()).collect()
    }

    #[test]
    fn a_client_that_subscribes_is_sent_the_snapshot_and_hears_what_follows() {
        let mut rig = rig();
        let (client, mut queue) = rig.hub.connect();
        rig.tx.send(OwnerRequest::Subscribe { client }).unwrap();
        assert_eq!(rig.owner.serve(&mut rig.ctl), 1);
        let first = said(&mut queue);
        assert_eq!(types(&first), ["snapshot", "catalog", "history", "inputs"], "the snapshot, what can be added to it, what can be undone, and which MIDI inputs are open");
        assert_eq!(first[1], json!({"type": "catalog", "rev": 0, "scanning": false, "entries": []}));
        assert_eq!(first[2], json!({"type": "history", "undo": null, "redo": null}));
        assert_eq!(first[3], json!({"type": "inputs", "names": []}), "a program that watches no device has none open");
        assert_eq!(first[0]["snapshot"]["rev"], 0);
        assert_eq!(first[0]["snapshot"]["graph"]["nodes"].as_array().unwrap().len(), 3, "two lanes and the master");

        let intent = volume(&rig, 0.5);
        rig.tx.send(OwnerRequest::Apply { client, req_id: 1, intent }).unwrap();
        rig.owner.serve(&mut rig.ctl);
        let next = said(&mut queue);
        assert_eq!(types(&next), ["event", "history", "ack"], "the event, what can be undone now, then the answer");
        assert_eq!(next[0]["event"]["rev"], 1);
        assert_eq!(next[2], json!({"type": "ack", "reqId": 1, "rev": 1}));
    }

    #[test]
    fn the_snapshot_and_the_events_after_it_are_cut_at_one_revision() {
        let mut rig = rig();
        let (early, mut early_queue) = rig.hub.connect();
        rig.tx.send(OwnerRequest::Subscribe { client: early }).unwrap();
        rig.owner.serve(&mut rig.ctl);
        for n in 0..3 {
            let intent = volume(&rig, 0.1 * (n + 1) as f32);
            rig.tx.send(OwnerRequest::Apply { client: early, req_id: n, intent }).unwrap();
        }
        rig.owner.serve(&mut rig.ctl);
        said(&mut early_queue);

        // A client that comes late starts from where the session is, and then hears only what is new.
        let (late, mut late_queue) = rig.hub.connect();
        rig.tx.send(OwnerRequest::Subscribe { client: late }).unwrap();
        let intent = volume(&rig, 0.9);
        rig.tx.send(OwnerRequest::Apply { client: early, req_id: 9, intent }).unwrap();
        rig.owner.serve(&mut rig.ctl);
        let heard = said(&mut late_queue);
        assert_eq!(types(&heard), ["snapshot", "catalog", "history", "inputs", "event"]);
        assert_eq!(heard[0]["snapshot"]["rev"], 3);
        assert_eq!(heard[4]["event"]["rev"], 4);
    }

    #[test]
    fn a_client_that_has_not_subscribed_gets_its_answers_and_no_events() {
        let mut rig = rig();
        let (subscribed, mut subscribed_queue) = rig.hub.connect();
        let (plain, mut plain_queue) = rig.hub.connect();
        rig.tx.send(OwnerRequest::Subscribe { client: subscribed }).unwrap();
        rig.owner.serve(&mut rig.ctl);
        said(&mut subscribed_queue);

        let intent = volume(&rig, 0.5);
        rig.tx.send(OwnerRequest::Apply { client: plain, req_id: 4, intent }).unwrap();
        rig.owner.serve(&mut rig.ctl);
        assert_eq!(said(&mut plain_queue), [json!({"type": "ack", "reqId": 4, "rev": 1})]);
        assert_eq!(types(&said(&mut subscribed_queue)), ["event", "history"], "another front-end's change reaches the subscribed");
    }

    #[test]
    fn an_intent_that_fails_is_rejected_with_its_reason_and_tells_nobody() {
        let mut rig = rig();
        let (client, mut queue) = rig.hub.connect();
        rig.tx.send(OwnerRequest::Subscribe { client }).unwrap();
        rig.owner.serve(&mut rig.ctl);
        said(&mut queue);
        let ghost = NodeId(9_999);
        rig.tx
            .send(OwnerRequest::Apply { client, req_id: 2, intent: Intent::SetGain { node: ghost, gain: Gain::Volume, value: 1.0 } })
            .unwrap();
        rig.owner.serve(&mut rig.ctl);
        assert_eq!(
            said(&mut queue),
            [json!({"type": "rejected", "reqId": 2, "error": {"code": "notFound", "message": "there is no node 9999"}})]
        );
        assert_eq!(rig.ctl.rev, 0, "nothing was applied");
    }

    #[test]
    fn requests_are_answered_in_the_order_they_came_and_counted() {
        let mut rig = rig();
        let (client, mut queue) = rig.hub.connect();
        for req_id in 0..4 {
            let intent = volume(&rig, 0.1 * req_id as f32);
            rig.tx.send(OwnerRequest::Apply { client, req_id, intent }).unwrap();
        }
        assert_eq!(rig.owner.serve(&mut rig.ctl), 4);
        assert_eq!(rig.owner.serve(&mut rig.ctl), 0, "and then there is nothing");
        let acks: Vec<(u64, u64)> = said(&mut queue).iter().map(|m| (m["reqId"].as_u64().unwrap(), m["rev"].as_u64().unwrap())).collect();
        assert_eq!(acks, [(0, 1), (1, 2), (2, 3), (3, 4)]);
        assert_eq!(rig.shared.rev.load(Ordering::Relaxed), 4, "the revision a new connection is told is current after a request");
    }

    #[test]
    fn a_request_from_a_client_that_has_gone_is_applied_and_the_answer_goes_nowhere() {
        let mut rig = rig();
        let (client, queue) = rig.hub.connect();
        rig.hub.disconnect(client);
        drop(queue);
        let intent = volume(&rig, 0.5);
        rig.tx.send(OwnerRequest::Apply { client, req_id: 1, intent }).unwrap();
        rig.tx.send(OwnerRequest::Subscribe { client }).unwrap();
        assert_eq!(rig.owner.serve(&mut rig.ctl), 2);
        assert_eq!(rig.ctl.rev, 1, "what was asked is done");
    }

    #[test]
    fn a_client_that_does_not_keep_up_is_evicted_and_the_session_goes_on() {
        // (A queue of the four that a client is sent when it subscribes: the snapshot, the catalog, what can be undone and the MIDI inputs that are open.)
        let mut rig = rig_with(SESSION, 4);
        let (slow, mut slow_queue) = rig.hub.connect();
        let (quick, mut quick_queue) = rig.hub.connect();
        for client in [slow, quick] {
            rig.tx.send(OwnerRequest::Subscribe { client }).unwrap();
        }
        rig.owner.serve(&mut rig.ctl);
        said(&mut quick_queue);
        for n in 0..5 {
            let intent = volume(&rig, 0.1 * (n + 1) as f32);
            rig.tx.send(OwnerRequest::Apply { client: quick, req_id: n, intent }).unwrap();
            rig.owner.serve(&mut rig.ctl);
            let told = types(&said(&mut quick_queue)).join(" ");
            // (The first change leaves something to undo, which is told with it; the rest are the same step.)
            assert_eq!(told, if n == 0 { "event history ack" } else { "event ack" });
        }
        assert_eq!(rig.ctl.rev, 5);
        assert_eq!(rig.hub.client_count(), 1, "the slow one is gone");
        assert!(slow_queue.try_recv().is_ok());
        while slow_queue.try_recv().is_ok() {}
        assert!(matches!(slow_queue.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Disconnected)));
    }

    #[test]
    fn a_client_that_asks_to_watch_nodes_is_told_which_are_watched_and_the_controller_has_them() {
        let mut rig = rig();
        let (client, mut queue) = rig.hub.connect();
        let osc = rig.ctl.model.instruments[0].node;
        // The master has a scope of its own and there is no node 9999.
        rig.tx.send(OwnerRequest::Probes { client, nodes: vec![NodeId::MASTER, osc, NodeId(9_999)] }).unwrap();
        rig.owner.serve(&mut rig.ctl);
        assert_eq!(said(&mut queue), [json!({"type": "probes", "nodes": [osc.0]})]);
        assert_eq!(rig.ctl.telemetry.probe_node(0), Some(osc.0), "the ring is the node's, for whoever sends its frames");
        // What it asks for next replaces that.
        rig.tx.send(OwnerRequest::Probes { client, nodes: vec![] }).unwrap();
        rig.owner.serve(&mut rig.ctl);
        assert_eq!(said(&mut queue), [json!({"type": "probes", "nodes": []})]);
        assert_eq!(rig.ctl.telemetry.probe_node(0), None);
    }

    #[test]
    fn a_probe_does_not_change_the_revision_or_tell_anyone_else() {
        let mut rig = rig();
        let (watcher, mut watcher_queue) = rig.hub.connect();
        let (other, mut other_queue) = rig.hub.connect();
        rig.tx.send(OwnerRequest::Subscribe { client: other }).unwrap();
        rig.owner.serve(&mut rig.ctl);
        said(&mut other_queue);
        let osc = rig.ctl.model.instruments[0].node;
        rig.tx.send(OwnerRequest::Probes { client: watcher, nodes: vec![osc] }).unwrap();
        rig.owner.serve(&mut rig.ctl);
        assert_eq!(types(&said(&mut watcher_queue)), ["probes"]);
        assert!(said(&mut other_queue).is_empty());
        assert_eq!(rig.ctl.rev, 0);
        assert!(!rig.ctl.dirty);
    }

    #[test]
    fn the_probes_of_a_client_that_has_gone_are_let_go_of_at_the_next_refresh_and_the_others_are_kept() {
        let mut rig = rig();
        let (gone, _gone_queue) = rig.hub.connect();
        let (staying, _staying_queue) = rig.hub.connect();
        let (first, second) = (rig.ctl.model.instruments[0].node, rig.ctl.model.instruments[1].node);
        rig.tx.send(OwnerRequest::Probes { client: gone, nodes: vec![first] }).unwrap();
        rig.tx.send(OwnerRequest::Probes { client: staying, nodes: vec![second] }).unwrap();
        rig.owner.serve(&mut rig.ctl);
        assert_eq!((rig.ctl.telemetry.probe_node(0), rig.ctl.telemetry.probe_node(1)), (Some(first.0), Some(second.0)));
        rig.hub.disconnect(gone);
        rig.owner.refresh(&rig.ctl);
        assert_eq!(rig.ctl.telemetry.probe_node(0), Some(first.0), "refresh only reads the controller");
        rig.owner.refresh_probes(&mut rig.ctl);
        assert_eq!((rig.ctl.telemetry.probe_node(0), rig.ctl.telemetry.probe_node(1)), (None, Some(second.0)));
    }

    #[test]
    fn the_watch_of_the_terminal_is_kept_when_the_clients_that_have_gone_are_let_go_of() {
        let mut rig = rig();
        let (gone, _gone_queue) = rig.hub.connect();
        let (first, second) = (rig.ctl.model.instruments[0].node, rig.ctl.model.instruments[1].node);
        rig.tx.send(OwnerRequest::Probes { client: gone, nodes: vec![first] }).unwrap();
        rig.owner.serve(&mut rig.ctl);
        // (`tang --web`: the terminal watches a node of its own, and is no connection of the hub's.)
        rig.ctl.set_probes(Watcher::TERMINAL, &[second]);
        assert_eq!((rig.ctl.telemetry.probe_node(0), rig.ctl.telemetry.probe_node(1)), (Some(first.0), Some(second.0)));
        rig.hub.disconnect(gone);
        rig.owner.refresh_probes(&mut rig.ctl);
        assert_eq!((rig.ctl.telemetry.probe_node(0), rig.ctl.telemetry.probe_node(1)), (None, Some(second.0)), "the client's is let go of, the terminal's is kept");
    }

    #[test]
    fn refresh_keeps_the_revision_current_for_the_hello() {
        let mut rig = rig();
        assert_eq!(rig.shared.rev.load(Ordering::Relaxed), 0);
        // The TUI applies an intent on its own, through the same controller.
        let intent = volume(&rig, 0.5);
        rig.ctl.apply(intent).unwrap();
        rig.owner.refresh(&rig.ctl);
        assert_eq!(rig.shared.rev.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn what_the_tui_applies_through_the_controller_reaches_the_web_clients_too() {
        let mut rig = rig();
        let (client, mut queue) = rig.hub.connect();
        rig.tx.send(OwnerRequest::Subscribe { client }).unwrap();
        rig.owner.serve(&mut rig.ctl);
        said(&mut queue);
        let intent = volume(&rig, 0.5);
        rig.ctl.apply(intent).unwrap();
        assert_eq!(types(&said(&mut queue)), ["event"]);
    }

    #[test]
    fn handle_hands_back_the_intent_it_applied_so_that_a_front_end_that_shows_the_session_can_react() {
        let mut rig = rig();
        let (client, _queue) = rig.hub.connect();
        let intent = volume(&rig, 0.5);
        let applied = rig.owner.handle(&mut rig.ctl, OwnerRequest::Apply { client, req_id: 1, intent }).expect("it was applied");
        assert_eq!(applied.rev, 1);
        assert_eq!(applied.events.len(), 1);

        let ghost = NodeId(9_999);
        let refused = OwnerRequest::Apply { client, req_id: 2, intent: Intent::SetGain { node: ghost, gain: Gain::Volume, value: 1.0 } };
        assert_eq!(rig.owner.handle(&mut rig.ctl, refused), None, "nothing was applied");
        assert_eq!(rig.owner.handle(&mut rig.ctl, OwnerRequest::Subscribe { client }), None, "nor is a subscription an intent");
    }

    #[test]
    fn serve_with_hands_back_each_intent_that_was_applied_in_the_order_they_came() {
        let mut rig = rig();
        let (client, _queue) = rig.hub.connect();
        for req_id in 0..3 {
            let intent = volume(&rig, 0.1 * (req_id + 1) as f32);
            rig.tx.send(OwnerRequest::Apply { client, req_id, intent }).unwrap();
        }
        let ghost = NodeId(9_999);
        rig.tx.send(OwnerRequest::Apply { client, req_id: 3, intent: Intent::SetGain { node: ghost, gain: Gain::Volume, value: 1.0 } }).unwrap();
        rig.tx.send(OwnerRequest::Subscribe { client }).unwrap();
        let mut revs = Vec::new();
        assert_eq!(rig.owner.serve_with(&mut rig.ctl, |applied| revs.push(applied.rev)), 5, "every request is counted");
        assert_eq!(revs, [1, 2, 3], "and every intent that was applied is handed back");
    }

    #[test]
    fn what_the_tui_edits_without_an_intent_reaches_the_clients_as_a_snapshot_that_carries_the_layout() {
        let mut rig = rig();
        let (client, mut queue) = rig.hub.connect();
        rig.tx.send(OwnerRequest::Subscribe { client }).unwrap();
        rig.owner.serve(&mut rig.ctl);
        said(&mut queue);

        // The tempo, set in the TUI's popup.
        rig.ctl.model.bpm = 90.0;
        assert!(rig.ctl.resync());
        let told = said(&mut queue);
        assert_eq!(types(&told), ["snapshot"]);
        assert_eq!((told[0]["snapshot"]["rev"].clone(), told[0]["snapshot"]["meta"]["bpm"].clone()), (json!(1), json!(90.0)));
        assert_eq!(rig.shared.rev.load(Ordering::Relaxed), 1, "a client that connects now is told where the session is");

        // A lane, added with `n`. The snapshot carries the new layout, so it is not told again.
        let before = rig.shared.layout().rev;
        rig.ctl.add_lane();
        assert!(rig.ctl.resync());
        let told = said(&mut queue);
        assert_eq!(types(&told), ["snapshot"]);
        assert_eq!(told[0]["snapshot"]["graph"]["nodes"].as_array().unwrap().len(), 4, "three lanes and the master");
        assert_ne!(rig.shared.layout().rev, before, "the frames are made for the new layout at once");
        assert_eq!(told[0]["snapshot"]["telemetry"]["rev"], json!(rig.shared.layout().rev));
        rig.owner.refresh(&rig.ctl);
        assert!(said(&mut queue).is_empty(), "the layout was told in the snapshot");
    }

    fn plugin(name: &str, id: &str, instrument: bool) -> crate::plugin::PluginInfo {
        crate::plugin::PluginInfo { name: name.to_string(), id: id.to_string(), is_instrument: instrument, param_count: 4, preset_count: 2, path: "/where".to_string(), scan_ms: 0 }
    }

    #[test]
    fn a_client_is_told_the_catalog_as_it_is_with_the_snapshot_and_it_keeps_up_with_the_scan() {
        let mut rig = rig();
        // A scan is under way, and has found the built-ins.
        let (batches, scan) = crossbeam_channel::unbounded();
        rig.ctl.catalog.follow(scan);
        batches.send(vec![plugin("Oscillator", "builtin:osc", true)]).unwrap();
        rig.ctl.poll();
        rig.owner.refresh(&rig.ctl);

        let (client, mut queue) = rig.hub.connect();
        rig.tx.send(OwnerRequest::Subscribe { client }).unwrap();
        rig.owner.serve(&mut rig.ctl);
        let first = said(&mut queue);
        assert_eq!(types(&first), ["snapshot", "catalog", "history", "inputs"]);
        assert_eq!(first[1]["rev"], 1);
        assert_eq!(first[1]["scanning"], true, "the scan is not over");
        assert_eq!(first[1]["entries"], json!([{"id": "builtin:osc", "name": "Oscillator", "format": "Built-in", "isInstrument": true, "params": 4, "presets": 2}]));
        assert!(!first[1].to_string().contains("where"), "where it is installed is nobody's business");

        // Nothing changed: nothing is said.
        rig.ctl.poll();
        rig.owner.refresh(&rig.ctl);
        assert!(said(&mut queue).is_empty());

        // The scan finds more: the catalog is told again, whole, with the revision after.
        batches.send(vec![plugin("Reverb", "builtin:reverb", false)]).unwrap();
        rig.ctl.poll();
        rig.owner.refresh(&rig.ctl);
        let told = said(&mut queue);
        assert_eq!(types(&told), ["catalog"]);
        assert_eq!((told[0]["rev"].clone(), told[0]["scanning"].clone()), (json!(2), json!(true)));
        assert_eq!(told[0]["entries"].as_array().unwrap().len(), 2);

        // The scan ends: the same plugins, and no more scanning, which a picker shows.
        drop(batches);
        rig.ctl.poll();
        rig.owner.refresh(&rig.ctl);
        let told = said(&mut queue);
        assert_eq!(types(&told), ["catalog"]);
        assert_eq!((told[0]["rev"].clone(), told[0]["scanning"].clone()), (json!(2), json!(false)));
        rig.owner.refresh(&rig.ctl);
        assert!(said(&mut queue).is_empty(), "once");
    }

    #[test]
    fn what_undo_and_redo_would_do_is_told_to_the_subscribed_with_the_answer_when_it_changes_and_not_otherwise() {
        let mut rig = rig();
        let (client, mut queue) = rig.hub.connect();
        let (_plain, mut plain_queue) = rig.hub.connect();
        rig.tx.send(OwnerRequest::Subscribe { client }).unwrap();
        rig.owner.serve(&mut rig.ctl);
        said(&mut queue);
        rig.owner.refresh(&rig.ctl);
        assert!(said(&mut queue).is_empty(), "nothing changed");

        // A change leaves something to undo: told after the event and before the answer.
        let intent = volume(&rig, 0.5);
        rig.tx.send(OwnerRequest::Apply { client, req_id: 1, intent }).unwrap();
        rig.owner.serve(&mut rig.ctl);
        let told = said(&mut queue);
        assert_eq!(types(&told), ["event", "history", "ack"]);
        assert_eq!(told[1], json!({"type": "history", "undo": "set volume", "redo": null}));
        rig.owner.refresh(&rig.ctl);
        assert!(said(&mut queue).is_empty(), "told once");

        // The same kind of change again is the same step, which is no news; Undo is: what it did can be redone.
        let intent = volume(&rig, 0.6);
        rig.tx.send(OwnerRequest::Apply { client, req_id: 2, intent }).unwrap();
        rig.owner.serve(&mut rig.ctl);
        assert_eq!(types(&said(&mut queue)), ["event", "ack"]);
        rig.tx.send(OwnerRequest::Apply { client, req_id: 3, intent: Intent::Undo }).unwrap();
        rig.owner.serve(&mut rig.ctl);
        let told = said(&mut queue);
        assert_eq!(types(&told), ["event", "history", "ack"], "what Undo did is an event like the others");
        assert_eq!(told[0]["event"]["type"], "gainSet");
        assert_eq!(told[1], json!({"type": "history", "undo": null, "redo": "set volume"}));
        assert!(said(&mut plain_queue).is_empty(), "a client that has not subscribed is told nothing");

        // What the TUI does through the controller is told by the refresh.
        let intent = volume(&rig, 0.9);
        rig.ctl.apply(intent).unwrap();
        assert_eq!(types(&said(&mut queue)), ["event"]);
        rig.owner.refresh(&rig.ctl);
        assert_eq!(said(&mut queue), [json!({"type": "history", "undo": "set volume", "redo": null})]);
    }

    #[test]
    fn an_owner_that_starts_when_something_can_be_undone_does_not_tell_it_again_to_the_first_client() {
        // (The TUI starts the web interface on a session it has been changing.)
        let (handles, _engine) = testutil::stub_engine();
        let mut ctl = Controller::new(load(SESSION).model, handles, None);
        let node = ctl.model.instruments[0].node;
        ctl.apply(Intent::SetGain { node, gain: Gain::Volume, value: 0.5 }).unwrap();
        let hub = Arc::new(Hub::new(64));
        let shared = Arc::new(Shared::new(ctl.rev, ctl.model.telemetry_layout()));
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut owner = Owner::new(&mut ctl, hub.clone(), shared, rx);
        let (client, mut queue) = hub.connect();
        tx.send(OwnerRequest::Subscribe { client }).unwrap();
        owner.serve(&mut ctl);
        let told = said(&mut queue);
        assert_eq!(told[2], json!({"type": "history", "undo": "set volume", "redo": null}));
        owner.refresh(&ctl);
        assert!(said(&mut queue).is_empty(), "it was told with the snapshot");
    }

    #[test]
    fn a_client_that_subscribes_late_is_told_what_undo_would_do_then_and_nobody_is_told_it_twice() {
        let mut rig = rig();
        let (early, mut early_queue) = rig.hub.connect();
        rig.tx.send(OwnerRequest::Subscribe { client: early }).unwrap();
        let intent = volume(&rig, 0.5);
        rig.tx.send(OwnerRequest::Apply { client: early, req_id: 1, intent }).unwrap();
        rig.owner.serve(&mut rig.ctl);
        said(&mut early_queue);
        let (late, mut late_queue) = rig.hub.connect();
        rig.tx.send(OwnerRequest::Subscribe { client: late }).unwrap();
        rig.owner.serve(&mut rig.ctl);
        let told = said(&mut late_queue);
        assert_eq!(types(&told), ["snapshot", "catalog", "history", "inputs"]);
        assert_eq!(told[2], json!({"type": "history", "undo": "set volume", "redo": null}));
        rig.owner.refresh(&rig.ctl);
        assert!(said(&mut early_queue).is_empty() && said(&mut late_queue).is_empty());
    }

    /// A watcher of the devices whose open inputs a test sets, as the MIDI manager has them.
    #[derive(Clone, Default)]
    struct Inputs(std::rc::Rc<std::cell::RefCell<Vec<String>>>);

    impl Inputs {
        fn plug_in(&self, name: &str) {
            self.0.borrow_mut().push(name.to_string());
        }

        fn unplug(&self, name: &str) {
            self.0.borrow_mut().retain(|open| open != name);
        }
    }

    impl DeviceWatcher for Inputs {
        fn poll_new_devices(&mut self) -> crate::midi::DeviceChanges {
            crate::midi::DeviceChanges::default()
        }

        fn sources_of(&self, _: &str) -> crate::source::SourceSet {
            crate::source::SourceSet::NONE
        }

        fn open_names(&self) -> Vec<String> {
            self.0.borrow().clone()
        }
    }

    #[test]
    fn a_client_is_told_which_inputs_are_open_with_the_snapshot_and_again_when_a_device_comes_or_goes_and_not_otherwise() {
        let mut rig = rig();
        let devices = Inputs::default();
        devices.plug_in("Seaboard RISE");
        rig.ctl.watch_devices(Box::new(devices.clone()));
        // (Nobody is there to be told when the device is found: the list that the owner started with was empty.)
        rig.owner.refresh(&rig.ctl);

        let (client, mut queue) = rig.hub.connect();
        let (_plain, mut plain_queue) = rig.hub.connect();
        rig.tx.send(OwnerRequest::Subscribe { client }).unwrap();
        rig.owner.serve(&mut rig.ctl);
        let first = said(&mut queue);
        assert_eq!(types(&first), ["snapshot", "catalog", "history", "inputs"]);
        assert_eq!(first[3], json!({"type": "inputs", "names": ["Seaboard RISE"]}), "the list as it is when the client subscribes");
        rig.owner.refresh(&rig.ctl);
        assert!(said(&mut queue).is_empty(), "nothing changed: nothing is said");

        devices.plug_in("Launchkey Mini MK4 37 MIDI Out");
        rig.owner.refresh(&rig.ctl);
        assert_eq!(said(&mut queue), [json!({"type": "inputs", "names": ["Seaboard RISE", "Launchkey Mini MK4 37 MIDI Out"]})], "the whole list, whole");
        rig.owner.refresh(&rig.ctl);
        assert!(said(&mut queue).is_empty(), "once");

        devices.unplug("Seaboard RISE");
        rig.owner.refresh(&rig.ctl);
        assert_eq!(said(&mut queue), [json!({"type": "inputs", "names": ["Launchkey Mini MK4 37 MIDI Out"]})]);

        devices.unplug("Launchkey Mini MK4 37 MIDI Out");
        rig.owner.refresh(&rig.ctl);
        assert_eq!(said(&mut queue), [json!({"type": "inputs", "names": []})], "none is open: a list that is empty, and not no word");
        assert!(said(&mut plain_queue).is_empty(), "a client that has not subscribed is told nothing");

        // A client that comes late is told the list as it is, and nobody is told it twice.
        devices.plug_in("IAC Driver Bus 1");
        rig.owner.refresh(&rig.ctl);
        said(&mut queue);
        let (late, mut late_queue) = rig.hub.connect();
        rig.tx.send(OwnerRequest::Subscribe { client: late }).unwrap();
        rig.owner.serve(&mut rig.ctl);
        assert_eq!(said(&mut late_queue)[3], json!({"type": "inputs", "names": ["IAC Driver Bus 1"]}));
        rig.owner.refresh(&rig.ctl);
        assert!(said(&mut queue).is_empty() && said(&mut late_queue).is_empty());
    }

    #[test]
    fn an_owner_that_starts_when_devices_are_open_does_not_tell_them_again_to_the_first_client() {
        // (The TUI starts the web interface on a program that has found its devices.)
        let (handles, _engine) = testutil::stub_engine();
        let mut ctl = Controller::new(load(SESSION).model, handles, None);
        let devices = Inputs::default();
        devices.plug_in("Seaboard RISE");
        ctl.watch_devices(Box::new(devices.clone()));
        let hub = Arc::new(Hub::new(64));
        let shared = Arc::new(Shared::new(ctl.rev, ctl.model.telemetry_layout()));
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut owner = Owner::new(&mut ctl, hub.clone(), shared, rx);
        let (client, mut queue) = hub.connect();
        tx.send(OwnerRequest::Subscribe { client }).unwrap();
        owner.serve(&mut ctl);
        assert_eq!(said(&mut queue)[3], json!({"type": "inputs", "names": ["Seaboard RISE"]}));
        owner.refresh(&ctl);
        assert!(said(&mut queue).is_empty(), "it was told with the snapshot");
    }

    #[test]
    fn the_catalog_is_not_told_to_a_client_that_has_not_subscribed() {
        let mut rig = rig();
        let (_plain, mut plain_queue) = rig.hub.connect();
        let (batches, scan) = crossbeam_channel::unbounded();
        rig.ctl.catalog.follow(scan);
        batches.send(vec![plugin("Oscillator", "builtin:osc", true)]).unwrap();
        rig.ctl.poll();
        rig.owner.refresh(&rig.ctl);
        assert!(said(&mut plain_queue).is_empty());
    }

    #[test]
    fn a_change_of_layout_is_told_once_to_the_subscribed_and_kept_for_the_frames() {
        let mut rig = rig();
        let (subscribed, mut subscribed_queue) = rig.hub.connect();
        let (_plain, mut plain_queue) = rig.hub.connect();
        rig.tx.send(OwnerRequest::Subscribe { client: subscribed }).unwrap();
        rig.owner.serve(&mut rig.ctl);
        said(&mut subscribed_queue);

        rig.owner.refresh(&rig.ctl);
        assert!(said(&mut subscribed_queue).is_empty(), "nothing changed");

        // A lane is added behind the owner's back (the TUI's `n`).
        let before = rig.shared.layout().rev;
        rig.ctl.add_lane();
        rig.owner.refresh(&rig.ctl);
        let told = said(&mut subscribed_queue);
        assert_eq!(types(&told), ["layout"]);
        assert_ne!(rig.shared.layout().rev, before);
        assert_eq!(told[0]["layout"]["rev"], json!(rig.shared.layout().rev));
        assert!(said(&mut plain_queue).is_empty());

        rig.owner.refresh(&rig.ctl);
        assert!(said(&mut subscribed_queue).is_empty(), "once");
    }
}
