// Copyright (c) 2026-2027 Resonator LLC. Licensed under MIT.

//! Smoke test for the C ABI shim (antenna/src/ffi.rs, antenna/include/antenna.h).
//!
//! Opens an embedded antenna in a temp dir, pushes a SPIN ASK Turtle through
//! `antenna_send`, polls `antenna_drain` until the `sp:AskResult` comes back,
//! then tears down. Exercises the full lifecycle including worker spawn,
//! libjami account mint, tick loop, ring-buffer round-trip, and clean shutdown.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use antenna::ffi::{
    antenna_clock_fd, antenna_create, antenna_destroy, antenna_drain, antenna_free, antenna_send,
    AntennaHandle,
};

/// Heap-allocated drain sink — receives a copy of every Turtle doc the
/// callback sees. Wrapped in a Mutex so we can pass `&Mutex` as the C user
/// pointer and unlock it safely inside the callback.
type Sink = Mutex<Vec<String>>;

extern "C" fn collect_cb(user: *mut c_void, turtle: *const c_char, len: usize) {
    if user.is_null() || turtle.is_null() || len == 0 {
        return;
    }
    // SAFETY: user was set to a &Sink by the test driver below and outlives
    // the drain call; turtle..turtle+len is a valid UTF-8 slice owned by
    // antenna for the duration of this callback.
    let sink: &Sink = unsafe { &*(user as *const Sink) };
    let bytes = unsafe { std::slice::from_raw_parts(turtle as *const u8, len) };
    if let Ok(s) = std::str::from_utf8(bytes) {
        if let Ok(mut guard) = sink.lock() {
            guard.push(s.to_string());
        }
    }
}

fn unique_dir(prefix: &str) -> String {
    let pid = std::process::id();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = format!("{}/{}-{}-{}", std::env::temp_dir().display(), prefix, pid, ts);
    std::fs::create_dir_all(&path).expect("create temp dir");
    path
}

/// Drive `antenna_drain` until `predicate` finds a match or the timeout
/// elapses. Returns the matched doc, or panics with the full drained log.
fn drain_until<F>(handle: *mut AntennaHandle, sink: &Sink, timeout: Duration, predicate: F) -> String
where
    F: Fn(&str) -> bool,
{
    let start = Instant::now();
    loop {
        // SAFETY: handle came from antenna_create and is still live.
        let rc: c_int = unsafe {
            antenna_drain(
                handle,
                Some(collect_cb),
                sink as *const Sink as *mut c_void,
            )
        };
        assert!(rc >= 0, "antenna_drain returned {rc}");

        if let Ok(guard) = sink.lock() {
            if let Some(found) = guard.iter().find(|s| predicate(s)).cloned() {
                return found;
            }
        }

        if start.elapsed() > timeout {
            let dump = sink.lock().map(|g| g.join("\n")).unwrap_or_default();
            panic!(
                "drain_until timed out after {:?}; collected:\n{}",
                timeout, dump
            );
        }

        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn ffi_create_send_drain_destroy_roundtrip() {
    // tracing is opt-in here — the FFI shim already logs via tracing macros
    // and the test still works without a subscriber installed.
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {});

    let data_dir = unique_dir("antenna-ffi-smoke-data");
    let store_dir = unique_dir("antenna-ffi-smoke-store");

    let data_dir_c = CString::new(data_dir.clone()).unwrap();
    let store_dir_c = CString::new(store_dir.clone()).unwrap();

    let mut out_account_id: *mut c_char = std::ptr::null_mut();

    // SAFETY: all string pointers are valid for the call; out_account_id is a
    // writable stack slot.
    let handle = unsafe {
        antenna_create(
            data_dir_c.as_ptr(),
            std::ptr::null(),
            store_dir_c.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            &mut out_account_id as *mut *mut c_char,
        )
    };
    assert!(!handle.is_null(), "antenna_create returned NULL");
    assert!(!out_account_id.is_null(), "out_account_id was not populated");

    // SAFETY: out_account_id is a NUL-terminated string we own until we hand
    // it back to antenna_free.
    let account_id = unsafe { CStr::from_ptr(out_account_id) }
        .to_str()
        .expect("account id is valid UTF-8")
        .to_string();
    assert!(!account_id.is_empty(), "minted account id should be non-empty");

    // Clock fd is best-effort on macOS (pipe-based) but must be non-negative.
    // SAFETY: handle is live.
    let clock_fd = unsafe { antenna_clock_fd(handle) };
    assert!(clock_fd >= 0, "antenna_clock_fd returned {clock_fd}");

    // Push a SPIN ASK against `{ ?s ?p ?o }`. The design bundle is loaded at
    // boot so this is guaranteed to be true on a fresh handle.
    let ask = br#"@prefix sp: <http://spinrdf.org/sp#> .
[] a sp:Ask ; sp:text "ASK { ?s ?p ?o }" ."#;
    // SAFETY: handle is live; ask points at a valid byte range for ask.len().
    let send_rc = unsafe { antenna_send(handle, ask.as_ptr() as *const c_char, ask.len()) };
    assert_eq!(send_rc, 0, "antenna_send returned {send_rc}");

    let sink: Sink = Mutex::new(Vec::new());
    let hit = drain_until(handle, &sink, Duration::from_secs(15), |s| {
        s.contains("AskResult") && s.contains("true")
    });
    assert!(
        hit.contains("sp:AskResult") || hit.contains("AskResult"),
        "expected sp:AskResult, got: {hit}",
    );

    // SAFETY: ptr was returned by antenna_create; antenna_free is the
    // matching deallocator.
    unsafe {
        antenna_free(out_account_id as *mut c_void);
    }
    // SAFETY: handle came from antenna_create; antenna_destroy joins the
    // worker thread and drops the box.
    unsafe {
        antenna_destroy(handle);
    }

    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&store_dir);
}
