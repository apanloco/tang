//! A WebSocket client for the tests of the web runtime: the real thing
//! (`tokio-tungstenite`) against a server on a local port, and a plain HTTP request
//! for what is not a WebSocket. Everything waits a few seconds at most, so a server
//! that says nothing fails a test instead of hanging it.

use std::io::{Read, Write};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

pub type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Open a WebSocket to the server at `addr`, with these headers added to the request
/// (lower case names): `Origin`, say, or another `Host`.
pub async fn connect_as(addr: SocketAddr, headers: &[(&'static str, String)]) -> Result<Client, tungstenite::Error> {
    let mut request = format!("ws://{addr}/ws").into_client_request().unwrap();
    for (name, value) in headers {
        request.headers_mut().insert(*name, value.parse().unwrap());
    }
    tokio_tungstenite::connect_async(request).await.map(|(client, _response)| client)
}

pub async fn connect(addr: SocketAddr) -> Client {
    connect_as(addr, &[]).await.expect("the server takes the connection")
}

/// The next frame, or `None` when the connection is over.
pub async fn recv(client: &mut Client) -> Option<Message> {
    match tokio::time::timeout(Duration::from_secs(5), client.next()).await.expect("nothing from the server in five seconds") {
        Some(Ok(message)) => Some(message),
        Some(Err(_)) | None => None,
    }
}

/// The next frame, which has to be JSON text.
pub async fn said(client: &mut Client) -> Value {
    match recv(client).await {
        Some(Message::Text(text)) => serde_json::from_str(text.as_str()).unwrap(),
        other => panic!("expected a text frame, got {other:?}"),
    }
}

/// Everything the server says until the connection is over: the JSON texts, and the
/// code of the close frame (if there was one).
pub async fn until_closed(client: &mut Client) -> (Vec<Value>, Option<u16>) {
    let (mut texts, mut code) = (Vec::new(), None);
    while let Some(message) = recv(client).await {
        match message {
            Message::Text(text) => texts.push(serde_json::from_str(text.as_str()).unwrap()),
            Message::Close(frame) => code = frame.map(|frame| u16::from(frame.code)),
            _ => {}
        }
    }
    (texts, code)
}

pub async fn tell(client: &mut Client, message: Value) {
    client.send(Message::text(message.to_string())).await.unwrap();
}

/// Subscribe, and take what the server says to a client that has: the snapshot, the plugin
/// catalog after it, and what Undo and Redo would do. The snapshot.
pub async fn subscribe(client: &mut Client) -> Value {
    tell(client, json!({"type": "subscribe"})).await;
    let snapshot = said(client).await;
    assert_eq!(snapshot["type"], "snapshot");
    let catalog = said(client).await;
    assert_eq!(catalog["type"], "catalog", "the catalog follows the snapshot: {catalog}");
    let history = said(client).await;
    assert_eq!(history["type"], "history", "what can be undone follows the catalog: {history}");
    snapshot
}

/// Wait (a few seconds at most) for something another thread does.
pub async fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < deadline, "{what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// A plain HTTP/1.1 request, for what is not a WebSocket: the status, the headers
/// (names in lower case) and the body.
pub fn get(addr: SocketAddr, host: Option<&str>, method: &str, path: &str, extra: &[(&str, &str)]) -> (u16, Vec<(String, String)>, String) {
    let mut stream = std::net::TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut request = format!("{method} {path} HTTP/1.1\r\nConnection: close\r\n");
    if let Some(host) = host {
        request += &format!("Host: {host}\r\n");
    }
    for (name, value) in extra {
        request += &format!("{name}: {value}\r\n");
    }
    request += "\r\n";
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let (head, body) = response.split_once("\r\n\r\n").expect("a response with a head");
    let mut lines = head.lines();
    let status = lines.next().unwrap().split_whitespace().nth(1).unwrap().parse().unwrap();
    let headers = lines
        .map(|line| {
            let (name, value) = line.split_once(':').unwrap();
            (name.to_ascii_lowercase(), value.trim().to_string())
        })
        .collect();
    (status, headers, body.to_string())
}

/// The value of the header `name` (lower case) of a response, if it has one.
pub fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers.iter().find(|(n, _)| n == name).map(|(_, value)| value.as_str())
}
