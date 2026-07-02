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
        Self {
            messages: Vec::new(),
        }
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
        .replace(
            "__AUTO_EXPORT_PATH__",
            "/tmp/messenger2-test/auto-export.gz",
        );
    store
        .insert_turtle(&pipeline_ttl)
        .expect("insert messenger2 pipeline");
    let seed_ttl =
        std::fs::read_to_string(rel("radios/messenger2/seed.ttl")).expect("read messenger2 seed");
    store
        .insert_turtle(&seed_ttl)
        .expect("insert messenger2 seed");
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

/// The carrier's AccountReady header — sets `globalThis.account` (via
/// noteAccount) + `globalThis.selfUri`. carrier:RemoveContact / carrier:
/// RemoveConversation both *require* carrier:account, so the remove path only
/// fires once an account is ready — exactly the production ordering (AccountReady
/// always precedes any contact event).
fn account_ready_event(account: &str, self_uri: &str) -> String {
    format!(
        "[] a antenna:Test ; carrier:AccountReady \"_\" ; \
         carrier:account \"{account}\" ; \
         carrier:selfUri \"{self_uri}\" ."
    )
}

const PEER_URI: &str = "0123456789abcdef0123456789abcdef01234567";
const SELF_URI: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[test]
fn blocking_a_peer_hides_their_messages() {
    let (store, dag) = build_messenger2_pipeline();
    let mut out = CaptureOut::new();

    // Peer comes online (auto-selected as the active thread) and sends a
    // message — it renders in the panel.
    dispatch::dispatch(
        &contact_online_event(PEER_URI),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 30);
    dispatch::dispatch(
        &text_message_event(PEER_URI, "conv-1", "HELLO_BEFORE_BLOCK"),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 30);

    assert!(
        inbox_widget(&store).contains("HELLO_BEFORE_BLOCK"),
        "pre-block message should render in the active thread",
    );

    // Block the peer from their vCard.
    dispatch::dispatch(
        &tap_event(&format!("urn:msg2:block:{PEER_URI}")),
        &store,
        &dag,
        None,
        "",
        &mut out,
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
        &store,
        &dag,
        None,
        "",
        &mut out,
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
        &store,
        &dag,
        None,
        "",
        &mut out,
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
        &store,
        &dag,
        None,
        "",
        &mut out,
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
    let vcard =
        vcard_widget(&store, PEER_URI).expect("blocked contact must still get a vCard scene");
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
        &store,
        &dag,
        None,
        "",
        &mut out,
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

    dispatch::dispatch(
        &contact_online_event(PEER_URI),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 30);
    dispatch::dispatch(
        &tap_event(&format!("urn:msg2:block:{PEER_URI}")),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 30);

    // Unblock from the vCard.
    dispatch::dispatch(
        &tap_event(&format!("urn:msg2:unblock:{PEER_URI}")),
        &store,
        &dag,
        None,
        "",
        &mut out,
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
        &store,
        &dag,
        None,
        "",
        &mut out,
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
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 30);
    dispatch::dispatch(
        &text_changed_event("urn:msg2:onboarding:nick", "alice"),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 30);

    // CMP-019 conversational flow: tap the connect action before accepting the
    // Terms — emitOnboardingCreate()'s EULA guard must refuse to mint.
    dispatch::dispatch(
        &tap_event("urn:msg2:onboarding:connect"),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    let pre = settle_collect_emits(&dag, &store, &mut out, 30);
    assert!(
        !pre.iter().any(|e| e.contains("carrier:CreateAccount")),
        "connect must not mint an account before the Terms are accepted; emits:\n  {}",
        pre.join("\n  "),
    );

    // Accept the Terms (the "I agree" turn), then connect works.
    dispatch::dispatch(
        &tap_event("urn:msg2:onboarding:agree"),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 30);
    dispatch::dispatch(
        &tap_event("urn:msg2:onboarding:connect"),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    let post = settle_collect_emits(&dag, &store, &mut out, 30);
    assert!(
        post.iter().any(|e| e.contains("carrier:CreateAccount")),
        "connect must mint an account once the Terms are accepted; emits:\n  {}",
        post.join("\n  "),
    );
}

// ---------------------------------------------------------------------------
// CMP-022 — in-app report mechanism
// ---------------------------------------------------------------------------

#[test]
fn reporting_a_contact_composes_an_evidence_bundle_and_offers_block() {
    let (store, dag) = build_messenger2_pipeline();
    let mut out = CaptureOut::new();

    // A peer comes online and sends an objectionable message.
    dispatch::dispatch(
        &contact_online_event(PEER_URI),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 30);
    dispatch::dispatch(
        &text_message_event(PEER_URI, "conv-1", "HELLO_OBJECTIONABLE_PAYLOAD"),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 30);

    // Tap Report on the peer's vCard.
    dispatch::dispatch(
        &tap_event(&format!("urn:msg2:report:{PEER_URI}")),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    let emits = settle_collect_emits(&dag, &store, &mut out, 30);

    // The report routes out-of-band: a mailto: to the published support
    // contact carrying the evidence bundle (sender identity + the offending
    // inbound message). encodeURIComponent leaves the hex URI + the
    // alnum/underscore message text literal, so we can match them directly.
    let mailto = emits
        .iter()
        .find(|e| e.contains("urn:msg:OpenExternal") && e.contains("mailto:"))
        .unwrap_or_else(|| {
            panic!(
                "Report must emit an OpenExternal mailto; emits:\n  {}",
                emits.join("\n  ")
            )
        });
    assert!(
        mailto.contains("mailto:support@resonator.network"),
        "report must be addressed to the published support contact; got:\n{mailto}",
    );
    assert!(
        mailto.contains(PEER_URI),
        "evidence bundle must carry the sender's keypair identity; got:\n{mailto}",
    );
    assert!(
        mailto.contains("HELLO_OBJECTIONABLE_PAYLOAD"),
        "evidence bundle must carry the offending inbound message; got:\n{mailto}",
    );

    // The reporter gets a confirmation and is offered Block in the same flow.
    let vcard = vcard_widget(&store, PEER_URI).expect("reported contact vCard");
    assert!(
        vcard.contains("Report composed"),
        "vCard must confirm the report was composed; got:\n{vcard}",
    );
    assert!(
        vcard.contains(&format!("urn:msg2:block:{PEER_URI}")),
        "the report-confirmation flow must still offer Block; got:\n{vcard}",
    );
}

// ---------------------------------------------------------------------------
// CMP-024 — default subscribable signed blocklist
// ---------------------------------------------------------------------------

// Production-signed test vectors — the exact (payload_b64, sig_b64) pairs
// Station would fetch and hand to antenna, signed by the real blocklist key
// whose public half is pinned as `BLOCKLIST_PUBKEY` in `antenna::blocklist` and
// whose secret lives only in 1Password (custody: compliance/plan/
// cmp024-blocklist-key.md). They name the inert test fingerprint PEER_URI, so
// the signed bytes are harmless public artifacts. Regenerate with the CUT-25
// keygen helper if PEER_URI or the pinned key ever changes.

/// Signed `# resonator default blocklist\n{PEER_URI}\n`.
const SIGNED_LIST_PAYLOAD: &str = "IyByZXNvbmF0b3IgZGVmYXVsdCBibG9ja2xpc3QKMDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWYwMTIzNDU2Nwo=";
const SIGNED_LIST_SIG: &str =
    "+0xX46+KHmCiLYrOQhOpvjw8bdobB81OBG5kcEDTAbEpBhOeLwy01Lk9gJmI0R1dOp2SRH3wIKEOR8aXtpPkDQ==";

/// Signed `{PEER_URI}\n` (no header line).
const SIGNED_PEER_PAYLOAD: &str = "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWYwMTIzNDU2Nwo=";
const SIGNED_PEER_SIG: &str =
    "BtKxVmPk/c2FfseDKJHe5VJSGjsH6esCI7W1vv3exv4GIrj8LbpXdo3G60Hxa/wzGwZdAGFDaf1HnPSa5L+0Bg==";

fn subscribe_event(payload_b64: &str, sig_b64: &str) -> String {
    format!(
        "[] a antenna:SubscribeBlocklist ; \
         antenna:blocklistPayload \"{payload_b64}\" ; \
         antenna:blocklistSig \"{sig_b64}\" ."
    )
}

fn peer_in_blocklist_graph(store: &RdfStore) -> bool {
    ask(
        store,
        &format!(
            "ASK {{ GRAPH <{BLOCKLIST_GRAPH}> {{ \
             <urn:resonator:blocked:{PEER_URI}> a <urn:resonator:BlockedPeer> }} }}"
        ),
    )
}

#[test]
fn subscribed_blocklist_applies_through_the_same_gate() {
    let (store, dag) = build_messenger2_pipeline();
    let mut out = CaptureOut::new();

    // The peer is a live contact whose messages render.
    dispatch::dispatch(
        &contact_online_event(PEER_URI),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 30);
    dispatch::dispatch(
        &text_message_event(PEER_URI, "conv-1", "BEFORE_SUBSCRIPTION"),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 30);
    assert!(
        inbox_widget(&store).contains("BEFORE_SUBSCRIPTION"),
        "pre-subscription message renders"
    );

    // A developer-signed list naming the peer is applied (antenna verifies the
    // signature, re-emits BlocklistApply, the pipeline blocks via the same path).
    dispatch::dispatch(
        &subscribe_event(SIGNED_LIST_PAYLOAD, SIGNED_LIST_SIG),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 40);

    // Same enforcement path as a manual block: blocklist graph + render gate.
    assert!(
        peer_in_blocklist_graph(&store),
        "subscription must persist via the blocklist graph"
    );
    let vcard = vcard_widget(&store, PEER_URI).expect("vCard");
    assert!(
        vcard.contains("You blocked this contact"),
        "subscribed entry blocks the vCard; got:\n{vcard}"
    );

    // A later message from the now-blocked peer is dropped.
    dispatch::dispatch(
        &text_message_event(PEER_URI, "conv-1", "AFTER_SUBSCRIPTION"),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 30);
    assert!(
        !inbox_widget(&store).contains("AFTER_SUBSCRIPTION"),
        "a subscription-blocked peer's later message must never render",
    );

    // Transparency: the self card labels subscription entries.
    let self_card = vcard_widget(&store, "urn:msg2:saved").expect("self card");
    assert!(
        self_card.contains("(subscription)"),
        "the self card must mark subscription-applied entries; got:\n{self_card}",
    );
}

#[test]
fn subscribed_blocklist_override_is_sticky() {
    let (store, dag) = build_messenger2_pipeline();
    let mut out = CaptureOut::new();

    dispatch::dispatch(
        &contact_online_event(PEER_URI),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 30);

    dispatch::dispatch(
        &subscribe_event(SIGNED_PEER_PAYLOAD, SIGNED_PEER_SIG),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 40);
    assert!(peer_in_blocklist_graph(&store), "subscription applied");

    // The user overrides locally by unblocking the subscribed entry.
    dispatch::dispatch(
        &tap_event(&format!("urn:msg2:unblock:{PEER_URI}")),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 30);
    assert!(
        !peer_in_blocklist_graph(&store),
        "override must lift the block locally"
    );

    // Re-applying the same signed list must NOT re-block the overridden entry.
    dispatch::dispatch(
        &subscribe_event(SIGNED_PEER_PAYLOAD, SIGNED_PEER_SIG),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 40);
    assert!(
        !peer_in_blocklist_graph(&store),
        "a re-subscribe must respect the local override and not re-apply the entry",
    );
}

#[test]
fn tampered_blocklist_is_rejected_not_applied() {
    let (store, dag) = build_messenger2_pipeline();
    let mut out = CaptureOut::new();

    dispatch::dispatch(
        &contact_online_event(PEER_URI),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 30);

    // Take a valid production signature over `{PEER_URI}\n`, then ship a
    // DIFFERENT payload under it — antenna must reject the mismatch.
    let tampered = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .encode(b"deadbeefdeadbeefdeadbeefdeadbeefdeadbeef\n")
    };
    dispatch::dispatch(
        &subscribe_event(&tampered, SIGNED_PEER_SIG),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 40);

    assert!(
        !peer_in_blocklist_graph(&store),
        "a tampered list must apply NOTHING",
    );
    let self_card = vcard_widget(&store, "urn:msg2:saved").expect("self card");
    assert!(
        self_card.contains("Blocklist rejected"),
        "a rejected list must surface a notice, not silently apply; got:\n{self_card}",
    );
}

// ---------------------------------------------------------------------------
// CMP-002 remainder — safe-mode toggle
// ---------------------------------------------------------------------------

#[test]
fn safe_mode_defaults_on_and_toggles_with_persistence() {
    let (store, dag) = build_messenger2_pipeline();
    let mut out = CaptureOut::new();

    // Trigger init + render of the self card.
    dispatch::dispatch(
        &contact_online_event(PEER_URI),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 30);

    let self_card = vcard_widget(&store, "urn:msg2:saved").expect("self card");
    assert!(
        self_card.contains("Safe mode (hide images)"),
        "self card must surface the safe-mode toggle; got:\n{self_card}",
    );
    assert!(
        self_card.contains("urn:msg2:safemode:toggle"),
        "safe-mode toggle must be tappable; got:\n{self_card}",
    );

    // Toggling OFF must persist the preference as the SafeModeOff marker.
    dispatch::dispatch(
        &tap_event("urn:msg2:safemode:toggle"),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    let emits = settle_collect_emits(&dag, &store, &mut out, 30);
    assert!(
        emits.iter().any(|e| e.contains("SafeModeOff")),
        "toggling safe mode off must persist a SafeModeOff marker; emits:\n  {}",
        emits.join("\n  "),
    );
}

// ---------------------------------------------------------------------------
// ISSUE-135 — remove contact (plain un-friend, not a ban)
// ---------------------------------------------------------------------------

#[test]
fn removing_a_contact_emits_carrier_removes_and_purges_the_scene() {
    let (store, dag) = build_messenger2_pipeline();
    let mut out = CaptureOut::new();

    // Account ready first — carrier:RemoveContact / RemoveConversation both
    // require carrier:account, so the remove path is gated on it.
    dispatch::dispatch(
        &account_ready_event("acct-1", SELF_URI),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 30);

    // A peer comes online and exchanges a message: this creates the contact,
    // emits its vCard scene, and records the 1:1 conversationId under
    // contactConv[PEER_URI] (via the TextMessage handler's noteContactConv).
    dispatch::dispatch(
        &contact_online_event(PEER_URI),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 30);
    dispatch::dispatch(
        &text_message_event(PEER_URI, "conv-peer", "HELLO_BEFORE_REMOVE"),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 30);

    // Pre-conditions: the thread renders and the vCard offers a Remove CTA.
    assert!(
        inbox_widget(&store).contains("HELLO_BEFORE_REMOVE"),
        "pre-remove message should render in the active thread",
    );
    let vcard = vcard_widget(&store, PEER_URI).expect("contact must have a vCard scene");
    assert!(
        vcard.contains(&format!("urn:msg2:remove:{PEER_URI}")),
        "an established contact's vCard must offer a Remove CTA; got:\n{vcard}",
    );

    // Tap Remove.
    dispatch::dispatch(
        &tap_event(&format!("urn:msg2:remove:{PEER_URI}")),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    let emits = settle_collect_emits(&dag, &store, &mut out, 30);

    // (a) Plain un-friend at the daemon: RemoveContact carrying the account.
    assert!(
        emits
            .iter()
            .any(|e| e.contains("carrier:RemoveContact") && e.contains(PEER_URI)),
        "remove must emit carrier:RemoveContact for the peer; emits:\n  {}",
        emits.join("\n  "),
    );
    // (b) ...and drops the 1:1 conversation by its swarm id.
    assert!(
        emits
            .iter()
            .any(|e| e.contains("carrier:RemoveConversation") && e.contains("conv-peer")),
        "remove must emit carrier:RemoveConversation for the peer's thread; emits:\n  {}",
        emits.join("\n  "),
    );
    // (c) Remove is an un-friend, NOT a ban: no BlockContact, no blocklist entry.
    assert!(
        !emits.iter().any(|e| e.contains("carrier:BlockContact")),
        "remove must not ban the contact; emits:\n  {}",
        emits.join("\n  "),
    );
    assert!(
        !ask(
            &store,
            &format!(
                "ASK {{ GRAPH <{BLOCKLIST_GRAPH}> {{ \
                 <urn:resonator:blocked:{PEER_URI}> a <urn:resonator:BlockedPeer> }} }}"
            ),
        ),
        "remove must not persist a blocklist entry",
    );

    // (d) The contact is gone from the rendered rail and its vCard scene/level
    // are torn down.
    assert!(
        !inbox_widget(&store).contains("HELLO_BEFORE_REMOVE"),
        "the removed contact's thread must no longer render",
    );
    assert!(
        vcard_widget(&store, PEER_URI).is_none(),
        "the removed contact's vCard Level must be deleted",
    );
    assert!(
        !ask(
            &store,
            &format!(
                "PREFIX ant: <{ANTENNA_NS}> \
                 ASK {{ <urn:msg2:contact:{PEER_URI}:scene> a ant:Scene }}"
            ),
        ),
        "the removed contact's vCard Scene must be deleted",
    );
}

#[test]
fn saved_messages_self_card_offers_no_remove_cta() {
    // The Saved Messages self-thread must never be removable (it's the
    // single-member-self swarm — decision #10 invite-lock). Its vCard is the
    // kind==='self' branch, which returns before the Remove CTA is appended.
    let (store, dag) = build_messenger2_pipeline();
    let mut out = CaptureOut::new();

    dispatch::dispatch(
        &account_ready_event("acct-1", SELF_URI),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 30);
    dispatch::dispatch(
        &contact_online_event(PEER_URI),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 30);

    let self_card = vcard_widget(&store, "urn:msg2:saved").expect("self card");
    assert!(
        !self_card.contains("urn:msg2:remove:"),
        "the Saved Messages self card must never offer a Remove CTA; got:\n{self_card}",
    );
}
