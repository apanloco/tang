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
use crate::app::intent::Applied;
use crate::app::protocol::{ServerMessage, WireError};
use crate::web::Shared;
use crate::web::connection::OwnerRequest;
use crate::web::hub::{ClientId, Hub, HubSink, Outgoing};

pub struct Owner {
    hub: Arc<Hub>,
    shared: Arc<Shared>,
    requests: Receiver<OwnerRequest>,
}

impl Owner {
    /// An owner for `ctl`: from now on what the controller applies is told to the
    /// hub's subscribed clients, whichever front-end asked for it, and so is a
    /// snapshot when the session changes in a way no intent describes.
    pub fn new(ctl: &mut Controller, hub: Arc<Hub>, shared: Arc<Shared>, requests: Receiver<OwnerRequest>) -> Owner {
        ctl.subscribe(Box::new(HubSink { hub: hub.clone(), shared: shared.clone() }));
        Owner { hub, shared, requests }
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
    #[cfg_attr(not(test), expect(dead_code, reason = "the TUI shows it, from the commit that has it serve the web interface"))]
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
                // Subscribed and sent the snapshot in one step: no event can come between.
                if self.hub.subscribe(client) {
                    self.send(client, &ServerMessage::Snapshot { snapshot: ctl.snapshot() });
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
                self.send(client, &reply);
                applied
            }
        };
        self.shared.rev.store(ctl.rev, Ordering::Relaxed);
        applied
    }

    /// Keep what the connections read current: the session's revision, and the
    /// meaning of the telemetry slots, which the clients are told when it changes. Call
    /// it regularly (a few times a second): the model can change without an intent,
    /// under the TUI's hands.
    pub fn refresh(&mut self, ctl: &Controller) {
        self.shared.rev.store(ctl.rev, Ordering::Relaxed);
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
        assert_eq!(types(&first), ["snapshot"]);
        assert_eq!(first[0]["snapshot"]["rev"], 0);
        assert_eq!(first[0]["snapshot"]["graph"]["nodes"].as_array().unwrap().len(), 3, "two lanes and the master");

        let intent = volume(&rig, 0.5);
        rig.tx.send(OwnerRequest::Apply { client, req_id: 1, intent }).unwrap();
        rig.owner.serve(&mut rig.ctl);
        let next = said(&mut queue);
        assert_eq!(types(&next), ["event", "ack"], "the event, then the answer");
        assert_eq!(next[0]["event"]["rev"], 1);
        assert_eq!(next[1], json!({"type": "ack", "reqId": 1, "rev": 1}));
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
        assert_eq!(types(&heard), ["snapshot", "event"]);
        assert_eq!(heard[0]["snapshot"]["rev"], 3);
        assert_eq!(heard[1]["event"]["rev"], 4);
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
        assert_eq!(types(&said(&mut subscribed_queue)), ["event"], "another front-end's change reaches the subscribed");
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
        let mut rig = rig_with(SESSION, 2);
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
            assert_eq!(types(&said(&mut quick_queue)), ["event", "ack"]);
        }
        assert_eq!(rig.ctl.rev, 5);
        assert_eq!(rig.hub.client_count(), 1, "the slow one is gone");
        assert!(slow_queue.try_recv().is_ok());
        while slow_queue.try_recv().is_ok() {}
        assert!(matches!(slow_queue.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Disconnected)));
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
        rig.ctl.global_bpm = 90.0;
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
