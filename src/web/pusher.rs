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
use crate::app::protocol::{TelemetryChannel, telemetry_frame};
use crate::telemetry::Telemetry;
use crate::web::Shared;
use crate::web::hub::{Hub, Outgoing};

/// Every channel there is, in the order of their numbers.
const CHANNELS: [TelemetryChannel; 2] = [TelemetryChannel::Meters, TelemetryChannel::Modulation];

/// The frame of `channel`: the value of every entry of its list in `layout`, in order,
/// as the atomics hold it now. A count (how many buffers clipped) goes as a float, which
/// is exact up to 2^24 of them; a client watches a count change rather than reading it.
pub fn frame(channel: TelemetryChannel, layout: &TelemetryLayout, telemetry: &Telemetry) -> Vec<u8> {
    let entries = match channel {
        TelemetryChannel::Meters => &layout.meters,
        TelemetryChannel::Modulation => &layout.modulation,
    };
    let values: Vec<f32> = entries
        .iter()
        .map(|entry| match entry.kind {
            SlotKind::Clips => telemetry.count_index(entry.slot) as f32,
            SlotKind::Peak | SlotKind::ModOutput | SlotKind::Effective => telemetry.load_index(entry.slot),
        })
        .collect();
    telemetry_frame(channel, layout.rev, &values)
}

/// One turn: queue a frame for the clients of each channel that somebody wants.
pub fn push_once(hub: &Hub, shared: &Shared, telemetry: &Telemetry) {
    for channel in CHANNELS {
        if hub.wanted(channel) {
            let frame = frame(channel, &shared.layout(), telemetry);
            hub.send_telemetry(channel, &Outgoing::Binary(frame.into()));
        }
    }
}

/// Push every `period` until told to stop.
pub async fn run(hub: Arc<Hub>, shared: Arc<Shared>, telemetry: Arc<Telemetry>, period: Duration, mut stop: watch::Receiver<bool>) {
    let mut ticker = interval(period);
    // A late tick is not made up for with a burst: the values are the latest anyway.
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = ticker.tick() => push_once(&hub, &shared, &telemetry),
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
    use crate::app::protocol::FRAME_HEADER_LEN;
    use crate::app::testutil::load;
    use crate::telemetry::Slot;

    /// A session with everything a layout has entries for: two lanes, one with a filter
    /// and an LFO that drives its cutoff.
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

        let meters = decode(&frame(TelemetryChannel::Meters, &layout, &telemetry));
        assert_eq!((meters.channel, meters.flags, meters.rev), (0, 0, layout.rev));
        assert_eq!(meters.values, expected_meters);
        let modulation = decode(&frame(TelemetryChannel::Modulation, &layout, &telemetry));
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
        assert_eq!(decode(&frame(TelemetryChannel::Meters, &layout, &telemetry)).values[at], 3.0);
    }

    #[test]
    fn a_session_without_entries_for_a_channel_sends_an_empty_frame() {
        let layout = TelemetryLayout { rev: 9, meters: vec![], modulation: vec![] };
        let decoded = decode(&frame(TelemetryChannel::Modulation, &layout, &Telemetry::new()));
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
        push_once(&hub, &shared, &Telemetry::new());
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
        push_once(&hub, &Shared::new(0, layout()), &Telemetry::new());
        assert!(binary(&mut queue).is_empty());
    }

    #[test]
    fn a_new_layout_is_what_the_next_frame_follows() {
        let (hub, [mut meters, ..]) = hub_with_three_clients();
        let shared = Shared::new(0, layout());
        let telemetry = Telemetry::new();
        push_once(&hub, &shared, &telemetry);
        let before = decode(&binary(&mut meters)[0]);
        shared.set_layout(TelemetryLayout { rev: 12345, meters: vec![], modulation: vec![] });
        push_once(&hub, &shared, &telemetry);
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
            push_once(&hub, &shared, &Telemetry::new());
        }
        assert_eq!(binary(&mut queue).len(), 2, "the queue held two, the rest were skipped");
        assert_eq!(hub.client_count(), 1);
        push_once(&hub, &shared, &Telemetry::new());
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
        let pusher = tokio::spawn(run(hub.clone(), shared, telemetry.clone(), Duration::from_millis(5), stop_rx));

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
