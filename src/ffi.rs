// Copyright (c) 2026-2027 Resonator LLC. Licensed under MIT.

//! C ABI for embedding Antenna in-process.
//!
//! Surface mirrors the existing transport contract: Turtle bytes in, Turtle
//! bytes out, dispatched by `rdf:type`. A worker thread owns the
//! `AntennaContext` and drives `tick()` between clock-fd-blocked waits.
//! Callers push input via [`antenna_send`] and pop emissions via
//! [`antenna_drain`].
//!
//! See `include/antenna.h` for the canonical C declarations consumed by Dart
//! bindings.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::os::fd::RawFd;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::channel::{
    AntennaIn, AntennaOut, ChannelIn, ChannelOut, ChannelReader, ChannelWriter, InternalChannel,
};
use crate::AntennaContext;

/// Per-direction ring-buffer capacity (bytes). Sized for the design bundle +
/// full pipeline replacements without forcing the caller to drain mid-burst.
const FFI_RING_BYTES: usize = 1 << 20;

/// Worker tick cap (ms). Matches `AntennaContext::run`'s clamp so a quiet
/// swarm doesn't park script emits behind libjami's idle interval.
const FFI_MAX_SLEEP_MS: i32 = 25;

/// Process-global live handle. Carrier's `g_carrier` mutex makes libjami a
/// per-process singleton, and libjami can't be cleanly re-initialised within
/// one process (`libjami::fini` followed by `libjami::init` leaves new
/// accounts stuck out of REGISTERED state). So on Flutter hot-restart —
/// where the Dart isolate dies without calling `antenna_destroy` — we
/// rebind the orphaned handle to the new isolate instead of recreating
/// (ISSUE-122). The fresh isolate gets back the same `*mut AntennaHandle`
/// and the same IN/OUT rings; the worker keeps spinning across the
/// restart. Stored as `usize` because `*mut AntennaHandle` is not `Send`.
/// `antenna_destroy` clears the slot when it matches, so a normal
/// create/destroy cycle still tears down libjami at the end.
static LIVE_HANDLE: Mutex<Option<usize>> = Mutex::new(None);

/// Opaque handle returned to C. The layout is private to Rust; the embedding
/// app only sees `*mut AntennaHandle`.
pub struct AntennaHandle {
    in_writer: ChannelWriter,
    out_reader: ChannelReader,
    out_clock_fd: RawFd,
    done: Arc<AtomicBool>,
    /// Set by the worker thread after a panic in `tick()`. Once true, the
    /// worker has exited; `antenna_send` rejects further input and
    /// `antenna_drain` still delivers any queued docs (including the
    /// `antenna:Error` Turtle blob the worker pushed before exiting).
    /// Self-healing is left to the caller via `antenna_destroy` +
    /// `antenna_create`.
    poisoned: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    /// Active Jami account id, shared with the worker's `AntennaContext`.
    /// Populated synchronously on the cold-boot mint / load paths and
    /// asynchronously when `carrier:AccountReady` fires on the
    /// onboarding-wait path. `antenna_account_id` reads through this so a
    /// caller that booted with `account_id_or_null = ""` can poll for the
    /// late update. The rebind path (Flutter hot-restart) clones the
    /// `Arc`, so the new isolate observes the same id without any sync
    /// step on its side.
    account_id: Arc<Mutex<String>>,
}

/// Callback signature for [`antenna_drain`]. The `(turtle, len)` pair points
/// at a non-NUL-terminated UTF-8 buffer owned by Antenna and only valid for
/// the duration of the call; the callee MUST NOT retain the pointer.
pub type AntennaEmitCb =
    Option<unsafe extern "C" fn(user: *mut c_void, turtle: *const c_char, len: usize)>;

/// Create an embedded Antenna instance.
///
/// On success returns a non-null `*mut AntennaHandle`; on failure returns
/// null. If `out_account_id` is non-null on success, `*out_account_id` is
/// populated with a heap-allocated, NUL-terminated UTF-8 string holding the
/// active account ID — the caller must release it with [`antenna_free`].
///
/// # Safety
/// * `data_dir` must be a valid NUL-terminated UTF-8 string.
/// * Each `*_or_null` argument is either null or a valid NUL-terminated
///   UTF-8 string for the duration of this call.
/// * `out_account_id`, if non-null, must point at writable storage for a
///   single `*mut c_char`.
#[no_mangle]
pub unsafe extern "C" fn antenna_create(
    data_dir: *const c_char,
    account_id_or_null: *const c_char,
    store_dir_or_null: *const c_char,
    pipeline_ttl_or_null: *const c_char,
    seed_ttl_or_null: *const c_char,
    out_account_id: *mut *mut c_char,
) -> *mut AntennaHandle {
    // Install the canonical tracing subscriber on first antenna_create in this
    // process. Without it every tracing::* call in the embedded path is a
    // silent no-op (Cut 8.10 — diagnosis gap surfaced by ISSUE-107). init()
    // swallows double-init via try_init().ok(), so reusing the same process
    // for a destroy/recreate cycle is safe.
    let _ = crate::logging::init("debug", "");

    // Rebind path for Flutter hot-restart (ISSUE-122): the Dart isolate
    // dies without calling antenna_destroy, so the previous AntennaHandle
    // is still live in this process. libjami can't be cleanly re-inited
    // inside a single process (carrier_jami.cc's g_carrier singleton, and
    // accounts don't re-register after libjami::fini/init), so we hand
    // the new isolate back the same handle and let it keep using the
    // already-running worker, IN/OUT rings, store, and account.
    //
    // The rebind also republishes the account id through out_account_id so
    // the caller sees the same contract a cold boot would.
    {
        let live = LIVE_HANDLE.lock().expect("LIVE_HANDLE poisoned");
        if let Some(prev) = *live {
            let prev_ptr = prev as *mut AntennaHandle;
            tracing::warn!(
                target: "FFI",
                "antenna_create: prior handle still live (likely Flutter hot-restart); rebinding"
            );
            if !out_account_id.is_null() {
                // SAFETY: `prev` was produced by Box::into_raw in a prior
                // call to this function and stored in LIVE_HANDLE. It has
                // not been freed (antenna_destroy clears the slot before
                // freeing), so the pointer is still valid here.
                let acct = unsafe {
                    (*prev_ptr)
                        .account_id
                        .lock()
                        .expect("account_id poisoned")
                        .clone()
                };
                if let Ok(c_id) = CString::new(acct) {
                    // SAFETY: caller contract — writable storage for one
                    // *mut c_char.
                    unsafe {
                        *out_account_id = c_id.into_raw();
                    }
                }
            }
            return prev_ptr;
        }
    }

    let result = catch_unwind(AssertUnwindSafe(|| {
        tracing::info!(target: "FFI", "antenna_create: entered");
        // SAFETY: caller contract — each non-null pointer is a NUL-terminated
        // UTF-8 buffer valid for this call.
        let data_dir = unsafe { opt_cstr(data_dir) }?;
        let data_dir = data_dir.ok_or(())?;
        let account_id = unsafe { opt_cstr(account_id_or_null) }?;
        let store_dir = unsafe { opt_cstr(store_dir_or_null) }?;
        let pipeline_ttl = unsafe { opt_cstr(pipeline_ttl_or_null) }?;
        let seed_ttl = unsafe { opt_cstr(seed_ttl_or_null) }?;

        let ctx =
            AntennaContext::new_with_ttl(data_dir, account_id, store_dir, pipeline_ttl, seed_ttl)
                .map_err(|e| {
                tracing::error!(target: "FFI", %e, "antenna_create: context init failed");
            })?;

        let pair_in = InternalChannel::new(FFI_RING_BYTES).map_err(|e| {
            tracing::error!(target: "FFI", %e, "antenna_create: IN channel alloc failed");
        })?;
        let pair_out = InternalChannel::new(FFI_RING_BYTES).map_err(|e| {
            tracing::error!(target: "FFI", %e, "antenna_create: OUT channel alloc failed");
        })?;

        let in_writer = pair_in.writer();
        let out_reader = pair_out.reader();
        let out_clock_fd = out_reader.clock_fd();

        let ant_in = ChannelIn::new(pair_in.reader());
        let ant_out = ChannelOut::new(pair_out.writer());

        let done = Arc::new(AtomicBool::new(false));
        let done_w = done.clone();
        let poisoned = Arc::new(AtomicBool::new(false));
        let poisoned_w = poisoned.clone();
        // Both the handle and the worker observe the same Arc<Mutex<String>>
        // so a late `AccountReady` update inside `tick()` is visible to
        // `antenna_account_id` without a sync step.
        let account_handle = ctx.account_id.clone();
        let account_for_caller = account_handle.lock().expect("account_id poisoned").clone();

        let worker = thread::Builder::new()
            .name("antenna-ffi-worker".to_string())
            .spawn(move || {
                let mut ctx = ctx;
                let mut ant_in = DebugPanicIn { inner: ant_in };
                let mut ant_out = ant_out;
                while !done_w.load(Ordering::Acquire) {
                    let interval_ms = ctx.interval().as_millis() as i32;
                    let timeout_ms = interval_ms.clamp(1, FFI_MAX_SLEEP_MS);

                    if let Some(clock_fd) = ant_in.clock_fd() {
                        let mut pfd = libc::pollfd {
                            fd: clock_fd,
                            events: libc::POLLIN,
                            revents: 0,
                        };
                        // SAFETY: pfd is a stack-allocated valid pollfd;
                        // clock_fd is owned by ant_in and lives as long as
                        // this worker thread.
                        unsafe {
                            libc::poll(&mut pfd, 1, timeout_ms);
                        }
                    } else {
                        thread::sleep(Duration::from_millis(timeout_ms as u64));
                    }

                    // Catch panics in tick() so a single misbehaving script
                    // node or libjami callback doesn't take down Station.
                    // The error blob lets the embedding app surface a
                    // crash banner; the worker exits and the handle stays
                    // poisoned until the caller recycles it.
                    let tick_result = catch_unwind(AssertUnwindSafe(|| {
                        ctx.tick(&mut ant_in, &mut ant_out)
                    }));
                    match tick_result {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => {
                            tracing::error!(target: "FFI", %e, "tick failed");
                        }
                        Err(payload) => {
                            let msg = panic_payload_message(&payload);
                            let turtle = format!(
                                "[] a <http://resonator.network/v2/antenna#Error> ; \
                                 <http://resonator.network/v2/antenna#message> \"{}\" .",
                                escape_turtle_string(&msg)
                            );
                            ant_out.send(&turtle);
                            poisoned_w.store(true, Ordering::Release);
                            tracing::error!(target: "FFI", message = %msg, "worker panicked; handle poisoned");
                            break;
                        }
                    }
                }
            })
            .map_err(|e| {
                tracing::error!(target: "FFI", %e, "worker spawn failed");
            })?;

        if !out_account_id.is_null() {
            if let Ok(c_id) = CString::new(account_for_caller) {
                // SAFETY: caller contract guarantees out_account_id points at
                // writable storage for one *mut c_char.
                unsafe {
                    *out_account_id = c_id.into_raw();
                }
            }
        }

        Ok::<AntennaHandle, ()>(AntennaHandle {
            in_writer,
            out_reader,
            out_clock_fd,
            done,
            poisoned,
            worker: Some(worker),
            account_id: account_handle,
        })
    }));

    match result {
        Ok(Ok(handle)) => {
            let raw = Box::into_raw(Box::new(handle));
            *LIVE_HANDLE.lock().expect("LIVE_HANDLE poisoned") = Some(raw as usize);
            raw
        }
        Ok(Err(())) => std::ptr::null_mut(),
        Err(_) => {
            tracing::error!(target: "FFI", "antenna_create panicked");
            std::ptr::null_mut()
        }
    }
}

/// Split `input` on newlines and push each non-empty trimmed line through
/// `push`. Mirrors the WS server's line framing (ws.rs): dispatch() reads
/// one `rdf:type` per call, so a multi-statement payload (e.g. the theme
/// picker's `sp:Modify` + `design:ResolveActiveTheme` pair) must arrive as
/// separate ring entries or only the first statement runs.
///
/// Returns 0 if every line pushed cleanly, -3 if the ring filled mid-batch.
fn push_turtle_lines<F: FnMut(&str) -> bool>(input: &str, mut push: F) -> c_int {
    for line in input.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !push(trimmed) {
            return -3;
        }
    }
    0
}

/// Push one Turtle document (or batch of newline-separated statements) onto
/// the worker's IN ring. Newlines split into separate dispatches.
///
/// Returns 0 on success and a negative code on failure:
/// * `-1` — invalid arguments (null handle, or null pointer with non-zero len)
/// * `-2` — bytes are not valid UTF-8
/// * `-3` — ring buffer full after bounded retry
/// * `-4` — handle poisoned (worker panicked; caller must recycle via
///   `antenna_destroy` + `antenna_create`)
///
/// # Safety
/// * `handle` must have been returned by [`antenna_create`] and not yet
///   passed to [`antenna_destroy`].
/// * `turtle` must point at `len` valid bytes, or be null when `len == 0`.
#[no_mangle]
pub unsafe extern "C" fn antenna_send(
    handle: *mut AntennaHandle,
    turtle: *const c_char,
    len: usize,
) -> c_int {
    if handle.is_null() || (turtle.is_null() && len > 0) {
        return -1;
    }
    let result = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: caller contract — handle is live; turtle/len describe a
        // readable buffer (or len == 0 when turtle is null).
        let h = unsafe { &*handle };
        if h.poisoned.load(Ordering::Acquire) {
            return -4;
        }
        let slice: &[u8] = if len == 0 {
            &[]
        } else {
            // SAFETY: caller contract — turtle..turtle+len is readable for
            // the duration of this call.
            unsafe { std::slice::from_raw_parts(turtle as *const u8, len) }
        };
        let s = match std::str::from_utf8(slice) {
            Ok(s) => s,
            Err(_) => return -2,
        };
        push_turtle_lines(s, |line| h.in_writer.send(line))
    }));
    result.unwrap_or(-1)
}

/// Drain whatever Turtle docs are queued on the OUT ring, invoking `cb` once
/// per doc. Returns the number of docs delivered, or `-1` on bad arguments.
///
/// The clock fd is consumed before draining so callers blocked on it via
/// poll/select can re-arm cleanly. When `cb` is null, returns 0 without
/// draining.
///
/// # Safety
/// * `handle` must be live (see [`antenna_send`]).
/// * `cb`, when non-null, must remain valid for the duration of this call.
#[no_mangle]
pub unsafe extern "C" fn antenna_drain(
    handle: *mut AntennaHandle,
    cb: AntennaEmitCb,
    user: *mut c_void,
) -> c_int {
    if handle.is_null() {
        return -1;
    }
    let result = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: caller contract — handle is live.
        let h = unsafe { &*handle };
        h.out_reader.consume_clock();
        let cb = match cb {
            Some(f) => f,
            None => return 0,
        };
        let mut count: c_int = 0;
        while let Some(s) = h.out_reader.recv() {
            // SAFETY: cb was provided by the caller and is valid for this
            // call; the (ptr, len) pair points into an owned String that
            // lives until after cb returns.
            unsafe {
                cb(user, s.as_ptr() as *const c_char, s.len());
            }
            count = count.saturating_add(1);
        }
        count
    }));
    result.unwrap_or(-1)
}

/// Return the read end of the OUT-side clock fd so callers can block in
/// `poll`/`select`/`kqueue` rather than busy-loop on [`antenna_drain`].
/// Returns -1 if the handle is null.
///
/// # Safety
/// `handle` must be live.
#[no_mangle]
pub unsafe extern "C" fn antenna_clock_fd(handle: *mut AntennaHandle) -> c_int {
    if handle.is_null() {
        return -1;
    }
    // SAFETY: caller contract — handle is live.
    let h = unsafe { &*handle };
    h.out_clock_fd
}

/// Copy the current Jami account id (NUL-terminated, UTF-8) into `out_buf`.
/// Returns the number of bytes actually written, excluding the trailing NUL.
///
/// * If `handle` is null, returns 0 and does not touch `out_buf`.
/// * If the active account id is still empty (onboarding-wait path before
///   `carrier:AccountReady` has fired), returns 0 and writes a single NUL
///   byte into `out_buf` when `out_buf_len >= 1`.
/// * If the id does not fit, copies as much as fits including a trailing
///   NUL (i.e. `out_buf_len - 1` id bytes plus the NUL) and still returns
///   the number of id bytes written. Callers should size the buffer at
///   `CARRIER_ACCOUNT_ID_LEN` (64) which is libjami's hard cap.
///
/// # Safety
/// `handle` must be live (see [`antenna_send`]). `out_buf` must be writable
/// for `out_buf_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn antenna_account_id(
    handle: *mut AntennaHandle,
    out_buf: *mut c_char,
    out_buf_len: usize,
) -> usize {
    if handle.is_null() || out_buf.is_null() || out_buf_len == 0 {
        return 0;
    }
    let result = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: caller contract — handle is live.
        let h = unsafe { &*handle };
        let id = h.account_id.lock().expect("account_id poisoned").clone();
        let bytes = id.as_bytes();
        let max_copy = bytes.len().min(out_buf_len - 1);
        // SAFETY: out_buf is writable for at least max_copy + 1 bytes.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), out_buf as *mut u8, max_copy);
            *out_buf.add(max_copy) = 0;
        }
        max_copy
    }));
    result.unwrap_or(0)
}

/// Signal the worker to exit, join it, and release all resources owned by
/// the handle (including libjami via the dropped `AntennaContext`).
///
/// # Safety
/// `handle` must have been returned by [`antenna_create`] and not previously
/// destroyed. Null is a no-op.
#[no_mangle]
pub unsafe extern "C" fn antenna_destroy(handle: *mut AntennaHandle) {
    if handle.is_null() {
        return;
    }
    // Clear the registry slot if this is the tracked live handle. The
    // recycler in antenna_create may have already taken it; either way
    // leaves the slot empty so no later create double-frees this handle.
    {
        let mut g = LIVE_HANDLE.lock().expect("LIVE_HANDLE poisoned");
        if *g == Some(handle as usize) {
            *g = None;
        }
    }
    // SAFETY: caller contract — handle came from Box::into_raw in
    // antenna_create and has not been freed yet.
    let mut boxed = unsafe { Box::from_raw(handle) };
    boxed.done.store(true, Ordering::Release);
    if let Some(worker) = boxed.worker.take() {
        let _ = worker.join();
    }
}

/// Release a pointer previously handed out by an `antenna_*` function — at
/// present only `out_account_id` from [`antenna_create`]. Null is a no-op.
///
/// # Safety
/// `ptr` must be a pointer returned by an antenna function that documented
/// release-with-`antenna_free`, or null. Double-free is undefined behavior.
#[no_mangle]
pub unsafe extern "C" fn antenna_free(ptr: *mut c_void) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: caller contract — ptr originated from CString::into_raw inside
    // antenna_create. CString::from_raw is the matching deallocator.
    unsafe {
        let _ = CString::from_raw(ptr as *mut c_char);
    }
}

// --- helpers ---------------------------------------------------------------

/// Convert an optional NUL-terminated C pointer to an optional borrowed
/// `&str`. Returns `Err(())` only if the bytes are not valid UTF-8.
///
/// # Safety
/// `p` must be null or point at a NUL-terminated UTF-8 string valid for the
/// returned reference's lifetime.
unsafe fn opt_cstr<'a>(p: *const c_char) -> Result<Option<&'a str>, ()> {
    if p.is_null() {
        return Ok(None);
    }
    // SAFETY: caller contract — p is NUL-terminated and lives for 'a.
    match unsafe { CStr::from_ptr(p) }.to_str() {
        Ok(s) => Ok(Some(s)),
        Err(_) => Err(()),
    }
}

/// Pull a readable string out of a `Box<dyn Any>` panic payload. Matches the
/// two payload shapes the standard library produces from `panic!(...)`.
fn panic_payload_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        return (*s).to_string();
    }
    if let Some(s) = payload.downcast_ref::<String>() {
        return s.clone();
    }
    "(non-string panic payload)".to_string()
}

/// Escape a string for use inside a Turtle short (single-quoted) literal.
/// Covers the four characters Turtle 1.1 § 7 requires escaping in
/// STRING_LITERAL_QUOTE: backslash, quote, newline, carriage return.
fn escape_turtle_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

/// `AntennaIn` decorator that watches for the debug panic-injection blob.
///
/// The matching test blob is `[] a antenna:DebugPanic .` — sending it on
/// the IN ring triggers a panic in the worker thread on the next `recv()`,
/// which exercises the catch_unwind + poison path used in production for
/// real panics. The check is feature-gated so Station's embedded staticlib
/// (`--no-default-features --features ffi-embed`) compiles it out entirely;
/// debug builds and default release builds keep it for testability.
struct DebugPanicIn<I: AntennaIn> {
    inner: I,
}

impl<I: AntennaIn> AntennaIn for DebugPanicIn<I> {
    fn recv(&mut self) -> Option<String> {
        let line = self.inner.recv()?;
        #[cfg(feature = "debug-panic")]
        if line.contains("antenna:DebugPanic") {
            panic!("antenna:DebugPanic injected via FFI input");
        }
        Some(line)
    }

    fn clock_fd(&self) -> Option<RawFd> {
        self.inner.clock_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::push_turtle_lines;

    fn collect(input: &str) -> Vec<String> {
        let mut out = Vec::new();
        let rc = push_turtle_lines(input, |line| {
            out.push(line.to_string());
            true
        });
        assert_eq!(rc, 0);
        out
    }

    #[test]
    fn single_statement_passes_through_unchanged() {
        let lines = collect("[] a sp:Ask ; sp:text \"ASK { ?s ?p ?o }\" .");
        assert_eq!(lines, vec!["[] a sp:Ask ; sp:text \"ASK { ?s ?p ?o }\" ."]);
    }

    #[test]
    fn theme_picker_payload_splits_into_two_dispatches() {
        // Exact wire format from station/lib/ui/components/theme_picker.dart's
        // buildThemeSwapTurtle — `sp:Modify` then `design:ResolveActiveTheme`.
        // Pre-fix this fired only `sp:Modify` over FFI (the resolve was
        // silently dropped), so theme swaps committed in the store but never
        // broadcast a new design bundle to Station.
        let wire = "@prefix sp: <http://spinrdf.org/sp#> .\n\
                    @prefix design: <http://resonator.network/v2/design#> .\n\
                    [] a sp:Modify ; sp:text \"DELETE { } INSERT { } WHERE { }\" .\n\
                    [] a design:ResolveActiveTheme .\n";
        let lines = collect(wire);
        assert_eq!(lines.len(), 4);
        assert!(lines[2].contains("sp:Modify"));
        assert!(lines[3].contains("design:ResolveActiveTheme"));
    }

    #[test]
    fn empty_and_whitespace_lines_are_skipped() {
        let lines = collect("\n  \n[] a foo:Bar .\n\n");
        assert_eq!(lines, vec!["[] a foo:Bar ."]);
    }

    #[test]
    fn push_failure_returns_minus_three_and_stops() {
        let mut count = 0;
        let rc = push_turtle_lines("a\nb\nc\n", |_| {
            count += 1;
            count < 2
        });
        assert_eq!(rc, -3);
        assert_eq!(count, 2);
    }
}
