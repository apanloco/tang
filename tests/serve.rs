//! `tang serve` as a person runs it: the real binary, on the null audio backend (no
//! sound card is opened, so nothing is heard), with a WebSocket client in front of it.
//!
//! What the unit tests of `web::` show with a stub engine, this shows with the whole
//! program: a browser that plays a note and moves a parameter hears it in the master's
//! meter, because the note went through the WebSocket to the note path, the parameter
//! through the owner to the audio thread, and the peak back through the atomics and the
//! telemetry frames. See `docs/web-ui.md` §12, step 10.

#![cfg(feature = "web")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::SocketAddr;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// A saw oscillator through a filter an LFO sweeps; quiet on purpose. The tests' own copy of the tracer scene, and not
/// `examples/tracer.toml`, which a person who runs the program on it can save over.
const TRACER: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/web/src/client/fixtures/tracer.toml");

/// `NodeId::MASTER`, as a client sees it.
const MASTER: u64 = u32::MAX as u64;

type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// A child process that is killed when this goes, however the test ends: one that panics
/// while the program is starting (it never says where it serves, say) must not leave it running.
struct Process(Child);

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A running `tang serve`.
struct Tang {
    child: Process,
    addr: SocketAddr,
    /// What it has logged (its stderr), for the message of a test that fails.
    log: Arc<Mutex<String>>,
}

impl Tang {
    fn start() -> Tang {
        Tang::start_on(TRACER)
    }

    /// `tang serve` on the session in `session`.
    fn start_on(session: impl AsRef<std::ffi::OsStr>) -> Tang {
        Tang::start_in(None, session)
    }

    /// `tang serve` on the session in `session`, started in the directory `dir` (where the test is, if none).
    fn start_in(dir: Option<&std::path::Path>, session: impl AsRef<std::ffi::OsStr>) -> Tang {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tang"));
        // No sound card and no MIDI input is opened: nothing is heard, nothing is listened to.
        command
            .args(["--audio-device", "none", "--midi-device", "no such device", "serve"])
            .arg(session)
            .args(["--web-bind", "127.0.0.1:0"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(dir) = dir {
            command.current_dir(dir);
        }
        let mut child = Process(command.spawn().expect("tang starts"));

        let log = Arc::new(Mutex::new(String::new()));
        let stderr = child.0.stderr.take().unwrap();
        let sink = log.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let mut log = sink.lock().unwrap();
                log.push_str(&line);
                log.push('\n');
            }
        });

        // Where it serves is the first line on stdout.
        let stdout = child.0.stdout.take().unwrap();
        let (lines, first) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = lines.send(line);
            }
        });
        let line = first
            .recv_timeout(Duration::from_secs(60))
            .unwrap_or_else(|_| panic!("tang did not say where it serves within a minute:\n{}", log.lock().unwrap()));
        let url = line.strip_prefix("Web interface: http://").unwrap_or_else(|| panic!("an unexpected first line: {line:?}"));
        let addr = url.trim_end_matches('/').parse().unwrap_or_else(|e| panic!("{url:?}: {e}"));
        Tang { child, addr, log }
    }

    fn log(&self) -> String {
        self.log.lock().unwrap().clone()
    }

    /// Ask it to stop the way Ctrl+C does.
    #[cfg(unix)]
    fn interrupt(&self) {
        let status = Command::new("kill").args(["-INT", &self.child.0.id().to_string()]).status().expect("kill runs");
        assert!(status.success());
    }

    fn exit_within(&mut self, within: Duration) -> Option<std::process::ExitStatus> {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if let Some(status) = self.child.0.try_wait().unwrap() {
                return Some(status);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    }
}

/// A WebSocket the way a browser opens one from the page: with its origin.
async fn connect_as(tang: &Tang, origin: &str) -> Result<Client, tungstenite::Error> {
    let mut request = format!("ws://{}/ws", tang.addr).into_client_request().unwrap();
    request.headers_mut().insert("origin", origin.parse().unwrap());
    tokio_tungstenite::connect_async(request).await.map(|(client, _response)| client)
}

async fn connect(tang: &Tang) -> Client {
    connect_as(tang, &format!("http://{}", tang.addr)).await.expect("the server takes the page's own origin")
}

async fn tell(client: &mut Client, message: Value) {
    client.send(Message::text(message.to_string())).await.unwrap();
}

/// The next JSON text (the telemetry frames that come between are skipped).
async fn next_json(client: &mut Client) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(5), client.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => return serde_json::from_str(text.as_str()).unwrap(),
            Ok(Some(Ok(_))) => {}
            other => panic!("the connection gave {other:?} while a message was awaited"),
        }
    }
    panic!("no message in twenty seconds");
}

/// The answer to request `req_id`: the next `ack` or `rejected` for it (`event`s come first).
async fn answer(client: &mut Client, req_id: u64) -> Value {
    loop {
        let message = next_json(client).await;
        if message["reqId"] == req_id {
            return message;
        }
    }
}

/// Where in a meters frame the master's peak is, from the layout of a snapshot.
fn master_peak_at(snapshot: &Value) -> usize {
    let meters = snapshot["snapshot"]["telemetry"]["meters"].as_array().expect("a meters list");
    meters.iter().position(|e| e["node"] == MASTER && e["kind"] == "peak").expect("the master has a peak")
}

/// A snapshot's node by name, and a parameter of its plugin by name: (node id, parameter index).
fn param_of(snapshot: &Value, node: &str, param: &str) -> (u64, u64) {
    let nodes = snapshot["snapshot"]["graph"]["nodes"].as_array().unwrap();
    let node = nodes.iter().find(|n| n["name"] == node).unwrap_or_else(|| panic!("no node {node}"));
    let param = node["plugin"]["params"].as_array().unwrap().iter().find(|p| p["name"] == param).unwrap_or_else(|| panic!("no {param}"));
    (node["id"].as_u64().unwrap(), param["index"].as_u64().unwrap())
}

/// Read frames until the master's peak satisfies `done`, and return it. Fails, with what
/// the server logged, when it has not within `within`.
async fn peak_until(client: &mut Client, tang: &Tang, at: usize, within: Duration, what: &str, done: impl Fn(f32) -> bool) -> f32 {
    let deadline = Instant::now() + within;
    let mut last = f32::NAN;
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), client.next()).await {
            Ok(Some(Ok(Message::Binary(frame)))) => {
                assert_eq!(frame[0], 0, "a meters frame");
                last = f32::from_le_bytes(frame[8 + 4 * at..12 + 4 * at].try_into().unwrap());
                if done(last) {
                    return last;
                }
            }
            Ok(Some(Ok(_))) | Err(_) => {}
            Ok(Some(Err(_))) | Ok(None) => panic!("the connection ended while waiting for {what}\n{}", tang.log()),
        }
    }
    panic!("{what}: the master's peak was {last} after {within:?}\n{}", tang.log());
}

/// Connected, subscribed, and asking for the meters: the client, and where the master's peak is.
async fn listening(tang: &Tang) -> (Client, usize, Value) {
    let mut client = connect(tang).await;
    let hello = next_json(&mut client).await;
    assert_eq!((hello["type"].clone(), hello["protocol"].clone(), hello["needsToken"].clone()), (json!("hello"), json!(1), json!(false)));
    tell(&mut client, json!({"type": "subscribe"})).await;
    let snapshot = next_json(&mut client).await;
    assert_eq!(snapshot["type"], "snapshot", "{snapshot}");
    let at = master_peak_at(&snapshot);
    tell(&mut client, json!({"type": "telemetry", "channels": ["meters"]})).await;
    (client, at, snapshot)
}

const SOON: Duration = Duration::from_secs(10);

#[tokio::test]
async fn a_browser_plays_a_note_and_moves_a_parameter_and_hears_it_in_the_meters() {
    let tang = Tang::start();
    let (mut client, at, snapshot) = listening(&tang).await;
    let (osc, volume) = param_of(&snapshot, "Oscillator", "volume");

    // Nothing is playing: the meter is at rest.
    peak_until(&mut client, &tang, at, SOON, "silence before any note", |peak| peak == 0.0).await;

    // A key pressed in the browser sounds; released, it dies away.
    tell(&mut client, json!({"type": "noteOn", "note": 60, "velocity": 100})).await;
    peak_until(&mut client, &tang, at, SOON, "the note", |peak| peak > 0.01).await;
    tell(&mut client, json!({"type": "noteOff", "note": 60})).await;
    peak_until(&mut client, &tang, at, Duration::from_secs(15), "the note dying away", |peak| peak < 0.001).await;

    // The oscillator's volume turned to nothing: the same note is silent, and the browser is told it was done.
    tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "setParam", "node": osc, "param": volume, "value": 0.0}})).await;
    assert_eq!(answer(&mut client, 1).await["type"], "ack");
    // (The oscillator eases its volume over a few milliseconds, and a note that starts in the
    // block where it changes is heard with the old one fading out, so give it a moment.)
    tokio::time::sleep(Duration::from_millis(100)).await;
    tell(&mut client, json!({"type": "noteOn", "note": 60, "velocity": 100})).await;
    let quiet_until = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < quiet_until {
        match tokio::time::timeout(Duration::from_millis(200), client.next()).await {
            Ok(Some(Ok(Message::Binary(frame)))) => {
                let peak = f32::from_le_bytes(frame[8 + 4 * at..12 + 4 * at].try_into().unwrap());
                assert!(peak < 0.001, "the volume is 0 and the meter says {peak}");
            }
            Ok(Some(Ok(_))) | Err(_) => {}
            other => panic!("{other:?}"),
        }
    }

    // And back up: it was the parameter that silenced it, not a dead engine.
    tell(&mut client, json!({"type": "request", "reqId": 2, "intent": {"type": "setParam", "node": osc, "param": volume, "value": 1.0}})).await;
    assert_eq!(answer(&mut client, 2).await["type"], "ack");
    peak_until(&mut client, &tang, at, SOON, "the note with the volume back", |peak| peak > 0.01).await;
}

#[tokio::test]
async fn a_browser_saves_the_session_to_its_own_file_and_the_file_has_what_it_changed() {
    // A copy of the tracer, not the example: the program writes where the session came from.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("jam.toml");
    std::fs::copy(TRACER, &file).unwrap();
    let tang = Tang::start_on(&file);
    let (mut client, _, snapshot) = listening(&tang).await;
    assert_eq!(snapshot["snapshot"]["meta"]["file"], "jam.toml", "the session is known by its file's name, not its path");
    let (filter, resonance) = param_of(&snapshot, "Filter", "resonance");

    // A change is in the session and not in the file.
    tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "setParam", "node": filter, "param": resonance, "value": 7.5}})).await;
    assert_eq!(answer(&mut client, 1).await["type"], "ack");
    let before = std::fs::read_to_string(&file).unwrap();
    assert!(!before.contains("resonance = 7.5"), "{before}");

    // Saving is a request like the others, and answered by the revision it made, and the event that says so.
    tell(&mut client, json!({"type": "request", "reqId": 2, "intent": {"type": "save"}})).await;
    let mut said = Vec::new();
    let saved = loop {
        let message = next_json(&mut client).await;
        if message["reqId"] == 2 {
            break message;
        }
        said.push(message);
    };
    assert_eq!((saved["type"].clone(), saved["rev"].clone()), (json!("ack"), json!(2)), "{saved}");
    assert!(
        said.iter().any(|m| m["type"] == "event" && m["event"]["type"] == "saved" && m["event"]["file"] == "jam.toml" && m["event"]["rev"] == 2),
        "the client is told of the save: {said:?}"
    );
    let after = std::fs::read_to_string(&file).unwrap();
    assert!(after.contains("resonance = 7.5"), "the file has the change:\n{after}");

    // A client that arrives now is given a session with nothing unsaved.
    let (_, _, again) = listening(&tang).await;
    assert_eq!(again["snapshot"]["meta"]["dirty"], false);
}

#[tokio::test]
async fn a_browser_is_told_what_plugins_can_be_added_and_never_where_they_are_installed() {
    let tang = Tang::start();
    let mut client = connect(&tang).await;
    assert_eq!(next_json(&mut client).await["type"], "hello");
    tell(&mut client, json!({"type": "subscribe"})).await;
    assert_eq!(next_json(&mut client).await["type"], "snapshot");

    // The catalog comes after the snapshot, as far as the scan has got, and again while it finds more: the
    // built-ins are the first batch (what else is installed depends on the machine).
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut told = Vec::new();
    let osc = loop {
        let message = next_json(&mut client).await;
        assert_eq!(message["type"], "catalog", "nothing else is told to a client that only listens: {message}");
        let found = message["entries"].as_array().unwrap().iter().find(|entry| entry["id"] == "builtin:osc").cloned();
        told.push(message);
        if let Some(found) = found {
            break found;
        }
        assert!(Instant::now() < deadline, "the built-ins did not come in 30 s; told {told:?}\n{}", tang.log());
    };
    assert_eq!((osc["name"].clone(), osc["format"].clone(), osc["isInstrument"].clone()), (json!("Oscillator"), json!("Built-in"), json!(true)), "{osc}");
    assert!(osc["params"].as_u64().unwrap() > 0, "{osc}");

    // Which plugins are installed is the machine's business: where, nobody's but the program's.
    let last = told.last().unwrap();
    assert!(last["rev"].as_u64().unwrap() > 0, "{last}");
    for entry in last["entries"].as_array().unwrap() {
        let mut keys: Vec<_> = entry.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["format", "id", "isInstrument", "name", "params", "presets"], "{entry}");
    }
    assert!(!last.to_string().contains(env!("CARGO_MANIFEST_DIR")), "no path of the machine in the catalog");
}

#[tokio::test]
async fn a_session_named_by_a_relative_path_is_saved_to_that_file_wherever_the_program_is_by_then() {
    // Plugins that are scanned or loaded can change the working directory of the whole program (some
    // do), and a path that is relative to it then leads somewhere else: a save would write the
    // session into a plugin's directory. The program here stays where it is started, and says which file it writes.
    let dir = tempfile::tempdir().unwrap();
    let here = dir.path().canonicalize().unwrap();
    std::fs::copy(TRACER, here.join("jam.toml")).unwrap();
    let tang = Tang::start_in(Some(&here), "jam.toml");
    let (mut client, _, _) = listening(&tang).await;

    tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "save"}})).await;
    assert_eq!(answer(&mut client, 1).await["type"], "ack");
    let wanted = format!("Session saved to {}", here.join("jam.toml").display());
    let deadline = Instant::now() + Duration::from_secs(5);
    while !tang.log().contains(&wanted) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(tang.log().contains(&wanted), "the file is named by where it is, not by how it was asked for:\n{}", tang.log());
}

/// The master's peak a moment after `wait` has passed, as the frames say (the frames are read all the while).
async fn peak_after(client: &mut Client, at: usize, wait: Duration) -> f32 {
    let deadline = Instant::now() + wait;
    let mut last = f32::NAN;
    while Instant::now() < deadline {
        if let Ok(Some(Ok(Message::Binary(frame)))) = tokio::time::timeout(Duration::from_millis(100), client.next()).await {
            last = f32::from_le_bytes(frame[8 + 4 * at..12 + 4 * at].try_into().unwrap());
        }
    }
    last
}

#[tokio::test]
async fn a_browser_puts_a_filter_in_the_chain_and_hears_it_in_the_meter_and_the_saved_file_has_it() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("jam.toml");
    std::fs::copy(TRACER, &file).unwrap();
    let tang = Tang::start_on(&file);
    let (mut client, at, snapshot) = listening(&tang).await;
    let (osc, _) = param_of(&snapshot, "Oscillator", "volume");
    let (first_filter, _) = param_of(&snapshot, "Filter", "cutoff");

    // A note, held: the level the meter shows through the chain as it is.
    tell(&mut client, json!({"type": "noteOn", "note": 60, "velocity": 100})).await;
    peak_until(&mut client, &tang, at, SOON, "the note", |peak| peak > 0.01).await;
    let before = peak_after(&mut client, at, Duration::from_millis(500)).await;
    assert!(before > 0.01, "the note is heard: {before}");

    // The browser puts a filter at the end of the lane's chain (the catalog names it: the built-ins are the first batch
    // the scan finds, folded in long before this): told by a snapshot, answered by the revision.
    tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "addEffect", "parent": osc, "plugin": "builtin:filter"}})).await;
    let mut said = Vec::new();
    let answered = loop {
        let message = next_json(&mut client).await;
        if message["reqId"] == 1 {
            break message;
        }
        said.push(message);
    };
    assert_eq!((answered["type"].clone(), answered["rev"].clone()), (json!("ack"), json!(1)), "{answered}");
    let told = said.iter().find(|m| m["type"] == "snapshot").expect("the new structure is told as a snapshot");
    let names: Vec<_> = told["snapshot"]["graph"]["nodes"].as_array().unwrap().iter().map(|n| n["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Oscillator", "Filter", "Filter", "LFO", "Master"]);
    let (added, cutoff) = {
        let nodes = told["snapshot"]["graph"]["nodes"].as_array().unwrap();
        let added = nodes.iter().find(|n| n["name"] == "Filter" && n["id"] != json!(first_filter)).expect("a second filter");
        let cutoff = added["plugin"]["params"].as_array().unwrap().iter().find(|p| p["name"] == "cutoff").unwrap();
        (added["id"].as_u64().unwrap(), cutoff["index"].as_u64().unwrap())
    };

    // It has a node whose parameters are turned like the others', and the engine has it in the chain: closed down to nothing,
    // the note is hardly heard (the meter falls 40 dB a second, so a moment on it shows what is there).
    tell(&mut client, json!({"type": "request", "reqId": 2, "intent": {"type": "setParam", "node": added, "param": cutoff, "value": 0.0}})).await;
    assert_eq!(answer(&mut client, 2).await["type"], "ack");
    let after = peak_after(&mut client, at, Duration::from_secs(2)).await;
    assert!(after < before / 10.0, "the new filter is in the audio path: {after} against {before}\n{}", tang.log());
    tell(&mut client, json!({"type": "noteOff", "note": 60})).await;

    // And what is saved has it, with the value it was given.
    tell(&mut client, json!({"type": "request", "reqId": 3, "intent": {"type": "save"}})).await;
    assert_eq!(answer(&mut client, 3).await["type"], "ack");
    let saved = std::fs::read_to_string(&file).unwrap();
    assert_eq!(saved.matches("plugin = \"builtin:filter\"").count(), 2, "{saved}");
    assert!(saved.contains("cutoff = 0.0"), "{saved}");
}

/// The answer to request `req_id`, and what was said before it (the snapshot a change of structure is told as comes first).
async fn answer_with_what_came_before(client: &mut Client, req_id: u64) -> (Value, Vec<Value>) {
    let mut said = Vec::new();
    loop {
        let message = next_json(client).await;
        if message["reqId"] == req_id {
            return (message, said);
        }
        said.push(message);
    }
}

#[tokio::test]
async fn a_browser_adds_a_lane_with_an_instrument_and_hears_it_in_the_meter_and_the_saved_file_has_both_lanes() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("jam.toml");
    std::fs::copy(TRACER, &file).unwrap();
    let tang = Tang::start_on(&file);
    let (mut client, at, snapshot) = listening(&tang).await;
    let before_meters = snapshot["snapshot"]["telemetry"]["meters"].as_array().unwrap().len();

    // A note, and the level the tracer's one lane makes of it (quiet on purpose: its volume is 0.1).
    tell(&mut client, json!({"type": "noteOn", "note": 60, "velocity": 100})).await;
    peak_until(&mut client, &tang, at, SOON, "the note", |peak| peak > 0.01).await;
    let before = peak_after(&mut client, at, Duration::from_millis(500)).await;
    tell(&mut client, json!({"type": "noteOff", "note": 60})).await;
    peak_until(&mut client, &tang, at, Duration::from_secs(15), "the note dying away", |peak| peak < 0.001).await;

    // The browser adds a lane with an oscillator in it (the catalog names it: the built-ins are the first batch the scan finds):
    // told by a snapshot, with the lane's meters in its layout, answered by the revision.
    tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "addInstrument", "plugin": "builtin:osc"}})).await;
    let (answered, said) = answer_with_what_came_before(&mut client, 1).await;
    assert_eq!((answered["type"].clone(), answered["rev"].clone()), (json!("ack"), json!(1)), "{answered}");
    let told = said.iter().find(|m| m["type"] == "snapshot").expect("the new structure is told as a snapshot");
    let nodes = told["snapshot"]["graph"]["nodes"].as_array().unwrap();
    let names: Vec<_> = nodes.iter().map(|n| n["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Oscillator", "Filter", "LFO", "Oscillator", "Master"]);
    assert_eq!(nodes[3]["parent"], json!(null), "a lane of its own, in no group");
    assert_eq!(told["snapshot"]["telemetry"]["meters"].as_array().unwrap().len(), before_meters + 2, "a peak and a clip count for the lane");

    // The next note is heard through it: the oscillator is at its own volume, ten times the tracer's lane.
    tell(&mut client, json!({"type": "noteOn", "note": 60, "velocity": 100})).await;
    let after = peak_until(&mut client, &tang, at, SOON, "the note through the new lane", |peak| peak > 3.0 * before).await;
    assert!(after > 3.0 * before, "{after} against {before}\n{}", tang.log());
    tell(&mut client, json!({"type": "noteOff", "note": 60})).await;

    // And what is saved has both lanes.
    tell(&mut client, json!({"type": "request", "reqId": 2, "intent": {"type": "save"}})).await;
    assert_eq!(answer(&mut client, 2).await["type"], "ack");
    let saved = std::fs::read_to_string(&file).unwrap();
    assert_eq!(saved.matches("[[instrument]]").count(), 2, "{saved}");
    assert_eq!(saved.matches("plugin = \"builtin:osc\"").count(), 2, "{saved}");
}

#[tokio::test]
async fn a_browser_replaces_the_instrument_of_a_lane_and_what_was_set_on_the_old_one_is_gone_from_the_sound_the_cables_and_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("jam.toml");
    std::fs::copy(TRACER, &file).unwrap();
    let tang = Tang::start_on(&file);
    let (mut client, at, snapshot) = listening(&tang).await;
    let (osc, volume) = param_of(&snapshot, "Oscillator", "volume");
    let (_, detune) = param_of(&snapshot, "Oscillator", "detune");
    let lfo = snapshot["snapshot"]["graph"]["nodes"].as_array().unwrap().iter().find(|n| n["name"] == "LFO").unwrap()["id"].as_u64().unwrap();

    // The old instrument is turned down to nothing, and has a cable of the lane's LFO on its detune.
    tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "setParam", "node": osc, "param": volume, "value": 0.0}})).await;
    assert_eq!(answer(&mut client, 1).await["type"], "ack");
    let cable = json!({"type": "addModEdge", "modulator": lfo, "to": {"node": osc, "port": {"type": "param", "index": detune}}});
    tell(&mut client, json!({"type": "request", "reqId": 2, "intent": cable})).await;
    assert_eq!(answer(&mut client, 2).await["type"], "ack");
    // (The oscillator eases its volume over a few milliseconds.)
    tokio::time::sleep(Duration::from_millis(100)).await;
    tell(&mut client, json!({"type": "noteOn", "note": 60, "velocity": 100})).await;
    let quiet_until = Instant::now() + Duration::from_millis(1000);
    while Instant::now() < quiet_until {
        if let Ok(Some(Ok(Message::Binary(frame)))) = tokio::time::timeout(Duration::from_millis(100), client.next()).await {
            let peak = f32::from_le_bytes(frame[8 + 4 * at..12 + 4 * at].try_into().unwrap());
            assert!(peak < 0.001, "the volume is 0 and the meter says {peak}");
        }
    }
    tell(&mut client, json!({"type": "noteOff", "note": 60})).await;

    // The browser puts an oscillator in the lane, in place of that one: told by a snapshot, the lane's node the same,
    // the cable on the old one's detune gone with it (the filter's cable is not the old instrument's), and the new
    // instrument's parameters its own.
    tell(&mut client, json!({"type": "request", "reqId": 3, "intent": {"type": "replaceInstrument", "node": osc, "plugin": "builtin:osc"}})).await;
    let (answered, said) = answer_with_what_came_before(&mut client, 3).await;
    // (The revisions: the volume 1, the cable 2 and the snapshot it was told as 3, the replacement 4.)
    assert_eq!((answered["type"].clone(), answered["rev"].clone()), (json!("ack"), json!(4)), "{answered}");
    let told = said.iter().find(|m| m["type"] == "snapshot").expect("the new structure is told as a snapshot");
    let lane = told["snapshot"]["graph"]["nodes"].as_array().unwrap().iter().find(|n| n["id"] == json!(osc)).expect("the lane keeps its node");
    let params: Vec<_> = lane["plugin"]["params"].as_array().unwrap().iter().map(|p| (p["name"].as_str().unwrap(), p["value"].as_f64().unwrap())).collect();
    assert!(params.contains(&("volume", 1.0)) && params.contains(&("waveform", 0.0)), "{params:?}");
    let cables = told["snapshot"]["graph"]["edges"].as_array().unwrap().iter().filter(|e| e["kind"] == "modulation").count();
    assert_eq!(cables, 1, "the cable to the filter is left, and the one to the old detune is not");

    // The same note is heard again: it was the old instrument that was silent.
    tell(&mut client, json!({"type": "noteOn", "note": 60, "velocity": 100})).await;
    peak_until(&mut client, &tang, at, SOON, "the note through the new instrument", |peak| peak > 0.01).await;
    tell(&mut client, json!({"type": "noteOff", "note": 60})).await;

    // What is saved has no trace of the old instrument's edits or its cable, and the rest of the lane is as it was.
    tell(&mut client, json!({"type": "request", "reqId": 4, "intent": {"type": "save"}})).await;
    assert_eq!(answer(&mut client, 4).await["type"], "ack");
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(!saved.contains("[instrument.params]") && !saved.contains("waveform = 3"), "the saw and the volume of 0 are gone: {saved}");
    assert!(!saved.contains("detune"), "the cable is gone: {saved}");
    assert!(!saved.contains("volume = 0.0"), "{saved}");
    assert!(saved.contains("plugin = \"builtin:filter\"") && saved.contains("cutoff"), "the lane has its filter and the LFO's cable to it: {saved}");
    assert!(saved.contains("volume = 0.1"), "and its own volume: {saved}");
}

#[tokio::test]
async fn a_browser_sets_the_keys_of_a_lane_and_a_note_outside_them_is_silent_and_one_inside_is_heard_and_the_saved_file_has_them() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("jam.toml");
    std::fs::copy(TRACER, &file).unwrap();
    let tang = Tang::start_on(&file);
    let (mut client, at, snapshot) = listening(&tang).await;
    let (osc, _) = param_of(&snapshot, "Oscillator", "volume");

    // Every note is heard to begin with (the tracer's one lane is quiet on purpose: its volume is 0.1).
    tell(&mut client, json!({"type": "noteOn", "note": 48, "velocity": 100})).await;
    peak_until(&mut client, &tang, at, SOON, "the note", |peak| peak > 0.01).await;
    tell(&mut client, json!({"type": "noteOff", "note": 48})).await;
    peak_until(&mut client, &tang, at, Duration::from_secs(15), "the note dying away", |peak| peak < 0.001).await;

    // The browser gives the lane the keys C4 to C5: told as an event, answered by the revision.
    tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "setRange", "node": osc, "range": {"low": 60, "high": 72}}})).await;
    let (answered, said) = answer_with_what_came_before(&mut client, 1).await;
    assert_eq!((answered["type"].clone(), answered["rev"].clone()), (json!("ack"), json!(1)), "{answered}");
    let told = said.iter().find(|m| m["type"] == "event").expect("the keys are told as an event");
    assert_eq!(
        (told["event"]["type"].clone(), told["event"]["node"].clone(), told["event"]["range"].clone()),
        (json!("rangeSet"), json!(osc), json!({"low": 60, "high": 72})),
        "{told}"
    );

    // A note below them is not played by the lane, and what is below is silent: the meter has nothing a moment after.
    tell(&mut client, json!({"type": "noteOn", "note": 48, "velocity": 100})).await;
    let outside = peak_after(&mut client, at, Duration::from_millis(800)).await;
    assert!(outside < 0.001, "a note outside the keys is silent: {outside}\n{}", tang.log());
    tell(&mut client, json!({"type": "noteOff", "note": 48})).await;
    // A note inside them is heard.
    tell(&mut client, json!({"type": "noteOn", "note": 64, "velocity": 100})).await;
    peak_until(&mut client, &tang, at, SOON, "a note inside the keys", |peak| peak > 0.01).await;
    tell(&mut client, json!({"type": "noteOff", "note": 64})).await;
    peak_until(&mut client, &tang, at, Duration::from_secs(15), "the note dying away", |peak| peak < 0.001).await;

    // What is saved has the keys, as the TUI writes them.
    tell(&mut client, json!({"type": "request", "reqId": 2, "intent": {"type": "save"}})).await;
    assert_eq!(answer(&mut client, 2).await["type"], "ack");
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(saved.contains("range = \"C4-C5\""), "{saved}");

    // No range is every note again: the note that was silent is heard, and the file has no range.
    tell(&mut client, json!({"type": "request", "reqId": 3, "intent": {"type": "setRange", "node": osc, "range": null}})).await;
    assert_eq!(answer(&mut client, 3).await["type"], "ack");
    tell(&mut client, json!({"type": "noteOn", "note": 48, "velocity": 100})).await;
    peak_until(&mut client, &tang, at, SOON, "the note that was outside the keys", |peak| peak > 0.01).await;
    tell(&mut client, json!({"type": "noteOff", "note": 48})).await;
    tell(&mut client, json!({"type": "request", "reqId": 4, "intent": {"type": "save"}})).await;
    assert_eq!(answer(&mut client, 4).await["type"], "ack");
    assert!(!std::fs::read_to_string(&file).unwrap().contains("range ="), "no keys are every key");

    // A range that is not one is refused in the controller's words, and the lane keeps the keys it had.
    tell(&mut client, json!({"type": "request", "reqId": 5, "intent": {"type": "setRange", "node": osc, "range": {"low": 90, "high": 40}}})).await;
    let refused = answer(&mut client, 5).await;
    assert_eq!(refused["type"], "rejected", "{refused}");
    assert_eq!(refused["error"]["message"], "the range goes from F#6 down to E2: its low note has to be the lower", "{refused}");
}

/// Read frames until the held notes' (channel 2: 128 values, one for each note, 1 for a note that is held) satisfy `done`, and
/// return them. Fails, with what the server logged, when they have not within `within`.
async fn held_until(client: &mut Client, tang: &Tang, within: Duration, what: &str, done: impl Fn(&[f32]) -> bool) -> Vec<f32> {
    let deadline = Instant::now() + within;
    let mut last = Vec::new();
    while Instant::now() < deadline {
        if let Ok(Some(Ok(Message::Binary(frame)))) = tokio::time::timeout(Duration::from_millis(500), client.next()).await {
            if frame[0] != 2 {
                continue;
            }
            assert_eq!((frame[1], u16::from_le_bytes([frame[2], frame[3]]), u32::from_le_bytes(frame[4..8].try_into().unwrap())), (0, 128, 0), "128 notes, and no layout");
            last = frame[8..].chunks(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
            if done(&last) {
                return last;
            }
        }
    }
    let held: Vec<_> = last.iter().enumerate().filter(|(_, v)| **v > 0.0).map(|(note, _)| note).collect();
    panic!("{what}: the held notes were {held:?} after {within:?}\n{}", tang.log());
}

#[tokio::test]
async fn the_notes_that_are_held_are_told_to_every_browser_that_asks_whoever_holds_them() {
    let tang = Tang::start();
    let (mut player, _, _) = listening(&tang).await;
    let mut watcher = connect(&tang).await;
    next_json(&mut watcher).await;
    tell(&mut watcher, json!({"type": "subscribe"})).await;
    assert_eq!(next_json(&mut watcher).await["type"], "snapshot");
    tell(&mut watcher, json!({"type": "telemetry", "channels": ["held"]})).await;

    // Nothing is held to begin with, and the frame is a value for each of the 128 notes.
    let none = held_until(&mut watcher, &tang, SOON, "the first frame", |_| true).await;
    assert_eq!(none.len(), 128);
    assert!(none.iter().all(|v| *v == 0.0), "{none:?}");

    // What one browser plays, another sees, by the number of the note.
    tell(&mut player, json!({"type": "noteOn", "note": 60, "velocity": 100})).await;
    tell(&mut player, json!({"type": "noteOn", "note": 64, "velocity": 100})).await;
    let two = held_until(&mut watcher, &tang, SOON, "the two notes", |v| v[60] == 1.0 && v[64] == 1.0).await;
    assert_eq!(two.iter().filter(|v| **v > 0.0).count(), 2, "and no other");
    tell(&mut player, json!({"type": "noteOff", "note": 60})).await;
    held_until(&mut watcher, &tang, SOON, "the first note ending", |v| v[60] == 0.0 && v[64] == 1.0).await;

    // A browser that goes with a note held leaves nothing held: the server ends its notes.
    drop(player);
    held_until(&mut watcher, &tang, SOON, "the note ending with the browser that held it", |v| v.iter().all(|x| *x == 0.0)).await;
}

#[tokio::test]
async fn a_browser_locks_the_piano_to_a_scale_and_a_note_outside_it_is_dropped_before_it_is_heard_or_held_and_the_saved_file_has_the_scale() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("jam.toml");
    std::fs::copy(TRACER, &file).unwrap();
    let tang = Tang::start_on(&file);
    let (mut client, at, _) = listening(&tang).await;
    // (Another browser watches the notes that are held.)
    let mut watcher = connect(&tang).await;
    next_json(&mut watcher).await;
    tell(&mut watcher, json!({"type": "subscribe"})).await;
    assert_eq!(next_json(&mut watcher).await["type"], "snapshot");
    tell(&mut watcher, json!({"type": "telemetry", "channels": ["held"]})).await;

    // The scale is told as an event, in the form the session file has it and with the notes that a page draws.
    tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "setScale", "scale": "Bb Natural Minor"}})).await;
    let (answered, said) = answer_with_what_came_before(&mut client, 1).await;
    assert_eq!((answered["type"].clone(), answered["rev"].clone()), (json!("ack"), json!(1)), "{answered}");
    let told = said.iter().find(|m| m["type"] == "event").expect("the scale is told as an event");
    assert_eq!(told["event"], json!({"rev": 1, "type": "scaleSet", "scale": "A# minor", "root": 10, "kind": "minor", "notes": [0, 1, 3, 5, 6, 8, 10]}), "{told}");
    tell(&mut client, json!({"type": "request", "reqId": 2, "intent": {"type": "setLocked", "locked": true}})).await;
    let (answered, said) = answer_with_what_came_before(&mut client, 2).await;
    assert_eq!((answered["type"].clone(), answered["rev"].clone()), (json!("ack"), json!(2)), "{answered}");
    let told = said.iter().find(|m| m["type"] == "event").expect("the lock is told as an event");
    assert_eq!(told["event"], json!({"rev": 2, "type": "lockedSet", "locked": true}), "{told}");

    // D4 (62) is not in A# minor: it is dropped where it is sent, so the lane is silent...
    tell(&mut client, json!({"type": "noteOn", "note": 62, "velocity": 100})).await;
    let outside = peak_after(&mut client, at, Duration::from_millis(800)).await;
    assert!(outside < 0.001, "a note outside the scale is silent: {outside}\n{}", tang.log());
    // ...and C#4 (61) is, and is heard and held (the note before it was handled first: it is not held either).
    tell(&mut client, json!({"type": "noteOn", "note": 61, "velocity": 100})).await;
    let held = held_until(&mut watcher, &tang, SOON, "a note inside the scale", |v| v[61] == 1.0).await;
    assert_eq!(held[62], 0.0, "the note outside the scale was never held");
    peak_until(&mut client, &tang, at, SOON, "a note inside the scale", |peak| peak > 0.01).await;
    tell(&mut client, json!({"type": "noteOff", "note": 61})).await;
    peak_until(&mut client, &tang, at, Duration::from_secs(15), "the note dying away", |peak| peak < 0.001).await;

    // What is saved has the scale as the TUI writes it, and the lock.
    tell(&mut client, json!({"type": "request", "reqId": 3, "intent": {"type": "save"}})).await;
    assert_eq!(answer(&mut client, 3).await["type"], "ack");
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(saved.contains("[piano]") && saved.contains("scale = \"A# minor\"") && saved.contains("locked = true"), "{saved}");

    // Let go, the note that was dropped is played, held and heard.
    tell(&mut client, json!({"type": "request", "reqId": 4, "intent": {"type": "setLocked", "locked": false}})).await;
    assert_eq!(answer(&mut client, 4).await["type"], "ack");
    tell(&mut client, json!({"type": "noteOn", "note": 62, "velocity": 100})).await;
    held_until(&mut watcher, &tang, SOON, "the note that was dropped", |v| v[62] == 1.0).await;
    peak_until(&mut client, &tang, at, SOON, "the note that was dropped", |peak| peak > 0.01).await;
    tell(&mut client, json!({"type": "noteOff", "note": 62})).await;

    // Text that is not a scale is refused in the controller's words, and the scale stays.
    tell(&mut client, json!({"type": "request", "reqId": 5, "intent": {"type": "setScale", "scale": "Cé"}})).await;
    let refused = answer(&mut client, 5).await;
    assert_eq!(refused["type"], "rejected", "{refused}");
    assert_eq!(refused["error"]["message"], "\"Cé\" is not a scale: a root and a kind of scale, like C major or F# dorian", "{refused}");
}

/// The next value, in a frame of the modulation channel of the layout `rev`, at place `at` of it (frames of another layout, and of other channels, are skipped).
async fn modulation_value(client: &mut Client, rev: u32, at: usize) -> f32 {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Ok(Some(Ok(Message::Binary(frame)))) = tokio::time::timeout(Duration::from_millis(500), client.next()).await
            && frame[0] == 1
            && u32::from_le_bytes(frame[4..8].try_into().unwrap()) == rev
        {
            return f32::from_le_bytes(frame[8 + 4 * at..12 + 4 * at].try_into().unwrap());
        }
    }
    panic!("no frame of the modulation channel of the layout {rev} in ten seconds");
}

/// Throw away what the connection has already received and not been read: the frames that came while a test was waiting say what was, not what is.
async fn drain(client: &mut Client) {
    while let Ok(Some(Ok(_))) = tokio::time::timeout(Duration::from_millis(10), client.next()).await {}
}

#[tokio::test]
async fn a_preset_loaded_into_a_plugin_that_a_cable_drives_is_what_the_cable_is_centred_on_in_the_audio_thread() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("jam.toml");
    std::fs::copy(TRACER, &file).unwrap();
    let tang = Tang::start_on(&file);
    let (mut client, _, snapshot) = listening(&tang).await;
    let (osc, _) = param_of(&snapshot, "Oscillator", "volume");
    let lfo = snapshot["snapshot"]["graph"]["nodes"].as_array().unwrap().iter().find(|n| n["name"] == "LFO").unwrap()["id"].as_u64().unwrap();
    // (The catalog follows the snapshot, as far as the scan has got: the built-ins are its first batch. A plugin is only added once it is told.)
    loop {
        let message = next_json(&mut client).await;
        if message["type"] == "catalog" && message["entries"].as_array().unwrap().iter().any(|e| e["id"] == "builtin:reverb") {
            break;
        }
    }

    // A reverb in the lane's chain, and the lane's LFO on its decay at a small depth: the decay is 0.88 as the reverb is made, and the
    // cable moves it by a tenth of its range (0.89) either way.
    tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "addEffect", "parent": osc, "plugin": "builtin:reverb"}})).await;
    let (acked, said) = answer_with_what_came_before(&mut client, 1).await;
    assert_eq!(acked["type"], "ack", "{acked}");
    let told = said.iter().find(|m| m["type"] == "snapshot").expect("a snapshot");
    let (reverb, decay) = param_of(told, "Reverb", "decay");
    let cable = json!({"type": "addModEdge", "modulator": lfo, "to": {"node": reverb, "port": {"type": "param", "index": decay}}});
    tell(&mut client, json!({"type": "request", "reqId": 2, "intent": cable})).await;
    let (acked, said) = answer_with_what_came_before(&mut client, 2).await;
    assert_eq!(acked["type"], "ack", "{acked}");
    let told = said.iter().find(|m| m["type"] == "snapshot").expect("a snapshot");
    let edge = told["snapshot"]["graph"]["edges"].as_array().unwrap().iter().find(|e| e["kind"] == "modulation" && e["to"]["node"] == json!(reverb)).expect("the cable")["id"].clone();
    tell(&mut client, json!({"type": "request", "reqId": 3, "intent": {"type": "setEdgeDepth", "edge": edge, "depth": 0.1}})).await;
    assert_eq!(answer(&mut client, 3).await["type"], "ack");

    // Where the value the cable puts on the decay is in the frames of the modulation channel.
    let layout = &told["snapshot"]["telemetry"];
    let at = layout["modulation"].as_array().unwrap().iter().position(|e| e["kind"] == "effective" && e["node"] == json!(reverb) && e["port"]["index"] == json!(decay)).expect("a slot for the decay");
    let rev = layout["rev"].as_u64().unwrap() as u32;
    tell(&mut client, json!({"type": "telemetry", "channels": ["modulation"]})).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    drain(&mut client).await;
    for _ in 0..10 {
        let value = modulation_value(&mut client, rev, at).await;
        assert!((0.78..=0.99).contains(&value), "the decay is modulated around the reverb's own: {value}");
        tokio::time::sleep(Duration::from_millis(40)).await;
    }

    // The room has a decay of 0.55. The cable is centred on that now, and the decay moves around it (the old base, put back at the next buffer, would
    // have the decay at 0.79 or more for the preset that was loaded).
    tell(&mut client, json!({"type": "request", "reqId": 4, "intent": {"type": "loadPreset", "node": reverb, "preset": "room"}})).await;
    assert_eq!(answer(&mut client, 4).await["type"], "ack");
    tokio::time::sleep(Duration::from_millis(300)).await;
    drain(&mut client).await;
    for _ in 0..20 {
        let value = modulation_value(&mut client, rev, at).await;
        assert!((0.4..=0.7).contains(&value), "the decay is modulated around the preset's: {value}\n{}", tang.log());
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

#[tokio::test]
async fn a_second_browser_hears_what_the_first_one_changes() {
    let tang = Tang::start();
    let (mut first, _, snapshot) = listening(&tang).await;
    let (mut second, _, _) = listening(&tang).await;
    let (filter, cutoff) = param_of(&snapshot, "Filter", "cutoff");

    tell(&mut first, json!({"type": "request", "reqId": 9, "intent": {"type": "setParam", "node": filter, "param": cutoff, "value": 0.8}})).await;
    assert_eq!(answer(&mut first, 9).await["rev"], 1);
    // (The second hears the event somewhere among its own telemetry frames.)
    loop {
        let message = next_json(&mut second).await;
        if message["type"] == "event" {
            assert_eq!(message["event"]["type"], "paramSet");
            assert_eq!((message["event"]["node"].clone(), message["event"]["param"].clone()), (json!(filter), json!(cutoff)));
            assert_eq!(message["event"]["rev"], 1);
            break;
        }
    }
}

#[tokio::test]
async fn a_browser_that_vanishes_with_a_key_held_does_not_leave_the_note_sounding() {
    let tang = Tang::start();
    let (mut watcher, at, _) = listening(&tang).await;
    let mut player = connect(&tang).await;
    next_json(&mut player).await;
    tell(&mut player, json!({"type": "noteOn", "note": 67, "velocity": 100})).await;
    peak_until(&mut watcher, &tang, at, SOON, "the note", |peak| peak > 0.01).await;

    // The tab is closed: no note off, no close frame, the socket is simply gone.
    drop(player);
    peak_until(&mut watcher, &tang, at, Duration::from_secs(15), "the note ending with its connection", |peak| peak < 0.001).await;
}

/// A plain HTTP request: the status line and the whole answer.
fn http(tang: &Tang, host: &str, extra: &str) -> (u16, String) {
    let mut stream = std::net::TcpStream::connect(tang.addr).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write!(stream, "GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n{extra}\r\n").unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let status = response.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, response)
}

#[tokio::test]
async fn pages_and_hosts_that_are_not_this_servers_are_refused() {
    let tang = Tang::start();
    let own = tang.addr.to_string();

    let (status, page) = http(&tang, &own, "");
    assert_eq!(status, 200);
    // (The interface if it has been built, and what says how to build it if not: both are called Tang.)
    assert!(page.contains("<title>Tang</title>"), "{page}");

    // DNS rebinding: a name of somebody else's that points here.
    assert_eq!(http(&tang, &format!("evil.example:{}", tang.addr.port()), "").0, 403);
    // A page on another origin, for the page and for the WebSocket.
    assert_eq!(http(&tang, &own, "Origin: http://evil.example\r\n").0, 403);
    match connect_as(&tang, "http://evil.example").await {
        Err(tungstenite::Error::Http(response)) => assert_eq!(response.status().as_u16(), 403),
        other => panic!("{:?}", other.map(|_| ())),
    }
    // Nobody was let in: the session has not been touched.
    let (mut client, _, snapshot) = listening(&tang).await;
    assert_eq!(snapshot["snapshot"]["rev"], 0);
    let _ = client.close(None).await;
}

#[cfg(unix)]
/// A port that is taken is found out at once, not after the session has loaded (which takes
/// seconds with a heavy instrument): the session here is one that cannot be loaded, so what the
/// program says first is what it found out first. `tang serve` and `tang --web` alike.
#[test]
fn a_port_that_is_taken_is_reported_before_the_session_is_loaded() {
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = taken.local_addr().unwrap();
    let web_bind = addr.to_string();
    let web = format!("--web={addr}");
    for args in [
        vec!["serve", "/no/such/session.toml", "--web-bind", web_bind.as_str()],
        vec![web.as_str(), "/no/such/session.toml"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_tang"))
            .args(["--audio-device", "none", "--midi-device", "no such device"])
            .args(&args)
            .stdin(Stdio::null())
            .output()
            .expect("tang runs");
        let said = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{args:?}: {said}");
        assert!(said.contains(&format!("cannot listen on {addr}")), "{args:?}: the program said: {said}");
    }
}

#[tokio::test]
async fn ctrl_c_closes_the_browsers_and_stops_the_program() {
    let mut tang = Tang::start();
    let (mut client, _, _) = listening(&tang).await;
    tang.interrupt();
    // The browser is told the server is going away.
    let mut code = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(2), client.next()).await {
            Ok(Some(Ok(Message::Close(frame)))) => {
                code = frame.map(|frame| u16::from(frame.code));
                break;
            }
            Ok(Some(Ok(_))) | Err(_) => {}
            Ok(Some(Err(_))) | Ok(None) => break,
        }
    }
    assert_eq!(code, Some(1001), "{}", tang.log());
    let status = tang.exit_within(Duration::from_secs(10)).unwrap_or_else(|| panic!("tang did not stop\n{}", tang.log()));
    assert!(status.success(), "{status:?}\n{}", tang.log());
    assert!(std::net::TcpStream::connect(tang.addr).is_err(), "nobody listens any more");
}
