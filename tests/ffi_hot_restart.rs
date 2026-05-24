// Copyright (c) 2026-2027 Resonator LLC. Licensed under MIT.

//! Regression test for ISSUE-122 — Flutter hot-restart leaks the previous
//! antenna handle (Dart isolate dies without calling `antenna_destroy`), so
//! a second `antenna_create` in the same process used to return NULL: the
//! Carrier `g_carrier` singleton was still live and `carrier_new` refused
//! the second init. libjami can't be cleanly re-initialised within one
//! process either, so the fix is to rebind — return the previous handle
//! and let the new isolate keep using the already-spun-up worker + rings.
//!
//! Lives in its own integration-test binary so the singleton state is
//! pristine at the start — each `tests/*.rs` is a separate cargo binary.

use std::ffi::{c_char, c_void, CStr, CString};
use std::ptr;

use antenna::ffi::{antenna_create, antenna_destroy, antenna_free};

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

#[test]
fn second_create_rebinds_to_orphaned_handle() {
    let data_dir = unique_dir("antenna-hotrestart-data");
    let store_dir = unique_dir("antenna-hotrestart-store");
    let data_dir_c = CString::new(data_dir.clone()).unwrap();
    let store_dir_c = CString::new(store_dir.clone()).unwrap();
    let mut out_account_a: *mut c_char = ptr::null_mut();

    // First boot — mints an account, libjami init, worker spinning,
    // SQLite store locked.
    // SAFETY: pointers are valid for the call; out_account_id is writable.
    let handle_a = unsafe {
        antenna_create(
            data_dir_c.as_ptr(),
            ptr::null(),
            store_dir_c.as_ptr(),
            ptr::null(),
            ptr::null(),
            &mut out_account_a as *mut *mut c_char,
        )
    };
    assert!(!handle_a.is_null(), "first antenna_create returned NULL");
    assert!(!out_account_a.is_null(), "first call did not populate out_account_id");
    // SAFETY: out_account_a is a NUL-terminated string we own.
    let account_a = unsafe { CStr::from_ptr(out_account_a) }
        .to_str()
        .expect("account id is valid UTF-8")
        .to_string();

    // Deliberately do NOT call antenna_destroy(handle_a). That's the
    // Flutter-hot-restart shape: Dart isolate dies, Rust staticlib stays
    // resident, the previous worker keeps spinning.

    let mut out_account_b: *mut c_char = ptr::null_mut();
    // SAFETY: pointers are valid; out_account_id is writable.
    let handle_b = unsafe {
        antenna_create(
            data_dir_c.as_ptr(),
            ptr::null(),
            store_dir_c.as_ptr(),
            ptr::null(),
            ptr::null(),
            &mut out_account_b as *mut *mut c_char,
        )
    };
    assert!(!handle_b.is_null(), "second antenna_create returned NULL");
    assert_eq!(
        handle_a, handle_b,
        "second antenna_create should rebind to the orphaned handle (got a different pointer)"
    );
    assert!(!out_account_b.is_null(), "second call did not populate out_account_id");
    // SAFETY: out_account_b is a NUL-terminated string we own.
    let account_b = unsafe { CStr::from_ptr(out_account_b) }
        .to_str()
        .expect("account id is valid UTF-8")
        .to_string();
    assert_eq!(
        account_a, account_b,
        "rebound handle should report the same account id"
    );

    // Single destroy is correct — handle_a and handle_b are the same pointer.
    // SAFETY: out_account_* came from antenna_create; handle_b is live.
    unsafe {
        antenna_free(out_account_a as *mut c_void);
        antenna_free(out_account_b as *mut c_void);
        antenna_destroy(handle_b);
    }

    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&store_dir);
}
