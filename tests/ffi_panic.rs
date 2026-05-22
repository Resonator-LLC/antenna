// Copyright (c) 2026-2027 Resonator LLC. Licensed under MIT.

//! Panic-catch + poison test for the C ABI shim.
//!
//! Drives the FFI worker into a panic via the debug-only
//! `[] a antenna:DebugPanic .` Turtle blob, then asserts:
//!   1. An `antenna:Error` Turtle blob reaches the OUT ring before the
//!      worker exits.
//!   2. A follow-up `antenna_send` returns the poisoned-handle code (-4).
//!
//! Requires the `debug-panic` feature, which is included in the crate's
//! default feature set — `cargo test --release ffi_panic` picks it up
//! automatically. Station's plugin disables the feature via
//! `--no-default-features --features ffi-embed`, so the panic-injection
//! check is compiled out entirely there.

#![cfg(feature = "debug-panic")]

use std::ffi::{c_char, c_int, c_void, CString};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use antenna::ffi::{
    antenna_create, antenna_destroy, antenna_drain, antenna_free, antenna_send, AntennaHandle,
};

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

fn drain_until<F>(handle: *mut AntennaHandle, sink: &Sink, timeout: Duration, predicate: F) -> String
where
    F: Fn(&str) -> bool,
{
    let start = Instant::now();
    loop {
        // SAFETY: handle came from antenna_create and is still live.
        let rc: c_int = unsafe {
            antenna_drain(handle, Some(collect_cb), sink as *const Sink as *mut c_void)
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
fn ffi_debug_panic_emits_error_and_poisons_handle() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {});

    let data_dir = unique_dir("antenna-ffi-panic-data");
    let store_dir = unique_dir("antenna-ffi-panic-store");

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

    // Inject the debug-panic Turtle blob. The DebugPanicIn wrapper panics
    // when this line reaches the worker, the catch_unwind block grabs the
    // payload, formats an antenna:Error blob, pushes it onto OUT, and
    // poisons the handle.
    let bomb = br#"@prefix antenna: <http://resonator.network/v2/antenna#> .
[] a antenna:DebugPanic ."#;
    // SAFETY: handle is live; bomb points at a readable buffer.
    let send_rc = unsafe { antenna_send(handle, bomb.as_ptr() as *const c_char, bomb.len()) };
    assert_eq!(send_rc, 0, "antenna_send (panic blob) returned {send_rc}");

    let sink: Sink = Mutex::new(Vec::new());
    let err = drain_until(handle, &sink, Duration::from_secs(15), |s| {
        s.contains("antenna#Error") || s.contains("antenna:Error")
    });
    assert!(
        err.contains("antenna:DebugPanic injected") || err.contains("DebugPanic"),
        "error blob should carry the panic payload, got: {err}",
    );

    // A subsequent send must return the poisoned-handle code (-4). Poll
    // briefly because the worker's poisoned-store happens on a different
    // thread than antenna_send and may race the first attempt.
    let probe = b"# no-op turtle comment\n";
    let deadline = Instant::now() + Duration::from_secs(5);
    let last_rc = loop {
        // SAFETY: handle is still allocated; probe is a readable byte slice.
        let rc = unsafe { antenna_send(handle, probe.as_ptr() as *const c_char, probe.len()) };
        if rc == -4 || Instant::now() >= deadline {
            break rc;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(
        last_rc, -4,
        "post-panic antenna_send must return -4 (poisoned), got {last_rc}",
    );

    if !out_account_id.is_null() {
        // SAFETY: out_account_id came from antenna_create's CString::into_raw.
        unsafe {
            antenna_free(out_account_id as *mut c_void);
        }
    }
    // SAFETY: handle came from antenna_create; antenna_destroy joins the
    // (already-exited) worker and drops the box. Safe to call on a poisoned
    // handle.
    unsafe {
        antenna_destroy(handle);
    }

    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&store_dir);
}
