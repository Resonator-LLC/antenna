// Copyright (c) 2026-2027 Resonator LLC. Licensed under MIT.

//! Transport traits and implementations: AntennaIn/AntennaOut, PipeTransport, internal channels.
use std::io::{self, BufRead, Write};
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Traits
// ---------------------------------------------------------------------------

pub trait AntennaIn: Send {
    fn recv(&mut self) -> Option<String>;
    fn clock_fd(&self) -> Option<RawFd>;
}

pub trait AntennaOut: Send {
    fn send(&mut self, turtle: &str);
}

// ---------------------------------------------------------------------------
// Clock — eventfd on Linux, self-pipe on macOS/iOS
// ---------------------------------------------------------------------------

pub struct Clock {
    read_fd: RawFd,
    write_fd: RawFd,
}

impl Clock {
    pub fn new() -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        {
            // SAFETY: eventfd is a well-defined Linux syscall; the returned fd is
            // owned exclusively by this Clock and closed in Drop.
            let fd = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                read_fd: fd,
                write_fd: fd,
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let mut fds = [0i32; 2];
            // SAFETY: pipe() writes two valid fds into the array; we check the
            // return code and own both fds exclusively (closed in Drop).
            if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            // Set both ends non-blocking
            for &fd in &fds {
                // SAFETY: fd is valid (pipe() succeeded above); fcntl only
                // modifies flags on this fd, no aliasing concerns.
                unsafe {
                    let flags = libc::fcntl(fd, libc::F_GETFL);
                    libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
                    libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
                }
            }
            Ok(Self {
                read_fd: fds[0],
                write_fd: fds[1],
            })
        }
    }

    pub fn signal(&self) {
        // SAFETY: write_fd is a valid fd owned by this Clock (eventfd or pipe
        // write end). A short write on a non-blocking fd is harmless here —
        // the reader only cares that *some* data arrived.
        let buf: [u8; 1] = [1];
        unsafe {
            libc::write(self.write_fd, buf.as_ptr() as *const _, 1);
        }
    }

    pub fn consume(&self) {
        // SAFETY: read_fd is a valid fd owned by this Clock. We read into a
        // stack buffer; partial/failed reads are harmless (just draining the
        // signal). 8-byte buffer accommodates both eventfd (8 bytes) and pipe.
        let mut buf = [0u8; 8];
        unsafe {
            libc::read(self.read_fd, buf.as_mut_ptr() as *mut _, buf.len());
        }
    }

    pub fn fd(&self) -> RawFd {
        self.read_fd
    }
}

impl Drop for Clock {
    fn drop(&mut self) {
        // SAFETY: We own these fds exclusively and close each exactly once.
        // For eventfd, read_fd == write_fd so we close once. For pipe, they
        // differ so we close both ends.
        unsafe {
            libc::close(self.read_fd);
            if self.write_fd != self.read_fd {
                libc::close(self.write_fd);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// InternalChannel — SPSC ring buffer + clock, used for DAG channels
// ---------------------------------------------------------------------------

pub struct RingBuffer {
    head: AtomicU32,
    tail: AtomicU32,
    buf: Vec<u8>,
    capacity: u32,
}

impl RingBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            head: AtomicU32::new(0),
            tail: AtomicU32::new(0),
            buf: vec![0u8; capacity],
            capacity: capacity as u32,
        }
    }

    /// Write a length-prefixed message. Returns false if not enough space.
    pub fn push(&self, data: &[u8]) -> bool {
        let len = data.len() as u32;
        let total = 4 + len; // u32 length prefix + data
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);

        let used = head.wrapping_sub(tail);
        let free = self.capacity - used;
        if total > free {
            return false;
        }

        // SAFETY (all writes below): This is a single-producer ring buffer.
        // Only one thread ever calls push(), so no concurrent writes occur.
        // The reader only advances `tail` *after* reading, and we only read
        // `tail` above with Acquire ordering, so we never overwrite unread
        // data. The cast from *const to *mut is sound because Vec owns the
        // backing allocation and no other reference exists during writes.

        // Write length
        let len_bytes = len.to_le_bytes();
        for (i, &b) in len_bytes.iter().enumerate() {
            let idx = ((head + i as u32) % self.capacity) as usize;
            unsafe {
                let ptr = self.buf.as_ptr() as *mut u8;
                *ptr.add(idx) = b;
            }
        }
        // Write data
        for (i, &b) in data.iter().enumerate() {
            let idx = ((head + 4 + i as u32) % self.capacity) as usize;
            unsafe {
                let ptr = self.buf.as_ptr() as *mut u8;
                *ptr.add(idx) = b;
            }
        }

        self.head.store(head.wrapping_add(total), Ordering::Release);
        true
    }

    /// Read a length-prefixed message. Returns None if empty.
    pub fn pop(&self) -> Option<Vec<u8>> {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);

        if tail == head {
            return None;
        }

        // Read length
        let mut len_bytes = [0u8; 4];
        for (i, b) in len_bytes.iter_mut().enumerate() {
            let idx = ((tail + i as u32) % self.capacity) as usize;
            *b = self.buf[idx];
        }
        let len = u32::from_le_bytes(len_bytes);

        // Read data
        let mut data = vec![0u8; len as usize];
        for (i, b) in data.iter_mut().enumerate() {
            let idx = ((tail + 4 + i as u32) % self.capacity) as usize;
            *b = self.buf[idx];
        }

        self.tail
            .store(tail.wrapping_add(4 + len), Ordering::Release);
        Some(data)
    }
}

/// A unidirectional channel: ring buffer + clock signal.
pub struct InternalChannel {
    ring: Arc<RingBuffer>,
    clock: Arc<Clock>,
}

impl InternalChannel {
    pub fn new(capacity: usize) -> io::Result<Self> {
        Ok(Self {
            ring: Arc::new(RingBuffer::new(capacity)),
            clock: Arc::new(Clock::new()?),
        })
    }

    pub fn writer(&self) -> ChannelWriter {
        ChannelWriter {
            ring: self.ring.clone(),
            clock: self.clock.clone(),
        }
    }

    pub fn reader(&self) -> ChannelReader {
        ChannelReader {
            ring: self.ring.clone(),
            clock: self.clock.clone(),
        }
    }
}

pub struct ChannelWriter {
    ring: Arc<RingBuffer>,
    clock: Arc<Clock>,
}

impl ChannelWriter {
    /// Send data through the channel. Returns true if sent, false if the
    /// ring buffer was full after bounded retry with exponential backoff.
    pub fn send(&self, data: &str) -> bool {
        for attempt in 0..20u32 {
            if self.ring.push(data.as_bytes()) {
                self.clock.signal();
                return true;
            }
            // Exponential backoff: 1µs, 2µs, 4µs, ... capped at 1024µs (~1ms)
            std::thread::sleep(std::time::Duration::from_micros(1 << attempt.min(10)));
        }
        tracing::warn!(bytes = data.len(), "channel full, message dropped");
        false
    }
}

// SAFETY: ChannelWriter only holds Arc<RingBuffer> (atomic head/tail) and
// Arc<Clock> (fd-based signaling). The SPSC contract guarantees only one
// writer exists, so Send is safe. Sync is safe because send() could be
// serialized externally, and the atomics handle visibility.
unsafe impl Send for ChannelWriter {}
unsafe impl Sync for ChannelWriter {}

pub struct ChannelReader {
    ring: Arc<RingBuffer>,
    clock: Arc<Clock>,
}

impl ChannelReader {
    pub fn recv(&self) -> Option<String> {
        self.ring
            .pop()
            .map(|data| String::from_utf8_lossy(&data).into_owned())
    }

    pub fn clock_fd(&self) -> RawFd {
        self.clock.fd()
    }

    pub fn consume_clock(&self) {
        self.clock.consume();
    }
}

// SAFETY: Same reasoning as ChannelWriter — Arc<RingBuffer> + Arc<Clock>.
// The SPSC contract guarantees only one reader exists.
unsafe impl Send for ChannelReader {}
unsafe impl Sync for ChannelReader {}

// ---------------------------------------------------------------------------
// PipeIn / PipeOut — stdin/stdout transport for CLI mode
// ---------------------------------------------------------------------------

pub struct PipeIn {
    reader: io::BufReader<io::Stdin>,
}

impl Default for PipeIn {
    fn default() -> Self {
        Self::new()
    }
}

impl PipeIn {
    pub fn new() -> Self {
        Self {
            reader: io::BufReader::new(io::stdin()),
        }
    }
}

impl AntennaIn for PipeIn {
    fn recv(&mut self) -> Option<String> {
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => None, // EOF
            Ok(_) => {
                let trimmed = line.trim().to_string();
                if trimmed.is_empty() {
                    Some(String::new()) // empty line, caller will skip
                } else {
                    Some(trimmed)
                }
            }
            Err(_) => None,
        }
    }

    fn clock_fd(&self) -> Option<RawFd> {
        Some(0) // stdin fd
    }
}

pub struct PipeOut;

impl Default for PipeOut {
    fn default() -> Self {
        Self::new()
    }
}

impl PipeOut {
    pub fn new() -> Self {
        Self
    }
}

impl AntennaOut for PipeOut {
    fn send(&mut self, turtle: &str) {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        let _ = writeln!(out, "{}", turtle);
        let _ = out.flush();
    }
}

// ---------------------------------------------------------------------------
// ChannelPair — for FFI: two InternalChannels cross-connected
// ---------------------------------------------------------------------------

pub struct ChannelPair {
    /// App writes here → Antenna reads (Antenna's IN)
    pub app_to_ant: InternalChannel,
    /// Antenna writes here → App reads (Antenna's OUT)
    pub ant_to_app: InternalChannel,
}

impl ChannelPair {
    pub fn new(capacity: usize) -> io::Result<Self> {
        Ok(Self {
            app_to_ant: InternalChannel::new(capacity)?,
            ant_to_app: InternalChannel::new(capacity)?,
        })
    }
}

/// Adapter: reads from the app_to_ant channel (Antenna's IN side)
pub struct ChannelIn {
    reader: ChannelReader,
}

impl ChannelIn {
    pub fn new(reader: ChannelReader) -> Self {
        Self { reader }
    }
}

impl AntennaIn for ChannelIn {
    fn recv(&mut self) -> Option<String> {
        self.reader.recv()
    }

    fn clock_fd(&self) -> Option<RawFd> {
        Some(self.reader.clock_fd())
    }
}

/// Adapter: writes to the ant_to_app channel (Antenna's OUT side)
pub struct ChannelOut {
    writer: ChannelWriter,
}

impl ChannelOut {
    pub fn new(writer: ChannelWriter) -> Self {
        Self { writer }
    }
}

impl AntennaOut for ChannelOut {
    fn send(&mut self, turtle: &str) {
        let _ = self.writer.send(turtle);
    }
}
