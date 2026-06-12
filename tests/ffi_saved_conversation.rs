// Copyright (c) 2026-2027 Resonator LLC. Licensed under MIT.

//! FFI round-trip for `carrier:GetSavedConversation` / `carrier:SavedConversation`
//! — the find-or-create verb that backs messenger2's "Saved Messages" workspace.
//!
//! Runs in its own test binary so libjami's process-scoped singleton starts
//! clean every time.

use std::ffi::{c_char, c_int, c_void, CString};
use std::sync::Mutex;
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
    // SAFETY: user is `&Sink`; turtle..turtle+len is valid UTF-8 for this call.
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

/// Drain until a line at index >= `start_idx` matches `predicate`.
/// Returns (line, idx_after_match) so callers can chain searches forward.
fn drain_until<F>(
    handle: *mut AntennaHandle,
    sink: &Sink,
    start_idx: usize,
    timeout: Duration,
    predicate: F,
) -> (String, usize)
where
    F: Fn(&str) -> bool,
{
    let start = Instant::now();
    loop {
        // SAFETY: handle came from antenna_create and is still live.
        let rc: c_int =
            unsafe { antenna_drain(handle, Some(collect_cb), sink as *const Sink as *mut c_void) };
        assert!(rc >= 0, "antenna_drain returned {rc}");

        {
            let guard = sink.lock().unwrap();
            if let Some((i, found)) = guard
                .iter()
                .enumerate()
                .skip(start_idx)
                .find(|(_, s)| predicate(s))
            {
                return (found.clone(), i + 1);
            }
        }

        if start.elapsed() > timeout {
            let dump = sink.lock().map(|g| g.join("\n")).unwrap_or_default();
            panic!("drain_until timed out after {timeout:?}; collected:\n{dump}");
        }

        std::thread::sleep(Duration::from_millis(10));
    }
}

fn extract_conv_id(line: &str) -> Option<String> {
    let key = "carrier:conversationId \"";
    let start = line.find(key)? + key.len();
    let rest = &line[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn read_account_id(handle: *mut AntennaHandle) -> String {
    let mut buf = [0u8; 96];
    // SAFETY: handle is live; buf is writable for buf.len() bytes.
    let n = unsafe { antenna_account_id(handle, buf.as_mut_ptr() as *mut c_char, buf.len()) };
    String::from_utf8(buf[..n].to_vec()).unwrap_or_default()
}

#[test]
fn get_saved_conversation_mints_then_returns_same_id_idempotently() {
    let data_dir = unique_dir("antenna-ffi-saved-data");
    let store_dir = unique_dir("antenna-ffi-saved-store");

    let data_dir_c = CString::new(data_dir.clone()).unwrap();
    let store_dir_c = CString::new(store_dir.clone()).unwrap();
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
    assert!(!handle.is_null(), "antenna_create returned NULL");
    // SAFETY: handle is live.
    assert!(unsafe { antenna_clock_fd(handle) } >= 0);

    let sink: Sink = Mutex::new(Vec::new());

    // Mint a fresh account so libjami has something to attach the swarm to.
    let create = br#"[] a carrier:CreateAccount ; carrier:displayName "alice" ."#;
    let send_rc = unsafe { antenna_send(handle, create.as_ptr() as *const c_char, create.len()) };
    assert_eq!(send_rc, 0);

    let (ready_line, cursor) = drain_until(handle, &sink, 0, Duration::from_secs(60), |s| {
        s.contains("carrier:AccountReady")
    });
    assert!(
        ready_line.contains("carrier:account"),
        "AccountReady missing account"
    );

    let account = read_account_id(handle);
    assert!(
        !account.is_empty(),
        "account id should populate after AccountReady"
    );

    // ---- First call: mints the swarm. ----
    let req1 = format!(
        r#"[] a carrier:GetSavedConversation ; carrier:account "{}" ."#,
        account
    );
    let bytes1 = req1.as_bytes();
    // SAFETY: handle live; bytes1 is a valid byte slice for the call.
    let rc = unsafe { antenna_send(handle, bytes1.as_ptr() as *const c_char, bytes1.len()) };
    assert_eq!(rc, 0);

    let (saved1, cursor) = drain_until(handle, &sink, cursor, Duration::from_secs(15), |s| {
        s.contains("carrier:SavedConversation")
    });
    let conv1 =
        extract_conv_id(&saved1).expect("SavedConversation must carry carrier:conversationId");
    assert_eq!(
        conv1.len(),
        40,
        "conversationId must be 40-hex; got {conv1:?}"
    );
    assert!(
        conv1
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "conversationId must be lower-hex; got {conv1:?}",
    );

    // ---- Second call: must return the same id. ----
    let req2 = req1.clone();
    let bytes2 = req2.as_bytes();
    let rc = unsafe { antenna_send(handle, bytes2.as_ptr() as *const c_char, bytes2.len()) };
    assert_eq!(rc, 0);

    let (saved2, _) = drain_until(handle, &sink, cursor, Duration::from_secs(15), |s| {
        s.contains("carrier:SavedConversation")
    });
    let conv2 = extract_conv_id(&saved2)
        .expect("second SavedConversation must carry carrier:conversationId");
    assert_eq!(
        conv1, conv2,
        "GetSavedConversation must be idempotent; first={conv1} second={conv2}",
    );

    // SAFETY: pointers came from antenna_create / matching allocators.
    unsafe {
        antenna_free(out_account_id as *mut c_void);
        antenna_destroy(handle);
    }
    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&store_dir);
}
