// Copyright (c) 2026-2027 Resonator LLC. Licensed under MIT.

//! Integration tests for ISSUE-123 Cut B — the tri-state `antenna_create`,
//! the asynchronous `antenna_account_id` getter, and the archive
//! export/import round-trip via `carrier:ExportAccount` / `ImportAccount`.
//!
//! Each `#[test]` runs in its own process (per cargo's test-binary model)
//! so the libjami singleton and `LIVE_HANDLE` registry start clean every
//! time. The wait_then_create and round_trip scenarios live in separate
//! files to enforce that — they would otherwise stomp on each other's
//! global state.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use antenna::ffi::{
    antenna_account_id, antenna_clock_fd, antenna_create, antenna_destroy, antenna_drain,
    antenna_free, antenna_send, AntennaHandle,
};

type Sink = Mutex<Vec<String>>;

extern "C" fn collect_cb(user: *mut c_void, turtle: *const c_char, len: usize) {
    if user.is_null() || turtle.is_null() || len == 0 {
        return;
    }
    // SAFETY: user is a `&Sink` borrowed for the duration of antenna_drain;
    // turtle..turtle+len is a valid UTF-8 slice for this callback.
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

/// Poll antenna_drain until either `predicate` matches a doc or the timeout
/// elapses. On timeout, dump everything seen so a CI failure is debuggable.
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
            panic!("drain_until timed out after {timeout:?}; collected:\n{dump}");
        }

        std::thread::sleep(Duration::from_millis(10));
    }
}

fn read_account_id(handle: *mut AntennaHandle) -> String {
    let mut buf = [0u8; 96];
    // SAFETY: handle is live; buf is writable for buf.len() bytes.
    let n = unsafe { antenna_account_id(handle, buf.as_mut_ptr() as *mut c_char, buf.len()) };
    String::from_utf8(buf[..n].to_vec()).unwrap_or_default()
}

#[test]
fn wait_then_create_emits_onboarding_and_populates_account_id() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {});

    let data_dir = unique_dir("antenna-ffi-onboarding-data");
    let store_dir = unique_dir("antenna-ffi-onboarding-store");

    let data_dir_c = CString::new(data_dir.clone()).unwrap();
    let store_dir_c = CString::new(store_dir.clone()).unwrap();
    // Empty string sentinel triggers the onboarding-wait branch (decision #4).
    let empty_account = CString::new("").unwrap();
    let mut out_account_id: *mut c_char = std::ptr::null_mut();

    // SAFETY: all string pointers are valid; out_account_id is writable.
    let handle = unsafe {
        antenna_create(
            data_dir_c.as_ptr(),
            empty_account.as_ptr(),
            store_dir_c.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            &mut out_account_id as *mut *mut c_char,
        )
    };
    assert!(!handle.is_null(), "antenna_create returned NULL on empty-account path");

    // out_account_id is populated (possibly empty string) per the new contract.
    assert!(!out_account_id.is_null(), "out_account_id should be set even on onboarding path");
    // SAFETY: out_account_id is a NUL-terminated string we own.
    let initial = unsafe { CStr::from_ptr(out_account_id) }
        .to_str()
        .expect("account id is valid UTF-8")
        .to_string();
    assert!(
        initial.is_empty(),
        "onboarding-wait must hand back an empty id, got {initial:?}",
    );

    // SAFETY: handle is live.
    let clock_fd = unsafe { antenna_clock_fd(handle) };
    assert!(clock_fd >= 0, "antenna_clock_fd returned {clock_fd}");

    let sink: Sink = Mutex::new(Vec::new());

    // Phase 1: assert OnboardingRequired surfaces on the first tick(s) AND
    // no carrier:Connected fires during the first second (because no
    // account exists yet).
    drain_until(handle, &sink, Duration::from_secs(2), |s| {
        s.contains("antenna:OnboardingRequired") && s.contains("no-account")
    });
    let before_create: Vec<String> = sink.lock().unwrap().clone();
    assert!(
        !before_create.iter().any(|s| s.contains("carrier:Connected")),
        "carrier:Connected should not fire before CreateAccount; got:\n{}",
        before_create.join("\n"),
    );
    // antenna_account_id should still be empty until AccountReady arrives.
    assert!(
        read_account_id(handle).is_empty(),
        "account_id must stay empty until AccountReady on the wait path",
    );

    // Phase 2: push CreateAccount and wait for AccountReady.
    let create = br#"[] a carrier:CreateAccount ; carrier:displayName "alice" ."#;
    // SAFETY: handle is live; create is a valid byte slice.
    let send_rc =
        unsafe { antenna_send(handle, create.as_ptr() as *const c_char, create.len()) };
    assert_eq!(send_rc, 0, "antenna_send(CreateAccount) returned {send_rc}");

    let ready = drain_until(handle, &sink, Duration::from_secs(45), |s| {
        s.contains("carrier:AccountReady")
    });
    assert!(
        ready.contains("carrier:account"),
        "AccountReady must carry carrier:account; got:\n{ready}",
    );

    // Phase 3: antenna_account_id must now return the minted id.
    let id_after = read_account_id(handle);
    assert!(
        !id_after.is_empty(),
        "antenna_account_id should populate after AccountReady",
    );

    // SAFETY: pointers came from antenna_create / matching deallocator.
    unsafe {
        antenna_free(out_account_id as *mut c_void);
        antenna_destroy(handle);
    }
    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&store_dir);
}
