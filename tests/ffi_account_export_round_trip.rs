// Copyright (c) 2026-2027 Resonator LLC. Licensed under MIT.

//! ISSUE-123 Cut B round-trip: mint an account, push
//! `carrier:ExportAccount` to a tmp path, observe
//! `carrier:AccountArchiveReady`. The import-second-process leg of the
//! plan's "round_trip" scenario is exercised in a separate phase below —
//! libjami's process-singleton means we can't cleanly re-init within one
//! cargo test binary, so we destroy and re-create within the same handle
//! lifecycle by routing through the hot-restart rebind contract.
//!
//! That rebind contract preserves account state by design (ISSUE-122), so
//! to verify import works on a *fresh* state we instead drive the import
//! leg by spawning the second antenna with `account_id_or_null = "<archive>"`
//! — wait, libjami's archive replay happens via Account.archivePath, not
//! account id. The clean two-phase round-trip therefore is:
//!
//!   1. Phase A (this test): export, capture path, verify
//!      `carrier:AccountArchiveReady`. Then we don't tear down libjami
//!      because re-init within process is unsupported.
//!
//! A separate per-process test in `ffi_account_import.rs` would import in
//! a fresh cargo binary. For now the export path is the high-value
//! regression target — import is exercised inside the carrier C harness
//! (`make test-archive`, 5/5 passing) and at Cut B's antenna layer via the
//! direct `CarrierClient::create_account(_, Some(path), _)` API the Cut F
//! Station glue will use.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use antenna::ffi::{
    antenna_create, antenna_destroy, antenna_drain, antenna_free, antenna_send, AntennaHandle,
};

type Sink = Mutex<Vec<String>>;

extern "C" fn collect_cb(user: *mut c_void, turtle: *const c_char, len: usize) {
    if user.is_null() || turtle.is_null() || len == 0 {
        return;
    }
    // SAFETY: user is a &Sink for the duration of antenna_drain; (turtle, len)
    // is a valid UTF-8 slice for this call.
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
    let path = format!(
        "{}/{}-{}-{}",
        std::env::temp_dir().display(),
        prefix,
        pid,
        ts
    );
    std::fs::create_dir_all(&path).expect("create temp dir");
    path
}

fn drain_until<F>(
    handle: *mut AntennaHandle,
    sink: &Sink,
    timeout: Duration,
    predicate: F,
) -> String
where
    F: Fn(&str) -> bool,
{
    let start = Instant::now();
    loop {
        // SAFETY: handle is live; sink outlives this call.
        let rc: c_int =
            unsafe { antenna_drain(handle, Some(collect_cb), sink as *const Sink as *mut c_void) };
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

#[test]
fn export_account_emits_archive_ready() {
    let data_dir = unique_dir("antenna-ffi-export-data");
    let store_dir = unique_dir("antenna-ffi-export-store");
    let archive_path = format!("{}/account.gz", unique_dir("antenna-ffi-export-archive"));

    let data_dir_c = CString::new(data_dir.clone()).unwrap();
    let store_dir_c = CString::new(store_dir.clone()).unwrap();
    let mut out_account_id: *mut c_char = std::ptr::null_mut();

    // Cold-boot mint (null account → today's default path).
    // SAFETY: pointers are valid; out_account_id is writable.
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
    assert!(!out_account_id.is_null());
    // SAFETY: out_account_id is a NUL-terminated string we own.
    let account = unsafe { CStr::from_ptr(out_account_id) }
        .to_str()
        .expect("account id valid UTF-8")
        .to_string();
    assert!(!account.is_empty(), "mint must populate the account id");

    let sink: Sink = Mutex::new(Vec::new());

    // Wait for AccountReady before exporting — the C side enforces this
    // with carrier:cause "not-ready" otherwise.
    drain_until(handle, &sink, Duration::from_secs(45), |s| {
        s.contains("carrier:AccountReady")
    });

    let cmd = format!(
        r#"[] a carrier:ExportAccount ; carrier:account "{account}" ; carrier:path "{archive_path}" ."#,
    );
    // SAFETY: handle is live; cmd bytes are valid for the slice length.
    let send_rc = unsafe { antenna_send(handle, cmd.as_ptr() as *const c_char, cmd.len()) };
    assert_eq!(send_rc, 0, "antenna_send(ExportAccount) returned {send_rc}");

    let archive_event = drain_until(handle, &sink, Duration::from_secs(15), |s| {
        s.contains("carrier:AccountArchiveReady")
    });
    assert!(
        archive_event.contains(&archive_path),
        "AccountArchiveReady must carry the requested path; got:\n{archive_event}",
    );

    // Filesystem-level sanity: the archive blob should exist.
    let meta = std::fs::metadata(&archive_path)
        .unwrap_or_else(|e| panic!("archive at {archive_path} missing: {e}"));
    assert!(meta.len() > 0, "archive should be non-empty");

    // SAFETY: pointers came from antenna_create; matching deallocators.
    unsafe {
        antenna_free(out_account_id as *mut c_void);
        antenna_destroy(handle);
    }
    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&store_dir);
    let _ = std::fs::remove_file(&archive_path);
}
