// Copyright (c) 2026-2027 Resonator LLC. Licensed under MIT.

//! ISSUE-127 regression — Swarm history replay through `SwarmLoaded`.
//!
//! Before the fix, libjami's cold-load surfaced `ConversationReady` for every
//! Swarm on disk but never replayed the commits inside, so a Station restart
//! rendered an empty Saved Messages chat. The fix:
//!   1. `carrier_load_conversation_messages` (and the corresponding
//!      `carrier:LoadConversationMessages` dispatch verb) wrap libjami's
//!      `loadConversation` async walk.
//!   2. Carrier registers `ConversationSignal::SwarmLoaded` and dispatches
//!      each historical commit through the same TextMessage / GroupMessage /
//!      FileRecv events the live SwarmMessageReceived path uses — including
//!      commits authored by self (the live filter drops those, but replay
//!      must surface them so the bubble can render on cold start).
//!   3. The `on_registration_state` cold-load loop kicks the walk per Swarm
//!      automatically, so messenger pipelines see the history without
//!      pipeline changes.
//!
//! This test exercises (1) + (2) deterministically in-process: libjami can't
//! be re-initialised within a single process (the singleton fini+init leaves
//! accounts stuck OUT of REGISTERED — see ISSUE-122), so we drive the
//! `LoadConversationMessages` verb directly on a still-live antenna to
//! simulate the cold-load loop's call. The cold-load auto-trigger in (3) is
//! covered by the manual smoke walkthrough in ISSUE-127.

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

fn count_matches(sink: &Sink, from: usize, predicate: impl Fn(&str) -> bool) -> usize {
    sink.lock()
        .unwrap()
        .iter()
        .skip(from)
        .filter(|s| predicate(s))
        .count()
}

fn drain_idle(handle: *mut AntennaHandle, sink: &Sink, idle: Duration) {
    // Pump until the sink has been quiet for `idle` consecutive ms.
    let mut last_len = sink.lock().unwrap().len();
    let mut last_change = Instant::now();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let rc =
            unsafe { antenna_drain(handle, Some(collect_cb), sink as *const Sink as *mut c_void) };
        assert!(rc >= 0, "antenna_drain returned {rc}");
        let now_len = sink.lock().unwrap().len();
        if now_len != last_len {
            last_len = now_len;
            last_change = Instant::now();
        } else if last_change.elapsed() >= idle {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
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
fn load_conversation_messages_replays_own_commits_as_group_messages() {
    let data_dir = unique_dir("antenna-ffi-history-data");
    let store_dir = unique_dir("antenna-ffi-history-store");

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

    // Mint a fresh account.
    let create = br#"[] a carrier:CreateAccount ; carrier:displayName "alice" ."#;
    let send_rc = unsafe { antenna_send(handle, create.as_ptr() as *const c_char, create.len()) };
    assert_eq!(send_rc, 0);

    let (_ready, cursor) = drain_until(handle, &sink, 0, Duration::from_secs(60), |s| {
        s.contains("carrier:AccountReady")
    });
    let account = read_account_id(handle);
    assert!(
        !account.is_empty(),
        "account id should populate after AccountReady"
    );

    // Mint Saved Messages (the multi-party self swarm that messenger2 uses).
    let req = format!(
        r#"[] a carrier:GetSavedConversation ; carrier:account "{}" ."#,
        account
    );
    let bytes = req.as_bytes();
    let rc = unsafe { antenna_send(handle, bytes.as_ptr() as *const c_char, bytes.len()) };
    assert_eq!(rc, 0);

    let (saved_line, cursor) = drain_until(handle, &sink, cursor, Duration::from_secs(15), |s| {
        s.contains("carrier:SavedConversation")
    });
    let saved_conv =
        extract_conv_id(&saved_line).expect("SavedConversation must carry conversationId");

    // Commit two text messages to the saved swarm. Live SwarmMessageReceived
    // collapses own commits into MessageSent (carries the id but no body) —
    // this is the well-formed live behaviour the test deliberately observes
    // before triggering replay.
    for body in ["hello self", "second note"] {
        let send = format!(
            "[] a carrier:SendConversationMsg ; carrier:account \"{}\" ; \
                carrier:conversationId \"{}\" ; carrier:text \"{}\" .",
            account, saved_conv, body
        );
        let b = send.as_bytes();
        let rc = unsafe { antenna_send(handle, b.as_ptr() as *const c_char, b.len()) };
        assert_eq!(rc, 0);
    }

    // Wait until two MessageSent events surface (one per own commit). This
    // also drains the live path so the subsequent replay events stand alone.
    let mut after_sends = cursor;
    let mut sent_seen = 0;
    let deadline = Instant::now() + Duration::from_secs(20);
    while sent_seen < 2 {
        let (_line, idx) = drain_until(handle, &sink, after_sends, Duration::from_secs(10), |s| {
            s.contains("carrier:MessageSent") && s.contains(&saved_conv)
        });
        sent_seen += 1;
        after_sends = idx;
        if Instant::now() > deadline {
            panic!("did not observe two MessageSent events in 20s");
        }
    }

    // Sanity: live path must NOT emit GroupMessage for own commits.
    let live_group_msgs = count_matches(&sink, cursor, |s| {
        s.contains("carrier:GroupMessage") && s.contains(&saved_conv)
    });
    assert_eq!(
        live_group_msgs, 0,
        "live SwarmMessageReceived must collapse own commits into MessageSent, not GroupMessage"
    );

    let replay_cursor = sink.lock().unwrap().len();

    // libjami's loadConversation walker short-circuits on commits already
    // present in its in-process quickAccess cache (populated by the live
    // SwarmMessageReceived path). On cold-start the cache is naturally
    // empty; in-process the test has to clear it to reach the same state.
    let clear_req = format!(
        "[] a carrier:ClearConversationCache ; carrier:account \"{}\" ; \
            carrier:conversationId \"{}\" .",
        account, saved_conv
    );
    let b = clear_req.as_bytes();
    let rc = unsafe { antenna_send(handle, b.as_ptr() as *const c_char, b.len()) };
    assert_eq!(rc, 0);

    // Now trigger the replay path that cold-load would invoke automatically.
    let load_req = format!(
        "[] a carrier:LoadConversationMessages ; carrier:account \"{}\" ; \
            carrier:conversationId \"{}\" .",
        account, saved_conv
    );
    let b = load_req.as_bytes();
    let rc = unsafe { antenna_send(handle, b.as_ptr() as *const c_char, b.len()) };
    assert_eq!(rc, 0);

    // Drain until the io thread pool's loadMessages walk + SwarmLoaded
    // dispatch settles. 30 s is the same upper bound as the saved-conv test.
    drain_idle(handle, &sink, Duration::from_millis(750));

    let replayed: Vec<String> = sink
        .lock()
        .unwrap()
        .iter()
        .skip(replay_cursor)
        .filter(|s| s.contains("carrier:GroupMessage") && s.contains(&saved_conv))
        .cloned()
        .collect();

    assert!(
        replayed
            .iter()
            .any(|s| s.contains(r#"carrier:text "hello self""#)),
        "replay must surface 'hello self' as a GroupMessage; got:\n  {}",
        replayed.join("\n  ")
    );
    assert!(
        replayed
            .iter()
            .any(|s| s.contains(r#"carrier:text "second note""#)),
        "replay must surface 'second note' as a GroupMessage; got:\n  {}",
        replayed.join("\n  ")
    );
    assert_eq!(
        replayed.len(),
        2,
        "replay should surface exactly two text commits, got {}:\n  {}",
        replayed.len(),
        replayed.join("\n  ")
    );

    // Each replayed GroupMessage must carry our own selfUri as contactUri —
    // proving the own-author filter is correctly bypassed on the replay path.
    let self_uri = {
        // SelfId carries the self URI; emit one and grab it.
        let req = b"[] a carrier:GetId .";
        let send_cursor = sink.lock().unwrap().len();
        let rc = unsafe { antenna_send(handle, req.as_ptr() as *const c_char, req.len()) };
        assert_eq!(rc, 0);
        let (line, _) = drain_until(handle, &sink, send_cursor, Duration::from_secs(5), |s| {
            s.contains("carrier:SelfId")
        });
        let key = "carrier:selfUri \"";
        let start = line.find(key).expect("SelfId carries selfUri") + key.len();
        let rest = &line[start..];
        let end = rest.find('"').expect("selfUri closing quote");
        rest[..end].to_string()
    };
    for line in &replayed {
        assert!(
            line.contains(&format!(r#"carrier:contactUri "{self_uri}""#)),
            "replayed GroupMessage should carry our selfUri as contactUri (proving \
             the own-author filter is bypassed on replay); got: {line}",
        );
    }

    // SAFETY: pointers came from antenna_create / matching allocators.
    unsafe {
        antenna_free(out_account_id as *mut c_void);
        antenna_destroy(handle);
    }
    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&store_dir);
}
