///! WebSocket transport for Antenna.
///! Accepts multiple sequential clients (one at a time).
///! Each WS message = one Turtle document, dispatched through the same pipeline.

use std::net::TcpListener;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc;
use std::thread;

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
    eprintln!("antenna: WebSocket server listening on ws://{}", addr);

    // Channels shared across all client connections
    let (in_tx, in_rx) = mpsc::channel::<String>();
    let (out_tx, out_rx) = mpsc::channel::<String>();

    // Accept loop runs in a background thread
    thread::spawn(move || {
        // We need to share out_rx across clients. Since only one client
        // is active at a time, we use a wrapper.
        // Actually, mpsc::Receiver can't be shared. Instead, use a separate
        // channel pair per client, with the out_rx drained by each client thread.

        // Simpler: keep out_rx in this thread, forward to active client.
        let mut active_ws_tx: Option<mpsc::Sender<String>> = None;

        // Spawn a thread to forward out_rx → active client
        let (client_tx_sender, client_tx_receiver) = mpsc::channel::<mpsc::Sender<String>>();

        thread::spawn(move || {
            let mut current_client: Option<mpsc::Sender<String>> = None;
            loop {
                // Check for new client registration
                if let Ok(new_tx) = client_tx_receiver.try_recv() {
                    current_client = Some(new_tx);
                }
                // Forward outgoing messages to current client
                if let Ok(msg) = out_rx.try_recv() {
                    if let Some(ref tx) = current_client {
                        if tx.send(msg).is_err() {
                            current_client = None;
                        }
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        });

        // Accept clients in a loop
        loop {
            eprintln!("antenna: waiting for WebSocket client...");
            match listener.accept() {
                Ok((stream, peer)) => {
                    eprintln!("antenna: client connected from {}", peer);
                    let in_tx = in_tx.clone();
                    let ref client_tx_sender = client_tx_sender;
                    let ref greeting = greeting;
                    let result = catch_unwind(AssertUnwindSafe(|| {
                        match accept(stream) {
                            Ok(mut ws) => {
                                eprintln!("antenna: WebSocket handshake complete");

                                // Send greeting (prefixes) to new client
                                if let Some(ref g) = greeting {
                                    let _ = ws.send(Message::Text(g.clone()));
                                }

                                // Per-client outgoing channel
                                let (per_client_tx, per_client_rx) = mpsc::channel::<String>();
                                let _ = client_tx_sender.send(per_client_tx);

                                ws.get_mut().set_nonblocking(true).ok();

                                // Handle this client until disconnect
                                'client: loop {
                                    // Read from WS
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
                                            eprintln!("antenna: client disconnected");
                                            let _ = ws.close(frame);
                                            // Drain until peer acknowledges or connection drops
                                            loop {
                                                match ws.read() {
                                                    Ok(Message::Close(_)) | Err(_) => break,
                                                    _ => {}
                                                }
                                            }
                                            break 'client;
                                        }
                                        Err(tungstenite::Error::Io(ref e))
                                            if e.kind() == std::io::ErrorKind::WouldBlock => {}
                                        Err(_) => {
                                            eprintln!("antenna: client connection lost");
                                            break 'client;
                                        }
                                        _ => {}
                                    }

                                    // Write to WS
                                    while let Ok(msg) = per_client_rx.try_recv() {
                                        if ws.send(Message::Text(msg)).is_err() {
                                            eprintln!("antenna: write error, dropping client");
                                            break 'client;
                                        }
                                    }

                                    std::thread::sleep(std::time::Duration::from_millis(1));
                                }
                            }
                            Err(e) => {
                                eprintln!("antenna: WebSocket handshake failed: {}", e);
                            }
                        }
                    }));
                    if let Err(panic) = result {
                        eprintln!("antenna: client handler panicked: {:?}", panic);
                    }
                }
                Err(e) => {
                    eprintln!("antenna: accept error: {}", e);
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
            }
        }
    });

    Ok((WsIn { rx: in_rx }, WsOut { tx: out_tx }))
}
