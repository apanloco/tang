//! The wire protocol: the types that cross the socket between the controller and
//! a browser, and the TypeScript that is generated from them; see
//! `docs/web-ui.md` §7.
//!
//! The types themselves live next to what they describe (`NodeId` in `model`,
//! `Intent` and `Event` in `intent`, ...) and derive `serde` always and `ts_rs::TS`
//! under the `web` feature. This module lists the roots of the protocol and
//! writes their TypeScript, into `web/src/protocol/generated/`, which is committed.
//! A test fails when what is committed is not what the types generate now:
//!
//! ```text
//! TANG_UPDATE_TYPES=1 cargo test --features web typescript
//! ```
//!
//! rewrites the directory.

#![cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "only the web runtime speaks the protocol"))]

use serde::{Deserialize, Serialize};

use crate::app::catalog::CatalogEntry;
use crate::app::intent::{Intent, IntentError, StampedEvent};
use crate::app::layout::TelemetryLayout;
use crate::app::snapshot::Snapshot;

/// The version of the protocol, in `Hello`: it goes up when a message changes in a
/// way an old client cannot read, and a client refuses a server that says another.
pub const PROTOCOL: u32 = 1;

/// What a client sends, as a text frame of JSON.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ClientMessage {
    /// The token a server that wants one (`Hello.needs_token`) is sent first: nothing
    /// else is processed before it.
    Auth { token: String },
    /// Start hearing about the session: a snapshot now, and the events after it.
    Subscribe,
    /// Which telemetry frames to send (each channel at about 30 Hz); none to stop.
    Telemetry { channels: Vec<TelemetryChannel> },
    /// Apply an intent; the answer (`Ack` or `Rejected`) carries `req_id`.
    Request { req_id: u32, intent: Intent },
    /// A note, straight to the audio thread (it does not pass the controller).
    NoteOn { note: u8, velocity: u8 },
    NoteOff { note: u8 },
}

/// What the server sends, as a text frame of JSON (telemetry goes as binary
/// frames, see `telemetry_frame`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ServerMessage {
    /// The first message of every connection.
    Hello {
        /// `PROTOCOL`: a client that was built for another refuses to go on.
        protocol: u32,
        /// The session's revision now.
        rev: u64,
        /// What the server can do: `notes`, `telemetry`.
        features: Vec<String>,
        /// The next message must be `Auth`.
        needs_token: bool,
    },
    /// The answer to `Subscribe`: where to start from.
    Snapshot { snapshot: Snapshot },
    /// Something an intent changed, from this client or another front-end.
    Event { event: StampedEvent },
    /// What the telemetry slots mean now: sent when it changes, so that frames and
    /// layout stay in step (a frame names the `rev` of the layout it follows).
    Layout { layout: TelemetryLayout },
    /// The plugins installed on this machine, which a client can add to the session by `id`: sent
    /// after the snapshot, and again whenever the scan finds more or ends (`scanning` goes false).
    /// It replaces what the client had; `rev` goes up with it.
    Catalog { rev: u64, scanning: bool, entries: Vec<CatalogEntry> },
    /// The intent of the request `req_id` was applied; the session is at `rev`.
    Ack { req_id: u32, rev: u64 },
    /// The intent of the request `req_id` was not applied.
    Rejected { req_id: u32, error: WireError },
    /// The connection is about to close: what was wrong with it.
    Closing { error: WireError },
}

/// A group of telemetry frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum TelemetryChannel {
    /// The peaks and clip counts (`TelemetryLayout::meters`).
    Meters,
    /// The modulators' outputs and the parameters' effective values
    /// (`TelemetryLayout::modulation`).
    Modulation,
    /// Which notes are held, by whichever source holds them (a MIDI keyboard, the virtual piano, a
    /// browser): 128 values, the note numbers being their places, 1 for a note that is held and 0
    /// for the rest. It follows no layout (its `layout_rev` is 0).
    Held,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
pub struct WireError {
    pub code: ErrorCode,
    /// A sentence for a person, not for a program.
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum ErrorCode {
    /// No node has the id the intent names.
    NotFound,
    /// The intent does not make sense for what it names.
    Invalid,
    /// A file could not be written.
    Io,
    /// The message was not one the server understands.
    BadRequest,
    /// The token is wrong or missing.
    Unauthorized,
    /// Too many requests too fast.
    RateLimited,
    /// The client falls behind: it is dropped and should reconnect.
    Overloaded,
}

impl WireError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        WireError { code, message: message.into() }
    }
}

impl From<&IntentError> for WireError {
    fn from(error: &IntentError) -> Self {
        let code = match error {
            IntentError::NotFound(_) | IntentError::EdgeNotFound(_) => ErrorCode::NotFound,
            IntentError::Invalid(_) => ErrorCode::Invalid,
            IntentError::Io(_) => ErrorCode::Io,
        };
        WireError::new(code, error.to_string())
    }
}

/// Bytes before the values of a telemetry frame.
pub const FRAME_HEADER_LEN: usize = 8;

/// A telemetry frame, the binary message that carries the current values of one
/// channel: `u8 channel | u8 flags | u16 n | u32 layout_rev | n × f32`, all little
/// endian. `channel` is 0 for the meters, 1 for the modulation and 2 for the held notes, `flags` is 0
/// (reserved), `layout_rev` is the `rev` of the `TelemetryLayout` the values follow:
/// value *i* is that of entry *i* of the channel's list in it (the held notes follow none: 0).
pub fn telemetry_frame(channel: TelemetryChannel, layout_rev: u32, values: &[f32]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(FRAME_HEADER_LEN + values.len() * 4);
    frame.push(match channel {
        TelemetryChannel::Meters => 0,
        TelemetryChannel::Modulation => 1,
        TelemetryChannel::Held => 2,
    });
    frame.push(0);
    frame.extend_from_slice(&(values.len().min(u16::MAX as usize) as u16).to_le_bytes());
    frame.extend_from_slice(&layout_rev.to_le_bytes());
    for value in values.iter().take(u16::MAX as usize) {
        frame.extend_from_slice(&value.to_le_bytes());
    }
    frame
}

#[cfg(test)]
mod tests {
    #[test]
    fn node_ids_are_plain_numbers_on_the_wire() {
        use crate::app::model::NodeId;
        assert_eq!(serde_json::to_string(&NodeId(7)).unwrap(), "7");
        assert_eq!(serde_json::from_str::<NodeId>("7").unwrap(), NodeId(7));
    }
}

#[cfg(test)]
mod message_tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::app::intent::{Event, Gain};
    use crate::app::model::NodeId;

    fn wire<T: Serialize>(value: &T) -> Value {
        serde_json::to_value(value).unwrap()
    }

    fn client_messages() -> Vec<(ClientMessage, Value)> {
        vec![
            (ClientMessage::Auth { token: "s3cret".to_string() }, json!({"type": "auth", "token": "s3cret"})),
            (ClientMessage::Subscribe, json!({"type": "subscribe"})),
            (
                ClientMessage::Telemetry { channels: vec![TelemetryChannel::Meters, TelemetryChannel::Modulation] },
                json!({"type": "telemetry", "channels": ["meters", "modulation"]}),
            ),
            (
                ClientMessage::Request { req_id: 7, intent: Intent::SetGain { node: NodeId(3), gain: Gain::Volume, value: 0.5 } },
                json!({"type": "request", "reqId": 7, "intent": {"type": "setGain", "node": 3, "gain": "volume", "value": 0.5}}),
            ),
            (ClientMessage::NoteOn { note: 60, velocity: 100 }, json!({"type": "noteOn", "note": 60, "velocity": 100})),
            (ClientMessage::NoteOff { note: 60 }, json!({"type": "noteOff", "note": 60})),
        ]
    }

    #[test]
    fn a_client_message_is_an_object_tagged_with_its_camel_case_name() {
        for (message, json) in client_messages() {
            assert_eq!(wire(&message), json, "{message:?}");
            assert_eq!(serde_json::from_value::<ClientMessage>(json).unwrap(), message);
        }
    }

    #[test]
    fn what_a_client_sends_that_is_not_a_message_is_an_error() {
        let bad = [
            ("no type", json!({"token": "x"})),
            ("an unknown type", json!({"type": "shout"})),
            ("a Rust name", json!({"type": "NoteOn", "note": 60, "velocity": 100})),
            ("a note that is not a number", json!({"type": "noteOn", "note": "C4", "velocity": 100})),
            ("a note past a byte", json!({"type": "noteOn", "note": 300, "velocity": 100})),
            ("a request with no id", json!({"type": "request", "intent": {"type": "setParam", "node": 1, "param": 0, "value": 0.5}})),
            ("a request whose intent is nonsense", json!({"type": "request", "reqId": 1, "intent": {"type": "explode"}})),
            ("a channel that does not exist", json!({"type": "telemetry", "channels": ["scope"]})),
        ];
        for (what, json) in bad {
            assert!(serde_json::from_value::<ClientMessage>(json).is_err(), "{what}");
        }
    }

    #[test]
    fn a_server_message_is_an_object_tagged_with_its_camel_case_name() {
        let event = StampedEvent { rev: 9, event: Event::ParamSet { node: NodeId(3), param: 2, value: 0.5 } };
        let cases = vec![
            (
                ServerMessage::Hello { protocol: PROTOCOL, rev: 4, features: vec!["notes".to_string()], needs_token: true },
                json!({"type": "hello", "protocol": PROTOCOL, "rev": 4, "features": ["notes"], "needsToken": true}),
            ),
            (
                ServerMessage::Event { event },
                json!({"type": "event", "event": {"rev": 9, "type": "paramSet", "node": 3, "param": 2, "value": 0.5}}),
            ),
            (ServerMessage::Ack { req_id: 7, rev: 10 }, json!({"type": "ack", "reqId": 7, "rev": 10})),
            (
                ServerMessage::Rejected { req_id: 7, error: WireError::new(ErrorCode::NotFound, "there is no node 9") },
                json!({"type": "rejected", "reqId": 7, "error": {"code": "notFound", "message": "there is no node 9"}}),
            ),
            (
                ServerMessage::Closing { error: WireError::new(ErrorCode::Overloaded, "too slow") },
                json!({"type": "closing", "error": {"code": "overloaded", "message": "too slow"}}),
            ),
            (
                ServerMessage::Catalog {
                    rev: 3,
                    scanning: true,
                    entries: vec![CatalogEntry { id: "builtin:osc".to_string(), name: "Oscillator".to_string(), format: "Built-in".to_string(), is_instrument: true, params: 8, presets: 0 }],
                },
                json!({
                    "type": "catalog",
                    "rev": 3,
                    "scanning": true,
                    "entries": [{"id": "builtin:osc", "name": "Oscillator", "format": "Built-in", "isInstrument": true, "params": 8, "presets": 0}],
                }),
            ),
        ];
        for (message, json) in cases {
            assert_eq!(wire(&message), json, "{message:?}");
            assert_eq!(serde_json::from_value::<ServerMessage>(json).unwrap(), message);
        }
    }

    #[test]
    fn snapshots_and_layouts_are_nested_under_their_own_names() {
        use crate::app::testutil::{GROUPS, load};
        let model = load(GROUPS).model;
        let layout = model.telemetry_layout();
        let message = ServerMessage::Layout { layout: layout.clone() };
        let json = wire(&message);
        assert_eq!(json["type"], "layout");
        assert_eq!(json["layout"]["rev"], json!(layout.rev));
        assert_eq!(serde_json::from_value::<ServerMessage>(json).unwrap(), message);
    }

    #[test]
    fn every_error_code_has_its_own_camel_case_name() {
        let names: Vec<Value> = [
            ErrorCode::NotFound,
            ErrorCode::Invalid,
            ErrorCode::Io,
            ErrorCode::BadRequest,
            ErrorCode::Unauthorized,
            ErrorCode::RateLimited,
            ErrorCode::Overloaded,
        ]
        .iter()
        .map(wire)
        .collect();
        assert_eq!(names, ["notFound", "invalid", "io", "badRequest", "unauthorized", "rateLimited", "overloaded"].map(Value::from));
    }

    #[test]
    fn an_intent_error_says_what_it_was_in_a_code_and_a_sentence() {
        let missing = WireError::from(&IntentError::NotFound(NodeId(9)));
        assert_eq!((missing.code, missing.message.as_str()), (ErrorCode::NotFound, "there is no node 9"));
        let invalid = WireError::from(&IntentError::Invalid("no good".to_string()));
        assert_eq!((invalid.code, invalid.message.as_str()), (ErrorCode::Invalid, "no good"));
        let io = WireError::from(&IntentError::Io("Permission denied".to_string()));
        assert_eq!((io.code, io.message.as_str()), (ErrorCode::Io, "Permission denied"));
    }

    #[test]
    fn a_telemetry_frame_is_a_small_header_and_little_endian_floats() {
        let frame = telemetry_frame(TelemetryChannel::Modulation, 0x0102_0304, &[1.0, -2.5, 0.25]);
        assert_eq!(frame.len(), FRAME_HEADER_LEN + 3 * 4);
        assert_eq!(frame[0], 1, "the modulation channel");
        assert_eq!(frame[1], 0, "no flags");
        assert_eq!(&frame[2..4], &[3, 0], "three values");
        assert_eq!(&frame[4..8], &[4, 3, 2, 1], "the layout's revision");
        let values: Vec<f32> = frame[8..].chunks(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
        assert_eq!(values, [1.0, -2.5, 0.25]);
        assert_eq!(telemetry_frame(TelemetryChannel::Meters, 0, &[])[0], 0, "the meters channel");
        assert_eq!(telemetry_frame(TelemetryChannel::Held, 0, &[0.0, 1.0])[0], 2, "the held notes' channel");
        assert_eq!(telemetry_frame(TelemetryChannel::Meters, 0, &[]).len(), FRAME_HEADER_LEN);
    }

    #[test]
    fn a_telemetry_frame_never_claims_more_values_than_it_carries() {
        let values = vec![0.5f32; 70_000];
        let frame = telemetry_frame(TelemetryChannel::Meters, 1, &values);
        let n = u16::from_le_bytes([frame[2], frame[3]]) as usize;
        assert_eq!(n, u16::MAX as usize);
        assert_eq!(frame.len(), FRAME_HEADER_LEN + n * 4);
    }
}

#[cfg(all(test, feature = "web"))]
mod typescript {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};

    use ts_rs::{Config, TS};

    use serde::Serialize;

    use crate::app::graph::{Graph, Waveform};
    use crate::app::intent::{Event, Gain, Intent, ModulatorField, StampedEvent};
    use crate::app::layout::TelemetryLayout;
    use crate::app::model::{DEPTH_MAX, DEPTH_MIN};
    use crate::app::snapshot::Snapshot;
    use super::{ClientMessage, ServerMessage};

    /// Every gain and every modulator setting, to write the range of each. (One left out
    /// is a key missing in the `Record` that is generated, which the web's typecheck refuses.)
    const GAINS: [Gain; 4] = [Gain::Volume, Gain::Mix, Gain::GroupVolume, Gain::GroupPan];
    const MODULATOR_FIELDS: [ModulatorField; 6] = [
        ModulatorField::Rate,
        ModulatorField::Waveform,
        ModulatorField::Attack,
        ModulatorField::Decay,
        ModulatorField::Sustain,
        ModulatorField::Release,
    ];

    /// The entries of a TypeScript object that says `[min, max]` for each of `all`, by the
    /// name it has on the wire (the variants are serialized as strings).
    fn range_entries<T: Serialize + Copy>(all: &[T], range: fn(T) -> (f32, f32)) -> String {
        all.iter()
            .map(|&item| {
                let name = serde_json::to_value(item).unwrap();
                let (min, max) = range(item);
                format!("  {}: [{min}, {max}],\n", name.as_str().expect("a unit variant is a string"))
            })
            .collect()
    }

    /// The entries of a TypeScript object that says what each setting of a modulator is when the
    /// modulator is new: what a new LFO and a new envelope start as.
    fn default_entries() -> String {
        use crate::app::model::ModSourceSlot;
        let (lfo, envelope) = (ModSourceSlot::new_lfo(), ModSourceSlot::new_envelope());
        MODULATOR_FIELDS
            .iter()
            .map(|&field| {
                let name = serde_json::to_value(field).unwrap();
                let value = lfo.setting(field).or_else(|| envelope.setting(field)).expect("a new LFO or a new envelope has every setting");
                format!("  {}: {value},\n", name.as_str().expect("a unit variant is a string"))
            })
            .collect()
    }

    /// The names an LFO's waveforms have on the wire, in the order of their index (the value
    /// of `ModulatorField::Waveform`: `LfoWaveform::ALL`), as the elements of a TypeScript array.
    fn waveform_names() -> String {
        let names: Vec<String> = crate::plugin::chain::LfoWaveform::ALL
            .iter()
            .map(|&waveform| serde_json::to_string(&Waveform::from(waveform)).unwrap())
            .collect();
        names.join(", ")
    }

    /// The names of the roots a scale can have, by pitch class (0 C to 11 B), as the elements of a TypeScript
    /// array: what `setScale` and a session file call them (`scale::NOTE_NAMES`).
    fn scale_root_names() -> String {
        let names: Vec<String> = crate::scale::NOTE_NAMES.iter().map(|name| serde_json::to_string(name).unwrap()).collect();
        names.join(", ")
    }

    /// The kinds of scale, in the order the TUI lists them, as the lines of a TypeScript array of objects: the name a
    /// person reads, the short name that `setScale` and a session file say, and the semitones from the root of each of
    /// its notes (`scale::SCALES`).
    fn scale_kinds() -> String {
        crate::scale::SCALES
            .iter()
            .map(|scale| {
                let name = serde_json::to_string(scale.name).unwrap();
                let short = serde_json::to_string(scale.short).unwrap();
                let intervals = serde_json::to_string(scale.intervals).unwrap().replace(',', ", ");
                format!("  {{ name: {name}, short: {short}, intervals: {intervals} }},\n")
            })
            .collect()
    }

    /// Where the generated TypeScript is committed.
    fn generated_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("web/src/protocol/generated")
    }

    /// Write the TypeScript of every root type of the protocol, and of what each
    /// uses, into `dir`: one file per type.
    fn export_typescript(dir: &Path) -> anyhow::Result<()> {
        // Not `Config::from_env()`: what is generated must not depend on the
        // environment of whoever runs the test. `u64` counts (revisions) stay far
        // below 2^53, so they are plain numbers rather than `bigint`.
        let cfg = Config::new().with_large_int("number").with_out_dir(dir);
        Intent::export_all(&cfg)?;
        Event::export_all(&cfg)?;
        StampedEvent::export_all(&cfg)?;
        Graph::export_all(&cfg)?;
        TelemetryLayout::export_all(&cfg)?;
        Snapshot::export_all(&cfg)?;
        ClientMessage::export_all(&cfg)?;
        ServerMessage::export_all(&cfg)?;
        // The constants a client needs that are not types.
        fs::create_dir_all(dir)?;
        fs::write(
            dir.join("constants.ts"),
            format!(
                "// This file was generated by `app::protocol` (`TANG_UPDATE_TYPES=1 cargo test --features web typescript`). Do not edit this file manually.\n\
                 import type {{ Gain }} from \"./Gain\";\n\
                 import type {{ ModulatorField }} from \"./ModulatorField\";\n\
                 import type {{ Waveform }} from \"./Waveform\";\n\n\
                 /** The version of the protocol: `Hello.protocol` says what the server speaks. */\n\
                 export const PROTOCOL = {};\n\n\
                 /** Bytes before the values of a binary telemetry frame. */\n\
                 export const FRAME_HEADER_LEN = {};\n\n\
                 /** The lowest and the highest value of each host-side gain: what the controller clamps to (`Gain::range`). */\n\
                 export const GAIN_RANGES: Record<Gain, readonly [number, number]> = {{\n{}}};\n\n\
                 /** The same for each setting of a modulator (`ModulatorField::range`). */\n\
                 export const MODULATOR_FIELD_RANGES: Record<ModulatorField, readonly [number, number]> = {{\n{}}};\n\n\
                 /** What each setting of a modulator is when the modulator is new (`ModSourceSlot::new_lfo`, `new_envelope`): where a double click puts a knob. */\n\
                 export const MODULATOR_FIELD_DEFAULTS: Record<ModulatorField, number> = {{\n{}}};\n\n\
                 /** The lowest and the highest depth of a modulation cable (`DEPTH_MIN`, `DEPTH_MAX`). */\n\
                 export const DEPTH_RANGE: readonly [number, number] = [{DEPTH_MIN}, {DEPTH_MAX}];\n\n\
                 /** The waveforms of an LFO by the index that is the value of its `waveform` setting (`LfoWaveform::ALL`). */\n\
                 export const WAVEFORMS: readonly Waveform[] = [{}];\n\n\
                 /** The roots a scale can have by pitch class (0 C to 11 B), as `setScale` and a session file name them (`scale::NOTE_NAMES`). */\n\
                 export const SCALE_ROOTS: readonly string[] = [{}];\n\n\
                 /** The kinds of scale, in the order the TUI lists them: the name a person reads, the short name that `setScale` and a session file say, and the semitones above the root of each of its notes (`scale::SCALES`). */\n\
                 export const SCALE_KINDS: readonly {{ name: string; short: string; intervals: readonly number[] }}[] = [\n{}];\n\n\
                 /** How far from the origin a node may be drawn, either way: what the controller clamps a place to (`NodeUi::LIMIT`). */\n\
                 export const NODE_UI_LIMIT = {};\n\n\
                 /** A place is kept to this many parts of a pixel: a tenth (`NodeUi::PARTS`). */\n\
                 export const NODE_UI_PARTS = {};\n",
                super::PROTOCOL,
                super::FRAME_HEADER_LEN,
                range_entries(&GAINS, Gain::range),
                range_entries(&MODULATOR_FIELDS, ModulatorField::range),
                default_entries(),
                waveform_names(),
                scale_root_names(),
                scale_kinds(),
                crate::session::NodeUi::LIMIT,
                crate::session::NodeUi::PARTS,
            ),
        )?;
        Ok(())
    }

    /// The `.ts` files in a directory, by name.
    fn typescript_files(dir: &Path) -> BTreeMap<String, String> {
        let Ok(entries) = fs::read_dir(dir) else { return BTreeMap::new() };
        entries
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "ts"))
            .map(|path| (path.file_name().unwrap().to_string_lossy().into_owned(), fs::read_to_string(&path).unwrap()))
            .collect()
    }

    /// How the `.ts` files in `committed` differ from those in `fresh`, one line
    /// each, by file name. Other files in `committed` are not looked at.
    fn differences(committed: &Path, fresh: &Path) -> Vec<String> {
        let (committed, fresh) = (typescript_files(committed), typescript_files(fresh));
        let mut names: Vec<&String> = committed.keys().chain(fresh.keys()).collect();
        names.sort();
        names.dedup();
        names
            .into_iter()
            .filter_map(|name| match (committed.get(name), fresh.get(name)) {
                (Some(a), Some(b)) if a == b => None,
                (Some(_), Some(_)) => Some(format!("{name} is out of date")),
                (Some(_), None) => Some(format!("{name} is not generated any more")),
                (None, _) => Some(format!("{name} is not committed")),
            })
            .collect()
    }

    fn write(dir: &Path, name: &str, text: &str) {
        fs::write(dir.join(name), text).unwrap();
    }

    #[test]
    fn the_range_of_a_gain_is_written_by_the_gains_wire_name() {
        assert_eq!(
            range_entries(&GAINS, Gain::range),
            "  volume: [0, 4],\n  mix: [0, 1],\n  groupVolume: [0, 4],\n  groupPan: [-1, 1],\n"
        );
    }

    #[test]
    fn the_range_of_a_modulator_setting_is_written_the_way_a_person_writes_the_number() {
        // (Not `0.009999999776482582`: an f32 is written as the shortest number that is it.)
        assert_eq!(
            range_entries(&MODULATOR_FIELDS, ModulatorField::range),
            "  rate: [0.01, 50],\n  waveform: [0, 3],\n  attack: [0.001, 10],\n  decay: [0.001, 10],\n  sustain: [0, 1],\n  release: [0.001, 10],\n"
        );
    }

    #[test]
    fn what_a_new_modulator_starts_as_is_written_for_every_setting() {
        assert_eq!(
            default_entries(),
            "  rate: 1,\n  waveform: 0,\n  attack: 0.01,\n  decay: 0.3,\n  sustain: 0.7,\n  release: 0.5,\n"
        );
    }

    #[test]
    fn the_waveforms_are_written_in_the_order_of_their_index() {
        assert_eq!(waveform_names(), "\"sine\", \"triangle\", \"saw\", \"square\"");
        // (The index is what a modulator's `waveform` setting is.)
        for (index, name) in crate::plugin::chain::LfoWaveform::ALL.iter().enumerate() {
            assert_eq!(name.to_index(), index);
        }
    }

    #[test]
    fn the_roots_of_a_scale_are_written_in_the_order_of_their_pitch_class() {
        assert_eq!(scale_root_names(), "\"C\", \"C#\", \"D\", \"D#\", \"E\", \"F\", \"F#\", \"G\", \"G#\", \"A\", \"A#\", \"B\"");
    }

    #[test]
    fn the_kinds_of_scale_are_written_with_the_name_a_person_reads_and_the_short_name_a_session_file_says() {
        let written = scale_kinds();
        assert_eq!(written.lines().count(), crate::scale::SCALES.len());
        assert!(
            written.starts_with(
                "  { name: \"Major\", short: \"major\", intervals: [0, 2, 4, 5, 7, 9, 11] },\n  { name: \"Natural Minor\", short: \"minor\", intervals: [0, 2, 3, 5, 7, 8, 10] },\n"
            ),
            "{written}"
        );
        assert!(written.contains("{ name: \"Minor Pentatonic\", short: \"min pent\", intervals: [0, 3, 5, 7, 10] },\n"), "{written}");
        assert!(written.ends_with("{ name: \"Chromatic\", short: \"chromatic\", intervals: [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11] },\n"), "{written}");
    }

    #[test]
    fn the_limits_of_a_place_are_in_the_constants_as_the_controller_keeps_a_place() {
        let dir = tempfile::tempdir().unwrap();
        export_typescript(dir.path()).unwrap();
        let constants = fs::read_to_string(dir.path().join("constants.ts")).unwrap();
        assert!(constants.contains("export const NODE_UI_LIMIT = 100000;\n"), "{constants}");
        assert!(constants.contains("export const NODE_UI_PARTS = 10;\n"), "{constants}");
    }

    #[test]
    fn two_equal_directories_have_no_differences() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        for dir in [&a, &b] {
            write(dir.path(), "One.ts", "export type One = number;\n");
            write(dir.path(), "Two.ts", "export type Two = string;\n");
        }
        assert_eq!(differences(a.path(), b.path()), Vec::<String>::new());
    }

    #[test]
    fn a_changed_missing_and_extra_file_are_each_reported() {
        let (committed, fresh) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        write(committed.path(), "Same.ts", "a");
        write(fresh.path(), "Same.ts", "a");
        write(committed.path(), "Changed.ts", "old");
        write(fresh.path(), "Changed.ts", "new");
        write(fresh.path(), "Missing.ts", "m");
        write(committed.path(), "Extra.ts", "e");
        assert_eq!(
            differences(committed.path(), fresh.path()),
            [
                "Changed.ts is out of date",
                "Extra.ts is not generated any more",
                "Missing.ts is not committed",
            ]
        );
    }

    #[test]
    fn a_file_that_is_not_typescript_is_left_alone() {
        let (committed, fresh) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        write(committed.path(), "README.md", "notes");
        assert_eq!(differences(committed.path(), fresh.path()), Vec::<String>::new());
    }

    #[test]
    fn the_generated_typescript_is_up_to_date() {
        let committed = generated_dir();
        if std::env::var_os("TANG_UPDATE_TYPES").is_some() {
            fs::create_dir_all(&committed).unwrap();
            for entry in fs::read_dir(&committed).unwrap() {
                let path = entry.unwrap().path();
                if path.extension().is_some_and(|ext| ext == "ts") {
                    fs::remove_file(path).unwrap();
                }
            }
            export_typescript(&committed).unwrap();
            return;
        }
        let fresh = tempfile::tempdir().unwrap();
        export_typescript(fresh.path()).unwrap();
        let differences = differences(&committed, fresh.path());
        assert!(
            differences.is_empty(),
            "web/src/protocol/generated is stale:\n  {}\nrun `TANG_UPDATE_TYPES=1 cargo test --features web typescript` and commit the result",
            differences.join("\n  ")
        );
    }
}
