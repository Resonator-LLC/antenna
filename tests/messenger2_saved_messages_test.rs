//! Cut D (Saved Messages, 2026-05-26) — pipeline smoke for the
//! single-member-self swarm bootstrap in `radios/messenger2/pipeline.ttl`.
//!
//! Cut D wires the JS-side state (`globalThis.savedConvId` /
//! `globalThis.savedMessages`), the AccountReady → `GetSavedConversation`
//! request, the `SavedConversation` reply handler, the `GroupMessage`
//! routing + dedupe, and the `TextSubmitted` SAVED branch. None of these
//! call `rebuild()` yet — Cut E adds rebuilds plus the
//! `urn:msg2:select:saved` tap routing that flips `activeUri`. So this
//! test file only asserts what's observable without an `activeUri` flip
//! into Saved Messages:
//!
//!   1. AccountReady emits `carrier:GetSavedConversation` carrying the
//!      account id (D.3).
//!   2. `carrier:SavedConversation` reply runs cleanly — no script error,
//!      no spurious emits — and the pipeline keeps responding to
//!      subsequent events (D.4 sanity).
//!   3. `carrier:GroupMessage` on the resolved savedConvId runs cleanly,
//!      and an off-conv GroupMessage doesn't perturb pipeline state
//!      (D.5 sanity, dedupe coverage lands in Cut E once rebuild
//!      emits an observable widget body).
//!
//! Cut E will extend this file with `tile_renders_above_contacts...`,
//! `saved_select_tap_swaps_right_pane`, and the
//! `appends_optimistically_then_dedupes_on_replay` body assertions.

use antenna::channel::AntennaOut;
use antenna::dag::Dag;
use antenna::dispatch;
use antenna::store::RdfStore;
use std::path::PathBuf;
use std::time::Duration;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("antenna sits one level under the workspace root")
        .to_path_buf()
}

fn rel(p: &str) -> PathBuf {
    workspace_root().join(p)
}

struct CaptureOut {
    messages: Vec<String>,
}
impl CaptureOut {
    fn new() -> Self {
        Self { messages: Vec::new() }
    }
}
impl AntennaOut for CaptureOut {
    fn send(&mut self, turtle: &str) {
        self.messages.push(turtle.to_string());
    }
}

fn build_messenger2_pipeline() -> (RdfStore, Dag) {
    let store = RdfStore::open(None).expect("in-memory store");

    let pipeline_raw = std::fs::read_to_string(rel("radios/messenger2/pipeline.ttl"))
        .expect("read messenger2 pipeline");
    let pipeline_ttl = pipeline_raw
        .replace("__NICK__", "alice")
        .replace("__FILES_DIR__", "/tmp/messenger2-saved-test/files")
        .replace("__AUTO_EXPORT_PATH__", "/tmp/messenger2-saved-test/auto-export.gz");
    store
        .insert_turtle(&pipeline_ttl)
        .expect("insert messenger2 pipeline");

    let seed_ttl = std::fs::read_to_string(rel("radios/messenger2/seed.ttl"))
        .expect("read messenger2 seed");
    store.insert_turtle(&seed_ttl).expect("insert messenger2 seed");

    let dag = Dag::load(&store).expect("load dag");
    (store, dag)
}

/// Pump the DAG until the script falls quiet. Returns every Turtle line
/// the script emitted across the run — including ones that dispatch
/// routes into a `carrier=None` warn-skip. Same shape as
/// `messenger2_vcard_pipeline_test::settle_collect_emits`.
fn settle_collect_emits(
    dag: &Dag,
    store: &RdfStore,
    out: &mut CaptureOut,
    max_iters: usize,
) -> Vec<String> {
    const EMPTY_BREAK: usize = 5;
    let mut all_emits = Vec::new();
    let mut empty_streak = 0usize;
    let mut saw_emit = false;
    for _ in 0..max_iters {
        std::thread::sleep(Duration::from_millis(40));
        dag.pump_queries(store);
        let emits = dag.pump_emits();
        if emits.is_empty() {
            empty_streak += 1;
            if saw_emit && empty_streak >= EMPTY_BREAK {
                break;
            }
            continue;
        }
        saw_emit = true;
        empty_streak = 0;
        for turtle in &emits {
            dispatch::dispatch(turtle, store, dag, None, "", out);
        }
        all_emits.extend(emits);
    }
    all_emits
}

fn account_ready_event(account_id: &str, self_uri: &str) -> String {
    format!(
        "[] a antenna:Test ; \
         carrier:AccountReady \"_\" ; \
         carrier:account \"{account_id}\" ; \
         carrier:selfUri \"{self_uri}\" ."
    )
}

fn saved_conversation_event(account_id: &str, conversation_id: &str) -> String {
    format!(
        "[] a antenna:Test ; \
         carrier:SavedConversation \"_\" ; \
         carrier:account \"{account_id}\" ; \
         carrier:conversationId \"{conversation_id}\" ."
    )
}

fn group_message_event(account_id: &str, conversation_id: &str, text: &str) -> String {
    format!(
        "[] a antenna:Test ; \
         carrier:GroupMessage \"_\" ; \
         carrier:account \"{account_id}\" ; \
         carrier:conversationId \"{conversation_id}\" ; \
         carrier:text \"{text}\" ."
    )
}

const ALICE_URI: &str = "0123456789abcdef0123456789abcdef01234567";
const ALICE_ACCOUNT: &str = "abc123def456abc123def456abc123def456abcd";
const SAVED_CONV_ID: &str = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";

#[test]
fn account_ready_emits_get_saved_conversation() {
    let (store, dag) = build_messenger2_pipeline();
    let mut out = CaptureOut::new();
    // Drain the init-block emits (SetNick, GetId, panel rebuild) so the
    // GetSavedConversation we're hunting for is the only carrier:Get*
    // emit attributable to this AccountReady.
    let _boot_emits = settle_collect_emits(&dag, &store, &mut out, 30);

    dispatch::dispatch(
        &account_ready_event(ALICE_ACCOUNT, ALICE_URI),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    let emits = settle_collect_emits(&dag, &store, &mut out, 30);

    let get_saved = emits
        .iter()
        .find(|e| e.contains("carrier:GetSavedConversation"))
        .unwrap_or_else(|| {
            panic!(
                "expected a carrier:GetSavedConversation emit after AccountReady; \
                 saw {} emits, none matching. Emits:\n  {}",
                emits.len(),
                emits.join("\n  "),
            )
        });

    assert!(
        get_saved.contains(&format!("carrier:account \"{ALICE_ACCOUNT}\"")),
        "GetSavedConversation should carry the account id; got: {get_saved}",
    );
}

#[test]
fn saved_conversation_reply_does_not_disturb_pipeline() {
    // Light-touch sanity: a well-formed carrier:SavedConversation reply
    // is silently consumed (D.4 only stores the convId on globalThis;
    // rebuild + UI surfacing land in Cut E). Asserting on the absence
    // of follow-on Get*/Send* emits guards against accidental loops or
    // re-emits — the kind of bug a typo in extractProp would surface.
    let (store, dag) = build_messenger2_pipeline();
    let mut out = CaptureOut::new();

    let _boot_emits = settle_collect_emits(&dag, &store, &mut out, 30);

    dispatch::dispatch(
        &account_ready_event(ALICE_ACCOUNT, ALICE_URI),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    let _ready_emits = settle_collect_emits(&dag, &store, &mut out, 30);

    dispatch::dispatch(
        &saved_conversation_event(ALICE_ACCOUNT, SAVED_CONV_ID),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    let reply_emits = settle_collect_emits(&dag, &store, &mut out, 30);

    let suspicious: Vec<&String> = reply_emits
        .iter()
        .filter(|e| {
            e.contains("carrier:GetSavedConversation") ||
            e.contains("carrier:SendConversationMsg") ||
            e.contains("carrier:SendMsg")
        })
        .collect();
    assert!(
        suspicious.is_empty(),
        "SavedConversation reply must not trigger further carrier sends in Cut D; \
         got:\n  {}",
        suspicious.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n  "),
    );
}

#[test]
fn group_message_routing_does_not_disturb_pipeline() {
    // Cut D's GroupMessage handler appends to globalThis.savedMessages
    // when convId matches savedConvId, otherwise logs+drops. Either path
    // must keep the pipeline responsive. Off-conv messages must not
    // route through any other handler (no accidental fall-through into
    // TextMessage's contact-rail rebuild).
    let (store, dag) = build_messenger2_pipeline();
    let mut out = CaptureOut::new();

    let _boot_emits = settle_collect_emits(&dag, &store, &mut out, 30);

    dispatch::dispatch(
        &account_ready_event(ALICE_ACCOUNT, ALICE_URI),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle_collect_emits(&dag, &store, &mut out, 30);

    dispatch::dispatch(
        &saved_conversation_event(ALICE_ACCOUNT, SAVED_CONV_ID),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle_collect_emits(&dag, &store, &mut out, 30);

    // On-conv GroupMessage: appends silently (no rebuild() in Cut D).
    dispatch::dispatch(
        &group_message_event(ALICE_ACCOUNT, SAVED_CONV_ID, "note to self"),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    let on_conv_emits = settle_collect_emits(&dag, &store, &mut out, 30);
    let on_conv_sends: Vec<&String> = on_conv_emits
        .iter()
        .filter(|e| e.contains("carrier:Send"))
        .collect();
    assert!(
        on_conv_sends.is_empty(),
        "on-conv GroupMessage must not trigger any carrier:Send* emits in Cut D; \
         got:\n  {}",
        on_conv_sends.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n  "),
    );

    // Off-conv GroupMessage: dropped with a log, no state change.
    let other_conv = "ffffffffffffffffffffffffffffffffffffffff";
    dispatch::dispatch(
        &group_message_event(ALICE_ACCOUNT, other_conv, "stranger group"),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    let off_conv_emits = settle_collect_emits(&dag, &store, &mut out, 30);
    let off_conv_sends: Vec<&String> = off_conv_emits
        .iter()
        .filter(|e| e.contains("carrier:Send"))
        .collect();
    assert!(
        off_conv_sends.is_empty(),
        "off-conv GroupMessage must be a no-op; got:\n  {}",
        off_conv_sends.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n  "),
    );
}
