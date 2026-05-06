// Copyright (c) 2026-2027 Resonator LLC. Licensed under MIT.

//! WebSocket transport for Antenna.
//! Accepts multiple sequential clients (one at a time).
//! Each WS message = one Turtle document, dispatched through the same pipeline.
use std::net::TcpListener;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use tungstenite::{accept, Message};

use crate::channel::{AntennaIn, AntennaOut};

/// WebSocket IN: receives messages from any connected client.
pub struct WsIn {
    rx: mpsc::Receiver<String>,
}

/// WebSocket OUT: sends messages to the currently connected client.
pub struct WsOut {
    tx: mpsc::Sender<String>,
}

impl AntennaIn for WsIn {
    fn recv(&mut self) -> Option<String> {
        self.rx.try_recv().ok()
    }

    fn clock_fd(&self) -> Option<i32> {
        None
    }
}

impl AntennaOut for WsOut {
    fn send(&mut self, turtle: &str) {
        let _ = self.tx.send(turtle.to_string());
    }
}

/// Start a WebSocket server that accepts clients in a loop.
/// Returns (WsIn, WsOut) — messages from/to whichever client is currently connected.
/// When a client disconnects, the server waits for the next one.
pub fn start_ws_server(port: u16, greeting: Option<String>) -> anyhow::Result<(WsIn, WsOut)> {
    let addr = format!("0.0.0.0:{}", port);
    let listener = TcpListener::bind(&addr)?;
    tracing::info!(target: "WS", addr = %addr, "WebSocket server listening");

    // Channels shared across all client connections
    let (in_tx, in_rx) = mpsc::channel::<String>();
    let (out_tx, out_rx) = mpsc::channel::<String>();

    // Accept loop runs in a background thread
    thread::spawn(move || {
        // Spawn a thread to forward out_rx → active client.
        // Uses recv_timeout instead of try_recv + sleep(1ms) to avoid busy-polling.
        let (client_tx_sender, client_tx_receiver) = mpsc::channel::<mpsc::Sender<String>>();

        thread::spawn(move || {
            let mut current_client: Option<mpsc::Sender<String>> = None;
            loop {
                // Check for new client registration
                if let Ok(new_tx) = client_tx_receiver.try_recv() {
                    current_client = Some(new_tx);
                }
                // Block waiting for outgoing messages (100ms timeout to check for new clients)
                match out_rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(msg) => {
                        // Re-check the registration channel before forwarding.
                        // The accept() side is on a separate thread: a client
                        // can register AFTER the top-of-loop try_recv but
                        // BEFORE recv_timeout returns, so without this second
                        // check the first message destined for the new client
                        // would be silently dropped (current_client still
                        // None). This was the M5-B-α first-frame race that
                        // Station's `sp:Ask` warmup probe was added to mask;
                        // closing it here lets that workaround retire in a
                        // follow-up Station cut.
                        if let Ok(new_tx) = client_tx_receiver.try_recv() {
                            current_client = Some(new_tx);
                        }
                        if let Some(ref tx) = current_client {
                            if tx.send(msg).is_err() {
                                current_client = None;
                            }
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        });

        // Accept clients in a loop
        loop {
            tracing::debug!(target: "WS", "waiting for WebSocket client");
            match listener.accept() {
                Ok((stream, peer)) => {
                    tracing::info!(target: "WS", peer = %peer, "client connected");
                    let in_tx = in_tx.clone();
                    let client_tx_sender = &client_tx_sender;
                    let greeting = &greeting;
                    let result = catch_unwind(AssertUnwindSafe(|| {
                        match accept(stream) {
                            Ok(mut ws) => {
                                tracing::debug!(target: "WS", "WebSocket handshake complete");

                                // Send greeting (prefixes) to new client
                                if let Some(ref g) = greeting {
                                    let _ = ws.send(Message::Text(g.clone()));
                                }

                                // Per-client outgoing channel
                                let (per_client_tx, per_client_rx) = mpsc::channel::<String>();
                                let _ = client_tx_sender.send(per_client_tx);

                                // Use poll() on the TCP socket fd instead of
                                // non-blocking + sleep(1ms) busy-loop.
                                ws.get_mut().set_nonblocking(true).ok();

                                #[cfg(unix)]
                                let tcp_fd = {
                                    use std::os::unix::io::AsRawFd;
                                    ws.get_ref().as_raw_fd()
                                };

                                // Handle this client until disconnect
                                'client: loop {
                                    // Poll the TCP socket for incoming data (100ms timeout)
                                    #[cfg(unix)]
                                    {
                                        let mut pfd = libc::pollfd {
                                            fd: tcp_fd,
                                            events: libc::POLLIN,
                                            revents: 0,
                                        };
                                        // SAFETY: pfd is a valid pollfd struct on the stack;
                                        // tcp_fd is a valid file descriptor owned by the WS.
                                        unsafe {
                                            libc::poll(&mut pfd, 1, 100);
                                        }
                                    }

                                    #[cfg(not(unix))]
                                    std::thread::sleep(Duration::from_millis(100));

                                    // Read from WS
                                    loop {
                                        match ws.read() {
                                            Ok(Message::Text(text)) => {
                                                for line in text.lines() {
                                                    let line = line.trim();
                                                    if !line.is_empty() {
                                                        let _ = in_tx.send(line.to_string());
                                                    }
                                                }
                                            }
                                            Ok(Message::Close(frame)) => {
                                                tracing::info!(target: "WS", "client disconnected");
                                                let _ = ws.close(frame);
                                                loop {
                                                    match ws.read() {
                                                        Ok(Message::Close(_)) | Err(_) => break,
                                                        _ => {}
                                                    }
                                                }
                                                break 'client;
                                            }
                                            Err(tungstenite::Error::Io(ref e))
                                                if e.kind() == std::io::ErrorKind::WouldBlock =>
                                            {
                                                break; // No more data available right now
                                            }
                                            Err(_) => {
                                                tracing::warn!(target: "WS", "client connection lost");
                                                break 'client;
                                            }
                                            _ => {}
                                        }
                                    }

                                    // Write to WS — drain all pending outgoing messages.
                                    //
                                    // The TCP socket is non-blocking (set above for
                                    // poll-driven reads), so tungstenite's `send` may
                                    // return Io(WouldBlock) when the OS send buffer
                                    // fills mid-burst (e.g. the M5-C scene SPARQL
                                    // response that emits ~30 Turtle messages in
                                    // tight succession). WouldBlock is transient —
                                    // tungstenite has internally buffered whatever
                                    // wouldn't fit, and a follow-up flush() on the
                                    // next poll iteration drains it once the kernel
                                    // makes room. Treating WouldBlock as terminal
                                    // (the pre-Task-#14 behaviour) caused live
                                    // Station renders to flash "Scene not in store"
                                    // when antenna prematurely tore down the client
                                    // partway through a multi-row response.
                                    'drain: while let Ok(msg) = per_client_rx.try_recv() {
                                        match ws.send(Message::Text(msg)) {
                                            Ok(()) => {}
                                            Err(tungstenite::Error::Io(ref e))
                                                if e.kind()
                                                    == std::io::ErrorKind::WouldBlock =>
                                            {
                                                // Send buffer full — pause draining.
                                                // The remaining per_client_rx items
                                                // stay queued; tungstenite's internal
                                                // out_buffer will be flushed below
                                                // (and again next iteration if still
                                                // WouldBlock).
                                                break 'drain;
                                            }
                                            Err(e) => {
                                                tracing::warn!(target: "WS", %e, "write error, dropping client");
                                                break 'client;
                                            }
                                        }
                                    }

                                    // Pump tungstenite's internal write buffer to
                                    // the kernel. flush() returns WouldBlock when
                                    // the buffer is partially drained; that's fine
                                    // — we'll loop back here on the next poll pass.
                                    // Only non-WouldBlock errors are terminal.
                                    match ws.flush() {
                                        Ok(()) => {}
                                        Err(tungstenite::Error::Io(ref e))
                                            if e.kind()
                                                == std::io::ErrorKind::WouldBlock => {}
                                        Err(e) => {
                                            tracing::warn!(target: "WS", %e, "flush error, dropping client");
                                            break 'client;
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!(target: "WS", %e, "WebSocket handshake failed");
                            }
                        }
                    }));
                    if let Err(panic) = result {
                        tracing::error!(target: "WS", ?panic, "client handler panicked");
                    }
                }
                Err(e) => {
                    tracing::error!(target: "WS", %e, "accept error");
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
        }
    });

    Ok((WsIn { rx: in_rx }, WsOut { tx: out_tx }))
}
