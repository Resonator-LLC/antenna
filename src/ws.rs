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
    tracing::info!(addr = %addr, "WebSocket server listening");

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
            tracing::debug!("waiting for WebSocket client");
            match listener.accept() {
                Ok((stream, peer)) => {
                    tracing::info!(peer = %peer, "client connected");
                    let in_tx = in_tx.clone();
                    let client_tx_sender = &client_tx_sender;
                    let greeting = &greeting;
                    let result = catch_unwind(AssertUnwindSafe(|| {
                        match accept(stream) {
                            Ok(mut ws) => {
                                tracing::debug!("WebSocket handshake complete");

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
                                                tracing::info!("client disconnected");
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
                                                tracing::warn!("client connection lost");
                                                break 'client;
                                            }
                                            _ => {}
                                        }
                                    }

                                    // Write to WS — drain all pending outgoing messages
                                    while let Ok(msg) = per_client_rx.try_recv() {
                                        if ws.send(Message::Text(msg)).is_err() {
                                            tracing::warn!("write error, dropping client");
                                            break 'client;
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!(%e, "WebSocket handshake failed");
                            }
                        }
                    }));
                    if let Err(panic) = result {
                        tracing::error!(?panic, "client handler panicked");
                    }
                }
                Err(e) => {
                    tracing::error!(%e, "accept error");
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
        }
    });

    Ok((WsIn { rx: in_rx }, WsOut { tx: out_tx }))
}
