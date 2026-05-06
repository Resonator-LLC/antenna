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

/// Task #14 — message bursts must not drop the client.
///
/// The pre-Task-#14 ws.rs treated every `tungstenite::Error` from
/// `ws.send` as terminal. Because the per-client TCP socket is
/// non-blocking (set in ws.rs to drive read polling via libc::poll),
/// a tight burst of writes that fills the OS send buffer returns
/// `Io(WouldBlock)` — and the prior code interpreted that as
/// "client gone, drop it." The M5-C scene SPARQL response (5+ rows
/// → ~30 Turtle messages) reproduces the buffer-fill condition
/// reliably enough that live Station renders flashed "Scene not in
/// store" while antenna logged `WARN [WS] write error, dropping client`.
///
/// This test bursts a couple hundred small messages back-to-back so
/// at least one write hits the WouldBlock path on macOS (default 64KB
/// SO_SNDBUF), then asserts (a) the client receives all of them and
/// (b) the server didn't tear down the connection mid-burst.
#[test]
fn test_ws_burst_does_not_drop_client_on_wouldblock() {
    let port = free_port();
    let (_ws_in, mut ws_out) =
        start_ws_server(port, None).expect("server should start");
    thread::sleep(Duration::from_millis(50));

    let (mut client, _response) =
        connect(format!("ws://127.0.0.1:{}", port)).expect("client should connect");
    // Bound the client read so the test fails loudly instead of hanging
    // if the server drops the connection mid-burst.
    if let MaybeTlsStream::Plain(ref tcp) = client.get_ref() {
        tcp.set_read_timeout(Some(Duration::from_secs(5))).ok();
    }

    // Wait for the forwarder to register the client. Without this the
    // first few messages race the registration (covered by a separate
    // test below) and cloud the burst-tolerance signal.
    thread::sleep(Duration::from_millis(150));

    // Each message ~1KB; 500 × 1KB = 500KB pushes well past the default
    // 64KB SO_SNDBUF on macOS so several writes WouldBlock.
    let payload = "x".repeat(1024);
    let count = 500;
    for i in 0..count {
        ws_out.send(&format!("<urn:burst:{}> <p> \"{}\" .", i, payload));
    }

    // Read all messages back. Each ws.read drains one message; we walk
    // until we've seen `count` Text frames or the read times out.
    let mut received = 0usize;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while received < count && std::time::Instant::now() < deadline {
        match client.read() {
            Ok(Message::Text(text)) => {
                let expected_prefix = format!("<urn:burst:{}>", received);
                assert!(
                    text.starts_with(&expected_prefix),
                    "out-of-order burst message at index {}: got {}",
                    received,
                    &text[..text.len().min(64)],
                );
                received += 1;
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(ref e))
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                // Brief stall — give server time to flush more.
                thread::sleep(Duration::from_millis(20));
            }
            Err(e) => panic!(
                "server dropped client mid-burst (received {}/{}): {:?}",
                received, count, e,
            ),
        }
    }
    assert_eq!(
        received, count,
        "client received only {} of {} burst messages — server must have dropped",
        received, count,
    );

    client.close(None).ok();
}

/// Task #14 — the FIRST message a server emits after a client connects
/// must be delivered.
///
/// The pre-Task-#14 forwarder thread had a registration-vs-message race:
/// a client could register its `per_client_tx` AFTER the forwarder's
/// top-of-loop `try_recv` ran but BEFORE `out_rx.recv_timeout` returned
/// the inbound message. The forwarder would then drop that first message
/// silently because `current_client` was still `None`. Station worked
/// around this by sending an `sp:Ask` warmup probe that retries on
/// timeout — once antenna covers the race, the warmup can retire.
///
/// This test reproduces the race by sending an outbound message
/// immediately after `connect()` returns, with NO grace sleep on the
/// server side. The forwarder must pick up the new registration on the
/// `recv_timeout(Ok(...))` path before forwarding.
#[test]
fn test_ws_first_message_after_connect_is_delivered() {
    let port = free_port();
    let (_ws_in, mut ws_out) =
        start_ws_server(port, None).expect("server should start");
    thread::sleep(Duration::from_millis(50));

    let (mut client, _response) =
        connect(format!("ws://127.0.0.1:{}", port)).expect("client should connect");
    if let MaybeTlsStream::Plain(ref tcp) = client.get_ref() {
        tcp.set_read_timeout(Some(Duration::from_secs(2))).ok();
    }

    // Emit immediately — no warmup sleep on the server side. The
    // forwarder must close the registration race itself; otherwise this
    // message lands in out_rx before the per_client_tx registration is
    // visible and gets dropped.
    let turtle = "<http://example.org/s> <http://example.org/p> \"first\" .";
    ws_out.send(turtle);

    let msg = client.read().expect("first message should arrive without warmup");
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
