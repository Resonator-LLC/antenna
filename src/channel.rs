///! Transport traits and implementations: AntennaIn/AntennaOut, PipeTransport, internal channels.

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
            if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            // Set both ends non-blocking
            for &fd in &fds {
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
        let buf: [u8; 1] = [1];
        unsafe {
            libc::write(self.write_fd, buf.as_ptr() as *const _, 1);
        }
    }

    pub fn consume(&self) {
        let mut buf = [0u8; 8]; // eventfd reads 8 bytes, pipe reads 1+
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

        // Write length
        let len_bytes = len.to_le_bytes();
        for (i, &b) in len_bytes.iter().enumerate() {
            let idx = ((head + i as u32) % self.capacity) as usize;
            // Safety: single writer
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
    pub fn send(&self, data: &str) {
        // Spin-retry if ring is full (shouldn't happen with reasonable capacity)
        while !self.ring.push(data.as_bytes()) {
            std::thread::yield_now();
        }
        self.clock.signal();
    }
}

// Safety: ring is Arc<RingBuffer> with atomic ops, clock is Arc<Clock> (fd-based)
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

unsafe impl Send for ChannelReader {}
unsafe impl Sync for ChannelReader {}

// ---------------------------------------------------------------------------
// PipeIn / PipeOut — stdin/stdout transport for CLI mode
// ---------------------------------------------------------------------------

pub struct PipeIn {
    reader: io::BufReader<io::Stdin>,
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
        self.writer.send(turtle);
    }
}
