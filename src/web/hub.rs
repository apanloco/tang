//! The clients, and what is sent to them; see `docs/web-ui.md` §3.2.
//!
//! One `Hub` is shared by the owner of the controller (which broadcasts what it
//! applied) and the connections (which register, subscribe and say which telemetry
//! they want). Each client has a bounded queue, and the hub never waits for one: a
//! full queue means the client is not keeping up, which for the events that carry the
//! session's state is the end of it (the client is evicted, and reconnects and starts
//! from a snapshot), and for telemetry, which is only ever the latest values anyway,
//! just a frame that is skipped.
//!
//! The hub's methods are synchronous and short (a lock and `try_send`), so the
//! owner thread can call them from inside the controller's `apply`.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc::{self, error::TrySendError};

use crate::app::intent::Applied;
use crate::app::model::NodeId;
use crate::app::protocol::{ServerMessage, TelemetryChannel};
use crate::app::snapshot::Snapshot;
use crate::app::subscribers::EventSink;
use crate::web::Shared;

/// Names a connection for as long as it is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ClientId(u64);

impl ClientId {
    /// The number the client is known by to the controller's watchers (`app::probes::Watcher`).
    pub fn number(self) -> u64 {
        self.0
    }
}

/// A frame to send to a client: JSON text, or a binary telemetry frame. Shared, so
/// that sending the same thing to many clients serializes it once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outgoing {
    Text(Arc<str>),
    Binary(Arc<[u8]>),
}

struct Client {
    tx: mpsc::Sender<Outgoing>,
    /// Gets the events from now on (the snapshot it was sent says where they start).
    subscribed: bool,
    channels: HashSet<TelemetryChannel>,
    /// The nodes whose signal the client wants (`ClientMessage::Probes`).
    probes: HashSet<NodeId>,
}

pub struct Hub {
    clients: Mutex<HashMap<ClientId, Client>>,
    next_id: AtomicU64,
    /// How many frames a client's queue holds.
    queue: usize,
}

impl Hub {
    pub fn new(queue: usize) -> Hub {
        Hub { clients: Mutex::new(HashMap::new()), next_id: AtomicU64::new(0), queue: queue.max(1) }
    }

    /// Register a client: its id, and the end its connection reads what is sent to it
    /// from. It hears nothing until it subscribes. When the receiver yields `None` the
    /// client has been evicted.
    pub fn connect(&self) -> (ClientId, mpsc::Receiver<Outgoing>) {
        let id = ClientId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = mpsc::channel(self.queue);
        self.lock().insert(id, Client { tx, subscribed: false, channels: HashSet::new(), probes: HashSet::new() });
        (id, rx)
    }

    /// Forget a client that has gone.
    pub fn disconnect(&self, id: ClientId) {
        self.lock().remove(&id);
    }

    /// How many clients are connected.
    pub fn client_count(&self) -> usize {
        self.lock().len()
    }

    /// Send the client the events from now on. False if it is not there (any more).
    pub fn subscribe(&self, id: ClientId) -> bool {
        self.lock().get_mut(&id).map(|client| client.subscribed = true).is_some()
    }

    /// Which telemetry the client wants, replacing what it wanted.
    pub fn set_channels(&self, id: ClientId, channels: &[TelemetryChannel]) -> bool {
        self.lock().get_mut(&id).map(|client| client.channels = channels.iter().copied().collect()).is_some()
    }

    /// Which nodes the client wants the signal of, replacing what it wanted. False if it is not there (any more).
    pub fn set_probes(&self, id: ClientId, nodes: &[NodeId]) -> bool {
        self.lock().get_mut(&id).map(|client| client.probes = nodes.iter().copied().collect()).is_some()
    }

    /// Whether any client wants the signal of `node`, so that nobody builds a frame for nobody.
    pub fn probe_wanted(&self, node: NodeId) -> bool {
        self.lock().values().any(|client| client.probes.contains(&node))
    }

    /// Queue a probe `frame` of `node` for every client that wants it. A client whose queue is full misses this frame, and is not evicted: the next one is newer.
    pub fn send_probe(&self, node: NodeId, frame: &Outgoing) {
        let mut gone = Vec::new();
        let mut clients = self.lock();
        for (id, client) in clients.iter() {
            if client.probes.contains(&node) {
                if let Err(TrySendError::Closed(_)) = client.tx.try_send(frame.clone()) {
                    gone.push(*id);
                }
            }
        }
        for id in gone {
            clients.remove(&id);
        }
    }

    /// The numbers (`ClientId::number`) of the clients that are connected.
    pub fn connected(&self) -> HashSet<u64> {
        self.lock().keys().map(|id| id.0).collect()
    }

    /// Whether any client wants `channel`, so that nobody builds frames for nobody.
    pub fn wanted(&self, channel: TelemetryChannel) -> bool {
        self.lock().values().any(|client| client.channels.contains(&channel))
    }

    /// Queue `message` for one client. If its queue is full the client is evicted
    /// and this is false; so it is for a client that is not there.
    pub fn send(&self, id: ClientId, message: Outgoing) -> bool {
        let mut clients = self.lock();
        let Some(client) = clients.get(&id) else { return false };
        match client.tx.try_send(message) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) | Err(TrySendError::Closed(_)) => {
                clients.remove(&id);
                false
            }
        }
    }

    /// Queue `message` for every client that has subscribed; those whose queue is
    /// full are evicted. Returns them.
    pub fn broadcast(&self, message: &Outgoing) -> Vec<ClientId> {
        let mut clients = self.lock();
        let mut evicted = Vec::new();
        for (id, client) in clients.iter() {
            if client.subscribed && client.tx.try_send(message.clone()).is_err() {
                evicted.push(*id);
            }
        }
        for id in &evicted {
            clients.remove(id);
        }
        evicted
    }

    /// Queue a telemetry `frame` for every client that wants `channel`. A client whose
    /// queue is full misses this frame, and is not evicted: the next one is newer.
    pub fn send_telemetry(&self, channel: TelemetryChannel, frame: &Outgoing) {
        let mut gone = Vec::new();
        let mut clients = self.lock();
        for (id, client) in clients.iter() {
            if client.channels.contains(&channel) {
                if let Err(TrySendError::Closed(_)) = client.tx.try_send(frame.clone()) {
                    gone.push(*id);
                }
            }
        }
        for id in gone {
            clients.remove(&id);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<ClientId, Client>> {
        // A panic while the lock is held cannot leave the map half-edited (each edit is one call).
        self.clients.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// What the controller tells when it applies an intent, passed on to the clients: an
/// event message for each event, to every client that has subscribed. And when it
/// changes in a way no intent describes (`Controller::resync`), the snapshot to start
/// again from.
pub struct HubSink {
    pub hub: Arc<Hub>,
    /// What the connections and the telemetry frames read: a snapshot brings the
    /// revision and the layout it carries, and they follow at once.
    pub shared: Arc<Shared>,
}

impl EventSink for HubSink {
    fn applied(&mut self, applied: &Applied) {
        for event in applied.stamped() {
            let json = serde_json::to_string(&ServerMessage::Event { event }).expect("an event is serializable");
            self.hub.broadcast(&Outgoing::Text(json.into()));
        }
    }

    fn resync(&mut self, snapshot: &Snapshot) {
        self.shared.rev.store(snapshot.rev, Ordering::Relaxed);
        self.shared.set_layout(snapshot.telemetry.clone());
        let json = serde_json::to_string(&ServerMessage::Snapshot { snapshot: snapshot.clone() }).expect("a snapshot is serializable");
        self.hub.broadcast(&Outgoing::Text(json.into()));
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc::error::TryRecvError;

    use super::*;
    use crate::app::intent::{Event, Gain};
    use crate::app::model::NodeId;

    fn text(s: &str) -> Outgoing {
        Outgoing::Text(s.into())
    }

    fn drain(rx: &mut mpsc::Receiver<Outgoing>) -> Vec<Outgoing> {
        let mut got = Vec::new();
        while let Ok(message) = rx.try_recv() {
            got.push(message);
        }
        got
    }

    #[test]
    fn clients_have_ids_of_their_own_and_are_counted() {
        let hub = Hub::new(8);
        let (a, _ra) = hub.connect();
        let (b, _rb) = hub.connect();
        assert_ne!(a, b);
        assert_eq!(hub.client_count(), 2);
        hub.disconnect(a);
        assert_eq!(hub.client_count(), 1);
        hub.disconnect(a);
        assert_eq!(hub.client_count(), 1, "twice is the same as once");
        let (c, _rc) = hub.connect();
        assert!(c != a && c != b, "ids are not reused");
    }

    #[test]
    fn what_is_sent_to_a_client_arrives_in_order_and_only_there() {
        let hub = Hub::new(8);
        let (a, mut ra) = hub.connect();
        let (_b, mut rb) = hub.connect();
        assert!(hub.send(a, text("one")));
        assert!(hub.send(a, Outgoing::Binary(vec![1, 2, 3].into())));
        assert!(hub.send(a, text("three")));
        assert_eq!(drain(&mut ra), [text("one"), Outgoing::Binary(vec![1, 2, 3].into()), text("three")]);
        assert!(drain(&mut rb).is_empty());
    }

    #[test]
    fn a_broadcast_reaches_the_clients_that_subscribed_and_no_others() {
        let hub = Hub::new(8);
        let (a, mut ra) = hub.connect();
        let (_b, mut rb) = hub.connect();
        let (c, mut rc) = hub.connect();
        assert!(hub.subscribe(a));
        assert!(hub.subscribe(c));
        assert!(hub.broadcast(&text("event")).is_empty());
        assert_eq!(drain(&mut ra), [text("event")]);
        assert!(drain(&mut rb).is_empty(), "b has not subscribed");
        assert_eq!(drain(&mut rc), [text("event")]);
    }

    #[test]
    fn subscribing_a_client_that_is_gone_says_so() {
        let hub = Hub::new(8);
        let (a, _ra) = hub.connect();
        hub.disconnect(a);
        assert!(!hub.subscribe(a));
        assert!(!hub.set_channels(a, &[TelemetryChannel::Meters]));
        assert!(!hub.send(a, text("x")));
    }

    #[test]
    fn a_client_that_falls_behind_is_evicted_and_the_others_do_not_notice() {
        let hub = Hub::new(3);
        let (slow, mut slow_rx) = hub.connect();
        let (quick, mut quick_rx) = hub.connect();
        hub.subscribe(slow);
        hub.subscribe(quick);
        let mut evicted = Vec::new();
        for n in 0..5 {
            evicted.extend(hub.broadcast(&text(&n.to_string())));
            // The quick one reads as it goes; the slow one never does.
            assert_eq!(drain(&mut quick_rx), [text(&n.to_string())]);
        }
        assert_eq!(evicted, [slow], "evicted once, on the broadcast that did not fit");
        assert_eq!(hub.client_count(), 1);
        // What was queued is still there to read, and then the queue is over.
        assert_eq!(drain(&mut slow_rx), [text("0"), text("1"), text("2")]);
        assert!(matches!(slow_rx.try_recv(), Err(TryRecvError::Disconnected)), "evicted: nothing more will come");
        assert!(hub.broadcast(&text("again")).is_empty(), "and it is not evicted again");
    }

    #[test]
    fn sending_to_a_full_client_evicts_it() {
        let hub = Hub::new(2);
        let (a, mut ra) = hub.connect();
        assert!(hub.send(a, text("1")));
        assert!(hub.send(a, text("2")));
        assert!(!hub.send(a, text("3")), "the queue holds two");
        assert_eq!(hub.client_count(), 0);
        assert_eq!(drain(&mut ra), [text("1"), text("2")]);
        assert!(matches!(ra.try_recv(), Err(TryRecvError::Disconnected)));
    }

    #[test]
    fn a_client_whose_connection_has_gone_is_forgotten_when_something_is_sent_to_it() {
        let hub = Hub::new(8);
        let (a, ra) = hub.connect();
        hub.subscribe(a);
        drop(ra);
        assert_eq!(hub.broadcast(&text("x")), [a]);
        assert_eq!(hub.client_count(), 0);
    }

    #[test]
    fn telemetry_goes_to_the_clients_that_asked_for_that_channel() {
        let hub = Hub::new(8);
        let (a, mut ra) = hub.connect();
        let (b, mut rb) = hub.connect();
        let (_c, mut rc) = hub.connect();
        hub.set_channels(a, &[TelemetryChannel::Meters]);
        hub.set_channels(b, &[TelemetryChannel::Meters, TelemetryChannel::Modulation]);
        assert!(hub.wanted(TelemetryChannel::Meters) && hub.wanted(TelemetryChannel::Modulation));
        hub.send_telemetry(TelemetryChannel::Modulation, &Outgoing::Binary(vec![9].into()));
        hub.send_telemetry(TelemetryChannel::Meters, &Outgoing::Binary(vec![7].into()));
        assert_eq!(drain(&mut ra), [Outgoing::Binary(vec![7].into())]);
        assert_eq!(drain(&mut rb), [Outgoing::Binary(vec![9].into()), Outgoing::Binary(vec![7].into())]);
        assert!(drain(&mut rc).is_empty());
    }

    #[test]
    fn telemetry_does_not_need_a_subscription_to_events() {
        let hub = Hub::new(8);
        let (a, mut ra) = hub.connect();
        hub.set_channels(a, &[TelemetryChannel::Meters]);
        hub.send_telemetry(TelemetryChannel::Meters, &Outgoing::Binary(vec![1].into()));
        assert_eq!(drain(&mut ra).len(), 1);
        assert!(hub.broadcast(&text("event")).is_empty());
        assert!(drain(&mut ra).is_empty(), "but events still wait for the subscription");
    }

    #[test]
    fn setting_the_channels_replaces_them_and_an_empty_list_stops_them() {
        let hub = Hub::new(8);
        let (a, mut ra) = hub.connect();
        hub.set_channels(a, &[TelemetryChannel::Meters]);
        hub.set_channels(a, &[TelemetryChannel::Modulation]);
        assert!(!hub.wanted(TelemetryChannel::Meters));
        hub.send_telemetry(TelemetryChannel::Meters, &Outgoing::Binary(vec![1].into()));
        assert!(drain(&mut ra).is_empty());
        hub.set_channels(a, &[]);
        assert!(!hub.wanted(TelemetryChannel::Modulation), "nobody wants any: nobody builds frames");
    }

    #[test]
    fn a_client_that_cannot_keep_up_with_telemetry_misses_frames_and_stays() {
        let hub = Hub::new(2);
        let (a, mut ra) = hub.connect();
        hub.set_channels(a, &[TelemetryChannel::Meters]);
        for n in 0..5u8 {
            hub.send_telemetry(TelemetryChannel::Meters, &Outgoing::Binary(vec![n].into()));
        }
        assert_eq!(hub.client_count(), 1, "not evicted");
        assert_eq!(drain(&mut ra), [Outgoing::Binary(vec![0].into()), Outgoing::Binary(vec![1].into())], "the first two fit");
    }

    #[test]
    fn a_probe_goes_to_the_clients_that_watch_its_node_and_to_no_others() {
        let hub = Hub::new(8);
        let (a, mut ra) = hub.connect();
        let (b, mut rb) = hub.connect();
        let (_c, mut rc) = hub.connect();
        hub.set_probes(a, &[NodeId(7), NodeId(8)]);
        hub.set_probes(b, &[NodeId(8)]);
        assert!(hub.probe_wanted(NodeId(7)) && hub.probe_wanted(NodeId(8)) && !hub.probe_wanted(NodeId(9)));
        hub.send_probe(NodeId(7), &Outgoing::Binary(vec![7].into()));
        hub.send_probe(NodeId(8), &Outgoing::Binary(vec![8].into()));
        hub.send_probe(NodeId(9), &Outgoing::Binary(vec![9].into()));
        assert_eq!(drain(&mut ra), [Outgoing::Binary(vec![7].into()), Outgoing::Binary(vec![8].into())]);
        assert_eq!(drain(&mut rb), [Outgoing::Binary(vec![8].into())]);
        assert!(drain(&mut rc).is_empty());
    }

    #[test]
    fn setting_the_probes_replaces_them_and_a_client_that_has_gone_watches_nothing() {
        let hub = Hub::new(8);
        let (a, mut ra) = hub.connect();
        assert!(hub.set_probes(a, &[NodeId(7)]));
        hub.set_probes(a, &[NodeId(8)]);
        assert!(!hub.probe_wanted(NodeId(7)) && hub.probe_wanted(NodeId(8)));
        hub.send_probe(NodeId(7), &Outgoing::Binary(vec![1].into()));
        assert!(drain(&mut ra).is_empty());
        hub.set_probes(a, &[]);
        assert!(!hub.probe_wanted(NodeId(8)), "nobody wants any: nobody builds frames");
        hub.set_probes(a, &[NodeId(8)]);
        hub.disconnect(a);
        assert!(!hub.probe_wanted(NodeId(8)));
        assert!(!hub.set_probes(a, &[NodeId(8)]), "a client that is not there says so");
    }

    #[test]
    fn a_probe_for_a_client_whose_connection_has_gone_forgets_it_and_one_that_falls_behind_misses_frames() {
        let hub = Hub::new(1);
        let (a, mut ra) = hub.connect();
        let (b, rb) = hub.connect();
        hub.set_probes(a, &[NodeId(7)]);
        hub.set_probes(b, &[NodeId(7)]);
        drop(rb);
        hub.send_probe(NodeId(7), &Outgoing::Binary(vec![1].into()));
        hub.send_probe(NodeId(7), &Outgoing::Binary(vec![2].into()));
        assert_eq!(hub.client_count(), 1, "the one whose connection was gone is forgotten and the slow one is not evicted");
        assert_eq!(drain(&mut ra), [Outgoing::Binary(vec![1].into())], "only the first fits");
    }

    #[test]
    fn the_numbers_of_the_clients_that_are_connected_are_known() {
        let hub = Hub::new(8);
        let (a, _ra) = hub.connect();
        let (b, _rb) = hub.connect();
        assert_eq!(hub.connected(), [a.number(), b.number()].into_iter().collect());
        hub.disconnect(a);
        assert_eq!(hub.connected(), [b.number()].into_iter().collect());
    }

    #[test]
    fn a_telemetry_client_whose_connection_has_gone_is_forgotten() {
        let hub = Hub::new(8);
        let (a, ra) = hub.connect();
        hub.set_channels(a, &[TelemetryChannel::Meters]);
        drop(ra);
        hub.send_telemetry(TelemetryChannel::Meters, &Outgoing::Binary(vec![1].into()));
        assert_eq!(hub.client_count(), 0);
    }

    #[test]
    fn what_the_controller_applies_goes_out_as_event_messages_to_the_subscribed() {
        let hub = Arc::new(Hub::new(8));
        let (a, mut ra) = hub.connect();
        let (_b, mut rb) = hub.connect();
        hub.subscribe(a);
        let mut sink = HubSink { hub: hub.clone(), shared: shared() };
        sink.applied(&Applied::of(
            4,
            vec![
                Event::ParamSet { node: NodeId(3), param: 1, value: 0.5 },
                Event::GainSet { node: NodeId(3), gain: Gain::Mix, value: 1.0 },
            ],
        ));
        let got = drain(&mut ra);
        let json: Vec<serde_json::Value> = got
            .iter()
            .map(|m| match m {
                Outgoing::Text(t) => serde_json::from_str(t).unwrap(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            json,
            [
                serde_json::json!({"type": "event", "event": {"rev": 4, "type": "paramSet", "node": 3, "param": 1, "value": 0.5}}),
                serde_json::json!({"type": "event", "event": {"rev": 4, "type": "gainSet", "node": 3, "gain": "mix", "value": 1.0}}),
            ]
        );
        assert!(drain(&mut rb).is_empty());
    }

    fn shared() -> Arc<Shared> {
        Arc::new(Shared::new(0, crate::app::model::SessionModel::default().telemetry_layout()))
    }

    #[test]
    fn a_snapshot_the_controller_resyncs_with_goes_to_the_subscribed_clients() {
        let hub = Arc::new(Hub::new(16));
        let (subscribed, mut subscribed_queue) = hub.connect();
        let (_plain, mut plain_queue) = hub.connect();
        hub.subscribe(subscribed);
        let (handles, _engine) = crate::app::testutil::stub_engine();
        let mut ctl = crate::app::controller::Controller::new(crate::app::testutil::load("[[instrument]]\nplugin = \"builtin:osc\"\n").model, handles, None);
        let shared = Arc::new(Shared::new(ctl.rev, ctl.model.telemetry_layout()));
        let mut sink = HubSink { hub: hub.clone(), shared: shared.clone() };

        ctl.add_lane();
        let mut snapshot = ctl.snapshot();
        snapshot.rev = 7;
        sink.resync(&snapshot);

        // (Through the text, as the wire has it: an f32 as its shortest text, not widened to an f64.)
        let text = serde_json::to_string(&ServerMessage::Snapshot { snapshot: snapshot.clone() }).unwrap();
        let expected: serde_json::Value = serde_json::from_str(&text).unwrap();
        let told: Vec<serde_json::Value> = drain(&mut subscribed_queue)
            .iter()
            .map(|m| match m {
                Outgoing::Text(t) => serde_json::from_str(t).unwrap(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(told, [expected]);
        assert!(drain(&mut plain_queue).is_empty(), "a client that has not subscribed is not told");
        // What the connections and the frames read follows at once: a client that
        // connects now is told this revision, and the frames are made for this layout.
        assert_eq!(shared.rev.load(Ordering::Relaxed), 7);
        assert_eq!(*shared.layout(), snapshot.telemetry);
    }

    #[test]
    fn many_threads_can_broadcast_while_clients_come_and_go() {
        let hub = Arc::new(Hub::new(10_000));
        let (watcher, mut watching) = hub.connect();
        hub.subscribe(watcher);
        let threads: Vec<_> = (0..4)
            .map(|t| {
                let hub = hub.clone();
                std::thread::spawn(move || {
                    for n in 0..200 {
                        let (id, _rx) = hub.connect();
                        hub.subscribe(id);
                        hub.broadcast(&text(&format!("{t}:{n}")));
                        hub.disconnect(id);
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(drain(&mut watching).len(), 800, "the one that stayed heard every broadcast");
        assert_eq!(hub.client_count(), 1);
    }
}
