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

/// A saw oscillator through a filter an LFO sweeps; quiet on purpose.
const TRACER: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/tracer.toml");

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
