//! The telemetry pusher; see `docs/web-ui.md` §6.
//!
//! About thirty times a second, for each channel that some client has asked for, the
//! values of the channel's layout are read from the engine's atomics, put in a binary
//! frame and queued for the clients that want it. Nothing here touches the owner of
//! the controller: the layout is read from `Shared` (which the owner keeps current),
//! and the cells are the ones the audio thread stores into. A channel nobody wants
//! costs a lock and a lookup per tick.
//!
//! The pusher is a task of the server's runtime, so it runs at the runtime's own
//! cadence; a client that cannot take a frame (its queue is full) misses it, and the
//! next one is newer.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::time::{MissedTickBehavior, interval};

use crate::app::layout::{SlotKind, TelemetryLayout};
use crate::app::model::NodeId;
use crate::app::protocol::{SCOPE_FRAME_LENGTH, TelemetryChannel, probe_frame as frame_of_probe, telemetry_frame};
use crate::held_notes::HeldNotes;
use crate::telemetry::{PROBES, Telemetry};
use crate::web::Shared;
use crate::web::hub::{Hub, Outgoing};

/// Every channel there is, in the order of their numbers.
const CHANNELS: [TelemetryChannel; 4] = [TelemetryChannel::Meters, TelemetryChannel::Modulation, TelemetryChannel::Held, TelemetryChannel::Scope];

/// The frame of `channel`: the value of every entry of its list in `layout`, in order,
/// as the atomics hold it now. A count (how many buffers clipped) goes as a float, which
/// is exact up to 2^24 of them; a client watches a count change rather than reading it.
/// (The held notes have no layout and are not in the slots: they are `held_frame`.)
pub fn frame(channel: TelemetryChannel, layout: &TelemetryLayout, telemetry: &Telemetry, held: &HeldNotes) -> Vec<u8> {
    let entries = match channel {
        TelemetryChannel::Meters => &layout.meters,
        TelemetryChannel::Modulation => &layout.modulation,
        TelemetryChannel::Held => return held_frame(held),
        TelemetryChannel::Scope => return scope_frame(telemetry),
    };
    let values: Vec<f32> = entries
        .iter()
        .map(|entry| match entry.kind {
            SlotKind::Clips => telemetry.count_index(entry.slot) as f32,
            SlotKind::Peak | SlotKind::ModOutput | SlotKind::ModPosition | SlotKind::Effective | SlotKind::Reduction => telemetry.load_index(entry.slot),
        })
        .collect();
    telemetry_frame(channel, layout.rev, &values)
}

/// The frame of the held notes: a value for each of the 128 notes, 1 for one that is held, 0 for the rest, at the place
/// of its number. It is as long every time (a client reads it by note number, and a bitset would take the same
/// care to unpack for 128 values that cost 512 bytes thirty times a second), and follows no layout.
pub fn held_frame(held: &HeldNotes) -> Vec<u8> {
    let values: Vec<f32> = (0..128u8).map(|note| if held.is_held(note) { 1.0 } else { 0.0 }).collect();
    telemetry_frame(TelemetryChannel::Held, 0, &values)
}

/// The frame of the scope: the latest frames of the output, the left side and then the right, `SCOPE_FRAME_LENGTH` of each, the latest last. It is as long
/// every time (silence before the first frames) and follows no layout. (The client finds the place to start a picture of the wave at: a stable one is
/// a matter of where the wave goes up through the middle, which is the page's to look for in what it is given.)
pub fn scope_frame(telemetry: &Telemetry) -> Vec<u8> {
    let mut values = vec![0.0f32; 2 * SCOPE_FRAME_LENGTH];
    let (left, right) = values.split_at_mut(SCOPE_FRAME_LENGTH);
    telemetry.scope.latest(left, right);
    telemetry_frame(TelemetryChannel::Scope, 0, &values)
}

/// The frame of a node that is watched: the latest frames at the output of the node, the left side and then the right, `SCOPE_FRAME_LENGTH` of each, the latest last, as
/// the scope's are (`scope_frame`), read from ring `ring` of the probe pool and named for `node` (a ring that the pool has not is silence).
pub fn probe_frame(telemetry: &Telemetry, ring: usize, node: NodeId) -> Vec<u8> {
    let mut values = vec![0.0f32; 2 * SCOPE_FRAME_LENGTH];
    if let Some(ring) = telemetry.probes.get(ring) {
        let (left, right) = values.split_at_mut(SCOPE_FRAME_LENGTH);
        ring.latest(left, right);
    }
    frame_of_probe(node, &values)
}

/// One turn: queue a frame for the clients of each channel that somebody wants, and for each client the frame of each node that it watches and that has a ring (the controller
/// says which node a ring is for, `Telemetry::probe_node`).
pub fn push_once(hub: &Hub, shared: &Shared, telemetry: &Telemetry, held: &HeldNotes) {
    for channel in CHANNELS {
        if hub.wanted(channel) {
            let frame = frame(channel, &shared.layout(), telemetry, held);
            hub.send_telemetry(channel, &Outgoing::Binary(frame.into()));
        }
    }
    for ring in 0..PROBES {
        let Some(node) = telemetry.probe_node(ring).map(NodeId) else { continue };
        if hub.probe_wanted(node) {
            hub.send_probe(node, &Outgoing::Binary(probe_frame(telemetry, ring, node).into()));
        }
    }
}

/// Push every `period` until told to stop.
pub async fn run(hub: Arc<Hub>, shared: Arc<Shared>, telemetry: Arc<Telemetry>, held: Arc<HeldNotes>, period: Duration, mut stop: watch::Receiver<bool>) {
    let mut ticker = interval(period);
    // A late tick is not made up for with a burst: the values are the latest anyway.
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = ticker.tick() => push_once(&hub, &shared, &telemetry, &held),
            // (The guard `wait_for` returns is not `Send`, so it must not outlive this future.)
            _ = async { let _ = stop.wait_for(|stopped| *stopped).await; } => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc::Receiver;

    use super::*;
    use crate::app::layout::LayoutEntry;
    use crate::app::protocol::{FRAME_HEADER_LEN, SCOPE_FRAME_LENGTH};
    use crate::app::testutil::load;
    use crate::held_notes::HeldNotes;
    use crate::telemetry::Slot;

    /// A session with everything a layout has entries for: two lanes, one with a filter
    /// and an LFO that drives its cutoff, the other with a step sequencer.
    const SESSION: &str = r#"
[[instrument]]
plugin = "builtin:osc"

[[instrument.effect]]
plugin = "builtin:filter"

[[instrument.modulator]]
type = "lfo"
rate = 2.0

[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = 0.5

[[instrument]]
plugin = "builtin:osc"

[[instrument.modulator]]
type = "steps"
"#;

    fn layout() -> TelemetryLayout {
        load(SESSION).model.telemetry_layout()
    }

    /// The slot behind an entry, to store into.
    fn slot_of(entry: &LayoutEntry) -> Slot {
        Slot::for_test(entry.slot)
    }

    struct Decoded {
        channel: u8,
        flags: u8,
        rev: u32,
        values: Vec<f32>,
    }

    fn decode(frame: &[u8]) -> Decoded {
        let n = u16::from_le_bytes([frame[2], frame[3]]) as usize;
        assert_eq!(frame.len(), FRAME_HEADER_LEN + 4 * n, "the header's count is the number of values");
        Decoded {
            channel: frame[0],
            flags: frame[1],
            rev: u32::from_le_bytes(frame[4..8].try_into().unwrap()),
            values: frame[FRAME_HEADER_LEN..].chunks(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect(),
        }
    }

    fn binary(queue: &mut Receiver<Outgoing>) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        while let Ok(message) = queue.try_recv() {
            match message {
                Outgoing::Binary(bytes) => frames.push(bytes.to_vec()),
                other => panic!("{other:?}"),
            }
        }
        frames
    }

    #[test]
    fn a_frame_holds_what_the_atomics_hold_for_each_entry_of_the_channels_list_in_order() {
        let layout = layout();
        assert!(layout.meters.len() > 2 && layout.modulation.len() > 1, "the session has lanes, an LFO and a modulated parameter");
        let telemetry = Telemetry::new();
        let mut expected_meters = Vec::new();
        for (n, entry) in layout.meters.iter().enumerate() {
            let value = 0.01 * (n + 1) as f32;
            match entry.kind {
                SlotKind::Clips => {
                    for _ in 0..n {
                        telemetry.bump(slot_of(entry));
                    }
                    expected_meters.push(n as f32);
                }
                _ => {
                    telemetry.store(slot_of(entry), value);
                    expected_meters.push(value);
                }
            }
        }
        let mut expected_modulation = Vec::new();
        for (n, entry) in layout.modulation.iter().enumerate() {
            let value = -0.5 + 0.1 * n as f32;
            telemetry.store(slot_of(entry), value);
            expected_modulation.push(value);
        }

        let meters = decode(&frame(TelemetryChannel::Meters, &layout, &telemetry, &HeldNotes::new()));
        assert_eq!((meters.channel, meters.flags, meters.rev), (0, 0, layout.rev));
        assert_eq!(meters.values, expected_meters);
        let modulation = decode(&frame(TelemetryChannel::Modulation, &layout, &telemetry, &HeldNotes::new()));
        assert_eq!((modulation.channel, modulation.flags, modulation.rev), (1, 0, layout.rev));
        assert_eq!(modulation.values, expected_modulation);
    }

    #[test]
    fn a_clip_count_goes_as_a_count_not_as_the_bits_of_a_float() {
        let layout = layout();
        let telemetry = Telemetry::new();
        let clips = layout.meters.iter().find(|entry| entry.kind == SlotKind::Clips).expect("a meter that counts clips");
        for _ in 0..3 {
            telemetry.bump(slot_of(clips));
        }
        let at = layout.meters.iter().position(|entry| entry == clips).unwrap();
        assert_eq!(decode(&frame(TelemetryChannel::Meters, &layout, &telemetry, &HeldNotes::new())).values[at], 3.0);
    }

    #[test]
    fn the_held_notes_are_a_frame_of_a_value_for_each_note_one_for_a_note_that_is_held() {
        let held = HeldNotes::new();
        held.note_on(60);
        held.note_on(127);
        held.note_on(0);
        let decoded = decode(&held_frame(&held));
        assert_eq!((decoded.channel, decoded.flags, decoded.rev), (2, 0, 0), "no layout: the numbers of the notes are the places");
        assert_eq!(decoded.values.len(), 128);
        for (note, value) in decoded.values.iter().enumerate() {
            assert_eq!(*value, if [0, 60, 127].contains(&note) { 1.0 } else { 0.0 }, "note {note}");
        }
        held.note_off(60);
        assert_eq!(decode(&held_frame(&held)).values[60], 0.0, "a note that ended is not held");
    }

    #[test]
    fn the_held_notes_go_to_the_clients_that_ask_for_them_and_to_no_others() {
        let hub = Hub::new(16);
        let (keys, mut keys_queue) = hub.connect();
        let (meters, mut meters_queue) = hub.connect();
        let (_idle, mut idle_queue) = hub.connect();
        hub.set_channels(keys, &[TelemetryChannel::Held]);
        hub.set_channels(meters, &[TelemetryChannel::Meters]);
        let held = HeldNotes::new();
        held.note_on(64);
        push_once(&hub, &Shared::new(0, layout()), &Telemetry::new(), &held);
        let frames = binary(&mut keys_queue);
        assert_eq!(frames.len(), 1);
        assert_eq!((frames[0][0], decode(&frames[0]).values[64]), (2, 1.0));
        let others = binary(&mut meters_queue);
        assert_eq!((others.len(), others[0][0]), (1, 0), "the meters, and not the notes");
        assert!(binary(&mut idle_queue).is_empty());
    }

    /// `n` frames of a ramp from `from`: the left side counts up and the right side is its negative.
    fn ramp(from: usize, n: usize) -> Vec<Vec<f32>> {
        vec![(from..from + n).map(|i| i as f32).collect(), (from..from + n).map(|i| -(i as f32)).collect()]
    }

    #[test]
    fn a_probe_is_a_frame_of_the_latest_frames_at_a_node_the_left_side_and_then_the_right_with_the_node_in_the_header() {
        let telemetry = Telemetry::new();
        telemetry.probes[2].push(&ramp(0, 3000));
        let decoded = decode(&probe_frame(&telemetry, 2, NodeId(7)));
        assert_eq!((decoded.channel, decoded.flags, decoded.rev), (4, 0, 7), "the node where the layout's revision is");
        let n = SCOPE_FRAME_LENGTH;
        assert_eq!(decoded.values.len(), 2 * n, "a side and then the other");
        let left: Vec<f32> = (3000 - n..3000).map(|i| i as f32).collect();
        let right: Vec<f32> = left.iter().map(|v| -v).collect();
        assert_eq!(decoded.values[..n], left[..]);
        assert_eq!(decoded.values[n..], right[..]);
        assert!(decode(&probe_frame(&telemetry, 5, NodeId(9))).values.iter().all(|&v| v == 0.0), "a ring that nothing was stored into is silence");
        assert_eq!(probe_frame(&telemetry, 99, NodeId(1)).len(), FRAME_HEADER_LEN + 8 * n, "and a ring that the pool has not is silence too");
    }

    #[test]
    fn a_probe_goes_to_the_clients_that_watch_its_node_and_to_no_others() {
        let hub = Hub::new(16);
        let (a, mut a_queue) = hub.connect();
        let (b, mut b_queue) = hub.connect();
        let (_c, mut c_queue) = hub.connect();
        hub.set_probes(a, &[NodeId(7)]);
        hub.set_probes(b, &[NodeId(9)]);
        let telemetry = Telemetry::new();
        telemetry.probes[1].push(&ramp(0, 100));
        telemetry.set_probe_node(1, Some(7));
        push_once(&hub, &Shared::new(0, layout()), &telemetry, &HeldNotes::new());
        let frames = binary(&mut a_queue);
        assert_eq!(frames.len(), 1);
        let decoded = decode(&frames[0]);
        assert_eq!((decoded.channel, decoded.rev), (4, 7));
        assert_eq!(decoded.values[SCOPE_FRAME_LENGTH - 1], 99.0, "the newest frame of the ring is the last of the left side");
        assert!(binary(&mut b_queue).is_empty(), "node 9 has no ring");
        assert!(binary(&mut c_queue).is_empty());
    }

    #[test]
    fn a_ring_that_is_for_no_node_and_a_node_that_nobody_watches_are_sent_to_nobody() {
        let hub = Hub::new(16);
        let (a, mut a_queue) = hub.connect();
        hub.set_probes(a, &[NodeId(0), NodeId(7)]);
        let telemetry = Telemetry::new();
        // (A ring that has something and is for no node, though a client watches node 0; and a ring for node 8, that nobody watches.)
        telemetry.probes[3].push(&ramp(0, 100));
        telemetry.probes[4].push(&ramp(0, 100));
        telemetry.set_probe_node(4, Some(8));
        push_once(&hub, &Shared::new(0, layout()), &telemetry, &HeldNotes::new());
        assert!(binary(&mut a_queue).is_empty());
    }

    #[test]
    fn every_ring_that_is_for_a_watched_node_is_sent_every_turn() {
        let hub = Hub::new(64);
        let (a, mut a_queue) = hub.connect();
        let nodes: Vec<NodeId> = (10..10 + crate::telemetry::PROBES as u32).map(NodeId).collect();
        hub.set_probes(a, &nodes);
        let telemetry = Telemetry::new();
        for (ring, node) in nodes.iter().enumerate() {
            telemetry.set_probe_node(ring, Some(node.0));
        }
        push_once(&hub, &Shared::new(0, layout()), &telemetry, &HeldNotes::new());
        let seen: Vec<u32> = binary(&mut a_queue).iter().map(|frame| decode(frame).rev).collect();
        assert_eq!(seen, nodes.iter().map(|node| node.0).collect::<Vec<_>>(), "one frame for each, in the order of the rings");
    }

    #[test]
    fn the_scope_is_a_frame_of_the_latest_frames_of_the_output_the_left_side_and_then_the_right() {
        let telemetry = Telemetry::new();
        telemetry.scope.push(&ramp(0, 3000));
        let decoded = decode(&scope_frame(&telemetry));
        assert_eq!((decoded.channel, decoded.flags, decoded.rev), (3, 0, 0), "no layout");
        let n = SCOPE_FRAME_LENGTH;
        assert_eq!(decoded.values.len(), 2 * n, "a side and then the other");
        let left: Vec<f32> = (3000 - n..3000).map(|i| i as f32).collect();
        assert_eq!(decoded.values[..n], left[..]);
        assert!(decoded.values[n..].iter().zip(&left).all(|(right, left)| *right == -left), "the right side follows the left");
    }

    #[test]
    fn a_scope_that_has_nothing_yet_is_a_frame_of_silence_as_long_as_any() {
        let decoded = decode(&scope_frame(&Telemetry::new()));
        assert_eq!(decoded.values, vec![0.0; 2 * SCOPE_FRAME_LENGTH]);
    }

    #[test]
    fn the_scope_goes_to_the_clients_that_ask_for_it_and_to_no_others() {
        let hub = Hub::new(16);
        let (scope, mut scope_queue) = hub.connect();
        let (meters, mut meters_queue) = hub.connect();
        let (_idle, mut idle_queue) = hub.connect();
        hub.set_channels(scope, &[TelemetryChannel::Scope]);
        hub.set_channels(meters, &[TelemetryChannel::Meters]);
        let telemetry = Telemetry::new();
        telemetry.scope.push(&[vec![0.5; 100]]);
        push_once(&hub, &Shared::new(0, layout()), &telemetry, &HeldNotes::new());
        let frames = binary(&mut scope_queue);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0][0], 3);
        let decoded = decode(&frames[0]);
        assert_eq!((decoded.values[SCOPE_FRAME_LENGTH - 1], decoded.values[2 * SCOPE_FRAME_LENGTH - 1]), (0.5, 0.5), "the last frame of each side");
        let others = binary(&mut meters_queue);
        assert_eq!((others.len(), others[0][0]), (1, 0), "the meters, and not the scope");
        assert!(binary(&mut idle_queue).is_empty());
    }

    #[test]
    fn a_session_without_entries_for_a_channel_sends_an_empty_frame() {
        let layout = TelemetryLayout { rev: 9, meters: vec![], modulation: vec![] };
        let decoded = decode(&frame(TelemetryChannel::Modulation, &layout, &Telemetry::new(), &HeldNotes::new()));
        assert_eq!((decoded.channel, decoded.rev, decoded.values.len()), (1, 9, 0));
    }

    fn hub_with_three_clients() -> (Hub, [Receiver<Outgoing>; 3]) {
        let hub = Hub::new(16);
        let (meters, meters_queue) = hub.connect();
        let (modulation, modulation_queue) = hub.connect();
        let (_idle, idle_queue) = hub.connect();
        hub.set_channels(meters, &[TelemetryChannel::Meters]);
        hub.set_channels(modulation, &[TelemetryChannel::Modulation]);
        (hub, [meters_queue, modulation_queue, idle_queue])
    }

    #[test]
    fn each_channel_goes_to_the_clients_that_asked_for_it_and_to_no_others() {
        let (hub, [mut meters, mut modulation, mut idle]) = hub_with_three_clients();
        let shared = Shared::new(0, layout());
        push_once(&hub, &shared, &Telemetry::new(), &HeldNotes::new());
        let meter_frames = binary(&mut meters);
        let modulation_frames = binary(&mut modulation);
        assert_eq!((meter_frames.len(), modulation_frames.len()), (1, 1));
        assert_eq!((meter_frames[0][0], modulation_frames[0][0]), (0, 1));
        assert!(binary(&mut idle).is_empty());
    }

    #[test]
    fn nothing_is_sent_when_nobody_asks() {
        let hub = Hub::new(16);
        let (_client, mut queue) = hub.connect();
        push_once(&hub, &Shared::new(0, layout()), &Telemetry::new(), &HeldNotes::new());
        assert!(binary(&mut queue).is_empty());
    }

    #[test]
    fn a_new_layout_is_what_the_next_frame_follows() {
        let (hub, [mut meters, ..]) = hub_with_three_clients();
        let shared = Shared::new(0, layout());
        let telemetry = Telemetry::new();
        push_once(&hub, &shared, &telemetry, &HeldNotes::new());
        let before = decode(&binary(&mut meters)[0]);
        shared.set_layout(TelemetryLayout { rev: 12345, meters: vec![], modulation: vec![] });
        push_once(&hub, &shared, &telemetry, &HeldNotes::new());
        let after = decode(&binary(&mut meters)[0]);
        assert_ne!(before.rev, after.rev);
        assert_eq!((after.rev, after.values.len()), (12345, 0));
    }

    #[test]
    fn a_client_that_is_behind_misses_frames_and_is_not_dropped() {
        let hub = Hub::new(2);
        let (client, mut queue) = hub.connect();
        hub.set_channels(client, &[TelemetryChannel::Meters]);
        let shared = Shared::new(0, layout());
        for _ in 0..10 {
            push_once(&hub, &shared, &Telemetry::new(), &HeldNotes::new());
        }
        assert_eq!(binary(&mut queue).len(), 2, "the queue held two, the rest were skipped");
        assert_eq!(hub.client_count(), 1);
        push_once(&hub, &shared, &Telemetry::new(), &HeldNotes::new());
        assert_eq!(binary(&mut queue).len(), 1, "and it hears the next one");
    }

    #[tokio::test]
    async fn the_pusher_sends_frames_with_the_latest_values_until_it_is_stopped() {
        let hub = Arc::new(Hub::new(64));
        let (client, mut queue) = hub.connect();
        hub.set_channels(client, &[TelemetryChannel::Meters]);
        let layout = layout();
        let peak = layout.meters.iter().find(|entry| entry.kind == SlotKind::Peak).unwrap().slot;
        let at = layout.meters.iter().position(|entry| entry.slot == peak).unwrap();
        let shared = Arc::new(Shared::new(0, layout));
        let telemetry = Arc::new(Telemetry::new());
        let (stop_tx, stop_rx) = watch::channel(false);
        let pusher = tokio::spawn(run(hub.clone(), shared, telemetry.clone(), Arc::new(HeldNotes::new()), Duration::from_millis(5), stop_rx));

        async fn next_peak(queue: &mut Receiver<Outgoing>, at: usize) -> f32 {
            match tokio::time::timeout(Duration::from_secs(5), queue.recv()).await.expect("no frame in five seconds") {
                Some(Outgoing::Binary(bytes)) => decode(&bytes).values[at],
                other => panic!("{other:?}"),
            }
        }
        telemetry.store(Slot::for_test(peak), 0.25);
        assert_eq!(next_peak(&mut queue, at).await, 0.25);
        telemetry.store(Slot::for_test(peak), 0.75);
        // (Frames queued before the store may come first.)
        while next_peak(&mut queue, at).await != 0.75 {}

        stop_tx.send_replace(true);
        tokio::time::timeout(Duration::from_secs(5), pusher).await.expect("the pusher did not stop").unwrap();
        while queue.try_recv().is_ok() {}
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(queue.try_recv().is_err(), "no more frames after it stopped");
    }
}
