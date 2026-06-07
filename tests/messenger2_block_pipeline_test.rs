//! CMP-002 (UGC moderation — block by identity) pipeline tests for
//! `radios/messenger2/pipeline.ttl`.
//!
//! Proves the two compliance-critical claims directly against the real
//! pipeline JS (no carrier daemon required — these run with `carrier=None`,
//! exercising the render gate + persisted blocklist graph that are the
//! durable, serverless enforcement layer):
//!
//!   1. **Blocked peers' content never renders.** A peer's text message shows
//!      in the thread before the block and disappears after; a fresh message
//!      from the blocked peer is dropped and never reaches the rendered widget.
//!   2. **The block survives a real restart.** On a serverless network the
//!      durable source of truth is libjami's persisted ban, which the carrier
//!      replays at AccountReady as `carrier:ContactRestored ; carrier:blocked
//!      "true"`. `blocklist_survives_restart_via_carrier_rehydration` drives
//!      that exact event against a FRESH in-memory store (the default
//!      messenger2 boot — the RDF store is rebuilt from pipeline+seed every
//!      time) and proves the pipeline re-hydrates its render gate, re-projects
//!      the blocklist graph, and re-renders the tile/vCard in blocked state —
//!      no hand-seeded graph required. `blocklist_persists_across_restart`
//!      separately covers the on-disk-store hydration path (loadBlocklist).
//!
//! Plus reversibility (unblock restores delivery) and the store-required
//! Terms gate (CREATE refuses until the EULA checkbox is accepted).
//!
//! Mirrors the harness in `messenger2_vcard_pipeline_test.rs`.

use antenna::channel::AntennaOut;
use antenna::dag::Dag;
use antenna::dispatch;
use antenna::store::RdfStore;
use oxigraph::sparql::QueryResults;
use std::path::PathBuf;
use std::time::Duration;

const ANTENNA_NS: &str = "http://resonator.network/v2/antenna#";
const BLOCKLIST_GRAPH: &str = "urn:resonator:blocklist";

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
        .replace("__FILES_DIR__", "/tmp/messenger2-test/files")
        .replace("__AUTO_EXPORT_PATH__", "/tmp/messenger2-test/auto-export.gz");
    store
        .insert_turtle(&pipeline_ttl)
        .expect("insert messenger2 pipeline");
    let seed_ttl = std::fs::read_to_string(rel("radios/messenger2/seed.ttl"))
        .expect("read messenger2 seed");
    store.insert_turtle(&seed_ttl).expect("insert messenger2 seed");
    let dag = Dag::load(&store).expect("load dag");
    (store, dag)
}

fn settle(dag: &Dag, store: &RdfStore, out: &mut CaptureOut, max_iters: usize) {
    const EMPTY_BREAK: usize = 5;
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
    }
}

fn settle_collect_emits(
    dag: &Dag,
    store: &RdfStore,
    out: &mut CaptureOut,
    max_iters: usize,
) -> Vec<String> {
    const EMPTY_BREAK: usize = 5;
    let mut all = Vec::new();
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
        all.extend(emits);
    }
    all
}

fn ask(store: &RdfStore, sparql: &str) -> bool {
    match store.query(sparql).expect("sparql ASK") {
        QueryResults::Boolean(b) => b,
        _ => panic!("expected ASK result"),
    }
}

/// The rendered inbox panel — the active thread (incl. message bubbles) lives
/// inside this Level's `antenna:widget` DSL. Returns the (single) widget body.
fn inbox_widget(store: &RdfStore) -> String {
    let QueryResults::Solutions(rows) = store
        .query(&format!(
            "PREFIX ant: <{ANTENNA_NS}> \
             SELECT ?w WHERE {{ <urn:msg2:inbox:level> a ant:Level ; ant:widget ?w }}"
        ))
        .expect("select inbox widget")
    else {
        panic!("expected SELECT result");
    };
    let mut bodies: Vec<String> = rows
        .map(|r| r.expect("row").iter().next().expect("?w").1.to_string())
        .collect();
    assert_eq!(
        bodies.len(),
        1,
        "expected exactly one inbox Level widget, got {}",
        bodies.len()
    );
    bodies.remove(0)
}

/// The per-contact vCard Level widget DSL (`urn:msg2:contact:<uri>:vlevel`),
/// where the Block/Unblock CTA and the "You blocked this contact" copy live.
/// `None` if the scene hasn't been emitted for this contact.
fn vcard_widget(store: &RdfStore, contact_uri: &str) -> Option<String> {
    let QueryResults::Solutions(rows) = store
        .query(&format!(
            "PREFIX ant: <{ANTENNA_NS}> \
             SELECT ?w WHERE {{ <urn:msg2:contact:{contact_uri}:vlevel> a ant:Level ; ant:widget ?w }}"
        ))
        .expect("select vcard widget")
    else {
        panic!("expected SELECT result");
    };
    rows.filter_map(|r| r.ok())
        .next()
        .map(|s| s.iter().next().expect("?w").1.to_string())
}

fn contact_online_event(contact_uri: &str) -> String {
    format!(
        "[] a antenna:Test ; carrier:ContactOnline \"_\" ; \
         carrier:contactUri \"{contact_uri}\" ."
    )
}

/// The carrier's AccountReady replay for a libjami-banned contact: one
/// `carrier:ContactRestored` carrying `carrier:blocked "true"`. This is the
/// exact wire shape `replay_contacts` emits for a peer the user blocked in a
/// prior session — the durable, serverless cross-restart signal.
fn contact_restored_blocked_event(contact_uri: &str) -> String {
    format!(
        "[] a antenna:Test ; carrier:ContactRestored \"_\" ; \
         carrier:contactUri \"{contact_uri}\" ; \
         carrier:displayName \"\" ; \
         carrier:blocked \"true\" ."
    )
}

fn text_message_event(contact_uri: &str, conv_id: &str, text: &str) -> String {
    format!(
        "[] a antenna:Test ; carrier:TextMessage \"_\" ; \
         carrier:contactUri \"{contact_uri}\" ; \
         carrier:conversationId \"{conv_id}\" ; \
         carrier:text \"{text}\" ."
    )
}

fn tap_event(target: &str) -> String {
    format!(
        "[] a antenna:Test ; antenna:TapEvent \"_\" ; \
         <http://resonator.network/v2/antenna#target> <{target}> ."
    )
}

fn text_changed_event(target: &str, value: &str) -> String {
    format!(
        "[] a antenna:Test ; antenna:TextChanged \"_\" ; \
         <http://resonator.network/v2/antenna#target> <{target}> ; \
         <http://resonator.network/v2/antenna#value> \"{value}\" ."
    )
}

const PEER_URI: &str = "0123456789abcdef0123456789abcdef01234567";

#[test]
fn blocking_a_peer_hides_their_messages() {
    let (store, dag) = build_messenger2_pipeline();
    let mut out = CaptureOut::new();

    // Peer comes online (auto-selected as the active thread) and sends a
    // message — it renders in the panel.
    dispatch::dispatch(&contact_online_event(PEER_URI), &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 30);
    dispatch::dispatch(
        &text_message_event(PEER_URI, "conv-1", "HELLO_BEFORE_BLOCK"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 30);

    assert!(
        inbox_widget(&store).contains("HELLO_BEFORE_BLOCK"),
        "pre-block message should render in the active thread",
    );

    // Block the peer from their vCard.
    dispatch::dispatch(
        &tap_event(&format!("urn:msg2:block:{PEER_URI}")),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 30);

    let after_block = inbox_widget(&store);
    assert!(
        !after_block.contains("HELLO_BEFORE_BLOCK"),
        "blocking must purge the rendered conversation; widget still had it:\n{after_block}",
    );
    assert!(
        after_block.contains("You blocked this contact"),
        "blocked thread should show the notice; got:\n{after_block}",
    );

    // The block is persisted to the blocklist graph (survives restart).
    assert!(
        ask(
            &store,
            &format!(
                "ASK {{ GRAPH <{BLOCKLIST_GRAPH}> {{ \
                 <urn:resonator:blocked:{PEER_URI}> a <urn:resonator:BlockedPeer> }} }}"
            ),
        ),
        "block must persist an entry in the blocklist named graph",
    );

    // A fresh message from the blocked peer is dropped — never rendered.
    dispatch::dispatch(
        &text_message_event(PEER_URI, "conv-1", "HELLO_AFTER_BLOCK"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 30);
    assert!(
        !inbox_widget(&store).contains("HELLO_AFTER_BLOCK"),
        "a blocked peer's later message must never reach the rendered widget",
    );
}

#[test]
fn blocklist_survives_restart_via_carrier_rehydration() {
    // The real default-boot restart path. messenger2's RDF store is in-memory
    // and rebuilt from pipeline+seed every launch, so the blocklist graph does
    // NOT carry over on its own. What carries over is libjami's persisted ban,
    // which the carrier replays at AccountReady as
    //   carrier:ContactRestored ; carrier:blocked "true"
    // Drive that event against a fresh pipeline (NO pre-seeded graph) and prove
    // the block fully re-materialises.
    let (store, dag) = build_messenger2_pipeline();
    let mut out = CaptureOut::new();

    dispatch::dispatch(
        &contact_restored_blocked_event(PEER_URI),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 40);

    // (a) The blocklist graph is re-projected from the daemon ban, so the
    // self-card list + any SPARQL probe see the entry again.
    assert!(
        ask(
            &store,
            &format!(
                "ASK {{ GRAPH <{BLOCKLIST_GRAPH}> {{ \
                 <urn:resonator:blocked:{PEER_URI}> a <urn:resonator:BlockedPeer> }} }}"
            ),
        ),
        "restart must re-project the blocklist graph from carrier's persisted ban",
    );

    // (b) The render gate is re-hydrated: a freshly replayed message from the
    // blocked peer is dropped and never reaches the rendered widget.
    dispatch::dispatch(
        &text_message_event(PEER_URI, "conv-1", "GHOST_AFTER_RESTART"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 30);
    assert!(
        !inbox_widget(&store).contains("GHOST_AFTER_RESTART"),
        "a peer blocked before restart must stay filtered after carrier re-hydration",
    );

    // (c) The contact still renders as blocked in the rail tile, and (d) its
    // vCard still shows the "blocked" copy + the Unblock CTA — matching the
    // in-session state before the restart.
    assert!(
        inbox_widget(&store).contains("(blocked)"),
        "the restored-blocked contact's rail tile must read as (blocked)",
    );
    let vcard = vcard_widget(&store, PEER_URI).expect("blocked contact must still get a vCard scene");
    assert!(
        vcard.contains("You blocked this contact"),
        "the restored-blocked vCard must show the blocked notice; got:\n{vcard}",
    );
    assert!(
        vcard.contains("urn:msg2:unblock:") && vcard.contains("Unblock"),
        "the restored-blocked vCard must offer Unblock; got:\n{vcard}",
    );
}

#[test]
fn blocklist_persists_across_restart() {
    // The on-disk-store hydration path (loadBlocklist). When messenger2 is run
    // with a persistent Oxigraph store, the blocklist graph carries over and
    // init's loadBlocklist hydrates globalThis.blocked from it directly. Seed
    // the graph BEFORE the pipeline processes its first event to simulate that
    // persisted state. (The default in-memory boot relies on carrier
    // re-hydration instead — see blocklist_survives_restart_via_carrier_rehydration.)
    let (store, dag) = build_messenger2_pipeline();
    store
        .insert_turtle_to_graph(
            &format!("<urn:resonator:blocked:{PEER_URI}> a <urn:resonator:BlockedPeer> ."),
            BLOCKLIST_GRAPH,
        )
        .expect("seed blocklist graph");

    let mut out = CaptureOut::new();

    // First event is a message from the already-blocked peer. ensureContact
    // refuses to admit them, so no contact scene is created and the body is
    // never stored.
    dispatch::dispatch(
        &text_message_event(PEER_URI, "conv-1", "GHOST_MESSAGE"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 40);

    assert!(
        !inbox_widget(&store).contains("GHOST_MESSAGE"),
        "a peer blocked in a prior session must stay filtered after restart",
    );
    assert!(
        !ask(
            &store,
            &format!(
                "PREFIX ant: <{ANTENNA_NS}> \
                 ASK {{ <urn:msg2:contact:{PEER_URI}:scene> a ant:Scene }}"
            ),
        ),
        "a persisted-blocked peer must not produce a contact scene on boot",
    );
}

#[test]
fn unblock_restores_message_delivery() {
    let (store, dag) = build_messenger2_pipeline();
    let mut out = CaptureOut::new();

    dispatch::dispatch(&contact_online_event(PEER_URI), &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 30);
    dispatch::dispatch(
        &tap_event(&format!("urn:msg2:block:{PEER_URI}")),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 30);

    // Unblock from the vCard.
    dispatch::dispatch(
        &tap_event(&format!("urn:msg2:unblock:{PEER_URI}")),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 30);

    // The persisted entry is gone.
    assert!(
        !ask(
            &store,
            &format!(
                "ASK {{ GRAPH <{BLOCKLIST_GRAPH}> {{ \
                 <urn:resonator:blocked:{PEER_URI}> a <urn:resonator:BlockedPeer> }} }}"
            ),
        ),
        "unblock must remove the persisted blocklist entry",
    );

    // Messages flow again.
    dispatch::dispatch(
        &text_message_event(PEER_URI, "conv-1", "HELLO_AFTER_UNBLOCK"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 30);
    assert!(
        inbox_widget(&store).contains("HELLO_AFTER_UNBLOCK"),
        "after unblock, the peer's messages must render again",
    );
}

#[test]
fn eula_gate_blocks_account_creation_until_accepted() {
    let (store, dag) = build_messenger2_pipeline();
    let mut out = CaptureOut::new();

    dispatch::dispatch(
        "[] a antenna:OnboardingRequired ; antenna:reason \"no-account\" .",
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 30);
    dispatch::dispatch(
        &text_changed_event("urn:msg2:onboarding:nick", "alice"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 30);

    // Tap CREATE without accepting the Terms — no carrier:CreateAccount.
    dispatch::dispatch(
        &tap_event("urn:msg2:onboarding:create"),
        &store, &dag, None, "", &mut out,
    );
    let pre = settle_collect_emits(&dag, &store, &mut out, 30);
    assert!(
        !pre.iter().any(|e| e.contains("carrier:CreateAccount")),
        "CREATE must not mint an account before the Terms are accepted; emits:\n  {}",
        pre.join("\n  "),
    );

    // Accept the Terms, then CREATE works.
    dispatch::dispatch(
        &tap_event("urn:msg2:onboarding:eula-toggle"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 30);
    dispatch::dispatch(
        &tap_event("urn:msg2:onboarding:create"),
        &store, &dag, None, "", &mut out,
    );
    let post = settle_collect_emits(&dag, &store, &mut out, 30);
    assert!(
        post.iter().any(|e| e.contains("carrier:CreateAccount")),
        "CREATE must mint an account once the Terms are accepted; emits:\n  {}",
        post.join("\n  "),
    );
}
