use std::thread;
use std::time::Duration;

use antenna::channel::{AntennaIn, AntennaOut};
use antenna::ws::start_ws_server;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{connect, Message};

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Poll `WsIn::recv()` with a short retry loop (up to `timeout`).
fn recv_with_retry(ws_in: &mut antenna::ws::WsIn, timeout: Duration) -> Option<String> {
    let start = std::time::Instant::now();
    loop {
        if let Some(msg) = ws_in.recv() {
            return Some(msg);
        }
        if start.elapsed() >= timeout {
            return None;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn test_ws_server_sends_greeting() {
    let port = free_port();
    let greeting = "@prefix ant: <http://resonator.network/v2/antenna#> .".to_string();

    let (_ws_in, _ws_out) =
        start_ws_server(port, Some(greeting.clone())).expect("server should start");

    thread::sleep(Duration::from_millis(50));

    let (mut client, _response) =
        connect(format!("ws://127.0.0.1:{}", port)).expect("client should connect");

    let msg = client.read().expect("should receive greeting");
    match msg {
        Message::Text(text) => assert_eq!(text, greeting),
        other => panic!("expected Text message, got {:?}", other),
    }

    client.close(None).ok();
}

#[test]
fn test_ws_client_to_server() {
    let port = free_port();

    let (mut ws_in, _ws_out) =
        start_ws_server(port, None).expect("server should start");

    thread::sleep(Duration::from_millis(50));

    let (mut client, _response) =
        connect(format!("ws://127.0.0.1:{}", port)).expect("client should connect");

    let turtle = "<http://example.org/s> <http://example.org/p> \"hello\" .";
    client
        .send(Message::Text(turtle.to_string()))
        .expect("client should send");

    let received = recv_with_retry(&mut ws_in, Duration::from_secs(2));
    assert_eq!(received, Some(turtle.to_string()));

    client.close(None).ok();
}

#[test]
fn test_ws_server_to_client() {
    let port = free_port();

    let (_ws_in, mut ws_out) =
        start_ws_server(port, None).expect("server should start");

    thread::sleep(Duration::from_millis(50));

    let (mut client, _response) =
        connect(format!("ws://127.0.0.1:{}", port)).expect("client should connect");

    // Small sleep so the server registers the client for outgoing messages
    thread::sleep(Duration::from_millis(100));

    let turtle = "<http://example.org/s> <http://example.org/p> \"world\" .";
    ws_out.send(turtle);

    // Set a read timeout so the test doesn't hang if something goes wrong.
    if let MaybeTlsStream::Plain(ref tcp) = client.get_ref() {
        tcp.set_read_timeout(Some(Duration::from_secs(2))).ok();
    }

    let msg = client.read().expect("client should receive message");
    match msg {
        Message::Text(text) => assert_eq!(text, turtle),
        other => panic!("expected Text message, got {:?}", other),
    }

    client.close(None).ok();
}

#[test]
fn test_ws_client_disconnect() {
    let port = free_port();

    let (_ws_in, _ws_out) =
        start_ws_server(port, None).expect("server should start");

    thread::sleep(Duration::from_millis(50));

    // First client connects then disconnects
    {
        let (mut client, _response) =
            connect(format!("ws://127.0.0.1:{}", port)).expect("first client should connect");
        thread::sleep(Duration::from_millis(50));
        client.close(None).ok();
        // Drain close acknowledgement
        loop {
            match client.read() {
                Ok(Message::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
    }

    // Give the server time to accept the disconnect and loop back to accept()
    thread::sleep(Duration::from_millis(200));

    // Second client should be able to connect
    let (mut client2, _response) =
        connect(format!("ws://127.0.0.1:{}", port)).expect("second client should connect after first disconnects");

    // Verify the second connection is functional by sending a message
    client2
        .send(Message::Text("ping".to_string()))
        .expect("second client should be able to send");

    client2.close(None).ok();
}
